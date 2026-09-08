//! A released worker from an expired walk cannot poison its live successor.

use std::{
    fs,
    path::Path,
    sync::mpsc::{self, SyncSender},
    time::{Duration, Instant},
};

use super::Fixture;
use crate::{
    command::status_probe::with_io_deadline_bounded,
    internal::layer::ExclusionSnapshot,
    utils::{
        ignore::{BoundedIgnoreWalk, IgnorePolicy},
        util,
    },
};

struct ReleaseWorker(Option<SyncSender<()>>);

impl ReleaseWorker {
    fn release(&mut self) {
        self.0
            .take()
            .expect("worker release sender")
            .send(())
            .expect("blocked worker still owns receiver");
    }
}

impl Drop for ReleaseWorker {
    fn drop(&mut self) {
        // A failed assertion closes the channel too, so the worker's recv
        // returns Disconnected instead of retaining a pool slot indefinitely.
        drop(self.0.take());
    }
}

#[test]
fn expired_ignore_worker_preserves_successor_failure_latch_and_matcher() {
    super::run_case("late_epoch_isolation");
}

pub(super) fn run() {
    // Given: a real pooled worker waits behind an explicit release gate.
    let fixture = Fixture::open();
    let root = fixture.snapshotter.scope.worktree_root.clone();
    let source = root.join(".gitignore");
    let target = root.join("secret.txt");
    fs::write(&source, b"secret.txt\n").expect("valid ignore source");
    fs::write(&target, b"not a parent snapshot payload\n").expect("target file");
    util::prewarm_ignore_config(&root);
    let walk1 = BoundedIgnoreWalk::new(&root, ExclusionSnapshot::default());
    let epoch1 = util::secure_ignore_walk_epoch();
    assert_ne!(epoch1, 0, "bounded walk must own a secure epoch");
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (finished_tx, finished_rx) = mpsc::sync_channel(1);
    let mut release = ReleaseWorker(Some(release_tx));
    let old_root = root.clone();
    let old_target = target.clone();
    let ready = std::env::var_os(super::READY_ENV).expect("supervised child ready path");
    fs::write(ready, b"ready").expect("publish fixture readiness");

    // When: walk1 times out and ends before its worker performs the real read.
    let timed_out = with_io_deadline_bounded(Duration::from_millis(150), move || {
        let _ = started_tx.send(());
        if release_rx.recv().is_err() {
            return;
        }
        let ignored = util::check_gitignore_with_layers_as_dir_for_walk(
            &old_root,
            &old_target,
            &ExclusionSnapshot::default(),
            false,
            epoch1,
        );
        let _ = finished_tx.send(ignored);
    });
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("real pooled worker reached the release gate");
    assert!(timed_out.is_err(), "unreleased worker must time out");
    drop(walk1);

    let walk2 = BoundedIgnoreWalk::new(&root, ExclusionSnapshot::default());
    let epoch2 = util::secure_ignore_walk_epoch();
    assert_ne!(epoch2, 0);
    assert_ne!(epoch1, epoch2);
    let index = super::Index::new();
    assert_eq!(
        walk2.should_ignore(
            Path::new("secret.txt"),
            IgnorePolicy::Respect,
            &index,
            false,
            Instant::now() + Duration::from_secs(1),
        ),
        Some(true),
        "successor must first capture a valid deciding matcher",
    );
    fs::write(&source, [0xff, b'\n']).expect("late worker encounters invalid UTF-8");
    release.release();
    assert!(
        !finished_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("released worker must finish its real ignore lookup"),
        "invalid UTF-8 source has no usable deciding matcher",
    );

    // Then: the live walk retains its own healthy cached matcher and latch.
    assert!(
        !util::ignore_read_failed(),
        "expired worker poisoned live walk"
    );
    assert_eq!(
        walk2.should_ignore(
            Path::new("secret.txt"),
            IgnorePolicy::Respect,
            &index,
            false,
            Instant::now() + Duration::from_secs(1),
        ),
        Some(true),
        "late failure replaced the successor's captured matcher",
    );
    assert!(!util::ignore_read_failed());
    drop(walk2);
    assert_eq!(
        util::secure_ignore_walk_epoch(),
        0,
        "both walk guards ended"
    );
    fs::write(source, b"secret.txt\n").expect("restore healthy fixture bytes");
    assert_eq!(
        with_io_deadline_bounded(Duration::from_secs(1), || 7_u32),
        Ok(7),
        "released-worker scenario must leave the process able to run healthy jobs",
    );
}
