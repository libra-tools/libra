# diff-tree 命令开发设计

## 命令实现目标

`libra diff-tree <a> <b>` 显示两个 tree 之间的差异，作为 `diff` 引擎的底层入口，不分叉第二套 diff 实现。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`diff-tree <tree-a> <tree-b> [-- <path>...]`、`-r`（空操作，总是递归）、`-p`/`--patch`、`-s`/`--no-patch`、`--raw`、`--name-only`、`--name-status`、`--exit-code`、`--quiet`、`-c`/`--cc`（接受拼写）。ADR-PL-09：默认有差异也 0，`--exit-code`/`--quiet` 才转 1。
- 未公开（延后）：单提交 `diff-tree <commit>`（对比父）、`-t`/`--stdin`。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::DiffTree` → `command::diff_plumbing::execute_tree_safe`。
- 源码分层：`src/command/diff_plumbing.rs`（三命令共享）：`DiffTreeArgs`（`tree_a`/`tree_b`/`paths`(last=true)）→ 合成 argv `["diff","--old",a,"--new",b,"--",paths...]` → `DiffArgs::try_parse_from` → `command::diff::execute_safe`。**复用唯一 diff 引擎**（输出/退出码/rename/空白全一致）。
- 底层操作对象：对象库（读 tree，经 diff 引擎）。无 refs/网络/index/工作树写入。

## 实现历史

- 2026-06-30（GGT-11 diff-plumbing 部分，`grit-gap.md` 阶段 4-5）：与 `diff-index`/`diff-files` 同批新增；`repack`/`pack-objects` 为后续 Phase B。

## 当前状态

- 公开状态：已公开（`Commands::DiffTree`）。
- 测试：`tests/command/diff_plumbing_test.rs`（名称输出、退出码、两 tree 与 pathspec）。
- 用户文档：`docs/commands/diff-tree.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容差异项 | 单提交 vs 父、`-t`/`--stdin` | `-r`、raw、`--name-only`/`--name-status`、`--exit-code`/`--quiet` 已公开。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 必须继续复用 `command::diff` 引擎，不得新建第二套 diff 实现。

- 2026-11-13（#478 LG-09，ADR-LG-06 第 2、4 条）：默认输出改为 Git 的非递归 raw（根 tree 的 blob/tree 条目，完整 40/64 位 OID，缺失侧 `000000`+全零 id），`-r` 递归 blob、`-t` 递归时附 tree 条目；新增单提交形式（`<commit>` 先输出提交 id，再与第一父提交非递归 diff）、`--no-commit-id`、`--root`（根提交对空 tree 全量创建）；`-p` 继续委托共享 diff 引擎并保留 id 行，raw 走 `diff_plumbing` 原生渲染。回归：`test_diff_tree_raw_and_patch_matrix`。
