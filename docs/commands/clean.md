# `libra clean`

Remove untracked files (and optionally directories) from the working tree.

## Synopsis

```
libra clean -n [-d] [-x | -X] [-e <pattern> | --exclude <pattern>]... [--json] [--quiet] [pathspec]...
libra clean -f [-d] [-x | -X] [-e <pattern> | --exclude <pattern>]... [--json] [--quiet] [pathspec]...
```

## Description

`libra clean` removes untracked files from the working tree. Unlike Git,
Libra requires an explicit mode flag: `-n` for a dry-run preview or `-f`
for actual deletion. Running `libra clean` without either flag is an
error. This prevents accidental data loss by forcing the user to state
intent explicitly.

By default, only files are removed and Git/Libra ignore sources are honored
(ignored files are skipped). The `-d` flag opts into removing untracked
directories as well; `-x` opts into removing files the ignore rules would
otherwise protect; `-X` flips the rules so that *only* ignored files are
removed. Every candidate path is canonicalized and verified to reside
inside the worktree root before deletion, preventing symlink-escape
attacks.

Optional pathspecs limit cleaning to matching untracked files or directory
prefixes. This is the current literal prefix matcher used by `clean`; shared
pathspec magic such as `:(exclude)` / `:(glob)` is not enabled for deletion
paths yet.

## Options

| Flag | Short | Long | Description |
|------|-------|------|-------------|
| Dry run | `-n` | `--dry-run` | Show what would be removed without deleting anything. |
| Force | `-f` | `--force` | Actually remove untracked files. |
| Directories | `-d` | `--dir` | Also remove untracked directories (otherwise only files). |
| Include ignored | `-x` | | Remove untracked files **including** those matched by ignore rules. |
| Only ignored | `-X` | | Remove **only** untracked files that are matched by ignore rules. |
| Exclude | `-e` | `--exclude <pattern>` | Add an extra exclusion pattern; may be repeated. |
| Pathspec | | positional | Limit candidates to matching files or directory prefixes. Shared pathspec magic is not enabled for `clean` yet. |
| JSON | | `--json` | Emit structured JSON output (see below). |
| Quiet | | `--quiet` | Suppress all human-readable stdout. |

`-x` and `-X` are mutually exclusive — `-x` *includes* ignored files in
addition to normally-untracked ones, `-X` restricts the operation to
ignored files only.

### Option Details

**`-n` / `--dry-run`**

Preview mode. Lists every untracked path that *would* be deleted without
touching the filesystem:

```bash
$ libra clean -n
Would remove build/output.log
Would remove notes.txt
```

**`-f` / `--force`**

Deletion mode. Removes every untracked path and reports each removal:

```bash
$ libra clean -f
Removing build/output.log
Removing notes.txt
```

**`-d` / `--dir`**

Opt-in for untracked directories. Without `-d`, untracked directories
are left in place (their contents are still considered if the directory
itself is tracked). With `-d`, the directory tree is walked and the
empty directory is removed after its files are.

**`-x`**

Override configured ignore sources. Without this flag, ignored files (build
artifacts, caches, etc.) are skipped. With `-x`, they are treated like
any other untracked file and removed.

**`-X`**

Inverse of `-x`. Removes only the files that ignore sources would
normally protect. Useful for "clean my build artifacts but leave
hand-edited files alone."

**`-e` / `--exclude <pattern>`**

Add an additional exclusion pattern (in Git ignore syntax) for this
invocation only. Can be passed multiple times to layer patterns:

```bash
libra clean -f --exclude '*.log' --exclude 'tmp/**'
```

**Combining `-n` and `-f`**: When both flags are passed, the dry-run
takes precedence and no files are deleted.

## Common Commands

```bash
# Preview what would be removed
libra clean -n

# Remove all untracked files (files only)
libra clean -f

# Also remove untracked directories
libra clean -fd

# Remove untracked files including ignored ones (build artifacts, caches)
libra clean -fx

# Remove only ignored files (keep hand-edited files intact)
libra clean -fX

# Layer an additional exclusion pattern on top of configured ignore sources
libra clean -f --exclude '*.log'

# Preview in JSON format (useful for scripting)
libra clean -n --json
```

## Human Output

Dry-run:

```text
Would remove build/output.log
Would remove notes.txt
```

Forced removal:

```text
Removing build/output.log
Removing notes.txt
```

`--quiet` suppresses stdout.

## Structured Output (JSON)

```json
{
  "ok": true,
  "command": "clean",
  "data": {
    "dry_run": true,
    "removed": ["build/output.log", "notes.txt"]
  }
}
```

`removed` is empty when there is nothing to clean.

## Design Rationale

### Why require an explicit mode flag?

Git's `clean` without `-f` (and without `clean.requireForce = false`)
prints an error asking for `-f`. This is a config-dependent guardrail.
Libra makes the guardrail unconditional: you must always pass `-n` or
`-f`. There is no configuration to weaken this requirement. This
eliminates an entire class of "I accidentally ran clean" incidents.

### Why no interactive mode (`-i`)?

Git's interactive clean mode presents a menu for selecting files. Libra
targets AI-agent and scripting workflows where interactive prompts are
unusable. The dry-run/force two-step workflow achieves the same safety
with full automation support: run `-n --json` to inspect, then `-f` to
execute.

### Why ship `-d` / `-x` / `-X` after originally declining them?

The original `clean` design intentionally declined the directory and
ignore-override flags out of safety concerns (`docs/development/commands/clean.md`
listed them as non-goals). Subsequent user feedback showed that build
workflows in agent-driven environments routinely need to clear ignored
artifacts, and the missing flags forced users to fall back on raw `rm
-rf` which is strictly less safe than `clean` (no symlink-escape
verification, no dry-run preview). The flags were added with the same
worktree-confinement and symlink checks as the base mode, preserving the
safety guarantees while restoring parity with `git clean`.

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Dry run | `-n` / `--dry-run` | `-n` / `--dry-run` | N/A (no clean command) |
| Force delete | `-f` / `--force` | `-f` / `--force` | N/A |
| Remove directories | `-d` / `--dir` | `-d` | N/A |
| Ignore override (all) | `-x` | `-x` | N/A |
| Ignore override (only ignored) | `-X` | `-X` | N/A |
| Exclude pattern | `-e <pattern>` / `--exclude <pattern>` (repeatable) | `-e <pattern>` (repeatable) | N/A |
| Interactive mode | Not supported | `-i` | N/A |
| Quiet mode | `--quiet` | `-q` / `--quiet` | N/A |
| JSON output | `--json` | Not supported | N/A |
| Pathspec filter | Literal file/directory prefix pathspecs | `<pathspec>...` | N/A |
| Require force config | Always required | `clean.requireForce` (default true) | N/A |

Note: jj does not have a `clean` command because its working-copy model
tracks all files automatically and untracked files are not a concept in
the jj data model.

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Missing `-f` / `-n` | `LBR-CLI-002` | 129 |
| Corrupted index or untracked scan failure | `LBR-IO-001` | 128 |
| Path resolves outside the worktree | `LBR-CONFLICT-002` | 128 |
| File deletion failed | `LBR-IO-002` | 128 |
