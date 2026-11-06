# `libra update-index`

Modify the index directly — a focused subset of `git update-index`. The
companion to [`write-tree`](write-tree.md): `--cacheinfo` registers an index
entry from an object id without reading the working tree, so an index can be
built purely from objects.

## Synopsis

```
libra update-index [--index-file <PATH>] [OPTIONS]
libra update-index --add <path>...
libra update-index --remove <path>...
libra update-index --cacheinfo <mode>,<object>,<path>...
libra update-index --skip-worktree <path>...
libra update-index --refresh
libra update-index --index-info < <(printf '100644 <oid>\t<path>\n')
libra update-index --chmod=+x <path>...
```

## Description

`update-index` walks operations in command-line order: each `--cacheinfo` entry
is applied where it appears, and each positional path is processed with the
path-operation flags in effect at that point. It saves the index once after the
walk. `--index-file <PATH>` selects a separate index instead of `.libra/index`;
if that file does not exist, Libra starts with an empty index, which is useful
for scratch index composition. `--add` and `--remove` are permissions rather
than alternatives: `--remove` alone drops each named path from the index,
`--add` alone (re)stages it from the working tree, and given together they
route per path by whether it still exists — an existing path is staged, a
missing one is dropped.

- `--cacheinfo <mode>,<object>,<path>` inserts/updates an entry directly. The
  object **need not exist yet** (matching Git), so you can build an index from
  hashes computed with `hash-object`. `<mode>` is an octal file mode:
  `100644` (file), `100755` (executable), `120000` (symlink), `160000`
  (gitlink). The object id length must match the repository hash format. The
  path is an index key — absolute paths and `..` traversal are rejected. A later
  `write-tree` or `commit` validates object existence/type and fails with
  `LBR-REPO-002` if a blob/tree entry still points at a missing or wrong-type
  object.
- `--add <path>...` (re)stages files from the working tree, allowing paths not
  yet tracked. Working-tree symlinks are staged as mode `120000` blobs whose
  content is the link target bytes; the target is not followed. Without
  `--add`, a positional path must already be tracked.
- `--remove <path>...` drops the named paths from the index. Given together with
  `--add`, presence on disk decides per path: an existing path is staged, a missing
  one is dropped — which is how upstream setup steps use `--add --remove` in one
  call. `--remove` on its own always drops the path, whether or not it still exists.
- `--skip-worktree <path>...` marks tracked paths with the skip-worktree bit
  (index v3 extended flags): `status`/`diff` then ignore the working-tree copy,
  and default `add`/`update-index` path updates skip it, so the path can stay
  absent or diverge locally without noise. To explicitly stage a changed,
  existing sparse path, use `libra add --sparse <path>`; that preserves the
  skip-worktree bit. The mark is recorded in the index only — the working tree
  is never read and the entry content is untouched. A path that is not in the
  index fails with `Unable to mark file` (exit 128). Setting the bit rewrites
  the index as format v3; `--no-skip-worktree` clears it again, and the index
  returns to format v2 once no entry carries extended flags. Repositories with
  no marked paths keep byte-identical v2 indexes.

Working-tree staging returns `LBR-IO-002` if the blob or its durable cloud
index marker cannot be written; it does not panic or save an index entry that
lacks repair ownership. The failure message follows the canonical wording:
the object payloads were stored safely, no paths were staged, and a direct
retry reuses the already-stored payloads without any lock-file cleanup (lock
timeouts additionally name the lock holder, and lock files must never be
deleted). A normal retry re-registers a payload that the failed attempt
already persisted.

Local index persistence and the later cloud-catalog update have separate
durability boundaries. If a terminal background `object_index` error happens
after the blob and index are saved, `update-index` keeps its normal success
output, emits an actionable stderr warning, and retains an atomic repair marker
for the next schema-aware repository command. `cloud sync` and destructive
agent cleanup fail closed while repair remains pending. With
`--exit-code-on-warning`, the completed local update returns exit 9 /
`LBR-WARN-001`; retrying `update-index` is unnecessary.

## Options

| Option | Description | Example |
|--------|-------------|---------|
| `--index-file <PATH>` | Use this index instead of `.libra/index`. A missing file starts as an empty index. | `libra update-index --index-file /tmp/draft.index --cacheinfo 100644,<oid>,a.txt` |
| `--add` | Allow positional paths to add new (untracked) files. | `libra update-index --add a.txt` |
| `--remove` | Remove the positional paths from the index. With `--add`, presence on disk decides per path (existing → staged, missing → removed). | `libra update-index --remove old.txt` |
| `--force-remove` | Drop the positional paths from the index regardless of whether the working-tree file exists; paths the index does not know are a no-op, and every stage of an unmerged path is removed. Wins over `--add`/`--remove`. | `libra update-index --force-remove old.txt` |
| `--skip-worktree` | Mark tracked paths with the skip-worktree bit (index v3): status/diff ignore the working-tree copy; default add/update-index path updates skip it. `libra add --sparse` can explicitly stage a changed, existing path while preserving the bit. Untracked paths fail with exit 128. | `libra update-index --skip-worktree s.txt` |
| `--no-skip-worktree` | Clear the skip-worktree bit again; the index returns to v2 when no extended flags remain. | `libra update-index --no-skip-worktree s.txt` |
| `--add --remove` | Both permissions at once: stage what exists, drop what is gone. | `libra update-index --add --remove a.txt gone.txt` |
| `--cacheinfo <mode>,<object>,<path>` | Register an entry from an object id (repeatable). | `libra update-index --cacheinfo 100644,<oid>,dir/f.txt` |
| `--refresh` | Re-stat the index against the working tree: stat-only drift is refreshed silently; a modified or missing file prints `<path>: needs update` and exits 1. Unmerged entries print `<path>: needs merge`. Options `-q` (suppress output and the exit code), `--ignore-missing`, and `--unmerged` must precede `--refresh`. | `libra update-index --refresh` |
| `--really-refresh` | Same as `--refresh` (Libra has no assume-unchanged bit). Both clear a leftover assume-unchanged (`CE_VALID`) bit; see [Legacy assume-unchanged bits](#legacy-assume-unchanged-bits). | `libra update-index --really-refresh` |
| `--chmod=(+\|-)-x` | Flip the executable bit on the positional (and `--stdin`) paths that follow it until overridden. The working-tree file is untouched. | `libra update-index --chmod=+x a.sh` |
| `--info-only` | Record object ids without storing the blobs through the staging path. | `libra update-index --add --info-only new.txt` |
| `--verbose` | Report each action on stdout: `add '<path>'`, `remove '<path>'`, `chmod +x '<path>'` / `chmod -x '<path>'`. | `libra update-index --add --verbose a.txt` |
| `--index-info` | Read index records from stdin (NUL with `-z`), must be the last argument. Accepts `<mode> SP <oid> TAB <path>`, `<mode> SP <type> SP <oid> TAB <path>`, or `<mode> SP <oid> SP <stage> TAB <path>`; `mode 0` removes every stage. A malformed line exits 128 with the index unchanged. | `printf '100644 <oid> 3\tfoo\n' \| libra update-index --index-info` |
| `--stdin` | Read one path per record from stdin (NUL with `-z`), must be the last argument, processed like positional paths under the current flags. | `printf 'a.txt\n' \| libra update-index --add --stdin` |
| `-z` / `--null` | With `--index-info`/`--stdin`, records are NUL-terminated. | `libra update-index -z --add --stdin` |
| `--json` / `--machine` | Structured output: `{ updated: <n>, removed: <n> }`. | `libra --json update-index --add a.txt` |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | The index was updated and saved. |
| `1` | `--refresh` found entries needing update/merge; the index (with refreshed stat fields) was saved. |
| `9` / `LBR-WARN-001` | The local index was saved, but cloud index repair remains pending and `--exit-code-on-warning` was used. |
| `128` | Not inside a repository, a usage error (bad `--cacheinfo`, untracked path without `--add`, malformed `--index-info` line), a missing working-tree file, or `--skip-worktree`/`--no-skip-worktree` on a path that is not in the index. |
| `128` / `LBR-IO-002` | Working-tree blob or durable cloud index-marker persistence failed; fix storage permissions and retry. |

## Examples

```bash
# Build an index entry from an object id, then write the tree
OID=$(libra hash-object -w data.bin)
libra update-index --cacheinfo 100644,"$OID",assets/data.bin
libra write-tree

# A missing cacheinfo object may be registered, but write-tree will reject it
libra update-index --cacheinfo 100644,1111111111111111111111111111111111111111,missing.bin
libra write-tree   # fails with LBR-REPO-002

# Stage and unstage working-tree files
libra update-index --add src/new.rs
libra update-index --remove src/old.rs

# Mark a path index-only: status/diff ignore its working-tree copy
libra update-index --skip-worktree s.txt
libra ls-files -v          # `S s.txt`
libra update-index --no-skip-worktree s.txt

# Refresh the index stat cache against the working tree
libra update-index --refresh
libra update-index -q --refresh        # never exits 1

# Write unmerged stage entries straight from stdin
printf '100644 %s 3\tfoo\n' "$OID" | libra update-index --index-info
printf 'foo\0bar\0' | libra update-index -z --add --stdin

# Flip the executable bit without touching the working tree
libra update-index --chmod=+x script.sh
libra update-index --add --info-only staged-later.txt   # index entry only

# Stage a working-tree symlink as a 120000 link-target blob
libra update-index --add link-to-target
```

## Comparison with Git

| Task | Libra | Git |
|------|-------|-----|
| Stage a file | `libra update-index --add f` | `git update-index --add f` |
| Remove a path | `libra update-index --remove f` | `git update-index --remove f` |
| Remove a path even when the file is gone | `libra update-index --force-remove f` | `git update-index --force-remove f` |
| Mark a path skip-worktree | `libra update-index --skip-worktree f` | `git update-index --skip-worktree f` |
| Register by id | `libra update-index --cacheinfo m,oid,p` | `git update-index --cacheinfo m,oid,p` |
| Refresh stat cache | `libra update-index --refresh` | `git update-index --refresh` |
| Batch stdin records | `libra update-index --index-info` / `--stdin [-z]` | `git update-index --index-info` / `--stdin [-z]` |
| Flip executable bit | `libra update-index --chmod=(+\|-)-x` | `git update-index --chmod=(+\|-)-x` |
| Index entry without storing the object | `libra update-index --add --info-only f` | `git update-index --add --info-only f` |

Deferred (not exposed): `--assume-unchanged`/`--no-assume-unchanged`,
`--unresolve`, `--again`, and other Git flags.

### Legacy assume-unchanged bits

Libra built with git-internal 0.10.3 or earlier (Libra releases before the
issues/637 fix) wrote Git's assume-unchanged bit (`CE_VALID`, `0x8000`) on
every index entry it created, although Libra has no assume-unchanged
semantics. Libra itself ignores the bit, but Git reading such an index skips
the working-tree `stat` for those entries and misses edits and deletions.

Entries Libra creates now never carry the bit, and an entry that Libra
replaces (for example by `add` of a changed file) loses it. An existing index
keeps the bit on untouched entries after loading: reading never rewrites
flags. To normalize a whole index, run:

```bash
libra update-index --refresh      # or --really-refresh
```

Unlike Git, whose `--refresh` keeps `CE_VALID`, Libra's refresh clears the bit
from every stage-0 entry it examines, including entries it reports as
`needs update`, missing or skip-worktree. Unmerged entries (stages 1–3) are
left unchanged; resolve the conflict and refresh again. `libra add --refresh`
applies the same clearing to the paths it refreshes.

### Index v3 and downgrade note

Marking a path with `--skip-worktree` rewrites the index in Git index format
v3 (the extended-flags format upstream Git has used since 2006). Libra reads
both v2 and v3. Older Libra releases (`<= 0.23.x` for the index reader) reject
v3 indexes with an "index version is not supported" error. Before downgrading,
clear the marks so the index returns to v2:

```bash
libra ls-files -v | grep '^S ' | cut -d' ' -f2- |
  xargs -r libra update-index --no-skip-worktree
```
