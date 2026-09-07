# merge-base 命令开发设计

## 命令实现目标

`libra merge-base` 打印两个提交的最佳共同祖先（LCA），并提供 `--all`（全部 LCA）与 `--is-ancestor`（祖先测试）。同一 LCA 实现（`internal/merge_base.rs`）被 `diff A...B` 复用。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`merge-base <a> <b>`（单 base）、`--all`（全部 LCA）、`--is-ancestor`（exit 0/1）、`--json`/`--machine`。
- 退出码：0 找到/祖先成立；1 无共同祖先/祖先不成立（无输出，**对齐 Git**——计划早期写「无共同祖先 → 128」与 Git 不符，Git 此情形 exit 1、128 留给坏 rev，已据此调和）；128 坏 rev / 参数个数错误。
- 未公开（延后）：多于两个提交、`--octopus`/`--independent`/`--fork-point`。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::MergeBase` → `command::merge_base::execute_safe`。
- 核心：`src/internal/merge_base.rs` —— **唯一** LCA 实现：
  - `CommitSource` trait（唯一读取面）+ `ObjectStoreCommits`（带缓存，经 `object_ext::CommitExt::try_load`，不依赖 `command::`）。每个 commit 只暴露走图需要的两项事实：parents 与 committer date（`CommitNode`）。测试与 benchmark 换成内存图实现，因此 10^4 提交的合成历史不需要写任何对象。
  - `merge_bases(a,b)` = `paint_down_to_common` + `remove_redundant`（与 git@`3cb9185f6` `commit-reach.c:187` 同构）：从两个 tip 同时向下染色，commit 从 committer-date 优先队列取出并把自己的 flag（`lhs`/`rhs`/`stale`）传给 parents；双侧命中即记为候选并置 `stale`（其祖先不可能是极大共同祖先）。只有候选集（通常 1 个）再做极大化过滤：`len() <= 1` 直接返回、不走图，否则做**一次多源遍历**（种子 = 全部候选的 parents，走到的候选即被支配；DAG 无环，候选走不到自己），而不是「每个候选一次全祖先遍历」。结果仍按 hex 排序，确定性不变。
  - **终止判据保守**：队列被抽干，不做 Git 的 side-exhaustion / single-result 提前退出——那两条以 commit-graph generation 拓扑序为前提（`commit-reach.c:194`），而本仓库只有 commit-graph writer、没有 reader；只凭 committer date（会 skew）提前退出不成立。committer date 因此只决定**访问顺序**，不参与正确性：日期乱序/全等的历史与旧实现结果一致（`merge_bases_ignore_skewed_committer_dates`）。
  - 复杂度（相对被替换的实现）：可达性从「每侧一次全量 BFS + 两个全祖先 `HashSet`」变为「单次染色遍历，每个 commit 至多按其获得的 flag 入队 3 次」，读取数上界 `3 × (commits + edges)`（**边敏感**：每次出队要读自身 + 每个 parent）；极大化从「对**每个**共同祖先做一次全祖先遍历」（O(|common|×E)，长共享主干时 |common| 就是整条主干）变为「一次多源遍历」。内存上不再同时驻留两份全祖先集合，优先队列只持有 frontier（宽度相关、与深度无关，`paint_down_frontier_tracks_width_not_depth` 钉住）。
  - `merge_base(a,b)` = 第一个 LCA；`is_ancestor(anc,desc)` = `anc ∈ ancestors(desc)`（自反，对齐 `--is-ancestor X X`→0）。
  - **修正 first-found**：旧 `log.rs`/`rebase.rs` 的 `find_merge_base` 返回首个命中（非 LCA），交叉合并下可能偏高；本实现返回真 LCA。`rebase` 已于 P1-07 迁到本模块（`rebase.rs:2691` 的 `merge_base`、`:2494`/`:2503` 的 `is_ancestor`），`am` 亦消费 `is_ancestor`（`am.rs:391`）；只剩 `log A...B` 仍是自有实现。
- CLI：`src/command/merge_base.rs`：`MergeBaseArgs`（`all`/`is_ancestor`/`commits`）；`--is-ancestor` 与 `--all` 互斥；要求恰好 2 个 commit；`resolve_commit`（`util::get_commit_base`，坏 rev→128）；无共同祖先/祖先不成立→`silent_exit(1)`；`--json` `{ bases }` / `{ is_ancestor }`。
- `diff A...B`：`diff.rs::normalize_diff_range` 在两点解析**之前**先 `split_once("...")`，解析 left/right→`get_commit_base`→`merge_base::merge_base`，把 `args.old` 设为 base、`args.new` 设为 right；无法解析/无 base 时保持 pathspec 回落。保留既有 `A..B` 语义。
- 底层操作对象：对象库（读 commit）。无 refs/网络/index/工作树写入。

## 实现历史

- 2026-06-30（GGT-09 Phase A，`grit-gap.md` 阶段 4）：新建 `internal/merge_base.rs` + `merge-base` CLI + `diff A...B`。
- 2026-09-04（plan-20260903 MG-01）：算法升级为 Git 同构的 paint-down（见「设计方案」），公开 API、结果与排序全部不变；调用面 merge / merge-base / `diff A...B` / rebase / `am`（`is_ancestor`）逐一回归。

## 当前状态

- 公开状态：已公开（`Commands::MergeBase`）。
- 测试：`tests/command/merge_base_test.rs`（Y 形 merge-base=base、`--is-ancestor` 双向、`--all`、`--json`、坏 rev 128、参数个数 128、`diff A...B` 用 merge-base）；`src/internal/merge_base.rs` 内联单测（对拍：被替换的 BFS-交集实现作为 oracle 逐例比对——Y 形、criss-cross 双 base、自反与祖先、无关历史、skewed/全等日期；frontier 只随宽度不随深度；`bench_merge_base_scaling` 在 12,000 提交主干上断言染色读取数线性，并在小规模上实测被替换实现的读取数随主干长度**超线性**增长）。
- 用户文档：`docs/commands/merge-base.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 共享收口（Phase B） | 只剩 `log A...B` 未迁移到 `internal/merge_base.rs`（`rebase`/`am` 已迁） | **有意延后**：迁移会改变 `log A..B` 输出，需 golden 回归 + `legacy-merge-base` 开关（计划要求）。 |
| 兼容差异项 | 多提交、`--octopus`/`--independent`/`--fork-point` | 延后。 |
| ✅ 已实现 | LCA dominated 计算的 O(common×E) 全祖先遍历 | 已由 paint-down 取代（MG-01）：极大化只作用于候选集，单候选直接返回。 |
| 性能（延后） | commit-graph generation 的消费，以及依赖 generation 的提前退出（Git 的 side-exhaustion / single-result 早退） | **尚未排期**：本仓库只有 commit-graph writer、无 reader，早退在只有 committer date 时不成立。重启条件：reader 基础设施落地且 profiling 证明保守终止不足。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- LCA 逻辑只允许存在于 `internal/merge_base.rs`；Phase B 迁移 `log`/`rebase` 时必须先有 golden 回归与 legacy 开关。
