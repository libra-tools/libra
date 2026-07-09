# write-tree 命令开发设计

## 命令实现目标

`libra write-tree` 把当前 `.libra/index` 写成一个嵌套 Git tree 对象并打印根 tree id，作为 `read-tree` 的底层配套与 commit/merge/cherry-pick 树构造的共享入口。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：无参数或 `--index-file <path>` 把 index 写成嵌套 tree（保留 mode 与 hash kind），`--json`/`--machine`。空 index → 规范空 tree。写入前校验 stage-0 index 条目中的 blob/tree 对象存在且类型匹配，缺失/错类型 fail-closed 为 `LBR-REPO-002`；gitlink 不校验。
- 未公开：Git 的 `--prefix=<prefix>`、`--missing-ok`（延后）。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::WriteTree` → `command::write_tree::execute_safe`。
- 源码分层：
  - `src/command/write_tree.rs`：`WriteTreeArgs`（`index_file`）、`execute`/`execute_safe`、`WriteTreeOutput`（`--json`）。
  - `src/internal/tree_plumbing.rs`：**单一**的 index↔tree 领域实现。`write_tree_from_index(index)` 先调用 `validate_index_objects` 校验 stage-0 条目的对象存在/类型，再收集 `(path, TreeItemMode, hash)` 叶子并调用 `write_tree_from_leaves`；后者按父目录分组、`ensure_ancestor_dirs` 注册所有祖先目录、递归构建并保存每个子 tree、返回根 tree id。**正确处理中间空目录**（如 `a/b/c.txt`，`a`/`a/b` 下无直接文件）——这是早期各命令各自的树构造器漏掉的 case。
- 执行路径：`require_repo` → `Index::load(path::index())` / `--index-file`（缺失 scratch index 视为空）→ `tree_plumbing::write_tree_from_index` → 打印/JSON 根 tree id。
- 共享与去重（GGT-05 收口）：`cherry_pick.rs::create_tree_from_index`（index-based）改为委托 `write_tree_from_index`；`merge.rs::create_tree_from_items_map`（items-based）改为委托 `write_tree_from_leaves`。二者删除各自重复的 `build_tree_recursively`/`tree_entries_map_from_items` 等。tree_plumbing 带签名冻结测试与 mode 映射/中间目录单元测试。
- 仍未收口的重复（已知，记入未完成项）：`rebase.rs`（items-based，含 `RebaseTreeEntry`/`create_tree_from_items_map`）与 `stash.rs`（**基于文件系统遍历**的 `build_tree_recursive`，算法不同）未并入；`internal/ai/history.rs::write_tree` 是 `&[TreeItem] -> 单 tree` 的不同 API（AI 历史按类型分组），非 index→嵌套 tree 同形，故有意独立。
- 底层操作对象：`.libra/index`、对象库（`util::objects_storage()` put）。无 refs/网络/工作树写入。
- 输出与错误契约：human/`--json` 经 `OutputConfig`；仓库缺失 `repo_not_found()`，tree 构造或 index 对象校验失败 `RepoCorrupt`（`LBR-REPO-002`）→ 128。

## 实现历史

- 2026-06-30（GGT-05，`grit-gap.md` 阶段 2）：新建 `internal/tree_plumbing.rs`（单一 index↔tree 实现，修正中间空目录 bug）；公开 `write-tree`/`read-tree`；cherry-pick/merge 委托共享 helper。

## 当前状态

- 公开状态：已公开（`Commands::WriteTree`）。
- 测试：`tests/command/write_tree_test.rs`（空 index→空 tree、嵌套目录、`--json`、非仓库 128、与 read-tree round-trip）；`tests/compat/write_tree_missing_object_test.rs`（P0-09：缺失/错类型 index 对象 fail-closed）；`src/internal/tree_plumbing.rs` 单元测试（签名冻结、mode 映射、中间目录注册）。
- 用户文档：`docs/commands/write-tree.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容差异项 | `--prefix` / `--missing-ok` | 延后；按需补齐。 |
| 去重收口 | `rebase.rs`/`stash.rs` 树构造、`ai/history.rs::write_tree` 未并入 tree_plumbing | rebase items-based 可后续委托 `write_tree_from_leaves`；stash 为 FS 遍历、history 为不同 API，记为后续/有意独立。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 树构造逻辑只允许存在于 `internal/tree_plumbing.rs`；新增 index→tree 调用方必须复用 `write_tree_from_index` / `write_tree_from_leaves`，不得新增重复实现。
