//! A blocked configured ignore read must not monopolize the matcher cache.

use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use super::{Fixture, fs, ready, source_path};
use crate::{
    command::status_probe::with_io_deadline_bounded, internal::layer::ExclusionSnapshot,
    utils::util,
};

#[test]
fn blocked_raw_ignore_source_does_not_hold_global_matcher_cache() {
    super::super::run_case("raw_cache_lock_isolation");
}

fn writer_after_reader_open(source: &Path) -> File {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(source)
        {
            Ok(writer) => return writer,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                assert!(Instant::now() < deadline, "reader never opened FIFO");
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("nonblocking FIFO writer handshake: {error}"),
        }
    }
}

pub(super) async fn run() {
    // Given: a configured raw FIFO and an independent ordinary deciding source.
    let fixture = Fixture::open();
    let root = fixture.snapshotter.scope.worktree_root.clone();
    let source = source_path(&fixture, true).await;
    fs::write(root.join(".gitignore"), b"ordinary.txt\n").expect("ordinary source");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&source)
            .status()
            .expect("mkfifo available on Unix test host")
            .success()
    );
    ready();
    let (reader_done_tx, reader_done_rx) = mpsc::sync_channel(1);
    let blocked_root = root.clone();
    let reader = thread::spawn(move || {
        let ignored = util::check_gitignore_with_layers_as_dir_for_walk(
            &blocked_root,
            &blocked_root.join("requires-configured-source.txt"),
            &ExclusionSnapshot::default(),
            false,
            0,
        );
        let _ = reader_done_tx.send(ignored);
    });

    // When: opening a writer proves the raw reader has reached the FIFO open.
    // Keep this writer open without data: the reader cannot finish until drop.
    // File RAII also releases it if an assertion fails in the supervised child.
    let writer = writer_after_reader_open(&source);
    assert!(
        matches!(reader_done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "raw reader completed before the writer released EOF",
    );
    let ordinary_root = root.clone();
    let (ordinary_done_tx, ordinary_done_rx) = mpsc::sync_channel(1);
    let ordinary = with_io_deadline_bounded(Duration::from_millis(500), move || {
        let ignored = util::check_gitignore_with_layers_as_dir_for_walk(
            &ordinary_root,
            &ordinary_root.join("ordinary.txt"),
            &ExclusionSnapshot::default(),
            false,
            0,
        );
        let _ = ordinary_done_tx.send(ignored);
        ignored
    });

    // Release and join before the behavioral assertion, including on old code.
    drop(writer);
    assert_eq!(
        reader_done_rx.recv_timeout(Duration::from_secs(2)),
        Ok(false),
        "EOF releases the configured reader",
    );
    reader.join().expect("raw reader joined");
    assert_eq!(
        ordinary_done_rx.recv_timeout(Duration::from_secs(1)),
        Ok(true),
        "the ordinary worker is released even when its bounded caller timed out",
    );

    // Then: unrelated rules resolve while the FIFO reader is still blocked.
    assert_eq!(
        ordinary,
        Ok(true),
        "raw ignore I/O held the global matcher cache lock",
    );
    assert_eq!(
        fs::read(root.join(".gitignore")).expect("ordinary source unchanged"),
        b"ordinary.txt\n",
    );
}
