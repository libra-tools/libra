# rerere 命令开发设计

## 命令实现目标

`libra rerere` 记录冲突解决（preimage→postimage）并在规范化后的同一冲突再现时复用。它既可独立运行，也由 merge/rebase/cherry-pick 在 `rerere.enabled=true` 时自动调用。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`rerere`（默认 update：记录 preimage / 复用 postimage / 记录已解决的 postimage）、`status`/`diff`/`forget`/`clear`/`gc`。存储 `.libra/rerere/<id>/{preimage,postimage}`（repository 共享 cache）+ per-worktree `<local_gitdir>/MERGE_RR`（W2 §C.4.3：一个 worktree 的 clear/auto-update 不影响另一 worktree 的 current conflicts；legacy `.libra/rerere/MERGE_RR` 按 ambiguous-sidecar 规则——linked 永不读、单 worktree main 首写迁移、有 linked 证据时不消费仅提示留待 W3 doctor）。
- 匹配：`<id>` 是每个完整冲突 hunk 两侧按字典序排列、丢弃 diff3 base 后的 SHA-256；普通上下文与 marker 标签不参与 id，故 ours/theirs 互换和无关上下文变化仍命中。无完整 marker 时回退 SHA-256(整文件)。旧整文件键缓存无需迁移并与新键并存。
- 回放：cache 的规范化 preimage 为三方 base，当前规范化冲突为 ours，postimage 为 theirs；仅 clean 的 `diffy::merge_bytes` 结果可写回。回放冲突时保持工作树原样。`rerere.enabled` 开启时 merge/rebase/cherry-pick 均自动 record/replay，三个命令的 `--rerere-autoupdate`/`--no-rerere-autoupdate` 均生效并持久化至 `--continue`。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::Rerere` → `command::rerere::execute_safe`。
- 源码分层：`src/command/rerere.rs`：`RerereArgs`（`Option<RerereSubcommand>`）、`RerereSubcommand`（Status/Diff/Forget/Clear/Gc）、`update`/`status`/`diff`/`forget`/`clear`/`gc` + helper（`is_conflicted`/`conflict_id`/`read_merge_rr`/`write_merge_rr`/`write_entry`/`entry_path`）。
- update：`Index::load` 后收集 stage 0–3 的候选路径；完整冲突 marker 经递归 hunk normalizer 生成 preimage 与仅含 hunk sides 的 fingerprint。命中 cache 时以 `preimage/current/postimage` 做 fail-safe 三方回放；否则记录规范化 preimage + 写入本 scope 的 MERGE_RR。先对 MERGE_RR 中已解决（无 marker）的文件记 postimage 并移出 MERGE_RR。scope 经 `WorktreeScope::for_request()` 每请求解析一次并显式传递。
- diff：`diffy::create_patch(preimage, current)`（复用 diff 库）。
- 存储目录：`util::try_get_storage_path(None)?.join("rerere")`（仓库外→`repo_not_found` 128）。
- gc：按 preimage mtime + 是否有 postimage 分别用 60d/15d TTL 删除 `<id>` 目录。
- 底层操作对象：`.libra/rerere/` + 只读 index/worktree（update 写回被复用的 worktree 文件）。无对象库/refs/网络写入。

## 实现历史

- 2026-06-30（GGT-12 Phase A，`grit-gap.md` 阶段 5）：新增独立 rerere 存储 + CLI。

## 当前状态

- 公开状态：已公开（`Commands::Rerere`）。
- 测试：`tests/command/rerere_test.rs`（record→resolve→replay、交换 sides、干净三方回放保留当前非冲突改动、回放冲突不写回、status、forget(+未知路径 128)、clear、diff、gc、仓库外）+ `rerere.rs` 的 `rerere::normalize` 单测（hunk 提取、排序、丢弃 diff3 base、无 marker 回退）。
- 用户文档：`docs/commands/rerere.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| ✅ 已实现 | 自动集成 | `rerere.enabled` 开启时，merge/rebase/cherry-pick 在冲突时自动 record/replay，在解决并提交或 `--continue` 时自动记录 postimage；`--rerere-autoupdate` 控制是否暂存。 |
| ✅ 已实现 | 逐 hunk 归一化 | 丢弃 diff3 base、按字典序归一化 sides；id 只哈希 hunk sides，当前文件上下文由三方回放保留。 |
| 配置 | `gc.rerereResolved`/`gc.rerereUnresolved` 可配 | 当前用默认 60/15 天常量。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- diff 与回放必须继续复用 `diffy`；回放只能在三方合并 clean 时写回，任何冲突都必须保留当前工作树字节不变。
