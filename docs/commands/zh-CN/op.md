# `libra op`

检查和恢复命令级 operation history。

## 概要

```bash
libra op log [OPTIONS]
libra op show [OPTIONS] <OP_REF>
libra op restore [OPTIONS] <OP_REF>
```

## 说明

`libra op` 在命令行上暴露 operation service 和 wrapper layers 持久化的 operation graph。

当前支持三个子命令：

- `op log`：列出已记录 operations，支持分页和可选命令过滤。
- `op show`：检查一个 operation，并可选显示捕获的 restore view。
- `op restore`：将 HEAD 和分支 refs 恢复到先前捕获的 view。

## Operation References

`<OP_REF>` 可以是：

- 具体 operation id，例如 `019e3f00-8ee5-7e62-a54c-0ab1f1bba0f9`
- reflog 风格索引，例如 `@{0}` 表示最新 operation，`@{1}` 表示前一个

## `libra op log`

列出 operation history。

```bash
libra op log [--page <N>] [-n <PER_PAGE>] [--command <NAME>] [--verbose]
```

### 选项

### `-n, --number <PER_PAGE>`

每页显示的 operations 数量。默认 `50`。

```bash
libra op log -n 20
```

### `--page <N>`

要显示的页码。默认 `1`。

```bash
libra op log --page 2 -n 20
```

### `--command <NAME>`

按精确命令名过滤 operations，例如 `branch` 或 `op restore`。

```bash
libra op log --command branch
libra op log --command "op restore"
```

### `--verbose`

将一个 operation 显示为包含 actor、status 和 timestamp 的多行块。

```bash
libra op log -n 5 --verbose
```

## `libra op show`

检查单个 operation。

```bash
libra op show [--view] <OP_REF>
```

### 选项

### `--view`

打印捕获的 restore view，包括 HEAD target 和 refs。

```bash
libra op show @{0} --view
```

## `libra op restore`

从先前捕获的 operation view 恢复受支持的 HEAD/ref 状态，而不是任意工作树或嵌套仓库内容。HEAD 和捕获的 branch refs 会重置为目标 view，本地分支中不存在于该 view 的会被 prune，因此 restore 会复现该 operation 的精确本地分支集合。恢复后的 HEAD branch 始终保留；remote-tracking refs 和 Libra-owned internal refs（locked `main`/`intent`/`traces` branches 以及保留 `libra/` namespace，例如 AI history branch `libra/intent`）永不 prune。

```bash
libra op restore [--what <all|working-copy|index|sequencer|sparse|head>] \
  [--confirm-repo-wide] [--force] [--dry-run] <OP_REF>
```

### 选项

### `--force`

即使工作树 dirty，也允许继续 restore。这不会增加恢复能力，也不会把 `Partial` 捕获变成完整快照。

```bash
libra op restore @{0} --force
```

### `--dry-run`

显示目标 HEAD 和 refs，但不写入新的 restore operation。

```bash
libra op restore @{0} --dry-run
```

### `--what <FACET>`

选择要恢复的状态 facet。默认值为 `all`，也可以选择 `working-copy`、
`index`、`sequencer`、`sparse` 或 `head`。v2 restore 会输出 receipt，其中包含
目标 view、选择的 facet、路径数量，以及真实恢复产生的新 operation ID。

### `--confirm-repo-wide`

显式确认目标 view 含有多个 workspace。引擎仍只会对当前请求固定的 worktree
应用恢复，并拒绝不包含当前 worktree 的目标。

机器调用方可以使用命令的标准 `--json` 输出模式取得 receipt。dry-run 不写入
operation，也不会修改工作区。

## `libra op undo`、`redo` 和 `revert`

这些命令都会追加新的 operation，不会改写或删除 commit 对象。`undo` 要求目标
是当前唯一 head，并移动到它的 parent view；`redo` 只接受当前的 undo head，并
重放该 undo 记录的源 operation；`revert` 必须显式提供 `--parent`，并将该
parent 的 view 作为逆向结果应用。

```bash
libra op undo <OP_REF> [--force] [--confirm-repo-wide] [--dry-run]
libra op redo <UNDO_OP_REF> [--force] [--confirm-repo-wide] [--dry-run]
libra op revert <OP_REF> --parent <PARENT_OP_REF> \
  [--force] [--confirm-repo-wide] [--dry-run]
```

三个命令都支持 JSON receipt，包含选择的 facet、变更路径数量、目标 view，以及
实际发布时的新 operation ID。dry-run 只计算计划，不发布 operation。
工作区 dirty 时默认拒绝，必须显式使用 `--force`。

## `libra op doctor`

检查 operation 对象闭包、heads、未完成 journal 和 workspace pointer。默认只读；
`--fix` 才会执行 journal 恢复和 pointer 重建，`--dry-run` 只报告计划中的修复。

```bash
libra op doctor [--fix] [--dry-run]
```

## 示例

```bash
# 列出最新十个 operations
libra op log -n 10

# 只显示第 2 页上的 branch operations
libra op log --command branch --page 2 -n 5

# 检查最新 operation 及其 view snapshot
libra op show @{0} --view

# 恢复到前一个 operation view
libra op restore @{1}

# 预览 restore，不修改仓库状态
libra op restore @{1} --dry-run
```

## 说明

- `op restore` 成功时会记录一个新的 `op restore` operation。
- `op restore --dry-run` 不写入新 operation。
- Restore 会重置 HEAD 和目标 view 中捕获的 branch refs，并 prune 该 view 中不存在的本地分支（恢复后的 HEAD branch 始终保留；remote-tracking refs 保持不变）。

### Operation 作用域执行（未发布）

对于经过 v2 operation middleware 的变更，每个 operation 都保持独立、已固定的仓库／工作树请求上下文，包括异步执行期间。独立 linked worktree 使用各自的 private gitdir 与 scope lease，即使共享仓库存储，也不会因这一 lease 彼此串行化；其他仓库锁和命令限制仍然适用。

`<private-gitdir>/info/operation-v2.lock` 的 scope lease 只尝试一次非等待式加锁：默认争用等待时间为零，没有自动重试循环。同一 scope 的竞争 operation 会在业务回调或 operation journal 开始之前被拒绝。Busy 错误包含 scope 和锁路径；请等待另一个 operation 结束后重试。

锁由打开的文件持有，文件关闭即释放锁；锁文件本身可以在 operation 结束后继续存在。**不要删除锁文件来解决争用**，也不要在 operation 活跃期间替换 private metadata 目录。这些规则不承诺任意文件系统 I/O 的硬时限，也不保证任意并发祖先路径替换下的安全；没有新增 CLI 参数、环境变量或配置项。

持久锁沿用仓库既有的 `core.sharedRepository` 文件权限。对于
group/all/数值 shared 仓库，由一个用户首次创建的锁仍能被 shared mode
允许的其他用户打开；default/false/umask 模式继续遵循进程 umask。

### 隔离 agent 任务的 sync-back operation（未发布）

隔离的 `libra code` DAG 任务在临时 copy 或 FUSE workspace 中运行工具，
而不是在具有独立 operation scope 的真实 linked worktree 中运行。该
workspace 内的变更工具仍然经过 permission、hardening、audit、redaction
与 sandbox 检查，但不会针对主 workspace 为每次调用分别发布
`agent.tool.*` operation。

任务完成后，Libra 会串行地把变更 replay 到主 workspace。成功且改变了
捕获 view 的 replay 会发布一个 `agent.task.sync-back`
`WorkspaceMutation`；其 `causal_context_id` 保存任务 UUID，以便把该
operation 归因到具体任务。若 replay 后 view 不变，则不会创建 operation。

如果主 scope lease 正忙，scheduler 会先使用短暂且有上限的退避，仅重试
sync-back；已完成的任务 workspace 会被保留，也不会消耗任务的 fresh-baseline
重试预算。持续争用之后才可能回落到普通任务重试策略。若 operation
pointer/CAS 在 replay 开始前发生变化，则仍须从新的 baseline 重新运行。
如果 replay 已完成、但 post-snapshot 或 operation publication 失败，Libra
**不会**自动重试：主 workspace 可能已经包含任务变更。请按错误提示先检查
`libra status` 与 `libra op log`，再决定恢复还是重新运行。

### HEAD 捕获的权威来源（未发布）

新 v2 捕获读取 pinned worktree scope 的 SQLite HEAD 行，不读取 HEAD sidecar，也不凭空回退到 `main`。HEAD 行缺失、重复、损坏或查询失败时，会拒绝捕获，而不是用假定 HEAD 返回成功快照。有效 detached HEAD 的 commit 保留为快照引用的 root；仅 HEAD 改变也会反映到新快照的 content identity。这些行为已在未发布分支实现，完整集成、实现审查与发布验收仍待完成。

这项修复不会重写既有不可变 manifest，也不代表已复现实际垃圾回收数据丢失；它不增加完整恢复能力或整个快照的 mixed-hash 并发支持。

### 当前分支的捕获契约（未发布）

以下 operation-v2 行为已在当前未发布分支实现，集成与发布验收仍待完成。既有 legacy `op` 查询／HEAD-ref 恢复接口与 v2 捕获基础设施并存。v2 的 `Full` 捕获不代表已支持完整恢复。

- 在稳定工作树下，已跟踪 gitlink（index mode `160000`）及其子路径是父仓库快照的不透明边界：不得枚举、读取嵌套内容，或把内容持久化成父仓库 snapshot blobs。这不是子模块备份功能。
- 该边界必须同时覆盖字面路径与物理目录别名，包括文件系统认定为同一目录的大小写／Unicode 拼写别名；不同拼写不得绕过边界。
- 非字面 file 或 symlink 路径若具有相同文件系统身份，也可能是 hardlink 别名。扫描器无法安全确认边界时，必须跳过不安全的捕获并将快照标记为 `Partial`，不能假设路径安全。这不代表所有 hardlink 都会触发 `Partial`。
- 索引损坏／不可读，或文件系统身份未知时，同样必须保守产生 `Partial` 捕获，不能假设索引为空后无限制扫描，也不能把缺失内容报告为完整捕获。
- Ignore 检查共用扫描的原始 deadline，不为每个路径重新计时。Ignore 判定未知、ignore 文件读取失败（含 invalid UTF-8）或该 deadline 到期时，捕获标记为 `Partial` 并丢弃 visible-file listing；不会 hash 或持久化该被拒绝 listing 中的文件，不能把不可读规则当成允许扫描的空规则。
- 检测到身份漂移时，同样丢弃 listing 并把捕获标记为 `Partial`。快照不会冻结外部文件系统，也不保证任意持续并发改写下的原子视图，包括两次检查之间先改变再恢复的 ABA。Listing 可能读取 ignore 规则，不是仅 metadata 的操作；扫描 deadline 不等于所有捕获阶段或任意文件系统 I/O 均有 30 秒硬时限。
- 命令结果、快照完整性与可恢复性是不同维度。命令失败仍可能留下 operation／pre-snapshot 记录，但这不允许捕获不透明的嵌套内容。`Full` 或 `--force` 都不会赋予恢复未捕获内容或不受支持状态的能力。

当前分支尚未发布的数据库过渡见 [operation-v2 收敛](init.md#operation-v2-收敛当前分支未发布)。
