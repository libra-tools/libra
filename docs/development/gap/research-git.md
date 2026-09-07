# research-git 与 Libra 能力差距分析

> 参考项目路径：`/Volumes/Data/competition/StepzeroLab/research-git`（**这是个 Libra 仓库**——有 `.libra/` 无 `.git/`，读历史用 `libra log -n <N>`），revision `62bcdf5`，版本 `0.0.7`（Alpha，Python 3.11+/3.12，依赖仅 `libcst>=1.1` + `mcp>=1.2,<2`）。已交付 v1（Memory Loop）、v2（Graph Intelligence + Ambient Capture）、v3（Research Layer）、多宿主安装/guidance 平面，以及本轮新增的 History Digest（`rgit digest` 历史回填队列）、运行时自更新（`rgit update`）与存储自检（`rgit doctor`）。
>
> 校验时间：2026-08-27（竞品 checkout 与 [`plan-long.md`](../plan/plan-long.md) 第九次竞品审计快照同为 `62bcdf5`，竞品侧自 2026-08-25 未再前进）。
>
> Libra 侧基线：`main` HEAD `89081277a`，`Cargo.toml` 版本 `0.21.27`。
>
> 目标：分析 research-git 的产品/技术能力与 Libra 现状的差距，给出 Libra-native 的补齐方向。本文只记录分析与计划，不改变当前命令面。本文所有 Phase / 优先级条目均归属 [`plan-long.md`](../plan/plan-long.md) 的 **LR-10**（Feature/Research Capsule 与实验谱系，状态「已验证」），**不新增编号**；实施排期以 `plan-long.md` 与各日期计划为准。

## 本次刷新（2026-08-27）

竞品自 `0.0.2` 跨 5 个 patch 到 `0.0.7`（漂移全部来自本文基线滞后，不是竞品在本轮前进）；Libra 侧自 `0.17.1777` 走到 `0.21.27`。

**竞品侧修正：** ① CLI 顶层子命令 17 → **20**，新增 `digest`（历史回填队列，6 个子命令）、`update`（自更新 + 宿主刷新）、`doctor`（只读 store 自检）（`src/rgit/cli.py:542-725`）；② 数据模型六表 → **八业务表 + `schema_metadata`**（新增 `digest_units` / `digest_meta`，`store/db.py:7-73`），`Capsule` 增 `origin`（`live|backfill`，`store/models.py:57`）、`Proposal` 增 `source_commit` 且 trigger 增 `backfill`（`store/models.py:107-115`——trigger 在 `:109`、`source_commit` 在 `:115`）；③ plugin 由 2 个 skill 增至 **3 个**（新增 `rgit-digest`），`plugin.json` 升至 `0.0.7`；④ 安装目标 4 → **5**（新增 `generic` 别名），`rgit install` 的 platform 已可省略并自动探测本机全部客户端（`installer.py:245,256-273`）；⑤ `capture` 改为位置参数 `REV|A..B` + 自动挑选，`review` 增 `--decide/--keep`，`edges` 增 `--scope/--limit`；⑥ MCP 仍是 **7 个只读工具**（`recall` 增 `exclude_backfill`，`mcp_server.py:20-28`；7 处 `mcp.tool()` 注册在 `:62-68`），写路径仍只走 CLI——两平面纪律未变。**最值得记下的设计分歧**：`digest` 回填路径**没有人工审批门**，信任模型是「标记 + 可过滤 + 可批量撤销」而非 gate（`_plugin/skills/rgit-digest/SKILL.md`）。

**Libra 侧修正（推翻或收紧了上一版的四处判断）：** ① `McpAuthorizer` **不再是** schema-only 占位——它已接入 `server.rs` 的 `resources/{list,read,templates}` 与部分 `tools/call`，但**生产从不安装 handler（`authz=None`），等价 allow-all no-op**，且 `tools/list` 与若干 tool impl 仍未接门（[`docs/development/tracing/code.md`](../tracing/code.md) C9；`plan-20260715` / `plan-20260824` 的 **DEFER-03** 显式延期）；② `refs/libra/traces` 的 checkpoint commit **已落地**（`agent_checkpoint.traces_commit`、catalog 重建 / 整链 prune、`agent checkpoint rewind/export`、`agent push --force-rewrite`），上一版「Phase 2 接线中」已过期；③ 「外部 hooks 捕获 7 类宿主」需限定——**7 类是 `agent_kind` 归一枚举**，可安装 hook 的 supported roster 只有 **3 类**（claude-code / codex / opencode），gemini 为卸载-only，cursor/copilot/factory_ai `supported=false`，因此上一版「Libra 在多宿主接入维度领先」改判为「归一化对象模型更强、可安装宿主数不领先」；④ 计数漂移：`SessionEvent` 9 → **10** 个变体（+`CodeWorkflow`），`LifecycleEventKind` 11 → **13** 类（+`SubagentStart`/`SubagentEnd`），MCP `#[tool]` 33 → **28** 个。

**维持不变的判断：** 符号抽取器仍是 **Rust 单语言**；Libra 仍**没有** capsule 一等对象、持久 recall 与实验谱系（LR-10 状态「已验证」）；`src/internal/ai/` 仍无 memory 模块、`src/cli.rs` 仍无 `memory` 子命令——但 **MEM-01/MEM-02 已由「已验证」推进为「已排期」**（M2 计划 [`plan-20260819.md`](../plan/plan-20260819.md)）。

**失效路径更正：** `docs/development/memory.md` → [`docs/development/tracing/memory.md`](../tracing/memory.md)；`docs/development/mcp.md` → [`docs/development/integration/integration-scenarios/mcp.md`](../integration/integration-scenarios/mcp.md)。

**待复核：** ① 上一版「33 个 `#[tool]`」的历史口径无法从当前 checkout 复原（不确定是否包含 resources/templates），本文按 `mcp/resource.rs` 现有 `#[tool(` 站点计数改为 28；② `src/internal/ai/mcp/authz.rs` 的模块级 rustdoc 自身仍写 "not yet wired"，与 `server.rs` 现状矛盾，属竞品文档之外的 Libra 侧文档债，本文不引用它作为证据；③ `plan-long.md:252` 竞品能力矩阵对 research-git 的「不学」表述（「实验 DSL 绑定单一 Agent」）已与竞品现状不符，待在 plan-long 侧同步（见 §不应照搬）。

**复核订正（2026-08-27，对抗式复核结论 PASS，仅 8 条 P2 精度项，无结论性改判）：** ① 竞品锚点区间三处收紧——`Proposal` 改引 `store/models.py:107-115`（原 `105-113` 未盖住 `:115` 的 `source_commit`）、MCP 注册点改引 `mcp_server.py:62-68`（原 `60-66` 漏 `ablation`/`provenance` 两只）、`detect_platforms()` 改引 `installer.py:256-273`（原 `255-272` 前后各偏一行）；② Libra 侧锚点两处消歧——`history.rs` 首次出现补全为 `src/internal/ai/history.rs` 并标注同名文件另有两个（`ai/codex/history.rs`、`ai/automation/history.rs`，`:3310`/`:3470`/`:3755`/`:4661` 只在前者成立）、`libra agent hooks` 的「从 stdin 摄取」改引模块 rustdoc `src/command/agent/hooks.rs:1-8` 与真正的摄取点 `src/internal/ai/hooks/runtime.rs:163,199-203`（原引的 `hooks.rs:25-28` 只是 `AgentHooksSubcommand` 枚举声明，不支撑该结论）；③ 措辞三处去歧义——SB-01 映射改为「主题是消除生产路径可触发 panic（`plan-long.md:438`），无界资源判据在 `:445`，本条是**新增**补充判据而非 SB-01 现有主题」、§捕获与冻结的 CAS 一句改为「建议在 §Phase 0 **增补**一条契约」（§Phase 0 现有 5 条中确无此条，原措辞像回指已有文字）、M2 的 SB-02 前置改为「前置只挂在 **MCP 表面**，M2 切片本身不被阻塞（`plan-20260819.md:22,342`）」。以上均为锚点/措辞精度修正，**未改动任何事实判断、编号归属或优先级**；LR-10 / MEM-01 / MEM-02 / MEM-04 / DEFER-02 / DEFER-03 / SB-01 / SB-02 / PD-02 / LB-01 / W5-08 一律未重编、未删除。**遗留项仍为上一段「待复核」的三条**（33→28 的历史口径、`authz.rs` rustdoc 文档债、`plan-long.md:252` 表述同步），复核未新增待复核项；本轮未改动 `plan-long.md`。

## 执行结论

`research-git` 不是一个替代 Git 的 VCS，而是一个叠加在普通 Git 仓库之上的“研究记忆层”。它把一次探索中的一个想法抽成 `Feature Capsule`，把 capsule、run、metric、edge、proposal 写入 `.rgit/graph.db` 和 `.rgit/objects/`，再通过只读 MCP 和本地 agent subagent 完成召回、组合与再生。其定位已从“Claude Code 插件”扩展为“**Works with Claude Code, Codex, Gemini CLI, and opencode.**”（README 现行副标题为 “**Reapply or remove previous experiments & features safely on today’s code.**”——注意 **safely remove** 已是与“复活”并列的一等用例，本文其余章节多处只讲复活，读时需补上这一半），并配套了把自身写成默认能力的多宿主 installer（claude-code / codex / gemini / opencode / generic）。

Libra 的优势在另一层：它已经是 AI-agent-native VCS，拥有 `.libra/libra.db`、对象存储、Git 兼容命令、AI workflow 对象、Session JSONL、外部 agent checkpoint（`agent_kind` 已归一 7 类宿主，其中 3 类可安装 hook）、redaction、usage 统计、MCP tools/resources、skills/sub-agent runtime、tree-sitter 符号抽取和 thread graph。但 Libra 目前缺少一个等价于 `Feature Capsule` 的一等对象：可以把“一个可复活的想法/实验变体”从会话、diff、checkpoint 和 metric 中独立出来，作为可查询、可审查、可重放到当前代码的语义单位。

建议不要把 `research-git` 的 Python `.rgit` 存储原样嵌入 Libra。更合理的方向是新增 Libra-native 的 `research memory` 平面：使用普通 Libra/Git blob + `refs/libra/research` + SQLite projection，复用现有 redaction、agent session、usage、MCP authorizer（需先装上真实生产策略，当前生产为 allow-all）、skills、semantic extractor 和 internal sub-agent dispatcher，在此之上实现 capsule capture / review / recall / compose / compare / ablation / provenance。

## research-git 能力拆解

### 定位与边界

- 源码入口：`src/rgit/cli.py`，发布入口为 `rgit = "rgit.cli:main"`，包名 `research-git`，版本 `0.0.7`（Development Status: Alpha）。
- 运行环境：Python 3.11+（classifier 已加 3.12），核心依赖只有 `libcst`（symbol map）和 `mcp>=1.2,<2`（read-only 共享平面；上限来自竞品提交 `db6e389 Pin MCP dependency below v2`）。
- 存储根：`rgit init` 在 Git root 下创建 `.rgit/`，不是替换 `.git/`。
- 客户端边界：从 Claude-Code-only 扩展为“任意 MCP 客户端”。智能动作（segment / regenerate / edge-judge）始终走宿主 agent 的 subagent，**不调用任何 paid API**；确定性 engine（graph、CAS object store、git diff、byte-exact freeze）不调用 LLM。
- MCP 边界：`mcp_server.py` 用 FastMCP 暴露 7 个**只读**工具（recall / compose / get_feature / list_features / compare / ablation / provenance；`recall_tool` 现带 `exclude_backfill` 参数，可在共享面过滤掉历史回填 capsule）；所有写路径（approve / dismiss / decide / edges / segment / metric-dir / digest accept·skip·clear）只走 CLI。

### 完整 CLI 命面（20 个子命令，已全部交付）

| 命令 | 作用 |
|---|---|
| `init` | 在 git root 建 `.rgit/`（不装 hook）；现会交互式提供历史消化计划：`--digest [MODE]`（默认 `layered`）/ `--range A..B` / `--all` / `--no-digest` |
| `run` | 跑实验 → byte-exact freeze artifact → 记录 run + edges → 暂存 proposal；`--from`/`--with`/`--refresh-guide-file`/`--init` |
| `capture` | 把 diff 切成 proposal 候选（Phase 1 免费 segmenter）；首选**位置参数 `REV` \| `A..B`**，省略时自动挑选（工作树有改动→工作树，否则→最后一次提交），`--commit`/`--range`/`--worktree` 降级为 `argparse.SUPPRESS` 的遗留拼写；`--trigger`（取值增加 `backfill`）、`--init` |
| `review` | 列 open proposals；`--approve` / `--dismiss` / `--decide`（新增，配 `--keep NAME[,NAME]` 一次性决定保留哪些候选、其余丢弃），三者互斥且**均可不带 id**——省略时自动指向唯一 open proposal；`--name`/`--index` |
| `features` | 列已 approve 的 capsule |
| `mcp` | 启动只读 MCP server |
| `edges` | `--apply`（确定性 overlaps）/ `--candidates`（depends_on 候选）/ `--add TYPE SRC DST`；新增 `--scope ID[,ID]`（限制在触及这些 capsule 的 pair，digest 批次后的增量路径）与 `--limit N`（depends 候选上限 = edge-judge 配额） |
| `pending` | 列 open proposal（带 diff/candidates，`--json`） |
| `digest` | **（新增）** 历史回填队列（engine plane）：`scan [A..B] --mode/--window/--all` / `status` / `next --batch` / `accept <pid>` / `skip <unit>` / `clear` |
| `resegment` | 用 agent 质量 capsule 替换启发式候选（`<pid> --from-json`） |
| `watch` | 前台 watch loop，空闲去抖暂存 Phase-1 proposal；`--interval/--idle/--once` |
| `install` | 把 plugin + MCP 装进 AI 客户端；`PLATFORM` **已可省略**（省略时自动探测本机全部客户端）、`--list/--uninstall`；`--dry-run`/`--guidance {default,manual-only,none}`/`--scope {user,project,local}`/`--json`/`--from-update` 已全部改为隐藏 plumbing |
| `update` | **（新增）** 升级包 + 以 conservative 模式刷新已安装宿主的 managed guidance block；`--on/--off` 开关更新提示 |
| `install-hooks` | 装 post-commit 自动 capture hook（不动外部 hook） |
| `compare` | 变体簇按 metric 排名（只读）；`TARGET --metric --higher/--lower` |
| `ablation` | 对 capsule 幂集建 base/+A/+A+B 网格（只读） |
| `provenance` | 某 run 的 per-slice clean vs adapted 审计（只读） |
| `metric-dir` | `set <metric> {higher,lower}` / `list` / `suggest` |
| `graph` | 渲染 graph：`--mermaid/--dot/--text`（默认 mermaid），`--runs` |
| `doctor` | **（新增）** 只读 store 自检（schema 版本、payload/artifact 完整性、proposal、edge 对称性/悬挂）；`--json`。实现在 `doctor.py`，用 `Store.open(readonly=True)`，明确 "no migrations, no repairs" |

### 数据模型

`src/rgit/store/db.py` 定义八类业务表 + `schema_metadata`（`SCHEMA_VERSION = "1"`），`src/rgit/store/models.py` 定义对应 dataclass：

| 表 | 作用 |
|---|---|
| `features` | Feature Capsule：name、intent、status、base_commit、knobs、data_assumptions、resurrection_guide、result_summary、payload_hash、**`origin`（`live` \| `backfill`，历史回填的可过滤标记）** |
| `runs` | 一次实验运行：cmd、artifact_hash、metrics、base_commit、env、created_at、returncode |
| `edges` | capsule/run 之间的 11 类关系（见下表） |
| `proposals` | capture 产生的候选队列：trigger（`run\|commit\|manual\|watch\|`**`backfill`**）、diff_ref、candidates、status、run_id、from_features、**`source_commit`（被捕获 diff 所属的提交，`None` 表示工作树）** |
| `events` | comment-in/out toggle 形成的 activate/deactivate 事件 |
| `metric_directions` | metric 越大越好或越小越好的 verdict 配置 |
| `digest_units` | **（新增）** 一段被聚类的主线历史消化单元：`id`（sha-set hash，幂等键）、`kind`（landed\|dead）、`shas`、`score`、`status`（pending\|staged\|done\|skipped）、`skip_reason`、`proposal_id`、`capsule_ids`、`meta` |
| `digest_meta` | **（新增）** 一次 digest 扫描的模式与边界：mode / head_at_scan / range |

`Capsule` 是核心资产——不是 patch，而是 intent + `code_slices` + knobs + data_assumptions + result_summary + resurrection_guide 的“想法规格书”。两个子结构值得注意：

- `CodeSlice`：`file`、`symbol`、`anchor`、`code`、`kind`（`add`/`wrap`/`insert`）——capsule 的代码切片单位。
- `ResultSummary`：`verdict`（`improved`/`neutral`/`regressed`）、`key_delta`、`failure_reason`、`notes`——把“这个想法赢没赢”结构化。

**11 类 edge**（确定性 baseline + agent-judged 语义边）：

| edge | 方向 | 语义 |
|---|---|---|
| `produced` | 有向 | capsule → 它产生的 run（frozen artifact） |
| `touches` | 有向 | capsule → 它改动的 module/file |
| `active` | 有向 | run → 运行时声明 active 的 capsule（ablation 的 active-set 依据） |
| `variant_of` | 有向 | capsule 是另一个的再生/变体（regeneration lineage） |
| `depends_on` | 有向 | X 用到 Y 定义的 symbol（确定性 over-produce 候选 → edge-judge 确认） |
| `supersedes` | 有向 | X 严格替代 Y（agent-judged） |
| `derived_from` | 有向 | lineage（schema 中保留，当前用得少） |
| `overlaps` | 对称 baseline | X/Y 触及同一 (file, top-level symbol)，中性 |
| `alternative_to` | 对称 agent-judged | 竞争实现，二选一 |
| `composable_with` | 对称 agent-judged | 可叠加共存 |
| `conflicts_with` | 对称 agent-judged | 真正不兼容 |

`store/models.py:8-24` 现在把 `CAPSULE_EDGE_TYPES`（8 类 capsule↔capsule）与 `SYMMETRIC_EDGE_TYPES`（`overlaps`/`alternative_to`/`composable_with`/`conflicts_with`）导出为**单一定义**，供 `doctor` 校验边的对称性与悬挂，防止 edge 词汇在写入侧与校验侧漂移。Libra 若实现 `ResearchEdge`，这条纪律值得照抄。

### 捕获与冻结

- `runner.py::run_experiment` 执行用户命令，随后 `gitutil.py::freeze_worktree` 把当前工作树打成确定性 tar，写入 content-addressed object store；即使命令失败也记录 artifact 和 `returncode`（v2 区分成功/失败 run）。
- `gitutil.py::diff_since`（`:288`）用 `git diff HEAD` 加 `git diff --no-index /dev/null <untracked>` 捕获 tracked 与 untracked 变化。
- `gitutil.py` 现已把 diff 来源抽象成一族 `DiffSource` Protocol 实现（`:418`）：`WorktreeDiffSource`（`:433`）/ `CommitDiffSource`（`:449`）/ `RangeDiffSource`（`:480`）/ `EmptyTreeRangeDiffSource`（`:605`，把「HEAD 中已不存在的死掉工作」当作从空树的 add-only patch），外加 `is_shallow`（`:543`）/ `mainline_commits`（`:553`）/ `head_files`（`:598`）供 digest 扫描使用。这是 `capture <REV|A..B>` 与 `digest` 共用的取 diff 基座。
- `segmenter.py`：`HeuristicSegmenter`（免费，confidence 0.3，按 file 分组）做 Phase 1，再把 proposal 交给 `capsule-segmenter` subagent 做 Phase 2 重分割。
- `astmap.py`：`libcst` 的 `changed_symbols()`（hunk→top-level def/class）、`read_symbol_source()`、`symbol_at_line()`。
- `watch.py` 用 mtime_ns snapshot 做 idle debounce（`tick()` 只在空闲且 diff 变化且未暂存时落 proposal）。
- `toggles.py` 识别 Python `#` 注释开关（hunk 级），把“启用/禁用一个 capsule”的信号写入 events。
- `hooks.py` 装/卸 post-commit hook，用 marker 自标记、绝不覆盖外部 hook，返回结构化 action report。
- 本轮竞品还修了 object store 的原子性（提交 `bea0427 fix(store): make object writes atomic and verifiable (#40)`，并让 `doctor` 报告 object 完整性失败）——建议在 Libra §Phase 0 的契约清单中**增补一条**「CAS 写入必须原子且可验证」（§Phase 0 现有 5 条为 ref / projection / 对象 / schema / 安全，尚无此条），并把它记为既有 **SB-01** 的补充完成判据，**不新增编号**。

### 图谱、召回与研究分析

- `edges.py` 确定性写 `overlaps`（同 (file, top-symbol)），并按 name 引用 over-produce `depends_on` 候选（带 evidence），交给 `edge-judge` agent 判断。
- `ranking.py` + `recall.py`：wildcard-safe 的字段加权 lexical（intent/name ×3、knobs/result_summary ×2、code/guide ×1）+ structural boost + neighbor boost；不依赖 embedding；每个 hit 携带 depends_on / same-region 子图。
- `compose.py` 为 agent 再生生成 brief：capsule 字段、当前 symbol live source、冲突列表和 merge context。
- `compare.py` 按 `variant_of` 传递闭包构建变体簇，跟 `produced` run 的 metric 排名，给出 Δ 与 ★ winner。
- `ablation.py` 按 run 的 `active` edge（缺失时回退 `produced`）对 capsule 幂集建 base/+A/+A+B 网格。
- `provenance.py` 从 frozen artifact in-memory 解 tar，与 capsule clean slice 做 clean/adapted/missing 审计。
- `metricdir.py` 用启发式（loss/err/nll/ppl/perplex→lower；acc/f1/reward/score/bleu/rouge→higher，仅自信匹配）给出 metric 方向，`best_index` 据存储方向选最优。
- `graphview.py` 提供 Mermaid / DOT / text 的 capsule/run graph，并在有更精确 same-region 关系时抑制冗余 overlaps。
- `tables.py` 终端定宽表格 + ★ winner 标记 + clean/adapted diff 渲染（compare/ablation/provenance 的人读输出）。
- `metrics.py::parse_metrics`（`:9-28`，本轮新增模块）：`rgit_metrics.json` 优先，否则从 stdout 抓 `key=value` / `key: value`（可选 `RGIT_METRIC` 前缀），只保留能解析为 float 的值——容错设计，坏的 metric 行绝不中断已经跑完的实验。
- `digestscan.py`（本轮新增）：确定性历史聚类——first-parent 行走、revert 配对成 dead unit、同作者重叠文件 streak 合并、纯 infra 预丢弃、评分；`MODES = ("layered","trunk","dead","archaeology")`、`DEFAULT_WINDOW = 400`、`UNIT_MAX_DIFF_BYTES = 300_000`（staging 期 oversized 标记）。与 `digestqueue.py` 一起构成 `rgit digest` 的 engine 面。

### Plugin / subagent 平面

`src/rgit/_plugin/`（plugin.json v0.0.7）定义三个 skill + 三个 subagent：

| 组件 | 职责 |
|---|---|
| `rgit-capture` skill | 读取 pending proposals → 派 `capsule-segmenter` → 写回 resegment → `rgit edges --apply` → 派 `edge-judge` |
| `rgit-recall` skill | 调 MCP recall/compose → 派 `capsule-regenerator` 把 capsule 重应用到当前代码 → review → 用户 `rgit run --from` 冻结 |
| `rgit-digest` skill（**新增**） | 排空历史消化队列：`rgit digest status/next --batch` 取批 → 并发派 `capsule-segmenter`（附 `history_context`：commit subject/date/author、revert 信息、`oversized`）→ `rgit resegment` 写回 → `rgit digest accept` 以 `origin=backfill` **非交互**入库 |
| `capsule-segmenter` | 把 messy diff 切成高质量 capsule（输出含 `dropped` 的噪声项） |
| `capsule-regenerator` | 按 capsule intent/guide 在当前代码上重实现；**只 author**，不跑程序/不冻结/不提交 |
| `edge-judge` | 把 `depends_on` 候选与 `overlaps` baseline 细分成 alternative_to/composable_with/supersedes/conflicts_with |

这个拆分值得借鉴：共享 memory plane 保持 dumb/read-only，真正的智能动作在本地 agent plane 运行；agent 只负责 authoring，byte-exact replay 不依赖 agent。

**但 `rgit-digest` 引入了一处 Libra 必须记下的设计分歧**：回填路径**刻意没有人工审批门**——`digest accept` 直接以 `origin=backfill` 入库，SKILL.md 明确要求「不要用 `rgit review`、不要逐 capsule 问用户」，信任模型是「mark-and-filter，不是 gate」，事后用 `rgit digest clear` 批量撤销。Libra 若做同类历史回填，需自行判断这条是否与自身的 review-gated 纪律相容（本文 §风险 2 的立场是「不自动把每个 diff 变成 confirmed memory」，与之相反）。

（注：README「Two planes」小节仍写 “two skills”（README:150），与源码不符——以源码为准。）

### 多宿主安装与 agent guidance 平面（v3 新增，本次重点补记）

`install [platform]` 不只装 plugin/MCP，还通过 `agent_guidance.py` 把一段“何时该用 research-git”的 **managed guidance block** 写进各宿主的全局指令文件，使工具在 install + restart 后成为默认能力。**platform 现已可省略**：bare `rgit install` 调 `installer.py::detect_platforms()`（`:256-273`）自动探测本机全部客户端（`which claude` / `~/.codex` / `~/.gemini` / `which opencode`｜`~/.config/opencode`）并全部安装：

| 平台 | skills 落点 | guidance 文件 | reload |
|---|---|---|---|
| `claude-code` | `claude` CLI plugin | `$CLAUDE_CONFIG_DIR`（默认 `~/.claude`）`/CLAUDE.md` | 重启 / `/reload-plugins` |
| `codex` | `~/.agents/skills/` | `$CODEX_HOME`（默认 `~/.codex`）`/AGENTS.md` | 新开 Codex session |
| `gemini` | `~/.agents/skills/` | `~/.gemini/GEMINI.md` | 新开 Gemini CLI session |
| `opencode` | `~/.agents/skills/` | `$XDG_CONFIG_HOME/opencode/AGENTS.md`（默认 `~/.config/opencode`，XDG-aware） | 新开 opencode session |
| `generic`（**新增**） | `~/.agents/skills/` | 无（`guidance_target("generic")` 返回 `None`） | —— |

- `generic` 是「任何读 `~/.agents/skills` 的 agent CLI」的友好别名，**故意永不被自动探测**（`installer.py:243-245`、`agent_platforms.py:22-48`）——它是安装目标，不是安装信号。
- `agent_platforms.py::guidance_target(platform)` 给出 `{path, reload}`；`agent_guidance.py` 以 `<!-- research-git:start/end -->` 标记做幂等 upsert/remove、原子写、dry-run。START 标记现带指纹（`h=<12 位 hash>`），用于区分官方 block 与用户改过的 block。
- guidance 模式 pinned 跨升级：`default`（改完代码考虑 capture）/ `manual-only`（仅显式请求）/ `custom`（继承 default + repo `.rgit/` 偏好）。（CLI `--guidance` 的第三个取值是 `none`——不写 block，与 block 内的 `custom` 是两回事。）
- block 状态被分为 `absent | broken | pristine | customized`（`agent_guidance.py:200-239`）；`rgit update` 以 **conservative** 模式刷新已安装宿主（`installer.py:283-286`），`customized` 一律 `skipped_customized` 不覆盖。
- 覆盖优先级：session/user 指令 > repo 偏好 > 全局 default。
- `installer.py` 支持 `--scope {user,project,local}`、`--uninstall`、`--dry-run`，对 agent-CLI family 用 `~/.agents/skills/` 双向符号链接，使 skill 能找到 bundled agent。

> 这是 research-git 相对 `0.0.2` 最实质的新增之一：它把“让宿主 agent 主动用这个工具”做成了一等的、跨 5 个安装目标（4 个可自动探测 + `generic` 别名）的可安装/可卸载/可 dry-run/可自更新的能力。

### 运行时自更新与存储自检（本轮新增）

- `selfupdate.py::detect_installer()`（`:20`）识别 uv-tool / pipx / pip 安装形态（含 PEP 668 与 Windows 文件锁的降级分支），`rgit update` 据此升级自身，再以 conservative 模式刷新已安装宿主。
- `updatecheck.py`：`~/.rgit/update-check.json` 缓存，TTL 24 h，后台线程查询 PyPI（`https://pypi.org/pypi/research-git/json`），失败静默；`rgit update --on/--off` 持久化开关。
- `doctor.py`：只读 store 自检，`Store.open(readonly=True)`，"no migrations, no repairs"；检查 schema 版本、feature payload、run artifact、proposal 候选合法性、edge 对称性与悬挂端点，输出 `{ok, schema, summary, findings}`。**这条映射 Libra 既有 SB-01**——SB-01 的主题是「消除生产路径可触发 panic」（`plan-long.md:438`），其已述完成判据含对象/内容尺寸上限等**无界资源**项（`:445`），但**不含** store 完整性与原子写；因此「本地 CAS 对象写入须原子且可验证 + 只读自检可报完整性失败」是给 SB-01 的**新增补充完成判据**，按 `plan-long.md:94` 的口径挂在既有编号下，**不新增编号**。

## Libra 当前相邻能力

### 已有强项

| Libra 能力 | 现状 |
|---|---|
| VCS 真源 | `.libra/libra.db`、对象存储、SQLite refs/index/reflog、Git 兼容命令与 `refs/libra/*` AI 分支，不依赖外部 `.git`。 |
| Session 事件流 | `src/internal/ai/session/jsonl.rs` 的 `SessionEvent`（**10 个变体**：SessionSnapshot / ContextFrame / CompactionEvent / MemoryAnchor / AgentRun / ToolCall / ToolResult / Goal / AiArtifact / **CodeWorkflow**），append-only JSONL + unknown-event-safe 读取。 |
| Prompt 内记忆 | `context_budget/memory_anchor.rs` 的 `MemoryAnchor`（kind、scope、confidence、review_state{Draft/Confirmed/Revoked/Superseded}、expires_at、superseded_by、source_event_id 等 13 字段），可 replay 到 prompt section。 |
| 外部 agent 捕获（多宿主） | 迁移 `2026050303_agent_capture.sql` 的 `agent_session` 已用 `agent_kind` CHECK **归一 7 类宿主**（claude_code / cursor / codex / gemini / opencode / copilot / factory_ai，`adapter.rs::AgentKind` 镜像），含 `state`、`redaction_report`；`agent_checkpoint`（scope: temporary/committed/subagent）+ `refs/libra/traces`。**注意区分归一与可安装**：可安装 hook 的 supported roster 只有 **3 类**（claude-code / codex / opencode），`gemini` 已降级为 uninstall-only，`cursor`/`copilot`/`factory_ai` 为 `supported=false`（`docs/commands/agent.md:45-55`、`COMPATIBILITY.md:189`）。 |
| Checkpoint commit（已落地） | `agent_checkpoint.traces_commit`（`2026050303_agent_capture.sql:52`）+ `src/internal/ai/history.rs`（注意仓库内另有 `ai/codex/history.rs` 与 `ai/automation/history.rs`，以下行号只在前者成立）的 catalog→`refs/libra/traces` 链重建（`:3310`）、整链重写式 prune（`:3470`/`:3755`）、traces commit 反构 catalog 行（`:4661`）；`agent checkpoint rewind/export`、`agent push --force-rewrite`（force-with-lease）。上一版记的「Phase 2 接线中」已过期。 |
| Lifecycle 归一 | `hooks/lifecycle.rs` 的 `LifecycleEventKind`（**13 类**：SessionStart / TurnStart / ToolUse / ModelUpdate / Compaction / CompactionCompleted / PermissionRequest / SourceEnabled / SourceDisabled / TurnEnd / SessionEnd / **SubagentStart** / **SubagentEnd**）+ envelope validation；`hooks/runtime.rs` 从 stdin 摄取。 |
| 符号抽取 | **`tools/semantic/extractor.rs` 已有 tree-sitter（Rust grammar）符号抽取**：`SemanticSymbol`（kind: Function/Method/Struct/Enum/Trait/Module/Const/Static/TypeAlias；scope: File/Module/Crate/Workspace/External；range/selection_range/byte_range/confidence/approximate/container）。当前单语言（Rust），但已是可复用的结构化基座。 |
| Patch 应用 | `tools/apply_patch/`（Codex 风格 `*** Begin Patch`，fuzzy seek_sequence）能拿到精确修改区间。 |
| MCP 平面 | `mcp/server.rs` + `mcp/resource.rs` 暴露 **28 个 `#[tool]`**（13 个 `create_*` + 13 个 `list_*`，覆盖 intent/task/run/context_snapshot/plan/patchset/evidence/tool_invocation/provenance/decision/context_frame/plan_step_event/run_usage，另加 `update_intent` 与 AI-VCS 工具 `run_libra_vcs`；按 `mcp/resource.rs` 现有 `#[tool(` 站点计数）与 `libra://` 资源（2 个静态：`history/latest`、`context/active`；2 个模板：`object/{object_id}`、`objects/{object_type}`）。`mcp/authz.rs` 的 `McpAuthorizer` **已在 `server.rs` 接线**（`:29` 导入、`:47` 字段、`:91` `set_authz`、`:144-155` 分派 Allow/Deny/NeedsHuman，`resources/{list,read,templates}` 于 `:186/194/479` 接门），**但生产两个构造函数都把 `authz` 初始化为 `None`（`:62`/`:77`），`set_authz` 仅测试调用，无 handler 时 `:144` 无条件放行——等价 allow-all no-op**；`tools/list` 与若干 tool impl（`create_patchset_impl`/`list_patchsets_impl`/`create_evidence_impl`/`create_tool_invocation_impl` 等）仍未接门。权威表述见 [`docs/development/tracing/code.md`](../tracing/code.md) C9；`plan-20260715` / `plan-20260824` 的 **DEFER-03** 为显式延期决策。 |
| Skills / sub-agent | `skills/*`（parser/loader/dispatcher，project/user/embedded 三层）；`agent/runtime/sub_agent.rs` 的 `TaskInvocation`/`TaskResult`/`SubAgentDispatcher`，`TaskFailure` 含 13 类（含 PermissionEscalationDenied / SafetyDenied / BudgetExceeded / ApprovalRejected / Timeout），统一权限/预算/安全门。 |
| Usage / Graph | `command/usage.rs`（report/prune，`--by {Model, Agent, AgentProviderModel}`，`--session`/`--thread` 为过滤，Human/Json/Csv）；`command/graph.rs` 展示 AI thread projection graph（交互式 TUI 入口已于 W5-08 / v0.20.0 删除，非 `--json`/`--machine` 直接报 usage 错误并提示走 `libra code`）。 |
| 宿主集成 installer | **`libra agent enable/disable`（别名 `add`/`remove`）** 才是安装/卸载宿主 hook 的命令面（`docs/commands/agent.md:65,67`、`src/command/agent/mod.rs:222`）；`libra agent hooks <host> <subcommand>` 是宿主回调的**运行时入口点**（`src/command/agent/hooks.rs:1-8` 模块 rustdoc：由 `libra agent enable` 写出的 per-agent hook config 调用；stdin 摄取实现在 `src/internal/ai/hooks/runtime.rs:163,199-203`），不是安装器。与 research-git 的 `install [platform]` 同维度的入口已存在，但可安装宿主只有 3 个。 |
| 历史回填（与 `rgit digest` 同维度） | `libra agent import` 已能在**显式同意**下发现并导入历史 Claude/Codex transcript，或一次可信沙箱化的 OpenCode 导出，带 typed redaction、coverage/import 围栏、本地擦除 tombstone、原子 no-clobber loose-object 发布与幂等重放（`COMPATIBILITY.md:189`、`docs/commands/agent.md`）。**关键差别：Libra 回填的是「会话」，research-git 回填的是「提交历史 → capsule」。** |
| 按冻结内容复算 | `libra agent review --checkpoint <id>` / `agent investigate start --checkpoint <id>`（PD-02）：评审/调查的工作区就是 checkpoint 内容只读物化（`checkpoint-input/`，不做 worktree 快照），缺失或不可物化时在任何 run 创建前 fail closed（`COMPATIBILITY.md:193,194`）——与 research-git `provenance`（frozen artifact vs clean slice）同类。 |
| Harness bridge | `libra agent bridge --stdio`（plan-20260818 LB-01）：repository-scoped JSON-RPC 2.0 NDJSON 入口，20 个 v1 方法自 v0.21.1 全部实现，含封闭 `mode` 的 `diff.get`（worktree/staged/checkpoint）、`commit.create`（仅当前 index）、要求显式 `expected_head` 围栏的 `checkpoint.restore`、`review.run`（`COMPATIBILITY.md:189`）。 |
| Skill 检索与 registry | `libra agent skill search/list`（按 skill/provider/session/时间窗 keyset 分页检索已捕获的 skill 事件，读时投影无专表）与 `agent skill registry`（每 agent 的策展可发现 skill 注册表）；`libra agent graph <session>` 是只读捕获投影（session → turn → revision → subagent），frozen JSON v1 + 严格元数据白名单，与 orchestrator 线程的 `libra graph` 区分。 |
| 规划中的持久记忆 | [`docs/development/tracing/memory.md`](../tracing/memory.md)（draft，Last updated 2026-08-10）已设计 branch-aware、namespace/path-keyed、review-gated 的 Memory 子系统；当前源码**仍没有 `src/internal/ai/memory` 实现**，`src/cli.rs` 也无 `memory` 子命令。但 **MEM-01/MEM-02 已由「已验证」推进为「已排期」**：M2 计划 [`plan-20260819.md`](../plan/plan-20260819.md) 已冻结 `MemoryNote`/`MemoryEvent` 自定义 JSON 对象、受保护本地分支 `refs/heads/libra/memory/repo`、单一写入器 `MemoryWriter`、SQLite **FTS5 + `bm25()`** 首个召回通道与最小命令面 `libra memory search / show / status / rebuild`；该计划明确**不实现 MCP Memory tools**；**该 MCP 表面**仍受 `SB-02` 的 default-deny authorizer 与 principal threading 前置约束，而 M2 切片本身因不注册 Memory MCP 而**不被 SB-02 阻塞**（`plan-20260819.md:22,342`）。 |
| 唯一现存“research”痕迹 | `prompt/embedded/contexts/research.md` 只是一个 prompt context 模板，**不是** research-memory 基础设施。 |

### 当前缺口概览

| 维度 | research-git | Libra 当前 | 差距 |
|---|---|---|---|
| 语义单位 | `Feature Capsule`（intent + code_slices + knobs + assumptions + guide + result_summary） | commit、PatchSet、Run、ContextFrame、MemoryAnchor、checkpoint | 缺“一条可复活的功能/实验想法”一等对象。 |
| 捕获入口 | `run` / `capture <REV\|A..B>` / `watch` / post-commit hook / `digest`（历史回填） | AI session、external hook（**归一 7 类，可安装 3 类**）、checkpoint、apply_patch diff、普通 commit、`agent import`（会话级历史回填） | 缺把一次 diff/session 切成 capsule proposal 的 pipeline；历史回填两边都有，但 Libra 回填「会话」、竞品回填「提交历史 → capsule」。 |
| 人工审查 | `proposals` + `review --approve/--dismiss` + `curation.py` | MemoryAnchor 有 review_state；agent checkpoint 无 capsule review queue | 缺面向想法/capsule 的 review UX 与状态机。 |
| 代码切片 | Python-only `libcst` top-level symbol（窄但闭环） | **已有 tree-sitter Rust 抽取器**，但仅 Rust、未做成通用 slice service | 差距缩小为：把现有 extractor 扩到多语言 + 包成可复用 slice/anchor 服务 + 永远保留 raw diff fallback。 |
| 召回 | lexical + edge-aware recall（字段加权 + neighbor boost） | [`tracing/memory.md`](../tracing/memory.md) 规划了 recall，M2 计划已把首个召回通道冻结为 FTS5+`bm25()`（MEM-02「已排期」）；源码仍无持久 recall；MCP 无 research recall | 缺 capsule 召回工具与排序策略；实现时应与 MEM-02 的召回通道收敛，而不是另起一套。 |
| 再生 | `compose` brief + capsule-regenerator（只 author） | 内部 AgentRuntime/subagent 可执行任务，但无 capsule regeneration protocol | 缺把 recalled capsule 变成当前代码 diff 的 workflow。 |
| 实验分析 | compare / ablation / provenance / metric-dir | `usage` 聚合、graph JSON/Web Code UI；无按 feature variant 的 metric lineage | 缺 variant/run/metric/capsule 关系与研究表格。 |
| 共享 | MCP query-only（7 tools，`recall` 可 `exclude_backfill`）+ local subagent | MCP 有 28 objects/resources tools；`McpAuthorizer` 已接线但**生产 `authz=None` = allow-all**，`tools/list` 与若干 tool impl 未接门 | 缺 research memory 的只读 MCP 工具；且需先**安装真实授权策略并补齐未接门站点**（DEFER-03）。 |
| 宿主可发现性 | `install [platform]` 写 managed guidance block 到 5 个安装目标（4 可自动探测 + `generic`），bare install 自动全装 | `libra agent enable/disable` 可装宿主 capture hook，但**可安装 roster 只有 3 个**；research 能力本身无“默认可用”分发策略 | 思路可借鉴，但 Libra 应通过自身 MCP/skills/hooks 暴露，而非编辑各宿主 CLAUDE.md/AGENTS.md。 |
| 存储 | `.rgit/graph.db` + objects | `.libra/libra.db` + objects + refs | 需要 Libra-native projection/ref，而不是导入 `.rgit`。 |

## 关键差距详解

### 1. Libra 缺少 `ResearchCapsule` 这类一等对象

Libra 当前 AI 对象更偏向工作流执行：Intent、Plan、Task、Run、PatchSet、Evidence、Decision、ContextFrame、MemoryAnchor、agent checkpoint。它们能回答“这次 agent 做了什么、用了什么上下文、产生了什么证据”，但不能稳定回答：

- 这个曾经试过的“想法”是什么？
- 它触及哪些 symbol/file？
- 它的 knobs、assumptions、result_summary 是什么？
- 它和别的想法是依赖、替代、可组合、supersedes 还是冲突？
- 如何把它重新实现到今天的代码？

`MemoryAnchor` 可以保存短事实或约束，但粒度太小，不适合作为带 code_slices 和 metric lineage 的实验 capsule。`agent_checkpoint` 可以冻结 transcript/artifact，但粒度太大，不会把一个混杂 diff 切成多个正交 idea。

### 2. Libra 捕获的是 session/checkpoint，不是 idea

`research-git` 的核心价值不是 frozen artifact 本身，而是 `segment_diff -> proposal -> approve -> capsule`。Libra 已有更强的底层捕获点：

- `apply_patch` 路径能拿到精确修改区间。
- `SessionEvent` 能保存 tool call/result、goal、artifact。
- `agent_kind` 已把 7 类宿主（Claude/Codex/Gemini/opencode/Cursor/Copilot/Factory）session 归一为同一对象模型；但**能安装 capture hook 的只有 claude-code / codex / opencode 三类**（gemini 卸载-only，其余 `supported=false`），所以“可捕获宿主”不等于枚举里的 7。
- `refs/libra/traces` 能持久化 redacted checkpoint（checkpoint commit 已落地）。
- `libra agent import` 已能回填历史会话——与 `rgit digest` 同为“把过去补进记忆”的入口，但 Libra 回填的粒度是**会话**，竞品回填的粒度是**提交历史聚类出的 capsule**。

但这些信号没有被收敛成“候选功能胶囊”。因此 Libra 现在更容易恢复会话或审计执行，不容易召回一个被混在会话中的实验想法。

### 3. symbol：research-git 窄但闭环，Libra 已有基座但未成服务

`research-git` 的 symbol mapping 只覆盖 Python top-level function/class（`astmap.py`），`toggles.py` 的注释开关也只识别 Python `#`——窄，但 capture→recall→regenerate 闭环完整。

Libra 这边的事实需要更新：它**已经有** tree-sitter 符号抽取器（`tools/semantic/extractor.rs`，Rust grammar，输出带 kind/scope/range/confidence 的 `SemanticSymbol`）。所以“缺 symbol slice service”是高估——但也不要读成“已备第二语言”：`Cargo.toml:125-127` 虽挂了 `tree-sitter` / `tree-sitter-bash` / `tree-sitter-rust`，**bash grammar 的唯一使用者是 `src/internal/ai/sandbox/command_safety.rs:11`（shell 命令安全解析），与符号抽取无关**；`SemanticLanguage` 仍只有 `Rust` 一个变体，`language_for_path` 只认 `.rs`，`query/` 下只有 `rust.scm`。真实差距是：

1. 把现有 extractor 从 Rust 扩到 TS/Python/Markdown/SQL（多 grammar 或 LSP 路线）。
2. 把它包成一个 capsule 用得上的 **slice/anchor 服务**（给定 diff hunk → (file, symbol, anchor, code, kind)）。
3. 未知语言降级到 file+hunk+anchor；docs/config 用 section/key anchor。
4. 永远保留原始 diff/artifact hash——symbol 只是定位加速，错了也要可审计。

### 4. Libra 的 MCP 平面还不是 research memory query plane，且授权门生产为 allow-all

Libra MCP 已暴露 28 个 AI workflow object 工具与 `libra://` 资源，但有两件事要先处理：

- `mcp/authz.rs` 的 `McpAuthorizer` **已接入 server 请求路径的一部分**（`resources/{list,read,templates}` 与部分 `tools/call`），但**生产从不安装 handler**——两个构造函数都令 `authz = None`，`set_authz` 仅测试调用，无 handler 时授权检查无条件返回 `Ok(())`，因此**当前生产 MCP 授权是 allow-all no-op**；`tools/list` 与若干 tool impl 甚至还没接门。research memory 的只读 MCP 要安全暴露给团队，前提是**安装真实授权策略并补齐未接门站点**（按 namespace/actor/sensitivity 过滤）。这是 **DEFER-03** 的显式延期范围（`plan-20260715.md:2796`、`plan-20260824.md:891`），权威描述见 [`tracing/code.md`](../tracing/code.md) C9。
- research-git 的 MCP 设计有一条要沿用的边界：共享 plane 只读、只返回 graph snippets；智能再生在本地 agent plane 执行。

因此 Libra 应：

- `research recall/get/compose/compare/ablation/provenance` 暴露为 MCP 只读工具。
- 写入 capsule、approve proposal、改 metric direction、执行 regeneration 必须走 CLI/AgentRuntime，并经 permission、sandbox、audit。
- 不要把 MCP 变成 agent turn control 面；这与 [`docs/development/integration/integration-scenarios/mcp.md`](../integration/integration-scenarios/mcp.md)（原 `docs/development/mcp.md`，已随文档重组移动）已有边界一致。该文的 transport 拆分本身仍未落地：`libra code --stdio` 现已被标注为 **deprecated MCP-only legacy transport**（`src/command/code.rs:699`），独立 `libra mcp --stdio` 是 **DEFER-02**（`plan-20260824.md:25,890`）。

### 5. Libra 有 usage，但缺 feature/run metric lineage

`libra usage` 能按 `Model / Agent / AgentProviderModel` 聚合 token/cost（session/thread 作为过滤维度）。`research-git` 的研究层不是成本统计，而是“哪个变体赢了”“ablation 表怎么读”“再生后的代码是否忠实于原 capsule”。

Libra 需要新增的是实验语义关系：

- `ResearchCapsule -produced-> ResearchRun`
- `ResearchCapsule -variant_of-> ResearchCapsule`
- `ResearchRun -active-> ResearchCapsule`
- `ResearchRun.metrics` 与 metric direction（参考 `metricdir.py` 的启发式 + 显式覆盖）
- clean slice vs adapted slice provenance

这些可以消费现有 usage/agent run evidence，但不能被 usage 表直接替代。

### 6. Libra 应避免 research-git 的 Git 依赖与 Python-only 限制

`research-git` 通过 shell 调 `git rev-parse`、`git diff`、`git ls-files`、`git diff --no-index`。Libra 不能这样设计自己的核心闭环。实现时应走 Libra 内部对象、index、diff、worktree、ignore 和 storage API，保证：

- 在 `.libra` 仓库没有 `.git` 时仍工作。
- 与 Libra 的 SHA-1/SHA-256、SQLite refs、`.libraignore`/`.gitignore` 语义一致。
- 能用 `libra push` / cloud sync / publish 传播 research memory，而不是生成旁路 `.rgit` 状态。

## 建议的 Libra-native 方案

### Phase 0：定义研究记忆平面和契约

新增设计文档或扩展 [`docs/development/tracing/memory.md`](../tracing/memory.md)，先固定对象和边界：

- ref：`refs/libra/research`，用于 capsule/run/edge/proposal 的 Git blob/tree/commit 真源。**待与 MEM-01 owner 收敛**：M2 计划 [`plan-20260819.md`](../plan/plan-20260819.md) 已冻结 `refs/heads/libra/memory/repo`（受保护本地分支、单一写入器 `MemoryWriter`、普通 push/fetch/mirror 不传播）；两者 ref 命名空间形态不同（`refs/libra/*` vs `refs/heads/libra/*`），写入器纪律也需要对齐（Memory 侧是 single-writer，本文尚未规定）。本文不擅自改动 MEM-01 已冻结的命名。
- projection：SQLite 表只做可重建索引，例如 `research_capsule`、`research_run`、`research_edge`、`research_proposal`、`research_metric_direction`。
- 对象：使用普通 JSON blob，不新增 git-internal typed object variant，保持与 Memory/agent traces 的存储纪律一致。
- schema：所有对象带 `schema_version`、`object_id`、`created_at`、`created_by`、`source_refs`、`trust`、`sensitivity`、`redaction_report`。
- 安全：code_slices 可能含 secret，必须在持久化和 MCP 返回前经过 redaction/sensitivity gate。

建议对象草案：

| 对象 | 必填字段 |
|---|---|
| `ResearchCapsule` | id、name、intent、status、base_commit、source_diff_oid、code_slices（含 file/symbol/anchor/kind）、knobs、data_assumptions、result_summary、resurrection_guide |
| `ResearchRun` | id、cmd、artifact_tree_oid 或 checkpoint/ref、metrics、return_code、env_summary、base_commit、active_capsules |
| `ResearchEdge` | src、dst、type、confidence、evidence、created_by |
| `ResearchProposal` | id、trigger、source_event/session/run、diff_ref、candidates、status |

### Phase 1：实现确定性捕获与 review queue

先实现不依赖 LLM 的 walking skeleton：

- 新增 `libra research init/status/capture/review/list` 或在 `libra code` 下提供内部入口；命令名需单独评审。
- 捕获输入来自 Libra 内部 diff/worktree（复用 `apply_patch` 与 worktree/diff API），而不是 `git diff`。
- 支持 `--from-session`、`--from-checkpoint`、`--from-run`、`--staged`、`--worktree` 等来源。
- 默认 segmenter 按 file/hunk 生成低置信 candidate，可调用 `tools/semantic/extractor.rs` 给 Rust 切片提精度，未知语言落 file+hunk；记录 raw diff 和 source event。
- `review --approve/--dismiss/--rename/--edit-intent` 将 proposal 提升为 capsule。
- 所有写入走 CAS ref update，projection 可从 `refs/libra/research` 重建。

验收重点：

- 在没有 `.git` 的 Libra 仓库中可运行。
- dirty worktree 下不改用户 index。
- capsule approve 后可通过 `--json` 列出，projection rebuild 后一致。
- redaction 失败 fail-closed。

### Phase 2：接入 agent 分割、edge judge 与 recall/compose

在确定性路径稳定后接入智能层：

- 内部 skill：`libra-research-capture`，读取 pending proposals，调用 sub-agent 生成高质量 capsules。
- 内部 skill：`libra-research-recall`，召回 capsule、读取 dependencies、生成 compose brief。
- edge baseline：确定性写 `overlaps` / `same_region`（复用 extractor 的 (file, symbol) 键）。
- edge judge：通过内部 `SubAgentDispatcher` 或现有 AgentRuntime 派生 reviewer，确认 `depends_on`、`alternative_to`、`composable_with`、`supersedes`、`conflicts_with`。
- recall：先实现 research-git 风格 lexical（字段加权）+ structure + neighbor boost；embedding 留作可选索引，不做真源。
- compose：返回 capsule intent/knobs/assumptions/guide、clean slices、current source、merge context。

注意：`research-git` 依赖宿主 CLI 的 subagent subscription；Libra 应优先复用自身 provider/runtime/usage/sandbox，不新增旁路 plugin 执行面。

### Phase 3：实现 regeneration 与 reproducibility close loop

目标是“再生由 agent author，复现由 Libra freeze/checkpoint 保证”：

- `research recall` 只产生候选 capsule 和 compose brief。
- `research apply` 或 skill 驱动 internal AgentRuntime 修改工作树，但不得自动提交。
- 用户或 workflow 跑测试/命令后，用 `libra research run --from <capsule>` 或已有 `libra code` run 记录 artifact/metrics。
- approval 后写 `variant_of` / `produced` / `active` edge。
- 如果 regeneration 改善了 guide，支持 `capsule update-guide`，保留旧 revision。

这能保留 research-git 的关键安全边界：agent 只负责 authoring，byte-exact artifact/replay 不依赖 agent。

### Phase 4：补研究分析层

对齐 research-git v3 能力，但做成 Libra-native 输出：

- `libra research compare <capsule|symbol>`：按 variant cluster + metric direction 排名（参考 `compare.py` 的传递闭包 + Δ + ★）。
- `libra research ablation <capsule...>`：按 active feature set 生成幂集网格。
- `libra research provenance <run>`：clean slice vs frozen/adapted slice。
- `libra research metric-dir set/list/suggest`：配置 metric 方向（含 `metricdir.py` 式启发式建议）。
- `libra research graph`：输出 JSON/Mermaid，后续接入 Web graph，而不是新增长期 TUI。

这些命令应复用 `usage` 的 CSV/JSON 输出习惯，但不要混淆成本 usage 与实验 metric。

### Phase 5：MCP 与团队共享

- 先为 `mcp/authz.rs` 的 `McpAuthorizer` **安装真实生产授权策略**并补齐未接门站点（`tools/list` 与若干 tool impl）——接线本身已部分完成，缺的是 handler 与覆盖面，这是开放 research MCP 的硬前提。本条**不自立新前置**，而是引用既有约束：**DEFER-03**（`plan-20260715.md:2796`、`plan-20260824.md:891`）、`plan-long.md` 的「不得在 `SB-02` 完成前开放非 loopback Memory MCP」、以及 M2 计划的非目标「不实现 MCP Memory tools；该表面仍受 `SB-02` 的 default-deny authorizer 与 principal threading 前置约束」（`MEM-04` 同理）。
- MCP 暴露只读 `research_recall`、`research_get_capsule`、`research_compose`、`research_compare`、`research_ablation`、`research_provenance`。
- 所有 mutating 操作继续走 CLI/AgentRuntime，并经 `McpAuthorizer` / approval / sandbox。
- `refs/libra/research` 可由 `libra push` 或 cloud sync/publish 传播；projection 在 clone/restore 后重建。
- 对 secret/private capsules 做 namespace/actor/sensitivity gate，默认不向共享 MCP 返回。

## 不应照搬的部分

| research-git 做法 | Libra 不应照搬的原因 | Libra 替代 |
|---|---|---|
| `.rgit/graph.db` 作为事实源 | Libra 已有 `.libra/libra.db`、对象库和 refs；旁路存储会破坏 push/cloud/restore 一致性 | `refs/libra/research` + 可重建 SQLite projection |
| shell out 到 `git` | Libra refs/index/worktree 语义不同，且部分仓库无 `.git` | 使用 Libra 内部 diff/index/worktree/object API |
| Python-only `libcst` symbol map | Libra 面向多语言仓库 | 扩展已有 `semantic/extractor.rs`（tree-sitter）到多语言，未知语言 file+hunk fallback |
| MCP write tools | Libra MCP 已有控制面边界和授权计划 | 只读 MCP；写入走 CLI/AgentRuntime（先为 `McpAuthorizer` 安装真实策略并补齐未接门站点，DEFER-03） |
| `digest accept` 无人工审批门（`origin=backfill` 直入库） | 与本文 §风险 2「不自动把每个 diff 变成 confirmed memory」相反；Libra 的 MemoryAnchor 已有 `review_state`，M2 也以自动编译 + 证据引用而非「无门直入」为纪律 | 历史回填也进 proposal/review 状态机；若要低摩擦，用批量 review + `origin`/`trust` 标记 + 可撤销，而不是取消门 |
| subagent plugin 作为唯一智能入口 | Libra 已有 provider/runtime/skills/sub-agent/usage/sandbox（含 13 类 TaskFailure 门） | 内部 AgentRuntime 派生语义 agent，统一计费/审计/权限 |
| `install [platform]` 写 managed block 到各宿主 CLAUDE.md/AGENTS.md | Libra 不应通过编辑外部 agent 的全局指令文件来“宣传自己”；与 Libra 自有 skills/MCP/hooks 发现机制重叠且更难审计 | 通过 Libra 自身 MCP resources/skills/`libra agent enable` 暴露 research 能力；guidance 留在 Libra 侧 |
| 只本地 `.rgit` share | 团队无法沿 Libra remote/cloud/publish 传播 | research ref 进入 Libra push/cloud/publish 模型 |

> 治理注：[`plan-long.md`](../plan/plan-long.md) §竞品能力矩阵（`:252`）对 research-git 的「不学」列写的是「实验 DSL 绑定单一 Agent」——该表述已与竞品现状不符（官方定位已是 “Works with Claude Code, Codex, Gemini CLI, and opencode.”，installer 支持 5 个目标且 bare install 自动全装）。按上表最后两行的现成理由，建议 plan-long 侧把「不学」改判为「把自身 guidance 写进各宿主全局指令文件（CLAUDE.md/AGENTS.md）以求默认可用」；这是表述更正，**不新增编号、不改变 LR-10 优先级**。本轮未修改 plan-long，此项列为待同步。

## 风险与设计约束

1. **隐私与 secret 泄露。** Capsule code_slices、resurrection_guide、metrics/env 可能携带密钥或私有实验数据。写入、MCP 返回、cloud sync 前必须有 redaction/sensitivity gate（且 `McpAuthorizer` 要先装上真实生产策略——当前生产 `authz=None` 等价 allow-all，见 DEFER-03）。
2. **错误 capsule 会污染未来召回。** 必须保持 proposal/review 状态、confidence、source evidence、supersedes/revoke，而不是自动把每个 diff 变成 confirmed memory。
3. **symbol extractor 不可靠。** 即便 Libra 已有 tree-sitter 抽取器，symbol 也只是定位加速，不能成为唯一真源。所有 capsule 必须保留 raw diff/artifact/source refs。
4. **图关系过度智能化。** `depends_on` / `conflicts_with` 错边会比漏边更坏。默认写 neutral `overlaps`，高语义边需要 evidence 和可撤销。
5. **与 `Memory` 子系统边界（已具体化到 ref 命名与写入器纪律）。** `Memory` 适合长期事实与规则；`ResearchCapsule` 适合可再生实验想法。两者可以互相引用，但不要合并成一个表。**具体冲突点**：M2 计划已冻结 `refs/heads/libra/memory/repo` + 单一写入器 `MemoryWriter` + local-only 不传播；本文 Phase 0 提议的 `refs/libra/research` 命名空间形态不同，且默认要进 push/cloud/publish 模型。ref 命名、写入器数量、传播默认值三项**待与 MEM-01 owner 收敛**，不得各自实现。
6. **与 external-agent checkpoint 边界。** `agent_checkpoint` 是外部会话审计与恢复；`ResearchCapsule` 是想法抽象。Checkpoint 可作为 evidence/source，不应被当成 capsule。
7. **性能。** 召回必须有 limit、分页、字节/token 上限；graph traversal 不得在大型 monorepo 中无界扩张。

## 推荐优先级

下表全部条目归属 [`plan-long.md`](../plan/plan-long.md) 的 **LR-10**（状态「已验证」，完成判据：capsule 可捕获、召回、在今日代码上安全 reapply/remove，并带 provenance），**不新增编号**；P0..P4 是本文内部的实施次序建议，不是 plan-long 的优先级字段。

| 优先级 | 任务 | 原因 |
|---|---|---|
| P0 | 固定 `ResearchCapsule` / `ResearchRun` / `ResearchEdge` JSON schema 与 `refs/libra/research` 存储契约 | 防止实现先行导致事实源漂移 |
| P1 | 做确定性 capture/review/list walking skeleton | 不依赖 LLM，能尽快验证数据模型 |
| P1 | 从 Libra diff/apply_patch/session/checkpoint 建立 source evidence 引用 | 这是 Libra 相对 research-git 的结构性优势 |
| P1 | 把 `semantic/extractor.rs` 包成多语言 slice/anchor 服务 | capsule code_slices 的精度基座，已有 Rust 实现可复用 |
| P2 | 接入内部 skill/sub-agent 做 semantic segmenter 和 edge judge | 对齐 research-git 的核心智能闭环 |
| P2 | 先为 `McpAuthorizer` 装上真实生产策略并补齐未接门站点（DEFER-03），再加 read-only recall/compose MCP tools | 让外部 agent 可消费 memory，但不放开写面、不泄露 secret |
| P3 | compare/ablation/provenance/graph | 形成研究层产品差异，但依赖 capsule/run/edge 先稳定 |
| P4 | cloud/publish/team sharing policy | 需要等 redaction、sensitivity、namespace gate 稳定后再开放 |

## 一句话产品差距

Libra 已经能很好地保存“AI 和 VCS 发生过什么”（且在**归一化的会话对象模型**——7 类 `agent_kind` + 统一 redaction/checkpoint/权限/预算/安全门——与 tree-sitter 符号基座上结构性领先；但在**可安装宿主数量**上并不领先：Libra 3 个可安装 roster vs research-git 5 个安装目标且 bare install 自动全装）；`research-git` 强在保存“这个探索里真正值得以后复活——或安全移除——的想法是什么”，并已把它做成跨 5 个安装目标默认可用、且可回填整段历史的闭环。Libra 要补的不是另一个 Git wrapper，而是把“可复活的想法”变成与 commit、run、checkpoint 一样可版本化、可审查、可召回、可传播的一等对象。
