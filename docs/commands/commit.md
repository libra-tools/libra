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
the commit is automatically GPG-signed. Pre-commit and commit-msg hooks are executed unless
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

Skip the pre-commit hook only. The commit-msg hook still runs.

```bash
libra commit --disable-pre -m "Quick fix"
```

### `--no-verify`

Skip all pre-commit and commit-msg hooks/validations. Aligns with Git's `--no-verify`
behavior.

```bash
libra commit --no-verify -m "WIP: work in progress"
```

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

`--status` seeds the working-tree status, as `#`-commented lines, into the
commit-message editor template (Git shows this by default; Libra defaults to
omitting it, so `--status` opts in). Because the lines are comments, the message
cleanup strips them — they are informational only and never enter the final
commit message. This has no effect when no editor is opened (e.g. with `-m`).
The status is also omitted under cleanup modes that keep comment lines
(`--cleanup=verbatim`, `--cleanup=whitespace`, and `--cleanup=scissors` — explicit
scissors keeps `#` lines above the marker), so it can never leak into the message;
it is seeded only when an editor opens and the effective cleanup strips comments
(`strip`/`default`). `-v` only truncates the appended diff — it does not force a
strip — so the status stays omitted under those modes even with `-v`. `--no-status` (the
default) omits the status section. The two are a last-one-wins toggle.

```bash
libra commit --status          # opens the editor with the status commented in
libra commit --no-status -m "message"
```

### `--no-gpg-sign`

Force an unsigned commit: skip Libra's vault GPG signing for this commit,
matching `git commit --no-gpg-sign`. Vault signing runs when `vault.signing=true`
(the `libra init` default) and a vault unseal key is available; `--no-gpg-sign`
suppresses it regardless, so it is a no-op only when signing would not have
happened anyway. Git's positive `-S`/`--gpg-sign` is not exposed; Libra's commit
signing is driven by the `vault.signing` config instead.

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
`commit.gpgsign` -- a multi-step process that most developers skip. Libra's vault
automatically generates and manages a PGP signing key at repository initialization, so
commits are signed by default with zero configuration. This makes signed commits the norm
rather than the exception, improving supply-chain security for the entire ecosystem. Users
who do not want signing can disable it with `libra config vault.signing false`.

### `--disable-pre` flag

The `--disable-pre` flag skips only the pre-commit hook while still running the commit-msg
hook. This is more granular than Git's `--no-verify`, which skips all hooks. The use case
is when a developer trusts the commit message validation (e.g., conventional commit checks
via commit-msg hook) but wants to skip expensive pre-commit checks (e.g., full test suite,
large linter runs) during rapid iteration. This separation of concerns is intentional: the
commit message is part of the permanent record and should be validated even during quick
iterations.

### `--no-verify` to skip hooks

For cases where all hook validation needs to be bypassed (e.g., emergency fixes, WIP commits),
`--no-verify` skips both pre-commit and commit-msg hooks. This aligns with Git's behavior
and naming convention. The flag name was chosen for Git compatibility so that developers
switching from Git do not need to learn a new flag name.

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
| Message file unreadable | `LBR-IO-001` | 128 | -- |
| Empty commit message | `LBR-REPO-003` | 128 | "use -m to provide a commit message" |
| Tree creation failed | `LBR-INTERNAL-001` | 128 | Issues URL |
| Object storage failed | `LBR-IO-002` | 128 | -- |
| Parent commit missing | `LBR-REPO-002` | 128 | "the parent commit is missing or corrupted" |
| HEAD update failed | `LBR-IO-002` | 128 | -- |
| Pre-commit hook failed | `LBR-REPO-003` | 128 | "use --no-verify to bypass the hook" |
| Conventional commit invalid | `LBR-CLI-002` | 129 | "see https://www.conventionalcommits.org for format rules" |
| Vault signing failed | `LBR-AUTH-001` | 128 | "check vault configuration with 'libra config --list'" |
| Auto-stage failed | `LBR-IO-001` | 128 | -- |
| Staged changes computation | `LBR-REPO-002` | 128 | "failed to compute staged changes" |

## Compatibility Notes

- `libra commit` opens the editor to compose the message when no `-m`/`-F`/`-C` source is given (and `--no-edit` is absent), and `-e`/`--edit` always opens it. The editor is resolved as `$GIT_EDITOR` → `core.editor` → `$VISUAL` → `$EDITOR` → `vi`. An explicitly configured editor runs even without a TTY; the `vi` fallback requires an interactive terminal. Without a TTY and without a configured editor, a commit needing a message aborts (it never hangs).
- `-v`/`--verbose` appends the staged diff to the editor template (below a `# ----- >8 -----` scissors line); the diff is stripped on save and never enters the commit message. When no editor is opened, `-v` prints the staged diff to stderr.
- jj does not have a traditional `commit` command with staging; `jj commit` finalizes the working copy commit
- `--fixup` and `--squash` are supported (autosquash markers); `--cleanup=<mode>` controls comment/scissors stripping
- Vault signing replaces Git's `commit.gpgsign` and `user.signingkey` configuration
