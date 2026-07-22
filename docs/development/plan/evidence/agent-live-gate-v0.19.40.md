# Live agent gate evidence — M6 (`v0.19.40` post-release)

> plan-20260713「本机 live agent 执行验证门」固定字段记录。sanitized-only；
> provider session ids、home/source paths、transcript text、digests and raw command output are omitted.

- release/tag: `v0.19.40`
- commit: `8d6c5b58cc05cc19c24f0cc7abfb63cb1dadeeed`
- tree: `322c254d7a1023da30b0eaabeb947a71ad954ec5`
- UTC time: 2026-07-22T01:51:45Z
- providers: real local `claude-code` 2.1.216, `codex` 0.144.6, and
  `opencode` 1.17.18 captures already imported into the current repository
- scope: M6 = DR-07 capture-only `libra agent graph` TUI/JSON projection,
  legacy compatibility, explicit subagent link state, privacy whitelist,
  non-TTY refusal, and zero-write behavior
- post-release commands:
  - `LIBRA_RUN_LIVE_AGENT_GATE=1 cargo test --features test-live-agent
    --test agent_live_gate_test live_m6_agent_graph_real_capture_is_private_and_readonly
    -- --exact --nocapture --test-threads=1`
  - installed `/home/eli/.libra/bin/libra --version`
  - installed `/home/eli/.libra/bin/libra agent graph --help`
- sanitized aggregate results:
  - the gated live test passed after `origin/main` advanced to the release
    commit; provider/source absence is a failure, not a skip
  - real captured Claude Code, Codex, and OpenCode projections retained
    non-empty indexed turn/revision structures under frozen JSON schema v1
  - the real subagent projection retained the observed unresolved link without
    fabricating a boundary checkpoint
  - forbidden raw metadata/path/blob/digest fields and the repository path were
    absent from JSON output
  - non-TTY without `--json`/`--machine` was rejected before TUI initialization
    with `LBR-CLI-002`
  - row-for-row snapshots of all 10 capture/import/export catalog tables were
    identical before and after graph/refusal paths
  - erased display/non-resurrection remains pinned by the deterministic L1
    tombstone fixture; no operator-owned live capture was erased for this gate
- release verification:
  - installed binary reports `libra 0.19.40`; Cargo, lockfile, web, and worker
    version surfaces all report `0.19.40`
  - `origin/main` resolves to the release commit above
  - the clean published commit and the fully reviewed pre-publication release
    have the identical tree OID above; the clean commit excludes malformed
    unpublished stash-derived commit `14fb7c2`
  - focused graph 11/11, migration 35/35, all registered compatibility targets
    (serialized), fmt, all-target/all-feature clippy with `-D warnings`, and
    `cargo build --release` passed on the published tree
- stable result: success (1 post-release gated M6 test passed, 0 failed; installed
  public help/version present; zero catalog mutations)
