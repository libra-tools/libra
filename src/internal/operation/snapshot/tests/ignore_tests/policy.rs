//! Positive policy controls for the bounded snapshot ignore adapter.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use git_internal::internal::index::IndexEntry;

use super::{Fixture, Index, PathBuf, fs, listing};
use crate::{
    internal::{
        head::Head,
        layer::{self, ExclusionSnapshot, LayerStore},
        worktree_scope::WorktreeScope,
    },
    utils::ignore::{BoundedIgnoreWalk, IgnorePolicy},
};

pub(super) async fn run(name: &str) {
    match name {
        "policy_layer_priority" => layer_priority().await,
        "policy_tracked_non_utf8" => tracked_non_utf8(),
        #[cfg(unix)]
        "policy_dirent_nofollow" => dirent_nofollow(),
        _ => panic!("unknown supervised policy case: {name}"),
    }
}

fn ready() {
    fs::write(
        std::env::var_os(super::READY_ENV).expect("child ready path"),
        b"ready",
    )
    .expect("publish fixture readiness");
}

async fn layer_priority() {
    // Given: a real materialized layer, a tracked alias, and an explicit negation.
    let fixture = Fixture::open();
    let root = &fixture.snapshotter.scope.worktree_root;
    let source = tempfile::tempdir().expect("external layer source");
    fs::write(source.path().join("overlay.txt"), b"local overlay").expect("source");
    Head::update_result(Head::Branch("main".into()), None)
        .await
        .expect("seed unborn main HEAD for layer collision checks");
    LayerStore::add(
        &WorktreeScope::Main,
        "policy",
        source.path().to_str().expect("source path"),
        0,
        true,
    )
    .await
    .expect("register layer");
    layer::apply(&WorktreeScope::Main)
        .await
        .expect("materialize layer");
    layer::refresh_exclusion_snapshot_strict(&WorktreeScope::Main)
        .await
        .expect("refresh main layer ownership");
    let layers = ExclusionSnapshot::for_scope(&WorktreeScope::Main);
    assert!(layers.is_owned("overlay.txt"));
    let other = WorktreeScope::Linked("other-policy-scope".into());
    layer::refresh_exclusion_snapshot_strict(&other)
        .await
        .expect("empty other scope");
    let _pin = WorktreeScope::pin_scope_for_test(other, root.clone());
    assert!(ExclusionSnapshot::for_request().is_empty());
    let mut builder = ::ignore::gitignore::GitignoreBuilder::new(root);
    assert!(
        builder.add_line(None, "[z-a]").is_err(),
        "fixture must reject the pattern"
    );
    fs::write(
        root.join(".libraignore"),
        b"[z-a]\noverlay.txt\n!overlay.txt\nignored.txt\n",
    )
    .expect("warning-only pattern plus valid rules");
    let mut index = Index::new();
    index.add(IndexEntry::new_from_blob(
        "overlay.txt".into(),
        listing::blob_oid(b"local overlay"),
        0,
    ));
    let walk = BoundedIgnoreWalk::new(root, layers);
    ready();

    // When: the caller-supplied snapshot is consulted under a different ambient scope.
    let deadline = Instant::now() + Duration::from_secs(2);
    let outcomes = [
        IgnorePolicy::Respect,
        IgnorePolicy::OnlyIgnored,
        IgnorePolicy::IncludeIgnored,
    ]
    .map(|policy| walk.should_ignore(Path::new("overlay.txt"), policy, &index, false, deadline));

    // Then: layer priority wins over tracked/negation without changing policy semantics.
    assert_eq!(outcomes, [Some(true), Some(false), Some(false)]);
    for (path, expected) in [
        (PathBuf::from("overlay.txt"), true),
        (PathBuf::from("ignored.txt"), true),
        (PathBuf::from("visible.txt"), false),
        // An absolute path bypasses the caller's relative-key layer shortcut;
        // the worker must also retain Main's snapshot under the Linked pin.
        (root.join("overlay.txt"), true),
    ] {
        assert_eq!(
            walk.should_ignore(&path, IgnorePolicy::Respect, &Index::new(), false, deadline),
            Some(expected),
            "{path:?}"
        );
    }
}

fn tracked_non_utf8() {
    // Given: tracked and unknown-encoding paths cannot require a failed ignore read.
    let fixture = Fixture::open();
    let root = &fixture.snapshotter.scope.worktree_root;
    fs::write(root.join(".gitignore"), [0xff, b'\n']).expect("unreadable UTF-8 policy");
    let mut index = Index::new();
    index.add(IndexEntry::new_from_blob(
        "tracked.txt".into(),
        listing::blob_oid(b"tracked"),
        0,
    ));
    let paths = [PathBuf::from("tracked.txt")];
    #[cfg(unix)]
    let paths = {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        [
            paths[0].clone(),
            PathBuf::from(OsString::from_vec(b"non-\xff.txt".to_vec())),
        ]
    };
    let walk = BoundedIgnoreWalk::new(root, ExclusionSnapshot::default());
    ready();

    // When: all existing policies are evaluated through the bounded adapter.
    let deadline = Instant::now() + Duration::from_secs(2);
    let outcomes: Vec<_> = paths
        .iter()
        .map(|path| {
            [
                IgnorePolicy::Respect,
                IgnorePolicy::OnlyIgnored,
                IgnorePolicy::IncludeIgnored,
            ]
            .map(|policy| walk.should_ignore(path, policy, &index, false, deadline))
        })
        .collect();

    // Then: no ignore read turns the tracked/non-UTF8 shortcut into unknown state.
    assert_eq!(
        outcomes,
        vec![[Some(false), Some(true), Some(false)]; paths.len()]
    );
}

#[cfg(unix)]
fn dirent_nofollow() {
    // Given: a symlink to a directory matches a directory-only pattern by name.
    let fixture = Fixture::open();
    let root = &fixture.snapshotter.scope.worktree_root;
    fs::create_dir(root.join("target")).expect("target directory");
    std::os::unix::fs::symlink("target", root.join("alias")).expect("directory symlink");
    fs::write(root.join(".gitignore"), b"target/\nalias/\n").expect("directory-only rules");
    let entries: Vec<_> = fs::read_dir(root)
        .expect("real dirents")
        .map(|entry| entry.expect("dirent"))
        .filter(|entry| entry.file_name() == "alias" || entry.file_name() == "target")
        .collect();
    assert_eq!(entries.len(), 2);
    let walk = BoundedIgnoreWalk::new(root, ExclusionSnapshot::default());
    ready();

    // When: the actual no-follow dirent flag is passed into the adapter.
    let deadline = Instant::now() + Duration::from_secs(2);
    let outcomes: std::collections::BTreeMap<_, _> = entries
        .into_iter()
        .map(|entry| {
            let kind = entry.file_type().expect("no-follow dirent type");
            let path = PathBuf::from(entry.file_name());
            let outcome = walk.should_ignore(
                &path,
                IgnorePolicy::Respect,
                &Index::new(),
                kind.is_dir(),
                deadline,
            );
            (path, outcome)
        })
        .collect();

    // Then: only the real directory is excluded; following the link would exclude both.
    assert_eq!(
        outcomes,
        std::collections::BTreeMap::from([
            (PathBuf::from("alias"), Some(false)),
            (PathBuf::from("target"), Some(true)),
        ])
    );
}

#[test]
fn layers_outrank_tracked_paths_and_negation_without_changing_pattern_warnings() {
    super::run_case("policy_layer_priority");
}

#[test]
fn bounded_ignore_preserves_tracked_and_non_utf8_policy_shortcuts() {
    super::run_case("policy_tracked_non_utf8");
}

#[cfg(unix)]
#[test]
fn bounded_ignore_honors_nofollow_dirent_kind() {
    super::run_case("policy_dirent_nofollow");
}
