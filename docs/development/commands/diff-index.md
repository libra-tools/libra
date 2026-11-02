# diff-index 命令开发设计

## 命令实现目标

`libra diff-index <tree>` 显示一个 tree 与工作树的差异，作为 `diff` 引擎的底层入口。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`diff-index [--cached] <tree> [-- <path>...]`、`-p`/`--patch`、`-s`/`--no-patch`、`--raw`、`--name-only`、`--name-status`、`--exit-code`、`--quiet`。`--cached` 把 tree 与 index 比较。工作树一侧与 `diff-files` 一样按条目存在性给状态字母（空 blob stat-invalid 为 `M` 不是 `A`，工作树截断为空为 `M` 不是 `D`）。ADR-PL-09：默认有差异也 0，`--exit-code`/`--quiet` 才转 1。
- **未公开**：`-m` 与其余 porcelain 标志面。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::DiffIndex` → `command::diff_plumbing::execute_index_safe`。
- 源码分层：`src/command/diff_plumbing.rs`：`DiffIndexArgs`（`cached`/`tree`/`paths` 及输出模式）。`--cached` 比较 index；否则比较工作树。合成 argv 后走 `command::diff::execute_safe`。复用唯一 diff 引擎。
- 底层操作对象：对象库 + 工作树（经 diff 引擎读）。无写入。

## 实现历史

- 2026-06-30（GGT-11 diff-plumbing 部分）：与 `diff-tree`/`diff-files` 同批新增。

## 当前状态

- 公开状态：已公开（`Commands::DiffIndex`）。
- 测试：`tests/command/diff_plumbing_test.rs`（名称输出、退出码、TC-0044 空 blob 为 `M`）。
- 用户文档：`docs/commands/diff-index.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容差异项 | `-m` 与其余 porcelain 标志面 | `--cached`、raw、`--name-only`/`--name-status`、`--exit-code`/`--quiet` 已公开。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 必须继续复用 `command::diff` 引擎；`--cached` 已走引擎的 index 比较，不要在本命令内另建 diff。
