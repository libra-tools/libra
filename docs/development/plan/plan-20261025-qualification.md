> **2026-10-24 21:42:59 UTC FIX-LTR-01本地A实际终态：** 独立clone的唯一结构oracle修复已通过nightly fmt、strict all-target/all-feature Clippy、exact1/1、完整target54/54及默认全量10761/10761的框架核验，exit均0；21原ignored不变，无失败/flake，3962.768/7200秒、2126输入无漂移。输出38个runtime skip标记已逐身份/逐文字归属，其中1个是缺interpreter的负向断言，37个是既有feature/live/工具/平台覆盖缺口，不能据此宣称所有live行为已测。唯一遗留worker归属当前status-pause无helper用例，已核PID/start identity并通过pidfd人工SIGTERM，确认exit/reap；原资源缺口保留，FIX03未修。A仅本卡locally-accepted，自己的真实Claude ER05、patch/C/D均待；版本仍0.30.86、无提交/push。三卡正式0/3，FIX02/03 pending。以下旧时点事实保留。**

> **2026-10-24 20:23:39 UTC FIX-LTR-01实施入口：** 本计划工作副本位于独立原生clone `/media/eli/data/libra-qualification-20261025`，branch `fix/qualification-schema-doctor`，基线0ca47cc。FIX11实际done/complete且两个告警fixed；恢复R5方案/治理审查PASS。现仅改既有结构字串加入Ui排除，生产CLI、runtime断言和相邻startup guard不变。A未启动、自己的代码ER05未取得；没有bump/提交/push/Release/CI新结果。外部修复仍0/3。下方原时点快照保留。

> **FIX-MTR-11实际收口（2026-10-24 20:22:25 UTC）：Dependabot23/24已由GitHub实际标fixed，fixed_at分别2026-10-24T20:16:51Z/20:16:52Z，均非dismissed；main与v0.42.70解引用均932731d9b7f274638d2fb1bfa10d571207a54a39，人工Release已发布，main Harbor40112708799及tag Docker40112738161三job全部success。自己的Claude R3 ER05/H0 PASS、21 targets/3085 PASS/0 FAIL/3原ignored的五门C与signed/DCO均已核，现done/complete；RED1 FAIL/STOP与原P2不改。原件D-alerts-fixed-terminal-20261025 SHA256 2585fecfeacde5f37a6bae1eed701898e20f8254e11d704cc08e017129a0e7f9。Mega15/112（活动15/111）、Libra0/26（活动0/25）、联合15/138（活动15/136）；外部恢复0/3。恢复方案R4及治理自身R5真实本地Claude均PASS。安全前置已解除；下一步优先在独立Libra clone核入场并实施FIX-LTR-01，MTR101亦仅解除本安全前置，不跳过各卡独立门。原Libra merge签名政策、H1/LTR10及LTR01仍未完成。**

> **2026-10-24 20:15:32 UTC 治理采纳与自身审查实际收据：** 九份文档已按受审hash采纳；真实本地Claude R5 `DOCS_ER05_VERDICT / ER03_VERDICT / H0_SYNC_VERDICT / VERDICT: PASS`，P0/P1=0，session `e72fd9de-c479-457d-9eff-71ea07a7b49d`，审查输出SHA256 `cd632543ed9722003ebdda14f6e6f32512eb84f7b8e63e0940ce13a96291d4b7`，manifest SHA256 `096eae5326d3714271eaa2c4c3ba59dfa1f7c1156b6aa3d48d8bdf5c7f0fa78a`。本行是审查之后的事实收据，不声称审查器读过其自身后产出的输出。root接受非阻断P2：例行维护收据/预算约束以下方具名表为准，执行前并入对应条款并实审；历史横幅保留，本文首收据为当前时点；终态执行器仍须生成/冻结/实审。缓存复制实际exit0，31147文件（deps11178/build4563/fingerprint15406）、19035122524字节，root已核排除第一方/测试可执行文件及硬链接；这只完成源准备，不是验收。两仓精确signed本地提交待，尚未push；原治理由既定MTR01/LTR01承载。三张修复仍pending、0/3；原14/138不变；FIX11两告警实际fixed及逐卡入场条件未满足前不实施，不复用任何旧FAIL/STOP窗口。

> **2026-10-24 20:09:49 UTC 当前审查状态：** 恢复方案R4真实本地Claude ER03/H0/VERDICT均PASS，P0/P1=0；原R1～R3 FAIL保留。本文现为P2事实修订后最终治理候选，自己的DOCS-ER05/H0审查待取得；三卡仍pending/未实施、未测试、未提交、未发布。

# Libra 2026-10-25 来源资格前滚修复

> ER-03 候选，尚未采纳、实施或开启测试窗口。依据 [plan-template](plan-template.md) 与仓库 AGENTS.md；按用户当前指令只使用真实本地 Claude CLI 审查，不运行隔离 Codex。本文只承接下列三个缺陷，不替换 `plan-20261015.md` scoped-traces 计划（原工作区现行未发布治理另行承载）。

## 目标与来源

恢复选定上游 `0ca47cc147b2099ffb835ade91272c4cfe1efd21` 的真实来源资格，保留新 UI 行为和全部原测试断言。原工作区 HEAD `ed566b0229dfe2805a2317f889355e449f7d4e77` 是外部合并，包含未发布治理且没有签名/DCO；本计划不改写它、不替它补造签名，也不从该 HEAD 推送修复。

原首次资格已 FAIL/STOP：coordinator exit1、native full exit100，2026-10-24T19:12:05Z；4982.973/7200秒，非超时。10761 eligible 身份全部到达，10760最终通过、1最终失败、21 source ignored、1 flaky success；实际失败尝试4次（3+1），最终报告重复的失败标题不重复计数。fmt/strict Clippy/build 已 exit0；doc/network 未开始。2125输入未漂移。原件在仓外 `libra-upstream-qualification-20261025/r2`；terminal SHA256 `7f1a5258024676525210f3cd6a48c834d8450dd209a6251d3b25348481fc02a6`，full JUnit `e9f08229091d55e028307cbe1d98f71b477b63299be7110532f564ee51a8c5fa`。现存 proc-macro-error2 future-incompatibility 提示仍是具名风险，不能称零诊断。

实际本地 Claude 失败处置审查：session `a87e189c-332a-4dd0-9db9-c412cc81e314`，2026-10-24T19:20:05.975264Z，exit0、FAILURE_VERDICT/VERDICT PASS、无漂移，输出 SHA256 `7e9531e8736c8d67b9e83ea17b954885f3694dc918ae9047b9bdd5c357ba4edf`。它只认可失败记账与三个独立修复方向，不是产品/资格 PASS，也不是本计划的 ER-03 准入。

### 独立发布与原工作区保护

- root 是唯一写者和串行发布者。使用真正独立的原生 Libra clone（不是共享 HEAD/index 的 worktree），从现场核实的 origin/main 建立短期 feature branch。先检查类型、HEAD、远端 ancestry、工作区/索引、并发写者、身份和 vault 签名；若上游变化，先审差异并重核本卡，不覆盖新增提交。
- 每卡独立 patch、A/B、实际 Claude ER-05、C、PR、merge、tag/人工 Release 和真实 D；不建立 G-08 家族或 G-13 聚合，不把测试修复塞进 LTR-01。未来卡的实现不得混入前一卡提交。
- 修复 checkout 的公开写集仅本计划、基于实际已发布上游版本的 `docs/development/plan/plan-status.md` 中本计划登记行、当前卡测试文件、该卡实际版本面/工具链 lock。与原工作区未发布 scoped-traces 治理提交做 ancestry 和逐文件 diff 守卫，禁止携带 `ed566b0`、其本地治理父链或原三份治理 WIP；plan-status 只在上游版本中新增/更新本计划行，禁止复制原工作区整份文件。本文是新的独立修复计划，不复制未发布的 scoped-traces 文本到发布分支。
- 原工作区两仓既有八份治理文件只登记事实/外部依赖，仍由原 MTR-01/LTR-01 与 Mega 后续既定 C/D 承载；其中原 scoped-traces 治理增量不属于三个修复分支的发布写集；plan-status 的新修复计划行是下述独立同步写集。新修复发布后，原工作区只通过原生前滚整合保留双方历史；若本文本地未跟踪副本与远端已发布版本碰撞，先保留快照、核同源与内容，再只处理 root 自建副本，不清理无关文件、不 reset/force。
- `ed566b0` 的签名/DCO 政策仍须在原工作区 LTR-10/LTR-01/任何 push 前明确处置。新修复不以其为祖先，不借独立发布解决或掩盖旧合并政策。

### 使用规则与共享状态保护

依模板使用规则，新建本计划必须在 `plan-status.md` 的「计划一览」登记；推进每张卡时同一变更更新该行的卡状态、实际版本/commit及UTC秒级时间。每卡 I 都包含本文件和该行。原工作区先增加候选行；独立发布 clone 从实际上游 plan-status 开始，只新增/修改该行。逐行 diff 守卫必须证明原 scoped-traces 未发布段没有被带入。后续原生整合时对同一状态行逐项合并，保留双方历史，不以整文件覆盖解决冲突。原「LTR01唯一承载」指 Traces 治理的发布，不涵盖此独立恢复计划。

### 执行预算与停止规则

用户已明确授权为完成两个计划进行必要操作；本候选把必要的前滚修复具体化，必须先获得实际本地 Claude ER-03/H0 PASS，才按各卡新来源开始。原7200秒 claim/STOP封存，不使用剩余约2217秒，不重开旧FIX3/LTR00的3/3诊断及8/8窗口，不以换目录重置旧预算。失败处置 review 自身不授执行权。

候选独立 A 窗口：FIX-LTR-01一次7200秒，FIX-LTR-02/03各一次10800秒（共享夹具真实云消费者或最终doc/network，均包含完整默认套件）；每卡最终 bump 后本地 C 各一次10800秒；FIX-LTR-03 的 C 包含完整全量、doc及原3例network。三卡实际D全绿后另有一次计划最终资格窗口10800秒，执行最终发布树fmt/clippy/full/doc/network，不以C代替模板计划完成门。上限是新独立来源的命令编排预算，不修改任何测试内部 deadline、并发组、断言或原 nextest retries。依据本机已测 build+listing+full4983秒及 strict Clippy约876秒，额外预留最终 release build/网络 feature 编译；不把冷构建慢当代码原因。本地窗口从第一条Cargo命令到本地门/构建/安装结束，包含编译和listing；PR CI、merge及Release D远端等待不计入该本地执行预算，单独登记run/job/ref，观察超时不重启作业、不记PASS。任一意外失败/超时/漂移立即保留终态并STOP，不自动再开窗口；后续只读归属与另行ER-03，不因剩余时间继续命令。

所有 Cargo 门 source `.env.test` 和 `.env.live-test`，核非秘密 presence/控制项、原工具链与 Cargo 配置；缺文件或必要凭据停止相应门。延续隔离 HOME、已核 CARGO_BUILD_JOBS=1、umask0022，不复制用户凭据到仓库或日志。listing与JUnit按(target,case)精确核数、保留原ignored/重试及成功输出中的runtime skip；缺环境、缺 feature、early return不冒充所需行为通过。代码完成、自身review、提交、push、D逐项分别登记。

构建缓存：独立 clone 使用自己的 `target/`，不与原工作区共享可写 target。不携带旧日志作为新证据；确认原 target 无 Cargo/rustc 写者后可复制缓存到新 target（禁止硬链接），记录工具链/配置/来源，Cargo仍在新来源重新核fingerprint。不能完成安全复制则使用冷 target，并在claim前重新审预算；不在claim外预编译来隐藏成本。当前7200/10800秒是暖缓存依据下的候选上限，不保证冷构建能完成，不得到时自动续窗。

完整默认套件的唯一命令为 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`；nightly fmt为 `cargo +nightly fmt --all --check`，严格Clippy为 `cargo clippy --all-targets --all-features -- -D warnings`。所有门读取并绑定同一受控输入，不改变nextest并发/配置。

每卡 A 包含其 Verification、nightly fmt及strict all-target/all-feature Clippy；触发全量的卡另跑原双env默认全量。自己的 ER-05 PASS 后，C严格按模板ER-04/08顺序：版本面parity→实际patch→工具链刷新lock→最终fmt/clippy和本卡完整全量/所需focused→`cargo build --release`→将本卡release二进制安装到仓外、每卡独立的 `install/bin/libra` 并以绝对路径核version/hash（复制+chmod0755，hash须等于本次target/release/libra）；不覆盖原工作区使用的libra或改变全局PATH→精确`libra add`及signed+DCO提交→feature branch/PR→该head的base/CodeQL全部适用job通过→squash merge并核merge SHA/tree/DCO/签名；merge tree必须等于已测bump后tree，任何上游变化或树不同即STOP并ER-03，不tag、不借旧PASS→人工Release及版本tag→release工作流与渠道artifact真实核实。遇分支保护、必需他人review或环境approval，保留实际pending状态；不使用admin bypass、不自行批准。未完成D不得done。编译诊断按实际记录；若既有future-incompat提示违反当次发布门，先独立归属，不静默豁免。

### 被测树、逐卡证据与终止链（执行前预登记，模板v2.14）

1. **每卡C前冻结。** 先把已真实取得的A/B、自己的Claude ER-05、前一卡已发生的C/D写入本计划与plan-status本计划行；当前卡只写实际已知状态，C/D未知项明确pending，不预填成功、未来commit或run。完成patch与工具链lock更新后冻结全部输入；本次C就在这棵树上执行。C到提交/PR/merge之间不再修改任何跟踪文件，包括状态和证据文档，因此最终提交树必须逐字等于C被测树，merge tree也必须相等。任何漂移STOP/ER03；不以文档白名单免除这三个树相等的约束。
2. **C/D后置证据归属。** FIX01的真实C/D、提交/tag/version/job/UTC收据由FIX02开工时在两份既定文档内补记；FIX02由FIX03同样补记。前一卡只有实际C/D全绿后才启动后继，不以文档中尚待补记的旧快照当证据。状态推进与证据更新在同一文档变更中完成，并在后继自己的C前冻结，后继C实际覆盖这些新增字节。若后继尚未开工，真实结果保持待补记，不提前执行后继、不发布单独的未登记补记提交；若后继STOP，随该次ER-03前滚修订在本计划正文同步补记前一卡已发布的C/D事实，仍用两文档既定写集与自身文档验收，不允许事实永久只留仓外。
3. **FIX03 done与计划完成分开。** FIX03自己的AC/VER、A/B、自身ER-05和独立C/D实际全部通过后才可done/complete；计划最终资格不是其AC的递归前置。随后在FIX03实际merge SHA的冻结树运行已登记的一次10800秒计划最终资格（fmt、strict Clippy、双env全量、doc、network原3例），再取得真实本地Claude来源资格PASS。该窗口不能包含之后才写出的终态文档，也不能把已发布产品树换成新的上游树。
4. **唯一终态登记提交。** 计划最终资格通过后，在独立clone创建一笔纯文档登记提交，I精确为 `docs/development/plan/plan-20261025-qualification.md` 与 `docs/development/plan/plan-status.md` 的本计划行；R=N/A、version=N/A、无tag/Release、不改变任一产品/测试/配置/lock/版本面。该提交承接FIX03真实C/D、最终资格和三卡状态，以及它自己的真实Claude文档ER-05收据；核对实际证据，登记本终态文档自身D仍pending。它是本计划预登记终止链末端，不是第四个产品修复或新版本发布点，不替代三卡各自C/D。没有独立发布原工作区Traces治理的权限。
5. **终态登记验收。** 终态文档相对FIX03 merge树只允许上两路径；逐行证明plan-status只改本计划行，其它来源逐文件相同。该文档差异不能宣称被前面的全量覆盖：先在候选文档完成结构/链接/状态一致性验收，再获得真实Claude文档ER-05 PASS；仅追加该审查的实际session/输入hash/结论/P2处置/UTC收据，不能预填或修改其它内容。随后在含已登记ER-05收据的最终字节上重新做结构/链接/状态一致性、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`，及 `source .env.test && source .env.live-test && cargo nextest run --test compat_agent_architecture_guard --no-fail-fast --retries 0`（实际非零完整target，包含读取plan-status/long的守卫）；另只读扫描全部文档消费者，若还影响其它target则执行其完整target，输入超出此两路径或超出已登记预算先ER-03。预登记本地一次3600秒，仅这些文档相关门，无新的产品全量窗口。追加审查收据以外的任何改动都必须重新审查并重跑相关本地门；不得因此自动追加3600秒窗口，超出预算先ER-03。冻结最终文档字节，精确signed+DCO提交并核签名；走feature PR，base八job/CodeQL两job全绿后squash，核提交树=merge树=最终本地门被测树（含已登记的ER-05收据）；审查原输入与唯一后置收据差异分别记录，不声称Claude读过自己后来产生的输出，再观察merge SHA的全部适用base/CodeQL。无tag则release.yml不触发，具名N/A而非success。此文档提交的产品C覆盖仅继承FIX03已发布产品树；新增文档字节由自己的本地门与真实PR/main D覆盖。
6. **消除自引用，不静默完成。** 最终登记提交自身SHA、run ID、结论和观察UTC，只能推送后取得。依模板v2.14不再为它新建本计划提交：在下一次 `plan-status.md` 例行同步中补记，搭车既有维护提交；若到下一工作日尚无例行同步，以一笔明确的计划状态例行同步补记，属于plan-status维护，不属本计划终止链、不重启终止链。例行维护提交仍须精确signed+DCO、结构/链接/nightlyfmt/strictClippy与完整compat_agent_architecture_guard target（双env、retries0），真实Claude文档审查及PR/main base/CodeQL；不因属于维护而免门，后续若暴露其它读者则按实际影响补齐。这是库内证据落点，须实际执行；末端D未绿时不报告计划完成。原两计划接收本资格时同时登记该末端ref/D以及例行补记归属，不改原卡独立门。

任何最终登记门/PR/main D失败都保留实际FAIL、STOP并前滚处置，不为截断终止链豁免失败。计划最终资格与末端文档review都必须是实际结果，不预写未来PASS。仓库内不新增证据文件/目录；摘要、SHA、计数、run/UTC与修订历史落在上述计划正文，唯一例外是第6项模板指定的plan-status例行补记。

**终态B的只读执行器：** 使用仓外 `/tmp/libra-qualification-recovery-plan-20261025/terminal-document-check.py`（在该末端执行前生成、hash冻结并纳入真实Claude输入，不属于产品源码）。执行命令为 `python3 /tmp/libra-qualification-recovery-plan-20261025/terminal-document-check.py`，以已核FIX03 merge SHA、候选树及两路径allowlist为显式冻结输入；缺输入即失败。必须以原生Libra diff逐路径证明只有本计划与plan-status本计划行变化；全其它来源hash一致；卡片/索引状态与已发生原件逐项对应；本计划全部必需章节/三卡字段存在；相对链接解析到当前clone；已登记审查收据只作后置事实，不改审查结论或其它字节；保留所有旧FAIL/STOP。自动结构结果不替代人审语义和真实门结果。

## 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-QUAL-SOURCE | incoming | 上游revision | libra-tools/libra@0ca47cc147b2099ffb835ade91272c4cfe1efd21 | root | 独立clone的实际远端来源、允许差异及签名工具链已核；变化先重审 | 本文来源与精确比较记录 | 不追写原工作区，不用旧资格冒充新树 |
| DEP-QUAL-SECURITY | incoming | 跨仓前置 | Mega2 plan-20261015 FIX-MTR-11 | root | 既定安全卡完整C/D及两个告警实际fixed；不改变其不自行延期条款 | FIX11已done/complete，932731d/v0.42.70；23/24实际fixed于2026-10-24 20:16:51/52 UTC，见文首真实D收据（此前open保留为历史） | 未满足不实施本计划；只做恢复治理与审查 |
| DEP-QUAL-PUBLISH | incoming | 发布前置 | 独立clone与实际GitHub工作流 | root | 发布branch不包含原未发布治理祖先/文件；vault签名与DCO、版本面、PR/Release权限与artifact门实际可用 | 开工逐项原生核对，不能用gh浏览器登录互代 | 分叉/缺凭据/来源变化先停止归属，不force |
| DEP-QUAL-HANDOFF | outgoing | 来源资格 | plan-20261015 LTR-10 / LTR-01 | root | 三卡独立C/D完成、FIX03 bump后C及发布后最终资格窗口全量/doc/network与本地Claude明确PASS（绑定实际merge树）、终态登记提交实际PR/main D通过并具名例行补记，再核受控整合树、原工作区受控整合/受影响门通过 | 后续真实提交/版本/远端ref/原件，当前无 | 来源资格未闭合不得解除LTR-10前置；不更改H1或替代LTR各卡门 |

## 实施顺序与发布窗口

`DEP-QUAL-SECURITY + DEP-QUAL-SOURCE + DEP-QUAL-PUBLISH -> FIX-LTR-01 -> FIX-LTR-02 -> FIX-LTR-03 -> DEP-QUAL-HANDOFF`。顺序用于共享本计划/版本面的串行发布，不表示合并发布。先修确定性全量阻断，再修共享fixture，最后完成MCP测试清理与整份修复计划的最终资格。每卡真正 C/D 闭合后才发布下一卡。

### 实际 D 映射与现场复核

当前 `base.yml` 为PR及main/release/** push，`codeql.yml`为main目标PR及main/release/** push，均无paths过滤；实际job/if必须在每卡开工重新读取完整workflow，绑定最终head与merge SHA，不能只看workflow标题。`release.yml`由v* tag触发，当前build-and-upload有linux amd64/arm64、darwin arm64、windows amd64四个平台并通过OIDC release API上传；完整发布job/渠道探针一并核真实成功。失败前滚，不回写版本tag，不伪造未触发workflow通过。


### 三卡共同适用的逐项D清单

每卡逐项登记run/job/attempt/ref/实际终态；不得用一条workflow success代替漏项。无顶层job if的项均适用；步骤条件和后续workflow变化在开工重新核。PR被测merge-ref、PR head和main merge SHA分开记录。

| Workflow | Job | 触发事件与ref | 判据 |
|---|---|---|---|
| `base.yml` | `format / compat-rustfmt` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 格式零diff |
| `base.yml` | `clippy / compat-clippy` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | all-target/all-feature lint零warning且rustdoc links通过 |
| `base.yml` | `owner-liveness-macos / compat-owner-liveness-macos` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 真实macOS具名owner safety用例非零且全通过 |
| `base.yml` | `scoped-input-windows / compat-scoped-input-windows` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 真实Windows scoped input全部具名门通过 |
| `base.yml` | `redundancy / compat-redundancy` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 现行冗余/生成表检查全部通过 |
| `base.yml` | `test / compat-offline-core` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 实际L1+已注入L2/L3与独立feature步骤全通过，记录skip/flake |
| `base.yml` | `network-remotes / compat-network-remotes` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 真实network-remotes具名目标通过 |
| `base.yml` | `opencode-export-sandbox-linux` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | Ubuntu隔离工具及具名export sandbox门全通过 |
| `codeql.yml` | `analyze / security-codeql-actions、security-codeql-rust` | PR目标main（refs/pull/<n>/merge及head）和merge后push refs/heads/main；base与CodeQL均另适用release/** push | 两个matrix job分别success |
| `release.yml` | `build-and-upload × linux-amd64/linux-arm64/darwin-arm64/windows-amd64` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 四平台编译上传全部success，digest/size与下载artifact匹配 |
| `release.yml` | `upload-install-scripts` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 脚本上传成功、发布版本面与CDN脚本实际一致 |
| `release.yml` | `update-homebrew-tap` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 实际tap更新成功并绑定本卡版本/digest |
| `release.yml` | `verify-homebrew-formula` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 更新后formula验证job实际success |
| `release.yml` | `request-stable-manifest` | 人工创建版本tag/Release触发push refs/tags/v<本卡版本>，解引用为已核merge SHA | 四平台完整签名manifest发布success，stable/CDN实际version/digest对应本卡版本 |

## 任务卡

### Task FIX-LTR-01: 同步全局schema doctor的完整auto-upgrade结构守卫

**Task type:** `implementation`

**Lifecycle / Acceptance:** `in-progress` / `locally-accepted`（2026-10-24 21:42:59 UTC：限定本卡A通过；自己的代码ER05及C/D待，非done；下方记原runtime缺口与人工资源清理）

**Description:** 只更新现有完整结构字符串，使它同时保留schema_doctor、Upgrade和当前Ui排除，恢复确定性兼容测试；不改CLI行为。

**Out of scope:** fixture时序由FIX-LTR-02承接，MCP teardown由FIX-LTR-03承接；UI产品行为测试/解释由上游UI所有者承接，若需要改变生产逻辑则本卡停止并重新ER-03。

**Current evidence:** `tests/compat/global_config_schema_future_test.rs:727`在三次原生尝试均失败。该文件在2755eb7和0ca47cc的SHA均为e16a5fa6e3307b2dfe6eeb0827b3337faae4a4de18903363112a10829585e2ab；`src/cli.rs:3636`新增Ui排除，旧结构字符串未同步。前面的真实scope/role/path/canary断言已到达并通过。

**Acceptance criteria:**

- [x] 完整字符串包含原schema_doctor和Upgrade排除，并加入当前Ui排除；不得删除结构守卫或改为松散子串。
- [x] 该用例全部原runtime角色/路径/损坏canary/legacy断言与相邻startup recovery守卫逐字保留。
- [x] 完整compat_global_config_schema_future目标通过，生产CLI与其它测试字节不变。

**Verification:**

- [x] `source .env.test && source .env.live-test && cargo nextest run --test compat_global_config_schema_future -E 'test(=config_doctor_uses_global_role)' --retries 0`；必须精确1例、0失败、不可skip。
- [x] `source .env.test && source .env.live-test && cargo nextest run --test compat_global_config_schema_future --no-fail-fast --retries 0`；与运行前实际完整listing逐名一致。
- [x] 只读diff守卫：唯一测试语义改动为既有完整字串增加`|Commands::Ui(_)`；原runtime断言/CLI源码均不变。

**Full-suite trigger:** T-5：本卡修复新来源全量的确定性阻断；A与最终C均明确要求完整默认套件，不以单例PASS替代。

**Dependencies:** DEP-QUAL-SOURCE, DEP-QUAL-SECURITY, DEP-QUAL-PUBLISH；仅消费具名发布后来源与当前卡实际准入，前驱未complete不开始本卡发布。

**Deliverables:** N/A；测试修复及本计划同卡记录完整交付。

**Implementation write set:** `tests/compat/global_config_schema_future_test.rs`；本计划文件；`docs/development/plan/plan-status.md`仅本计划行（以实际发布上游文件为基线）。原scoped-traces三治理文件的未发布增量仅由协调者另行本地同步，不进入本卡发布分支。

**Release write set:** Inherited；以当前compat_version_surface_sync为权威的Cargo.toml/install.sh/install.ps1等实际版本面、工具链Cargo.lock及artifact；开工重核，不手改lock。

**Files likely touched:** 与I/R完全一致，不新增target或公共helper注册。

**Docs and compatibility impact:** 本计划及plan-status本计划行同卡更新真实状态；用户CLI/参数/输出/schema没有变化，双语命令指南、COMPATIBILITY、错误码与网站N/A。tests/INDEX既有目标名称/用途不变；若实际发生变化先扩写I并ER-03，不能漏文档。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无schema/数据迁移；未发布源码保留精确差异，已发布问题只用新签名提交/新patch前滚，不回退ref或版本tag。

**Security and privacy:** 只使用合成fixture与具名测试资源；不读取真实provider正文，不在证据中保存env值/token。进程清理必须证明归属，云资源按fixture唯一namespace，不清扫既有资源。

**Performance budget:** A一次7200秒；C一次10800秒；单测timeout/retry不变。一次claim，非预期失败STOP；具体执行器与输入manifest在执行前实审。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`；本卡自己的完整C/D，不免bump、不合并其它修复。

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=global-schema doctor的auto-upgrade结构oracle对齐; recovery=immutable-release前滚本卡单轴测试修复; complete=yes; self-contained=yes; AC=3/8; VER=3/8; landing=1; prod-files=0; scope=S; deps=DEP-QUAL-SOURCE, DEP-QUAL-SECURITY, DEP-QUAL-PUBLISH; writeset=三卡本计划与R串行; release=independent; split-from=新上游资格20261025实际FAIL/STOP; exception=N/A`

### Task FIX-LTR-02: 让合成reasoning fixture在索引结清后进入erase

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空（ER-03候选；无源码/测试/提交/发布）

**Description:** 在 `seed_checkpoint` 中既有writer marker退休成功后、`conn.close()`之前建立有界、可失败的索引结清前置（在调用方直接INSERT索引及CLI erase之前），保留原删除保护和所有reasoning隐私/生命周期断言；不是修改生产erase为等待或重试。

**Out of scope:** 不改HistoryManager/ClientStorage生产API，不删除/重放marker以造绿，不归因历史C2或机器负载。结构oracle由FIX-LTR-01、MCP teardown由FIX-LTR-03承接。

**Current evidence:** 现有QUAL-LTR-FLAKY在新旧来源各出现一次；本轮metadata OID689bb4bc1a6da590532651d8db132eef0b0037b3仍有durable marker，删除栅栏正确返回WouldBlock。`seed_checkpoint`使用capture_scope=None，未等待后台索引；同仓`agent_checkpoint_export_test.rs:741-747`已有有界等待先例。失败当时为何未drain尚未证明。

**Acceptance criteria:**

- [ ] 使用现有有界后台任务等待，界限不超过已有同类fixture的10秒；超界明确失败，不sleep固定延迟或重试erase。
- [ ] 等待返回后只读枚举本fixture `.libra/object-index-repair/`，目录不存在或为空才通过，存在任何marker或枚举错误均失败；不扩大私有常量/失败计数生产API。消费者失败留下marker时必须失败，安全诊断仅OID/类型/文件名，不打印body/secret。
- [ ] 原erase及GC、reasoning私密refs/export和坏digest断言保持，生产删除栅栏与队列实现不改。
- [ ] 两个直接消费者均验收：完整agent_checkpoint_export_test模块及真实云reasoning生命周期用例；真实云缺凭据不可skip通过。

**Verification:**

- [ ] `source .env.test && source .env.live-test && cargo nextest run --test command_test -E 'test(command::agent_checkpoint_export_test::)' --no-fail-fast --retries 0`；按实际完整模块listing非零逐名核对。
- [ ] `source .env.test && source .env.live-test && cargo nextest run --features test-live-cloud --test agent_cloud_tombstone_test -E 'test(=reasoning_artifact_mirrors_and_erases_with_checkpoint)' --retries 0`；精确1例真实D1/R2，保留fixture唯一repo UUID及cleanup证据，禁止early-return冒PASS。
- [ ] 检查失败路径仍fail-closed：保留marker不擦除、不放宽erase断言；只读核该helper两个直接消费者及无新增生产API。新增确定性回归如必需，须在同一行为轴/I中先登记selector并审查，不以重复跑同一flake代替因果。

**Full-suite trigger:** T-4/T-5：修改tests/helpers共享测试基建，A及最终C完整默认套件；真实云selector另跑，不被默认feature的PASS替代。

**Dependencies:** FIX-LTR-01；仅消费具名发布后来源与当前卡实际准入，前驱未complete不开始本卡发布。

**Deliverables:** N/A；测试修复及本计划同卡记录完整交付。

**Implementation write set:** `tests/helpers/reasoning_artifact.rs`；本计划文件；`docs/development/plan/plan-status.md`仅本计划行（以实际发布上游文件为基线）。原scoped-traces三治理文件的未发布增量仅由协调者另行本地同步，不进入本卡发布分支。

**Release write set:** Inherited；以当前compat_version_surface_sync为权威的Cargo.toml/install.sh/install.ps1等实际版本面、工具链Cargo.lock及artifact；开工重核，不手改lock。

**Files likely touched:** 与I/R完全一致，不新增target或公共helper注册。

**Docs and compatibility impact:** 本计划及plan-status本计划行同卡更新真实状态；用户CLI/参数/输出/schema没有变化，双语命令指南、COMPATIBILITY、错误码与网站N/A。tests/INDEX既有目标名称/用途不变；若实际发生变化先扩写I并ER-03，不能漏文档。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无schema/数据迁移；未发布源码保留精确差异，已发布问题只用新签名提交/新patch前滚，不回退ref或版本tag。

**Security and privacy:** 只使用合成fixture与具名测试资源；不读取真实provider正文，不在证据中保存env值/token。进程清理必须证明归属，云资源按fixture唯一namespace，不清扫既有资源。

**Performance budget:** A一次10800秒（含全量与真实云消费者）；C一次10800秒；已有10秒fixture界限不放大，云失败不自动重试。一次claim，非预期失败STOP；具体执行器与输入manifest在执行前实审。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`；本卡自己的完整C/D，不免bump、不合并其它修复。

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=合成checkpoint fixture的索引结清前置; recovery=immutable-release前滚本卡单轴测试修复; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=0; scope=S; deps=FIX-LTR-01; writeset=三卡本计划与R串行; release=independent; split-from=新上游资格20261025实际FAIL/STOP; exception=N/A`

### Task FIX-LTR-03: 回收MCP无helper断言用例拥有的暂停worker

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空（ER-03候选；无源码/测试/提交/发布）

**Description:** 仅修当前MCP测试的owned worker/父进程清理，成功、断言panic及可归属的失败路径均回收；不改变产品worker生命周期。

**Out of scope:** 不加生产PDEATHSIG、不改worker deadline/内存限制/子进程禁止语义，不扩大到其它MCP功能；不能声称重建本轮未保存的PID文件。其余两个独立缺陷由FIX-LTR-01/02承接。stdout-holder fixture的KillHold早期失败缺口尚未排期，具名RISK-QUAL-HOLDER；若本轮资源审计命中则STOP并ER-03登记修复，不吞掉该失败。

**Current evidence:** 本轮full退出后遗留原session3874874的worker PID4064241，PPid1、独立PGID、deleted fixture cwd；root经pidfd核身份后SIGTERM，进程实际消失。精确原case未采集，不能以时间重合证明。静态`read_worker_status_spawns_no_helpers`在703-704行仅kill/wait父进程，而exec.rs:141让worker拥有独立进程组，pause hook永久等待，构成明确清理缺口。原遗留进程也不能排除stdout-holder fixture；其KillHold在191-204行前的失败路径无guard、kill -9不核身份是另一处已知静态风险，不声称本卡能证明历史PID来自哪个case。

**Acceptance criteria:**

- [ ] 保留descendant_pids(worker).is_empty原断言；取得worker PID后先建立具有身份核验的cleanup guard，再执行可能panic的断言。
- [ ] 成功/panic以及PID取得前后的失败路径均处理当前测试拥有的父进程与可证明归属的worker；不信任复用PID、不杀其它用例或系统进程。
- [ ] 保留pause文件发现上限10秒，cleanup退出观察上限1秒；先向核身份的owned worker/其独立组发退出信号，再kill+wait MCP父进程，避免先杀父丢失归属；不把父进程wait成功当worker清理证明；Linux核 `/proc/<pid>/stat` 的starttime/state，区分已退出僵尸与活进程并另记是否已回收，不以kill(pid,0)独证；其它平台用实际进程身份/退出能力，不能照搬procfs或跳过原测试；现有descendant_pids在非Linux上恒为空，不能当归属证据，若当前I/现有平台API不足以建立安全身份，必须在claim前ER-03。PID文件取得前先枚举MCP父进程的真实子关系并核身份（Linux覆盖所有 `/proc/<mcp>/task/*/children`），再执行清理；不能先杀父进程再猜孤儿归属。不提高测试超时。
- [ ] 完整agent_mcp_exec_test通过，原deadline、内存、slot、禁止helper断言全部保持；本卡bump后C全量/doc/network通过，实际本卡Claude ER-05通过；三卡D之后的计划最终资格属于计划完成门，不倒挂本卡done条件。

**Verification:**

- [ ] `source .env.test && source .env.live-test && cargo nextest run --test agent_mcp_exec_test --no-fail-fast --retries 0`；完整目标按listing逐名核验。
- [ ] `source .env.test && source .env.live-test && cargo nextest run --test agent_mcp_exec_test -E 'test(=read_worker_status_spawns_no_helpers)' --retries 0`；精确1例。成功路径核owned PID/start identity/PGID与exit；失败路径逐分支静态审查spawn/write/try_wait/读PID/parse/panic及Drop顺序，不宣称已做错误注入运行；不运行无界循环或额外重试。若当前I不足以确定安全归属，先ER-03停止，不能退化为按进程名批量kill。
- [ ] `source .env.test && source .env.live-test && cargo test --doc`；原doc目录/计数/ignored与实际退出核实。
- [ ] `source .env.test && source .env.live-test && cargo nextest run --features test-network --test network_remotes_test --no-fail-fast --retries 0`；精确原3例，无skip/过滤。其oracle分别是refs、有限PACK标头、LFS batch JSON，不冒称完整对象下载。

**Full-suite trigger:** T-5：本计划最后一个独立修复，A及最终C完整默认套件并完成doc/network；不是把前三卡聚合为一次发布。

**Dependencies:** FIX-LTR-02；仅消费具名发布后来源与当前卡实际准入，前驱未complete不开始本卡发布。

**Deliverables:** N/A；测试修复及本计划同卡记录完整交付。

**Implementation write set:** `tests/agent_mcp_exec_test.rs`；本计划文件；`docs/development/plan/plan-status.md`仅本计划行（以实际发布上游文件为基线）。原scoped-traces三治理文件的未发布增量仅由协调者另行本地同步，不进入本卡发布分支。

**Release write set:** Inherited；以当前compat_version_surface_sync为权威的Cargo.toml/install.sh/install.ps1等实际版本面、工具链Cargo.lock及artifact；开工重核，不手改lock。

**Files likely touched:** 与I/R完全一致，不新增target或公共helper注册。

**Docs and compatibility impact:** 本计划及plan-status本计划行同卡更新真实状态；用户CLI/参数/输出/schema没有变化，双语命令指南、COMPATIBILITY、错误码与网站N/A。tests/INDEX既有目标名称/用途不变；若实际发生变化先扩写I并ER-03，不能漏文档。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无schema/数据迁移；未发布源码保留精确差异，已发布问题只用新签名提交/新patch前滚，不回退ref或版本tag。

**Security and privacy:** 只使用合成fixture与具名测试资源；不读取真实provider正文，不在证据中保存env值/token。进程清理必须证明归属，云资源按fixture唯一namespace，不清扫既有资源。

**Performance budget:** A一次10800秒（含最终全量/doc/network）；C一次10800秒（含bump后完整全量/doc/network）；不提高原测试timeout或retries。一次claim，非预期失败STOP；具体执行器与输入manifest在执行前实审。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`；本卡自己的完整C/D，不免bump、不合并其它修复。

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MCP read-worker测试夹具完整teardown; recovery=immutable-release前滚本卡单轴测试修复; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=0; scope=S; deps=FIX-LTR-02; writeset=三卡本计划与R串行; release=independent; split-from=新上游资格20261025实际FAIL/STOP; exception=N/A`

## 完成判据与交接

三卡分别实际A/B、Claude ER-05 PASS、精确signed/DCO提交、独立patch/PR/merge/tag/人工Release、实际CI与发布artifact D完成；全部P0/P1关闭，P2具名处置。FIX-LTR-03 的C④在bump后树执行完整默认套件、`cargo test --doc`及原3例network；merge树与被测树不等即STOP。三卡各自实际D完成后，在与FIX03发布merge树相同的冻结来源执行上述一次10800秒计划最终资格窗口（fmt、strict Clippy、双env完整默认套件、doc、原3例network），核全部身份/退出/ignored/flake/runtime skip与owned资源；根因未知的缺口不得改成已证。最终实际本地Claude来源资格PASS后，才把三个具体发布SHA/版本及适用覆盖交给原scoped-traces计划；其H1、原merge签名政策、LTR自身A/B/C/D仍独立。

原两份计划正式计数仍Mega14/112、Libra0/26，联合14/138（活动14/136）；本外部修复计划单列0/3，不把必要修复藏进原26卡或擅改旧历史计数。联合依赖审计须把这3个外部节点展开，检查无环和入场前沿。只要任一卡或交接缺少实际证据，本计划未完成。

## 修订记录

| 日期 | 触发 | 变更 | 实际状态 |
|---|---|---|---|
| 2026-10-24 19:47:13 UTC | 新上游资格full实际FAIL、既有flake复现与手工清理的worker | 三个独立测试修复、隔离发布与原计划incoming交接 | 本地Claude ER-03/H0待审；没有执行或发布 |

## 任务卡粒度审计表

| 卡 | 行为轴 | I（测试+同步文档） | AC/VER | landing/prod | 依赖 | release/C-D |
|---|---|---|---|---|---|---|
| FIX-LTR-01 | doctor完整结构oracle | global_config_schema_future_test.rs、本计划、plan-status本计划行 | 3/8、3/8 | 1/0 S | DEP-QUAL-SOURCE/SECURITY/PUBLISH | independent/self |
| FIX-LTR-02 | reasoning fixture索引结清 | reasoning_artifact.rs、本计划、plan-status本计划行 | 4/8、3/8 | 1/0 S | FIX-LTR-01 | independent/self |
| FIX-LTR-03 | MCP owned worker teardown | agent_mcp_exec_test.rs、本计划、plan-status本计划行 | 4/8、4/8 | 1/0 S | FIX-LTR-02 | independent/self |

## 风险登记

| ID | 事实/边界 | root处置/重启条件 | 状态 |
|---|---|---|---|
| RISK-QUAL-FUTURE | proc-macro-error2现有future-incompat提示；旧Clippy exit0不等于无诊断 | 原件保留；任何适用发布门拒绝则STOP独立归属，不allow压制 | open P2 |
| RISK-QUAL-FLAKE | marker为什么未drain尚未证；fixture边界缺等待有源码证据 | FIX02严格有界等待并独立核marker，真实云消费者；不能宣称证明历史C2 | open，FIX02验收待 |
| RISK-QUAL-HOLDER | 历史PID不能排除stdout-holder；KillHold早期失败缺口及裸kill -9已知 | root接受为FIX03当前范围外残余风险，尚未排期；若实际资源审计命中或当前路径失败，立即STOP并ER03新增修复，不以手工kill当自动PASS | open P2，非历史PID确定归因 |
| RISK-QUAL-CACHE | 独立路径可能使Cargo重新冷编译，暖缓存时间不保证 | 独占缓存、记录配置；安全复制不可用或需超候选预算则claim前ER03；运行超时STOP不自动续时 | open P2 |
| RISK-QUAL-MERGE | PR期间upstream/tree/版本可能变化；原unsigned merge未处理 | 比较被测tree与merge tree，不等STOP；原merge仅原工作区后续解决，不进入修复祖先 | open，硬门 |

## Claude review log

按用户指令单用本地Claude，替代模板reviewer名称，不降低实审或PASS要求。

| Round | Scope | Result | P0/P1 | P2处置 | Evidence |
|---|---|---|---|---|---|
| R1 | ER03/H0候选 | FAIL | P1-1顺序表漏门、P1-2状态登记缺失、P1-3发布树资格未绑定 | R2逐项修订；残余风险由root具名登记 | session04ce4421-a050-4bf3-a003-7662306ee05f；output SHA256 52d6588527ba5c8a16b96072e6857cf0aa60576ba7bcb28d7bcab270f962f96f |
| R2 | ER03/H0恢复候选 | FAIL | 原三P1已闭；新终态证据/被测树P1 | R3前滚修订，未执行产品窗口 | session e9f0e17d-07b4-40d8-9098-3f2a6e68c857；output 92b3745fc0f02790221e06d6db9f80ce56acab39d945378cfeeb23cab6e6c5ba；H0 PASS |
| R3 | ER03/H0终止链候选 | FAIL | 终态ER05收据无落点 | R4闭合，原FAIL不改 | session 82ca0f53-d8f6-4148-af66-13721b381b95；output 8f15fea4fd997f6157f3a82d323b286a9838b91fe8ef5e960a05be1baaa9876b；H0 PASS |
| R4 | ER03/H0最终恢复方案 | PASS | 无 | root具名逐项处置如下；最终事实字节待DOCS-ER05/H0实审 | session 67756716-0e46-4d90-bf70-fd23dff6b918；output 48f93079f925f4129fa9523b78a5cf2bae123e9aa4fab890be11b9b15dd793a2；manifest 925901de4eac79a2d4a1e12540bfff0bd24417c42158a9606875041ff7b222ed |

R2实际ER03 FAIL/H0 PASS，session e9f0e17d-07b4-40d8-9098-3f2a6e68c857，output SHA256 92b3745fc0f02790221e06d6db9f80ce56acab39d945378cfeeb23cab6e6c5ba；原三P1已闭，新的终态证据/被测树矛盾由本轮预登记终止链修订。**R3时点快照：** 当时R3尚待实际审查；后来R3 FAIL与R4 PASS见实际日志，不倒改原结论。P2-7已实际核GitHub上游存在旧plan-20261015（blob bfcf4e20aff8289dce9c7a0889e5ebb9ddb926f9）；不能因此认为含当前本地治理。新计划改用纯文本说明原计划；发布clone仍须自身链接核验。

### 本轮前滚修订历史

| 时间 | 触发 | 变更内容 | 原卡→新卡 | 受影响引用 |
|---|---|---|---|---|
| 2026-10-24 19:47:13 UTC | 本地Claude R1 FAIL | 同步五门、状态登记窄写集、bump后C及发布后资格树、逐项D与安装缓存界限、风险/审查/粒度表 | 三卡编号/轴不变 | 两仓8份治理、外部新计划、原LTR10顺序/卡片/审计表；R2待审 |

### R3审查修订

| 时间 | 触发 | 变更 | 卡/依赖影响 | 当前事实 |
|---|---|---|---|---|
| 2026-10-24 19:56:18 UTC | R2 ER03 FAIL/H0 PASS | C前冻结pending事实、C到merge零文档漂移，后继补记C/D，v2.14终态纯文档提交/例行补记，FIX03自身与计划完成门分离 | 三卡编号/轴/依赖不变；新增末端文档两路径门与一次3600秒预算，非新产品发布卡 | R3候选；未实施/测试/提交/发布 |

**R2残余P2处置（root）：** 非Linux与PID取得前的身份能力在claim前核，不足则ER03；D的release/**措辞修正。来源存在性现以独立clone HEAD=origin/main=`0ca47cc147b2099ffb835ade91272c4cfe1efd21`为冻结锚点，不能把未绑定ref的旧GitHub查询当精确来源证据。独立clone已核干净、独立SQLite、4702个祖先不含ed566b0/2755eb7；身份已从继承的测试默认值改为Eli Ma <eli@patch.sh>，本地签名开启、公钥1067ADBC49C4E7D18BA154AC21594333739ACBAF，尚无新提交签名验证。旧target927GB、reflink不支持，缓存复制/实际冷编译预算尚待claim前核定，不以方案PASS跳过该门。

**上游发布状态行边界：** 独立clone的plan-status本计划行只写本计划的三卡状态、实际来源/验收/版本/提交/UTC；不复制原工作区的scoped-traces0/26、两仓治理R轮或其未发布状态。本文自身R1/R2/R3实审记录是本独立计划的历史，可以随本计划发布。原工作区本地索引中的交叉前沿只由原Traces承载。

**B证据能力：** 自动检查证明字段/链接/依赖表一致、历史字节保持与写集边界；`published_tree_qualification_bound`等布尔项只是合同存在检查，不能证明未来运行或语义正确。终止链的可执行性由本轮实际Claude审查，未来树/hash与退出由真实执行核验。

### R4前滚与历史纠正

| 时间 | 触发 | 变更 | 结果 |
|---|---|---|---|
| 2026-10-24 20:03:41 UTC | R3 ER03 FAIL/H0 PASS | 终态ER05先审、仅补记收据、最终字节本地门、三树相等；STOP时前一卡C/D落点；例行维护自身门；只读B脚本合同 | R4待真实审查，零产品执行 |

R3真实session `82ca0f53-d8f6-4148-af66-13721b381b95`，output SHA256 `8f15fea4fd997f6157f3a82d323b286a9838b91fe8ef5e960a05be1baaa9876b`，ER03 FAIL/H0 PASS，原件保留。R3的候选生成器把R2已写的两条历史行时间从2026-10-24 19:47:13 UTC改成19:56:18 UTC；本轮恢复原始行时间，仅本轮新行使用新时钟。不倒改R3原件或把其失败写成成功。

**RISK-QUAL-TERMINAL-BASE（root，P2）：** FIX03 merge之后main可能前进，使终态树相对冻结资格树不再只有两路径。必须STOP并ER03审新差异/所需门；不能换基线后继承旧PASS、force或忽略新增来源。该风险可能延长交付，但不降低资格范围。

**签名证据边界（root，P2）：** 新clone仅完成本地身份、公钥与signing设置。首次实际提交后必须在同一个SHA核DCO、gpgsig并以导出公钥实际验签，失败不得push；GitHub签名verification单独按实际回执记录，不把本地开关或新公钥当成GitHub已信任。若实际仓库策略要求额外信任/注册，先解决后发布，不绕过规则。

### R4 PASS后的P2具名处置与治理自身审查

| 时间 | 事项 | root处置 | 边界 |
|---|---|---|---|
| 2026-10-24 20:09:49 UTC | P2-1 ISO历史横幅 | 逐字去时钟比较R1/R2后，将两仓8份文档共同横幅恢复至首次相同文本的原时间；R4未覆盖的ISO时间问题在本轮纠正，UTC两行恢复事实保持。原R1/R2/R3/R4候选及审查输出不改 | 本轮真实修改范围由最终diff/hash绑定，不把R4对旧字节PASS扩大到尚未审的修订 |
| 2026-10-24 20:09:49 UTC | P2-2 review log与旧状态 | 表内补R2/R3/R4真实结论、session/hash，R3待审句标历史快照；当前索引明确R4方案PASS、治理自身ER05待 | 不填未来审查/验收PASS |
| 2026-10-24 20:09:49 UTC | P2-3 后续例行维护 | 维护提交照终态第5项顺序：先文档核验及真实Claude，收据仅写plan-status本计划行，再对最终字节跑相关本地门，核提交/merge/被测树一致。执行前必须在当次维护写集中登记其独立的一次窗口及预算并实审，不挪用本计划终态3600秒、不自动再开；目前维护尚未执行 | 属v2.14例行维护，未获自己的实际执行准入，不影响本次方案审查；缺准入不得执行 |
| 2026-10-24 20:09:49 UTC | P2-4 缓存复制 | 原cache-selection只是开始前的依赖文件子集11178；实际复制31147还包括第三方build/.fingerprint，排除libra-*目录、第一方库/测试二进制、incremental、硬链接。当前复制仍在运行；必须取实际终态、逐项核数/排除与独占来源后才能claim | 缓存不是源码/测试资格，也未消耗或获得新产品验收PASS |
| 2026-10-24 20:09:49 UTC | P2-5 执行期残余 | 后继STOP补记、终态基线变化、同SHA验签/远端信任与终态只读执行器冻结均保留硬门 | 三卡和末端执行均未开工 |

本次R5仅对以上事实修订及两仓治理自身做真实本地Claude DOCS-ER05/H0审查，并确认R4方案准入没有规范回退；结果未取得前不宣称治理自身review完成。两仓原治理写集及MTR01/LTR01承载关系不变，新独立计划未来随自身三卡发布，原工作区仅本地保存。


### FIX-LTR-01 A终态及具名限制（2026-10-24 21:42:59 UTC）

- 单一A claim `2026-10-24T20:33:43.564369Z`，终态 `2026-10-24T21:39:46.332180Z`，3962.768秒/7200秒，执行器与原nextest配置/timeout/retries未改；原资格FAIL/STOP及耗尽的旧窗口保留，没有自动重跑。输入manifest2126项 SHA256 `eb26c80a4cc4b05274a1349b20e8ee59ec5dc56e337bd2aaa0da096b5e363dde`；终态SHA256 `c393a8baaf2cf741e19c1b4cac91fc1ad0bcaf9fd865b865f881670bf284a0cf`；root限定A收据SHA256 `e92bbed501956b8019ecadfae14cfbd2e2c7c6d3d652fdf391fe6baeda64692d`。
- 自身准入来自真实本地Claude session `f8b78ffe-b7d4-4459-a633-333bf24c55b0`，EXECUTION/VERDICT PASS，仅A准入、不是本卡代码ER05。自己的最终代码ER05仍待；未使用隔离Codex。
- 三个执行集合按listing/JUnit逐(target,case)核实：exact1、完整compat_global_config_schema_future54、默认全量10761；各自0失败、0framework skip、0flake，未丢失/额外case。全量10782个列举身份中原21 ignored保持逐名相同。full stderr SHA256 `c22ec3f2f66d6e9e7c3939a03cebffdcaee1e9b8a4fc764e5d19c7c080d4b1de`；JUnit SHA256 `80f85859f67f676cb7632bde4d81cc5a2019b866bfdca7a7d3e7089a3be44953`。本卡唯一测试差异仍为完整字串增加Ui排除，生产cli和所有runtime/startup guard不变；测试文件SHA256 `535f31b6aa50c500ad9fe3876e893e8fb02581caef83a6aef2e23ed13ae000bd`。
- **P2-QUAL-RUNTIME（owner root）：** 当前38个标记与原来源逐身份/文字相同，来源hash逐项核实；1个是`opencode_plugin_skip_visible_when_interpreter_missing`的真实负向assert，不是跳过。其余37个包含：9个default-feature下未启用test-live-cloud、10个显式GitHub live未配置、12个Mega2 browser live未配置、1个Mega LFS endpoint未配置、2个大小写不敏感文件系统前提、1个当前build拒绝stage非UTF8文件、Bun与PowerShell各1个。保持已登记默认全量命令并披露缺口，不称已证明这些行为；本卡exact/完整target没有runtime skip。FIX02必须另跑具名真实云用例；H1/LTR的live门不被本卡覆盖。原件 `/tmp/fix-ltr01-A-20261025/root-terminal-audit.json` 及 `runtime-skip-source-reference.json`；自己的Claude必须独立判断此限定是否充分。
- **P2-QUAL-WORKER（owner root）：** 本次唯一新PPid1进程为PID2074789/start_ticks10678129、session1941756，exe属于本clone且fixture cwd已deleted；命令为read-worker，STATUS_PAUSE=1及本fixture pause.pid、无READ_DEADLINE_MS，匹配当前`read_worker_status_spawns_no_helpers`的唯一环境形状，区别于带2000ms deadline的另一调用点。没有原PID文件，不倒推旧历史PID4064241身份。终态后pidfd绑定身份、单次SIGTERM，`2026-10-24T21:40:25.108156Z`确认退出并已回收，全量baseline差集无其它新孤儿/owned残留。人工清理SHA256 `456b49ad48fc97b83ceb29025f3b465703f5ffb6b609742659dea36cf5dfe70b`；原资源失败事实保留，不宣称自动cleanup通过、FIX03修复或最终来源资格完成。此处仅处置已登记FIX03风险；自己的Claude若判P0/P1则STOP，不以root的P2归属代替PASS。
- **P2-QUAL-COMPILER（owner root）：** strict Clippy exit0；既有`proc-macro-error2 2.0.1` future-incompatibility提示仍存在，不把“零lint失败”写成“零任意诊断文本”。若后续C门对此有新的禁止条件，先独立归属，不静默免门。
- C入口只读预检：远端main仍0ca47cc，三版本面0.30.86一致；账户有push/admin权限但不使用admin bypass，release environments无人工review要求。两clone均没有release target，不能把debug cache冒充release暖缓存；C一次10800秒仍未claim，须以实际冷release条件和本次66分钟A重新进行真实Claude执行准入/预算判断，不在窗口外预编译、不自动延长。草稿 `/tmp/fix-ltr01-C-20261025/run-C.py` 尚未获准或执行。
- FIX02外部条件只读预检在 `2026-10-24T21:30:04.500055Z` 完成：D1仅SELECT常数、R2仅HEAD既有bucket，均HTTP200；无fixture/用户数据读写，无secret值日志。它不是FIX02的真实写入/erase/cleanup验收，更不是本卡full的live覆盖。
- 当前代码完成、A通过；自己的ER05待、提交无、push无、CI/Release无；本卡仍in-progress/locally-accepted，正式0/3。B按implementation无额外类型门，但两文档结构/链接、状态行only与原生产字节保持须在自己的审查前再核。C前只允许追加实际自己的review收据、更新本计划状态行，然后按已登记版本/bump/最终树门执行；没有放行后继实施或借原工作区治理推送。


### FIX-LTR-01 自身ER-05实际收据及C准入条件（2026-10-24 21:54:29 UTC）

- 真实本地Claude于2026-10-24 21:48:29 UTC返回`ER05_VERDICT: PASS`及`VERDICT: PASS`，P0/P1无；session `c7fb3f6b-3738-4d33-830a-be70172cd39f`，输入manifest SHA256 `504664b7f68d4b13f6f02703543fee559db322f45de7580d978f1d40397467e7`，输出SHA256 `8b43a841bf8566e6a3692601e6c0a13643dc49bfe5534005c4ec31a6177d83a1`。原件 `/tmp/fix-ltr01-own-review-20261025`；root实际重算manifest/output及逐输入hash，未漂移。Claude明确披露未自行计算hash，不能把root核验写成Claude独立计算。该结论只覆盖既定源码/文档和已发生A终态，不代表C准入或C/D完成。
- P2-1由root承接：37项runtime early-return缺口只是打印标记识别的下限，不穷举静默early-return或未启用feature；FIX02及最终资格必须保持此限定和具名live门。1项负向assert与37项缺口分开。
- P2-2由root承接：C执行期间如发现与本次A同样形状的已登记FIX03孤儿，仅允许在终态后、核本窗口观察到的PID/start identity、nextest session、独立PGID、read-worker argv、已删除fixture cwd及STATUS_PAUSE/pause.pid/无READ_DEADLINE_MS后，以pidfd单次SIGTERM人工回收并记录exit/reap；不发组信号、不按进程名清扫、不自动重试。任何其它形状、多个未能唯一归属的进程或stdout-holder（RISK-QUAL-HOLDER）立即STOP/ER03；不以手工清理冒充自动cleanup PASS。FIX03后的最终来源资格必须零孤儿。C执行器只记录并拒绝未知残留，不自行执行人工清理。
- P2-3由root承接：保留proc-macro-error2的future-incompat提示，strict Clippy实际exit0不扩称零诊断文本。P2-4的旧候选句/横幅格式在FIX02开工的既定文档写集中标为历史，本次不改已审正文。
- P2-5由root承接：cold release、实际A耗时及C一次10800秒上限必须另获真实本地Claude执行准入；尚未claim、bump或预编译，超时/失败STOP，不追加或转移旧窗口。只冻结本次实际后置收据和本计划status行后，再由C审查绑定最终字节/执行器/环境控制。
- 审查边界披露：Claude为核计数使用Grep读取full.stderr的Summary、两条MCP PASS和FLAKY/TRY计数，并读Clippy提示两行，超出原prompt禁止原始测试输出的约束；未读.env/credentials/JUnit正文。保留原输出及披露，不伪造为完全遵守；后续准入明确允许这些无secret的受限摘要。
- 当前：源码及A通过、自身ER05 PASS，Lifecycle/Acceptance仍in-progress/locally-accepted；提交、push、CI/Release与C/D均未完成，正式0/3。本文收据为审查后的事实追加，不声称Claude事先审过自己的输出；未来C/D由FIX02按既定终止链补记。
