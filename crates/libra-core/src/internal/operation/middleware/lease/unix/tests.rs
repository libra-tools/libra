//! Real filesystem failures preserve the bounded create-or-open protocol.

use std::{
    fs::{self, File},
    io,
    path::Path,
};

use super::{open_existing_leaf, open_leaf};

#[test]
fn removed_parent_preserves_create_error_without_recreating_directory() {
    // Given an owned directory fd whose empty directory has been removed.
    let fixture = tempfile::tempdir().expect("removed-parent fixture");
    let info = fixture.path().join("info");
    fs::create_dir(&info).expect("create owned info directory");
    let parent = File::open(&info).expect("hold owned info directory");
    let path = info.join("operation-v2.lock");
    fs::remove_dir(&info).expect("remove owned empty info directory");

    // When the actual creation path receives a non-EEXIST OS error.
    let error = open_leaf(&parent, &path, 0o666).expect_err("removed parent must reject creation");

    // Then the original cause and create context survive without recreating the parent.
    assert_not_found(&error.to_string(), &path, "create");
    assert_eq!(
        fs::symlink_metadata(&info)
            .expect_err("removed parent must stay absent")
            .kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn removed_existing_leaf_is_not_recreated_by_existing_open() {
    // Given a fixed lock leaf that disappears before the existing-open branch.
    let fixture = tempfile::tempdir().expect("removed-leaf fixture");
    let info = fixture.path().join("info");
    fs::create_dir(&info).expect("create owned info directory");
    let parent = File::open(&info).expect("hold owned info directory");
    let path = info.join("operation-v2.lock");
    File::create(&path).expect("precreate owned lock leaf");
    fs::remove_file(&path).expect("remove owned lock leaf");

    // When the actual existing-open helper runs, not a stub or alternate primitive.
    let error = open_existing_leaf(&parent, &path, c"operation-v2.lock")
        .expect_err("existing-open must not recreate a disappeared leaf");

    // Then ENOENT and the affected path survive, with no retry that recreates the leaf.
    assert_not_found(&error.to_string(), &path, "open");
    assert_eq!(
        fs::symlink_metadata(&path)
            .expect_err("removed leaf must stay absent")
            .kind(),
        io::ErrorKind::NotFound
    );
}

fn assert_not_found(message: &str, path: &Path, action: &str) {
    let cause = io::Error::from_raw_os_error(libc::ENOENT).to_string();
    assert!(message.contains(&cause), "{message}");
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(
        message.contains(&format!("cannot {action} operation scope lease")),
        "{message}"
    );
    assert!(!message.contains("already held"), "{message}");
}
