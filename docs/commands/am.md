# `libra am`

Apply one or more plain-text `format-patch` mail files as commits. The files
are processed in the order given, preserving each mail's subject/body, author,
and `Date:` metadata while using the current Libra identity as committer.

## Synopsis

```text
libra am [-3] <patch|mbox|-> ...
libra am --continue
libra am --skip
libra am --abort
```

## Behavior

A new series requires a local branch with an existing commit and no staged or
tracked working-tree changes. Unrelated untracked files are preserved, but any
existing non-index path that a mail would touch—including an ignored path—is
rejected before sequencer state is saved. The aggregate mail input is limited
to 64 MiB and 10,000 mails.

Each input may be a single mail or an mbox: a file (or stdin) whose first
line is an mbox `From ` envelope line is split into its messages and
applied in order. The envelope test is ctime-shaped (an `hh:mm:ss` time
token followed by a 4-digit year, trailing timezone tokens allowed), so
commit-message prose like `From my reading of RFC 9110` never splits a
mail, and body content — including `>From ` quoting — is preserved
byte-for-byte like git's default (mboxo) reading. `-` reads one mail
or mbox from standard input (allowed at most once); the full mail content
is persisted in the sequencer state, so `--continue` / `--skip` /
`--abort` work for stdin-sourced series exactly like file-sourced ones.
Multi-message sources are position-labelled `<source>#<n>` in output and
state.

Beyond plain text diffs, mails may carry git extended sections: `GIT
binary patch` payloads (both `literal` and `delta`, decoded from base85 +
zlib with bounded inflation), `rename from`/`rename to` (with or without
content hunks — the source deletion and the destination are staged in the
same commit), `copy from`/`copy to`, and mode-only `old mode`/`new mode`
changes (the executable bit is applied to the worktree and staged
directly). Every extended target passes the same path-safety checks.

The mail parser accepts UTF-8 messages with `7bit`, `8bit`, `binary`,
quoted-printable, or base64 transfer encoding — single-part `text/plain`,
or MIME multipart containers (`multipart/mixed`/`alternative`, nested up
to a bounded depth): parts are split on the declared boundary, every
supported text part (`text/plain`, `text/x-patch`, `text/x-diff`) is
decoded with its own transfer encoding and concatenated in order, HTML
alternatives and binary attachments are skipped, and a multipart mail
with no supported text part fails closed. `format-patch --attach` output
therefore applies directly. It reads `From:`,
`Date:`, and `Subject:`, removes a leading `[PATCH ...]` subject marker, honors
the standard in-body `From:` override, and extracts the text `diff --git`
section after the `---` separator. UTF-8/US-ASCII RFC 2047 `B` and `Q` encoded
words are decoded.

Every target is checked against absolute paths, empty/`.`/`..` components, NUL
bytes, `.libra/`, and existing symlink path components. All files in one mail are test-applied before
the first write. File replacements use atomic rename and content patches retain
the existing permission bits.

Sequencer state is saved before worktree writes. Each successful commit moves
the branch, writes its reflog, and advances or clears the `am` position in one
SQLite transaction. Resume and skip reject a branch whose tip moved outside
the sequencer. If interruption occurs after state is saved but before the
current mail writes anything—including between two commits—`--continue`
retries that mail. `--abort` resets the original branch tip, index, and tracked
worktree, and also removes a new-file target left by an interruption before it
was staged.

## Hooks

The applypatch hook family runs from `.libra/hooks` through the
sandboxed repository-hook runner (same contract as the commit hooks;
`LIBRA_NO_HOOKS=1` bypasses):

- `applypatch-msg <msg-file>` — before any worktree write. The proposed
  commit message is written to the worktree's `COMMIT_EDITMSG` (the one
  writable hook file); the hook may edit it in place, and a non-zero
  exit refuses the mail with the series left resumable.
- `pre-applypatch` — after the worktree write and staging, before the
  commit; a non-zero exit pauses the series with the changes staged.
  It also gates the resolved `--continue` commit (Git's `--resolved`
  semantics); `applypatch-msg` does not re-run there.
- `post-applypatch` — after the commit; advisory (failures warn, never
  fail the applied mail).

## Conflict recovery

With `-3`/`--3way`, a text patch that does not apply falls back to a
three-way merge: the base is the `index` header's old blob resolved from
the local object store (abbreviated ids resolve only when unambiguous),
theirs is the patch applied to that base, ours is the current content. A
clean merge applies silently; a conflicting one writes `<<<<<<<` markers
into the worktree and pauses the series (resolve, stage, `--continue`).
A base that is not locally present keeps the plain refusal — the
fallback never fabricates content. The `-3` choice persists in the saved
series state, so resumes keep the same semantics.

Without `-3`, when a patch does not apply the command leaves the current
branch tip unchanged and keeps the series resumable:

1. resolve the affected paths manually;
2. stage only paths named by the current patch with `libra add`;
3. run `libra am --continue`.

Use `--skip` to discard the current patch and continue with the next one, or
`--abort` to discard the entire series and restore the pre-`am` state.

## Options

| Option | Meaning |
|---|---|
| `--continue` | Commit the fully staged resolution and continue. Unstaged current-patch paths, unrelated tracked changes, unresolved index entries, an empty resolution, or staged unrelated paths are rejected. A pristine recovery state retries the current mail. |
| `--skip` | Reset the current patch and continue with the remaining mails. |
| `--abort` | Restore the original branch tip, index, and tracked worktree and clear the sequencer. |
| `--json` / `--machine` | Emit the action, applied source files/subjects/commit IDs, and optional restored HEAD in the standard envelope. |

## Examples

```bash
# Generate and replay a series
libra format-patch -o outgoing origin/main..HEAD
libra switch target
libra am outgoing/0001-*.patch outgoing/0002-*.patch

# Pipe a whole series as one mbox
libra format-patch --stdout origin/main..HEAD | libra am -

# Resolve a stopped patch
$EDITOR src/lib.rs
libra add src/lib.rs
libra am --continue

# Cancel the complete series
libra am --abort
```

## Current limitations

This is not yet full Git `am` parity. It does not expose Git's wider
flag set (`--signoff`, `--keep`, `--scissors`, and others). The shared parser is also
available as the standalone [`libra mailinfo`](mailinfo.md) command.
