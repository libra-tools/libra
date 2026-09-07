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
# they are emitted as CONTENT-ANCHORED rows judged `global` (fail-closed):
# `<site:<path>:macro:<macro_name>#<ordinal>>` inside a macro_rules! body,
# `<site:<path>:orphan#<ordinal>>` otherwise — never line numbers (TA-02).
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
# FAIL-CLOSED ALLOWLIST (plan-20260825 TA-01): a `none` verdict now means the
# delimited function body's ENTIRE call surface is proven pollution-free:
# every macro / method / free / path call must be (a) on the explicit
# allowlist below (each entry carries a why-safe reason), (b) a known
# process-wide API (it contributes a lane instead), (c) a same-file or
# shared/visible fn whose body passes the same check under BOUNDED
# transitive expansion (identity-keyed stacks; cycles, depth-cap overruns,
# and ambiguous resolutions all fail closed), or
# (d) an uppercase constructor/variant (Rust convention: construction does
# not touch process-global state; the pollution APIs here are snake_case,
# and `ChangeDirGuard` is caught by the blacklist substring scan first).
# Anything else — unknown helpers, recursive helpers, cross-crate calls,
# ambiguous (duplicate) fn names, unknown macros — is judged `global`
# (fail-closed). Expansion also PROPAGATES lanes: a body calling a
# helper that holds ChangeDirGuard is `lane:cwd`, not `none`. A wrong
# `global` costs a slow test; a wrong `none` costs a flaky suite.
# Known boundary (documented, accepted): turbofish calls (`f::<T>()`),
# function pointers invoked by allowlisted callers, and external-crate
# method impls are not resolved; method names are checked by name against
# the same-file index and the allowlist, which is conservative for this
# repository's pollution surface (std::env / cwd / hash-kind APIs).
# `use Type as Alias` renames of guard types (e.g. EnvVarGuard) hide the
# substring/qualified match and fail closed to `global`, never to `none`.
# Parameter invocations (`predicate(...)` on an `impl Fn` argument) fail
# closed: a fn-pointer VALUE can reference a never-judged body without a
# call site, so callable parameters are never trusted.
# DEPENDENCY lanes: spawning without env_clear() inherits the racing parent
# env, and std::env::current_dir() reads the racing cwd — both are judged as
# needing the corresponding lane even though they mutate nothing.
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
import os, re, sys, time
_T0 = time.time()
_TM = {}


def _tmark(tag):
    if os.environ.get('SERIAL_CLASSIFY_TIME') == '1':
        print('t\t%s\t%.2f' % (tag, time.time() - _T0), file=sys.stderr)

ATTR_START = re.compile(r'#\[(?:serial_test::)?serial')
FN   = re.compile(r'^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)')
_RX_CACHE = {}


def _rx(pat):
    r = _RX_CACHE.get(pat)
    if r is None:
        r = _RX_CACHE[pat] = re.compile(pat)
    return r
FN_INLINE = re.compile(r'\bfn\s+([A-Za-z_][A-Za-z0-9_]*)')
RAW_STR = re.compile(r'(?:[bc]?r)(#*)"')
CHAR_LIT = re.compile(r"'(?:\\(?:[nrt0\\'\"]|x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]{1,6}\})|[^\\'])'")

CONFIG_ARG = re.compile(r'(?:inner_attrs|crate)\s*=')
RAW_IDENT  = re.compile(r'\br#(?=[A-Za-z_])')
NON_ASCII  = re.compile(r'[^\x00-\x7f]')
UFCS       = re.compile(r'<\s*([A-Za-z_][A-Za-z0-9_:\s]*?)\s*(?:as\s+[A-Za-z0-9_:\s]+)?>\s*::')

TURBOFISH    = re.compile(r'::\s*<[^<>]*>')
SPACED_SEP   = re.compile(r'\s*::\s*')

def norm_calls_text(t):
    """Normalize a body for call analysis: drop turbofish segments
    (`Alias::<u8>::new` -> `Alias::new`, repeatedly for nesting) and tighten
    whitespace around `::` (`Command :: new` -> `Command::new`) — Codex TA-01
    R2 P0: both spellings escaped the textual rules."""
    prev = None
    while prev != t:
        prev = t
        t = TURBOFISH.sub('', t)
        # Codex TA-01 R19 P0: lower UFCS — `<path::Type>::f` and
        # `<Type as Trait>::f` become `path::Type::f` / `Type::f`, so the
        # spawn prover and path resolution see the real associated call.
        t = UFCS.sub(lambda m: m.group(1) + '::', t)
    t = SPACED_SEP.sub('::', t)
    return re.sub(r'\s*\.\s*(?!\.)', '.', t)   # join multi-line builder chains

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

# Process-wide APIs, matched as substrings over blanked code. Direct hits in a
# test body contribute lanes; hits inside a ONE-level-expanded helper body
# propagate the same lanes to the caller (TA-01).
GLOBAL_CALLS = ('set_var', 'remove_var', 'EnvVarGuard')
CWD_CALLS    = ('ChangeDirGuard', 'set_current_dir')
HASH_CALLS   = ('set_hash_kind',)
BLACK_NAMES  = frozenset((
    'set_var', 'remove_var', 'set_current_dir',
    'set_hash_kind', 'set_hash_kind_for_test', 'ChangeDirGuard',
    'EnvVarGuard',
))

# Calls that ARE the recorded pollution: the name passes only when the body
# already carries the lane the call produces (e.g. `EnvVarGuard::set` in a
# body whose EnvVarGuard substring put it on the env lane).
LANE_SANCTIONED = {'set': 'env'}

# Qualified `Type::fn` calls that resolve process-wide state from the cwd or
# env: matching is on the LAST TWO path segments, before any bare-name rule.
# Codex TA-01 R13 P0: a direct parent-env READ races env-mutating tests —
# the same dependency class as an env-inheriting spawn. Reads are laned env
# UNLESS every literal key the file reads is on this audited benign list
# (build/harness keys no test mutates; a startup self-check asserts they never
# appear in the tree's mutation set). Non-literal keys, `vars()` iteration and
# `env::temp_dir` always lane.
BENIGN_READ_KEYS = frozenset((
    'LLVM_PROFILE_FILE', 'CARGO_BIN_EXE_libra', 'WINDIR', 'SYSTEMROOT',
))
ENV_READ_NAMES = frozenset(('var', 'var_os', 'vars', 'temp_dir'))

KNOWN_LANE_QUALIFIED = {
    'ConfigKv::set':        'cwd',   # writes the config DB of the repo at the process cwd
    'ConfigKv::get':        'cwd',   # reads the same cwd-resolved DB
    'Head::current_commit': 'cwd',   # reads the repository at the process cwd
    'EnvVarGuard::set':     'env',   # scoped set_var wrapper
    'env::current_dir':     'cwd',   # std cwd READ races cwd-mutating tests
}

# Helpers OUTSIDE the scanned tree (src/utils/test.rs) that are known to touch
# a process-wide resource: calling them contributes the named lane.
KNOWN_LANE_HELPERS = {
    'setup_with_new_libra_in': 'cwd',   # holds ChangeDirGuard/CWD_LOCK during repo init
    'setup_with_new_libra':    'cwd',   # same family
    # cwd-DEPENDENT reads: they resolve the repository from the process cwd, so
    # running them in parallel with a cwd-mutating test is a read/write race —
    # they need the cwd lane even though they mutate nothing.
    'current_commit':          'cwd',   # Head::current_commit reads the repo at cwd
    'current_dir':             'cwd',   # free/path std::env::current_dir cwd READ
}

# --- TA-01 allowlist -------------------------------------------------------
# Reason constants (every entry below references exactly one):
R_CHILD = 'spawns/configures a CHILD process only; parent env/cwd untouched'
R_FS    = 'tempdir-scoped filesystem or fd I/O; no process-global state'
R_PURE  = 'pure in-process value/data manipulation'
R_READ  = 'read-only query of process/OS/repo state; mutates nothing global'
R_ABORT = 'test-abort/assertion surface; terminates instead of mutating'
R_PRIM  = 'audited shared test primitive (tests/command/mod.rs): drives the CLI child with an isolated HOME; no parent pollution'

MACRO_ALLOW = {
    'assert': R_ABORT, 'assert_eq': R_ABORT, 'assert_ne': R_ABORT,
    'panic': R_ABORT, 'unreachable': R_ABORT, 'todo': R_ABORT,
    'matches': R_PURE, 'format': R_PURE, 'format_args': R_PURE,
    'vec': R_PURE, 'concat': R_PURE, 'stringify': R_PURE,
    'json': R_PURE, 'anyhow': R_PURE, 'bail': R_ABORT,
    'println': R_PURE, 'eprintln': R_PURE, 'print': R_PURE, 'eprint': R_PURE,
    'write': R_PURE, 'writeln': R_PURE, 'dbg': R_PURE,
    'env': R_READ, 'option_env': R_READ, 'cfg': R_READ,
    'line': R_READ, 'file': R_READ, 'column': R_READ,
    'include_str': R_READ, 'include_bytes': R_READ,
}

CALL_ALLOW = {
    # audited shared primitives (tests/command/mod.rs) — R_PRIM
    'run_libra_command': R_PRIM, 'run_libra_command_with_stdin': R_PRIM,
    'run_libra_command_with_stdin_and_env': R_PRIM,
    'spawn_libra_command_with_env': R_PRIM, 'base_libra_command': R_PRIM,
    'init_repo_via_cli': R_PRIM, 'configure_identity_via_cli': R_PRIM,
    'assert_cli_success': R_PRIM, 'parse_cli_error_stderr': R_PRIM,
    'parse_json_stdout': R_PRIM, 'skip_permission_denied_test_if_root': R_READ,
    # tempdirs — R_FS
    'tempdir': R_FS, 'tempdir_in': R_FS,
    # child processes — R_CHILD
    'spawn': R_CHILD, 'output': R_CHILD, 'status': R_CHILD, 'wait': R_CHILD,
    'wait_with_output': R_CHILD, 'kill': R_CHILD, 'arg': R_CHILD,
    'args': R_CHILD, 'env': R_CHILD, 'envs': R_CHILD, 'env_clear': R_CHILD,
    'env_remove': R_CHILD, 'stdout': R_CHILD,
    'stderr': R_CHILD, 'stdin': R_CHILD, 'id': R_READ, 'try_wait': R_CHILD,
    # filesystem — R_FS
    'write': R_FS, 'write_all': R_FS, 'read': R_FS, 'read_to_string': R_FS,
    'read_to_end': R_FS, 'read_dir': R_FS, 'read_line': R_FS,
    'create_dir': R_FS, 'create_dir_all': R_FS, 'remove_file': R_FS,
    'remove_dir': R_FS, 'remove_dir_all': R_FS, 'copy': R_FS, 'rename': R_FS,
    'set_permissions': R_FS, 'metadata': R_READ, 'symlink_metadata': R_READ,
    'canonicalize': R_READ, 'symlink': R_FS, 'hard_link': R_FS, 'flush': R_FS,
    'set_len': R_FS, 'sync_all': R_FS, 'open': R_FS, 'create': R_FS,
    'from_mode': R_PURE, 'permissions': R_READ, 'set_readonly': R_FS,
    'set_mode': R_FS, 'mode': R_READ,
    # read-only env/os queries — R_READ
    'exists': R_READ,
    'is_file': R_READ, 'is_dir': R_READ, 'file_name': R_READ,
    'extension': R_READ, 'file_type': R_READ, 'is_symlink': R_READ,
    # time — R_READ
    'now': R_READ, 'elapsed': R_READ, 'from_secs': R_PURE,
    'from_millis': R_PURE, 'from_micros': R_PURE, 'sleep': R_READ,
    'duration_since': R_PURE, 'checked_add': R_PURE, 'as_secs': R_PURE,
    'as_millis': R_PURE,
    # conversion / parsing / formatting — R_PURE
    'from_utf8_lossy': R_PURE, 'from_utf8': R_PURE, 'from_str': R_PURE,
    'from_slice': R_PURE, 'from_reader': R_PURE, 'to_string': R_PURE,
    'to_string_lossy': R_PURE, 'to_str': R_PURE, 'to_owned': R_PURE,
    'to_vec': R_PURE, 'to_path_buf': R_PURE, 'as_str': R_PURE,
    'as_bytes': R_PURE, 'as_slice': R_PURE, 'as_ref': R_PURE,
    'as_mut': R_PURE, 'parse': R_PURE, 'display': R_PURE,
    # Result/Option plumbing — R_PURE
    'unwrap': R_ABORT, 'expect': R_ABORT, 'unwrap_err': R_ABORT,
    'expect_err': R_ABORT, 'unwrap_or': R_PURE, 'unwrap_or_else': R_PURE,
    'unwrap_or_default': R_PURE, 'ok': R_PURE, 'err': R_PURE,
    'ok_or': R_PURE, 'ok_or_else': R_PURE, 'is_some': R_PURE,
    'is_none': R_PURE, 'is_ok': R_PURE, 'is_err': R_PURE,
    'is_some_and': R_PURE, 'is_ok_and': R_PURE, 'context': R_PURE,
    'with_context': R_PURE, 'map_err': R_PURE, 'and_then': R_PURE,
    'or_else': R_PURE, 'take': R_PURE, 'replace': R_PURE, 'insert': R_PURE,
    # collections / iterators / strings — R_PURE
    'iter': R_PURE, 'iter_mut': R_PURE, 'into_iter': R_PURE, 'map': R_PURE,
    'filter': R_PURE, 'filter_map': R_PURE, 'flat_map': R_PURE,
    'flatten': R_PURE, 'collect': R_PURE, 'find': R_PURE, 'find_map': R_PURE,
    'position': R_PURE, 'any': R_PURE, 'all': R_PURE, 'count': R_PURE,
    'enumerate': R_PURE, 'zip': R_PURE, 'rev': R_PURE, 'skip': R_PURE,
    'chain': R_PURE, 'last': R_PURE, 'next': R_PURE, 'peekable': R_PURE,
    'cloned': R_PURE, 'copied': R_PURE, 'sum': R_PURE, 'min': R_PURE,
    'max': R_PURE, 'sort': R_PURE, 'sort_by': R_PURE, 'sort_by_key': R_PURE,
    'dedup': R_PURE, 'join': R_PURE, 'split': R_PURE, 'splitn': R_PURE,
    'split_whitespace': R_PURE, 'rsplit': R_PURE, 'rsplitn': R_PURE,
    'lines': R_PURE, 'chars': R_PURE, 'bytes': R_PURE, 'trim': R_PURE,
    'trim_start': R_PURE, 'trim_end': R_PURE, 'trim_start_matches': R_PURE,
    'trim_end_matches': R_PURE, 'starts_with': R_PURE, 'ends_with': R_PURE,
    'strip_prefix': R_PURE, 'strip_suffix': R_PURE, 'contains': R_PURE,
    'contains_key': R_PURE, 'push': R_PURE, 'push_str': R_PURE, 'pop': R_PURE,
    'remove': R_PURE, 'extend': R_PURE, 'clear': R_PURE, 'get': R_PURE,
    'get_mut': R_PURE, 'entry': R_PURE, 'or_insert': R_PURE,
    'or_insert_with': R_PURE, 'keys': R_PURE, 'values': R_PURE,
    'is_empty': R_PURE, 'len': R_PURE, 'first': R_PURE, 'clone': R_PURE,
    'eq': R_PURE, 'ne': R_PURE, 'cmp': R_PURE, 'into': R_PURE,
    'try_into': R_PURE, 'from': R_PURE, 'default': R_PURE, 'repeat': R_PURE,
    'char_indices': R_PURE, 'nth': R_PURE, 'fold': R_PURE, 'retain': R_PURE,
    'truncate': R_PURE, 'windows': R_PURE, 'concat': R_PURE, 'to_lowercase': R_PURE,
    'to_uppercase': R_PURE, 'path': R_READ, 'as_path': R_PURE, 'parent': R_PURE,
    'components': R_PURE, 'file_stem': R_PURE, 'with_extension': R_PURE,
    'strip_prefix_path': R_PURE, 'exit_ok': R_PURE, 'success': R_READ,
    'code': R_READ, 'signal': R_READ,
    # JSON value plumbing — R_PURE
    'pointer': R_PURE, 'as_array': R_PURE, 'as_object': R_PURE,
    'as_u64': R_PURE, 'as_i64': R_PURE, 'as_f64': R_PURE, 'as_bool': R_PURE,
    'to_value': R_PURE, 'from_value': R_PURE, 'take_mut': R_PURE,
    # runtime plumbing (process-local) — R_PURE
    'block_on': R_PURE, 'new_current_thread': R_PURE, 'enable_all': R_PURE,
    'build': R_PURE, 'builder': R_PURE, 'handle': R_PURE, 'abort': R_PURE,
    'await_holder': R_PURE, 'lock': R_PURE, 'read_lock': R_PURE,
    'send': R_PURE, 'recv': R_PURE, 'try_recv': R_PURE, 'subscribe': R_PURE,
    # constructor convention for `Type::new(...)`; the startup self-check below
    # refuses to run if any shared-scope `fn new` body carries pollution
    'new': R_PURE,
    # borrow/氏 conversions & inspections — R_PURE
    'into_owned': R_PURE, 'as_deref': R_PURE, 'matches': R_PURE,
    'is_string': R_PURE, 'is_number': R_PURE, 'is_null': R_PURE,
    'is_array': R_PURE, 'is_boolean': R_PURE, 'is_object': R_PURE,
    'as_u16': R_PURE, 'to_ascii_uppercase': R_PURE,
    'to_ascii_lowercase': R_PURE, 'is_ascii_digit': R_PURE,
    'is_ascii_alphabetic': R_PURE, 'difference': R_PURE, 'field': R_PURE,
    'from_string': R_PURE, 'from_sql_and_values': R_PURE,
    'from_type_and_data': R_PURE, 'parse_from_rfc3339': R_PURE,
    'is_success': R_READ, 'header': R_PURE, 'headers': R_PURE,
    # tempdir-scoped SQLite/sea-orm plumbing — R_FS
    'query_one': R_FS, 'query_all': R_FS, 'connect': R_FS, 'commit': R_FS,
    'rollback': R_FS, 'begin': R_FS,
    # round-2 audited tail — R_PURE unless noted
    'json': R_PURE, 'as_os_str': R_PURE, 'is_none_or': R_PURE,
    'null': R_PURE, 'text': R_PURE, 'body': R_PURE, 'bytes': R_PURE,
    'run_builtin_migrations': R_FS, 'get_database_backend': R_PURE,
    'post': R_PURE, 'put': R_PURE, 'delete': R_PURE, 'piped': R_PURE,
    'inherit': R_PURE, 'query_one_raw': R_FS, 'kind': R_PURE,
    'timeout': R_PURE, 'basic_auth': R_PURE, 'bearer_auth': R_PURE,
    'last_os_error': R_READ, 'is_absolute': R_PURE, 'current_exe': R_READ,
    'rfind': R_PURE, 'prefix': R_PURE, 'split_at': R_PURE, 'try_clone': R_FS,
    'split_once': R_PURE, 'rsplit_once': R_PURE,
}
# Names safe ONLY in method position (`.name(` on a receiver): the same bare
# name in free/path position is a different symbol — e.g. free `execute_safe(`
# is libra's in-process CLI entry that resolves the repo from the process cwd
# (src/command/*.rs), so it must NOT be blessed by the sea-orm reasoning.
METHOD_ONLY_ALLOW = {
    'execute': R_FS, 'execute_raw': R_FS,
    # `.current_dir(` is the Command CHILD configurator; the free/path spelling
    # `current_dir()` is std::env's process-cwd READ and lands on the cwd lane
    # via KNOWN_LANE_HELPERS instead (Codex TA-01 R3 P0).
    'current_dir': R_CHILD,
}

KEYWORDS = frozenset((
    'if', 'for', 'while', 'match', 'loop', 'return', 'fn', 'let', 'as', 'in',
    'move', 'ref', 'mut', 'impl', 'where', 'use', 'pub', 'else', 'unsafe',
    'dyn', 'break', 'continue', 'struct', 'enum', 'trait', 'mod', 'const',
    'static', 'type', 'crate', 'super', 'self', 'Self', 'true', 'false',
    'await', 'async', 'box', 'extern', 'drop',
))

def code_only(lines):
    """Blank comments and string literals, preserving columns and line count,
    so attribute/fn matching never sees text inside strings or comments."""
    out = []
    block_comment = 0      # nested /* */ depth
    in_string = False      # normal "..." (also b"..." / c"...") with escapes
    raw_hashes = None      # inside r#*"..."#* with this many '#'
    for line in lines:
        # fast path: outside every string/comment state, a line with no
        # quote, char, or slash cannot change state or need blanking
        if (block_comment == 0 and not in_string and raw_hashes is None
                and '"' not in line and "'" not in line
                and '/' not in line):
            out.append(line)
            continue
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

# ---------- pass 1: load every tests/**.rs (blanked) and index fn bodies ----
def arg_literal(raw_lines, ln, col):
    """First-argument string literal of a call whose `(` ends at (ln, col),
    following LINE CONTINUATIONS (blank and //-comment lines are skipped).
    Returns the literal or None; anything not provably a same-call string
    literal — expressions, multi-line strings, block comments — fails toward
    None (dynamic)."""
    j, l = col, ln
    for _ in range(24):
        raw = raw_lines[l] if l < len(raw_lines) else ''
        while j < len(raw) and raw[j] in ' \t':
            j += 1
        if j >= len(raw) or raw[j:].lstrip().startswith('//'):
            l += 1
            j = 0
            continue
        if raw[j] == '"':
            k2 = raw.find('"', j + 1)
            return raw[j + 1:k2] if k2 != -1 else None
        return None
    return None


def arg_prefix(raw_lines, ln, col):
    """The first 48 raw characters of a call's first argument (continuation
    lines followed like arg_literal); '' when nothing is found."""
    j, l = col, ln
    for _ in range(24):
        raw = raw_lines[l] if l < len(raw_lines) else ''
        while j < len(raw) and raw[j] in ' \t':
            j += 1
        if j >= len(raw) or raw[j:].lstrip().startswith('//'):
            l += 1
            j = 0
            continue
        return raw[j:j + 48]
    return ''


def delimit(code, k0, col0):
    """Brace-match a body starting at code[k0][col0:]. Returns (closed, text)."""
    depth = 0; seen = False; closed = False; body = []
    k = k0
    while k < len(code):
        seg = code[k][col0:] if k == k0 else code[k]
        depth += seg.count('{') - seg.count('}')
        if '{' in seg:
            seen = True
        body.append(seg)
        if seen and depth <= 0:
            closed = True
            break
        k += 1
    return closed, '\n'.join(body)

FILES = []
raw_index = {}   # path -> RAW lines (string values intact, for #[path] attrs)
UNRESOLVED_INCLUDES = set()   # files whose include! target cannot be indexed
INCLUDE_CALL = re.compile(r'\binclude!\s*\(')


def _load_rs(p):
    lines = open(p, encoding='utf-8', errors='replace').read().split('\n')
    blanked = code_only(lines)
    # Codex TA-01 R7 P0: raw identifiers (`r#cmd`) must resolve like their
    # plain spelling. Strings are already blanked, so a surviving `r#` is a
    # raw identifier — blank the sigil column-preservingly.
    blanked = [RAW_IDENT.sub('  ', l) if 'r#' in l else l for l in blanked]
    return lines, blanked


for root, dirs, files in os.walk('tests'):
    dirs[:] = sorted(d for d in dirs if d not in ('data', 'fixtures'))
    for name in sorted(files):
        if not name.endswith('.rs'):
            continue
        path = os.path.join(root, name)
        lines, blanked = _load_rs(path)
        # Codex TA-01 R26 P0: item-position `include!` splices ANOTHER file's
        # items into this one — even from the pruned fixtures/ and data/
        # dirs, which would otherwise never enter the indexes. Splice every
        # literal include target into THIS file's indexed view (transitive,
        # cycle- and bomb-capped, tests/-rooted .rs only); any non-literal,
        # out-of-tree, missing, or cyclic include leaves unscannable source
        # in the binary — the file fails closed and the benign gate is
        # disabled (consumed below).
        merged_raw, merged_blanked = [], []
        pending = [(path, lines, blanked)]
        seen_inc = {os.path.normpath(path)}
        while pending:
            ipath, ilines, iblanked = pending.pop(0)
            merged_raw.extend(ilines)
            merged_blanked.extend(iblanked)
            for iln, ibl in enumerate(iblanked):
                for im in INCLUDE_CALL.finditer(ibl):
                    tgt = arg_literal(ilines, iln, im.end())
                    cand = None
                    if tgt and not os.path.isabs(tgt):
                        cand = os.path.normpath(
                            os.path.join(os.path.dirname(ipath), tgt))
                    if not (cand and cand.endswith('.rs')
                            and cand.startswith('tests' + os.sep)
                            and os.path.isfile(cand)):
                        UNRESOLVED_INCLUDES.add(path)
                        continue
                    if cand in seen_inc or len(seen_inc) > 64:
                        UNRESOLVED_INCLUDES.add(path)
                        continue
                    seen_inc.add(cand)
                    l2, b2 = _load_rs(cand)
                    pending.append((cand, l2, b2))
        raw_index[path] = merged_raw
        FILES.append((path, merged_blanked))
FILES.sort()

_tmark('loaded')
AMBIGUOUS = object()
TYPE_DECL = re.compile(r'\b(?:struct|enum|impl)(?:\s*<[^>]*>)?\s+([A-Z][A-Za-z0-9_]*)')
TYPE_ALIAS = re.compile(r'\btype\s+([A-Za-z_][A-Za-z0-9_]*)(?:<[^>]*>)?\s*(?:where[^=;]*)?=\s*([A-Za-z0-9_:\s<>,\x27&]+?)\s*;')
TYPE_ANY   = re.compile(r'\btype\s+([A-Za-z_][A-Za-z0-9_]*)')
USE_STMT   = re.compile(r'\buse\s+[^;]*;', re.S)
USE_RENAME = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)\s+as\s+([A-Za-z_][A-Za-z0-9_]*)')
CONST_BIND = re.compile(r'\b(?:const|static)\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:([^=;]*)=\s*([^;]+);')
CMD_ALIAS = re.compile(r'\b(?:use\s+[A-Za-z0-9_:]*::Command\s+as\s+([A-Za-z_][A-Za-z0-9_]*)|type\s+([A-Za-z_][A-Za-z0-9_]*)(?:<[^>]*>)?\s*=\s*[A-Za-z0-9_:]*::?Command(?:<[^>]*>)?\s*;)')
MACRO_DEF = re.compile(r'\bmacro_rules!\s*([A-Za-z_][A-Za-z0-9_]*)')
MOD_DECL  = re.compile(r'^\s*(?:#!?\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;')
PATH_ATTR = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]')
fn_index = {}      # path -> {fn name: body text | AMBIGUOUS}
type_index = {}    # path -> {type names declared (struct/enum/impl) in the file}
alias_index = {}   # path -> {Alias: target type last ident} for `type A = B;`
cmd_aliases = {}   # path -> {names that mean std/tokio process::Command}
macro_index = {}   # path -> {macro_rules! name: body text | AMBIGUOUS}
rename_index = {}  # path -> {alias: original (last one or two idents)} from use/type/const bindings
rename_path_index = {}  # path -> {alias: FULL original path (R36 fs aliases)}
poison_index = {}  # path -> {const/static names with unresolvable callable initializers}
poison_callable_index = {}  # path -> subset of poison whose declared TYPE is callable
mod_index = {}     # path -> {module names declared via `mod x;` in the file}
mod_path_index = {}  # path -> {module name: explicit #[path = ".."] file}
unparsed_types_index = {}  # path -> {type names whose alias decl failed to parse}
env_read_index = {}   # path -> (literal read keys, has-nonliteral-read, has-vars/temp_dir)
MUTATED_ENV_KEYS = set()
DYNAMIC_ENV_MUTATION = []  # (path, line) of non-literal set_var/remove_var keys
for path, code in FILES:
    idx = {}
    types = set()
    any_types = set()
    aliases = {}
    cmds = set()
    macros_here = {}
    mods_here = set()
    mod_paths_here = {}
    for i, line in enumerate(code):
        for m in re.finditer(FN_INLINE, line):
            nm = m.group(1)
            closed, text = delimit(code, i, m.start())
            entry = text if closed else AMBIGUOUS
            idx[nm] = AMBIGUOUS if nm in idx else entry
        types.update(TYPE_DECL.findall(line))
        any_types.update(TYPE_ANY.findall(line))
        for am in TYPE_ALIAS.finditer(line):
            # base type = last ident of the PATH part before any generics —
            # `Polluter<T>` aliases Polluter, never the parameter T (Codex R9)
            base = am.group(2).split('<')[0]
            targets = re.findall(r'[A-Za-z_][A-Za-z0-9_]*', base)
            targets = [t for t in targets if t not in ('std', 'core', 'alloc')]
            if targets:
                aliases[am.group(1)] = targets[-1]
        for cm in CMD_ALIAS.finditer(line):
            cmds.add(cm.group(1) or cm.group(2))
        for mm in MACRO_DEF.finditer(line):
            nm = mm.group(1)
            closed, text = delimit(code, i, mm.start())
            entry = text if closed else AMBIGUOUS
            macros_here[nm] = AMBIGUOUS if nm in macros_here else entry
        dm = MOD_DECL.match(line)
        if dm:
            mods_here.add(dm.group(1))
            # Codex TA-01 R8 P0: `#[path = "..."] mod x;` redirects the module
            # file. The string value is blanked in `code`, so read it from the
            # RAW lines (same line or the nearest preceding attribute line).
            raw = raw_index[path]
            pm = PATH_ATTR.search(raw[i]) if i < len(raw) else None
            if pm:
                mod_paths_here[dm.group(1)] = pm.group(1)
            else:
                # Codex TA-01 R12 P0: an outer attribute binds across any
                # number of blank/comment/attribute lines — walk upward
                # through the contiguous attribute block, no fixed window.
                b = i - 1
                while b >= 0:
                    stripped = raw[b].strip()
                    if stripped == '' or stripped.startswith('//'):
                        b -= 1
                        continue
                    pm = PATH_ATTR.search(raw[b])
                    if pm:
                        mod_paths_here[dm.group(1)] = pm.group(1)
                        break
                    if stripped.startswith('#[') or stripped.startswith('#!['):
                        b -= 1
                        continue
                    break
    fn_index[path] = idx
    type_index[path] = types
    alias_index[path] = aliases
    cmd_aliases[path] = cmds
    macro_index[path] = macros_here
    mod_index[path] = mods_here
    mod_path_index[path] = mod_paths_here
    # Codex TA-01 R3 P0: ANY `use X as Y` (brace groups and lowercase aliases
    # included) or `type y = X;` rename can launder any rule keyed on a name.
    # Build a per-file rename map and substitute originals into the analyzed
    # text BEFORE every other rule — laundering then reduces to the original
    # names, and unknown renames fail closed like any unknown call.
    renames = {}
    poisoned = set()
    whole = '\n'.join(code)
    rename_paths = {}

    def _use_alias_paths(use_text):
        """Codex TA-01 R36/R37 P0s: {alias: full::path} for every `as`
        alias in a use statement, NESTED brace groups resolved recursively —
        `use std::{fs::{write as persist}}` yields std::fs::write."""
        t9 = re.sub(r'\s+', ' ', use_text.strip())
        t9 = re.sub(r'^(?:pub\s*(?:\([^)]*\)\s*)?)?use\s+', '', t9)
        t9 = t9.rstrip(';').strip()
        out9 = {}

        def _walk9(prefix9, seg9):
            parts9, depth9, cur9 = [], 0, []
            for ch9 in seg9:
                if ch9 == '{':
                    depth9 += 1
                elif ch9 == '}':
                    depth9 -= 1
                if ch9 == ',' and depth9 == 0:
                    parts9.append(''.join(cur9))
                    cur9 = []
                    continue
                cur9.append(ch9)
            parts9.append(''.join(cur9))
            for p9 in parts9:
                p9 = p9.strip()
                if not p9:
                    continue
                b9 = p9.find('{')
                if b9 != -1 and p9.endswith('}'):
                    _walk9(prefix9 + re.sub(r'\s+', '', p9[:b9]),
                           p9[b9 + 1:-1])
                    continue
                a9 = re.match(
                    r'((?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*'
                    r'[A-Za-z_][A-Za-z0-9_]*)\s+as\s+'
                    r'([A-Za-z_][A-Za-z0-9_]*)$', p9)
                if a9:
                    out9[a9.group(2)] = prefix9 + re.sub(r'\s+', '',
                                                         a9.group(1))
        _walk9('', t9)
        return out9

    for um in USE_STMT.finditer(whole):
        _ut = um.group(0)
        rename_paths.update(_use_alias_paths(_ut))
        for om, am in USE_RENAME.findall(_ut):
            if om != am and am not in ('self', 'crate'):
                renames[am] = om
    for al, tgt in aliases.items():
        if al != tgt:
            renames.setdefault(al, tgt)
    # Codex TA-01 R5 P0: `const`/`static` FUNCTION-POINTER bindings alias
    # pollution APIs to arbitrary (even allowlisted) names. A plain-path
    # initializer joins the rename map keeping its last TWO segments (so
    # `Command::new` still hits the spawn rule after substitution); any other
    # initializer (closures, expressions) poisons the name — calls to a
    # poisoned name fail closed.
    whole_norm = norm_calls_text(whole)
    poisoned_callable = set()

    def _type_is_callable(ty):
        # fn pointers, Fn-trait objects/impls, and dyn/impl erasures; a local
        # type alias chaining to `fn` (e.g. `type T = fn();`) counts too
        if re.search(r'\bfn\s*\(|\bFn(?:Mut|Once)?\b|\bdyn\b|\bimpl\b',
                     ty):
            return True
        for tid in re.findall(r'[A-Za-z_][A-Za-z0-9_]*', ty):
            cur, seen3 = tid, set()
            while cur in aliases and cur not in seen3:
                seen3.add(cur)
                cur = aliases[cur]
            if cur == 'fn':
                return True
        return False

    for cb in CONST_BIND.finditer(whole_norm):
        nm2, ty2, init = cb.group(1), cb.group(2), cb.group(3).strip()
        if not init:
            continue                      # blanked literal (string/char): harmless
        if re.fullmatch(r'[A-Za-z0-9_:]+', init):
            segs = [x for x in init.split('::') if x]
            if segs and not segs[-1].isdigit():
                tgt = '::'.join(segs[-2:]) if len(segs) >= 2 else segs[-1]
                if tgt != nm2:
                    renames[nm2] = tgt
        elif re.search(r'[A-Za-z_]', init):
            poisoned.add(nm2)
            # Codex TA-01 R25 P0: a poisoned name with a CALLABLE type is a
            # fn value — even a bare VALUE REFERENCE hands it to in-process
            # machinery, so refs of these fail closed (data-typed poisoned
            # consts stay inert in value position: nothing can call them).
            if _type_is_callable(ty2):
                poisoned_callable.add(nm2)
    # resolve chains with a cycle guard (multi-segment targets do not chain)
    resolved = {}
    for al in renames:
        seen, cur = set(), al
        while cur in renames and cur not in seen and '::' not in cur:
            seen.add(cur)
            cur = renames[cur]
        resolved[al] = cur
    for _cb9 in CONST_BIND.finditer(whole_norm):
        _ini9 = _cb9.group(3).strip()
        if re.fullmatch(r'[A-Za-z0-9_:]+', _ini9) and '::' in _ini9:
            rename_paths.setdefault(_cb9.group(1), _ini9)
    rename_path_index[path] = rename_paths
    rename_index[path] = resolved
    poison_index[path] = poisoned
    poison_callable_index[path] = poisoned_callable
    rk, rnl, rvi = set(), False, False
    TF = r'(?:::\s*<[^<>]*>\s*)?'
    # Codex TA-01 R21 P0: detect read CALLS on the BLANKED view (comments and
    # strings are neutralized there — comment text can neither fabricate nor
    # hide a call, and a comment gap between name and paren becomes plain
    # whitespace), then recover the literal KEY from the RAW line at the
    # matched argument column (blanking is column-preserving). Anything not
    # recoverable as a same-line string literal counts as non-literal.
    raw_lines = raw_index[path]
    _mut_gate = tuple(sorted(set(['set_var', 'remove_var']
                                 + [al for al, tgt in
                                    rename_index.get(path, {}).items()
                                    if tgt.split('::')[-1]
                                    in ('set_var', 'remove_var')])))
    for ln, bl in enumerate(code):
        if not ('var' in bl or 'temp_dir' in bl
                or any(g in bl for g in _mut_gate)):
            continue
        for rm2 in _rx(r'\bvar(?:_os)?\s*' + TF + r'\(').finditer(bl):
            j = rm2.end()
            raw = raw_lines[ln] if ln < len(raw_lines) else ''
            while j < len(raw) and raw[j] in ' \t':
                j += 1
            if j < len(raw) and raw[j] == '"':
                k2 = raw.find('"', j + 1)
                if k2 != -1:
                    rk.add(raw[j + 1:k2])
                else:
                    rnl = True
            else:
                rnl = True               # non-literal or multi-line argument
        if _rx(r'\bvars\s*' + TF + r'\(|env::temp_dir\s*\(|\btemp_dir\s*\(').search(bl):
            rvi = True
        # Codex TA-01 R22 P0: aliases/const bindings resolving to set_var or
        # remove_var are mutation sites under their ALIAS spelling — scan them
        # by alias name (columns stay true; no substitution shift).
        mut_names = ['set_var', 'remove_var']
        for al, tgt in rename_index[path].items():
            if tgt.split('::')[-1] in ('set_var', 'remove_var'):
                mut_names.append(al)
        mut_alt = r'\b(?:' + '|'.join(sorted(set(map(re.escape, mut_names)))) + r')'
        for mm2 in _rx(mut_alt + r'\s*' + TF + r'\(').finditer(bl):
            key = arg_literal(raw_lines, ln, mm2.end())
            if key is not None:
                MUTATED_ENV_KEYS.add(key)
            else:
                DYNAMIC_ENV_MUTATION.append((path, ln, mm2.end()))
    env_read_index[path] = (rk, rnl, rvi)
    # Codex TA-01 R9 P0: a `type NAME ...` declaration our parser could NOT
    # turn into an alias mapping (exotic where-clauses, fn-pointer RHS, future
    # syntax) must poison NAME as a TYPE PREFIX — `NAME::fn()` fails closed
    # instead of falling through to the external-constructor convention.
    unparsed_types_index[path] = any_types - set(aliases)

_tmark('pass1-done')
SHARED_PREFIXES = ('tests/command/mod.rs', 'tests/harness/', 'tests/helpers/')
shared_bodies = {}   # fn name -> [(body, defining path)] under the shared prefixes
shared_macros = {}   # macro name -> [(body, defining path)] under the shared prefixes
shared_types = {}    # type name -> defining shared path | AMBIGUOUS
all_fns = {}         # fn name -> [(body, defining path)] anywhere under tests/
all_types = {}       # type name -> [defining path] anywhere under tests/
all_macros = {}      # macro name -> [(body, defining path)] anywhere under tests/
shared_fn_names = set()
for path, _ in FILES:
    for nm, body in macro_index[path].items():
        all_macros.setdefault(nm, []).append((body, path))
    for nm, body in fn_index[path].items():
        all_fns.setdefault(nm, []).append((body, path))
    for tn in type_index[path]:
        all_types.setdefault(tn, []).append(path)
    if not path.startswith(SHARED_PREFIXES):
        continue
    for nm, body in fn_index[path].items():
        shared_bodies.setdefault(nm, []).append((body, path))
        shared_fn_names.add(nm)
    for nm, body in macro_index[path].items():
        shared_macros.setdefault(nm, []).append((body, path))
    for tn in type_index[path]:
        shared_types[tn] = AMBIGUOUS if tn in shared_types else path

_tmark('shared-done')
# Codex TA-01 R23 P0: dynamic mutation evidence (a non-literal set_var /
# remove_var key) was recorded but never consumed — a dynamic site could
# mutate a benign key while readers of that key stayed `none`. Resolution:
# PARAM-FORWARDING TRACE, two passes over the dynamic sites.
#
# Pass A — assoc-fn hosts. A site is traceable when its host fn is an
# ASSOCIATED FN WITHOUT self inside an `impl Type` block: such a fn cannot be
# called bare (`use Type::assoc` is invalid for inherent impls), so
# `Type::host(` / `Self::host(` / `<Type>::host(` are its only call
# spellings. Every call site's first-arg string literal (continuation lines
# followed) joins MUTATED_ENV_KEYS; a QUALIFIED REFERENCE WITHOUT A CALL is a
# fn-pointer escape and kills the trace.
#
# Pass B — `self.field` sites (the EnvVarGuard Drop-restore pattern). The
# replayed key can only be a value the object was BUILT with, so the site is
# traceable iff every struct-literal construction of the impl type sits
# inside a no-self assoc fn of that type with the field bound verbatim to a
# fn parameter (shorthand or `field: param`), every such builder is itself a
# Pass-A-traced host, and the field is never reassigned (`.field =`).
#
# ANY other shape — free-fn host, self-taking method with a non-self.field
# key, unparsable impl header, non-literal caller key, zero traced callers,
# an untracked bare/pointer reference to set_var / remove_var or an alias —
# is UNTRACEABLE, and the benign read list is DISABLED for the whole run:
# readers of formerly-benign keys lane env like every other env reader.
_FILEMAP = dict(FILES)
_TFQ = r'(?:::\s*<[^<>]*>\s*)?'

BENIGN_DISABLED = False
_EXPL = os.environ.get('SERIAL_CLASSIFY_EXPLAIN') == '1'

def _untraceable(dp, dl, why):
    global BENIGN_DISABLED
    BENIGN_DISABLED = True
    if _EXPL:
        print('explain: dynamic env mutation %s:%d untraceable (%s)'
              % (dp, dl + 1, why), file=sys.stderr)

def _impl_owner_map(code_lines):
    """Per-line name of the enclosing `impl Type` block (state at line start).

    Brace-depth tracked on the blanked view; an impl header whose type cannot
    be parsed pushes the sentinel '?' so sites inside it stay untraceable."""
    owner = [None] * len(code_lines)
    stack = []      # (type name or '?', depth at which the impl block opened)
    depth = 0
    pending = None
    for i, line in enumerate(code_lines):
        owner[i] = stack[-1][0] if stack else None
        mi = re.search(r'\bimpl\b', line)
        if mi:
            tail = re.sub(r'^\s*<[^<>]*>', '', line[mi.end():])
            fm = _rx(r'\bfor\s+([A-Za-z_][A-Za-z0-9_]*)').search(tail)
            tm = fm or re.match(r'\s*([A-Za-z_][A-Za-z0-9_]*)', tail)
            pending = tm.group(1) if tm else '?'
        elif re.search(r'\btrait\b', line):
            pending = '?'
        for ch in line:
            if ch == '{':
                depth += 1
                if pending is not None:
                    stack.append((pending, depth))
                    pending = None
            elif ch == '}':
                if stack and stack[-1][1] == depth:
                    stack.pop()
                depth -= 1
    return owner

_impl_owner_memo = {}

def _owner_at(path, ln):
    if path not in _impl_owner_memo:
        _impl_owner_memo[path] = _impl_owner_map(_FILEMAP[path])
    om = _impl_owner_memo[path]
    return om[ln] if ln < len(om) else None

def _host_fn_at(code_lines, ln):
    """(name, header line) of the nearest fn header at or above `ln`; a wrong
    host can only reduce traced callers, which fails toward disabling."""
    for b in range(ln, -1, -1):
        ms = list(FN_INLINE.finditer(code_lines[b]))
        if ms:
            return ms[-1].group(1), b
    return None, None

_SELF_FN = r'\s*(?:<[^<>]*>)?\s*\(\s*&?\s*(?:mut\s+)?self\b'

def _fn_params(code_lines, hline, host):
    """Single-line fn-header param NAMES; None when the header is multi-line
    or unparsable (callers then fail toward untraceable)."""
    m = _rx(r'\bfn\s+' + re.escape(host)
                  + r'\s*(?:<[^<>]*>)?\s*\(([^()]*)\)').search(code_lines[hline])
    if not m:
        return None
    return set(re.findall(r'([A-Za-z_][A-Za-z0-9_]*)\s*:', m.group(1)))

def _qualified_pat(own, host, call):
    stem = (r'(?:\b(?:' + re.escape(own) + r'|Self)|<\s*' + re.escape(own)
            + r'\s*>)::' + re.escape(host))
    if call:
        return re.compile(stem + r'\s*' + _TFQ + r'\(')
    return re.compile(stem + r'\b(?!\s*' + _TFQ + r'\()')

def _trace_assoc_host(dpath, dline, own, host):
    """Collect every qualified caller's literal first arg into
    MUTATED_ENV_KEYS. True iff all callers are literal, at least one exists,
    and no call-free qualified reference (fn-pointer escape) is in the tree."""
    call_pat = _qualified_pat(own, host, call=True)
    ref_pat = _qualified_pat(own, host, call=False)
    ok, traced = True, 0
    for p2, c2 in FILES:
        raw2 = raw_index[p2]
        for ln2, bl2 in enumerate(c2):
            if ref_pat.search(bl2):
                _untraceable(p2, ln2, 'call-free qualified reference to '
                             + own + '::' + host)
                ok = False
            for qm in call_pat.finditer(bl2):
                key = arg_literal(raw2, ln2, qm.end())
                if key is None:
                    _untraceable(p2, ln2, 'non-literal caller key for '
                                 + own + '::' + host)
                    ok = False
                else:
                    MUTATED_ENV_KEYS.add(key)
                    traced += 1
    if traced == 0:
        _untraceable(dpath, dline, 'zero traced callers for '
                     + own + '::' + host)
        ok = False
    return ok

_pass_b = []          # (path, line, owner type, field name)
_traced_hosts = set()  # (path, owner type, host fn) with an all-literal trace
for _dp, _dl, _dc in DYNAMIC_ENV_MUTATION:
    _dcode = _FILEMAP[_dp]
    _host, _hline = _host_fn_at(_dcode, _dl)
    if _host is None:
        _untraceable(_dp, _dl, 'no enclosing fn')
        continue
    _own = _owner_at(_dp, _dl)
    if _own is None or _own == '?':
        _untraceable(_dp, _dl, 'free fn or unparsable impl header')
        continue
    _pref = arg_prefix(raw_index[_dp], _dl, _dc) if _dc is not None else ''
    _sm = re.match(r'self\s*\.\s*([A-Za-z_][A-Za-z0-9_]*)', _pref)
    if _sm:
        _pass_b.append((_dp, _dl, _own, _sm.group(1)))
        continue
    if _rx(r'\bfn\s+' + re.escape(_host) + _SELF_FN).search(_dcode[_hline]):
        _untraceable(_dp, _dl, 'self-taking method')
        continue
    if _trace_assoc_host(_dp, _dl, _own, _host):
        _traced_hosts.add((_dp, _own, _host))

for _dp, _dl, _own, _field in _pass_b:
    _dcode = _FILEMAP[_dp]
    _bad = False
    for _p2, _c2 in FILES:
        for _ln2, _bl2 in enumerate(_c2):
            if _rx(r'\.\s*' + re.escape(_field) + r'\s*=(?!=)').search(_bl2):
                _untraceable(_p2, _ln2, 'field .%s is reassigned' % _field)
                _bad = True
            for _cm in _rx(r'(?<![A-Za-z0-9_])(' + re.escape(_own)
                    + r'|Self)\s*\{').finditer(_bl2):
                head = _bl2[:_cm.start()]
                if re.search(r'\b(?:impl|struct|enum|trait|union|mod)\b',
                             head) or head.rstrip().endswith('->'):
                    continue           # declaration or return-type brace
                if _cm.group(1) == 'Self' and _owner_at(_p2, _ln2) != _own:
                    continue           # Self of an unrelated impl
                if _owner_at(_p2, _ln2) != _own:
                    _untraceable(_p2, _ln2, 'construction of %s outside its '
                                 'impl' % _own)
                    _bad = True
                    continue
                _g, _gl = _host_fn_at(_c2, _ln2)
                if _g is None or _rx(r'\bfn\s+' + re.escape(_g) + _SELF_FN).search(_c2[_gl]):
                    _untraceable(_p2, _ln2, 'construction of %s outside a '
                                 'no-self assoc fn' % _own)
                    _bad = True
                    continue
                _closed, _body = delimit(_c2, _ln2, _cm.end() - 1)
                _params = _fn_params(_c2, _gl, _g)
                _fb = _rx(r'(?:\{|,)\s*' + re.escape(_field)
                                + r'\s*(?::\s*([A-Za-z_][A-Za-z0-9_]*)\s*)?'
                                + r'(?:,|\})').search(_body or '')
                if (not _closed or _params is None or '..' in (_body or '')
                        or not _fb
                        or (_fb.group(1) is not None
                            and _fb.group(1) not in _params)
                        or (_fb.group(1) is None
                            and _field not in _params)):
                    _untraceable(_p2, _ln2, 'field %s not bound verbatim to '
                                 'a builder param' % _field)
                    _bad = True
                    continue
                if (_p2, _own, _g) not in _traced_hosts:
                    _untraceable(_p2, _ln2, 'builder %s::%s is not a traced '
                                 'assoc host' % (_own, _g))
                    _bad = True
    if _bad:
        _untraceable(_dp, _dl, 'self.%s delegation failed' % _field)

# Codex TA-01 R24 P0 support index: fns that a BARE IDENTIFIER can actually
# reference as a value. Inherent assoc fns and methods need `Type::name`, and
# trait fns are not values — only fns OUTSIDE impl/trait blocks qualify.
# Resolution is tree-wide unique-or-fail (same over-approximation as the
# `#[macro_use]` macro index, Codex R3).
_tmark('drop-done')
free_fn_names = {}         # path -> {fn names defined outside impl/trait}
for _p2, _c2 in FILES:
    if _p2 not in _impl_owner_memo:
        _impl_owner_memo[_p2] = _impl_owner_map(_c2)
    _own2 = _impl_owner_memo[_p2]
    _fns2 = set()
    for _ln2, _bl2 in enumerate(_c2):
        for _fm2 in FN_INLINE.finditer(_bl2):
            if _own2[_ln2] is None:
                _fns2.add(_fm2.group(1))
    free_fn_names[_p2] = _fns2

_tmark('noncall-done')


def resolve_mod_file(file_path, m):
    """Resolve `mod m;` declared in file_path to its scanned file, honoring
    an explicit #[path] attribute; None when unresolvable."""
    base = os.path.dirname(file_path)
    explicit = mod_path_index.get(file_path, {}).get(m)
    cands = []
    if explicit:
        cands.append(os.path.normpath(os.path.join(base, explicit)))
    cands.extend((os.path.join(base, m + '.rs'),
                  os.path.join(base, m, 'mod.rs')))
    for cand in cands:
        if cand in fn_index:
            return cand
    return None


_mod_closure_cache = {}

def mod_closure(file_path):
    """Transitive closure of `mod x;` declarations starting at file_path
    (BFS, cycle-safe) — Codex TA-01 R11 P0: nested local modules
    (`outer::inner::write`) must stay inside the judged scope."""
    if file_path in _mod_closure_cache:
        return _mod_closure_cache[file_path]
    seen = [file_path]
    queue = [file_path]
    while queue:
        cur = queue.pop(0)
        for m in sorted(mod_index.get(cur, ())):
            cand = resolve_mod_file(cur, m)
            if cand and cand not in seen:
                seen.append(cand)
                queue.append(cand)
    _mod_closure_cache[file_path] = seen
    return seen


_visible_types_cache = {}

def visible_types(file_path):
    """type name -> defining path | AMBIGUOUS: the file's own types plus the
    types of every mod-declared file (Codex TA-01 R10 P0 — a type imported
    from a `mod`-declared helper file must expand its real impl fns, never
    fall to the external-constructor convention)."""
    if file_path in _visible_types_cache:
        return _visible_types_cache[file_path]
    out = {}
    for p in mod_closure(file_path):
        for tn in type_index.get(p, ()):
            out[tn] = AMBIGUOUS if tn in out and out[tn] != p else p
    _visible_types_cache[file_path] = out
    return out


# Codex TA-01 R27 P0: `impl Drop for T` runs at scope exit with NO call in
# the test body — a clean constructor plus a polluting destructor laundered
# pollution to `none`. Index every Drop impl's `fn drop` body per type; a
# Drop impl whose fn drop cannot be located inside the impl block (e.g.
# macro-generated) or a duplicated type maps to AMBIGUOUS (fail closed when
# the type is mentioned).
_tmark('vis-done')
DROP_IMPL = re.compile(r'\bimpl\b[^{;]*?\bDrop\b\s+for\s+'
                       r'((?:[A-Za-z_][A-Za-z0-9_]*::)*)'
                       r'([A-Za-z_][A-Za-z0-9_]*)')
drop_impl_index = {}       # path -> {type: fn-drop body | AMBIGUOUS}
DROP_MACRO_UNKNOWN = set()   # crates with an unprovable Drop target
# Codex TA-01 R31 P0: a macro can also emit CALLABLE ITEMS whose NAMES are
# metavariables (`fn $name() { set_var(..) }`, `impl P { fn $name() … }`) —
# invisible to the line-scan fn index, then blessed by allowlisted spellings
# like `write`/`new` at the call site. Any macro whose body matches `fn $`
# is a callable emitter: each invocation's ident arguments bind the emitted
# fn body into the invoking file's fn index (plain macro_rules cannot mint
# new idents, so the generated name must be one of the arguments — binding
# them all over-approximates safely; collisions go AMBIGUOUS). Bodies that
# cannot be extracted, carry metavariables past the signature, or emitters
# defined by an AMBIGUOUS macro body poison the invoking crate.
FN_EMIT = re.compile(r'\bfn\s+\$')


def _drop_alias_chain(p, name):
    """Codex TA-01 R29 P0: `impl Drop for Alias` (type Alias = Polluter)
    attaches the destructor to the UNDERLYING type. Return every spelling on
    the alias chain (the drop body is indexed under all of them), or None on
    a cycle. An UNPARSED alias decl makes the target unprovable — the caller
    poisons the whole crate."""
    chain, cur = [name], name
    amap = alias_index.get(p, {})
    while cur in amap:
        cur = amap[cur]
        if cur in chain:
            return None
        chain.append(cur)
    return chain


_DROP_QUICK = re.compile(r'\bDrop\b')
for _p2, _c2 in FILES:
    _dmap = {}
    _dl9 = [i9 for i9, l9 in enumerate(_c2) if 'Drop' in l9]
    for _ln2 in _dl9:
        _bl2 = _c2[_ln2]
        _dm2 = DROP_IMPL.search(_bl2)
        if not _dm2:
            continue
        # Codex TA-01 R30 P0: `impl Drop for m::Alias` — resolve the module
        # qualifier to its file and walk the alias chain THERE; the Drop
        # impl itself still belongs to THIS file's crate (one crate per
        # tests/*.rs), so the spellings index into this file's map. An
        # unresolvable qualifier fails the crate closed.
        _qsegs = [x for x in _dm2.group(1).rstrip(':').split('::')
                  if x and x not in ('self', 'crate')]
        _defp2 = _p2
        _qok = True
        for _sg in _qsegs:
            if _sg in mod_index.get(_defp2, ()):
                _nx2 = resolve_mod_file(_defp2, _sg)
                if _nx2 is None:
                    _qok = False
                    break
                _defp2 = _nx2
            elif _sg in type_index.get(_defp2, ()):
                continue
            else:
                _qok = False
                break
        if not _qok:
            DROP_MACRO_UNKNOWN.add(_p2)
            continue
        _T2 = _dm2.group(2)
        _entry = AMBIGUOUS
        _closed2, _btxt = delimit(_c2, _ln2, _dm2.start())
        if _closed2:
            _blines = _btxt.split('\n')
            for _k2, _kl in enumerate(_blines):
                _fm2 = _rx(r'\bfn\s+drop\b').search(_kl)
                if _fm2:
                    _c3, _b3 = delimit(_blines, _k2, _fm2.start())
                    if _c3:
                        _entry = _b3
                    break
        if _T2 in unparsed_types_index.get(_defp2, ()):
            DROP_MACRO_UNKNOWN.add(_p2)
            continue
        _chainT = _drop_alias_chain(_defp2, _T2)
        if _chainT is None:
            DROP_MACRO_UNKNOWN.add(_p2)
            continue
        for _Tc in _chainT:
            _dmap[_Tc] = AMBIGUOUS if _Tc in _dmap else _entry
    drop_impl_index[_p2] = _dmap

# Codex TA-01 R28 P0: an item-position macro can GENERATE `impl Drop for $T`
# — the literal Drop scan above cannot see the metavariable target. Any macro
# whose body can emit a Drop impl with a metavariable target makes each of
# its invocations' IDENT ARGUMENTS a Drop-carrying type (bound to the drop-fn
# body extracted from the macro text; a body that cannot be extracted or
# still carries metavariables binds AMBIGUOUS). An invocation whose argument
# idents cannot be recovered poisons the whole invoking CRATE (each
# tests/*.rs is its own test crate, and the orphan rule confines a generated
# `impl Drop` to types of that crate — the file plus its mod closure).
DROP_EMIT = re.compile(r'\bimpl\b[^{;]*?\bDrop\b[^{;]*?\bfor\b[^{;]*?\$')


def _drop_body_of(text_block):
    blines = text_block.split('\n')
    for k4, bl4 in enumerate(blines):
        if re.search(r'\bimpl\b.*\bDrop\b', bl4):
            c4, impl_txt = delimit(blines, k4, 0)
            if not c4:
                return AMBIGUOUS
            ilines = impl_txt.split('\n')
            for k5, il5 in enumerate(ilines):
                fm5 = _rx(r'\bfn\s+drop\b').search(il5)
                if fm5:
                    c5, b5 = delimit(ilines, k5, fm5.start())
                    if c5 and '$' not in b5:
                        return b5
                    return AMBIGUOUS
            return AMBIGUOUS
    return AMBIGUOUS


drop_emitting = {}     # macro name -> drop-fn body | AMBIGUOUS (tree-wide)
callable_emitting = {}  # macro name -> (emitted fn body|AMBIGUOUS, is_free)
ambiguous_macros = set()  # macro names with an uncapturable body anywhere
for _p2 in macro_index:
    for _mn2, _mb2 in macro_index[_p2].items():
        if _mb2 is AMBIGUOUS:
            ambiguous_macros.add(_mn2)
            if _mn2 in drop_emitting:
                drop_emitting[_mn2] = AMBIGUOUS
            continue
        if DROP_EMIT.search(_mb2):
            _ent2 = _drop_body_of(_mb2)
            drop_emitting[_mn2] = (AMBIGUOUS if _mn2 in drop_emitting
                                   else _ent2)
        _fes = [(_k6, _fm6) for _k6, _l6 in enumerate(_mb2.split('\n'))
                for _fm6 in FN_EMIT.finditer(_l6)]
        if _fes:
            _entF = AMBIGUOUS
            if len(_fes) == 1:
                _blF = _mb2.split('\n')
                _cF, _bF = delimit(_blF, _fes[0][0], _fes[0][1].start())
                if _cF:
                    _brF = _bF.find('{')
                    if _brF != -1 and '$' not in _bF[_brF:]:
                        # neutralize the metavariable NAME in the signature
                        # so the judged text parses as an ordinary fn
                        _entF = re.sub(r'\$\s*[A-Za-z_][A-Za-z0-9_]*',
                                       '__ta01_generated',
                                       _bF[:_brF]) + _bF[_brF:]
            _isfree = not _rx(r'(?s)\bimpl\b[^{;]*\{[^{}]*?\bfn\s+\$'
                              ).search(_mb2)
            callable_emitting[_mn2] = (AMBIGUOUS, _isfree) \
                if _mn2 in callable_emitting else (_entF, _isfree)

# (DROP_MACRO_UNKNOWN also collects files whose Drop-emitting invocation
# lost its argument idents — same crate-level consumption.)
_emit_names = (set(drop_emitting) | set(callable_emitting)
               | ambiguous_macros)
_EMIT_QUICK = (re.compile('|'.join(sorted(map(re.escape, _emit_names))))
               if _emit_names else None)
for _p2, _c2 in FILES:
    if _EMIT_QUICK is None:
        break
    _dmap4 = drop_impl_index[_p2]
    for _ln2, _bl2 in enumerate(_c2):
        if not _EMIT_QUICK.search(_bl2):
            continue
        for _iv in re.finditer(r'\b([A-Za-z_][A-Za-z0-9_]*)!\s*[\(\[\{]',
                               _bl2):
            _mn2 = _iv.group(1)
            if _mn2 not in drop_emitting and _mn2 not in callable_emitting \
                    and _mn2 not in ambiguous_macros:
                continue
            # Codex TA-01 R32 P0: recover the arguments by BALANCED
            # delimiter parsing to the invocation's actual closer — a fixed
            # line window silently dropped trailing arguments (fail-open).
            # Bomb-capped; an unclosed invocation poisons the crate.
            _achars = []
            _depth = 1
            _ok5 = False
            _l4, _j4 = _ln2, _iv.end()
            _steps = 0
            while _l4 < len(_c2) and _steps < 20000:
                _steps += 1
                _line4 = _c2[_l4]
                if _j4 >= len(_line4):
                    _l4 += 1
                    _j4 = 0
                    _achars.append(' ')
                    continue
                _ch4 = _line4[_j4]
                _j4 += 1
                if _ch4 in '([{':
                    _depth += 1
                elif _ch4 in ')]}':
                    _depth -= 1
                    if _depth == 0:
                        _ok5 = True
                        break
                _achars.append(_ch4)
            if not _ok5:
                DROP_MACRO_UNKNOWN.add(_p2)
                continue
            _atxt = ''.join(_achars)
            _ids = [i4 for i4 in re.findall(r'[A-Za-z_][A-Za-z0-9_]*', _atxt)
                    if i4 not in KEYWORDS]
            if not _ids or _mn2 in ambiguous_macros:
                DROP_MACRO_UNKNOWN.add(_p2)
                continue
            if _mn2 in callable_emitting:
                _entF, _isfree = callable_emitting[_mn2]
                for _i4 in _ids:
                    fn_index[_p2][_i4] = (AMBIGUOUS
                                          if _i4 in fn_index[_p2] else _entF)
                    if _isfree and _entF is not AMBIGUOUS:
                        free_fn_names[_p2].add(_i4)
            if _mn2 not in drop_emitting:
                continue
            _ent2 = drop_emitting[_mn2]
            for _i4 in _ids:
                if _i4 in unparsed_types_index.get(_p2, ()):
                    DROP_MACRO_UNKNOWN.add(_p2)
                    continue
                _chain4 = _drop_alias_chain(_p2, _i4)
                if _chain4 is None:
                    DROP_MACRO_UNKNOWN.add(_p2)
                    continue
                for _i5 in _chain4:
                    _dmap4[_i5] = AMBIGUOUS if _i5 in _dmap4 else _ent2

# Codex TA-01 R33 P0: the R_FS/R_READ allowlist blessed path I/O
# UNCONDITIONALLY — but `fs::write("relative", ..)` or
# `Path::new("rel").exists()` depends on the process CWD: under another
# test's cwd swap the same call hits a different file. Every path-taking
# call site must PROVE its path: a literal absolute base, a tempdir-derived
# expression, or a binding chain resolving to one. Anything unproven puts
# the ENCLOSING FN on the cwd lane (consumed via the judge stack, so helper
# chains propagate).
PATH_ARG_CALLS = {
    'write': (0,), 'read': (0,), 'read_to_string': (0,), 'read_to_end': (0,),
    'read_dir': (0,), 'create_dir': (0,), 'create_dir_all': (0,),
    'remove_file': (0,), 'remove_dir': (0,), 'remove_dir_all': (0,),
    'copy': (0, 1), 'rename': (0, 1), 'set_permissions': (0,),
    'symlink': (0, 1), 'hard_link': (0, 1), 'canonicalize': (0,),
    'metadata': (0,), 'symlink_metadata': (0,), 'tempdir_in': (0,),
}
PATH_ARG_METHODS = {'open': (0,), 'connect': (0,)}
PATH_QUAL_ONLY = {'open': (0,), 'create': (0,), 'connect': (0,)}
PATH_RECV_METHODS = ('exists', 'is_file', 'is_dir', 'try_exists')
# Codex TA-01 R34/R35 P0s: path safety is proven ONLY structurally — the
# safe origins are CALLEE names inside the call branch (local definitions
# shadow them, so a local `fn tempdir` is analyzed, never blessed), and
# every METHOD on a proven value must be PATH-PRESERVING (`.path()` on an
# arbitrary local type is not a proof; `file_name()`/`strip_prefix()` yield
# relative components and never pass).
STD_SAFE_CALLEES = frozenset(
    ('tempdir', 'tempdir_in', 'temp_dir', 'tempfile'))
STD_SAFE_PREFIXES = frozenset(('TempDir', 'NamedTempFile', 'Builder'))
STD_ARG_DERIVED = frozenset(
    ('read_dir', 'canonicalize', 'metadata', 'symlink_metadata'))
PATH_PRESERVING = frozenset((
    'path', 'join', 'as_path', 'to_path_buf', 'to_owned', 'clone',
    'as_ref', 'unwrap', 'expect', 'display', 'to_string_lossy', 'to_str',
    'as_os_str', 'into', 'parent', 'canonicalize', 'flatten', 'as_deref',
    'borrow', 'to_string', 'as_str', 'prefix', 'suffix', 'tempdir',
    'tempfile', 'tempdir_in', 'rand_bytes', 'into_path', 'keep',
    'as_file', 'child', 'canonicalize_utf8', 'with_extension',
    'with_file_name', 'collect', 'ok', 'iter',
    'into_iter', 'cloned', 'copied', 'take', 'skip', 'rev', 'filter',
    'next', 'last', 'find', 'sort', 'sorted', 'remove', 'pop', 'get',
    'first', 'push'))
_MAP_FAMILY = frozenset(('map', 'filter_map', 'and_then', 'flat_map',
                         'map_while', 'then', 'unwrap_or_else',
                         'map_or_else'))
_METH_ITER = re.compile(r'\.\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(')
_MAP_CLOSURE = re.compile(
    r'\.\s*(?:map|filter_map|and_then|flat_map|map_while|then'
    r'|unwrap_or_else|map_or_else)\s*\(\s*(?:move\s*)?'
    r'\|([^|]*)\|\s*([^)|]*)')


def _chain_ok(expr):
    """Every TOP-LEVEL method on the expression must preserve pathness —
    methods inside call ARGUMENTS are the argument's own business
    (`join(pack.file_name()…)` keeps an absolute base regardless). A
    map-family combinator's CLOSURE must return a value derived from its
    own parameter through preserving methods, or diverge."""
    depth = 0
    k = 0
    L = len(expr)
    while k < L:
        ch = expr[k]
        if ch in '([{':
            depth += 1
            k += 1
            continue
        if ch in ')]}':
            depth -= 1
            k += 1
            continue
        if ch == '.' and depth == 0:
            mm = re.match(r'\.\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(',
                          expr[k:])
            if mm:
                m = mm.group(1)
                astart = k + mm.end()
                d2 = 1
                k2 = astart
                while k2 < L and d2 > 0:
                    if expr[k2] in '([{':
                        d2 += 1
                    elif expr[k2] in ')]}':
                        d2 -= 1
                    k2 += 1
                if m in _MAP_FAMILY:
                    inner = expr[astart:k2 - 1]
                    cm2 = re.match(r'\s*(?:move\s*)?\|([^|]*)\|(.*)$',
                                   inner, re.S)
                    if cm2 is None:
                        return False
                    cb = cm2.group(2).strip()
                    if re.match(r'(?:panic!|unreachable!|todo!)', cb):
                        pass             # diverging closure: inert
                    else:
                        pn = re.findall(r'[A-Za-z_][A-Za-z0-9_]*',
                                        cm2.group(1))
                        rm = re.match(r'(?:Some\s*\(|Ok\s*\()?\s*'
                                      r'(?:[A-Za-z_][A-Za-z0-9_]*::)*'
                                      r'([A-Za-z_][A-Za-z0-9_]*)', cb)
                        if rm is None or not pn:
                            return False
                        if rm.group(1) != pn[0] and not _chain_ok_leaf(
                                cb, pn[0]):
                            return False
                elif m not in PATH_PRESERVING:
                    return False
                k = k2
                continue
        k += 1
    return depth <= 0 or True


_TFP = r'(?:::\s*<[^<>]*>\s*)?'
_PATH_CTOR = re.compile(r'\b(?:Path|PathBuf)::(?:new|from)\s*\(')


def _chain_ok_leaf(cb, param):
    """closure body like `fs::read_to_string(e.unwrap().path()).ok()`:
    a call whose FIRST argument roots at the closure param and whose own
    chains preserve pathness also derives from the parameter."""
    cm3 = _CALL_HEAD.match(cb)
    if not cm3 or cm3.group(3) == '!':
        return False
    args3 = _split_args_text(cb[cm3.end():])
    if not args3:
        return False
    rm3 = re.match(r'\s*&?\s*([A-Za-z_][A-Za-z0-9_]*)', args3[0])
    return (rm3 is not None and rm3.group(1) == param
            and _chain_ok(args3[0]))


def _call_args_at(raw_lines, ln, col):
    """Top-level argument expressions of the call whose `(` ends at
    (ln, col), raw text, balanced, bomb-capped; None when unclosed."""
    args, cur, depth = [], [], 1
    l, j, steps = ln, col, 0
    while l < len(raw_lines) and steps < 4000:
        steps += 1
        line = raw_lines[l]
        if j >= len(line):
            l += 1
            j = 0
            cur.append(' ')
            continue
        ch = line[j]
        j += 1
        if ch == '"':
            cur.append(ch)
            k = line.find('"', j)
            if k == -1:
                return None          # multi-line string: unprovable
            cur.append(line[j:k + 1])
            j = k + 1
            continue
        if ch in '([{':
            depth += 1
        elif ch in ')]}':
            depth -= 1
            if depth == 0:
                args.append(''.join(cur))
                return args
        elif ch == ',' and depth == 1:
            args.append(''.join(cur))
            cur = []
            continue
        cur.append(ch)
    return None


def _recv_expr_at(raw_lines, ln, dotcol):
    """The receiver chain ending at the '.' at (ln, dotcol), walked
    backwards over balanced brackets and up to four continuation lines."""
    out = []
    l, j, depth, steps = ln, dotcol - 1, 0, 0
    while l >= 0 and steps < 2000:
        steps += 1
        line = raw_lines[l]
        if j < 0:
            l -= 1
            if l < ln - 6:
                break
            j = len(raw_lines[l]) - 1 if l >= 0 else -1
            below = raw_lines[l + 1].lstrip() if l + 1 <= ln else ''
            if out and ''.join(reversed(out)).strip() \
                    and not below.startswith('.'):
                break                    # chain does not continue upward
            continue
        ch = line[j]
        if ch in ')]}':
            depth += 1
        elif ch in '([{':
            if depth == 0:
                break
            depth -= 1
        elif ch in ' \t':
            if depth == 0 and out and (out[-1].isalnum()
                                       or out[-1] == '_'):
                # peek left: two adjacent identifier tokens are separate
                k5 = j - 1
                while k5 >= 0 and line[k5] in ' \t':
                    k5 -= 1
                if k5 >= 0 and (line[k5].isalnum() or line[k5] == '_'):
                    break
            j -= 1
            continue
        elif depth == 0 and ch in '=,;!&|+<>?':
            break
        out.append(ch)
        j -= 1
    expr = ''.join(reversed(out)).strip()
    return expr or None


class _FnMap:
    """Per-file map: site line -> enclosing fn (name, RAW body, ordered
    params). Headers are located once; bodies and params are memoized."""

    def __init__(self, code_lines, raw_lines):
        self._raw = raw_lines
        self._code = code_lines
        self._heads = []          # (line, name, header text, header col)
        # line-level open/close counts for O(1)-ish body-end resolution
        self._opens = [l.count('{') for l in code_lines]
        self._closes = [l.count('}') for l in code_lines]
        for i2, l2 in enumerate(code_lines):
            for m2 in FN_INLINE.finditer(l2):
                self._heads.append((i2, m2.group(1), l2, m2.start()))
        self._memo = {}

    def at(self, ln):
        lo, hi = 0, len(self._heads)
        while lo < hi:
            mid = (lo + hi) // 2
            if self._heads[mid][0] <= ln:
                lo = mid + 1
            else:
                hi = mid
        if lo == 0:
            return None, None, None
        hline, name, htext, hcol = self._heads[lo - 1]
        if hline not in self._memo:
            # delimit to the fn's REAL end so a later fn's same-named `let`
            # can never satisfy this fn's binding chase (line-level brace
            # counts precomputed in __init__)
            end = hline
            seg0 = self._code[hline][hcol:]
            depth2 = seg0.count('{') - seg0.count('}')
            opened = '{' in seg0
            if not (opened and depth2 <= 0):
                for k2 in range(hline + 1,
                                min(len(self._code), hline + 2000)):
                    depth2 += self._opens[k2] - self._closes[k2]
                    if self._opens[k2]:
                        opened = True
                    if opened and depth2 <= 0:
                        end = k2
                        break
                else:
                    end = min(len(self._code), hline + 400) - 1
            body = '\n'.join(self._raw[hline:end + 1])
            params = None
            pm = _rx(r'\bfn\s+' + re.escape(name)
                           + r'\s*(?:<[^<>]*>)?\s*\(').search(htext)
            if pm:
                # balanced walk (headers nest parens in types and may span
                # lines); ordered top-level param NAMES, self excluded
                header = htext[pm.end():]
                for extra in range(1, 13):
                    if hline + extra < len(self._raw):
                        header += '\n' + self._raw[hline + extra]
                depth, cur, parts = 1, [], []
                for ch in header:
                    if ch in '([{<':
                        depth += 1
                    elif ch in ')]}>':
                        depth -= 1
                        if depth == 0:
                            parts.append(''.join(cur))
                            break
                    elif ch == ',' and depth == 1:
                        parts.append(''.join(cur))
                        cur = []
                        continue
                    cur.append(ch)
                else:
                    parts = None
                if parts is not None:
                    params = []
                    for pt in parts:
                        nm = _rx(r'\s*(?:mut\s+)?'
                                      r'([A-Za-z_][A-Za-z0-9_]*)\s*:').match(pt)
                        if nm:
                            params.append(nm.group(1))
            self._memo[hline] = (name, body, params)
        return self._memo[hline]


def _split_args_text(txt):
    """Top-level comma split of a call's argument text `a, b, c)` (single
    string, balanced); None when the closer is missing."""
    args, cur, depth = [], [], 1
    for ch in txt[:4000]:
        if ch == '"':
            cur.append(ch)
            continue
        if ch in '([{':
            depth += 1
        elif ch in ')]}':
            depth -= 1
            if depth == 0:
                args.append(''.join(cur))
                return args
        elif ch == ',' and depth == 1:
            args.append(''.join(cur))
            cur = []
            continue
        cur.append(ch)
    return None


_ret_memo = {}
_rex_memo = {}


def _tail_expr_of(body):
    """The fn's tail expression: strip ONE trailing '}' (the fn's own),
    cut at the last ';'. A struct-literal tail keeps its own braces (the
    old brace strip ate `Self { … }`'s closer and the '{' cut ate its
    head)."""
    t = body.rstrip()
    if t.endswith('}'):
        t = t[:-1]
    cut = t.rfind(';')
    t2 = t[cut + 1:]
    if cut == -1:
        br = t2.find('{')
        t2 = t2[br + 1:] if br != -1 else t2
    # statement blocks (for/if) end without ';' — drop their closers
    return re.sub(r'^[\s}]+', '', t2).strip()


def _return_exprs(path, name):
    """[(expr text, body, params, body path)] for every return/tail
    expression of fn `name`; None when the body cannot be resolved."""
    key = (path, name)
    if key in _rex_memo:
        return _rex_memo[key]
    _rex_memo[key] = None
    body = fn_index.get(path, {}).get(name)
    bpath = path
    if body is None or body is AMBIGUOUS:
        ent = shared_bodies.get(name, ())
        if len(ent) == 1 and ent[0][0] is not AMBIGUOUS:
            body, bpath = ent[0]
        else:
            return None
    fm = _fnmaps.get(bpath)
    params = None
    if fm is not None:
        for hl, hn, _ht, _hc in fm._heads:
            if hn == name:
                _, _, params = fm.at(hl)
                break
    exprs = _rx(r'\breturn\s+([^;]+);').findall(body)
    t2 = _tail_expr_of(body)
    if t2 and not t2.startswith('//'):
        exprs.append(t2)
    out = [(e, body, params, bpath) for e in exprs]
    _rex_memo[key] = out
    return out


def _return_statuses(path, name):
    """[(status, paramnames|field-expr text)] for every return/tail
    expression of fn `name` in `path`, in CALLEE-local terms. AMBIGUOUS or
    unresolvable bodies yield [('bad', None)]."""
    key = (path, name)
    if key in _ret_memo:
        return _ret_memo[key]
    _ret_memo[key] = [('bad', None)]        # cycle default
    body = fn_index.get(path, {}).get(name)
    bpath = path
    if body is None or body is AMBIGUOUS:
        ent = shared_bodies.get(name, ())
        if len(ent) == 1 and ent[0][0] is not AMBIGUOUS:
            body, bpath = ent[0]
        else:
            return _ret_memo[key]
    fm = _fnmaps.get(bpath)
    params = None
    if fm is not None:
        for hl, hn, _ht, _hc in fm._heads:
            if hn == name:
                _, _, params = fm.at(hl)
                break
    exprs = _rx(r'\breturn\s+([^;]+);').findall(body)
    tail = re.sub(r'[\s}]+$', '', body)
    # a `match` tail yields through its ARMS — diverging arms are inert
    mt = re.search(r'\bmatch\b[^{]*\{([^{}]*(?:\{[^{}]*\}[^{}]*)*)$',
                   tail)
    _use_match = mt is not None
    if mt is not None:
        for arm in _rx(r'=>\s*([^,\n]+)').findall(mt.group(1)):
            arm = arm.strip().rstrip(',')
            if re.match(r'(?:panic!|unreachable!|todo!|continue\b'
                        r'|break\b|return\b)', arm):
                continue
            exprs.append(arm)
    else:
        tail = _tail_expr_of(body)
        if tail and not tail.startswith('//'):
            exprs.append(tail)
    out = []
    for ex in exprs:
        out.append(_path_expr_status(ex, body, params, bpath))
    _ret_memo[key] = out if out else [('bad', None)]
    return _ret_memo[key]


def _struct_field_expr(expr, field):
    """The init expression of `field` inside a struct-literal expression
    (`Self { field: X }` / shorthand `{ field }`); None when absent."""
    bm = re.search(r'\{', expr)
    if not bm:
        return None
    inner = expr[bm.end():]
    parts = _split_args_text(inner)
    if parts is None:
        parts = [p for p in inner.split(',')]
    for pt in parts:
        pm = _rx(r'(?s)\s*' + re.escape(field) + r'\s*:\s*(.+)$').match(pt)
        if pm:
            return pm.group(1)
        if re.fullmatch(r'\s*' + re.escape(field) + r'\s*', pt):
            return field                    # shorthand: chase the local
    return None


_SCHEME_FMT = re.compile(r'^(?:/|\{|[A-Za-z][A-Za-z0-9+.-]*://\{)')
_status_memo = {}
_CALL_HEAD = re.compile(r'\s*((?:[A-Za-z_][A-Za-z0-9_]*::)*)'
                        r'([A-Za-z_][A-Za-z0-9_]*)\s*(!?)\s*'
                        r'(?:::<[^<>]*>)?\s*\(')
_ROOT_FIELDS = re.compile(r'(?:ref\s+)?([A-Za-z_][A-Za-z0-9_]*)'
                          r'((?:\s*\.\s*'
                          r'[A-Za-z_][A-Za-z0-9_]*\b(?!\s*\())*)')
_FIELD_ITER = re.compile(r'\.\s*([A-Za-z_][A-Za-z0-9_]*)')
_NEUTRAL_RE = re.compile(r'^\s*(?:None|Vec::new\s*\(\s*\)'
                         r'|Vec::with_capacity\s*\([^)]*\)'
                         r'|String::new\s*\(\s*\))\s*$')
_WRAP_RE = re.compile(r'^\s*(?:Some|Ok)\s*\(')
_FMT_LIT = re.compile(r'\s*"([^"]*)"\s*$')
_COMMA_CLOSE = re.compile(r'[,)]')


def _path_expr_status(expr, fnbody, params, path, depth=0, seen=None):
    """('safe', None) — proven absolute/tempdir-derived;
    ('param', {names}) — rooted only at fn parameters ('self' included);
    ('bad', None) — relative literal or unprovable."""
    if expr is None or depth > 10:
        return ('bad', None)
    _mk = (expr, id(fnbody), path) if depth == 0 else None
    if _mk is not None and _mk in _status_memo:
        return _status_memo[_mk]
    _r = _path_expr_status_inner(expr, fnbody, params, path, depth, seen)
    if _mk is not None:
        _status_memo[_mk] = _r
    return _r


_binds_memo = {}


def _binds_of(fnbody, root):
    _bk = (id(fnbody), root)
    if _bk in _binds_memo:
        return _binds_memo[_bk]
    lets = _rx(r'\blet\s+([^=;\n]*?\b' + re.escape(root)
                      + r'\b[^=;\n]*)=\s*([^;]+);').findall(fnbody)
    fors = _rx(r'\bfor\s+[^{\n]*?\b' + re.escape(root)
                      + r'\b[^{\n]*?\s+in\s+([^{\n]+)').findall(fnbody)
    asgn = _rx(r'(?<![\w.])(?<!let )(?<!mut )' + re.escape(root)
                      + r'\s*=(?!=)\s*([^;]+);').findall(fnbody)
    push = _rx(r'\b' + re.escape(root)
                      + r'\s*\.\s*push\s*\(([^;]*)\)\s*;').findall(fnbody)
    _binds_memo[_bk] = (lets, fors + asgn + push)
    return _binds_memo[_bk]


def _path_expr_status_inner(expr, fnbody, params, path, depth, seen):
    seen = seen or frozenset()
    expr = expr.strip().lstrip('&').strip()
    if expr.startswith('mut '):
        expr = expr[4:].strip()
    if expr.startswith('*'):
        expr = expr[1:].strip()
    if not expr:
        return ('bad', None)
    pc = _PATH_CTOR.search(expr)
    if pc is not None:
        inner = _COMMA_CLOSE.split(expr[pc.end():], maxsplit=1)[0]
        return _path_expr_status(inner, fnbody, params, path, depth + 1, seen)
    if expr[0] == '(':
        parts = _split_args_text(expr[1:])
        if parts is None:
            return ('bad', None)
        pnames = set()
        for pt in parts:
            st, pn = _path_expr_status(pt, fnbody, params, path,
                                       depth + 1, seen)
            if st == 'bad':
                return ('bad', None)
            if st == 'param':
                pnames |= pn
        return ('param', pnames) if pnames else ('safe', None)
    if expr[0] == '"':
        lit0 = expr[1:expr.find('"', 1)] if '"' in expr[1:] else expr[1:]
        if lit0.startswith('/'):
            return ('safe', None)
        if lit0.startswith('sqlite:') and 'memory' in lit0:
            return ('safe', None)        # in-memory database: no path
        return ('bad', None)
    cm = _CALL_HEAD.match(expr)
    if cm and cm.group(3) == '!':
        if cm.group(2) in ('env', 'option_env'):
            return ('safe', None) if _chain_ok(expr) else ('bad', None)
        if cm.group(2) in ('format', 'concat'):
            args = _split_args_text(expr[cm.end():])
            if not args:
                return ('bad', None)
            fl = _FMT_LIT.match(args[0])
            if fl is None:
                return ('bad', None)
            if not args[1:]:
                return (('safe', None) if fl.group(1).startswith('/')
                        else ('bad', None))
            if not _SCHEME_FMT.match(fl.group(1)):
                return ('bad', None)
            pnames = set()
            for a in args[1:]:
                st, pn = _path_expr_status(a, fnbody, params, path,
                                           depth + 1, seen)
                if st == 'bad':
                    return ('bad', None)
                if st == 'param':
                    pnames |= pn
            return ('param', pnames) if pnames else ('safe', None)
        return ('bad', None)
    if cm:
        callee = cm.group(2)
        args = _split_args_text(expr[cm.end():])
        _locally = (callee in fn_index.get(path, {})
                    or (len(shared_bodies.get(callee, ())) == 1
                        and shared_bodies[callee][0][0] is not AMBIGUOUS))
        if not _locally:
            _pref8 = [x for x in cm.group(1).rstrip(':').split('::') if x]
            if not _chain_ok(expr):
                return ('bad', None)
            if callee in STD_SAFE_CALLEES \
                    or (_pref8 and _pref8[-1] in STD_SAFE_PREFIXES):
                return ('safe', None)
            if callee in STD_ARG_DERIVED:
                if args is None or not args:
                    return ('bad', None)
                return _path_expr_status(args[0], fnbody, params, path,
                                         depth + 1, seen)
            return ('bad', None)
        pnames = set()
        for st, pn in _return_statuses(path, callee):
            if st == 'bad':
                return ('bad', None)
            if st == 'param':
                # map the callee's param names to THIS call's arguments
                cparams = None
                fmc = _fnmaps.get(path)
                body2 = fn_index.get(path, {}).get(callee)
                bp2 = path
                if body2 is None or body2 is AMBIGUOUS:
                    ent = shared_bodies.get(callee, ())
                    if len(ent) == 1:
                        bp2 = ent[0][1]
                fmc = _fnmaps.get(bp2)
                if fmc is not None:
                    for hl, hn, _ht, _hc in fmc._heads:
                        if hn == callee:
                            _, _, cparams = fmc.at(hl)
                            break
                if cparams is None or args is None:
                    return ('bad', None)
                for n in pn:
                    if n == 'self' or n not in cparams:
                        return ('bad', None)
                    ai = cparams.index(n)
                    if ai >= len(args):
                        return ('bad', None)
                    st2, pn2 = _path_expr_status(args[ai], fnbody, params,
                                                 path, depth + 1, seen)
                    if st2 == 'bad':
                        return ('bad', None)
                    if st2 == 'param':
                        pnames |= pn2
        return ('param', pnames) if pnames else ('safe', None)
    m = _ROOT_FIELDS.match(expr)
    if not m:
        return ('bad', None)
    if not _chain_ok(expr):
        return ('bad', None)
    root, fields = m.group(1), _FIELD_ITER.findall(m.group(2) or '')
    if root == 'self':
        if fields:
            return ('param', {'self.' + fields[0]})
        return ('param', {'self'})
    if (root, path, id(fnbody)) in seen:
        return ('bad', None)
    seen = seen | {(root, path, id(fnbody))}
    pnames_extra = set()
    binds = []
    _lets5, _rest5 = ([], []) if fnbody is None else _binds_of(fnbody, root)
    if fnbody is not None:
        for pat5, rhs5 in _lets5:
            pat5 = pat5.strip()
            if pat5.startswith('('):
                # tuple destructuring: project the MATCHING position only
                parts5 = _split_args_text(pat5[1:] + ')')
                idx5 = None
                if parts5 is not None:
                    for k6, pp in enumerate(parts5):
                        if _rx(r'\b' + re.escape(root) + r'\b').search(pp):
                            idx5 = k6
                            break
                if idx5 is None:
                    return ('bad', None)
                rhs5s = rhs5.strip()
                if rhs5s.startswith('('):
                    el5 = _split_args_text(rhs5s[1:])
                    if el5 is None or idx5 >= len(el5):
                        return ('bad', None)
                    binds.append(el5[idx5])
                    continue
                cm5 = _CALL_HEAD.match(rhs5s)
                if not cm5:
                    return ('bad', None)
                got5 = False
                _rex5 = _return_exprs(path, cm5.group(2))
                for ex5, bod5, par5, bp5 in (_rex5 or ()):
                    ex5 = ex5.strip()
                    if not ex5.startswith('('):
                        return ('bad', None)
                    el5 = _split_args_text(ex5[1:])
                    if el5 is None or idx5 >= len(el5):
                        return ('bad', None)
                    st5, pn5 = _path_expr_status(el5[idx5], bod5, par5,
                                                 bp5, depth + 1, seen)
                    if st5 == 'bad':
                        return ('bad', None)
                    if st5 == 'safe':
                        binds.append('"/proven"')
                    if st5 == 'param':
                        # callee params -> this call's arguments
                        ca5 = _split_args_text(rhs5s[cm5.end():])
                        if ca5 is None or par5 is None:
                            return ('bad', None)
                        for n5 in pn5:
                            if n5 not in par5 \
                                    or par5.index(n5) >= len(ca5):
                                return ('bad', None)
                            st6, pn6 = _path_expr_status(
                                ca5[par5.index(n5)], fnbody, params,
                                path, depth + 1, seen)
                            if st6 == 'bad':
                                return ('bad', None)
                            if st6 == 'param':
                                pnames_extra.update(pn6)
                        binds.append('"/proven"')
                    got5 = True
                if not got5:
                    return ('bad', None)
                continue
            binds.append(rhs5)
        binds += _rest5
    # inert value sources contribute no path; Some/Ok wrappers unwrap
    kept = []
    for b in binds:
        if _NEUTRAL_RE.match(b.strip()):
            continue
        wm = _WRAP_RE.match(b)
        if wm:
            inner = _split_args_text(b[wm.end():])
            if inner:
                kept.append(inner[0])
                continue
        kept.append(b)
    kept2 = []
    for b in kept:
        bm2 = _ROOT_FIELDS.match(b.strip())
        if bm2 and bm2.group(1) == root and _chain_ok(b):
            continue                     # shadowing identity rebinding
        kept2.append(b)
    binds = kept2 if kept2 else kept
    if not binds and fnbody is not None:
        # INLINE closure parameter (`.filter_map(|e| …e…)`): the value is
        # an ELEMENT of the map-family receiver — prove the receiver chain
        for icm in re.finditer(
                r'\.\s*(?:map|filter_map|and_then|flat_map|for_each'
                r'|map_while|find|filter|any|all|find_map|retain'
                r'|inspect)\s*\(\s*(?:move\s*)?\|([^|]*)\|', fnbody):
            names6 = re.findall(r'[A-Za-z_][A-Za-z0-9_]*', icm.group(1))
            if root not in names6:
                continue
            # receiver: walk BACKWARD over the flat body text
            jj = icm.start() - 1
            depth6 = 0
            out6 = []
            while jj >= 0:
                ch6 = fnbody[jj]
                if ch6 in ')]}':
                    depth6 += 1
                elif ch6 in '([{':
                    if depth6 == 0:
                        break
                    depth6 -= 1
                elif depth6 == 0 and ch6 in '=;,!&|<>?':
                    break
                out6.append(ch6)
                jj -= 1
            recv6 = ''.join(reversed(out6)).strip()
            if recv6:
                binds.append(recv6)
        if binds:
            binds = binds[:4]
    if not binds and fnbody is not None:
        # `let f = |dir: &Path| { … }` — a closure-param root is proven by
        # the closure's OWN call sites inside this fn
        for cn, cps in _rx(r'\blet\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*'
                r'(?:move\s*)?\|([^|]*)\|').findall(fnbody):
            names5 = [nm5.group(1) for nm5 in _rx(r'(?:^|,)\s*(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)').finditer(cps)]
            if root not in names5:
                continue
            _ai5 = names5.index(root)
            for _cargs in _rx(r'\b' + re.escape(cn)
                                      + r'\s*\(').finditer(fnbody):
                _al5 = _split_args_text(fnbody[_cargs.end():])
                if _al5 is None or _ai5 >= len(_al5):
                    return ('bad', None)
                binds.append(_al5[_ai5])
    if not binds:
        if params and root in params:
            return ('param', {root})
        return ('bad', None)
    pnames = set()
    for b in binds:
        if fields:
            # MULTI-LEVEL field projection (Codex R35 follow-through): walk
            # `root.f1.f2…` by resolving each constructor's struct-literal
            # field init in ITS OWN body context; the terminal expression is
            # then proven like any other. No whole-value pre-check — a
            # struct-literal return is opaque as a VALUE but its fields are
            # individually provable.
            _curb, _curbody, _curparams, _curpath = b, fnbody, params, path
            _fchain = list(fields)
            _fok = True
            while _fchain:
                _fd = _fchain.pop(0)
                bc = _CALL_HEAD.match(_curb.strip())
                if not bc or bc.group(3) == '!':
                    _fok = False
                    break
                body2 = fn_index.get(_curpath, {}).get(bc.group(2))
                bp2 = _curpath
                if body2 is None or body2 is AMBIGUOUS:
                    ent = shared_bodies.get(bc.group(2), ())
                    if len(ent) == 1 and ent[0][0] is not AMBIGUOUS:
                        body2, bp2 = ent[0]
                if body2 is None or body2 is AMBIGUOUS:
                    _fok = False
                    break
                _rets2 = (re.findall(r'\breturn\s+([^;]+);', body2)
                          + [_tail_expr_of(body2)])
                _fex2 = None
                for _re2 in _rets2:
                    _re2s = _re2.strip()
                    # a ctor returning a LOCAL (`let fixture = Self {…};
                    # … fixture`) dereferences to its struct literal
                    if re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', _re2s):
                        _lets8, _rest8 = _binds_of(body2, _re2s)
                        _c8 = [r for _p8, r in _lets8] + _rest8
                        if len(_c8) == 1:
                            _re2s = _c8[0]
                    _fx = _struct_field_expr(_re2s, _fd)
                    if _fx is not None:
                        _fex2 = _fx
                        break
                if _fex2 is None:
                    _fok = False
                    break
                fmc = _fnmaps.get(bp2)
                cparams2 = None
                if fmc is not None:
                    for hl, hn, _ht, _hc in fmc._heads:
                        if hn == bc.group(2):
                            _, _, cparams2 = fmc.at(hl)
                            break
                if _fex2.strip() == _fd:
                    # shorthand init: the ctor's local (or param) feeds it
                    _curb2 = None
                    if cparams2 and _fd in cparams2:
                        # ctor param: map through the CONSTRUCTION call args
                        cargs2 = _split_args_text(
                            _curb.strip()[bc.end():])
                        if cargs2 is None \
                                or cparams2.index(_fd) >= len(cargs2):
                            _fok = False
                            break
                        _curb = cargs2[cparams2.index(_fd)]
                        # context stays the CALLER's for the mapped arg
                        continue
                    _lets9, _rest9 = _binds_of(body2, _fd)
                    _cands9 = [r for _p9, r in _lets9] + _rest9
                    # a SHADOWING rebinding whose root is the same name and
                    # whose methods all preserve pathness is an identity
                    # transform — it adds no requirement
                    _cands9 = [c9 for c9 in _cands9
                               if not (_ROOT_FIELDS.match(c9.strip())
                                       and _ROOT_FIELDS.match(
                                           c9.strip()).group(1) == _fd
                                       and _chain_ok(c9))]
                    if not _cands9:
                        _fok = False
                        break
                    if len(_cands9) > 1:
                        if _fchain:
                            _fok = False
                            break
                        # terminal field, several sources: ALL must prove
                        for _c10 in _cands9:
                            _st10, _pn10 = _path_expr_status(
                                _c10, body2, cparams2, bp2,
                                depth + 1, seen)
                            if _st10 != 'safe':
                                _fok = False
                                break
                        if not _fok:
                            break
                        _curb = '"/proven"'
                        continue
                    _curb, _curbody, _curparams, _curpath = \
                        _cands9[0], body2, cparams2, bp2
                    continue
                _curb, _curbody, _curparams, _curpath = \
                    _fex2, body2, cparams2, bp2
            if not _fok:
                return ('bad', None)
            st, pn = _path_expr_status(_curb, _curbody, _curparams,
                                       _curpath, depth + 1, seen)
            if st == 'bad':
                return ('bad', None)
            if st == 'param':
                if _curpath == path and _curbody is fnbody:
                    pnames |= pn
                else:
                    # params of a DIFFERENT fn context: map through the
                    # original construction call's arguments
                    bc0 = _CALL_HEAD.match(b.strip())
                    cargs0 = (_split_args_text(b.strip()[bc0.end():])
                              if bc0 else None)
                    fmc0 = _fnmaps.get(_curpath)
                    cp0 = _curparams
                    if cargs0 is None or cp0 is None:
                        return ('bad', None)
                    for n9 in pn:
                        if n9 not in cp0 or cp0.index(n9) >= len(cargs0):
                            return ('bad', None)
                        st9, pn9 = _path_expr_status(
                            cargs0[cp0.index(n9)], fnbody, params, path,
                            depth + 1, seen)
                        if st9 == 'bad':
                            return ('bad', None)
                        if st9 == 'param':
                            pnames |= pn9
            continue
            continue
        st, pn = _path_expr_status(b, fnbody, params, path, depth + 1, seen)
        if st == 'bad':
            return ('bad', None)
        if st == 'param':
            pnames |= pn
    pnames |= pnames_extra
    return ('param', pnames) if pnames else ('safe', None)


_tmark('pre-phaseA')
# ---- phase A: classify every direct path site --------------------------
fs_cwd_fns = {}      # path -> {fn names with an unproven path site} | {'*'}
_obligations = {}    # (path, fn) -> {param positions | ('self', field|None)}
_fnmaps = {}
for _p2, _c2 in FILES:
    _fnmaps[_p2] = _FnMap(_c2, raw_index[_p2])


def _oblig_of(_prA, _pnA):
    """Map param NAMES from a status to obligation positions; None when a
    name cannot be mapped (caller must fail closed)."""
    _pos = set()
    for _n in _pnA:
        if _n == 'self':
            _pos.add(('self', None))
        elif _n.startswith('self.'):
            _pos.add(('self', _n[5:]))
        elif _prA and _n in _prA:
            _pos.add(_prA.index(_n))
        else:
            return None
    return _pos
_PA_FREE = re.compile(
    r'(?<![\w.])((?:[A-Za-z_][A-Za-z0-9_]*::)*)('
    + '|'.join(sorted(PATH_ARG_CALLS)) + r')\s*' + _TFP + r'\(')
_PA_METH = re.compile(
    r'\.\s*(' + '|'.join(sorted(PATH_ARG_METHODS)) + r')\s*' + _TFP + r'\(')
_PA_QUAL = re.compile(
    r'\b((?:[A-Za-z_][A-Za-z0-9_]*::)+)('
    + '|'.join(sorted(PATH_QUAL_ONLY)) + r')\s*' + _TFP + r'\(')
_PA_RECV = re.compile(
    r'\.\s*(' + '|'.join(PATH_RECV_METHODS) + r')\s*\(\s*\)')
_PA_QUICK = re.compile('|'.join(sorted(
    set(PATH_ARG_CALLS) | set(PATH_ARG_METHODS) | set(PATH_QUAL_ONLY)
    | set(PATH_RECV_METHODS))))

for _p2, _c2 in FILES:
    _rawP = raw_index[_p2]
    _fm = _fnmaps[_p2]
    _hits = fs_cwd_fns.setdefault(_p2, set())

    def _site(_lnA, _exprs, _tag='?'):
        _fnA, _fbA, _prA = _fm.at(_lnA)
        for _exA in _exprs:
            _stA, _pnA = _path_expr_status(_exA, _fbA, _prA, _p2)
            if os.environ.get('SERIAL_CLASSIFY_FSDBG2') == '1' \
                    and _stA != 'safe':
                print('site\t%s:%d\t%s\t%s\t%r' % (
                    _p2, _lnA + 1, _tag, _stA, (_exA or '')[:60]),
                    file=sys.stderr)
            if _stA == 'bad' or (_stA == 'param' and not _fnA):
                _hits.add(_fnA if _fnA else '*')
            elif _stA == 'param':
                if _fnA == 'drop' and all(n.startswith('self')
                                          for n in _pnA):
                    # a destructor replays a FIELD path: prove every
                    # struct-literal construction's field init instead —
                    # a destructor has no call sites to obligate.
                    if _p2 not in _impl_owner_memo:
                        _impl_owner_memo[_p2] = _impl_owner_map(_c2)
                    _ownD = _impl_owner_memo[_p2][_lnA] \
                        if _lnA < len(_impl_owner_memo[_p2]) else None
                    _fields = set(n[5:] for n in _pnA
                                  if n.startswith('self.'))
                    if _ownD in (None, '?') or not _fields:
                        _hits.add('drop')
                        continue
                    _okD = True
                    _sawD = False
                    for _lnC, _blC in enumerate(_c2):
                        for _cmC in _rx(r'(?<![A-Za-z0-9_])(?:'
                                + re.escape(_ownD) + r'|Self)\s*\{').finditer(_blC):
                            headC = _blC[:_cmC.start()]
                            if re.search(r'\b(?:impl|struct|enum|trait'
                                         r'|union|mod)\b', headC) \
                                    or headC.rstrip().endswith('->'):
                                continue
                            _clC, _txC = delimit(_c2, _lnC,
                                                 _cmC.end() - 1)
                            _fnC, _fbC, _prC = _fm.at(_lnC)
                            for _fd in _fields:
                                _feC = _struct_field_expr(
                                    '{' + (_txC or ''), _fd)
                                if _feC == _fd and _fbC is not None:
                                    _stC, _ = _path_expr_status(
                                        _fd, _fbC, _prC, _p2)
                                elif _feC is not None:
                                    _stC, _ = _path_expr_status(
                                        _feC, _fbC, _prC, _p2)
                                else:
                                    _stC = 'bad'
                                if _stC != 'safe':
                                    _okD = False
                            _sawD = True
                    if not (_okD and _sawD):
                        _hits.add('drop')
                    continue
                _pos = _oblig_of(_prA, _pnA)
                if _pos is None:
                    _hits.add(_fnA)
                else:
                    _obligations.setdefault((_p2, _fnA),
                                            set()).update(_pos)

    # Codex TA-01 R36 P0: aliases of std path APIs (`use std::fs::write as
    # persist`) are path sites under their ALIAS spelling. A full captured
    # path decides fs-family membership exactly; a bare target counts only
    # when nothing local shadows it (a scanned definition is analyzed at
    # its own body instead).
    _alias_calls = {}
    for _al9, _fp9 in rename_path_index.get(_p2, {}).items():
        _seg9 = [x for x in _fp9.split('::') if x]
        if not _seg9 or _seg9[-1] not in PATH_ARG_CALLS \
                and _seg9[-1] not in PATH_QUAL_ONLY:
            continue
        _isfs9 = any(x in ('fs', 'path') for x in _seg9[:-1]) \
            or (_seg9[:-1][-1:] or [''])[0] in ('File', 'OpenOptions')
        if len(_seg9) == 1:
            if _al9 in shared_fn_names or any(
                    _seg9[-1] in fn_index.get(_pp9, {})
                    for _pp9 in mod_closure(_p2)):
                continue
            _isfs9 = True                # bare unshadowed target: assume std
        if _isfs9:
            _alias_calls[_al9] = PATH_ARG_CALLS.get(
                _seg9[-1], PATH_QUAL_ONLY.get(_seg9[-1], (0,)))
    _ALIAS_PAT = (re.compile(
        r'(?<![\w.])(' + '|'.join(sorted(map(re.escape, _alias_calls)))
        + r')\s*' + _TFP + r'\(') if _alias_calls else None)

    for _ln2, _bl2 in enumerate(_c2):
        if _ALIAS_PAT is not None:
            for _cm6 in _ALIAS_PAT.finditer(_bl2):
                _args6 = _call_args_at(_rawP, _ln2, _cm6.end())
                _idx6 = _alias_calls[_cm6.group(1)]
                if _args6 is None:
                    _fnA, _, _ = _fm.at(_ln2)
                    _hits.add(_fnA if _fnA else '*')
                    continue
                _site(_ln2, [_args6[_i7] if _i7 < len(_args6) else None
                             for _i7 in _idx6], _cm6.group(1))
        if not _PA_QUICK.search(_bl2):
            continue
        for _pat, _table in ((_PA_FREE, PATH_ARG_CALLS),
                             (_PA_QUAL, PATH_QUAL_ONLY)):
            for _cm6 in _pat.finditer(_bl2):
                _nmP = _cm6.group(2)
                _prefP = [x for x in
                          _cm6.group(1).rstrip(':').split('::')
                          if x and x not in ('self', 'crate')]
                # a spelling that RESOLVES LOCALLY is not the std fs API —
                # the local definition's own sites are analyzed at its body
                if not _prefP:
                    if _nmP in rename_index.get(_p2, {}) \
                            or _nmP in shared_fn_names \
                            or any(_nmP in fn_index.get(_pp9, {})
                                   for _pp9 in mod_closure(_p2)):
                        continue
                elif _prefP[0] in mod_index.get(_p2, ()) \
                        or _prefP[0] in type_index.get(_p2, ()):
                    continue
                _args6 = _call_args_at(_rawP, _ln2, _cm6.end())
                _idx6 = _table[_nmP]
                if _args6 is None:
                    _fnA, _, _ = _fm.at(_ln2)
                    _hits.add(_fnA if _fnA else '*')
                    continue
                _site(_ln2, [_args6[_i7] if _i7 < len(_args6) else None
                             for _i7 in _idx6], _nmP)
        for _cm6 in _PA_METH.finditer(_bl2):
            _args6 = _call_args_at(_rawP, _ln2, _cm6.end())
            _idx6 = PATH_ARG_METHODS[_cm6.group(1)]
            if _args6 is None:
                _fnA, _, _ = _fm.at(_ln2)
                _hits.add(_fnA if _fnA else '*')
                continue
            _site(_ln2, [_args6[_i7] if _i7 < len(_args6) else None
                         for _i7 in _idx6], _cm6.group(1))
        for _cm6 in _PA_RECV.finditer(_bl2):
            _site(_ln2, [_recv_expr_at(_rawP, _ln2, _cm6.start())],
                  '.' + _cm6.group(1))

_tmark('phaseA-done')
# ---- phase B: propagate parameter obligations to CALL SITES ------------
# A helper whose path roots at its own parameter is safe exactly when every
# caller proves the argument at that position; a caller forwarding its own
# parameter inherits the obligation (bounded fixpoint). Obligated names are
# matched only where the helper is actually VISIBLE — its own file, files
# whose mod closure includes it, and everywhere for the shared prefixes —
# so a test-local `fn command` never taints unrelated files' `command`
# variables.
def _oblig_visible_names(fpath, focus=None):
    """Obligated names visible from fpath; `focus` (round N>1) restricts
    to the names whose obligations changed last round — everything else
    was already fully checked."""
    out = {}
    closure = set(mod_closure(fpath))
    for (_op, _ofn), _pos in _obligations.items():
        if focus is not None and _ofn not in focus:
            continue
        if _op == fpath or _op in closure \
                or _op.startswith(SHARED_PREFIXES):
            out.setdefault(_ofn, set()).update(_pos)
    return out


_ob_regex_cache = {}
_joined_cache = {}
_focus = None
for _round in range(12):
    _newly = {}
    if not _obligations:
        break
    for _p2, _c2 in FILES:
        _byn = _oblig_visible_names(_p2, _focus)
        if not _byn:
            continue
        _rawP = raw_index[_p2]
        _fm = _fnmaps[_p2]
        _hits = fs_cwd_fns[_p2]
        _ck = frozenset(_byn)
        if _ck not in _ob_regex_cache:
            _alt = '|'.join(sorted(map(re.escape, _byn)))
            _ob_regex_cache[_ck] = (
                re.compile(r'(?<![\w.])((?:[A-Za-z_][A-Za-z0-9_]*::)*)('
                           + _alt + r')\s*' + _TFP + r'\('),
                re.compile(r'\.\s*()(' + _alt + r')\s*' + _TFP + r'\('),
                re.compile(r'\b(' + _alt + r')\b(?!\s*(?:'
                           + _TFP + r'\(|::|!|:))'),
                re.compile(_alt))
        _OB, _OBM, _OBREF, _OBQ = _ob_regex_cache[_ck]
        if _p2 not in _joined_cache:
            _jt = '\n'.join(_c2)
            _offs = [0]
            for _l9 in _c2:
                _offs.append(_offs[-1] + len(_l9) + 1)
            _joined_cache[_p2] = (_jt, _offs)
        _jt, _offs = _joined_cache[_p2]

        def _lineof(_pos9):
            import bisect as _bis
            return _bis.bisect_right(_offs, _pos9) - 1
        _closure2 = set(mod_closure(_p2))
        _use_span = set()
        _lu = 0
        while _lu < len(_c2):
            if re.match(r'\s*(?:pub\s*(?:\([^)]*\)\s*)?)?use\b',
                        _c2[_lu]):
                _le = _lu
                while _le < len(_c2) and ';' not in _c2[_le]:
                    _le += 1
                _use_span.update(range(_lu, _le + 1))
                _lu = _le + 1
                continue
            _lu += 1

        def _resolves_to_obligated(_pref7, _ofn7):
            """Does this call site actually reach an obligated def? A
            qualifier is resolved through visible local types / modules —
            an external qualifier (std's Command::new) never does; a bare
            name resolves through the same-file/closure/shared chain. A
            METHOD spelling (empty prefix from _OBM) checks any visible
            obligated def of that name (receiver types are not tracked)."""
            _segs7 = [x for x in _pref7.rstrip(':').split('::')
                      if x and x not in ('self', 'crate')]
            if _segs7:
                _dp7 = _p2
                for _sg7 in _segs7:
                    if _sg7 == 'Self':
                        continue
                    if _sg7 in type_index.get(_dp7, ()):
                        continue
                    _vt7 = visible_types(_p2).get(_sg7)
                    if isinstance(_vt7, str):
                        _dp7 = _vt7
                        continue
                    if _sg7 in mod_index.get(_dp7, ()):
                        _nx7 = resolve_mod_file(_dp7, _sg7)
                        if _nx7 is not None:
                            _dp7 = _nx7
                            continue
                    return None          # external qualifier: not ours
                return _dp7 if (_dp7, _ofn7) in _obligations else None
            if _ofn7 == 'drop':
                return None              # bare drop(x) is std::mem::drop
            if _ofn7 in fn_index.get(_p2, {}):
                return _p2 if (_p2, _ofn7) in _obligations else None
            for _cp7 in _closure2:
                if _ofn7 in fn_index.get(_cp7, {}):
                    return (_cp7
                            if (_cp7, _ofn7) in _obligations else None)
            for (_op7, _on7) in _obligations:
                if _on7 == _ofn7 and _op7.startswith(SHARED_PREFIXES):
                    return _op7
            return None

        _cands = set()
        for _qm9 in _OBQ.finditer(_jt):
            _cands.add(_lineof(_qm9.start()))
        for _ln2 in sorted(_cands):
            _bl2 = _c2[_ln2]
            for _pat6 in (_OB, _OBM):
                for _cm6 in _pat6.finditer(_bl2):
                    _ofn = _cm6.group(2)
                    _fnA, _fbA, _prA = _fm.at(_ln2)
                    if _fnA == _ofn:
                        continue         # the definition/self recursion
                    if _pat6 is _OB:
                        _odp = _resolves_to_obligated(_cm6.group(1), _ofn)
                        if _odp is None:
                            continue
                        _obset = _obligations[(_odp, _ofn)]
                    else:
                        _obset = _byn[_ofn]
                    _args6 = _call_args_at(_rawP, _ln2, _cm6.end())
                    for _i7 in sorted(_obset,
                                      key=lambda x: (isinstance(x, tuple),
                                                     str(x))):
                        if isinstance(_i7, tuple):
                            # ('self', field): prove the method RECEIVER
                            # (and project the named field)
                            _dcol = _bl2.rfind('.', 0, _cm6.start(2))
                            _exA = (_recv_expr_at(_rawP, _ln2, _dcol)
                                    if _dcol != -1 else None)
                            if _exA is not None and _i7[1]:
                                _exA = _exA + '.' + _i7[1]
                        else:
                            _exA = (_args6[_i7]
                                    if _args6 is not None
                                    and _i7 < len(_args6) else None)
                        _stA, _pnA = _path_expr_status(_exA, _fbA, _prA,
                                                       _p2)
                        if _stA == 'bad' or (_stA == 'param'
                                             and not _fnA):
                            _hits.add(_fnA if _fnA else '*')
                        elif _stA == 'param':
                            _pos = _oblig_of(_prA, _pnA)
                            _key = (_p2, _fnA)
                            if _pos is None:
                                _hits.add(_fnA)
                            elif not _pos <= _obligations.get(_key,
                                                             set()):
                                _newly.setdefault(_key,
                                                  set()).update(_pos)
            if _ln2 in _use_span:
                continue                 # imports name the helper, not a use
            for _cm6 in _OBREF.finditer(_bl2):
                # an obligated helper escaping as a VALUE: its future
                # arguments are unknowable — the referring fn pays. Only
                # refs that RESOLVE to the helper count: the name must not
                # be shadowed by a let/for binding or a param in the
                # enclosing fn, and must reach an obligated def.
                _ofn = _cm6.group(1)
                _fnA, _fbA, _prA = _fm.at(_ln2)
                if _fnA == _ofn:
                    continue
                if _resolves_to_obligated('', _ofn) is None:
                    continue
                if _fbA is not None and _rx(r'\b(?:let|for)\s+[^=;\n]*?\b' + re.escape(_ofn)
                        + r'\b').search(_fbA):
                    continue             # local binding shadows the fn
                if _prA and _ofn in _prA:
                    continue             # parameter shadows the fn
                _hits.add(_fnA if _fnA else '*')
    if not _newly:
        break
    _focus = set(k[1] for k in _newly)
    for _key, _pos in _newly.items():
        _obligations.setdefault(_key, set()).update(_pos)

_tmark('phaseB-done')


def fs_lane_of(path, name):
    """Codex TA-01 R33 P0: does this fn identity carry an unproven
    path-argument site? Attached at each judgement-key introduction point
    (expand return, drop merge, the top-level driver) — NEVER inside
    judge_body, whose expansion memo must stay context-independent."""
    n2 = 'drop' if str(name).startswith('drop@') else name
    fset = fs_cwd_fns.get(path)
    return bool(fset) and (n2 in fset or '*' in fset)


if os.environ.get('SERIAL_CLASSIFY_FSDBG') == '1':
    for _p2 in sorted(fs_cwd_fns):
        if fs_cwd_fns[_p2]:
            print('fsdbg\t%s\t%s' % (_p2, ','.join(
                sorted(str(h) for h in fs_cwd_fns[_p2]))), file=sys.stderr)

_visible_free_cache = {}

def visible_free_fns(file_path):
    """Like visible_fns, but restricted to fns a BARE IDENTIFIER can
    reference as a VALUE — fns outside impl/trait blocks (inherent assoc fns
    and methods need `Type::name`, trait fns are not values). A name that is
    both free and assoc in one file keeps its AMBIGUOUS fn_index entry."""
    if file_path in _visible_free_cache:
        return _visible_free_cache[file_path]
    out = {}
    for p in mod_closure(file_path):
        for nm in free_fn_names.get(p, ()):
            body = fn_index.get(p, {}).get(nm, AMBIGUOUS)
            if nm in out:
                out[nm] = AMBIGUOUS
            else:
                out[nm] = AMBIGUOUS if body is AMBIGUOUS else (body, p)
    _visible_free_cache[file_path] = out
    return out

_tmark('r23-done')
# Untracked bare/pointer references to set_var / remove_var (or an alias):
# the key is unknowable. Resolved `use … as` / const / static / type binding
# DECLARATIONS are exempt — their aliases are tracked by the rename layer and
# scanned as mutation sites under the alias spelling.
_DECL_USE = re.compile(r'^\s*(?:pub\s*(?:\([^)]*\)\s*)?)?use\b')
_DECL_BIND = re.compile(r'^\s*(?:pub\s*(?:\([^)]*\)\s*)?)?'
                        r'(?:const|static|type)\s+(?!fn\b)'
                        r'([A-Za-z_][A-Za-z0-9_]*)')
for _p2, _c2 in FILES:
    _names = ['set_var', 'remove_var']
    for _al, _tgt in rename_index[_p2].items():
        if _tgt.split('::')[-1] in ('set_var', 'remove_var'):
            _names.append(_al)
    _ref = re.compile(r'\b(?:'
                      + '|'.join(sorted(set(map(re.escape, _names))))
                      + r')\b(?!\s*' + _TFQ + r'\()')
    # A decl spans every line through its terminating ';' — exempt the WHOLE
    # span when its binder resolved into the rename layer (the alias is then
    # scanned as a mutation site under the alias spelling); an unresolved
    # binder poisons the span instead.
    _exempt, _unres = set(), set()
    _ln2 = 0
    while _ln2 < len(_c2):
        _du = _DECL_USE.match(_c2[_ln2])
        _bm = _DECL_BIND.match(_c2[_ln2])
        if not (_du or _bm):
            _ln2 += 1
            continue
        _l3 = _ln2
        while _l3 < len(_c2) and ';' not in _c2[_l3]:
            _l3 += 1
        _txt = '\n'.join(_c2[_ln2:_l3 + 1])
        if _du:
            _ok = all(a in rename_index[_p2] for a in _rx(r'\bas\s+([A-Za-z_][A-Za-z0-9_]*)').findall(_txt))
        else:
            _ok = _bm.group(1) in rename_index[_p2]
        (_exempt if _ok else _unres).update(range(_ln2, _l3 + 1))
        _ln2 = _l3 + 1
    _gate2 = tuple(sorted(set(_names)))
    for _ln2, _bl2 in enumerate(_c2):
        if not any(g in _bl2 for g in _gate2):
            continue
        if not _ref.search(_bl2) or _ln2 in _exempt:
            continue
        if _ln2 in _unres:
            _untraceable(_p2, _ln2, 'unresolved decl binding set_var/'
                         'remove_var (or an alias)')
        else:
            _untraceable(_p2, _ln2, 'bare/pointer reference to set_var/'
                         'remove_var (or an alias)')

# An unresolved include! leaves unscannable source in the test binary — no
# key of any mutation inside it can be enumerated.
for _p2 in sorted(UNRESOLVED_INCLUDES):
    _untraceable(_p2, 0, 'unresolved include! target')

# A file with non-ASCII IDENTIFIERS (visible after blanking) is invisible to
# every ASCII regex above — if it also mentions set_var / remove_var in code,
# no key can be enumerated there (e.g. a unicode alias of set_var).
for _p2, _c2 in FILES:
    _joined = '\n'.join(_c2)
    if NON_ASCII.search(_joined) and \
            re.search(r'\b(?:set_var|remove_var)\b', _joined):
        _untraceable(_p2, 0, 'non-ascii identifiers alongside '
                     'set_var/remove_var')

if BENIGN_DISABLED:
    if _EXPL:
        print('explain: dynamic env mutation is untraceable -> benign '
              'env-read list DISABLED for this run', file=sys.stderr)
    BENIGN_READ_KEYS = frozenset()

bad_benign = sorted(BENIGN_READ_KEYS & MUTATED_ENV_KEYS)
if bad_benign:
    print('FAIL: benign env-read key(s) %s are mutated somewhere in tests/ — '
          'remove them from BENIGN_READ_KEYS' % ','.join(bad_benign),
          file=sys.stderr)
    sys.exit(3)

# Self-check: an allowlisted name redefined in shared scope with a polluted
# body would launder pollution through the allowlist — refuse to run. Every
# same-named shared body is scanned, ambiguous or not.
for nm in sorted(CALL_ALLOW):
    for body, _bp in shared_bodies.get(nm, ()):
        if isinstance(body, str) and any(c in body for c in GLOBAL_CALLS + HASH_CALLS + CWD_CALLS):
            print('FAIL: allowlisted helper %s is defined in shared scope with '
                  'process-wide pollution — remove it from CALL_ALLOW' % nm,
                  file=sys.stderr)
            sys.exit(3)

# ---------- call surface extraction ----------------------------------------
ATTR_IN_BODY = re.compile(r'#!?\[[^\]\n]*\]')

SPAWN_TERMINALS = ('.output(', '.status(', '.spawn(')

def _depth_profile(t):
    d, out = 0, []
    for ch in t:
        if ch == '{':
            d += 1
        out.append(d)
        if ch == '}':
            d -= 1
    return out

def spawn_env_ok(tnorm, spawn_names):
    """True iff EVERY `X::new(` spawn site is discharged by a `.env_clear()`
    proven to run on the same builder before its terminal spawn:
      * direct chain: `X::new(..).…env_clear()…` with no terminal earlier in
        the same statement segment;
      * receiver-bound: `let [mut] r = X::new(..);` followed, at the SAME brace
        depth, by an `r.`-statement containing `.env_clear(` before the first
        `r.`-statement that fires a terminal (`.output(`/`.status(`/`.spawn(`).
    Deeper-brace statements (conditionals, closures) never count; anything
    unresolvable fails closed."""
    depth = _depth_profile(tnorm)

    def stmt_bounds(pos, d):
        start = pos
        while start > 0 and not (tnorm[start - 1] == ';' and depth[start - 1] == d) \
                and tnorm[start - 1] != '{':
            start -= 1
        end = pos
        while end < len(tnorm) and not (tnorm[end] == ';' and depth[end] == d):
            end += 1
        return start, end

    for sn in spawn_names:
        needle = sn + '::new'
        pos = tnorm.find(needle)
        while pos != -1:
            d = depth[pos]
            start, end = stmt_bounds(pos, d)
            stmt = tnorm[pos:end]
            clear_i = stmt.find('.env_clear(')
            if clear_i != -1 and depth[pos + clear_i] != d:
                clear_i = -1              # a clear inside deeper braces proves nothing
            term_i = min((i for i in (stmt.find(t2) for t2 in SPAWN_TERMINALS) if i != -1),
                         default=-1)
            if clear_i != -1 and (term_i == -1 or clear_i < term_i):
                pos = tnorm.find(needle, pos + 1)
                continue                          # discharged inside its own chain
            if term_i != -1:
                return False                      # spawned in-chain, never cleared
            head = tnorm[start:pos]
            rm = _rx(r'let\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*$').search(head)
            if not rm:
                return False                      # expression position: unresolvable
            r = rm.group(1)
            i = end + 1
            cleared = False
            while i < len(tnorm):
                if depth[min(i, len(tnorm) - 1)] < d:
                    break                         # left the enclosing block
                s2, e2 = i, i
                while e2 < len(tnorm) and not (tnorm[e2] == ';' and depth[e2] == d):
                    e2 += 1
                seg = tnorm[s2:e2]
                if (r + '.') in seg and depth[min(s2, len(tnorm) - 1)] <= d:
                    c2 = seg.find('.env_clear(')
                    if c2 != -1 and depth[s2 + c2] != d:
                        c2 = -1           # conditional/closure clear proves nothing
                    t2i = min((x for x in (seg.find(t3) for t3 in SPAWN_TERMINALS) if x != -1),
                              default=-1)
                    if c2 != -1 and (t2i == -1 or c2 < t2i):
                        cleared = True
                        break
                    if t2i != -1:
                        return False              # terminal fired before any clear
                i = e2 + 1
            if not cleared:
                # never cleared: only safe if no terminal ever fires in this
                # body — the builder escapes (helper returns it) and the CALLER
                # is judged on its own; fail closed unless nothing spawned here.
                rest = tnorm[end:]
                if any(t3 in rest for t3 in SPAWN_TERMINALS):
                    return False
            pos = tnorm.find(needle, pos + 1)
    return True

_visible_cache = {}

def visible_fns(file_path):
    """fn name -> (body, defining path) | AMBIGUOUS: the file's own fns plus
    the fns of every file it declares via `mod x;` (Codex R6 follow-up: a
    sibling non-shared helper module is precisely visible through its mod
    declaration — no tree-wide guessing)."""
    if file_path in _visible_cache:
        return _visible_cache[file_path]
    out = {}
    for p in mod_closure(file_path):
        for nm, body in fn_index.get(p, {}).items():
            if nm in out:
                out[nm] = AMBIGUOUS
            else:
                out[nm] = AMBIGUOUS if body is AMBIGUOUS else (body, p)
    _visible_cache[file_path] = out
    return out

_rename_re_cache = {}

def resolve_renames(t, file_path):
    """Rewrite per-file aliases back to their original names so every
    downstream rule (blacklist substrings, spawn scan, qualified lanes,
    allowlist) sees the real API (Codex TA-01 R3 P0)."""
    rmap = rename_index.get(file_path) if file_path else None
    if not rmap:
        return t
    if file_path not in _rename_re_cache:
        # Codex TA-01 R16 P0: method resolution in Rust is receiver-based —
        # lexical aliases (use/type/const) can NEVER rename a method call, so
        # method-position tokens (preceded by `.`) must not be rewritten: an
        # alias named `output` must not erase the `.output(` spawn terminal.
        _rename_re_cache[file_path] = re.compile(
            r'(?<!\.)\b(' + '|'.join(sorted(map(re.escape, rmap))) + r')\b')
    return _rename_re_cache[file_path].sub(lambda m: rmap[m.group(1)], t)

FN_DEF_NAME  = re.compile(r'\bfn\s+[A-Za-z_][A-Za-z0-9_]*')
MACRO_CALL   = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)!\s*[\(\[\{]')
METHOD_CALL  = re.compile(r'\.([a-z_][A-Za-z0-9_]*)\s*\(')
PATH_CALL    = re.compile(r'(?<![\w])((?:[A-Za-z_][A-Za-z0-9_]*::)+)([A-Za-z_][A-Za-z0-9_]*)\s*\(')
FREE_CALL    = re.compile(r'(?<![\w.:])([A-Za-z_][A-Za-z0-9_]*)\s*\(')
# identifier OUTSIDE call position: not a field/method (.x), not called (x(),
# x!), not a path prefix (x::), not a struct literal / field name (x{, x:)
REF_IDENT    = re.compile(
    r"(?<![.\w':])([A-Za-z_][A-Za-z0-9_]*)\b(?!\s*(?:\(|!|::|\{|:))")
# path-qualified identifier OUTSIDE call position (`m::F`, `S::F` as a VALUE)
QUAL_REF     = re.compile(
    r"(?<![\w.])((?:[A-Za-z_][A-Za-z0-9_]*::)+)([A-Za-z_][A-Za-z0-9_]*)"
    r"\b(?!\s*(?:\(|::|!|\{))")

def calls_of(text):
    """Extract the call surface of a blanked body: (macros, names, quals).
    `quals` carries the last two segments of every path call (`Type::fn`);
    the rest of the justification rules are name-based."""
    t = norm_calls_text(text)
    t = ATTR_IN_BODY.sub(' ', t)
    t = FN_DEF_NAME.sub(' ', t)
    macros = set(MACRO_CALL.findall(t))
    methods = set(METHOD_CALL.findall(t))
    names = set(n for n in FREE_CALL.findall(t) if n not in KEYWORDS)
    quals = set()
    quals_full = set()
    for prefix, last in PATH_CALL.findall(t):
        segs = [x for x in prefix.rstrip(':').split('::') if x and x != 'self']
        if not segs:
            names.add(last)
            continue
        quals.add((segs[-1] + '::' + last, segs[-1], last))
        if len(segs) > 1 or True:
            quals_full.add((tuple(segs), last))
    return macros, names, methods, quals, quals_full

LANE_ORDER = ('env', 'hash_kind', 'cwd')

def lanes_of(text):
    lanes = []
    if any(c in text for c in GLOBAL_CALLS):
        lanes.append('env')
    if any(c in text for c in HASH_CALLS):
        lanes.append('hash_kind')
    if any(c in text for c in CWD_CALLS):
        lanes.append('cwd')
    return lanes

def check_helper_body(text):
    """ONE-level expansion: judge a helper body with no further expansion.
    Returns (ok, lanes). Any call that is not allowlist / blacklist /
    known-lane / uppercase-constructor — including calls to further helpers,
    i.e. depth two, and recursion — fails the caller closed."""
    lanes = set(lanes_of(text))
    macros, names = calls_of(text)
    for mac in sorted(macros):
        if mac not in MACRO_ALLOW:
            return False, lanes
    for nm in sorted(names):
        if nm in BLACK_NAMES:
            continue              # already contributed a lane above
        if nm in KNOWN_LANE_HELPERS:
            lanes.add(KNOWN_LANE_HELPERS[nm])
            continue
        if nm in CALL_ALLOW:
            continue
        if nm[0].isupper():
            continue              # constructor/variant convention
        return False, lanes       # unknown, nested helper, or recursive: fail closed
    return True, lanes

EXPLAIN = os.environ.get('SERIAL_CLASSIFY_EXPLAIN') == '1'

def _explain(test_fn, why):
    if EXPLAIN:
        print('%s\t%s' % (test_fn, why), file=sys.stderr)

# Bounded transitive judgement (TA-01, wording amended by ER-10 on 2026-08-26:
# the original "one level" cut-off forced every two-deep local helper chain to
# `global`; bounded expansion with cycle detection and a depth cap is strictly
# stronger analysis with the same fail-closed posture — a cycle, a duplicate
# name, an unknown call or an over-deep chain still fails closed).
DEPTH_CAP = 8
_memo = {}   # (scope_path or '<shared>', fn name) -> (ok, frozenset(lanes))

def judge_body(text, scope_path, stack, test_fn, file_path=None):
    """Prove a body's call surface pollution-free. Returns (ok, lanes).
    `scope_path` is the file whose local fns are visible (None inside shared
    helpers — they cannot see test files); `file_path` is the file the body
    physically lives in (alias/type scope — differs from scope_path for
    shared helper bodies). `stack` carries the names being expanded for
    cycle/depth failure."""
    if file_path is None:
        file_path = scope_path
    text = resolve_renames(text, file_path)
    lanes = set(lanes_of(text))
    # Codex TA-01 R18 P0: strings and comments are already blanked, so any
    # surviving non-ASCII character is part of an IDENTIFIER — invisible to
    # every ASCII regex in this classifier (renames, call extraction, spawn
    # names). A body the tokenizer cannot see cannot be proven: fail closed.
    if NON_ASCII.search(text):
        _explain(test_fn, 'non-ascii-identifier')
        return False, lanes
    # Codex TA-01 R26 P0: a file with an UNRESOLVED include! carries spliced
    # source the indexes never saw — nothing in it can be proven.
    if file_path in UNRESOLVED_INCLUDES:
        _explain(test_fn, 'unresolved-include')
        return False, lanes
    # Codex TA-01 R28 P0: a Drop-emitting macro invocation whose target
    # idents could not be recovered may have attached a polluting destructor
    # to ANY type of this crate (the orphan rule confines it to the crate).
    if file_path and any(p in DROP_MACRO_UNKNOWN
                         for p in mod_closure(file_path)):
        _explain(test_fn, 'drop-macro-unknown')
        return False, lanes
    # Codex TA-01 R19 P0 backstop: any `>::` surviving UFCS lowering is an
    # associated call the tokenizer could not resolve — fail closed.
    if re.search(r'>\s*::', norm_calls_text(text)):
        _explain(test_fn, 'ufcs-unparsed')
        return False, lanes
    # Dependency lanes (P0, 2026-08-26 pre-review audit): a spawn WITHOUT
    # env_clear inherits the parent environment mid-mutation — same-binary
    # lane:env tests (EnvVarGuard, LIBRA_CONFIG_GLOBAL_DB, HOME swaps) race
    # such children, so the READER needs the env lane too, exactly like
    # Head::current_commit needs cwd. `base_libra_command` (tests/command/
    # mod.rs) calls env_clear() and pins the env, so the audited primitives
    # stay lane-free; ad-hoc local spawn helpers do not. Codex TA-01 R1 P0:
    # `use ...::Command as X` / `type X = ...Command;` aliases count too.
    spawn_names = {'Command'} | (cmd_aliases.get(file_path, set()) if file_path else set())
    tnorm = norm_calls_text(text)
    if any((sn + '::new') in tnorm for sn in sorted(spawn_names)):
        # Codex TA-01 R2/R4 P0: the discharge proof must be ORDER- and
        # RECEIVER-aware — a `.env_clear()` after the terminal spawn, inside a
        # deeper brace level (dead/conditional code), or on another receiver
        # proves nothing. `spawn_env_ok` walks each spawn site; any site it
        # cannot discharge puts the body on the env lane. A local/shared
        # `fn env_clear` definition is a decoy that voids every proof.
        decoy = (scope_path and 'env_clear' in fn_index.get(scope_path, {})) or \
                ('env_clear' in shared_fn_names)
        if decoy or not spawn_env_ok(tnorm, sorted(spawn_names)):
            lanes.add('env')
    if len(stack) > DEPTH_CAP:
        _explain(test_fn, 'depth-cap:' + '>'.join(sorted(str(k) for k in stack)))
        return False, lanes
    macros, names, methods, quals, quals_full = calls_of(text)
    own = fn_index.get(scope_path, {}) if scope_path else {}
    vis = visible_fns(scope_path) if scope_path else {}
    local_macros = macro_index.get(file_path, {}) if file_path else {}
    poisoned_here = poison_index.get(file_path, set()) if file_path else set()

    def expand(body2, scope2, fpath2, key):
        """Bounded expansion of one resolved definition. Stack keys are
        RESOLVED IDENTITIES, not bare names — a `.spawn(` method colliding
        with harness `fn spawn` is not a cycle (Codex R6 follow-up)."""
        if key in _memo:
            ok2, sub2 = _memo[key]
        else:
            ok2, sub2 = judge_body(body2, scope2, stack | {key}, test_fn,
                                   file_path=fpath2)
            if ok2:
                _memo[key] = (ok2, frozenset(sub2))
        if ok2 and fs_lane_of(key[0], key[1]):
            sub2 = set(sub2) | {'cwd'}
        return ok2, sub2

    for mac in sorted(macros):
        mkey = 'macro!' + mac
        if mkey in stack:
            _explain(test_fn, 'macro-cycle:' + mac)
            return False, lanes
        mbody = None
        mpath = file_path
        if mac in local_macros:
            mbody = local_macros[mac]
        elif mac in shared_macros:
            entries = shared_macros[mac]
            mbody, mpath = entries[0] if len(entries) == 1 else (AMBIGUOUS, file_path)
        elif mac in all_macros:
            # Codex TA-01 R3 P0: `#[macro_use] mod` makes a macro from another
            # scanned file visible — over-approximate visibility to the whole
            # tree, expanding a unique definition and failing closed on
            # duplicates.
            entries = all_macros[mac]
            mbody, mpath = entries[0] if len(entries) == 1 else (AMBIGUOUS, file_path)
        if mbody is not None:
            # Codex TA-01 R2 P0: a macro_rules! defined in scanned scope shadows
            # any allowlist entry of the same name — judge its body instead.
            if mbody is AMBIGUOUS:
                _explain(test_fn, 'macro-ambiguous:' + mac)
                return False, lanes
            # Codex TA-01 R17 P0: a macro body using METAVARIABLES expands to
            # caller-supplied tokens (`$c::new(...)` becomes a real spawn) —
            # the raw body cannot be judged, fail closed.
            if re.search(r'\$\s*[A-Za-z_]', mbody):
                _explain(test_fn, 'macro-metavar:' + mac)
                return False, lanes
            ok, sub = judge_body(mbody, scope_path, stack | {mkey}, test_fn,
                                 file_path=mpath)
            if not ok:
                _explain(test_fn, 'macro-body:' + mac)
                return False, lanes
            lanes |= sub
            continue
        if mac not in MACRO_ALLOW:
            _explain(test_fn, 'macro!:' + mac)
            return False, lanes

    ext_assoc = {}
    must_local = set()
    mod_qualified = set()
    mod_resolved = []
    walked = set()          # (qual2, last) handled by the full-path walk
    for segs, last in sorted(quals_full):
        if not (file_path and segs[0] in mod_index.get(file_path, ())):
            continue        # not a locally-declared module path: existing rules
        cur = file_path
        i = 0
        ok_walk = True
        while i < len(segs) and segs[i] in mod_index.get(cur, ()):
            nxt = resolve_mod_file(cur, segs[i])
            if nxt is None:
                _explain(test_fn, 'mod-unresolved:' + '::'.join(segs))
                return False, lanes
            cur = nxt
            i += 1
        if i == len(segs):
            ent = fn_index.get(cur, {}).get(last)
        elif i == len(segs) - 1 and segs[i] in type_index.get(cur, ()):
            ent = fn_index.get(cur, {}).get(last)
        else:
            _explain(test_fn, 'mod-walk:' + '::'.join(segs) + '::' + last)
            return False, lanes
        if ent is None or ent is AMBIGUOUS:
            _explain(test_fn, 'mod-assoc:' + '::'.join(segs) + '::' + last)
            return False, lanes
        key = (cur, last)
        if key in stack:
            _explain(test_fn, 'cycle:' + last)
            return False, lanes
        ok, sub = expand(ent, cur, cur, key)
        if not ok:
            _explain(test_fn, 'helper-mod:' + '::'.join(segs) + '::' + last)
            return False, lanes
        lanes |= sub
        walked.add((segs[-1] + '::' + last, last))
    local_types = type_index.get(file_path, set()) if file_path else set()
    local_aliases = alias_index.get(file_path, {}) if file_path else {}
    for qual, tprefix, last in sorted(quals):
        if (qual, last) in walked:
            continue        # already resolved by the full-path module walk
        # Codex TA-01 R2 P0: `Self::fn` refers to the enclosing impl type —
        # it must resolve in this file's fn index, never through the external
        # constructor convention or the allowlist.
        if tprefix == 'Self':
            must_local.add(last)
            continue
        # Codex TA-01 R1 P0: resolve local `type Alias = Target;` before any
        # external-constructor convention — `Alias::new()` on a local polluting
        # type must expand, and an alias of Command must hit the spawn rule.
        seen_alias = set()
        while tprefix in local_aliases and tprefix not in seen_alias:
            seen_alias.add(tprefix)
            tprefix = local_aliases[tprefix]
        qual = tprefix + '::' + last
        if file_path and tprefix in unparsed_types_index.get(file_path, ()) \
                and tprefix not in local_types:
            _explain(test_fn, 'unparsed-alias:' + tprefix)
            return False, lanes
        if tprefix in (cmd_aliases.get(file_path, set()) if file_path else set()):
            qual = 'Command::' + last
            tprefix = 'Command'
        if qual in KNOWN_LANE_QUALIFIED:
            lanes.add(KNOWN_LANE_QUALIFIED[qual])
            continue
        # `Type::fn` where Type is NOT declared in this file: the same-file fn
        # index must not resolve `fn` (an unrelated local `fn new` would be a
        # misattribution — found once in the 2026-08-26 pre-review audit);
        # such names go through shared-type/allowlist/constructor rules only.
        if tprefix[0].isupper() and tprefix not in local_types:
            ext_assoc.setdefault(last, set()).add(tprefix)
        elif file_path and tprefix in mod_index.get(file_path, ()):
            # Codex TA-01 R8 P0: `hidden::write()` through a locally declared
            # module (incl. #[path]-redirected) must resolve INSIDE that
            # module's file — never collapse to a bare allowlisted name.
            mod_resolved.append((tprefix, last))
        else:
            names.add(last)              # fall through to the name-based rules
            mod_qualified.add(last)      # a std-ish `mod::fn` path — the module
                                         # is not declared here
    names |= must_local
    for k in must_local:
        ext_assoc.pop(k, None)
    names -= set(ext_assoc)

    for mprefix, last in sorted(set(mod_resolved)):
        tf = resolve_mod_file(file_path, mprefix)
        if tf is None:
            _explain(test_fn, 'mod-unresolved:' + mprefix + '::' + last)
            return False, lanes
        ent = fn_index.get(tf, {}).get(last)
        if ent is None or ent is AMBIGUOUS:
            _explain(test_fn, 'mod-assoc:' + mprefix + '::' + last)
            return False, lanes
        key = (tf, last)
        if key in stack:
            _explain(test_fn, 'cycle:' + last)
            return False, lanes
        ok, sub = expand(ent, tf, tf, key)
        if not ok:
            _explain(test_fn, 'helper-mod:' + mprefix + '::' + last)
            return False, lanes
        lanes |= sub

    read_tokens = (names | methods) & ENV_READ_NAMES
    if read_tokens or 'env::var' in tnorm or 'env::vars' in tnorm \
            or 'env::temp_dir' in tnorm:
        rk, rnl, rvi = env_read_index.get(file_path, (set(), True, True)) \
            if file_path else (set(), True, True)
        if rvi or rnl or 'vars' in read_tokens or 'temp_dir' in read_tokens \
                or any(k not in BENIGN_READ_KEYS for k in rk) \
                or not (rk or rnl or rvi):
            # the last clause is the structural fail-closed (Codex R14 P0):
            # the judge DETECTED an env read but the raw key scan produced no
            # evidence at all — the spelling escaped the scan (e.g. turbofish
            # before this fix), so the read cannot be proven benign.
            lanes.add('env')
        names -= ENV_READ_NAMES
        methods = methods - ENV_READ_NAMES

    deferred = sorted(n for n in (names | methods) if n in LANE_SANCTIONED)
    names -= set(deferred)
    methods = methods - set(deferred)

    def common_gate(nm):
        """Checks shared by every position; returns True when handled."""
        if nm in poisoned_here:
            _explain(test_fn, 'poisoned-const:' + nm)
            return 'fail'
        if nm in BLACK_NAMES:
            return 'ok'                  # the substring scan recorded the lane
        return None

    # ---- fn REFERENCES (Codex TA-01 R24 P0) ----------------------------
    # `thread::spawn(pollute_env)`, `Builder::new().spawn(f)`, `map(f)` run
    # their CALLABLE ARGUMENT in-process — an allowlisted spawn is a child-
    # process terminal only when no callable rides along. Any bare identifier
    # OUTSIDE call position that names a resolvable scanned fn is treated as
    # a fn pointer and its body judged exactly like a call (aliases were
    # substituted by resolve_renames, so a renamed set_var reference already
    # surfaced to the blacklist substring scan). Identifiers resolving to
    # nothing are data bindings and stay inert; AMBIGUOUS fails closed.
    tref = FN_DEF_NAME.sub(' ', ATTR_IN_BODY.sub(' ', norm_calls_text(text)))
    # ---- Drop-impl lanes (Codex TA-01 R27 P0) --------------------------
    # Any mention of a scoped type carrying a Drop impl runs that destructor
    # at scope exit: judge the drop body and merge its lanes; ambiguous or
    # unlocatable drop bodies fail closed.
    if file_path:
        visible_drops = {}
        for pdp in mod_closure(file_path):
            for T3, db3 in drop_impl_index.get(pdp, {}).items():
                visible_drops[T3] = (AMBIGUOUS if T3 in visible_drops
                                     else (db3, pdp))
        for pdp, dmap3 in drop_impl_index.items():
            if pdp.startswith(SHARED_PREFIXES) and pdp != file_path:
                for T3, db3 in dmap3.items():
                    visible_drops[T3] = (AMBIGUOUS if T3 in visible_drops
                                         and visible_drops[T3][1] != pdp
                                         else (db3, pdp))
        for T3 in sorted(visible_drops):
            if not _rx(r'\b' + re.escape(T3) + r'\b').search(tref):
                continue
            ent3 = visible_drops[T3]
            if ent3 is AMBIGUOUS or ent3[0] is AMBIGUOUS:
                _explain(test_fn, 'drop-ambiguous:' + T3)
                return False, lanes
            db3, pdp3 = ent3
            key3 = (pdp3, 'drop@' + T3)
            if key3 in stack:
                continue                 # destructor already being merged
            ok3, sub3 = expand(db3, pdp3, pdp3, key3)
            if not ok3:
                _explain(test_fn, 'drop-body:' + T3)
                return False, lanes
            lanes |= sub3

    vfree = visible_free_fns(scope_path) if scope_path else {}
    poisoned_callable_vis = set()
    if file_path:
        for pcv in mod_closure(file_path):
            poisoned_callable_vis |= poison_callable_index.get(pcv, set())
    for nm in sorted(set(REF_IDENT.findall(tref)) - KEYWORDS):
        if nm in poisoned_callable_vis:
            _explain(test_fn, 'poisoned-callable-ref:' + nm)
            return False, lanes
        ent = vfree.get(nm)
        if ent is None:
            shared_free = [e for e in shared_bodies.get(nm, ())
                           if nm in free_fn_names.get(e[1], ())]
            if not shared_free:
                continue                 # data identifier, method, assoc fn
            ent = (shared_free[0]
                   if len(shared_free) == 1
                   and shared_free[0][0] is not AMBIGUOUS
                   else AMBIGUOUS)
        if ent is AMBIGUOUS:
            _explain(test_fn, 'ambiguous-fn-ref:' + nm)
            return False, lanes
        body2, defp = ent
        key = (defp, nm)
        if key in stack:
            _explain(test_fn, 'cycle:' + nm)
            return False, lanes
        ok, sub = expand(body2, defp, defp, key)
        if not ok:
            _explain(test_fn, 'fn-ref:' + nm)
            return False, lanes
        lanes |= sub

    # path-QUALIFIED value references: `S::F` reaches assoc fns and assoc
    # callable consts, `m::F` reaches another file's fns — resolve through
    # the local type/module graph; external heads are inert values (their
    # only polluting members, set_var/remove_var, are caught by the
    # blacklist substring scan on the path text itself).
    vtypes_ref = visible_types(file_path) if file_path else {}
    for pref, last in sorted(set(QUAL_REF.findall(tref))):
        segs = [x for x in pref.rstrip(':').split('::')
                if x and x not in ('self', 'crate')]
        if not segs:
            continue
        head = segs[0]
        if head == 'Self':
            defp = scope_path
        elif head in vtypes_ref:
            defp = vtypes_ref[head]
        elif file_path and head in mod_index.get(file_path, ()):
            defp = resolve_mod_file(file_path, head)
        else:
            continue                     # external path: inert value
        if defp is None or defp is AMBIGUOUS:
            _explain(test_fn, 'qual-ref-ambiguous:' + pref + last)
            return False, lanes
        dead = False
        for seg in segs[1:]:
            if seg in type_index.get(defp, ()):
                continue                 # type qualifier inside the same file
            if seg in mod_index.get(defp, ()):
                nxt = resolve_mod_file(defp, seg)
                if nxt is None:
                    dead = True
                    break
                defp = nxt
                continue
            dead = True
            break
        if dead:
            _explain(test_fn, 'qual-ref-unresolved:' + pref + last)
            return False, lanes
        ent = fn_index.get(defp, {}).get(last)
        if ent is not None:
            if ent is AMBIGUOUS:
                _explain(test_fn, 'ambiguous-fn-ref:' + pref + last)
                return False, lanes
            key = (defp, last)
            if key in stack:
                _explain(test_fn, 'cycle:' + last)
                return False, lanes
            ok, sub = expand(ent, defp, defp, key)
            if not ok:
                _explain(test_fn, 'fn-ref:' + pref + last)
                return False, lanes
            lanes |= sub
            continue
        if last in poison_callable_index.get(defp, set()):
            _explain(test_fn, 'poisoned-callable-ref:' + pref + last)
            return False, lanes
        if last in rename_index.get(defp, {}):
            # an alias of a fn referenced as a value from OUTSIDE its file —
            # the substitution layer only rewrites that file's own bodies
            _explain(test_fn, 'qual-alias-ref:' + pref + last)
            return False, lanes
        # enum variant, unit struct, or data const: inert value

    # ---- free/path-position names --------------------------------------
    for nm in sorted(names):
        g = common_gate(nm)
        if g == 'fail':
            return False, lanes
        if g == 'ok':
            continue
        if nm in must_local and nm not in own:
            _explain(test_fn, 'self-assoc:' + nm)
            return False, lanes          # Self::fn with no local definition: fail closed
        if nm in vis:
            ent = vis[nm]
            if ent is AMBIGUOUS:
                _explain(test_fn, 'ambiguous-local:' + nm)
                return False, lanes
            body2, defp = ent
            key = (defp, nm)
            if key in stack:
                _explain(test_fn, 'cycle:' + nm)
                return False, lanes
            ok, sub = expand(body2, defp, defp, key)
            if not ok:
                _explain(test_fn, 'helper-local:' + nm)
                return False, lanes
            lanes |= sub
            continue
        if nm in shared_bodies:
            # scanned-scope definitions SHADOW the allowlist (Codex R6 P0): a
            # shared helper named `write` wrapping a renamed set_var must be
            # judged by its body, never blessed by name.
            bodies = shared_bodies[nm]
            if len(bodies) != 1 or bodies[0][0] is AMBIGUOUS:
                _explain(test_fn, 'ambiguous-shared:' + nm)
                return False, lanes
            sb, sp = bodies[0]
            key = (sp, nm)
            if key in stack:
                _explain(test_fn, 'cycle:' + nm)
                return False, lanes
            ok, sub = expand(sb, None, sp, key)
            if not ok:
                _explain(test_fn, 'helper-shared:' + nm)
                return False, lanes
            lanes |= sub
            continue
        if nm in CALL_ALLOW:
            continue
        if nm in KNOWN_LANE_HELPERS:
            lanes.add(KNOWN_LANE_HELPERS[nm])
            continue
        if nm[0].isupper():
            continue                     # constructor/variant convention
        _explain(test_fn, 'unknown:' + nm)
        return False, lanes              # unknown call: fail closed

    # ---- external-type associated calls --------------------------------
    for nm in sorted(ext_assoc):
        g = common_gate(nm)
        if g == 'fail':
            return False, lanes
        if g == 'ok':
            continue
        vtypes = visible_types(file_path) if file_path else {}
        resolved_any = False
        bad = False
        for tp in sorted(ext_assoc[nm]):
            # resolution chain: module-graph visible type -> shared type ->
            # tree-wide UNIQUE type (Codex R10 P0); ambiguity fails closed,
            # an unresolvable type keeps the constructor convention below.
            sp = None
            if tp in vtypes:
                sp = vtypes[tp]
            elif tp in shared_types:
                sp = shared_types[tp]
            elif tp in all_types:
                entries = all_types[tp]
                sp = entries[0] if len(set(entries)) == 1 else AMBIGUOUS
            if sp is None:
                continue
            resolved_any = True
            if sp is AMBIGUOUS:
                _explain(test_fn, 'ambiguous-type:' + tp)
                bad = True
                break
            body2 = fn_index.get(sp, {}).get(nm)
            if body2 is None or body2 is AMBIGUOUS:
                _explain(test_fn, 'type-assoc:' + tp + '::' + nm)
                bad = True
                break
            key = (sp, nm)
            if key in stack:
                _explain(test_fn, 'cycle:' + nm)
                bad = True
                break
            ok, sub = expand(body2, sp, sp, key)
            if not ok:
                _explain(test_fn, 'helper-type:' + tp + '::' + nm)
                bad = True
                break
            lanes |= sub
        if bad:
            return False, lanes
        if resolved_any:
            continue
        if nm in CALL_ALLOW:
            continue
        if nm in KNOWN_LANE_HELPERS:
            lanes.add(KNOWN_LANE_HELPERS[nm])
            continue
        if nm[0].isupper():
            continue                     # constructor/variant convention
        _explain(test_fn, 'unknown:' + nm)
        return False, lanes

    # ---- method-position names -----------------------------------------
    # A method call could be a std/external method OR one of OUR impl methods
    # of the same name. Judge EVERY scanned candidate (their pollution
    # propagates; any failing candidate fails the caller); a candidate whose
    # identity is already on the stack is the body being judged — that
    # occurrence takes the std/allowlist interpretation (its own scan already
    # counts its pollution). After the candidates pass, an allowlisted name or
    # a fully-judged candidate set both allow the call (Codex R6 P0 + the
    # cycle-collision follow-up).
    for nm in sorted(methods):
        g = common_gate(nm)
        if g == 'fail':
            return False, lanes
        if g == 'ok':
            continue
        if nm in vis:
            # Codex TA-01 R1 P0: a same-file (or mod-declared) `fn <name>`
            # shadows the method-only allowlist — a polluting local method
            # named `execute` is expanded, never laundered. A candidate whose
            # identity is already on the stack is the body being judged —
            # that occurrence takes the std/allowlist interpretation.
            ent = vis[nm]
            if ent is AMBIGUOUS:
                _explain(test_fn, 'ambiguous-local:' + nm)
                return False, lanes
            body2, defp = ent
            key = (defp, nm)
            if key in stack:
                if nm in METHOD_ONLY_ALLOW or nm in CALL_ALLOW:
                    continue
                _explain(test_fn, 'cycle:' + nm)
                return False, lanes
            ok, sub = expand(body2, defp, defp, key)
            if not ok:
                _explain(test_fn, 'helper-local:' + nm)
                return False, lanes
            lanes |= sub
            continue
        judged_all = False
        if nm in shared_bodies:
            judged_all = True
            for sb, sp in shared_bodies[nm]:
                if sb is AMBIGUOUS:
                    _explain(test_fn, 'ambiguous-shared:' + nm)
                    return False, lanes
                key = (sp, nm)
                if key in stack:
                    judged_all = False   # self-occurrence: std interpretation
                    continue
                ok, sub = expand(sb, None, sp, key)
                if not ok:
                    _explain(test_fn, 'helper-shared:' + nm)
                    return False, lanes
                lanes |= sub
        if nm in METHOD_ONLY_ALLOW or nm in CALL_ALLOW:
            continue
        if judged_all and nm in shared_bodies:
            continue                     # every scanned candidate proved clean
        if nm in KNOWN_LANE_HELPERS:
            lanes.add(KNOWN_LANE_HELPERS[nm])
            continue
        _explain(test_fn, 'unknown:' + nm)
        return False, lanes

    for nm in deferred:
        # checked LAST so lanes contributed by helper expansion are visible:
        # the call passes only as the recorded pollution itself
        if LANE_SANCTIONED[nm] not in lanes:
            _explain(test_fn, 'unknown:' + nm)
            return False, lanes
    return True, lanes

# ---------- pass 2: scan serial attributes and classify ---------------------
# TA-02: site keys are CONTENT anchors, never line numbers. An attribute
# inside a `macro_rules!` body keys as
#   <site:<path>:macro:<macro_name>#<ordinal>>
# (ordinal = 1-based position among that macro body's serial attributes);
# an unattributable attribute outside any macro body keys as
#   <site:<path>:orphan#<ordinal>> (per-file ordinal). Line drift above the
# attribute can no longer invalidate the key (plan-20260824 DF-05 broke the
# old line anchor by inserting cases above the site).
_macro_spans = {}  # path -> [(name, start (line,col), end (line,col))]
for _pS, _cS in FILES:
    _spansS = []
    for _iS, _lS in enumerate(_cS):
        for _mS in MACRO_DEF.finditer(_lS):
            _clS, _txS = delimit(_cS, _iS, _mS.start())
            _endS = (len(_cS) - 1, 1 << 30)
            if _clS:
                # delimit is line-granular: locate the BALANCING close
                # character to get a true (line, col) body end
                _dS, _seenS, _offS = 0, False, None
                for _qS, _chS in enumerate(_txS):
                    if _chS == '{':
                        _dS += 1
                        _seenS = True
                    elif _chS == '}':
                        _dS -= 1
                        if _seenS and _dS == 0:
                            _offS = _qS
                            break
                if _offS is not None:
                    _preS = _txS[:_offS + 1]
                    _nlS = _preS.count('\n')
                    _lstS = _preS.split('\n')[-1]
                    _endS = (_iS + _nlS,
                             (_mS.start() + len(_lstS)) if _nlS == 0
                             else len(_lstS))
            _spansS.append((_mS.group(1), (_iS, _mS.start()), _endS))
    _macro_spans[_pS] = _spansS

_site_counters = {}


def _site_key(path, line_idx, col):
    # COLUMN-AWARE containment (Codex TA-02 R1 P1): an attribute on the same
    # line as a one-line macro body but AFTER its closing brace is outside.
    pos = (line_idx, col)
    inner = None
    for _nmS, _stS, _enS in _macro_spans.get(path, ()):
        if _stS < pos < _enS and (inner is None or _stS > inner[1]):
            inner = (_nmS, _stS)
    if inner is not None:
        ck = (path, 'macro', inner[0], inner[1])
        _site_counters[ck] = _site_counters.get(ck, 0) + 1
        return '<site:%s:macro:%s#%d>' % (path, inner[0],
                                          _site_counters[ck])
    ck = (path, 'orphan')
    _site_counters[ck] = _site_counters.get(ck, 0) + 1
    return '<site:%s:orphan#%d>' % (path, _site_counters[ck])


rows = []
# TA-03: the converter consumes the FROZEN manifest for verdicts and this
# machine-readable SITE MAP for attribute locations — one lexer, one
# attribution contract. stdout (the manifest surface) is unchanged.
_sites_out = os.environ.get('SERIAL_CLASSIFY_SITES_FILE')
_sites_rows = []
for path, code in FILES:
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
                _k = _site_key(path, i, m.start())
                rows.append((_k, 'global'))
                if _sites_out:
                    _sites_rows.append((_k, path, i, m.start(), i,
                                        len(code[i])))
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
                _k = _site_key(path, i, m.start())
                rows.append((_k, 'global'))
                if _sites_out:
                    _sites_rows.append((_k, path, i, m.start(),
                                        end_li, end_col))
            else:
                fn = fm.group(1)
                closed, text = delimit(code, i if same_line else j,
                                       fm.start() if same_line else 0)
                text = resolve_renames(text, path)
                # one lane per matched process-wide resource — serial_test
                # supports multiple keys, so mixed cases keep every exclusion;
                # env pollution is the composable `env` lane, not a short-circuit
                parts = lanes_of(text)
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
                    uncovered = [p for p in parts if p in LANE_ORDER and p not in keys]
                    if keys and uncovered:
                        print("FAIL: %s (%s): named key(s) %s do not cover process-wide pollution lane(s) %s — make the attribute unkeyed or remove the pollution" % (
                            fn, path, '+'.join(keys), '+'.join(uncovered)), file=sys.stderr)
                        sys.exit(3)
                    if parts:
                        verdict = 'lane:' + '+'.join(parts)
                    else:
                        # none candidate: the whole call surface must be proven
                        # pollution-free (TA-01), and one-level expansion may
                        # surface lanes the body itself does not show.
                        ok, extra = judge_body(text, path, frozenset(((path, fn),)), fn, file_path=path)
                        if ok and fs_lane_of(path, fn):
                            extra = set(extra) | {'cwd'}
                        if not ok:
                            verdict = 'global'
                        elif extra:
                            ordered = [l for l in LANE_ORDER if l in extra]
                            verdict = 'lane:' + '+'.join(ordered)
                        else:
                            verdict = 'none'
                rows.append((fn, verdict))
                if _sites_out:
                    _sites_rows.append((fn, path, i, m.start(),
                                        end_li, end_col))
            if end_li == i:
                pos = end_col if end_col > m.end() else m.end()
            else:
                break

rows.sort()
for fn, v in rows:
    print('%s\t%s' % (fn, v))
if _sites_out:
    with open(_sites_out, 'w', encoding='utf-8') as _sf:
        for _k, _p, _sl, _sc, _el, _ec in sorted(_sites_rows):
            _sf.write('%s\t%s\t%d\t%d\t%d\t%d\n'
                      % (_k, _p, _sl, _sc, _el, _ec))
CLASSIFY_PY
