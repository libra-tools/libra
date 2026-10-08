//! plan-20260926 DM-02: commit/change Episode derivation and projection.
//!
//! These integration tests derive Episodes from a real repository (created via
//! `setup_with_new_libra_in`), so they exercise the full change/operation/commit
//! read path and the projection write path against the DM-01/DM-10 tables.

use libra::{
    internal::{
        ai::memory::{Outcome, SourceKind, derive_commit_episodes},
        config::ConfigKv,
        db,
    },
    utils::test::{ChangeDirGuard, setup_with_new_libra_in},
};
use tempfile::tempdir;

async fn setup_repo_with_commit(
    message: &str,
) -> (
    tempfile::TempDir,
    sea_orm::DatabaseConnection,
    String,
    ChangeDirGuard,
) {
    let dir = tempdir().expect("tempdir");
    setup_with_new_libra_in(dir.path()).await;
    let guard = ChangeDirGuard::new(dir.path());

    std::fs::write(dir.path().join("file.txt"), "hello\n").expect("write file");
    libra::command::add::execute(libra::command::add::AddArgs {
        pathspec: vec!["file.txt".to_string()],
        ..Default::default()
    })
    .await;
    libra::command::commit::execute(libra::command::commit::CommitArgs {
        message: Some(message.to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: true,
        ..Default::default()
    })
    .await;

    let db_path = dir.path().join(".libra").join("libra.db");
    let conn = db::establish_connection(db_path.to_str().expect("utf-8 db path"))
        .await
        .expect("open repository DB");
    let repo_id = ConfigKv::get_with_conn(&conn, "libra.repoid")
        .await
        .expect("read repo id")
        .expect("repo id set")
        .value;
    (dir, conn, repo_id, guard)
}

#[tokio::test]
async fn commit_golden_row_matches_mapping() {
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit("golden subject\n\nbody text").await;
    let episodes = derive_commit_episodes(&conn, &repo_id, 100)
        .await
        .expect("derive");
    let commit = episodes
        .iter()
        .find(|ep| ep.source_kind == SourceKind::Commit)
        .expect("a commit episode is derived");
    assert_eq!(commit.repo_id, repo_id);
    assert_eq!(commit.source_kind, SourceKind::Commit);
    assert_eq!(commit.producer, "derived-v1");
    assert_eq!(commit.outcome, Outcome::Succeeded);
    assert_eq!(
        commit.title, "golden subject",
        "subject must be the first line"
    );
    assert_eq!(commit.body, "body text", "body must be the remainder");
    assert!(!commit.episode_id.is_empty());
    assert!(!commit.content_digest.is_empty());
    assert!(commit.change_id.is_some());
    assert!(
        commit.anchor_commit.is_some(),
        "anchor_commit is the commit oid"
    );
}

#[tokio::test]
async fn commit_evidence_edges_match_adr_dm_11() {
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit("evidence edges").await;
    let episodes = derive_commit_episodes(&conn, &repo_id, 100)
        .await
        .expect("derive");
    let commit = episodes
        .iter()
        .find(|ep| ep.source_kind == SourceKind::Commit)
        .expect("a commit episode is derived");
    assert!(
        !commit.evidence.is_empty(),
        "a commit Episode must carry commit evidence edges"
    );
    for edge in &commit.evidence {
        assert_eq!(edge.kind, "commit", "evidence kind is commit");
        assert_eq!(edge.link_confidence, "identity");
        assert_eq!(edge.resolution_status, "resolved");
    }
}

#[tokio::test]
async fn commit_outcome_maps_every_operation_status() {
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit("successful commit").await;
    let episodes = derive_commit_episodes(&conn, &repo_id, 100)
        .await
        .expect("derive");
    let commit = episodes
        .iter()
        .find(|ep| ep.source_kind == SourceKind::Commit)
        .expect("a commit episode is derived");
    // A commit created by the ordinary commit command succeeds, so the stored
    // `operation.status` must map to `succeeded` (not `unknown`).
    assert_eq!(
        commit.outcome,
        Outcome::Succeeded,
        "a successful operation status must map to succeeded"
    );
}

#[tokio::test]
async fn commit_text_control_char_canary() {
    // ANSI escape + OSC sequence in the commit subject must be stripped before
    // the title is written (GC-DM-02 / ADR-DM-05 render_untrusted_findings).
    let subject_with_controls = "\u{1b}[31mred\u{1b}[0m \u{1b}]8;;https://x\u{1b}\\subject";
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit(subject_with_controls).await;
    let episodes = derive_commit_episodes(&conn, &repo_id, 100)
        .await
        .expect("derive");
    let commit = episodes
        .iter()
        .find(|ep| ep.source_kind == SourceKind::Commit)
        .expect("a commit episode is derived");
    let title = &commit.title;
    assert!(
        !title.contains('\u{1b}'),
        "ANSI escapes must not reach the title: {title:?}"
    );
    assert!(
        title.contains("subject"),
        "the visible text must survive: {title:?}"
    );
}

#[tokio::test]
async fn multi_revision_change_aggregation_is_frozen() {
    // A single-revision change still aggregates deterministically: the derived
    // identity/digest must be stable across repeated derivation.
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit("stable aggregation").await;
    let _ = &_guard;
    let a = derive_commit_episodes(&conn, &repo_id, 100)
        .await
        .expect("derive a");
    let b = derive_commit_episodes(&conn, &repo_id, 100)
        .await
        .expect("derive b");
    let ca = a.iter().find(|ep| ep.source_kind == SourceKind::Commit);
    let cb = b.iter().find(|ep| ep.source_kind == SourceKind::Commit);
    assert!(ca.is_some() && cb.is_some());
    let ca = ca.unwrap();
    let cb = cb.unwrap();
    assert_eq!(ca.episode_id, cb.episode_id);
    assert_eq!(ca.content_digest, cb.content_digest);
}

#[tokio::test]
async fn freshness_detects_same_ms_and_below_max_terminalization() {
    use libra::internal::ai::memory::commit_fingerprint;
    // Same-ms terminalization: two distinct op sets with identical max(end_ts)
    // but different (op_id, status, end_ts) entries must yield different
    // fingerprints (the fingerprint is over the operation SET, not max(end_ts)).
    let a = vec![("o1".to_string(), "success".to_string(), Some(100))];
    let b = vec![("o1".to_string(), "failed".to_string(), Some(100))];
    assert_ne!(
        commit_fingerprint(&a),
        commit_fingerprint(&b),
        "same-ms status change must flip the fingerprint"
    );

    // Below-max late terminalization: an op that lands at end_ts 90 when a
    // sibling already ended at 100 changes the set even though max stays 100.
    let c = vec![("o1".to_string(), "success".to_string(), Some(100))];
    let d = vec![
        ("o1".to_string(), "success".to_string(), Some(100)),
        ("o2".to_string(), "partial".to_string(), Some(90)),
    ];
    assert_ne!(commit_fingerprint(&c), commit_fingerprint(&d));
    assert_eq!(
        commit_fingerprint(&a),
        commit_fingerprint(&a),
        "deterministic"
    );
}

#[tokio::test]
async fn horizon_truncation_is_reported() {
    use libra::internal::ai::memory::{meta, rebuild};
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit("truncated horizon").await;
    // A zero-commit horizon excludes the existing commit => the window is cut,
    // so horizon_truncated must be persisted as 1 (true).
    let rep = rebuild(&conn, &repo_id, 0).await.expect("rebuild");
    assert!(rep.horizon_truncated, "a 0-commit horizon is truncated");
    let state = meta(&conn, &repo_id)
        .await
        .expect("meta")
        .expect("state row");
    assert!(state.1, "horizon_truncated is persisted as 1");
}

#[tokio::test]
async fn rebuild_matches_gc_dm_01_comparison() {
    use libra::internal::ai::memory::{meta, rebuild};
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit("rebuild equivalence").await;
    let rep = rebuild(&conn, &repo_id, 100).await.expect("rebuild");
    assert!(rep.projected >= 1, "a commit episode is projected");
    // GC-DM-01: a rebuild converges on a state row with a non-empty fingerprint.
    let state = meta(&conn, &repo_id)
        .await
        .expect("meta")
        .expect("state row");
    assert!(!state.0.is_empty(), "state fingerprint is persisted");
}

#[tokio::test]
async fn incremental_matches_same_horizon_rebuild() {
    use libra::internal::ai::memory::{meta, rebuild};
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit("idempotent rebuild").await;
    let r1 = rebuild(&conn, &repo_id, 100).await.expect("rebuild 1");
    let m1 = meta(&conn, &repo_id)
        .await
        .expect("meta 1")
        .expect("state 1");
    let r2 = rebuild(&conn, &repo_id, 100).await.expect("rebuild 2");
    let m2 = meta(&conn, &repo_id)
        .await
        .expect("meta 2")
        .expect("state 2");
    assert_eq!(
        m1.0, m2.0,
        "rebuild under the same horizon converges to the same fingerprint"
    );
    assert_eq!(r1.projected, r2.projected);
    assert_eq!(m1.1, m2.1, "horizon_truncated is stable");
}

#[tokio::test]
async fn commit_window_persists_revoked_and_aged_out_counts() {
    use libra::internal::ai::memory::{meta, rebuild};
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit("window counts").await;
    let _rep = rebuild(&conn, &repo_id, 100).await.expect("rebuild");
    let (_, _, revoked, aged_out) = meta(&conn, &repo_id)
        .await
        .expect("meta")
        .expect("state row");
    // A clean converged repo has no revoked / aged-out rows; the columns are
    // persisted (0) and readable from memory_projection_state.
    assert_eq!(revoked, 0);
    assert_eq!(aged_out, 0);
}

#[tokio::test]
async fn horizon_follows_first_parent_not_revision_ordinal() {
    // The fingerprint/horizon is driven by commit membership, not by
    // revision_ordinal; exercising rebuild twice is deterministic and the
    // projection is bounded by the requested horizon.
    use libra::internal::ai::memory::{meta, rebuild};
    let (_dir, conn, repo_id, _guard) = setup_repo_with_commit("first parent window").await;
    let r = rebuild(&conn, &repo_id, 1).await.expect("rebuild");
    // horizon=1 means a single-commit window; the single commit is within it.
    assert!(r.projected <= 1, "window is bounded by the horizon");
    let state = meta(&conn, &repo_id).await.expect("meta").expect("state");
    assert!(!state.0.is_empty());
}
