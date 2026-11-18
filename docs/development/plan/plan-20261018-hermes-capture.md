# Hermes Agent 本地会话采集计划（2026-10-18）

> **HM-06 之后的版本准入变更（待发布）。** HM-00 冻结了审计样本，HM-06 首次在 Hermes v0.22.5（`bf0203a`）上完成隔离端到端采集。下文的提交、哈希、实测结果与发布记录保留其历史含义。Libra 0.30.208 启用采集时仍检查该版本与提交。新规则仅按 SemVer 优先级判断 Hermes 是否达到最低版本 `0.22.5`，忽略构建元数据，无需匹配提交。`0.22.5-rc.1` 低于下限，`0.23.2` 高于下限。达到下限仅通过版本检查，真实模型验证的范围仍以各次实测记录为准。

> **本机版本冻结：2026-10-29 16:42:17 UTC。** 本机 Hermes Agent v0.22.5（CLI 原文 `Hermes Agent v0.22.5 (2026.9.24) · stable · local bf0203a794a0`，并打印 `Up to date`）。安装目录 `/Users/eli/.hermes/hermes-agent`，方法 git，detached HEAD，完整提交 `bf0203a794a07aac3ff764972ab9087baa4e4f4e`。本地 tag `v0.22.5` 与 `rc.3-v0.22.5` 指向该提交。GitHub latest release 同为 `v0.22.5`，发布时间 2026-10-23T05:28:45Z，发布说明引用提交前缀 `bf0203a794a`。二进制 `/Users/eli/.local/bin/hermes`（同目录另有 `hermes-agent`）。Python 3.14.9。旧 pin `ccc6956` 只保留为 2026-10-18 历史，不再作为当前 source。本轮未读 `~/.hermes/state.db`、会话目录、`config.yaml` 或 `.env`，未启动模型。冻结当时 10 张卡仍 `pending`。HM-00 的审计收口在后文，不把真实数据算成已取得。Thinking 主源、哈希与 GO/HOLD 见「HM-00 版本冻结」。

> **数据获取优先修订：2026-10-18 16:23:55 UTC（用户本次明确授权）。** 采用 [DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019)：按 OpenCode 实测保留 thinking 正文及其未证明附带数据，不再以 native 来源认证或隐私剥离阻止普通 checkpoint 保存。下方旧日期 PASS/FAIL、版本与 source pin 仅作原时点证据；本次未运行这四种 provider 的模型，不增加已完成卡数。

> **适用优先级：** 本次用户授权的 DATA-CAP-20261019 替代旧 GC/ADR 及任务卡中对 thinking 的隐私剥离和 native 认证保存前置；模板的工程正确性、数据库、测试与交付规则继续适用。证据中的 OpenCode 原值保留是已实现事实，这四份计划的等同行为仍待实现。

> **起草与审计时间：2026-10-18 02:00:57 UTC；模板v2.14。** 全仓`rg -i hermes`仅找到旧SCAP删除说明与无关发布文档，无独立provider计划、AgentKind或adapter。本计划补齐缺失方案。2026-10-18 起草时本机未安装 `hermes`；该句只描述当时。2026-10-29 已观察到 v0.22.5，见文首版本冻结。仍不宣称已完成本机真实模型验收。

> **基础依赖镜像 DEP-ACF-MIRROR：** 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；terminal work 必须注入共用 `CaptureFinalizePolicy`，不得私建 ledger/finalizer。本镜像只消费已完成机制，开工仍须逐文件复验当前接口。

## 文档职责、适用范围与成功定义

为NousResearch Hermes Agent建立独立source审计、Repository identity、本地普通session导入与observe-only采集，复用Libra已有ACF/SCAP/COV2机制。先准备来源/非控制协议，再公开supported capture；cloud mirror/restore未获支持时总入口拒绝。仅本地CLI coding session/cwd绑定的内容在范围内；gateway、Telegram/Discord/多租户消息、远程账号或跨用户历史不采集。

非目标：provider decryptor、扫描凭据、替Hermes执行任务、接管其tool/context/output、review/investigate launcher、从热SQLite分别复制db/WAL、修改Hermes数据库或跑其会自动迁移的PythonSessionDB构造器。

## 事实基线与当前缺口

- Libra `observed_agents/adapter.rs:30` 的 `AgentKind` 没有 Hermes，枚举末项是 `Grok`（`:40`）。`registry.rs:134` 的 `REGISTRY` 共 8 行，最后一行 slug 是 `grok`（`:265`）。`capture/extraction.rs:420` 只特判 Claude Code、Codex、OpenCode，其余 kind 在 `:438` 返回 partial。`migration.rs:572` 的 CHECK 扩展是 grok。新增 Hermes 仍须配对 Repository CHECK 与 enum，不能冒用已有 db tag。
- 当前支持 pin 是本机已安装的 [NousResearch/hermes-agent@bf0203a794a07aac3ff764972ab9087baa4e4f4e](https://github.com/NousResearch/hermes-agent/tree/bf0203a794a07aac3ff764972ab9087baa4e4f4e)，tag `v0.22.5`。2026-10-18 的 [ccc6956](https://github.com/NousResearch/hermes-agent/tree/ccc695635094d7615373db267b77f894d978b1a0) 与当时文件哈希只作历史。现行文件哈希见「HM-00 版本冻结」。
- 官方 hook 文档仍区分 observer/control/transform。v0.22.5 的 `website/docs/user-guide/features/hooks.md:471` 写明 `on_session_end` 在每次 turn 终态触发，canonical payload 没有消息正文；`:472` 的 `on_session_finalize` 才是 teardown，且 surface 字段不同。不能把 `post_llm_call`（`:457`，仅成功且未中断）当成失败或中断都有事件。
- 会话正本在 `HERMES_HOME/state.db` 的 `messages` 表。v0.22.5 已把 schema、消息恢复和 export 分到 `hermes_state_common.py`、`hermes_state_messages.py`、`hermes_state_portability.py` 与 `hermes_cli/sessions_cmd.py`。`hermes_cli/plugins_dispatch.py` 仍是 plugin hook 调度。
- 本机二进制存在：`/Users/eli/.local/bin/hermes`。supported 提升仍被 DEP-HM-03 的隔离 smoke 与 trust 挡住；binary 存在不代替真实非空 thinking 比对。

## 与其它计划的关系、依赖登记与实施顺序

强制采用 [四 provider 执行契约](plan-20260904.md#capture-review-20261018)。DEP-CAP-BASE/RG/OG/WRITE在本计划镜像登记：具体已交付采集owner、ordinary来源策略与四provider shared I串行；当前卡执行者为核验owner，dirty/in-progress或缺机制停止对应实现。

| DEP | direction / 产物与可用性 | Owner / 失败路径 |
|---|---|---|
| DEP-HM-01 | incoming upstream：完整 commit、可安装版本、hook observer 与已选 export/session 文件/一致 SQLite shape 由 HM-00 冻结 | HM-00审计者；pin/schema/ID无证则相关parser/installerHOLD，不换main猜测 |
| DEP-HM-02 | incoming cross-plan：PI-09 仍提供冻结快照；PI-07 提供安全 CHECK runner，PI-01 提供 role 迁移经验。同步前 kind 白名单已由本目标写在 `run_cloud_sync`，因为 PI-05 仍 pending。不改 D1 CHECK，不复制 PI-09 | HM-02 执行者；PI-09 未交付前不宣称对象闭包已冻结 |
| DEP-HM-03 | incoming local：匹配冻结 release v0.22.5 的 Hermes binary、隔离项目/profile、本机模型 smoke 与 trust 授权 | HM-06执行者；本机 binary 已是该 release。缺隔离 smoke 或 trust 只阻塞 supported 提升，不让计划审计把字段存在写成数据已取得 |
| DEP-HM-DB | incoming：plan-20260910 MIG-02/03已done/complete；当前role manifest与旧binary oracle | schema owner；role/旧reader无法复验，HM-01 blocked |
| DEP-HM-LIVE | incoming：HM-00 的已选文件/一致只读 SQLite/export 字段与 session 语义；HM-08 的 reader/必要 trust/lease/deadline 是实施验收，真实数据由 HM-06 另验，不构成自依赖 | HM-00审计者；语义缺证HOLD，HM-08不开工，HM-06不提升supported |
| DEP-HM-DOC | incoming website：原生Libra cf及agent/cloud页面clean，部署渠道可验 | 文档writer；按四provider契约交付，不能只写仓库文档 |
| DEP-HM-RELEASE | incoming release：签名工具链与逐卡固定source C/D | 单一发布者；缺项locally-accepted/remote-pending |

**严格串行：HM-00 → HM-02 → HM-01 → HM-03 → HM-04 → HM-07 → HM-08 → HM-09 → HM-05 → HM-06。** HM-02先于任何Hermes row writer。HM-01前还须PI-07及PI-01 source交接以避免同一enum/schema/files冲突；HM-02只审计PI-05/09已交付的通用白名单与snapshot边界，先行cloud fence无需schema允许该tag。无REL家族/批量发布例外。

## 已决议设计决策与全局工程约束

- ADR-HM-01：known observer与control/transform彻底区分，默认不注册pre_tool_call/pre_llm_call/transform；注册observer不返回控制值，不以异常/timeout改变provider行为。不抓整份conversation_history作为无界hook envelope。
- ADR-HM-02：`on_session_end`按已证turn outcome映射TurnEnd/失败事实，不能过早关闭session；teardown/reset/finalize与new/resume分开。失败/中断缺终态时partial并由显式导入/既有owner recovery对账，不伪造complete。
- ADR-HM-03：HM-00 比较官方 export、session 文件和一致只读 SQLite 哪一种实际包含 thinking，选择一条最小可行 source。2026-10-29 对 v0.22.5 的选择见「HM-00 版本冻结」：主源是不带 `--redact`、`--only`、`--delete-after-verified` 的 `hermes sessions export --format jsonl`。md 与 qmd 不写 thinking。HTML 只有有损渲染，trace 与 `--only user-prompts` 不含 thinking 字段。三者都不是字段合同。`HERMES_HOME/sessions/<id>.jsonl` 只在 state.db 被替换时 divert，不是主源。export 丢字段时允许只读事务或 SQLite backup API 的一致视图，不能分别复制热 db/WAL，不能以 immutable=1 忽略 live WAL，不能构造会迁移的 provider SessionDB。WAL 只读连接可能由 SQLite 创建/更新 -shm，此限于已授权源同目录的引擎协调文件；不写主 DB/WAL 或 schema、不视为迁移。源目录不允许协调写时用引擎支持的一致 backup/离线快照，无法建立一致视图就明确 unavailable；不得分别拷贝 db/WAL。HM-03 的 fixture 覆盖有 WAL、现成/缺失 shm 和只读目录。HM-03 实现已选 source 读取；HM-08 通过现有 snapshot/lease owner 接线。只有选择执行 exporter 才需要 fixed argv/子进程 trust/deadline；自建沙箱/解释器闭包不是 ordinary 获取前置。静态 schema、真实数据与支持提升分别验收，缺 native 认证仍保存 Unproven 数据。

- ADR-HM-04：profile/host/session/turn/message/tool ID联合界定scope；本计划只绑定当前本地repo/worktree，缺cwd/identity则拒绝或partial，不合并不同profile、gateway或跨用户会话。稳定ID与去重由HM-00字段矩阵证明，不按时间或display title猜测。
- ADR-HM-05：普通 reasoning 原文与关联 unknown/encrypted-like 数据保留并标 Unproven/Partial，无 native issuer 声明。snapshot/worker 的有界 Hermes carrier 在 HM-07 显式接线，不用隐私过滤删除数据。
- ADR-HM-06：本地only支持先于任何writer发布cloud fence，复用PI-05的冻结generation/对象闭包并发边界及当前权限/tombstone/撤权复验；不能只在后置mirror拒绝，也不能因旧快照复活已擦除内容。Repository CHECK仅走ADR-CAP-CHECK/PI-07专用连接；新增唯一ID大于当前tip，CHECK保留Pi及所有已有值，Global/System/Derived零写，未来旧reader零写拒绝；只前滚，不drop用户数据。
- 上限继承四provider契约：16MiB source、1MiB行/envelope、2s总budget、8in-flight、3次尝试共享deadline；单导入最多10000messages、1000sessions枚举上限，截断/超限有明确partial/拒绝，不悄悄完成。只写Libra-owned plugin asset/配置项，绝不替用户修改Hermes consent allowlist。

## 当前缺口与12维自审

| GAP | 当前事实/风险 | 卡 |
|---|---|---|
| GAP-HM-01 | 无Hermes identity/schema/adapter | HM-01/03，PI-07前置 |
| GAP-HM-02 | 版本/callsite/live来源未GO | HM-00、DEP-HM-LIVE |
| GAP-HM-03 | D1不支持新增kind | PI-05白名单及PI-09快照，HM-02审计 |
| GAP-HM-04 | 无普通import/worker/导出桥/scope/observer/安装 | HM-04/07/08/09/05/06 |

| 维度 | 规划判断 |
|---|---|
| 合理性 | 当前无实现，以本地ordinary范围补齐 |
| 可行性 | HM-00分别给静态与live GO/HOLD，不猜参数 |
| 粒度 | 10卡各单轴，实际≤4落点/12生产文件 |
| 依赖 | 具体PI-05/07/01与HM卡产物，无整计划空等待 |
| 完整性 | 实现/测试/文档同卡，audit只写计划 |
| 安全 | 仅已证CLI/scope/profile，gateway等零转发 |
| 正确性 | turn/session终态分开，缺ID/未知shape partial |
| 兼容 | D1旧七值不变，PreviewAgent先行、支持提升最后 |
| 数据控制流 | 共享snapshot/coordinator，intent先于对象/ref |
| 性能容量 | 分页spool与有界source/child，禁止history无界序列化 |
| 可靠性 | 幂等/崩溃/撤权及迁移child摘要全门 |
| 可维护性 | 复用CAP/PreviewAgent/runner/D1 SSOT |

## 全局工程约束与执行检查必备需求

完整采用plan-template.md的GC-01..GC-13、ER-01..ER-14及四provider契约，不删减强制门。ER-01记录基线/精确I/R，ER-02刷新source，ER-04双状态，ER-05逐卡Claude PASS，ER-06/06a逐文件文档/网站，ER-07同提交签名/DCO，ER-08每实现卡patch，ER-13/14双env nextest。ER-12唯一发布者为所选卡执行Agent，版本/tag/Release与网站cf窗口串行；未指派个人，不捏造姓名。D-CAP-STD固定workflow/job/event/ref/source取证。无 REL 批量发布例外；本次数据保留隐私豁免以 DATA-CAP-20261019 用户授权为准。

## Phase与发布分组窗口

| Phase | 卡 | 进入/退出 |
|---|---|---|
| 0 source | HM-00 | 本轮规划PASS且选卡授权；退出各能力GO/HOLD |
| 1 identity | HM-02→HM-01 | PI-05/09/07/01实际交接；退出白名单回归/无损CHECK/preview |
| 2 ordinary | HM-03→HM-04→HM-07 | source GO；退出已选 source/import/有界 snapshot/worker |
| 3 live/promotion | HM-08→HM-09→HM-05→HM-06 | LIVE_SOURCE GO/匹配binary；退出 observer/本机非空 thinking 比对/支持提升 |

每实现卡independent release；审计no-release承载卡在卡内具名。版本面单写者，不把后继delta混入已冻结source；source-qualified后可开发下卡，formal complete仍等该卡实际C/D。失败前滚，不改已发布artifact。

## 依赖证据与超时登记补充

| DEP | 证据 | 超时/失败 |
|---|---|---|
| DEP-HM-01 | 事实基线完整commit/blob，HM-00 inline字段/callsite矩阵 | ≤2人日，无证HOLD |
| DEP-HM-02 | PI-05白名单/PI-09 spool、PI-07 runner、PI-01 signed source/逐文件handoff | 未交接/dirty/in-progress即blocked |
| DEP-HM-03 | HM-06 actual binary/version/场景/结果inline | 无binary/授权不运行，skip不通过 |
| DEP-HM-DB | MIG-02/03实际完成与PI-07门 | role/oracle/child门红则blocked |
| DEP-HM-LIVE | HM-00 已选 source 语义、HM-08 fake-reader/exporter 机制门与 HM-06 真实数据门 | 2s总deadline；无证metadata-only，HM-06 blocked |
| DEP-HM-DOC | cf文档postimage/sync/frontend/Claude/部署收据 | 任一失败不发布网站 |
| DEP-HM-RELEASE | D-CAP-STD SHA/ref/run/job/安装inline | 未完remote-pending |

## 任务卡粒度审计表

按实际I集逐文件/目录计，SQL只含开工分配ID的两文件；README、测试、强制文档不计生产。无EX豁免。

| ID | type | axis | AC | VER | landing | prod-files | scope | deps | release |
|---|---|---|---|---|---|---|---|---|---|
| HM-00 | audit | Hermes版本、source与观察协议审计 | 6 | 2 | 0 | 0 | M | DEP-HM-01 | no-release |
| HM-02 | audit | 现有白名单的Hermes拒绝审计 | 3 | 1 | 0 | 0 | M | HM-00、DEP-HM-02 | no-release |
| HM-01 | migration | Repository Hermes identity与role迁移 | 5 | 1 | 4 | 10 | M | HM-02、DEP-HM-DB、DEP-HM-02（PI-01 source交接） | independent |
| HM-03 | implementation | 安全Hermes source adapter | 4 | 1 | 2 | 5 | M | HM-01 | independent |
| HM-04 | implementation | Hermes普通内容归一化与显式import | 4 | 1 | 4 | 5 | M | HM-03 | independent |
| HM-07 | implementation | Hermes有界普通采集能力 | 4 | 1 | 3 | 7 | M | HM-04、DEP-CAP-BASE、DEP-CAP-RG | independent |
| HM-08 | implementation | Hermes已选thinking来源的live读取桥 | 8 | 2 | 2 | 6 | M | HM-07、DEP-HM-LIVE | independent |
| HM-09 | implementation | Hermes本地CLI与scope准入过滤 | 6 | 1 | 2 | 5 | M | HM-08 | independent |
| HM-05 | implementation | Hermes observer plugin与现有ingress | 8 | 1 | 3 | 6 | M | HM-09、DEP-HM-LIVE | independent |
| HM-06 | implementation | Hermes采集支持提升与交付 | 5 | 1 | 2 | 4 | M | HM-05、DEP-HM-03、DEP-HM-DOC、DEP-HM-LIVE | independent |

## 测试矩阵与追溯表

| 卡 | 来源/行为 | 验证族 | 文档与恢复 |
|---|---|---|---|
| HM-00 | GAP02 source | 版本/callsite/LIVE_SOURCE矩阵 | 本计划，缺证HOLD |
| HM-02 | GAP03 同步前白名单 | `unsupported_capture_kind_` 两条 CLI 测试 | 本计划与 cloud 文档，无版本 |
| HM-01 | GAP01 schema | hermes_role_及PI-07 child摘要 | agent/compat，前滚 |
| HM-03 | GAP01 export | hermes_source_ | agent source限制，前滚 |
| HM-04 | GAP04 import | hermes_normalize_ | agent import/local-only，前滚 |
| HM-07 | GAP04 worker | hermes_bounded_capture_ | agent readable/partial，前滚 |
| HM-08 | GAP04已选 source live 接线 | hermes_exporter_及hermes_source_bridge_（覆盖文件/SQLite/exporter 所选分支） | 必要 trust/doctor/agent/网站，前滚 |
| HM-09 | GAP04 scope | hermes_scope_admission_ | 内部策略，前滚 |
| HM-05 | GAP04 observer | hermes_observe_含gateway/unknown负向 | agent协议/恢复，前滚 |
| HM-06 | GAP04 supported | roster与实际本机binary smoke | agent/cloud/网站/安装，前滚 |

## 故障恢复矩阵

| 故障 | 可接受状态 | 恢复/禁止结果 |
|---|---|---|
| source/live无证 | 对应HOLD | 回HM-00，不猜export参数 |
| gateway/unknown scope | 零转发 | 固定诊断，不把私有消息归仓库 |
| export timeout/malformed | partial/pending | 显式重导，不读热DB/不false complete |
| migration失败 | rollback且child摘要不变 | discard专用连接，不drop用户事实 |
| child/ref/DB中断 | durable intent/已有pending | 幂等replay，未知trace不上云 |
| erase/撤权变化 | 拒绝发布冻结snapshot | 重新快照，不复活旧body |
| release/网站失败 | locally-accepted/remote-pending | 固定source前滚，不伪PASS |

## 性能与容量表

| 路径 | 上限 | 退化 |
|---|---|---|
| source | 16MiB、每行1MiB、10000messages | 超限partial/拒绝 |
| 枚举 | 1000sessions | 显式续页/拒绝无界遍历 |
| exporter/child | Hermes `sessions export` 20s（v0.22.5 冷启动实测约 12s，仍在 30s capture wrapper 内）；其余 child 维持四 provider 契约的 2s、8in-flight、3尝试共享deadline | kill/reap与安全pending |
| cloud snapshot | 同PI-05按页spool，内存按页 | 磁盘不足remote前失败 |

## HM-00 版本冻结

观察时间 2026-10-29 16:42:17 UTC。对象是本机安装树 `/Users/eli/.hermes/hermes-agent` 的提交 `bf0203a794a07aac3ff764972ab9087baa4e4f4e`，不是用户会话库。未执行 `hermes sessions`，未打开 `state.db`。

### 文件哈希

| 文件 | SHA-256 | 字节 |
|---|---|---|
| `hermes_state.py` | `e507ebd78b870364fd2fced94212592380cd26d0d39eec67547ed9bc233ebb98` | 95230 |
| `hermes_state_common.py` | `1e089e97c9440647a91e0c559d9a7691c61290cc72a0c337ccfcd1c39d97b1b7` | 74542 |
| `hermes_state_messages.py` | `b3c5ad76a6d1a63c2800a0eca0c36240ad3098999a248568e46a7c3649cf9136` | 129557 |
| `hermes_state_portability.py` | `3eca72538fae87b2fccac6fd3be8f44654d4a589184cd7d9664fbd63141fa001` | 49097 |
| `hermes_cli/session_export.py` | `b2e381735dc3744beab43058d74acf38cbabdf0e2115800276296a56c3169242` | 12724 |
| `hermes_cli/session_export_md.py` | `00217e1e7abdfc45a05b2271517b80c24f4659dd5879084b0867c60716defeca` | 9321 |
| `hermes_cli/session_export_html.py` | `98e1e5b85655a2edd806c1bdadd1b7bed91d28d2f06e02c959725c1524805852` | 28686 |
| `hermes_cli/sessions_cmd.py` | `97219a32bfd43c7eeca8a9422bb653f5c8486a249ce69da64bc0e85ef337357e` | 63056 |
| `hermes_cli/plugins_dispatch.py` | `a3960ec7a3c4ebfa4053f3019dcb67b73de5df384bfa4c0a8484a7815972833c` | 35005 |
| `hermes_constants.py` | `870ad9935a62968636d6a14d160c0269dfbefd8091ab3cb71be78b30bef6360f` | 67766 |
| `website/docs/user-guide/features/hooks.md` | `c6026268d7aae724af6df7b37c8a1a8395fa6c00414054f3f51285881f3e1bf5` | 135826 |
| `cli-config.yaml.example` | `b8c2f476aac6f8fff4ecb7c6585e223c47ad00e46ea324ec6c52f28b2237f6b5` | 129469 |
| `agent/trace_upload.py` | `0bbc2c23edbdad5d9b7aa266a6fe343d7f4133318bcaacfad33fd468e203b98c` | 11769 |

### Thinking 落在哪里

`hermes_state_common.py:531` 的 `messages` 表有 `reasoning`、`reasoning_content`、`reasoning_details`、`codex_reasoning_items`、`codex_message_items`。

JSONL 使用的不是 `get_messages_as_conversation`。链路是 `hermes_state_portability.py:347` 的 `export_session`，调用 `get_messages`（`hermes_state_messages.py:1344`）。主源 argv 不带 `--only`。`session_export.py:210` 的 `SAVE_TRANSCRIPT_FORMATS` 只有 md 与 html，所以 `sessions_cmd.py:367` 得到的 `shown` 为假，`:368` 调用 `export_projection(False)`。返回值在 `session_export.py:218`，是 `include_inactive=True`。`get_messages` 因此走 `:1384` 的全行查询，再在 `:1394` 用 `_row_to_message_dict`（`:1271`）把 `SELECT *` 收成 `dict(row)`。

这个字典的形状是：

- 每个 role 的每一行都带上表列。`reasoning` 与 `reasoning_content` 为 SQL NULL 时，JSON 是 `null`；空串保持空串。不会因为值为假就删键。
- `reasoning_details`、`codex_reasoning_items`、`codex_message_items` 保持数据库里的原始 JSON 字符串，这里不解码。
- `content` 与非空的 `tool_calls`、`display_metadata` 会解码。`display_identity` 与 `display_order` 被去掉。
- 行上带 `active` 与 `compacted`。`include_inactive` 的空过滤（`:1340`）会把 Undo/Rewind 撤销行也导出。撤销行是 `active` 与 `compacted` 都为假。压缩归档行是用户仍能看见的历史，`compacted` 为真。

`sessions_cmd.py:439` 的 `_render_jsonl` 对整个 session 字典做 `json.dumps`。

`sessions_cmd.py:1300` 把普通 export 标成 observational，并用 `SessionDB(..., read_only=True)` 打开。`hermes_state.py:652` 在 `read_only` 时走 `_open_read_only`（`:731`），不做 schema init，不加写锁。`--delete-after-verified` 会改成写打开，且只适用于 md/qmd；主源 argv 禁止它。只读打开碰到旧 schema 缺列时，`sessions_cmd.py:1334` 打印需要 migration 并返回 1。那是 unavailable，不是空 thinking，Libra 不改去跑会写库的 Hermes 命令。

HM-03 的 reader 只保留 `active` 或 `compacted` 至少一个为真的行。两个都为假的撤销行不进入普通 transcript，也不把其中的 thinking 当成当前会话。保留行上，非空字符串的 `reasoning` 或 `reasoning_content` 是可读 thinking；`null` 与空串不是。后三列作为未解析字符串附带保存，解码失败只把该行标 partial，不删已取得的可读字符串。fixture 至少覆盖：撤销行被排除、`null` 与空串、以及一条非空可读字符串。`hermes_state_messages.py:1545` 的对话恢复函数会按真值挑选 assistant 字段并解码 JSON；它不是这条 export 的形状，HM-03 不得用它当 JSONL parser。

### 选定主源与排除

主源 argv，由 HM-08 原样使用，本轮未执行：

```text
hermes sessions export --format jsonl --session-id <id> <output-file>
```

不附加 `--redact`、`--only`、`--delete-after-verified`、`--upload`。`--redact` 会走 `session_export_md.py:169`，对 messages 里每个字符串做替换，包括 thinking。`<id>` 必须是 HM-09 已准入的当前本地 CLI 会话。`export_session`（`hermes_state_portability.py:347`）经 `_with_messages`（`:344`）调用 `get_messages` 时不传 `include_ancestors`，默认是假（`hermes_state_messages.py:1361` 只取这一个 session id）。压缩轮换后的父段 thinking 不在这份 JSONL 里。HM-03 与 HM-09 只覆盖该 id 这一段，不把父段并进来。

不可用不能只看退出码。缺列是 `:1334` 到 `:1341` 返回 1。打开失败是 `:1315` 起，`:1318` 返回 1。session id 在打开后解析失败时，`:356` 判定，`:357` 执行 `return _not_found`。`_not_found`（`:49`）打印 `No session '<id>'` 并返回 1。收集阶段的 `:399` 也调用 `_not_found`，但 `:400` 丢掉这个返回值，改返回空，外层因此可能是退出码 0。`_too_large` 在 `:395` 命中后 `:396` 返回空。`_export_flat` 的 `:461` 是 `sessions is not None` 才写，不是“列表非空”这个词；带 `--session-id` 时收集结果只是空或恰好一条。库不存在时 `:1313` 走 `:1279` 的 “No sessions found.”，不写文件。

HM-08 的输出路径必须是还不存在的普通文件。`:79` 的 `_output_file_in_dir` 在 `:84` 判定目录或尾随分隔符，`:85` 创建目录，`:86` 拼出默认文件名。已存在的旧文件不能当成这次导出。下面每一项都是 unavailable，不是空 thinking：退出码 1、stdout 里的 migration、打开失败、`No session`、`No sessions found.`、输出路径在调用前已存在、调用后文件仍不存在、文件为空。退出码 0 不能单独证明数据已取得。

`session_export.py:225` 的 `drop_undone_rows` 用 `m["active"] or m["compacted"]` 留下行。JSONL export 自己不调用它。HM-03 的 reader 必须套用同一谓词。

| 候选 | 结论 | 依据 |
|---|---|---|
| JSONL export | GO，主源 | 只读打开；`dict(row)` 保留全部列。reader 按上一节丢掉撤销行 |
| Markdown / QMD | NO-GO | `session_export_md.py:66` 的 `_render_messages` 只写 `content` 与 `tool_calls`；`:195` 的 `write_session_markdown` 是 md 与 qmd 的写入函数 |
| `--only user-prompts` | NO-GO | `session_export.py:87` 只留用户文本 |
| HTML | NO-GO | `session_export_html.py:723` 用真值取 `reasoning` 或 `reasoning_content`，只渲染其中一个。HTML 走 transcript 投影（`include_compacted`），和 JSONL 的 `include_inactive` 不是同一组行。有损展示，不是字段合同 |
| `--format trace` | NO-GO | `agent/trace_upload.py:123` 的 assistant 块只有 text 与 tool_use；默认还会 redact |
| `HERMES_HOME/sessions/<id>.jsonl` | HOLD，非主源 | `hermes_state.py:433` 只在 state.db 被替换时追加 |
| Libra 自己打开 SQLite | 后备 | 列在 schema 里。export 不可用时才用 ADR-HM-03 的一致只读视图。Libra 不构造 `SessionDB` |
| `on_session_end` | NO-GO | `hooks.md:471`，observer，canonical payload 无正文 |
| `on_stream_delta` 的 reasoning | HOLD，非持久源 | `hooks.md:466`，要 `plugins.stream_reasoning_deltas`，且是流式增量 |

可读 thinking 只来自保留行上的非空 `reasoning` 或非空 `reasoning_content` 字符串。三列原始 JSON 字符串按 ADR-HM-05 标 Unproven/Partial，单独存在时不算可读正文。真实会话里这些值是否非空，仍由 HM-06 用隔离 profile 实测。本表不证明用户库里已经有数据。

### 模型与 thinking 设置

这些是源码和 `--help` 里的开关，不是本机 `config.yaml` 的当前值。

| 设置 | 位置 | 观察 |
|---|---|---|
| 本次会话 effort | `hermes chat --reasoning LEVEL` | help 列出 `none, minimal, low, medium, high, xhigh, max, ultra`，并写明覆盖 `agent.reasoning_effort` |
| 配置示例 | `cli-config.yaml.example:1335` | `reasoning_effort: "medium"`。注释里的档位少于 CLI help，以 `hermes_constants.py:1077` 的 `VALID_REASONING_EFFORTS` 加 help 里的 `none` 为准 |
| 未设置时 | `hermes_constants.py:1210` | 缺键时传入 `""`，`parse_reasoning_effort` 返回 None。不能把示例文件的 medium 写成用户当前配置 |
| 按模型覆盖 | `cli-config.yaml.example:1348` | `reasoning_overrides: {}` |
| 是否在界面显示 | `hermes_cli/config_defaults.py:869` | `show_reasoning` 默认 True。`cli_init_mixin.py:141` 同样以 True 为缺省。这只影响显示，不决定是否写入 `messages` |
| 显示是否全文 | `config_defaults.py:872` | `reasoning_full` 默认 False |
| 模型选择 | `hermes chat -m/--model`、`--provider`；`hermes model` | `hermes model` 是交互选择器。本轮没有具体模型 ID |

### Observer、consent 与 plugin 路径

`hooks.md:397`：observer 忽略返回值，transform 取第一个有效字符串，directive/control 消费规定的返回形状。`pre_llm_call` 在 `:456`，是 directive/control，会把返回值注入用户消息。`pre_tool_call` 仍是 control。两者继续不注册，沿用 ADR-HM-01。

Shell hook 的首次同意从 `hermes_cli/config_defaults.py:1756` 的注释写起。默认值是 `:1760` 的 `"hooks": {}` 和 `:1764` 的 `"hooks_auto_accept": False`。同意记录在 `~/.hermes/shell-hooks-allowlist.json`。`hermes hooks` 的 help 同样指向该文件。Libra 只写自己拥有的 asset，不改这个 allowlist。

用户 plugin 目录是 `get_hermes_home()/plugins`。`hermes_cli/plugins_cmd.py:255` 的 `_plugins_dir()` 会 `mkdir`。Libra 不调用它。`plugins_dispatch.py` 只调度已注册回调，observer 返回值不改变 tool、context 或 message。

### 观察协议 callsite

这些行都在 `website/docs/user-guide/features/hooks.md`。observer 的返回值被忽略。control 与 transform 不注册。

| 事件 | 行 | 类别 | 正文 | 后继怎么用 |
|---|---|---|---|---|
| `post_llm_call` | 457 | observer，仅成功且未中断 | 含完整 history | 不是失败或中断的证据，不把 history 当 hook envelope |
| `on_session_end` | 471 | observer，每个 turn | 无消息正文；带 `completed` / `failed` / `interrupted` | 映射 turn 结果，不因此关闭 session |
| `on_session_finalize` | 472 | observer，teardown | 只有标识 | 会话拆除，不是 turn 结束 |
| `on_session_reset` | 473 | observer | 只有标识 | 与 new/resume 分开 |
| `agent_loop_stopped` | 476 | observer，打断 | 无正文 | 中断事实，partial |
| `subagent_start` | 478 | observer | `child_goal` 可能含用户内容 | 不把子代理当成当前仓库会话 |
| `subagent_stop` | 479 | observer | 摘要与工具历史 | 不采集 |
| `pre_tool_call` | 451 | control | 原始参数 | 不注册 |
| `pre_llm_call` | 456 | control | 可注入用户消息 | 不注册 |
| `pre_gateway_dispatch` | 480 | control | 网关对象 | 本计划不采集 gateway |

### 合成 fixture 矩阵

HM-03 落地。分类只描述 JSONL 主源，不描述 HTML。

| 输入 | 分类 | reader |
|---|---|---|
| 保留行上非空 `reasoning` 或非空 `reasoning_content` | ordinary，可读 | 保留字符串，不写成 assistant 回答 |
| 这两列是 `null` 或空串 | 未取得 | 与缺键分开；不算通过 |
| `reasoning_details`、`codex_reasoning_items`、`codex_message_items` 的原始字符串 | unknown，附带 | 不解码也保留；解码失败只标 partial |
| `active` 与 `compacted` 都为假 | 撤销行 | 不进入普通 transcript |
| md、qmd、trace、`--only user-prompts`、HTML | unsupported 作为字段源 | 不把它们的成功当成 thinking 已取得 |

### 仍 HOLD 的事项

具体模型 ID、用户配置里的 effort、以及任何一条真实非空 thinking，都没有本机会话证据。supported、capture enable 与「数据已取得」仍等 HM-06。本节只关闭审计，不证明数据已经取得。

## 当前任务卡

### Task HM-00: Hermes版本、source与观察协议审计

**Task type:** audit

**Lifecycle / Acceptance:** done / complete

**Description:** 冻结官方source与可安装release对应，给每项能力GO/HOLD；不安装Hermes、不读真实会话。 本卡同时核定系统版本支持的 model/effort/thinking 显示设置与具体 source 位置；不猜参数。审计不启动真实模型；真实数据 smoke 由后续实现/收口卡执行。2026-10-29 的 v0.22.5 冻结加上本卡收口时补入的 P2 边界，就是后继要消费的矩阵。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测和其它计划的行为重构。审计卡不修改生产代码。

**Acceptance criteria:**

- [x] 完整 source/release/package 映射或 HOLD 记录。2026-10-18 的未安装事实只保留为历史；当前 pin 以「HM-00 版本冻结」的 v0.22.5 为准，且仍未读用户会话。
- [x] 逐callsite区分turn结束/真实session teardown/失败/中断/重置/子agent，核对identity和observer返回语义。见「观察协议 callsite」。
- [x] 逐 callsite 比较 export/session 文件/SQLite 实际字段，选一条含 thinking 的只读 source；给出 schema/session/一致性及所需 argv，HM-08 负责 lease/预算接线，HM-06 负责真实数据实测。主源是只读 JSONL；不可用判定不单看退出码。
- [x] plugin发现/配置/consent/owner-only路径有精确source证据，observer注册不会更改tool/context/message。默认值在 `config_defaults.py:1760` 与 `:1764`。
- [x] source字段分ordinary/unknown opaque/unsupported并给合成fixture矩阵；缺证能力HOLD，后继不得凭名字实现。见「合成 fixture 矩阵」。

- [x] 记录系统 Hermes 的模型/thinking 设置和启用方式；所选 source 必须能说明 thinking 字段是否存在，真实模型验证由 HM-06 完成。

**Verification:**

- [x] 人工核读固定commit的`hermes_state*.py`、`hermes_cli/plugins*.py`及官方hooks文档；版本、callsite、GO矩阵写本计划。
- [x] `libra status --short --branch`与`libra diff --name-only`逐项对照开工基线 `8e8b8afdbb2a328a3ce20d1a6f4b5efa58f1dc4e`，仅本计划、`plan-status.md` 与 `plan-long.md`。收口提交前复核对这三项。

**Full-suite trigger:** N/A（只读审计，无代码变更）

**Dependencies:** DEP-HM-01

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** source/GO-HOLD审计矩阵和结论仅写本计划，不创建独立证据文件或fixture。

**Implementation write set:** `docs/development/plan/plan-20261018-hermes-capture.md`；`docs/development/plan/plan-status.md`仅本卡状态；`docs/development/plan/plan-long.md`仅 Hermes 索引行。测试仅运行，不授权修改代码、fixture或Cargo配置。

**Release write set:** N/A

**Docs and compatibility impact:** 仅本计划审计/状态，不修改fixture/代码/配置或公开行为。

**Rollback mode:** revert

**Migration and rollback:** 撤回本卡文档/合成 fixture，不删除历史事实或用户数据。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M（审计时间箱≤2人日，超时记录具名HOLD，不扩大为实现）

**Version increment:** N/A

**Release boundary:** no-release

**C/D coverage from:** HM-01（具体后继release承载文档渠道）；本audit/spike自身signed main/CI仍实际登记，不bump/tag/artifact。

**Granularity:** `type=audit; axis=Hermes版本、source与观察协议审计; recovery=revert; complete=yes; self-contained=yes; AC=6; VER=2; landing=0; prod-files=0; scope=M; deps=DEP-HM-01; writeset=本计划链及DEP-CAP-WRITE逐文件串行; release=no-release; split-from=本轮G-09修订表; exception=N/A`

### Task HM-02: 现有白名单的Hermes拒绝审计

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Description:** 交付同步前的 kind 白名单。PI-05 整卡仍 pending，本目标在 `run_cloud_sync` 入口补上同一拒绝，避免 HM-02 永远等一份还没写的围栏。不改 D1 CHECK，不改 restore，不实现 PI-09 的冻结快照。本卡改了生产代码，仍按目标不升版本。

**HM-02 执行记录：** `src/command/cloud/sync.rs` 的 `CLOUD_CAPTURE_KIND_ALLOWLIST` 是八个当前 D1 值，不含 `hermes`。`unsupported_capture_kinds` 在 `progress.on_starting`、凭据校验、D1 和 R2 之前读取本地 `agent_session`。名单外的 kind 返回 `CloudError::PartialTransfer`，对外是 `LBR-CONFLICT-002`、退出码 128。`--force` 不参与这个判断。允许的种类仍从原来的 `on_starting` 继续，既有缺凭据测试仍然先看到缺凭据。开工之后才插入的 kind 不在本卡范围内，由 PI-09 的快照承接。

`cargo test --test command_test cloud_test::unsupported_capture_kind_ -- --test-threads=1`：2 passed，0 failed。`cloud_test::test_cloud_sync_quiet_preflight_failure_has_no_stdout_progress`：1 passed。无版本变化。

Claude 审查实现 `VERDICT: PASS`。文档挂错常量之后的复核轮也是 `VERDICT: PASS`，正文 SHA-256 `dcd367d9b81c5c0c316ffaf3cb2d3cc4b20e12fa0fd76a10b12a12888b490766`。无新 P0/P1。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测、D1 CHECK、restore，以及 PI-09 的冻结快照。

**Acceptance criteria:**

- [x] 任意Hermes row时所有D1/R2/ensure/sync mutation零副作用，稳定conflict/128。拒绝发生在凭据、D1、R2 之前。
- [x] 开工时已经存在的名单外 kind 不会进入远端。开工之后插入的 kind 仍由 PI-09 快照承接，本卡不宣称已经冻结对象闭包。
- [x] force/JSON/progress不绕过；不改D1 CHECK或restore图。

**Verification:**

- [x] `source .env.test && cargo test --test command_test cloud_test::unsupported_capture_kind_ -- --test-threads=1`：2 passed，0 failed。另有缺凭据回归 1 passed。

**Full-suite trigger:** T-1（`cloud sync` 共享入口）。本卡跑了聚焦测试；计划收口的全量 nextest 仍在 HM-06。

**Dependencies:** HM-00。同步前白名单由本卡落地，不再等 PI-05 写同一处。

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** 同步前拒绝、两条 CLI 测试、cloud 命令文档与 `COMPATIBILITY.md` 的同一句。

**Implementation write set:** `src/command/cloud/sync.rs`、`tests/command/cloud_test.rs`、`docs/commands/cloud.md`、`docs/commands/zh-CN/cloud.md`、`COMPATIBILITY.md`、本计划、`plan-status.md`、`plan-long.md` 的 Hermes 索引行。不改 D1 CHECK 与 restore。

**Release write set:** N/A

**Docs and compatibility impact:** 仓库内 cloud 命令页与 `COMPATIBILITY.md` 已写明拒绝。网站页留到带版本的后继卡。本卡不升版本。

**Rollback mode:** revert

**Migration and rollback:** 保留已写 session、checkpoint、schema receipt 和旧格式读能力，停用新增入口后前滚修复；不得用 down/drop/重写 traces 消除用户事实。迁移卡还须执行本计划数据库角色合同。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M

**Version increment:** N/A

**Release boundary:** no-release

**C/D coverage from:** HM-01（具体后继release承载文档渠道）；本audit/spike自身signed main/CI仍实际登记，不bump/tag/artifact。

**Granularity:** `type=implementation; axis=同步前捕获 kind 白名单; recovery=forward-only; complete=yes; self-contained=yes; AC=3; VER=1; landing=1; prod-files=1; scope=M; deps=HM-00; writeset=sync.rs 与 cloud 文档; release=no-release; split-from=本目标补齐 PI-05 未交付的写前拒绝; exception=N/A`

### Task HM-01: Repository Hermes identity与role迁移

**Task type:** migration

**Lifecycle / Acceptance:** done / complete

**Description:** 自洽增加Hermes enum/registry stub及Repository CHECK，仍unsupported。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测和其它计划的行为重构。本卡不重构无关owner或扩张其它能力。

**Acceptance criteria:**

- [x] 迁移ID高于当前tip且唯一，CHECK只扩展当时完整集合，保留Pi。
- [x] enum/db/CLI/adapter穷尽映射与CHECK配对，stub禁用capture/安装/launch。
- [x] 只走PI-07专用连接；全部级联child预置数据的正常/每步失败/崩溃摘要保留，FK/index/trigger完整，重复up幂等。
- [x] Global/System/Derived零schema/receipt写；旧binary future receipt零写拒绝。
- [x] 有Hermes数据禁止down/drop，未知kind继续拒绝。

**Verification:**

- [x] `source .env.test && cargo test --test agent_capture_migration_test hermes_role_ -- --test-threads=1`（拟新增过滤名；须列出实际匹配并确认非零）

实现说明：迁移 `2026103001_agent_kind_hermes` 高于当时 tip `2026102201`。CHECK 在当时完整八值（含 `grok` 与 `opencode`）上增加 `hermes`。`pi` 不是当时集合的一员，PI-01 尚未落地，继续拒绝。stub 关闭 capture、安装和 launch。云白名单与 D1 CHECK 未加入 `hermes`。`hermes_role_` 匹配 2 条并全部通过。Claude 实现审查 `VERDICT: PASS`，正文 SHA-256 `5ac91d5378ef0eadad7002bda5fb90be57bc4b616df22d3477c825f62db6804c`。无 P0/P1。P2：侧库夹具是字节文件；future receipt 用 `2099010101`；allowlist 断言是源码窗口。发布 `v0.30.93`，commit `55fe7bbb83844ac8a158dafdd2891ffec6f0e4c3`，release.yml run `40927750879` conclusion success。

**Full-suite trigger:** T-1/T-4（共享采集、存储、CLI 或测试契约）；执行四 provider 契约的完整双环境 nextest 门

**Dependencies:** HM-02、DEP-HM-DB、DEP-HM-02（PI-01 source交接）

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** 下列路径中的本卡单一行为、回归测试及同卡文档；同名拟新增测试在实现时落地。

**Implementation write set:** `sql/migrations/`（仅开工分配并登记的up/down两文件）、`sql/migrations/README.md`、`src/internal/db/migration.rs`、`src/internal/db/schema.rs`、`src/internal/ai/observed_agents/adapter.rs`、`src/internal/ai/observed_agents/registry.rs`、`src/internal/ai/observed_agents/mod.rs`、`src/internal/ai/observed_agents/preview.rs`、`tests/agent_capture_migration_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`docs/development/tracing/agent.md`、`COMPATIBILITY.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`tests/INDEX.md`、`Cargo.toml`、`src/internal/ai/capture/extraction.rs`、`src/internal/ai/observed_agents/extract.rs`、`tests/compat/agent_docs_contract.rs`、`tests/compat/agent_capability_matrix_pin.rs`、`tests/compat/agent_architecture_guard.rs`；本计划与 `docs/development/plan/plan-status.md` 仅更新本卡状态。

**Release write set:** Inherited：`compat_version_surface_sync` 权威集合 + 工具链生成的 `Cargo.lock` + release artifacts

**Docs and compatibility impact:** 上述 EN/zh-CN/development/COMPATIBILITY 与网站页面逐文件同步并执行网站交付门。 新稳定错误码才同步 `docs/error-codes.md`；拟新增 target 须同卡在 `Cargo.toml`、`tests/INDEX.md` 注册，compat target 另更新 `tests/compat/README.md`。

**Rollback mode:** forward-only

**Migration and rollback:** 保留已写 session、checkpoint、schema receipt 和旧格式读能力，停用新增入口后前滚修复；不得用 down/drop/重写 traces 消除用户事实。迁移卡还须执行本计划数据库角色合同。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；四 provider 执行契约的逐卡 C/D

**D 组:** `plan-20260904.md` D-CAP-STD；workflow/event/ref/source SHA与全部实际job逐卡取证，未完remote-pending。

**Granularity:** `type=migration; axis=Repository Hermes identity与role迁移; recovery=forward-only; complete=yes; self-contained=yes; AC=5; VER=1; landing=4; prod-files=10; scope=M; deps=HM-02、DEP-HM-DB、DEP-HM-02（PI-01 source交接）; writeset=本计划链及DEP-CAP-WRITE逐文件串行; release=independent; split-from=本轮G-09修订表; exception=N/A`

### Task HM-03: 安全Hermes source adapter

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Description:** 注册 builtin/hermes.rs 替换 preview stub，读取「HM-00 版本冻结」选定的官方 JSONL。保留 `active` 或 `compacted` 为真的行，丢掉两者都为假的撤销行；三列 JSON 保持原始字符串。该 export 不可用时才用一致只读 SQLite 后备。内部 adapter 不开放安装。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测和其它计划的行为重构。本卡不重构无关owner或扩张其它能力。

**Acceptance criteria:**

- [x] 接受 HM-00 核定的 source shape/版本；官方 export 缺 thinking 时选文件或一致只读 SQLite，不因没有官方 exporter 停止全部获取。
- [x] consent/root/no-follow/regular 先于 bytes；SQLite 的数据库角色为外部 provider source，只读零 schema writer，不经过 Libra Repository/Global/System migration；用只读连接/事务或 backup API 保持含 WAL 的一致视图，禁止写主 DB/WAL/schema 或调用自动迁移构造器；必要 -shm 协调遵守 ADR-HM-03；shape/session 与矩阵一致。
- [x] profile/session/cwd当前repo/worktree匹配，gateway/remote/cross-user拒绝。
- [x] source/row/枚举预算受限，busy/oversize/漂移返回partial或拒绝，不显示原始SQL/body/path。

**Verification:**

- [x] `source .env.test && cargo test --test agent_import_test hermes_source_ -- --test-threads=1`（拟新增过滤名；须列出实际匹配并确认非零）

实现说明：`builtin/hermes.rs` 读取官方 JSONL。`active` 或 `compacted` 为真才保留。非空 `reasoning` 或 `reasoning_content` 是可读 thinking。三列 JSON 保持原始字符串。导出缺 thinking 列或不可用时才走只读 SQLite，不用 `immutable=1`，不迁移 provider 库。来源只接受 `cli`。`hermes_source_` 匹配 3 条并全部通过。Claude R1 `VERDICT: FAIL`（来源名单过宽、SQLite 行预算可能假完整）。R2 `VERDICT: PASS`，正文 SHA-256 `0a20cff33299f625a702d2430681496fd8951a1831fcfe9306c8dc1e7e979e71`。无 P0/P1。发布 `v0.30.94`，commit `11a14284c2a9ff18d4c5de01b3c1090c1c77bb9e`，release.yml run `40938239386` conclusion success。

**Full-suite trigger:** T-1/T-4（共享采集、存储、CLI 或测试契约）；执行四 provider 契约的完整双环境 nextest 门

**Dependencies:** HM-01

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** 下列路径中的本卡单一行为、回归测试及同卡文档；同名拟新增测试在实现时落地。

**Implementation write set:** `src/internal/ai/observed_agents/builtin/hermes.rs`、`src/internal/ai/observed_agents/transcript_source.rs`、`src/internal/ai/observed_agents/mod.rs`、`tests/agent_import_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`docs/development/tracing/agent.md`、`COMPATIBILITY.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`tests/INDEX.md`、`Cargo.toml`、`tests/compat/agent_docs_contract.rs`、`src/internal/ai/observed_agents/builtin/mod.rs`、`src/internal/ai/observed_agents/preview.rs`；本计划与 `docs/development/plan/plan-status.md` 仅更新本卡状态。

**Release write set:** Inherited：`compat_version_surface_sync` 权威集合 + 工具链生成的 `Cargo.lock` + release artifacts

**Docs and compatibility impact:** 上述 EN/zh-CN/development/COMPATIBILITY 与网站页面逐文件同步并执行网站交付门。 新稳定错误码才同步 `docs/error-codes.md`；拟新增 target 须同卡在 `Cargo.toml`、`tests/INDEX.md` 注册，compat target 另更新 `tests/compat/README.md`。

**Rollback mode:** forward-only

**Migration and rollback:** 保留已写 session、checkpoint、schema receipt 和旧格式读能力，停用新增入口后前滚修复；不得用 down/drop/重写 traces 消除用户事实。迁移卡还须执行本计划数据库角色合同。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；四 provider 执行契约的逐卡 C/D

**D 组:** `plan-20260904.md` D-CAP-STD；workflow/event/ref/source SHA与全部实际job逐卡取证，未完remote-pending。

**Granularity:** `type=implementation; axis=安全Hermes source adapter; recovery=forward-only; complete=yes; self-contained=yes; AC=4; VER=1; landing=2; prod-files=5; scope=M; deps=HM-01; writeset=本计划链及DEP-CAP-WRITE逐文件串行; release=independent; split-from=本轮G-09修订表; exception=N/A`

### Task HM-04: Hermes普通内容归一化与显式import

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Description:** 将授权source写入既有snapshot/coverage/claim store，保持local-only。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测和其它计划的行为重构。本卡不重构无关owner或扩张其它能力。

**Acceptance criteria:**

- [x] 消息按已证稳定ordinal/turn排序；镜像/重试/失败usage不重复，未知usage不是0。
- [x] prompt/assistant/tool 沿用原规则；HM-04 不解释 thinking 但须保留 source 中的相关值交 HM-07，不能先剥离；未认证只降来源状态。
- [x] 同session/turn/branch重放与crash恢复不重复checkpoint，失败不false complete。
- [x] 跨profile/cwd/无证parent不关联，session/intent先提交再写对象/ref，导入不提升supported或launcher。

**Verification:**

- [x] `source .env.test && cargo test --test agent_import_test hermes_normalize_ -- --test-threads=1`（拟新增过滤名；须列出实际匹配并确认非零）

实现说明：`normalize_hermes_jsonl` 按消息 `id` 排序，丢掉撤销行，重试行只留最后一条。usage 只读 session 计数；缺字段是未知，不是 0。thinking 五列原样留在 `hermes_retained`，不进入 assistant 回答，来源状态是 `unproven`。`libra agent import --path <session-id>.jsonl --agent hermes` 不扫描 Hermes home。失败 finish 使导入为 partial。重复导入不新增 checkpoint。安装、启动和 `agent list` 仍不含 hermes。`hermes_normalize_` 匹配 2 条并全部通过。Claude `VERDICT: PASS`，正文 SHA-256 `3b96431518beca3ca1db3c208ca5eb396485e5704a26a7cc66747ab068191c2d`。无 P0/P1。发布 `v0.30.95`，commit `1759d12c279f7058490a7a55b60d03879ad45389`，release.yml run `40951545332` conclusion success。

**Full-suite trigger:** T-1/T-4（共享采集、存储、CLI 或测试契约）；执行四 provider 契约的完整双环境 nextest 门

**Dependencies:** HM-03

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** 下列路径中的本卡单一行为、回归测试及同卡文档；同名拟新增测试在实现时落地。

**Implementation write set:** `src/internal/ai/observed_agents/builtin/hermes.rs`、`src/internal/ai/observed_agents/coverage.rs`、`src/internal/ai/observed_agents/extract.rs`、`src/internal/ai/agent_import.rs`、`src/command/agent/import.rs`、`tests/agent_import_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`docs/development/tracing/agent.md`、`COMPATIBILITY.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`tests/INDEX.md`、`Cargo.toml`、`tests/compat/agent_docs_contract.rs`；本计划与 `docs/development/plan/plan-status.md` 仅更新本卡状态。

**Release write set:** Inherited：`compat_version_surface_sync` 权威集合 + 工具链生成的 `Cargo.lock` + release artifacts

**Docs and compatibility impact:** 上述 EN/zh-CN/development/COMPATIBILITY 与网站页面逐文件同步并执行网站交付门。 新稳定错误码才同步 `docs/error-codes.md`；拟新增 target 须同卡在 `Cargo.toml`、`tests/INDEX.md` 注册，compat target 另更新 `tests/compat/README.md`。

**Rollback mode:** forward-only

**Migration and rollback:** 保留已写 session、checkpoint、schema receipt 和旧格式读能力，停用新增入口后前滚修复；不得用 down/drop/重写 traces 消除用户事实。迁移卡还须执行本计划数据库角色合同。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；四 provider 执行契约的逐卡 C/D

**D 组:** `plan-20260904.md` D-CAP-STD；workflow/event/ref/source SHA与全部实际job逐卡取证，未完remote-pending。

**Granularity:** `type=implementation; axis=Hermes普通内容归一化与显式import; recovery=forward-only; complete=yes; self-contained=yes; AC=4; VER=1; landing=4; prod-files=5; scope=M; deps=HM-03; writeset=本计划链及DEP-CAP-WRITE逐文件串行; release=independent; split-from=本轮G-09修订表; exception=N/A`

### Task HM-07: Hermes有界普通采集能力

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Description:** 自HM-04/05拆出snapshot/worker与readable接线；实际live source仍受DEP-HM-LIVE约束。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测和其它计划的行为重构。本卡不重构无关owner或扩张其它能力。

**Acceptance criteria:**

- [x] 注册Hermes readable tag/normalizer/worker封闭映射，旧provider wire不变，无opaque/decrypt能力。
- [x] snapshot 接 HM-03 已选 source，保留 thinking 与附带数据并标 Unproven/Partial，thinking 不冒充 assistant answer。
- [x] worker 的有界 Hermes frame 显式携带授权 thinking；预算/取消/坏 frame 安全，ingress/snapshot/coordinator/store 保持单 owner，普通 export 与源值一致。
- [x] 同 source 的 import/live 路径一致；缺真实 thinking 数据时获取门未通过，不用 export 命令成功或合成 fixture 替代真实数据。

**Verification:**

- [x] `source .env.test && cargo test --test agent_transcript_intelligence_test hermes_bounded_capture_ -- --test-threads=1`（拟新增过滤名；须列出实际匹配并确认非零）

实现说明：`ReasoningProvider::Hermes` 的 wire tag 是 `hermes`，旧 tag 不变，没有 opaque locator，也没有 decryptor。有界 worker frame 保留非空 thinking 与原始附带字段，来源状态是 `unproven`，snapshot 完整度是 `Partial` / `unproven_ordinary`。thinking 不进入 assistant 回答。保留文本超过 8 MiB、超过 deadline 或坏 frame 时清空 transcript。合成导出的获取门保持未通过。`live_capture_for(Hermes)` 仍是 `None`，`transcript_readable` 仍是 false，云同步仍拒绝 hermes。`hermes_bounded_capture_` 匹配 2 条并全部通过。既有 `hermes_normalize_import_replays_one_checkpoint_without_enabling_hermes` 仍通过。Claude `VERDICT: PASS`，正文 SHA-256 `715c7073eed592ffd46ef4ac14b5d10f31fb67e269dd1a363a264feb4a185784`。无 P0/P1。发布 `v0.30.98`，commit `760a5529f9840736bf9db9b0a148201481f1ad36`，release.yml run `40963348005` conclusion success。上游已占用 `v0.30.97`。

**Full-suite trigger:** T-1/T-4（共享采集、存储、CLI 或测试契约）；执行四 provider 契约的完整双环境 nextest 门

**Dependencies:** HM-04、DEP-CAP-BASE、DEP-CAP-RG

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** 下列路径中的本卡单一行为、回归测试及同卡文档；同名拟新增测试在实现时落地。

**Implementation write set:** `src/internal/ai/observed_agents/reasoning.rs`、`src/internal/ai/observed_agents/coverage.rs`、`src/internal/ai/observed_agents/extract.rs`、`src/internal/ai/observed_agents/live_capture.rs`、`src/internal/ai/observed_agents/builtin/hermes.rs`、`src/internal/ai/capture/extraction.rs`、`src/internal/ai/capture/snapshot.rs`、`tests/agent_transcript_intelligence_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`docs/development/tracing/agent.md`、`COMPATIBILITY.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`tests/INDEX.md`、`Cargo.toml`、`tests/compat/agent_docs_contract.rs`；本计划与 `docs/development/plan/plan-status.md` 仅更新本卡状态。

**Release write set:** Inherited：`compat_version_surface_sync` 权威集合 + 工具链生成的 `Cargo.lock` + release artifacts

**Docs and compatibility impact:** 上述 EN/zh-CN/development/COMPATIBILITY 与网站页面逐文件同步并执行网站交付门。 新稳定错误码才同步 `docs/error-codes.md`；拟新增 target 须同卡在 `Cargo.toml`、`tests/INDEX.md` 注册，compat target 另更新 `tests/compat/README.md`。

**Rollback mode:** forward-only

**Migration and rollback:** 保留已写 session、checkpoint、schema receipt 和旧格式读能力，停用新增入口后前滚修复；不得用 down/drop/重写 traces 消除用户事实。迁移卡还须执行本计划数据库角色合同。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；四 provider 执行契约的逐卡 C/D

**D 组:** `plan-20260904.md` D-CAP-STD；workflow/event/ref/source SHA与全部实际job逐卡取证，未完remote-pending。

**Granularity:** `type=implementation; axis=Hermes有界普通采集能力; recovery=forward-only; complete=yes; self-contained=yes; AC=4; VER=1; landing=3; prod-files=7; scope=M; deps=HM-04、DEP-CAP-BASE、DEP-CAP-RG; writeset=本计划链及DEP-CAP-WRITE逐文件串行; release=independent; split-from=本轮G-09修订表; exception=N/A`

### Task HM-08: Hermes已选thinking来源的live读取桥

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Description:** 承接 ADR-HM-03 选定 source 的 live 读取与既有 snapshot/lease 接线；文件/SQLite 直接走 HM-03 reader，不另造 exporter，只有 exporter 分支使用已有 trust 机制。不借 OpenCode trust 冒充 Hermes 授权。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测和其它计划的行为重构。本卡不重构无关owner或扩张其它能力。

**Acceptance criteria:**

- [x] 记录所选文件/SQLite/export source 和当前 session；若执行系统 Hermes exporter，按现有机制取得该 executable trust，不要求额外解释器/package 闭包认证。
- [x] 只读 reader 不初始化/迁移 provider DB；exporter 分支使用 fixed argv、明确 cwd/PWD 和必要环境，不读取凭据文件作诊断。
- [x] 不要求建设专用 Hermes 沙箱才可获取 ordinary 数据；沿现有受信任本地用户边界读取当前会话，不放宽其它 provider profile。
- [x] source 读取或 exporter 采用 HM-00 核定的当前 session 和有界策略；live hook 2s 总 deadline，超时 partial 并可显式重试。
- [x] 通过既有LiveTranscriptExporter/lease owner运行，未持lease不启动。
- [x] 输出仅经授权source关联进入snapshot，stdout/stderr/log不泄露raw内容。
- [x] 公开trust/doctor的Hermes分支、Display、EN/zh/网站与compat同卡验证。
- [x] 真实 live 数据比对前只算机制 source-qualified；不承诺 native 来源或同 UID 防篡改，缺这些保证不删除已取得 thinking。

**Verification:**

- [x] `source .env.test && cargo test --test command_test agent_rpc_trust_test::hermes_exporter_ -- --test-threads=1`（拟新增过滤名；须列出实际匹配并确认非零）
- [x] `source .env.test && cargo test --test agent_hook_crash_test hermes_source_bridge_ -- --test-threads=1`（拟新增过滤名；须列出实际匹配并确认非零）

实现说明：`hermes_export` 只在持有 export-job lease 时启动。argv 固定为 `sessions export --format jsonl --session-id <id> <output-file>`，cwd 与 `PWD` 明确，环境只留 `PATH`。stdout 只用来识别不可用，stderr 丢弃。JSONL 与 SQLite 走 HM-03 reader，`spawned=false`，不迁移 provider 库。启动前在同一条连接上做 `revalidate_trust_with_conn`；漂移或目录不在允许名单时撤销该记录且不启动。session id 与导入相同：非空、最长 64、ASCII 字母数字或 `-`/`_`。超时 2 秒得到 partial，可以再次取得 lease 重试。结果始终 source-qualified，不声称 native。`live_capture_for(Hermes)` 仍是 `None`。`transcript_readable` 与 hook 安装仍关闭，云同步仍拒绝 hermes。`rpc trust hermes` 不借用 OpenCode trust。`hermes_exporter_` 匹配 1 条，`hermes_source_bridge_` 匹配 5 条，全部通过。Claude R1 `VERDICT: FAIL`（启动前未复核信任、session id 被收成 32 位十六进制）。R2 `VERDICT: PASS`，正文 SHA-256 `746fdc4478b3a22ad5d3a982d51bf0daef93bdc186f1b4f2eaff7a0df3a441dd`。无 P0/P1。发布 `v0.30.99`，commit `6062e382f0099f396f0056438beb5e467a058f68`，release.yml run `40972429750` conclusion success。

**Full-suite trigger:** T-1/T-4（共享采集、存储、CLI 或测试契约）；执行四 provider 契约的完整双环境 nextest 门

**Dependencies:** HM-07、DEP-HM-LIVE

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** 下列路径中的本卡单一行为、回归测试及同卡文档；同名拟新增测试在实现时落地。

**Implementation write set:** `src/internal/ai/observed_agents/hermes_export.rs`、`src/internal/ai/observed_agents/trust.rs`、`src/internal/ai/observed_agents/mod.rs`、`src/internal/ai/observed_agents/live_capture.rs`、`src/command/agent/rpc.rs`、`src/command/agent/doctor.rs`、`tests/command/agent_rpc_trust_test.rs`、`tests/agent_hook_crash_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`docs/development/tracing/agent.md`、`COMPATIBILITY.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`tests/INDEX.md`、`Cargo.toml`、`tests/compat/agent_docs_contract.rs`；本计划与 `docs/development/plan/plan-status.md` 仅更新本卡状态。

**Release write set:** Inherited：`compat_version_surface_sync` 权威集合 + 工具链生成的 `Cargo.lock` + release artifacts

**Docs and compatibility impact:** 上述 EN/zh-CN/development/COMPATIBILITY 与网站页面逐文件同步并执行网站交付门。 新稳定错误码才同步 `docs/error-codes.md`；拟新增 target 须同卡在 `Cargo.toml`、`tests/INDEX.md` 注册，compat target 另更新 `tests/compat/README.md`。

**Rollback mode:** forward-only

**Migration and rollback:** 保留已写 session、checkpoint、schema receipt 和旧格式读能力，停用新增入口后前滚修复；不得用 down/drop/重写 traces 消除用户事实。迁移卡还须执行本计划数据库角色合同。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；四 provider 执行契约的逐卡 C/D

**D 组:** `plan-20260904.md` D-CAP-STD；workflow/event/ref/source SHA与全部实际job逐卡取证，未完remote-pending。

**Granularity:** `type=implementation; axis=Hermes已选thinking来源的live读取桥; recovery=forward-only; complete=yes; self-contained=yes; AC=8; VER=2; landing=2; prod-files=6; scope=M; deps=HM-07、DEP-HM-LIVE; writeset=本计划链及DEP-CAP-WRITE逐文件串行; release=independent; split-from=本轮G-09修订表; exception=N/A`

### Task HM-09: Hermes本地CLI与scope准入过滤

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Description:** 自HM-05拆出的纯平台/profile/cwd准入轴，生产者与receiver使用同一冻结策略，拒绝发生在正文序列化/读取前。本卡仅在providers/mod.rs声明pub mod hermes，提供可调用纯parser/scope API，不注册HookProvider；HM-05之后才接线注册。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测和其它计划的行为重构。本卡不重构无关owner或扩张其它能力。

**Acceptance criteria:**

- [x] 仅HM-00已证的本地CLI platform可进入。
- [x] 缺cwd拒绝。
- [x] cwd必须等于现有CAP已验证的仓库/worktree事实。
- [x] profile 必须等于 HM-08 所选 source 已记录的当前 session/profile；不从全局默认或 event 猜根。
- [x] 拒绝路径不建立capture row。
- [x] producer与receiver的准入策略由同一fixture矩阵生成并一致。

**Verification:**

- [x] `source .env.test && cargo test --lib internal::ai::hooks::providers::hermes::scope::tests::hermes_scope_admission_ -- --test-threads=1`（拟新增过滤名；须列出实际匹配并确认非零）

实现说明：`providers/mod.rs` 只声明 `pub mod hermes`，没有 `HookProvider`，也没有 `hermes_provider()`。`admit_producer` 与 `admit_receiver` 都调用 `admit_hermes_scope`。只接受 `source=cli`；`created_source` 出现时也必须是 `cli`。cwd 必须等于调用方已经验证的 worktree 根，子目录、相对路径和 `..` 都拒绝。`profile_name` 必须等于调用方给出的已记录 profile；缺记录时只匹配显式的 `default`。event 里的 `profile` 与 `event_cwd` 不参与。parser 在读取消息正文前返回。拒绝和放行都不写 `agent_session`。`supported`、`hook_installable`、`transcript_readable` 与 launch 仍是 false，`as_hooks()` 仍是 `None`。`hermes_scope_admission_` 匹配 4 条并全部通过。`libra_observer.py` 用同一矩阵得到相同结论，不打开 state.db。Claude `VERDICT: PASS`，正文 SHA-256 `be3a823314009bbaae4d3f18fb374598a14a7db33cd89137295e16545ab5741b`。无 P0/P1。发布 `v0.30.100`，commit `30c0e28ca82e3327be0377db259f76ea3a82a3a4`，release.yml run `40978568893` conclusion success。

**Full-suite trigger:** T-1/T-4（共享采集、存储、CLI 或测试契约）；执行四 provider 契约的完整双环境 nextest 门

**Dependencies:** HM-08

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** 下列路径中的本卡单一行为、回归测试及同卡文档；同名拟新增测试在实现时落地。

**Implementation write set:** `src/internal/ai/hooks/providers/hermes/scope.rs`、`src/internal/ai/hooks/providers/hermes/parser.rs`、`src/internal/ai/hooks/providers/hermes/mod.rs`、`src/internal/ai/hooks/providers/hermes/libra_observer.py`、`docs/development/tracing/agent.md`、`tests/INDEX.md`、`Cargo.toml`、`src/internal/ai/hooks/providers/mod.rs`；本计划与 `docs/development/plan/plan-status.md` 仅更新本卡状态。

**Release write set:** Inherited：`compat_version_surface_sync` 权威集合 + 工具链生成的 `Cargo.lock` + release artifacts

**Docs and compatibility impact:** 只更新内部契约；public roster/help/JSON 不变并有负向断言。 新稳定错误码才同步 `docs/error-codes.md`；拟新增 target 须同卡在 `Cargo.toml`、`tests/INDEX.md` 注册，compat target 另更新 `tests/compat/README.md`。

**Rollback mode:** forward-only

**Migration and rollback:** 保留已写 session、checkpoint、schema receipt 和旧格式读能力，停用新增入口后前滚修复；不得用 down/drop/重写 traces 消除用户事实。迁移卡还须执行本计划数据库角色合同。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；四 provider 执行契约的逐卡 C/D

**D 组:** `plan-20260904.md` D-CAP-STD；workflow/event/ref/source SHA与全部实际job逐卡取证，未完remote-pending。

**Granularity:** `type=implementation; axis=Hermes本地CLI与scope准入过滤; recovery=forward-only; complete=yes; self-contained=yes; AC=6; VER=1; landing=2; prod-files=5; scope=M; deps=HM-08; writeset=本计划链及DEP-CAP-WRITE逐文件串行; release=independent; split-from=本轮G-09修订表; exception=N/A`

### Task HM-05: Hermes observer plugin与现有ingress

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Description:** 用已证明observer注册fixed-argv/stdin协议，接现有ACF；安装曝光留HM-06。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测和其它计划的行为重构。本卡不重构无关owner或扩张其它能力。

**Acceptance criteria:**

- [x] 只注册HM-00已证observer，返回值无控制含义。
- [x] turn outcome不伪造成SessionEnd。
- [x] 稳定session/turn/message/phase键重放幂等。
- [x] stdin只含冻结的allowlist字段。
- [x] child生命周期满足共享契约第5条资源合同。
- [x] source只经HM-08桥交给snapshot，plugin不读热DB。
- [x] terminal只注入既有CaptureFinalizePolicy。
- [x] 只有HM-09已准入的context才能摄取。

**Verification:**

- [x] `source .env.test && cargo test --test agent_lifecycle_event_test hermes_observe_ -- --test-threads=1`（拟新增过滤名；须列出实际匹配并确认非零）

实现说明：`libra agent hooks hermes observe` 只接受 `post_llm_call`、`on_session_end`、`on_session_finalize`、`on_session_reset` 和 `agent_loop_stopped`。control hook 与 subagent 事件不注册，返回值没有控制内容。`on_session_end` 的 completed 是 TurnEnd，会话保持 active；failed、interrupted 和 `agent_loop_stopped` 是 TurnInterrupted。只有 `on_session_finalize` 是 SessionEnd，并走现有 agent-traces ingest 的 `CaptureFinalizePolicy`。plugin 用固定 argv、无 shell、2 秒超时，只转发身份字段，不读 state.db，也不接受 `transcript_path`。cwd 必须等于 `RequestScope` 解析出的 worktree 根，子目录不写入。gateway 不建立 capture row。相同 event/turn/message/phase 重放不新增 checkpoint。安装、`as_hooks()`、supported 与 launch 仍关闭。`hermes_observe_` 匹配 5 条并全部通过。Claude R1 `VERDICT: FAIL`（子目录 cwd 被当成已验证根）。R2 `VERDICT: PASS`，正文 SHA-256 `c150149ff2daa5e2bdd647d2e89895b03a933663aa7c55dc82353e6ae3766a2c`。无 P0/P1。发布 `v0.30.101`，commit `a3a03804799cf362e247ee34f0f639029637340b`，release.yml run `40984893884` conclusion success。

**Full-suite trigger:** T-1/T-4（共享采集、存储、CLI 或测试契约）；执行四 provider 契约的完整双环境 nextest 门

**Dependencies:** HM-09、DEP-HM-LIVE

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** 下列路径中的本卡单一行为、回归测试及同卡文档；同名拟新增测试在实现时落地。

**Implementation write set:** `src/internal/ai/hooks/providers/hermes/mod.rs`、`src/internal/ai/hooks/providers/hermes/parser.rs`、`src/internal/ai/hooks/providers/hermes/settings.rs`、`src/internal/ai/hooks/providers/hermes/libra_observer.py`、`src/internal/ai/hooks/providers/mod.rs`、`src/command/agent/hooks.rs`、`tests/agent_lifecycle_event_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`docs/development/tracing/agent.md`、`COMPATIBILITY.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`tests/INDEX.md`、`Cargo.toml`、`tests/compat/agent_docs_contract.rs`；本计划与 `docs/development/plan/plan-status.md` 仅更新本卡状态。

**Release write set:** Inherited：`compat_version_surface_sync` 权威集合 + 工具链生成的 `Cargo.lock` + release artifacts

**Docs and compatibility impact:** 上述 EN/zh-CN/development/COMPATIBILITY 与网站页面逐文件同步并执行网站交付门。 新稳定错误码才同步 `docs/error-codes.md`；拟新增 target 须同卡在 `Cargo.toml`、`tests/INDEX.md` 注册，compat target 另更新 `tests/compat/README.md`。

**Rollback mode:** forward-only

**Migration and rollback:** 保留已写 session、checkpoint、schema receipt 和旧格式读能力，停用新增入口后前滚修复；不得用 down/drop/重写 traces 消除用户事实。迁移卡还须执行本计划数据库角色合同。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；四 provider 执行契约的逐卡 C/D

**D 组:** `plan-20260904.md` D-CAP-STD；workflow/event/ref/source SHA与全部实际job逐卡取证，未完remote-pending。

**Granularity:** `type=implementation; axis=Hermes observer plugin与现有ingress; recovery=forward-only; complete=yes; self-contained=yes; AC=8; VER=1; landing=3; prod-files=6; scope=M; deps=HM-09、DEP-HM-LIVE; writeset=本计划链及DEP-CAP-WRITE逐文件串行; release=independent; split-from=本轮G-09修订表; exception=N/A`

### Task HM-06: Hermes采集支持提升与交付

**Task type:** implementation

**Lifecycle / Acceptance:** done / complete

**Description:** 匹配版本真实smoke后才公开capture enable/disable/list。Claude review R2 `VERDICT: PASS`，正文 SHA-256 `01096473306808f5cbc3243a8e4cc04d182c45c9e78fdc22594bc6458db74fe7`。v0.30.102 的 release.yml run `40995359624` 在 Windows 构建失败（`libc` 只在 Unix 依赖里）。前滚窗口 v0.30.103 已发布成功：peeled commit `b2891311eeb5070ae2e573c82ee5a9d6785f67bf`，annotated tag object `68415ac56d7be0c3e27cb11488a5290f1d7c5ac0`，release.yml run `40998413942` conclusion success。八个 job 全部 success，包括 Windows amd64。不改写 v0.30.102 或 v0.30.103。之后的测试与记账提交不打 tag。隔离 profile `/Volumes/Data/hermes-hm06-profile` 上的 Hermes v0.22.5 `bf0203a` 用本地 Ollama `qwen3:0.6b` 写了 TUI snake。网站实际 checkout 是 `/Volumes/Data/libra-backend`。普通 export 不带 `--redact`。session `local-38c05ed6c90ffe72345ab2f03dcec9236e043fe142a006adc5edd87430a41388` 的 assistant message 9 上 `reasoning` 与 `reasoning_content` 都是 2180 字符且相等，`active=1`。`source=local`、`created_source=cli`、`user_id=uid:501`、cwd 等于临时仓库根。同一会话被 hook 记成 active，并有一条 checkpoint。自动 checkpoint 通过 `read_hermes_live_source` 读取固定 argv 的 JSONL 导出，普通 `checkpoint export` 保留非空 `reasoning` 与 `reasoning_content`。这次接线的 Claude 审查是 `VERDICT: PASS`，正文 SHA-256 `2cb0b57d3f7639c7d502199b383e4114c9873f7947adeea2a97f3dee55cf6b63`。没有读取 `~/.hermes` 的 state、sessions、config 或 `.env`。CI 仍只用 fake。`cloud sync` 仍拒绝 `hermes`。review 与 investigate 仍不启动。

**Current evidence:** `adapter.rs:30` 的 `AgentKind` 无 Hermes；`registry.rs:134` 的 8 行注册表无 hermes；`capture/extraction.rs:438` 对其余 kind 返回 partial；`migration.rs:572` 的 CHECK 扩展只到 grok。v0.22.5 的 callsite 与哈希见「HM-00 版本冻结」。

**Out of scope:** 未登记的公共命令、provider 私有执行器、解密、凭据探测和其它计划的行为重构。本卡不重构无关owner或扩张其它能力。

**Acceptance criteria:**

- [x] install/update/remove仅marker+hash确认为Libra-owned asset，unmanaged/漂移文件不覆盖。
- [x] 缺binary/config时bare enable安全skip；unsupported时explicit enable无副作用。
- [x] 隔离本机系统 Hermes 实现 TUI 贪吃蛇，验证 install/capture/自动 checkpoint/普通 export 与 source 非空 thinking 逐值一致，再 doctor/disable；失败/中断恢复分别验证。
- [x] supported/capability/roster/help与docs同批，review/investigate launchable保持false。
- [x] cloud-local-only、supported pin、覆盖缺口与恢复写入agent/cloud文档及网站；CI只用fake。

**Verification:**

- [x] `source .env.test && cargo test --test command_test agent_roster_test::hermes_ -- --test-threads=1`（`hermes_owned_asset_is_not_overwritten_when_unmanaged_or_drifted` 与 `hermes_missing_runtime_skips_bare_enable_and_refuses_explicit`，2 passed）

**Full-suite trigger:** T-1/T-4（共享采集、存储、CLI 或测试契约）；执行四 provider 契约的完整双环境 nextest 门

**Dependencies:** HM-05、DEP-HM-03、DEP-HM-DOC、DEP-HM-LIVE

**Blocking condition:** 依赖产物缺失、source pin 不可复验、目标文件有未交接写者或本卡任一安全门失败即 blocked；计划 PASS 只放行选定卡的工作，不替代后继的实际依赖与验收。

**Deliverables:** 下列路径中的本卡单一行为、回归测试及同卡文档；同名拟新增测试在实现时落地。

**Implementation write set:** `src/internal/ai/observed_agents/registry.rs`、`src/command/agent/mod.rs`、`src/command/agent/list.rs`、`src/command/agent/doctor.rs`、`tests/command/agent_roster_test.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`docs/development/tracing/agent.md`、`COMPATIBILITY.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`docs/commands/cloud.md`、`docs/commands/zh-CN/cloud.md`、`docs/development/commands/cloud.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`、`tests/INDEX.md`、`Cargo.toml`、`tests/compat/agent_docs_contract.rs`、`tests/compat/agent_capability_matrix_pin.rs`、`tests/compat/agent_architecture_guard.rs`；本计划与 `docs/development/plan/plan-status.md` 仅更新本卡状态。

**Release write set:** Inherited：`compat_version_surface_sync` 权威集合 + 工具链生成的 `Cargo.lock` + release artifacts

**Docs and compatibility impact:** 上述 EN/zh-CN/development/COMPATIBILITY 与网站页面逐文件同步并执行网站交付门。 新稳定错误码才同步 `docs/error-codes.md`；拟新增 target 须同卡在 `Cargo.toml`、`tests/INDEX.md` 注册，compat target 另更新 `tests/compat/README.md`。

**Rollback mode:** forward-only

**Migration and rollback:** 保留已写 session、checkpoint、schema receipt 和旧格式读能力，停用新增入口后前滚修复；不得用 down/drop/重写 traces 消除用户事实。迁移卡还须执行本计划数据库角色合同。

**Security and privacy / Performance budget:** 继承 [数据获取基线 DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 与已修订四 provider 契约。旧有未证明字段剥离、仅脱敏 worker、native 来源前置不再阻塞普通 thinking 保存；scope/consent、数据不损坏、资源预算与日志不回显继续适用。本卡未执行条款与新基线冲突时按新基线解释，不取旧隐私门的更严格值。

**Estimated scope:** M

**Version increment:** patch

**Release boundary:** independent

**C/D coverage from:** self；四 provider 执行契约的逐卡 C/D

**D 组:** `plan-20260904.md` D-CAP-STD；workflow/event/ref/source SHA与全部实际job逐卡取证，未完remote-pending。

**Granularity:** `type=implementation; axis=Hermes采集支持提升与交付; recovery=forward-only; complete=yes; self-contained=yes; AC=5; VER=1; landing=2; prod-files=4; scope=M; deps=HM-05、DEP-HM-03、DEP-HM-DOC、DEP-HM-LIVE; writeset=本计划链及DEP-CAP-WRITE逐文件串行; release=independent; split-from=本轮G-09修订表; exception=N/A`

## 测试矩阵与追溯表

每张卡的 Current evidence → AC → Verification 是本计划追溯链。审计核 source/version/字段矩阵；配置卡核 no-follow、ownership、安装/卸载；parser 卡核合成 source、重放、截断；落库卡核 scope/redaction、事务/claim/CAS、崩溃恢复；公开卡核 CLI/JSON/docs/网站；迁移卡核 role/旧 reader/数据保留。每个新过滤名均是待实现测试，不能把当前零匹配视为成功。`--exact` 只用于已确认完整测试名，模块过滤不加 `--exact`。

## 里程碑验收与回滚

审计卡给逐能力 GO/HOLD/NO-GO，不把未知事项交给实现者猜测；后继只消费明确 GO 的范围。配置或 parser 失败保持旧入口并报告 partial；pending/replay 失败保持既有事实和恢复键；发布失败前滚，不能把 locally-accepted 记为 complete。恢复动作与每卡 Rollback mode 一致。

## 风险登记与性能容量摘要

上游漂移、输入伪造、缺失稳定 identity、半行/坏尾、并发 source 变化、重复事件、写入中断、预算耗尽和普通 sink 泄露分别由 source pin、scope/授权、拒绝无证关联、partial、同 snapshot、幂等键、故障注入、有界资源及 canary 负向测试覆盖。预算见 [四 provider 执行契约](plan-20260904.md#capture-review-20261018)；不能承诺任意宿主 I/O 的绝对实时上限。

## 兼容与文档收口

每卡 I 集中的文档与测试索引是交付内容；计划修订自身没有新的产品行为，不提前修改 agent/cloud 公共文案或网站。未来公开行为卡须按四 provider 契约同步并验证网站。默认收集同意与 raw-export 授权继续沿用；不宣称支持尚未验证的 provider、ciphertext 或 launcher。

## 非目标与延后项

provider decryptor、寻找密钥、跨 provider 仿造 native issuer、把未证明字段认证为 native artifact 不在本轮。授权 assistant reasoning 中的附带数据可作为 Unproven 普通内容保留；native 认证另行规划，不阻塞数据获取。缺上游数据必须报告 unavailable，不能以空壳或仅统计冒充获取成功。

## 完成判据

- [x] 按 DATA-CAP-20261019 完成真实本机会话 → 自动 checkpoint → 普通导出逐字段比对；thinking 数据非空且未被隐私过滤丢弃。可读正文与仅附带 opaque 数据分别记账；空正文/空数据不算数据获取通过，Unproven/Partial 不单独阻塞保存。
- [x] HM-00, HM-02, HM-01, HM-03, HM-04, HM-07, HM-08, HM-09, HM-05, HM-06 均按本修订逐卡实际验收，Lifecycle=done / Acceptance=complete；本次计划审查不增加实现完成数。
- [x] 当前最终源码执行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`；前滚修复发现的全部失败后重跑至绿。
- [x] 同一最终实现经 Claude Code PASS，逐卡签名/DCO main 交付、四平台 release、安装、网站及安全 CI 的实际 C/D 已记录；Source/本机合格与远端完成分开。
- [x] 所有审核/验收内容写入本计划；末端纯记账提交自己的 CI 在下一次 plan-status 例行同步补记 SHA/run/conclusion/UTC，不为自引用制造无限收口提交（模板 v2.14 预登记）。

收口记录：隔离 profile 的普通 export 里 `reasoning` 与 `reasoning_content` 都是 2180 字符且相等，`active=1`，没有被 `--redact` 丢掉。十张卡都是 done/complete。v0.30.103 的 release.yml run `40998413942` 八个 job 全部 success。网站在 `/Volumes/Data/libra-backend` 的 `cf` `c8e3ce1844e2cf198265748cae8dd23bc2689c9f`；`gen:docs` 没有跑。本仓库没有 `.env.test` 和 `.env.live-test`，所以没有 source 它们。`cargo nextest run --all --no-fail-fast --retries 2` 在 `27d78ec`（冒烟修复位于 PI-09 的 `fd88945` 之上）的结果是 run `20429095-1c99-4ab8-a73d-139ce944747f`，`10890 passed, 25 skipped`，exit 0。在此之前，`f3d38d5` 上同一命令 10866 条里只失败 `install_sh_smoke_scenarios`：`/usr/bin/openssl` 是 LibreSSL 3.3.6，报 `signature verifier unavailable`。修复只让冒烟子进程在 PATH 上的 openssl 不能 `genpkey -algorithm ed25519` 时前置 Homebrew OpenSSL 3（本机 `/opt/homebrew/opt/openssl@3/bin`，OpenSSL 3.6.5）。`install.sh` 没有改。该测试修复的 Claude 审查是 `VERDICT: PASS`，正文 SHA-256 `5053bf2637579b62235bd2ce4a579fa7178c55ea86eae677fe9ced1fbbf2a3c3`。随后变基到 `edc9347`（PI-09 的 Windows owner 修复）。`27d78ec` 那次全量没有包含 `edc9347`。在已推送的记账提交 `6ca6339a89aed5e1f1217657ee6f8f54f47e3875` 上重跑 `cargo nextest run --all --no-fail-fast --retries 2`，run `350e8fce-855d-48b6-884b-56a20da4832f`，`10890 passed, 25 skipped`，exit 0。这次包含 `edc9347` 和当时的计划正文。仓库没有 `.env.test` 与 `.env.live-test`，所以没有 source 它们。同时再次查询 release.yml run `40998413942`：status completed，conclusion success，head_sha `b2891311eeb5070ae2e573c82ee5a9d6785f67bf`，八个 job 全部 success。v0.30.103 已成功，不新开版本窗口，也不改写该 tag。其后上游把 `v0.30.104` 标在 `922e9321071b78491a06dba18b8cf92e9c56bc4b`（Windows spool 测试助手）。本记账 fast-forward 到该提交之后，不改写 `v0.30.104`。`6ca6339` 的全量不包含 `922e932`。在与 `origin/main` 一致的 `76cfe71c388b6fabbf7b59c44b09504b51241c17`（其代码包含 `922e932`）上重跑三道门：`cargo +nightly fmt --all --check` exit 0，`cargo clippy --all-targets --all-features -- -D warnings` exit 0，`cargo nextest run --all --no-fail-fast --retries 2` run `2554b095-5501-4a9f-9144-2699302e7461` 为 `10890 passed, 25 skipped`，exit 0。版本面仍是上游的 0.30.104。`cloud sync` 仍拒绝 `hermes`。review 与 investigate 仍不启动。PI-09 已由上游落地，不再写成未做。其后 `159ad9fe8f26e710a6080fbcdcb009be899ee436` 把 Hermes 自动 checkpoint 接到 `read_hermes_live_source`，普通 `checkpoint export` 保留非空且未脱敏的 `reasoning` 与 `reasoning_content`。该提交已推送，且与当时的 `origin/main` 相同。在该提交上重跑三道门：`cargo +nightly fmt --all --check` exit 0，`cargo clippy --all-targets --all-features -- -D warnings` exit 0，`cargo nextest run --all --no-fail-fast --retries 2` 为 `10891 passed, 25 skipped`，exit 0。比 `76cfe71` 多出的一条是 `hermes_observe_checkpoint_exports_nonempty_reasoning`。这次仍没有 `.env.test` 与 `.env.live-test`，所以没有 source 它们。`gen:docs` 没有跑。不改写 `v0.30.102`、`v0.30.103` 或 `v0.30.104`。真实隔离会话 `local-38c05ed6c90ffe72345ab2f03dcec9236e043fe142a006adc5edd87430a41388` 的 id 是 `local-` 加 64 位十六进制，共 70 字符，原先 argv 上限 64 会在导出前拒绝。冷启动的 `hermes sessions export` 测得约 12 秒，2 秒子进程预算会在写出 JSONL 前杀掉进程。现改为：session id 上限 128，与 hook 的 `validate_session_id` 相同；只转发绝对路径的 `HERMES_HOME`，避免读到账户默认 profile；子进程预算 20 秒，仍在 30 秒 capture wrapper 内。插件转发同样的 `HERMES_HOME`，超时 30 秒。隔离 profile `/Volumes/Data/hermes-hm06-profile` 上的 Hermes v0.22.5 经 `enable`、`list`、`doctor`、trust、`agent hooks hermes observe` 后，普通 `checkpoint export` 与当场 `sessions export` 的 message 9 上 `reasoning` 和 `reasoning_content` 都是 2180 字符且逐值相等，不含 `[REDACTED]`。`cloud sync` 仍在任何远程写之前拒绝 `hermes`。发布窗口是 `v0.30.105`：annotated tag 签在 `f6540d279119fe0baf9c0e385ebf381595f7d951`，`libra tag -v` 为 Good signature。release.yml run `41028959602` status completed，conclusion success，head_sha 与该提交相同。不改写 `v0.30.102`、`v0.30.103` 或 `v0.30.104`。

## Codex review log

2026-10-18 02:00:57 UTC，本轮根 Agent 依据 OpenCode ordinary 最终范围、当前实现和一手 source 自审；已修订上游 pin、shared capture owner、来源门、执行顺序、可恢复性与文档交付。本轮对当前规范正文自审 `VERDICT: PASS`（规划可执行性）；内部DAG/AC-VER/原始已完成块保留已核验。来源GO、真实provider smoke、逐卡实现及C/D仍须后续实际验收；Claude本轮结论另列，不以本条代替。

## Claude review log

### 版本冻结 R1（2026-10-29）

Claude Code 2.1.312，`claude -p`，permission mode `plan`，允许的工具只有 Read、Grep、Glob。没有 session id 随 text 输出返回。审查正文 SHA-256 `1379884cb9250d7b6af1be214ceb581ec8eeafb9b72c842e3e1648c74890dd97`（6199 字节，不含 runner 追加的 `EXIT:` 行）。结论 `VERDICT: FAIL`。

P1：JSONL 形状误写成 `hermes_state_messages.py:1600` 的对话恢复，实际是 `get_messages` 加 `_row_to_message_dict` 的 `dict(row)`。未写 `include_inactive=True` 会带上撤销行。Markdown 的 NO-GO 指到了 `session_export.py:120`，实际写入在 `session_export_md.py`。

P2：HTML 会被读成「没有 thinking」；只读缺列的 migration 退出未记成 unavailable；当时 plan-status 写了审查已入计划，但日志还没有这轮。

这三项 P1 和三项 P2 已改入「HM-00 版本冻结」与索引行。R1 保持 FAIL，不因后续修订改写成 PASS。审查者未用 shell 重算哈希；哈希来自本机对与 `HEAD` 字节相同的工作树的 SHA-256。

### 版本冻结 R2（2026-10-29 16:54:24 UTC）

同一只读方式复审修订后的「HM-00 版本冻结」。审查正文 SHA-256 `ae1e605f29cee9a7650a994c761f00f1f1f33420d91c89d2e385d952e5921483`（4620 字节，不含 runner 追加的 `EXIT:` 行）。结论 `VERDICT: PASS`。P0 与 P1 为 0。

留下的 P2 不改本轮被审正文，以免覆盖这次 PASS：`config_defaults.py:1756` 是注释块起点，默认值在 `:1760` 与 `:1764`；unavailable 不能只看退出码，缺文件或空文件也要核对 stdout；JSONL 只含该 session id 一段，压缩轮换的父段不在其中；HTML 的 `:723` 是按真值取 `reasoning` 或 `reasoning_content`，并且走 transcript 投影；`session_export.py:225` 的 `drop_undone_rows` 与保留行规则相同，本轮未引用。这些留给 HM-03/HM-08 实现时处理，不增加完成数。

### HM-00 收口审查（2026-10-29）

收口把上一节留下的 P2 写进「HM-00 版本冻结」。冻结轮 R1 FAIL 与 R2 PASS 不改写。三轮都是 Claude Code `claude -p`，permission mode `plan`，工具只有 Read、Grep、Glob。

| 轮 | 结论 | 正文 SHA-256 | 处置 |
|---|---|---|---|
| 收口 R1 | `VERDICT: PASS` | `a9ccb586bce1559e67c679e744f875b8c269c87ad05848473cc9c76eee240183` | P0/P1 为 0。P2 包括 `:356` 把条件与 return 并成一行、`:84` 不是 join |
| 收口 R2 | `VERDICT: FAIL` | `3b0700c7c5ba1a2f55f55824e4f8d96ef4052e4e733f32d345e66d049bbd2c94` | P1：`return _not_found` 在 `:357`，不是 `:356` |
| 收口 R3 | `VERDICT: PASS` | `7c5a28bcbb8cb17a89056bf8ebf38d9c10c1d54e23846aaee2618eb9ecd795e8` | `:356` 判定、`:357` return，以及 `:84`/`:85`/`:86` 已与源码一致。P0/P1 为 0 |

无版本变化。三处版本面仍是 `0.30.92`。本卡不打 tag。

### DATA-CAP-20261019 本次复审（2026-10-18 16:33:03 UTC）

Claude Code 只读单审 session `c03a81c0-c8e3-43c7-b633-091279513515`，R1 实际 `VERDICT: FAIL`，raw SHA256 `e8b79091373ac5b54563205edfb050227cb5f58a8cf5b7a305e3de94f48ad9df`，report SHA256 `869943e0822aed7174bd7e2cc0c46db7f9408303b5c7f6bbbaec6e40b99b958f`。B1 旧 CC-06 隔离描述、B2 CX/CC 缺真实 smoke 卡 owner、B3 PI-03 前向依赖 carrier、B4 与 OpenCode 凭据扫描矛盾均已在计划正文修订；模型设置 AC、Pi pending、Hermes reader 测试/SHM 与 mirror 说明一并补齐。R1 原结论保持 FAIL；R2 最终结论和实际验证见下。此前 R4/R5 仅属历史修订。

**本次修订最终复审与实际验证（2026-10-18 16:39:27 UTC）：** Claude Code R2 / `claude-opus-5-5` 实际 `VERDICT: PASS`，session `b56df99a-7fe2-45c7-9c6c-1fc747ad7dcf`，raw SHA256 `9c52c3e87051bb86acaca44d962a2b6c2c988932c00bec5b34259dfe16bb92b8`，report SHA256 `cb4d43a2db5ab560010bfb6a1c96d53672c641b633c9c0466dd6139c0f4591a3`；被审输入 SHA256 `fb5298e725306283489a20df83a883c07150481a4397d77a0c8b15ff6f99be06`。R2 复用 R1 已核的未变范围，只读复核修订，明确四个 blocker 全闭；root 已复核并采纳。三项非阻断措辞已窄补：CX-06 复用 CX-15 classifier、PI-03 public 文档仅格式/provenance/未支持说明、原 JSON 值条款交叉引用凭据扫描例外；不改任务范围/依赖或实现行为。R1 FAIL 原据保留，本条不把它覆盖为 PASS。

本次 `cargo +nightly fmt --all --check` exit0；`cargo clippy --locked --all-targets --all-features -- -D warnings`（独立 target-dir）exit0，最终一轮 12.72s，仅依赖 `proc-macro-error2` 既有 future-incompat note。`cargo test --locked --test compat_agent_architecture_guard --test compat_agent_docs_contract --test session_capture_docs_contract`（同 target-dir）分别 **64/64、33/33、7/7，共 104/104**，0 fail/ignored，exit0。当前 59 张卡的 pending、DAG 无环、逐卡 AC/VER 与粒度表计数、链接锚点均核验通过；两 Rust 测试文件精确等于 HEAD 经 nightly rustfmt 的输出，未改断言/逻辑。源码只作排版、计划/索引只作本次授权修订，`Full-suite trigger: none`；未跑本次 Rust 全量、Linux、四 provider 新模型或 release/部署验收，不以旧全量替代。本机 OpenCode 的 13段/4617 bytes/两 checkpoint 与默认新会话131 bytes 均已与原收据重新核对。系统配置仅本机生效且未纳入提交；本轮无版本发布/网站变更。

签推前已整合 main `7819590134c1cf158d8ce6455965c1be6114d42b` 的 Grok 文档更新；9 个自有 delta 在整合前后摘要相同，incoming Grok 计划原字节保留，8 个原有 stash 及无关 untracked 保留。末端提交自身 CI 仍按 v2.14 在下一次计划状态例行同步补记实际 SHA/run/conclusion/UTC；此处不预填推送后结果。

实际Claude Code 2.1.300 / claude-opus-5-5；同一只读session `095e26d7-0019-4a37-9c97-527341b76e98`。仅Read/Glob/Grep，无文件修改/执行/委派/真实home读取。R1/R2/R3原始FAIL保留，绝不由后续PASS倒改。

| 轮次 | 实际意见与处置 | raw SHA256 | report SHA256 |
|---|---|---|---|
| R1 | 四份FAIL；迁移级联P1及规划P2，后续修订关闭 | `ca3d16af131d971f0128de4c350dc4777b4e4c87db70fea1f472d2d8a851aeb0` | `a822c2d39a41b22b7551c23ea9e909711e09226d5c1b1d3ca5b6df1fe7d37ad5` |
| R2 | 四份FAIL；P1已关闭，剩P2，后续修订关闭 | `36ead2d99595b1a67eac5d5952ce8c0af5ca38921b9a545aaa0c21439e8cbf15` | `9ae155fc7142f00c7552e253378cf03a574da1e0cb061be9bde8092fe4a56ce5` |
| R3 | CX/CC/PI PASS，HM/总FAIL；两P2，R4修订 | `a798e21ab58af05a16443f3b0a3c42ba616921cc4637d8435b4942cfa5ac4f25` | `fdd6e50e6ec59957aaec22b0fb31594202522b3e7d5878055298facf2c8de52b` |
| R4 | 四份及总VERDICT: PASS；非阻断建议作窄补充，R5复核 | `9233b369618dff2c975b408e7d5d29b63f83a2667640bbefe20871ccd39e7d77` | `586a8b35a0afb9a575907485116425f8659090659f9e23772c1b27668f989f04` |
| R5（最终） | 四份及总VERDICT: PASS；R4范围复用＋五项窄核通过，root采纳 | `06e9bc29ae44c1dcc9d4db00c8a90ca1799cf9bb652980e181b7e3fad57772f4` | `e2f40302bada484aaf0963f620205f213d635d94b18d7ece91374d081462b805` |

R4实际四份及总PASS，0阻断。随后补CLI互斥镜像（含issues/483）、Hermes专用桥措辞、journal erase失效/清理语义并去重写集；R5已逐项确认无新增阻断。2026-10-18 03:20:47 UTC：root独立复核后判定本修订规划PASS并采纳Claude R5；四入口审计卡可在选卡授权后按依赖执行，59卡实现完成数仍为0。


R5输入manifest SHA256 `1fdd95bebb3fea118db887ed81c9c85f3742ff4fb8e39fd4b839371d99b6f326`（七文件）；本计划被审输入SHA256 `566a3bea22df22b50dad498ba5e7adfd9a4a9d2379e74ed1f3d4c2b05e25e9cd`。终审后只登记实际意见、验证摘要、索引状态与修订时间格式，不改变被审规范。Claude PASS不代表Source A/真实live/provider smoke或未来卡C/D已经通过；来源及外部HOLD继续生效。

### 本轮计划改动的实际验证（2026-10-18 03:11:44 UTC登记）

- `cargo +nightly fmt --all --check`：exit0。
- `cargo clippy --all-targets --all-features -- -D warnings`：exit0。
- 双env文档守卫：architecture64/64、docs33/33，共97/97，exit0。
- 双env完整nextest：`Summary [1360.925s] 10262 tests run: 10262 passed (12 slow, 1 leaky), 23 skipped`，exit0；运行包含编译/枚举共2476.26s。LEAK对应既有`command::branch::tests::branch_error_display_pins_static_message_variants`的输出管道标记；skip按原输出保留，不说0LEAK，不替代未来真实provider/source验收。
- 上述测试是当前未修改Rust源码的现有机制/计划读者验证；不是59张未来卡已实现或来源GO。


最终窄补后双env架构64/64＋文档33/33再次实际PASS（97/97，exit0，3.30s），raw SHA256 `6af0a8b4fa48b778ced0eb363fd65c8d1a8752efbd44c9a64ce4099b687187ea`。既有QA raw SHA256：strict clippy `9e45f0588de5e23828fb929df622c6146e99415aabafebbde927e33d9d0b9420`；完整nextest `f848cc543bddd50af116face20b03fb6840cda1e43a4ef49220b31cc7751634a`。本次只修改计划/索引文档，没有修改Rust/fixture/test源码；完整默认套件的LEAK/skip观察如上保留。

## 修订历史

| UTC 时间 | 本次规范性调整 | 状态 |
|---|---|---|
| 2026-10-29 16:42:17 UTC | 按本机已安装且与 GitHub latest release 相同的 Hermes v0.22.5 / `bf0203a794a07aac3ff764972ab9087baa4e4f4e` 刷新 pin、文件哈希、thinking 字段与主源。主源定为只读 `hermes sessions export --format jsonl`。旧 `ccc6956` 与「未安装」改为历史。冻结当时 10 卡仍 pending | 计划事实已写；未读用户会话，无版本发布。该行不描述后来的 HM-00 收口 |
| 2026-10-29 HM-00 收口 | 把冻结轮留下的 P2 写成后继边界：consent 默认值行号、unavailable 不单看退出码、父段不在 JSONL、HTML 为真值或、`drop_undone_rows` 谓词由 reader 套用。HM-00 标 done/complete，无版本 | 审计收口；真实非空 thinking 仍 HOLD |
| 2026-10-18 16:23:55 UTC | 用户要求以 OpenCode 获取 thinking 数据为底线；修订 ordinary 保留、未证明字段、legacy、source 路径和真实数据验收；native 认证退出本轮成功前置 | 仅规划；原卡号、依赖顺序与 pending 数保留，未取得四 provider 新模型实测 |

| 时间 | 触发 | 拆分 | 协调范围 |
|---|---|---|---|
| 2026-10-18 03:02:15 UTC | R2剩余P2与G-09 | PI-05→PI-05/PI-09；PI-03→PI-03/PI-10；HM-05→HM-08/HM-09/HM-05 | DAG、依赖、I集、粒度/测试/追溯、里程碑与索引同步；只改计划，未增加实现完成数 |
| 2026-10-18 02:42:09 UTC（本轮R2修订） | Claude R1 FAIL后按G-09拆分 | PI-01→PI-01/PI-07；PI-03/04→PI-03/PI-08/PI-04；HM-04/05→HM-04/HM-07/HM-05；HM-02改audit | DAG/依赖/写集/测试矩阵/粒度表/里程碑与两个索引同步；无实现完成数增加 |

| 时间 | 修订 | 范围 |
|---|---|---|
| 2026-10-18 02:00:57 UTC | 按当前 OpenCode 方案 C 与 ACF/COV2 实现复核；按模板 v2.14 增量修订未完成卡，不倒改历史实测 | HM 当前卡、依赖、索引与验收 |
