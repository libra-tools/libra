# Libra Web API 设计

> **状态**：草稿 / 接口规范
> **负责人**：Web 团队、Agent runtime 团队
> **读者**：Rust 后端实现者、web 客户端工程师
> **最后更新**：2026-04-25

本文档定义了 **Libra Web 客户端**（位于 `web/` 下的 Next.js / React 应用）向 **Libra agent 后端**（由 `libra code` 暴露的 Rust runtime）所要求的 HTTP + 流式 API 契约。当前 web 客户端是基于进程内的 mock 数据层（`web/src/lib/mock/*`）进行渲染的；一旦 Rust 端开始提供这些端点，客户端只需把 mock 模块替换为真正的 fetch 层，UI 无需任何改动。

该契约由 [docs/ai/workflow.md](../ai/workflow.md) 中描述的五阶段流水线塑形：

```
Phase 0 Intent  →  Phase 1 Plan  →  Phase 2 Execution  →  Phase 3 Validation  →  Phase 4 Release
```

web 应用中的每一个 UI 界面都映射到其中一个或多个阶段：

| UI 界面                         | 阶段                | 小节                                   |
|--------------------------------|---------------------|----------------------------------------|
| 侧边栏线程列表                  | meta / 跨阶段       | [Threads](#1-线程)                  |
| 聊天记录                        | 0–2                 | [Messages](#2-消息与流式传输)     |
| 工作流流水线 / 卡片             | 0–4                 | [Workflow snapshot](#3-工作流快照) |
| 工作流 → Summary 标签页         | 0–4                 | [Summary](#4-摘要)                  |
| 工作流 → Diff 标签页            | 2–3                 | [Patches & diff](#5-patch-与-diff)     |
| 工作流 → Detail 面板            | 0–4                 | [Detail records](#6-detail-记录)    |
| 终端窗格                        | 2（sandbox runtime）| [Sandbox stream](#7-sandbox-流)    |
| 工作区底部 Pause/Continue       | 2                   | [Run control](#8-运行控制)          |

---

## 0. 约定

### 0.1 基础 URL

```
{LIBRA_BASE_URL}/api/v1
```

在本地开发时，其值为 `http://127.0.0.1:7373/api/v1` —— 与 `libra code --web` 已经绑定的端口一致。

### 0.2 认证

本地：隐式的环回（loopback）会话。无需任何 header。
远程：`Authorization: Bearer <token>`，由 Libra 凭据服务签发。

web 客户端将 token 存储在 `localStorage` 的 `libra.auth.token` 键下。下文所有示例都假设是环回场景。

### 0.3 标识符

| 前缀   | 含义                 | 示例          |
|--------|----------------------|---------------|
| `thr_` | Thread               | `thr_t1`      |
| `msg_` | 聊天消息             | `msg_8a3f`    |
| `pln_` | Plan                 | `plan-exec-04`|
| `stp_` | Plan step            | `s1`、`t1`    |
| `run_` | 执行 run             | `run-11`      |
| `pat_` | PatchSet             | `ps-07`       |
| `frm_` | ContextFrame         | `cf-0418`     |
| `evd_` | Evidence             | `evd_b3d2`    |
| `int_` | Intent 修订          | `r2`          |

ID 是不透明的短字符串。客户端不得解析它们。

### 0.4 时间戳

所有时间戳均为 RFC 3339 UTC 格式（`2026-04-25T10:42:13Z`）。UI 会转换为本地时间用于显示。

### 0.5 阶段枚举

```ts
type PhaseKey = "intent" | "plan" | "execution" | "validate" | "release";
type PhaseOrdinal = 0 | 1 | 2 | 3 | 4;
```

### 0.6 错误

```jsonc
// HTTP 4xx / 5xx
{
  "error": {
    "code": "phase_locked",            // stable machine code
    "message": "Phase 2 is gated on plan confirmation.",
    "phase": "execution",              // optional context
    "retryable": false
  }
}
```

UI 会显式处理的稳定错误码：

| 错误码              | 含义                                                     | UI 行为                                  |
|---------------------|----------------------------------------------------------|------------------------------------------|
| `unauthenticated`   | token 缺失/过期                                          | 强制重新认证                             |
| `forbidden`         | token 缺少所需 scope                                     | 弹出 toast + 禁用该操作                  |
| `not_found`         | thread / run / patch 不存在                              | 空状态                                   |
| `phase_locked`      | 当前阶段下该操作非法                                     | 显示内联门禁横幅                         |
| `intent_unconfirmed`| 在 intent 确认前请求了阶段 1+                            | 将用户弹回 Intent 卡片                   |
| `sandbox_offline`   | sandbox 镜像尚未启动                                     | 禁用终端输入                             |
| `rate_limited`      | token 配额耗尽                                           | 显示剩余配额标记                         |

### 0.7 流式传输

长时间运行的界面（聊天 token、工作流更新、sandbox 行）使用 **Server-Sent Events**（`text/event-stream`）。
每个事件都带有一个 `type` 和一个不透明的 `seq` 以支持续传：

```
event: message.delta
id: 1842
data: {"messageId":"msg_8a3f","delta":"reading src/lib/query.ts…"}
```

重连时通过发送 `Last-Event-ID: <seq>` 来续传。

刻意不采用 WebSockets —— SSE 能与静态导出的 web 客户端及 Next.js 的 edge runtime 组合，并契合 Libra 的追加式（append-only）事件模型。

---

## 1. 线程

**Thread** 是顶层单元。一个经用户确认的 Intent → 一个 Thread。一个 Thread 承载一个或多个 plan 修订、run、evidence 记录，以及一个最终的发布 decision。

### 1.1 列出线程 — `GET /threads`

返回**侧边栏线程列表**背后的数据。

**查询参数**

| 参数    | 类型            | 默认值  | 说明                                   |
|---------|-----------------|---------|----------------------------------------|
| `q`     | string          | —       | 针对线程标题的子串过滤                  |
| `phase` | `PhaseKey[]`    | —       | 过滤出当前处于某阶段的线程             |
| `limit` | int (1–200)     | 50      |                                        |
| `cursor`| string          | —       | 不透明的分页游标                       |

**响应**

```json
{
  "items": [
    {
      "id": "thr_t1",
      "title": "Add optimistic updates to useMutation",
      "phase": 2,
      "phaseKey": "execution",
      "branch": "agent/optimistic-mutate",
      "updatedAt": "2026-04-25T10:46:11Z",
      "ago": "1m"
    }
  ],
  "nextCursor": null
}
```

`ago` 由服务端渲染为人类可读字符串，以保证显示一致性。客户端也可以从 `updatedAt` 重新计算。

### 1.2 创建线程 — `POST /threads`

创建一个处于 **Phase 0（Intent draft）** 的线程。

**请求体**

```json
{ "title": "Optional seed title", "seedMessage": "Optional first user prompt" }
```

**响应**：单个 thread 实体（形状与 1.1 中的列表项相同）。

### 1.3 获取线程 — `GET /threads/{threadId}`

形状与 1.1 中的列表项相同，外加一个内嵌的 `intent` 修订摘要（不含完整的 markdown 正文）：

```json
{
  "id": "thr_t1", "title": "...", "phase": 2, "phaseKey": "execution",
  "branch": "agent/optimistic-mutate",
  "intent": { "id": "int_r2", "revision": "r2", "confirmed": true }
}
```

### 1.4 删除 / 归档 — `DELETE /threads/{threadId}`

软归档。处于 Phase 4 且带有已封存 IntentEvent 的线程不能被硬删除。

---

## 2. 消息与流式传输

聊天记录是一个限定在单个线程内的追加式列表。助手 token 通过 SSE 流式传输；用户消息是原子化的 POST。

### 2.1 列出消息 — `GET /threads/{threadId}/messages`

```jsonc
{
  "items": [
    {
      "id": "msg_8a3f",
      "role": "user",                   // user | assistant
      "body": "Let's add optimistic…",
      "createdAt": "2026-04-25T10:42:00Z",
      "streaming": false
    },
    {
      "id": "msg_4b21",
      "role": "assistant",
      "body": "I read src/lib/query.ts…",
      "createdAt": "2026-04-25T10:42:14Z",
      "streaming": false,
      "modelId": "claude-sonnet-4.5"
    }
  ]
}
```

### 2.2 发送用户消息 — `POST /threads/{threadId}/messages`

```json
{
  "body": "Looks right. One thing — the rollback has to preserve ordering…",
  "context": [
    { "kind": "file", "path": "src/lib/query.ts" }
  ],
  "mode": "Plan"
}
```

`mode` 为 `"Plan" | "Build"` —— 与编辑器的开关对应。`Plan` 使阶段 0/1 保持只读；`Build` 允许 agent 推进到 Phase 2。

**响应**：已持久化的消息 + 助手待流式输出的消息：

```json
{
  "user":      { "id": "msg_5a9c", "role": "user", "body": "...", "createdAt": "..." },
  "assistant": { "id": "msg_5a9d", "role": "assistant", "body": "", "streaming": true, "createdAt": "..." }
}
```

### 2.3 流式事件 — `GET /threads/{threadId}/events`（SSE）

一条单一的多路复用事件流，涵盖消息、工作流状态、evidence 以及 run 进度。web 客户端为每个活跃线程打开一次该流，并按 `type` 路由事件。

```
event: message.delta
data: {"messageId":"msg_5a9d","delta":"Got it — \"add optimistic updates"}

event: message.done
data: {"messageId":"msg_5a9d"}

event: workflow.patch
data: {"path":["plans","execution","steps","s3","status"],"value":"running"}

event: run.update
data: {"runId":"run-13","result":"running","ago":"now","patch":"…"}

event: evidence.append
data: {"kind":"tool","label":"grep \"MutationOptions\"","meta":"9 matches in 4 files"}

event: terminal.line
data: {"kind":"info","text":"[agent] capturing PatchSet ps-07"}

event: phase.changed
data: {"phase":2,"phaseKey":"execution","reason":"plan confirmed"}
```

事件类型：

| 类型                | Payload schema                                  | UI 界面              |
|---------------------|-------------------------------------------------|----------------------|
| `message.delta`     | `{messageId, delta}`                            | 聊天流式文本         |
| `message.done`      | `{messageId}`                                   | 聊天结束             |
| `workflow.patch`    | 针对 snapshot 树的 RFC 6902 add/replace patch    | 工作流卡片           |
| `run.update`        | 形状与 `ExecutionRun` 相同                       | Runs 卡片 / 时间线   |
| `evidence.append`   | `EvidenceRow`                                   | Evidence 窗格        |
| `terminal.line`     | `TerminalLine`                                  | 终端面板             |
| `phase.changed`     | `{phase, phaseKey, reason}`                      | 阶段条               |
| `intent.revised`    | `{intentId, revision}`                           | Intent 卡片          |
| `decision.recorded` | `{kind: "auto"|"human", verdict}`               | 发布卡片             |

### 2.4 取消流式消息 — `POST /threads/{threadId}/messages/{messageId}/cancel`

停止流式消息，并触发一个带 `{cancelled: true}` 的 `message.done` 事件。

---

## 3. 工作流快照

工作流窗格按线程读取单个去规范化的 snapshot，然后就地应用 SSE patch。这与 `web/src/lib/mock/workflow.ts` 完全镜像。

### 3.1 获取快照 — `GET /threads/{threadId}/workflow`

```jsonc
{
  "currentPhase": 2,
  "intent": {
    "id": "int_r2",
    "title": "Add optimistic updates to useMutation",
    "revision": "r2",
    "summary": "Introduce optimistic cache patching with rollback-on-error…",
    "constraints": [
      "Do not break MutationOptions<T> public shape",
      "Keep rollback safe under concurrent mutations",
      "Cover happy + error path with tests"
    ],
    "confirmed": true
  },
  "plans": {
    "execution": {
      "id": "plan-exec-04",
      "steps": [
        { "id": "s1", "label": "Snapshot cache at mutate() entry",          "status": "done"    },
        { "id": "s2", "label": "Apply optimistic patch to subscribers",     "status": "done"    },
        { "id": "s3", "label": "Per-key revision counter for safe rollback","status": "running" },
        { "id": "s4", "label": "Reconcile server response into cache",      "status": "queued"  },
        { "id": "s5", "label": "Surface onError with rollback context",     "status": "queued"  }
      ]
    },
    "test": {
      "id": "plan-test-02",
      "steps": [
        { "id": "t1", "label": "Happy-path optimistic update reflects immediately", "status": "queued" },
        { "id": "t2", "label": "Failure rolls back and preserves concurrent writes","status": "queued" },
        { "id": "t3", "label": "Reconciliation replaces optimistic entry",          "status": "queued" }
      ]
    }
  },
  "runs": [
    { "id": "run-11", "step": "s1", "result": "pass",    "ago": "2m", "patch": "+12 −0" },
    { "id": "run-12", "step": "s2", "result": "pass",    "ago": "2m", "patch": "+34 −7" },
    { "id": "run-13", "step": "s3", "result": "running", "ago": "now","patch": "…"      }
  ],
  "evidence": [
    { "kind": "tool",  "label": "read src/lib/query.ts",       "meta": "214 lines" },
    { "kind": "tool",  "label": "read src/hooks/useMutation.ts","meta": "88 lines" },
    { "kind": "tool",  "label": "grep \"MutationOptions\"",     "meta": "9 matches in 4 files" },
    { "kind": "frame", "label": "ContextFrame cf-0418",         "meta": "cache shape captured" },
    { "kind": "patch", "label": "PatchSet ps-07",               "meta": "+46 −7 across 2 files" }
  ],
  "tokensUsed": 48200,
  "graphHead": "agent/optimistic-mutate"
}
```

**字段语义**

| 字段             | 类型                                   | 说明                                                 |
|------------------|----------------------------------------|------------------------------------------------------|
| `currentPhase`   | `PhaseOrdinal`                         | 驱动阶段条 + 状态徽章                                 |
| `intent`         | `IntentDoc`                            | 每线程一个；`revision` 在每次修订时递增              |
| `plans`          | `{execution: Plan, test: Plan}`        | 在执行 DAG 稳定前 test plan 处于门禁状态             |
| `runs`           | `ExecutionRun[]`                       | 按时间先后排列；`result === "running"` 是互斥的      |
| `evidence`       | `EvidenceRow[]`                        | 追加式；从不重排                                      |
| `tokensUsed`     | int                                    | 显示：工作流头部的 `48.2k` 标记                       |
| `graphHead`      | string                                 | 显示：GitTimeline 的底部                              |

`StepStatus` 为 `"queued" | "running" | "done" | "failed"`。每个 plan 至多有一个 step 处于 `running`。

### 3.2 更新 Intent — `PATCH /threads/{threadId}/intent`

```json
{ "title": "...", "summary": "...", "constraints": ["..."], "confirmed": true }
```

确认一个 intent（`confirmed: true`）就是准入 Phase 1 的门禁。

### 3.3 确认 plan — `POST /threads/{threadId}/plans/{planId}/confirm`

成功返回 200；若前置阶段未完成则返回 `409 phase_locked`。

---

## 4. 摘要

**Summary 标签页** 是一个派生的 projection。它既可以作为去规范化的 GET 提供，也可以由客户端根据工作流 snapshot 计算得出 —— 由后端决定。web 客户端期望：

### 4.1 获取摘要 — `GET /threads/{threadId}/summary`

```jsonc
{
  "progress": [
    { "done": true,  "text": "Read src/lib/query.ts and snapshot current cache shape" },
    { "done": false, "text": "Wire per-key revision counter so rollback preserves ordering" }
  ],
  "branch": {
    "name": "agent/optimistic-mutate",
    "base": "main",
    "pr":   "No pull request",
    "changes": "2 files changed, 1 untracked"
  },
  "artifacts": [
    { "kind": "PatchSet", "id": "ps-07",   "meta": "+46 −7 across 2 files" },
    { "kind": "Frame",    "id": "cf-0418", "meta": "cache shape captured" }
  ],
  "todo": [
    { "done": true,  "text": "Snapshot cache at mutate() entry" },
    { "done": false, "text": "Per-key revision counter for safe rollback" }
  ]
}
```

`progress` 反映 agent 的叙述式检查清单（通常是消息级别的分解）。`todo` 镜像执行 plan 的各 step。两者都必须保持稳定排序。

---

## 5. Patch 与 diff

**Diff 标签页** 由一个或多个在 Phase 2 产出的 PatchSet 提供数据。

### 5.1 列出 patch — `GET /threads/{threadId}/patches`

```json
{
  "items": [
    { "id": "pat_ps-07", "createdAt": "2026-04-25T10:46:08Z", "stats": { "files": 2, "add": 46, "del": 7 } }
  ]
}
```

### 5.2 获取 patch 内容 — `GET /patches/{patchId}`

```jsonc
{
  "id": "pat_ps-07",
  "stats": { "files": 2, "add": 46, "del": 7 },
  "files": [
    {
      "path": "src/lib/query.ts",
      "add": 34, "del": 7,
      "hunks": [
        {
          "header": "@@ -214,10 +214,23 @@ export function useMutation<T>(",
          "lines": [
            { "kind": "ctx", "n1": 214, "n2": 214, "text": "    const [state, setState] = React.useState…" },
            { "kind": "del", "n1": 217,             "text": "      const result = await fetcher(input);" },
            { "kind": "add",            "n2": 217, "text": "      const snap = cache.snapshot(key);" }
          ]
        }
      ]
    }
  ]
}
```

`kind` 为 `"ctx" | "add" | "del"`。上下文行同时带有两个行号；删除行只有 `n1`；新增行只有 `n2`。

### 5.3 最新 patch 快捷方式 — `GET /threads/{threadId}/patch`

解析为该线程最近的 PatchSet —— Diff 标签页的默认加载内容。

---

## 6. Detail 记录

工作流 detail 面板会打开五种 detail。每一种都有自己的端点，使该面板能够分页 / 惰性加载工具调用及其输出，而不会让 snapshot 膨胀。

### 6.1 Intent 详情 — `GET /threads/{threadId}/intent/{revision}`

返回完整的 markdown 正文，连同结构化字段：

```json
{
  "id": "int_r2", "revision": "r2", "title": "Add optimistic updates to useMutation",
  "summary": "Introduce optimistic cache patching…",
  "constraints": ["..."],
  "markdown": "# Add optimistic updates to useMutation\n\n…",
  "confirmed": true,
  "createdAt": "2026-04-25T10:42:14Z"
}
```

### 6.2 Plan step 详情 — `GET /plans/{planId}/steps/{stepId}`

```json
{
  "id": "s3", "label": "Per-key revision counter for safe rollback", "status": "running",
  "planId": "plan-exec-04", "planKind": "execution",
  "purpose": "Execution step — mutates cache/code inside the sandbox…",
  "toolCalls": [
    { "name": "read", "arg": "src/lib/query.ts", "result": "214 lines",  "running": false },
    { "name": "edit", "arg": "src/lib/query.ts", "result": "patchset ps-07", "running": false },
    { "name": "test", "arg": "useMutation.test.ts", "result": "running…", "running": true }
  ],
  "siblings": ["s1", "s2", "s4", "s5"]
}
```

### 6.3 Run 详情 — `GET /runs/{runId}`

```jsonc
{
  "id": "run-13",
  "step": "s3",
  "result": "running",
  "ago": "now",
  "patch": "…",
  "sandbox": "libra-sbx-04 · rw",
  "output": "$ cargo test --lib optimistic\n   Compiling libra-cache v0.3.1\n…",
  "diff": {
    "path": "src/lib/query.ts",
    "patch": "@@ useMutation ()\n- const result = await fetcher(input);\n+ const snap = cache.snapshot(key);\n…"
  }
}
```

### 6.4 Validation 详情 — `GET /threads/{threadId}/validation`

```json
{
  "checks": [
    { "name": "SAST · static analysis",      "status": "queued" },
    { "name": "SCA · dependency advisories", "status": "queued" },
    { "name": "Type-check",                  "status": "queued" },
    { "name": "Test plan · full run",        "status": "queued" },
    { "name": "Compatibility · API surface", "status": "queued" }
  ],
  "verdict": null,
  "evidenceLink": "/threads/thr_t1/evidence?kind=audit"
}
```

`verdict` 为 `"pass" | "fail" | null`（仍在运行中）。

### 6.5 Release 详情 — `GET /threads/{threadId}/release`

```json
{
  "policy": "web3infra/default",
  "surface": "internal hook · 2 callers",
  "blastRadius": "low",
  "reversibility": "clean revert",
  "decision": null,
  "intentEventId": null
}
```

一旦 Phase 4 关闭，`decision` 即变为 `"auto-merge" | "request-review"`；`intentEventId` 是追加式日志中那条已签名的事件。

---

## 7. Sandbox 流

终端面板读取自每线程一个的 sandbox。

### 7.1 获取历史 — `GET /threads/{threadId}/terminal`

```json
{
  "sandbox": {
    "id": "libra-sbx-04",
    "image": "rust:1.81-slim",
    "fs":    "rw(tmp)",
    "net":   "off"
  },
  "lines": [
    { "kind": "meta",   "text": "libra sandbox v0.4.2 · image rust:1.81-slim · net=off · fs=rw(tmp)" },
    { "kind": "prompt", "text": "cargo test --lib optimistic" },
    { "kind": "pass",   "text": "test optimistic::snapshot_before_mutate ... ok" }
  ]
}
```

`kind` 是 `"meta" | "prompt" | "stdout" | "pass" | "fail" | "run" | "warn" | "info"` 之一 —— 与 `web/src/lib/mock/types.ts` 中的 `TerminalLineKind` 对应。

### 7.2 运行 sandbox 命令 — `POST /threads/{threadId}/terminal/exec`

```json
{ "cmd": "ls", "tab": "tools" }
```

回复行通过线程的 SSE 流以 `terminal.line` 事件到达。HTTP 响应只是一个确认：

```json
{ "accepted": true, "execId": "exec_91" }
```

当 sandbox 被锁定为仅供 agent 执行时，Rust 端可能会拒绝命令（`sandbox_offline` / `phase_locked`）。

---

## 8. 运行控制

工作流底部的 Pause/Continue 按钮映射到两个端点：

### 8.1 暂停 — `POST /threads/{threadId}/control/pause`
在下一个安全检查点处停下执行 DAG。当前正在运行的 step 会完成（或通过其 sandbox 守卫回滚）；不再启动任何后续 step。

### 8.2 继续 — `POST /threads/{threadId}/control/continue`
从最近一次暂停的检查点恢复。若线程未处于暂停状态则返回 `409 phase_locked`。

两者都回复最新的工作流 snapshot，以支持乐观 UI 更新。

---

## 9. 线程模型与幂等性

- 每个写端点都接受一个可选的 `Idempotency-Key` header。Rust 端会持久化 `(threadId, key) → response`，保留 24 小时，使重试不会重复创建消息或 run。
- 工作流 patch 带版本号：每个 `workflow.patch` 事件携带一个单调递增的 `version`。客户端一旦观察到序号断档，就会重新拉取完整 snapshot。
- intent 与发布 decision 通过追加式（append-only）的 `IntentEvent` 记录封存 —— 与 Rust 核心使用的原语相同（参见 [ai-object-model-reference.md](../ai/object-model-reference.md)）。

---

## 10. Mock → 真实接口切换计划

当前 web 客户端通过单一的 barrel 文件导入 mock 数据：

```ts
// web/src/lib/mock/index.ts
export { THREADS, MESSAGES, WORKFLOW, SUMMARY, REVIEW, TERMINAL_LINES, PHASES } from "./*";
```

当 Rust 后端发布时，用一个轻薄的客户端模块替换这个 barrel：

```ts
// web/src/lib/api/index.ts
export const PHASES = …;          // static; can stay client-side
export async function getThreads()         { return fetch("/api/v1/threads").then(r => r.json()); }
export async function getWorkflow(id: string) { … }
export function openThreadStream(id, onEvent) { /* EventSource */ }
```

各组件目前直接消费 mock 常量。迁移步骤如下：

1. 将每个消费者改为由 SWR/React Query 支撑的 hook（例如 `useThreads()`、`useWorkflow(threadId)`）。
2. 用 HTTP 调用替换 mock 模块；类型签名保持完全一致，因为 `web/src/lib/mock/types.ts` 中的 mock 类型就是契约本身。
3. 添加一个共享的 `<ThreadStreamProvider>`，它只打开一次 `/threads/{id}/events`，并将事件分发给各卡片级别的订阅者。

mock 模块即规范 —— 请让其类型与本文档保持同步。

---

## 11. 待解决问题

1. **自托管 Libra 的认证面**：我们是通过 Libra 凭据服务进行联合认证，还是接受来自可配置 JWKS 的任意已签名 token？桩实现假设是前者。
2. **多线程扇出**：SSE 设计为每个活跃线程打开一条流。侧边栏是否也应订阅一条粗粒度的 `/threads/events` 流以显示未读徽章？大概率需要，但此处暂不在范围内。
3. **Patch 存储**：PatchSet 体积很大。我们是像 §5 那样内联提供，还是以 `application/x-libra-patch` 分块流式传输？在单个 hunk 超过约 1MB 之前，内联是可以接受的。
4. **终端二进制输出**：目前仅支持文本。如果 sandbox 工具产出二进制内容（例如火焰图），我们将需要一个单独的 `terminal.attachment` 事件来引用某个 artifact ID。

这些问题应在第一个真正的实现落地之前解决。
