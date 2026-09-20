//! CEX-10 file-level undo contract tests.

use std::{collections::BTreeSet, fs};

use libra::internal::ai::session::file_history::{FileHistoryError, FileHistoryStore};
use tempfile::TempDir;

#[test]
fn file_history_undo_restores_multi_file_batch_atomically() {
    let workspace = TempDir::new().expect("workspace");
    let session_root = workspace.path().join(".libra").join("sessions").join("s1");
    let store = FileHistoryStore::new(session_root);

    let a = workspace.path().join("a.txt");
    let b = workspace.path().join("nested").join("b.txt");
    let added = workspace.path().join("added.txt");
    fs::create_dir_all(b.parent().expect("nested parent")).expect("nested dir");
    fs::write(&a, "a before\n").expect("write a");
    fs::write(&b, "b before\n").expect("write b");

    let paths = BTreeSet::from([a.clone(), b.clone(), added.clone()]);
    store
        .record_preimages("turn-1", workspace.path(), &paths)
        .expect("record preimages");

    fs::write(&a, "a after\n").expect("modify a");
    fs::write(&b, "b after\n").expect("modify b");
    fs::write(&added, "new file\n").expect("add file");

    let report = store
        .undo_latest_batch(workspace.path())
        .expect("undo latest batch");

    assert_eq!(report.batch_id, "turn-1");
    assert_eq!(fs::read_to_string(&a).expect("read a"), "a before\n");
    assert_eq!(fs::read_to_string(&b).expect("read b"), "b before\n");
    assert!(!added.exists());
}

#[test]
fn file_history_undo_preflights_failures_without_half_rollback() {
    let workspace = TempDir::new().expect("workspace");
    let session_root = workspace.path().join(".libra").join("sessions").join("s1");
    let store = FileHistoryStore::new(session_root);

    let a = workspace.path().join("a.txt");
    let blocked_child = workspace.path().join("blocked").join("child.txt");
    fs::create_dir_all(blocked_child.parent().expect("blocked parent")).expect("blocked dir");
    fs::write(&a, "a before\n").expect("write a");
    fs::write(&blocked_child, "child before\n").expect("write child");

    let paths = BTreeSet::from([a.clone(), blocked_child.clone()]);
    store
        .record_preimages("turn-1", workspace.path(), &paths)
        .expect("record preimages");

    fs::write(&a, "a after\n").expect("modify a");
    fs::remove_dir_all(workspace.path().join("blocked")).expect("remove blocked dir");
    fs::write(workspace.path().join("blocked"), "not a directory\n").expect("create blocker");

    let err = store
        .undo_latest_batch(workspace.path())
        .expect_err("undo should fail during preflight");

    assert!(matches!(err, FileHistoryError::Preflight { .. }));
    assert_eq!(fs::read_to_string(&a).expect("read a"), "a after\n");
    assert_eq!(
        fs::read_to_string(workspace.path().join("blocked")).expect("read blocker"),
        "not a directory\n"
    );
}

#[test]
fn file_history_keeps_at_most_50_versions_per_file() {
    let workspace = TempDir::new().expect("workspace");
    let session_root = workspace.path().join(".libra").join("sessions").join("s1");
    let store = FileHistoryStore::new(session_root);
    let path = workspace.path().join("a.txt");

    for batch in 0..51 {
        fs::write(&path, format!("version {batch}\n")).expect("write version");
        store
            .record_preimages(
                &format!("turn-{batch}"),
                workspace.path(),
                &BTreeSet::from([path.clone()]),
            )
            .expect("record preimage");
    }

    for _ in 0..50 {
        store
            .undo_latest_batch(workspace.path())
            .expect("retained batch should undo");
    }
    let err = store
        .undo_latest_batch(workspace.path())
        .expect_err("oldest version should be pruned");
    assert!(matches!(err, FileHistoryError::NoUndoBatch));
}
