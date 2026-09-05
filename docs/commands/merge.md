# `libra merge`

Merge one target into the current branch.

## Synopsis

```text
libra merge [--ff | --ff-only | --no-ff] [-s ours | -X <ours|theirs>] [--allow-unrelated-histories] [--log[=<n>] | --no-log] [--squash | --no-commit] [-m <msg>] [--no-verify] [--no-edit] [--stat | -n | --no-stat] [--verify-signatures | --no-verify-signatures] [--no-rerere-autoupdate] [--no-gpg-sign] [--dry-run] [--autostash | --no-autostash] <branch>
libra merge --continue [-m <msg>] [--no-verify]
libra merge --abort
libra merge --restart
```

## Description

`libra merge <branch>` resolves a local branch, commit hash, or remote-tracking ref such as `refs/remotes/origin/main`.

If the current branch can be fast-forwarded, Libra moves the branch pointer to the target commit and restores the index and working tree. If the branches have diverged, Libra performs a single-head three-way merge using the merge base — or, when the history leaves more than one merge base, using a recursive virtual ancestor built from all of them (see below).

The default three-way strategy accepts `-X ours` or `-X theirs`: only conflicting hunks/paths choose that side, while clean changes from both sides remain. This is different from `-s ours`, which always creates a two-parent merge commit (unless the target is already an ancestor) while retaining the entire current HEAD tree. Other strategies and strategy options are rejected during argument parsing.

### Criss-cross histories (several merge bases)

Two branches that have already merged each other leave **several** merge bases, none of them better than the others. Picking one arbitrarily reports conflicts that the history actually explains, so Libra folds them the way Git's recursive strategy does:

1. The merge bases are sorted by object id and folded pairwise, left to right, each fold being itself a three-way merge (with its own merge base, recursively).
2. The single tree that comes out — the **virtual ancestor** — is the base of the real merge.

Inside the fold, decisions differ from a real merge, matching Git:

- `-X ours` / `-X theirs` do **not** apply; a virtual ancestor is a synthetic input, not something you asked to bias.
- A content conflict is not surfaced. It is recorded *as content*, with the conflict markers widened by two characters per recursion level so a nested conflict can never be read as one the outer merge produced.
- A change/delete keeps the base version: there is no midpoint between "changed" and "gone".
- Binary content (Git's rule: a NUL byte in the first 8000, or an input larger than 1023 MiB) is not line-merged; the ancestor keeps the base's content, or the empty blob when there is no base.
- Symlinks — and any two sides that are different *kinds* of entry — keep the base version, which means the path is simply absent from the ancestor when there is no base.
- The resulting file mode follows Git's rule: the other side's mode when the two agree or ours is unchanged, otherwise ours.
- Nesting deeper than 20 levels, or more than 32 merge bases at one level — counted on the bare ids before any base is loaded, and again on the candidate ancestors collected while folding — is refused with `LBR-UNSUPPORTED-001` rather than recursed (the fold's work grows with the square of the width; a criss-cross has two). `-s ours` and `--ff-only` never fold and are never refused for width (a diverged `--ff-only` merge is refused as non-fast-forward, as always).

The synthetic commit's parents are **all** the real merge bases folded so far (Git chains one two-parent virtual commit per fold step); the reachable history is the same, the object ids are not. The virtual ancestor's tree and its synthetic commit are written as ordinary loose objects, but they are **one-shot**: they are deliberately *not* recorded in the merge state, so they are not garbage-collection roots. `libra maintenance run --task gc` may reclaim them at any time, including while a conflicted merge is still in progress — nothing depends on them surviving, and `libra merge --restart` recomputes the same ancestor from the real merge bases. Consequently `merge-state.json` records a `base` only when the merge base is a single real commit.

`libra merge --dry-run` previews a criss-cross merge under the same contract as any other preview — no object-store, index, working-tree, HEAD, reflog, merge-state or autostash-sidecar write: the fold keeps its blobs in memory and materializes no virtual tree or commit. (What the contract does *not* cover is repository-wide housekeeping the CLI performs before any command runs, such as the schema auto-upgrade on database open; see `docs/development/commands/merge.md`.)

Histories without a common ancestor remain rejected unless `--allow-unrelated-histories` is explicit. With it, Libra uses a virtual empty merge base: disjoint root trees combine normally, overlapping additions conflict normally, and conflict state survives `--continue`, `--abort`, and `--restart` without creating a fake base object.

Clean three-way merges create a two-parent merge commit, update HEAD, rebuild the index, restore the working tree, and write a merge reflog entry. Conflicting three-way merges write line-level conflict markers to the working tree (matching Git — only the diverging hunks are enclosed between `<<<<<<< HEAD` / `=======` / `>>>>>>>`, with shared context left outside; binary or modify/delete paths fall back to whole-file markers), write unmerged index stages, save Libra merge state, and return `LBR-CONFLICT-002` with hints for `libra merge --continue` and `libra merge --abort`.

### Conflict style (`merge.conflictStyle`)

The marker format follows the Git-compatible `merge.conflictStyle` config key (config-only — matching Git, `merge` has no CLI style flag):

```bash
libra config merge.conflictStyle diff3
```

- `merge` (default, or unset) — the two-marker style above.
- `diff3` — additionally emits the common-ancestor content between a `||||||| base` marker and the `=======` separator, so you can see what both sides started from.
- Any other value — including the unimplemented `zdiff3` — is a hard error when a conflict must be rendered (exit 128), never a silent fall-back to the default style.

For a merge with **several merge bases** the value is read before the merge runs rather than only when a conflict is rendered, because the recursive virtual ancestor's own content depends on it (Git does the same at every recursion depth). An invalid value therefore stops a criss-cross merge even when that merge would have come out clean.

The config is honored by both `libra merge` and `libra cherry-pick` for line-level text conflicts. Binary and modify/delete conflicts keep their two-part whole-file presentation (Git also emits no base block there), and `libra rebase` currently renders whole-file markers without a base block regardless of this setting.

### Directory/file collisions (D/F conflicts)

When one side keeps (or edits) a **file** `foo` while the other side turns `foo` into a **directory** — deleting the file and adding paths beneath `foo/` — the two cannot share one path. Libra follows Git's recursive strategy (`merge-ort.c`, `unique_path`): the directory keeps `foo`, and the file is written under a unique name derived from the branch that holds it — `foo~HEAD` when the file is ours, `foo~<branch>` (with `/` in the branch name replaced by `_`) when it is theirs; if that name is already used by any input of the merge or by its result — as a file *or* as a directory, and a path only the merge base had counts too, exactly the set Git's `unique_path` checks — `_0`, `_1`, … is appended. The merge stops as a conflict (`LBR-CONFLICT-002`) and prints Git's line first:

```text
CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD instead.
```

The moved file is the unmerged path: the index records it at the new name with the file's own side on stage 2 (ours) or stage 3 (theirs) and, when the merge base tracked a file at `foo`, that file on stage 1; there is no stage 0 entry, and `merge-state.json` lists the new name in `conflicted_paths`. The directory's contents merge normally. Two shapes exist, both as in Git: a file only one side *added* is a pure file/directory conflict — written verbatim at the new name, reported as `file-directory` by `--dry-run`; a file the merge base tracked and the file side *edited* is a modify/delete conflict that merely moves — the new name carries the base (stage 1) and the editing side, `--dry-run` reports it as `modify-delete` with `original_path`, Git's second line is printed too (`CONFLICT (modify/delete): foo~HEAD deleted in <branch> and modified in HEAD.  Version HEAD of foo~HEAD left in tree.`), and the file is left verbatim at the new name, as that line says. A file one side left untouched while the other replaced it with a directory is a plain clean deletion: no conflict, no message. A directory holding nothing but *empty* trees counts as in the way only when the merge base had nothing at that path — Git adopts such a new directory verbatim, while a directory the base already had is traversed, found to contain no file, and leaves the file where it is (a plain modify/delete). Both cases match `git merge` on crafted trees. Under `--json`/`--machine` none of these lines is printed: stdout stays machine-clean and the conflict is the error envelope on stderr. A strategy option (`-X ours` / `-X theirs`) settles content hunks only: a modify/delete under a directory stays a conflict, as in Git. The merge never writes or deletes *through* a symbolic link in the working tree — an ignored `foo -> elsewhere` standing where `foo/…` must be written, or above a tracked file the merge removes, refuses the whole merge before anything changes — while a symlink sitting exactly where the moved file goes is replaced by the file, and a *tracked* symlink `foo` giving way to a directory `foo/` moves to `foo~HEAD` like any file. Only paths the merge tracks right now are ever removed: an untracked file that happens to carry a historical name stays. Resolve it by editing the `foo~…` file if you want to keep it, staging it with `libra add foo~…` and running `libra merge --continue`; or run `libra merge --abort`, which restores `foo` as it was before the merge and removes both the moved copy and the directory the merge created (directories the merge emptied are pruned, so a nested `foo/a/` never blocks the file's return). Discarding the moved file instead of keeping it is not yet possible: `libra rm` does not accept an unmerged path, so the only way to end up without it is `--abort` (Git resolves that case with `git rm foo~HEAD`).

A directory that merges to *nothing* (every entry beneath it was deleted by the file's side and untouched by the other) is not in the way: the file keeps its path, as in Git. Inside a recursive criss-cross fold (see above) the same rule applies without asking: the file moves to `foo~Temporary merge branch 1` (or `2`) in the virtual ancestor, exactly as Git does at `call_depth > 0`, so the ancestor tree never holds a blob and a subtree under one name.

Only collisions between the two sides' *tracked* contents are handled here. An **untracked** `foo` or `foo~HEAD` in the working tree that the merge would overwrite is refused up front by the existing untracked-overwrite check. An **ignored** file is expendable and gets replaced, exactly as `git merge` treats it — whether it sits at one of these names or where the merge has to create a directory. If you keep something precious under an ignore rule at a path a merge may write, it is not protected. Collisions created by rename detection are out of scope until rename support lands.

### History-changing merge defaults

When the corresponding CLI flag is absent, Libra reads these Git-compatible defaults through the local → global → system cascade:

- `merge.ff=true|false|only` allows fast-forwarding, forces a two-parent merge commit, or rejects a non-fast-forward merge. `--ff`, `--no-ff`, and `--ff-only` override it. `only` (like `--ff-only`) rejects only a genuinely diverged history: a fast-forwardable `--squash` or `--no-commit` is still allowed, matching Git.
- `merge.log=true|false|<n>` appends up to 20 (for `true`) or `<n>` target-side commit subjects to the generated merge message. `--log[=<n>]` and `--no-log` override config and are last-one-wins; bare `--log` means 20. An explicit `-m` suppresses config-only `merge.log`, while an explicit `--log` still appends the shortlog to the custom message. The resolved message is recorded in merge state, so a merge finished later with `merge --continue` commits with the same message and shortlog.
- `merge.verifySignatures=true|false` controls tip-signature verification; `--verify-signatures` and `--no-verify-signatures` override it. Verification runs on the resolved target before any mutation — including autostash creation — so a rejected merge writes nothing (no stash entry, no objects).

Invalid or unreadable local/global values fail before HEAD, index, worktree, or merge-state mutation (`LBR-CLI-002` or `LBR-IO-001`). Encrypted local/global values are decrypted; unreadable or unsupported system scope is skipped. Exception: a global config store whose schema is newer than this Libra binary is skipped with a one-time deduplicated warning instead of failing (see `LBR-CONFIG-001`).

### Submodules (`160000` gitlink entries)

Libra is a monorepo client and never merges submodule content. A three-way merge treats gitlinks in two tiers:

- **The merge would have to arbitrate the gitlink** — any side records a different commit id than the merge base, including a side that added or removed the entry. The merge is refused **before anything is written** (no merge state, no index or working-tree change, HEAD unmoved) with `LBR-UNSUPPORTED-001` naming the path:

  ```
  error: merge would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules
  ```

  Resolve the submodule pointer outside Libra, or drop the gitlink entry from the branches being merged.

- **All three sides record the same commit id** — nothing has to be decided, so the pointer is carried into the merge result verbatim. (Previously such entries were silently dropped, which deleted the submodule from the merged tree.)

`libra rebase` and `libra cherry-pick` share the same guard and the same wording, with `rebase` / `cherry-pick` in place of `merge`.

Libra still does not implement octopus merges, merge strategies other than `ours`, strategy options other than `ours`/`theirs`, or interactive message editing (`--edit`/launching an editor). Signature verification (`--verify-signatures`) is supported but limited to the local vault PGP key (no external GPG keyring).

## Options

| Option | Description |
|--------|-------------|
| `<branch>` | Target branch, commit, or remote-tracking ref to merge. |
| `-m, --message <MSG>` | Override the merge commit message (default `Merge <branch> into <head>`). Also accepted with `--continue`, where it overrides the message recorded when the conflicted merge started — a Libra extension, since Git's `--continue` takes no arguments and Libra never opens an editor for merge. |
| `--ff` | Allow fast-forwarding when possible, overriding `merge.ff=false|only`. |
| `--ff-only` | Refuse to merge unless the current branch can be fast-forwarded. |
| `--no-ff` | Always create a two-parent merge commit, even when a fast-forward is possible. |
| `-s ours`, `--strategy=ours` | Record the merge relationship with two parents while retaining the complete current HEAD tree. Distinct from `-X ours`. Other strategies are rejected. |
| `-X ours`, `-X theirs`, `--strategy-option=<ours\|theirs>` | Resolve only conflicting hunks/paths in favor of that side; clean changes from both sides are retained. Repeatable; the last value wins. Cannot be combined with `-s ours`. |
| `--allow-unrelated-histories` | Permit histories with no common ancestor by using a virtual empty merge base. The permission is persisted for conflict `--restart`. |
| `--log[=<N>]` | Append up to N target-side subjects to the merge message; bare `--log` uses 20. Overrides `merge.log`, and also appends to an explicit `-m` message. Last-one-wins with `--no-log`. |
| `--no-log` | Disable the merge-message shortlog, overriding `merge.log` and an earlier `--log`. |
| `--squash` | Produce the merged index/working tree but create no commit and do not move HEAD; finish with a plain `libra commit`. |
| `--no-commit` | Perform the merge and stage the result but stop before committing; finish with `libra merge --continue`. |
| `--no-verify` | Skip all `.libra/hooks` for this merge. With `--continue`, bypass the pending commit/message/post hooks. |
| `--no-edit` | Accept the auto-generated merge message without launching an editor. Libra never opens an editor for merge, so this is a no-op accepted for Git parity. |
| `--stat` | Show a diffstat of the merge result (the changes between the pre-merge HEAD and the new commit) after the merge completes. Git shows this by default; Libra defaults to no diffstat, so `--stat` opts in. Last-one-wins toggle with `--no-stat`/`-n`. Human output only. |
| `-n`, `--no-stat` | Do not show a diffstat at the end of the merge (Libra's default). Last-one-wins toggle with `--stat`. |
| `--no-progress` | Do not show a progress meter. No-op accepted for Git parity: Libra's merge never renders a progress meter. |
| `--verify-signatures` | Verify the PGP signature on the tip commit and abort if it is unsigned or bad. Overrides `merge.verifySignatures`; only signatures made by this repository's vault PGP key can be validated. |
| `--no-verify-signatures` | Do not verify the merged commit's signature, overriding `merge.verifySignatures=true`. The inverse of `--verify-signatures`; the last one wins. |
| `--no-rerere-autoupdate` | Accepted for Git parity. Rerere IS integrated: with `rerere.enabled`, a conflicted merge records each conflict's preimage and replays a recorded resolution when one matches; auto-staging of replayed files follows the `rerere.autoUpdate` config. The per-invocation override is not implemented — staging follows the config either way. (Git's positive `--rerere-autoupdate` is not exposed.) |
| `--no-gpg-sign` | Do not GPG-sign the merge commit. No-op accepted for Git parity: Libra's merge never signs. (Git's `-S`/`--gpg-sign` is not implemented.) |
| `--continue` | Finish an in-progress merge after conflicts have been resolved and staged. |
| `--abort` | Restore the pre-merge HEAD, index, and working tree (re-applies a held autostash). |
| `--autostash` / `--no-autostash` | Stash local tracked changes before the merge and restore staged index and unstaged worktree layers separately when it concludes — held (outside `stash list`) across a conflict until `--continue`/`--abort`; a conflicting re-apply is saved to the stash list with a notice, never lost. Config: `merge.autostash` (boolean; invalid value = hard error). Untracked files are not stashed (Git parity). `--json` adds `autostash: applied\|stashed\|kept`. |
| `--dry-run` | Libra extension: preview the merge outcome writing **nothing** — reports fast-forward / already-up-to-date / clean three-way / would-conflict (with the paths). Exits 0 for a clean preview, 1 when the merge would conflict. Mutually exclusive with `--continue`/`--abort`/`--restart`/`--squash`/`--no-commit`. |
| `--restart` | Libra extension (ports Lore's `branch merge restart`): abort the in-progress conflicted merge — discarding any resolution work, exactly like `--abort` — then immediately re-run the same merge against the recorded target commit, regenerating fresh conflict markers and state. Takes no branch and no merge options. Recovery-critical `--allow-unrelated-histories` is replayed; presentation/policy options such as the original `-m`/`--no-ff` are not. Requires a **conflicted** merge: a staged `--no-commit` merge is refused (finish it with `--continue` or discard with `--abort`). |
| `--json` | Emit a structured success envelope. |
| `--machine` | Emit the same structured envelope as one compact JSON line. |
| `--quiet` | Suppress human success output. |

## Repository hooks

`pre-merge-commit` blocks an automatic merge commit (including `--continue`) but does not
run for fast-forward, squash, or a pending `--no-commit` result. Automatic merge commits
then run `prepare-commit-msg <file> merge`, `commit-msg <file>`, and advisory
`post-commit`; message hooks may modify `.libra/COMMIT_EDITMSG`. `post-merge` runs
advisory after a completed merge/fast-forward with argument `0`, or after squash with
argument `1`; it does not run for already-up-to-date or conflicted outcomes.
`--no-verify` skips every hook in that merge lifecycle. Pull uses the same lifecycle; use
`LIBRA_NO_HOOKS=1` when an explicit bypass is required. See
[Repository hooks](repository-hooks.md) for the sandbox and failure contract.

## Common Commands

```bash
libra merge feature-x
libra merge -X ours feature-x
libra merge -s ours obsolete-history
libra merge --allow-unrelated-histories imported-root
libra merge --log=10 feature-x
libra merge refs/remotes/origin/main
libra merge --continue
libra merge --continue -m "merge: reconcile release notes"
libra merge --abort
libra merge --dry-run feature-x
libra merge --restart
libra merge --json feature-x
```

## Conflict Lifecycle

When a merge conflicts:

1. Edit files containing conflict markers.
2. Stage each resolved path with `libra add <path>`.
3. Run `libra merge --continue` to create the two-parent merge commit.

Run `libra merge --abort` before continuing to restore the branch, index, and working tree to the pre-merge commit. `libra status` shows the in-progress merge target and the continue/abort commands while merge state exists.

To throw away a botched resolution attempt and start over in one step, run `libra merge --restart`: it restores the pre-merge state exactly like `--abort` (any edits to conflicted files are **discarded**) and immediately re-runs the same merge against the recorded target commit — deterministic even if the branch has moved since — leaving fresh conflict markers and a fresh merge state. The re-run uses default merge options.

## Dry Run

`libra merge --dry-run <branch>` (a Libra extension — Git has no true merge dry-run) reports what the merge *would* do without writing anything: no HEAD, index, working-tree, reflog, merge-state, or object-store mutation (auto-merged blobs are computed in memory only). Because it is read-only it also works on a dirty working tree — note the preview does not validate cleanliness, so a real merge may still refuse where the preview succeeded.

Outcomes and exit codes:

| Preview outcome | Human output | Exit |
|-----------------|--------------|------|
| Fast-forward possible | `Would fast-forward` | 0 |
| Already up to date | `Already up to date.` | 0 |
| Clean three-way/ours merge | `Would merge cleanly by the '<strategy>' strategy.` | 0 |
| Would conflict | `Would conflict in: <paths>` (a directory/file collision is listed under the `~`-suffixed name the file would be moved to) | 1 |

The would-conflict exit of 1 is an outcome signal (like `merge-file` and `diff --exit-code`), deliberately distinct from the 128 a *real* conflicting merge exits with — the preview itself succeeded. With `--json`/`--machine` the summary carries `"dry_run": true` and, when conflicting, `"would_conflict": true` plus `conflicted_paths` and `conflict_kinds` — one `{"path", "kind", "original_path"?}` object per conflicted path, `kind` being `content`, `modify-delete` or `file-directory`, with `original_path` present only for a directory/file move (the path the file held before; `path` is then the `~`-suffixed name it would be written to, and `conflicted_paths` lists that same name). All of these keys are absent from every real merge's output (frozen schema).

## Human Output

Fast-forward:

```text
Fast-forward
```

Clean three-way merge:

```text
Merge made by the 'three-way' strategy.
```

Ours strategy:

```text
Merge made by the 'ours' strategy.
```

Already up to date:

```text
Already up to date.
```

After `--continue`:

```text
Merge completed.
```

After `--abort`:

```text
Merge aborted.
```

Conflict errors are printed through Libra's standard structured error envelope on stderr and include recovery hints.

## JSON / Machine Output

Success output keeps the historical `files_changed` numeric field and adds merge-lifecycle fields only when relevant.

```json
{
  "ok": true,
  "command": "merge",
  "data": {
    "strategy": "three-way",
    "old_commit": "abc1234...",
    "commit": "def5678...",
    "files_changed": 2,
    "up_to_date": false,
    "parents": ["abc1234...", "fedcba9..."]
  }
}
```

`-s ours` uses `strategy: "ours"`, `files_changed: 0`, and reports both parents. Already-up-to-date merges use `strategy: "already-up-to-date"`, `commit: null`, `files_changed: 0`, and `up_to_date: true`.

`--abort` sets `aborted: true`; `--continue` sets `continued: true`. Conflict failures return an error envelope on stderr with `LBR-CONFLICT-002`. `--dry-run` adds `dry_run`, `would_conflict`, `conflicted_paths` and `conflict_kinds` (see Dry Run).

## Parameter Comparison: Libra vs Git vs jj

| Parameter | Libra | Git | jj |
|-----------|-------|-----|----|
| Branch target | `<branch>` (single target) | `<commit>...` (one or more) | N/A (use `jj new`) |
| Fast-forward | Supported | Supported | N/A |
| Single-head three-way | Supported | Supported | N/A |
| Criss-cross (several merge bases) | Recursive virtual ancestor (fold order: ascending object id; max depth 20, max 32 bases per level) | Recursive virtual ancestor (`-s recursive`/`ort`, unbounded depth) | N/A |
| Continue / abort | `--continue`, `--abort` | `--continue`, `--abort` | N/A |
| Octopus merge | Not supported | Supported | N/A |
| Fast-forward only | `--ff-only` | `--ff-only` | N/A |
| Force merge commit | `--no-ff` | `--no-ff` | N/A |
| Squash | `--squash` | `--squash` | N/A |
| No-commit | `--no-commit` | `--no-commit` | N/A |
| Skip all merge-lifecycle hooks | `--no-verify` | `--no-verify` | N/A |
| Commit message | `-m <msg>` | `-m <msg>` | N/A |
| No editor | `--no-edit` (no-op; never edits) | `--no-edit` | N/A |
| Post-merge diffstat | `--stat` (prints it); `-n` / `--no-stat` (default: omit) | `--stat` (default) / `-n` / `--no-stat` | N/A |
| No progress meter | `--no-progress` (no-op; never renders one) | `--no-progress` | N/A |
| Disable signature verification | `--no-verify-signatures` (default; disables `--verify-signatures`) | `--no-verify-signatures` | N/A |
| No rerere autoupdate | `--no-rerere-autoupdate` (accepted; staging follows `rerere.autoUpdate`) | `--no-rerere-autoupdate` | N/A |
| No GPG sign | `--no-gpg-sign` (no-op; never signs) | `--no-gpg-sign` | N/A |
| Ours strategy | `-s ours` | `-s ours` | N/A |
| Conflict-side preference | `-X ours/theirs` | `-X ours/theirs` | N/A |
| Unrelated histories | `--allow-unrelated-histories` | Supported | N/A |
| Merge-message shortlog | `--log[=<n>]` / `--no-log` | Supported | N/A |
| Other custom strategies/options | Not supported | Supported | N/A |
| Verify signatures | `--verify-signatures` (vault-key PGP only) | `--verify-signatures` | N/A |
| JSON output | `--json` / `--machine` | Not supported | N/A |

## Error Handling

| Scenario | StableErrorCode | Exit |
|----------|-----------------|------|
| Missing branch / action | `LBR-CLI-001` | 129 |
| Target ref cannot be resolved | `LBR-CLI-003` | 129 |
| Failed to load merge target/current commit/tree | `LBR-REPO-002` | 128 |
| Unrelated histories without `--allow-unrelated-histories` | `LBR-REPO-003` | 128 |
| Three-way merge would have to arbitrate a `160000` gitlink (submodule) | `LBR-UNSUPPORTED-001` | 128 |
| Recursive virtual ancestor would nest deeper than 20 levels or fold more than 32 bases at one level | `LBR-UNSUPPORTED-001` | 128 |
| Unsupported `-s` / `-X` value or incompatible strategy combination | `LBR-CLI-002` | 129 |
| `--verify-signatures`: tip unsigned, signature invalid, or vault unavailable | `LBR-REPO-003` | 128 |
| Merge conflicts | `LBR-CONFLICT-002` | 128 |
| Dirty worktree or staged changes | `LBR-CONFLICT-002` | 128 |
| Untracked file would be overwritten | `LBR-CONFLICT-002` | 128 |
| Merge already in progress | `LBR-CONFLICT-002` | 128 |
| No merge in progress for `--continue` / `--abort` | `LBR-REPO-003` | 128 |
| Unsupported `merge.conflictStyle` value (e.g. `zdiff3`) when rendering a conflict | `LBR-REPO-003` | 128 |
| Unresolved conflict stages remain for `--continue` | `LBR-CONFLICT-002` | 128 |
| Failed to read merge state or index | `LBR-IO-001` | 128 |
| Failed to save state, index, tree, commit, HEAD, or worktree | `LBR-IO-002` | 128 |
