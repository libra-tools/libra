#!/bin/sh
# NEXTEST_GROUPS.sh — mechanically derive .config/nextest.toml from
# tests/SERIAL_REGISTRY.tsv (plan-20260827 NP-01, ADR-NP-01).
#
# Group shape: one union group `external` (nextest test-groups are
# exclusive-membership — a test belongs to the first matching override
# only, so overlapping per-key groups cannot express compound rows).
# Members:
#   - every fn row whose lane key set contains any EXTERNAL key
#     (external = any named key outside the in-process closed set
#     {cwd, env, hash_kind} defined by the classifier's process model;
#     a newly named key is external by default — fail-safe: at worst it
#     over-serializes)                 -> filter = 'test(/(^|::)<fn>$/)'
#     (anchored regex, not test(=..): aggregated binaries nest modules, so
#     the full nextest name is e.g. command::blame_test::<fn>; fn names are
#     tree-unique — the registry guard rejects duplicates — so matching the
#     last path segment is exact. Over-match against a hypothetical
#     same-named non-serial test would only add it to the exclusion group,
#     which is the safe direction.)
#   - every pure-global site row's host target (whole test binary)
#                                     -> filter = 'binary(=<target>)'
# cwd/env/hash_kind never generate groups (in-process locks dissolve
# under one-process-per-test).
#
# usage: sh tests/NEXTEST_GROUPS.sh            # rewrite .config/nextest.toml
#        sh tests/NEXTEST_GROUPS.sh --stdout   # print to stdout (drift check)
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
REG="$ROOT/tests/SERIAL_REGISTRY.tsv"
OUT="$ROOT/.config/nextest.toml"
[ -f "$REG" ] || { echo "FAIL: $REG missing" >&2; exit 2; }

TMP=$(mktemp)
trap 'rm -f "$TMP"' EXIT

LC_ALL=C awk -F'\t' '
NR == 1 { next }
$1 ~ /^<site:/ {
    if ($2 == "global") {
        path = $1
        sub(/^<site:/, "", path); sub(/:.*$/, "", path)
        t = path
        sub(/^tests\//, "", t); sub(/\.rs$/, "", t)
        if (t !~ /^[A-Za-z0-9_]+$/) { print "BADTARGET\t" t; exit 3 }
        targets[t] = 1
    }
    next
}
$2 ~ /^lane:/ {
    keys = $2
    sub(/^lane:/, "", keys)
    n = split(keys, arr, "+")
    ext = 0
    for (i = 1; i <= n; i++)
        if (arr[i] != "cwd" && arr[i] != "env" && arr[i] != "hash_kind") ext = 1
    if (!ext) next
    if ($1 !~ /^[A-Za-z0-9_]+$/) { print "BADFN\t" $1; exit 3 }
    fns[$1] = 1
}
END {
    for (f in fns)      print "F\t" f
    for (t in targets)  print "B\t" t
}
' "$REG" | LC_ALL=C sort > "$TMP"

if grep -q "^BAD" "$TMP"; then
    echo "FAIL: unexpected identifier in registry:" >&2
    grep "^BAD" "$TMP" >&2
    exit 2
fi

emit() {
    printf '%s\n' "# generated — do not edit"
    printf '%s\n' "# regenerate: sh tests/NEXTEST_GROUPS.sh  (source of truth: tests/SERIAL_REGISTRY.tsv)"
    printf '%s\n' "# plan-20260827 NP-01 / ADR-NP-01: union external-resource mutual-exclusion group."
    printf '%s\n' "# plan-20260827 NP-02: profiles carry runner behavior only (groups/threads/junit);"
    printf '%s\n' "# Cargo features and env always travel with the command line."
    printf '\n%s\n%s\n' "[test-groups.external]" "max-threads = 1"
    printf '\n%s\n%s\n' "[profile.default.junit]" 'path = "junit.xml"'
    printf '\n%s\n' "# test-provider scenario runs (base.yml provider section): overrides are"
    printf '%s\n' "# inherited from profile.default. test-threads = 1 preserves the CI"
    printf '%s\n' "# section's --test-threads=1 semantic verbatim: provider scenario tests"
    printf '%s\n' "# are calibrated for single-threaded timing (lease-release sleeps)."
    printf '\n%s\n%s\n%s\n' "[profile.test-provider]" "test-threads = 1" 'junit = { path = "junit.xml" }'
    LC_ALL=C sort "$TMP" | while IFS="$(printf '\t')" read -r kind name; do
        printf '\n%s\n' "[[profile.default.overrides]]"
        if [ "$kind" = "F" ]; then
            printf "filter = 'test(/(^|::)%s$/)'\n" "$name"
        else
            printf "filter = 'binary(=%s)'\n" "$name"
        fi
        printf '%s\n' "test-group = 'external'"
    done
}

if [ "${1:-}" = "--stdout" ]; then
    emit
else
    mkdir -p "$ROOT/.config"
    emit > "$OUT"
    fn_n=$(grep -c "^filter = 'test(/" "$OUT")
    bin_n=$(grep -c "^filter = 'binary(=" "$OUT")
    echo "wrote $OUT (external group: $fn_n test filters + $bin_n binary filters)"
fi
