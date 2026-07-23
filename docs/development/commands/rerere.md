# rerere 命令开发设计

## 命令实现目标

`libra rerere` 记录冲突解决（preimage→postimage）并在相同冲突再现时复用。Phase A：独立的存储 + 记录/复用 + `status`/`diff`/`forget`/`clear`/`gc`；Phase B：merge/rebase/cherry-pick 自动集成。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`rerere`（默认 update：记录 preimage / 复用 postimage / 记录已解决的 postimage）、`status`/`diff`/`forget`/`clear`/`gc`。存储 `.libra/rerere/<id>/{preimage,postimage}`（repository 共享 cache）+ per-worktree `<local_gitdir>/MERGE_RR`（W2 §C.4.3：一个 worktree 的 clear/auto-update 不影响另一 worktree 的 current conflicts；legacy `.libra/rerere/MERGE_RR` 按 ambiguous-sidecar 规则——linked 永不读、单 worktree main 首写迁移、有 linked 证据时不消费仅提示留待 W3 doctor）。
- **有意差异/延后**：匹配为**整文件逐字节**（`<id>`=SHA-256(冲突文件字节)），非 Git 的逐 hunk 归一化/ours-theirs 顺序无关；与 merge/rebase/cherry-pick 的**自动**集成（`rerere.enabled`/`--rerere-autoupdate` 实际生效）为 Phase B —— 目前显式运行 `libra rerere`，那些命令的 `--rerere-autoupdate` 仍 no-op。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::Rerere` → `command::rerere::execute_safe`。
- 源码分层：`src/command/rerere.rs`：`RerereArgs`（`Option<RerereSubcommand>`）、`RerereSubcommand`（Status/Diff/Forget/Clear/Gc）、`update`/`status`/`diff`/`forget`/`clear`/`gc` + helper（`is_conflicted`/`conflict_id`/`read_merge_rr`/`write_merge_rr`/`write_entry`/`entry_path`）。
- update：`Index::load`→`tracked_files()`，对每个 worktree 文件：含冲突标记（`<<<<<<<` + `=======`/`>>>>>>>`）→ `id`=sha256(content)；postimage 存在→复用（写回 worktree）；否则记 preimage + 入本 scope 的 MERGE_RR。先对 MERGE_RR 中已解决（无标记）的文件记 postimage 并移出 MERGE_RR。scope 经 `WorktreeScope::current()` 每请求解析一次并显式传递。
- diff：`diffy::create_patch(preimage, current)`（复用 diff 库）。
- 存储目录：`util::try_get_storage_path(None)?.join("rerere")`（仓库外→`repo_not_found` 128）。
- gc：按 preimage mtime + 是否有 postimage 分别用 60d/15d TTL 删除 `<id>` 目录。
- 底层操作对象：`.libra/rerere/` + 只读 index/worktree（update 写回被复用的 worktree 文件）。无对象库/refs/网络写入。

## 实现历史

- 2026-06-30（GGT-12 Phase A，`grit-gap.md` 阶段 5）：新增独立 rerere 存储 + CLI。

## 当前状态

- 公开状态：已公开（`Commands::Rerere`）。
- 测试：`tests/command/rerere_test.rs`（record→resolve→replay 全循环、status、forget(+未知路径 128)、clear、diff、gc no-op、仓库外 128）+ `rerere.rs` 单测（冲突标记检测、conflict_id 稳定且内容寻址）。
- 用户文档：`docs/commands/rerere.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 自动集成（Phase B） | merge/rebase/cherry-pick 在冲突时自动 record、解决后自动 record、再冲突自动 replay（`rerere.enabled`/`--rerere-autoupdate` 生效） | **有意延后**：需接入各 sequencer 的冲突处理；当前显式 `libra rerere`，`--rerere-autoupdate` 仍 no-op。 |
| 归一化 | 逐 hunk 归一化 + ours/theirs 顺序无关 | 延后；当前整文件逐字节匹配（正确但更窄）。 |
| 配置 | `gc.rerereResolved`/`gc.rerereUnresolved` 可配 | 当前用默认 60/15 天常量。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- diff 必须继续复用 `diffy`；Phase B 接入 sequencer 时，record/replay 必须保持「整文件逐字节」或先实现 hunk 归一化，避免破坏文件。
