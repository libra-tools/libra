//! Regression scenarios exercised inside a killable libtest child.

use std::time::{Duration, Instant};

use git_internal::internal::index::IndexEntry;

use super::{Completeness, Fixture, Index, PathBuf, fs, listing};
use crate::{
    internal::{config::ConfigKv, db::open_database_without_migrations},
    utils::util,
};

#[path = "cache.rs"]
mod cache;
#[cfg(unix)]
#[path = "cache_lock.rs"]
mod cache_lock;

const SAFE: &[u8] = b"independent candidate accepted before nested ignore failure\n";
const SECRET: &[u8] = b"content never persisted under unknown ignore policy\n";
const VISIBLE: &[u8] = b"ordinary nested file\n";

pub(super) async fn run(name: &str) {
    match name {
        "ordinary_ignore_rules_preserve_tracked_paths" => regular().await,
        "invalid_utf8_per_directory_ignore_stays_partial_until_repaired" => {
            cache::invalid_utf8(false).await
        }
        "invalid_utf8_core_excludes_file_stays_partial_until_repaired" => {
            cache::invalid_utf8(true).await
        }
        "listing_consumes_original_deadline_before_ignore_lookup" => shared_deadline(),
        "late_epoch_isolation" => super::epoch::run(),
        #[cfg(unix)]
        "raw_cache_lock_isolation" => cache_lock::run().await,
        "policy_layer_priority" | "policy_tracked_non_utf8" => super::policy::run(name).await,
        #[cfg(unix)]
        "policy_dirent_nofollow" => super::policy::run(name).await,
        #[cfg(unix)]
        "fifo_per_directory" => fifo(false).await,
        #[cfg(unix)]
        "fifo_configured" => fifo(true).await,
        _ => panic!("unknown supervised test case: {name}"),
    }
}

fn ready() {
    let path = std::env::var_os(super::READY_ENV).expect("child ready path");
    fs::write(path, b"ready").expect("publish fixture readiness");
}

fn write_payloads(fixture: &Fixture) {
    let root = &fixture.snapshotter.scope.worktree_root;
    fs::create_dir_all(root.join("blocked")).expect("nested directory");
    fs::write(root.join("safe.txt"), SAFE).expect("independent payload");
    fs::write(root.join("blocked/secret.txt"), SECRET).expect("secret payload");
    fs::write(root.join("blocked/visible.txt"), VISIBLE).expect("visible payload");
}

async fn source_path(fixture: &Fixture, configured: bool) -> PathBuf {
    if !configured {
        return fixture
            .snapshotter
            .scope
            .worktree_root
            .join("blocked/.gitignore");
    }
    let path = fixture.snapshotter.scope.gitdir.join("core-ignore");
    let connection =
        open_database_without_migrations(&fixture.snapshotter.scope.storage.join("libra.db"))
            .await
            .expect("open existing fixture database");
    ConfigKv::set_with_conn(
        &connection,
        "core.excludesFile",
        path.to_str().expect("source path"),
        false,
    )
    .await
    .expect("set fixture-local excludesFile");
    connection.close().await.expect("close config connection");
    // All callers have the isolated worktree as CWD. This does not change the
    // production config/layer scope contract or mutate parent process state.
    util::prewarm_ignore_config(&fixture.snapshotter.scope.worktree_root);
    assert_eq!(
        util::optional_cascaded_config_path(
            "core.excludesFile",
            &fixture.snapshotter.scope.worktree_root,
        ),
        Some(path.clone()),
        "the raw pathname cache route must really be exercised",
    );
    path
}

async fn regular() {
    // Given: an existing tracked path matches a valid nested ignore rule.
    let mut fixture = Fixture::open();
    write_payloads(&fixture);
    fs::write(
        source_path(&fixture, false).await,
        b"secret.txt\nvisible.txt\n",
    )
    .expect("valid ignore rules");
    let mut index = Index::new();
    index.add(IndexEntry::new_from_blob(
        "blocked/secret.txt".into(),
        listing::blob_oid(SECRET),
        0,
    ));
    index
        .save(fixture.snapshotter.scope.gitdir.join("index"))
        .expect("tracked index");
    ready();

    // When: the ordinary scanner applies tracked-aware ignore policy.
    let scan = fixture.snapshotter.scan_working_copy().await.expect("scan");

    // Then: tracked content is visible, untracked ignored content is absent.
    assert_eq!(scan.completeness, Completeness::Full);
    assert_eq!(
        scan.tracked,
        std::collections::BTreeMap::from([(
            "blocked/secret.txt".into(),
            listing::blob_oid(SECRET)
        ),])
    );
    assert_eq!(
        scan.untracked,
        std::collections::BTreeMap::from([("safe.txt".into(), listing::blob_oid(SAFE)),])
    );
    let outcome = fixture
        .snapshotter
        .capture()
        .await
        .expect("ordinary capture");
    assert_eq!(outcome.snapshot.completeness, Completeness::Full);
}

fn shared_deadline() {
    // Given: the real deadline is short even though snapshotter defaults to 30s.
    let fixture = Fixture::open();
    write_payloads(&fixture);
    ready();
    let deadline = Instant::now() + Duration::from_millis(150);
    listing::consume_deadline_in_next_listing(deadline);

    // When: deterministic enumeration returns only after that exact deadline.
    let (files, complete) = fixture
        .snapshotter
        .list_visible_files(&Index::new(), deadline)
        .expect("expired listing is a partial result");

    // Then: post-listing ignore work cannot renew the capture budget.
    assert!(
        Instant::now() >= deadline,
        "fixture did not consume the deadline"
    );
    assert!(!complete, "expired listing was declared complete");
    assert!(
        files.is_empty(),
        "expired enumeration authorized candidates: {files:?}"
    );
}

#[cfg(unix)]
async fn fifo(configured: bool) {
    use std::os::unix::fs::FileTypeExt;
    // Given: each beneath-ignore/raw excludesFile FIFO has its own child.
    let fixture = Fixture::open();
    write_payloads(&fixture);
    let source = source_path(&fixture, configured).await;
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&source)
            .status()
            .expect("mkfifo available on Unix test host")
            .success()
    );
    ready();

    // When: the caller supplies its remaining capture budget, not the 30s default.
    let started = Instant::now();
    let deadline = started + Duration::from_millis(150);
    let (files, complete) = fixture
        .snapshotter
        .list_visible_files(&Index::new(), deadline)
        .expect("FIFO must produce a partial listing, not a fatal worker error");

    // Then: fail closed promptly; child termination also contains any stuck
    // process-static test-host worker without claiming production pool recovery.
    assert!(!complete);
    assert!(
        files.is_empty(),
        "unknown ignore state retained candidates: {files:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "fresh default budget was used"
    );
    assert!(
        fs::symlink_metadata(source)
            .expect("FIFO retained")
            .file_type()
            .is_fifo()
    );
    assert_eq!(
        fs::read(fixture.snapshotter.scope.worktree_root.join("safe.txt")).expect("safe"),
        SAFE
    );
}
