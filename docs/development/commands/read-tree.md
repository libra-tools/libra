# read-tree 命令开发设计

## 命令实现目标

`libra read-tree` 把一个 tree-ish 解析后读入 `.libra/index`（替换 index 内容），作为 `write-tree` 的底层逆操作。首版仅 index、不触工作树。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`read-tree <tree-ish>`（tree id / commit / ref / tag / `HEAD`，以及 `^{tree}`/`^{commit}` 等共享 revision 语法，严格剥离到 tree），`--json`/`--machine`。
- 未公开：`-m`（合并）、`-u`（更新工作树）、`--reset`、`--prefix`、多 tree 合并（延后）。因此首版**不可能**静默覆盖工作树文件——它只写 index。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::ReadTree` → `command::read_tree::execute_safe`。
- 源码分层：
  - `src/command/read_tree.rs`：`ReadTreeArgs { tree_ish }`、`execute`/`execute_safe`、`ReadTreeOutput`（`--json`：`tree` + `entries`）、`resolve_tree_ish`。
  - `src/internal/tree_plumbing.rs::read_tree_into_index(tree_id)`：递归展平 tree 的叶子为 stage-0 index 条目（mode 由 `tree_mode_to_index_mode` 还原；blob size 置 0，tree id 仅由 `(mode,id,name)` 决定故 round-trip 不受影响）。
- 执行路径：`require_repo` → `resolve_tree_ish`（委托 `util::resolve_tree_ish_typed`，共享严格 revision/typed-peel/tag 解析并验证最终对象为 tree-ish） → `read_tree_into_index` → `index.save(path::index())`。
- 安全：仅写 index，不动工作树，故无覆盖风险；无效 tree-ish → 128。
- 底层操作对象：对象库（读 tree）、`.libra/index`（写）。无 refs/网络/工作树写入。
- 输出与错误契约：默认静默（Git read-tree 风格），`--json` 给 `{tree, entries}`；仓库缺失 `repo_not_found()`，无效 tree-ish `CliInvalidTarget`+exit 128，缺参数为 clap 用法错误。

## 实现历史

- 2026-06-30（GGT-05，`grit-gap.md` 阶段 2）：与 write-tree、tree_plumbing 一同新增。

## 当前状态

- 公开状态：已公开（`Commands::ReadTree`）。
- Synopsis：`libra read-tree <tree-ish>`。
- 测试：`tests/command/read_tree_test.rs`（HEAD 替换 index、显式 tree id、`--json`、无效 tree-ish 128、缺参数用法错误、非仓库 128）。
- 用户文档：`docs/commands/read-tree.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容差异项 | `-m`/`-u`/`--reset`/`--prefix`、多 tree 合并 | 延后；首版仅 index。更新工作树用 `restore`/`checkout`。 |
| 精度 | 读入条目的 blob size 置 0 | 不影响 tree round-trip；后续如需精确 stat 再补。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- tree→index 逻辑只允许存在于 `internal/tree_plumbing.rs::read_tree_into_index`。
