# Agent Trace 对照 Libra 归属方案：分析与改进设计

## 状态

Proposed（分析 / 设计意见）

> **状态注（2026-08-27 校验）**：本文 P0–P3 的**全部任务卡至今零落地**——`ai_edit_trace` 表（`grep -rn "ai_edit_trace" src/ sql/` 零命中）、`blame --ai`（`src/command/blame.rs` 无该标志）、`log --ai-only`（`src/command/log.rs` 无 `ai_only`）、`usage report --by file`（`UsageReportBy` @ `src/command/usage.rs:97` 仍仅 `Model`/`Agent`/`AgentProviderModel`）、`TraceRecord` 导出/导入在树内均无实现。对应的长期计划条目是 [`plan-long.md`](../plan/plan-long.md) 的 **AG-ATTR**（`plan-long.md:267`：「Agent 代码归因与 transcript 归一（候选）| P2 | **候选** | ……先只读导出，不改 Git 对象默认语义」），**未排期**。因此本文仍是设计意见而非实施计划；本轮刷新只更新论证前提与代码锚点，不改判路线图本体。

## 日期

2026-06-18（成文）

最近校验：2026-08-27（第一次周期刷新，明细见 §9）

## 参照与基线

- **参考项目路径**：`/Volumes/Data/competition/cursor/agent-trace`（HEAD `2754f07` = `2754f077f3e50c1fb5088183f5c9362077cc8ca1`，最后提交 2026-02-06 "Update abstract"）。⚠️ **上游远端 404**，[`plan-long.md:64`](../plan/plan-long.md) 记该仓库为 `blocked-network`——本地 revision **未证明是远端最新**（详见 §1 开头的上游状态告警）。
- **参考版本**：Agent Trace **v0.1.0**（`README.md:3` `**Version**: 0.1.0`），许可 CC BY 4.0（`README.md:537`）。
- **Libra 侧基线**：`main` HEAD `89081277a`，`Cargo.toml` 版本 **0.21.27**（`Cargo.toml:3`）。本文所有 Libra 现状断言均已对照该基线逐条复核。

## 摘要

本文对照 Cursor 主导的开放规范 **Agent Trace**（v0.1.0 RFC，CC BY 4.0）与 Libra 当前 AI 代码归属（attribution / provenance）实现，给出一份可直接落地、分阶段的改进路线图。

**一句话结论**：Agent Trace 与 Libra 在同一问题上做了不同取舍——Agent Trace 赌**互操作**（极简、厂商中立的 JSON 交换格式），Libra 赌**深度**（SQLite + 内部对象模型 + 真 VCS 语义）。Libra 的内部模型在对象粒度上已经比 Agent Trace 更深；照搬 spec 当作内部模型是降级。Agent Trace 对 Libra 的价值只集中在两点：

1. 补上 Libra 真正缺失的**行级归属（line-range）**。
2. 把归属做成一层标准化的**交换皮肤**，以接入 Agent Trace 联盟（Amp、Cline、Vercel、Cloudflare、Cognition、OpenCode、Jules …）。

而 Libra 作为**真 VCS**，恰好能解决 spec 自己回避的三个最难问题——**可查询的持久存储、可签名可验证、rebase/merge 后仍稳定**——应作为 Libra 的差异化主线。

> **⚠️ 护城河降调（2026-08-27 复核）**：上述三项**不再等价**，因为竞争格局已变。`/Volumes/Data/competition/git-ai-project/git-ai`（`793066013`，2026-08-24）是已出货的开源 Git 扩展，把行级归属存进 `refs/notes/ai`（`specs/git_ai_standard_v3.0.0.md:23-24` 明令不得使用 `refs/notes/commits`），并声明**随 `git push`/`git fetch` 同步**（`README.md:250`）；其规范 v3.0.0 的 **§2 History Rewriting Behaviors**（`:420-881`，含 2.1 Rebase `:424`、2.2 Merge `:580`、2.3 Reset `:632`、2.4 Cherry-pick `:689`、2.5 Stash/Pop `:749`、2.6 Amend `:817`）逐场景规定了历史重写下归属的 MUST 行为，且已有 `git ai blame`（`README.md:20,137`）。因此逐项重估：
>
> - ① **可查询的持久存储**——仍成立，但差异收窄为「与 SQLite 深模型 + `usage` 聚合**同库可 join**」。git-ai 的 OSS CLI 只做归属，跨 PR/团队/仓库的聚合是其付费 teams 版（`README.md:209`）。
> - ② **可签名可验证**——**唯一未被覆盖的真差异**：`specs/git_ai_standard_v3.0.0.md` 全文对 signature/signed **零命中**。
> - ③ **rebase/merge 后仍稳定**——**已被竞品覆盖**，不再是 Libra 独有。
>
> 措辞订正：原文「这是任何 hook 式编辑器插件结构上做不到的」已删。git-ai 声明**不用 Git hooks、不 wrap Git 二进制**（`README.md:80,199-200`），但它仍**由自己安装并托管 agent hooks** 采集（`README.md:202-203` "Git AI manages the agent hooks"）——所以 §1.4 关于「hook 采集保真度低于 `apply_patch` 源头」的论证**对 git-ai 依然成立**，被推翻的只是「hook 世界结构上无法持久化/过线」这一条。

---

## 1. Agent Trace 方案速览

> **⚠️ 上游状态告警（2026-08-27 新增）**：本文成文时把 Agent Trace 当作「活跃演进中的开放规范」，该前提**已不再可靠**。[`plan-long.md:64`](../plan/plan-long.md) 与 `:112` 记 `cursor/agent-trace` 远端 `git fetch` **仍 404（仓库已删除或迁移）**，状态 `blocked-network`；本地 checkout `2754f07` 的最后提交是 **2026-02-06**，自本文成文（2026-06-18）以来**零提交**。这意味着：
>
> 1. 本地 revision 是本文唯一可读的事实源，**未证明是远端最新**；规范可能已停更或迁移。
> 2. 「对齐该规范」的收益应按**停更风险折价**——这正是把 P2-9（导出）继续排在 P3-13（导入）之前、且两者都不进 P0/P1 的直接理由。
> 3. **AG-ATTR 的完成判据不应绑定在本规范上**（见 §4.1.3 与 §7.3）。
>
> 下述 §1–§1.4 的全部竞品事实断言已按本地 `2754f07` 逐条复核准确，仅 §1.4.c 的一处行号精度已订正。

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

`schemas.ts:89` 的 version regex 是 `^[0-9]+\\.[0-9]+\\.[0-9]+$`（强制三段 semver），但**紧接的 `:90`** `.describe()` 给出的示例是 `"e.g., '1.0'"`——**描述自身给了一个会被 regex 拒绝的例子**。加之 `reference/trace-store.ts:151` 实际写 `version: "1.0"`、README 示例用 `0.1.0`、spec 正文 §6.1 的 JSON Schema description 又写 `"e.g., '1.0.0'"`，一篇 spec 内出现 4 种 version 形状。导入端必须在解析层宽容接受再内部规范化，不能在任何边界做严校验。

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
| `observed_agents` | `src/internal/ai/observed_agents/`（2026-08-27：已由 3 个文件长到 15 个 `.rs` + `builtin/`，新增 `trust/coverage/compliance/capability/registry/transcript_source/opencode_export/skill_projection/preview/extract/rpc`）+ hooks runtime | session / checkpoint on `refs/libra/traces` | 封闭（可导出） | 外部 hook 捕获 + redaction + 归一化事件（`has_tool_input`）；**仍缺的只剩 model_id 规范化到 conversation 级与行区间**（`grep -rn "to_canonical_string" src/internal/ai/observed_agents/` 零命中）。~~`HookTarget::AgentTraces` 仍为 Phase1 stub~~ → **已实现**（见下） |
| `session` + `file_history` | `src/internal/ai/session/{state,file_history,store,jsonl}.rs` | session | JSON 可导出 | model 仅 session 级；`file_history` 只记**文件**快照，不记哪些行 |
| `usage` | `src/internal/ai/usage/{recorder,pricing,query,format}.rs`、`src/command/usage.rs` | session | 封闭 | **已有完整聚合/过滤/JSON-CSV 管线**，但无 file/path 维度 |
| `agent_run` | `src/internal/ai/agent_run/{patchset,evidence,event}.rs` | mixed | 封闭 | `TouchedFile` 只有 `path/change_type/lines_added/lines_deleted`，**无行区间**（结论不变）。（2026-08-27 订正：该类型定义在**外部 crate** `git-internal` 的 `object/patchset.rs:61-70`，`grep -n "TouchedFile" src/internal/ai/agent_run/patchset.rs` 零命中。**2026-08-27 复核再订正**：Libra 侧 `TouchedFile` 的真实使用点只有 `src/internal/ai/mcp/resource.rs:37`（import）与 `:2380`（`TouchedFile::new`）；`mcp/resource.rs:1136` 与 `src/internal/ai/orchestrator/persistence.rs:4584-4615` 是**仓内自定义的镜像参数结构 `TouchedFileParams`**，不是外部类型的使用点——它可自由扩展，正是行区间字段该落的地方） |
| `hooks` | `src/internal/ai/hooks/{lifecycle,runtime}.rs` | session | 封闭 | `runtime.rs::append_normalized_event_with_envelope`（@ `:3110`，**生产路径**，调用点 @ `:3043`）把 `tool_input` 塌成布尔 `has_tool_input`（@ `:3147`），**把行数据丢了**（2026-08-27 复核订正：同名的 `append_normalized_event` @ `:3097` 带 `#[cfg(test)]` @ `:3096`，只是转调前者的测试包装，不是机制主体） |
| `publish/ai_export` | `src/internal/publish/ai_export.rs` | commit | JSON 可导出 | 对象级（Intent/Plan/Task/Run），**无 `files[]/ranges[]`** |
| git 表面 | `src/command/{commit,blame,log,notes}.rs`、`src/internal/ai/history.rs` | commit | git-native | `blame` 零 AI 维度；无 `Co-Authored-By` 自动写入；但已有 `notes` 设施与 `Libra-*` trailer |

**一句话**：Libra 处处停在**文件级 / 会话级**，唯独没有 Agent Trace 的核心——**行级**。但它已具备 model 拆分、聚合管线、notes、trailer、rebase 引擎等全部“半成品零件”。

---

## 3. 关键判断

- Libra 的内部 AI 对象模型（Intent → Plan → Task → Run → PatchSet → Provenance，见 `ai_export.rs`）在**对象粒度上已经比 Agent Trace 更深**。把 Agent Trace 当作**内部模型**来采用，对 Libra 是降级。
- Agent Trace 对 Libra 的价值**纯在边缘**：① 补上 Libra 真正缺的**行级粒度**；② 提供一层**厂商中立的交换皮肤**用于联盟互操作。
- **正确定位**：把 Agent Trace 当**导出/导入格式**（在 `publish` / `export` / 可选导入边界 emit/ingest），**内部深模型保持私有作为护城河**，并在 spec 明确回避的三件事上领先——**持久可查询存储、可签名可信记录、重写后稳定的归属**——这些是 Libra 作为真 VCS 的结构性优势（其中「重写后稳定」已被 git-ai 覆盖，见摘要的护城河降调注）。
- **（2026-08-27 新增）计划治理归属**：本文 P0-0..P3-13 的全部任务卡隶属 [`plan-long.md`](../plan/plan-long.md) 的 **AG-ATTR**（`:267`，**P2 / 候选 / 未排期**）。其完成判据为 `plan-long.md:276`：「**至少一种外部 transcript/归因格式可导入为只读证据；默认不污染 Git 历史**」。
  → 判据落在**导入侧**（对应本文 P3-13），而本文正文把 P3-13 排在最后且评为「价值有限」。二者不冲突，但须显式调和：**AG-ATTR 的完成判据由 P3-13（或等价的 git-ai / trajectory 导入器）满足；P2-9 导出是超出判据的加分项。判据不要求格式必须是 agent-trace**——`plan-long.md:267` 本身即并列了 agent-trace / trajectory / git-ai 三种证据来源（见 §7.3）。
  → **优先级重估待决策**：同目录 [`delta.md`](delta.md) §6.2 **B1** 建议「AG-ATTR 由 P2 候选**升为 P1**，并与 `plan-20260822` 的 **CH-04 `ai_operation_link`** 排为同一条线（CH-04 供稳定 ID，AG-ATTR 供查询面）」。本文**不单方面改判**，只登记该提案为待决策项（详见 §4.3 P1-5 的交叉引用注）。

---

## 4. 合并后的改进意见

### 4.1 设计原则

1. **内部权威、外部交换分层**：Libra 自己产生的 `apply_patch` / agent runtime trace 才是权威归属数据；Agent Trace 只作为 `publish` / export / 可选导入的交换层。不要把 `.agent-trace/traces.jsonl` 当成本地事实源，也不要用它替代 `agent_session`、`agent_checkpoint`、`agent_usage_stats`、`refs/libra/traces`。
2. **采集优先级必须从 VCS 内部开始**：`apply_patch` 已在写文件前知道精确替换区间，比 hook payload 或字符串回查更可信；外部 hook 只能作为 `trusted=false` 的外部声明进入系统，在 `blame --ai` 中必须可区分。
3. **导出严格，导入宽容**：Libra 导出的 `TraceRecord.version` 必须使用三段 semver（当前按 RFC 用 `0.1.0`），通过 fixture 固定 MIME 与 JSON schema；导入时可兼容参考实现里出现的 `1.0` 等非三段版本，但要规范化为内部版本并打低置信/兼容警告。
   **（2026-08-27 补充，对齐 `plan-long.md:253`「未冻结 RFC 前当完成标准 → 不应照搬」）**：在上游远端 404 且规范自 2026-02-06 停更的前提下（见 §1 上游状态告警），导出版本应**pin 到本地 revision `2754f07` 观察到的 `0.1.0`**，并由本地 config `ai.traceExportVersion` 可覆写；**规范未冻结前，不把 schema 一致性当作 AG-ATTR 的完成判据**。
4. **用 `metadata["tools.libra.*"]` 承载 Libra 深模型**：标准字段只放 Agent Trace 规定的 `vcs/tool/files/conversations/ranges`；Libra 的 `session_id`、`run_id`、`traces_commit`、`checkpoint_id`、`hash_kind`、签名、confidence 来源等全部放反向域名 vendor metadata，避免污染标准层。
5. **先产出可测试合同，再做命令面**：在实现 `blame --ai` / `usage --by file` 前，先增加 publish/export fixture 与 round-trip 校验，固定 `TraceRecord` 的字段、版本、hash 语义、可信来源枚举和外部声明降级规则。
6. **导入端必须显式建信任模型**：来自 `.agent-trace/traces.jsonl` 或其他 Agent Trace 联盟成员的记录，一律进入独立命名空间，`blame --ai` / `usage` 输出时标注来源与 `trusted` 位；绝不与 Libra 内部 `apply_patch` 产生的权威行区间混合排序。参考实现的三级 fallback 产生的区间置信度应显著低于精确 `compute_replacements` 结果。

### 4.2 对抗验证发现的硬约束（务必先读）

> 以下结论经对实际代码核验，推翻了若干“听起来很美”的直觉做法。照着直觉做会撞墙。

1. **`git-internal` 是外部 pinned crate**（`Cargo.toml:30`：`git-internal = "0.8.6"`，无 `[patch]` 覆盖。2026-08-27 订正：原文写 `0.8.1`、§5 又写 `0.7.4`，三处互相矛盾，统一为 `0.8.6`）。改其 `TouchedFile` / `Provenance` **不是 Libra 仓内 PR**，需发上游版本再升级依赖；且二者都带 `#[serde(deny_unknown_fields)]`，加字段对旧版本读者是**前向不兼容**。
   → **行区间类型必须在 Libra 仓内自定义，不要动 `git-internal`。**
2. **Libra 的 `notes` 不是 git 原生 `refs/notes/*` 树**，而是 SQLite 表（`sql/migrations/2026061401_notes.sql`）+ 对象库里的 blob 哈希。它**不随 push/fetch 传输，外部工具发现不了**；`idx_notes_ref` 只建在 `(notes_ref)` 上。
   → 用 notes 做“跨工具可发现的互操作后端”**不成立**；notes 只适合**本地**存储。真正的互操作只能在 **`publish/ai_export` 边界**导出标准 JSON。
3. ~~**Libra 自己的 `commit` 与 agent-session 没有任何耦合**~~ → **改判（2026-08-27）：落点已存在，但只覆盖 bridge 一条路径**。`plan-20260818`（LB-01..LB-07，**已完成**，protocol v1 20 method 自 v0.21.1 起全部实现，见 `plan-long.md:122`）的 **LB-05 `commit.create`** 在提交后调用 `record_association_links(...)`（`src/internal/ai/agent_bridge/mutations.rs:283-343`，helper 定义 @ `:432`），把 session / actor / workspace / parent lineage / evidence id 以 `agent_bridge_link` 行**按 commit oid 建索引**；schema 见 `sql/migrations/2026081801_agent_bridge_capture.sql` 与 `2026082401_agent_bridge_link_relations.sql`（后者把唯一键由 `(source_type,source_id)` 放宽到全边 `(source_type,source_id,target_type,target_id)`，正是为让一次 commit 挂多条关联）。
   **但仍是缺口**：① 覆盖面仅限 bridge 路径；② `src/command/commit.rs` 本身仍不感知 session、不写 `traces_commit`（grep 为空），`agent_bridge/vcs.rs` 的 `vcs::commit_create` 只是复用 `command::commit::run_commit`。
   → 凡“在 active agent session 内提交时写 trailer/链接”的建议，其前提**不再是「从零建耦合」，而是「把 `agent_bridge_link` 的 commit↔session 边推广到非 bridge 的 `libra commit` 路径」**。现有归属模型的另一半（观察外部 agent、checkpoint 落 `refs/libra/traces` orphan ref、`agent_checkpoint.traces_commit`）不变。
4. **`rebase` / `cherry-pick` 不调用 `compute_diff`**（`compute_diff` 的调用方是 `blame.rs` 与 `log.rs`，rebase/cherry-pick 均 0 命中），重放走树级三向合并。
   → “复用 rebase 已算好的 diff 来重排行区间”**是错的**；但 old→new commit 映射**确实存在**（~~`rebase.rs:566`~~ → `rebase.rs:1137` `summary.applied_commits` / `RebaseAppliedCommitOutput`），故 commit 级 note 重锚可行，行级重排是另一回事（更贵）。（2026-08-27 锚点重定位：`RebaseAppliedCommitOutput` 现 @ `rebase.rs:1137`，字段 `applied_commits` @ `:1123`/`:1152`，消费点 `:1650`/`:1701`；`compute_diff` 在 `rebase.rs`/`cherry_pick.rs` 仍 **0 命中**，结论不变。）
5. ~~**`Vault` 有 `pgp_sign` 但无 `verify`**~~ → **改判（2026-08-27）：已实现**。`src/internal/vault.rs:261` `pub async fn pgp_verify(root_dir, unseal_key, data, signature_hex) -> Result<bool>`，走 `{PKI_MOUNT_PATH}/keys/verify`（`:286`），解析 `{valid: bool}`（兼容 `{result: bool}`，`:292-301`）；模块 doc `:4` 已写「generate PGP keys, sign data, **and verify**」。`pgp_sign` @ `:218`、`signature_to_gpgsig` @ `:658`。
   → 签名与验证**双向齐备**；**剩余净新工作只有密钥分发 / 信任模型**，不再包含 verify 本身（P3-10 的工作量/风险相应下调，见 §4.3 与 §7.4）。
6. **Agent Trace 参考实现不是可直接照搬的合同**。`schemas.ts` 要求 `version` 满足三段 semver，README 示例用 `0.1.0`，但 `reference/trace-store.ts:151` 实际写 `version: "1.0"`；`computeRangePositions:102` 在 hook 未给 range 且字符串回查失败时会退化成 `1..lineCount`（甚至硬 `1..1`）。此外还会写 `.shell-history`、`.sessions` 合成文件。
   → Libra 应该**严格导出（永远用 `0.1.0` 或后续正式三段）、宽容导入（接受 "1.0" / "0.1.0"，对合成路径与全文件 fallback 打低 `trusted`）、显式降级可信度**。参考实现只可作兼容测试样本。
7. **Libra worktree 共享同一个 `.libra/libra.db`**——**结论仍成立，但证据链已作废并替换（2026-08-27）**。
   ~~原证据：`src/command/worktree.rs:671,844` 把每个 worktree 的 `.libra` 建成指向 shared storage 的**符号链接**；`src/utils/path.rs:23` `database()`。~~ 该布局已被 plan-20260714 Part C 取代：`libra worktree add` 现在**创建带 `commondir` 指针的真实 per-worktree gitdir**（`src/command/worktree.rs:2243` doc 明写「it is **NOT** a symlink to shared storage」，各自有 HEAD / index / HEAD reflog 与稳定 `worktree_id`）；symlink 降为 **legacy 布局**（`src/utils/util.rs:848` `is_legacy_symlink_worktree()`，须走 `libra worktree repair --migrate-layout` 迁移）。
   **新证据链**：`storage_path()`（`src/utils/util.rs:823`）经 `worktree_common_storage()`（`src/utils/util.rs:422`）跟随 `commondir` 解析回**公共存储**，`database()`（`src/utils/path.rs:76`，**不是 `:23`**）= `storage_path().join(DATABASE)`。**故物理 DB 仍是单一共享库。**
   → 结论不变：**P1-4 规划的 `ai_edit_trace` 表是跨所有并发 worktree 会话共享的单一物理表**——不是 git worktree 那种各自隔离。任何“apply 时写 NULL `commit_oid`、commit 时回填全部 NULL 行”的简单方案在并发会话下**会串号**（worktree A 的提交回填了 worktree B 的待提交行）。回填**必须按 `session_id`（或 `run_id`）严格 scope**，绝不按“所有 `commit_oid IS NULL`”一把回填。详见 §7.2。
8. **（2026-08-27 新增）LB-05 AC4 已确立「不把归属写进 commit 对象」的既定取舍**。`src/internal/ai/agent_bridge/mutations.rs:283-288` 的 doc 明写：「The commit itself carries **no bridge metadata (LB-05 AC4)**: session, actor, workspace, parent lineage and evidence ids are recorded as `agent_bridge_link` rows keyed by the commit oid, so the association is queryable **without parsing a commit message**.」即 Libra 已在自己的计划里**主动选择 sidecar link 而非 commit message trailer**。
   → **P1-6（`Co-Authored-By` trailer）与该取舍方向相反**，必须以 config `ai.coAuthoredBy` **默认关闭**起步，并在文中显式说明它是对 AC4 的**可选 opt-in 例外**，而非推翻 AC4。这条与 `plan-long.md:524`「不采纳：未冻结的 agent-trace RFC 直接写进默认 commit 元数据」互为呼应（见 §4.3 P1-6 的限定注）。

**利好**：

- `content_hash` 别引 murmur3——**`git-internal::IntegrityHash::compute`** 可用 SHA-256 计算字节哈希，序列化为 `integrity:sha256:<hex>`。**（2026-08-27：原「唯一无法在本仓树内核验」的告警撤销。）** 已对照 registry 源实读核验：`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/git-internal-0.8.6/src/internal/object/integrity.rs` —— `pub fn compute(content: &[u8]) -> Self` @ `:31`，**恒用 SHA-256**（`:18` `use sha2::{Digest, Sha256}`；模块 doc `:9-11` 明写「always uses SHA-256 regardless of the underlying Git repository format」），`to_hex()` @ `:39`，另有 canonical-JSON 变体 `compute_integrity_hash<T: Serialize>` @ `:94`。
  ⚠️ **新增实现约束**：其 `Serialize`（`fn serialize` @ `:75`）输出的是**裸 64 位 hex**，`FromStr`（`fn from_str` @ `:64`）**强制 `s.len() == 64`**。因此本文 §4.7.b / §6 约定的 `integrity:sha256:<hex64>` 前缀**必须在 Libra 导出/导入边界自行拼接与剥离**，不能把带前缀的字符串直接喂给 `IntegrityHash::from_str`（会因长度校验失败）。
- `model_id` 规范化几乎免费——**`ModelBinding::to_canonical_string()`**（`src/internal/ai/agent/profile/spec.rs:129`）已能产出 `provider/model[@variant]`，且 `AgentRunEvent::Spawned`、`agent_usage_stats`、`UsageContext` 都已把 provider 与 model **拆开存**，只差在序列化边界拼接。
- **唯一被低估的资产**：`apply_patch` 已经算好行区间。`src/internal/ai/tools/apply_patch/core.rs` 的 `compute_replacements` 产出精确的 `(start_index, old_len, new_lines)`，handler 还建了 `FileDiff` + unified diff，然后**仅用于 UI projection 显示就丢弃**。这是全系统唯一对“AI 写了哪些行”有完美、无竞态认知的地方。

### 4.3 改进路线图

按“采集 → 存储 → 读取 → 互操作 → 护城河”的依赖链组织。**唯一真正的地基是“行级采集”，而最便宜且最准确的采集点是 Libra 自己的 `apply_patch`，不是 hook，也不是 `git-internal`。**

#### P0 — 地基

| 编号 | 任务 | 触及文件 | 工作量/风险 | 说明 |
|---|---|---|---|---|
| **P0-0** | 固定 Libra 的 Agent Trace 交换合同 | `src/internal/publish/contract.rs`、`src/internal/publish/ai_export.rs`、`tests/data/publish/` | S / 低 | 先定义最小 `TraceRecord` fixture，固定 `version="0.1.0"`（严格导出）、MIME `application/vnd.agent-trace.record+json`、`tools.libra.*` metadata key、`trusted`/`source` 规则、hash-kind 标记和外部声明降级策略（接受 "1.0"/"0.1.0"、合成路径、全文件 fallback 均打低 trusted）。同时放一个来自 Cursor 参考实现的 golden trace（含 "1.0" + fallback range）作为导入 roundtrip 测试输入。 |
| **P0-1** | 在 `apply_patch` 源头采集行区间 | `src/internal/ai/tools/apply_patch/core.rs`、`handlers/apply_patch.rs` | M / 低 | 把 `compute_replacements`（返回 `(start_index, old_len, new_lines)` 1-based 区间）已算出的精确信息从 handler 透出到一个 capture sink（可挂 `RuntimeContext` 或 `UsageContext` 里的 trace collector）。保留现有 `metadata.diffs` 给 UI projection，区间数据走另一条持久化路径。`compute_replacements` / `apply_replacements` 目前私有，需沿调用链暴露或在 `ApplyResult` 上增加 `line_ranges: Vec<EditRange>` 结构。 |
| **P0-2** | 把 model/run/session 身份穿进 `FileHistoryRuntimeContext` | 类型：`src/internal/ai/sandbox/mod.rs:106`（字段挂载 `:73`）；**唯一生产注入点：`src/internal/ai/runtime/execution_control.rs:308`**（`user_task_runtime_context()`）。（2026-08-27 订正：原文写的 `src/internal/ai/web/headless.rs` 仍存在，但已非注入点。**2026-08-27 复核再订正**：`src/internal/ai/agent/runtime/tool_loop.rs:2393` **不是**注入点——它在该文件 `#[cfg(test)] mod tests`（起于 `:1598`）内，构造值是 `session_root: temp_dir.path().join("session-root")` / `batch_id: "turn-7"` 的测试夹具，可作 P0-2 新字段的测试改造样例。全仓 `grep -rn 'FileHistoryRuntimeContext' src/` 共 **6 行**，去掉 2 处 `use` import（`execution_control.rs:20`、`tool_loop.rs:2226`）与本表已单列的字段声明 `sandbox/mod.rs:73` 后，**仅 3 处实体命中**：`sandbox/mod.rs:106` 类型定义、`execution_control.rs:308` 唯一生产构造、`tool_loop.rs:2393` 测试） | M / 低 | 模型在 dispatch 处已知（`ToolLoopConfig.usage_context` 已带全套身份）；`FileHistoryRuntimeContext` 已能到达 apply_patch handler，只差这几个字段。纯内存，无迁移。 |
| **P0-3** | model_id 规范化到 models.dev | `src/internal/ai/agent/profile/spec.rs`、`usage/format.rs`、`publish/ai_export.rs` | S / 低 | 加 `canonical_model_id(provider, model)` helper，**只在序列化/导出边界**用，不动 DB 列。对 `ollama/llama3` 这类非 models.dev 厂商，规范化只是拼接，不保证联盟有效性。 |

#### P1 — 存储 + 读取（用户可见价值）

| 编号 | 任务 | 触及文件 | 工作量/风险 | 说明 |
|---|---|---|---|---|
| **P1-4** | 新建 Libra 本地 trace 存储（SQLite 表，别用 notes 做互操作） | `sql/migrations/<date>_ai_edit_trace.sql`、`src/internal/model/`、`src/internal/db/migration.rs` | L / 中 | 新表 `ai_edit_trace`，apply 时写入（`commit_oid` 留空），commit 时回填。设计抉择：建议 denormalize provider/model 而非 join `agent_usage_stats`（`blame --ai` 每行都要查，额外 join 在高频路径上不划算）。`content_hash` 复用 `IntegrityHash`。DDL 草稿：<br><br>```sql<br>CREATE TABLE IF NOT EXISTS `ai_edit_trace` (<br>    `id`               INTEGER PRIMARY KEY AUTOINCREMENT,<br>    `session_id`       TEXT NOT NULL,<br>    `thread_id`        TEXT,<br>    `run_id`           TEXT NOT NULL,<br>    `provider`         TEXT NOT NULL,<br>    `model`            TEXT NOT NULL,<br>    `file_path`        TEXT NOT NULL,<br>    `start_line`       INTEGER NOT NULL,<br>    `end_line`         INTEGER NOT NULL,<br>    `content_hash`     TEXT,<br>    `commit_oid`       TEXT,  -- NULL until commit backfill<br>    `contributor_type` TEXT NOT NULL DEFAULT 'ai',<br>    `source`           TEXT NOT NULL DEFAULT 'libra_apply_patch',<br>    `trusted`          INTEGER NOT NULL DEFAULT 1,<br>    `created_at`       TEXT NOT NULL<br>);<br>CREATE INDEX IF NOT EXISTS idx_ai_edit_trace_file<br>    ON `ai_edit_trace`(`file_path`, `start_line`, `end_line`);<br>CREATE INDEX IF NOT EXISTS idx_ai_edit_trace_commit<br>    ON `ai_edit_trace`(`commit_oid`);<br>CREATE INDEX IF NOT EXISTS idx_ai_edit_trace_session<br>    ON `ai_edit_trace`(`session_id`);<br>```<br><br>**Crash 恢复**：commit 回填前若 crash，`commit_oid` 残留 NULL。`blame --ai` 查询时需兜底：通过 `run_id → session_id → agent_checkpoint` 反查 `traces_commit` 补填，或按 `(file_path, start_line, end_line)` 匹配最近已知 commit。优先实现反查路径（已有数据、免额外扫描）。<br><br>**并发回填（worktree 共享 DB）**：因 `.libra/libra.db` 跨 worktree 共享（§4.2.7 / §7.2），回填 UPDATE 必须带 `WHERE session_id = ? AND commit_oid IS NULL`，**禁止**裸 `WHERE commit_oid IS NULL`。<br><br>**（2026-08-27 升级）作用域列必须照抄 plan-20260714 Part C **W4** 的五列约定**，不再只是「建议加 `worktree_id`」：`repo_id` / `worktree_id` / `workspace_id` / `workspace_fence` / `scope_state TEXT NOT NULL CHECK(scope_state IN ('legacy_unknown','scoped'))`。证据：`sql/migrations/2026080401_agent_capture_workspace_scope.sql` 已把这套列批量应用到 capture 家族——`agent_export_job`（`:16-22`）与 `agent_import_identity`（`:24-30`）拿到全部五列，`agent_session`（`:9-14`）拿到其中四列（`repo_id`/`workspace_id`/`workspace_fence`/`scope_state`，**未加 `worktree_id`**）；`agent_bridge_session`（`sql/migrations/2026081801_agent_bridge_capture.sql:18-19`）同样带 `worktree_id`/`workspace_id`。理由：`ai_edit_trace` 与 `agent_session` 同属 capture 家族，作用域语义必须一致，否则 `blame --ai` 的过滤与 doctor adoption 路径要写两套。<br><br>**（2026-08-27 新增）不要建第三套关联表**：`ai_edit_trace` 的 commit / operation 关联应优先复用**已落地的 `agent_bridge_link`**（§4.2.3）与 `plan-20260822` 的 **`ai_operation_link`**。**（2026-08-27 复核订正：编号归属拆清，原文笼统挂在 CH-04 名下）** 该表的三段归属是：**建表在 `Task OL-02`**（`plan-20260822.md:759`，v2 schema 替换清单 @ `:765` 列出 `ai_operation_link`）；**字段集由 `ADR-OL-08` 决定**（`:191`：`session_id/run_id/tool_invocation_id/intent_id/repo_id/worktree_id/workspace_id/lease_generation/config_provenance_digest/redaction_version`，通过稳定 ID 关联 AI 对象而非易变 Commit OID）；**写入/查询由 `Task CH-04` 实现**（`:1528`）。→ P1-4 的**建表前置是 OL-02 而非 CH-04**，依赖排序不可只等 CH-04。注意整个 `plan-20260822`（LR-02/LR-03）当前状态是**「已排期、实现未开始」**（`plan-long.md:118,200`），故 P1-4 落地时该表可能尚不存在——需按依赖顺序处理，勿假设可用。<br><br>⚠️ **命名冲突预警**：`trusted` / `source` 这两个字段名会与仓内既有语义撞车——`src/internal/ai/observed_agents/trust.rs` 已占用 `trust` 概念，但语义完全不同（AG-18 的**外部 `libra-agent-*` 二进制信任/隔离记录**：`TrustRecord` @ `:59`、`Provenance` @ `:71`、config key 前缀 `agent.trust.` @ `:32`）。建议改用不冲突的命名，如 `attribution_trust` / `claim_source`。 |
| **P1-5** | `libra blame --ai` | `src/command/blame.rs`、`tests/command/` | M / 低 | spec 把它描述成“blame + trace 两个工具的舞蹈”，Libra 同进程既有 blame 又有 trace 存储，能合成一条命令。`BlameLine`（**2026-08-27 重定位：`src/command/blame.rs:128`**，字段已扩为 `line_number/short_hash/hash/author/**author_email**/date/**timestamp**/content`）加可选 `contributor` / `model_id`（`#[serde(skip_serializing_if = "Option::is_none")]`，JSON 加性兼容）；`--ai` 时按行 join trace。须满足三个 compat guard（`BLAME_EXAMPLES`、`docs/commands/blame.md` Examples、help banner）并更新 `COMPATIBILITY.md`。<br><br>**（2026-08-27 加码）输出形态义务扩大**：`COMPATIBILITY.md:170` 显示 blame 现已支持 `--porcelain`/`-p`、`--line-porcelain`、`-e`、`-l`、`-s`、`-t`、`-f`、`--abbrev`、`--root`、`-w`、`-L` 全套，因此新增 `contributor`/`model_id` **必须同时定义三种输出形态**（human / `--porcelain` / `--line-porcelain`）的呈现，不能只做 JSON 加性字段。<br><br>**⚠️ 与 [`delta.md`](delta.md) §6.2 B1 的收敛待决策**：本卡提的是 `libra blame --ai`（按行给 contributor/model_id，数据来自 `apply_patch` 源头采集 → `ai_edit_trace`）；`delta.md` B1 提的是 `libra blame --provenance`（按行给 session/turn/intent，路径是 PatchSet hunk→turn 映射）。**两者是同一命令表面的两种切法**，落地前需二选一或合并为同一 flag 的两种投影。同时登记 `delta.md` B1 的「AG-ATTR 由 P2 升 P1」提案为待决策（见 §3），本文不单方面改判。 |
| **P1-6** | agent 驱动的主线 commit 自动加 `Co-Authored-By` trailer | `src/command/commit.rs`、`src/internal/ai/history.rs`、`docs/commands/commit.md`、`COMPATIBILITY.md` | S / 低 | 最便宜的 git 原生赢：把归属盖进 commit 对象本身，**随 clone/push 传播、GitHub/git log 直接可读、零 Libra 工具依赖**。复用 `commit.rs::append_trailers()`（@ `src/command/commit.rs:1964`）与 `history.rs::format_libra_trailers`（@ `src/internal/ai/history.rs:6225`）的 trailer 模式。**硬前提（2026-08-27 改判）**：得先让 commit 知道自己“在 agent session 内”——**该耦合的落点已存在但只覆盖 bridge 一条路径**（`agent_bridge_link`，§4.2.3），本卡需把 commit↔session 边**推广到非 bridge 的 `libra commit` 路径**，而不是从零新建。<br><br>**⚠️ 限定注（2026-08-27，对齐计划治理）**：① 本卡写入的是 **Git 原生 `Co-Authored-By`**（当前 `grep -rn "Co-Authored-By" src/` 零命中），**不是 agent-trace RFC 字段**；② 必须以 config **`ai.coAuthoredBy` 默认关闭**起步——这既是对 §4.2.8（LB-05 AC4「commit 对象不携带归属元数据」）的**可选 opt-in 例外**，也是对 `plan-long.md:524`「**不采纳**：未冻结的 agent-trace RFC 直接写进默认 commit 元数据」的遵守；③ 任何把 `TraceRecord` / `tools.libra.*` 字段写进**默认** commit 元数据的做法**已被该「不采纳」条明令拒绝**，本文不得以「最便宜的 git 原生赢」为由绕过。 |
| **P1-7** | `libra log --ai-only / --human-only / --model <id>` | `src/command/log.rs` | M / 低 | trailer 落主线后，这是 `CommitFilter` 上一个纯消息谓词，零 schema 变更，照 `--author/--grep` 的样子加。严格排在 P1-6 之后。 |

#### P2 — 互操作皮肤 + 聚合

| 编号 | 任务 | 触及文件 | 工作量/风险 | 说明 |
|---|---|---|---|---|
| **P2-8** | `usage report --by file` | `src/command/usage.rs`、`src/internal/ai/usage/query.rs` | L / 中 | `usage.rs` **已有 spec 缺的整套聚合/过滤/JSON-CSV 管线**，只差 file/path 维度——P0-1 的区间一旦喂进来即近乎免费。这把 Libra 的数字变成“哪个文件/目录/release 多少比例 AI、哪个模型、随时间趋势”的**权威答案**。 |
| **P2-9** | 在 `publish/ai_export` 边界导出标准 `TraceRecord` | `src/internal/publish/ai_export.rs`、`contract.rs`、`tests/data/publish/` | M / 低 | 这是“加入联盟”的交付物：把内部模型映射成规范 JSON，`vcs:{type:git, revision}` 绑真实 commit OID，把 Libra 更深的 Intent/Plan/Task 本体塞进 `metadata["tools.libra.*"]`，`conversation.url` 指向 `associatedIds.tracesCommit`。导出优先于导入；导出必须用严格三段 semver，不要继承参考实现的 `version: "1.0"`。 |

#### P3 — 护城河（spec 回避、唯有真 VCS 能做）

| 编号 | 任务 | 触及文件 | 工作量/风险 | 说明 |
|---|---|---|---|---|
| **P3-10** | 签名 trace 记录（Vault PGP） | `src/internal/vault.rs`、`publish/contract.rs` | **S–M / 低–中**（2026-08-27 下调） | 复用 `vault::pgp_sign`（@ `:218`），把自声明变成可验证凭据；签名嵌入 `metadata["tools.libra.signature"]`。~~坑：`vault` 没有 verify，验证侧是净新工作~~ → **改判：`pgp_verify` 已实现**（@ `src/internal/vault.rs:261`，见 §4.2.5），签名与验证双向齐备；**剩余净新工作只有密钥分发 / 信任模型**。附带价值：`specs/git_ai_standard_v3.0.0.md` 对 signature 零命中，这是 Libra 相对 git-ai 唯一未被覆盖的真差异（见摘要护城河降调注），值得提前于 P3 其余卡评估。 |
| **P3-11** | rebase/cherry-pick 后重锚 trace | `src/command/rebase.rs`、`src/command/cherry_pick.rs` | XL / 高 | old→new commit 映射已存在，**commit 级 note/链接复制可行**；**行级重排**因 rebase 不算 `compute_diff` 而是净新工作。压到最后，且依赖 P0-1/P1-4。 |
| **P3-12** | merge 归属 | `src/command/merge.rs` | L / 中 | union 双亲的 trace、重叠且 contributor 不同的区间标 `mixed`，把 spec 的歧义变成可复现规则。 |
| **P3-13** | 外部 `.agent-trace/traces.jsonl` 导入 | `src/internal/ai/observed_agents/adapter.rs` | L / 中 | 价值有限（继承 spec 全部弱点：无签名、位置漂移、重叠声明无解）。即便做也只作 `trusted=false` 的独立命名空间，在 `blame --ai` 中标 `(external claim, source=agent-trace-jsonl)`，绝不与 Libra 权威数据混合。必须实现 Cursor 参考实现的全部 fallback 语义 + 合成路径过滤（.shell-history 等应忽略或特殊处理）。更像 `observed_agents` 适配器的扩展而非全新命令。<br><br>**（2026-08-27 治理注）**：本卡（或等价导入器）正是 **AG-ATTR 完成判据**「至少一种外部 transcript/归因格式可导入为只读证据」（`plan-long.md:276`）的承担者——尽管本文正文评其「价值有限」。但**判据不要求格式必须是 agent-trace**：鉴于上游 404 / 停更（§1 告警），候选目标格式还包括 **git-ai Standard v3.0.0**（notes 承载、行级、覆盖历史重写）与 **`letta-ai/trajectory`**（`21ae92d`，多 runtime transcript 归一化）。选型应重估，不必默认锁定 agent-trace。 |

### 4.4 明确不建议 / 易踩坑

- ❌ **别改 `git-internal` 的 `TouchedFile` / `Provenance`**（外部 crate + `deny_unknown_fields` 前向不兼容）→ 行区间类型放 Libra 仓内。
- ❌ **别把 `notes` 当跨工具互操作后端**（SQLite 本地、不随 push 传输）→ notes 只做本地存储，互操作只在 publish 导出。
- ❌ **别假设 Libra `commit` 是 AI 写入路径** → 现状是 observe-external；trailer/链接类功能要先补 commit↔session 耦合。**（2026-08-27 修订）** 该耦合**已在 bridge 路径存在**（LB-05 `commit.create` → `agent_bridge_link`，§4.2.3），但 `src/command/commit.rs` 本身仍不感知 session——所以要补的是**推广到非 bridge 路径**，不是从零新建。
- ❌ **别照搬 Cursor 的 reference hook/store 当 Libra 实现** → 它是示例代码，存在版本形状不一致与区间 fallback，适合作导入兼容样本，不适合作权威采集路径。
- ⚠️ 任何写 OID 的新代码都要走 **hash-kind preflight**（`cli.rs` 读 `core.objectformat`），别硬编码 40-hex。
- ⚠️ `apply_patch` 之外的 observed 外部 agent（Claude Code/Cursor/Codex…）拿不到原生区间，但其**完整转写已被 redact 后存为 blob**（由 `src/internal/ai/hooks/runtime.rs` 写入），可后处理重解析出区间——重解析须**容忍被 redact 的片段**，不能假设逐字。
- ❌ **别假设 `ai_edit_trace` 是 worktree-私有表** → **（2026-08-27 证据更新）** linked worktree 的 `.libra` 已**不是**符号链接，而是带 `commondir` 指针的真实 gitdir（`worktree.rs:2243`；symlink 仅存于 legacy 布局 `util.rs:848`）——但 `storage_path()` 经 `commondir` 解析回公共存储（`util.rs:422`）、`database()` @ `path.rs:76`，**DB 仍跨 worktree 共享**（§7.2）。结论不变：任何按“全表 NULL `commit_oid`”做的回填/清理都会跨会话串号；一切写/回填/GC 都要按 `session_id` scope。
- ⚠️ **（2026-08-27 新增）别复用 `trusted` / `trust` 作字段名** → `src/internal/ai/observed_agents/trust.rs` 已占用该概念但语义完全不同（AG-18 外部 `libra-agent-*` 二进制的信任/隔离记录）。P0-0 / P1-4 的字段建议改名为 `attribution_trust` / `claim_source`。

### 4.5 最小起步序列

> **P0-0（交换合同，S） + P0-3（model_id 规范化，S） + P0-1/P0-2（`apply_patch` 行级采集，M）** 是无依赖的真地基；**P1-6（`Co-Authored-By`，S）** 是最便宜的用户可见 git 原生赢（补上 session 耦合后）。这些落地即可解锁 **P1-5（`blame --ai`）** 与 **P2-8（`usage --by file`）**。先做这几件，Libra 就从“文件级封闭”迈到“行级 + 可向联盟导出”，且每一步都是加性、低风险、可独立交付。

### 4.6 Cursor Agent Trace 方案 vs Libra 方案的本质差异（总结）

| 维度 | Cursor / Agent Trace（hook 世界） | Libra（真 VCS + 运行时） | Libra 应发挥的优势 |
|------|----------------------------------|---------------------------|-------------------|
| 采集时机 | afterFileEdit / PostToolUse hook，**声称**区间 | `apply_patch` 内部 `compute_replacements`，**实际执行前**精确区间 | 权威性高一个数量级；可核对 patch 实际影响 vs 声称 |
| 存储 | `.agent-trace/traces.jsonl` append-only，无索引 | SQLite `ai_edit_trace`（规划）+ `agent_checkpoint` on orphan ref + 对象库 | 可查询、可 join usage、支持 blame 同进程合成 |
| 持久性 | 工作树文件，易丢、易被用户手改 | 受 `libra commit` / checkpoint 保护，进入对象存储与 publish 流程 | 历史可验证 |
| 重写处理 | 未定义（rebase 后 line 静默漂移） | rebase/cherry-pick 有 old→new commit 映射；未来可做范围重排 | 解决 spec 最大痛点之一——但**不再是 Libra 独有**：git-ai Standard v3.0.0 §2（`:420-881`）已规范化 rebase/merge/reset/cherry-pick/stash/amend 下的归属 MUST 行为（2026-08-27） |
| 可信度 | 自声明 | 可签名（Vault PGP，**签验双向齐备**：`pgp_sign` @ `vault.rs:218` / `pgp_verify` @ `:261`）+ source 枚举 + trusted 位 | 合规/审计场景的差异化卖点；**相对 git-ai 亦是唯一未被覆盖的真差异**（其规范对 signature 零命中） |
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

> **锚点复核说明（2026-08-27）**：下表全部行号已对 `main` HEAD `89081277a` 重新核验。漂移的锚点已就地替换；改判的主张（Vault verify、`HookTarget::AgentTraces`、commit↔session 耦合、worktree symlink）已在对应行标注。逐条对照见 §9。

| 主张 | 锚点 |
|---|---|
| hook 把 `tool_input` 塌成布尔丢弃区间 | `src/internal/ai/hooks/runtime.rs`（**生产路径** `append_normalized_event_with_envelope` @ `:3110`，调用点 @ `:3043`，`has_tool_input` @ `:3147`；`append_normalized_event` @ `:3097` 带 `#[cfg(test)]` @ `:3096`，仅为转调前者的测试包装——2026-08-27 复核订正）；`tool_input` 已在更前处 redact |
| `apply_patch` 已算出精确区间却仅供 UI projection | `src/internal/ai/tools/apply_patch/core.rs`（`compute_replacements`）；`handlers/apply_patch.rs`（`FileDiff` + unified diff） |
| 全套归属身份已在 dispatch 处在场 | `src/internal/ai/usage/recorder.rs`（`UsageContext`）；`agent/runtime/tool_loop.rs`（`ToolLoopConfig`） |
| model 已拆 provider/model，且有规范化器 | `agent/profile/spec.rs:129`（`ModelBinding::to_canonical_string`）；`agent_run/event.rs:261`（`Spawned`，2026-08-27 由 `:267` 重定位）；`agent_usage_stats` |
| `TouchedFile` 仅文件级计数，无区间 | `git-internal 0.8.6`（外部 crate）`object/patchset.rs:61-70`（`path/change_type/lines_added/lines_deleted`，`deny_unknown_fields` @ `:60`）；Libra 侧 `TouchedFile` **真实使用点**为 `src/internal/ai/mcp/resource.rs:37`（import）与 `:2380`（`TouchedFile::new`）；`mcp/resource.rs:1136` 与 `src/internal/ai/orchestrator/persistence.rs:4584-4615` 是仓内**镜像参数结构 `TouchedFileParams`**，非外部类型使用点（2026-08-27 复核订正） |
| `blame` 零 AI 维度 | `src/command/blame.rs:128`（`BlameLine`：`line_number/short_hash/hash/author/**author_email**/date/**timestamp**/content`，2026-08-27 由 `:72` 重定位并补齐两个新字段）；`--ai`/`contributor`/`model_id` 仍全部 grep 零命中；`compute_diff` 的调用方为 `blame.rs:429,431` + `log.rs:2034`（rebase/cherry-pick 0 命中） |
| `usage` 有聚合管线但无 file 维度 | `src/command/usage.rs:97`（`UsageReportBy`，变体仍为 `Model`/`Agent`/`AgentProviderModel` @ `:98-104`；2026-08-27 由 `:94` 重定位）；`usage/query.rs` |
| `notes` 为 SQLite + blob 哈希，非 git-wire ref | `src/internal/notes.rs`；`sql/migrations/2026061401_notes.sql`（`idx_notes_ref` 仅 `(notes_ref)`） |
| commit 不写 agent 链接；归属在 orphan ref | `src/command/commit.rs`（仍无 `traces_commit`／session 写入，2026-08-27 复核仍为空）；`history.rs`（`format_libra_trailers` @ `:6225`、`refs/libra/traces`）。**⚠️ 部分改判**：bridge 路径已有 commit↔session 关联（`agent_bridge/mutations.rs:283-343` 写 `agent_bridge_link`，§4.2.3） |
| rebase old→new 映射存在；但不算行级 diff | `src/command/rebase.rs:1137`（`RebaseAppliedCommitOutput`；`applied_commits` @ `:1123`/`:1152`，消费点 `:1650`/`:1701`；2026-08-27 由 `:566` 重定位）；`compute_diff` 在 rebase/cherry-pick 仍 0 命中 |
| ~~Vault 能签不能验~~ → **Vault 可签**可验**（2026-08-27 改判）** | `src/internal/vault.rs`：`pgp_sign` @ `:218`、`pgp_verify` @ `:261`（`{PKI_MOUNT_PATH}/keys/verify` @ `:286`，解析 `{valid}` @ `:292-301`）、`signature_to_gpgsig` @ `:658`；模块 doc `:4` 已含 "and verify" |
| ai_export 为对象级，无 files/ranges | `src/internal/publish/ai_export.rs`（Intent/Plan/Task/Run/PatchSet/Provenance；`ranges`/`start_line` 仍 grep 零命中）；`associatedIds.tracesCommit` |
| Agent Trace reference 版本与区间 fallback 不可照搬 | `/Volumes/Data/competition/cursor/agent-trace/schemas.ts:89`（`version` regex，`.describe()` 在 `:90`）；`reference/trace-store.ts:151`（`version: "1.0"`）、`:102`（`computeRangePositions` 三级回退 + 合成路径） |
| `IntegrityHash::compute` 可用 | `git-internal-0.8.6/src/internal/object/integrity.rs:31`（2026-08-27 订正：原写 `0.7.4`；已对照 registry 源实读——恒 SHA-256，`to_hex` @ `:39`，`from_str` 强制 64 长度 @ `:64`，`serialize` 输出裸 hex @ `:75`，canonical-JSON 变体 @ `:94`） |
| Cursor hook 采集 vs Libra apply_patch | `reference/trace-hook.ts:94`（PostToolUse 走 tool_input 回查）；Libra `apply_patch/handlers/apply_patch.rs:144`（只转 unified diff 给 UI projection，区间丢弃。2026-08-27 订正：原文此处写 `:145` 而 §7.5 写 `:144`，实际注释在 `:144`，两处统一） |
| normalized event 丢区间 | `hooks/runtime.rs:3147`（`"has_tool_input": event.tool_input.is_some()`，原始内容已在 redaction 层前丢弃；2026-08-27 由 `:1118` 重定位） |
| ~~`HookTarget::AgentTraces` 为 Phase-1 stub~~ → **已实现（2026-08-27 改判）** | `src/internal/ai/hooks/runtime.rs:82-86` doc 明写 "**Fully wired**"；分派 @ `:196` `return ingest_agent_traces(...)`；ingest 链 `ingest_agent_traces` @ `:436` → `ingest_agent_traces_payload` @ `:496` → `..._with_scope` @ `:520`（写 `agent_session`，`SessionEnd` 写 `refs/libra/traces` checkpoint）；测试 @ `:3751` |
| worktree 布局已变，但 DB 仍共享 | `src/command/worktree.rs:2243`（真实 per-worktree gitdir + `commondir`，"**NOT** a symlink"）；legacy symlink 判定 `src/utils/util.rs:848`；`storage_path()` @ `util.rs:823` → `worktree_common_storage()` @ `util.rs:422`；`database()` @ `src/utils/path.rs:76`（2026-08-27 由 `path.rs:23` 重定位） |

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
    "version": "0.21.27"
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
- `content_hash` 使用 `integrity:sha256:<hex>`，由 `git-internal::IntegrityHash::compute` 计算，不采用 spec 示例中的 `murmur3`。注意它**不是** git object hash——它是被修改行的内容 hash，用于跨重写位置追踪。**（2026-08-27）** `integrity:sha256:` 前缀必须由 Libra 的导出/导入边界自行拼接与剥离：`IntegrityHash` 的 `Serialize` 输出裸 64 位 hex、`FromStr` 强制长度 64，带前缀字符串直接解析会失败（见 §4.2「利好」）。
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
                          │   前提：commit↔session 耦合（bridge 路径已有，需推广到       │
                          │        非 bridge 的 libra commit，§4.2.3；2026-08-27 改判）│
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

**事实链（2026-08-27 已按 plan-20260714 Part C 之后的布局重新核验；结论不变，证据全部替换）**：

1. ~~`libra worktree add` 把新 worktree 的 `.libra` 建成指向 shared storage 的符号链接（`worktree.rs:671`/`:844`）~~ → **已作废**。`libra worktree add` 现在创建**带 `commondir` 指针的真实 per-worktree gitdir**（`src/command/worktree.rs:2243`：「creates a real per-worktree `.libra` gitdir that records a `commondir` pointer to the shared object store and a stable `worktree_id` — **it is NOT a symlink to shared storage**」）。符号链接只存在于 **legacy 布局**（`src/utils/util.rs:848` `is_legacy_symlink_worktree()`；须 `libra worktree repair --migrate-layout` 迁移）。
2. 但路径解析**仍然收敛到同一 storage**：`storage_path()`（`src/utils/util.rs:823`）经 `worktree_common_storage()`（`src/utils/util.rs:422`）跟随 `commondir` 解析回公共存储；`database()`（`src/utils/path.rs:76`，**不是 `:23`**）= `storage_path().join(util::DATABASE)`。
3. 因此**所有 worktree 仍共享同一个 `.libra/libra.db`**。HEAD/index/HEAD reflog 自 plan-20260714 Part C 起已按 `worktree_id` 隔离（现在是物理隔离的 per-worktree gitdir，不只是逻辑列），与 git worktree 的"各自独立 HEAD/index"语义一致；**共享的只剩物理数据库文件本身**。

**推论（对 P1-4 的修正）**：

- `ai_edit_trace` 是一张**跨所有并发 agent 会话的单一物理表**，不是 per-worktree。
- 朴素回填（commit 时 `UPDATE … WHERE commit_oid IS NULL`）在两个 worktree/会话并发时**会把 A 的提交盖到 B 的待提交行上**。→ 回填 **MUST** `WHERE session_id = ? AND commit_oid IS NULL`。
- ~~建议表加 `worktree_id` 列~~ → **（2026-08-27 升级）该建议已被现实超越：必须照抄 plan-20260714 Part C **W4** 的五列作用域约定**——`repo_id` / `worktree_id` / `workspace_id` / `workspace_fence` / `scope_state CHECK IN ('legacy_unknown','scoped')`。W4 已把这套列批量应用到 capture 家族：`sql/migrations/2026080401_agent_capture_workspace_scope.sql` 给 `agent_export_job`（`:16-22`）与 `agent_import_identity`（`:24-30`）加满五列，给 `agent_session`（`:9-14`）加四列（**未含 `worktree_id`**）；`agent_bridge_session` 亦带 `worktree_id`/`workspace_id`（`sql/migrations/2026081801_agent_bridge_capture.sql:18-19`）。理由：`ai_edit_trace` 与 `agent_session` 同属 capture 家族，作用域语义必须一致，否则 `blame --ai` 的过滤与 doctor adoption 路径要写两套。这些列同时满足原三项用途：① 审计"哪个 worktree 产生的归属"；② `blame --ai` 在共享库里按 worktree 过滤；③ GC 时安全清理某个已删除 worktree 的孤儿行。
- 写入争用：`db.rs` 默认 30s `busy_timeout` 兜底 SQLite 串行写；但**高频 apply_patch 写入 × 多并发 agent** 可能拉长尾延迟。建议 capture sink 做**会话内批量 flush**（如每 N 次 apply 或 commit 前一次性 INSERT），而非每次 apply 单条事务。
- crash 恢复（§P1-4 已述）同样要叠加 session scope：反查 `run_id → session_id → agent_checkpoint.traces_commit` 补填时，只补本会话的 NULL 行。

> 一句话：**Libra 的 worktree 在 DB 层没有隔离，归属表必须自己用 `session_id` + W4 五列重建隔离**，不能依赖文件系统层面的 per-worktree DB。（2026-08-27 措辞订正：Part C 之后 HEAD/index 已是物理隔离的 per-worktree gitdir，"隔离模型比 git 弱"只对**数据库**成立，不再对 HEAD/index 成立。）

### 7.3 互操作生态定位：为什么"只能在 publish 边界互操作"是结构性结论

Agent Trace 联盟成员（README 致谢）大致分两类采集/存储形态，理解它们能精确定位 Libra 该在哪一层接：

| 形态 | 代表 | 存储/传播 | 与 Libra 的接点 |
|---|---|---|---|
| **编辑器/Agent hook → 本地 JSONL** | Cursor、Cline、Amp、OpenCode、Jules | `.agent-trace/*.jsonl` 工作树文件，**不随 VCS 传播** | 只能作 P3-13 低信任**导入**样本 |
| **VCS-native 旁路（git notes 等）** | **git-ai**（`refs/notes/ai`；规范 `specs/git_ai_standard_v3.0.0.md`；本地 checkout `/Volumes/Data/competition/git-ai-project/git-ai@793066013`）。2026-08-27 订正：原文写"（`refs/notes/*` 思路）"低估了成熟度——它不是思路而是**已实现 + 已发布规范 + 已宣称 Fortune 100 生产使用**（`README.md:206`），namespace 是具体的 `refs/notes/ai`，且规范明令**不得**用 `refs/notes/commits`（`:23-24`） | git 原生 notes，**随 push/fetch 同步**（`README.md:250` 明确"Attribution notes synced to/from the remote"），外部可发现 | Libra 学不来：Libra 的 `notes` 是 SQLite + blob，**不过线**（§4.2.2 已证 `idx_notes_ref` 仅本地） |
| **平台/分析后端** | Vercel、Cloudflare、Amplitude、Cognition、Tapes | 各自云端 | 消费 Libra **导出**的标准 JSON |

**结论再加固**：Libra 既不能靠工作树 JSONL（易丢、不过线），也不能靠自己的 notes（SQLite 本地、不过线）。**唯一既"过线/可被联盟发现"又"受 VCS 保护"的出口，就是 `publish/ai_export` 把内部权威模型 emit 成标准记录（P2-9）**。这不是偏好，是 Libra 现有存储形态决定的——publish 是 Libra 体系里唯一已建成的"对外可发现内容"边界。

> **（2026-08-27 新增）互操作格式不止 agent-trace 一家**。鉴于 agent-trace 上游 404 且自 2026-02-06 停更（§1 告警），"至少一种外部格式"的候选目标至少有三个：
>
> | 候选格式 | 本地事实源 | 形态 | 相对 agent-trace 的优势 |
> |---|---|---|---|
> | **Agent Trace v0.1.0** | `/Volumes/Data/competition/cursor/agent-trace@2754f07` | 工作树 JSONL，文件+行区间 | 联盟名单最广；但**已停更/远端 404** |
> | **git-ai Standard v3.0.0** | `/Volumes/Data/competition/git-ai-project/git-ai@793066013` | git notes（`refs/notes/ai`）承载、行级、**已覆盖历史重写语义**（§2 `:420-881`） | 活跃维护、随 push/fetch 过线、有可执行参考实现 |
> | **`letta-ai/trajectory`** | `/Volumes/Data/competition/letta-ai/trajectory@21ae92d` | 多 runtime **transcript 归一化**（TypeScript 包 `@letta-ai/trajectory`） | 覆盖 transcript 侧而非行区间侧，正对 AG-ATTR 判据里的 "transcript" 半边 |
>
> 三者同时被 `plan-long.md:267` 并列为 AG-ATTR 的证据来源。**`AG-ATTR` 的「至少一种外部格式」完成判据不必绑定在已停更的 agent-trace 上**——选型应在 P3-13 立卡时重估。

> 注：若未来确有"wire-native 归属旁路"需求（即不经 publish、随 push 直接带归属），Libra 现有形态下最干净的路径是 §P1-6 的 **commit trailer**（`Co-Authored-By` / `Libra-*`）——trailer 在 commit 对象里，天然随 clone/push 传播、GitHub/git log 直接可读。这也是为什么 P1-6 被列为"最便宜的 git 原生赢"。
>
> **⚠️ 边界（2026-08-27 新增，务必与 P1-6 的限定注一并读）**："干净"仅限于 **Git 原生 `Co-Authored-By`**，且必须 config `ai.coAuthoredBy` **默认关闭**。① `plan-long.md:524` 已把「**未冻结的 agent-trace RFC 直接写进默认 commit 元数据**」登记为**不采纳**（不进入长期优先队列）；② Libra 自己的 LB-05 AC4（§4.2.8）已确立"commit 对象不携带归属元数据、关联走 sidecar link"的既定取舍。因此本注不得被读成"把 `TraceRecord` / `tools.libra.*` 塞进 commit 是可行方案"——那条路径已被明令拒绝。原文"唯一干净路径"的措辞已改为"最干净的路径"，以免与上述两条决策相抵。

### 7.4 统一 CLI 面 delta 与 compat-guard 义务（落地清单）

把散落在 P0–P3 的用户可见表面集中成一张表，并对齐 CLAUDE.md 的**三道 compat guard + COMPATIBILITY.md + error-codes** 纪律——任何新命令/新标志落地前都要逐项过：

| 任务 | 新增表面 | 类型 | compat-guard / 文档义务 |
|---|---|---|---|
| P1-5 | `libra blame --ai`（`BlameLine` @ `blame.rs:128` 加可选 `contributor`/`model_id`，`skip_serializing_if=None` 加性兼容） | 新标志 | `BLAME_EXAMPLES` banner、`docs/commands/blame.md` Examples 段、help-examples-banner guard；`COMPATIBILITY.md` blame 行（`:170`）。**（2026-08-27 加码）** blame 已支持 `--porcelain`/`-p`/`--line-porcelain`/`-e`/`-l`/`-s`/`-t`/`-f`/`--abbrev`/`--root`/`-w`/`-L` 全套，故必须同时定义 **human / `--porcelain` / `--line-porcelain` 三种输出形态**的呈现，不只是 JSON 加性字段。**并须与 [`delta.md`](delta.md) §6.2 B1 的 `blame --provenance` 收敛为同一命令表面**（见 §4.3 P1-5 注） |
| P1-6 | `libra commit` 自动 `Co-Authored-By`（**必须**以 config `ai.coAuthoredBy` **默认关闭**起步） | 行为变更 + config | `docs/commands/commit.md`、`COMPATIBILITY.md` commit 行；config 键文档。**（2026-08-27）** 须在文档中写明它是对 §4.2.8（LB-05 AC4）的 opt-in 例外，且**不触碰** `plan-long.md:524` 的「不采纳」条 |
| P1-7 | `libra log --ai-only / --human-only / --model <id>` | 新标志 | `LOG_EXAMPLES`、`docs/commands/log.md` Examples、help banner；`COMPATIBILITY.md` log 行 |
| P2-8 | `libra usage report --by file`（`UsageReportBy` @ `usage.rs:97` 加 `File`/`Path` 变体；现仅 `Model`/`Agent`/`AgentProviderModel` @ `:98-104`） | 新枚举值 | `usage` JSON/CSV fixture；`docs/commands/usage.md`；`COMPATIBILITY.md` usage 行 |
| P2-9 | `libra publish` 导出 `TraceRecord`（或 `libra export --agent-trace`） | 新导出 | publish round-trip fixture `tests/data/publish/`；MIME `application/vnd.agent-trace.record+json` |
| P3-10 | trace 签名（`metadata["tools.libra.signature"]`） | 内部+导出 | 签名 fixture。~~注意 Vault **无 verify**（净新工作）~~ → **（2026-08-27 改判）Vault `pgp_verify` 已实现**（`vault.rs:261`），验签 fixture 可直接用真实 verify 路径；剩余净新工作只有密钥分发 / 信任模型 |
| P3-13 | 外部 `.agent-trace` 导入（observed_agents 适配器扩展，非新顶层命令） | 导入 | golden 兼容样本（含 `"1.0"` + fallback range + `.shell-history` 合成路径） |

**新增 `StableErrorCode` 提醒**：若 P1-4/P3-13 引入新错误码（如导入畸形 trace、签名验证失败），必须同步 `docs/error-codes.md`（否则 `compat_error_codes_doc_sync` guard 红）。

**新增 SQLite 表/迁移提醒**：`ai_edit_trace` 迁移文件按 `YYYYMMDDNN_ai_edit_trace.sql` 命名、排在**现最新 `2026082401_agent_bridge_link_relations.sql`** 之后（2026-08-27 订正：原文写 `2026061401_notes.sql`，早已不是最新）；forward DDL 幂等（`CREATE TABLE IF NOT EXISTS`）、配 `_down.sql`，并加 migration apply/revert 测试（CLAUDE.md `sql/migrations/README.md` 约定）。列集须含 W4 五列作用域约定（§7.2）。

### 7.5 锚点二次核验结论

> **本节保留 2026-06-18 第二轮的原始结论以存档；2026-08-27 第一次周期刷新的复核结果以本节末尾的「三次核验增补」为准（有冲突处以增补为准）。**

本轮对 §5 全部锚点 + 新增主张逐条复核，结果：**14/14 内部代码主张对当前树准确**。修正/精化两条：

- ✏️ `compute_diff` 的调用方是 **`blame.rs` + `log.rs`**（原文 §5 写"仅此处用"已更正）；rebase/cherry-pick 仍 0 命中（结论不变）。
- ⚠️ `git-internal::IntegrityHash::compute` 位于**外部 pinned crate**，本仓树内 grep 不到，是唯一**未能在树内自证**的锚点——见 §4.2"利好"已加的告警；落地前对照 vendored crate 源确认。

精化的精确锚点（供实现直接跳转）——⚠️ **以下行号为 2026-06-18 存档值，多数已漂移；实现请直接用下方「三次核验增补」的重定位表，勿按本段跳转**：`BlameLine` @ `blame.rs:72`（`line_number/short_hash/hash/author/date/content`）；`UsageReportBy` @ `usage.rs:94`（`Model`/`Agent`/`AgentProviderModel`）；`compute_replacements` @ `apply_patch/core.rs:285`（私有，兄弟 `apply_replacements` @ `:585` 亦私有）；UI-only diff 注释 @ `handlers/apply_patch.rs:144`；`append_normalized_event` @ `hooks/runtime.rs:1100`（`has_tool_input` @ `:1118`）；`HookTarget::AgentTraces` Phase-1 stub @ `hooks/runtime.rs:76-83`（运行时 reject "not yet wired"）；`traces_commit` @ `publish/contract.rs:384`；`TRACES_BRANCH` @ `src/internal/branch.rs:42`；`ModelBinding::to_canonical_string` @ `spec.rs:129`；`vault::pgp_sign` @ `:218` / `signature_to_gpgsig` @ `:658`（无 verify）；`RebaseAppliedCommitOutput` @ `rebase.rs:566`。

#### 三次核验增补（2026-08-27，对 `main` HEAD `89081277a`）

**两条结论级改判**（原文上述两条告警均已作废）：

- ✅ `git-internal::IntegrityHash::compute` **已可核验**，"唯一未能在树内自证的锚点"告警**撤销**——源在 `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/git-internal-0.8.6/src/internal/object/integrity.rs`，`compute` @ `:31`，恒 SHA-256。同时订正版本号：依赖是 **`git-internal = "0.8.6"`**（`Cargo.toml:30`），原文 §4.2.1 写 `0.8.1`、§5 写 `0.7.4`，均已统一。
- ✅ `HookTarget::AgentTraces` **不再是 Phase-1 stub**——`hooks/runtime.rs:82-86` 已标 "Fully wired"，`:196` 分派到 `ingest_agent_traces`，`SessionEnd` 写 `refs/libra/traces` checkpoint，测试 @ `:3751`。
- ✅ `vault` **已有 verify**——`pgp_verify` @ `src/internal/vault.rs:261`（详见 §4.2.5）。

**锚点行号重定位**（结论均不变，仅位置漂移）：

| 锚点 | 原（2026-06-18） | 现（2026-08-27） |
|---|---|---|
| `BlameLine` | `blame.rs:72` | **`blame.rs:128`**（字段新增 `author_email`、`timestamp`） |
| `UsageReportBy` | `usage.rs:94` | **`usage.rs:97`**（变体不变，@ `:98-104`） |
| `append_normalized_event` | `hooks/runtime.rs:1100` | **`:3097`**，但该函数现带 `#[cfg(test)]`（@ `:3096`）；**生产塌陷发生在 `append_normalized_event_with_envelope` @ `:3110`**（调用点 @ `:3043`，2026-08-27 复核订正） |
| `has_tool_input` | `hooks/runtime.rs:1118` | **`:3147`** |
| `RebaseAppliedCommitOutput` | `rebase.rs:566` | **`rebase.rs:1137`**（`applied_commits` @ `:1123`/`:1152`） |
| `AgentRunEvent::Spawned` | `agent_run/event.rs:267` | **`:261`** |
| `database()` | `src/utils/path.rs:23` | **`src/utils/path.rs:76`** |
| UI-only diff 注释 | §5 写 `:145` / §7.5 写 `:144`（自相矛盾） | **统一为 `handlers/apply_patch.rs:144`** |
| `TouchedFile`（Libra 侧使用点） | `agent_run/patchset.rs` | **`mcp/resource.rs:37`（import）+ `:2380`（`TouchedFile::new`）**（`agent_run/patchset.rs` 零命中）；`mcp/resource.rs:1136` 与 `orchestrator/persistence.rs:4584-4615` 属仓内镜像结构 **`TouchedFileParams`**，非外部类型使用点（2026-08-27 复核订正） |
| `FileHistoryRuntimeContext` 注入点 | `web/headless.rs` | **唯一生产注入点 `runtime/execution_control.rs:308`**（类型仍在 `sandbox/mod.rs:106`，字段挂载 `:73`；`agent/runtime/tool_loop.rs:2393` 位于 `#[cfg(test)] mod tests`（起 `:1598`）内，是测试夹具**不是注入点**，2026-08-27 复核订正） |

**复核为未漂移、仍准确**：`compute_replacements` @ `apply_patch/core.rs:285` 与 `apply_replacements` @ `:585`（均仍私有）；`ModelBinding::to_canonical_string` @ `spec.rs:129`；`traces_commit` @ `publish/contract.rs:384`；`TRACES_BRANCH` @ `src/internal/branch.rs:42`；`vault::pgp_sign` @ `:218` / `signature_to_gpgsig` @ `:658`；`format_libra_trailers` @ `history.rs:6225`；`append_trailers` @ `commit.rs:1964`；`idx_notes_ref` 仅建在 `(notes_ref)`（`sql/migrations/2026061401_notes.sql:9`）；`FileHistoryEntry { path, existed, snapshot, size }` @ `session/file_history.rs:81`（无任何 line 字段）；`ai_export.rs` 的 `ranges`/`start_line` 零命中；`compute_diff` 调用方仍仅 `blame.rs:429,431` + `log.rs:2034`，rebase/cherry-pick 仍 0 命中。

---

## 8. 参考

- Agent Trace 规范与参考实现（Cursor）：`/Volumes/Data/competition/cursor/agent-trace`（HEAD `2754f07`，2026-02-06；`README.md`、`schemas.ts`、`reference/{trace-store,trace-hook}.ts`、`index.ts` 用于构建 JSON Schema 与站点）。⚠️ 上游远端 404 / `blocked-network`，见 §1 告警。
- 相邻竞品（AG-ATTR 的其他候选互操作格式，见 §7.3）：`/Volumes/Data/competition/git-ai-project/git-ai`（`793066013`，含 `specs/git_ai_standard_v3.0.0.md`）、`/Volumes/Data/competition/letta-ai/trajectory`（`21ae92d`）。
- 计划治理：[`plan-long.md`](../plan/plan-long.md)（**AG-ATTR** @ `:267`，完成判据 @ `:276`，不采纳条 @ `:524`，竞品对照 @ `:253`，本仓 revision 快照 @ `:64`）；[`plan-20260818.md`](../plan/plan-20260818.md)（LB-05，已完成）；[`plan-20260822.md`](../plan/plan-20260822.md)（`ai_operation_link`：建表 `Task OL-02` @ `:759`（清单 @ `:765`）、字段集 `ADR-OL-08` @ `:191`、写入/查询 `Task CH-04` @ `:1528`；整体已排期未开始）。
- 同目录相邻 gap 文档：[`delta.md`](delta.md)（§6.2 **B1** 建议 AG-ATTR 升 P1 + `blame --provenance`，与本文 P1-5 需收敛）。
- Libra AI 对象模型：[`docs/ai/object-model-reference.md`](../ai/object-model-reference.md)、[`docs/development/code-agent-runtime.md`](code-agent-runtime.md)。
- 兼容性矩阵：[`COMPATIBILITY.md`](../../COMPATIBILITY.md)（blame 行 @ `:170`）。
- 关键实现锚点（2026-08-27 已重定位，详见 §7.5 三次核验增补）：`src/internal/ai/tools/apply_patch/core.rs:285`（compute_replacements）、`src/internal/ai/tools/handlers/apply_patch.rs:144`（仅产 UI metadata）、`src/internal/ai/hooks/runtime.rs:3110`（`append_normalized_event_with_envelope` 塌陷，`has_tool_input` @ `:3147`）、`src/internal/ai/observed_agents/`（外部采集 + redaction + traces orphan ref）、`src/internal/publish/ai_export.rs`（对象级导出）。

---

## 9. 刷新记录

> 本节按轮次**追加**，不覆盖旧记录。§7（2026-06-18 第二轮）是本文既有的「一轮核验 = 一个编号节」体例先例；本节沿用该体例，并保持既有节号不变（新节追加在 §8 之后，不重编号）。

### 2026-08-27（第一次周期刷新）

**竞品侧**：参照 `cursor/agent-trace`，本地 checkout `/Volumes/Data/competition/cursor/agent-trace`，HEAD `2754f07`（`2754f077f3e50c1fb5088183f5c9362077cc8ca1`，最后提交 2026-02-06 "Update abstract"）。

**竞品漂移量：零提交**——自本文成文（2026-06-18）以来本地无任何变化。§1 / §1.2 / §1.3 / §1.4 / §4.7.a 的全部竞品事实断言逐条复核准确：`schemas.ts:89` 的三段 semver regex、`reference/trace-store.ts:102` `computeRangePositions` 的三级回退（`edit.range` → `indexOf` → `{1, lineCount}`）与 `:151` 写 `version: "1.0"`、`reference/trace-hook.ts:94` `PostToolUse` 对 `Write`/`Edit` 同路径、README `**Version**: 0.1.0`（`:3`）与 CC BY 4.0（`:537`）及联盟名单。**唯一修正**：§1.4.c 原写 `.describe()` 在「同行」，实际在紧接的 `schemas.ts:90`。

⚠️ **上游状态告警（新增，见 §1 开头）**：`plan-long.md:64,112` 记该仓库为 `blocked-network`——远端 `cursor/agent-trace` `git fetch` 仍 404（已删除或迁移）。本地 revision **未证明是远端最新，规范可能已停更或迁移**。本文一切「对齐该规范」的收益判断均按此折价；`AG-ATTR` 的完成判据不应绑定在本规范上。

**Libra 侧基线**：`main` HEAD `89081277a`，`Cargo.toml` `0.21.27`（成文时为 0.17.x 时代）。

**本次修正要点**：

1. **失效路径 2 处清理**（§5 锚点表、§8 参考）：`/Volumes/Data/cursor/agent-trace` → `/Volumes/Data/competition/cursor/agent-trace`（依据：`ls /Volumes/Data/cursor` → No such file or directory）。
2. **§4.2.5 / §4.6 / §5 / §7.4 / §7.5 改判「Vault 无 verify」为已实现**：`src/internal/vault.rs:261` `pgp_verify` 已落地（`keys/verify` @ `:286`，解析 `{valid}` @ `:292-301`，模块 doc `:4` 含 "and verify"）。P3-10 工作量由 `M / 中` 下调为 `S–M / 低–中`，剩余净新工作仅密钥分发 / 信任模型。
3. **§2 / §7.5 改判「`HookTarget::AgentTraces` Phase-1 stub」为已实现**：`hooks/runtime.rs:82-86` 标 "Fully wired"，`:196` 分派到 `ingest_agent_traces`（`:436` → `:496` → `:520`），`SessionEnd` 写 `refs/libra/traces` checkpoint，测试 @ `:3751`。同行顺带更新 `observed_agents/` 的文件盘点（3 → 15 个 `.rs`），并确认**仍成立的缺口只剩行区间与 model_id 规范化到 conversation 级**（`to_canonical_string` 在该目录零命中）。
4. **§4.2.3 / §4.4 / §7.1 改判「commit↔session 无耦合」为「落点已存在但只覆盖 bridge 一条路径」**：`plan-20260818`（**已完成**，`plan-long.md:122`）LB-05 的 `commit.create` 已按 commit oid 写 `agent_bridge_link` 关联（`src/internal/ai/agent_bridge/mutations.rs:283-343`，helper @ `:432`；schema `2026081801_agent_bridge_capture.sql` + `2026082401_agent_bridge_link_relations.sql`）；`src/command/commit.rs` 本身仍不感知 session。P1-6 的硬前提相应由「需先建耦合」改为「需推广到非 bridge 路径」。
5. **新增硬约束 §4.2.8（LB-05 AC4）**：`mutations.rs:283-288` doc 明写 commit 对象**不携带** bridge 归属元数据，关联走 sidecar `agent_bridge_link`——即 Libra 已主动选择 sidecar 而非 commit trailer。P1-6 因此被限定为「config 默认关闭的 opt-in 例外」。
6. **§4.2.7 / §7.2 保留结论、整体更换证据链**：linked worktree 的 `.libra` 已**不是** symlink 而是带 `commondir` 指针的真实 gitdir（`worktree.rs:2243`），symlink 降为 legacy 布局（`util.rs:848`，`worktree repair --migrate-layout` 迁移）；但 `storage_path()`（`util.rs:823`）经 `worktree_common_storage()`（`util.rs:422`）解析、`database()` @ `path.rs:76`，**物理 DB 仍单一共享**，故「回填必须按 `session_id` scope」的结论不变。§7.2 的 `worktree_id` 建议**升级为照抄 plan-20260714 W4 五列约定**（`repo_id`/`worktree_id`/`workspace_id`/`workspace_fence`/`scope_state`，`sql/migrations/2026080401_agent_capture_workspace_scope.sql`）。
7. **`git-internal` 版本三处矛盾统一为 `0.8.6`**（`Cargo.toml:30`；原文 §4.2.1 写 `0.8.1`、§5 写 `0.7.4`）。`IntegrityHash::compute` **已对照 registry 源实读核验**（`git-internal-0.8.6/.../integrity.rs:31`，恒 SHA-256），**「唯一无法在树内核验的锚点」告警撤销**；新增实现约束：其 `serialize` @ `:75` 输出裸 64-hex、`from_str` @ `:64` 强制长度 64，故 `integrity:sha256:` 前缀必须由 Libra 导出/导入边界自行拼接与剥离。
8. **锚点行号批量重定位**（明细表见 §7.5「三次核验增补」）：`BlameLine` `:72`→`:128`（并新增 `author_email`/`timestamp`）、`UsageReportBy` `:94`→`:97`、`append_normalized_event` `:1100`→`:3097`（`has_tool_input` `:1118`→`:3147`）、`RebaseAppliedCommitOutput` `:566`→`:1137`、`Spawned` `:267`→`:261`、`database()` `:23`→`:76`、UI-diff 注释统一为 `handlers/apply_patch.rs:144`（原文 §5 与 §7.5 自相矛盾）；`TouchedFile` 的 Libra 侧使用点由 `agent_run/patchset.rs`（零命中）更正为 `mcp/resource.rs:37,2380`（**复核再订正**：`mcp/resource.rs:1136` + `orchestrator/persistence.rs:4584-4615` 是镜像结构 `TouchedFileParams`，不是 `TouchedFile` 使用点）；P0-2 注入点由 `web/headless.rs` 更正为**唯一生产注入点** `runtime/execution_control.rs:308`（**复核再订正**：`tool_loop.rs:2393` 属 `#[cfg(test)]` 测试夹具，非注入点）；`append_normalized_event` 的生产塌陷主体是 `append_normalized_event_with_envelope` @ `:3110`（**复核再订正**，`:3097` 已是 `#[cfg(test)]` 包装）。以上三条见本节末「复核订正」。**未漂移**：`compute_replacements` @ `core.rs:285`、`apply_replacements` @ `:585`、`to_canonical_string` @ `spec.rs:129`、`traces_commit` @ `contract.rs:384`、`TRACES_BRANCH` @ `src/internal/branch.rs:42`、`append_trailers` @ `commit.rs:1964`、`format_libra_trailers` @ `history.rs:6225`、`idx_notes_ref` @ `2026061401_notes.sql:9`、`FileHistoryEntry` @ `file_history.rs:81`；`compute_diff` 调用方仍仅 `blame.rs:429,431` + `log.rs:2034`，rebase/cherry-pick 仍 0 命中。
9. **护城河论述降调（摘要 + §4.6 + §7.3）**：`git-ai-project/git-ai`（`793066013`）已出货行级归属存 `refs/notes/ai`（`specs/git_ai_standard_v3.0.0.md:23-24`）、随 push/fetch 同步（`README.md:250`），并以规范 §2（`:420-881`）逐场景规定 rebase/merge/reset/cherry-pick/stash/amend 下的归属 MUST 行为，另有 `git ai blame`（`README.md:20,137`）。故「rebase/merge 后稳定」**不再是 Libra 独有**；仍成立的独有项为**可签名可验证**（git-ai 规范对 signature 零命中）与**与 usage 聚合同库可 join 的可查询存储**（git-ai 的聚合是付费 teams 版，`README.md:209`）。措辞「任何 hook 式编辑器插件结构上做不到」已删。⚠️ **对简报的订正**：git-ai 声明不用 **Git** hooks、不 wrap Git 二进制（`README.md:80,199-200`），但它**仍由自己安装并托管 agent hooks** 采集（`README.md:202-203`）——因此 §1.4「hook 采集保真度低于 `apply_patch` 源头」的论证对 git-ai **依然成立**，被推翻的只有「hook 世界结构上无法持久化/过线」一条。§7.3 表补 `letta-ai/trajectory`（`21ae92d`）与 git-ai 为第二/第三种候选互操作格式。
10. **计划治理对齐**：§状态 + §3 补 `AG-ATTR` 归属（`plan-long.md:267`，**P2 / 候选 / 未排期**）与完成判据（`:276`，落在导入侧 = P3-13，且**不要求格式必须是 agent-trace**）；§4.1.3 按 `plan-long.md:253` 补「导出版本 pin 到本地 revision 观察到的 `0.1.0`，`ai.traceExportVersion` 可覆写，规范未冻结前不把 schema 一致性当完成判据」；P1-6 与 §7.3 注加限定，正面引用 `plan-long.md:524` 的**不采纳**条（「未冻结的 agent-trace RFC 直接写进默认 commit 元数据」）——该条**保留不删、不改判**，本文只加限定以免相抵；§7.3 原「唯一干净路径」措辞改为「最干净的路径」。
11. **与 [`delta.md`](delta.md) 的交叉引用（§3 + §4.3 P1-5 + §7.4）**：`delta.md` §6.2 **B1** 提议 `libra blame --provenance`（按行给 session/turn/intent，走 PatchSet hunk→turn 映射）并**建议 AG-ATTR 由 P2 升为 P1**、与 CH-04 `ai_operation_link` 排为同一条线。本文 P1-5 提的是 `libra blame --ai`（按行给 contributor/model_id，走 `apply_patch` → `ai_edit_trace`）。**两者是同一命令表面的两种切法**，已登记为待决策（落地前二选一或合并为一个 flag 的两种投影）；优先级重估提案**登记但不在本文单方面改判**。
12. **P1-4 的关联表复用约定（新增）**：`ai_edit_trace` 的 commit/operation 关联应优先复用**已落地的 `agent_bridge_link`** 与 `plan-20260822` 的 **`ai_operation_link`**，避免第三套关联表；注意该计划状态为**「已排期、实现未开始」**（`plan-long.md:118,200`），落地时该表可能尚不存在。（**2026-08-27 复核订正**：该表编号归属拆为三段——建表 `Task OL-02` @ `:759`/`:765`、字段集 `ADR-OL-08` @ `:191`、写入查询 `Task CH-04` @ `:1528`；原文笼统写「CH-04 的 `ai_operation_link`」会漏掉 OL-02 这个建表前置。）
13. **§7.4 迁移排序提醒订正**：现最新迁移已是 `2026082401_agent_bridge_link_relations.sql`（原文写 `2026061401_notes.sql`）。
14. **§1-§4.3 表面缺口复核结论**：P0-0..P3-13 **全部仍未落地**——`ai_edit_trace`、`blame --ai`、`log --ai-only`、`usage report --by file`、`TraceRecord` 导出/导入在树内均零命中。路线图本体不变，仅论证前提与锚点更新。
15. **新增避坑提醒（§4.4）**：`trusted`/`trust` 命名与既有 `src/internal/ai/observed_agents/trust.rs`（AG-18 外部 `libra-agent-*` 二进制信任记录：`TrustRecord` @ `:59`、`Provenance` @ `:71`、config 前缀 `agent.trust.` @ `:32`）语义冲突，P0-0/P1-4 的字段建议改名为 `attribution_trust`/`claim_source`。
16. **§6 示例订正**：`tool.version` 由 `"0.42.0"` 改为 `"0.21.27"`（与真实基线一致，避免读者当成真实产物版本）。

**本轮对上一阶段盘点简报的订正**（三条，均以实读文件为准）：

- 简报称 `2026080401_agent_capture_workspace_scope.sql` 给 `agent_session` 加了 `worktree_id`——**不准确**。该迁移给 `agent_session` 只加了 4 列（`repo_id`/`workspace_id`/`workspace_fence`/`scope_state`，`:9-14`），`worktree_id` 只加给了 `agent_export_job`（`:17`）与 `agent_import_identity`（`:25`）；全仓 `grep -rn "worktree_id" sql/` 亦无 `agent_session` 命中。文中已按实际写。
- 简报称 git-ai「明确宣称不使用 git hooks、不 wrap git」并据此认为其非 hook 采集——**只对一半**。它确实不用 Git hooks / 不 wrap Git 二进制（`README.md:80,199-200`），但 `README.md:202-203` 明写 "Git AI **manages the agent hooks**"。见要点 9 的订正。
- 简报把 git-ai 历史重写规范定位在 `:422-566`——**范围不准**。实际 §2 History Rewriting Behaviors 覆盖 `:420-881`（2.1 Rebase `:424`、2.2 Merge `:580`、2.3 Reset `:632`、2.4 Cherry-pick `:689`、2.5 Stash/Pop `:749`、2.6 Amend `:817`）。文中已按实际写。
- 另有两处行号微差已按实读取值：`worktree_common_storage()` 在 `util.rs:422`（简报写 `:423`）；`IntegrityHash` 的 `from_str` @ `:64`、`serialize` @ `:75`（简报写 `:60-71` / `:73-80` 区间）。

**复核订正（2026-08-27，同轮对抗式复核后回改，全部经实读复验）**：

本轮刷新自身被逐条对抗复核，命中 5 条（1 实质 + 4 精度）。**P2 精度订正（第二轮复核判 PASS 后，同日就地收口，均经实读复验、无一改变结论、全部行内改写不增删行）**：① §4.3 P0-2 与下表 C-1 把一条具名 grep 的输出写成「仅 3 处命中」，实测 `grep -rn 'FileHistoryRuntimeContext' src/` 输出 **6 行**（多出 `execution_control.rs:20`、`tool_loop.rs:2226` 两处 import 与 `sandbox/mod.rs:73` 字段声明，后者同单元格自己也引用了，属自相矛盾），两处均改为「共 6 行 → 去 import 与字段声明后 3 处实体命中」；② 本节末「本次未能核实」小节未随 C-5 同步，仍把 `:191` 字段集挂在 CH-04 名下、收尾句漏 OL-02 建表前置，已改挂 **ADR-OL-08** 并补齐「OL-02 建表 / CH-04 写入」双前置；③ 裸文件名锚点 `TRACES_BRANCH` @ `branch.rs:42` 会误导到 `src/command/branch.rs:42`（实为 `utils::{` import 行），补全为 **`src/internal/branch.rs:42`**，§7.5 存档段、§7.5 未漂移清单、§9 要点 8 三处同改。第一轮 5 条的修正如下，**编号体系与路线图结论不变，仅锚点归属与编号精度收紧**：

| # | 订正点 | 原写法（本轮刷新引入） | 实测事实 | 已改章节 |
|---|---|---|---|---|
| C-1 | **P0-2 注入点（实质）** | 「**两个**实际注入点：`execution_control.rs:308` 与 `tool_loop.rs:2393`」 | `tool_loop.rs:2393` 在该文件 `#[cfg(test)] mod tests`（`#[cfg(test)]` @ `:1598`，文件共 3716 行）内，构造值为 `temp_dir.path().join("session-root")` / `batch_id: "turn-7"`，是测试夹具。全仓 `grep -rn 'FileHistoryRuntimeContext' src/` 共 **6 行**，除 2 处 `use` import（`execution_control.rs:20`、`tool_loop.rs:2226`）与字段声明 `sandbox/mod.rs:73` 外，**仅 3 处实体命中**：`sandbox/mod.rs:106`（定义）、`execution_control.rs:308`（`user_task_runtime_context()`，**唯一生产构造**）、`tool_loop.rs:2393`（测试）。→ **生产注入点只有一个** | §4.3 P0-2、§7.5 重定位表、§9 要点 8 |
| C-2 | **hook 塌陷函数命名** | 主锚点写 `append_normalized_event` @ `:3097` | `:3097` 带 `#[cfg(test)]`（@ `:3096`），只转调 `append_normalized_event_with_envelope` @ `:3110`；`"has_tool_input": event.tool_input.is_some()` @ `:3147` 在后者体内，生产调用点 @ `:3043`。结论（tool_input 被塌成布尔）不变，仅主体命名归属改正 | §2 表、§5 锚点表、§7.5 重定位表、§8 锚点清单 |
| C-3 | **`TouchedFile` vs `TouchedFileParams`** | 「Libra 侧使用点是 `mcp/resource.rs:37,1136,2380` + `persistence.rs:4584-4615`」 | `:1136` 是 `pub struct TouchedFileParams {`（仓内自定义镜像结构），`persistence.rs:4584-4615` 亦全为 `TouchedFileParams`；只有 `:37`（import）与 `:2380`（`TouchedFile::new`）是外部类型使用点。混列会让读者误以为改 `git-internal` 须同步这些位置——恰恰相反，`TouchedFileParams` 可自由扩展，正是行区间字段该落的地方 | §2 表、§5 锚点表、§7.5 重定位表、§9 要点 8 |
| C-4 | **§2 表体例过度改动** | `agent_run` 行的「关键文件」列被整列替换为 `TouchedFile` 使用点 | 表内其余各行第二列均为子系统文件清单，且 `src/internal/ai/agent_run/{patchset,evidence,event}.rs` 三个文件今天仍全部存在（`ls src/internal/ai/agent_run/`）。已恢复子系统清单为第二列，把外部 crate + 使用点的订正移入第五列「关键缺口」 | §2 表 |
| C-5 | **`ai_operation_link` 编号归属** | 「`plan-20260822` **CH-04 的 `ai_operation_link`**（schema 见 `:191`）」 | `:191` 是 **ADR-OL-08「OperationMetaV2 只保存 redacted 因果 ID」**的 Decision 行；**建表任务是 `Task OL-02`**（`:759`，v2 schema 替换清单 @ `:765`）；`Task CH-04`（`:1528`）只负责写入/查询。⚠️ **复核员原判「建表在 OL-05」不成立**——`OL-05` @ `:924` 是「WorkspaceStatePointer 与 stale/sibling 检测」，与本表无关；本文按实读取 **OL-02**。按 plan-long 治理规则编号不得混用，故三段归属分列 | §4.3 P1-4、§8 参考、§9 要点 12 |

**遗留待复核项**：

- C-5 的三段归属（OL-02 建表 / ADR-OL-08 字段集 / CH-04 写入查询）**均取自 `plan-20260822` 的计划文本，而非已落地代码**——该计划整体「已排期、实现未开始」（`plan-long.md:118,200`），P1-4 真正立卡时须重新确认表名、列名与任务编号未在实现阶段变更。
- C-2 已确认 `:3043` 是 `append_normalized_event_with_envelope` 的**仓内唯一非测试调用点**；若后续 hooks 管线重构新增调用路径，§2/§5 的锚点须同步。

**本次未能核实（待复核）**：

- **无阻塞性待复核项。** 简报列为待复核的 `ai_operation_link` **字段集**本轮**已核实**（**`ADR-OL-08`** @ `plan-20260822.md:191` 明列字段集；建表归 `Task OL-02` @ `:759`/`:765`、写入查询归 `Task CH-04` @ `:1528`——三段归属见上表 C-5，CH-04 **不拥有** schema），故该项关闭；但仍需注意该计划整体「实现未开始」，P1-4 若要复用其稳定 ID，须在 **OL-02 建表 / CH-04 写入**真正落地后再确认列名与语义未变。
- **上游规范本体无法复核**：`cursor/agent-trace` 远端 404，本地 `2754f07` 是否等于远端最新**无法证实**。若日后仓库迁移到新 org/名，须重新对齐 §1/§1.3/§1.4 的全部竞品断言与导出 version pin。
