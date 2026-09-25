//! Cloud backup and storage tests covering D1 metadata, R2 object storage, and full sync/restore workflows.
//!
//! Three concentric circles of coverage live here:
//! 1. Mock tests (`mock_*`) exercise `RemoteStorage`, `TieredStorage`, and `search`
//!    against `object_store::memory::InMemory` — fully deterministic.
//! 2. Configuration-error tests (`cloud_*_fails_without_*`) shell out to the real
//!    binary with one half of the cloud env vars deliberately missing, asserting
//!    we surface a precise actionable error mentioning the missing variable.
//! 3. Live cloud tests (`d1_*`, `r2_*`, `cloud_full_workflow_end_to_end`,
//!    `cloud_sync_name_conflict`) hit production Cloudflare D1 + R2.
//!
//! **Layer:** Mock + error-path tests are L1. Live tests are L3 — require
//! `--features test-live-cloud` plus `LIBRA_D1_*` and/or `LIBRA_STORAGE_*`.
//! Most legacy live cases skip when the feature or credentials are unset; the
//! feature-gated `cloud_live_preflight` fails on missing credentials. Run selected
//! live cases through `tests/cloud_live_no_skip.sh` to reject silent skips.
//! Live tests use `#[serial(cloud_live)]` to avoid shared D1/R2 collisions.

use std::{path::Path, process::Command, str::FromStr, sync::Arc};

use git_internal::internal::object::{ObjectTrait, blob::Blob};
use libra::utils::{
    d1_client::{D1Client, D1Statement},
    storage::{Storage, local::LocalStorage, remote::RemoteStorage, tiered::TieredStorage},
};
use object_store::memory::InMemory;
use serial_test::serial;
use tempfile::tempdir;
use uuid::Uuid;

fn env_is_present(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty())
}

fn live_d1_tests_enabled() -> bool {
    cfg!(feature = "test-live-cloud")
        && [
            "LIBRA_D1_ACCOUNT_ID",
            "LIBRA_D1_API_TOKEN",
            "LIBRA_D1_DATABASE_ID",
        ]
        .iter()
        .all(|name| env_is_present(name))
}

fn live_r2_tests_enabled() -> bool {
    cfg!(feature = "test-live-cloud")
        && [
            "LIBRA_STORAGE_ENDPOINT",
            "LIBRA_STORAGE_BUCKET",
            "LIBRA_STORAGE_ACCESS_KEY",
            "LIBRA_STORAGE_SECRET_KEY",
        ]
        .iter()
        .all(|name| env_is_present(name))
}

fn live_cloud_tests_enabled() -> bool {
    live_d1_tests_enabled() && live_r2_tests_enabled()
}

#[cfg(feature = "test-live-cloud")]
const CLOUD_LIVE_REQUIRED_ENV: [&str; 7] = [
    "LIBRA_D1_ACCOUNT_ID",
    "LIBRA_D1_API_TOKEN",
    "LIBRA_D1_DATABASE_ID",
    "LIBRA_STORAGE_ENDPOINT",
    "LIBRA_STORAGE_BUCKET",
    "LIBRA_STORAGE_ACCESS_KEY",
    "LIBRA_STORAGE_SECRET_KEY",
];

#[cfg(feature = "test-live-cloud")]
fn cloud_live_preflight_env() -> Result<(), String> {
    for name in CLOUD_LIVE_REQUIRED_ENV {
        let value =
            std::env::var(name).map_err(|_| format!("cloud live preflight: missing {name}"))?;
        if value.trim().is_empty() {
            return Err(format!("cloud live preflight: empty {name}"));
        }
    }
    Ok(())
}

/// Named L3 gate: run this alone before any cloud test that writes D1 or R2.
#[cfg(feature = "test-live-cloud")]
#[test]
fn cloud_live_preflight() {
    if let Err(message) = cloud_live_preflight_env() {
        panic!("{message}");
    }
}

#[cfg(feature = "test-live-cloud")]
mod local_cloud_preflight_tests {
    use std::{net::TcpListener, path::Path, process::Command};

    use syn::{ExprCall, ExprMacro, ExprMethodCall, ExprStruct, ItemFn, StmtMacro, visit::Visit};

    const FAKE_CLOUD_ENV: [(&str, &str); 7] = [
        ("LIBRA_D1_ACCOUNT_ID", "fake-account"),
        ("LIBRA_D1_API_TOKEN", "fake-token"),
        ("LIBRA_D1_DATABASE_ID", "fake-database"),
        ("LIBRA_STORAGE_ENDPOINT", "http://127.0.0.1:1"),
        ("LIBRA_STORAGE_BUCKET", "fake-bucket"),
        ("LIBRA_STORAGE_ACCESS_KEY", "fake-access"),
        ("LIBRA_STORAGE_SECRET_KEY", "fake-secret"),
    ];
    const D1_REQUIRED_ENV: [&str; 3] = [
        "LIBRA_D1_ACCOUNT_ID",
        "LIBRA_D1_API_TOKEN",
        "LIBRA_D1_DATABASE_ID",
    ];
    const R2_REQUIRED_ENV: [&str; 4] = [
        "LIBRA_STORAGE_ENDPOINT",
        "LIBRA_STORAGE_BUCKET",
        "LIBRA_STORAGE_ACCESS_KEY",
        "LIBRA_STORAGE_SECRET_KEY",
    ];

    #[derive(Clone, Copy)]
    enum CloudVarOverride<'a> {
        Missing(&'a str),
        Empty(&'a str),
    }

    fn fake_environment(command: &mut Command, r2_endpoint: &str, fake_home: &Path) {
        std::fs::create_dir_all(fake_home).expect("create fake cloud test home");
        command.env_clear();
        for (name, value) in FAKE_CLOUD_ENV {
            command.env(name, value);
        }
        command.env("LIBRA_STORAGE_ENDPOINT", r2_endpoint);
        command.env("HOME", fake_home);
        command.env("USERPROFILE", fake_home);
        command.env("XDG_CONFIG_HOME", fake_home.join(".config"));
    }

    fn preflight_child(
        changed: Option<CloudVarOverride<'_>>,
        endpoint: &str,
        list_only: bool,
    ) -> std::process::Output {
        let isolation = tempfile::tempdir().expect("create fake preflight environment");
        let executable = std::env::current_exe().expect("locate current test binary");
        let mut command = Command::new(executable);
        command.args(["--exact", "cloud_live_preflight"]);
        if list_only {
            command.arg("--list");
        } else {
            command.arg("--nocapture");
        }
        fake_environment(&mut command, endpoint, isolation.path());
        command.current_dir(isolation.path());
        match changed {
            Some(CloudVarOverride::Missing(name)) => {
                command.env_remove(name);
            }
            Some(CloudVarOverride::Empty(name)) => {
                command.env(name, "");
            }
            None => {}
        }
        command.output().expect("run isolated cloud preflight")
    }

    fn combined_output(output: &std::process::Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn assert_key_failure_matrix(group: &str, names: &[&str]) {
        assert_preflight_source_is_local();
        let r2_listener = TcpListener::bind("127.0.0.1:0").expect("bind fake R2 listener");
        r2_listener
            .set_nonblocking(true)
            .expect("nonblocking fake R2 listener");
        let endpoint = format!("http://{}", r2_listener.local_addr().expect("R2 address"));
        let mut checked = 0;
        for name in names {
            for (kind, changed) in [
                ("missing", CloudVarOverride::Missing(name)),
                ("empty", CloudVarOverride::Empty(name)),
            ] {
                let output = preflight_child(Some(changed), &endpoint, false);
                let text = combined_output(&output);
                let expected_error = format!("cloud live preflight: {kind} {name}");
                assert!(
                    !output.status.success(),
                    "{group} {name} {kind} must fail: {text}"
                );
                assert!(
                    text.contains(&expected_error),
                    "{group} {name} {kind} reported the wrong error: {text}"
                );
                assert_eq!(
                    text.matches("cloud live preflight:").count(),
                    1,
                    "{group} {name} {kind} must identify only the changed key: {text}"
                );
                assert!(
                    !text.contains("test result: ok."),
                    "{group} {name} {kind}: {text}"
                );
                assert!(!text.to_ascii_lowercase().contains("skipped ("));
                println!(
                    "{group} {name} {kind} child_exit={:?} error={expected_error}",
                    output.status.code()
                );
                checked += 1;
            }
        }
        let network_calls = r2_listener
            .incoming()
            .take_while(|result| result.is_ok())
            .count();
        assert_eq!(network_calls, 0, "{group} preflight contacted fake R2");
        assert_eq!(checked, names.len() * 2);
        println!("{group} checked={checked} R2_loopback_calls={network_calls}");
    }

    #[test]
    fn local_cloud_preflight_missing_d1_is_error() {
        assert_key_failure_matrix("D1", &D1_REQUIRED_ENV);
    }

    #[test]
    fn local_cloud_preflight_missing_r2_is_error() {
        assert_key_failure_matrix("R2", &R2_REQUIRED_ENV);
    }

    #[test]
    fn local_cloud_preflight_complete_fake_env_is_local_only() {
        assert_preflight_source_is_local();
        let r2_listener = TcpListener::bind("127.0.0.1:0").expect("bind fake R2 listener");
        r2_listener
            .set_nonblocking(true)
            .expect("nonblocking fake R2 listener");

        let r2_endpoint = format!("http://{}", r2_listener.local_addr().expect("R2 address"));
        let feature_on_list = preflight_child(None, &r2_endpoint, true);
        assert!(
            feature_on_list.status.success(),
            "feature-on list failed: {}",
            combined_output(&feature_on_list)
        );
        let feature_on = preflight_child(None, &r2_endpoint, false);
        let feature_on_text = combined_output(&feature_on);
        assert!(
            feature_on.status.success(),
            "feature-on preflight failed: {feature_on_text}"
        );
        let selected_on = feature_on_list
            .stdout
            .split(|byte| *byte == b'\n')
            .filter_map(|line| line.strip_suffix(b": test"))
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect::<Vec<_>>();
        let exact_result = regex::Regex::new(
            r"(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; (?P<filtered>[0-9]+) filtered out; finished in [0-9.]+s$",
        )
        .expect("compile exact libtest result expression");
        let result_captures = exact_result.captures(&feature_on_text);
        let run_pass_skip = if feature_on_text.contains("running 1 test")
            && feature_on_text.contains("test cloud_live_preflight ... ok")
            && result_captures.is_some()
            && !feature_on_text.contains("skipped (set --features test-live-cloud")
        {
            (1, 1, 0)
        } else {
            (0, 0, 1)
        };
        // The D1 client has a fixed Cloudflare URL; the source guard above is
        // the D1 no-client proof. This listener observes the injected R2 URL.
        let network_calls = r2_listener
            .incoming()
            .take_while(|result| result.is_ok())
            .count();
        let observed = (selected_on, run_pass_skip, network_calls);
        let expected = (vec!["cloud_live_preflight".to_string()], (1, 1, 0), 0);
        let filtered_out = result_captures
            .and_then(|capture| capture.name("filtered"))
            .map(|value| value.as_str())
            .unwrap_or("missing");
        println!("preflight observed={observed:?}, libtest filtered_out={filtered_out}");
        assert_eq!(
            observed, expected,
            "isolated preflight observation: {feature_on_text}"
        );
    }

    #[derive(Default)]
    struct PreflightCalls {
        calls: Vec<String>,
        methods: Vec<String>,
        macros: Vec<String>,
        structs: Vec<String>,
    }

    impl<'ast> Visit<'ast> for PreflightCalls {
        fn visit_expr_call(&mut self, node: &'ast ExprCall) {
            if let syn::Expr::Path(path) = node.func.as_ref() {
                self.calls.push(
                    path.path
                        .segments
                        .iter()
                        .map(|segment| segment.ident.to_string())
                        .collect::<Vec<_>>()
                        .join("::"),
                );
            } else {
                self.calls.push("<indirect call>".to_string());
            }
            syn::visit::visit_expr_call(self, node);
        }

        fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
            self.methods.push(node.method.to_string());
            syn::visit::visit_expr_method_call(self, node);
        }

        fn visit_expr_macro(&mut self, node: &'ast ExprMacro) {
            self.macros.push(
                node.mac
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::"),
            );
            syn::visit::visit_expr_macro(self, node);
        }

        fn visit_stmt_macro(&mut self, node: &'ast StmtMacro) {
            self.macros.push(
                node.mac
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::"),
            );
            syn::visit::visit_stmt_macro(self, node);
        }

        fn visit_expr_struct(&mut self, node: &'ast ExprStruct) {
            self.structs.push(
                node.path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::"),
            );
            syn::visit::visit_expr_struct(self, node);
        }
    }

    fn audit_fn(
        function: &ItemFn,
        calls: &[&str],
        methods: &[&str],
        macros: &[&str],
    ) -> Result<(), String> {
        let mut observed = PreflightCalls::default();
        observed.visit_block(&function.block);
        observed.calls.sort();
        observed.methods.sort();
        observed.macros.sort();
        observed.structs.sort();
        let expected = (
            calls
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>(),
            methods
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>(),
            macros
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>(),
            Vec::<String>::new(),
        );
        let actual = (
            observed.calls,
            observed.methods,
            observed.macros,
            observed.structs,
        );
        if actual == expected {
            Ok(())
        } else {
            Err(format!("preflight call manifest changed: {actual:?}"))
        }
    }

    fn assert_preflight_source_is_local() {
        let source = include_str!("cloud_storage_backup_test.rs");
        let file = syn::parse_file(source).expect("parse current cloud test source");
        let find_fn = |name: &str| {
            file.items.iter().find_map(|item| match item {
                syn::Item::Fn(function) if function.sig.ident == name => Some(function),
                _ => None,
            })
        };
        let helper = find_fn("cloud_live_preflight_env").expect("preflight env helper exists");
        let gate = find_fn("cloud_live_preflight").expect("preflight test exists");
        audit_fn(
            helper,
            &["Err", "Ok", "std::env::var"],
            &["is_empty", "map_err", "trim"],
            &["format", "format"],
        )
        .expect("preflight env helper may only read environment");
        audit_fn(gate, &["cloud_live_preflight_env"], &[], &["panic"])
            .expect("preflight gate may only call env helper");
    }

    #[test]
    fn local_cloud_preflight_has_no_remote_client_references() {
        assert_preflight_source_is_local();
        let source = include_str!("cloud_storage_backup_test.rs");
        let file = syn::parse_file(source).expect("parse current cloud test source");
        let baseline = file
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Fn(function) if function.sig.ident == "cloud_live_preflight_env" => {
                    Some(function.clone())
                }
                _ => None,
            })
            .expect("preflight env helper exists");
        let mutants = [
            (
                "D1_client_constructor",
                syn::parse_quote! {
                    D1Client::new(account, token, database);
                },
                "D1Client::new",
            ),
            (
                "R2_reachable_helper",
                syn::parse_quote! {
                    r2_storage_from_env("fake-repo");
                },
                "r2_storage_from_env",
            ),
            (
                "direct_HTTP_send",
                syn::parse_quote! {
                    client.send();
                },
                "send",
            ),
        ];
        let rejected = mutants
            .into_iter()
            .map(|(name, added_call, expected_sink)| {
                let mut mutant = baseline.clone();
                let before_return = mutant.block.stmts.len() - 1;
                mutant.block.stmts.insert(before_return, added_call);
                let diagnostic = audit_fn(
                    &mutant,
                    &["Err", "Ok", "std::env::var"],
                    &["is_empty", "map_err", "trim"],
                    &["format", "format"],
                )
                .expect_err("injected network sink must change the exact call manifest");
                let rejected = diagnostic.contains(expected_sink);
                println!("preflight mutant {name} rejected={rejected} sink={expected_sink}");
                (name, rejected)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rejected,
            vec![
                ("D1_client_constructor", true),
                ("R2_reachable_helper", true),
                ("direct_HTTP_send", true),
            ]
        );
    }
}

/// Read an env var or panic with a pointer to the file header for setup instructions.
/// Used inside live-cloud tests after the gate condition has already confirmed the
/// variable is set, so a panic here genuinely indicates a partial cloud config.
fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("Missing required env var: {name}. See tests/cloud_storage_backup_test.rs header for setup.")
    })
}

/// Build a Cloudflare D1 client from `LIBRA_D1_*` env vars. Callers must have already
/// gated on `LIBRA_D1_ACCOUNT_ID` being set (see the live-cloud tests).
fn d1_client_from_env() -> D1Client {
    D1Client::new(
        required_env("LIBRA_D1_ACCOUNT_ID"),
        required_env("LIBRA_D1_API_TOKEN"),
        required_env("LIBRA_D1_DATABASE_ID"),
    )
}

/// Build a `RemoteStorage` pointing at the configured S3-compatible bucket, scoped to
/// `repo_id` so tests cannot trample each other's objects in shared infrastructure.
/// Defaults `LIBRA_STORAGE_REGION` to "auto" because R2 is region-less.
fn r2_storage_from_env(repo_id: &str) -> RemoteStorage {
    let endpoint = required_env("LIBRA_STORAGE_ENDPOINT");
    let bucket = required_env("LIBRA_STORAGE_BUCKET");
    let access_key = required_env("LIBRA_STORAGE_ACCESS_KEY");
    let secret_key = required_env("LIBRA_STORAGE_SECRET_KEY");
    let region = std::env::var("LIBRA_STORAGE_REGION").unwrap_or_else(|_| "auto".to_string());

    let s3 = object_store::aws::AmazonS3Builder::new()
        .with_bucket_name(bucket)
        .with_region(region)
        .with_endpoint(endpoint)
        .with_access_key_id(access_key)
        .with_secret_access_key(secret_key)
        .with_virtual_hosted_style_request(false)
        .build()
        .expect("Failed to build S3 client");

    RemoteStorage::new_with_prefix(Arc::new(s3), repo_id.to_string())
}

async fn assert_remote_object_available(
    storage: &RemoteStorage,
    hash: &git_internal::hash::ObjectHash,
    description: &str,
) {
    let mut last_error = "object was not visible".to_string();

    for attempt in 0..8 {
        match storage.get(hash).await {
            Ok((data, obj_type)) => {
                let computed = git_internal::hash::ObjectHash::from_type_and_data(obj_type, &data);
                assert_eq!(
                    computed, *hash,
                    "{} was readable but hashed to {} instead of {}",
                    description, computed, hash
                );
                return;
            }
            Err(error) => {
                last_error = error.to_string();
                tokio::time::sleep(std::time::Duration::from_millis(250 * (attempt + 1))).await;
            }
        }
    }

    panic!(
        "{} {} should be readable from remote storage after sync; last error: {}",
        description, hash, last_error
    );
}

fn isolated_libra_command(current_dir: &Path, home: &Path) -> Command {
    let config_home = home.join(".config");
    let global_config_db = home.join(".libra-global-config.db");
    std::fs::create_dir_all(&config_home).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_libra"));
    command
        .current_dir(current_dir)
        .env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
        )
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", home)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("LIBRA_TEST", "1")
        .env("LIBRA_TEST_ENV", "1")
        .env("LIBRA_CONFIG_GLOBAL_DB", &global_config_db);
    if let Some(systemroot) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", systemroot);
    }
    if let Some(windir) = std::env::var_os("WINDIR") {
        command.env("WINDIR", windir);
    }
    command
}

/// Initialize a new Libra repo in a temp dir using the actual binary, with a fully
/// isolated HOME / XDG_CONFIG_HOME / USERPROFILE so global user config cannot leak
/// in. Returns the `TempDir` (must stay alive — drop removes the on-disk repo).
fn init_repo() -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let home = dir.path().join(".home");
    let output = isolated_libra_command(dir.path(), &home)
        .args(["init"])
        .output()
        .unwrap();
    assert!(output.status.success());
    dir
}

/// Scenario: store a single blob through `RemoteStorage` backed by an in-memory
/// `object_store`, then exist-check and re-fetch it. Smoke-tests the
/// `Storage::put`/`exist`/`get` contract for the remote backend.
#[tokio::test]
async fn mock_remote_storage_basic() {
    let memory_store = Arc::new(InMemory::new());
    let remote_storage = RemoteStorage::new(memory_store);

    let blob = Blob::from_content("Hello Mock Storage!");
    let path = remote_storage
        .put(&blob.id, &blob.data, blob.get_type())
        .await
        .expect("Put failed");
    assert!(!path.is_empty());
    assert!(remote_storage.exist(&blob.id).await);

    let (data, obj_type) = remote_storage.get(&blob.id).await.expect("Get failed");
    assert_eq!(data, blob.data);
    assert_eq!(obj_type, blob.get_type());
}

/// Scenario: when constructed with `new_with_prefix("repo-a")`, every put writes
/// under `repo-a/objects/...`. Pins the per-repo prefix isolation contract that the
/// cloud backup workflow depends on for multi-tenant safety.
#[tokio::test]
async fn mock_remote_storage_with_repo_prefix() {
    let memory_store = Arc::new(InMemory::new());
    let remote_storage = RemoteStorage::new_with_prefix(memory_store, "repo-a".to_string());

    let blob = Blob::from_content("Hello Prefix!");
    let path = remote_storage
        .put(&blob.id, &blob.data, blob.get_type())
        .await
        .expect("Put failed");

    assert!(path.starts_with("repo-a/objects/"));
    assert!(remote_storage.exist(&blob.id).await);
}

/// Scenario: with a 10-byte threshold, a 3-byte blob and a 15-byte blob both end up
/// in local storage (small objects are stored permanently, large objects are LRU
/// cached locally) and the large blob remains retrievable through the tier
/// abstraction. Pins the dual-write semantics the production tiered backend relies
/// on.
#[tokio::test]
async fn mock_tiered_storage_logic() {
    let memory_store = Arc::new(InMemory::new());
    let remote = RemoteStorage::new(memory_store);

    let dir = tempdir().unwrap();
    let local = LocalStorage::new(dir.path().to_path_buf());

    let tiered = TieredStorage::new(local.clone(), remote, 10, 1024);

    let small_blob = Blob::from_content("123");
    tiered
        .put(&small_blob.id, &small_blob.data, small_blob.get_type())
        .await
        .expect("Put small failed");
    assert!(local.exist(&small_blob.id).await);

    let large_blob = Blob::from_content("123456789012345");
    tiered
        .put(&large_blob.id, &large_blob.data, large_blob.get_type())
        .await
        .expect("Put large failed");
    assert!(local.exist(&large_blob.id).await);

    let (data, _) = tiered.get(&large_blob.id).await.expect("Get large failed");
    assert_eq!(data, large_blob.data);
}

/// Scenario: insert a blob with a known hex prefix and verify `search` returns a
/// match for full and partial prefixes (`"aabb"`, `"a"`) and an empty result for a
/// non-matching prefix (`"ccdd"`). Guards the prefix-search contract that the
/// `cloud restore` flow uses.
#[tokio::test]
async fn mock_remote_search() {
    let memory_store = Arc::new(InMemory::new());
    let remote_storage = RemoteStorage::new(memory_store);

    let hash_str = "aabbccdd12345678901234567890123456789012";
    let hash = git_internal::hash::ObjectHash::from_str(hash_str).unwrap();
    let blob = Blob::from_content("search me");
    remote_storage
        .put(&hash, &blob.data, blob.get_type())
        .await
        .unwrap();

    let res = remote_storage.search("aabb").await;
    assert_eq!(res.len(), 1);
    assert_eq!(res[0], hash);

    let res = remote_storage.search("a").await;
    assert_eq!(res.len(), 1);
    assert_eq!(res[0], hash);

    let res = remote_storage.search("ccdd").await;
    assert!(res.is_empty());
}

/// Scenario: invoke `libra cloud sync` with D1 env vars present but R2 absent and
/// confirm the binary exits non-zero with the typed auth error contract:
/// `LBR-AUTH-001`, operation-scoped summary (`missing cloud configuration for sync`),
/// and the specific missing variable `LIBRA_STORAGE_ENDPOINT`.
#[test]
fn cloud_sync_fails_without_r2_env() {
    let dir = init_repo();
    let home = dir.path().join(".home");
    let output = isolated_libra_command(dir.path(), &home)
        .args(["cloud", "sync"])
        .env("LIBRA_D1_ACCOUNT_ID", "test-account")
        .env("LIBRA_D1_API_TOKEN", "test-token")
        .env("LIBRA_D1_DATABASE_ID", "test-db")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Error-Code: LBR-AUTH-001"));
    assert!(stderr.contains("missing cloud configuration for sync"));
    assert!(stderr.contains("LIBRA_STORAGE_ENDPOINT"));
}

/// Scenario: same as the sync variant but for `cloud restore` — when D1 is set and
/// R2 is missing, the binary surfaces `LBR-AUTH-001`,
/// `missing cloud configuration for restore`, and `LIBRA_STORAGE_ENDPOINT` so the
/// user knows which variable to set.
#[test]
fn cloud_restore_fails_without_r2_env() {
    let dir = init_repo();
    let home = dir.path().join(".home");
    let output = isolated_libra_command(dir.path(), &home)
        .args(["cloud", "restore", "--repo-id", "test-repo"])
        .env("LIBRA_D1_ACCOUNT_ID", "test-account")
        .env("LIBRA_D1_API_TOKEN", "test-token")
        .env("LIBRA_D1_DATABASE_ID", "test-db")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Error-Code: LBR-AUTH-001"));
    assert!(stderr.contains("missing cloud configuration for restore"));
    assert!(stderr.contains("LIBRA_STORAGE_ENDPOINT"));
}

/// Scenario: invoke `libra cloud sync` with R2 env vars present but D1 absent and
/// confirm the auth contract still reports `LBR-AUTH-001` plus
/// `LIBRA_D1_ACCOUNT_ID` as a missing key.
#[test]
fn cloud_sync_fails_without_d1_env() {
    let dir = init_repo();
    let home = dir.path().join(".home");
    let output = isolated_libra_command(dir.path(), &home)
        .args(["cloud", "sync"])
        .env("LIBRA_STORAGE_ENDPOINT", "https://example.invalid")
        .env("LIBRA_STORAGE_BUCKET", "test-bucket")
        .env("LIBRA_STORAGE_ACCESS_KEY", "test-access")
        .env("LIBRA_STORAGE_SECRET_KEY", "test-secret")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Error-Code: LBR-AUTH-001"));
    assert!(stderr.contains("missing cloud configuration for sync"));
    assert!(stderr.contains("LIBRA_D1_ACCOUNT_ID"));
}

/// Scenario: live D1 smoke test — submit `SELECT 1` to confirm the API token,
/// account ID, and database ID are wired correctly. Skipped silently when
/// `LIBRA_D1_ACCOUNT_ID` is unset.
#[tokio::test]
#[serial(cloud_live)]
async fn d1_connection() {
    if !live_d1_tests_enabled() {
        eprintln!("skipped (set --features test-live-cloud and LIBRA_D1_*)");
        return;
    }
    let client = d1_client_from_env();
    let result = client.execute("SELECT 1 as test", None).await;
    assert!(result.is_ok(), "D1 connection failed: {:?}", result.err());
}

/// Scenario: call `ensure_object_index_table` against live D1. Verifies the DDL
/// the cloud backup layer issues is accepted by the real database and is idempotent
/// (the test runs against a possibly-already-existing table). Skipped without D1
/// credentials.
#[tokio::test]
#[serial(cloud_live)]
async fn d1_ensure_table() {
    if !live_d1_tests_enabled() {
        eprintln!("skipped (set --features test-live-cloud and LIBRA_D1_*)");
        return;
    }
    let client = d1_client_from_env();
    let result = client.ensure_object_index_table().await;
    assert!(result.is_ok(), "Failed to create table: {:?}", result.err());
}

/// Scenario: against live D1, upsert one object index row using a timestamp-suffixed
/// hash and confirm `get_object_indexes` returns it. The timestamp suffix avoids
/// collisions across test runs that share the same D1 instance. Skipped without D1
/// credentials.
#[tokio::test]
#[serial(cloud_live)]
async fn d1_upsert_and_query() {
    if !live_d1_tests_enabled() {
        eprintln!("skipped (set --features test-live-cloud and LIBRA_D1_*)");
        return;
    }
    let client = d1_client_from_env();
    client.ensure_object_index_table().await.unwrap();

    let test_hash = format!("test_hash_{}", chrono::Utc::now().timestamp());
    client
        .upsert_object_index(
            &test_hash,
            "blob",
            100,
            "test-repo-id",
            chrono::Utc::now().timestamp(),
        )
        .await
        .unwrap();

    let indexes = client.get_object_indexes("test-repo-id").await.unwrap();
    assert!(indexes.iter().any(|idx| idx.o_id == test_hash));
}

/// Scenario: against live D1, execute three INSERT statements via the batch API and
/// confirm all three rows land. Pins the contract that `cloud sync` relies on when
/// pushing many object-index entries in one round trip. Skipped without D1
/// credentials.
#[tokio::test]
#[serial(cloud_live)]
async fn d1_batch() {
    if !live_d1_tests_enabled() {
        eprintln!("skipped (set --features test-live-cloud and LIBRA_D1_*)");
        return;
    }
    let client = d1_client_from_env();
    client.ensure_object_index_table().await.unwrap();

    let timestamp = chrono::Utc::now().timestamp();
    let statements: Vec<D1Statement> = (0..3)
        .map(|i| D1Statement {
            sql: "INSERT OR REPLACE INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) VALUES (?1, ?2, ?3, ?4, ?5, ?6)".to_string(),
            params: Some(vec![
                serde_json::json!(format!("batch_test_{}_{}", timestamp, i)),
                serde_json::json!("blob"),
                serde_json::json!(i * 100),
                serde_json::json!("batch-test-repo"),
                serde_json::json!(timestamp),
                serde_json::json!(1),
            ]),
        })
        .collect();

    let result = client.batch(statements).await;
    assert!(result.is_ok(), "Batch operation failed: {:?}", result.err());

    let indexes = client.get_object_indexes("batch-test-repo").await.unwrap();
    let batch_count = indexes
        .iter()
        .filter(|idx| idx.o_id.starts_with(&format!("batch_test_{}", timestamp)))
        .count();
    assert_eq!(batch_count, 3);
}

/// Scenario: against live R2 (or any S3-compatible endpoint), put a blob, confirm
/// existence, and read it back. The content is timestamp-suffixed so concurrent or
/// repeated runs do not collide. Skipped without `LIBRA_STORAGE_ENDPOINT`.
#[tokio::test]
#[serial(cloud_live)]
async fn r2_connection_basic() {
    if !live_r2_tests_enabled() {
        eprintln!("skipped (set --features test-live-cloud and LIBRA_STORAGE_*)");
        return;
    }
    let storage = r2_storage_from_env("cloud-backup-test");

    let content = format!("Test content {}", chrono::Utc::now().timestamp());
    let blob = Blob::from_content(&content);

    storage
        .put(&blob.id, &blob.data, blob.get_type())
        .await
        .unwrap();
    assert!(storage.exist(&blob.id).await);

    let (data, obj_type) = storage.get(&blob.id).await.expect("R2 get failed");
    assert_eq!(data, blob.data);
    assert_eq!(obj_type, blob.get_type());
}

/// Scenario: end-to-end cloud backup against live D1 + R2. Two repos with distinct
/// `repo_id`s and `cloud.name`s commit a shared text file (intentionally same
/// content to test object dedup) plus a binary file (only in repo A). After
/// `cloud sync`, both R2 prefixes contain the shared blob (cross-repo dedup is NOT
/// enforced) and the binary is in repo A only. Restore both repos into fresh dirs:
/// repo A by `--repo-id` and repo B by `--name`, confirming both restore mechanisms.
/// The restored repo A's binary file is present in repo A's restore but NOT in repo
/// B's restore — proving repo isolation. Finally `libra config --get libra.repoid`
/// confirms the per-repo config also restored. The test configures a local author
/// identity in each isolated repo so it does not depend on the developer's global
/// `~/.libra/config.db`. Skipped without both D1 and R2 envs.
#[tokio::test]
#[serial(cloud_live)]
async fn cloud_full_workflow_end_to_end() {
    if !live_cloud_tests_enabled() {
        eprintln!("skipped (set --features test-live-cloud plus LIBRA_D1_* and LIBRA_STORAGE_*)");
        return;
    }
    // Setup - Initialize two separate local repos
    let repo_a_dir = init_repo();
    let repo_b_dir = init_repo();
    let repo_a_path = repo_a_dir.path();
    let repo_b_path = repo_b_dir.path();

    // Generate unique repo IDs for isolation test
    let repo_id_a = format!("test-repo-a-{}", Uuid::new_v4());
    let repo_id_b = format!("test-repo-b-{}", Uuid::new_v4());

    let envs = [
        ("LIBRA_D1_ACCOUNT_ID", required_env("LIBRA_D1_ACCOUNT_ID")),
        ("LIBRA_D1_API_TOKEN", required_env("LIBRA_D1_API_TOKEN")),
        ("LIBRA_D1_DATABASE_ID", required_env("LIBRA_D1_DATABASE_ID")),
        (
            "LIBRA_STORAGE_ENDPOINT",
            required_env("LIBRA_STORAGE_ENDPOINT"),
        ),
        ("LIBRA_STORAGE_BUCKET", required_env("LIBRA_STORAGE_BUCKET")),
        (
            "LIBRA_STORAGE_ACCESS_KEY",
            required_env("LIBRA_STORAGE_ACCESS_KEY"),
        ),
        (
            "LIBRA_STORAGE_SECRET_KEY",
            required_env("LIBRA_STORAGE_SECRET_KEY"),
        ),
        ("LIBRA_STORAGE_REGION", "auto".to_string()),
    ];

    // Helper to run libra command
    let run_libra = |dir: &std::path::Path, args: &[&str]| {
        let home = dir.join(".home");
        let config_home = home.join(".config");
        std::fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.current_dir(dir)
            .args(args)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("USERPROFILE", &home);
        for (k, v) in &envs {
            cmd.env(k, v);
        }
        let output = cmd.output().expect("Failed to execute libra");
        if !output.status.success() {
            eprintln!("Command failed: libra {}", args.join(" "));
            eprintln!("Stderr: {}", String::from_utf8_lossy(&output.stderr));
            panic!("Command failed");
        }
        output
    };

    // Configure local commit identities. The test isolates HOME/XDG_CONFIG_HOME per
    // repo, so relying on a developer's global config would make the live cloud gate
    // fail before it reaches the D1/R2 behavior under test.
    for repo in [repo_a_path, repo_b_path] {
        run_libra(repo, &["config", "--local", "user.name", "Libra Test"]);
        run_libra(
            repo,
            &["config", "--local", "user.email", "libra@example.com"],
        );
        run_libra(repo, &["config", "--local", "vault.signing", "false"]);
    }

    // Set repo IDs using local scope
    // libra config expects: libra config --local libra.repoid <value>
    run_libra(
        repo_a_path,
        &["config", "--local", "libra.repoid", &repo_id_a],
    );
    run_libra(
        repo_b_path,
        &["config", "--local", "libra.repoid", &repo_id_b],
    );

    // Set cloud names for testing name-based restore
    let name_a = format!("end-to-end-test-a-{}", Uuid::new_v4());
    let name_b = format!("end-to-end-test-b-{}", Uuid::new_v4());
    run_libra(repo_a_path, &["config", "--local", "cloud.name", &name_a]);
    run_libra(repo_b_path, &["config", "--local", "cloud.name", &name_b]);

    // Create content in Repo A
    let file_a = repo_a_path.join("file_a.txt");
    std::fs::write(&file_a, "Content from Repo A").unwrap();

    // Add a binary file to test non-text content
    let bin_file_a = repo_a_path.join("logo.bin");
    let bin_content = vec![0u8, 15, 255, 10, 42]; // Simple binary signature
    std::fs::write(&bin_file_a, &bin_content).unwrap();

    run_libra(repo_a_path, &["add", "."]);
    run_libra(repo_a_path, &["commit", "-m", "Commit A"]);

    // Create content in Repo B (Same content -> Same Hash, Different Repo)
    let file_b = repo_b_path.join("file_b.txt");
    std::fs::write(&file_b, "Content from Repo A").unwrap(); // Intentionally same content
    run_libra(repo_b_path, &["add", "."]);
    run_libra(repo_b_path, &["commit", "-m", "Commit B (Same Content)"]);

    // Cloud Sync both repos
    run_libra(repo_a_path, &["cloud", "sync"]);
    run_libra(repo_b_path, &["cloud", "sync"]);

    // Verification (Direct D1/R2 check)
    let d1 = d1_client_from_env();
    let r2_a = r2_storage_from_env(&repo_id_a);
    let r2_b = r2_storage_from_env(&repo_id_b);

    // Verify D1 indexes exist for both
    let idx_a = d1.get_object_indexes(&repo_id_a).await.unwrap();
    let idx_b = d1.get_object_indexes(&repo_id_b).await.unwrap();

    assert!(!idx_a.is_empty(), "Repo A should have indexes");
    assert!(!idx_b.is_empty(), "Repo B should have indexes");

    // Verify Object Isolation in R2
    // We expect the blob (same hash) to exist in BOTH prefixes
    use git_internal::internal::object::types::ObjectType;
    let blob_hash = git_internal::hash::ObjectHash::from_type_and_data(
        ObjectType::Blob,
        "Content from Repo A".as_bytes(),
    );
    let bin_hash = git_internal::hash::ObjectHash::from_type_and_data(
        ObjectType::Blob,
        &[0u8, 15, 255, 10, 42],
    );

    let blob_id_from_d1 = blob_hash.to_string();
    let bin_blob_id = bin_hash.to_string();

    // Verify D1 has these objects
    assert!(
        idx_a.iter().any(|idx| idx.o_id == blob_id_from_d1),
        "Repo A should have the text blob in D1"
    );
    assert!(
        idx_a.iter().any(|idx| idx.o_id == bin_blob_id),
        "Repo A should have the binary blob in D1"
    );

    assert_remote_object_available(&r2_a, &blob_hash, "Text blob in Repo A").await;
    assert_remote_object_available(&r2_a, &bin_hash, "Binary blob in Repo A").await;
    assert_remote_object_available(&r2_b, &blob_hash, "Text blob in Repo B").await;

    // Restore Scenarios

    // Restore Repo A using ID (Legacy/Explicit ID method)
    let restore_dir_a = tempdir().unwrap();
    let restore_path_a = restore_dir_a.path();

    // Init empty
    let restore_home_a = restore_path_a.join(".home");
    let restore_config_a = restore_home_a.join(".config");
    std::fs::create_dir_all(&restore_config_a).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.current_dir(restore_path_a)
        .args(["init"])
        .env("HOME", &restore_home_a)
        .env("XDG_CONFIG_HOME", &restore_config_a)
        .env("USERPROFILE", &restore_home_a);
    cmd.output().unwrap();

    // Restore from Cloud using Repo A's ID
    let mut restore_cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    restore_cmd
        .current_dir(restore_path_a)
        .args(["cloud", "restore", "--repo-id", &repo_id_a])
        .env("HOME", &restore_home_a)
        .env("XDG_CONFIG_HOME", &restore_config_a)
        .env("USERPROFILE", &restore_home_a);
    for (k, v) in &envs {
        restore_cmd.env(k, v);
    }
    let out = restore_cmd.output().unwrap();
    assert!(
        out.status.success(),
        "Restore A (by ID) failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Check if objects are in `.libra/objects`
    let objects_path_a = restore_path_a.join(".libra/objects");
    let local_store_a = LocalStorage::new(objects_path_a);
    assert!(
        local_store_a.exist(&blob_hash).await,
        "Restored repo A should have the text blob {}",
        blob_hash
    );
    assert!(
        local_store_a.exist(&bin_hash).await,
        "Restored repo A should have the binary blob {}",
        bin_hash
    );

    // Verify config was restored (repoid)
    // We can check by running `libra config --get libra.repoid`
    let config_out = run_libra(restore_path_a, &["config", "--get", "libra.repoid"]);
    let config_val = String::from_utf8_lossy(&config_out.stdout)
        .trim()
        .to_string();
    assert_eq!(
        config_val, repo_id_a,
        "Restored repo should have correct repo_id in config"
    );

    // Restore Repo B using Name (New method)
    let restore_dir_b = tempdir().unwrap();
    let restore_path_b = restore_dir_b.path();

    // Init empty
    let restore_home_b = restore_path_b.join(".home");
    let restore_config_b = restore_home_b.join(".config");
    std::fs::create_dir_all(&restore_config_b).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.current_dir(restore_path_b)
        .args(["init"])
        .env("HOME", &restore_home_b)
        .env("XDG_CONFIG_HOME", &restore_config_b)
        .env("USERPROFILE", &restore_home_b);
    cmd.output().unwrap();

    // Restore from Cloud using Repo B's Name
    let mut restore_cmd_b = Command::new(env!("CARGO_BIN_EXE_libra"));
    restore_cmd_b
        .current_dir(restore_path_b)
        .args(["cloud", "restore", "--name", &name_b])
        .env("HOME", &restore_home_b)
        .env("XDG_CONFIG_HOME", &restore_config_b)
        .env("USERPROFILE", &restore_home_b);
    for (k, v) in &envs {
        restore_cmd_b.env(k, v);
    }
    let out_b = restore_cmd_b.output().unwrap();
    assert!(
        out_b.status.success(),
        "Restore B (by Name) failed: {}",
        String::from_utf8_lossy(&out_b.stderr)
    );

    // Check if objects are in `.libra/objects`
    let objects_path_b = restore_path_b.join(".libra/objects");
    let local_store_b = LocalStorage::new(objects_path_b);
    assert!(
        local_store_b.exist(&blob_hash).await,
        "Restored repo B should have the blob {}",
        blob_hash
    );

    // Verify binary blob (Repo A only) is NOT present
    assert!(
        !local_store_b.exist(&bin_hash).await,
        "Restored repo B should NOT have the binary blob {}",
        bin_hash
    );

    // Verify config (repoid)
    let config_out_b = run_libra(restore_path_b, &["config", "--get", "libra.repoid"]);
    let config_val_b = String::from_utf8_lossy(&config_out_b.stdout)
        .trim()
        .to_string();
    assert_eq!(
        config_val_b, repo_id_b,
        "Restored repo B should have correct repo_id"
    );
}

/// Scenario: two distinct repos request the same `cloud.name`. The first sync wins
/// and registers the name; the second sync must fail with a message mentioning
/// "already taken by another repository". Pins the cloud-name uniqueness contract
/// — the runtime cannot allow two repos to share a public-facing name. Skipped
/// without both D1 and R2 envs.
#[tokio::test]
#[serial(cloud_live)]
async fn cloud_sync_name_conflict() {
    if !live_cloud_tests_enabled() {
        eprintln!("skipped (set --features test-live-cloud plus LIBRA_D1_* and LIBRA_STORAGE_*)");
        return;
    }
    let repo_a = init_repo();
    let repo_b = init_repo();
    let cloud_name = format!("conflict-test-{}", Uuid::new_v4());

    // Repo A
    run_libra_cmd(
        repo_a.path(),
        &["config", "--local", "cloud.name", &cloud_name],
    );
    let file_a = repo_a.path().join("a.txt");
    std::fs::write(&file_a, "A").unwrap();
    run_libra_cmd(repo_a.path(), &["add", "."]);
    run_libra_cmd(repo_a.path(), &["commit", "-m", "A"]);
    let out_a = run_libra_cmd(repo_a.path(), &["cloud", "sync"]);
    assert!(
        out_a.status.success(),
        "Repo A sync failed: {}",
        String::from_utf8_lossy(&out_a.stderr)
    );

    // Repo B
    run_libra_cmd(
        repo_b.path(),
        &["config", "--local", "cloud.name", &cloud_name],
    );
    let file_b = repo_b.path().join("b.txt");
    std::fs::write(&file_b, "B").unwrap();
    run_libra_cmd(repo_b.path(), &["add", "."]);
    run_libra_cmd(repo_b.path(), &["commit", "-m", "B"]);
    let out_b = run_libra_cmd(repo_b.path(), &["cloud", "sync"]);

    assert!(
        !out_b.status.success(),
        "Repo B sync should fail due to name conflict"
    );
    let stderr = String::from_utf8_lossy(&out_b.stderr);
    assert!(
        stderr.contains("already taken by another repository"),
        "Error message mismatch: {}",
        stderr
    );
}

/// Spawn the real Libra binary with isolated HOME/XDG paths and the full set of
/// cloud env vars wired in. Used by the live-cloud workflow tests so each repo can
/// execute commands with a fresh global config but shared cloud credentials.
/// Panics if any required cloud env var is missing — callers must already have
/// gated on the live-cloud condition before invoking this.
fn run_libra_cmd(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    let home = dir.join(".home");
    let config_home = home.join(".config");
    std::fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.current_dir(dir)
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home);

    let env_vars = [
        "LIBRA_D1_ACCOUNT_ID",
        "LIBRA_D1_API_TOKEN",
        "LIBRA_D1_DATABASE_ID",
        "LIBRA_STORAGE_ENDPOINT",
        "LIBRA_STORAGE_BUCKET",
        "LIBRA_STORAGE_ACCESS_KEY",
        "LIBRA_STORAGE_SECRET_KEY",
    ];

    for var in env_vars {
        let val =
            std::env::var(var).unwrap_or_else(|_| panic!("Missing required env var: {}", var));
        cmd.env(var, val);
    }

    if std::env::var("LIBRA_STORAGE_REGION").map_or(true, |v| v.is_empty()) {
        cmd.env("LIBRA_STORAGE_REGION", "auto");
    } else {
        cmd.env(
            "LIBRA_STORAGE_REGION",
            std::env::var("LIBRA_STORAGE_REGION").unwrap(),
        );
    }

    cmd.output().expect("Failed to execute libra")
}

/// **Layer:** L3 — live S3/R2. Skipped without `--features test-live-cloud` and
/// `LIBRA_STORAGE_*`.
///
/// End-to-end `libra fsck --heal` against a real durable tier: with
/// `LIBRA_STORAGE_*` configured, commits write objects through to the remote, so
/// deleting a local object and running `fsck --heal` must re-fetch it from the
/// durable tier, verify it, restore it locally, and exit 0 (lore.md §0.4). This
/// is the durable-tier-backed complement to the L1 local-only heal tests in
/// `tests/command/fsck_test.rs` and the storage-layer heal unit tests.
#[tokio::test]
#[serial(cloud_live)]
async fn fsck_heal_restores_object_from_durable_tier() {
    if !live_r2_tests_enabled() {
        eprintln!("skipped (set --features test-live-cloud and LIBRA_STORAGE_*)");
        return;
    }

    let repo_dir = tempdir().unwrap();
    let repo = repo_dir.path();
    let home = repo.join(".home");
    std::fs::create_dir_all(home.join(".config")).unwrap();

    // Objects are content-addressed and puts are idempotent; the root commit's
    // hash also varies by timestamp, so concurrent/repeat runs sharing a bucket
    // cannot corrupt each other.
    let storage_type = std::env::var("LIBRA_STORAGE_TYPE").unwrap_or_else(|_| "s3".to_string());
    let region = std::env::var("LIBRA_STORAGE_REGION").unwrap_or_else(|_| "auto".to_string());
    let envs = [
        ("LIBRA_STORAGE_TYPE", storage_type),
        ("LIBRA_STORAGE_BUCKET", required_env("LIBRA_STORAGE_BUCKET")),
        (
            "LIBRA_STORAGE_ENDPOINT",
            required_env("LIBRA_STORAGE_ENDPOINT"),
        ),
        (
            "LIBRA_STORAGE_ACCESS_KEY",
            required_env("LIBRA_STORAGE_ACCESS_KEY"),
        ),
        (
            "LIBRA_STORAGE_SECRET_KEY",
            required_env("LIBRA_STORAGE_SECRET_KEY"),
        ),
        ("LIBRA_STORAGE_REGION", region),
    ];

    let run = |args: &[&str]| -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
        cmd.current_dir(repo)
            .args(args)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("USERPROFILE", &home);
        for (key, value) in &envs {
            cmd.env(key, value);
        }
        cmd.output().expect("failed to execute libra")
    };

    assert!(run(&["init"]).status.success(), "init");
    assert!(
        run(&["config", "--local", "user.name", "Libra Test"])
            .status
            .success(),
        "config name"
    );
    assert!(
        run(&["config", "--local", "user.email", "libra@example.com"])
            .status
            .success(),
        "config email"
    );
    std::fs::write(repo.join("f.txt"), "durable heal\n").unwrap();
    assert!(run(&["add", "f.txt"]).status.success(), "add");
    assert!(
        run(&["commit", "-m", "seed", "--no-verify"])
            .status
            .success(),
        "commit"
    );

    // Note the commit OID so we can assert it is restored later.
    let log = run(&["log", "--pretty=%H"]);
    let stdout = String::from_utf8_lossy(&log.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim().to_string();
    let commit_obj_path = repo
        .join(".libra")
        .join("objects")
        .join(&commit_hash[0..2])
        .join(&commit_hash[2..]);

    // Delete ALL local loose objects (commit + tree + blob) so they remain only
    // in R2. `fsck --heal` must then re-fetch the whole reachable graph across
    // MULTIPLE discovery rounds (healing the commit reveals its tree, which
    // reveals its blob) — exercising the fixed-point heal loop.
    let objects_dir = repo.join(".libra").join("objects");
    for entry in std::fs::read_dir(&objects_dir).expect("read objects dir") {
        let path = entry.expect("dir entry").path();
        let is_loose_dir = path.is_dir()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.len() == 2);
        if is_loose_dir {
            std::fs::remove_dir_all(&path).expect("delete loose object dir");
        }
    }
    assert!(
        !commit_obj_path.exists(),
        "precondition: local objects removed"
    );

    // `fsck --heal` must re-fetch every reachable object from the durable tier
    // and restore them, exiting 0 once the graph is whole again.
    let heal = run(&["--json", "fsck", "--heal"]);
    let json: serde_json::Value =
        serde_json::from_slice(&heal.stdout).expect("fsck --json output should be JSON");
    assert!(
        json["data"]["heal"]["healed"]
            .as_u64()
            .expect("heal.healed")
            >= 2,
        "the commit and at least its tree should be healed across rounds"
    );
    assert_eq!(
        json["data"]["heal"]["unrecoverable"]
            .as_u64()
            .expect("heal.unrecoverable"),
        0,
        "every object is present in the durable tier, so nothing is unrecoverable"
    );
    assert!(
        commit_obj_path.exists(),
        "healed commit restored to the local store"
    );
    assert!(
        heal.status.success(),
        "fsck --heal exits 0 once every object is repaired"
    );
}
