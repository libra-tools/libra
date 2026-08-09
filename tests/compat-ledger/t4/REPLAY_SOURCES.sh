#!/bin/sh
# plan-20260729 CT3-06 — the source replayer, and the ONLY implementation of
# GC-14's replay protocol. CT3-04's GATES.sh and CT3-02's PROVENANCE gate both
# call this; neither reimplements it.
#
# Every oracle value recorded in the migration (`expected_from` in
# PROVENANCE.md, `git_expected` in DEFECTS.md) carries the three-field source
#
#     <source_kind>|<source_anchor>|<probe_digest>
#
# and this script proves, for each one, that replaying the source today still
# yields that digest. Without it a recorded expectation is just an assertion
# about the past that nobody can check.
#
# The security posture matters more than the mechanism. A source line is
# attacker-influenced data: it names a file to read or an argv to run. So
# paths are constrained to tracked repository-relative locations with no `.`
# or `..` component, upstream content is read from the pinned blob rather than
# the working tree, and probes must match a frozen allowlist of read-only
# argvs whose every option is on a whitelist. A blacklist was tried and is not
# enough — `git grep -O<prog>` is a short alias for --open-files-in-pager and
# launches an arbitrary program while satisfying every naive check.
#
# Usage: SOURCES_FILE=<file> GD=<scratch dir> sh REPLAY_SOURCES.sh
#   SOURCES_FILE  one `kind|anchor|digest` per line
#   GD            a run-scoped scratch directory the caller owns
#   GRIT_REPO     the pinned grit checkout (needed by grit-doc-anchor/git-probe)
set -eu

PIN=dfb079967b9cbc99e533c21e65f674bb3f5e8b07
READONLY_SUBCOMMANDS=" show cat-file rev-parse rev-list log ls-tree ls-files grep describe diff-tree "
ALLOWED_OPTS=" -- -n --name-only --no-color --no-pager --numstat --raw --stat --format=%H \
  --pretty=oneline --abbrev-commit --no-renames --full-index --text -l -L "

: "${SOURCES_FILE:?set SOURCES_FILE to the file of sources to replay}"
: "${GD:?set GD to a scratch directory}"
[ -f "$SOURCES_FILE" ] || { echo "FAIL: no such SOURCES_FILE: $SOURCES_FILE"; exit 1; }
[ -d "$GD" ] || { echo "FAIL: GD is not a directory: $GD"; exit 1; }

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
[ -f "$ROOT/COMPATIBILITY.md" ] || { echo "FAIL: cannot locate the repository root"; exit 1; }

# Failures go to stderr: three of the four replay paths run inside a command
# substitution, and a message on stdout would be captured into the variable
# instead of reaching the caller, who would then see a digest mismatch
# rather than the real reason.
die() { echo "FAIL: $1" >&2; exit 1; }

# A repository-relative path with no `.` or `..` component. `docs/../Cargo.toml`
# passes a naive prefix test and `ls-files --error-unmatch`, so the components
# are checked one by one rather than by pattern.
check_path() {
  _p=$1; _allow=$2
  case "$_p" in
    /*) die "source_anchor path must be repo-relative, not absolute: $_p" ;;
  esac
  _rest=$_p
  while [ -n "$_rest" ]; do
    case "$_rest" in
      */*) _seg=${_rest%%/*}; _rest=${_rest#*/} ;;
      *)   _seg=$_rest; _rest= ;;
    esac
    case "$_seg" in
      .|..) die "source_anchor path must not contain . or .. components: $_p" ;;
      '')   die "source_anchor path has an empty component: $_p" ;;
    esac
  done
  # Each caller passes the prefixes its kind permits.
  for _pref in $_allow; do
    case "$_p" in
      "$_pref"|"$_pref"/*) return 0 ;;
    esac
  done
  die "source_anchor path is outside the permitted roots ($_allow): $_p"
}

digest_of_line() {   # $1 = file, $2 = 1-based line number -> stdout: sha256
  sed -n "${2}p" "$1" > "$GD/_line.raw" || die "cannot read line $2 of $1"
  [ -s "$GD/_line.raw" ] || die "line $2 of $1 is empty or absent"
  sed -e 's/[[:space:]]*$//' "$GD/_line.raw" > "$GD/_line.trim" || die "cannot trim line $2"
  _d=$(shasum -a 256 "$GD/_line.trim") || die "cannot hash line $2 of $1"
  printf '%s' "${_d%% *}"
}

replay_libra_doc_anchor() {   # $1 = <file>:<line>
  _file=${1%:*}; _line=${1##*:}
  case "$_line" in ''|*[!0-9]*) die "source_anchor must end in :<line>: $1" ;; esac
  check_path "$_file" "COMPATIBILITY.md docs"
  ( cd "$ROOT" && libra ls-files --error-unmatch -- "$_file" >/dev/null 2>&1 ) \
    || die "source_anchor names an untracked file: $_file"
  [ -f "$ROOT/$_file" ] || die "source_anchor names a missing file: $_file"
  digest_of_line "$ROOT/$_file" "$_line"
}

replay_grit_doc_anchor() {    # $1 = <file>:<line>, always read from the pin
  _file=${1%:*}; _line=${1##*:}
  case "$_line" in ''|*[!0-9]*) die "source_anchor must end in :<line>: $1" ;; esac
  check_path "$_file" "tests"
  : "${GRIT_REPO:?set GRIT_REPO to the pinned grit checkout}"
  git -C "$GRIT_REPO" show "$PIN:$_file" > "$GD/_pin.blob" 2>/dev/null \
    || die "source_anchor is not present at the pin: $_file"
  digest_of_line "$GD/_pin.blob" "$_line"
}

replay_git_probe() {          # $1 = a complete argv
  _argv=$1
  : "${GRIT_REPO:?set GRIT_REPO to the pinned grit checkout}"
  case "$_argv" in *"$PIN"*) : ;; *) die "probe argv does not pin the revision: $_argv" ;; esac
  printf '%s' "$_argv" | tr -d 'A-Za-z0-9 ._/=:^~@+-' > "$GD/_meta" || die "tr on probe argv"
  [ ! -s "$GD/_meta" ] || die "probe argv has characters outside the allowed set: $_argv"
  # Shape first, membership second. A `reset --hard` or a `--output=` must be
  # named for what it is; reporting only "not in the allowlist" would hide the
  # fact that the argv was dangerous, not merely unapproved.
  # shellcheck disable=SC2086
  set -- $_argv
  case "$READONLY_SUBCOMMANDS" in
    *" $1 "*) : ;;
    *) die "probe subcommand is not in the read-only closed set: $1" ;;
  esac
  _sub=$1; shift
  for _tok in "$@"; do
    case "$_tok" in
      -*) case "$ALLOWED_OPTS" in
            *" $_tok "*) : ;;
            *) die "probe carries a disallowed option: $_tok" ;;
          esac ;;
    esac
  done
  # And it must be one of the frozen, approved probes — verbatim.
  _allowfile=$(dirname -- "$0")/PROBES.allow
  [ -s "$_allowfile" ] || die "PROBES.allow is missing or empty"
  _hit=0
  while IFS= read -r _line || [ -n "$_line" ]; do
    [ "$_line" = "$_argv" ] && _hit=1
  done < "$_allowfile"
  [ "$_hit" -eq 1 ] || die "probe argv is not in the frozen PROBES.allow: $_argv"
  # The corpus must still be at the pin and clean, or the probe measures
  # something other than what was recorded.
  _head=$(git -C "$GRIT_REPO" rev-parse HEAD) || die "cannot read grit HEAD"
  [ "$_head" = "$PIN" ] || die "grit HEAD $_head is not the pin"
  _st=$(git -C "$GRIT_REPO" status --porcelain -- tests) || die "cannot read grit status"
  [ -z "$_st" ] || die "grit tests/ is dirty; the probe would not be reproducible"
  # A hermetic environment: no user config, no pager, no prompts, no stdin.
  # Local aliases are not a hole here: git refuses to let an alias shadow a
  # builtin, and every subcommand in the read-only closed set is one —
  # verified by setting alias.show in a scratch repository and watching the
  # builtin run anyway. A non-builtin alias does fire, which is exactly why
  # the closed set is a whitelist of builtins rather than a blacklist.
  env -i PATH=/usr/bin:/bin HOME=/nonexistent LC_ALL=C GIT_CONFIG_NOSYSTEM=1 \
    GIT_CONFIG_GLOBAL=/dev/null GIT_PAGER=cat GIT_TERMINAL_PROMPT=0 \
    git -C "$GRIT_REPO" "$_sub" "$@" > "$GD/_probe.out" 2>/dev/null < /dev/null \
    || die "the approved probe failed to run: $_argv"
  _d=$(shasum -a 256 "$GD/_probe.out") || die "cannot hash the probe output"
  printf '%s' "${_d%% *}"
}

n=0
while IFS= read -r src || [ -n "$src" ]; do
  [ -n "$src" ] || continue
  case "$src" in \#*) continue ;; esac
  n=$((n + 1))
  kind=${src%%|*}
  rest=${src#*|}
  anchor=${rest%|*}
  digest=${rest##*|}
  [ "$kind" != "$src" ] && [ "$anchor" != "$rest" ] \
    || die "source line $n is not <source_kind>|<source_anchor>|<probe_digest>: $src"
  case "$digest" in
    *[!0-9a-f]*|'') die "source line $n has a non-hex probe_digest: $digest" ;;
  esac
  [ "${#digest}" -eq 64 ] || die "source line $n: probe_digest is not 64 hex characters"

  case "$kind" in
    libra-doc-anchor) actual=$(replay_libra_doc_anchor "$anchor") ;;
    grit-doc-anchor)  actual=$(replay_grit_doc_anchor "$anchor") ;;
    git-probe)        actual=$(replay_git_probe "$anchor") ;;
    *) die "unknown source_kind '$kind' on line $n" ;;
  esac
  [ "$actual" = "$digest" ] \
    || die "replay digest mismatch on line $n ($kind $anchor): $actual != $digest"
done < "$SOURCES_FILE"

[ "$n" -ge 1 ] || die "no sources to replay in $SOURCES_FILE"
echo "OK: replayed $n source(s)"
