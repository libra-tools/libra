# 计划执行状况总表（plan-status.md）

> **本文件是全仓计划的单一执行状况视图**，按任务卡粒度汇总每一份计划的执行状态，并登记计划内的延后决策/实施项（`DEFER-*`）与跨计划依赖。**任何执行计划的 Agent 在完成/推进一张任务卡时，必须在同一变更中同步更新本文件**；新建计划时必须在「计划一览」登记一行，并把本文件的更新义务写入新计划的「使用规则」或修订历史。
>
> **维护规则（强制）**
>
> 1. 每张卡的状态推进（`pending` → `in-progress` → `blocked` → `done`，`Acceptance` 随 ER-04 转移）都在「计划一览」的对应行更新，并附发布版本 / commit / 时间（`YYYY-MM-DD HH:MM:SS UTC`）。
> 2. 计划收口、拆卡、合并发布、新增 `DEFER-*`、`DEP-*` 状态变化，同步更新「延后与未决策项」与「跨计划依赖」两节。
> 3. 新建计划：在「计划一览」加一行（类别、状态、一句话进度），并在「未启动计划」或「实施中计划」小节落位。
> 4. 以「计划一览」表为权威，其余小节是它的展开视图；冲突时以任务卡自身 `Lifecycle / Acceptance` 与 `plan-long.md` 的日期索引交叉核对。
> 5. 状态快照时间见本文件头，格式为 `YYYY-MM-DD HH:MM:SS UTC`（24 小时制、UTC、精确到秒）。每次更新必须把快照时间改成这次写入时的 UTC 时钟时间，便于多个 Agent 区分先后。已经写下的纯日期记录保持原样，不补写时间。
>
> **当前快照：** 2026-10-09 11:25:15 UTC（模板 `v2.12`：验收证据写入计划文件，不新建证据文件或文件夹）。[`plan-20260904.md`](plan-20260904.md) FIX-RG-01 `done`/`complete`（v0.30.39：PR #624 → 9b8fc5c7，release run 37777413227 八任务 success，默认 macOS 安装 smoke PASS；2026-10-08 13:54:21 UTC）。[`plan-20260926.md`](plan-20260926.md)：**DM-00 `done`/`complete`**（2026-10-07 01:03 UTC——`## 取材清单` 追加（34 项 memory 文件 + 迁移号跳越 + `MemoryConfidence` 来源），#456 远端事实带 UTC 时间戳冻结（`state=CLOSED`、`mergeable=MERGEABLE`、`mergeStateStatus=UNSTABLE`、149 files、26 commits、reviews=0），`plan-20260819` 已正式登记退役（M2-01..M2-16E 全部退役、R30 评审日志保留）；取材 clone 降级至 `/tmp/libra-pr456`（`/run/media/genedna/data` 不可写），pin `bc1be587e4` 与 merge-base 校验通过。`plan-20260819.md` 状态改「已取代」。）；**DM-01 `done` / `complete`**（2026-10-07：迁移 A `2026092901_memory_core` 已交付并经 REL-DM-01 独立发布至 **v0.30.31**（merge SHA `603bad3`、PR #614、release run `37588423715`）；本地全量 nextest（umask 0022）9549/9549 全绿，D 组 `base.yml`/CodeQL 全绿，`compat_version_surface_sync` 通过；期间补齐 `mutable_state_ownership` 对三张投影表的分类（`MIGRATION_ONLY_TABLES`））。**DM-10 `done` / `complete`**（2026-10-07：迁移 B `2026092902_memory_path_search` 交付并经 REL-DM-01 独立发布至 **v0.30.35**（merge `c384bc7`、PR #618；随后的版本面补丁 `b51f5c3` 把 0.30.35 对齐到 tag，`compat_version_surface_sync` 通过）；D 组 `base.yml`/CodeQL 全绿、`compat-offline-core` 全量通过；含 `mutable_state_ownership` 对二张新表的分类与模拟旧 runner `UnsupportedFuture` 断言）。**DM-02 `done` / `complete`**（2026-10-08：`src/internal/ai/memory/` commit/change 派生实现并经 REL-DM-01 独立发布至 **v0.30.37**（merge `125657b`、PR #620）；本地聚焦门 + `memory_projection_test` 全绿、DM-02 `rg` 守卫零命中、含修复 `parse::<ObjectHash>` → `object_format::parse_repo_oid` 的 `compat_object_hash_parsing_guard` 通过；D 组 `base.yml`+CodeQL 全绿、`compat-offline-core` 全量通过。ER-05 评审待记录）。**DM-13 `done` / `complete`**（2026-10-08：commit 源新鲜度/horizon/重建等价实现并经 REL-DM-01 独立发布至 **v0.30.38**（merge `99c4bce`、PR #622）；`memory_projection_test` 11/11 全绿、D 组 `base.yml`+CodeQL 全绿、`compat-offline-core` 全量通过；ER-05 评审待记录）。`plan-20260907` 仍**已收口** —— B3-00..B3-17 全部 `done`/`complete`（B3-00 `v0.23.68` → B3-10 `v0.29.0`；REL-B3-01/02 家族经 B3-15/B3-17 发布）；最终 Codex/Claude 双评审 R40 **PASS**（0×P0/0×P1）；GC-B3-01/GC-B3-02 收口守卫全绿（本次复核，含把 `bundle_client.rs` 的 `as_str` 臂与 `local_client.rs` 注释收敛到 `object_format::as_str`/注释化，消除 GC-B3-01 零命中守卫告警）；`DEP-B3-05` 已满足。`plan-20260913.md`（Media）实为 **实施中**：FL-00/01/07/02/06/03/04/05 均 `done`/`complete`（`v0.30.0`..`v0.30.5`），仅剩计划级收口门——此前本文件误列「未启动 / FL-00 pending」，已更正。 [`issues/500.md`](issues/500.md)：R41 终确认轮 Codex full-plan **PASS**（findings: clean，P0=0、P1=0、P2=0）及 Claude targeted **PASS**（P0=0、P1=0、P2=0，仅 P3 nit）——**双评门已满足**。**2026-10-05 执行收口：NO-GO**——本地 trunk compose 部署实测 CS-16/19/17 均 `not-qualified`（storage-only 读面无独立 ACL；pin 无 committed-set revoke 路由），CS-20 aggregate=`not-qualified`，且维护者未点名 `agent_session`（已发征询）；CS-00 判定 **NO-GO**，CS-10/11/12/13 延入 DEFER-CS-02/03/07/10，CS-18 登记 defer+outgoing handoff，CS-14 closeout 收口。**2026-10-05 收口 patch 发布：`v0.30.29`**（PR #613/queue head `370dc94`；版本面与 `Cargo.lock` 一致；恢复 `tests/compat-ledger/SURFACES.gen` exec bit（100755）；本地全量 `nextest --all --no-fail-fast --retries 2`（umask 0022）9525/9525 全绿（1 flaky 重试转绿），`fmt`/`clippy -D warnings` 通过；远端 D 组：D-1 base.yml PR #613 head `ade89a1`（run `37344599122` attempt 2）全绿；D-2a CodeQL PR run `37344599037` 全绿；D-3 release run `37336809254` 8/8 全绿；D-2b merge-to-main CodeQL run `37354926375` 全绿（merge SHA `cbded577`，security-codeql-actions/Rust 均 success）。**当前卡：CS-00/14/15/16/17/18/19/20 `done` / `complete`；CS-10/11/12/13 `deferred` / `not-applicable`。[`plan-20261001-mega-browser-noninteractive.md`](plan-20261001-mega-browser-noninteractive.md)：MN-05/06/09 已合入 `origin/main`（v0.30.24–v0.30.26）。[`issues/498.md`](issues/498.md)：**已收口**——TT-05 / TT-02 均为 `done`/`complete`（v0.30.22 / v0.30.23）；M3 全量收口门 8754/8754 passed；#498 CLOSED；DEFER-TT-01..08 残留。[`plan-20261001-mega-browser-noninteractive.md`](plan-20261001-mega-browser-noninteractive.md)：12 卡全部 `done`/`complete`，v0.30.27 已发布；原实现基线 `faae20c` 的全量 8839/8839 与网站示例 1/1 保持为历史证据；Codex R19 `PASS`（最终计划收口审查）与 R20 `PASS`（对照 incoming main `721fec9` 的文档差异审查）见计划评审日志。R21 FAIL 的文档问题已由 R22 PASS 复核关闭。PR #609 原 head `adb9b59`（base `892f37c`）的 run `37201651296` 有 2 个失败 job、4 个失败测试（9523 项中 9519 通过）；PR 于 2026-10-04 14:27 UTC 以该 head 合并。后续 CI 修复已从当前 main `622bed5` 拆至分支 `fix/opencode-exporter-cancel-ci`，全量本地测试、格式及 workflow lint 通过；本地 macOS Clippy 未编译 Linux-only helper，PR #612 首轮 run `37230096807` 因 `linux_pidfd_supported` unused 而失败；该未调用函数已删除并等待新 run 验证。run `37231625328` 的 `opencode-export-linux` 在 `runner_controls_preserved` 失败，因为通用 fixture runner 没有 PID namespace；现只在 trusted-bwrap 入口要求 namespace-init pidfd，bridge E2E 因前置失败未运行；本地 exporter 模块定向测试 15/15、Clippy、格式与 workflow lint 通过；run `37233480909` 的 Linux sandbox gates 通过，但 Clippy 与 bridge 集成测试编译均被 `run_bounded_exporter` 的 `dead_code` 阻断。已将该仅用于单元测试的 wrapper 限定为 `cfg(test)`，待新 run 验收 Clippy 与真实 bridge E2E。[`plan-20260924.md`](plan-20260924.md)：**已收口**（ACF-09 `Lifecycle=done`、`Acceptance=complete`）；`DEP-ACF-MIRROR` 已交接；`DEP-ACF-CAP` 的 ACF 侧必要条件已交接（CAP-04..07 仍受独立 security/privacy RFC `blocked`）；`DEP-ACF-DM06` 正式 handoff 已完成，DM-05 仍 `pending`；main/no bump/no PR/no branch，DEFER-ACF-05 不变。`issues/451.md`（Issue #451 version-aware M2 Episode memory RFC stub，从未评审）已并入 [`plan-20260926.md`](plan-20260926.md) 附录并删除原文件（2026-10-07），不新增任务卡。`issues/468.md` 已更新陈旧引用（`plan-20260819` R30 → `plan-20260926`）并明确边界：其为 ACF/SCAP capture 协调层下游的精炼/契约管道，非第二个 ingest 协调层（2026-10-07）。**[`plan-20260919.md`](plan-20260919.md) 已收口（GCX-02/03/04 全部 `done`/`complete`，2026-10-07）**：GCX-02/03/04 分别经独立发布 **v0.30.32 / v0.30.33 / v0.30.34**（PR #615/#616/#617；`compat_version_surface_sync` 通过；D 组 `base.yml`+CodeQL 全绿，含 `compat-offline-core` 全量 L1+L2+L3；GCX-04 的 `opencode-export-linux` 为 OpenCode Session Capture 开发中已知波动，不阻塞）。

---

## 一、计划一览

状态列取值：`未启动` / `实施中` / `已收口` / `已排期`（设计计划，尚未执行）。

| 计划 | 类别 | 状态 | 一句话进度（卡片状态） |
|---|---|---|---|
| [`plan-20261001-mega-browser-noninteractive.md`](plan-20261001-mega-browser-noninteractive.md) | 横切（Mega2 browser 非交互操作 / 黑盒驱动） | **已收口** | **2026-10-04 21:11:16 UTC**：MN-01..MN-12 全部 `done`/`complete`，v0.30.27 与 `DEP-MN-03` 已交付；PR #609 以原 head `adb9b59` 合并，后续修复基于 main `622bed5` 移至 `fix/opencode-exporter-cancel-ci`；全量本地测试、格式与 workflow lint 通过；PR #612 首轮 run `37230096807` 的 Linux Clippy 检出未使用的 `linux_pidfd_supported`，已删除，run `37231625328` 又发现通用 runner 错误要求 `NSpid=1`；现仅 trusted-bwrap runner 捕获 namespace pidfd，本地 exporter 模块 15/15、Clippy/fmt/actionlint 通过；run `37233480909` 的 sandbox gates 通过，Clippy 与 bridge 编译均遇到 `run_bounded_exporter` `dead_code`，现已限定 wrapper 为 `cfg(test)`；待新 run 验收 Clippy 与 bridge E2E。原实现基线 `faae20c`、Codex R19/R20/R22 与版本信息见计划正文；无 LR 编号|
| [`plan-20260924.md`](plan-20260924.md) | B（Agent Capture 通用架构前置） | **已收口** | 2026-10-03 14:56:26 UTC：ACF-01..20 与 FIX-ACF-01 `done`/`complete`；R91 `VERDICT: PASS`；ACF-09 已交接 `DEP-ACF-MIRROR`／ACF 侧 `DEP-ACF-CAP`／`DEP-ACF-DM06`。CAP-04..07 仍受独立 RFC `blocked`；DM-05 仍 `pending`。 |
| [`plan-20260925.md`](plan-20260925.md) | B（Session Capture 决策中层） | **已收口** | SCAP-02 / SCAP-01 均为 `done`/`complete`（`v0.23.55` / `fa3849e`；D 组 release+CodeQL 全绿；2026-09-28 最终本地门 8241/8241 passed，本轮不 bump 版本） |
| [`plan-20260923.md`](plan-20260923.md) | 横切（CLI 补全） | **实施中（CP-00 审计）** | CP-00 audit handoff delivered **2026-10-07**（`plan-20260923-coverage.tsv`：1742 行 / 0 未分配，377 命令节点 + 1365 参数覆盖；CP-00 `locally-accepted`，`done` 待 CP-06 release 覆盖）；CP-01..20 pending；CP-06 static acceptance 阻塞全部动态工作；无实现/发布认领 |
| [`plan-20260918.md`](plan-20260918.md) | 横切（`add` 命令收口） | **实施中** | OI-01..03 `done/complete`（v0.23.4/5/6）；**OI-04 `in-progress`（v0.23.7）**；OI-05..WT-07 `pending` |
| [`plan-20260919.md`](plan-20260919.md) | 横切（global 配置迁 XDG） | **已完成** | GCX-01 `done/complete`（v0.23.1）；GCX-02/03/04 `done/complete`（2026-10-07 分别独立发布 **v0.30.32 / v0.30.33 / v0.30.34**，PR #615/#616/#617，D 组 `base.yml`+CodeQL 全绿；GCX-04 的 `opencode-export-linux` 为 OpenCode Session Capture 开发中已知波动，不阻塞；本轮按操作者指示未调用 Codex review） |
| [`plan-20260920.md`](plan-20260920.md) | 横切（拆 Code/Publish/Worker） | **已收口** | RC-00..RC-36 全部 `done/complete`；完成判据全勾选；DEFER-RC-04/05/06/07 已关闭；RC-00 缝清单已并入正文附录；原 `issues/ci-baseline-20260920.md` 的 CI 基线稳定性修复内容已并入「附录：CI 基线稳定性汇总修复」 |
| [`plan-20260927.md`](plan-20260927.md) | 横切（六个大命令实现的模块拆分） | **实施中（核心已发布）** | 22 张卡：核心结构拆分已落地（merge→6 模块+output、worktree→doctor/operations、status→output、diff→compare/render、rebase→state、cloud→restore）；FIX-CM-01/04/08 已实现；Windows 跨编译修复；Cloudflare D1 错误上浮修复。发布 **`v0.30.8` 全平台成功**（8/8 release job 绿：Linux amd64/arm64、macOS、Windows、homebrew-tap、install-scripts、verify-formula、stable-manifest）；全量 nextest 8274/8274 绿，clippy all-targets 干净，worktree-fuse feature 编译+`fuse_repair_*` 2/2 绿。live-compat `compat-live-cloud` **21/21 绿**（`fsck_heal` 已修——测试未先 `cloud sync` 致 blob 未进 R2；`cloud_sync_name_conflict` 亦 ok）。仍待：Cloud L3 受保护 CI dispatch（live workflow 已 enable，21/21 已证）、FIX-CM-WT-MOVE 的 DEP-CM-WT-COMPAT patch 兼容证明、22 项 EX 批准与计划级 Claude `VERDICT: PASS`。具体修复动作见 closeout 临时计划。真实 Cloud L3 仅在受保护 release-SHA CI `workflow_dispatch` 执行。 |
| [`plan-20260927-closeout.md`](plan-20260927-closeout.md) | 横切收尾（剩余 gap 跟踪） | **已排期** | 跟踪 4 项剩余：G-1 fsck_heal live 测试补全、G-2 FIX-CM-WT-MOVE 补丁兼容证明、G-3 四个 Cloud FIX 卡基建（LIVE-GATE 接线/Recovery/Repo-Scope/Live-Safety）、G-4 22 项 EX + 计划级 `VERDICT: PASS`。逐项可执行 checklist 已写入该文件；只提交推送，不 bump 版本。 |
| [`plan-20260921.md`](plan-20260921.md) | 横切（GnuPG HOME 密钥导入仓库 vault） | **已完成** | 原 `plan-20260919-gpg-import.md`；R29 双 PASS。**2026-09-24 收口：** 15/15 卡 `done`/`complete`（勾选 238/238）；操作者裁决 decision (ii) 家族面 + 管理面合并发布为 **`v0.23.65`**（run `35993926394` 8/8 success、CDN gate PASS、安装冒烟 PASS）；发布前合并上游 38 提交（`c71bee8`）并过门 55（fmt/clippy + nextest 8054/8054 + CI 附加段）。遗留：本仓 vault 签名未恢复（操作者裁决维持未签名发布）。 |
| [`plan-20260926.md`](plan-20260926.md) | C（MEM-01/02 研发历程记忆 · 确定性投影） | **实施中** | 原 `plan-20260923.md`（2026-09-25 改名为 `plan-20260925.md`；2026-09-24 合并上游时因与上游 Session Capture 计划 `plan-20260925.md` 撞名，再改为 `plan-20260926.md`）。**取代 [`plan-20260819.md`](plan-20260819.md) 承担 MEM-01/02**（使用者 2026-09-23 要求在 `libra code` 拆除后独立重设计，不沿用 R30）。**14 卡**：`DM-00` **`done`/`complete`**（2026-10-07：`## 取材清单` 已追加、#456 远端事实冻结、plan-20260819 退役横幅）；**`DM-01` `done`/`complete`**（2026-10-07：迁移 A `2026092901_memory_core` 交付，REL-DM-01 独立发布至 v0.30.31 / PR #614 / merge `603bad3` / release run `37588423715`；本地 nextest（umask 0022）9549/9549 + D 组 base.yml/CodeQL 全绿；ER-05 评审待记录，发布后置 done/complete）；**`DM-10` `done`/`complete`**（2026-10-07：迁移 B `2026092902_memory_path_search` 交付，REL-DM-01 独立发布至 v0.30.35 / PR #618 / merge `c384bc7` / 版本面补丁 `b51f5c3`；D 组 base.yml/CodeQL + compat-offline-core 全绿；ER-05 评审待记录）；**`DM-02` `done`/`complete`**（2026-10-08：`src/internal/ai/memory/` commit/change 派生实现，REL-DM-01 独立发布至 v0.30.37 / PR #620 / merge `125657b`；D 组 base.yml/CodeQL + compat-offline-core 全绿；含 `parse_repo_oid`（object_format）修复；ER-05 待记录）；**`DM-13` `done`/`complete`**（2026-10-08：commit 源新鲜度/horizon/重建等价实现，REL-DM-01 独立发布至 v0.30.38 / PR #622 / merge `99c4bce`；D 组 base.yml/CodeQL + compat-offline-core 全绿；ER-05 待记录）；+ `DM-03`..`DM-09` + `DM-11`/`DM-12`全计划**无活动 `EX-*` 豁免**。`DM-09` 为唯一收口卡，AC 按 `DM-06` 的 go/no-go 结论分 A/B 两组；`NO-GO` 时 `DM-07`/`DM-08` 经规范性修订移入 `DEFER-DM-12`。发布周期：GO 分支 10 个、降级分支 8 个（R30 为 25 卡 / 约 25 周期）。**R04 范围收窄（使用者决策）：** 「未提交 Agent 工作按代码路径召回」实测不可实现（`agent_checkpoint.tree_oid` 是 traces 树、`ToolCallRecord.paths_written` 无生产者），已移入 `DEFER-DM-11`。**评审（findings 已改为直接记入计划正文的「Codex review log」章节，`*.review/` 目录已删除）：** Codex r01 `FAIL`（21×P1）、r02（18）、r03（9）、r04（13）、r05（8）、r06（11）、r07（12×P1/4×P2），Grok r09 `FAIL`（2×P1）；**Grok r11 字面 `VERDICT: PASS`（`P0=0` / `P1=0`）**。`P0` 全程为 0。正式 handoff 已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）完成；`DM-05` 仍 `pending`，开工日须按 ER-02 重核。2026-10-07：`issues/451.md`（Issue #451 version-aware M2 Episode memory RFC stub）已并入本计划附录并删除原文件，不新增任务卡。 |
| [`plan-20260917.md`](plan-20260917.md) | 横切（cargo-test 进程内剥落） | **已收口** | SH-00..SP-01 五卡全部 `done/complete`；SP-00 结论文档已并入正文附录 |
| [`plan-20260916.md`](plan-20260916.md) | B（Mega agent capture-push） | 未启动 | CAP-01..03 `pending`，仅可 zero-raw-persistence 重核；`DEP-ACF-CAP` 的 ACF 侧必要条件已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；CAP-04..07 `blocked`，须独立 security/privacy RFC 与新卡 |
| [`plan-20260913.md`](plan-20260913.md) | A（LR-09 FastCDC Media） | **实施中** | FL-00/01/07/02/06/03/04/05 全部 `done`/`complete`（发布 `v0.30.0`..`v0.30.5`）；前置 plan-20260907 已收口（`DEP-FL-04` 满足）；仅剩计划级收口门（默认+fastcdc 全量、Codex/Claude 双 review 与文档/网站同步）未勾 |
| [`plan-20260912.md`](plan-20260912.md) | B（memory boundary） | **已收口** | MB-01..MB-05、MB-07/08、MB-10/11 全部 `done/complete`（v0.23.37/.39/.40/.41/.42/.43/.44/.45/.46，各卡 ER-14 全量绿、D-MB-STD 远端 evidence 绿）；MB-06/09/12 墓碑 `done`（无对应子命令）；EX-MB-01/02（G-03）与 EX-MB-03（ER-07 签名）已登记；使用者 2026-09-21 双评审豁免已登记；完成判据全数勾选 |
| [`plan-20260911.md`](plan-20260911.md) | B（pi capture / hook boundary） | 未启动 | PI-01..06 全部 `pending`；`DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；Claude Code 429 未给出 verdict 仍是本计划自身开工门 |
| [`plan-20260910.md`](plan-20260910.md) | 横切（数据库迁移作用域） | **已收口** | MIG-00..MIG-06、MIG-R01..R03 全部 `done/complete` |
| [`plan-20260907.md`](plan-20260907.md) | 横切（BLAKE3 object format） | **已收口** | B3-00..B3-17 全部 `done`/`complete`（`v0.23.68`→`v0.29.0`；REL-B3-01/02 家族经 B3-15/B3-17）；最终 Codex/Claude R40 双 `PASS`（0×P0/0×P1）；GC-B3-01/GC-B3-02 收口守卫绿；`DEP-B3-05` 已满足，下游 FL-00 解锁；无 LR 编号 |
| [`plan-20260906.md`](plan-20260906.md) | 横切（安全扫描） | 未启动 | SC-01..SC-07、SC-CLOSE 全部 `pending` |
| [`plan-20260905.md`](plan-20260905.md) | B（Claude hooks/reasoning） | 未启动 | CC-00..CC-06 全部 `pending`；`DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接 |
| [`plan-20260904.md`](plan-20260904.md) | B（Codex reasoning） | 未启动 | 6 張 RG + 29 張已定義 CX 卡（共 35 卡）全部 `pending`；`DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接。2026-10-08 10:41:48 UTC：FIX-RG-01 `in-progress`/`locally-accepted`——#621 已 squash merge 为 main 913efe3d（12 项检查 SUCCESS），0.30.39 独立发布候选 fmt/strict clippy/version-surface 2/2/release build 通过、R6 full 9530/9530 按 version-only 例外复用，fresh 双审与 push/PR/tag/release/安装待完成；R6 Codex 组合源码 P1 均在上游 memory 模块及其 change store 依赖（F4），登记 `DEP-MEM-R6-HANDOFF` 移交 plan-20260926（本行状态列与基线文字按 append-only 保留；FIX-RG-SSH-01 树内卡态为 in-progress，其 v0.30.36 发布记录随后续计划树入库）。2026-10-08 13:12:40 UTC：**FIX-RG-01 `done`/`complete`（v0.30.39，PR #624 → 9b8fc5c7057e，release run 37777413227 八任务 success，默认 macOS 安装 smoke PASS）**；正式进度 1/61 |
| [`plan-20260903.md`](plan-20260903.md) | A（LR-05 merge） | 实施中（收尾） | MG-01..MG-21 卡片全部 `done/complete`；最终计划收口与 deferred 差异仍待完成 |
| [`plan-20260902.md`](plan-20260902.md) | B（OpenCode artifact／memory） | 未启动 | OG-00..OG-15 全部 `pending`；`DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接。2026-10-03 15:37:28 UTC：捕获插件写操作永久禁止（GC-OG-03）；`DEFER-OG-01` 仍只延期 checkpoint 回灌。2026-10-03 15:47:05 UTC：计划口径对齐模板 v2.12。2026-10-03 15:53:30 UTC：唯一 pin 为 OpenCode 2.0.22；成功门为 Linux 与 macOS 都捕获到内容。Codex R72 `VERDICT: PASS`（2026-10-03 16:54:47 UTC）。2026-10-08 10:41:48 UTC：执行会话切换为 Claude Code（执行者陈述：当前唯一会话）；该计划另有未入库的执行会话工作副本，入库前以本表/卡文为准，随后续发布树同步到本行。2026-10-08 12:49:33 UTC：NPR-19 + FIX-NPR-CONFIG-01 独立发布候选 release/npr-19-v0.30.40（基线 main 9b8fc5c7057e，三版本面 0.30.40）本地门全过、fresh 双审/PR/CI/release/安装/cf 交付待完成，二者 in-progress/locally-accepted |
| [`plan-20260901.md`](plan-20260901.md) | 横切（SB-01 pkt-line fail-closed） | **已收口** | 全部卡 `done/complete`；最新 v0.22.47 |
| [`plan-20260830.md`](plan-20260830.md) | 横切（SB-02 sandbox export） | **已收口** | SBX-01..05 `done/locally-accepted`（发布步按 DEFER-SBX-06 延后）；ER-13 收口门绿 |
| [`plan-20260827.md`](plan-20260827.md) | 横切（SB-04 测试并行度） | **已收口** | NP-00..05 全部 `complete` |
| [`plan-20260825.md`](plan-20260825.md) | B（历史：Code provider / RT-01 后续）+ 横切测试 | **已收口** | **历史完成/封存**：PS 产品轴已随 plan-20260920 拆除，Code 专属 DEFER 已墓碑化；TA 测试并行度交付保持有效，`DEFER-PS-06/07` 仅可作为另立通用计划的候选 |
| [`plan-20260824.md`](plan-20260824.md) | B（历史：RT-01 延后项收口） | **已收口** | **历史完成/封存**：DF-01..09 全部 `done/complete`、v0.22.0 已发布；其 Code 产品面与专属 DEFER 已随 plan-20260920 拆除/墓碑化 |
| [`plan-20260822.md`](plan-20260822.md) | A（LR-02/LR-03 Operation Log v2） | 实施中（PR #503 收口） | OL-01..13、CH-01..04 `done/complete`；OL-14 **已取消**（`web/` 拆除，2026-09-20）；OL-15A `done/complete`，OL-15 `done/remote-pending`（等待 compat-offline-core） |
| [`plan-20260821.md`](plan-20260821.md) | A（UP-01） | **已收口** | 客户端与 CI 全部落地；closeout `00bc815`；DEFER-02..06 残留 |
| [`plan-20260819.md`](plan-20260819.md) | C（MEM-01/02 Memory，R30 历史方案） | **已取代**（2026-10-07，DM-00 正式退役） | R30 r37 于 2026-09-22 对冻结版 sha256 `4fbaf4bd…` 取得 Codex/Claude 同版双 PASS；25 张 M2 卡全部 `pending`、未实施。**MEM-01/02 已由 [`plan-20260926.md`](plan-20260926.md) 承接，M2-01..M2-16E 全部正式退役（DM-00 已登记）**，`src/internal/ai/memory/**` 与 `libra memory` 仅由 plan-20260926 拥有（DEP-DM-05）。R30 评审日志保留为历史事实；旧版 PASS 不代表新计划的评审门通过。 |
| [`plan-20260818.md`](plan-20260818.md) | B（deepseek-harness bridge） | **已收口** | LB-01..07 全部 `done/complete`；protocol v1 20-method 全实现 |
| [`plan-20260729.md`](plan-20260729.md) | A（CT-01） | 实施中（收尾） | CT4-01 发布卡已执行（v0.21.21）；**CT3-07 `blocked`/已延后**；完成判据未全部勾选 |
| [`plan-20260715.md`](plan-20260715.md) | B（历史：RT-01 Code Web-only） | **已收口** | **历史完成/封存**：W0..W6 + W5-01/WIO 完成证据保留；Code 产品面由 plan-20260920 拆除，DEFER-01..08 已关闭/墓碑化，DEFER-09/10 已完成关闭 |
| [`plan-20260714.md`](plan-20260714.md) | A（UP-01、LR-01）+ 横切 | **已收口** | Part A 迁移至 plan-long；Part C W1..W4 勾选；Part D 残留由 LR 承接 |
| [`plan-20260713.md`](plan-20260713.md) | B（LR-06/07/10 捕获前置） | **已收口** | DR-BASELINE..DR-07 全部实现并收口 |
| [`plan-20260708.md`](plan-20260708.md) | A（LR-04/05/09 相邻基础） | **已收口** | 41 项主线全部实现；只保留历史记录，活跃残留另行排期 |

### issues/ 下的计划（Issue 驱动的 Git 对齐修复计划）

`issues/` 目录每份文件对应一个 GitHub Issue，是独立的可执行计划。`474`/`477`/`486`/`497`/`498`/`574`/`577`/`582` 已收口或关闭、`476` 执行中（用户 2026-09-20 覆盖：执行期间不调用 Codex/Claude 评审）。其余多为设计计划。状态列取值同上。

| 计划 | Issue 主题 | 状态 | 任务卡 |
|---|---|---|---|
| [`issues/470.md`](issues/470.md) | 工作树物化丢失可执行位与 mode 变化检测 | 未启动 | FM-01/02/05（3 卡） |
| [`issues/473.md`](issues/473.md) | `init` 与 Git 对齐 | 未启动 | IN-01..IN-12（12 卡） |
| [`issues/474.md`](issues/474.md) | clone 浅克隆完整性、bundle 源、bare 与 mirror 对齐 | **已收口** | CL-01..CL-15 全 `done`/`complete`（`v0.23.47`–`v0.23.54`、`v0.23.57`–`v0.23.63`）；closeout `v0.23.64`（#569）；DEP-CL-07 完成（#474 CLOSED） |
| [`issues/475.md`](issues/475.md) | `config` Git 兼容参数层对齐 | 未启动 | CF-01..CF-15（15 卡） |
| [`issues/476.md`](issues/476.md) | 工作树命令族与 Git 对齐 | **实施中** | WT-02 `v0.23.29` / WT-04 `v0.23.30` / WT-08 `v0.23.31` / WT-09 `v0.23.32` / WT-10 `v0.23.33` / WT-11 `v0.23.34` / WT-01 `v0.23.35`（`done`/`remote-pending`）；WT-03 受 DEP-WT-08 阻塞；intent-to-add 已迁至 plan-20260918 |
| [`issues/477.md`](issues/477.md) | 历史改写命令族与 Git 对齐 | **已收口** | HF-01..HF-31（31 卡）全 `done/complete`，聚合发布 v0.22.49；子 issue #495；2026-10-07：477b 后续（#522/#523/#525/#526/#527/#533/#536，HW-01..HW-07 全部 `pending`，未启动）并入该文件「后续计划：#477b 新缺陷跟进」一节，原 `issues/477b.md` 已删除 |
| [`issues/478.md`](issues/478.md) | log/show/diff/grep/blame/notes/reflog 命令族 | 未启动 | LG-01..LG-25（25 卡） |
| [`issues/479.md`](issues/479.md) | plumbing 与 Git 对齐 | 未启动 | EC-01、RV-01/02、UI-01..03、DF-01、SR-01、UR-01（9 卡） |
| [`issues/480.md`](issues/480.md) | remote/fetch/pull/push/credential/rerere 对齐 | 未启动 | HP-01..HP-17（17 卡；HP-17 为 SSH 传输入口 host-key 校验，issue #560） |
| [`issues/481.md`](issues/481.md) | 维护与杂项命令族对齐 | 未启动 | MX-01..MX-16（16 卡） |
| [`issues/483.md`](issues/483.md) | `count-objects` 与预览命令零对象写入 | 未启动 | CO-01..CO-04（4 卡；CO-03/04 受 DEP-CO-04 / CX-30 `src/cli.rs` 串行约束） |
| [`issues/486.md`](issues/486.md) | upstream ahead/behind 计数 | **已收口** | AB-01 `done/complete`（v0.22.31，已关闭） |
| [`issues/487.md`](issues/487.md) | 本地 Git 转换挂死与中断恢复 | 未启动 | IG-01..IG-05（5 卡） |
| [`issues/488.md`](issues/488.md) | `grep` 的 `--exclude-standard` 与子目录作用域 | 执行中 | GR-01、GR-02 `in-progress`（实现+验证通过，发布阻塞） |
| [`issues/490.md`](issues/490.md) | skip-worktree 索引位与 `add` 稀疏路径诊断 | 未启动 | SW-01..SW-07（7 卡；SW-06 已迁至 plan-20260918） |
| [`issues/496.md`](issues/496.md) | 本地路径 clone 停住（Fetching objects 0% CPU） | **已收口（v0.30.28）** | CLH-01 `done`/`complete`（诊断）、CLH-02 `done`/`complete`（修复 + 回归守卫 + 文档/版本面/发布；收口二维均满足——**②** 全量 **9522/9522 全绿**（上游 #612 修复 `opencode_export` bwrap probe 与 `history::cleanup_helper_guard`）＋ **③** 后续发布 **`v0.30.28`**（release 成功，4 平台）收口 `v0.30.2` 的 ER-14 门——见 `ENV-496-01`/`EX-496-01`）；根因 ADR-CLH-01（`local_client.rs` 旧 `encode_pack_bytes` 先喂满有界输入通道再排空有界输出通道的循环等待）；已复用 `pack_writer::encode_pack_bytes`，新增 pack_writer 单元回归 + clone 集成回归；文档/`COMPATIBILITY.md` 同步，`../libra-backend`（Libra 仓库，`cf` 分支，提交 `905d7a2`）已交付（`DEP-496-03` 已满足）；版本面/CHANGELOG 标为 `0.30.1`（GitHub 实际 tag `v0.30.2`）。Codex review：R1..R14，**R13 `PASS` + R14 最终确认 `PASS`**；**计划 #496 已于 2026-10-05 正式收口（`v0.30.28`）**。 |
| [`issues/497.md`](issues/497.md) | 删除最后一个被跟踪文件后 commit 报 nothing to commit | **已收口** | CD-01..CD-04 全部 `done`/`complete`；`v0.27.2`（PR #581 squash merge `859d7fb`；PR head `base.yml` 7/7 + CodeQL 绿；`release.yml` 8/8 + stable manifest `0.27.2`）；ER-07 签名例外 EX-CD-01（仓库 vault 不可 unseal，操作者 2026-09-24 裁决维持未签名发布） |
| [`issues/498.md`](issues/498.md) | `tag <name> <commit>` 不接受显式目标提交 | **已收口** | TT-05 `done`/`complete`（v0.30.22，PR #602 `a2f1c34`，release `37055840149` 8/8；网站 `cf@a976e4c`）；TT-02 `done`/`complete`（v0.30.23，PR #604 `52bf7d2`，release `37087459978` 8/8；网站 `cf@e908d01`）；M3 收口门 2026-10-03 06:19:24 UTC 全绿（CI 口径：8754/8754 passed / 4 skipped）；完成判据全勾选；`issues/478.md` DEFER-04 已登记由 TT-02 交付；#498 CLOSED；残留 DEFER-TT-01..08（含 DEFER-TT-08 `for-each-ref`/`describe` 嵌套 peel）；TT-01/03/04 废弃，TT-06/07 合并回 TT-02；EX-TT-01 已批准 |
| [`issues/468.md`](issues/468.md) | Data collection and refinement | 未启动 | DC-01..DC-04（4 卡）；2026-10-07：更新陈旧引用（`plan-20260819` R30 → `plan-20260926`）并明确为 ACF/SCAP capture 协调层下游的精炼/契约管道（非第二个 ingest 协调层） |
| [`issues/500.md`](issues/500.md) | Feature：Centralized Storage for Libra Statistics Data | **已收口（NO-GO，v0.30.29）** | **2026-10-05 20:10:35 UTC**：本地 trunk compose 部署实测资格——CS-16/19/17 均为 `not-qualified`（storage-only read 面无 ACL：anonymous set/manifest/resolve/object GET/HEAD 读为 200；缺少独立 cross-repo/team principal，identity-specific probes 明确 `not-run`；且 pin `cead1d8` 无 committed-set revoke 路由，`DELETE /sets/{id}` 405）→ CS-20 aggregate=`not-qualified` → CS-00 判定 **NO-GO**；维护者 2026-09-21 评论只确认 Mega2 为后端，未点名 `agent_session`（DEP-500-02 数据集核准未满足，已发征询；GitHub issue #500 保持 OPEN，不把计划 NO-GO 收口等同关闭 issue）。CS-10/11/12/13 延入 DEFER-CS-02/03/07/10（重启条件：维护者在 #500 确证数据集且新 CS-00b 为 GO）；CS-18 登记 defer+outgoing handoff（DEP-500-14→genedna）；CS-14 closeout 收口；**2026-10-05 收口 patch 发布：`v0.30.29`**（PR #613 squash merge `cbded577`；tag `v0.30.29` 指向 release commit `370dc94` 且是 main 祖先；版本面/`Cargo.lock` 一致；恢复 `tests/compat-ledger/SURFACES.gen` 模式为 100755；本地 `cargo +nightly fmt --all --check`、clippy 与 `nextest --all --no-fail-fast --retries 2`（umask 0022）通过，9525/9525 passed（1 flaky 重试转绿）；D-1 base.yml PR #613 head `ade89a1`，run `37344599122` attempt 2 全绿（首次 attempt 的 OpenCode bridge marker 断言失败，retry 通过）；D-2a CodeQL PR run `37344599037` 全绿；D-3 release run `37336809254` 8/8 全绿；D-2b merge-to-main CodeQL run `37354926375` 全绿（merge SHA `cbded577`，actions/Rust jobs 均 success）。**计划卡状态：CS-00/14/15/16/17/18/19/20 `done` / `complete`；CS-10/11/12/13 正式 `deferred` / `not-applicable`。 |CS-00 AC/VER=17/20、14/20；CS-10/11/12/13=17/39/32/41@EX-01（CS-13 raw checklist 42，含 ER-06a）；CS-14=19/20、14/20 audit；CS-15=11/12、11/12 release；CS-16 audit AC/VER=20/20、20/20；CS-19 audit AC/VER=16/20、16/20；CS-17 audit AC/VER=24/20@EX-02、24/20@EX-02 audit（P01=not-qualified、P02=pass、P03=pass、P04=pass、P05=not-run、P06=not-run、P07=pass、P08=pass、P09=pass、P10=pass、P11=pass、P12=fail、P13=not-run、P14=not-run、P15=not-run、P16=not-run、P17=not-run、P18=not-qualified（pre-revoke control pass，post-revoke not-run）、P19=fail、P20=not-run、P21=not-run、P22=not-run、P23=not-run（pre-revoke control pass，post-revoke not-run）、P24=not-qualified；EX-02 清单豁免）；CS-20 audit AC/VER=17/20、17/20；CS-18 audit AC/VER=17/20、17/20 audit。 |
| [`issues/574.md`](issues/574.md) | 只讀查詢走寫操作邊界（`branch -l` 25s）與 log／rev-list 全量遍歷 | **已收口（BRL-01..05 done/complete；DEFER-BRL-01..03）** | BRL-01/BRL-02 `done`/`complete`（v0.30.9／v0.30.10，PR #587 `22b9534`，release 36843865958 8/8）；BRL-03 `done`/`complete`（v0.30.11，PR #588 `a4121a8`，release 36866068892 8/8；网站 cf@961ea3f）；BRL-04 `done`/`complete`（v0.30.12，PR #589 `f14841c`，release 36879702000 8/8；网站 cf@83d620c）；BRL-05 `done`/`complete`（v0.30.13，PR #590 squash `ce2411d`，release 36910331385 8/8；量测 AC ~15.24/15.68/14.96s under `LIBRA_READ_POLICY=local`）；完成判據全勾選；DEFER-BRL-03 executable-bit 基线已红（branch 查詢分類 → tag/remote/reflog/notes 查詢分類 → 快照 stat 短路；log 日期優先 walker → rev-list 下推）；R1-R3 雙評審 FAIL；R4 Claude FAIL、Codex 中斷；R5 兩者 FAIL；R6 Codex FAIL＋Claude PASS；R7 Claude FAIL、Codex 中斷（模型不再支援）；R8 兩者 PASS；R9 兩者 PASS；R10 Codex FAIL（golden 快照須依 v2.12 內聯）＋Claude PASS；R11 兩者 PASS（模板升 v2.12 後合規確認）；R12 兩者 PASS（結案確認）；EX-BRL-01 覆蓋 BRL-01..05（G-03 門族豁免，一門＝一完整具名測試函式＋`-E 'test(=command::…)'` 全枚舉＋零匹配失敗保護＋可執行組內數量比較，分子 33/55/28/10/18；118 個 `command_test` 門皆有 `command::` 前綴）；`BRL-02 -> BRL-03` 跨鏈串行邊（G-10）；census `--lib` 矩陣守衛；`persist_files` 持久化短路；ADR-BRL-02/04 語義修訂；`RevListArgs` 全量歸屬表；tag `no_column` 漏檢同源修正；輸出等價以 inline 期望值對拍（依 v2.12 不另存證據檔）；網站八頁與 `_compatibility.md`／integration scenarios 補齊（評審證據內聯於計劃文件）；repair 鎖殘留經 GC-01 判定已緩解（DEFER-BRL-01）；網站文件受 DEP-BRL-02（`../libra-backend` 不在本 checkout 旁）阻塞發布 |
| [`issues/577.md`](issues/577.md) | SSH 公钥认证失败误报 pkt-line 协议错误与配置文档 | **已收口** | 2026-09-27：SA-02/SA-01 均为 `done` / `complete`；PR #578 合并为 `5eb833f9`，v0.24.1 发布与 8/8 release jobs 全绿；网站 `cf@667d7da8`、Worker `c7011920` 和七个生产页验证完成；Issue #577 CLOSED |
| [`issues/582.md`](issues/582.md) | Git 相容的互動式 SSH 主機金鑰確認 | **已收口** | HKT-00 `done/complete`（no-release 設計與 HP-17／DEFER-07 移交）；HKT-01 `done/complete`（首次 clone 的 host-key policy cascade）；HKT-02 `done/complete`（受限 human-terminal unknown-host confirmation）；聚合发布 `v0.29.1`；全量 nextest 8271/8271 绿

---

## 二、未启动的计划与卡

以下计划中，`plan-20260921` 已于 2026-09-24 完成并发布为 `v0.23.65`（15/15 卡 `done`/`complete`，见下表本行注）；其余计划均未进入实施/发布阶段（多数为设计态、全部 `pending`；`plan-20260919` 已全部 `done`/`complete`，GCX-01..04 含 GCX-02/03/04 的 v0.30.32/33/34 交付见下表本行注）。按建议优先级排列，优先级依据为跨计划依赖（`DEP-*`）与产品路线（`plan-long.md`）：

| 计划 | 全部待执行卡 | 开工前置条件 |
|---|---|---|
| [`plan-20260924.md`](plan-20260924.md) | ACF-01..20 与 FIX-ACF-01 `done`/`complete` | **已收口**（ACF-09 `Lifecycle=done`、`Acceptance=complete`）；`DEP-ACF-MIRROR` 已交接 |
| [`plan-20260926.md`](plan-20260926.md) | DM-00..DM-13（14 卡；`DM-00` `done`/`complete`，`DM-01` `done`/`complete`，`DM-10` `done`/`complete`，`DM-02` `done`/`complete`，其余 pending） | R11 字面 `VERDICT: PASS`（`P0=0` / `P1=0`）。`DM-00` 已于 2026-10-07 `done`/`complete`（#456 取材清单、远端事实冻结、plan-20260819 退役）。`DM-01` 已于 2026-10-07 `done`/`complete`（迁移 A `2026092901_memory_core` 交付，REL-DM-01 独立发布至 v0.30.31 / PR #614 / merge `603bad3` / release run `37588423715`；本地 nextest（umask 0022）9549/9549 + D 组 base.yml/CodeQL 全绿；ER-05 评审待记录）。`DM-10` 已于 2026-10-07 `done`/`complete`（迁移 B `2026092902_memory_path_search` 交付，REL-DM-01 独立发布至 v0.30.35 / PR #618 / merge `c384bc7` / 版本面补丁 `b51f5c3`；D 组 base.yml/CodeQL + compat-offline-core 全绿；ER-05 评审待记录）。`DM-02` 已于 2026-10-08 `done`/`complete`（`src/internal/ai/memory/` commit/change 派生实现，REL-DM-01 独立发布至 v0.30.37 / PR #620 / merge `125657b`；D 组 base.yml/CodeQL + compat-offline-core 全绿；含 `parse_repo_oid`（object_format）修复；ER-05 待记录）。`DEP-DM-06` 正式 handoff 已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）完成；DM-05 仍 `pending` 并须在开工日按 ER-02 重核（经 DEP-DM-06） |
| [`plan-20260902.md`](plan-20260902.md) | OG-00..OG-15 | `DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；OG-00/04 以各自 review gate 为准。捕获插件写操作按 GC-OG-03 永久禁止；`DEFER-OG-01` 仍只延期 checkpoint 回灌。计划口径已对齐模板 v2.12（2026-10-03 15:47:05 UTC）。唯一 pin 为 OpenCode 2.0.22；成功门为 Linux 与 macOS 都捕获到内容（2026-10-03 15:53:30 UTC）。Codex R72 `VERDICT: PASS`（2026-10-03 16:54:47 UTC）
| [`plan-20260904.md`](plan-20260904.md) | 6 張 RG + 29 張已定義 CX 卡（共 35 卡） | `DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；CX-00/12 以原 Phase 0 gate 为准；CX-30 另受 DEP-CLI-mirror |
| [`plan-20260905.md`](plan-20260905.md) | CC-00..CC-06 | `DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；CC-00 以原 review gate 为准，reasoning 卡保留 RG 前置 |
| [`plan-20260923.md`](plan-20260923.md) | CP-00 (audit handoff delivered)..CP-20 pending; expand bounded domain cards after inventory if needed | CP-00 audit handoff delivered 2026-10-07（`plan-20260923-coverage.tsv`，0 未分配）；CP-01 可消费 CP-00 审计产物；计划级 review PASS 及 DEP-CP-01..07 仍待满足；动态实现要求 CP-06 done/complete |
| [`plan-20260906.md`](plan-20260906.md) | SC-01..SC-07、SC-CLOSE | — |
| [`plan-20260907.md`](plan-20260907.md) | B3-00..B3-17 | **已收口**（B3-00..B3-17 全部 `done`/`complete`；`DEP-B3-05` 已满足，不再阻塞 plan-20260913） |
| [`plan-20260911.md`](plan-20260911.md) | PI-01..PI-06 | `DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；只读 pin/source 重核可先行；Claude Code 429 未出 verdict 仍是本计划自身开工门 |
| [`plan-20260912.md`](plan-20260912.md) | MB-01..05、MB-07/08/10/11（MB-06/09/12 已取消） | 依赖 CAP（plan-20260916）等 |
| [`plan-20260913.md`](plan-20260913.md) | FL-00..FL-07 | **前置 plan-20260907 已完整收口（DEP-FL-04 满足）**；FL-00/01/07/02/06/03/04/05 已全部 `done`/`complete`，仅剩计划级收口门 |
| [`plan-20260916.md`](plan-20260916.md) | CAP-01..CAP-07 | 双评审已 PASS；CAP-01..03 仅按 ER-CAP-02 pin 作 zero-raw-persistence 重核；`DEP-ACF-CAP` 的 ACF 侧必要条件已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；CAP-04..07 `blocked`，须独立 security/privacy RFC 与新卡 |
| [`plan-20260919.md`](plan-20260919.md) | GCX-02/03/04 | GCX-01 已 `done`（v0.23.1）；GCX-02/03/04 均 `done/complete`（2026-10-07 分别发布 v0.30.32 / v0.30.33 / v0.30.34）；写集与 plan-20260918 串行（DEP-GCX-02） |
| [`plan-20260921.md`](plan-20260921.md) | VG-00..VG-14（15 卡） | **已完成（2026-09-24）**：15/15 卡 `done`/`complete`；`v0.23.65` 已发布（decision (ii) 合并发布；REL-VG-01 证据齐备）；证据口径按操作者裁决以替代机制满足双字面 PASS 与 supervisor capture；DEP-CP-04 所述 REL-VG-01 文件保留窗口随发布结束 |
| `issues/` 设计计划 | 见「计划一览」issues 表 | 各计划 Codex review `PASS` 前不得开工；`issues/476`/`479`/`483`/`490` 与 `plan-20260918` 写集串行（DEP-WT-09 / DEP-PL-04 / DEP-AD-06 / DEP-AD-07） |

---

## 三、实施中的计划、待退役的历史方案与当前卡

按执行窗口排序；当前执行与并行工作树见「四、当前执行指针」。

### plan-20261001-mega-browser-noninteractive（mega2 browser 非交互操作，12 卡全串行）

发布窗口顺序 MN-10 → MN-01 → MN-02 → MN-03 → MN-11 → MN-04 → MN-08 → MN-12 → MN-05 → MN-06 → MN-09 → MN-07；单一发布者为 Codex goal 线程 `01a1015b`（ER-12；移交见计划修订历史）。开工前置：`DEP-MN-02`（`../libra-backend` 为 Git 且在 `cf`，MN-01 起）；MN-03 起按 GC-MN-08 重核 `DEP-MN-01`（mega2@`8ff880c`）；MN-03 另需 `DEP-MN-06`；MN-07 另需 `DEP-MN-04`。

| 卡 | Lifecycle / Acceptance | 版本 / 证据 |
|---|---|---|
| MN-10 | `done` / `complete` | v0.30.14；PR #593 squash `80f58b4`；release `36959675389` 8/8、CodeQL main `36959660259` 绿、下载 200；2026-10-02 04:00:07 UTC |
| MN-01 | `done` / `complete` | v0.30.15；PR #595 squash `6e86578`；release `36968922078` 8/8、CodeQL main `36968913909` 绿、下载 200、网站标记计数 3（`cf@973f650`）；2026-10-02 06:11:28 UTC |
| MN-02 | `done` / `complete` | v0.30.16；PR #596 squash `36c05be`；release `36978760075` 8/8、CodeQL main `36978751927` 绿、下载 200、网站标记计数 4（`cf@23c21fc`）；2026-10-02 08:00:39 UTC |
| MN-03 | `done` / `complete` | v0.30.17；PR #597 squash `548a33c`；release `36987596125` 8/8、CodeQL main `36987587577` 绿、下载 200、网站标记计数 5（`cf@a31ced6`）；2026-10-02 09:36:00 UTC |
| MN-11 | `done` / `complete` | v0.30.18；PR #598 squash `6230874`（首轮 offline-core 败于 runner 基础设施，重跑后 12/12）；release `37002565759` 8/8、CodeQL main `37002556477` 绿、下载 200、网站标记计数 3（`cf@42def02`）；2026-10-02 12:16:22 UTC |
| MN-04 | `done` / `complete` | v0.30.19；PR #599 squash `81d1655`；release `37011384218` 8/8、CodeQL main `37011373062` 绿、下载 200、网站标记计数 5（`cf@1f7aa03`）；2026-10-02 13:37:24 UTC |
| MN-08 | `done` / `complete` | v0.30.20；PR #600 squash `36e826a`（首轮 network-remotes 败于外网抖动，重跑后 12/12）；release `37024751607` 8/8、CodeQL main `37024735662` 绿、下载 200、网站标记计数 5（`cf@cea2d47`）；2026-10-02 15:40:05 UTC |
| MN-12 | `done` / `complete` | v0.30.21；PR #601 squash `fb1cf80`；release `37035218493` 8/8、CodeQL main `37035206595` 绿、下载 200、网站标记计数 5（`cf@8f0e356`）；2026-10-02 17:09:37 UTC |
| MN-05 | `done` / `complete` | v0.30.24；PR #603 squash `9507623`（v0.30.22、v0.30.23 先后被并发发布占用，两次重建：`a4d05cb` → `98f1e84` → `4116c7f`）；release `37094701957` 8/8、CodeQL main `37094694927` 绿、下载 200、网站标记计数 11（`cf@57b7d12`）；2026-10-03 04:18:34 UTC |
| MN-06 | `done` / `complete` | v0.30.25；PR #605 squash `cdd32b7`；release `37099685600` 8/8、CodeQL main `37099680422` 绿、下载 200、网站 `--create-tag` 计数 7（`cf@57b7d12`）；2026-10-03 10:57:41 UTC |
| MN-09 | `done` / `complete` | v0.30.26；PR #606 squash `fc85e6e`；release `37104652942` 8/8、CodeQL main `37104701999` 绿、下载 200、网站 `--delete-tag` 计数 5（`cf@a56bce3`）、stable manifest 0.30.26；2026-10-03 10:57:41 UTC |
| MN-07 | `done` / `complete` | v0.30.27；PR #608 squash `faae20c`；release `37123533785` 8/8、CodeQL main `37123437271` 2/2、下载 200、stable 0.30.27；pinned-source live 12/12，`DEP-MN-03` 已交付；2026-10-03 13:05:47 UTC |

### issues/577（SSH 公钥认证拒绝诊断与设置指南）

SA-02 与 SA-01 均为 `done` / `complete`。Claude I11 第二次重基最终候选审查取得
字面 `VERDICT: PASS`（P0–P3=0）；最终 fmt、Clippy、Nextest 8208/8208、release
build 与隔离 locked install 全绿。候选 `ea89d047` 经 PR #578 的 12/12 检查后
squash merge 为 `5eb833f9`；v0.24.1 标签指向该提交，release workflow
`36296462821` 的 8/8 jobs 全绿，四平台 CDN 摘要、签名稳定清单、公开安装和上一版
固定产物均已验证。网站签名提交为 `cf@667d7da8`，活动 Worker 为 `c7011920`，
SSH 指南与六命令生产页均为 HTTP 200。Issue #577 已关闭；完整证据见
[`issues/577.md`](issues/577.md) 的「实施证据汇总」小节（原 `evidence/issues-577/` 目录已并入）。

### plan-20260927（六命令模块拆分，22 个独立切片）

2026-09-25 的干净 `main` 基线上，FUSE repair 两项测试因旧版 Operation 审计查询而 2/2 失败，故按 ER-10 新增独立 FIX-CM-04。同日 CM-01 的全量 Nextest 8115 项中通过 8114 项，`op_undo_redo::doctor_recovers_a_running_publish_journal_once` 三次均失败：物理 `a.txt` 留在 `two\n` 而非预期的 `one\n`。同一二进制从 linked CWD 失败、从 main 或 `/tmp` 通过。FIX-CM-01 只修 pinned scope 并建确定性红绿回归；Operation doctor 误报、中途回滚、undo/redo/revert 入口，以及 `switch` branch-attach 与 repo-wide refs restore 跨工作树锁序竞态，均属未修的独立 P1，由仓库维护者/单一发布者以 `DEFER-CM-OP-01` 另立高优先级安全计划或 issue。若本计划任一卡的 focused/C/D 或最终全量因此变红，须按 ER-10 立依赖 FIX 或先完成独立安全修复，再重跑至绿；DEFER 不能豁免红灯。FIX-CM-01 发布后 CM-01 重基并重跑 C/D，其余卡方可进入 C/T-5；FIX-CM-04 `done/complete` 且本修订计划取得新一轮 Claude Code 字面 `VERDICT: PASS` 后，CM-04 才可解除阻塞。R6/R9 PASS 只证明各自历史修订版；R10/R11/R12/R14 的 Codex/Claude 双 FAIL 均不放行，22 项 EX 未获批准；R15 Claude FAIL；R16 Claude FAIL（14 项 P1）；R17 修订稿待新 SHA 双评审；尚无本计划卡完成 D 组发布。

| 卡 | 本地状态 | 发布 |
|---|---|---|
| FIX-CM-01 Operation restore pinned scope | Lifecycle `done`（本地验证）/ Acceptance ；（2026-09 期间复核：`src/internal/head.rs`、`src/internal/operation/restore.rs` 已含 scoped 变体与 `restore_pinned_head_callsite_map_guard`；`tests/op_undo_redo.rs` 含 parent/child 回归）。验证：`op_undo_redo` 15/15 全绿，`doctor_recovers_a_running_publish_journal_once`、`doctor_recovery_uses_pinned_main_scope_from_linked_cwd`、`doctor_recovery_linked_cwd_child` 均 PASS；head/restore lib 33/33；三个 head guard 1/1 全绿。`tests/INDEX.md` 已更新 relevant-src。发布未执行 | 未发布 |
| FIX-CM-08 diff 生产 panic 守卫 | Lifecycle `done`（本地验证）；已在 `Cargo.toml`/`tests/INDEX.md`/`tests/compat/README.md` 注册 `compat_diff_production_expect_guard`，TARGET_FILES 覆盖 `src/command/diff.rs` + `diff/{compare,render,options}.rs`（移除不存在的 `blob_similarity.rs`）；守卫 1/1 绿，CM-08 diff 源码已移动 | 未发布 |
| FIX-CM-LIVE-GATE Cloud live 无跳过门 | Lifecycle `pending` / Acceptance 空；待 FIX-CM-01 与 CM-13 独立发布，提供无跳过脚本与纯本地 preflight；C 组按模板标准双 env/default-feature 全量，测试体 feature-off 不得静默跳过；本卡不使用真实 Cloud 资源 | 未发布 |
| FIX-CM-CLOUD-RECOVERY-AUTH 来源认证 | Lifecycle `pending` / Acceptance 空；`AC=35/8@EX-CM-REC-AUTH-G03`、`VER=35/8`；先独立发布受保护 workflow、签名来源 verifier 和零删除探针，D 不需要真实来源 artifact；EX 未批准 | 未发布 |
| FIX-CM-CLOUD-RECOVERY-CLEANUP 受限恢复 | Lifecycle `pending` / Acceptance 空；`AC=16/8@EX-CM-REC-CLEANUP-G03`、`VER=16/8`；依赖 AUTH 发布，补充精确 D1/R2 范围删除与同签名范围幂等重试；本卡 D 仍零真实删除；EX 未批准 | 未发布 |
| FIX-CM-CLOUD-REPO-SCOPE 云端仓库作用域 | Lifecycle `pending` / Acceptance 空；`AC=51/8@EX-CM-CLOUD-REPO-SCOPE`、`VER=40/8`；依赖 CLEANUP 发布，以本地假端点验证 CLI/D1 ensure/R2 sink 拒绝未登记 repo/slot 和越界写；EX 未批准 | 未发布 |
| FIX-CM-CLOUD-LIVE-SAFETY Cloud L3 写前准入 | Lifecycle `pending` / Acceptance 空；独立 `implementation`/`compensating` patch，`AC=39/8@EX-CM-CLOUD-SAFETY`、`VER=27/8@EX-CM-CLOUD-SAFETY`，EX 待新冻结 SHA 具名审批；依赖 FIX-CM-01、CM-03、CM-13、LIVE-GATE、RECOVERY-AUTH/CLEANUP、REPO-SCOPE 与 DEP-CM-05.vars-ready/recovery-ready。交付 CI-only 身份/只读探针、D1 全库加密备份与读回、全局及逐例预分配 manifest、失败关闭；自身 release-SHA 真实 D 与 `E-CM-L3-SAFETY` 必须通过且发布后才放行 CM-10。现 `premerge/postrelease=unknown`，未作真实 L3 写入 | 未发布 |
| CM-01 merge 状态与 autostash | Lifecycle `blocked` / Acceptance 空；R6 本地 A/B 与独立 Codex review `VERDICT: PASS`（`/root/cm01_review`）仅为历史预备，R11 新 sidecar/owner guard 尚未落地；首次 C 组 Nextest 8114/8115，`op_undo_redo::doctor_recovers_a_running_publish_journal_once` 三次均失败。待 FIX-CM-01 独立发布、新版计划 PASS 与 EX-CM-01 获批后重基，完成新 A/B，再于已 bump 树重跑 C/D | 未发布 |
| FIX-CM-04 FUSE repair v2 审计测试契约 | Lifecycle `done`（本地验证）；修正 `worktree_fuse_test.rs` 的 `repair_audit_rows` 查询 `command_name='worktree'` 与 `success`/`failed` 状态（v2 CLI 不再记 `worktree repair`/`succeeded`）；`worktree-fuse` feature 编译绿、`fuse_repair_*` 2/2 通过；并按 ADR-CM-01 为 `worktree.rs` 的 `mod doctor/operations` 增加显式 `#[path]` 声明 | 未发布 |
| CM-04 worktree registry | Lifecycle `blocked` / Acceptance 空；`AC=29/8@EX-CM-04`、`VER=18/8@EX-CM-04` 待具名批准；依赖 FIX-CM-04 与 CM-12；隔离本地试作默认 focused 291/291、FUSE lib 12/12，通过；FUSE command 8/10，两项失败已在干净 main 复现。预备证据不计验收，须等 FIX-CM-04 `done/complete` 及修订计划 Claude PASS 后重基重验 | 未发布 |
| FIX-CM-WT-MOVE 跨设备 move 数据保全 | Lifecycle `blocked` / Acceptance 空；`AC=57/8@EX-CM-WT-MOVE`、`VER=56/8`；依赖 CM-04 与 DEP-CM-WT-COMPAT。当前 patch 兼容的旧二进制和已打开 writer 排除证明不足，不能进行 C/merge/release；若需 breaking/minor，先提交具体兼容方案请用户决定。EX 未批准 | 未发布 |
| CM-06 status 输入、扫描与缓存 | Lifecycle `in-progress` / Acceptance 空；旧本地 A/B 与 `dirty_test` 14/14 仅为隔离预备，R11 新 owner/warning-order guard 尚未落地。待共享 INDEX 前驱发布、新版计划 PASS 与 EX-CM-06 获批后重基并重新执行正式 A/B；C/D 还须等 FIX-CM-01 发布 | 未发布 |
| CM-10 cloud sync、metadata 与 Agent catalog | Lifecycle `in-progress` / Acceptance 空；本地 cloud 命令 9/9、源内 52/52、mock/error 7/7、`compat_serial_registry` 25/25 为隔离预备。待 CM-03、CM-13、FIX-CM-CLOUD-LIVE-SAFETY 独立发布后重基；DEP-CM-05.premerge 的 vars owner/readback 已通过，但本地/mock 首写负例未证；postrelease 本卡 release-SHA 受保护 CI dispatch、`E-CM-L3-10` 身份/备份/manifest/善后与两个完整 live target 未证，阻 D/complete。本地仅 fake/mock/default C，不执行真实 Cloud 写 | 未发布 |
| CM-13 worktree repair/doctor 结构拆分 | Lifecycle `in-progress`（本地实现+验证中）；扩张 `src/command/worktree/doctor.rs`（确认/预览/scope 诊断/legacy 捕获接纳/migration 恢复/repair 驱动），保留 `doctor` 专有输出与公共 `pub(crate)` 路径；`worktree.rs` 由 7,439 降至约 4,000 行；focused 验证/全量待跑；`AC=34/8@EX-CM-13`、`VER=24/8@EX-CM-13` 待具名批准；Operation 中途回滚行为缺陷不在本卡修复范围，既有 doctor approved-project 确认写入的分类/锁/审计缺口记 `DEFER-CM-WT-01`，未被本卡修复 | 未发布 |
| CM-07 status 输出格式 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/status/output.rs`（human/JSON/porcelain/短格式渲染），status `command_test` 261/261 全绿；`StatusData` 保留在 facade；网站 `status.en.md` 交付与 DEP-CM-04 的 `cf` 远端证据待办；正式 C/D 待全量 | 未发布 |
| CM-11 cloud restore | Lifecycle `in-progress`（本地实现）；扩张 `src/command/cloud/restore.rs`（对象/metadata/Agent-catalog 恢复），`cloud.rs` 由 5,771 降至约 4,000 行；clippy 全绿；真实 L3 `E-CM-L3-11` 的受保护 CI `workflow_dispatch` 与两个完整 live target 属 postrelease D/complete 门，本地只做 fake/mock/default C | 未发布 |
| CM-05 worktree 普通操作所有权 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/worktree/operations.rs`（add/reattach/list/lock/unlock/move/prune/remove/umount + lifecycle/journal），worktree `command_test` 185/185 全绿；`worktree.rs` 由 7,439 降至约 1,700 行；全量 nextest 8273/8273 绿 | 未发布 |
| CM-02 merge 树裁决 | Lifecycle `in-progress`（本地实现+验证）；已拆出 `merge/{tree_merge,virtual_base,workdir,content,rename_merge,conflict}.rs` 六个职责模块；`merge.rs` 由 19,277 降至约 14,060 行；clippy 全绿；merge `command_test` 全量待跑（serde skip 属性已恢复）；`AC=39/8@EX-CM-02`、`VER=24/8@EX-CM-02` 待具名批准 | 未发布 |
| CM-03 merge facade/输出 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/merge/output.rs`（CLI 输出渲染/错误映射/merge message/签名），merge `command_test` 334/334 全绿；`merge.rs` 降至约 13,900 行（含内联测试）；clippy 全绿 | 未发布 |
| CM-08 diff 比较管线 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/diff/compare.rs`（revision/scan/algorithm、side 水合、pathspec/rename 检测），diff `command_test` 77/77 全绿；`diff.rs` 由 8,166 降至约 6,200 行；clippy 全绿 | 未发布 |
| CM-09 diff 渲染 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/diff/render.rs`（word/patch/stat 渲染、summary/stat/raw/unified、colorize），diff `command_test` 77/77 全绿；compare/算法部分仍留在入口（CM-08 未做）；正式 C/D 待全量 | 未发布 |
| CM-12 rebase 状态/replay | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/rebase/state.rs`（状态/autostash 侧车、scope-aware ref 更新与 GC roots），rebase `command_test` 86/86 全绿；interactive/replay 仍留在入口；正式 C/D 待全量 | 未发布 |

各卡发布仍按 ER-12 由单一发布者串行执行；FIX-CM-01 → CM-01，FIX-CM-04/CM-12 → CM-04 → CM-05 → CM-13，CM-07 → FIX-CM-08 → CM-08，CM-03/CM-13 → FIX-CM-LIVE-GATE → FIX-CM-CLOUD-RECOVERY-AUTH → FIX-CM-CLOUD-RECOVERY-CLEANUP → FIX-CM-CLOUD-REPO-SCOPE → FIX-CM-CLOUD-LIVE-SAFETY → CM-10 → CM-11。除 FIX-CM-01 自身外所有卡的 C/T-5 都等其独立发布 `done/complete`，须从关联工作树 cwd 实跑。`tests/INDEX.md` 全写者发布链为 FIX-CM-01 → CM-01 → CM-06 → CM-02 → CM-03 → CM-07 → FIX-CM-08 → CM-08 → CM-09 → CM-12 → CM-04 → CM-05 → CM-13 → FIX-CM-LIVE-GATE → FIX-CM-CLOUD-RECOVERY-AUTH → FIX-CM-CLOUD-RECOVERY-CLEANUP → FIX-CM-CLOUD-REPO-SCOPE → FIX-CM-CLOUD-LIVE-SAFETY → CM-10 → CM-11；共享 `tests/compat/serial_registry.rs` 另有 CM-03 → CM-10。CM-03 同卡把 INDEX 的 serial 守卫行改为无固定计数。CM-07 的 DEP-CM-04 仅核网站 `cf` 开工基线，实际 `status.en.md` 交付由本卡完成；已推送后失败走条件化 `compensating`，远端/部署不确定为 `remote-pending`。`CM-Publisher` 为本轮主 Agent（ID `/root`），负责状态、旧→新符号路径映射和发布；可委托文档代理编辑，但由发布者精确暂存与提交。`plan-status.md` 是 G-10 协调写集，允许随对应卡发布写集精确暂存，不使用 `commit -a`。Cloud 真实 L3 仅在受保护 release-SHA CI `workflow_dispatch` 写 job：先由安全卡交付 fail-closed 协议并产生独立 `E-CM-L3-SAFETY`，再由 CM-10/11 各自产生 `E-CM-L3-10/11`；本地只跑 fake/mock/default C，不凭本地 env 发真实写请求。

**规则与延期门：** 本版 22 项 `EX-CM-*` G-03 数量例外在 R14 双 FAIL、R15 Claude FAIL 后均未获批准，必须分别取得新冻结 SHA 的具名 Codex reviewer 对准确分子和证据的书面同意，以及本修订版 Claude Code 字面 `VERDICT: PASS`，才对对应卡生效；此前新卡不得开工。`DEFER-CM-OP-01` 是未修的独立 P1；CM-13 之外的 `worktree doctor --adopt/--clear-approved-project` 写入被归为 ReadOnly 且无审计/写锁，另记 `DEFER-CM-WT-01`，由仓库维护者/单一发布者立行为 FIX 或安全 issue；两者均非本计划已验收能力。`DEFER-CM-CLOUD-STATUS` 仅在既有本地 status 职责增长、cloud root 再超预算或修改反复跨 sync/restore 时重做职责审计并另立卡。

| Cloud L3 证据 ID | 已确认的共享只读背景 | 本卡强制门与状态 |
|---|---|---|
| `E-CM-L3-SAFETY`（FIX-CM-CLOUD-LIVE-SAFETY） | 用户已确认冻结 `.env.live-test` SHA256 `c8409dbd59232c9e17c970adc029564ca7d112f17a8ae25c2f56a44b5d768b6d` 的专用精确期望：D1 account `3a6c7e7927dbd254d3c78c1fc6222441`、`libra-testing` database UUID `8e067bd6-f12c-4462-a536-65f8acde59ce`，R2 同 account、bucket `libra-action`；用户最新澄清同一 account 的其它 D1 库属于其它项目，账号级 D1 建库/删库与远端整库导入均禁止。06:57 UTC 本地只读 D1 GET 200/UUID 匹配、R2 有界 ListObjectsV2 200/Name 匹配；R2 管理 API 403。后续 D1 只读 schema 清点为 25 用户表、1 SQLite 内部表、1 view、25 triggers（9 源码匹配、16 remote-existing/unmatched）、0 virtual table；16 条不得盲判安全。四个独立 repo vars 于 08:35:02 UTC 全部设置，API readback exact 4/4；live workflow id `283278914` 自 07:27 UTC `disabled_manually`，无待/运行 job。均不是本卡真实 D 证据 | `vars-ready=passed`；`premerge=unknown`（本地/mock 证明首写前 secret 对 vars、双只读探针、全库 SQL/Time Travel bookmark 的 `age` 加密 runner 外 artifact 上传读回、global/per-example 预分配 manifest、失败零写）；`postrelease=unknown`（独立受保护 environment、七项凭据迁移并移除 repo/base 默认注入、`cloud-live-recover.yml` 恢复通路与独立 custodian、实际 CI release-SHA 受保护 dispatch/精确 grant、两完整 target、写后限定清理/保留）。自身 `E-CM-L3-SAFETY` 未证，不可真实首写、D/complete 或放行 CM-10；本地 fake/mock/default C 可执行 |
| `E-CM-L3-10`（CM-10） | 继承上述用户确认期望与历史只读背景；不得借安全卡的 release 时点证据 | 安全卡先发布；本卡重基后另证 DEP-CM-05.premerge，本卡 release-SHA CI 独立核 secret/vars、D1/R2 探针、trigger 完整 SQL/owner 基线、全库加密备份读回、global/per-example UUID manifest 与写后清理/受控保留，两个完整 live target 均真跑。`E-CM-L3-10=unknown`、postrelease=unknown，阻本卡真实写与 D/complete；本地 fake/mock/default C 可执行 |
| `E-CM-L3-11`（CM-11） | 同一冻结精确期望与历史只读背景，不可沿用安全卡或 CM-10 写前证据 | CM-10 发布后本卡独立重证 DEP-CM-05.premerge，并在本卡 release-SHA CI 生成独立备份/身份/manifest/善后与两完整 live target 的 `E-CM-L3-11`。当前 `unknown`，真实写、D/complete 未放行；本地 fake/mock/default C 可执行 |

### 3.1 plan-20260918（add 收口）— 当前活动计划

执行链（依赖边）：`OI-01 → OI-02 → OI-03 → OI-04 → OI-05 → IA-01 → IA-02 → CH-01 → CH-02 → AU-01..06（验收，no-release）→ PSF-01..03 → SW-06 → FM-03/04 → WT-05..07`。

| 卡 | 状态 | 发布 |
|---|---|---|
| OI-01 进程内锁回归守卫 | `done/complete` | v0.23.4 |
| OI-02 锁超时诊断 | `done/complete` | v0.23.5 |
| OI-03 锁等待退避与预检回放 | `done/complete` | v0.23.6 |
| **OI-04 批量发布 repair marker** | **`in-progress`** | **v0.23.7** |
| OI-05 marker 失败错误契约 | `pending`（写 `src/cli.rs`，受 DEP-AD-12 / CX-30 串行约束） | — |
| IA-01/IA-02、CH-01/CH-02 | `pending` | — |
| AU-01..AU-06（验收收口，#491–#494） | `pending` | no-release（v0.22.48 已交付） |
| PSF-01..03、SW-06、FM-03/04、WT-05..07 | `pending` | — |

### 3.2 plan-20260919（XDG 配置迁移）

| 卡 | 状态 | 发布 |
|---|---|---|
| GCX-01 路径决议统一与 XDG 默认 | `done/complete` | v0.23.1 |
| GCX-02 legacy 库自动迁移 | `done/complete`（2026-10-07：实现/单测/集成/失败注入/文档、fmt+clippy+聚焦门全绿；独立发布 v0.30.32，PR #615） | v0.30.32 |
| GCX-03 全域 vault unseal key 随迁 | `done/complete`（2026-10-07：实现/单测/集成/密码学回归/文档、fmt+clippy+聚焦门全绿；独立发布 v0.30.33，PR #616） | v0.30.33 |
| GCX-04 用户级 hooks 路径对齐 | `done/complete`（2026-10-07：实现/单测/compat 回归/文档、fmt+clippy+聚焦门全绿；独立发布 v0.30.34，PR #617） | v0.30.34 |

> **2026-10-07 收口说明**：GCX-02/03/04 已全部 `done/complete`，分别经独立发布 v0.30.32 / v0.30.33 / v0.30.34（D 组 `base.yml`+CodeQL 全绿，含 `compat-offline-core` 全量 L1+L2+L3；GCX-04 的 `opencode-export-linux` 为 OpenCode Session Capture 开发中已知波动，不阻塞）。版本面 `compat_version_surface_sync` 通过。本轮按操作者指示**不调用 Codex review**，故「完成判据」的 review 门仍未勾；DEP-GCX-01（`../libra-backend`）仍不满足，仅阻塞网站页同步；DEP-GCX-03 已满足（`.env.test`/`.env.live-test` 存在）。

### 3.3 plan-20260819（Memory M2，R30 历史方案）

2026-09-21 R30 重设计：按 0.23.x 拆除后的 main 重写架构 seam（ADR-M2-05/07/10 改写，新增 ADR-M2-12..15），整份迁到模板 v2.8（ER-08 逐卡发布 + ER-14 nextest + 版本面集合以 `compat_version_surface_sync` 为权威），新增 M2-16A..E 接口融合卡与 M2-16D1（自 M2-16D 条件性拆出），并把 PR #456 改为正向移植源（禁止 merge）。R30 r37 于 2026-09-22 对冻结版 `4fbaf4bd…` 取得 Codex/Claude 同版双 PASS，但全部 M2 卡仍为 `pending`，未实施。使用者随后要求独立重设计；MEM-01/02 现由 [`plan-20260926.md`](plan-20260926.md) 承接，旧 M2 卡待该计划 DM-00 正式登记退役。本节保留 R30 历史状态，不构成当前开工许可。

| 卡 | 状态 |
|---|---|
| M2-16A #456 memory 正向移植与 seam 重挂（R30 执行起点） | `pending` |
| M2-16B `EpisodeCompilerModel` seam | `pending` |
| M2-16C bridge `memory.recall` / `memory.episode.record` | `pending` |
| M2-16D 终态适配决策（audit，no-release） | `pending` |
| M2-16D1 条件性 AgentRun 适配 | `pending`（僅 M2-16D 判定可映射時啟動並納入收口；不可映射時登記 `DEFER-M2-09` 墓碑） |
| M2-16E #456 收口与迁移映射（docs，no-release） | `pending` |
| M2-01 冻结 M2 合同与领域类型 | `pending`（R30 双评审 + M2-16A 移植后复核） |
| M2-01K 仓库本地 keyed digest 基础 | `pending` |
| M2-02 Memory/Episode SQLite schema | `pending` |
| M2-02F FTS5 构建与搜索 schema | `pending` |
| M2-02R 共享 receipt 账本与保留策略 | `pending` |
| M2-03 线性 ref CAS transaction primitive | `pending` |
| M2-04 MemoryWriter 与 repo 权威历史 | `pending` |
| M2-05 保护并隐藏本地 Memory branch | `pending` |
| M2-06 事件重放/增量投影与 rebuild | `pending` |
| M2-07 来源、证据与自动信任边界 | `pending` |
| M2-08 终态触发与可恢复 generation job | `pending` |
| M2-09 Task Episode 编译 | `pending` |
| M2-10 Intent Iteration 编译 | `pending` |
| M2-11 FTS5/BM25 召回与证据展开 | `pending` |
| M2-12 Agent 冻结上下文与 ContextSelectionReceiptV1 | `pending` |
| M2-13 最小 `libra memory` 命令 | `pending` |
| M2-14 Episode/召回 benchmark 与文章 | `pending` |
| M2-14C 核心覆盖率与 CI 门 | `pending` |
| M2-15 计划收口与最终聚合发布 | `pending` |

**分階段收口：** M2-16D/E 為 no-release 卡，在 M2-15 的 C/D 前保持 `locally-accepted`；M2-15 的 C/D 以其變更作為同一 release closure，D 證據產出後回填 `complete`，再執行 PR #456 關閉/supersede。

**外部 host 前置：** `DEP-M2-DSH-01`（DeepSeek Harness bridge 22-method 客戶端）為**非阻塞** post-M2 互操作驗證；未交付時 M2-16C 以 in-repo 新舊 host fixture 完成，真實互操作記 N/A + 殘餘風險。

**來源前置：** `DEP-M2-SRC-01`（#456 只讀 source clone；`git` 審計命令只在此 clone 執行，目標 Libra checkout 一律用 `libra`/`rg --files`）。

**阻塞登记：** PR #456（`Anduin9527:codex/memory-m2-core-draft-pr`）不可直接合并（距 main 296 commits、39 档冲突、依赖已删 Code SCC）；由 M2-16A 正向移植、M2-16E 收口关闭（ADR-M2-15）。

### 3.7 issues/474（clone 浅克隆 / bundle / bare / mirror）

发布窗口：CL-01 → … → CL-15。Phase 0 Codex R6 `PASS`（2026-09-23）。

| 卡 | 状态 | 发布 |
|---|---|---|
| CL-01 fsck 断链检测与 shallow 豁免 | `done`/`complete` | v0.23.47（#510） |
| CL-02 log/rev-list shallow helper | `done`/`complete` | v0.23.48（#511） |
| CL-03 其余历史遍历 | `done`/`complete` | v0.23.49（#512） |
| CL-04 本地 Git 浅边界 | `done`/`complete` | v0.23.50（#514） |
| CL-05 `--depth` 隐含单分支 | `done`/`complete` | v0.23.51（#516） |
| CL-06 普通路径忽略浅化参数 | `done`/`complete` | v0.23.52（#517） |
| CL-07 shallow Git 源克隆 | `done`/`complete` | v0.23.53（#518） |
| CL-08 bundle create 写入 HEAD | `done`/`complete` | v0.23.54（#520） |
| CL-09 clone 接受 bundle 源 | `done`/`complete` | v0.23.57（#561） |
| CL-10 以 bundle 为 remote 的 fetch/pull | `done`/`complete` | v0.23.58（#562） |
| CL-11 bare/mirror 默认目录名与 bare 布局 | `done`/`complete` | v0.23.59（#563） |
| CL-12 clone --mirror 全命名空间 | `done`/`complete` | v0.23.60（#564） |
| CL-13 mirror-aware fetch/prune | `done`/`complete` | v0.23.61（#565） |
| CL-14 depth 测试 + T5 live | `done`/`complete` | v0.23.62（#567） |
| CL-15 branch -a/-r 远程显示 | `done`/`complete` | v0.23.63（#568 / `d00366b`） |
| Closeout proxy hygiene | `done`/`complete` | v0.23.64（#569 / `3554f1e`；DEFER-CL-ENV-*） |

### 3.6 issues/476（工作树命令族）

发布窗口：WT-01 → WT-02 → WT-04 → WT-08 → WT-09 → WT-10 → WT-11 → WT-03。用户 2026-09-20 覆盖 ER-05（执行 Agent 自审）。

| 卡 | 状态 | 发布 |
|---|---|---|
| WT-02 `status -M` | `done`/`remote-pending` | v0.23.29 |
| WT-04 `rm` 拒绝分类 | `done`/`remote-pending` | v0.23.30 |
| WT-08 裸 stash = push | `done`/`remote-pending` | v0.23.31 |
| WT-09 stash push 预检顺序 | `done`/`remote-pending` | v0.23.32 |
| WT-10 消息格式 | `done`/`remote-pending` | v0.23.33 |
| WT-11 `--index` 恢复 | `done`/`remote-pending` | v0.23.34 |
| **WT-01 回归守卫** | **`done`/`remote-pending`（C 组落地中）** | **v0.23.35** |
| WT-03 init 默认 ignore | `pending`（DEP-WT-08 用户评审） | — |

### 3.4 plan-20260822（Operation Log v2）

| 卡 | 状态 |
|---|---|
| OL-01..OL-12、CH-01..CH-04 | `done/complete` |
| OL-13 多 worktree Operation heads 与 reconcile | `done/complete`（v0.23.0，`9da06b4`） |
| OL-14 Operation/Change Web 只读图 | **已取消**（G-09 墓碑，2026-09-20：`web/` 随 plan-20260920 拆除且不重建；`libra op log/show` 为现行查询面） |
| OL-15A v1-boundary runtime cutover | `done/complete`（PR #503：生产 mutation 统一 v2 middleware，legacy namespace 仅 migration/schema fixture 保留） |
| OL-15 移除 v1 operation 代码/表/命令 | `done/remote-pending`（PR #503：v1 wrapper/service/model/fallback 已移除；等待 compat-offline-core） |

### 3.5 plan-20260903（merge）与 plan-20260729（CT-01）

- plan-20260903：MG-01..MG-21 卡片全 `done/complete`；**计划级收口（完成判据、deferred 差异登记）仍待完成**，`plan-long.md` 标「实施中」。
- plan-20260729：CT4-01 已发布（v0.21.21）；**CT3-07 已正式延后（转换轴 → DEFER-09，已被 plan-20260825/27 承接关闭）**；完成判据 13 项未勾选。

### 3.6 plan-20260830（sandbox export）

SBX-01..05 `done/locally-accepted`；**发布步按 DEFER-SBX-06 正式延后**（DEP-SBX-05 未就绪），ER-13 收口门已绿（2026-09-01）。

---

## 四、当前执行指针（next action）

- **当前正在执行：** `plan-20260907` 已收口（B3-00..B3-17 全 `done`/`complete`，`v0.23.68`→`v0.29.0`；最终 Codex/Claude R40 双 PASS；GC-B3-01/02 收口守卫绿；`DEP-B3-05` 已满足）。`plan-20260913`（Media）FL-00..FL-05 亦已 `done`/`complete`（`v0.30.0`..`v0.30.5`），仅剩计划级收口门。**`plan-20260926` DM-00 已 `done`/`complete`**（2026-10-07），`plan-20260819` 已登记取代；**`DM-01`、`DM-10`、`DM-02` 已 `done`/`complete`**（DM-01 → v0.30.31；DM-10 → v0.30.35；DM-02 → v0.30.37，均 REL-DM-01 独立发布 + D 组全绿；ER-05 评审待记录）。**下一个零依赖候选为 `plan-20260916` CAP-01（先重核 `DEP-CAP-01`）；plan-20260926 内的下一张为 `DM-13`**（新鲜度、horizon 窗口与重建等价）。`plan-20260925` SCAP-01 / SCAP-02 已 `done`/`complete`（`v0.23.55` / `fa3849e`，D 组全绿；2026-09-28 本地最终门 8241/8241 passed，本轮不 bump 版本）。
- **本会话已收口：** `issues/577` SA-02/SA-01 已 `done`/`complete`；PR #578、v0.24.1、8/8 release jobs、网站 `cf` 与生产页 D 证据均完成，Issue #577 CLOSED。
- **SCAP-01 与 B3-00：** SCAP-01 已收口，`runtime.rs` 写集互斥结束；B3-00 已收口（`v0.23.68`）。
- **下一步：** `issues/474` 已收口（DEP-CL-07 完成，#474 CLOSED）。`plan-20260926` DM-00 已 `done`/`complete`、DM-01/DM-10/DM-02 已 `done`/`complete`；下一个零依赖候选为 `plan-20260916` CAP-01（先重核 `DEP-CAP-01`），plan-20260926 内下一张为 `DM-13`。
- **并行窗口（不在本执行指针）：** `plan-20260927` 的 CM-01 等 pinned scope FIX-CM-01，CM-06/10 有上述本地进度，所有卡 C/T-5 等 scope FIX 发布；CM-10 还等 CM-03/CM-13 共享文件、FIX-CM-LIVE-GATE 与 FIX-CM-CLOUD-LIVE-SAFETY 独立发布；真实 Cloud L3 只在受保护 release-SHA CI 写 job，DEP-CM-05 的 vars-ready 已过、premerge/postrelease 未过，live-compat workflow 当前 `disabled_manually`，由 root 满足受保护环境、凭据迁移与恢复门后重开；FIX-CM-04 待修订计划 Claude PASS，CM-04 阻塞；FIX-CM-08 与 CM-13 待开工，尚无本计划远端发布。Operation 独立 P1 交 `DEFER-CM-OP-01`，worktree approved-project 写入缺口交 `DEFER-CM-WT-01`，Cloud 小型 status 抽离交 `DEFER-CM-CLOUD-STATUS`。`issues/476` WT-03 仍等 DEP-WT-08；`plan-20260918` 其余 add 卡待推进。MEM-01/02 由 `plan-20260926` 承接，R11 字面 `VERDICT: PASS`；`DM-00` 已 `done`/`complete`、`DM-01` `done`/`complete`、`DM-10` `done`/`complete`，其余卡 `pending`，`DM-05` 仍受 `DEP-DM-06` 阻塞。
- **`plan-20260924` ACF-12 最新增量（2026-10-01 04:22:38 UTC）：** 修正逐条刷新队列可能突破单次批次上限的问题，doctor repair 共用最多 5 次 replay budget 与 2 秒 cooperative deadline；bounded queue 与共享预算单测各 1/1、`agent_doctor_repair_test` 30/30、`session_capture_docs_contract` 6/6、`compat_agent_docs_contract` 9/9、`cargo check --lib` exit 0。EN/zh、development command/tracing 与 `COMPATIBILITY.md` 已同步；backend 网站 mirror 当时未改（缺少 `cf` VCS 命令）；该限制已由 2026-10-01 04:49:35 UTC 的 backend 网站同步取代（见 plan-20260924 ACF-12 Verification update）。实现仍无真实 pending-artifact CAS/object publication 验证，12 条 unused warnings 未清；ACF-12 及整计划保持未验收，完整进度见 plan-20260924 当前卡证据。

---

## 四·零、零依赖可立即启动的任务卡（入度为零）

本表在每个「未执行」计划中列出**依赖图入度为零**（无内部前置卡）的任务卡，并标注它是否真的「可立即开工」，还是仍被计划级 review 门或跨计划 `DEP-*` 门控。判据：`开工许可 = 内部无前置 ∧ review 门已 PASS ∧ 外部/跨计划 DEP 已满足`。

| 计划 | 入口卡 | 卡要做什么（简述） | 内部前置 | 计划级 review 门 | 外部/跨计划门控 | 可立即开工 |
|---|---|---|---|---|---|---|
| [`plan-20260822`](plan-20260822.md) | （PR #503 收口；OL-14 已取消、OL-15 `remote-pending`） | — | — | — | — | ⏳ 等待 compat-offline-core 远端门禁 |
| [`plan-20260926`](plan-20260926.md) | DM-00 | 冻结 #456 事实基线、编制取材清单并登记 R30 退役 | 无 | **已过**（R11 字面 `VERDICT: PASS`，`P0=0` / `P1=0`） | **DEP-DM-03**（#456 只读 clone，`/tmp/libra-pr456` 降级路径）+ **DEP-DM-05**（R30 退役登记）均已满足 | ✅ `done`/`complete`（2026-10-07：取材清单 + 冻结远端事实 + R30 退役） |
| [`plan-20260919`](plan-20260919.md) | GCX-02 | legacy 全局 config DB 首次使用自动迁移（锁+快照+校验+原子提交） | GCX-01（已 `done/complete`） | **本轮按操作者指示未评审** | DEP-GCX-02：与 plan-20260918 串行写 `COMPATIBILITY.md`/网站页（0918 未开工，写集 clean） | ✅ `done/complete`（v0.30.32，PR #615） |
| [`plan-20260919`](plan-20260919.md) | GCX-04 | 用户级 hooks 文件路径对齐 XDG（macOS 只读回退，实现泛化到任何原生配置目录不同的平台） | GCX-01（已 `done`） | **本轮按操作者指示未评审** | DEP-GCX-01 不满足（本机无 `../libra-backend`），仅阻塞网站页；发布队列 GCX-02→GCX-03→GCX-04 | ✅ `done/complete`（v0.30.34，PR #617；opencode-export-linux 为 OpenCode 开发中波动不阻塞） |
| [`plan-20260912`](plan-20260912.md) | MB-01 | 有界 mega2 tree transport 与 wire validation | 无 | **双评审已 PASS**（Codex R7 / Claude R5） | DEP-MB-01：mega2 `a1293686` tree API pin 现场重核（2026-09-21 已前推） | ✅ `done/complete`：v0.23.37（84f6bd8）；codeql + release.yml 8/8 jobs 全绿；CDN 产物 HTTP 200 |
| [`plan-20260916`](plan-20260916.md) | CAP-01 | Agent Capture wire types、URL、uid、transport trait | 无 | **双评审已 PASS**（Codex R3 / Claude R3） | DEP-CAP-01：monoengine `2b8f365` capture HTTP pin 重核（ER-CAP-02） | ⚠️ 需重核 DEP-CAP-01 |
| [`plan-20260924`](plan-20260924.md) | ACF-01 | Agent Capture validated ingress contract | `DEP-ACF-01`（既有執行基線） | R91 字面 `VERDICT: PASS` | ACF-09 `Lifecycle=done`、`Acceptance=complete`；`DEP-ACF-MIRROR` 已交接 | ✅ **已收口**；ACF-01..20 与 FIX-ACF-01 `done`/`complete` |
| [`plan-20260913`](plan-20260913.md) | FL-00 | 核实 Media 前提与热路径（audit，no-release） | 无 | 联合 review 已 PASS（U2 `VERDICT: PASS`） | DEP-FL-04：plan-20260907 **已完整收口** | ✅ 已 `done`/`complete`（FL-05 亦完成；本计划仅剩计划级收口门） |
| [`plan-20260904`](plan-20260904.md) | CX-00 | codex-cli 0.152 基线探测与 ADR go/no-go | 无 | **未过**（R5 `FAIL`；Claude 亦未出 verdict） | CX-30 另受 DEP-CLI-mirror | ❌ 禁止开工 |
| [`plan-20260905`](plan-20260905.md) | CC-00 | Claude Code 2.1.259 Hook source 契约探测 | 无 | **未过**（须 Claude `PASS`） | 无 | ❌ 禁止开工 |
| [`plan-20260906`](plan-20260906.md) | SC-01 / SC-02（可并发） | SC-01 `base.yml` 最小权限加固；SC-02 会话入口 id 守卫 | 无 | **未定稿**（R2 PASS 已作废；R22 `FAIL`） | SC-04 受 DEP-SC-01/04/05/06；SC-07 受 DEP-SC-07 | ❌ 禁止开工 |
| [`plan-20260902`](plan-20260902.md) | OG-00 | opencode 2.0.22 Hook/export 契约探测 | 无 | Codex R72 字面 `VERDICT: PASS`（2026-10-03 16:54:47 UTC）。本轮没有新的 Claude 复审 | 无（Phase 1 与 RG 六卡解耦） | ❌ 禁止开工 |
| [`plan-20260911`](plan-20260911.md) | PI-01 | Repository-only `agent_kind=pi` migration | 无（DEP-PI-04 已满足：9/10 已收口） | **Claude Code 429 无 verdict，禁止开工** | DEP-PI-01/03 | ❌ 禁止开工 |
| [`plan-20260925`](plan-20260925.md) | SCAP-01 | AgentTraces ingest 改调 `decide`。`v0.23.55` / `fa3849e` D 组全绿 | SCAP-02（登记已核对） | R9 双 `PASS` | 已收口 | `done`/`complete` |
| [`plan-20260907`](plan-20260907.md) | B3-00 | pin `git-internal`（保持 `=0.10.2`）并引入 `object_format` 事实源 | 无 | **双评审已 PASS**（R40 Codex/Claude 双 PASS） | `v0.23.68` 已发布；focused VER 绿 | `done`/`complete` |
| [`issues/470`](issues/470.md) | FM-01 | 共享写入原语与 `restore` 系物化 | 无 | 尚未 Codex review | 关闭依赖 plan-20260918 FM-03/04（DEP-FM-06/07） | ❌ 禁止开工 |
| [`issues/473`](issues/473.md) | IN-01 / IN-03 / IN-02 | 空模板自引用防护 / 存储路径前置检测 / 换格式 reinit fail-closed | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/474`](issues/474.md) | — | CL-01..CL-15 + closeout + DEP-CL-07 已完成；#474 CLOSED | — | **R6 `PASS`** | 无 | ✅ **已收口** |
| [`issues/475`](issues/475.md) | CF-02 / CF-01 | key/模式校验与退出码 / 带 value-pattern 的删除 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/500`](issues/500.md) | CS-16（upstream qualification 入口卡；其 qualified 结论受外部硬门 DEP-500-11/13/14 门控，执行本身不被阻塞；CS-00 为其下游） | upstream qualification（CS-16 → CS-19 → CS-17 → CS-20 依序执行）后由 CS-00 盘点本地统计产物并对 Mega2 artifacts 做 GO/NO-GO；CS-16 为零内部前置入口 | 无（入口 = CS-16；执行链见描述） | **R41 双评已过**（Codex full-plan PASS + Claude targeted PASS，P0/P1=0） | CS-16/CS-19/CS-17/CS-20 qualification audits 已完成并记录 deployment-bound `not-qualified` evidence；CS-00 据此 NO-GO，CS-10..13 正式延后。CS-20 aggregate 非 qualified，因此 GO 与实现卡启动硬门未满足。 | ✅ **已收口（NO-GO）**：本地 trunk compose 实测 CS-16/19/17 `not-qualified` → CS-20 `not-qualified` → CS-00 判定 NO-GO；CS-10/11/12/13 延入 DEFER-CS-02/03/07/10（重启条件：维护者在 #500 确证数据集且新 CS-00b 为 GO）；CS-18 登记 defer+outgoing handoff |
| [`issues/476`](issues/476.md) | WT-03 | `init` 不再创建默认 `.libraignore` | DEP-WT-08、DEP-WT-05 | 用户 2026-09-20 覆盖：执行 Agent 自审 | DEP-WT-08（ADR-WT-04 用户评审）未满足 | ❌ 阻塞 |
| [`issues/478`](issues/478.md) | LG-01 | `log`/`rev-list` `--grep` 模式类型与匹配范围 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/479`](issues/479.md) | EC-01 | plumbing 退出码契约守卫 | 无 | 尚未 Codex review | DEP-PL-04：与 plan-20260918 `add.rs` 串行 | ❌ 禁止开工 |
| [`issues/480`](issues/480.md) | HP-01 / HP-02 | `remote add` 默认 refspec 与 `--mirror` / `remote rename` 改写推送目标 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/481`](issues/481.md) | MX-01 | 短对象名候选去重 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/483`](issues/483.md) | CO-01 | `count-objects` 无参数形式与命令契约 | 无 | 尚未 Codex review | CO-03/04 受 DEP-CO-04（`src/cli.rs`） | ❌ 禁止开工 |
| [`issues/487`](issues/487.md) | IG-01 | 本地传输复用已修复的 pack 编码器 | 无（必最先完成） | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/488`](issues/488.md) | GR-01 | `grep --exclude-standard` / `--no-exclude-standard` | 无 | Codex R1 `PASS`（2026-10-06）；DEP-GR-01/02 未满足，发布步因 `gh` 未认证阻塞 | 实现完成；E1-E9 矩阵与 git 实跑一致；测试 48/48；`compat_ledger_schema` 43/43 | 实施中 |
| [`issues/488`](issues/488.md) | GR-02 | 仓库模式子目录作用域、相对路径与 `--full-name` | GR-01 | Codex R1 `PASS`；M-SCOPE 16/16 与 git 逐字节一致；S11 TC-1596 移植通过 | 实现完成；测试 54/54；fmt/clippy 通过 | 实施中 |
| [`issues/490`](issues/490.md) | SW-01 | 采用支持 index v3 扩展标志的 `git-internal` | 无 | 尚未 Codex review | DEP-AD-07：与 plan-20260918 串行 | ❌ 禁止开工 |
| [`issues/582`](issues/582.md) | HKT-00 | 固定安全 host-key interaction 設計，並移交 HP-17／DEFER-07 | 无 | R1 `PASS`（計劃自審） | DEP-HKT-01/02/03 已滿足：HKT-00 固定 Git/OpenSSH 參照並完成移交，HKT-01/02 已实现 | ✅ HKT-00/01/02 全部 `done/complete`（v0.29.1） |

> 说明：✅ = 无任何门控，可立即开工；⚠️ = 内部无前置但仍有外部/跨计划 `DEP-*` 或发布队列约束；❌ = 计划级 review 门未过（多数 issue 计划尚未 Codex review），按模板 ER-05 / GC-01 **禁止**标 `in-progress`。
>
> **一致结论：** `plan-20260907` B3-00 已收口（`done`/`complete`，`v0.23.68`）。`plan-20260913` FL-00（Media）实已 `done`/`complete`，只剩计划级收口门。**`plan-20260926` DM-00 已 `done`/`complete`**（2026-10-07：取材清单 + #456 冻结 + R30 退役），`plan-20260819` 已登记取代。当前真正「零依赖且 review 已 PASS」的**下一个可开工**未启动候选是 **`plan-20260916` CAP-01**（需先重核 `DEP-CAP-01` monoengine pin）。`plan-20260822` OL-13 已于 2026-09-20 补记账为 `done/complete`（实现随 `9da06b4`/v0.23.0 发布）；OL-14 已取消（G-09 墓碑），OL-15A 已完成 runtime cutover，OL-15 为 `done/remote-pending`，等待 compat-offline-core 远端门禁。

---

## 五、延后决策或实施项（DEFER-* 汇总）

### 5.1 plan-20260920（拆 Code/Publish）

| ID | 内容 | 状态 |
|---|---|---|
| DEFER-RC-01 | `ai_*` 表 down-migration | 延后（对象闭包风险；冷冻即可） |
| DEFER-RC-02 | 物理回收 `refs/libra/intent` / `sessions/code` 对象 | 延后（doctor 残留报告稳定后） |
| DEFER-RC-03 | 重建内部 mutating 执行器 | 延后（用户明确不要） |
| DEFER-RC-08 | 为外部捕获另建用量记账 | 延后（产品要求捕获用量时重启） |
| DEFER-RC-04/05/06/07 | 独立 MCP / 版本面四→三 / automation 去留 / 删 `worker/` | **已关闭** |

### 5.2 plan-20260919（XDG）

| ID | 内容 | 状态 |
|---|---|---|
| DEFER-GCX-01 | 显式 `libra config migrate-global` | 延后 |
| DEFER-GCX-02 | 清理 `~/.libra` 残留空目录/旧 key | 延后 |
| DEFER-GCX-03 | `XDG_DATA_HOME`/`XDG_STATE_HOME` 全量 XDG 化 | 延后 |
| DEFER-GCX-04 | OS keyring 承载全域 unseal key | 延后 |

### 5.3 plan-20260918（add）

DEFER-AD-01..16：Git advice、ignored 相对路径、`add -u --ignore-missing` 128 语义、typechange/gitlink 暂存、全局 `-c`、其余 pathspec 开关、`add -i`/`--edit`、旧锁文件/超时键、`add -n` 零写入、`add -p` 会话（回 477）、gitlink 缺失暂存、Windows symlink fixture、批量 add 剩余耗时、conflict-marker-size、`add -n`/`-v` 对齐 Git 格式。全部延后（多为外部用例/对照集升级重启）。

### 5.4 其它计划

- **DEFER 候选（2026-10-08 13:54:21 UTC 登记，来源：NPR-19 r3 Claude 独立审查 P3-4）**：`DEFER-CFG-RAW-SET-FLAG-INHERITANCE`——`ConfigKv::set_with_conn`（raw 存储 API）保留"历史 flag 继承"：调用方传 `encrypted=false` 而行已加密时按 `effective_encrypted=true` 落明文值；CLI/import 已经由 FIX-NPR-CONFIG-01 的 guard 保护，但约四十处生产调用（init/branch/clone/vault/auth/credential/hooks）仍走 raw API，需后续单轴卡让其走同一 guard 或记录不可走的理由。

- **DEFER 候选（2026-10-08 12:25:39 UTC 登记，来源：NPR-19 r1 Codex 独立审查的 inherited upstream 项，均在 FIX-NPR-CONFIG-01/NPR-19 写集外、基线既有）**：`DEFER-CFG-IMPORT-PLAINTEXT-FALLBACK`——`src/command/config.rs` import 在缺 unseal key/加密失败时回落明文却保留 encrypted 标记（单值 :4137、多值 :4094–4107）；`DEFER-CFG-TYPED-ERROR-VALUE-DISCLOSURE`——typed-value 转换错误消息内插输入值（:2170/:2188/:2192）；`DEFER-CFG-LEGACY-PANIC-PATHS`——`src/internal/config.rs` 遗留生产 `expect`/`panic!`（:3038 等 10 处）。三项需各自单轴 FIX 卡与设计双审后处置，不借 NPR-19 载体扩写。

- issues/500：`DEFER-CS-01`（中央查询/聚合 UI，重启条件：独立查询契约）；`DEFER-CS-02/03/07/10`（仅 NO-GO，由 CS-18 登记各卡独立重启条件；卡进入 DEFER 后脱开初始 GO 判定，重启条件为维护者在 #500 确证数据集且新 CS-00b 为 GO）；`DEFER-CS-04`（Mega2 committed-set revoke/retention 与完整 artifacts read-surface authorization lifecycle，重启条件：DEP-500-14 接收方计划交付 upstream qualification harness，且 CS-16 metadata-read、CS-19 object GET/HEAD、CS-17 revoke、CS-20 bundle qualification 全部 passed）；`DEFER-CS-05`（冷冻表物理删除，随 plan-20260920 RC 策略）；`DEFER-CS-06`（接线 `stats.rs` 计数器，独立产品决策）；`DEFER-CS-08`（`--since` 导出窗口，产品要求时）；`DEFER-CS-09`（gated Libra live artifacts 写；仅在 CS-16 metadata-read、CS-19 object-read、CS-17 revoke、CS-20 bundle outcome=`qualified` 与 CS-11 实现后作 publication gate）；`DEFER-CS-11`（`plan-long.md:179/:795` usage 陈旧表述更正，随 `DEP-500-12` 移交 plan-long 维护者——非阻塞本计划）。
- plan-20261001-mega-browser-noninteractive：`DEFER-MN-01..09`（按名称取 tag、非根 tag path 与 tag 的 target/tagger 字段、文件条目操作、组合 move/批处理/轮询、成功状态码与可配置超时、stable code 映射统一、`path/provision` 与 `import-repo/remove`、mega2 仓内的 Libra 黑盒用例（由 mega2 plan-20261001 `DEP-BB-04`/`DEFER-BB-03` 承接）、`DEP-MN-04` 超时后的 live 证据降级）。
- plan-20260927：`DEFER-CM-01` 暂不机械拆分 `fetch.rs`、`push.rs`、`maintenance.rs`；生产职责本身继续增长或真实任务反复跨职责修改时，先重审源码与测试归属，再另立日期计划。
- plan-20260923: DEFER-CP-01 additional shells; DEFER-CP-02 network suggestions; DEFER-CP-03 unimplemented underlying capabilities. Existing local Libra capabilities may not be hidden by these deferrals.
- plan-20260924：DEFER-ACF-01..04 保留；DEFER-ACF-05 为 TurnEnd/nonterminal 自主恢复，按用户选择方案 1 延后，不保证 provider 重投或 turn-level checkpoint 补齐；DEFER-ACF-06（snapshot/extraction 既有 AgentKind dispatch，计数 ratchet 冻结）、DEFER-ACF-07（live_capture → subagent_content 依赖边）、DEFER-ACF-08（provider live capture 实现迁至各 builtin adapter）为 ACF-06 拆分新登记。
- plan-20260925：`DEFER-SCAP-01` 已按用户指示删除，ID 不再复用；`DEFER-SCAP-03` 不引入 Entire git phase；`DEFER-SCAP-04` 不改 `docs/development/tracing/agent.md`，也不向其它计划派发该文件。owner 的 SessionStart/TurnStart 豁免已经存在，本计划不改。
- plan-20260902：`DEFER-OG-01` 仍只延期 checkpoint 回灌（import，以及回灌流程中的 session delete / `session.remove`）。捕获插件在采集路径上调用 `remove`、`compact`、`create` 或其他会改会话的方法，以及填写会改变提示、压缩结果、标题、重试或网络帧的钩子字段，按 GC-OG-03 永久禁止，无 ADR 重启。观察并转发 `session.compacted` 留在本计划。读取 `parentID` 仍是 `DEFER-OG-02`。2026-10-03 15:47:05 UTC 起，未执行卡按模板 v2.12：nextest 全量门、三处版本面、每卡 ER-06a 文档字段、验收记录只写在计划文件内。2026-10-03 15:53:30 UTC：唯一 pin 为 OpenCode 2.0.22 @ `527f0b931d1f9b3ebd34e106c51b31ce5db5b075`；成功门为 Linux 与 macOS 都捕获到内容，macOS 还要先满足 export 子进程及其后代的取消安全隔离。Codex R72 字面 `VERDICT: PASS`（2026-10-03 16:54:47 UTC）。

- plan-20260830：`DEFER-SBX-06` 发布步延后（DEP-SBX-05 未就绪）。
- plan-20260729：`DEFER-09`（CT3-07 转换轴）——已被 plan-20260825 TA-01/02 + plan-20260827 NP-00 承接关闭。
- plan-20260819：`DEFER-M2-01..09`（含 `DEFER-M2-09`：AgentRun/session 终态适配，M2-16D 判定不可映射时启用）。
- plan-20260822：`DEFER-01..03`（Operation Log 范围外）；`DEFER-02`（Web 图 SSE）已随 OL-14 取消关闭；`DEFER-05` 已由 PR #503 的 OL-15A 承接并关闭；OL-15 等待远端兼容门禁收口。
- plan-20260903：`DEFER-01..12`（merge 范围外/deferred 差异）。
- plan-20260715：历史封存；Code 专属 `DEFER-01..08` 已由 plan-20260824 交付后拆除或由 plan-20260920 直接墓碑化，`DEFER-09/10` 已完成关闭；无现行可重启项。
- plan-20260824：历史封存；全部 Code 专属 DEFER 已由 plan-20260920 墓碑化，无现行可重启项。
- plan-20260825：历史封存；Code 专属 `DEFER-PS-01..04` 已墓碑化；`DEFER-PS-05` 已由 plan-20260917 关闭。仅通用测试基础设施 `DEFER-PS-06/07` 保留为另立计划候选，不得用于恢复 Code。
- issues/498：`DEFER-TT-01..08`（tree/blob 目标、nested-tag advice、`-f` 覆盖措辞、show/log 嵌套展示、传输非 commit tag、`-n` gpgsig 标题、umask 敏感测试、`for-each-ref`/`describe` 嵌套 peel）。计划已收口；DEFER-TT-02/03/06 → `issues/477.md` 的 477b HW-05；DEFER-TT-04 → 478；DEFER-TT-05 → 474/480；DEFER-TT-08 待新建 issue。

---

## 六、跨计划依赖（DEP-* 现行生效项）

| DEP-ID | 类型 | 内容 | 现状 |
|---|---|---|---|
| DEP-MEM-R6-HANDOFF | 跨计划缺陷移交（outgoing：plan-20260904 FIX-RG-01 → plan-20260926） | FIX-RG-01 R6 组合源码的独立 Codex 原审（本机路径 `/private/tmp/libra-review-original-reports/fix-rg-01-r6-codex/codex-verdict.md`，不可公开复现；SHA-256 `9a4373126529a2187e720d299a6a42a4a0ab9e08585b78bfe844b1b46ac5d46f`）在上游 memory 模块（`src/command/memory.rs`、`src/internal/ai/memory/**`，F4 另见 `src/internal/change/store.rs:273`；DM-13（已 done/complete）与 DM-03（代码经 #623 e237365d 合并，卡仍 pending）交付、与 main 913efe3d 逐字节相同）发现 F1–F10 共 6 P1/3 P2/1 P3（占位子命令静默 exit 0、rebuild 非原子、跨来源删除、horizon 窗口 clamp 1..200、meta 解码吞错、fingerprint 域不完整；revoked/aged-out 计数硬编码 0、horizon 截断不可判定、freshness 契约无实现；`memory show` 示例无效），详见 plan-20260904 2026-10-08 10:41:48 UTC 记录。不在 FIX-RG-01 prod0 写集内，FIX 不改 memory；由 plan-20260926 owner 在 DM-11/DM-12 前处置 | 生效（2026-10-08 10:41:48 UTC 登记；不阻 FIX-RG-01 version-only 0.30.39 发布） |
| DEP-MN-03 | 跨仓交付（outgoing） | plan-20261001-mega-browser-noninteractive 交付 `mega2 browser` 非交互操作、机器契约与 live 门；接收方是 mega2 plan-20261001 的 Libra 域（`scripts/libra_smoke_storage_only.sh`；`DEP-BB-04`/`DEFER-BB-03`；`BB-65` 起编号已预留）。Libra 仓内的测试不受 mega2 工具链规定约束；任何一方都不得向 mega2 的 curl + git smoke 脚本加入 libra 用例 | **已交付（2026-10-03 13:05:47 UTC）**；功能下限 stable v0.30.26，live 参考与当前 stable v0.30.27；[契约](https://github.com/libra-tools/libra/blob/faae20c6d9e48c46b9bb290637a8425b6d1e073a/docs/commands/mega2.md#non-interactive-operations)、[live 门](https://github.com/libra-tools/libra/blob/faae20c6d9e48c46b9bb290637a8425b6d1e073a/tests/command/mega2_browser_noninteractive_test.rs#L2651)；lightweight tag 可能列表缺失或跨页重复，annotated tag 按名称分页。接收方在其 Libra 专用脚本追加，本计划不等待接收方 |
| DEP-MN-05 | 跨计划信息移交（outgoing） | plan-20261001-mega-browser-noninteractive 新增的 `mega2 browser` 操作 flag 进入 plan-20260923 CP-19 的能力台账；双方无实现写集交集 | 生效（信息性，不阻塞任一方） |
| DEP-SA-03 | Issue #577 与 plan-20260927 共享写集/发布窗口 | #577 SA-01 使用 `tests/command/mod.rs`、serial registry/nextest、版本面；与 CM/FIX 命中相同文件的实施和全部发布顺序串行，前卡推送后重基重测。SA-02 网站指南先行，其网站 `cf` 写集也须核对其它计划实际页面写集。 | **已解除（2026-09-27）**；#577 经两次主线重基、最终全量、PR #578、v0.24.1 和网站 D 门完成，SA-02/SA-01 均 `done`/`complete`。后续 CM/FIX 从已发布 main 重新核写集。 |
| DEP-CM-01 / DEP-CM-02 / DEP-CM-03 | 命令写集、发布窗口与 Cloud L3 | 13 张 CM + 9 张 FIX，共 22 卡；开工前核实际同文件写集。FIX-CM-01 → CM-01，FIX-CM-04 → CM-04，CM-05 → CM-13，CM-07 → FIX-CM-08 → CM-08，CM-03/CM-13 → FIX-CM-LIVE-GATE → FIX-CM-CLOUD-RECOVERY-AUTH → FIX-CM-CLOUD-RECOVERY-CLEANUP → FIX-CM-CLOUD-REPO-SCOPE → FIX-CM-CLOUD-LIVE-SAFETY → CM-10 → CM-11。其它全部卡 C/T-5 等 scope FIX 独立发布。`tests/INDEX.md` 全写者按 FIX-CM-01 → CM-01 → CM-06 → CM-02 → CM-03 → CM-07 → FIX-CM-08 → CM-08 → CM-09 → CM-12 → CM-04 → CM-05 → CM-13 → FIX-CM-LIVE-GATE → FIX-CM-CLOUD-RECOVERY-AUTH → FIX-CM-CLOUD-RECOVERY-CLEANUP → FIX-CM-CLOUD-REPO-SCOPE → FIX-CM-CLOUD-LIVE-SAFETY → CM-10 → CM-11 串行发布，CM-03 → CM-10 另串行写 serial registry；`CM-Publisher` `/root` 精确暂存。真实 Cloud L3 只在受保护 release-SHA CI dispatch 写 job 执行，按安全卡/CM-10/CM-11 各自 `E-CM-L3-SAFETY/10/11` 证独立期望、D1/R2 只读、trigger 全 SQL 基线、全库加密备份 runner 外上传读回、global/逐例 manifest、限定补偿/清理。 | 生效；CM-01、CM-04 `blocked`/空，CM-06、CM-10 `in-progress`/空，其余 17 卡 `pending`/空，均未发布。用户精确期望已确认，四 vars API readback 4/4；D1 25 triggers 中 16 remote-existing/unmatched 尚待 owner 核签。DEP-CM-03 与三个 E 仍 `unknown`；本地仅 fake/mock/default C，不执行真实 D1/R2 写。 |
| DEP-CM-04 | CM-07 网站 `cf` 开工基线 | 仅要求开工前只读核后端 VCS、远端 `cf` pre-SHA 与 `status.en.md` blob、隔离 clone、签名/DCO/普通推送配置权限和站点命令可用。网站页编辑、新源码锚点/porcelain-v2 修正、gen:docs/typecheck/build、签名推送、远端及实际部署证据，均为 CM-07 自身交付门；已推后失败按远端 post-SHA 条件化签名补偿，远端竞态/部署不确定进入 `remote-pending`。 | 开工基线未重核，网站未交付；DEP-CM-04 不要求先完成 CM-07 网站变更，避免自环；只 Git push 不算部署。 |
| DEP-CM-05 | Cloud CI 首写与发布门 | `vars-ready`：四项非密 repo vars 最后一项由 root 于 2026-09-25 08:35:02 UTC 设置，GitHub API exact readback 4/4 与用户确认 tuple 一致；workflow id `283278914` 于 07:27 UTC 禁用、API `disabled_manually`，无待/运行 job。`premerge`：安全卡先交付、CM-10/11 各自重基复核；本地/mock 负例证首写前 secret→独立 vars 比对、D1/R2 只读、trigger/schema/lease 预检、全库 SQL/Time Travel bookmark `age` 加密 runner 外 artifact 上传读回、global/逐例 manifest、失败非零/no-skip；只阻合入，不要求 release SHA 或不可读真实 secrets。`postrelease`：先将七项 Cloud repo secrets 迁受保护 environment、移除 repo 副本与 `base.yml` 默认注入，删除 schedule；独立 `cloud-live-recover.yml` 受保护恢复入口、custodian、dry-run，root 才能 enable/API 核 active。每卡自己的 release SHA 经受保护 `workflow_dispatch` nonce/run 授权，由 `genedna` 设置并回读精确 grant、无需独立 actor 审批；job 首步用仅 Variables-read token 最长 10 分钟实时 GET，严格绑定 run/attempt/ref/SHA/nonce 后才映射 Cloud 凭据并运行真实身份/备份/manifest、两个完整 target、写后善后与 artifact 后才可 D/complete。 | `vars-ready=passed; premerge=unknown; postrelease=unknown`。七项凭据迁移、protected environment、独立恢复与真实 release-SHA CI D 尚未执行；workflow 仍 `disabled_manually`，root 是恢复 owner。当前无真实 Cloud 写；不能以旧 `skip=true`、本地结果、旧 SHA 或被过滤目标替代。 |
| DEP-ACF-MIRROR | Agent Capture 架構前置 | 0902/0904/0905/0911 production 卡的 `DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接 | 已交接 |
| DEP-ACF-CAP | Agent Capture 必要而非充分條件 | `DEP-ACF-CAP` 的 ACF 側必要條件已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；CAP-01..03 僅可 zero-raw-persistence 重核，CAP-04..07 另須獨立 security/privacy RFC 與新卡，維持 `blocked` | 生效（ACF 側已交接；CAP-04..07 仍 `blocked`） |
| DEP-ACF-DM06 / DEP-DM-06 | Agent session 語義契約 | 正式 handoff 已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）完成。0926 DM-05 開工日仍須按 ER-02 重核 terminal、explicit CLI resume、live reactivation、import reactivation | 已交接；DM-05 目前 `pending` |
| DEP-CP-01 / DEP-CP-07 | External prerequisites | Authorized backend cf documentation access; isolated env/nextest/five-shell validation | Unverified; block applicable acceptance, no access inferred from plan |
| DEP-CP-02 | File exclusion | Completion CLI edits join DEP-CLI-mirror reservations | Pending per-card reservation |
| DEP-CP-03 | Read contract | Completion config reads must not trigger plan-20260919 migration | Pending read-only API and legacy-only fixture evidence |
| DEP-CP-04 | Release/window exclusion | Respect plan-20260921 REL-VG-01 file reservation | Verify before intersecting edits, not only before version bump |
| DEP-CP-05 | Contract/file exclusion | issues/476/478/480 command evolution and overlapping files | Refresh actual parameters and reserve conflicting files |
| DEP-CP-06 | Agent read contract | plan-20260819 models and identity-scoped readers | Verify before CP-13..15; does not block static stage |
| DEP-AD-12 / DEP-CLI-mirror | 跨计划写集互斥 | `src/cli.rs` 三态串行：plan-20260918 OI-05、plan-20260904 CX-30、plan-20260912 MB-03/05、plan-20260916 CAP-07、issues/483 CO-03/04、plan-20261001-mega-browser-noninteractive MN-03（仅改 `src/cli.rs:2006-2009` 注释，`DEP-MN-06`）、issues/500 CS-13 | 生效；OI-05、MN-03、CS-13 开工前必须核对 |
| DEP-GCX-02 | 跨计划写集互斥 | plan-20260919 与 plan-20260918 的 `COMPATIBILITY.md`/docs/网站页串行 | 生效 |
| DEP-FL-04 | 跨计划前置 | plan-20260913 依赖 plan-20260907 完整收口 | **已满足**（plan-20260907 已收口，B3-00..B3-17 全 `done`/`complete`） |
| DEP-CC-05 | 跨计划前置 | plan-20260905 CC-02..06 依赖 plan-20260904 全部非延后卡完成 | plan-20260904 未启动 |
| DEP-SCAP-01 | 跨计划前置 | 改变 AgentTraces ingest 的 state 字符串或 checkpoint 类别的卡，等待 plan-20260925 SCAP-01 `done`/`complete` 后改 `decide`。import 与 session stop/resume 不在此列 | SCAP-01 已 `done`/`complete`；后续采集卡可改 `decide` |
| DEP-SCAP-03 | 跨计划写集 | OG-11、OG-05、CX-06、CX-28、CX-02、CX-07、CX-19、CX-09、CX-26、CC-02、CC-05、PI-04 在 SCAP-01 complete 前不得 in-progress | SCAP-01 已 `done`/`complete`；名单卡可开工（仍受各自计划门控） |
| DEP-SCAP-04 | 跨计划写集互斥 | plan-20260925 SCAP-01 与 plan-20260907 B3-00 不得同时改 `runtime.rs`。SCAP-01 C 组落地后互斥放开 | SCAP-01 已收口；互斥解除 |
| DEP-SBX-06 | 内部发布延后 | plan-20260830 SBX 发布步 | 未就绪 |
| DEP-05 | 内部前置（已承接） | plan-20260822 v1-boundary runtime cutover（branch/sequencer/worktree repair/v1 op restore → v2 middleware）；由 OL-15A 承接，OL-15 依赖其完成 | PR #503 OL-15A 已完成 |
| DEP-WT-09 | 跨计划前置 | issues/476 关闭依赖 plan-20260918 WT-05..07 完成 | plan-20260918 未到 WT |
| DEP-FM-06 / DEP-FM-07 | 跨计划前置 | issues/470 关闭依赖 plan-20260918 FM-03/FM-04 完成 | plan-20260918 未到 FM |
| DEP-AD-06 / DEP-AD-07 | 跨计划写集互斥 | plan-20260918 与 issues/483（CO）、issues/490（SW）、issues/479（PL）的 `add.rs`/index 写集串行 | 生效 |
| DEP-PL-04 | 跨计划写集互斥 | issues/479 与 plan-20260918 IA-01/IA-02（`add.rs`）串行 | 生效 |
| DEP-CO-04 | 跨计划写集互斥 | issues/483 CO-03/04 与 plan-20260904 CX-30（`src/cli.rs`）三态串行 | 生效；CO-03/04 开工前核对 |

---

## 七、完成计划清单（已收口）

日期计划：`plan-20260708`、`plan-20260713`、`plan-20260714`、`plan-20260715`（历史完成、Code 产品面已拆除）、`plan-20260818`、`plan-20260821`、`plan-20260824`（历史完成、Code 产品面已拆除）、`plan-20260825`（历史完成、PS 产品轴已拆除；TA 测试轴保留历史）、`plan-20260827`、`plan-20260901`、`plan-20260910`、`plan-20260917`、`plan-20260920`。

Issue 计划：`issues/477`（31 卡，v0.22.49）、`issues/486`（AB-01，v0.22.31）、`issues/497`（CD-01..CD-04，v0.27.2；PR #581 squash merge `859d7fb`）、`issues/498`（TT-05/TT-02，v0.30.22 / v0.30.23；#498 CLOSED）。各自完成判据见对应计划文件。

### 2026-10-07：FIX-RG-SSH-01 独立0.30.35发布提案（审批前）

plan-20260904新增ER10前驱FIX-RG-SSH-01，in-progress/locally-accepted，7/7AC、3/3VER。实际main c0eaae5/package0.30.34上的test-only SSH及版本面0.30.35五文件候选macOS全量9507/9507 PASS（13slow、2LEAK、5skip、817.873s、exit0）；SSH族32/32仅single-threaded diagnostic，旧并发31/32历史保留。两LEAK各三次无重试孤立PASS、根因未明；完整R2 blob diff及installer LF/未改hook EOL/PTY parent边界见任务卡。R2限定发布提案只追加本卡和本状态段，不引入尚未交付的RG/OG实现或aggregate dirty写集；七文件最终树C组未执行、独立双审材料修订待复核，未commit/push/PR/merge/tag/release，不能记done/complete。Linux功能由用户后验；新版本不是tag预留，发布前重新核对actual refs。

### 2026-10-07 14:00:42 UTC：用户指定#618顺序后的当前0.30.36发布候选

用户明确要求「上游已经有一个PR更新到0.30.35，#618编号，请顺延发布新的版本」；当前active版本改为0.30.36，0.30.35留给#618。该指令授权当前独立FIX-RG-SSH-01发布及必要签名提交/推送/PR/checks/merge/tag/release步骤，先完成具体候选验收与双审，不再把旧“未获发布授权”的历史状态当当前决定。只处理本卡切片，保留root其它dirty及native Source A NO-GO；Linux功能仍由用户后验。

最新只读事实：#618 open/head5a5e5e2b4dca21e2e1e7357075c7f806286291f9当前三版本面仍0.30.34；main仍c0eaae5d9bf4e9cfae035d61b759b0ac80799db7，refs查询未返回v0.30.35/v0.30.36。0.30.35是按用户发布顺序留给该PR，不冒称已有tag或已合并。本0.30.36树目前只是c0eaae5上provisional准备；正式C组必须重新pin到#618实际合入且package0.30.35的main，按真实patch+1重新检验，并等待其发布窗口结束后发布本卡。不得把未合并#618 migration夹带进本卡I或替别人merge该PR。

旧完整0.30.35五文件候选9507/9507绿保留为历史；追加卡/status后的七文件重验已因本次顺延仅向owned Nextest SIGINT，中止结果7396/9507 passed（10slow、3LEAK、5skip）、2111未运行、exit100、源1982/1982字节/mode MATCH，明确CANCELLED/SUPERSEDED而非PASS，未运行该轮release/private install。新0.30.36只改三版本面，Cargo metadata --offline由工具链刷新self package lock一行，无依赖变化；SSH代码逐byte相同。当前无正式C/D完成或publication事实。

### 2026-10-07T15:30:36.955151+00:00：0.30.36最终基线准备

实际merged main=9a92e7fbf3af34ce8cbf9b1a7a5b9574b18ef83f、package0.30.35，包含#618 merge=c384bc7757ccea6f47b70df2cdd72fb528860713；v0.30.35 tag=c384bc7757ccea6f47b70df2cdd72fb528860713，唯一release workflow=37641579067 completed/failure，先前窗口已结束；不把其失败写成绿色。本候选是该真实基线上的0.30.36 patch+1；此前c0/package0.30.34只是provisional历史，未用旧全量绿代替本树验收。

仅SSH cfg(test)期限切片、三版本面与toolchain self-lock、FIX卡/status文档，既有upstream代码不成为本卡I写集。完整patch zero-offset/fuzz准入，尚待本最终冻结树的focused/full/fmt/strictClippy/build/install及fresh独立双审、签名commit/push/PR检查/tag/release/D证据；不得提前complete。用户已授权0.30.36发布，Linux功能仍由用户后验；Source A native NO-GO及其它任务卡门不变。

上游.35 tag与package补交提交不一致的事实独立保留；本卡既不改写他人tag也不冒认该窗口绿色。窗口terminal后前滚发布自有0.30.36；发布前重新核对main/tag与串行占用。


### 2026-10-07 22:04:44 UTC：FIX-RG-01 最终基线与全量事实

R2 FIX-RG-01当前独立0.30.38候选基于实际上游84442ee658bff007f9eed5b7b996638990610b45；保留CP-00已发布四路径，不回退上游审计交付。修复owned仅两cfg(test)期限、三version面/工具链lock、本卡plan及本status。R1旧f59 full与originals冻结保留，不宣称最终全量/C-D完成。原审两个P3修正为caller-supplied reader deadline和六特有AC/8、五VER/8，强制full另列，.37保留。本1985文件冻结源的fmt/strict/focused与双环境默认并发Nextest已结束：exit0、9514/9514passed、13slow、4LEAK、5skip、0flaky，817.894s；四LEAK根因未明，待逐项隔离复验，不宣称零泄漏。两原审已各自CLOSED PASS；只在full结束后修正本卡I五处RPC参数事实及本状态UTC标题，1983非记录文件与模式保持一致。新文档双审、最终树绑定、build/install/签名提交及实际C/D仍未完成；本卡继续in-progress/空。


### 2026-10-08 03:40 UTC：#620 发布前置已满足，#621 合并后重新验收

使用者要求的顺序已满足：#620 merge 125657b 与 v0.30.37 release run 37721401960 八作业全部 success。#621 私有候选从 a242401 正常合并实际上游 e95b8bd（含 DM-02 六新增文件及发布状态），版本四面冲突保留 0.30.38；签名 DCO merge 742517d。原 9514/9514 与旧双审不代替新增功能合并后的 full/fresh 双审，当前均待执行；FIX-RG-01 继续 in-progress/空，实际 C/D 及发布未完成。


### 2026-10-08 03:57 UTC：#621 合并后本地门通过，发布仍未完成

FIX-RG-01 R4实际1991源逐门前后MATCH：fmt/strict/聚焦与双环境默认并发Nextest exit0、9522/9522、13slow/1LEAK/5skip/0FLAKY（798.323s测试、832.229s进程）。唯一LEAK为未改merge_ext_driver_clean_result_is_read_from_percent_a，一次精确隔离1/1无LEAK，根因未明、不称修复。独立Codex/Claude两原稿CLOSED PASS，Claude既有RPC注释一P3非阻断；两原稿先关闭后父级完整读取，final树/文档门和构建/安装/签名、实际PR CI/merge/release八作业与线上安装尚待关闭。full之后仅本卡两处记录变更，1989非记录源字节/模式不变。旧9514原记录不改，本卡in-progress/空。

### 2026-10-08 10:41:48 UTC：FIX-RG-01 #621 已合并，0.30.39 独立发布候选本地门通过

#621 由 genedna 于 09:46:06Z squash merge 为 main 913efe3d60ca0a714eac240e75059e7f03fbdd8e，12 项检查 SUCCESS；v0.30.38 已被 DM-13（99c4bce）占用，故本卡 patch+1 顺延 0.30.39。候选分支 release/fix-rg-01-v0.30.39 仅改三版本面与 self-lock 一行；源码与 FIX-RG-01 私审第 6 轮（R6）私有合并 f224a0be diff 0 行（R6 运行树 = f224a0be + 同一未提交 version-only bump），R6 full 9530/9530（13 slow/1 flaky/1 leaky/5 skip）按 version-only 例外复用。本树 fmt/strict clippy/`cargo nextest run --test compat_version_surface_sync` 2/2/`cargo build --locked --release`（实测 libra 0.30.39、91702144 bytes、SHA-256 ab95ea24a900cc09493d76d4a8ba1314b3f233cd27dd61562a0acf53e0cefab4）全部 exit 0；R6 full 原始日志与 R6 原稿为本机路径，SHA-256 登记于 plan-20260904 同时间记录。R6 Codex 6 P1 均属上游 memory 模块，登记 DEP-MEM-R6-HANDOFF 移交 plan-20260926；fresh 双审、签名提交、push/PR/CI/merge/tag/release/安装 smoke 待完成，FIX-RG-01 仍 in-progress/locally-accepted。执行会话切换为 Claude Code（执行者陈述：当前唯一会话、承接 ER-12 publisher）。

### 2026-10-08 10:48:46 UTC：FIX-RG-01 0.30.39 候选 delta fresh 双审最终双 PASS

四轮只读独立审查（Codex gpt-6.1-sol / Claude claude-fable-5-1，彼此不读对方报告，每轮前后 1994 跟踪文件清点零漂移）：r1 双 FAIL（概览行卡态无树内支撑、证据附件缺失）、r2 Claude PASS/Codex FAIL（概览行改写基线）、r3 Claude PASS/Codex FAIL（intake 时间戳过期）、r4 **双 PASS**（Codex P0/P1/P2/P3=0；Claude P0/P1/P2=0）。各轮报告 SHA-256 登记于 plan-20260904 同时间记录。随后执行签名 DCO 提交/push/PR/CI/merge/tag/release/安装 smoke；FIX-RG-01 仍 in-progress/locally-accepted。

### 2026-10-08 12:49:33 UTC：NPR-19 / FIX-NPR-CONFIG-01 0.30.40 发布候选树已组装（未发布）

基线公开 main 9b8fc5c7057e6e317edd88f1518ba1bb25f2929f（#624 已 squash merge；FIX-RG-01 0.30.39 的 release run 37777413227 当时仍在进行中）；owned 7 文件与 compose 树逐字节一致（实现源相同，compose 树的双 env full 9539/9539 按 version-only 例外复用）；三版本面 0.30.40。**本次 macOS 门与审查（均在实现源相同的 compose 树上完成，最终树仅加版本面与 plan 记录）：** `cargo +nightly fmt --all --check` 0；`cargo clippy --all-targets --all-features -- -D warnings` 0；NPR-19 四具名门（`npr_19_literal`/`npr_19_case`/`npr_19_exact` 3/3、`npr_19_cli_registry` 1/1）与 FIX-NPR-CONFIG-01 五具名门（CLI 3/3、unit 2/2）全过；`command::config_test::` 177/177；`compat_command_docs_examples_section`/`compat_error_codes_doc_sync` 3/3；双 env 全量 `cargo nextest run --all --no-fail-fast --retries 2` 9539/9539（22 slow、1 leaky `command::branch::tests::test_format_branch_name_with_full_remote_ref` retries 0 隔离另记、5 skipped、964.871s，source manifest `68f8b0ac2c39809e57f4bb1a05805f169ef529574db62a78bdee6b0784067749`）。fresh 独立双审 r1（代码+文档 delta）：Codex PASS 0/0/0/0（SHA-256 `00bc7b3f3fbd60e2567a4d855e9cd4cda72c33ac828deed628dc19af16c5312b`）、Claude PASS P3=3（SHA-256 `c75fda07c12859aad5b850cee10b158849ea94e08ef90c153078d0deea5d260f`）；三项 P3 已采纳（import 命令错误边界复用 `config_plaintext_replacement_error()`、rollback 失败串含原拒绝原因、`--plaintext` 表格/COMPATIBILITY 措辞与 import 节各一句）并重跑上述门；Codex 另列三项基线既有 inherited P1 已登记 DEFER 候选（plan-status 五·5.4）。网站 cf 阶段（882a850 + 三已审 backend 提交）typecheck/build 0、DOCFIX 10/10。最终树的 fresh 双审（含版本面与本记录）见后续登记。 fresh 双审、签名提交、PR/CI/merge、tag/release、安装 smoke、cf 推送/线上验收均待完成。

### 2026-10-08 13:12:40 UTC：FIX-RG-01 done / complete（v0.30.39 公开发布，正式进度 1/61）

PR #624 squash merge 9b8fc5c7057e6e317edd88f1518ba1bb25f2929f（2026-10-08T12:28:30Z by genedna (squash)）；签名 tag v0.30.39 → 9b8fc5c7057e6e317edd88f1518ba1bb25f2929f；release.yml run 37777413227 八任务 success；Release 发布 GitHub Release 页面待用户以 tag v0.30.39 创建（本会话 `gh release create` 被执行环境分类器拒绝）；四平台 artifact、CDN install 脚本、Homebrew formula、Ed25519 stable manifest 均已由 release run 发布；CDN DEFAULT_VERSION=v0.30.39 (install.sh) / v0.30.39 (install.ps1)；Homebrew 0.30.39 @ 6011b20d4b9f4c057c7840ffe2b389280384fe2b；darwin-arm64 76743008 bytes / d2c44e5a01fd3f353650dae455893eaf3e840101843e12227c77614fcfcadc6d；默认安装器隔离 smoke `libra 0.30.39` PASS。下一卡 NPR-19。详见 plan-20260904 同时间记录。

### 2026-10-08 14:51:55 UTC：NPR-19 / FIX-NPR-CONFIG-01 最终树事实更新（取代 12:49:33 记录中的 compose 树数字）

最终树 release/npr-19-v0.30.40 @ 9b8fc5c7（含 r2–r4 审查前滚：guard 错误分类器、import 回归测试、key-aware 拒绝）：fmt 0、strict clippy 0、具名 14/14、`command::config_test::` 178/178、docs/version 守卫 5/5、双 env full **9544/9544**（0 flaky/0 leaky、5 skipped）；文本修订后 docs/version/agent-docs 守卫 14/14、config_test 178/178 复跑。fresh 独立双审 r5：Codex PASS 0/0/0/0、Claude PASS P3=3（已采纳）。随后签名提交/push/PR；CI/merge/tag/release/安装/cf 仍待。

### 2026-10-09 02:51:00 UTC：NPR-19 自己的前滚 C/D（未 complete）

隔离方式：`libra worktree add -b release/npr-19-v0.30.42 /Volumes/Sea/libra-npr19-cd origin/main`。脏 main 892f37c 未提交、未 reset。不重发 v0.30.40；v0.30.41 不关闭 NPR-19。

版本面 0.30.42。fmt exit 0。clippy --all-targets --all-features -D warnings exit 0。compat_version_surface_sync 2/2。四具名门各 1 passed。full nextest 9545 passed / 0 failed（1243.572s；2 flaky 重试后过；2 leaky；5 skipped）。release binary `libra 0.30.42`。

网站 cf 源 commit acc2635 已推 origin/cf。DOCFIX vitest 10/10，typecheck 与 build exit 0。wrangler 未登录，未执行 deploy。线上 https://libra.tools/en/docs/commands/config 在 deploy 前为 HTTP 200，body 没有 “Keys are literal”。

PR、CI、merge、tag、release.yml、发布后安装 smoke、线上 cf 仍开放。NPR-19 保持 in-progress / locally-accepted。FAST-RG-01 仍被未采用的 FIX-RG-SCOPED-04 挡住。

### 2026-10-09 04:40:47 UTC：NPR-19 merge、tag、release（未 complete）

PR #627 squash merge `4a557df5163d7b3ebed50ca53859498774bf37d8`。Check run https://github.com/libra-tools/libra/actions/runs/37876625067 conclusion success。CodeQL PR https://github.com/libra-tools/libra/actions/runs/37876625016 conclusion success。main CodeQL https://github.com/libra-tools/libra/actions/runs/37882008811 conclusion success，headSha 同为该 merge。tag `v0.30.42` 指向该 merge。release.yml https://github.com/libra-tools/libra/actions/runs/37882123871 conclusion success，八个 job 全 success。

隔离安装 smoke 得到 `libra 0.30.42`，sha256 `5a440a9dab7da87a5b33a1de070346b4af642e8c0d8f355bf03c86c9b2b3c426`。用户目录里的 libra 仍是 0.30.39。

线上配置页仍未部署。`wrangler whoami` 未登录，所以没有执行 deploy。NPR-19 保持 in-progress / remote-pending，不是 done/complete。FAST-RG-01 未开始。FIX-RG-SCOPED-04 未采用。

### 2026-10-09 04:55:03 UTC：NPR-19 线上配置页已交付

https://libra.tools/en/docs/commands/config 返回 HTTP 200，`content-type: text/html; charset=utf-8`，页面 `lang=en`，正文含 “Keys are literal”。NPR-19 改为 done / complete。其 C/D 所承载的 FIX-NPR-CONFIG-01 与 FIX-RG-DOC-TEST-01 同改 done / complete。

下一发布点 FAST-RG-01 仍被 DEP-FAST-RG-IN 挡住：四 scoped fix 的 fresh 设计采用和 local deliverable 未就绪。未开始 FAST-RG-01，未采用 FIX-RG-SCOPED-04，未放开 DEP-OG-02。

### 2026-10-09 05:17:20 UTC：采纳 FIX-RG-SCOPED-04

人类采纳既有 FIX-RG-SCOPED-04，不新造跨进程协议。macOS 四具名门 4 passed。fmt 与 strict clippy exit 0。Windows `compat-scoped-input-windows` 已写入 base.yml，实跑 UNRUN。修订双审未执行。FIX-RG-SCOPED-02、FIX-RG-SCOPED-03 和 FAST-RG-01 未开始。

### 2026-10-09 05:19:40 UTC：FIX-RG-SCOPED-04 PR

2026-10-09 05:19:14 UTC：PR https://github.com/libra-tools/libra/pull/628 head 7e10632d490ad8fe1ab90e319defc442dabad355。base.yml run https://github.com/libra-tools/libra/actions/runs/37888011474 ；compat-scoped-input-windows 状态 IN_PROGRESS，job https://github.com/libra-tools/libra/actions/runs/37888011474/job/113682280952 。未称该 Windows job 通过。修订双审未执行。未开始 FIX-RG-SCOPED-02、FIX-RG-SCOPED-03、FAST-RG-01。

### 2026-10-09 05:53:41 UTC：SCOPED-04 Windows job failure

2026-10-09 05:53:41 UTC：当前 head 的 compat-scoped-input-windows 结论 failure。run https://github.com/libra-tools/libra/actions/runs/37888104494 job https://github.com/libra-tools/libra/actions/runs/37888104494/job/113682968003 。cargo test --locked --lib --list 未能编译 lib test，37 个错误，无 checkpoint_input.rs；含 opencode_export.rs 找不到 fake_exporter，以及多处仅 Windows 的 unused import。不是本卡 confined API 断言失败，未改那些无关文件，不称 Windows 通过。前一 run 37888011474 的同名 job 被 cancelled。修订双审未勾。未开始 02/03/FAST-RG-01。

### 2026-10-09 06:23:51 UTC：Windows lib test 编译修复

2026-10-09 06:23:51 UTC：只修 Windows 上 `cargo test --locked --lib --list` 在 `-D warnings` 下的 37 个编译错误。仅 Unix 使用的 import 收到 `#[cfg(unix)]`；Windows 提前返回的函数加 `cfg_attr` allow；`runner_controls_preserved` 改为 `unix` 且非 macOS，使 `#[cfg(unix)]` 的 `fake_exporter` 可见。未改 `checkpoint_input.rs` 行为。macOS `cargo clippy --all-targets --all-features -- -D warnings` exit 0；`cargo test --lib fix_rg_scoped_04_ -- --test-threads=1`：4 passed / 0 failed / 11.99s。该通过不是 Windows 通过。本提交推送后的 Check workflow 才是 `compat-scoped-input-windows` 证据，提交时结论未知。修订双审未勾。未开始 02/03/FAST-RG-01。

### 2026-10-09 06:30:39 UTC：编译修复的 Check run

2026-10-09 06:30:39 UTC：编译修复 head 6da86e8cbb64621b3ad70e57a81d099eb29da944 已在 origin/fix/rg-scoped-04。Check run https://github.com/libra-tools/libra/actions/runs/37893476162 。查看时 compat-rustfmt success；compat-scoped-input-windows job https://github.com/libra-tools/libra/actions/runs/37893476162/job/113699736070 为 in_progress，不称 Windows 通过。本会话 macOS 复跑 `cargo clippy --all-targets --all-features -- -D warnings` exit 0，`cargo test --lib fix_rg_scoped_04_ -- --test-threads=1` 为 4 passed / 0 failed / 11.76s。run 37888104494 的 compat-offline-core 与 compat-network-remotes 注释为 concurrency cancel-in-progress，不是测试断言失败，未扩 scope。修订双审未勾。未开始 02/03/FAST-RG-01。未标 FIX-RG-SCOPED-04 done/complete。

### 2026-10-09 07:08:04 UTC：Windows job 再次失败于 -D warnings

2026-10-09 07:08:04 UTC：run https://github.com/libra-tools/libra/actions/runs/37893476162 的 compat-scoped-input-windows job https://github.com/libra-tools/libra/actions/runs/37893476162/job/113699736070 结论 failure。setup-rust-toolchain 默认注入 `RUSTFLAGS=-D warnings`，`cargo test --locked --lib -- --list` 在四具名夹具之前失败：52 个错误，均为 Windows 上未调用的 Unix helper（dead_code / unused import），`throw 'Listing the scoped Windows fixtures failed'`。不是四夹具断言失败，不称 Windows 通过。本卡 job 将该 action 的 rustflags 置空，并在夹具步骤显式 `RUSTFLAGS=""`，使 `--list` 不再被这些既有 Unix helper 挡住。Linux clippy 步骤仍自带 `-D warnings`。未改 checkpoint_input.rs。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-09 07:46:11 UTC：Windows lib test 在 -D warnings 下无本仓警告

2026-10-09 07:46:11 UTC：撤回夹具步骤对 `RUSTFLAGS` 的清空，Windows job 重新使用 setup-rust-toolchain 默认的 `-D warnings`。Unix 专用、Windows lib test 不会调用的项加上 `#[cfg_attr(windows, allow(dead_code))]`，未使用的 re-export 加上 `#[cfg_attr(windows, allow(unused_imports))]`。未改这些函数在 Unix 上的行为，未改 checkpoint_input API。本机 `RUSTFLAGS="-D warnings" cargo test --locked --target x86_64-pc-windows-gnu --lib --no-run` exit 0，libra 自身无 warning。macOS `cargo clippy --all-targets --all-features -- -D warnings` exit 0。`cargo test --lib fix_rg_scoped_04_ -- --test-threads=1`：4 passed / 0 failed / 11.89s。该本机 GNU 通过不是 GitHub `windows-latest` 通过。依赖 `proc-macro-error2` 的 future-incompat 提示仍会打印，不是本仓 lint。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-09 08:26:44 UTC：Windows 四夹具通过，仍有链接警告

2026-10-09 08:26:44 UTC：compat-scoped-input-windows job https://github.com/libra-tools/libra/actions/runs/37901058709/job/113723675272 结论 success。run https://github.com/libra-tools/libra/actions/runs/37901058709 head 29a7c6cf08738ed18403698be8f485088f5eeb40，merge 27dab0616d6cfe8bd6dd1e05671e603dd0e979ce，base 426d2165cf9fd03fad629bf106515051c73b825a。步骤环境 `RUSTFLAGS=-D warnings`。四夹具各 `test result: ok. 1 passed; 0 failed; 0 ignored`（0.02s、0.15s、62.37s、0.22s）。同一次链接仍打印 `LNK4099`（openssl-sys 静态库缺少 `ossl_static.pdb`）以及 `libra (lib test) generated 1 warning`，另有 `proc-macro-error2` v2.0.1 的 future-incompat（E0365，crates.io 最新仍是 2.0.1，无法靠升级去掉）。本卡 job 在保留 `-D warnings` 的同时追加 `-C link-arg=/IGNORE:4099`，只压这一条 MSVC 链接警告。新 run 尚未出结果，不把本段写成下一次 Windows 通过。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-09 10:02:57 UTC：合并父提交与事件基线不一致

2026-10-09 10:02:57 UTC：run https://github.com/libra-tools/libra/actions/runs/37913022407 job https://github.com/libra-tools/libra/actions/runs/37913022407/job/113762449766 在夹具之前失败。检出的 merge b753f2fd7257219190115e204bc9e625db67c0e2 的父提交是 acc865ff650d28faa656d4af7c5601da097320a0 与 head 015537552cd6ddbe67b7f60c9f929ccae121d79a。事件里的 base 仍是 426d2165cf9fd03fad629bf106515051c73b825a，main 在事件快照之后前进了。脚本因此抛出 Pull merge parents do not match the actual PR base and head。不是四夹具失败，也不是 LNK4099。现改为：第二父提交必须等于 PR head；第一父提交是 GitHub 实际合并的基线，事件 base 落后时只记录、不失败。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。未标 done/complete。

### 2026-10-09 10:39:11 UTC：新基线校验后的 Windows 四夹具

2026-10-09 10:39:11 UTC：run https://github.com/libra-tools/libra/actions/runs/37915284178 job https://github.com/libra-tools/libra/actions/runs/37915284178/job/113770285993 结论 success。head 7a18553d8ebc4aa548c61043c6b3cc6cb4b40f15，merge a73f687063dc7c128f4d021574b2e6228cb9b42b。日志写明 Event base 426d2165cf9fd03fad629bf106515051c73b825a lagged the merge base acc865ff650d28faa656d4af7c5601da097320a0，然后继续测试。四夹具各 `1 passed / 0 failed / 0 ignored`（0.02s、0.18s、56.44s、0.21s）。LNK4099 与 `generated 1 warning` 均为 0 次。`proc-macro-error2` future-incompat 按决定未处理。同一次 Check 的 compat-offline-core 与 compat-clippy 查看时仍 in_progress，本记录不称整次 run 通过。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。未标 done/complete。证据先留在本地，避免取消仍在进行的 Check。

### 2026-10-09 11:25:15 UTC：compat-offline-core 失败于 live reservation deadline

2026-10-09 11:25:15 UTC：run https://github.com/libra-tools/libra/actions/runs/37915284178 已 completed，结论 failure。失败 job 是 compat-offline-core，https://github.com/libra-tools/libra/actions/runs/37915284178/job/113770286360 ，失败步骤 `Run tests (L1 + L2 + L3)`。nextest：9591 项中 9590 passed、1 failed、7 skipped，进程 exit 100。唯一失败是 `internal::ai::subagent_content::tests::subagent_content_live_reservation_deadline_returns_retryable_error_not_skip`，三次尝试都在当时的 `src/internal/ai/subagent_content.rs:6733` 断言 `rendered.contains("another writer")`。该文件相对 origin/main 为 0 行差异；30ms 预算来自 60c40df2（2026-10-03）。安静本机用原 30ms 预算 1 passed / 0.16s。并行负载下这 30ms 在第一次 reservation probe 内耗尽，错误是裸的 command deadline，还没有 “another writer”。修复：捕获预算改为既有 5 秒 `test_mutation_deadline()`（仍短于种下的 60 秒租约）；已经观察到 live writer 之后，下一次 probe 的 deadline 错误同样带上这条可重试文案。本机复跑该测试 1 passed / 5.14s。`cargo clippy --all-targets --all-features -- -D warnings` exit 0。未跑全量 nextest。`proc-macro-error2` 2.0.1 future-incompat 仍只打印，未处理。未标 FIX-RG-SCOPED-04 done/complete。修订双审未勾。未开始 FIX-RG-SCOPED-02、FIX-RG-SCOPED-03、FAST-RG-01。未把未跑的 Linux 门标成 PASS。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-09 09:25:29 UTC：IGNORE:4099 之后四夹具通过且无 LNK4099

2026-10-09 09:25:29 UTC：compat-scoped-input-windows job https://github.com/libra-tools/libra/actions/runs/37905150212/job/113737033866 结论 success（completed 2026-10-09T08:58:48Z）。run https://github.com/libra-tools/libra/actions/runs/37905150212 当时仍 in_progress。head 469a426e404314869d457dd2bbe83a3a66810dc5，merge b30c0acbc3bdeb4fdeceb1a637ba9937f6d3799f，base 426d2165cf9fd03fad629bf106515051c73b825a。步骤 `RUSTFLAGS=-D warnings -C link-arg=/IGNORE:4099`。四夹具各 `test result: ok. 1 passed; 0 failed; 0 ignored`：readonly_cleanup 0.02s，metadata_boundary 0.14s，preflight_budget 67.72s，deadline_cancel_owner 0.28s。日志中 `LNK4099` 出现 0 次，`generated 1 warning` 出现 0 次。未标 FIX-RG-SCOPED-04 done/complete。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。
