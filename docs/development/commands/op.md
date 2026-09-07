# `libra op` Development Notes

## Command Goal

`libra op` exposes Libra's command-level operation history. It is a Libra-native
extension rather than a Git command. The current public surface supports:

- `libra op log`
- `libra op show`
- `libra op restore`

## Compatibility

- Tier: `intentionally-different`.
- Rationale: Git has reflog and reset/restore flows, but it does not expose this
  Libra operation-graph model or the command-level restore view used here.

## Implementation

- CLI entry: `src/cli.rs::Commands::Op`.
- Command implementation: `src/command/op.rs`.
- Storage/service layer: `src/internal/operation.rs`.
- Transaction wrapper: `src/internal/operation_wrapper.rs`.
- Operation tables are part of the bootstrap schema and are also ensured by the
  explicit database upgrade path for older repositories.

## Operation Log v2 mutation boundary

The M2/M3 implementation uses the sidecar-only working-copy pointer and does
not add a `change-id` commit header, rewrite Git commit OIDs, bump versions, or
create release artifacts. The operation middleware classifies every mutation
surface as one of `WorkspaceMutation`, `RepoMutation`, `SequencerMutation`,
`LibraStateMutation`, `ExternalOrUnknown`, `ReadOnly`, or `InternalWorker`.
Unknown classifications fail closed; read-only commands and internal workers
do not create operations. Agent shell and external VCS tools must provide
verified before/after evidence before they can be admitted.

The v2 snapshot facets capture the raw index byte-for-byte together with
sequencer and sparse-view state. `WorkspaceSnapshotV2` excludes ignored files
by default and records bounded scans as partial rather than presenting an
incomplete snapshot as complete. The focused contracts are registered in
`tests/INDEX.md`; the full restore engine remains OL-10.

## Current Behavior

- `op log` lists operations by repository with pagination and exact command
  filtering.
- `op show` resolves an operation id or `@{n}` reference and can print the
  captured view snapshot.
- `op restore` restores HEAD and captured branch refs from a previous operation
  view and records a new successful restore operation. It also **prunes** local
  branches that are absent from the target view, so restore reproduces that
  operation's exact local-branch set rather than only updating named refs. Never
  pruned: the restored HEAD branch, remote-tracking refs, the locked branches
  (`main`/`intent`/`traces`/`agent-traces`), and the reserved `libra/` namespace
  (AI history `libra/intent`, orchestrator `libra/src`/`libra/target`).
  `--dry-run` previews the prune (and the restore) without writing.

## Remaining Gaps

- Broader command coverage is incremental. At present, branch creation is wired
  through operation logging as the first command integration target.
