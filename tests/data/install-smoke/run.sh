#!/bin/sh
# install.sh smoke harness — twenty-four scenarios (plan-20260821 A1-05).
#
#   bash tests/data/install-smoke/run.sh
#
# Contract (ADR-UP01-03/06): the harness copies the production installer to a
# temp dir and rewrites ONLY the clearly-marked trust/origin constants in the
# COPY (test public key, local fixture server); the production file must stay
# byte-identical and expose zero runtime key-override entry points. Each
# scenario asserts the exit code AND whether a binary landed on disk.
#
# Requires: python3 (fixture HTTP server + marker rewrite), openssl >= 1.1.1
# with Ed25519 (the verifier under test), sha256sum/shasum.

set -eu

SMOKE_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SMOKE_DIR/../../.." && pwd)
INSTALLER="$REPO_ROOT/install.sh"
FIXTURES="$SMOKE_DIR/fixtures"

WORK=$(mktemp -d)
SERVER_PID=""
cleanup() {
    [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
    rm -rf "$WORK"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $1" >&2
    [ -f "$WORK/out.log" ] && sed 's/^/  | /' "$WORK/out.log" >&2
    exit 1
}

# ── global production-file assertions ───────────────────────────────────────

cp "$INSTALLER" "$WORK/install.sh.orig"

# Exactly one fixed-literal definition of each trust constant; no environment
# indirection anywhere near them (zero runtime key-override entry points).
grep -c '^LIBRA_RELEASE_MANIFEST_PUBLIC_KEY_HEX="[0-9a-f]\{64\}"$' "$INSTALLER" \
    | grep -qx '1' || fail "public-key hex constant is not a single fixed literal"
grep -q 'LIBRA_INSTALL_PUBLIC_KEY' "$INSTALLER" \
    && fail "found a runtime public-key override entry point" || true
grep -q 'LIBRA_RELEASE_MANIFEST_PUBLIC_KEY[^=]*:-' "$INSTALLER" \
    && fail "trust constants must not read the environment" || true
grep -q 'LIBRA_RELEASE_MANIFEST_ORIGIN[^=]*:-' "$INSTALLER" \
    && fail "pinned origin must not read the environment" || true
# BSD grep (macOS) rejects {n,m} repetition counts above 255; any such bound
# in either installer's regexes would fail closed on every Mac.
grep -oE '\{[0-9]+,[0-9]+\}' "$INSTALLER" "$REPO_ROOT/install.ps1" | awk -F'[{,}]' '$3 > 255 {print; exit 1}' \
    || fail "found a regex repetition bound above BSD grep's 255 ceiling"

# ── fixture server ──────────────────────────────────────────────────────────

DOCROOT="$WORK/docroot"
mkdir -p "$DOCROOT"
cp -R "$FIXTURES/tree/." "$DOCROOT/"

python3 - "$DOCROOT" "$WORK/port" > "$WORK/server.log" 2>&1 <<'EOF' &
import http.server, socketserver, sys, os
os.chdir(sys.argv[1])
handler = http.server.SimpleHTTPRequestHandler
handler.log_message = lambda *a, **k: None
socketserver.TCPServer.allow_reuse_address = True
httpd = socketserver.TCPServer(("127.0.0.1", 0), handler)
with open(sys.argv[2], "w") as f:
    f.write(str(httpd.server_address[1]))
httpd.serve_forever()
EOF
SERVER_PID=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -s "$WORK/port" ] && break
    sleep 0.3
done
[ -s "$WORK/port" ] || fail "fixture server did not report a port"
PORT=$(cat "$WORK/port")
BASE="http://127.0.0.1:$PORT"
curl -fsS -o /dev/null "$BASE/libra/releases/v9.9.9/libra-linux-amd64" \
    || fail "fixture server did not come up on $BASE"

# ── prepared installer copy ─────────────────────────────────────────────────

python3 - "$INSTALLER" "$WORK/install-copy.sh" "$BASE" <<'EOF'
import re, sys
src_path, dst_path, base = sys.argv[1:4]
src = open(src_path).read()

PROD_PEM = """-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAaKoA6pNY1FVkUBDYEdQHArP2fOxL3/UtPU+4EHr67tM=
-----END PUBLIC KEY-----"""
TEST_PEM = """-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAqKAN7RPdr6rVJfq93BPvxxeynr7VDNbWUxlgV/qPikM=
-----END PUBLIC KEY-----"""

def replace(pattern, repl, count_expected=1, regex=False):
    global src
    if regex:
        src, n = re.subn(pattern, repl, src)
    else:
        n = src.count(pattern)
        src = src.replace(pattern, repl)
    if n != count_expected:
        raise SystemExit(f"marker drift: {pattern!r} matched {n} times, expected {count_expected}")

replace(PROD_PEM, TEST_PEM)
replace('LIBRA_RELEASE_MANIFEST_KEY_ID="libra-release-1"',
        'LIBRA_RELEASE_MANIFEST_KEY_ID="libra-release-test-1"')
replace('LIBRA_RELEASE_MANIFEST_PUBLIC_KEY_HEX="68aa00ea9358d455645010d811d40702b3f67cec4bdff52d3d4fb8107afaeed3"',
        'LIBRA_RELEASE_MANIFEST_PUBLIC_KEY_HEX="a8a00ded13ddafaad525fabddc13efc717b29ebed50cd6d653196057fa8f8a43"')
replace('LIBRA_RELEASE_MANIFEST_ORIGIN="https://download.libra.tools"',
        f'LIBRA_RELEASE_MANIFEST_ORIGIN="{base}"')
# The test keypair's validity window (fixtures are signed inside it).
replace('LIBRA_RELEASE_MANIFEST_KEY_NOT_BEFORE="2026-08-31T11:09:55Z"',
        'LIBRA_RELEASE_MANIFEST_KEY_NOT_BEFORE="2026-01-01T00:00:00Z"')
replace('LIBRA_RELEASE_MANIFEST_KEY_NOT_AFTER="2027-08-31T00:00:00Z"',
        'LIBRA_RELEASE_MANIFEST_KEY_NOT_AFTER="2028-01-01T00:00:00Z"')
replace('BASE_URL="${LIBRA_BASE_URL:-https://download.libra.tools/libra/releases}"',
        'BASE_URL="${LIBRA_BASE_URL:-' + base + '/libra/releases}"')
replace(r'DEFAULT_VERSION="v[0-9][0-9.]*"', 'DEFAULT_VERSION="v9.9.8"', regex=True)
replace('api_url="https://api.github.com/repos/libra-tools/libra/releases/latest"',
        f'api_url="{base}/repos/libra-tools/libra/releases/latest"')
# NOTE: the signed-URL origin pin and the `${STABLE_URL#...}` fetch rewrite
# stay untouched: signed URLs are production-form by contract, and the copy
# re-bases only the FETCH through the rewritten origin constant.

open(dst_path, "w").write(src)
EOF
chmod +x "$WORK/install-copy.sh"

# A stub PATH dir whose openssl always fails: the verifier-unavailable state.
mkdir -p "$WORK/no-openssl"
printf '#!/bin/sh\nexit 1\n' > "$WORK/no-openssl/openssl"
chmod +x "$WORK/no-openssl/openssl"

# ── scenario runner ─────────────────────────────────────────────────────────

SCENARIOS_RUN=0

# run_scenario <name> <manifest|-none-> <expect: ok|fail> <expect-installed: yes|no> \
#              <required-output-substring> [extra env assignments...]
run_scenario() {
    name=$1; manifest=$2; expect=$3; expect_installed=$4; needle=$5
    shift 5

    rm -f "$DOCROOT/libra/releases/stable/manifest-v1.json"
    if [ "$manifest" != "-none-" ]; then
        mkdir -p "$DOCROOT/libra/releases/stable"
        cp "$FIXTURES/$manifest" "$DOCROOT/libra/releases/stable/manifest-v1.json"
    fi

    home="$WORK/home-$name"
    mkdir -p "$home"
    rc=0
    env -i PATH="${SMOKE_PATH:-$PATH}" HOME="$home" TMPDIR="$WORK" \
        LIBRA_NO_TUI=1 NO_COLOR=1 LIBRA_HOME="$home/.libra" "$@" \
        sh "$WORK/install-copy.sh" > "$WORK/out.log" 2>&1 < /dev/null || rc=$?

    if [ "$expect" = "ok" ] && [ "$rc" -ne 0 ]; then
        fail "$name: expected success, exited $rc"
    fi
    if [ "$expect" = "fail" ] && [ "$rc" -eq 0 ]; then
        fail "$name: expected failure, exited 0"
    fi
    if [ "$expect_installed" = "yes" ] && [ ! -x "$home/.libra/bin/libra" ]; then
        fail "$name: expected an installed binary at \$LIBRA_HOME/bin/libra"
    fi
    if [ "$expect_installed" = "no" ] && [ -e "$home/.libra/bin/libra" ]; then
        fail "$name: a binary was installed on a fail-closed path"
    fi
    grep -qi "$needle" "$WORK/out.log" \
        || fail "$name: output does not mention '$needle'"
    SCENARIOS_RUN=$((SCENARIOS_RUN + 1))
    echo "ok: $name"
}

# Host platform (must be in the release matrix for the harness to run).
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)            HOST_PLATFORM=linux-amd64 ;;
    Linux-aarch64|Linux-arm64) HOST_PLATFORM=linux-arm64 ;;
    Darwin-arm64)            HOST_PLATFORM=darwin-arm64 ;;
    *) fail "unsupported harness platform $(uname -s)-$(uname -m)" ;;
esac

# 1. Valid signed manifest → verified install.
run_scenario valid manifest-valid.json ok yes "stable manifest verified"
cmp -s "$WORK/home-valid/.libra/bin/libra" \
    "$FIXTURES/tree/libra/releases/v9.9.9/libra-$HOST_PLATFORM" \
    || fail "valid: installed binary differs from the signed artifact"
# The verified install records signed provenance for `libra upgrade`.
marker="$WORK/home-valid/.libra/bin/.libra-official-install.json"
[ -f "$marker" ] || fail "valid: official-install marker missing"
grep -q '"install_source":"official_signed_manifest"' "$marker" \
    || fail "valid: marker lacks the official install_source"
grep -q '"version":"9.9.9"' "$marker" || fail "valid: marker version wrong"
grep -q "\"sha256\":\"$(sha256sum "$FIXTURES/tree/libra/releases/v9.9.9/libra-$HOST_PLATFORM" | awk '{print $1}')\"" "$marker" \
    || fail "valid: marker sha256 does not match the artifact"

# 2. Tampered signature → fail closed, nothing on disk.
run_scenario bad-signature manifest-bad-signature.json fail no "SIGNATURE VERIFICATION FAILED"

# 3. Signed sha256 does not match the artifact → fail closed.
run_scenario sha-mismatch manifest-sha-mismatch.json fail no "sha256 mismatch against the SIGNED manifest"

# 4. Signed size does not match the artifact → fail closed.
run_scenario size-mismatch manifest-size-mismatch.json fail no "size mismatch"

# 5. Expired signed manifest → fail closed (no fallback offer).
run_scenario expired manifest-expired.json fail no "is expired"

# 6. paused=true emergency brake → fail closed.
run_scenario paused manifest-paused.json fail no "PAUSED"

# 7. Signed version present in its own revoked_versions → fail closed.
run_scenario revoked manifest-revoked.json fail no "REVOKED"

# 8. Signed + unexpired but older than the installer's pinned baseline →
#    the stateless anti-replay floor refuses it.
run_scenario stale-replay manifest-stale-replay.json fail no "older than this installer's baseline"

# 9. Payload swapped after signing (signature is over the ORIGINAL bytes) →
#    verification fails before any policy field is trusted.
run_scenario tampered-payload manifest-tampered-payload.json fail no "SIGNATURE VERIFICATION FAILED"

# 10. Signed artifact row with size 0 → refused at parse time (before any
#     download; the native contract bounds size to (0, 128 MiB]).
run_scenario zero-size manifest-zero-size.json fail no "outside (0, 128 MiB]"

# 11. min_key_generation above the installer's pinned key generation.
run_scenario future-min-key manifest-future-min-key.json fail no "min_key_generation"

# 12. Signed lifetime beyond the pinned key's validity window.
run_scenario key-window manifest-key-window.json fail no "validity window"

# 13. Properly signed but not the canonical top-level serialization.
run_scenario noncanonical manifest-noncanonical.json fail no "canonical serialization"

# 14. Impossible calendar date (2026-09-31) → refused despite valid ranges.
run_scenario bad-calendar manifest-bad-calendar.json fail no "2026-09-31"

# 15. min_key_generation wider than the bounded numeric grammar → the
#     structural gate refuses before any shell integer comparison.
run_scenario huge-min-key manifest-huge-min-key.json fail no "canonical serialization"

# 16. Artifact-shaped object TRAILING the artifacts array → end anchor refuses.
run_scenario trailing-artifact manifest-trailing-artifact.json fail no "canonical serialization"

# 17. Pretty-printed ENVELOPE around the canonical compact payload → verifies
#     and installs identically (envelope spelling is not signature-bound).
run_scenario pretty-envelope manifest-pretty-envelope.json ok yes "stable manifest verified"

# 18. Served artifact one byte larger than the signed size → the bounded
#     download refuses instead of accepting extra bytes.
run_scenario undersized manifest-undersized.json fail no "downloaded file is empty"

# 19. SIGNED multi-line payload (canonical first line + trailing artifact
#     row on line two) → the printable-ASCII single-line gate refuses.
run_scenario multiline-payload manifest-multiline-payload.json fail no "canonical serialization"

# 20. SemVer component wider than the bounded nine-digit grammar.
run_scenario huge-semver manifest-huge-semver.json fail no "not canonical X.Y.Z"

# 21. Manifest 404 (chain not enabled) without opt-in → explicit stop.
run_scenario transition-404 -none- fail no "signature chain is not enabled yet"

# 22. Manifest 404 + LIBRA_ALLOW_FALLBACK=1 → explicit UNVERIFIED legacy install.
run_scenario transition-404-fallback -none- ok yes "proceeding UNVERIFIED" \
    LIBRA_ALLOW_FALLBACK=1
cmp -s "$WORK/home-transition-404-fallback/.libra/bin/libra" \
    "$FIXTURES/tree/libra/releases/v9.9.8/libra-$HOST_PLATFORM" \
    || fail "transition-404-fallback: legacy binary content mismatch"
[ ! -e "$WORK/home-transition-404-fallback/.libra/bin/.libra-official-install.json" ] \
    || fail "transition-404-fallback: an UNVERIFIED install must not carry the official marker"

# 23. Verifier unavailable without opt-in → explicit stop (third state).
SMOKE_PATH="$WORK/no-openssl:$PATH"
run_scenario verifier-unavailable manifest-valid.json fail no "signature verifier unavailable"

# 24. Verifier unavailable + LIBRA_ALLOW_FALLBACK=1 → explicit UNVERIFIED install.
run_scenario verifier-unavailable-fallback manifest-valid.json ok yes "proceeding UNVERIFIED" \
    LIBRA_ALLOW_FALLBACK=1
SMOKE_PATH=""

# ── production file untouched ───────────────────────────────────────────────
cmp -s "$INSTALLER" "$WORK/install.sh.orig" \
    || fail "the production install.sh was modified by the harness"

[ "$SCENARIOS_RUN" -eq 24 ] || fail "expected 24 scenarios, ran $SCENARIOS_RUN"
echo "install-smoke: all $SCENARIOS_RUN scenarios passed"
