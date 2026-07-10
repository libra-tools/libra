# check-attr 命令开发设计

## 命令实现目标

`libra check-attr` 的目标是：对一组路径，报告 Git/Libra attributes 来源表达的属性值。它是只读查询，不修改 index/工作树，对齐 `git check-attr` 的输出形状（`<path>: <attr>: <value>`）与退出码（成功恒为 0，用法/仓库错误 128）。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 有意差异（D5）：Libra **不**实现 Git `.gitattributes` 的 smudge/clean filter 桥接；`check-attr` 是对 attributes 的只读查询，不运行 filter。当前支持通用属性状态：`attr`、`attr=value`、`-attr`、`!attr`；`filter=lfs` 会驱动 LFS 跟踪判断，`diff=<driver>` 会驱动 `diff --textconv`，`export-ignore` 会驱动 `archive` 过滤。
- 已支持：`<attr>... [--] <pathname>...`、`-a/--all`（仅报告已设置属性）、`--stdin`、`-z`（NUL 输入/输出分隔），以及全局 `--json`/`--machine`。
- 未公开/不适用：Git 的 `--cached` / `--source` 与 attributes 宏展开。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::CheckAttr(command::check_attr::CheckAttrArgs)`，dispatch 到 `command::check_attr::execute_safe`。CLI 名 `check-attr`（clap kebab-case）。
- 源码分层：
  - `src/command/check_attr.rs`：`CheckAttrArgs`（`attrs`/`paths`(last=true)/`all`/`stdin`/`null`）、`execute`/`execute_safe`、`CheckAttrEntry`/`CheckAttrOutput`（`--json`）、`attribute_value`、`read_stdin_paths`、`render`。
  - `src/utils/attributes.rs`：共享 attributes 来源解析与缓存，按低到高优先级读取 `core.attributesFile`、按目录从根到子目录的 `.gitattributes`、同目录高优先级的 `.libra_attributes`，最后读取最高优先级的 `.git/info/attributes`；对逃出 worktree 的路径返回 `unspecified`。
  - `src/utils/lfs.rs`：`is_lfs_tracked` 委托 `utils::attributes::is_lfs_tracked`，让 `add` / `lfs ls-files` 与 `check-attr filter` 使用同一判定。
- 执行路径：
  1. `util::require_repo()`（不在仓库 → 128）。
  2. 位置参数消歧：`--all` → 全部位置参数为路径；显式 `--` → 之前属性、之后路径（`paths` 用 `#[arg(last = true)]` 捕获）；`--stdin` → 位置参数为属性名、路径来自 stdin；否则首位置参数为属性、其余为路径。
  3. 校验：非 `--all` 且无属性 → 128；无路径 → 128；`--stdin` 与位置路径并存 → 128。
  4. 对每个路径通过 `utils::attributes` 计算属性；`--all` 时输出已设置/已取消/有值属性；否则对每个属性输出 `set` / `unset` / `<value>` / `unspecified`。
  5. 渲染：默认 `<path>: <attr>: <value>`；`-z` 三字段 NUL 分隔并 NUL 终止；`--json` 走 `emit_json_data`。
  6. 退出码：成功恒 0；用法/仓库错误 128（无 “exit 1” 语义，区别于 `check-ignore`）。
- 底层操作对象：attributes 来源文件（`core.attributesFile`、`.git/info/attributes`、`.gitattributes`、`.libra_attributes`）与缓存。无对象库/refs/index/网络写入。
- 输出与错误契约：human / `--json` / `--machine` 经 `OutputConfig`；用法错误 `CliError::command_usage(...).with_exit_code(128)`，仓库缺失 `repo_not_found()`（fatal → 128），`--stdin` 上限 64 MiB（超限 128）。
- 副作用边界：纯读取。

## 实现历史

- 2026-06-30（GGT-04，`grit-gap.md` 阶段 1）：新增 `check-attr`，复用 `lfs::is_lfs_tracked`（不复制属性匹配逻辑）；实现 `--all`/`--stdin`/`-z`/`--json` 与 `--`/首参消歧；同步 `COMPATIBILITY.md`、用户文档与集成测试。
- 2026-07-09（plan-20260708 P1-02）：新增 `src/utils/attributes.rs` 共享 attributes 来源解析，`check-attr` 改为读取 Git 标准 attributes 来源与 `.libra_attributes`，并把 `filter=lfs`、`diff=<driver>`、`export-ignore` 分别接到 LFS、diff textconv 与 archive。

## 当前状态

- 公开状态：已公开（`src/cli.rs::Commands::CheckAttr`）。
- Synopsis：`libra check-attr [-z] (<attr>... | --all) [--] <pathname>... | --stdin`。
- 测试：`tests/command/check_attr_test.rs`（filter 命中/未命中、`--all`、`--stdin`、`-z`、`--` 多属性、`--json`、用法错误、非仓库），登记于 `tests/command/mod.rs`。
- 用户文档：`docs/commands/check-attr.md`（EN）、`docs/commands/zh-CN/check-attr.md`。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 有意差异 | Git `.gitattributes` smudge/clean filter 驱动 | D5 拒绝；`check-attr` 仅查询，不运行 filter。 |
| 兼容差异项 | `--cached` / `--source` 与 attributes 宏展开 | 不公开；默认查询工作树 attributes 来源。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 任何行为变更先核对 `check_attr.rs` / `lfs::is_lfs_tracked`，再同步 `COMPATIBILITY.md`、`docs/commands/check-attr.md` 与测试。
