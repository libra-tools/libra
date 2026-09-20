# Code Command Development

> **已移除（plan-20260920）：** `libra code` 公开命令已于 0.23.0 删除；本文保留为历史开发记录。外部 Agent 观测入口是 `libra agent`。

`libra code` is an intentionally different Libra AI extension, not a
Git-compatible command.

The active development contract, backlog, and compatibility guardrails live in
[`../tracing/code.md`](../tracing/code.md). Keep this file as the command
development index entry so `docs/development/commands/README.md` can list every
public CLI command without duplicating the Code planning document.

Task shell toolchain-root handling is documented in the [command reference](../../commands/code.md#rust-toolchains-in-task-worktrees) and [runtime contract](../internal/code-agent-runtime.md#task-shell-rustup-root-preservation-fix-pkt-04). On Unix, preserve the user's installed Rustup root before task HOME isolation, with explicit `RUSTUP_HOME` values taking precedence and existing sandbox policy governing access.
