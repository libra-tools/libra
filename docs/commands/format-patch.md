# `libra format-patch`

Generate mbox-formatted email patch files from commits.

## Synopsis

```bash
libra format-patch [OPTIONS] [revision-range]
```

## Description

`libra format-patch` walks a revision range (`A..B` or a single commit treated
as `<commit>..HEAD`), while `-1 [commit]` selects exactly one commit and
`--root [commit]` includes the root and every reachable non-merge ancestor. It
produces one patch file per non-merge commit (named with the `--suffix`,
default `.patch`, unless `--numbered-files` is set, which uses bare sequence
numbers). Filenames include a zero-padded sequence prefix regardless of `-n`
or `-N`. Each patch is formatted as an mbox message with RFC 2822 headers, a
plain-text diffstat, and a unified diff. The output is compatible with `git am`.

Merge commits are skipped by default. When the revision range resolves to zero
commits, the command exits successfully without generating patches. Plain output
is empty; with `--json`, the payload contains `"patches": []`. A repository
with no commits or an unresolvable revision still fails. `--ignore-if-in-upstream`
also succeeds with an empty series when every commit is already upstream.

## Options

The `format-patch` options below control the generated mail headers, patch
body, and series metadata.

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `[revision-range]` | | `A..B` range or single commit; single commit means `<commit>..HEAD` | `HEAD` |
| `-1` | `-1` | Generate only the named commit, or `HEAD` when no revision is given | false |
| `--root` | | Include the root commit and all reachable non-merge ancestors | false |
| `--output-directory <DIR>` | `-o` | Write patch files into `DIR` | current directory |
| `--stdout` | | Print all patches to stdout | false |
| `--numbered` | `-n` | Add the `[PATCH n/m]` counter to commit patch Subject lines | false |
| `--no-numbered` | `-N` | Do not add the `[PATCH n/m]` counter to commit patch Subject lines | off |
| `--rfc[=<text>]` | | Prefix the subject with `RFC` (`[RFC PATCH]`); with a value, that text is used instead (`--rfc=WIP` → `[WIP PATCH]`). The value must be attached with `=` so a revision argument is never swallowed | off |
| `--no-rfc` | | Cancel a preceding `--rfc` (last flag wins) | off |
| `--filename-max-length <N>` | | Truncate output filenames to at most `N` bytes, counting the `0001-` prefix and the suffix; the stem may collapse to just the sequence number, matching Git's cut points | unlimited |
| `--commit-list-format <FMT>` | | Cover-letter commit list. Default (or `short`) is Git's author-grouped shortlog (`Name (N):` + subjects wrapped at 72 columns); `modern` renders `[i/N] subject`; any other value is a pretty-format string supporting `%h`, `%H`, `%s`, `%an`, `%ae`, `%n` and `%%` (unsupported conversions fail with `'<fmt>' is not a valid format string`) | `short` |
| `--relative[=<path>]` (unsupported) | | Accepted only to name the gap: Libra has no per-command relative-path filter for exported patches, so the command exits 128 with `LBR-UNSUPPORTED-001` | — |
| `--all` (unsupported) | | Accepted only to name the gap: Libra's revision grammar has no all-refs selector; name branches explicitly (`main..topic`) | — |
| `--no-base` | | Do not emit the `base-commit:` trailer, overriding `--base` (Libra has no `format.useAutoBase` config to reset) | off |
| `--start-number <N>` | | Start numbering at `N` | 1 |
| `--subject-prefix <PREFIX>` | | Use `PREFIX` instead of `PATCH` in the Subject: line | `PATCH` |
| `--cover-letter` | | Generate a cover-letter template (`0000-cover-letter<suffix>`, or `0` under `--numbered-files`) | false |
| `--thread` | | Add `In-Reply-To` and `References` headers (default on) | true |
| `--no-thread` | | Disable threading headers | false |
| `--in-reply-to <MESSAGE_ID>` | | Make the first mail a reply to the given Message-ID | none |
| `--to <ADDRESS>` | | Add a `To:` header (repeatable; multiple addresses fold like git). Placed after the MIME headers, on each patch and the cover letter | none |
| `--cc <ADDRESS>` | | Add a `Cc:` header (repeatable; folds like git) | none |
| `--no-to` / `--no-cc` | | Suppress the `To:` / `Cc:` headers (Libra has no `format.to`/`format.cc` config to reset) | false |
| `--from[=<IDENT>]` | | Use `<IDENT>` in the `From:` header instead of the commit author (bare `--from` uses the committer's configured identity). When it differs from the author, the original author is preserved as an in-body `From:` line so `git am` can restore it | author |
| `--reroll-count <N>` | `-v` | Mark as version `N` (changes `[PATCH]` to `[PATCH vN]`) | none |
| `--signoff` | `-s` | Append a `Signed-off-by` trailer to each commit message | false |
| `--no-signoff` | | Disable signoff, overriding `format.signOff` | false |
| `--notes[=<REF>]` | | Append each commit's notes after the `---` line, before the diffstat. Bare `--notes` uses the default ref (`refs/notes/commits`); `--notes=<ref>` reads `<ref>`. Rendered as `Notes:` (default ref) or `Notes (<ref>):`, each line indented four spaces; commits without a note are emitted unchanged | off |
| `--attach` | | Emit each patch as a `multipart/mixed` MIME message: the log message + diffstat in a `text/plain` part, the diff in a `text/x-patch` part with `Content-Disposition: attachment`. Mutually exclusive with `--inline` | off |
| `--inline` | | Like `--attach`, but the patch part uses `Content-Disposition: inline` | off |
| `--base <COMMIT>` | | Record a `base-commit:` trailer (and a `prerequisite-patch-id:` line for each non-merge commit between the base and the series, oldest-first) so `git am --base` can verify the series applies. The trailer rides on the last patch, or the cover letter under `--cover-letter`. The base must be an ancestor of the series (otherwise exit 128). `--base=auto` is not supported (exit 129). Patch-ids match `git patch-id --stable` for text diffs; **binary-file prerequisites are not guaranteed to match Git** | off |
| `--full-index` | | Show full object IDs in diff index header lines | false |
| `--minimal` | | Request the smallest Myers edit script; Libra's default Myers backend already guarantees this, so output is equivalent to the default | false |
| `--histogram` | | Generate text hunks with the Histogram diff algorithm | false |
| `--ignore-if-in-upstream` | | Suppress commits whose stable patch-id already appears on the excluded side of the range | false |
| `--src-prefix <PREFIX>` / `--dst-prefix <PREFIX>` | | Replace the default `a/` and `b/` diff path prefixes | `a/`, `b/` |
| `-p`, `--no-stat` | | Suppress the diffstat summary | false |
| `--keep-subject` | `-k` | Keep the commit subject byte-identical: no `[PATCH]` prefix and no `n/m` counter is added (Git's contract). Without it, a leading bracket group in the subject is stripped before the prefix is added | false |
| `--suffix <SFX>` | | Filename suffix for generated patches (e.g. `.txt`); ignored under `--numbered-files` | `.patch` |
| `--zero-commit` | | Use an all-zero hash in each patch's `From <hash>` envelope line | false |
| `--signature <SIGNATURE>` | | Text placed after the `-- ` line of each patch and the cover letter | libra version |
| `--no-signature` | | Omit the `-- `/signature footer entirely | false |
| `--signature-file <FILE>` | | Read the signature footer text from a file (mutually exclusive with `--signature`) | |
| `--encode-email-headers` / `--no-encode-email-headers` | | RFC 2047 Q-encode `From`/`Subject` header values that contain non-ASCII characters | off |
| `--numbered-files` | | Name output files by a bare sequence number (suffix not applied) | false |

## Configuration

When the corresponding CLI option is absent, `format.subjectPrefix`,
`format.signOff`, `format.outputDirectory`, and `format.suffix` are read through
the strict local → global → system config cascade. CLI values win, including
`--no-signoff`; `--stdout` does not consult `format.outputDirectory`. Invalid
Git booleans and config read failures are reported instead of silently using a
fallback.

## Examples

### Basic range
```bash
# Generate patches for the last three commits
libra format-patch HEAD~3..HEAD

# Generate exactly HEAD as a stream
libra format-patch -1 --stdout

# Include history all the way through the root commit
libra format-patch --root --stdout

# Numbered patches in a directory
libra format-patch -n -o patches/ main..feature

# With cover letter and threading
libra format-patch --cover-letter --thread origin/main..

# Version 2, replying to a previous thread
libra format-patch -v 2 --in-reply-to '<msgid@example>' origin/main..

# Pipe to an external tool
libra format-patch --stdout origin/main.. | git am

# Record the base the series applies to (for `git am --base`)
libra format-patch --base=origin/main --stdout origin/main..HEAD

# Last two commits, RFC-marked, with short filenames
libra format-patch -2 --rfc --filename-max-length=30

# Skip changes already present upstream and use custom diff prefixes
libra format-patch --ignore-if-in-upstream --src-prefix=old/ --dst-prefix=new/ origin/main..HEAD
```

## Output Format

Each patch file is an mbox message:

```
From <commit-oid> <unix-mbox-date>
From: Author Name <email>
Date: <RFC 2822 date>
Subject: [PATCH n/m] commit subject
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
Content-Transfer-Encoding: 8bit

commit message body
---
diffstat summary
unified diff
--
<libra-version>
```

The `-- ` footer defaults to the libra version; `--signature <text>` replaces
it with custom text, `--signature-file <file>` reads the footer text from a
file, and `--no-signature` omits the footer entirely. `--encode-email-headers`
RFC 2047 Q-encodes `From`/`Subject` header values that contain non-ASCII
characters. It is off by default in Libra (which has no `format.encodeEmailHeaders`
config knob); Git derives its default from that config, which is itself off
unless set.

With `--json` or `--machine`, `data.patches` lists every generated output.
When `--cover-letter` is set, the list includes the cover letter as record
number `0` before the commit patch records. Its filename is
`0000-cover-letter` with the configured suffix (default `.patch`), or just `0`
under `--numbered-files`.

`-N` keeps the `NNNN-` sequence prefix in filenames and suppresses the
`[PATCH n/m]` counter on commit patch Subject lines; a requested cover letter
retains its `[PATCH 0/N]` counter. `-n` does not change filenames either.
`--numbered-files` is the exception and uses bare sequence numbers. Earlier
Libra versions omitted the prefix under `-N`, so scripts that relied on those
filenames should rename the generated files themselves.

The cover letter's commit list is Git's shortlog by default: authors in
first-appearance order, each as `Name (N):` followed by their subjects
indented two spaces and wrapped at 72 columns (four-space continuation
indent). `--commit-list-format=modern` swaps that for `[i/N] subject` lines,
and any other value is read as a pretty-format string.

The complete series is rendered before any output file is created, and each
patch file is persisted with a temporary-file + atomic-rename write. Piped
stdout follows Libra's normal quiet BrokenPipe behavior.

## Error Handling

| Scenario | StableErrorCode |
|----------|-----------------|
| Not in a Libra repository | `LBR-REPO-001` |
| Unknown revision | `LBR-CLI-003` |
| `--base` is not an ancestor of the series | `LBR-CLI-003` (exit 128) |
| `--base=auto` (unsupported) | `LBR-CLI-002` (exit 129) |
| `--relative`, `--all` (unsupported) | `LBR-UNSUPPORTED-001` (exit 128) |
| Output file write failure | `LBR-IO-002` |
| Output directory creation failure | `LBR-IO-002` |
| Config read failure | `LBR-IO-001` |
| Invalid `format.signOff` / empty configured output directory | `LBR-CLI-003` |

## Blake3 repositories

When `core.objectformat` is `blake3`, `--base` prerequisite patch-ids are BLAKE3 digests (64 hex), not the SHA-1 Git-stable combiner used for SHA-1 repositories.
