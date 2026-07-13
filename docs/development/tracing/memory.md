# Memory：为 Libra Agent 提供持久化、可版本化的知识

> **Out-of-scope of `tracing/plan.md`**（§0 范围声明）：AI-agent persistent memory 设计面（agent knowledge store）不属于 AG-16~AG-24 外部捕获改进计划。已知冲突（plan.md §0 记录）：(1) 本文档断言 `LifecycleEventKind` 共 11 变体、"无需新增 hook 事件"，与 A4 新增 `SubagentStart`/`SubagentEnd`（现 13 变体）冲突——枚举描述待文档 owner 更新；(2) 本文档以 `libra mcp --stdio`（其链接的 docs/development/mcp.md 当前不存在）为前提，与 C6 固定的 `libra code --stdio` 冲突，待 C6 落地对齐。

> Status: draft
> Last updated: 2026-07-13
> Scope: 规范 Memory 子系统——agent 跨 run / thread / branch 的持久化知识存储，及其在 Libra 三层对象模型（快照/事件/投影）、运行时生命周期与 MCP 边界上的落地约定。

本文档规范 `Memory`——一个让 agent 能够跨 run、跨 thread、跨 branch 记住事物的 Libra 子系统，且不会像 `CLAUDE.md` 这样的扁平大块文件那样污染上下文。

本设计借鉴了 [`memoir-ai`](https://github.com/zhangfengcdt/memoir) 项目（“Git for AI Memory”），以及相关的记忆系统，如
[Letta / MemGPT](https://docs.letta.com/guides/agents/memory)、
[LangGraph memory](https://docs.langchain.com/oss/python/langgraph/memory)、
[OpenAI Agents SDK memory](https://openai.github.io/openai-agents-python/sandbox/memory/)、
[Mem0](https://arxiv.org/abs/2504.19413) 与
[Zep / Graphiti](https://arxiv.org/abs/2501.13956)。它被改编以适配
Libra 在 [`agent.md`](../ai/object-model.md) 与
[`ai-object-model-reference.md`](../ai/object-model-reference.md) 中所记录的三层对象模型。

如果本文档与 `agent.md` 或 `agent-workflow.md`（见 [`agent-workflow.md`](../ai/workflow.md)）有冲突，以那两份文档为准。此外，外部 agent 捕获契约（见 [`docs/development/tracing/agent.md`](./commands/agent.md)）在其覆盖的存储与捕获面、MCP 边界（见 [`docs/development/mcp.md`](./mcp.md)）在其覆盖的 MCP 命令面，亦各自为准。

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

- `.libra/memory/**` Markdown 只是人类可读、可审查的 projection；
- `indexes/**` 只保存可重建索引或 manifest；
- 用户或 agent 对 Markdown 的编辑必须转译为 proposal → validation → authorization/review → `MemoryEvent`，不能原地改变 Policy 或 branch truth；
- consolidation 保留原始 M0 / source refs，不用摘要覆盖或删除证据。

#### 0.0.4 时间事实参考：Graphiti 的 provenance、有效期与 supersession

[Graphiti](https://github.com/getzep/graphiti) 把 episode 作为派生事实的来源，并为事实关系保留有效期，支持时间、关键词、语义与图遍历混合检索。Libra 应将这套思路落到既有 note/event 模型：

- `MemoryNote.created_at` 与相应 `MemoryEvent.at` 表示 Libra 何时记录、接受或改变事实；
- `valid_from` / `valid_until` 配合 `Branch` scope 与 commit `evidence_refs`，表示事实在哪段代码历史中成立；
- `supersedes` / `contradicts` / `supports` 显式表达替代和冲突，旧事实不被就地覆盖；
- 图、embedding 与实体消解只是 Phase E 可重建投影。

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

### 0.1 方案审查结论

本方案总体合理且可行：它没有为 Memory 新增 git-internal typed object，而是把 Memory 作为普通 Git blob + refs + 可重建 SQLite 投影实现，符合 Libra 现有三层对象模型、外部 agent 捕获边界与 MCP 边界。路径键、namespace、scope、审查状态、trust / sensitivity 门禁也能解决 CLAUDE.md 式扁平长期记忆的上下文污染、不可审计与不可回滚问题。

为达到可落地标准，本文档补强以下方面：

- **接口兼容性**：固定 ref 命名、CLI / MCP 工具名、JSON schema version 与旧 reader 跳过未知字段的规则。
- **数据流正确性**：规定摄入、事件追加、投影更新、prompt 注入的顺序，避免确认前注入、投影漂移与并发覆盖。
- **安全性**：要求在 LLM 分类、MCP 返回、远端分层存储之前先做 redaction / owner filtering / size cap，且所有 mutating 操作 fail-closed。
- **可靠性**：定义 CAS 式 ref 更新、事件 total order、幂等 event replay、rebuild 校验与损坏对象处理。
- **性能**：限定路径深度、查询上限、prompt 预算、LLM 调用预算、cache TTL 与分页，避免 unbounded prefix scan 或 token 膨胀。
- **合规性**：把 `forget` 明确为投影级脱敏与未来压实 API，而非承诺立刻从不可变 Git 历史中物理删除。

### 0.2 骨架选型决策（2026-07-13）

基于 §0.0 的外部参考，曾对比五种互斥骨架：**A** 分类树导航型（memoir-ai 主导，即本文 §1–§17 的形态）、**B** 记忆编译器型（Statewave 主导，M0 唯一入口、一切皆编译产物）、**C** 双时间事实账本型（Graphiti 主导）、**D** 可读投影审查型（ReMe / Letta Code 主导）、**E** 运行态分离蒸馏型（LangGraph / PowerMem / MemOS 主导）。

**决策：骨架采用 A，同时把 B 的两个原语从「字段级补丁」升级为硬性退出门槛。** 这不是可选优化——骨架 A 的原始形态（直接 `remember` 即成为 confirmed 候选）对 §0.0.1 约束 2「先编译，再使用」满足最弱，以下两条是让 A 合规的必要修正：

1. **编译记录（`CompileRecord`，§4.1.1）是 Phase A 的发布门槛。** 任何入口（显式 remember、anchor 提升、frame 蒸馏、分类器、consolidation、onboarding、import）产生的每条 `MemoryNote` 都必须携带完整编译记录（来源入口、producer / prompt / model / policy 版本、输入对象哈希、幂等键）；缺失或不完整一律拒绝写入，同幂等键重复摄入不产生新 note。
2. **注入回执（`ContextReceipt`，§8.6）是 Phase C 的发布门槛。** 每次 prompt 注入与引擎内召回都必须产出回执，记录选中对象、排序原因、token 预算与 policy / index 快照；`memory inspect-injection` 从回执重放，而非从当前投影反推。回执原语与 mainline ML-05 / ML-08 共用（§0.0.10），本文不新建第二种 receipt。

C 的双时间语义（§4.1 已含 `valid_from` / `valid_until` + commit anchor 形态的 `evidence_refs`）与 E 的 skill promotion 管线（§0.0.7、§10.5）保留为骨架 A 上的后续增量，不改变本决策。

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

1. **分层语义路径**（`procedural.coding.tabs`），而非 UUID 或向量键。前缀查找为 O(log n)；且人类可读。
2. **在路径处做记忆聚合**——把多条记忆收拢在同一个语义位置之下，而不是散落为彼此独立的文档。
3. **价值过滤（worthiness）**——并非每个 turn 都值得写入记忆；系统会显式地分类“到底要不要记”。

memoir-ai 使用 Prolly-tree 作为后端存储。**Libra 不需要这套**：我们已经实现了 Git 的磁盘格式，并拥有一等的内容寻址存储、refs、branch、commit、blame 与 revert。Memory 搭载在 `git-internal` 的 refs 上，与现有 AI 工作流对象所在的孤儿分支 `libra/intent` 并列共存。

### 2.4 其他系统对本设计的改变

外部系统都在强化同一个方向，但也暴露出第一版草稿必须补上的缺口：

| 系统 | 有用的想法 | Libra 的改编 |
|---|---|---|
| memoir-ai | 分类树路径、branch 感知记忆、读取 hook、Stop-hook 捕获、代码库 onboarding 命名空间 | 采纳按路径作键的存储与 branch refs，但把历史真源存放在 `git-internal`，而非 Prolly tree。引入命名空间，使 `default` 用户事实与 `codebase:onboard` 快照不共享保留策略与 prompt 策略。 |
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
- **Namespace**（它属于哪个集合）：`default`、`codebase:onboard`、`project:onboard`、`metrics.turn`、`metrics.code`，或 `private:<actor-ref>`。Namespace 决定保留策略、prompt 注入、onboarding 与审查策略。
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
| `metrics.turn` | 每个 turn 的延迟、token、工具与结果度量 | 默认从不注入 prompt | 积极修剪投影 |
| `metrics.code` | 每个 branch 的代码变更审计度量 | 默认从不注入 prompt | 保留摘要；修剪原始尾部数据 |
| `private:<actor-ref>` | actor 本地偏好或接近 secret 的笔记 | 仅对匹配的 actor 可见 | 仅可选择性提升 |

## 4. 对象模型

Memory 遵循与 Libra 其余部分**相同的快照（Snapshot）/ 事件（Event）/ 投影（Projection）三层模型**（见 `../ai/object-model-reference.md`）。三层缺一不可；省略其中任何一层都会重新引入 CLAUDE.md 那种反模式。

不过这里有一条至关重要的存储边界必须先讲清楚。Memory 采用的存储策略与**外部 agent 捕获完全相同**（参见 `docs/development/tracing/agent.md` 的「持久化与对象边界」）：把自定义 JSON 序列化为**普通的 git blob**，组织进 tree，提交到专用的 `libra/memory*` ref，再叠加一层**可重建的 SQLite 投影**。Memory **不**向 git-internal 新增任何 `ObjectType` 变体。git-internal 的 typed AI 对象族（`ObjectType::is_ai_object()` 这个闭合枚举：Intent/Plan/Task/Run/PatchSet/ContextSnapshot/Provenance 等快照，以及 RunEvent/TaskEvent/IntentEvent/PlanStepEvent/RunUsage/ToolInvocation/Evidence/Decision/ContextFrame 等事件）**专属于内部 AgentRuntime**，落在孤儿分支 `libra/intent` 上。Memory 借用的只是 git-internal 对象模型在快照/事件/投影上的那套**「设计纪律」**（见 `../ai/object-model-reference.md`）——但 `MemoryNote`/`MemoryEvent` 的字节是**自定义 JSON blob**（与 `traces` 上的 checkpoint 同构），并不是 git-internal 的一等对象。因此，下文中 `evidence_refs` 所指向的 git-internal `Evidence`/`Run`/`Decision` 对象（它们位于 `libra/intent` 平面）与 commit OID 之间，构成的是**跨平面引用**：Memory 平面的 blob 引用了 AgentRuntime 平面的一等对象，但两者各自独立存储、各自版本化。

### 4.1 `MemoryNote` —— 快照 [S]

单条记忆某一版本的不可变、内容寻址正文。

| 字段 | 类型 | 含义 |
|---|---|---|
| `schema_version` | `u32` | JSON schema 版本；第一版固定为 `1` |
| `note_id` | `Uuid` | 同一事实的多个版本之间保持稳定的逻辑标识 |
| `revision_id` | `ObjectId` | 本版本的内容哈希（Git OID） |
| `namespace` | `String` | 逻辑集合，例如 `default` 或 `codebase:onboard` |
| `path` | `String` | 分类法路径，例如 `procedural.coding.tabs` |
| `kind` | enum | `Procedural` / `Semantic` / `Episodic` |
| `scope` | enum | `Repo` / `Branch(name)` / `Worktree(id)` / `Actor(ref)` / `Global` |
| `lifecycle` | enum | `Replacement`（覆盖式）/ `Accretive`（累加式） |
| `body` | `String` | 被记住的陈述（允许 Markdown，保持简短） |
| `rationale` | `Option<String>` | 可选的「为何重要」/「从何而来」说明 |
| `evidence_refs` | `Vec<EvidenceRef>` | 指向 `Evidence`、`Run`、`Decision`、commit OID 的指针，用以佐证该条记忆 |
| `links` | `Vec<MemoryLink>` | 显式的 sibling / prerequisite / contradicts / supersedes 链接 |
| `parents` | `Vec<ObjectId>` | 同一 `note_id` 的先前版本（版本谱系） |
| `tags` | `Vec<String>` | 自由形式标签（`security`、`flaky`、`infra`、……） |
| `confidence` | enum | `Low` / `Medium` / `High`（复用自 `MemoryAnchorConfidence`） |
| `trust` | enum | `Verified` / `RepoEvidence` / `UserAsserted` / `ExternalUntrusted` / `Inferred` |
| `sensitivity` | enum | `Public` / `Internal` / `Confidential` / `SecretLike` |
| `valid_from` | `Option<DateTime<Utc>>` | 该事实开始为真的时间（若已知） |
| `valid_until` | `Option<DateTime<Utc>>` | 该事实不再为真的时间（若已知） |
| `expires_at` | `Option<DateTime<Utc>>` | 可进入 prompt 的可见性 TTL；历史 note 本身仍保持不可变 |
| `author` | `ActorRef` | 提出本版本的人类或 agent |
| `created_at` | `DateTime<Utc>` | 写入时冻结 |
| `compile_record` | `CompileRecord` | 本版本如何被生产出来（§4.1.1）；所有入口必填 |

规则（沿袭 `../ai/object-model.md` 中 `Intent` / `Plan` 的快照规则）：

- `MemoryNote` JSON 必须使用稳定字段名与向后兼容的 serde 策略。新增字段只能 additive，旧 reader 必须忽略未知字段；删除或改变字段语义必须 bump `schema_version` 并提供迁移 / rebuild 逻辑。
- 一个 `MemoryNote` 快照回答的是**「agent 在这一版本相信什么？」**，且永不被改写。
- 撤销、取代或修剪一条记忆都是一个**事件（Event）**，而非对快照的就地编辑。
- 对同一 `note_id` 而言，`namespace`、`scope`、`path` 在逻辑上不可变。要移动一条记忆，应写一条新 note 并取代旧的（§10.2）。
- `SecretLike` 的 note 只能以已编辑（redacted）的正文加证据引用的形式存储；它们绝不会被注入 prompt。
- `body` 必须有硬大小上限。第一版建议默认拒绝超过 16 KiB 的正文；更大的内容应存为 `EvidenceRef` 或 onboarding artifact，并在 Memory 中只保留摘要与引用。召回侧的对应模式是「文件即上下文」：此类大体量证据以临时文件句柄交给 agent 按需 read / grep，而非整体注入 prompt（Cursor 的 A/B 实验自报该模式使 token 消耗下降 46.9%，见 §17 开放问题 4 所引分析文章）。
- `compile_record` 缺失或不完整的 note 必须在写入事务第 1 步被拒绝（§4.1.1、§4.2.1）；编译记录随正文一同内容寻址，事后不可补写。

#### 4.1.1 `CompileRecord` —— 编译记录（Phase A 硬性门槛）

沿 §0.0.2（Statewave）与 §0.2 决策，每个 `MemoryNote` 版本必须内嵌一份编译记录，回答「这条记忆是被谁、用什么版本的规则 / prompt / 模型、从哪些输入生产出来的」：

| 字段 | 类型 | 含义 |
|---|---|---|
| `origin` | enum | `Explicit` / `PromotedFromAnchor` / `DistilledFromFrame` / `Classifier` / `Consolidation` / `Onboard` / `Import` |
| `producer` | `String` | 生产者标识与版本，如 `libra-memory/0.19.0` 或 `consolidation-job/1` |
| `rules_version` | `u32` | 确定性规则集（worthiness 正则、路径验证、redaction 策略）的版本 |
| `prompt_version` | `Option<String>` | 参与生产的 LLM prompt 模板版本；纯确定性路径为 `None` |
| `model_id` | `Option<String>` | 参与生产的模型标识；纯确定性路径为 `None` |
| `policy_version` | `String` | 当时生效的 namespace / promotion policy 版本 |
| `input_hashes` | `Vec<String>` | 输入对象哈希（源 trace / anchor / frame / 用户文本的规范化哈希），不得为空 |
| `idempotency_key` | `String` | `sha256(origin ‖ scope ‖ namespace ‖ input_hashes ‖ producer 版本族)`；创建时冻结 |

规则：

- 写入事务（§4.2.1）第 1 步即校验编译记录完整性：`origin` 与调用入口不符、`input_hashes` 为空或幂等键缺失，一律 fail-closed 拒绝写入。
- 幂等键去重只作用于**新建**（`Created`）：同一 `(scope, namespace)` 内同键重复摄入不产生新 note，直接返回既有 `note_id` 且不追加新事件（与 §4.2 的 event 幂等语义一致）。显式 `revise` / `move` 针对既有 `note_id`，不受其约束。
- LLM 参与生产的 note（`prompt_version` / `model_id` 非空）默认最高只能进入 `Draft`；`trust` 上限沿 §7.3 规则，不因编译记录存在而放宽。
- 发现某个 producer / prompt / model 版本产出系统性坏记忆时，必须能按编译记录批量定位受影响 note 并 quarantine 或重新编译——这是把编译记录设为硬门槛的直接回报。
- 编译记录是 note 正文的一部分，随 blob 不可变、可随投影重建；`memory_note_index` 投影为此新增 `origin` 与 `idempotency_key` 列（§5.2），存储创建版本的键以支撑去重与批量召回。

### 4.2 `MemoryEvent` —— 事件 [E]

针对某个 `MemoryNote` 的只追加（append-only）生命周期记录。

| 字段 | 类型 | 含义 |
|---|---|---|
| `schema_version` | `u32` | JSON schema 版本；第一版固定为 `1` |
| `event_id` | `Uuid` | 事件标识 |
| `note_id` | `Option<Uuid>` | 目标 note；命名空间 / 分类法 / prompt 元事件无此字段 |
| `revision_id` | `Option<ObjectId>` | 受影响的具体版本；元事件无此字段 |
| `namespace` | `Option<String>` | 元事件所影响的命名空间 |
| `target_path` | `Option<String>` | 元事件所影响的路径 |
| `action` | enum | `Created` / `Revised` / `Confirmed` / `Quarantined(reason)` / `Superseded(by_revision)` / `Revoked(reason)` / `Pruned(policy)` / `RejectedAtIntake(reason)` / `TaxonomyExpanded` / `PromptTrimmed` / `SessionAttached` / `Consolidated` |
| `actor` | `ActorRef` | 执行该动作的主体 |
| `at` | `DateTime<Utc>` | 发生时间 |
| `evidence_refs` | `Vec<EvidenceRef>` | 可选的、为该动作提供佐证的新证据 |
| `next_note_id` | `Option<Uuid>` | 与 `IntentEvent.next_intent_id` 同义——指向后继 note 的推荐边 |

规则：

- `MemoryEvent` 是改变记忆状态的**唯一**途径。`MemoryNote` 上没有任何可变字段。
- note 的生命周期事件必须携带 `note_id` 与 `revision_id`。命名空间、分类法以及 prompt 修剪类元事件则改为携带 `namespace` / `target_path`。
- 「agent 此刻在路径 X 上相信什么」这一当前状态，是通过遍历事件计算得出的**投影**（§4.3、§4.4）。
- `event_id` 必须在 replay 时幂等；重复事件按同一 `event_id` 去重，内容不一致则视为历史损坏并 fail loud。
- 同一 memory ref 内的事件顺序以 commit 拓扑顺序为主、commit 内以 tree path 字典序为辅、同一时间戳不得作为唯一排序键。投影 replay 必须确定性地产生同一结果。
- 事件 action 状态转移必须受状态机约束：`Draft -> Confirmed|Quarantined|Revoked`，`Confirmed -> Revised|Superseded|Revoked|Quarantined|Consolidated|Pruned`，`Quarantined -> Confirmed|Revoked|Superseded`，`Revoked` 与 `Superseded` 为终态，除 `revive` 通过新事件生成新的 live head 外不得原地复活。`RejectedAtIntake` 是无 live note 的摄入元事件，不产生 `MemoryHead`。

### 4.2.1 写入事务与投影顺序

一次成功写入必须按以下顺序执行，避免 prompt 读到未审查或半写入的记忆：

1. 对输入执行 owner filtering、路径规范化、大小限制、redaction 与 worthiness 检查，并校验 `CompileRecord` 完整性与幂等键（§4.1.1）：编译记录缺失即拒绝；同幂等键重复摄入直接返回既有 note，不进入后续步骤。
2. 写入 `MemoryNote` blob 与 `MemoryEvent` blob，构造新的 memory commit。
3. 使用 compare-and-swap 语义更新目标 `refs/libra/memory*`：只有当当前 ref 仍等于读取时的旧 OID 时才推进；失败则重新读取、重放冲突检查并重试有限次数。
4. 在同一 SQLite transaction 中 replay 新事件并更新 `memory_head`、`memory_path_summary`、`memory_note_index`、`memory_link_index`、`memory_taxonomy_node`。
5. 只有投影事务提交后，prompt 构建器才允许读取新的 confirmed head。

若步骤 4 失败，Git 历史仍是事实源；实现必须标记投影 stale，并在下一次读取前执行局部 replay 或要求 `libra memory rebuild`。不得因为投影失败而回滚已提交的 Git 对象，也不得把 SQLite 投影当作事实源覆盖 Git 历史。

### 4.3 `MemoryHead` —— 投影 [L]

按 `(scope, namespace, path, note_id)` 划分的游标，指向某条逻辑 note 当前生效的版本，并附带去规范化（denormalised）的元数据以加速读取。它存放在 SQLite，而非 `git-internal` 中。

| 字段 | 类型 | 含义 |
|---|---|---|
| `scope_key` | `String` | 规范化的作用域编码（如 `branch:main`） |
| `namespace` | `String` | 逻辑集合 |
| `path` | `String` | 分类法路径 |
| `note_id` | `Uuid` | 逻辑 note |
| `head_revision_id` | `ObjectId` | 当前生效版本 |
| `head_action` | enum | 产生此 head 的最近一次动作（`Confirmed`、`Revised`、`Superseded`、……） |
| `head_review_state` | enum | `Draft` / `Confirmed` / `Quarantined` / `Revoked` / `Superseded` |
| `recent_revisions` | `Vec<ObjectId>` | `memory log` 使用的、有上限的版本尾部列表 |
| `last_used_at` | `DateTime<Utc>` | 召回时更新；驱动修剪策略 |
| `use_count` | `u64` | 召回时更新 |
| `rank_hint` | `i64` | 由 kind、置信度（confidence）、新近度、使用次数推导出的、用于 prompt 排序的确定性次序裁决值 |

规则：

- 缺少某行 `MemoryHead` 表示**「投影缺失」**，而非**「记忆不存在」**——这与 `Thread`、`Scheduler` 投影的契约一致（见 `../ai/object-model.md` §7）。
- 该投影可完全从 `MemoryNote` + `MemoryEvent` 历史重建。`libra memory rebuild` 即执行此操作。

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
| `preview` | `String` | 供调用方驱动召回（recall）使用的、稳定的一句话摘要 |
| `last_changed_at` | `DateTime<Utc>` | 影响该路径的最近一次事件时间 |
| `last_used_at` | `DateTime<Utc>` | 触及该路径的最近一次召回时间 |

规则：

- `memory.get(scope, namespace, path)` 返回该记忆单元（cell）下所有已确认的 `MemoryHead` 行，按 `rank_hint` 排序。
- `memory.get_note(note_id)` 是直接的单条 note 查找。
- `MemoryPathSummary` 允许有损；它是一个 prompt 选材辅助物，而非历史真相。

### 4.5 `MemoryTaxonomy` —— 投影 [L]

活动分类树（taxonomy）的缓存、可重建视图。

| 字段 | 类型 | 含义 |
|---|---|---|
| `path` | `String` | 完整路径，例如 `procedural.coding` |
| `parent_path` | `Option<String>` | 上一行的父路径 `procedural` |
| `is_seed` | `bool` | 若随二进制内置则为 `true` |
| `expanded_from` | `Option<EventRef>` | 引入该分支的那次迭代式分类器事件 |
| `note_count` | `u64` | `path == self.path` 的存活 note 数 |
| `prefix_count` | `u64` | 路径位于 `self.path` 之下的存活 note 数（用于 `O(log n)` 的 summarise） |
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
MemoryEvent[E] --revision_id----------> MemoryNote[S]
MemoryEvent[E] --next_note_id?--------> MemoryNote[S]

Projection
==========

MemoryHead[L] --(scope,namespace,path,note_id)--> MemoryNote[S].note_id
MemoryHead[L] --head_revision_id------> MemoryNote[S]
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
refs/libra/intent                        # 现有 AI 工作流对象的孤儿分支（Intent/Plan/...，归 AgentRuntime）
refs/libra/memory                         # 新增：Repo / Global 作用域的默认 memory ref（NEW）
refs/libra/memory/branch/<branch-name>    # 新增：分支作用域的 memory（NEW）
refs/libra/memory/worktree/<id>           # 新增：worktree 作用域的 memory（NEW）
```

一个「memory commit」就是一个普通的 Git commit，其 tree 中包含：

```text
notes/<namespace>/<note_id>/<revision_id>.json    # MemoryNote 正文
events/<yyyy>/<mm>/<event_id>.json                # MemoryEvent
taxonomy/expansion/<event_id>.json                # 分类法扩展记录
```

正因为这些 memory commit 是普通 Git commit、其内容是普通 blob 与 tree（与 `traces` 上的 checkpoint 同构，而非 git-internal 一等对象），`libra log refs/libra/memory/branch/main` 立刻就能用，对某条 note 路径执行 `libra blame` 立刻就能用，跨 memory ref 的 `libra cherry-pick` 也立刻就能用——无需任何新的 VCS 代码。

#### 5.1.1 ref 与 tree path 编码

Memory 的逻辑 key 不得直接拼接进 Git ref 或 tree path：branch name、namespace、actor ref 与动态 path 段都可能包含 `/`、`..`、控制字符、Unicode 归一化差异或大小写冲突。实现必须使用一套稳定、可逆、跨平台大小写安全的编码：

- `scope_key` 的逻辑编码形如 `repo`、`global`、`branch:<branch-name>`、`worktree:<id>`、`actor:<actor-ref>`；写入 ref 时再编码为 Git-ref-safe segment。
- `refs/libra/memory/branch/<branch-name>` 中的 `<branch-name>` 必须是编码后的 segment，而不是用户分支名原文。CLI 展示时再解码。
- tree path 中的 `<namespace>`、`<note_id>`、`<revision_id>`、`<event_id>` 必须只包含 `[A-Za-z0-9._-]`；namespace 如 `private:<actor-ref>` 必须编码为安全 segment。
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
    head_revision_id      TEXT NOT NULL,
    head_action           TEXT NOT NULL,
    head_review_state     TEXT NOT NULL,
    kind                  TEXT NOT NULL,
    lifecycle             TEXT NOT NULL,
    confidence            TEXT NOT NULL,
    trust                 TEXT NOT NULL,
    sensitivity           TEXT NOT NULL,
    valid_from            TEXT,
    valid_until           TEXT,
    expires_at            TEXT,
    rank_hint             INTEGER NOT NULL DEFAULT 0,
    last_used_at          TEXT,
    use_count             INTEGER NOT NULL DEFAULT 0,
    updated_at            TEXT NOT NULL,
    PRIMARY KEY (scope_key, namespace, path, note_id)
);
CREATE INDEX idx_memory_head_lookup
    ON memory_head(scope_key, namespace, path, head_review_state);
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
    last_used_at          TEXT,
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
    origin                TEXT NOT NULL,
    idempotency_key       TEXT NOT NULL,
    created_at            TEXT NOT NULL
);
-- 幂等键在 (scope, namespace) 内唯一；存储创建版本的键（§4.1.1）。
CREATE UNIQUE INDEX idx_memory_note_idempotency
    ON memory_note_index(scope_key, namespace, idempotency_key);

-- 派生的链接索引。历史真相是 MemoryNote.links。
CREATE TABLE memory_link_index (
    source_note_id        TEXT NOT NULL,
    target_note_id        TEXT NOT NULL,
    link_kind             TEXT NOT NULL,
    source_path           TEXT NOT NULL,
    target_path           TEXT NOT NULL,
    PRIMARY KEY (source_note_id, target_note_id, link_kind)
);
CREATE INDEX idx_memory_link_target
    ON memory_link_index(target_note_id, link_kind);

-- 分类法投影（可重建）。
CREATE TABLE memory_taxonomy_node (
    path                  TEXT PRIMARY KEY,
    parent_path           TEXT,
    is_seed               INTEGER NOT NULL,
    expanded_from         TEXT,
    note_count            INTEGER NOT NULL DEFAULT 0,
    prefix_count          INTEGER NOT NULL DEFAULT 0,
    last_classified_at    TEXT
);
CREATE INDEX idx_memory_taxonomy_parent ON memory_taxonomy_node(parent_path);

-- 可选：分类器缓存，带 TTL，键为
-- hash(scope + namespace + content + taxonomy_version)。
CREATE TABLE memory_classifier_cache (
    cache_key             TEXT PRIMARY KEY,
    namespace             TEXT NOT NULL,
    suggested_path        TEXT NOT NULL,
    confidence            TEXT NOT NULL,
    created_at            TEXT NOT NULL,
    expires_at            TEXT NOT NULL
);

-- 注入回执账本（§8.6）。注意：这是本地 append-only 审计账本，
-- **不是**投影——它记录读取时刻的选择，无法也无须从 Git 历史重建；
-- rebuild 不触碰它，保留策略负责有界修剪。
CREATE TABLE memory_context_receipt (
    receipt_id            TEXT PRIMARY KEY,
    emitted_at            TEXT NOT NULL,
    scope_key             TEXT NOT NULL,
    as_of_ref_oid         TEXT NOT NULL,
    as_of_commit          TEXT,
    selector_version      TEXT NOT NULL,
    rules_version         INTEGER NOT NULL,
    index_version         TEXT NOT NULL,
    policy_version        TEXT NOT NULL,
    token_budget          INTEGER NOT NULL,
    tokens_used           INTEGER NOT NULL,
    selected_json         TEXT NOT NULL,
    dropped_json          TEXT NOT NULL,
    bundle_hash           TEXT NOT NULL
);
CREATE INDEX idx_memory_receipt_time
    ON memory_context_receipt(emitted_at);
```

除 `memory_context_receipt`（账本，见 §8.6）外，这些都是投影，**不是**历史真相。若被丢弃，可从 `refs/libra/memory/...` 重建；回执账本被丢弃则只是丢失本地审计尾巴，`rebuild` 不会、也不应重建它。

查询实现必须始终带上 `scope_key` 与 `namespace`，禁止只按 `path` 做全局查询后在内存中过滤。跨 scope / namespace 的检索只能由显式 `--all-namespaces` 或策略允许的 scope fallback 触发，并且必须在结果中保留原始 `scope` 与 `namespace`，防止 prompt 注入时发生来源混淆。

`list_prefix` 与 `summarize` 不得执行无上限扫描。实现应使用规范化后的 path 前缀范围查询，并设置默认 `LIMIT`（建议 100 条 summary、50 条 note）与分页游标；CLI / MCP 可显式提高上限，但必须受硬上限保护。

在访问模式上还有一条对齐约定值得明确：Memory 的投影表用 SeaORM entity 来访问（与同样可重建的 `ai_index_*` 投影一致），而不采用 `agent_session` / `agent_checkpoint` / `agent_usage_stats` 那种**故意**保持的 raw-SQL、无 entity 风格。原因在于：`agent_*` 那批表是外部捕获的独立账本，而 Memory 的这些表是 git 真源（`refs/libra/memory/...`）的可重建投影，本质与 `ai_index_*` 同类，因而对齐 `ai_index_*` 的 SeaORM 模式。唯一例外是 `memory_context_receipt`（§8.6）：它与 `agent_*` 一样是本地账本而非投影，沿用 raw-SQL 账本模式，不配 entity。

### 5.3 ClientStorage 分层

`MemoryNote` 的 blob 与其他 AI 快照走完全相同的 `ClientStorage`（本地 + S3/R2）——见 `../ai/workflow.md` 2026-04-29 的说明。无需任何特殊处理：一条记忆正文不过是又一个小的 JSON blob。

超过 `LIBRA_STORAGE_THRESHOLD` 的大型记忆会自动分层下沉到远端存储。

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
   （作为元事件处理；不列入 §4.2，以保持那张表专注于 note 生命周期——
   但它存放在同一份事件日志中）。
4. 一旦被接受，新路径即成为分类树中的一等节点，后续写入可以直接将其作为目标。

记忆之间的交叉引用（memoir-ai 的 `related_keys`）以历史方式存储在
`MemoryNote.links` 中，并投影（Projection）到 `memory_link_index`。
链接种类（link kinds）：

- `sibling`：同一次写入被分类到多条路径。
- `supports`：本 note 强化或解释了另一条 note。
- `contradicts`：本 note 与另一条 live note 冲突，应触发隔离（quarantine）
  或人工解决（resolution）。
- `supersedes`：本 note 有意取代（supersede）另一条逻辑 note。

直接路径编辑（direct-path edit）通过「先取后并」（fetch-then-merge）保留
既有的 `sibling` 链接，与 memoir-ai 的编辑语义一致。分类器驱动的重写
（classifier-driven rewrite）则可以替换链接，因为此时分类器是在有意地对该
note 重新聚类。

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
**回退到 LLM**（LLM-fallback）的策略。一次价值拒绝会被记录为一条 action 为
`RejectedAtIntake` 的 `MemoryEvent`，这样人类便能看到 agent 为何没有记住
某件事。

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

- 若 `path` 无效（深度 >5、未知根、静态槽位中出现动态段），施加渐进式
  缩短（progressive shortening）：逐段丢弃末尾段，直到找到一个有效前缀。
- 若仍然无效，回退到 `<root>.unsorted`（一条保证存在的种子路径），并发出
  一条警告事件。

路径验证必须使用 §6.2 的文法与 §5.1.1 的编码规则。动态段只能出现在已声明允许动态段的模板位置；若模板未知，动态值必须先映射到已有静态 bucket（例如 `episodic.runs.unsorted`）或作为 tag / evidence metadata 保存，不能任意扩展 taxonomy。

### 7.5 阶段 4 —— 冲突与可信度门

在一条 note 变为可进入 prompt 的（prompt-visible）之前：

1. 加载同一 `(scope, namespace, path)` 单元（cell）内的已确认 note。
2. 对于 `replacement`（覆盖式）note，检查新正文是否与某条现有 live 正文
   矛盾，以及哪一方拥有更强的证据。
3. 若冲突可由谱系（lineage）解决（`parents` 包含旧 revision），则确认新
   revision 并取代（supersede）旧的。
4. 若两侧都站得住脚、且证据上谁都不占优，则创建
   `MemoryEvent { action: Quarantined(reason) }`，添加 `contradicts` 链接，
   并将两者都排除在 prompt 注入之外，直到 `libra memory resolve` 选定一个
   结果。
5. 若 `trust == ExternalUntrusted`，则在该 note 离开 `Draft` 之前，要求
   要么提供指向已抓取来源的 `EvidenceRef`，要么获得明确的人工确认。

6. 若 `sensitivity >= Confidential`，则默认只能保持 `Draft` 或进入 `private:<actor-ref>` namespace；除非命名空间策略显式允许，否则不得自动确认。`SecretLike` 永远不可 prompt-visible。

信任等级的排序固定为：`Verified > RepoEvidence > UserAsserted > Inferred > ExternalUntrusted`。自动确认门禁只能提升到输入证据可支持的等级，不能由 LLM 自述提升。

这正是 Memoria 风格的隔离（quarantine）与 Zep 风格的时间性真值处理
（temporal truth handling）进入 Libra 的切入点——而无需在基础设计中引入
图数据库。

## 8. 召回管线

Memory 暴露四种召回（recall）模式——与 memoir-ai 相同的拆分方式，外加
Libra 特有的直接 get 与调用方驱动的原语。

### 8.1 直接路径 get（无 LLM）

```rust
memory.get(scope, namespace, "procedural.coding.tabs") -> Vec<MemoryNote>
memory.get_note(note_id) -> Option<MemoryNote>
memory.list_prefix(scope, namespace, "procedural.coding.") -> Vec<MemoryPathSummary>
```

经由 SQLite 的 `memory_head` 与 `memory_path_summary` 达到 O(log n)。
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
memory.get(scope, namespace, path) -> Vec<MemoryNote>
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
3. 对于 `episodic.*`，检索按「新近度 × 使用次数 × 与当前任务的标签重叠度」
   排序的前 K 个最相关 head。K 取较小值（5–10）。
4. 作为有预算约束的 `ProjectMemory` 与 `MemoryAnchor` 上下文段注入 prompt，
   上限为可配置的 token 天花板（默认 1.5k tokens）。

该注入渲染为一个稳定、对前缀缓存友好（prefix-cache-friendly）的块——其顺序
是确定性的，且除非某个 head 发生变化，否则格式在各 turn 之间不变。

可进入 prompt 的（prompt-visible）note 必须展示 `path`、`namespace`、
`scope`、`confidence`、`trust` 以及一个简短的证据指针。agent 被告知：
记忆只是指引，当前的源文件 / 命令输出会覆盖陈旧的记忆。

### 8.6 注入回执（`ContextReceipt`，Phase C 硬性门槛）

每次 `with_memory(...)` 注入（§8.5）以及引擎内召回（§8.2 / §8.3）完成后，必须产出一份 `ContextReceipt`；写不出完整回执，该次注入 / 召回按失败处理（fail-closed）。直接路径 get（§8.1）与调用方驱动原语（§8.4）是确定性查询、不含引擎侧选择，不产回执。

| 字段 | 含义 |
|---|---|
| `receipt_id` | 回执标识 |
| `emitted_at` | 产出时间 |
| `selected` | 选中的 note revision / path summary ID 列表，每项附 reason code（为何入选） |
| `dropped` | 因预算 / 门禁被丢弃项及 reason code（与 `PromptTrimmed` 事件互链） |
| `token_budget` / `tokens_used` | 预算与实际用量 |
| `as_of` | 解析时的 memory ref OID 与 code commit / branch |
| `versions` | selector / rules（编译规则集）/ index（投影 schema）/ policy 版本 |
| `bundle_hash` | 注入渲染块的规范化哈希 |

规则（承 §0.0.2 与 §0.0.11）：

- **同输入同选择可重建。** 固定 `as_of` 快照、同一版本组与同一预算，重放选择必须得到相同 `selected` 集合与 `bundle_hash`；缺失快照时返回 `stale / non-reproducible` 错误，不静默 fallback。该承诺只覆盖选择与渲染输入，不扩张为任何 provider 输出逐字节重放的承诺（§0.0.1 约束 4）。
- **回执是本地审计账本，不是投影。** 存入本地 append-only 表 `memory_context_receipt`（§5.2）；它记录的是读取时刻的选择，无法也无须从 Git 历史重建，因此明确豁免于「删表可 rebuild」条款（§13.1），并按保留策略有界修剪。回执、query hash 与 selected IDs 可能泄露工作意图，默认 local-only、不进团队 ref；任何共享另做 visibility / retention / threat-model 审查。
- **单一 receipt 原语。** Rust 类型与 mainline ML-05 / ML-08 共用同一定义（§0.0.10）；本文与 mainline 各自的注入管线写同一张回执面，不得分叉出两种 schema。
- `memory inspect-injection`（CLI 与 MCP）从回执读取并重放展示，而非从当前投影反推；`PromptTrimmed` 事件以 additive 可选字段携带对应 `receipt_id`。

## 9. 分支与版本

### 9.1 按分支隔离的记忆

`scope = Branch("main")` 的 note 存放在
`libra/memory/branch/main`（git ref 全名写作
`refs/libra/memory/branch/main`）。切换用户的工作分支
（通过 `libra switch`）会隐式切换被查询的作用域：

```text
libra switch experiment
   -> agent 从 libra/memory/branch/experiment 读取记忆，
      并对 Repo 作用域的条目回退到 libra/memory（默认分支）
```

这解决了 §2.1 中描述的「上下文污染」失效模式。

### 9.2 记忆的 log / diff / blame

```bash
libra memory log [path]                  # 影响该路径的提交
libra memory diff <rev1>..<rev2> [path]  # 两个记忆 revision 之间发生的变化
libra memory blame <path>                # 当前 head 由谁、于何时设定
```

这些都是 Libra 既有 `log` / `diff` / `blame` 命令之上的薄封装（thin shim），
作用域限定在 `refs/libra/memory/...`。

### 9.3 merge 与 rebase

一次记忆 merge 就是在记忆 ref 上的一次普通 Git merge。冲突
解决规则如下：

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

## 10. 生命周期与价值过滤（worthiness）

### 10.1 创建

三个入口：

1. **显式（Explicit）** —— 从 CLI 执行 `libra memory remember "..."`，
   或从 MCP / agent 工具调用 `memory_remember`。
2. **由 anchor 提升（Promoted from anchor）** —— 在 thread 结束时，
   已确认（confirmed）且 `MemoryAnchorScope::Project` 的
   `MemoryAnchor` 会通过 TurnEnd / SessionEnd 集成被提升（§11.5）。
3. **从 ContextFrame 蒸馏（Distilled from ContextFrame）** —— 当某个 Run
   产出一个被标记为可复用的 `ContextFrame`（例如 `kind == VerifiedFinding`）时，
   agent 可以提议一次 Memory 写入。

在以上三种情况下，价值过滤（worthiness，§7.1）都会先行运行。

### 10.2 取代（Supersession）

在同一 `note_id` 上写入新 revision，产生的是一次普通的新 revision。
要写入一个应**替换**同一路径下既有 note 的新 note：

1. 写入新的 `MemoryNote`（新的 `note_id`）。
2. 向旧 note 追加 `MemoryEvent { action: Superseded(by_revision = new) }`。
3. 更新 `MemoryHead`，使其指向新 revision。

旧 note 仍可通过 `libra memory log` 查询，绝不删除。

### 10.3 撤销（Revocation）

```bash
libra memory revoke <path-or-note-id> --reason "..."
```

追加 `MemoryEvent { action: Revoked(reason) }`。head 移动到
最近的未撤销 revision；若没有任何 revision 幸存，则整个 `MemoryHead`
被移除。prompt 注入会跳过已撤销的 head。

### 10.4 修剪（Pruning）

修剪只在**投影（projection）**层面进行 —— 它绝不重写
`refs/libra/memory*` 上的 Git 历史。默认策略：

- `last_used_at` 早于 90 天且 `use_count <= 1` 的 `episodic.*` head
  会从 `memory_head` 中被修剪。
- 底层的 note 仍保留在磁盘上，可通过
  `libra memory revive <path>` 复活。

对 `procedural.*` 和 `semantic.*` 而言，修剪是 opt-in（默认不开启）的。

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

### 10.6 隐私与遗忘（forgetting）

记忆有两种「类删除」操作，二者的保证不同：

- `revoke`（撤销）：将一个 note 从当前投影和 prompt 注入中移除，
  但保留其历史 body 以供审计。
- `forget`（遗忘）：针对法律或策略敏感的内容，写入一个
  墓碑（tombstone）事件，并在投影中将可进入 prompt 的 body
  替换为一个已编辑（redacted）的占位符。若底层对象存储日后
  支持加密的墓碑压实（compaction），`forget` 即是驱动它的 API。

`forget` 需要给出理由（reason），且会拒绝对那些被不可变发布
工件（immutable release artifact）所依赖的 `evidence_refs` 执行，
除非调用方显式选择 `--break-evidence-link` 模式。

## 11. Agent 运行时集成

记忆挂接到 Libra 既有的 agent 生命周期
（`src/internal/ai/hooks/event.rs`、`src/internal/ai/hooks/lifecycle.rs`），
**无需新增任何 hook 事件**。它锚定在 Libra 归一化（normalized）的
`LifecycleEventKind` 上——共 11 个变体：SessionStart / TurnStart /
ToolUse / ModelUpdate / Compaction / CompactionCompleted /
PermissionRequest / SourceEnabled / SourceDisabled / TurnEnd /
SessionEnd（参见 `src/internal/ai/hooks/lifecycle.rs`，并与
`../ai/object-model.md` 的 E3 节交叉对比）。各 provider 的原生 hook 名
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

- 解析作用域：`(repo, current_branch, current_worktree, actor)`。
- 对 git 仓库加载最新的 `codebase:onboard` 摘要，对非 git 文件夹
  加载 `project:onboard` 摘要。若已过时（stale），仅注入一条
  过时提示（staleness hint），并请 agent 在有用时再刷新。
- 积极（eagerly）加载少量 `default` 下已确认的 `procedural.*` 和
  高置信度（high-confidence）的 `semantic.*` head。
- 预热（warm）分类器缓存。
- 发出一条 `MemoryEvent { action: SessionAttached }` 供遥测使用。

### 11.2 TurnStart（即既有的 UserPromptSubmit hook）

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
  `VerifiedFinding` 的新 `Evidence`，则对其运行蒸馏与归并流水线
  （§10.1 与 §10.5）。

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
  增量同步，见 §17 开放问题 4 所引分析文章）。

### 11.5 SessionEnd / TurnEnd

- 对每个已确认且 `MemoryAnchorScope::Project` 的 `MemoryAnchor`，
  提议将其提升为一次 Memory 写入。
- 对对话尾部（最后 2 个 turn）运行价值过滤，并为被接受的事实
  创建草稿（draft）候选。
- 交互模式（interactive mode）将候选以「memorize?」提示的形式浮现，
  未经用户批准不会确认。
- 自动模式（auto-mode）仅在以下条件全部满足时才可确认：分类器
  置信度为 `High`、可信度（trust）至少为 `RepoEvidence`、敏感度
  （sensitivity）不为 `Confidential` 或 `SecretLike`、未检测到冲突，
  且命名空间策略允许自动确认。

### 11.6 与 MemoryAnchor 的关系

`MemoryAnchor`（既有于
`src/internal/ai/context_budget/memory_anchor.rs`）保持其当前角色，
即**活动 thread 的 prompt 注入槽位**。记忆则成为填充该槽位的
**持久化后备存储**：

- 在 SessionStart 时，`MemoryAnchor` 行从 `MemoryHead` 读取中
  播种（read-side 投影）。
- 在 SessionEnd 时，已确认的 anchor 回流为 Memory 写入
  （write-side 提升）。

两套系统尽可能共享 `MemoryAnchorKind` 与 `MemoryAnchorConfidence`。
记忆的审查状态在既有的 `MemoryAnchorReviewState` 词表上扩展出
`Quarantined`；在日后将 anchor 折叠为一个薄 Memory 投影的重构
完成之前，anchor 层可继续把被隔离（quarantined）的行当作非活动处理。

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

丢弃行为会记录到一条 `MemoryEvent { action: PromptTrimmed }`，
以便该行为可审计；该事件以 additive 可选字段携带本次注入的
`receipt_id`，与 `ContextReceipt`（§8.6）互链。

## 12. CLI 命令面

```text
libra memory remember <text> [-n <namespace>] [-p <path>] [--scope <s>] [--confidence <c>]
libra memory recall <query> [-n <namespace>] [--mode {direct|single|tiered|caller}] [--limit N]
libra memory summarize [-n <namespace>] [--prefix <p>] [--depth N]
libra memory get <path> [-n <namespace>]
libra memory get-note <note-id>
libra memory list [--prefix <p>] [-n <namespace>] [--scope <s>]
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
libra memory rebuild                         # 从 ref 重建 SQLite 投影
libra memory show-taxonomy [--root <r>]
libra memory expand <parent-path> <new-segment> --rationale <r>
libra memory prune [--policy <p>] [--dry-run]
libra memory revive <path>
libra memory inspect-injection [--last-run|--current]
```

约定：

- `--scope` 接受 `branch`、`repo`、`worktree`、`actor:<ref>`、
  `global`。默认值是 `branch:<current-branch>`；若当前目录不是 Libra/Git 仓库，则回退到 `worktree:<id>` 或 `repo`，并在 `--json` 输出中显式报告解析后的 scope。
- `-n / --namespace` 默认为 `default`；只有当调用方传入
  `--all-namespaces` 时，recall 才会跨多个 namespace 搜索。
- 所有写入类命令都支持 `--dry-run` 与 `--json`，便于脚本化。
- `libra memory recall` 默认使用 `tiered` 模式。

## 13. MCP 命令面

Memory 的 MCP 工具挂在拆分后的 **`libra mcp --stdio`** 命令面上（见
[`docs/development/mcp.md`](./mcp.md)），**不**挂在
`libra code` 之下。实现位置仍在 `src/internal/ai/mcp/`：

| 工具 | 用途 |
|---|---|
| `memory_remember` | 写入一条 memory；执行价值过滤（worthiness）与分类流水线 |
| `memory_recall` | 自由文本召回；支持 `mode` 参数 |
| `memory_get` | 按路径直接查找 |
| `memory_get_note` | 按 `note_id` 直接查找单条 note |
| `memory_list_prefix` | 廉价的前缀列举，用于调用方驱动（caller-driven）的检索 |
| `memory_summarize` | 不调用 LLM 的摘要（路径、子路径、note 计数、预览） |
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
| `memory_prune` | 修剪可重建投影中的低价值 head |
| `memory_revive` | 从历史 note 重新生成 live head |
| `memory_show_taxonomy` | 展示当前 taxonomy 投影 |
| `memory_inspect_injection` | 从 `ContextReceipt`（§8.6）重放最近一次或当前 prompt 注入 |

每个工具与一条 CLI 命令一一对应，因此 Claude Desktop 会话与
`libra memory ...` 的 shell 用户看到的是同一套命令面。

MCP 参数 schema 必须与 CLI `--json` 输出共用同一 Rust 类型或同一 schema 测试。字段新增必须向后兼容；重命名、删除、改变默认值或改变 enum 字面量，都属于 public interface 变更，必须同步 `docs/commands/*`、`COMPATIBILITY.md` 与 compat tests。

**边界纪律（见 mcp.md 的「Boundaries」一节）。** 所有会产生变更的
memory 工具（memory_remember / memory_confirm / memory_quarantine /
memory_resolve / memory_revoke / memory_forget / memory_onboard /
memory_expand / memory_revise / memory_move / memory_prune）都必须经过既有的
**`McpAuthorizer`**（`src/internal/ai/mcp/authz.rs`）、tool policy、redaction
与 audit 流程。只读工具（memory_recall / memory_get / memory_get_note /
memory_list_prefix / memory_summarize / memory_log / memory_show_taxonomy /
memory_inspect_injection）以只读、且受权限控制的方式暴露。

MCP stdio 独占 stdin/stdout：在 stdio 模式下，**不得**输出 banner、warning
或任何非 JSON-RPC 文本——memory 操作绝不能在该模式下 `println`。MCP
**不是** agent 控制面：memory 工具只是有界（bounded）的数据操作，
绝不等同于一次 agent turn 的 submit / respond / cancel。任何 `SecretLike`
内容，以及 `forget` 已编辑（redacted）的内容，绝不能未脱敏地越过 MCP 边界。

### 13.1 正确性、安全与兼容性不变量

以下不变量是实现与评审时的发布门槛：

- **确认前不可见。** `Draft`、`Quarantined`、`RejectedAtIntake`、`Revoked`、`Superseded`、`SecretLike` 与已过期 head 不得进入 prompt 注入；只读 API 返回它们时必须显式标注状态，默认列表隐藏非 live 项。
- **证据不可伪造。** `EvidenceRef` 指向 Libra typed AI object、agent checkpoint、commit OID 或外部 URL 时，必须包含 type、object id、source plane 与可验证 hash / timestamp；无法解析的引用不能提升 trust。
- **无编译记录不落盘。** 任何入口产生的 `MemoryNote` 缺少完整 `CompileRecord`（§4.1.1）必须在写入事务第 1 步被拒绝；同幂等键的重复摄入不得产生第二条 note。这是 Phase A 的硬性退出门槛（§0.2）。
- **注入必须留回执。** 任何 prompt 注入或引擎内召回若未能写出完整 `ContextReceipt`（§8.6），该次操作按失败处理（fail-closed）；`inspect-injection` 只从回执重放，不从当前投影反推。回执默认 local-only，未经 visibility / retention 审查不得成批越过 MCP 边界。这是 Phase C 的硬性退出门槛（§0.2）。
- **投影不可越权。** prompt 注入、MCP 只读返回与 CLI recall 都必须先解析 actor、repo、branch、worktree 和 namespace policy；`private:<actor-ref>` 只能被同一 actor 或授权 reviewer 读取。
- **错误 fail loud。** Git ref 更新失败、投影 replay 失败、schema 版本不支持、MCP authz 拒绝、redaction 不确定、LLM 输出畸形、scope 解析失败与 namespace policy 缺失，都必须返回 actionable error 或保持 draft；不得静默降级为 confirmed memory。
- **兼容默认保守。** 新 namespace、新 action、新 enum 值对旧 reader 必须表现为“不注入 prompt、可保留历史、提示升级”，而不是 panic 或误当 confirmed。
- **可重放。** 删除所有 `memory_*` 投影表（回执账本 `memory_context_receipt` 除外——它不是投影，rebuild 不重建它，§8.6）后，`libra memory rebuild` 必须从 Git 历史确定性恢复同一 live head 集合；若遇到损坏 blob，应跳过该 commit 的受影响对象、记录诊断并继续重建其它对象。
- **资源有界。** 每次 recall、summarize、onboard refresh、consolidation 和 prompt injection 都必须有 note 数、字节数、token 数、LLM 调用数与 wall-clock timeout 上限。
- **不可承诺物理删除。** `forget` 的第一版语义是投影级脱敏与 future compaction tombstone，不承诺从不可变 Git history、远端 object store、backup 或已发布 artifact 中立即物理删除。

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
- `memory_link_index`（§5.2）
- `memory_taxonomy_node`（§5.2）
- `memory_classifier_cache`（§5.2，可选，带 TTL）
- `memory_context_receipt`（§8.6，账本类：append-only、豁免 rebuild、按保留策略有界修剪）

除 `memory_context_receipt`（账本，不可亦无须重建）外，所有投影表都允许被整体删除后，再由 `libra memory rebuild` 从
`refs/libra/memory/...` 重建。

值得一提的是模式选择上的对比：Memory 的投影表用 SeaORM entity 建模（与同样可
重建的 `ai_index_*` 投影同模式），而外部捕获的 `agent_session` /
`agent_checkpoint` / `agent_usage_stats` 是**故意**采用 raw-SQL、不配 entity 的。
Memory 之所以对齐 `ai_index_*` 的 SeaORM 模式，是因为它的表本身就是 git 真源的
可重建投影。`memory_context_receipt` 是唯一例外：它与 `agent_*` 同为本地账本，
沿 raw-SQL 账本模式访问（§8.6）。

## 15. 分阶段路线图

### Phase A — 可审计的存储（1–2 周）

- 在 `src/internal/ai/memory/` 中定义 `MemoryNote` / `MemoryEvent`
  的 Rust 类型。
- 对默认 memory ref（git ref 全名 `refs/libra/memory`）的写入 / 读取。
- SQLite 投影（§5.2）+ Sea-ORM entity：
  `memory_head`、`memory_path_summary`、`memory_note_index`、
  `memory_link_index`、`memory_taxonomy_node`。
- `libra memory remember / get / get-note / list / summarize / log /
  blame / rebuild`。
- 不引入分类器——调用方必须自行提供 `namespace` 与 `path`。
- `CompileRecord` 类型与写入侧强制校验（§4.1.1）：所有入口必须携带
  编译记录，幂等键在 `(scope, namespace)` 内去重。

退出门槛：路径聚合可用；投影重建逐字节稳定（含编译记录字段）；分支
切换不会让 memory 跨 ref 泄漏；**每条已写入 note 都携带完整编译记录，
缺失即拒绝写入，同幂等键重复摄入不产生新 note——硬性门槛，见 §0.2**。

### Phase B — 摄入安全（1–2 周）

- 价值过滤（worthiness）、secret / PII 的 redaction、敏感度（sensitivity）
  与可信度（trust）分类。
- 评审状态：`Draft`、`Confirmed`、`Quarantined`、`Revoked`、
  `Superseded`。
- 冲突检测与 `libra memory resolve`。
- 带「编辑后投影」（redacted projection）语义的 `forget` API。

退出门槛：任何未经评审、形似 secret、外部不可信、或处于隔离（quarantine）
状态的 note，都不能进入 prompt 注入。

### Phase C — 分类、召回与注入（2–3 周）

- 带缓存与多路径同级链接（sibling link）的「模式 + LLM」分类器。
- direct、single-stage、tiered、以及调用方驱动（caller-driven）四种检索。
- MCP 工具（§13）。
- prompt 期注入（§11.7），使用 `ProjectMemory` 与
  `MemoryAnchor` 预算分段。
- `ContextReceipt` 产出与 `memory_context_receipt` 账本（§8.6）：
  注入与引擎内召回写不出完整回执即按失败处理。
- 用于可观测性的 `libra memory inspect-injection`（从回执重放）。

退出门槛：召回具备分阶段计时元数据、确定性的回退路径，以及针对畸形
LLM 输出的测试固件（fixture）；**每次注入产出完整 ContextReceipt，
`inspect-injection` 从回执重放，固定 `as_of` 快照重放选择必须得到相同
selected 集合与 bundle hash，缺失快照 fail loud——硬性门槛，见 §0.2**。

### Phase D — onboarding、归并（consolidation）与分支操作（2 周）

- `codebase:onboard` 与 `project:onboard` 的 cold / warm / meta-only
  刷新。
- SessionEnd 的 draft 捕获，以及定时归并（consolidation）（§10.5）。
- 按分支 / 按 worktree 的 ref（§9.1）。
- memory 的 merge / cherry-pick 冲突解决（§9.3）。

退出门槛：一个特性分支可以携带它自己的 onboarding 与 memory，随后再显式地
将选定的 note cherry-pick 或 merge 进 `main`。

### Phase E — UI 与可选索引（后续）

- 在 `libra code` 中提供 Web UI，用于浏览分类树、路径摘要、链接、
  隔离项、prompt 注入与 diff。
- 可选的 embedding 索引，用于 ANN 召回。
- 可选的时序 / 实体图投影，用于回答 Zep 风格的多跳问题。
- 在对 Libra 有意义之处，与 memoir-ai 的 Claude Code 插件达成功能对等
  （slash 命令、statusline、UI 启动）。

## 16. 验证计划

Memory 只有在配齐有针对性的回归覆盖后才发布：

- 投影重建：写入若干 note / event，删除全部 memory 投影表，再重建，
  断言摘要、head 与链接完全一致。
- 编译记录：各入口（显式 / anchor 提升 / frame 蒸馏 / 分类器 /
  consolidation / onboard）写入的 note 均携带完整 `CompileRecord`；
  缺失、`origin` 与入口不符或 `input_hashes` 为空时 fail-closed；
  同幂等键重复摄入返回既有 note 且不追加新事件；可按 producer /
  prompt / model 版本批量检索受影响 note。
- 分支污染：创建互相冲突的、分支作用域（branch-scoped）的 memory，
  切换分支，断言 prompt 注入随分支一同改变。
- 聚合：在同一路径下存入多条 note，断言 `get(path)` 返回全部已确认
  （confirmed）的 note，而 `get-note(id)` 恰好返回一条。
- 隔离：引入相互矛盾的覆盖式（replacement）note，断言召回与 prompt 注入
  都排除未解决的冲突。
- 隐私：让形似 token、形似私钥的字符串经过摄入流程，断言存储已被编辑
  （redacted）、且不进入 prompt 注入。
- 检索健壮性：畸形 LLM 输出、`NONE`、未知 mode、空结果等情形，都必须
  明确报错（fail loud），或只返回计时类可观测信息，绝不能静默地注入
  任意 memory。
- prompt 预算：把 memory 填到溢出，断言高置信度的程序性（procedural）规则
  比低置信度或陈旧的 note 存活得更久。
- 注入回执：每次注入与引擎内召回都写出 `ContextReceipt`；固定 `as_of`
  快照重放选择得到相同 selected 集合与 bundle hash；缺失快照返回
  stale / non-reproducible；删除全部投影表并 rebuild 后回执账本不受
  影响、也不被重建；`PromptTrimmed` 事件与回执按 `receipt_id` 互链。
- onboarding：cold、warm 与 meta-only 刷新产出确定性的路径，且不会改写
  无关的 namespace。
- ref / path 安全：使用包含 `/`、`..`、大小写冲突、Unicode、控制字符和超长 segment 的 branch / namespace / actor 输入，断言编码可逆、跨平台不冲突，并且非法输入 fail-closed。
- 并发写入：两个 writer 同时写同一 `(scope, namespace, path)`，断言 CAS ref update 触发重试，最终历史可重放且不会丢失任一事件。
- schema 兼容：旧 reader 遇到未知 additive 字段时能跳过；遇到不支持的 `schema_version` 或未知 action / enum 时不注入 prompt，并输出升级诊断。
- MCP 边界：mutating memory 工具必须经过 `McpAuthorizer`；stdio 模式不得输出非 JSON-RPC 文本；`SecretLike` 与 `private:<actor-ref>` 不得越权返回。
- 资源上限：构造大量 namespace / path / note，断言 `recall`、`summarize`、`list_prefix`、`onboard` 和 `consolidate` 均受 limit、分页、timeout、LLM 调用预算与 token 预算限制。
- forget 合规语义：执行 `forget` 后，prompt 注入、MCP 默认读取与 `memory get` 默认读取只显示 redacted body；审计命令仍能解释 tombstone 与无法物理删除的历史边界。

## 17. 开放问题

1. **跨 worktree 可见性。** `libra worktree` 衍生出的关联 worktree 如今
   共享 `.libra/`。它们是否也应共享 memory？当前提议是：`Repo`
   作用域的共享，`Worktree` 作用域的不共享。这一点需要与
   [`libra-worktree-architecture.md`](./libra-worktree-architecture.md)
   做设计对齐。
2. **加密。** `procedural.review.merge-policy` 可能会捕获敏感策略。基础
   提议将 blob 与其他所有 Libra 对象一样以明文存盘，依赖文件系统权限来
   保护。后续可选方案：接入既有的 `LIBRA_STORAGE_*` 信封加密（envelope
   encryption）流水线。
3. **大体量 memory 的 LFS。** 较长的情节性（episodic）发现（例如一份事故
   复盘草稿）可能超出合理的内联大小。当某条 memory 正文超过
   `LIBRA_STORAGE_THRESHOLD` 时，复用 Libra 既有的 LFS 管道
   （`lfs_structs.rs`、`protocol/lfs_client.rs`）。
4. **embedding 索引扩展。** memoir-ai 在其核心中刻意回避向量；本设计沿用
   这一取舍。后续可选的 `memory.embed` 扩展可以在路径键检索**之上**叠加
   ANN 搜索，而绝不取而代之。若实现该扩展，检索的优化目标应当是
   「哪条记忆帮助 agent 达成目标」，而非通用文本相似度——Cursor 的
   代码库索引实践用 agent session traces 训练自有 embedding 模型：
   统计成功任务中被反复访问的内容，再由 LLM 反推「什么本该更早浮现」
   （见 [How Does Cursor Index Your Codebase?](https://manthanguptaa.in/posts/how_cursor_index_your_codebase/)，
   逆向分析博文，数字为 Cursor 自报口径）。Libra 已系统性捕获同类
   训练信号（`traces` checkpoint、`metrics.turn` / `metrics.code`
   命名空间、`ai_*` run 记录）；`MemoryHead.rank_hint` 由
   `use_count` / `last_used_at` 驱动即是该思想的无模型版本。
5. **跨仓库的 memory 联邦（federation）。** 当前不在范围内。一个在多个
   仓库间工作的用户，仍然是每个仓库一份独立的 memory 存储。
6. **prompt 注入可观测性。** Phase B 应当同时发布一个
   `libra code --debug-memory` 标志，每个 turn 逐字打印被注入的 memory
   槽位，让人类能在每次工具调用前精确看到 agent 究竟「记得」什么。

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
```

至此，agent 的 memory 成为仓库的一份带版本、可分支、可审计的产物——与代码
本身完全一样。
