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

### 显式转换内容（I1062-06）

- 普通工作区 diff 的候选 OID 通过共享 `ConversionScan` 计算；仅属性源/索引指纹有效且 stat 匹配时复用索引 OID。`WorktreeContents` 让渲染、pickaxe、二进制处理和 rename 消费相同 clean 内容，非零候选 OID 不一致时报可重试读取错误。转换读取失败必须传播。
- plumbing 仍由 `get_files_blobs_stat_only` 选择候选，raw/name/退出码保留 stat 语义。`-p` 读取 clean 内容，内容相同则清空补丁但保留候选；真实编辑的 patch index 行使用实际 clean OID。缓存按路径与 OID 联合索引，防止多个全零 OID 候选串用文件内容。intent-to-add 无有效 stat，使用共享 clean 内容计算候选哈希，避免 raw OID 与 clean 正文不一致；TC-0044 条目存在性规则保持。
- 已存储 index/tree blob 不转换；LFS 指针来自原始载荷摘要、symlink 读取目标字节，外部 diff/textconv 与 `--no-index` 输入契约保持。比较不写源文件、索引或对象库。`text=auto` 由 #861 承接。
- 回归位于既有 `command_test`：`diff_test::i1062_explicit_`、`diff_test::i1062_failure_`、`diff_test::i1062_content_view_` 与 `diff_plumbing_test::i1062_plumbing_explicit_conversion_matches_git_modes`。固定 Git 对照保留各模式退出码，raw OID 显示使用 Git `--abbrev=7` 对齐 Libra 既有宽度，不宣称默认宽度相同。

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

## 共享元数据属性（#1062）

旧版符号链接工作树的内容比较从已解析的元数据目录读取 `info/attributes`。属性文件必须是可读的普通文件；属性文件自身的符号链接会被拒绝。 raw 输出和差异退出码仍保持上述基于 stat 的规则。

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 必须继续复用 `command::diff` 引擎；`--cached` 已走引擎的 index 比较，不要在本命令内另建 diff。
