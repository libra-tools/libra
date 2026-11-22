# `libra ui` 真实数据接入计划（2026-11-20）

**模板版本:** `v2.16`（本计划未具名 `code-only`，按自身默认发布合同执行：每张实现卡 `patch + 1`、完整 C/D、ER-05 review 与 ER-13/ER-14 分层测试。）

**状态:** 设计计划（草案，R1–R7 双评 FAIL、R8 Codex FAIL、R9 双评 PASS 后修订至第十版，R10 双评 PASS 后做最后一致性清理）。七张活动卡 `UID-01`..`UID-07` 全部 `pending` / 验收空；本文件只规划任务，不宣称任何数据已接入窗口。

**卡 ID 前缀说明:** 使用 `UID-`（UI Desktop）以区别于 `issues/479` 已占用的 `UI-01`..`UI-03`。

## 文档职责

本文解决 `libra ui` 桌面窗口只显示内嵌演示数据（26 个设计页、`sessions.json` 演示目录、`screens/history.rs` 的静态提交表）而无法回答"我的仓库现在是什么状态、Agent 到底做了什么"的问题。目标是交付一条**只读、离主线程、可失败、成本可测**的真实数据接入路径，并让六个数据表面（History、Sessions、Operations、Memory、Review runs、Investigate runs）与一个可用性状态面按卡序从演示数据切换到仓库事实。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- 命令表面：`libra ui`（无参数启动、全局 flag 前后皆可、`--help` 与错误退出码不变）。
- 数据表面：仓库上下文、提交历史与归属、Agent session/checkpoint（`capture.db`）、operation v2 journal、memory episode 投影、review/investigate run 目录。
- 代码表面：`src/gui/**`（新增 `src/gui/data/`）、必要的 internal/命令层读 API 提升（单一事实源），以及 `src/command/ui.rs` 的文档化行为说明（不新增 CLI flag）。
- 文档与发布：`docs/commands/ui.md` EN+zh、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、相邻网站 `../libra-backend` 的 `commands/ui.en.md`、版本面与 `gh` 发布。

### 非目标

- **窗口内写操作**：restore / trust / rebase / checkout / 导入 / 克隆等按钮仍只显示预览提示（`DEFER-UID-01`）。
- **实时流式刷新**：不做 watcher 或增量推送（`DEFER-UID-02`）。
- **多仓库标签页与 `libra ui <path>`**：一次只接入当前工作目录仓库（`DEFER-UID-03`）。
- **远端/云数据**：不读 Mega2 traces、Cloud 快照或网络（`DEFER-UID-04`）。
- **其余演示页**：本计划不接真实数据（`DEFER-UID-05`，页面清单见该条目）。
- **diff/hunk 内容视图**：只接提交元数据与拓扑（`DEFER-UID-06`）。
- **run 全文（findings/diff 内容）**：只显示摘要（`DEFER-UID-07`）。
- **Windows/Linux 桌面人工验收**：验收平台为 macOS（`DEFER-UID-08`）。
- **dirty 摘要**：现有 status 采集器会哈希并写证书，本计划不接（`DEFER-UID-09`）。
- **复活 `libra code` / Web UI**：GPUI 原生窗口是唯一交互外壳。

### 成功定义

- 在仓库内启动 `libra ui` 时，六个数据表面与可用性状态面显示仓库真实数据；仓库外启动时窗口正常打开并显示明确空状态，不报错退出。
- 所有仓库 I/O 在单一后台 worker 上执行，GPUI 主线程不阻塞；读取全程不写仓库（无 operation 行、无 schema 升级、无 writer boundary、无 conversion 证书写入）。
- 失败路径按 ADR-UID-06 的分类矩阵呈现（源级与条目级分开），不 panic、不静默降级成演示数据。
- 每卡同步 EN+zh 命令文档、开发文档、`docs/development/commands/README.md`、兼容矩阵与网站页面；网站 `cf` typecheck/build、Claude Code PASS 与部署检查通过。
- 计划完成判据全部满足（见「完成判据」）。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新；本表锚点已按 2026-11-20 当前树刷新。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `libra ui` 由进程入口在主线程识别并调用 `gui::run` | `src/main.rs:539`、`src/main.rs:646`、`src/cli.rs:1401` |
| 代码入口 | `gui::run` 定义（GPUI 主线程） | `src/gui/mod.rs:30` |
| 代码入口 | 调度器路径的 `ui` 永远返回 `LBR-UNSUPPORTED-001` | `src/command/ui.rs:42`、`src/cli.rs:4084` |
| 代码入口 | `ui` 的 preflight 为 `none()`、scope 为 `ReadOnly`、豁免自动升级检查 | `src/cli.rs:1956`、`src/cli.rs:2338`、`src/cli.rs:3990` |
| 数据/状态 | 窗口数据是内嵌演示目录，不读仓库 | `src/gui/model.rs:14`、`src/gui/view.rs:1` |
| 数据/状态 | 演示目录另有 `search` 与 `capabilities` 两个消费者 | `src/gui/screens/search.rs:56`、`src/gui/screens/capabilities.rs:14` |
| 数据/状态 | History 提交表是静态常量 `COMMITS` | `src/gui/screens/history.rs:20` |
| 数据/状态 | History 现有筛选按作者 `eli` 分类、图形按作者 `Codex` 与行号绘制 | `src/gui/screens/history.rs:183`、`src/gui/screens/history.rs:224`、`src/gui/screens/history.rs:483` |
| 数据/状态 | History 内还有演示文案 "acme-libra" / "Design preview" | `src/gui/screens/history.rs:95`、`src/gui/screens/history.rs:107` |
| 数据/状态 | 窗口状态只有 `Desktop` 的页面/选择/输入字段 | `src/gui/desktop.rs:145`、`src/gui/desktop.rs:55` |
| 数据/状态 | `RequestScope` 解析与 scope pin API | `src/internal/worktree_scope.rs:80`、`src/internal/worktree_scope.rs:226` |
| 数据/状态 | Agent session/checkpoint 目录已拆分到 `.libra/private/capture.db` | `src/command/maintenance.rs:3558`、`sql/migrations/2026111601_capture_database.sql:304` |
| 数据/状态 | capture 库只读打开 API（真正 read-only）、库路径与错误消息 | `src/internal/ai/capture/store.rs:93`、`:796`、`:41`、`:98`、`:808` |
| 数据/状态 | capture 库会话/checkpoint/事件/归属表与归属索引 | `sql/migrations/2026111601_capture_database.sql:304`、`:135`、`:57`、`:329`；`idx_agent_session_commit_oid` |
| 数据/状态 | `session_page_sql` 是命令层 `pub(super)` helper | `src/command/agent/session.rs:390` |
| 数据/状态 | `mod session` 在 agent 模块内是私有的 | `src/command/agent/mod.rs:48` |
| 数据/状态 | 会话↔提交链接 helper 接收注入连接，但内部仍会 `open_cached_repository`；方向是 session→commits；过滤含 tombstone/scope/fence | `src/command/agent/session.rs:763`、`:800`、`:813` |
| 数据/状态 | 文件会话存储 `SessionStore` 会在 `load()` 时迁移旧格式（有副作用） | `src/internal/ai/session/store.rs:29`、`:377` |
| 数据/状态 | memory episode 读面：`list_episodes` 无 limit，子表按 repo 全量取，freshness 成本随链接 operation 数增长 | `src/internal/ai/memory/reader.rs:198`、`:319`、`:392` |
| 数据/状态 | memory 全源指纹由投影写入 | `src/internal/ai/memory/projection.rs:183` |
| 数据/状态 | operation v2 存储 API；head 按 scope；`end_ts` 可在 head 不变时改变 | `src/internal/operation/store.rs:330`、`:949`、`:757` |
| 数据/状态 | operation 历史查询是命令层私有 helper：ranked CTE + COUNT + 外层 OFFSET，按可变 `end_ts` 排序 | `src/command/op.rs:650`、`:662`、`:696` |
| 数据/状态 | CLI 分页从第 1 页开始 | `src/command/op.rs:251` |
| 数据/状态 | review/investigate run 共享 `.libra/sessions/agent-runs/<run_id>/`；investigate 显式按 kind 过滤，review 只按 wire 类型反序列化且不校验 `kind` | `src/internal/ai/review/mod.rs:6`、`src/internal/ai/review/store.rs:894`、`src/internal/ai/investigate/store.rs:932` |
| 数据/状态 | run 逐项读取用无上限 `fs::read` | `src/internal/ai/review/store.rs:1226` |
| 数据/状态 | 屏幕静态 run 字面量 | `src/gui/screens/reviews.rs:31`、`src/gui/screens/investigate.rs:29` |
| 数据库连接 | `get_db_conn_instance()` 失败即 panic | `src/internal/db.rs:608` |
| 数据库连接 | 显式路径连接默认允许 schema 升级；连接失败被包装为 `io::Error::other` | `src/internal/db.rs:624`、`:635`、`:277` |
| 数据库连接 | 无 schema 管理连接（可传 busy timeout；读写打开） | `src/internal/db.rs:217` |
| 数据库连接 | 两个库使用 DELETE journal（写者会短暂阻塞读者） | `src/internal/db.rs` 的 journal 设置、`src/internal/ai/capture/store.rs:796` |
| 数据库连接 | 按连接检查 schema 的 API；按角色 API 拒绝 Capture 角色 | `src/internal/db/schema.rs:381`、`:875` |
| 数据库连接 | CLI 对只读命令关闭升级的入口（窗口路径绕过） | `src/cli.rs:3849` |
| 代码入口 | 自动升级检查对 `ui` 的豁免点 | `src/cli.rs:3990` |
| 仓库定位 | `storage_path()`/`working_dir()` 在仓库外 panic；`try_get_storage_path` 返回错误 | `src/utils/util.rs:1289`、`:1296`、`:1491` |
| 工作树状态 | `changes_to_be_staged` 在 stat 证据不足时会做内容哈希并写 conversion 证书 | `src/command/status.rs:1146`、`:1570`、`:1586`、`src/utils/worktree_conversion.rs:450` |
| 用户命令 | `libra ui` 不读取也不写入仓库 | `docs/commands/ui.md:47` |
| 文档 | 开发文档把「接真实数据」列为未实现项 | `docs/development/commands/ui.md:37` |
| 文档 | 开发文档索引仍写 "Needs no repository" | `docs/development/commands/README.md:85` |
| 测试 | 现有 `ui_test` 只覆盖 help/多余参数/Linux 无显示 | `tests/command/ui_test.rs:1` |
| 测试 | session 列表分页契约测试 | `tests/agent_checkpoint_reader_test.rs:449` |
| 外部参照 | capture 库拆分已完成（CQ-04 `done`/`complete`） | `plan-status.md` 的 capture-queue 行 |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-UID-01 | 窗口无仓库上下文与数据接缝，只有静态演示数据 | 用户看不到自己的仓库/Agent 事实 | `src/gui/model.rs:14`、`src/gui/screens/history.rs:20` | `UID-01` |
| GAP-UID-02 | History 显示 9 条固定提交，筛选/图形基于作者名与行号，另有演示文案 | 历史、归属与文案错误 | `src/gui/screens/history.rs:20`、`:224`、`:95` | `UID-02` |
| GAP-UID-03 | Sessions 显示 8 条原型 session | 无法查看真实 Agent 会话与证据 | `src/gui/model.rs:14` | `UID-03` |
| GAP-UID-04 | Operations 页面是静态卡片 | 无法查看操作日志 | `src/gui/screens/operations.rs` | `UID-04` |
| GAP-UID-05 | Memory 页面是静态卡片 | 无法查看 episode 投影 | `src/gui/screens/memory.rs` | `UID-05` |
| GAP-UID-06 | Review/Investigate 运行列表是静态卡片；review 列表不校验 kind | 无法查看真实 run 状态 | `src/gui/screens/reviews.rs:31`、`src/internal/ai/review/store.rs:894` | `UID-06` |
| GAP-UID-07 | Splash/Welcome/About/Capability 是插图或固定文案 | 无法判断仓库与数据面可用性 | `src/gui/screens/splash.rs:67`、`src/gui/screens/about.rs:69`、`src/gui/screens/capabilities.rs:14` | `UID-07` |
| GAP-UID-08 | 主线程直接渲染，无 I/O 线程模型；`storage_path()`/`get_db_conn_instance()` 会 panic | 真实读取会冻结或崩溃窗口 | `src/utils/util.rs:1296`、`src/internal/db.rs:608`、`src/gui/mod.rs:30` | `UID-01` |
| GAP-UID-09 | 文档仍写「不读取仓库」「演示数据」 | 文档与行为不一致 | `docs/commands/ui.md:47`、`docs/development/commands/ui.md:37` | `UID-01`..`UID-07` |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| [`plan-long.md`](plan-long.md) | 不新增 LR；RT-01 只保留历史封存 | 不复活 `libra code`/Web UI；只在该文件的日期计划索引登记本计划 |
| [`plan-20261116-capture-queue.md`](plan-20261116-capture-queue.md) | 前置（incoming）：`capture.db` 拆分与只读打开 | 消费 `.libra/private/capture.db` 布局（`DEP-UID-01`） |
| [`plan-20260926.md`](plan-20260926.md) | 前置（incoming）：memory episode 只读投影 API | 只消费 `memory` 读面（`DEP-UID-02`） |
| `plan-20261117-*.md`（15 份 Git 兼容计划） | 写集协调（incoming）：`COMPATIBILITY.md`、`docs/commands/`、`docs/development/commands/README.md`、`plan-status.md`、网站 `commands/` 页面 | 按 ER-12 串行窗口写入（`DEP-UID-03`） |
| `../libra-backend` | 网站文档交付（incoming） | 每卡同步 `commands/ui.en.md`、`cf` typecheck/build、Claude Code PASS 与部署检查（`DEP-UID-04`） |
| `libra ui` 全局 flag 修复 | 基线前置（incoming） | 开工前确认该行为已合入 main（`DEP-UID-05`，按行为判定） |
| `plan-status.md` | 协调者共享写集 | 每卡推进必须在同一变更更新对应行与 §五/§六 登记 |
| `docs/development/commands/ui.md` | 命令开发事实源 | 更新其「当前状态 / 还未实现的功能」章节 |
| [`docs/development/internal/stats.md`](../internal/stats.md) | 统计面事实源 | 只登记 UI 消费哪些统计字段，不改统计口径 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 值得做：窗口已是默认构建的一部分，但只显示演示数据 | 无 |
| 可行性 | 可拆：接缝卡 + 六个表面卡，每卡单轴、M 规模 | 按表面拆成 7 卡 |
| 任务卡粒度 | 五轮发现 AC 捆绑、计数口径、测试路径与映射 | AC 改为单一用户可见谓词；只读不变量移入 Verification；逐卡 AC→test 映射 |
| 依赖与顺序 | 发现写集相交未入图、DEP 粒度/消费者错误 | 补齐串行链与文件级写集窗口；修正消费者；每卡登记 DEP-03/04 |
| 完整性 | 补齐共享读 API 提升写集（含 `agent/mod.rs`）、`T-1` 门、D 组 job 清单 | 见各卡与 ER-04 |
| 安全性 | 只读面、fail-closed、redaction、scope pin、错误分类与脱敏 | 见 GC-UID-02/04 与 ADR-UID-06 |
| 功能正确性 | 源级/条目级状态分离；History 归属与父链；id 快照分页；run kind 校验 | 见 ADR-UID-06 与各卡 AC |
| 接口兼容 | 不新增 CLI flag、不改退出码；提升 API 保持 CLI 输出 | 见各卡 Verification |
| 数据流与控制流 | worker deadline + 取消 + scope pin + 有界读取 + 进程级 handle 归属 | 见 ADR-UID-01 |
| 性能与容量 | 按既有 API 真实成本写契约（freshness 全源成本单列、run 解析上限；dirty 摘要延后） | 见「性能与容量摘要」 |
| 可靠性与容错 | 损坏 run 逐项隔离；worker 停滞有预算与进程级归属 | 见故障恢复矩阵 |
| 可维护性 | 共享读 API 提升为单一事实源 | 见 GC-UID-03 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-21 16:00:00 UTC | UID-04 执行期评审（Codex/Claude 十八轮） | 共享 `operation_snapshot_rows`（50k 上限/truncated）；生产 `filtered_page` 先过滤后分页；独立搜索/命令过滤与翻页夹紧；快照交付重置；绝对路径（引号/列表/URL/Windows）脱敏；schema 兼容与缺账本分类；只读逻辑指纹；真实 50k/10k fixture 与终止化用例 | `UID-04` AC/VER/写集与映射 | UID-04 AC/VER、写集、AC→test 映射 |
| 2026-11-21 12:00:00 UTC | UID-03 执行期评审（Codex/Claude 二十一轮，末轮双 PASS） | 共享 `SESSION_PAGE_COLUMNS` 提升；`SessionSummaryRead::prepare_for` 支持 agent 感知与共享预算；catalog/单行失败分离（`CatalogBusy`/`DatabaseBusy` 保锁分类，`CatalogUnavailable` 页面级）；逐行摘要状态（malformed/unavailable）与可读性过滤；typed Sessions 源级状态（Stale/Absent/Busy/SchemaOld/SchemaNew）；链接与摘要上限；只读逻辑指纹；搜索/过滤/选择/状态消息回归用例 | `UID-03` AC/VER/写集与映射、EX-UID-03-01 登记 | UID-03 写集、AC→test 映射、ADR-UID-06（状态分类口径） |
| 2026-11-21 08:00:00 UTC | UID-02 执行期评审（Claude/Codex） | History 提交按提交时间倒序并截断 200 行；归属查询按 CaptureScope（repo/worktree/workspace/fence）过滤、scope 由注入仓库连接解析、fence 仅 SELECT；归属失败降级为 Human 而不隐藏提交；补 tombstone/merge/过滤用例；Verification 改用既有 `session_list_walks_keyset_pages`；写集补 `snapshot.rs`/`worker.rs`/`widgets.rs` | `UID-02` AC/VER/写集 | UID-02 AC/VER/写集、ADR-UID-03 |
| 2026-11-21 06:00:00 UTC | UID-01 执行期评审（Codex 四轮） | 补 anyhow 链式池超时回归用例；ADR-UID-01 说明 `with_request_scope` 为 task-local（spawn 任务不继承）；A5 与 ADR 记录退出路径的预算内等待例外；A2 映射补真实 worker 仓库外用例 | `UID-01` A5/映射 | ADR-UID-01、UID-01 AC/VER |
| 2026-11-21 05:20:00 UTC | UID-01 执行期评审（Codex 三轮 / Claude 三轮） | 池超时与连接获取超时统一归 `Busy`（`PoolTimedOut`/`Connection pool timed out`/错误链）；ADR-UID-01 scope API 更正为 `with_request_scope`；UID-01 AC→test 映射改为真实用例名；zh-CN 首页与开发文档措辞同步（真实上下文已接入、单一 runtime 已实现）；轮询在窗口销毁后停止；关闭期主线程等待标注为有意且受预算约束 | `UID-01` 映射与 ADR 措辞 | ADR-UID-01、UID-01 AC/VER、docs/commands/zh-CN/ui.md、docs/development/commands/ui.md |
| 2026-11-21 04:40:00 UTC | UID-01 执行期评审（Codex 二轮） | ADR-UID-06 增 `worker` 源级状态（worker 启动失败不误报 `Corrupt`/fsck 提示）；记录 UID-01 的实现期修订 | `UID-01` 状态分类 | ADR-UID-06、UID-01 AC/VER |
| 2026-11-21 03:10:00 UTC | R11 双评 PASS 后的 P2/P3 清理 | availability 预算改"无争用 ≤80ms"并明确 `busy` 只由该探测产生、`stale` 取 memory 快照；UID-07 A7 收窄为"search 删除 catalog 后显示空状态"；UID-06 映射顺序整理；补 R10/R11 评审行与 §一/§二 措辞 | `UID-07` A7 措辞 | 各卡 AC/VER、ADR-UID-06、粒度审计表、性能摘要、§一/§二 |
| 2026-11-21 02:50:00 UTC | R10 双评 PASS 后的 P2/P3 清理 | About 的 `未检查` 占位符登记进 ADR 并把状态来源移出 AC；UID-07 延后页徽标改"新增"并给出 `desktop.rs` 按 `Page` 枚举绘制；`search` 保留 demo 徽标而 `capabilities` 显示真实数据；计数口径补"显示与回退合并"规则；UID-05 freshness 成本测试进入映射与性能行；可用性探测给出 ≤200ms busy timeout 与 `busy` 显示；traceability 去 dirty 口径；人工验收改"临时仓库"；映射顺序整理 | `UID-07` AC 调整 | 各卡 AC/VER、ADR-UID-06、粒度审计表、性能摘要、追溯表 |
| 2026-11-21 02:10:00 UTC | R9 双评 PASS 后的 P2/P3 清理 | UID-04 A5 拆为 id 快照/截断/重置三谓词、UID-07 拆出导入文案与延后页 demo 徽标、UID-06 A5 去实现注释、UID-02 A1 去 `COMMITS` 条款与措辞统一、UID-01 scope 措辞改线程局部；About 的 `stale`/`busy` 取自对应屏幕快照且 80ms 预算只含存在性/读打开/schema；freshness 成本由具名测试记录；deadline 澄清为锁等待上限；计数同步 | `UID-04`/`UID-07` AC 调整 | 各卡 AC/VER、粒度审计表、性能摘要 |
| 2026-11-21 01:20:00 UTC | Codex R8 FAIL | UID-06 A6、UID-02 A4、UID-07 A6 改为单一可见谓词（kind-first/批量查询/删除动作移入 Verification）；freshness 从单页预算中剔除并区分慢但新鲜与真 stale；VER 计数按新口径重算（UID-02=8、UID-07=6）；清除 dirty 残留（风险表与兼容清单）；fault matrix 的 wrong-kind 改 malformed；plan-status §五 补 `DEFER-UID-09`；worker helper 锚点改 `context.rs:136` | `UID-02`/`UID-06`/`UID-07` AC 与计数调整 | 各卡 AC/VER、ADR-UID-06、粒度审计表、性能摘要、故障矩阵、§五 |
| 2026-11-21 00:30:00 UTC | Codex R7 FAIL + Claude R7 FAIL | 去掉无源可依的 dirty 摘要（`DEFER-UID-09`）；memory freshness 去掉时间预算（只按真实过期 `stale` 零行）；kind 未知改记 `malformed`（取消 `wrong-kind`）；worker 用 `with_request_scope_sync` 线程局部 scope；归属查询用注入仓库连接解析 scope、fence 仅 SELECT；AC 只留用户可见谓词（内部不变量移入 Verification）；统一 Verification 计数口径（排除 ER-04 强制门与既有 surface 回归）并按新口径重算；补 R6 评审行与状态/门禁引用；锚点 `worktree_conversion.rs:450` | `UID-01`/`UID-04`/`UID-05`/`UID-06`/`UID-07` AC 与计数调整 | 各卡 AC/VER、ADR-UID-01/03/06、依赖登记表、粒度审计表、性能摘要、§五 |
| 2026-11-20 23:40:00 UTC | Claude R6 FAIL + Codex R6 FAIL | ADR-UID-06 增 `available` 源级状态；kind 读取顺序固定（先读 kind 再按对应 wire 类型解析，其他已知 kind 静默隐藏，未知 kind 记 `wrong-kind`）；锚点改 `cli.rs:1956`/`3990`、`worktree_conversion.rs:450`；AC→test 映射改为可观测的守卫/命令（`command::op_test`/`memory_test`、catalog 与 SessionStore 零命中、dead-code 由 clippy 门覆盖）；UID-02 增 commit-link 回归 `session_show_commit_links` 与演示文案守卫（`\bCOMMITS\b`）；ADR-UID-01 注明 worker pin 落在进程级 fallback；归属语义补 `Agent-authored`；DEP-UID-03 写集表述与 plan-status 对齐 | `UID-02` 写集与 VER 调整 | 各卡 AC/VER/写集、ADR-UID-01/03/06、依赖登记表、粒度审计表、§五 |
| 2026-11-20 23:00:00 UTC | Codex R6 FAIL（+ Claude R6 findings） | memory 取消 `degraded`：freshness 超预算/投影过期一律 `stale` 且零行显示（fail-closed，与 `libra memory` 一致）；UID-03 A3 拆为 redaction 与 payload cap、A6 拆为 Sessions 不调 catalog 与其他消费者保留，`no SessionStore` 移入 Verification；UID-07 去掉 A2 内的不写不变量、dead-code 移入 Verification；各卡人工验收拆成单项 gate 并重算 VER；修订历史与计数同步 | `UID-03`/`UID-07` AC 与 VER 调整 | 各卡 AC/VER、ADR-UID-06、粒度审计表、性能摘要 |
| 2026-11-20 22:10:00 UTC | Codex R5 FAIL + Claude R5 FAIL | 关闭归属改**进程级 handle registry**（跨 `gui::run` 返回仍持有）；ADR-UID-03 显式授权共享 snapshot-id/fetch-by-id helper；AC 只保留用户可见单一谓词、只读不变量移入 Verification；ADR-UID-06 增 `busy`/`degraded` 源级状态；run 增加 kind 校验与 wrong-kind fixture、解析候选按 metadata mtime 排序；归属语义定义 Human/Has checkpoint 并只用 capture 连接（不经 fence UPDATE 分支）；id 快照 50k 上限加 truncated；UID-03 守卫改 `model::catalog\(\)`；UID-04 CLI 输出门改由 `command::op_test` 证明；DEP-UID-03 纳入 `plan-status.md`；DEP-UID-05 统一行为门；计数与锚点按当前树重算 | `UID-01`/`UID-02`/`UID-03`/`UID-07` AC 与计数调整 | 各卡 AC/VER/写集、ADR-UID-01/03/06、依赖登记表、粒度审计表、性能摘要、故障矩阵、§五/§六 |
| 2026-11-20 21:20:00 UTC | Codex R4 FAIL + Claude R4 FAIL | dirty 改 stat-only；关闭改 controller 记录 + 持有 handle；op 改 id 快照；权限读打开探测；run 解析上限 + truncated；History commit-keyed 批量归属；catalog 迁移序列固定；AC 去 panic 捆绑 + 逐卡映射；每卡登记 DEP-03/04；DEP-UID-05 改行为门；锚点/landing 修正；ADR-UID-02 区分 capture 只读与 repo 读写 | `UID-02`/`UID-07` 写集与计数调整 | 各卡 AC/VER/写集、ADR、依赖登记表、粒度审计表、性能摘要、故障矩阵 |
| 2026-11-20 20:05:00 UTC | Codex R3 FAIL + Claude R3 FAIL | 双连接注入并移除内部 open；补 `agent/mod.rs`；关闭 ADR/AC 对齐；freshness 全源成本；op head pin；run 句柄有界读；按连接 schema + Capture 自身 `schema_versions`；权限前置探测；AC 具名 test fn；守卫正则；DEP 消费者/README/页面数；补回两节 | `UID-02`/`UID-03` 计数与写集调整 | 各卡 AC/VER/写集、ADR、依赖登记表、粒度审计表、性能摘要、故障矩阵、§五/§六 |
| 2026-11-20 19:10:00 UTC | Codex R2 FAIL + Claude R2 FAIL | 注入连接 + busy timeout、scope pin、memory 伴生 API、op 真实成本、run 目录/字节上限、状态分离、D 组枚举 job、守卫模式、`op log`、相对路径、计数、DEP 粒度、补偿演练收口一次 | `UID-04`/`UID-05` 口径调整 | 各卡 AC/VER/写集、ADR、ER-04/ER-06a、依赖登记表、粒度审计表、性能摘要、故障矩阵 |
| 2026-11-20 18:05:00 UTC | Codex R1 FAIL + Claude R1 FAIL | 拆 Operations/Memory；卡 ID 改 `UID-*`；AC 逐谓词；补串行依赖边；补共享读 API 提升与 `T-1`；D 组补 `release.yml`；rollback 改 `compensating`；DEP 补证据与失败路径；修正锚点与计数；补守卫命令；补 §五/§六/plan-long 登记 | `UI-01..UI-06` → `UID-01..UID-07` | 实施顺序、依赖登记表、粒度审计表、测试矩阵、追溯表、里程碑、风险表 |
| 2026-11-20 17:10:00 UTC | 自审（成稿） | 首版：6 卡、5 项 ADR、5 项依赖、串行发布 | N/A | 实施顺序、依赖登记表、粒度审计表 |

## 已决议设计决策

### ADR-UID-01: 窗口数据经单一后台 worker 读取，主线程只渲染快照

- **Status:** Accepted
- **Context:** GPUI 必须占用进程主线程（`src/gui/mod.rs:30`）；读取涉及 SQLite、对象库与文件系统；`storage_path()`/`working_dir()` 在仓库外 panic（`src/utils/util.rs:1296`、`:1491`）；既有读面多为 async API；`std::thread` 的 `join` 无超时，阻塞读无法被中断；`gui::run` 返回后局部变量即释放。
- **Decision:** `src/gui/data/` 提供一个常驻 `std::thread` worker：线程内建立**一个长期存在的 current-thread Tokio runtime** 执行 async 读 API，并用 `with_request_scope`（`src/internal/worktree_scope/context.rs:128`）把读取 future 绑定到请求 scope（Tokio task-local；该 future 内 spawn 出的任务不继承，需显式重绑），不依赖进程级 fallback。连接由调用方注入：仓库库用 `open_connection_without_schema_management(path, busy_timeout ≤2s)`（`src/internal/db.rs:217`），capture 库用真正只读的 `open_readonly`（`src/internal/ai/capture/store.rs:93`、`:796`）；worker 不调用 `get_db_conn_instance()`。每次请求带 deadline（busy timeout + 行/字节上限）；worker 在请求边界检查取消标志。关闭窗口时只有两种可声明结果：
  (a) **joined**：worker 在关闭预算（3s）内结束；
  (b) **stalled**：worker 仍在不可中断的阻塞读中，**由控制侧**（窗口关闭路径）记录 `worker stalled` 状态；线程 handle 存入**进程级 registry**（`src/gui/data/worker.rs` 内的按 handle 分项的 `WORKER_HANDLES`），因此 `gui::run` 返回后仍持有该线程（不 detach、不等待），窗口在预算内返回，进程退出时由 OS 回收。**唯一的主线程等待**发生在退出路径：`on_app_quit` 在原生终止前按关闭预算（≤3s）等待 worker 结束，以便记录 joined/stalled。
- **Alternatives considered:** GPUI executor 上做 I/O；每次操作新建 runtime；在阻塞读上等待 join（无超时）；把 handle 放在 `gui::run` 局部变量（返回即 drop → detach）。
- **Consequences:** 引入 supersession、取消、scope pin 与进程级 stalled 归属语义；禁止主线程直接调用 `storage_path()`/`get_db_conn_instance()`；数据层测试必须无显示器且能注入不可合作的慢读。
- **Revisit when:** GPUI 提供稳定的后台 executor，或需要流式刷新（`DEFER-UID-02`）。

### ADR-UID-02: 只读边界——无 schema 管理、无 writer boundary、无 operation 行

- **Status:** Accepted
- **Context:** 显式路径连接默认允许 schema 升级（`src/internal/db.rs:624`）；CLI 对只读命令用 `set_schema_upgrade_allowed(false)`（`src/cli.rs:3849`），窗口路径绕过 CLI preflight；`changes_to_be_staged` 在 stat 证据不足时会写 conversion 证书（`src/command/status.rs:1586`、`src/utils/worktree_conversion.rs:450`）。
- **Decision:** 窗口读取使用跳过 schema 管理的仓库连接（`open_connection_without_schema_management`，读写打开但从不写）与 capture 库真正只读的 `open_readonly`（`read_only(true)`）；不获取 writer boundary/lease、不写 operation journal、不触发迁移、**不使用会写证书的 `changes_to_be_staged`**。schema 落后/超前按 ADR-UID-06 分类显示。
- **Alternatives considered:** 复用 CLI preflight 全流程；允许自动升级。
- **Consequences:** 每卡必须提供"读后仓库状态不变"判据（schema receipt、operation head、conversion 证书）。
- **Revisit when:** 窗口需要写操作（`DEFER-UID-01`）。

### ADR-UID-03: 单一事实源——共享读 API 提升，GUI 只做投影

- **Status:** Accepted
- **Context:** 需要复用的读逻辑部分是命令层私有 helper：`session_page_sql`（`src/command/agent/session.rs:390`，`pub(super)`）、会话↔提交链接（`:763`，内部仍自开连接，方向是 session→commits）、operation 历史查询（`src/command/op.rs:662`，只有 LIMIT/OFFSET 全行查询）、memory 读面（`src/internal/ai/memory/reader.rs:198`）。GUI 若自行复制会漂移（GC-02）。
- **Decision:** 提升可见性并**显式授权新增共享读 helper**，同时保持 CLI 输出不变：
  - `session_page_sql` 与取行函数提升并在 `src/command/agent/mod.rs` 导出。
  - `read_session_commit_links` 改为双连接注入并移除内部 `open_cached_repository`。
  - **新增 commit-keyed 批量归属查询**（`agent_session_commit` 按 commit id 集合一次查询，使用既有 `idx_agent_session_commit_oid`），供 History 使用，避免 per-commit N+1；语义定义为：**Human** = 该 commit 无任何 session 链接；**Agent-authored** = 该 commit 链接到至少一条 session；**Has checkpoint** = 该 commit 链接到至少一条含 checkpoint 的 session。归属查询用**注入的仓库连接**解析 scope（避免 `CaptureScope::resolve` 内部 `open_paired_repository` 另开 30s busy timeout 连接），用 **capture 连接**执行 commit-keyed SQL 与 tombstone 过滤；workspace fence 只用 SELECT-only 的 `assert_workspace_fence_live`，不经 `UPDATE` 分支。
  - `query_operation_log_page` 提升为 `pub(crate)`；**新增 snapshot-id helper（按 CLI 排序取 op id 列表）与 fetch-by-id helper（按 id 集合取行）**，CLI 仍走原分页路径，输出逐字节不变。
  - memory 新增**伴生**有界读 API（保留 `list_episodes` 原签名）。
  GUI 只把结果转换为行类型。唯一允许 GUI 直接反序列化的是 run 的公开 wire 类型——复用 wire 类型而非复制业务逻辑。
- **Alternatives considered:** GUI 解析命令 stdout；GUI 复制 SQL；按 commit 逐个调用 session-keyed 查询（N+1）；用 LIMIT/OFFSET 查询充当 id 快照（无法提供稳定快照）。
- **Consequences:** 提升卡必须证明 CLI 行为未变（既有命令测试），并按 ER-13 `T-1` 跑全量。
- **Revisit when:** 某表面需要 CLI 独有输出布局。

### ADR-UID-04: 隐私与路径卫生

- **Status:** Accepted
- **Context:** Agent 会话内容可能含 secret 与主机绝对路径；ER-11 要求 sanitized 证据。
- **Decision:** 显示前复用既有 redaction（`utils::redact` / `observed_agents::redaction`）；所有路径在快照边界转换为仓库相对路径（`session_page_sql` 的绝对 `working_dir` 与 capture 打开错误消息里的绝对路径都必须转换/脱敏）；快照不含原始 tool payload 全文，单条截断 ≤4 KiB。
- **Alternatives considered:** 原样展示；GUI 内另写 redaction。
- **Consequences:** 部分内容被掩码；截断上限写入性能预算。
- **Revisit when:** 远端/云数据进入窗口（`DEFER-UID-04`）。

### ADR-UID-05: 仓库可选，窗口永不因缺少仓库而失败

- **Status:** Accepted
- **Context:** `libra ui` 可在任意目录启动；改为"必须有仓库"会破坏现有退出码与用户预期；`storage_path()`/`working_dir()` 在仓库外 panic。
- **Decision:** 仓库定位只用 `RequestScope::try_resolve`（`src/internal/worktree_scope.rs:80`）与 `try_get_storage_path`（`src/utils/util.rs:1289`）；找到仓库则加载数据，找不到则以空上下文启动。
- **Alternatives considered:** 无仓库即退出；新增 `--repo` 参数（推迟到 `DEFER-UID-03`）。
- **Consequences:** 每个表面必须实现空状态；空状态不得伪装成演示数据。
- **Revisit when:** 引入 `libra ui <path>` 或多仓标签页。

### ADR-UID-06: 源级与条目级状态分类

- **Status:** Accepted
- **Context:** 同一缺失数据源曾被同时定义为错误和空状态；源级损坏与条目级损坏恢复语义不同；现有 open API 会把连接失败包装为 `io::Error::other`，`capture::open_readonly` 用 `is_file()` 会把不可访问父目录当作 NotFound；两个库使用 DELETE journal，写者会短暂阻塞读者。
- **Decision:** 分类函数只返回以下状态，GUI 按返回值渲染：

  - **源级**：`available`（可读且 schema 兼容）、`absent`（未创建 → 空状态）、`unreadable`（读打开探测保留 `io::ErrorKind`，含 PermissionDenied → 类型化错误）、`corrupt`（读打开成功但连接/结构失败 → 类型化错误）、`busy`（写者持锁且超过 busy timeout → 可重试提示）、`schema-old`（库版本低于二进制 → "用 CLI 升级"）、`schema-new`（库版本高于二进制 → "二进制过旧"）、`stale`（投影过期 → 提示 `libra memory rebuild`；**零行显示**，fail-closed）、`worker`（窗口数据 worker 启动失败 → 提示重开窗口，不误报数据库损坏）。
  - **条目级**：`ok`、`malformed`（内容不可解析 → 该行不可读）、`oversized`（超过字节上限 → 该行标记超大）、`incomplete`（产物缺件 → 该行提示不完整）、**读取顺序固定：先读 `kind` 字段，再用对应 kind 的 wire 类型解析**；其他已知 kind 的记录被本页静默隐藏；`kind` 值未知的记录按条目级 `malformed` 处理。

  源级判定顺序：先 `std::fs::metadata`（保留 NotFound/PermissionDenied），再做**读打开探测**（`std::fs::File::open`）保留其 io kind，最后才打开数据库；数据库打开失败若读打开已成功则归类 `corrupt`，busy/locked 归类 `busy`。capture 打开错误消息在渲染前脱敏。schema 判定用按连接 API `inspect_schema_for_connection(conn, role)`（`src/internal/db/schema.rs:381`）；Capture 角色不被按角色 API 接受（`:875`），因此 capture 库的 `schema-old`/`schema-new` 由注入的 capture 连接读取其 `schema_versions` 判定。**显示占位符 `未检查`** 仅表示"该屏幕尚未加载、因此尚无源级判定"，不是数据源状态。
- **Alternatives considered:** 各页自定义语义；一律报错；把 busy 当 corrupt。
- **Consequences:** 每卡 AC 只断言"分类函数返回并可渲染"，矩阵细节在本 ADR 而非 AC 中计数。
- **Revisit when:** 引入远端数据源（网络不可达成为第九类源级状态）。

## 全局工程约束

- **GC-01 现状核实前置:** 开工前重新核对计划、文档、代码与测试。
- **GC-02 单一事实源:** 禁止 CLI、Web、Agent adapter 或测试 fixture 各自复制逻辑。
- **GC-03 Git 互操作与 Libra 扩展边界:** 说明与 Git 的一致点与有意差异。
- **GC-04 输出与错误契约:** 稳定 `LBR-*` 码并同步 `docs/error-codes.md`。
- **GC-05 文档同步:** EN+zh、开发文档、`COMPATIBILITY.md` 与测试索引同卡同步。
- **GC-06 测试索引:** 新增/重命名 `--test` target 必须更新 `tests/INDEX.md`。
- **GC-07 安全默认值:** 未满足前置时默认 fail-closed。
- **GC-08 原子性与恢复:** 定义事务边界、幂等键、崩溃窗口与回滚/前滚。
- **GC-09 并发与资源生命周期:** 锁、CAS、lease、队列、子进程、连接池、临时目录必须有释放语义。
- **GC-10 性能预算:** 不得引入无界扫描、无界内存或 N+1；写出规模与断言。
- **GC-11 生产 panic 禁止:** 不得新增裸 `unwrap()`、`expect()`、`panic!()`。
- **GC-12 精确暂存:** 只 `libra add <相关路径>`，不得 `commit -a`。
- **GC-13 数据库迁移作用域与全局配置隔离:** 读路径按角色区分并登记 allowlist：`Repository` = `.libra/libra.db`（跳过 schema 管理、从不写）、`Capture` = `.libra/private/capture.db`（只读打开）、`GlobalConfig`/`SystemConfig` = 配置库（只读配置读取）。每连接声明角色与迁移策略；测试使用隔离的 `LIBRA_CONFIG_GLOBAL_DB`/`LIBRA_CONFIG_SYSTEM_DB`。

**计划专属约束:**

- **GC-UID-01 主线程零 I/O:** 所有读取经 `src/gui/data/` 的 worker（ADR-UID-01）。
- **GC-UID-02 只读不写:** 不写 operation 行、不获取 writer boundary、不推进 schema、不写 conversion 证书（ADR-UID-02）。
- **GC-UID-03 不复制业务逻辑:** 复用既有或本卡提升的共享实现；唯一例外是 run 的公开 wire 类型反序列化（ADR-UID-03）。
- **GC-UID-04 路径与隐私卫生:** 快照不含宿主绝对路径；redaction 复用；payload 截断（ADR-UID-04）。
- **GC-UID-05 无演示兜底:** 不得回退到内嵌演示数据。**迁移序列固定**：`UID-03` 让 Sessions 不再使用 `catalog()`，但保留 `sessions.json`/`catalog()` 给 `search`/`capabilities`；`UID-07` 删除 `sessions.json` 与 `catalog()` 并同步改写这两个消费者。
- **GC-UID-06 无显示器可测:** 数据层测试放在 `src/gui/data/tests.rs`（生产文件不含测试代码），路径为 `gui::data::tests::*`；屏幕模块保留内联测试，其 panic 守卫不适用。窗口人工验收需要 macOS 桌面会话。
- **GC-UID-07 文档同卡交付:** EN+zh、开发文档、`docs/development/commands/README.md`、兼容矩阵与网站页面同卡交付（ER-06a）。
- **GC-UID-08 状态矩阵统一:** 所有表面按 ADR-UID-06 返回状态。

## 执行检查必备需求（强制）

1. **ER-01 开工前安全检查:** `libra status --short --branch`；默认在 `main` 上按卡直推。
2. **ER-02 先核对后实现:** 刷新源码/文档锚点、测试 target 与外部参照。
3. **ER-03 粒度门禁:** 按 `G-*` 复核本卡 `Granularity` 行；范围扩大先拆卡。
4. **ER-04 每卡验收门:** A 组（本卡 `Verification` 的 nextest 命令）+ B 组（`Task type`）+ C 组（发布收口）+ D 组（远端后置）累加。C 组按 ①版本面 parity → ②`patch + 1` 与三处版本面同步 → ③`cargo build` 由工具链刷新 `Cargo.lock` → ④bump 后重跑 fmt + 严格 Clippy + 本卡 A 组（`Full-suite trigger` 命中时改跑双环境全量 nextest）→ ⑤`cargo build --release` → ⑥安装 → ⑦`libra add <相关路径>` + `libra commit -s` → ⑧推送 `main` → ⑨`gh release create v<version>`。D 组逐 job 登记并取证：`base.yml`（push `main`）的 `format`/`clippy`/`redundancy`/`test`；`codeql.yml`（push `main`）的 `analyze`；`release.yml`（push `v*` tag）的 `build-and-upload`（四平台 leg）/`upload-install-scripts`/`update-homebrew-tap`/`verify-homebrew-formula`/`request-stable-manifest`，head SHA 为本卡提交；产物判据为 `download.libra.tools` 对应版本 HTTP 200 + 版本号匹配。任一失败前滚修复。
5. **ER-05 review 闭环:** 代码 review 者为 Codex（维护者可指定 Claude），P0/P1 关闭并以 `VERDICT: PASS` 收口；网站 `cf` 推送前另需后端 `AGENTS.md` 要求的 Claude Code PASS。
6. **ER-06 文档与兼容同步:** 按 ER-06a 逐文件交付；网站写入前确认 `../libra-backend` 是 Libra 仓库且为 `cf`，跑 sync check、typecheck、build 与 Claude Code PASS；部署判据为公开页 URL HTTP 200 且新文案可见。
7. **ER-07 Libra-native 工作流与提交签名:** 只用 `libra status`/`libra add`/`libra commit -s`/`libra push origin main`；提交后校验 `gpgsig` 与 `Signed-off-by` 同一提交。
8. **ER-08 版本与发布:** 每卡 `patch + 1`，三处版本面同步，`Cargo.lock` 只由工具链刷新，`gh release create v<version>`。
9. **ER-09 push 失败策略:** 非 fast-forward 先 pull/merge 后重新验收再推。
10. **ER-10 内部服务错误:** 有界重试（≤5 次、≤30 分钟），超预算置 `blocked`。
11. **ER-11 证据卫生:** 证据只写摘要与计数。
12. **ER-12 并发边界与串行发布:** 全串行；单一发布者。
13. **ER-13 测试执行分层:** 执行阶段按 `Verification`；`Full-suite trigger` 按卡判定；全部卡完成后跑全量收口门并修复暴露的 Bug。
14. **ER-14 发布集成测试执行器:** focused 证据以 nextest 执行；全量收口门用双环境 nextest；`cargo test --all` 只作诊断。

## 实施顺序

- `UID-01 -> UID-02`
- `UID-02 -> UID-03`
- `UID-03 -> UID-04`
- `UID-04 -> UID-05`
- `UID-05 -> UID-06`
- `UID-06 -> UID-07`

依赖边同时承载 G-10 的写集串行理由：七卡共享 `src/gui/desktop.rs`、`src/gui/data/mod.rs` 与同一组文档。

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-UID-01 | incoming | 跨计划 | [`plan-20261116-capture-queue.md`](plan-20261116-capture-queue.md)（CQ-04） | capture-queue 计划 owner | `.libra/private/capture.db` 承载 `agent_session`/`agent_checkpoint`/`agent_session_commit`，且存在只读打开 API；CQ-04 `done`/`complete` | `src/internal/ai/capture/store.rs:93`、`sql/migrations/2026111601_capture_database.sql:135`、`:329`、`plan-status.md` 的 capture-queue 行（核对 2026-11-20） | 布局变化时 `UID-02`/`UID-03`/`UID-07` 置 `blocked` 并登记 `FIX-*`；等待上限 1 个发布切片 |
| DEP-UID-02 | incoming | 跨计划 | [`plan-20260926.md`](plan-20260926.md)（DM-11 读面） | memory 计划 owner | `memory` 读面 API 在 main 可用（DM-11 已合入 main，卡面 `locally-accepted`） | `src/internal/ai/memory/reader.rs:198`、`plan-status.md` 的 0926 行（核对 2026-11-20） | API 变更时 `UID-05`/`UID-07` 置 `blocked`；等待上限 1 个发布切片 |
| DEP-UID-03 | incoming | 跨计划（文件级写集窗口） | `plan-20261117-*.md` 15 份计划中当前写 `COMPATIBILITY.md`、`docs/commands/`、`docs/development/commands/README.md` 或 `plan-status.md` 的活动卡（例如 `plan-20261117-branch.md#BR-01`、`plan-20261117-global-init-infra.md#GII-01`、`plan-20261117-plumbing-misc.md#PM-01`） | 20261117 批次发布者 | 文件级窗口：开工前扫描这些计划的 `in-progress` 卡，确认无卡写 `COMPATIBILITY.md`、`docs/commands/ui.md`、`docs/development/commands/README.md` 或 `plan-status.md`（整文件）；窗口上限 1 个发布切片 | `plan-status.md` 20261117 批次节（核对 2026-11-20） | 窗口不可得则排队；超过 1 个发布切片置 `blocked` |
| DEP-UID-04 | incoming | 外部仓库 | `../libra-backend`（`cf`） | 网站维护者/本计划发布者 | `libra status --short --branch` 显示 `cf`；sync check、typecheck/build、Claude Code PASS 通过 | 后端 `AGENTS.md`「Libra command documentation delivery」（核对 2026-11-20） | 文档未交付 → `blocked`；已提交推送仅部署待确认 → `remote-pending`；等待上限 1 个发布切片 |
| DEP-UID-05 | incoming | 基线前置 | `libra ui` 全局 flag 行为修复 | 本计划发布者 | 行为判据：main 上 `libra ui --json` 与 `libra ui --no-pager` 打开窗口（不再返回 `LBR-UNSUPPORTED-001`），且 `ui_test` 通过；不绑定具体实现 | `tests/command/ui_test.rs`、`docs/commands/ui.md`（核对 2026-11-20） | 未合入则 `UID-01` 置 `blocked`；等待上限 1 个发布切片；不得在本计划内夹带该修复 |

### 发布分组与并发窗口

默认每张卡独立发布（G-07）；本计划不登记 `REL-*`。

**并发声明:** 全串行（七卡写集相交于 `src/gui/desktop.rs`、`src/gui/data/mod.rs`、`docs/commands/ui.md`、`COMPATIBILITY.md`、`plan-status.md`；G-10 I–I 相交禁止并发）。

**发布者:** 单一发布者（维护者指定；未指定时由执行 Agent 串行承担 bump/构建/提交/推送/`gh release create` 与 D 组取证）。

**发布窗口顺序:** `UID-01` → `UID-02` → `UID-03` → `UID-04` → `UID-05` → `UID-06` → `UID-07`；同一时刻只允许一张卡处于「已 bump 未完成推送 / 未触发 release」状态。

### Phase 0: 基线冻结和消歧

**目标:** 冻结 `libra ui` 行为与数据面事实，确认七个表面的数据来源与既有/待提升 API，冻结写集。

**进入条件:** `libra status --short --branch` 干净；`DEP-UID-01`/`DEP-UID-02`/`DEP-UID-05` 满足；`UID-01` 粒度行通过 ER-03。

**退出条件:** 锚点在开工日复核；维护者批准计划文本（R11 定向确认 `PASS`）并授权 `UID-01`。

### Phase 1: 只读数据接缝

**目标:** 交付 `src/gui/data/` worker 接缝、仓库上下文解析（复用 `RequestScope`）、源级/条目级状态分类、快照/错误类型与生命周期管理。

**进入条件:** Phase 0 退出条件满足。

**退出条件:** `UID-01` `done`/`complete`；窗口在仓库内/外都能启动并显示上下文或空状态，主线程零 I/O 与只读性有测试证据。

### Phase 2: 六个数据表面

**目标:** 按卡序把 History、Sessions、Operations、Memory、Review runs、可用性状态六个表面切到真实数据。

**进入条件:** `UID-01` `done`/`complete`。

**退出条件:** `UID-02`..`UID-07` 全部 `done`/`complete`；`sessions.json`/`catalog()`、`COMMITS` 常量与静态 run 字面量已删除（`catalog()` 在 `UID-03` 保留给 `DEFER-UID-05` 页面，`UID-07` 删除并改写消费者）。

### Phase 3: 收口

**目标:** 全量收口门、文档一致性、计划状态同步。

**进入条件:** Phase 2 退出条件满足。

**退出条件:** 「完成判据」全部勾选。

## 任务卡

### 任务卡粒度规则（强制）

沿用模板 `G-01`..`G-11`。本计划登记一项 `EX-UID-03-01`（G-04 生产文件上限，具名审批：Codex R12 PASS）。

#### 字段全局默认与例外

- **Release boundary 默认:** 每张卡独立发布切片
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `compensating`
- **Migration and rollback 默认:** `N/A：无 schema 迁移`
- **Security and privacy 默认:** 继承 GC-07、GC-11 与 GC-UID-04
- **Performance budget 默认:** 继承 GC-10 与 GC-UID-01

**默认覆盖:**

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| 全部卡 | `Docs and compatibility impact` | EN+zh、开发文档、开发文档索引、`COMPATIBILITY.md` 与网站页面同卡同步（ER-06a） |
| `UID-03` | `Security and privacy` | Agent 内容经既有 redaction，快照不含原始 payload |
| `UID-02`/`UID-03`/`UID-04`/`UID-05` | `Full-suite trigger` | 提升/新增共享读 API（GC-02 单一事实源）命中 `T-1` |

**规则 waiver（`EX-*`）:**

| 编号 | 任务 | 豁免规则 | 理由与恢复 | 具名审批 | Review round | 证据 | 有效期 |
|---|---|---|---|---|---|---|---|
| `EX-UID-03-01` | `UID-03` | G-04 `M` 上限（落点 5>4、生产文件 14>12） | Sessions 的共享错误分类必须跨 `capture/catalog.rs`、`capture/summary.rs`、`checkpoint_reader.rs` 与两个 CLI 错误映射同型接线；拆卡会把同一恢复轴（会话投影）撕开。恢复：revert 提交 + 下一 patch 发布 + 网站重同步 | Codex（R12 PASS；R16 复核 EX-UID-03-01） | R12/R13/R16 | `plan-20261120-ui-real-data.md` 修订历史 UID-03 行（含 waiver 登记） | 本计划内 |

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `UID-01` | implementation | 只读数据接缝、状态分类与 worker 生命周期 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 5/8 | 7/8 | 2/7 | M | DEP-UID-03, DEP-UID-04, DEP-UID-05 | no-overlap（链首） | independent | N/A | N/A |
| `UID-02` | implementation | History 提交历史、归属与拓扑真实化 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 7/8 | 8/8 | 4/9 | M | UID-01, DEP-UID-01, DEP-UID-03, DEP-UID-04 | 序列化于 UID-01 | independent | N/A | N/A |
| `UID-03` | implementation | Sessions Agent 会话真实投影与隐私边界 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 7/8 | 7/8 | 5/14 | L-exception:EX-UID-03-01 | UID-02, DEP-UID-01, DEP-UID-03, DEP-UID-04 | 序列化于 UID-02 | independent | N/A | EX-UID-03-01 |
| `UID-04` | implementation | Operations 操作日志真实投影 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 4/8 | 4/5 | M | UID-03, DEP-UID-03, DEP-UID-04 | 序列化于 UID-03 | independent | N/A | N/A |
| `UID-05` | implementation | Memory episode 有界投影与 stale 语义 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 5/8 | 4/8 | 4/5 | M | UID-04, DEP-UID-02, DEP-UID-03, DEP-UID-04 | 序列化于 UID-04 | independent | N/A | N/A |
| `UID-06` | implementation | Review/Investigate run 记录投影与损坏隔离 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 5/8 | 3/5 | M | UID-05, DEP-UID-03, DEP-UID-04 | 序列化于 UID-05 | independent | N/A | N/A |
| `UID-07` | implementation | 仓库与数据面可用性状态呈现 | 补偿：revert 提交 + 下一 patch 发布 + 网站重同步 | yes | yes | 8/8 | 7/8 | 3/10 | M | UID-06, DEP-UID-01, DEP-UID-02, DEP-UID-03, DEP-UID-04 | 序列化于 UID-06 | independent | N/A | N/A |

#### Verification 判定口径

- AC 只登记**用户可见的单一行为谓词**；只读不变量（schema receipt / operation head / 证书不变）与性能预算作为本卡 Verification 中的具名 test fn，不计入 AC 上限。
- 每条 AC 由**一个具名 test fn 或零命中守卫**覆盖；每卡正文给出 AC→test 映射（不计入条目数）。
- **计数口径统一**：`Verification` 不计 ER-04 强制门（fmt/clippy/全量 nextest）与既有 surface 回归（`command::ui_test`、`agent_checkpoint_reader_test`、`command::op_test`、`command::memory_test`）；只计本卡新增用例、零命中守卫与人工项。
- **显示与回退合并规则**：同一表面的"显示真实数据"与其"空/absent 回退"合为一个 AC（全卡一致，避免同一可见行为的两个分支被重复计数）。
- 数据层测试路径为 `gui::data::tests::<fn>`；`Verification` 用 `-E 'test(uid0N_)'` 选中本卡全部数据层用例。
- 零命中守卫统一模板：

  ```bash
  if rg -n '<pattern>' <paths>; then
    echo "FAIL: forbidden pattern found"; exit 1
  else
    rc=$?
    if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi
    echo "OK: zero hits"
  fi
  ```

- 数据层生产文件不含测试代码：测试放在 `src/gui/data/tests.rs`，守卫以 `--glob '!tests.rs'` 排除；屏幕模块保留内联测试，panic 守卫只覆盖数据层。
- 新增 test fn 标 `(new)`；不新增 `--test` target。
- 全量 nextest 不替代本卡指定用例。

### Task UID-01: 只读数据接缝、状态分类与 worker 生命周期

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 建立 `src/gui/data/` 只读接缝：用 `RequestScope::try_resolve`/`try_get_storage_path` 解析当前工作目录仓库，启动单一后台 worker（长期 runtime + 请求 scope（task-local）+ 注入连接 + 进程级 handle registry），向 GPUI 主线程投递带序号的不可变快照与 ADR-UID-06 的源级状态。行为轴是**数据获取的线程、生命周期、只读性与失败分类**。

**Out of scope:** 各表面行数据 → `UID-02`..`UID-07`；dirty 摘要 → `DEFER-UID-09`；条目级状态 → `UID-03`..`UID-06`；写操作 → `DEFER-UID-01`；流式刷新 → `DEFER-UID-02`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| GPUI 在主线程运行 | `src/gui/mod.rs:30`、`src/main.rs:539` |
| `Desktop` 只有页面/选择/输入状态 | `src/gui/desktop.rs:145` |
| `storage_path()`/`working_dir()` 在仓库外 panic | `src/utils/util.rs:1296`、`:1491` |
| `RequestScope` 解析与请求 scope API | `src/internal/worktree_scope.rs:80`、`src/internal/worktree_scope/context.rs:128` |
| 连接默认允许 schema 升级；连接错误被包装 | `src/internal/db.rs:624`、`:277` |
| 无 schema 管理连接（可传 busy timeout） | `src/internal/db.rs:217` |
| 按连接检查 schema；按角色 API 拒绝 Capture | `src/internal/db/schema.rs:381`、`:875` |

**Acceptance criteria:**

- [ ] A1 仓库内启动窗口时显示仓库上下文（仓库相对工作区标签、`.libra` 存储标签、分支、HEAD）。
- [ ] A2 仓库外启动窗口时正常打开并显示 `absent` 源级状态，不报错退出。
- [ ] A3 源级状态按 ADR-UID-06 渲染（`absent`/`unreadable`/`corrupt`/`busy`/`schema-old`/`schema-new`）。
- [ ] A4 过期响应不会覆盖较新快照（窗口不显示回退数据）。
- [ ] A5 关闭窗口在预算内返回；唯一例外是退出路径（`on_app_quit`）按预算（≤3s）等待 worker 结束。

**AC→test 映射（非计数）:** A1→`uid01_context_snapshot_in_repository`；A2→`uid01_outside_repository_absent`+`uid01_real_worker_outside_repository_is_absent`；A3→`uid01_source_state_classification`+`uid01_probe_and_schema_classification`+`uid01_corrupt_and_schema_old`；A4→`uid01_supersession_and_queue_cap`+`uid01_obsolete_in_flight_response_is_dropped`；A5→`uid01_shutdown_and_close_budget`。实现不变量→`uid01_main_thread_read_count_zero`、`uid01_worker_thread_identity`、`uid01_read_only_receipt_and_head`、`uid01_busy_state_is_rendered`、`uid01_startup_failure_is_displayed`（Verification）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib -E 'test(uid01_)'` (new；含只读不变量用例)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：临时仓库内启动窗口并核对上下文标签
- [ ] 手工（macOS 桌面会话）：仓库外启动窗口并核对 `absent` 空状态
- [ ] 手工（macOS 桌面会话）：关闭窗口并记录返回时间（≤3s）
- [ ] 手工（macOS 桌面会话）：注入不可合作慢读后关闭，核对 `worker stalled` 记录与进程级 handle 归属
- [ ] 手工（macOS 桌面会话）：仓库内 `libra ui --json` 与 `libra ui --no-pager` 均开窗（`DEP-UID-05` 行为门）

**Full-suite trigger:** `none`（不动 `src/cli.rs` 命令注册与全局 flag，不改共享读 API，不新增 `--test` target）

**Dependencies:** `DEP-UID-03`、`DEP-UID-04`、`DEP-UID-05`

**Deliverables:** N/A

**Implementation write set:** `src/gui/data/mod.rs`、`src/gui/data/repo.rs`、`src/gui/data/worker.rs`、`src/gui/data/snapshot.rs`、`src/gui/data/state.rs`、`src/gui/data/tests.rs`、`src/gui/mod.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md:85`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A（复用现有码）。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A（无 schema 迁移）。补偿：revert 提交 → 下一 patch 发布 → 网站重同步。

**Security and privacy:** 继承 GC-07/GC-11/GC-UID-04；worker 只读；上下文标签为仓库相对路径；capture 打开错误脱敏。

**Performance budget:** 上下文快照 ≤50ms；请求队列上限 8；读取 deadline ≤2s（指锁等待/busy timeout 上限，不是整次读取的墙钟截断）；关闭预算 3s。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=只读数据接缝与 worker 生命周期; recovery=compensating; complete=yes; self-contained=yes; AC=5/8; VER=7/8; landing=2; prod-files=7; scope=M; deps=DEP-UID-03,DEP-UID-04,DEP-UID-05; writeset=no-overlap; release=independent; split-from=N/A; exception=N/A`

### Task UID-02: History 提交历史、归属与拓扑真实化

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 用真实提交历史替换 `screens/history.rs` 的静态 `COMMITS` 表，按真实归属数据（`agent_session_commit` 的 commit-keyed 批量查询）与真实父链重建筛选与拓扑，并移除该页残留的演示文案。行为轴是**History 表面的数据、归属与拓扑**。

**Out of scope:** diff/hunk 内容 → `DEFER-UID-06`；分支切换 UI → 沿用现有状态；提交图布局重构 → 保留绘制骨架。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| History 数据是 9 条静态常量 | `src/gui/screens/history.rs:20` |
| 筛选按作者 `eli`、图形按 `Codex` 与行号 | `src/gui/screens/history.rs:183`、`:224`、`:483` |
| 页面残留演示文案 | `src/gui/screens/history.rs:95`、`:107` |
| 归属表与归属索引 | `sql/migrations/2026111601_capture_database.sql:329`、`idx_agent_session_commit_oid` |
| 现有链接 helper 是 session→commits 方向 | `src/command/agent/session.rs:763` |
| `mod session` 私有 | `src/command/agent/mod.rs:48` |

**Acceptance criteria:**

- [ ] A1 History 显示真实提交（≤200，按提交时间倒序）。
- [ ] A2 提交行显示短 OID、作者、时间与主题（主题 ≤200 字符）。
- [ ] A3 提交图按真实父链绘制，不用作者名或行号判断拓扑。
- [ ] A4 Human / Agent-authored / Has checkpoint 三个筛选由真实归属数据决定。
- [ ] A5 搜索与筛选作用于真实数据，无匹配显示空状态。
- [ ] A6 空仓库显示 `absent` 状态（"尚无提交"）。
- [ ] A7 页面演示文案 "acme-libra"/"Design preview" 移除。

**AC→test 映射（非计数）:** A1→`uid02_history_rows`；A2→`uid02_history_row_fields`；A3→`uid02_parent_chain_graph`；A4→`uid02_commit_keyed_attribution`；A5→`uid02_search_and_filter`；A6→`uid02_empty_repository`；A7→零命中守卫 `history.rs` 的演示文案。实现不变量→`uid02_batch_query_no_n_plus_one`、`uid02_read_only_receipt`；capture 会话分页回归→`agent_checkpoint_reader_test::session_list_walks_keyset_pages`（Verification）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib -E 'test(uid02_)'` (new；含只读不变量用例)
- [ ] `source .env.test && cargo nextest run --test agent_checkpoint_reader_test -E 'test(session_list_walks_keyset_pages)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\bCOMMITS\b' src/gui/screens/history.rs`
- [ ] 零命中守卫（模板三分支）：`rg -n 'acme-libra|Design preview' src/gui/screens/history.rs`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：临时仓库造 3 条提交（1 条关联捕获会话），核对列表与行字段
- [ ] 手工（macOS 桌面会话）：核对三个筛选（Human/Agent-authored/Has checkpoint）
- [ ] 手工（macOS 桌面会话）：核对搜索与空状态

**Full-suite trigger:** `T-1: 新增 commit-keyed 批量归属查询并提升 session→commits helper 为共享 API（GC-02 共享 helper）`

**Dependencies:** `UID-01`、`DEP-UID-01`、`DEP-UID-03`、`DEP-UID-04`

**Deliverables:** N/A

**Implementation write set:** `src/command/agent/session.rs`、`src/command/agent/mod.rs`、`src/gui/data/history.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/data/snapshot.rs`、`src/gui/data/worker.rs`、`src/gui/screens/history.rs`、`src/gui/widgets.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（History 真实提交与归属）、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** 提交主题/作者按既有字段输出；不含绝对路径。

**Performance budget:** 提交列表 ≤150ms（≤10k 提交仓库）；归属批量查询一次（≤200 commit）；显示上限 200 行。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=History 数据/归属/拓扑真实化; recovery=compensating; complete=yes; self-contained=yes; AC=7/8; VER=8/8; landing=4; prod-files=9; scope=M; deps=UID-01,DEP-UID-01,DEP-UID-03,DEP-UID-04; writeset=序列化于 UID-01; release=independent; split-from=N/A; exception=N/A`

### Task UID-03: Sessions Agent 会话真实投影与隐私边界

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 用 `.libra/private/capture.db` 中的真实 `agent_session`/`agent_checkpoint`/`agent_bridge_event` 行替换 Sessions 的内嵌演示目录数据，并提升会话分页 SQL 为可复用读 API（连接注入、父模块导出）。行为轴是**Sessions 表面的数据来源、隐私边界与过渡消费者兼容**。

**Out of scope:** 实时捕获 → `DEFER-UID-02`；云端会话 → `DEFER-UID-04`；会话写入/编辑 → 本计划只读；`search`/`capabilities` 的真实化 → `UID-07`（本卡保留其演示接口）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 演示目录经 `include_str!` 内嵌，另有 search/capabilities 消费者 | `src/gui/model.rs:14`、`src/gui/screens/search.rs:56`、`src/gui/screens/capabilities.rs:14` |
| capture 库路径、只读打开与错误消息 | `src/internal/ai/capture/store.rs:41`、`:93`、`:98`、`:808` |
| capture 库会话/checkpoint/事件/归属表 | `sql/migrations/2026111601_capture_database.sql:304`、`:135`、`:57`、`:329` |
| `session_page_sql` 是 `pub(super)` helper | `src/command/agent/session.rs:390` |
| `mod session` 私有 | `src/command/agent/mod.rs:48` |
| 文件会话存储 `load()` 会迁移旧格式 | `src/internal/ai/session/store.rs:29`、`:377` |

**Acceptance criteria:**

- [ ] A1 Sessions 显示 `capture.db` 真实会话（≤200，按 `started_at` 倒序），不再使用演示目录数据。
- [ ] A2 会话详情显示真实 agent、模型、起止时间、usage、checkpoint 数与 `agent_session_commit` 链接行；缺失字段显示"—"。
- [ ] A3 会话内容经既有 redaction 后显示。
- [ ] A4 单条 payload 截断 ≤4 KiB。
- [ ] A5 搜索与"可读内容"过滤作用于真实数据，无结果显示空状态。
- [ ] A6 `capture.db` 未创建显示 `absent` 状态；不可读/损坏按 ADR-UID-06 源级状态显示（错误消息脱敏）。
- [ ] A7 Sessions 列表与详情不再调用 `catalog()`（显示的都是真实捕获数据）；能力地图仍由 `catalog()` 提供，属显式例外。

**AC→test 映射（非计数）:** A1→`uid03_sessions_rows_and_usage`；A2→`uid03_session_detail_fields_are_real`；A3→`uid03_summary_is_redacted`；A4→`uid03_summary_cap_is_512_chars`（比 4 KiB 更严）；A5→`uid03_search_hits_real_rows` + `uid03_readable_filter_predicate_filters_rows`；A6→`uid03_capture_absent_has_no_rows` + `uid03_corrupt_catalog_is_an_error` + `uid03_schema_mismatch_is_typed` + `uid03_busy_causes_map_to_busy`；A7→零命中守卫 `model::catalog\(\)`（`view.rs`；`desktop.rs` 的能力地图调用为显式例外）。实现不变量→`SessionStore` 零命中守卫、`uid03_other_consumers_preserved`、`uid03_read_only_receipt`（Verification）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib -E 'test(uid03_)'` (new；含只读不变量用例)
- [ ] `source .env.test && cargo nextest run --test agent_checkpoint_reader_test -E 'test(session_list_walks_keyset_pages)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n 'model::catalog\(\)' src/gui/view.rs`
- [ ] 零命中守卫（模板三分支）：`rg -n 'SessionStore' src/gui --glob '*.rs'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：临时仓库内造 2 条捕获会话，核对列表与详情字段
- [ ] 手工（macOS 桌面会话）：核对 redaction 后的会话内容
- [ ] 手工（macOS 桌面会话）：核对搜索、空状态与 `capture.db` 缺失时的 `absent` 状态

**Full-suite trigger:** `T-1: 把 `session_page_sql` 与取行提升为命令层与 GUI 共用的单一事实源（GC-02 共享 helper）`

**Dependencies:** `UID-02`、`DEP-UID-01`、`DEP-UID-03`、`DEP-UID-04`

**Deliverables:** N/A

**Implementation write set:** `src/command/agent/session.rs`、`src/command/agent/mod.rs`、`src/command/agent/doctor.rs`、`src/command/agent/checkpoint.rs`、`src/gui/data/sessions.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/data/worker.rs`、`src/gui/data/snapshot.rs`、`src/gui/view.rs`、`src/gui/model.rs`、`src/gui/desktop.rs`、`src/internal/ai/capture/summary.rs`、`src/internal/ai/capture/catalog.rs`、`src/internal/ai/checkpoint_reader.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（Sessions 真实捕获、redaction 边界、过渡接口）、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A（只读）。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** Agent 内容经既有 redaction；快照不含原始 payload；绝对 `working_dir` 与打开错误路径转换为仓库相对路径；不读取未注册 agent 的私有目录。

**Performance budget:** 会话列表 ≤200ms（≤200 会话、无 checkpoint 摘要富集；走既有分页索引）；摘要富集共用 ≤3s 预算，超预算的行降级为 `unavailable`；详情 ≤100ms（≤100 事件）；显示上限 200 行；payload 单条 ≤4 KiB。

**Estimated scope:** `L-exception:EX-UID-03-01`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=Sessions 真实会话投影与隐私边界; recovery=compensating; complete=yes; self-contained=yes; AC=7/8; VER=7/8; landing=5; prod-files=14; scope=L-exception:EX-UID-03-01; deps=UID-02,DEP-UID-01,DEP-UID-03,DEP-UID-04; writeset=序列化于 UID-02; release=independent; split-from=N/A; exception=EX-UID-03-01`

### Task UID-04: Operations 操作日志真实投影

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 用 operation v2 journal 替换 Operations 页面静态卡片，并新增命令层共享 `operation_snapshot_rows` 快照读取（CLI 输出不变）；分页以 **一次性快照**为基准（不依赖可变的 `end_ts` 排序）。行为轴是**Operations 表面的分页数据投影**。

**Out of scope:** 撤销/恢复执行 → `DEFER-UID-01`；统计聚合口径 → `docs/development/internal/stats.md`；memory 投影 → `UID-05`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| operation v2 存储 API；head 按 scope；`end_ts` 可在 head 不变时改变 | `src/internal/operation/store.rs:330`、`:949`、`:757` |
| 历史查询是命令层私有 helper（ranked CTE + COUNT + OFFSET，按可变 `end_ts` 排序） | `src/command/op.rs:650`、`:662`、`:696` |
| CLI 分页从第 1 页开始 | `src/command/op.rs:251` |
| 子命令是 `op log` | `src/command/op.rs:42` |

**Acceptance criteria:**

- [ ] A1 Operations 显示真实 operation 行（≤200，按时间倒序），不再显示静态卡片。
- [ ] A2 每行显示 kind、时间、状态与摘要（摘要 ≤200 字符）。
- [ ] A3 命令过滤与搜索作用于真实数据，无匹配显示空状态。
- [ ] A4 空仓库显示 `absent` 状态；不可读/损坏按 ADR-UID-06 源级状态显示。
- [ ] A5 翻页基于同一 id 快照切片（页间不重复、不丢行）。
- [ ] A6 快照超过 50k id 上限时列表标记 `truncated`。
- [ ] A7 快照变化时列表重置到第 1 页。
- [ ] A8 CLI `libra op log` 输出保持不变。

**AC→test 映射（非计数）:** A1→`uid04_operation_rows`；A2→`uid04_operation_rows`（字段断言）+ 详情面板；A3→`uid04_filter_and_search`；A4→`uid04_source_states` + `uid04_schema_mismatch_is_typed` + `uid04_missing_schema_ledger_is_schema_old`；A5→`uid04_id_snapshot_paging`；A6→`uid04_snapshot_cap_truncated`；A7→`uid04_snapshot_reset_to_page_one`；A8→`command::op_test`（CLI 输出回归）。实现不变量→`uid04_terminalization_no_duplicate`、`uid04_read_only_receipt`（Verification）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib -E 'test(uid04_)'` (new；含只读不变量用例)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::op_test)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：临时仓库内执行一次写操作后打开窗口，核对行与过滤
- [ ] 手工（macOS 桌面会话）：核对翻页与空状态

**Full-suite trigger:** `T-1: 在命令层新增共享 operation snapshot 读取（`operation_snapshot_rows`，GC-02 共享 helper）`

**Dependencies:** `UID-03`、`DEP-UID-03`、`DEP-UID-04`

**Deliverables:** N/A

**Implementation write set:** `src/command/op.rs`、`src/gui/data/operations.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/screens/operations.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（Operations 数据来源、id 快照与 truncated 语义）、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** 摘要不含绝对路径、不含原始 payload。

**Performance budget:** 单页 ≤150ms；显示上限 200 行/页；快照读取成本 = 既有 ranked CTE 排序 + 一次行快照（≤50k 行），10k operation fixture 实测记录于 `uid04_read_budget_10k`；不引入新索引。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=Operations 分页投影; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=4; prod-files=5; scope=M; deps=UID-03,DEP-UID-03,DEP-UID-04; writeset=序列化于 UID-03; release=independent; split-from=N/A; exception=N/A`

### Task UID-05: Memory episode 有界投影与 stale 语义

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 用 memory episode 只读投影替换 Memory 页面静态卡片，并新增**伴生**有界读 API（保留 `list_episodes` 原签名），把 episode 行与子表证据/路径限制在本页选中的 episode 集合；freshness 保留既有全源指纹语义（成本如实登记）。行为轴是**Memory 表面的有界投影与 stale 语义**。

**Out of scope:** `libra memory rebuild` 执行 → 本计划只读；投影算法与 horizon 口径 → `plan-20260926`；统计聚合 → `docs/development/internal/stats.md`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `list_episodes` 无 limit，子表按 repo 全量取 | `src/internal/ai/memory/reader.rs:198`、`:319` |
| freshness 用全源 operation 指纹，成本随链接 operation 数增长 | `src/internal/ai/memory/reader.rs:392`、`src/internal/ai/memory/projection.rs:183` |
| memory 过期 fail-closed 语义 | `src/command/memory.rs` 顶部注释（ADR-DM-10） |

**Acceptance criteria:**

- [ ] A1 Memory 显示真实 episode 行（≤200，按投影顺序），不再显示静态卡片。
- [ ] A2 每行显示 episode 短 id、时间、来源类型与摘要（摘要 ≤200 字符）。
- [ ] A3 投影过期时显示 `stale` 状态（提示 `libra memory rebuild`），不显示陈旧行。
- [ ] A4 搜索与来源过滤作用于真实数据，无结果显示空状态。
- [ ] A5 空投影显示 `absent` 状态；不可读/损坏按 ADR-UID-06 源级状态显示。

**AC→test 映射（非计数）:** A1→`uid05_episode_rows`；A2→`uid05_row_fields`；A3→`uid05_stale_state`；A4→`uid05_search_and_filter`；A5→`uid05_source_states`。实现不变量→`uid05_bounded_children`、`uid05_existing_signature_unchanged`、`uid05_read_only_receipt`、`uid05_freshness_cost_measured`；CLI 行为回归→`command::memory_test`（Verification）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib -E 'test(uid05_)'` (new；含只读不变量用例)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::memory_test)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：临时仓库内 `libra memory rebuild` 后打开窗口，核对行与字段
- [ ] 手工（macOS 桌面会话）：制造过期投影，核对 `stale` 状态显示零行

**Full-suite trigger:** `T-1: 在 `src/internal/ai/memory/` 新增共享有界读 API（GC-02 单一事实源）`

**Dependencies:** `UID-04`、`DEP-UID-02`、`DEP-UID-03`、`DEP-UID-04`

**Deliverables:** N/A

**Implementation write set:** `src/internal/ai/memory/reader.rs`、`src/gui/data/memory.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/screens/memory.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（Memory 数据来源、stale 语义与 freshness 成本）、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** episode 摘要不含绝对路径、不含原始 payload。

**Performance budget:** 单页 ≤150ms（≤200 选中 episode；子表按选中集合过滤）；freshness 保留既有全源校验（无新增时间预算；成本由 `uid05_freshness_cost_measured` 单独实测并把数值写入本计划性能摘要，不并入单页 ≤150ms 预算；慢但新鲜时正常显示行）；投影过期时返回 `stale` 并显示零行（fail-closed，与 `libra memory` 一致）；显示上限 200 行。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=Memory 有界投影与 stale 语义; recovery=compensating; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=4; prod-files=5; scope=M; deps=UID-04,DEP-UID-02,DEP-UID-03,DEP-UID-04; writeset=序列化于 UID-04; release=independent; split-from=N/A; exception=N/A`

### Task UID-06: Review/Investigate run 记录投影与损坏隔离

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 用 `.libra/sessions/agent-runs/<run_id>/` 的真实 run 目录替换 Reviews/Investigate 页面静态卡片：全目录扫描（上限 5000 metadata，按 metadata mtime 倒序取候选）、逐项**打开句柄的有界读**（≤64 KiB）解析公开 wire 类型（解析上限 500 个 state），先读 kind 再用对应 wire 类型解析（未知 kind 记 `malformed`）、按 `state.json` 的 `created_at`/`started_at` 排序并显示每 kind 最新 100，使损坏/超大/缺件行成为条目级状态而不是整页失败。行为轴是**运行记录面的逐项读取与损坏隔离**。

**Out of scope:** 触发新 run → `DEFER-UID-01`；run 全文 → `DEFER-UID-07`；run 清理 → 本计划只读。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| run 目录与 wire 文件；两种 kind 共享目录 | `src/internal/ai/review/mod.rs:6` |
| investigate 显式按 kind 过滤；review 只按 wire 类型反序列化、不校验 `kind` | `src/internal/ai/review/store.rs:894`、`src/internal/ai/investigate/store.rs:932` |
| 逐项读取用无上限 `fs::read` | `src/internal/ai/review/store.rs:1226` |
| 公开 wire 类型与逐项读取 API | `src/internal/ai/review/store.rs:514`、`src/internal/ai/investigate/store.rs:629` |
| 屏幕静态 run 字面量 | `src/gui/screens/reviews.rs:31`、`src/gui/screens/investigate.rs:29` |

**Acceptance criteria:**

- [ ] A1 Reviews/Investigate 显示真实 run（每 kind 最新 100，按 `state.json` 时间倒序），不再显示静态 run 字面量。
- [ ] A2 选中 run 时显示轮次、参与 agent 与结论摘要（摘要 ≤200 字符）。
- [ ] A3 损坏 `state.json` 只影响该行（条目级 `malformed`），其余行仍可用。
- [ ] A4 产物缺件（缺 `manifest.json`）显示条目级 `incomplete`。
- [ ] A5 超过 64 KiB 的文件显示条目级 `oversized`。
- [ ] A6 未知 `kind` 的 run 显示为不可读条目（`malformed`）。
- [ ] A7 扫描/解析上限（5000 metadata / 500 state）命中时列表标记 `truncated`，并说明结果只覆盖已检查子集。
- [ ] A8 run 目录未创建显示 `absent`；不可读按 ADR-UID-06 源级状态显示。

**AC→test 映射（非计数）:** A1→`uid06_run_rows`；A2→`uid06_run_detail`；A3→`uid06_malformed_isolation`；A4→`uid06_incomplete_item`；A5→`uid06_oversized_item`；A6→`uid06_unknown_kind_malformed`；A7→`uid06_truncated_subset`；A8→`uid06_source_states`。实现不变量→`uid06_kind_first_parsing`、`uid06_read_only`（Verification）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib -E 'test(uid06_)'` (new；含只读不变量与未知-kind fixture)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\b(rv|iv)-[0-9a-f]{6}\b' src/gui/screens/reviews.rs src/gui/screens/investigate.rs`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：临时仓库内造正常/损坏/超大/未知-kind 四种 run，核对逐项隔离
- [ ] 手工（macOS 桌面会话）：制造超过扫描上限的目录，核对 `truncated` 子集语义

**Full-suite trigger:** `none`（只消费既有 wire 类型与逐项读取，不改共享读 API）

**Dependencies:** `UID-05`、`DEP-UID-03`、`DEP-UID-04`

**Deliverables:** N/A

**Implementation write set:** `src/gui/data/runs.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/screens/reviews.rs`、`src/gui/screens/investigate.rs`、`src/gui/desktop.rs`、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（run 记录来源、kind 校验、条目级状态、上限与 truncated）、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** run 摘要经既有 redaction；不含绝对路径；不含未脱敏 transcript。

**Performance budget:** 列表 ≤400ms（metadata 扫描 ≤5000 + state 解析 ≤500 × ≤64 KiB 有界读；显示 100 行）；详情 ≤50ms（`state.json`/`manifest.json` 各自 ≤64 KiB）。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=run 记录逐项投影与损坏隔离; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=5/8; landing=3; prod-files=5; scope=M; deps=UID-05,DEP-UID-03,DEP-UID-04; writeset=序列化于 UID-05; release=independent; split-from=N/A; exception=N/A`

### Task UID-07: 仓库与数据面可用性状态呈现

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 让 Splash、Welcome、About 与 Capability map 反映真实仓库与数据面可用性：显示仓库是否已打开、分支/HEAD、各数据源（capture、memory、operation、runs、history）的状态；替换固定文案与插图，删除 `catalog()`/`sessions.json` 并改写 `search`/`capabilities` 消费者，把窗口标签改为 "Read-only preview"。行为轴是**状态与可用性呈现**。

**Out of scope:** 导入/克隆/初始化执行 → `DEFER-UID-01`；多仓库列表 → `DEFER-UID-03`；远端/云可用性 → `DEFER-UID-04`；其余演示页 → `DEFER-UID-05`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Splash 固定文案 | `src/gui/screens/splash.rs:67` |
| Welcome 预览不打开/不创建仓库 | `src/gui/screens/welcome.rs:1` |
| About 固定 "not connected" | `src/gui/screens/about.rs:69` |
| Capability map 与 search 依赖演示目录 | `src/gui/screens/capabilities.rs:14`、`src/gui/screens/search.rs:56` |
| 窗口 "Design preview" 标签 | `src/gui/desktop.rs:332` |
| 页面枚举（徽标绘制依据） | `src/gui/desktop.rs:25` |
| dirty 摘要的现有采集器会哈希并写证书（本卡不接，见 `DEFER-UID-09`） | `src/command/status.rs:1146`、`:1570`、`:1586`、`src/utils/worktree_conversion.rs:450` |

**Acceptance criteria:**

- [ ] A1 Splash 显示真实仓库名（工作目录名）、分支、HEAD 短值与对象格式；仓库外显示 `absent`。
- [ ] A2 About 显示真实仓库状态与各数据源状态（ADR-UID-06 状态；该屏幕尚未加载时显示占位符 `未检查`），不再显示固定 "not connected"。
- [ ] A3 Capability map 按真实可用性标注五个数据面，缺失项显示 ADR-UID-06 中的状态。
- [ ] A4 Welcome 在仓库内显示当前仓库摘要，仓库外保留导入/克隆/打开入口且不执行动作。
- [ ] A5 导入表单的 `acme-libra` 占位与示例文案同步改写。
- [ ] A6 窗口标签由 "Design preview" 改为 "Read-only preview"，并同步文档。
- [ ] A7 `search` 在 catalog 删除后显示空状态（不读演示目录）。
- [ ] A8 16 个延后演示页新增逐页 demo 徽标（由 `desktop.rs` 按 `Page` 枚举绘制），不与真实数据页混淆。

**AC→test 映射（非计数）:** A1→`uid07_context`；A2→`uid07_about_availability`；A3→`uid07_capability_map_states`；A4→`uid07_welcome_in_and_out`；A5→`uid07_import_copy`；A6→`uid07_label_copy`；A7→`uid07_search_after_catalog_removal`；A8→`uid07_deferred_pages_badge`。实现不变量→`model::catalog\(\)` 零命中守卫、`uid07_read_only_no_certificate`（Verification）；dead-code 由 clippy `-D warnings` 门覆盖。

**Verification:**

- [ ] `source .env.test && cargo nextest run --lib -E 'test(uid07_)'` (new；含证书不变用例)
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'`
- [ ] 零命中守卫（模板三分支）：`rg -n 'acme-libra|not connected|Design preview' src/gui/screens/splash.rs src/gui/screens/welcome.rs src/gui/screens/about.rs src/gui/screens/capabilities.rs src/gui/desktop.rs`
- [ ] 零命中守卫（模板三分支）：`rg -n 'model::catalog\(\)' src/gui --glob '*.rs'`
- [ ] 零命中守卫（模板三分支）：`rg -n '\.unwrap\(|\.expect\(|panic!\(' src/gui/data --glob '*.rs' --glob '!tests.rs'`
- [ ] 手工（macOS 桌面会话）：临时仓库内/外分别核对 Splash/About/Capability/Welcome 状态差异
- [ ] 手工（macOS 桌面会话）：核对 "Read-only preview" 标签文案
- [ ] 手工（macOS 桌面会话）：核对 `search`/`capabilities` 在 catalog 删除后的空/真实状态

**Full-suite trigger:** `none`（复用 `RequestScope`/`try_get_storage_path` 与既有只读状态读取，不改共享读 API）

**Dependencies:** `UID-06`、`DEP-UID-01`、`DEP-UID-02`、`DEP-UID-03`、`DEP-UID-04`

**Deliverables:** N/A

**Implementation write set:** `src/gui/data/context.rs`、`src/gui/data/tests.rs`、`src/gui/data/mod.rs`、`src/gui/screens/splash.rs`、`src/gui/screens/welcome.rs`、`src/gui/screens/about.rs`、`src/gui/screens/capabilities.rs`、`src/gui/screens/search.rs`、`src/gui/desktop.rs`、`src/gui/model.rs`、`src/gui/sessions.json`（删除）、`docs/commands/ui.md`、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/ui.en.md`

**Release write set:** Inherited

**Files likely touched:** 上述文件

**Docs and compatibility impact:** 逐文件：`docs/commands/ui.md`（状态页、可用性矩阵与标签文案；dirty 摘要按 `DEFER-UID-09` 不交付）、`docs/commands/zh-CN/ui.md`、`docs/development/commands/ui.md`、`docs/development/commands/README.md`、`COMPATIBILITY.md:179`、网站 `commands/ui.en.md`。`docs/error-codes.md` N/A。

**Rollback mode:** `compensating`

**Migration and rollback:** N/A。补偿：revert 提交 → 新 patch 发布 → 网站重同步。

**Security and privacy:** 只显示仓库相对路径与分支名；不显示宿主绝对路径。

**Performance budget:** 可用性探测 ≤80ms（无争用路径）；自身 busy timeout ≤200ms，超时显示 `busy`（`busy` 只由该探测产生；About 的 `busy` 取该探测结果，`stale` 取 memory 屏幕快照）；不做 dirty 摘要（`DEFER-UID-09`）。

**Estimated scope:** M

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=仓库与数据面可用性呈现; recovery=compensating; complete=yes; self-contained=yes; AC=8/8; VER=7/8; landing=3; prod-files=10; scope=M; deps=UID-06,DEP-UID-01,DEP-UID-02,DEP-UID-03,DEP-UID-04; writeset=序列化于 UID-06; release=independent; split-from=N/A; exception=N/A`

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | 仓库上下文、worker supersession/队列/关闭/stalled、源级状态分类 | `source .env.test && cargo nextest run --lib -E 'test(uid01_)'` |
| 单元 | 各表面行、归属、id 快照分页、redaction、条目级状态 | `source .env.test && cargo nextest run --lib -E 'test(uid0)'` |
| 集成 | `libra ui` CLI 表面不变 | `source .env.test && cargo nextest run --test command_test -E 'test(command::ui_test)'` |
| 集成 | session 列表分页契约（提升后的共享 SQL） | `source .env.test && cargo nextest run --test agent_checkpoint_reader_test -E 'test(session_list_walks_keyset_pages)'` |
| 集成 | 提升后的读 API 不改变既有命令输出 | `source .env.test && cargo nextest run --test command_test -E 'test(command::op_test) or test(command::memory_test)'` |
| 兼容 | 文档与 `COMPATIBILITY.md` 同步守卫 | `python3 scripts/sync-libra-command-docs.py --libra-root ../libra --only ui`（后端仓库） |
| 安全 | 无裸 panic（数据层）、无绝对路径、redaction、不构造 `SessionStore` | `rg` 零命中守卫（模板三分支，排除 `tests.rs`） |
| 性能 | 上限截断与实测成本记录 | 数据层单测中的规模断言（≤200 行/≤400ms/≥10k 规模实测） |
| live/gated | 窗口人工验收（需显示器） | 手工：`./target/debug/libra ui` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| `UID-01` | `src/gui/mod.rs:30`、`src/utils/util.rs:1296`、`src/internal/db.rs:217`、`src/internal/db/schema.rs:381` | `src/gui/data/{mod,repo,worker,snapshot,state}.rs`、`src/gui/mod.rs`、`src/gui/desktop.rs` | `docs/commands/ui.md` EN+zh、开发文档、开发文档索引、`COMPATIBILITY.md`、网站 | `uid01_` 系列 (new) |
| `UID-02` | `src/gui/screens/history.rs:20`、`:224`、`:95`、`sql/migrations/2026111601_capture_database.sql:329` | `src/command/agent/{session,mod}.rs`、`src/gui/data/history.rs`、`src/gui/screens/history.rs` | 同上 | `uid02_` 系列 (new) |
| `UID-03` | `src/gui/model.rs:14`、`src/command/agent/session.rs:390`、`src/internal/ai/capture/store.rs:93` | `src/command/agent/{session,mod,doctor,checkpoint}.rs`、`src/gui/data/{sessions,worker,snapshot}.rs`、`src/gui/{view,model,desktop}.rs`、`src/internal/ai/capture/{summary,catalog}.rs`、`src/internal/ai/checkpoint_reader.rs` | 同上（含 redaction 与过渡边界） | `uid03_` 系列 (new) |
| `UID-04` | `src/command/op.rs:662`、`src/internal/operation/store.rs:757` | `src/command/op.rs`、`src/gui/data/operations.rs`、`src/gui/screens/operations.rs` | 同上（id 快照语义） | `uid04_` 系列 (new) |
| `UID-05` | `src/internal/ai/memory/reader.rs:198`、`:319`、`:392` | `src/internal/ai/memory/reader.rs`、`src/gui/data/memory.rs`、`src/gui/screens/memory.rs` | 同上（stale 与 freshness 成本） | `uid05_` 系列 (new) |
| `UID-06` | `src/internal/ai/review/mod.rs:6`、`src/internal/ai/review/store.rs:894`、`:1226` | `src/gui/data/runs.rs`、`src/gui/screens/reviews.rs`、`src/gui/screens/investigate.rs` | 同上（kind 校验与条目级状态） | `uid06_` 系列 (new) |
| `UID-07` | `src/gui/screens/splash.rs:67`、`src/gui/screens/about.rs:69`、`src/gui/desktop.rs:332` | `src/gui/data/context.rs`、五个屏幕、`src/gui/model.rs`、`src/gui/desktop.rs` | 同上（可用性状态、标签与延后页徽标；dirty 摘要不交付，`DEFER-UID-09`） | `uid07_` 系列 (new) |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 基线冻结、计划 R11 定向确认 PASS、维护者授权 | 计划文件签名提交、`plan-status.md` 行 | N/A |
| M1 | `UID-01` `done`/`complete` | patch 发布、`release.yml` success、D 组绿 | 发布前：revert 提交；发布后：补偿发布 + 网站重同步 |
| M2 | `UID-02`..`UID-07` 全部 `done`/`complete` | 六次独立发布与 D 组证据 | 同上逐卡补偿 |
| M3 | 计划完成判据全部满足 | 全量收口门绿、计划状态同步；**收口时做一次补偿演练** | 前滚修复 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| worker 启动后窗口打开前失败 | 无 | 重开窗口或显示状态 | 主线程 panic |
| 读取中途数据库被外部迁移 | 无（不写） | 显示 `schema-old`/`schema-new`，用户用 CLI 升级 | 自动迁移或静默降级 |
| 写者持锁超时 | 无（不写） | 显示 `busy` 并允许重试 | 把 busy 当 corrupt |
| 窗口关闭时 worker 仍在读 | 已投递旧快照（丢弃） | 预算内 joined，或控制侧记录 `worker stalled` 并由进程级 registry 持有 handle；退出路径按关闭预算等待 | 无限等待、退出路径以外的阻塞或线程无主 |
| `capture.db` 未创建 | 无 | 显示 `absent` | 回退演示数据 |
| `capture.db` 不可读/权限拒绝 | 无 | 显示 `unreadable`（读打开探测保留 io kind，消息脱敏） | 把不可读当空状态 |
| 库结构损坏 | 无 | 显示 `corrupt` | 把损坏当空状态 |
| run 目录损坏/超大/缺件/未知 kind | 该行条目级状态（未知 kind 记 `malformed`） | 逐项隔离，其余可用 | 整页失败 |
| 扫描/解析超过上限 | 列表标记 `truncated` | 显示已检查子集的最新行 | 静默截断 |
| 快照序号乱序 | 无 | 丢弃过期响应并重发 | 用旧数据覆盖新数据 |
| 已发布版本/网站回滚需求 | 已发布 artifact 保留 | revert 提交 + 新 patch 发布 + 网站重同步（补偿键=新版本号） | 删除已发布 artifact 或强推 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| GPUI 主线程被 I/O 阻塞导致窗口冻结 | 高：用户以为程序卡死 | GC-UID-01 + worker 单测（含不可合作慢读）+ 手工验收 | `UID-01` |
| 只读面触发 schema 升级、operation 行或证书写入 | 高：污染仓库状态 | ADR-UID-02 + 无 schema 管理连接 + 不接 dirty 摘要 + 读后状态不变判据 | `UID-01`/`UID-07` |
| GUI 复制命令逻辑导致语义漂移 | 中：显示错误事实 | ADR-UID-03 + 提升/新增共享读 API + CLI 输出回归 | `UID-02`..`UID-05` |
| Agent 内容泄漏 secret 或绝对路径 | 高：隐私暴露 | ADR-UID-04 + redaction 复用 + 路径转换 + 脱敏 | `UID-03` |
| 演示数据兜底掩盖读取失败 | 中：用户看到假数据 | GC-UID-05 + ADR-UID-06 + 固定迁移序列 | 全部 |
| 与 20261117 批次并发改共享文档 | 中：冲突/覆盖 | ER-12 文件级串行窗口 + 精确暂存 | 全部 |
| 大仓库读取超预算 | 中：窗口迟滞 | 行/字节/解析上限 + 实测成本记录（不做虚假 O(200) 承诺） | `UID-02`..`UID-06` |
| `storage_path()`/`get_db_conn_instance()` panic 路径被误用 | 高：窗口崩溃 | 只用 `RequestScope::try_resolve`/`try_get_storage_path`/注入连接 | `UID-01` |
| 提升共享读 API 破坏 CLI 输出 | 高：兼容回归 | 同卡 CLI 输出回归测试 + `T-1` 全量门 | `UID-02`..`UID-05` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 仓库上下文快照 | O(1) 存在性/scope 检查 | 每窗口一次 | ≤50ms | `UID-01` 单测 |
| 提交历史列表 + 归属 | O(200 提交元数据 + 父链) + 一次批量归属查询 | 每次页面重读 | ≤150ms、200 行 | `UID-02` 单测 |
| 会话列表/详情 | O(200 会话)、O(100 事件) | 每次页面重读 | ≤200ms / ≤100ms（摘要富集共用 ≤3s 预算，超预算行降级） | `UID-03` 单测 |
| operation 单页 | id 快照（≤50k id）+ 既有 ranked CTE 成本 | 每次翻页 | ≤150ms；≥10k operation 实测记录 | `UID-04` 单测 |
| memory 单页 | O(200 选中 episode) + 选中集合子表 | 每次页面重读 | ≤150ms（**不含 freshness**）；`uid05_freshness_scale_and_separate_timings` 在 ≥10k episode/≥50k link fixture 实测：页面 ≈1.3ms、freshness ≈294ms（debug 构建，macOS），freshness 不并入页面预算 | `UID-05` 单测 |
| run 列表/详情 | metadata ≤5000 + state 解析 ≤500 × ≤64 KiB | 每次页面重读 | ≤400ms / ≤50ms、100 行 | `UID-06` 单测 |
| 可用性探测 | O(5 数据面) | 每窗口一次 | ≤80ms（无争用）；持锁路径 busy timeout ≤200ms 后显示 `busy` | `UID-07` 单测 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已同步，或说明 `N/A`。
- [ ] `docs/commands/ui.md` 已同步，或说明 `N/A`。
- [ ] `docs/commands/zh-CN/ui.md` 已同步，或说明 `N/A`。
- [ ] `docs/development/commands/ui.md` 已同步，或说明 `N/A`。
- [ ] `docs/development/commands/README.md` 已同步，或说明 `N/A`。
- [ ] `docs/error-codes.md` 已同步，或说明 `N/A`。
- [ ] `tests/INDEX.md` 已同步，或说明 `N/A`（不新增 `--test` target）。
- [ ] `Cargo.toml` `[[test]]` 已同步，或说明 `N/A`。
- [ ] `plan-long.md` 日期计划索引已登记本计划，或说明 `N/A`。
- [ ] dirty 摘要已按 `DEFER-UID-09` 明确不在本计划交付范围（无未勾选的 dirty 门）。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。`FAIL` 必须列出 P0/P1 条目并在下一轮复审关闭。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 | 全文（首版，SHA-256 `286ed2943ff417e25024b8d441fd714d27abe510d7cc2cb6ecb015fd4ad3197d`） | FAIL | Codex：P0=0；P1=7（迁移默认开启、`SessionStore` 副作用、无界读、History 演示语义、D 组缺 `release.yml`、focused 未覆盖全部表面、网站不可写误记 `remote-pending`、关闭无取消边界）。Claude：P0=0；P1=7（AC 捆绑、写集漏共享 API 提升且 `T-1` 误写 `none`、写集相交未入图、未登记 `ui` 修复依赖） | 修订：拆卡、AC 逐谓词、补 `T-1`、补依赖边、补 `release.yml`、补 `compensating`、补状态矩阵、补守卫命令、修正锚点与计数 | Codex exec 与 Claude Code 只读全文审查（2026-11-20 18:0x UTC） |
| R2 | 全文（第二版，SHA-256 `eea3da953ed91a58986ffbb5317b94acc0c5f303431be4639ae599409c69bb6b`） | FAIL | Codex：P0=0；P1=7（归属迁移注入、关闭边界、memory 子查询无界、op CTE 成本、offset 翻页重复、run 目录/文档上限、缺 session CLI focused 门）。Claude：P0=0；P1=6（AC 计数口径、写集漏调用方/`tests.rs`、守卫不可命中或不可通过、目录消费者矛盾、D 组未枚举 job、DEP 粒度/等待上限） | 修订：注入连接 + busy timeout、scope pin、memory 伴生 API、op 真实成本、run 上限、状态分离、D 组枚举 job、守卫模式、`op log`、相对路径、计数、DEP 粒度、补偿演练收口一次 | Codex exec 与 Claude Code 只读全文审查（2026-11-20 19:0x UTC） |
| R3 | 全文（第三版，SHA-256 `4ab80bbebf9d4c785022b3deb1595ee252b5adc642489175a1c00fde7385eb1a`） | FAIL | Codex：P0=0；P1=8（归属自开连接、关闭 ADR/AC 冲突、memory 页内 freshness 冲突、op head 不足、run 字节预检、schema 绕过注入、权限错误被抹、AC→test 映射与测试路径）。Claude：P0=0；P1=5（写集漏 `agent/mod.rs`、UID-01 计数、状态矩阵缺口、读写打开误述、op head/页码/事务、run 扫描与守卫、History 残留文案、DEP 消费者与 README、演示目录过渡矛盾、缺两节） | 修订：双连接注入并移除内部 open；补 `agent/mod.rs`；关闭 ADR/AC 对齐；freshness 全源成本；op head pin；run 句柄有界读；按连接 schema + Capture 自身 `schema_versions`；权限前置探测；AC 具名 test fn；守卫正则；DEP 消费者/README/页面数；补回两节 | Codex exec 与 Claude Code 只读全文审查（2026-11-20 20:0x UTC） |
| R4 | 全文（第四版，SHA-256 `0c5ced37d011937f82a74b81fa32088431a4af2459e2d7f1677b9e30d575541e`） | FAIL | Codex：P0=0；P1=8（dirty reader 写证书、关闭仍要求不可中断线程结束、revision token 漏排序变化、metadata 不保留读打开权限失败、最新 100 无法由扫描预算保证、catalog 删除条件冲突、AC 计数仍捆绑、AC→test 映射缺失）。Claude：P0=0；P1=5（UID-03 写集/landing、UID-02 归属方向、关闭机制归属、UID-06 排序与 kind、状态矩阵/锚点漂移） | 修订：dirty 改 stat-only、关闭改 controller 记录 + 持有 handle、op 改 id 快照、权限读打开探测、run 解析上限 + truncated、History commit-keyed 批量归属、catalog 迁移序列固定、AC 去 panic 捆绑 + 逐卡映射、每卡登记 DEP-03/04、DEP-UID-05 改行为门、锚点/landing 修正、ADR-UID-02 区分 capture 只读与 repo 读写 | Codex exec 与 Claude Code 只读全文审查（2026-11-20 21:0x UTC） |
| R5 | 全文（第五版，SHA-256 `e831282ec8fa54af949cea3504bc15bf62d527b51000c7a2799f7dbbc478303a`） | FAIL | Codex：P0=0；P1=3（关闭归属仍在 `gui::run` 局部、op API 无法提供 id 快照、AC 仍含独立谓词捆绑）+ P2/P3（DEP-UID-05 口径、锚点漂移、run kind 基线、计数）。Claude：P0=0；P1=2（归属查询语义未定义且含会 UPDATE 的 fence 分支、op id 快照与 ADR 冲突）+ P2（busy 被归类 corrupt、DEP-UID-05 旧口径、计数/锚点/守卫/映射） | 修订：关闭归属改进程级 handle registry；ADR-UID-03 显式授权 snapshot-id/fetch-by-id helper；AC 只保留用户可见单一谓词、只读不变量移入 Verification；ADR-UID-06 增 `busy`/`degraded`；run 增 kind 校验与 wrong-kind fixture、候选按 metadata mtime 排序；归属语义定义 Human/Has checkpoint 且只用 capture 连接；id 快照 50k 上限加 truncated；UID-03 守卫改 `model::catalog\(\)`；UID-04 CLI 输出门改 `command::op_test`；DEP-UID-03 纳入 `plan-status.md`；DEP-UID-05 统一行为门；计数与锚点按当前树重算 | Codex exec 与 Claude Code 只读全文审查（2026-11-20 22:0x UTC） |
| R6 | 全文（第六版，SHA-256 `86e7bf8be9561f863a32dae374ac63fd1b1b651ba7869b7c25f5735a10e7130a`） | FAIL | Codex：P0=0；P1=2（memory 超预算 `degraded` 与 fail-closed 冲突、AC 仍捆绑实现不变量）。Claude：P0=0；P1=5（AC 计数、dirty 无 stat-only reader、freshness 无预算/无自动 fixture、wrong-kind 自相矛盾、状态/门禁引用陈旧） | 修订：取消 `degraded` 改 `stale` 零行；AC 拆分并把实现不变量移入 Verification；取消 dirty 摘要（`DEFER-UID-09`）；取消 freshness 时间预算；kind 未知改 `malformed`；补 R6 行与门禁引用；锚点修正 | Codex exec 与 Claude Code 只读全文审查（2026-11-20 23:3x UTC） |
| R7 | 全文（第七版，SHA-256 `23fc01cf3e4103d28573e6fcd7fe21b51f69577cf6d2f5525f8583429d46f029`） | FAIL | Codex：P0=0；P1=2（AC 仍混入实现约束、freshness 超预算缺自动 fixture）+ P2（VER 计数口径、状态/门禁矛盾、锚点）。Claude：P0=0；P1=4（VER 计数超限、freshness 无预算、wrong-kind 矛盾、无 stat-only dirty reader）+ P2/P3（门禁引用、R6 行缺失、锚点、归属连接、scope pin、welcome 文案） | 修订：取消 dirty 摘要与 freshness 时间预算；kind 未知改 `malformed`；AC 只留用户可见谓词；统一 VER 计数口径并重算；worker 线程局部 scope；归属注入连接与 SELECT fence；补 R6 评审行、门禁引用与锚点 | Codex exec 与 Claude Code 只读全文审查（2026-11-21 00:2x UTC） |
| R8 | 全文（第八版，SHA-256 `80f9478a9f39e5d72d73961318c6991fd540f5d276a546f01749e418920a6e13`） | FAIL | Codex：P0=0；P1=1（AC 仍混入实现规则：UID-06 A6、UID-02 A4、UID-07 A6）+ P2（freshness 预算不一致、VER 计数、dirty 残留、wrong-kind 矩阵、DEFER 注册、锚点） | 修订：三处 AC 改单一可见谓词、实现规则移入 Verification；freshness 预算分离；VER 重算；清 dirty 残留；矩阵改 malformed；§五补 UID-09；锚点修正 | Codex exec 只读全文审查（2026-11-21 01:1x UTC） |
| R9 | 全文（第九版，SHA-256 `fabfd59a25dad60da08ae98d81c5b72ed6cc02741dd199c2431eee4a433ddd62`） | PASS | Codex：P0=0、P1=0（3×P2：dirty 残留、门禁引用 R8、VER 计数）；Claude：P0=0、P1=0（6×P2、5×P3：About 预算、AC 实现规则、延后页 demo 徽标、锚点、freshness 记录、措辞、修订历史顺序） | 清理见下一行修订 | Codex exec 与 Claude Code 只读全文审查（2026-11-21 02:0x UTC） |
| R10 | 全文（第十版，SHA-256 `282aed1896f338f59d389d07a7b6974007ef882d9cc129c02dd9f51c20603b6c`） | PASS | Codex：P0=0、P1=0（1×P2：§二 摘要陈旧）；Claude：P0=0、P1=0（2×P2、7×P3：§二 摘要、徽标"新增"、`未检查` 占位、计数规则、traceability、R8 口径、映射顺序、临时仓库、availability timeout） | 清理见修订历史（02:50 行） | Codex exec 与 Claude Code 只读全文审查（2026-11-21 02:4x UTC） |
| R11 | 全文（第十一版，SHA-256 `59aa83d914e7207fc35c7dfe159ba2edddee317992d02abc86f41760b03462c4`） | PASS | Codex：P0=0、P1=0（2×P2/P3：busy 契约、评审日志/状态行）；Claude：P0=0、P1=0（4×P2、1×P3：availability 预算措辞、A7 重复、§一/§二 漂移、映射顺序） | 清理见修订历史（R11 后一行） | Codex exec 与 Claude Code 只读全文审查（2026-11-21 03:0x UTC） |
| R12 | 全文（最终字节，SHA-256 `d932f1b32419a108ed56088a0c8aaf57fc76e06473a99f2ff00b0d2d32607597`） | PASS | Codex：P0=0、P1=0（2×P2/P3：性能摘要口径、§二 摘要）；Claude：P0=0、P1=0（4×P2、1×P3，均在 R11 清理项内） | 清理：性能摘要区分无争用 ≤80ms 与持锁 ≤200ms；§二 摘要同步 R12 | Codex exec（受审 SHA `f7f31b2b897a2bc405679cac320b4b98999cef0cce3234396747eae5157734b6`）与 Claude Code（受审 SHA `0639dea4a50833e0c8e865fe6f1a7de7966744358e965ec29f8c4555276e2819`）只读全文审查（2026-11-21 03:3x UTC） |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-UID-01 | 窗口内写操作（restore/trust/rebase/checkout/导入/克隆/触发 run/memory rebuild） | 本计划只读；写操作需要 writer boundary 与确认语义 | 只读数据面稳定且维护者立项 | 未来 `libra ui` 写操作计划（不登记 outgoing DEP） |
| DEFER-UID-02 | 实时流式刷新（watcher/增量推送） | 快照模式已足够；避免线程与背压复杂度 | 用户要求实时更新 | 未来刷新计划 |
| DEFER-UID-03 | 多仓库标签页与 `libra ui <path>` | 需要路径参数与状态隔离 | 多仓需求确认 | 未来多仓计划 |
| DEFER-UID-04 | 远端/云数据（Mega2 traces、Cloud 快照） | 需要网络、认证与缓存策略 | 本地只读面稳定 | 未来云端数据计划 |
| DEFER-UID-05 | 其余 16 个演示页：refs、agents、automation、bisect、blame、conflict、deps、lanes、rebase、search、settings、views、merge review、cloud、mega2 browser、splash error | 本计划只接六个数据表面 + 一个状态面（`search`/`capabilities` 仅做演示目录移除后的空/真实状态切换） | 演示目录消费者全部迁移后 | 未来逐面接入计划 |
| DEFER-UID-06 | diff/hunk 内容视图 | 只接提交元数据与拓扑 | 需要 hunk 级审阅 | 未来 diff 面计划 |
| DEFER-UID-07 | run 全文（findings/diff 内容） | 只显示摘要 | 需要审阅 run 细节 | 未来 run 详情计划 |
| DEFER-UID-08 | Linux/Windows 桌面人工验收 | 验收平台为 macOS | 有对应桌面会话或图形 CI | 未来跨平台验收 |
| DEFER-UID-09 | stat-only dirty 摘要（staged/unstaged/untracked + `unverified`） | 现有 status 采集器会做内容哈希并写 conversion 证书，需要独立的 stat-only 共享 reader（`T-1`） | 维护者批准新增 stat-only 共享 reader | 未来 dirty 摘要计划 |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [ ] 所有任务卡满足粒度规则 `G-*`：无未登记的 L 例外、无 XL 卡、无碎片卡、无未登记的合并发布例外、实现写集冲突均已消解；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`。
- [ ] 所有任务的 Verification 命令已运行并记录结果。
- [ ] **计划完成门 / 全量收口门（ER-13）**：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿。
- [ ] **全量收口门暴露的 Bug 已全部处理**：本计划改动导致的失败已前滚修复并重跑全量至全绿；既有失败附基线复现证据并登记 `FIX-*`/`DEFER-*`。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成（含网站 `cf` 交付、Claude Code PASS 与部署检查）。
- [ ] 每张卡的 `Rollback mode` 已被实际验证（收口时一次补偿演练；无法验证时记录原因）。
- [ ] Codex（或维护者指定 reviewer）最终结论为 `PASS`，P0/P1 全部关闭。
- [ ] 每张卡已 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新、`gh release create` 触发 `release.yml` 并取得 `base.yml`/`codeql.yml`/`release.yml` 的 D 组证据。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-status.md` 的计划行与 §五/§六 登记、`plan-long.md` 日期计划索引已同步。
