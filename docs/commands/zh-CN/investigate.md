# `libra investigate`

使用外部 agent CLI 运行只读的严格轮询（round-robin）调查（AG-23）。

## 概要

```bash
libra investigate start --topic <text> --agent <slug>... [--max-turns <n>] [--quorum <n>]
libra investigate list [--json] [--limit <n>] [--cursor <token>]
libra investigate show <run_id> [--json]
libra investigate continue <run_id>
libra investigate cancel <run_id>
libra investigate clean [--run <run_id>] [--all]
libra investigate fix <run_id>
libra investigate attach <run_id> <file> [--json]
```

## 产物（Artifacts）

run 结束时把 `findings.md` 对象化进对象库（manifest 的 `findings_oid` 是内容寻址、带
`object_index` 标记、可被 doctor 修复的 blob）。`libra investigate attach <run_id>
<file>` 把外部文件以 `provenance=manual` 记入审计链：字节先脱敏、再对象化，并追加到
manifest 的 `manual_attach` 列表；永不修改 findings 或 run 状态。

## 说明

`libra investigate` 以**严格轮询**方式调查一个主题：每轮只有一个 investigator
运行，按 `--agent` 顺序依次拉起为最小权限的只读外部 CLI。这与 `libra review`
的并发扇入模型**不同**——investigator 永不并行运行。首批可启动的 investigator
为 `claude-code`、`codex`、`opencode`；其它 agent slug 会在任何进程被拉起之前
返回可操作的拒绝错误。

每个 investigator 都在**隔离 workspace** 中运行——按 ignore 规则物化的仓库镜像
（`.env.test` 等被 gitignore 的 secret 文件不会进入镜像），使用最小权限只读的
CLI 形态，环境变量只注入 allowlist。investigator 永远不会在仓库工作树本身运行。

每一轮从 investigator 的 stdout 收集其 stance（经 redaction），追加到 run 的
`stances` 列表与单写 `findings.md`，并推进轮询位置
（`next_agent_idx` / `turn` / `completed_rounds`）。

### Terminal state 与 pause

一次 drive 要么 **terminal**，要么 **paused**：

- **terminal**（记录 `terminal_state`）：
  - `quorum` —— 至少 `--quorum` 个不同的 investigator 提交了 concluding
    stance（investigator 通过在输出中包含 "conclude" 表示结论）；
  - `max_turns` —— 在达成 quorum 前耗尽 turn 预算（是否读作 "success/partial"
    仅信息性，取决于是否记录了 findings）；
  - `cancelled` —— run 被取消（`investigate cancel` / Ctrl-C / SIGTERM，
    同一条 cleanup 路径）；
  - `timeout` —— 超过 run 级墙钟预算（`max_turns × 120s`，上限 3600s）；
    fail-closed，释放所有进程/锁/workspace。
- **paused**（记录 `pending_turn`，可用 `continue` 续跑）：
  - `stalled` —— 一个成功的 turn 未产出新 findings（空输出）；
  - `agent_failure` —— investigator 启动失败、非零退出或超过 per-turn 截止。

`libra investigate continue <run_id>` 从 pending turn 续跑一个 paused run。
OS 级 run lock 使同一 run 的并发 `continue` fail-closed 并给出可操作错误，
因此一个 run 永不被两个进程同时驱动。

run 状态持久化在 `.libra/sessions/agent-runs/<run_id>/`：`state.json`
（轮询状态——`turn`、`next_agent_idx`、`stances`、`pending_turn`、`quorum` …）、
`manifest.json`（`kind: "investigate"`）、`findings.md`，以及每个 investigator 的
`reviewers/<slug>.stdout.redacted.log` / `.stderr.redacted.log`。所有落盘的
investigator 输出都经过 secret redaction 管线；每条流上限 64 KiB（刷屏的
investigator 会被截断并打上标记）。

### Untrusted seed 与 findings

调查主题是**不可信 seed**（issue link 或操作者文本）。它——以及作为上下文注入的
每个先前 investigator stance——在进入任何 agent prompt 之前都会经 redaction 并以
明确的 spotlighting 定界包裹，因此永不会被误认为指令。investigator findings 是
**不可信自由文本**；`libra investigate show` 在渲染 `findings.md`（与 topic）之前
总是剥离 ANSI/终端控制序列，恶意 investigator 无法伪造终端输出。JSON 输出携带的
也是同样经过消毒的渲染结果。

### Quorum 与 turns

- `--max-turns <n>` 限制 investigator turn 数（默认 6）。
- `--quorum <n>` 是达成收敛所需的**不同** investigator 数（默认为给定的
  `--agent` 数——全体共识）。大于 agent 数的值会被 clamp 并给出提示。

### 分页

`libra investigate list` 走统一 keyset 分页契约：默认 `--limit 50`，上限 500，
按 `started_at DESC, run_id DESC` 排序。JSON envelope 携带 `schema_version`、
`items`、`next_cursor`（不透明 token——原样回传）和 `has_more`。

### `fix`

`libra investigate fix <run_id>` 依赖内部 AgentRuntime fix bridge，该 bridge
尚未落地。它总是返回稳定错误码 `LBR-AGENT-010`，绝不伪装成功。由于 topic 是
不可信 seed，mutating fix 还需显式 approval；bridge 落地后，未经批准的
untrusted-seed mutation 会返回 `LBR-AGENT-011`。只读 findings 仍可通过
`libra investigate show` 查看。

## Examples

```bash
# 用一个 agent 启动轮询调查
libra investigate start --topic "why is startup slow" --agent codex

# 在两个 agent 间轮询（严格，一次一个）
libra investigate start --topic "auth bug" --agent codex --agent claude-code

# 限制 turn 数并要求两个 concluding agent
libra investigate start --topic "memory leak" --agent codex --max-turns 8 --quorum 2

# 列出 run，再取下一页
libra investigate list
libra investigate list --limit 10 --cursor <token>

# 查看一个 run（状态、stances、消毒后的 findings）
libra investigate show <run_id>
libra investigate show <run_id> --json

# 续跑一个 paused（stalled / agent-failure）run
libra investigate continue <run_id>

# 取消一个正在运行的调查（与 Ctrl-C 相同的 cleanup）
libra investigate cancel <run_id>

# 删除 run 目录
libra investigate clean --run <run_id>
libra investigate clean --all
```

## 并发

`investigate` 与 `review` 在仓库范围内共享一个 run 级并发预算。最多
`agent.max_concurrent_runs` 个 run（默认 `2`）同时执行；预算占满时启动的 run 会进入队列
阻塞等待（`Ctrl-C` 取消等待）。若等待队列已达上限（10），新 run 会以稳定错误码
`LBR-AGENT-014`（退出码 128）fail-closed 拒绝。用
`libra config set agent.max_concurrent_runs <N>` 调整上限。

## 退出状态

- `0` —— run 落在 `quorum`、`max_turns`、`timeout` 或 `cancelled`，或 PAUSE
  （`stalled` / `agent_failure`）；子命令执行成功。
- 非零 —— 用法错误、run 落在 `error` terminal state、未知 run id、对已锁 run 的
  并发 `continue`、`fix`（稳定错误码 `LBR-AGENT-010`），或 run 队列已满（`LBR-AGENT-014`）。

## 另请参阅

- `libra review` —— 只读并发 agent 代码 review（AG-22）
- `libra agent` —— 外部 agent 捕获、checkpoint 与 hook
- `docs/development/commands/investigate.md` —— 架构与安全说明
