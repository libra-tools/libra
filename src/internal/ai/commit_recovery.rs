//! Bounded, repository-local reconstruction of agent commit links.
//! Detection is read-only. Doctor prepares the bounded commit graph outside
//! the writer lock, then rechecks HEAD, scope, tombstones and links inside the
//! repair transaction. No commit object is rewritten.

use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    path::Path,
};

use anyhow::{Context, Result, bail};
use git_internal::{
    hash::ObjectHash,
    internal::object::{ObjectTrait, commit::Commit, tag::Tag, types::ObjectType},
};
use sea_orm::{ConnectionTrait, Statement};
use serde::Serialize;

use crate::{
    common_utils::parse_commit_msg,
    internal::{
        ai::{capture_scope::CaptureScope, commit_journal},
        object_format::parse_repo_oid,
    },
    storage::{
        capture_store::{
            AgentSessionCommitRecord, CaptureCheckpointScope, CaptureHashKind, CaptureSessionState,
            CaptureStore,
        },
        git_state_store::GitStateStore,
        git_store::{GitStore, RefKey, RefTarget},
    },
    utils::storage::{Storage, local::LocalStorage, tiered::verify_fetched_object},
};

const COMMITS_PER_PAGE: usize = 100;
const MAX_PAGE: u16 = 100;
const MAX_VISITED_OBJECTS: usize = 12_000;
const MAX_COMMIT_BYTES: u64 = 256 * 1024;
const MAX_LINKS_PER_REPAIR: usize = 64;
const MAX_CANDIDATES_PER_PAGE: usize = 128;
const MAX_TRAILERS_PER_COMMIT: usize = 64;

#[derive(Debug, Serialize)]
pub struct CommitRecoveryReport {
    pub evidence_state: &'static str,
    pub evidence_completeness: &'static str,
    pub commit_page: u16,
    pub next_commit_page: Option<u16>,
    pub history_incomplete: bool,
    pub journal_records: usize,
    pub journal_bad_tail: bool,
    pub journal_unavailable: bool,
    pub journal_unavailable_reason: Option<&'static str>,
    pub journal_reset: bool,
    pub foreign_scope_intents: usize,
    pub unmatched_scope_intents: usize,
    pub reachable_commits_scanned: usize,
    pub trailer_candidates: usize,
    pub candidate_links: usize,
    pub missing_links: usize,
    pub repaired_links: usize,
    pub rejected_links: usize,
    pub more_missing_links: bool,
    pub repair_applied: bool,
    pub permission_blocked: bool,
    pub cleanup_incomplete: bool,
}

impl CommitRecoveryReport {
    fn new(page: u16) -> Self {
        Self {
            evidence_state: "no_evidence",
            // A local journal and reachable trailers cannot prove that every
            // prior trailer-free link survived a cloud restore or erasure.
            evidence_completeness: "unknown",
            commit_page: page,
            next_commit_page: None,
            history_incomplete: false,
            journal_records: 0,
            journal_bad_tail: false,
            journal_unavailable: false,
            journal_unavailable_reason: None,
            journal_reset: false,
            foreign_scope_intents: 0,
            unmatched_scope_intents: 0,
            reachable_commits_scanned: 0,
            trailer_candidates: 0,
            candidate_links: 0,
            missing_links: 0,
            repaired_links: 0,
            rejected_links: 0,
            more_missing_links: false,
            repair_applied: false,
            permission_blocked: false,
            cleanup_incomplete: false,
        }
    }

    pub fn unavailable(page: u16, _repair: bool) -> Self {
        let mut report = Self::new(page);
        report.evidence_state = "unavailable";
        report
    }
}

fn oid_kind(oid: ObjectHash) -> &'static str {
    match oid.kind() {
        git_internal::hash::HashKind::Sha1 => "sha1",
        git_internal::hash::HashKind::Sha256 => "sha256",
        git_internal::hash::HashKind::Blake3 => "blake3",
    }
}

fn capture_hash_kind(oid: ObjectHash) -> CaptureHashKind {
    match oid.kind() {
        git_internal::hash::HashKind::Sha1 => CaptureHashKind::Sha1,
        git_internal::hash::HashKind::Sha256 => CaptureHashKind::Sha256,
        git_internal::hash::HashKind::Blake3 => CaptureHashKind::Blake3,
    }
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte))
}

#[derive(Debug)]
pub struct PreparedPage {
    head: Option<ObjectHash>,
    reachable: BTreeMap<String, Commit>,
    has_more: bool,
    incomplete: bool,
    trailer_candidates: Vec<(String, String, ObjectHash)>,
    trailer_limit_reached: bool,
}

async fn current_head<C: ConnectionTrait>(
    conn: &C,
    scope: &CaptureScope,
) -> Result<(Option<ObjectHash>, bool)> {
    // A foreign remote or another worktree's branch can carry an arbitrary
    // trailer naming a local session. Recovery authority starts only at this
    // invocation's current HEAD; switch worktrees/branches to inspect others.
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT COALESCE(h.\"commit\", b.\"commit\") FROM reference h
         LEFT JOIN reference b ON h.\"commit\" IS NULL
             AND b.kind = 'Branch' AND b.name = h.name
             AND b.remote IS NULL AND b.worktree_id IS NULL
         WHERE h.kind = 'Head' AND h.remote IS NULL AND COALESCE(h.worktree_id, '') = ?
         ORDER BY h.id, b.id LIMIT 2",
            [scope.worktree_id.clone().into()],
        ))
        .await
        .context("read current-worktree commit-link HEAD")?;
    if rows.len() > 1 {
        // Two HEAD rows for one worktree are ambiguous authority. Preserve
        // the diagnosis, but select neither as a repair root.
        return Ok((None, true));
    }
    let mut incomplete = false;
    let mut head = None;
    for row in rows {
        let value: Option<String> = row
            .try_get_by_index(0)
            .context("decode commit-link ref target")?;
        match value {
            Some(value) => match parse_repo_oid(&value) {
                Ok(oid) => head = Some(oid),
                Err(_) => incomplete = true,
            },
            None => incomplete = true,
        }
    }
    Ok((head, incomplete))
}

/// Resolve only this worktree's local HEAD, then read bounded objects without
/// holding the SQLite writer lock. Repair revalidates the HEAD under its lock.
pub async fn prepare_page<C: ConnectionTrait>(
    conn: &C,
    storage_root: &Path,
    scope: &CaptureScope,
    page: u16,
) -> Result<PreparedPage> {
    if page == 0 || page > MAX_PAGE {
        bail!("commit-link page must be between 1 and 100");
    }
    let (head, mut incomplete) = current_head(conn, scope).await?;
    let mut queue = VecDeque::new();
    if let Some(oid) = head {
        queue.push_back(oid);
    }
    let storage = LocalStorage::open_no_create_with_alternates(storage_root.join("objects"));
    let mut visited = HashSet::new();
    let mut commits_seen = 0usize;
    let mut selected = BTreeMap::new();
    let first = (usize::from(page) - 1) * COMMITS_PER_PAGE;
    let end = first + COMMITS_PER_PAGE;
    while let Some(oid) = queue.pop_front() {
        if !visited.insert(oid) {
            continue;
        }
        if visited.len() > MAX_VISITED_OBJECTS {
            incomplete = true;
            break;
        }
        let fetched = storage.get_with_limit(&oid, MAX_COMMIT_BYTES).await;
        let Ok((payload, object_type)) = fetched else {
            incomplete = true;
            continue;
        };
        if verify_fetched_object(&oid, object_type, &payload).is_err() {
            incomplete = true;
            continue;
        }
        match object_type {
            ObjectType::Commit => {
                let Ok(commit) = Commit::from_bytes(&payload, oid) else {
                    incomplete = true;
                    continue;
                };
                queue.extend(commit.parent_commit_ids.iter().copied());
                if commits_seen >= first && commits_seen < end {
                    selected.insert(oid.to_string(), commit);
                }
                commits_seen += 1;
                if commits_seen >= end {
                    break;
                }
            }
            ObjectType::Tag => match Tag::from_bytes(&payload, oid) {
                Ok(tag) => queue.push_back(tag.object_hash),
                Err(_) => incomplete = true,
            },
            _ => incomplete = true,
        }
    }
    let has_more = queue.iter().any(|oid| !visited.contains(oid));
    let mut trailer_candidates = Vec::new();
    let mut trailer_limit_reached = false;
    'commits: for (oid, commit) in &selected {
        let message = parse_commit_msg(&commit.message).0;
        let mut per_commit = 0;
        for trailer in crate::internal::log::trailer::parse_trailers(message) {
            if !trailer.key_matches("Libra-Agent-Session") || !valid_session_id(&trailer.value) {
                continue;
            }
            if per_commit >= MAX_TRAILERS_PER_COMMIT
                || trailer_candidates.len() >= MAX_CANDIDATES_PER_PAGE
            {
                trailer_limit_reached = true;
                break 'commits;
            }
            trailer_candidates.push((oid.clone(), trailer.value, commit.id));
            per_commit += 1;
        }
    }
    Ok(PreparedPage {
        head,
        reachable: selected,
        has_more,
        incomplete,
        trailer_candidates,
        trailer_limit_reached,
    })
}

async fn session_eligible<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    scope: &CaptureScope,
) -> Result<bool> {
    let mut values = vec![session_id.into()];
    values.extend(scope.sql_values());
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            format!(
                "SELECT 1 FROM agent_session s WHERE s.session_id = ?
             AND {} AND s.state IN ('active','condensed','stopped')
             AND NOT EXISTS (SELECT 1 FROM agent_import_tombstone t
                             WHERE t.erased_session_id = s.session_id)
             AND EXISTS (SELECT 1 FROM agent_checkpoint c
                         WHERE c.session_id = s.session_id AND c.scope = 'committed')",
                CaptureScope::matches_existing_sql("s")
            ),
            values,
        ))
        .await
        .context("recheck commit-link recovery session scope")?
        .is_some())
}

async fn session_owned_and_not_erased<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    scope: &CaptureScope,
) -> Result<bool> {
    let mut values = vec![session_id.into()];
    values.extend(scope.sql_values());
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            format!(
                "SELECT 1 FROM agent_session s WHERE s.session_id = ? AND {}
                 AND NOT EXISTS (SELECT 1 FROM agent_import_tombstone t
                                 WHERE t.erased_session_id = s.session_id)",
                CaptureScope::matches_existing_sql("s")
            ),
            values,
        ))
        .await
        .context("recheck completed agent commit journal session ownership")?
        .is_some())
}

async fn link_exists<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    oid: ObjectHash,
) -> Result<bool> {
    Ok(conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT 1 FROM agent_session_commit WHERE session_id = ? AND hash_kind = ? AND commit_oid = ?",
        [session_id.into(), oid_kind(oid).into(), oid.to_string().into()],
    )).await.context("read existing agent commit link")?.is_some())
}

async fn link_count<C: ConnectionTrait>(conn: &C, oid: ObjectHash) -> Result<i64> {
    conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT COUNT(*) FROM agent_session_commit WHERE hash_kind = ? AND commit_oid = ?",
        [oid_kind(oid).into(), oid.to_string().into()],
    ))
    .await
    .context("count existing agent commit links")?
    .context("commit-link count query returned no row")?
    .try_get_by_index(0)
    .context("decode agent commit-link count")
}

/// Apply a page prepared outside the writer lock. A changed HEAD invalidates
/// the evidence rather than applying it to a different branch.
pub async fn inspect_prepared<R: ConnectionTrait, C: ConnectionTrait>(
    repo: &R,
    catalog: &C,
    storage_root: &Path,
    scope: &CaptureScope,
    page: u16,
    repair: bool,
    prepared: &PreparedPage,
) -> Result<CommitRecoveryReport> {
    if repair && current_head(repo, scope).await?.0 != prepared.head {
        bail!("current HEAD changed during commit-link inspection; retry doctor --repair");
    }
    let mut report = CommitRecoveryReport::new(page);
    let (records, bad_tail) = match commit_journal::read(storage_root) {
        Ok(result) => result,
        Err(_) => {
            report.journal_unavailable = true;
            report.journal_unavailable_reason = commit_journal::read_issue(storage_root);
            (Vec::new(), false)
        }
    };
    report.journal_records = records.len();
    report.journal_bad_tail = bad_tail;
    let reachable = &prepared.reachable;
    let has_more = prepared.has_more;
    let incomplete = prepared.incomplete;
    report.reachable_commits_scanned = reachable.len();
    report.history_incomplete =
        incomplete || prepared.trailer_limit_reached || (has_more && page == MAX_PAGE);
    report.next_commit_page = (has_more && page < MAX_PAGE).then_some(page + 1);
    let mut candidates: BTreeMap<(String, String), ObjectHash> = BTreeMap::new();
    for record in &records {
        if &record.scope != scope {
            report.foreign_scope_intents += 1;
            continue;
        }
        let Some(commit) = reachable.get(&record.commit_oid) else {
            report.unmatched_scope_intents += 1;
            continue;
        };
        if record.hash_kind != oid_kind(commit.id) {
            report.unmatched_scope_intents += 1;
            continue;
        }
        for session_id in &record.session_ids {
            if candidates.len() >= MAX_CANDIDATES_PER_PAGE {
                report.more_missing_links = true;
                report.history_incomplete = true;
                break;
            }
            candidates.insert((record.commit_oid.clone(), session_id.clone()), commit.id);
        }
    }
    for (oid, session_id, commit_id) in &prepared.trailer_candidates {
        if candidates.len() >= MAX_CANDIDATES_PER_PAGE {
            report.more_missing_links = true;
            report.history_incomplete = true;
            break;
        }
        report.trailer_candidates += 1;
        candidates.insert((oid.clone(), session_id.clone()), *commit_id);
    }
    report.candidate_links = candidates.len();
    report.evidence_state = if report.journal_unavailable {
        "unavailable"
    } else if report.journal_records > 0 || report.trailer_candidates > 0 {
        "available"
    } else {
        "no_evidence"
    };
    if repair
        && scope
            .assert_workspace_fence_live_for_commit(repo)
            .await
            .is_err()
    {
        report.permission_blocked = true;
        return Ok(report);
    }
    report.repair_applied = repair;
    for ((_, session_id), oid) in candidates {
        if link_exists(catalog, &session_id, oid).await? {
            continue;
        }
        if !session_eligible(catalog, &session_id, scope).await? {
            report.rejected_links += 1;
            continue;
        }
        report.missing_links += 1;
        if !repair {
            continue;
        }
        if report.repaired_links >= MAX_LINKS_PER_REPAIR {
            report.more_missing_links = true;
            break;
        }
        if link_count(catalog, oid).await? >= 64 {
            report.rejected_links += 1;
            continue;
        }
        let inserted = catalog
            .execute_raw(Statement::from_sql_and_values(
                catalog.get_database_backend(),
                "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid)
             VALUES (?, ?, ?) ON CONFLICT(session_id, hash_kind, commit_oid) DO NOTHING",
                [
                    session_id.into(),
                    oid_kind(oid).into(),
                    oid.to_string().into(),
                ],
            ))
            .await
            .context("repair authorized agent commit link")?;
        report.repaired_links += inserted.rows_affected() as usize;
    }
    if repair && bad_tail && !report.journal_unavailable {
        commit_journal::repair_torn_tail(storage_root)
            .context("repair torn agent commit journal tail")?;
    }
    Ok(report)
}

/// Called only after doctor has committed repaired DAO rows. A fresh writer
/// transaction fences erase and append while the now-durable links are checked
/// again. Never remove prepared evidence before its replacement row commits.
pub async fn reclaim_reconciled<R: ConnectionTrait, C: ConnectionTrait>(
    repo: &R,
    catalog: &C,
    storage_root: &Path,
    scope: &CaptureScope,
    prepared: &PreparedPage,
) -> Result<usize> {
    scope.assert_workspace_fence_live_for_commit(repo).await?;
    if current_head(repo, scope).await?.0 != prepared.head {
        bail!("current HEAD changed before agent commit journal cleanup; retry doctor --repair");
    }
    let (records, bad_tail) = commit_journal::read(storage_root)?;
    if bad_tail {
        bail!("repair the torn agent commit journal tail before reclaiming recovered links");
    }
    let reachable = &prepared.reachable;
    let mut reconciled = Vec::new();
    for record in records {
        if &record.scope != scope {
            continue;
        }
        let Some(commit) = reachable.get(&record.commit_oid) else {
            continue;
        };
        if record.hash_kind != oid_kind(commit.id) {
            continue;
        }
        let mut complete = true;
        for session_id in &record.session_ids {
            if !session_owned_and_not_erased(catalog, session_id, scope).await?
                || !link_exists(catalog, session_id, commit.id).await?
            {
                complete = false;
                break;
            }
        }
        if complete {
            reconciled.push(record);
        }
    }
    commit_journal::compact_reconciled(storage_root, &reconciled)
        .context("reclaim verified agent commit journal recovery records")
}

async fn current_head_with_git_store(
    git_store: &dyn GitStore,
    scope: &CaptureScope,
) -> Result<(Option<ObjectHash>, bool)> {
    let worktree_id = (!scope.worktree_id.is_empty()).then(|| scope.worktree_id.clone());
    let head_target = git_store
        .get_ref(&RefKey::LocalHead { worktree_id })
        .await
        .context("read current-worktree commit-link HEAD from git store")?;
    let Some(target) = head_target else {
        return Ok((None, false));
    };
    let resolved = match target {
        RefTarget::Direct(oid) => Some(oid),
        RefTarget::Symbolic(branch_name) => match git_store
            .get_ref(&RefKey::LocalBranch { name: branch_name })
            .await
            .context("resolve symbolic HEAD branch from git store")?
        {
            Some(RefTarget::Direct(oid)) => Some(oid),
            _ => None,
        },
    };
    match resolved {
        Some(value) => match parse_repo_oid(&value) {
            Ok(oid) => Ok((Some(oid), false)),
            Err(_) => Ok((None, true)),
        },
        None => Ok((None, true)),
    }
}

/// Resolve only this worktree's local HEAD via a read-only [`GitStore`] handle,
/// then read bounded commit objects without opening SQLite (`RB-22`).
pub async fn prepare_page_with_git_store(
    git_store: &dyn GitStore,
    storage_root: &Path,
    scope: &CaptureScope,
    page: u16,
) -> Result<PreparedPage> {
    if page == 0 || page > MAX_PAGE {
        bail!("commit-link page must be between 1 and 100");
    }
    let (head, mut incomplete) = current_head_with_git_store(git_store, scope).await?;
    let mut queue = VecDeque::new();
    if let Some(oid) = head {
        queue.push_back(oid);
    }
    let storage = LocalStorage::open_no_create_with_alternates(storage_root.join("objects"));
    let mut visited = HashSet::new();
    let mut commits_seen = 0usize;
    let mut selected = BTreeMap::new();
    let first = (usize::from(page) - 1) * COMMITS_PER_PAGE;
    let end = first + COMMITS_PER_PAGE;
    while let Some(oid) = queue.pop_front() {
        if !visited.insert(oid) {
            continue;
        }
        if visited.len() > MAX_VISITED_OBJECTS {
            incomplete = true;
            break;
        }
        let fetched = storage.get_with_limit(&oid, MAX_COMMIT_BYTES).await;
        let Ok((payload, object_type)) = fetched else {
            incomplete = true;
            continue;
        };
        if verify_fetched_object(&oid, object_type, &payload).is_err() {
            incomplete = true;
            continue;
        }
        match object_type {
            ObjectType::Commit => {
                let Ok(commit) = Commit::from_bytes(&payload, oid) else {
                    incomplete = true;
                    continue;
                };
                queue.extend(commit.parent_commit_ids.iter().copied());
                if commits_seen >= first && commits_seen < end {
                    selected.insert(oid.to_string(), commit);
                }
                commits_seen += 1;
                if commits_seen >= end {
                    break;
                }
            }
            ObjectType::Tag => match Tag::from_bytes(&payload, oid) {
                Ok(tag) => queue.push_back(tag.object_hash),
                Err(_) => incomplete = true,
            },
            _ => incomplete = true,
        }
    }
    let has_more = queue.iter().any(|oid| !visited.contains(oid));
    let mut trailer_candidates = Vec::new();
    let mut trailer_limit_reached = false;
    'commits: for (oid, commit) in &selected {
        let message = parse_commit_msg(&commit.message).0;
        let mut per_commit = 0;
        for trailer in crate::internal::log::trailer::parse_trailers(message) {
            if !trailer.key_matches("Libra-Agent-Session") || !valid_session_id(&trailer.value) {
                continue;
            }
            if per_commit >= MAX_TRAILERS_PER_COMMIT
                || trailer_candidates.len() >= MAX_CANDIDATES_PER_PAGE
            {
                trailer_limit_reached = true;
                break 'commits;
            }
            trailer_candidates.push((oid.clone(), trailer.value, commit.id));
            per_commit += 1;
        }
    }
    Ok(PreparedPage {
        head,
        reachable: selected,
        has_more,
        incomplete,
        trailer_candidates,
        trailer_limit_reached,
    })
}

async fn session_eligible_with_capture(
    capture: &dyn CaptureStore,
    session_id: &str,
    scope: &CaptureScope,
) -> Result<bool> {
    let Some(session) = capture
        .get_session(session_id)
        .await
        .context("recheck commit-link recovery session scope in capture store")?
    else {
        return Ok(false);
    };
    if !scope.matches_scope_fields(
        session.scope_state,
        session.repo_id.as_deref(),
        session.worktree_id.as_deref(),
        session.workspace_id.as_deref(),
        session.workspace_fence,
    ) || !matches!(
        session.state,
        CaptureSessionState::Active | CaptureSessionState::Condensed | CaptureSessionState::Stopped
    ) {
        return Ok(false);
    }
    let tombstones = capture
        .list_import_tombstones()
        .await
        .context("check import tombstones for commit recovery")?;
    if tombstones.iter().any(|t| t.erased_session_id == session_id) {
        return Ok(false);
    }
    let checkpoints = capture
        .list_checkpoints_by_session(session_id)
        .await
        .context("check committed checkpoints for commit recovery")?;
    Ok(checkpoints
        .iter()
        .any(|cp| cp.scope == CaptureCheckpointScope::Committed))
}

async fn session_owned_and_not_erased_with_capture(
    capture: &dyn CaptureStore,
    session_id: &str,
    scope: &CaptureScope,
) -> Result<bool> {
    let Some(session) = capture
        .get_session(session_id)
        .await
        .context("recheck completed agent commit journal session ownership in capture store")?
    else {
        return Ok(false);
    };
    if !scope.matches_scope_fields(
        session.scope_state,
        session.repo_id.as_deref(),
        session.worktree_id.as_deref(),
        session.workspace_id.as_deref(),
        session.workspace_fence,
    ) {
        return Ok(false);
    }
    let tombstones = capture
        .list_import_tombstones()
        .await
        .context("check import tombstones for journal reclaim")?;
    Ok(!tombstones.iter().any(|t| t.erased_session_id == session_id))
}

async fn link_exists_with_capture(
    capture: &dyn CaptureStore,
    session_id: &str,
    oid: ObjectHash,
) -> Result<bool> {
    let kind = capture_hash_kind(oid);
    let oid_str = oid.to_string();
    let commits = capture
        .list_session_commits(session_id)
        .await
        .context("read existing agent commit links from capture store")?;
    Ok(commits
        .iter()
        .any(|row| row.hash_kind == kind && row.commit_oid == oid_str))
}

async fn link_count_with_capture(capture: &dyn CaptureStore, oid: ObjectHash) -> Result<usize> {
    let kind = capture_hash_kind(oid);
    let oid_str = oid.to_string();
    let sessions = capture
        .list_sessions_by_started_desc()
        .await
        .context("list sessions to count commit links in capture store")?;
    let mut count = 0usize;
    for session in sessions {
        let commits = capture
            .list_session_commits(&session.session_id)
            .await
            .context("read session commits for commit-link count")?;
        if commits
            .iter()
            .any(|row| row.hash_kind == kind && row.commit_oid == oid_str)
        {
            count += 1;
        }
    }
    Ok(count)
}

/// Inspect (and optionally repair) a prepared commit page using backend-neutral
/// [`GitStore`], [`GitStateStore`], and [`CaptureStore`] handles (`RB-22`).
#[allow(clippy::too_many_arguments)]
pub async fn inspect_prepared_with_stores(
    git_store: &dyn GitStore,
    state_store: &dyn GitStateStore,
    capture_store: &dyn CaptureStore,
    storage_root: &Path,
    scope: &CaptureScope,
    page: u16,
    repair: bool,
    prepared: &PreparedPage,
) -> Result<CommitRecoveryReport> {
    if repair && current_head_with_git_store(git_store, scope).await?.0 != prepared.head {
        bail!("current HEAD changed during commit-link inspection; retry doctor --repair");
    }
    let mut report = CommitRecoveryReport::new(page);
    let (records, bad_tail) = match commit_journal::read(storage_root) {
        Ok(result) => result,
        Err(_) => {
            report.journal_unavailable = true;
            report.journal_unavailable_reason = commit_journal::read_issue(storage_root);
            (Vec::new(), false)
        }
    };
    report.journal_records = records.len();
    report.journal_bad_tail = bad_tail;
    let reachable = &prepared.reachable;
    let has_more = prepared.has_more;
    let incomplete = prepared.incomplete;
    report.reachable_commits_scanned = reachable.len();
    report.history_incomplete =
        incomplete || prepared.trailer_limit_reached || (has_more && page == MAX_PAGE);
    report.next_commit_page = (has_more && page < MAX_PAGE).then_some(page + 1);
    let mut candidates: BTreeMap<(String, String), ObjectHash> = BTreeMap::new();
    for record in &records {
        if &record.scope != scope {
            report.foreign_scope_intents += 1;
            continue;
        }
        let Some(commit) = reachable.get(&record.commit_oid) else {
            report.unmatched_scope_intents += 1;
            continue;
        };
        if record.hash_kind != oid_kind(commit.id) {
            report.unmatched_scope_intents += 1;
            continue;
        }
        for session_id in &record.session_ids {
            if candidates.len() >= MAX_CANDIDATES_PER_PAGE {
                report.more_missing_links = true;
                report.history_incomplete = true;
                break;
            }
            candidates.insert((record.commit_oid.clone(), session_id.clone()), commit.id);
        }
    }
    for (oid, session_id, commit_id) in &prepared.trailer_candidates {
        if candidates.len() >= MAX_CANDIDATES_PER_PAGE {
            report.more_missing_links = true;
            report.history_incomplete = true;
            break;
        }
        report.trailer_candidates += 1;
        candidates.insert((oid.clone(), session_id.clone()), *commit_id);
    }
    report.candidate_links = candidates.len();
    report.evidence_state = if report.journal_unavailable {
        "unavailable"
    } else if report.journal_records > 0 || report.trailer_candidates > 0 {
        "available"
    } else {
        "no_evidence"
    };
    if repair {
        let now_ms = chrono::Utc::now().timestamp_millis();
        if scope
            .assert_workspace_fence_live_with_state(state_store, now_ms)
            .await
            .is_err()
        {
            report.permission_blocked = true;
            return Ok(report);
        }
    }
    report.repair_applied = repair;
    for ((_, session_id), oid) in candidates {
        if link_exists_with_capture(capture_store, &session_id, oid).await? {
            continue;
        }
        if !session_eligible_with_capture(capture_store, &session_id, scope).await? {
            report.rejected_links += 1;
            continue;
        }
        report.missing_links += 1;
        if !repair {
            continue;
        }
        if report.repaired_links >= MAX_LINKS_PER_REPAIR {
            report.more_missing_links = true;
            break;
        }
        if link_count_with_capture(capture_store, oid).await? >= 64 {
            report.rejected_links += 1;
            continue;
        }
        capture_store
            .put_session_commit(&AgentSessionCommitRecord {
                session_id,
                hash_kind: capture_hash_kind(oid),
                commit_oid: oid.to_string(),
            })
            .await
            .context("repair authorized agent commit link in capture store")?;
        report.repaired_links += 1;
    }
    if repair && bad_tail && !report.journal_unavailable {
        commit_journal::repair_torn_tail(storage_root)
            .context("repair torn agent commit journal tail")?;
    }
    Ok(report)
}

/// Reclaim journal records whose links are durably committed in [`CaptureStore`] (`RB-22`).
pub async fn reclaim_reconciled_with_stores(
    git_store: &dyn GitStore,
    state_store: &dyn GitStateStore,
    capture_store: &dyn CaptureStore,
    storage_root: &Path,
    scope: &CaptureScope,
    prepared: &PreparedPage,
) -> Result<usize> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    scope
        .assert_workspace_fence_live_with_state(state_store, now_ms)
        .await?;
    if current_head_with_git_store(git_store, scope).await?.0 != prepared.head {
        bail!("current HEAD changed before agent commit journal cleanup; retry doctor --repair");
    }
    let (records, bad_tail) = commit_journal::read(storage_root)?;
    if bad_tail {
        bail!("repair the torn agent commit journal tail before reclaiming recovered links");
    }
    let reachable = &prepared.reachable;
    let mut reconciled = Vec::new();
    for record in records {
        if &record.scope != scope {
            continue;
        }
        let Some(commit) = reachable.get(&record.commit_oid) else {
            continue;
        };
        if record.hash_kind != oid_kind(commit.id) {
            continue;
        }
        let mut complete = true;
        for session_id in &record.session_ids {
            if !session_owned_and_not_erased_with_capture(capture_store, session_id, scope).await?
                || !link_exists_with_capture(capture_store, session_id, commit.id).await?
            {
                complete = false;
                break;
            }
        }
        if complete {
            reconciled.push(record);
        }
    }
    commit_journal::compact_reconciled(storage_root, &reconciled)
        .context("reclaim verified agent commit journal recovery records")
}
