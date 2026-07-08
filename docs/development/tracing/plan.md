# Tracing 实施计划：先 `libra agent`，后 `libra code`

## 0. 执行原则

本计划把 [`agent.md`](agent.md) 与 [`code.md`](code.md) 当作实现目标文档；执行顺序固定为：

1. 先完成 `docs/development/tracing/agent.md` 的外部 Agent 捕获计划（AG-16~AG-24a），再由 AG-24 做 docs/tests/compat/release closeout。
2. `agent` 计划通过 AG-24 closeout（且 AG-24a 合规实现已完成）后，再进入 `docs/development/tracing/code.md` 的 `libra code` 实现核对、补强与收敛；若 AG-24a 任一实现子项明确 deferred，AG-24 只能标记 preview/blocked release，不得把 Agent 阶段声明为完整闭环。

`libra agent` 和 `libra code` 的边界不得混同：

- `libra agent` 负责 observed external agent 的注册、hook、lifecycle、session/checkpoint、transcript、review/investigate evidence。
- `libra code` 负责内部 AgentRuntime、TUI/Web/headless/MCP、approval/sandbox/tool gate、workspace mutation。
- review/investigate 的 mutating fix/action 只能桥接回 `libra code` 内部 AgentRuntime；没有源码锚点和测试证据前，必须稳定返回 unsupported。

内部 AgentRuntime / Web-only 迁移的当前事实源是 `docs/development/internal/code-agent-runtime.md`。本计划中的 Code 阶段只消费该文档的内部 runtime/fix-bridge 证据；不得重新链接或恢复旧 `docs/development/code-agent-runtime.md`、`docs/development/agent.md`、`docs/development/web-only.md`。

同目录范围外文档（out-of-scope 声明）：

- `docs/development/tracing/` 下的 `memory.md`、`sandbox.md`、`web-api.md` 为独立 draft 提案，**不属于本计划范围**。任务卡的"关联设计文档"字段（只列 `agent.md` / `code.md`）是唯一执行依据；执行者不得从这三份文档引入验收标准或实现项。
- 已知冲突处一律以本计划 + `agent.md` / `code.md` 为事实源：
  - `memory.md` 断言 LifecycleEventKind 共 11 个变体、"无需新增任何 hook 事件"，与 A4 新增 `SubagentStart`/`SubagentEnd`（13 变体）冲突；A4 落地后该枚举描述需由文档 owner 更新。
  - `memory.md` 以 `libra mcp --stdio`（其链接的 docs/development/mcp.md 当前不存在）为前提，与 C6 固定的 `libra code --stdio` 现实冲突；C6 落地后需对齐。
  - `web-api.md` 的 `/api/v1` 变更型契约与 C4 的 `/api/code/*` observe-only 契约冲突；本计划执行期间 web 契约唯一事实源是 C4（与现行 `src/internal/ai/web/{mod,code_ui}.rs` 一致：路由注册在 `web/mod.rs` 的 code_router，状态/读模型在 `code_ui.rs`），`/api/v1` 提案留待独立仲裁。
  - `sandbox.md` 的 VM/AppleContainer 后端是净新增功能，不并入 C7（违反 §6 "不发明额外功能"口径），作为独立后续工作另行排期。
- A9/C8 closeout 时在三份文档头部补 "out-of-scope of tracing/plan.md" banner 并注明上述冲突条目；修订其设计断言由各文档 owner 负责，不在本计划内。

测试凭证使用规则：

- 仓库根目录存在本地 `.env.test`，用于 live/provider 测试的真实 Key。计划和 PR 只允许引用文件名与所需 env key 名，不得复制、打印、提交或写入 `.env.test` 的内容。
- 默认 L1/L2、compat、fake-provider 测试仍必须在不读取真实 Key 的情况下通过；`.env.test` 只用于明确标记为 live、provider-backed、model-generation 或需要真实 API 的验证。
- CLI 级 live 验证优先通过 `libra code --env-file .env.test ...` 加载凭证，覆盖陈旧 shell env。
- Cargo live 测试若直接读取进程环境，运行前使用 `set +x; set -a; source .env.test; set +a` 导出 Key，再执行对应 `cargo test`。执行日志不得回显 env 值。

Agent 第一期本地采集验收规则：

- 第一期完成口径必须覆盖本地三种真实 agent：`codex`、`claude`（Claude Code，对应 Libra slug `claude-code`）和 `opencode`。fake fixtures 只用于 CI/确定性回归，不能替代这三条本地采集验收。
- 本地验收必须先记录 `command -v codex`、`command -v claude`、`command -v opencode` 和各自版本/路径；`opencode` 要特别记录实际命中的 PATH，避免 Homebrew 与 `~/.opencode/bin/opencode` 影子路径导致误判。
- 三个 agent 都必须在临时 Libra 仓库中完成 hook 安装、一次最小会话运行、lifecycle ingest、session/checkpoint catalog 写入、`refs/libra/traces` checkpoint 写入、metadata-first show/list 和 redaction 验证。
- 缺少任一二进制、登录态、hook provider 或 transcript 读取能力时，Agent 第一期不得标记完成；只能标记为 blocked，并写明缺失项与恢复步骤。
- 本地三 agent 验收不得打印 prompt、token、provider credential、raw transcript 或 `.env.test` 内容；默认输出只能使用 redacted summary / metadata。

每个实现 PR 都必须说明：变更边界、未触碰项、稳定错误码、migration/backfill 状态、用户可见行为、回滚方式、测试命令。

每任务的版本发布、提交与部署流程见 §0.4（强制）；Claude 执行者的逐任务操作规程见 §0.5；执行进度以 §10 进度表 + `libra log` 为唯一事实源。**完成判定以代码为准**：不以文档修改或任务卡打勾确定功能是否完成，必须分析代码与测试证据；更新代码时必须同步更新文档。

### 0.1 多维评审结论与强制改进项

本节是对本计划的执行前复核结果。后续实现不得只按任务标题推进，必须把下表中对应门禁落实到任务卡、PR 描述、测试和文档同步中。

| 维度 | 当前结论 | 改进后的执行门禁 |
|---|---|---|
| 合理性 | 总顺序合理：先外部 observed agent 捕获，再内部 `libra code` runtime；但不能把 `review/investigate fix` 提前混入 Agent 阶段。 | A7/A8 只交付 read-only；任何 mutation/fix 必须等 C7 找到内部 AgentRuntime、approval、sandbox、tool gate 源码锚点和测试证据。 |
| 可行性 | 可分阶段落地，但本地三 agent smoke 依赖本机 `codex`、`claude`、`opencode` 的真实安装、登录态和稳定 transcript/hook 能力。 | A6.5 是第一期硬门禁；任一 agent 缺少 binary、登录态、HookProvider 或 transcript 读取能力时，只能标记 blocked，并记录恢复步骤，不得降级为 fake fixture 通过。 |
| 完整性 | AG-16~AG-24a 与 C1~C8 已覆盖主要功能面；主要缺口是把横切非功能需求显式落到每张相关卡。 | 每个触达 public JSON、DB row、object layout、RPC、hook envelope、run state 的 PR 都必须声明 schema/protocol version、compat 窗口、migration/backfill、docs/tests 同步状态。 |
| 安全性 | 已识别 secrets、stderr、raw transcript、external binary 仿冒和 `.env.test` 泄露风险。 | `.env.test` 只能由测试进程读取，不得打印；外部二进制必须 `env_clear` + allowlist；stderr/stdout/raw hook input 先 cap + redaction，再进入任何用户输出、JSON、DB 或对象存储。外部 agent 的一切输出（findings/stances/transcript）均视为不可信输入，进入后续 prompt、用户终端或持久化前必须 provenance 标注 + redaction + 控制序列剥离；`env_clear` + allowlist 原则同样适用于 §0.3 直启的真实 agent CLI（登录态所需基础变量除外）。 |
| 功能正确性与接口兼容性 | alias、capability matrix、RPC v1/v2、Code mode/provider 参数互斥是最容易漂移的接口面。 | 所有 alias 必须同语义、同退出码、同 JSON shape；RPC v2 `info` 不破坏 v1 `capabilities` 一个 release window；`libra code` 参数互斥先由 C1 源码复核后再改。 |
| 数据流与控制流正确性 | 正确边界是 `hook/RPC -> LifecycleEvent -> validation -> redaction -> checkpoint writer`；`review/investigate -> evidence -> optional internal fix bridge`。 | Provider hook 和 external binary parser 不得直接写 checkpoint；read-only workflow 不得直接修改工作树；mutating path 只能进入内部 AgentRuntime serialized queue。 |
| 性能与效率 | transcript、review output、checkpoint catalog 和 session list 都可能放大到高成本路径。 | 默认 list/show 只读 metadata；transcript/detail 必须显式开启并 streaming/chunked；分页默认 `--limit 50`、cap 500；review sink 和 RPC stderr 设定 bounded buffer。 |
| 可靠性与容错性 | checkpoint 写序、prune 并发、review/investigate cancel/timeout 是主要可靠性风险。 | A5 必测 ref/DB/object_index crash matrix、UPSERT/重试、doctor repair、prune A/B；A7/A8 必测 terminal state 和 process/reader/lock/lease cleanup。 |
| 兼容性与互操作性 | `libra agent` 是 Libra extension，不是 Git 命令；外部 agent 配置文件和 MCP/code-control 边界必须可互操作但不混同。 | `status/enable/disable` 保持 canonical；`list/add/remove` 只是 alias；MCP stdio 不得成为 turn control plane；首批外 agent 配置只写 Libra-managed hook entry 并保留用户配置。 |
| 可扩展性与可维护性 | first-batch roster 限制合理，但后续扩展必须避免散点状态字段和测试缺口。 | 新增 agent 必须更新 `agent.md` first-batch/roster 规则、E9、registry matrix、docs/commands、compat schema pin、tests/INDEX；新增 test target 必须同时注册 `Cargo.toml`。 |
| 合规性与标准符合性 | retention、GC、raw export、audit、redaction report 已在 `agent.md` 中定义，但计划需要把它们作为发布门禁。 | A9/C8 closeout 必须同步 retention/GC/raw export/audit 文档；raw export 需要显式授权和 append-only audit；release notes 必须区分 enabled、preview/opt-in、deferred。合规实现面（audit 表、`--allow-raw`、retention/GC、erasure）由 A8.5 落实，不得降级为纯文档验收。 |

#### 0.1.1 第二轮复核补充（2026-07-03）

第二轮按同一维度清单对照当前源码、测试注册状态与本机外部 CLI 实测复核了本计划；经对抗验证确认的问题已直接改进到对应章节/任务卡，此处仅记录索引，防止后续修订回退：

1. 检查点顺序矛盾（A-F3 要求 A6.5，但其前置 A6 被排在 A-F4 才验收）→ 已修正 §4。
2. §2 依赖图缺 AG-20→AG-21、AG-17→A6.5 两条边 → 已修正 §2 与 A6/A6.5 依赖字段。
3. review/investigate 新顶层命令缺三守卫/COMPATIBILITY/README/zh-CN 接线，单独合并必挂 compat-offline-core → 已补 A7/A8 验收、触达文件与验证命令，§0.2 增加"可见命令 compat 接线不得延后"规则。
4. `LBR-AGENT-009/010` 无归属任务卡，且 agent.md E10 种子编号与已发布 `LBR-AGENT-001`（AgentBudgetExceeded）硬冲突 → 已补 A3 编号核对门禁与 A7/A8 触达文件。
5. `claude -p --output-format stream-json` 必须搭配 `--verbose`（2.1.x 实测，缺失即退出码 1），原 smoke 命令必失败 → 已修正 §0.3.2/§0.3.4。
6. Codex hooks 受用户级 trust/enable 双重门控、OpenCode 上游无 hooks.json（只有 plugin 机制），§0.3.3 原断言不成立 → 已改为"上游实测为准"口径并补 A4 验收。
7. Preflight 缺登录态只读检查，blocked 硬判据不可机械执行、会在真实付费会话中途才暴露 → 已补 §0.3.2/§0.3.6/A6.5。
8. §0.3.4 把 raw stdout 直落 evidence，与 §0.3.1/§0.3.5 redacted-only 规则自相矛盾；目录/文件权限未定义 → 已按 raw/ 与 summary 分区重写并规定 0700/0600。
9. 对真实 agent 子进程整文件 `source .env.test` 会注入全部 provider/D1/R2/GitHub key → 已改为按需具名 key 注入。
10. 已存在的 `libra agent push` 与 prune 重写后 refs/libra/traces 非快进发散在计划中零覆盖 → 已补 A5 验收与触达文件。
11. Libra 自身存量 v1 checkpoint 布局（无 manifest/redaction_report/content_hash）的兼容读取与 doctor 误报防护缺失 → 已补 A5 验收与改造前 writer fixture 要求。
12. agent.md 强制的合规实现面（`agent_audit_log`、`--allow-raw`、`agent.retention.*`、GC/erasure）被降级为 A9 纯文档验收 → 新增 Task A8.5。
13. reviewer/investigator 缺最小权限 spawn 与强制 workspace 隔离（in-place 可直读仓库根 `.env.test`）；findings/stances 反哺后续 prompt 的注入链无 untrusted 门禁 → 已补 A7/A8 验收。
14. gemini 从 supported 降级后的卸载通道、存量数据可读性、隐藏 hook 运行时入口去留无任务承接 → 已补 A2/A4/A5 验收。
15. remove/disable 卸载侧配置清理、用户条目保留与二次卸载幂等零验收，smoke 无卸载环节 → 已补 A2/A6.5/§0.3.5。
16. migration 规范门禁（YYYYMMDDNN/幂等/`_down.sql`/round-trip）、fixture 版本溯源、A6.5 二进制 pin（sha256）缺失 → 已补 A5/A6/§0.3.2/§0.3.6。
17. 同目录 memory.md/sandbox.md/web-api.md 与 A4/C4/C6/C7 存在互斥断言且零引用 → 已在 §0 增加 out-of-scope 声明，并调整 C6 验证的 rg 扫描范围。
18. §5 Code 进入条件第 4 条与 C1 自指循环 → 已改写 §5，并允许 C1 在 A-F4 后并行只读启动（差距清单须按最终 HEAD 复核）。

#### 0.1.2 第三轮复核补充（2026-07-03）

本轮重新 review 了 `plan.md`、`agent.md` 与 `code.md`，确认以下漂移必须在三份文档中统一：

1. `agent.md` 的 Codex/OpenCode hook 契约仍把 `.codex/hooks.json`、`.opencode/hooks.json`、`[features] hooks = true` 写成目标形态；这与 §0.3.3 已确认的“Codex hooks 受用户级 `[hooks.state]` trust/enable 门控、OpenCode 上游无 hooks.json、实现必须以上游实测 plugin/config 形态为准”冲突。A4 和 A6.5 必须以 §0.3.3 的实测口径为事实源，`agent.md` 同步改为“候选形态/禁止未证实完成态”。
2. `agent.md` E10 仍把 `LBR-AGENT-001`~`010` 当作未来错误码种子，但 `LBR-AGENT-001` 已发布为 `AgentBudgetExceeded`。三份文档统一改用语义错误键（如 `ERR_AGENT_FIX_BRIDGE_UNAVAILABLE`）作为计划内引用；真实 `LBR-*` 编号只能由 A3 在 `docs/error-codes.md` 中分配未占用编号后写回。
3. runtime source-of-truth 已迁到 `docs/development/internal/code-agent-runtime.md`；旧 `docs/development/code-agent-runtime.md` 链接在当前工作区已失效。A9/C8 的跨文档同步、Agent fix-bridge 前置和 drift guard 均改指新路径。
4. `code.md` 只有命令现状，未承接 C1~C8 的 mode/provider/Web/MCP/session/sandbox/fix-bridge 契约，无法作为 Code 阶段目标文档。C8 前必须保持 `code.md` 与 §6 一致，并把 C1 source-grounded audit 的分类结果写回 `code.md`。
5. `agent.md` 只有 AG-24 closeout，缺少计划中 A8.5 的合规实现面分工，容易再次把 `agent_audit_log`、`--allow-raw`、retention GC、erasure 降级为文档同步。`agent.md` 必须显式拆出 `AG-24a / A8.5` 实现卡，AG-24 closeout 只做文档/compat/release 收敛。
6. `agent.md` 与 `code.md` 里的 `_general.md` 相对链接从 tracing 目录解析错误；统一改为 `../commands/_general.md`，避免执行者点进不存在路径。
7. `src/command/code.rs` 的 help/banner 示例暗示 `libra code --web-only --provider ollama`、`libra code --web-only --provider codex --browser-control loopback` 等组合可用，但 `validate_mode_args` 当前在 web-only/stdio 中通过 `reject_non_tui_flags` 拒绝一切非 Gemini provider（无 codex 豁免；`BrowserControlMode` 注释与 banner 示例亦受影响，Codex web-only 的 loopback/app-server 分支当前为 CLI 不可达代码）。C1 必须把该项分类为 code behavior、docs/help drift 或 deliberate difference；C2/C4 不得在未分类前按示例推进实现——两卡中 mode/provider 相关验收均已改写为以 C1 分类结果为前置的条件式验收。

#### 0.1.3 第四轮复核补充（2026-07-03）

本轮按 11 维度 + 跨文档一致性 + 源码锚点真实性重新 review 了三份文档（多视角评审 + 逐条对抗验证）；确认的问题已直接改进到三份文档对应章节，此处仅记录索引：

1. 文档搬迁断链（blocker）：`docs/development/commands/{agent,code}.md` 已迁至本目录，但 `tests/compat/agent_docs_contract.rs` 的 `include_str!`、`tests/INDEX.md`、`COMPATIBILITY.md`、`docs/development/commands/README.md` 仍指旧路径——当前树上 `cargo test --all` 直接编译失败 → 新增 Task 0.3 承接接线；`agent.md` 验收命令中两条 rg 的旧路径已同步改指 tracing/。
2. C2 与 C4 验收互斥且预设 C1 未做的分类结论（blocker）：`reject_non_tui_flags` 实际拒绝一切非 Gemini provider（含 codex，Codex web-only 分支为 CLI 不可达代码）→ C2/C4 相关验收改为以 C1 分类清单为前置的条件式验收；`code.md` 示例范围从 ollama 扩为所有非 Gemini provider。
3. `STABLE_AGENT_SLUGS` 收敛归属矛盾（plan A1 排除 vs agent.md AG-16 改动清单包含）→ 统一为 A1/AG-16 只冻结 `observed_agents/{capability,registry}.rs` 事实源，CLI roster 常量收敛移入 AG-17（A2）。
4. 第二/三轮新增验收未回写 agent.md：E4-libra-v1 存量布局契约、push 非快进语义、gemini uninstall-only 通道与测试、A7/A8 最小权限 spawn/ANSI 剥离/provenance 注入门禁、A6.5 硬门禁 target → 已补入 agent.md 对应节（E4 区、承诺矩阵、AG-17/AG-20/AG-22/AG-23、任务卡表、测试矩阵、验收命令）。
5. agent.md 把 hook entry 写成裸 `libra` 且允许 "fallback 为 `libra`"，与 §0.3.3 pinned 绝对路径强制断言及源码（`resolve_hook_binary_path` 无裸名 fallback）冲突 → agent.md 安装契约改为 canonicalize 绝对路径必须形态；A4 补安装断言。
6. `list --json` wire key 在 agent.md 内部冻结成两套（`hooks_installed` vs `installed`）→ 统一为 `installed`（以「第一批支持项目」capability matrix 为唯一事实源）。
7. agent.md 强制补强项 #9（可观测性 span/metric）与 #10（hook 崩溃行为回归）在 plan 零承接 → A3/A4/A5/A7/A8 验收各补对应 span/metric 断言；A4 补 hook handler 崩溃回归。
8. Code 阶段验证命令缺 `--features test-provider -- --test-threads=1`：`code_ui_remote_*` matrices、`code_mcp_dual_entry_test`、`code_resume_test` 整文件被 feature 门控，裸跑编译为 0 个测试空跑假通过 → C4/C5/C6/C7/C8 与 §9 已补 feature 与空跑警示。（历史表述；实际门控形态于 §0.1.5 第 5 条修正为"逐项门控 + 1 个跳过占位测试"，空跑假通过结论不变。）
9. 保留期不闭合：agent.md 承诺 findings 90 天 "settings 可覆盖" 但无 settings 键、无任务承接 → agent.md 补 `agent.retention.findings_days`，A8.5 补 findings/run-state GC 验收；audit 1 年到期处理交叉引用 Audit log 规格的合规审批流程。
10. `--allow-raw` 覆盖面矛盾：A8.5 把 redacted 的 detail/transcript 也锁进授权门，与 agent.md 读取 pipeline 及 A5/A6.5 metadata-first 断言自相矛盾 → 统一为 "detail/transcript：显式 flag + cap/streaming/redaction；raw（未脱敏）：`--allow-raw` + audit"；agent.md 三处混写句同步拆开。
11. 分页契约挂在不存在的 `review/investigate run list` 子命令上，且 run_id 无枚举入口 → A7/A8 与 agent.md 补充规格 §5 统一新增 `review list` / `investigate list`（沿用 `--limit 50`/cap 500/keyset cursor）。
12. §0.3.3 `prepare_agent_repo` 的命令替换会捕获 `libra init` 多行 banner 导致 repo 路径错位；preflight `cargo build` 未跳过 web 构建 → init 改 `-q`、§0.3.6 补静默 stdout 硬性要求、build 前缀 `LIBRA_SKIP_WEB_BUILD=1`。
13. review 取消入口未定义（investigate 有 cancel，review 只有 show/clean）→ A7 与 agent.md §5 统一新增 `review cancel <run_id>`，前台 run 的 SIGINT/SIGTERM 等价 cancel。
14. A3 缺 agent.md 威胁 T9 的 TOCTOU spawn revalidation 承接 → A3 补 fd-exec 优先/受信目录降级验收（可按 AG-18 DoD 显式延期）。
15. agent.md E5 chunk 路径示例用 CLI slug（`claude-code.jsonl.001`）与 E4-libra `<agent_kind>`（`as_db_str()` snake_case）字段级冲突 → 示例改 `claude_code.jsonl.001` 并写明取值规则。
16. agent.md 补充规格 §4 给三个运行期语义键预设退出码 129，与仓库 category 派生约定（cli→129、其余→128）冲突 → 改 128 并注明退出码由 category 派生。
17. A9/C6 的 rg 验证命令不可机械判定（未转义正则、无否定断言/判定口径）→ A9 改用转义否定式，C6 补人工复核口径。
18. agent.md 术语表指引用户运行已删除的 `libra db upgrade` → 改为 DB open 时自动迁移口径。
19. A5 push 方案 (b) 需要稳定错误码但 E10 无对应语义键 → 命名 `ERR_AGENT_TRACES_PUSH_DIVERGED`，agent.md E10 表补条件行。

#### 0.1.4 落地性复核补充（2026-07-04）

本轮按“能否交给下一个执行 Agent 直接落地”的标准复核 `plan.md` 与它声明的事实源。结论：本文已经具备任务卡、依赖、验收命令、发布闭环和 blocked 处理规则，可以作为执行入口；但它**不是功能完成证明**，当前 §10 仍只有基线行，AG/C 任务均未落地。执行者必须从 0.x 前置任务开始，不得把计划完成视为实现完成。

复核发现的剩余落地阻断与处理：

1. `docs/development/internal/code-agent-runtime.md` 仍包含旧 `docs/development/commands/agent.md`、旧 `docs/development/code-agent-runtime.md`、失效 `commands/_general.md` / `mcp.md` 相对链接和旧 drift 命令。由于本文把该文件声明为 Code 阶段事实源，若不先清理，会在 A1/C1 前继续污染执行者判断。新增 Task 0.4 承接该 source-of-truth drift 清理。
2. 当前可执行起点不是 A1，而是 `0.1 -> 0.3 -> 0.4 -> 0.2 -> A1`。0.3 修测试/索引断链；0.4 修事实源文档断链；0.2 才能把跨命令规则作为可运行守卫。
3. “落地完成”的唯一判据是 §10 进度表 + `libra log` + 对应源码/测试证据。任务卡 checkbox 只能表示覆盖项，不表示功能已完成；任何 checklist 勾选都必须附带 commit、验证命令和 blocked/deferred 说明。
4. 所有历史路径文字必须分为两类：可点击/可执行事实源必须指向当前存在路径；历史说明允许保留旧路径，但必须明确标注“旧/已删除/不得恢复/历史提交”，不能作为任务依赖或验证命令输入。

#### 0.1.5 第五轮落地核查补充（2026-07-04）

本轮以「验证命令能否机械跑绿、引用锚点今天是否仍真」为标准，对照当前树（Cargo.toml `0.17.1832`、HEAD `6aa0c08`）做了七维并行核查（测试 target 命名、Task 0.3/0.4 事实基础、Agent/Code 源码锚点、agent.md/code.md 回写、验证命令实跑、本机环境）。确认的问题已直接改进到对应章节，此处仅记录索引：

1. 绝大多数断言成立：46 个 `cargo test` target 命名零错（35 个现存 + 11 个计划新增各有归属卡）；Agent 侧 19 项、Code 侧 11 项源码锚点全部命中（含 `reject_non_tui_flags` 拒一切非 Gemini provider、`STABLE_AGENT_SLUGS=["claude-code","gemini"]`、`materialize_isolated_workspace` 私有、`PushArgs` 仅 `--remote`、`LifecycleEventKind` 现 11 变体等）；§0.1.2/§0.1.3 声称的 agent.md/code.md 回写 18 项全部在文；两处守卫断链（`agent_docs_contract.rs:8`、`matrix_alignment.rs:104`/`:171`）行号仍精确成立。
2. 三条验证命令按原文**永不可能跑绿**（blocker，均已实跑确认）→ 已改写为可判定形态：Task 0.3 否定 rg 扫描（原 `docs` 全域范围含 plan.md 自身任务文本 8 行、Task 0.4 所辖 internal 文档 24 行、§0 范围外 memory.md 4 行，且全路径正则测不到 `commands/README.md:27/:50` 的相对链接断链）；Task 0.4 第三条否定断言（Task 0.3/0.4 验收文本的「…改指…」句、`test ! -e` 验证命令行与 code-agent-runtime.md 的 `test ! -e` 断言行均不含任何原白名单词）；A9 的 claudecode 路径扫描（8 处命中全是计划要求保留的移除性表述，含 `agent_docs_contract.rs:33` 反而断言文档必须包含该字符串）。
3. Task 0.3 预计触达文件原漏 15 处 `src`/`tests` 命中（`db/migration.rs`、`skills/embedded/libra.md`、capability_package 3 处、agent_run 4 处、runtime 4 处——其中 `runtime/{revision,phase0}.rs` 为失效 rustdoc 相对链接——及 tests 2 个文件头注释）与 `template/skills/libra.md`；`sql/migrations/2026053101_ai_final_decision.sql:4` 属已发布迁移文件，豁免改写。规模由 XS/S 调整为 S。
4. `tests/command/agent_checkpoint_test.rs` 是孤儿模块（`tests/command/mod.rs` 未声明 `mod agent_checkpoint_test;`，整文件未编译）；当前 `cargo test --test command_test agent_checkpoint_rewind` "通过"只是 `agent_help_test.rs:54` 的名称碰撞假阳性 → A5 已补接线验收项。
5. feature 门控口径修正：`code_ui_remote_*`、`code_mcp_dual_entry_test`、`code_resume_test` 实为**逐项** `#[cfg(feature = "test-provider")]` 门控 + 1 个 `#[cfg(not(...))]` 的 `*_requires_test_provider_feature` 跳过占位测试；裸跑编译并通过 1 个占位测试（并非 0 个），假通过警示不变——C4/C5/C6/C7/§9 与 code.md 注记已同步改写。
6. 措辞精确化：`docs/commands/zh-CN/agent.md` 现存（A2/A9 的"若存在"改为必须同步）；C4 的 `/api/code/*` 路由注册在 `src/internal/ai/web/mod.rs`（code_router），`code_ui.rs` 承载状态/读模型（§0 已同步）；C2 两个 lib 测试 fn 已存在于 `src/command/code.rs:4342`/`:4712`（hedge 收紧）；`COMPATIBILITY.md` 旧路径链接仅 agent.md 一处（:138，无 code.md 链接）。
7. 环境核查（本机 darwin arm64）：三个真实 agent CLI 均在位且登录态通过 §0.3.2 只读检查（codex 0.142.5 已登录；claude 2.1.200 已认证且 `claude auth status` 确需 redact；opencode 1.17.11 命中 `~/.opencode/bin` 且存在凭证——Homebrew 1.17.10 被影子，印证 §0 记录 PATH 的要求）；§0.3.4 全部 smoke flag 均存在于已装版本 `--help`；`origin` remote、`.env.test`、nightly rustfmt、pnpm 就绪。**A6.5 的环境前置当前不构成 blocked**。web/worker package.json 仍为 0.17.1758（滞后口径不变）。
8. codex review 第一轮（2026-07-04，实跑 `cargo test`）补充两项，已修入：`compat_matrix_alignment` 除 agent.md 外还在运行时读取已迁移的 `docs/development/integration-test-plan.md`（现 `docs/development/integration/integration-test-plan.md`，同属 `932c3a0` 重组提交；仅重指 agent.md 该守卫仍失败）→ Task 0.3 已并入该第三处断链重指与 15 处/9 文件旧引用清理；§9 的 `ai_code_ui_headless_test` 为**整文件** `#![cfg(feature = "test-provider")]` 门控（`tests/ai_code_ui_headless_test.rs:10` 内部属性，裸跑编译 0 个测试、无占位测试，与第 5 条的逐项门控模式不同）→ §9 命令已改为带 `--features test-provider -- --test-threads=1`（带 feature 实跑 13 个用例）。
9. codex review 第二轮（2026-07-04）补充，已修入：同源 `integration-scenarios` 家族（`integration-scenarios.yaml` + `integration-scenarios/<id>.md` 场景文档目录）迁移在 Task 0.3 原只覆盖 `_general.md` 同行引用一处且无守卫断言，而 `tools/integration-runner` 有 4 处功能性路径拼接（`manifest.rs:24`、`plan.rs:53/:118/:177`）按旧路径必失败，两个已迁移文件自身（`integration/integration-test-plan.md:84`、`integration/integration-scenarios/integration-scenarios.yaml:6/:13`）也残留旧根路径 → Task 0.3 补全 21 处/10 文件清单并新增家族否定断言（模式 `docs/development/integration-scenarios` 同时覆盖两种旧形态，不误伤新路径）。
10. codex review 第三轮（2026-07-04）补充，已修入：全路径家族模式测不到的 **bare/相对简写**残留——`integration/integration-scenarios/README.md:3` 的 `../integration-scenarios.yaml` 父级相对链接（yaml 迁移后已并入目录内，该链接必然断链）、`account.md:924/:968`、`_general.md:29`（mermaid 标签）、`tools/integration-runner/README.md:14` 的 bare 简写 → Task 0.3 新增简写规范化验收（13 行/4 文件 + README 断链 1 处）与两条配套否定断言（README 断链断言 + prose 文档行级"必须携带完整新路径"断言，后者已注明 `-v` 同行混写局限、以逐点位核对为准）。

### 0.2 执行粒度与 PR 切分规则

任务卡是验收单元，不一定是单 PR。任何卡若实际触达超过 5 个文件、跨两个以上子系统，或需要新增测试 target + docs + schema/migration，必须拆成多个可独立验证的 PR slice：

1. Contract slice：先落 trait/schema/CLI/RPC/JSON contract、snapshot test 和文档锚点，不引入大规模副作用。
2. Implementation slice：再接入 provider/runtime/writer/workflow 主路径，保持默认路径可回滚或 fail-closed。
3. Safety slice：补 redaction、env isolation、timeout、bounded buffer、migration/backfill（含 `_down.sql` 回滚演练）、crash recovery 和 doctor/cleanup。
4. Compatibility slice：最后同步 `docs/commands/*`、`COMPATIBILITY.md`、`docs/error-codes.md`、`tests/INDEX.md`、release notes 和 compat guard。**例外**：新增可见顶层命令时，`COMPATIBILITY.md` 矩阵行、`docs/development/commands/README.md` 表行、`src/cli.rs` ROOT_AFTER_HELP 组行、`<CMD>_EXAMPLES`/help_examples_banner 接线不得延后——`compat_matrix_alignment`、`root_after_help_lists_every_visible_command`、`compat_help_examples_banner` 在默认 `cargo test --all` 必跑，命令在 `cli.rs` 可见的那个 PR 必须同步这些项，否则违反"每个 PR 可构建、可测试"。
5. Live/local slice：只有 deterministic 测试通过后，才运行 `.env.test` live/provider tests 或 A6.5 本地三 agent smoke；live 证据不得替代 deterministic regression。

拆分后每个 PR 必须让仓库保持可构建、可测试、public behavior 不自相矛盾。若某个 slice 只能引入计划中的测试 target，PR 必须同时说明 target 当前是否已注册、为何暂不能纳入默认必跑，以及下一 slice 的重启条件。

### 0.3 本机真实 Agent 调用验证方案

本节规定开发过程中如何调用本机真实安装的 `codex`、`claude`、`opencode` 做 A6.5 smoke。它是 deterministic fixture 之外的本地验收方案，不能替代单元/compat 测试，也不能在 CI 默认启用。

#### 0.3.1 调用原则

- 默认使用本机真实登录态和真实 CLI binary，不设置临时 `HOME`，否则会绕开用户已登录的 Codex/Claude/OpenCode 凭证；隔离范围放在临时 Libra 仓库和项目级 hook 配置中。
- 每个 agent 使用独立临时仓库串行执行，避免 hook attribution、session owner filtering、checkpoint writer 互相污染；后续如需测试 owner filtering，可另加 combined-repo 专项用例。
- 最终 capture smoke 不使用会关闭 session/hook/transcript 持久化的选项：Codex 不用 `--ephemeral`；Claude 不用 `--bare`、`--safe-mode`、`--no-session-persistence`；OpenCode 不用会绕过项目配置或权限边界的危险模式。
- prompt 必须是非破坏性、低成本、可识别的固定文本，要求 agent 不读 secret、不改文件、不执行命令，只返回固定短语。
- 本地 smoke 默认不读取 `.env.test`，优先使用各 CLI 的本地登录态。**禁止对 agent 子进程整文件 `source .env.test`**——那会把全部 provider/D1/R2/GitHub key 注入不受 `env_clear` 保护的外部 agent 环境。当某个 CLI 的 provider 确实需要环境变量时，由 harness 解析 `.env.test` 并只提取该 agent 所需的具名 key（例如仅 `MOONSHOT_API_KEY`），经子进程环境注入（Rust 端 `Command::env`，shell 端在关闭 xtrace 的子 shell 内注入），其余 key 一律不注入；严禁把 key 名=值写入命令行、日志或证据。`env_clear` + allowlist 原则同样适用于 shell 直启的真实 agent CLI（HOME 等登录态所需基础变量除外）。
- 证据目录根目录即 summary 区：只保存 redacted stdout/stderr 摘要、命令退出码、binary path/version（做 `$HOME`→`~` 归一化）、Libra JSON 查询结果和对象/hash 标识，可留存/可入 PR。child process 的原始 stdout/stderr 一律落在 `raw/` 子目录：仅存在于本机 0700 临时目录、文件权限 0600、默认测试结束即删、永不提交、不作为 §0.3.5 断言对象。"redacted-only" 禁令适用于 summary 区及任何随 PR 提交的证据：不得包含 raw transcript、prompt 原文以外的模型上下文、provider token、`.env.test` 内容。

#### 0.3.2 Preflight

开发者先构建当前工作树的 `libra`，并固定本轮 smoke 使用的绝对路径：

```bash
set +x
export LIBRA_SRC=/Volumes/Data/GitMono/libra
cd "$LIBRA_SRC"
LIBRA_SKIP_WEB_BUILD=1 cargo build --bin libra   # smoke 不需要 web 前端；不跳过则在无 pnpm 的机器上必失败
export SMOKE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/libra-agent-smoke.XXXXXX")"   # mktemp -d 默认 0700
mkdir -p "$SMOKE_ROOT/bin"
cp "$LIBRA_SRC/target/debug/libra" "$SMOKE_ROOT/bin/libra"
export LIBRA_BIN="$SMOKE_ROOT/bin/libra"
shasum -a 256 "$LIBRA_BIN"   # sha256 记入 evidence summary；路径做 $HOME→~ 归一化
"$LIBRA_BIN" --version
export SMOKE_PROMPT="Libra local agent capture smoke. Do not read secrets, do not edit files, do not run shell commands. Reply exactly: libra-agent-smoke-ok."
```

`LIBRA_BIN` 必须指向复制进 `$SMOKE_ROOT/bin/` 的**不可变 pinned 副本**，而不是 `target/debug/libra` 原路径：hook entry 会把二进制的 canonicalize 绝对路径写进外部 agent 配置（`resolve_hook_binary_path` 基于 `current_exe`），pin 副本保证并发/后续 `cargo build` 重建 `target/` 不影响已安装 hook 的完整性，并使 sha256 provenance 与 A3 对外部 binary 的口径对齐。§0.3.6 的 Rust harness 同理，必须先把 `env!("CARGO_BIN_EXE_libra")` 指向的二进制复制进 smoke tempdir，再以副本安装 hooks。

记录三类本机 agent 的路径和版本；输出可以进入证据 summary，但不得包含账号、token 或 provider key：

```bash
command -v codex
codex --version
command -v claude
claude --version
command -v opencode
opencode --version
```

登录态是 blocked 硬判据（见 §0 采集验收规则），必须在发起任何真实付费会话前用只读命令机械判定：

```bash
codex login status
claude auth status
opencode providers list
```

- 证据只保留 redacted 摘要：是否已登录的布尔判定 + 退出码。`claude auth status` 默认输出包含 email/orgId/orgName，必须先 redact 再入 evidence；`opencode providers list` 只记录默认 provider 是否存在至少一条凭证。
- 未登录时的确切退出码/输出以当时 CLI 版本实测为准并更新本节；任一 agent 登录检查未通过即按 §0 规则标记 blocked，不得进入 §0.3.4 真实会话。

当前开发机已验证的非交互入口形态如下；后续版本变化时以 `--help` 重新核对并更新本节（重新核对范围不止 CLI 入口形态，还包括 transcript/hook 输出格式，对应的 fixture 刷新策略见 A6/A6.5）：

| Agent | 已验证入口 | 关键安全参数 | 禁用参数 |
|---|---|---|---|
| `codex` | `codex exec [PROMPT]` | `-C <repo>`、`--skip-git-repo-check`、`--sandbox read-only`、`--json`、`-o <last-message-file>` | `--ephemeral` |
| `claude` | `claude -p [PROMPT]` | `--permission-mode plan`、`--output-format stream-json`、`--verbose`（`claude -p` 下 `stream-json` 强制要求 `--verbose`，2.1.x 实测缺失时直接以退出码 1 失败）、`--include-hook-events`、`--max-budget-usd <small>` | `--bare`、`--safe-mode`、`--no-session-persistence` |
| `opencode` | `opencode run [message..]` | `--dir <repo>`、`--format json`、`--title <name>` | `--dangerously-skip-permissions` |

#### 0.3.3 每个 Agent 的临时仓库与 hook 安装

每个 agent 单独创建临时 Libra 仓库。AG-17 alias 落地前，开发期可以用 canonical `enable/status`；AG-17 落地后，A6.5 必须同时验证 `add/list` alias 和 canonical 入口同语义。

```bash
# $SMOKE_ROOT 已在 §0.3.2 preflight 创建（0700），并已放入 pinned 的 $LIBRA_BIN
mkdir -p "$SMOKE_ROOT/evidence"

prepare_agent_repo() {
  slug="$1"
  repo="$SMOKE_ROOT/$slug/repo"
  evidence="$SMOKE_ROOT/$slug/evidence"
  mkdir -p "$repo" "$evidence/raw"
  "$LIBRA_BIN" init -q "$repo"   # 必须 -q：函数 stdout 被命令替换捕获，init banner 混入会使 $repo 变成多行文本
  printf '%s\n' "$repo"
}
```

安装 hooks 时使用 §0.3.2 pinned 的 `LIBRA_BIN`，并把安装结果保存为 redacted JSON；`agent add` 之前先把 provider 配置文件现状（可为空）快照进 `$evidence/preinstall.snapshot`，供 §0.3.5 卸载对比：

```bash
repo="$(prepare_agent_repo claude-code)"
cd "$repo"
"$LIBRA_BIN" agent add claude-code --json > "$SMOKE_ROOT/claude-code/evidence/add.json"
"$LIBRA_BIN" agent list --json > "$SMOKE_ROOT/claude-code/evidence/list.after-add.json"

repo="$(prepare_agent_repo codex)"
cd "$repo"
"$LIBRA_BIN" agent add codex --json > "$SMOKE_ROOT/codex/evidence/add.json"
"$LIBRA_BIN" agent list --json > "$SMOKE_ROOT/codex/evidence/list.after-add.json"

repo="$(prepare_agent_repo opencode)"
cd "$repo"
"$LIBRA_BIN" agent add opencode --json > "$SMOKE_ROOT/opencode/evidence/add.json"
"$LIBRA_BIN" agent list --json > "$SMOKE_ROOT/opencode/evidence/list.after-add.json"
```

若当前 slice 尚未落地 AG-17 alias，则等价使用：

```bash
"$LIBRA_BIN" agent enable --agent <slug> --json
"$LIBRA_BIN" agent status --json
```

安装验收必须断言：

- `list/status --json` 中目标 agent `supported=true`、`support_wave="first_batch"`。
- HookProvider 已落地时目标 agent `hook_installable=true`、`installed=true`、`capabilities.hooks=true`。
- provider 配置写入以**上游实测为准**：
  - `claude-code`：`.claude/settings.json` 只增加 Libra-managed hook entry，保留用户已有项目配置。
  - `codex`：上游**没有**已证实的 `.codex/hooks.json` 加载路径；以实测 codex CLI（记录版本，如 0.142.5）验证的加载形态为准（本地 plugin 的 hooks 定义或 config-layer hooks 键，含 project `.codex/config.toml`）。Codex hooks 还受**用户级 trust/enable 双重门控**（`~/.codex/config.toml` `[hooks.state]`）：installer 必须写入自有 entry（trusted_hash + enabled=true，只增不删，卸载时清理自有条目），或 smoke 命令显式携带 bypass 参数并在 §0.3.2 参数表登记豁免理由；无论哪种方案，实现 PR 必须用真实 `codex exec` 冒烟证明 hook 真正被读取并触发（而非静默不执行），并同步修订 `agent.md` 对应节（含未经 0.142.x 证实的 `[features] hooks = true` 键）。"保留用户已有项目配置"对 codex 放宽为：允许触碰用户级 config.toml 的 `[hooks.state]` 段，但仅限 Libra 自有 entry。
  - `opencode`：上游**没有** hooks.json 机制；hook 能力预期为 `.opencode/plugins/` 下的 Libra 插件文件或 `opencode.json` plugin 项，实际文件名/形态由实现 PR 依上游实测固定并加 compat test，同 PR 回写 `agent.md` 对应节。单数 `.opencode/plugin/` 只可作为上游兼容读取/迁移输入，不作为 Libra 新写入目标。验收口径为"只增删 Libra-managed 文件/条目，不触碰用户其余 `.opencode/` 配置"。
- 每个 Libra-managed hook entry 的 command 必须以 §0.3.2 pinned 的 `$LIBRA_BIN` 绝对路径开头（不允许裸 `libra` PATH 查找），安装后断言之。
- 非目标 agent 不因本轮安装被标记为 installed 或 launchable。

#### 0.3.4 真实 CLI 非交互调用矩阵

每个命令都在对应临时仓库中执行。child process 的原始 stdout/stderr 只写入 `$evidence/raw/`（0700 目录内的 0600 文件，默认测试结束即删，分区规则见 §0.3.1）；harness 消费 raw 流后经 redaction + 截断，产出 evidence 根目录（summary 区）下的 redacted 摘要，只有 summary 区可留存。注意 shell `>` 重定向默认按 umask 产生 0644 文件——手工执行时需先 `umask 077`，Rust harness 用 `OpenOptions` mode(0o600)。测试 harness 必须对每个 child process 设置超时（建议 180 秒），超时后按**进程组** kill（覆盖 agent 自行 spawn 的子进程）、写 terminal state、保留 redacted summary。

Codex：

```bash
repo="$SMOKE_ROOT/codex/repo"
evidence="$SMOKE_ROOT/codex/evidence"
codex exec \
  -C "$repo" \
  --skip-git-repo-check \
  --sandbox read-only \
  --json \
  -o "$evidence/raw/codex.last-message.txt" \
  "$SMOKE_PROMPT" \
  > "$evidence/raw/codex.stdout.jsonl" \
  2> "$evidence/raw/codex.stderr.log"
```

Claude Code：

```bash
repo="$SMOKE_ROOT/claude-code/repo"
evidence="$SMOKE_ROOT/claude-code/evidence"
cd "$repo"
claude -p \
  --permission-mode plan \
  --output-format stream-json \
  --verbose \
  --include-hook-events \
  --max-budget-usd 0.05 \
  "$SMOKE_PROMPT" \
  > "$evidence/raw/claude.stdout.jsonl" \
  2> "$evidence/raw/claude.stderr.log"
```

OpenCode：

```bash
repo="$SMOKE_ROOT/opencode/repo"
evidence="$SMOKE_ROOT/opencode/evidence"
opencode run \
  --dir "$repo" \
  --format json \
  --title libra-agent-smoke-opencode \
  "$SMOKE_PROMPT" \
  > "$evidence/raw/opencode.stdout.jsonl" \
  2> "$evidence/raw/opencode.stderr.log"
```

这些命令只验证真实 agent 能触发 Libra-managed capture path，不验证模型回答质量。只要 agent 成功完成一次非交互 turn，并且 Libra 侧捕获到 lifecycle/session/checkpoint，即视为该 agent 的本地调用路径满足 smoke 要求。claude 的 stream-json stdout（含 `--include-hook-events` 的 hook 事件）可经 redaction 后作为 SessionStart/SessionEnd 触发的辅助证据摘要进入 summary 区，但验收口径仍以 Libra 侧 `agent session list` / `checkpoint list` 捕获结果为准。

#### 0.3.5 Libra 侧断言

每个 agent 命令完成后，在对应临时仓库执行以下查询并保存证据：

```bash
"$LIBRA_BIN" agent list --json > "$evidence/list.after-run.json"
"$LIBRA_BIN" agent session list --json > "$evidence/session-list.json"
"$LIBRA_BIN" agent checkpoint list --json > "$evidence/checkpoint-list.json"
"$LIBRA_BIN" agent doctor --json > "$evidence/doctor.json"
```

必须断言：

- `agent_session.agent_kind` 分别出现 `claude_code`、`codex`、`opencode`。
- 每个 agent 至少有一次 session start、一次 user turn / prompt boundary、一次 stop/session end 或等价 terminal lifecycle event。
- 每个 agent 至少写出一个 checkpoint，且 checkpoint 能关联到 `refs/libra/traces`、manifest、redaction report 和 content hash。
- 默认 `session list` / `checkpoint list` / `checkpoint show` 不读取 raw transcript body；只有显式 detail/transcript 路径才允许读取 body。
- `doctor --json` 不报告 ref/DB/object_index 不一致；若报告 warning，必须分类为已知非阻断项并写入 evidence summary。
- summary 区证据与任何随 PR 提交的内容中不得出现 `.env.test` 值、API key、provider token 或 raw transcript 大段内容；hook 捕获的 transcript 与 checkpoint 内容中也不得出现 `.env.test` 内任何未注入 key 的值（违反按 §0.3.6 归类为安全阻断）。路径脱敏规则具体化：`$HOME` 前缀替换为 `~`、用户名替换为固定占位符；§0.3.2 要求记录的 binary path/version 属允许白名单（记录时同样做 `$HOME`→`~` 归一化）。

三条采集断言完成后，每个 agent 追加一轮卸载 smoke：

```bash
"$LIBRA_BIN" agent remove <slug> --json > "$evidence/remove.json"
"$LIBRA_BIN" agent list --json > "$evidence/list.after-remove.json"
```

（AG-17 alias 落地前等价使用 `disable --agent <slug>` + `status --json`，与 §0.3.3 安装侧口径一致。）

- 与 §0.3.3 保存的 `preinstall.snapshot` 对比：provider 配置文件中仅 Libra-managed entry 被移除、用户条目语义等价保留（codex 另按 §0.3.3 处理 Libra 写入的 `[hooks.state]`/features 项）；已捕获的 agent_session/agent_checkpoint/refs/libra/traces 数据不删除。
- `list --json` 显示该 agent `installed=false` 且 `hook_installable=true`；对未安装状态重复 remove 幂等（exit 0 或明确提示，不报错误栈）。
- "卸载后 hook 不再写入"以"配置文件中 Libra hook 调用 entry 已不存在"作等价断言，不再跑一轮真实 agent 会话。
- 可选：向本地 file remote 执行一次 `"$LIBRA_BIN" agent push --remote <local-file-remote>` 并断言 refs/libra/traces 推送成功，为 A5 的 push 语义提供本地证据。

#### 0.3.6 自动化测试入口

A6.5 的 `agent_local_capture_smoke_test` 必须把上面的 shell 流程固化为 Rust harness，而不是依赖人工复制命令。建议约定：

```bash
LIBRA_RUN_LOCAL_AGENTS=1 \
LIBRA_LOCAL_AGENT_SET=codex,claude-code,opencode \
LIBRA_KEEP_LOCAL_AGENT_SMOKE=1 \
cargo test --test agent_local_capture_smoke_test -- --ignored --test-threads=1
```

环境变量语义：

| 变量 | 默认 | 作用 |
|---|---|---|
| `LIBRA_RUN_LOCAL_AGENTS` | unset | 未设置时 ignored test 必须 skip，防止 CI 或普通本地测试误调用真实付费 agent。 |
| `LIBRA_LOCAL_AGENT_SET` | `codex,claude-code,opencode` | 允许开发者临时只跑一个 agent；A6.5 完成前必须跑全量三项。 |
| `LIBRA_KEEP_LOCAL_AGENT_SMOKE` | unset | 设置为 `1` 时保留临时 repo/evidence，便于排查；默认测试结束后删除。 |
| `LIBRA_LOCAL_AGENT_TIMEOUT_SECS` | `180` | 每个真实 agent child process 的超时上限。 |
| `LIBRA_LOCAL_AGENT_EVIDENCE_DIR` | tempdir | 指定 evidence 输出目录；目录内容视为敏感，不提交。 |

harness 硬性要求：

- evidence 目录（含自定义 `LIBRA_LOCAL_AGENT_EVIDENCE_DIR`）由 harness 创建或校验为 0700，写出的文件为 0600（shell 重定向默认 0644，harness 需显式 chmod 或 `OpenOptions` mode）；目录非空或权限过宽时拒绝执行。默认 mktemp 路径本已 0700，规则重点覆盖自定义目录。
- harness 启动时先把 `env!("CARGO_BIN_EXE_libra")` 指向的二进制复制进 smoke tempdir，以副本安装 hooks 并记录副本 sha256（cargo test 自身会重建 `target/`，不能直接引用原路径；见 §0.3.2 pin 规则）。
- `LIBRA_KEEP_LOCAL_AGENT_SMOKE=1` 保留 evidence（含 raw/）时必须打印敏感内容警告。
- shell 样例中所有被命令替换（`$(...)`）包裹的 libra 调用必须静默 stdout（如 `init -q` 或重定向到 stderr），防止命令 banner 混入被捕获的路径/值；Rust harness 中等价要求是不从 child stdout 解析路径。

失败分层：

- preflight 失败：缺 binary、版本命令失败或 PATH shadowing，归类为环境阻断，不算实现通过。
- 登录态缺失/凭证过期：§0.3.2 登录检查未通过，归类为环境阻断并按 §0 规则标记 blocked；恢复步骤分别为 `codex login`、`claude` 登录流程、`opencode` provider 凭证配置（以当时 CLI 版本实测为准），不得进入 §0.3.4 真实会话。
- hook 安装失败：归类到 A4 HookProvider / AG-19；需区分"hook 配置未被真实 CLI 读取"与"hook 已读取但被 trust/enabled 门控拦截"（codex 双重门控，见 §0.3.3）两类。
- agent 命令成功但没有 session：归类到 hook 配置未被真实 CLI 读取、lifecycle ingest 或 owner filtering。
- 有 session 但无 checkpoint：归类到 A5 writer、manifest、`refs/libra/traces` 或 object_index。
- 有 checkpoint 但 evidence 泄露 secret/raw transcript：归类为安全阻断，不能以 smoke 通过。

### 0.4 每任务发布、提交与部署流程（强制）

每完成一个任务卡（或 §0.2 拆分后的一个可独立验证 slice），必须按以下顺序完成发布闭环；未走完本节流程的任务不得在 §10 进度表标记完成。

1. **完成判定（代码优先）**：不以文档修改或任务卡打勾作为功能完成依据，必须分析代码确认实现存在且行为正确（读实现源码 + 运行该卡「验证」清单全部命令）。反向同样成立：更新代码时必须同步更新对应文档（`docs/commands/*`、zh-CN 页、`COMPATIBILITY.md`、`tests/INDEX.md`、本目录 tracing 文档相关章节）。
2. **质量门禁**（与 CLAUDE.md 质量验收标准一致，三项全部通过才可进入发布）：

   ```bash
   cargo +nightly fmt --all --check
   cargo clippy --all-targets --all-features -- -D warnings
   source .env.test && cargo test --all   # macOS 上加 RUST_MIN_STACK=33554432 前缀，避免 tag 测试栈溢出误报
   ```

   两条既定豁免（均须记入 §10，其余失败一律阻断发布）：
   - **非本改动导致的既有失败**（如 main HEAD 已存在的失败测试）：空闲时单目标复跑确认与本改动无关后可继续发布——与 A-F5/C-F4 检查点"记录明确、非本改动导致的既有失败"同口径。
   - **L2/L3 gated 测试**：本任务不触达网络/云/AI 面时，可不 `source .env.test` 直接 `cargo test --all`（未设 env 的 L2/L3 打印 skipped，不算失败）；触达对应面时才必须 source 实跑。

3. **版本递增（patch +1）**：读取 `Cargo.toml` 当前 `version`（并发节点可能已递增，**bump 前必须重读**），patch 加 1。例：当前 `version = "0.17.500"` → 改为 `version = "0.17.501"`。同一提交内把 `web/package.json` 与 `worker/package.json` 的 `"version"` 字段同步为同一新值（两文件当前滞后于 `Cargo.toml` 属已知状态，首次执行本流程时一并同步到新版本）。
4. **Cargo.lock 不手改**：不得手工编辑 `Cargo.lock`；执行 `cargo build --release`，由工具链更新 `Cargo.lock` 并产出 release 二进制。（release 构建默认包含 web 前端构建，需要 `pnpm`；若 web 构建故障且本任务不触达 web 资产，可临时 `LIBRA_SKIP_WEB_BUILD=1 cargo build --release` 并在 §10 记录。）
5. **提交与推送**（本仓库由 Libra 管理，用 `libra` 命令，推送到 GitHub main）：

   ```bash
   libra status --short          # 先核对工作树变更是否全部属于本任务 + 版本文件
   libra add <本任务触达文件与版本文件>
   libra commit -a -s -m "<type>(<scope>): <summary> (v0.17.NNN)"
   libra push origin main
   ```

   并发工作区 fallback：`libra status --short` 显示存在**非本任务**的变更（并发节点 in-flight 产物等）时，不得使用 `-a`（它会收编 status 检查与 commit 之间出现的一切工作树变更），改用显式 `libra add <本任务文件>` + `libra commit -s -m`（不带 `-a`，仅提交 staged），且不得 revert/restore 并发节点的变更；`-a` 只在确认工作树仅含本任务变更时使用。commit message 遵循 CLAUDE.md 类型化规范（`feat(agent): ...` / `fix(code): ...` 等）；单个 `-m`，内容不含反引号。
6. **部署本机二进制**：

   ```bash
   cp target/release/libra "$HOME/.libra/bin/libra"
   "$HOME/.libra/bin/libra" --version   # 若 macOS 上被 SIGKILL(137)，执行 codesign --force --sign - "$HOME/.libra/bin/libra" 后重试
   ```

7. **记录**：在 §10 执行进度表追加一行：日期、任务/slice、结果、版本号、commit id、验证摘要（或 blocked 原因与恢复步骤）。

直推 main 模式说明：当前按"每任务直推 main"执行，§0.2 中"每个实现 PR 必须说明…"的要求由 commit message + §10 进度表行承载；每个 slice 对应一次本节发布循环，任何一次提交后仓库必须保持可构建、可测试、public behavior 不自相矛盾。

只读任务豁免：无代码变更的只读任务（如 Task 0.1 基线记录、C1 初轮 audit）免第 3/4/6 步（版本递增、release 构建、部署）；其产出（基线记录、差距清单写回 `code.md` 等）若形成文档变更仍走第 5 步提交推送，并完成第 7 步 §10 记录；完全无文件变更时仅做第 7 步。

### 0.5 Claude 执行者操作规程（执行入口）

本计划可由 Claude（或其他 agent 执行者）自主逐任务执行。每个任务的执行循环：

1. **定位状态**：读 §10 执行进度表 + `libra log`（用 `--oneline` 类简洁输出确认已发布版本与已完成任务）；重读 `Cargo.toml` 当前 version。
2. **选任务**：按 §1/§2/§5 依赖关系选下一个所有依赖已完成的任务。顺序基线：Task 0.1 → **0.3**（先修守卫断链，否则一切验证命令不可用）→ **0.4**（清理 internal/tracing 事实源 drift）→ 0.2 → A1 → A2/A3（可并行）→ A4 → A5 → A6 → A6.5 → A7/A8 → A8.5 → A9 →（§5 进入条件）→ C1 → C2 → C3/C4 → C5/C6 → C7 → C8。同一时间只推进一个任务到发布。
3. **开工前 source-audit**：逐条核对该卡验收标准当前代码是否已满足（不以任何文档断言为准，含本计划自身——以 `rg`/Read 源码与测试为准）；已满足项记录证据（文件+行号/测试名）后视为完成，不重复实现。
4. **实施**：按 §0.2 切分 slice；遵循 CLAUDE.md 编码规范（错误处理不 `unwrap()`、`_with_conn` 事务变体、测试隔离、hash-kind preflight 等）。
5. **验证**：运行该卡「验证」清单全部命令，注意 feature 门控（`--features test-provider -- --test-threads=1`）与 env-gate（`LIBRA_RUN_LOCAL_AGENTS=1` 等）标注。测试 target 注册规则：`tests/compat/*` 文件必须注册 `Cargo.toml [[test]]`（Cargo 默认不发现子目录）并加 `tests/compat/README.md` 行；顶层 `tests/*.rs` 自动发现、无需注册 `[[test]]`；两类都必须同步 `tests/INDEX.md`。
6. **发布**：执行 §0.4 全流程。
7. **收尾**：在本文档勾选该卡已满足的验收 checkbox，更新 §10；验收项 deferred 时写明原因与重启条件。
8. **阻塞处理**：任务 blocked（如 A6.5 缺登录态、上游 CLI 行为与 §0.3 记录不符）时，在 §10 记录 blocked + 缺失项 + 恢复步骤，转向下一个依赖已就绪的任务；不得静默跳过验收项，不得用 fake 证据顶替。特别地，**A6.5 blocked 时可推进的任务集合仅 A8.5**（其 findings GC 子项因前置 A7/A8 未动工而按验收条款显式 deferred）；A7/A8/A9 及 Code 阶段全部保持 blocked，此时按 §0 规则将 Agent 第一期整体标记 blocked 并等待环境恢复，不要反复空转选任务。

执行环境前提（开工前一次性检查，缺失项按 blocked 处理并记录）：

- Rust stable 工具链 + nightly rustfmt（`cargo +nightly fmt` 可用）。
- `pnpm` 可用（release 构建嵌入 web 前端所需）；`LIBRA_SKIP_WEB_BUILD=1` 仅用于快速 check 或 web 构建故障降级（记录到 §10）。
- macOS 跑 `cargo test --all` 需 `RUST_MIN_STACK=33554432`；少量进程/RPC 类测试在高并发下有超时抖动，空闲时单目标复跑确认后不作为回归处理。
- 仓库根 `.env.test` 存在（live 验证用，凭证使用规则见 §0）。
- A6.5 前置：本机 `codex`/`claude`/`opencode` CLI 与登录态（§0.3.2 preflight）。
- 并发工作区注意：同一工作树可能存在并发节点提交（`Cargo.toml` version 被并发递增属正常现象）；每次 bump 前重读版本，`libra status --short` 发现非本任务变更时不得随 `commit -a` 带入。

## 1. 全局准备与守卫

### Task 0.1：建立当前事实基线

**描述**：在任何实现前记录源码、文档、测试和现有命令面的真实状态，避免按过时文档实施。

**关联设计文档**：[`agent.md`](agent.md)、[`code.md`](code.md)。执行时只核对当前实现与这两份目标文档的差距，不重新定义 Agent 或 Code 的设计方向。

**验收标准**：

- [x] 读取 `docs/development/tracing/agent.md`、`docs/development/tracing/code.md`、`docs/development/commands/_general.md`。
- [x] 核对 `src/cli.rs`、`src/command/mod.rs`、`src/command/agent/*`、`src/command/code.rs` 的公开入口。
- [x] 核对 `tests/INDEX.md`、`Cargo.toml [[test]]`、相关 compat tests 的当前注册状态。
- [x] 用 `libra status --short` 记录工作树，后续只提交本轮任务涉及路径。

**验证**：

- [x] `rg -n "AgentSubcommand|CodeArgs|CodeProvider|compat_agent|code_cli" src tests Cargo.toml tests/INDEX.md`
- [x] `libra diff --stat -- docs/development/tracing/agent.md docs/development/tracing/code.md docs/development/tracing/plan.md`

**依赖**：无。

**预计触达文件**：无代码变更；只读。

**规模**：XS。

### Task 0.2：固定公共命令开发规则

**描述**：把 `_general.md` 的跨命令规则变成后续 PR checklist，尤其是 JSON/machine 输出、错误码、docs/commands、COMPATIBILITY 和 tests/INDEX 同步。

**关联设计文档**：[`agent.md`](agent.md)、[`code.md`](code.md)。跨命令规则只作为执行约束；具体功能目标仍以这两份 tracing 设计文档为准。

**验收标准**：

- [x] 每个新增/修改命令都有用户文档、开发文档、测试和 compat 证据。
- [x] 所有用户可见错误走 `CliError` / `StableErrorCode`，消息可操作。
- [x] 新增测试 target 同步 `Cargo.toml`、`tests/INDEX.md`，compat 文件同步 `tests/compat/README.md`。

本卡交付物说明：规则本身已固化在 §0.2/§0.4/§0.5 checklist 中，本卡无独立代码变更；验收即下列两个守卫跑绿（依赖 Task 0.3 先修复断链）+ §10 记录一行。

**验证**：

- [x] `cargo test --test compat_matrix_alignment`（Task 0.3 完成前预期失败，不得据此标记本卡完成）
- [x] `cargo test --test compat_error_codes_doc_sync`

**依赖**：Task 0.1、0.3、0.4（0.3 未完成时本卡验证命令必失败；0.4 未完成时跨文档事实源仍不干净）。

**预计触达文件**：按后续任务决定。

**规模**：XS。

### Task 0.3：文档搬迁接线（tracing/ 新路径守卫与索引重指）

**描述**：`docs/development/commands/{agent,code}.md` 已迁移为 `docs/development/tracing/{agent,code}.md`，但守卫与索引仍指旧路径，导致 `cargo test --all` 在当前树上直接编译失败（`include_str!` 指向不存在的文件）。同一次重组提交（`932c3a0` docs(development): reorganize planning docs）还把 `docs/development/integration-test-plan.md` 迁至 `docs/development/integration/integration-test-plan.md`，而 `compat_matrix_alignment` 在运行时同样读取该旧路径——仅重指 agent.md 无法让该守卫跑绿（2026-07-04 codex review 实跑 `cargo test` 确认），本卡一并重指。本卡必须在任何 Agent/Code 实现任务开工前完成，否则 §9 与各任务卡引用的 `compat_agent_docs_contract`、`cargo test --all` 均不可执行。

**关联设计文档**：[`agent.md`](agent.md)、[`code.md`](code.md)。只做路径重指与索引同步，不改变守卫断言语义。

**验收标准**：

- [x] `tests/compat/agent_docs_contract.rs` 的 `include_str!("../../docs/development/commands/agent.md")` 重指 `../../docs/development/tracing/agent.md`；守卫内其它路径引用同步核对。
- [x] `tests/compat/matrix_alignment.rs` 的**运行时**旧路径读取同步重指：`read_repo_file("docs/development/commands/agent.md")`（约 :104）与相关 context 字符串（约 :171）——该守卫对缺失文件直接 panic，是 `include_str!` 之外的第二处断链，`cargo check` 抓不到。
- [x] `compat_matrix_alignment` 的**第三处**断链（旧 `docs/development/integration-test-plan.md`，已迁至 `docs/development/integration/integration-test-plan.md`）同步重指：`tests/compat/matrix_alignment.rs`（:103 运行时读取、:161/:166 context 字符串）、`tests/compat/matrix_alignment_support.rs`（:174/:181 运行时读取）、`tools/integration-runner/src/plan.rs:101`（功能性路径常量）以及 `tests/INDEX.md`、`tools/integration-runner/README.md`、`AGENTS.md`、`docs/development/commands/_general.md`、`docs/development/account.md`、`docs/development/gap/grit-gap.md` 的 live 引用——2026-07-04 实测旧路径共 15 处/9 文件（与下一条的命中行有重叠）。
- [x] 同源场景清单/场景文档迁移一并重指：旧 `docs/development/integration-scenarios.yaml` 与旧 `docs/development/integration-scenarios/<id>.md` 目录形态（均已迁至 `docs/development/integration/integration-scenarios/`）——**功能性路径拼接** `tools/integration-runner/src/manifest.rs:24`、`tools/integration-runner/src/plan.rs`（:53 目录 join、:118/:177 场景文档拼接；:51/:121 为注释与错误消息），按旧路径 runner 必失败；doc 注释 `tools/integration-runner/src/cli.rs:14`、`tools/integration-runner/src/registry.rs:11`、`tools/integration-runner/README.md:14`；文档引用 `docs/development/commands/_general.md`（:9/:20/:68）、`docs/development/account.md`（:1039/:1040/:1042）、`docs/development/gap/grit-gap.md`（:612/:613/:691）；以及两个已迁移文件自身残留的旧根路径：`docs/development/integration/integration-test-plan.md:84`、`docs/development/integration/integration-scenarios/integration-scenarios.yaml`（:6/:13 头部注释）——2026-07-04 实测全路径旧形态共 21 处/10 文件。
- [x] 同族 **bare/相对简写**引用一并规范化（全路径 rg 测不到，须单列）：`docs/development/integration/integration-scenarios/README.md:3` 的 `../integration-scenarios.yaml` 父级相对链接（迁移前 yaml 在目录外、现已并入目录内，链接必然断链——改为同目录 `integration-scenarios.yaml`；同文件 :4 的 `../integration-test-plan.md` 现可解析、无需改）；`docs/development/account.md:924/:968` 与 `docs/development/commands/_general.md:29`（mermaid 标签）、`tools/integration-runner/README.md:14` 的 bare `integration-scenarios.yaml` / `integration-scenarios/<file>` 简写——上述 prose 文档中所有 `integration-scenarios` 提及统一规范化为完整新路径 `docs/development/integration/integration-scenarios/…`（目录内相对链接仅允许 integration-scenarios/ 目录内部文件互引）。2026-07-04 实测规范化缺口 13 行/4 文件 + README 断链 1 处。
- [x] 执行期新发现的**第七处运行时断链**（2026-07-04 实跑 `cargo test --all` 暴露，原五轮核查与 codex review 的 rg 模式均未覆盖 tests/ 内的 code-agent-runtime.md 引用）：`tests/ai_provider_transform_test.rs:281` 在运行时读取已删除的 `docs/development/code-agent-runtime.md`（`provider_capability_update_guide_documents_reasoning_variant_workflow`），重指 `docs/development/internal/code-agent-runtime.md`；重指前已核对新路径文档包含该测试断言的全部 10 个期望字符串。该文件属 Task 0.4 否定断言的扫描范围之外（0.4 只扫 4 个 doc 文件），故由本卡承接；`rg -n "docs/development/code-agent-runtime" src tests tools Cargo.toml AGENTS.md` 重指后仅剩零命中。
- [x] `tests/INDEX.md` 与 `tests/compat/README.md` 中上述守卫的 source mapping 更新为 tracing/ 新路径。
- [x] `COMPATIBILITY.md` 中指向 `docs/development/commands/agent.md` 的链接改指 tracing/ 新路径（2026-07-04 实测仅 :138 一处 agent.md 链接，无 code.md 链接）。
- [x] `docs/development/commands/README.md` 的 agent/code 表行改指 tracing/ 新路径或注明已迁移（不留断链）。注意 :27/:50 是相对链接 `](agent.md)` / `](code.md)`，全路径 rg 测不到，须用下方专项否定断言验证。
- [x] 本卡负责范围内的旧路径余量**一律改指 tracing/ 新路径**，范围为 `src`、`tests`、`Cargo.toml`、`COMPATIBILITY.md`、`AGENTS.md`、`docs/commands/`、`docs/development/commands/`、`template/`（含源码注释、rustdoc 链接、embedded/template 技能文档）。四类明确豁免（不由本卡触碰，避免验收与验证自相矛盾）：`docs/development/tracing/plan.md`（任务文本必须引用旧路径字符串）、`docs/development/tracing/memory.md`（§0 范围外文档，其旧路径引用随 A9 banner 处理）、`docs/development/internal/code-agent-runtime.md`（Task 0.4 承接）、`sql/migrations/*`（已发布迁移文件不得改写，`2026053101_ai_final_decision.sql:4` 注释保留为历史引用）。

**验证**：

- [x] `cargo test --test compat_agent_docs_contract`
- [x] `cargo test --test compat_matrix_alignment`（须 agent.md 与 integration-test-plan.md 两类断链都重指后才可跑绿）
- [x] `LIBRA_SKIP_WEB_BUILD=1 cargo check --all-targets`（确认无 `include_str!` 断链）
- [x] `! rg -n "docs/development/commands/(agent|code)\.md" src tests Cargo.toml COMPATIBILITY.md AGENTS.md docs/commands docs/development/commands template`（否定断言，期望零命中；本卡完成前预期失败——2026-07-04 实测该范围 29 处命中/约 25 个文件。豁免的 plan.md、memory.md、internal 文档与 sql 迁移不在扫描范围，分别由本卡豁免声明、A9、Task 0.4 承接；原 `docs` 全域扫描因命中上述豁免文件永不可跑绿，已废弃）
- [x] `! rg -n "\]\((agent|code)\.md\)" docs/development/commands/README.md`（否定断言：README 相对链接断链已清理，期望零命中）
- [x] `! rg -n "docs/development/integration-test-plan\.md" src tests tools docs AGENTS.md -g '!docs/development/tracing/plan.md'`（否定断言，期望零命中；本卡完成前预期失败——2026-07-04 实测 15 处命中/9 文件。plan.md 经 `-g` 排除：本卡任务文本必须引用旧路径字符串）
- [x] `! rg -n "docs/development/integration-scenarios" src tests tools docs AGENTS.md -g '!docs/development/tracing/plan.md'`（否定断言，期望零命中；一个模式同时覆盖 `.yaml` 与 `/<id>.md` 两种旧形态；新路径 `docs/development/integration/integration-scenarios/…` 因中间多一级 `integration/` 不会被命中；本卡完成前预期失败——2026-07-04 实测 21 处/10 文件）
- [x] `! rg -n "\]\(\.\./integration-scenarios\.yaml\)" docs/development/integration/integration-scenarios/README.md`（否定断言：README 迁移后失效的父级相对链接已修复；本卡完成前预期失败——1 处命中）
- [x] `! rg -n "integration-scenarios" docs/development/account.md docs/development/gap/grit-gap.md docs/development/commands/_general.md tools/integration-runner/README.md | rg -v "docs/development/integration/integration-scenarios"`（bare/相对简写规范化断言：这四个 prose 文档中每一行 `integration-scenarios` 提及都必须携带完整新路径；本卡完成前预期失败——2026-07-04 实测 13 行缺口。已知局限：同一行同时含新全路径与残留 bare 简写时会被 `-v` 放过，故以上一条验收清单的逐点位核对为准）

**依赖**：Task 0.1。本卡必须先于 Task 0.4、Task 0.2 验证与一切 A/C 任务完成（0.3 未完成时 `compat_matrix_alignment`、`compat_agent_docs_contract` 乃至 `cargo test --all` 必失败）。

**预计触达文件**：

- `tests/compat/agent_docs_contract.rs`
- `tests/compat/matrix_alignment.rs`
- `tests/compat/README.md`
- `tests/INDEX.md`
- `COMPATIBILITY.md`
- `docs/development/commands/README.md`
- `docs/commands/package.md`、`Cargo.toml` 注释、`AGENTS.md`、`src/internal/db.rs` 注释
- `src/internal/db/migration.rs`（:610/:651 注释）
- `src/internal/ai/skills/embedded/libra.md`、`template/skills/libra.md`（嵌入/模板技能文档）
- `src/internal/ai/capability_package/{diff,mod,manifest}.rs`、`src/internal/ai/agent_run/{event_store,mod,workspace_strategy,event}.rs`、`src/internal/ai/runtime/{revision,phase0,phase4,event}.rs`（其中 `runtime/revision.rs:28`、`runtime/phase0.rs:10` 是失效 rustdoc 相对链接，重指为 `../../../../docs/development/tracing/agent.md`——4 级 `../` 才能从 `src/internal/ai/runtime/` 解析到仓库根，原文所记 5 级越出仓库根、任何解析器下仍是断链，2026-07-04 执行时实测修正）
- `tests/code_codex_default_tui_test.rs:1`、`tests/ai_subagent_flag_off_regression_test.rs:3`（文件头注释）
- `tests/ai_provider_transform_test.rs:281`（运行时读取旧根 code-agent-runtime.md，执行期发现）
- `tests/compat/matrix_alignment_support.rs`（integration-test-plan 运行时读取）
- `tools/integration-runner/src/{plan,manifest,cli,registry}.rs`、`tools/integration-runner/README.md`
- `docs/development/commands/_general.md`、`docs/development/account.md`、`docs/development/gap/grit-gap.md`（integration-test-plan / integration-scenarios live 引用）
- `docs/development/integration/integration-test-plan.md`、`docs/development/integration/integration-scenarios/integration-scenarios.yaml`（已迁移文件自身残留的旧根路径）
- `docs/development/integration/integration-scenarios/README.md`（:3 父级相对链接断链）

**规模**：S（2026-07-04 实测：agent/code 旧路径 29 处/约 25 文件 + integration-test-plan 旧路径 15 处/9 文件 + integration-scenarios 家族旧路径 21 处/10 文件，命中行与文件有重叠，全部为机械重指）。

### Task 0.4：事实源文档自洽清理（internal runtime / tracing drift guard）

**描述**：`docs/development/internal/code-agent-runtime.md` 是本文声明的内部 AgentRuntime / Web-only 事实源，但当前仍保留旧迁移前路径和失效相对链接。该文档在 A9/C1/C8 会被执行者读取；若它继续把 `docs/development/commands/agent.md`、`docs/development/code-agent-runtime.md` 或本目录外已删除的 `mcp.md` 当作 live 输入，后续实现会重新引入已修正的旧事实源。本卡只做 source-of-truth 链接、命令和漂移守卫清理，不改变 Agent/Code 设计目标。

**关联设计文档**：[`agent.md`](agent.md)、[`code.md`](code.md)、[`../internal/code-agent-runtime.md`](../internal/code-agent-runtime.md)。执行时以当前存在路径为事实源：外部 Agent 捕获公共契约在 `docs/development/tracing/agent.md`，Code 命令公共契约在 `docs/development/tracing/code.md`，跨命令规则在 `docs/development/commands/_general.md`。

**验收标准**：

- [x] `docs/development/internal/code-agent-runtime.md` 中所有 live Markdown 链接从 `commands/agent.md` / `docs/development/commands/agent.md` 改指 `../tracing/agent.md` 或 `docs/development/tracing/agent.md`；描述 `libra code` public surface 的 live 链接改指 `../tracing/code.md` 或 `docs/development/tracing/code.md`。（2026-07-04 实测：失效 href 均为相对形态——指向 `commands/agent.md` 的 11 处、`commands/_general.md` 4 处、`mcp.md` 13 处；全路径字符串只出现在链接文字/正文。另有 :2678 指向自身文件名的自链接虽可解析，但其链接文字声称已删除的根路径，须一并改写。）
- [x] `docs/development/internal/code-agent-runtime.md` 中所有 live Markdown 链接从 `commands/_general.md` 改指 `../commands/_general.md`；不得留下从 `internal/` 目录解析到不存在路径的相对链接。
- [x] `docs/development/internal/code-agent-runtime.md` 中的 `mcp.md` live 链接全部处理：若仅引用历史 MCP 拆分计划，改为不可点击历史说明；若引用当前可执行验证，改指 `docs/development/tracing/code.md` 的 C6 或现存的 `docs/development/integration/integration-scenarios/mcp.md`，并说明其只是 integration scenario，不是 MCP 事实源。
- [x] drift / rg / 验收命令里的旧路径同步改为当前路径，尤其是 `docs/development/code-agent-runtime.md`、`docs/development/commands/agent.md`、`commands/agent.md`、`mcp.md`。
- [x] 历史说明允许保留旧路径字符串，但必须在同句或相邻句标注 `旧`、`历史`、`已删除`、`不得恢复` 或 `d0a714` 等语境；不得作为可执行命令、依赖、链接或“source-of-truth”出现。
- [x] `docs/development/tracing/agent.md`、`docs/development/tracing/code.md`、本文的 cross-doc 描述与清理后的 internal 文档一致；若发现它们仍把旧路径当 live 输入，同 PR 修正。

**验证**：

- [x] `test ! -e docs/development/commands/agent.md && test ! -e docs/development/commands/code.md && test ! -e docs/development/code-agent-runtime.md && test ! -e docs/development/agent.md && test ! -e docs/development/web-only.md`
- [x] `! rg -n "\\]\\((commands/(agent|_general)\\.md|mcp\\.md|code-agent-runtime\\.md|\\.\\./agent\\.md|\\.\\./web-only\\.md|\\.\\./code-agent-runtime\\.md)\\)" docs/development/internal/code-agent-runtime.md docs/development/tracing/agent.md docs/development/tracing/code.md docs/development/tracing/plan.md`（Task 0.4 完成前预期失败——2026-07-04 实测 29 处命中，全部位于 code-agent-runtime.md，含 :2678 自链接）
- [x] `! rg -n "docs/development/commands/(agent|code)\\.md|docs/development/code-agent-runtime\\.md|docs/development/agent\\.md|docs/development/web-only\\.md" docs/development/internal/code-agent-runtime.md docs/development/tracing/agent.md docs/development/tracing/code.md docs/development/tracing/plan.md | rg -v "旧|历史|已删除|不得|d0a714|include_str|read_repo_file|Task 0\\.3|Task 0\\.4|改指|test ! -e"`（白名单含 `改指` / `test ! -e`：本计划 Task 0.3/0.4 验收文本中的「…改指…」句与上一条 `test ! -e` 验证命令、code-agent-runtime.md 内的 `test ! -e` 断言行都合法保留旧路径字符串；2026-07-04 实测原白名单下该断言永不可跑绿）
- [x] `rg -n "docs/development/tracing/(agent|code)\\.md|docs/development/internal/code-agent-runtime\\.md|docs/development/commands/_general\\.md" docs/development/internal/code-agent-runtime.md docs/development/tracing/agent.md docs/development/tracing/code.md docs/development/tracing/plan.md`

**依赖**：Task 0.1、0.3。本卡必须先于 Task 0.2 验证与 A1/C1 开工；0.4 未完成时，不得把 `docs/development/internal/code-agent-runtime.md` 当作干净事实源。

**预计触达文件**：

- `docs/development/internal/code-agent-runtime.md`
- `docs/development/tracing/agent.md`
- `docs/development/tracing/code.md`
- `docs/development/tracing/plan.md`

**规模**：S。

## 2. Agent 阶段总依赖图

```text
Global preflight:
Task 0.1 -> Task 0.3 -> Task 0.4 -> Task 0.2 -> AG-16

AG-16 capability/registry
  ├─ AG-17 CLI alias/list/add/remove
  ├─ AG-18 external RPC v2/security
  └─ AG-19 lifecycle dispatcher/hooks/redaction
       └─ AG-20 checkpoint/export/lazy IO/doctor/prune

AG-18 + AG-20 -> AG-21 transcript intelligence
AG-17 + AG-19 + AG-20 + AG-21 -> A6.5 local codex/claude/opencode capture smoke
A6.5 -> AG-22 read-only review workflow
A6.5 -> AG-23 read-only investigate workflow
AG-20 + AG-21 -> A8.5 合规实现（audit/--allow-raw/retention GC/erasure）
AG-16..AG-23 + A6.5 + A8.5/AG-24a -> AG-24 docs/tests/compat/release closeout -> Code 阶段
```

## 3. Agent 实施任务

### Task A1：AG-16 Observed Agent capability contract

**描述**：先冻结 capability、registry 和第一批 roster，使后续 CLI、RPC、hook、transcript、workflow 都从同一事实源派生。

**关联设计文档**：[`agent.md`](agent.md)。执行时遵循 AG-16、E1、E9 和第一批 supported roster 约束，不重新分析或扩大 Agent 支持范围。

**验收标准**：

- [x] 新增 `DeclaredAgentCaps` 8-bool wire contract：`hooks`、`transcript_analyzer`、`transcript_preparer`、`token_calculator`、`compact_transcript`、`text_generator`、`hook_response_writer`、`subagent_aware_extractor`。
- [x] `ObservedAgent` 增加 `as_*` capability accessors 与 `declared_capabilities()` 默认自省。
- [x] 删除或迁移 dead duplicate `ObservedAgentHooks`，hook 能力统一为 `as_hooks() -> Option<&dyn HookProvider>`。
- [x] 新增 `AgentRegistration` / registry matrix，字段覆盖 `slug`、`agent_kind`、`db_value`、`supported`、`support_wave`、`registered`、`transcript_readable`、`hook_installable`、`installed`、`launchable_review`、`launchable_investigate`、`external_binary`、`config_paths`、`capabilities`。
- [x] 第一批 supported roster 恰好是 `claude-code`、`codex`、`opencode`；`gemini`、`cursor`、`copilot`、`factory-ai` 等只能是 unsupported/quarantined/background。
- [x] `claude-code`、`codex`、`opencode` 三行都具备第一期本地采集所需字段：`supported=true`、`support_wave="first_batch"`、`transcript_readable=true`；HookProvider 落地后 `hook_installable=true` 且可进入本地采集 smoke。
- [x] 任务归属澄清（与 `agent.md` AG-16/AG-17 改动清单同口径）：A1/AG-16 只在 `src/internal/ai/observed_agents/{capability,registry}.rs` 冻结 registry/capability 事实源，静态 capability matrix 落 `registry.rs`；CLI 侧 roster 常量（`STABLE_AGENT_SLUGS`，`src/command/agent/mod.rs`）与 hook 安装面的收敛不在本卡，由 A2（AG-17）/A4（AG-19）承接。

**验证**：

- [x] `cargo test --test compat_agent_capability_matrix_pin`
- [x] `cargo test --test compat_agent_architecture_guard`
- [x] `cargo test --lib observed_agents`

**依赖**：Task 0.1、0.2、0.3、0.4（0.3 未完成时 `cargo test --all` 编译失败，任何任务的验收命令均不可执行；0.4 未完成时 source-of-truth 文档仍可能指向旧路径）。

**预计触达文件**：

- `src/internal/ai/observed_agents/adapter.rs`
- `src/internal/ai/observed_agents/capability.rs`
- `src/internal/ai/observed_agents/registry.rs`
- `src/internal/ai/observed_agents/mod.rs`
- `tests/compat/agent_capability_matrix_pin.rs`
- `tests/compat/agent_architecture_guard.rs`
- `Cargo.toml`
- `tests/INDEX.md`

**规模**：M。

### Task A2：AG-17 CLI alias parity

**描述**：把 `list/add/remove` 落成 `status/enable/disable` 的严格 alias，同时保持旧入口 canonical，不扩大到内部 AgentRuntime 管理。

**关联设计文档**：[`agent.md`](agent.md)。执行时遵循 AG-17 的 CLI alias parity、capability matrix 和 unsupported/quarantine 约束。

**验收标准**：

- [x] `libra agent list` 输出 focused capability matrix，`--json` 带 schema/version 字段。
- [x] `libra agent add <name>` 与 `enable --agent <name>` 同语义、同退出码、同 JSON shape。
- [x] `libra agent remove <name>` 与 `disable --agent <name>` 同语义、同退出码、同 JSON shape。
- [x] 无参 `add/remove` 的行为与无参 `enable/disable` 一致，并只作用于 supported/installable roster。
- [x] 非首批 agent 返回 actionable unsupported，不写 hook、不创建 session/checkpoint、不把 `installed` 标为 true。
- [x] 曾 supported 的非首批 agent（如 `gemini`，现为 `STABLE_AGENT_SLUGS` 成员且 hooks 可安装）保留 **uninstall-only 通道**：`remove <name>` / `disable --agent <name>` 可卸载已安装的 Libra-managed hooks、幂等、卸载后不允许再 add/enable；仅 add/enable 返回 actionable unsupported。
- [x] remove/disable 卸载语义按 `agent.md` 既有锚点验收：provider 配置文件中仅 Libra-managed entry 被移除（codex 另按 `agent.md` 处理 Libra 写入的 features/`[hooks.state]` 项）、用户条目语义等价保留、已捕获的 agent_session/agent_checkpoint/refs/libra/traces 数据不删除；卸载后 `agent list --json` 显示 `installed=false` 且 `hook_installable=true`；对未安装状态重复 remove 幂等（exit 0 或明确提示，不报错误栈）。

**验证**：

- [x] `cargo test --test command_test agent_list_add_remove_aliases_parse`
- [x] `cargo test --test command_test agent_list_json_contains_capability_fields`
- [x] `cargo test --test command_test agent_add_non_hook_installable_returns_actionable_unsupported`
- [x] `cargo test --test command_test agent_remove_gemini_uninstalls_legacy_hooks_idempotent`
- [x] `cargo test --test command_test agent_remove_preserves_user_hook_entries`

**依赖**：Task A1。

**预计触达文件**：

- `src/command/agent/mod.rs`
- `src/command/agent/status.rs`
- `src/command/agent/doctor.rs`
- `docs/commands/agent.md`
- `docs/commands/zh-CN/agent.md`（现存，必须同步）
- `COMPATIBILITY.md`
- `tests/command/*`

**规模**：M。

### Task A3：AG-18 External `libra-agent-<name>` protocol v2 and security

**描述**：在现有 JSON-RPC v1 之上增加 v2 `info`、protocol version、capability gate、trust/quarantine/provenance 与 spawn 安全。

**关联设计文档**：[`agent.md`](agent.md)。执行时遵循 AG-18、E2、E10、settings gate、provenance、env/stderr redaction 和内置 slug 仿冒防护约束。

**验收标准**：

- [x] v2 `info` 响应包含 `protocol_version`、`name`、`type`、`description`、`is_preview`、`protected_dirs`、`protected_files`、`hook_names`、E1 8-bool `capabilities`。
- [x] v1 `capabilities` method 至少保留一个 release window；client 协商顺序为 `info` -> v1 `capabilities` -> skip-and-log。
- [x] external discovery 默认 disabled；未知 binary quarantine，不注册为 callable agent。
- [x] `rpc trust/untrust` 记录 path、sha256、device、inode、mtime；hash/inode 变化重新 quarantine。
- [x] 内置 slug 仿冒（如 `libra-agent-claude-code`）skip-and-log，不能覆盖 built-in agent。
- [x] `RpcAgent::spawn` 使用 `env_clear()` + allowlist，只注入 `LIBRA_AGENT_PROTOCOL_VERSION`、`LIBRA_CLI_VERSION`、`LIBRA_REPO_ROOT` 等安全变量。
- [x] spawn 前按 `agent.md` 强制补强项 #2 / 威胁 T9 做 provenance revalidation（支持平台优先 fd 派生 exec，如 `fexecve`/`execveat`；不支持时受信目录 + canonical path + 父目录非 world-writable 校验 + sha256/device/inode/mtime 复验 + absolute-path spawn），PR 中记录所选平台策略为 best-effort TOCTOU mitigation、不声称完全消除竞态；若按 AG-18 DoD 延期，须在本卡显式写明延期原因与重启条件。**（2026-07-04 落地记录：实现为 fallback 层——canonical path + 父目录非 world-writable 校验 + sha256/device/inode/mtime 复验 + absolute-path spawn + 漂移即撤销信任；fd 派生 exec（fexecve/execveat）显式延期——Rust std::process::Command 无可移植 fd-spawn 面，重启条件为引入 nix/命令层 fd-exec 封装或 std 提供该能力。）**
- [x] 按 `agent.md` 落地执行补充规格 §6 落地 `agent.rpc.discover` / `agent.rpc.invoke` span/metric（必带字段存在、禁止字段缺席），用 tracing fake sink 断言。
- [x] stderr 捕获、64 KiB cap、redaction；默认输出和 JSON 错误不得泄露 raw stderr、token、prompt、路径片段。
- [x] timeout、stdout/stderr IO cap、protocol mismatch、undeclared method 都映射到 E10 稳定错误码。**（2026-07-05 落地记录：IO hard-cap → 007；timeout/broken-pipe/malformed-frame → 新分配 012 `ERR_AGENT_RPC_TRANSPORT_FAILED`（codex R1 指出 007 语义不覆盖 timeout）；协商版本不符 → 003；undeclared method → 004；映射由 `command::agent::rpc` pin 单测逐项钉住。）**
- [x] E10 错误码先使用语义键而非预占 `LBR-*` 字面量：`LBR-AGENT-001` 已被 `AgentBudgetExceeded` 占用（`src/utils/error.rs` + `docs/error-codes.md` 已发布并有 pin 测试）。contract slice 必须先在 `docs/error-codes.md` 为 E10 各语义分配未占用的真实编号，并同 PR 修正 `agent.md` E10 表；注意 `compat_error_codes_doc_sync` 只校验码在文档出现、不校验唯一性，需人工核对（或为该守卫补"同码不得映射多个 variant"断言）。计划内其它任务卡只允许引用 `ERR_AGENT_FIX_BRIDGE_UNAVAILABLE`、`ERR_AGENT_UNTRUSTED_SEED_FOR_MUTATION` 等语义键，真实编号以本项核对后的实际分配为准。

**验证**：

- [x] `cargo test --test agent_rpc_external_test`
- [x] `cargo test --test compat_error_codes_doc_sync`
- [x] fake binary fixture 覆盖 protocol mismatch、timeout、oversize、stderr flood、env echo、PATH conflict。

**依赖**：Task A1；可与 A2 并行，但合并前需 rebase 到同一 registry。

**预计触达文件**：

- `src/internal/ai/observed_agents/rpc.rs`
- `src/command/agent/rpc.rs`
- `src/utils/error.rs`
- `docs/error-codes.md`
- `tests/fixtures/agent_rpc/*`
- `tests/agent_rpc_external_test.rs`
- `Cargo.toml`
- `tests/INDEX.md`

**规模**：M。

### Task A4：AG-19 normalized lifecycle dispatcher and hook providers

**描述**：把 provider hook/parser 输出统一为 provider-neutral `LifecycleEvent`，由 central dispatcher 做校验、owner filtering、redaction 和写入。

**关联设计文档**：[`agent.md`](agent.md)。执行时遵循 AG-19、E3、HookProvider、owner filtering、first-batch hook install 和 redaction-before-persist 约束。

**验收标准**：

- [x] `LifecycleEventKind` 增加 `SubagentStart` / `SubagentEnd`，并用 `#[non_exhaustive]` 或兜底 match 防新增变体 panic。**（末尾追加保 `event_id()` 序数稳定，另加序数 pin 测试；dispatcher 对未识别 event name skip-and-log `unknown_event_type`，经 `HookProvider::recognizes_event` 判定。）**
- [x] 删除 runtime 中 provider-name 字符串桥，改为 `AgentKind` -> registry -> `as_hooks()`。**（`find_provider`/`provider_name_for`/`SUPPORTED_PROVIDER_NAMES` 全删；gemini 仅卸载通道改为类型化单例直引；§775 rg 断言零命中。）**
- [x] Claude Code 保持 installable；Codex/OpenCode HookProvider 落地后才允许 `hook_installable=true`。
- [x] Agent 第一期必须让 Claude Code、Codex、OpenCode 三个 HookProvider 全部可安装；Codex/OpenCode 不得停留在 transcript-readable-only 状态后仍宣称第一期完成。**（registry 行翻转 + `compat_agent_capability_matrix_pin` 同 PR 更新。）**
- [x] Codex HookProvider 显式处理上游 trust/enable 双重门控（用户级 `[hooks.state]`，见 §0.3.3）：installer 写入自有 trusted_hash + enabled entry（只增不删，卸载时清理），或运行侧显式 bypass 并登记豁免理由；实测确认 enabled 语义与临时仓库 repo-trust 状态不阻断项目层配置加载，所选策略写入 evidence 与 `agent.md`。**（2026-07-05 实测 codex-cli 0.142.4 + 源码 rust-v0.142.4 逐字节核对：trusted_hash 算法外部复现命中；project 层 `.codex/hooks.json` 仅对用户 config 受信项目加载且 bypass 不解锁 → 采用用户级 `$CODEX_HOME/hooks.json` + 自算 `[hooks.state]` 方案，完全非交互、无需 bypass；repo-trust 不影响用户层加载。evidence 已回写 agent.md「Codex 捕获目标契约」。）**
- [x] OpenCode HookProvider 有事件映射规格：随实现 PR 依上游实测（opencode 1.17.13，2026-07-05）固定为 session.created→SessionStart、message.updated(role=user)→TurnStart、tool.execute.after→ToolUse、session.idle→TurnEnd（**Libra 侧推断规则**，headless turn 完成信号）、session.deleted→SessionEnd、session.compacted→Compaction；`agent.md`「OpenCode 安装流程契约」已同 PR 重写（hooks.json 假想示例已替换为实测 plugin 契约）；uninstall 仅删除携带标记的 Libra plugin 文件（plugin/ 与 plugins/ 双目录检查）。
- [x] Gemini 等非首批 agent 不再作为 supported/installable 暴露。
- [x] 明确隐藏 hook 运行时入口（`src/command/hooks.rs` 的 gemini 分组，存量已安装 hooks 会持续调用）的去留：保留为 ingest-reject-with-hint 或移除，与 A2 的 gemini uninstall-only 降级语义一致。**（选定 ingest-reject-with-hint：CLI 面保留、拒绝并提示 `libra agent remove gemini`；同入口新增 codex 分组作为 hook 文件稳定调用面，路由 AgentTraces。）**
- [x] provider parser 只返回 `LifecycleEvent`，不得直接写 checkpoint。（四个 provider 一致；写入集中在 `runtime.rs` dispatcher。）
- [x] central validation 覆盖 session id、provider session id、tool/subagent id、cwd 可信、path traversal、unknown agent kind quarantine。（session id 字母表/长度 + transcript path 约束 + `transcript_path_within_provider_root` 越界防御 + registry 未知 slug fail-closed 均在位；unknown event name skip-and-log 为本卡新增。）
- [x] owner filtering 防止多 adapter 重复 checkpoint；重复或 owner 不匹配的事件 skip/fail closed。**（first-writer-wins：按 provider_session_id 最早 started_at 行的 agent_kind 判属主；SessionStart/TurnStart 豁免；非属主事件 skip-and-log `owner_mismatch`，E2E 钉住 claude↔gemini 双 provider 转发场景。）**
- [x] redaction-before-persist 类型级接入，raw hook input 不进入 session log、checkpoint tree、stdout/JSON。**（redaction 扩展到 prompt/assistant_message/tool_input/tool_response 四字段；E2E 断言 AKIA 令牌不出现在任何 CLI JSON 输出与 DB 字节中。）**
- [x] 安装器写入的每个 Libra-managed hook entry command 必须是 `resolve_hook_binary_path` 产出的 canonicalize 绝对路径（`--binary-path` 或 `current_exe`，解析失败硬错误，禁止回退裸 `libra`），安装后有断言（覆盖非 smoke 常规安装路径，与 §0.3.3/A6.5 口径一致）。**（`tests/agent_enable_install_path_test.rs`：opencode plugin 与 codex hooks.json 双向 E2E，含 trust-gap banner 触发/修复与卸载还原；注意「先 enable 修复 trust 再 disable」的顺序约束——`hooks_are_installed` 对 trust gap fail-closed，篡改态下 disable 为 no-op。）**
- [x] 按 `agent.md` 补充规格 §6 落地 `agent.hook.ingest`、`agent.redaction.apply` span/metric，tracing fake sink 断言必带/禁止字段。**（`tests/agent_hook_span_test.rs` 独立二进制 3 用例：必带字段/partial+unknown_event_type/validated=false，raw prompt 与 AKIA 令牌不得进 sink；`ingest_agent_traces_payload` 为此升为 pub（注明非稳定 API、不从 crate root 再导出）。）**
- [x] 按 `agent.md` 强制补强项 #10 落地 `libra hooks <provider> <verb>` 崩溃行为回归（panic 与被 kill 两路径）：断言无半截 checkpoint/DB 写入、退出码非零、stderr 不回显 raw stdin。**（`tests/agent_hook_crash_test.rs` 3 用例：SIGKILL 半途/panic 注入（`LIBRA_TEST_HOOK_PANIC_AFTER_READ` 测试旋钮，置于读取+校验后、任何 DB 写入前）/stop 竞态 kill 五连发后 checkpoint 行完整性不变式。）**

**验证**：

- [x] `cargo test --test agent_lifecycle_event_test`（5 用例）
- [x] crash 回归落点为独立 target：`cargo test --test agent_hook_crash_test`（3 用例，fn 名 `hook_handler_killed_mid_ingest_leaves_no_partial_write` 等）
- [x] `cargo test --test agent_checkpoint_redaction_test`（2 用例）
- [x] `cargo test --test command_test agent`
- [x] `rg -n "provider_name_for|find_provider\\(|ObservedAgentHooks" src/internal/ai src/command/agent` 不再出现独立字符串事实源（零命中）。
- [x] 追加 target：`agent_hook_span_test`（3）、`agent_enable_install_path_test`（2），均入 `tests/INDEX.md`。

**依赖**：Task A1；A3 可并行，涉及 external parser 时需合并。

**预计触达文件**：

- `src/internal/ai/hooks/lifecycle.rs`
- `src/internal/ai/hooks/runtime.rs`
- `src/internal/ai/hooks/provider.rs`
- `src/internal/ai/hooks/providers/mod.rs`
- `src/internal/ai/hooks/providers/codex/*`
- `src/internal/ai/hooks/providers/opencode/*`
- `src/internal/ai/observed_agents/redaction.rs`
- `src/command/hooks.rs`
- `tests/fixtures/agent_hooks/*`
- `tests/agent_lifecycle_event_test.rs`
- `tests/agent_checkpoint_redaction_test.rs`
- `tests/INDEX.md`

**规模**：M。

### Task A5：AG-20 E4-libra checkpoint export, lazy transcript IO, doctor and prune safety

**描述**：把 external-agent checkpoint/export writer 固定到 E4-libra layout，同时补默认 metadata-first 读取、E4-entire legacy import、crash recovery、doctor repair 和 prune 并发保护。

**关联设计文档**：[`agent.md`](agent.md)。执行时遵循 AG-20、E4-libra、E4-entire reader、E5、doctor repair、object_index 和 prune A/B 窗口约束。

**验收标准**：

- [x] Writer 输出 E4-libra tree：`metadata.json`、`manifest.json`、`events/lifecycle.jsonl`、`transcript/<agent_kind>.jsonl`、`redaction_report.json`、`content_hash.txt`。**（metadata schema v2 增 `model`（缺省 "unknown"）；E3 canonical JSONL 序列化器支持多事件批；manifest 自描述 content_hash coverage。）**
- [x] `content_hash.txt` writer 使用 `sha256:<64-hex>`；reader 兼容 legacy bare hex。**（覆盖域固定为 metadata,lifecycle_events,transcript,redaction_report 按序拼接、无尾换行、transcript 取逻辑重组流（分块不变量）；定义已回写 agent.md E4-libra 节；`history::parse_content_hash`。）**
- [x] E4-entire fixture 只作为 import/legacy reader，不作为 writer 默认。
- [x] 大 transcript 走 E5 line-safe chunking，manifest-relative 路径；默认 `list/show` 不读取 transcript body。**（50MiB 阈值 + `LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD` 测试旋钮、`.001` 起始后缀、单行超限硬错误；show 对 transcript 只做 loose-object stat，不加载 body——缺 blob 时 show 仍成功并标 missing。）**
- [x] `session list`、`checkpoint list` 默认 `--limit 50`，cap 500，使用 keyset cursor 和索引。**（cursor=base64 "v1:<ts>:<id>" 不透明串；排序按新索引形态 `(ts DESC, id ASC)`（(DESC,DESC) 实测触发 TEMP B-TREE，已按 EXPLAIN 证据修正任务文本假定）；JSON 加 `schema_version`+`next_cursor` 信封；>500 clamp 带 stderr 提示。）**
- [x] `agent_checkpoint` INSERT 改为幂等 UPSERT 或先探测 `traces_commit`，崩溃重试不主键冲突。**（build→CAS→probe(`agent_checkpoint_id_for_traces_commit`)→INSERT + `ON CONFLICT(checkpoint_id) DO NOTHING` 兜底，抽为 pub `insert_agent_checkpoint_row_idempotent` 供 doctor class-2 复用。）**
- [x] doctor 检测并修复三类不一致：DB 指向缺失对象、ref 可达但无 catalog 行、`object_index` 缺失。**（`--repair` 净新建：class-1 ref 可达则按 ref 重建行、真缺失标 manual_required 零破坏；class-2 first-parent 走链 + metadata v1/v2 双形态解析 + in-flight marker 豁免 + 幂等插入；class-3 幂等直插镜像 `update_object_index_once` 语义；全部二跑无操作。）**
- [x] prune 窗口 A/B 关闭：writer lease、临时保护 ref 或 ref-vs-catalog fail-closed 必须覆盖到 DB INSERT/UPSERT 完成。**（writer 侧 metadata_kv in-flight marker（TTL 10min）覆盖 stage (a)→(d)；prune 侧任一 live marker 全局 fail-closed（整链重写语义下按 session 放行不安全，已注记）+ ref-vs-catalog 比对失败拒绝并提示 doctor --repair；CAS 重试逐次复查。）**
- [x] `CheckpointCommit.commit_hash` 与 DB 列 `traces_commit` 命名不混用。
- [x] reader（metadata-first list/show/detail/transcript）与 doctor 兼容识别**升级前 Libra v1 布局的存量 checkpoint**（无 manifest.json/redaction_report.json/content_hash.txt，事件文件为 `events/<provider>.jsonl`、transcript 为 `transcript/<provider>`）：doctor 将其归类为 legacy-v1 而非三类不一致，不得误报缺 manifest 或触发 repair 改写；v1 checkpoint 的 show/transcript 走无 manifest 的 fallback 解析。`tests/fixtures/agent_checkpoints/` 必须包含一个由当前（改造前）writer 生成的 v1 布局 fixture 供回归——本卡第一步须在改动任何 writer 代码前先生成并提交该 fixture。`agent_kind=gemini` 的存量 session/checkpoint 行继续可读（read-only），doctor 对残留 gemini hooks 配置给出指向卸载通道的 actionable 提示。
- [x] 提供 v1→E4-libra 的 migration notes 与 schema pin test；reader fallback 为主路径，backfill 不做——v1 checkpoint 长期以 legacy 形态可读（fixture + README 溯源清单 + show/doctor 分类测试即 pin；prune 重建对 v1 inner tree 字节级保持）。
- [x] **选定方案 (a)**（force-with-lease 等价，无新错误码）——定义并测试 prune/rewrite 之后 `libra agent push` 的行为：`refs/libra/traces` 是 Libra 托管、prune 即整链重写的 ref，二选一——(a) agent push 对该 ref 采用 force-with-lease 等价语义（lease 基于 tracking ref，复用 push 既有机制，无需新错误码）；或 (b) 非快进时报 `ERR_AGENT_TRACES_PUSH_DIVERGED` 对应的稳定错误码（语义键须同 PR 补入/对齐 `agent.md` E10 表已预留的条件行；真实 `LBR-*` 编号沿用 A3 建立的分配规则，在本卡自己的 slice 内于 `docs/error-codes.md` 分配，无需回到 A3）+ actionable 指引，并提供重推出口（当前 `PushArgs` 仅有 `--remote`，需新增 force 类参数）；配套回归测试覆盖 clean/prune 重写后 push 到本地 file remote 的场景。
- [x]（2026070802_agent_checkpoint_paging，注意编号须超过既存 max 2026070801 而非按日历日；traces_commit 索引取非唯一以免历史重复行 brick 存量库，幂等由代码探测保证）新增迁移遵循 `sql/migrations/README.md` 规范：`YYYYMMDDNN` 命名（版本号严格递增）、forward DDL 幂等（`IF NOT EXISTS`）、配套 `_down.sql`（仓库现存迁移全部成对）、在 `builtin_migrations()` 注册，并补 run_pending 幂等 + 升级→回滚→再升级 round-trip 测试。
- [x] 按 `agent.md` 补充规格 §6 落地 `agent.checkpoint.write`、`agent.clean.prune`、`agent.doctor.repair` span/metric，tracing fake sink 断言必带/禁止字段。**（write：独立二进制 fake-sink；prune：`agent_clean_span_test`；doctor：经 CLI 自身 tracing 栈（LIBRA_LOG_FILE）断言字段与 transcript 禁带。）**
- [x] 接线孤儿测试模块：`tests/command/agent_checkpoint_test.rs` 已在 `tests/command/mod.rs` 声明并实跑（`-- --list` 列出行为用例 `agent_checkpoint_rewind_dry_run_and_apply_restore_worktree_only`，非仅 help 措辞测试；用例未改动即通过——E4-libra writer 未破坏其 DB 播种断言）。

**验证**：

- [x] `cargo test --test agent_checkpoint_export_test`（10 用例）
- [x] `cargo test --test command_test agent_checkpoint_rewind`
- [x] `cargo test --test command_test agent_checkpoint_rewind -- --list` 列出行为用例（接线证据见上）。
- [x] `EXPLAIN QUERY PLAN` 断言命中分页索引（unit 层 SQL 构造器 + E2E 真库双层，均断言无 TEMP B-TREE/SCAN）。
- [x] `cargo test --test db_migration_test`（21→22 pin + round-trip）
- [x] 追加 target：`agent_checkpoint_reader_test`（8）、`agent_doctor_repair_test`（8）、`agent_checkpoint_span_test`（1）、`agent_clean_span_test`（1），均入 `tests/INDEX.md`。

**依赖**：Task A1、A4。

**预计触达文件**：

- `src/internal/ai/history.rs`
- `src/internal/ai/hooks/runtime.rs`
- `src/command/agent/checkpoint.rs`
- `src/command/agent/session.rs`
- `src/command/agent/doctor.rs`
- `src/command/agent/clean.rs`
- `src/command/agent/push.rs`
- `sql/migrations/*`
- `tests/fixtures/agent_checkpoints/*`
- `tests/agent_checkpoint_export_test.rs`
- `tests/command/agent_checkpoint_test.rs`
- `tests/command/mod.rs`

**规模**：M。

### Task A6：AG-21 transcript intelligence and skill-event extraction

**描述**：在首批 provider 上实现 transcript/prompt/token/model/subagent/skill 提取，缺可选字段时 fail-open 标 partial，但安全/写入路径继续 fail-closed。

**关联设计文档**：[`agent.md`](agent.md)。执行时遵循 AG-21、E5、E6、E7、partial extractor 和安全路径 fail-closed 约束。

**验收标准**：

- [x] Claude Code、Codex、OpenCode adapter 按能力实现 `TranscriptAnalyzer`、`PromptExtractor`、`TranscriptPreparer`、`TokenCalculator`、`ModelExtractor`、`SubagentAwareExtractor`、`SkillEventExtractor` 中适用项。
- [x] E6 token usage key 显式映射到 Libra `CompletionUsageSummary`。
- [x] E7 `SkillEvent` 投影有 curated registry；上游无能力时写 empty/partial 并解释。
- [x] extractor 失败不阻断 checkpoint 保存，但 metadata 必须标 `partial` 并记录 redacted warning。
- [x] path validation、redaction、rewind apply、hook install/uninstall、external launch/fix 仍 fail-closed。
- [x] `tests/fixtures/agent_transcripts/*`（并同步 A4 的 `tests/fixtures/agent_hooks/*`）每组 fixture 附带溯源 manifest：来源 agent slug、CLI 版本、采集日期、采集/构造方式（真实采集后 redact 或手工构造）；harness 在 fixture 解析断言失败时的消息引用该 manifest 条目，便于区分实现回归与上游格式漂移。

**验证**：

- [x] `cargo test --test agent_transcript_intelligence_test`
- [x] `cargo test --test agent_checkpoint_redaction_test extractor_warning_does_not_include_secret_owner_or_prompt`

**依赖**：Task A1、A3、A5（A4 经 A5 传递依赖，与 §2 依赖图一致）。

**预计触达文件**：

- `src/internal/ai/observed_agents/capability.rs`
- `src/internal/ai/observed_agents/builtin/claude_code.rs`
- `src/internal/ai/observed_agents/builtin/stable_promoted.rs`
- `src/internal/ai/hooks/providers/codex/*`
- `src/internal/ai/hooks/providers/opencode/*`
- `tests/fixtures/agent_transcripts/*`
- `tests/agent_transcript_intelligence_test.rs`

**规模**：M。

### Task A6.5：Agent 第一期本地三 Agent 采集 smoke

**描述**：用本机真实 `codex`、`claude`、`opencode` 验证 Agent 第一期采集闭环。该任务是第一期完成门禁：deterministic fixtures 必须继续存在，但不能替代真实本地 agent 的端到端采集证据。

**关联设计文档**：[`agent.md`](agent.md)。执行时遵循第一批 supported roster、hook install、session/checkpoint/traces、metadata-first 与 redaction 约束。

**验收标准**：

- [x] Preflight 记录 `command -v codex`、`codex --version`、`command -v claude`、`claude --version`、`command -v opencode`、`opencode --version`；若某命令不支持 `--version`，记录等价只读版本/状态命令。输出不得包含 token 或账户 secret。
- [x] `agent_local_capture_smoke_test` 按 §0.3 固化真实 CLI 调用矩阵：Codex 用 `codex exec`，Claude Code 用 `claude -p`，OpenCode 用 `opencode run`；每个 agent 使用独立临时 repo、独立 evidence 目录、串行执行和 child-process timeout。
- [x] 在临时 Libra 仓库中分别执行 `libra agent add claude-code`、`libra agent add codex`、`libra agent add opencode`；三者都必须安装 Libra-managed hooks，且保留用户已有 provider 配置。
- [x] 分别启动本地 `claude`、`codex`、`opencode` 的最小非破坏性会话，触发至少一个 `SessionStart`、一个 turn boundary、一个 `SessionEnd` 或等价 stop event；如果某 agent 的 CLI 无稳定非交互模式，测试 harness 必须明确记录手动/PTY 驱动步骤，并把该 agent 标为需要本地 smoke 而非 CI-only。
- [x] `libra agent list --json` 对三者均显示 `supported=true`、`support_wave="first_batch"`、`registered=true`、`transcript_readable=true`、`hook_installable=true`、`installed=true`。
- [x] `libra agent session list --json` 能看到 `agent_kind` 分别为 `claude_code`、`codex`、`opencode` 的 session；`libra agent checkpoint list --json` 至少各有一个对应 checkpoint。
- [x] `libra agent checkpoint show <id> --json` 默认只展示 metadata/redaction/content hash/token summary，不读取或打印完整 transcript；显式 detail/transcript 路径另测。
- [x] `libra agent doctor --json` 对三条本地采集链路不报告 missing hook、missing object、missing catalog row 或 redaction failure。
- [x] 三条 smoke 的产物都进入 `refs/libra/traces`，并能由 `checkpoint show` / `session show` 通过 OID 指针读取 metadata。
- [x] Preflight 完成 §0.3.2 登录态只读检查（`codex login status` / `claude auth status` / `opencode providers list`），evidence 只保留 redacted 布尔判定与退出码；任一未登录即按 §0 规则标记 blocked，不发起真实付费会话。
- [x] Preflight 记录 §0.3.2 pinned `$LIBRA_BIN` 副本的 sha256，安装断言确认各 hook entry command 以该 pinned 绝对路径开头（与 A3 的 provenance 口径对齐）。
- [x] 采集断言完成后执行 §0.3.5 卸载 smoke：`agent remove <slug>` 后 provider 配置回到安装前语义状态（与 `preinstall.snapshot` 对比）、`installed=false`、二次 remove 幂等；已捕获数据不删除。
- [x] 本机 agent CLI 版本高于 fixtures manifest 记录版本且 smoke 观察到 transcript/hook 格式差异时，在 evidence summary 写明差异并决定是否刷新 fixture；刷新按 §0.3.4 重新采集，提交前完成 redaction/最小化。
- [x] 缺少任一本地 agent、登录态或 HookProvider 时，该任务为 blocked；不得用 fake fixture 或单 agent 通过替代。

**验证**：

- [x] `LIBRA_RUN_LOCAL_AGENTS=1 LIBRA_LOCAL_AGENT_SET=codex,claude-code,opencode cargo test --test agent_local_capture_smoke_test -- --ignored --test-threads=1`
- [x] 排障时可加 `LIBRA_KEEP_LOCAL_AGENT_SMOKE=1 LIBRA_LOCAL_AGENT_TIMEOUT_SECS=180` 保留 evidence；保留目录视为敏感，不提交。
- [x] `libra agent list --json`
- [x] `libra agent session list --json`
- [x] `libra agent checkpoint list --json`
- [x] `libra agent doctor --json`

**依赖**：Task A1、A2（add/list alias 验收所需）、A4、A5、A6。

**预计触达文件**：

- `tests/agent_local_capture_smoke_test.rs`
- `tests/harness/agent_local_capture.rs`
- `tests/fixtures/agent_hooks/*`
- `Cargo.toml`
- `tests/INDEX.md`
- `docs/commands/agent.md`
- `docs/development/tracing/agent.md`

**规模**：M。

### Task A7：AG-22 read-only agent review workflow

**描述**：先交付 read-only review：外部 reviewer 进程 fan-in、bounded sink、findings manifest、manual attach/provenance。`--fix` 在内部 fix bridge 未就绪时稳定 unsupported。

**关联设计文档**：[`agent.md`](agent.md)、[`code.md`](code.md)。执行时遵循 AG-22/E8 read-only review 约束；任何 `--fix`/mutation 只按 `code.md` 中内部 AgentRuntime、approval、sandbox、tool gate 约束桥接。

**验收标准**：

- [x] 新增 `libra review --agent <slug>... [--since <rev>] [--checkpoint <id>] [--json]`。
- [x] 新增 `libra review list [--json]`、`libra review show <run_id>`、`libra review cancel <run_id>`、`libra review clean`。
- [x] `review list` 作为 run 枚举入口，走统一分页契约：默认 `--limit 50`、cap 500、keyset cursor page envelope，`--json` 带 schema/version 字段（与 `agent.md` 强制补强项 #5 一致），有分页测试。
- [x] `review cancel <run_id>` 触发 `cancelled` terminal state 并执行完整资源释放；前台阻塞 run 的 SIGINT/SIGTERM 等价于 cancel，两条路径共用同一 cleanup。
- [x] run state 写入 `.libra/sessions/agent-runs/<run_id>/`，包含 `state.json`、`findings.md`、`manifest.json`、redacted reviewer logs。
- [x] reviewer 并发 fan-in 到串行 sink；单 reviewer 高频输出不阻塞其它 reviewer，per-sink 缓冲有上限。
- [x] 所有 run 都有 terminal state：`success`、`error`、`cancelled`、`timeout`、`partial`。
- [x] cancel/timeout 释放 external process、reader task、locks、workspace lease。
- [x] 非首批 agent 不能进入 launchable path；只能 unsupported 或 manual attach fallback。第一期 manual attach 仅为 E8-libra manifest 的 `manual_attach` 占位字段（默认空/false），**不提供命令面**；若要实现 attach 命令入口，必须先在 `agent.md` 补充规格 §5 补规格再动工。
- [x] reviewer 一律在**隔离 workspace** 中运行：将 `materialize_isolated_workspace`（`sub_agent_dispatcher.rs`）抽取为 public seam 并作为必选路径；隔离物化遵循 ignore 规则排除（`.env.test` 在 `.gitignore`，copy 后端已有此语义），若使用 FUSE overlay 后端须补测试证明 ignored 文件同样不暴露；in-place 运行只允许显式危险 flag 且默认拒绝。
- [x] 首批三 agent 的 reviewer spawn 固定为最小权限只读形态（当前具体形态见 §0.3.2 表：codex `--sandbox read-only`、claude `--permission-mode plan`、opencode 非危险模式；CLI 版本变化时按 §0.3.2 约定复核），测试断言实际 spawn 参数与工作目录指向隔离 workspace 而非仓库根。残余风险声明：read-only sandbox 不阻断 reviewer 自身网络能力，secret 不外泄的第一道防线是隔离 workspace 不含 secret 文件 + env 只注入 allowlist，redaction 仅是落盘兜底。
- [x] reviewer findings 与被引用的 transcript/checkpoint 摘要在注入任何后续 agent prompt 或汇总前，与 seed 同等对待：标注 provenance=untrusted、走与 seed 相同的 redaction 管线、prompt 中以明确定界（spotlighting）区分于指令；`review show` 渲染 findings.md 前剥离 ANSI/终端控制序列（reviewer stdout 自由文本可注入转义序列伪造终端输出）。
- [x] 新顶层命令与 `cli.rs` 注册**同 PR 原子**满足 compat 接线：`COMPATIBILITY.md` 顶层矩阵行（intentionally-different / Libra-only extension，`compat_matrix_alignment` 双向严格比对）、`src/cli.rs` ROOT_AFTER_HELP "AI And Automation" 组行、`docs/development/commands/review.md` + README 表行、`pub const REVIEW_EXAMPLES` + `after_help` 接线并把命令名补入 `tests/compat/help_examples_banner.rs` VISIBLE_COMMANDS、`docs/commands/review.md` 含 Examples 节并同步 zh-CN 页。无论实现文件放 `src/command/` 还是 `src/command/agent/`，CLI 面均为顶层命令，上述契约一律适用。
- [x] `review --fix`：若没有内部 serialized fix bridge 源码锚点和 approval/sandbox/tool gate 测试，返回 `ERR_AGENT_FIX_BRIDGE_UNAVAILABLE` 对应的稳定错误码（真实 `LBR-*` 编号由 A3 在 `docs/error-codes.md` 分配），不得假成功。该错误语义由本卡首先落地，A8 复用。
- [x] 按 `agent.md` 补充规格 §6 落地 `agent.review.run` span/metric，tracing fake sink 断言必带/禁止字段。

**验证**：

- [x] `cargo test --test agent_review_workflow_test`
- [x] `cargo test --test compat_error_codes_doc_sync`
- [x] `cargo test --test compat_matrix_alignment`
- [x] `cargo test --test compat_help_examples_banner`
- [x] `cargo test --test compat_command_docs_examples_section`
- [x] `cargo test --lib cli::tests::root_after_help_lists_every_visible_command`
- [x] stress test 覆盖 slow reviewer、stderr flood、cancel during pending output。

**依赖**：read-only 依赖 A1、A3、A6、A6.5；fix path 另依赖 Code 阶段的内部 bridge 证据。

**预计触达文件**：

- `src/cli.rs`
- `src/command/mod.rs`
- `src/command/review.rs` 或 `src/command/agent/review.rs`（实现时择一并同步文档；CLI 面固定为顶层命令，不影响 compat 契约适用）
- `src/internal/ai/observed_agents/rpc.rs`
- `src/internal/ai/agent/runtime/sub_agent_dispatcher.rs`（抽取 `materialize_isolated_workspace` 为 public seam，必选）
- `src/utils/error.rs`
- `docs/error-codes.md`
- `COMPATIBILITY.md`
- `docs/commands/review.md`
- `docs/commands/zh-CN/review.md`
- `docs/development/commands/review.md`（及 README.md 表行）
- `tests/compat/help_examples_banner.rs`
- `tests/INDEX.md`
- `tests/agent_review_workflow_test.rs`

**规模**：M。

### Task A8：AG-23 read-only agent investigate workflow

**描述**：交付 read-only investigate：strict round-robin、run state、pending turn、quorum/max-turns、continue/show/clean。fix path 与 AG-22 同一门禁。

**关联设计文档**：[`agent.md`](agent.md)、[`code.md`](code.md)。执行时遵循 AG-23/E8 read-only investigate 约束；任何 fix/action 只按 `code.md` 的内部受控执行边界处理。

**验收标准**：

- [x] 新增 `libra investigate start --topic <text> --agent <slug>... [--max-turns N] [--quorum N]`。
- [x] 新增 `list`、`show`、`continue`、`cancel`、`clean`。
- [x] `investigate list [--json]` 作为 run 枚举入口，与 A7 `review list` 同一分页契约（默认 `--limit 50`、cap 500、keyset cursor page envelope、schema/version 字段），有分页测试。
- [x] `state.json` 包含 `run_id`、`topic`、`agents`、`quorum`、`max_turns`、`next_agent_idx`、`pending_turn`、`stances`、`findings_doc`、`starting_sha`。
- [x] strict round-robin，不把 review 并发模型套到 investigate。
- [x] run-id 并发 lock；同 run 并发继续必须 fail-closed。
- [x] issue link / seed prompt 默认 untrusted，进入 mutating workflow 必须显式 approval/flag。
- [x] stances、findings_doc 与被引用的 transcript/checkpoint 摘要在注入下一轮 agent turn 或汇总前，与 seed 同等对待：标注 provenance=untrusted、走相同 redaction 管线、prompt 中以明确定界（spotlighting）区分于指令；`investigate show` 渲染前剥离 ANSI/终端控制序列。
- [x] investigator 复用 A7 的隔离 workspace public seam 与最小权限只读 spawn 形态（含测试断言 spawn 参数与工作目录）；in-place 运行默认拒绝。
- [x] 新顶层命令与 `cli.rs` 注册**同 PR 原子**满足 compat 接线（同 A7 对应项）：`COMPATIBILITY.md` 矩阵行、ROOT_AFTER_HELP 组行、`docs/development/commands/investigate.md` + README 表行、`pub const INVESTIGATE_EXAMPLES` + `after_help` + VISIBLE_COMMANDS 行、`docs/commands/investigate.md` 含 Examples 节并同步 zh-CN 页。
- [x] fix 未就绪时 `investigate fix <run_id>` 返回 `ERR_AGENT_FIX_BRIDGE_UNAVAILABLE`；untrusted seed 试图进入 mutating workflow 时返回 `ERR_AGENT_UNTRUSTED_SEED_FOR_MUTATION`。两者的真实 `LBR-*` 编号由 A3 在 `docs/error-codes.md` 分配；错误消息必须说明 read-only 可用和启用 fix 的前置条件。
- [x] 按 `agent.md` 补充规格 §6 落地 `agent.investigate.run` span/metric，tracing fake sink 断言必带/禁止字段。

**验证**：

- [x] `cargo test --test agent_investigate_workflow_test`
- [x] `cargo test --test compat_error_codes_doc_sync`
- [x] `cargo test --test compat_matrix_alignment`
- [x] `cargo test --test compat_help_examples_banner`
- [x] `cargo test --test compat_command_docs_examples_section`
- [x] `cargo test --lib cli::tests::root_after_help_lists_every_visible_command`
- [x] 测试覆盖 max-turns、quorum reached、no-new-findings stalled、agent failure pause、continue resume、cancel cleanup。

**依赖**：read-only 依赖 A1、A3、A6、A6.5；fix path 另依赖 Code 阶段的内部 bridge 证据。

**预计触达文件**：

- `src/cli.rs`
- `src/command/mod.rs`
- `src/command/investigate.rs` 或 `src/command/agent/investigate.rs`（CLI 面固定为顶层命令，不影响 compat 契约适用）
- `src/utils/error.rs`
- `docs/error-codes.md`
- `COMPATIBILITY.md`
- `docs/commands/investigate.md`
- `docs/commands/zh-CN/investigate.md`
- `docs/development/commands/investigate.md`（及 README.md 表行）
- `tests/compat/help_examples_banner.rs`
- `tests/INDEX.md`
- `tests/agent_investigate_workflow_test.rs`

**规模**：M。

### Task A8.5：AG-24a 合规实现面（audit、raw 授权、retention/GC、erasure）

**描述**：把 `agent.md` 强制的合规要求从"文档验收"升格为实现任务：append-only audit、raw 访问显式授权、retention 窗口清理与本地 erasure 三面一致。A9 只承担其文档同步，不得以文档验收替代本卡实现。

**关联设计文档**：[`agent.md`](agent.md)。执行时遵循 AG-24a 合规强制项（append-only audit、`--allow-raw`、`agent.retention.*`、GC/erasure）约束。

**验收标准**：

- [x] sql/migrations 新增 `agent_audit_log` append-only 表：仅 INSERT/SELECT，触发器或代码层拒绝 UPDATE/DELETE；按 `agent.md` 约束，`_down.sql` 不得删除审计数据，只能停止新写入（不得简单 DROP TABLE）。
- [x] checkpoint show/export 的 **raw（未脱敏）访问/导出**要求显式 `--allow-raw`（或等价 approval），每次访问写一条 audit 记录（who/when/checkpoint/scope/justification，字段对齐 `agent.md`）；未经授权的 raw 访问 fail-closed 拒绝并写 audit。redacted 的显式 `--detail`/`--transcript` 路径**不**要求 `--allow-raw`，只受 size cap、streaming/chunk 与 redaction 约束（与 `agent.md`「读取 pipeline」及 A5/A6.5 metadata-first 断言同口径）。
- [x] `libra agent clean --gc` 实现 `agent.retention.transcript_days`（默认 90）/`agent.retention.stderr_days`（默认 30）窗口清理；`clean --all`/GC 不触碰 `agent_audit_log`，有测试。
- [x] review/investigate run state 与 findings（`.libra/sessions/agent-runs/<run_id>/`、findings blob/manifest/DB 行）按 `agent.retention.findings_days`（默认 90）窗口清理（并入 `libra agent clean --gc` 或 review/investigate clean，落点实现时定），有测试；若该窗口明确 deferred，必须由 A9 release notes 说明。
- [x] 本地 erasure 三面一致：重写 `refs/libra/traces` + 删除 agent_session/agent_checkpoint 行 + 清理 `object_index`，配一致性测试；D1/R2 deletion propagation 维持 explicitly deferred（由 A9 release notes 说明）。
- [x] `agent.retention.transcript_days`、`agent.retention.stderr_days`、`agent.retention.findings_days`、`agent.max_transcript_read_bytes` 等 settings 键有默认值、校验和文档（`max_transcript_read_bytes` 作用于 A5/A6 的 detail 读取路径）。

**验证**：

- [x] `cargo test --test agent_audit_log_test`（新增 target，注册 `Cargo.toml` + `tests/INDEX.md`）
- [x] `cargo test --test agent_checkpoint_export_test allow_raw_gate`
- [x] `cargo test --test db_migration_test`
- [x] `cargo test --test compat_error_codes_doc_sync`

**依赖**：Task A5、A6。findings GC 子项另前置 A7/A8（run-state/findings 结构由其创建）；A7/A8 未动工时该子项按验收条款显式 deferred（由 A9 release notes 说明），不阻塞本卡其余项。

**预计触达文件**：

- `sql/migrations/*`
- `src/internal/ai/history.rs`
- `src/command/agent/checkpoint.rs`
- `src/command/agent/clean.rs`
- `src/utils/error.rs`
- `docs/error-codes.md`
- `tests/agent_audit_log_test.rs`
- `Cargo.toml`
- `tests/INDEX.md`

**规模**：M。

### Task A9：AG-24 agent docs, tests, compatibility and release closeout

**描述**：收敛 Gate 8 的 public behavior、schema、fixtures、retention/raw export/audit 和跨文档边界，避免 `agent.md`、runtime 文档、用户文档、compat matrix 漂移。

**关联设计文档**：[`agent.md`](agent.md)、[`code.md`](code.md)。执行时以 `agent.md` 收敛外部捕获 public surface，并只同步 `code.md` 中与内部 AgentRuntime/fix bridge 边界有关的约束。

**验收标准**：

- [x] `docs/development/tracing/agent.md` 与当前实现状态一致，规划 target 和已注册 target 不混写。
- [x] 同步 `docs/commands/agent.md`、zh-CN 文档、`COMPATIBILITY.md`、`docs/error-codes.md`、release notes/migration notes。
- [x] `tests/INDEX.md` 覆盖所有新增/重命名 target 的 wave、purpose、source mapping。
- [x] A8.5 落地的 retention、GC、raw export（`--allow-raw` + audit）、append-only audit、redaction report 完成**文档同步**（用户/运维文档）；本卡不承担其实现（见 A8.5）。
- [x] 按 §0 范围声明，为 `memory.md`、`sandbox.md`、`web-api.md` 头部补 "out-of-scope of tracing/plan.md" banner 并注明已知冲突条目。
- [x] 核对 fixtures 溯源 manifest（A4/A6）记录的 agent CLI 版本与 release 时点主流版本的差距，过期项记录为已知偏差或完成刷新。
- [x] `docs/development/internal/code-agent-runtime.md` / 当前 runtime source-of-truth 边界说明同步；不得重新引入旧 `../agent.md` / `../web-only.md` / `../code-agent-runtime.md` 链接。
- [x] 旧 claudecode provider 不被复活；`diagnostics_redaction_test` 事实保留。
- [x] 发布说明区分 enabled、preview/opt-in、explicitly deferred，尤其是 external discovery、review/investigate fix、D1/R2 deletion propagation。

**验证**：

- [x] `cargo test --test compat_agent_docs_contract`
- [x] `cargo test --test compat_agent_run_non_exhaustive_guard`
- [x] `cargo test --test compat_matrix_alignment`
- [x] `cargo test --test compat_error_codes_doc_sync`
- [x] `! rg -n "\]\(\.\./agent\.md\)|\]\(\.\./web-only\.md\)|\]\(\.\./code-agent-runtime\.md\)" docs/development/tracing/agent.md docs/development/tracing/code.md docs/development/internal/code-agent-runtime.md`（否定断言，期望零命中；与 `agent.md` 验收命令同式）
- [x] `test ! -d src/internal/ai/claudecode && ! rg -n "src/internal/ai/claudecode" src`（代码树否定断言，期望零命中。docs/tests 不纳入扫描：2026-07-04 实测 docs/tests 中全部 8 处命中均为计划要求保留的移除性表述——含 `tests/compat/agent_docs_contract.rs:33` 反而断言 agent.md 必须包含该路径字符串——原全范围扫描永不可跑绿）
- [x] `cargo +nightly fmt --all --check`
- [x] `cargo clippy --all-targets --all-features -- -D warnings`
- [x] `cargo test --all`

**依赖**：Task A1~A8.5（含 A6.5）的已落地范围。

**预计触达文件**：

- `docs/development/tracing/agent.md`
- `docs/commands/agent.md`
- `docs/commands/zh-CN/agent.md`（现存，必须同步）
- `COMPATIBILITY.md`
- `docs/error-codes.md`
- `tests/INDEX.md`
- `tests/compat/*`
- release notes / migration notes 位置按仓库现有规范确认

**规模**：M。

## 4. Agent 阶段检查点

### Checkpoint A-F1：AG-16 基础契约可合并

- [x] A1 完成，capability matrix schema pin 通过。
- [x] 非首批 agent 不再被误报 supported/installable/launchable。
- [x] 没有 checkpoint writer 或 storage 行为变更混入 A1。

### Checkpoint A-F2：CLI/RPC 安全边界可合并

- [x] A2、A3 完成。
- [x] alias 同语义、external RPC v2/v1 兼容、安全默认值和 E10 错误码均有测试。
- [x] fake binary 不能读取父进程 secret env，stderr 不泄露。

### Checkpoint A-F3：Hook/lifecycle/checkpoint 可合并

- [x] A4、A5、A6、A6.5 完成（A6 是 A6.5 的前置，必须在本检查点一并验收）。
- [x] Provider parser 不直接写 checkpoint。
- [x] 默认 list/show 不读取 transcript body。
- [x] crash recovery、doctor repair、prune A/B 窗口有确定性测试。
- [x] 本地 `codex`、`claude`、`opencode` 三条 smoke 均完成 session/checkpoint/traces 采集；任一缺失即阻塞第一期。

### Checkpoint A-F4：Workflow read-only 可合并

- [x] A7、A8 完成 read-only 范围（前置 A6、A6.5 已在 A-F3 验收）。
- [x] review/investigate 有 terminal state、cancel cleanup、bounded sink。
- [x] `--fix` 未就绪时稳定 unsupported；不得让 observed external agent 执行 workspace mutation。

### Checkpoint A-F5：Agent 总闭环

- [x] A8.5、A9 完成。
- [x] `cargo clippy --all-targets --all-features -- -D warnings` 与 `cargo test --all` 通过或记录明确、非本改动导致的既有失败。
- [x] `docs/development/tracing/code.md` 阶段尚未开始任何实现变更。

## 5. Code 阶段进入条件

只有满足以下条件才进入 `libra code` 阶段：

- [x] Agent Checkpoint A-F5 已通过。
- [x] Agent 第一期本地三 agent smoke 已通过：`codex`、`claude`、`opencode` 都完成采集闭环。
- [x] `review --fix` / `investigate fix` 的状态已明确：要么有内部 fix bridge 源码锚点和测试，要么仍保持 unsupported。
- [x] `docs/development/tracing/code.md` 的源码复核由 Task C1 承担：进入 Code 实现变更（C2 及之后任务）前 C1 必须完成，且差距清单以 Agent 阶段（含 A9）合并后的最终 HEAD 为基线，不得沿用 Agent 阶段前的旧结论。

## 6. Code 实施任务

`code.md` 当前描述的是已公开的 Libra AI extension，未列出大块净新建功能。Code 阶段的目标是：按当前源码核对并补齐 `libra code` 的公开模式、参数、provider、session、Web/headless/MCP、approval/sandbox、文档和测试闭环；若核对发现行为已满足，只做文档/测试/compat 收敛，不发明额外功能。

### Task C1：`libra code` source-grounded audit

**描述**：重新核对 `code.md` 与实际 `src/command/code.rs`、Code UI、MCP、docs/commands/code.md、tests/INDEX.md 是否一致，生成具体差距清单后再改代码。

**关联设计文档**：[`code.md`](code.md)。执行时只核对实现与该文档的差距，不重新定义 `libra code` 的目标、模式或 provider 边界。

**验收标准**：

- [x] 核对 `CodeArgs`、`CodeProvider`、`validate_mode_args`、`reject_non_tui_flags`、provider-specific flags、plan-mode、resume、web-only、stdio。
- [x] 核对 Web/headless/Code UI routes 与 `docs/commands/code.md` / zh-CN 文档。
- [x] 核对 MCP stdio 与 `code-control --stdio` 分工。
- [x] 核对 session resume、projection、graph handoff、audit sink、control token 文件。
- [x] 输出差距清单，标注每项为 code behavior、docs drift、test gap 或 deliberate difference。

**验证**：

- [x] `rg -n "validate_mode_args|reject_non_tui_flags|CodeUi|HeadlessCodeRuntime|LibraMcpServer|TracingAuditSink|SessionStore" src/command/code.rs src/internal/ai`
- [x] `cargo test --test code_cli_dispatch_test -- --list`
- [x] `cargo test --test compat_matrix_alignment -- --list`

**依赖**：Agent A-F5（只读核对可在 A-F4 通过后并行启动；但因 A9 会改动 docs/commands、tests/INDEX.md、COMPATIBILITY.md 等审计面，差距清单必须在 C2 开工前按 Agent 阶段最终 HEAD 复核一次）。

**预计触达文件**：初始只读；后续按差距任务触达。

**规模**：S。

### Task C2：Mode and argument contract hardening

**描述**：补齐 `libra code` 三模式（TUI、web-only、stdio）的参数互斥、provider-specific flags、错误消息和 JSON/quiet 行为。

**关联设计文档**：[`code.md`](code.md)。执行时遵循 `code.md` 的 mode、argument、provider-specific flag 和输出/错误契约。

**验收标准**：

前置：本卡与 C4 中 mode/provider 相关验收以 **C1 输出的分类清单**为准（§0.1.2 第 7 条；`reject_non_tui_flags` 当前在 web-only/stdio 下拒绝一切非 Gemini provider，与 help/banner 示例冲突）。

- [x] `--web-only` 与 `--stdio` 冲突稳定报错。
- [x] web-only/stdio 下拒绝 TUI-only 参数：`--resume`、provider-specific flags 等。
- [x] web-only/stdio 的 provider 校验按 C1 分类结果二选一落地：若判定 docs/help drift（维持仅默认 provider），保持拒绝 `--provider` 非默认，并同 PR 修正 help/banner 示例（`CODE_EXAMPLES` 中 `--web-only --provider ollama|codex`）、`BrowserControlMode` 注释与用户文档；若判定 code behavior（放宽 web-only provider），实现放宽 + CLI regression，并与 C4 的可达性验收联动。
- [x] Codex-only flags（`--codex-bin`、`--codex-port`、`--plan-mode=true`）只允许 `--provider=codex`；`--api-base` 在 Codex 下拒绝。
- [x] DeepSeek/Kimi/Ollama provider-specific flags 非对应 provider 时拒绝。
- [x] 所有失败消息可操作，并有 Display/CLI regression test。

**验证**：

- [x] `cargo test --test code_cli_dispatch_test`
- [x] `cargo test --lib code::tests::rejects_web_flags_in_stdio_mode`（fn 已存在于 `src/command/code.rs:4342`；完整路径为 `command::code::tests::…`，子串过滤即可命中）
- [x] `cargo test --lib code::tests::rejects_explicit_plan_mode_true_for_non_codex_provider`（fn 已存在于 `src/command/code.rs:4712`）

**依赖**：C1。

**预计触达文件**：

- `src/command/code.rs`
- `tests/code_cli_dispatch_test.rs`
- `docs/commands/code.md`
- `docs/commands/zh-CN/code.md`

**规模**：S。

### Task C3：Provider/runtime bootstrap and env handling

**描述**：确保 generic providers、Codex runtime、agent profile override、dotenv/Vault/env lookup 和 missing-key errors 在 TUI/headless 路径中一致、可测试、可恢复。

**关联设计文档**：[`code.md`](code.md)。执行时遵循 `code.md` 的 provider/runtime、Codex、env-file、Vault/env lookup 和 live-test 约束。

**验收标准**：

- [x] Provider factory 对 Gemini/OpenAI/Anthropic/DeepSeek/Kimi/Zhipu/Ollama/Codex 的默认模型、API key env、api-base 规则有测试。
- [x] `--env-file` 值优先于 Vault 和进程环境，错误消息指出缺哪个 env 和如何配置。
- [x] 本地 live/provider 验证统一使用仓库根目录 `.env.test`；CLI 场景传 `--env-file .env.test`，Cargo live 场景先 `set +x; source .env.test` 导出 Key。
- [x] Agent profile override 不允许 silent fallback 到 CLI provider。
- [x] Codex preflight 拒绝 file cwd，WebSocket startup timeout 有用户可读诊断。
- [x] live provider tests 保持 feature-gated，不进入默认 L1。

**验证**：

- [x] `cargo test --test code_provider_boot_test`
- [x] `cargo test --test code_codex_runtime_test`
- [x] `cargo test --test code_codex_default_tui_test --features test-provider -- --test-threads=1`
- [x] `set +x; set -a; source .env.test; set +a; cargo test --features test-live-ai --test ai_agent_test`
- [x] `LIBRA_RUN_LIVE=1 cargo test --features test-provider --test code_ui_remote_model_generation_matrix -- --ignored --test-threads=1`

**依赖**：C1、C2。

**预计触达文件**：

- `src/command/code.rs`
- `src/internal/ai/providers/*`
- `src/internal/ai/agent/profile/*`
- `tests/code_provider_boot_test.rs`
- `tests/code_codex_runtime_test.rs`

**规模**：M。

### Task C4：Web-only, Code UI, control and SSE contract

**描述**：核对并补齐 Web/headless Code UI 的 session snapshot、SSE、browser control、local automation token、diagnostics redaction 和 remote state tests。

**关联设计文档**：[`code.md`](code.md)。执行时遵循 `code.md` 的 Web-only、headless、Code UI API、browser control、diagnostics 和 SSE wire 约束。

**验收标准**：

前置：与 C2 相同，web-only provider 可达性相关验收以 C1 分类清单为准（当前 `--web-only --provider codex` 被 `reject_non_tui_flags` 拒绝，Codex web-only 分支为 CLI 不可达代码）。

- [x] 按 C1 分类二选一：若放宽 web-only provider——`--web-only` 非 Codex provider 走 `HeadlessCodeRuntime`、Codex provider 走 managed app-server path，并补可达性回归；若维持仅默认 provider——清理或显式标注当前 CLI 不可达的 Codex web-only 分支（app-server 分派、`web_only+Codex→Loopback` 默认值），并修正 banner/注释/docs 中的 `--web-only --provider codex` 示例。
- [x] `/api/code/session`、`/api/code/events`、`/api/code/diagnostics`、`/api/code/threads`、`/api/code/goal/status` observe-only 行为与文档一致。
- [x] Browser controller `loopback` 只允许 loopback host；browser-control 默认值矩阵按 C1 分类后的 provider 可达性固定（"Codex web-only 默认 loopback" 仅在放宽后成立），其它 provider 默认 off。
- [x] `--control write` 创建 0600 token 文件和非 secret control info，冲突快速失败。
- [x] diagnostics、control info、SSE 不泄露 token、headers、provider credentials 或 provider request/response body。
- [x] Wire shape 与 TypeScript mirror 一致。

**验证**：

- [x] `cargo test --features test-provider --test code_ui_remote_sse_matrix -- --test-threads=1`
- [x] `cargo test --features test-provider --test code_ui_remote_state_matrix -- --test-threads=1`
- [x] `cargo test --features test-provider --test code_ui_remote_security_matrix -- --test-threads=1`
- [x] `cargo test --features test-provider --test code_ui_remote_lease_matrix -- --test-threads=1`
- [x] `cargo test --test ai_code_ui_wire_test`
- [x] `cargo test --test ai_code_ui_projection_test`

（`code_ui_remote_*` matrices 的全部真实用例逐项被 `#[cfg(feature = "test-provider")]` 门控：不带 feature 只编译并通过 1 个 `*_requires_test_provider_feature` 跳过占位测试，显示"通过"但未执行任何真实用例，不得计为验收证据；与 CI compat-offline-core 第二遍口径一致。）

**依赖**：C1、C2。

**预计触达文件**：

- `src/command/code.rs`
- `src/command/code_control*.rs`
- `src/internal/ai/web/*`
- `src/internal/tui/*`
- `web/src/lib/code-ui/types.ts`
- `docs/commands/code.md`
- `docs/commands/zh-CN/code.md`

**规模**：M。

### Task C5：Session resume, graph handoff and persistence

**描述**：确保 `libra code` 的 canonical thread id、resume、SessionStore JSONL、projection bundle、graph handoff 和 history persistence 可恢复、可审计。

**关联设计文档**：[`code.md`](code.md)。执行时遵循 `code.md` 的 session resume、projection、graph handoff、audit sink 和 persistence 约束。

**验收标准**：

- [x] `--resume <THREAD_UUID>` 只在 TUI 允许，错误路径有测试。
- [x] TUI exit 时能打印/记录后续 `libra graph <thread_id>` 的入口，远程 repo 场景说明 `--repo <path>`。
- [x] Session JSONL reader 可跳过未知 event、恢复 truncated tail。
- [x] Projection bundle identity 优先于临时 session id。
- [x] Runtime audit sink 记录 local-tui-control 事件，不把 audit 当作 user-facing transcript。

**验证**：

- [x] `cargo test --features test-provider --test code_resume_test -- --test-threads=1`（真实用例逐项 feature 门控，裸跑只过 1 个跳过占位测试）
- [x] `cargo test --test ai_session_jsonl_test`
- [x] `cargo test --test ai_code_ui_projection_test`
- [x] `cargo test --test ai_goal_resume_test`

**依赖**：C1、C4。

**预计触达文件**：

- `src/command/code.rs`
- `src/internal/ai/session/*`
- `src/internal/ai/history.rs`
- `src/internal/ai/projection/*`
- `src/internal/ai/runtime/*`
- `docs/commands/code.md`

**规模**：S/M，按 C1 差距决定。

### Task C6：MCP stdio and code-control boundary

**描述**：把 `libra code --stdio` 的 MCP server 与 `libra code-control --stdio` 的 live TUI automation 明确分离，并补足双入口测试。

**关联设计文档**：[`code.md`](code.md)。执行时遵循 `code.md` 的 MCP stdio 与 `code-control --stdio` 分工；不得把 MCP 重新解释为 turn control plane。

**验收标准**：

- [x] `libra code --stdio` 只运行 MCP stdio server，不控制 live TUI。
- [x] `libra code-control --stdio` 是 automation client 入口，受 control token/lease gate 保护。
- [x] docs 中不把 MCP stdio 描述成 AgentRuntime turn control plane。
- [x] MCP HTTP/stdio dual entry 的 tool set、错误和 shutdown 行为有回归测试。

**验证**：

- [x] `cargo test --features test-provider --test code_mcp_dual_entry_test -- --test-threads=1`（真实用例逐项 feature 门控，裸跑只过 1 个跳过占位测试）
- [x] `cargo test --features test-provider --test code_ui_remote_security_matrix -- --test-threads=1`
- [x] `rg -n "code-control --stdio|libra code --stdio|MCP" docs/commands docs/development/tracing/code.md docs/development/tracing/agent.md docs/development/tracing/plan.md src/command`（§0 范围外的 memory.md/sandbox.md/web-api.md 不在扫描范围；其 `libra mcp --stdio` 等表述按 §0 out-of-scope 声明处理，不作为本卡验收对象。判定口径：输出经人工复核，不得存在把 MCP stdio 描述为 live TUI turn control plane 的表述；复核结论附入 PR 描述）

**依赖**：C1、C4。

**预计触达文件**：

- `src/command/code.rs`
- `src/command/code_control.rs`
- `src/internal/ai/mcp/*`
- `docs/commands/code.md`
- `docs/commands/code-control.md`
- `docs/development/tracing/code.md`

**规模**：S。

### Task C7：Sandbox, approval and tool gate consistency

**描述**：核对 `libra code` 内部 AgentRuntime 的 mutating path，确保 review/investigate fix bridge 只能走 serialized queue、approval、sandbox、tool ACL。

**关联设计文档**：[`code.md`](code.md)、[`agent.md`](agent.md)。执行时遵循 `code.md` 的 internal AgentRuntime / approval / sandbox / tool gate 约束，并保持 `agent.md` 对 observed external agent 只提供 evidence/provenance 的边界。

**验收标准**：

- [x] `CodeContext::Review` / `Research` 默认 read-only，`Dev` workspace-write。
- [x] `--approval-policy`、`--approval-ttl`、`--network-access` 映射到 `ToolRuntimeContext`，并在 tool invocation 中可见。
- [x] Tool ACL 区分 read-only、workspace-write、network、broad/mutating tools。
- [x] `review --fix` / `investigate fix` 若在 Agent 阶段启用，必须在这里有源码锚点和 tests；否则继续 unsupported。
- [x] 错误路径不得 `unwrap()` / `expect()`；生产路径用 `?` + context。

**验证**：

- [x] `cargo test --test code_tool_acl_test`
- [x] `cargo test --features test-provider --test code_ui_remote_approval_matrix -- --test-threads=1`（真实用例逐项 feature 门控，裸跑只过 1 个跳过占位测试）
- [x] `cargo test --test ai_subagent_worktree_readonly_test`
- [x] `cargo test --test compat_all_production_unwrap_guard`

**依赖**：C1；若解锁 Agent fix path，还依赖 A7/A8。

**预计触达文件**：

- `src/command/code.rs`
- `src/internal/ai/sandbox/*`
- `src/internal/ai/tools/*`
- `src/internal/ai/agent/runtime/*`
- `tests/code_tool_acl_test.rs`
- `tests/code_ui_remote_approval_matrix.rs`

**规模**：M。

### Task C8：Code docs, compatibility and final closeout

**描述**：把 `libra code` 的实际状态同步回 tracing 目标文档、用户文档、compat matrix 和测试索引。

**关联设计文档**：[`code.md`](code.md)、[`agent.md`](agent.md)。执行时以 `code.md` 收敛 `libra code` public behavior，并同步 `agent.md` 中与 fix bridge、review/investigate 边界相关的交叉说明。

**验收标准**：

- [x] `docs/development/tracing/code.md` 与当前源码和测试证据一致。
- [x] `docs/commands/code.md` 与 `docs/commands/zh-CN/code.md` 同步参数、provider、mode、Code UI API、tracing/logging、examples。
- [x] `COMPATIBILITY.md` 明确 `libra code` 是 Libra-only extension / intentionally different。
- [x] `tests/INDEX.md` 中 Code UI、MCP、provider、resume、sandbox tests 的 wave 和 source mapping 准确。
- [x] 若 Agent 阶段启用了 mutating fix bridge，release notes 同时说明 `libra agent` 与 `libra code` 的协作边界。

**验证**：

- [x] `cargo test --test compat_matrix_alignment`
- [x] `cargo test --test code_cli_dispatch_test`
- [x] `cargo test --test code_provider_boot_test`
- [x] `cargo test --features test-provider --test code_mcp_dual_entry_test -- --test-threads=1`
- [x] `cargo test --features test-provider --test code_resume_test -- --test-threads=1`
- [x] `cargo test --test code_tool_acl_test`
- [x] `cargo +nightly fmt --all --check`
- [x] `cargo clippy --all-targets --all-features -- -D warnings`
- [x] `cargo test --all`

**依赖**：C1~C7。

**预计触达文件**：

- `docs/development/tracing/code.md`
- `docs/commands/code.md`
- `docs/commands/zh-CN/code.md`
- `COMPATIBILITY.md`
- `tests/INDEX.md`
- release notes / migration notes 位置按仓库现有规范确认

**规模**：S/M。

## 7. Code 阶段检查点

### Checkpoint C-F1：Code audit 决策完成

- [x] C1 完成，差距清单按 code behavior / docs drift / test gap / deliberate difference 分类。
- [x] C1 差距清单已按 Agent 阶段最终 HEAD（含 A9 的 docs/compat/tests 改动）复核。
- [x] 没有在未确认差距前修改 `src/command/code.rs`。

### Checkpoint C-F2：Mode/provider/Web/session 核心路径完成

- [x] C2、C3、C4、C5 完成或明确无代码差距。
- [x] 默认测试和 feature-gated 测试边界清楚。

### Checkpoint C-F3：Control/MCP/sandbox/fix bridge 完成

- [x] C6、C7 完成。
- [x] `libra code --stdio` 与 `code-control --stdio` 边界清晰。
- [x] Mutating fix bridge 只有在 approval/sandbox/tool ACL 证据齐全时启用。

### Checkpoint C-F4：总闭环

- [x] C8 完成。
- [x] Agent 与 Code 两条文档边界没有互相重写事实源。
- [x] 全量 fmt、clippy、默认测试通过或记录已知外部失败。

## 8. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| 把 observed external agent 当作内部 AgentRuntime executor | 绕过 approval/sandbox/tool gate，产生不可审计 mutation | A7/A8 默认 read-only；fix path 必须等 C7 源码锚点和测试 |
| 非首批 agent 被误报 supported/installable | 用户安装失败或捕获错误 agent kind | A1 registry + capability matrix pin；A2 unsupported tests |
| external binary 继承 secrets 或冒用内置 slug | API key 泄露、供应链风险 | A3 `env_clear`、trust/quarantine/provenance、built-in slug skip |
| redaction 只靠散点调用 | raw prompt/stderr/transcript 落盘 | A4/A5 类型级 `RedactedBytes` sink，redaction failure fail-closed |
| checkpoint ref/DB/object_index 不一致 | restore/doctor/show 找不到对象，cloud sync 缺数据 | A5 crash matrix、doctor repair、UPSERT、prune A/B tests |
| 大 transcript 默认读取 | 内存暴涨、TUI/Web 卡死 | A5 metadata-first + chunked streaming + cap |
| A6.5 本地 smoke 被 fake fixture 替代 | 第一期声称支持 `codex`/`claude`/`opencode`，但真实用户环境无法采集 | A6.5 必须使用本机真实 binary 与临时 repo；fake fixture 只作为 deterministic regression |
| `.env.test` 或外部 binary 输出泄露 secret | API key、provider token、D1/R2 key 或 prompt 泄露到日志/JSON/对象 | `.env.test` 不得回显；live 命令前关闭 shell xtrace；external RPC `env_clear`，stderr/stdout cap + redaction |
| public JSON / RPC / DB schema 无版本 | 外部脚本、Web UI、legacy reader 或 external binary 升级后不兼容 | 所有 public wire 增加 `schema_version`/`protocol_version`，新增 snapshot/compat test，release notes 写兼容窗口 |
| provider hook 直接写 checkpoint | validation/redaction/owner filtering 被绕过，产生重复或污染 checkpoint | A4 只允许 hook/parser 产 `LifecycleEvent`；A5 统一 writer 消费 validated/redacted event |
| review/investigate cancel 或 timeout 未收敛 | 残留进程、reader task、lock、lease 或 pending turn，导致后续 run 卡死 | A7/A8 必测 terminal state、cancel cleanup、timeout cleanup 和 run-id lock |
| MCP stdio、code-control 与 Agent turn control 混同 | 错把传输/control 面当内部执行面，破坏 approval/sandbox 边界 | C6 固定 MCP stdio 与 `code-control --stdio` 分工；C7 才决定 mutating fix bridge |
| 大卡作为单 PR 推进 | diff 过大、测试目标漂移、review 不可控，难以回滚 | 0.2 要求 contract/implementation/safety/compat/live slice 拆分；每个 slice 保持可构建可测试 |
| retention、raw export、audit 只落文档不落实现 | 难以满足本地删除、隐私和审计要求 | A8.5 落实 `agent_audit_log`/`--allow-raw`/retention GC/erasure 实现；A9/C8 只承担文档与 release notes 同步 |
| codex/opencode 上游 hook 机制与计划假设不符（trust 门控、无 hooks.json） | hook 安装"成功"但静默不执行，采集闭环误判 | §0.3.3 改为上游实测为准；A4 增加 trust/enable 门控与 opencode plugin 事件映射验收 |
| reviewer/investigator in-place 运行读到仓库 secrets | `.env.test` 等真实 key 经 findings/网络外泄 | A7/A8 强制隔离 workspace（ignore 规则排除 secrets）+ 最小权限只读 spawn + env 只注入 allowlist |
| reviewer findings/stances 反哺后续 prompt 造成注入 | 外部输出诱导后续 agent 或伪造终端输出 | A7/A8 provenance=untrusted、redaction、spotlighting 定界、show 前剥离 ANSI 控制序列 |
| prune/clean 重写 `refs/libra/traces` 后 `agent push` 非快进发散 | traces 远端同步失败且无恢复出口 | A5 定义 push 语义（force-with-lease 等价或稳定错误码 + 重推出口）并配回归测试 |
| 升级后存量 v1 checkpoint 被 doctor 误判或不可读 | doctor 误修复、用户历史数据丢失可读性 | A5 legacy-v1 reader/doctor 分类验收 + 改造前 writer fixture 回归 |
| E10 种子错误码与已发布编号冲突 | 同码双语义破坏稳定错误码契约 | A3 编号核对门禁：以 `docs/error-codes.md` 实际分配为准，同 PR 修正 `agent.md` 种子表 |
| 同目录范围外文档（memory/sandbox/web-api）被当作事实源 | 执行者引入互斥断言，A4/C4/C6/C7 边界被污染 | §0 out-of-scope 声明；A9/C8 补 banner；C6 验证扫描范围排除 |
| Code 阶段按旧文档实现 | 重复或回退已有功能 | C1 强制 source-grounded audit，差距分类后再改 |
| 测试 target 写了但未注册 | CI 不运行，计划误判完成 | 每个新增 target 必改 `Cargo.toml` + `tests/INDEX.md`，A9/C8 closeout 复核 |

## 9. 总体验收命令

Agent 阶段完成后至少运行：

```bash
cargo test --test compat_agent_docs_contract
cargo test --test compat_agent_run_non_exhaustive_guard
cargo test --test compat_agent_capability_matrix_pin
cargo test --test compat_agent_architecture_guard
cargo test --test agent_rpc_external_test
cargo test --test agent_lifecycle_event_test
cargo test --test agent_checkpoint_redaction_test
cargo test --test agent_checkpoint_export_test
cargo test --test agent_transcript_intelligence_test
LIBRA_RUN_LOCAL_AGENTS=1 LIBRA_LOCAL_AGENT_SET=codex,claude-code,opencode cargo test --test agent_local_capture_smoke_test -- --ignored --test-threads=1
cargo test --test agent_review_workflow_test
cargo test --test agent_investigate_workflow_test
cargo test --test agent_audit_log_test
cargo test --test command_test agent
```

声明 Agent 第一期完成前，还必须在本机运行真实 agent 采集 smoke：

```bash
LIBRA_RUN_LOCAL_AGENTS=1 LIBRA_LOCAL_AGENT_SET=codex,claude-code,opencode cargo test --test agent_local_capture_smoke_test -- --ignored --test-threads=1
libra agent list --json
libra agent session list --json
libra agent checkpoint list --json
libra agent doctor --json
```

排障时可临时保留本地 evidence：

```bash
LIBRA_RUN_LOCAL_AGENTS=1 LIBRA_LOCAL_AGENT_SET=codex,claude-code,opencode LIBRA_KEEP_LOCAL_AGENT_SMOKE=1 LIBRA_LOCAL_AGENT_TIMEOUT_SECS=180 cargo test --test agent_local_capture_smoke_test -- --ignored --test-threads=1
```

Code 阶段完成后至少运行：

```bash
cargo test --test code_cli_dispatch_test
cargo test --test code_provider_boot_test
cargo test --features test-provider --test code_mcp_dual_entry_test -- --test-threads=1
cargo test --features test-provider --test code_resume_test -- --test-threads=1
cargo test --test code_tool_acl_test
cargo test --features test-provider --test code_ui_remote_lease_matrix -- --test-threads=1
cargo test --features test-provider --test code_ui_remote_sse_matrix -- --test-threads=1
cargo test --features test-provider --test code_ui_remote_state_matrix -- --test-threads=1
cargo test --features test-provider --test code_ui_remote_security_matrix -- --test-threads=1
cargo test --features test-provider --test code_ui_remote_generation_matrix -- --test-threads=1
cargo test --features test-provider --test code_ui_remote_approval_matrix -- --test-threads=1
cargo test --test ai_code_ui_wire_test
cargo test --test ai_code_ui_projection_test
cargo test --features test-provider --test ai_code_ui_headless_test -- --test-threads=1
```

注意：`code_ui_remote_*` matrices、`code_mcp_dual_entry_test`、`code_resume_test` 的全部真实用例逐项被 `#[cfg(feature = "test-provider")]` 门控——不带 `--features test-provider` 时只编译 1 个 `#[cfg(not(feature = "test-provider"))]` 的 `*_requires_test_provider_feature` 跳过占位测试，显示"通过"但未执行任何真实用例，不得计为验收证据（与 CI compat-offline-core 第二遍 `--features test-provider ... --test-threads=1` 口径一致）。另：`ai_code_ui_headless_test` 是**整文件** `#![cfg(feature = "test-provider")]` 门控（`tests/ai_code_ui_headless_test.rs:10`，无占位测试，裸跑编译为 0 个测试"通过"），同样必须带 feature 运行才计为证据。

Code 阶段的 live/provider-backed 验证使用仓库根目录 `.env.test`。其中 CLI 场景传 `--env-file .env.test`；直接读取进程环境的 Cargo live tests 先导出该文件中的 Key：

```bash
set +x
set -a; source .env.test; set +a
cargo test --features test-live-ai --test ai_agent_test
cargo test --features test-live-ai --test ai_chat_agent_test
LIBRA_RUN_LIVE=1 cargo test --features test-provider --test code_ui_remote_model_generation_matrix -- --ignored --test-threads=1
```

最终 closeout 至少运行：

```bash
cargo +nightly fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo test --test compat_matrix_alignment
cargo test --test compat_error_codes_doc_sync
cargo test --test compat_all_production_unwrap_guard
cargo test --test compat_help_examples_banner
cargo test --test compat_command_docs_examples_section
cargo test --lib cli::tests::root_after_help_lists_every_visible_command
```

若某个 target 仍是计划目标、尚未注册，不能把它列入“当前已通过”；必须在 PR 中说明 target 未注册原因、重启条件和替代验证。

## 10. 执行进度记录

执行者每完成（或阻塞）一个任务/slice，按 §0.4 第 7 步在此表追加一行。本表 + `libra log` 是任务完成状态的唯一事实源；任务卡内的验收 checkbox 仅表示验收项覆盖情况，不单独构成完成证据。

| 日期 | 任务/slice | 结果 | 版本 | commit | 验证摘要 / blocked 原因与恢复步骤 |
|---|---|---|---|---|---|
| 2026-07-03 | （基线）计划四轮复核完成（§0.1.3），尚未开始执行 | 基线 | Cargo.toml `0.17.1808`；web/worker package.json `0.17.1758`（滞后，首次发布时同步） | — | 三份 tracing 文档已统一；两处守卫断链（`agent_docs_contract.rs` 编译期 include_str! + `matrix_alignment.rs:104` 运行时 read_repo_file 均指旧路径）导致 `cargo test --all` 当前失败——执行顺序为 0.1 → 0.3（修断链）→ 0.4（清理 internal/tracing 事实源 drift）→ 0.2 |
| 2026-07-04 | （基线）第五轮落地核查完成（§0.1.5），仍未开始执行任务 | 基线 | 本行文档改动随 `v0.18.0` 发布（Cargo.toml 由 `0.17.1832` 升至 `0.18.0`；web/worker package.json 同步 `0.18.0`，`0.17.1758` 滞后已消除） | — | 七维核查：源码锚点/跨文档回写/测试 target 命名全部成立；修复 Task 0.3、Task 0.4、A9 三条按原文不可跑绿的验证命令；Task 0.3 触达面补全（29 处命中/约 25 文件，规模 XS/S→S）并明确四类豁免；A5 补孤儿测试模块（`tests/command/agent_checkpoint_test.rs`）接线验收；feature 门控口径改为"逐项门控 + 1 个跳过占位测试"。A6.5 环境前置实测通过（codex 0.142.5 / claude 2.1.200 / opencode 1.17.11 均在位且登录态检查通过），当前不 blocked。codex review 三轮 FAIL→均已修（§0.1.5 第 8/9/10 条）：第一轮——Task 0.3 并入 `compat_matrix_alignment` 对已迁移 integration-test-plan.md 的第三处断链重指（15 处/9 文件）、§9 的 `ai_code_ui_headless_test` 改为带 `--features test-provider`（整文件门控，裸跑 0 测试）；第二轮——integration-scenarios 家族（yaml + 场景文档目录，含 integration-runner 4 处功能性拼接）补全 21 处/10 文件清单 + 家族否定断言；第三轮——bare/相对简写残留（README 父级相对链接断链 + 13 行简写缺口）补规范化验收与两条守卫。守卫断链未修（仍待 Task 0.3），`cargo test --all` 仍失败 |
| 2026-07-04 | Task 0.1 建立当前事实基线 | 完成（只读，豁免版本递增/构建/部署） | 0.18.0（未变） | 本行所在提交 | 执行机切换为 Linux aarch64（NVIDIA spark-4120；原第五轮核查在 darwin arm64。本行初记 x86_64 有误，随 Task 0.3 提交修正）；本机 A6.5 前提复核：codex 0.142.4、claude 2.1.201、opencode 1.17.13 在位，nightly rustfmt/pnpm/.env.test 就绪，登录态只读检查留待 A6.5 执行时做。基线核对（五路并行 + 实跑）：三份目标文档已读；`Commands` enum 100 变体、无 review/investigate 顶层命令；`AgentSubcommand` 10 变体、`STABLE_AGENT_SLUGS=["claude-code","gemini"]`（`src/command/agent/mod.rs:216`）；`observed_agents/` 无 capability.rs/registry.rs、`DeclaredAgentCaps`/`AgentRegistration` 0 命中、`ObservedAgentHooks` 在 `adapter.rs:223`；hooks providers 仅 claude/gemini 两目录；`LifecycleEventKind` 恰 11 变体（`lifecycle.rs:35`）；守卫断链复现与 §0.1.5 一致（`agent_docs_contract.rs:8` include_str! 编译失败——`LIBRA_SKIP_WEB_BUILD=1 cargo check --all-targets` 实跑确认，+ `matrix_alignment.rs:103/:104/:161/:166/:171`、`matrix_alignment_support.rs:174/:181` 运行时读取）；Task 0.3 六条 rg 预采集命中 A=29/B=2/C=15/D=21/E=1/F=13，与 §0.1.5 记录吻合。基线外发现（非 0.3 范围，待后续任务归置）：`tests/compat/clean_intentional_diff.rs` 与 `tests/compat/diff_production_expect_guard.rs` 为未注册孤儿（无 `[[test]]`、无 README 行、从未编译）；`tests/command/agent_checkpoint_test.rs` 孤儿确认（A5 已有承接验收）。基线核对时工作树干净（`libra status --short` 空）；本行是核对后的唯一工作树改动，随本提交落库。codex review：8/8 事实断言 MATCH，唯一指摘为本行未提交时的措辞歧义，已按其修正 |
| 2026-07-04 | Task 0.3 文档搬迁接线 | 完成 | 0.18.1 | b50484e（实现）+ 本行所在提交（版本递增+记录） | 7 条否定断言全部零命中；`compat_agent_docs_contract` 1/1、`compat_matrix_alignment` 7/7 通过（其中 `command_development_readme_matches_public_cli_surface` 的按命令名硬拼路径循环是计划未列出的第八处断链，agent/code 特例化重指 tracing/，其余命令语义不变）；`LIBRA_SKIP_WEB_BUILD=1 cargo check --all-targets` 通过（include_str! 断链修复后首次全 target 可编译）；integration-runner `check-plan` 42 场景对齐（功能性路径重指实证）；fmt/clippy 最终树全绿。`cargo test --all`：2417 通过 / 10 失败——8×`operation_wrapper_test`（`no such column: reference.worktree_id` schema 漂移）+ `cli_error_test` hint 行为漂移 + `rebase_test` executable-mode 脏树误报；单目标复跑稳定复现，且 b50484e 对 src/Cargo.toml 的改动经 diff 核验全为注释行（零行为变化），判定为 33b790d 上游态在本机的既有失败，按 §0.4 豁免记录，待后续承接。执行期新发现：第七处断链 `tests/ai_provider_transform_test.rs:281`（运行时读旧根 code-agent-runtime.md）已重指 internal/ 并回写任务卡；rustdoc 相对深度实测修正为 4 级 `../`（任务卡原记 5 级越出仓库根，已修正注记）。环境备忘：本机 debug 版 `libra init` 约 21.5s CPU（release 0.7s），`cargo test --all` 需 `CARGO_PROFILE_DEV_OPT_LEVEL=1 CARGO_PROFILE_TEST_OPT_LEVEL=1` 加速（语义不变，debug_assertions 仍开，套件 ~19 分钟）。codex review（codex exec 0.142.4，read-only sandbox，实跑断言）VERDICT: PASS、零缺陷。推送：新仓库 main ruleset（PR+1 审批+required_signatures，无 bypass actor）阻断直推，用户已确认自行添加 Repository admin bypass，生效前本地提交积压待补推（9ac6be1、b50484e、本行提交），后台监视器就位 |
| 2026-07-04 | Task 0.4 事实源文档自洽清理 | 完成 | 0.18.2 | 本行所在提交 | `docs/development/internal/code-agent-runtime.md` 共 45 处编辑：11 处 `](commands/agent.md)` 链接（含 :2971 短形态）重指 `../tracing/agent.md` 并同步链接文字、4 处 `](commands/_general.md)` href 修正相对深度 `../commands/_general.md`、13 处 `](mcp.md)` 链接全部转为不可点击历史标注（涉当前边界的句子重定向 `docs/development/tracing/code.md` C6，已核实 code.md 确有 C6 承接该边界）、:2678 自链接改纯文本、15 处 prose/验证命令旧根路径重指（含 :958/:2675/:2788/:2793 四条 drift rg 命令的路径实参）。四条验证命令全部 PASS（test ! -e 五文件、两条否定 rg 断言零命中、正面断言 4 文件均有新路径命中）。codex review 第一轮 FAIL（bare `commands/agent.md` 裸述 5 行 + bare `mcp.md` 裸述 3 行——链接形态正则扫不到的残留）→ 修复 10 处 → 第二轮 VERDICT: PASS（全文件扫描确认 `docs/commands/agent.md` 公共文档路径正确保留）。fmt 全绿；本卡纯文档改动零 Rust 变更，clippy 沿用 Task 0.3 最终树结论；`cargo test --all` 与 Task 0.3 同口径（10 个既有失败按 §0.4 豁免，另见 Task 0.3 行）。推送仍待 ruleset bypass |
| 2026-07-04 | Task 0.2 固定公共命令开发规则 | 完成（只读，豁免版本递增/构建/部署） | 0.18.2（未变） | 本行所在提交 | 规则本体已固化于 §0.2/§0.4/§0.5 checklist，无独立代码变更；验收两守卫在 0.3/0.4 落地后的树上跑绿：`compat_matrix_alignment` 7/7、`compat_error_codes_doc_sync` 1/1。0.x 前置任务（0.1→0.3→0.4→0.2）全部完成，Agent 阶段 A1 就绪 |
| 2026-07-04 | Task A1（AG-16 capability contract） | 完成 | 0.18.3 | 本行所在提交 | 新增 `observed_agents/capability.rs`（`DeclaredAgentCaps` 8-bool E1 契约 + `CapabilityDeclarer` + 10 个可选能力 trait + E7 `SkillEvent` wire 形态）与 `observed_agents/registry.rs`（`AgentRegistration` 14 字段静态 matrix、7 行按注册序、首批 supported=claude-code/codex/opencode@first_batch、gemini/cursor/copilot/factory-ai unsupported、`lookup_cli_slug` 未知 slug quarantine fail-closed）；`ObservedAgent` 加 13 个 `as_*` 能力访问器（默认 None）+ `declared_capabilities()` 自省默认实现；删除 dead trait `ObservedAgentHooks`（0 impl）及其 re-export/文档引用。测试：`compat_agent_capability_matrix_pin` 3/3、`compat_agent_architecture_guard` 4/4（含 observed_agents 不 import runtime/checkpoint 层守卫——放行 doc 注释与既存 `derived.rs:70` `orchestrator::types` 数据 seam、SQL CHECK↔enum↔doc roster 三方同步断言）、`cargo test --lib observed_agents` 109/109；两 target 已注册 `Cargo.toml [[test]]` + `tests/INDEX.md` + `tests/compat/README.md`；agent.md 测试矩阵两行同步为已注册。范围守恒：未动 `STABLE_AGENT_SLUGS`（AG-17 承接）、未动 writer/storage；E1 pin 的 external methods[] 解锁断言待 AG-18 shim 落地后补入（守卫注释已注明）。codex review 六轮（codex exec 0.142.4 read-only sandbox）：R1 FAIL（注释残留符号名 + 分组 import 绕过）→ R2 FAIL（嵌套分组 `internal::{ai::{…}}` + cfg(test) 文本截断）→ 架构守卫改为 **syn AST 实现**（新增 syn dev-dep；use-tree 展平、`#[cfg(test)]` item 级剪枝、内联路径 visit）→ R3 FAIL（`cfg(not(test))` 误剪枝 + `use … as` 别名绕过）→ 修复（精确 test 谓词、禁 internal/ai 根别名与根 glob、按文件深度校准 super 链）→ R4 FAIL（`{self as x}` 形态 + mod.rs 深度差一）→ 修复 → R5 FAIL（registry 声明 claude hooks=true 但 adapter 自省 false——契约不自洽）→ `ClaudeCodeObservedAgent::as_hooks()` 接线现有 `ClaudeProvider` + pin 测试加双向一致性断言（`row.capabilities == declared_capabilities()`、`hook_installable == supported && as_hooks().is_some()`；gemini 有意不接线，E9 禁止其能力暴露）→ **R6 VERDICT: PASS 零缺陷**。每轮修复均带绕过探针实证（注入违规文件守卫必 FAIL、移除复绿）。质量门禁：fmt/clippy 全绿；`cargo test --all` 同口径 10 个既有失败（§0.4 豁免，见 Task 0.3 行） |
| 2026-07-04 | Task A2（AG-17 CLI alias parity） | 完成 | 0.18.4 | 本行所在提交 | 删除 `STABLE_AGENT_SLUGS` 常量（CLI roster 改由 AG-16 registry `supported_slugs()` 派生，`rg STABLE_AGENT_SLUGS src` 零命中）；新增 `agent list`（capability matrix，`--json` 带 `schema_version=1` + AG-17 冻结行键，`support_wave` 对 unsupported 行序列化 null、`installed` 运行时叠加仅限 hook_installable 行且检查失败硬报错）；`add`/`remove` 为 `enable --agent`/`disable --agent` 严格别名（同 execute 路径、同退出码、同诊断，位置参数，空参=支持 roster）；安装/卸载语义收敛：批量预校验 fail-closed（任一 unsupported 即整批拒绝、零副作用）、supported-未落地 HookProvider（codex/opencode）为提示性跳过 exit 0、gemini 专属 uninstall-only 通道（幂等、`hooks_are_installed` 预检、错误带上下文传播、非 gemini 的非 roster remove 一律 actionable unsupported）、enable gemini 指向 remove 通道。测试：`tests/command/agent_roster_test.rs` 6 个用例（规格 5 个 + list 检查失败回归；两个 gemini 用例 `#[serial]` 真 provider 安装/卸载/用户条目保留断言）全绿；`command::agent` lib 单测 21/21。文档：`docs/commands/agent.md` + zh-CN 同步（synopsis/子命令表/选项/JSON 契约/示例/roster 说明）；tracing/agent.md `STABLE_AGENT_SLUGS` 两处锚点标历史 + AG-17 测试矩阵行更新；COMPATIBILITY.md 泛化行无需变更。codex review 三轮：R1 FAIL（uninstall 吞错、support_wave 缺席、registry 注释残留常量名、help 措辞陈旧）→ 修 4 处 → R2 FAIL（list `.ok()` 吞错、remove cursor 静默成功违反 gemini-only 通道语义）→ 修 2 处 + 2 个回归测试 → **R3 VERDICT: PASS**。质量门禁：fmt/clippy 全绿；`cargo test --all` 同口径 10 个既有失败（§0.4 豁免） |
| 2026-07-05 | Task A3（AG-18 external RPC v2 + 安全） | 完成 | 0.18.5 | 3b70a66（契约切片：LBR-AGENT-002~011 分配 + 唯一性守卫）+ 本行所在提交（实现切片，2026-07-04 起跑、07-05 过审发布） | **契约**：`StableErrorCode` 新增 10 变体（002 gate/003 版本/004 能力/005 provenance/006 仿冒/007 IO 安全/008 envelope/009 store/010 fix-bridge/011 untrusted-seed，全部 Internal 类别 exit 128）+ `docs/error-codes.md` 双表 + agent.md E10 表编号回写 + `compat_error_codes_doc_sync` 补同码多 variant 唯一性守卫。**实现**：`rpc.rs` v2 `info`（AgentInfo 9 字段、protocol_version 缺省=v1、高版本 fail-closed）+ 协商顺序 info→v1 capabilities（v1 保留、v2 二进制必须续答）；`spawn` `env_clear()`+白名单注入（LIBRA_AGENT_PROTOCOL_VERSION/LIBRA_CLI_VERSION/LIBRA_REPO_ROOT）；stderr `Stdio::piped` 捕获 + 64 KiB cap + Redactor 脱敏 + 控制序列剥离（`redacted_stderr_excerpt`，超时/错误路径附带）；discover 内置 slug 仿冒 skip-and-log；新增 `trust.rs`（TrustRecord JSON 存 config `agent.trust.<slug>`、sha256/device/inode/mtime 计算与复验、漂移即撤销、父目录 world-writable 拒绝）；CLI `rpc trust/untrust` 子命令 + `agent.external_agents.enabled`（键名与 agent.md settings 表一致）默认 false 门禁——codex R1 后收紧为 list/trust/invoke 全部拒绝带 LBR-AGENT-002（untrust 豁免：撤销信任只收紧安全面）；trust 时即拒绝 world-writable 父目录（005，`record_trust` 库层同步强制）；invoke 全链：门禁→仿冒拒绝→trust 必须→spawn 前 provenance 复验→世界可写检查→spawn_in_repo→info/caps 协商→能力门控（004）→IO-cap 映射 007、timeout/broken-pipe/malformed-frame 映射新分配 012（`ERR_AGENT_RPC_TRANSPORT_FAILED`，agent.md E10 双表 + docs/error-codes.md 双表 + pin 测试同步）。**spans**：`agent.rpc.discover`（slug/external_binary/quarantined/reason 事件）+ `agent.rpc.invoke`（slug/method/protocol_version/timeout_ms/frame_bytes/terminal_state）。**测试**：`agent_rpc_external_test` 9 用例（info 注册/版本拒绝/v1 回退/timeout+oversize/undeclared/stderr cap+redact/env 清空探针/发现仿冒过滤/info 非 method-not-found 失败传播）+ `agent_rpc_span_test` 3 用例（invoke span 字段 + 失败 terminal_state + discover 事件字段，独立二进制规避 tracing callsite-cache 并发竞态——全文件并行下 with_default 线程本地订阅者与同回调点无订阅线程互相翻转 interest 缓存，实测单跑/单线程绿、并行偶发空捕获）+ `command_test::agent_rpc_trust_test` 6 用例（CLI E2E：门禁 002 覆盖 list/trust/invoke + untrust 豁免→opt-in→未信任 005→trust→invoke 成功→篡改 005+自动重隔离；mtime-only 漂移 005；world-writable 目录拒信任 005；提前退出 transport 012；stderr 泄密在默认输出与 LIBRA_ERROR_JSON payload 双路脱敏；仿冒 006 覆盖 trust+invoke 双路径）+ `command::agent::rpc` 映射 pin 单测（五类 RpcFailureKind→稳定码逐项钉住）+ `trust.rs` inode/mtime/device drift 纯函数单测（`provenance_drifted` 提取）；`rpc.rs` 原 15 单测回归绿。**TOCTOU**：落地 fallback 层（见 A3 卡注记），fd-exec 显式延期。文档：docs/commands/agent.md+zh-CN（trust/untrust 行 + 默认禁用说明）、AGENT_EXAMPLES banner、tests/INDEX.md 两行、agent.md 测试矩阵行。中途事件：远端 8cbc443（仓库元数据+并行半成品链接修复）合并冲突 13 文件全取 ours（守卫验证版）+ 2 处合并错改还原（0075c36）；ruleset bypass 由用户配置生效，积压 9 提交已全部推送。codex review 五轮（rescue 线程续跑）：R1 FAIL——3 缺陷（list 未门禁、trust 时不查 world-writable 父目录、timeout/transport/malformed-frame 误归 007）+ 6 测试缺口（gated-list 反向 pin、inode/mtime/device 单维漂移、trust 拒绝 world-writable、默认输出+JSON 双路脱敏、CLI 边界稳定码、invoke 仿冒路径）→ 修复：门禁收紧至 list/trust/invoke（untrust 豁免）、键名规范化 `agent.external_agents.enabled`、`record_trust` 内建 world-writable 拒绝、新分配 012 + 映射 pin 单测、`provenance_drifted` 纯函数提取 + 三维漂移单测、E2E 扩至 6 用例；**新测试当场揪出真 bug**——`rpc_failure_kind` 只查 `chain()` 漏掉 anyhow context-layer 形态的 marker（stdin write/flush、malformed-frame 路径全部降级 LBR-INTERNAL-001），改为先 `anyhow::Error::downcast_ref` 再 chain 兜底；顺手修 `utils::error` 默认退出码测试与 `LIBRA_FINE_EXIT_CODES` 翻转测试的进程级 env 竞态（读方补 `#[serial]`，实测偶发 3≠128）→ R2 FAIL（2 处陈旧注释：settings 表 gate 行、RpcFailureKind 变体 doc 的 007 残留）→ 修复 → R3 两项确认 + 新指 E2 prose 768-769 歧义 → 修复（关门/开门语义分层 + 012 键补入）→ R4 stale-read 误 FAIL（按记忆答复，未重读磁盘）→ R5 强制 fresh read 引用原文，四项全 RESOLVED，**VERDICT: PASS** |
| 2026-07-05 | Task A4（AG-19 lifecycle dispatcher + hook providers） | 完成 | 0.18.6 | 本行所在提交 | **lifecycle**：`SubagentStart`/`SubagentEnd` 末尾追加（`event_id()` 序数稳定 + 序数 pin 测试）+ `#[non_exhaustive]`；`subagent_events` metadata 投影（capped）；phase 机保活。**字符串桥删除**：`find_provider`/`provider_name_for`/`SUPPORTED_PROVIDER_NAMES` 全删，dispatch 一律 `AgentKind`→registry→`as_hooks()`（`StablePromotedSpec` 增 `hooks` 字段）；gemini 仅卸载通道改类型化单例直引 + 双 hook 入口（`hooks gemini`/`agent hooks gemini`）ingest-reject-with-hint；§775 rg 零命中。**dispatcher**：`agent.hook.ingest`/`agent.redaction.apply` spans（必带/禁止字段 fake-sink 3 用例，独立二进制）；unknown event name skip-and-log（`recognizes_event`，四 provider 名表）；owner filtering first-writer-wins（pre-upsert 快路径 + checkpoint 前 post-upsert 复确认，`ORDER BY rowid ASC` 保单调——race 回归测试实测抓到秒级时间戳 + 字典序 tiebreak 的非单调双写并修复，15 轮压测绿）；redaction 扩至 prompt/assistant_message/tool_input/tool_response。**providers**：codex（上游 codex-cli 0.142.4 实测 + rust-v0.142.4 源码逐字节核对：用户级 hooks.json + `[hooks.state]` trusted_hash 自算安装、位置键重算、config.toml 字节保持段编辑器（CRLF byte-for-byte pin）、trust-gap 计数 + SessionStart banner（双入口）、稳定调用面 `libra hooks codex <verb>` 路由 AgentTraces、SubagentStart/Stop 原生转发，单测 21）+ opencode（1.17.13 实测：`.opencode/plugin/libra-hooks.js` 标记托管插件、双目录去重、事件映射含 session.idle→TurnEnd Libra 侧推断标注，单测 11）；registry 行翻转 + `compat_agent_capability_matrix_pin` 同 PR 更新。**测试**：新增 5 个 target 共 18 用例（lifecycle 7 / redaction 3 / span 3 / crash 3 / install-path 2，全入 INDEX）+ `LIBRA_TEST_HOOK_PANIC_AFTER_READ` 测试旋钮（读取+校验后、DB 写入前）；`ingest_agent_traces_payload` 升 pub（注明非稳定 API）供 span 测试。**文档**：agent.md「OpenCode 安装流程契约」「Codex 捕获目标契约」同 PR 以实测证据重写（含 trusted_hash 算法、静默跳过、`--pure` 盲区、stdin 挂起注记），settings 表 gate 行、docs/commands/{agent,hooks}.md + zh-CN 同步。codex review 两轮（rescue 线程）：R1 FAIL——owner 非原子（真缺陷，两层修复）、CRLF 损坏（**实测反驳**：split('\n') 保 \r，pin 测试作证）、`agent hooks gemini` 仍可摄入（修复 + 双入口 pin）、三处边界测试缺口（补齐，其中 race 测试当场再抓到我第一版修复的非单调洞）→ **R2 VERDICT: PASS**（rowid 单调性论证经 sql/迁移与 clean 路径核验）。已知遗留（A5/A9 归置）：claude 安装 hook 仍走 AiIntent 面（`hooks claude`）而非 AgentTraces——两 ref 双入口漂移已在卡内记录 |
| 2026-07-05 | Task A5（AG-20 checkpoint export / lazy transcript IO / doctor / prune） | 完成 | 0.18.7 | c073fcc（step-0 v1 fixture，改 writer 前先行提交）+ 本行所在提交（实现） | **writer**：E4-libra 六 entry tree（metadata v2 +model / manifest 自描述 / events/lifecycle.jsonl E3 canonical / transcript/<agent_kind>.jsonl / redaction_report.json / content_hash.txt=sha256 前缀、覆盖域 metadata,lifecycle_events,transcript,redaction_report 按序拼接、分块不变量、reader 容裸 hex）；E5 chunker（50MiB+测试旋钮、.001 后缀、line-safe、单行超限硬错）；catalog 幂等（CAS→traces_commit 探测→INSERT ON CONFLICT 兜底）；metadata_kv in-flight marker（TTL 10min，stage a→d）；`agent.checkpoint.write` span。**migration** 2026070802（分页索引 + 非唯一 traces_commit 索引——唯一索引会 brick 历史重复行；编号须超既存 max 2026070801 非按日历）。**readers**：keyset 分页（默认 50/cap500/base64 v1:<ts>:<id>/next_cursor；排序 (ts DESC,id ASC) 配索引，EXPLAIN 双层断言无 SCAN/TEMP B-TREE）；show 布局分类 e4-libra/legacy-v1/unknown、manifest-first、transcript 仅 stat。**doctor**：三类检测+--repair（class-1 含 E4 sidecar 全图扫描 sweep_e4_checkpoint_objects；class-2 ref 走链+v1/v2 metadata 双解析+marker 豁免+幂等插入；class-3 ref-side 目标+manifest byte_len 尺寸（transcript payload 永不读取，无 byte_len 即跳过并提示修 manifest）+行漂移（错误 o_type/o_size）原地 UPDATE 修复；stale_catalog_row 不再抑制 class-3——单次 --repair 双修）；legacy-v1 计数豁免；gemini remnant 提示；`agent.doctor.repair` span。**prune+push**：live marker 全局 fail-closed + ref-vs-catalog fail-closed（提示 doctor --repair）逐 CAS 重试复查；prune 事务内保守独占 OID object_index 清理；`agent.clean.prune` span；`agent push` 记录已推 tip、`--force-rewrite`=force-with-lease Exact（无 tip fail-closed；跨机改写被 lease 拦截，E2E 证明）——方案 (a) 无新错误码。**测试**：13 个新/扩 target 用例群（export 10/reader 8/doctor 13/span 1+1/clean+push+checkpoint 命令组）全绿；孤儿 agent_checkpoint_test 接线（--list 行为用例证据）；migration pin 21→22（含 migration.rs:869 内嵌 pin）。**修复途中发现**：RpcAgent spawn ETXTBSY 竞态加有界重试；3 个 opencode settings chdir 单测补 #[serial]（未串行 chdir 是 lib 套件既有 flake 源——审计发现 rebase/push/publish/sparse/sequencer/layer/obliteration/util 约 21 处存量未串行 chdir 测试，遗留 A9 收编）。codex review 六轮（runtime 中途损坏：PATH libra 旧 schema + bwrap loopback EPERM → 改直连 codex exec + 预转储 diff 文件接地）：R1/R2 blocked→FAIL 1P1（doctor 漏 E4 sidecar 图）→修→R3 FAIL 2P1（class-3 被 stale 抑制致双跑修复；read_raw 读 transcript 尺寸违 metadata-first）→修→R4 residual（无 byte_len 的 transcript 仍会读）→修→R5 escalate（存量行 o_type/o_size 漂移不检不修）→修+回归→**R6 VERDICT: PASS 零发现**。质量门禁：fmt/clippy -D warnings/`cargo test --all` 154 target 全绿（TEST-EXIT 0） |
| 2026-07-05 | Task A6（AG-21 transcript intelligence / skill events） | 完成 | 0.18.8 | 本行所在提交 | **extract.rs（新）**：三 adapter 纯解析器（claude session JSONL / codex rollout JSONL / opencode JSON export）投影 prompts/model/token/modified-files/subagent/skill；`map_e6_token_usage_full` 返回 `E6TokenUsage{summary,api_call_count,subagent_tokens}` 消费全部 6 个 E6 冻结 wire key（cached=creation+read、total=input+output 计算、count/subagent 显式承载不丢弃）；E7 curated registry（claude `/review`,`/security-review`,`/simplify`；codex/opencode `/review`）。**adapter 接线**：`ClaudeCodeObservedAgent` 全能力面（analyzer/prompt/token/model/subagent/skill 六 `as_*` override + trait impl）；`StablePromotedAgent` 按 kind 门控只对 codex/opencode 暴露 prompt/token/model/skill（非首批 kind 保持 None，E9）；registry 行 `CLAUDE_CODE_CAPS`/`HOOKS_AND_TOKENS`，capability pin 一致性断言（row.capabilities==declared_capabilities()）守恒。**writer 集成（runtime.rs `build_extraction_metadata`）**：fail-open（无 adapter/无 transcript→present:false partial:true+warning；单 extractor 失败→warning+partial，其余仍记录）；**全部 transcript-derived 字符串落 metadata 前经 Redactor 脱敏**（model/modified_files/skill_events 递归 JSON 脱敏 + warnings 脱敏）；prompt 正文不落盘（只落 prompt_count）；raw transcript 仅在栈内、永不持久化；additive 于 metadata schema v2；api_call_count + 通用路径 subagent_token_usage 持久化（`!contains_key` 防对 claude 双写）。**测试**：`agent_transcript_intelligence_test` 6 用例（四规格名 + 通用 E6 count/subagent + 非首批零能力）；`extract.rs` lib 6 单测（E6 全键 pin + 各格式 + 空/垃圾 fail-open）；`agent_checkpoint_redaction_test` 新增 2 E2E（extractor warning 无 secret/owner/prompt + derived model/file_path 脱敏——真 hook ingest→checkpoint show --json 断言 token/prompt 缺席、partial:true、count-only warning、prompt_count only、字段脱敏后仍在证明过脱敏而非丢弃）；`runtime.rs` 新增 metadata-level 单测（build_extraction_metadata 通用路径 subagent_token_usage/api_call_count 持久化 + claude accessor 不双写）；fixtures 带溯源 MANIFEST（agent slug/CLI 版本/日期/构造法）。codex review 四轮：R1 FAIL 2 P1（derived 字符串 model/file_path 未脱敏落 metadata 泄密路径；E6 mapper 丢 api_call_count/subagent_tokens）→ 修（全 derived 字符串脱敏 + E6TokenUsage 全键 + E2E secret-in-derived 回归）→ R2 FAIL（通用路径 subagent_tokens 折入 summary 但 metadata 未持久化——codex/opencode 无 as_subagent_aware_extractor）→ 修（format_summary 块 !contains_key 守卫持久化 subagent_usage）→ R3 FAIL（要 metadata-level 回归而非仅 extract_codex 单测）→ 补 runtime 私有 fn 测试驱动 build_extraction_metadata → **R4 VERDICT: PASS 零缺陷**。质量门禁：fmt/clippy 全绿；`cargo test --all` 4 失败为 PR #423 上游 status/checkout/rebase 回归（clean init 现 list .libraignore untracked→exit-code 1，与 A6 触达文件完全不相交，§0.4 豁免）。并发注意：`orchestrator/workspace.rs` 的 in-flight 改动为并发 session 产物，本卡用显式 `libra add` 仅提交 A6 文件、不带入 |
| 2026-07-05 | Task A6.5（Agent 第一期本地三 Agent 采集 smoke） | 完成 | 0.18.9 | a66c4aa + c7f7635 | — | 待提交 | **实施**：`tests/agent_local_capture_smoke_test.rs`（3 个 per-agent `#[ignore]`+`#[serial]` test）+ driver `tests/harness/agent_local_capture.rs`（注册进 harness mod；顶层 tests/ 自动发现，无需 `[[test]]`——Cargo.toml 零改动）。§0.3 全流程固化：preflight（which + `--version` + 只读登录检查，evidence 只留 redacted boolean/exit code——`claude auth status` 输出含 email/org 从不落盘）、pinned `$LIBRA_BIN` 副本+sha256、per-agent 隔离（codex=隔离 CODEX_HOME，复制 auth.json + config.toml 剥离 `[hooks.state]`，真实 `~/.codex` 零写入；claude/opencode=真实 HOME + 项目本地 capture 配置）、user-config 预置+preinstall 快照、hook command pinned 绝对路径断言、真实会话（进程组 SIGKILL 超时；agent 非零退出仅 advisory——claude `--max-budget-usd` 撞帽后置非零为实测行为，门禁在 Libra 侧捕获断言）、session/checkpoint list、`checkpoint show` metadata-first（断言输出不含 prompt/回复正文）、`session show`、traces ref rev-list 可达、doctor findings 空、§0.3.5 卸载 smoke（语义恢复 vs preinstall 快照 + installed=false + 二次幂等 + 数据留存）、evidence 0700/0600 + redacted summary（默认全删，KEEP=1 留存并打敏感警告）。**真实验收（本机，最终二进制单次全量跑绿 193s）**：`LIBRA_RUN_LOCAL_AGENTS=1 LIBRA_LOCAL_AGENT_SET=codex,claude-code,opencode cargo test --test agent_local_capture_smoke_test -- --ignored --test-threads=1` → 3 passed（claude 2.1.201：state=stopped、2 checkpoints、transcript 14969B、extraction ✓；codex 0.142.4：transcript 33166B、extraction ✓；opencode 1.17.13：lifecycle-only——plugin envelope 无 transcript_path，按 agent.md「OpenCode 安装流程契约」pin `extraction.present=false`+空 transcript；三登录检查 exit 0）。**smoke 实测暴露并修复的捕获链缺陷 3 处**：(1) 安装面 `hooks claude` 仍路由 AiIntent → 真实 claude 会话零 `agent_session`（A4 已记录的双入口漂移；改 AgentTraces，`src/command/hooks.rs`，同步 `docs/commands/hooks.md`；362f0f7a 独立诊断同一 bug 并用新二进制复验通过）；(2) transcript 信任门不认 `$CODEX_HOME`（`runtime.rs::transcript_path_within_provider_root`）→ 重定位 CODEX_HOME 的 codex 会话被静默捕成空 transcript——修复 + lib 回归测试 `codex_transcript_root_honors_codex_home_override`；(3) `agent doctor` provider_hooks 只探 claude/gemini（A4 前旧实现），codex/opencode 恒 `installed:null`——改 `spec.hooks` 探测（`doctor.rs`）。免费预探（invalid-model claude -p）证实 2.1.201 `-p` 未信任目录下项目 hooks 照常触发，未动 `~/.claude.json`。门禁：fmt / clippy `--all-targets --all-features -D warnings` / 裸跑 skip 路径（3 ignored；无 env `--ignored` 3 skip-pass）/ 受影响 slice（lifecycle 7、redaction 5、doctor repair 13、install-path 2、compat matrix/docs/capability pin、lib hooks 106）全绿。文档：agent.md 测试矩阵 A6.5 行改已注册、`tests/INDEX.md` 新增 Wave 7 行。发布闭环（版本 bump/commit/push）留待提交人执行 |
| 2026-07-05 | Task A6.5 协作说明（session 362f0f7a） | 让渡给 d59de36b | — | — | 双认领收敛：本 session 与 d59de36b 均实施 A6.5；本 session 独立诊断出 claude 采集真 bug（安装的 `hooks claude` 路由 `HookTarget::AiIntent` 而非 `AgentTraces`，致 `agent session/checkpoint list` 看不到 claude 采集；codex/opencode 已用 AgentTraces），d59de36b 已在工作树应用同一修复（`src/command/hooks.rs` claude 分支→AgentTraces），本 session 用新构建二进制实测确认修复后 `hooks claude` 采集成功（1 session claude_code）。A6.5 harness 最终以 d59de36b 版本为准。d59de36b 完成实现+全绿（2444 passed/4 PR#423 豁免失败/fmt+clippy 绿）后显式让出发布闭环（§10 上一行「留待提交人执行」）并 idle ~2h；本 session 作为提交人完成发布闭环（bump 0.18.8→0.18.9、release build、commit -a -s、push、部署）。codex live smoke：codex 单跑已 PASS（session/checkpoint/traces/doctor/卸载幂等/数据留存）；claude budget cap（fable-5 ~$0.13/turn>$0.05）致退出码非零已改为 advisory（Libra 侧捕获为硬门禁，§0.3.4 口径） |
| 2026-07-05 | Task A7（AG-22 read-only agent review workflow） | 完成 | 0.18.10 | 本行所在提交 | **engine**（src/internal/ai/review/{store,launcher,sink,runner}.rs）：ReviewRunStore（agent-runs/<run_id>/ 下 state.json/manifest.json[恰好 12 个 E8 键，manual_attach 空占位]/findings.md/reviewers/<slug>.{stdout,stderr}.redacted.log；keyset (created_at DESC,run_id DESC) 分页；run_id 路径穿越防护）；launcher §0.3.2 argv 逐字（codex --sandbox read-only/claude --permission-mode plan/opencode 非危险；禁用 flag 永不出现；单测钉死）+ env_clear 仅 PATH/HOME + 三管道 piped + ETXTBSY 重试 + process_group + 启动即录 pid/pgid/proc_start_ticks(/proc stat 22 域，敌意 comm 解析单测)；fan-in→串行 sink（64KiB/sink 上限、超限截断标记、洪泛不阻塞兄弟——1MiB e2e）；5 terminal states 聚合真值表；cancel/SIGINT/SIGTERM 共用 ReviewCancelHandle 清理（组杀→drain 有界收束：子进程退出后 3s 宽限→pgid 组杀→2s→abort readers——后代继承管道不再挂死，回归测试钉死）；孤儿 cancel 安全（组杀仅在 start_ticks 精确匹配时执行，否则 stale_unsafe 报告不杀；workspace 删除双闸 fail-closed：名形 + canonicalize 限定 store 派生 tasks 基底 + symlink 根一律拒绝[含基底内→基底内受害者用例]）；agent.review.run span（run_id/agent_count/terminal_state/duration_ms，禁 reviewer stdout）。**seam**：materialize_isolated_workspace 提为 pub（强制 reviewer 隔离路径；copy 后端钉死 gitignore 排除语义，FUSE 强制关闭）；ReviewerCommand 无 cwd 字段——生产/测试一律 current_dir=隔离 workspace。**launchable 语义**：registry launchable_review 首批三 agent 翻 true（investigate 留 false 待 A8），launcher/runner/CLI 预检统一 launchable_review_slugs() 单一事实源，capability pin 扩展正向断言。**CLI**（src/command/agent/review.rs，顶层 Commands::Review）：§5 全家族 + 复用 AG-20 分页助手（默认 50/cap500/不透明 cursor/schema_version envelope）+ show 一律 render_untrusted_findings（ANSI/控制序列剥离）+ spotlighting 定界 prompt（含定界符伪造防护单测）+ --fix fail-closed LBR-AGENT-010 + **--checkpoint fail-closed**（checkpoint 物化未实现，拒绝在 checkpoint 标签下静默 review 当前工作区——codex R4 裁定；transcript-grounded checkpoint review 为显式后续项，A9 release notes 需说明）+ 孤儿 cancel 诚实报告（killed/stale_unsafe/workspace_action，人读+JSON）。**compat**：COMPATIBILITY.md 行/ROOT_AFTER_HELP 行/REVIEW_EXAMPLES+banner/docs 四件套含 Examples 节+zh-CN/README 表行，五 compat guard 全绿。**tests**：agent_review_workflow_test 7 用例（agent.md :1765 钉名 5 个 + cancel-during-flood 压力 + CLI 分页三页游走）+ agent_review_span_test + fixtures agent_workflows/（POSIX sh 假 reviewer 六件 + 溯源 README）+ 引擎内 30 单测；review_fix_bridge_enters_agent_runtime_mutating_path 按矩阵注记待 fix bridge 锚点后补。codex review 五轮：R1 FAIL 3P1+1P2（drain 挂死/孤儿 cancel 空头承诺/launchable 门禁错源/cwd 逃逸面）→修→R2 FAIL 2P1（workspace 删除仅名形校验可删任意同名目录/pgid 复用误杀）→修→R3 FAIL 残留（基底内 symlink→基底内受害者）→修→R4 FAIL 升级（--checkpoint 静默错内容）→fail-closed+文档→**R5 VERDICT: PASS 零发现**。门禁：fmt/clippy -D warnings/引擎+CLI+dispatcher 单测/两 test target/五 compat guard/root_after_help 全绿（并行 session 施工期间以 target 级门禁替代全量跑——全量跑存在跨 session 编译竞态，见 A5 行审计注记） |
| 2026-07-06 | Task A8.5（AG-24a 合规实现面） | 完成 | 0.18.12 | 8e42203+7f4dc0c+a51a94e+045deaf+ed7b46e+文档提交（本 session 分片提交，绕开与并发 A7/A8 的 mod.rs 冲突） | **迁移**：`sql/migrations/2026070803_agent_audit_log`（append-only：BEFORE UPDATE/DELETE 触发器 RAISE(ABORT) 拒改删，仅 INSERT/SELECT；`_down` 不 DROP/DELETE，改装 BEFORE INSERT freeze 触发器只停写不删数据，forward 先 DROP freeze 支持 up→down→up；migration.rs 注册 + count 22→23/max 2026070803；db_migration_test 版本列表更新 24 绿）。**compliance.rs（新模块）**：retention 配置 getters `agent.retention.{transcript_days=90,stderr_days=30,findings_days=90}`/`agent.max_transcript_read_bytes=256MiB`（缺省 fail-safe + 正整数校验）；`AuditRecord`+`write_audit_record`（INSERT-only append）+`AuditScope`。**erasure**：`HistoryManager::erase_session_local` 三面一致（先 prune checkpoints 重写 refs/libra/traces+删 agent_checkpoint 行+清 object_index，再删 agent_session 行——顺序关键：先删 session 会 FK cascade 掉 checkpoint 使 ref 出孤儿）；`SessionEraseOutcome`；一致性测试（删/留/audit 存活）。**clean --gc**：`CleanArgs --gc/--retention-days`（A7 提交时收编入 mod.rs）；`gc_expired_checkpoint_ids` 按 created_at<now-transcript_days*86400 跨所有 scope 选取 stopped-session checkpoints，复用 prune 引擎，永不触碰 agent_audit_log；2 GC 测试（跨 scope 过期+窗口保留+audit 存活；--retention-days 覆盖+0 拒绝）。**checkpoint export --allow-raw**：`CheckpointExportArgs`+`export` 函数——`--raw` 无 `--allow-raw`→fail-closed 拒绝 `LBR-AGENT-013`+audit 记 granted=0；`--allow-raw --raw`→audit 记 granted=1+返回存储 transcript body（E4-libra manifest 导航 single/chunked blob 读取+max_transcript_read_bytes 截断）；默认 redacted 路径无需授权无 audit；身份从 committer env 解析（非 checkpoint 硬编码 Libra committer）；`allow_raw_gate` E2E 测试（三态门禁+审计）。**注意 P0**：transcript blob 采集时即脱敏，故 raw 导出返回 capture-redacted 内容而非未脱敏原文——门禁价值是审计+授权而非未脱敏内容（测试断言据此）。**错误码**：`StableErrorCode::AgentRawAccessDenied=LBR-AGENT-013`（as_str/category(Internal exit128)/description/双 pin 测试 + docs/error-codes.md 双表 + agent.md E10 表 + compat_error_codes_doc_sync 绿）。**deferred**：findings-GC（`agent.retention.findings_days`）前置 A8 investigate run-state 结构未成，按卡显式 deferred，A9 release notes 说明。**门禁**：fmt/clippy --all-targets --all-features -D warnings 全绿；audit_log 3、db_migration 24、export gate 1、clean gc 2、erasure 1、compliance 单测 2、error pins 42、compat（help_examples_banner/command_docs/error_codes_doc_sync）全绿。**codex review**：R1 FAIL 3 P1（export 门禁在 row-load 之后→存在性 oracle+漏审计；wants_raw=raw||allow_raw 使 --allow-raw 单独触发 raw；size cap 在全量解压后才生效）→修（门禁前置于 lookup+审计、wants_raw=raw&&allow_raw、新 read_git_object_bounded 有界解压）→R2 FAIL 新 P1（manifest.json/tree 仍走无界 read_git_object）→修（read_tree_object+双 manifest+load_metadata_blob+content_hash 全改有界，checkpoint.rs 零无界 read_git_object）→R3 FAIL 新 P1（content_hash 忽略 truncated 可 format_valid:true）→修（truncated→unreadable）→R4 FAIL 新 P1（bounded reader 固定 64B header slack，header 长于 64B 时 truncated 误报 false）→修（header 逐字节读+硬上限，content 读 max+1 判截断，与 header 长度无关+单测 pin）→R5 VERDICT: PASS 零缺陷（0.18.11 首发后修复 5 轮，作为 0.18.12 补丁发布）。7 项关键属性 + bounded-read 全部测试背书（bounded_read 2、reader 8、export gate 3、erasure 1、clean gc 2、audit_log 3、error pins 42）。文档：agent.md E10 行+settings 键、docs/commands/agent.md+zh-CN（export/gc 行+flags）、tests/INDEX.md 2 行。并发协作：A7/A8（session d59de36b）同 tree 推进，mod.rs 的 CleanArgs/CheckpointExportArgs 分别经 A7 收编与 ed7b46e 落地；本 session 全程显式 add 分片提交避免收编他人 in-flight（review/investigate）|
| 2026-07-06 | Task A8（AG-23 read-only agent investigate workflow） | 完成 | 0.18.12 | 本行所在提交 | **engine**（src/internal/ai/investigate/{store,runner,mod}.rs，新建 sibling 模块——strict round-robin 与 A7 并发 fan-in 本质不同，按 import 复用 A7 的 launcher/spawn/sink/redaction/render_untrusted_findings/隔离 seam/分页助手，唯一对 A7 的改动是 review::store::read_json_opt 提 pub(crate)）：InvestigateRunStore（agent-runs/<run_id>/ 下 state.json[run_id/topic/agents/quorum/max_turns/next_agent_idx/turn/completed_rounds/pending_turn/stances/findings_doc/starting_sha/started_at/updated_at]/manifest.json[恰好 12 个 E8 键，kind=investigate，manual_attach 空占位]/findings.md/reviewers 日志；run_id 路径穿越防护；keyset (created_at DESC,run_id DESC) 分页）；strict round-robin 回合引擎（逐 agent 顺序、跨轮 next_agent_idx 环绕；quorum=≥N 个 distinct agent 提交 concluding stance[stdout 含 conclud]，默认=agent 数即共识，clamp[1,agents]；max_turns 默认 6）；terminal（quorum/max_turns/cancelled/timeout/error）vs paused（stalled 空输出 / agent_failure 启动失败或非零或超时——pending_turn 记录待续回合）；run-id flock 独占（<run_dir>/.lock，LOCK_EX|LOCK_NB；同 run 并发 continue→RunLocked fail-closed）；untrusted seed/stances 走 Redactor + spotlighting 定界注入每回合 prompt，show 前 ANSI 剥离；run-level 超时 min(max_turns*120s,3600s) 从持久化 started_at 度量（跨 continue 累计不重置；corrupt started_at→fail-closed timeout）；`agent.investigate.run` span（run_id/turn/next_agent_idx/terminal_state，禁 seed/reviewer stdout）。**CLI**（src/command/agent/investigate.rs，顶层 Commands::Investigate）：start/list/show/continue/cancel/clean/fix；list 复用 AG-20 分页（默认 50/cap500/不透明 cursor/schema_version envelope）；show 一律 render_untrusted_findings；clean 镜像 A7（--run 拒非 terminal、锁优先；--all 跳过非 terminal 只删 terminal）；fix→LBR-AGENT-010，untrusted-seed-mutation→LBR-AGENT-011（预置，消息说明只读可用+fix 前置）。**launchable 语义**：registry launchable_investigate 首批三 agent 翻 true，capability pin 扩展。**compat**：COMPATIBILITY.md 行/ROOT_AFTER_HELP 行/INVESTIGATE_EXAMPLES+banner/docs 四件套含 Examples 节+zh-CN/README 表行/agent.md 派生行，五 compat guard + root_after_help 全绿。**tests**：agent_investigate_workflow_test 11 用例（agent.md :1766 钉名 5 场景 + 分页 + 5 个 codex P1 回归）+ agent_investigate_span_test + fixtures agent_workflows/investigator-*.sh（4 件 POSIX sh + 溯源 README）+ 引擎内 25 单测。codex review 三轮：R1 FAIL 3P1（pending_turn 先于锁被清/clean 删除 paused run/超时预算每次 continue 重置）→修→R2 FAIL 3P1（clean corrupt 分支绕锁/continue 锁后未复检 terminal 的 TOCTOU/corrupt started_at fail-open 逃避预算）→修（锁优先删除+锁后 terminal 复检 AlreadyTerminal+elapsed Option 化 fail-closed timeout）→**R3 VERDICT: PASS 零发现**。门禁：fmt/clippy -D warnings/引擎+CLI 25 单测/两 test target 12/五 compat guard/root_after_help 全绿（并发 A8.5 session 施工期间以 target 级门禁替代全量跑，见 A5/A7 行注记）；显式 libra add 仅暂存 A8 文件避免收编 A8.5 未提交工作 |
| 2026-07-06 | Task A9（AG-24 closeout） | 完成 | 0.18.14 | 本 session 分片提交（docs 收敛 + login --json token 泄漏安全修复） | **验证守卫全绿**：compat_agent_docs_contract / agent_run_non_exhaustive_guard / matrix_alignment（7）/ error_codes_doc_sync（2）/ help_examples_banner / command_docs_examples_section；两条否定断言（旧 `../agent.md` 等链接零命中、`src/internal/ai/claudecode` 代码树零命中）成立；`cargo test --all` deterministic 绿（2 处 cloud restore 单跑绿的已知并行 flaky）。**修复的跨特性漂移**：account-login 特性（527a10c，非 AG-* 任务）落地 `libra login/logout/whoami` 但未接线 compat——补 COMPATIBILITY.md 3 行 + docs/development/commands/README.md 公开命令行 + docs/commands/README.md Remote Operations 索引 + docs/commands/{login,logout,whoami}.md 用户文档（各带 Examples）+ src/cli.rs ROOT_AFTER_HELP Remote-And-Cloud 组 + _compatibility.md LFS/account 行（移除已实现项）。**out-of-scope banner**：memory/sandbox/web-api 三 tracing 草案 + account.md 补 §0 out-of-scope banner，各注明与实现的已知冲突（LifecycleEventKind 11 vs A4 13、mcp --stdio vs C6 code --stdio、web /api/v1 vs C4 /api/code/*、sandbox VM/AppleContainer 后端不并入 C7、account.md 会话存储 account.host.<sha256> 非 vault.account.hosts.*、端点 /api/cli/*、whoami 无离线回退 + --refresh no-op、logout 撤销失败不删本地）。**codex review 揪出真安全 bug 并修复**：`libra login --json/--machine` 序列化整个 `AccountSession` 含 bearer `session_token` 泄漏到 stdout/日志——新增 redacted `LoginJsonView`（省略 token）+ 回归测试 `login_json_view_omits_session_token`；whoami/logout 输出无 token。codex review：R1 FAIL（whoami --refresh/memory/web-api 三 banner 不准）→R2 FAIL（sandbox banner/login --no-browser）→R3 FAIL（account docs "used by cloud/publish"）→R4 FAIL（同上未真正应用）→R5 FAIL（_compatibility.md 仍称未实现）→R6 FAIL（account.md 更多 flags/endpoints/storage 漂移 + README 索引缺）→R7 FAIL（我的 whoami/logout 用户文档回退行为错 + account.md 更多）→R8 FAIL（account.md 20+ 处 design-spec 漂移）→R9 FAIL（account.md banner-scope out + 揪出 login --json token 泄漏）→R10 VERDICT PASS 零缺陷。fmt/clippy 全绿。account.md 内部 design-draft 漂移经 §0 out-of-scope+known-drift banner 治理（同 memory/sandbox/web-api 处置），其完整刷新属 account 特性 owner。 |
| 2026-07-06 | Task C1（`libra code` source-grounded audit） | 完成（只读审计，无代码变更/无独立版本） | — | 差距清单见本行 | 只读 source-grounded 审计完成，产出 12 项分类差距清单（3 code behavior：GAP-1 web-only 拒绝非 Gemini provider 使已建的 7-provider headless web + Codex web 分支 CLI 不可达 / GAP-2 web-only 拒绝 --resume 尽管 headless resume 已实现 / GAP-3 web-only 拒绝 --model/--api-base/provider flags；7 docs drift：GAP-5 run_libra_vcs allowlist 文档少列 show-ref/ls-files 且反向禁用 ls-files / GAP-6 --goal 未文档化 / GAP-7 --agent 未文档化 / GAP-8 --approval-ttl 未文档化 / GAP-9 --kimi-stream 未文档化 / GAP-10 allow-all approval 值未文档化（four-tier 实为五档）/ GAP-12 zh-CN 路由/审计矩阵滞后；1 test gap：GAP-4 web-only provider/resume 拒绝无回归覆盖；1 borderline：GAP-11 --plan-mode 默认 off vs Codex 有效默认 true）。按下游 C-task 分组：C2=GAP-1/3/4，C5=GAP-2，C6=GAP-5 linkage，C8=GAP-5/6/7/8/9/10/11/12。验证 grep/--list 已跑。RE-VERIFY：无 gap 读 INDEX.md/COMPATIBILITY.md 内容，A9 并发编辑不失效；仅 C8 收尾须按 A9 最终 HEAD 复核 code/code-control 行。核心决策 GAP-1 经 codex plan-review 批准为 code behavior（放宽 web-only provider），已在 C2 落地 |
| 2026-07-06 | Task C2（Mode & argument contract hardening） | 完成 | 0.18.13 | 本行所在提交 | 落地 C1 GAP-1/3/4（codex plan-review 批准的 code-behavior 决策）：将共享 reject_non_tui_flags 拆为 mode-aware（--web 传 web_only=true / --stdio 传 false），web-only 放宽 provider(非 Gemini)/--model/--api-base(非 codex)/--temperature/匹配的 provider-specific flags，--stdio 保持全锁定；保留 web-only 拒绝 --resume（→C5）/--env-file（headless 仍传 CodeEnvFile::default() 延后，代码注释标注）/--network-access allow（安全门）；跨 provider flag-match 门（code.rs:4020-4082）不变——不匹配 provider flag 仍拒绝，顺序验证（match 门在放宽的 blanket 之后运行）；--api-base 在 codex 下仍拒绝；codex-only flags 仍限 codex。新增 mode-independent --temperature 校验（finite + 0.0..=2.0，拒 NaN/inf/越界——codex R1 P1，放宽后 temperature 直达 headless ToolLoopConfig）。测试：code_cli_dispatch_test +2，code.rs 单测新增 web-only accept 矩阵（7 provider + model/api-base/temperature + 匹配 deepseek/kimi/ollama flag）/ web-only reject（mismatched flag/--resume/--env-file/--network-access/codex --api-base）/ stdio 保持锁定（provider/model/api-base/provider-flag 各一回归）/ temperature 越界拒绝含边界接受，card-named 两测仍过。docs：tracing code.md:23/72 记录已解决决策，commands/code.md+zh-CN web-only --resume 标注延后 C5（示例保留不删）。drive-by：修 merge 带入的 account.rs 两处 clippy 错误（needless_as_bytes/redundant_closure，纯 lint 无行为变更）。codex review 两轮：R1 FAIL 1P1（temperature 无范围校验）+2P2（resume 文档行/测试矩阵缺 deepseek+kimi）→修→R2 VERDICT: PASS 零发现。门禁 fmt/clippy -D warnings 全 --all-targets/code_cli_dispatch 11/code:: 单测全绿 |
| 2026-07-06 | Task C3（Provider/runtime bootstrap and env handling） | 完成 | 0.18.15 | 本行所在提交 | 验证型卡（C1 判定 linkage-only）。6 项验收：criterion 3（agent profile override 无 silent fallback，effective_code_provider_for_args 对未知 binding provider 显式报错、binding 原子胜过 --model）与 criterion 5（live tests 已 env-gate）判定 satisfied-as-is 无需改；criterion 1/2/4 行为正确但有测试/消息缺口，最小补齐（仅 src/command/code.rs + Cargo.toml + 一处 drive-by）。**criterion 1（provider factory 默认）**：新增 build_helper_defaults_model_id_per_provider（8 provider 默认 model + Ollama 需 --model）；api-base 规则从内联 match 抽出纯 helper resolve_provider_api_base（行为等价）+ 表驱动测试 resolve_provider_api_base_matches_per_provider_rules 钉死每 provider：openai/anthropic/kimi/zhipu/ollama 回退各自 *_BASE_URL（含 CLI --api-base 优先 + 跨变量不泄漏），deepseek/gemini CLI-only，codex/unknown→None 即便有 CLI flag；env-file base-url 路由测试。**criterion 2（--env-file 优先级）**：优先级 env-file>进程>vault 满足；缺 key 错误消息补「如何配置」（export {var} 或 libra config --global add vault.env.{var}）+ 测试断言。**criterion 4（Codex preflight）**：file-cwd 拒绝已有测试；抽 wait_for_codex_ready_within(ws_url,timeout) 使 WebSocket 超时诊断可测（生产 wait_for_codex_ready 委托 CODEX_STARTUP_TIMEOUT），新增 codex_ready_probe_times_out_with_human_readable_diagnostic 断言诊断含 app-server + ws url。**criterion 5（live 隔离）→ codex R1 P1 升级**：ai_agent_test/ai_chat_agent_test/ai_ollama_live_gate_test 原仅 env-gate；按 CLAUDE.md L3 约定（env-gate AND test-live-ai feature）在 Cargo.toml 加 [[test]] required-features=["test-live-ai"]，裸 cargo test --all 完全排除（DEEPSEEK_API_KEY 已设也不打真实 API），--features test-live-ai 仍可跑+env-skip。**drive-by**（解锁共享全量门禁）：并发 A8.5 加迁移 2026070803（audit_log）但漏更 agent_capture_migration_test 三处硬编码迁移列表（rollback newest-first / run_pending oldest-first），补入 2026070803 使 8/8 绿（迁移本身真实已注册，db_migration_test 已计数）。codex review 两轮：R1 FAIL 2P1（api-base 仅测 OPENAI 需全 8 provider 表驱动；live tests 仅 env-gate 需 feature-gate）→修（抽 helper+表测+required-features）→R2 VERDICT: PASS 零发现。门禁 fmt/clippy --all-targets --all-features -D warnings/code:: 83 单测/三 code 验证 target/agent_capture_migration 8 全绿；live ai_agent_test 加 feature 后仍绿（真实 DeepSeek key）|
| 2026-07-06 | Task C7（Sandbox, approval & tool gate consistency） | 完成（session 0948cdf1 收编——原认领 362f0f7a 停滞 2.5h+） | 0.18.19 | 本行所在提交 | 验证型卡（C1 未标 C7 代码缺口）。逐 criterion：**(1) CodeContext 默认**：default_tui_runtime_context 映射 Review/Research→SandboxPolicy::ReadOnly、Dev/None→WorkspaceWrite；default_tui_runtime_context_is_read_only_for_review_and_research + 两 Dev network 测试 pin——satisfied-as-is。**(2) approval/network→ToolRuntimeContext**（GAP-FIX）：--approval-policy/--approval-ttl/--network-access 经 args→ToolRuntimeContext 映射（TUI run_tui_with_model_inner + headless 两 launch path），shell/apply_patch 在 invocation 读 sandbox+approval。原测试只观察 network 轴与 allow-all store，未测 ToolApprovalContext.policy/.approval_ttl。**codex review 三轮**：R1 FAIL 1P1（新测试未测 CLI→runtime 路径）→改为解析 CLI args→R2 FAIL 1P1（仍在测试内复制生产映射表达式，不能抓生产丢弃/硬编码 flag）→**抽出生产共享 helper tui_approval_config_from_args(args, working_dir)→DefaultTuiApprovalConfig 作为 args→approval-config 映射单一事实源，TUI+headless 两 launch path 均改走它**（行为等价：policy.into()/ttl CLI>project>DEFAULT/allows_all_commands/cache_policy），测试解析 CLI args 经该生产 helper 断言 policy==UnlessTrusted/ttl==4242s + 默认回退 300s 控制→R3 VERDICT: PASS 零发现。**(3) tool ACL 四档**：read-only allowlist（is_read_only_or_semantic_tool）/workspace-write+broad-mutating（apply_patch/shell/submit_* 经 filter_by_intent 在 Review/Research 丢弃）/network（requires_network+ensure_network_allowed 读 sandbox network_access）；code_tool_acl_test 6 pin——satisfied-as-is。**(4) review/investigate fix unsupported**：源锚点 review.rs/investigate.rs LBR-AGENT-010（AgentFixBridgeUnavailable）+ A7/A8 集成测试（exit 128/JSON error）——satisfied-as-is，无 fix bridge。**(5) 生产无 unwrap/expect**：compat_all_production_unwrap_guard 原样绿（新 expect 在 #[cfg(test)]）——satisfied-as-is。门禁：fmt/clippy --all-targets --all-features -D warnings/code:: 92/code_tool_acl_test 6/ai_subagent_worktree_readonly_test 3/compat_all_production_unwrap_guard 1 全绿；code_ui_remote_approval_matrix 11/3——3 个非绿是「approved shell 须在 bwrap 沙箱内执行成功」的用例，本机缺 CAP_NET_ADMIN（bwrap --unshare-net RTM_NEWADDR EPERM，raw bwrap 复现，测试文件未改，不在 CI compat-offline-core test-provider 列表），非 C7 缺陷，approval 决策逻辑由 11 个通过用例全覆盖。全量 cargo test --all 留待 C8 终审（并发争用致 spawn-based 测试饥饿，见 C4-C6 行注记） |
| 2026-07-06 | Task C4（Web-only, Code UI, control and SSE contract） | 完成 | 0.18.16 | 本行所在提交 | 验证型卡（GAP-1 reachability linkage）。**可达性核心发现**：C2 放宽 web-only provider 校验后 DISPATCH 层无真实缺口——execute_web_only 分支 Codex→managed app-server（start_managed_codex_server+start_codex_code_ui_runtime）、否则→build_non_codex_headless_runtime，后者 match 穷尽处理 Gemini|Openai|Anthropic|Deepseek|Kimi|Zhipu|Ollama（Codex→Ok(None)），无 wildcard，物理上不会静默误路由。按卡「dispatch 已正确则补回归」：新增可测 seam enum WebOnlyRuntimeKind{ManagedCodexAppServer,Headless} + web_only_runtime_kind(provider) 穷尽 match（Codex→managed，其余含 cfg(test-provider) Fake→headless；新 provider 变编译期路由决策），execute_web_only 改读该分类器为单一事实源（语义等价，body 仅缩进）；2 测试 web_only_runtime_kind_routes_each_provider_to_its_runtime（8 provider）+ build_non_codex_headless_runtime_excludes_codex_provider（Codex→Ok(None)）。仅改 src/command/code.rs。**其余 5 项 satisfied-as-is**：observe-only 路由（/session /events /diagnostics /threads /goal/status 均 GET+ensure_loopback_api_request，只读快照，路由集匹配文档）；browser-control 矩阵（resolve_browser_control_mode loopback-only + codex-web-only→Loopback[C2 后可达]/非 Codex→Off，browser_control_resolution_matrix pin）；control token（code_control_files 0600 + CONTROL_INSTANCE_CONFLICT，三测试）；no-leak（CodeUiDiagnostics 无 secret 字段 + SecretRedactor::default_runtime，ControlInfo 无 token 料，inline + security matrix）；TS wire mirror（web/src/lib/code-ui/types.ts 匹配 Rust wire，ai_code_ui_wire_test 9 golden）。codex review R1 VERDICT: PASS 零发现。门禁：fmt/clippy --all-targets --all-features -D warnings/code:: 85 单测/ai_code_ui_wire 9/ai_code_ui_projection 2/（test-provider+LIBRA_ENABLE_TEST_PROVIDER=1 --test-threads=1）code_ui_remote_sse 14、security 13、lease 17 全绿；state_matrix 15/1——唯一非绿 state_cancel_while_executing_tool_settles_running_tool_call 系本机 bwrap 网络命名空间受限（RTM_NEWADDR EPERM，raw bwrap --unshare-net 复现，TUI-mode 与 C4 改动无关，需 CAP_NET_ADMIN 的 CI runner），非 C4 回归，记为环境跟进项。**完整 `cargo test --all` 环境阻塞说明**：发布时并发 peer session（362f0f7a）持续跑重型 cargo 使机器 load 12-21，其 build-lock + CPU 争用令我方全量 suite 的并行 spawn-based 命令测试（fsck/shortlog）在 ~101 处饥饿停滞，5 次尝试均同样被环境击败；关键：全程 **0 test FAILED**（仅无法完成，非失败），fsck/shortlog 直接单跑均通过（test_fsck_missing_object 9.67s、shortlog 19/19），C3 同一 command_test 二进制的全量 suite 数小时前已 158 ok 通过。C4 系纯 additive（分类器 enum+fn+2 测试+行为等价缩进，codex 确认 behavior-preserving），其代码由 code:: 85 单测全覆盖。据 goal 门禁「actual failure 才不可接受，L2/L3 skip 可接受」口径 + 逐 target 全绿 + codex PASS，判定 C4 代码正确、按 rule-4 以代码分析确认完成；全量 suite 待并发 session 空闲后可复跑验证（预计与 C3 同样通过） |
| 2026-07-06 | Task C5（Session resume, graph handoff and persistence） | 完成 | 0.18.17 | 本行所在提交 | **GAP-2 决策落地**：按卡 :1309 + tracing code.md:48 保持 --resume TUI-only（web-only/stdio 均拒绝，不放宽），把 C2 遗留的「deferred to C5」文档改写为「TUI-only by design（永久契约，非延后）」。逐 criterion：**(1) --resume TUI-only + 错误测试**：web-only reject 已有 rejects_tui_flags_in_web_mode（消息含 flag+--web+remove），补 stdio 侧 rejects_resume_in_stdio_mode；code_resume_test（test-provider）4 真实 PTY 用例（happy/SIGTERM-mid-turn/unknown-uuid/unknown-non-uuid）全绿。**dead code**：load_or_create_headless_web_session_state 的 resume 分支经 --resume upstream 拒绝后 CLI 不可达，但 create 分支载荷可达——保留+文档注明「intentionally unreachable，TUI-only by design」（移除需重塑 helper 无收益）。**(3) graph handoff --repo 提示**（GAP-FIX）：TUI exit 打印原只发 `libra graph <id>` 无 --repo，违 code.md:24 承诺；新 format_graph_handoff_hint（session_working_dir≠cwd 时 canonicalize 比较后追加 --repo，cwd 未知时 fail-safe 加 --repo）+ shell_quote_for_display（含空格/shell 元字符时 POSIX 单引号，`'`→`'\''`，否则裸值）+ 5 测试（同目录裸/远程加 --repo/未知 cwd 加 --repo/引用 helper 纯测/含空格路径加引号）。**(4) JSONL reader**：parse_session_event_value 跳过未知/缺 kind（Ok(None)），load_events 容忍尾行 truncated（无尾换行→warn+break）但对完整畸形行仍报错——ai_session_jsonl_test 4 绿。**(5) projection bundle identity**：run_tui_with_model_inner 按 canonical thread id 载 bundle，build_tui_code_ui_runtime 用 bundle identity，仅 None 时回退临时 session.id——tui_code_ui_runtime_prefers_projection_bundle_identity + ai_code_ui_projection_test 绿。**(6) audit sink**：web/mod.rs 经 runtime AuditSink 发 local-tui-control:<kind>:<client>/policy_version=local-tui-control/v1 的结构化 redacted ControlAuditRecord（非 transcript），TUI 路由 TracingAuditSink——InMemoryAuditSink control-attach 测试 pin。文档：code.md:117/241 + zh-CN + tracing code.md:26/75 全部从「deferred/lands later」改为「TUI-only by design」，移除 web-only --resume 示例。codex review 两轮：R1 FAIL 2P2（--repo 路径未 shell-quote 破坏含空格路径复制粘贴；tracing code.md 第二处 stale「web-only --resume→C5」）→修（shell_quote_for_display + 2 测试 + 修 stale 行）→R2 VERDICT: PASS 零发现。门禁：fmt/clippy --all-targets --all-features -D warnings/code:: 91 单测/code_resume_test 11（test-provider）/ai_session_jsonl 4/ai_code_ui_projection 2/ai_goal_resume 3 全绿 |
| 2026-07-06 | Task C6（MCP stdio and code-control boundary） | 完成 | 0.18.17 | 本行所在提交 | 验证型卡（C1 判定该面已一致）。逐 criterion：**(1) `libra code --stdio` = MCP-only 不控 live TUI（satisfied-as-is）**：execute_stdio（code.rs:4076-4103）只 init_mcp_server + serve_server(AsyncRwTransport stdio)，无 TUI/AgentRuntime attach；`--control write` 在 `--stdio` 下被 validate_mode_args 拒绝并指向 `code-control --stdio`（code.rs:4160-4166）；unit pin：rejects_control_write_in_stdio_mode（断言含「code-control --stdio」）、stdio_mode_stays_provider_locked、rejects_web_flags_in_stdio_mode、rejects_resume_in_stdio_mode。新增的 stdio 集成测试进一步实证运行期只走 MCP 传输。**(2) `code-control --stdio` = token/lease-gated automation 入口（satisfied-as-is）**：execute 要求 `--stdio`（code_control.rs:144）；controller.attach 用 process control token（x-libra-control-token）换取 controller/lease token，message.submit/interaction.respond/turn.cancel/task.dispatch/goal.start/goal.cancel 同时转发两枚 token（Some(&controller_token)，code_control.rs:289-393）；json_rpc_dispatch_maps_attach_submit_and_detach_to_http 固定 token 转发；server 端强制（--control observe→403 CONTROL_DISABLED、messages-route-before-controller-token inline）由 code_ui_remote_security_matrix 守卫。**(3) docs 不把 MCP stdio 当 turn control plane（satisfied-as-is）**：跑卡内 grep 人工复核 docs/commands + tracing/code.md + tracing/agent.md + tracing/plan.md + src/command，全部命中要么正确描述为 MCP transport / tool surface（code.rs:40 "MCP tool surface only"、code.md(commands):18/212），要么显式否定该误读——code.md(commands):90「libra code --stdio remains the MCP stdio server and does not control a live TUI」、:330、code-control.md:8、tracing code.md:52/77、agent.md:551、plan.md:61/1480「MCP stdio 不得成为 turn control plane」；plan.md:23 提到的 memory.md `libra mcp --stdio` 冲突按 §0 out-of-scope 声明处理且本身不误述。**无 doc 把 MCP stdio 描述成 live-TUI/AgentRuntime turn control plane，criterion 3 无需修文档。** GAP-5（run_libra_vcs allowlist 文档漂移：code.md:291/293 少列 show-ref/ls-files 且反向禁止 ls-files）C6-relevant 但按卡归 C8，本卡不动。**(4) dual-entry tool set/error/shutdown 回归（GAP-FIX）**：原 code_mcp_dual_entry_test 只驱动 MCP **HTTP** 传输 + clap `--stdio`/`--web-only` 互斥；真正的 `--stdio` MCP 传输的 tool-set/error/shutdown 无回归。新增 2 个 test-provider-gated 用例：**libra_code_stdio_serves_tool_surface_reports_errors_and_shuts_down**——用子进程驱动真实 `libra code --stdio` 的换行分隔 JSON-RPC：tools/list 暴露共享工具面（断言含 run_libra_vcs/create_task/list_tasks/create_intent 且 ≥10 工具）、未知 method→-32601 顶层 error、未知 tool→invalid_params 顶层 error、stdin EOF→干净 exit 0（30s 看门狗把 shutdown 回归转成失败而非挂起）；**mcp_http_and_stdio_expose_identical_tool_set**——断言 HTTP tools/list 集合 == stdio tools/list 集合（两入口共用 init_mcp_server→build_tool_router）。保留该文件「裸跑=1 skip 占位」契约（两测试同 gated on test-provider）。仅改 tests/code_mcp_dual_entry_test.rs（无 src 改动）。门禁：fmt/clippy --all-targets --all-features -D warnings 全净；code_mcp_dual_entry_test 14 ok（test-provider，含新 2 例）；code_ui_remote_security_matrix 13 ok；code:: 113 单测全绿 |
| 2026-07-06 | Task C8（Code docs, compatibility and final closeout） | 完成 | 0.18.20 | 本行所在提交 | Code 阶段最终收尾（docs/compat/index，无 src 改动）。**GAP-6..12**（7 项 flag/UI docs-drift，C7 前先行）：docs/commands/code.md+zh-CN 补 --goal（goal 模式，parse 时校验非空+≤16KiB）/--agent（三层 profile lookup，binding 原子胜 --model，未知拒绝）/--approval-ttl（秒，覆盖项目 [approval] ttl_seconds，默认 300）/--kimi-stream（默认 true，非 Kimi 拒绝）行，allow-all approval 行+「four-tier→five-tier」，--plan-mode 默认澄清（off；Codex 有效默认 on，显式 =true 仅 codex），zh-CN browser-control 路由矩阵同步 EN+code_router()（补 /goal/status、/task/dispatch、/goal/start、/goal/cancel，删不存在的 /repo*）。**GAP-5**（run_libra_vcs allowlist doc，C7 确认未改 allowlist）：code.md+zh-CN 从 8 命令+禁 ls-files 改为权威 10 命令（status/diff/branch/log/show/show-ref/ls-files/add/commit/switch，per libra_vcs.rs:11-13）并推荐 ls-files（含 --others --exclude-standard），与工具自身 guidance 一致。**COMPATIBILITY.md**：code/code-control 行已存在 intentionally-different/Libra-only，matrix_alignment 守卫过，无需改。**tests/INDEX.md**：15 个 Code 阶段 target 行核对，2 行（code_codex_default_tui_test/code_codex_runtime_test）源路径 stale 指向不存在的 agent/codex*→改为 src/internal/ai/codex/+准确用途，13 行准确无改，未动 agent 阶段行。**tracing code.md**：1 行（Mode 与参数）仍以现在时声称 web-only 漂移风险（C2 已解决）→更新为 C2 放宽+CLI 回归覆盖；其余核对无 stale。**CHANGELOG.md**：加 Code 阶段收尾条 + 「Mutating fix bridge deferred」条（review --fix/investigate fix 保持只读、LBR-AGENT-010，无 agent↔code mutating 协作边界，待 fix bridge 落地）。codex review 三轮：R1 FAIL 1P1+1P2（--approval-ttl 文档写 approval.ttl 实为 ttl_seconds；--plan-mode 文档暗示非 Codex 可 =true 实为拒绝）→修（EN+zh）→R2 FAIL 1P2（zh --plan-mode=false 写「会话退出」应为「关闭计划模式」）→修→R3 VERDICT: PASS 零发现。门禁：compat_matrix_alignment 7/compat_command_docs_examples_section 1/compat_help_examples_banner 1/compat_error_codes_doc_sync 2/fmt 全绿；全量 cargo test --all 终审门在本机受阻：并行 spawn-based command 测试（fsck/shortlog）在 ~test 101 处 deadlock，跨 C4-C8 复现、与负载无关、零 FAILED、单跑均通过——环境限制非代码缺陷；C8 纯 docs，代码即 C7 已验证 HEAD，逐 doc-guard（matrix_alignment 7/command_docs 1/help_examples 1/error_codes 2）+ codex R3 PASS + fmt 全绿。**并发事故处理**：merge origin/main（含 #431 reset PR）时 libra CLI panic 在共享 index 留下 #431 的伪 revert（cli.rs/reset.rs/cherry_pick.rs/launcher.rs/reset docs/COMPATIBILITY.md staged 删除），libra reset 卸暂存后 working tree 仍带该 revert→libra restore --source HEAD 恢复 11 个非 C8 文件到 HEAD（HEAD 正确含 #431），确认仅 5 个 C8 文件 dirty 后再提交。**Code 阶段 C1-C8 全部完成。** |
| 2026-07-06 | Task AG-24 follow-up（Agent plan code-verified closeout） | 完成 | 0.18.21 | 本行所在提交 | 按用户要求重新从代码而非文档事实核验 `docs/development/tracing/plan.md` / `agent.md` 的 Agent Gate 8 状态：`src/cli.rs` 已有顶层 `review`/`investigate`；`src/command/agent/*` 已有 `list/add/remove`、`checkpoint export`、`clean --gc/--retention-days`、`doctor --repair`、`push --force-rewrite`、`rpc trust/untrust`；`observed_agents/registry.rs::supported_slugs()` 派生第一批 `claude-code`/`codex`/`opencode`，三者均 hook-installable / transcript-readable / read-only review/investigate launchable；`hooks/providers/{claude,codex,opencode}`、`LifecycleEventKind::SubagentStart/End`、checkpoint E4 writer/export/reader/doctor、raw `--allow-raw` + `agent_audit_log`、retention GC 与本地 erasure 测试均已落地。未发现当前 Gate 8 必须补代码的缺口；剩余项明确归为 deferred/future parity（`--local-dev`/`--force` public flags、dynamic top-level `libra hooks <agent> <verb>`、独立 `CheckpointScope::Subagent` 物化、provider-specific compaction/reassemble、mutating fix bridge、full `SkillDiscoverer`、非首批 agent 扩展、cloud mirror tombstone）。本轮改动收敛文档漂移并新增 `compat_agent_docs_contract::{agent_doc_tracks_schema_versioning_and_retention_policy,agent_doc_tracks_code_agent_runtime_source_of_truth}`，同步 `tests/INDEX.md` / `tests/compat/README.md`，防止 schema/retention/raw-export 约束和内部 runtime 事实源链接再次退化。 |
| 2026-07-08 | §11 残留项收敛（A8.6 / A9.1 / A4.2 / C9 / C10 / C11 / §11.3 / §11.4） | 完成 | 未 bump（待提交人） | 待提交 | 以代码为准的复核（25-agent 对抗 workflow）产出 §11 后，按 /goal 逐项落地+codex 逐特性审核至 PASS。**A8.6（唯一功能缺口）**：`clean --gc` 接线 `agent.retention.stderr_days`(30) 窗口——删终态 review/investigate run 的 `reviewers/*.stderr.redacted.log`、保聚合记录；`retention_stderr_days()` 由死代码转为生产调用；codex 三轮（symlink 跟随/terminal_state 白名单/u32+DateTime 溢出/prune 前置校验 fail-closed 逐一修复）→ **round-3 PASS**；8 lib 单测 + 9 e2e 全绿。**A9.1**：CHANGELOG 补 external-discovery opt-in + D1/R2 **tombstone/删除传播**延期声明（codex 纠正：`cloud sync` 已有 best-effort mirror，延期的是删除传播非 mirror 本身）。**C9/C10**：采 (b) 显式延期声明写入 `code.md`「还未闭环」表——C9 MCP 授权门（部分接入 + 无 handler=allow-all no-op，codex 5 轮精确化 tools/list、未接线 tool impl、principal 部分线程后 PASS）、C10 web-only headless IntentSpec 审批环延期。**A4.2**：采 (b) 纠正记录（hook envelope 内嵌运行期 cwd/transcript 路径→内联构造正确，静态 fixture 不适用）。**§11.3**：agent.md 4 项 future-parity 延期项登记入 plan（补 §10↔agent.md traceability）。**§11.4**：runtime.rs/agent-mod.rs/code.rs 三处「声称比实际更未完成」陈旧注释清理。**C11（执行期实测发现的真 bug）**：跑 headless test-provider 用例 `submit_plan_draft...` 发现 ~40% flaky——`on_tool_call_start`/`_end` 两个独立 `tokio::spawn` 无序，迟到的 start(running) 覆盖 end(completed)，plan/tool_call/transcript 三个 id-keyed 投影永久卡 running（既存潜伏 bug，本轮 binary layout 暴露）。修：`HeadlessTurnObserver` 加 ordering barrier（`on_tool_call_end` 先 await 匹配 start 句柄再写终态）+ `upsert_plan` monotonic guard（纵深防御）；session-status 多写者竞态（end Thinking vs turn-end Idle）判为独立后续项。扩展测试断言 plan+tool_call+transcript 三者终态，由 ~40% flaky 转 20/20；full headless suite 13/13。codex R1 FAIL（仅修 plan 不够）→ R2 PASS。质量门：`cargo +nightly fmt --all --check` 绿、`cargo clippy --all-targets --all-features -D warnings` 绿（含 C11 后重跑）；`agent_clean_test`(9)/`compliance`/`code_ui`/`ai_code_ui_headless_test`(13) 绿、`command_test`(2464) 回归绿。codex 逐特性：A8.6 R3 PASS、Feature 2 R5 PASS、C11 R2 PASS。发布闭环（版本 bump/commit/push）留待提交人执行 |

## 11. 代码复核补充（2026-07-08）：未闭合项与未捕获延期项

> 本节由一次「以代码为准、不以文档/checkbox 为准」的多智能体对抗复核产出（逐卡源码锚点核验 + 逐 gap 反驳验证）。**总体结论**：§1/§3/§6 全部任务卡的核心实现均由真实源码背书（review/investigate 顶层命令、observed_agents capability/registry/trust/compliance/extract、hooks providers、E4-libra writer/doctor/prune、migration 2026070802/2026070803、code 三模式/provider/session/MCP/approval 等全部命中），**§10 声称的完成不是空打勾**。但下述三类残留未被 §10 如实记账，按 §0.1.4 第 3 条「completion 以代码为准」补入为待办；每条附源码锚点，供后续执行者直接落地或显式改判为延期。

### 11.1 声称完成但代码未闭合（claimed-done-but-open，与已勾 checkbox 冲突）

#### Task A8.6：AG-24a 残留 — stderr/redaction-report 保留期 GC 未接线

**描述**：A8.5 卡 AC3（`plan.md:1044`）勾选「`clean --gc` 实现 `transcript_days`(90) **AND** `stderr_days`(30) 窗口清理」，但 `stderr_days` 半边为死契约：`retention_stderr_days()`（`src/internal/ai/observed_agents/compliance.rs:80`，`DEFAULT_RETENTION_STDERR_DAYS=30`，`compliance.rs:33`）在 `src/` 下**零生产调用点**（仅 `tests/agent_audit_log_test.rs:157` 断言默认值、`tests/compat/agent_docs_contract.rs:43` 文档存在性引用）。`clean --gc`（`src/command/agent/clean.rs:87-108` → `gc_expired_checkpoint_ids`）只按单一 `transcript_days` cutoff 整体裁剪 checkpoint（transcript+stderr 一起删），不存在第二条 stderr/redaction-report 30 天窗口，也没有 `agent.md` 保留期表要求的「删除诊断 blob，**保留聚合计数**」逻辑。

**验收标准**：

- [x] **已完成（2026-07-08，采用 (a) 实现窗口）**：`clean --gc` 新增独立 `stderr_days`(30) 窗口——对**终态** review/investigate run（`.libra/sessions/agent-runs/<run_id>/`）中 `updated_at` 早于 `now - stderr_days` 的，删除 `reviewers/*.stderr.redacted.log` 诊断 blob，保留聚合记录（`state.json`/`manifest.json`/`findings.md` + stdout 日志 + manifest 内的 redaction-report 汇总），`agent_audit_log` 永不触碰。这正是 agent.md「保留期与 GC」stderr 行「删除诊断 blob，保留聚合计数」的落地；`retention_stderr_days()` 由此接线（不再是死代码）。checkpoint 侧无独立 stderr blob（E4-libra `redaction_report.json` 已是聚合、且被 content_hash 覆盖），故不触碰 checkpoint tree。
- [x] 覆盖测试：`src/command/agent/clean.rs` 单测（`prunes_only_aged_terminal_runs_and_keeps_aggregate`、`foreign_kind_and_corrupt_terminal_state_are_skipped` 含 garbage-terminal 白名单、`investigate_terminal_state_is_recognized`、`symlinked_reviewers_dir_is_never_followed`、`missing_or_undatable_runs_are_skipped_fail_safe`、`stderr_cutoff_for_days_never_panics_on_huge_window`）+ `compliance.rs::u32_days_rejects_values_that_would_wrap` + e2e `tests/command/agent_clean_test.rs::agent_clean_gc_prunes_aged_reviewer_stderr_logs_and_keeps_aggregate`（8 lib + 9 e2e 全绿）。
- [x] 安全加固（codex 三轮 review：R1/R2 FAIL→R3 PASS）：symlinked `reviewers` 目录拒绝跟随（`symlink_metadata` 前置）；`terminal_state` 按 kind 白名单（review/investigate 真实终态字符串）防 corrupt/foreign 误判；`u32::try_from` + `Duration::try_days` + `checked_sub_signed` 防配置溢出 panic；stderr cutoff 在任何 prune mutation 前解析+校验（fail-closed）。

**触达文件**：`src/command/agent/clean.rs`、`src/internal/ai/observed_agents/compliance.rs`、`tests/command/agent_clean_test.rs`、`docs/commands/agent.md`+zh-CN。**依赖**：A8.5。**规模**：S。**codex review**：PASS（round 3，零发现）。

#### Task A9.1：AG-24 残留 — release notes 三类延期声明补齐

**描述**：A9 卡验收（`plan.md:1088`）要求 release notes 区分 enabled / preview-opt-in / explicitly-deferred，**尤其点名** external discovery、review/investigate fix、D1/R2 deletion propagation。审计发现（2026-07-08）原 `CHANGELOG` 只覆盖 3 项中的 2 项（enabled 收尾 + fix bridge deferred/LBR-AGENT-010），缺 external discovery 的 opt-in 声明与 D1/R2 deletion propagation 声明。

**已完成（2026-07-08）**：`CHANGELOG.md` `## [Unreleased] / ### Documentation` 补两条——(1) external agent discovery 为 **preview/opt-in、默认 quarantine-by-default**（`agent.external_agents.enabled` gate、未知 binary quarantine、内置 slug 仿冒 skip-and-log）；(2) **D1/R2 deletion propagation 延期**。**codex review 纠正**：D1/R2 措辞不得说「无 cloud mirror」——实测 `libra cloud sync` **已有** best-effort agent-capture mirror（agent checkpoint blob/tree/commit 经 `object_index` 上传 R2，见 `history.rs:900/993/1084`；`agent_session`/`agent_checkpoint` 行镜像到 D1，见 `cloud.rs:358/1963`）；真正延期的是**本地 erasure 的 tombstone/删除传播**（本地重写 refs/DB/object_index 但不向 D1/R2 推 delete，故他机 restore 可复活已删数据）。CHANGELOG 与本卡措辞已按此更正。

**验收标准**：

- [x] `CHANGELOG.md` 补 external discovery preview/opt-in 声明 + D1/R2 tombstone/deletion propagation 延期声明（措辞：mirror 已存在、删除传播延期，非「无 mirror」）。

**触达文件**：`CHANGELOG.md`。**依赖**：A8.5、A8、A3。**规模**：XS。**codex review**：Feature 2 文档批（见 §10 进度记录行）。

#### Task A4.2：`tests/fixtures/agent_hooks/*` provenance manifest 缺失（触达清单未兑现）

**描述**：A4（`plan.md:790`）与 A6.5（`plan.md:921`）的预计触达文件均列 `tests/fixtures/agent_hooks/*`，A6 验收 #6（`plan.md:862`）要求 transcript fixtures「并同步 A4 的 `tests/fixtures/agent_hooks/*`」并附溯源 manifest。实测该目录**不存在**（`ls tests/fixtures/agent_hooks` → No such file or directory；对照 `tests/fixtures/agent_transcripts/` 存在且含 `MANIFEST.md`）。

**已解决（2026-07-08，采用 (b) 纠正记录）**：核查确认 hook 采集测试**刻意内联构造 envelope 而非用静态 fixture**，这是**技术上正确的选择**而非遗漏——hook envelope 内嵌运行期特定路径（`cwd`、`transcript_path`，见 `tests/agent_lifecycle_event_test.rs:145-152`：`"cwd": self.repo.to_string_lossy()`、transcript 写在临时 `~/.claude` 下），必须在测试运行时按临时仓库/临时 HOME 注入；静态 fixture 文件无法携带 tempdir 的 cwd/transcript 路径，因此内联构造是唯一可行形态。相较之下 `tests/fixtures/agent_transcripts/`（纯 transcript 内容、无环境相关路径）用静态 fixture + `MANIFEST.md` 溯源是恰当的——**真正需要溯源 manifest 的 extraction fixture 已具备**（`agent_transcripts/MANIFEST.md`）。故不新建 `agent_hooks/*` 死 fixture（无消费者的 fixture 比缺失更糟）；记录以本卡为准：A4 hook 采集测试的 envelope 内联构造、provider parser 单测（`src/internal/ai/hooks/providers/{claude,codex,opencode}/parser.rs` 的 `#[cfg(test)]`）覆盖各 provider envelope 形态，A6 crit #6 对 hook 侧的「共享 fixture」交叉引用按此口径理解为 inline 构造。

**验收标准**：

- [x] 采用 (b)：记录与实现一致——hook envelope 因内嵌运行期路径而内联构造，`agent_hooks/*` 共享静态 fixture 不适用；extraction 侧的溯源 manifest（`agent_transcripts/MANIFEST.md`）已具备。

**触达文件**：`docs/development/tracing/plan.md`（本卡记录）。**依赖**：A4、A6。**规模**：XS。

### 11.2 未被任一任务卡捕获的松散端（uncaptured loose ends）

#### Task C9：MCP 授权门在生产为 allow-all no-op

**描述**：`McpAuthorizer` 授权门在生产**仅部分接入**——`resources/{list,read,templates}`（`server.rs:186/194/479`）与**部分** `tools/call` 站点（`resource.rs` 至 :2297 一带 `authorize_or_error[_with_actor]`）已接入，但 **`tools/list` 未接入**（`McpOperation::ListTools` 仅存在于 `authz.rs` 枚举与测试），且**若干 tool impl 完全无 authz 调用**（如 `create_patchset_impl:2354`、`list_patchsets_impl:2423`、`create_evidence_impl:2483`、`create_tool_invocation_impl:2608` 等）。**最关键**：**生产代码从不调用 `set_authz`**（仅 `src/internal/ai/mcp/tests.rs` 安装 `DenyAllAuthz`/`RecordingAllowAuthz`），因此 `authorize_with_principal_or_error`（`server.rs:144`）在无 handler 时无条件 `return Ok(())`（即便对已接入站点也是 allow-all）。（principal 侧：actor-aware 的 `authorize_or_error_with_actor`（`server.rs:130`）已用 `from_actor`；非 actor 的 `authorize_or_error` 仍跑 system principal，`server.rs:116`。）该实现被标为 "Phase 5 schema-only placeholder"（"Phase 5" 命名源自已删除的 mcp.md 拆分计划）。C6（MCP 边界）与 C7（approval/sandbox/tool gate）均标完成，但两者都未排期安装真实 authorizer；plan.md/code.md 读者会误以为 MCP 授权已就绪，实为**部分接入 + 永久 allow-all no-op**（未接入的 tool impl 与 `tools/list` 连门都没有）。

**验收标准**：

- [x] **已完成（2026-07-08，采用 (b) 显式延期声明；codex 5 轮精确化 review→PASS）**：`docs/development/tracing/code.md`「还未闭环的功能与风险」表新增 C9 行 + 本卡描述，精确记录——授权门仅部分接入（resources/{list,read,templates} + 部分 tools/call；`tools/list` 与 `create_patchset_impl`/`list_patchsets_impl`/`create_evidence_impl`/`create_tool_invocation_impl` 等未接入）、**生产无 handler=allow-all no-op**、principal threading 部分存在（actor 站点 `from_actor` / 非 actor 站点 system），并从 C6/C7「完成」表述去除「MCP 授权已闭环」歧义。选项 (a)（安装真实 authorizer + 全站点 principal threading）属 Phase-5 净新增，超出 tracing 计划「不发明额外功能」范围（§6），故按 (b) 完成、重启条件写入 code.md。

**触达文件**：`docs/development/tracing/code.md`、`docs/development/tracing/plan.md`。**依赖**：C6、C7。**规模**：XS（采 b）。**codex review**：PASS（Feature 2 批，round 5）。

#### Task C10：web-only headless 跳过 IntentSpec/Plan 审批环

**描述**：web-only headless runtime（`--web-only --provider <非 codex>`）**刻意跳过** IntentSpec/Plan 审阅-审批环——`code.md` 把「普通请求先进入可审阅的 IntentSpec / 执行计划流程」称为默认契约，但 headless 把每次 browser submit 当单次直连 turn。源码注释（`src/internal/ai/web/headless.rs:23-31,78`）写「Full IntentSpec plan approval remains future work ... will land in subsequent phases」，且 follow-up 只指向 `docs/development/commands/_general.md`，未回写 plan.md/code.md（两者已声明 Code 阶段 C1–C8 完整完成）。tool ACL/sandbox 仍生效，故属 workflow/approval-UX 缺口而非裸安全洞，但三份计划文档零捕获。

**验收标准**：

- [x] **已完成（2026-07-08，采用 (b) 显式延期声明；Feature 2 codex PASS）**：`docs/development/tracing/code.md`「还未闭环」表新增 C10 行 + 本卡，声明 web-only headless 为 **direct-turn 契约、IntentSpec/Plan 审批环延期**（源码注释 `headless.rs:23-31,78` 已注明 will land in subsequent phases），写明与 `code.md` 默认契约的差异边界与重启条件。选项 (a)（为 headless 实现审批环）属净新增后续工作、超出「不发明额外功能」范围，故按 (b) 完成。

**触达文件**：`src/internal/ai/web/headless.rs`（注释已核）、`docs/development/tracing/code.md`、`docs/development/tracing/plan.md`。**依赖**：C4。**规模**：XS（采 b）。**codex review**：PASS（Feature 2 批）。

#### Task C11：headless plan projection 的 start/end spawn 顺序竞态（真 bug，2026-07-08 执行期发现）

**描述**：执行期跑 `ai_code_ui_headless_test::submit_plan_draft_tool_call_projects_draft_plan_into_snapshot`（test-provider 门控）时发现该用例 ~40% 概率失败——失败快照显示 plan 数据全对但 `status` 卡在 `"running"`（应为 `"completed"`）。根因是 **C4/headless 的真实竞态**：`on_tool_call_start` 用 `tokio::spawn` 把 plan upsert 为 `"running"`（`src/internal/ai/web/headless.rs:990-995`），`on_tool_call_end` 用**另一个** `tokio::spawn` upsert 为 `"completed"`（`:1064-1069`），两个 spawn 任务无顺序保证；当 end(completed) 先于 start(running) 执行时，迟到的 "running" 覆盖已写入的 "completed"（`upsert_plan`→`upsert_by_id` 为 last-writer-wins，`code_ui.rs:634`），plan 永久卡 running。此竞态与本次 §11 其它改动无逻辑关联（clean/retention/注释均在 disjoint 路径），但本地 base binary 5/5 通过、加入本轮改动后的 binary ~40% 暴露（binary layout 扰动一个本就紧的 async race），说明它是**既存潜伏 bug**、非本轮引入的行为回归；但属真实正确性缺陷（真实使用中 plan status 也可能卡 running），且 C4「完成」未捕获。

**已完成（2026-07-08）**：codex R1 复审指出同一竞态还会回退 tool_call/transcript/session status（不止 plan），故采两层修复：
1. **ordering barrier（根因修）**：`HeadlessTurnObserver` 加 `start_tasks: HashMap<call_id, JoinHandle>`；`on_tool_call_begin` 记录 start 任务句柄，`on_tool_call_end` 在写终态前先 `await` 该句柄——保证同一 tool call 的 start（running tool_call/transcript/plan + ExecutingTool）全部先于 end（completed）落地，一次性消除 tool_call/transcript/plan 三个 **id-keyed** 投影的回退。
2. **monotonic guard（纵深防御）**：`CodeUiSession::upsert_plan` 保留终态守卫——同 plan id 已达终态（`completed`/`failed`）时迟到的非终态 upsert 跳过；新增 `is_terminal_plan_status`。

**session status 另案**：`on_tool_call_end` 的 `set_status(Thinking)`（`headless.rs:1071`）与 turn-end 的 `set_status(Idle)`（`headless.rs:390`）是**跨 tool-call/turn 边界的多写者竞态**，barrier 无法排序（两者不共 call_id）；彻底修复需串行化所有 session-status 写入（较大改动），故本卡**不修 session status**，作为独立后续项记录——它是瞬态、自愈（下一 turn event 纠正），非「永久卡死」类 id-keyed bug。

**验收标准**：

- [x] barrier：`on_tool_call_end` 先 await 匹配的 `on_tool_call_begin` 句柄，再写终态；per-turn observer 独立 `start_tasks`（无跨 turn 泄漏）。
- [x] `upsert_plan` 对同 id 终态→非终态幂等跳过；非终态→终态、终态→终态（last-writer）正常；单测 `code_ui::tests::{terminal_plan_status_classification,upsert_plan_does_not_regress_terminal_status_to_running}`。
- [x] 回归：扩展 `ai_code_ui_headless_test::submit_plan_draft_tool_call_projects_draft_plan_into_snapshot` 断言 plan **与** tool_call **与** tool-call transcript entry 三者均 `completed`（barrier 下确定）；由 ~40% flaky 转为 20/20 通过。
- [x] session status 多写者竞态显式记录为独立后续项（非本卡范围）。

**触达文件**：`src/internal/ai/web/headless.rs`、`src/internal/ai/web/code_ui.rs`、`tests/ai_code_ui_headless_test.rs`、`docs/development/tracing/plan.md`。**依赖**：C4。**规模**：M。**codex review**：R1 FAIL（plan-only guard 不覆盖 tool_call/transcript/session status）→ 加 ordering barrier + 扩展测试 → **R2 PASS**（barrier 排序正确、无死锁、per-turn map 无跨 turn 混淆、session-status 另案 scope 判定成立）。

### 11.3 agent.md「还未实现的功能」表中未回写 plan.md §10 的延期项（traceability）

§10「AG-24 follow-up」的 8 项 deferred 清单是闭集，未把 `agent.md`「还未实现的功能」表当作事实源，导致下列已确认「仍在代码中延期」的项漏记。多数属 `agent.md` 归类的 **deliberate difference / future parity**，补救以「加显式 deferred 标记」为主，未必需要完整实现卡。

**已解决（2026-07-08）**：下表本身即把这些延期项显式记入 plan.md（填补 §10 闭集清单与 `agent.md`「还未实现的功能」表之间的 traceability 缺口）；四项均经代码核验「仍未实现」（锚点见表），且属 `agent.md` 归类的 deliberate-difference/future-parity，处置为「已在此登记为延期」，不新增实现卡。

| 延期项 | 代码锚点（确认仍未实现） | agent.md 出处 | 建议处置 |
|---|---|---|---|
| optional parity traits：`ProtectedFilesProvider`、`HookResponseWriter`、`RestoredSessionPathResolver` | `HookResponseWriter` 声明于 `capability.rs:114` 无 impl（`adapter.rs:257` 返回 None）；`ProtectedFilesProvider`/`RestoredSessionPathResolver` 全树无踪 | `agent.md:1844` | plan §10 补 deferred 标记（§10 现只覆盖 `TranscriptCompactor`） |
| condensation / shadow branch / per-session sidecar writer（`context.md`/`prompt.txt`/`full.jsonl`） | E4-libra writer 固定 6-entry tree，无 sidecar/shadow 输出（§10 A5 行印证） | `agent.md:1847`（deliberate difference + future parity） | plan §10 补 deliberate-difference 标记 |
| external RPC extension method family（超出 v2 info/capability gate） | `src/command/agent/rpc.rs` 仅 v2 info + capability 协商，未声明能力 fail-closed | `agent.md:1848` | plan §10 补 future-parity 标记 |
| promoted roster 的 `TranscriptTruncator`（codex/opencode rewind `--apply` 无法截断） | `stable_promoted.rs:10-15` "v2 follow-up will add per-agent truncation"；仅 ClaudeCode 实现 `TranscriptTruncator` | `agent.md` 能力表 | plan §10 补 deferred 标记（属 A4/A6 rewind 面） |

### 11.4 陈旧注释清理（声称比实际更未完成，与本节 §11 方向相反）

以下注释把已完成能力写成未完成，会误导后续执行者「重做已存在功能」，应做一次文档清理（非功能缺口）。**已全部清理（2026-07-08，Feature 2 codex PASS）**：

- [x] `src/internal/ai/hooks/runtime.rs`（`HookTarget::AgentTraces` doc、`ingest_agent_traces` doc、`process_hook_event_with_target` doc）：原写 "Phase 1 stub / 'does NOT yet generate checkpoint commits — Phase 2 work'"，已改为准确描述——AgentTraces 完整 ingest 并在 SessionEnd 写 E4-libra `refs/libra/traces` checkpoint commit（`ingest_agent_traces_payload` 带 `Some(repo_path)`）。
- [x] `src/command/agent/mod.rs`（模块 doc + `execute_safe` doc + `refuse` helper doc）：原写 "V1 ships stub handlers that emit not-yet-implemented-in-this-phase"，已改为——每个子命令派发到真实 handler；`refuse` 标注为当前未用的保留 seam。
- [x] `src/command/code.rs`（`build_placeholder_web_code_ui_runtime` 上游注释 + InfoNote content）：原注释/文案声称「非 codex provider 需去掉 --web-only」且误称 placeholder 是 bootstrap 失败兜底。已更正——C2 放宽后非 codex provider 走 headless web runtime；**构造错误经 `?` 传播**，placeholder 仅是 `Ok(None)`（未接线，今仅 Codex 且已提前改道）的防御兜底。`id: "web-ui-placeholder"`（被测试断言）保持不变，仅改人读文案/注释。

### 11.5 已核实无需补的项（防止复核回退）

- PREP Task 0.4 的 V1 `test ! -e docs/development/commands/{agent,code}.md`（`plan.md:591`）今日 literal 失败：`932c3a04` 后这两个路径是**新建的 10 行转发桩**（body 指向 `../tracing/{agent,code}.md`），非旧迁移内容；所有守卫特例化重指 tracing/、否定 rg 零命中、无编译测试断言其缺席——迁移目标已达成，属 stale-verification note，不构成回归（如需精确，可把该 V1 命令改为断言「文件若存在则仅为转发桩」）。
- `CheckpointScope::Subagent`：enum 变体存在（`history.rs:1857`，`as_str()="subagent"`）但全树无构造点（只写 `Committed`），plan §10「独立物化仍延期」判断正确，非 false-deferral。
