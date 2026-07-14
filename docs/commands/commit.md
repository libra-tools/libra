# `libra commit`

Create a new commit from staged changes.

**Alias:** `ci`

## Synopsis

```
libra commit [OPTIONS] -m <MESSAGE>
libra commit [OPTIONS] -F <FILE>
libra commit [OPTIONS] -C <COMMIT>
libra commit [OPTIONS] -c <COMMIT>
libra commit [OPTIONS] -t <FILE>
libra commit [OPTIONS] --date <DATE> -m <MESSAGE>
libra commit [OPTIONS] --fixup <COMMIT>
libra commit [OPTIONS] --squash <COMMIT>
libra commit --amend [--no-edit]
```

## Description

`libra commit` creates a new commit from staged changes, builds tree and commit objects,
validates messages (including optional conventional commit format and GPG signing via vault),
and updates HEAD and refs.

The command reads the index to determine which files are staged, constructs a tree object
hierarchy matching the staged content, creates a commit object with the provided message and
author/committer metadata, and advances the current branch ref. When vault signing is enabled,
the commit is automatically GPG-signed. The `.libra/hooks` commit lifecycle runs unless
bypassed with `--no-verify`.

Before computing staged changes or writing tree/commit objects, `commit` validates stage-0
index entries for missing or mistyped blob/tree objects. A corrupt index entry fails closed
with `LBR-REPO-002` and leaves `HEAD` unchanged.

Author identity comes from `--author`, then `GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`, then
configured `user.name`/`user.email`; committer identity comes from
`GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`, then config. Git environment variables
take priority over config unless `user.useConfigOnly=true`, in which case environment
identity is ignored. `LIBRA_COMMITTER_NAME`/`LIBRA_COMMITTER_EMAIL` remain as
lower-priority fallbacks for older automation.

## Options

### `-m, --message <MESSAGE>`

Use the given message as the commit message. When omitted (and no `-F`/`-C`/`--no-edit`
source is given), the editor is opened to compose the message.

```bash
libra commit -m "Add new feature"
```

### `-e, --edit`

Open the editor to edit the final message even when `-m`/`-F`/`-C` is given (the supplied
message is the initial buffer). Conflicts with `--no-edit`.

```bash
libra commit -e -m "Draft message"
```

### `-t, --template <FILE>`

Use the contents of `FILE` as the initial commit message. With the editor open (the default
when no other message source is given), `FILE` seeds the editor buffer; with `--no-edit` it is
used directly. When the `-t` flag is unset, the `commit.template` config (a file path, with a
leading `~/` expanded to `$HOME`) is consulted. The template is **ignored** when a message
source (`-m`/`-F`/`-C`/`-c`/`--fixup`/`--squash`) is given — that source wins and the template
file is not even read. As in Git, if the editor leaves the template unchanged the commit is
aborted ("you did not edit the message"); `--no-edit` bypasses that check.

```bash
libra commit -t .libra/commit-template.txt
```

### `-v, --verbose`

Show the staged diff in the editor template (below a scissors line, stripped on save) or, when
no editor is opened, on stderr. The diff never enters the commit message.

Setting the `commit.verbose` config to true makes `-v` the default (an explicit `-v` on the
command line still forces it on). The value is a Git `bool-or-int`, so `commit.verbose=2` is
accepted and enables verbose — but Libra's `-v` is on/off only: there is no `-vv` verbosity
level (no unstaged-diff rendering) and no `--no-verbose` to force it off for a single commit.

```bash
libra commit -v
libra config commit.verbose true   # make -v the default
```

### `--no-edit`

Reuse the existing message (the `--amend` parent's, or the one from `-m`/`-F`) without opening
the editor. Conflicts with `--edit`.

```bash
libra commit --amend --no-edit
libra commit --no-edit -m "msg"
```

### `-F, --file <FILE>`

Read the commit message from the given file. Mutually exclusive with `-m` when `--no-edit`
is not in use.

```bash
libra commit -F message.txt
```

### `--amend`

Replace the tip of the current branch by creating a new commit. The new commit has the same
parent(s) as the replaced commit. Cannot amend merge commits (commits with multiple parents).
When the index tree and message are unchanged, `--amend --no-edit` still rewrites the commit
and refreshes committer metadata; it never reports a successful amend while leaving `HEAD`
unchanged.

```bash
libra commit --amend
libra commit --amend -m "Updated message"
```

### `--no-edit`

When used with `--amend`, reuse the message from the original commit without prompting for
changes. A clean amend still creates a replacement commit with a refreshed committer date.
Conflicts with `-m` and `-F`.

```bash
libra commit --amend --no-edit
```

### `--conventional`

Validate the commit message against the Conventional Commits specification
(https://www.conventionalcommits.org). The message must match the pattern
`<type>[optional scope]: <description>`. Fails with an error if validation fails.

```bash
libra commit -m "feat: add login" --conventional
libra commit -m "fix(auth): handle expired tokens" --conventional
```

### `-a, --all`

Automatically stage tracked files that have been modified or deleted before committing.
Equivalent to running `libra add -u` before `libra commit`. Does not add new untracked files.

```bash
libra commit -a -m "Fix typo"
```

### `-s, --signoff`

Add a `Signed-off-by` trailer at the end of the commit message, using the committer's
identity.

```bash
libra commit -s -m "Add feature"
```

### `--allow-empty`

Allow creating a commit with no changes (empty diff from parent). Useful for triggering CI
or marking milestones.

```bash
libra commit --allow-empty -m "Trigger CI"
```

### `--disable-pre`

Skip only `pre-commit`. Message hooks and post-operation hooks still run.

```bash
libra commit --disable-pre -m "Quick fix"
```

### `--no-verify`

Skip every `.libra/hooks` lifecycle hook and commit-message validation for this invocation.

```bash
libra commit --no-verify -m "WIP: work in progress"
```

### Repository hooks

The order is `pre-commit` → `prepare-commit-msg` → editor/trailers → `commit-msg` →
commit/ref update → `post-commit` → `post-rewrite` for amend. The two message hooks may
edit `.libra/COMMIT_EDITMSG`; blocking hook failures leave history unchanged, while the
post hooks are advisory after success. All hooks use the required workspace-only,
network-denied sandbox. See [Repository hooks](repository-hooks.md) for arguments,
resolution, output behavior, and escape valves.

### `--author <AUTHOR>`

Override the commit author. Must use the standard `A U Thor <author@example.com>` format.

```bash
libra commit --author "Jane Doe <jane@example.com>" -m "Patch"
```

### `--date <DATE>`

Set the author date for the new commit. The committer date still uses the
current time unless `GIT_COMMITTER_DATE` is set. Accepted formats include Git
raw dates (`<unix> <+HHMM|-HHMM>`), RFC 3339, `YYYY-MM-DD HH:MM:SS +HHMM`,
`YYYY-MM-DD`, relative dates such as `2 days ago`, and Unix timestamps.
`--date` takes precedence over `GIT_AUTHOR_DATE`.

```bash
libra commit --date "1700000000 +0000" -m "Backdated author timestamp"
```

### Identity And Date Environment

`GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, and `GIT_AUTHOR_DATE` set the author
identity/date. `GIT_COMMITTER_NAME`, `GIT_COMMITTER_EMAIL`, and
`GIT_COMMITTER_DATE` set the committer identity/date. If a Git committer
identity field is absent, Libra falls back through the matching author field,
`EMAIL` for the email, then `LIBRA_COMMITTER_*`; config is used after those
environment fallbacks. `user.useConfigOnly=true` disables all identity
environment fallbacks, but explicit `--author` still applies.

### `--cleanup <MODE>`

Clean up the commit message before committing. Accepted values: `strip` (default, removes
commentary lines and trims whitespace), `whitespace` (only trims whitespace), `verbatim` (no
cleanup), `scissors` (whitespace cleanup plus truncation at the scissors line — the truncation
only applies when the message is edited; on a non-editor `-m`/`-F` commit it behaves like
`whitespace`, with no truncation), `default` (strip when the message is edited, otherwise
whitespace).

When `--cleanup` is not given, the `commit.cleanup` config value is used as the default (config
cascade: local repo, then global); the CLI flag always takes precedence.

```bash
libra commit --cleanup=strip -m "feat: add login"
libra config commit.cleanup whitespace   # default cleanup when --cleanup is omitted
```

### `--dry-run`

Do not actually create the commit. Show the commit summary that would be produced.
The preview does not run repository hooks or open the commit-message editor, so
no message is required and subprocesses cannot observe or mutate a live index while
`-a` is being previewed. `-v` still prints the staged preview diff directly.

```bash
libra commit --dry-run -m "Draft commit"
```

### `--porcelain`

Print the working-tree status in machine-readable porcelain v1 format (the same as
`libra status --porcelain`: staged changes in column 1, unstaged in column 2, untracked
as `??`, untracked directories collapsed) instead of the human commit summary, mirroring
`git commit --porcelain`. Like Git, `--porcelain` **implies `--dry-run`**: it prints the
would-be-committed state and does **not** create the commit (and leaves the index
untouched, even with `-a`, which is auto-staged only for the preview). Inert under
`--json` (the JSON envelope is emitted instead).

```bash
libra commit --porcelain
```

### `--status` / `--no-status`

The commit-message editor template includes the working-tree status as
`#`-commented lines by default. `commit.status=false` disables it; `--status`
and `--no-status` explicitly override that config (last flag wins). Because the
lines are comments, the message
cleanup strips them — they are informational only and never enter the final
commit message. This has no effect when no editor is opened (e.g. with `-m`).
The status is also omitted under cleanup modes that keep comment lines
(`--cleanup=verbatim`, `--cleanup=whitespace`, and `--cleanup=scissors` — explicit
scissors keeps `#` lines above the marker), so it can never leak into the message;
it is seeded only when an editor opens and the effective cleanup strips comments
(`strip`/`default`). `-v` only truncates the appended diff — it does not force a
strip — so the status stays omitted under those modes even with `-v`.

`commit.status` uses the strict local -> global -> system cascade and accepts a
Git boolean (including numeric forms such as `0k` and `2`). When an editor status
template can actually be generated, an invalid value fails with `LBR-CLI-002`,
and an unreadable local/global config store fails with `LBR-IO-001`, before `-a`,
hooks, object writes, or history updates. `-m`, dry-run/porcelain, JSON, and
non-comment-stripping cleanup bypass the key because no template can exist. An explicit
`--status` or `--no-status` bypasses the matching config read. When the status
section is enabled, failures from its `status.*` config, collection, or rendering
also preserve their stable error code and abort before the pre-commit hook,
editor, commit/tree object write, or ref update; `--no-status` bypasses that
template path. Consequently, `--status` with an unreadable shared config store
reports the first required `status.*` key rather than `commit.status`, proving
the explicit toggle skipped the latter read. For `--dry-run -a`, the entire preview uses a task-local isolated
temporary index; the live index is never replaced, and temporary auto-stage
blobs, LFS backups, and tree objects are not persisted. Non-verbose previews
hash regular files in a 64 KiB stream and retain no payload. Auto-stage detects
tracked symlinks without following them (including dangling links and paths
matched by an LFS attribute), hashes/stores the link-target bytes, and preserves
mode `120000` in real commits and previews. Verbose previews
reserve every unique blob on the changed HEAD, already-staged, and newly auto-staged sides before it
is loaded, with limits of 32 MiB per blob, 64 MiB aggregate charged scratch, and
4,096 objects. The complete changed-object count is rejected before any storage
size batch, loose decode, or pack-index scan. Auto-stage reserves a provisional byte/count slot before reading
the worktree payload, then reconciles it by object ID, so neither staging nor
hash deduplication can bypass those limits. Scratch lives under the common
repository storage at `.libra/tmp/commit-preview` (shared by linked worktrees),
is capped at 256 MiB across concurrent runs,
and scavenges at most 32 inactive runs older than 24 hours per bounded startup
scan. A run with missing or unreadable reservation metadata fails closed instead
of being charged as zero bytes, including while its run lock is still active.
Limit failures preserve the live index/object store and advise rerunning
without verbose output. A changed blob is likewise refused before loading when
its backend cannot perform a bounded local preflight (for example, a remote-only
blob or a packed blob whose index is missing); the preview never rebuilds pack
indexes. Actual preview reads use an explicit local-only bounded-load API rather
than relying on task-local state inside the storage runtime; this path reads
existing pack indexes only and never rebuilds unrelated missing indexes. Loose and non-delta
packed objects reject an over-limit declared length before decoding the payload;
loose objects otherwise stream to the declared boundary and reject size
mismatches or bytes after the zlib stream. An oversized delta instruction declaration
is rejected before its base chain is visited; accepted packed deltas validate and charge the
complete base/instruction/result chain with bounded depth and fail closed on
malformed instructions; a batch enumerates packs once and opens each existing
index once. The batch receives the remaining aggregate budget, uses the same
4 KiB minimum per-object charge as the cache, and stops probing later pack
payloads immediately after the budget is crossed. The subsequent bounded pack
read uses an uncached delta decoder and moves its final payload, avoiding the
process-wide 200 MiB pack cache and extra full-payload clones. Rerun without
verbose output to avoid an unbounded preview load.
Dry-run and porcelain previews also skip hooks, editors,
rerere updates, and `post_commit` automation because no commit occurred.
When `status.showStash=true`, an unreadable/non-file stash ref (including a
symlink) or an unreadable/corrupt stash log aborts status template collection with `LBR-IO-001` before hooks, the
editor, or ref writes. The same fail-closed behavior applies to fresh
`status --cached` output.
Auto-stage file/LFS reads fail as
`LBR-IO-001`; preview, LFS-backup, and object writes fail as `LBR-IO-002`
instead of panicking. For a real `-a`, blob/LFS
materialization uses a streamed temporary snapshot, derives the pointer from
those exact bytes, and atomically replaces any stale or truncated backup. With
`--sync-data`, newly created shard ancestors, the temporary payload, and both
the staging and destination directories are synced around that atomic
replacement; Windows uses a write-through atomic replace because it has no
portable directory-fsync equivalent. The
objects and staged index intentionally remain if later status
collection aborts, matching the existing auto-stage-on-abort behavior.

```bash
libra commit                   # editor template includes commented status
libra config commit.status false
libra commit --status          # override commit.status=false for this commit
libra commit --no-status       # omit status for this commit
```

### `--no-gpg-sign`

Force an unsigned commit: skip Libra's vault GPG signing for this commit,
matching `git commit --no-gpg-sign`. Vault signing runs when `vault.signing=true`
(the `libra init` default) and a vault unseal key is available. The Git-compatible
`commit.gpgSign=true|false` default overrides `vault.signing`: `true` force-signs
with the repository vault key and `false` disables signing. `--no-gpg-sign` has
highest precedence and suppresses either configuration. Git's positive
`-S`/`--gpg-sign` is not exposed.

```bash
libra commit --no-gpg-sign -m "message"
```

### `--fixup <COMMIT>`

Create a fixup commit whose message is `fixup! <target subject>`.

```bash
libra commit --fixup HEAD~1
```

### `--squash <COMMIT>`

Create a squash commit whose message is `squash! <target subject>`.

```bash
libra commit --squash abc1234
```

### `-C <COMMIT>`, `--reuse-message <COMMIT>`

Reuse the commit message and author metadata (name, email, author date, and
timezone) from the specified commit. The new commit still receives the current
committer identity/date, or `GIT_COMMITTER_*` overrides when set.

```bash
libra commit -C HEAD~1
```

### `-c <COMMIT>`, `--reedit-message <COMMIT>`

Reuse the commit message and author metadata from the specified commit, then
open the editor to edit the message. If no editor is configured, the command
uses the reused message unchanged.

```bash
libra commit -c HEAD~1
```

### `--trailer <TRAILER>`

Add a trailer line to the commit message. Can be specified multiple times.

```bash
libra commit -m "Add feature" --trailer "Reviewed-by: Jane Doe"
```

### `--reset-author`

When amending, reset the author to the current author identity and date instead
of preserving the amended commit's original author. Current author identity/date
honors `GIT_AUTHOR_*` and `--date` as described above. For new non-amend commits
this is already the default.

```bash
libra commit --amend --reset-author --no-edit
```

## Common Commands

```bash
libra commit -m "Add new feature"
libra commit -m "feat: add login" --conventional
libra commit --amend
libra commit --amend --no-edit
libra commit -a -m "Fix typo"
libra commit -F message.txt
libra commit --date "2026-07-09 10:00:00 +0800" -m "Backdated author date"
libra commit -s -m "Add feature"
libra commit --allow-empty -m "Trigger CI"
libra commit --dry-run -m "Draft commit"
libra commit --fixup HEAD~1
libra commit -C HEAD~1
libra commit -m "Add feature" --trailer "Reviewed-by: Jane Doe"
libra commit --json -m "Add feature"
```

## Human Output

Default human mode writes the commit summary to `stdout`.

Normal commit:

```text
[main abc1234] Add new feature
 2 files changed (new: 1, modified: 1, deleted: 0)
```

Root commit:

```text
[main (root-commit) abc1234] Initial commit
 1 file changed (new: 1, modified: 0, deleted: 0)
```

`--quiet` suppresses all `stdout` output.

## Structured Output

`libra commit` supports the global `--json` and `--machine` flags.

- `--json` writes one success envelope to `stdout`
- `--machine` writes the same schema as compact single-line JSON
- both suppress hook stdout/stderr (piped instead of inherited)
- `stderr` stays clean on success

Example:

```json
{
  "ok": true,
  "command": "commit",
  "data": {
    "head": "main",
    "branch": "main",
    "commit": "abc1234def5678901234567890abcdef12345678",
    "short_id": "abc1234",
    "subject": "Add new feature",
    "root_commit": false,
    "amend": false,
    "files_changed": {
      "total": 2,
      "new": 1,
      "modified": 1,
      "deleted": 0
    },
    "signoff": false,
    "conventional": null,
    "signed": true
  }
}
```

Root commit:

```json
{
  "ok": true,
  "command": "commit",
  "data": {
    "head": "main",
    "branch": "main",
    "commit": "abc1234def5678901234567890abcdef12345678",
    "short_id": "abc1234",
    "subject": "Initial commit",
    "root_commit": true,
    "amend": false,
    "files_changed": {
      "total": 1,
      "new": 1,
      "modified": 0,
      "deleted": 0
    },
    "signoff": false,
    "conventional": null,
    "signed": true
  }
}
```

Amend:

```json
{
  "ok": true,
  "command": "commit",
  "data": {
    "head": "main",
    "branch": "main",
    "commit": "def5678abc1234901234567890abcdef12345678",
    "short_id": "def5678",
    "subject": "Amended message",
    "root_commit": false,
    "amend": true,
    "files_changed": {
      "total": 1,
      "new": 0,
      "modified": 1,
      "deleted": 0
    },
    "signoff": false,
    "conventional": null,
    "signed": true
  }
}
```

### Schema Notes

- `head` is the branch name or `"detached"` for backward compatibility
- `branch` is `null` when HEAD is detached; `Some(name)` otherwise
- `conventional` is `true` when `--conventional` was passed and validation succeeded; `null` when not requested
- `signed` is `true` when vault signing is enabled and the commit was GPG-signed
- `signoff` is `true` when `-s` / `--signoff` appended a `Signed-off-by` trailer

## Design Rationale

### `--conventional` flag for conventional commits

Git has no built-in support for commit message format validation; teams rely on external
tools like commitlint, husky, or CI checks to enforce Conventional Commits. Libra provides
first-class `--conventional` validation directly in the commit command. This serves two
purposes: (1) it gives immediate feedback at commit time rather than delayed feedback in CI,
and (2) it enables AI agents (which generate commit messages programmatically) to validate
their output without external tooling. The flag is opt-in rather than mandatory, respecting
teams that use different commit message conventions.

### Vault signing by default instead of manual GPG setup

In Git, commit signing requires configuring `user.signingkey`, `gpg.program`, and
`commit.gpgSign` -- a multi-step process that most developers skip. Libra's vault
automatically generates and manages a PGP signing key at repository initialization, so
commits are signed by default with zero configuration. This makes signed commits the norm
rather than the exception, improving supply-chain security for the entire ecosystem. Users
can use `commit.gpgSign` for a Git-compatible scoped default; when it is unset,
`vault.signing` remains the Libra default.

### `--disable-pre` flag

The `--disable-pre` flag skips only the pre-commit hook while still running the message and
post-operation hooks. This is more granular than `--no-verify`, which skips all repository hooks. The use case
is when a developer trusts the commit message validation (e.g., conventional commit checks
via commit-msg hook) but wants to skip expensive pre-commit checks (e.g., full test suite,
large linter runs) during rapid iteration. This separation of concerns is intentional: the
commit message is part of the permanent record and should be validated even during quick
iterations.

### `--no-verify` to skip hooks

For cases where all hook validation needs to be bypassed (e.g., emergency fixes, WIP commits),
`--no-verify` skips every `.libra/hooks` lifecycle hook plus commit-message validation.
The flag name follows Git's convention, while Libra's additional post-hook bypass is
documented explicitly because post hooks are otherwise advisory.

## Parameter Comparison: Libra vs Git vs jj

| Parameter / Flag | Git | jj | Libra |
|---|---|---|---|
| Commit with message | `git commit -m "msg"` | `jj commit -m "msg"` | `libra commit -m "msg"` |
| Commit from file | `git commit -F file` | N/A | `libra commit -F file` |
| Amend last commit | `git commit --amend` | `jj describe` (edits working copy commit) | `libra commit --amend` |
| Amend without edit | `git commit --amend --no-edit` | `jj describe --no-edit` | `libra commit --amend --no-edit` |
| Auto-stage tracked | `git commit -a` | N/A (automatic tracking) | `libra commit -a` |
| Allow empty commit | `git commit --allow-empty` | `jj commit --allow-empty` | `libra commit --allow-empty` |
| Signoff trailer | `git commit -s` / `--signoff` | N/A | `libra commit -s` / `--signoff` |
| GPG sign commit | `git commit -S` (manual GPG) | N/A (no signing) | Automatic (vault-backed) |
| Override author | `git commit --author="..."` | N/A | `libra commit --author="..."` |
| Author date | `git commit --date=<date>` | N/A | `libra commit --date <date>` |
| Conventional check | External tool (commitlint) | N/A | `libra commit --conventional` |
| Skip pre-commit only | N/A | N/A | `libra commit --disable-pre` |
| Skip all hooks | `git commit --no-verify` | N/A | `libra commit --no-verify` |
| Fixup commit | `git commit --fixup=<commit>` | N/A | `libra commit --fixup=<commit>` |
| Squash commit | `git commit --squash=<commit>` | `jj squash` | `libra commit --squash=<commit>` |
| Reuse message + author | `git commit -C/-c <commit>` | N/A | `libra commit -C/-c <commit>` |
| Interactive message | `git commit` (opens editor) | `jj commit` (opens editor) | `libra commit` / `libra commit -e` (opens editor) |
| Verbose diff in editor | `git commit -v` | N/A | `libra commit -v` |
| Status in editor template | default on; `commit.status` / `--[no-]status` | N/A | default on; `commit.status` / `--[no-]status` |
| Reset author date | `git commit --reset-author` | N/A | `libra commit --reset-author` |
| Cleanup mode | `git commit --cleanup=<mode>` | N/A | `libra commit --cleanup=<mode>` |
| Trailer | `git commit --trailer="..."` | N/A | `libra commit --trailer="..."` |
| Structured JSON output | N/A | N/A | `--json` / `--machine` |
| Error hints | Minimal | Minimal | Every error type has an actionable hint |

## Error Handling

Every `CommitError` variant maps to an explicit `StableErrorCode`.

| Scenario | Error Code | Exit | Hint |
|----------|-----------|------|------|
| Index corrupted | `LBR-REPO-002` | 128 | "the index file may be corrupted; try 'libra status' to verify" |
| Index object missing or wrong type | `LBR-REPO-002` | 128 | "run 'libra fsck' to inspect missing or mistyped objects" |
| Failed to save index | `LBR-IO-002` | 128 | -- |
| Nothing to commit (clean) | `LBR-REPO-003` | 128 | "use 'libra add' to stage changes" |
| Nothing to commit (no tracked) | `LBR-REPO-003` | 128 | "create/copy files and use 'libra add' to track" |
| Author identity missing | `LBR-AUTH-001` | 128 | "run 'libra config user.name ...' and 'libra config user.email ...'" |
| No commit to amend | `LBR-REPO-003` | 128 | "create a commit before using --amend" |
| Amend merge commit | `LBR-REPO-003` | 128 | "create a new commit instead of amending a merge commit" |
| Invalid author format | `LBR-CLI-002` | 129 | "expected format: 'Name <email>'" |
| Invalid author/committer date | `LBR-CLI-002` | 129 | Supported date formats |
| Invalid `commit.status` | `LBR-CLI-002` | 129 | Fix the offending config value |
| `commit.status` config unreadable | `LBR-IO-001` | 128 | Repair the local/global config store |
| Message file unreadable | `LBR-IO-001` | 128 | -- |
| Empty commit message | `LBR-REPO-003` | 128 | "use -m to provide a commit message" |
| Tree creation failed | `LBR-INTERNAL-001` | 128 | Issues URL |
| Object storage failed | `LBR-IO-002` | 128 | -- |
| Parent commit missing | `LBR-REPO-002` | 128 | "the parent commit is missing or corrupted" |
| HEAD update failed | `LBR-IO-002` | 128 | -- |
| Blocking repository hook failed | `LBR-REPO-003` | 128 | "use --no-verify to bypass repository hooks" |
| Conventional commit invalid | `LBR-CLI-002` | 129 | "see https://www.conventionalcommits.org for format rules" |
| Vault signing failed | `LBR-AUTH-001` | 128 | "check vault configuration with 'libra config --list'" |
| Auto-stage source read/hash failed | `LBR-IO-001` | 128 | Check the named working-tree file |
| Auto-stage preview/object/LFS write failed | `LBR-IO-002` | 128 | Check space and permissions for the named target |
| Staged changes computation | `LBR-REPO-002` | 128 | "failed to compute staged changes" |

## Compatibility Notes

- `libra commit` opens the editor to compose the message when no `-m`/`-F`/`-C` source is given (and `--no-edit` is absent), and `-e`/`--edit` always opens it. The editor is resolved as `$GIT_EDITOR` → `core.editor` → `$VISUAL` → `$EDITOR` → `vi`. An explicitly configured editor runs even without a TTY; the `vi` fallback requires an interactive terminal. Without a TTY and without a configured editor, a commit needing a message aborts (it never hangs).
- `-v`/`--verbose` appends the staged diff to the editor template (below a `# ----- >8 -----` scissors line); the diff is stripped on save and never enters the commit message. When no editor is opened, `-v` prints the staged diff to stderr.
- jj does not have a traditional `commit` command with staging; `jj commit` finalizes the working copy commit
- `--fixup` and `--squash` are supported (autosquash markers); `--cleanup=<mode>` controls comment/scissors stripping
- Vault signing replaces the external keyring; `commit.gpgSign` is honored while `user.signingkey` remains vault-managed
