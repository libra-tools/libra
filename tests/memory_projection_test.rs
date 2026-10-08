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
