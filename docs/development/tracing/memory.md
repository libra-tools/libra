# Memory：为 Libra Agent 提供持久化、可版本化的知识

> **Out-of-scope of `tracing/plan.md`**（§0 范围声明）：AI-agent persistent memory 设计面（agent knowledge store）不属于 AG-16~AG-24 外部捕获改进计划。本文已按当前事实对齐两项历史冲突：`LifecycleEventKind` 现为 13 个变体；MCP transport 使用 `libra code --stdio`。Memory MCP 工具仍须等待 `tracing/code.md` 所列 C9 default-deny 生产授权闭环，不能因为 transport 已存在就提前暴露。

> Status: draft
> Last updated: 2026-08-09
> Scope: 规范 Memory 子系统——agent 跨 run / thread / branch 的持久化知识存储，及其在 Libra 三层对象模型（快照/事件/投影）、运行时生命周期与 MCP 边界上的落地约定。

本文档规范 `Memory`——一个让 agent 能够跨 run、跨 thread、跨 branch 记住事物的 Libra 子系统，且不会像 `CLAUDE.md` 这样的扁平大块文件那样污染上下文。

本设计借鉴了 [`memoir-ai`](https://github.com/zhangfengcdt/memoir) 项目（“Git for AI Memory”），以及相关的记忆系统，如
[Letta / MemGPT](https://docs.letta.com/guides/agents/memory)、
[LangGraph memory](https://docs.langchain.com/oss/python/langgraph/memory)、
[OpenAI Agents SDK memory](https://openai.github.io/openai-agents-python/sandbox/memory/)、
[Mem0](https://arxiv.org/abs/2504.19413) 与
[Zep / Graphiti](https://arxiv.org/abs/2501.13956)。它被改编以适配
Libra 在 [`object-model.md`](../../ai/object-model.md) 与
[`object-model-reference.md`](../../ai/object-model-reference.md) 中所记录的三层对象模型。

如果本文档与 [`object-model.md`](../../ai/object-model.md) 或 [`workflow.md`](../../ai/workflow.md) 有冲突，以那两份文档为准。此外，外部 agent 捕获契约以 [`agent.md`](agent.md) 为准，内部 AgentRuntime、MCP transport 与授权现状以 [`code.md`](code.md) 为准；portable intent / pin / receipt 重叠面以 [`mainline.md`](../gap/mainline.md) 的 ML-07 收敛决策为准。

## 执行摘要

Memory 是 Libra 的 VCS-native 长期知识层：让 agent 跨 run / thread / branch 记住事物，且不会像 `CLAUDE.md` 扁平大块文件那样污染上下文或不可审计。核心机制是快照（`MemoryNote`）/ 事件（`MemoryEvent`）/ 投影（SQLite）三层，历史真源落在 `refs/libra/memory/*` 的普通 Git 线性历史上。每一条写入都必须携带 `CompileRecord`（可复现），每一次注入都必须产出 `ContextReceipt`（可重放）；写入统一经 `MemoryWriter` 单一 seam，读取统一经冻结的 `ResolvedMemoryView`。安全性以 fail-closed 为底线：Draft / Quarantined / SecretLike 永不注入 prompt，MCP 工具在 C9 default-deny authorizer 落地前不注册。本设计对应长期路线图 `MEM-01..05`（见下方追溯表与 [`plan-long.md`](../plan/plan-long.md)）。

### 目录

- §0 设计基线（外部参考 + 六条不可折中约束 + 竞品语料复核）
- §1 目标与非目标
- §2 为何需要 Memory（CLAUDE.md 反模式）
- §3 概念模型（四轴分类、分类树、`ResolvedMemoryView`）
- §4 对象模型（`MemoryNote` / `MemoryEvent` / 投影 + `MemoryWriter`）
- §5 存储布局（refs、SQLite 投影、ClientStorage、远端边界、物化投影、可移植导出/导入）
- §6 分类法（种子根、路径文法、迭代扩展、实体消歧）
- §7 分类管线（worthiness → 模式 → LLM → 验证 → 冲突/可信度门 + Trust Gate）
- §8 召回管线（direct / single / tiered / caller + 注入 + 回执 + 混合检索）
- §9 分支与版本（branch 隔离、fork、log/diff/blame、merge/cherry-pick）
- §10 生命周期与价值过滤（创建/取代/撤销/修剪/归并/投毒隔离/遗忘）
- §11 Agent 运行时集成（SessionStart..SessionEnd、MemoryAnchor + wire 映射、prompt 预算）
- §12 CLI 命令面
- §13 MCP 命令面 + 安全不变量
- §14 数据库 Schema 新增
- §15 分阶段路线图（Phase A–E）
- §16 评测与验收
- §17 验证计划
- §18 开放问题
- §19 并行多 Agent 协调 Memory（MEM-06）
- 总结规则

### MEM 追溯表

| MEM | 能力 | 本文落地位置 | 状态 |
|---|---|---|---|
| MEM-01 | VCS-native Memory 存储与隐私基线 | §3.4 / §4.1 / §4.2 / §5.1–5.3 | 设计定稿，Phase A |
| MEM-02 | 混合召回与会话注入（有界 token） | §8.1–8.7 | 设计定稿，Phase C |
| MEM-03 | 巩固、衰减、遗忘与团队晋升门禁 | §7.5 / §10.5 / §10.6 / §13.1 | 设计定稿，Phase B/D |
| MEM-04 | 经鉴权的 Memory MCP / 机器接口 | §13（受 C9 门禁） | 设计定稿，Phase C-MCP |
| MEM-05 | 可移植导出（`.af` / MemFS 子集）与 skill 投影 | §5.5 / §15 Phase D/E | 设计定稿，Phase D/E |
| MEM-06 | 并行多 Agent 协调 Memory（协调通道） | §19 | 设计提案（新增），依赖 MEM-01/03 |

## 0. 设计基线

### 0.0 外部记忆系统完整分析（2026-07-13 收敛版，设计参考）

本节综合当前 [`mainline.md`](../gap/mainline.md) §5.1 / §9 / §15 与用户提供的[开源 Agent Memory 调研](https://chatgpt.com/s/t_6a5460adb8608191abe538f582230f18)，并以各项目一手资料复核其承重机制。它只确定设计取舍与所有权边界，不把外部 README、benchmark、默认后端或路线图变成 Libra 的实现事实或验收标准；实现 PR 必须按当时版本重新核对。

#### 0.0.1 总结论：Libra 自有权威层，外部系统只提供机制与候选

任何现有 Agent Memory 框架都不应成为 Libra 的权威存储层。Libra 需要的不只是跨对话偏好与相似度检索，还包括 branch / commit 语义、Run / Decision / Evidence 来源链、可审查 promotion、跨 actor 授权、错误记忆撤销以及上下文选择的可解释重放。由此得到六条不可折中的约束：

1. **权威真源保持 Libra-native。** 既有 AgentRuntime typed AI objects、code commits、`MemoryNote` / `MemoryEvent` JSON blobs、Git refs 与可重建 SQLite 投影共同承载事实；不因外部调研再引入一个泛化 `MemoryObject` typed object 或第二套数据库真源。
2. **先编译，再使用。** 原始 trace 只能成为 compiler 输入；长期 Fact / Episode / Skill 必须记录来源、编译器 / prompt / model / policy 版本、幂等键与审查状态后，才有资格进入召回候选集。
3. **写入不等于可注入。** capture、Draft、Confirmed、可被当前 principal 读取、可进入 prompt 是不同门；网页、prompt、tool output、远端 publication 与 sidecar 结果都不能越过 promotion / authorization。
4. **可重放的是选择，不是模型输出。** Context bundle 的对象集合、排序原因、token 预算与 policy/index 快照应可复核；不得把它扩张为外部 provider 回答逐字节确定性的承诺。
5. **时间与 Git DAG 同时显式。** Fact 需要 recorded time、有效 commit / branch anchor 与 supersession；单一 wall-clock 区间不能替代 branch ancestry，也不能把分叉历史伪装成线性真值。
6. **衰减与遗忘不破坏审计。** recency 只影响排名；expiration / tombstone 阻止未来读取与注入；物理删除另受不可变 Git 历史、远端副本与 durable tier 的合规约束。

建议的逻辑数据流为：

```text
M0 private trace / existing typed AI objects / code commits
        ↓ deterministic rules + optional LLM compiler
M1 Fact + M2 Episode + M3 Policy/Skill/Profile candidates
        ↓ validation + authorization/review + append-only events
confirmed MemoryNote/MemoryEvent history on Libra refs
        ↓ scope/ACL/trust/sensitivity filter before ranking
ContextBundle + private ContextReceipt
        ↓ optional candidate-only adapters
graph / vector / linker / reranker sidecars
```

这里的 M0–M3 是**逻辑层级**，不是新增的 git-internal `ObjectType` 家族。规范存储仍使用本文 §4 的 `MemoryNote` / `MemoryEvent` / projection 模型；`ContextBundle` / `ContextReceipt` 是读取时的局部审计产物，不自动成为长期 Memory，也默认不得进入团队 ref。

#### 0.0.2 核心架构参考：Statewave 的 compile-then-use 与 receipt

[Statewave](https://github.com/smaramwbc/statewave) 的承重价值是 `append-only episode → typed compiled memory → token-bounded context bundle → governance` 生命周期，以及从 bundle 回指 source episode、policy snapshot 和 assembly receipt 的 provenance。Libra 应吸收：

- compiler 输入对象哈希、算法 / prompt / model 版本与幂等键；
- 选择对象、排名原因、token 预算、index manifest、policy snapshot 与 bundle hash；
- 同一固定输入和选择策略可重建同一 bundle 的契约。

Libra 不采用其 Postgres / pgvector 真源，也不照搬「相同查询返回相同 bytes」为整个 agent 的确定性承诺。receipt 原语必须与 `mainline.md` ML-05 / ML-08 共用，Memory 不再创建第二种互不兼容的选择回执。

#### 0.0.3 本地可读投影参考：ReMe 的 capture / consolidate 流水线

[ReMe](https://github.com/agentscope-ai/ReMe) 把原始 session / resource 渐进处理为 daily 与 digest 下的个人事实、procedure 和 wiki Markdown，并用 BM25、embedding、wikilink 与后台 consolidation 支撑召回。其 `capture → index → consolidate → recall` 分工适合 Libra，但文件角色必须反转为：

- `.libra/views/memory/**` Markdown 只是人类可读、可审查的 projection（§5.5）；
- `indexes/**` 只保存可重建索引或 manifest；
- 用户或 agent 对 Markdown 的编辑必须转译为 proposal → validation → authorization/review → `MemoryEvent`，不能原地改变 Policy 或 branch truth；
- consolidation 保留原始 M0 / source refs，不用摘要覆盖或删除证据。

#### 0.0.4 时间事实参考：Graphiti 的 provenance、有效期与 supersession

[Graphiti](https://github.com/getzep/graphiti) 把 episode 作为派生事实的来源，并为事实关系保留有效期，支持时间、关键词、语义与图遍历混合检索。Libra 应将这套思路落到既有 note/event 模型：

- `MemoryNote.created_at` 与相应 `MemoryEvent.at` 表示 Libra 何时记录、接受或改变事实；
- `valid_from` / `valid_until` 配合 `Branch` scope 与 commit `evidence_refs`，表示事实在哪段代码历史中成立；
- `supersedes` / `contradicts` / `supports` 显式表达替代和冲突，旧事实不被就地覆盖；
- 结构化 entity mention 与 link index 是可重建的基础投影；图数据库、embedding 与高级 entity linker 仍只是 Phase E Adapter。

Libra 不引入 Neo4j / FalkorDB / Neptune 一类图后端作为权威层，也不假设外部图对节点属性、显著性、冲突和 Git 分支语义已给出完整答案。

#### 0.0.5 Git UX 与自编辑边界：Letta Code / MemGPT

[Letta Code](https://github.com/letta-ai/letta-code) 证明 context、memory block 与 skill 可以具备 Git history、agent / project scope、查看与质量诊断 UX；[Letta / MemGPT](https://docs.letta.com/guides/agents/memory) 的「小型常驻 core + 大型按需 archival」也为 token 分级提供了直接参考。

Libra 采纳 diff / history / review、常驻小核心、按需档案与 scope-aware skill，但拒绝 agent 无审查地重写 memory、prompt、Policy、Skill 或 Constraint。所有高影响自编辑都必须先形成显式 proposal，经过 evidence、权限、schema 和安全策略验证后才追加新事件；历史版本保持可 blame / revert。

#### 0.0.6 运行状态与长期记忆分离：LangGraph / LangMem

[LangGraph persistence](https://docs.langchain.com/oss/python/langgraph/persistence) 明确区分 thread-scoped checkpointer 与 cross-thread store；[LangMem](https://github.com/langchain-ai/langmem) 则区分 agent hot-path 写入与 background extraction / consolidation。Libra 的映射为：

| 外部概念 | Libra 归属 | 禁止的捷径 |
|---|---|---|
| Checkpoint / thread state | run / session checkpoint、Run 内 `ContextFrame`、恢复 / fork | 不因持久化就自动成为长期或团队 Memory |
| Raw interaction / tool trajectory | M0 Trace、Run / ToolInvocation / Evidence | 不直接进入 prompt 常驻段 |
| Cross-thread fact | M1 `MemoryNote` + 来源与有效性 | 不允许无来源的 LLM 摘要直接 Confirmed |
| Background manager | §10.5 consolidation job | 不在 hook 热路径中静默改写真源 |

这项参考不授权 Libra 引入第二套 graph runtime，也不改变现有 AgentRuntime 的 checkpoint / history 边界。

#### 0.0.7 Experience-to-Skill：PowerMem / MemOS

[PowerMem](https://github.com/oceanbase/powermem) 的 Experience + Skill 两层蒸馏与混合检索、[MemOS](https://github.com/MemTensor/MemOS) 的 memory cube / scheduler / 分层 skill 演化，适合补足 M2 → M3：

```text
多次 Run / Episode
  → 聚类出候选 procedure
  → 记录适用与不适用条件、权限和正反样本
  → 在固定 code version 上以 Test / Evidence 试运行
  → review / signature promotion 为 Skill
  → 代码或依赖变化后 revalidate；失败时 rollback / revoke
```

时间衰减最多进入 ranking score，不能删除 Episode、失败样本、promotion 记录或审计事件。Memory cube / pack 可作为未来导入导出格式参考，但必须包含 source refs、ACL / policy、compiler versions、signatures 与 index hints；导入包仍先进入隔离候选区。

#### 0.0.8 补充参考、sidecar 与算法基线

| 方案 | 可吸收机制 | Libra 定位与边界 |
|---|---|---|
| [memoir-ai](https://github.com/zhangfengcdt/memoir) | 语义路径、路径聚合、worthiness、branch-aware capture、迭代 taxonomy | 保留为 §6 / §7 的键控与分类参考；拒绝另建 Prolly-tree 真源，补上 namespace、审查、trust 与 sensitivity。 |
| [perstate](https://github.com/graphwisdom/perstate) | `repo + branch` state 绑定、session fork / switch / status、bare repo + worktree、Markdown/YAML 实体关系图与 shell 渐进查询 | 作为 session 绑定、可读物化投影和图浏览 UX 的参考；不采纳「branch 即 actor」、把 worktree 当 writer lock、pull 失败后继续写、Agent 在 prepare/commit 之间直写真源或开放 schema 无校验。所有写入仍须经过 §4.2.1 的统一 writer、ACL / policy、CAS 与事件状态机。 |
| [OpenAI Agents SDK memory](https://openai.github.io/openai-agents-python/sandbox/memory/) | summary → searchable index → detailed rollout 的渐进披露与「当前环境优先」的陈旧性纪律 | `memory.summarize()` 与 caller-driven recall 的参考；Markdown 不是权威层，新近度不能静默删除低频高价值事实。 |
| [Cognee](https://github.com/topoteretes/cognee) | 自托管 graph/vector knowledge、traceability、MCP 与 Rust client | 外部文档、代码知识和跨仓库关系的只读 candidate provider；不保存或决定 Run / Evidence / Decision 的权威状态。 |
| [Mem0](https://github.com/mem0ai/mem0) | scope、filter、CRUD/Search API、延迟 / token / 质量测量 | API 与 benchmark baseline。其 OSS v3 ADD-only / entity-boost 行为是版本限定事实，不能替代 Libra 的 supersession、branch 或图遍历语义。 |
| [A-MEM](https://github.com/agiresearch/a-mem) / [HippoRAG](https://github.com/OSU-NLP-Group/HippoRAG) | Zettelkasten-style 动态链接、graph / Personalized PageRank 多跳召回 | 可插拔 `MemoryLinker` / `ContextRetriever` 算法实验；MVP 不增加其存储依赖。 |
| [agentmemory.md](https://agentmemory.md/) / [Memoria](https://arxiv.org/abs/2512.12686) | 人类可读记录、append-only history、混合搜索、矛盾隔离 | 强化 quarantine、回滚和可审计性；论文或站点描述不直接形成实现验收。 |

所有 sidecar 统一遵守 candidate-only 契约：可返回内容、score、reason 与 provenance；不可直接 append `AI_REF` / memory ref、生成 pin、改变 `review_state`、写入 Policy / Skill，或决定当前 branch truth。

#### 0.0.9 M0–M3 在本文对象模型中的归属

| 层 | 内容 | 本文规范映射 | Promotion 门槛 |
|---|---|---|---|
| **M0 Trace** | 消息、hook / session、ToolInvocation、Run、PatchSet、Test、Evidence | 既有 typed AI objects 与私有原始捕获；只是 `evidence_refs` 来源 | 自动捕获可行，但默认 local / private，绝不自动注入或团队发布 |
| **M1 Fact** | 项目事实、环境状态、约束、实体关系 | semantic `MemoryNote` + `MemoryEvent`；带 recorded / valid time、commit anchor、confidence、supersedes | 规则或 compiler 产生 Draft；冲突、来源、scope 与权限验证后 Confirmed |
| **M2 Episode** | Task / Run 的目标、过程、结果、失败原因 | episodic `MemoryNote`，引用 Run / Outcome / Evidence / Decision | 必须有稳定 outcome 与 evidence；摘要不能覆盖原始 trace |
| **M3 Profile** | user / actor 偏好 | semantic / procedural note，actor / user scope | 仅主体本人或明确授权 principal 可修改 |
| **M3 Skill** | 可复用 procedure | procedural note + validation / promotion events | 正反样本、工具权限、适用代码版本、成功 evidence、review 与 revalidation |
| **M3 Policy** | 组织规则、稳定约束 | 最高信任级 procedural note | 默认 human / signature approval；任何 imported / model-generated 内容不得自动生效 |

该映射修正了外部调研中把 `MemoryObject` 当作新权威对象的歧义：Libra 需要吸收其字段语义，而不是新增平行对象族。M1–M3 的每次变化都通过 append-only event 表达，SQLite、Markdown、Hub、graph 与 vector 仅是投影。

#### 0.0.10 与 mainline.md 的五轴收敛

`mainline.md` 是 portable sealed code intent / pin / recall 的 committed schedule；本文是通用长期 Memory 的设计提案。二者按其 §9 与 ML-07 只允许以下关系：

| 重叠轴 | 单一归属 / 共用原语 | 禁止重复建设 |
|---|---|---|
| 存储与传输 | intent-team 与 memory 可有独立 ref / schema；共同复用 classification、lease、validation、tracking、watermark 与 safe-publication primitives | 不 mirror 原始 `AI_REF`；Memory 不复制 transport stack |
| Decision / Evidence | mainline ML-03 扩展 live git-internal `Decision`；Memory `evidence_refs` 指向同一对象 | 不在 Memory 重新定义 Decision 真源 |
| Recall / 注入 / receipt | 单一 scope-aware 检索与 prompt 注入管线；ContextReceipt 原语由 ML-05 / ML-08 与本文共用 | 不创建两套 `with_memory` / SessionStart 注入和两种 receipt |
| Constraint / Skill | portable code constraint 的排期由 mainline ML-12 管；通用 M1 / M3 语义由本文定义，ML-07 先收敛 | 未定案前不让 `constraint` 与 `procedural.*` 双写 |
| Hub / Markdown / API | 共用 `code.md` C4 `/api/code/*` 读面，所有编辑回到 proposal/event 流程 | 不建立平行 `/api/v1` 或可直写真源的 UI |

因此，mainline 只消费 seal / pin / provenance 完整且通过 policy 的高置信对象；它不负责通用 Fact compiler、Episode consolidation、Skill promotion 或独立 memory refs。反过来，本文也不重排 ML-01~ML-13，不把普通 Memory 自动升级为 team intent。

#### 0.0.11 安全、可回放与验收纪律

[OWASP AI Agent Security Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/AI_Agent_Security_Cheat_Sheet.html) 将 memory poisoning 明确列为跨会话风险；[OWASP Agent Memory Guard](https://owasp.org/www-project-agent-memory-guard/) 的完整性检查思路可作为补充，但 hash 只能证明 bytes 未变，不能证明内容可信、可见或有权注入。Libra 至少必须做到：

- principal / namespace / scope / visibility / ACL / trust / sensitivity / redaction 在 relevance、recency、graph expansion 与 rerank **之前**执行；
- prompt、网页、tool output、远端 publication 与低信任 trace 默认进 Draft / Quarantined，不得直接 promotion 为 Policy / Skill / Constraint；
- ContextReceipt 记录 selected IDs、reason codes、bundle hash、as-of commit/ref、selector/compiler/index/policy version；缺失快照时返回 stale / non-reproducible，不静默 fallback；
- receipt、query hash 和 selected IDs 也可能泄露工作意图，默认 local-only；任何共享另做 visibility / retention / threat-model 审查；
- append / confirm / supersede / revoke / forget / import / merge 均须 schema validation、CAS / lease、审计事件和稳定错误语义，mutating path fail-closed；
- 安全 fixture 覆盖跨 actor / user 越权、poisoning、secret recall、malformed import、stale receipt、投影漂移、tombstone 与 rollback；外部自报 benchmark 只能提供场景，不能替代 Libra 测试结果。

最终定位：**Libra Memory = Libra-native 内容寻址与事件历史 + Statewave 式 compiler / receipt + ReMe 式可读投影 + Graphiti 式时间事实 + Letta 式 Git 审计 UX + LangGraph 式运行态分离 + PowerMem / MemOS 式受审查 Skill 晋升；图、向量与外部框架永远是可替换的候选层。**

#### 0.0.4 本地竞品语料复核（2026-08-07，第六次审计）

本节把第六次竞品审计的本地语料（`/Volumes/Data/competition/*/*`，Memory 类 11 个 + 相邻参考 3 个，revision 已引脚）固化为设计取舍；逐项目机制分析与验证入口见 [`../gap/memory-redesign.md`](../gap/memory-redesign.md) §2。vendor 自报数字只提供场景与目标参考，不进入验收（§16）。**第七次审计（2026-08-09）**：Memory 类竞品仓库全部无更新，唯一 revision 变化是 `letta-ai/letta-code` `ac359eeb`→`a75f4d93`（新增 `EnterWorktree`/`ExitWorktree` worktree 工具，属 LR-01 并行工作区，与 Memory 核心无关）；设计取舍不变，新增 G12–G15 固化投毒隔离、高信任再验证、`--debug-memory` 硬交付与 adaptive 触发（见 §10.5/§13.1/§18）。

| 项目 | revision | 承重机制 | 取舍 |
|---|---|---|---|
| `rohitg00/agentmemory` | `d60652a` | 四层巩固（working→episodic→semantic→procedural）、BM25+vector+graph 三流 RRF(k=60)+会话去重（max 3/session）、隐私过滤、Git snapshot | 采纳机制：MEM-01/02/03 主证据；54 MCP tools 作规模反例 |
| `MachineWisdomAI/fava-trails` | `6653f9f` | `drafts/` 隔离、Trust Gate（LLM 评审）、supersession 隐藏旧版、原子提交 | 采纳 Trust Gate 为显式 promotion 阶段（A4，§10 修订） |
| `matrixorigin/Memoria` | `54c9114` | 记忆 snapshot/branch/merge/rollback、per-user DB 数据边界、vector+全文混合、LongMemEval+BEAM 6-bucket 能力分类 | 采纳数据边界教训与 6-bucket 评测视角（§16）；宣传口径不采纳 |
| `sachinsharma9780/memweave` | `2ff82df` | Markdown+SQLite（FTS5 BM25 + sqlite-vec）、MMR 去重、核心路径零 LLM、无 embedding 降级 | 采纳「确定性基线常开 + 可选向量 + 降级」（§8.7） |
| `sqliteai/sqlite-memory` | `0f0aede` | vector+FTS5 混合、llama.cpp 本地 embedding、CRDT offline-first 同步 | 参考团队同步（§5.4 未来项）；托管服务不采纳 |
| `graphwisdom/perstate` | `95e27e3` | branch-as-identity、session 绑定、物化视图 | 已采纳（§3.4/§5.5），维持 |
| `sl4m3/ledgermind` | `99220d1` | 自主知识生命周期、自愈衰减、无监督演化 | **反例**（见下） |
| `ruvnet/agentic-flow` | `d3735a3` | 66 agents / 213 MCP tools / QuantumDAG（宣传口径） | 不作证据 |
| `letta-ai/trajectory` | `59c0db5` | 多 runtime transcript 归一化 + diagnostics | 采纳为外部导入前置（A5 / AG-ATTR） |
| `letta-ai/agent-file` | `78212eb` | `.af` 可移植格式（identity / memory blocks / skills / settings） | 采纳子集评估（A5） |
| `letta-ai/letta-code` | `a75f4d93` | memory blocks、MemFS（git 跟踪）、`/sleeptime` dreaming、`/doctor` | 采纳记忆块投影与排期巩固（A3/A5）；自改 harness 哲学不采纳 |
| `letta-ai/skills` | `16352df` | 社区 skill 知识库 + peer review | 参考 skill 投影（A5） |
| `facebook/sapling`（相邻） | `119e6d1f75d` | Crewmate 私有目录授权钩子（默认 deny、有界授权扇出） | 参考团队 publication 授权（A6，§5.4 未来项） |

**反例登记（不采纳）**：

- **ledgermind 式无监督自主演化/自愈**：无审计的自主变异违反「先确定性、后智能化」与可审计原则；任何巩固、衰减与矛盾消解都必须可解释、可回滚。
- **agentic-flow 宣传口径**（66 agents / 213 tools / QuantumDAG）：无源码承重证据，不作参考。
- **agentmemory 54-tool 堆砌**：作为 MEM-04 的规模反例——Memory 工具面必须小而稳定。
- **`.af` 完整云生态绑定**：只评估子集（identity / memory blocks / skills / settings 中与 Libra 兼容的部分）。

注意事项：`agentmemory/DESIGN.md` 实为 Lamborghini 站点设计稿，与记忆无关——语料内部质量参差，机制引用必须按文件逐一验证；本节的取舍不因任何仓库的 README 宣传自动成立。

### 0.1 方案审查结论

本方案总体合理且可行：它没有为 Memory 新增 git-internal typed object，而是把 Memory 作为普通 Git blob + refs + 可重建 SQLite 投影实现，符合 Libra 现有三层对象模型、外部 agent 捕获边界与 MCP 边界。路径键、namespace、scope、审查状态、trust / sensitivity 门禁也能解决 CLAUDE.md 式扁平长期记忆的上下文污染、不可审计与不可回滚问题。

为达到可落地标准，本文档补强以下方面：

- **接口兼容性**：固定 ref 命名、CLI / MCP 工具名、JSON schema version 与旧 reader 跳过未知字段的规则。
- **数据流正确性**：规定摄入、事件追加、投影更新、prompt 注入的顺序，避免确认前注入、投影漂移与并发覆盖。
- **安全性**：要求在 LLM 分类、MCP 返回、远端分层存储之前先做 redaction / owner filtering / size cap，且所有 mutating 操作 fail-closed。
- **可靠性**：定义 CAS 式 ref 更新、事件 total order、幂等 event replay、rebuild 校验与损坏对象处理。
- **性能**：限定路径深度、查询上限、prompt 预算、LLM 调用预算、cache TTL 与分页，避免 unbounded prefix scan 或 token 膨胀。
- **合规性**：把 `forget` 明确为投影级脱敏与未来压实 API，而非承诺立刻从不可变 Git 历史中物理删除。

本轮多维复核同时识别并修正了八个原草案中的实现阻断；后文规范均以这些结论为准：

1. **ref 前缀冲突**：Git 不能同时保存 `refs/libra/memory` 与 `refs/libra/memory/...`；规范根 ref 改为 `refs/libra/memory/repo`，其它 scope 使用其兄弟 ref。
2. **OID 自引用**：`MemoryNote` 正文不能包含“自身 blob OID”再参与该 OID 的计算；`revision_oid` 改为写入后派生的 envelope / event 字段，不属于序列化正文。
3. **事件总序**：普通 Git merge 的 DAG 拓扑不是全序；每个 memory ref 必须保持 first-parent 线性历史，并以单调 `event_seq` 重放。跨 ref 合并由 `libra memory merge/cherry-pick` 重新验证并追加，不直接接受任意 merge commit。
4. **读写分离**：`SessionAttached`、`PromptTrimmed`、`last_used_at`、`use_count` 属本地访问遥测，不写入权威 memory ref；否则一次读取也会争抢 CAS，并破坏“投影可从 note/event 完全重建”的定义。
5. **MCP 前置条件**：当前 `McpAuthorizer` 未在生产安装时仍是 allow-all，且工具覆盖不完整；Memory MCP surface 必须等 C9 default-deny、principal threading、`tools/list` 与全部 call-site 授权完成后才可发布。
6. **敏感数据落盘边界**：`SecretLike` 原文不得进入 note、event、compile hash、receipt、SQLite 或远端 tier；`Confidential` 在无加密与明确 publication policy 前只允许保留本地脱敏摘要和受控证据引用。
7. **prune / forget 语义**：缓存修剪不能产生历史状态，也不需要 `revive`；法律遗忘只能阻止未来默认读取并记录 tombstone，不能虚假承诺从 Git、backup 或远端副本物理删除。
8. **确定性边界**：receipt 保证固定 source OID 与 selector snapshot 下的选择输入、顺序和渲染可验证；使用次数、wall clock、随机 UUID 与 provider 输出不得进入 canonical bundle hash。

### 0.2 骨架选型决策（2026-07-13）

基于 §0.0 的外部参考，曾对比五种互斥骨架：**A** 分类树导航型（memoir-ai 主导，即本文 §1–§18 的形态）、**B** 记忆编译器型（Statewave 主导，M0 唯一入口、一切皆编译产物）、**C** 双时间事实账本型（Graphiti 主导）、**D** 可读投影审查型（ReMe / Letta Code 主导）、**E** 运行态分离蒸馏型（LangGraph / PowerMem / MemOS 主导）。

**决策：骨架采用 A，同时把 B 的两个原语从「字段级补丁」升级为硬性退出门槛。** 这不是可选优化——骨架 A 的原始形态（直接 `remember` 即成为 confirmed 候选）对 §0.0.1 约束 2「先编译，再使用」满足最弱，以下两条是让 A 合规的必要修正：

1. **编译记录（`CompileRecord`，§4.1.1）是 Phase A 的发布门槛。** 任何入口（显式 remember、anchor 提升、frame 蒸馏、分类器、consolidation、onboarding、branch fork、import）产生的每条 `MemoryNote` 都必须携带完整编译记录（来源入口、producer / prompt / model / policy 版本、输入对象哈希、幂等键）；缺失或不完整一律拒绝写入，同幂等键重复摄入不产生新 note。
2. **注入回执（`ContextReceipt`，§8.6）是 Phase C 的发布门槛。** 每次 prompt 注入与引擎内召回都必须产出回执，记录选中对象、排序原因、token 预算与 policy / index 快照；`memory inspect-injection` 从回执重放，而非从当前投影反推。回执原语与 mainline ML-05 / ML-08 共用（§0.0.10），本文不新建第二种 receipt。

C 的双时间语义（§4.1 已含 `valid_from` / `valid_until` + commit anchor 形态的 `evidence_refs`）与 E 的 skill promotion 管线（§0.0.7、§10.5）保留为骨架 A 上的后续增量，不改变本决策。

### 0.3 采用 perstate 参考后的演进顺序（2026-07-18）

perstate 证明 Git-native memory 的第一性产品价值并不来自新的后端，而来自一个小而可解释的使用面：当前 session 绑定到哪份 state、如何 fork / switch、如何直接浏览实体和关系、以及每次写入如何形成可审查提交。Libra 吸收这些价值，但必须保留 §0.0.1 的 compiler / review / authorization / receipt 纪律。演进顺序固定为：

1. **先冻结 writer 与 session view 的所有权。** 所有 CLI、hook、MCP、onboarding、consolidation 与 branch fork Adapter 只通过一个 `MemoryWriter` Module 改变真源（§4.2.1）；读取侧统一解析 `ResolvedMemoryView`（§3.4），不让调用方各自拼 scope、ref、watermark 或 policy。
2. **Phase A 先交付可审计闭环。** 在 repo ref、事件历史和 SQLite 投影之外，同步交付只读 `memory status` 与 Markdown 物化投影（§5.5），让用户在分类器、图数据库和 Web UI 之前就能检查「当前看见什么」与「为何看见」。
3. **Phase B / C 再增加知识加工。** 摄入侧补实体消歧、语义重复候选与关系所有权规则（§6.4）；召回侧先基于 `memory_link_index` 提供有界邻接查询，图数据库仍只是 Phase E Adapter。
4. **Phase D 才开放分支复制与归并。** branch memory 不随代码分支创建而静默复制；`memory fork` 从冻结的 source ref OID 语义复制已选择的 confirmed note，重新分配目标 scope 的 note/event 标识并重跑 policy（§9.1.1）。
5. **Phase E 最后提供富 UI 与可选索引。** 任何 Markdown / graph / Web renderer 都把正文视为不可信输入，必须先满足 §13.1 的本地资源、转义、净化与 CSP 门槛。

这个顺序加深了两个关键 Module：`MemoryWriter` 用小 Interface 隐藏摄入到投影提交的全部复杂度，`ResolvedMemoryView` 用小 Interface 隐藏 scope 叠加与新鲜度判断。删除任一 Module 都会让相同复杂度扩散到所有调用方，因而它们提供真实 leverage 与 locality；可读投影与图索引只是其上的可替换 Adapter，不成为平行真源。

## 1. 目标与非目标

### 1.1 目标

- 为 agent 提供一个**持久化、可查询的知识存储**，能在 thread 关闭、进程重启与 branch 切换之后继续存活。
- 让人类能够用 Libra 已为代码构建的同一套 Git 级工具，去**审计、diff、blame 与 revert** agent 学到的东西。
- 保持召回**透明且廉价**：先做分层路径查找，仅在必要时才用基于 LLM 的分类，且基础实现中**不引入 embedding 索引**。
- 复用 Libra 既有的**快照（Snapshot）/ 事件（Event）/ 投影（Projection）**三层划分，使 Memory 继承与 `Intent` / `Plan` / `Task` / `Run` 相同的审计、重建与并发保证。
- 做到**branch 感知**：切换用户的工作 branch 时应自动切换 agent 的记忆视图。
- 保持写入**可审查且可逆**：自动捕获可以起草记忆，但要把它提升为可进入 prompt 的事实，须经过审查状态、置信度（confidence）、来源出处（provenance）与冲突检查的把关。
- 支持在同一个仓库存储中容纳**多个逻辑集合**，使用户事实、代码库 onboarding、项目 onboarding、度量指标与私有 actor 笔记不至于坍缩进同一个嘈杂的命名空间。

### 1.2 非目标

- 取代 `ContextFrame`、`ContextSnapshot` 或 `MemoryAnchor`。Memory 是一个对它们形成补充的新层——见 §3.1。
- 提供向量 / embedding 搜索引擎。基于路径的召回是默认方案；embedding 索引可作为后续扩展。
- 在基础实现中提供图数据库。时序图与实体图可以叠加在 Memory 的事件流之上，但历史层面的真源仍然是 `refs/libra/memory*` 上的普通 Git 历史。
- 静默存储 secret、私有数据或不可信的网络主张。入库时必须先对敏感度（sensitivity）与可信度（trust）进行分类，任何记忆才能成为可进入 prompt 的内容。
- 跨仓库联邦化记忆。Memory 是**按仓库（per-repo）**的构造，就像 `.libra/` 状态那样。跨仓库联邦留待将来的设计。
- 与完整聊天历史持久化竞争。Memory 存储的是**蒸馏后的、可复用的事实**，而不是原始 transcript。Transcript 已经存放在 `.libra/sessions/*.jsonl` 与 `git-internal` 的 AI 历史中。

## 2. 为何需要 Memory

### 2.1 CLAUDE.md 反模式

如今很多 agent 把长期记忆硬塞进一个扁平的全局文件（`CLAUDE.md`、`MEMORY.md`、各种 scratchpad）。这有三种失败模式（memoir-ai 的归纳在此直接适用）：

- **上下文污染。** 一次切到另一个 branch 的 `git checkout`，会让 agent 拿着*上一个 branch* 的笔记继续推理。
- **Token 租金。** 每一次细小的编辑都会使前缀缓存失效；agent 每个 turn 都要重新读取整块记忆大文件。
- **没有版本化。** 一次糟糕的插入（一个幻觉、一个过时的不变量）会毒化此后的每一次召回。对记忆本身既没有 `blame`，也没有 `revert`，更没有 `diff`。

### 2.2 Libra 已经具备什么

| 机制 | 作用域 | 生命周期 | 粒度 |
|---|---|---|---|
| `ContextFrame` ([E]) | 单个 Run / Plan / Step | Run 内仅追加 | 增量事实，不可变 |
| `ContextSnapshot` ([S]) | 单个 Run / 候选发布版 | 冻结基线 | 一组稳定的 frame 捆绑 |
| `MemoryAnchor` （session JSONL 中的 [E]） | 单个 Thread 或 Project | thread 期间确认 | 单条规则，可注入 prompt |
| `Run` / `Evidence` / `Decision` | 单次执行尝试 | 不可变 | 审计事实 |

Memory 是位于上述所有机制**之上**的那一层：

- `ContextFrame` 是 **run 内**的草稿——Run 一旦被取代（supersede）即消失。
- `MemoryAnchor` 是 **thread 内**的草稿——对单次对话有用，但无法按路径或 branch 寻址。
- Memory 是**跨 thread、跨 branch、可查询的**——由语义路径作键的持久知识。

### 2.3 memoir-ai 做对了什么

我们原封不动采纳 memoir-ai 的三个核心做法：

1. **分层语义路径**（`procedural.coding.tabs`），而非 UUID 或向量键。带复合索引和有界范围扫描时，定位起点为 O(log n)，总查询成本为 O(log n + k)；且人类可读。
2. **在路径处做记忆聚合**——把多条记忆收拢在同一个语义位置之下，而不是散落为彼此独立的文档。
3. **价值过滤（worthiness）**——并非每个 turn 都值得写入记忆；系统会显式地分类“到底要不要记”。

memoir-ai 使用 Prolly-tree 作为后端存储。**Libra 不需要这套**：我们已经实现了 Git 的磁盘格式，并拥有一等的内容寻址存储、refs、branch、commit、blame 与 revert。Memory 搭载在 `git-internal` 的 refs 上，与现有 AI 工作流对象所在的孤儿分支 `libra/intent` 并列共存。

### 2.4 其他系统对本设计的改变

外部系统都在强化同一个方向，但也暴露出第一版草稿必须补上的缺口：

| 系统 | 有用的想法 | Libra 的改编 |
|---|---|---|
| memoir-ai | 分类树路径、branch 感知记忆、读取 hook、Stop-hook 捕获、代码库 onboarding 命名空间 | 采纳按路径作键的存储与 branch refs，但把历史真源存放在 `git-internal`，而非 Prolly tree。引入命名空间，使 `default` 用户事实与 `codebase:onboard` 快照不共享保留策略与 prompt 策略。 |
| perstate | `repo + branch` state 绑定、session fork / switch / status、Markdown/YAML 文件图与 shell 渐进查询 | 采纳可解释的 `ResolvedMemoryView`、显式语义 fork、只读 status 与物化投影；拒绝把 branch 当 actor、把 worktree 当并发锁、pull 失败后继续写或让 Agent 直接编辑权威文件。 |
| Letta / MemGPT | 把始终可见的核心记忆与按需调取的归档记忆分开 | 把始终可见的记忆映射到 `ContextSegmentKind::ProjectMemory` / `MemoryAnchor`；除非被召回选中，episodic 与大体量 semantic 记忆保持按需调取。 |
| LangGraph / Deep Agents | 区分 semantic、episodic、procedural 记忆；支持 user / agent / organization 作用域以及后台归并（consolidation） | 保留 CoALA 风格的 kind 轴，新增 `namespace`，并把归并做成一个排期的 Memory 作业，而非临时的 prompt 摘要。 |
| OpenAI Agents SDK memory | 渐进式披露：先给小摘要，再给搜索索引，再给详细的 rollout 摘要；记忆可能过时 | `memory.summarize()` 必须成为面向 agent 的默认原语。召回到的笔记是指引，必须附带 evidence、置信度（confidence）、可信度（trust）与陈旧度（staleness）元数据。 |
| Mem0 | 抽取、归并、图增强召回，以及可度量的延迟 / token 节省 | 增加抽取 / 归并流水线与延迟 / token 度量。把向量 / 图召回保留为可选的二级索引，绝不作为真源。 |
| Zep / Graphiti | 时序事实与实体关系能改善多跳召回与“何时发生了什么变化”的召回 | 现在就加入有效期区间（validity interval）、来源时间戳与显式的记忆链接；把实体图的物化推迟到扩展中实现。 |
| [agentmemory.md](https://agentmemory.md/) / [Memoria](https://arxiv.org/abs/2512.12686) | 人类可读文件、仅追加日志、混合搜索、回滚、对低置信度或相互矛盾事实的隔离（quarantine） | 把可审计性与回滚作为一等公民保留。引入隔离（quarantine）、隐私把关，以及投影层修剪（prune），而非破坏性删除。 |

## 3. 概念模型

### 3.1 Memory vs ContextFrame vs MemoryAnchor

```text
within-run        cross-thread / cross-run
+---------+      +-------------------------+
| Context |      |        Memory           |
| Frame   |      | (this document)         |
| (per    |  --> | path-keyed,             |
|  Run)   |      | versioned, branched     |
+---------+      +-------------------------+
                              ^
                              | confirm / promote
                              |
                +-------------------------+
                | MemoryAnchor (within    |
                | thread, prompt-tier)    |
                +-------------------------+
```

提升（promotion）规则：

- 被发现可复用的 `ContextFrame`（例如在某次 Run 中检测到的“用户偏好 tabs”）可被**蒸馏（distil）**为一次 Memory 写入。
- 在某个 thread 中被确认的 `MemoryAnchor` 可被**提升（promote）**为某条合适路径下的 Memory 条目。降级（demotion，Memory → anchor）是读取侧的操作：与当前 prompt 相关的 Memory 条目会在 prompt 构建时被投影回 `with_memory_anchors()` 注入槽。

### 3.2 四轴分类

每个 Memory 条目沿四个正交的轴进行分类：

- **Kind**（它是什么）：`procedural` / `semantic` / `episodic`——对应 memoir-ai 的 memento 所采用的 CoALA agent-memory 本体。
- **Scope**（在哪/对谁适用）：`repo` / `branch` / `worktree` / `actor` / `global`。Scope 决定哪些查询会返回该条目。
- **Namespace**（它属于哪个集合）：`default`、`codebase:onboard`、`project:onboard`，或 `private:<actor-ref>`。Namespace 决定保留策略、prompt 注入、onboarding 与审查策略。turn/code usage metrics 属本地 telemetry / access ledger，不作为长期事实写入 MemoryNote。
- **Lifecycle**（它如何变化）：`replacement`（覆盖式，在路径处覆写，例如 `semantic.user.timezone`）或 `accretive`（累加式，在路径处追加，例如 `episodic.runs.<run-id>.outcome`）。

`scope + namespace + path` 标识一个**记忆单元（cell）**，而非单条笔记。一个 cell 可包含多条存活笔记，与 memoir-ai 的聚合模型一致。覆盖式（replacement）生命周期意味着“每个逻辑事实至多一条已确认的存活笔记”，而非“该路径下至多一条笔记”。累加式（accretive）生命周期意味着：在修剪（prune）策略移除其投影行之前，该路径下所有未被撤销（revoke）的笔记都保持可见。

### 3.3 Memory 分类树根

三个顶层根，照搬 CoALA / memoir-ai 的 memento，但针对 Libra 的语境（VCS 中的代码 agent）命名：

```text
procedural.*    -- HOW the agent should work
                   (rules, conventions, build/test commands,
                   repo-specific lints)
                   replacement-mostly

semantic.*      -- WHAT the world is
                   (user identity, tool inventory, infra facts,
                   architecture decisions)
                   replacement-mostly

episodic.*      -- WHAT has happened
                   (run outcomes, incidents, debugging breadcrumbs,
                   verified findings tied to a date or commit)
                   accretive
```

示例（路径仅为说明用途，并非规范）：

```text
procedural.coding.style.tabs
procedural.coding.tests.command
procedural.review.merge-policy

semantic.user.timezone
semantic.user.preferences.terse-replies
semantic.repo.entry-binary
semantic.repo.architecture.three-layer-split

episodic.commits.cb8adb64.regression
episodic.runs.2026-05-09.flaky-test-1147
episodic.findings.context-window-too-small
```

二进制中内置一份**固定的种子分类树**（约 50–100 条路径，覆盖上述场景）。Agent 可以通过迭代式分类器（§7）**扩展**它，但扩展出的节点会成为一等的分类树节点，并像任何其他写入一样受到审计。

种子命名空间随附策略默认值：

| Namespace | 用途 | Prompt 默认 | 保留策略 |
|---|---|---|---|
| `default` | 用户捕获与 agent 捕获的持久事实 | 摘要 + 选择性注入 | 按 kind 而定 |
| `codebase:onboard` | Git 仓库结构、命令、当前架构、经验教训 | 在 SessionStart 注入紧凑摘要 | 在 commit 移动 / 30 天陈旧时刷新 |
| `project:onboard` | 非 git 项目结构与工作流 | 在 SessionStart 注入紧凑摘要 | 在文件系统快照哈希变化时刷新 |
| `private:<actor-ref>` | actor 本地偏好或接近 secret 的笔记 | 仅对匹配的 actor 可见 | 仅可选择性提升 |

metrics 不进入上表的权威 namespace：高频遥测会放大 Git 历史、泄露工作模式并使每个 turn 争抢 ref。需要从 metrics 提炼长期结论时，由 compiler 读取本地有界 telemetry，输出带聚合阈值与来源摘要的 Draft note；原始 metrics 仍受独立 retention policy 管理。

### 3.4 `ResolvedMemoryView` —— session-local 读取投影 [L-local]

`ResolvedMemoryView` 是一次 session / CLI 调用实际可见 memory 的冻结读取视图。它不是新的 Git 对象、长期账本或授权真源；它由当前仓库身份、已认证 principal、full branch ref、worktree identity、当前 code commit、scope policy 与各 memory ref head 解析得到，并在进入选择 / 排名之前完成授权过滤。

| 字段 | 含义 |
|---|---|
| `repository_id` | 规范化仓库身份；不得仅用目录 basename |
| `principal` | 已认证 actor 的 opaque principal；不得从请求体自报字段推断 |
| `code_anchor` | 当前 code commit、full branch ref 与可选 worktree id |
| `layers` | 按 `Actor -> Worktree -> Branch -> Repo -> Global` 排列的可见层；每层记录 `scope_key`、source ref、source ref OID、projection watermark 与 policy snapshot hash |
| `excluded_layers` | 未进入 view 的候选层及稳定 reason code；只记录元数据，不携带不可见 note 的数量、path 或正文 |
| `view_hash` | 对 `repository_id`、`principal`、`code_anchor`、`layers` 与 `excluded_layers` canonicalize 后计算的哈希；不含解析时间与 `refresh_reason`，供 status、receipt 与 debug 输出互相对照 |
| `refresh_reason` | `SessionStart` / `BranchChanged` / `WorktreeChanged` / `PrincipalChanged` / `ExplicitRefresh` |

规则：

- view 解析必须 fail-closed：任何参与层出现未知 policy、watermark 不匹配、principal 不可验证或 scope 编码失败时，该层不得进入 view；安全敏感读取不得静默降级到更宽的 Repo / Global 层。
- SessionStart 冻结初始 view；branch、worktree 或 principal 改变时必须重新解析并产生新的 `view_hash`。旧 ContextReceipt 继续引用旧 view，不被当前状态覆盖。
- `memory status` 可按当前状态即时重算 view，并展示每层 source OID、新鲜度和排除原因；session attach / refresh 只进入本地有界 access audit，不推进 memory ref。
- `ContextReceipt.as_of` 必须包含 `view_hash` 与完整 layer snapshot。selector 不得在一次注入过程中重新读取当前 HEAD 或 wall clock 后偷换 view。

## 4. 对象模型

Memory 遵循与 Libra 其余部分**相同的快照（Snapshot）/ 事件（Event）/ 投影（Projection）三层模型**（见 [`object-model-reference.md`](../../ai/object-model-reference.md)）。三层缺一不可；省略其中任何一层都会重新引入 CLAUDE.md 那种反模式。

不过这里有一条至关重要的存储边界必须先讲清楚。Memory 复用外部 agent 捕获的**存储范式**（普通 blob/tree/commit + 专用 ref + SQLite 读模型），但不声称与 checkpoint layout 或生命周期“完全相同”（参见 [`agent.md`](agent.md) 的「持久化与对象边界」）。Memory **不**向 git-internal 新增任何 `ObjectType` 变体。git-internal 的 typed AI 对象族（`ObjectType::is_ai_object()` 这个闭合枚举：Intent/Plan/Task/Run/PatchSet/ContextSnapshot/Provenance 等快照，以及 RunEvent/TaskEvent/IntentEvent/PlanStepEvent/RunUsage/ToolInvocation/Evidence/Decision/ContextFrame 等事件）专属于内部 AgentRuntime，落在孤儿分支 `libra/intent` 上。Memory 借用的只是 [`object-model-reference.md`](../../ai/object-model-reference.md) 中快照/事件/投影的设计纪律；`MemoryNote` / `MemoryEvent` 是自定义 versioned JSON blob。`evidence_refs` 因而是显式跨平面引用，必须携带 source plane、kind、OID/hash 和可见性信息，不能把私有 AI 对象正文复制进 memory ref。

### 4.1 `MemoryNote` —— 快照 [S]

单条记忆某一版本的不可变、内容寻址正文。

| 字段 | 类型 | 含义 |
|---|---|---|
| `schema_version` | `u32` | JSON schema 版本；第一版固定为 `1` |
| `note_id` | `Uuid` | 同一事实的多个版本之间保持稳定的逻辑标识 |
| `content_digest` | `String` | 对不含任何存储 OID 的 canonical note payload 计算的 `sha256:<hex>`；用于跨哈希算法比较与幂等校验 |
| `namespace` | `String` | 逻辑集合，例如 `default` 或 `codebase:onboard` |
| `path` | `String` | 分类法路径，例如 `procedural.coding.tabs` |
| `kind` | enum | `Procedural` / `Semantic` / `Episodic` |
| `scope` | enum | `Repo` / `Branch(full_refname)` / `Worktree(id)` / `Actor(principal_id)` / `Global`（repo-local global policy） |
| `visibility` | enum | `Private` / `RepoLocal` / `TeamCandidate`；第一版只实现前两者，`TeamCandidate` 仍不可直接 push |
| `acl_policy_id` | `String` | 创建时适用的 versioned policy snapshot 标识；读取还必须应用当前 policy，取二者更严格者 |
| `lifecycle` | enum | `Replacement`（覆盖式）/ `Accretive`（累加式） |
| `body` | `String` | 被记住的陈述（允许 Markdown，保持简短） |
| `rationale` | `Option<String>` | 可选的「为何重要」/「从何而来」说明 |
| `evidence_refs` | `Vec<EvidenceRef>` | 指向 `Evidence`、`Run`、`Decision`、commit OID 的指针，用以佐证该条记忆 |
| `links` | `Vec<MemoryLink>` | 显式的 sibling / supports / prerequisite / contradicts / supersedes 链接 |
| `entities` | `Vec<MemoryEntityMention>` | 可选的结构化实体 mention；用于 alias 消歧与可重建实体索引，不创建第二套实体真源 |
| `parents` | `Vec<ObjectId>` | 同一 `note_id` 的先前版本（版本谱系） |
| `tags` | `Vec<String>` | 自由形式标签（`security`、`flaky`、`infra`、……） |
| `confidence` | enum | `Low` / `Medium` / `High`（复用自 `MemoryAnchorConfidence`） |
| `trust` | enum | `Verified` / `RepoEvidence` / `UserAsserted` / `ExternalUntrusted` / `Inferred` |
| `sensitivity` | enum | `Public` / `Internal` / `Confidential` / `SecretLike` |
| `valid_from` | `Option<DateTime<Utc>>` | 该事实开始为真的时间（若已知） |
| `valid_until` | `Option<DateTime<Utc>>` | 该事实不再为真的时间（若已知） |
| `effective_from_commit` | `Option<ObjectId>` | 该事实开始适用的 code commit anchor；Branch scope 下按 ancestry 判断 |
| `effective_until_commit` | `Option<ObjectId>` | 可选的结束 commit；仅在同一 ancestry path 上解释，不把分叉 DAG 线性化 |
| `expires_at` | `Option<DateTime<Utc>>` | 可进入 prompt 的可见性 TTL；历史 note 本身仍保持不可变 |
| `author` | `ActorRef` | 提出本版本的人类或 agent |
| `created_at` | `DateTime<Utc>` | 写入时冻结 |
| `compile_record` | `CompileRecord` | 本版本如何被生产出来（§4.1.1）；所有入口必填 |

规则（沿袭 [`object-model.md`](../../ai/object-model.md) 中 `Intent` / `Plan` 的快照规则）：

- `MemoryNote` JSON 必须使用稳定字段名与向后兼容的 serde 策略。新增字段只能 additive，旧 reader 必须忽略未知字段；删除或改变字段语义必须 bump `schema_version` 并提供迁移 / rebuild 逻辑。
- `revision_oid` 是 `MemoryNote` blob 写入后得到的 Git OID，只存在于 tree path、`MemoryEvent`、返回 envelope 与投影中，**不得**序列化进 `MemoryNote` 正文。否则会出现“正文包含自身哈希”的不可解自引用。
- canonical JSON 必须固定 UTF-8、字段序、数字与时间格式；`content_digest` 的输入排除该字段本身。读取时同时校验 blob OID 和 `content_digest`，任一不符均视为损坏。
- 一个 `MemoryNote` 快照回答的是**「agent 在这一版本相信什么？」**，且永不被改写。
- 撤销、取代或遗忘一条记忆都是一个**事件（Event）**，而非对快照的就地编辑；cache prune 不改变 note 生命周期。
- 对同一 `note_id` 而言，`namespace`、`scope`、`path` 在逻辑上不可变。要移动一条记忆，应写一条新 note 并取代旧的（§10.2）。
- `visibility` 或 ACL 放宽必须产生新 revision 并重新 review；当前 policy 可以随时收紧读取，但不能静默扩大历史 note 的可见范围。`Actor` / `private:*` note 的创建者声明必须绑定到已认证 principal，不能信任请求体自报的 `ActorRef`。
- `valid_from` / `valid_until` 是业务时间；`created_at` / event `at` 是 recorded time；`effective_*_commit` 是 Git DAG 有效性。召回必须先验证当前 code commit 与 anchor 的 ancestry，再考虑 wall-clock 时间。无法证明 ancestry 时标记 stale / not-applicable，不按时间戳猜测。
- `SecretLike` 的 note 只能以已编辑（redacted）的正文加证据引用的形式存储；它们绝不会被注入 prompt。
- `SecretLike` 原文及其未加盐普通哈希不得进入 `input_hashes`、事件 reason、receipt 或日志；需要去重时使用本地密钥派生的 HMAC，且 HMAC 不得进入团队 ref。`Confidential` 正文在加密-at-rest 与 publication policy 落地前也不得进入共享 ref 或远端 durable tier。
- `body` 必须有硬大小上限。第一版建议默认拒绝超过 16 KiB 的正文；更大的内容应存为 `EvidenceRef` 或 onboarding artifact，并在 Memory 中只保留摘要与引用。召回侧的对应模式是「文件即上下文」：此类大体量证据以临时文件句柄交给 agent 按需 read / grep，而非整体注入 prompt（Cursor 的 A/B 实验自报该模式使 token 消耗下降 46.9%，见 §18 开放问题 4 所引分析文章）。
- `compile_record` 缺失或不完整的 note 必须在写入事务第 1 步被拒绝（§4.1.1、§4.2.1）；编译记录随正文一同内容寻址，事后不可补写。

#### 4.1.1 `CompileRecord` —— 编译记录（Phase A 硬性门槛）

沿 §0.0.2（Statewave）与 §0.2 决策，每个 `MemoryNote` 版本必须内嵌一份编译记录，回答「这条记忆是被谁、用什么版本的规则 / prompt / 模型、从哪些输入生产出来的」：

| 字段 | 类型 | 含义 |
|---|---|---|
| `origin` | enum | `Explicit` / `PromotedFromAnchor` / `DistilledFromFrame` / `Classifier` / `Consolidation` / `Onboard` / `BranchFork` / `Import` |
| `producer` | `String` | 生产者标识与版本，如 `libra-memory/0.19.0` 或 `consolidation-job/1` |
| `rules_version` | `u32` | 确定性规则集（worthiness 正则、路径验证、redaction 策略）的版本 |
| `prompt_version` | `Option<String>` | 参与生产的 LLM prompt 模板版本；纯确定性路径为 `None` |
| `model_id` | `Option<String>` | 参与生产的模型标识；纯确定性路径为 `None` |
| `policy_version` | `String` | 当时生效的 namespace / promotion policy 版本 |
| `input_hashes` | `Vec<String>` | 输入对象哈希（源 trace / anchor / frame / 用户文本的规范化哈希），不得为空 |
| `idempotency_key` | `String` | 对 `origin ‖ scope ‖ namespace ‖ input_hashes ‖ producer 版本族` 计算的 repository-local keyed HMAC；创建时冻结，不进入团队 publication |

规则：

- 写入事务（§4.2.1）第 1 步即校验编译记录完整性：`origin` 与调用入口不符、`input_hashes` 为空或幂等键缺失，一律 fail-closed 拒绝写入。
- 幂等键去重只作用于**新建**（`Created`）：同一 `(scope, namespace)` 内同键重复摄入不产生新 note，直接返回既有 `note_id` 且不追加新事件（与 §4.2 的 event 幂等语义一致）。显式 `revise` / `move` 针对既有 `note_id`，不受其约束。
- LLM 参与生产的 note（`prompt_version` / `model_id` 非空）默认最高只能进入 `Draft`；`trust` 上限沿 §7.3 规则，不因编译记录存在而放宽。
- 发现某个 producer / prompt / model 版本产出系统性坏记忆时，必须能按编译记录批量定位受影响 note 并 quarantine 或重新编译——这是把编译记录设为硬门槛的直接回报。
- 编译记录是 note 正文的一部分，随 blob 不可变、可随投影重建；`memory_note_index` 投影为此新增 `origin` 与 `idempotency_key` 列（§5.2），存储创建版本的键以支撑去重与批量召回。
- 每个 revision 的 producer / prompt / model / policy / input fingerprint 进入 `memory_revision_index` 投影；只在 note 级保存创建 origin 无法定位后续坏 revision，因此不能满足批量 quarantine / recompile 要求。
- `input_hashes` 只能基于已经 redacted、canonicalized 的输入。对可能仍具可识别性的用户文本或 secret-like token，使用 repository-local keyed HMAC 并在字段中标明算法/key generation；普通 SHA-256 不能被当作匿名化。

#### 4.1.2 引用、链接与 policy 最小契约

为使授权、provenance 与图扩展可实现，以下 supporting types 必须 versioned，不能留成无约束 JSON：

- `EvidenceRef` 至少包含 `schema_version`、`source_plane`、`kind`、`object_id`、`content_hash`、`visibility`、可选 `captured_at` / `code_commit`。解析或授权失败只会降低 trust / 排除候选，绝不能通过远程 URL 即时抓取来“补齐证据”。
- `MemoryEntityMention` 至少包含 `schema_version`、`canonical_key`、`display_name`、`aliases`、`role`（subject / object / topic）、`resolution_confidence` 与 evidence 指针。`canonical_key` 是 repository-local 的规范键，不得编码 actor PII；alias 冲突只产生 merge proposal，不自动改写真源。
- `MemoryLink` 至少包含 `kind`、`target_note_id`、可选 `target_revision_oid`、`evidence_refs` 与可选 `valid_from` / `valid_until`。link 归属于 source `MemoryNote` revision，其变化通过 source note 的新 revision 表达；需要独立 review / revoke / supersede 生命周期的关系必须建模为单独 semantic note，不能把嵌入式 link 扩成第二套事件系统。读取 link 前先对 target 重新执行 scope / ACL / sensitivity 检查；禁止先图扩展后过滤，否则计数、路径和 timing 都会泄露私有节点。
- `acl_policy_id` 指向 canonical policy snapshot hash。policy 至少定义可读/可写 principal、允许的 namespace/scope、自动确认上限、远端存储许可、retention 与 prompt 注入许可。policy 缺失、未知或 hash 不匹配时，mutating path 与 prompt injection fail-closed。
- `manifest.json` 固定当前 ref 的 scope、schema version、last_event_seq、policy snapshot hash 与 writer version。它不得携带 secret 或 actor PII；actor ref 使用 principal HMAC / opaque ID。

### 4.2 `MemoryEvent` —— 事件 [E]

针对某个 `MemoryNote` 的只追加（append-only）生命周期记录。

| 字段 | 类型 | 含义 |
|---|---|---|
| `schema_version` | `u32` | JSON schema 版本；第一版固定为 `1` |
| `event_id` | `Uuid` | 事件标识 |
| `event_seq` | `u64` | 目标 memory ref 内严格递增的逻辑序号；由 CAS writer 在提交时分配 |
| `note_id` | `Option<Uuid>` | 目标 note；命名空间 / 分类法 / prompt 元事件无此字段 |
| `revision_oid` | `Option<ObjectId>` | 受影响的具体 `MemoryNote` blob OID；元事件无此字段 |
| `namespace` | `Option<String>` | 元事件所影响的命名空间 |
| `target_path` | `Option<String>` | 元事件所影响的路径 |
| `action` | enum | `Created` / `Revised` / `Confirmed` / `Quarantined` / `Superseded` / `Revoked` / `Forgotten` / `TaxonomyExpanded` / `Consolidated` |
| `reason_code` | `Option<String>` | 稳定、非敏感的机器原因码；详细诊断仅进入本地脱敏 audit |
| `actor` | `ActorRef` | 执行该动作的主体 |
| `at` | `DateTime<Utc>` | 发生时间 |
| `evidence_refs` | `Vec<EvidenceRef>` | 可选的、为该动作提供佐证的新证据 |
| `next_note_id` | `Option<Uuid>` | 与 `IntentEvent.next_intent_id` 同义——指向后继 note 的推荐边 |

规则：

- `MemoryEvent` 是改变记忆状态的**唯一**途径。`MemoryNote` 上没有任何可变字段。
- note 的生命周期事件必须携带 `note_id` 与 `revision_oid`。分类法元事件携带 `namespace` / `target_path`。
- 「agent 此刻在路径 X 上相信什么」这一当前状态，是通过遍历事件计算得出的**投影**（§4.3、§4.4）。
- `event_id` 必须在 replay 时幂等；重复事件按同一 `event_id` 去重，内容不一致则视为历史损坏并 fail loud。
- 每个 memory ref 必须保持单父 first-parent 线性历史；拒绝导入含 merge commit、重复/倒退 `event_seq` 或 first-parent 断裂的 ref。commit 内事件按 `event_seq` 排序，tree path 只是存储定位，不参与业务顺序。
- 事件 action 状态转移必须受状态机约束：`Created` 产生 Draft revision；`Draft -> Confirmed|Quarantined|Revoked|Forgotten`；`Confirmed -> Superseded|Revoked|Quarantined|Forgotten`；`Quarantined -> Confirmed|Revoked|Superseded|Forgotten`；`Revoked`、`Superseded`、`Forgotten` 为终态。`Revised` 创建同一 note_id 的新 Draft revision，但旧 confirmed revision 在新 revision 被 Confirmed 前仍是 live head；`Consolidated` 只是审计 annotation，不改变源 note 的 review state。恢复终态内容必须创建新 revision / note 并保留来源边，不能原地复活。
- `SessionAttached`、`PromptTrimmed`、`last_used_at`、`use_count` 属读取/访问遥测，写入本地有界 audit / receipt，不属于 `MemoryEvent`。读取不得推进 memory ref。

### 4.2.1 写入事务与投影顺序

一次成功写入必须按以下顺序执行，避免 prompt 读到未审查或半写入的记忆：

所有 mutating Adapter——CLI、hook、MCP、onboarding、consolidation、branch fork、merge 与 cherry-pick——必须调用同一个 `MemoryWriter` Module。其 Interface 接收已认证 principal、目标 scope、写入 proposal 与预期 source ref OID，返回已提交的 `MemoryRecord` envelope / rejection；Implementation 独占下面的 redaction、CompileRecord、schema / ACL / conflict validation、event sequencing、object persistence、CAS 与投影事务。不得暴露「prepare 返回工作目录 → 调用方直接编辑 → commit」的公共 seam，也不得让任何 Adapter 直接更新 memory ref、tree 或 SQLite live projection。

1. 对输入执行 principal / owner filtering、来源标注、路径规范化、大小限制与本地 redaction；任何敏感原文在进入 LLM、hash、日志或对象存储前先移除。随后执行 worthiness，并校验 `CompileRecord` 完整性与幂等键：编译记录缺失即拒绝；同幂等键重复摄入直接返回既有 note，不进入后续步骤。
2. 读取目标 ref 的 head、`last_event_seq` 与 policy snapshot；在内存中完成状态转移、冲突、ACL 和 branch-anchor 验证。为本批事件分配连续的 `event_seq`。
3. 写入 canonical `MemoryNote` blob，取得 `revision_oid`；再写引用该 OID 的 `MemoryEvent` blob并构造**单父** memory commit。
4. 使用 compare-and-swap 语义更新目标 memory ref：只有当当前 ref 仍等于读取时的旧 OID 时才推进；失败则丢弃未引用 commit，重新读取并从步骤 2 重做，最多重试配置的有界次数。
5. 在同一 SQLite transaction 中 replay 新事件并更新 `memory_head`、`memory_path_summary`、`memory_note_index`、`memory_revision_index`、`memory_link_index`、`memory_entity_index`、`memory_taxonomy_node` 与 projection watermark。
6. 只有投影事务提交且 watermark 等于目标 ref head 后，prompt 构建器才允许读取新的 confirmed head。

若步骤 5 失败，Git 历史仍是事实源；实现必须标记投影 stale。prompt injection、MCP、自动 promotion 等安全敏感读取在 watermark 不一致时 fail-closed；人类只读 CLI 可以显式选择受限的 ref 直读诊断。不得因为投影失败而回滚已提交的 Git 对象，也不得把 SQLite 投影当作事实源覆盖 Git 历史。

worktree 是否存在、某 branch 是否已 checkout、或调用方是否位于同一进程，都不是 writer lock。实现可以在 `MemoryWriter` 内使用 repository-local 有界 mutex / lease 降低 CAS 重试，但锁必须可超时、可恢复，且不能替代步骤 4 的 ref CAS；两个 session 复用同一 worktree 时仍必须通过并发测试。

### 4.3 `MemoryHead` —— 投影 [L]

按 `(scope, namespace, path, note_id)` 划分的游标，指向某条逻辑 note 当前生效的版本，并附带去规范化（denormalised）的元数据以加速读取。它存放在 SQLite，而非 `git-internal` 中。

| 字段 | 类型 | 含义 |
|---|---|---|
| `scope_key` | `String` | 规范化的作用域编码（如 `branch:main`） |
| `namespace` | `String` | 逻辑集合 |
| `path` | `String` | 分类法路径 |
| `note_id` | `Uuid` | 逻辑 note |
| `latest_revision_oid` | `ObjectId` | 该 note 最新写入的 `MemoryNote` blob OID，可能仍为 Draft |
| `live_revision_oid` | `Option<ObjectId>` | 当前可召回的 confirmed revision；新 revision 评审期间可继续指向旧 confirmed revision |
| `latest_action` | enum | 作用于 latest revision 的最近一次动作（`Created`、`Revised`、`Confirmed`、……） |
| `latest_review_state` | enum | latest revision 的 `Draft` / `Confirmed` / `Quarantined` / `Revoked` / `Superseded` / `Forgotten` |
| `last_event_seq` | `u64` | 产生当前状态的最后逻辑事件序号 |
| `rank_hint` | `i64` | 仅由 note/event 中可重放字段和固定 selector 版本推导的确定性排序提示；访问次数不得进入此字段 |

规则：

- 缺少某行 `MemoryHead` 表示**「投影缺失或该 note 无 live head」**；调用方必须结合 projection watermark 与 ref 直读区分二者。这与 `Thread`、`Scheduler` 投影的“投影不是历史真源”契约一致（见 [`object-model.md`](../../ai/object-model.md)）。
- 该投影可完全从 `MemoryNote` + `MemoryEvent` 历史重建。`libra memory rebuild` 即执行此操作。
- `last_used_at` / `use_count` 移入独立的本地 `memory_access_stats`，不参加历史重建，也不能影响 receipt 所承诺的 deterministic selector；若某种 adaptive ranking 使用访问统计，必须显式选择非确定性 profile 并在 receipt 中记录统计快照 OID/hash。

### 4.4 `MemoryPathSummary` —— 投影 [L]

按 `(scope, namespace, path)` 划分的聚合，用于回忆录式（memoir-style）的路径聚合与渐进式披露。

| 字段 | 类型 | 含义 |
|---|---|---|
| `scope_key` | `String` | 规范化的作用域编码 |
| `namespace` | `String` | 逻辑集合 |
| `path` | `String` | 分类法路径 |
| `confirmed_count` | `u64` | 直接位于该路径下、已确认（confirmed）的存活 note 数 |
| `quarantined_count` | `u64` | 直接位于该路径下、处于隔离（quarantined）状态的存活 note 数 |
| `child_count` | `u64` | 直接子路径的数量 |
| `prefix_count` | `u64` | 该前缀下已确认的存活 note 数 |
| `preview` | `String` | 供调用方驱动召回使用的稳定预览；第一版由确定性截断 / 首句规则生成，rebuild 不调用 LLM |
| `last_changed_at` | `DateTime<Utc>` | 影响该路径的最近一次事件时间 |

规则：

- `memory.get(scope, namespace, path)` 只从 `live_revision_oid` 返回该记忆单元（cell）下所有已确认的 `MemoryRecord` envelope（`note` + `revision_oid` + review state + source ref OID），按 `rank_hint` 排序。管理命令可显式查看 `latest_revision_oid` 对应的 Draft / Quarantined revision。
- `memory.get_note(note_id)` 返回当前 envelope；历史版本必须通过显式 revision OID 查询。API 不得只返回裸 `MemoryNote` 后让调用方丢失状态与来源快照。
- `MemoryPathSummary` 允许有损；它是一个 prompt 选材辅助物，而非历史真相。

### 4.5 `MemoryTaxonomy` —— 投影 [L]

活动分类树（taxonomy）的缓存、可重建视图。

| 字段 | 类型 | 含义 |
|---|---|---|
| `scope_key` | `String` | taxonomy 所属 scope；seed 节点在各 scope 投影中物化 |
| `namespace` | `String` | taxonomy 所属 namespace |
| `path` | `String` | 完整路径，例如 `procedural.coding` |
| `parent_path` | `Option<String>` | 上一行的父路径 `procedural` |
| `is_seed` | `bool` | 若随二进制内置则为 `true` |
| `expanded_from` | `Option<EventRef>` | 引入该分支的那次迭代式分类器事件 |
| `note_count` | `u64` | `path == self.path` 的存活 note 数 |
| `prefix_count` | `u64` | 路径位于 `self.path` 之下的存活 note 数（用于 O(log n + k) 的有界 summarize） |
| `last_classified_at` | `DateTime<Utc>` | 驱动 LLM 缓存的陈旧度判定 |

### 4.6 关系图

```text
Snapshot
========

MemoryNote[S] --parents---------------> MemoryNote[S]      (revision lineage)
MemoryNote[S] --evidence_refs---------> Evidence[E]
MemoryNote[S] --evidence_refs---------> Run[S] / Decision[E] / commit OID
MemoryNote[S] --links-----------------> MemoryNote[S]      (sibling / contradicts)

Event
=====

MemoryEvent[E] --note_id--------------> MemoryNote[S]
MemoryEvent[E] --revision_oid---------> MemoryNote[S]
MemoryEvent[E] --next_note_id?--------> MemoryNote[S]

Projection
==========

MemoryHead[L] --(scope,namespace,path,note_id)--> MemoryNote[S].note_id
MemoryHead[L] --latest_revision_oid---> MemoryNote[S]
MemoryHead[L] --live_revision_oid?----> MemoryNote[S]
MemoryPathSummary[L] --(scope,namespace,path)---> set of MemoryHead[L]
MemoryTaxonomy[L] --path--------------> set of MemoryHead[L] / MemoryNote[S]

Cross-layer
===========

MemoryAnchor (existing) <-----promote--- MemoryHead[L]      (read-time projection
                                                           into prompt slot)
ContextFrame[E] -----distil-----------> MemoryNote[S]      (write-time)
ContextReceipt (local ledger) --selected--> MemoryHead[L] / MemoryNote[S]
                                                           (read-time audit, §8.6)
```

## 5. 存储布局

### 5.1 Git refs

如 §4 开头所述，Memory 的字节是自定义 JSON blob，存活在自己的 `libra/memory*` ref 上，与内部 AgentRuntime 的对象历史分离。后者位于孤儿分支 `libra/intent`（常量 `AI_REF`，`src/internal/ai/history.rs:72`），承载 git-internal 的 typed AI 对象（Intent/Plan/...）；而外部 agent 捕获位于 `traces`（常量 `TRACES_BRANCH`，`src/internal/branch.rs:42`，文档中写作 `refs/libra/traces`）。Memory 自己的 ref 命名沿用同一约定：

```text
refs/libra/intent                              # 现有 AI 工作流对象的孤儿分支（Intent/Plan/...，归 AgentRuntime）
refs/libra/memory/repo                         # 新增：仓库共享 memory（NEW）
refs/libra/memory/global                       # 新增：仅在显式启用 repo-local global policy 时使用（NEW）
refs/libra/memory/branch/<encoded-full-refname> # 新增：分支作用域 memory（NEW）
refs/libra/memory/worktree/<encoded-id>        # 新增：worktree 作用域 memory（NEW）
refs/libra/memory/actor/<principal-hash>       # 新增：actor 私有 memory；不得用可逆 PII 作 ref 名（NEW）
```

Git 的 file/dir ref 规则禁止同时存在 `refs/libra/memory` 和 `refs/libra/memory/*`，因此不存在无后缀的 memory ref。`Global` 在本设计中仍是**当前仓库内的逻辑作用域**，不等于跨仓库用户档案；真正跨仓库 federation 仍不在范围内。

一个「memory commit」就是一个普通的 Git commit，其 tree 中包含：

```text
notes/<encoded-namespace>/<note_id>/<revision-oid>.json  # MemoryNote 正文
events/<event-seq>-<event-id>.json                       # MemoryEvent；seq 固定重放顺序
taxonomy/expansion/<event-seq>-<event-id>.json            # 分类法扩展记录
manifest.json                                             # schema、scope、last_event_seq、policy/index 版本
```

这些 memory commit 是普通 Git commit、内容是普通 blob 与 tree，因此底层 `libra log` / `diff` 可以用于诊断。但语义级 `memory blame`、merge、cherry-pick 仍需要 Memory shim：note 使用 JSON blob、revision 文件名和事件状态机，普通文本 blame 不能回答逻辑 note 的当前 head；任意 Git cherry-pick 也不能替代 schema、ACL、event_seq、冲突与 policy 校验。

#### 5.1.1 ref 与 tree path 编码

Memory 的逻辑 key 不得直接拼接进 Git ref 或 tree path：branch name、namespace、actor ref 与动态 path 段都可能包含 `/`、`..`、控制字符、Unicode 归一化差异或大小写冲突。实现必须使用一套稳定、可逆、跨平台大小写安全的编码：

- `scope_key` 的逻辑编码形如 `repo`、`global`、`branch:<full-refname>`、`worktree:<id>`、`actor:<principal-id>`；branch 使用规范化 full refname（如 `refs/heads/main`），而不是短名或当前 commit OID。分支 rename 是显式 scope migration，不能静默产生一份新 memory 或丢失旧 scope。
- `refs/libra/memory/branch/<encoded-full-refname>` 使用 validated canonical refname 的跨平台编码。actor ref 使用不可逆 principal hash，避免 ref 枚举泄露账号或邮箱。
- tree path 中的 `<namespace>`、`<note_id>`、`<revision-oid>`、`<event-id>` 必须只包含 `[A-Za-z0-9._-]`；namespace 如 `private:<actor-ref>` 必须编码为安全 segment。
- 解码后必须拒绝空 segment、`.`、`..`、绝对路径、反斜杠、NUL、控制字符以及超过长度上限的 segment。
- macOS / Windows 上不得依赖文件系统大小写行为区分两条 memory key；编码后的 segment 必须规范化为大小写不敏感仍不冲突的形式，或在写入前显式检测冲突并 fail-closed。

这条规则同时适用于 CLI、MCP、onboarding、rebuild 与 merge/cherry-pick。任何绕过规范化的直接 ref/path 操作都属于 bug。

### 5.2 SQLite 投影表

这些投影表通过**新建一个版本化迁移** `sql/migrations/YYYYMMDDNN_memory.sql` 引入，前向 DDL 必须幂等（`CREATE TABLE IF NOT EXISTS ...`），并可配套一份 `*_down.sql` 回滚脚本；**不要**把它们追加进 bootstrap 文件 `sql/sqlite_20260309_init.sql`。这与外部捕获采用迁移 `2026050303_agent_capture.sql` 的方式属于同一模式。

下面以普通 `CREATE TABLE` 形式给出表结构，落地时请置于上述迁移文件中并加上 `IF NOT EXISTS` 幂等保护：

```sql
-- 每条存活逻辑 note 的当前 head。
CREATE TABLE memory_head (
    scope_key             TEXT NOT NULL,
    namespace             TEXT NOT NULL,
    path                  TEXT NOT NULL,
    note_id               TEXT NOT NULL,
    latest_revision_oid   TEXT NOT NULL,
    live_revision_oid     TEXT,
    latest_action         TEXT NOT NULL,
    latest_review_state   TEXT NOT NULL CHECK (latest_review_state IN ('draft','confirmed','quarantined','revoked','superseded','forgotten')),
    kind                  TEXT NOT NULL,
    lifecycle             TEXT NOT NULL,
    confidence            TEXT NOT NULL,
    trust                 TEXT NOT NULL,
    sensitivity           TEXT NOT NULL,
    visibility            TEXT NOT NULL,
    acl_policy_id         TEXT NOT NULL,
    valid_from            TEXT,
    valid_until           TEXT,
    effective_from_commit TEXT,
    effective_until_commit TEXT,
    expires_at            TEXT,
    rank_hint             INTEGER NOT NULL DEFAULT 0,
    last_event_seq        INTEGER NOT NULL,
    updated_at            TEXT NOT NULL,
    PRIMARY KEY (scope_key, namespace, path, note_id)
);
CREATE INDEX idx_memory_head_lookup
    ON memory_head(scope_key, namespace, path, latest_review_state);
CREATE INDEX idx_memory_head_path_prefix
    ON memory_head(scope_key, namespace, path);

-- 每条路径的当前聚合。这是 summarize()、prompt 注入、
-- 以及分类法下钻的快路径。
CREATE TABLE memory_path_summary (
    scope_key             TEXT NOT NULL,
    namespace             TEXT NOT NULL,
    path                  TEXT NOT NULL,
    confirmed_count       INTEGER NOT NULL DEFAULT 0,
    quarantined_count     INTEGER NOT NULL DEFAULT 0,
    child_count           INTEGER NOT NULL DEFAULT 0,
    prefix_count          INTEGER NOT NULL DEFAULT 0,
    preview               TEXT NOT NULL DEFAULT '',
    last_changed_at       TEXT NOT NULL,
    PRIMARY KEY (scope_key, namespace, path)
);
CREATE INDEX idx_memory_path_summary_prefix
    ON memory_path_summary(scope_key, namespace, path);

-- 反向索引：note_id -> head 行，用于 O(1) 回答「这条 note 在哪里？」。
CREATE TABLE memory_note_index (
    note_id               TEXT PRIMARY KEY,
    scope_key             TEXT NOT NULL,
    namespace             TEXT NOT NULL,
    path                  TEXT NOT NULL,
    kind                  TEXT NOT NULL,
    lifecycle             TEXT NOT NULL,
    review_state          TEXT NOT NULL,
    confidence            TEXT NOT NULL,
    trust                 TEXT NOT NULL,
    sensitivity           TEXT NOT NULL,
    visibility            TEXT NOT NULL,
    acl_policy_id         TEXT NOT NULL,
    origin                TEXT NOT NULL,
    idempotency_key       TEXT NOT NULL,
    created_at            TEXT NOT NULL
);
-- 幂等键在 (scope, namespace) 内唯一；存储创建版本的键（§4.1.1）。
CREATE UNIQUE INDEX idx_memory_note_idempotency
    ON memory_note_index(scope_key, namespace, idempotency_key);

-- revision 级 provenance / 影响面索引；可由每个 MemoryNote.compile_record 重建。
CREATE TABLE memory_revision_index (
    revision_oid          TEXT PRIMARY KEY,
    note_id               TEXT NOT NULL,
    scope_key             TEXT NOT NULL,
    namespace             TEXT NOT NULL,
    origin                TEXT NOT NULL,
    producer              TEXT NOT NULL,
    rules_version         INTEGER NOT NULL,
    prompt_version        TEXT,
    model_id              TEXT,
    policy_version        TEXT NOT NULL,
    input_fingerprints_json TEXT NOT NULL,
    created_at            TEXT NOT NULL
);
CREATE INDEX idx_memory_revision_producer
    ON memory_revision_index(producer, prompt_version, model_id, policy_version);

-- 派生的链接索引。历史真相是 MemoryNote.links。
CREATE TABLE memory_link_index (
    source_scope_key      TEXT NOT NULL,
    source_namespace      TEXT NOT NULL,
    source_note_id        TEXT NOT NULL,
    source_revision_oid   TEXT NOT NULL,
    target_note_id        TEXT NOT NULL,
    target_revision_oid   TEXT,
    link_kind             TEXT NOT NULL,
    source_path           TEXT NOT NULL,
    target_path           TEXT NOT NULL,
    evidence_refs_json    TEXT NOT NULL,
    valid_from            TEXT,
    valid_until           TEXT,
    PRIMARY KEY (source_revision_oid, target_note_id, link_kind)
);
CREATE INDEX idx_memory_link_source
    ON memory_link_index(source_scope_key, source_namespace, source_note_id);
CREATE INDEX idx_memory_link_target
    ON memory_link_index(source_scope_key, source_namespace, target_note_id, link_kind);

-- 实体 mention / alias 反向索引。历史真相是 MemoryNote.entities；
-- canonical_key 与 alias_key 均为经过规范化的 repository-local key。
CREATE TABLE memory_entity_index (
    scope_key             TEXT NOT NULL,
    namespace             TEXT NOT NULL,
    canonical_key         TEXT NOT NULL,
    alias_key             TEXT NOT NULL,
    note_id               TEXT NOT NULL,
    revision_oid          TEXT NOT NULL,
    mention_role          TEXT NOT NULL,
    display_name          TEXT NOT NULL,
    resolution_confidence TEXT NOT NULL,
    path                  TEXT NOT NULL,
    PRIMARY KEY (scope_key, namespace, canonical_key, alias_key, note_id, mention_role)
);
CREATE INDEX idx_memory_entity_alias
    ON memory_entity_index(scope_key, namespace, alias_key);

-- 分类法投影（可重建）。
CREATE TABLE memory_taxonomy_node (
    scope_key             TEXT NOT NULL,
    namespace             TEXT NOT NULL,
    path                  TEXT NOT NULL,
    parent_path           TEXT,
    is_seed               INTEGER NOT NULL,
    expanded_from         TEXT,
    note_count            INTEGER NOT NULL DEFAULT 0,
    prefix_count          INTEGER NOT NULL DEFAULT 0,
    last_classified_at    TEXT,
    PRIMARY KEY (scope_key, namespace, path)
);
CREATE INDEX idx_memory_taxonomy_parent
    ON memory_taxonomy_node(scope_key, namespace, parent_path);

-- 每个 memory ref 的已构建水位线。安全敏感读取要求
-- projected_ref_oid == current_ref_oid。
CREATE TABLE memory_projection_state (
    scope_key             TEXT PRIMARY KEY,
    projected_ref_oid     TEXT NOT NULL,
    last_event_seq        INTEGER NOT NULL,
    schema_version        INTEGER NOT NULL,
    policy_version        TEXT NOT NULL,
    rebuilt_at            TEXT NOT NULL
);

-- 本地访问统计，不是 Git 投影，不参与默认 deterministic ranking。
CREATE TABLE memory_access_stats (
    scope_key             TEXT NOT NULL,
    namespace             TEXT NOT NULL,
    note_id               TEXT NOT NULL,
    last_used_at          TEXT NOT NULL,
    use_count             INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (scope_key, namespace, note_id)
);

-- 可选：分类器缓存，带 TTL，键为
-- keyed_hmac(scope + namespace + redacted_content + taxonomy_version)。
CREATE TABLE memory_classifier_cache (
    cache_key             TEXT PRIMARY KEY,
    scope_key             TEXT NOT NULL,
    namespace             TEXT NOT NULL,
    taxonomy_version      TEXT NOT NULL,
    classifier_version    TEXT NOT NULL,
    policy_version        TEXT NOT NULL,
    suggested_path        TEXT NOT NULL,
    confidence            TEXT NOT NULL,
    created_at            TEXT NOT NULL,
    expires_at            TEXT NOT NULL
);

-- 可选：向量通道缓存（§8.7）。按内容哈希缓存 embedding；
-- 可整体丢弃，不参与 rebuild。
CREATE TABLE memory_embedding_cache (
    content_hash          TEXT PRIMARY KEY,
    embedding_json        TEXT NOT NULL,
    model_id              TEXT NOT NULL,
    embedding_version     TEXT NOT NULL,
    created_at            TEXT NOT NULL
);

-- 注入回执账本（§8.6）。注意：这是本地 append-only 审计账本，
-- **不是**投影——它记录读取时刻的选择，无法也无须从 Git 历史重建；
-- rebuild 不触碰它，保留策略负责有界修剪。
CREATE TABLE memory_context_receipt (
    receipt_id            TEXT PRIMARY KEY,
    emitted_at            TEXT NOT NULL,
    scope_key             TEXT NOT NULL,
    view_hash             TEXT NOT NULL,
    source_refs_json      TEXT NOT NULL,
    projection_watermarks_json TEXT NOT NULL,
    as_of_commit          TEXT,
    effective_at          TEXT NOT NULL,
    selector_version      TEXT NOT NULL,
    rules_version         INTEGER NOT NULL,
    index_version         TEXT NOT NULL,
    policy_version        TEXT NOT NULL,
    query_hmac            TEXT,
    token_budget          INTEGER NOT NULL,
    tokens_used           INTEGER NOT NULL,
    selected_json         TEXT NOT NULL,
    dropped_json          TEXT NOT NULL,
    bundle_hash           TEXT NOT NULL
);
CREATE INDEX idx_memory_receipt_time
    ON memory_context_receipt(emitted_at);
```

`memory_head`、`memory_path_summary`、`memory_note_index`、`memory_revision_index`、`memory_link_index`、`memory_entity_index`、`memory_taxonomy_node`、`memory_projection_state` 是可重建投影。`memory_classifier_cache` 与 `memory_embedding_cache`（§8.7）是可丢弃 cache；`memory_access_stats` 与 `memory_context_receipt` 是本地有界账本，不能从 Git 历史重建，`rebuild` 不触碰它们。删除账本会降低本地可观测性，但不能改变 live memory 语义。

查询实现必须始终带上 `scope_key` 与 `namespace`，禁止只按 `path` 做全局查询后在内存中过滤。跨 scope / namespace 的检索只能由显式 `--all-namespaces` 或策略允许的 scope fallback 触发，并且必须在结果中保留原始 `scope` 与 `namespace`，防止 prompt 注入时发生来源混淆。

`list_prefix` 与 `summarize` 不得执行无上限扫描。实现应使用规范化后的 path 前缀范围查询和 keyset pagination，并设置默认 `LIMIT`（建议 100 条 summary、50 条 note）与硬上限。因为 SQL `LIKE` 的转义与 collation 容易引入前缀越界，推荐存储 canonical `path_key` / `parent_path` 后做复合索引范围查询；复杂度表述统一为 O(log n + k)，其中 k 是有界返回量。

在访问模式上还有一条对齐约定值得明确：Memory 的投影表用 SeaORM entity 来访问（与同样可重建的 `ai_index_*` 投影一致），而不采用 `agent_session` / `agent_checkpoint` / `agent_usage_stats` 那种**故意**保持的 raw-SQL、无 entity 风格。原因在于：`agent_*` 那批表是外部捕获的独立账本，而 Memory 的这些表是 git 真源（`refs/libra/memory/...`）的可重建投影，本质与 `ai_index_*` 同类，因而对齐 `ai_index_*` 的 SeaORM 模式。账本例外是 `memory_access_stats` 与 `memory_context_receipt`（§8.6）：二者都不是可重建投影，沿用 raw-SQL 账本模式，不配 entity。

### 5.3 ClientStorage 分层

`MemoryNote` / `MemoryEvent` blob 应经 `ClientStorage` 写入，使 `object_index`、本地对象库和可选 durable tier 的行为与其它 AI 对象一致（见 [`workflow.md`](../../ai/workflow.md) 2026-04-29 的说明）。但“可分层”不等于“可无条件上传”：

- `Public` / `Internal` 仅在 repository storage policy 明确允许时进入远端 durable tier。
- `Confidential` 默认 local-only；只有加密-at-rest、密钥轮换、删除/retention 和远端 ACL 均有实现与测试后才能 opt-in。
- `SecretLike` 原文永不进入 ClientStorage；只能存脱敏摘要与受控引用。
- `LIBRA_STORAGE_THRESHOLD` 只决定对象的存储层级 / cache 行为，不是 LFS 路由，也不能绕过 sensitivity policy。

因此 Memory writer 必须在 `ClientStorage::put` 之前完成 sensitivity gate，并验证所有新 OID 已按预期登记进 `object_index`。远端写入失败不得导致 ref 指向接收端无法读取的对象；commit/ref 更新只能在所需对象持久化成功后发生。

### 5.4 远端发布与传输边界

第一版所有 `refs/libra/memory/*` 都是 **local-only**，不会进入默认 push/fetch/clone，也不提供 `memory push`。这里的 `Repo` 表示同一仓库 / 关联 worktree 的本地共享 scope，不表示团队远端可见。原因是普通 Git ref 传输只有并发保护，没有 record-level ACL、redaction manifest、revocation propagation 或 remote principal authorization。

未来若增加团队 Memory publication，必须另行定义类似 `mainline.md` ML-01 的 allow-list manifest、隔离 tracking ref、ingress validation、lease、visibility/trust/sensitivity policy 与 tombstone 传播；不能直接 mirror 本地 memory ref。portable sealed intent / pin 仍由 `refs/libra/intent-team` 负责，Memory 不复制其 transport stack。该约束也意味着 `Confidential`、actor-private note、receipt、access stats 和原始 compile input HMAC 永不进入团队 publication。

### 5.5 人类可读的物化投影

`libra memory materialize` 将当前 `ResolvedMemoryView` 生成到默认目录 `.libra/views/memory/<view-hash>/`，或调用方显式指定的输出目录。它借鉴 perstate 的 Markdown/YAML 实体关系图，但严格保持为**只读、可删除、可重建的投影**；JSON blob、事件与 memory ref 仍是唯一真源。

第一版稳定布局为：

```text
manifest.json
notes/<encoded-scope>/<encoded-namespace>/<encoded-path>/<note-id>.md
entities/<encoded-canonical-key>/index.md
relations/<encoded-link-kind>/<source-note-id>--<target-note-id>.md
taxonomy/<encoded-scope>/<encoded-namespace>.md
```

- `manifest.json` 记录 `view_hash`、code anchor、每层 source ref OID / projection watermark、policy snapshot hash、materializer schema / renderer version 与截断上限；不写 wall-clock 生成时间，使同一 view 与版本产生逐字节稳定的 manifest。缺少 manifest、OID 不可解析或 watermark 不一致的目录一律视为 stale，不得作为 recall 输入。
- note Markdown 的 frontmatter 只包含已授权、已脱敏的 envelope 字段：scope、namespace、path、note/revision ID、review state、trust、sensitivity、有效期与 evidence/link 标识；正文使用同一 redacted body。`SecretLike` 原文、query、principal PII、compile HMAC 与 receipt 不得被物化。
- entity / relation 文件由 `MemoryNote.entities`、`MemoryNote.links` 与对应索引确定性生成；它们不获得独立写入语义。用户直接编辑、删除或新增这些文件不得改变真源，也不得被下一次 materialize 静默摄入。
- 第一版不提供「目录编辑后直接 import」。未来若增加 `propose-from-view`，它只能产生 Draft proposal，并重新经过 `MemoryWriter` 的 redaction、CompileRecord、authorization、冲突和 CAS 流程。
- 重新物化默认拒绝覆盖含用户改动的输出目录。`--discard-view-edits` 只对 manifest 能证明属于当前 materializer、且位于默认 `.libra/views/memory/` 根下的托管目录生效；显式 `--output` 必须指向不存在或为空的目录，永不递归删除调用方指定的任意路径。任何丢弃都只影响可重建投影，不触碰 memory ref。
- materialize 必须有 note / relation / byte 上限与分页清单；所有 segment 复用 §5.1.1 编码，不依赖大小写敏感文件系统。

### 5.6 可移植导出 / 导入（A5，MEM-05）

沿 §0.0.4（letta `.af` / MemFS / skills）与 MEM-05，提供一条可移植的
导出 / 导入路径，让用户带走 Agent 人格 / 技能子集或与生态交换，但
不绑定单一 vendor、不自动双向同步：

- **导出格式**：第一版为 Libra 自有 `libra-bundle`（开放 JSON + 脱敏正文 +
  `CompileRecord` 摘要 + ACL / policy / compiler 版本 + index hints）；
  后续评估 `.af` 子集（identity / memory blocks / skills / settings 中
  与 Libra 兼容的部分，§0.0.4）与 MemFS 布局。
- **skill 投影**：`procedural.skills.*` 以只读方式投影到
  `.agents/skills`（对齐已有 `libra agent skill` 基础），不获得独立写入语义。
- **导入即私有 Draft**：导入一律生成新的私有 Draft，重新走 `MemoryWriter`
  的 redaction、`CompileRecord`、authorization、冲突与 CAS 全流程
  （§4.2.1）；导入包中的 `CompileRecord.origin = Import`，回指源 bundle。
- **非目标**：不完整兼容 Letta 云；不自动双向 sync 任意 GitHub memory repo。
- **验收**：至少一条「导出 → 清空 → 导入 → 召回仍命中」的往返测试 +
  兼容范围文档化（§16.5 MEM-05）。

外部 transcript 导入前置：多 runtime transcript 需先经 trajectory 式归一化
+ diagnostics（与 `plan-long.md` AG-ATTR 衔接），再进入上述导入流程。

## 6. 分类法

### 6.1 内置种子根

二进制内置了一套固定的种子分类树（seed taxonomy），分布在三个根下
（`procedural`、`semantic`、`episodic`），约 50–100 条路径，覆盖代码
agent 的常见场景。种子路径被标记为 `is_seed = true`，不可删除（但可以为空）。

### 6.2 路径文法

```text
path        := segment ("." segment)*
segment     := [a-z][a-z0-9-]* | "<" identifier ">"
identifier  := [A-Za-z0-9-]+
```

- 全小写，连字符分隔。
- `<...>` 段是动态段（例如 `episodic.runs.<run-id>.outcome`）。
  动态段不得出现在种子路径中。
- 最大深度：**5 段**。更深的路径被禁止——以保持召回 prompt 的简短。

### 6.3 迭代式扩展

当 LLM 分类器（§7.3）被要求安放某段没有任何现有路径覆盖的内容时，
它可以**提议**一个新的子段。接受规则沿用 memoir-ai 的
`LLMIterativeTaxonomy` 模式：

1. 提议必须挂在某个现有父节点之下。
2. 提议不得超过深度 5。
3. 提议被记录为一条 action 为 `TaxonomyExpanded` 的 `MemoryEvent`
   （作为 §4.2 已列出的元事件处理，存放在同一份事件日志中）。
4. 一旦被接受，新路径即成为分类树中的一等节点，后续写入可以直接将其作为目标。

记忆之间的交叉引用（memoir-ai 的 `related_keys`）以历史方式存储在
`MemoryNote.links` 中，并投影（Projection）到 `memory_link_index`。
链接种类（link kinds）：

- `sibling`：同一次写入被分类到多条路径。
- `supports`：本 note 强化或解释了另一条 note。
- `prerequisite`：使用本 note 前必须先满足或加载另一条 note；召回扩展仍受深度、数量与授权上限约束。
- `contradicts`：本 note 与另一条 live note 冲突，应触发隔离（quarantine）
  或人工解决（resolution）。
- `supersedes`：本 note 有意取代（supersede）另一条逻辑 note。

直接路径编辑（direct-path edit）通过「先取后并」（fetch-then-merge）保留
既有的 `sibling` 链接，与 memoir-ai 的编辑语义一致。分类器驱动的重写
（classifier-driven rewrite）则可以替换链接，因为此时分类器是在有意地对该
note 重新聚类。

### 6.4 实体消歧、语义重复与关系所有权

分层 path 回答「记忆放在哪里」，不等于稳定的实体身份。摄入一个带 `MemoryEntityMention` 的 proposal 时，compiler 必须在 principal / scope / namespace 过滤后查询 `memory_entity_index`，按 canonical key、规范化 display name 与 alias 产生以下确定性结果：`New` / `UniqueMatch` / `Ambiguous` / `MergeCandidate`。

- `UniqueMatch` 只复用 canonical key，不自动改写既有 note；新增来源仍形成新 revision 或新 note。
- `Ambiguous` 保持 Draft 并列出候选，必须由调用方或 reviewer 选择；禁止按最近更新时间或最高使用次数静默合并。
- `MergeCandidate` 是 proposal，不是状态转移。只有 `MemoryWriter` 完成证据、scope、ACL、validity 与冲突验证后，才能通过 revise / supersede / link 表达结果。
- `CompileRecord.idempotency_key` 只防止同一输入重复摄入，**不是语义去重**。来自不同 evidence 的近义陈述必须保留各自 provenance；compiler 可以建立 `supports` / `contradicts` 链接或提出 consolidation，不能把正文无痕拼接。
- 嵌入式 `MemoryLink` 的生命周期与 source revision 相同。若一条关系需要独立确认、有效期、撤销或取代，应创建一条结构化 semantic note，并让该 note 分别链接 subject / object；不得在 `memory_link_index` 上直接写状态。
- Phase C 的 `memory related` 仅在过滤后的 `memory_link_index` / `memory_entity_index` 上执行有界一跳或两跳查询，并对每个 target 重新授权。Phase E 的图数据库 Adapter 只能返回候选，不能改变上述语义。

## 7. 分类管线

一次写入请求是 `(content, optional_namespace, optional_path, scope,
kind?, lifecycle?, trust?, sensitivity?)`。分类负责补齐缺失的字段。

### 7.1 阶段 0 —— 价值过滤（worthiness）

memoir-ai 称之为「memory worthiness」。对一个代码 agent 而言，价值过滤
（worthiness）通常会排除：

- 寒暄、闲聊、瞬态状态（如「我这就帮你看一下」）。
- 对已经出现在 diff 中的代码的复述。
- 已经被 `Evidence` 捕获的工具错误消息。
- 密钥、token、凭据、私钥或高风险个人数据——除非存储的正文已经过编辑
  （redacted），且该 note 被标记为 `SecretLike` 使其无法被 prompt 注入。
- 没有绑定到已抓取来源（fetched source）或明确用户断言的外部网络声明。

价值过滤（worthiness）采用**确定性优先**（正则 / 启发式）、对边界情形
**回退到 LLM**（LLM-fallback）的策略。一次价值拒绝只在本地有界 intake
audit 中记录非敏感 reason code、producer 版本与脱敏输入 HMAC；它不创建
`MemoryNote` / `MemoryEvent`，避免把本应拒绝持久化的原文反而写进 Git 历史。

安全顺序不可颠倒：任何内容进入 LLM worthiness / classifier 之前，必须先完成本地 redaction、owner filtering、size cap 与来源标注。若内容被判定为 `Confidential` 或 `SecretLike`，默认不得发送给远端 LLM provider；只有在明确配置允许、provider 被标记为本地或受信任、且正文已脱敏后才可调用。分类失败时必须返回 draft + `unsorted` 路径或拒绝摄入，不能把未分类正文直接确认为 prompt-visible memory。

### 7.2 阶段 1 —— 模式分类器（离线）

如果调用方已经提供了 `path`，直接跳到验证（§7.4）。否则：

- 在 `memory_classifier_cache` 中查找
  `cache_key = sha256(scope || namespace || content || taxonomy_version)`。
  命中则返回缓存的建议。
- 对内容运行一个固定的模式匹配器（按每个顶层根分别播种的正则表）。
  一次高置信度（high-confidence）匹配会短路掉 LLM 调用。

这就是 memoir-ai 所记述的「1–5ms」快速路径。

### 7.3 阶段 2 —— LLM 分类器（带缓存）

未命中时，构建一条单独的 LLM prompt，其中包含：

- 查询内容。
- 分类树块（从 `memory_taxonomy_node` 渲染，附带每条路径的 note 计数和
  一个样例——与 memoir-ai 使用的形态一致）。
- 指令：挑选一条或多条具体的现有路径，或在某个现有父节点下提议一个新的
  子段。

输出为结构化 JSON：

```json
{
  "namespace": "default",
  "paths": ["procedural.coding.tests.command"],
  "kind": "procedural",
  "lifecycle": "replacement",
  "confidence": "high",
  "trust": "repo_evidence",
  "sensitivity": "internal",
  "propose_new": null,
  "rationale": "Command preference is reusable across runs."
}
```

多路径结果会创建 sibling 链接的 note（§6.3）。结果以分类树版本
（taxonomy version）为键、带 TTL 地缓存。

LLM provider 沿用 libra 既有的 provider 矩阵
（`gemini` / `openai` / `anthropic` / `deepseek` / `kimi` / `zhipu` /
`ollama`）。默认模型可通过 `LIBRA_MEMORY_CLASSIFIER_MODEL` 配置；
推荐使用一个小而快的模型（Haiku 量级，或在隐私策略要求时使用本地小模型）。

LLM 输出必须按 schema 校验：未知 enum、未知 namespace、非法 path、超过路径数量上限、非 JSON、重复 path 或 `propose_new` 与 `paths` 同时违反策略时，全部 fail closed 并记录分类失败事件。不得把 LLM 生成的 `trust` 直接当作事实；`trust` 的最高等级只能由本地证据检查、用户确认或 repo evidence 推导得出。

分类器参与生产的每条 note，必须把本次调用的 prompt 模板版本、模型标识与 taxonomy version 写入 `CompileRecord`（§4.1.1）；`memory_classifier_cache` 的 `cache_key` 即幂等键的组成部分。缓存命中复用的建议同样要在编译记录中标注来源（cache hit），不得伪装成一次新的模型调用。

### 7.4 阶段 3 —— 路径验证与回退

- 调用方显式提供的 `path` 无效（深度 >5、未知根、静态槽位中出现动态段）时直接拒绝，绝不静默缩短或改写用户指定的语义位置。
- 分类器建议的 path 无效时可退到 `<root>.unsorted`，但 note 只能保持 `Draft`，并在本地 intake audit 记录稳定 reason code；它不能因 fallback 自动 confirmed。

路径验证必须使用 §6.2 的文法与 §5.1.1 的编码规则。动态段只能出现在已声明允许动态段的模板位置；若模板未知，动态值必须先映射到已有静态 bucket（例如 `episodic.runs.unsorted`）或作为 tag / evidence metadata 保存，不能任意扩展 taxonomy。

### 7.5 阶段 4 —— 冲突与可信度门

在一条 note 变为可进入 prompt 的（prompt-visible）之前：

1. 加载同一 `(scope, namespace, path)` 单元（cell）内的已确认 note。
2. 对于 `replacement`（覆盖式）note，检查新正文是否与某条现有 live 正文
   矛盾，以及哪一方拥有更强的证据。
3. 若冲突可由谱系（lineage）解决（`parents` 包含旧 revision），则确认新
   revision 并取代（supersede）旧的。
4. 若两侧都站得住脚、且证据上谁都不占优，则创建
   `MemoryEvent { action: Quarantined, reason_code }`，添加 `contradicts` 链接，
   并将两者都排除在 prompt 注入之外，直到 `libra memory resolve` 选定一个
   结果。
5. 若 `trust == ExternalUntrusted`，则在该 note 离开 `Draft` 之前，要求
   要么提供指向已抓取来源的 `EvidenceRef`，要么获得明确的人工确认。

6. 若 `sensitivity >= Confidential`，则默认只能保持 `Draft` 或进入 `private:<actor-ref>` namespace；除非命名空间策略显式允许，否则不得自动确认。`SecretLike` 永远不可 prompt-visible。

信任等级的排序固定为：`Verified > RepoEvidence > UserAsserted > Inferred > ExternalUntrusted`。自动确认门禁只能提升到输入证据可支持的等级，不能由 LLM 自述提升。

这正是 Memoria 风格的隔离（quarantine）与 Zep 风格的时间性真值处理
（temporal truth handling）进入 Libra 的切入点——而无需在基础设计中引入
图数据库。

#### 7.5.1 Trust Gate —— Draft → Confirmed 的显式晋升阶段（A4）

沿 §0.0.4（fava-trails Trust Gate）与 §0.2，`Draft → Confirmed` 不是自动
完成，而必须经过一个命名的 **Trust Gate** 阶段，把「确定性规则 + 可选 LLM
评审」统一为一条可审计的晋升路径：

- **输入**：待晋升 note（Draft）+ 当前 policy / namespace 策略 + 引用
  `EvidenceRef`（若有）+ `CompileRecord.policy_version`。
- **输出**：`pass` → `Confirmed`；`fail` → `Quarantined` 或保持 `Draft`
  （按 reason code）；`needs-human` → 保持 `Draft` 并标记待人工评审。
- **规则**：默认是确定性规则（§7.5 的冲突 / trust / sensitivity / expiry
  门）；可选 LLM reviewer 仅在 policy 明确允许且输入已脱敏时参与，
  **LLM 判定不得单独确认**——任何确认都必须落在确定性可复核的门之上。
- **记录**：gate 的输入（draft / policy / evidence）与输出
  （pass / fail / needs-human + reason code）写入 `CompileRecord.policy_version`
  与对应 `MemoryEvent` reason；gate 失败不泄漏私有原文（同 §13.1）。

自动确认（§11.5 auto-mode）也必须视为经过 Trust Gate：仅当确定性规则
全部通过（高置信、`trust >= RepoEvidence`、非 Confidential/SecretLike、
无冲突、policy 允许）时，自动模式才视为 `pass`。任何绕过 Trust Gate 直达
`Confirmed` 的路径都属于发布阻断。

## 8. 召回管线

Memory 暴露四种召回（recall）模式——与 memoir-ai 相同的拆分方式，外加
Libra 特有的直接 get 与调用方驱动的原语。

### 8.1 直接路径 get（无 LLM）

```rust
memory.get(scope, namespace, "procedural.coding.tabs") -> Vec<MemoryRecord>
memory.get_note(note_id) -> Option<MemoryRecord>
memory.list_prefix(scope, namespace, "procedural.coding.") -> Vec<MemoryPathSummary>
```

经由 SQLite 的 `memory_head` 与 `memory_path_summary` 达到 O(log n + k)，其中 k 受 limit 约束。
一旦 agent 知道路径，这就是它最常发起的调用。

### 8.2 单阶段分类器召回（引擎内，1 次 LLM 调用）

适用于路径未知但延迟敏感的自由文本查询：

1. 从 `memory_path_summary` 渲染一个紧凑的分类树块。
2. 让 LLM 挑选至多 5 条具体路径并返回结构化 JSON。
3. 对挑中的路径执行直接 `get`。

这比分层召回（tiered recall）延迟更低，但在大型分类树上鲁棒性较弱。
它支撑 `memory recall --mode single`。

### 8.3 分层下钻（引擎内，2–3 次 LLM 调用）

适用于路径未知的自由文本查询：

1. 从 `memory_taxonomy_node.prefix_count` 构建一个 L1 直方图——无 LLM。
2. LLM 挑选 1–2 个 L1 桶（`procedural` vs `semantic` vs `episodic`）。
3. 在每个桶内，LLM 从一份聚焦列表中挑选 1–3 条 L2 / L3 路径。
4. 对挑中的路径执行直接 `get`；若 `lifecycle == Accretive`（累加式），
   则连同其直接子节点一并取出。

总预算：每次召回 ≤2 次 LLM 调用。这是 `memory recall` 的默认模式。

### 8.4 调用方驱动（Memory 内部无 LLM）

暴露两个无 LLM 的原语，供外层 agent 自行组合：

```rust
memory.summarize(scope, namespace, prefix, depth) -> Vec<MemoryPathSummary>
memory.get(scope, namespace, path) -> Vec<MemoryRecord>
```

`MemoryPathSummary` 携带路径、子路径、note 计数、隔离（quarantine）计数，
以及一段稳定的一句话预览。外层 agent（本身已是 LLM）负责挑选——并且能用上
记忆引擎所不具备的对话上下文。这正是 `libra code` 运行时应默认使用的方式。

### 8.5 Prompt 时注入

在 prompt 构建时，既有的 `with_memory_anchors()` 被扩展为 `with_memory(...)`：

1. 当已解析 scope 的 `codebase:onboard` 或 `project:onboard` 摘要新鲜时，
   纳入该紧凑摘要。
2. 纳入 `default` 中高置信度、已确认的 `procedural.*` 以及选定的
   `semantic.*` head，前提是其 scope 与当前分支 / worktree 匹配。「选定」
   意味着该 note 简短、足够新近，且未被取代、未过期、未被隔离、也非
   secret-like。
3. 对于 `episodic.*`，默认 deterministic profile 按「有效 commit anchor / recorded time / confidence / trust / 与当前任务的标签重叠度」排序前 K 个 head，K 取较小值（5–10）。访问次数只可用于显式的 adaptive profile，且必须冻结统计快照并写入 receipt，否则无法承诺重放。
4. 作为有预算约束的 `ProjectMemory` 与 `MemoryAnchor` 上下文段注入 prompt，
   上限为可配置的 token 天花板（默认 1.5k tokens）。

该注入渲染为稳定、对前缀缓存友好的块。顺序由固定 selector/render version、source ref OID、policy snapshot 和 code anchor 决定；wall clock 只先归一化为 receipt 中冻结的 `effective_at`。同一快照下不得因一次读取更新统计而改变下一次默认排序。

可进入 prompt 的（prompt-visible）note 必须展示 `path`、`namespace`、
`scope`、`confidence`、`trust` 以及一个简短的证据指针。agent 被告知：
记忆只是指引，当前的源文件 / 命令输出会覆盖陈旧的记忆。

### 8.6 注入回执（`ContextReceipt`，Phase C 硬性门槛）

每次 `with_memory(...)` 注入（§8.5）以及引擎内召回（§8.2 / §8.3）完成后，必须产出一份 `ContextReceipt`；写不出完整回执，该次注入 / 召回按失败处理（fail-closed）。直接路径 get（§8.1）与调用方驱动原语（§8.4）是确定性查询、不含引擎侧选择，不产回执。

| 字段 | 含义 |
|---|---|
| `receipt_id` | 回执标识 |
| `emitted_at` | 产出时间 |
| `selected` | 选中的 note `revision_oid` / path summary key 列表，每项附 reason code、稳定 score 分量与顺序 |
| `dropped` | 因预算 / 门禁被丢弃项及 reason code |
| `token_budget` / `tokens_used` | 预算与实际用量 |
| `as_of` | `ResolvedMemoryView.view_hash`、解析时所有参与 scope 的 memory ref OID、各自 projection watermark 与 code commit / full branch ref |
| `versions` | selector / rules（编译规则集）/ index（投影 schema）/ policy 版本 |
| `effective_at` | 选择时冻结的时间，用于 TTL / recency；重放不得读取当前 wall clock |
| `query_hmac` | 可选，本地密钥派生的查询 HMAC；不得保存 raw query 或可跨仓库关联的普通 hash |
| `bundle_hash` | 注入渲染块的规范化哈希 |

规则（承 §0.0.2 与 §0.0.11）：

- **同输入同选择可验证。** 固定所有参与 scope 的 source ref OID / projection watermark、code anchor、`effective_at`、query HMAC 对应的调用输入、同一版本组与预算，重放必须得到相同 selected IDs、顺序、reason codes 与 `bundle_hash`；`receipt_id`、`emitted_at` 等非确定字段不进入 canonical hash。缺失对象、policy 或 index snapshot 时返回 `stale / non-reproducible`，不静默 fallback。该承诺只覆盖选择和渲染输入，不承诺 provider 输出逐字节一致。
- **回执是本地审计账本，不是投影。** 存入本地 append-only 表 `memory_context_receipt`（§5.2）；它记录的是读取时刻的选择，无法也无须从 Git 历史重建，因此明确豁免于「删表可 rebuild」条款（§13.1），并按保留策略有界修剪。回执、query hash 与 selected IDs 可能泄露工作意图，默认 local-only、不进团队 ref；任何共享另做 visibility / retention / threat-model 审查。
- **单一 receipt 原语。** Rust 类型与 mainline ML-05 / ML-08 共用同一定义（§0.0.10）；本文与 mainline 各自的注入管线写同一张回执面，不得分叉出两种 schema。
- `memory inspect-injection` 从回执读取并重放展示，而非从当前投影反推。被预算丢弃的项直接记录在 receipt 的 `dropped` 中，不再为每次读取追加 `PromptTrimmed` 权威事件。

### 8.7 混合检索通道（Hybrid Retrieval Channels）

§8.1–§8.4 的召回原语之上，检索面由**确定性基线 + 可选增强通道**组成（实现 MEM-02 的检索面）。向量与图永远是可替换的候选层（§0.0.11），不改变 §8.6 的确定性承诺。

```text
query
  -> Channel 0: path prefix + BM25/FTS5（常开，确定性）
  -> Channel 1: vector（可选；sqlite-vec 或等价本地实现 + 本地 embedding）
  -> Channel 2: entity graph 1–2 hop（Phase E，§6.4 有界）
  -> fusion: RRF 固定 k + MMR/会话去重上限
  -> fail-closed: 无 embedding 配置或 provider 失败 -> 仅 Channel 0
```

- **Channel 0（常开）**：路径前缀（§6.2）+ BM25/FTS5 关键词检索；无任何 embedding 配置即可用、可测（memweave 的「零外部服务 + 纯关键词降级」与 agentmemory 的「BM25 always on」为同向证据）。
- **Channel 1（可选）**：向量相似度。embedding 必须可本地运行（llama.cpp / Ollama 等）；embedding 按内容哈希缓存（`memory_embedding_cache`，§5.2，可整体丢弃）。provider 缺失、失败或超时 → 自动回退 Channel 0；**不得**把不可验证的向量结果注入 prompt。
- **Channel 2（Phase E）**：§6.4 已定义的实体图有界一跳/两跳查询，只返回候选。
- **融合与去重**：RRF 融合使用固定 k（起点 k=60，agentmemory 口径）；MMR 或等价去重 + 会话多样化上限（起点 max 3/session），避免同一事实多 chunk 重复注入。
- **确定性边界**：融合排序、channel snapshot（哪些通道启用、各自版本与权重）必须冻结进 ContextReceipt（§8.6）；embedding 向量与索引**不**进入 canonical hash / `bundle_hash` / 回执。
- **adaptive profile**：`memory_access_stats.use_count` / `last_used_at` 只能用于离线训练、诊断或显式 adaptive profile（冻结统计快照并写入 receipt），不得改写 canonical `MemoryHead.rank_hint`（§18 开放问题 4）。
- **评测**：Channel 0 单独与 Channel 0+1 融合分别上报 recall/NDCG（§16.3），确保「无 embedding 配置仍可用」始终可验证。

## 9. 分支与版本

### 9.1 按分支隔离的记忆

`scope = Branch("main")` 的 note 存放在
`libra/memory/branch/<encoded-full-refname>`（git ref 全名写作
`refs/libra/memory/branch/<encoded-full-refname>`）。切换用户的工作分支
（通过 `libra switch`）会隐式切换被查询的作用域：

```text
libra switch experiment
   -> agent 从 libra/memory/branch/<encoded-refs-heads-experiment> 读取 Branch 作用域，
      并按显式 precedence 合并 refs/libra/memory/repo 的 Repo 作用域
```

这解决了 §2.1 中描述的「上下文污染」失效模式。

scope 解析顺序固定为 `Actor -> Worktree -> Branch -> Repo -> Global`，但这只是候选叠加顺序，不是权限提升：每层都先做 principal / namespace / trust / sensitivity 过滤。同一 replacement cell 在多个 scope 命中时使用更具体 scope；同级冲突进入 quarantine，不以“最后读取者胜”解决。detached HEAD、unborn branch、branch rename 和缺失 worktree id 必须有稳定诊断，不得静默回退到 Repo 后继续自动确认。

#### 9.1.1 branch 创建、fork 与 session 绑定

创建或切换代码 branch **不会**静默复制、合并或重新绑定 actor memory。默认行为是：新的 Branch scope 从空的 sibling ref 开始，查询仍可按 `ResolvedMemoryView` 读取 Repo / Global 共享层；确实对所有分支成立的事实应提升到 Repo scope，而不是依赖隐式 branch inheritance。

需要复制 branch-local state 时，使用显式语义 fork：

```bash
libra memory fork --from branch:<source-full-ref> --to branch:<target-full-ref> \
  [--all-confirmed | --select <note-id>...]
```

fork 在开始时冻结 source memory ref OID 与 source code commit，并由 `MemoryWriter` 执行：

1. 只读取 source view 中已授权、已确认、未过期且适用于冻结 code anchor 的 note；Draft / Quarantined / actor-private / SecretLike 默认不可复制。
2. 对每条被选择 note 在 target scope 创建新的 `note_id` / revision，`CompileRecord.origin = BranchFork`，`input_hashes` 与 `evidence_refs` 回指 source scope、source ref OID、source revision OID 与 fork code anchor。
3. 重新运行 target policy、ACL、sensitivity、validity、路径与冲突检查；第一版 fork 是 all-or-nothing batch，任何失败项都中止整批写入，并在 dry-run / result 中给出稳定 reason code，不能部分静默跳过。
4. 在 target ref 内分配新的 event_id / event_seq，并以一个有界 CAS batch 提交。source event_seq、review actor 或 ACL 不得原样复制。

这是一份冻结的语义复制，不是动态 overlay：fork 完成后 source 的新提交不会出现在 target，target 的后续写入也不会回流 source。重复执行同一 `(source ref OID, target scope, selected revisions, policy version)` 使用 fork idempotency key 返回既有结果。`memory status` 必须显示当前 code branch、target Branch ref 是否存在、最近 fork 来源与当前 view layer；它不提供独立于 `libra switch` 的「memory switch」，避免把 code applicability 与 actor identity 再次合并成 branch。

### 9.2 记忆的 log / diff / blame

```bash
libra memory log [path]                  # 影响该路径的提交
libra memory diff <rev1>..<rev2> [path]  # 两个记忆 revision 之间发生的变化
libra memory blame <path>                # 当前 head 由谁、于何时设定
```

这些都是 Libra 既有 `log` / `diff` / `blame` 命令之上的薄封装（thin shim），
作用域限定在 `refs/libra/memory/...`。

### 9.3 merge 与 rebase

Memory ref 的权威历史保持 first-parent 线性，因此语义 merge **不是**直接在 memory ref 上保留普通 Git merge commit。`libra memory merge <source-ref>` 读取 source/target 的共同祖先，执行三方语义合并、重新授权与重新分配 `event_seq`，最后以单父 commit 追加到 target；source provenance 写入 merge audit。冲突解决规则如下：

- **同一 `note_id` 谱系**：fast-forward 到后代 revision。
- **覆盖式生命周期、同一 cell、不同 `note_id`**：若两份 body
  兼容，则两者都保留；若相互矛盾，则隔离（quarantine）证据较弱的一方
  或两方，并要求 `libra memory resolve`。
  对生产级记忆而言，「时间戳最新者胜」并不足够安全。
- **累加式（accretive）生命周期**：合并各条目的并集，并按
  规范化 body 哈希 + 证据哈希去重。除非某条被显式撤销，
  否则双方各自的 note 都保留。
- **分类树（taxonomy）扩展**：仅当父路径仍存在、且新片段
  不与同级兄弟节点冲突时才合并；否则隔离该扩展事件，并将
  相关 note 保留在其先前有效的路径上。

### 9.4 跨分支 cherry-pick

`libra memory cherry-pick <rev>` 将一个记忆 revision 从一个
分支 ref 提取并应用到另一个分支 ref。当某个实验分支发现了
确实应当落到 `main` 上的真实不变式时，这很有用。

cherry-pick 不能复制原事件序号或绕过目标 namespace policy；它会生成新的 event_id / event_seq，保留 source ref/OID provenance，并重新执行 schema、ACL、sensitivity、evidence 与冲突检查。

## 10. 生命周期与价值过滤（worthiness）

### 10.1 创建

三个入口：

1. **显式（Explicit）** —— 从 CLI 执行 `libra memory remember "..."`，
   或从 MCP / agent 工具调用 `memory_remember`。
2. **由 anchor 提升（Promoted from anchor）** —— 在 thread 结束时，
   已确认（confirmed）且 `MemoryAnchorScope::Project` 的
   `MemoryAnchor` 会通过 TurnEnd / SessionEnd 集成被提升（§11.5）。
3. **从 ContextFrame 蒸馏（Distilled from ContextFrame）** —— 当某个 Run
   的 `ContextFrame` / Evidence 被本地规则识别为高信号、且关联了
   `MemoryAnchorKind::VerifiedFinding` 或等价验证证据时，agent 可以提议一次 Memory 写入。

在以上三种情况下，价值过滤（worthiness，§7.1）都会先行运行。

### 10.2 取代（Supersession）

在同一 `note_id` 上写入新 revision，产生新的 immutable blob 与 `Revised` 事件。**修订默认回到 `Draft`**，旧 confirmed revision 在新 revision 获得 `Confirmed` 前继续作为 live head；只有明确 policy 允许且同一原子批次完成验证/确认时，才可直接切换。
要写入一个应**替换**同一路径下既有 note 的新 note：

1. 写入新的 `MemoryNote`（新的 `note_id`）。
2. 先让新 note 通过 `Created -> Confirmed`；若未确认，不影响旧 live note。
3. 在同一 CAS commit 中向旧 note 追加 `Superseded`（`next_note_id = new`），并把 replacement cell 的 `live_revision_oid` 原子切换到新 `revision_oid`。

旧 note 仍可通过 `libra memory log` 查询，绝不删除。

### 10.3 撤销（Revocation）

```bash
libra memory revoke <path-or-note-id> --reason "..."
```

追加 `MemoryEvent { action: Revoked, reason_code }`。该 note 进入
终态并从 live projection 移除；**不得自动回退到较老 revision**，因为较老版本可能已被新证据否定。若要恢复旧内容，调用方必须显式创建新 revision、重新验证并确认。prompt 注入会跳过已撤销的 note。

### 10.4 修剪（Pruning）

修剪只清理**可丢弃 cache / 本地账本**，绝不重写 memory ref，也不改变由事件重建出的 live head。默认策略：

- `last_used_at` 早于 90 天且 `use_count <= 1` 的 `episodic.*` 访问统计、预览 cache 与可选二级索引可被清理。
- `memory_head` 作为当前语义投影不得按访问频率删除；否则 rebuild 会把“被修剪”的 head 无条件带回，形成前后不一致。

若产品需要让某条 live note 不再被默认召回，必须使用 `revoke`、`supersede`、`forget` 或 `expires_at`，而不是 cache prune。因此第一版删除 `memory revive` 命令；恢复终态内容使用 `remember/revise` 创建新历史并链接来源。

### 10.5 归并（Consolidation）

归并是 memoir-ai Stop-hook 捕获与 OpenAI 式布局归并的
定时（scheduled）对应物：

1. 读取某个作用域 / 命名空间窗口内近期的 `episodic.*` note、
   已确认的 `MemoryAnchor`，以及高信号的 `ContextFrame`。
2. 产出候选的 `semantic.*` 或 `procedural.*` note，其 body 紧凑，
   带有回指源 note 的 `evidence_refs`，以及 `links.supports` 边。
3. 将源 episodic note 标记为 `Consolidated`，而非撤销。
4. 除非策略允许自动确认，否则将归并产生的 note 保持在 `Draft` 状态。

这样既保留了原始 episode 以备审计，又防止 prompt 注入
逐渐沦为一份过时的事件流水账（incident log）。

**Working 层（A3，四层巩固）。** 沿 §0.0.4（agentmemory 四层巩固）与 §0.2 决策，
归并需要一条显式、有界的 **Working 摄入缓冲**：原始 observation 只在本地
有界 intake 缓冲中作为 compiler 输入（§7.1 的 intake audit），**不写入
`refs/libra/memory/*`，也不新增 note kind**——与「先编译，再使用」一致。
Working 层不是 `MemoryNote` 快照，只是 intake 到确认之间的中间态：

- 只保留经 redaction 的 observation 摘要 + 来源 + 脱敏输入 HMAC + TTL；
- TTL 到期或缓冲区超过字节上限时 auto-evict（丢弃，不产生 note）；
- 归并作业读取 Working 缓冲 + 近期 episode，产出候选 semantic/procedural note；
- Working 缓冲是本地有界账本（同 §5.2 的 `memory_access_stats` 模式），
  不参与 rebuild，可整体丢弃。

排期归并由 `libra memory consolidate`（§12）或 SessionEnd / 定时作业触发
（借鉴 letta `/sleeptime` 的排期巩固），**不在 hook 热路径静默改写真源**。
矛盾检测优先走确定性规则（body hash + evidence 重叠），LLM 仅增强。

### 10.6 投毒隔离（Poisoning Isolation）与再验证

记忆投毒（§0.0.11）需要一条**有审计**的处置闭环，区别于 ledgermind 式
无监督自主变异（§0.0.4 反例：不可审计，不采纳）：

- **触发**：矛盾骤增、低置信度集中、来源失效（`EvidenceRef` 不可解析）、
  或显式 `libra memory quarantine --reason poisoning` 批量隔离。
- **处置**：受影响 note 批量进入 `Quarantined`（不静默删除），
  按 `CompileRecord` 的 `producer` / `prompt_version` / `model_id` /
  `rules_version` 追溯受影响集合（§4.1.1 的批量定位能力）。
- **恢复**：`revalidate`（在固定 code version 上重跑确定性验证）或
  `recompile`（以新 producer/prompt 版本重生成），二者均产生新 Draft 并
  重走 §7.5 gate；无修复路径者保持 `Quarantined`。
- **纪律**：任何投毒处置必须可解释、可回滚、写入 event reason 与本地
  脱敏 audit；不静默改写、不物理删除（§10.7），不把 quarantine 当无成本排除阀。

高信任 note 的再验证（G13）：`trust = Verified / RepoEvidence` 是写入时快照，
不永久固化。对高信任 `procedural.*` / `semantic.repo.*` 绑定 `valid_until` 或
`revalidate_after`；代码 / 依赖 / 事实版本变化时重跑验证，失败则降级 `trust`
或 quarantine（§7.5 与 §13.1）。

### 10.7 隐私与遗忘（forgetting）

记忆有两种「类删除」操作，二者的保证不同：

- `revoke`（撤销）：将一个 note 从当前投影和 prompt 注入中移除，
  但保留其历史 body 以供审计。
- `forget`（遗忘）：针对法律或策略敏感内容，写入 `Forgotten` tombstone，使默认 CLI / MCP / prompt / projection 不再返回原 body，只显示已脱敏占位符和必要的审计元数据。历史 blob 仍可能存在于本地对象库、remote、backup 和 clone；只有未来另行设计的加密删除 / retention compaction 能提供更强保证。

`forget` 需要稳定 reason code 和本地审计理由。它不能因为被 release artifact 引用就拒绝隐私请求；实现应保留不可逆的“证据已删除/不可用”占位并报告受影响引用。`--break-evidence-link` 只能确认影响范围，不能恢复或继续暴露敏感正文。

## 11. Agent 运行时集成

记忆挂接到 Libra 既有的 agent 生命周期
（`src/internal/ai/hooks/event.rs`、`src/internal/ai/hooks/lifecycle.rs`），
**无需新增任何 hook 事件**。它锚定在 Libra 归一化（normalized）的
`LifecycleEventKind` 上——当前共 13 个变体：SessionStart / TurnStart /
ToolUse / ModelUpdate / Compaction / CompactionCompleted /
PermissionRequest / SourceEnabled / SourceDisabled / TurnEnd /
SessionEnd / SubagentStart / SubagentEnd（参见 `src/internal/ai/hooks/lifecycle.rs`）。该 enum 为 `#[non_exhaustive]`，Memory consumer 必须保留 unknown/future fallback，不能依赖固定变体数做数组索引或 wire 编码。各 provider 的原生 hook 名
被改写为对应的归一化 kind：UserPromptSubmit → TurnStart；
PreToolUse / PostToolUse → ToolUse（pre / post 由事件元数据区分，
而非各自独立的 kind）；Stop → TurnEnd；SessionStart / SessionEnd
保持不变。

记忆消费的是**归一化之后**的 `LifecycleEvent`，因此与平面（plane）无关：
无论该 turn 来自内部 `libra code` 的 AgentRuntime，还是来自
`docs/development/tracing/agent.md` 所描述的外部 observed-agent 捕获，
集成都同样适用。由生命周期事件触发的 memory 写入同样遵守
`docs/development/tracing/agent.md` AG-19 的「持久化前先编辑」
（redaction-before-persist）与「按 owner 过滤」（owner-filtering）纪律。

### 11.1 SessionStart

- 解析作用域：`(repo, current_branch, current_worktree, actor)`，构造并冻结 §3.4 的 `ResolvedMemoryView`；把 `view_hash`、各层 source ref OID / watermark 与排除原因写入本地 session attach audit。
- 对 git 仓库加载最新的 `codebase:onboard` 摘要，对非 git 文件夹
  加载 `project:onboard` 摘要。若已过时（stale），仅注入一条
  过时提示（staleness hint），并请 agent 在有用时再刷新。
- 积极（eagerly）加载少量 `default` 下已确认的 `procedural.*` 和
  高置信度（high-confidence）的 `semantic.*` head。
- 预热（warm）分类器缓存。
- 在本地有界 access audit 中记录 session attach；不得为一次读取推进 memory ref。

### 11.2 TurnStart（即既有的 UserPromptSubmit hook）

先比较当前 principal、full branch ref、worktree identity 与 code commit 是否仍匹配 session 的 `ResolvedMemoryView`。若发生变化，必须在任何 recall / injection / capture 之前重建 view 并记录 `refresh_reason`；重建失败时本 turn 的自动注入 fail-closed，不能继续使用上一分支的冻结 view。

若用户消息看起来像一条指令（「从现在起……」、「记住……」、
「别忘了 X」），则预先运行价值过滤；若被接受，便起草一次
Memory 写入，使其浮现到 agent 的工具调用空间中——其 UX 形态
与 memoir-ai 的 prompt-submit hook 相同，但**不自动提交**。

### 11.3 ToolUse（pre / post）

ToolUse 事件由其元数据区分 pre 阶段与 post 阶段：

- **pre 阶段（PreToolUse）**：若即将运行的工具在 Memory 中有已知
  不变式（如 `procedural.shell.never-rm-rf-root` 等），则将其作为
  一条忠告（advisory）浮现在工具描述中。
- **post 阶段（PostToolUse）**：若工具产出了被标记为
  高信号且可验证的新 `Evidence`，则只 enqueue 蒸馏候选；`VerifiedFinding`
  是现有 `MemoryAnchorKind`，不是 `ContextFrameKind` 或 `Evidence` 的通用字段。

### 11.4 onboarding 刷新

memoir-ai 插件将「用户事实」与「代码库快照」分离开来；
Libra 应当照做：

- `libra memory onboard --namespace codebase:onboard` 执行一次
  冷扫描（cold scan）：顶层目录、README / AGENTS / CLAUDE 文件、
  包清单（package manifest）、workflow，以及近期提交摘要。它用
  `-p` 写入确定性路径，因此无需任何 LLM 分类器。
- 热刷新（warm refresh）将当前提交与上次 onboard 的提交作比较，
  仅重写受影响的 `semantic.repo.*`、`procedural.repo.*` 和
  `episodic.commits.*` 路径。
- 元数据级（meta-only）刷新仅在提交未移动时更新
  `semantic.repo.onboard.last-refresh`。
- 非 git 文件夹改用 `project:onboard`，并以文件系统快照哈希
  取代分支 / 提交元数据。单一快照哈希只能判断「变了」、不能定位
  「哪里变了」——`project:onboard` 应对目录建一棵文件 / 目录级
  Merkle 树，刷新时只遍历哈希不同的子树、只重写受影响的路径，
  使非 git 目录获得与 warm refresh 同级的增量粒度。对 Libra / git
  仓库这一能力是免费的：commit tree 本身就是 Merkle 树，warm
  refresh 的 tree diff 即为此算法（Cursor 的代码库索引采用同款
  增量同步，见 §18 开放问题 4 所引分析文章）。

### 11.5 SessionEnd / TurnEnd

- 对每个已确认且 `MemoryAnchorScope::Project` 的 `MemoryAnchor`，
  enqueue 一次 Memory Draft 提议。
- 对对话尾部（最后 2 个 turn）运行价值过滤，并为被接受的事实
   创建草稿（draft）候选。
- 交互模式（interactive mode）将候选以「memorize?」提示的形式浮现，
  未经用户批准不会确认。
- 自动模式（auto-mode）仅在以下条件全部满足时才可确认：分类器
  置信度为 `High`、可信度（trust）至少为 `RepoEvidence`、敏感度
  （sensitivity）不为 `Confidential` 或 `SecretLike`、未检测到冲突，
   且命名空间策略允许自动确认。

hook 热路径只允许 enqueue 有界候选任务，不允许同步调用远端 LLM、执行 consolidation、扫描大型 transcript 或争抢 memory ref。队列满、进程退出或超时时应丢弃自动候选并记录脱敏诊断，不能阻塞 SessionEnd / TurnEnd，也不能把失败候选升级为 confirmed。

### 11.6 与 MemoryAnchor 的关系

`MemoryAnchor`（既有于
`src/internal/ai/context_budget/memory_anchor.rs`）保持其当前角色，
即**活动 thread 的 prompt 注入槽位**。记忆则成为填充该槽位的
**持久化后备存储**：

- 在 SessionStart 时，`MemoryAnchor` 行从 `MemoryHead` 读取中
  播种（read-side 投影）。
- 在 SessionEnd 时，已确认的 anchor 回流为 Memory 写入
  （write-side 提升）。

两套系统可以复用 `MemoryAnchorConfidence` 的语义映射，但不得直接扩展现有
`MemoryAnchorReviewState` wire enum：该 enum 当前使用 `deny_unknown_fields` 的
session JSONL 模型，只有 Draft / Confirmed / Revoked / Superseded。Memory 的
`Quarantined` 保持为独立状态；投影到 anchor 时一律不生成活动 anchor。若未来
修改 anchor enum，必须作为独立 additive session-schema 变更，并验证旧 reader。

#### 11.6.1 规范性 wire 映射（B3）

`MemoryAnchor` ↔ `MemoryNote` 的字段与状态机映射固定如下（实现与测试必须
据此逐字段断言，不得各自猜测）：

| MemoryAnchor 字段 | MemoryNote / MemoryEvent 对应 | 方向 |
|---|---|---|
| `MemoryAnchorKind`（`VerifiedFinding` 等） | `MemoryNote.kind` / `links`（finding 关联） | read 播种时推断 |
| `MemoryAnchorScope`（`Thread` / `Project`） | `Project` → Memory 候选提升；`Thread` 不提升 | write 提升门 |
| `MemoryAnchorConfidence` | `MemoryNote.confidence`（Low / Medium / High） | 双向复用语义 |
| `MemoryAnchorReviewState`（`Draft` / `Confirmed` / `Revoked` / `Superseded`） | 映射到 `MemoryNote` review state；`Quarantined` **不**进 anchor wire enum | read 投影 |
| 单条规则正文 | `MemoryNote.body` | 播种 / 提升 |

- **read 播种（SessionStart）**：`MemoryHead.live_revision_oid` 的
  `Confirmed` / 非 `Quarantined` / 非 `SecretLike` head 投影为活动 anchor；
  `Quarantined` 一律不生成活动 anchor。
- **write 提升（SessionEnd）**：仅 `MemoryAnchorScope::Project` 且
  `Confirmed` 的 anchor 进入提升候选；`Thread` 作用域不提升。提升后经
  §7.5.1 Trust Gate 才能 `Confirmed`（§11.5）。
- **wire 约束**：任何 `Quarantined` 投影到 anchor 只写本地审计，不写入
  session JSONL 的 `MemoryAnchorReviewState`；`SecretLike` 原文与
  compile HMAC 永不进入 anchor wire。

### 11.7 prompt 预算

记忆 prompt 槽位的硬上限：可通过
`LIBRA_MEMORY_PROMPT_BUDGET_TOKENS` 配置（默认 1500）。当预算
溢出时，注入器按以下顺序丢弃：

1. 已过期的 note 与过时的 onboarding 提示。
2. 低置信度的 semantic / procedural note。
3. 与活动任务无关的较旧 episodic 发现（finding）。
4. 中等置信度的 semantic 事实。
5. 高置信度的 procedural 规则最后才保留，除非其本身就长于整个
   预算；此时它们会被替换为其路径摘要与一条 direct-get 提示。

丢弃行为记录在 `ContextReceipt.dropped` 与本地有界 audit 中，以便审计；它是读取决策，不追加权威 `MemoryEvent`，避免每个 turn 都推进 memory ref。

## 12. CLI 命令面

```text
libra memory remember <text> [-n <namespace>] [-p <path>] [--scope <s>] [--confidence <c>]
libra memory recall <query> [-n <namespace>] [--mode {direct|single|tiered|caller}] [--limit N]
libra memory summarize [-n <namespace>] [--prefix <p>] [--depth N]
libra memory status [--session <id>] [--json]    # 解析并展示当前 ResolvedMemoryView
libra memory get <path> [-n <namespace>]
libra memory get-note <note-id>
libra memory list [--prefix <p>] [-n <namespace>] [--scope <s>]
libra memory related <path-or-id> [--kind <k>] [--depth {1|2}] [--limit N]
libra memory confirm <path-or-id> [--reason <r>]
libra memory quarantine <path-or-id> --reason <r>
libra memory resolve <path> --choose <note-id> --reason <r>
libra memory revoke <path-or-id> --reason <r>
libra memory forget <path-or-id> --reason <r> [--break-evidence-link]
libra memory revise <path> <text>            # 在同一 note_id 上写入新的修订
libra memory move <old-path> <new-path>      # 取代旧路径 + 新写入
libra memory onboard [--namespace codebase:onboard|project:onboard] [--force]
libra memory log [<path>]
libra memory diff <rev1>..<rev2> [<path>]
libra memory blame <path>
libra memory branches                        # 列出 memory 的各个 ref
libra memory fork --from <scope> --to <scope> [--all-confirmed|--select <id>...]
libra memory rebuild                         # 从 ref 重建 SQLite 投影
libra memory materialize [--output <dir>] [--discard-view-edits]
libra memory show-taxonomy [--root <r>]
libra memory expand <parent-path> <new-segment> --rationale <r>
libra memory prune [--policy <p>] [--dry-run]
libra memory consolidate [--scope <s>] [-n <namespace>] [--window <w>] [--dry-run]   # 触发一次排期归并（§10.5）
libra memory inspect-injection [--last-run|--current]
```

约定：

- `--scope` 接受 `branch`、`repo`、`worktree`、`actor:<ref>`、`global`。默认值是当前 full branch ref；detached HEAD、unborn branch、非仓库目录或 scope identity 无法稳定解析时，mutating 命令必须要求显式 `--scope`，不得静默回退。所有 `--json` 输出显式报告解析后的 scope key、display name 与 source ref。
- `-n / --namespace` 默认为 `default`；只有当调用方传入
  `--all-namespaces` 时，recall 才会跨多个 namespace 搜索。
- 所有写入类命令都支持 `--dry-run` 与 `--json`，便于脚本化。
- `libra memory recall` 默认使用 `tiered` 模式。
- `status`、`related` 与 `materialize` 是只读命令：它们可以写本地有界 audit / 可重建视图，但不得推进 memory ref。`materialize --discard-view-edits` 只允许删除 manifest 能证明属于同一 materializer 的投影目录，不能接受任意路径或越过 `.libra/views/memory/` 默认根。
- `fork` 是 Phase D 的高影响语义写入，必须显式给出 source / target scope；source OID 在 dry-run 与实际执行之间发生变化时 CAS precondition 失败，要求重新预览，不能自动改用最新 source。

## 13. MCP 命令面

Memory 的 MCP transport 使用现有 **`libra code --stdio`**，实现位置仍在 `src/internal/ai/mcp/`。但 transport 可用不表示 Memory tool 已获准发布：[`code.md`](code.md) 已记录当前生产 `McpAuthorizer` 未安装时 allow-all、`tools/list` 和部分 call-site 尚未完整授权。以下工具只有在 C9 完成 default-deny authorizer、per-request principal threading、`tools/list` 与所有调用点覆盖，并通过 deny/needs-human 测试后才能注册；在此之前 Phase A/B/C 仅发布 CLI / 内部 Rust API，MCP `tools/list` 不得出现任何 `memory_*` 工具。

| 工具 | 用途 |
|---|---|
| `memory_remember` | 写入一条 memory；执行价值过滤（worthiness）与分类流水线 |
| `memory_recall` | 自由文本召回；支持 `mode` 参数 |
| `memory_get` | 按路径直接查找 |
| `memory_get_note` | 按 `note_id` 直接查找单条 note |
| `memory_list_prefix` | 廉价的前缀列举，用于调用方驱动（caller-driven）的检索 |
| `memory_summarize` | 不调用 LLM 的摘要（路径、子路径、note 计数、预览） |
| `memory_related` | 对已授权 note 执行有界的一跳 / 两跳关系查询；每个 target 重新鉴权 |
| `memory_confirm` | 确认一条 draft 或被隔离（quarantined）的 note |
| `memory_quarantine` | 隔离一条 path 或 note，并要求 reason |
| `memory_resolve` | 解决一条路径冲突 |
| `memory_revoke` | 撤销一条路径或 note |
| `memory_forget` | 对策略敏感的 note，编辑（redact）其可进入 prompt 的内容 |
| `memory_revise` | 在同一 `note_id` 上写入新的修订 |
| `memory_move` | 以新路径写入新 note，并 supersede 旧 note |
| `memory_log` | 某条路径的历史 |
| `memory_onboard` | 填充或刷新 `codebase:onboard` / `project:onboard` |
| `memory_expand` | 提议或确认一个新的分类法（taxonomy）分段 |
| `memory_prune` | 清理本地访问统计、预览 cache 与可选二级索引；不删除 live head |
| `memory_consolidate` | 触发一次排期归并（§10.5）；产出候选 semantic/procedural note 并回指源 episode |
| `memory_show_taxonomy` | 展示当前 taxonomy 投影 |
| `memory_inspect_injection` | 从 `ContextReceipt`（§8.6）重放最近一次或当前 prompt 注入 |

工具与 CLI 共用 request/response schema，但不要求所有 CLI 管理操作都暴露给 MCP。尤其 `forget`、taxonomy expansion、onboard、fork、merge、prune、consolidate 等高影响操作即使 C9 完成，也可由 policy 保持 CLI-only 或 `NeedsHuman`；`status` 与 `materialize` 会暴露本地 ref / 路径或写本地视图，第一版保持 CLI-only。“一一对应”不是兼容性要求。

MCP 参数 schema 必须与 CLI `--json` 输出共用同一 Rust 类型或同一 schema 测试。字段新增必须向后兼容；重命名、删除、改变默认值或改变 enum 字面量，都属于 public interface 变更，必须同步 `docs/commands/*`、`COMPATIBILITY.md` 与 compat tests。

**边界纪律。** 所有会产生变更的
memory 工具（memory_remember / memory_confirm / memory_quarantine /
memory_resolve / memory_revoke / memory_forget / memory_onboard /
memory_expand / memory_revise / memory_move / memory_prune / memory_consolidate）都必须经过生产已安装且 default-deny 的
**`McpAuthorizer`**（`src/internal/ai/mcp/authz.rs`）、per-request principal、tool policy、redaction
与 audit 流程。只读工具（memory_recall / memory_get / memory_get_note /
memory_list_prefix / memory_summarize / memory_related / memory_log / memory_show_taxonomy /
memory_inspect_injection）以只读、且受权限控制的方式暴露。

MCP stdio 独占 stdin/stdout：在 stdio 模式下，**不得**输出 banner、warning
或任何非 JSON-RPC 文本——memory 操作绝不能在该模式下 `println`。MCP
**不是** agent 控制面：memory 工具只是有界（bounded）的数据操作，
绝不等同于一次 agent turn 的 submit / respond / cancel。任何 `SecretLike`
内容，以及 `forget` 已编辑（redacted）的内容，绝不能未脱敏地越过 MCP 边界。

### 13.1 正确性、安全与兼容性不变量

以下不变量是实现与评审时的发布门槛：

- **确认前不可见。** `Draft`、`Quarantined`、`Revoked`、`Superseded`、`Forgotten`、`SecretLike` 与已过期 head 不得进入 prompt 注入；只读 API 返回非 live 项时必须显式标注状态并默认隐藏原 body。intake rejection 只存在于本地 audit，不是 note 状态。
- **证据不可伪造。** `EvidenceRef` 指向 Libra typed AI object、agent checkpoint、commit OID 或外部 URL 时，必须包含 type、object id、source plane 与可验证 hash / timestamp；无法解析的引用不能提升 trust。
- **无编译记录不落盘。** 任何入口产生的 `MemoryNote` 缺少完整 `CompileRecord`（§4.1.1）必须在写入事务第 1 步被拒绝；同幂等键的重复摄入不得产生第二条 note。这是 Phase A 的硬性退出门槛（§0.2）。
- **注入必须留回执。** 任何 prompt 注入或引擎内召回若未能写出完整 `ContextReceipt`（§8.6），该次操作按失败处理（fail-closed）；`inspect-injection` 只从回执重放，不从当前投影反推。回执默认 local-only，未经 visibility / retention 审查不得成批越过 MCP 边界。这是 Phase C 的硬性退出门槛（§0.2）。
- **写入只有一个 seam。** 所有 mutating Adapter 必须通过 `MemoryWriter`；任何绕过 CompileRecord、authorization、事件状态机、CAS 或投影事务而直接写 ref / tree / SQLite 的路径都属于发布阻断。worktree checkout 状态不能充当 writer lock。
- **session view 必须冻结且可解释。** recall / injection 使用的 `ResolvedMemoryView.view_hash`、source ref OID、watermark、policy 与 code anchor 必须进入 status / receipt；branch、worktree 或 principal 改变后不得继续使用旧 view。
- **OID 不自引用。** `MemoryNote` 正文不含 `revision_oid`；写入后 OID 只出现在 event / envelope / tree path / projection。读取必须同时验证 Git OID 与 canonical `content_digest`。
- **权威历史线性。** 每个 memory ref 只接受 first-parent 单父 commit 与严格递增 `event_seq`；跨 ref merge/cherry-pick 必须经语义 writer 重新追加，不能直接导入任意 Git merge DAG。
- **投影不可越权。** prompt 注入、MCP 只读返回与 CLI recall 都必须先解析 actor、repo、branch、worktree 和 namespace policy；`private:<actor-ref>` 只能被同一 actor 或授权 reviewer 读取。
- **投影必须新鲜。** 安全敏感读取要求 `memory_projection_state.projected_ref_oid` 等于当前 source ref；不一致时 fail-closed 或显式 rebuild，不能用 stale SQLite 行继续注入。
- **错误 fail loud。** Git ref 更新失败、投影 replay 失败、schema 版本不支持、MCP authz 拒绝、redaction 不确定、LLM 输出畸形、scope 解析失败与 namespace policy 缺失，都必须返回 actionable error 或保持 draft；不得静默降级为 confirmed memory。
- **兼容默认保守。** 新 namespace、新 action、新 enum 值对旧 reader 必须表现为“不注入 prompt、可保留历史、提示升级”，而不是 panic 或误当 confirmed。
- **可重放且不掩盖损坏。** 删除可重建投影后，`libra memory rebuild` 必须从 Git 历史确定性恢复同一 live head 集合。遇到未知 schema、缺失 blob、OID/digest 不符、event_seq 间隙或非法状态转移时，该 scope 的 watermark 不得推进越过损坏点；可以继续诊断其它独立 scope，但不能“跳过坏对象后宣称该 scope 已成功重建”。
- **资源有界。** 每次 recall、summarize、onboard refresh、consolidation 和 prompt injection 都必须有 note 数、字节数、token 数、LLM 调用数与 wall-clock timeout 上限。
- **物化与渲染不可信内容。** Markdown / entity / relation / graph / Web renderer 必须转义 frontmatter、默认禁用 raw HTML、净化 URL / Markdown 输出、设置严格 CSP，并使用随二进制固定或同源校验的资源；禁止直接把 memory body 拼入 `innerHTML`、从远端 CDN 加载执行脚本或在渲染时抓取 evidence URL。物化目录永远不是写入真源。
- **不可承诺物理删除。** `forget` 的第一版语义是投影级脱敏与 future compaction tombstone，不承诺从不可变 Git history、远端 object store、backup 或已发布 artifact 中立即物理删除。
- **投毒隔离有审计。** 检测到矛盾骤增、低置信度集中、来源失效（`EvidenceRef` 不可解析）或显式 `quarantine --reason poisoning` 时，批量 `Quarantined`（不静默删除），按 `CompileRecord` 的 `producer` / `prompt_version` / `model_id` / `rules_version` 追溯受影响 note 集合；处置经 `revalidate`（固定 code version 重跑确定性验证）或 `recompile`（新 producer/prompt 版本重生成）恢复，二者均产生新 Draft 重走 gate，无修复路径者保持 `Quarantined`。任何投毒处置必须可解释、可回滚、写入 event reason；拒绝 ledgermind 式无监督自主变异（§0.0.4 反例）。
- **高信任可再验证。** `trust = Verified / RepoEvidence` 是写入时快照而非永久固化；对高信任 `procedural.*` / `semantic.repo.*` 绑定 `valid_until` 或 `revalidate_after`，代码 / 依赖 / 事实版本变化时重跑验证，失败则降级 `trust` 或 quarantine（§7.5）。
- **读取不改真源。** prompt selection、session attach、usage count、trim 与 inspect 只写本地账本；读取不得推进 memory ref，也不得因 telemetry 写失败改变已选择内容。若要求 receipt 为硬门槛，应先写 receipt 再把 bundle 交给 caller。

## 14. 数据库 Schema 新增

Memory 的表通过**版本化迁移**引入，而非追加进 bootstrap 初始化脚本：新建
`sql/migrations/YYYYMMDDNN_memory.sql`（前向 DDL 须幂等，统一用
`CREATE TABLE IF NOT EXISTS …`，并可配套一份 `*_down.sql` 回滚脚本）。这与外部
agent 捕获使用的 `2026050303_agent_capture.sql` 迁移属于同一模式，**不**将这些
表追加进 `sql/sqlite_20260309_init.sql`。

迁移中创建以下表：

- `memory_head`（§5.2）
- `memory_path_summary`（§5.2）
- `memory_note_index`（§5.2）
- `memory_revision_index`（§5.2）
- `memory_link_index`（§5.2）
- `memory_entity_index`（§5.2）
- `memory_taxonomy_node`（§5.2）
- `memory_projection_state`（§5.2）
- `memory_classifier_cache`（§5.2，可选，带 TTL）
- `memory_embedding_cache`（§8.7，可选向量通道缓存，可丢弃）
- `memory_access_stats`（§5.2，本地账本，不参与 rebuild）
- `memory_context_receipt`（§8.6，账本类：append-only、豁免 rebuild、按保留策略有界修剪）

只有 `memory_head`、`memory_path_summary`、`memory_note_index`、`memory_revision_index`、`memory_link_index`、`memory_entity_index`、`memory_taxonomy_node`、`memory_projection_state` 可由 `libra memory rebuild` 从 `refs/libra/memory/...` 重建。classifier cache 与 embedding cache 可直接丢弃；access stats 与 receipt 是本地账本，不参与 rebuild。

值得一提的是模式选择上的对比：Memory 的投影表用 SeaORM entity 建模（与同样可
重建的 `ai_index_*` 投影同模式），而外部捕获的 `agent_session` /
`agent_checkpoint` / `agent_usage_stats` 是**故意**采用 raw-SQL、不配 entity 的。
Memory 之所以对齐 `ai_index_*` 的 SeaORM 模式，是因为它的表本身就是 git 真源的
可重建投影。`memory_access_stats` 与 `memory_context_receipt` 是例外：二者与
`agent_*` 同为本地账本，沿 raw-SQL 账本模式访问（§5.2、§8.6）。

## 15. 分阶段路线图

### Phase A — 可审计的存储（1–2 周）

- 在 `src/internal/ai/memory/` 中定义 `MemoryNote` / `MemoryEvent`
  的 Rust 类型。
- 建立唯一的 `MemoryWriter` Module 与 Adapter contract；Phase A 的 CLI 写入只通过该 seam，writer 内部完成对象持久化、事件序号、CAS 与投影事务。
- 对 repo memory ref（`refs/libra/memory/repo`）的写入 / 读取；每个 commit 单父、`event_seq` 连续、CAS 有界重试。
- SQLite 投影（§5.2）+ Sea-ORM entity：
  `memory_head`、`memory_path_summary`、`memory_note_index`、
  `memory_revision_index`、`memory_link_index`、`memory_entity_index`、
  `memory_taxonomy_node`、`memory_projection_state`。
- `libra memory remember / get / get-note / list / summarize / log /
  blame / rebuild / status / materialize / consolidate`；`status` 展示 `ResolvedMemoryView`，
  `materialize` 生成 §5.5 的只读、脱敏 Markdown 投影，`consolidate` 触发一次排期归并（§10.5）。
- 不引入分类器——调用方必须自行提供 `namespace` 与 `path`。
- `CompileRecord` 类型与写入侧强制校验（§4.1.1）：所有入口必须携带
  编译记录，幂等键在 `(scope, namespace)` 内去重。

退出门槛：所有 Phase A 写入 Adapter 都无法绕过 `MemoryWriter`；路径聚合与 `memory status` 可用；物化投影对同一 `view_hash` 产生相同 manifest / 文件集合，且不可见正文不落盘、raw HTML 默认禁用、frontmatter 与 URL 经转义 / 净化；投影重建在语义行、watermark、排序与 canonical digest 上稳定；OID 无自引用；损坏点阻止对应 scope watermark 前进；**每条已写入 note 都携带完整编译记录，
缺失即拒绝写入，同幂等键重复摄入不产生新 note——硬性门槛，见 §0.2**。

### Phase B — 摄入安全（1–2 周）

- 价值过滤（worthiness）、secret / PII 的 redaction、敏感度（sensitivity）
  与可信度（trust）分类。
- 评审状态：`Draft`、`Confirmed`、`Quarantined`、`Revoked`、
  `Superseded`、`Forgotten`。
- 冲突检测与 `libra memory resolve`。
- **Working 摄入缓冲（A3，§10.5）**：本地有界 intake 缓冲（只读 observation 摘要 + TTL / auto-evict），作为 compiler 输入，不写入 ref。
- **Trust Gate（A4，§7.5.1）**：`Draft → Confirmed` 的显式晋升阶段（确定性规则默认 + 可选 LLM reviewer），gate 输入/输出写入 `CompileRecord` 与 event reason。
- `MemoryEntityMention` 的 canonical key / alias 规范化，`New` / `UniqueMatch` /
  `Ambiguous` / `MergeCandidate` 决策与 Draft-only 语义重复 proposal；不得把
  idempotency 当作语义去重。
- 带「编辑后投影」（redacted projection）语义的 `forget` API。

退出门槛：任何未经评审、形似 secret、外部不可信、或处于隔离（quarantine）
状态的 note，都不能进入 prompt 注入；entity alias 歧义不能触发静默合并；
Working 缓冲有 TTL / auto-evict；任何 Draft→Confirmed 都经 Trust Gate（§7.5.1）。

### Phase C — 分类、召回与注入（2–3 周）

- 带缓存与多路径同级链接（sibling link）的「模式 + LLM」分类器。
- direct、single-stage、tiered、以及调用方驱动（caller-driven）四种检索。
- 基于 `memory_link_index` / `memory_entity_index` 的 `memory related` 有界一跳 /
  两跳查询；授权先于图扩展，每个 target 重新鉴权。
- prompt 期注入（§11.7），使用 `ProjectMemory` 与
  `MemoryAnchor` 预算分段。
- `ContextReceipt` 产出与 `memory_context_receipt` 账本（§8.6）：
  注入与引擎内召回写不出完整回执即按失败处理。
- 用于可观测性的 `libra memory inspect-injection`（从回执重放）与
  **`libra code --debug-memory`（G14，Phase C 硬交付）**：每 turn 逐字打印
  被注入的 memory 槽位。
- **投毒隔离处置（G12，§10.6）**：矛盾骤增 / 低置信集中 / 来源失效 /
  显式 `quarantine --reason poisoning` 时批量隔离，按 `CompileRecord` 追溯，
  经 revalidate / recompile 恢复或保持 quarantine。

退出门槛：召回具备分阶段计时元数据、确定性的回退路径，以及针对畸形
LLM 输出的测试固件（fixture）；**每次注入产出完整 ContextReceipt，
`inspect-injection` 从回执重放，固定 `as_of` 快照重放选择必须得到相同
selected 集合与 bundle hash，缺失快照 fail loud；`--debug-memory` 逐 turn
可用并与 receipt 对应——硬性门槛，见 §0.2**。

MCP 不属于 Phase C 的无条件交付。只有 `tracing/code.md` C9 完成生产 default-deny authorizer、principal threading、`tools/list` 和全部 call-site 覆盖后，才可增加一个独立的 **Phase C-MCP** slice；否则 `memory_*` 工具保持未注册。Phase C-MCP 的退出门槛必须包含“未安装 authorizer 时 server fail closed”，不能沿用当前 no-handler allow-all 行为。

### Phase D — onboarding、归并（consolidation）与分支操作（2 周）

- `codebase:onboard` 与 `project:onboard` 的 cold / warm / meta-only
  刷新。
- SessionEnd 的 draft 捕获，以及定时归并（consolidation）（§10.5）。
- 按 branch / worktree / actor 的 sibling ref（§9.1），含 branch rename / detached HEAD / principal identity 处理。
- 显式 `memory fork`（§9.1.1）：冻结 source ref OID，使用
  `CompileRecord.origin = BranchFork` 语义复制到 target scope；默认不随代码
  branch 创建隐式复制，也不提供独立 memory switch。
- memory 的语义 merge / cherry-pick（§9.3）：目标 ref 保持单父线性，重分配 event id / seq。
- **高信任再验证（G13，§10.6 / §7.5）**：高信任 `procedural.*` / `semantic.repo.*`
  绑定 `valid_until` / `revalidate_after`，代码 / 依赖变化时重跑验证，失败降级或 quarantine。
- **可移植导出 / 导入（A5，§5.6）**：`libra-bundle` 导出 + `.af` 子集评估 +
  skills 只读投影；导入即私有 Draft 重走全流程；trajectory 归一化作外部导入前置。

退出门槛：一个特性分支可以显式 fork 一份冻结、可追溯的 branch memory，携带
自己的 onboarding 与后续 delta，再将选定 note cherry-pick 或 merge 进 `main`；
source 在 dry-run 后移动必须使 fork fail-closed，重复 fork 幂等。

上述周数只可作为最初估算，不能作为承诺。Phase A 同时涉及新 public CLI、对象 layout、CAS writer、迁移、投影和 compat tests，实际应按 contract / writer / projection / CLI-compat 四个可独立验证的 slice 交付；任一 slice 都必须保持未完成功能不可见或 fail-closed。

### Phase E — UI 与可选索引（后续）

- 在 `libra code` 中提供 Web UI，用于浏览分类树、路径摘要、链接、
  隔离项、prompt 注入与 diff。
- 可选的 embedding 索引，用于 ANN 召回。
- 可选的时序 / 实体图投影，用于回答 Zep 风格的多跳问题。
- 在对 Libra 有意义之处，与 memoir-ai 的 Claude Code 插件达成功能对等
  （slash 命令、statusline、UI 启动）。

退出门槛：UI 不使用第三方 CDN 执行脚本，不把 raw Markdown / frontmatter 直接
写入 `innerHTML`；恶意 HTML、`javascript:` URL、事件属性、超长图和越权节点 fixture
均被净化、截断或拒绝，CSP 与本地资源完整性测试通过。

## 16. 评测与验收（Evaluation & Acceptance）

本设计的所有长期完成判据都必须有可度量口径（对齐 `plan-long.md` MEM-01..05）。vendor 自报数字（agentmemory R@5 95.2%、memweave LongMemEval-S R@5 97.24%±0.12% 等）只提供场景与目标参考，**不**进入验收。

### 16.1 原则

1. **两层评测**：确定性探针（离线、无 LLM、CI 必跑）与 LLM 场景（nightly，`--features test-live-ai`）。
2. **评测只读**：使用独立 seed corpus 与冻结 view 快照；不修改权威 ref，不写 receipt 之外的本地账本。
3. **双口径**：官方 benchmark 标签为主（LongMemEval-S / BEAM），能力桶为辅助视角（Memoria `memory-ability-taxonomy.md` 的 6-bucket）。

### 16.2 能力桶与场景集

| 桶 | 覆盖能力 | Libra 场景示例 |
|---|---|---|
| Single-Session Grounding | 单会话事实/上下文提取 | `codebase:onboard` 摘要命中 |
| Preference Understanding | 用户偏好（非关键词命中） | `default` 用户约束 recall |
| Multi-Session Synthesis | 跨会话整合 | 巩固后 semantic note 覆盖多 session 事实 |
| Temporal State Tracking | 时间/顺序/状态演化 | `valid_from` / `valid_until` + commit anchor 过滤 |
| Knowledge Update & Conflict | 新旧知识、矛盾消解 | supersede / quarantine 后的 recall 结果 |
| Abstention & Constraint | 无证据拒答、遵守约束 | 无命中时不注入、不编造 |

Libra 专属场景（官方 benchmark 不覆盖）：branch/commit anchor 切换后 recall 变化；supersession 隐藏旧版；`forget` 后默认读取不可见；actor 隐私隔离；跨 worktree 边界；无 embedding 配置降级。

### 16.3 指标定义

| 指标 | 定义 | 门槛 |
|---|---|---|
| recall@k / NDCG@k | k=5、10；Channel 0 与 Channel 0+1 分别上报（§8.7） | Phase C 基线冻结后定目标 |
| 注入预算合规 | 注入 tokens ≤ `LIBRA_MEMORY_PROMPT_BUDGET_TOKENS` | 100% |
| 重放一致性 | 同 view 快照 → 同 selected IDs / 顺序 / `bundle_hash` | 100%（硬门槛） |
| 隐私泄漏 | `SecretLike` / `Confidential` 进入 prompt / 日志 / receipt 次数 | 0（硬门槛） |
| 召回延迟 | 引擎内召回 P99 | 有界 |

### 16.4 评测资产与 CI

- 资产：`tests/data/memory/`（seed corpus、scenario、期望结果 JSON）；确定性探针 target 建议 `tests/memory_eval.rs` 并按 `tests/INDEX.md` 登记。
- 确定性探针 offline CI 必跑；LLM 场景 nightly。
- 输出固定 JSON 报告（`--json`），与版本化基线对比，回归即失败。

### 16.5 验收门槛（对应 MEM）

| MEM | 门槛 |
|---|---|
| MEM-01 | 本地单仓记录/列出/删除 round-trip；秘密探针零泄漏；schema forward + 迁移测试；损坏数据 fail loud |
| MEM-02 | 无 embedding 配置召回可用且可测；注入不超预算；citation 可人工核验；与 LR-07 preflight 共享同一检索服务 |
| MEM-03 | 巩固/遗忘单测 + 集成测；晋升失败不泄漏私有原文；doctor 可报告记忆健康 |
| MEM-05 | 导出→清空→导入→召回命中；兼容范围文档化 |
| MEM-06 | 单写者赢（并发 claim 恰一成功）；移交闭环（A→B/`any` 注入 + ack）；TTL 过期不毒化、历史可审计；冲突声明触发隔离；协调条目可从 refs 重建且不绕过 `MemoryWriter` |

## 17. 验证计划

Memory 只有在配齐有针对性的回归覆盖后才发布：

- writer seam：分别从 CLI、hook、onboarding、consolidation 与 fork Adapter 发起
  proposal，断言均进入同一 `MemoryWriter` 校验路径；直接 ref/tree/SQLite 写入无
  可调用生产 seam，步骤 3/4/5 任一点故障都不会产生可注入的半提交状态。
- 投影重建：写入若干 note / event，删除全部 memory 投影表，再重建，
  断言摘要、head、实体 mention 与链接完全一致。
- 编译记录：各入口（显式 / anchor 提升 / frame 蒸馏 / 分类器 /
  consolidation / onboard / branch fork / import）写入的 note 均携带完整 `CompileRecord`；
  缺失、`origin` 与入口不符或 `input_hashes` 为空时 fail-closed；
  同幂等键重复摄入返回既有 note 且不追加新事件；可按 producer /
  prompt / model 版本批量检索受影响 note。
- 分支污染：创建互相冲突的、分支作用域（branch-scoped）的 memory，
  切换分支，断言 prompt 注入随分支一同改变。
- session view：冻结一个 `ResolvedMemoryView`，在同一 session 中切换 branch /
  worktree / principal，断言旧 `view_hash` 不再用于新注入、status 展示新的 layer
  OID / watermark，而历史 receipt 仍可按旧 view 重放。
- branch fork：冻结 source ref OID 后 dry-run，再让 source 前进，断言实际 fork
  因 precondition 失败；固定 source 重试时 target note 使用新 ID / event_seq、
  `origin = BranchFork`、保留 source revision provenance，重复执行保持幂等。
- 聚合：在同一路径下存入多条 note，断言 `get(path)` 返回全部已确认
  （confirmed）的 note，而 `get-note(id)` 恰好返回一条。
- 隔离：引入相互矛盾的覆盖式（replacement）note，断言召回与 prompt 注入
  都排除未解决的冲突。
- 实体消歧：构造相同 canonical key、alias 冲突与不同 evidence 的近义事实，
  断言 unique match 只复用实体键、ambiguous 保持 Draft、semantic duplicate 只产生
  proposal / link 而不无痕拼接正文。
- 隐私：让形似 token、形似私钥的字符串经过摄入流程，断言存储已被编辑
  （redacted）、且不进入 prompt 注入。
- 物化投影：相同 `view_hash` 两次生成同一 manifest / 文件集合；越权 note、
  SecretLike 原文、receipt 与 compile HMAC 不落盘；含用户修改的输出目录默认拒绝
  覆盖，`--discard-view-edits` 不能删除默认根之外的路径。
- 安全渲染：用 raw HTML、`javascript:` URL、事件属性、畸形 frontmatter、超长
  Markdown 和越权 relation 构造 fixture，断言 CLI materialize 与 Web renderer
  转义 / 净化 / 截断且不发起远端 evidence / CDN 请求。
- 检索健壮性：畸形 LLM 输出、`NONE`、未知 mode、空结果等情形，都必须
  明确报错（fail loud），或只返回计时类可观测信息，绝不能静默地注入
  任意 memory。
- prompt 预算：把 memory 填到溢出，断言高置信度的程序性（procedural）规则
  比低置信度或陈旧的 note 存活得更久。
- 注入回执：每次注入与引擎内召回都写出 `ContextReceipt`；固定 `as_of`
  快照重放选择得到相同 selected 集合与 bundle hash；缺失快照返回
  stale / non-reproducible；删除全部投影表并 rebuild 后回执账本不受
  影响、也不被重建；预算丢弃只进入 `ContextReceipt.dropped` 与按
  `receipt_id` 互链的本地 audit，不追加 `PromptTrimmed` 权威事件。
- onboarding：cold、warm 与 meta-only 刷新产出确定性的路径，且不会改写
  无关的 namespace。
- ref / path 安全：使用包含 `/`、`..`、大小写冲突、Unicode、控制字符和超长 segment 的 branch / namespace / actor 输入，断言编码可逆、跨平台不冲突，并且非法输入 fail-closed。
- 并发写入：两个 writer（包括复用同一 worktree 的两个 session）同时写同一
  `(scope, namespace, path)`，断言 correctness 不依赖 checkout lock，CAS ref update
  触发有界重试，最终历史可重放且不会丢失任一事件。
- schema 兼容：旧 reader 遇到未知 additive 字段时能跳过；遇到不支持的 `schema_version` 或未知 action / enum 时不注入 prompt，并输出升级诊断。
- MCP 边界：mutating memory 工具必须经过 `McpAuthorizer`；stdio 模式不得输出非 JSON-RPC 文本；`SecretLike` 与 `private:<actor-ref>` 不得越权返回。
- 资源上限：构造大量 namespace / path / note / entity / relation，断言 `recall`、`summarize`、`list_prefix`、`related`、`onboard`、`consolidate`、`fork`、`materialize` 与 prompt injection 均受各自适用的条目数、批大小、分页、字节数、timeout、LLM 调用预算与 token 预算限制。
- forget 合规语义：执行 `forget` 后，prompt 注入、MCP 默认读取与 `memory get` 默认读取只显示 redacted body；审计命令仍能解释 tombstone 与无法物理删除的历史边界。

## 18. 开放问题

1. **跨 worktree 可见性。** `libra worktree` 衍生出的关联 worktree 如今
   共享 `.libra/`。它们是否也应共享 memory？当前提议是：`Repo`
   作用域的共享，`Worktree` 作用域的不共享。这一点需要与
   [`libra-worktree-architecture.md`](../libra-worktree-architecture.md)
   做设计对齐。
2. **加密。** `procedural.review.merge-policy` 可能会捕获敏感策略。基础
   提议将 blob 与其他所有 Libra 对象一样以明文存盘，依赖文件系统权限来
   保护。后续可选方案：接入既有的 `LIBRA_STORAGE_*` 信封加密（envelope
   encryption）流水线。
3. **大体量 memory 的 LFS。** 较长的情节性（episodic）发现（例如一份事故
   复盘草稿）可能超出合理的内联大小。当某条 memory 正文超过
   `LIBRA_STORAGE_THRESHOLD` 时，复用 Libra 既有的 LFS 管道
   （`lfs_structs.rs`、`protocol/lfs_client.rs`）。
4. **adaptive profile（向量通道已由 §8.7 收敛；触发已明确）。** 混合检索的「可选向量 + 确定性回退」已定为一等设计；基于 `memory_access_stats.use_count` / `last_used_at` 的**离线训练 / 显式 adaptive profile**（Cursor 实践：统计成功任务中被反复访问的内容，再由 LLM 反推「什么本该更早浮现」，见 [How Does Cursor Index Your Codebase?](https://manthanguptaa.in/posts/how_cursor_index_your_codebase/)，逆向分析博文，数字为 Cursor 自报口径）。**触发信号与硬约束（G15）**：adaptive 仅可在显式 opt-in / 离线训练时启用；触发后必须冻结统计快照 OID/hash 并写入 `ContextReceipt`，不得改写 canonical `MemoryHead.rank_hint`；关闭时默认 deterministic ranking 不受 access stats 影响（§8.7）。
5. **跨仓库的 memory 联邦（federation）。** 当前不在范围内。一个在多个
   仓库间工作的用户，仍然是每个仓库一份独立的 memory 存储。
6. **prompt 注入可观测性（已升为 Phase C 硬交付）。** `libra code --debug-memory`
   标志每个 turn 逐字打印被注入的 memory 槽位，让人类能在每次工具调用前
   精确看到 agent 究竟「记得」什么。该标志已由开放问题升级为 Phase C 硬性
   退出门槛（与 §8.6 的 `ContextReceipt` / `inspect-injection` 并列，G14）。

## 19. 并行多 Agent 协调 Memory（MEM-06，设计提案）

前文 §1–§18 规范的 `Memory` 是**持久知识层**：agent 跨 run / thread / branch 记住并召回可复用事实。本节新增一个**相邻但正交**的能力——**协调 Memory（Coordination Memory）**：多个 Agent 在同一仓库中**并行执行开发工作**时，通过 Memory 通道相互通信与协调。它与持久知识共享同一套基础设施（`MemoryWriter` seam、`refs/libra/memory/*`、事件系统、CAS、Working 缓冲），但语义、生命周期与授权目标不同。

### 19.1 开发者问题

在 `libra code` / `worktree` 并行工作区中，多个 agent 同时编辑同一仓库时反复出现四类协调失效：

- **任务所有权冲突**：两个 agent 同时声称"我来改 `src/command/status.rs`"，导致重复劳动或互相覆盖。
- **移交（handoff）丢失**：一个 agent 完成阶段工作后，不知道把结果交给谁、下一步该谁做。
- **变更面冲突延迟发现**：agent 各自并行改到重叠文件，直到 merge 时才撞冲突，而不是尽早协调。
- **进度与同步点缺失**：没有可查询的"谁在做什么、做到哪一步、哪里是同步点"。

`CLAUDE.md` / 全局文件的方案不可行（与 §2.1 相同：上下文污染、token 租金、无版本化），且共享文件本身就有并发写冲突。协调需要的是**有界、可审计、可过期、单写者赢（CAS）**的通道，而不是另一个扁平大文件。

### 19.2 目标与非目标

**目标：**

- 在仓库内提供一个共享、有界、可审计的协调通道，让并行 agent 发布/订阅：**所有权声明（claim）**、**移交（handoff）**、**进度（progress）**、**冲突声明（conflict-declare）**、**同步点（sync-point）**。
- 复用现有 Memory 三层模型与 `MemoryWriter` 单一 seam，不新建第二套存储或第二套并发机制。
- 所有权声明用 CAS 保证**单写者赢**；协调条目带短 TTL 自动过期，防止陈旧声明毒化后续工作。
- 每个 agent 在 SessionStart 注入当前 `CoordinationView`（活跃声明、待处理移交、未解冲突、同步点），并在 TurnEnd 把协调变更写回。
- 协调变更默认进入 Working 缓冲（§10.5），仅在达到晋升门槛时巩固为持久 `episodic.*` 或 `procedural.*` note。

**非目标：**

- **不是消息总线 / 实时聊天。** 协调是**异步、有界、可过期**的写读，不承诺实时投递、不提供订阅推送、不是 agent 之间的自由文本 IM。
- **不是分布式锁替代。** `claim` 只协调工作所有权，不替代 `MemoryWriter` 的 ref CAS 或文件系统锁；真正写入冲突仍由 CAS / 冲突检测兜底。
- **不是持久团队知识。** 协调条目默认 ephemeral（短 TTL），不自动进入 `default` 持久知识；需要持久化的才经 Trust Gate 巩固（§7.5.1）。
- **不复制 mainline 的 intent-team publication。** 协调是仓库内共享，不是远端团队发布（§5.4 边界仍适用）。

### 19.3 数据模型：CoordinationNamespace 与协调条目

新增一个保留 namespace `coordination`，与 `default` / `codebase:onboard` / `project:onboard` / `private:*` 并列。其路径使用既有 §6.2 文法，根为 `coordination.*`：

```text
coordination.<task-id>.claims             # 任务所有权声明（replacement，单写者赢）
coordination.<task-id>.handoff            # 移交记录（accretive，追加）
coordination.<task-id>.progress           # 进度标记（replacement，覆盖）
coordination.<task-id>.sync-point         # 同步点（accretive，追加）
coordination.conflicts.<path-or-tag>      # 变更面冲突声明（accretive，追加）
```

协调条目复用 `MemoryNote` 快照 + `MemoryEvent` 生命周期，但字段语义有协调专属约定：

| 字段 | 协调语义 |
|---|---|
| `kind` | 沿用 `Episodic`（进度/移交）或 `Semantic`（冲突/同步点）；**不新增 kind** |
| `lifecycle` | `claims`/`progress` 用 `Replacement`；`handoff`/`sync-point`/`conflicts` 用 `Accretive` |
| `expires_at` | 协调条目的**默认短 TTL**（如 30 分钟声明、6 小时移交）；过期即从 `CoordinationView` 排除，不进入默认召回 |
| `actor` | 声明者/写者；所有权以已认证 principal 为准（§4.1 规则） |
| `body` | 有界正文：声明的文件/路径范围、移交的交付物摘要、进度的一行状态 |
| `evidence_refs` | 移交/冲突可选引用 commit OID / Run / Evidence |

协调条目**默认**是本地 Working 缓冲中的 ephemeral 状态，只有达到晋升门槛（§19.5）才写为持久 `MemoryNote`。为区分，协调写入走 `MemoryWriter` 的一个专门入口 `MemoryCoordinator`（见 §19.4），它复用 `CompileRecord.origin` 的 `Coordinator` 值。

### 19.4 `MemoryCoordinator` Module 与并发语义

协调写入**必须**经 `MemoryWriter` 内部的 `MemoryCoordinator` Module（不新增公共 seam，仍符合 §4.2.1「写入只有一个 seam」）：

- **claim（所有权声明）**：`MemoryCoordinator::claim(scope, task_id, paths, principal)`。对 `coordination.<task>.claims` 执行 **CAS 单写者赢**——仅当该 cell 当前无活声明（或声明已过期）时才接受；否则返回 `OwnedBy{other_actor, claim_note_id}`。claim 写为 `Replacement` note，带短 TTL。
- **yield / release**：`release(task_id, claim_id)` 追加 `Revoked` 事件，释放所有权（同 §10.3 撤销，但只作用于协调 cell）。
- **handoff**：`handoff(task_id, from, to, deliverable_summary, evidence_refs)` 追加 `Accretive` note，写移交记录；`to` 可以是 actor 或 `any`（表示"下一个空闲 agent"）。
- **progress**：`progress(task_id, status, detail)` 写 `Replacement` note，更新进度标记。
- **conflict-declare**：`conflict_declare(paths_or_tag, mine, theirs, evidence_refs)` 追加 `Accretive` note 到 `coordination.conflicts.*`，触发 `contradicts` 链接，进入隔离/人工解决（§7.5）。
- **sync-point**：`sync_point(task_id, revision, description)` 追加 `Accretive` note，登记一个可被其他 agent 查询的同步点。

并发语义与现有 §4.2.1 完全一致：所有协调写入都走 ref CAS + 有界重试；claim 的单写者赢在 `(scope, namespace, path)` cell 上由事件状态机 + CAS 共同保证，**不引入文件系统锁**（§4.2.1 明确 worktree checkout 状态不能充当 writer lock）。

### 19.5 `CoordinationView` 与注入

每个 agent 在 SessionStart（§11.1）额外解析一个 `CoordinationView`——它是 `ResolvedMemoryView` 的一个协调子集，冻结当前仓库作用域下的：

- 活跃 claims（未过期的所有权声明，含持有者与路径范围）；
- 待处理 handoff（`to = <me>` 或 `any` 且未 ack）；
- 未解决 conflicts（`coordination.conflicts.*` 中非终态条目）；
- 最近 sync-points（有界数量）。

`CoordinationView` 作为有预算的 prompt 槽位注入（§11.7），**不进入默认持久 recall**；它回答 agent "现在谁在做什么、哪里撞了、我该接手什么"。协调变更在 TurnEnd 经 Working 缓冲回写；达到以下门槛才巩固为持久 note：

- claim 的持有者释放后，其历史由 `episodic.coordination.<task>.handoff` 承接（经 Trust Gate 确认）；
- 一个 sync-point 被 ≥2 个 agent 复用且稳定，可经 consolidation（§10.5）提升为 `procedural.repo.coordination` 或 `semantic.repo.*` 持久 note。

### 19.6 授权、隐私与过期

- `coordination` namespace 是仓库共享，但**仍须**在读取前做 principal / sensitivity 过滤（§13.1「投影不可越权」）：`SecretLike` / `Confidential` 正文不进协调通道；协调条目 body 默认 `Internal`。
- claim / handoff 的 `actor` 以已认证 principal 为准，不接受请求体自报（§4.1.1）。
- 短 TTL 只从 `CoordinationView` 与默认召回排除过期条目；**历史事件与 note 不物理删除**（§10.7），可 `libra memory log` 审计。
- 协调条目**不得**进入团队 publication / 远端 durable tier，除非经 §5.4 的团队 publication 流程（前置 C9 + SB-02 + mainline ML-01 manifest）。

### 19.7 与现有 MEM 的关系

| 能力 | MEM-01..05 | MEM-06 协调 |
|---|---|---|
| 目的 | 持久知识（记住并召回可复用事实） | 并行工作协调（谁做、交给谁、哪里撞） |
| 生命周期 | 长期，取代/累加 + 巩固 | 短 TTL ephemeral，默认 Working 缓冲 |
| 注入 | 默认持久 recall + 有界预算 | 独立 `CoordinationView` 槽位 |
| 写入 | `MemoryWriter` | `MemoryWriter.MemoryCoordinator`（同一 seam） |
| 晋升 | Trust Gate → Confirmed 持久 note | 仅 sync-point / handoff 稳定后经 consolidation 巩固 |
| 并发 | CAS + 事件状态机 | 同左；claim 额外单写者赢（cell CAS） |

MEM-06 **不新增存储层、不新增第二套并发机制**，完全构建在既有 §3/§4/§5 之上；它主要是一个新的保留 namespace + 一个 `MemoryCoordinator` 入口 + 一个 `CoordinationView` 投影，并复用 Working 缓冲与 Trust Gate。

### 19.8 验收与验证（对应 plan-long MEM-06）

- **单写者赢**：两个 agent 并发 `claim` 同一 `(task, paths)` cell，断言恰好一个成功、另一个收到 `OwnedBy{other}`；释放后可重新 claim。
- **移交闭环**：A handoff → B（`to=<me>`）在 SessionStart 注入；B ack 后 A 的声明释放；`to=any` 时任一空闲 agent 可接手。
- **过期不毒化**：claim 的 TTL 过期后从 `CoordinationView` 排除，历史仍在 `log` 可审计；不因过期条目拒绝新 claim。
- **冲突声明**：两个 agent 并行改重叠路径，`conflict_declare` 触发 `contradicts` 链接并进入隔离；`CoordinationView` 显示未解冲突。
- **不越权**：`SecretLike` / `Confidential` 不进协调通道；`actor` 不信任自报。
- **无第二真源**：协调条目仍从 `refs/libra/memory/*` 可重建；Working 缓冲可丢弃；`MemoryCoordinator` 不绕过 `MemoryWriter`。
- **与并行工作区衔接**：跨 worktree（§9.1 / [`libra-worktree-architecture.md`](../libra-worktree-architecture.md)）的 agent 都能读到同一 repo 级协调（Repo scope 共享），worktree 级协调按需隔离。

### 19.9 分阶段与优先级

MEM-06 依赖 MEM-01（存储/隐私）与 MEM-03（Trust Gate / 巩固），建议排在其后：

| 阶段 | 内容 |
|---|---|
| 前置 | MEM-01（Phase A）、MEM-03（Phase B）先落地 |
| MEM-06-a | `coordination` namespace + `MemoryCoordinator`（claim/release/handoff/progress）+ 单写者赢 CAS |
| MEM-06-b | `CoordinationView` 注入（SessionStart）+ TurnEnd Working 回写 |
| MEM-06-c | conflict-declare / sync-point + 经 consolidation 巩固为持久 note |

协调 Memory 的价值在于：它把并行 agent 的隐式协调（靠猜、靠共享文件、靠 merge 后撞冲突）变成显式、可审计、可过期的仓库内通道，与 Libra 的 VCS-native + 事件历史 + 单一 writer 纪律完全同构。

## 总结规则（Summary Rule）

```text
1. Hierarchical paths replace flat blobs.        (memoir-ai)
2. Namespaces separate user facts, onboarding,
   metrics, and private actor memory.             (memoir-ai + Libra)
3. Snapshot stores "what the memory is".          (libra)
4. Event stores "what happened to the memory".    (libra)
5. Projection stores "what is current".           (libra)
6. Draft, quarantine, trust, and sensitivity
   gates decide what can enter the prompt.         (Libra safety)
7. Git refs are the historical truth — refs,
   commits, blame, diff, revert all work
   unchanged on memory.                           (libra-native)
8. Compile records make every write reproducible;
   context receipts make every injection
   replayable.                                    (Statewave, §0.2)
9. One MemoryWriter owns every mutation; callers
   never edit refs or projections directly.       (Libra safety)
10. ResolvedMemoryView freezes session-visible
    scopes, refs, watermarks, and policy.          (perstate UX + Libra)
11. Markdown and graph views are rebuildable,
    redacted, untrusted projections.               (perstate + Libra)
12. Coordination Memory gives parallel agents a
    shared, bounded, expiring, single-writer channel
    for claims, handoffs, conflicts, and sync points. (MEM-06, §19)
```

至此，agent 的 memory 成为仓库的一份带版本、可分支、可审计的产物——与代码
本身完全一样。
