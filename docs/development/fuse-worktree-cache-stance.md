# FUSE Worktree Rust Cache: Position Statement

This document records Libra's position on an external proposal to build a
dedicated Rust build-artifact cache layer into Libra's FUSE worktree feature. It
is a development-facing design note, not a plan or task card, and is not
governed by [`docs/development/plan/plan-template.md`](plan/plan-template.md).

## Status

- Source: [libra-tools/libra#1069](https://github.com/libra-tools/libra/issues/1069) —
  "[Analysis] FUSE Worktree Rust disk-bloat proposal — multi-agent review
  requested" (filed 2026-11-22 by @genedna).
- Two independent agent reviews on #1069:
  - **R1 Grok Bot** (2026-11-22 15:48 UTC) — specification-first review; caught
    the `st_dev` / `rename(2)` cross-mount gap, the `unlink(symlink)` POSIX
    semantics hazard, the `0444` hardlink immutability gap, and the broken
    `unknown_url` citation in the proposal's bibliography (reference #11).
  - **R2 ChatGPT Dot** (2026-11-22 15:53 UTC) — product/decision-first review;
    verified `build.build-dir` stabilized in Cargo 1.91 (2025-10-30), flagged
    the 1.100 layout change against any Libra contract tied to the layout,
    framed a three-condition comparison (default FUSE vs private host output
    vs private host output + external compiler cache), and explicitly rejected
    a default `RUSTC_WRAPPER` install or stripping third-party debug info by
    default.
- Companion filing: `docs/development/plan/plan-20261123-fuse-cache-baseline.md`
  was attempted as a formal plan under `plan-template.md` and abandoned after
  seven Codex review rounds with 62 cumulative P1 findings and no convergence.
  Codex R7 confirmed that G-05 (verification must give directly copy-executable
  commands) is non-waivable, which makes a short natural-language plan
  non-compliant, and that CLH-02 of `issues/496.md` does not establish an
  ER-13 closeout exemption that would otherwise fit this work. The attempt is
  removed in the same commit that lands this statement.

## Scope

This document covers:

- The default behavior of `libra worktree add --fuse`
  (see [`Cargo.toml:19`](../../Cargo.toml) `worktree-fuse  = []`;
  [`src/command/worktree-fuse.rs:267`](../../src/command/worktree-fuse.rs)
  `upper_dir`) when Rust projects build under
  `.libra/worktree/fuse/<name>/workspace`.
- A decision framework for whether Libra should own a dedicated Rust
  build-artifact cache layer.
- The specific hazards both reviews converged on, with Libra's response
  for each.

It does not cover:

- Virtual mtime semantics or Warm Base copy-on-write (deferred below).
- RUSTC_WRAPPER / BLAKE3 CAS / an in-tree compiler cache (rejected below).
- Changes to the `worktree-fuse` release build policy (current state is
  opt-in at build time; release binaries do not ship the feature).

## Decision framework

The proposal's quantitative claims (65–83% physical reclaim, 2.6×–4.8×
first-edit speedup) extrapolate from third-party projects (worktrunk, kache,
Ferry, Alacritty). Libra has no first-party measurement. Before committing any
code to an in-tree cache layer, a first-party comparison must produce three
treatments measured on the same fixture, FS, hardware, and toolchain versions:

- **A — Default FUSE baseline.** `libra worktree add --fuse` with the user's
  normal `cargo` invocations and no additional configuration.
- **B — Documentation path.** Operator manually sets a per-worktree
  `CARGO_BUILD_BUILD_DIR` + `CARGO_TARGET_DIR` on the host filesystem;
  Libra itself is not involved beyond providing the FUSE workspace.
- **C — Documentation path plus external compiler cache.** B plus an existing
  external cache (sccache or kache), version-pinned.

Measurement must be per-worktree, per-round, per-tier. The decision rule is a
prioritized tree evaluated top-down at N = 8 worktrees (the regime where
Libra's prospective layer would add the most value). `Δp95(X→Y)` and
`Δphys(X→Y)` are relative improvements from X to Y; both must be measured
with positive, finite baseline values or the measurement is invalid and must
be rerun (not defaulted).

| Step | Criterion (assuming no earlier row matched) | Decision |
|---|---|---|
| 1 | `Δp95(A→C) < 25%` **or** `Δphys(A→C) < 50%` | `NO-GO` |
| 2 | `Δp95(B→C) ≥ 15%` **or** `Δphys(B→C) ≥ 15%` | `GO-DOCS-GUIDANCE` |
| 3 | `Δp95(A→B) ≥ 25%` **and** `Δphys(A→B) ≥ 50%` | `GO-PHASE-1-NARROW` |
| 4 | otherwise | `GO-DOCS-GUIDANCE` |

Reading the rule:

- `NO-GO` means the external cache alone does not deliver enough combined
  latency and disk-space improvement over the default FUSE setup to justify
  any new Libra code. Close the issue and document the current advice.
- `GO-DOCS-GUIDANCE` means most of the available improvement comes from the
  external cache on top of a manual host-side output dir. Libra should
  document the setup rather than owning a new layer.
- `GO-PHASE-1-NARROW` means the host-side output dir alone (without the
  external cache) materially moves both metrics, and the external cache's
  incremental contribution is small enough that Libra's bind-mount /
  schema-v4 / `.libraignore` prune work is worth building.

## Review consensus and Libra's response (GAP table)

| ID | Consensus hazard | Libra response |
|---|---|---|
| GAP-FCB-01 | Bind-mount and environment injection only cover processes Libra launches. A user's own terminal, IDE, `rust-analyzer`, or `./target`-hardcoded script sits outside the bind. | Any `GO-PHASE-1-NARROW` deliverable must enumerate which invocation paths are covered and which are not. Paths outside coverage must be diagnosed, not silently treated as protected. |
| GAP-FCB-02 | Same `st_dev` does not guarantee `rename(2)` across mounts: Linux `rename(2)` refuses cross-mount even when both mounts share one filesystem. | Any in-tree code touching atomic-rename semantics must probe the actual `temp → final` path pair in the relevant namespace, keep temp files beside their destination, and handle `EXDEV` without publishing partial output. `st_dev` equality is a storage-choice hint, not an atomicity proof. |
| GAP-FCB-03 | The proposal translates `unlink(symlink)` into recursive cleanup of the referent to work around Cargo issue [#7510](https://github.com/rust-lang/cargo/issues/7510). This changes ordinary POSIX semantics and makes a routine `unlink` destructive. | Rejected. The replacement path for a `GO-PHASE-1-NARROW` deliverable is to direct Cargo to the real private output directory and treat a missing anchor as a diagnosable repair target, not to overload the `unlink` handler. |
| GAP-FCB-04 | A `(pid, start_time)` lease does not cover independently launched builds, editors, or surviving children; if the first shipped slice auto-deletes host-side artifacts, lease safety must land with it, not after. | Any `GO-PHASE-1-NARROW` deliverable that auto-deletes host-side artifacts must land lease/journal safety in the same card. If deferred, auto-deletion must also be deferred. Boot/session and PID-namespace identity, not just `(pid, start_time)`, are the minimum lease identity. |
| GAP-FCB-05 | 65–83% / 2.6–4.8× are external-project extrapolations; the proposal's bibliography reference #11 is literally `unknown_url`. | Libra will not act on the proposal's quantitative claims. The decision above depends only on first-party measurements of A/B/C on the chosen fixture, FS, hardware, and toolchain. |
| GAP-FCB-06 | A virtual mtime scheme that fools Cargo's freshness check can also fool `make`, `rsync -a`, `inotify`-based watchers, and any other tool with legitimate mtime consumers. An incorrectly fresh build can report a passing test for stale code. | Out of scope for this stance. Deferred under "Deferred items" below. Virtual mtime is excluded from any `GO-PHASE-1-NARROW` deliverable regardless of the decision. |

## Guardrails (independent of decision)

The following hold regardless of the A/B/C outcome:

- **No in-tree RUSTC_WRAPPER by default.** Libra will not ship or auto-install
  a built-in compiler cache wrapper. Users who want one adopt sccache, kache,
  or an equivalent of their choice.
- **No default change to third-party debug info.** Any slimming of
  third-party debug symbols must be opt-in; the default does not touch
  `profile.dev.package.*.debug`.
- **No layout coupling to a specific Cargo internal version.** If any Libra
  code consumes `CARGO_BUILD_BUILD_DIR` or `CARGO_TARGET_DIR`, it must treat
  the internal layout under those paths as opaque; the internal layout has
  already changed once between 1.91 and 1.100 and will change again.
- **No POSIX-semantics overrides in the FUSE layer.** No handler may
  reinterpret ordinary `unlink`, `rename`, or `link` into destructive or
  compound operations. The FUSE layer may refuse operations; it may not
  redefine them.

## Downstream paths

Each decision has one well-defined follow-up:

| Decision | Follow-up |
|---|---|
| `GO-PHASE-1-NARROW` | A follow-up plan (recommended working title `plan-YYYYMMDD-fuse-redirect-mvp.md`) scoped to Linux unprivileged bind-mount of `target/`, FUSE registry schema v4, `.libraignore`-backed virtual prune boundary, and a FUSE health probe in `libra worktree doctor`. If the slice auto-deletes host artifacts, lease/journal safety lands in the same card per GAP-FCB-04. |
| `GO-DOCS-GUIDANCE` | A follow-up documentation change in `docs/commands/worktree.md` that recommends the host-side `CARGO_BUILD_BUILD_DIR` + `CARGO_TARGET_DIR` pattern together with an external compiler cache; no new Libra commands or subcommands. |
| `NO-GO` | Close #1069 with a summary comment pointing to this document, and add a one-sentence pointer in the FUSE worktree section of `docs/commands/worktree.md` ("for large Rust workloads, pair FUSE worktrees with an external compiler cache"). |

## Deferred items

- **Virtual mtime / Warm Base copy-on-write.** Both reviews flagged a specific
  silent-failure mode ("tests pass against stale code"). Not in scope here.
  Any future consideration must enter via its own design document and prove
  the semantics are safe for the full set of mtime consumers, not just
  Cargo's freshness check.
- **RUSTC_WRAPPER / BLAKE3 CAS / in-tree compiler cache.** Out of mission
  scope for an AI-native VCS; reconsider only if the external
  compiler-cache ecosystem (sccache, kache) becomes unusable.

## References

- GitHub issue: [libra-tools/libra#1069](https://github.com/libra-tools/libra/issues/1069)
- External project citations used by the original proposal: huacnlee/cargo,
  Meta EdenFS Redirections, kache, cargo-reapi, ferry, mr-boxington,
  worktrunk, cow-cli, faster_rust_worktrees, LayerFS, Treebeard, FluxDown.
- Cargo issues cited: rust-lang/cargo#5931, #7510, #12516, #13136, #16147,
  #16230, #16804, #17508, #4282.
- Cargo 1.91 changelog (2025-10-30) stabilizing `build.build-dir`:
  https://doc.rust-lang.org/cargo/CHANGELOG.html#cargo-191-2025-10-30
- Cargo build-cache reference:
  https://doc.rust-lang.org/cargo/reference/build-cache.html
- Inside Rust blog (2026-08-18) on `-Zembed-metadata=no`:
  https://blog.rust-lang.org/inside-rust/2026/08/18/reducing-target-directory-size-on-nightly/
