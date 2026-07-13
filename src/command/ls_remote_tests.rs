use std::fs;

use git_internal::errors::GitError;
use serial_test::serial;
use tempfile::tempdir;

use super::{
    LsRemoteArgs, LsRemoteEntry, LsRemoteError, LsRemoteOutput, LsRemoteSymref,
    ls_remote_filter::{CompiledPattern, include_reference},
    ls_remote_redaction::{
        sanitize_discovery_error, sanitize_remote_error_reason, visible_remote_display,
        visible_remote_url,
    },
    parse_symrefs, resolve_output_symrefs, resolve_remote, write_ref_lines,
};
use crate::{
    internal::protocol::DiscRef,
    utils::{test::ChangeDirGuard, util},
};

#[test]
fn ls_remote_error_display_pins_each_owned_variant() {
    assert_eq!(
        LsRemoteError::ConfigRead("db locked".to_string()).to_string(),
        "failed to read remote configuration: db locked",
    );
    assert_eq!(
        LsRemoteError::InvalidRemote {
            spec: "ftp://example.com/repo".to_string(),
            reason: "unsupported scheme".to_string(),
        }
        .to_string(),
        "invalid remote 'ftp://example.com/repo': unsupported scheme",
    );
    assert_eq!(
        LsRemoteError::InvalidPattern {
            pattern: "**".to_string(),
            reason: "empty alternation".to_string(),
        }
        .to_string(),
        "invalid ref pattern '**': empty alternation",
    );
    assert_eq!(
        LsRemoteError::UnsupportedSortKey("unknown".to_string()).to_string(),
        "unsupported ls-remote sort key 'unknown'",
    );
}

fn disc_ref(refname: &str) -> DiscRef {
    DiscRef {
        _hash: "1111111111111111111111111111111111111111".to_string(),
        _ref: refname.to_string(),
    }
}

fn args_with_filters(heads: bool, tags: bool, refs: bool) -> LsRemoteArgs {
    LsRemoteArgs {
        heads,
        tags,
        refs,
        get_url: false,
        exit_code: false,
        sort: None,
        symref: false,
        repository: "origin".to_string(),
        patterns: vec![],
    }
}

#[test]
fn plain_pattern_matches_ref_tail() {
    let pattern = CompiledPattern::new("main").unwrap();
    assert!(pattern.matches("refs/heads/main"));
    assert!(!pattern.matches("refs/heads/feature"));
}

#[test]
fn glob_pattern_matches_nested_refs_across_slashes() {
    let full_ref = CompiledPattern::new("refs/heads/*").unwrap();
    assert!(full_ref.matches("refs/heads/feature/foo"));
    assert!(!full_ref.matches("refs/tags/feature/foo"));

    let tail_ref = CompiledPattern::new("feature*").unwrap();
    assert!(tail_ref.matches("refs/heads/feature/foo"));

    let question_ref = CompiledPattern::new("a?b").unwrap();
    assert!(question_ref.matches("refs/heads/a/b"));
}

#[test]
fn refs_flag_excludes_head_and_peeled_tags() {
    let args = args_with_filters(false, false, true);
    assert!(!include_reference(&disc_ref("HEAD"), &args, &[]));
    assert!(!include_reference(
        &disc_ref("refs/tags/v1.0^{}"),
        &args,
        &[]
    ));
    assert!(include_reference(&disc_ref("refs/tags/v1.0"), &args, &[]));
}

#[test]
fn heads_and_tags_filters_use_union() {
    let args = args_with_filters(true, true, false);
    assert!(include_reference(&disc_ref("refs/heads/main"), &args, &[]));
    assert!(include_reference(&disc_ref("refs/tags/v1.0"), &args, &[]));
    assert!(!include_reference(&disc_ref("HEAD"), &args, &[]));
}

#[test]
fn visible_remote_url_redacts_http_credentials() {
    assert_eq!(
        visible_remote_url("https://token@example.com/repo.git"),
        "https://example.com/repo.git"
    );
    assert_eq!(
        visible_remote_url("https://user:secret@example.com/repo.git"),
        "https://example.com/repo.git"
    );
}

#[test]
fn visible_remote_url_redacts_scp_password() {
    assert_eq!(
        visible_remote_url("user:secret@example.com:repo.git"),
        "[REDACTED]@example.com:repo.git"
    );
}

#[tokio::test]
#[serial]
async fn resolve_direct_url_skips_broken_current_repo_config() {
    let repo = tempdir().unwrap();
    let storage = repo.path().join(util::ROOT_DIR);
    fs::create_dir_all(&storage).unwrap();
    fs::write(storage.join(util::DATABASE), b"not sqlite").unwrap();
    let _guard = ChangeDirGuard::new(repo.path());

    let resolved = resolve_remote("https://example.com/repo.git")
        .await
        .unwrap();

    assert_eq!(
        resolved,
        (
            "https://example.com/repo.git".to_string(),
            "https://example.com/repo.git".to_string(),
            None
        )
    );
}

#[test]
fn visible_remote_display_redacts_direct_url_but_preserves_remote_name() {
    assert_eq!(
        visible_remote_display("https://token@example.com/repo.git", None),
        "https://example.com/repo.git"
    );
    assert_eq!(visible_remote_display("origin", Some("origin")), "origin");
}

#[test]
fn visible_remote_display_redacts_direct_scp_password() {
    assert_eq!(
        visible_remote_display("user:secret@example.com:repo.git", None),
        "[REDACTED]@example.com:repo.git"
    );
    assert_eq!(
        visible_remote_display("user:secret@example.com:repo.git", Some("origin")),
        "user:secret@example.com:repo.git"
    );
}

#[test]
fn invalid_remote_reason_redacts_valid_url_credentials() {
    let remote = "file://user:secret@example.com/repo.git";
    let reason = format!("invalid file url: {remote}");

    let sanitized = sanitize_remote_error_reason(&reason, remote);

    assert!(!sanitized.contains("user"));
    assert!(!sanitized.contains("secret"));
    assert!(sanitized.contains("file://example.com/repo.git"));
}

#[test]
fn invalid_remote_reason_redacts_malformed_url_like_credentials() {
    let remote = "https://user:secret@";
    let reason = format!("invalid local repository '{remote}': not found");

    let sanitized = sanitize_remote_error_reason(&reason, remote);

    assert!(!sanitized.contains("user"));
    assert!(!sanitized.contains("secret"));
    assert!(sanitized.contains("https://[REDACTED]@"));
}

#[test]
fn invalid_remote_reason_redacts_scp_like_password_credentials() {
    let remote = "user:secret@example.com:repo.git";
    let reason = format!("invalid local repository '{remote}': not found");

    let sanitized = sanitize_remote_error_reason(&reason, remote);

    assert!(!sanitized.contains("user:secret"));
    assert!(sanitized.contains("[REDACTED]@example.com:repo.git"));
}

#[test]
fn discovery_error_redacts_url_credentials_in_source() {
    let remote = "https://user:secret@example.invalid/repo.git";
    let source = GitError::NetworkError(format!(
        "Failed to send request: error sending request for url ({remote}/info/refs?service=git-upload-pack): dns error"
    ));

    let sanitized = sanitize_discovery_error(source, remote).to_string();

    assert!(!sanitized.contains("user"));
    assert!(!sanitized.contains("secret"));
    assert!(sanitized.contains("https://example.invalid/repo.git"));
}

#[test]
fn parse_symrefs_extracts_from_to_pairs() {
    let caps = vec![
        "multi_ack".to_string(),
        "symref=HEAD:refs/heads/main".to_string(),
        "symref=refs/remotes/origin/HEAD:refs/remotes/origin/main".to_string(),
        "agent=git/2.40".to_string(),
    ];
    let symrefs = parse_symrefs(&caps);
    assert_eq!(symrefs.len(), 2, "two well-formed symref capabilities");
    assert_eq!(symrefs[0].name, "HEAD");
    assert_eq!(symrefs[0].target, "refs/heads/main");
    assert_eq!(symrefs[1].name, "refs/remotes/origin/HEAD");
    assert_eq!(symrefs[1].target, "refs/remotes/origin/main");
}

#[test]
fn parse_symrefs_ignores_malformed_and_non_symref_capabilities() {
    let caps = vec![
        "thin-pack".to_string(),
        "symref=HEAD".to_string(),             // no `:` body
        "symref=:refs/heads/main".to_string(), // empty name
        "symref=HEAD:".to_string(),            // empty target
        "ofs-delta".to_string(),
    ];
    assert!(
        parse_symrefs(&caps).is_empty(),
        "malformed and non-symref capabilities yield no symrefs"
    );
}

fn entry(hash: &str, refname: &str) -> LsRemoteEntry {
    LsRemoteEntry {
        hash: hash.to_string(),
        refname: refname.to_string(),
    }
}

fn output_with(entries: Vec<LsRemoteEntry>, symrefs: Vec<LsRemoteSymref>) -> LsRemoteOutput {
    LsRemoteOutput {
        remote: "origin".to_string(),
        url: "https://example.com/repo.git".to_string(),
        heads_only: false,
        tags_only: false,
        refs_only: false,
        get_url: false,
        exit_code: false,
        sort: None,
        patterns: vec![],
        entries,
        symrefs,
    }
}

#[test]
fn resolve_output_symrefs_keeps_only_present_names() {
    let caps = vec!["symref=HEAD:refs/heads/main".to_string()];
    let entries = vec![
        entry("a".repeat(40).as_str(), "HEAD"),
        entry("a".repeat(40).as_str(), "refs/heads/main"),
    ];
    let discovered = vec![disc_ref("HEAD"), disc_ref("refs/heads/main")];
    let symrefs = resolve_output_symrefs(&caps, &entries, &discovered, true);
    assert_eq!(symrefs.len(), 1);
    assert_eq!(symrefs[0].name, "HEAD");
    assert_eq!(symrefs[0].target, "refs/heads/main");
}

#[test]
fn resolve_output_symrefs_drops_filtered_out_names() {
    // HEAD advertised but excluded from the entries (e.g. `--heads`): no symref.
    let caps = vec!["symref=HEAD:refs/heads/main".to_string()];
    let entries = vec![entry("b".repeat(40).as_str(), "refs/heads/main")];
    let discovered = vec![disc_ref("HEAD"), disc_ref("refs/heads/main")];
    assert!(resolve_output_symrefs(&caps, &entries, &discovered, true).is_empty());
}

#[test]
fn resolve_output_symrefs_empty_when_not_requested_or_no_capability() {
    let entries = vec![entry("c".repeat(40).as_str(), "HEAD")];
    // Not requested.
    assert!(
        resolve_output_symrefs(
            &["symref=HEAD:refs/heads/main".to_string()],
            &entries,
            &[disc_ref("HEAD"), disc_ref("refs/heads/main")],
            false
        )
        .is_empty()
    );
    // Requested but the remote advertised no `symref=` capability (e.g. a local
    // Libra repo; local Git repos via git-upload-pack do advertise it).
    assert!(resolve_output_symrefs(&[], &entries, &[disc_ref("HEAD")], true).is_empty());
}

#[test]
fn resolve_output_symrefs_derives_local_libra_head_without_capability() {
    let oid = "c".repeat(40);
    let entries = vec![entry(&oid, "HEAD"), entry(&oid, "refs/heads/main")];
    let discovered = vec![
        DiscRef {
            _hash: oid.clone(),
            _ref: "HEAD".to_string(),
        },
        DiscRef {
            _hash: oid,
            _ref: "refs/heads/main".to_string(),
        },
    ];
    let symrefs = resolve_output_symrefs(&[], &entries, &discovered, true);
    assert_eq!(symrefs.len(), 1);
    assert_eq!(symrefs[0].name, "HEAD");
    assert_eq!(symrefs[0].target, "refs/heads/main");
}

#[test]
fn write_ref_lines_emits_ref_line_before_matching_oid() {
    let oid = "d".repeat(40);
    let data = output_with(
        vec![
            entry(&oid, "HEAD"),
            entry(&oid, "refs/heads/main"),
            entry(&"e".repeat(40), "refs/tags/v1"),
        ],
        vec![LsRemoteSymref {
            name: "HEAD".to_string(),
            target: "refs/heads/main".to_string(),
        }],
    );
    let mut buf: Vec<u8> = Vec::new();
    write_ref_lines(&mut buf, &data).unwrap();
    let text = String::from_utf8(buf).unwrap();
    assert_eq!(
        text,
        format!(
            "ref: refs/heads/main\tHEAD\n{oid}\tHEAD\n{oid}\trefs/heads/main\n{}\trefs/tags/v1\n",
            "e".repeat(40)
        ),
        "ref: line precedes only the HEAD oid line, in order"
    );
}

#[test]
fn ls_remote_output_json_omits_empty_symrefs_and_includes_populated() {
    // Empty symrefs => field omitted (skip_serializing_if).
    let empty = output_with(vec![entry(&"f".repeat(40), "refs/heads/main")], vec![]);
    let value = serde_json::to_value(&empty).unwrap();
    assert!(
        value.as_object().unwrap().get("symrefs").is_none(),
        "empty symrefs must be omitted from JSON"
    );

    // Populated symrefs => present with name/target.
    let populated = output_with(
        vec![entry(&"f".repeat(40), "HEAD")],
        vec![LsRemoteSymref {
            name: "HEAD".to_string(),
            target: "refs/heads/main".to_string(),
        }],
    );
    let value = serde_json::to_value(&populated).unwrap();
    assert_eq!(
        value["symrefs"],
        serde_json::json!([{"name": "HEAD", "target": "refs/heads/main"}]),
    );
}
