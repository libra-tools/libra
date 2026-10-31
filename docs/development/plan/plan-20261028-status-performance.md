# status 正确性与性能优化任务计划（2026-10-28）

> 模板：`plan-template.md` v2.14。创建时间：2026-10-28 01:56:39 UTC。源码基线：`380c262257c5b4eae0725898444bb092bb86919e`；分支 `remove-status-speedup-proposal`。任务与 Issue 已建立；用户随后授权提交并推送计划文档，本次不实施代码、不发布版本。总览 Issue：[#638](https://github.com/libra-tools/libra/issues/638)。

## 文档职责与范围

以建卡时位于根目录的 `LIBRA_STATUS_PERFORMANCE_REVIEW.md`、`LIBRA_STATUS_PERFORMANCE_REVIEW_AUDIT.md` 和用户后续决策为输入，规划可独立验收的修复与优化。两份报告已由用户移出工作区，不纳入本次提交；此次不重新访问外部副本。本计划正文与 Issue 自包含，不依赖未发布报告链接才可理解。

**非目标：** CE_VALID 默认值及历史标志修复由他人负责，本计划不修改策略、不添加前置依赖。无机会性 status index 写回，无新后台服务，无 SQLite migration，无直接修改全局配置。pack 会话缓存、marker 合并、stat/hash IPC 批处理暂缓。

**成功定义：** 两项正确性缺陷修复；核心路径减少重复工作且输出/错误/安全语义不退化；收益有前后对照，未设置未经测量的 Git 性能追平目标。可选项可以保持 deferred，不阻塞核心轮次完成。

## 事实基线与审计校正

- restore 批量 size 缓存将加载预算 `peak_bytes` 当作 decoded size；缓存只预取未 tracked 路径，不能声称所有更新条目必然受影响。
- status 安全加载根 tree 后，递归子树仍进入会 panic 的 `Tree::load`。
- 默认 rename 和 porcelain v2 可重复展开 HEAD；原报告 `--no-renames` 基准不代表默认命令全部成本。
- `path::objects()` 仍走 ambient storage discovery；缓存 storage 前计算路径也有成本。
- `ignore_sources_for_target` 从 target 的父目录开始，因此检查目录并未预读目录自身的 ignore 文件；仅根预热不能证明 tracked 短路等价。
- 出队 marker probe 明确承担防目录替换的重新验证，不可直接删除；线程池提交次数不能证明固定线程切换下界。
- 审计表的 9,647 次 readlink 属于 242 次总发现的归因，不能全部算给 206 次对象读取。
- 约 733→491 ms、关闭 untracked 约省 200 ms、约 12 万次 idx read 均是历史报告引用。本轮未读取工作区外的原始归档，未独立复算，不承诺各项收益可相加。

## 设计决策与优先级

| 决策 | 约束 |
|---|---|
| ADR-SP-01 | 正确性先行：panic、size 契约为 P1 必选；CE_VALID 完全排除。 |
| ADR-SP-02 | 本轮核心：stat、HEAD 复用、request storage scope、pack 基础查询、ignore 上下文。 |
| ADR-SP-03 | SP-04 为建议可选项，默认本轮不启用；延后缓存/marker/IPC 需复测和新实施决定。 |
| ADR-SP-04 | 不移除 deadline、beneath/no-follow、racy-clean、逐路径错误及部分结果保护以换取速度。 |
| ADR-SP-05 | 现有 load-cost 接口保持保守预算语义，新增 decoded-size 能力；避免误伤 preview 限额。 |

## 与现有工作关系及依赖登记

2026-10-28 查询上游 open Issues 和标题含 status 的历史 Issues，未发现同范围开放任务；#470/#476 等为已关闭的 mode/worktree 兼容工作，不重复开启。#611 为 add/fsck 性能工作，与本计划不等价。`plan-status.md` 未检出同名 status 性能计划。`plan-20261025-qualification.md` 涉及 client_storage/util，实施前必须刷新冲突窗口。

| ID | 产物/负责人 | 可用标准 | 不可用处理 |
|---|---|---|---|
| DEP-SP-STORAGE | 现有资格计划的 storage/util 行为与写集；负责人由该计划登记，不在此虚构 | 开工核验其当前源码及在跑写集，无 I–I 冲突；配置失败不降级的回归可运行 | 暂停 SP-06，先协调顺序，不复用旧 PASS 代替当前验证 |
| DEP-SP-DOCS | 网站仓库 cf 文档交付；负责人待实施指定 | 在用户授权路径内核实 VCS、cf、逐文件路径及无冲突，完成 sync/check、相关前端验证、Claude Code PASS、部署验证 | 公开行为卡不得标 complete；当前仅列候选文件，未访问外仓 |
| DEP-SP-OPT | 用户/维护者对 SP-04 的启用决定 | SP-09 后有明确 adopt 决策及写集重新审查 | 保持 optional pending，不阻塞核心完成 |
| DEP-SP-DATA | 历史 Linux 原始归档或等价隔离 fixture；负责人待指定 | 访问范围获授权、数据/命令可核验；否则选择本工作区临时 fixture 建立新基线 | 不访问相邻目录；历史数字维持引用，不能声称复现历史样本 |

Nowledge 定向检索已尝试，服务连接被环境阻止；本计划依据当前仓库，不依赖未取回的历史结论。

## 全局执行约束与字段默认

- 建卡时各卡 Lifecycle=pending，Acceptance=空且没有 agent-ready 标签；当前状态以任务卡为准。2026-10-29 用户已选定并授权 SP-01 实施；未授权的后续卡不因此获得开工权限。实施前刷新源码、计划、Issue、写集冲突和环境；先复现失败再修复。任务字段列明的 `(new)` 测试必须在同卡新增，过滤零命中不算通过。
- 每次推进同一变更同步本计划与 `plan-status.md`。证据只汇总在本计划，不创建新证据目录/文件；工具临时运行文件不作为独立交付产物。
- 全局继承模板 GC-*、ER-*、G-*；新测试复用现有 target，若需要新增 target/serial lane，先更新卡写集及 `tests/INDEX.md`/registry 等再实施，不暗自扩张。
- **用户资源硬门槛（2026-10-30 澄清）：32G 内存门槛仅约束本机 nextest 执行；本机启动前核验物理总内存及当前资源压力，低于 32G 或无法确认则不启动，不通过降低并发绕过。GitHub Actions CI 不在此限制内，按现有工作流触发全量测试，不要求修改 CI 或增加资源预检。该澄清不授权自动 SSH 到其它主机；本机 not-run 与 CI 实际运行结果分别记账。**
- 全量门采用 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`；只在相关环境已安全核验后运行，不能打印 secrets。每次会推送的卡须 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`。全量失败需前滚修复并重跑，focused 通过不能冒充全量。
- implementation 默认 independent，Version increment=patch；Release write set=权威版本面集合（`compat_version_surface_sync`）、工具链更新的 Cargo.lock、artifact。实际提交/推送/Release 需后续授权，本次建 Issue 不包含这些动作。
- implementation 默认 Rollback mode=immutable-release：未发布可回退本卡实现，已发布用修复版恢复；无 schema 迁移，不自动重写历史 index。下载旧版本无法撤销已写出的 stat，必要时安全刷新，不能保留可疑 clean 缓存。
- 默认 Docs and compatibility impact=N/A 仅适用于输出与错误完全不变的内部优化；公开行为变化卡须同卡双语与网站文档。网站路径为候选，DEP-SP-DOCS 核验后才允许开工；遵循用户 standing instruction 直接 cf，通过 Libra 提交推送，不能另建网站分支/PR/worktree。
- 无家族合并发布；C/D coverage=self（实现卡），SP-09 继承下列具体实现卡。SP-00 按用户批准的 EX-SP-00-AUDIT-01 独立文档验收（见下）；该例外不免除评审、fmt、Clippy 或本卡 PR CI，也不授权其它卡开工。

### SP-03 验证数量与CI收口决策（EX-SP-03-VER-01 / EX-SP-03-CLOSE-01）

2026-10-31，用户在“批准数量例外→提交推送建PR跑CI→收口”的建议上明确要求顺序改为“1→3→2”：把本地整改结果、最终CI成功的完成判据及回执的后续登记方式先写进同一提交，以免CI成功后另建进度提交触发第二轮CI。本决策只针对SP-03本仓库修复交付；本机nextest仍not-run，网站与独立版本发布仍未执行，不冒充完整发布或网站部署完成。当前修复卡在同一最终PR head的适用base.yml与CodeQL全部成功后，满足本次用户批准的收口条件；成功回执按模板v2.14随下一次例行状态同步登记，不专建成功记账提交。

| ID | 豁免项/范围 | Approver | Review round | 证据与有效期 |
|---|---|---|---|---|
| EX-SP-03-VER-01 | 仅SP-03 Verification列允许15/8；全部15个验证门保留 | 用户（本线程“顺序是1 3 2”） | R-SP03-FINAL | 已核对三模型整改关闭证据、指定门计数和单一stat信任/失效轴；仅当前SP-03交付有效，不扩展到其它卡，不免除全量与静态门 |
| EX-SP-03-CLOSE-01 | 当前修复交付先预登记本地完成/CI待验，再统一提交推送建PR；全量证据由同一最终PR head的CI取得，不要求本机先跑nextest；免除为CI成功回执专建记账提交 | 用户（本线程“顺序是1 3 2”） | R-SP03-FINAL | 本机16GiB及两份env缺失仍为not-run；当前候选全量CI不预称通过。仅本次本仓库修复与PR交付；网站按DEFER-SP-DOCS-03延期，独立发布未授权、不执行 |

### SP-02 修复卡收口决策（EX-SP-02-CLOSE-01）

2026-10-30 03:06:14 UTC，在本仓库实现、单元/集成/兼容守卫、EN/zh-CN 命令文档及 `fmt`/严格 `Clippy` 通过后，用户明确要求“首先设置任务完成，然后再提交、推送、创建 PR，版本不变”。本次按用户指令登记 `SP-02` 为 `done / complete`，保持 `Cargo.toml` 版本 `0.30.101` 不变；这是对模板 ER-04/ER-08 完整发布切片及推送后远端门顺序的本卡具名例外，不扩展至其它任务卡。

- 网站 `restore` 文档交付继续由 `DEFER-SP-DOCS-02` 跟踪（仍未执行），独立 `patch` 版本递增、构建安装、tag/Release 与发布后置门转入 `DEFER-SP-REL-02`（仍未执行），均不伪称已完成。
- 本机 16 GiB 内存硬门槛下的 `nextest` 保持 `not-run`；PR CI（`.github/workflows/base.yml` 与 `.github/workflows/codeql.yml`）在本次提交推送并创建 PR 后触发，不在提交前预先宣称远端检查或合并通过（后续远端回执按模板 v2.14 在例行同步补记）。

### SP-01 已知上游 CI 失败合并决策（EX-SP-01-UPSTREAM-CI-01）

2026-10-30，用户在核实失败来源后明确要求“这个问题提出一个 Issue，然后合并 PR 并完成本卡片”。上游问题已登记 [#656](https://github.com/libra-tools/libra/issues/656)，本次授权仅允许 PR #655 在披露该已知上游失败的前提下合并，不修改守卫、不提高基线、不修复 Hermes，也不把该次运行记为全绿。SP-01 保持 done/complete（修复卡口径），网站及发布延期项保持原状。

- head `d9b5eeeb4fab47062088f819732922a410bfdf15` 的 base run [40973355588](https://github.com/libra-tools/libra/actions/runs/40973355588) 失败：10875 项执行，10874 passed（2 flaky）、1 failed、23 skipped，572.500s；唯一最终失败是 `compat_agent_architecture_guard::live_capture_shared_runtime_is_provider_neutral`，三次均报告 extraction.rs 的非测试 AgentKind 引用 16 超过冻结基线 15。其它适用 PR 检查、Clippy、CodeQL 均成功。
- checkout 实证：CI 测试 merge `90e3f4bb600742acc737d9e52018a37007dd10bb`，合并到 main 基线 `6062e382f0099f396f0056438beb5e467a058f68`。extraction.rs 在该基线与 merge 的 blob 均为 `88c131d8a0e6420183c5c647fc860d6558aae0bb`；SP-01 未修改该文件。上游 `760a5529f9840736bf9db9b0a148201481f1ad36` 新增 AgentKind::Hermes 分支；#656 跟踪恢复 provider 中立边界及原守卫要求。
- 本次后续提交仅同步两份计划文档和 PR 已知失败说明，生产代码不变。其新 head 的 CI 与合并后 main CI 各自单独观察；不将先前 head 的成功或失败冒称为新运行。本次合并例外不授权绕过其它新发现的代码缺陷，也不关闭 #656。

### SP-01 修复卡收口决策（EX-SP-01-CLOSE-01）

2026-10-30，用户在 PR #655 的全部检查通过后要求“设置卡片完成，推送更新 PR”。本次以修复实现、本仓库文档及 PR CI 验收作为 SP-01 的完成口径，登记 `done / complete`；这是对模板 ER-04/ER-08 完整发布切片要求的本卡具名例外，不代表版本发布或网站部署已完成，不扩展至其它实现卡。网站交付按用户此前决定转入 DEFER-SP-DOCS-01，独立 patch 发布转入 DEFER-SP-REL-01，均保留为未执行的后续交付项，不作为本次修复卡关闭条件。

验收证据为 PR head `bb88db39feb584792821cfd2589a50ff7651123d`：base run [40910569542](https://github.com/libra-tools/libra/actions/runs/40910569542) 与 CodeQL run [40910569525](https://github.com/libra-tools/libra/actions/runs/40910569525) 均 success；全量 nextest 10841/10841 passed（577.819s、13 slow、23 skipped），其它适用 PR 检查均 SUCCESS。本次后续状态更新只改文档；新 head 的 CI 仍须单独报告，不把旧 head 的运行说成新运行。原本机 16 GiB 的 not-run 记录继续保留，CI 成功不改写本机历史。

### SP-00 独立文档验收决策（EX-SP-00-AUDIT-01）

2026-10-29，用户接受“取消 SP-00 对 SP-01 发布验收的继承，保留 SP-01 对基线的执行依赖”的建议，并明确授权修改、提交和推送。本决策仅针对 SP-00，作为模板 ER-04 对 no-release 卡必须继承发布点 C/D 的具名例外；不修改模板，不扩展至 SP-09 或任何实现卡。

- SP-00 的产物仅为本计划中的基线协议、测量记录和审计修正，以及状态索引；独立验证这些产物不需要 SP-01 的生产修复或二进制发布。继续保留 no-code / no-config、no-release、版本递增 N/A，不 bump、不安装、不创建 tag/Release。
- 本卡自行承担文档验收：四项 AC、两项 Verification、文档结构/内部链接与变更范围检查、Codex review PASS、最终文档状态下的 `cargo +nightly fmt --all --check` 和 `cargo clippy --all-targets --all-features -- -D warnings`；不以本决策免除任何现有文档判据或推送前静态门。
- 本卡 D 门为 PR #652 最终 head 上 `.github/workflows/base.yml`（Check, Build and Test）及 `.github/workflows/codeql.yml`（CodeQL Advanced）的全部适用检查成功；`neutral` 的汇总检查不替代底层 CodeQL 成功。随后合并 PR，核验文档进入上游 main，才可登记 `done / complete` 并关闭 #639。检查未完成时保留 `in-progress / locally-accepted`；失败则修复后重验，不以 mergeable 代替 CI 通过。
- 本卡不执行 C 组的版本 bump、release build、安装及发布步骤，不继承 SP-01 的全量测试或发布后置门；基准采样不因归属调整而重跑，历史实测与本轮文档核对继续区分。
- SP-01 的 `Dependencies: SP-00, DEP-SP-DOCS`、实现范围、回归测试、独立 patch 发布及完整 C/D 门保持原样。它消费的是本卡已冻结并通过文档验收的基线；不得再以 SP-01 的未来发布反向阻止本卡收口。
- 恢复：如撤销本决策，恢复 SP-00 的 `C/D coverage from: SP-01` 及原状态条件；不能回写历史 CI 或发布结果。

## 实施顺序与发布窗口

核心串行顺序：`SP-00 → SP-01 → SP-02 → SP-03 → SP-05 → SP-06 → SP-07 → SP-08 → SP-09`。

可选：`SP-09 + DEP-SP-OPT → SP-04`。这些顺序边既含产物依赖，也为共享源码/测试写集串行化；不是声称所有相邻卡存在算法上的前置关系。公开行为卡另依赖 DEP-SP-DOCS；SP-00 消费 DEP-SP-DATA 的可用性决策。无多 Agent 并发，发布者待实施授权后指定，任一时刻仅一个发布窗口。

Phase 0 基线：SP-00；Phase 1 正确性与 stat：SP-01..03；Phase 2 核心重复工作：SP-05..08；Phase 3 验收决策：SP-09；后续可选 SP-04。实现卡各自独立发布，禁止将 SP-09 当成替其它卡补做版本/验收的发布点。

## 任务总览

| 卡 | 优先级 | 选择 | Issue | Lifecycle / Acceptance |
|---|---|---|---|---|
| SP-00 冻结 status 性能基线与修正审计口径 | P1 | 必选验收前置 | [#639](https://github.com/libra-tools/libra/issues/639) | done / complete |
| SP-01 status 递归子树加载返回错误而非 panic | P1 | 必选正确性 | [#640](https://github.com/libra-tools/libra/issues/640) | done / complete |
| SP-02 restore 区分 decoded size 与对象加载内存成本 | P1 | 必选正确性 | [#641](https://github.com/libra-tools/libra/issues/641) | done / complete |
| SP-03 materialization 后安全写入真实 index stat | P1 | 本轮核心提速 | [#642](https://github.com/libra-tools/libra/issues/642) | done / complete |
| SP-04 单次 status 复用同一 index 快照 | P2 | 可选建议项，默认未启用 | [#643](https://github.com/libra-tools/libra/issues/643) | pending / 空 |
| SP-05 status 共享一次 HEAD tree 展开结果 | P2 | 本轮核心提速：第一梯队 | [#644](https://github.com/libra-tools/libra/issues/644) | pending / 空 |
| SP-06 对象读取复用请求级 storage scope | P2 | 本轮核心提速：第二梯队 | [#645](https://github.com/libra-tools/libra/issues/645) | pending / 空 |
| SP-07 pack index 整块 fanout 与二分查找 | P2 | 本轮核心提速：第一梯队 | [#646](https://github.com/libra-tools/libra/issues/646) | pending / 空 |
| SP-08 walk 级 ignore 上下文与 tracked 短路 | P2 | 本轮核心提速：第二梯队 | [#648](https://github.com/libra-tools/libra/issues/648) | pending / 空 |
| SP-09 核心优化复测、全量收口与可选项决策 | P2 | 必选验收收口 | [#647](https://github.com/libra-tools/libra/issues/647) | pending / 空 |

## 任务卡

### Task SP-00: 冻结 status 性能基线与修正审计口径

**Task type:** audit

**Lifecycle / Acceptance:** done / complete

**Priority / Selection:** P1；必选验收前置

**Issue:** [#639](https://github.com/libra-tools/libra/issues/639)

**Description:** 建立可重放的性能与语义对照协议；纠正旧审计的过强结论，不把文档中的历史统计当作本轮实测。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** LIBRA_STATUS_PERFORMANCE_REVIEW.md:72；LIBRA_STATUS_PERFORMANCE_REVIEW_AUDIT.md:168,221,228；benchmark/run.sh；benchmark/README.md（基线 380c262，开工刷新锚点）。

**Acceptance criteria:**

- [x] 在本计划记录实际基线源码修订、Git/Libra 二进制标识、操作系统、测试数据形状与样本协议。

- [x] 记录默认/刷新索引 × 显示/不显示未跟踪文件 × 默认重命名/关闭重命名 × 机器可读 v1/v2 的 Git/Libra 对照命令矩阵。

- [x] 逐项修正审计中的 242/206 次发现混用、线程切换下界与零风险预热结论。

- [x] 记录每个已采样格子的输出、退出状态与索引不变量；缺失格子明确标未测。


**Verification:**

- [x] 人工核对：本计划基线表的每格均有准确命令、源码修订、样本数及 measured/not-run 状态。

- [x] 人工核对：两份报告的历史统计与本轮实测分栏，未访问的外部归档不标独立复算。


**Full-suite trigger:** N/A：纯文档 audit；按 EX-SP-00-AUDIT-01 独立验收，不执行版本/二进制发布 C 组

**Dependencies:** DEP-SP-DATA

**Deliverables:** 本计划正文与 plan-status.md；不产生新证据文件

**Implementation write set:** `docs/development/plan/plan-20261028-status-performance.md`, `docs/development/plan/plan-status.md`

**Release write set:** N/A

**Docs and compatibility impact:** N/A：仅规划/审计记录，不改变产品行为。

**Rollback mode:** revert

**Migration and rollback:** 无 schema 迁移；撤销本卡文档变更即可；远端 Issue 保留讨论历史，不自动删除。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 无未经实测的绝对毫秒承诺；按 SP-00 的同环境前后配对协议报告中位数、尾延迟及工作量；语义退化即失败，噪声内变化记不确定。

**Estimated scope:** S

**Version increment:** N/A

**Release boundary:** no-release；本卡文档随 PR #652 独立合入上游 main，不等待 SP-01 发版

**C/D coverage from:** self（EX-SP-00-AUDIT-01）；自行通过文档、评审、fmt/Clippy 与 PR #652 最终 head 的 base/CodeQL 门，不继承 SP-01 发布。PR #652 最终 head `5e395e0` 的全部检查成功；2026-10-29T15:47:49Z 合并为 `d0578dbe6dd3d484521db4cf60daff736256dc59`，文档已进入上游 main。Issue #639 仍 open，状态关闭未同步。

**Granularity:** type=audit; axis=冻结 status 性能基线与修正审计口径; recovery=回退文档; complete=yes; self-contained=yes; AC=4/20; VER=2/20; landing=0; prod-files=0; scope=S; deps=DEP-SP-DATA; writeset=按实施顺序串行; release=no-release; split-from=N/A; exception=EX-SP-00-AUDIT-01

### Task SP-01: status 递归子树加载返回错误而非 panic

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Priority / Selection:** P1；必选正确性

**Issue:** [#640](https://github.com/libra-tools/libra/issues/640)

**Description:** 为 status 的 HEAD 递归展开提供可失败的读取路径，缺失或损坏子树返回上下文错误，禁止退化为空树或 clean。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** src/utils/object_ext.rs:54,111；src/command/status.rs:1030-1039；src/command/status/scan.rs:1327；src/command/status/output.rs:1361（基线 380c262，开工刷新锚点）。

**Acceptance criteria:**

- [x] 缺失子树返回 LBR-REPO-002 而非 panic。

- [x] 损坏子树返回包含受影响 OID 的可操作错误。

- [x] human/JSON/porcelain 错误输出遵守现有格式与退出契约。

- [x] 合法含 gitlink 的 tree 不被当成缺失子树。

- [x] ER-06a：本仓库 EN/zh-CN status、COMPATIBILITY 与错误码文档同步并验收；网站按用户决定登记 DEFER-SP-DOCS-01，仍未执行，依 EX-SP-01-CLOSE-01 不计本次修复卡完成门。


**Verification:**

- [x] `cargo test --test command_test status_error_test::`

- [x] `cargo test --test command_test status_error_test::status_nested_tree_ -- --nocapture` — `(new)` 测试前缀；须非零命中。


**Full-suite trigger:** T-1：共享 tree 读取辅助逻辑

**Dependencies:** SP-00（已验收）；DEP-SP-DOCS 的网站交付按 EX-SP-01-CLOSE-01 转 DEFER-SP-DOCS-01，仍未执行、不阻塞本次修复卡收口

**Deliverables:** 实现、同卡回归及列明文档

**Implementation write set:** `src/utils/object_ext.rs`, `src/command/status.rs`, `src/command/status/scan.rs`, `src/command/status/output.rs`, `tests/command/status_error_test.rs`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`

**Release write set:** Inherited

**Docs and compatibility impact:** `docs/commands/status.md`；`docs/commands/zh-CN/status.md`；`COMPATIBILITY.md`；`docs/error-codes.md`；`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`。网站路径仍待核实，后续交付由 DEFER-SP-DOCS-01 跟踪；本仓库四份文档已验收。

**Rollback mode:** immutable-release

**Migration and rollback:** 无 schema 迁移；发布前回退本卡，发布后前滚修复；不把版本回退视为历史 index 修复。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 无未经实测的绝对毫秒承诺；按 SP-00 的同环境前后配对协议报告中位数、尾延迟及工作量；语义退化即失败，噪声内变化记不确定。

**Estimated scope:** M

**Version increment:** patch（DEFER-SP-REL-01：后续独立发布时执行；本次修复卡收口不 bump）

**Release boundary:** independent（发布交付在 DEFER-SP-REL-01 单独跟踪；本次修复卡按 EX-SP-01-CLOSE-01 收口）

**C/D coverage from:** self（EX-SP-01-CLOSE-01 的修复/PR CI 完成口径）；本地 18 个不同用例、fmt/严格 Clippy 与本仓库文档检查通过；bb88db3 的 PR base/CodeQL 全绿，全量 nextest 10841 passed、23 skipped。后续 d9b5eee 的合并基线全量因已知上游架构守卫失败，按 EX-SP-01-UPSTREAM-CI-01 / #656 单独披露并获用户合并授权；不记作全绿。PR #655 已于 2026-10-30T01:23:41Z 合并为 `e4eb85061af79b4deff37e569265f1c6b82787d4`，Issue #640 已于 2026-10-30T01:23:43Z 关闭。发布构建/安装/tag/Release/CDN 等仍未执行，登记 DEFER-SP-REL-01，不称完整发布 C/D 通过。

**Granularity:** type=implementation; axis=status 递归子树加载返回错误而非 panic; recovery=前滚修复版; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=3; prod-files=4; scope=M; deps=SP-00,DEP-SP-DOCS; writeset=按实施顺序串行; release=independent; split-from=N/A; exception=EX-SP-01-CLOSE-01

### Task SP-02: restore 区分 decoded size 与对象加载内存成本

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Priority / Selection:** P1；必选正确性

**Issue:** [#641](https://github.com/libra-tools/libra/issues/641)

**Description:** 补充语义明确的 decoded-size 查询用于 restore index size；保留原 object_size(s) 的保守加载预算语义，避免预览安全限额被削弱。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** src/command/restore.rs:737-770,1839；src/utils/storage/mod.rs:241,254；src/utils/storage/local.rs:640,1277；src/utils/storage/tiered.rs:202；src/utils/client_storage.rs:647；src/utils/storage/load_cost/loose.rs:109；src/utils/storage/load_cost/pack.rs:297,388（原始基线 380c262；2026-10-30 刷新至 main `e4eb85061af79b4deff37e569265f1c6b82787d4`）。

**Acceptance criteria:**

- [x] 普通与 OFS/REF delta 对象产生真实 decoded size。

- [x] 多层 delta 查询不把 peak_bytes 写入 index size。

- [x] 查询失败或缓存未命中的 fallback 保留原错误语义。

- [x] 原有 preview load-cost 限额仍能拒绝超预算重建。

- [x] staged-only 不读取工作区 stat 来伪造 clean。

- [x] ER-06a：本仓库 `docs/commands/restore.md` 与 `docs/commands/zh-CN/restore.md` 已同步并验收；网站 `/Volumes/Sea/libra-backend` 未挂载，登记 DEFER-SP-DOCS-02，仍未执行，依 EX-SP-02-CLOSE-01 不计本次修复卡收口门。


**Verification:**

- [x] `cargo test --lib utils::storage::`

- [x] `cargo test --test command_test restore_test::`

- [x] `cargo test --test command_test restore_test::restore_delta_index_size_ -- --nocapture` — `(new)` 测试前缀；须非零命中。


**Full-suite trigger:** T-1：共享存储接口及预算逻辑

**Dependencies:** SP-01（已验收合并 `e4eb850`）；DEP-SP-DOCS 的网站交付按 EX-SP-02-CLOSE-01 转 DEFER-SP-DOCS-02，仍未执行、不阻塞本次修复卡收口

**Deliverables:** 实现、同卡回归及列明文档

**Implementation write set:** `src/command/restore.rs`, `src/utils/client_storage.rs`, `src/utils/storage/mod.rs`, `src/utils/storage/local.rs`, `src/utils/storage/tiered.rs`, `src/utils/storage/load_cost/mod.rs`, `src/utils/storage/load_cost/loose.rs`, `src/utils/storage/load_cost/pack.rs`, `tests/command/restore_test.rs`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`

**Release write set:** Inherited

**Docs and compatibility impact:** `docs/commands/restore.md`；`docs/commands/zh-CN/restore.md`；`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`。本仓库 EN/zh-CN 两份文档已同步并验收；网站路径待核实，后续交付由 DEFER-SP-DOCS-02 跟踪。

**Rollback mode:** immutable-release

**Migration and rollback:** 无 schema 迁移；发布前回退本卡，发布后前滚修复；不把版本回退视为历史 index 修复。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 无未经实测的绝对毫秒承诺；按 SP-00 的同环境前后配对协议报告中位数、尾延迟及工作量；语义退化即失败，噪声内变化记不确定。

**Estimated scope:** M

**Version increment:** patch（DEFER-SP-REL-02：后续独立发布时执行；本次按用户“版本不变”指令保持 `0.30.101` 不 bump）

**Release boundary:** independent（发布交付在 DEFER-SP-REL-02 单独跟踪；本次修复卡按 EX-SP-02-CLOSE-01 收口）

**C/D coverage from:** self（EX-SP-02-CLOSE-01 本仓修复收口口径）；本仓库单元/集成/守卫（`utils::storage::` 84/84、`storage_config_hold` 11/11、`command::restore::tests::` 9/9、`restore_test::` 50/50、新增 `restore_delta_index_size_` 4/4、`compat_serial_registry` 26/26、两项生产 unwrap 守卫各 1/1）与 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 均通过（上游依赖 `block v0.1.6` future-incompat 提示保留）；本机 16 GiB 内存硬门槛下 T-1 全量 nextest 为 not-run。PR CI（base.yml/CodeQL）随本次推送建 PR 触发，不在提交前预称远端全绿；网站与独立版本发布分别登记 DEFER-SP-DOCS-02 / DEFER-SP-REL-02，均未执行，不称完整发布 C/D 通过。

**Granularity:** type=implementation; axis=restore 区分 decoded size 与对象加载内存成本; recovery=前滚修复版; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=4; prod-files=8; scope=M; deps=SP-01,DEP-SP-DOCS; writeset=按实施顺序串行; release=independent; split-from=N/A; exception=EX-SP-02-CLOSE-01

### Task SP-03: materialization 后安全写入真实 index stat

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete（最终 PR head CI 全绿并按 EX-SP-03-CLOSE-01 收口；网站与独立发布仍按延期/未执行跟踪）

**Priority / Selection:** P1；本轮核心提速

**Issue:** [#642](https://github.com/libra-tools/libra/issues/642)

**Description:** 仅在实际写入 worktree 并更新 index 的受控路径建立可信 stat 缓存，减少 fresh clone 的无效 hash；不把 staged-only index 与工作区内容混为一谈。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** PR #661 最终 head `dbd21ed8eaf6d0eaf42239ad1f928e60e663d32a` 的 base run `41227810790` 与 CodeQL run `41227810789` 全部适用检查 SUCCESS；PR 已于 2026-10-31T19:37:49Z 合并为 `b1b9c32259d5e2d972521e1cc30129e5319488d2`。源码 manifest 与两轮完整矩阵见下。

**Acceptance criteria:**

- [x] worktree+staged 落盘后普通文件 size/stat 与可靠文件快照一致。

- [x] staged-only 后的未暂存修改仍能被 status 检出。

- [x] hash/写入期间的并发修改不能形成错误 clean 缓存；发布后/匹配 hash 后两个确定性窗口及真实 status 均通过。

- [x] racy-clean 文件仍触发必要的内容校验。

- [x] LFS smudge 后 index size 使用实际工作区长度。

- [x] symlink 使用 no-follow 元数据语义。

- [x] 既有 mode 与 flags 的合法语义保持不变；不处理 CE_VALID 策略。

- [x] ER-06a：本仓库四份 clone/restore 文档已随 PR #661 同步并验收；网站交付按 DEFER-SP-DOCS-03 保留且不计完成，本卡按 EX-SP-03-CLOSE-01 收口（全局必需门，不计入特有 AC）。


**合并集成后当前验收摘要（2026-10-31 02:23:54 UTC；本节取代下方原候选历史摘要）：**

- PR #661首个head bb8e2c3与主线的唯一文本冲突为SRC_SERIAL_REGISTRY追加行；已完整保留双方登记，合入上游98aa6be6da28646effbc2879959805c0b326547c，继承其Storage/Tree Result传播，不回退上游错误处理。版本随主线0.30.118，SP-03没有额外bump。当前修复diff仍为同一stat信任/失效轴与原13个文件，非引入其它业务改动。
- 合并后实测：stat_diff2/2、restore lib12/12、restore58/58、clone43/43、worktree65/65、isolation119/119、doctor13/13、checkpoint3/3、bridge VCS8/8、bridge checkpoint3/3、registry26/26；三个racy过滤各1/1，均0failed/0ignored。fmt、严格全特性Clippy、cargo check及候选/主线同profile nightly release均exit0。nightly Cargo的manual_readme/redundant_homepage两条主线清单提示保留；Clippy没有因而放宽-D warnings。
- 新候选binary SHA256 `557625f321697abd9f98a085aedb25a1670b8afc8ed44c1c9359e19e4d408808`，主线父binary `c6561f91e1e4929ea8c72e3dcd8269720c07c8978896120610114aa2c07c160a`；Git `6210287b8021b25eea03b1866f8bbb775c02ac1185f0d69f2a70c01e73b1ab9c`。直接对照基线为98aa6be，而非旧bbfb4ef。生产源码manifest `8185f969254e2550eab24b1ec01015dbe8d9313f129e3803f256278c8fe2de58`，采样前后源码及三工具binary不变，独立核验时仍匹配当前源码。
- 严格SP-00完整16格、fixture d0a5c28、2125路径，每边每格5预热+20正式；共1200次调用、960正式样本，ABC/CBA逐对交替、新CLI/热页面缓存。所有exit0、stderr空、各工具原始输出稳定、index SHA不变；跨工具/索引规范化记录一致，独立重算median及nearest-rank p95通过。
- 默认索引8格倍率下降30.03%–51.86%；refreshed8格倍率下降范围-4.15%至1.56%，不将小幅波动包装成确定收益。clone成本仍不列验收阻断。完整结果如下。
- 原始临时证据 `/tmp/libra-sp03-full-matrix-fvrq4x63`；result.json SHA256 `7437da264ecc103b35fad52fc0afc3f0c826d75ebaacac5625a38b04f2de4c91`，samples.jsonl `a46838f6c1f702ebb0626054b3e053b5bf22b4dacff2c33757d8d894327b82e6`，sampler `9d9795644de7cb438c762e61c929968eb0a76fcc40b282e0a578378353e6c6ef`。主线ZIP `c48df74e925ce3804e6f938594eb3dc3c8ebb7236e580607e7c400ab5e9c9d03`，校验/构建日志 `/tmp/libra-sp03-merged-final-7dovzozl`。关键计数、hash及完整表已入本计划，临时文件不承诺永久保存。
- 本地 nextest 历史仍为 not-run（16 GiB 及两份 env 缺失）；最终 PR head `dbd21ed8` 的 base run `41227810790`（含 compat-offline-core 全量门）与 CodeQL run `41227810789` 全部适用检查 SUCCESS，满足 EX-SP-03-CLOSE-01。PR #661 已于 2026-10-31T19:37:49Z 合并为 `b1b9c32259d5e2d972521e1cc30129e5319488d2`；现按用户收口指令登记 done/complete。网站与独立发布仍未执行，不计完成。

| 索引 | 未跟踪 | 重命名 | 输出 | Git median/p95 ms | 父版本 median/p95 ms | 候选 median/p95 ms | 父/Git | 候选/Git | 倍率下降 | 候选更快配对数 |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| default | no | default | v1 | 11.297/12.512 | 650.894/664.990 | 369.545/381.692 | 57.615× | 32.711× | 43.23% | 20/20 |
| default | no | default | v2 | 11.362/12.329 | 748.948/762.897 | 463.892/475.187 | 65.918× | 40.829× | 38.06% | 20/20 |
| default | no | no-renames | v1 | 10.926/12.383 | 551.115/567.697 | 265.319/277.440 | 50.442× | 24.284× | 51.86% | 20/20 |
| default | no | no-renames | v2 | 11.059/12.016 | 649.618/662.361 | 368.440/376.707 | 58.744× | 33.317× | 43.28% | 20/20 |
| default | normal | default | v1 | 17.152/17.857 | 876.176/892.002 | 585.053/599.109 | 51.084× | 34.111× | 33.23% | 20/20 |
| default | normal | default | v2 | 16.186/18.425 | 976.619/986.463 | 683.292/698.963 | 60.336× | 42.214× | 30.03% | 20/20 |
| default | normal | no-renames | v1 | 16.073/17.347 | 777.079/784.921 | 487.279/504.275 | 48.348× | 30.317× | 37.29% | 20/20 |
| default | normal | no-renames | v2 | 16.567/17.821 | 876.482/898.635 | 585.854/596.900 | 52.906× | 35.363× | 33.16% | 20/20 |
| refreshed | no | default | v1 | 11.231/12.690 | 386.322/499.812 | 383.827/400.224 | 34.397× | 34.175× | 0.65% | 14/20 |
| refreshed | no | default | v2 | 12.345/13.800 | 544.560/588.749 | 567.134/588.605 | 44.110× | 45.939× | -4.15% | 6/20 |
| refreshed | no | no-renames | v1 | 12.729/14.525 | 350.730/373.490 | 346.665/365.395 | 27.553× | 27.233× | 1.16% | 11/20 |
| refreshed | no | no-renames | v2 | 13.334/14.221 | 492.118/527.165 | 484.445/493.261 | 36.907× | 36.332× | 1.56% | 18/20 |
| refreshed | normal | default | v1 | 20.319/30.404 | 783.379/848.559 | 784.063/1108.152 | 38.554× | 38.588× | -0.09% | 8/20 |
| refreshed | normal | default | v2 | 22.071/29.787 | 947.019/1331.211 | 941.310/1374.843 | 42.908× | 42.650× | 0.60% | 11/20 |
| refreshed | normal | no-renames | v1 | 21.175/27.448 | 688.210/734.506 | 690.657/775.251 | 32.500× | 32.616× | -0.36% | 9/20 |
| refreshed | normal | no-renames | v2 | 23.152/51.988 | 926.091/1047.449 | 937.693/1175.663 | 40.001× | 40.502× | -1.25% | 7/20 |

**完整16格复测（同一源码/二进制/原夹具，不重置index）：** 首轮后半段三工具的绝对耗时与p95同时升高，refreshed/no/default/v2中位数观察到慢4.15%、6/20配对更快，因此追加完整16格复测，而非只挑该格或只挑有利结果。原夹具的六份index SHA与首轮一致，HEAD仍为d0a5c28；每格重新5预热+20正式，完整1200调用/960正式样本。复测的源码、三二进制、全部输出记录与首轮一致，index仍未改变，两轮都独立重算median/p95通过。

复测默认8格下降29.87%–51.74%，每格20/20更快；refreshed8格下降范围-0.48%至0.80%。首轮慢4.15%的同格复测为快0.08%、10/20配对更快，未稳定复现；不能据此证明任何环境下绝无回归。当前证据支持default收益稳定，refreshed没有可复现的一致收益/回归，保留不确定性。两轮分别报告，未筛除首轮，不把它们混为一轮n=20。

复测临时证据 `/tmp/libra-sp03-matrix-replication-atk55x1v`：result.json SHA256 `760ef3f2b911ba30f664ac7468fb3b1e8d47d6f5a0aeb602e618f4509f026575`、samples.jsonl `20597e8573948259058c204fb8986a1b4d64dbee60be5d9e5c7839a3aa25171d`、replicator `049d094a226c810851817fcebd8583d971965bdd1099c776e181a84463e380ae`。合计2400调用/1920正式样本，每轮完整16格。

| 索引 | 未跟踪 | 重命名 | 输出 | Git median/p95 ms | 父版本 median/p95 ms | 候选 median/p95 ms | 父/Git | 候选/Git | 倍率下降 | 候选更快配对数 |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| default | no | default | v1 | 11.371/11.710 | 650.234/663.544 | 365.574/372.950 | 57.185× | 32.151× | 43.78% | 20/20 |
| default | no | default | v2 | 10.881/11.515 | 743.553/758.481 | 460.026/474.177 | 68.337× | 42.279× | 38.13% | 20/20 |
| default | no | no-renames | v1 | 10.683/11.497 | 549.422/567.875 | 265.141/275.076 | 51.429× | 24.819× | 51.74% | 20/20 |
| default | no | no-renames | v2 | 10.746/11.728 | 645.425/649.342 | 364.882/371.789 | 60.061× | 33.955× | 43.47% | 20/20 |
| default | normal | default | v1 | 16.411/17.536 | 869.398/891.529 | 580.111/594.469 | 52.975× | 35.348× | 33.27% | 20/20 |
| default | normal | default | v2 | 16.177/16.952 | 963.610/974.307 | 675.824/685.283 | 59.565× | 41.776× | 29.87% | 20/20 |
| default | normal | no-renames | v1 | 16.329/18.559 | 771.424/790.861 | 482.178/495.529 | 47.242× | 29.529× | 37.50% | 20/20 |
| default | normal | no-renames | v2 | 16.268/18.578 | 865.629/882.641 | 577.733/590.409 | 53.211× | 35.514× | 33.26% | 20/20 |
| refreshed | no | default | v1 | 10.982/12.146 | 365.711/380.775 | 366.988/395.135 | 33.302× | 33.419× | -0.35% | 8/20 |
| refreshed | no | default | v2 | 10.900/11.608 | 461.520/469.972 | 461.160/477.557 | 42.341× | 42.308× | 0.08% | 10/20 |
| refreshed | no | no-renames | v1 | 10.831/12.117 | 266.753/277.944 | 266.220/276.247 | 24.629× | 24.580× | 0.20% | 11/20 |
| refreshed | no | no-renames | v2 | 11.450/12.969 | 364.197/376.219 | 364.444/375.310 | 31.808× | 31.829× | -0.07% | 9/20 |
| refreshed | normal | default | v1 | 15.961/17.897 | 576.496/589.612 | 578.669/596.519 | 36.119× | 36.255× | -0.38% | 8/20 |
| refreshed | normal | default | v2 | 15.805/16.131 | 672.646/685.118 | 672.934/688.708 | 42.560× | 42.578× | -0.04% | 12/20 |
| refreshed | normal | no-renames | v1 | 15.894/18.271 | 479.152/492.352 | 481.458/501.678 | 30.147× | 30.292× | -0.48% | 8/20 |
| refreshed | normal | no-renames | v2 | 16.016/18.002 | 578.983/603.962 | 574.359/592.854 | 36.149× | 35.861× | 0.80% | 12/20 |

**Verification:**

执行回执共19行已取得、1行全量未运行；其中racy一行含3条独立命令。G-03按下方逐项分类保守计为15个本卡指定验证门，超过implementation上限8；用户现按EX-SP-03-VER-01批准本卡数量例外。19/20不再用于粒度字段，也不通过合并命令隐藏独立门。

- [x] `cargo test --lib utils::stat_diff::tests::` — 2 passed；任一零时间戳均禁止stat信任。

- [x] `cargo test --lib command::restore::tests::` — 12 passed。

- [x] `cargo test --test command_test restore_test::` — 58 passed。

- [x] `cargo test --test command_test clone_test::` — 43 passed。

- [x] `restore_stat_snapshot_`八项 — 包含于合并后restore58项中，未额外重复运行该过滤命令。

- [x] `cargo test --test command_test worktree_test::` — 65 passed。

- [x] `cargo test --test command_test worktree_isolation_test::` — 119 passed。

- [x] `cargo test --test command_test worktree_doctor_test::` — 13 passed。

- [x] `cargo test --test command_test agent_checkpoint_test::` — 3 passed。

- [x] `cargo test --test agent_bridge_vcs_test` — 8 passed。

- [x] `cargo test --test agent_bridge_checkpoint_test` — 3 passed。

- [x] `racy_edit_between_read_and_stat_smudges_the_entry / stat_short_circuit_rehashes_racy_files / racily_clean_entry_is_content_compared_not_trusted` — 两项 lib 与一项 command_test 各 1 passed。

- [x] `cargo test --test compat_serial_registry` — 26 passed。

- [x] `cargo check --locked --bin libra` — exit 0。

- [x] `cargo +nightly fmt --all --check` — exit 0。

- [x] `cargo clippy --all-targets --all-features -- -D warnings` — exit 0；既有 block v0.1.6 future-incompat 提示保留。

- [x] `cargo +nightly build --release --locked --bin libra` — candidate 与直接父版本均 exit 0；同一工具链/profile。

- [x] `SP-00 完整三边矩阵` — 16 格 / 1200 调用 / 960 正式样本；独立核验通过。

- [x] `临时源码副本的仓库发现调用计数` — 历史 f42 实现：父15334→候选13211，三次一致；根路径复用实现未再变动，此计数不计作最终候选重新插桩或release耗时证据。

- [x] T-1 全量门：最终 PR head `dbd21ed8` 的 base run `41227810790` / `compat-offline-core` SUCCESS，满足 EX-SP-03-CLOSE-01；本机 nextest 历史仍 not-run（16 GiB <32 GiB 且两份 env 缺失），不以 focused 或旧 CI 冒充本机全量。

**Full-suite trigger:** T-1: restore 物化/index helper 被 clone、worktree 和 agent bridge 共用。

**Dependencies:** SP-02（已完成）；DEP-SP-DOCS 暂由用户批准延后，登记为 DEFER-SP-DOCS-03；网站文档同步、验证与部署未执行且不计完成。

**Deliverables:** 实现、同卡回归及列明文档

**Implementation write set:** `src/command/restore.rs`, `src/utils/stat_diff.rs`, `tests/command/restore_test.rs`, `tests/command/clone_test.rs`, `tests/command/worktree_test.rs`, `tests/agent_bridge_vcs_test.rs`, `tests/SRC_SERIAL_REGISTRY.tsv`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`；协调文件为本计划与 `plan-status.md`；网站原写集仍由 DEFER-SP-DOCS-03 跟踪。

**Release write set:** Inherited

**Docs and compatibility impact:** `docs/commands/clone.md`；`docs/commands/zh-CN/clone.md`；`docs/commands/restore.md`；`docs/commands/zh-CN/restore.md`；`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`；`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`。网站候选路径待 DEP-SP-DOCS 核实。

**Rollback mode:** immutable-release

**Migration and rollback:** 无 schema 迁移；发布前回退本卡，发布后前滚修复；不把版本回退视为历史 index 修复。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 性能验收严格针对 SP-00 完整 16 场景；报告中位数、p95、Git 倍率与工作量；语义退化即失败，噪声内变化记不确定。clone 增加耗时为预期缓存建立成本，不作为性能验收阻断项。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；本仓库定向、静态、release 构建、SP-00 三边矩阵及最终 PR head 全量 CI/CodeQL 均已验证，PR #661 已合并；网站门 DEFER-SP-DOCS-03 与独立发布仍未执行，不影响本次按 EX-SP-03-CLOSE-01 收口。

**原候选历史验收摘要（ee598 / bb8e2c3，合并前；当前以本卡合并集成后摘要为准）：**

- 最终源码生产写集为 restore.rs 与 stat_diff.rs。发布后内容复核、最终stat复核、干净路径不误报 restored_files、无skip-worktree的clean→dirty、可信路径跳过fallback blob、旧API传递快照及每pass复用workdir均已实现并定向验证。
- 非Unix缺少可靠ctime证据时保守smudge；任一零时间戳不能授权stat短路；LFS epoch路径保留实际32768字节而非pointer的130字节。后两项均有实际RED→GREEN。没有原生Windows执行环境，不能据此宣称Windows原生测试通过。未改变CE_VALID或生产签名默认。
- 最新定向/静态流水14741通过：stat_diff2、restore lib12、restore58、clone43、racy各1、fmt、严格全特性Clippy。最终源码另重验worktree65、isolation119、doctor13、agent checkpoint3、bridge VCS8/checkpoint3、registry26与cargo check，均通过。stat前缀8项包含在restore58项中。全量nextest未运行，网站仍延期，整卡保持in-progress；未提交/推送/发布。
- 最终release candidate `ee59861c2e557ebf0f478ad27ca2df3a808f442299d2ec1661783ee127095cd5`；直接父提交 `bbfb4ef5460da91bddb415c55a9a832bb9701104`（含SP-02），父binary `8675f71605d05a86c65cc1607aa1ae3e91a84453c9ba18d8bfc41bc95718660a`；Git 2.56.0 binary `6210287b8021b25eea03b1866f8bbb775c02ac1185f0d69f2a70c01e73b1ab9c`；相同nightly release工具链/profile。三者采样前后SHA及生产源码manifest均不变。
- 严格沿用SP-00公开fixture `d0a5c28a6b236421c14a38339b592fe37137585e`、2125 tracked paths及完整16格；每工具每格5预热+20正式，新CLI进程/热页面缓存；Git/候选/父及反序交替。1200调用/960正式样本，退出0、stderr空、输出稳定及index SHA不变；独立重算median/nearest-rank p95，并检查输出hash、跨工具/索引规范化语义与当前源码manifest，全部通过。夹具准备及clone不计入status计时，两个既有PowerShell EOL例外保留。
- 默认索引8格：耗时与对Git倍率均下降29.50%–51.68%，每格20/20配对更快；refreshed8格倍率下降在-0.66%至+0.64%，视为约持平，不宣称明确收益。不能将SP-00历史版本到当前全部差异归因于SP-03。clone增加为预期成本，不设clone+首次status阻断门。
- 最终证据 `/tmp/libra-sp03-full-matrix-010cyo8g`：samples.jsonl SHA256 `8e1ec2bfe67c76d2c208bf0eee98cc661aa4c44caa6bfd5213339a93193cebb0`；result.json SHA256 `6961a4fda26ff92a2be0b58703238c14af05397b9831e1264e8584203bfe6dda`；sampler SHA256 `b953f6adf904c756cb3af425f850b5b84ed0a8596fa07b122a438fcc3fdbceca`。调用方复验日志位于 `/tmp/libra-sp03-zero-final-81ogrkt1/recheck-*.log`。临时产物不承诺永久保留，完整矩阵在下表登记。

| 索引 | 未跟踪 | 重命名 | 输出 | Git median/p95 ms | 父版本 median/p95 ms | 候选 median/p95 ms | 父/Git | 候选/Git | 倍率下降 | 候选更快配对数 |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| default | no | default | v1 | 12.334/14.032 | 650.670/658.958 | 372.329/379.741 | 52.753× | 30.187× | 42.78% | 20/20 |
| default | no | default | v2 | 12.365/14.056 | 747.231/758.495 | 462.623/471.189 | 60.432× | 37.415× | 38.09% | 20/20 |
| default | no | no-renames | v1 | 12.790/13.797 | 550.751/565.763 | 266.120/279.669 | 43.060× | 20.806× | 51.68% | 20/20 |
| default | no | no-renames | v2 | 12.396/13.657 | 645.454/659.471 | 371.954/382.693 | 52.072× | 30.007× | 42.37% | 20/20 |
| default | normal | default | v1 | 17.513/20.164 | 868.413/878.435 | 583.508/591.651 | 49.586× | 33.318× | 32.81% | 20/20 |
| default | normal | default | v2 | 17.640/20.484 | 966.963/987.128 | 681.704/694.866 | 54.818× | 38.646× | 29.50% | 20/20 |
| default | normal | no-renames | v1 | 17.960/20.304 | 774.241/792.003 | 485.093/505.061 | 43.109× | 27.009× | 37.35% | 20/20 |
| default | normal | no-renames | v2 | 18.520/19.333 | 865.871/881.726 | 584.262/596.316 | 46.754× | 31.548× | 32.52% | 20/20 |
| refreshed | no | default | v1 | 12.698/14.091 | 366.291/380.368 | 368.643/384.494 | 28.847× | 29.032× | -0.64% | 7/20 |
| refreshed | no | default | v2 | 12.320/13.925 | 462.421/469.415 | 460.389/470.576 | 37.534× | 37.369× | 0.44% | 12/20 |
| refreshed | no | no-renames | v1 | 13.722/14.569 | 265.758/268.931 | 266.490/275.628 | 19.368× | 19.421× | -0.28% | 10/20 |
| refreshed | no | no-renames | v2 | 12.789/14.809 | 364.335/374.091 | 363.703/372.997 | 28.488× | 28.438× | 0.17% | 10/20 |
| refreshed | normal | default | v1 | 17.166/19.053 | 582.166/679.988 | 578.430/589.085 | 33.915× | 33.697× | 0.64% | 13/20 |
| refreshed | normal | default | v2 | 17.324/19.184 | 669.526/684.007 | 673.960/731.263 | 38.646× | 38.902× | -0.66% | 7/20 |
| refreshed | normal | no-renames | v1 | 17.030/19.154 | 472.158/482.204 | 472.915/488.959 | 27.725× | 27.770× | -0.16% | 8/20 |
| refreshed | normal | no-renames | v2 | 16.980/18.109 | 568.269/577.446 | 567.667/581.885 | 33.467× | 33.432× | 0.11% | 9/20 |

**EX-SP-03-PERF-01 对比协议：** 用户于 2026-10-30 要求本卡验收前加入对比测试。以 SP-00 同一公开仓库提交 `d0a5c28a6b236421c14a38339b592fe37137585e`（2,125 tracked paths）构造固定的本地 Libra source fixture；比较基线 `main` 与本卡最终候选的相同 release toolchain / build profile。每边 5 次预热 + 20 次正式样本，按配对交替顺序，每次均克隆到全新目标目录；clone、首次 status、两者合计分别计时。夹具准备、build 与校验不计入命令耗时；确认每边所有 status 都为空输出且 exit 0，tracked path 集相同，并且每次 status 前后 index hash 不变；另外记录样本环境、fixture 文件数、退出码、hash 与 size/stat trust 覆盖数。报告中位数与 p95；只对状态命令的前后差异解释 SP-03 效果，同时披露 clone 成本及总耗时，不将成本转移包装为总收益。

**EX-SP-03-PERF-01 实测（2026-10-30）：** macOS 27.0.1（26A434），Apple M1 arm64，16 GiB RAM；`rustc 1.100.0-nightly (574ff7d98 2026-09-14)`，`--release --locked`。基线 `main` commit `bbfb4ef5460da91bddb415c55a9a832bb9701104`，binary SHA-256 `39d0fa9ea09a4ad6d32c8d9f4750e0dc00918fdcaa3eee77b38169c8972aec99`；候选 binary SHA-256 `1178520d08b85e41e8b7d5f83f61d413adc77b10790b891553697880b360fdf3`。fixture HEAD `80147b265ada48ff75825eef815935aa073e765a`，来自要求的 SP-00 fixture commit；2,125 entries，未包含 LFS pointer 文件。每边 warmup=5、正式 n=20，AB/BA 逐对交替：

| 指标 | main median / p95 (ms) | SP-03 median / p95 (ms) | 中位数变化 |
|---|---:|---:|---:|
| clone | 4,916.769 / 5,138.167 | 5,471.021 / 5,664.479 | +11.27% |
| 首次 status | 802.446 / 820.868 | 512.197 / 540.674 | -36.17% |
| clone + 首次 status | 5,722.835 / 5,927.623 | 5,984.622 / 6,180.898 | +4.57% |

逐对 median delta：clone +504.304 ms（候选 19/20 对较慢），status -286.649 ms（候选 20/20 对较快），合计 +228.177 ms（候选 18/20 对较慢）。所有 status 均 exit 0、空 stdout（SHA-256 `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`）；两边各 20/20 次 index hash 稳定，path 集均为 2,125；main 初始有效 stat 0/2,125，候选 2,125/2,125。结论：首次 status 明显变快，但 clone 成本增长后，本夹具的端到端中位耗时慢 4.57%；不宣称整体性能收益。该对比验收项指验证协议已完成，不代表总耗时无回归；网站文档门仍延后，整卡保持 in-progress。

**Granularity:** type=implementation; axis=materialization 后安全写入真实 index stat; recovery=前滚修复版; complete=yes; self-contained=yes; AC=7/8; VER=15/8@EX-SP-03-VER-01; landing=2; prod-files=2; scope=M; deps=SP-02,DEFER-SP-DOCS-03; writeset=按实施顺序串行; release=independent; split-from=N/A; exception=EX-SP-03-DOCS-01,EX-SP-03-PERF-01,EX-SP-03-VER-01,EX-SP-03-CLOSE-01

**三模型整改启动（2026-10-30 17:53:44 UTC）：** 用户授权完成当前 TODO；继续在 codex/SP-03，保留原有 9 文件修改。SP-02 #641 已核为 CLOSED；#642 OPEN，远端旧 pending 正文待同步。此前 AC/VER 数字为整改前历史结果，不代表当前候选通过。性能验收仅针对 SP-00 完整 16 场景，clone 耗时增加为预期成本，不列阻断项。新 clean-combined CLI 回归已实际 RED（1 failed，输出 Updated 1 path(s) from HEAD）；已分离内部 stat 刷新和用户可见恢复列表，GREEN 与普通无 skip-worktree 场景验证进行中。剩余：并发测试及安全修复、延迟 fallback size、旧 API 快照传递、workdir 复用、双语文档、相关 focused/fmt/clippy、全量门、父版本/Git 完整矩阵、Issue/状态汇总。网站继续 DEFER-SP-DOCS-03，不计完成。本轮不提交、推送或发布。

**整改定向验证（2026-10-30 17:54:01 UTC）：** clean-combined 回归 RED 1 failed → GREEN 1 passed；`cargo test --test command_test restore_stat_snapshot_ -- --nocapture` 实际 6 passed/0 failed/0 ignored（含新增 clean CLI/JSON 与无 skip-worktree clean→dirty）。已执行格式化，最终 fmt check 待本轮回执；严格 Clippy、其余 affected suites、并发修复及修复后矩阵均未完成。

**并发整改（2026-10-30 17:58:53 UTC）：** 新增 cfg(test) 同步点及 `command::restore::tests::restore_races_do_not_certify_modified_content_as_clean`，覆盖 publication 后等长原位修改并保留 mtime、已有匹配文件 hash 后修改两个窗口，并检查最终 index stat 及真实 status 输出。第一轮 fixture vault 签名失败不算目标 RED；fixture 显式禁签后实际 RED 1 failed：published 分支错误保留非零 mtime。生产修复在 publication 后重新读取内容 hash，返回读取前 metadata 供最终 verified_index_entry 检查。EN/zh-CN clone/restore 文档同步。GREEN、serial registry 守卫运行中，不计通过。新 serial 注解已登记 registry 并重生 nextest 配置；未启动 nextest。

**并发 GREEN 回执（2026-10-30 18:01:20 UTC）：** `RUST_BACKTRACE=0 cargo test --lib restore_races_do_not_certify_modified_content_as_clean -- --nocapture` 实际 1 passed/0 failed/0 ignored，单例内部两个同步窗口均执行，均检查 smudged index 及真实 status 输出。`cargo +nightly fmt --all --check` exit 0。serial registry target 已进入 26 项运行，终态未取得；不提前计通过。其余 TODO 继续待办，未复用修复前性能矩阵作最终验收。

**registry 修正（2026-10-30 18:02:27 UTC）：** 守卫首轮实际 25 passed/1 failed，唯一失败是新增 src 单测误填 tests/SERIAL_REGISTRY.tsv 的 dangling row。已移至 tests/SRC_SERIAL_REGISTRY.tsv（lane:cwd+env），按生成器重生配置；未改 classifier 或断言。重跑进程进行中，终态待验。

**registry GREEN（2026-10-30 18:03:11 UTC）：** 修正后 `RUST_BACKTRACE=0 cargo test --test compat_serial_registry` 实际 26 passed/0 failed/0 ignored。并发新回归 1/1、fmt check exit0 均为本轮实测；剩余 size/API/workdir、相关套件/Clippy/全量与最终完整矩阵未完成。

**重复读取整改（2026-10-30 18:09:22 UTC）：** 新回归 restore_trusted_stat_does_not_load_fallback_blob 实际 RED（多余 blob load）→ GREEN 1/1。实现先建立 verified_entries，可信路径跳过 decoded-size 查询；仅其余实际更新路径批量预取，失败按旧语义回退，staged-only 仍用 decoded size；保留未变化 gitlink 原 stat。index workdir 单次解析。旧 API 新回归 restore_legacy_api_reuses_published_snapshot 实际 RED（丢弃快照导致 fallback hash）；已传递 snapshots，并缓存 worktree pass 根路径，lib restore 全组验证运行中，尚未计 GREEN。新增测试登记 SRC_SERIAL_REGISTRY；不修改 classifier。

**restore lib 整组 GREEN（2026-10-30 18:11:12 UTC）：** `RUST_BACKTRACE=0 cargo test --lib command::restore::tests:: -- --nocapture` 实际 12 passed/0 failed/0 ignored，包含 3 个新增整改单测（两个竞态窗口、跳过无效 fallback blob、旧 API 复用发布快照）及原有显式目的地/size/error 守卫。旧 API 单独 RED 1 failed 后由整组确认 GREEN。命令 restore/clone/worktree 和 agent_bridge_vcs 定向验证进程已启动，终态待；最终矩阵、严格 Clippy、全量与网站门仍未完成。

**命令组回执（2026-10-30 18:15:00 UTC）：** restore 56/56、clone 43/43 实际通过。worktree 首轮 60 passed/5 failed，backtrace=0 复跑确认五项均在 fixture 初始 commit 因 vault 无 unseal key 失败，未到 restore；只修改 tests/command/worktree_test.rs 的本地 exec_commit helper，加 --no-gpg-sign，未改生产签名或断言。65 项重跑运行中。原串行进程因 worktree 失败终止，未执行 bridge；另启动 bridge_vcs/checkpoint 两 target，终态待。此处扩写集仅 worktree fixture helper。实际宿主 17179869184 bytes（16 GiB），memory free 68%，禁止 nextest；.env.test/.env.live-test、/Volumes/Sea/libra-backend 和 Claude CLI 当前均不可用；不声称全量或网站验收完成。

**worktree GREEN（2026-10-30 18:15:25 UTC）：** 局部 fixture 禁签后 `RUST_BACKTRACE=0 cargo test --test command_test worktree_test::` 实际 65 passed/0 failed/0 ignored，含五个先前前置失败的分支，不再因宿主 vault 条件阻止 restore 断言。restore lib 12/12、命令 restore 56/56、clone 43/43 均保留。bridge_vcs/checkpoint 两 target 仍在运行（exec session 59046），未重启或提前记通过。

**bridge 与静态门进度（2026-10-30 18:16:37 UTC）：** checkpoint target 3/3 PASS；vcs target 首轮 0/8，全部 fixture commit vault 前置失败。局部 fixture commit 禁签后 7/8 PASS，唯一剩余为被测 commit.create 同样缺 unseal key；该 target 不测签名，现将其 repo_with_initial_commit 的临时仓库 commit.gpgSign=false，保留 VCS 全部断言及生产默认，重跑 session 3253。扩写集 tests/agent_bridge_vcs_test.rs 仅 fixture 设置。严格全特性 Clippy session 51316 在跑；因 fixture 后续调整，最终候选还需确认静态输入一致性。上述过程不记全绿。

**补充验证（2026-10-30 18:22:33 UTC）：** bridge VCS fixture 的临时仓库 commit.gpgSign=false 后 8/8 PASS（保留所有 VCS 断言，不改生产默认）；bridge checkpoint 3/3、agent_checkpoint_test 3/3、worktree_isolation_test 119/119 均实际通过。严格全特性 Clippy exit0，既有 block v0.1.6 future-incompat 提示保留；最终静态输入复核待末次 fmt/clippy。父提交 bbfb4ef 导出 TAR 因现有长路径限制失败，部分 TAR 不用；ZIP 完整导出/安全解包成功且无 symlink，父基线源码不含 SP-03 未提交变更。修复后 candidate release build 与新增 serial 守卫仍在运行，不提前计通过。

**final-source registry（2026-10-30 18:25:01 UTC）：** 新增源单测后的 compat_serial_registry 26/26 PASS（137.66s），包括 src lane 与 nextest generator 一致性。候选 release session87659 仍活跃，rustc 实际在运行而非锁文件推断；父源码 ZIP 位于 task-owned /tmp/libra-sp03-final-facg4634，TAR 失败产物不参与。追加 doctor 定向验证运行中。三边矩阵脚本已按完整16格与逐对AB/BA准备，尚未启动计时、不记性能通过。

**候选冻结与最终静态门（2026-10-30 18:29:26 UTC）：** candidate release 实际构建通过（7m58s），SHA256 f42f131662da0b0dac48342a687f1695382d013ec405baf4788907e5a4df0c66，冻结在 /tmp/libra-sp03-final-facg4634/candidate/libra；父源码来自直接父提交 bbfb4ef（包含 SP-02），ZIP hash/上下文记录于 task 临时目录，父 release 同工具链构建运行中 session88130。最终 fmt check 与严格全特性 Clippy 再验 exit0；stat 前缀当前6/6 PASS；两项lib racy与真实status racily-clean各1/1 PASS；doctor13/13 PASS。三边完整矩阵尚未开始，未将修复前矩阵当作新候选结果；父 build 完成后需冻结 parent/libra，并恢复 root target/release/libra 为 candidate，然后运行 /tmp/sp03-final-matrix.py。仍待性能计数/最终矩阵、Issue同步与全量/网站外部门；不标完成。

**路径发现计数与矩阵启动（2026-10-30 18:49:50 UTC）：** 父 release 构建成功（6m15s），SHA256 8675f71605d05a86c65cc1607aa1ae3e91a84453c9ba18d8bfc41bc95718660a；candidate f42f1316 已恢复至 root target/release/libra，两份冻结二进制用于 /tmp/sp03-final-matrix.py，进程36217、产物/tmp/libra-sp03-full-matrix-o29ac28n，当前准备三份完整克隆，尚未计性能通过。辅助计数仅在临时源码副本的 try_get_paths_full_with_storage_lookup 入口打印 marker，debug rustc1.96.0、同一 d0a5c28/2125路径干净组合restore，父三次[15334,15334,15334]、候选[13211,13211,13211]，减少2123次约13.85%；不作release耗时声明。计数bin SHA分别 fae0689ad79ff811f3dba4274583fb21cde9dff04b8562a96ed6ececb981b50c / be1e64b2096d79504f535a9fb7febc6a0bd256449512cf1917a52bd78a6d9afe。早期引用已消失旧夹具失败及copy2旧mtime导致相同二进制的计数均作废；修正为显式检出冻结提交、2125路径断言、touch强制重编译、不同SHA硬断言后才开启计时门。profile前debug二进制已按SHA核验恢复；插桩未进入仓库源码，正式计时与插桩任务不重叠。

**三边矩阵完成（2026-10-30 18:55:39 UTC）：** 36217实际exit0，16格/1200调用/960正式样本，独立 /tmp/sp03-final-report.py 逐条重算样本数、median/p95、exit/stderr/output/index不变量通过。f42f1316候选默认索引8格相对bbfb父版本中位数下降29.77%–51.79%，每格20/20配对更快；refreshed8格变化在-0.93%至+0.70%，不宣称明确收益，既有状态输出语义一致。原始产物/tmp/libra-sp03-full-matrix-o29ac28n/result.json、samples.jsonl、final-matrix.md，正式计时未与profiling重叠。收尾新增 restore_stat_snapshot_unreadable_source_keeps_read_error，将既有 unreadable-source 契约扩至组合restore，验证中session55894；若暴露回归，f42矩阵仅作为该候选历史结果，不替代修复后验收。

**非 Unix stat 证据补充（2026-10-30 19:18:04 UTC）：** 收尾跨平台审查确认共享 verified_index_entry 的非Unix时钟来自 created()/modified()，不足以证明等长保留mtime改写未发生。restore_index_entry 在非Unix传None给既有smudge机制，保留观测size并强制清零ctime/mtime/identity字段，不授予stat短路信任；Unix仍传入hash前metadata。测试预期及四份EN/zh文档同步；没有Windows执行环境，不能声称native Windows通过。新源Unix lib验证14263运行/待终态。此前f42f1316矩阵为该冻结二进制的历史有效结果；生产源码现有补充变更，最终release/矩阵绑定须重新核验，不以旧结果替代新候选。Issue642上一轮已实际同步且readback OPEN，bodySHA3892ea3cdeda961889be08952f7224856a008f97efac2ad18227491708ef2846；现阶段继续in-progress。

**零stat与LFS补充回执（2026-10-30 19:47:31 UTC）：** pure zero_stat_fields_never_authorize_short_circuit 实际RED后，共享stat_diff显式拒绝零ctime/mtime，utils::stat_diff::tests 2/2 GREEN。非Unix把None交verified_index_entry以smudge，保留实际size；Unix沿用pre-read metadata。LFS新增epoch mtime阶段实际RED（size130而非32768），修复restore_index_entry保留shared helper已smudged条目的实际size后单例GREEN 1/1。该零stat必被共享比较函数拒绝，不会被当成clean缓存。原始7/8 AC与18/19 VER计数为f42历史候选；当前新源最终矩阵/静态门待刷新。35223为最终release构建进程；新9组pipeline另起，未提前记通过。7038完成的过渡构建不绑定最终源码，也未用于验收矩阵。

**最终新源绑定（2026-10-30 19:57:54 UTC）：** LFS epoch修复后9组14741实际exit0：stat_diff2/2、restore lib12/12、命令restore58/58、clone43/43、三项racy各1/1、fmt与严格Clippy exit0。最终release35223 exit0（6m27s），SHA256 ee59861c2e557ebf0f478ad27ca2df3a808f442299d2ec1661783ee127095cd5，冻结/tmp/libra-sp03-zero-final-81ogrkt1/candidate/libra。新一轮严格完整三边矩阵71571，产物/tmp/libra-sp03-full-matrix-010cyo8g；使用三份新完整克隆/固定d0a5c28/2125路径，计时门仅在14741终态0及源码manifest/二进制SHA核验后打开。旧f42矩阵是历史，不混入新样本。新矩阵尚在准备，不提前记性能通过；native Windows、T-1全量及网站门未完成。


**最终回执与TODO复核（2026-10-30 20:10:41 UTC）：** Issue #642 已按授权更新并回读OPEN；updatedAt `2026-10-30T20:10:19Z`，正文SHA256 `af92a3bee6df36d2bfcf27765e4af8c1b74db070258501cd0f09ed634604449c`。最终文档状态fmt检查与libra diff --check均exit0；生产源码manifest继续等于采样版本 `7e924054f11c48fcffcf4c658198edec99ed4db2c09d5386971687b5cdecd872`。TODO1–8本地实现/文档与定向证据已完成；TODO9受影响定向检查通过、T-1全量未运行；TODO10最终16格三边矩阵及独立审计完成；TODO11当前结果与远端同步已完成，但G-03粒度计数需单独核对，不把19/20执行回执冒充粒度上限；TODO12按用户批准继续延期。未提交、推送、发布；无Windows原生执行证据。


**G-03计数核对（2026-10-30 20:12:48 UTC，未批准的新例外不生效）：**

| 类别 | 独立门数 | 逐项归属 |
|---|---:|---|
| 当前指定lib门 | 4 | stat_diff全组、restore全组、racy_edit_between_read_and_stat_smudges_the_entry、stat_short_circuit_rehashes_racy_files |
| 当前指定集成门 | 10 | restore、clone、worktree、isolation、doctor、agent_checkpoint、bridge_vcs、bridge_checkpoint、serial_registry、racily_clean_entry_is_content_compared_not_trusted；每个target/filter各计一门，不以同属command_test合并 |
| 当前性能判据 | 1 | SP-00完整16格三边矩阵；16格为同一矩阵协议内的场景，不合并其它独立命令 |
| 重复/历史回执，不另计当前必需门 | 0 | restore_stat_snapshot_八项已包含于最终restore58项，无另增验收要求；仓库发现次数为历史f42辅助计数，不作为最终候选新插桩验收门 |
| 全局强制门，不计特有条目 | 0 | fmt、严格Clippy、编译检查、release构建、T-1全量nextest及ER-06网站文档；其通过状态仍逐项登记，不因不计数而豁免 |

本卡指定验证门合计15，15/8超限。模板G-03要求按独立门计数并禁止私自修改分母；现有EX-SP-03-DOCS-01与EX-SP-03-PERF-01均未豁免此列。

**供用户审阅的处理方案（草案，非已批准决策）：** 建议具名例外 `EX-SP-03-VER-01` 仅豁免SP-03的Verification列至15/8，保留全部15个门、T-1全量与网站门。理由：三模型整改直接涉及同一stat信任/失效轴，物化与index共享入口必须由多个调用方回归证明；将已经执行的必要验证拆到另一张卡会割裂本卡正确性证据。评审输入为sol/k3.1/opus三份报告及本轮计数核对；审批人和正式review PASS尚未取得，不预填。未经用户批准，不激活例外、不标粒度通过或整卡完成。若用户选择拆卡，则先按G-09提出具体拆分与依赖，获得授权后实施，不删除任何已要求的验证。

资源规则复核：本计划已明确32GiB门槛只限制本机nextest，GitHub Actions CI不受该门槛约束；Full Access不改变物理内存。CI结果仍需绑定本次候选提交；当前未获本次提交/推送/建PR授权，不能自动触发新PR流水线，也不能复用旧head的CI回执。


**G-03同步回执（2026-10-30 20:13:39 UTC）：** #642仍OPEN，updatedAt `2026-10-30T20:13:20Z`，正文SHA256 `3797a471abc5edfaa9d4f9c5520d69aee6baed6c827db64ff11d22b1cc336d10`；与库内15/8未获例外及待审草案一致。此次仅两份计划文档记账，生产manifest与最终矩阵一致，fmt检查exit0。用户已切换Full Access，不据此扩大提交/推送、远程执行授权；等待G-03处理决定与全量执行途径，不新增生产代码变更。


**当前生效的用户决策与交付预登记（2026-10-31 01:12:32 UTC）：** 用户明确采用“1→3→2”，以避免CI通过后再专门提交卡片进度触发第二轮CI。先登记以下例外、本地整改结果和后置判据，再将当前候选统一提交/推送并创建PR。此前“未授权提交/推送”与“粒度待批准”仅为历史，不再代表当前授权。此次不合并PR、不发布tag/Release；独立版本发布及网站继续单独跟踪，不将其伪称完成。

本卡具名例外白名单见全局执行约束下的“SP-03 验证数量与CI收口决策”；以下判据为该决策的执行预登记。

**预登记的完成判据：** 本地整改及定向/静态/矩阵已通过。提交包含本卡全部当前源、测试、文档和进度；推送origin/codex/SP-03后创建libra-tools/libra的PR。只有同一最终PR head上的base.yml（Check, Build and Test，包含全量nextest）和codeql.yml全部适用检查成功，且无未关闭P0/P1时，本次修复交付才完成；此前保持remote-pending。失败时按实际错误前滚修复并在新head重跑，不复用旧head结果。没有任何CI成功预登记。

**避免记账触发循环：** 本次CI的head SHA、run ID、计数、结论与UTC观察时间先留在PR/Actions与本次交付答复；按模板v2.14随下一次plan-status.md例行同步登记，不为本次成功回执单独新建提交。卡片的后置判据已在本提交中写明，CI全绿即可满足该条件，不需要再推一笔“设置完成”的提交。

**R-SP03-FINAL / Codex自审：PASS（当前源码与上述用户例外；不冒充三模型再次运行review）。** 发布后hash+pre-read快照与最终stat复核闭环；两个竞态窗口及真实status回归已GREEN；clean CLI/JSON无误报、可信条目避免fallback blob、旧API快照复用、gitlink与flags保留、非Unix保守smudge、零stat拒绝信任、LFS实际size保持均有当前证据。共享调用方与serial守卫重验通过，源码manifest与最终16格矩阵一致。未发现当前未关闭P0/P1；Windows原生执行与全量测试留待PR CI，不用macOS定向结果替代。网站延期及CI待验保持显式。

**合并后R-SP03-FINAL复核（2026-10-31 02:23:54 UTC）：PASS。** 复核SP-03相对98aa6be的实际diff、上游Result处理保留、双方serial登记、当前定向/静态与新16格矩阵证据；无新增未关闭P0/P1。不是对全部上游改动的重新评审，也不是未运行CI的通过证明。

### Task SP-04: 单次 status 复用同一 index 快照

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P2；可选建议项，默认未启用

**Issue:** [#643](https://github.com/libra-tools/libra/issues/643)

**Description:** 在 status 入口获取一致的 index 内容与文件版本信息，供 staged/unstaged 和输出共同消费；循环复用 entry 引用。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** src/command/status.rs:999；src/command/status_untracked.rs:84,93,235；src/command/status/scan.rs:101（基线 380c262，开工刷新锚点）。

**Acceptance criteria:**

- [ ] 单次 status 的 staged 与 unstaged 使用同一 index 快照。

- [ ] racy 判定使用与该 index 版本对应的 mtime，不在读盘后另取可能已替换路径的 mtime。

- [ ] 并发原子替换 index 不产生两份 index 混合的 staged/unstaged 结果。

- [ ] 损坏 index 仍保留既有错误行为。


**Verification:**

- [ ] `cargo test --test command_test status_test::`

- [ ] `cargo test --test command_test status_test::status_index_snapshot_ -- --nocapture` — `(new)` 测试前缀；须非零命中。


**Full-suite trigger:** none

**Dependencies:** SP-09, DEP-SP-OPT

**Deliverables:** 实现、同卡回归及列明文档

**Implementation write set:** `src/command/status.rs`, `src/command/status_untracked.rs`, `src/command/status/scan.rs`, `src/command/status/output.rs`, `tests/command/status_test.rs`

**Release write set:** Inherited

**Docs and compatibility impact:** N/A：保持现有 CLI 参数、输出、错误、配置及存储格式，仅消除内部重复工作；若实现改变契约必须先更新本字段和逐文件写集。

**Rollback mode:** immutable-release

**Migration and rollback:** 无 schema 迁移；发布前回退本卡，发布后前滚修复；不把版本回退视为历史 index 修复。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 无未经实测的绝对毫秒承诺；按 SP-00 的同环境前后配对协议报告中位数、尾延迟及工作量；语义退化即失败，噪声内变化记不确定。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；尚未运行。

**Granularity:** type=implementation; axis=单次 status 复用同一 index 快照; recovery=前滚修复版; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=4; scope=M; deps=SP-09,DEP-SP-OPT; writeset=按实施顺序串行; release=independent; split-from=N/A; exception=N/A

### Task SP-05: status 共享一次 HEAD tree 展开结果

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P2；本轮核心提速：第一梯队

**Issue:** [#644](https://github.com/libra-tools/libra/issues/644)

**Description:** 在单次 status 中将 HEAD tree 展开结果共享给 staged 比较、rename 及 porcelain v2 构造，避免干净仓库默认多次展开。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** src/command/status/scan.rs:101,177；src/command/status.rs:1039；src/command/status/output.rs:1361（基线 380c262，开工刷新锚点）。

**Acceptance criteria:**

- [ ] 有 HEAD 时单次 status 最多展开一次 HEAD tree。

- [ ] 默认 rename 与 --no-renames 的原分类行为保持一致。

- [ ] porcelain v1/v2 与 JSON 输出保持既有语义。

- [ ] unborn HEAD 和子树读取失败遵循既有错误处理。

- [ ] 在相同 packed fixture 上记录前后对象读取次数和无插桩墙钟。


**Verification:**

- [ ] `cargo test --test command_test status_test::`

- [ ] `cargo test --test command_test status_wave0_test::`

- [ ] `cargo test --test command_test status_test::status_head_expansion_ -- --nocapture` — `(new)` 测试前缀；须非零命中。


**Full-suite trigger:** none

**Dependencies:** SP-03

**Deliverables:** 实现、同卡回归及列明文档

**Implementation write set:** `src/command/status.rs`, `src/command/status/scan.rs`, `src/command/status/output.rs`, `tests/command/status_test.rs`, `tests/command/status_wave0_test.rs`

**Release write set:** Inherited

**Docs and compatibility impact:** N/A：保持现有 CLI 参数、输出、错误、配置及存储格式，仅消除内部重复工作；若实现改变契约必须先更新本字段和逐文件写集。

**Rollback mode:** immutable-release

**Migration and rollback:** 无 schema 迁移；发布前回退本卡，发布后前滚修复；不把版本回退视为历史 index 修复。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 无未经实测的绝对毫秒承诺；按 SP-00 的同环境前后配对协议报告中位数、尾延迟及工作量；语义退化即失败，噪声内变化记不确定。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；尚未运行。

**Granularity:** type=implementation; axis=status 共享一次 HEAD tree 展开结果; recovery=前滚修复版; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=3; scope=M; deps=SP-03; writeset=按实施顺序串行; release=independent; split-from=N/A; exception=N/A

### Task SP-06: 对象读取复用请求级 storage scope

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P2；本轮核心提速：第二梯队

**Issue:** [#645](https://github.com/libra-tools/libra/issues/645)

**Description:** 让 status 对象读取消费已 pin 的仓库身份，避免每个子树读取前重新从 cwd 发现仓库；优先显式传递 storage，不顺带重写 database/hooks 全局路径。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** src/utils/path.rs:46；src/utils/util.rs:1136,1364；src/utils/object_ext.rs:54（基线 380c262，开工刷新锚点）。

**Acceptance criteria:**

- [ ] 同一请求子树数量增长不再线性增加仓库发现次数。

- [ ] linked worktree 对象读取仍指向正确 common storage。

- [ ] 并发不同仓库请求不串用 storage。

- [ ] 无 pinned scope 的合法调用保留明确 fallback 行为。

- [ ] 配置/后端读取失败不被缓存成成功或静默回退。


**Verification:**

- [ ] `cargo test --lib utils::`

- [ ] `cargo test --test command_test status_test::status_storage_scope_ -- --nocapture` — `(new)` 测试前缀；须非零命中。


**Full-suite trigger:** T-1：共享路径与对象读取上下文

**Dependencies:** SP-05, DEP-SP-STORAGE

**Deliverables:** 实现、同卡回归及列明文档

**Implementation write set:** `src/utils/path.rs`, `src/utils/util.rs`, `src/utils/object_ext.rs`, `src/command/status/scan.rs`, `tests/command/status_test.rs`

**Release write set:** Inherited

**Docs and compatibility impact:** N/A：保持现有 CLI 参数、输出、错误、配置及存储格式，仅消除内部重复工作；若实现改变契约必须先更新本字段和逐文件写集。

**Rollback mode:** immutable-release

**Migration and rollback:** 无 schema 迁移；发布前回退本卡，发布后前滚修复；不把版本回退视为历史 index 修复。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 无未经实测的绝对毫秒承诺；按 SP-00 的同环境前后配对协议报告中位数、尾延迟及工作量；语义退化即失败，噪声内变化记不确定。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；尚未运行。

**Granularity:** type=implementation; axis=对象读取复用请求级 storage scope; recovery=前滚修复版; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=4; scope=M; deps=SP-05,DEP-SP-STORAGE; writeset=按实施顺序串行; release=independent; split-from=N/A; exception=N/A

### Task SP-07: pack index 整块 fanout 与二分查找

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P2；本轮核心提速：第一梯队

**Issue:** [#646](https://github.com/libra-tools/libra/issues/646)

**Description:** 将普通单对象 pack index 查询改为整块 fanout 读取与有界二分查找，先减少小 read 与线性扫描，不在本卡增加跨请求缓存。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** src/utils/storage/local.rs:1440,1465,1535,1827（基线 380c262，开工刷新锚点）。

**Acceptance criteria:**

- [ ] v1/v2 查询返回与原实现相同的对象 offset。

- [ ] SHA-1/SHA-256 的对象名与 offset 解码正确。

- [ ] 截断或损坏索引返回错误而非 panic 或错误对象。

- [ ] repack/索引替换不读取已失效的复用状态。

- [ ] 固定 packed fixture 的 read 次数显著低于旧逐项/线性读取且报告真实墙钟。


**Verification:**

- [ ] `cargo test --lib utils::storage::local::`

- [ ] `cargo test --lib utils::storage::local::status_pack_lookup_ -- --nocapture` — `(new)` 测试前缀；须非零命中。


**Full-suite trigger:** T-1：共享对象读取路径

**Dependencies:** SP-06

**Deliverables:** 实现、同卡回归及列明文档

**Implementation write set:** `src/utils/storage/local.rs`

**Release write set:** Inherited

**Docs and compatibility impact:** N/A：保持现有 CLI 参数、输出、错误、配置及存储格式，仅消除内部重复工作；若实现改变契约必须先更新本字段和逐文件写集。

**Rollback mode:** immutable-release

**Migration and rollback:** 无 schema 迁移；发布前回退本卡，发布后前滚修复；不把版本回退视为历史 index 修复。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 无未经实测的绝对毫秒承诺；按 SP-00 的同环境前后配对协议报告中位数、尾延迟及工作量；语义退化即失败，噪声内变化记不确定。

**Estimated scope:** S

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；尚未运行。

**Granularity:** type=implementation; axis=pack index 整块 fanout 与二分查找; recovery=前滚修复版; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=1; scope=S; deps=SP-06; writeset=按实施顺序串行; release=independent; split-from=N/A; exception=N/A

### Task SP-08: walk 级 ignore 上下文与 tracked 短路

**Task type:** implementation

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P2；本轮核心提速：第二梯队

**Issue:** [#648](https://github.com/libra-tools/libra/issues/648)

**Description:** 在实际进入的每个目录建立完整 ignore 上下文后跳过 tracked 文件的无效匹配，消除重复发现与不必要调度；保持 unreadable/timeout 和路径安全契约。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** src/utils/util.rs:3181,3365,3504；src/command/status_untracked.rs:368,473,588（基线 380c262，开工刷新锚点）。

**Acceptance criteria:**

- [ ] 每个实际进入目录的自身 ignore 文件被纳入检查，不能仅预热根目录。

- [ ] 根/深层 ignore 读取失败仍产生约定 io_blocked。

- [ ] 逐目录、info/exclude、全局 excludes 的优先级不变。

- [ ] tracked 文件短路不遗漏所需错误报告。

- [ ] 真正 untracked 文件的输出与原规则一致。

- [ ] 目录替换及 symlink 不能把规则读取引向工作区外。

- [ ] 同一 walk 的重复上下文发现与文件级调度减少并有测量证据。


**Verification:**

- [ ] `cargo test --test command_test status_test::`

- [ ] `cargo test --test command_test status_wave0_test::`

- [ ] `cargo test --test command_test status_test::status_ignore_context_ -- --nocapture` — `(new)` 测试前缀；须非零命中。


**Full-suite trigger:** T-1：共享 ignore 解析辅助逻辑

**Dependencies:** SP-07

**Deliverables:** 实现、同卡回归及列明文档

**Implementation write set:** `src/utils/util.rs`, `src/command/status_untracked.rs`, `tests/command/status_test.rs`, `tests/command/status_wave0_test.rs`

**Release write set:** Inherited

**Docs and compatibility impact:** N/A：保持现有 CLI 参数、输出、错误、配置及存储格式，仅消除内部重复工作；若实现改变契约必须先更新本字段和逐文件写集。

**Rollback mode:** immutable-release

**Migration and rollback:** 无 schema 迁移；发布前回退本卡，发布后前滚修复；不把版本回退视为历史 index 修复。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 无未经实测的绝对毫秒承诺；按 SP-00 的同环境前后配对协议报告中位数、尾延迟及工作量；语义退化即失败，噪声内变化记不确定。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；尚未运行。

**Granularity:** type=implementation; axis=walk 级 ignore 上下文与 tracked 短路; recovery=前滚修复版; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=2; prod-files=2; scope=M; deps=SP-07; writeset=按实施顺序串行; release=independent; split-from=N/A; exception=N/A

### Task SP-09: 核心优化复测、全量收口与可选项决策

**Task type:** audit

**Lifecycle / Acceptance:** pending / 空

**Priority / Selection:** P2；必选验收收口

**Issue:** [#647](https://github.com/libra-tools/libra/issues/647)

**Description:** 在核心修复完成后按冻结矩阵验证实际收益和语义，执行最终全量门，决定是否值得启用后续一致性或 IPC/cache 工作。

**Out of scope:** CE_VALID 永久不在本计划范围；其它行为轴按任务总览分别承接；pack 会话缓存/marker 合并/IPC 批处理登记为 DEFER-SP-01..03，未启用。

**Current evidence:** 本计划 SP-00 的基线矩阵；LIBRA_STATUS_PERFORMANCE_REVIEW.md:200-214（基线 380c262，开工刷新锚点）。

**Acceptance criteria:**

- [ ] 已冻结矩阵逐格有前后可比无插桩计时，缺失数据不包装成收益。

- [ ] 语义验证覆盖 clean/dirty、rename、ignore、错误与 index 不变量。

- [ ] 报告新增风险和残余热点，不将 syscall 占比等同墙钟占比。

- [ ] SP-04 与三个 DEFER 项分别记录 adopt/defer 决策及依据。

- [ ] 最终全量门失败全部处置并重跑，未通过时不宣称本轮完成。


**Verification:**

- [ ] 人工核对：基线矩阵每个性能结论能追溯到相同环境/fixture/选项的前后计时与样本数。

- [ ] 人工核对：SP-01/02/03/05/06/07/08 的 focused、fmt/clippy、适用全量和 C/D 证据逐项存在。


**Full-suite trigger:** T-2：核心轮次聚合与最终全量门

**Dependencies:** SP-08

**Deliverables:** 本计划正文与 plan-status.md；不产生新证据文件

**Implementation write set:** `docs/development/plan/plan-20261028-status-performance.md`, `docs/development/plan/plan-status.md`

**Release write set:** N/A

**Docs and compatibility impact:** N/A：仅规划/审计记录，不改变产品行为。

**Rollback mode:** revert

**Migration and rollback:** 无 schema 迁移；撤销本卡文档变更即可；远端 Issue 保留讨论历史，不自动删除。

**Security and privacy:** 保留既有路径边界、错误隔离与超时；只操作隔离 fixture，禁止读取/修改真实全局配置或外传 secrets。

**Performance budget:** 无未经实测的绝对毫秒承诺；按 SP-00 的同环境前后配对协议报告中位数、尾延迟及工作量；语义退化即失败，噪声内变化记不确定。

**Estimated scope:** S

**Version increment:** N/A

**Release boundary:** no-release；规划记录随实现交付进入仓库

**C/D coverage from:** SP-08；尚未运行。

**Granularity:** type=audit; axis=核心优化复测、全量收口与可选项决策; recovery=回退文档; complete=yes; self-contained=yes; AC=5/20; VER=2/20; landing=0; prod-files=0; scope=S; deps=SP-08; writeset=按实施顺序串行; release=no-release; split-from=N/A; exception=N/A

## 延后项（不创建实施任务、不阻塞本轮）

### SP-03 文档依赖暂搁决策（EX-SP-03-DOCS-01）

2026-10-30，用户明确要求“文档依赖先搁置，推进任务”。SP-03 继续本仓库实现、测试和 EN/zh-CN 文档；网站 clone/restore 文档同步、网站验证及部署因 `/Volumes/Sea/libra-backend` 当前不可用，登记为 `DEFER-SP-DOCS-03`。该例外只解除 SP-03 实施阶段对 DEP-SP-DOCS 的阻塞，不将网站交付视为完成，也不允许本卡标记 `done / complete`；恢复网站仓库后须核实 `cf` 分支、写集并完成原 DEP-SP-DOCS 验收。

| ID | 事项 | 启用条件 | 不做的理由 |
|---|---|---|---|
| DEFER-SP-DOCS-01 | SP-01 网站 status 文档同步、验证和部署；未执行 | 网站仓库可用且用户恢复交付时，按原 DEP-SP-DOCS 验收 | 用户先前明确暂搁；不伪称与本仓库文档一并完成 |
| DEFER-SP-REL-01 | SP-01 独立 patch bump、发布构建/安装、tag/Release 及发布后置门；未执行 | 用户授权发布并协调资格修复占用的 v0.30.92 后，确定后续版本并执行原发布门 | 本次指令仅关闭修复卡、提交推送更新 PR；无发布成功证据 |
| DEFER-SP-DOCS-02 | SP-02 网站 restore 文档同步、验证和部署；未执行 | 网站仓库可用且用户恢复交付时，按原 DEP-SP-DOCS 验收 | `/Volumes/Sea/libra-backend` 未挂载，用户明确批准按 DEFER-SP-DOCS-02 暂搁；不伪称与本仓库双语文档一并完成 |
| DEFER-SP-DOCS-03 | SP-03 网站 clone/restore 文档同步、验证和部署；未执行 | 网站仓库恢复可用后，核实 `cf` 分支、冲突与路径，再按 DEP-SP-DOCS 验收 | 用户明确批准先搁置网站文档依赖以推进本地任务；不伪称网站交付完成 |
| DEFER-SP-REL-02 | SP-02 独立 patch bump、发布构建/安装、tag/Release 及发布后置门；未执行 | 用户授权发布时确定后续版本并执行原发布门 | 用户明确要求“首先设置任务完成，然后再提交、推送、创建 PR，版本不变”；无发布成功证据 |
| DEFER-SP-01 | 会话级 pack 列表/句柄/fanout 缓存 | SP-09 仍显示 pack 打开/解析占显著成本，且缓存失效设计获接受，再建卡/Issue | 先验证整块读与二分收益；避免过早引入 repack/并发失效复杂性 |
| DEFER-SP-02 | marker probe 与 read-dir 合并 | SP-09 证实 IPC 残余成本，且替换安全/嵌套仓库语义有可复现回归，再建卡/Issue | 两次 probe 时间边界不同，不能直接删出队检查 |
| DEFER-SP-03 | tracked stat/hash 有界批处理 | SP-09 证实主要残余为该串行路径，并验证逐路径 deadline/取消/部分结果设计，再建卡/Issue | 上游重复工作减少后才有可靠收益判断 |

## 任务卡粒度审计表

| 卡 | type | AC | VER | landing / prod-files | scope | writeset | release | exception |
|---|---|---|---|---|---|---|---|---|
| SP-00 | audit | 4/20 | 2/20 | 0/0 | S | 按依赖串行 | no-release | EX-SP-00-AUDIT-01 |
| SP-01 | implementation | 4/8 | 2/8 | 3/4 | M | 按依赖串行 | independent | EX-SP-01-CLOSE-01 |
| SP-02 | implementation | 5/8 | 3/8 | 4/8 | M | 按依赖串行 | independent | EX-SP-02-CLOSE-01 |
| SP-03 | implementation | 7/8 | 15/8@EX-SP-03-VER-01 | 2/2 | M | 按依赖串行 | independent | EX-SP-03-DOCS-01,EX-SP-03-VER-01,EX-SP-03-CLOSE-01 |
| SP-04 | implementation | 4/8 | 2/8 | 2/4 | M | 按依赖串行 | independent | none |
| SP-05 | implementation | 5/8 | 3/8 | 2/3 | M | 按依赖串行 | independent | none |
| SP-06 | implementation | 5/8 | 2/8 | 2/4 | M | 按依赖串行 | independent | none |
| SP-07 | implementation | 5/8 | 2/8 | 1/1 | S | 按依赖串行 | independent | none |
| SP-08 | implementation | 7/8 | 3/8 | 2/2 | M | 按依赖串行 | independent | none |
| SP-09 | audit | 5/20 | 2/20 | 0/0 | S | 按依赖串行 | no-release | none |

## 测试矩阵与追溯

| 卡 | 报告来源 | 回归落点 | 性能/正确性证据 |
|---|---|---|---|
| SP-00/09 | 两报告统计与本轮用户决策 | 本计划 | 无插桩计时与插桩工作量分开；原样本未复算 |
| SP-01 | Audit Finding 3 | command_test::status_error_test | 缺失/损坏子树错误传播 |
| SP-02 | Audit Finding 1 | storage lib + command_test::restore_test | decoded size 与 peak_bytes 分离 |
| SP-03 | Review §5 | command_test::restore_test/clone_test | stat/racy/LFS/symlink/staged-only |
| SP-04 | Audit Finding 6 | command_test::status_test | 同一 index 快照及版本 mtime |
| SP-05 | Audit Finding 3 | command_test::status_test/status_wave0_test | 默认 rename 与 v2 的单次 HEAD 展开 |
| SP-06 | Audit Finding 4（修正归因口径） | utils lib + command_test::status_test | scope 隔离与发现次数 |
| SP-07 | Review §4 | storage::local lib | v1/v2、hash 类型、损坏索引、read 次数 |
| SP-08 | Review §6 + Audit Finding 5（修正预热前提） | command_test::status_test/status_wave0_test | 深层 ignore 错误、安全路径与调度次数 |

### 基线与实施证据汇总

SP-01 已按 EX-SP-01-CLOSE-01 / EX-SP-01-UPSTREAM-CI-01 合并收口（`e4eb85061af79b4deff37e569265f1c6b82787d4`，#640 CLOSED）；SP-02 已在本仓库完成代码、双语文档、单元/集成/守卫与 fmt/严格 Clippy 验证，并按用户指令登记 EX-SP-02-CLOSE-01（`done / complete`，版本 `0.30.101` 不变；网站与独立发版由 DEFER-SP-DOCS-02 / DEFER-SP-REL-02 跟踪，PR CI 随推送建 PR 触发后单独核验）；SP-03（#642）已按 EX-SP-03-CLOSE-01 登记 `done/complete`；最终 PR head 的全量 CI/CodeQL 全绿并合并为 `b1b9c32`，网站文档仍由 DEFER-SP-DOCS-03 暂搁；SP-04..09 仍为 pending / not-run。SP-00 已执行 release 构建与下述基线采样，`cargo +nightly fmt --all --check` 通过；该历史采样轮未运行 Clippy 和全量测试。Clippy 仍是推送前必跑门；全量测试按 audit 卡的 N/A 触发条件不在本卡执行，当前验收归属按 EX-SP-00-AUDIT-01 调整为本卡独立文档验收；历史登记 SP-01 已被该决策取代。

#### SP-00 冻结协议与环境

DEP-SP-DATA 按用户指定采用公开标准仓库 [`libra-tools/libra@d0a5c28`](https://github.com/libra-tools/libra/commit/d0a5c28a6b236421c14a38339b592fe37137585e)：Git 与 Libra 各自完整克隆并检出该提交，不再使用临时生成 5,000 个文件的测试仓库。未访问历史 Linux 原始归档或工作区外报告，历史数字只保留为引用。基线定义为**同一系统内，指定版本 Git 与当前构建 Libra 在标准仓库的等价状态下直接性能对照**，不是某个操作系统的绝对毫秒数。下表是 macOS 样本；其他系统使用相同协议、指定 Git 版本和当前 Libra 重跑即可验证，不要求持有 Mac，也不跨系统直接比较绝对耗时或倍率。

| 项目 | 冻结值 |
|---|---|
| 标准仓库与提交 | `https://github.com/libra-tools/libra.git`；`d0a5c28a6b236421c14a38339b592fe37137585e` |
| 标准仓库形状 | 2,125 个跟踪文件；两边初始状态干净；另构造 10 个暂存重命名、10 个未暂存修改、100 个未跟踪文件 |
| Libra 源码修订 | `8abdcf7633f8fef8f6d0994d9f0621bb074de292` |
| Libra 构建 | `cargo +nightly build --release --locked`；`rustc 1.100.0-nightly (574ff7d98 2026-09-14)` |
| Libra 发布构建 | `libra 0.30.91`；SHA-256 `72967e40bba1b2d0e8c85d337771509b7ad873a653f5a3d7c89114f8b9b0dc9e` |
| Git 对照 | `git version 2.56.0`；`/opt/homebrew/bin/git`；SHA-256 `40163333061f7223ecb604ed0f12988cfc8096d57ca8b1db006d410d4aeb700e` |
| 本次样本系统 | macOS 27.0.1 (26A434)，Darwin 27.0.0 arm64，Apple M1 |
| 缓存 | 新 CLI 进程、热页面缓存；清系统缓存 not-run（未获额外操作授权） |
| 插桩工作量 | not-run；本表只含无插桩实际经过时间，不能与历史插桩计数混算 |

仓库 runner 仍只直接覆盖 `status --short`。本卡使用一次性 Python 标准库采样器（非交付产物），实际入口为 `python3 /tmp/libra-sp00-standard-baseline.8HXV64/run-baseline.py`，结果为同目录的 `result.json`；以下命令、路径选择规则与断言是重放依据，临时脚本和原始结果不是交付物。

所有子进程使用临时 `HOME`、`USERPROFILE`、`XDG_CONFIG_HOME`、`LIBRA_HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 和空 `GIT_CONFIG_GLOBAL`，并设置 `GIT_CONFIG_NOSYSTEM=1`、`GIT_OPTIONAL_LOCKS=0`、`NO_COLOR=1`、`LC_ALL=C`，不读取真实全局配置。`GIT_OPTIONAL_LOCKS=0` 防止 Git status 的可选索引写回，使两边遵守相同的只读索引不变量。克隆、检出、测试状态构造与刷新均不计时，Git/Libra 分别执行以下等价步骤：

1. Git 执行 `git clone --quiet https://github.com/libra-tools/libra.git <repo>` 和 `git checkout --quiet --detach d0a5c28a6b236421c14a38339b592fe37137585e`；Libra 执行 `libra clone --quiet --no-progress https://github.com/libra-tools/libra.git <repo>` 和 `libra checkout --quiet --detach <同一提交>`。两边 `rev-parse HEAD` 均等于标准提交，初始 `status --porcelain=v1 --untracked-files=normal` 均为空，跟踪路径集合一致。
2. 逐文件核对克隆结果：2,123 个文件内容及符号链接目标一致。`install.ps1` 与 `template/pre-commit.ps1` 是仅有的两个例外，Git 按 `eol=crlf` 检出 CRLF，Libra 检出 LF；去除 CRLF 中的 CR 后内容一致，且两边初始状态均干净。该实际检出差异予以保留，不改写文件制造逐字节相同。
3. 按路径字典序取 `src/**/*.rs` 中前 20 个普通非符号链接文件。前 10 个在原目录改名为 `sp00-renamed-<原文件名>`，各自执行 `add -A -- <新旧路径>`，形成 10 个内容不变的暂存重命名；后 10 个各追加 `SP-00 status baseline change\n`，形成 10 个未暂存修改。HEAD 始终保持标准提交。
4. 在 `sp00-untracked/dir-01/` 至 `sp00-untracked/dir-10/` 各写 10 个文件，共 100 个未跟踪文件。复制整个测试仓库得到默认/已刷新两种状态；仅已刷新副本执行一次对应工具的 `update-index --refresh`。两边均对 10 个修改文件报告 `needs update` 并返回 1，这是预期构造结果，不是 status 样本失败。

矩阵是索引 `{默认, 已刷新}` × 未跟踪文件 `{normal, no}` × 重命名 `{默认, --no-renames}` × 机器可读输出 `{v1, v2}`，共 16 个场景；每个场景分别测量 Git 和 Libra。两边使用相同参数：`--no-pager status --porcelain=v1|v2 --untracked-files=normal|no`，重命名默认格不追加标志，关闭格追加 `--no-renames`。

每个场景和索引状态执行 25 轮，Git/Libra 按 AB/BA 交替顺序运行；每边前 5 次为预热，后 20 次为正式样本。以 `time.perf_counter_ns()` 测量独立进程实际经过时间；中位数为排序后第 10/11 个值的均值，p95 为 nearest-rank（20 个排序样本的第 19 个），另计算 Libra/Git 中位数倍率。每次调用均断言退出码 0、标准错误为空、各工具自身原始标准输出稳定，并比较 `.git/index` 或 `.libra/index` 调用前后 SHA-256。

跨工具语义断言使用排序后的机器可读记录集合：每行作为一条记录，排序后计算 SHA-256，Git/Libra 与两种索引状态必须一致；原始输出哈希仍分别记录，不假定逐字节相同。实测发现 v2 中 Git 将未暂存修改排在重命名前，Libra 顺序相反，记录内容一致，因此采用该规范化规则而不是掩盖排序差异。另断言 normal/no 的未跟踪标记有无，以及默认重命名的重命名记录与 `--no-renames` 的删除/新增记录。下表记录双方原始输出 SHA-256、字节/行数、退出码和索引不变量；预热也计入后三项，但不计入耗时统计。

#### SP-00 基线结果

16/16 个场景均为 measured，Git/Libra 每边各 5 次预热 + 20 次正式样本，无缺失格子；最终完整矩阵共 800 次调用、640 次正式计时，先前采样器断言调试轮不属于样本。Git 与 Libra 测试仓库 HEAD 均为标准提交 `d0a5c28a6b236421c14a38339b592fe37137585e`。耗时单位是 ms；命令列省略两边共同的 `--no-pager status` 前缀。原始输出列依次记录 Git/Libra SHA-256、字节数和行数；退出与索引列均覆盖每边全部 25 次调用。

| 索引 | 未跟踪 | 重命名 | 输出 | 共同命令参数 | 状态 | 每边 n | Git 中位数 / p95 | Libra 中位数 / p95 | L/G 中位数 | 原始输出 SHA-256 G/L / bytes / lines | 退出 G/L | 索引不变 G/L |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 默认 | no | 默认 | v1 | `--porcelain=v1 --untracked-files=no` | measured | 5+20 | 9.970 / 12.893 | 686.544 / 903.225 | 68.863× | G `d59c81cf692430c79ceba8b5834af4d758cee38089788c678c604f2d13652c57` / L `d59c81cf692430c79ceba8b5834af4d758cee38089788c678c604f2d13652c57` / 1008 / 20 | 0/0（各 25/25） | 25/25（每边各 25） |
| 默认 | no | 默认 | v2 | `--porcelain=v2 --untracked-files=no` | measured | 5+20 | 9.630 / 11.281 | 761.951 / 807.037 | 79.119× | G `88aed3eb0ca7b1a07f37995b2c5d0c7608d92222480dd44b144ec095546f02e6` / L `d84f73f2d21971c2507c6503dcbb989df6ee58f64cde345edb6e3102f6a7583f` / 3228 / 20 | 0/0（各 25/25） | 25/25（每边各 25） |
| 默认 | no | no-renames | v1 | `--porcelain=v1 --untracked-files=no --no-renames` | measured | 5+20 | 10.299 / 11.489 | 581.572 / 667.757 | 56.466× | G `d9a0482a655e669a54646e5c88e3f33d3bc238ec4afde579050c5ee5a3b2dd79` / L `d9a0482a655e669a54646e5c88e3f33d3bc238ec4afde579050c5ee5a3b2dd79` / 1008 / 30 | 0/0（各 25/25） | 25/25（每边各 25） |
| 默认 | no | no-renames | v2 | `--porcelain=v2 --untracked-files=no --no-renames` | measured | 5+20 | 9.712 / 11.677 | 680.091 / 833.644 | 70.029× | G `9fdd114d73b798732dba9cdc02acdab8f0a1589060d3d6231a8d431f5812e7de` / L `9fdd114d73b798732dba9cdc02acdab8f0a1589060d3d6231a8d431f5812e7de` / 4308 / 30 | 0/0（各 25/25） | 25/25（每边各 25） |
| 默认 | normal | 默认 | v1 | `--porcelain=v1 --untracked-files=normal` | measured | 5+20 | 15.777 / 20.037 | 915.928 / 1029.299 | 58.055× | G `168ee3f173d5a9da0501cae47e23edc35272e25eee6fe0b2815926e338da8990` / L `168ee3f173d5a9da0501cae47e23edc35272e25eee6fe0b2815926e338da8990` / 1027 / 21 | 0/0（各 25/25） | 25/25（每边各 25） |
| 默认 | normal | 默认 | v2 | `--porcelain=v2 --untracked-files=normal` | measured | 5+20 | 15.896 / 20.614 | 1022.059 / 1205.166 | 64.296× | G `223d1ec03f76660de9413da87f81ef0968cded7430251896e4a1f0356583a078` / L `d3132dbaa1f635c41561c34c8ce97e689e8582d35600df329e95460d61193f6d` / 3246 / 21 | 0/0（各 25/25） | 25/25（每边各 25） |
| 默认 | normal | no-renames | v1 | `--porcelain=v1 --untracked-files=normal --no-renames` | measured | 5+20 | 15.699 / 22.133 | 818.139 / 1027.019 | 52.113× | G `74beebb3d6cc22253e1cf47d5a4379bc25aab5bd0e99227732365302dde19435` / L `74beebb3d6cc22253e1cf47d5a4379bc25aab5bd0e99227732365302dde19435` / 1027 / 31 | 0/0（各 25/25） | 25/25（每边各 25） |
| 默认 | normal | no-renames | v2 | `--porcelain=v2 --untracked-files=normal --no-renames` | measured | 5+20 | 20.144 / 23.943 | 1205.840 / 1649.260 | 59.862× | G `c810acd9dfed468e286f4c1a91fcfd35c7f44eb7d4cc92a8dd5532bccc2e2a6b` / L `c810acd9dfed468e286f4c1a91fcfd35c7f44eb7d4cc92a8dd5532bccc2e2a6b` / 4326 / 31 | 0/0（各 25/25） | 25/25（每边各 25） |
| 已刷新 | no | 默认 | v1 | `--porcelain=v1 --untracked-files=no` | measured | 5+20 | 11.773 / 16.355 | 500.876 / 754.638 | 42.543× | G `d59c81cf692430c79ceba8b5834af4d758cee38089788c678c604f2d13652c57` / L `d59c81cf692430c79ceba8b5834af4d758cee38089788c678c604f2d13652c57` / 1008 / 20 | 0/0（各 25/25） | 25/25（每边各 25） |
| 已刷新 | no | 默认 | v2 | `--porcelain=v2 --untracked-files=no` | measured | 5+20 | 11.442 / 20.081 | 608.665 / 809.822 | 53.196× | G `88aed3eb0ca7b1a07f37995b2c5d0c7608d92222480dd44b144ec095546f02e6` / L `d84f73f2d21971c2507c6503dcbb989df6ee58f64cde345edb6e3102f6a7583f` / 3228 / 20 | 0/0（各 25/25） | 25/25（每边各 25） |
| 已刷新 | no | no-renames | v1 | `--porcelain=v1 --untracked-files=no --no-renames` | measured | 5+20 | 10.848 / 13.265 | 332.737 / 416.497 | 30.672× | G `d9a0482a655e669a54646e5c88e3f33d3bc238ec4afde579050c5ee5a3b2dd79` / L `d9a0482a655e669a54646e5c88e3f33d3bc238ec4afde579050c5ee5a3b2dd79` / 1008 / 30 | 0/0（各 25/25） | 25/25（每边各 25） |
| 已刷新 | no | no-renames | v2 | `--porcelain=v2 --untracked-files=no --no-renames` | measured | 5+20 | 11.325 / 14.416 | 467.609 / 554.719 | 41.291× | G `9fdd114d73b798732dba9cdc02acdab8f0a1589060d3d6231a8d431f5812e7de` / L `9fdd114d73b798732dba9cdc02acdab8f0a1589060d3d6231a8d431f5812e7de` / 4308 / 30 | 0/0（各 25/25） | 25/25（每边各 25） |
| 已刷新 | normal | 默认 | v1 | `--porcelain=v1 --untracked-files=normal` | measured | 5+20 | 16.611 / 22.278 | 694.357 / 1073.463 | 41.802× | G `168ee3f173d5a9da0501cae47e23edc35272e25eee6fe0b2815926e338da8990` / L `168ee3f173d5a9da0501cae47e23edc35272e25eee6fe0b2815926e338da8990` / 1027 / 21 | 0/0（各 25/25） | 25/25（每边各 25） |
| 已刷新 | normal | 默认 | v2 | `--porcelain=v2 --untracked-files=normal` | measured | 5+20 | 18.886 / 26.372 | 897.033 / 1294.295 | 47.498× | G `223d1ec03f76660de9413da87f81ef0968cded7430251896e4a1f0356583a078` / L `d3132dbaa1f635c41561c34c8ce97e689e8582d35600df329e95460d61193f6d` / 3246 / 21 | 0/0（各 25/25） | 25/25（每边各 25） |
| 已刷新 | normal | no-renames | v1 | `--porcelain=v1 --untracked-files=normal --no-renames` | measured | 5+20 | 18.447 / 28.521 | 647.583 / 975.519 | 35.105× | G `74beebb3d6cc22253e1cf47d5a4379bc25aab5bd0e99227732365302dde19435` / L `74beebb3d6cc22253e1cf47d5a4379bc25aab5bd0e99227732365302dde19435` / 1027 / 31 | 0/0（各 25/25） | 25/25（每边各 25） |
| 已刷新 | normal | no-renames | v2 | `--porcelain=v2 --untracked-files=normal --no-renames` | measured | 5+20 | 19.897 / 32.019 | 799.181 / 1038.466 | 40.166× | G `c810acd9dfed468e286f4c1a91fcfd35c7f44eb7d4cc92a8dd5532bccc2e2a6b` / L `c810acd9dfed468e286f4c1a91fcfd35c7f44eb7d4cc92a8dd5532bccc2e2a6b` / 4326 / 31 | 0/0（各 25/25） | 25/25（每边各 25） |

最终完整矩阵全部 800 次 status 调用的标准错误均为空；16 组 Git/Libra/索引状态的规范化记录集合一致，各工具原始输出稳定，未跟踪与重命名语义断言通过。采样结束后的索引 SHA-256 为：Git 默认 `64ddde456b6812b4c6442fc4980bbce28434b6df54b443531ca2b25bf5e13da9`、Git 已刷新 `eb75558abcd6e37822a75a38e0d1c77402ba14d673bfd7e8b494df07300bdc6f`、Libra 默认 `4c0d8ce6c8c3c2e694a84f0c033c4b9e8795b8208e2059228eea588496d44634`、Libra 已刷新 `1c31c085e4c09ff31ba902c174eda230d00d664f1ee7cef1f38898f5fd45a624`。两边预备刷新均返回预期 1，status 样本自身全部返回 0。当前样本的 Libra/Git 中位数倍率为 30.672–79.119×；不显示未跟踪文件时为 30.672–79.119×，显示时为 35.105–64.296×。该表以 Git 2.56.0 为外部参照，不设未经决策的追平目标，也不把单轮倍率或两种索引状态的差值解释为后续优化收益。

审计口径复核：9,647 次 readlink 属于 242 次总发现，206 次对象读取只是其中的另一归因子集，不能把全部 readlink 算到对象读取；线程池提交次数不能推出固定线程切换下界；`ignore_sources_for_target` 从 target 父目录开始，根预热既未覆盖目录自身 ignore 文件，也不能单独证明 tracked 短路等价，因此“零风险预热”结论不成立。出队 marker probe 仍承担防目录替换的重新验证，本卡不建议删除。

历史报告中的约 733→491 ms、关闭 untracked 约省 200 ms、约 12 万次 idx read 及 242/206 次计数均为未访问原始归档的历史引用，本轮未独立复算；本轮实测仅以上述同机 Git/Libra 16 场景无插桩对照为准。冷缓存、其他 OS 和插桩工作量仍为 not-run；其他系统可按同一协议产生自己的 Git/Libra 对照，不需要复现这份 macOS 绝对数据。

### 里程碑、风险与恢复

| 里程碑 | 完成条件 | 恢复 |
|---|---|---|
| M0 | SP-00 基线与边界确认 | 文档回退，不改代码 |
| M1 | SP-01..03 正确性门通过 | 前滚修复；staged-only 不伪造 stat |
| M2 | SP-05..08 核心重复工作消除 | 各独立切片前滚，保留安全检查 |
| M3 | SP-09 全矩阵与最终全量通过 | 失败留 open，修复后重跑；可选项可保持延后 |

风险：共享 storage/util 与资格计划冲突（DEP-SP-STORAGE）；外部归档不可用（DEP-SP-DATA）；网站落点未核实（DEP-SP-DOCS）；缓存/批处理扩大安全面（DEFER）；多个因素交互导致收益不可加和（SP-00/09 配对复测）。

## 完成判据

- [ ] 必选与本轮核心任务达到 done/complete，所有适用门证据写入本计划；未启用 SP-04 明确保留 optional pending。
- [ ] SP-09 的最终全量、fmt/clippy 通过；发现的缺陷前滚修复并重跑。
- [ ] 默认命令与原 no-renames 基准都被覆盖；未测平台和冷缓存限制明确披露。
- [ ] 适用网站文档、部署及远端 C/D 门完成，或明确保持未完成，不能以局部绿代替。
- [ ] plan-status 同步；Issue 关闭依据为实际验收而非创建任务或合并 PR。

## 自审与修订历史

本轮自审：正确性与性能分卡；无 CE_VALID 重复工作；实现卡均不超过 M/8AC/8VER；重叠写集按顺序串行；网站与外部数据未核实处有显式依赖。正式外部 review 未执行，不宣称 agent-ready。最终纯文档登记提交采用模板 v2.14 终止链协议：推送后 D 组 SHA/run ID/结论/UTC 由下一次 plan-status 例行同步补记，不制造无限自引用提交。

| UTC 时间 | 变更 | 状态 |
|---|---|---|
| 2026-10-28 01:56:39 UTC | 依据两报告及用户决策建立 SP-00..09；CE_VALID 排除，SP-04 可选，缓存/marker/IPC 延后 | pending；无实施 |

| 2026-10-28 02:18:21 UTC | 用户授权提交并推送两份计划文档；原报告由用户移出工作区。文档路径/任务依赖 DAG 自查通过，`cargo +nightly fmt --all --check` 通过；Clippy 在依赖获取前被 Cargo 缓存写权限阻塞，提升权限两次自动审核超时，未完成代码检查；nextest 未启动 | 仅文档交付，不表示实现卡或 C/D 验收完成 |

| 2026-10-28 09:42:48 UTC | SP-00 在 `codex/SP-00` 冻结 macOS 新基线；16/16 格、400 次调用与 320 次正式样本完成，输出/退出/index 断言全过，审计口径与历史引用边界已复核 | 当时曾标本地 done / complete；后续被 Git 直接对照方案取代 |

| 2026-10-28 10:19:45 UTC | 用户要求以指定 Git 与当前 Libra 直接对照；SP-00 重开并完成 16 场景、800 次调用与 640 次正式计时，双方语义、退出与索引断言全过，v2 排序差异以规范化记录集合处理 | SP-00 本地 done / complete；未提交、未推送，Issue #639 保持 open |

| 2026-10-28 12:30:24 UTC | 用户指定 `libra-tools/libra@d0a5c28a6b236421c14a38339b592fe37137585e` 为标准仓库；Git/Libra 分别完整克隆并检出该提交，在 2,125 个跟踪文件的真实工作区上重跑 16 场景，最终完整矩阵 800 次调用、640 次正式计时及语义/退出/索引断言全过；两个 PowerShell 文件的 CRLF/LF 检出差异已披露，旧合成仓库数据仅保留为历史 | SP-00 本地 done / complete；未提交、未推送，Issue #639 保持 open |

| 2026-10-29 | #639 收口复核：在 `codex/SP-00` 以未提交合并方式合入 main `1e5310c24ef4eaa07575256482490705181c360a`（含 PR #654），保留其全部生产/测试修复；`plan-status.md` 冲突保留本卡基线行及 main 其它最新条目。16 格唯一组合、800/640 样本数及展示精度下倍率一致性检查通过；历史采样原件未读取、基准未重跑，原采样结果按 PR #652 记录引用，非本轮独立实测。修正“audit 可免 Clippy”表述及过早 complete：本卡为 in-progress / locally-accepted，SP-01 的 C/D 覆盖仍待；本轮 fmt 与合并后 `cargo clippy --all-targets --all-features --offline -- -D warnings` 均 exit 0（Clippy 58.85s）；依赖 `block 0.1.6` 的 future-incompat 提示保留，不称零警告。首轮在合并完成前读到旧源码的 Clippy exit 101，仅作失败历史，不用于判断 #654；本轮未启动 nextest。 | PR #652 已提交/推送、仍 open；本轮修复尚未提交/推送，Issue 未关闭；不发布版本、不扩大到 SP-01 |

| 2026-10-29 | 用户批准 EX-SP-00-AUDIT-01：SP-00 取消继承 SP-01 发布验收，改为自行完成文档/评审/静态门及 PR #652 最终 head 的 CI；保留 SP-01 对 SP-00 的执行依赖及其原有 C/D。历史反向继承记录保留为当时事实，以本决策为现行口径。 | Codex 本轮文档复核 PASS（范围、依赖方向、现行/历史分离及验收条件一致）；SP-01 卡逐字未改、4 AC/2 VER 保留、workflow 与变更内链接目标及冲突标记检查通过。fmt 与严格 Clippy（offline，1.88s）均 exit 0，既有 block 0.1.6 future-incompat 提示保留；未重跑基准或 nextest。旧索引中的跨仓链接不在本次变更检查范围。待提交/推送；最终 head CI、合并与 Issue 收口仍未完成，保持 in-progress / locally-accepted |

| 2026-10-29 SP-01 开工 | 实核 #652 已合并 d0578db，最终 head 全部 CI SUCCESS；据 EX-SP-00-AUDIT-01 将 SP-00 登记 done/complete，#639 仍 open 另计。用户授权 SP-01，从 upstream/main d0578db 创建 codex/SP-01；发现根 tree 安全加载后仍有三处使用递归 panic 展开路径。测试边界沿用卡内指定 status CLI 集成测试。 | 第一条缺失第二层子树回归已写，等待 RED；网站 /Volumes/Sea/libra-backend 不存在，用户明确暂且搁置网站同步，公开文档交付门仍未完成；本机 macOS arm64、物理内存 17179869184 bytes（16 GiB）且资源压力已查，禁止 nextest。两份 .env 测试文件缺失；本轮仅运行使用隔离 HOME/config 的既有 deterministic CLI 测试，不宣称全量通过 |

| SP-01 本仓库 RED → GREEN | 首次短过滤器搭配 --exact 命中 0，不计验收；改为完整 `command::status_error_test::status_nested_tree_missing_returns_repo_error` 后实际 1/1 RED：Tree::load 缺失第二层子树 panic、CLI exit 1 而非 128。加入携带 OID/source 的 TreeExpansionError 和独立 fallible 展开函数，status 三处展开调用改为错误传播；保留 TreeExt 原公开契约与其它命令原路径。首次 missing 1/1 GREEN；扩展四例 4/4 GREEN；独立函数调整后整个 status_error_test 14/14 GREEN（9.43s），包含四例新回归。 | 三类错误各覆盖 human/no-renames/porcelain v1/v2/JSON/machine 六种调用，均 exit 128、LBR-REPO-002、包含 OID 与 fsck 提示、stdout 空、结构化模式单 JSON、index 字节不变；合法嵌套 gitlink 三种输出成功。EN/zh-CN status、错误码表和兼容表已同步；网站按用户决定搁置。严格 Clippy（all-targets/all-features，offline，2m12s）exit 0，既有 block 0.1.6 future-incompat 提示保留；Display 单元 1/1、现有缺失 HEAD 1/1、现有 gitlink 2/2 通过；新增前缀在最终实现再次 4/4 通过（4.26s）。含 status_error_test 14 项共 18 个不同用例，重跑不重复计数。补充回归初次误用文件名 status_wave0_test 作模块过滤命中 0，不计通过；查询测试清单后以实际模块 command::status_wave0 执行以上 1/1 和 2/2。fmt、diff check、双语文档及 TreeExt 契约/三调用点检查通过。代码差异自查未新增生产 panic/unwrap/expect；正式完整验收与 C/D 未取得，仍 in-progress / 验收空 |

| SP-01 当前交付边界 | 本仓库四个生产文件、status_error_test 与四份现有命令/错误/兼容文档已修改；原 TreeExt trait 逐字保持，status 的三处展开均改为 fallible 调用。测试和静态门实证见上一行。 | 网站依用户决定暂搁；16 GiB nextest 硬门槛与缺失双 env 使 T-1 全量 not-run；版本 bump、release build/安装、提交/推送、PR/发布及远端 C/D 均未执行，不将局部绿标为 complete |

| 2026-10-30 用户澄清与 CI 交付 | 用户明确“在本机资源不足时候不触发，CI 不在此列”；32G 门槛仅适用于本机，CI 按现有 base.yml 运行，不修改工作流、不增加资源检查。SP-01 实现已 signed+DCO 提交 aaa2a986be5a6d613749aa78d27f000105a57700 并推送 origin/codex/SP-01；用户已授权创建 PR 和触发全量。 | 本轮同步澄清后推送并创建草稿 PR，触发现有 compat-offline-core 全量；CI 结果待实证，本机 nextest 仍 not-run，网站仍依用户决定搁置；版本发布与完整 C/D 未完成 |

| 2026-10-30 SP-01 修复卡完成 | 用户明确要求设置卡片完成、推送更新 PR。依据 EX-SP-01-CLOSE-01，本仓库修复、文档、18 个不同定向用例、fmt/Clippy，以及 bb88db3 的全量 CI/CodeQL 均通过；nextest 10841/10841 passed、13 slow、23 skipped，577.819s。 | SP-01 done/complete；网站和独立版本发布仍未执行，分别登记 DEFER-SP-DOCS-01 / DEFER-SP-REL-01。PR #655 仍 open，未合并；本次仅文档更新，不回写旧 head 运行结果、不声称新 head CI 已成功 |

| 2026-10-30 SP-01 合并授权与上游问题移交 | 用户授权先建 Issue、再合并 #655 并完成卡片；已创建 #656，登记 EX-SP-01-UPSTREAM-CI-01。新基线全量失败 1 项、2 项 flaky 的事实保留，extraction.rs 基线/merge blob 一致及上游 Hermes 新分支证据已核。 | 本次仅文档记账，SP-01 done/complete；允许带该已知上游失败合并，#656 保持 open。合并 SHA/Issue #640 关闭结果以后续实际回执为准，不在提交前伪造；不新增版本发布或网站部署完成声明 |

| 2026-10-30 03:00:58 UTC | 实核 SP-01 PR #655 已合并为 `e4eb85061af79b4deff37e569265f1c6b82787d4`（2026-10-30T01:23:41Z）、#640 已关闭（01:23:43Z）。用户选定并授权 SP-02（#641），确认：(1) 基于 main `e4eb850` 创建 `codex/SP-02`；(2) `/Volumes/Sea/libra-backend` 未挂载，网站文档按 DEFER-SP-DOCS-02 暂搁；(3) 本机 16 GiB 内存（`17179869184` bytes）低于 32 GiB 硬门槛，本机 nextest 记 not-run、交由 PR CI 执行；(4) 批准实现写集由 5 扩至 8 个生产文件（新增 `src/utils/storage/tiered.rs`、`src/utils/storage/load_cost/mod.rs`、`src/utils/storage/load_cost/loose.rs`，`landing/prod-files` 更新为 `4/8`，仍属 M 范围且与在跑计划无写集冲突）。先复现 RED：`restore_delta_index_size_matches_decoded_length_for_packed_deltas` 在修复前将 `peak_bytes`（`52952`）写入 `.libra/index` `size` 而非真实 decoded 长度（`26400`）。完成 `decoded_object_size(s)` 链路（loose/pack OFS+REF 多层 delta `result_size`、超限对象返回 `None` 交回 fallback、`LocalStorage` alternates、`TieredStorage` 仅本地层、`ClientStorage` 保持配置拒绝 fail-closed），`restore_index_tracked` 改用 `decoded_object_sizes` 并覆盖新增与已跟踪跨版本更新条目；保留 `object_size(s)`/`object_sizes_with_total_limit`/`get_with_limit` 的 `peak_bytes` 预览限额与 staged-only 零 stat 语义；同步 EN/zh-CN `restore.md`。验证：`cargo test --lib utils::storage::` 84/84、`storage_config_hold` 11/11（1 ignored）、`command::restore::tests::` 9/9、`restore_test::` 50/50、新增 `restore_delta_index_size_` 4/4、`compat_serial_registry` 26/26、两项生产 unwrap 守卫各 1/1，`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0（依赖 `block v0.1.6` future-incompat 提示保留）。 | SP-02 登记 `in-progress / 空`；本仓库实现、测试、双语文档与静态门已完成，尚未提交/推送/建 PR，远端 CI、DEFER-SP-DOCS-02 网站交付及独立 patch 发布门仍待 |

| 2026-10-30 03:06:14 UTC | 用户明确授权并指示“首先设置任务完成，然后再提交、推送、创建 PR，版本不变”。按用户决策登记具名本卡例外 `EX-SP-02-CLOSE-01` 与独立发版延期项 `DEFER-SP-REL-02`，将 SP-02 置为 `done / complete`，保持 `Cargo.toml` 版本 `0.30.101` 不变；准备精确暂存 13 个本卡文件、PGP+DCO 签名提交、推送 `origin/codex/SP-02` 并创建上游 PR。 | SP-02 `done / complete`（`EX-SP-02-CLOSE-01`）；网站与独立版本发布分别由 `DEFER-SP-DOCS-02` / `DEFER-SP-REL-02` 跟踪（未执行）；PR CI 在推送建 PR 后触发，不在提交前伪称远端通过，Issue #641 由 PR/后续核验闭环 |

| 2026-10-30 19:08:43 UTC | 三模型整改实施与最终定向/静态/三边矩阵回执统一登记：18/19 VER，全量 nextest not-run；clone 为预期成本。 | SP-03 继续 in-progress；Issue 同步待远端回执，不提交/推送/发布；网站 DEFER-SP-DOCS-03 保留 |

| 2026-10-30 20:08:44 UTC | 最终ee598候选：新源定向/静态及16格三边矩阵独立审计PASS，调用方已按最终共享stat helper重验；19/20 VER（含明确标识的历史计数证据）。 | T-1全量和网站门未完成；SP-03仍in-progress，Issue同步待回执，未提交/推送/发布 |

| 2026-10-31 01:12:32 UTC | 用户采用1→3→2，批准SP-03验证数量例外并授权统一提交/推送建PR；本地整改与CI后置判据预登记。 | EX-SP-03-VER-01、EX-SP-03-CLOSE-01生效；本地完成/remote-pending，CI不预记通过，不专建成功记账提交；网站与独立发布保留延期/未执行 |

| 2026-10-31 02:23:54 UTC | PR #661必要的主线集成：合并98aa6be，保留双方serial登记；最终定向/静态、两release、新16格三边矩阵独立核验通过。 | 更新同一PR；最终head CI仍待验，不另建成功记账提交；历史bb8/ee598证据保留且明确版本边界 |

| 2026-10-31 19:59:44 UTC | 用户授权新建 `codex/SP-03-closeout` 并直接完成 SP-03 收口；实核最终 head `dbd21ed8` 的 base run `41227810790` 与 CodeQL run `41227810789` 全部适用检查 SUCCESS，PR #661 已合并为 `b1b9c32259d5e2d972521e1cc30129e5319488d2`。 | SP-03 按 EX-SP-03-CLOSE-01 登记 done/complete；本机 nextest 历史 not-run 保留；网站与独立发布仍未执行，Issue #642 已关闭（completed） |
