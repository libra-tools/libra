# `libra shortlog`

Summarize reachable commits by author.

**Alias:** `slog`

## Synopsis

```
libra shortlog [<revision>] [-n] [-s] [-e] [-c] [--no-merges | --merges]
               [--top <N>] [--min-count <N>] [--reverse]
               [--since <date>] [--until <date>] [-w[<W>[,<I1>[,<I2>]]]] [--format <FORMAT>]

git log | libra shortlog [-n] [-s] [-e] [--group <TYPE>] [--author <pattern>] [-w[...]]
```

## Description

`libra shortlog` summarizes reachable commits grouped by author, primarily used for release announcements and contributor overviews. It walks the commit graph from the specified revision (defaulting to HEAD) and aggregates commits per author, displaying each author's commit count and optionally their commit subjects.

When no revision is given and standard input is not a terminal and carries data, `libra shortlog` instead summarizes piped `git log` / `libra log` output (e.g. `git log | libra shortlog`), matching Git's stdin mode. An empty or terminal stdin falls back to the `HEAD` default (an intentional convenience over Git, which has no default revision). Pipe mode parses the default (`medium`) or `fuller` log format — `Author:` / `Commit:` identity headers and the 4-space-indented message — and honors the grouping and display options (`-n` / `-s` / `-e` / `--group` / `--author` / `-w` / `--top` / `--min-count` / `--reverse`); the walk-only filters (`--since` / `--until` / `--merges` / `--no-merges` / `--format`) have no commit objects to act on and are ignored, as in Git. Pipe mode still runs inside a Libra repository.

By default, authors are sorted alphabetically by name. With `-n`, they are sorted by commit count (descending). The `-s` flag produces a summary with only counts, suppressing individual commit subjects. The `-e` flag includes the author's email address in the output.

Date filtering via `--since` and `--until` restricts which commits are included based on their committer timestamp, supporting formats like `YYYY-MM-DD`, `"N days ago"`, and Unix timestamps.

## Options

| Option | Short | Long | Description |
|--------|-------|------|-------------|
| Numbered | `-n` | `--numbered` | Sort output by number of commits per author (descending) instead of alphabetically. |
| Summary | `-s` | `--summary` | Suppress commit descriptions; show only per-author commit counts. |
| Email | `-e` | `--email` | Show the email address of each author alongside their name. When enabled, authors are grouped by `name <email>` pair. |
| Committer | `-c` | `--committer` | Group commits by committer identity instead of author. |
| Group | | `--group <TYPE>` | Group by `author` (default), `committer`, or `trailer:<key>` (one group per value of the named commit-message trailer, e.g. `trailer:Co-authored-by`). Takes precedence over `-c`. |
| No merges | | `--no-merges` | Exclude merge commits (commits with more than one parent) before aggregation. |
| Merges | | `--merges` | Include only merge commits (the inverse of `--no-merges`; the two override each other). |
| Top | | `--top <N>` | Show only the top N identities (after sorting). |
| Min count | | `--min-count <N>` | Show only identities with at least N commits. |
| Reverse | | `--reverse` | Reverse the output order. |
| Since | | `--since <date>` | Only include commits more recent than the specified date. |
| Until | | `--until <date>` | Only include commits older than the specified date. |
| Wrap | `-w` | `--wrap [<W>[,<I1>[,<I2>]]]` | Linewrap subjects at width `W` (default 76), first-line indent `I1` (6), continuation indent `I2` (9). `-w0` indents without wrapping. |
| Format | | `--format <FORMAT>` | Render each commit line under its author header with a custom template instead of the subject. Supports the same `%`-placeholders as `libra log --format`, including `%H`, `%h`, `%P`, `%p`, `%s`, `%f`, `%b`, `%B`, `%n`, ASCII/control `%xNN`, `%%`, `%an`, `%ae`, `%ad`, `%aI`, `%at`, `%cn`, `%ce`, `%cd`, `%cI`, `%ct`, `%d`, `%D`, `%m`, and color placeholders. |
| Revision | | positional (optional) | The revision to summarize from. Defaults to `HEAD`. |
| JSON | | `--json` | Emit structured JSON output. |
| Quiet | | `--quiet` | Suppress human-readable output. |

### Option Details

**`-n` / `--numbered`**

Sorts authors by descending commit count. When two authors have the same count, they are sorted alphabetically:

```bash
$ libra shortlog -n
   5  Alice
   3  Bob
   1  Charlie
```

**`-s` / `--summary`**

Produces compact output with only counts, omitting individual commit subjects:

```bash
$ libra shortlog -s
   2  Test User
```

Without `-s`, commit subjects are listed under each author:

```bash
$ libra shortlog
   2  Test User
      initial
      follow-up
```

**`-e` / `--email`**

Appends the email address to each author. When enabled, authors with the same name but different emails are listed separately:

```bash
$ libra shortlog -e
   2  Test User <test@example.com>
      initial
      follow-up
```

**`--since` / `--until`**

Filter commits by committer timestamp. Supported date formats include:

- `YYYY-MM-DD` (e.g., `2026-01-01`)
- Relative dates (e.g., `"7 days ago"`, `"2 weeks ago"`)
- Unix timestamps

```bash
# Commits in the last month
libra shortlog --since "30 days ago"

# Commits in a date range
libra shortlog --since 2026-01-01 --until 2026-03-31
```

**Revision argument**

Specify a starting point other than HEAD:

```bash
# Summarize the last 5 commits
libra shortlog HEAD~5

# Summarize from a tag
libra shortlog v1.0
```

## Common Commands

```bash
# Default shortlog from HEAD
libra shortlog

# Summary with counts only, sorted by count
libra shortlog -n -s

# Include email addresses
libra shortlog -e

# Last 5 commits summary
libra shortlog HEAD~5

# Commits in a date range
libra shortlog --since 2026-01-01 --until 2026-03-31

# JSON output for scripting
libra shortlog --json

# Summarize piped log output (Git's stdin mode)
git log | libra shortlog -n -s
```

## Human Output

Default (alphabetical, with subjects):

```text
   2  Test User
      initial
      follow-up
```

Summary mode (`-s`) suppresses subjects. `-e` appends `<email>`.

Subject extraction skips embedded signature headers and uses the first meaningful commit message line.

The count column is right-aligned with consistent width based on the maximum count across all authors.

## Structured Output (JSON)

```json
{
  "ok": true,
  "command": "shortlog",
  "data": {
    "revision": "HEAD",
    "numbered": false,
    "summary": false,
    "email": false,
    "total_authors": 1,
    "total_commits": 2,
    "authors": [
      {
        "name": "Test User",
        "email": null,
        "count": 2,
        "subjects": ["initial", "follow-up"]
      }
    ]
  }
}
```

In summary mode, `subjects` is an empty array. When `-e` is enabled, the `email` field contains the author's email string; otherwise it is `null`.

The `total_authors` and `total_commits` fields provide aggregate counts for quick consumption by scripts and agents.

## Design Rationale

### How `--group` works

Git's `--group=author`/`--group=committer`/`--group=trailer:<key>` selects what commits are grouped by. Libra supports all three: `author` (the default) and `committer` mirror the identity grouping also available via `-c`, while `trailer:<key>` groups by each value of the named commit-message trailer (e.g. `--group=trailer:Co-authored-by`), which is useful for analyzing co-authored commits or attributions recorded via trailers like `Signed-off-by`. A commit may contribute to several trailer groups (one per matching trailer line) or none. `--group` takes precedence over `-c`/`--committer`. The trailer key is matched case-insensitively against the last paragraph (the trailer block) of each commit message, and `Name <email>` values are split into name and email for the report. Git's full `interpret-trailers` configuration (folding, separators, custom config) is not modeled.

### Revision argument and piped input

Git's `shortlog` can operate in two modes: reading `git log` output piped via stdin, or directly traversing commit history. Libra supports **both**, and both feed the `--json` output. Its primary mode takes the revision as a positional argument (defaulting to `HEAD`) and reads directly from the commit graph — simpler and faster (no serialization round-trip). When no revision is given and stdin is piped with data, Libra parses that piped log output instead (`git log | libra shortlog`), for Unix-style composability and parity with Git's stdin mode. Because the piped data is serialized text rather than commit objects, pipe mode is limited to the identity/subject information the log format carries: the grouping and display options apply, but commit-graph filters (`--since`/`--until`/`--merges`/`--no-merges`) and the object-dependent `--format` template do not.

### Why a curated filter subset instead of full log options?

Git's `shortlog` inherits the full set of `git log` options when used directly (not piped) — `--author`, `--grep`, `--no-merges`, and dozens of others. Libra exposes a curated subset that covers the common shortlog needs — date filtering (`--since`/`--until`), `--author`, and `--merges`/`--no-merges` — without inheriting the full complexity of the log command's option space. Less common log filters such as `--grep` are not exposed.

### Why committer timestamp for filtering?

The `--since`/`--until` filters use the committer timestamp (not the author timestamp), matching Git's behavior. The committer timestamp reflects when a commit was actually applied to the current branch (e.g., after rebase), which is more relevant for release-period summaries than the original authoring date.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Numbered sort | `-n` / `--numbered` | `-n` / `--numbered` | N/A (no shortlog command) |
| Summary only | `-s` / `--summary` | `-s` / `--summary` | N/A |
| Show email | `-e` / `--email` | `-e` / `--email` | N/A |
| Since date | `--since <date>` | `--since <date>` / `--after <date>` | N/A |
| Until date | `--until <date>` | `--until <date>` / `--before <date>` | N/A |
| Revision | `<revision>` (positional) | `<revision range>...` | N/A |
| Group by | `--group=author\|committer\|trailer:<key>` | `--group=author\|committer\|trailer:<key>` | N/A |
| Format | `--format=<format>` | `--format=<format>` | N/A |
| Committer grouping | `-c` / `--committer` | `--committer` (deprecated, use `--group=committer`) | N/A |
| Piped input | `git log \| libra shortlog` (no revision, non-tty stdin with data; inside a repo) | Reads from stdin when piped | N/A |
| No merges | `--no-merges` | `--no-merges` | N/A |
| Merges only | `--merges` | `--merges` | N/A |
| Author filter | `--author=<pattern>` | `--author=<pattern>` | N/A |
| Output wrapping | `-w[<width>[,<i1>[,<i2>]]]` | `-w[<width>[,<i1>[,<i2>]]]` | N/A |
| Grep filter | Not supported | `--grep=<pattern>` | N/A |
| JSON output | `--json` | Not supported | N/A |
| Quiet mode | `--quiet` | Not supported | N/A |

Note: jj does not have a shortlog command. Similar information can be obtained by filtering `jj log` output, but there is no built-in author aggregation.

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Invalid `--since` / `--until` date | `LBR-CLI-002` | 129 |
| Invalid revision | `LBR-CLI-003` | 129 |
| HEAD has no commit | `LBR-REPO-003` | 128 |
| Failed to read refs or commit graph | `LBR-IO-001` / `LBR-REPO-002` | 128 |
