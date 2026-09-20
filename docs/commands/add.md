# `libra add`

Stage file contents for the next commit.

## Synopsis

```
libra add [OPTIONS] [PATHSPEC...]
libra add -A
libra add -u [PATHSPEC...]
libra add --refresh [PATHSPEC...]
libra add --resolved [PATHSPEC...]
```

## Description

`libra add` stages file changes from the working tree into the index, preparing them
for the next `libra commit`. It supports shared Git-style pathspec matching,
`--dry-run` preview, and `--refresh` to re-stat already tracked entries without
staging new content.

The command resolves pathspecs relative to the current working directory, validates them
against the repository root, and respects Git/Libra ignore sources. Files tracked by LFS are
automatically staged as pointer files. The `-A` flag stages all changes (adds, modifies,
removes) across the entire working tree, while `-u` updates only tracked files without
adding new ones.

Symbolic links are staged as Git-compatible symlink blobs: the index mode is
`120000` and the blob content is the link target bytes. The link is never
followed while staging, so a symlink pointing outside the worktree is recorded
as the link itself rather than as the target file's contents.

## Options

### `[PATHSPEC...]`

One or more files or directories to stage. Paths are resolved relative to the current
directory. Required unless `-A`, `-u`, `--refresh`, or `--resolved` is specified.

The global `--literal-pathspecs` flag (and `GIT_LITERAL_PATHSPECS`) disables
globbing and `:(magic)` for this invocation; `--no-literal-pathspecs` turns
that off. Unlike Git, the flag is also accepted after `add`.

Pathspecs use Libra's shared Git-style matcher: plain pathspecs match a file or
directory prefix, wildcard pathspecs are supported, and the high-value magic
forms `:(top)`, `:/`, `:(glob)`, `:(literal)`, `:(icase)`,
`:(exclude)`, `:!`, and `:^` are honored. Exclude pathspecs subtract from the
positive selection, and pathspec matching follows `core.ignorecase` when enabled.
Wildcard-looking pathspecs also match an exact path or directory prefix with
the same literal text, matching Git's bracket-file and bracket-directory
behavior.

```bash
libra add file.txt
libra add src/ tests/
libra add .
libra add ':(glob)src/*.rs' ':(exclude)src/generated.rs'
libra add ':(literal)literal/[abc].txt'
```

### `-A, --all`

Update the index to match the entire working tree. Stages new files, modifications, and
deletions. When no pathspec is given, all files in the working tree are updated. Mutually
exclusive with `-u` and `--refresh`.

```bash
libra add -A
```

### `-u, --update`

Update the index only where it already has entries matching the pathspec. Stages
modifications and deletions of tracked files but does not add new (untracked) files.
A pathspec that names an untracked working-tree file is refused before any
staging (`pathspec '…' did not match any file(s) known to the index`,
`LBR-CLI-003`, exit 129) and the index is left unchanged. `--ignore-errors`
skips that check and stages the paths that do match. Mutually exclusive with
`-A` and `--refresh`.

```bash
libra add -u
libra add -u src/
```

### `--refresh`

Refresh index entries for all files currently in the index. Updates only metadata
(timestamps, file size) of existing index entries to match the working tree, without
adding new files or removing entries. Mutually exclusive with `-A` and `-u`.

```bash
libra add --refresh
```

### `-f, --force`

Allow adding files that are otherwise ignored by Git/Libra ignore rules.

```bash
libra add -f ignored_file.log
```

### `-n, --dry-run`

Preview what would be staged without actually modifying the index. Output shows which
files would be added, modified, or removed. `-n` matches Git; `-d` is also accepted
as a Libra-compatible short alias.

```bash
libra add -n file.txt
libra add --dry-run .
```

### `-v, --verbose`

Produce more detailed output, showing per-file actions during staging.

```bash
libra add -v src/
```

### `--ignore-errors`

Continue staging remaining files when individual paths fail. Failed paths are reported
in the output but do not cause the command to exit with an error.

```bash
libra add --ignore-errors src/
```

### `--pathspec-from-file <file>`

Read pathspecs from `<file>` (one per line); the command line must then carry no
pathspec arguments. Entries use the same shared pathspec matcher and magic forms as
positional pathspecs. A value of `-` reads the list from stdin instead of opening a
file (a worktree file literally named `-` is never read). Newline mode splits on `\n`
and strips one trailing `\r` per line, so CRLF lists work; empty lines are ignored.
Pair with `--pathspec-file-nul` when the list is NUL-separated (e.g. produced by
another tool's `-z` output) — NUL mode keeps every byte, including a CR.

A payload that is not valid UTF-8, or a file that cannot be read, is a fatal
`LBR-IO-001` error (exit 128) with zero writes. An empty list is a usage error
(`nothing specified, nothing added`, exit 129); Git accepts an empty list as a
no-op, an intentional difference.

In newline mode a line that starts with `"` is decoded as one Git C-style quoted
string (`\n`, `\t`, `\"`, `\\`, octal escapes, …), so paths containing spaces
or quotes survive; an unquoted line is used verbatim. Malformed quoting
(unterminated, trailing bytes after the closing quote, or an unknown escape) is a
fatal `LBR-IO-001` error (exit 128) with zero writes. NUL mode never decodes.

`--pathspec-from-file` cannot be combined with `-p`/`--patch`, `--edit`,
`--interactive`, or command-line pathspec arguments (Git's
`cannot be used together` contract): each combination is a usage error
(`LBR-CLI-002`, exit 129) that writes nothing. (Git refuses the same combinations
with exit 128; Libra's 129 is its usage-error code — an intentional difference.
`--interactive` keeps its own declined-flag refusal, exit 128 +
`LBR-UNSUPPORTED-001`.)

```bash
libra add --pathspec-from-file paths.txt
printf 'a.txt\nb.txt\n' | libra add --pathspec-from-file=-
libra add --pathspec-from-file paths.bin --pathspec-file-nul
```

### `--pathspec-file-nul`

Treat the `--pathspec-from-file` input as NUL-separated rather than newline-separated.
Requires `--pathspec-from-file`; using it alone is a usage error.

### `--chmod=(+|-)x`

Staged file modes also honor `core.filemode`: when it is `false`, re-staging
an existing entry keeps its recorded mode and a new path is recorded as
`100644`; `--chmod` (and `update-index --cacheinfo`) still apply the explicit
mode. An invalid `core.filemode` value fails `add` closed before any write.

Force the executable bit recorded in the index for the matched paths: `+x` records
mode `100755`, `-x` records `100644`. The blob content is unchanged. A path whose
recorded mode actually changes is reported as modified, even when its content did
not change.

Only regular files carry an executable bit. A matched symlink (`120000`) or
gitlink (`160000`) is refused: the entry is left unchanged, an
`error: cannot chmod +x '<path>'` line is printed to stderr for each refusal, and
`add` exits 1 after the remaining paths have been processed normally. In `--json`
mode the refusals appear as `chmod_rejected: [{"path", "flip"}]` on the envelope
instead, with the same exit 1. (Git exits 255 here; Libra uses the process-level
exit 1 model shared with the ignored-path report.)

An invalid value (anything other than `+x` / `-x`) is a usage error.

With no pathspec (and none of `-A`, `-u`, `--refresh`, `--renormalize`,
`--resolved`), `--chmod` is a successful no-op: `add` exits 0 without touching
the index or object store — there is nothing to apply the mode to.

```bash
libra add --chmod=+x scripts/build.sh
libra add --chmod=-x notes.txt
```

### `--renormalize`

Re-stage tracked files from scratch, rewriting their blobs even when the content is
unchanged. Implies `-u`: only tracked files are processed (never untracked ones), and
a tracked file removed from the working tree has its deletion staged.

```bash
libra add --renormalize
libra add --renormalize src/
```

### `--ignore-missing`

Under `--dry-run`, a pathspec that matches no add candidate is classified
against the configured ignore rules (`.libraignore`, `.gitignore`): an ignored
pattern is reported like other ignored paths and makes `add` exit 1 (unless it
is the only pathspec, which keeps the `LBR-ADD-001` / exit 128 contract); a
path that is not ignored is skipped with a warning on stderr. Mirrors Git:
`--ignore-missing` requires `--dry-run`.

```bash
libra add --dry-run --ignore-missing maybe-missing.txt other.txt
```

### `--resolved`

Stage only unmerged (conflict) paths. Working-tree copies still containing
conflict markers are refused as a group (`LBR-CONFLICT-001`, exit 128) and the
index is left unchanged. A path whose working-tree file was deleted is removed
from the index. Does not require a pathspec; when one is given, only matching
unmerged paths are considered. Unconflicted local modifications are not staged.

Mutually exclusive with `-u`/`--update` and `-A`/`--all`. The diagnostic is
Git's `options '…' and '--resolved' cannot be used together` (`LBR-CLI-002`,
exit 129). Git reports the same combination as exit 128.

```bash
libra add --resolved
libra add --resolved path/to/file
```

### `--sparse`

Allow updating entries that exist outside the sparse-checkout definition
(skip-worktree entries). Without it, a pathspec that matches only such an
entry is reported on stderr — a header naming the sparse-checkout definition,
each pathspec, and a hint — and `add` exits 1 without touching the index
(`--dry-run`, `--ignore-missing`, `--renormalize`, and `--chmod` included).
JSON carries `data.sparse_paths: [string]` and no human diagnostic.

With `--sparse` the entry is stageable: a modified working-tree file is staged
and the skip-worktree bit is preserved; a deleted working-tree file keeps the
ordinary `pathspec '…' did not match any files` error. A pathspec that also
matches a non-sparse entry gets no sparse diagnostic.

```bash
libra add --sparse path/to/sparse-file
libra --json add --dry-run --ignore-missing path/to/sparse-file
```

### `-p, --patch`

Interactively stage hunks. For each hunk Libra prints the unified diff and
prompts `Stage this hunk [y,n,q,a,d,s,e,p,P,?]? ` (letters shrink to the
commands that apply). `s` splits a hunk at context islands; `e` opens the
hunk in `$GIT_EDITOR` / `core.editor`. `--auto-advance` (default) moves to
the next hunk after `y`/`n`; `--no-auto-advance` stays and offers `>`/`<`
to cycle files. Cannot be combined with `--json`, `--machine`, `--dry-run`,
or `--resolved`.

```bash
libra add -p
libra add -p --no-auto-advance src/main.rs
```

### `--auto-advance` / `--no-auto-advance`

Last one wins. `--no-auto-advance` without `-p`/`--patch` is
`LBR-CLI-002` / 128: `the option '--no-auto-advance' requires '--interactive/--patch'`.

## Common Commands

```bash
libra add file.txt
libra add src/
libra add .
libra add -n file.txt
libra add --refresh
libra add --ignore-errors src/
libra add --pathspec-from-file paths.txt
libra add ':(glob)src/*.rs' ':(exclude)src/generated.rs'
libra add --chmod=+x scripts/build.sh
libra add --renormalize
libra add --resolved
libra add -p
```

Unmerged (conflict) paths are part of the same candidate set: `add`, `add -A`,
`add .`, and `add -u` write the working-tree copy to stage 0 and drop stages
1–3 in the same index transaction. Ordinary `add` does not check leftover
conflict markers (`--resolved` does). Resolved unmerged paths are reported as
modified, not as new files.

## Human Output

When stdout is a terminal, default human mode writes the staging summary.
When stdout is redirected or piped, default mode is silent (matching Git).
`-v` and `--dry-run` always print. `--quiet` still suppresses stdout.
Stderr warnings are unchanged.

Single file:

```text
add 'src/main.rs' (new file)
```

Multiple files:

```text
add 'src/main.rs' (new file)
add 'src/lib.rs' (modified)
add 'old.txt' (deleted)
```

Dry-run:

```text
add 'src/main.rs' (new file)
add 'src/lib.rs' (modified)
(dry run, no files were staged)
```

Ignored files produce a warning on `stderr`:

```text
warning: the following paths are ignored by configured ignore rules:
ignored.log
Hint: use -f if you really want to add them.
```

When some paths were staged (or reported by a dry-run) **and** other explicit
pathspecs were ignored, `add` finishes the whole operation — staging, output,
warnings and automation events included — and then exits `1`, like Git.
When *every* path was ignored and nothing else was staged, `add` fails with
`LBR-ADD-001` and exits `128` instead (intentional difference from Git's 1).
`--json` keeps the regular data envelope on stdout (with the ignored paths in
`data.ignored`) and exits `1`; the exit code `1` also wins over
`--exit-code-on-warning`'s `9`.

`--quiet` suppresses all `stdout` output but preserves `stderr` warnings.

## Structured Output

`libra add` supports the global `--json` and `--machine` flags.

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- `stderr` stays clean on success

Example:

```json
{
  "ok": true,
  "command": "add",
  "data": {
    "added": ["src/main.rs"],
    "modified": ["src/lib.rs"],
    "removed": ["old.txt"],
    "refreshed": [],
    "ignored": [],
    "failed": [],
    "dry_run": false
  }
}
```

Dry-run:

```json
{
  "ok": true,
  "command": "add",
  "data": {
    "added": ["src/main.rs"],
    "modified": [],
    "removed": [],
    "refreshed": [],
    "ignored": [],
    "failed": [],
    "dry_run": true
  }
}
```

Partial failure with `--ignore-errors`:

```json
{
  "ok": true,
  "command": "add",
  "data": {
    "added": ["good.txt"],
    "modified": [],
    "removed": [],
    "refreshed": [],
    "ignored": [],
    "failed": [
      {"path": "bad.bin", "message": "file too large"}
    ],
    "dry_run": false
  }
}
```

### Schema Notes

- `added` / `modified` / `removed` correspond to new, changed, and deleted files staged
- `refreshed` is populated only when `--refresh` is used
- `ignored` lists paths skipped by Git/Libra ignore rules
- `failed` lists paths that failed to stage, each with `path` and `message`
- `dry_run` is `true` when `-n` / `--dry-run` is passed; no files are actually staged

## Design Rationale

### No `--intent-to-add` / `-N`

Git's `--intent-to-add` (`-N`) records an empty blob for untracked files so that they
appear in `git diff` output without actually staging their content. This is a workflow
convenience for reviewing new files before staging them. Libra omits this flag because
`libra status` already shows untracked files clearly, and `libra diff` is designed to
work with the full working tree state. The two-step "intent then stage" workflow adds
cognitive overhead without meaningfully improving the review experience. Users who want
to review new files before committing can use `libra add --dry-run` followed by
`libra diff --staged` after staging.

### `--patch` / `-p` interactive staging

`libra add -p` is the Git-compatible hunk session (`y/n/q/a/d/j/J/k/K/g///s/e/p/P/?`,
`--[no-]auto-advance`). `--json` / `--machine` / `--dry-run` stay refused with the
patch session so agents keep a non-interactive path. `add -i` remains declined (D15).

### `--refresh` as explicit flag

In Git, `git add --refresh` silently updates stat information for tracked files. Libra
surfaces this as a first-class mode that is mutually exclusive with `-A` and `-u` (enforced
by clap argument groups). This makes the intent explicit: `--refresh` never stages new
content, only updates metadata. The mutual exclusivity prevents confusing combinations like
`-A --refresh` where the user's intent would be ambiguous.

### Ignore Source Precedence

Libra reads Git standard ignore files (`.gitignore`, the worktree-local
`info/exclude` — `.libra/info/exclude` in the current worktree's own gitdir,
plus `.git/info/exclude` in a Git- or dual-layout tree — and `core.excludesFile`) plus
Libra extension files (`.libraignore`). In the same directory, `.libraignore`
has higher precedence than `.gitignore`; nearer directory sources override
ancestors; `info/exclude` and `core.excludesFile` are lower-precedence
fallbacks. `info/exclude` is per-worktree (never shared via `commondir`; see
[check-ignore.md](check-ignore.md)). All sources use Git ignore pattern
syntax.

`libra init` still creates a root `.libraignore` in non-bare repositories for
Libra-specific rules, and Git imports or non-bare clones may copy existing
`.gitignore` files to matching `.libraignore` files for explicit Libra
overrides.

## Parameter Comparison: Libra vs Git vs jj

| Parameter / Flag | Git | jj | Libra |
|---|---|---|---|
| Stage a file | `git add file.txt` | N/A (jj auto-tracks) | `libra add file.txt` |
| Stage everything | `git add .` or `git add -A` | N/A (automatic) | `libra add .` or `libra add -A` |
| Update tracked only | `git add -u` | N/A | `libra add -u` |
| Dry-run preview | `git add -n` / `--dry-run` | N/A | `libra add -n` / `--dry-run` |
| Force add ignored | `git add -f` | N/A | `libra add -f` |
| Refresh stat info | `git add --refresh` | N/A | `libra add --refresh` |
| Verbose output | `git add -v` | N/A | `libra add -v` |
| Ignore errors | `git add --ignore-errors` | N/A | `libra add --ignore-errors` |
| Intent to add | `git add -N` / `--intent-to-add` | N/A | N/A (not implemented) |
| Interactive patch | `git add -p` / `--patch` | N/A | `libra add -p` / `--patch` |
| Interactive select | `git add -i` / `--interactive` | N/A | N/A |
| Edit diff before staging | `git add -e` / `--edit` | N/A | N/A |
| Chmod only | `git add --chmod=+x` | `libra add --chmod=+x` (non-regular index entries are refused with exit 1) | N/A |
| Sparse checkout paths | `git add --sparse` | N/A | N/A |
| Ignore file | `.gitignore` | N/A (jj uses `.gitignore`) | `.gitignore` + `.libraignore` |
| Structured JSON output | N/A | N/A | `--json` / `--machine` |
| Error hints | Minimal | N/A | Every error type has an actionable hint |

## Error Handling

Every `AddError` variant maps to an explicit `StableErrorCode`.

Local staging and the later cloud-catalog update have separate durability
boundaries. If a terminal background `object_index` error occurs after the blob
and index are saved, `add` keeps its normal success output, emits an actionable
stderr warning, and retains an atomic repair marker for the next schema-aware
repository command. `cloud sync` and destructive agent cleanup fail closed
while repair remains pending. With `--exit-code-on-warning`, the completed local
staging operation returns exit 9 / `LBR-WARN-001`; retrying `add` is unnecessary.

| Scenario | Error Code | Exit | Hint |
|----------|-----------|------|------|
| Not inside a repository | `LBR-REPO-001` | 128 | "run 'libra init' to create a repository" |
| Pathspec matched nothing | `LBR-CLI-003` | 129 | "check the spelling and use 'libra status' to see what changed" |
| Path outside repository root | `LBR-CLI-003` | 129 | "only files within the repository root can be staged" |
| Invalid path encoding | `LBR-CLI-003` | 129 | "path contains invalid UTF-8 characters" |
| Index file corrupted | `LBR-REPO-002` | 128 | "the index file may be corrupted; try 'libra status' to verify" |
| Failed to save index | `LBR-IO-002` | 128 | "check disk space and file permissions" |
| Refresh failed | `LBR-IO-001` | 128 | -- |
| Entry creation failed | `LBR-IO-002` | 128 | -- |
| Object or durable index-marker write failed | `LBR-IO-002` | 128 | Check storage permissions and retry; the error is returned without a panic and the staging area is unchanged: the message says the object payloads were stored safely, no paths were staged, and a direct retry reuses the already-stored payloads without any lock-file cleanup. When the failure is a lock timeout, the message names the holder (its pid and purpose, e.g. `marker_publication`, `queued_update`, `replay`, `deletion_fence`) or says the holder could not be determined; wait for that process to finish and retry. Waiting uses a Git-style quadratic backoff and gives up after 10 seconds. Never delete the lock files under `.libra/object-index-repair-locks`: they exist only to arbitrate concurrent writers, do not block anything by themselves, and are released automatically when their owner exits. Read-only commands that replay pending cloud-index repair markers skip the replay (silently, without a warning) when the lock is busy and retry on the next command |
| Paths staged but cloud index repair remains pending, with `--exit-code-on-warning` | `LBR-WARN-001` | 9 | Fix the reported database/marker error; the next repository command retries automatically |
| Working directory error | `LBR-REPO-001` | 128 | "cannot determine the working tree" |
| Status computation failed | `LBR-REPO-002` | 128 | -- |
| All paths ignored (nothing staged) | `LBR-ADD-001` | 128 | "use -f if you really want to add them" |
| No pathspec and no mode flag | `LBR-CLI-001` | 129 | "maybe you wanted to say 'libra add .'?" |
| `add -u` pathspec is untracked | `LBR-CLI-003` | 129 | "did not match any file(s) known to the index" |
| `--resolved` combined with `-u` or `-A` | `LBR-CLI-002` | 129 | Git's `cannot be used together` wording (Git itself exits 128) |
| `--resolved` with leftover conflict markers | `LBR-CONFLICT-001` | 128 | Lists every still-marked path; the index is not written |

## Compatibility Notes

- jj does not have an `add` command; it automatically tracks all working tree changes
- Libra's `add` is required before `commit`, matching Git's explicit staging model
- `.gitignore` and `.libraignore` both use Git ignore syntax; `.libraignore`
  remains the Libra-specific override file when both exist in the same directory
- An un-ignored missing pathspec still prints Libra's `--ignore-missing` skip
  warning (Git is silent there)
- With a directory pathspec, an already-existing ignored parent directory is not
  additionally reported (Git also lists it)
- C-quoted `--pathspec-from-file` lines accept one to three octal digits (Git
  requires exactly three) and reject trailing bytes after the closing quote
  (Git ignores them)
- LFS-tracked files are automatically converted to pointer files during staging
- Remaining unsupported interactive options fail with `LBR-UNSUPPORTED-001` (`-i`/`--interactive`, D15 remainder). Use `libra add -p` or `libra add <pathspec>`.

## Issue #477 notes

remaining unsupported interactive options fail with `LBR-UNSUPPORTED-001`
