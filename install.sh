#!/bin/sh
# libra installer · TUI
#
#   curl -fsSL https://download.libra.tools/install.sh | sh
#   curl -fsSL https://download.libra.tools/install.sh | sh -s -- -v v0.17.874
#
# Visual design ports the Libra TUI Installer mock — banner, conversational
# agent voice, animated per-step spinner, themed colors, success box.
# Set NO_COLOR=1 or LIBRA_NO_TUI=1 (or pipe to a non-tty) for plain output.

set -e

# ─── config ──────────────────────────────────────────────────────────────────
BASE_URL="${LIBRA_BASE_URL:-https://download.libra.tools/libra/releases}"
LIBRA_HOME="${LIBRA_HOME:-${HOME:-/tmp}/.libra}"
INSTALL_DIR="${LIBRA_INSTALL_DIR:-$LIBRA_HOME/bin}"
# DEFAULT_VERSION is only used when the release API is unreachable AND the
# user opts in with LIBRA_ALLOW_FALLBACK=1. Default behaviour is fail-fast so
# offline installs cannot silently regress to a stale version. Bump this on
# every release so the opt-in fallback remains useful.
DEFAULT_VERSION="v0.17.874"

# ─── theme (Dusk) ────────────────────────────────────────────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ -z "${LIBRA_NO_TUI:-}" ] && [ "${TERM:-dumb}" != "dumb" ]; then
    TTY=1
else
    TTY=0
fi

if [ "$TTY" = "1" ]; then
    C_RESET=$(printf '\033[0m')
    C_BOLD=$(printf '\033[1m')
    C_DIM=$(printf '\033[38;5;244m')
    C_TEXT=$(printf '\033[38;5;252m')
    C_ACCENT=$(printf '\033[38;5;117m')
    C_ACCENT2=$(printf '\033[38;5;159m')
    C_SUCCESS=$(printf '\033[38;5;114m')
    C_WARN=$(printf '\033[38;5;221m')
    C_ERROR=$(printf '\033[38;5;210m')
    C_HIDE=$(printf '\033[?25l')
    C_SHOW=$(printf '\033[?25h')
    C_CLR=$(printf '\r\033[K')
    if sleep 0.05 2>/dev/null; then SPIN_DELAY=0.08; else SPIN_DELAY=1; fi
else
    C_RESET=; C_BOLD=; C_DIM=; C_TEXT=
    C_ACCENT=; C_ACCENT2=; C_SUCCESS=; C_WARN=; C_ERROR=
    C_HIDE=; C_SHOW=; C_CLR=
    SPIN_DELAY=1
fi

cleanup() {
    [ -n "${TEMP_DIR:-}" ] && rm -rf "$TEMP_DIR"
    [ "$TTY" = "1" ] && printf '%s' "$C_SHOW"
    return 0
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# ─── drawing primitives ──────────────────────────────────────────────────────
banner() {
    printf '\n'
    printf '%s%s  ██╗     ██╗ ██████╗ ██████╗  █████╗ %s\n' "$C_BOLD" "$C_ACCENT" "$C_RESET"
    printf '%s%s  ██║     ██║ ██╔══██╗██╔══██╗██╔══██╗%s\n' "$C_BOLD" "$C_ACCENT" "$C_RESET"
    printf '%s%s  ██║     ██║ ██████╔╝██████╔╝███████║%s\n' "$C_BOLD" "$C_ACCENT" "$C_RESET"
    printf '%s%s  ██║     ██║ ██╔══██╗██╔══██╗██╔══██║%s\n' "$C_BOLD" "$C_ACCENT" "$C_RESET"
    printf '%s%s  ███████╗██║ ██████╔╝██║  ██║██║  ██║%s\n' "$C_BOLD" "$C_ACCENT" "$C_RESET"
    printf '%s%s  ╚══════╝╚═╝ ╚═════╝ ╚═╝  ╚═╝╚═╝  ╚═╝%s\n' "$C_BOLD" "$C_ACCENT" "$C_RESET"
    printf '    %s▸%s %sAI-agent-native version control · %s%s%s\n\n' \
        "$C_DIM" "$C_RESET" "$C_TEXT" "$C_ACCENT" "${VERSION:-$DEFAULT_VERSION}" "$C_RESET"
}

# Conversational box: ┌─ ◆ libra-agent ─… / └─…
agent_say() {
    if [ "$TTY" = "1" ]; then
        printf '%s┌─%s ◆ libra-agent %s─────────────────────────────────────────────────%s\n' \
            "$C_DIM" "$C_ACCENT" "$C_DIM" "$C_RESET"
        printf '  %s%s%s\n' "$C_TEXT" "$1" "$C_RESET"
        printf '%s└──────────────────────────────────────────────────────────────────────%s\n\n' \
            "$C_DIM" "$C_RESET"
    else
        printf '[libra-agent] %s\n\n' "$1"
    fi
}

section() {
    printf '  %s── %s ──%s\n' "$C_DIM" "$1" "$C_RESET"
}

fact() {
    printf '  %s✓%s  %s%-20s%s %s%s%s\n' \
        "$C_SUCCESS" "$C_RESET" \
        "$C_TEXT" "$1" "$C_RESET" \
        "$C_DIM" "$2" "$C_RESET"
}

warn_fact() {
    printf '  %s!%s  %s%-20s%s %s%s%s\n' \
        "$C_WARN" "$C_RESET" \
        "$C_TEXT" "$1" "$C_RESET" \
        "$C_DIM" "$2" "$C_RESET"
}

# Run a command with a Braille spinner; replace with ✓/✗ on completion.
run_step() {
    label=$1
    shift
    if [ "$TTY" != "1" ]; then
        printf '  ·  %s ... ' "$label"
        if "$@" >/dev/null 2>&1; then
            printf 'ok\n'
            return 0
        else
            rc=$?
            printf 'fail\n'
            return $rc
        fi
    fi

    # Keep step logs inside TEMP_DIR so the trap cleanup sweeps them up on
    # SIGTERM/INT. mktemp's template form is portable across GNU and BSD.
    log=$(mktemp "${TEMP_DIR:-/tmp}/libra-step.XXXXXX" 2>/dev/null) || return 1
    ( "$@" ) >"$log" 2>&1 &
    pid=$!

    printf '%s' "$C_HIDE"
    i=0
    while kill -0 "$pid" 2>/dev/null; do
        case $((i % 10)) in
            0) f='⠋' ;; 1) f='⠙' ;; 2) f='⠹' ;; 3) f='⠸' ;; 4) f='⠼' ;;
            5) f='⠴' ;; 6) f='⠦' ;; 7) f='⠧' ;; 8) f='⠇' ;; 9) f='⠏' ;;
        esac
        printf '%s  %s%s%s  %s%s%s' "$C_CLR" "$C_ACCENT" "$f" "$C_RESET" "$C_TEXT" "$label" "$C_RESET"
        i=$((i + 1))
        sleep "$SPIN_DELAY" 2>/dev/null || true
    done

    if wait "$pid"; then rc=0; else rc=$?; fi
    printf '%s' "$C_CLR"
    printf '%s' "$C_SHOW"

    if [ "$rc" = "0" ]; then
        printf '  %s✓%s  %s%s%s\n' "$C_SUCCESS" "$C_RESET" "$C_TEXT" "$label" "$C_RESET"
    else
        printf '  %s✗%s  %s%s%s\n' "$C_ERROR" "$C_RESET" "$C_ERROR" "$label" "$C_RESET"
        if [ -s "$log" ]; then
            while IFS= read -r ln; do
                printf '       %s%s%s\n' "$C_DIM" "$ln" "$C_RESET"
            done <"$log"
        fi
    fi
    rm -f "$log"
    return $rc
}

success_box() {
    printf '  %s%s╭───────────────────────────────╮%s\n' "$C_BOLD" "$C_SUCCESS" "$C_RESET"
    printf '  %s%s│                               │%s\n' "$C_BOLD" "$C_SUCCESS" "$C_RESET"
    printf '  %s%s│   ✓  libra is ready to use    │%s\n' "$C_BOLD" "$C_SUCCESS" "$C_RESET"
    printf '  %s%s│                               │%s\n' "$C_BOLD" "$C_SUCCESS" "$C_RESET"
    printf '  %s%s╰───────────────────────────────╯%s\n\n' "$C_BOLD" "$C_SUCCESS" "$C_RESET"
}

# Rust-compiler-styled error block + recovery hints; exits 1.
error_exit() {
    msg=$1
    stage=${2:-install}
    detail=${3:-}
    printf '\n  %s✗ install failed at stage — %s%s\n\n' "$C_ERROR" "$stage" "$C_RESET"
    printf '  %s┃%s  %serror:%s %s\n' "$C_ERROR" "$C_RESET" "$C_ERROR" "$C_RESET" "$msg"
    if [ -n "$detail" ]; then
        printf '  %s┃%s  %s%s%s\n' "$C_ERROR" "$C_RESET" "$C_DIM" "$detail" "$C_RESET"
    fi
    printf '  %s┃%s\n' "$C_ERROR" "$C_RESET"
    printf '  %s┗━%s I know this kind of failure. Try one of these:\n' "$C_ERROR" "$C_RESET"
    printf '       %s▸%s use the default user-local path  %sunset LIBRA_INSTALL_DIR LIBRA_HOME; re-run the installer%s\n' \
        "$C_ACCENT" "$C_RESET" "$C_ACCENT2" "$C_RESET"
    # shellcheck disable=SC2016  # $HOME is shown to the user verbatim
    printf '       %s▸%s pick a writable directory        %sexport LIBRA_HOME="$HOME/.libra"%s\n' \
        "$C_ACCENT" "$C_RESET" "$C_ACCENT2" "$C_RESET"
    printf '       %s▸%s pin a known-good version         %scurl -fsSL https://download.libra.tools/install.sh | sh -s -- -v v0.1.0%s\n' \
        "$C_ACCENT" "$C_RESET" "$C_ACCENT2" "$C_RESET"
    printf '       %s▸%s open a bug report                %sgithub.com/libra-tools/libra/issues%s\n' \
        "$C_ACCENT" "$C_RESET" "$C_ACCENT2" "$C_RESET"
    printf '\n  %sneed the full log? re-run with:%s\n' "$C_DIM" "$C_RESET"
    printf '  %scurl -fsSL https://download.libra.tools/install.sh | sh 2>&1 | tee install.log%s\n\n' "$C_TEXT" "$C_RESET"
    exit 1
}

# ─── argument parsing ────────────────────────────────────────────────────────
usage() {
    cat <<EOF
libra installer

USAGE:
    install.sh [OPTIONS]

OPTIONS:
    -v, --version <VERSION>    Specify version (default: latest)
    -d, --dir <PATH>           Installation directory (default: \$HOME/.libra/bin)
        --no-modify-path       Do not touch shell rc files (still writes \$LIBRA_HOME/env)
    -h, --help                 Show this help message

EXAMPLES:
    # Install latest version (no sudo, lives entirely under \$HOME/.libra)
    curl -fsSL https://download.libra.tools/install.sh | sh

    # Install specific version
    curl -fsSL https://download.libra.tools/install.sh | sh -s -- -v v0.1.0

    # Install to custom directory (must be user-writable; we never sudo)
    curl -fsSL https://download.libra.tools/install.sh | sh -s -- -d ~/bin

    # Skip shell-rc modification (source \$HOME/.libra/env yourself)
    curl -fsSL https://download.libra.tools/install.sh | sh -s -- --no-modify-path

ENVIRONMENT VARIABLES:
    LIBRA_VERSION              Override version detection
    LIBRA_HOME                 Override install root (default: \$HOME/.libra)
    LIBRA_INSTALL_DIR          Override binary directory (default: \$LIBRA_HOME/bin)
    LIBRA_BASE_URL             Override download base URL
    LIBRA_REQUIRE_CHECKSUM=1   Fail if mirror does not publish <binary>.sha256
    LIBRA_ALLOW_FALLBACK=1     If release API is unreachable, install \$DEFAULT_VERSION
                               instead of erroring out (default: error out — prevents
                               silent regression to a stale baked-in version)
    NO_COLOR / LIBRA_NO_TUI    Disable colored / animated output
EOF
    exit 0
}

parse_args() {
    VERSION="${LIBRA_VERSION:-}"
    MODIFY_PATH=1
    while [ $# -gt 0 ]; do
        case "$1" in
            -h|--help)         usage ;;
            -v|--version)
                [ $# -lt 2 ] && error_exit "missing argument for $1" "args" "expected: -v <version>"
                VERSION="$2"; shift 2 ;;
            -d|--dir)
                [ $# -lt 2 ] && error_exit "missing argument for $1" "args" "expected: -d <path>"
                INSTALL_DIR="$2"; shift 2 ;;
            --no-modify-path)  MODIFY_PATH=0; shift ;;
            *) error_exit "unknown option: $1" "args" "use --help to see supported flags" ;;
        esac
    done
}

# Reject paths that would corrupt the generated env file (which inserts the
# path inside double-quoted shell strings). POSIX path conventions allow most
# printable chars but a few are dangerous when embedded in shell source.
validate_path() {
    name=$1
    val=$2
    bad=""
    case "$val" in
        *'"'*) bad='"' ;;
        *'$'*) bad='$' ;;
        *'`'*) bad='`' ;;
        *'\'*) bad='\' ;;
    esac
    # Newline is hard to express in a `case` pattern portably; check via tr.
    if [ -z "$bad" ] && [ "$(printf '%s' "$val" | tr -d '\n')" != "$val" ]; then
        bad='newline'
    fi
    if [ -n "$bad" ]; then
        error_exit "$name contains unsafe character ($bad) — would corrupt the generated env file" "args" \
            "use a plain path (letters, digits, / - _ . space are fine)"
    fi
}

# ─── platform detection ──────────────────────────────────────────────────────
detect_os() {
    OS_RAW=$(uname -s)
    case "$OS_RAW" in
        Linux)  OS=linux  ;;
        Darwin) OS=darwin ;;
        *) error_exit "unsupported operating system: $OS_RAW" "detect" "libra ships builds for linux & darwin" ;;
    esac
}

detect_arch() {
    ARCH_RAW=$(uname -m)
    case "$ARCH_RAW" in
        x86_64|amd64)  ARCH=amd64 ;;
        aarch64|arm64) ARCH=arm64 ;;
        *) error_exit "unsupported architecture: $ARCH_RAW" "detect" "libra builds amd64 and arm64" ;;
    esac
}

check_dependencies() {
    if command -v curl >/dev/null 2>&1; then
        DOWNLOADER=curl
    elif command -v wget >/dev/null 2>&1; then
        DOWNLOADER=wget
    else
        error_exit "neither curl nor wget found" "detect" "install one of them, then re-run"
    fi
}

download_file() {
    # Bounded timeouts so a stalled mirror cannot hang CI / autoinstall flows.
    # 300s max wallclock covers a ~12 MB binary down to ~40 KB/s; .sha256 is tiny
    # so the same cap applies harmlessly.
    if [ "$DOWNLOADER" = "curl" ]; then
        curl -fsSL --connect-timeout 10 --max-time 300 "$1" -o "$2"
    else
        wget -q --timeout=30 --tries=3 "$1" -O "$2"
    fi
}

# Print sha256 hex of "$1", or empty string if no hashing tool is available.
sha256_of() {
    file=$1
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" 2>/dev/null | awk '{print $1; exit}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" 2>/dev/null | awk '{print $1; exit}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$file" 2>/dev/null | awk '{print $NF; exit}'
    fi
}

# Verify "$1" (binary) against "<$2>.sha256" published next to it.
# Behaviour:
#   - hash file present + matches  → ok, prints fact line.
#   - hash file 404                → warn + skip (forward-compatible with releases
#                                    that don't publish .sha256 yet). Set
#                                    LIBRA_REQUIRE_CHECKSUM=1 to make this fatal.
#   - hash file present + differs  → fatal (supply-chain alarm).
verify_checksum() {
    bin_file=$1
    bin_url=$2
    sum_url="${bin_url}.sha256"
    sum_file="${TEMP_DIR}/$(basename "$bin_file").sha256"

    if ! download_file "$sum_url" "$sum_file" 2>/dev/null; then
        if [ "${LIBRA_REQUIRE_CHECKSUM:-0}" = "1" ]; then
            error_exit "no checksum published at $sum_url" "verify" \
                "LIBRA_REQUIRE_CHECKSUM=1 is set; unset it or wait for a release that publishes .sha256"
        fi
        warn_fact "checksum" "not published at mirror — skipping (set LIBRA_REQUIRE_CHECKSUM=1 to enforce)"
        return 0
    fi

    expected=$(awk '{print $1; exit}' "$sum_file" 2>/dev/null)
    if [ -z "$expected" ]; then
        error_exit "checksum file at $sum_url is empty or malformed" "verify" \
            "the mirror returned an unusable .sha256 — retry, or report at github.com/libra-tools/libra/issues"
    fi
    actual=$(sha256_of "$bin_file")
    if [ -z "$actual" ]; then
        if [ "${LIBRA_REQUIRE_CHECKSUM:-0}" = "1" ]; then
            error_exit "no sha256 tool found (need sha256sum / shasum / openssl)" "verify" \
                "install one of them, or unset LIBRA_REQUIRE_CHECKSUM"
        fi
        warn_fact "checksum" "no hashing tool — skipping (install sha256sum / shasum / openssl to verify)"
        return 0
    fi
    if [ "$expected" != "$actual" ]; then
        error_exit "sha256 mismatch (expected $expected, got $actual)" "verify" \
            "the mirror may be compromised — please report at github.com/libra-tools/libra/issues"
    fi
    fact "checksum" "sha256 ok"
}

fetch_latest_version() {
    # Returns the latest tag, or empty string on failure. Caller decides what
    # to do with empty (fail-fast vs. opt-in fallback) — see main().
    api_url="https://api.github.com/repos/libra-tools/libra/releases/latest"
    if [ "$DOWNLOADER" = "curl" ]; then
        curl -fsSL --connect-timeout 5 --max-time 10 "$api_url" 2>/dev/null \
            | grep '"tag_name":' | head -n1 \
            | sed 's/.*"tag_name": "\([^"]*\)".*/\1/'
    else
        wget -q --timeout=10 --tries=1 -O- "$api_url" 2>/dev/null \
            | grep '"tag_name":' | head -n1 \
            | sed 's/.*"tag_name": "\([^"]*\)".*/\1/'
    fi
}

probe_network() {
    if [ "$DOWNLOADER" = "curl" ]; then
        curl -fsSL --max-time 4 -o /dev/null https://libra.tools 2>/dev/null
    else
        wget -q --tries=1 --timeout=4 -O /dev/null https://libra.tools 2>/dev/null
    fi
}

# Normalize a version string to "vX.Y.Z..." form (idempotent).
norm_version() {
    case "$1" in
        v*) printf '%s' "$1" ;;
        '') printf '%s' "$1" ;;
        *)  printf 'v%s' "$1" ;;
    esac
}

# Detect a prior libra install. Sets EXISTING_PATH and EXISTING_VERSION.
#  - prefers $INSTALL_DIR/libra (the target we'd write to)
#  - falls back to whatever's first on $PATH
# Leaves EXISTING_VERSION empty if the binary cannot report a parseable version.
detect_existing_install() {
    EXISTING_PATH=""
    EXISTING_VERSION=""

    candidate=""
    if [ -x "${INSTALL_DIR}/libra" ]; then
        candidate="${INSTALL_DIR}/libra"
    elif command -v libra >/dev/null 2>&1; then
        candidate=$(command -v libra)
    fi
    [ -n "$candidate" ] || return 0

    EXISTING_PATH=$candidate
    ev=$("$candidate" --version 2>/dev/null | head -n1 \
            | grep -oE 'v?[0-9]+\.[0-9]+\.[0-9]+[A-Za-z0-9.+-]*' \
            | head -n1)
    [ -n "$ev" ] || return 0
    EXISTING_VERSION=$(norm_version "$ev")
}

# ─── screens (ports of the design) ───────────────────────────────────────────
screen_welcome() {
    banner
    agent_say "Hi — I'm the libra installer. I'll set up the AI-agent-native VCS for you in about 30 seconds. I'll show you what I'm doing at every step."
    printf '  %sgithub.com/libra-tools/libra%s\n'   "$C_DIM" "$C_RESET"
    printf '  %scurl -fsSL https://download.libra.tools/install.sh | sh%s\n\n' "$C_DIM" "$C_RESET"
    [ "$TTY" = "1" ] && sleep 0.5 2>/dev/null || true
}

screen_detect() {
    section "01 · detect environment"
    agent_say "Scanning your system. This won't change anything yet — just looking around."

    fact "operating system" "$OS_RAW ($OS)"
    fact "architecture"     "$ARCH_RAW ($ARCH)"

    dl_ver=$($DOWNLOADER --version 2>/dev/null | head -n1 | awk '{print $2}')
    fact "downloader"       "$DOWNLOADER ${dl_ver:-?}"

    if [ -n "$EXISTING_VERSION" ]; then
        if [ "$EXISTING_VERSION" = "$VERSION" ]; then
            fact      "libra (installed)" "$EXISTING_VERSION at $EXISTING_PATH — already at requested version"
        else
            warn_fact "libra (installed)" "$EXISTING_VERSION at $EXISTING_PATH — will replace with $VERSION"
        fi
    elif [ -n "$EXISTING_PATH" ]; then
        warn_fact "libra (installed)" "$EXISTING_PATH (could not read --version) — will overwrite"
    else
        fact "libra (installed)"      "none — first install"
    fi

    if command -v df >/dev/null 2>&1; then
        check_dir=$(dirname "$INSTALL_DIR")
        [ -d "$check_dir" ] || check_dir="${HOME:-/}"
        avail_kb=$(df -k "$check_dir" 2>/dev/null | awk 'NR==2 {print $4}')
        if [ -n "$avail_kb" ] && [ "$avail_kb" -gt 0 ] 2>/dev/null; then
            avail_mb=$((avail_kb / 1024))
            if [ "$avail_kb" -lt 51200 ]; then
                warn_fact "disk space" "${avail_mb} MB available — low (50 MB+ recommended)"
            else
                fact "disk space" "${avail_mb} MB available"
            fi
        fi
    fi

    if probe_network; then
        fact "network"      "libra.tools reachable"
    else
        warn_fact "network" "libra.tools unreachable — using fallback ${DEFAULT_VERSION}"
    fi

    fact "shell"            "${SHELL:-unknown}"

    if [ "$OS" = "linux" ] && command -v ldd >/dev/null 2>&1; then
        glibc=$(ldd --version 2>&1 | head -n1 | grep -oE '[0-9]+\.[0-9]+' | head -n1)
        if [ -n "$glibc" ]; then
            major=$(echo "$glibc" | cut -d. -f1)
            minor=$(echo "$glibc" | cut -d. -f2)
            if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 31 ]; }; then
                warn_fact "glibc"   "$glibc — libra prefers 2.31+"
            else
                fact "glibc"        "$glibc"
            fi
        fi
    fi

    printf '\n'
    agent_say "Everything checks out. You're on a supported platform with the toolchain I need."
}

screen_method() {
    section "02 · choose install method"
    agent_say "Picking the prebuilt binary — fastest path, ready in seconds. I'll verify a SHA256 if the mirror publishes one. (cargo / source builds also available; re-run with --help to see flags.)"
    printf '  %s▸%s %s%sPrebuilt binary%s  %s(recommended)%s\n' \
        "$C_ACCENT" "$C_RESET" "$C_BOLD" "$C_TEXT" "$C_RESET" "$C_ACCENT2" "$C_RESET"
    printf '      %ssize:%s   ~12 MB compressed\n'  "$C_DIM" "$C_RESET"
    printf '      %stime:%s   a few seconds\n'      "$C_DIM" "$C_RESET"
    printf '      %sneeds:%s  %s\n\n'               "$C_DIM" "$C_RESET" "$DOWNLOADER"
}

screen_already_installed() {
    success_box
    agent_say "libra ${VERSION} is already installed at ${EXISTING_PATH}. Nothing to do."

    section "installed"
    printf '  %s✓%s libra %s%s · %s%s\n\n' \
        "$C_SUCCESS" "$C_RESET" "$C_TEXT" "$VERSION" "$EXISTING_PATH" "$C_RESET"

    printf '  %sneed a different version?%s\n' "$C_DIM" "$C_RESET"
    printf '  %scurl -fsSL https://download.libra.tools/install.sh | sh -s -- -v <version>%s\n\n' "$C_TEXT" "$C_RESET"
}

screen_install() {
    section "03 · install"
    if [ -n "$EXISTING_VERSION" ]; then
        agent_say "Replacing libra ${EXISTING_VERSION} with ${VERSION} for ${OS}/${ARCH} in ${INSTALL_DIR}. No sudo — the target must be user-writable."
    else
        agent_say "Downloading libra ${VERSION} for ${OS}/${ARCH} into ${INSTALL_DIR}. No sudo — the target must be user-writable."
    fi

    binary_name="libra-${OS}-${ARCH}"
    download_url="${BASE_URL}/${VERSION}/${binary_name}"
    TEMP_DIR=$(mktemp -d 2>/dev/null) \
        || error_exit "mktemp failed" "install" "make sure mktemp is installed and \$TMPDIR is writable"
    temp_file="${TEMP_DIR}/${binary_name}"

    # Create LIBRA_HOME and INSTALL_DIR; both are under $HOME by default,
    # so this should never need elevated privileges.
    if ! mkdir -p "$LIBRA_HOME" "$INSTALL_DIR" 2>/dev/null; then
        error_exit "cannot create $INSTALL_DIR" "install" \
            "pick a writable path with LIBRA_HOME or -d (we never sudo)"
    fi

    run_step "fetch $binary_name" download_file "$download_url" "$temp_file" \
        || error_exit "download failed" "install" "url: $download_url"

    [ -s "$temp_file" ] || error_exit "downloaded file is empty" "install" "the mirror may be corrupted — please retry"

    verify_checksum "$temp_file" "$download_url"

    BIN_SIZE=$(wc -c <"$temp_file" 2>/dev/null | awk '{printf "%.1f MB", $1/1048576}')

    run_step "verify & make executable" chmod +x "$temp_file" \
        || error_exit "could not chmod binary" "install"

    target="${INSTALL_DIR}/libra"
    if [ ! -w "$INSTALL_DIR" ]; then
        error_exit "no write permission to $INSTALL_DIR" "install" \
            "this installer never sudos — pick a writable path with LIBRA_HOME or -d"
    fi

    run_step "install to $target" mv "$temp_file" "$target" \
        || error_exit "could not install to $target" "install"

    INSTALLED_PATH="$target"
    printf '\n'
}

# Generate $LIBRA_HOME/env (POSIX) and $LIBRA_HOME/env.fish.
# Sourcing the file is idempotent — it adds INSTALL_DIR to PATH only when missing.
write_env_files() {
    mkdir -p "$LIBRA_HOME" 2>/dev/null || return 1

    # POSIX-compatible (sh / bash / zsh / dash / ksh).
    # $PATH must stay literal so the *target* shell expands it at source time.
    {
        printf '#!/bin/sh\n'
        printf '# libra shell setup; source me from your shell rc.\n'
        # shellcheck disable=SC2016
        printf 'case ":${PATH}:" in\n'
        printf '    *:"%s":*) ;;\n' "$INSTALL_DIR"
        # shellcheck disable=SC2016
        printf '    *) export PATH="%s:$PATH" ;;\n' "$INSTALL_DIR"
        printf 'esac\n'
    } > "$LIBRA_HOME/env"
    chmod 644 "$LIBRA_HOME/env" 2>/dev/null || true

    # fish syntax; $PATH must stay literal for the target fish shell.
    {
        printf '# libra shell setup; source me from your fish config.\n'
        # shellcheck disable=SC2016
        printf 'if not contains -- "%s" $PATH\n' "$INSTALL_DIR"
        # shellcheck disable=SC2016
        printf '    set -gx PATH "%s" $PATH\n' "$INSTALL_DIR"
        printf 'end\n'
    } > "$LIBRA_HOME/env.fish"
    chmod 644 "$LIBRA_HOME/env.fish" 2>/dev/null || true
}

# Append the source line to an rc file if not already present.
# Returns: 0 = wrote new line, 2 = already wired, 1 = file does not exist / not writable.
# Sets RC_TOUCHED_LIST as a side effect when 0.
RC_TOUCHED_LIST=""
RC_ALREADY_LIST=""
RC_STALE_LIST=""
update_rc() {
    rc=$1
    syntax=${2:-posix}
    [ -e "$rc" ] || return 1
    [ -w "$rc" ] || return 1

    # Idempotency: look for our marker, then check the path it references.
    # We never auto-rewrite the block (that would silently destroy any user
    # edits inside it); instead we warn loudly if the block is stale.
    if grep -qF '# >>> libra >>>' "$rc" 2>/dev/null; then
        if grep -qF "\"$LIBRA_HOME/env" "$rc" 2>/dev/null; then
            RC_ALREADY_LIST="$RC_ALREADY_LIST $rc"
            return 2
        else
            RC_STALE_LIST="$RC_STALE_LIST $rc"
            return 3
        fi
    fi

    if [ "$syntax" = "fish" ]; then
        {
            printf '\n# >>> libra >>>\n'
            printf 'source "%s/env.fish"\n' "$LIBRA_HOME"
            printf '# <<< libra <<<\n'
        } >> "$rc" || return 1
    else
        {
            printf '\n# >>> libra >>>\n'
            printf '. "%s/env"\n' "$LIBRA_HOME"
            printf '# <<< libra <<<\n'
        } >> "$rc" || return 1
    fi

    RC_TOUCHED_LIST="$RC_TOUCHED_LIST $rc"
    return 0
}

screen_shell() {
    section "04 · shell integration"

    write_env_files || error_exit "could not write $LIBRA_HOME/env" "shell" \
        "check that $LIBRA_HOME is writable"

    # If already on PATH (e.g. user pre-added or re-running install), tell them.
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            agent_say "${INSTALL_DIR} is already on your PATH — wrote ${LIBRA_HOME}/env for new shells anyway."
            return 0
            ;;
    esac

    if [ "${MODIFY_PATH:-1}" = "0" ]; then
        agent_say "Skipping shell-rc modification (--no-modify-path). To activate libra now, run the line below; add it to your shell profile when you're ready."
        printf '  %s. "%s/env"%s\n\n' "$C_TEXT" "$LIBRA_HOME" "$C_RESET"
        return 0
    fi

    # Touch a conservative set of common rc files. POSIX shells get $HOME/.profile
    # as the universal fallback; bash/zsh/fish get their own.
    [ -n "${HOME:-}" ] || return 0

    # Ensure .profile exists so login shells pick libra up even if no bashrc exists.
    if [ ! -e "$HOME/.profile" ]; then
        : > "$HOME/.profile" 2>/dev/null || true
    fi

    update_rc "$HOME/.profile"      posix || true
    update_rc "$HOME/.bashrc"       posix || true
    update_rc "$HOME/.bash_profile" posix || true
    update_rc "$HOME/.zshrc"        posix || true
    update_rc "$HOME/.zshenv"       posix || true
    if [ -d "$HOME/.config/fish" ]; then
        [ -e "$HOME/.config/fish/config.fish" ] || : > "$HOME/.config/fish/config.fish" 2>/dev/null || true
        update_rc "$HOME/.config/fish/config.fish" fish || true
    fi

    if [ -n "$RC_TOUCHED_LIST" ]; then
        agent_say "Wired libra into your shell. New terminals will pick it up automatically; for the current shell, source the env file once."
        for rc in $RC_TOUCHED_LIST; do
            fact "updated" "$rc"
        done
        for rc in $RC_ALREADY_LIST; do
            fact "already wired" "$rc"
        done
        printf '\n  %sactivate now (current shell):%s\n' "$C_DIM" "$C_RESET"
        printf '  %s. "%s/env"%s\n\n' "$C_TEXT" "$LIBRA_HOME" "$C_RESET"
    elif [ -n "$RC_ALREADY_LIST" ]; then
        agent_say "Your shell rc files are already wired to libra — no changes needed."
        for rc in $RC_ALREADY_LIST; do
            fact "already wired" "$rc"
        done
        printf '\n'
    else
        agent_say "Could not auto-modify a shell profile. Add the line below to your shell rc (~/.zshrc, ~/.bashrc, or fish equivalent)."
        printf '  %s. "%s/env"%s        %s# posix shells%s\n'  "$C_TEXT" "$LIBRA_HOME" "$C_RESET" "$C_DIM" "$C_RESET"
        printf '  %ssource "%s/env.fish"%s   %s# fish%s\n\n'   "$C_TEXT" "$LIBRA_HOME" "$C_RESET" "$C_DIM" "$C_RESET"
    fi

    # Stale-path warning: another LIBRA_HOME is wired in this rc. New shells
    # will keep sourcing the OLD env file, not this one. We refuse to auto-
    # rewrite the block (the user may have edited it); make the fix explicit.
    if [ -n "$RC_STALE_LIST" ]; then
        agent_say "Heads up: some shell rc files still source a different LIBRA_HOME. New shells will pick up the OLD install, not this one. Remove the libra block (between '# >>> libra >>>' and '# <<< libra <<<') in each file below, then re-run."
        for rc in $RC_STALE_LIST; do
            warn_fact "stale libra block" "$rc"
        done
        printf '\n'
    fi
}

screen_success() {
    success_box
    agent_say "Installed in about 30 seconds. You're all set — here's what to try first:"

    pad="                                       "
    fmtcmd() {
        cmd=$1; desc=$2
        len=${#cmd}
        # right-pad cmd to width 38
        if [ "$len" -lt 38 ]; then
            sp=$(printf '%s' "$pad" | cut -c1-$((38 - len)))
        else
            sp=' '
        fi
        printf '  %s$%s %s%s%s%s%s  %s%s%s\n' \
            "$C_DIM" "$C_RESET" \
            "$C_BOLD" "$C_ACCENT" "$cmd" "$C_RESET" "$sp" \
            "$C_DIM" "$desc" "$C_RESET"
    }

    fmtcmd 'libra init'                              'turn the current directory into a libra repo'
    fmtcmd 'libra agent ask "review my changes"'     'let the agent take a look'
    fmtcmd 'libra status'                            'familiar — works just like git'
    fmtcmd 'libra --help'                            'every command, with examples'
    printf '\n'

    section "installed"
    printf '  %s✓%s libra %s%s · %s · %s%s\n' \
        "$C_SUCCESS" "$C_RESET" \
        "$C_TEXT" "$VERSION" "${BIN_SIZE:-binary}" "${INSTALLED_PATH:-${INSTALL_DIR}/libra}" "$C_RESET"
    case ":$PATH:" in
        *":$INSTALL_DIR:"*)
            printf '  %s✓%s on PATH — open any new terminal and run %slibra --help%s\n\n' \
                "$C_SUCCESS" "$C_RESET" "$C_ACCENT" "$C_RESET"
            ;;
        *)
            printf '  %s▸%s to use it in this shell now:  %s. "%s/env"%s\n\n' \
                "$C_ACCENT" "$C_RESET" "$C_ACCENT2" "$LIBRA_HOME" "$C_RESET"
            ;;
    esac

    section "next"
    printf '  %s📖 docs.libra.tools%s\n'                          "$C_TEXT" "$C_RESET"
    printf '  %s💬 discord.libra.tools%s\n'                       "$C_TEXT" "$C_RESET"
    printf '  %s⭐ github.com/libra-tools/libra%s\n\n'   "$C_TEXT" "$C_RESET"
}

# ─── main ────────────────────────────────────────────────────────────────────
main() {
    parse_args "$@"

    # Fail fast on shell-unsafe paths before they reach generated files.
    validate_path "LIBRA_HOME"        "$LIBRA_HOME"
    validate_path "LIBRA_INSTALL_DIR" "$INSTALL_DIR"

    detect_os
    detect_arch
    check_dependencies

    if [ -z "$VERSION" ]; then
        VERSION=$(fetch_latest_version)
        if [ -z "$VERSION" ]; then
            if [ "${LIBRA_ALLOW_FALLBACK:-0}" = "1" ]; then
                VERSION=$DEFAULT_VERSION
            else
                error_exit "could not determine latest version (release API unreachable or rate-limited)" "version" \
                    "pass -v <version> explicitly, or set LIBRA_ALLOW_FALLBACK=1 to use $DEFAULT_VERSION"
            fi
        fi
    fi
    VERSION=$(norm_version "$VERSION")

    detect_existing_install

    screen_welcome
    screen_detect

    # Short-circuit: same version already installed → don't touch anything.
    if [ -n "$EXISTING_VERSION" ] && [ "$EXISTING_VERSION" = "$VERSION" ]; then
        screen_already_installed
        exit 0
    fi

    screen_method
    screen_install
    screen_shell
    screen_success
}

main "$@"
