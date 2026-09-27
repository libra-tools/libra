#!/bin/sh
set -eu

repo_root=${1:?usage: install_alias_smoke.sh <repo-root>}
installer="$repo_root/install.sh"
version=v0.18.88
system_path=/usr/bin:/bin:/usr/sbin:/sbin
work=$(mktemp -d "${TMPDIR:-/tmp}/libra-install-alias.XXXXXX")
trap 'rm -rf "$work"' 0 HUP INT TERM

fail() {
    printf 'install alias smoke: %s\n' "$1" >&2
    if [ -n "${last_log:-}" ] && [ -f "$last_log" ]; then
        printf '%s\n' '--- installer output ---' >&2
        sed 's/^/  /' "$last_log" >&2
    fi
    exit 1
}

sh -n "$installer" || fail "install.sh is not valid POSIX shell syntax"

fake_bin="$work/fake-bin"
mkdir -p "$fake_bin"
fake_libra="$work/libra-fixture"
cat >"$fake_libra" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
    printf 'libra 0.18.88\n'
    exit 0
fi
printf 'fixture libra\n'
EOF
chmod +x "$fake_libra"

cat >"$fake_bin/curl" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "--version" ]; then
    printf 'curl 8.0.0 fixture\n'
    exit 0
fi

url=
out=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o)
            [ "$#" -ge 2 ] || exit 2
            out=$2
            shift 2
            ;;
        http://*|https://*)
            url=$1
            shift
            ;;
        *)
            shift
            ;;
    esac
done

case "$url" in
    *.sha256) exit 22 ;;
    '') exit 2 ;;
esac
[ -n "$out" ] || exit 2
cp "$FAKE_LIBRA_SOURCE" "$out"
EOF
chmod +x "$fake_bin/curl"

help_out="$work/help.out"
HOME="$work/help-home" NO_COLOR=1 \
    sh "$installer" --help >"$help_out" 2>&1 || fail "--help failed"
grep -q -e '--no-alias' "$help_out" || fail "--help does not list --no-alias"
grep -q 'LIBRA_NO_ALIAS=1' "$help_out" || fail "--help does not list LIBRA_NO_ALIAS=1"

run_installer() {
    case_name=$1
    no_alias=$2
    shift 2
    root="$work/$case_name"
    home="$root/home"
    install_dir="$home/.libra/bin"
    mkdir -p "$home" "$install_dir"
    last_log="$root/install.log"
    if ! env \
        HOME="$home" \
        LIBRA_HOME="$home/.libra" \
        LIBRA_INSTALL_DIR="$install_dir" \
        LIBRA_BASE_URL="https://fixture.invalid/releases" \
        LIBRA_NO_ALIAS="$no_alias" \
        NO_COLOR=1 \
        FAKE_LIBRA_SOURCE="$fake_libra" \
        PATH="${RUN_PATH:-$fake_bin:$system_path}" \
        sh "$installer" -v "$version" --no-modify-path "$@" >"$last_log" 2>&1
    then
        fail "$case_name installer invocation failed"
    fi
}

assert_relative_alias() {
    root=$1
    install_dir="$work/$root/home/.libra/bin"
    [ -L "$install_dir/lba" ] || fail "$root did not create an lba symlink"
    [ "$(readlink "$install_dir/lba")" = "libra" ] \
        || fail "$root lba target is not the relative name 'libra'"
    libra_version=$("$install_dir/libra" --version)
    lba_version=$("$install_dir/lba" --version)
    [ "$libra_version" = "$lba_version" ] \
        || fail "$root lba --version differs from libra --version"
}

# Clean install and an unchanged rerun are both idempotent.
run_installer clean 0
assert_relative_alias clean
run_installer clean 0
assert_relative_alias clean

# The same-version early return must repair a missing alias without downloading
# or replacing the already-installed binary.
clean_dir="$work/clean/home/.libra/bin"
printf '%s\n' '# preserve-same-version-marker' >>"$clean_dir/libra"
clean_hash_before=$(cksum "$clean_dir/libra")
rm "$clean_dir/lba"
run_installer clean 0
assert_relative_alias clean

# Opting out on a later run does not delete an already-created Libra alias.
run_installer clean 0 --no-alias
assert_relative_alias clean
grep -q 'already installed' "$last_log" || fail "missing-alias repair did not use same-version path"
clean_hash_after=$(cksum "$clean_dir/libra")
[ "$clean_hash_before" = "$clean_hash_after" ] || fail "same-version alias repair replaced libra"

# An accepted absolute alias is refreshed to the movable relative target.
rm "$clean_dir/lba"
ln -s "$clean_dir/libra" "$clean_dir/lba"
run_installer clean 0
assert_relative_alias clean

# Both opt-out controls leave no alias behind.
run_installer no_alias_flag 0 --no-alias
flag_dir="$work/no_alias_flag/home/.libra/bin"
if [ -e "$flag_dir/lba" ] || [ -L "$flag_dir/lba" ]; then
    fail "--no-alias still created lba"
fi

run_installer no_alias_env 1
env_dir="$work/no_alias_env/home/.libra/bin"
if [ -e "$env_dir/lba" ] || [ -L "$env_dir/lba" ]; then
    fail "LIBRA_NO_ALIAS=1 still created lba"
fi

# A user-owned regular file must survive a clean Libra install byte-for-byte.
regular_dir="$work/foreign_regular/home/.libra/bin"
mkdir -p "$regular_dir"
printf 'user-owned-lba\n' >"$regular_dir/lba"
regular_before=$(cksum "$regular_dir/lba")
run_installer foreign_regular 0
regular_after=$(cksum "$regular_dir/lba")
[ "$regular_before" = "$regular_after" ] || fail "foreign regular lba was overwritten"
[ ! -L "$regular_dir/lba" ] || fail "foreign regular lba became a symlink"
grep -q 'leaving it unchanged' "$last_log" || fail "foreign regular lba emitted no warning"

# A directory collision is user-owned too, including its contents.
directory_path="$work/foreign_directory/home/.libra/bin/lba"
mkdir -p "$directory_path"
printf 'keep-directory-content\n' >"$directory_path/owned.txt"
run_installer foreign_directory 0
[ -d "$directory_path" ] || fail "foreign lba directory was replaced"
grep -q 'keep-directory-content' "$directory_path/owned.txt" \
    || fail "foreign lba directory content was changed"
grep -q 'leaving it unchanged' "$last_log" || fail "foreign lba directory emitted no warning"

# A symlink to another command is also user-owned and must not be repointed.
foreign_root="$work/foreign_symlink/home/.libra"
foreign_dir="$foreign_root/bin"
mkdir -p "$foreign_dir"
printf '#!/bin/sh\nexit 0\n' >"$foreign_root/custom-lba"
chmod +x "$foreign_root/custom-lba"
ln -s ../custom-lba "$foreign_dir/lba"
run_installer foreign_symlink 0
[ "$(readlink "$foreign_dir/lba")" = "../custom-lba" ] \
    || fail "foreign lba symlink was repointed"
grep -q 'leaving it unchanged' "$last_log" || fail "foreign lba symlink emitted no warning"

# Broken foreign symlinks are still symlinks and must never be replaced.
broken_dir="$work/foreign_broken_symlink/home/.libra/bin"
mkdir -p "$broken_dir"
ln -s ../missing-user-command "$broken_dir/lba"
run_installer foreign_broken_symlink 0
[ "$(readlink "$broken_dir/lba")" = "../missing-user-command" ] \
    || fail "broken foreign lba symlink was repointed"
grep -q 'leaving it unchanged' "$last_log" || fail "broken foreign symlink emitted no warning"

# Command substitution must not collapse a foreign target ending in a newline
# into the accepted exact target "libra".
newline_dir="$work/foreign_newline_symlink/home/.libra/bin"
mkdir -p "$newline_dir"
newline_target_with_sentinel=$(printf 'libra\n_')
newline_target=${newline_target_with_sentinel%_}
ln -s "$newline_target" "$newline_dir/lba"
run_installer foreign_newline_symlink 0
# GNU readlink prints a terminator while Darwin's does not, so compare the raw
# link bytes. The sentinel also preserves the target's trailing newline across
# command substitution.
newline_actual_with_sentinel=$(
    readlink -n "$newline_dir/lba" 2>/dev/null || true
    printf '_'
)
newline_actual=${newline_actual_with_sentinel%_}
[ "$newline_actual" = "$newline_target" ] \
    || fail "newline-suffixed foreign lba symlink was repointed"
grep -q 'leaving it unchanged' "$last_log" || fail "newline foreign symlink emitted no warning"

# A platform/filesystem that rejects symlinks still gets a successful libra
# installation and an actionable warning.
no_symlink_bin="$work/no-symlink-bin"
mkdir -p "$no_symlink_bin"
cat >"$no_symlink_bin/ln" <<'EOF'
#!/bin/sh
last=
for arg in "$@"; do
    last=$arg
done
case "$last" in
    */lba) exit 1 ;;
esac
exec /bin/ln "$@"
EOF
chmod +x "$no_symlink_bin/ln"
RUN_PATH="$no_symlink_bin:$fake_bin:$system_path" run_installer no_symlink 0
no_symlink_dir="$work/no_symlink/home/.libra/bin"
[ -x "$no_symlink_dir/libra" ] || fail "symlink failure prevented libra installation"
if [ -e "$no_symlink_dir/lba" ] || [ -L "$no_symlink_dir/lba" ]; then
    fail "symlink failure left an unexpected lba path"
fi
grep -q 'symlinks may be unavailable' "$last_log" || fail "symlink failure emitted no fallback warning"

if command -v shellcheck >/dev/null 2>&1; then
    shellcheck "$installer" "$repo_root/tests/compat/install_alias_smoke.sh" \
        || fail "shellcheck reported an installer regression"
else
    printf '%s\n' 'install alias smoke: shellcheck not installed; syntax and behavioral tests still ran'
fi

printf '%s\n' 'install alias smoke: ok'
