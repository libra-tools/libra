---
name = "libra"
description = "Project-local guidance for working with this libra-format repository. Seeded by `libra init` / `libra clone`. Customize freely."
version = "1.0.0"
allowed-tools = [
  "read_file",
  "list_dir",
  "grep_files",
  "search_files",
  "web_search",
  "shell",
  "apply_patch",
  "run_libra_vcs",
  "update_plan",
  "submit_task_complete"
]
---

# This project's libra repository

This skill was automatically created when the repository was initialized with `libra init` or cloned with `libra clone`.

**You should edit this file** to capture the specific knowledge, conventions, and workflows that AI agents (and humans) need when working on *this* codebase:
- Important architecture decisions
- Build, test, and lint commands used by the team
- Code style / review rules
- Domain-specific gotchas
- Preferred agent profiles or sub-agent usage

The universal libra knowledge (layout, CLI entry points, test layers, hard rules about `unwrap`, etc.) is always available from the built-in embedded skill. This file is your chance to make the agent *project-smart*.

## Quick reference — libra layout (do not assume .git)

- Metadata: `.libra/libra.db` (SQLite + SeaORM) + `.libra/vault.db`
- Objects/packs/index remain Git-compatible on disk
- Never look for `.git/`

## Essential commands (update these for your project)

```bash
# Format
cargo +nightly fmt --all

# Lint (the real gate)
cargo clippy --all-targets --all-features -- -D warnings

# Fast check
cargo check
```

### Tests

This file is not an independent acceptance policy. If `AGENTS.md` exists, read its **Testing Guidelines**; if `CLAUDE.md` exists, read its **Quality Acceptance Criteria**. If a task card has been selected, follow that card and its plan; do not assume a task card exists. If `docs/development/plan/plan-template.md` exists and governs the selected work, follow **ER-13** / **ER-14**. If some resources are absent, follow the applicable guidance that is available. If no project test guidance exists, identify and run the tests appropriate for this project's changes when feasible, and report what actually ran and what remains unrun; do not treat an example command as universal acceptance.

In Further reading, every listed path is read only when that file exists in the current project. The Libra source repository's `CLAUDE.md` uses this exact uppercase spelling.

## Core rules enforced in this repository

- Rust 2024, 4-space indent, Std/External/Crate import order
- **No `unwrap()`/`expect()` in `src/**`** (tests only; add `// INVARIANT:` comment for the rare infallible case)
- Actionable errors with context (what + which resource + fix hint)
- `_with_conn` suffix for DB helpers that take an existing connection

## When changing public CLI surface

Update in order:
1. `src/cli.rs`
2. `src/command/<name>.rs`
3. `COMPATIBILITY.md`
4. `docs/commands/<name>.md` (must have Examples section)
5. Tests + `tests/INDEX.md`
6. `pub const <CMD>_EXAMPLES`

## Further reading (inside this repo)

- `AGENTS.md` — primary agent guidance, when present in this project
- `CLAUDE.md` — contributor setup, env vars, test layers, when present in this project
- `COMPATIBILITY.md` — when present in this project
- `docs/development/tracing/agent.md` — when present in this project

Activate with `/skill libra` inside `libra code`.

Happy (agentic) hacking!
