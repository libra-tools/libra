use git_internal::hash::ObjectHash;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TransactionTrait};
use thiserror::Error;
use uuid::Uuid;

use super::{
    applicability::{
        CodeApplicability, CodeHistory, RepositoryCodeHistory, assess_code_applicability,
    },
    domain::MemoryNoteV1,
    evidence::{EvidenceExpansionV1, EvidenceResolver},
    fts_sql::{EpisodeSearchCandidate, MemoryFtsError, search_candidates},
    policy::{AuthenticatedMemoryContext, authorizes_note_read},
    query::{EpisodeQueryError, EpisodeQueryV1},
    selector::{EPISODE_SELECTOR_VERSION, SelectableEpisode, select_episode_indexes},
    tree::load_note_bytes,
    validation::parse_memory_note_v1,
    view::{FrozenCodeAnchorV1, ResolvedMemoryViewError, ResolvedMemoryViewV1},
};
use crate::internal::{
    ai::{history::HistoryManager, keyed_digest::RepositoryKeyedDigest},
    head::Head,
    worktree_scope::WorktreeScope,
};

pub(crate) struct EpisodeReader<'a> {
    history: &'a HistoryManager,
    database: DatabaseConnection,
    digest: &'a RepositoryKeyedDigest,
    code_history: RepositoryCodeHistory,
    evidence: EvidenceResolver<'a>,
}

impl<'a> EpisodeReader<'a> {
    pub(crate) fn new(
        history: &'a HistoryManager,
        digest: &'a RepositoryKeyedDigest,
    ) -> Result<Self, EpisodeReaderError> {
        if history.repository_path().as_os_str().is_empty()
            || history.database_connection().get_database_backend()
                != sea_orm::DatabaseBackend::Sqlite
        {
            return Err(EpisodeReaderError::new(
                EpisodeReaderErrorKind::InvalidConfiguration,
            ));
        }
        Ok(Self {
            history,
            database: history.database_connection(),
            digest,
            code_history: RepositoryCodeHistory::new(history.repository_path()),
            evidence: EvidenceResolver::new(history).map_err(|_| {
                EpisodeReaderError::new(EpisodeReaderErrorKind::InvalidConfiguration)
            })?,
        })
    }

    pub(crate) async fn freeze_view(
        &self,
        context: &AuthenticatedMemoryContext,
    ) -> Result<ResolvedMemoryViewV1, EpisodeReaderError> {
        let transaction = self
            .database
            .begin()
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
        let worktree_scope = WorktreeScope::current();
        let current = Head::current_result_with_conn(&transaction)
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::InvalidCodeAnchor))?;
        let Head::Branch(branch) = current else {
            return Err(EpisodeReaderError::new(
                EpisodeReaderErrorKind::InvalidCodeAnchor,
            ));
        };
        let commit_oid = Head::current_commit_result_with_conn(&transaction)
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::InvalidCodeAnchor))?
            .ok_or_else(|| EpisodeReaderError::new(EpisodeReaderErrorKind::InvalidCodeAnchor))?;
        let full_branch_ref = if branch.starts_with("refs/heads/") {
            branch
        } else {
            format!("refs/heads/{branch}")
        };
        if WorktreeScope::current() != worktree_scope {
            return Err(EpisodeReaderError::new(
                EpisodeReaderErrorKind::InvalidCodeAnchor,
            ));
        }
        let code_anchor =
            FrozenCodeAnchorV1::new(commit_oid, full_branch_ref, worktree_scope.storage_key())
                .map_err(EpisodeReaderError::from)?;
        self.code_history
            .parents(code_anchor.commit_oid())
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::InvalidCodeAnchor))?;
        let view = ResolvedMemoryViewV1::freeze(
            &transaction,
            self.history.repository_path(),
            self.digest,
            context,
            code_anchor,
        )
        .await
        .map_err(EpisodeReaderError::from)?;
        transaction
            .commit()
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
        Ok(view)
    }

    pub(crate) async fn search(
        &self,
        context: &AuthenticatedMemoryContext,
        view: &ResolvedMemoryViewV1,
        query: &EpisodeQueryV1,
    ) -> Result<EpisodeSearchResultV1, EpisodeReaderError> {
        query.validate().map_err(EpisodeReaderError::from)?;
        let transaction = self
            .database
            .begin()
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
        view.revalidate(
            &transaction,
            self.history.repository_path(),
            self.digest,
            context,
        )
        .await
        .map_err(EpisodeReaderError::from)?;
        if view.memory_ref_oid().is_none() {
            transaction
                .commit()
                .await
                .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
            return Ok(EpisodeSearchResultV1::empty(view));
        }

        let projected = search_candidates(&transaction, query)
            .await
            .map_err(EpisodeReaderError::from)?;
        let candidates_examined = projected.len();
        let mut loaded = Vec::with_capacity(projected.len());
        let mut relation_omissions = 0usize;
        for candidate in projected {
            let note = self.load_candidate(&candidate)?;
            if !authorizes_note_read(context, view.repository_id(), &note) {
                continue;
            }
            let episode = note.episode.as_ref().ok_or_else(|| {
                EpisodeReaderError::new(EpisodeReaderErrorKind::CorruptProjection)
            })?;
            if !query.matches_episode(episode) {
                relation_omissions = relation_omissions.saturating_add(1);
                continue;
            }
            let assessment = assess_code_applicability(
                &self.code_history,
                view.code_anchor().commit_oid(),
                &episode.code,
            );
            loaded.push(LoadedEpisode {
                selectable: SelectableEpisode {
                    note_id: candidate.note_id,
                    revision_oid: candidate.revision_oid,
                    bm25_score: candidate.bm25_score,
                    ended_at: candidate.ended_at,
                    applicability: assessment.applicability,
                },
                note,
                read_cost: EpisodeReadCostV1 {
                    projection_rows: 1,
                    note_objects: 1,
                    code_commits_visited: assessment.commits_visited,
                    code_paths_compared: assessment.paths_compared,
                    evidence_items: 0,
                },
            });
        }
        view.revalidate(
            &transaction,
            self.history.repository_path(),
            self.digest,
            context,
        )
        .await
        .map_err(EpisodeReaderError::from)?;
        transaction
            .commit()
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;

        let selectable = loaded
            .iter()
            .map(|candidate| SelectableEpisode {
                note_id: candidate.selectable.note_id,
                revision_oid: candidate.selectable.revision_oid,
                bm25_score: candidate.selectable.bm25_score,
                ended_at: candidate.selectable.ended_at,
                applicability: candidate.selectable.applicability,
            })
            .collect::<Vec<_>>();
        let selected = select_episode_indexes(&selectable, query.include_diagnostics, query.limit);
        let selector_limit_omissions = selectable
            .iter()
            .filter(|candidate| query.include_diagnostics || candidate.applicability.injectable())
            .count()
            .saturating_sub(selected.len());
        let omitted_by_applicability = loaded
            .iter()
            .filter(|candidate| !candidate.selectable.applicability.injectable())
            .count();
        let mut items = Vec::with_capacity(selected.len());
        let mut evidence_reads = 0usize;
        for index in selected {
            let candidate = &loaded[index];
            let evidence = if query.expand_evidence {
                let expanded = self
                    .evidence
                    .expand(
                        context,
                        view.repository_id(),
                        &candidate.note,
                        view.memory_ref_oid(),
                    )
                    .await;
                evidence_reads = evidence_reads
                    .saturating_add(expanded.resolved.len())
                    .saturating_add(expanded.omissions.len());
                expanded
            } else {
                EvidenceExpansionV1::default()
            };
            let mut read_cost = candidate.read_cost;
            read_cost.evidence_items = evidence
                .resolved
                .len()
                .saturating_add(evidence.omissions.len());
            items.push(EpisodeReadItemV1 {
                note: candidate.note.clone(),
                revision_oid: candidate.selectable.revision_oid,
                bm25_score: candidate.selectable.bm25_score,
                applicability: candidate.selectable.applicability,
                evidence,
                read_cost,
            });
        }
        Ok(EpisodeSearchResultV1 {
            view_hash: view.view_hash().to_string(),
            selector_version: EPISODE_SELECTOR_VERSION,
            candidates_examined,
            relation_omissions,
            omitted_by_applicability,
            selector_limit_omissions,
            evidence_reads,
            items,
        })
    }

    /// Load one authorized Episode revision through the same frozen-view gate
    /// as search. Without an explicit revision, only the current confirmed
    /// revision is returned.
    pub(crate) async fn show(
        &self,
        context: &AuthenticatedMemoryContext,
        view: &ResolvedMemoryViewV1,
        note_id: Uuid,
        requested_revision: Option<ObjectHash>,
        expand_evidence: bool,
    ) -> Result<Option<EpisodeReadItemV1>, EpisodeReaderError> {
        let transaction = self
            .database
            .begin()
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
        view.revalidate(
            &transaction,
            self.history.repository_path(),
            self.digest,
            context,
        )
        .await
        .map_err(EpisodeReaderError::from)?;
        let revision_oid = resolve_show_revision(&transaction, note_id, requested_revision).await?;
        let Some(revision_oid) = revision_oid else {
            transaction
                .commit()
                .await
                .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
            return Ok(None);
        };
        // SQLite repositories deliberately use a single pooled connection.
        // Evidence expansion may resolve Agent-history objects through that
        // same database, so keeping this transaction checked out would make
        // every nested lookup wait for its own connection-acquire timeout.
        // The selected revision is immutable; release the first view check,
        // expand it, then revalidate the frozen view in a fresh transaction.
        transaction
            .commit()
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
        let bytes = load_note_bytes(self.history.repository_path(), revision_oid)
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::CorruptProjection))?;
        let note = parse_memory_note_v1(&bytes)
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::CorruptProjection))?;
        if note.note_id != note_id || !authorizes_note_read(context, view.repository_id(), &note) {
            return Err(EpisodeReaderError::new(
                EpisodeReaderErrorKind::Unauthorized,
            ));
        }
        let episode = note
            .episode
            .as_ref()
            .ok_or_else(|| EpisodeReaderError::new(EpisodeReaderErrorKind::CorruptProjection))?;
        let assessment = assess_code_applicability(
            &self.code_history,
            view.code_anchor().commit_oid(),
            &episode.code,
        );
        let evidence = if expand_evidence {
            self.evidence
                .expand(context, view.repository_id(), &note, view.memory_ref_oid())
                .await
        } else {
            EvidenceExpansionV1::default()
        };
        let verification = self
            .database
            .begin()
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
        view.revalidate(
            &verification,
            self.history.repository_path(),
            self.digest,
            context,
        )
        .await
        .map_err(EpisodeReaderError::from)?;
        verification
            .commit()
            .await
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
        Ok(Some(EpisodeReadItemV1 {
            note,
            revision_oid,
            bm25_score: 0.0,
            applicability: assessment.applicability,
            read_cost: EpisodeReadCostV1 {
                projection_rows: 1,
                note_objects: 1,
                code_commits_visited: assessment.commits_visited,
                code_paths_compared: assessment.paths_compared,
                evidence_items: evidence
                    .resolved
                    .len()
                    .saturating_add(evidence.omissions.len()),
            },
            evidence,
        }))
    }

    fn load_candidate(
        &self,
        candidate: &EpisodeSearchCandidate,
    ) -> Result<MemoryNoteV1, EpisodeReaderError> {
        let bytes = load_note_bytes(self.history.repository_path(), candidate.revision_oid)
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::CorruptProjection))?;
        let note = parse_memory_note_v1(&bytes)
            .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::CorruptProjection))?;
        let episode = note
            .episode
            .as_ref()
            .ok_or_else(|| EpisodeReaderError::new(EpisodeReaderErrorKind::CorruptProjection))?;
        if note.note_id != candidate.note_id
            || episode.root_kind != candidate.root_kind
            || episode.root_id != candidate.root_id
            || episode.completion_status != candidate.completion_status
            || episode.code_change_status != candidate.code_change_status
            || episode.ended_at != candidate.ended_at
            || note.valid_from != candidate.valid_from
            || note.valid_until != candidate.valid_until
            || note.expires_at != candidate.expires_at
        {
            return Err(EpisodeReaderError::new(
                EpisodeReaderErrorKind::CorruptProjection,
            ));
        }
        Ok(note)
    }
}

async fn resolve_show_revision<C: ConnectionTrait>(
    database: &C,
    note_id: Uuid,
    requested_revision: Option<ObjectHash>,
) -> Result<Option<ObjectHash>, EpisodeReaderError> {
    let (sql, values) = match requested_revision {
        Some(revision_oid) => (
            "SELECT revision_oid
             FROM memory_revision_index
             WHERE scope_key = 'repo' AND namespace = 'default'
               AND note_id = ? AND revision_oid = ? LIMIT 2",
            vec![note_id.to_string().into(), revision_oid.to_string().into()],
        ),
        None => (
            "SELECT live_revision_oid AS revision_oid
             FROM memory_head
             WHERE scope_key = 'repo' AND namespace = 'default'
               AND note_id = ? AND latest_review_state = 'confirmed' LIMIT 2",
            vec![note_id.to_string().into()],
        ),
    };
    let rows = database
        .query_all_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            sql,
            values,
        ))
        .await
        .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::StorageUnavailable))?;
    if rows.len() > 1 {
        return Err(EpisodeReaderError::new(
            EpisodeReaderErrorKind::CorruptProjection,
        ));
    }
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    let value = row
        .try_get::<Option<String>>("", "revision_oid")
        .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::CorruptProjection))?;
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| EpisodeReaderError::new(EpisodeReaderErrorKind::CorruptProjection))
        })
        .transpose()
}

struct LoadedEpisode {
    selectable: SelectableEpisode,
    note: MemoryNoteV1,
    read_cost: EpisodeReadCostV1,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EpisodeReadItemV1 {
    pub(crate) note: MemoryNoteV1,
    pub(crate) revision_oid: ObjectHash,
    pub(crate) bm25_score: f64,
    pub(crate) applicability: CodeApplicability,
    pub(crate) evidence: EvidenceExpansionV1,
    pub(crate) read_cost: EpisodeReadCostV1,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct EpisodeReadCostV1 {
    pub(crate) projection_rows: usize,
    pub(crate) note_objects: usize,
    pub(crate) code_commits_visited: usize,
    pub(crate) code_paths_compared: usize,
    pub(crate) evidence_items: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EpisodeSearchResultV1 {
    pub(crate) view_hash: String,
    pub(crate) selector_version: &'static str,
    pub(crate) candidates_examined: usize,
    pub(crate) relation_omissions: usize,
    pub(crate) omitted_by_applicability: usize,
    pub(crate) selector_limit_omissions: usize,
    pub(crate) evidence_reads: usize,
    pub(crate) items: Vec<EpisodeReadItemV1>,
}

impl EpisodeSearchResultV1 {
    fn empty(view: &ResolvedMemoryViewV1) -> Self {
        Self {
            view_hash: view.view_hash().to_string(),
            selector_version: EPISODE_SELECTOR_VERSION,
            candidates_examined: 0,
            relation_omissions: 0,
            omitted_by_applicability: 0,
            selector_limit_omissions: 0,
            evidence_reads: 0,
            items: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EpisodeReaderErrorKind {
    InvalidConfiguration,
    InvalidQuery,
    InvalidCodeAnchor,
    Unauthorized,
    StaleProjection,
    UnknownPolicy,
    StorageUnavailable,
    CorruptProjection,
}

#[derive(Debug, Error)]
#[error("Episode reader failed ({kind:?})")]
pub(crate) struct EpisodeReaderError {
    kind: EpisodeReaderErrorKind,
}

impl EpisodeReaderError {
    const fn new(kind: EpisodeReaderErrorKind) -> Self {
        Self { kind }
    }

    #[cfg(test)]
    pub(crate) const fn for_tests(kind: EpisodeReaderErrorKind) -> Self {
        Self::new(kind)
    }

    pub(crate) const fn kind(&self) -> EpisodeReaderErrorKind {
        self.kind
    }
}

impl From<EpisodeQueryError> for EpisodeReaderError {
    fn from(_: EpisodeQueryError) -> Self {
        Self::new(EpisodeReaderErrorKind::InvalidQuery)
    }
}

impl From<MemoryFtsError> for EpisodeReaderError {
    fn from(error: MemoryFtsError) -> Self {
        match error {
            MemoryFtsError::InvalidQuery { .. } => Self::new(EpisodeReaderErrorKind::InvalidQuery),
            MemoryFtsError::CorruptProjection => {
                Self::new(EpisodeReaderErrorKind::CorruptProjection)
            }
            MemoryFtsError::InvalidDocument { .. } | MemoryFtsError::Storage(_) => {
                Self::new(EpisodeReaderErrorKind::StorageUnavailable)
            }
        }
    }
}

impl From<ResolvedMemoryViewError> for EpisodeReaderError {
    fn from(error: ResolvedMemoryViewError) -> Self {
        use super::view::ResolvedMemoryViewErrorKind as View;
        match error.kind() {
            View::InvalidCodeAnchor => Self::new(EpisodeReaderErrorKind::InvalidCodeAnchor),
            View::Unauthorized | View::DigestUnavailable => {
                Self::new(EpisodeReaderErrorKind::Unauthorized)
            }
            View::StorageUnavailable => Self::new(EpisodeReaderErrorKind::StorageUnavailable),
            View::StaleProjection => Self::new(EpisodeReaderErrorKind::StaleProjection),
            View::CorruptProjection => Self::new(EpisodeReaderErrorKind::CorruptProjection),
            View::UnknownPolicy => Self::new(EpisodeReaderErrorKind::UnknownPolicy),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use git_internal::internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
    };
    use sea_orm::{ConnectionTrait, Statement};

    use super::*;
    use crate::{
        internal::ai::{
            context_budget::{
                ContextBudget, ContextSegmentBudget, ContextSegmentKind, TruncationPolicy,
                memory::MemoryContextAssembler,
            },
            memory::{
                domain::{EpisodeRoot, EpisodeRootKind, MemorySensitivity},
                policy::TrustedMemoryTarget,
                writer::tests::{Fixture, fixture, proposal},
            },
            prompt::SystemPromptBuilder,
        },
        utils::{object::write_git_object, storage::local::LocalStorage},
    };

    const CODE_OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    pub(crate) fn history(fixture: &Fixture) -> HistoryManager {
        HistoryManager::new(
            Arc::new(LocalStorage::new(fixture._temp.path().join("objects"))),
            fixture._temp.path().to_path_buf(),
            Arc::clone(&fixture.database),
        )
    }

    async fn frozen_view(fixture: &Fixture) -> ResolvedMemoryViewV1 {
        ResolvedMemoryViewV1::freeze(
            fixture.database.as_ref(),
            fixture._temp.path(),
            fixture.digest.as_ref(),
            &fixture.context,
            FrozenCodeAnchorV1::new(
                CODE_OID.parse().expect("fixed code OID"),
                "refs/heads/main",
                "",
            )
            .expect("valid code anchor"),
        )
        .await
        .expect("freeze reader view")
    }

    fn write_code_commit(
        fixture: &Fixture,
        parents: Vec<ObjectHash>,
        content: &[u8],
    ) -> ObjectHash {
        let blob_oid =
            write_git_object(fixture._temp.path(), "blob", content).expect("write code blob");
        let tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            blob_oid,
            "README.md".to_string(),
        )])
        .expect("construct code tree");
        let tree_oid = write_git_object(
            fixture._temp.path(),
            "tree",
            &tree.to_data().expect("serialize code tree"),
        )
        .expect("write code tree");
        let commit = Commit::new(
            Signature::new(
                SignatureType::Author,
                "Libra Test".to_string(),
                "test@libra.local".to_string(),
            ),
            Signature::new(
                SignatureType::Committer,
                "Libra Test".to_string(),
                "test@libra.local".to_string(),
            ),
            tree_oid,
            parents,
            "reader code anchor",
        );
        write_git_object(
            fixture._temp.path(),
            "commit",
            &commit.to_data().expect("serialize code commit"),
        )
        .expect("write code commit")
    }

    pub(crate) async fn seed_code_head(fixture: &Fixture) -> ObjectHash {
        let commit_oid = write_code_commit(fixture, Vec::new(), b"reader anchor\n");
        fixture
            .database
            .execute_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "INSERT INTO reference(name, kind, `commit`, remote, worktree_id)
                 VALUES ('main', 'Branch', ?, NULL, NULL),
                        ('main', 'Head', NULL, NULL, NULL)",
                [commit_oid.to_string().into()],
            ))
            .await
            .expect("seed current Libra branch and HEAD");
        commit_oid
    }

    async fn advance_code_head(
        fixture: &Fixture,
        parent: ObjectHash,
        content: &[u8],
    ) -> ObjectHash {
        let commit_oid = write_code_commit(fixture, vec![parent], content);
        fixture
            .database
            .execute_raw(Statement::from_sql_and_values(
                fixture.database.get_database_backend(),
                "UPDATE reference SET `commit` = ?
                 WHERE name = 'main' AND kind = 'Branch' AND remote IS NULL",
                [commit_oid.to_string().into()],
            ))
            .await
            .expect("advance current Libra branch");
        commit_oid
    }

    pub(crate) async fn commit_injectable_episode(
        fixture: &Fixture,
        code_commit: ObjectHash,
        task_id: &str,
        generation: u8,
        sensitivity: MemorySensitivity,
        goal: &str,
        summary: &str,
    ) -> TrustedMemoryTarget {
        let target =
            TrustedMemoryTarget::episode(EpisodeRoot::task(task_id).expect("injection target"));
        let mut candidate = proposal(&target, fixture.key_id, generation);
        candidate.note_mut().effective_from_commit = Some(code_commit.to_string());
        candidate.note_mut().sensitivity = sensitivity;
        let episode = candidate
            .note_mut()
            .episode
            .as_mut()
            .expect("Episode payload");
        episode.goal.claim = goal.to_string();
        episode.summary.claim = summary.to_string();
        episode.code.result_oid = Some(code_commit.to_string());
        episode.code.branch_ref = Some("refs/heads/main".to_string());
        episode.code.paths = vec!["README.md".to_string()];
        fixture
            .writer
            .commit(&fixture.context, &target, &candidate, None)
            .await
            .expect("commit injectable Episode");
        target
    }

    #[tokio::test]
    async fn reader_freezes_current_libra_head() {
        let fixture = fixture().await;
        let commit_oid = seed_code_head(&fixture).await;
        let history = history(&fixture);
        let reader = EpisodeReader::new(&history, fixture.digest.as_ref()).expect("reader");
        let view = reader
            .freeze_view(&fixture.context)
            .await
            .expect("freeze current Libra HEAD");
        assert_eq!(view.code_anchor().commit_oid(), commit_oid);
        assert_eq!(view.code_anchor().full_branch_ref(), "refs/heads/main");
    }

    #[tokio::test]
    async fn injection_budget_and_receipt_gate_prompt_delivery() {
        let fixture = fixture().await;
        let code_commit = seed_code_head(&fixture).await;
        let selected_target = commit_injectable_episode(
            &fixture,
            code_commit,
            "task-injection-small",
            1,
            MemorySensitivity::Internal,
            "budgettoken authentication retry",
            "The bounded retry restored the request.",
        )
        .await;
        let omitted_target = commit_injectable_episode(
            &fixture,
            code_commit,
            "task-injection-large",
            2,
            MemorySensitivity::Internal,
            "budgettoken authentication retry",
            &"largecontext ".repeat(250),
        )
        .await;
        let history = history(&fixture);
        let assembler = MemoryContextAssembler::new(&history, Arc::clone(&fixture.digest));
        let budget = ContextBudget::from_segments(
            700,
            vec![ContextSegmentBudget::new(
                ContextSegmentKind::ProjectMemory,
                700,
                TruncationPolicy::OldestFirst,
            )],
        )
        .expect("bounded Memory budget");
        let effective_at = Utc
            .with_ymd_and_hms(2026, 8, 25, 12, 0, 0)
            .single()
            .expect("fixed effective time");
        let query = EpisodeQueryV1 {
            text: Some("budgettoken".to_string()),
            ..EpisodeQueryV1::default()
        };

        let bundle = assembler
            .assemble(&fixture.context, &query, &budget, effective_at)
            .await
            .expect("assemble audited Memory context");
        assert_eq!(bundle.receipt().selected().len(), 1);
        assert_eq!(
            bundle.receipt().selected()[0].object_id,
            selected_target.root().note_id().to_string(),
        );
        assert!(bundle.receipt().omissions().iter().any(|omission| {
            matches!(
                omission.reason_code.as_str(),
                "segment_budget" | "total_budget"
            ) && omission.count == 1
        }));
        assert!(
            bundle
                .prompt_section()
                .contains(selected_target.root().id())
        );
        assert!(!bundle.prompt_section().contains(omitted_target.root().id()));
        assert_eq!(bundle.receipt().effective_at(), effective_at);
        assert_eq!(
            bundle.receipt().code_commit().map(str::to_string),
            Some(code_commit.to_string()),
        );
        assert_eq!(bundle.receipt().full_branch_ref(), Some("refs/heads/main"));
        assert_eq!(bundle.receipt().token_budget(), 700);
        assert_eq!(
            bundle.receipt().source_heads().get("memory_repo"),
            bundle.receipt().projection_watermarks().get("memory_repo"),
        );
        assert!(bundle.receipt().policy_hash().starts_with("sha256:"));
        assert_eq!(
            bundle.receipt().selector_version(),
            "episode-fts-bm25-v1+context-budget-v1",
        );
        assert!(!bundle.receipt().query_hmac().contains("budgettoken"));

        let prompt = SystemPromptBuilder::new(fixture._temp.path())
            .expect("prompt builder")
            .with_memory_bundle(&bundle)
            .build()
            .expect("deliver audited bundle to prompt");
        assert!(prompt.contains("## Retrieved Project Memory"));
        assert!(prompt.contains(selected_target.root().id()));
    }

    #[tokio::test]
    async fn injection_replay_is_deterministic_and_view_identity_changes() {
        let fixture = fixture().await;
        let code_commit = seed_code_head(&fixture).await;
        let target = commit_injectable_episode(
            &fixture,
            code_commit,
            "task-injection-replay",
            1,
            MemorySensitivity::Internal,
            "replaytoken branch invariant",
            "The branch invariant remained stable.",
        )
        .await;
        let history = history(&fixture);
        let reader = EpisodeReader::new(&history, fixture.digest.as_ref()).expect("reader");
        let frozen = reader
            .freeze_view(&fixture.context)
            .await
            .expect("freeze replay view");
        let assembler = MemoryContextAssembler::new(&history, Arc::clone(&fixture.digest));
        let budget = ContextBudget::default();
        let effective_at = Utc
            .with_ymd_and_hms(2026, 8, 25, 12, 30, 0)
            .single()
            .expect("fixed effective time");
        let query = EpisodeQueryV1 {
            text: Some("replaytoken".to_string()),
            ..EpisodeQueryV1::default()
        };

        let first = assembler
            .assemble_for_view(&fixture.context, &frozen, &query, &budget, effective_at)
            .await
            .expect("first frozen selection");
        let repeated = assembler
            .assemble_for_view(&fixture.context, &frozen, &query, &budget, effective_at)
            .await
            .expect("repeated frozen selection");
        assert_ne!(
            first.receipt().receipt_id(),
            repeated.receipt().receipt_id()
        );
        assert_eq!(first.prompt_section(), repeated.prompt_section());
        assert_eq!(first.receipt().selected(), repeated.receipt().selected());
        assert_eq!(
            first.receipt().bundle_hash(),
            repeated.receipt().bundle_hash()
        );
        assert_eq!(
            first.receipt().query_hmac(),
            repeated.receipt().query_hmac()
        );

        let next_commit = advance_code_head(&fixture, code_commit, b"reader anchor\n").await;
        let next = assembler
            .assemble(&fixture.context, &query, &budget, effective_at)
            .await
            .expect("selection on descendant unchanged code");
        assert_ne!(first.view_hash(), next.view_hash());
        assert_ne!(first.receipt().query_hmac(), next.receipt().query_hmac());
        assert_eq!(
            next.receipt().code_commit().map(str::to_string),
            Some(next_commit.to_string()),
        );
        assert_eq!(next.receipt().selected().len(), 1);
        assert_eq!(
            next.receipt().selected()[0].object_id,
            target.root().note_id().to_string(),
        );
    }

    #[tokio::test]
    async fn injection_secret_and_stale_or_path_changed_are_excluded() {
        let fixture = fixture().await;
        let code_commit = seed_code_head(&fixture).await;
        let history = history(&fixture);
        let reader = EpisodeReader::new(&history, fixture.digest.as_ref()).expect("reader");
        let stale_view = reader
            .freeze_view(&fixture.context)
            .await
            .expect("freeze pre-Memory view");

        let secret_target = TrustedMemoryTarget::episode(
            EpisodeRoot::task("task-injection-secret").expect("secret target"),
        );
        let mut secret = proposal(&secret_target, fixture.key_id, 1);
        secret.note_mut().sensitivity = MemorySensitivity::SecretLike;
        let secret_result = fixture
            .writer
            .commit(&fixture.context, &secret_target, &secret, None)
            .await;
        assert!(
            secret_result.is_err(),
            "secret-like Episode must not persist"
        );

        commit_injectable_episode(
            &fixture,
            code_commit,
            "task-injection-confidential",
            1,
            MemorySensitivity::Confidential,
            "confidentialtoken",
            "This local-only detail must not enter a provider prompt by default.",
        )
        .await;

        commit_injectable_episode(
            &fixture,
            code_commit,
            "task-injection-path-changed",
            2,
            MemorySensitivity::Internal,
            "pathchangedtoken",
            "The original path evidence applied to the old tree.",
        )
        .await;
        let assembler = MemoryContextAssembler::new(&history, Arc::clone(&fixture.digest));
        let query = EpisodeQueryV1 {
            text: Some("pathchangedtoken".to_string()),
            ..EpisodeQueryV1::default()
        };
        let effective_at = Utc
            .with_ymd_and_hms(2026, 8, 25, 12, 45, 0)
            .single()
            .expect("fixed effective time");
        let confidential = assembler
            .assemble(
                &fixture.context,
                &EpisodeQueryV1 {
                    text: Some("confidentialtoken".to_string()),
                    ..EpisodeQueryV1::default()
                },
                &ContextBudget::default(),
                effective_at,
            )
            .await
            .expect("audit confidential omission");
        assert!(confidential.receipt().selected().is_empty());
        assert!(confidential.prompt_section().is_empty());
        assert!(confidential.receipt().omissions().iter().any(|omission| {
            omission.reason_code == "sensitivity_policy" && omission.count == 1
        }));
        let stale = assembler
            .assemble_for_view(
                &fixture.context,
                &stale_view,
                &query,
                &ContextBudget::default(),
                effective_at,
            )
            .await;
        let stale_error = match stale {
            Ok(_) => panic!("advanced Memory projection must invalidate the old view"),
            Err(error) => error,
        };
        assert_eq!(
            stale_error.kind(),
            crate::internal::ai::context_budget::memory::MemoryContextAssemblerErrorKind::StaleView,
        );

        advance_code_head(&fixture, code_commit, b"changed reader anchor\n").await;
        let changed = assembler
            .assemble(
                &fixture.context,
                &query,
                &ContextBudget::default(),
                effective_at,
            )
            .await
            .expect("audit path-changed omission");
        assert!(changed.receipt().selected().is_empty());
        assert!(changed.prompt_section().is_empty());
        assert!(changed.receipt().omissions().iter().any(|omission| {
            omission.reason_code == "code_applicability" && omission.count == 1
        }));
    }

    #[tokio::test]
    async fn injection_receipt_failure_returns_no_bundle() {
        let fixture = fixture().await;
        let code_commit = seed_code_head(&fixture).await;
        commit_injectable_episode(
            &fixture,
            code_commit,
            "task-injection-receipt-failure",
            1,
            MemorySensitivity::Internal,
            "receiptfailuretoken",
            "This candidate must never cross a failed audit gate.",
        )
        .await;
        let history = history(&fixture);
        let assembler = MemoryContextAssembler::new(&history, Arc::clone(&fixture.digest));
        fixture
            .database
            .execute_unprepared("DROP TABLE context_selection_receipt")
            .await
            .expect("remove fixture receipt ledger");
        let result = assembler
            .assemble(
                &fixture.context,
                &EpisodeQueryV1 {
                    text: Some("receiptfailuretoken".to_string()),
                    ..EpisodeQueryV1::default()
                },
                &ContextBudget::default(),
                Utc.with_ymd_and_hms(2026, 8, 25, 13, 0, 0)
                    .single()
                    .expect("fixed effective time"),
            )
            .await;
        let error = match result {
            Ok(_) => panic!("receipt failure must stop bundle delivery"),
            Err(error) => error,
        };
        assert_eq!(
            error.kind(),
            crate::internal::ai::context_budget::memory::MemoryContextAssemblerErrorKind::ReceiptStore,
        );
    }

    #[tokio::test]
    async fn reader_bm25_weights_asc() {
        let fixture = fixture().await;
        let goal_target =
            TrustedMemoryTarget::episode(EpisodeRoot::task("task-rank-goal").expect("goal target"));
        let summary_target = TrustedMemoryTarget::episode(
            EpisodeRoot::task("task-rank-summary").expect("summary target"),
        );
        let mut goal = proposal(&goal_target, fixture.key_id, 1);
        let goal_episode = goal.note_mut().episode.as_mut().expect("Episode payload");
        goal_episode.goal.claim = "ranktoken appears in the goal".to_string();
        goal_episode.summary.claim = "an unrelated summary".to_string();
        let mut summary = proposal(&summary_target, fixture.key_id, 2);
        let summary_episode = summary
            .note_mut()
            .episode
            .as_mut()
            .expect("Episode payload");
        summary_episode.goal.claim = "an unrelated goal".to_string();
        summary_episode.summary.claim = "ranktoken appears in the summary".to_string();
        fixture
            .writer
            .commit(&fixture.context, &goal_target, &goal, None)
            .await
            .expect("commit goal-weighted Episode");
        fixture
            .writer
            .commit(&fixture.context, &summary_target, &summary, None)
            .await
            .expect("commit summary-weighted Episode");

        let history = history(&fixture);
        let reader = EpisodeReader::new(&history, fixture.digest.as_ref()).expect("reader");
        let result = reader
            .search(
                &fixture.context,
                &frozen_view(&fixture).await,
                &EpisodeQueryV1 {
                    text: Some("ranktoken".to_string()),
                    include_diagnostics: true,
                    ..EpisodeQueryV1::default()
                },
            )
            .await
            .expect("search weighted documents");
        assert_eq!(result.items.len(), 2);
        assert_eq!(
            result.items[0]
                .note
                .episode
                .as_ref()
                .expect("Episode")
                .root_id,
            goal_target.root().id(),
        );
        assert!(result.items[0].bm25_score < result.items[1].bm25_score);
    }

    #[tokio::test]
    async fn reader_deterministic_ties_and_structured_filters() {
        let fixture = fixture().await;
        let ended_at = Utc
            .with_ymd_and_hms(2026, 8, 24, 12, 0, 0)
            .single()
            .expect("fixed timestamp");
        let mut expected_ids = Vec::new();
        for (task_id, generation) in [("task-tie-b", 1), ("task-tie-a", 2)] {
            let target =
                TrustedMemoryTarget::episode(EpisodeRoot::task(task_id).expect("tie target"));
            let mut candidate = proposal(&target, fixture.key_id, generation);
            let episode = candidate
                .note_mut()
                .episode
                .as_mut()
                .expect("Episode payload");
            episode.goal.claim = "deterministictoken".to_string();
            episode.summary.claim = "same summary".to_string();
            episode.ended_at = Some(ended_at);
            fixture
                .writer
                .commit(&fixture.context, &target, &candidate, None)
                .await
                .expect("commit tied Episode");
            expected_ids.push(target.root().note_id());
        }
        expected_ids.sort();

        let history = history(&fixture);
        let reader = EpisodeReader::new(&history, fixture.digest.as_ref()).expect("reader");
        let view = frozen_view(&fixture).await;
        let query = EpisodeQueryV1 {
            text: Some("deterministictoken".to_string()),
            root_kind: Some(EpisodeRootKind::Task),
            root_id: Some("task-tie-a".to_string()),
            related_task_id: Some("task-tie-a".to_string()),
            ended_from: Some(ended_at),
            ended_until: Some(ended_at),
            path: Some(super::super::query::EpisodePathFilter::Prefix(
                "episodic.tasks".to_string(),
            )),
            include_diagnostics: true,
            ..EpisodeQueryV1::default()
        };
        let filtered = reader
            .search(&fixture.context, &view, &query)
            .await
            .expect("search structured filters");
        assert_eq!(filtered.items.len(), 1);
        assert_eq!(
            filtered.items[0]
                .note
                .episode
                .as_ref()
                .expect("Episode")
                .root_id,
            "task-tie-a",
        );

        let unfiltered = EpisodeQueryV1 {
            text: Some("deterministictoken".to_string()),
            include_diagnostics: true,
            ..EpisodeQueryV1::default()
        };
        let first = reader
            .search(&fixture.context, &view, &unfiltered)
            .await
            .expect("first deterministic search");
        let second = reader
            .search(&fixture.context, &view, &unfiltered)
            .await
            .expect("second deterministic search");
        assert_eq!(first, second);
        assert_eq!(
            first
                .items
                .iter()
                .map(|item| item.note.note_id)
                .collect::<Vec<_>>(),
            expected_ids,
        );
    }

    #[tokio::test]
    async fn reader_only_returns_the_confirmed_live_revision() {
        let fixture = fixture().await;
        let mut old = proposal(&fixture.target, fixture.key_id, 1);
        old.note_mut()
            .episode
            .as_mut()
            .expect("Episode payload")
            .goal
            .claim = "oldrevisiontoken".to_string();
        fixture
            .writer
            .commit(&fixture.context, &fixture.target, &old, None)
            .await
            .expect("commit old revision");
        let mut current = proposal(&fixture.target, fixture.key_id, 2);
        current
            .note_mut()
            .episode
            .as_mut()
            .expect("Episode payload")
            .goal
            .claim = "currentrevisiontoken".to_string();
        fixture
            .writer
            .commit(&fixture.context, &fixture.target, &current, None)
            .await
            .expect("commit current revision");

        let history = history(&fixture);
        let reader = EpisodeReader::new(&history, fixture.digest.as_ref()).expect("reader");
        let view = frozen_view(&fixture).await;
        let old_result = reader
            .search(
                &fixture.context,
                &view,
                &EpisodeQueryV1 {
                    text: Some("oldrevisiontoken".to_string()),
                    include_diagnostics: true,
                    ..EpisodeQueryV1::default()
                },
            )
            .await
            .expect("search old posting");
        assert!(old_result.items.is_empty());
        let current_result = reader
            .search(
                &fixture.context,
                &view,
                &EpisodeQueryV1 {
                    text: Some("currentrevisiontoken".to_string()),
                    include_diagnostics: true,
                    ..EpisodeQueryV1::default()
                },
            )
            .await
            .expect("search current posting");
        assert_eq!(current_result.items.len(), 1);
    }
}
