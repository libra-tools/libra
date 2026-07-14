# Live agent gate evidence — M3 (pre-review)

> plan-20260713「本机 live agent 执行验证门」固定字段记录。sanitized-only。

- release/tag: `pre-review`
- commit: working tree over `57655dc` (idle wiring + file-backed stdout fix)
- UTC time: 2026-07-14T04:01:50Z
- provider: `opencode` (real local CLI 1.17.18; plan probe pin was 1.17.13 —
  best-effort declaration updated by this evidence)
- scope: M3 = DR-04b export bridge (trust + Required bwrap offline profile +
  bounds + normalizer + idle-path wiring)
- procedure & results (via `LIBRA_RUN_LIVE_AGENT_GATE=1 cargo test
  --features test-live-agent live_opencode`):
  - operator-grade trust registration of the real binary (trusted dir +
    provenance record) succeeded; `trusted_opencode_binary` revalidated
  - REAL `opencode export <real-session-id>` under the Required bwrap
    offline profile (--unshare-net, ro binds, tmpfs /tmp, WAL-store rw
    exception): **success, 378,956 bytes — byte-identical to the
    unsandboxed baseline**; normalized to coverage-v1 turns
  - two REAL defects found and fixed by this gate:
    1. WAL-mode SQLite store needs write access even for reads — profile
       gained a narrowly-scoped rw bind of the opencode data dir only
    2. upstream CLI truncates large exports (~64 KiB) into backpressured
       PIPES while exiting success — bridge switched to an inherited
       anonymous-FILE stdout (flushes synchronously, crosses the sandbox
       mount namespace); full-size export verified
- stable result: success
