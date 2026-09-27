# Memory 确定性投影迁移执行修订（R08，2026-09-26）

> **状态：R08及既有增补的独立评审见owner回执；R08-G04按2026-09-27用户授权更新本地推进规则。** 本文是 [plan-20260926.md](plan-20260926.md) 的规范性修订附件，不是第二份Memory owner计划。MEM-01/02、任务状态和发布归属仍由owner与 [plan-status.md](plan-status.md) 维护。原R01–R07及后续FAIL/INVALID回执完整保留。计划PASS和DM-00本地audit验收+review已有记录；后续同分支本地开发按§8.3推进，不以本轮文字复核另设开工门，也不预填本轮review PASS。未满足的外部依赖不会因此解除。

## 1. 本次授权、规范优先级和冻结基线

用户授权在 leris 的 `libra-dev-anduin` 容器中参考旧开发分支迁移 Memory，使用 subagent 开发和 review，且不偏离最新确定性设计。**不修改 CI、仓库测试调度配置，不恢复 Code/provider/client/context-budget/runtime，不做外部 agent 适配器或真实 agent 联调，不新建 PR，不 push、不 release。** 内部来源合同、数据库、查询和 CLI 的确定性测试仍在范围内。

主计划的活动 R08 正文把 schema/source/query/门契约与完整任务卡规范委托给本附件；第3–8节为领域契约，第9节为导航，第12节是唯一活动卡定义。只继承 owner 的活动 ADR/GC/ER 与非目标，不从其明确标为历史档案的 R01–R07 正文恢复约束。适用模板为 [plan-template.md](plan-template.md) v2.9：ER-06/06a 必需同卡文档门不计入 G-03 特有门数，但逐文件写集与交付不能省略。任何后续规范变更必须进入主计划修订历史，不能只改代码。

| 对象 | 冻结事实与使用方法 |
|---|---|
| 目标 | `libra-tools/libra` `6576333644c67475f32ae9f553e899b89fe2eb8e`，v0.23.67；实际目标是容器内 Git checkout，分支 `codex/memory-deterministic-migration`；版本控制按实测 Git 元数据执行，不把别处无 `.git` 的历史说明套用到这里 |
| 取材源 | 旧开发分支 `codex/memory-m2-core-clean`，`cdeb5b4495f1cba4b4bd6c6d078dc1c4e8bc1ad4`；只读、逐文件取材，不整包 merge、不改写旧历史 |
| 历史来源 | PR #456 的 `bc1be587e462572ed7d19be32124a9f13548e1f2` 仅为历史 pin；[冻结远端证据](plan-20260926-pr456-evidence.json)于2026-09-26 12:44:37 UTC取得CLOSED（closedAt=2026-09-25T07:45:18Z），实际PR head=`fb37929a1d8c680b7692503d5467a9431319bb9b`，不同于本期取材pin；PR baseRefOid=`a4d6ca6b`也不是本期目标main。不得重复关闭PR，历史checks不能验证新迁移 |
| 迁移 tip | 目标 `src/internal/db/migration.rs:2324-2331`：65 条，tip `2026091901`。取材分支迁移实际为 `2026092501_memory_core`、`2026092502_memory_fts_search`、`2026092503_context_selection_receipt`，不是旧计划写的 `202609070*` |
| 新迁移预留 | DM-01 = `2026092601_memory_core`；DM-10 = `2026092602_memory_path_search`。注册、SQL、down、README、测试与全部规范引用同批更新；开工前重新检查冲突，冲突则先修计划 |
| 工具 | `cargo-nextest 0.9.143` 已在容器工具目录安装；`.env.test` 与无凭据 example 一致；operator 于2026-09-27明确确认本轮不配置live凭据，容器已提供仅记录该决定、受Git忽略的 `.env.live-test`。双文件source成功；T1全量已运行但失败，不能声称全量或live服务验收通过 |
| 后端文档 | 容器当前未找到可用 `libra-backend` checkout；对候选远端 `cf` 的非交互查询要求认证。仅能得出“访问前置未满足”，不能断言仓库不存在 |
| 上游语义依赖 | `plan-20260924.md` ACF-01..09 仍 pending；具名 provider tests 尚未出现在目标源码。`DEP-DM-06` 继续阻塞会话来源的生产接线 |

### 不变的范围

Memory 是仓库本地、零权威、可重建的确定性投影；权威仍是现存 operation/change/session/checkpoint/run manifest 与提交图。没有模型调用或模型 trait，没有新 ref/对象写入，没有 bridge 方法变更、host 信封、编译作业、lease、上下文注入、选择回执、向量检索或旧权威数据自动转换。会话不产生代码 path 行；traces 树不冒充代码树；失败 operation 不用 pre/post view 猜测修改路径。叙述层及跨机器传播继续留在 DEFER-DM-01/02/03。

“迁移”是代码能力迁移，不是将旧 M2 权威记忆无损转换成纯派生事实。旧分支、旧数据库、旧 ref 和对象必须保留；本期不删除或重新解释它们。

## 2. 当前证据对旧方案的取舍

DM-00 的 `plan-20260926-456-salvage.md` 必须对冻结来源的完整 Memory 文件清单、三组迁移、调用方与测试逐文件标 `port/adapt/drop/defer/already-main`，记录目标卡与理由。可参考的是确定性排序/查询限额、FTS external-content 更新协议、证据错误处理和测试场景；旧 FTS 查询算法不视为已满足本期字面转义契约，必须按 §6 重新验证或实现。旧 ref writer、LLM 编译器、jobs、bridge/DSH 专用输入、context receipt、providers/context-budget 全部不进入目标写集。领域类型按本附件重建，不复制旧 trust/sensitivity 分类，也不把旧 MemoryAnchorConfidence 改名后继续带入。

实际事实要求两项原计划未覆盖的修正：

- `src/internal/ai/hooks/session_capture.rs::decide` 只说明会话生命周期；`src/command/agent/session.rs:596-605` 的手工 stop 也会写 stopped。因此 **stopped 不证明任务成功**，不能直接写 succeeded。
- `git-internal 0.10.2::TreeItem` 用 String 保存名字，`from_bytes_with_kind` 对非 UTF-8 尝试 GBK 解码；`src/internal/tree_plumbing.rs:309` 的 flatten helper 同样是 String。无损路径实现不得走这个已解码名字的链路。

## 3. 存储、身份与重建契约

### 3.1 迁移与旧库保护

仅 Repository 角色注册两张新迁移；不改 bootstrap/global/system schema 版本。DM-01 建 3 表，DM-10 建 2 表。下列声明中 `TEXT!`/`INTEGER!`/`BLOB!` 表示 `NOT NULL`，`?` 表示可 NULL；所有布尔为 INTEGER CHECK IN (0,1)，所有计数为 INTEGER CHECK >=0。未列默认值时不得隐式补值。

Repository的schema-managed普通打开、缓存再获取及自动升级均共享只读lineage/shape preflight；未知或缺失的Memory shape不能因receipt版本已最新而跳过，已确认不兼容不能由缓存的临时错误fallback吞掉。真正BUSY/共享pool既有无死锁合同继续保留，非Repository角色不增加Memory校验，FutureSchema拒绝保持优先。缓存的Memory领域错误按类型区分，不能按消息子串识别；只有明确BUSY/共享pool等待才沿用临时策略，SQL形状/解码故障不伪装为争用。ledger/receipt/shape判定须同一只读快照或等价一致性读，避免合法并发迁移被误判并关闭健康pool；不得持有事务后向同一单连接pool再次取连接。自动升级入口必须在任何DDL前完成preflight；包括 `db::establish/create` 内runner之前的bootstrap/top-up和runner的ensure_schema_versions，不能等执行新up SQL才发现旧表。识别到旧 M2 的 `memory_note_index`、旧 `memory_projection_state(scope_key, projected_ref_oid,...)` 或旧三张迁移回执时，**拒绝本期自动转换并给出保留旧库、使用原二进制检查/备份的可操作说明**；事务内不删除、不改名、不清空旧表，不改旧 ref。迁移claim持有写锁时重验shape，避免TOCTOU。`CREATE TABLE IF NOT EXISTS` 不构成结构兼容验证。测试使用有旧receipt和无receipt的旧/未知同名schema，断言拒绝前后schema、rows、receipts与ref字节全不变。

| 表 | 冻结列及约束 |
|---|---|
| `memory_episode`（DM-01） | `episode_id TEXT! PRIMARY KEY`；`repo_id TEXT!`；`source_kind TEXT! CHECK IN ('commit','agent_session','agent_run','bridge_operation')`；`source_key TEXT!`；`outcome TEXT! CHECK IN ('succeeded','failed','aborted','partial','unknown')`；`actor TEXT?`；`started_at INTEGER!`；`ended_at INTEGER!`；`anchor_commit TEXT?`；`change_id TEXT?`；`title TEXT!`；`body TEXT!`；`content_digest TEXT!`（64 个小写 hex）；`producer TEXT! DEFAULT 'derived-v1'`（本切片只写此值）；`rules_version INTEGER! CHECK >0`；`UNIQUE(repo_id,source_kind,source_key)`；list索引 `(repo_id,ended_at DESC,episode_id)` |
| `memory_episode_evidence`（DM-01） | `episode_id TEXT! REFERENCES memory_episode(episode_id) ON DELETE CASCADE`；`ordinal INTEGER! CHECK >=0`；`kind TEXT! CHECK IN ('commit','checkpoint','review_run','operation')`；`ref_id TEXT!`；`link_confidence TEXT! CHECK IN ('identity','operation','temporal')`；`resolution_status TEXT! CHECK IN ('resolved','unresolved')`；`PRIMARY KEY(episode_id,ordinal)` |
| `memory_projection_state`（DM-01） | `repo_id TEXT!`；`source_kind TEXT!`（同词表）；`cursor_json TEXT! CHECK json_valid(cursor_json)`；`fingerprint TEXT!`（64 个小写 hex）；`rules_version INTEGER! CHECK >0`；`schema_version INTEGER! CHECK >0`；`horizon_truncated INTEGER!`；`rebuilt_at INTEGER!`；`fts_synced_fingerprint TEXT?`（NULL或64个小写hex，运维列）；`PRIMARY KEY(repo_id,source_kind)` |
| `memory_episode_path`（DM-10） | `episode_id TEXT! REFERENCES memory_episode(episode_id) ON DELETE CASCADE`；`code_path BLOB!`（`typeof='blob'`，非空，不以前导 `/` 开始，不含 NUL）；`change_kind TEXT! CHECK IN ('added','modified','deleted','renamed')`；`blob_oid_at_end TEXT?`；`mode_at_end INTEGER? CHECK IN (33188,33261,40960,57344)`（十进制对应100644/100755/120000/160000，删除时与OID同时NULL）；`ended_at INTEGER!`；`PRIMARY KEY(episode_id,code_path)`；索引 `(code_path,ended_at DESC,episode_id)` |
| `memory_episode_search_doc`（DM-10） | `rowid INTEGER PRIMARY KEY`；`episode_id TEXT! UNIQUE REFERENCES memory_episode(episode_id) ON DELETE CASCADE`；`title TEXT!`；`body TEXT!`；`paths_text TEXT!` |

`source_key`、repo id、OID 的长度/格式在 typed input 校验；查询必须 repo-scoped。bridge_operation 仅预留，不产出行。`started_at/ended_at` 一律是 UTC epoch **microseconds**：operation 毫秒 ×1000，session/checkpoint 秒 ×1000000，run RFC3339 解析为微秒；转换溢出或 end < start 为无效来源，不能 saturate 后伪造 chronology。所有人读时间格式固定 UTC。

五表在 `src/internal/mutable_state_ownership.rs::MUTABLE_STATE_OWNERSHIP` 逐名登记Repository；`memory_episode.anchor_commit`（DM-01）和`memory_episode_path.blob_oid_at_end`（DM-10）在 `src/command/maintenance.rs::GC_OBJECT_SOURCE_INVENTORY` 逐列记`NonRoot`，说明可重建派生引用不保活对象、缺对象保留unresolved并触发freshness。不得借此恢复旧owned-ref walker或将Memory变GC root。新增schema必须通过既有ownership双向/source+materialized守卫与GC inventory穷举守卫，属schema同轴机械义务。

`enabled_source_kinds` 为当前编译阶段的adapter registry固定集合（DM-13起`{commit}`；DM-14接入后`{commit,agent_session}`；GO的DM-07后再加`agent_run`；bridge不在集合内）。该完整集合与rules_version一起进入每source fingerprint。**每repo每个enabled source即使窗口为空也保留一行empty-snapshot state**；reconcile全量替换为精确预期key集合，不以删除最后Episode为由删掉source state。缺失state、多余旧source state或集合变化均不算fresh，必须对账后重建；五表比较也包含空source state。

### 3.2 身份、摘要和 rowid

命名空间冻结为 `Uuid::new_v5(Uuid::NAMESPACE_URL, b"https://libra.tools/memory/episode/v1")`；Episode 身份仍为该 namespace 下 `repo_id + U+001F + source_kind + U+001F + source_key` 的 UUIDv5。字符串身份字段禁止 U+001F，避免串接碰撞；golden 向量必须包含 repo/source/key 变化和相同输入稳定性。

内容摘要 = SHA-256(UTF-8 canonical JSON)：固定字段顺序、无空白、路径表示为小写 hex、证据按 `(kind,ref_id,link_confidence,resolution_status)` 字典序去重后赋 ordinal。包含所有 Episode 语义列及排序后的 path/evidence，排除 `content_digest` 自身和运维时间。query 输出不读 HashMap 的未定义迭代顺序。

搜索 rowid 不由插入顺序分配：取 SHA-256(episode_id UTF-8) 的首 8 字节为 big-endian u64，计算 `1 + ((x & 0x7fff_ffff_ffff_ffff) % i64::MAX)`。同一投影内不同 episode_id 若碰撞，整次发布失败并保留上一代投影，不随机重试、不覆盖旧行。测试注入碰撞证明无数据损失；hash 规则固定于 SCHEMA_VERSION。

GC-DM-01 比较仍是 5 表：episode、evidence、path、search_doc 四表按主键全列比较（包括rowid、mode_at_end）；projection state只比较fingerprint/rules_version/schema_version/horizon_truncated/cursor_json五列；rebuilt_at及fts_synced_fingerprint是运维列，显式不参与源派生语义比较。FTS是可再生posting，单独integrity-check及恢复往返测试，不参与行序比较。删除/新增导致的增量状态必须与相同源快照、同窗口的fresh rebuild相等。

### 3.3 删除分类不是历史权威

撤销与出窗计数不加持久历史列：`ensure_fresh/rebuild` 返回本次 reconciliation 的 `revoked/aged_out` 诊断；`status` 以当前源快照对比已投影 keys 即时计算 `pending_revoked/pending_aged_out`，追平后为 0。不得把上次重建前累积计数写入确定性内容或 cursor。status 不是“只读 meta 一张表”：它可读取来源摘要及聚合计数，且不反向修改事实。

## 4. 来源、窗口与终态映射

### 4.1 窗口的完整定义

三个窗口独立，规则版本初值 1；配置 `memory.horizon` 默认 5000、`memory.horizon_runs` 默认 2000，均必须为 1..100000。输入越界给可操作配置错误，不静默 clamp。窗口指纹含配置值。

| 来源 | 窗口和聚合 |
|---|---|
| commit | 当前请求捕获的 HEAD 的 first-parent 链前 N 个提交，unborn HEAD 为合法空集；N+1 只用于截断检测。按链位置反查 repo 的 change_revision；没有 change_identity 的普通 Git 历史不伪造 operation/change Episode，并在 status 计数说明。每个 change 当前 revision = 窗内最近 tip，tie `commit_oid ASC`。该 change 的全部关联 revision 以 keyset 页读取，不能调用 limit<=200 的 revisions_for_repo；一次请求最多读取 20000 条 revision，超预算 fail-closed，不能伪称全历史聚合完成 |
| agent_session | 仅 `repo_id` 等于当前仓库且 `scope_state='scoped'` 的 session；包含全部 state 后按 `(started_at DESC,session_id ASC)` 取 N，再应用 terminal predicate。不可先过滤 stopped 再选窗口，否则 resume 会变成出窗而非撤销。legacy_unknown、foreign repo 不自动收编。选中会话的 checkpoint 总读取上限 20000，超限 fail-closed；没有 checkpoint 仍合法 |
| agent_run | 共享agent-runs目录中合法UUID run-id的真实目录，名字字节升序前horizon_runs；symlink不跟随。目录名枚举+排序O(全部run数)，不声称有界。**每attempt**每候选最多解析一次state.json与一次manifest.json，不再叠加局部重读；整请求最多2次完整attempt，故各文件类型解析次数<=2×horizon_runs。缺失/不一致来源使本attempt失败，第二次仍不一致则陈旧失败。不能调用list_runs/list_runs_page |

20000是本次规则版本的固定来源预算，不是偷偷省略历史的阈值；该row读取预算同样覆盖整请求全部attempt，失败说明减少horizon或等待后续容量方案。截断检测和预算用计数器验收，禁止逐Episode N+1。run文件/正文单文件4MiB、**整请求累计32MiB**（两attempt共享），超限fail-closed，不截断findings后冒充完整结论。

全量投影域就是窗口内有效终态集合。窗口内但不再符合终态/来源已删除 = revoked；窗口外仍存在的 key = aged_out。两个集合和其级联行都删除；无法判断是删除还是出窗时查询源的存在性，不按猜测分类。

### 4.2 穷尽的 outcome

全字段映射固定如下，未特别声明的可空字段填NULL，不依赖当前用户、时钟或工作树：

| 字段 | commit | agent_session | agent_run |
|---|---|---|---|
| source_key / change_id | change_id / change_id | session_id / NULL | run_id / NULL |
| title | current revision提交subject | `agent_kind + " session"` | `kind + " run: " + agents按manifest顺序逗号连接` |
| body | current revision的提交body及trailers | 固定模板 `checkpoints=<十进制数量>; scope=<scope_state>\n<选中checkpoint description，NULL为空>` | 归一化findings正文，NULL或不可解析时空串 |
| actor | current revision的operation.actor | NULL | NULL |
| started_at / ended_at | 下述terminal operation min(start)/max(end) | session.started_at / stopped_at | manifest.created_at / updated_at |
| anchor_commit | current revision commit_oid | 下述选中checkpoint parent_commit或NULL | manifest.starting_sha |
| producer | derived-v1 | derived-v1 | derived-v1 |
| path | current revision与首父raw-tree diff | 永远无 | 仅下述历史可冻结revision范围 |

UUID/digest/rules按§3；title/body及actor来源显示值均适用§6归一化。各来源golden row必须逐列固定以上派生，不从旧档案继承相反outcome。

| 来源值 | 产出与 outcome |
|---|---|
| operation running / end_ts NULL | 不产出该 current revision 的 Episode；不把 running 当 partial |
| operation success / failed / partial / aborted（且 end_ts 非 NULL） | 分别 succeeded / failed / partial / aborted |
| operation 未知状态（且 end_ts 非 NULL） | unknown；status 计数；保留原始状态仅在诊断字段中，不 panic |
| session stopped 且 stopped_at 非 NULL | **unknown**：证明 capture 已终止，不证明任务成功；title/body 只陈述生命周期与 checkpoint 事实 |
| session pending / active / condensed / quarantined / stopped 但时间为空 / 未知值 | 不产出 Episode；quarantined 不升级为 aborted，遵守 CTR-ACF-DM06-v1；未知值诊断，不当终态 |
| review success / error / cancelled / timeout / partial | succeeded / failed / aborted / failed / partial |
| investigate quorum / max_turns / cancelled / timeout / error | succeeded / partial / aborted / failed / failed；max_turns 是预算耗尽，不凭 findings 是否存在改写成成功 |
| run terminal_state NULL（含 paused） | 不产出；未来非空未知 terminal 字符串为 unknown，但须通过格式、版本、run kind 的安全校验 |

commit 聚合只在 current revision 具有有效 terminal operation 时产出。`started_at=min(关联 terminal op.start_ts)`，**`ended_at=max(关联 terminal op.end_ts)`，不是 start_ts 的最大值**；历史存在 failed/partial/aborted/unknown 时聚合 partial；仅历史 running 不降级为失败；current 本身 unknown 时保持 unknown。title/body/anchor/path 全部取 current revision；历史 revisions 只成为证据。

session anchor = `(created_at DESC,checkpoint_id ASC)` 首 checkpoint 的 parent_commit，允许 NULL；零 checkpoint anchor=NULL；description 取同一行。全部 checkpoint 证据排序固定，绝不从 traces 树生成代码路径。checkpoint→session 的直接所属关系与 checkpoint→代码提交的 temporal 关系必须区分，后者永不进入 citation；不因存在不相关 bridge link 自动升级整条关系。

run使用manifest的starting_sha/created_at/updated_at；state与manifest的run id/kind/terminal值必须一致。findings_oid为NULL表示无findings、body为空，不算对象损坏；非NULL不可解析则保留Episode、body为空、证据unresolved。只从OID读取正文，不回退到内容可能不同的工作文件findings.md。

**run历史路径解析冻结：** `target_scope` 只接受两个可历史解析端点组成的 `A..B`；端点为完整OID，或以manifest.starting_sha为基点解释的HEAD及其`^n`/`~n`后缀（默认n=1，整数与遍历深度受§5预算）。不得用当前HEAD/branch resolver。包含main、tag、缩写OID等无法从持久事实冻结的端点、不支持的语法或自然语言时不产生path，并在diagnostics记录 `unfrozen_revision_scope`；不是编造路径也不是丢弃Episode。advancing HEAD、retargeting branch、同源rebuild必须得到同样run路径/诊断。

### 4.3 指纹必须涵盖实际输入

原计划的“三/四元组”只是最低示例，不足以判断完整新鲜度。本修订冻结 fingerprint 为 canonical source snapshot 的 SHA-256，含 repo identity、捕获 HEAD、replace signature、规则/schema/selector 版本、配置、窗口排序、截断标记，以及**每个实际派生输入**：

- commit：change/revision 的身份、commit oid、created_op_id 与全部关联 operation 的 status/actor/start_ts/end_ts；使用的不可变 commit/tree OID；引用/replace 变化；对象是否可解析。
- session：session id/kind/state/started_at/stopped_at/sync_revision/作用域字段；全部候选 checkpoint id/parent_commit/description/scope/created_at/sync_revision；参与证据升级的明确 link 行。
- run：kind/run id/agents顺序/starting_sha/target_scope/created_at/updated_at/terminal_state/findings_oid/可解析性；实际读到的 manifest/state 内容摘要；来源版本与 pause 状态。

先在一致 SQLite read transaction 取得来源行快照；文件与 refs 读前后校验，相同 token 才允许原子发布五表。变更期间有限重试（最多 2 次完整尝试），耗尽返回 LBR-MEMORY-001；不得写新 fingerprint 搭配旧 rows。多进程并发在写事务内验证期望 projection generation/fingerprint 后发布，查询与其读快照绑定，防止另一个 worktree HEAD 的投影顶替当前请求。fault injection 必须证明失败时旧 rows/meta/posting 不被部分覆盖。

## 5. 路径、排序、drift 与 CLI interface

Memory module 对调用方只暴露来源快照驱动的 `ensure_fresh/rebuild/status/query` interface；原始 source adapter、canonicalization 与 SQL/FTS 协调是内部 seam，不要求 CLI 知道写入顺序。测试跨同一 interface，不复制派生逻辑。

阶段性公开模型冻结：DM-11的list/show/status JSON只有Episode与evidence，不公开path字段；即使DM-17已写内部path行，也不得经通用数据库序列化泄漏。DM-04首次将path接入show/recall/JSON及formatter，并同卡承接全部路径输出canary和D文档；DM-17只交付raw-tree派生/缓存值及其无损性测试，不单独宣传路径查询。

路径以原始 Git bytes 读取、比较与写 BLOB，不经 String/GBK/lossy 解码。使用现有可保真 reader；若仓库没有，则在 Memory 私有 `paths.rs` 实现有长度校验的 raw-tree entry reader，复用 ObjectHash/对象读取，不改通用 tree parser。解析错误包括缺分隔符、非法 mode、错误 OID 宽度、NUL、空/`.`/`..`/含 `/` 的单 segment；不接受宿主文件系统穿越。SHA-1 与 SHA-256、symlink 与 gitlink 都要覆盖。不要为了本模块提升 lossy collect_tree_leaves 的可见性。

`recall --path <OS path>` 在 Unix 接收 OsString bytes；另提供与之互斥的 `--path-hex <lowercase hex>` 供所有平台精确机器查询。repo-relative bytes 禁止绝对路径、NUL、`.`/`..` segment、空 segment；反斜杠在 Unix 是普通名字字节，不替换成 `/`。非 UTF-8 不能显示成 U+FFFD 后参与匹配。JSON 每条路径固定 `{path: string|null, path_hex: string, change_kind, blob_oid_at_end, mode_at_end, drift}`：UTF-8 可解码时 path 为原文 JSON escaped，否则 null；path_hex 永远完整无损。人读与 paths_text 用 C-style lossless quoting，固定 quotePath=true，不能随用户配置输出 ESC。

tree diff按相同subtree OID剪枝；根提交对空树；rename不做身份跟随，表现为delete+add，renamed词表仅预留。仅mode变化也算modified并持久化mode_at_end；gitlink OID用于相同路径对象比较但不声明为普通blob内容。延续预算fixture：深度6/5000叶/3变更tree reads<=38；0变更<=2。

**规则v1容量保护（不是质量指标）：** 单commit对象4MiB、单tree对象8MiB；整请求所有source、drift和最多2个attempt共用256MiB对象bytes、2,000,000 tree entries、200,000 projected path rows、深度256的Budget，重试不能重置。读取使用现有Storage::get_with_limit或等价底层限额，分配前先扣单对象与剩余累计预算；禁止command::load_object/无界get读完再截断。根commit也受相同限制。超限返回可操作错误并保持旧投影不变；giant-root、deep-tree、累计bytes/entries/path、第二attempt共享限额场景均纳入budget fixture。

selector_version=1：路径匹配按精确文件优先，然后目录 descendant 匹配的相对深度升序；每个 Episode 选最优一条 matched path 排名，避免一个多文件 Episode 消耗全部 limit。其后 `ended_at DESC`、outcome 权重 `failed=4, aborted=3, partial=2, unknown=1, succeeded=0` 降序、episode_id ASC。目录用 byte-prefix + `/` segment 边界，不用 SQL LIKE 通配符。默认limit=20，合法1..1000；list 按 ended_at DESC/episode_id ASC；show按id查；均 repo-scoped。这些选择都要 golden fixture，不称为检索质量基准。

recall每个返回Episode只展示参与排名的一条matched path并算一次drift（其余路径由show获取，不做全量drift），因此解析次数<=limit，实际对象读取另受共享Budget约束。終止时 **(OID,mode_at_end)** 与当前HEAD同路径的(OID,mode)同时相等且存在=current；任一不同=changed；不存在=path-gone（包括历史delete记录，不把null==null写current）。chmod或regular→symlink即使OID相同也必须changed。未提交worktree/index改动不参与HEAD drift。HEAD/对象损坏不是path-gone。

所有查询成功 JSON 固定外壳 `{schema_version:1,stale:boolean,selector_version:1,rules_version:1,data:...}`，静态键序固定，缺值用null，不生成浮动时间。status/rebuild 的运维数据另在data下明确列名，rebuilt_at例外；query不输出绝对私有路径。`--allow-stale` 仅允许现有已验证投影回答，保留schema/所有者/数据完整性检查；无投影不能伪装空结果。无该flag时追平失败返回LBR-MEMORY-001。002=id不存在；003=搜索输入超限；004=FTS unavailable。外部错误码/退出码与现有CLI错误渲染方式对齐并有Display-pin。memory command归ReadOnly，所有子命令验证不新增operation、不修改facts/refs/objects。

## 6. FTS、文本披露和配置库隔离

FTS仅在DM-06 GO后交付。DM-08承接虚表/posting，DM-15承接search命令。内容表由DM-10创建，down修订落点是 `2026092602_memory_path_search_down.sql`，不是已不存在的单迁移路径。首建虚表在同事务 rebuild 既有docs；增量更新先按external-content协议删除旧值再写新posting，与五表一起commit；resume/出窗/源删除都必须去除posting。故障注入覆盖首建、删除、回填、事务回滚和shadow清理。

**确切FTS定义与owner：** Repository-only幂等top-up由 `src/internal/ai/memory/fts.rs` 唯一持有，不加入global/system/bootstrap，不创建trigger。主动top-up允许清单恰为一个虚表 `memory_episode_fts`：

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS memory_episode_fts USING fts5(
    title, body, paths_text,
    content='memory_episode_search_doc', content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
```

既有同名虚表必须核对external-content/tokenizer契约，错误shape不因IF NOT EXISTS被接受；报004且不擅自删除未知对象。SQLite拥有的shadow精确为`memory_episode_fts_data`、`memory_episode_fts_idx`、`memory_episode_fts_docsize`、`memory_episode_fts_config`；逐名登记ownership，仅由虚表机制创建/维护，不另写shadow。down先drop虚表（连带shadow），再删search_doc；不得用memory_*通配清理。此名单已用容器内存SQLite验证，仅作规范核对，不代替DM-15对实际bundled构建的capability gate。

继承bm25权重8/4/2、tie ended_at DESC/episode_id ASC、4KiB/256B每词/32词、双引号双写字面量、双env哨兵失效注入。UTF-8长度按bytes，Unicode whitespace按Rust whitespace分词；空查询为可操作输入错误，不能触发全表搜索。status integrity-check失败时标diagnostic，不阻断其他非search命令；rebuild在FTS不可用时仍重建5张基础表并显式上报FTS不可用。对象/FTS损坏不得吞异常伪称fresh。

**FTS不可用期间与恢复协议：** 每source state的 `fts_synced_fingerprint` 记录最后一次与posting原子发布的fingerprint。普通发布成功同时推进两值；FTS不可用时基础投影仍可原子更新，但synced值保留旧值（全rebuild后为NULL），不能假称posting同步。下一次可用search在查询前检查**预期enabled source集合与当前repo的meta key集合精确相等**，且每个synced匹配；不是只循环已有行，零meta不算同步。虚表新建、任一synced不匹配/缺失、source集合变化/旧source尚未清理时，先按§3完成source对账，再在同一写事务全量FTS rebuild并更新所有synced值，从该事务快照查询。状态持久化，跨进程恢复可见。失败回滚并报004，不返回旧posting。`--allow-stale`只能放宽来源新鲜度，不绕过当前content/posting同代与完整meta集合检查。完整往返fixture从已存在FTS开始：不可用期间增删/重建基础表→新进程恢复→旧词消失/新词命中/integrity正确；还需全删最后Episode、全空窗口、meta缺行、新增source、遗留多余source与allow-stale各具名数据行；仅首建失败测试不能覆盖此门。

DM-08在ownership registry逐名登记FTS及实际shadow表；精确扩展现有DDL source scanner识别本虚表定义，并在生产FTS初始化后实查materialized表双向分类（含SQLite实际生成的shadow名单）。不以fresh create_database未触发lazy FTS为覆盖，不用`memory_*`通配豁免。配置库不创建这些表。

只索引原计划3类来源文本，入库前复用 `review::sink::render_untrusted_findings`；该sanitizer不是secret redactor，不能声称它删除了原始文本中的凭据。这里的披露承诺精确定义为“不索引先前未获准的来源，不扩大权限/跨仓库可见性”，而不是“公开源一定没有secret”。源已有脱敏策略保持原样，测试正文control canary与禁止transcript/prompt/tool原文。身份字段agents/title中的不可信内容同样sanitize。

不改 `.github/**`、`tests/SERIAL_REGISTRY.tsv` 或 `.config/nextest.toml`。配置库隔离优先扩展已有 `tests/db_migration/role_scope.rs::{global_and_system_bootstrap_create_no_repository_shape,role_scoped_schema_writer_keeps_ledgers_disjoint}` 的现有env lane，验证新表与FTS/shadow不会出现、receipts未推进。若实现真的需要新增调度配置，先停该写入并请求新授权，不能以“只是测试”越界。

## 7. 可证伪价值门：两条独立轴

DM-16 是 implementation 卡，拥有 `scripts/memory-value-gate.sh` 和确定性fixture测试；DM-06保留audit，只运行已交付脚本、归档脱敏结果，不新增代码/配置。脚本固定 HEAD + canonical source fingerprint；仅HEAD pin不够，因为session/run事实会独立变化。复算时任一pin变化非零退出，禁止更换样本后仍称同一报告。

**commit/drift轴 C：** 仅用原始log首父窗口统计每个byte路径的提交次数、最后变更距HEAD提交时间的天数（不是运行时钟）；三层按序去重选最多5条：(1)次数最高取2；(2)次数>=2且距HEAD<=30天、尚未选取者取2；(3)次数=1且未选者取1。每层次数降序、raw byte路径升序；不足时依次下一层、再第一层后续候选补齐，记录每条补齐来源。少于5个唯一byte路径时全取，按实际K输出，验证不得硬写等于5。样本选择不读Memory。阳性只包含已确认failed/aborted/partial，或changed/path-gone；unknown不充当失败证据。至少3条阳性路径=GO；窗口存在可测阳性但样本0阳性=NO-GO；其余INCONCLUSIVE（包括窗口无阳性样本）。可测性只能在功能验证通过后判定，不把派生缺失当无样本。K<3不可能仅凭此轴GO。所有计数和补齐来源由冻结脚本产生。

**session轴 S：** 从同一scope/window的原始terminal session按 `(started_at DESC,session_id ASC)` 选最多5个，而不是先读Memory挑样本；对每个用list+show核验source_key、代码锚、证据和checkpoint数量。候选中存在一条“没有可关联代码提交、但有已终止session事实”，且该Episode准确可达，才为GO——log无法表达这条source identity/lifecycle记录；不存在这类源样本为INCONCLUSIVE。S没有以缺失Episode构造的产品NO-GO；只是outcome=unknown不构成阳性。

**先判功能正确性，再合并产品价值：** 两轴都必须先逐样本对照原始来源。命令失败、应有Episode缺失、误归因、证据计数错误或任何语义不等价均为 `FAIL`，修复后重测；不能进入价值合并，不可被另一轴GO覆盖。以下表只接受功能正确的轴结果；C已构成NO-GO时，S无可用阳性不撤销该结论。

| C | S | 整体产品结论 |
|---|---|---|
| GO | GO | GO |
| GO | INCONCLUSIVE | GO |
| NO-GO | GO | GO |
| NO-GO | INCONCLUSIVE | NO-GO |
| INCONCLUSIVE | GO | GO |
| INCONCLUSIVE | INCONCLUSIVE | INCONCLUSIVE |

完整truth table还必须含`C=GO,S=semantic FAIL`与`C=semantic FAIL,S=GO`，整体都为FAIL而非GO。明确标“session生命周期信息增量”，不夸称因果经验总结；NO-GO只是本次冻结可用语料上的阶段性判断，不证明session总体无价值。NO-GO/INCONCLUSIVE仍强制走主计划DM-09 B组，DM-07/08/15进入DEFER-DM-12，不做“先实现再看”；功能FAIL阻塞DM-06，不是降级成功。GO-only集合固定为run派生、findings召回、FTS/search及其文档/错误码003/004/测试；公共完成门不再无条件要求这些产物。

## 8. 两项真实前置和本地/发布分离

### 8.1 DEP-DM-06 不解除

`CTR-ACF-DM06-v1`原样保持：terminal=`stopped AND stopped_at NOT NULL`；explicit resume清空stopped_at并增加sync_revision；live reactivation离开stopped但可保留旧stopped_at；import reactivation置active、清空时间并增加revision。`quarantined`不是终态契约的一员，不把它映射aborted。

DM-05/DM-14仅在主计划原(a)/(b)之一以真实交付证据满足后才能生产接线；b要求上游provider tests `agent_lifecycle_event_test::{terminal_consumer_contract_v1,explicit_cli_resume_consumer_contract_v1}`与本侧`memory_agent_session_test::stop_resume_reconciles_deletion`消费门，a要求ACF-08 done/complete并重核4 predicates。现在只有规范文本而没有测试交付，不能宣称b已满足。DM-04到原deadline仍未齐则按原规则回落a。可先推进DM-00/01/10/02/13/03/11/17/04本地验证，不为绕过依赖改capture writer，不把DSH gate当替代。

### 8.2 DEP-DM-07：后端文档交付

新增incoming依赖，owner为本任务主执行者负责准备、仓库维护者负责授权发布。公开命令卡必须同卡交付：EN、zh-CN、开发文档、README索引、COMPATIBILITY/error-codes以及backend `apps/tanstack-app/content/docs/commands/memory.en.md`。先获得可访问checkout，检查VCS唯一元数据、严格`cf`分支、干净状态与基线SHA；按后端AGENTS验收。缺checkout或认证时不得N/A，不提前把对应卡locally-accepted；独立内部卡可继续。

后端提交/推送/发布目前**无授权**：准备可review diff和运行本地校验，等用户授权后精确stage目标页，按后端签名/DCO规则提交，普通fast-forward push，记录前后SHA及后端部署证据。失败保留本地修改，不force、不覆盖他人提交；恢复用独立前滚文档提交。适用v2.9干净但超前cf例外时只用经核实的同remote隔离clone。

### 8.3 R08-G04：本地推进不等于验收完成

2026-09-27用户明确要求先做力所能及的相关测试并继续推进，大量测试留到PR阶段，并允许为恢复测试条件作有限Docker调整。本条替换此前“命中全量卡必须先完整本地验收，后继卡才能消费”的自设推进门；授权直接生效，不另设等待本轮文档review PASS的开工门。此次只改执行时序，不删卡、不增减AC/VER、不改变领域合同或正式验收状态定义。

**同分支本地消费条件：** 前置卡已有针对当前变更的独立code review字面PASS，以及实际执行的相关focused/nextest、fmt和clippy通过证据，且已登记全部已知失败及证据限制时，可描述为 `development-ready`，供后继内部卡本地实现使用。这不是Lifecycle或Acceptance的新枚举，不表示`locally-accepted`、完整验收、CI绿或发布完成；缺适用验收门的卡仍为`in-progress / 空`。后续每卡仍须做相关测试与独立review；有新变更或失败时按影响范围重测并区分新增回归、已复现的基线问题和未归因问题，不能以旧失败概括豁免新失败，不能跳过断言、弱化guard或修改CI/测试调度来造绿。新发现的P0/P1代码review阻断项仍须修复、复审。

**当前应用：** DM-01的C04 code-only review PASS、nextest迁移103/103及定向9/9、config/AC6与fmt/clippy已有真实证据，因此可作为development-ready前置，**允许DM-10按获准写集继续本地开发**。DM-01仍in-progress、Acceptance空；首次T1 FAIL、历史FAIL/INVALID、D01/D02的锁冲突及候选放大风险、stash flaky和容器前置问题全部保留，未被定向通过关闭。DM-10已按获准写集落盘，当前in-progress、Acceptance空，测试准备中；本条不是其通过声明。

**PR/最终验证与发布：** 保留各卡ER-13 full-suite trigger；大量全量及适用feature测试安排在PR准备/最终验证阶段，必须针对实际候选树取得真实证据。完整全量命令仍为 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`；失败、FLAKY、实际gate-helper skipped如实记录，回归前滚修复，最终重跑至绿。既有失败须有pin基线证据并保留FIX/DEFER归属；未归因不写成纯环境问题。不能用focused、cargo test --all或远端构建代替ER-13/14；发布前仍须在适用的bump后树执行C④及全部C/D门。无live凭据的operator决定与合法env文件已记录，不伪造live成功或将source失败视为通过。全部适用本地验收门与独立review满足后才可locally-accepted，C/D未齐不能done/complete。

**未改变的边界：** 所有本地DAG内部实现箭头按以上消费条件解释；DM-00 audit、DM-06真实价值/GO分支、DEP-DM-06/session合同（含deadline与provider/consumer门）、DEP-DM-07/backend同卡文档及其前置、DEP-DM-09真实语料不适用development-ready替代。DM-12仍要求子卡locally-accepted+review PASS+获准的合规提交；DM-09完整前置及发布队列不变。版本bump、提交、push、PR、merge、release仍需原独立授权；agent适配器实测不在本轮范围。有限Docker修复当前限定非特权subreaper，须最小、可恢复并保留数据与应用sandbox，不使用privileged、不关闭sandbox、不重启原容器，不泛化为任意安全配置变更或容器重建；本条不执行具体环境操作。

## 9. 任务拆分、写集与执行DAG

原编号保持owner；新编号只追加，不复用历史编号。下表只作导航与写集估算，不是执行卡或验收计数权威；唯一活动执行卡为 §12 的18张完整卡。ER-04/06通用门另列不计G-03，仍强制。每卡附自己的测试与docs，禁止分离补测试/补文档卡。

| 卡 | 修订后的唯一轴 / 专属实现写集 | 承接与验收主题（独立门上限8） |
|---|---|---|
| DM-00 | 基线/取材/退役，仅计划文档 | source inventory完整性；UTC远端取证；旧新schema区分；禁止模块清单；旧owner退役；状态同步 |
| DM-01 | 核心3表、migration registry、db入口preflight、ownership/GC登记 | 3表schema；list索引query plan；down；no-op；旧M2shape拒绝；role隔离；通用registry/ownership/GC守卫；4落点7生产文件 |
| DM-10 | path/search2表、registry、ownership/GC登记、只读schema兼容策略 | 2表schema；path索引；down；no-op；registry；旧支持上限入口策略；role隔离；4落点6生产文件 |
| DM-02 | 身份和commit纯派生，`memory/{mod,episode,error,derive_commit}.rs`、`ai/mod.rs` | UUID；digest；rowid；完整mapping；多revision聚合；first-parent窗口；输入预算；文本canary。自本卡拆出DM-13；2落点5文件 |
| DM-13（新增） | 从DM-02拆出新鲜度与原子reconcile，`memory/{mod,projection}.rs` | input变更检出；原子崩溃；并发；revoked；aged_out；增量=rebuild；missing/corrupt源；fail-closed。1落点2文件 |
| DM-03 | CLI注册与ReadOnly分类，原写集 | 保留注册、scope、census、no-operation、help、compat等门；family child，R=N/A；不要求后续子命令尚不存在时逐个执行 |
| DM-11 | status/list/show/rebuild命令与error interface | list/order；show/error；rebuild；staleflag；JSON；status diagnostics；error Display-pin；完整同卡文档通用门。family child，R=N/A |
| DM-12 | CLI家族唯一release点 | 前置locally-accepted非done；聚合、bump、安装、PR/CI/release证据继续保留、待授权 |
| DM-17（新增） | 从DM-04拆出lossless path派生，`memory/{paths,mod,projection}.rs` | raw bytes；合法/非法树；SHA-1/256；mode变化；rootcommit；容量预算；subtree剪枝预算；path层增量=rebuild。1落点3文件 |
| DM-04 | path查询/排序/drift，`memory/query.rs`、`command/memory.rs`、`memory/mod.rs` | 精确/目录查询；OS与hex入口；golden排序；drift三态；limit预算；路径四面canary；路径JSON；HEAD损坏失败。2落点3文件 |
| DM-05 | 会话纯派生，`memory/derive_agent_session.rs`、`memory/mod.rs` | predicate；unknown语义；anchor聚合；零checkpoint；无path；文本canary；scope隔离；budget。自本卡拆出DM-14；依赖DEP-DM-06；1落点2文件 |
| DM-14（新增） | 从DM-05拆出可撤销session投影接线，`memory/{projection,mod}.rs` | stop/resume；同秒终态；后补checkpoint；live/importreactivation；revoked/agedout；rebuild等价；CLI可达。继承DEP-DM-06；1落点2文件 |
| DM-16（新增） | DM-06原脚本迁出，`scripts/memory-value-gate.sh` | 采样独立；K<5；C轴truth table；S轴truth table；合并truth table；pin改变拒绝；完整byte复算。implementation，1落点1生产文件 |
| DM-06 | 价值audit，仅脱敏报告/owner状态 | 运行DM-16；记录两轴；完整复算；冻结结论；分支处置；不得新增脚本 |
| DM-07 | run来源投影，`memory/{derive_agent_run,projection,mod}.rs` | terminal全映射；paused排除；文本canary；scopepath；parse预算；findings消失恢复；原子source对账；symlink/损坏failclosed。仅GO，1落点3文件 |
| DM-08 | FTS持久index maintenance，`memory/fts.rs`、`memory/{projection,mod}.rs`、path_search_down.sql、ownership登记/扫描 | firstcreatebackfill；同事务posting；deleteoldvalues；integrity/rebuild；rollback；downshadow；恢复追平；role隔离。仅GO；自本卡拆出DM-15；3落点5文件 |
| DM-15（新增） | 从DM-08拆出search公开interface，`command/memory.rs`、`memory/{query,fts}.rs`、`utils/error.rs` | bm25golden；literal语法；input限额；降级仅search；双哨兵；queryJSON；errorDisplay；构建capability。仅GO；3落点4文件 |
| DM-09 | 原唯一收口release | 公共集合+对应分支；core及全文nextest；review闭环；历史/owner同步；PR现状与取材回执；全部待授权发布证据；无新行为 |

**本修订共18卡**：原13卡 + DM-13/14/15/16/17共5卡。GO=18；降级延后DM-07/08/15后公共集合=15。上述18个唯一任务ID与DAG须机械交叉核对。

执行顺序（内部实现依赖按§8.3的本地消费条件解释，不豁免外部合同、正式验收或发布门）：

`DM-00 → DM-01 → DM-10 → DM-02 → DM-13 → DM-03 → DM-11 → DM-17 → DM-04 → DM-05 → DM-14 → DM-16 → DM-06`；`DM-03 + DM-11 → DM-12` 是独立的CLI家族发布队列；DM-05/14额外依赖DEP-DM-06；公开命令本地验收额外依赖DEP-DM-07。GO分支`DM-06 → DM-07 → DM-08 → DM-15 → DM-09`；降级分支`DM-06 → DM-09`，DM-07/08/15正式移入DEFER-DM-12。DM-09最终complete仍等全部适用C/D与DM-12，不把本地DAG当发布完成。

所有Memory实现写集重叠，**生产编辑串行**。subagent可以并行只读审计、设计review和独立测试分析；获准编辑时每次只把一张卡的独占文件写集交给一个实现者。主执行者负责集成与状态文档，review者不发布。

## 10. R07逐项闭合表（提案，不预填PASS）

| R07项 | 本修订措施 | 接纳/执行证据要求 |
|---|---|---|
| P1-1 | §3两迁移与§6正确down路径 | owner中所有活动引用一致；registry/DDL/down测试 |
| P1-2 | v2.9计数+§9拆DM13/14/15/16/17 | 按独立谓词重审每卡，AC/VER<=8；不可只数checkbox |
| P1-3 | §8.3局部验收及family R=N/A | DAG无complete互等；C/D前不done |
| P1-4 | §4run解析与dirent预算分开 | 目录2000+ fixture计数，不调用list_runs_page |
| P1-5 | §4.2全部值，stopped unknown/quarantined不终态 | exhaustive truth table与现有source anchors |
| P1-6 | §4窗口、§3.3即时diagnostics | `revoked_sources_are_deleted`、`aged_out_sources_are_deleted`、`incremental_equals_same_snapshot_rebuild`（new） |
| P1-7 | §7S轴与合并truth table | fixture含零commit terminal session，无路径也可判值 |
| P1-8 | §7GO-only集合与§9降级DAG | 两分支各自要求清单，降级不要求run/search交付 |
| P1-9 | §9保持原ID并追加，owner七处同步 | owner DAG/依赖/REL/追溯/矩阵/里程碑/风险及history一致 |
| P1-10 | DM16 implementation拥有脚本，DM06 audit no-code | audit diff仅报告与状态 |
| P1-11 | §3typedSQL与§5BLOB/hex/OSbytes | nonUTF8存储、查询、JSON、rebuild、ANSI/OSC/换行/反斜杠canaries |
| P1-12 | DEP-DM-07具名跨repo提交验证/恢复 | cf/基线/本地验收证据，发布待授权不N/A |
| P2-1 | 保留不可恢复评审事实 | 可恢复位置补章名/锚点，不可恢复逐条标明，不伪造原文 |
| P2-2 | 仅禁止活动waiver | 历史EX原文保留，活动表为空 |
| P2-3 | §9生产文件与落点重算 | owner粒度表逐文件复算，文档/测试只从计数排除不从写集删除 |
| P2-4 | 锚点按符号刷新 | `OperationStatusV2`与middleware Running构造、store插入/update分别取行，不用start_ts行证明Running |

额外新增关闭义务：旧schema撞名的pre-DDL保护；rowid逐列重建；完整输入fingerprint；source单位统一；路径reader实际可保真；stopped不是成功；来源范围隔离；snapshot并发原子性；HEAD之外事实pin。独立review本轮确认的历史run范围解析、mode/type drift、全请求预算/重试、已有FTS恢复posting、巨大对象有界读取、价值门功能FAIL优先、list排序索引也作为正式修订项保留，不把发现过程抹去。它们全部属于本次Memory轴，不修无关上游模块。

## 11. 验证与接受记录

既有R08/G01–G03计划review、DM00及DM01代码/测试证据以owner原始回执为准，不反写旧hash或FAIL。R08-G04按用户授权更新本地推进规则，不预填本轮文字review结论，也不把该复核设为再次等待开工的门；独立一致性复核发现缺口仍须明确修正文档。后续代码review与各卡测试义务保持，任何进一步领域规范变更须先修本附件及owner映射。

全量执行时机按§8.3；命令仍是 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，必须记录失败与FLAKY，既有失败在pin基线复现，不把cargo test --all换成通过证据。每卡具名focused门在实现时确保真的注册并执行到非零测试；fmt/clippy全绿；测试target新增同步INDEX，兼容target同步Cargo.toml/compat README，CI与调度配置diff必须为空。

最终证据必须包含：完整取材清单、R08与代码独立review闭环、schema与旧库保护、5表重建等价、来源变更/撤销/并发、无损路径与HEAD drift、公共CLI与docs、上游会话契约解除、双轴价值报告及分支、条件FTS/run门、后端cf交付、全量nextest，以及仍未授权的C/D明确状态。不能用“核心单测通过”证明全部模块迁移完成。

## 12. 完整活动任务卡（执行权威）

### 卡片共享字段与强制门

以下是明确默认字段，不是省略验收。每卡只在偏离时重写：

- `Lifecycle / Acceptance = pending / 空`是冻结卡初始值；当前状态看owner最新回执和plan-status。内部实现依赖按§8.3消费，development-ready仅描述资格，不改卡的正式状态或AC/VER。
- `Release boundary = independent`，`Version increment = patch`，`Release write set = Cargo.toml, Cargo.lock, install.sh, install.ps1, release artifact`；集合以 `compat_version_surface_sync` 现场核对。family child/no-release的R=N/A，version=N/A；没有本轮发布授权。
- `C/D coverage from = self`，发布流程继承模板ER-04/08/14；不能用本地门替代远端门。
- 每卡 `Implementation write set` **追加两个精确协调路径** `docs/development/plan/plan-20260926.md`、`docs/development/plan/plan-status.md`（记录本卡状态/证据）；未改规范时不改本附件。主执行者串行维护协调文档。
- `Current evidence` 每卡符号锚点与下列G-05冻结file:line表共同构成证据；开工仍按目标pin重核。不存在的新文件只标(new)，不能作为现状证据。
- 全部命令在目标容器checkout执行。下列cargo test是本地focused门；首次创建函数必须标(new)、确认目标已注册、命令实际执行到至少1项而非0项。最终发布集成证据一律按ER-14用nextest相同过滤重跑。
- 统一非计数门：fmt/clippy；目标变更适用的ER-04表面门；ER-06/06a的逐文件文档/compat同步；新增target的INDEX/注册；源代码无不明unwrap/expect/panic；代码独立review PASS；状态写集与无CI/调度配置diff。ER-13 full-suite trigger单列，不能省略或计入专属VER凑数。
- 新schema通用机械守卫：`cargo test --lib internal::mutable_state_ownership::tests::mutable_state_scope_registration_guard -- --exact`、`cargo test --lib internal::mutable_state_ownership::tests::materialized_schema_tables_are_all_classified -- --exact`、`cargo test --test db_migration_test gc_object_source_inventory_covers_every_oid_column -- --exact`。DM-01/10各跑；DM-08另外扩展ownership既有materialized守卫为生产FTS初始化后的逐名双向断言，并覆盖DDL scanner的虚表定义。它们是Repository schema登记的既有契约，不以新表无scope列或projection非root为由省略。
- 全公开子命令的no-operation属于既有ER-04命令分类通用门：DM-11仍执行 `cargo test --test memory_cli_test base_commands_write_no_operation -- --exact`（new），DM-04/15同卡扩展此具名测试至各自新子命令；不因专属AC槽重分配而删门。
- 生产卡的安全/性能/回滚分别继承§3–8及owner ADR/GC，不能按本卡较短AC删除共享契约。每张schema卡的Down仅测试/受控运维，已应用用户库只能forward-fix。
- **文档集合D（逐路径展开）**：`docs/commands/memory.md`、`docs/commands/zh-CN/memory.md`、`docs/development/commands/memory.md`、`docs/commands/README.md`、`docs/commands/zh-CN/README.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`。标D的卡将以上8个精确路径全部加入I，同卡按实际内容更新并逐文件确认；无变更文件记录基线已准确的核查证据。backend映射由DEP-DM-07先核实，不存在/未授权不可N/A。另有错误码时显式加 `docs/error-codes.md`。

#### G-05冻结证据表（目标65763336，2026-09-26复核）

| 卡 | file:line证据；含义 |
|---|---|
| DM-00 | `docs/development/plan/plan-20260926-pr456-evidence.json:2` UTC与`:8` PR；source pin与完整机械清单见salvage，非实现证据 |
| DM-01 | `src/internal/db/migration.rs:291,1222,2324` runner/registry/count；`src/internal/mutable_state_ownership.rs:1054` materialized守卫；`tests/db_migration_test.rs:3825` OID inventory守卫；`src/internal/db/schema.rs:784`普通establish兼容捷径；`src/internal/db.rs:245`缓存再获取 |
| DM-10 | `src/internal/db/migration.rs:370` runner up-to；`src/internal/db/schema.rs:784`普通open兼容策略提取点；`tests/db_migration/role_scope.rs:109,218`既有env lane；`tests/db_migration_test.rs:120`模块注册 |
| DM-02 | `src/internal/change/store.rs:258`受限API；`src/internal/operation/store.rs:85`状态枚举；`src/internal/ai/review/sink.rs:149`sanitizer |
| DM-13 | `src/internal/revision_ordinal.rs:98,163`freshness/snapshot pin先例，不能照搬其较窄fingerprint |
| DM-03 | `src/cli.rs:54,353,1781,2106`help/enum/scope/boundary；`src/internal/operation/middleware.rs:92`分类 |
| DM-11 | `src/utils/error.rs:190`稳定码；`src/cli.rs:2106`无operation边界；Memory CLI为(new) |
| DM-12 | `.github/workflows/base.yml:1,10`PR触发/jobs；`.github/workflows/release.yml:3,14`发布流程，只读 |
| DM-17 | `src/internal/tree_plumbing.rs:309`String flatten；`src/utils/storage/mod.rs:74`fail-closed bounded trait；`src/utils/storage/local.rs:1712`限额读取；`src/utils/storage/tiered.rs:311`本地有界读取 |
| DM-04 | `src/command/status.rs:2729`现有byte quoting；`src/internal/tree_plumbing.rs:309`不得误用的lossy路径；query为(new) |
| DM-05 | `src/internal/ai/hooks/session_capture.rs:33`decide；`src/command/agent/session.rs:597`手工stop/resume；`docs/development/plan/plan-20260924.md:168`合同 |
| DM-14 | `src/command/agent/session.rs:604`scope-aware UPDATE+revision；`docs/development/plan/plan-20260924.md:116,168`镜像/合同；provider尚未交付 |
| DM-16 | `src/cli.rs:353`既有CLI入口；本附件§7算法是新规范，脚本/tests为(new)，不声称已有执行器 |
| DM-06 | PR JSON `:2`仅时间证据；真实source catalog依DEP-DM-09尚待提供，Git checkout不是Libra facts；报告(new) |
| DM-07 | `src/internal/ai/review/store.rs:53,175,481,676,715`终态/manifest/原地写/全扫；`src/internal/ai/investigate/store.rs:65,213`终态/manifest |
| DM-08 | `src/internal/mutable_state_ownership.rs:764,1054`source/materialized守卫漏lazy FTS；`Cargo.lock:3894`SQLite依赖，能力仍须测试 |
| DM-15 | `src/utils/error.rs:190`稳定码；`src/internal/ai/review/sink.rs:149`归一化边界；FTS/search为(new) |
| DM-09 | `.github/workflows/release.yml:3,14`远端门；PR JSON `:2,8`取证；未完成依赖不得据此声称收口 |

### Task DM-00：冻结来源与取材归属

**Task type:** audit。**Lifecycle / Acceptance:** pending / 空。
**Description:** 把唯一owner及取材来源固化为可核验文档，不改生产代码。**Out of scope:** 自动旧数据转换；再次关闭已关闭PR；生产移植。
**Current evidence:** 目标pin与源pin见§1；源分支三组迁移；主计划历史R30取代关系。
**Acceptance criteria（6）:**
1. 逐文件清单覆盖冻结来源的完整Memory相关改动集合。
2. 每行具有合法分类及确切承接卡或defer理由。
3. PR状态记录包含UTC时间和所用远端证据/降级证据。
4. 清单如实区分旧三迁移和新两迁移。
5. 原M2活动owner已退役，历史内容保留。
6. 主计划与状态账引用同一R08 owner。
**Verification（6，人工证据亦独立计门）:** `git -C "$MEMORY_SOURCE" rev-parse HEAD`等于来源pin；`git -C "$MEMORY_SOURCE" diff --name-only <经核实merge-base> <source-pin>`与salvage清单逐行set-diff为空；清单分类列枚举校验；PR证据UTC/链接人工核查；`grep -n 'plan-20260926' docs/development/plan/plan-20260819.md`；owner与plan-status双向链接核对。`MEMORY_SOURCE`是已核实只读来源Git目录，merge-base由git merge-base现场取得，不把占位符直接当命令运行。
**Dependencies:** DEP-DM-03、DEP-DM-05；计划review PASS。
**Deliverables / Implementation write set:** `docs/development/plan/plan-20260926-456-salvage.md`、`docs/development/plan/plan-20260926-pr456-evidence.json`、`docs/development/plan/plan-20260819.md`，加共享协调路径。远端JSON已准备，卡验收时核对pin与来源时间，不把历史checks当新迁移通过。
**Docs and compatibility impact:** 仅计划文档，无公开行为，D=N/A。
**Rollback mode:** revert。**Full-suite trigger:** N/A。**Estimated scope:** S。
**Release boundary / Version increment / Release write set:** no-release / N/A / N/A。**C/D coverage from:** DM-09。
**Granularity:** `type=audit; axis=来源归属; AC=6/20; VER=6/20; artifacts=5; scope=S; deps=DEP-DM-03,DEP-DM-05; release=no-release`。

### Task DM-01：核心投影schema

**Task type:** migration。**Lifecycle / Acceptance:** pending / 空。
**Description:** 交付§3的3张核心投影表。**Out of scope:** 路径/search表（DM-10）；派生（DM-02）；自动旧M2转换（非目标）。
**Current evidence:** `src/internal/db/migration.rs::repository_migrations`、`builtin_runner_registers_current_builtin_migrations`；`tests/db_migration/role_scope.rs`两项既有env lane。 `src/internal/config.rs:3419-3478`既有配置回退测试先建canonical DB后伪造latest receipt；2026092601下触发正确的未知Memory lineage拒绝，须仅修失真fixture。
**Acceptance criteria（8）:**
1. memory_episode满足§3列、PK、UNIQUE与CHECK规范。
2. memory_episode_evidence满足§3完整schema规范。
3. memory_projection_state满足§3完整schema规范。
4. down按FK顺序仅清除这3表。
5. 第二次run_pending不改变库或receipts。
6. 旧M2/未知同名shape在任何写入前被拒绝（含普通open、缓存再获取及升级）。 wrong-name输入变体必须具有正确2026092601版本与完整合法核心schema，仅改变receipt name；正向兼容fixture保留真实完整ledger，不放宽guard。
7. 迁移只在Repository角色生效。
8. list使用(repo_id,ended_at DESC,episode_id)索引且无额外排序。
**Verification（8，new，第7扩展既有lane）:**
1. `cargo test --test db_migration_test memory_core_episode_schema -- --exact`
2. `cargo test --test db_migration_test memory_core_evidence_schema -- --exact`
3. `cargo test --test db_migration_test memory_core_state_schema -- --exact`
4. `cargo test --test db_migration_test memory_core_down_is_scoped -- --exact`
5. `cargo test --test db_migration_test memory_core_second_run_is_noop -- --exact`
6. `cargo test --test db_migration_test memory_core_rejects_legacy_shape_without_mutation -- --exact`
7. `cargo test --test db_migration_test role_scope::role_scoped_schema_writer_keeps_ledgers_disjoint -- --exact`
8. `cargo test --test db_migration_test memory_list_index_is_used -- --exact`（EXPLAIN QUERY PLAN）
**Dependencies:** DM-00、DEP-DM-02、DEP-DM-04。
**Implementation write set:** `sql/migrations/2026092601_memory_core.sql`、`sql/migrations/2026092601_memory_core_down.sql`、`sql/migrations/README.md`、`src/internal/db/migration.rs`、`src/internal/db/schema.rs`（仅Repository schema-managed普通open共享只读preflight）、`src/internal/db.rs`（任何DDL前lineage preflight）、`src/internal/mutable_state_ownership.rs`、`src/command/maintenance.rs`（仅新列NonRoot登记）、`tests/db_migration_test.rs`、`tests/db_migration/role_scope.rs`、`tests/db_migration/branch_convergence.rs`（既有完整registry/count/tip机械守卫）、`src/internal/config.rs`（仅`#[cfg(test)]::legacy_config_fallback_tolerates_missing_table`的fixture/断言），加协调路径。没有serial registry或测试调度配置写集；不修改config生产函数、共享`write_schema_version` helper或原serial注解。
**既有registry联动守卫：** `cargo test --test db_migration_test branch_convergence::`须实际执行现有7项（不能0项）。仅同步本卡新增迁移后的完整registry数量、tail、applied列表及最新tip；历史分支58条回执及旧数据/回执不变、失败回滚原子性断言继续保留。`operation_boundary_claim_columns`的forward-only要求按该迁移version定位，不因Memory有受控down而删除旧断言。此项属于已有registry机械义务，不新增生产写集或专属AC/VER。
**既有配置兼容回归（ER-04 Rust单元表面门）：** `source .env.test && cargo test --lib internal::config::tests::legacy_config_fallback_tolerates_missing_table -- --exact`须实际执行1项。保留建库生成的真实完整receipts并确认max version等于latest；比较读取前后按序`version/name/applied_at`完全相等，用不触发schema管理的连接确认`config`读后仍不存在，保留原`None`与`legacy-main`断言。删除伪造ledger及失实pin-latest注释；AC6补正确version/完整核心shape但wrong receipt name的拒绝及零变更断言。此项为既有受影响表面回归，不新增产品AC/VER。
**Docs and compatibility impact:** 内部schema，无公开命令变化，D=N/A；migration README逐行说明旧库拒绝/前滚。迁移注册/count/tip属于模板ER-04既有通用门，另强制执行 `cargo test --lib internal::db::migration::tests::builtin_runner_registers_current_builtin_migrations -- --exact`，不因AC拆分消失。**Rollback mode:** forward-only。**Full-suite trigger:** T-1。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=migration; axis=核心schema; AC=8/8; VER=8/8; landing=4; prod-files=7; scope=M; deps=DM-00,DEP-DM-02,DEP-DM-04; release=independent`。 `config.rs`仅测试代码属于随附同步集，不增加7个生产文件或4个行为落点。
四个实现目录为 `sql/migrations`、`src/internal/db`、`src/internal`（含db.rs与mutable_state_ownership.rs）、`src/command`；不把同一目录中的两个文件重复算作两个落点。

### Task DM-10：路径与搜索文档schema

**Task type:** migration。**Lifecycle / Acceptance:** in-progress / 空。代码已按获准写集落盘，测试准备中；尚无本卡验收通过声明。
**Description:** 自DM-01拆出，交付2张依赖核心Episode的投影表。**Out of scope:** FTS虚表（DM-08）。
**Current evidence:** DM-01的§3 schema；`MigrationRunner`支持上限与普通open兼容策略；现有role fixture。裸`run_pending`的既有历史策略不作为future-schema拒绝入口，也不因本卡改写。
**Acceptance criteria（8）:**
1. memory_episode_path满足§3的BLOB契约。
2. memory_episode_search_doc满足§3的rowid契约。
3. 路径查询命中指定索引。
4. down仅删除本迁移表。
5. 第二次run_pending为no-op。
6. 用截断到2026092601的runner支持上限，调用与普通open相同的只读兼容策略，对完整2026092602库返回future-schema拒绝且schema/行/receipts不变；这是旧支持上限的生产入口策略路径，不是运行旧编译产物。
7. 完整runner能够从目标旧tip升级。
8. 配置库shape与版本不受迁移影响。
**Verification（8，new，最后扩展既有lane）:**
1. `cargo test --test db_migration_test memory_path_schema_is_byte_faithful -- --exact`
2. `cargo test --test db_migration_test memory_search_doc_schema -- --exact`
3. `cargo test --test db_migration_test memory_path_index_is_used -- --exact`
4. `cargo test --test db_migration_test memory_path_search_down_is_scoped -- --exact`
5. `cargo test --test db_migration_test memory_path_search_second_run_is_noop -- --exact`
6. `cargo test --test db_migration_test memory_old_runner_rejects_future_schema -- --exact`（2026092601支持上限 + 生产入口共享helper；比较拒绝前后schema/行/receipts，无真实旧binary）
7. `cargo test --test db_migration_test memory_new_runner_upgrades_target_baseline -- --exact`
8. `cargo test --test db_migration_test role_scope::global_and_system_bootstrap_create_no_repository_shape -- --exact`
**Dependencies:** DM-01、DEP-DM-02、DEP-DM-04。
**Implementation write set（10文件）:** `sql/migrations/2026092602_memory_path_search.sql`、`sql/migrations/2026092602_memory_path_search_down.sql`、`sql/migrations/README.md`、`src/internal/db/migration.rs`、`src/internal/db/schema.rs`（仅下述只读兼容策略helper与普通open复用）、`src/internal/mutable_state_ownership.rs`、`src/command/maintenance.rs`（仅新列NonRoot登记）、`tests/db_migration_test.rs`、`tests/db_migration/role_scope.rs`、`tests/db_migration/branch_convergence.rs`（既有完整registry/count/tip机械守卫），加协调路径。
**只读兼容策略边界：** 仅抽取`#[doc(hidden)] check_schema_support_for_connection(conn, role, latest_supported) -> SchemaCompatibility / Err`；普通open传builtin上限并复用原future-schema拒绝。helper不执行DDL、不返回新连接；inspect的`UnsupportedFuture`分类接口不变，裸`run_pending`历史行为不变。AC6以截断runner的2026092601上限调用同一helper验证2026092602库拒绝且无损，不改变生产支持上限，不把它宣称为真实旧binary gate；后者仍属DEFER-DM-10。
**既有registry联动守卫：** `cargo test --test db_migration_test branch_convergence::`须实际执行现有7项（不能0项）。仅同步本卡新增迁移后的完整registry数量、tail、applied列表及最新tip；历史分支58条回执及旧数据/回执不变、失败回滚原子性断言继续保留。`operation_boundary_claim_columns`的forward-only要求按该迁移version定位，不因Memory有受控down而删除旧断言。此项属于已有registry机械义务，不新增生产写集或专属AC/VER。
**Docs and compatibility impact:** D=N/A（内部schema）；README记录兼容矩阵。registry/count同步及其既有ER-04迁移守卫为通用门，不删。**Rollback mode:** forward-only。**Full-suite trigger:** T-1。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=migration; axis=路径schema; AC=8/8; VER=8/8; landing=4; prod-files=6; scope=M; deps=DM-01,DEP-DM-02,DEP-DM-04; release=independent; split-from=DM-01`。 四落点仍为`sql/migrations`、`src/internal/db`、`src/internal`、`src/command`；schema.rs与migration.rs同目录，不增加落点。

### Task DM-02：身份与commit纯派生

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 生成确定性Episode值，不负责持久发布。**Out of scope:** 新鲜度/事务（拆出DM-13）；path（DM-17）；CLI（DM-03/11）。
**Current evidence:** `ChangeStore::revisions_for_repo`的200上限、`OperationStatusV2`、`review::sink::render_untrusted_findings`。
**Acceptance criteria（8）:**
1. UUID身份符合§3 golden vector。
2. 内容摘要符合canonical序列化向量。
3. rowid符合稳定映射及碰撞拒绝规范。
4. operation值的派生truth table完整。
5. 多revision聚合符合§4的golden row。
6. 候选窗口严格采用HEAD first-parent顺序。
7. revision读取不超过冻结预算。
8. commit文本入值前完成规范sanitization。
**Verification（8，new）:** `cargo test --lib internal::ai::memory::episode::tests::uuid_vectors -- --exact`；`cargo test --lib internal::ai::memory::episode::tests::digest_vectors -- --exact`；`cargo test --lib internal::ai::memory::episode::tests::rowid_vectors_and_collision -- --exact`；`cargo test --lib internal::ai::memory::derive_commit::tests::operation_mapping -- --exact`；`cargo test --lib internal::ai::memory::derive_commit::tests::multi_revision_golden -- --exact`；`cargo test --lib internal::ai::memory::derive_commit::tests::first_parent_window -- --exact`；`cargo test --lib internal::ai::memory::derive_commit::tests::revision_budget -- --exact`；`cargo test --lib internal::ai::memory::derive_commit::tests::text_canary -- --exact`。
**Dependencies:** DM-10、DEP-DM-04。
**Implementation write set:** `src/internal/ai/memory/mod.rs`、`src/internal/ai/memory/episode.rs`、`src/internal/ai/memory/error.rs`、`src/internal/ai/memory/derive_commit.rs`、`src/internal/ai/mod.rs`，加协调路径。
**Docs and compatibility impact:** D=N/A（私有派生未暴露命令）。**Rollback mode:** revert。**Full-suite trigger:** T-5（首次建立Memory module）。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=commit派生; AC=8/8; VER=8/8; landing=2; prod-files=5; scope=M; deps=DM-10,DEP-DM-04; release=independent; split-out=DM-13`。

### Task DM-13：原子投影与新鲜度

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 自DM-02拆出，将确定性派生值原子发布成当前窗口缓存。**Out of scope:** source adapter新语义与公开命令。
**Current evidence:** `RevisionOrdinalIndex`的freshness模式；§3/4完整snapshot契约。
**Acceptance criteria（8）:**
1. 任何参与派生的输入变更使snapshot失效。
2. 注入写入失败不留下半代投影。
3. 并发不同HEAD请求不会读到另一快照结果。
4. revoked集合被级联删除。
5. aged_out集合被级联删除。
6. 同snapshot下增量与rebuild逐行一致。
7. 缺失/损坏来源不被误当空数据。
8. 重试耗尽返回明确stale错误。
**Verification（8，new，target同卡注册）:** `cargo test --test memory_projection_test fingerprint_covers_all_inputs -- --exact`；`cargo test --test memory_projection_test failed_publish_is_atomic -- --exact`；`cargo test --test memory_projection_test concurrent_heads_are_isolated -- --exact`；`cargo test --test memory_projection_test revoked_sources_are_deleted -- --exact`；`cargo test --test memory_projection_test aged_out_sources_are_deleted -- --exact`；`cargo test --test memory_projection_test incremental_equals_same_snapshot_rebuild -- --exact`；`cargo test --test memory_projection_test corrupt_source_fails_closed -- --exact`；`cargo test --test memory_projection_test retry_budget_is_finite -- --exact`。
**Dependencies:** DM-02、DEP-DM-04。
**Implementation write set:** `src/internal/ai/memory/mod.rs`、`src/internal/ai/memory/projection.rs`、`tests/memory_projection_test.rs`、`tests/INDEX.md`、`Cargo.toml`（仅新test注册如所需），加协调路径。
**Docs and compatibility impact:** D=N/A（内部缓存interface）。**Rollback mode:** revert。**Full-suite trigger:** T-1（单一投影source-of-truth）。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=投影发布; AC=8/8; VER=8/8; landing=1; prod-files=2; scope=M; deps=DM-02,DEP-DM-04; release=independent; split-from=DM-02`。

### Task DM-03：CLI注册与分类

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 注册隐藏在本地family窗口内的memory命令骨架。**Out of scope:** 可执行子命令由DM-11交付；本卡单独不发布。
**Current evidence:** `src/cli.rs::Commands/command_scope/ROOT_AFTER_HELP`；operation `classify_command`；compat命令清单。
**Acceptance criteria（4）:**
1. clap注册与dispatch可编译。
2. command_scope为ReadOnly。
3. operation census分类为ReadOnly。
4. command_has_existing_operation_boundary不包含memory。
**Verification（4，new）:** `cargo test --lib cli::tests::memory_registration_compiles -- --exact`；`cargo test --lib cli::tests::memory_scope_is_read_only -- --exact`；`cargo test --test operation_command_coverage memory_census_is_read_only -- --exact`；`cargo test --test operation_command_coverage memory_has_no_mutation_boundary -- --exact`。
**Dependencies:** DM-13、DEP-DM-01、DEP-DM-04。
**Implementation write set:** `src/command/memory.rs`、`src/command/mod.rs`、`src/cli.rs`、`src/internal/operation/middleware.rs`、`COMPATIBILITY.md`、`tests/compat/help_examples_banner.rs`、`tests/operation_command_coverage.rs`，加协调路径。
**Docs and compatibility impact:** COMPATIBILITY/help/EXAMPLES为ER-04/06同步门；D其余由同一不发布family的DM-11交付。不是公开无文档命令的N/A豁免。
**Rollback mode:** revert。**Full-suite trigger:** T-1。**Estimated scope:** M。
**Release boundary / Version increment / Release write set:** family child REL-DM-02 / N/A / N/A。**C/D coverage from:** DM-12。
**Granularity:** `type=implementation; axis=CLI注册; AC=4/8; VER=4/8; landing=3; prod-files=4; scope=M; deps=DM-13,DEP-DM-01,DEP-DM-04; release=family-child`。

### Task DM-11：基础查询CLI

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 自DM-03拆出，交付status/list/show/rebuild的完整可用interface。**Out of scope:** recall/search及family发布。
**Current evidence:** DM-03骨架、DM-13投影interface、`StableErrorCode`、现有CLI error renderer。
**Acceptance criteria（8）:**
1. list采用冻结分页/排序。
2. show对未知id返回002。
3. rebuild符合5表比较口径。
4. stale/allow-stale状态机符合§5。
5. JSON外壳稳定。
6. status诊断不伪报历史计数。
7. 001/002的Display稳定。
8. temporal证据不进入citation字段。
**Verification（8，new）:** `cargo test --test memory_cli_test list_order -- --exact`；`cargo test --test memory_cli_test show_missing -- --exact`；`cargo test --test memory_cli_test rebuild_contract -- --exact`；`cargo test --test memory_cli_test stale_contract -- --exact`；`cargo test --test memory_cli_test json_contract -- --exact`；`cargo test --test memory_cli_test status_contract -- --exact`；`cargo test --test memory_cli_test error_display_contract -- --exact`；`cargo test --test memory_cli_test temporal_not_citation -- --exact`。路径尚未公开的JSON模型断言属于第5门，后续DM-17仍运行此guard。
**Dependencies:** DM-03、DEP-DM-04、DEP-DM-07。
**Implementation write set:** `src/command/memory.rs`、`src/utils/error.rs`、`docs/error-codes.md`、`tests/memory_cli_test.rs`、`tests/INDEX.md`、`Cargo.toml`（test注册），D的8路径，加协调路径。采用独立temp/subprocess target，不新增cwd/env lane。
**Docs and compatibility impact:** D全部新建/同步，001/002逐条错误码；backend需cf证据。
**Rollback mode:** revert。**Full-suite trigger:** T-1。**Estimated scope:** M。
**Release boundary / Version increment / Release write set:** family child REL-DM-02 / N/A / N/A。**C/D coverage from:** DM-12。
**Granularity:** `type=implementation; axis=基础查询CLI; AC=8/8; VER=8/8; landing=2; prod-files=2; scope=M; deps=DM-03,DEP-DM-04,DEP-DM-07; release=family-child; split-from=DM-03`。

### Task DM-12：CLI家族发布点

**Task type:** release。**Lifecycle / Acceptance:** pending / 空。
**Description:** 仅执行REL-DM-02发布，不新增行为。**Out of scope:** 本轮未授权任何发布动作。
**Current evidence:** DM-03/11本地验收和review记录；发布时重新检查workflow真实jobs。
**Acceptance criteria（9）:**
1. 子卡均locally-accepted，不要求它们先done。
2. 子卡独立review均PASS。
3. 已获准本地提交签名可验证。
4. 同一提交有正确DCO trailer。
5. base.yml对应冻结PR head全绿。
6. CodeQL对应同一PR head全绿。
7. release/artifact对应冻结merge SHA。
8. CDN产物对应本次发布。
9. Homebrew版本/安装证据对应本次发布。
**Verification（9）:** 子卡验收记录核查；review记录核查；对获准且冻结的提交执行git verify-commit；核查该提交Signed-off-by；核查同head的base.yml全部实际required jobs；独立核查同head的CodeQL；gh run view已冻结release run的headSha/status/conclusion/jobs/url并核artifact；核CDN checksum/version；核Homebrew安装/version。PR、commit、run具体ID在获得发布授权后从真实结果写入执行记录，当前无可执行ID，不伪造。模板通用bump/fmt/clippy/全量/build/install门仍全部保留，不在此重复计数。
**Dependencies:** DM-03、DM-11、DEP-DM-04、DEP-DM-07、DEP-DM-08（发布授权）。
**Deliverables / Implementation write set:** 发布证据写入owner，加共享协调路径；R为版本面及artifact。
**Docs and compatibility impact:** 核实D与后端交付版本，不新增文档行为。**Rollback mode:** immutable-release。**Full-suite trigger:** T-2。**Estimated scope:** S。**Version increment:** patch。**Release boundary:** family release point REL-DM-02。**C/D coverage from:** self。
**Granularity:** `type=release; axis=CLI发布; AC=9/12; VER=9/12; landing=1; prod-files=0; scope=S; deps=DM-03,DM-11,DEP-DM-04,DEP-DM-07,DEP-DM-08; release=family-point`。

### Task DM-17：字节保真路径派生

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 自DM-04拆出，计算可重建的路径变化值并进入投影。**Out of scope:** recall公开查询由DM-04交付；不改通用lossy parser。
**Current evidence:** git-internal TreeItem String/GBK实现；status quoting helper；§5raw-tree规范。
**Acceptance criteria（8）:**
1. 非UTF8路径存储和重建无损。
2. malformed tree输入返回错误。
3. SHA-1/256 OID宽度解释正确。
4. mode-only修改被识别。
5. rootcommit与空树diff正确。
6. 共享容量Budget从不超过§5冻结上限（giant-root、deep-tree、累计与重试fixture）。
7. 相同subtree剪枝符合既定I/O读次数上限。
8. path投影增量等于rebuild。
**Verification（8，new）:** `cargo test --test memory_paths_test non_utf8_roundtrip -- --exact`；`cargo test --test memory_paths_test malformed_tree_rejected -- --exact`；`cargo test --test memory_paths_test object_hash_widths -- --exact`；`cargo test --test memory_paths_test mode_only_change -- --exact`；`cargo test --test memory_paths_test root_commit_diff -- --exact`；`cargo test --test memory_paths_test request_budget_vector -- --exact`（含上述具名fixture）；`cargo test --test memory_paths_test unchanged_subtrees_are_pruned -- --exact`；`cargo test --test memory_paths_test incremental_path_rebuild_equivalence -- --exact`。
**Dependencies:** DM-11、DEP-DM-04。
**Implementation write set:** `src/internal/ai/memory/paths.rs`、`src/internal/ai/memory/mod.rs`、`src/internal/ai/memory/projection.rs`、`tests/memory_paths_test.rs`、`tests/memory_cli_test.rs`（保持未公开path的回归）、`tests/INDEX.md`、`Cargo.toml`（test注册），加协调路径。
**Docs and compatibility impact:** D=N/A：私有路径派生尚未接入公开返回模型，DM-04首次公开并同卡说明BLOB/hex、rename和HEAD语义；不是已公开行为缺文档。**Rollback mode:** revert。**Full-suite trigger:** T-1（projection共享单一实现）。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=路径派生; AC=8/8; VER=8/8; landing=1; prod-files=3; scope=M; deps=DM-11,DEP-DM-04; release=independent; split-from=DM-04`。

### Task DM-04：recall与drift

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 为已有path投影提供冻结的查询interface。**Out of scope:** 树解析由拆出的DM-17承接；全文search不在本卡。
**Current evidence:** DM-17path投影、DM-11CLI、§5selector_v1。
**Acceptance criteria（8）:**
1. 文件/目录查询按segment匹配。
2. OSpath与hex精确入口语义等价。
3. 排序命中冻结golden顺序。
4. 三态drift按HEAD的OID+mode计算（包含chmod/regular→symlink同OID）。
5. drift读取次数<=limit。
6. 首次公开path的全部输出面canary无终端控制注入。
7. raw-path JSON可无损恢复。
8. HEAD对象损坏不伪报path-gone。
**Verification（8，new）:** `cargo test --test memory_recall_test segment_matching -- --exact`；`cargo test --test memory_recall_test os_and_hex_paths -- --exact`；`cargo test --test memory_recall_test frozen_order -- --exact`；`cargo test --test memory_recall_test drift_states -- --exact`；`cargo test --test memory_recall_test drift_budget -- --exact`；`cargo test --test memory_recall_test quoting_canaries -- --exact`（raw存储、JSON、paths_text、人读四面）；`cargo test --test memory_recall_test raw_path_json -- --exact`；`cargo test --test memory_recall_test corrupt_head_fails_closed -- --exact`。
**Dependencies:** DM-17、DEP-DM-04、DEP-DM-07；公开发布仍等DM-12，local可消费已review的CLI。
**Implementation write set:** `src/internal/ai/memory/query.rs`、`src/internal/ai/memory/mod.rs`、`src/command/memory.rs`、`tests/memory_recall_test.rs`、`tests/memory_cli_test.rs`、`tests/INDEX.md`、`Cargo.toml`（test注册），D的8路径，加协调路径。
**Docs and compatibility impact:** D同步recall/ospath/hex/limit/outcome及HEAD非worktree语义；扩展现有no-operation通用门至recall。**Rollback mode:** revert。**Full-suite trigger:** T-5。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=路径查询; AC=8/8; VER=8/8; landing=2; prod-files=3; scope=M; deps=DM-17,DEP-DM-04,DEP-DM-07; release=independent; split-out=DM-17`。

### Task DM-05：终态session纯派生

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 依赖上游合同读取会话事实，产出不含path的值。**Out of scope:** 来源消费接线/撤销对账由拆出DM-14承接。
**Current evidence:** plan-20260924 CTR-ACF-DM06-v1；hooks/session_capture.rs；CLI session stop/resume。
**Acceptance criteria（8）:**
1. 每种session state的产出truth table符合§4。
2. 完整golden row不虚构任务成功。
3. anchor遵循checkpoint确定性选择。
4. 零checkpoint会话仍可派生。
5. 该来源始终不产生path。
6. description canary被归一化。
7. foreign/legacy_unknown来源被排除。
8. checkpoint读取预算可执行。
**Verification（8，new）:** `cargo test --lib internal::ai::memory::derive_agent_session::tests::state_mapping -- --exact`；`cargo test --lib internal::ai::memory::derive_agent_session::tests::golden_row -- --exact`；`cargo test --lib internal::ai::memory::derive_agent_session::tests::anchor_order -- --exact`；`cargo test --lib internal::ai::memory::derive_agent_session::tests::zero_checkpoint -- --exact`；`cargo test --lib internal::ai::memory::derive_agent_session::tests::no_paths -- --exact`；`cargo test --lib internal::ai::memory::derive_agent_session::tests::description_canary -- --exact`；`cargo test --lib internal::ai::memory::derive_agent_session::tests::scope_isolation -- --exact`；`cargo test --lib internal::ai::memory::derive_agent_session::tests::checkpoint_budget -- --exact`。
**Dependencies:** DM-04、DEP-DM-04、DEP-DM-06。未解除DEP不得写本卡生产代码。
**Implementation write set:** `src/internal/ai/memory/derive_agent_session.rs`、`src/internal/ai/memory/mod.rs`，加协调路径。
**Docs and compatibility impact:** D=N/A（纯派生尚未接入公开查询）；DM-14同卡交付公开结果文档。
**Rollback mode:** revert。**Full-suite trigger:** T-5。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=session派生; AC=8/8; VER=8/8; landing=1; prod-files=2; scope=M; deps=DM-04,DEP-DM-04,DEP-DM-06; release=independent; split-out=DM-14`。

### Task DM-14：可撤销session消费接线

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 自DM-05拆出，将既有终态会话派生接入投影和查询。**Out of scope:** 不修改capture writer，不做真实外部agent联调。
**Current evidence:** CTR四predicate；DM-05纯派生与DM-13reconcile。
**Acceptance criteria（8）:**
1. explicit stop→resume移除Episode。
2. 同秒重复终态化变化被捕获。
3. stop后追加checkpoint使projection更新。
4. import reactivation移除旧Episode。
5. live reactivation矩阵移除旧Episode。
6. session出窗按aged_out删除。
7. 会话增量结果等于同源rebuild。
8. 零提交session经CLI list/show可达。
**Verification（8，new，首条保持双向合同名）:** `cargo test --test memory_agent_session_test stop_resume_reconciles_deletion -- --exact`；`cargo test --test memory_agent_session_test same_second_terminalization -- --exact`；`cargo test --test memory_agent_session_test checkpoint_after_stop -- --exact`；`cargo test --test memory_agent_session_test import_reactivation -- --exact`；`cargo test --test memory_agent_session_test live_reactivation_matrix -- --exact`；`cargo test --test memory_agent_session_test session_aged_out -- --exact`；`cargo test --test memory_agent_session_test session_incremental_equals_rebuild -- --exact`；`cargo test --test memory_agent_session_test zero_commit_cli_reachable -- --exact`。
**Dependencies:** DM-05、DEP-DM-04、DEP-DM-06、DEP-DM-07。
**Implementation write set:** `src/internal/ai/memory/projection.rs`、`src/internal/ai/memory/mod.rs`、`tests/memory_agent_session_test.rs`、`tests/INDEX.md`、`Cargo.toml`（test注册），D的8路径，加协调路径。
**Docs and compatibility impact:** D说明agent_session来源、outcome unknown、actor null、无path、resume删除与scope排除。
**Rollback mode:** revert。**Full-suite trigger:** T-5。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=session消费; AC=8/8; VER=8/8; landing=1; prod-files=2; scope=M; deps=DM-05,DEP-DM-04,DEP-DM-06,DEP-DM-07; release=independent; split-from=DM-05`。

### Task DM-16：价值门可复算执行器

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 从DM-06迁出可执行代码，交付不篡改来源的审计工具。**Out of scope:** 不替真实value audit伪造GO。
**Current evidence:** §7冻结算法、DM-04/14CLI输出。
**Acceptance criteria（8）:**
1. 样本不读取Memory结果进行挑选。
2. K<5时使用实际样本数。
3. C轴truth table完整。
4. S轴truth table完整。
5. 两轴合并truth table完整，功能FAIL优先于任何另一轴GO。
6. HEAD或source pin漂移拒绝复算。
7. 同输入完整输出byte-identical。
8. 非UTF8抽样路径无损。
**Verification（8，new）:** `cargo test --test memory_value_gate_test selection_independent -- --exact`；`cargo test --test memory_value_gate_test fewer_than_five -- --exact`；`cargo test --test memory_value_gate_test commit_axis -- --exact`；`cargo test --test memory_value_gate_test session_axis -- --exact`；`cargo test --test memory_value_gate_test combined_verdict -- --exact`（具名数据行C_GO_S_SEMANTIC_FAIL、C_SEMANTIC_FAIL_S_GO以及missing/wrong_attribution/evidence_count）；`cargo test --test memory_value_gate_test pin_drift_rejected -- --exact`；`cargo test --test memory_value_gate_test report_is_byte_identical -- --exact`；`cargo test --test memory_value_gate_test raw_path_sampling -- --exact`。
**Dependencies:** DM-14、DEP-DM-04。
**Implementation write set:** `scripts/memory-value-gate.sh`、`tests/memory_value_gate_test.rs`、`tests/INDEX.md`、`Cargo.toml`（test注册），加协调路径。
**Docs and compatibility impact:** D=N/A（开发审计脚本，不改变用户memory命令）；执行方法即§7。**Rollback mode:** revert。**Full-suite trigger:** T-1（Cargo.toml非版本注册变更）。**Estimated scope:** S。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=价值审计执行器; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=S; deps=DM-14,DEP-DM-04; release=independent; split-from=DM-06`。

### Task DM-06：真实价值审计

**Task type:** audit。**Lifecycle / Acceptance:** pending / 空。
**Description:** 运行DM-16并归档唯一分支判定。**Out of scope:** 无代码/配置；不把fixture gate等同真实价值。
**Current evidence:** DM-16已验证执行器、具备来源catalog的可读Libra仓库快照。
**Acceptance criteria（6）:**
1. 报告记录完整HEAD与来源pin。
2. 实际样本完全来自冻结算法。
3. C/S轴计数有原始命令证据。
4. 综合结论符合truth table。
5. 完整报告可同源byte复算。
6. 未选分支卡归属被正式修订。
**Verification（6）:** 报告pin人工核验；样本名单set-diff；每样本命令计数核查；truth table人工复算；`sh scripts/memory-value-gate.sh <HEAD-pin> <source-pin>`规范输出与报告byte-diff；owner/status的GO或DEFER清单核查。参数从报告严格解析，不允空pin。
**Dependencies:** DM-16；真实来源样本可用性记录在DEP-DM-09。
**Deliverables / Implementation write set:** `docs/development/plan/plan-20260926-value-gate.md`，加协调路径；NO-GO需同步本附件§9分支归属。
**Docs and compatibility impact:** D=N/A（审计阶段不改用户命令）；降级声明由DM-09交付。**Rollback mode:** revert。**Full-suite trigger:** N/A。**Estimated scope:** S。
**Release boundary / Version increment / Release write set:** no-release / N/A / N/A。**C/D coverage from:** DM-09。
**Granularity:** `type=audit; axis=价值判定; AC=6/20; VER=6/20; artifacts=4; scope=S; deps=DM-16,DEP-DM-09; release=no-release; split-out=DM-16`。

### Task DM-07：run来源投影

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 仅GO分支接入review/investigate现存终态manifest。**Out of scope:** 不改writer，不读raw transcript，不实现FTS。
**Current evidence:** `ReviewManifest/ReviewTerminalState`、`InvestigateManifest/InvestigateTerminalState`、state/manifest写入与findings对象化。
**Acceptance criteria（8）:**
1. 每种run终态映射符合§4。
2. paused investigate不产生Episode。
3. findings正文canary归一化。
4. scope路径仅按starting_sha可冻结的历史端点派生，HEAD/branch后移不改变旧run归因。
5. 枚举解析符合计数/字节预算。
6. findings对象消失/恢复正确降级恢复。
7. run增量与rebuild一致。
8. symlink或损坏/不一致来源不发布为fresh。
**Verification（8，new）:** `cargo test --test memory_agent_run_test terminal_mapping -- --exact`；`cargo test --test memory_agent_run_test paused_not_terminal -- --exact`；`cargo test --test memory_agent_run_test findings_canary -- --exact`；`cargo test --test memory_agent_run_test historical_scope_paths -- --exact`（advanceHEAD/retargetref/natural language）；`cargo test --test memory_agent_run_test parse_budget -- --exact`；`cargo test --test memory_agent_run_test findings_disappear_recover -- --exact`；`cargo test --test memory_agent_run_test run_incremental_equals_rebuild -- --exact`；`cargo test --test memory_agent_run_test unsafe_source_rejected -- --exact`。
**Dependencies:** DM-06 GO、DEP-DM-04、DEP-DM-07。
**Implementation write set:** `src/internal/ai/memory/derive_agent_run.rs`、`src/internal/ai/memory/projection.rs`、`src/internal/ai/memory/mod.rs`、`tests/memory_agent_run_test.rs`、`tests/INDEX.md`、`Cargo.toml`（test注册），D的8路径，加协调路径。
**Docs and compatibility impact:** D同步run来源、解析预算、截断语义、findings unresolved，以及不扩大披露来源。**Rollback mode:** revert。**Full-suite trigger:** T-5。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=run投影; AC=8/8; VER=8/8; landing=1; prod-files=3; scope=M; deps=DM-06(GO),DEP-DM-04,DEP-DM-07; release=independent`。

### Task DM-08：FTS持久索引

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 仅GO分支建立可重建external-content posting。**Out of scope:** search公开interface拆出DM-15；不改CI探针。
**Current evidence:** 新内容表DM-10、bundledSQLite FTS5、原external-content协议取材。
**Acceptance criteria（8）:**
1. 首建FTS回填既有内容。
2. posting与内容同事务发布。
3. 删除旧值符合FTS协议。
4. integrity/rebuild能检出并恢复坏posting。
5. 注入失败不会留下半代index。
6. down无FTS/shadow残留。
7. 已有FTS经历不可用窗口后跨进程恢复到当前基础投影。
8. 配置库不存在FTS/top-up副作用。
**Verification（8，new，最后扩展既有lane）:** `cargo test --test memory_fts_test first_create_backfills -- --exact`；`cargo test --test memory_fts_test posting_transaction -- --exact`；`cargo test --test memory_fts_test delete_old_values -- --exact`；`cargo test --test memory_fts_test integrity_rebuild -- --exact`；`cargo test --test memory_fts_test posting_rollback -- --exact`；`cargo test --test db_migration_test memory_down_has_no_fts_shadow -- --exact`；`cargo test --test memory_fts_test unavailable_window_recovers_existing_postings -- --exact`；`cargo test --test db_migration_test role_scope::role_scoped_schema_writer_keeps_ledgers_disjoint -- --exact`。
**Dependencies:** DM-07、DEP-DM-04、DEP-DM-07。
**Implementation write set:** `src/internal/ai/memory/fts.rs`、`src/internal/ai/memory/projection.rs`、`src/internal/ai/memory/mod.rs`、`sql/migrations/2026092602_memory_path_search_down.sql`、`src/internal/mutable_state_ownership.rs`（FTS/shadow逐名登记、scanner及实化guard）、`tests/memory_fts_test.rs`、`tests/db_migration_test.rs`、`tests/db_migration/role_scope.rs`、`tests/INDEX.md`、`Cargo.toml`（test注册），D的8路径，加协调路径。
**Docs and compatibility impact:** D同卡说明status/rebuild的FTS诊断，不能提前宣传search存在。**Rollback mode:** forward-only。**Full-suite trigger:** T-1。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=FTS维护; AC=8/8; VER=8/8; landing=3; prod-files=5; scope=M; deps=DM-07,DEP-DM-04,DEP-DM-07; release=independent; split-out=DM-15`。

### Task DM-15：search公开interface

**Task type:** implementation。**Lifecycle / Acceptance:** pending / 空。
**Description:** 自DM-08拆出，交付冻结BM25与安全查询入口。**Out of scope:** 不调整ranking权重，不扩大来源。
**Current evidence:** DM-08索引、§6双引号phrase规则；StableErrorCode。
**Acceptance criteria（8）:**
1. BM25排序符合golden。
2. 操作符输入仅按字面量解释。
3. 4KiB/256B/32词及空query限制明确。
4. FTS不可用只降级search。
5. 失败注入仅双env同时生效。
6. search JSON稳定。
7. 003/004 Display稳定。
8. 当前构建的SQLite具备FTS5。
**Verification（8，new）:** `cargo test --test memory_search_test bm25_order -- --exact`；`cargo test --test memory_search_test literal_escaping -- --exact`；`cargo test --test memory_search_test input_limits -- --exact`；`cargo test --test memory_search_test search_only_degradation -- --exact`；`cargo test --test memory_search_test dual_sentinel -- --exact`；`cargo test --test memory_search_test search_json -- --exact`；`cargo test --test memory_search_test search_error_display -- --exact`；`cargo test --test memory_search_test bundled_fts5 -- --exact`。
**Dependencies:** DM-08、DEP-DM-04、DEP-DM-07。
**Implementation write set:** `src/command/memory.rs`、`src/internal/ai/memory/query.rs`、`src/internal/ai/memory/fts.rs`、`src/utils/error.rs`、`docs/error-codes.md`、`tests/memory_search_test.rs`、`tests/memory_cli_test.rs`、`tests/INDEX.md`、`Cargo.toml`（test注册），D的8路径，加协调路径。
**Docs and compatibility impact:** D同卡search小节与003/004；扩展CLI无operation通用门。**Rollback mode:** revert。**Full-suite trigger:** T-1。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=implementation; axis=search查询; AC=8/8; VER=8/8; landing=3; prod-files=4; scope=M; deps=DM-08,DEP-DM-04,DEP-DM-07; release=independent; split-from=DM-08`。

### Task DM-09：唯一计划收口

**Task type:** release。**Lifecycle / Acceptance:** pending / 空。
**Description:** 执行已经确定分支的聚合验收与授权后发布。**Out of scope:** 不新增行为，不以缩范围掩盖未完成卡。
**Current evidence:** DM-06报告、适用卡验收、backend交付、PR已关闭事实。
**Acceptance criteria（8）:**
1. 进入本卡前，另外14张公共卡满足各自前置验收（00/06仅本地验收+review，其余12张适用完整C/D）；self不列入前置。
2. GO时额外3卡达到完整验收；降级时它们按DEFER归档。
3. 整体代码review无未闭合P0/P1。
4. 同source snapshot下5表可重建。
5. 真实价值报告可复算。
6. GO记录run增量价值，降级记录实验性声明与重开的ADR。
7. 用户/开发/backend三处文档与已交付表面一致。
8. 最终发布证据达到原C/D要求。
**Verification（8）:** 14张公共前置卡逐项证据核验（00/06只查局部验收，不先要求继承的C/D）；分支卡/DEFER集合核验；review findings闭环核验；`cargo test --test memory_projection_test incremental_equals_same_snapshot_rebuild -- --exact`；DM-06完整报告复算；分支声明人工核验；D文档逐文件一致性核验；最终release/artifact/CDN/Homebrew证据核验。独立ER-13/14全量、fmt/clippy门仍强制，不用这8项替代。
**Dependencies:** DM-06本地验收、DM-15(GO)、DM-12、其它适用独立卡C/D（排除DM-09自身，00/06继承部分也不作前置）、DEP-DM-03、DEP-DM-04、DEP-DM-07、DEP-DM-08。DM-09完成后才回填00/06继承C/D，再核公共15/GO18全done/complete。
**Deliverables / Implementation write set:** `docs/development/tracing/memory.md`、`docs/development/plan/plan-long.md`、`docs/development/plan/plan-20260926-value-gate.md`、`docs/development/plan/plan-20260926-456-salvage.md`、本附件、D的8路径，加协调路径。
**Docs and compatibility impact:** tracing改历史+现行指针；MEM01/02按真证据更新；降级D添加实验性；PR关闭回执已有事实可引用，新增公开评论仍需授权。
**Rollback mode:** immutable-release。**Full-suite trigger:** T-2。**Estimated scope:** M。**Version increment:** patch。**C/D coverage from:** self。
**Granularity:** `type=release; axis=计划收口; AC=8/12; VER=8/12; landing=1; prod-files=0; scope=M; deps=DM-06,DM-15(GO),DM-12,DEP-DM-03,DEP-DM-04,DEP-DM-07,DEP-DM-08; release=independent`。
