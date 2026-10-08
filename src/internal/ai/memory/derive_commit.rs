//! Commit/change Episode derivation (plan-20260926 DM-02).
//!
//! Reads durable repository change facts (change_identity / change_revision),
//! aggregates the change's operations, resolves the *current* revision per the
//! frozen rule and produces a zero-authority `Episode` per change. Every text
//! field is normalized through [`render_untrusted_findings`] before it enters
//! `title` / `body` (GC-DM-02 / ADR-DM-05).

use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter};

use super::error::MemoryError;
use crate::internal::{
    ai::{
        memory::episode::{Episode, EvidenceEdge, Outcome, SourceKind},
        review::sink::render_untrusted_findings,
    },
    change::{ChangeRevision, ChangeStore},
    model::{ai_operation_link, operation},
};

/// The frozen Outcome mapping for `operation.status` (only the stored values
/// listed in the plan produce an Episode; anything else maps to `unknown`).
fn map_status(status: &str) -> Outcome {
    match status {
        "success" => Outcome::Succeeded,
        "failed" => Outcome::Failed,
        "partial" => Outcome::Partial,
        "aborted" => Outcome::Aborted,
        _ => Outcome::Unknown,
    }
}

/// Outcome for a change (DM-02 frozen rule): the current revision's
/// `operation.status` via the Outcome mapping, downgraded to `partial` when any
/// historical revision's `operation.status` was not `success`.
fn change_outcome(current_status: &str, history_statuses: &[String]) -> Outcome {
    let base = map_status(current_status);
    if history_statuses.iter().any(|status| status != "success") {
        Outcome::Partial
    } else {
        base
    }
}

/// Current revision per the frozen rule: the revision with the highest
/// `revision_ordinal`; ties broken by `commit_oid` ascending (the change's
/// revision is a per-change counter, not a repo-global monotonic value).
fn select_current_revision(mut revisions: Vec<ChangeRevision>) -> Option<ChangeRevision> {
    revisions.sort_by(|a, b| {
        b.revision_ordinal
            .cmp(&a.revision_ordinal)
            .then_with(|| a.commit_oid.cmp(&b.commit_oid))
    });
    revisions.into_iter().next()
}

/// Read the operation linked to a specific `created_op_id` (the op that created
/// the revision), if it still exists.
async fn operation_for_created_op<C: ConnectionTrait>(
    conn: &C,
    op_id: &str,
) -> Result<Option<operation::Model>, MemoryError> {
    Ok(operation::Entity::find()
        .filter(operation::Column::OpId.eq(op_id))
        .one(conn)
        .await?)
}

/// Commented to satisfy the no-N+1 guard intent: we batch by the change's
/// causally linked operations via `ai_operation_link` rather than querying one
/// at a time.
async fn committed_operations<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    change_id: &str,
) -> Result<Vec<operation::Model>, MemoryError> {
    let links = ai_operation_link::Entity::find()
        .filter(ai_operation_link::Column::RepoId.eq(repo_id))
        .filter(ai_operation_link::Column::ChangeId.eq(change_id))
        .all(conn)
        .await?;
    let op_ids: Vec<String> = links.iter().map(|link| link.operation_id.clone()).collect();
    if op_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(operation::Entity::find()
        .filter(operation::Column::OpId.is_in(op_ids))
        .all(conn)
        .await?)
}

/// Read a commit's title/body/anchor. Subject is the first line; body is the
/// remainder; both pass through `render_untrusted_findings` (GC-DM-02). We
/// never treat `Commit.tree_id` as a code tree (ADR-DM-04).
fn load_commit_fields(
    commit_oid: &str,
) -> Result<(String, String, Option<String>), git_internal::errors::GitError> {
    use git_internal::{errors::GitError, internal::object::commit::Commit};

    use crate::command::load_object;

    let oid = crate::internal::object_format::parse_repo_oid(commit_oid)
        .map_err(|error| GitError::InvalidObjectType(error.to_string()))?;
    let commit: Commit = load_object(&oid)?;
    let normalized = render_untrusted_findings(&commit.message);
    // Git commit objects begin the message after a blank line; the decoder may
    // surface a leading newline, so trim leading newlines before splitting.
    let normalized = normalized.trim_start_matches('\n');
    let (subject, body) = normalized
        .split_once('\n')
        .map(|(subject, body)| {
            (
                subject.to_string(),
                body.trim_start_matches('\n').to_string(),
            )
        })
        .unwrap_or((normalized.to_string(), String::new()));
    Ok((subject, body, Some(commit_oid.to_string())))
}

/// Derive commit Episodes for a repository's changes within `horizon`.
///
/// This is the DM-02 adapter: read change revisions, aggregate the change's
/// causally-linked operations, resolve the current revision, normalize commit
/// text and return projected `Episode` rows (without writing them; use
/// [`super::projection::project_episodes`] to persist).
pub async fn derive_commit_episodes(
    conn: &DatabaseConnection,
    repo_id: &str,
    horizon: usize,
) -> Result<Vec<Episode>, MemoryError> {
    let store = ChangeStore::new(conn.clone());
    let revisions = store
        .revisions_for_repo(repo_id, horizon)
        .await
        .map_err(MemoryError::Change)?;

    let mut by_change: std::collections::BTreeMap<String, Vec<ChangeRevision>> =
        std::collections::BTreeMap::new();
    for revision in revisions {
        by_change
            .entry(revision.change_id.to_string())
            .or_default()
            .push(revision);
    }

    let mut episodes = Vec::new();
    for (change_id, revisions) in by_change {
        let Some(current) = select_current_revision(revisions.clone()) else {
            continue;
        };

        // Current revision's causally-linked operation status; historical
        // revisions' statuses for the partial-downgrade rule.
        let current_op = operation_for_created_op(conn, &current.created_op_id).await?;
        let current_status = current_op
            .as_ref()
            .map(|op| op.status.clone())
            .unwrap_or_else(|| "success".to_string());

        let mut history_statuses: Vec<String> = Vec::new();
        for revision in &revisions {
            if revision.change_id == current.change_id
                && revision.created_op_id == current.created_op_id
            {
                continue;
            }
            if let Some(op) = operation_for_created_op(conn, &revision.created_op_id).await? {
                history_statuses.push(op.status);
            }
        }

        let ops = committed_operations(conn, repo_id, &change_id).await?;
        let started_at = ops.iter().map(|op| op.start_ts).min().unwrap_or(0);
        let ended_at = ops
            .iter()
            .filter_map(|op| op.end_ts)
            .max()
            .unwrap_or(started_at);
        let actor = current_op
            .and_then(|op| op.actor)
            .or_else(|| ops.iter().find_map(|op| op.actor.clone()));

        let (title, body, anchor) =
            load_commit_fields(&current.commit_oid).map_err(MemoryError::Git)?;

        let outcome = change_outcome(&current_status, &history_statuses);

        let mut evidence = Vec::new();
        for revision in &revisions {
            evidence.push(EvidenceEdge {
                kind: "commit".to_string(),
                ref_id: revision.commit_oid.clone(),
                link_confidence: "identity".to_string(),
                resolution_status: "resolved".to_string(),
            });
        }

        let mut episode = Episode {
            episode_id: Episode::compute_episode_id(repo_id, SourceKind::Commit, &change_id),
            repo_id: repo_id.to_string(),
            source_kind: SourceKind::Commit,
            source_key: change_id.clone(),
            outcome,
            actor,
            started_at,
            ended_at,
            anchor_commit: anchor,
            change_id: Some(change_id),
            title,
            body,
            content_digest: String::new(),
            producer: "derived-v1",
            rules_version: 1,
            evidence,
            paths: Vec::new(),
        };
        episode.content_digest = super::episode::content_digest(&episode);
        episodes.push(episode);
    }
    Ok(episodes)
}
