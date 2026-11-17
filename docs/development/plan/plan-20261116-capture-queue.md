# 捕获队列与独立捕获库计划（2026-11-16）

**模板版本:** `v2.16`。本计划不在 v2.16 具名 `code-only` 范围内，按普通发布合同执行：会推送的独立卡与家族发布点各自 `patch + 1` 并走 ER-08。

**状态:** 实施完成，收口全量通过；发布与网站同步待操作者执行（见「完成判据」）。CQ-01、CQ-02、CQ-03、CQ-07、CQ-04、CQ-05、CQ-06 均为 `Lifecycle=done`、`Acceptance=complete`。FIX-CQ-01（CQ-02/CQ-03/CQ-07 拆库回归）随 CQ-05 关闭。FIX-CQ-02（cloud sync/restore 未读 `capture.db`）已修复，作为 CQ-06 之前的独立提交交付。用户要求每张卡在 `main` 上提交并推送，因此这次执行不遵守 REL-CQ-01 的“子卡不推送”。用户另要求捕获相关的新测试不进入 GitHub Actions，以本地运行为准：这些测试由 `LIBRA_RUN_CAPTURE_TESTS=1` 门控，CI 中打印 `skipped`。

## 文档职责

本文解决实时 Agent hook 在打开仓库库 `libra.db` 时互相抢 SQLite 写锁、并以「库由更新的 Libra 写成」这条错误丢失捕获的问题。目标是：捕获目录进入独立的 `capture.db`；hook 只把已脱敏信封原子写入文件系统队列；每个仓库只有一个消费者把队列写入 `capture.db`。

落地时每张卡必须先按 ER-02 刷新源码锚点，再按任务卡验收。

### 使用规则

- 推进任何任务卡时，同一变更必须更新 [`plan-status.md`](plan-status.md) 的「计划一览」行（状态、版本或 commit、`YYYY-MM-DD HH:MM:SS UTC`）。
- 不得在 `docs/development/plan/` 下新建证据目录或证据文件。验收摘要写入本文「实施证据汇总」。
- 执行分支名为 `plan/20261116-capture-queue`。发布动作只有一个发布者，按 ER-12 串行。

### 适用范围

- 共享捕获入口 `process_hook_event_with_target` → `ingest_agent_traces`。覆盖 Claude、Codex、Grok、OpenCode、Hermes、Pi 的实时 hook。
- 捕获目录的数据库角色、schema ledger，以及 `.libra/private/capture.db`。
- `.libra/private/capture-spool/` 队列与单消费者。
- `libra agent doctor` 对队列积压和隔离库的诊断；hook 失败文案。

### 非目标

- 不把 refs、config、sequencer、对象库或 `vault.db` 移出 `libra.db`。
- 不启用 WAL。捕获库保持 DELETE + `synchronous=FULL`。
- 不把队列做成 `capture.db` 里的表。入队本身不能再拿 SQLite 写锁。
- 不改各 Agent 的信封解析、安装器或插件脚本。Gemini 仍是拒绝安装的入口，不入队。
- `libra agent import` 继续在调用进程内直接写 `capture.db`，不经过队列。
- 查询命令不读队列。hook 成功不等于 `libra agent` 立刻可见。
- 不在本计划修改 Grok/Claude 项目 hook 配置，也不减少它们注册的事件种类。

### 成功定义

- 并行 hook 不再因为等不到 `libra.db` 或 `capture.db` 的写锁而退出 128。锁冲突不再显示成「install a newer Libra」。
- 已进入 `ready/` 的信封在进程崩溃后仍会被唯一消费者幂等写入 `capture.db`。
- `libra status` 一类仓库命令与捕获写入不再抢同一个数据库文件。
- 捕获 schema 新于本二进制时，只拒绝 `capture.db`，不把仓库库标成不可打开。
- Claude、Codex、Grok、OpenCode、Hermes、Pi 的实时 hook 都走这条队列。Pi 的硬超时内只要求入队完成。
- 计划完成以「完成判据」为准，而不是以本文成稿为准。

## 事实基线

锚点核对于 2026-11-16 的当前 checkout。开工当天必须重核行号。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | Claude/Codex 的 `libra hooks` 与 `libra agent hooks` 都进入 `process_hook_event_with_target`，AgentTraces 目标调用 `ingest_agent_traces` | `src/internal/ai/hooks/runtime.rs:244-248`；`src/command/agent/hooks.rs:174-229` |
| 数据/状态 | 捕获目录与 refs/config 同在仓库库。`DatabaseRole` 只有 Repository、GlobalConfig、SystemConfig、Derived。最新 schema 只按 Repository 账本计算 | `src/internal/db/schema.rs:23-28`；`src/internal/db/migration.rs:3415-3418` |
| 热路径锁 | hook 开库的 busy 等待被帽在 200ms。非 NotFound 的打开失败一律变成 DatabaseUnavailable | `src/internal/ai/capture/live.rs:211-215`；`src/internal/ai/capture/live_pipeline.rs:316-345`；`src/internal/ai/capture/scope_binding.rs:228-237` |
| 用户可见错误 | 该失败渲染为「could not open the repository database; if it was written by a newer Libra…」，稳定码 `LBR-IO-001` | `src/command/hooks.rs:359-364`；`src/command/hooks.rs:552`；`src/utils/error.rs:382` |
| 已有单飞锁 | 恢复 worker 用仓库级 OS 锁保证一个进程，但它不负责实时入队 | `src/internal/ai/capture/worker.rs:1-30` |
| Provider | OpenCode 用 `agent hooks opencode <verb>` 同步或异步转发。Hermes 与 Pi 只有 observe。Pi 在捕获截止时间后再加 500ms 以退出码 124 结束进程 | `src/internal/ai/hooks/providers/opencode/settings.rs:134-147`；`src/internal/ai/hooks/providers/hermes/mod.rs:1-6`；`src/internal/ai/hooks/providers/pi/mod.rs:1-10`；`src/command/agent/hooks.rs:233-252` |
| 现场 | 多 hook 并行时，`observe` 持有 `libra.db` 写锁超过 2 秒；失败 hook 在 258–645ms 退出。同一会话里多数 hook 仍然成功。库文件存在，空闲时完整性正常，schema 为 `2026103101` | 2026-11-16 对 `/mnt/data/gitmono/mega2` 的只读核对。本文不记录会话正文 |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-01 | 捕获目录与仓库命令共用 `libra.db` 单写者 | 捕获迁移或长事务会堵住 `status`/`commit`，仓库 schema 拒绝也会阻断 hook | `src/internal/db/schema.rs:23-28` | CQ-01..CQ-04 |
| GAP-02 | 每个 hook 进程直接开库，等待只有 200ms | 并行 tool hook 丢失事件，文案误导为需要升级或恢复仓库 | `src/internal/ai/capture/live.rs:215` | CQ-05 |
| GAP-03 | 没有 hook 进程之外的持久队列 | 进程内 channel 无效；把队列表放进 SQLite 仍要先拿写锁 | `src/internal/ai/capture/live_pipeline.rs:339` | CQ-05 |
| GAP-04 | doctor 不能报告未入库的捕获积压 | 入队成功后用户无法区分「还在队列」和「已经丢失」 | `src/command/agent/doctor.rs:1784-1788` | CQ-06 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-long.md` | B 类 AG-ATTR 已实施的捕获架构，无新 LR 编号 | 只在日期索引登记本计划，不改能力编号 |
| `plan-20260924.md` / `plan-20260925.md` | 捕获通用架构与会话目录已收口 | 复用 `ingest_agent_traces` 与目录事务，不重做 redaction 或 checkpoint 语义 |
| `plan-20260910.md` | 数据库角色隔离已收口 | 新角色遵守 GC-13，不重开 MIG 卡 |
| `plan-20260911.md`、`plan-20261018-hermes-capture.md`、`plan-20261018-grok-capture.md`、`plan-20260902.md`、`plan-20260905.md`、`plan-20260904.md` | 各 Agent 的解析与安装 | 不改 parser/插件；只改共享入库入口 |
| `docs/development/plan/plan-template.md` | 本文件结构来源 | 按 v2.16 全文执行 |

## 评审结论与修订记录

成稿自审。Codex 复审结果只写在「Codex review log」，不在这里预填 PASS。

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 现场丢失与 200ms 帽、单文件写锁一致，值得做 | 保持队列在库外 |
| 可行性 | 目录搬迁与 hook 切换拆成两个发布单元 | REL-CQ-01 先于 CQ-05 |
| 任务卡粒度 | 七张卡均 ≤ M；连接切换按目录切开 | 见粒度审计表 |
| 依赖与顺序 | CQ-04 是家族唯一推送点；CQ-05 依赖该树 | 实施顺序已写成具体边 |
| 完整性 | 迁移、崩溃窗口、文档和错误文案都有卡 | CQ-06 单独交付 doctor |
| 安全性 | 脱敏仍在入队前；队列目录 0700 | ADR-CQ-02 |
| 功能正确性 | 终态收条优先；import 不入队；Gemini 不入队 | ADR-CQ-05、ADR-CQ-06 |
| 接口兼容 | hook 成功含义从「已入库」变为「已入队」 | ADR-CQ-04，必须写进命令文档 |
| 数据流与控制流 | 一个 OS 锁、一个写者、提交后才删文件 | ADR-CQ-03 |
| 性能与容量 | 入队有个数和字节上限 | CQ-05 |
| 可靠性与容错 | `tmp/` 可丢，`ready/` 必重放 | 故障恢复矩阵 |
| 可维护性 | 连接只有 `capture_store` 一个入口 | CQ-01 提供，CQ-02/CQ-03 只改调用点 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-16 12:52:52 UTC | 自审成稿 | 初稿。捕获库家族与队列卡分离；删表必须在写入切换之后 | 无 → CQ-01..CQ-06 | 实施顺序、REL-CQ-01、审计表、plan-status、plan-long 索引 |
| 2026-11-16 13:01:34 UTC | Codex R1 FAIL | 补 GC-13 清单与旧读者判定；删表改由 CQ-07 执行且不推进仓库 schema；CQ-03 改为 forward-only；CQ-05 按单谓词重写并固定入队后的退出码；发布验证改为 nextest；补上可复制的 rg 守卫和 D 组 | CQ-03 的删表 → CQ-07 | 实施顺序、REL-CQ-01、审计表、追溯表、ADR-CQ-04 |
| 2026-11-16 13:05:45 UTC | Codex R2 FAIL | 补 Derived 角色与 bootstrap/top-up 空集；旧读者判定落到 `capture_db_old_reader_schema_unchanged`；CQ-05 改为前滚，恢复命令是 `libra __capture-spool-drain` | 无拆卡 | CQ-05 Rollback、GC-CQ-02、完成判据 |
| 2026-11-16 13:07:49 UTC | Codex R3 FAIL | M1 恢复动作改为安装本版本或更新版本后执行 `libra __capture-spool-drain`。删表归属从残留的 CQ-03 改为 CQ-07 | 无拆卡 | 里程碑、风险表、CQ-02 Out of scope |
| 2026-11-16 13:10:31 UTC | Codex R4 FAIL | 默认覆盖表的 CQ-05 改为 forward-only。`ready/` 与 `quarantine/` 共用 1024 文件或 256 MiB；`tmp/` 超过 24 小时由消费者删除 | 无拆卡 | 默认覆盖、CQ-05 AC/预算、故障矩阵、容量表 |
| 2026-11-16 13:14:31 UTC | Codex R5 FAIL | 容量检查、发布和过期 `tmp/` 删除都放进 `capacity.lock`；`tmp/` 最多 64 个，不依赖消费者启动 | 无拆卡 | ADR-CQ-02、CQ-05 AC/验证/预算、故障矩阵 |
| 2026-11-16 13:17:14 UTC | Codex R6 FAIL | `tmp/` 改为在 `capacity.lock` 内 `O_EXCL` 创建，锁外才写内容，避免并发创建越过 64 | 无拆卡 | ADR-CQ-02、CQ-05 AC/预算、故障矩阵、容量表 |
| 2026-11-17 16:55:05 UTC | CQ-05 实施 | 消费者重放使用自己的捕获期限（提供方自管导出窗口的沿用其预算，其余 30 秒），数据库等待用满该期限；消费者只继承白名单环境变量（含提供方目录、沙箱、对象存储），丢弃 API key；`__capture-spool-drain` 在有信封进入 `quarantine/` 时非零退出；入队前只用文件元数据检查 `libra.db` 是否存在（`LBR-REPO-002`/`LBR-IO-001`），schema 过新由消费者报告。新增调试构建测试缝 `LIBRA_TEST=1` + `LIBRA_TEST_CAPTURE_SPOOL_NO_CONSUMER=1`。捕获新测试以 `LIBRA_RUN_CAPTURE_TESTS=1` 门控，不进入 GitHub Actions | 无拆卡；新增 FIX-CQ-01 | CQ-05 写集、ADR-CQ-03、修复登记、实施证据汇总 |
| 2026-11-17 20:52:53 UTC | FIX-CQ-02 | `cloud sync`/`cloud restore` 改从 `capture.db` 读写目录，`libra.db` 只留 ref 与对象索引；冻结对两个库各做变更检测 | 无拆卡 | 修复登记、实施证据汇总 |
| 2026-11-17 21:50:12 UTC | CQ-06 实施 | doctor 报告 `capture.db` 可否打开（schema 过新、库忙、损坏三种文案与码）与 `ready`/`quarantine` 文件数和字节数（只 stat、上限 1024） | 无拆卡 | CQ-06 卡片、实施证据汇总 |

## 已决议设计决策

实现时偏离本节必须先改本文。

### ADR-CQ-01: 捕获目录使用独立 SQLite 文件

- **Status:** Accepted
- **Context:** 捕获表与仓库表没有跨表外键，但共享 `libra.db` 的单写者和 Repository schema 账本。
- **Decision:** 新增 `DatabaseRole::Capture`。文件为 `.libra/private/capture.db`，自有 `schema_versions`。目录事务只提交这个文件。对象仍先写入 `.libra/objects`，再提交目录行。禁止 `ATTACH` 把两个库绑进同一个连接。
- **Alternatives considered:** 继续共用 `libra.db`，只加长 busy 等待。拒绝：仓库命令和捕获迁移仍互相阻塞，schema 拒绝范围也不对。
- **Consequences:** 旧二进制在家族发布后不能再从 `libra.db` 读捕获目录。发布说明必须写明需要同版或更新的 Libra。
- **Revisit when:** 出现必须和 ref 更新原子提交的捕获写入。当前没有这种写入。

### ADR-CQ-02: 队列是文件系统，不是第二张 SQL 表

- **Status:** Accepted
- **Context:** hook 是短命进程。队列若在 `capture.db` 中，入队仍要写锁。
- **Decision:** `.libra/private/capture-spool/{tmp,ready,quarantine}/`。同目录 `fsync` 文件、`rename` 到 `ready/`、再 `fsync` 目录。文件名是排序前缀、ULID 和去重键。`O_EXCL` 已存在视为入队成功。内容是已脱敏、带版本号的内部信封。`tmp/` 文件必须在持有 `capture-spool/capacity.lock` 时用 `O_EXCL` 创建。创建前先在同一临界区删除修改时间超过 24 小时的 `tmp/`，并确认创建后 `tmp/` 仍不超过 64 个；确认失败就不创建。随后可以释放锁，再写入并 `fsync`。再次拿锁后才统计 `ready/` 与 `quarantine/` 并 `rename`。这把锁不负责数据库写入。
- **Alternatives considered:** 只加长 200ms 帽。拒绝：写事务已观察到超过 2 秒，帽再加长仍会堆到 host 的 30 秒杀进程。
- **Consequences:** 查询落后于 hook 返回，正常为秒级。
- **Revisit when:** 单消费者重放持续超过 60 秒且积压经常顶到上限。

### ADR-CQ-03: 每个仓库一个消费者

- **Status:** Accepted
- **Context:** DELETE 日志只有一个写者。多个 drainer 会把锁争用搬回 `capture.db`。
- **Decision:** 复用恢复 worker 的仓库级 OS 锁语义，锁名与恢复 worker 分开。抢不到锁的进程立即退出。hook 入队后最多尝试拉起一次消费者。消费者用 30 秒连接超时，按文件名顺序重放现有目录事务，提交成功才删除 `ready/` 文件。
- **Alternatives considered:** 每个 hook 直接写 `capture.db` 并等待数秒。拒绝：Pi 的 2 秒宿主超时和 OpenCode 的 16 进程上限仍然会杀进程。
- **Consequences:** 消费者崩溃不丢 `ready/` 文件。锁随进程退出释放。
- **Revisit when:** 需要跨仓库的全局消费者。本计划不需要。

### ADR-CQ-04: hook 成功只表示已入队

- **Status:** Accepted
- **Context:** 今天 hook 退出 0 表示目录事务已经提交。
- **Decision:** 实时 hook 退出 0 只表示 `ready/` 里有完整信封，或同一去重键已经存在。消费者启动失败、捕获库忙、重放失败都不再改变这个退出码；那些结果留在 `ready/` 或 `quarantine/`，由下一次消费者或 doctor 处理。hook 只有在信封没有 durable 落入 `ready/` 时才失败，包括队列已满、入队前校验失败和 Pi 硬超时。`libra agent` 的查询不读队列。`doctor` 报告 `ready/` 与 `quarantine/` 的文件数和字节数。
- **Alternatives considered:** hook 阻塞到消费者提交。拒绝：并行 hook 会重新串成长等待。
- **Consequences:** 命令文档必须改掉「hook 返回即已捕获进库」的隐含含义。
- **Revisit when:** 产品要求工具返回前读到自己的捕获。那是 DEFER-CQ-02，不在本计划。

### ADR-CQ-05: 终态收条优先，import 不入队

- **Status:** Accepted
- **Context:** Codex `SessionEnd` 需要内容无关的收条；`Stop` 今天会占写锁直到 30 秒被杀。import 已经在一个长寿命进程里。
- **Decision:** `SessionEnd` 和内容无关的终态收条使用排序前缀 `0-`，其它事件使用 `1-`，后面再接 ULID。消费者按文件名顺序，因此终态先入库。目录写入必须允许工具事件先于 `SessionStart` 被重放。`libra agent import` 直接写 `capture.db`。Gemini 入口保持拒绝，不写队列。
- **Alternatives considered:** 用单独的高优先级数据库锁。拒绝：hook 又要开库。
- **Consequences:** 重放测试必须覆盖「工具事件先于会话行」。
- **Revisit when:** 终态收条的目录契约不再允许缺席的会话行。

### ADR-CQ-06: 共享入口一处切换

- **Status:** Accepted
- **Context:** 六个实时 Agent 都调用 `ingest_agent_traces`。
- **Decision:** 只在 AgentTraces 实时路径把开库改成入队。不修改各 provider parser。非 Unix 的 Codex 平台错误和 Claude `SessionStart` 的信任提示仍在入队前返回。
- **Alternatives considered:** 每个 provider 各写一个队列适配器。拒绝：行为会分叉，也突破落点上限。
- **Consequences:** 一个回归会同时影响六个 Agent，所以 CQ-05 的测试必须点名这六个入口。
- **Revisit when:** 某个 provider 的实时路径不再进入 `ingest_agent_traces`。

## 全局工程约束

模板 GC-01..GC-13 全部生效。下面只写本计划的附加约束。

- **GC-CQ-01 单连接入口:** 捕获目录的打开、迁移和兼容检查只能经过 `capture_store`。禁止新增直接对 `capture.db` 调用 `establish_connection` 的生产路径。
- **GC-CQ-02 角色隔离:** 角色、账本和 writer 的封闭清单见下表。Capture migration 不得创建或推进 Repository、GlobalConfig、SystemConfig 的 receipt，也不得在那三个库里建捕获表。Repository migration 不得写入 `capture.db`。从 `libra.db` 移除捕获表不是仓库 schema 升级，不得因此增加 `libra.db` 的 `schema_versions`；否则旧二进制会拒绝打开整个仓库。

| 角色 | 文件 | 账本 | 本计划允许的 writer | 禁止 |
|---|---|---|---|---|
| Repository | `.libra/libra.db` | `schema_versions` | 现有仓库 migration。CQ-07 只按主键删除已搬空的捕获表，不写 receipt | 不执行 Capture migration，不建新的捕获 receipt |
| Capture | `.libra/private/capture.db` | 该文件自己的 `schema_versions` | `capture_store`、CQ-01 的拷贝、CQ-07 在此记录 detach receipt | 不写 refs、config、sequencer |
| GlobalConfig | 临时测试库 / 真实 `~/.libra/config.db` 只允许负向断言 | `configuration_schema_versions` | 无 | 本计划任何测试和工具都不得写入真实文件 |
| SystemConfig | 临时测试库 / 真实 `/etc/libra/config.db` 只允许负向断言 | `configuration_schema_versions` | 无 | 同上 |
| Derived | 不存在本计划使用的派生库 | 无 | 无 | 不打开、不建库、不做 migration、bootstrap 或 top-up |
- **GC-CQ-03 入队前脱敏:** spool 文件只保存已经走完现有 redaction 的字节。失败的入队不得留下可读的未脱敏 `ready/` 文件。
- **GC-CQ-04 写锁范围:** 消费者持有 `capture.db` 写事务期间，不得做文件哈希、工作树扫描或网络。这些工作留在入队进程或事务之外。

## 执行检查必备需求（强制）

ER-01..ER-14 按模板 v2.16 执行，本文不削弱。补充：

- 家族子卡 CQ-01、CQ-02、CQ-03、CQ-07 不 push、不 bump。只有 CQ-04 的集成树进入远端。
- CQ-05 与 CQ-06 是 CQ-04 之后的独立发布切片。
- 全量命令只使用 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。
- 终止链若将来出现纯文档末端提交，其 D 组证据按模板 v2.14 由下一次 `plan-status.md` 例行同步补记。本计划当前没有这种卡。

## 实施顺序

- `CQ-01 -> CQ-02`
- `CQ-02 -> CQ-03`
- `CQ-03 -> CQ-07`
- `CQ-07 -> CQ-04`
- `CQ-04 -> CQ-05`
- `CQ-05 -> CQ-06`

### 依赖登记表

本计划没有计划外依赖，表中无 `DEP-*` 行。各 Agent 解析器计划已经交付实时入口，不构成等待项。

### 发布分组与并发窗口

| ID | 成员 | 唯一发布点 | 窗口规则 | 失败回滚顺序 | 理由 |
|---|---|---|---|---|---|
| REL-CQ-01 | CQ-01、CQ-02、CQ-03、CQ-07、CQ-04 | CQ-04 | 子卡只在本地提交。窗口内不推送、不 bump。CQ-04 一次发布集成树 | 未推送的子卡直接放弃该分支；已发布的 CQ-04 只能前滚 | 只切换一部分调用点，或删表而未切走读写，会丢捕获或把读写指向不同文件 |

**并发声明:** 全串行。CQ-07 删除 `libra.db` 里的捕获表，必须晚于 CQ-02 和 CQ-03。禁止与其它计划同时修改 `src/internal/db/schema.rs`、`src/internal/db/migration.rs`、`src/internal/ai/capture/live_pipeline.rs`、`src/command/hooks.rs`。

**发布者:** 接到对应发布卡执行授权的那一个 Agent。同一时刻只有一张卡处于已 bump 未完成 release。

**发布窗口顺序:** CQ-04，然后 CQ-05，然后 CQ-06。

### Phase 0: 捕获库家族

**目标:** `capture.db` 成为捕获目录的唯一生产库，且只有集成树被发布。

**进入条件:**

- 本文 Codex review 为 PASS。
- ER-01 确认分支 `plan/20261116-capture-queue`，目标文件无无关未提交改动。

**退出条件:**

- CQ-04 `Acceptance=complete`。
- 仓库命令与捕获写入使用不同的数据库文件。

### Phase 1: 文件系统队列

**目标:** 实时 hook 不再为了入库打开 SQLite。

**进入条件:**

- CQ-04 已发布，`capture_store` 是捕获连接的唯一入口。

**退出条件:**

- CQ-06 `Acceptance=complete`。
- 计划完成判据全部勾选。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `revert`
- **Migration and rollback 默认:** `N/A`
- **Security and privacy 默认:** 继承 GC-07、GC-11、GC-CQ-03
- **Performance budget 默认:** 继承 GC-10

**默认覆盖:**

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| CQ-01 | `Task type` / `Rollback mode` / `Release boundary` | `migration` / `forward-only` / `family child of REL-CQ-01`。新建库角色，子卡不单独发布 |
| CQ-02 | `Release boundary` | `family child of REL-CQ-01`。单独发布会使内部写入指向新库而命令仍读旧库 |
| CQ-03 | `Release boundary` | `family child of REL-CQ-01`。读路径切换必须和 CQ-02 同树发布 |
| CQ-04 | `Task type` / `Release boundary` | `release` / `family release point of REL-CQ-01`。不引入新行为 |
| CQ-05 | `Rollback mode` | `forward-only`。恢复是安装本版本或更新版本后执行 `libra __capture-spool-drain`，不 revert 消费者 |

没有 `EX-*`。G-01、G-02、G-05..G-11 不豁免，本计划也不申请 G-03 或 G-04 豁免。

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| CQ-01 | migration | capture database role | 停止新库创建；已拷贝数据保留在 capture.db，libra.db 原表仍在 | yes | yes | 6/8 | 3/8 | 3/5 | M | none | no-overlap | family child | N/A | N/A |
| CQ-02 | implementation | capture-internal writers | revert 本卡后内部写入回到 libra.db；拷贝数据仍在 | yes | yes | 4/8 | 2/8 | 1/8 | M | CQ-01 | no-overlap | family child | N/A | N/A |
| CQ-03 | implementation | agent command readers | 未发布则放弃分支；发布后只能前滚，不能把读路径退回 libra.db | yes | yes | 4/8 | 2/8 | 2/12 | M | CQ-02 | 序列化于 CQ-02 | family child | N/A | N/A |
| CQ-07 | migration | detach repository catalog | 未发布则放弃分支；已删表只能前滚 | yes | yes | 4/8 | 2/8 | 1/1 | S | CQ-03 | 序列化于 CQ-03 | family child | CQ-03 | N/A |
| CQ-04 | release | integrated capture.db release | 未推送则放弃分支；已推送则前滚 | yes | yes | 4/12 | 3/12 | 0/0 | S | CQ-07 | no-overlap | family release point | N/A | N/A |
| CQ-05 | implementation | queued hook ingress | 前滚：安装本版本或更新版本后运行 spool drain 入口 | yes | yes | 8/8 | 4/8 | 4/7 | M | CQ-04 | no-overlap | independent | N/A | N/A |
| CQ-06 | implementation | doctor spool report | revert 后 doctor 不再报告队列，文件仍在 | yes | yes | 4/8 | 1/8 | 1/1 | S | CQ-05 | no-overlap | independent | N/A | N/A |

### Task CQ-01: 建立 Capture 数据库角色并拷贝目录

**Task type:** `migration`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 增加 `DatabaseRole::Capture` 和 `.libra/private/capture.db`，把现有捕获目录拷贝进去，并提供唯一打开入口 `capture_store`。本卡不切换生产调用点，也不删除 `libra.db` 里的原表。

**Out of scope:**

- 生产读写改走新库：由 CQ-02、CQ-03 承接。
- 删除旧表：由 CQ-07 承接。
- hook 队列：由 CQ-05 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 角色枚举没有 Capture | `src/internal/db/schema.rs:23-28` |
| 最新版本只解释 Repository 账本 | `src/internal/db/migration.rs:3415-3418` |
| 捕获表住在仓库库 | `src/internal/db/migration.rs:572-624` |

**Acceptance criteria:**

- [x] 新仓库创建 `.libra/private/`（0700）和 `capture.db`，其 `schema_versions` 与仓库库分开。
- [x] Capture migration 不修改 Repository、GlobalConfig、SystemConfig 的 receipt。
- [x] 拷贝可重入：第二次执行不增加行数。
- [x] `capture.db` schema 过新时只拒绝该文件，同一夹具的 `libra.db` 仍能打开。
- [x] 本卡结束后 `libra.db` 的 `schema_versions` 最大值与拷贝前相同，旧表仍在。
- [x] 生产调用点还没有改走 `capture_store`。本卡测试是唯一调用者。

**Verification:**

- [x] `source .env.test && cargo nextest run -E 'test(/^capture_db_/)'`（new：角色、四库隔离、重入拷贝、未来 schema、仓库 schema 版本不变）
- [x] `if rg -n 'capture_store::' src --glob '!src/internal/ai/capture/store.rs'; then echo 'FAIL: production caller'; exit 1; else rc=$?; if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi; echo 'OK: zero hits'; fi`
- [x] `source .env.test && cargo nextest run --test compat_global_config_schema_future`

**Full-suite trigger:** `N/A`（family child，不执行 C 组）

**Dependencies:** 无

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/db/schema.rs`、`src/internal/db.rs`、`src/internal/db/migration.rs`、`sql/migrations/2026111601_capture_database.sql`、`src/internal/ai/capture/store.rs`、`tests/db_migration_test.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列写集。

**Docs and compatibility impact:** `N/A`。本卡不改变用户可见命令、输出或错误码。用户可见的库文件切换在 CQ-04 的集成树文档里记录。

**Rollback mode:** `forward-only`

**Migration and rollback:** 不删除旧表，但已经写成的 `capture.db` 不自动删除，所以恢复不是 revert。未推送时放弃家族分支。已发布后的恢复是继续使用该版本或更新版本。恢复验证是 `capture_db_old_reader_schema_unchanged`：仓库 schema 最大值不变，global/system receipt 不变。

**Security and privacy:** 新文件继承 `.libra/private` 的 0700。拷贝不打印行内容。

**Performance budget:** 拷贝只在第一次打开缺失的 `capture.db` 时运行，按主键分批，每批最多 500 行；不得一次把全部 transcript 载入内存。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-CQ-01`（1/5）

**C/D coverage from:** `CQ-04`

**Granularity:** `type=migration; axis=capture database role; recovery=abandon unpublished branch; after publish keep capture.db and the unchanged repository schema; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=3; prod-files=5; scope=M; deps=none; writeset=no-overlap; release=family child; split-from=N/A; exception=N/A`

**Migration allowlist:** 只有新文件 `sql/migrations/2026111601_capture_database.sql`。它既是 Capture 的 bootstrap，也是唯一的 migration；没有第二条 DDL，也没有 schema top-up。Capture runner 只在创建 `capture.db` 时执行这一份 SQL。它不进入 Repository 的 `builtin_migrations()`。Repository、GlobalConfig、SystemConfig、Derived 都没有新的 migration、bootstrap 或 top-up。旧二进制兼容用例是 `capture_db_old_reader_schema_unchanged`：断言 `libra.db` 的 schema 最大值不变，因此旧二进制的 future-schema 拒绝不会被触发。CI 不下载旧二进制；这个断言就是该判定的可执行用例。

### Task CQ-02: 捕获内部写入改走 capture_store

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 把 `src/internal/ai/capture/` 里直接打开仓库库的生产写入改为 `capture_store`。命令层读路径本卡不动。

**Out of scope:**

- `src/command/agent/` 与 `src/internal/ai/hooks/intent.rs`：由 CQ-03 承接。
- 删除 `libra.db` 中的捕获表：由 CQ-07 承接。
- 队列：由 CQ-05 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 内部捕获模块直接持有仓库连接 | `src/internal/ai/capture/live_pipeline.rs:339`；`src/internal/ai/capture/catalog.rs` 的目录事务 |

**Acceptance criteria:**

- [x] 写集内八个生产文件的捕获读写都经过 `capture_store`，不再对 `libra.db` 调用 `establish_connection`。
- [x] 目录事务提交的是 `capture.db`。对象仍写在 `.libra/objects`。
- [x] 开工时若发现写集外还有 `src/internal/ai/capture/` 生产文件直接开仓库库，先按 ER-03 拆卡，不得在本卡扩写。
- [x] 既有捕获单测在只提供 `capture.db`、且 `libra.db` 不含捕获表的夹具上通过。该夹具只在本卡测试里使用，不代表生产已经删表。

**Verification:**

- [x] `source .env.test && cargo nextest run -E 'test(/^capture_store_writer_/)'`（new）
- [x] `if rg -n 'establish_connection|get_db_conn' src/internal/ai/capture/live_pipeline.rs src/internal/ai/capture/live_checkpoint.rs src/internal/ai/capture/checkpoint.rs src/internal/ai/capture/snapshot.rs src/internal/ai/capture/live.rs src/internal/ai/capture/scope_binding.rs src/internal/ai/capture/key.rs src/internal/ai/capture/catalog.rs; then echo 'FAIL: repository connection remains'; exit 1; else rc=$?; if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi; echo 'OK: zero hits'; fi`

**Full-suite trigger:** `N/A`

**Dependencies:** CQ-01 的 `capture_store::open` 与已迁移的捕获 schema。

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/ai/capture/live_pipeline.rs`、`src/internal/ai/capture/live_checkpoint.rs`、`src/internal/ai/capture/checkpoint.rs`、`src/internal/ai/capture/snapshot.rs`、`src/internal/ai/capture/live.rs`、`src/internal/ai/capture/scope_binding.rs`、`src/internal/ai/capture/key.rs`、`src/internal/ai/capture/catalog.rs`、对应现有测试。

**Release write set:** `N/A`

**Files likely touched:** 上列文件。

**Docs and compatibility impact:** `N/A`。没有独立于 CQ-04 集成树的用户可见行为。子卡不单独发布。

**Rollback mode:** `revert`

**Migration and rollback:** `N/A`。本卡不新写 schema。revert 后内部代码回到 `libra.db`；CQ-01 留下的拷贝不删除。

**Security and privacy:** Inherited

**Performance budget:** Inherited

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-CQ-01`（2/5）

**C/D coverage from:** `CQ-04`

**Granularity:** `type=implementation; axis=capture-internal writers; recovery=revert this card; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=8; scope=M; deps=CQ-01; writeset=no-overlap; release=family child; split-from=N/A; exception=N/A`

### Task CQ-03: Agent 命令读路径改走 capture_store

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 把已经列出的 Agent 命令和 intent hook 的捕获读取改为 `capture_store`。本卡不删除 `libra.db` 里的旧表。

**Out of scope:**

- 捕获内部模块：由 CQ-02 承接，本卡不改那些文件。
- 删除旧表：由 CQ-07 承接。
- hook 入队：由 CQ-05 承接。本卡之后 hook 仍同步写 `capture.db`。
- import 改走队列：永久非目标，理由见 ADR-CQ-05。import 只改连接，仍直接写库。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 命令层与 intent 仍打开仓库库 | `src/command/agent/doctor.rs:1784-1788`；`src/internal/ai/hooks/intent.rs` |

**Acceptance criteria:**

- [x] 写集中的 12 个生产文件通过 `capture_store` 读写捕获目录。
- [x] 这 12 个文件不再调用仓库连接帮助函数。
- [x] 本卡结束时 `libra.db` 里的捕获表还在。
- [x] 开工时写集外若还有生产读路径，先按 ER-03 拆卡。

**Verification:**

- [x] `source .env.test && cargo nextest run -E 'test(/^capture_store_reader_/)'`（new：12 个入口的连接归属）
- [x] `if rg -n 'establish_connection|get_db_conn' src/command/agent/status.rs src/command/agent/skill.rs src/command/agent/checkpoint.rs src/command/agent/bridge.rs src/command/agent/clean.rs src/command/agent/mega2_stream.rs src/command/agent/graph.rs src/command/agent/import.rs src/command/agent/doctor.rs src/command/agent/workspace.rs src/command/agent/session.rs src/internal/ai/hooks/intent.rs; then echo 'FAIL: repository connection remains'; exit 1; else rc=$?; if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi; echo 'OK: zero hits'; fi`

**Full-suite trigger:** `N/A`

**Dependencies:** CQ-02。内部写入已经改走 `capture_store`。CQ-01 的拷贝由 CQ-02 传递。

**Deliverables:** `N/A`

**Implementation write set:** `src/command/agent/status.rs`、`src/command/agent/skill.rs`、`src/command/agent/checkpoint.rs`、`src/command/agent/bridge.rs`、`src/command/agent/clean.rs`、`src/command/agent/mega2_stream.rs`、`src/command/agent/graph.rs`、`src/command/agent/import.rs`、`src/command/agent/doctor.rs`、`src/command/agent/workspace.rs`、`src/command/agent/session.rs`、`src/internal/ai/hooks/intent.rs`、对应测试。

**Release write set:** `N/A`

**Files likely touched:** 上列文件。

**Docs and compatibility impact:** `N/A`，理由同 CQ-02。集成树的用户说明由 CQ-04 写入。

**Rollback mode:** `forward-only`

**Migration and rollback:** 未推送时放弃整支家族分支，不把读路径单独 revert 回 `libra.db`。CQ-04 发布之后，新写入只在 `capture.db`；恢复动作是继续使用该版本或更新版本，不用旧二进制读捕获目录。恢复验证是 `capture_store_reader_` 仍指向 `capture.db`。本卡不删表。

**Security and privacy:** Inherited

**Performance budget:** Inherited

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-CQ-01`（3/5）

**C/D coverage from:** `CQ-04`

**Granularity:** `type=implementation; axis=agent command readers; recovery=abandon unpublished branch; after publish stay on capture.db; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=12; scope=M; deps=CQ-02; writeset=序列化于 CQ-02; release=family child; split-from=N/A; exception=N/A`

### Task CQ-07: 从仓库库摘除已搬空的捕获表

**Task type:** `migration`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 在读写都已经使用 `capture.db` 之后，按主键确认行已经拷贝，再从 `libra.db` 删除捕获目录表。删除不写入仓库 `schema_versions`，避免旧二进制把整个仓库库当成过新 schema。

**Out of scope:**

- 切换读写：由 CQ-02、CQ-03 承接。
- 把删除登记成 Repository migration：永久非目标。那会让旧二进制拒绝打开 `libra.db`。
- hook 队列：由 CQ-05 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 捕获表由 Repository migration 创建 | `sql/migrations/2026050303_agent_capture.sql` |
| 旧二进制按仓库 schema 版本拒绝打开仓库库 | `src/internal/db/schema.rs:389-398` |

**Acceptance criteria:**

- [x] 只有 `capture.db` 含有 `libra.db` 中每一张捕获表的全部主键时才删除这些表。
- [x] 缺行时一张表都不删，并返回失败。
- [x] 重跑看到表已不存在则成功，不再次删除其它表。
- [x] `libra.db` 的最大 `schema_versions` 在删除前后相同。GlobalConfig 与 SystemConfig 的 receipt 也不变。

**Verification:**

- [x] `source .env.test && cargo nextest run -E 'test(/^capture_catalog_detach_/)'`（new：缺行拒绝、重入、仓库 schema 版本不变、配置库不变）
- [x] `if rg -n 'capture_catalog_detach' src/internal/db/migration.rs sql/migrations; then echo 'FAIL: detach registered as repository migration'; exit 1; else rc=$?; if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi; echo 'OK: zero hits'; fi`

**Full-suite trigger:** `N/A`

**Dependencies:** CQ-03。命令读路径已经改走 `capture_store`。

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/ai/capture/detach.rs`、`tests/db_migration_test.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列两个文件。

**Docs and compatibility impact:** `N/A`。旧读者影响由 CQ-04 写入用户文档。

**Rollback mode:** `forward-only`

**Migration and rollback:** 不提供把已删表按空壳建回 `libra.db` 的 down migration。未推送时放弃家族分支。已发布后，捕获行只从 `capture.db` 读取。恢复验证是 detach 测试仍拒绝缺行删除，且仓库 schema 版本未增加。

**Security and privacy:** 删除语句不得把行内容写进错误或日志。

**Performance budget:** 按主键分批比较，每批最多 500 个键，不把 transcript 载入内存。

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-CQ-01`（4/5）

**C/D coverage from:** `CQ-04`

**Granularity:** `type=migration; axis=detach repository catalog; recovery=abandon unpublished branch; after publish do not recreate libra.db capture tables; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=1; scope=S; deps=CQ-03; writeset=序列化于 CQ-03; release=family child; split-from=CQ-03; exception=N/A`

### Task CQ-04: 发布捕获库集成树

**Task type:** `release`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 发布同时包含 CQ-01、CQ-02、CQ-03、CQ-07 的树。本卡不写新的捕获行为，只做版本、聚合守卫、文档和 ER-08 发布。

**Out of scope:**

- 队列与 hook 入队：由 CQ-05 承接。
- 新的 schema 变更：尚未排期。重启条件是集成树发布后发现漏表。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 四张子卡在本卡之前都还不能单独推送 | REL-CQ-01 |

**Acceptance criteria:**

- [x] 集成树上捕获读写只使用 `capture.db`，`libra.db` 没有捕获目录表。
- [x] 聚合守卫覆盖拷贝重入、读写连接归属和仓库命令不被捕获写锁阻塞。
- [x] `docs/commands/agent.md` 与 `docs/commands/zh-CN/agent.md` 说明捕获目录位于 `.libra/private/capture.db`，旧二进制不能读取该目录。
- [x] `COMPATIBILITY.md` 记录这一存储位置变化和旧读者行为。

**Verification:**

- [ ] `source .env.test && cargo nextest run -E 'test(/^capture_db_/) + test(/^capture_store_/) + test(/^capture_catalog_detach_/)'` — 已停止。链接全部测试产物时负载超过 400。未跑完，不算通过。
- [x] 同一谓词由 `cargo test --test db_migration_test --jobs 6 capture_ -- --test-threads=1`（8 通过，0 失败）和 `cargo test --lib --jobs 6 capture_store_ -- --test-threads=1`（3 通过，0 失败）执行。覆盖 `capture_db_`、`capture_store_` 与 `capture_catalog_detach_`。
- [ ] `source .env.test && cargo nextest run --test compat_version_surface_sync` — 未运行。
- [x] 同一目标由 `cargo test --test compat_version_surface_sync --jobs 6 -- --test-threads=1` 执行：2 通过，0 失败。
- [x] `if rg -n 'capture\.db' docs/commands/agent.md docs/commands/zh-CN/agent.md COMPATIBILITY.md; then echo 'OK: docs mention capture.db'; else rc=$?; if [ "$rc" -eq 1 ]; then echo 'FAIL: missing capture.db docs'; exit 1; fi; echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi`

**远端 D 组（不计入 AC/VER）:** `base.yml` 的 `format`、`clippy`、`redundancy`、`test` 必须在本卡 PR head 的 `pull_request` 上全部 success。`codeql.yml` 的 `analyze` 必须在同一个 head 上 success。`release.yml` 的 `build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 必须在 `gh release create` 产生的 `v*` tag push 上全部 success。缺任何一个 job 或 head 不一致时，本卡停在 `remote-pending`。

**Full-suite trigger:** `T-2: family release point`

**Dependencies:** CQ-07 的最终树。CQ-01、CQ-02、CQ-03 由该边传递。

**Deliverables:** `N/A`

**Implementation write set:** `docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`。不得改 `src/**` 或 `sql/**`。若集成失败，修在原行为卡，不在本卡加行为。

**Release write set:** Inherited

**Files likely touched:** 文档三件与版本面。

**Docs and compatibility impact:** `docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`../libra-backend` 上对应的 agent 命令页、`COMPATIBILITY.md`。网站页按 ER-06a 在已验证的 `cf` 分支更新。错误码本卡不新增，`docs/error-codes.md` 为 `N/A`。

**Rollback mode:** `forward-only`

**Migration and rollback:** 已发布的二进制会把新仓库写成 `capture.db`。回滚代码不能把新写入搬回 `libra.db`。用户继续使用本版或更新版本。恢复验证是 doctor 能打开 `capture.db`。

**Security and privacy:** Inherited

**Performance budget:** `N/A`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `family release point of REL-CQ-01`

**C/D coverage from:** `self`

**Granularity:** `type=release; axis=integrated capture.db release; recovery=forward-only after publish; complete=yes; self-contained=yes; AC=4/12; VER=3/12; landing=0; prod-files=0; scope=S; deps=CQ-07; writeset=no-overlap; release=family release point; split-from=N/A; exception=N/A`

D 组三条不计入 VER。它们是 ER-04 的远端门。

### Task CQ-05: 实时 hook 改为入队，由单消费者入库

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** AgentTraces 实时路径不再打开数据库。它把已脱敏信封原子写入 spool，并尝试拉起一个消费者。消费者把 `ready/` 写入 `capture.db`。这是本计划对 200ms 锁失败的修复。

**Out of scope:**

- doctor 的积压报告：由 CQ-06 承接。
- 改 parser、插件和项目 hook 配置：永久非目标。
- import 入队：永久非目标，ADR-CQ-05。
- Gemini 入队：永久非目标。它保持现有拒绝。
- 查询命令读 spool：尚未排期，见 DEFER-CQ-02。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 开库等待帽 200ms，失败文案把锁冲突说成新库 | `src/internal/ai/capture/live.rs:215`；`src/command/hooks.rs:359-364` |
| 六个实时入口汇入同一 ingest | `src/internal/ai/hooks/runtime.rs:244-248` |
| Pi 进程有硬超时 | `src/command/agent/hooks.rs:233-252` |

**Acceptance criteria:**

- [x] hook 退出 0 当且仅当 `ready/` 里已经有该去重键的完整文件。例外：Claude `observe` 对它不记录的事件（缺 session、超长、格式不对）保持静默退出 0，这类事件在入队前就被拒绝，不产生文件；已通过校验但写不进队列的事件（队列满、文件系统错误）照常失败（`capture_spool_hook_observe_queue_full_fails_with_stable_code`）。
- [x] 消费者启动失败或之后入库失败不改变这个退出 0。
- [x] `rename` 之前崩溃不会留下 `ready/` 文件。
- [x] 同一个 `ready/` 文件重放两次只产生一行目录。
- [x] 排序前缀 `0-` 的文件先于 `1-` 的文件被消费。
- [x] `tmp/` 的 `O_EXCL` 创建、过期删除、`ready/` 发布都在 `capacity.lock` 内完成。创建时 `tmp/` 不超过 64 个；发布时 `ready/` 与 `quarantine/` 合计不超过 1024 个文件或 256 MiB。越限返回队列已满错误码，文案不包含 `newer Libra`。并发创建和并发发布都不能越过各自的上限。
- [x] 第二个消费者抢不到锁时不会读取 `ready/`。
- [x] 1 KiB 信封的入队路径不调用目录检查点。

六个 provider 共用这一条 AgentTraces 分支，由下面的表驱动测试一次覆盖，不拆成六条 AC。schema 过新、捕获库忙和文件损坏只由消费者或 doctor 报告，不由已经入队的 hook 返回。

**Verification:**

- [ ] `source .env.test && cargo nextest run -E 'test(/^capture_spool_/)'` — 原命令的 `^` 锚点匹配不到带模块前缀的单测，不作为本卡证据。
- [x] `source .env.test && cargo nextest run -E 'test(/capture_spool_/)'` — 实际执行的命令：35 项，35 通过（含 `spool.rs`/`drain.rs` 单测与 7 项既有 `command::cloud` 用例）。（new：原子发布、崩溃窗口、去重、终态顺序、`capacity.lock` 下的并发名额、`tmp/` 24 小时清理、消费者单锁）
- [x] `source .env.test && cargo nextest run --test agent_lifecycle_event_test capture_spool_hook_`（new：六个入口不开库、错误文案）
- [x] `source .env.test && cargo nextest run --features test-live-agent --run-ignored all --test agent_opencode_bridge_test capture_spool_`（该 target 需要 `test-live-agent` 特性；按 `compat_agent_architecture_guard` 的要求，该 target 的用例都是 `#[ignore]` 的操作者本地门，须显式选中 ignored，选中后不再跳过）（new：OpenCode 转发进程在入队后返回）
- [x] `if rg -n 'establish_connection|get_db_conn' src/internal/ai/capture/live_pipeline.rs; then echo 'FAIL: live ingest still opens a database'; exit 1; else rc=$?; if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi; echo 'OK: zero hits'; fi`

**远端 D 组（不计入 AC/VER）:** 与 CQ-04 相同的三个 workflow 判据，但 head 和 tag 换成 CQ-05 自己的 PR 与版本。

**Full-suite trigger:** `T-1: 新增稳定错误码并修改共享 hook 入口`

**Dependencies:** CQ-04 已发布的 `capture_store` 与不含捕获表的 `libra.db`。

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/ai/capture/spool.rs`、`src/internal/ai/capture/drain.rs`、`src/internal/ai/capture/worker.rs`、`src/internal/ai/capture/live_pipeline.rs`、`src/internal/ai/hooks/runtime.rs`、`src/command/hooks.rs`、`src/utils/error.rs`、`docs/error-codes.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md`、`tests/agent_lifecycle_event_test.rs`、`tests/agent_opencode_bridge_test.rs`。spool 行为测试写在 `src/internal/ai/capture/spool.rs` 的 `#[cfg(test)]` 模块，不新开 `--test` target。

**Release write set:** Inherited

**Files likely touched:** 上列写集。`src/command/agent/hooks.rs` 不改，除非 ER-02 发现它没有复用 `map_capture_ingest_error`；发现后先拆卡。

**Docs and compatibility impact:** `docs/error-codes.md` 增加队列已满、捕获库忙、捕获库 schema 过新三个码。`docs/commands/agent.md` 与 zh-CN 对应页说明 hook 成功即已入队。`COMPATIBILITY.md` 记录退出 0 的新含义。`../libra-backend` 的 agent 命令页按 ER-06a 在已验证的 `cf` 同步。开发文档 `docs/development/commands/` 若已有 hook 入库段落，只更新那一个对应文件，不新增大范围说明。

**Rollback mode:** `forward-only`

**Migration and rollback:** 不通过 revert 恢复。revert 会删掉唯一能读 spool 的消费者，已入队事件会一直不可查询。恢复步骤是安装本卡发布的版本或更新版本，然后在仓库根执行 `libra __capture-spool-drain`。该入口只启动 CQ-05 的消费者，不读取信封内容到 stdout。测试名是 `capture_spool_drain_cli`，包含在 `capture_spool_` 过滤里。doctor 不消费队列。

**Security and privacy:** 信封在 redaction 之后才写盘。队列目录 0700。错误文案不含路径、会话 id 和信封正文。

**Performance budget:** 1 KiB 信封的入队在本地 ext4 上不超过 50ms（测试夹具计时，不含宿主冷启动；不含排队等 `capacity.lock` 的时间）。`tmp/` 文件先在锁内创建，锁外写入。`ready/` 与 `quarantine/` 共用 1024 个文件或 256 MiB，`tmp/` 最多 64 个。过期 `tmp/` 删除也在该锁内，由入队执行，不依赖消费者。`capture_spool_` 覆盖并发创建和并发发布。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=queued hook ingress; recovery=reinstall this or a newer binary and run libra __capture-spool-drain; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=4; prod-files=7; scope=M; deps=CQ-04; writeset=no-overlap; release=independent; split-from=N/A; exception=N/A`

落点固定为四个：`src/internal/ai/capture/`、`src/internal/ai/hooks/`、`src/command/`、`src/utils/`。生产文件固定为七个：`spool.rs`、`drain.rs`、`worker.rs`、`live_pipeline.rs`、`runtime.rs`、`src/command/hooks.rs`、`src/utils/error.rs`。新错误类写在这七个文件里，不改 `scope_binding.rs`，也不改 `src/command/agent/hooks.rs`。

### Task CQ-06: doctor 报告队列积压与捕获库

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** `libra agent doctor` 报告 `capture.db` 是否可打开，以及 spool 的 `ready/`、`quarantine/` 文件数和字节数。它不消费队列，也不在诊断时抢占消费者锁。

**Out of scope:**

- 自动重放或清空 quarantine：尚未排期。重启条件是 CQ-05 的 quarantine 有稳定的失败分类。
- 修改 hook 入队：由 CQ-05 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| doctor 把打不开仓库库渲染成升级或恢复仓库 | `src/command/agent/doctor.rs:1784-1788` |

**Acceptance criteria:**

- [x] doctor 打开的是 `capture.db`。打不开时说明捕获库，而不是仓库库；schema 过新与文件损坏的文案不同。
- [x] 输出包含 `ready` 与 `quarantine` 的文件数和字节数。空队列是 0，不是省略。
- [x] doctor 运行期间，已持有队列锁的消费者不会被 doctor 打断，doctor 也不等待该锁。
- [x] 人读输出与 `--json` 都包含上述计数，且 JSON 不包含文件绝对路径或信封内容。

**Verification:**

- [x] `source .env.test && cargo nextest run --test agent_doctor_repair_test capture_spool_doctor_`（new：人读与 JSON 计数、不抢消费者锁、schema 过新与损坏文案）

**Full-suite trigger:** `none`

**远端 D 组（不计入 AC/VER）:** 与 CQ-04 相同的三个 workflow 判据，但 head 和 tag 换成 CQ-06 自己的 PR 与版本。

**Dependencies:** CQ-05 的 spool 目录布局与错误类。

**Deliverables:** `N/A`

**Implementation write set:** `src/command/agent/doctor.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/error-codes.md`（仅当本卡复用 CQ-05 的码而需要补 doctor 句）、对应 doctor 测试。

**Release write set:** Inherited

**Files likely touched:** 上列文件。

**Docs and compatibility impact:** `docs/commands/agent.md` 与 zh-CN 页增加 doctor 的队列计数。`../libra-backend` 对应 agent 页按 ER-06a 在已验证的 `cf` 同步。`COMPATIBILITY.md` 为 `N/A`：不改变存储格式，只增加诊断字段；若 ER-02 发现兼容矩阵已承诺 doctor 的字段集合，则改为同卡更新该节，并在修订历史登记。

**Rollback mode:** `revert`

**Migration and rollback:** `N/A`

**Security and privacy:** 计数可以公开。路径和信封内容不可以。

**Performance budget:** doctor 只做目录项 `stat`，遇到 1024 个文件即停止精确计数并报告「至少 1024」。不得读取文件内容。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=doctor spool report; recovery=revert doctor output; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=S; deps=CQ-05; writeset=no-overlap; release=independent; split-from=N/A; exception=N/A`

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | 信封排序、容量判断、去重键 | `capture_spool_` |
| 集成 | 六个 hook 入口入队后返回；doctor 计数 | `agent_lifecycle_event_test`、`agent_doctor_repair_test`、`agent_opencode_bridge_test` |
| 兼容 | 版本面；旧读者不能把仓库库误判为捕获 schema | `compat_version_surface_sync`、`capture_db_` |
| 迁移 | 拷贝重入、删表前拒绝未拷贝行、配置库隔离 | `capture_db_`、`compat_global_config_schema_future` |
| 安全 | spool 不含未脱敏字节；错误不含路径和正文 | `capture_spool_` |
| 性能 | 1 KiB 入队上限 50ms；写事务内无目录扫描 | `capture_spool_` 的计时断言 |
| live/gated | 不需要真实模型或云 | `N/A` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| CQ-01 | GAP-01，`schema.rs:23` | `src/internal/db/`、`capture/store.rs`、新 migration | 无独立用户文档 | `capture_db_` |
| CQ-02 | GAP-01，`live_pipeline.rs:339` | `src/internal/ai/capture/` 八文件 | 集成树文档归 CQ-04 | `capture_store_writer_` |
| CQ-03 | GAP-01，`doctor.rs:1784` | `src/command/agent/` 十一文件与 `hooks/intent.rs` | 集成树文档归 CQ-04 | `capture_store_reader_` |
| CQ-07 | 自 CQ-03 拆出的删表 | `src/internal/ai/capture/detach.rs` | 集成树文档归 CQ-04 | `capture_catalog_detach_` |
| CQ-04 | REL-CQ-01 | 命令文档与兼容矩阵 | agent EN/zh、网站、`COMPATIBILITY.md` | `capture_db_` + `capture_store_` + `capture_catalog_detach_` |
| CQ-05 | GAP-02、GAP-03，`live.rs:215` | spool、drain、runtime、hooks、error | 错误码、agent EN/zh、网站、兼容矩阵 | `capture_spool_` |
| CQ-06 | GAP-04 | `doctor.rs` | agent EN/zh、网站 | `capture_spool_doctor_` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | CQ-04 complete，且 CQ-07 已含在该树 | 一个 patch 与 release run | 已发布则前滚；未推送则放弃分支 |
| M1 | CQ-05 complete | 下一个 patch | 安装本版本或更新版本，再执行 `libra __capture-spool-drain` |
| M2 | CQ-06 complete 且完成判据勾选 | 下一个 patch | revert doctor 输出 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 拷贝中崩溃 | 不完整的 `capture.db` | 重跑拷贝；旧表仍在所以不可发布半棵树 | 把半份目录当成已切换 |
| `rename` 前崩溃 | 已在锁内创建的未过期 `tmp/`，总数不超过 64 | 下一次入队在 `capacity.lock` 内删除修改时间超过 24 小时的 `tmp/`；已有 64 个则拒绝再创建 | 锁外先写文件从而让并发创建超过 64 个 |
| 提交成功、删除 `ready/` 前崩溃 | 已入库的 `ready/` 文件 | 按去重键重放并得到 already-complete | 第二行目录 |
| 消费者持锁时被杀 | `ready/` 仍在，锁已释放 | 下一个消费者继续 | 两个消费者同时写 |
| 队列已满 | 新 hook 失败，旧文件仍在 | 等消费者下降后重试 hook | 把失败说成需要恢复仓库 |
| Pi 硬超时杀进程 | 没有 `ready/` 文件 | hook 失败且无半文件 | 写出未脱敏的 ready 文件 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| 只发布部分调用点 | 高：读写指向不同库，新捕获不可见 | REL-CQ-01 禁止子卡推送 | CQ-04 |
| 删旧表时漏掉未拷贝行 | 高：数据丢失 | 仍有行则拒绝删除并失败 | CQ-07 |
| 重放把工具事件写到不存在的会话 | 中：目录约束失败并进 quarantine | 测试先事件后会话；quarantine 不丢文件 | CQ-05 |
| Pi 2 秒超时 | 中：大信封仍可能入队失败 | 只要求小信封在预算内；失败不留 ready 文件 | CQ-05 |
| 查询落后被当成丢失 | 中：用户误判 | doctor 计数；文档写明入队语义 | CQ-06 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| hook 入队 | 锁内创建 `tmp/`，锁外写入，再锁内 `rename` | 每个事件一个文件 | 50ms / 1 KiB；`ready/`+`quarantine/` 合计 1024 文件或 256 MiB；`tmp/` 最多 64 个，超过 24 小时在同一把锁内删除 | `capture_spool_` |
| 消费者提交 | 一条现有目录事务 | 每个 ready 文件一次 | 事务内不做目录扫描 | `capture_spool_` |
| doctor 计数 | 最多 1024 次 stat | 一次命令 | 不读文件内容 | `capture_spool_doctor_` |
| 首次拷贝 | 每批 ≤ 500 行 | 每个旧仓库一次 | 不把 transcript 一次载入内存 | `capture_db_` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 在 CQ-04 与 CQ-05 更新。
- [ ] `docs/commands/agent.md` 在 CQ-04、CQ-05、CQ-06 更新。
- [ ] `docs/commands/zh-CN/agent.md` 同上。
- [ ] `docs/development/commands/` 仅当已有 hook 入库段落时由 CQ-05 更新对应文件。
- [ ] `docs/error-codes.md` 由 CQ-05 新增三个码；CQ-06 只在 doctor 需要补句时修改。
- [ ] `tests/INDEX.md`：不新增 `--test` target 则 `N/A`。若 ER-02 必须新建 target，同卡登记并更新 `Cargo.toml` 与 `tests/compat/README.md`（仅 compat target）。
- [ ] `Cargo.toml` `[[test]]`：默认 `N/A`。
- [ ] `plan-long.md` 日期索引在本计划成稿时已加一行。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。R7 是计划文本的当前结论。R9 是 CQ-04 实现的当前结论。R10–R13 为 CQ-05，R13 是其当前结论。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 | 本文全文 | FAIL | P0=0。P1 七项：GC-13 清单、删表写集、CQ-03 回滚、CQ-05 谓词、CQ-04 使用 cargo test、rg/D 组不可执行、入队后的退出码未定义 | 无 P2 | Codex exec，只读 sandbox。七项已前滚 |
| R2 | 上述七项 | FAIL | P1 两项未完全关闭：Derived/bootstrap/旧读者用例；spool 重放入口 | 无 P2 | 已前滚 |
| R3 | R2 剩余两项 | FAIL | 卡片已关闭该两项。新 P1：M1 仍写 revert，与 CQ-05 forward-only 冲突 | 无 P2 | M1 已改为 `__capture-spool-drain` |
| R4 | M1 | FAIL | M1 已对齐。新 P1：默认覆盖表仍写 CQ-05 为 revert；quarantine/tmp 没有上限 | 无 P2 | 摘要改为 forward-only；ready 与 quarantine 共用上限，tmp 超过 24 小时删除 |
| R5 | 容量策略 | FAIL | 上限未串行化；tmp 清理只挂在消费者启动上 | 无 P2 | 改为 `capacity.lock` 内计数、发布和删除过期 tmp；tmp 最多 64 个 |
| R6 | tmp 并发创建 | FAIL | `tmp/` 在拿锁前写出，64 个上限仍可被并发撑破 | 无 P2 | 改为锁内 `O_EXCL` 创建，锁外再写内容 |
| R7 | 锁内创建 tmp | PASS | P0=0，P1=0。R6 的并发创建窗口已关闭 | 无 P2 | Codex exec，只读 sandbox，2026-11-16 |
| R8 | CQ-04 实现 | FAIL | P0=0，P1=1：两条 nextest 被勾成通过，与证据不符 | 无 P2 | Codex exec，只读，2026-11-17 |
| R9 | CQ-04 清单修正 | PASS | P0=0，P1=0。nextest 保持未勾选；通过的是 cargo test | 无 P2 | Codex exec，只读，2026-11-17 |
| R10 | CQ-05 实现与 FIX-CQ-01 | FAIL | P0=0。P1 四项：Claude `observe` 吞掉入队失败；拆库后 ref CAS 与目录写不再原子（FIX 余项不能随卡关闭）；重放打开 `capture.db` 时等待可超过期限；状态表写了尚未发生的提交推送 | P2 一项：已存在的队列目录不检查权限 | Codex exec，只读，2026-11-17。四项已前滚：observe 对入队失败（`CaptureEnqueueFailed`）报错；目录写失败时以补偿 CAS 把 ref 撤回原值；打开 `capture.db` 前按剩余期限重算等待；状态表删去推送字样。P2 已改：本用户拥有且对组或他人开放的目录收紧为 0700，他人拥有则拒绝 |
| R11 | R10 修复与新增 FIX | FAIL | P0=0。R10 四项确认关闭；FIX-CQ-02 作为 CQ-06 之前的独立项被接受。新 P1 五项：补偿 CAS 在另一追加者已前进时仍会留下未入目录的祖先；`capacity.lock` 等待不受 hook 期限约束且超时被报成队列满；`begin_write_transaction` 仅凭表结构认定捕获库；非 Unix 平台改名后不刷目录却写“持久入队”；T-1 全量未跑就标完成 | 无 P2 | Codex exec，只读，2026-11-17。前滚：追加在持有 `libra.db` 写锁时提交目录，再提交 ref（取代补偿 CAS）；`publish` 接收 hook 期限，锁等待取期限与 30 秒的较小者，超时报新稳定码 `LBR-AGENT-049`（`capture_spool_lock_wait_ends_at_the_hook_deadline`）；捕获库角色改由文件路径 `<storage>/private/capture.db` 判定，表结构仍须吻合；文档把耐久承诺收窄为 Unix，并写明 Windows 的改名不刷盘；T-1 全量 nextest 已运行，结果见实施证据 |
| R12 | R11 修复 | FAIL | P0=0。R11 五项中锁序、连接解析、`LBR-AGENT-049`、守卫与 oracle 修订被确认。新 P1 四项：重试命中已存在文件时不刷 `ready/` 目录就报成功；第二次取锁超时遗留 tmp 文件；目录写锁的获取不受捕获期限约束；ref 提交失败后留下的“目录有行、ref 无”状态 doctor 不报告 | P2 一项：注释仍写补偿 CAS | Codex exec，只读，2026-11-17。前滚：`AlreadyQueued` 两处都先刷 `ready/`；tmp 创建之后的任何失败都删掉本发布者的 tmp（`capture_spool_second_lock_timeout_removes_its_tmp_file`）；目录事务的取锁用捕获期限 `timeout_at`；提交顺序改为先 ref 后目录，残留窗口落在 doctor 已能自动修复的 `missing_catalog_row`；注释已更正 |
| R13 | R12 修复 | PASS | P0=0，P1=0。R12 四项确认关闭；已核对 doctor class 2（`missing_catalog_row`）能识别并用 `--repair` 重建 ref 可达而目录缺行的检查点，cleanup-pending marker 不会挡住该修复 | P2 一项：验证清单勾选了带 `^` 锚点的原命令，实际证据来自不带锚点的命令。已改为不勾原命令，另列实际执行的命令 | Codex exec，只读，2026-11-17 |
| F1 | FIX-CQ-02 | FAIL | P0=0。代码路由被确认（目录走目录连接，ref 与对象索引留在 `libra.db`，冻结对两个库都做检测，restore 先仓库后目录取锁）。P1：守卫修正后未重跑双环境全量 | P2：状态行写成“已独立提交” | Codex exec，只读，2026-11-17。已重跑全量并改写状态行 |
| F2 | FIX-CQ-02 复核 | PASS | P0=0，P1=0。全量 11841 项全部通过（1 项无关 flaky 重试通过），状态行不再声称已提交 | 无 P2 | Codex exec，只读，2026-11-17 |
| C1 | CQ-06 实现 | PASS | P0=0，P1=0。四项 AC、只 stat 的预算、无路径/内容、文档与 0.30.210 版本面均确认 | P2 一项：1024 上限按普通文件数计，非文件目录项不受限。已改为按访问的目录项计数，并加单测 `capture_spool_doctor_scan_is_bounded_by_entries_visited` | Codex exec，只读，2026-11-17 |
| C2 | C1 之后的提交（`b44367a` 中的有界扫描修复、`e247fab` clippy 前滚、`23b7e0b` 收口文档） | PASS | P0=0，P1=0。按目录项计数的上限、1024 边界、`at_least`/`unavailable` 语义与 `enumerate()` 改写都确认；收口记录与证据一致 | P2 两项：注释与证据写“访问至多 1024 项”，实际会多取一个不 stat 的迭代结果；`plan-long.md` 判据未勾选也未说明。已改注释与证据措辞，并勾选该判据 | Codex exec，只读，2026-11-17。这三处提交在推送前未经复审，违反逐卡“PASS 后再提交推送”的要求，此轮为补审 |
| C3 | C2 的 P2 修正 | FAIL | P0=0。P1：C2 行日期与复审日期不符；“只读出名字”比代码窄（迭代器在该位置也可能返回读取错误） | 无 | Codex exec，只读，2026-11-17。日期核对：本日志统一用 UTC，复审时为 2026-11-17 22:56 UTC（本机时区 UTC+8 已是 11-18），C2 日期保持不变；措辞已改为“再取一个迭代结果（目录项或读取错误）” |
| C4 | C3 修正 | PASS | P0=0，P1=0。注释与证据措辞与代码一致（Rust 侧只改注释）；C2/C3 日期按 UTC 一致 | 无 | Codex exec，只读，2026-11-17 |

## 修复登记

| ID | 来源 | 内容 | 状态 | 关闭条件 |
|---|---|---|---|---|
| FIX-CQ-01 | CQ-02、CQ-03、CQ-07 拆库后的回归。`2c4bab3` 上 CI `compat-offline-core` 166 项失败，主要是 `no such table`：`agent_*` 在 `capture.db`，`reference`/`metadata_kv`/`config_kv`/`workspace_record` 在 `libra.db` | 随 CQ-05 前滚：marker 事务改用独立仓库连接；导出任务与覆盖门在 `capture.db` 上取写锁；`metadata_kv`、仓库身份、工作区租约、HEAD/traces 引用、提交锚定从捕获连接改走配对的 `libra.db`（由连接自身文件路径推得，不回退当前目录；配对结果仍缺 `config_kv` 时视为同一个库，不再无限递归）；`agent clean` 修剪与会话擦除拆成仓库事务加目录事务；GC 从 `capture.db` 读取目录对象根；目录 SQL 里的 `workspace_record` 租约子查询在 `capture.db` 上改为调用方先行校验。Codex R10–R12 后补：拆库后的检查点追加先在 `libra.db` 写事务里做 ref CAS 与对象索引；在仍持有该写锁时打开 `capture.db` 目录事务（取锁受捕获期限约束），写入并授权目录，再对仓库事务做最终授权（期限与工作区租约），然后提交 ref，最后提交目录。所有拆库写者都先取仓库锁再取目录锁，并发追加因此串行；提交之前的任何失败都回滚两边，ref 不动，失败的追加照常交给 cleanup-pending marker。剩下的窗口只有两次提交之间的 I/O 错误或崩溃：ref 已含该检查点而目录行缺失，这正是 doctor 既有的 `missing_catalog_row`（ref 可达、目录无行），`libra agent doctor --repair` 会从 ref 重建该行；只读的 `metadata_kv` 检查改用配对库的 deferred 事务，不再在 marker 注册已持有 `libra.db` 写锁时再抢写锁（这是 doctor 手动重放 2 秒期限耗尽的原因）；doctor 的 Claude/Codex 遗留修订修复写 `capture.db`；不变检查点验证从配对库读 traces ref；Hermes 实时导出的租约在 `capture.db`，信任行从配对库复核；hook 未带期限时重放也不带期限（带期限会省略子代理来源）；测试夹具在装故障触发器前先让 `capture.db` 生成 | 关闭 | 本地 17 个捕获相关 target 中原 13 项余项全部通过；唯一剩下的 `hermes_observe_checkpoint_exports_nonempty_reasoning` 失败在 `cloud sync` 断言，归 FIX-CQ-02 |

| FIX-CQ-02 | CQ-03 拆库后的回归：`libra cloud sync` / `cloud restore` 仍只用 `libra.db`，而 detach 后该库没有 `agent_*` 表。sync 把仓库当作没有捕获目录：既不镜像 agent capture，也不执行本地 `agent_kind` 允许名单拒绝（`hermes_observe_checkpoint_exports_nonempty_reasoning` 的 `cloud sync` 断言因此失败，先报缺云配置）；restore 的目录写入与修剪栅栏同样落在错误的库 | 2026-11-17 20:52:53 UTC 前滚：新增 `cloud::agent_capture::local_capture_catalog`——仍含 `agent_session` 的库（合并库、测试夹具、本版本尚未打开过的仓库）就是它自己的目录，否则经 `capture::store::open` 打开 `capture.db`（缺失时创建，并搬走拆库前的目录）。sync 的本地 `agent_kind` 检查、快照冻结、durability 复查、`agent_capture_cloud_base` 读写、Claude 遗留修订、restore 的 schema 探测、修剪栅栏、导入墓碑与目录写入都改走该连接；traces head 经 `reference_peer` 从 `libra.db` 读取，`object_index` 留在 `libra.db`。快照冻结对 `capture.db` 连接同样做 TEMP 标记与 `data_version` 校验，任一库在冻结期间有提交就重试。restore 先取 `libra.db` 写锁再取 `capture.db` 写锁，先提交 ref 再提交目录（与追加者同序；目录 COMMIT 失败的残留是 doctor 可修复的 `missing_catalog_row`） | 关闭 | `split_catalog_is_frozen_from_the_capture_database` 与 `cloud_restore_writes_a_split_catalog_into_the_capture_database` 覆盖拆库仓库；含 `cloud` 的用例、`agent_cloud_tombstone_test`、`cloud_storage_backup_test` 与该 Hermes 用例共 209 项全部通过 |

已知边角：`agent session list` 与消费者首次创建 `capture.db` 同时发生时，读者可能得到 `LBR-IO-001`。本轮未复现；重启条件是再次复现，承接在 FIX-CQ-02 之后的收口全量。

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-CQ-01 | 捕获库改 WAL | WAL 仍是单写者，解决不了多个 hook 进程 | 测量证明读者阻塞是剩余主因 | 新的日期计划 |
| DEFER-CQ-02 | 查询命令合并 spool 与已提交目录 | 破坏「查询不读队列」的边界，需要另一套读侧一致性 | 产品要求 hook 返回前可见 | 新的日期计划 |
| DEFER-CQ-03 | quarantine 自动修复 | 失败分类还没有稳定枚举 | CQ-05 给 quarantine 文件写了封闭原因码 | 本计划的后续卡，不在 CQ-06 里顺带做 |

## 实施证据汇总

### CQ-01（2026-11-17 04:03:53 UTC）

先 `cargo test --test db_migration_test --test compat_global_config_schema_future --no-run`，再运行这两个产物。`db_migration_test` 里 `capture_db_*` 6 项通过，另有 `role_scope::database_role_manifest_is_total_and_disjoint` 与 `every_migration_sql_file_is_referenced_by_the_registry` 各 1 项通过。`compat_global_config_schema_future` 54 项通过，0 失败。全工作区 `cargo nextest` 会先编译每一个测试产物；那次编译把负载打到 450 后已停止，不把未跑完的 nextest 算通过。`rg` 在 `store.rs` 以外的 `src` 中找不到 `capture_store::`。`cargo +nightly fmt --all --check` exit 0。版本仍是 `0.30.207`。

穷尽匹配同时改了 `src/internal/config.rs` 和 `src/utils/client_storage.rs`。Repository 打开臂的原文未改。`2026111601` 没有进入 Repository 的 `builtin_migrations()`，`libra.db` 里的捕获表也没有删除。

这次执行按用户要求把子卡推到 `main`。REL-CQ-01 原文仍要求子卡留在本地，直到 CQ-04。

### CQ-02（2026-11-17 04:29:59 UTC）

`cargo test --lib capture_store_writer_ --jobs 6 -- --test-threads=1`：1 通过，0 失败。没有跑全工作区 `cargo nextest`，因为那次编译会先构建每一个测试产物。八个捕获文件对 `establish_connection|get_db_conn` 的 `rg` 打印 `OK: zero hits`。`cargo +nightly fmt --all --check` exit 0。版本仍是 `0.30.207`。

`capture_store_writer_hook_catalog_lands_in_capture_database` 先删掉 `libra.db` 里的 `agent_*` 表，再调用生产入口 `ingest_agent_traces`。新会话只出现在 `capture.db`，`libra.db` 的 `schema_versions` 最大值不变，捕获连接的 `pragma_database_list` 为 1。对象路径没有改，仍是 `.libra/objects`。注入单连接的旧单测不打开文件，本卡没有把它们改成这套夹具。

`ingest_agent_traces` 把 hash kind、scope、展示预算和恢复提示留在 `libra.db`，目录和 export-job lease 写入 `capture.db`。`capture.db` 没有 `config_kv`，目录事务用 `schema_versions` 的空更新拿写锁；带 `config_kv` 的连接仍走 `begin_write_transaction`。仓库打开包装在 `src/internal/db.rs`，这样八个文件不必写出被守卫拒绝的函数名。`src/internal/ai/capture/` 里没有别的生产文件直接打开仓库库，所以没有拆卡。命令读路径仍用 `libra.db`，留到 CQ-03。`libra.db` 的捕获表没有删除。

Clippy `--all-targets --all-features -D warnings` 先指出 CQ-01 留下的两处 `collapsible_if`（`store.rs`）和一处 `let_unit_value`（`tests/db_migration_test.rs`）。这两处已在本卡收掉，行为不变。收掉之后同一条 Clippy 命令 exit 0。

### CQ-03（2026-11-17 06:47:53 UTC）

`cargo test --lib capture_store_reader_ --jobs 6 -- --test-threads=1`：2 通过，0 失败。没有跑全工作区 `cargo nextest`，因为那次编译会先构建每一个测试产物。十二个命令和 intent 文件对 `establish_connection|get_db_conn` 的 `rg` 打印 `OK: zero hits`。`cargo +nightly fmt --all --check` exit 0。`cargo clippy --all-targets --all-features -- -D warnings` exit 0。版本仍是 `0.30.207`。

`capture_store_reader_commands_read_capture_database` 先删掉 `libra.db` 里的 `agent_*` 表，再调用生产的 status、session list、checkpoint list、doctor、graph、skill search 和 clean。新行只在 `capture.db`。`load_checkpoint_catalog` 也读到这条只存在于 `capture.db` 的检查点。`libra.db` 的 `schema_versions` 最大值不变，捕获表没有删除。mega2 和 workspace 仍打开 `libra.db`，并且不会创建 `capture.db`。

带 `config_kv` 的连接仍把目录和仓库看成同一个库。分开之后，import 屏障和对象索引修复不再是跨库原子事务，doctor 的子恢复检查在拿住 `metadata_kv` 锁之后另开捕获连接。两个库不 ATTACH。

写集外的 `checkpoint_reader` 是生产目录读者。没有另开任务卡。`capture.db` 存在时经 `capture_store::open_readonly` 读目录行，对象格式仍从 `libra.db` 读取；文件不存在时仍读仓库库里的旧表。另外扩大的文件是 `src/internal/db.rs` 的 peer 帮助函数、`store.rs` 的 `open_current` 与 `open_readonly`、`BridgeContext.repo` 和对应测试字面量、`agent_bridge/workspace.rs`、`commit_recovery` 的双连接签名，以及 `capture_store_reader_test.rs`。

这次执行按用户要求把子卡推到 `main`。REL-CQ-01 原文仍要求子卡留在本地，直到 CQ-04。

### CQ-07（2026-11-17 10:13:06 UTC）

`cargo test --test db_migration_test --jobs 6 capture_ -- --test-threads=1`：8 通过，0 失败。`cargo test --test agent_doctor_repair_test --jobs 6 -- --test-threads=1`：43 通过，0 失败。没有跑卡片上的 `cargo nextest`，也没有跑全工作区 nextest，因为那次编译会先构建每一个测试产物。`rg` 对 `capture_catalog_detach` 在 `src/internal/db/migration.rs` 与 `sql/migrations` 打印 `OK: zero hits`。`cargo +nightly fmt --all` exit 0。`cargo clippy --all-targets --all-features -- -D warnings` exit 0。版本仍是 `0.30.207`。

`store::open` 在回执为空时先拷贝再写回执，然后对真实的 `libra.db` 调用 `detach_copied_catalog`。detach 只比较已经拷贝的主键，不再插入。缺行则回滚，一张表都不删。错误不含行内容。重跑时捕获表已经不在，事务为空并成功，`config_kv` 仍在。`libra.db` 的最大 `schema_versions` 不变。GlobalConfig 与 SystemConfig 的 receipt 不变。两个库不 ATTACH。捕获库仍是 DELETE journal 与 `synchronous=FULL`。

`capture_db_copy_is_reentrant_and_batched` 与 `capture_db_old_reader_schema_unchanged` 现在断言打开之后 `libra.db` 里的 `agent_session` 已经不在，`config_kv` 仍在。CQ-01 的「旧表仍在」勾选保持历史记录，不改回去。

写集外的扩大：`pub mod detach`；`declared_capture_tables` 改为 `pub(crate)`；`open_existing_writer` 只打开已有捕获库，不拷贝、不摘除。检查点父提交经 `repository_peer` 读 HEAD。`metadata_kv` 不在当前事务上时，marker 登记、隔离区查询、恢复头、拒绝对象清理、终态 marker 状态、子代理写锁和 doctor 的 HistoryManager 改走仓库库。目录伴随提交发生在 traces ref 提交之后，经 `open_existing_writer`。合并夹具仍有 `config_kv` 时保持原来的单事务。doctor 测试夹具把 `agent_*` 读到 `capture.db`，marker 与 reference 仍读 `libra.db`。`mutable_state_ownership` 仍按源码 DDL 把这些表记在仓库库，本卡没有改那份清单。

这次执行按用户要求把子卡推到 `main`。REL-CQ-01 原文仍要求子卡留在本地，直到 CQ-04。

### CQ-04（2026-11-17 11:02:54 UTC）

`docs/commands/agent.md` 与 `docs/commands/zh-CN/agent.md` 写明捕获目录只在 `.libra/private/capture.db`。Unix 上父目录 `.libra/private` 为模式 `0700`，数据库文件为模式 `0600`。仓库命令仍用 `.libra/libra.db`，两个库不 ATTACH。比本发布更旧的二进制读不到这份目录。`COMPATIBILITY.md` 的 agent 行记录同一存储位置和旧读者行为。本卡没有改 `src/**` 或 `sql/**`。

版本面从 `0.30.207` 改为 `0.30.208`：`Cargo.toml`、`install.sh` 的 `DEFAULT_VERSION`、`install.ps1` 的 `$DefaultVersion`。`Cargo.lock` 由 `cargo update -p libra --offline` 刷新，只改 libra 自己的版本行。

卡片上的 `cargo nextest` 在链接全部测试产物时负载超过 400，已停止，不把未跑完的 nextest 算通过。验证清单里的两条 nextest 保持未勾选。同一批谓词改由 `cargo test` 执行，并单独勾选。`cargo test --test db_migration_test --jobs 6 capture_ -- --test-threads=1`：8 通过，0 失败。`cargo test --lib --jobs 6 capture_store_ -- --test-threads=1`：3 通过，0 失败。`cargo test --test compat_version_surface_sync --jobs 6 -- --test-threads=1`：2 通过，0 失败。`rg` 对三份文档打印 `OK: docs mention capture.db`。`cargo +nightly fmt --all --check` exit 0。`cargo clippy --all-targets --all-features --jobs 2 -- -D warnings` exit 0。T-2 全量 nextest 留到计划收口，本机没有跑完。

网站 `../libra-backend` 分支 `cf` 的 agent 页写入同一目录位置。推送发生在本仓提交之后。`gh release create v0.30.208` 也在本提交推送到 `main` 之后执行。远端 D 组不计入本卡 AC/VER。这次按用户要求在 `main` 发布，而不是把家族子卡留在未推送分支上。Codex R8 `VERDICT: FAIL`（P0=0，P1=1）：两条 nextest 被勾成通过。改为未勾选后，R9 `VERDICT: PASS`（P0=0，P1=0）。

### CQ-05（2026-11-17 16:55:05 UTC）

实时 AgentTraces 路径（`ingest_agent_traces`）不再打开数据库：先对事件脱敏，把带版本号的信封写入 `.libra/private/capture-spool/ready/`，再尝试拉起一个分离的消费者（`libra __capture-spool-drain`）。消费者持有 `capture-spool-consumer.lock`，抢不到锁立即退出；按文件名顺序重放，目录事务提交后才删文件；库忙、schema 过新、仓库库打不开、文件系统错误时文件留在 `ready/`，其余不能入库的信封移入 `quarantine/`，命令此时非零退出（`LBR-INTERNAL-001`，固定文案）。释放锁后若 `ready/` 仍有文件则再抢一次，避免锁竞争时丢下刚入队的文件。重放使用自己的捕获期限（提供方自管导出窗口的沿用其预算，其余 30 秒）。消费者只继承白名单环境变量，提供方 API key 不传入。入队前只用文件元数据检查 `libra.db` 是否存在，缺失返回 `LBR-REPO-002`，非普通文件返回 `LBR-IO-001`。新增稳定码 `LBR-AGENT-046`（队列已满）、`LBR-AGENT-047`（捕获库忙）、`LBR-AGENT-048`（捕获库 schema 过新）、`LBR-AGENT-049`（队列锁在 hook 的整个等待窗口内被占用；等待取 hook 剩余期限与 30 秒的较小者），已写入 `docs/error-codes.md`。耐久性：信封文件在改名前刷盘，Unix 上再刷 `ready/` 目录；Windows 上改名不刷盘，文档已如实写明。`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`COMPATIBILITY.md` 写明「hook 成功即已入队」。`docs/development/commands/agent.md` 没有 hook 入库段落，未改。

本机验证（`CARGO_PROFILE_DEV_DEBUG=0`，`LIBRA_RUN_CAPTURE_TESTS=1`）：卡片原命令 `cargo nextest run -E 'test(/^capture_spool_/)'` 只匹配集成测试——nextest 的单测名带模块前缀，`^` 锚点匹配不到 `spool.rs`/`drain.rs` 里的单测；不加锚点的 `-E 'test(/capture_spool_/)'` 跑 35 项，35 通过，其中 7 项是既有的 `command::cloud::tests::capture_spool_owner_*`，与本卡无关。`cargo nextest run --test agent_lifecycle_event_test capture_spool_hook_` 包含在上述结果中。`agent_opencode_bridge_test` 的 `capture_spool_opencode_forwarder_returns_after_queueing` 与该 target 其它用例一样是 `#[ignore]` 的操作者本地门，已登记进架构守卫的手动清单：`cargo nextest run --features test-live-agent --run-ignored all --test agent_opencode_bridge_test capture_spool_` 1 通过。其余新测试不设 `LIBRA_RUN_CAPTURE_TESTS` 时打印 `skipped` 后返回；CI 既不设该变量，也不构建 `test-live-agent` target，更不选 ignored 用例，所以都不在 CI 中运行（`command::cloud` 的 7 项不受此门控，原本就在 CI 中）。`rg` 对 `live_pipeline.rs` 打印 `OK: zero hits`。`cargo +nightly fmt --all --check` exit 0；`cargo clippy --all-targets --all-features -- -D warnings` exit 0；`RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --no-deps --all-features` exit 0。17 个捕获相关 target（lib、`command_test` 与 15 个 agent/compat target）最近一次本地 nextest：10630 项，10629 通过，1 失败：`hermes_observe_checkpoint_exports_nonempty_reasoning` 的 `cloud sync` 断言，归 FIX-CQ-02。修复前同一组为 161 项失败。`managed_hook_accepts_regular_file_stdin` 曾因测试起 hook 后立即读目录而失败，测试已改为先等队列入库。T-1 全量（卡片触发 `src/cli.rs` 隐藏入口、稳定错误码与 `docs/error-codes.md`）：`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 在最终代码上跑完：11839 项，11838 通过，1 失败，51 跳过；唯一失败仍是归 FIX-CQ-02 的 `hermes_observe_checkpoint_exports_nonempty_reasoning`（`cloud sync` 断言），三次重试均失败，不是偶发。同一轮全量暴露的 7 项 `compat_agent_architecture_guard` 失败已在本卡内修正，见 ADR-ACF-10 2026-11-17 修订。

写集外的扩大：`src/cli.rs`（隐藏入口与错误映射）、`src/command/hooks.rs`（入队失败的 observe 映射；`spool_replay_provider` 把 hook 名映射到 provider 并注入消费者，capture 模块因此不 import hook 入口层、也不出现 provider 字面量）、`tests/compat/agent_architecture_guard.rs` 与 `docs/development/tracing/agent.md`（ADR-ACF-10 2026-11-17 修订：四项冻结 oracle 的摘要随 `HookRepo` 夹具更新，holder 链与 `LiveCaptureBinding::resolve` 调用点各加消费者侧一处，OpenCode 新用例进手动清单）、`ingress.rs`（重放构造与已脱敏报告）、`observed_agents/live_capture.rs`（重放子源提示）、`observed_agents/hermes_export.rs`（导出租约在 `capture.db`）、`.env.test.example`、`CLAUDE.md`（测试环境变量），以及 FIX-CQ-01 涉及的 `db.rs`、`metadata.rs`、`workspace.rs`、`capture_scope.rs`、`traces.rs`、`history.rs`、`catalog.rs`、`pending.rs`、`pending_identity.rs`、`coverage_gate.rs`、`export_job.rs`、`agent_import.rs`、`subagent_content.rs`、`command/agent/import.rs`、`command/maintenance.rs`、`config_ownership.rs`、`mutable_state_ownership.rs` 和对应测试。没有新 `--test` target，`tests/INDEX.md` 与 `Cargo.toml` `[[test]]` 为 `N/A`。

开发中发现一个只影响测试的泄漏：仓库连接配对曾回退到当前目录，进程内单测从仓库根运行时把一行测试会话（`claude__S-bare`）写进了本开发仓库的 `.libra/private/capture.db`。已去掉该回退（未识别的库文件现在报错），并删掉这一行；备份留在本机草稿目录，没有其它行受影响。

版本面从 `0.30.208` 改为 `0.30.209`：`Cargo.toml`、`install.sh` 的 `DEFAULT_VERSION`、`install.ps1` 的 `$DefaultVersion`；`Cargo.lock` 由 `cargo update -p libra --offline` 刷新。

### FIX-CQ-02（2026-11-17 20:52:53 UTC）

改动只在 `src/command/cloud.rs`、`src/command/cloud/{agent_capture,sync,snapshot,restore}.rs` 与 `tests/compat/agent_architecture_guard.rs`（RG-05 摘要）。用户可见行为不变（`cloud` 文档不提库文件名，未改）；没有新稳定码、新 `--test` target 或版本变化。本地验证：`cargo nextest run -E 'test(/cloud/) | test(=hermes_observe_checkpoint_exports_nonempty_reasoning) | binary(agent_cloud_tombstone_test) | binary(cloud_storage_backup_test)'` 209 项全部通过，其中两项新单测覆盖拆库仓库的冻结与恢复。全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`：11841 项，11840 通过，唯一失败是 `compat_agent_architecture_guard::private_capture_identity_has_no_source_opener` 对 `cloud/agent_capture.rs` 序列化区的 RG-05 摘要固定——本修复改了该区，已在守卫中注明 FIX-CQ-02 范围并更新摘要，该 target 重跑 65/65 通过。CQ-05 起一直失败的 `hermes_observe_checkpoint_exports_nonempty_reasoning` 在此全量中通过。守卫修正后按 Codex 要求重跑同一双环境全量：11841 项全部通过，51 跳过；其中 `command_test::command::init_from_git_test::init_from_git_rejects_blake3_object_format` 首次失败、重试通过（nextest 记为 flaky），与本修复无关。

### CQ-06（2026-11-17 21:50:12 UTC）

`libra agent doctor` 经 `capture::store::open` 打开 `capture.db`；打不开时按 `drain::classify_capture_open` 分类：schema 由更新版本写入报 `LBR-AGENT-048`，库忙报 `LBR-AGENT-047`，文件损坏或不可读报 `LBR-IO-001`，三种文案都点名 `.libra/private/capture.db`，不提仓库库。报告新增 `capture_queue.ready` / `capture_queue.quarantine`（`files`、`bytes`、`at_least`、`unavailable`）与人读行 `Capture queue`；只对目录项 `stat`（`DirEntry::metadata`，不跟随 symlink），每个目录最多 stat 1024 个目录项（任何类型都计入，只统计普通文件），之后最多再从目录迭代器取一个结果（目录项或读取错误，不 stat），用来判断还有剩余并置 `at_least`，不读文件内容，不取消费者锁。`docs/commands/agent.md`、zh-CN 页与 `docs/error-codes.md`（047/048 增加 doctor 触发情形）已更新；`COMPATIBILITY.md` 为 `N/A`（只加诊断字段，兼容矩阵未承诺 doctor 字段集合）。网站 `../libra-backend` 的 agent 页已在本地 `cf` 工作区修改；该仓库的提交与推送被本次会话的自动权限拒绝，留给操作者。

本机验证：`source .env.test && cargo nextest run --test agent_doctor_repair_test capture_spool_doctor_` 3 项全部通过（计数与 JSON 无路径/内容、不等待也不动消费者锁、schema 过新与损坏文案）；`cargo nextest run --lib capture_spool_doctor_` 1 项通过（扫描按目录项计数的上限）；同时跑 `agent_doctor_repair_test` 全部、`compat_version_surface_sync`、`compat_error_codes_doc_sync` 共 50 项全部通过。新测试以 `LIBRA_RUN_CAPTURE_TESTS=1` 门控，不进 GitHub Actions。`cargo +nightly fmt --all --check` exit 0；`cargo clippy --all-targets --all-features -- -D warnings` exit 0；rustdoc 断链检查 exit 0。卡片 Full-suite trigger 为 `none`；计划收口全量随后运行。版本面 `0.30.209` → `0.30.210`（`Cargo.toml`、`install.sh`、`install.ps1`，`Cargo.lock` 经 `cargo update -p libra --offline`）。

### 计划收口（2026-11-17 22:21:33 UTC）

收口门在 `main` 最终代码（含 CQ-06 与其 clippy 前滚修复 `e247fab`）上运行：`cargo +nightly fmt --all --check` exit 0；`cargo clippy --all-targets --all-features -- -D warnings` exit 0；`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 11845 项全部通过，51 跳过，无 flaky。CQ-06 的提交 `b44367a` 曾带着一条 clippy `explicit_counter_loop` 推送（命令链未在 clippy 失败时停止），已由 `e247fab` 前滚修复并在本次收口复验。远端 `main` 的 CI 被其后提交的运行取代（Cancel superseded），以包含本计划全部提交的最新运行为准。

## 完成判据

- [ ] 所有任务卡满足 `G-*`，粒度审计表与卡片一致。
- [x] CQ-01、CQ-02、CQ-03、CQ-07、CQ-04、CQ-05、CQ-06 均为 `Lifecycle=done` 且 `Acceptance=complete`。
- [x] 每张卡的 Verification 已运行并记在「实施证据汇总」。
- [x] 计划收口跑过 fmt、严格 clippy，以及 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，并修复本次全量暴露的本计划缺陷。（11845/11845，见「计划收口」）
- [x] 与本计划无关的既有失败有基线复现证据和 `FIX-*` 或 `DEFER-*`。（收口全量无失败；拆库引入的回归登记为 FIX-CQ-01/02 并已关闭）
- [ ] 命令文档、网站文档、兼容矩阵和错误码按各卡写集完成。（仓库内文档、兼容矩阵与错误码已完成；`../libra-backend` 网站页已在 `cf` 本地修改，提交与推送待操作者执行）
- [ ] 每张卡的 `Rollback mode` 已验证，或记录了不可验证的原因。（CQ-05 为 forward-only，无回滚可验；CQ-06 的 `revert` 只撤 doctor 诊断字段，未单独演练）
- [x] Codex review 最终为 `PASS`，P0/P1 已关闭。（CQ-05 R13、FIX-CQ-02 F2、CQ-06 C1）
- [ ] CQ-04、CQ-05、CQ-06 各自完成 patch、版本面、`gh` 发布与适用 D 组证据。（patch 与版本面完成：0.30.208/209/210；`v0.30.208` 已发布；`v0.30.209`、`v0.30.210` 的 `gh` 发布待操作者确认）
- [x] 修订历史覆盖成稿后的规范性变更。
- [x] 未新建证据文件或证据目录。
- [x] `plan-long.md` 日期索引已有本计划一行。（收口提交 `23b7e0b` 已把该行更新为实施完成）
