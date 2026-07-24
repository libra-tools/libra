# Libra Worktree 架构 vs 传统 Git Worktree

本文将 Libra 当前的 Worktree 机制与传统 Git worktree 进行对比。
重点关注 `docs/agent` 下的 Agent 执行路径：任务级隔离的 worktree，
让 Libra 能在不让中间状态泄漏进主工作区的前提下，运行代码生成与
校验。

Libra 拥有两套相关但不同的 worktree 机制：

- **面向用户的关联 worktree（linked worktree）**，由 `libra worktree`
  提供，实现于
  [`src/command/worktree.rs`](../../src/command/worktree.rs)，并可选地在
  [`src/command/worktree-fuse.rs`](../../src/command/worktree-fuse.rs)
  中支持 FUSE。
- **Agent 任务 worktree**，实现于
  [`src/internal/ai/orchestrator/workspace.rs`](../../src/internal/ai/orchestrator/workspace.rs)，
  并通过
  [`src/internal/ai/runtime/environment.rs`](../../src/internal/ai/runtime/environment.rs)
  对外暴露。

Agent 机制是更高层的架构。它把 worktree 当作由 Libra 调度器（Scheduler）
拥有的执行环境，而不是由 VCS 拥有的长生命周期分支检出。

## 当前 Libra Worktree 模型

### 仓库关联 worktree

`libra worktree` 管理持久化的关联工作树（linked working tree），它们共享
同一个仓库存储目录。

```text
+--------------------------+        +--------------------------+
| main workspace           |        | linked workspace         |
| /repo                    |        | /repo-feature            |
|                          |        |                          |
| .libra/                  |<-------| .libra -> /repo/.libra   |
| worktrees.json           |        | files restored from HEAD |
| SQLite DB / object store |        |                          |
+--------------------------+        +--------------------------+
```

重要特性：

- 共享的 `.libra` 目录包含 SQLite 数据库、对象存储、配置以及
  `worktrees.json`。
- 每个关联 worktree（新隔离布局）包含它自己真实的 `.libra` gitdir——
  本地目录（非 symlink），保存该 worktree 私有的 HEAD、index 和 HEAD
  reflog，并记录 `commondir` 指针（指回共享存储）与稳定 `worktree_id`。
  由更早版本创建的 worktree 可能仍是旧的 `.libra` symlink 共享布局。
- `worktrees.json` 自 v0.19.57 起为 registry v2：顶层为
  `{schema_version: 2, entries: [...]}`（顶层键由 `worktrees` 更名为
  `entries`，使 v1 解析器对 v2 文件 fail-closed），每个条目存储规范化
  路径、主 worktree 标记、锁状态、可选锁定原因，linked 条目还持久化其
  stable `worktree_id`。旧 v1 文件在首个变更类命令（registry 锁内）就
  地升级并回填 id；无锁读取（`worktree list`）不重写文件。数据库迁移
  2026072401 的 capability 标记使 pre-v2 二进制在连接时即被拒绝。
- 状态写入是原子的：Libra 先写入一个临时 JSON 文件，再将其重命名
  到目标位置。
- `libra worktree add` 会创建一个空的关联目录，并在 `HEAD`
  存在时，将已提交的 `HEAD` 内容恢复到该目录中。它不会
  复制仅暂存（staged-only）的 index 状态。
- `libra worktree remove` 会注销该 worktree，但有意不
  删除目录，以降低意外丢数据的风险。
- `libra worktree repair` 会对注册表条目去重，并恢复
  「恰有一个条目是主 worktree」这一不变量。`repair <path>` 则依据
  registry 持久化的 stable id 恢复 linked worktree 缺失/损坏的
  `.libra/worktree_id` 与 `commondir`（指向另一存储的有效指针会被
  拒绝，且拒绝不产生写入）。

启用 `worktree-fuse` 特性后，Libra 还可以维护
`worktrees-fuse.json`，以及位于 `.libra/worktrees-fuse/` 下的
每个 worktree 各自的 upper 目录。目标路径会被挂载为 FUSE 叠加层
（overlay）：当前工作区作为 lower 层，每个 worktree 各自的 upper
目录存储写入。该模式可以在创建 worktree 时选择或创建一个分支，
同时仍使用由 Libra 管理的元数据。

### Agent 任务 worktree

Agent 任务 worktree 是临时的。Libra 为一次任务尝试创建一个，
在其中运行工具，通过一个合约感知（contract-aware）的回放步骤
将成功的实现变更同步回去，然后将其清理掉。

```text
primary workspace
      |
      | snapshot_workspace()
      v
+------------------------------+
| TaskWorktree baseline        |
| - file hashes                |
| - symlink targets            |
| - gitignore-aware traversal  |
| - protected metadata skipped |
+------------------------------+
      |
      v
+--------------------------------------------------------------+
| isolated task workspace                                      |
|                                                              |
| FUSE backend when available:                                 |
|   workspace/  = mounted overlay                              |
|   lower/      = materialized baseline                        |
|   upper/      = writes + .libra symlink                      |
|                                                              |
| Copy backend fallback:                                       |
|   workspace/  = materialized baseline + .libra symlink       |
|   file copies prefer CoW clonefile/FICLONE, then copy        |
+--------------------------------------------------------------+
      |
      | task tools run here
      v
+------------------------------+
| sync_task_worktree_back()    |
| - diff against baseline      |
| - enforce touchFiles/scope   |
| - reject concurrent changes  |
| - copy/delete changed paths  |
+------------------------------+
      |
      v
primary workspace updated only after successful replay
```

预备（provisioning）步骤：

1. Libra 通过 `snapshot_workspace()` 对主工作区拍快照（Snapshot）。
   该遍历遵守 `.gitignore`，保留 symlink 条目而不跟随它们，
   并跳过受保护的元数据目录，例如 `.git`、`.libra`、`.codex` 和
   `.agents`。
2. Libra 分配一个临时根目录，其命名包含 backend、进程 id
   以及任务 UUID。
3. 在带有活跃 Tokio 运行时的 Unix 上，Libra 会首先尝试 FUSE 叠加
   backend。它将基线（baseline）物化到 `lower/`，将共享的
   `.libra` 存储链接进 `upper/`，挂载 `workspace/`，并执行一次健康
   检查。
4. 如果 FUSE 不可用或不健康，Libra 会回退到 copy
   backend。它将 `.libra` 链接进 `workspace/`，在其中物化
   快照，并在可能时使用平台的写时复制（copy-on-write）克隆操作。
5. 对于实现类任务，工具注册表与 hook runner 会在任务运行前重新绑定
   到隔离的工作目录。

执行与回放规则：

- 实现类任务在任务 worktree 内运行。它们的变更只有在任务
  成功完成时才会被同步回去。
- 门禁（gate）任务同样在隔离的 worktree 内运行，但其输出会在检查
  后被丢弃。这能让校验产生的临时文件不进入主工作区。
- 同步回写通过将任务快照与捕获的基线进行对比来计算变更
  路径。
- 在回放某个已变更路径之前，Libra 会强制执行任务写入合约：
  `touchFiles` 存在时优先生效，否则由 `scope_in` 和
  `scope_out` 定义允许写入的区域。
- Libra 会检查主工作区中相应路径是否仍然
  与基线一致。如果用户或另一个任务在此期间
  并发地修改了它，则同步回写会失败，而不会将其覆盖。
- 调度器层面的互斥锁会串行化同步回写，因此并行的任务 worktree
  可以并发运行，但变更逐个集成。
- 清理时会在需要时卸载 FUSE worktree，并移除该临时
  根目录。

## 传统 Git Worktree 模型

Git worktree 是附着于某个 Git 仓库的持久化检出。
它们以分支/ref 为导向，而非以任务为导向。

```text
main checkout
      |
      | common Git directory
      v
.git/
  objects/
  refs/
  worktrees/
    feature/
      HEAD
      index
      gitdir
      commondir

linked checkout
  .git  -> text file: "gitdir: /repo/.git/worktrees/feature"
  files checked out for that worktree's HEAD
```

重要特性：

- 每个关联 worktree 都有一个 `.git` 文件，指向位于公共
  `.git/worktrees/` 区域下、属于该 worktree 的管理目录。
- 该 worktree 专属的管理目录存储 worktree 本地的状态，例如
  `HEAD`、`index`，以及指回公共 Git 目录的指针。
- 对象存储和大多数 ref 通过公共目录共享。
- Git 默认会阻止同一分支在多个
  worktree 中被同时检出。
- `git worktree add` 通常会创建或检出一个分支，或在某个 commit 上
  创建一个分离（detached）worktree。
- `git worktree remove` 默认在通过安全检查后删除该关联工作树。
- `git worktree prune`、锁文件以及 repair 操作用于管理陈旧的
  元数据和缺失的目录。

这种设计对人类基于分支的工作流非常出色。但它与 Agent 执行的
契合度较低，因为 Git worktree 并不编码任务合约、调度器状态、审计
事件或安全回放语义。

## 架构差异

| 方面 | 传统 Git Worktree | Libra Worktree |
|---|---|---|
| 主要拥有者 | Git ref 与检出机制 | Libra 调度器（Scheduler）与执行环境 |
| 主要隔离单元 | 分支、分离 commit，以及每个 worktree 各自的 index | 任务尝试、基线快照与写入合约 |
| 元数据布局 | `.git/worktrees/<id>` 下分散的文件系统控制文件，外加 `.git` 指针文件 | 持久化 worktree 使用人类/Agent 可读的 JSON；Agent worktree 使用临时任务状态 |
| 仓库存储链接 | `.git` 文件指向每个 worktree 各自的 Git 管理目录；`commondir` 指回公共存储 | 每个关联 worktree 拥有真实本地 `.libra` gitdir，内含 `commondir` 指针指回共享 Libra 存储（legacy 布局为 symlink）；任务的 FUSE backend 将存储链接进可写的 upper 层 |
| 起始内容 | Git 从某个分支或 commit 检出到 worktree 与 index | CLI 关联 worktree 恢复 `HEAD`；Agent worktree 对当前工作区状态拍快照，包括未被忽略的未提交文件 |
| 并行工作 | 多个持久化的分支检出 | 多次临时任务尝试可并行运行，而无需分配分支 |
| 集成方式 | 用户运行 merge、rebase、cherry-pick 或手动复制 | Libra 在通过作用域与并发检查后，将成功的任务变更同步回去 |
| 失败行为 | 失败的实验会留下持久化 worktree，直到被移除 | 失败的 Agent 任务会被丢弃；主工作区保持不变 |
| 校验临时空间 | 除非手动隔离，否则命令可能弄脏检出 | 门禁（gate）任务在用完即弃的 worktree 中运行，因此临时文件不会泄漏 |
| 安全边界 | Git 保护分支检出冲突与部分未提交状态 | Libra 保护声明的写入作用域、主工作区的并发编辑、被忽略的元数据，以及非破坏性移除 |
| 可审计性 | Git 在用户操作后记录 commit 与 reflog | Libra 围绕隔离执行记录任务/运行时事件（Event）、工具调用、证据（evidence）与补丁产物 |

## 为何 Libra 的设计更契合 Agent 执行

### 任务优先的隔离

一个 Agent 任务需要的是一个隔离的文件系统，未必是一个新分支。
Libra 可以从同一基线并行运行多个实现类任务，
然后只回放成功且在作用域内的编辑。Git worktree
把分支检出作为核心抽象，即使真正的目标只是一个短生命周期的任务
沙箱，也会增加分支管理开销。

### 合约感知的回放

Git worktree 隔离的是目录，但它们并不知道某个任务被
允许修改什么。Libra 把任务合约带入同步回写：
`touchFiles`、`scope_in` 和 `scope_out` 会在执行之后、
主工作区被修改之前被强制执行。这把「Agent 修改了某个
目录」变成了「Agent 恰好修改了允许的路径，并且主
工作区仍与基线一致」。

### 干净的失败语义

Libra 不会让失败的实现尝试或门禁临时文件
污染用户的检出。任务的 worktree 是临时的；如果任务
失败，清理会移除该 worktree，且不发生同步回写。这使得
重试与重新规划的循环在运维上远比每次尝试创建一个持久
分支的工作流更廉价。

### 并发但不静默覆盖

并行任务可以独立执行。集成是串行化的，并
逐个路径地将主工作区与捕获的基线进行比对。
如果另一个任务或用户修改了相同的路径，Libra 会让该回放失败，
而不是悄悄覆盖更新后的状态。

### 面向性能的物化

Libra 使用可用的、最廉价的隔离机制：

- 当运行时与挂载健康时，在 Unix 上使用 FUSE 叠加层。
- 在使用 copy backend 时，通过 macOS 上的 `clonefile` 或
  Linux 上的 `FICLONE` 进行写时复制（copy-on-write）克隆。
- 普通文件复制作为最终回退。

快照遍历同样遵守 ignore 文件，因此当被忽略时，诸如 `target/` 或
`node_modules/` 之类生成的构建产物不会被复制进任务
worktree。

### 共享仓库能力而无需复制存储

任务 worktree 通过链接到共享的仓库存储，让 `.libra` 保持可见。
需要仓库元数据的工具可以在隔离的工作区中运行，而无需
复制数据库或对象存储。与此同时，
元数据目录被排除在内容快照之外，因此
执行 diff 聚焦于项目文件。

### 更安全的持久化 worktree 使用体验

对于长生命周期、面向用户的 worktree，Libra 将注册表保存在一个
可检视的 `worktrees.json` 文件中，并避免破坏性的默认移除。
这有意比 Git 那种会删除目录的
`worktree remove` 行为更保守，也对那些关联检出中可能存在
未提交工作的 AI 辅助工作流更友好。

## 当前取舍

Libra 的设计针对 Agent 执行做了优化，因此它有意
不去匹配每一项 Git worktree 行为：

- 非 FUSE 的 `libra worktree add` 路径不会为每个
  worktree 分配一个分支。它创建一个从 `HEAD` 恢复的关联目录。
- 基于 FUSE 的用户 worktree 是可选的，且依赖平台/运行时。
  如果挂载路径不可用，Agent 任务 worktree 会回退到
  copy backend。
- Agent 任务 worktree 是临时的。它们不是
  长生命周期人类检出的替代品。
- 同步回写是一种保守的文件级回放。它会拒绝并发
  变更，而不会尝试进行语义合并。
- 由于任务 worktree 链接共享的 `.libra` 存储，调度器策略
  和提示词必须持续确保普通的 coder 任务不会执行
  非预期的版本控制变更。

这些取舍是有意为之的。传统 Git worktree 针对
人类的分支检出做优化。Libra Worktree 针对可复现、
合约约束、可审计的 Agent 执行做优化。
