# 计划执行状况总表（plan-status.md）

> **本文件是全仓计划的单一执行状况视图**，按任务卡粒度汇总每一份计划的执行状态，并登记计划内的延后决策/实施项（`DEFER-*`）与跨计划依赖。**任何执行计划的 Agent 在完成/推进一张任务卡时，必须在同一变更中同步更新本文件**；新建计划时必须在「计划一览」登记一行，并把本文件的更新义务写入新计划的「使用规则」或修订历史。
>
> **维护规则（强制）**
>
> 1. 每张卡的状态推进（`pending` → `in-progress` → `blocked` → `done`，`Acceptance` 随 ER-04 转移）都在「计划一览」的对应行更新，并附发布版本 / commit / 日期。
> 2. 计划收口、拆卡、合并发布、新增 `DEFER-*`、`DEP-*` 状态变化，同步更新「延后与未决策项」与「跨计划依赖」两节。
> 3. 新建计划：在「计划一览」加一行（类别、状态、一句话进度），并在「未启动计划」或「实施中计划」小节落位。
> 4. 以「计划一览」表为权威，其余小节是它的展开视图；冲突时以任务卡自身 `Lifecycle / Acceptance` 与 `plan-long.md` 的日期索引交叉核对。
> 5. 状态快照日期见本文件头；每次更新必须把日期改到当天。
>
> **当前快照：** 2026-09-21（下次更新时替换）。

---

## 一、计划一览

状态列取值：`未启动` / `实施中` / `已收口` / `已排期`（设计计划，尚未执行）。

| 计划 | 类别 | 状态 | 一句话进度（卡片状态） |
|---|---|---|---|
| [`plan-20260918.md`](plan-20260918.md) | 横切（`add` 命令收口） | **实施中** | OI-01..03 `done/complete`（v0.23.4/5/6）；**OI-04 `in-progress`（v0.23.7）**；OI-05..WT-07 `pending` |
| [`plan-20260919.md`](plan-20260919.md) | 横切（global 配置迁 XDG） | 已排期 | GCX-01 `done/complete`（v0.23.1）；GCX-02/03/04 `pending` |
| [`plan-20260920.md`](plan-20260920.md) | 横切（拆 Code/Publish/Worker） | **已收口** | RC-00..RC-36 全部 `done/complete`；完成判据全勾选；DEFER-RC-04/05/06/07 已关闭；RC-00 缝清单已并入正文附录 |
| [`plan-20260921.md`](plan-20260921.md) | 横切（GnuPG HOME 密钥导入仓库 vault） | 已排期 | 原 `plan-20260919-gpg-import.md`；R29 双 PASS；15 卡尚未执行 |
| [`plan-20260917.md`](plan-20260917.md) | 横切（cargo-test 进程内剥落） | **已收口** | SH-00..SP-01 五卡全部 `done/complete`；SP-00 结论文档已并入正文附录 |
| [`plan-20260916.md`](plan-20260916.md) | B（Mega agent capture-push） | 未启动 | CAP-01..07 全部 `pending`；双评审（Codex/Claude）已 PASS，尚未开工 |
| [`plan-20260913.md`](plan-20260913.md) | A（LR-09 FastCDC Media） | 未启动 | FL-00..FL-07 全部 `pending`；前置 plan-20260907 未收口 |
| [`plan-20260912.md`](plan-20260912.md) | B（memory boundary） | 未启动 | MB-06/09/12 已取消（`done`）；MB-01..05 `pending`；MB-07/08/10/11 `blocked` |
| [`plan-20260911.md`](plan-20260911.md) | B（pi capture / hook boundary） | 未启动 | PI-01..06 全部 `pending`；Claude Code 429 未给出 verdict，**禁止开工** |
| [`plan-20260910.md`](plan-20260910.md) | 横切（数据库迁移作用域） | **已收口** | MIG-00..MIG-06、MIG-R01..R03 全部 `done/complete` |
| [`plan-20260907.md`](plan-20260907.md) | 横切（BLAKE3 object format） | 未启动 | B3-00..B3-17 全部 `pending` |
| [`plan-20260906.md`](plan-20260906.md) | 横切（安全扫描） | 未启动 | SC-01..SC-07、SC-CLOSE 全部 `pending` |
| [`plan-20260905.md`](plan-20260905.md) | B（Claude hooks/reasoning） | 未启动 | CC-00..CC-06 全部 `pending` |
| [`plan-20260904.md`](plan-20260904.md) | B（Codex reasoning） | 未启动 | CX-00..CX-30（35 卡）全部 `pending` |
| [`plan-20260903.md`](plan-20260903.md) | A（LR-05 merge） | 实施中（收尾） | MG-01..MG-21 卡片全部 `done/complete`；最终计划收口与 deferred 差异仍待完成 |
| [`plan-20260902.md`](plan-20260902.md) | B（OpenCode artifact／memory） | 未启动 | OG-00..OG-15 全部 `pending` |
| [`plan-20260901.md`](plan-20260901.md) | 横切（SB-01 pkt-line fail-closed） | **已收口** | 全部卡 `done/complete`；最新 v0.22.47 |
| [`plan-20260830.md`](plan-20260830.md) | 横切（SB-02 sandbox export） | **已收口** | SBX-01..05 `done/locally-accepted`（发布步按 DEFER-SBX-06 延后）；ER-13 收口门绿 |
| [`plan-20260827.md`](plan-20260827.md) | 横切（SB-04 测试并行度） | **已收口** | NP-00..05 全部 `complete` |
| [`plan-20260825.md`](plan-20260825.md) | B（Code provider / RT-01 后续） | **已收口** | TA 系列全部落地；发布面按用户 2026-08-30 豁免闭合 |
| [`plan-20260824.md`](plan-20260824.md) | B（RT-01 延后项收口） | **已收口** | DF-01..09 全部 `done/complete`；v0.22.0 已发布 |
| [`plan-20260822.md`](plan-20260822.md) | A（LR-02/LR-03 Operation Log v2） | 实施中（PR #503 收口） | OL-01..13、CH-01..04 `done/complete`；OL-14 **已取消**（`web/` 拆除，2026-09-20）；OL-15A `done/complete`，OL-15 `done/remote-pending`（等待 compat-offline-core） |
| [`plan-20260821.md`](plan-20260821.md) | A（UP-01） | **已收口** | 客户端与 CI 全部落地；closeout `00bc815`；DEFER-02..06 残留 |
| [`plan-20260819.md`](plan-20260819.md) | C（MEM-01/02 Memory） | 实施中 | **M2-01 `in-progress`**；M2-02..M2-15 全部 `pending`（M2-15 为发布点） |
| [`plan-20260818.md`](plan-20260818.md) | B（deepseek-harness bridge） | **已收口** | LB-01..07 全部 `done/complete`；protocol v1 20-method 全实现 |
| [`plan-20260729.md`](plan-20260729.md) | A（CT-01） | 实施中（收尾） | CT4-01 发布卡已执行（v0.21.21）；**CT3-07 `blocked`/已延后**；完成判据未全部勾选 |
| [`plan-20260715.md`](plan-20260715.md) | B（RT-01 Code Web-only） | **已收口** | W0..W6 主线 + W5-01 家族全部合入；完成判据全勾选；DEFER-01..10 残留（部分由 plan-20260824 承接） |
| [`plan-20260714.md`](plan-20260714.md) | A（UP-01、LR-01）+ 横切 | **已收口** | Part A 迁移至 plan-long；Part C W1..W4 勾选；Part D 残留由 LR 承接 |
| [`plan-20260713.md`](plan-20260713.md) | B（LR-06/07/10 捕获前置） | **已收口** | DR-BASELINE..DR-07 全部实现并收口 |
| [`plan-20260708.md`](plan-20260708.md) | A（LR-04/05/09 相邻基础） | **已收口** | 41 项主线全部实现；只保留历史记录，活跃残留另行排期 |

### issues/ 下的计划（Issue 驱动的 Git 对齐修复计划）

`issues/` 目录每份文件对应一个 GitHub Issue，是独立的可执行计划。`477` 已收口、`486` 已关闭、`476` 执行中（用户 2026-09-20 覆盖：执行期间不调用 Codex/Claude 评审）。其余多为设计计划。状态列取值同上。

| 计划 | Issue 主题 | 状态 | 任务卡 |
|---|---|---|---|
| [`issues/470.md`](issues/470.md) | 工作树物化丢失可执行位与 mode 变化检测 | 未启动 | FM-01/02/05（3 卡） |
| [`issues/473.md`](issues/473.md) | `init` 与 Git 对齐 | 未启动 | IN-01..IN-12（12 卡） |
| [`issues/474.md`](issues/474.md) | clone 浅克隆完整性、bundle 源、bare 与 mirror 对齐 | 未启动 | CL-01..CL-15（15 卡） |
| [`issues/475.md`](issues/475.md) | `config` Git 兼容参数层对齐 | 未启动 | CF-01..CF-15（15 卡） |
| [`issues/476.md`](issues/476.md) | 工作树命令族与 Git 对齐 | **实施中** | WT-02 `v0.23.29` / WT-04 `v0.23.30` / WT-08 `v0.23.31` / WT-09 `v0.23.32` / WT-10 `v0.23.33` / WT-11 `v0.23.34` / WT-01 `v0.23.35`（`done`/`remote-pending`）；WT-03 受 DEP-WT-08 阻塞；intent-to-add 已迁至 plan-20260918 |
| [`issues/477.md`](issues/477.md) | 历史改写命令族与 Git 对齐 | **已收口** | HF-01..HF-31（31 卡）全 `done/complete`，聚合发布 v0.22.49；子 issue #495 |
| [`issues/478.md`](issues/478.md) | log/show/diff/grep/blame/notes/reflog 命令族 | 未启动 | LG-01..LG-25（25 卡） |
| [`issues/479.md`](issues/479.md) | plumbing 与 Git 对齐 | 未启动 | EC-01、RV-01/02、UI-01..03、DF-01、SR-01、UR-01（9 卡） |
| [`issues/480.md`](issues/480.md) | remote/fetch/pull/push/credential/rerere 对齐 | 未启动 | HP-01..HP-16（16 卡） |
| [`issues/481.md`](issues/481.md) | 维护与杂项命令族对齐 | 未启动 | MX-01..MX-16（16 卡） |
| [`issues/483.md`](issues/483.md) | `count-objects` 与预览命令零对象写入 | 未启动 | CO-01..CO-04（4 卡；CO-03/04 受 DEP-CO-04 / CX-30 `src/cli.rs` 串行约束） |
| [`issues/486.md`](issues/486.md) | upstream ahead/behind 计数 | **已收口** | AB-01 `done/complete`（v0.22.31，已关闭） |
| [`issues/487.md`](issues/487.md) | 本地 Git 转换挂死与中断恢复 | 未启动 | IG-01..IG-05（5 卡） |
| [`issues/488.md`](issues/488.md) | `grep` 的 `--exclude-standard` 与子目录作用域 | 未启动 | GR-01/02（2 卡） |
| [`issues/490.md`](issues/490.md) | skip-worktree 索引位与 `add` 稀疏路径诊断 | 未启动 | SW-01..SW-07（7 卡；SW-06 已迁至 plan-20260918） |

---

## 二、未启动的计划与卡

以下计划尚无任何卡开工（设计态，全部 `pending`）。按建议优先级排列，优先级依据为跨计划依赖（`DEP-*`）与产品路线（`plan-long.md`）：

| 计划 | 全部待执行卡 | 开工前置条件 |
|---|---|---|
| [`plan-20260904.md`](plan-20260904.md) | CX-00..CX-30（35 卡） | Phase 0：Codex `PASS`；CX-30 受 DEP-CLI-mirror 三态串行约束 |
| [`plan-20260905.md`](plan-20260905.md) | CC-00..CC-06 | CC-02..06 依赖 plan-20260904 的 RG 卡完成 |
| [`plan-20260906.md`](plan-20260906.md) | SC-01..SC-07、SC-CLOSE | — |
| [`plan-20260907.md`](plan-20260907.md) | B3-00..B3-17 | Phase 0 冻结；是 plan-20260913 的前置 |
| [`plan-20260911.md`](plan-20260911.md) | PI-01..PI-06 | **Claude Code 429 未出 verdict，禁止开工** |
| [`plan-20260912.md`](plan-20260912.md) | MB-01..05（MB-07/08/10/11 已 `blocked`，MB-06/09/12 已取消） | 依赖 CAP（plan-20260916）等 |
| [`plan-20260913.md`](plan-20260913.md) | FL-00..FL-07 | **前置 plan-20260907 完整收口**（DEP-FL-04） |
| [`plan-20260916.md`](plan-20260916.md) | CAP-01..CAP-07 | 双评审已 PASS；开工时按 ER-CAP-02 pin 重核 |
| [`plan-20260919.md`](plan-20260919.md) | GCX-02/03/04 | GCX-01 已 `done`（v0.23.1）；GCX-02→03→04 串行；写集与 plan-20260918 串行（DEP-GCX-02） |
| [`plan-20260921.md`](plan-20260921.md) | VG-00..VG-14（15 卡） | R29 双 PASS；Phase 0 剩余 DEP 复核、`gpg --version` 证据、VG-00 go、ADR Accepted |
| `issues/` 设计计划 | 见「计划一览」issues 表 | 各计划 Codex review `PASS` 前不得开工；`issues/476`/`479`/`483`/`490` 与 `plan-20260918` 写集串行（DEP-WT-09 / DEP-PL-04 / DEP-AD-06 / DEP-AD-07） |

---

## 三、实施中的计划与当前卡

按执行窗口排序；当前唯一在跑的卡见「四、当前执行指针」。

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
| GCX-02 legacy 库自动迁移 | `pending` | — |
| GCX-03 全域 vault unseal key 随迁 | `pending`（依赖 GCX-02） | — |
| GCX-04 用户级 hooks 路径对齐 | `pending`（依赖 GCX-01） | — |

### 3.3 plan-20260819（Memory M2）

| 卡 | 状态 |
|---|---|
| M2-01 冻结 M2 合同与领域类型 | **`in-progress`** |
| M2-01K..M2-15（含 M2-15 发布点） | `pending` |

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

- **当前正在执行：** `issues/476` → `WT-01`（回归守卫），`done`/`remote-pending`，版本面三处 `0.23.35`（C 组提交/tag/`gh release` 进行中）。
- **下一步（WT-01 C/D 落地后）：** `issues/476` → `WT-03` 仍等 DEP-WT-08 / ADR-WT-04；无其它未阻塞卡。
- **并行窗口（不在本执行指针）：** `plan-20260918` 其余 add 卡、`plan-20260819` M2 仍登记为实施中，但不抢本卡的 `stash.rs` 写集。

---

## 四·零、零依赖可立即启动的任务卡（入度为零）

本表在每个「未执行」计划中列出**依赖图入度为零**（无内部前置卡）的任务卡，并标注它是否真的「可立即开工」，还是仍被计划级 review 门或跨计划 `DEP-*` 门控。判据：`开工许可 = 内部无前置 ∧ review 门已 PASS ∧ 外部/跨计划 DEP 已满足`。

| 计划 | 入口卡 | 卡要做什么（简述） | 内部前置 | 计划级 review 门 | 外部/跨计划门控 | 可立即开工 |
|---|---|---|---|---|---|---|
| [`plan-20260822`](plan-20260822.md) | （PR #503 收口；OL-14 已取消、OL-15 `remote-pending`） | — | — | — | — | ⏳ 等待 compat-offline-core 远端门禁 |
| [`plan-20260919`](plan-20260919.md) | GCX-02 | legacy 全局 config DB 首次使用自动迁移（锁+快照+校验+原子提交） | GCX-01（已 `done/complete`） | 已过（GCX-01 已发布 v0.23.1） | DEP-GCX-02：与 plan-20260918 串行写 `COMPATIBILITY.md`/网站页 | ⚠️ 需核 DEP-GCX-02 写集 clean |
| [`plan-20260919`](plan-20260919.md) | GCX-04 | 用户级 hooks 文件路径对齐 XDG（macOS 只读回退） | GCX-01（已 `done`） | 已过 | DEP-GCX-01（网站 `cf` 分支）；发布队列 GCX-02→GCX-03→GCX-04 | ⚠️ 受发布队列与 DEP-GCX-01 |
| [`plan-20260907`](plan-20260907.md) | B3-00 | pin `git-internal` 0.9.0 并引入 `object_format` 事实源 | 无 | **双评审已 PASS**（Grok R2 / Claude R40 / Codex R40） | 外部无；开工需 `cp .env.test.example .env.test` | ✅ |
| [`plan-20260912`](plan-20260912.md) | MB-01 | 有界 monoengine tree transport 与 wire validation | 无 | **双评审已 PASS**（Codex R7 / Claude R5） | DEP-MB-01：monoengine `0e78ca1` tree API pin 现场重核 | ⚠️ 需重核 DEP-MB-01 |
| [`plan-20260916`](plan-20260916.md) | CAP-01 | Agent Capture wire types、URL、uid、transport trait | 无 | **双评审已 PASS**（Codex R3 / Claude R3） | DEP-CAP-01：monoengine `2b8f365` capture HTTP pin 重核（ER-CAP-02） | ⚠️ 需重核 DEP-CAP-01 |
| [`plan-20260913`](plan-20260913.md) | FL-00 | 核实 Media 前提与热路径（audit，no-release） | 无 | 联合 review 已 PASS（U2 `VERDICT: PASS`） | DEP-FL-04：**plan-20260907 须完整收口**（未启动 → 硬门） | ❌ 阻塞（等 9/07 收口） |
| [`plan-20260904`](plan-20260904.md) | CX-00 | codex-cli 0.152 基线探测与 ADR go/no-go | 无 | **未过**（R5 `FAIL`；Claude 亦未出 verdict） | CX-30 另受 DEP-CLI-mirror | ❌ 禁止开工 |
| [`plan-20260905`](plan-20260905.md) | CC-00 | Claude Code 2.1.259 Hook source 契约探测 | 无 | **未过**（须 Claude `PASS`） | 无 | ❌ 禁止开工 |
| [`plan-20260906`](plan-20260906.md) | SC-01 / SC-02（可并发） | SC-01 `base.yml` 最小权限加固；SC-02 会话入口 id 守卫 | 无 | **未定稿**（R2 PASS 已作废；R22 `FAIL`） | SC-04 受 DEP-SC-01/04/05/06；SC-07 受 DEP-SC-07 | ❌ 禁止开工 |
| [`plan-20260902`](plan-20260902.md) | OG-00 | opencode 1.18.29 Hook/export 契约探测 | 无 | **未取得双 PASS**（Claude 限额，Codex 仍在 FAIL 循环） | 无（Phase 1 与 RG 六卡解耦） | ❌ 禁止开工 |
| [`plan-20260911`](plan-20260911.md) | PI-01 | Repository-only `agent_kind=pi` migration | 无（DEP-PI-04 已满足：9/10 已收口） | **Claude Code 429 无 verdict，禁止开工** | DEP-PI-01/03 | ❌ 禁止开工 |
| [`issues/470`](issues/470.md) | FM-01 | 共享写入原语与 `restore` 系物化 | 无 | 尚未 Codex review | 关闭依赖 plan-20260918 FM-03/04（DEP-FM-06/07） | ❌ 禁止开工 |
| [`issues/473`](issues/473.md) | IN-01 / IN-03 / IN-02 | 空模板自引用防护 / 存储路径前置检测 / 换格式 reinit fail-closed | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/474`](issues/474.md) | CL-01 | fsck 断链检测与 shallow 豁免 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/475`](issues/475.md) | CF-02 / CF-01 | key/模式校验与退出码 / 带 value-pattern 的删除 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/476`](issues/476.md) | WT-03 | `init` 不再创建默认 `.libraignore` | DEP-WT-08、DEP-WT-05 | 用户 2026-09-20 覆盖：执行 Agent 自审 | DEP-WT-08（ADR-WT-04 用户评审）未满足 | ❌ 阻塞 |
| [`issues/478`](issues/478.md) | LG-01 | `log`/`rev-list` `--grep` 模式类型与匹配范围 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/479`](issues/479.md) | EC-01 | plumbing 退出码契约守卫 | 无 | 尚未 Codex review | DEP-PL-04：与 plan-20260918 `add.rs` 串行 | ❌ 禁止开工 |
| [`issues/480`](issues/480.md) | HP-01 / HP-02 | `remote add` 默认 refspec 与 `--mirror` / `remote rename` 改写推送目标 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/481`](issues/481.md) | MX-01 | 短对象名候选去重 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/483`](issues/483.md) | CO-01 | `count-objects` 无参数形式与命令契约 | 无 | 尚未 Codex review | CO-03/04 受 DEP-CO-04（`src/cli.rs`） | ❌ 禁止开工 |
| [`issues/487`](issues/487.md) | IG-01 | 本地传输复用已修复的 pack 编码器 | 无（必最先完成） | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/488`](issues/488.md) | GR-01 | `grep --exclude-standard` / `--no-exclude-standard` | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/490`](issues/490.md) | SW-01 | 采用支持 index v3 扩展标志的 `git-internal` | 无 | 尚未 Codex review | DEP-AD-07：与 plan-20260918 串行 | ❌ 禁止开工 |

> 说明：✅ = 无任何门控，可立即开工；⚠️ = 内部无前置但仍有外部/跨计划 `DEP-*` 或发布队列约束；❌ = 计划级 review 门未过（多数 issue 计划尚未 Codex review），按模板 ER-05 / GC-01 **禁止**标 `in-progress`。
>
> **一致结论：当前真正「零依赖且未被门控」可立即开工的是 `plan-20260907` B3-00。** 它是未启动计划中第一个双评审已 PASS 且外部无前置的卡——优先启动它可同时解锁 plan-20260913（FL Media）依赖链。`plan-20260822` OL-13 已于 2026-09-20 补记账为 `done/complete`（实现随 `9da06b4`/v0.23.0 发布）；OL-14 已取消（G-09 墓碑），OL-15A 已完成 runtime cutover，OL-15 为 `done/remote-pending`，等待 compat-offline-core 远端门禁。

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

- plan-20260830：`DEFER-SBX-06` 发布步延后（DEP-SBX-05 未就绪）。
- plan-20260729：`DEFER-09`（CT3-07 转换轴）——已被 plan-20260825 TA-01/02 + plan-20260827 NP-00 承接关闭。
- plan-20260819：`DEFER-M2-01..08`（Memory 范围外：向量检索、团队同步等）。
- plan-20260822：`DEFER-01..03`（Operation Log 范围外）；`DEFER-02`（Web 图 SSE）已随 OL-14 取消关闭；`DEFER-05` 已由 PR #503 的 OL-15A 承接并关闭；OL-15 等待远端兼容门禁收口。
- plan-20260903：`DEFER-01..12`（merge 范围外/deferred 差异）。
- plan-20260715：`DEFER-01..10`（RT-01 收尾；部分由 plan-20260824 承接关闭）。

---

## 六、跨计划依赖（DEP-* 现行生效项）

| DEP-ID | 类型 | 内容 | 现状 |
|---|---|---|---|
| DEP-AD-12 / DEP-CLI-mirror | 跨计划写集互斥 | `src/cli.rs` 三态串行：plan-20260918 OI-05、plan-20260904 CX-30、plan-20260912 MB-03/05、plan-20260916 CAP-07、issues/483 CO-03/04 | 生效；OI-05 开工前必须核对 |
| DEP-GCX-02 | 跨计划写集互斥 | plan-20260919 与 plan-20260918 的 `COMPATIBILITY.md`/docs/网站页串行 | 生效 |
| DEP-FL-04 | 跨计划前置 | plan-20260913 依赖 plan-20260907 完整收口 | plan-20260907 未启动 |
| DEP-CC-05 | 跨计划前置 | plan-20260905 CC-02..06 依赖 plan-20260904 全部非延后卡完成 | plan-20260904 未启动 |
| DEP-SBX-06 | 内部发布延后 | plan-20260830 SBX 发布步 | 未就绪 |
| DEP-05 | 内部前置（已承接） | plan-20260822 v1-boundary runtime cutover（branch/sequencer/worktree repair/v1 op restore → v2 middleware）；由 OL-15A 承接，OL-15 依赖其完成 | PR #503 OL-15A 已完成 |
| DEP-WT-09 | 跨计划前置 | issues/476 关闭依赖 plan-20260918 WT-05..07 完成 | plan-20260918 未到 WT |
| DEP-FM-06 / DEP-FM-07 | 跨计划前置 | issues/470 关闭依赖 plan-20260918 FM-03/FM-04 完成 | plan-20260918 未到 FM |
| DEP-AD-06 / DEP-AD-07 | 跨计划写集互斥 | plan-20260918 与 issues/483（CO）、issues/490（SW）、issues/479（PL）的 `add.rs`/index 写集串行 | 生效 |
| DEP-PL-04 | 跨计划写集互斥 | issues/479 与 plan-20260918 IA-01/IA-02（`add.rs`）串行 | 生效 |
| DEP-CO-04 | 跨计划写集互斥 | issues/483 CO-03/04 与 plan-20260904 CX-30（`src/cli.rs`）三态串行 | 生效；CO-03/04 开工前核对 |

---

## 七、完成计划清单（已收口）

日期计划：`plan-20260708`、`plan-20260713`、`plan-20260714`、`plan-20260715`、`plan-20260818`、`plan-20260821`、`plan-20260824`、`plan-20260825`、`plan-20260827`、`plan-20260901`、`plan-20260910`、`plan-20260917`、`plan-20260920`。

Issue 计划：`issues/477`（31 卡，v0.22.49）、`issues/486`（AB-01，v0.22.31）。各自完成判据见对应计划文件。
