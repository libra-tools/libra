#!/bin/sh
# Require a real Nextest pass count for selected live-cloud tests.
set -u

usage() {
    printf 'usage: %s <expected-count> <command...>\n' "$0" >&2
    printf '       %s --verify-feature-selection\n' "$0" >&2
    printf '       %s --self-test-case <name> | --self-test-feature-selection-case <name>\n' "$0" >&2
    exit 2
}

emit_fixture() {
    case "$1" in
        singular-summary)
            printf 'Summary [0.10s] 1 test run: 1 passed\n'
            ;;
        plural-summary)
            printf 'Summary [0.10s] 2 tests run: 2 passed\n'
            ;;
        slow-summary-accepted)
            printf 'Summary [0.10s] 2 tests run: 2 passed (1 slow)\n'
            ;;
        filtered-summary-accepted)
            printf 'Summary [0.10s] 2 tests run: 2 passed, 3 skipped\n'
            ;;
        test-skip-marker-reject)
            printf 'skipped (set --features test-live-cloud and LIBRA_D1_*)\n'
            printf 'Summary [0.10s] 1 test run: 1 passed\n'
            ;;
        count-mismatch-reject)
            printf 'Summary [0.10s] 1 test run: 1 passed\n'
            ;;
        earlier-valid-final-invalid-reject)
            printf 'Summary [0.01s] 1 test run: 1 passed\n'
            printf 'Summary [0.10s] 1 test run: 0 passed, 1 failed\n'
            ;;
        earlier-invalid-final-valid-accepted)
            printf 'Summary [0.01s] 1 test run: 0 passed, 1 failed\n'
            printf 'Summary [0.10s] 1 test run: 1 passed\n'
            ;;
        pass-count-mismatch-reject)
            printf 'Summary [0.10s] 2 tests run: 1 passed\n'
            ;;
        missing-summary-reject)
            printf 'PASS cloud_live_preflight\n'
            ;;
        case-insensitive-test-skip-reject)
            printf 'SKIPPED (SET --FEATURES TEST-LIVE-CLOUD and LIBRA_D1_*)\n'
            printf 'Summary [0.10s] 1 test run: 1 passed\n'
            ;;
        wrapped-command-nonzero-reject)
            printf 'Summary [0.10s] 1 test run: 1 passed\n'
            return 7
            ;;
        rg-error-reject)
            printf 'Summary [0.10s] 1 test run: 1 passed\n'
            ;;
        benign-cli-skipped-accepted)
            printf 'skipped (already exist)\n'
            printf 'Summary [0.10s] 1 test run: 1 passed\n'
            ;;
        complete-log-path-retained)
            printf 'fixture stdout: first line\n'
            printf 'fixture stderr: second line\n' >&2
            printf 'Summary [0.10s] 1 test run: 1 passed\n'
            ;;
        *) return 2 ;;
    esac
}

run_self_test_case() {
    name=$1
    expected=1
    should_pass=yes
    case "$name" in
        plural-summary|slow-summary-accepted|filtered-summary-accepted)
            expected=2
            ;;
        count-mismatch-reject|pass-count-mismatch-reject)
            expected=2
            should_pass=no
            ;;
        test-skip-marker-reject|case-insensitive-test-skip-reject|missing-summary-reject|earlier-valid-final-invalid-reject|wrapped-command-nonzero-reject|rg-error-reject)
            should_pass=no
            ;;
        singular-summary|benign-cli-skipped-accepted|complete-log-path-retained)
            ;;
        *) usage ;;
    esac

    result_file=$(mktemp "${TMPDIR:-/tmp}/libra-cloud-live-selftest.XXXXXX") || exit 2
    expected_file=$(mktemp "${TMPDIR:-/tmp}/libra-cloud-live-expected.XXXXXX") || exit 2
    fake_bin_dir=
    isolated_log_dir=
    if [ "$name" = rg-error-reject ]; then
        fake_bin_dir=$(mktemp -d "${TMPDIR:-/tmp}/libra-cloud-live-rg.XXXXXX") || exit 2
        printf '#!/bin/sh\nexit 2\n' >"$fake_bin_dir/rg"
        chmod +x "$fake_bin_dir/rg"
    fi
    if [ "$name" = complete-log-path-retained ]; then
        isolated_log_dir=$(mktemp -d "${TMPDIR:-/tmp}/libra-cloud-live-fresh.XXXXXX") || exit 2
    fi

    # Capture exactly the bytes the wrapped command should put in its log.
    if [ "$name" = complete-log-path-retained ]; then
        sh "$0" --emit-fixture "$name" >"$expected_file" 2>&1 || exit 2
    fi
    if [ -n "$fake_bin_dir" ]; then
        PATH="$fake_bin_dir:$PATH" sh "$0" "$expected" sh "$0" --emit-fixture "$name" >"$result_file" 2>&1
        wrapper_status=$?
    elif [ -n "$isolated_log_dir" ]; then
        TMPDIR="$isolated_log_dir" sh "$0" "$expected" sh "$0" --emit-fixture "$name" >"$result_file" 2>&1
        wrapper_status=$?
    else
        sh "$0" "$expected" sh "$0" --emit-fixture "$name" >"$result_file" 2>&1
        wrapper_status=$?
    fi
    log_path=$(sed -n 's/^cloud-live-log=//p' "$result_file" | tail -n 1)
    log_dir=${isolated_log_dir:-${TMPDIR:-/tmp}}
    case "$log_path" in
        "$log_dir"/libra-cloud-live.*) ;;
        *) printf 'FAIL %s: wrapper did not report a retained mktemp log\n' "$name" >&2; return 1 ;;
    esac
    if [ ! -f "$log_path" ]; then
        printf 'FAIL %s: log path is not a file\n' "$name" >&2
        return 1
    fi
    if [ "$should_pass" = yes ] && [ "$wrapper_status" -ne 0 ]; then
        printf 'FAIL %s: wrapper rejected a valid summary (exit %s)\n' "$name" "$wrapper_status" >&2
        return 1
    fi
    if [ "$should_pass" = no ] && [ "$wrapper_status" -eq 0 ]; then
        printf 'FAIL %s: wrapper accepted a forbidden result\n' "$name" >&2
        return 1
    fi
    if [ "$name" = complete-log-path-retained ] && ! cmp -s "$expected_file" "$log_path"; then
        printf 'FAIL %s: retained log differs from complete stdout/stderr fixture\n' "$name" >&2
        return 1
    fi
    if [ "$name" = wrapped-command-nonzero-reject ] && ! rg -q 'wrapped command failed \(exit 7\)' "$result_file"; then
        printf 'FAIL %s: wrapped exit 7 was not observed\n' "$name" >&2
        return 1
    fi
    if [ "$name" = count-mismatch-reject ] && ! rg -q 'final Nextest summary did not report 2 run and 2 passed' "$result_file"; then
        printf 'FAIL %s: count mismatch was not the reason for rejection\n' "$name" >&2
        return 1
    fi
    if [ "$name" = pass-count-mismatch-reject ] && ! rg -q 'final Nextest summary did not report 2 run and 2 passed' "$result_file"; then
        printf 'FAIL %s: pass-count mismatch was not the reason for rejection\n' "$name" >&2
        return 1
    fi
    if [ "$name" = missing-summary-reject ] && ! rg -q 'Nextest Summary is missing' "$result_file"; then
        printf 'FAIL %s: missing Summary was not the reason for rejection\n' "$name" >&2
        return 1
    fi
    if [ "$name" = earlier-valid-final-invalid-reject ] && ! rg -q 'final Nextest summary did not report 1 run and 1 passed' "$result_file"; then
        printf 'FAIL %s: prior valid Summary hid the invalid final result\n' "$name" >&2
        return 1
    fi
    if [ "$name" = earlier-valid-final-invalid-reject ]; then
        complement_file=$(mktemp "${TMPDIR:-/tmp}/libra-cloud-live-complement.XXXXXX") || return 1
        sh "$0" "$expected" sh "$0" --emit-fixture earlier-invalid-final-valid-accepted >"$complement_file" 2>&1
        complement_status=$?
        if [ "$complement_status" -ne 0 ] || ! rg -q 'selected tests passed' "$complement_file"; then
            printf 'FAIL %s: valid final Summary was not accepted (exit %s)\n' "$name" "$complement_status" >&2
            return 1
        fi
        printf 'PASS final-Summary complement wrapper_exit=%s result_log=%s\n' "$complement_status" "$complement_file"
    fi
    if [ "$name" = case-insensitive-test-skip-reject ] && ! rg -q 'selected test reported a live-cloud skip' "$result_file"; then
        printf 'FAIL %s: uppercase skip marker was not the reason for rejection\n' "$name" >&2
        return 1
    fi
    if [ "$name" = rg-error-reject ] && ! rg -q 'skip-marker check failed \(rg exit 2\)' "$result_file"; then
        printf 'FAIL %s: injected rg exit 2 was not observed\n' "$name" >&2
        return 1
    fi
    printf 'PASS %s wrapper_exit=%s log=%s selftest_result=%s\n' "$name" "$wrapper_status" "$log_path" "$result_file"
    rm -f "$expected_file"
    if [ -n "$fake_bin_dir" ]; then
        rm -f "$fake_bin_dir/rg"
        rmdir "$fake_bin_dir"
    fi
}

verify_feature_selection() {
    # Run this before starting the parallel test target. No cloud test executes.
    command -v python3 >/dev/null 2>&1 || {
        printf 'cloud live gate: python3 is required to parse Nextest JSON\n' >&2
        return 1
    }
    isolated_dir=$(mktemp -d "${TMPDIR:-/tmp}/libra-cloud-feature-list.XXXXXX") || return 1
    fake_home="$isolated_dir/home"
    mkdir -p "$fake_home/.config" || return 1
    printf 'cloud-live-feature-selection-log-dir=%s\n' "$isolated_dir"
    original_home=${HOME:-}
    if [ -z "$original_home" ]; then
        printf 'cloud live gate: HOME is required to locate offline Cargo caches\n' >&2
        return 1
    fi
    cargo_home=${CARGO_HOME:-$original_home/.cargo}
    rustup_home=${RUSTUP_HOME:-$original_home/.rustup}
    cargo_target_dir=${CARGO_TARGET_DIR:-$PWD/target}

    env -i PATH="${PATH:-/usr/bin:/bin}" HOME="$fake_home" USERPROFILE="$fake_home" \
        XDG_CONFIG_HOME="$fake_home/.config" CARGO_HOME="$cargo_home" \
        RUSTUP_HOME="$rustup_home" CARGO_TARGET_DIR="$cargo_target_dir" \
        RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}" CARGO_NET_OFFLINE=true \
        cargo nextest list --features test-live-cloud \
        --test cloud_storage_backup_test -E 'test(=cloud_live_preflight)' \
        --message-format json --offline >"$isolated_dir/feature-on.json" 2>"$isolated_dir/feature-on.stderr"
    on_status=$?
    if [ "$on_status" -ne 0 ]; then
        printf 'cloud live gate: feature-on Nextest list failed (exit %s)\n' "$on_status" >&2
        return 1
    fi
    env -i PATH="${PATH:-/usr/bin:/bin}" HOME="$fake_home" USERPROFILE="$fake_home" \
        XDG_CONFIG_HOME="$fake_home/.config" CARGO_HOME="$cargo_home" \
        RUSTUP_HOME="$rustup_home" CARGO_TARGET_DIR="$cargo_target_dir" \
        RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}" CARGO_NET_OFFLINE=true \
        cargo nextest list \
        --test cloud_storage_backup_test -E 'test(=cloud_live_preflight)' \
        --message-format json --offline >"$isolated_dir/feature-off.json" 2>"$isolated_dir/feature-off.stderr"
    off_status=$?
    if [ "$off_status" -ne 0 ]; then
        printf 'cloud live gate: feature-off Nextest list failed (exit %s)\n' "$off_status" >&2
        return 1
    fi

    env -i PATH="${PATH:-/usr/bin:/bin}" python3 - "$isolated_dir/feature-on.json" "$isolated_dir/feature-off.json" <<'PY'
import json
import sys


def inspect(path):
    with open(path, encoding="utf-8") as source:
        result = json.load(source)
    suites = result.get("rust-suites")
    if not isinstance(suites, dict) or len(suites) != 1:
        raise ValueError("expected exactly one cloud_storage_backup_test suite")
    suite = next(iter(suites.values()))
    if suite.get("binary-name") != "cloud_storage_backup_test":
        raise ValueError("unexpected Nextest binary")
    testcases = suite.get("testcases")
    if not isinstance(testcases, dict):
        raise ValueError("Nextest testcases missing")
    selected = []
    for name, case in testcases.items():
        match = case.get("filter-match")
        if not isinstance(match, dict):
            raise ValueError(f"filter result missing for {name}")
        status = match.get("status")
        if status == "matches":
            selected.append(name)
        elif status != "mismatch":
            raise ValueError(f"unknown filter status for {name}: {status}")
    return sorted(selected), testcases


try:
    feature_on, on_cases = inspect(sys.argv[1])
    feature_off, off_cases = inspect(sys.argv[2])
except (OSError, UnicodeError, json.JSONDecodeError, ValueError, AttributeError) as error:
    print(f"cloud live gate: invalid Nextest JSON: {error}", file=sys.stderr)
    sys.exit(1)

print(f"feature-on selected={feature_on}")
print(f"feature-off selected={feature_off}")
if feature_on != ["cloud_live_preflight"] or feature_off or "cloud_live_preflight" in off_cases:
    print("cloud live gate: preflight feature registration mismatch", file=sys.stderr)
    sys.exit(1)
PY
}

run_feature_selection_self_test() {
    selected_case=${1:-all}
    case "$selected_case" in
        all|valid|cargo-nonzero|cargo-nonzero-off|malformed-json|unknown-filter-status|zero-on-selection|unexpected-off-selection) ;;
        *) usage ;;
    esac
    fake_bin=$(mktemp -d "${TMPDIR:-/tmp}/libra-cloud-feature-fake.XXXXXX") || return 1
    cat >"$fake_bin/cargo" <<'SH'
#!/bin/sh
fixture_dir=$(dirname "$0")
mode=$(cat "$fixture_dir/mode")
is_on=no
for argument do
    if [ "$argument" = --features ]; then
        is_on=yes
    fi
done
if [ "$mode" = cargo-on-nonzero ] && [ "$is_on" = yes ]; then
    exit 23
fi
if [ "$mode" = cargo-off-nonzero ] && [ "$is_on" = no ]; then
    exit 23
fi
if [ "$is_on" = yes ]; then
    cat "$fixture_dir/on.json"
else
    cat "$fixture_dir/off.json"
fi
SH
    chmod +x "$fake_bin/cargo"
    good_on='{"rust-suites":{"libra::cloud_storage_backup_test":{"binary-name":"cloud_storage_backup_test","testcases":{"cloud_live_preflight":{"filter-match":{"status":"matches"}}}}}}'
    good_off='{"rust-suites":{"libra::cloud_storage_backup_test":{"binary-name":"cloud_storage_backup_test","testcases":{}}}}'
    bad_status='{"rust-suites":{"libra::cloud_storage_backup_test":{"binary-name":"cloud_storage_backup_test","testcases":{"cloud_live_preflight":{"filter-match":{"status":"unknown"}}}}}}'
    incomplete_off='{"rust-suites":{"libra::cloud_storage_backup_test":{"binary-name":"cloud_storage_backup_test"}}}'
    for mode in valid cargo-on-nonzero cargo-off-nonzero malformed-json schema-incomplete-off unknown-filter-status zero-on-selection unexpected-off-selection; do
        if [ "$selected_case" = cargo-nonzero ]; then
            [ "$mode" = cargo-on-nonzero ] || continue
        elif [ "$selected_case" = cargo-nonzero-off ]; then
            [ "$mode" = cargo-off-nonzero ] || continue
        elif [ "$selected_case" = malformed-json ]; then
            case "$mode" in malformed-json|schema-incomplete-off) ;; *) continue ;; esac
        elif [ "$selected_case" != all ] && [ "$mode" != "$selected_case" ]; then
            continue
        fi
        result_file=$(mktemp "${TMPDIR:-/tmp}/libra-cloud-feature-result.XXXXXX") || return 1
        printf '%s\n' "$mode" >"$fake_bin/mode"
        printf '%s\n' "$good_on" >"$fake_bin/on.json"
        printf '%s\n' "$good_off" >"$fake_bin/off.json"
        case "$mode" in
            malformed-json) printf '{invalid\n' >"$fake_bin/on.json" ;;
            schema-incomplete-off) printf '%s\n' "$incomplete_off" >"$fake_bin/off.json" ;;
            unknown-filter-status) printf '%s\n' "$bad_status" >"$fake_bin/on.json" ;;
            zero-on-selection) printf '%s\n' "$good_off" >"$fake_bin/on.json" ;;
            unexpected-off-selection) printf '%s\n' "$good_on" >"$fake_bin/off.json" ;;
        esac
        PATH="$fake_bin:$PATH" sh "$0" --verify-feature-selection >"$result_file" 2>&1
        status=$?
        if [ "$mode" = valid ]; then
            if [ "$status" -ne 0 ] || ! rg -q "feature-on selected=\['cloud_live_preflight'\]" "$result_file" || ! rg -q 'feature-off selected=\[\]' "$result_file"; then
                printf 'FAIL feature-selection %s (exit %s)\n' "$mode" "$status" >&2
                return 1
            fi
        else
            if [ "$status" -eq 0 ]; then
                printf 'FAIL feature-selection %s: invalid selection was accepted\n' "$mode" >&2
                return 1
            fi
            case "$mode" in
                cargo-on-nonzero) diagnostic='feature-on Nextest list failed \(exit 23\)' ;;
                cargo-off-nonzero) diagnostic='feature-off Nextest list failed \(exit 23\)' ;;
                malformed-json) diagnostic='invalid Nextest JSON:' ;;
                schema-incomplete-off) diagnostic='Nextest testcases missing' ;;
                unknown-filter-status) diagnostic='unknown filter status for cloud_live_preflight: unknown' ;;
                zero-on-selection|unexpected-off-selection) diagnostic='preflight feature registration mismatch' ;;
            esac
            if ! rg -q "$diagnostic" "$result_file"; then
                printf 'FAIL feature-selection %s: wrong rejection reason\n' "$mode" >&2
                return 1
            fi
        fi
        case "$selected_case" in
            cargo-nonzero|cargo-nonzero-off) display_case=$selected_case ;;
            *) display_case=$mode ;;
        esac
        printf 'PASS feature-selection %s wrapper_exit=%s result_log=%s\n' "$display_case" "$status" "$result_file"
        sed -n '/^feature-on selected=/p; /^feature-off selected=/p; /^cloud live gate:/p' "$result_file"
    done
    rm -f "$fake_bin/cargo" "$fake_bin/mode" "$fake_bin/on.json" "$fake_bin/off.json"
    rmdir "$fake_bin"
}

if [ "${1:-}" = --emit-fixture ]; then
    [ "$#" -eq 2 ] || usage
    emit_fixture "$2"
    exit $?
fi
if [ "${1:-}" = --self-test-case ]; then
    [ "$#" -eq 2 ] || usage
    run_self_test_case "$2"
    exit $?
fi
if [ "${1:-}" = --self-test ]; then
    [ "$#" -eq 1 ] || usage
    for name in singular-summary plural-summary slow-summary-accepted filtered-summary-accepted \
        test-skip-marker-reject case-insensitive-test-skip-reject count-mismatch-reject \
        pass-count-mismatch-reject missing-summary-reject earlier-valid-final-invalid-reject \
        wrapped-command-nonzero-reject \
        rg-error-reject benign-cli-skipped-accepted complete-log-path-retained; do
        run_self_test_case "$name" || exit 1
    done
    exit 0
fi
if [ "${1:-}" = --verify-feature-selection ]; then
    [ "$#" -eq 1 ] || usage
    verify_feature_selection
    exit $?
fi
if [ "${1:-}" = --self-test-feature-selection ]; then
    [ "$#" -eq 1 ] || usage
    run_feature_selection_self_test
    exit $?
fi
if [ "${1:-}" = --self-test-feature-selection-case ]; then
    [ "$#" -eq 2 ] || usage
    run_feature_selection_self_test "$2"
    exit $?
fi

[ "$#" -ge 2 ] || usage
expected=$1
shift
case "$expected" in
    ''|*[!0-9]*|0) usage ;;
esac

log=$(mktemp "${TMPDIR:-/tmp}/libra-cloud-live.XXXXXX") || exit 2
printf 'cloud-live-log=%s\n' "$log"
"$@" >"$log" 2>&1
command_status=$?
if [ "$command_status" -ne 0 ]; then
    printf 'cloud live gate: wrapped command failed (exit %s)\n' "$command_status" >&2
    exit 1
fi

if rg -qi 'skipped \(set --features test-live-cloud' "$log"; then
    printf 'cloud live gate: selected test reported a live-cloud skip\n' >&2
    exit 1
else
    rg_status=$?
    if [ "$rg_status" -ne 1 ]; then
        printf 'cloud live gate: skip-marker check failed (rg exit %s)\n' "$rg_status" >&2
        exit 1
    fi
fi

if summary_lines=$(rg '^[[:space:]]*Summary[[:space:]]+\[' "$log"); then
    final_summary=$(printf '%s\n' "$summary_lines" | tail -n 1)
else
    rg_status=$?
    if [ "$rg_status" -ne 1 ]; then
        printf 'cloud live gate: Summary scan failed (rg exit %s)\n' "$rg_status" >&2
    else
        printf 'cloud live gate: Nextest Summary is missing\n' >&2
    fi
    exit 1
fi

summary_pattern="^[[:space:]]*Summary[[:space:]]+\\[[^]]+\\][[:space:]]+${expected}[[:space:]]+tests?[[:space:]]+run:[[:space:]]+${expected}[[:space:]]+passed\\b"
if printf '%s\n' "$final_summary" | rg -q "$summary_pattern"; then
    printf 'cloud live gate: %s/%s selected tests passed\n' "$expected" "$expected"
    exit 0
else
    rg_status=$?
    if [ "$rg_status" -ne 1 ]; then
        printf 'cloud live gate: summary check failed (rg exit %s)\n' "$rg_status" >&2
    else
        printf 'cloud live gate: final Nextest summary did not report %s run and %s passed\n' "$expected" "$expected" >&2
    fi
    exit 1
fi
