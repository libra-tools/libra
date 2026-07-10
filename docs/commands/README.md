# Libra Command Reference

This directory contains detailed documentation for all Libra CLI commands. Each document includes a synopsis, option reference, human and structured (JSON) output examples, design rationale, and a parameter comparison with Git and jj.

Compatibility is tracked at two levels: each command page describes the
user-facing behavior of that command, while
[`COMPATIBILITY.md`](../../COMPATIBILITY.md#sub-face-compatibility-grading-p0p1-touched-commands)
grades the P0/P1 command surface by sub-face (`common-user-flow`,
`porcelain-machine`, `conflict-aware`, `config-aware`, and
`plumbing-compatible`). Use the sub-face table when a script depends on a
specific Git-compatible surface such as porcelain output, conflict handling, or
plumbing syntax.

## Global Flags

Every Libra command accepts the following global flags:

| Flag | Short | Description |
|------|-------|-------------|
| `--json` | `-J` | Output as JSON (formats: `pretty`, `compact`, `ndjson`) |
| `--machine` | | Strict machine mode (implies `--json=ndjson --no-pager --color=never --quiet`) |
| `--no-pager` | | Disable pager (`less`) |
| `--color` | | When to use colors (`auto`, `never`, `always`) |
| `--no-color` | | Disable colors; equivalent to `--color=never` |
| `--quiet` | `-q` | Suppress stdout |
| `--exit-code-on-warning` | | Return exit code 9 on warnings |
| `--progress` | | Control progress output (`json`, `text`, `none`, `auto`) |

## Command Index

### Repository Setup

| Command | Alias | Description | Doc |
|---------|-------|-------------|-----|
| `libra init` | | Create a new Libra repository with SQLite-backed metadata, vault signing, and optional Git import | [init.md](init.md) |
| `libra clone` | | Clone a remote repository with vault bootstrapping, shallow clone, and single-branch support | [clone.md](clone.md) |
| `libra config` | `cfg` | Manage repository-local and user-global configuration with vault-backed secret encryption | [config.md](config.md) |
| `libra completions` | | Generate a shell completion script (`bash`/`zsh`/`fish`/`powershell`/`elvish`) from the live CLI | [completions.md](completions.md) |

### Staging & Working Tree

| Command | Alias | Description | Doc |
|---------|-------|-------------|-----|
| `libra add` | | Stage file changes from the working tree into the index | [add.md](add.md) |
| `libra rm` | `remove`, `delete` | Remove files from the working tree and/or the index | [rm.md](rm.md) |
| `libra mv` | | Move or rename files, directories, or symlinks | [mv.md](mv.md) |
| `libra restore` | `unstage` | Restore working tree files or unstage changes from the index | [restore.md](restore.md) |
| `libra clean` | | Remove untracked files from the working tree (requires `-n` or `-f`) | [clean.md](clean.md) |
| `libra stash` | | Save and restore temporary changes with push/pop/list/apply/drop subcommands | [stash.md](stash.md) |
| `libra status` | `st` | Show the state of the working tree, staging area, and upstream tracking | [status.md](status.md) |
| `libra dirty` | | Advisory dirty-set marks for the status cache (Libra extension) | [dirty.md](dirty.md) |
| `libra revision` | | Revision ordinal index over first-parent chains (Libra extension) | [revision.md](revision.md) |
| `libra commit-tree` | `git commit-tree` | Create a commit object from a tree (plumbing) | [commit-tree.md](commit-tree.md) |
| `libra auth` | | Host-scoped HTTP token auth (Libra extension) | [auth.md](auth.md) |
| `libra service` | | Headless local service: notification bus + dirty-mark ingestion (Libra extension) | [service.md](service.md) |

### Commits & History

| Command | Alias | Description | Doc |
|---------|-------|-------------|-----|
| `libra commit` | `ci` | Record staged changes as a new commit with optional vault signing and conventional format | [commit.md](commit.md) |
| `libra log` | `hist`, `history` | Show commit history with graph, patch, stat, and custom format support | [log.md](log.md) |
| `libra logfile` | | Inspect the tracing log-file configuration (path, rotation, filter, size) | [logfile.md](logfile.md) |
| `libra shortlog` | `slog` | Summarize reachable commits grouped by author | [shortlog.md](shortlog.md) |
| `libra show` | | Display a commit, tag, tree, blob, or `REV:path` content | [show.md](show.md) |
| `libra diff` | | Compare differences between HEAD, index, working tree, or two revisions | [diff.md](diff.md) |
| `libra diff-tree` | | Diff between two trees (git diff-tree) | [diff-tree.md](diff-tree.md) |
| `libra diff-index` | | Diff a tree against the working tree (git diff-index) | [diff-index.md](diff-index.md) |
| `libra diff-files` | | Diff the index against the working tree (git diff-files) | [diff-files.md](diff-files.md) |
| `libra fast-export` | | Emit history as a git fast-import stream | [fast-export.md](fast-export.md) |
| `libra fast-import` | | Import a git fast-import stream | [fast-import.md](fast-import.md) |
| `libra blame` | | Trace each line of a file to its introducing commit | [blame.md](blame.md) |
| `libra describe` | `desc` | Find the nearest reachable tag and format as `tag-N-g<abbrev>` | [describe.md](describe.md) |
| `libra grep` | | Search for patterns in tracked files with regex, revision, and index support | [grep.md](grep.md) |
| `libra reflog` | | View, delete, or check existence of reference change logs | [reflog.md](reflog.md) |
| `libra rev-list` | | List commit objects reachable from a revision | [rev-list.md](rev-list.md) |
| `libra rev-parse` | | Parse revision names, abbreviate refs, and print repository paths | [rev-parse.md](rev-parse.md) |

### Branching & Navigation

| Command | Alias | Description | Doc |
|---------|-------|-------------|-----|
| `libra branch` | `br` | Create, delete, rename, list, and inspect branches | [branch.md](branch.md) |
| `libra metadata` | | Branch/repo metadata key-value store (protect/archive/lineage foundation) | [metadata.md](metadata.md) |
| `libra tag` | | Create, list, or delete lightweight and annotated tags | [tag.md](tag.md) |
| `libra switch` | `sw` | Switch branches, create new branches, or detach HEAD with fuzzy suggestions | [switch.md](switch.md) |
| `libra checkout` | | Branch compatibility surface and explicit `--` path-restore alias; prefer `switch` / `restore` | [checkout.md](checkout.md) |

### History Manipulation

| Command | Alias | Description | Doc |
|---------|-------|-------------|-----|
| `libra reset` | | Move HEAD and optionally reset index or working directory | [reset.md](reset.md) |
| `libra merge` | | Fast-forward merge a branch into the current branch | [merge.md](merge.md) |
| `libra merge-file` | | Three-way merge of three files (git merge-file) | [merge-file.md](merge-file.md) |
| `libra merge-base` | | Find the best common ancestor(s) of two commits | [merge-base.md](merge-base.md) |
| `libra rebase` | `rb` | Reapply commits on top of another base tip with conflict resolution | [rebase.md](rebase.md) |
| `libra cherry-pick` | `cp` | Apply changes from existing commits onto the current branch | [cherry-pick.md](cherry-pick.md) |
| `libra revert` | | Create a new commit that undoes changes from a specified commit | [revert.md](revert.md) |
| `libra replace` | | Substitute one object for another on read (refs/replace) | [replace.md](replace.md) |
| `libra rerere` | | Reuse recorded conflict resolutions | [rerere.md](rerere.md) |
| `libra bisect` | | Binary search to find the commit that introduced a bug; supports `start` / `bad` / `good` / `reset` / `skip` / `log` / `run` / `view` | [bisect.md](bisect.md) |
| `libra bundle` | | Create and inspect Git v2 bundle files (`create` / `verify` / `list-heads`) | [bundle.md](bundle.md) |

### Remote Operations

| Command | Alias | Description | Doc |
|---------|-------|-------------|-----|
| `libra remote` | | Manage remote repositories: add, remove, rename, inspect URLs, prune stale refs | [remote.md](remote.md) |
| `libra fetch` | | Download objects and update remote-tracking refs from one or all remotes | [fetch.md](fetch.md) |
| `libra ls-remote` | | List references advertised by a remote repository without fetching objects | [ls-remote.md](ls-remote.md) |
| `libra push` | | Send local commits and objects to a remote with LFS integration | [push.md](push.md) |
| `libra pull` | | Fetch and fast-forward merge into the current branch | [pull.md](pull.md) |
| `libra open` | | Open the repository's remote URL in the system browser | [open.md](open.md) |
| `libra lfs` | | Manage Large File Storage: track, lock, unlock, list LFS files | [lfs.md](lfs.md) |
| `libra credential` | | Vault-backed Git credential helper (fill/store/erase) | [credential.md](credential.md) |
| `libra login` | | Authenticate to a Libra host (`/api/cli/*`) and store a host-scoped session token | [login.md](login.md) |
| `libra logout` | | Clear stored Libra host session tokens (`--all` / `--local-only`) | [logout.md](logout.md) |
| `libra whoami` | | Report the identity for a stored Libra host session token | [whoami.md](whoami.md) |

### Cloud & Storage

| Command | Alias | Description | Doc |
|---------|-------|-------------|-----|
| `libra cloud` | | Cloud backup and restore operations via Cloudflare D1/R2 | [cloud.md](cloud.md) |
| `libra cache` | | Inspect the tiered-storage / LRU cache configuration (type, threshold, budget) | [cache.md](cache.md) |
| `libra publish` | | Manage read-only Cloudflare Worker publishing | [publish.md](publish.md) |
| `libra worktree` | `wt` | Manage multiple working trees attached to the repository | [worktree.md](worktree.md) |

### AI & Development

| Command | Alias | Description | Doc |
|---------|-------|-------------|-----|
| `libra code` | | Interactive TUI with AI agent, web server, and MCP integration | [code.md](code.md) |
| `libra code-control` | | Drive a local Libra Code TUI automation control session | [code-control.md](code-control.md) |
| Codex data storage | | Link `libra code --provider codex` to Codex app-server and persist Codex session data | [codex-data-storage.md](codex-data-storage.md) |
| `libra automation` | | List, run, and inspect AI automation rules | [automation.md](automation.md) |
| `libra usage` | | Report and prune AI provider/model usage aggregates | [usage.md](usage.md) |
| `libra graph` | | Inspect a Libra Code thread version graph in a dedicated TUI | [graph.md](graph.md) |
| `libra sandbox` | | Inspect AI sandbox diagnostics, including OS backend availability and downgrade warnings | [sandbox.md](sandbox.md) |
| `libra agent` | | Manage external-agent capture, checkpoints, hooks, and RPC adapters | [agent.md](agent.md) |

### Low-Level & Inspection

| Command | Alias | Description | Doc |
|---------|-------|-------------|-----|
| `libra apply` | | Check whether a unified-diff patch applies (`--check`) | [apply.md](apply.md) |
| `libra cat-file` | | Inspect Git objects and AI objects by type, size, or pretty-printed content | [cat-file.md](cat-file.md) |
| `libra check-attr` | | Report Git/Libra attributes (e.g. `filter`, `diff`, `export-ignore`) for pathnames | [check-attr.md](check-attr.md) |
| `libra check-mailmap` | | Resolve `Name <email>` contacts through `.mailmap` | [check-mailmap.md](check-mailmap.md) |
| `libra check-ignore` | | Report which pathnames are excluded by Git/Libra ignore rules | [check-ignore.md](check-ignore.md) |
| `libra fsck` | | Verify the integrity of objects, refs, and index in a Libra repository | [fsck.md](fsck.md) |
| `libra hash-object` | | Compute Git-compatible blob object IDs from files or standard input | [hash-object.md](hash-object.md) |
| `libra write-tree` | | Write the current index out as a tree object | [write-tree.md](write-tree.md) |
| `libra read-tree` | | Read a tree object into the index (index-only) | [read-tree.md](read-tree.md) |
| `libra update-index` | | Modify the index directly (add/remove/cacheinfo) | [update-index.md](update-index.md) |
| `libra update-ref` | | Safely update, create, or delete a refs/heads/<branch> ref | [update-ref.md](update-ref.md) |
| `libra verify-pack` | | Validate pack index files against their pack archives | [verify-pack.md](verify-pack.md) |
| `libra show-ref` | | List local refs (branches, tags, HEAD) and their object IDs | [show-ref.md](show-ref.md) |
| `libra symbolic-ref` | | Read or update the symbolic HEAD ref | [symbolic-ref.md](symbolic-ref.md) |
| `libra index-pack` | | Build a `.idx` pack index file for an existing `.pack` archive (hidden) | [index-pack.md](index-pack.md) |
| `libra hooks` | | External AI agent (Claude Code / Gemini) hook entry point; called by configs installed by `libra agent enable` (hidden) | [hooks.md](hooks.md) |

## Structured Output Envelope

All commands that support `--json` / `--machine` return a consistent JSON envelope:

```json
{
  "ok": true,
  "command": "<command-name>",
  "data": { ... }
}
```

On error:

```json
{
  "ok": false,
  "command": "<command-name>",
  "error": {
    "code": "LBR-XXX-NNN",
    "message": "Human-readable error description",
    "hint": "Suggested fix or next step"
  }
}
```

## Error Code Namespaces

| Prefix | Domain |
|--------|--------|
| `LBR-REPO-*` | Repository state errors (not a repo, corrupt objects, missing refs) |
| `LBR-CLI-*` | CLI argument validation errors (invalid flags, missing required args) |
| `LBR-NET-*` | Network and transport errors (auth failure, timeout, DNS) |
| `LBR-FS-*` | Filesystem errors (permission denied, disk full, path encoding) |
| `LBR-IDX-*` | Index/staging area errors (corrupt index, lock contention) |
| `LBR-OBJ-*` | Object storage errors (missing object, hash mismatch) |
| `LBR-VAULT-*` | Vault and encryption errors (unseal failure, key generation) |

## Design Philosophy

Libra's command-line interface is designed with these principles:

1. **Git compatibility where it makes sense** — Most commands mirror Git's flag names and behavior so existing muscle memory transfers directly.
2. **Structured output as a first-class citizen** — `--json` and `--machine` are global flags, and structured output is enabled command-by-command as each surface is modernised. Individual command pages document the currently stable machine-readable contract.
3. **SQLite over flat files** — Refs, config, and metadata are stored in SQLite for transactional consistency and atomic updates.
4. **Security by default** — Vault-backed signing and secret encryption are enabled by default, not opt-in.
5. **Explicit over implicit** — Commands like `clean` require `-f` or `-n`; `status --exit-code` is an explicit opt-in rather than Git's ambiguous exit code behavior.
6. **Actionable errors** — Every error includes a stable code (`LBR-*`), a human-readable message, and a hint for resolution.
7. **AI-native development** — The `libra code` command integrates AI agents directly into the version control workflow with multi-provider support and MCP protocol.
8. **Cloud-native storage** — Built-in tiered storage (S3/R2) and cloud backup (D1/R2) for distributed monorepo workflows.
