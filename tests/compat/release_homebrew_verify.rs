//! Exercise the real Homebrew verification shell offline, including failure paths
//! and the release-bound positive artifact. No actual installation or network I/O.

const WORKFLOW: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/.github/workflows/release.yml"
));

fn verification_job() -> &'static str {
    let anchor = "  verify-homebrew-formula:\n";
    assert_eq!(WORKFLOW.matches(anchor).count(), 1);
    WORKFLOW
        .split_once(anchor)
        .expect("verification job")
        .1
        .split_once("\n  request-stable-manifest:")
        .expect("next job")
        .0
}

fn verification_script() -> String {
    let job = verification_job();
    let anchor = "      - name: Install and verify Homebrew formula\n";
    assert_eq!(job.matches(anchor).count(), 1);
    let run = job
        .split_once(anchor)
        .expect("verification step")
        .1
        .split_once("        run: |\n")
        .expect("literal Bash body")
        .1;
    let mut script = String::new();
    for line in run.lines() {
        if let Some(body) = line.strip_prefix("          ") {
            script.push_str(body);
            script.push('\n');
        } else if line.is_empty() {
            script.push('\n');
        } else {
            break;
        }
    }
    assert!(!script.trim().is_empty());
    script
}

#[test]
fn homebrew_verification_requires_same_run_formula_and_success_artifact() {
    let job = verification_job();
    assert!(job.contains("timeout-minutes: 15"));
    assert!(!job.contains("continue-on-error:"));
    assert!(job.contains("name: formula-commit-sha\n"));
    assert!(
        !job.contains("run-id:"),
        "formula artifact must come from this run"
    );
    let upload = job
        .split_once("      - name: Upload successful Homebrew verification\n")
        .expect("success artifact step")
        .1;
    assert!(upload.contains("if: success()"));
    assert!(upload.contains("name: homebrew-verify-pass\n"));
    assert!(upload.contains("path: homebrew-verify-pass.txt\n"));
    assert!(upload.contains("if-no-files-found: error"));
    assert!(verification_script().contains("homebrew-verify-pass.txt"));
}

#[cfg(unix)]
mod shell_tests {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    use super::verification_script;

    const FORMULA_SHA: &str = "1111111111111111111111111111111111111111";

    fn executable(path: &std::path::Path, body: &str) {
        fs::write(path, body).expect("write fixture executable");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("fixture executable permission");
    }

    fn run_scenario(
        failure: &str,
        artifact: Option<&str>,
    ) -> (std::process::Output, Option<String>, String) {
        let temp = tempfile::tempdir().expect("isolated shell fixture");
        let dir = temp.path();
        let bin = dir.join("command stubs");
        let prefix = dir.join("installed formula");
        let tap = dir.join("release tap");
        fs::create_dir_all(&bin).expect("stub directory");
        fs::create_dir_all(prefix.join("bin")).expect("formula prefix");
        fs::create_dir_all(&tap).expect("tap directory");
        if let Some(artifact) = artifact {
            fs::write(dir.join("formula-commit-sha.txt"), artifact).expect("formula artifact");
        }
        executable(
            &bin.join("brew"),
            r#"#!/bin/bash
set -euo pipefail
[[ "${HOMEBREW_NO_AUTO_UPDATE:-}" == 1 && "${HOMEBREW_NO_INSTALL_UPGRADE:-}" == 1 ]]
echo "brew $*" >> "$CALL_LOG"
case "$1" in
  tap) [[ "$FAIL_STEP" != tap && "$2" == libra-tools/libra ]] ;;
  --repo) [[ "$FAIL_STEP" != tap-path ]]; [[ "$FAIL_STEP" == empty-tap-path ]] || printf '%s\n' "$TAP_DIR" ;;
  install) [[ "$FAIL_STEP" != install && "$2" == libra-tools/libra/libra ]] ;;
  --prefix) [[ "$FAIL_STEP" != prefix && "$2" == --installed && "$3" == libra-tools/libra/libra ]]; [[ "$FAIL_STEP" == empty-prefix ]] || printf '%s\n' "$FORMULA_PREFIX" ;;
  *) exit 99 ;;
esac
"#,
        );
        executable(
            &bin.join("git"),
            r#"#!/bin/bash
set -euo pipefail
echo "git $*" >> "$CALL_LOG"
[[ "$1" == -C && "$2" == "$TAP_DIR" ]]
case "$3" in
  fetch) [[ "$FAIL_STEP" != fetch && "$4" == --depth=1 && "$5" == origin && "$6" == "$FORMULA_SHA" ]] ;;
  checkout) [[ "$FAIL_STEP" != checkout && "$4" == --detach && "$5" == "$FORMULA_SHA" ]] ;;
  rev-parse)
    [[ "$FAIL_STEP" != rev-parse && "$4" == HEAD ]]
    if [[ "$FAIL_STEP" == wrong-commit ]]; then echo 2222222222222222222222222222222222222222; else echo "$FORMULA_SHA"; fi ;;
  *) exit 99 ;;
esac
"#,
        );
        executable(
            &prefix.join("bin/libra"),
            r#"#!/bin/bash
set -euo pipefail
echo "installed-libra $*" >> "$CALL_LOG"
[[ "$1" == --version && "$FAIL_STEP" != version ]]
if [[ "$FAIL_STEP" == wrong-version ]]; then echo 'libra 0.0.0'; else echo 'libra 9.8.7'; fi
"#,
        );
        // A PATH-based check must not accidentally pass using another installed binary.
        executable(&bin.join("libra"), "#!/bin/bash\nexit 97\n");
        let script = dir.join("verify.sh");
        fs::write(&script, verification_script()).expect("actual workflow script");
        let output = Command::new("bash")
            .arg(&script)
            .current_dir(dir)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("GITHUB_WORKSPACE", dir)
            .env("GITHUB_STEP_SUMMARY", dir.join("summary.txt"))
            .env("GITHUB_REF_NAME", "v9.8.7")
            .env("GITHUB_SHA", "3333333333333333333333333333333333333333")
            .env("GITHUB_RUN_ID", "123456")
            .env("FAIL_STEP", failure)
            .env("FORMULA_SHA", FORMULA_SHA)
            .env("TAP_DIR", &tap)
            .env("FORMULA_PREFIX", &prefix)
            .env("CALL_LOG", dir.join("calls.log"))
            .output()
            .expect("run offline Bash verification");
        let sentinel = match fs::read_to_string(dir.join("homebrew-verify-pass.txt")) {
            Ok(value) => Some(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("read verification sentinel: {error}"),
        };
        let calls = match fs::read_to_string(dir.join("calls.log")) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => panic!("read fixture command log: {error}"),
        };
        (output, sentinel, calls)
    }

    #[test]
    fn homebrew_verification_rejects_command_and_identity_failures() {
        for failure in [
            "tap",
            "tap-path",
            "empty-tap-path",
            "fetch",
            "checkout",
            "rev-parse",
            "wrong-commit",
            "install",
            "prefix",
            "empty-prefix",
            "version",
            "wrong-version",
        ] {
            let (output, sentinel, _) = run_scenario(failure, Some(FORMULA_SHA));
            assert!(!output.status.success(), "{failure} unexpectedly succeeded");
            assert!(
                sentinel.is_none(),
                "{failure} must not produce success evidence"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("::error::"),
                "{failure} must explain its failure"
            );
        }
    }

    #[test]
    fn homebrew_verification_rejects_missing_or_invalid_formula_artifact() {
        for artifact in [
            None,
            Some(""),
            Some("not-a-commit"),
            Some("1111111111111111111111111111111111111111\n2222"),
        ] {
            let (output, sentinel, calls) = run_scenario("", artifact);
            assert!(!output.status.success());
            assert!(sentinel.is_none());
            assert!(
                calls.is_empty(),
                "invalid input must fail before brew or git"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("::error::"),
                "invalid artifact must explain its failure"
            );
        }
    }

    #[test]
    fn homebrew_verification_success_binds_installed_binary_and_formula_commit() {
        let (output, sentinel, calls) = run_scenario("", Some(FORMULA_SHA));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            sentinel.as_deref(),
            Some(
                "version=v9.8.7\nformula_commit=1111111111111111111111111111111111111111\nrelease_commit=3333333333333333333333333333333333333333\nrun_id=123456\n"
            )
        );
        assert!(calls.contains("installed-libra --version"));
        assert!(
            calls.find("checkout --detach").expect("pinned checkout")
                < calls.find("brew install").expect("install")
        );
    }
}
