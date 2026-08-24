#!/bin/sh
# tests/SERIAL_CLASSIFY.sh — classify every `#[serial]`-marked test by WHY it needs
# exclusion, so the ones that need none can go back to running in parallel.
#
# Output: one `<test_fn>\t<verdict>` line per serial-marked test, sorted, where
# verdict is drawn from a closed set:
#
#   global            fail-closed only: the body could not be delimited, or the
#                     attribute sits inside a `macro_rules!` body
#   lane:env          mutates process-wide environment (`set_var`/`remove_var`)
#   lane:cwd          changes the process working directory
#   lane:hash_kind    sets the process-wide hash kind
#   lane:<key>(+<key>)*  one lane per matched process-wide resource (`serial_test`
#                        supports multiple keys, so mixed cases keep every
#                        exclusion, e.g. `lane:env+cwd`, `lane:hash_kind+cwd`);
#                        a test free of process-wide pollution keeps its own key
#   none              only spawns subprocesses with an explicit cwd, and uses tempdirs
#
# Attributes inside a `macro_rules!` body cannot be attributed to one function;
# they are emitted as `<site:<path>:<line>>` rows judged `global` (fail-closed).
#
# Judgement is by resource set: every matched process-wide resource contributes
# one lane (`env` / `hash_kind` / `cwd`), and the attribute's own key(s) are
# parsed, deduplicated and merged — a mixed env+cwd case keeps both lanes.
# NOTE: serial_test's unkeyed `#[serial]` locks only the empty-string key and
# is NOT exclusive with named lanes, so `global` rows must expand to the full
# resource key set at conversion time — see plan-20260729 S2/DEFER-09.
#
# Scanning is string/comment-aware: comments and string literals (normal, raw,
# byte/C strings, char literals) are blanked before matching, so a `#[serial]`
# inside text never produces a row, and `#[test] #[serial]` on one line is read.
#
# HEURISTIC, NOT PROOF: a `none` verdict only means the delimited function body
# does not textually contain a small blacklist of process-wide calls. Helpers
# called from the body are NOT expanded, so a `none` verdict is a deletion
# CANDIDATE only — mechanical removal waits for the strengthened classifier
# (helper expansion or unknown-call-is-global fallback), see
# docs/development/plan/plan-20260729.md DEFER-09. A wrong `global` costs a slow
# test; a wrong `none` costs a flaky suite, which is why deletion is gated.
#
# Why `none` is safe at all — three facts about this repository:
#   * `run_libra_command(args, cwd)` sets `.current_dir(cwd)` on the CHILD process
#     (`tests/command/mod.rs`), so it never touches parent state;
#   * process-wide cwd exclusion is already held by a reentrant `CWD_LOCK` inside
#     `ChangeDirGuard` (`src/utils/test.rs`), not by `#[serial]`;
#   * only a handful of test files actually call `set_var`.
set -eu
ROOT="${SERIAL_CLASSIFY_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$ROOT" || { echo "FAIL: cannot reach the repository root" >&2; exit 2; }
[ -f COMPATIBILITY.md ] && { [ -d .libra ] || [ -e .git ]; } || { echo "FAIL: not at the repository root" >&2; exit 2; }

python3 - <<'CLASSIFY_PY'
import os, re, sys

ATTR_START = re.compile(r'#\[(?:serial_test::)?serial')
FN   = re.compile(r'^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)')
FN_INLINE = re.compile(r'\bfn\s+([A-Za-z_][A-Za-z0-9_]*)')
RAW_STR = re.compile(r'(?:[bc]?r)(#*)"')
CHAR_LIT = re.compile(r"'(?:\\(?:[nrt0\\'\"]|x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\})|[^\\'])'")

CONFIG_ARG = re.compile(r'(?:inner_attrs|crate)\s*=')

def parse_keys(text):
    """Split an attribute argument list on top-level commas (depth-aware),
    dropping `inner_attrs = [...]` and `crate = <path>` config segments —
    those are not lock keys."""
    parts, cur, depth = [], [], 0
    for ch in text:
        if ch in '([':
            depth += 1
        elif ch in ')]':
            depth -= 1
        if ch == ',' and depth == 0:
            parts.append(''.join(cur))
            cur = []
        else:
            cur.append(ch)
    parts.append(''.join(cur))
    keys = []
    for p in parts:
        p = p.strip()
        if not p or CONFIG_ARG.match(p):
            continue          # config segments are not lock keys
        keys.append(p)
    return keys

def read_attr_keys(code, i, start):
    """Read a `#[serial...]` attribute whose `#[` is at code[i][start], balanced
    across lines. Returns (end_line, end_col, keys); keys is None when the
    attribute cannot be balanced (caller must fail closed)."""
    m = ATTR_START.match(code[i], start)
    j = m.end()
    li = i
    while True:
        line = code[li]
        while j < len(line) and line[j] in ' \t':
            j += 1
        if j < len(line):
            break
        li += 1
        if li >= len(code):
            return li, j, None
        j = 0
    line = code[li]
    if line[j] != '(':
        # bare form must close with `]` right here, otherwise the attribute is
        # malformed — fail closed instead of treating it as keyless
        if line[j] == ']':
            return li, j + 1, []
        return li, j, None
    depth = 0
    stack = []
    col = j
    inner = []
    while li < len(code):
        line = code[li]
        while col < len(line):
            ch = line[col]
            if ch in '([':
                stack.append(ch)
                if len(stack) > 1:
                    inner.append(ch)
            elif ch in ')]':
                want = '(' if ch == ')' else '['
                if not stack or stack[-1] != want:
                    return li, col, None      # mismatched delimiter: fail closed
                stack.pop()
                if not stack:
                    # outer `(...)` is closed — the attribute must end with `]`,
                    # skipping any amount of whitespace across lines
                    k = col + 1
                    while True:
                        line2 = code[li]
                        while k < len(line2) and line2[k] in ' \t':
                            k += 1
                        if k < len(line2):
                            break
                        li += 1
                        if li >= len(code):
                            return li, k, None
                        k = 0
                    if code[li][k] == ']':
                        return li, k + 1, parse_keys(''.join(inner))
                    return li, col, None
                inner.append(ch)
            else:
                inner.append(ch)
            col += 1
        li += 1
        col = 0
        inner.append(' ')
    return li, col, None

# Helpers that pull process-wide state in on the caller's behalf. NOTE: only
# the delimited function body is scanned — helper bodies are NOT expanded
# (heuristic, see plan-20260729 DEFER-09).
GLOBAL_CALLS = ('set_var', 'remove_var')
CWD_CALLS    = ('ChangeDirGuard', 'set_current_dir')
HASH_CALLS   = ('set_hash_kind',)

def code_only(lines):
    """Blank comments and string literals, preserving columns and line count,
    so attribute/fn matching never sees text inside strings or comments."""
    out = []
    block_comment = 0      # nested /* */ depth
    in_string = False      # normal "..." (also b"..." / c"...") with escapes
    raw_hashes = None      # inside r#*"..."#* with this many '#'
    for line in lines:
        buf = list(line)
        i, n = 0, len(line)
        while i < n:
            if raw_hashes is not None:
                if line[i] == '"' and line.startswith('#' * raw_hashes, i + 1):
                    for k in range(1 + raw_hashes):
                        buf[i + k] = ' '
                    i += 1 + raw_hashes
                    raw_hashes = None
                    continue
                buf[i] = ' '
                i += 1
                continue
            if in_string:
                if line[i] == '\\':
                    buf[i] = ' '
                    if i + 1 < n:
                        buf[i + 1] = ' '
                    i += 2
                    continue
                buf[i] = ' '
                if line[i] == '"':
                    in_string = False
                i += 1
                continue
            if block_comment > 0:
                if line.startswith('/*', i):
                    buf[i] = buf[i + 1] = ' '
                    block_comment += 1
                    i += 2
                    continue
                if line.startswith('*/', i):
                    buf[i] = buf[i + 1] = ' '
                    block_comment -= 1
                    i += 2
                    continue
                buf[i] = ' '
                i += 1
                continue
            # code state
            if line.startswith('//', i):
                for k in range(i, n):
                    buf[k] = ' '
                break
            if line.startswith('/*', i):
                buf[i] = buf[i + 1] = ' '
                block_comment += 1
                i += 2
                continue
            cm = CHAR_LIT.match(line, i)
            if cm:
                for k in range(cm.end() - i):
                    buf[i + k] = ' '
                i = cm.end()
                continue
            rm = RAW_STR.match(line, i)
            if rm:
                for k in range(len(rm.group(0))):
                    buf[i + k] = ' '
                raw_hashes = len(rm.group(1))
                i += len(rm.group(0))
                continue
            if line[i] == '"' or line.startswith(('b"', 'c"'), i):
                if line[i] in 'bc':
                    buf[i] = ' '
                    i += 1
                buf[i] = ' '
                i += 1
                in_string = True
                continue
            i += 1
        out.append(''.join(buf))
    return out

rows = []
for root, dirs, files in os.walk('tests'):
    dirs[:] = [d for d in dirs if d not in ('data', 'fixtures')]
    for name in sorted(files):
        if not name.endswith('.rs'):
            continue
        path = os.path.join(root, name)
        lines = open(path, encoding='utf-8', errors='replace').read().split('\n')
        code = code_only(lines)
        for i, cline in enumerate(code):
            pos = 0
            while True:
                m = ATTR_START.search(cline, pos)
                if not m:
                    break
                tail = cline[m.end():].lstrip()
                if tail and not tail.startswith(('(', ']')):
                    pos = m.end()
                    continue          # not the attribute (identifier prefix)
                end_li, end_col, keys = read_attr_keys(code, i, m.start())
                if keys is None:
                    rows.append(('<site:%s:%d>' % (path, i + 1), 'global'))
                    break
                fm = FN_INLINE.search(code[end_li], end_col)
                same_line = fm is not None
                j = end_li + 1
                while fm is None and j < len(code):
                    fm = FN.match(code[j])
                    if fm is None:
                        nxt = code[j].strip()
                        if nxt == '' or nxt.startswith('#['):
                            j += 1
                            continue
                        break
                if fm is None:
                    rows.append(('<site:%s:%d>' % (path, i + 1), 'global'))
                else:
                    fn = fm.group(1)
                    # body: brace-matched from the signature over code-only lines
                    depth = 0; seen = False; closed = False; body = []
                    k = i if same_line else j
                    while k < len(code):
                        seg = code[k][fm.start():] if (same_line and k == i) else code[k]
                        depth += seg.count('{') - seg.count('}')
                        if '{' in seg:
                            seen = True
                        body.append(seg)
                        if seen and depth <= 0:
                            closed = True
                            break
                        k += 1
                    text = '\n'.join(body)
                    # one lane per matched process-wide resource — serial_test
                    # supports multiple keys, so mixed cases keep every exclusion;
                    # env pollution is the composable `env` lane, not a short-circuit
                    parts = []
                    if any(c in text for c in GLOBAL_CALLS):
                        parts.append('env')
                    if any(c in text for c in HASH_CALLS):
                        parts.append('hash_kind')
                    if any(c in text for c in CWD_CALLS):
                        parts.append('cwd')
                    for k in keys:
                        if k and k not in parts:
                            parts.append(k)
                    if not closed:
                        verdict = 'global'      # could not delimit a balanced body: fail closed
                    else:
                        # a named key set that does not cover the body's own
                        # process-wide pollution is an insufficient lock — the
                        # runtime would lock only the named key(s) while the
                        # pollution escapes. Reject it instead of blessing a
                        # composite lane the source attribute cannot provide.
                        uncovered = [p for p in parts if p in ('env', 'hash_kind', 'cwd') and p not in keys]
                        if keys and uncovered:
                            print("FAIL: %s (%s): named key(s) %s do not cover process-wide pollution lane(s) %s — make the attribute unkeyed or remove the pollution" % (
                                fn, path, '+'.join(keys), '+'.join(uncovered)), file=sys.stderr)
                            sys.exit(3)
                        if parts:
                            verdict = 'lane:' + '+'.join(parts)
                        else:
                            verdict = 'none'
                    rows.append((fn, verdict))
                if end_li == i:
                    pos = end_col if end_col > m.end() else m.end()
                else:
                    break

rows.sort()
for fn, v in rows:
    print('%s\t%s' % (fn, v))
CLASSIFY_PY
