# diff-files 命令开发设计

## 命令实现目标

`libra diff-files` 显示 index 与工作树的差异（未暂存改动），作为 `diff` 引擎的底层入口。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`diff-files [-- <path>...]`（= 裸 `diff`，index 对工作树）、`-p`/`--patch`、`-s`/`--no-patch`、`--raw`（默认）、`--name-only`、`--name-status`、`--exit-code`、`--quiet`，所有 `diff` 全局标志与 `--json`；工作树一侧为 stat-based（issues/479 DF-01：stat 漂移条目按 `M` + 全零 worktree sha 报告，不哈希；`read-tree` 零 stat 的 racy 条目同理）。状态字母按条目存在性判定：空 blob 的 stat-invalid 条目是 `M` 不是 `A`，工作树截断为空是 `M` 不是 `D`（TC-0044）。ADR-PL-09 退出契约：默认有差异也 0，`--exit-code`/`--quiet` 才转 1。
- 未公开（延后）：`-1`/`-2`/`-3` 阶段选择。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::DiffFiles` → `command::diff_plumbing::execute_files_safe`。
- 源码分层：`src/command/diff_plumbing.rs`：`DiffFilesArgs`（`paths`/`patch`/`no_patch`/`raw`/`name_only`/`name_status`/`exit_code`/`quiet`）→ 合成 argv（`--no-renames`、可选 `--exit-code`、worktree 比较时 `--plumbing-stat-only`）→ `DiffArgs::try_parse_from` → `command::diff::execute_safe`。复用唯一 diff 引擎；`--plumbing-stat-only`（隐藏）让 `resolve_worktree_side` 走 `get_files_blobs_stat_only`：stat 匹配复用 index hash，stat 漂移记全零 sha 不读内容（intent-to-add 占位条目仍哈希）。
- 底层操作对象：index + 工作树（经 diff 引擎读）。无写入。

### 显式转换内容（I1062-06）

- 普通工作区 diff 的候选 OID 通过共享 `ConversionScan` 计算；仅属性源/索引指纹有效且 stat 匹配时复用索引 OID。`WorktreeContents` 让渲染、pickaxe、二进制处理和 rename 消费相同 clean 内容，非零候选 OID 不一致时报可重试读取错误。转换读取失败必须传播。
- plumbing 仍由 `get_files_blobs_stat_only` 选择候选，raw/name/退出码保留 stat 语义。`-p` 读取 clean 内容，内容相同则清空补丁但保留候选；真实编辑的 patch index 行使用实际 clean OID。缓存按路径与 OID 联合索引，防止多个全零 OID 候选串用文件内容。intent-to-add 无有效 stat，使用共享 clean 内容计算候选哈希，避免 raw OID 与 clean 正文不一致；TC-0044 条目存在性规则保持。
- 已存储 index/tree blob 不转换；LFS 指针来自原始载荷摘要、symlink 读取目标字节，外部 diff/textconv 与 `--no-index` 输入契约保持。比较不写源文件、索引或对象库。`text=auto` 由 #861 承接。
- 回归位于既有 `command_test`：`diff_test::i1062_explicit_`、`diff_test::i1062_failure_`、`diff_test::i1062_content_view_` 与 `diff_plumbing_test::i1062_plumbing_explicit_conversion_matches_git_modes`。固定 Git 对照保留各模式退出码，raw OID 显示使用 Git `--abbrev=7` 对齐 Libra 既有宽度，不宣称默认宽度相同。

## 实现历史

- 2026-10-20（issues/479 DF-01）：默认退出码 1→0，新增 `--exit-code`/`--quiet`；worktree 侧改 stat-based（全零 sha 不哈希）；TC-0021 移植测试 `add_test::test_add_refresh_with_pathspec_tc0021` 与 TC-0044 stat-raciness 测试入 `diff_plumbing_test`。
- 2026-06-30（GGT-11 diff-plumbing 部分）：与 `diff-tree`/`diff-index` 同批新增。

## 当前状态

- 公开状态：已公开（`Commands::DiffFiles`）。
- 测试：`tests/command/diff_plumbing_test.rs`（diff-files 显示未暂存改动）。
- 用户文档：`docs/commands/diff-files.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容差异项 | `-1`/`-2`/`-3` 阶段选择；未合并路径在 raw 记录渲染为 `D`（git 为 `U`，P2 residual 已登记） | 延后；更多用 `libra diff`。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 必须继续复用 `command::diff` 引擎。
