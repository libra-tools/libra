# `libra status`

Show the working tree status.

**Alias:** `st`

## Synopsis

```
libra status [OPTIONS] [pathspec]...
```

## Description

`libra status` shows the state of the working tree and staging area: which files are staged
for the next commit, which have modifications not yet staged, and which are untracked. It also
reports the current branch, detached HEAD state, and upstream tracking information.

The command computes the diff between HEAD, the index, and the working tree to classify files
into staged, unstaged, and untracked categories. It supports multiple output formats: a
human-readable long format (default, also selectable explicitly with `--long`), a short format (`--short`), a machine-readable porcelain
format, structured JSON for agent consumption, and `-z` NUL-terminated machine output. It can
also detect renames (`--find-renames`), align output into columns (`--column`), and control
whether upstream ahead/behind counts are shown (`--ahead-behind` / `--no-ahead-behind`).
Optional pathspecs limit the reported staged, unstaged, unmerged, ignored, and
untracked paths. They use the shared pathspec engine, including `:(top)`,
`:(exclude)`, `:(icase)`, `:(literal)`, and `:(glob)` magic.
An in-progress merge is still reported as a global repository state even when
the selected pathspec hides every conflicted path; `--exit-code` remains dirty
until the merge is continued or aborted.

During merge, rebase, and cherry-pick conflicts, unmerged index entries are reported as conflicts
instead of untracked files. Porcelain v1/short output uses Git-style XY codes such as `UU
conflict.txt`; porcelain v2 emits `u <XY> ...` records with stage modes and object IDs. The
default long format lists conflicts under an `Unmerged paths:` heading with human-readable
labels (`both modified:`, `deleted by them:`, `both added:`, …) plus resolve/abort hints.

Tracked symlinks participate in the same HEAD/index/worktree comparison as
regular files. `status` treats the symlink itself as the worktree object,
compares the stored link target bytes, and reports target changes as
modifications instead of following the link or treating dangling symlinks as
deleted.

### Display config defaults (`status.*`)

When the corresponding CLI flag is absent, Libra honors these Git-compatible
defaults, each read through the local → global → system cascade
(case-insensitive keys; encrypted local/global values decrypted; legacy rows
honored; an unreadable or unsupported system scope skipped):

- `status.showUntrackedFiles=no|normal|all` selects the untracked-file mode for
  every output format (`-u`/`--untracked-files` overrides it).
- `status.short=true|false` selects the short format by default; an explicit
  `--long` or `--porcelain` still wins.
- `status.branch=true|false` adds the branch header to the **short format
  only** (matching Git); porcelain headers still require an explicit
  `-b`/`--branch`, keeping porcelain output config-immune. `--no-branch`
  overrides a configured `true`.
- `status.showStash=true|false` shows the stash-count hint in the long format;
  `--no-show-stash` overrides a configured `true`.
- `status.relativePaths=true|false` (config-only, like Git): `true` — the
  default — renders human long/short paths relative to the current directory;
  `false` keeps repository-root-relative paths.
- `core.quotePath=true|false` (strict boolean, default `true`, matching Git):
  human-short and non-`-z` porcelain v1/v2 paths always C-style-escape
  control characters, `"` and `\` (wrapping the path in double quotes);
  under the default, bytes above `0x7F` are additionally escaped as `\ooo`
  octal. With `false`, bytes above `0x7F` are written RAW on the non-`-z`
  byte-oriented surfaces (short, human, porcelain v1/v2) — including names
  that are not valid UTF-8 (Git parity) — while control characters, `"`
  and `\` remain escaped. `-z` records always carry raw unquoted path bytes.

All six keys are validated up front: an invalid value fails closed with
`LBR-CLI-002` and an unreadable local/global scope with `LBR-IO-001`, before
any status output is produced. Exception: a global config store whose schema
is newer than this Libra binary is skipped with a one-time deduplicated
warning instead of failing; only commands that genuinely need global storage
config (`pull`/`push`/`fetch`/`clone`/`cloud`) fail closed with
`LBR-CONFIG-001`. Boolean values use the full Git grammar
(`true`/`yes`/`on`, `false`/`no`/`off`, and integers — non-zero is true — with
optional `k`/`m`/`g` suffixes); an empty value is rejected.

## Options

### `<pathspec>...`

Limit status output to matching paths. Pathspecs resolve from the current
working directory unless `:(top)` is used, and support exact files, directory
prefixes, default wildcards, and `:(top)` / `:(exclude)` / `:(icase)` /
`:(literal)` / `:(glob)` magic.

### `-s, --short`

Give the output in the short format. Each file is shown on a single line with a two-character
status code (e.g., `M ` for staged modified, ` M` for unstaged modified, `??` for untracked).
Conflicts with `--porcelain`. `status.short=true` selects this format by default.

```bash
libra status -s
libra status --short
```

### `--long`

Give the output in the long format — Libra's default — overriding
`status.short=true`. Conflicts with `--short`/`--porcelain`.

```bash
libra status --long
```

### `--porcelain [VERSION]`

Output in a machine-readable format. Accepts an optional version argument: `v1` (default) or
`v2` for extended format. Conflicts with `--short`.

```bash
libra status --porcelain
libra status --porcelain v1
libra status --porcelain v2
```

### `--branch` (`-b`) / `--no-branch`

Include branch information in short or porcelain output. Shows the current branch and its
tracking relationship on the first line. `-b` is the short alias, so `libra status -sb`
matches `git status -sb`. `status.branch=true` enables the header for the short format only
(porcelain requires the explicit flag, matching Git); `--no-branch` overrides the config
(and an earlier `--branch`; the last one wins).

```bash
libra status --short --branch
libra status -sb
libra status --porcelain --branch
libra status --no-branch          # suppress a configured status.branch=true
```

### `--ahead-behind` / `--no-ahead-behind`

Control whether ahead/behind counts are shown in the branch tracking line. `--no-ahead-behind`
suppresses the counts while still showing the upstream branch name. The default is to show the
counts when an upstream is configured.

```bash
libra status --short --branch --no-ahead-behind
libra status --porcelain --branch --no-ahead-behind
```

### `-z`

Terminate each machine-readable status entry with a NUL (`\0`) byte instead of a newline
(`--null` is the Git-parity long alias). With `--porcelain` or `--short` it NUL-terminates
that format; a bare `-z`/`--null` with no explicit format forces porcelain v1 (machine
intent, matching Git); a config-selected `status.short=true` counts as a format, so short
stays short + NUL. Combining it with `--long` or the cache modes
(`--scan`/`--cached`/`--check-dirty`) fails closed.

```bash
libra status --porcelain -z
libra status -s -z
```

### `--column`

Align human-readable status entries into columns. In staged/unstaged sections, status labels
(`modified:`, `deleted:`, `new file:`, `renamed:`) are padded to the same width. In untracked
and ignored sections, file names are laid out in multiple columns.

```bash
libra status --column
```

### `--no-column`

Do not align status entries into columns (equivalent to `--column=never`),
countermanding an earlier `--column` (last one on the command line wins). Status
is not columnar by default, so on its own this is a no-op.

```bash
libra status --no-column
```

### `--find-renames [PERCENT]`

Set the rename-detection similarity threshold. Rename detection is **on by default** at 50%
(matching Git), so `--find-renames` is only needed to change the threshold or to re-enable
detection after `status.renames=false`. When a deleted file and a new file are similar enough,
they are reported as one rename pair (`renamed: old -> new`) instead of separate delete/add
entries. The CLI accepts Git's full raw score grammar — a bare integer is read as
`0.<digits>` (so `505` is 50.5%), `N%` is a literal percent (`100%` = exact-only), and
decimals work (`0.8`); `0`/bare re-enable the 50% default. The three spellings
`--no-renames` / `--renames` / `--find-renames[=N]` obey true last-one-wins in argv order
(so `--no-renames --find-renames=80` re-enables at 80%). `-z` also has the Git-parity
`--null` long alias; a bare `-z`/`--null` with no explicit format forces porcelain v1, and
combining it with `--long` or the cache modes fails closed. The embedding API's
`find_renames: Option<u8>` keeps the simpler 0–100 percent range (documented narrowing —
the CLI grammar is the complete surface).

Renames are matched by the shared diffcore engine: exact matches are found by blob id, then a
unique-basename pass, then a bounded inexact spanhash pass with a per-side rename limit and a
similarity-comparison budget. The limit comes from `status.renameLimit` (falling back to
`diff.renameLimit`) through the strict local → global → system cascade: a non-negative integer,
`0` disables the cap, default 1000 (Git parity); invalid values fail closed before any output,
and exceeding the cap skips only the exhaustive stage with a structured
`rename_limit_product_skipped` warning. Staged renames pair the HEAD tree with the index; unstaged
renames pair the index with the worktree — but only when the `status.renameUntracked` config
(a Libra extension, strict boolean, default `false`) is enabled, because every unstaged "new"
path is an untracked file. With the default, a tracked→untracked move renders as `D` + `??`,
matching Git, and no unstaged rename record is produced. When the extension is enabled,
destination candidates come from an independent bounded worktree probe (R0-3): `-uno` and
collapsed untracked directories hide DISPLAY markers but never the probe, candidates are
qualified by the same tracked/ignore layering as the display scan (tracked paths, case-fold
aliases, unmerged stages, and ignored paths never qualify), and a call-global dual budget
(50k enumerated entries / 10k qualified destinations) bounds the walk — tripping it keeps
partial pairing and surfaces a structured `probe_truncated` warning. A directory whose only
candidates were all consumed by renames loses its `? dir/` marker; truncated or blocked
probes conservatively keep markers. An unreadable path never silently degrades into "no
rename": text formats fail closed with `LBR-IO-001`, while `--json` reports the partial
result through `data.io_blocked[]` (see *The io_blocked partial contract* below). A
destination whose name is not valid UTF-8 keeps its base `??` row but sits out rename
scoring entirely in this release, surfacing one `rename_path_encoding_unsupported`
warning (non-UTF-8 candidate scoring is a deferred extension). Detection runs on
repository-root-relative paths, so renames are found correctly even when `status` is invoked
from a subdirectory.

Renames render as Git-compatible records in every format: `renamed: <old> -> <new>` in the
human long format, `R  <old> -> <new>` in `--short` (`XY SP <new> NUL <old> NUL` under `-z`),
a single `2 XY <sub> <mH> <mI> <mW> <hH> <hI> R<score> <new>\t<old>` record — `XY` is the second
field (`R.` staged-only, `.R` unstaged-only) and `R<score>` the ninth, a separate column — with
real HEAD/index/worktree modes and hashes in
`--porcelain=v2`, and a top-level `renames[]` array (`{from, to, score, exact, staged,
unstaged}`) in `--json` — never as two separate `R`/`1 R` rows for the endpoints. When the
rename's destination is then modified or deleted in the worktree, that state rides in the
record's second XY column (`RM` / `RD`, like Git); a deleted destination reports `mW` as
`000000` in porcelain v2.

```bash
libra status --find-renames
libra status --find-renames=75
```

### Warnings and exit arbitration

Every structured warning is a `{code, message, source}` triple drawn from one frozen
schema — there is no bypass stderr-only channel; repository-level preflight advisories from
other subsystems are folded into the same list (see below):

| Code | Source | Meaning |
|------|--------|---------|
| `rename_limit_product_skipped` | `rename_detect` | One side exceeded the per-side rename limit; the exhaustive inexact stage was skipped |
| `similarity_budget_exceeded` | `rename_detect` | The similarity-comparison budget was exhausted; only the exhaustive inexact pass was discarded — exact and already-scored unique-basename matches were kept |
| `probe_truncated` | `probe` | The rename-destination probe tripped its enumeration/destination budget; pairing is partial |
| `rename_path_encoding_unsupported` | `rename_detect` | Candidates with non-UTF-8 names sat out rename scoring (base `??`/`D` rows unaffected) |
| `metadata_unavailable` | `metadata` | Repository objects missing, corrupt, unavailable, or cancelled mid-read (WIO-03 kills the out-of-process object-read helper when the ObjectReadBudget wall-clock deadline elapses); the dependent inexact candidates were skipped |
| `metadata_budget_exceeded` | `metadata` | Object-read budget or per-object size cap reached (2 MiB / 64 MiB / 64 objects / 5s batch); remaining candidates skipped |
| `worktree_budget_exceeded` | `worktree` | Worktree-read budget or per-file size cap reached; remaining candidates skipped |
| `worktree_read_failed` | `worktree` | A worktree read failed (I/O error); the affected path is in `data.io_blocked[]` or its rename candidate was skipped |
| `worktree_permission_denied` | `worktree` | A path could not be inspected (EACCES); the path is in `data.io_blocked[]` |
| `worktree_io_timeout` | `worktree` | A worktree read exceeded its deadline; the out-of-process I/O worker (not the status caller) is recycled. Ignore-file lookups stay in-process (SQLite-backed `core.excludesFile`) and use the thread-pool deadline |
| `dirty_cache_lock_stolen` | `cache` | A previous scanner's stale lock was stolen — that scanner may not have finished persisting; THIS scan rebuilds the cache and its result is persisted |
| `dirty_cache_stale_fallback` | `cache` | The dirty cache was missing/stale; degraded to a full status |
| `dirty_cache_concurrent_invalidate` | `cache` | A concurrent writer invalidated the cache mid-read |
| `dirty_cache_path_unencodable` | `cache` | A non-UTF-8 path could not be stored in the dirty cache; its row was omitted (the full status still reports it) |
| `repository_preflight` | `config` | A repository-level advisory raised before the command ran (e.g. a pending durable object-index repair) |

The `source` column is a frozen enum: `config`, `probe`, `rename_detect`, `worktree`,
`metadata`, `cache`. `probe` and `rename_detect` are deliberately distinct — a `probe`
warning means candidates may never have been *seen*, while `rename_detect` means they were
seen but could not be scored. `config` covers repository-level advisories that are not tied to a scan — currently the
`repository_preflight` row above. Config *resolution* itself never warns: an invalid value
fails closed instead of degrading.

Repository-level preflight advisories from other subsystems (for example a pending durable
object-index repair, emitted before any command runs) follow the SAME delivery matrix as status
warnings: printed on stderr in text modes, and carried in `data.warnings[]` as
`repository_preflight` / `config` under `--json` — where stderr stays clean. That keeps the rule
exact: exit 9 always corresponds to at least one entry in the structured list, so a `--json`
consumer never has to read stderr to learn why its exit code changed.

Human/short/porcelain modes print status warnings as `warning: …` on stderr (even under
`--quiet`); `--json` carries them in `data.warnings[]` and keeps stderr
clean on every successful run; the single exception is a non-EPIPE stdout failure while
emitting the JSON envelope itself, where the pending warnings are flushed to stderr rather
than silently lost. With the global `--exit-code-on-warning`, a warning exits 9 and takes precedence
over the `--exit-code` dirty exit 1 in every output mode; a fatal error (128/129) always
wins over both. The full priority is **fatal ≻ 9 (on-warning) ≻ 1 (dirty, including a
non-empty `io_blocked`) ≻ 0**, resolved by one arbitration path for every output mode.

### The io_blocked partial contract

A path the scan **cannot inspect** (permission denied, I/O failure) is neither a deletion
nor clean — fabricating either would corrupt downstream automation (`commit -a` recording
a fake deletion, a dirty check reporting clean). Instead:

- **Text formats** (human/short/porcelain) fail closed with `LBR-IO-001` naming the first
  blocked path and the total count, hinting at `--json` for the partial view.
- **`--json`** succeeds and reports the partial result: every blocked path appears in
  `data.io_blocked[]` as `{path: {display, raw_base64}, staged, reason, rename}` —
  `display` is the escaped repo-relative form (same quoting as non-`-z` porcelain),
  `raw_base64` carries the exact OS bytes when the name is not valid UTF-8 (else `null`) — on Unix
  the raw `OsStr` bytes, on Windows the UTF-16 code units serialized little-endian, so an
  unpaired surrogate round-trips too —
  `staged` is the known staged component (`"M"`/`"A"`/`"D"`/`"R"` or `null`), `reason` is
  `"permission_denied"`, `"io_error"`, or `"io_timeout"` (a single filesystem operation
  exceeded the probe deadline — Libra recycles the out-of-process I/O worker and
  keeps the last checkpointed partial, rather than hanging the `status` caller;
  `.gitignore` / `.libraignore` lookups are not helper-backed and still use the
  in-process thread-pool deadline). Directory walk is bounded by a repository-root
  fd/handle: Linux uses `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`; other
  Unix platforms walk with `openat(O_NOFOLLOW | O_DIRECTORY)`; Windows opens with
  `FILE_FLAG_OPEN_REPARSE_POINT` and fails closed on a reparse point. A directory
  swapped for an escaping symlink between check and open is reported as
  `io_error` in `io_blocked[]`, never followed.
  `rename` is the affected staged rename pair
  (`{from, to, score}`) when one is known. Entries are sorted by raw path bytes and
  deduplicated; each also emits a `worktree_*` warning.
- `data.base_scan_complete` is `false` when the base scan itself was blocked;
  `data.rename_detection_complete` is `false` whenever anything degraded rename pairing
  (probe truncation/blocks, engine skips, budgets, encoding skips); `data.complete` is the
  AND of both. `is_clean` is always `false` while `io_blocked` is non-empty, and
  `--exit-code` reports dirty (exit 1).
- The dirty-cache extensions never persist doubt: a blocked `--scan` refuses to replace
  the cache, and `--check-dirty` revalidation keeps rows it cannot re-verify untouched.


### `--renames` / `--no-renames`

Toggle rename detection. The three spellings `--renames` / `--no-renames` /
`--find-renames[=N]` obey true last-one-wins in argv order — whichever appears last decides
(`--no-renames --find-renames=80` re-enables at 80%; `--find-renames=80 --no-renames`
disables). Config applies only when none is given. The `status.renames` config (falling back to
`diff.renames`) sets the default through the strict local → global → system cascade: `false`
disables detection, a truthy value or an unset key enables it at 50%. `copy`/`copies` are
rejected (copy detection is not yet supported) rather than degrading to plain renames. Invalid
values fail closed with `LBR-CLI-002` before any output. Precedence applies to the *value*, not
to validation: like Git, the config is parsed before the flags are applied, so
`status.renames=copy` (or any non-boolean) still fails closed even when `--no-renames` would
have made the value irrelevant — fix the config rather than masking it with a flag. The Libra
dirty-cache extensions (`--cached`/`--check-dirty`) do not run rename detection at all, and
combining them with any rename flag is a usage error (`LBR-CLI-002`) rather than a silent
disable — the cache stores no rename information, so a request for it cannot be honored.

```bash
libra status --renames
libra status --no-renames
libra config status.renames false   # disable rename detection (for this config scope)
```

### `--scan` / `--cached` / `--check-dirty` (Libra extensions, lore.md 1.1)

`--scan` runs the normal full status AND atomically rebuilds the dirty-set
cache from it (TOCTOU-guarded on the index fingerprint + HEAD; a scan lock
blocks concurrent scanners, stale locks are stolen). `--cached` consumes the
cache instead of walking the worktree — O(dirty paths); any freshness doubt
degrades to the full status with a hint. Snapshot semantics: worktree-only
edits made after the scan are invisible until a rescan or a `libra dirty`
mark (that is what the marks are for). NOTE: unrelated to Git's `--cached`
(= the index). `--check-dirty` re-verifies only the cached set, pruning rows
proven clean. The three are mutually exclusive and conflict with
`--porcelain`/`--short`/`--ignored`; default `status` never touches the
cache and its JSON gains no keys. See [dirty.md](dirty.md).

### `--ignored`

Include ignored files in the output. Ignored/untracked classification follows
the shared ignore sources — `.gitignore`, the worktree-local `info/exclude` (`.libra/info/exclude`, plus `.git/info/exclude` in Git- or dual-layout trees),
`core.excludesFile`, and `.libraignore` (nearest directory wins; a
`.libraignore` beats a sibling `.gitignore`) — see
[check-ignore.md](check-ignore.md) for the full precedence.

```bash
libra status --ignored
```

### `-u, --untracked-files [<MODE>]`

Control how untracked files are displayed. Accepted values: `normal` (default, shows untracked
directories but not their contents), `all` (recursively lists files within untracked directories),
`no` (hides untracked files entirely). As in Git, the flag with no value means `all`, and the short
form takes an attached value (`-uno`, `-uall`, `-unormal`). When the flag is absent, the
`status.showUntrackedFiles` config default applies (any output format); the flag always wins.

```bash
libra status -uno                  # hide untracked files
libra status -u                    # same as -uall (recurse into untracked dirs)
libra status --untracked-files=all
```

### `--show-stash` / `--no-show-stash`

Show the number of stash entries after the long-format status ("Your stash
currently has N entries"). Only the long format renders the hint (short and
porcelain are unaffected). `status.showStash=true` enables it by default;
`--no-show-stash` overrides the config (and an earlier `--show-stash`; the
last one wins).

```bash
libra status --show-stash
libra status --no-show-stash
```

### `--exit-code`

Exit with code 1 if the working tree has changes, exit 0 if clean. Useful for scripting
and CI pipelines to detect dirty state without parsing output.

```bash
libra status --exit-code
libra status --quiet --exit-code   # silent dirty check
```

## Common Commands

```bash
libra status
libra status --short
libra status --porcelain -z
libra status --column
libra status --find-renames
libra status --json
libra status --exit-code
```

## Human Output

Default human mode writes the status summary to `stdout`.

Clean working tree:

```text
On branch main
nothing to commit, working tree clean
```

With changes:

```text
On branch main
Your branch is ahead of 'origin/main' by 2 commits.
  (use "libra push" to publish your local commits)

Changes to be committed:
        new file:   src/feature.rs
        modified:   src/lib.rs

Changes not staged for commit:
        modified:   README.md

Untracked files:
        notes.txt
```

Detached HEAD:

```text
HEAD detached at abc1234
nothing to commit, working tree clean
```

Short format (`--short`):

```text
A  src/feature.rs
M  src/lib.rs
 M README.md
?? notes.txt
```

Unmerged conflict:

```text
UU conflict.txt
```

`--quiet` suppresses all `stdout` output. Combined with `--exit-code`, it acts as a
silent dirty check (exit 1 if dirty, exit 0 if clean).

## Structured Output

`libra status` supports the global `--json` and `--machine` flags.

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- `stderr` stays clean on success: every warning, including a repository-level preflight
  advisory from another subsystem, is delivered through `data.warnings[]`

Example:

```json
{
  "ok": true,
  "command": "status",
  "data": {
    "head": {
      "type": "branch",
      "name": "main"
    },
    "has_commits": true,
    "upstream": {
      "remote_ref": "origin/main",
      "ahead": 2,
      "behind": 0,
      "gone": false
    },
    "staged": {
      "new": ["src/feature.rs"],
      "modified": ["src/lib.rs"],
      "deleted": []
    },
    "unstaged": {
      "modified": ["README.md"],
      "deleted": []
    },
    "untracked": ["notes.txt"],
    "ignored": [],
    "is_clean": false
  }
}
```

Clean working tree:

```json
{
  "ok": true,
  "command": "status",
  "data": {
    "head": {
      "type": "branch",
      "name": "main"
    },
    "has_commits": true,
    "upstream": null,
    "staged": {
      "new": [],
      "modified": [],
      "deleted": []
    },
    "unstaged": {
      "modified": [],
      "deleted": []
    },
    "untracked": [],
    "ignored": [],
    "is_clean": true
  }
}
```

Detached HEAD:

```json
{
  "ok": true,
  "command": "status",
  "data": {
    "head": {
      "type": "detached",
      "oid": "abc1234def5678..."
    },
    "has_commits": true,
    "upstream": null,
    "staged": { "new": [], "modified": [], "deleted": [] },
    "unstaged": { "modified": [], "deleted": [] },
    "untracked": [],
    "ignored": [],
    "is_clean": true
  }
}
```

### Schema Notes

- `head.type` is `"branch"` or `"detached"`
- When on a branch, `head.name` is the branch name; when detached, `head.oid` is the commit hash
- `upstream` is `null` when no tracking branch is configured or HEAD is detached
- `upstream.gone` is `true` when the remote tracking branch no longer exists
  (the tracking ref is resolved under its fully-qualified
  `refs/remotes/<remote>/<branch>` name — the shape clone/fetch/push write —
  with a legacy short-name fallback; issue #464)
- `upstream.ahead` / `upstream.behind` are `null` when `gone` is `true`
- `is_clean` is `true` only when staged, unstaged, untracked, and unmerged
  lists are empty, no global merge state is active, **and** `io_blocked` is
  empty ("cannot inspect" is never clean)
- `has_commits` is `false` in a freshly initialized repository with no commits
- `staged.renamed` / `unstaged.renamed` list rename pairs as `{from, to}`
  objects (path strings, repo-root-relative), and the top-level `renames[]`
  array carries the structured records (`{from, to, score, exact, staged,
  unstaged}`), sorted by destination
- `warnings[]` carries the structured `{code, message, source}` warnings (see
  *Warnings and exit arbitration*)
- `io_blocked[]` lists paths the scan could not inspect (see *The io_blocked
  partial contract*); `base_scan_complete`, `rename_detection_complete`, and
  their AND `complete` report whether anything was degraded
- `stash_entries` (optional, integer): present only when `--show-stash` is
  passed. Counts the entries on the stash stack (matching `libra stash list`)
  and may be `0`. Omitted entirely without `--show-stash` so JSON consumers
  can distinguish "stash subsystem not queried" from "stash subsystem
  queried, returned zero" — i.e. the field's *presence* signals an
  explicit opt-in, not the existence of stashed work.

## Design Rationale

### Porcelain v1 and v2

`libra status --porcelain` (no version) emits Git's classic v1 short-format
layout (`XY <path>` per file). `libra status --porcelain v2` emits the
extended v2 line layout — for each tracked file:

```text
1 XY <sub> <mode_HEAD> <mode_index> <mode_worktree> <hash_HEAD> <hash_index> <path>
```

A detected rename is ONE `2` record instead of two `1` rows:

```text
2 XY <sub> <mode_HEAD> <mode_index> <mode_worktree> <hash_HEAD> <hash_index> R<score> <new>\t<old>
```

`R<score>` is the similarity percentage (`R100` for an exact pair). The
`mode_HEAD`/`hash_HEAD` columns describe the rename's ORIGIN (the old
path's HEAD entry), while `mode_index`/`hash_index` describe the new
path's staged entry — a PURE unstaged rename (`.R`) therefore copies the
index entry into both the HEAD and index columns, because HEAD does not
know the destination. When the source also carries a staged change the
first column reflects it (`MR`, or `AR` for a staged-new source) and the
HEAD columns come from the real HEAD tree entry instead of the copy —
`hash_HEAD` and `hash_index` then differ. A destination modified or deleted in the worktree
rides in the second XY column (`RM`/`RD`); a deleted destination reports
`mode_worktree` as `000000`. Under `-z` the record's trailing field pair
is `…R<score> <new> NUL <old> NUL` — NEW first, then OLD, with raw
unquoted path bytes and no tab separator.

Untracked entries collapse to `? <path>` and ignored entries to `! <path>`,
matching Git's own v2 encoding. The implementation lives in
`src/command/status.rs::output_porcelain_v2` and is fed by
`build_porcelain_v2_data`, which pulls mode + hash metadata out of the
index and HEAD tree before rendering.

With `-z`, porcelain v1 and v2 records are NUL-terminated and contain no
trailing newlines. Rename-capable porcelain output does not use the human
`old -> new` arrow form under `-z`; scripts should split fields on NUL.

Most consumers should still prefer `--json` (or `--machine` for compact
single-line JSON): the JSON envelope carries the same staged/unstaged/
untracked partitioning plus upstream tracking and `stash_entries`, and
is far easier to parse than v2's positional text columns. Use
`--porcelain v2` only when you specifically need Git-compatible output
for tooling that already speaks the v2 grammar.

### Explicit `--exit-code` instead of implicit behavior

Git's `git status` always exits 0 regardless of repository state, and checking for dirty state
requires `git diff --exit-code` or parsing `git status --porcelain` output. Libra adds an
explicit `--exit-code` flag that returns exit 1 when the working tree is dirty. This is
intentionally opt-in (rather than default) to avoid breaking scripts that check `$?` after
`libra status`. Combined with `--quiet`, it provides a stdout-free, exit-code-driven dirty check
that is cleaner than parsing text output.

### `--show-stash` in standard mode only

The `--show-stash` flag only affects the long (standard) human-readable output, not short or
porcelain formats. This matches Git's behavior where `--show-stash` appends a stash summary
line to the long format. In JSON output, stash information could be added to the envelope in a
future iteration without needing a separate flag, since JSON consumers can simply ignore fields
they do not need.

### Enhanced upstream tracking info in JSON

Git's porcelain v1 does not include upstream tracking information; porcelain v2 adds a header
line with ahead/behind counts. Libra's JSON output always includes a full `upstream` object
with `remote_ref`, `ahead`, `behind`, and `gone` fields when a tracking branch is configured.
This rich upstream data is critical for AI agents and CI tools that need to determine whether
a branch needs to be pushed or pulled, without having to run separate `libra log` or
`libra branch -vv` commands.

## Parameter Comparison: Libra vs Git vs jj

| Parameter / Flag | Git | jj | Libra |
|---|---|---|---|
| Show status | `git status` | `jj status` / `jj st` | `libra status` |
| Long format | `git status --long` (default) | N/A | `libra status --long` (default) |
| Short format | `git status -s` / `--short` | N/A (always short) | `libra status -s` / `--short` |
| Porcelain v1 | `git status --porcelain` | N/A | `libra status --porcelain` |
| Porcelain v2 | `git status --porcelain=v2` | N/A | `libra status --porcelain v2` |
| Branch info in short | `git status -sb` | Always shown | `libra status -sb` (`--short --branch`) |
| Show stash count | `git status --show-stash` | N/A | `libra status --show-stash` (standard mode) |
| Show ignored files | `git status --ignored` | N/A | `libra status --ignored` |
| Untracked files control | `git status -u<mode>` | N/A (always shows) | `libra status -u<mode>` / `--untracked-files=<mode>` |
| Exit code for dirty | `git diff --exit-code` | N/A | `libra status --exit-code` |
| Quiet mode | `git status -q` | N/A | `libra status --quiet` (global flag) |
| Column display | `git status --column` | N/A | `libra status --column` (`--no-column` countermands) |
| Ahead/behind display | `git status -sb` (text only) | N/A | Human + structured `upstream` object in JSON |
| Find renames | `git status -M` | Automatic | `--find-renames` / `--renames` |
| Ignore submodules | `git status --ignore-submodules` | N/A | N/A (no submodules) |
| Structured JSON output | N/A | N/A | `--json` / `--machine` |
| Error hints | Minimal | Minimal | Every error type has an actionable hint |

## Exit Code Behavior

| Flag | Clean | Dirty |
|------|-------|-------|
| (default) | exit 0 | exit 0 |
| `--exit-code` | exit 0 | exit 1 |

`--exit-code` enables a silent dirty check useful for scripting. When combined with
`--quiet`, no `stdout` is produced -- the exit code signals the repository state. Diagnostics
are NOT suppressed: warnings still reach `stderr` (that is what makes `--quiet
--exit-code-on-warning` actionable), and a blocked path still fails closed with its
`LBR-IO-001` message.

A non-empty `io_blocked` set counts as dirty for `--exit-code`. With the global
`--exit-code-on-warning`, any structured warning exits 9 instead; the full arbitration
priority in every output mode is **fatal (128/129) ≻ 9 (on-warning) ≻ 1 (dirty) ≻ 0**.

## Error Handling

Every `StatusError` variant maps to an explicit `StableErrorCode`.

| Scenario | Error Code | Exit | Hint |
|----------|-----------|------|------|
| Index file corrupted | `LBR-REPO-002` | 128 | "the index file may be corrupted" |
| Invalid path encoding | `LBR-CLI-003` | 129 | "path contains invalid characters" |
| Failed to hash a file | `LBR-IO-001` | 128 | -- |
| Cannot list working directory | `LBR-IO-001` | 128 | -- |
| Working directory not found | `LBR-REPO-001` | 128 | -- |
| Bare repository | `LBR-REPO-003` | 128 | "this operation must be run in a work tree" |

## Compatibility Notes

- `--porcelain v2` emits the real v2 line grammar (`1`/`2`/`u`/`?`/`!` records with
  mode/hash columns); `--json` remains the richer structured surface
- Under `-z`, Unix porcelain v1/v2 write RAW OS path bytes (no quoting, lossless for
  non-UTF-8 names); non-`-z` formats follow `core.quotePath` for bytes above
  `0x7F` — octal-escaped when `true` (the default), written raw when `false`.
  A non-UTF-8 name never fails status — it keeps its
  base `??` row and only sits out rename scoring (with a
  `rename_path_encoding_unsupported` warning)
- jj's `jj status` always uses a short format and does not distinguish staged from unstaged changes (jj has no staging area)
- Rename detection is supported via `--find-renames[=<n>]` and the `--renames`/`--no-renames` toggles; Git's short `-M` alias is not exposed
- `--column` column-aligned display is supported; `--no-column` (equivalent to `--column=never`) countermands an earlier `--column` via clap's symmetric override (last one wins), and status is not columnar by default so `--no-column` alone is a no-op
