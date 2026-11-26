//! First-create race regression and controls; coordinated starts do not guarantee reproduction.

use std::{
    fs::{self, File},
    path::Path,
    sync::Barrier,
    thread,
};

use super::{OperationError, open_files};
use crate::utils::beneath::entry_identity_beneath;

type OpenResult = Result<(File, File), OperationError>;

#[test]
fn simultaneous_first_open_does_not_report_enoent() {
    for iteration in 0..128 {
        // Given a fresh directory with no info directory or existing lock file.
        let directory = tempfile::tempdir().expect("fresh first-open fixture");
        let info = directory.path().join("info");
        let path = info.join("operation-v2.lock");
        let start = Barrier::new(3);

        // When both threads open the real helper without deleting, renaming, or locking.
        let (first, second) = thread::scope(|workers| {
            let first = workers.spawn(|| {
                start.wait();
                open_files(&info, &path)
            });
            let second = workers.spawn(|| {
                start.wait();
                open_files(&info, &path)
            });
            start.wait();
            (first.join(), second.join())
        });
        let first =
            first.unwrap_or_else(|_| panic!("iteration {iteration}: first open worker panicked"));
        let second =
            second.unwrap_or_else(|_| panic!("iteration {iteration}: second open worker panicked"));

        // Then both results retain their file descriptors until both workers have joined.
        assert!(
            first.is_ok() && second.is_ok(),
            "iteration {iteration}: first={:?}; second={:?}",
            first.as_ref().err(),
            second.as_ref().err()
        );
        assert_same_file(&first, &second, iteration);
    }
}

#[test]
fn simultaneous_first_open_with_precreated_info() {
    run_control(true, |info, _| {
        fs::create_dir(info).expect("precreate info directory");
    });
}

#[test]
fn simultaneous_open_with_precreated_info_and_leaf() {
    run_control(true, |info, path| {
        fs::create_dir(info).expect("precreate info directory");
        File::create(path).expect("precreate lock file");
    });
}

#[test]
fn sequential_first_open_does_not_report_enoent() {
    run_control(false, |_, _| {});
}

fn run_control(concurrent: bool, prepare: impl Fn(&Path, &Path)) {
    for iteration in 0..128 {
        // Given an independent fixture with only the named setup difference.
        let directory = tempfile::tempdir().expect("fresh first-open control fixture");
        let info = directory.path().join("info");
        let path = info.join("operation-v2.lock");
        prepare(&info, &path);

        // When opening through the unchanged real helper, with no file locking.
        let (first, second) = if concurrent {
            let start = Barrier::new(3);
            let (first, second) = thread::scope(|workers| {
                let open = || {
                    start.wait();
                    open_files(&info, &path)
                };
                let first = workers.spawn(open);
                let second = workers.spawn(open);
                start.wait();
                (first.join(), second.join())
            });
            (
                first.unwrap_or_else(|_| panic!("iteration {iteration}: first worker panicked")),
                second.unwrap_or_else(|_| panic!("iteration {iteration}: second worker panicked")),
            )
        } else {
            (open_files(&info, &path), open_files(&info, &path))
        };

        // Then both handles stay owned until both joins and the identity check finish.
        assert_same_file(&first, &second, iteration);
    }
}

fn assert_same_file(first: &OpenResult, second: &OpenResult, iteration: usize) {
    assert!(
        first.is_ok() && second.is_ok(),
        "iteration {iteration}: first={:?}; second={:?}",
        first.as_ref().err(),
        second.as_ref().err()
    );
    let identity = |opened: &OpenResult| {
        let (file, _) = opened.as_ref().expect("open result checked above");
        entry_identity_beneath(file, Path::new(""))
            .expect("held file identity")
            .key
    };
    assert_eq!(
        identity(first),
        identity(second),
        "iteration {iteration}: different lock inodes"
    );
}
