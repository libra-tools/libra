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
DEFAULT_VERSION="v0.22.10"
# Public-only trust anchor for stable-manifest verification. It deliberately
# has no environment override: the install-smoke harness rewrites these
# clearly-marked constants in a temporary COPY of this script, never through
# the environment. The PEM is the same key as the hex, in SubjectPublicKeyInfo
# form for `openssl pkeyutl` (kept in sync by the trusted_keys unit tests).
LIBRA_RELEASE_MANIFEST_KEY_ID="libra-release-1"
LIBRA_RELEASE_MANIFEST_PUBLIC_KEY_HEX="68aa00ea9358d455645010d811d40702b3f67cec4bdff52d3d4fb8107afaeed3"
LIBRA_RELEASE_MANIFEST_PUBLIC_KEY_PEM="-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAaKoA6pNY1FVkUBDYEdQHArP2fOxL3/UtPU+4EHr67tM=
-----END PUBLIC KEY-----"
# Pinned origin of the signed stable channel (no env override; marker for the
# smoke harness only). Signed artifact URLs must live under this origin.
LIBRA_RELEASE_MANIFEST_ORIGIN="https://download.libra.tools"
# Key policy pins mirroring src/internal/upgrade/trusted_keys.rs (§7): the
# pinned key's rotation generation and validity window as canonical UTC.
# The window is checked against the SIGNED timestamps (published_at within,
# expires_at not beyond), exactly like the native verifier.
LIBRA_RELEASE_MANIFEST_KEY_GENERATION=1
LIBRA_RELEASE_MANIFEST_KEY_NOT_BEFORE="2026-08-31T11:09:55Z"
LIBRA_RELEASE_MANIFEST_KEY_NOT_AFTER="2027-08-31T00:00:00Z"

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
        --no-alias             Do not create the optional lba -> libra symlink
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

    # Install without the optional lba shorthand
    curl -fsSL https://download.libra.tools/install.sh | sh -s -- --no-alias

ENVIRONMENT VARIABLES:
    LIBRA_VERSION              Override version detection
    LIBRA_HOME                 Override install root (default: \$HOME/.libra)
    LIBRA_INSTALL_DIR          Override binary directory (default: \$LIBRA_HOME/bin)
    LIBRA_NO_ALIAS=1           Do not create the optional lba -> libra symlink
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
    CREATE_ALIAS=1
    [ "${LIBRA_NO_ALIAS:-0}" = "1" ] && CREATE_ALIAS=0
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
            --no-alias)        CREATE_ALIAS=0; shift ;;
            *) error_exit "unknown option: $1" "args" "use --help to see supported flags" ;;
        esac
    done
}

# Create the optional short command beside the installed binary. The relative
# target keeps the alias valid if LIBRA_HOME is moved as a directory.
#
# Safety contract:
# - a regular file/directory or a symlink to anything except this installation's
#   libra binary is never overwritten;
# - a valid relative/absolute libra symlink may be refreshed to the canonical
#   relative target;
# - filesystems/platforms that reject symlinks produce a warning, not a failed
#   libra installation.
ensure_lba_alias() {
    ALIAS_PATH="${INSTALL_DIR}/lba"

    if [ "${CREATE_ALIAS:-1}" != "1" ]; then
        ALIAS_STATUS=disabled
        return 0
    fi

    if [ ! -x "${INSTALL_DIR}/libra" ]; then
        ALIAS_STATUS=skipped
        warn_fact "lba alias" "not created — ${INSTALL_DIR}/libra is not executable"
        return 0
    fi

    if [ -L "$ALIAS_PATH" ]; then
        # Command substitution strips trailing newlines. Append and remove a
        # sentinel so a foreign target such as "libra<newline>" cannot be
        # misclassified as the exact safe target "libra".
        alias_target_with_sentinel=$(
            readlink -n "$ALIAS_PATH" 2>/dev/null || true
            printf '_'
        )
        alias_target=${alias_target_with_sentinel%_}
        case "$alias_target" in
            libra)
                ALIAS_STATUS=ready
                fact "lba alias" "$ALIAS_PATH -> libra"
                ;;
            "${INSTALL_DIR}/libra")
                if ln -sfn libra "$ALIAS_PATH" 2>/dev/null; then
                    ALIAS_STATUS=ready
                    fact "lba alias" "$ALIAS_PATH -> libra"
                else
                    ALIAS_STATUS=skipped
                    warn_fact "lba alias" "could not refresh $ALIAS_PATH — leaving the existing alias unchanged"
                fi
                ;;
            *)
                ALIAS_STATUS=skipped
                warn_fact "lba alias" "$ALIAS_PATH already points elsewhere — leaving it unchanged"
                ;;
        esac
        return 0
    fi

    if [ -e "$ALIAS_PATH" ]; then
        ALIAS_STATUS=skipped
        warn_fact "lba alias" "$ALIAS_PATH already exists and is not a Libra alias — leaving it unchanged"
        return 0
    fi

    if ln -s libra "$ALIAS_PATH" 2>/dev/null; then
        ALIAS_STATUS=ready
        fact "lba alias" "$ALIAS_PATH -> libra"
    else
        ALIAS_STATUS=skipped
        warn_fact "lba alias" "could not create $ALIAS_PATH — symlinks may be unavailable; use libra normally"
    fi
    return 0
}

# Reject paths that would corrupt the generated env file (which inserts the
# path inside double-quoted shell strings). POSIX path conventions allow most
# printable chars but a few are dangerous when embedded in shell source.
validate_path() {
    name=$1
    val=$2
    bad=""
    # shellcheck disable=SC1003  # case includes a literal backslash pattern/value
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
        *) error_exit "unsupported operating system: $OS_RAW" "detect" "libra ships builds for linux (amd64, arm64) & macOS (arm64 only)" ;;
    esac
}

detect_arch() {
    ARCH_RAW=$(uname -m)
    case "$ARCH_RAW" in
        x86_64|amd64)  ARCH=amd64 ;;
        aarch64|arm64) ARCH=arm64 ;;
        *) error_exit "unsupported architecture: $ARCH_RAW" "detect" "libra builds amd64 and arm64" ;;
    esac

    # The release matrix does not build an Intel-macOS artifact, so darwin/amd64 has no
    # downloadable binary. Fail here with an actionable message instead of letting the
    # download step 404 on ${BASE_URL}/${VERSION}/libra-darwin-amd64.
    if [ "$OS" = darwin ] && [ "$ARCH" = amd64 ]; then
        error_exit "unsupported platform: macOS on Intel (darwin/amd64)" "detect" \
            "libra ships macOS builds for Apple Silicon (arm64) only; on an Intel Mac build from source with 'cargo build --release', or run the linux/amd64 build under a Linux VM or container"
    fi
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

# Verified-channel variant: redirects are refused so the signed, origin-pinned
# URL cannot be bounced to another host by the (untrusted) transport, and the
# transfer is bounded by the SIGNED size — a hostile origin streaming more
# than the manifest promised is cut off instead of filling the disk.
# Both branches cap the stream at size+1 via head: an oversized response —
# chunked or not, Content-Length or not — yields at most one byte too many,
# which the mandatory size check then refuses. curl's --max-filesize adds an
# early abort when the length is declared up front.
download_file_pinned() {
    if [ "$DOWNLOADER" = "curl" ]; then
        curl -fsS --max-redirs 0 --max-filesize "$STABLE_SIZE" \
            --connect-timeout 10 --max-time 300 "$1" \
            | head -c $((STABLE_SIZE + 1)) > "$2"
    else
        wget -q --max-redirect=0 --timeout=30 --tries=3 -O - "$1" \
            | head -c $((STABLE_SIZE + 1)) > "$2"
    fi
}

# Official-install marker (§A.2/§A.4): records the signed provenance of the
# target so `libra upgrade` and `upgrade.mode=auto` accept this install as
# upgrade-manageable. Called ONLY on the verified path — an unverified
# fallback must never claim official provenance.
#
# Write discipline (§A.5-lite): the install dir must be OWNED by the current
# user and not world-writable, and the marker is composed inside a fresh
# 0700 staging DIRECTORY created atomically by mktemp -d — no other user can
# reach the staged file, and the unpredictable name plus private mode close
# the pre-created/replaced-symlink redirection races a bare temp file has.
# MARKER_WRITTEN feeds the final summary so a failure is never silent.
MARKER_WRITTEN=0
write_official_marker() {
    # POSIX-portable ownership + world-writability preflight (`test -O` and
    # `find -maxdepth` are not portable to dash/BSD): `ls -ldn` prints the
    # numeric owner uid in field 3 and the mode string's 9th character is
    # the others-write bit.
    dir_ls=$(ls -ldn "$INSTALL_DIR" 2>/dev/null) || {
        warn_fact "provenance" "cannot inspect the install dir — official-install marker skipped; re-run this installer to enable 'libra upgrade'"
        return 0
    }
    dir_uid=$(printf '%s\n' "$dir_ls" | awk '{print $3}')
    if [ "$dir_uid" != "$(id -u)" ]; then
        warn_fact "provenance" "install dir is not owned by you — official-install marker skipped; 'libra upgrade' will not manage this install"
        return 0
    fi
    # Match the Rust InstallDir policy (§A.5): group- OR others-writable
    # install dirs are refused by `libra upgrade`, and default umask 002
    # creates exactly such dirs. TIGHTEN the mode — but only for the
    # script's OWN default layout ($LIBRA_HOME/bin): a custom -d directory
    # may be group-shared on purpose, and silently stripping its group
    # write bit is not this installer's call.
    case "$dir_ls" in
        ????????w*|?????w*)
            if [ "$INSTALL_DIR" != "$LIBRA_HOME/bin" ]; then
                warn_fact "provenance" "custom install dir is group/world-writable, which 'libra upgrade' refuses — official-install marker skipped; run: chmod go-w '$INSTALL_DIR' if that is acceptable"
                return 0
            fi
            if chmod go-w "$INSTALL_DIR" 2>/dev/null; then
                fact "provenance" "tightened install dir permissions (chmod go-w) for upgrade management"
                # Re-verify after the change: the owner must still be us and
                # the writable bits must actually be gone (a swapped path or
                # a filesystem ignoring the chmod skips the marker).
                dir_ls=$(ls -ldn "$INSTALL_DIR" 2>/dev/null) || dir_ls=""
                case "$dir_ls" in
                    ????????w*|?????w*|"")
                        warn_fact "provenance" "install dir permissions could not be verified after tightening — official-install marker skipped"
                        return 0
                        ;;
                esac
                if [ "$(printf '%s\n' "$dir_ls" | awk '{print $3}')" != "$(id -u)" ]; then
                    warn_fact "provenance" "install dir changed owner unexpectedly — official-install marker skipped"
                    return 0
                fi
            else
                warn_fact "provenance" "install dir is group/world-writable and could not be tightened — official-install marker skipped; run: chmod go-w '$INSTALL_DIR'"
                return 0
            fi
            ;;
    esac
    marker_dir=$(mktemp -d "${INSTALL_DIR}/.libra-marker.XXXXXX" 2>/dev/null) || {
        warn_fact "provenance" "could not record the official-install marker — re-run this installer to enable 'libra upgrade'"
        return 0
    }
    # The destination must not be a directory/symlink someone pre-created:
    # `mv file dir` would silently move INTO it. Clear a regular file (the
    # normal overwrite case), refuse anything else.
    marker_dst="${INSTALL_DIR}/.libra-official-install.json"
    if [ -L "$marker_dst" ] || { [ -e "$marker_dst" ] && [ ! -f "$marker_dst" ]; }; then
        rm -rf "$marker_dir" 2>/dev/null
        warn_fact "provenance" "'$marker_dst' exists and is not a regular file — official-install marker skipped; remove it and re-run this installer"
        return 0
    fi
    if printf '{"schema_version":1,"installed_at":"%s","install_source":"official_signed_manifest","platform":"%s","version":"%s","sha256":"%s","size":%s,"manifest_key_id":"%s"}' \
        "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "${OS}-${ARCH}" "$STABLE_VERSION" \
        "$STABLE_SHA256" "$STABLE_SIZE" "$LIBRA_RELEASE_MANIFEST_KEY_ID" > "$marker_dir/marker.json" \
        && chmod 644 "$marker_dir/marker.json" \
        && mv "$marker_dir/marker.json" "$marker_dst" \
        && [ -f "$marker_dst" ] && [ ! -L "$marker_dst" ]; then
        MARKER_WRITTEN=1
        fact "provenance" "official-install marker written (enables 'libra upgrade')"
    else
        warn_fact "provenance" "could not record the official-install marker — re-run this installer to enable 'libra upgrade'"
    fi
    rm -rf "$marker_dir" 2>/dev/null
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

# ─── signed stable channel (UP-01 A1-05) ────────────────────────────────────
# Default installs verify the Ed25519-signed stable manifest before touching
# any binary. Failure taxonomy (ADR-UP01-03/06):
#   verified            → version/url/sha256/size come from the signed payload
#   manifest 404        → signature chain not enabled yet: explicit-confirm
#                         transition path (prompt, or LIBRA_ALLOW_FALLBACK=1)
#   verifier unavailable→ same explicit-confirm transition path
#   anything else       → fail closed (no partial install, non-zero exit)

# Whether this host can verify Ed25519 signatures: needs openssl with working
# ed25519 sign/verify (probed end-to-end with a throwaway key, which covers
# OpenSSL ≥ 1.1.1 and modern LibreSSL alike) plus a sha256 tool.
manifest_verifier_available() {
    command -v openssl >/dev/null 2>&1 || return 1
    [ -n "$(printf 'probe' | sha256_of_stdin)" ] || return 1
    probe_dir=$(mktemp -d 2>/dev/null) || return 1
    (
        cd "$probe_dir" || exit 1
        openssl genpkey -algorithm ed25519 -out t.pem >/dev/null 2>&1 || exit 1
        openssl pkey -in t.pem -pubout -out t.pub >/dev/null 2>&1 || exit 1
        printf 'libra-verifier-probe' > m.bin
        openssl pkeyutl -sign -inkey t.pem -rawin -in m.bin -out s.bin >/dev/null 2>&1 || exit 1
        openssl pkeyutl -verify -pubin -inkey t.pub -rawin -in m.bin -sigfile s.bin >/dev/null 2>&1
    )
    probe_ok=$?
    rm -rf "$probe_dir"
    return $probe_ok
}

# sha256 of stdin (mirrors sha256_of; empty when no tool exists).
sha256_of_stdin() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum 2>/dev/null | awk '{print $1; exit}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 2>/dev/null | awk '{print $1; exit}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 2>/dev/null | awk '{print $NF; exit}'
    fi
}

# Fetch the stable manifest into "$1". Prints one of: ok / missing / error.
# Redirects are NOT followed: the pinned origin must serve the manifest
# directly, a 3xx is treated as an error (fail closed), never as content.
fetch_stable_manifest() {
    manifest_url="${LIBRA_RELEASE_MANIFEST_ORIGIN}/libra/releases/stable/manifest-v1.json"
    if [ "$DOWNLOADER" = "curl" ]; then
        http_code=$(curl -sS --max-redirs 0 --max-filesize 1048576 --connect-timeout 10 --max-time 60 \
            -o "$1" -w '%{http_code}' "$manifest_url" 2>/dev/null) || http_code=000
        case "$http_code" in
            200) printf 'ok' ;;
            404) printf 'missing' ;;
            *)   printf 'error' ;;
        esac
    else
        # set -e guard: the || arm must capture wget's status, otherwise a 404
        # aborts the whole script here and the "missing" state is unreachable.
        wget_rc=0
        wget_out=$(wget -q --max-redirect=0 --server-response --timeout=30 --tries=2 \
            -O "$1" "$manifest_url" 2>&1) || wget_rc=$?
        if [ "$wget_rc" -eq 0 ]; then
            printf 'ok'
        elif printf '%s' "$wget_out" | grep -q ' 404 '; then
            printf 'missing'
        else
            printf 'error'
        fi
    fi
}

# POSIX lexicographic strictly-less (test's "<" is not portable).
lex_less() {
    [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | LC_ALL=C sort | head -n1)" = "$1" ]
}

# Strict canonical X.Y.Z (no leading "v", no leading zeros), the exact grammar
# of the native manifest contract. Signed payloads using any other spelling
# are rejected so revocation/floor comparisons can never be format-bypassed.
# Components are bounded to nine digits so the shell integer comparisons in
# semver_less can never overflow (stricter than the native u64 grammar — a
# ten-digit component fails closed here, which is the safe direction).
is_canonical_semver() {
    printf '%s' "$1" | grep -qE '^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$'
}

# Numeric semver strictly-less over two canonical X.Y.Z strings.
semver_less() {
    sl_a1=${1%%.*}; sl_rest=${1#*.}; sl_a2=${sl_rest%%.*}; sl_a3=${sl_rest#*.}
    sl_b1=${2%%.*}; sl_rest=${2#*.}; sl_b2=${sl_rest%%.*}; sl_b3=${sl_rest#*.}
    if [ "$sl_a1" -ne "$sl_b1" ]; then [ "$sl_a1" -lt "$sl_b1" ]; return $?; fi
    if [ "$sl_a2" -ne "$sl_b2" ]; then [ "$sl_a2" -lt "$sl_b2" ]; return $?; fi
    [ "$sl_a3" -lt "$sl_b3" ]
}

# Canonical, calendar-valid UTC timestamp ("Z", optional fractional seconds).
# Field ranges are enforced so nonsense like 2099-99-99T99:99:99Z can never
# reach the lexicographic comparisons.
is_canonical_utc() {
    printf '%s' "$1" | grep -qE \
        '^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])T([01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](\.[0-9]+)?Z$'
}

# Full calendar validity on top of the field ranges: the day must exist in
# the given month/year (leap-year aware), so 2026-09-31 is refused like the
# native RFC3339 parser would.
is_calendar_valid_utc() {
    is_canonical_utc "$1" || return 1
    cal_y=$(printf '%s' "$1" | cut -c1-4)
    cal_m=$(printf '%s' "$1" | cut -c6-7)
    cal_d=$(printf '%s' "$1" | cut -c9-10)
    # Strip leading zeros so $((...)) cannot misread them as octal.
    cal_y=${cal_y#0}; cal_y=${cal_y#0}; cal_y=${cal_y#0}
    cal_d=${cal_d#0}
    case "$cal_m" in
        01|03|05|07|08|10|12) cal_max=31 ;;
        04|06|09|11) cal_max=30 ;;
        02)
            if [ $((cal_y % 4)) -eq 0 ] && { [ $((cal_y % 100)) -ne 0 ] || [ $((cal_y % 400)) -eq 0 ]; }; then
                cal_max=29
            else
                cal_max=28
            fi
            ;;
        *) return 1 ;;
    esac
    [ "$cal_d" -le "$cal_max" ]
}

# Extract the value of a "key":"value" string field from compact JSON in $2.
json_string_field() {
    sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p" "$2" | head -n1
}

# Verify the signed stable manifest at "$1" and export STABLE_VERSION,
# STABLE_URL, STABLE_SHA256, STABLE_SIZE for this platform. Any failure here
# is terminal (fail closed) — callers must NOT fall back to unsigned installs.
verify_stable_manifest() {
    manifest_file=$1
    # Envelope byte cap mirroring the native MAX_MANIFEST_BYTES (1 MiB): a
    # hostile origin cannot force unbounded parsing.
    manifest_bytes=$(wc -c <"$manifest_file" 2>/dev/null | awk '{print $1}')
    if [ -z "$manifest_bytes" ] || [ "$manifest_bytes" -gt 1048576 ]; then
        error_exit "stable manifest exceeds the 1 MiB limit (${manifest_bytes:-?} bytes)" "verify" "refusing to install"
    fi
    work_dir=$(mktemp -d 2>/dev/null) \
        || error_exit "mktemp failed" "verify" "make sure \$TMPDIR is writable"

    # ENVELOPE extraction runs on a whitespace-stripped copy so both compact
    # and pretty-printed envelope spellings are accepted (the values — base64,
    # key ids, digits — contain no whitespace, so stripping is lossless). The
    # PAYLOAD below stays byte-exact: it is signature-bound and must be the
    # canonical compact serialization.
    norm_file="$work_dir/envelope-normalized.json"
    tr -d ' \t\r\n' < "$manifest_file" > "$norm_file"

    schema=$(sed -n 's/.*"schema_version":\([0-9][0-9]*\).*/\1/p' "$norm_file" | head -n1)
    [ "$schema" = "1" ] || { rm -rf "$work_dir"; error_exit "stable manifest has unsupported schema_version '${schema:-?}'" "verify" \
        "refusing to install — report at github.com/libra-tools/libra/issues"; }

    payload_b64=$(json_string_field payload "$norm_file")
    # The first signature entry carrying our key id (dual-signed rotations put
    # key_id before signature, as the backend serializer guarantees).
    sig_b64=$(sed -n "s/.*\"key_id\":\"${LIBRA_RELEASE_MANIFEST_KEY_ID}\",\"signature\":\"\\([^\"]*\\)\".*/\\1/p" "$norm_file" | head -n1)
    if [ -z "$payload_b64" ] || [ -z "$sig_b64" ]; then
        rm -rf "$work_dir"
        error_exit "stable manifest carries no signature from key '${LIBRA_RELEASE_MANIFEST_KEY_ID}'" "verify" \
            "refusing to install — the download origin may be compromised"
    fi

    printf '%s' "$payload_b64" | openssl base64 -d -A > "$work_dir/payload.bin" 2>/dev/null \
        || { rm -rf "$work_dir"; error_exit "stable manifest payload is not valid base64" "verify" "refusing to install"; }
    printf '%s' "$sig_b64" | openssl base64 -d -A > "$work_dir/sig.bin" 2>/dev/null \
        || { rm -rf "$work_dir"; error_exit "stable manifest signature is not valid base64" "verify" "refusing to install"; }
    printf '%s' "$LIBRA_RELEASE_MANIFEST_PUBLIC_KEY_PEM" > "$work_dir/trust.pem"
    # Domain-separated message: prefix (with one trailing NUL) || payload.
    { printf 'libra-upgrade-manifest-v1\0'; cat "$work_dir/payload.bin"; } > "$work_dir/msg.bin"

    if ! openssl pkeyutl -verify -pubin -inkey "$work_dir/trust.pem" -rawin \
        -in "$work_dir/msg.bin" -sigfile "$work_dir/sig.bin" >/dev/null 2>&1; then
        rm -rf "$work_dir"
        error_exit "stable manifest SIGNATURE VERIFICATION FAILED" "verify" \
            "refusing to install — the download origin may be compromised; report at github.com/libra-tools/libra/issues"
    fi

    payload_file="$work_dir/payload.bin"
    # The canonical payload is printable ASCII on a single line. grep/sed are
    # line-oriented, so a payload smuggling a second line (a canonical first
    # line plus trailing artifact rows) must be refused BEFORE the grammar
    # gate — any byte outside 0x20-0x7E is grounds for rejection.
    if [ "$(LC_ALL=C tr -d ' -~' < "$payload_file" | wc -c)" -ne 0 ]; then
        rm -rf "$work_dir"
        error_exit "signed manifest payload does not match the canonical serialization (non-printable bytes)" "verify" \
            "refusing to install — the payload field layout is not the release contract"
    fi
    # Structural grammar gate over the ENTIRE payload: the exact canonical
    # top-level field sequence, then artifact rows of the exact four-field
    # shape, then end-of-payload — anchored both ends. String fields cannot
    # contain quotes and every numeric field is bounded to nine digits (so
    # later shell integer comparisons can never overflow), and nothing can
    # precede, follow, or hide inside the artifacts array to spoof a value.
    # PORTABILITY: every {n,m} bound must stay <= 255 — BSD grep (macOS)
    # rejects larger repetition counts with "maximum repetition exceeds 255"
    # and the gate would then fail closed on every Mac. The revoked list uses
    # an unbounded bracket-free class instead: entries are re-validated one
    # by one below, and the whole payload is already capped at 1 MiB.
    grammar_row='\{"platform":"[^"]{1,32}","url":"[^"]{1,255}","sha256":"[0-9a-f]{64}","size":(0|[1-9][0-9]{0,8})\}'
    grammar_head='^\{"channel":"[^"]{1,32}","version":"[^"]{1,64}","control_revision":(0|[1-9][0-9]{0,8}),"published_at":"[^"]{1,64}","expires_at":"[^"]{1,64}","min_key_generation":(0|[1-9][0-9]{0,8}),"paused":(true|false),"revoked_versions":\[[^]]*\],"artifacts":\['
    if ! grep -qE "${grammar_head}${grammar_row}(,${grammar_row})*\\]\\}\$" "$payload_file"; then
        rm -rf "$work_dir"
        error_exit "signed manifest payload does not match the canonical serialization" "verify" \
            "refusing to install — the payload field layout is not the release contract"
    fi
    # Scalar fields are extracted ONLY from the payload head — everything
    # before the canonical trailing "artifacts" array — so artifact URL
    # contents can never spoof a top-level field for the sed extraction.
    head_file="$work_dir/payload-head.bin"
    sed 's/"artifacts":.*//' "$payload_file" > "$head_file"
    channel=$(json_string_field channel "$head_file")
    STABLE_VERSION=$(json_string_field version "$head_file")
    published_at=$(json_string_field published_at "$head_file")
    expires_at=$(json_string_field expires_at "$head_file")
    min_key_generation=$(sed -n 's/.*"min_key_generation":\([0-9][0-9]*\).*/\1/p' "$head_file" | head -n1)
    paused=$(sed -n 's/.*"paused":\(true\|false\).*/\1/p' "$head_file" | head -n1)

    [ "$channel" = "stable" ] || { rm -rf "$work_dir"; error_exit "signed manifest channel '${channel:-?}' is not 'stable'" "verify" "refusing to install"; }
    [ -n "$STABLE_VERSION" ] || { rm -rf "$work_dir"; error_exit "signed manifest carries no version" "verify" "refusing to install"; }
    if ! is_canonical_semver "$STABLE_VERSION"; then
        rm -rf "$work_dir"
        error_exit "signed manifest version '${STABLE_VERSION}' is not canonical X.Y.Z" "verify" \
            "refusing to install — versions must match the release contract exactly"
    fi
    # Key policy (§7, mirroring the native verifier): generation floor first,
    # then the pinned key's validity window around the SIGNED lifetime. The
    # bounded-digits re-check keeps the -gt comparison overflow-proof even if
    # the extraction ever drifts from the grammar gate.
    if [ -z "$min_key_generation" ] \
        || ! printf '%s' "$min_key_generation" | grep -qE '^(0|[1-9][0-9]{0,8})$' \
        || [ "$min_key_generation" -gt "$LIBRA_RELEASE_MANIFEST_KEY_GENERATION" ]; then
        rm -rf "$work_dir"
        error_exit "signed manifest min_key_generation ${min_key_generation:-?} is above this installer's pinned key generation ${LIBRA_RELEASE_MANIFEST_KEY_GENERATION}" "verify" \
            "a key rotation has retired this installer's trust anchor — re-download install.sh"
    fi
    # Stateless anti-replay floor: this installer was published alongside
    # DEFAULT_VERSION, so a signed manifest older than that baseline can only
    # be a replayed stale manifest — refuse it outright (no fallback).
    if semver_less "$STABLE_VERSION" "${DEFAULT_VERSION#v}"; then
        rm -rf "$work_dir"
        error_exit "signed stable manifest carries ${STABLE_VERSION}, older than this installer's baseline ${DEFAULT_VERSION#v}" "verify" \
            "possible replay of a stale manifest — re-download install.sh and retry"
    fi
    # Timestamps must be canonical, calendar-valid UTC ("Z"): offsets, bogus
    # field values, or impossible dates (2026-09-31) would defeat the
    # lexicographic comparisons below.
    if ! is_calendar_valid_utc "$expires_at"; then
        rm -rf "$work_dir"
        error_exit "signed manifest expires_at '${expires_at:-?}' is not canonical UTC (YYYY-MM-DDThh:mm:ssZ)" "verify" "refusing to install"
    fi
    if ! is_calendar_valid_utc "$published_at"; then
        rm -rf "$work_dir"
        error_exit "signed manifest published_at '${published_at:-?}' is not canonical UTC (YYYY-MM-DDThh:mm:ssZ)" "verify" "refusing to install"
    fi
    now_utc=$(date -u '+%Y-%m-%dT%H:%M:%S')
    expires_cmp=$(printf '%s' "$expires_at" | cut -c1-19)
    published_cmp=$(printf '%s' "$published_at" | cut -c1-19)
    if ! lex_less "$published_cmp" "$expires_cmp"; then
        rm -rf "$work_dir"
        error_exit "signed manifest published_at is not before expires_at" "verify" "refusing to install"
    fi
    if ! lex_less "$now_utc" "$expires_cmp"; then
        rm -rf "$work_dir"
        error_exit "signed stable manifest is expired (expires_at ${expires_at})" "verify" \
            "the publisher must renew the manifest — refusing to install"
    fi
    # Pinned-key validity window (inclusive), against the signed lifetime:
    # not_before <= published_at <= not_after AND expires_at <= not_after.
    key_nb=$(printf '%s' "$LIBRA_RELEASE_MANIFEST_KEY_NOT_BEFORE" | cut -c1-19)
    key_na=$(printf '%s' "$LIBRA_RELEASE_MANIFEST_KEY_NOT_AFTER" | cut -c1-19)
    if lex_less "$published_cmp" "$key_nb" || lex_less "$key_na" "$published_cmp" \
        || lex_less "$key_na" "$expires_cmp"; then
        rm -rf "$work_dir"
        error_exit "signed manifest lifetime is outside the pinned key's validity window (published_at ${published_at}, expires_at ${expires_at})" "verify" \
            "the signing key window ended or has not begun — re-download install.sh"
    fi
    if [ "$paused" = "true" ]; then
        rm -rf "$work_dir"
        error_exit "releases are PAUSED by the publisher (signed manifest paused=true)" "verify" \
            "an emergency stop is active — try again later or check github.com/libra-tools/libra"
    fi
    # Revoked versions are compared entry-by-entry in the same canonical
    # grammar as the version itself — no substring or format bypass.
    revoked_list=$(sed -n 's/.*"revoked_versions":\[\([^]]*\)\].*/\1/p' "$head_file" | head -n1)
    if [ -n "$revoked_list" ]; then
        old_ifs=$IFS
        IFS=','
        for revoked_entry in $revoked_list; do
            revoked_entry=${revoked_entry#\"}
            revoked_entry=${revoked_entry%\"}
            if ! is_canonical_semver "$revoked_entry"; then
                IFS=$old_ifs
                rm -rf "$work_dir"
                error_exit "signed manifest revoked_versions entry '${revoked_entry}' is not canonical X.Y.Z" "verify" "refusing to install"
            fi
            if [ "$revoked_entry" = "$STABLE_VERSION" ]; then
                IFS=$old_ifs
                rm -rf "$work_dir"
                error_exit "signed stable version ${STABLE_VERSION} is REVOKED by a newer control decision" "verify" \
                    "refusing to install a revoked build"
            fi
        done
        IFS=$old_ifs
    fi

    platform_key="${OS}-${ARCH}"
    artifact_row=$(sed -n "s/.*{\"platform\":\"${platform_key}\",\"url\":\"\\([^\"]*\\)\",\"sha256\":\"\\([0-9a-f]\\{64\\}\\)\",\"size\":\\([0-9]\\{1,9\\}\\)}.*/\\1 \\2 \\3/p" "$payload_file" | head -n1)
    rm -rf "$work_dir"
    if [ -z "$artifact_row" ]; then
        error_exit "signed manifest has no artifact for ${platform_key}" "verify" \
            "this platform is not in the release matrix"
    fi
    STABLE_URL=$(printf '%s' "$artifact_row" | awk '{print $1}')
    STABLE_SHA256=$(printf '%s' "$artifact_row" | awk '{print $2}')
    STABLE_SIZE=$(printf '%s' "$artifact_row" | awk '{print $3}')
    # Exact URL binding — origin, layout AND the tag derived from the signed
    # version. A signed row cannot point this version's install at another
    # tag's bytes, and prefix tricks under the pinned origin are impossible.
    if [ "$STABLE_URL" != "https://download.libra.tools/libra/releases/v${STABLE_VERSION}/libra-${platform_key}" ]; then
        error_exit "signed artifact URL does not match the pinned origin/version layout: $STABLE_URL" "verify" \
            "refusing to install"
    fi
    # Digest must be exactly 64 lowercase hex; size mirrors the native
    # (0, 128 MiB] bound — a signed zero-byte or oversized row is refused.
    printf '%s' "$STABLE_SHA256" | grep -qE '^[0-9a-f]{64}$' \
        || error_exit "signed manifest artifact sha256 is not 64 lowercase hex" "verify" "refusing to install"
    if [ -z "$STABLE_SIZE" ] || [ "$STABLE_SIZE" -le 0 ] || [ "$STABLE_SIZE" -gt 134217728 ]; then
        error_exit "signed manifest artifact size ${STABLE_SIZE:-?} is outside (0, 128 MiB]" "verify" "refusing to install"
    fi
}

# Explicit-confirm gate for the two transition states (manifest 404 and
# verifier unavailable). NEVER silent: requires an interactive yes or
# LIBRA_ALLOW_FALLBACK=1, otherwise the install stops with a clear message.
confirm_unverified_transition() {
    reason=$1
    if [ "${LIBRA_ALLOW_FALLBACK:-0}" = "1" ]; then
        warn_fact "signature" "$reason — proceeding UNVERIFIED (LIBRA_ALLOW_FALLBACK=1)"
        return 0
    fi
    if [ -t 0 ] && [ "$TTY" = "1" ]; then
        printf '  %s!%s %s%s%s\n' "$C_WARN" "$C_RESET" "$C_TEXT" "$reason" "$C_RESET"
        printf '  %sContinue with an UNVERIFIED download? [y/N]%s ' "$C_WARN" "$C_RESET"
        read -r answer
        case "$answer" in
            y|Y|yes|YES) warn_fact "signature" "$reason — user confirmed UNVERIFIED install"; return 0 ;;
        esac
    fi
    error_exit "$reason" "verify" \
        "no signed manifest verification is possible; set LIBRA_ALLOW_FALLBACK=1 (or confirm interactively) to opt in to an UNVERIFIED install"
}

# Resolve the install through the signed stable channel. On success sets
# INSTALL_VERIFIED=1 and VERSION. On a transition state (404 / no verifier),
# gates through confirm_unverified_transition and leaves INSTALL_VERIFIED=0.
resolve_stable_channel() {
    INSTALL_VERIFIED=0
    if ! manifest_verifier_available; then
        confirm_unverified_transition \
            "signature verifier unavailable (need openssl with Ed25519 support and a sha256 tool)"
        return 0
    fi
    MANIFEST_TMP=$(mktemp 2>/dev/null) \
        || error_exit "mktemp failed" "verify" "make sure \$TMPDIR is writable"
    manifest_status=$(fetch_stable_manifest "$MANIFEST_TMP")
    case "$manifest_status" in
        ok)
            verify_stable_manifest "$MANIFEST_TMP"
            rm -f "$MANIFEST_TMP"
            VERSION="v${STABLE_VERSION}"
            INSTALL_VERIFIED=1
            fact "signature" "stable manifest verified (Ed25519, key ${LIBRA_RELEASE_MANIFEST_KEY_ID})"
            ;;
        missing)
            rm -f "$MANIFEST_TMP"
            confirm_unverified_transition \
                "the auto-upgrade signature chain is not enabled yet (stable manifest does not exist)"
            ;;
        *)
            rm -f "$MANIFEST_TMP"
            error_exit "could not fetch the signed stable manifest" "verify" \
                "network problem reaching ${LIBRA_RELEASE_MANIFEST_ORIGIN} — retry; this is NOT the unsigned-fallback case"
            ;;
    esac
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
    if [ "$TTY" = "1" ]; then
        sleep 0.5 2>/dev/null || true
    fi
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
    if [ "${ALIAS_STATUS:-}" = "ready" ]; then
        agent_say "libra ${VERSION} is already installed at ${EXISTING_PATH}. The optional lba shorthand is ready too."
    else
        agent_say "libra ${VERSION} is already installed at ${EXISTING_PATH}. Nothing else to install."
    fi
    # The bootstrap re-run exists to write the marker; a failure here would
    # otherwise hide behind the normal success screen.
    if [ "${INSTALL_VERIFIED:-0}" = "1" ] && [ "${MARKER_WRITTEN:-0}" != "1" ]; then
        warn_fact "provenance" "upgrade management NOT enabled (the official-install marker was not written) — 'libra upgrade' will ask you to re-run this installer"
    fi

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
    if [ "${INSTALL_VERIFIED:-0}" = "1" ]; then
        # Signed path: the URL comes from the verified manifest. The download
        # goes through the pinned origin constant so the smoke harness can
        # redirect a COPY; in production this substitution is a no-op.
        download_url="${LIBRA_RELEASE_MANIFEST_ORIGIN}${STABLE_URL#https://download.libra.tools}"
    else
        download_url="${BASE_URL}/${VERSION}/${binary_name}"
    fi
    TEMP_DIR=$(mktemp -d 2>/dev/null) \
        || error_exit "mktemp failed" "install" "make sure mktemp is installed and \$TMPDIR is writable"
    temp_file="${TEMP_DIR}/${binary_name}"

    # Create LIBRA_HOME and INSTALL_DIR; both are under $HOME by default,
    # so this should never need elevated privileges.
    if ! mkdir -p "$LIBRA_HOME" "$INSTALL_DIR" 2>/dev/null; then
        error_exit "cannot create $INSTALL_DIR" "install" \
            "pick a writable path with LIBRA_HOME or -d (we never sudo)"
    fi

    fetcher=download_file
    [ "${INSTALL_VERIFIED:-0}" = "1" ] && fetcher=download_file_pinned
    run_step "fetch $binary_name" "$fetcher" "$download_url" "$temp_file" \
        || error_exit "download failed" "install" "url: $download_url"

    [ -s "$temp_file" ] || error_exit "downloaded file is empty" "install" "the mirror may be corrupted — please retry"

    if [ "${INSTALL_VERIFIED:-0}" = "1" ]; then
        # Signed path: size and sha256 come from the verified manifest and
        # are MANDATORY — any mismatch is fatal and nothing is installed.
        actual_size=$(wc -c <"$temp_file" 2>/dev/null | awk '{print $1}')
        if [ "$actual_size" != "$STABLE_SIZE" ]; then
            error_exit "size mismatch (signed manifest says $STABLE_SIZE bytes, got ${actual_size:-?})" "verify" \
                "refusing to install — the download origin may be compromised"
        fi
        actual_sha=$(sha256_of "$temp_file")
        if [ -z "$actual_sha" ] || [ "$actual_sha" != "$STABLE_SHA256" ]; then
            error_exit "sha256 mismatch against the SIGNED manifest (expected $STABLE_SHA256, got ${actual_sha:-none})" "verify" \
                "refusing to install — the download origin may be compromised"
        fi
        fact "checksum" "sha256 + size match the signed manifest"
    else
        verify_checksum "$temp_file" "$download_url"
    fi

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

    if [ "${INSTALL_VERIFIED:-0}" = "1" ]; then
        write_official_marker
    else
        # An unverified install must not sit next to a stale official marker.
        rm -f "${INSTALL_DIR}/.libra-official-install.json" 2>/dev/null || true
    fi

    INSTALLED_PATH="$target"
    ensure_lba_alias
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
    if [ "${ALIAS_STATUS:-}" = "ready" ]; then
        agent_say "Installed in about 30 seconds. You're all set — use libra normally, or the new lba shorthand."
    else
        agent_say "Installed in about 30 seconds. You're all set — here's what to try first:"
    fi
    # A verified install whose provenance marker could not be recorded is
    # working but NOT upgrade-manageable — say so where it cannot be missed.
    if [ "${INSTALL_VERIFIED:-0}" = "1" ] && [ "${MARKER_WRITTEN:-0}" != "1" ]; then
        warn_fact "provenance" "upgrade management NOT enabled (the official-install marker was not written) — 'libra upgrade' will ask you to re-run this installer"
    fi

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

    # Default path: the Ed25519-signed stable channel (UP-01 A1-05). An
    # explicit -v version or a custom mirror (LIBRA_BASE_URL) is an opt-in
    # UNVERIFIED path and is warned about loudly below.
    INSTALL_VERIFIED=0
    if [ -z "$VERSION" ] && [ -z "${LIBRA_BASE_URL:-}" ]; then
        resolve_stable_channel
    elif [ -n "$VERSION" ]; then
        warn_fact "signature" "-v pins a historic version: this path is NOT verified against the signed stable manifest"
    elif [ -n "${LIBRA_BASE_URL:-}" ]; then
        warn_fact "signature" "LIBRA_BASE_URL points at a custom mirror: this path is NOT verified against the signed stable manifest"
    fi

    if [ "$INSTALL_VERIFIED" = "0" ] && [ -z "$VERSION" ]; then
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
    # On the verified channel the existing binary must also HASH to the signed
    # manifest's digest — a self-reported version string alone is not proof
    # (a tampered binary can print any version it likes).
    if [ -n "$EXISTING_VERSION" ] && [ "$EXISTING_VERSION" = "$VERSION" ]; then
        skip_ok=1
        if [ "${INSTALL_VERIFIED:-0}" = "1" ]; then
            existing_sha=$(sha256_of "$EXISTING_PATH")
            existing_size=$(wc -c <"$EXISTING_PATH" 2>/dev/null | awk '{print $1}')
            if [ "$existing_sha" != "$STABLE_SHA256" ] || [ "$existing_size" != "$STABLE_SIZE" ]; then
                skip_ok=0
                warn_fact "verify" "installed ${EXISTING_VERSION} does not match the signed manifest digest — reinstalling"
            fi
        fi
        if [ "$skip_ok" = "1" ]; then
            # Bootstrap: the already-installed binary just matched the SIGNED
            # manifest digest, so (re)write the official marker — installs
            # made by older script versions carry none, and this no-op branch
            # is exactly where their re-run lands.
            [ "${INSTALL_VERIFIED:-0}" = "1" ] && write_official_marker
            # Re-running the installer repairs a missing/legacy alias even when
            # the binary itself does not need to be downloaded again.
            ensure_lba_alias
            screen_already_installed
            exit 0
        fi
    fi

    screen_method
    screen_install
    screen_shell
    screen_success
}

main "$@"
