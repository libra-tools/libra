// Included into `snapshot::tests` so nextest paths match
// `internal::operation::snapshot::tests::stat_short_circuit_*` (BRL-05 G1–G17).

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use git_internal::{
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::types::ObjectType,
    },
};
use sea_orm::{ActiveModelTrait, ActiveValue::Set};

use super::{ScanResult, WorkspaceSnapshotter};
use crate::{
    internal::{
        config::ConfigKv,
        db::create_database,
        model::reference,
        operation::{PinnedRequestScope, WorkspaceStatePointer},
        worktree_scope::WorktreeScope,
    },
    utils::{client_storage::ClientStorage, test::ChangeDirGuard, util},
};

struct ShortCircuitFixture {
    _directory: tempfile::TempDir,
    worktree: PathBuf,
    gitdir: PathBuf,
    storage: ClientStorage,
    scope: PinnedRequestScope,
}

impl ShortCircuitFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary repository");
        let worktree = directory.path().canonicalize().expect("canonical root");
        let storage_path = worktree.join(".libra");
        let gitdir = storage_path.clone();
        fs::create_dir_all(&gitdir).expect("gitdir");
        let connection = create_database(
            storage_path
                .join("libra.db")
                .to_str()
                .expect("database path"),
        )
        .await
        .expect("real repository schema");
        reference::ActiveModel {
            name: Set(Some("main".to_string())),
            kind: Set(reference::ConfigKind::Head),
            commit: Set(None),
            remote: Set(None),
            worktree_id: Set(None),
            ..Default::default()
        }
        .insert(&connection)
        .await
        .expect("unborn HEAD");
        connection.close().await.expect("close setup pool");
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        let storage = ClientStorage::init_local(storage_path.join("objects"));
        let scope = PinnedRequestScope {
            scope: WorktreeScope::Main,
            workdir: worktree.clone(),
            worktree_root: worktree.clone(),
            gitdir: gitdir.clone(),
            storage: storage_path,
        };
        Self {
            _directory: directory,
            worktree,
            gitdir,
            storage,
            scope,
        }
    }

    fn guard(&self) -> ChangeDirGuard {
        ChangeDirGuard::new(&self.worktree)
    }

    fn snapshotter(&self, short_circuit: bool) -> WorkspaceSnapshotter {
        WorkspaceSnapshotter::new(
            self.scope.clone(),
            WorkspaceStatePointer::new("stat-sc", blob_oid(b"seed"), 0),
        )
        .with_storage(self.storage.clone())
        .with_stat_short_circuit(short_circuit)
    }

    async fn set_autocrlf(&self, value: bool) {
        let db_path = self.worktree.join(".libra").join(util::DATABASE);
        let db = crate::internal::db::get_db_conn_instance_for_path(&db_path)
            .await
            .expect("open db");
        ConfigKv::set_with_conn(
            &db,
            "core.autocrlf",
            if value { "true" } else { "false" },
            false,
        )
        .await
        .expect("set autocrlf");
    }
}

fn blob_oid(bytes: &[u8]) -> ObjectHash {
    ObjectHash::from_type_and_data(ObjectType::Blob, bytes)
}

fn put_blob(storage: &ClientStorage, bytes: &[u8]) -> ObjectHash {
    let oid = blob_oid(bytes);
    storage
        .put(&oid, bytes, ObjectType::Blob)
        .expect("store blob");
    oid
}

fn touch_mtime_reference(target: &Path, reference: &Path) {
    let status = Command::new("touch")
        .arg("-r")
        .arg(reference)
        .arg(target)
        .status()
        .expect("spawn touch");
    assert!(status.success(), "touch -r failed");
}

/// Write files, store blobs, then save one index whose mtime is strictly newer.
fn track_files(fixture: &ShortCircuitFixture, files: &[(&str, &[u8])]) {
    for (relative, bytes) in files {
        let path = fixture.worktree.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dirs");
        }
        fs::write(&path, bytes).expect("write file");
        put_blob(&fixture.storage, bytes);
    }
    std::thread::sleep(Duration::from_millis(15));
    let mut index = Index::from_file(fixture.gitdir.join("index")).unwrap_or_else(|_| Index::new());
    for (relative, bytes) in files {
        let oid = blob_oid(bytes);
        let entry = IndexEntry::new_from_file(Path::new(relative), oid, &fixture.worktree)
            .expect("index entry");
        index.add(entry);
    }
    std::thread::sleep(Duration::from_millis(15));
    index
        .save(fixture.gitdir.join("index"))
        .expect("save index");
}

fn track_unchanged_file(fixture: &ShortCircuitFixture, relative: &str, bytes: &[u8]) {
    track_files(fixture, &[(relative, bytes)]);
}

#[cfg_attr(windows, allow(dead_code))]
fn load_index(fixture: &ShortCircuitFixture) -> Index {
    Index::from_file(fixture.gitdir.join("index")).expect("load index")
}

fn save_index(fixture: &ShortCircuitFixture, index: &Index) {
    std::thread::sleep(Duration::from_millis(15));
    index
        .save(fixture.gitdir.join("index"))
        .expect("save index");
}

async fn capture_scan(snapshotter: &mut WorkspaceSnapshotter) -> ScanResult {
    snapshotter.reset_stat_short_circuit_counters();
    snapshotter.scan_working_copy().await.expect("scan")
}

async fn assert_manifest_equiv(setup: impl FnOnce(&ShortCircuitFixture)) {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    setup(&fixture);
    let mut enabled = fixture.snapshotter(true);
    let mut disabled = fixture.snapshotter(false);
    let scan_on = capture_scan(&mut enabled).await;
    let scan_off = capture_scan(&mut disabled).await;
    assert_eq!(scan_on.tracked, scan_off.tracked, "tracked oid map");
    assert_eq!(scan_on.untracked, scan_off.untracked, "untracked oid map");
    assert_eq!(
        scan_on.completeness, scan_off.completeness,
        "completeness"
    );
}

#[tokio::test]
async fn stat_short_circuit_manifest_tracked_unchanged_case() {
    assert_manifest_equiv(|fixture| {
        track_unchanged_file(fixture, "kept.txt", b"unchanged payload\n");
    })
    .await;
}

#[tokio::test]
async fn stat_short_circuit_manifest_tracked_modified_case() {
    assert_manifest_equiv(|fixture| {
        track_unchanged_file(fixture, "edited.txt", b"original same len!\n");
        std::thread::sleep(Duration::from_millis(15));
        fs::write(fixture.worktree.join("edited.txt"), b"replaced same len!\n")
            .expect("overwrite");
    })
    .await;
}

#[tokio::test]
async fn stat_short_circuit_manifest_untracked_case() {
    assert_manifest_equiv(|fixture| {
        track_unchanged_file(fixture, "tracked.txt", b"tracked\n");
        fs::write(fixture.worktree.join("loose.txt"), b"untracked\n").expect("untracked");
    })
    .await;
}

#[tokio::test]
async fn stat_short_circuit_manifest_deleted_case() {
    assert_manifest_equiv(|fixture| {
        track_unchanged_file(fixture, "gone.txt", b"will delete\n");
        fs::remove_file(fixture.worktree.join("gone.txt")).expect("delete");
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn stat_short_circuit_manifest_symlink_case() {
    assert_manifest_equiv(|fixture| {
        use std::os::unix::fs::symlink;
        fs::write(fixture.worktree.join("target.txt"), b"target\n").expect("target");
        symlink("target.txt", fixture.worktree.join("link.txt")).expect("symlink");
        let link_oid = put_blob(&fixture.storage, b"target.txt");
        std::thread::sleep(Duration::from_millis(15));
        let mut entry =
            IndexEntry::new_from_file(Path::new("link.txt"), link_oid, &fixture.worktree)
                .expect("symlink entry");
        entry.mode = 0o120000;
        entry.hash = link_oid;
        let mut index = Index::new();
        index.add(entry);
        save_index(fixture, &index);
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn stat_short_circuit_manifest_gitlink_case() {
    assert_manifest_equiv(|fixture| {
        fs::create_dir_all(fixture.worktree.join("vendor/sub")).expect("gitlink dir");
        fs::write(fixture.worktree.join("vendor/sub/inner.txt"), b"nested\n").expect("nested");
        track_unchanged_file(fixture, "vendor/tracked.txt", b"sibling\n");
        let gitlink_oid =
            ObjectHash::from_type_and_data(ObjectType::Commit, b"external opaque commit");
        let mut entry = IndexEntry::new_from_blob("vendor/sub".into(), gitlink_oid, 0);
        entry.mode = 0o160000;
        let mut index = load_index(fixture);
        index.add(entry);
        save_index(fixture, &index);
    })
    .await;
}

#[tokio::test]
async fn stat_short_circuit_skips_unchanged_scan_hash_submissions() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    let mut files: Vec<(String, Vec<u8>)> = (0..200)
        .map(|i| (format!("f{i:03}.txt"), format!("payload-{i}\n").into_bytes()))
        .collect();
    files.push(("modified.txt".into(), b"before-mod\n".to_vec()));
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    track_files(&fixture, &refs);
    std::thread::sleep(Duration::from_millis(15));
    fs::write(fixture.worktree.join("modified.txt"), b"after-mod!!\n").expect("modify");
    fs::write(fixture.worktree.join("untracked.txt"), b"loose\n").expect("untracked");

    let mut snapshotter = fixture.snapshotter(true);
    let scan = capture_scan(&mut snapshotter).await;
    assert_eq!(scan.tracked.len(), 201);
    assert_eq!(scan.untracked.len(), 1);
    assert_eq!(
        snapshotter.scan_hash_submission_count(),
        2,
        "only modified + untracked should submit worker hashes"
    );
    assert_eq!(scan.reused_index_oid.len(), 200);
}

#[tokio::test]
async fn stat_short_circuit_skips_unchanged_persist_reads() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    let mut files: Vec<(String, Vec<u8>)> = (0..200)
        .map(|i| (format!("p{i:03}.txt"), format!("persist-{i}\n").into_bytes()))
        .collect();
    files.push(("modified.txt".into(), b"before-mod\n".to_vec()));
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    track_files(&fixture, &refs);
    std::thread::sleep(Duration::from_millis(15));
    fs::write(fixture.worktree.join("modified.txt"), b"after-mod!!\n").expect("modify");
    fs::write(fixture.worktree.join("untracked.txt"), b"loose\n").expect("untracked");

    let mut snapshotter = fixture.snapshotter(true);
    snapshotter.reset_stat_short_circuit_counters();
    snapshotter.capture().await.expect("capture");
    assert_eq!(
        snapshotter.persist_read_count(),
        2,
        "persist should read only modified + untracked"
    );
    assert_eq!(
        snapshotter.persist_put_blob_count(),
        2,
        "persist should put_blob only modified + untracked"
    );
}

#[tokio::test]
async fn stat_short_circuit_rehashes_racy_files() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    let relative = "racy.txt";
    let bytes = b"racy payload\n";
    let path = fixture.worktree.join(relative);
    fs::write(&path, bytes).expect("write");
    let oid = put_blob(&fixture.storage, bytes);
    let entry =
        IndexEntry::new_from_file(Path::new(relative), oid, &fixture.worktree).expect("entry");
    let mut index = Index::new();
    index.add(entry);
    let index_path = fixture.gitdir.join("index");
    index.save(&index_path).expect("save index");
    // Refresh entry stats after aligning file mtime with the index file, then
    // restore the index-file mtime so file mtime is not strictly older (racy).
    // Keep the mtime reference under `.libra` so it is not scanned as untracked.
    touch_mtime_reference(&path, &index_path);
    let index_mtime_ref = fixture.gitdir.join("brl05-index-mtime-ref");
    fs::copy(&index_path, &index_mtime_ref).expect("copy index mtime ref");
    let entry =
        IndexEntry::new_from_file(Path::new(relative), oid, &fixture.worktree).expect("restat");
    let mut index = Index::new();
    index.add(entry);
    index.save(&index_path).expect("rewrite index");
    touch_mtime_reference(&index_path, &index_mtime_ref);
    touch_mtime_reference(&path, &index_path);

    let mut snapshotter = fixture.snapshotter(true);
    let scan = capture_scan(&mut snapshotter).await;
    assert!(
        !scan.reused_index_oid.contains(relative),
        "racy file must not short-circuit"
    );
    assert_eq!(snapshotter.scan_hash_submission_count(), 1);
}

#[tokio::test]
async fn stat_short_circuit_rehashes_same_size_stat_changed_replacement() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    track_unchanged_file(&fixture, "swap.txt", b"AAAAAAAA\n");
    std::thread::sleep(Duration::from_millis(15));
    fs::write(fixture.worktree.join("swap.txt"), b"BBBBBBBB\n").expect("same-size replace");
    let mut snapshotter = fixture.snapshotter(true);
    let scan = capture_scan(&mut snapshotter).await;
    assert!(!scan.reused_index_oid.contains("swap.txt"));
    assert_eq!(scan.tracked.get("swap.txt"), Some(&blob_oid(b"BBBBBBBB\n")));
}

#[tokio::test]
async fn stat_short_circuit_rehashes_timestamp_restored_content() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    track_unchanged_file(&fixture, "touch.txt", b"original!\n");
    let path = fixture.worktree.join("touch.txt");
    let mtime_ref = fixture.gitdir.join("brl05-mtime-ref");
    fs::copy(&path, &mtime_ref).expect("mtime reference copy");
    std::thread::sleep(Duration::from_millis(15));
    fs::write(&path, b"tampered!\n").expect("replace content");
    touch_mtime_reference(&path, &mtime_ref);
    let mut snapshotter = fixture.snapshotter(true);
    let scan = capture_scan(&mut snapshotter).await;
    assert!(!scan.reused_index_oid.contains("touch.txt"));
    assert_eq!(
        scan.tracked.get("touch.txt"),
        Some(&blob_oid(b"tampered!\n"))
    );
}

#[tokio::test]
async fn stat_short_circuit_rehashes_autocrlf_paths() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    fixture.set_autocrlf(true).await;
    track_unchanged_file(&fixture, "crlf.txt", b"line\n");
    let mut snapshotter = fixture.snapshotter(true);
    let scan = capture_scan(&mut snapshotter).await;
    assert!(!scan.reused_index_oid.contains("crlf.txt"));
    assert_eq!(snapshotter.scan_hash_submission_count(), 1);
}

#[tokio::test]
async fn stat_short_circuit_rehashes_text_eol_attributes() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    fs::write(
        fixture.worktree.join(".gitattributes"),
        "*.txt text eol=lf\n",
    )
    .expect("attributes");
    track_unchanged_file(&fixture, "attr.txt", b"textish\n");
    let mut snapshotter = fixture.snapshotter(true);
    let scan = capture_scan(&mut snapshotter).await;
    assert!(!scan.reused_index_oid.contains("attr.txt"));
}

#[tokio::test]
async fn stat_short_circuit_rehashes_filter_lfs_paths() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    fs::write(
        fixture.worktree.join(".gitattributes"),
        "*.bin filter=lfs\n",
    )
    .expect("attributes");
    track_unchanged_file(&fixture, "blob.bin", b"pointer-ish\n");
    let mut snapshotter = fixture.snapshotter(true);
    let scan = capture_scan(&mut snapshotter).await;
    assert!(!scan.reused_index_oid.contains("blob.bin"));
}

#[tokio::test]
async fn stat_short_circuit_rehashes_unproven_conversion_fail_closed() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    fs::write(fixture.worktree.join(".gitattributes"), "*.id ident\n").expect("attributes");
    track_unchanged_file(&fixture, "marked.id", b"$Id$\n");
    let mut snapshotter = fixture.snapshotter(true);
    let scan = capture_scan(&mut snapshotter).await;
    assert!(!scan.reused_index_oid.contains("marked.id"));
}

#[tokio::test]
async fn stat_short_circuit_missing_blob_falls_back_to_hash() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    let relative = "orphan.txt";
    let bytes = b"index-only oid\n";
    fs::write(fixture.worktree.join(relative), bytes).expect("write");
    let oid = blob_oid(bytes);
    std::thread::sleep(Duration::from_millis(15));
    let entry =
        IndexEntry::new_from_file(Path::new(relative), oid, &fixture.worktree).expect("entry");
    let mut index = Index::new();
    index.add(entry);
    save_index(&fixture, &index);
    let mut snapshotter = fixture.snapshotter(true);
    let scan = capture_scan(&mut snapshotter).await;
    assert!(!scan.reused_index_oid.contains(relative));
    assert_eq!(scan.tracked.get(relative), Some(&oid));
    assert_eq!(snapshotter.scan_hash_submission_count(), 1);
}

#[tokio::test]
async fn stat_short_circuit_completeness_and_budget_unchanged() {
    let fixture = ShortCircuitFixture::new().await;
    let _guard = fixture.guard();
    track_unchanged_file(&fixture, "a.txt", b"aaa\n");
    track_unchanged_file(&fixture, "b.txt", b"bbb\n");
    fs::write(fixture.worktree.join("u.txt"), b"uuu\n").expect("untracked");

    let mut enabled = fixture
        .snapshotter(true)
        .with_limits(Duration::from_secs(30), 100_000, 512 * 1024 * 1024);
    let mut disabled = fixture
        .snapshotter(false)
        .with_limits(Duration::from_secs(30), 100_000, 512 * 1024 * 1024);

    let on = capture_scan(&mut enabled).await;
    let off = capture_scan(&mut disabled).await;
    assert_eq!(on.completeness, off.completeness);
    assert_eq!(on.bytes, off.bytes);
    assert_eq!(on.tracked, off.tracked);
    assert_eq!(on.untracked, off.untracked);
}
