# `libra review`

使用外部 agent CLI 运行只读代码 review（AG-22）。

## 概要

```bash
libra review --agent <slug>... [--since <rev>] [--checkpoint <id>] [--json]
libra review list [--json] [--limit <n>] [--cursor <token>]
libra review show <run_id> [--json]
libra review cancel <run_id>
libra review clean [--run <run_id>] [--all]
libra review attach <run_id> <file> [--json]
```

## 产物（Artifacts）

run 结束时会把 `findings.md` 对象化进对象库：manifest 的 `findings_oid` 是内容寻址
blob，带 `object_index` 标记，因此 cloud sync / retention 可追踪、`libra agent doctor`
可在其丢失时修复。`libra review attach <run_id> <file>` 把外部文件以
`provenance=manual` 记入 run 的审计链——字节先脱敏、再对象化，并追加到 manifest 的
`manual_attach` 列表；attach 永不修改 findings 或 run 状态。

## 说明

`libra review` 将一段固定的 review prompt 扇出给一个或多个外部 reviewer
CLI，并把它们的 findings 记录为一次可审计的 run。首批可启动的 reviewer 为
`claude-code`、`codex`、`opencode`；其它 agent slug 会在任何进程被拉起之前
返回可操作的拒绝错误。

每个 reviewer 都在**隔离 workspace** 中运行——按 ignore 规则物化的仓库镜像
（`.env.test` 等被 gitignore 的 secret 文件不会进入镜像），使用最小权限只读
的 CLI 形态，环境变量只注入 allowlist。reviewer 永远不会在仓库工作树本身
运行。

run 在前台阻塞直到每个 reviewer 得到结果，并恰好落在五个 terminal state
之一：`success`、`error`、`cancelled`、`timeout`、`partial`。按 Ctrl-C（或
发送 SIGTERM）取消 run 时走与 `libra review cancel` 完全相同的 cleanup 路径：
杀死 reviewer 进程树、排空 reader task、释放 workspace，并把 run 标记为
`cancelled`。

run 状态持久化在 `.libra/sessions/agent-runs/<run_id>/` 下：`state.json`、
`manifest.json`、`findings.md` 以及每个 reviewer 的
`reviewers/<slug>.stdout.redacted.log` / `.stderr.redacted.log`。所有落盘的
reviewer 输出都经过 secret redaction 管线；每条流的输出上限为 64 KiB（刷屏
的 reviewer 会被截断并打上标记，不会阻塞其它 reviewer）。

reviewer findings 是**不可信自由文本**。`libra review show` 在渲染
`findings.md` 之前总是剥离 ANSI/终端控制序列，恶意 reviewer 无法伪造终端
输出；JSON 输出携带的也是同样经过消毒的渲染结果。

### 范围选择

记录在 run 里的 `target_scope` 标注了 reviewer 被要求 review 的范围：

- 默认：`HEAD~1..HEAD`（最近一次提交的变更）；
- `--since <rev>`：`<rev>..HEAD`；
- `--checkpoint <id>`：`checkpoint:<id>`（来自
  `libra agent checkpoint list` 的 agent checkpoint）。**尚未实现** ——
  命令会 fail-closed 拒绝执行，而不是在 checkpoint 标签下静默 review
  当前工作区；可先用 `libra agent checkpoint show <id>` 直接查看捕获状态。

### 分页

`libra review list` 走统一的 keyset 分页契约：默认 `--limit 50`，上限
500，按 `created_at DESC, run_id DESC` 排序。JSON envelope 携带
`schema_version`、`items`、`next_cursor`（不透明 token——原样回传）和
`has_more`。

### `--fix`

`libra review --fix` 依赖内部 AgentRuntime fix bridge，该 bridge 尚未落地。
它总是返回稳定错误码 `LBR-AGENT-010`，绝不伪装成功。只读 findings 仍可通过
`libra review show` 查看。

## Examples

```bash
# 用一个 reviewer review 最近一次提交
libra review --agent codex

# 把同一 review 并发扇出给两个 reviewer
libra review --agent codex --agent claude-code

# review 自某个修订以来的全部变更
libra review --agent codex --since v1.2.0

# checkpoint 范围的 review 在 checkpoint 物化落地前 fail-closed
libra review --agent codex --checkpoint <checkpoint_id>

# 结构化 run 结果（terminal state、逐 reviewer 结果）
libra review --agent codex --json

# 列出 run，再取下一页
libra review list
libra review list --limit 10 --cursor <token>

# 查看一个 run（状态、manifest 摘要、消毒后的 findings）
libra review show <run_id>
libra review show <run_id> --json

# 取消一个正在运行的 review（与 Ctrl-C 相同的 cleanup）
libra review cancel <run_id>

# 删除 run 目录
libra review clean --run <run_id>
libra review clean --all
```

## 并发

`review` 与 `investigate` 在仓库范围内共享一个 run 级并发预算。最多
`agent.max_concurrent_runs` 个 run（默认 `2`）同时执行；预算占满时启动的 run 会进入队列
阻塞等待（阻塞前台进程，`Ctrl-C` 取消等待并让队列向后推进）。若等待队列已达上限（10），
新 run 会以稳定错误码 `LBR-AGENT-014`（退出码 128）fail-closed 拒绝，而非超出预算。用
`libra config set agent.max_concurrent_runs <N>` 调整上限。

## 退出状态

- `0` —— run 落在 `success`、`partial`、`timeout` 或 `cancelled`（terminal
  state 会在输出中报告）；子命令执行成功。
- 非零 —— 用法错误、run 落在 `error` terminal state、未知 run id、
  `--fix`（稳定错误码 `LBR-AGENT-010`），或 run 队列已满（`LBR-AGENT-014`）。

## 另请参阅

- `libra agent` —— 外部 agent 捕获、checkpoint 与 hook
- `docs/development/commands/review.md` —— 架构与安全说明
