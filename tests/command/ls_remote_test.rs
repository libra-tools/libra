use std::{path::Path, str::FromStr};

use git_internal::{
    hash::{HashKind, ObjectHash, set_hash_kind_for_test},
    internal::object::{
        signature::{Signature, SignatureType},
        tag::Tag as GitTag,
        types::ObjectType,
    },
};
use libra::{
    command::save_object_to_storage,
    internal::{branch::Branch, db::get_db_conn_instance_for_path, model::reference},
    utils::{
        client_storage::ClientStorage,
        util::{DATABASE, ROOT_DIR},
    },
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use super::*;

#[test]
fn ls_remote_local_path_lists_head_and_branch_outside_repo() {
    let remote = create_committed_repo_via_cli();
    let outside = tempdir().expect("failed to create outside cwd");

    let remote_path = remote.path().to_string_lossy().to_string();
    let output = run_libra_command(&["ls-remote", &remote_path], outside.path());
    assert_cli_success(
        &output,
        "ls-remote local path should succeed outside a repo",
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\tHEAD"),
        "expected HEAD in ls-remote output, got: {stdout}"
    );
    assert!(
        stdout.contains("\trefs/heads/main"),
        "expected main branch in ls-remote output, got: {stdout}"
    );
}

#[test]
fn ls_remote_local_sha256_repo_lists_refs_outside_repo() {
    let remote = tempdir().expect("failed to create remote repository root");
    let output = run_libra_command(&["init", "--object-format", "sha256"], remote.path());
    assert_cli_success(&output, "failed to initialize sha256 repository");
    configure_identity_via_cli(remote.path());

    std::fs::write(remote.path().join("tracked.txt"), "tracked\n")
        .expect("failed to create tracked file");
    let output = run_libra_command(&["add", ".libraignore", "tracked.txt"], remote.path());
    assert_cli_success(&output, "failed to add tracked file");
    let output = run_libra_command(&["commit", "-m", "base", "--no-verify"], remote.path());
    assert_cli_success(&output, "failed to create initial commit");

    let outside = tempdir().expect("failed to create outside cwd");
    let remote_path = remote.path().to_string_lossy().to_string();
    let output = run_libra_command(&["ls-remote", &remote_path], outside.path());
    assert_cli_success(
        &output,
        "ls-remote local sha256 path should succeed outside a repo",
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let main_line = stdout
        .lines()
        .find(|line| line.ends_with("\trefs/heads/main"))
        .unwrap_or_else(|| panic!("expected main branch in ls-remote output, got: {stdout}"));
    let (hash, _) = main_line
        .split_once('\t')
        .expect("ls-remote ref line should be tab-separated");
    assert_eq!(hash.len(), 64, "expected sha256 ref hash, got: {stdout}");
}

#[test]
fn ls_remote_resolves_configured_remote_name() {
    let remote = create_committed_repo_via_cli();
    let local = create_committed_repo_via_cli();
    let remote_path = remote.path().to_string_lossy().to_string();

    let output = run_libra_command(&["remote", "add", "origin", &remote_path], local.path());
    assert_cli_success(&output, "remote add should succeed");

    let output = run_libra_command(&["ls-remote", "origin"], local.path());
    assert_cli_success(&output, "ls-remote origin should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\trefs/heads/main"),
        "expected configured remote main branch, got: {stdout}"
    );
}

#[test]
fn ls_remote_prefers_configured_remote_over_same_named_local_directory() {
    let remote = create_committed_repo_via_cli();
    let local = create_committed_repo_via_cli();
    let remote_path = remote.path().to_string_lossy().to_string();

    let output = run_libra_command(&["remote", "add", "origin", &remote_path], local.path());
    assert_cli_success(&output, "remote add should succeed");
    std::fs::create_dir(local.path().join("origin"))
        .expect("failed to create path-shaped remote-name directory");

    let output = run_libra_command(&["ls-remote", "origin"], local.path());
    assert_cli_success(
        &output,
        "ls-remote origin should resolve the configured remote",
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\trefs/heads/main"),
        "expected configured remote refs despite local origin/ directory, got: {stdout}"
    );
}

#[test]
fn ls_remote_heads_pattern_filters_refs() {
    let remote = create_committed_repo_via_cli();
    let outside = tempdir().expect("failed to create outside cwd");
    let remote_path = remote.path().to_string_lossy().to_string();

    let output = run_libra_command(
        &["ls-remote", "--heads", &remote_path, "main"],
        outside.path(),
    );
    assert_cli_success(&output, "ls-remote --heads main should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\trefs/heads/main"),
        "expected main branch in filtered output, got: {stdout}"
    );
    assert!(
        !stdout.contains("\tHEAD"),
        "--heads should not include HEAD, got: {stdout}"
    );
}

#[test]
fn ls_remote_heads_and_tags_returns_union() {
    let remote = create_committed_repo_via_cli();
    let outside = tempdir().expect("failed to create outside cwd");
    let tag_output = run_libra_command(&["tag", "v1.0"], remote.path());
    assert_cli_success(&tag_output, "tag creation should succeed");
    let remote_path = remote.path().to_string_lossy().to_string();

    let output = run_libra_command(
        &["ls-remote", "--heads", "--tags", &remote_path],
        outside.path(),
    );
    assert_cli_success(&output, "ls-remote --heads --tags should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\trefs/heads/main"),
        "expected branch ref in union output, got: {stdout}"
    );
    assert!(
        stdout.contains("\trefs/tags/v1.0"),
        "expected tag ref in union output, got: {stdout}"
    );
    assert!(
        !stdout.contains("\tHEAD"),
        "combined heads/tags filters should exclude HEAD, got: {stdout}"
    );
}

#[test]
fn ls_remote_tags_lists_local_libra_tags() {
    let remote = create_committed_repo_via_cli();
    let outside = tempdir().expect("failed to create outside cwd");
    let tag_output = run_libra_command(&["tag", "v1.0"], remote.path());
    assert_cli_success(&tag_output, "tag creation should succeed");
    let remote_path = remote.path().to_string_lossy().to_string();

    let output = run_libra_command(&["ls-remote", "--tags", &remote_path], outside.path());
    assert_cli_success(&output, "ls-remote --tags should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\trefs/tags/v1.0"),
        "expected local Libra tag ref, got: {stdout}"
    );
}

#[test]
fn ls_remote_tags_lists_peeled_annotated_tag_for_local_libra_remote() {
    let remote = create_committed_repo_via_cli();
    let outside = tempdir().expect("failed to create outside cwd");
    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], remote.path());
    assert_cli_success(&tag_output, "annotated tag creation should succeed");
    let remote_path = remote.path().to_string_lossy().to_string();

    let output = run_libra_command(&["ls-remote", "--tags", &remote_path], outside.path());
    assert_cli_success(&output, "ls-remote --tags should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\trefs/tags/v1.0\n"),
        "expected annotated tag object ref, got: {stdout}"
    );
    assert!(
        stdout.contains("\trefs/tags/v1.0^{}\n"),
        "expected peeled annotated tag ref, got: {stdout}"
    );

    let refs_output = run_libra_command(
        &["ls-remote", "--tags", "--refs", &remote_path],
        outside.path(),
    );
    assert_cli_success(&refs_output, "ls-remote --tags --refs should succeed");

    let refs_stdout = String::from_utf8_lossy(&refs_output.stdout);
    assert!(
        !refs_stdout.contains("\trefs/tags/v1.0^{}\n"),
        "--refs should suppress peeled annotated tag refs, got: {refs_stdout}"
    );
}

#[test]
fn ls_remote_tags_recursively_peels_nested_annotated_tag_for_local_libra_remote() {
    let remote = create_committed_repo_via_cli();
    let outside = tempdir().expect("failed to create outside cwd");
    let tag_output = run_libra_command(&["tag", "-m", "Inner release", "inner"], remote.path());
    assert_cli_success(&tag_output, "inner annotated tag creation should succeed");
    let (commit_hash, inner_tag_hash, outer_tag_hash) = create_nested_annotated_tag(remote.path());
    let remote_path = remote.path().to_string_lossy().to_string();

    let output = run_libra_command(&["ls-remote", "--tags", &remote_path], outside.path());
    assert_cli_success(&output, "ls-remote --tags should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("{outer_tag_hash}\trefs/tags/outer\n")),
        "expected outer annotated tag object ref, got: {stdout}"
    );
    assert!(
        stdout.contains(&format!("{commit_hash}\trefs/tags/outer^{{}}\n")),
        "expected outer peeled ref to final commit, got: {stdout}"
    );
    assert!(
        !stdout.contains(&format!("{inner_tag_hash}\trefs/tags/outer^{{}}\n")),
        "peeled ref should not stop at the inner tag object, got: {stdout}"
    );
}

fn create_nested_annotated_tag(repo: &Path) -> (String, String, String) {
    let _hash_guard = set_hash_kind_for_test(HashKind::Sha1);
    let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    runtime.block_on(async {
        let storage_path = repo.join(ROOT_DIR);
        let db = get_db_conn_instance_for_path(&storage_path.join(DATABASE))
            .await
            .expect("failed to open repository database");
        let inner_ref = reference::Entity::find()
            .filter(reference::Column::Name.eq("refs/tags/inner"))
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
            .one(&db)
            .await
            .expect("failed to query inner tag ref")
            .expect("inner tag ref should exist");
        let inner_tag_hash = inner_ref.commit.expect("inner tag should have a target");
        let inner_tag_id = ObjectHash::from_str(&inner_tag_hash).expect("inner tag hash is valid");
        let commit_hash = Branch::find_branch_result_with_conn(&db, "main", None)
            .await
            .expect("failed to resolve main branch")
            .expect("expected committed main branch")
            .commit
            .to_string();
        let outer_tag = GitTag::new(
            inner_tag_id,
            ObjectType::Tag,
            "outer".to_string(),
            Signature::new(
                SignatureType::Tagger,
                "Test User".to_string(),
                "test@example.com".to_string(),
            ),
            "Outer release".to_string(),
        );
        let storage = ClientStorage::init(storage_path.join("objects"));
        save_object_to_storage(&storage, &outer_tag, &outer_tag.id)
            .expect("failed to save outer tag object");
        let outer_tag_hash = outer_tag.id.to_string();
        reference::ActiveModel {
            name: Set(Some("refs/tags/outer".to_string())),
            kind: Set(reference::ConfigKind::Tag),
            commit: Set(Some(outer_tag_hash.clone())),
            remote: Set(None),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("failed to insert outer tag ref");

        (commit_hash, inner_tag_hash, outer_tag_hash)
    })
}

#[test]
fn ls_remote_json_reports_entries() {
    let remote = create_committed_repo_via_cli();
    let outside = tempdir().expect("failed to create outside cwd");
    let remote_path = remote.path().to_string_lossy().to_string();

    let output = run_libra_command(
        &["--json=compact", "ls-remote", &remote_path],
        outside.path(),
    );
    assert_cli_success(&output, "json ls-remote should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "ls-remote");
    let entries = json["data"]["entries"]
        .as_array()
        .expect("entries should be an array");
    assert!(
        entries
            .iter()
            .any(|entry| entry["refname"] == "refs/heads/main"),
        "expected refs/heads/main in entries: {json}"
    );
}
