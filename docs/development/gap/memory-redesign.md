# Memory 设计修订方案（2026-08-07 第六次竞品审计；2026-08-09 第七次审计增补 G12–G15）

## 文档职责

本文是对 [`../tracing/memory.md`](../tracing/memory.md)（1809 行 draft、尚未实现）的**设计修订提案**：以 `/Volumes/Data/competition` 本地语料（第六次竞品审计，30 个直接两层仓库）的实测机制为证据，给出 memory.md 的差距清单、逐项修订方案、评测与验收设计以及落地顺序。

- **代码现状（2026-08-07 复核）**：`src/` 中尚无 `MemoryNote` / `MemoryEvent` / `refs/libra/memory/*` 实现，也无 `libra memory` 命令；已落地的只有 `MemoryAnchor` 体系（`src/internal/ai/context_budget/memory_anchor.rs`）与 `with_memory_anchors()`（`src/internal/ai/prompt/builder.rs`）。memory.md 的 Phase A 尚未开工，设计修订成本最低。
- **事实来源**：memory.md 全文、`plan-long.md`（MEM-01..05）、本地竞品仓库 README / DESIGN / docs（revision 引脚见 §2）、Libra 相关源码。
- **权威边界**：本文是提案，不替代 memory.md / `agent.md` / `code.md` / `mainline.md` 的设计权威；与既有文档冲突时以对应权威为准，本文只登记「文档同步债」或「待决策」。
- **落地状态**：A1 / A2 / A7 已按本文落地到 memory.md（§16 评测与验收、§8.7 混合检索通道、§0.0.4 本地语料复核）；**A3（Working 层 / §10.5）、A4（Trust Gate / §7.5.1）、A5（可移植导出导入 / §5.6）、A6 决策（scope_key 为最小单位 / 团队同步维持 local-only）、B2（TOC + 执行摘要 + MEM 追溯表）、B3（MemoryAnchor wire 映射 / §11.6.1）、C（consolidate 命令 / §12–§13）以及 G12–G15（§10.5/§13.1/§18）已按第七次审计合并到 memory.md（2026-08-09）**；B1（§0.0 细节外移）留待后续按需。**2026-08-09 增补 MEM-06**：memory.md 新增 §19 并行多 Agent 协调 Memory（`coordination` namespace + `MemoryCoordinator` + `CoordinationView`），plan-long.md 登记 MEM-06（P1 候选），见 memory.md §19 与 plan-long.md MEM-06。

## 1. 结论速览

| 修订项 | 目标位置 | 对应 | 优先级 | 状态 |
|---|---|---|---|---|
| A1 评测与验收 | memory.md 新 §16 | MEM-02 验收 | P0 | **已落地** |
| A2 混合检索通道 | memory.md §8.7、§5.2/§14 表 | MEM-02 | P0 | **已落地** |
| A7 §0.0 语料刷新 + 反例登记 | memory.md §0.0.4 | 文档治理 | P0 | **已落地** |
| A3 四层巩固与归并语义 | §3.2 / §10.5 | MEM-03 | P1 | **已落地（§10.5 Working 层）** |
| A4 Trust Gate 显式化 | §7.5 / §10 | MEM-03 | P1 | **已落地（§7.5.1）** |
| A5 可移植导出/导入 | §5.5 扩展 | MEM-05 | P2 | **已落地（§5.6）** |
| A6 数据边界与团队同步 | §5.4 / §17(→18) | 开放问题 | P1 决策 / P3 实现 | **决策已落地（scope_key 最小单位、团队同步 local-only）；实现待 Phase D** |
| B1 §0.0 细节外移 | gap/memory-redesign.md 承接 | 结构 | P1 | 待落地 |
| B2 目录 + 执行摘要 + MEM 追溯表 | memory.md 头部 | 治理 | P1 | **已落地** |
| B3 MemoryAnchor 规范性 wire 映射 | §11.6 | 一致性 | P1 | **已落地（§11.6.1）** |
| C `consolidate` 命令补齐 | §12 / §13 | 一致性 | P1 | **已落地** |
| G12 投毒隔离与再验证闭环 | §10.5 / §13.1 | MEM-03 | P1 | **已落地（§10.6 / §13.1）** |
| G13 高信任 note 定期再验证 | §7.5 | MEM-03 | P2 | **已落地（§10.6 / §13.1）** |
| G14 `--debug-memory` 升 Phase C 硬交付 | §18 | 可观测性 | P1 | **已落地（§18）** |
| G15 adaptive profile 触发与验收 | §8.7 / §18 | MEM-02 | P2 | **已落地（§18）** |

## 2. 本地竞品语料与机制分析（第六次审计，revision 引脚）

范围：`/Volumes/Data/competition/*/*` 30 个仓库中 Memory 类 11 个 + 相邻参考 3 个。所有 revision 为 2026-08-07 第六次同步后的本地 HEAD。

| 项目 | revision | 承重机制（已验证入口） | 取舍 |
|---|---|---|---|
| `rohitg00/agentmemory` | `d60652a` | 四层巩固 working→episodic→semantic→procedural；Ebbinghaus 衰减；SHA-256 去重（5min 窗口）；隐私过滤；BM25+vector+graph 三流 RRF(k=60) + 会话去重（max 3/session）；SessionStart 注入 2000-token；Git snapshot（README "Memory Pipeline" / "4-Tier Memory Consolidation"） | 采纳机制（MEM-01/02/03 主证据）；54 MCP tools 作规模反例；R@5 95.2% 为自报口径 |
| `MachineWisdomAI/fava-trails` | `6653f9f` | Markdown+YAML 存于 Git（JJ colocate）；`drafts/` 隔离；Trust Gate（LLM 评审）；supersession 隐藏旧版；原子提交；engine/fuel 分离（README） | 采纳 Trust Gate 为显式 promotion 阶段（A4）；数据仓库分离形态不照搬 |
| `matrixorigin/Memoria` | `54c9114` | 记忆 snapshot/branch/merge/rollback（MatrixOne CoW）；vector+全文混合；自治理（矛盾检测、低置信隔离）；**per-user DB 数据边界**（docs/per-user-database-architecture.md）；LongMemEval+BEAM 6-bucket 能力分类（docs/memory-ability-taxonomy.md） | 采纳「版本语义必须落在正确数据边界」与 6-bucket 评测视角（A1/A6）；「Git for memory」宣传口径不采纳 |
| `sachinsharma9780/memweave` | `2ff82df` | 纯 Markdown + SQLite（FTS5 BM25 + sqlite-vec）；MMR 去重；核心路径零 LLM；embedding 内容哈希缓存；无 embedding 时降级纯关键词；LongMemEval-S R@5 97.24%±0.12%（README + benchmarks/） | 采纳「确定性基线常开 + 可选向量 + 降级」（A2）；数字自报 |
| `sqliteai/sqlite-memory` | `0f0aede` | SQLite 扩展；Markdown 为真源；vector+FTS5 混合；markdown-aware chunking；llama.cpp 本地 embedding；**CRDT offline-first 同步**（sqlite-sync） | 参考团队同步（A6）；默认上传托管服务不采纳 |
| `graphwisdom/perstate` | `95e27e3` | branch-as-identity；session↔branch 绑定；fork/switch/prune；物化 Markdown 视图（SKILL.md） | 已采纳（memory.md §3.4 / §5.5），维持 |
| `sl4m3/ledgermind` | `99220d1` | 自主知识生命周期（SQLite+Git）；自愈衰减；PATTERN→EMERGENT→CANONICAL；无监督演化（README） | **反例**：无审计自主变异；登记不采纳 |
| `ruvnet/agentic-flow` | `d3735a3` | 编排 + 自学习 hooks + 记忆/trajectory；66 agents / 213 MCP tools / QuantumDAG（README） | 宣传口径，不作证据 |
| `letta-ai/trajectory` | `59c0db5` | 多 runtime transcript 归一化为验证记录；`diagnostics` 恒存在（README） | 采纳为外部 transcript 导入前置（A5，与 AG-ATTR 衔接） |
| `letta-ai/agent-file` | `78212eb` | `.af` 开放格式：identity / memory blocks / skills / settings；可 checkpoint 与版本控制（README） | 采纳子集评估（A5）；完整云生态绑定不采纳 |
| `letta-ai/letta-code` | `ac359eeb` | memory blocks（agent 自改写）；MemFS（全部上下文 git 跟踪）；`/sleeptime` dreaming；`/doctor` / `/palace`；三级 skills（README） | 采纳记忆块投影与排期巩固（A3/A5）；「自改 harness」哲学不采纳 |
| `letta-ai/skills` | `16352df` | 社区 skill 知识库 + peer review（README） | 参考 skill 投影（A5） |
| 相邻：`facebook/sapling` | `119e6d1f75d` | Crewmate 私有目录授权钩子：默认 deny、有界授权扇出（commit 119e6d1f75d） | 参考团队 publication 的 fail-closed 授权（A6） |

注意事项（source-grounded 纪律）：

- `agentmemory/DESIGN.md` 实为 Lamborghini 网站设计稿，与记忆无关——语料内部质量参差，机制引用必须按 README / 源码文件逐一验证，不能因仓库名采信。
- 所有 R@5 / token 节省数字均为 vendor 自报口径，只提供场景与目标参考，不进入 Libra 验收（见 §5.6）。
- 本轮所有仓库均以 `--ff-only` 同步且 revision 已引脚；`agenta-ai/agenta` 因 dirty 不作证据。

## 3. 当前设计评价

### 3.1 保留的强项（不修改）

1. Snapshot/Event/Projection 三层模型 + `refs/libra/memory/*` 线性历史 + CAS 并发（§0.1、§4）。
2. CompileRecord / ContextReceipt 的确定性可重放承诺（§0.2、§8.6）。
3. 分层 scope（Actor→Worktree→Branch→Repo→Global）与 fail-closed 读取（§3.4）。
4. 遗忘/脱敏语义、MCP C9 门禁、`SecretLike` 边界（§10.6、§13）。
5. 投影表可重建、账本表豁免 rebuild 的边界（§5.2、§14）。

### 3.2 差距清单

| # | 差距 | 证据 | 对应修订 |
|---|---|---|---|
| G1 | 无评测/验收面，MEM-02 无法验收 | agentmemory / memweave / Memoria 均有 recall 基准与能力桶 | A1 |
| G2 | 混合检索只是开放问题，不是一等设计 | agentmemory / memweave / sqlite-memory / Memoria 收敛到 BM25+向量 hybrid + 降级 | A2 |
| G3 | 四层巩固模型不完整（无 Working 层、无调度/衰减语义） | agentmemory 四层 + Ebbinghaus；letta `/sleeptime`；MEM-03 | A3 |
| G4 | Trust Gate 未显式化 | fava-trails drafts 隔离 + LLM 评审 | A4 |
| G5 | 可移植导出/导入（MEM-05）在设计文档缺失 | letta `.af` / MemFS / skills | A5 |
| G6 | 团队同步与数据边界无决策记录 | Memoria per-user DB；sqlite-sync CRDT；Sapling Crewmate | A6 |
| G7 | §0.0 语料停留在 2026-07-13，缺本地主证据与反例登记 | 第六次审计语料 | A7 |
| G8 | CLI/MCP 缺 `consolidate`（§10.5/Phase D 引用但 §12/§13 列表缺失） | memory.md 内部 | C |
| G9 | 无目录/执行摘要/MEM 追溯表 | memory.md 内部 | B2 |
| G10 | §11.6 MemoryAnchor 对齐是散文，无规范性 wire 映射 | memory.md 内部 | B3 |
| G11 | §0.0 大段外部分析混在规范内 | memory.md 内部 | B1 |

## 4. 修订方案（A1–A7 + B + C）

### A1 评测与验收（已落地 memory.md §16）

- 确定性探针集（离线、无 LLM）＋ LLM 场景（nightly）双层；能力桶采用 Memoria 6-bucket 辅助视角、LongMemEval-S / BEAM 官方标签为主口径；另建 Libra 专属场景（branch/commit anchor、supersession、forget、actor 隐私、跨 worktree 边界、无 embedding 配置）。
- 指标：recall@k / NDCG@k、注入预算合规、重放一致性（同快照同 bundle_hash 100%）、`SecretLike`/`Confidential` 零泄漏、召回 P99 延迟。
- CI：确定性探针必跑（offline）；LLM 场景 nightly（`--features test-live-ai`）；评测输出固定 JSON 报告并与版本化基线对比；评测只读，不修改权威 ref。
- 完整设计见本文 §5。

### A2 混合检索通道（已落地 memory.md §8.7）

- Channel 0 确定性常开（路径前缀 + BM25/FTS5）；Channel 1 可选向量（sqlite-vec + 本地 embedding，内容哈希缓存）；Channel 2 图（Phase E，§6.4 有界一跳/两跳）。
- 融合：RRF 固定 k（起点 k=60）+ MMR/会话去重上限（起点 max 3/session）；融合排序冻结进 ContextReceipt。
- 回退：无 embedding 配置 → 仅 Channel 0，fail-closed；embedding 不进 canonical hash / bundle_hash。
- 数据表：新增 `memory_embedding_cache`（可丢弃，§5.2/§14）。
- 完整设计见本文 §6。

### A7 §0.0 语料刷新与反例登记（已落地 memory.md §0.0.4）

- 新增 §0.0.4 本地语料复核：revision 引脚表、机制→取舍、反例登记、vendor 口径警示、指向本文 §2。
- 反例登记：ledgermind 无监督自主演化；agentic-flow 宣传口径；agentmemory 54-tool 规模；`.af` 云生态绑定。

### A3 四层巩固与归并语义（待落地，对应 MEM-03）

- 明确 Working = 本地有界摄入缓冲（raw observation 只作 compiler 输入，不进 ref、不新增 note kind，与「先编译再使用」一致）。
- §10.5 补排期巩固作业（SessionEnd/定时，借鉴 letta `/sleeptime`）、TTL/auto-evict、矛盾检测确定性规则优先（body hash + evidence 重叠）再 LLM 增强。
- 验收：巩固/遗忘单测+集成；晋升失败不泄漏私有原文；`doctor` 报告记忆健康。

### A4 Trust Gate 显式化（待落地，对应 MEM-03）

- 在 §7.5 与 §10 之间定义 Draft → Gate（确定性规则默认 + 可选 LLM reviewer，fava-trails 模式）→ Confirmed；gate 输入（draft/policy/evidence）、输出（pass / fail→quarantine 或保持 draft / needs-human）写入 `CompileRecord.policy_version` 与 event reason；gate 失败不泄漏私有原文。

### A5 可移植导出/导入（待落地，对应 MEM-05）

- 导出格式：`libra-bundle`（第一版）→ `.af` 子集评估 → MemFS 布局；skills 以 `procedural.skills.*` 投影到 `.agents/skills`（只读）。
- 导入一律生成私有 Draft，重走 redaction / CompileRecord / authz / CAS 全流程。
- 验收：导出→清空→导入→recall 命中 round-trip + 兼容范围文档化。
- 外部 transcript 导入前置：trajectory 式归一化 + diagnostics（与 AG-ATTR 衔接）。

### A6 数据边界与团队同步（待落地）

- 固化不变量：**scope_key 是 snapshot/fork/rollback 的最小单位**（Memoria per-user DB 教训）；Worktree 开放问题按「Repo 共享、Worktree 不共享」定案并补跨 scope 泄漏 fail-closed 规则。
- 团队 publication 维持 local-only；把 sqlite-sync CRDT、Sapling Crewmate fail-closed authz 记为未来触发证据（前置 C9 + SB-02 + mainline ML-01 manifest）。

### B 结构修订（待落地）

- B1：§0.0 每项目细节移入本文（gap/memory-redesign.md），memory.md 只留决策表与引脚。
- B2：memory.md 补目录、一页执行摘要、MEM-01..05 追溯表。
- B3：§11.6 补规范性 wire 映射（MemoryAnchorKind/Scope/Confidence/ReviewState ↔ MemoryNote 字段/状态机，含 Quarantined 不进 anchor wire enum 的现有结论）。

### C 一致性修复（待落地）

- §12 CLI 补 `libra memory consolidate`（mutating）；§13 MCP 补 `memory_consolidate`（受 C9 门禁）。
- §18 开放问题重排：#4 已由 §8.7 解决；#6（`--debug-memory`）升为 Phase C 硬性交付。

## 5. 评测与验收设计（A1 全文）

### 5.1 目标与原则

1. 所有长期完成判据必须有可度量口径；vendor 自报数字只作目标参考，不进入验收。
2. 评测分两层：**确定性探针**（离线、无 LLM、CI 必跑）与 **LLM 场景**（nightly，`--features test-live-ai`）。
3. 评测只读：使用独立 seed corpus 与冻结 view 快照，不修改权威 ref、不写 receipt 之外的本地账本。

### 5.2 能力桶与场景集

主口径：LongMemEval-S / BEAM 官方能力标签。辅助视角（Memoria `memory-ability-taxonomy.md` 的 6-bucket）：

| 桶 | 覆盖能力 | Libra 场景示例 |
|---|---|---|
| Single-Session Grounding | 单会话事实/上下文提取 | `codebase:onboard` 摘要命中 |
| Preference Understanding | 用户偏好（非关键词命中） | `default` 用户约束 recall |
| Multi-Session Synthesis | 跨会话整合 | 巩固后 semantic note 覆盖多 session 事实 |
| Temporal State Tracking | 时间/顺序/状态演化 | `valid_from/valid_until` + commit anchor 过滤 |
| Knowledge Update & Conflict | 新旧知识、矛盾消解 | supersede / quarantine 后 recall 结果 |
| Abstention & Constraint | 无证据拒答、遵守约束 | 无命中时不注入、不编造 |

Libra 专属场景：branch/commit anchor 切换后 recall 变化；supersession 隐藏旧版；`forget` 后默认读取不可见；actor 隐私隔离；跨 worktree 边界；无 embedding 配置降级。

### 5.3 指标定义

| 指标 | 定义 | 门槛建议 |
|---|---|---|
| recall@k / NDCG@k | k=5、10，确定性基线 vs 混合通道分别上报 | Phase C 基线冻结后再定目标；vendor 数字只参考 |
| 注入预算合规 | 注入 tokens ≤ `LIBRA_MEMORY_PROMPT_BUDGET_TOKENS` | 100% |
| 重放一致性 | 同 view 快照 → 同 selected IDs / 顺序 / bundle_hash | 100%（硬门槛） |
| 隐私泄漏 | `SecretLike`/`Confidential` 进入 prompt/日志/receipt 的次数 | 0（硬门槛） |
| 召回延迟 | 引擎内召回 P99 | 有界（与预算同量级） |

### 5.4 评测资产与 CI

- 资产目录：`tests/data/memory/`（seed corpus、scenario 定义、期望结果 JSON）；评测 target 建议 `tests/memory_eval.rs`（确定性探针）并登记 `tests/INDEX.md`。
- 确定性探针：offline CI 必跑；LLM 场景：nightly。
- 输出：固定 JSON 报告（`--json`），与版本化基线对比，回归即失败；评测不写权威 ref。

### 5.5 验收门槛（对应 MEM）

| MEM | 门槛 |
|---|---|
| MEM-01 | 本地单仓记录/列出/删除 round-trip；秘密探针零泄漏；schema forward + 迁移测试；损坏数据 fail loud |
| MEM-02 | 无 embedding 配置召回可用且可测；注入不超预算；citation 可人工核验；与 LR-07 preflight 共享检索服务 |
| MEM-03 | 巩固/遗忘单测+集成；晋升失败不泄漏私有原文；doctor 可报告健康 |
| MEM-05 | 导出→清空→导入→召回命中；兼容范围文档化 |

### 5.6 vendor 参考口径

| 项目 | 自报数字 | 用途 |
|---|---|---|
| agentmemory | R@5 95.2%；92% fewer tokens | 场景/目标参考，不验收 |
| memweave | LongMemEval-S R@5 97.24%±0.12%；NDCG@5 92.28% | 场景/目标参考，不验收 |
| Memoria | LongMemEval + BEAM 双基准 | 能力桶与双口径（retrieval + end-to-end QA）参考 |

## 6. 混合检索设计（A2 全文）

通道与降级（详见 memory.md §8.7，此处为评审快照）：

```text
query
  -> Channel 0: path prefix + BM25/FTS5（常开，确定性）
  -> Channel 1: vector（可选；sqlite-vec + 本地 embedding，内容哈希缓存）
  -> Channel 2: entity graph 1-2 hop（Phase E，§6.4 有界）
  -> fusion: RRF(k=60 起点) + MMR/会话去重（max 3/session 起点）
  -> fail-closed: 无 embedding 配置或 provider 失败 -> 仅 Channel 0
```

边界：

- embedding / 向量索引**不**进入 canonical hash、`bundle_hash` 与回执；adaptive profile 必须冻结统计快照。
- `memory_embedding_cache`（§5.2）可整体丢弃，不参与 rebuild。
- 融合排序与 channel snapshot 必须写入 ContextReceipt，保证同快照可重放。

## 7. 优先级与阶段映射

| 修订项 | memory.md 位置 | 对应 | 优先级 | 阶段 |
|---|---|---|---|---|
| A1 | §16（新） | MEM-02 | P0 | Phase A 起 |
| A2 | §8.7 | MEM-02 | P0 | Phase C |
| A7 | §0.0.4 | 治理 | P0 | 本次 |
| A6（决策） | §5.4 / §18 | 开放问题 | P1 | 本次评审 |
| C | §12 / §13 | 一致性 | P1 | **Phase A 前（第七次提前）** |
| A3 | §3.2 / §10.5 | MEM-03 | P1 | **Phase B 前（第七次提前）** |
| A4 | §7.5 / §10 | MEM-03 | P1 | **Phase B 前（第七次提前）** |
| G12 投毒隔离与再验证闭环 | §10.5 / §13.1 | MEM-03 | P1 | **Phase B（新增，第七次）** |
| G14 `--debug-memory` 升硬交付 | §18 | 可观测性 | P1 | **Phase C（新增，第七次）** |
| A5 | §5.5 扩展 | MEM-05 | P2 | Phase D/E |
| G13 高信任 note 再验证 | §7.5 | MEM-03 | P2 | **Phase D（新增，第七次）** |
| G15 adaptive profile 触发与验收 | §8.7 / §18 | MEM-02 | P2 | **Phase C 后（新增，第七次）** |
| A6（实现） | §5.4 | 团队同步 | P3 | 未来 |
| B1–B3 | 结构 | 治理 | P1 | 随 A 项改版 |

## 8. 落地顺序与验收

1. 本次：A7（§0.0.4 语料刷新 + 反例）→ A2（§8.7 混合检索 + 表）→ A1（§16 评测与验收）→ 提交。
2. 下次：C（consolidate 命令补齐）→ A4（Trust Gate）→ A3（四层巩固）→ G12（投毒隔离闭环）→ A6 决策定案 → B2/B3。
3. 每次落地执行：结构校验（表格列数、链接、编号引用、冲突标记、尾随空白）、`libra diff --check`、仅提交目标文件。

## 9. 第七次审计新增缺口（G12–G15）

第七次竞品审计（2026-08-09）Memory 类竞品仓库（agentmemory `d60652a`、fava-trails `6653f9f`、Memoria `54c9114`、memweave `2ff82df`、sqlite-memory `0f0aede`、ledgermind `99220d1`、perstate `95e27e3`、letta agent-file `78212eb` / skills `16352df` / trajectory `59c0db5` / letta-code `a75f4d93`）**全部无更新**，无新机制需吸收。下列缺口来自对 memory.md 现有规范的深读，非竞品更新触发；它们把 §0.0.11 已列出的投毒风险与可观测性承诺固化为可执行规范。

### G12 投毒隔离与再验证闭环（P1，对应 MEM-03）

**现状缺口**：§0.0.11 把 OWASP 记忆投毒列为风险，§4.1.1 支持按 CompileRecord（producer/prompt/model 版本）批量定位受影响 note，但没有定义「投毒隔离 → 追溯 → 再验证或重编译 → 恢复」的可执行闭环；§13.1 只有零散不变量。ledgermind 的无监督自愈是反例（不可审计），Libra 需要**有审计**的投毒处置。

**修订方案**：在 §10 增补「投毒隔离」小节——
- **触发**：矛盾检测、低置信度骤增、来源失效（`EvidenceRef` 不可解析）、或人工 `quarantine --reason poisoning` 批量隔离。
- **处置**：批量 `Quarantined`（不静默删除），按 `CompileRecord` 的 `producer`/`prompt_version`/`model_id`/`rules_version` 追溯受影响 note 集合。
- **恢复**：`revalidate`（在固定 code version 上重跑确定性验证）或 `recompile`（以新 producer/prompt 版本重生成），均产生新 Draft 重走 gate；无修复路径者保持 `Quarantined`。
- **反例登记**：拒绝 ledgermind 式无监督自主变异；任何投毒处置都必须可解释、可回滚、写入 event reason。

**验收**：投毒注入 fixture → 断言受影响 note 全部 quarantine、prompt 注入排除、可按 producer/model 版本追溯、revalidate/recompile 后恢复或保持隔离；处置过程写入 `memory_context_receipt` 之外的独立 audit。

### G13 高信任 note 定期再验证（P2，对应 MEM-03）

**现状缺口**：`trust = Verified/RepoEvidence` 是写入时快照，代码/依赖/事实变化后信任不自动衰减或降级；§10.5 归并与 §7.5 门禁都是写入时，无再验证。

**修订方案**：在 §7.5 增补确定性再验证规则——对高信任 `procedural.*`/`semantic.repo.*` 绑定 `valid_until` 或 `revalidate_after`（借鉴 agentmemory 衰减、PowerMem revalidate-on-code-change）；代码/依赖版本变化时重跑验证，失败则降级 `trust` 或 quarantine。写入时信任不永久固化。

**验收**：code commit 变化后高信任 note 触发再验证；验证失败 → 降级/quarantine；`doctor` 可报告待再验证集合。

### G14 `--debug-memory` 升为 Phase C 硬交付（P1，可观测性）

**现状缺口**：§18 开放问题 6 仍把它列为开放问题，但这是低成本、高价值的注入可观测性。

**修订方案**：把「`libra code --debug-memory` 每 turn 逐字打印被注入的 memory 槽位」从开放问题升为 Phase C 硬性退出门槛（与 `inspect-injection` 回执并列）。

**验收**：Phase C 退出时该标志可用，逐 turn 打印注入槽位与 `ContextReceipt` 对应。

### G15 adaptive profile 触发与验收（P2，对应 MEM-02）

**现状缺口**：§8.7 已把混合检索定案，但 §18 开放问题 4 的「离线训练/显式 adaptive profile」无明确触发与验收；统计快照冻结进 receipt 的约束已写，触发条件缺失。

**修订方案**：明确 adaptive profile 触发信号（统计成功任务中反复访问的内容 → LLM 反推「什么本该更早浮现」，Cursor 自报实践）与硬约束——必须冻结统计快照并写入 receipt、不得改写 canonical `MemoryHead.rank_hint`、仅离线训练/显式 opt-in。

**验收**：adaptive profile 开启时 receipt 含统计快照 OID/hash；关闭时默认 deterministic ranking 不受 access stats 影响。
