//! Wave 11 / PR 11 — `code_ui_remote_model_generation` matrix
//! runner (§5.19, L3, nightly-only).
//!
//! Loads `tests/data/code_ui_remote/model_generation_cases.json`
//! and runs the live-model-generation cases through a real
//! non-TTY `libra code` Web process. The matrix harness supports the
//! `provider.mode == "model_from_env_file"` shape; this runner
//! reads the configured env file (default `.env.test`), resolves
//! `LIBRA_CODE_TEST_PROVIDER` + `LIBRA_CODE_TEST_MODEL`, and
//! spawns `libra code --provider <provider> --model <model>
//! --env-file <path>` so the live API key is loaded from disk.
//!
//! Default behaviour: every case is `#[ignore]` so a normal
//! `cargo test` skips the suite. Run on demand with:
//!
//! ```bash
//! LIBRA_RUN_LIVE=1 cargo test --features test-provider \
//!   --test code_ui_remote_model_generation_matrix \
//!   -- --ignored --test-threads=1
//! ```
//!
//! The runner short-circuits with a clear, actionable error
//! before invoking the matrix when:
//!   * `LIBRA_RUN_LIVE` is unset (so an accidental `--ignored`
//!     run without explicit opt-in fails loud rather than firing
//!     against the model API), or
//!   * the env file is missing / lacks the required keys
//!     (build_session_options panics with the case name).
//!
//! Out-of-scope for this runner (still tracked in the §5.19 doc
//! note): the linked-cargo-cli case carries assertions like
//! `cargo fmt --all --check` + `cargo clippy --all-targets
//! --all-features -- -D warnings` that need a workspace-aware
//! `runRepoCommand` fan-out the JSON case file does not yet
//! describe; the runner accepts both cases but the second one
//! will only validate what `runRepoCommand` already enforces
//! today.

#[cfg(feature = "test-provider")]
mod harness;

#[cfg(feature = "test-provider")]
use anyhow::{Result, bail};
#[cfg(feature = "test-provider")]
use harness::CodeSession;
#[cfg(feature = "test-provider")]
use harness::matrix::{Case, CaseFile, build_session_options, find_case, load_case_file};
#[cfg(feature = "test-provider")]
use serial_test::serial;

#[cfg(feature = "test-provider")]
const CASE_FILE_PATH: &str = "tests/data/code_ui_remote/model_generation_cases.json";

#[cfg(feature = "test-provider")]
fn live_mode_enabled() -> bool {
    std::env::var("LIBRA_RUN_LIVE")
        .ok()
        .as_deref()
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false)
}

#[cfg(feature = "test-provider")]
fn run_model_generation_case(case_name: &str) -> Result<()> {
    if !live_mode_enabled() {
        bail!(
            "LIBRA_RUN_LIVE=1 must be set to run live model-generation case '{case_name}'; rerun with `LIBRA_RUN_LIVE=1 cargo test --features test-provider --test code_ui_remote_model_generation_matrix -- --ignored --test-threads=1`",
        );
    }
    let file_path = harness::matrix::data_path(CASE_FILE_PATH);
    let file: CaseFile = load_case_file(&file_path)?;
    let case: Case = find_case(&file, case_name)?;
    let options = build_session_options(&file, &case);
    let mut session = CodeSession::spawn(options)?;
    let outcome = harness::matrix::run_case(&mut session, &case);
    let shutdown = session.shutdown();
    outcome?;
    shutdown
}

#[cfg(feature = "test-provider")]
macro_rules! model_generation_case {
    ($name:ident) => {
        #[test]
        #[ignore = "L3 live model; run with LIBRA_RUN_LIVE=1 + .env.test (DEEPSEEK_API_KEY)"]
        #[serial]
        fn $name() -> Result<()> {
            run_model_generation_case(stringify!($name))
        }
    };
}

// Wave 11 — both P0 cases from `model_generation_cases.json`. The
// linked-cargo-cli case will pass through the harness but its
// quality-gate assertions (`cargo fmt`, `cargo clippy`,
// `cargo test`) are bounded by what `runRepoCommand` already
// supports; deeper workflow validation lands in a follow-up.
#[cfg(feature = "test-provider")]
model_generation_case!(model_generation_code_service_creates_tested_rust_file);
#[cfg(feature = "test-provider")]
model_generation_case!(model_generation_linked_cargo_cli_project_passes_quality_gates);

/// Wave 11 Codex pass-3 regression — pin the DeepSeek-flag
/// injection in `build_session_options`. The pass-2 fix appends
/// `--deepseek-thinking enabled --deepseek-reasoning-effort high`
/// when the env-resolved provider is `deepseek` (case-
/// insensitive). Without this assertion a future refactor could
/// silently drop the flags and the live `LIBRA_RUN_LIVE` matrix
/// would exercise the wrong code path — the §5.19 closure
/// criterion explicitly requires those flags.
///
/// Drives `build_session_options` with a synthetic env file in a
/// tempdir so the test runs in the regular non-live `cargo test`
/// invocation; no DeepSeek API call is made.
#[cfg(feature = "test-provider")]
#[test]
fn build_session_options_for_deepseek_provider_appends_thinking_flags() {
    use harness::matrix::{Case, CaseFile, ProviderSpec, build_session_options};

    let env_dir = tempfile::Builder::new()
        .prefix("model-gen-deepseek-flag-")
        .tempdir()
        .expect("tempdir for env file");
    let env_path = env_dir.path().join(".env.test");
    std::fs::write(
        &env_path,
        "LIBRA_CODE_TEST_PROVIDER=deepseek\nLIBRA_CODE_TEST_MODEL=deepseek-v4-flash\n",
    )
    .expect("write env file");

    let file = CaseFile {
        schema_version: 1,
        matrix: "test-deepseek-flags".to_string(),
        defaults: harness::matrix::Defaults {
            fixture: harness::matrix::FixtureRef {
                path: "tests/fixtures/code_ui/basic_chat.json".to_string(),
            },
            provider: Some(ProviderSpec::ModelFromEnvFile {
                env_file: env_path.display().to_string(),
                provider_env: "LIBRA_CODE_TEST_PROVIDER".to_string(),
                model_env: "LIBRA_CODE_TEST_MODEL".to_string(),
                required: true,
            }),
            options: harness::matrix::CaseOptions::default(),
        },
        cases: Vec::new(),
    };
    let case = Case {
        name: "deepseek-flag-injection".to_string(),
        priority: "P0".to_string(),
        fixture: None,
        provider: None,
        options: harness::matrix::CaseOptions::default(),
        steps: Vec::new(),
    };
    let options = build_session_options(&file, &case);
    assert_eq!(options.provider_override.as_deref(), Some("deepseek"));
    assert_eq!(options.model_override.as_deref(), Some("deepseek-v4-flash"));
    assert_eq!(
        options.extra_cli_args,
        vec![
            "--deepseek-thinking".to_string(),
            "enabled".to_string(),
            "--deepseek-reasoning-effort".to_string(),
            "high".to_string(),
        ],
        "DeepSeek live invocation must carry the §5.19 thinking + high-reasoning flags",
    );
}

/// Wave 11 Codex pass-3 regression — companion to the above:
/// providers OTHER than deepseek must NOT receive the
/// DeepSeek-specific flags. Otherwise a future provider would
/// inherit DeepSeek args meant for a different runtime.
#[cfg(feature = "test-provider")]
#[test]
fn build_session_options_for_non_deepseek_provider_omits_deepseek_flags() {
    use harness::matrix::{Case, CaseFile, ProviderSpec, build_session_options};

    let env_dir = tempfile::Builder::new()
        .prefix("model-gen-non-deepseek-flag-")
        .tempdir()
        .expect("tempdir for env file");
    let env_path = env_dir.path().join(".env.test");
    std::fs::write(
        &env_path,
        "LIBRA_CODE_TEST_PROVIDER=openai\nLIBRA_CODE_TEST_MODEL=gpt-4o-mini\n",
    )
    .expect("write env file");

    let file = CaseFile {
        schema_version: 1,
        matrix: "test-non-deepseek".to_string(),
        defaults: harness::matrix::Defaults {
            fixture: harness::matrix::FixtureRef {
                path: "tests/fixtures/code_ui/basic_chat.json".to_string(),
            },
            provider: Some(ProviderSpec::ModelFromEnvFile {
                env_file: env_path.display().to_string(),
                provider_env: "LIBRA_CODE_TEST_PROVIDER".to_string(),
                model_env: "LIBRA_CODE_TEST_MODEL".to_string(),
                required: true,
            }),
            options: harness::matrix::CaseOptions::default(),
        },
        cases: Vec::new(),
    };
    let case = Case {
        name: "non-deepseek-flag-omission".to_string(),
        priority: "P0".to_string(),
        fixture: None,
        provider: None,
        options: harness::matrix::CaseOptions::default(),
        steps: Vec::new(),
    };
    let options = build_session_options(&file, &case);
    assert_eq!(options.provider_override.as_deref(), Some("openai"));
    assert!(
        options.extra_cli_args.is_empty(),
        "non-DeepSeek provider must NOT inherit DeepSeek-specific flags; got {:?}",
        options.extra_cli_args,
    );
}

#[cfg(not(feature = "test-provider"))]
#[test]
fn model_generation_matrix_requires_test_provider_feature() {
    eprintln!("skipping model generation matrix; enable --features test-provider");
}
