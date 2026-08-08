# Agent Trace 对照 Libra 归属方案：分析与改进设计

## 状态

Proposed（分析 / 设计意见）

## 日期

2026-06-18

## 摘要

本文对照 Cursor 主导的开放规范 **Agent Trace**（v0.1.0 RFC，CC BY 4.0）与 Libra 当前 AI 代码归属（attribution / provenance）实现，给出一份可直接落地、分阶段的改进路线图。

**一句话结论**：Agent Trace 与 Libra 在同一问题上做了不同取舍——Agent Trace 赌**互操作**（极简、厂商中立的 JSON 交换格式），Libra 赌**深度**（SQLite + 内部对象模型 + 真 VCS 语义）。Libra 的内部模型在对象粒度上已经比 Agent Trace 更深；照搬 spec 当作内部模型是降级。Agent Trace 对 Libra 的价值只集中在两点：

1. 补上 Libra 真正缺失的**行级归属（line-range）**。
2. 把归属做成一层标准化的**交换皮肤**，以接入 Agent Trace 联盟（Amp、Cline、Vercel、Cloudflare、Cognition、OpenCode、Jules …）。

而 Libra 作为**真 VCS**，恰好能解决 spec 自己回避的三个最难问题——**可查询的持久存储、可签名可验证、rebase/merge 后仍稳定**——这是任何 hook 式编辑器插件结构上做不到的，应作为 Libra 的差异化主线。

---

## 1. Agent Trace 方案速览

Agent Trace 是一份**数据规范，不是产品**。它的唯一规范产物是一条 append-only 的 `TraceRecord` JSON：

```jsonc
{
  "version": "0.1.0",
  "id": "<uuid>",
  "timestamp": "<rfc3339>",
  "vcs": { "type": "git|jj|hg|svn", "revision": "<commit oid / change id>" },
  "tool": { "name": "cursor", "version": "2.4.0" },
  "files": [
    {
      "path": "src/utils/parser.ts",
      "conversations": [
        {
          "url": "https://api.cursor.com/v1/conversations/12345",
          "contributor": { "type": "ai", "model_id": "anthropic/claude-opus-4-5-20251101" },
          "ranges": [
            { "start_line": 42, "end_line": 67, "content_hash": "murmur3:9f2e8a1b" }
          ],
          "related": [
            { "type": "session", "url": "https://api.cursor.com/v1/sessions/67890" }
          ]
        }
      ]
    }
  ],
  "metadata": {
    "confidence": 0.95,
    "dev.cursor": { "workspace_id": "ws-abc123" }
  }
}
```

### 1.1 设计要点

| 维度 | Agent Trace 的选择 |
|---|---|
| 归属粒度 | **文件 + 1-indexed 行区间**，按 `conversation` 聚合以降低基数 |
| contributor | `human / ai / mixed / unknown` + `model_id`（models.dev `provider/model-name` 约定） |
| 采集 | **hook**（Cursor `afterFileEdit`；Claude Code `PostToolUse(Write\|Edit\|Bash)`），把声称的编辑区间 append 进 `.agent-trace/traces.jsonl` |
| 查询 | 不建索引，**派生式**：`blame 第 N 行 → revision → 载入该 (revision, file) 的 trace → 找包含 N 的 range` |
| 位置无关 | 可选 `content_hash`（murmur3 仅为示例） |
| 存储 | **故意不定义**（本地文件 / git notes / DB 皆可） |
| 扩展 | 反向域名 vendor key（如 `dev.cursor`、`com.github.copilot`）+ `confidence` 等自由字段 |
| 非目标 | 法律归属/版权、训练数据溯源、质量评估、UI |

其设计哲学与 Git（极小内容寻址核 + 其余皆约定）和 OpenTelemetry（只约定记录，让厂商在采集器/后端/UI 上竞争）同源：**标准化记录形状，把一切有争议的运营问题留空**。

### 1.3 Cursor 参考实现实测行为（与规范文本的差异）

参考实现位于 `reference/{trace-store.ts, trace-hook.ts}`，是 Cursor / Claude Code hook 的实际落地代码。**规范文本（README + schemas.ts）与实现之间存在可观测差异，Libra 必须在导入端宽容处理**：

- **version**：`createTrace` 硬编码为 `"1.0"`（非三段 semver）。而 `schemas.ts:89` 的 regex 是 `^[0-9]+\.[0-9]+\.[0-9]+$`，README 示例用 `0.1.0`。真实世界会同时看到 `"1.0"` 和 `"0.1.0"`。
- **range 计算**（`computeRangePositions`）三级回退：
  1. 若 `FileEdit.range` 存在，直接使用（Cursor 某些 hook 会带）。
  2. 否则在 `fileContent`（pre-edit 内容）里做 `indexOf(new_string)` 找位置，计算行号。
  3. 全部失败时退化到 `{ start_line: 1, end_line: lineCount }`（或单行 `1..1`）。
- **特殊合成路径**：
  - Bash 执行 → 写到 `.shell-history`（非真实文件）。
  - session 事件 → 写到 `.sessions`。
- **model 规范化**：仅前缀启发式（`claude-*` → `anthropic/`，`gpt-*`/`o1*` → `openai/` 等），不是完整的 models.dev 目录。未知模型直接透传。
- **conversation.url**：很多情况是 `file://` 指向本地 transcript，而不是公开可解析的 http URL。
- **存储**：始终 append 到 `<root>/.agent-trace/traces.jsonl`（每行一个完整 TraceRecord）。没有索引、没有去重、没有 compaction。
- **VCS**：仅尝试 `git rev-parse HEAD`，失败则省略 `vcs` 字段。

这些行为决定了 Libra 导入外部 trace 时的**降级策略**必须显式编码（见 P0-0 和 P3-13）。

### 1.4 参考实现的采集缺陷深度举证（"采集应从 VCS 内部开始"的最强证据）

> 以下行为经 `reference/trace-store.ts` 逐行确认。它们共同构成"采集源头必须是 `apply_patch` 而非 hook"的决定性论据。

#### a. 纯新增编辑必然退化到行 1（系统性错误）

`computeRangePositions`（`:102`）在 `edit.range` 缺失时，用 `fileContent.indexOf(edit.new_string)` 在**编辑前内容**中查找新字符串。对纯新增（addition）编辑——`old_string=""`、`new_string` 为原本不存在的新代码——`indexOf` 必然返回 `-1`，直接退化到 `{start_line: 1, end_line: lineCount}`，**声称从文件第 1 行写入**。

统计上纯新增是 AI 最频繁的编辑类型（新增函数、方法、impl 块），这意味着 hook 模式下**最常见编辑类型获得最低信号质量**。作为对比，Libra 的 `compute_replacements`（`apply_patch/core.rs:285`）对 `old_len=0` 的插入返回精确 `start_index`，绝无退化。

这是 P0-1"在 `apply_patch` 源头采集"的硬性论据——不是偏好，是必须。

#### b. Write 与 Edit 工具的无差别处理 + 多编辑信息丢失

Claude Code `PostToolUse` hook（`reference/trace-hook.ts:94`）对 `Write` 和 `Edit` 使用同一条路径：从 `tool_input.{old_string,new_string}` 合成单个编辑数组。但：

- **Write**：`old_string=""`（空串），`new_string` 为文件全文。若文件已存在，`indexOf` 成功，范围覆盖整个文件（语义正确但粒度粗——标记全文件为 AI 贡献）；若文件全新，`indexOf` 失败退化到行 1。
- **Edit**：`old_string` 有值。一次 Claude Code `Edit` 调用的 `tool_input` 可能含多个 `replace` 操作，但 `PostToolUse` hook payload 只暴露 `tool_input.old_string` / `.new_string` 的单组——**多编辑操作被塌成单记录**。
- Cursor 的 `afterFileEdit` hook 原生携带 `edits[]`（多编辑数组），但 Cursor 参考实现未做 Cursor↔Claude 路径统一。

→ Libra 导入时必须区分 `tool.name` 与事件源（见 §4.7.a 压实策略）；`apply_patch` 路径天然避开了上述全部问题。

#### c. `schemas.ts` 内部 self-contradiction

`schemas.ts:89` 的 version regex 是 `^[0-9]+\\.[0-9]+\\.[0-9]+$`（强制三段 semver），但同行的 `.describe()` 给出的示例是 `"e.g., '1.0'"`——**描述自身给了一个会被 regex 拒绝的例子**。加之 `reference/trace-store.ts:151` 实际写 `version: "1.0"`、README 示例用 `0.1.0`、spec 正文 §6.1 的 JSON Schema description 又写 `"e.g., '1.0.0'"`，一篇 spec 内出现 4 种 version 形状。导入端必须在解析层宽容接受再内部规范化，不能在任何边界做严校验。

#### d. 参考实现从不产出 `related` 和 `content_hash`

`createTrace` 的 `conversation` 和 `ranges` 从不写 `related` 数组（尽管 spec §6.8 详述其用法），也不写 `content_hash`（尽管 spec §6.6 详述）。导入端不可依赖这两个字段存在——但 Libra 导出时应主动产出（见 §6 增强示例）。

### 1.2 Agent Trace 故意回避的 = Libra 的机会

1. **存储不定义** → 没有可互操作的读取路径。工具 A 写 JSONL，工具 B 写 git notes，彼此之间无发现/优先级规则。
2. **rebase/merge/amend 未定义** → 记录绑定到 revision，历史重写后 `(revision, line)` 查询**静默失配**——而这正是 trunk-based / monorepo 的常态。
3. **无信任/签名/防篡改** → `TraceRecord` 是未认证的自声明，可伪造、可遗漏、可灌水；作为合规/许可/审计证据不可接受。
4. **无聚合/查询模型** → “这个文件/PR/release 有多少比例是 AI 写的、哪个模型、随时间趋势”答不了，每个消费者各自造轮子，数字互不一致。
5. **采集保真度依赖 hook** → 记录的是写入时**声称的**区间，不与实际 commit 内容核对；formatter、人工改、部分回退都会让记录与落地内容不符。
6. **无 commit 级原子性** → JSONL append 与工作树写入是两个独立动作，crash / `git commit --amend` 后 trace 与树可能不同步。
7. **无 provenance 链** → 一次编辑可能来自“模型 A 生成 + 人类微调 + 模型 B 重构”，spec 允许 per-range `contributor` override，但 Cursor 参考实现几乎不使用。

---

## 2. Libra 现状对照

| Libra 子系统 | 关键文件 | 当前归属粒度 | 互操作性 | 相对 Agent Trace 的关键缺口 |
|---|---|---|---|---|
| `observed_agents` | `src/internal/ai/observed_agents/{adapter,derived,redaction}.rs` + `builtin/*` + hooks runtime | session / checkpoint on `refs/libra/traces` | 封闭（可导出） | 目前只做外部 hook 捕获 + redaction + 归一化事件（`has_tool_input`）；仍缺 model_id 规范化到 conversation 级、行区间；`HookTarget::AgentTraces` 仍为 Phase1 stub |
| `session` + `file_history` | `src/internal/ai/session/{state,file_history,store,jsonl}.rs` | session | JSON 可导出 | model 仅 session 级；`file_history` 只记**文件**快照，不记哪些行 |
| `usage` | `src/internal/ai/usage/{recorder,pricing,query,format}.rs`、`src/command/usage.rs` | session | 封闭 | **已有完整聚合/过滤/JSON-CSV 管线**，但无 file/path 维度 |
| `agent_run` | `src/internal/ai/agent_run/{patchset,evidence,event}.rs` | mixed | 封闭 | `TouchedFile` 只有 `lines_added/deleted` 计数，**无行区间** |
| `hooks` | `src/internal/ai/hooks/{lifecycle,runtime}.rs` | session | 封闭 | `runtime.rs::append_normalized_event` 把 `tool_input` 塌成布尔 `has_tool_input`，**把行数据丢了** |
| `publish/ai_export` | `src/internal/publish/ai_export.rs` | commit | JSON 可导出 | 对象级（Intent/Plan/Task/Run），**无 `files[]/ranges[]`** |
| git 表面 | `src/command/{commit,blame,log,notes}.rs`、`src/internal/ai/history.rs` | commit | git-native | `blame` 零 AI 维度；无 `Co-Authored-By` 自动写入；但已有 `notes` 设施与 `Libra-*` trailer |

**一句话**：Libra 处处停在**文件级 / 会话级**，唯独没有 Agent Trace 的核心——**行级**。但它已具备 model 拆分、聚合管线、notes、trailer、rebase 引擎等全部“半成品零件”。

---

## 3. 关键判断

- Libra 的内部 AI 对象模型（Intent → Plan → Task → Run → PatchSet → Provenance，见 `ai_export.rs`）在**对象粒度上已经比 Agent Trace 更深**。把 Agent Trace 当作**内部模型**来采用，对 Libra 是降级。
- Agent Trace 对 Libra 的价值**纯在边缘**：① 补上 Libra 真正缺的**行级粒度**；② 提供一层**厂商中立的交换皮肤**用于联盟互操作。
- **正确定位**：把 Agent Trace 当**导出/导入格式**（在 `publish` / `export` / 可选导入边界 emit/ingest），**内部深模型保持私有作为护城河**，并在 spec 明确回避的三件事上领先——**持久可查询存储、可签名可信记录、重写后稳定的归属**——这些是 Libra 作为真 VCS 的结构性优势。

---

## 4. 合并后的改进意见

### 4.1 设计原则

1. **内部权威、外部交换分层**：Libra 自己产生的 `apply_patch` / agent runtime trace 才是权威归属数据；Agent Trace 只作为 `publish` / export / 可选导入的交换层。不要把 `.agent-trace/traces.jsonl` 当成本地事实源，也不要用它替代 `agent_session`、`agent_checkpoint`、`agent_usage_stats`、`refs/libra/traces`。
2. **采集优先级必须从 VCS 内部开始**：`apply_patch` 已在写文件前知道精确替换区间，比 hook payload 或字符串回查更可信；外部 hook 只能作为 `trusted=false` 的外部声明进入系统，在 `blame --ai` 中必须可区分。
3. **导出严格，导入宽容**：Libra 导出的 `TraceRecord.version` 必须使用三段 semver（当前按 RFC 用 `0.1.0`），通过 fixture 固定 MIME 与 JSON schema；导入时可兼容参考实现里出现的 `1.0` 等非三段版本，但要规范化为内部版本并打低置信/兼容警告。
4. **用 `metadata["tools.libra.*"]` 承载 Libra 深模型**：标准字段只放 Agent Trace 规定的 `vcs/tool/files/conversations/ranges`；Libra 的 `session_id`、`run_id`、`traces_commit`、`checkpoint_id`、`hash_kind`、签名、confidence 来源等全部放反向域名 vendor metadata，避免污染标准层。
5. **先产出可测试合同，再做命令面**：在实现 `blame --ai` / `usage --by file` 前，先增加 publish/export fixture 与 round-trip 校验，固定 `TraceRecord` 的字段、版本、hash 语义、可信来源枚举和外部声明降级规则。
6. **导入端必须显式建信任模型**：来自 `.agent-trace/traces.jsonl` 或其他 Agent Trace 联盟成员的记录，一律进入独立命名空间，`blame --ai` / `usage` 输出时标注来源与 `trusted` 位；绝不与 Libra 内部 `apply_patch` 产生的权威行区间混合排序。参考实现的三级 fallback 产生的区间置信度应显著低于精确 `compute_replacements` 结果。

### 4.2 对抗验证发现的硬约束（务必先读）

> 以下结论经对实际代码核验，推翻了若干“听起来很美”的直觉做法。照着直觉做会撞墙。

1. **`git-internal` 是外部 pinned crate**（`Cargo.toml`：`git-internal = "0.8.1"`，无 `[patch]` 覆盖）。改其 `TouchedFile` / `Provenance` **不是 Libra 仓内 PR**，需发上游版本再升级依赖；且二者都带 `#[serde(deny_unknown_fields)]`，加字段对旧版本读者是**前向不兼容**。
   → **行区间类型必须在 Libra 仓内自定义，不要动 `git-internal`。**
2. **Libra 的 `notes` 不是 git 原生 `refs/notes/*` 树**，而是 SQLite 表（`sql/migrations/2026061401_notes.sql`）+ 对象库里的 blob 哈希。它**不随 push/fetch 传输，外部工具发现不了**；`idx_notes_ref` 只建在 `(notes_ref)` 上。
   → 用 notes 做“跨工具可发现的互操作后端”**不成立**；notes 只适合**本地**存储。真正的互操作只能在 **`publish/ai_export` 边界**导出标准 JSON。
3. **Libra 自己的 `commit` 与 agent-session 没有任何耦合**。现有归属模型是“**观察外部 agent**”（checkpoint 落在 `refs/libra/traces` orphan ref，`agent_checkpoint.traces_commit`）；`commit.rs` 从不写 `traces_commit`。
   → 凡“在 active agent session 内提交时写 trailer/链接”的建议**今天没有落点**，需先建这条耦合。
4. **`rebase` / `cherry-pick` 不调用 `compute_diff`**（`compute_diff` 的调用方是 `blame.rs` 与 `log.rs`，rebase/cherry-pick 均 0 命中），重放走树级三向合并。
   → “复用 rebase 已算好的 diff 来重排行区间”**是错的**；但 old→new commit 映射**确实存在**（`rebase.rs:566` `summary.applied_commits` / `RebaseAppliedCommitOutput`），故 commit 级 note 重锚可行，行级重排是另一回事（更贵）。
5. **`Vault` 有 `pgp_sign` 但无 `verify`**（`src/internal/vault.rs`：`pgp_sign`、`signature_to_gpgsig` 存在，无任何 verify）。签名可行，验证侧是净新工作，还需密钥分发/信任模型。
6. **Agent Trace 参考实现不是可直接照搬的合同**。`schemas.ts` 要求 `version` 满足三段 semver，README 示例用 `0.1.0`，但 `reference/trace-store.ts:151` 实际写 `version: "1.0"`；`computeRangePositions:102` 在 hook 未给 range 且字符串回查失败时会退化成 `1..lineCount`（甚至硬 `1..1`）。此外还会写 `.shell-history`、`.sessions` 合成文件。
   → Libra 应该**严格导出（永远用 `0.1.0` 或后续正式三段）、宽容导入（接受 "1.0" / "0.1.0"，对合成路径与全文件 fallback 打低 `trusted`）、显式降级可信度**。参考实现只可作兼容测试样本。
7. **Libra worktree 共享同一个 `.libra/libra.db`**（核验：`src/command/worktree.rs:671,844` 把每个 worktree 的 `.libra` 建成指向 shared storage 的**符号链接**；`src/utils/path.rs:23` `database()` = `storage_path().join("libra.db")`）。（**注**：本条写于 plan-20260714 Part C 之前;linked worktree 的 HEAD/index 自 W0 起已按 `worktree_id` 隔离,但 `libra.db` 本身仍是单一共享物理库,故下述结论不变。）意味着 **P1-4 规划的 `ai_edit_trace` 表是跨所有并发 worktree 会话共享的单一物理表**——不是 git worktree 那种各自隔离。
   → 任何“apply 时写 NULL `commit_oid`、commit 时回填全部 NULL 行”的简单方案在并发会话下**会串号**（worktree A 的提交回填了 worktree B 的待提交行）。回填**必须按 `session_id`（或 `run_id`）严格 scope**，绝不按“所有 `commit_oid IS NULL`”一把回填。详见 §7.2。

**利好**：

- `content_hash` 别引 murmur3——**`git-internal::IntegrityHash::compute`** 可用 SHA-256 计算字节哈希，序列化为 `integrity:sha256:<hex>`。⚠️ 此 API 在**外部 pinned crate `git-internal 0.8.1`** 内（非 Libra 仓内可 grep 到），是本文档全部锚点中**唯一无法在本仓树内核验**的一条——落地 P0-0/P1-4 前必须先对照已 vendored 的 crate 源确认其确切签名与序列化语义，不要凭本文档直接调用。
- `model_id` 规范化几乎免费——**`ModelBinding::to_canonical_string()`**（`src/internal/ai/agent/profile/spec.rs:129`）已能产出 `provider/model[@variant]`，且 `AgentRunEvent::Spawned`、`agent_usage_stats`、`UsageContext` 都已把 provider 与 model **拆开存**，只差在序列化边界拼接。
- **唯一被低估的资产**：`apply_patch` 已经算好行区间。`src/internal/ai/tools/apply_patch/core.rs` 的 `compute_replacements` 产出精确的 `(start_index, old_len, new_lines)`，handler 还建了 `FileDiff` + unified diff，然后**仅用于 TUI 显示就丢弃**。这是全系统唯一对“AI 写了哪些行”有完美、无竞态认知的地方。

### 4.3 改进路线图

按“采集 → 存储 → 读取 → 互操作 → 护城河”的依赖链组织。**唯一真正的地基是“行级采集”，而最便宜且最准确的采集点是 Libra 自己的 `apply_patch`，不是 hook，也不是 `git-internal`。**

#### P0 — 地基

| 编号 | 任务 | 触及文件 | 工作量/风险 | 说明 |
|---|---|---|---|---|
| **P0-0** | 固定 Libra 的 Agent Trace 交换合同 | `src/internal/publish/contract.rs`、`src/internal/publish/ai_export.rs`、`tests/data/publish/` | S / 低 | 先定义最小 `TraceRecord` fixture，固定 `version="0.1.0"`（严格导出）、MIME `application/vnd.agent-trace.record+json`、`tools.libra.*` metadata key、`trusted`/`source` 规则、hash-kind 标记和外部声明降级策略（接受 "1.0"/"0.1.0"、合成路径、全文件 fallback 均打低 trusted）。同时放一个来自 Cursor 参考实现的 golden trace（含 "1.0" + fallback range）作为导入 roundtrip 测试输入。 |
| **P0-1** | 在 `apply_patch` 源头采集行区间 | `src/internal/ai/tools/apply_patch/core.rs`、`handlers/apply_patch.rs` | M / 低 | 把 `compute_replacements`（返回 `(start_index, old_len, new_lines)` 1-based 区间）已算出的精确信息从 handler 透出到一个 capture sink（可挂 `RuntimeContext` 或 `UsageContext` 里的 trace collector）。保留现有 `metadata.diffs` 给 TUI，区间数据走另一条持久化路径。`compute_replacements` / `apply_replacements` 目前私有，需沿调用链暴露或在 `ApplyResult` 上增加 `line_ranges: Vec<EditRange>` 结构。 |
| **P0-2** | 把 model/run/session 身份穿进 `FileHistoryRuntimeContext` | `src/internal/ai/sandbox/mod.rs`、`src/internal/tui/app.rs` | M / 低 | 模型在 dispatch 处已知（`ToolLoopConfig.usage_context` 已带全套身份）；`FileHistoryRuntimeContext` 已能到达 apply_patch handler，只差这几个字段。纯内存，无迁移。 |
| **P0-3** | model_id 规范化到 models.dev | `src/internal/ai/agent/profile/spec.rs`、`usage/format.rs`、`publish/ai_export.rs` | S / 低 | 加 `canonical_model_id(provider, model)` helper，**只在序列化/导出边界**用，不动 DB 列。对 `ollama/llama3` 这类非 models.dev 厂商，规范化只是拼接，不保证联盟有效性。 |

#### P1 — 存储 + 读取（用户可见价值）

| 编号 | 任务 | 触及文件 | 工作量/风险 | 说明 |
|---|---|---|---|---|
| **P1-4** | 新建 Libra 本地 trace 存储（SQLite 表，别用 notes 做互操作） | `sql/migrations/<date>_ai_edit_trace.sql`、`src/internal/model/`、`src/internal/db/migration.rs` | L / 中 | 新表 `ai_edit_trace`，apply 时写入（`commit_oid` 留空），commit 时回填。设计抉择：建议 denormalize provider/model 而非 join `agent_usage_stats`（`blame --ai` 每行都要查，额外 join 在高频路径上不划算）。`content_hash` 复用 `IntegrityHash`。DDL 草稿：<br><br>```sql<br>CREATE TABLE IF NOT EXISTS `ai_edit_trace` (<br>    `id`               INTEGER PRIMARY KEY AUTOINCREMENT,<br>    `session_id`       TEXT NOT NULL,<br>    `thread_id`        TEXT,<br>    `run_id`           TEXT NOT NULL,<br>    `provider`         TEXT NOT NULL,<br>    `model`            TEXT NOT NULL,<br>    `file_path`        TEXT NOT NULL,<br>    `start_line`       INTEGER NOT NULL,<br>    `end_line`         INTEGER NOT NULL,<br>    `content_hash`     TEXT,<br>    `commit_oid`       TEXT,  -- NULL until commit backfill<br>    `contributor_type` TEXT NOT NULL DEFAULT 'ai',<br>    `source`           TEXT NOT NULL DEFAULT 'libra_apply_patch',<br>    `trusted`          INTEGER NOT NULL DEFAULT 1,<br>    `created_at`       TEXT NOT NULL<br>);<br>CREATE INDEX IF NOT EXISTS idx_ai_edit_trace_file<br>    ON `ai_edit_trace`(`file_path`, `start_line`, `end_line`);<br>CREATE INDEX IF NOT EXISTS idx_ai_edit_trace_commit<br>    ON `ai_edit_trace`(`commit_oid`);<br>CREATE INDEX IF NOT EXISTS idx_ai_edit_trace_session<br>    ON `ai_edit_trace`(`session_id`);<br>```<br><br>**Crash 恢复**：commit 回填前若 crash，`commit_oid` 残留 NULL。`blame --ai` 查询时需兜底：通过 `run_id → session_id → agent_checkpoint` 反查 `traces_commit` 补填，或按 `(file_path, start_line, end_line)` 匹配最近已知 commit。优先实现反查路径（已有数据、免额外扫描）。<br><br>**并发回填（worktree 共享 DB）**：因 `.libra/libra.db` 跨 worktree 共享（§4.2.7 / §7.2），回填 UPDATE 必须带 `WHERE session_id = ? AND commit_oid IS NULL`，**禁止**裸 `WHERE commit_oid IS NULL`。建议给表加 `worktree_id`（来自 `util::storage_path()` 解析的实际 worktree 标识）以便审计与隔离查询。 |
| **P1-5** | `libra blame --ai` | `src/command/blame.rs`、`tests/command/` | M / 低 | spec 把它描述成“blame + trace 两个工具的舞蹈”，Libra 同进程既有 blame 又有 trace 存储，能合成一条命令。`BlameLine` 加可选 `contributor` / `model_id`（`#[serde(skip_serializing_if = "Option::is_none")]`，JSON 加性兼容）；`--ai` 时按行 join trace。须满足三个 compat guard（`BLAME_EXAMPLES`、`docs/commands/blame.md` Examples、help banner）并更新 `COMPATIBILITY.md`。 |
| **P1-6** | agent 驱动的主线 commit 自动加 `Co-Authored-By` trailer | `src/command/commit.rs`、`src/internal/ai/history.rs`、`docs/commands/commit.md`、`COMPATIBILITY.md` | S / 低 | 最便宜的 git 原生赢：把归属盖进 commit 对象本身，**随 clone/push 传播、GitHub/git log 直接可读、零 Libra 工具依赖**。复用 `commit.rs::append_trailers()` 与 `history.rs::format_libra_trailers` 的 trailer 模式。**硬前提**：得先让 commit 知道自己“在 agent session 内”——这条耦合今天不存在。建议先用 config 开关 `ai.coAuthoredBy` 起步。 |
| **P1-7** | `libra log --ai-only / --human-only / --model <id>` | `src/command/log.rs` | M / 低 | trailer 落主线后，这是 `CommitFilter` 上一个纯消息谓词，零 schema 变更，照 `--author/--grep` 的样子加。严格排在 P1-6 之后。 |

#### P2 — 互操作皮肤 + 聚合

| 编号 | 任务 | 触及文件 | 工作量/风险 | 说明 |
|---|---|---|---|---|
| **P2-8** | `usage report --by file` | `src/command/usage.rs`、`src/internal/ai/usage/query.rs` | L / 中 | `usage.rs` **已有 spec 缺的整套聚合/过滤/JSON-CSV 管线**，只差 file/path 维度——P0-1 的区间一旦喂进来即近乎免费。这把 Libra 的数字变成“哪个文件/目录/release 多少比例 AI、哪个模型、随时间趋势”的**权威答案**。 |
| **P2-9** | 在 `publish/ai_export` 边界导出标准 `TraceRecord` | `src/internal/publish/ai_export.rs`、`contract.rs`、`tests/data/publish/` | M / 低 | 这是“加入联盟”的交付物：把内部模型映射成规范 JSON，`vcs:{type:git, revision}` 绑真实 commit OID，把 Libra 更深的 Intent/Plan/Task 本体塞进 `metadata["tools.libra.*"]`，`conversation.url` 指向 `associatedIds.tracesCommit`。导出优先于导入；导出必须用严格三段 semver，不要继承参考实现的 `version: "1.0"`。 |

#### P3 — 护城河（spec 回避、唯有真 VCS 能做）

| 编号 | 任务 | 触及文件 | 工作量/风险 | 说明 |
|---|---|---|---|---|
| **P3-10** | 签名 trace 记录（Vault PGP） | `src/internal/vault.rs`、`publish/contract.rs` | M / 中 | 复用 `vault::pgp_sign`，把自声明变成可验证凭据；签名嵌入 `metadata["tools.libra.signature"]`。坑：`vault` 没有 verify，验证侧是净新工作 + 密钥分发/信任模型。 |
| **P3-11** | rebase/cherry-pick 后重锚 trace | `src/command/rebase.rs`、`src/command/cherry_pick.rs` | XL / 高 | old→new commit 映射已存在，**commit 级 note/链接复制可行**；**行级重排**因 rebase 不算 `compute_diff` 而是净新工作。压到最后，且依赖 P0-1/P1-4。 |
| **P3-12** | merge 归属 | `src/command/merge.rs` | L / 中 | union 双亲的 trace、重叠且 contributor 不同的区间标 `mixed`，把 spec 的歧义变成可复现规则。 |
| **P3-13** | 外部 `.agent-trace/traces.jsonl` 导入 | `src/internal/ai/observed_agents/adapter.rs` | L / 中 | 价值有限（继承 spec 全部弱点：无签名、位置漂移、重叠声明无解）。即便做也只作 `trusted=false` 的独立命名空间，在 `blame --ai` 中标 `(external claim, source=agent-trace-jsonl)`，绝不与 Libra 权威数据混合。必须实现 Cursor 参考实现的全部 fallback 语义 + 合成路径过滤（.shell-history 等应忽略或特殊处理）。更像 `observed_agents` 适配器的扩展而非全新命令。 |

### 4.4 明确不建议 / 易踩坑

- ❌ **别改 `git-internal` 的 `TouchedFile` / `Provenance`**（外部 crate + `deny_unknown_fields` 前向不兼容）→ 行区间类型放 Libra 仓内。
- ❌ **别把 `notes` 当跨工具互操作后端**（SQLite 本地、不随 push 传输）→ notes 只做本地存储，互操作只在 publish 导出。
- ❌ **别假设 Libra `commit` 是 AI 写入路径** → 现状是 observe-external；trailer/链接类功能要先补 commit↔session 耦合。
- ❌ **别照搬 Cursor 的 reference hook/store 当 Libra 实现** → 它是示例代码，存在版本形状不一致与区间 fallback，适合作导入兼容样本，不适合作权威采集路径。
- ⚠️ 任何写 OID 的新代码都要走 **hash-kind preflight**（`cli.rs` 读 `core.objectformat`），别硬编码 40-hex。
- ⚠️ `apply_patch` 之外的 observed 外部 agent（Claude Code/Cursor/Codex…）拿不到原生区间，但其**完整转写已被 redact 后存为 blob**（由 `src/internal/ai/hooks/runtime.rs` 写入），可后处理重解析出区间——重解析须**容忍被 redact 的片段**，不能假设逐字。
- ❌ **别假设 `ai_edit_trace` 是 worktree-私有表** → `.libra` 是指向 shared storage 的符号链接，DB 跨 worktree 共享（§7.2）。任何按“全表 NULL `commit_oid`”做的回填/清理都会跨会话串号；一切写/回填/GC 都要按 `session_id` scope。

### 4.5 最小起步序列

> **P0-0（交换合同，S） + P0-3（model_id 规范化，S） + P0-1/P0-2（`apply_patch` 行级采集，M）** 是无依赖的真地基；**P1-6（`Co-Authored-By`，S）** 是最便宜的用户可见 git 原生赢（补上 session 耦合后）。这些落地即可解锁 **P1-5（`blame --ai`）** 与 **P2-8（`usage --by file`）**。先做这几件，Libra 就从“文件级封闭”迈到“行级 + 可向联盟导出”，且每一步都是加性、低风险、可独立交付。

### 4.6 Cursor Agent Trace 方案 vs Libra 方案的本质差异（总结）

| 维度 | Cursor / Agent Trace（hook 世界） | Libra（真 VCS + 运行时） | Libra 应发挥的优势 |
|------|----------------------------------|---------------------------|-------------------|
| 采集时机 | afterFileEdit / PostToolUse hook，**声称**区间 | `apply_patch` 内部 `compute_replacements`，**实际执行前**精确区间 | 权威性高一个数量级；可核对 patch 实际影响 vs 声称 |
| 存储 | `.agent-trace/traces.jsonl` append-only，无索引 | SQLite `ai_edit_trace`（规划）+ `agent_checkpoint` on orphan ref + 对象库 | 可查询、可 join usage、支持 blame 同进程合成 |
| 持久性 | 工作树文件，易丢、易被用户手改 | 受 `libra commit` / checkpoint 保护，进入对象存储与 publish 流程 | 历史可验证 |
| 重写处理 | 未定义（rebase 后 line 静默漂移） | rebase/cherry-pick 有 old→new commit 映射；未来可做范围重排 | 解决 spec 最大痛点之一 |
| 可信度 | 自声明 | 可签名（Vault PGP）+ source 枚举 + trusted 位 | 合规/审计场景的差异化卖点 |
| 互操作 | 联盟目标（Cursor 牵头，多家参与） | 通过 publish/ai_export 边界 emit 标准格式 | 既能“加入生态”又不牺牲内部深度模型 |
| 粒度 | 行区间（conversation 聚合） | 目前 session/文件级；PatchSet 是对象级 | 补行级后同时拥有最细 + 最完整的对象链路 |

一句话：**不要把 Agent Trace 当内部模型用**，把它当**可互操作的皮肤**。Libra 的护城河在于“把 spec 故意留白的难题用真 VCS 能力解掉”，并在采集源头做到 hook 做不到的精确。

### 4.7 补充设计建议（二次核验后的增补）

> 以下基于对 `reference/{trace-store,trace-hook}.ts`、`.cursor/hooks.json`、`.claude/settings.json` 的精读，补充 4.3 路线图中未充分覆盖的设计点。

#### a. 外部 trace 导入端的压实（compaction）策略

参考实现每次 hook 调用产生一条独立 TraceRecord，无去重、无合并。P3-13 的导入器必须实现 **same-conversation compaction**：

- 按 `(vcs.revision, file.path, conversation.url | metadata.conversation_id)` 分组。
- 合并相邻或重叠 ranges：`end_line[i] + 1 >= start_line[i+1]` → 合并为 `[start_line[i], end_line[i+1]]`（间隙 ≤1 行的也合并，因 formatter 可能重排）。
- contributor 一致的合并后保留单一 contributor；不一致时拆分保留各自 range。
- 压实后标记 `source=external_claim`, `trusted=0`；`metadata["tools.libra.compacted_from"]` 记录原始记录数。
- 忽略 `.shell-history` / `.sessions` 合成文件（它们不在工作树中，无行级归属语义）。

#### b. 导出的字段语义约定

| 字段 | 约定 | 依据 |
|------|------|------|
| `id` | 由 `(traces_commit, file_path, start_line, end_line)` 派生 UUID v5（namespace = `tools.libra`），**不要每次导出重新生成随机 UUID** | 保证同一 trace 多次导出的 `id` 幂等，便于下游去重 |
| `timestamp` | 代码被生成的时刻（apply 时间），与 `ai_edit_trace.created_at` 对齐 | 语义最贴近"代码何时产生"，而非 commit 或导出时刻 |
| `confidence` | `libra_apply_patch` → `1.0`；`libra_observed_agent`（后解析） → `0.6-0.9`（按 redaction 损失率）；`external_claim` → `≤0.3` | 直接量化 4.1 的信任模型 |
| `vcs.type` | 填 `"git"`；`metadata["tools.libra.vcs_actual"]` 标 `"libra"` | Libra 不在 spec 枚举 `["git","jj","hg","svn"]` 中，但 git-compatible |
| `model_id` | `ModelBinding::to_canonical_string()` 产出 `provider/model[@variant]`；导出时 `@variant` 剥到 `metadata["tools.libra.model_variant"]` | models.dev 约定不含 variant 后缀 |
| `content_hash` | `integrity:sha256:<hex64>` via `IntegrityHash::compute` | 它是行内容 hash（跨重写位置追踪），**不是** git object hash |
| `version` | 严格三段 semver，跟随 Agent Trace spec 最新稳定版本；本地 config `ai.traceExportVersion` 可 pin | spec 自身有 4 种 version 形状（见 §1.4.c），Libra 导出端不应参与混乱 |

#### c. 用 `related[]` 标准化暴露 Libra 深模型

Libra 的 Intent→Plan→Task→Run→PatchSet→Provenance 对象链路不应只埋进 `metadata["tools.libra"]`（其他工具无法理解 vendor key）。应同时用 spec 的 `related[]` 暴露**类型标签**——即使链接不可解析，标签本身已是可被任何 Agent Trace reader 消费的标准化信号：

```json
"related": [
  { "type": "intent",   "url": "libra://intent/<intent_id>" },
  { "type": "plan",     "url": "libra://plan/<plan_id>" },
  { "type": "task",     "url": "libra://task/<task_id>" },
  { "type": "run",      "url": "libra://run/<run_id>" },
  { "type": "patchset", "url": "libra://patchset/<blob_oid>" }
]
```

这个做法的价值：让任何 Agent Trace 兼容工具**不读 vendor metadata 也能感知**"这个文件被完整的规划-执行-验证链路覆盖过"——`related` 的 `type` 标签就是最低成本的互操作信号。

#### d. vendor metadata 版本化 + P3-11 拆分

- **`metadata["tools.libra"]` 须自含 schema version**（如 `"tools.libra.schema_version": 1`），否则内部对象模型演进时外部消费者静默破裂。
- **P3-11（rebase 重锚）建议拆分为二**：
  — **P1-11a**（轻量）：commit 级 trace 链接重锚——`applied_commits` 映射已存在，旧 OID → 新 OID 是纯查表，无行级计算。排在 P1-4 之后。
  — **P3-11b**（重量）：行级重排——需在新 commit 上重新计算每行归属，净新工作。保持 P3。
  拆分后 rebase 后的 `blame --ai` 至少在 commit 级不会完全失配。

#### e. `mixed` contributor 的实用主义近似

实时区分"human-edited AI output"需对每次 apply 做前后内容 diff 比对（昂贵）。建议初始策略：同一 session 内，同一文件被多个 run 的 apply 修改、且 contributor 不一致时，重叠区间标 `mixed`——只做同 session、commit 前的区间重叠检测（interval tree，线性）。跨 session 的 `mixed` 推迟到 P3-12（merge 归属）。

#### f. 各 P 任务的测试要求（对应 AGENTS.md 纪律）

| P 编号 | 最低测试要求 |
|--------|------------|
| P0-0 | `tests/data/publish/` 放 golden trace fixture + 从 Cursor 参考实现提取的含 `"1.0"` + fallback range 的真实样本，做导入 round-trip 测试 |
| P1-4 | migration apply/revert 测试 + `ai_edit_trace` CRUD 集成测试 |
| P1-5 | `blame --ai` 须满足三个 compat guard（`BLAME_EXAMPLES`、`docs/commands/blame.md` Examples、help banner）；加行级 join 正确性测试 + 外部 trace 低 trust 标注测试 |
| P1-6 | `Co-Authored-By` trailer 生成 + `log --ai-only` 端到端测试 |
| P2-8 | `usage report --by file` JSON/CSV 输出格式 fixture 固定 |
| P2-9 | 导出 round-trip：Libra 内部 → TraceRecord JSON → schema 校验 → 反序列化对比 |

---

## 5. 核验过的代码锚点

| 主张 | 锚点 |
|---|---|
| hook 把 `tool_input` 塌成布尔丢弃区间 | `src/internal/ai/hooks/runtime.rs`（`append_normalized_event`、`has_tool_input`）；`tool_input` 已在更前处 redact |
| `apply_patch` 已算出精确区间却仅供 TUI | `src/internal/ai/tools/apply_patch/core.rs`（`compute_replacements`）；`handlers/apply_patch.rs`（`FileDiff` + unified diff） |
| 全套归属身份已在 dispatch 处在场 | `src/internal/ai/usage/recorder.rs`（`UsageContext`）；`agent/runtime/tool_loop.rs`（`ToolLoopConfig`） |
| model 已拆 provider/model，且有规范化器 | `agent/profile/spec.rs:129`（`ModelBinding::to_canonical_string`）；`agent_run/event.rs:267`（`Spawned`）；`agent_usage_stats` |
| `TouchedFile` 仅文件级计数，无区间 | `git-internal`（外部 crate）`object/patchset.rs`（`lines_added/lines_deleted`） |
| `blame` 零 AI 维度 | `src/command/blame.rs:72`（`BlameLine`：`line_number/short_hash/hash/author/date/content`）；`compute_diff` 的调用方为 `blame.rs` + `log.rs`（rebase/cherry-pick 0 命中） |
| `usage` 有聚合管线但无 file 维度 | `src/command/usage.rs`（`UsageReportBy`）；`usage/query.rs` |
| `notes` 为 SQLite + blob 哈希，非 git-wire ref | `src/internal/notes.rs`；`sql/migrations/2026061401_notes.sql`（`idx_notes_ref` 仅 `(notes_ref)`） |
| commit 不写 agent 链接；归属在 orphan ref | `src/command/commit.rs`（无 `traces_commit` 写入）；`history.rs`（`format_libra_trailers`、`refs/libra/traces`） |
| rebase old→new 映射存在；但不算行级 diff | `src/command/rebase.rs`（`summary.applied_commits` / `RebaseAppliedCommitOutput`）；`compute_diff` 0 命中 |
| Vault 能签不能验 | `src/internal/vault.rs`（`pgp_sign`、`signature_to_gpgsig`；无 verify） |
| ai_export 为对象级，无 files/ranges | `src/internal/publish/ai_export.rs`（Intent/Plan/Task/Run/PatchSet/Provenance）；`associatedIds.tracesCommit` |
| Agent Trace reference 版本与区间 fallback 不可照搬 | `/Volumes/Data/cursor/agent-trace/schemas.ts`（`version` regex）；`reference/trace-store.ts:151`（`version: "1.0"`）、`:102`（`computeRangePositions` 三级回退 + 合成路径） |
| `IntegrityHash::compute` 可用 | `git-internal-0.7.4/src/internal/object/integrity.rs:31` |
| Cursor hook 采集 vs Libra apply_patch | `reference/trace-hook.ts:94`（PostToolUse 走 tool_input 回查）；Libra `apply_patch/handlers/apply_patch.rs:145`（只转 unified diff 给 TUI，区间丢弃） |
| normalized event 丢区间 | `hooks/runtime.rs:1118`（`"has_tool_input": ...`，原始内容已在 redaction 层前丢弃） |

---

## 6. Libra 内部模型 → Agent Trace 交换示例

一个最小导出 fixture（P0-0 应固定）示例：

```json
{
  "version": "0.1.0",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2026-06-18T10:00:00Z",
  "vcs": {
    "type": "git",
    "revision": "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0"
  },
  "tool": {
    "name": "libra",
    "version": "0.42.0"
  },
  "files": [
    {
      "path": "src/utils/parser.rs",
      "conversations": [
        {
          "url": "libra://refs/libra/traces/a1b2c3d4",
          "contributor": {
            "type": "ai",
            "model_id": "openai/gpt-4o"
          },
          "ranges": [
            {
              "start_line": 42,
              "end_line": 67,
              "content_hash": "integrity:sha256:9f2e8a1b..."
            }
          ],
          "related": [
            { "type": "session",   "url": "libra://session/0192c6c0-..." },
            { "type": "intent",    "url": "libra://intent/<intent_id>" },
            { "type": "plan",      "url": "libra://plan/<plan_id>" },
            { "type": "task",      "url": "libra://task/<task_id>" },
            { "type": "run",       "url": "libra://run/<run_id>" },
            { "type": "patchset",  "url": "libra://patchset/<blob_oid>" }
          ]
        }
      ]
    }
  ],
  "metadata": {
    "confidence": 1.0,
    "source": "libra_apply_patch",
    "trusted": true,
    "tools.libra.schema_version": 1,
    "tools.libra": {
      "session_id": "0192c6c0-...",
      "run_id": "run-...",
      "checkpoint_id": "cp-...",
      "traces_commit": "a1b2c3d4...",
      "hash_kind": "sha1",
      "vcs_actual": "libra",
      "model_variant": "thinking"
    }
  }
}
```

说明：

- `url` 在 Libra 导出时指向内部 ref/session；外部联盟工具可安全忽略。
- `content_hash` 使用 `integrity:sha256:<hex>`，由 `git-internal::IntegrityHash::compute` 计算，不采用 spec 示例中的 `murmur3`。注意它**不是** git object hash——它是被修改行的内容 hash，用于跨重写位置追踪。
- `metadata.source` 枚举内部可信来源（`libra_apply_patch` = 精确区间，`libra_observed_agent` = 后解析，`external_claim` = 第三方声明）。`blame --ai` 读取时应据此降权或标注。
- `confidence` 按 source 定值：`libra_apply_patch` → `1.0`，`libra_observed_agent` → `0.6–0.9`（按 redaction 损失率），`external_claim` → `≤0.3`。
- `tools.libra.schema_version` 自含版本号，供外部消费者做兼容分发。
- `tools.libra.vcs_actual` 标 `"libra"`——因为 Libra 不在 Agent Trace 的 `vcs.type` 枚举中，但在 `metadata` 中注明实际 VCS。
- `tools.libra.model_variant` 剥离自 `model_id` 的 `@variant` 后缀——models.dev 约定不含 variant。
- `related[]` 用标准化 `type` 标签暴露 Libra 的 Intent→Plan→Task→Run→PatchSet 对象链路，使任何 Agent Trace 兼容工具**不读 vendor metadata 也能感知**完整的规划-执行-验证链路。

---

## 7. 二次核验与增补（2026-06-18 第二轮）

> 本节是对 §1–§6 的第二轮 ground-truth 核验 + 4 个此前未充分覆盖的设计维度。所有代码主张已对当前树逐条核实（结论见 §7.5）。

### 7.1 端到端生命周期（单图）

§2/§4.3 用分相表格描述了零件，但缺一张贯穿"采集 → 存储 → 回填 → 读取 → 导出/导入"的单流程图。补上如下——它也是实现顺序的依赖事实来源：

```
                          ┌────────────────────────────────────────────────────────┐
   ① 采集（权威）          │ apply_patch/core.rs::compute_replacements                │
   P0-1/P0-2              │   → (start_index, old_len, new_lines) 1-based 精确区间    │
                          │   + UsageContext 身份（provider/model/run_id/session_id） │
                          └───────────────────────────┬────────────────────────────┘
                                                      │ capture sink（内存，无竞态）
                                                      ▼
   ② 存储（本地权威）      ┌────────────────────────────────────────────────────────┐
   P1-4                   │ SQLite `ai_edit_trace`（commit_oid 暂 NULL）              │
                          │   ⚠️ 跨 worktree 共享单表（§7.2）→ 行必带 session_id      │
                          └───────────────────────────┬────────────────────────────┘
                                                      │
   ③ 提交回填             ┌────────────────────────────▼────────────────────────────┐
   P1-4/P1-6             │ libra commit：① 回填 commit_oid（WHERE session_id=?）     │
                          │              ② 可选写 Co-Authored-By trailer（随 push 传播）│
                          │   前提：commit↔session 耦合（今天不存在，需先建，§4.2.3）  │
                          └───────────────────────────┬────────────────────────────┘
                                                      │
        ┌─────────────────────────────────┬──────────┴───────────┬───────────────────────────┐
        ▼ ④ 本地读取                       ▼ ④ 聚合                ▼ ⑤ 互操作导出              ▼ ⑤ 导入
   blame --ai (P1-5)              usage report --by file   publish/ai_export →        observed_agents 适配器
   log --ai-only (P1-7)          (P2-8)                    TraceRecord JSON (P2-9)    ← .agent-trace/*.jsonl
   按行 join ai_edit_trace        复用现有聚合管线          严格三段 semver；深模型      (P3-13, trusted=0,
   标 source/trusted             + file/path 维度          塞 metadata["tools.libra"] 独立命名空间)
        │                                                  + related[] 类型标签
        ▼ ⑥ 重写后保稳（护城河）
   rebase/cherry-pick 重锚：commit 级查表（P1-11a，轻）+ 行级重排（P3-11b，重）
   merge 归属 union + 重叠标 mixed（P3-12）；签名 Vault PGP（P3-10）
```

**关键读法**：唯一的"地基"是 ①（apply_patch 源头采集）；②③ 是把它持久化并绑定 commit；④ 才是用户可见命令；⑤ 是联盟皮肤；⑥ 是 spec 故意回避、唯真 VCS 能做的护城河。**横向的 ④/⑤ 全部依赖纵向的 ①②③ 先打通**——任何想先做 `blame --ai` 或 `导出` 而跳过源头采集的顺序都是错的。

### 7.2 worktree 共享 `.libra` → `ai_edit_trace` 并发模型（本轮新增的硬约束）

这是 §4 路线图原本**完全没有覆盖**、却会在多会话场景直接导致归属串号的结构性事实。

**事实链（已核验）**：

1. `libra worktree add` 把新 worktree 的 `.libra` 建成**指向 shared storage 的符号链接**（`src/command/worktree.rs:671` 文档注释 + `:844` `std::os::unix::fs::symlink(storage, link_path)`）。
2. 所有路径解析走同一 storage：`src/utils/path.rs:23` `database()` = `util::storage_path().join("libra.db")`，`index()`/`objects()` 同理。
3. 因此**所有 worktree 共享同一个 `.libra/libra.db`**，。HEAD/index 曾经也共享,但自 plan-20260714 Part C W0 起已按 `worktree_id` 隔离,与 git worktree 的"各自独立 HEAD/index"语义一致;共享的只剩物理数据库文件本身。

**推论（对 P1-4 的修正）**：

- `ai_edit_trace` 是一张**跨所有并发 agent 会话的单一物理表**，不是 per-worktree。
- 朴素回填（commit 时 `UPDATE … WHERE commit_oid IS NULL`）在两个 worktree/会话并发时**会把 A 的提交盖到 B 的待提交行上**。→ 回填 **MUST** `WHERE session_id = ? AND commit_oid IS NULL`。
- 建议表加 `worktree_id` 列（取自 `util::storage_path()` 能解析到的实际 worktree 根），用于：① 审计"哪个 worktree 产生的归属"；② `blame --ai` 在共享库里按 worktree 过滤；③ GC 时安全清理某个已删除 worktree 的孤儿行。
- 写入争用：`db.rs` 默认 30s `busy_timeout` 兜底 SQLite 串行写；但**高频 apply_patch 写入 × 多并发 agent** 可能拉长尾延迟。建议 capture sink 做**会话内批量 flush**（如每 N 次 apply 或 commit 前一次性 INSERT），而非每次 apply 单条事务。
- crash 恢复（§P1-4 已述）同样要叠加 session scope：反查 `run_id → session_id → agent_checkpoint.traces_commit` 补填时，只补本会话的 NULL 行。

> 一句话：**Libra 的 worktree 隔离模型比 git 弱，归属表必须自己用 `session_id`/`worktree_id` 重建隔离**，不能依赖文件系统层面的 per-worktree DB。

### 7.3 互操作生态定位：为什么"只能在 publish 边界互操作"是结构性结论

Agent Trace 联盟成员（README 致谢）大致分两类采集/存储形态，理解它们能精确定位 Libra 该在哪一层接：

| 形态 | 代表 | 存储/传播 | 与 Libra 的接点 |
|---|---|---|---|
| **编辑器/Agent hook → 本地 JSONL** | Cursor、Cline、Amp、OpenCode、Jules | `.agent-trace/*.jsonl` 工作树文件，**不随 VCS 传播** | 只能作 P3-13 低信任**导入**样本 |
| **VCS-native 旁路（git notes 等）** | git-ai（`refs/notes/*` 思路） | git 原生 notes，**随 push/fetch 传输**，外部可发现 | Libra 学不来：Libra 的 `notes` 是 SQLite + blob，**不过线**（§4.2.2 已证 `idx_notes_ref` 仅本地） |
| **平台/分析后端** | Vercel、Cloudflare、Amplitude、Cognition、Tapes | 各自云端 | 消费 Libra **导出**的标准 JSON |

**结论再加固**：Libra 既不能靠工作树 JSONL（易丢、不过线），也不能靠自己的 notes（SQLite 本地、不过线）。**唯一既"过线/可被联盟发现"又"受 VCS 保护"的出口，就是 `publish/ai_export` 把内部权威模型 emit 成标准 `TraceRecord`（P2-9）**。这不是偏好，是 Libra 现有存储形态决定的——publish 是 Libra 体系里唯一已建成的"对外可发现内容"边界。

> 注：若未来确有"wire-native 归属旁路"需求（即不经 publish、随 push 直接带归属），唯一干净路径是 §P1-6 的 **commit trailer**（`Co-Authored-By` / `Libra-*`）——trailer 在 commit 对象里，天然随 clone/push 传播、GitHub/git log 直接可读。这也是为什么 P1-6 被列为"最便宜的 git 原生赢"。

### 7.4 统一 CLI 面 delta 与 compat-guard 义务（落地清单）

把散落在 P0–P3 的用户可见表面集中成一张表，并对齐 CLAUDE.md 的**三道 compat guard + COMPATIBILITY.md + error-codes** 纪律——任何新命令/新标志落地前都要逐项过：

| 任务 | 新增表面 | 类型 | compat-guard / 文档义务 |
|---|---|---|---|
| P1-5 | `libra blame --ai`（`BlameLine` 加可选 `contributor`/`model_id`，`skip_serializing_if=None` 加性兼容） | 新标志 | `BLAME_EXAMPLES` banner、`docs/commands/blame.md` Examples 段、help-examples-banner guard；`COMPATIBILITY.md` blame 行 |
| P1-6 | `libra commit` 自动 `Co-Authored-By`（先以 config `ai.coAuthoredBy` 开关起步） | 行为变更 + config | `docs/commands/commit.md`、`COMPATIBILITY.md` commit 行；config 键文档 |
| P1-7 | `libra log --ai-only / --human-only / --model <id>` | 新标志 | `LOG_EXAMPLES`、`docs/commands/log.md` Examples、help banner；`COMPATIBILITY.md` log 行 |
| P2-8 | `libra usage report --by file`（`UsageReportBy` 加 `File`/`Path` 变体；现仅 `Model`/`Agent`/`AgentProviderModel`） | 新枚举值 | `usage` JSON/CSV fixture；`docs/commands/usage.md`；`COMPATIBILITY.md` usage 行 |
| P2-9 | `libra publish` 导出 `TraceRecord`（或 `libra export --agent-trace`） | 新导出 | publish round-trip fixture `tests/data/publish/`；MIME `application/vnd.agent-trace.record+json` |
| P3-10 | trace 签名（`metadata["tools.libra.signature"]`） | 内部+导出 | 签名 fixture；注意 Vault **无 verify**（净新工作） |
| P3-13 | 外部 `.agent-trace` 导入（observed_agents 适配器扩展，非新顶层命令） | 导入 | golden 兼容样本（含 `"1.0"` + fallback range + `.shell-history` 合成路径） |

**新增 `StableErrorCode` 提醒**：若 P1-4/P3-13 引入新错误码（如导入畸形 trace、签名验证失败），必须同步 `docs/error-codes.md`（否则 `compat_error_codes_doc_sync` guard 红）。

**新增 SQLite 表/迁移提醒**：`ai_edit_trace` 迁移文件按 `YYYYMMDDNN_ai_edit_trace.sql` 命名、排在现最新 `2026061401_notes.sql` 之后；forward DDL 幂等（`CREATE TABLE IF NOT EXISTS`）、配 `_down.sql`，并加 migration apply/revert 测试（CLAUDE.md `sql/migrations/README.md` 约定）。

### 7.5 锚点二次核验结论

本轮对 §5 全部锚点 + 新增主张逐条复核，结果：**14/14 内部代码主张对当前树准确**。修正/精化两条：

- ✏️ `compute_diff` 的调用方是 **`blame.rs` + `log.rs`**（原文 §5 写"仅此处用"已更正）；rebase/cherry-pick 仍 0 命中（结论不变）。
- ⚠️ `git-internal::IntegrityHash::compute` 位于**外部 pinned crate**，本仓树内 grep 不到，是唯一**未能在树内自证**的锚点——见 §4.2"利好"已加的告警；落地前对照 vendored crate 源确认。

精化的精确锚点（供实现直接跳转）：`BlameLine` @ `blame.rs:72`（`line_number/short_hash/hash/author/date/content`）；`UsageReportBy` @ `usage.rs:94`（`Model`/`Agent`/`AgentProviderModel`）；`compute_replacements` @ `apply_patch/core.rs:285`（私有，兄弟 `apply_replacements` @ `:585` 亦私有）；TUI-only diff 注释 @ `handlers/apply_patch.rs:144`；`append_normalized_event` @ `hooks/runtime.rs:1100`（`has_tool_input` @ `:1118`）；`HookTarget::AgentTraces` Phase-1 stub @ `hooks/runtime.rs:76-83`（运行时 reject "not yet wired"）；`traces_commit` @ `publish/contract.rs:384`；`TRACES_BRANCH` @ `branch.rs:42`；`ModelBinding::to_canonical_string` @ `spec.rs:129`；`vault::pgp_sign` @ `:218` / `signature_to_gpgsig` @ `:658`（无 verify）；`RebaseAppliedCommitOutput` @ `rebase.rs:566`。

---

## 8. 参考

- Agent Trace 规范与参考实现（Cursor）：`/Volumes/Data/cursor/agent-trace`（`README.md`、`schemas.ts`、`reference/{trace-store,trace-hook}.ts`、`index.ts` 用于构建 JSON Schema 与站点）。
- Libra AI 对象模型：[`docs/ai/object-model-reference.md`](../ai/object-model-reference.md)、[`docs/development/code-agent-runtime.md`](code-agent-runtime.md)。
- 兼容性矩阵：[`COMPATIBILITY.md`](../../COMPATIBILITY.md)。
- 关键实现锚点：`src/internal/ai/tools/apply_patch/core.rs:285`（compute_replacements）、`src/internal/ai/tools/handlers/apply_patch.rs:145`（仅产 TUI metadata）、`src/internal/ai/hooks/runtime.rs:1100`（append_normalized_event 塌陷）、`src/internal/ai/observed_agents/`（外部采集 + redaction + traces orphan ref）、`src/internal/publish/ai_export.rs`（对象级导出）。
