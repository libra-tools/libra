# `libra archive`

Create an archive from a committed tree snapshot.

## Synopsis

```bash
libra archive [OPTIONS] [TREEISH] [PATH]...
libra archive --list
```

## Description

`libra archive` is analogous to `git archive`: it resolves a commit, branch,
tag, or abbreviated commit hash, walks that commit tree, and writes the tracked
files as an archive. The command does not modify the working tree or index.

When `TREEISH` is omitted, the command archives `HEAD`. The default format is
an uncompressed tar stream written to stdout. Use `--output <FILE>` when running
from an interactive shell so binary archive bytes are written to a file instead
of the terminal. When `PATH` arguments are provided after `TREEISH`, only
matching files or directories inside that committed tree are archived.

Entries whose path is matched by the `export-ignore` attribute in the archived
tree's `.gitattributes` or `.libra_attributes` files are omitted from the
archive. Uncommitted working-tree attribute changes do not affect an archive of
an existing `TREEISH`. `export-subst` is not implemented.

## Options

| Flag | Short | Description | Default |
|------|-------|-------------|---------|
| `[TREEISH]` | | Commit, branch, tag, or abbreviated commit hash to archive | `HEAD` |
| `[PATH]...` | | Limit the archive to matching files or directories inside `TREEISH` | all files |
| `--list` | `-l` | List supported archive formats and exit | false |
| `--format <FMT>` | `-f` | Archive format: `tar`, `tar.gz`, `tgz`, `tar.bz2`, `tbz2`, `tbz`, or `zip` | `tar` |
| `--output <FILE>` | `-o` | Write archive bytes to a file instead of stdout | stdout |
| `--prefix <PREFIX>` | | Prepend a relative directory prefix to each archived path | none |
| `--verbose` | `-v` | Report each archived path (prefix applied) to stderr as progress | false |
| `--add-file=<file>` | | Add an untracked working-tree file to the archive at its basename (under `--prefix`). Repeatable; not subject to the `[PATH]...` filter. Must appear before `[TREEISH]`. | none |
| `--compression-level <0-9>` | | Compression level for `tar.gz`/`tar.bz2`/`zip` (ignored for plain `tar`). This is Git's `-0`..`-9`, which clap can't model as bare numeric flags. bzip2 has no level 0, so 0 is treated as 1. | format default |
| `--mtime <time>` | | Set the modification time of all archive entries (same date formats as `--since`/`--until`: `YYYY-MM-DD`, RFC 3339, relative, or a Unix timestamp). | the archived commit's committer time |

`--prefix <PREFIX>` must be relative. Absolute prefixes and prefixes containing
`..` path components are rejected to prevent archive path traversal.

`PATH` arguments must also be relative and must not contain `..`. Directory
pathspecs include all matching files below that directory. `--list` does not
require a repository.

## Examples

```bash
# Write HEAD as an uncompressed tar archive.
libra archive -o project.tar

# Write a gzip-compressed release archive.
libra archive --format=tar.gz --prefix=project-v1.0/ -o project-v1.0.tar.gz v1.0

# Write a bzip2-compressed archive using the short format flag.
libra archive -f tbz2 -o project.tar.bz2 HEAD

# Write a zip archive for a branch.
libra archive --format=zip -o feature.zip feature-branch

# List supported formats.
libra archive --list

# Archive only files under src/ from HEAD.
libra archive -o src.tar HEAD src/

# Include an untracked file (e.g. release notes) alongside the tree.
libra archive --add-file=RELEASE_NOTES.txt -o release.tar HEAD
```

## Output

On success, `libra archive` writes archive bytes to stdout or to the path passed
with `--output <FILE>`. It does not print a separate success message.

Tar archives preserve regular files, executable file modes, symlinks, nested
paths, empty files, and Unicode filenames. Zip archives are built in memory
first because the zip writer requires seekable output, then flushed to the
requested destination.

## Error Handling

| Scenario | StableErrorCode |
|----------|-----------------|
| Unknown `TREEISH` or empty repository | `LBR-CLI-003` |
| `PATH` does not match any archived file | `LBR-CLI-003` |
| Unknown `--format <FMT>` value | `LBR-CLI-002` |
| Unsafe `--prefix <PREFIX>` | `LBR-CLI-002` |
| `--add-file=<file>` path is missing or unreadable | `LBR-IO-001` |
| `--add-file=<file>` is not a regular file (e.g. a directory) | `LBR-CLI-002` |
| Unsafe `PATH` pathspec | `LBR-CLI-002` |
| Referenced repository object cannot be read | `LBR-REPO-002` |
| Blob content cannot be read | `LBR-IO-001` |
| Output file cannot be created or written | `LBR-IO-002` |

Failure output uses Libra's standard structured CLI error report.
