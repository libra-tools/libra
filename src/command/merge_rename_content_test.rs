//! Boundary coverage for Git's recursive rename-content merge rules.

use std::path::Path;

use super::{
    Blob, MergeFavor, MergeTreeEntry, TreeItemMode, TreeMergeContext, VirtualBlobs,
    merge_rename_content,
};

fn entry(blobs: &mut VirtualBlobs, data: &[u8], mode: TreeItemMode) -> MergeTreeEntry {
    let blob = Blob::from_content_bytes(data.to_vec());
    let hash = blob.id;
    blobs.insert(hash, blob.data);
    MergeTreeEntry { hash, mode }
}

fn merge(
    blobs: &mut VirtualBlobs,
    base: Option<&MergeTreeEntry>,
    ours: &MergeTreeEntry,
    theirs: &MergeTreeEntry,
) -> (MergeTreeEntry, bool) {
    merge_rename_content(
        Path::new("renamed.txt"),
        base,
        ours,
        theirs,
        "temporary 1:a",
        "temporary 2:b",
        "base:old",
        diffy::ConflictStyle::Merge,
        &mut TreeMergeContext::nested(false, 1, None, blobs),
    )
    .expect("recursive content merge")
}

#[test]
fn binary_fold_uses_original_content_and_independently_merged_mode() {
    // Given: both binary versions diverged and one side made it executable.
    let mut blobs = VirtualBlobs::new();
    let base = entry(&mut blobs, b"base\0", TreeItemMode::Blob);
    let ours = entry(&mut blobs, b"ours\0", TreeItemMode::BlobExecutable);
    let theirs = entry(&mut blobs, b"theirs\0", TreeItemMode::Blob);
    // When: each fold order performs its nontrivial rename content merge.
    for (ours, theirs) in [(&ours, &theirs), (&theirs, &ours)] {
        let (result, clean) = merge(&mut blobs, Some(&base), ours, theirs);
        // Then: ll_binary_merge succeeds with orig, and mode merging survives.
        assert_eq!(result.hash, base.hash);
        assert_eq!(result.mode, TreeItemMode::BlobExecutable);
        assert!(clean);
    }
}

#[test]
fn binary_fold_without_a_regular_original_records_empty_content() {
    // Given: a missing or symlink original with genuinely divergent binaries.
    let mut blobs = VirtualBlobs::new();
    let old_link = entry(&mut blobs, b"original-target", TreeItemMode::Link);
    let ours = entry(&mut blobs, b"ours\0", TreeItemMode::Blob);
    let theirs = entry(&mut blobs, b"theirs\0", TreeItemMode::Blob);
    for base in [None, Some(&old_link)] {
        // When: Git's two-way content merge supplies an empty orig buffer.
        let (result, clean) = merge(&mut blobs, base, &ours, &theirs);
        // Then: an addressable empty blob remains at the regular-file path.
        assert_eq!(blobs.get(&result.hash), Some(&Vec::new()));
        assert_eq!(result.mode, TreeItemMode::Blob);
        assert!(clean);
    }
}

#[test]
fn binary_fold_does_not_hide_an_unresolved_mode_conflict() {
    // Given: without a regular original, both regular modes differ from it.
    let mut blobs = VirtualBlobs::new();
    let old_link = entry(&mut blobs, b"original-target", TreeItemMode::Link);
    let ours = entry(&mut blobs, b"ours\0", TreeItemMode::BlobExecutable);
    let theirs = entry(&mut blobs, b"theirs\0", TreeItemMode::Blob);
    for base in [None, Some(&old_link)] {
        // When: content falls back successfully but mode merging cannot agree.
        let (result, clean) = merge(&mut blobs, base, &ours, &theirs);
        // Then: Git retains ours' mode and reports the result unclean.
        assert_eq!(blobs.get(&result.hash), Some(&Vec::new()));
        assert_eq!(result.mode, ours.mode);
        assert!(!clean);
    }
}

#[test]
fn symlink_fold_restores_the_complete_original_and_remains_unclean() {
    // Given: nontrivial symlink versions, including a different original kind.
    // Real rename detection is exact-only for links, so this pins the helper
    // boundary rather than claiming an unreachable CLI shape is exercised.
    let mut blobs = VirtualBlobs::new();
    let base_link = entry(&mut blobs, b"original-target", TreeItemMode::Link);
    let base_file = entry(&mut blobs, b"original-file", TreeItemMode::BlobExecutable);
    let ours = entry(&mut blobs, b"ours-target", TreeItemMode::Link);
    let theirs = entry(&mut blobs, b"theirs-target", TreeItemMode::Link);
    for base in [&base_link, &base_file] {
        // When: merge-ort's recursive symlink rule selects the original.
        let (result, clean) = merge(&mut blobs, Some(base), &ours, &theirs);
        // Then: both the original kind and bytes survive, with clean=false.
        assert_eq!(result, *base);
        assert!(!clean);
    }
}

#[test]
fn non_file_rename_without_an_original_refuses_the_invalid_helper_contract() {
    // Given: callers normally supply an original; absence needs an optional
    // path result and belongs to virtual_conflict_resolution instead.
    let mut blobs = VirtualBlobs::new();
    let ours = entry(&mut blobs, b"ours-target", TreeItemMode::Link);
    let theirs = entry(&mut blobs, b"theirs-target", TreeItemMode::Link);
    // When: this non-optional rename helper is incorrectly given no original.
    let error = merge_rename_content(
        Path::new("renamed.txt"),
        None,
        &ours,
        &theirs,
        "a",
        "b",
        "base:old",
        diffy::ConflictStyle::Merge,
        &mut TreeMergeContext::nested(false, 1, None, &mut blobs),
    )
    .expect_err("do not invent either side as the virtual original");
    // Then: identify the affected original instead of silently choosing ours.
    assert!(error.to_string().contains("base:old"));
    assert!(error.to_string().contains("no original version"));
}

#[test]
fn binary_fold_resolves_trivial_oids_before_the_virtual_original_fallback() {
    // Given: one side changes content while the other changes only the mode.
    let mut blobs = VirtualBlobs::new();
    let base = entry(&mut blobs, b"base\0", TreeItemMode::Blob);
    let ours = MergeTreeEntry {
        hash: base.hash,
        mode: TreeItemMode::BlobExecutable,
    };
    let theirs = entry(&mut blobs, b"theirs\0", TreeItemMode::Blob);
    // When: rename content merging first evaluates the trivial OID rules.
    let (result, clean) = merge(&mut blobs, Some(&base), &ours, &theirs);
    // Then: the real one-sided edit survives instead of reverting to orig.
    assert_eq!(result.hash, theirs.hash);
    assert_eq!(result.mode, TreeItemMode::BlobExecutable);
    assert!(clean);
}

#[test]
fn outer_binary_content_selection_preserves_mode_and_mode_conflicts() {
    // Given: nontrivial binary versions with different modes, with or without
    // an original that can explain the executable-bit change.
    let mut blobs = VirtualBlobs::new();
    let base = entry(&mut blobs, b"base\0", TreeItemMode::Blob);
    let ours = entry(&mut blobs, b"ours\0", TreeItemMode::BlobExecutable);
    let theirs = entry(&mut blobs, b"theirs\0", TreeItemMode::Blob);
    for original in [Some(&base), None] {
        for favor in [None, Some(MergeFavor::Ours), Some(MergeFavor::Theirs)] {
            // When: the final merge chooses a whole binary side.
            let (result, clean) = merge_rename_content(
                Path::new("renamed.txt"),
                original,
                &ours,
                &theirs,
                "HEAD:a",
                "feature:b",
                "base:old",
                diffy::ConflictStyle::Merge,
                &mut TreeMergeContext::top_level(false, favor, None, &mut blobs),
            )
            .expect("outer binary merge");
            // Then: content selection cannot overwrite mode merging or turn
            // an unresolved mode conflict clean merely because -X is set.
            let selected = match favor {
                Some(MergeFavor::Theirs) => theirs.hash,
                Some(MergeFavor::Ours) | None => ours.hash,
            };
            assert_eq!(result.hash, selected);
            assert_eq!(result.mode, TreeItemMode::BlobExecutable);
            assert_eq!(clean, original.is_some() && favor.is_some());
        }
    }
}
