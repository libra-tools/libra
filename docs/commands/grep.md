# `libra grep`

Search for patterns in tracked files.

## Synopsis

```
libra grep [<options>] <pattern> [-- <pathspec>...]
libra grep -e <pattern> [-e <pattern>...] [-- <pathspec>...]
libra grep -f <file> [-- <pathspec>...]
```

## Description

`libra grep` searches for text patterns in tracked files. By default it searches the working tree, but it can also search the index (`--cached`) or a specific revision (`--tree <revision>`). Patterns are interpreted as regular expressions unless `--fixed-string` is specified.

Multiple patterns can be supplied via `-e` flags or read from files via `-f`. When multiple patterns are active, a file matches if any pattern matches at least one line (OR semantics). With `--all-match`, a file is included only if every pattern matches at least one line in that file (AND semantics across patterns, not across lines).

Output can be tuned with flags to show only filenames (`-l`, `-L`), match counts per file (`-c`), line numbers (`-n`), byte offsets (`-b`), or inverted matches (`-v`). The command supports pathspec filtering to restrict the search to specific files or directories. Repository searches use the shared pathspec engine, including `:(top)`, `:(exclude)`, `:(icase)`, `:(literal)`, and `:(glob)` magic.

When stdout is a terminal, output is sent through a pager. In JSON mode, structured output is emitted for programmatic consumption.

When stdout is piped and the downstream command exits early, `libra grep` exits quietly without
printing panic/backtrace or `Broken pipe` diagnostics.

Exit codes follow Git's grep contract: matches exit 0, no selected matches exit 1 without an error diagnostic, and grep command errors (for example invalid regexes, unsupported `-P`, missing pattern files, or invalid `--tree` revisions) exit 2. Repository preflight errors such as running repository mode outside a Libra repository keep the standard Libra fatal exit code.

## Options

| Flag | Short | Long | Description |
|------|-------|------|-------------|
| Pattern | | positional | The pattern to search for. Required unless `-e` or `-f` is specified. |
| Regexp | `-e` | `--regexp <PATTERN>` | Add a pattern to search for. Can be specified multiple times. |
| Pattern file | `-f` | `--file <FILE>` | Read patterns from a file, one per line. Can be specified multiple times. |
| All match | | `--all-match` | Require all patterns to match at least once in a file for that file to be included. |
| Fixed string | `-F` | `--fixed-string` | Treat the pattern as a literal string, not a regular expression. |
| Ignore case | `-i` | `--ignore-case` | Perform case-insensitive matching. |
| Count | `-c` | `--count` | Show only the number of matching lines for each file. |
| Files with matches | `-l` | `--files-with-matches` | Show only the names of files that contain matches. |
| Files without matches | `-L` | `--files-without-matches` | Show only the names of files that do not contain matches. |
| Line number | `-n` | `--line-number` | Prefix each matching line with its 1-based line number. |
| Word regexp | `-w` | `--word-regexp` | Match only lines where the pattern forms a complete word (surrounded by word boundaries). |
| Invert match | `-v` | `--invert-match` | Select non-matching lines instead of matching lines. |
| Byte offset | `-b` | `--byte-offset` | Show the 0-based byte offset of the first match on each line. |
| Max count | `-m` | `--max-count <NUM>` | Stop after NUM matching lines per file. |
| Only matching | `-o` | `--only-matching` | Print only the matched parts of a line, one match per output line (context lines are suppressed). |
| Pathspec | | positional (trailing) | Restrict search to files matching the given paths. |
| Tree | | `--tree <REVISION>` | Search in the specified revision or commit tree instead of the working tree. |
| Cached | | `--cached` | Search in the index (staging area) instead of the working tree. |
| Untracked | | `--untracked` | In addition to tracked files, also search untracked, non-ignored files in the working tree. Cannot be combined with `--cached` or `--tree`. |
| No index | | `--no-index` | Search the filesystem directly (the given paths, or the current directory) without a repository or index. Works outside a repository, recurses every file including ignored ones (skipping `.git`/`.libra`), and shows paths relative to the current directory. Cannot be combined with `--cached`, `--untracked`, or `--tree`. |
| Max depth | | `--max-depth <DEPTH>` | Descend at most DEPTH levels of directories below each pathspec. A file directly inside the pathspec directory is depth 0; a negative value means no limit. With no pathspec, depth is measured from the worktree root (not the current directory) — `libra grep` always searches the whole worktree with worktree-relative paths, so to limit to a subdirectory, pass it as a pathspec. |
| Heading | | `--heading` / `--no-heading` | Print each file name once as a heading above its matches instead of prefixing every line. `--no-heading` is the default. |
| Break | | `--break` / `--no-break` | Print an empty line between matches from different files. `--no-break` is the default. |
| Null | `-z` | `--null` | Output a NUL byte after the file name (and line number) instead of `:`, for machine consumption. |

### Option Details

**Positional `<pattern>`**

The primary search pattern. Interpreted as a regular expression by default:

```bash
$ libra grep "fn\s+execute"
src/command/merge.rs:pub async fn execute(args: MergeArgs) {
src/command/rebase.rs:pub async fn execute(args: RebaseArgs) {
```

**`-e` / `--regexp`**

Add additional patterns. When combined with `--all-match`, all patterns must match in a file:

```bash
# Find files containing both "TODO" and "FIXME"
$ libra grep -e "TODO" -e "FIXME" --all-match
```

**`-f` / `--file`**

Read patterns from a file, one per line:

```bash
$ libra grep -f patterns.txt
```

**`-F` / `--fixed-string`**

Treat the pattern as a literal string. Useful when searching for strings that contain regex metacharacters:

```bash
$ libra grep -F "Vec<String>"
```

**`-i` / `--ignore-case`**

Case-insensitive matching:

```bash
$ libra grep -i "error"
```

**`-c` / `--count`**

Show match counts instead of matching lines:

```bash
$ libra grep -c "TODO"
src/main.rs:2
src/lib.rs:5
```

**`-l` / `--files-with-matches`**

Show only filenames with matches:

```bash
$ libra grep -l "TODO"
src/main.rs
src/lib.rs
```

**`-L` / `--files-without-matches`**

Show only filenames without matches:

```bash
$ libra grep -L "TODO"
src/utils.rs
```

**`-n` / `--line-number`**

Show line numbers:

```bash
$ libra grep -n "TODO"
src/main.rs:42:// TODO: refactor this
```

**`-w` / `--word-regexp`**

Match whole words only:

```bash
# Matches "error" but not "errors" or "error_handler"
$ libra grep -w "error"
```

**`-v` / `--invert-match`**

Show lines that do not match:

```bash
$ libra grep -v "^$" src/main.rs
```

**`-b` / `--byte-offset`**

Show byte offsets of matches:

```bash
$ libra grep -b "TODO"
src/main.rs:1024:// TODO: refactor
```

**`--tree`**

Search in a specific revision:

```bash
$ libra grep --tree HEAD~3 "deprecated"
$ libra grep --tree v1.0 "config"
```

**`--cached`**

Search in the index instead of the working tree:

```bash
$ libra grep --cached "TODO"
```

## Common Commands

```bash
# Search for a pattern in tracked files
libra grep "TODO"

# Case-insensitive regex search
libra grep -i "error|warning"

# Search for a literal string
libra grep -F "HashMap<String, Vec<u8>>"

# Show only filenames with matches
libra grep -l "deprecated"

# Show match counts
libra grep -c "unwrap()"

# Search with line numbers
libra grep -n "fn main"

# Search in a specific revision
libra grep --tree HEAD~5 "old_function"

# Search in the index
libra grep --cached "staged_change"

# Restrict to specific paths
libra grep "TODO" -- src/command/

# Multiple patterns (OR)
libra grep -e "TODO" -e "FIXME" -e "HACK"

# Multiple patterns (AND across file)
libra grep -e "use.*serde" -e "Serialize" --all-match
```

## Human Output

Default output (file:line format):

```text
src/main.rs:// TODO: refactor this function
src/lib.rs:// TODO: add error handling
```

With line numbers (`-n`):

```text
src/main.rs:42:// TODO: refactor this function
src/lib.rs:15:// TODO: add error handling
```

With byte offset (`-b`):

```text
src/main.rs:1024:// TODO: refactor this function
```

Count mode (`-c`):

```text
src/main.rs:1
src/lib.rs:3
```

Files-with-matches mode (`-l`):

```text
src/main.rs
src/lib.rs
```

Files-without-matches mode (`-L`):

```text
src/utils.rs
src/config.rs
```

Tree search (`--tree`):

```text
HEAD~3:src/main.rs:// TODO: old code
```

## Structured Output (JSON)

```json
{
  "pattern": "TODO",
  "patterns": ["TODO"],
  "context": "working-tree",
  "total_matches": 5,
  "total_files": 2,
  "matches": [
    {
      "path": "src/main.rs",
      "line_number": 42,
      "line": "// TODO: refactor this function",
      "byte_offset": null
    }
  ],
  "counts": null,
  "files_with_matches": null,
  "files_without_matches": null,
  "warnings": []
}
```

When using `--count`:

```json
{
  "pattern": "TODO",
  "patterns": ["TODO"],
  "context": "working-tree",
  "total_matches": 5,
  "total_files": 2,
  "matches": null,
  "counts": [
    { "path": "src/main.rs", "count": 2 },
    { "path": "src/lib.rs", "count": 3 }
  ],
  "warnings": []
}
```

When using `-l`:

```json
{
  "pattern": "TODO",
  "patterns": ["TODO"],
  "context": "working-tree",
  "total_matches": 5,
  "total_files": 2,
  "matches": null,
  "files_with_matches": ["src/main.rs", "src/lib.rs"],
  "warnings": []
}
```

The `warnings` array contains entries for files that could not be read or were skipped (e.g., binary files):

```json
{
  "path": "assets/image.png",
  "message": "binary file, skipping"
}
```

## Design Rationale

### Why built-in instead of relying on external grep/ripgrep?

External tools like `grep`, `rg`, or `ag` are excellent for searching files on disk, but they have no awareness of version control state. They cannot:

- **Search the index**: `--cached` searches staged content, which may differ from the working tree. External tools can only see the working tree.
- **Search historical revisions**: `--tree` searches the content of a specific commit without checking it out. External tools would require extracting the entire tree to a temporary directory.
- **Respect tracked-file semantics**: Libra's grep searches only tracked files by default, automatically excluding untracked and ignored files without needing a separate ignore configuration.
- **Produce structured output**: The JSON output includes metadata like total counts, file lists, and warnings in a single parseable structure.

For pure working-tree searches, external tools are often faster. Libra's grep trades raw speed for version-control integration.

### Why `--tree` for revision search?

The `--tree` flag searches the content of a specific revision's tree object, reading blob data directly from the object store. This is equivalent to `git grep <revision>` but uses a named flag instead of a positional argument to avoid ambiguity between patterns and revision specifiers.

Git's positional syntax (`git grep <pattern> <revision>`) is a common source of confusion because the parser must distinguish between a pattern and a revision. Libra avoids this ambiguity by requiring `--tree` as an explicit flag.

### Why `--cached` for index search?

The index (staging area) may contain different content than the working tree when files have been staged but not yet committed. Searching the index is useful for verifying what will be committed, especially in scripting and CI workflows.

### Why regex by default?

Regular expressions are the standard pattern language for code search. Most developers expect `grep` to support regex, and Libra follows this convention. The `--fixed-string` flag is available for literal searches when regex metacharacters would cause problems.

### How does this compare to Git and jj?

Git's `grep` is similar in scope: it searches tracked files with regex support, offers `-l`, `-c`, `-n`, `-w`, `-v`, `-i`, and can search in revisions and the index. Libra's implementation covers the same core feature set with the addition of structured JSON output.

jj does not have a built-in grep command. Users are expected to use external tools for text search. This works well for working-tree searches but means there is no integrated way to search historical revisions without first checking them out.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Pattern | Positional | Positional | N/A (no grep) |
| Additional patterns | `-e` / `--regexp` | `-e` | N/A |
| Pattern file | `-f` / `--file` | `-f` | N/A |
| All match | `--all-match` | `--all-match` | N/A |
| Fixed string | `-F` / `--fixed-string` | `-F` / `--fixed-strings` | N/A |
| Ignore case | `-i` / `--ignore-case` | `-i` / `--ignore-case` | N/A |
| Count | `-c` / `--count` | `-c` / `--count` | N/A |
| Files with matches | `-l` / `--files-with-matches` | `-l` / `--files-with-matches` | N/A |
| Files without matches | `-L` / `--files-without-matches` | `-L` / `--files-without-match` | N/A |
| Line number | `-n` / `--line-number` | `-n` / `--line-number` | N/A |
| Word regexp | `-w` / `--word-regexp` | `-w` / `--word-regexp` | N/A |
| Invert match | `-v` / `--invert-match` | `-v` / `--invert-match` | N/A |
| Byte offset | `-b` / `--byte-offset` | Not supported | N/A |
| Pathspec | Trailing positional | Trailing positional | N/A |
| Revision search | `--tree <REVISION>` | `<revision>` (positional) | N/A |
| Index search | `--cached` | `--cached` | N/A |
| Context lines | `-A` / `-B` / `-C` | `-C` / `-A` / `-B` | N/A |
| Extended regexp | `-E` / `--extended-regexp` | `-E` / `--extended-regexp` | N/A |
| Perl regexp | Rejected (exit 2) | `-P` / `--perl-regexp` | N/A |
| Max count | `-m` / `--max-count` | `-m` / `--max-count` | N/A |
| Only matching | `-o` / `--only-matching` | `-o` / `--only-matching` | N/A |
| Show function | Not supported | `-p` / `--show-function` | N/A |
| Max depth | `--max-depth <DEPTH>` | `--max-depth <DEPTH>` | Equivalent when a pathspec is given (depth is measured relative to the pathspec). With no pathspec, Libra measures depth from the worktree root rather than the current directory, because `libra grep` always searches the whole worktree with worktree-relative paths — pass the directory as a pathspec to scope it. |
| Threads | Not supported | `--threads` | N/A |
| Color | Automatic (terminal detection) | `--color` | N/A |
| JSON output | Built-in JSON structure | Not supported | N/A |

Note: jj does not have a built-in grep command. Users rely on external tools like `grep`, `rg`, or `ag` for text search.

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Not a libra repository | `LBR-REPO-001` | 128 |
| No pattern provided (and no `-e` or `-f`) | Clap argument error | 2 |
| Invalid regex pattern | `LBR-CLI-002` (CliInvalidArguments) | 2 |
| Revision not found (`--tree`) | `LBR-CLI-003` (CliInvalidTarget) | 2 |
| No matches found | Status-only signal | 1 |
| Failed to read file (non-fatal) | Warning in output, file skipped | 0 |
| Failed to read pattern file (`-f`) | Error with file path details | 2 |
