# redb 双库存储与捕获帧队列并发访问改进计划（2026-11-21）

> **模板版本：** `v2.16`；继承 [`plan-template.md`](plan-template.md) 的 `GC-01`..`GC-13`、`ER-01`..`ER-14`、`G-01`..`G-11`。
> **状态：** **实施中（按用户 2026-11-22 指令于 `main` 分支逐卡提交推送，不建分支/PR；patch +1；Codex Review 异常授权跳过）**；开工前计划已获 Codex R34 `VERDICT: PASS`（`P0=0, P1=0, P2=0`，session `01a1ffdd-79dc-7591-8f67-44678a2fb712`，受审冻结版 SHA-256 `594acd82f37aadab8da23c1a2806a57c8ee1f45f94a27b27ffaf399e5f4b038e`）。共 154 卡；实时进度以各卡 `Lifecycle / Acceptance`、「实施证据汇总」及 [`plan-status.md`](plan-status.md) 为准。
> **基线快照：** `/Volumes/Data/gitmono/libra` `HEAD = d242b90f978196da443319836b64321c8c716c3d`（`Cargo.toml` 版本 `0.30.237`，核对时间 `2026-11-21 20:47:24 UTC`，`Cargo.toml:15` 当前为 `default = []`，`Cargo.toml:187` 含 dev 依赖 `syn = { version = "2.0", features = ["full", "visit", "visit-mut"] }`）。
> **网站交付路径（遵循 `AGENTS.md:135` 现行指令与 `DEP-RB-03` 交付前硬门）：** 按现行 [`AGENTS.md:135`](../../../AGENTS.md#L135) 长期交付指令，权威网站交付工作区定为 **`$LIBRA_BACKEND = /Volumes/Sea/libra-backend`**（生产分支 `cf`，直接工作于 `cf`、**保留无关未提交改动**、精确暂存本卡目标文件、同步检查与前端构建、独立 Claude Code `VERDICT: PASS`、签名提交推送并核验 Cloudflare 部署）。**现场客观观测与强制前置门（`DEP-RB-03`）：** `2026-11-21 12:36:00 UTC` 本机只读实测 `/Volumes/Sea/libra-backend` 当前尚未挂载/不存在（`No such file or directory`），而同级目录 `/Volumes/Data/gitmono/libra-backend` 存在且位于 `cf`（含 `.libra/` 无 `.git`、`remote.origin.url = git@github.com:libra-tools/libra-backend.git/`、`## cf...origin/cf`）。历史计划记录（如 `plan-20261117-fetch-push.md:10`）与模板默认路径（`plan-template.md:332`）**不得覆盖现行 `AGENTS.md:135`**；因此 `DEP-RB-03` 作为 `RB-30` 与 `RB-33c` 的强制外部先决条件（**不阻塞 Track A `RB-01`..`RB-12` 与 Track B 本地实现 `RB-13`..`RB-29b`**），在进入任何网站交付卡前必须满足二选一，否则**立即 STOP，严禁擅自写入未获授权路径**：① `/Volumes/Sea/libra-backend` 已就绪并通过 `.libra`/`origin`/`cf` 校验；或 ② 交付前由用户在当前会话新作出明确书面裁定批准改用 `/Volumes/Data/gitmono/libra-backend` 并同步修订 `DEP-RB-03` 与 `AGENTS.md:135`。

## 文档职责

本文面向 Libra 的多进程并发访问改进，按「**隔离工作区平行开发、零触碰存量 SQLite 并发与权限代码、原子家族接线**」原则制定完整的 `redb` 双库存储 + Capture 帧队列落地计划：

1. **双轨隔离开发与透明共享边界（`ADR-RB-08`）：**
   - **Track A（独立并行存储核心轨，Phase 0 – Phase 3，`RB-01`..`RB-12`，含全部 `G-09` 拆分子卡共 114 卡）：** 在独立隔离工作区（`../libra-redb-track-a`，分支 `plan/20261121-redb-storage-concurrency`）执行，不要求其它 Agent 的工作区干净。核心库 `RB-01d`..`RB-11o`（110 张连续卡）**100% 独占于全新目录 `src/storage/**`**，与所有业务目录零交集；Track A 仅在 3 个明确登记的串行点触及共享文件（`RB-01` 注册 `Cargo.toml` + `src/lib.rs` 1 行 `pub mod storage;`、`RB-11c` 注册 `Cargo.toml` `[[bin]] libra-migrate` 并在 `RB-11t` 验证 4 标志二进制门控、`RB-12` 追加 `.github/workflows/base.yml` 独立 job 与 `tests/INDEX.md`），且整个 Track A 期间 **编译期常量 `REDB_RUNTIME_UNLOCKED = false` 锁定 `libra-migrate` 实盘切换（`RB-11c`/`RB-11t`/`RB-12`）及后续 `RB-16` 接线后的 CLI 运行时（已发行二进制零环境变量旁路，仅 `--dry-run` 与 `#[cfg(test)]` 单测接缝可用）**，对外不暴露半成品后端。
   - **Track B（`G-08` 原子集成家族 `REL-RB-01` + 双矩阵 + 双后端双二进制发行 + 运行时默认切换家族 `REL-RB-03` + Cargo 默认切换家族 `REL-RB-02`，Phase 4 – Phase 5，`RB-13`..`RB-34c` 共 40 卡）：** 在独立隔离集成工作区（`../libra-redb-integration`，分支 `plan/20261121-redb-integration`）执行。经 `d242b90` 代码实盘清点出的全部 **146 个生产接线文件**（145 个非排除 DB/SeaORM 生产调用文件 + 1 个敏感文件沙箱策略文件 `src/internal/ai/sandbox/policy.rs`）被完整划分为 24 张 `family child` 接线卡（`RB-13`..`RB-28b`，含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`）、2 张后端感知 Hook 队列与守护/诊断卡（`RB-29`、`RB-29d`）、1 张运行时联合解锁卡（`RB-29a`）、1 张解锁态二进制测试与 `base.yml` 原子过渡卡（`RB-29c`）加 1 张 `syn 2.0` 逐调用点 AST 完备审计卡（`RB-29b`），全部 29 张子卡只在本地集成树提交、**窗口内不推送 `main`、不发半成品版本**，直至统一 rebase 最新 `origin/main` 并在唯一家族发布点 **`RB-30`（`Task type = release`）** 原子释放；随后经显式 SQLite + Redb + `keyring` 全量双矩阵 CI（`RB-31`、`RB-31b`）与 `release.yml` **双后端 `libra`（含 `keyring`）+ `libra-migrate`（不含 `keyring`）按各自真实构型协同打包及切前成品互操作只读审计**（`RB-32`、`RB-32b` `no-release`），再经 **`REL-RB-03` 家族**（`RB-33` 切换官方发行默认后端 `src/storage/backend.rs` → `Redb` 并同步仓内文档 + `RB-33b` 精确暂存双成品切后仓库/配置/全新仓活 Spool 双向回退发布前审计 + `RB-33c` 家族原子发布点与网站收口），最后经 **`REL-RB-02` 家族**（`RB-34` 切换 `Cargo.toml` 默认 Feature 至 `["storage-redb", "storage-queue"]` 与独立目标目录双构建/仓库验收 + `RB-34b` 最终默认+`keyring` 二进制全 4 类 Global/System 配置读写拒收审计 + `RB-34c` 家族发布点）原子收口。
2. **严守不修改存量 SQLite 权限与并发代码边界：** 本计划全周期严禁修改 `src/internal/db.rs`、`src/internal/ai/capture/{store,detach}.rs` 与 `src/utils/shared_perm.rs`；既有 SQLite 并发与权限问题全部归入非目标 `DEFER-RB-05`。
3. **目标存储架构：** 可叠加 Cargo Feature（`storage-sqlite`、`storage-redb`、`storage-queue`，其中 `storage-redb` 自动启用 `storage-queue`，确保一切含 `storage-redb` 构建恒具备完整队列交付路径）并行编译共存；Git 核心（`.libra/redb/git.gen-<id>.redb`，61 张活跃持久源表）与 Capture 核心（`.libra/private/capture.gen-<id>.redb`，25 张源表）拆为两个独立 `redb 5.0` `MultiWriter` 文件（均由两阶段原子清单 `.libra/storage-format.json` 指向，配置库按角色独立为 `config.redb` 并经有界 `config-migration.lock` 串行化首写/迁移与原位保留型 `config.db` 路径墓碑阻断旧版二进制重建第二份 SQLite，`.libra/vault.db` 保持 `libvault` 不变）；Hook 写入经 `.libra/private/capture-queue/{priority,normal}/` 双通道双校验 CRC32C 帧日志（16 字节自认证帧头 `HEADER_CRC32C` + 全帧 `FRAME_CRC32C` + 分配前界限护卫 + 共享目录锁原子扣减合并容量 + 全新空通道预建初始 34 字节 `LTM2` `(min_live=0, active_seg=0, committed_len=0, prefix_crc=0, sealed=[])` 双副本 `tail.meta`/`tail.meta.bak` + `0` 段文件严格区分全新空通道与已提交段丢失/元数据损坏 + 每次入队原子替换双副本且先验 `[0, committed_len)` `PREFIX_CRC32C` + 每次轮转原子记录 `(seg_id, sealed_len, sealed_crc32c)` 至 `LTM2.sealed[]` 供消费端强验轮转段边界（拦截精确停在完整帧边界的 `CleanEof` 截断）+ `actual_len < committed_len` 拒收 + 非空双副本同损无条件 fail-closed 零重建零截断 + 全量语义去重键 + 持锁 `sync_data`/dir-fsync）由单写入者按批（≤ 128 条 / ≤ 50ms）合入单次 `redb::WriteTransaction` 并仅随该写事务原子推进双通道库内高水位（消费者在 `writer.lock → capture-queue/append.lock` 下执行 Case C 尾损恢复保留全部积压与已同步完整尾帧且绝不提前推进高水位，回收历史段先原子推进 `MIN_LIVE_SEG` 摘除 `sealed[]` 再 `unlink` + `fsync_dir`）。

本文只规划任务，不宣称实现完成。落地时每张任务卡必须先按 `ER-02` 刷新源码锚点，再按任务卡验收。

### 适用范围

- **Track A（`RB-01`..`RB-12`，含全部 `G-09` 拆分子卡共 114 卡）：** `Cargo.toml`（可叠加 features 含 `storage-redb` 蕴含 `storage-queue`、可选 `redb`/`postcard`/`crc32c` 与 `[[bin]] libra-migrate`）、`src/lib.rs`（1 行模块声明）、独占目录 `src/storage/**`、`src/bin/libra_migrate.rs`、`tests/storage_concurrency_stress_test.rs`、`tests/INDEX.md`、`.github/workflows/base.yml` 独立 `compat-redb-storage` job。
- **Track B（`REL-RB-01` 家族 `RB-13`..`RB-30` 共 30 卡 + `RB-31`/`RB-31b`/`RB-32`/`RB-32b` + `REL-RB-03` 家族 `RB-33`/`RB-33b`/`RB-33c` + `REL-RB-02` 家族 `RB-34`/`RB-34b`/`RB-34c` 共 40 卡）：** 经静态清点核实的全部 **146 个生产接线文件**（完整映射于 `RB-13`..`RB-28b` 含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`）、后端感知队列接入（`RB-29`）与守护/`doctor` 指标（`RB-29d`）、CLI+`libra-migrate` 联合解锁（`RB-29a`）与解锁态测试/`base.yml` 原子切换（`RB-29c`）、`syn 2.0` 逐调用点 AST 完备审计（`RB-29b`）、家族原子发布点（`RB-30`）、全量双矩阵与 `keyring` CI（`RB-31`、`RB-31b`）、`release.yml` 双后端 `libra`（含 `keyring`）+ `libra-migrate`（不含 `keyring`）精确成品协同打包与切前互操作审计（`RB-32`、`RB-32b`）、`REL-RB-03` 运行时默认切红库与双向回退/文档/网站发布家族（`RB-33`/`RB-33b`/`RB-33c`），以及 `REL-RB-02` Cargo 默认 Feature 切换家族（`RB-34`/`RB-34b`/`RB-34c`）。

### 非目标

- **不修复或修改存量 SQLite 并发冲突与权限规则：** 严禁修改 `src/internal/db.rs`、`src/internal/ai/capture/{store,detach}.rs`、`src/utils/shared_perm.rs` 以及既有 SQLite 代码路径下的连接打开方式与锁策略；bare redb 仓库在 `shared_perm.rs:95` 的共享权限扩展同步归入 `DEFER-RB-05`。
- **不替换 `.libra/vault.db`：** 继续由 `libvault 0.4.0`（`storage_sqlite`）独立管理单张 `vault` KV 表（`src/internal/vault.rs:4,40,112`）；`RB-13` 仅适配 `vault.rs:263-280` 经 `ConfigKv` 读写配置元数据的分支。
- **不替换第三方 Agent 自有 SQLite 读取器：** `src/internal/ai/provider_index_io.rs`、`src/internal/ai/observed_agents/codex_index.rs`、`src/internal/ai/observed_agents/builtin/hermes.rs` 读取的是 OpenCode/Codex/Hermes 外部数据库，保持 `sea_orm::sqlx` 只读依赖不变。
- **不改变 Git 对象库（`.libra/objects`）与远程 D1/R2 协议：** `crate::utils::storage::Storage` 与 `src/utils/d1_client.rs` 保持不变。
- **查询命令不直接合并未消费队列帧：** 延续 `plan-20261116` `ADR-CQ-04` / `DEFER-CQ-02`。

### 成功定义

- **Track A 闭环：** 在独立工作区完成 `src/storage/**` 双后端全表实现（61+25+3 源表）、配置域 `config-migration.lock` 并发首写串行化与独立切换清单、跨库持久化意图重放协议、双通道 CRC32C 帧队列、无额外 fd 泄锁的 `VACUUM INTO` 备份与 SQLite 回滚日志原子围栏双向迁移器 `libra-migrate`（预集成期实盘切换 fail-closed 保护）及 32 进程压测，经 `--features storage-sqlite,storage-redb,storage-queue` 等价门与独立 CI `compat-redb-storage` 全绿；默认 CLI 行为与其它 Agent 业务目录零受扰。
- **Track B 闭环：** 146 个生产接线文件 100% 接入 `crate::storage` 并同步解锁 CLI 红库与 `libra-migrate` 实盘切换及 `base.yml` 解锁态 CI，在单一家族发布点 `RB-30` 原子上线；`RB-31`/`RB-31b` 将 `compat-offline-core` 显式锁定为 `--no-default-features --features storage-sqlite` 并新增全量 `compat-redb-queue`、`keyring` 与双后端发行构型验证；`RB-32`/`RB-32b`（`RB-32b` 为 `no-release` 审计）随 `release.yml` 按各自真实特性协同交付双后端 `libra`（含 `keyring`）与 `libra-migrate`（不含 `keyring`）并对精确暂存+SHA-256 校验后的成品实测切前互操作；`REL-RB-03` 家族（`RB-33`/`RB-33b`/`RB-33c`）切换官方发行默认后端至 `Redb`、以精确暂存双成品验证切后仓库/配置/全新仓活 Spool 双向 SQLite 回退并同步文档与网站后原子发布；`REL-RB-02` 家族（`RB-34`/`RB-34b`/`RB-34c`）切换 `Cargo.toml` 默认 Feature 至 `storage-redb,storage-queue`，并在最终默认+`keyring` 二进制上完整实测纯默认/纯 SQLite 回退构建、红库初始化、未迁移仓库及全部 4 类 Global/System 配置（`get/set --global` 与 `get/set --system`）fail-closed 拒收后原子发布。

## 使用规则

- 每次状态推进、快照更新与「修订历史」新增行都必须记录真实 UTC 时间（`date -u '+%Y-%m-%d %H:%M:%S UTC'`），不得预填或写未来时间（`v2.11`）。
- 任一任务卡推进或完成时，在同一变更中同步更新 [`plan-status.md`](plan-status.md)「计划一览」行（由 `ER-12` 单一发布者串行落盘）。
- 严禁在 `docs/development/plan/` 或仓库其它目录新建 `evidence/`、`baseline/`、`scratch/` 等外部证据文件或目录（`v2.12`）；所有测试摘要、SHA-256 与 CI run ID 统一写入本文「实施证据汇总」。
- 所有版本控制操作使用系统 `PATH` 中的 `libra`（禁止 `git` 命令，禁止 `libra commit -a`）。

## 事实基线

> 以下锚点于 `2026-11-21 10:34:12 UTC` 在 `/Volumes/Data/gitmono/libra`（`0.30.237`，`d242b90f`）现场只读核对；每卡开工前按 `ER-02` 重核行号。

| 类别 | 当前事实（已核实基线 `d242b90`） | 证据 |
|---|---|---|
| 构建与依赖 | `Cargo.toml:15` 当前为 `default = []`，尚无 `storage-*` feature；`sea-orm = "2.0.0"`（`sqlx-sqlite`，内嵌 `libsqlite3-sys 0.30.1` 支持 `VACUUM INTO` 与 `PRAGMA writable_schema`）、`serde = "1.0.228"`、`crc32fast = "1.5.0"`（IEEE CRC-32，**非 CRC32C**）为常驻依赖，`libvault = "0.4.0"` 启 `storage_sqlite`；`release.yml` 以 `--features keyring` 构建并向 `build/` 暂存单文件平台二进制与 `.sha256`/`.size`（非 tar 归档） | `Cargo.toml:15-27,66-72,103,132`；`Cargo.lock:6552`；`.github/workflows/base.yml:60,63,95`；`.github/workflows/release.yml:99-151` |
| 目录现状 | `src/storage/` 不存在；**`src/bin/` 当前不存在**（将于 `RB-11c` 新建）；`crate::utils::storage::Storage` 为 Git 对象存储 trait | `src/lib.rs:20-27`；`src/utils/storage/mod.rs:65` |
| 数据库角色与账本 | `DatabaseRole` 含 `Repository`（`.libra/libra.db`）、`Capture`（`.libra/private/capture.db`）、`GlobalConfig`（`<config_dir>/libra/config.db`，`configuration_schema_versions`）、`SystemConfig`（`/etc/libra/config.db`）、`Derived` | `src/internal/db/schema.rs:23-116,239-256` |
| 源表精确盘点（可复现） | ① **Repository 活跃持久源表共 61 张**（`4 + 7 + 50 = 61`，含 `agent_bridge/publication.rs:22,36` 在仓库事务内按需建表的 `agent_bridge_publication` 与 `agent_bridge_index_publication`；另有 7 张已退役删表不计入）；② **Capture 源表共 25 张**（`2026111601_capture_database.sql` 的 25 条 `CREATE TABLE` = 2 张账本/收据 + 23 张 `agent_*` 业务表）；③ **Config 源表共 3 张**（`config`、`config_kv`、`configuration_schema_versions`，另含可选屏障 `schema_versions`） | `src/internal/mutable_state_ownership.rs:45-503`（`L290` 登记 `agent_bridge_publication`）；`src/internal/ai/agent_bridge/publication.rs:22,36`；`sql/migrations/2026111601_capture_database.sql:7-453`；`src/internal/db/schema.rs:69-115` |
| 生产调用面精确盘点 | `src/` 除 `internal/db*`、`internal/model/*` 与测试文件外，共 **151 个**生产文件直接引用 `sea_orm` / `internal::db` / `internal::model` / `capture::store`：扣除 3 个第三方只读 SQLite 解析器与 3 个本计划禁改文件（`capture/{store,detach}.rs`、`shared_perm.rs`）得 **145 个 DB 调用文件**（含 `gui/data/{repo,state}.rs` 与 `agent_bridge/publication.rs`），再加 1 个 `.libra/` 敏感路径沙箱策略文件 `src/internal/ai/sandbox/policy.rs:661-702`，合计 **146 个生产接线文件** | 2026-11-21 AST 剥离 `#[cfg(test)]` 扫描实证；`src/internal/head.rs:171,324`；`src/internal/ai/capture/live_checkpoint.rs:147`；`src/gui/data/repo.rs:47`；`src/internal/ai/sandbox/policy.rs:661-702`；`src/utils/util.rs:213-224,475-494,808-835` |
| 锁、发现与 Spool 事实 | `Commands::Agent(_)` 不获取 CLI 级 `MaintenanceLock::shared`；`capture/spool.rs:111-145` 在创建 `tmp/` 文件与 rename 进 `ready/` 之间会释放 `capacity.lock`；`capture::store::open` 支持从旧 `libra.db` 拷贝捕获表；`util.rs` 与 `clone.rs:786` 通过 `libra.db` 判定有效仓库目录 | `src/cli.rs:2535-2585`；`src/internal/ai/capture/spool.rs:111-157`；`src/internal/ai/capture/store.rs:51-113`；`src/utils/util.rs:221,491,832`；`src/command/clone.rs:786` |
| 外部 crate 参照 | `redb v5.0.0`（`ConcurrencyMode::{ExclusiveWriter, SingleWriter, MultiWriter}`、`open_read_only`、`try_begin_write`、`Builder::new()` 默认 `ExclusiveWriter` + `1 GiB` 缓存、`DatabaseError::RepairAborted`）；`crc32c v0.6.9`（Castagnoli CRC32C，MIT/Apache-2.0） | `redb-5.0.0/src/db.rs:1897-1923,2253-2264,2321-2333,2454-2460`；`redb-5.0.0/src/error.rs:460-474`；crates.io `crc32c 0.6.9`（2026-11-21 核对） |
| 网站检出现状 | `AGENTS.md:135` 指定权威网站交付路径为 `/Volumes/Sea/libra-backend`（`cf` 分支）；2026-11-21 12:36:00 UTC 本机现场只读实测 `/Volumes/Sea/libra-backend` 当前不存在，同级目录 `/Volumes/Data/gitmono/libra-backend` 存在且位于 `cf`（含 `.libra` 无 `.git`、`remote.origin.url = git@github.com:libra-tools/libra-backend.git/`、`## cf...origin/cf`、`apps/tanstack-app/content/docs/commands/{agent,init}.en.md`、`scripts/sync-libra-command-docs.py`）；交付前须通过 `DEP-RB-03` 确认 `/Volumes/Sea/libra-backend` 就绪或先取得新的明确用户书面裁定后再执行 `RB-30`/`RB-33c` | `AGENTS.md:135`；2026-11-21 12:36:00 UTC 现场只读实测；`DEP-RB-03` |

#### 可复现源表与 redb 内部表清单

- **A. Repository 活跃持久源表（61 张，迁入 `.libra/redb/git.gen-<id>.redb`）：**
  - `ConfigStore`/`MetadataStore` 域（4 张，归 `RB-03`）：`schema_versions`、`config`、`config_kv`、`metadata_kv`。
  - `GitStore` 核心域（7 张，归 `RB-04`）：`reference`、`reflog`、`notes`、`object_index`、`object_obliteration`、`revision_ordinal`、`revision_ordinal_meta`。
  - `GitStore` 状态与投影域（50 张，归 `RB-05`）：`sequence_state`、`rebase_state`、`bisect_state`、`working_dirty`、`working_dirty_meta`、`layer`、`layer_path`、`sparse_view`、`sparse_view_meta`、`worktree_registry_capability`、`worktree_lifecycle`、`worktree_intent_journal`、`workspace_record`、`operation`、`operation_parent`、`operation_head`、`operation_journal`、`change_identity`、`change_revision`、`change_predecessor`、`ai_operation_link`、`ai_thread`、`ai_thread_participant`、`ai_thread_intent`、`ai_thread_provider_metadata`、`ai_scheduler_state`、`ai_scheduler_plan_head`、`ai_scheduler_selected_plan`、`ai_live_context_window`、`ai_index_intent_plan`、`ai_index_intent_task`、`ai_index_plan_step_task`、`ai_index_task_run`、`ai_index_run_event`、`ai_index_run_patchset`、`ai_index_intent_context_frame`、`ai_validation_report`、`ai_risk_score_breakdown`、`ai_decision_proposal`、`ai_final_decision`、`automation_log`、`approved_permission`、`source_call_log`、`memory_episode`、`memory_episode_evidence`、`memory_projection_state`、`memory_episode_path`、`memory_episode_search_doc`、`agent_bridge_publication`、`agent_bridge_index_publication`。（校验：`4 + 7 + 50 = 61`。注：`MUTABLE_STATE_OWNERSHIP` 另有 7 张已退役删表 `legacy_operation*` 5 张 + `cherry_pick_state` + `revert_sequence`，不在现行活库中，不计入 61 张活表；旧库若残留由迁移器跳过登记。）
- **B. Capture 源表（25 张，自 `sql/migrations/2026111601_capture_database.sql` 迁入 `.libra/private/capture.gen-<id>.redb`，归 `RB-06`）：**
  - 源账本与拷贝收据（2 张）：`schema_versions`、`capture_catalog_copy_receipt`。
  - 23 张 `agent_*` 业务目录表：`agent_audit_log`、`agent_bridge_checkpoint`、`agent_bridge_event`、`agent_bridge_link`、`agent_bridge_operation`、`agent_bridge_session`、`agent_capture_cloud_base`、`agent_capture_incarnation`、`agent_checkpoint`、`agent_checkpoint_prune_tombstone`、`agent_coverage_claim`、`agent_coverage_conflict`、`agent_coverage_revision`、`agent_export_job`、`agent_import_identity`、`agent_import_tombstone`、`agent_session`、`agent_session_commit`、`agent_subagent_content_claim`、`agent_subagent_content_revision`、`agent_subagent_link`、`agent_usage_stats`、`agent_workspace_scope_audit`。
- **C. GlobalConfig / SystemConfig 源表（3 张 + 可选屏障，归 `RB-03`）：** `config`、`config_kv`、`configuration_schema_versions`（及写屏障 `schema_versions`）。
- **D. redb 新增内部元数据与二级索引表（独立于源表计数）：**
  - 每库内置：`META_SCHEMA`（含迁移围栏 `min_reader_version == u32::MAX`）、`META_GENERATION`（跨库严格单调代际号 `cross_store_gen: u64`）。
  - `git.redb` 跨库意图发件箱：`META_CROSS_STORE_INTENT`（持久化未对账跨库意图，供重放修复与防止静默掩盖缺口）。
  - `capture.redb` 队列高水位表：`QUEUE_HIGHWATER_PRIORITY`、`QUEUE_HIGHWATER_NORMAL`。
  - 同事务二级索引表：`REFS_BY_WORKTREE`、`OBJECT_BY_PACK`、`SESSIONS_BY_TIME`、`CHECKPOINTS_BY_SESSION`、`EVENTS_BY_TIME`、`BRIDGE_EVENTS_BY_SEQ`、`MEMORY_BY_PATH` 等。

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-RB-01 | 无统一 `src/storage` 抽象与可叠加存储 Feature，146 个生产文件耦合 SQLite/SeaORM 或硬编码 `libra.db` | 无法并行演进新引擎且易与其他功能 Agent 产生写集冲突 | `Cargo.toml:15-27`；146 文件清单 | `RB-01`..`RB-06e`（含 `RB-01d`..`RB-02e`/`RB-03j`/`RB-03b`..`RB-03i`/`RB-04b`/`RB-05b`/`RB-06c`..`RB-06e`）、`RB-13`..`RB-28b`（含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`） |
| GAP-RB-02 | SQLite `DELETE` 模式读写互斥，缺少跨进程 MVCC 无锁读与多写者双库引擎 | 并发 Git 命令、Hook、`agent bridge`、桌面窗口与只读查询争锁阻塞 | `src/internal/db.rs:1327`；`src/gui/data/repo.rs:47` | `RB-02`、`RB-02d`、`RB-02b`、`RB-02c`、`RB-02e`、`RB-04`..`RB-06`、`RB-13`..`RB-30` |
| GAP-RB-03 | `capture-spool` 单事件多锁/目录遍历/单事务开销高，缺乏跨通道容量守恒的双校验 CRC32C 帧日志与单写入者批量提交 | 高频 Hook 下 I/O 与派生开销大 | `src/internal/ai/capture/spool.rs:111-157` | `RB-07`、`RB-07g`、`RB-07c`、`RB-07f`、`RB-07d`、`RB-07b`、`RB-07e`、`RB-08`、`RB-08t`、`RB-08d`、`RB-08u`、`RB-08f`、`RB-08b`、`RB-08g`、`RB-08m`、`RB-08n`、`RB-08i`、`RB-08q`、`RB-08k`、`RB-08v`、`RB-08j`、`RB-08r`、`RB-08l`、`RB-08o`、`RB-08s`、`RB-08c`、`RB-08y`、`RB-08z`、`RB-08e`、`RB-08h`、`RB-08x`、`RB-09`、`RB-09b`、`RB-09d`、`RB-09c`、`RB-09e`、`RB-09f`、`RB-09g`、`RB-09h`、`RB-09j`、`RB-09k`、`RB-09i`、`RB-09l`、`RB-29`、`RB-29d` |
| GAP-RB-04 | 缺少支持三类旧库布局、全域静默围栏、防已打开句柄分叉、两阶段原子清单与对称双向无损回退的迁移器 | 存量仓库无法安全迁移或回退，双库切分存在中间态与分叉风险 | `src/cli.rs:2535-2585`；`src/internal/ai/capture/spool.rs:111-157`；`src/internal/ai/capture/store.rs:51-113`；`src/internal/ai/capture/drain.rs:85-143` | `RB-10`..`RB-10n`、`RB-11`..`RB-11z`、`RB-11c`、`RB-11t` |
| GAP-RB-05 | 缺少跨库持久化意图重放与云快照一致校验、双后端全量 CI（含切换后 SQLite 显式回归与 `keyring` 组合）与门控发布 | 存在半接线暴露、跨库半提交缺口与切换后 SQLite CI 静默失效风险 | `.github/workflows/base.yml:95`；`.github/workflows/release.yml:99` | `RB-06b`、`RB-06e`、`RB-12`、`RB-28`、`RB-28b`、`RB-30`、`RB-31`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c`、`RB-34`、`RB-34b`、`RB-34c` |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| 全部在途 `plan-20261117-*.md`、`plan-20261120-ui-real-data.md`、`LVC-02`/`LVC-06`（`agent_bridge`）与 `issues/*.md` | 其它 Agent 并发修改 `src/cli.rs`、`src/command/**`、`src/gui/data/**`、`src/internal/**`、`src/utils/**`、`COMPATIBILITY.md`、`docs/commands/**` | Track A 在独立工作区执行，`RB-01d`..`RB-11o`（110 张连续卡）严格限于 `src/storage/**` 零业务冲突，仅 `RB-01`/`RB-11c`/`RB-12` 按 `DEP-RB-02` 短暂串行触及共享文件（`RB-11t` 仅触及独立 `src/bin/libra_migrate.rs`）；Track B 全部作为 `REL-RB-01` 家族子卡在隔离集成分支本地演进、不推送 `main`，仅在 `RB-30` 合入前经协调者单一冻结窗 rebase 汇流 |
| [`plan-20261116-capture-queue.md`](plan-20261116-capture-queue.md) | 已落地 `.libra/private/capture.db`、`.libra/private/capture-spool/`、`FIX-CQ-01`、`FIX-CQ-02` | **不改动其 SQLite 实现与 `store.rs`/`detach.rs`**；新 `redb` 侧沿用 `.libra/private/` 约定、脱敏前置、`SessionEnd` 终态优先（升级为独立 `priority/` 通道）与 `GitStore → CaptureStore` 提交序 |
| [`plan-20260910.md`](plan-20260910.md) | `GC-13` 数据库角色隔离契约 | 全 5 类角色在 `redb` 各自 `META_SCHEMA` 独立登记，`GlobalConfig` / `SystemConfig` 具备作用域有界协调锁、原位保留型路径墓碑、`config-format.json`、显式角色迁移与读回退/fail-closed 保护 |
| [`plan-20261028-status-performance.md`](plan-20261028-status-performance.md) | `status` 性能基线 | `RedbGitStore` 只读走 `open_read_only` 无锁快照 + `16 MiB` 缓存上限 |
| [`plan-long.md`](plan-long.md) | 关联 `LR-01`、`LR-02`、`AG-ATTR`、`MEM-01`/`MEM-06`、`SB-01`..`SB-04` | 收口时登记日期计划索引，不改长期能力编号 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | `redb` MVCC + 跨库持久化意图重放 + `storage-redb` 自动蕴含 `storage-queue` + 16B 自认证帧头、`LTM2` 34B+16B×N 双副本（含 `MIN_LIVE_SEG`/`PREFIX_CRC32C`/`sealed[]` 轮转段认证边界、两步 Case C 与 Case D 封存前恢复旧段完整尾、`MAX_SEALED_SEGMENTS = 256` 预变更背压与原地恢复协议、0 段文件四分法、非空双副本同损无条件 fail-closed）+ `writer.lock → append.lock` 锁序协调与完备高水位推进协议（活跃段停 `(S, batch_end_offset)` 绝不跳段、已封存段末批达 `sealed_len(S)` 推至 `(S+1, 0)`、已排空后轮转的封存段先认证 `LTM2.sealed[S]` 再执行原子零帧推进 `append_capture_batch(&[], (S+1, 0))`）+ SQLite 回滚日志原子围栏与零额外 fd 泄锁备份双向迁移闭环解决并发与升级安全 | 全部关闭 R1–R33 全部 P0/P1/P2 发现 |
| 可行性 | 146 个生产接线文件 100% 实盘清点并均分至 24 张 `family child` 接线卡（每卡 ≤ 12 文件、≤ 4 目录） | 采用 `G-08` 家族 `REL-RB-01`、`REL-RB-03` 与 `REL-RB-02` 原子发布，消除半接线暴露与未审默认切换 |
| 任务卡粒度 | 154 张卡均同时满足 `G-02`（每张 `implementation`/`migration` 卡交付唯一不重叠的独立生产行为与同卡完整验证，零纯测试实现卡）与 `G-03` 独立 pass/fail 谓词上限（`implementation`/`migration` ≤ 8、`release` ≤ 12、`audit` ≤ 20，零 waiver），`landing ≤ 4`、`prod-files ≤ 12`；且 `RB-32b` `audit` 严格对齐 `G-07` `no-release` | 按 `G-09` 累计从 R1 基线 32 卡演化至 154 卡（R31→R32 将 `RB-10m` 拆净为 `RB-10m` 6/8 全三类 WAL 竞争含活跃写者与 `RB-10n` 8/8、`RB-11z` 8/8（及 `RB-11q` 6/8）逐域独立快照零改动），全部 154 卡严格满足 `G-02`/`G-03` |
| 依赖与顺序 | Track A（114 卡，所有 `RB-08*` 纯生产者/段层实现与验证在前、所有 `RB-09*` 消费端实现与验证在 `RB-09`/`RB-09d` 之后严格可执行，且各 Phase 进入/退出条件与 DAG 精确对齐）→ Track B `REL-RB-01` 家族（`RB-13`..`RB-30` 共 30 卡）→ 双矩阵与 `keyring` CI（`RB-31`→`RB-31b`）→ 双后端成品打包与切前 `no-release` 审计（`RB-32`→`RB-32b`）→ `REL-RB-03` 运行时默认切红库家族（`RB-33`→`RB-33b`→`RB-33c` 共 3 卡）→ `REL-RB-02` Cargo 默认 Feature 切红库家族（`RB-34`→`RB-34b`→`RB-34c` 共 3 卡） | 写集交集严格串行，154 条内部依赖边无环且逐卡可执行闭合 |
| 完整性 | 覆盖 61+25+3 源表、三类旧库、WAL 静默与零额外 fd `VACUUM INTO` 锁安全备份、SQLite 回滚日志原子 `__libra_migrated_fence__` 围栏、双源双向 `unlink` 后持久化恢复、反向全程持 `capture-spool-consumer.lock` 排斥并发 `drain` 与 `quarantine/` 非空 fail-closed 保护、反向正常与恢复统一采用非覆写 `hard_link` 先发 `capture.db`（覆盖 `Ok`/`NotFound`/`AlreadyExists`/`Unsupported` 全分支与无崩溃竞争并轨实测）、16B 自认证 `HEADER_CRC32C + FRAME_CRC32C` 帧头与 `LTM2` 双副本 `tail.meta`/`tail.meta.bak`（0 段四分法防唯一活跃段缺失被重置为空通道、Case A/E/C/D、Subcase B1 单副本恢复、Subcase B2 非空双副本同损无条件 fail-closed、`LTM2.sealed[]` 认证全部未回收轮转段长度与 CRC32C 拦截完整帧边界截断、`writer.lock → append.lock` 消费者 Case C 恢复保留积压与已同步完整尾帧且绝不提前推进高水位，回收先更新 `LTM2` 再 `unlink`）、`storage-redb` 自动启用 `storage-queue` 的纯 `--features storage-redb` 捕获 E2E、全新 Redb 仓 spool 墓碑/活事件对账、配置域非覆写 `hard_link` 墓碑预占与 `init_phase="fresh_create"` 全边界恢复、独立 `--target-dir` 防覆盖的最终默认+`keyring` 二进制仓库及全 4 类 Global/System 读写拒收审计（`RB-34`/`RB-34b`/`RB-34c`），以及 `RB-12`/`RB-29c` 精确作用域 `compat-offline-core` `diff -u` 零修改守卫 | 全部 `--test` 目标逐项核对 `Cargo.toml`；全部零命中守卫遵循 `plan-template.md:591` `rc=$?; test "$rc" -eq 1` 模板；网站交付路径严格对齐现行 `AGENTS.md:135`（`/Volumes/Sea/libra-backend`）并以 `DEP-RB-03` 门控 `RB-30`/`RB-33c` |
| 安全性 | `.libra/redb/` 独立目录托管全部当前与未来代际 `git.gen-<id>.redb` 并由 `sandbox/policy.rs` 整目录钉死拒读；`O_NOFOLLOW` + `0700`/`0600`；`RepairAborted` 只读绝不升写；分配前帧长界限校验 + `HEADER_CRC32C` + `LTM2` `PREFIX_CRC32C`/`sealed[]` 认证边界防超大分配、静默损坏/缺失/完整帧边界截断穿透与误隔离；暂存测试绝不向固定 `/tmp` 路径执行 `rm -rf` | 彻底闭合 R2–R33 全部安全与完整性发现 |
| 功能正确性 | `storage-redb` 自动启用 `storage-queue` 消除无队列红库死角；16B 自认证帧头 `HEADER_CRC32C` + 全帧 `FRAME_CRC32C` + `LTM2` 双副本 `tail.meta`/`tail.meta.bak` 保证 0 段文件仅于双元数据皆缺时初始化空通道、正常入队与轮转段消费均强验已确认边界（拦截精确停在完整帧边界的 `CleanEof` 截断）、消费者尾损恢复保留全部积压与已同步完整尾帧且仅随 DB 提交原子推进高水位；共享目录锁 `capture-queue/append.lock` 原子校验 `priority + normal + quarantine + incoming_frame <= 256 MiB`；独立 `--target-dir` + SHA-256 校验确保最终默认+`keyring` 二进制绝不被纯 SQLite 构建覆写并端到端验证 `config get/set --global` 与 `config get/set --system` 全 4 路径拒收；`RB-12`/`RB-29c` 精确作用域 `diff -u` 校验 `compat-offline-core` 零修改 | 彻底闭合 R2–R33 全部 P1/P2（含 R33 `RB-10n`/`RB-11z` 8/8 与 `RB-11q` 6/8 逐域 1:1 独立快照断言拆净） |
| 接口兼容 | `ADR-RB-08` 保留 `*_with_conn` SeaORM 签名供 SQLite 与其它 Agent 消费，并行提供 `*_with_store` 给 `Redb` | 消除单签名覆写 `ConnectionTrait` 矛盾 |
| 数据流与控制流 | 正向/反向迁移共用对称两阶段围栏 + 锁安全不可变 Phase 1 备份 + 跨库 `META_CROSS_STORE_INTENT` 强制先重放再递增代际 | 彻底闭合 R2–R14 迁移一致性、队列双副本/锁序与恢复路径 |
| 性能与容量 | 缓存封顶 `16/32/64 MiB`、每批 128 条单事务、分段 `16 MiB` 轮转与 `256 MiB` 合并硬上限 | 见性能与容量摘要 |
| 可靠性与容错 | 全链路崩溃点与无崩溃竞态（含正常入队前已确认前缀位翻转、段短于 `committed_len`、单副本损毁与0 段文件含非零/损坏元数据（唯一活跃段缺失）、非空双副本同损（含完整帧边界截断、活跃段 0 字节截断及较旧段损毁）、未回收轮转段完整帧边界截断、消费者与暂停活跃生产者并发竞争及积压+已同步完整尾帧+崩溃半帧恢复、断电撕裂扇区热回滚日志、双源双向 `unlink`+`fsync_dir` 后、反向正常 Step 2.4 无崩溃竞争 `capture::store::open`、反向并发 `drain` 锁排除与 `quarantine/` 保护、配置与仓库反向 `targets_staged_for_rename` 旧二进制写入、两类可达 Step 2.6 解封中途、已打开句柄与跨库半提交）逐项枚举并配实测用例 | 见故障恢复矩阵 |
| 可维护性 | 两阶段严格写集守卫（实现阶段仅允许本卡精确 `Implementation write set` 并展开 `--untracked-files=all`，发布阶段由 `ER-12` 单独核验版本面） | 彻底闭合 R2 P1 #9 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-21 08:06:39 UTC | 自审成稿 | 初稿 16 卡 | 无 → `RB-01`..`RB-16` | 全文 |
| 2026-11-21 09:26:07 UTC | 用户指令修订 | 移除存量 SQLite 权限/并发止血改动，拆为 Track A / Track B 双轨 | 移除原 `RB-02` | 全文 |
| 2026-11-21 10:17:32 UTC | Codex R1 FAIL（20×P1、3×P2，SHA-256 `48bcc0220edd`）+ 拉齐 `origin/main = d242b90` | 闭环 R1 全部 23 项并刷新至 `d242b90`（61+25+3 源表、146 接线文件、32 卡） | 16 卡 → `RB-01`..`RB-32` | 全文 |
| 2026-11-21 10:34:12 UTC | Codex R2 FAIL（10×P1、2×P2，SHA-256 `c9dc39277fc1`） | 闭环 R2 全部 12 项发现（正向/反向两阶段围栏、`RB-06b` 跨库意图发件箱、共享队列锁、预集成门控、4 处 `compat_*` 测试名、`RB-01` 负向编译门、G-03 拆卡与两阶段写集守卫、`.libra/redb/` 沙箱目录、`RB-33` 纯红库 E2E） | 32 卡 → 37 卡（新增 `RB-02b`/`RB-06b`/`RB-10b`/`RB-11b`/`RB-33`） | 全文 |
| 2026-11-21 10:51:01 UTC | Codex R3 FAIL（5×P1、0×P2，SHA-256 `908e5f53eb7d`） | 闭环 R3 全部 5×P1（`source_digests` 摘要续跑、反向持锁单趟直灌未对账意图、`RB-29b` `syn 2.0` 逐调用点 AST 守卫、编译期常量 `REDB_RUNTIME_UNLOCKED` 去旁路、22B CRC32C 原子替换 `tail.meta` 与 `RB-08b`） | 37 卡 → 39 卡（新增 `RB-08b`、`RB-29b`） | 全文 |
| 2026-11-21 11:01:32 UTC | Codex R4 FAIL（7×P1、0×P2，SHA-256 `8cfd4a619b49`） | 全面闭环 R4 全部 7 项 P1（清单前置优先于 `LIBRA_STORAGE_BACKEND`、轮转孤儿后继段扫描、正向 `source_digests` 纳入 `queue_manifest_sha256`、`syn 2.0` 全树调用方可达性守卫、锁定/解锁二进制测试拆分、正向 Phase 2 旧句柄阻断与外部网站写集显式列出） | 39 卡不变 | 全文 |
| 2026-11-21 11:20:19 UTC | Codex R5 FAIL（4×P1、0×P2，SHA-256 `6cc941bbc64d`） | 闭环 R5 4 项 P1（SQLite DDL 围栏、WAL 静默与离线检查点备份、配置按运行时后端分派、`RB-29a` 解锁态 CI 原子切换） | 39 卡 → 40 卡（新增 `RB-29a`） | 全文 |
| 2026-11-21 11:36:41 UTC | Codex R6 FAIL（7×P1、0×P2，SHA-256 `808d6b89dfeb`） | 闭环 R6 7 项 P1（引入不移锁原地 spool/queue 墓碑、配置域独立 `config-format.json`、双后端官方发行构型、`RB-01` 模块边界收窄与首次 `G-03` 拆分至 45 卡） | 40 卡 → 45 卡（新增 `RB-03b`/`RB-10a`/`RB-10c`/`RB-11a`/`RB-11c`） | 全文 |
| 2026-11-21 12:04:09 UTC | Codex R7 FAIL（7×P1、0×P2，SHA-256 `d9ec066e91cd`） | 全面闭环 R7 全部 7 项 P1（100% SQLite 回滚日志原子围栏、单一非池化 `c_mig` + `VACUUM INTO` 零额外 fd 泄锁备份、每源 `phase2_progress` post-unlink 区分、终态后 Step 2.6 解封 Spool、配置域首写并发串行化、拆至 51 卡、`RB-32`/`RB-33` 暂存双特性成品实测） | 45 卡 → 51 卡（新增 `RB-01b`/`RB-02c`/`RB-03c`/`RB-10d`/`RB-11d`/`RB-34`） | 全文 |
| 2026-11-21 12:40:57 UTC | Codex R8 FAIL（6×P1、2×P2，SHA-256 `c6494563fb57`） | 全面闭环 R8 全部 6×P1 与 2×P2（配置域原位保留已围栏只读 `config.db` 墓碑、反向配置持久化 `targets_staged_for_rename` 保护、拆至 55 卡、双源双向 post-unlink 全覆盖、两类 100% 可达 Step 2.6 解封实测、围栏表述与配置有界锁契约收紧） | 51 卡 → 55 卡（新增 `RB-10e`/`RB-11e`/`RB-11f`/`RB-11g`） | 全文 |
| 2026-11-21 13:02:50 UTC | Codex R9 FAIL（5×P1、0×P2，SHA-256 `ef9740685382`） | 闭环 R9 #1/#2（配置首建先非覆写 `hard_link` 墓碑预占、4 步 `init_phase="fresh_create"` 持久化状态机与全边界恢复）并引入先发 `capture.db` + `PRAGMA application_id` 与全新 Redb 仓 spool 墓碑，拆至 59 卡 | 55 卡 → 59 卡（新增 `RB-03d`/`RB-03e`/`RB-10f`/`RB-11h`） | 全文 |
| 2026-11-21 15:36:12 UTC | Codex R10 FAIL（4×P1、1×P2，SHA-256 `32de15d6d868`） | 闭环 R10 全部 4×P1 与 1×P2（正常/恢复统一非覆写 `hard_link`、反向全程持 `capture-spool-consumer.lock` 与非空 `quarantine/` fail-closed、拆至 71 卡、网站路径对齐 `AGENTS.md:135` + `DEP-RB-03`、`RB-32`/`RB-33` 改 `mktemp -d` + `trap`） | 59 卡 → 71 卡（新增 12 卡） | 全文 |
| 2026-11-21 16:11:33 UTC | Codex R11 FAIL（3×P1、0×P2，SHA-256 `e0397fccf0c5`） | 闭环 R11 全部 3×P1（拆至 80 卡、引入 16B `HEADER_CRC32C + FRAME_CRC32C` 帧头与 `storage-redb` 自动蕴含 `storage-queue`） | 71 卡 → 80 卡（新增 9 卡） | 全文 |
| 2026-11-21 16:57:31 UTC | Codex R12 FAIL（5×P1、0×P2，SHA-256 `e165ec0f83fe`） | 闭环 R12 #2 活跃段校验、#5 `compat-offline-core` 精确作用域 `diff -u` 并初步拆至 90 卡与引入 `REL-RB-02` 家族 | 80 卡 → 90 卡（新增 10 卡） | 全文 |
| 2026-11-21 17:22:54 UTC | Codex R13 FAIL（3×P1、0×P2，SHA-256 `1c6db142befb`） | 全面闭环 R13 全部 3×P1：① **拆净 `RB-08` 与 `RB-08c` 动作级独立谓词（P1 #1）：** 将 `RB-08` 拆为 `RB-08`（4/8 并发与双副本提交及 Case C）+ `RB-08d`（7/8 纯 Case D 轮转孤儿恢复，逐项单动作拆分旧段短段拒收、旧段前缀位翻转拒收、有效后继保留、空孤儿清理、半帧隔离、半帧删孤儿、双副本同步），将 `RB-08c` 拆为 `RB-08c`（5/8 活跃段与单损）+ `RB-08e`（5/8 完整 Subcase B2 双副本缺损全通道现存段清洁重建与 fail-closed 零重建零截断守卫），彻底消除多动作组合 AC（零 waiver）；② **Subcase B2 双副本缺损全通道现存段扫描（P1 #2）：** 修订 `ADR-RB-05` #3 Subcase B2/#4 与 `RB-08e`，要求双副本同时缺损时按段号升序逐段校验通道内**全部现存段 `min_seg..=max_seg`**（含全部较旧未回收轮转段，且每较旧轮转段 `seg < max_seg` 必非空 `actual_len > 0`）均 100% `CleanEof` 才允许重建，并在 `RB-08e` 新增较旧段损坏与较旧段截断（含尾帧截断与 0 字节截断，即使 `max_seg` 洁净）fail-closed 零重建零截断实测；③ **`RB-34`/`RB-34b`/`RB-34c` 独立 `--target-dir` 防二进制覆盖（P1 #3）：** 默认+`keyring` 构建输出至 `target/rb34-default-keyring`、纯 SQLite+`keyring` 回退构建输出至 `target/rb34-sqlite-keyring`，验证前核对 `LIBRA_DEFAULT_KEYRING_SHA256` 且两产物 SHA-256 相异，彻底杜绝纯 SQLite 构建覆盖默认红库成品 | 90 卡 → 92 卡（新增 `RB-08d`/`RB-08e`） | 全文 |
| 2026-11-21 17:43:38 UTC | Codex R14 FAIL（3×P1、0×P2，SHA-256 `7b4acf25990f`） | 全面闭环 R14 全部 3×P1 并逐卡复审全计划：① **Subcase B2 双副本同损无条件 fail-closed（P1 #1）：** 修订 `ADR-RB-05` #3、`RB-07b` 与 `RB-08e`，规定空通道须先原子建盘并 `fsync_dir` 初始 26B `(0,0,0)` 双副本 `tail.meta`/`tail.meta.bak` 才建首段 `events-00000000.log`（确保首次入队崩溃归入 Case C）；当目录存在 ≥ 1 个 `events-*.log` 且双副本同时缺失或 CRC 损坏时，**一律无条件 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`、严禁自动重建元数据、零截断、零追加）**，彻底消除完整帧边界截断与活跃段 0 字节截断静默丢帧，并在 `RB-08e` 补齐专测；② **全计划 `G-03` 独立子动作谓词复审与拆分（P1 #2）：** 将 `RB-08d` 拆为 `RB-08d`（4/8）+ `RB-08f`（6/8，逐项单动作拆分 `unlink` 与 `fsync_dir`），并逐卡复核全计划拆出 `RB-03j`/`RB-03i`/`RB-06e`/`RB-07f`/`RB-07e`/`RB-08g`/`RB-09c`/`RB-10i`/`RB-10j`/`RB-10k`/`RB-11q`/`RB-11r`/`RB-11s`/`RB-11u`/`RB-11v`/`RB-11w`/`RB-11t`/`RB-32b` 及 `REL-RB-03` 家族 `RB-33`/`RB-33b`/`RB-33c`（并将 `RB-30` 收紧至 8/12），全 113 卡零超限零 waiver；③ **消费者未提交尾恢复与历史段回收严格受 `writer.lock → capture-queue/append.lock` 锁序协调（P1 #3）：** 修订 `ADR-RB-05` #4、`GC-RB-04`、`RB-09`、`RB-09b` 并拆出 `RB-09c`，禁止未持 `append.lock` 隔离未提交尾或 `unlink` 历史段，释放 `append.lock` 后才提交红库写事务（与生产者零死锁），并新增暂停活跃生产者并发实测 | 92 卡 → 113 卡（新增 21 卡并设 `REL-RB-03`） | 全文 |
| 2026-11-21 18:25:52 UTC | Codex R15 FAIL（4×P1、0×P2，SHA-256 `86e59df8eddd`） | 全面闭环 R15 全部 4×P1 并顺带修复 `RB-32b` `G-07`：① **0 段文件四分法封堵唯一活跃段缺失伪装空通道（P1-1）：** 修订 `ADR-RB-05` #3 与 `RB-08e`，仅当 `0` 个 `events-*.log` 且 `tail.meta`/`tail.meta.bak` 均不存在于磁盘时初始化 34B `LTM2` `(0,0,0,0,[])` 双副本；若 `0` 段但存在有效元数据记录 `committed_len > 0` / `active_seg > 0` / `!sealed.is_empty()` 或存在损坏元数据文件，一律 fail-closed 报 `CorruptCommittedFrame`（`LBR-IO-001`、零重置、零追加），拆出 `RB-08h` 专承非空 Subcase B2；② **消费者尾损恢复零跳帧与 DB 原子高水位（P1-2）：** 修订 `ADR-RB-05` #4 与拆出 `RB-09e`，消费者在 `writer.lock → append.lock` 下执行与生产者统一的 Case C 恢复（100% 保留 `committed_len` 后完整 CRC32C 已同步帧至 `recovered_valid_end`，仅隔离 `offset >= recovered_valid_end` 残字节并封存 `S` 切段 `S+1`，**绝不提前推进库内 `QUEUE_HIGHWATER_*` 或外部镜像**），由后续正常消费落库全部积压与恢复尾帧并仅在 `CaptureStore::append_capture_batch` 事务内原子推进高水位，补齐积压 + 完整同步尾 + 崩溃半帧端到端专测；③ **全计划 `G-03` 严格独立谓词重审拆分 + `RB-32b` `G-07` 修复（P1-3）：** 拆分 `RB-01`（+`RB-01d`）、`RB-07`（+`RB-07g`）、`RB-08e`（+`RB-08h`）、`RB-09b`（+`RB-09d`）、`RB-09c`（+`RB-09e`）、`RB-10` 离线 WAL 迁至 `RB-10a`、`RB-11d`（+`RB-11x`）、`RB-18`（+`RB-18b`）、`RB-20`（+`RB-20c`）、`RB-26`（+`RB-26b`）、`RB-28`（+`RB-28b`）、`RB-29a`（+`RB-29c`）、`RB-31`（+`RB-31b`），并将 `audit` 卡 `RB-32b` 修正为 `Release boundary: no-release`（`Version increment: N/A`、`Release write set: N/A`）；④ **未回收轮转段持久化认证边界 `LTM2.sealed[]`（P1-4）：** 升级 `tail.meta`/`tail.meta.bak` 为 `LTM2` 格式（30B 头 + `16B × SEALED_COUNT` `(SEG_ID, SEALED_LEN, SEALED_CRC32C)` 表 + 4B `META_CRC32C`，双副本按 `(active_seg, committed_len, min_live_seg)` 全序收敛），轮转原子登记封存边界，消费端强验全部未回收轮转段存在/长度/CRC32C（精准拦截完整帧边界 `CleanEof` 截断），回收严格先原子更新 `LTM2` 推进 `MIN_LIVE_SEG` 再 `unlink` + `fsync_dir`，由 `RB-09d` 逐项验证 | 113 卡 → 125 卡（新增 12 卡，`RB-32b` 改 `no-release`） | 全文 |
| 2026-11-21 18:47:29 UTC | Codex R16 FAIL（4×P1、0×P2，SHA-256 `954744000b07`） | 全面闭环 R16 全部 4×P1：① **两步 Case C 与 Case D 封存前恢复旧段完整尾（P1 #1）：** 修订 `ADR-RB-05` #3 与 `RB-08d`/`RB-08g` 并新增 `RB-08i`，Case C 先截断 `S` 至 `recovered_valid_end` + `sync_data()` 并立即提交 Step 1 `LTM2` 推进 `COMMITTED_LEN = recovered_valid_end`（Step 2 仅当 `recovered_valid_end > 0 && sealed.len() < 256` 再建 `S+1` 封存）；Case D 先校验 `[0, committed_len)` 并前向恢复 `[committed_len, actual_len(S))` 后以真实 `old_valid_end`/`old_sealed_crc` 封存旧段 `S`（绝不用旧 `committed_len`），并在 `RB-08i` 逐边界注入 Case C 全崩溃点与嵌套 Case C→D 含有效后继及消费排空实测；② **`LTM2.sealed[]` 256 项预变更上界与可恢复背压（P1 #2）：** 修订 `ADR-RB-05` #3 并新增 `RB-08j`，常规 16 MiB 轮转在创建 `S+1`/写盘前先检 `sealed.len() < 256`（超界返回可恢复 `LBR-AGENT-046`，零文件改动，消费回收后即恢复）；Case C 在 `sealed.len() == 256` 或 `recovered_valid_end == 0` 时完成 Step 1 原地恢复并跳过 Step 2 轮转（零第 257 项、零阻塞消费），全覆盖 `256 → 257` 边界转换测试；③ **全计划 `G-03` 独立谓词全面拆净（P1 #3）：** 将 `RB-01` 展开为 8 条单谓词 AC（`8/8`）、`RB-01d` 展开至 `6/8`、拆分 `RB-09e`→`RB-09e`+`RB-09f`、`RB-10b`→`RB-10b`+`RB-10l`、`RB-11j`→`RB-11j`+`RB-11y`、`RB-29`→`RB-29`+`RB-29d` 并展开 `RB-08`/`RB-09b`/`RB-09d` 复合条，同步修正审计表说明口径严格对齐 `plan-template.md` `G-03`；④ **修复 `RB-01` `src/lib.rs` 洁净态失败脚本与三依赖独立校验（P1 #4）：** 将 `rg -c "^-[^-]"` 改为遵循 `plan-template.md:591` 退出码分流的零删除门（实测洁净/删除/多增/空 diff 四态通过），并拆出 `redb`/`postcard(use-std)`/`crc32c`/`sea-orm` 4 条独立单门验证（`VER=8/8`） | 125 卡 → 131 卡（新增 `RB-08i`/`RB-08j`/`RB-09f`/`RB-10l`/`RB-11y`/`RB-29d`） | 全文 |
| 2026-11-21 19:10:13 UTC | Codex R17 FAIL（3×P1、0×P2，SHA-256 `f35a03aa391a`） | 全面闭环 R17 全部 3×P1 并消除只读沙箱 `/dev/fd` 进程替换依赖：① **严格按段状态与批边界推进高水位（P1 #1）：** 修订 `ADR-RB-05` #4、重写 `RB-09f` 并新增 `RB-09g`：活跃段（`S == active_seg`，含纯已同步尾、满 256 表 Step C1 原地恢复及 0 字节恢复）消费每批（≤ 128 帧）严格推进至 `(S, batch_end_offset)`（最高停在 `(S, committed_len)`，0 字节停在 `(S, 0)`）绝不跳至 `(S+1, 0)`；已封存段（`S < active_seg`）多批中间批次推进至 `(S, batch_end_offset)`，仅当最终批次消费至 `batch_end_offset == sealed_len(S)` 时随该 DB 事务原子推进至 `(S+1, 0)`；补齐已封存段与全部三类活跃恢复场景的多批（> 128 帧）排空与后续追加零跳帧零倒退实测；② **消除 `RB-08g`/`RB-08i`/`RB-08j` 独立谓词低估（P1 #2）：** 拆净为 `RB-08g`（4/8）+ `RB-08m`（6/8）、`RB-08i`（4/8）+ `RB-08k`（6/8）、`RB-08j`（5/8）+ `RB-08l`（5/8），全部 135 卡严格逐条单谓词且全审计表重生成；③ **修复 `RB-01` VER8 完整双态基线对比与零 `<(...)` 进程替换（P1 #3 + 沙箱可用性）：** `RB-01` VER8 基于显式前置基线 `PRE_CARD_BASE` 通过 `mktemp` 文件同时校验工作区完整 `src/lib.rs` 与已暂存 `libra diff --old "$base_ref" --staged -- src/lib.rs` 精确满足 `+1`/`-0`（覆盖未暂存与已暂存双态），同步将 `RB-01` VER7（双验工作区与 `:Cargo.toml` 索引）及 `RB-12`/`RB-29c`/`RB-30` 全部 `<(...)` 替换为 `mktemp` 临时文件 | 131 卡 → 135 卡（新增 `RB-08m`/`RB-08k`/`RB-08l`/`RB-09g`） | 全文 |
| 2026-11-21 19:19:18 UTC | Codex R18 FAIL（4×P1、0×P2，SHA-256 `63ef4c643f9d`） | 全面闭环 R18 全部 4×P1 并消除只读沙箱 `mktemp` 阻塞：① **已排空活跃段随后被轮转的原子零帧高水位推进（P1 #1）：** 在 `ADR-RB-05` #4 ② 新增规则 **(c)** 并新增 `RB-09h`（`7/8`）：当消费者先排空活跃段停在 `(S, committed_len)` 后生产者轮转 `S` 致 `S < active_seg && hw_offset == sealed_len(S)` 时，先认证 `LTM2.sealed[S]`（损坏/截断或 `hw_offset > sealed_len(S)` 立即 fail-closed 报 `CorruptCommittedFrame` `LBR-IO-001`），再经 `CaptureStore::append_capture_batch(&[], (S + 1, 0))` 原子零帧推进高水位并镜像外部 `highwater`，全面实测 16 MiB/Case C Step C2 交错、提交前后崩溃、`S+1` 排空与后续追加及 `S` 回收；② **理顺 `RB-08*` 与 `RB-09*` 依赖可执行顺序（P1 #2）：** 将 `RB-08k`/`RB-08j`/`RB-08l` 纯化为 `segment.rs` 生产者/段层实现与验证（写集严格 3 文件，零前置依赖 `writer.rs`/`highwater.rs`），把全部真实消费者端到端排空与回收验收后置至 `RB-09`..`RB-09h` 之后的新卡 `RB-09i`（`7/8`）；③ **彻底消解独立谓词低估（P1 #3）：** `RB-08k`（`5/8`）、`RB-08j`（`5/8`）、`RB-08l`（`6/8`，Step C1 隔离/截断刷盘/`LTM2` 提交/跳过 Step C2/段内追加/轮转背压逐项独立）、`RB-08m`（`7/8`）、`RB-09h`（`7/8`）、`RB-09i`（`7/8`）均严格 1:1 单谓词 ≤ 8；④ **替换 `RB-01` VER8 diff 行正则并消除 `mktemp`（P1 #4 + 只读沙箱可执行性）：** `RB-01` VER8 改为零临时文件纯内存 `python3` 完整行列表精确比对（含内联 `+`/`-` 行负例自测，覆盖工作区与 `:src/lib.rs` 索引全 9 态），同步将 `RB-01` VER7 及 `RB-12`/`RB-29c`/`RB-30` 改为零临时文件纯内存 `python3` 比对 | 135 卡 → 137 卡（新增 `RB-09h`/`RB-09i`） | 全文 |
| 2026-11-21 19:27:11 UTC | Codex R19 FAIL（1×P1、1×P2，SHA-256 `5d0ff12aec17`） | 彻底闭环 R19 仅余的 1×P1 与 1×P2：① **按最严独立可分别失败谓词拆净全部队列恢复卡（P1 #1）：** `RB-08m`→`RB-08m`（6/8 Step C1 逐项分离 `truncate` 与 `sync_data`）+`RB-08n`（7/8 Step C2 逐项分离建 `0600` 段、`fsync_dir`、封存、切段与 3 类帧解码）；`RB-08l`→`RB-08l`（5/8 满表 Step C1 逐项分离 `truncate` 与 `sync_data`）+`RB-08o`（5/8 跳过 Step C2 三不变量 `ACTIVE_SEG`/`sealed.len()`/零 `S+1` 文件与段内/轮转分流）；`RB-09h`→`RB-09h`（6/8 16 MiB 与 Case C Step C2 双轮转路径零帧推进 + 翻转/截断/缺失/越界 4 类拒收）+`RB-09j`（6/8 提交前后崩溃、`S+1` 既有帧与后续追加帧分离消费、活跃高水位与 `S` 安全回收）；同步将邻卡 `RB-08g`（7/8）、`RB-08i`（8/8）、`RB-08k`（7/8）、`RB-08j`（6/8）全部展开至 1:1 原子谓词，全 140 卡审计表自动重生成；② **修正 Phase 3 进入条件（P2 #1）：** 将 Phase 3 进入条件由 `RB-09f` 更新为 `RB-09i complete`，与实施顺序及 `RB-10` 依赖精确对齐 | 137 卡 → 140 卡（新增 `RB-08n`/`RB-08o`/`RB-09j`） | 全文 |
| 2026-11-21 19:33:48 UTC | Codex R20 FAIL（2×P1、0×P2，SHA-256 `45763539376e`） | 彻底闭环 R20 仅余的 2×P1 细粒度原子谓词拆分：`RB-08n`→`RB-08n`（6/8 Step C2 建文件/`fsync_dir`/`LTM2.sealed[]`/`ACTIVE_SEG`/`COMMITTED_LEN=0`/`PREFIX_CRC32C=0` 逐字段 1:1）+`RB-08p`（4/8 前缀/同步尾解码、追加 `Ok` 与新段解码）；`RB-08i`→`RB-08i`（6/8 边界 1–3 将 `ACTIVE_SEG` 与 `COMMITTED_LEN` 独立分条）+`RB-08q`（4/8 0 字节首帧隔离/截断/`ACTIVE_SEG`/`sealed.len()` 独立分条）；`RB-08j`→`RB-08j`（5/8 256 背压拆开 `LBR-AGENT-046`/零新建 `S+1`/既有文件零改 + 槽位释放建段/登记）+`RB-08r`（4/8 Case D 拆开 `LBR-IO-001` 与 `sealed.len()==256` + 解码器双拒收）；`RB-08o` 同步拆开满表轮转错误码与零改动（6/8）；`RB-09j`→`RB-09j`（5/8 提交前后崩溃与 `S+1` 消费/高水位）+`RB-09k`（4/8 旧段 `S` 回收拆开 `MIN_LIVE_SEG`/摘除 `sealed[]`/`unlink`/`fsync_dir`）；`RB-09i`→`RB-09i`（6/8 嵌套 Case C→D 消费 `LTM2.sealed[S]` 长度与 CRC32C 分离校验、3 类帧独立落库与高水位）+`RB-09l`（6/8 满 256 表真实消费回收解除背压、建 `S+1`/登记 `sealed[]`、满表 Case C 校验/256 段/活跃段独立落库）；同步更新 `RB-10` 与 Phase 3 进入条件至 `RB-09l` | 140 卡 → 145 卡（新增 `RB-08p`/`RB-08q`/`RB-08r`/`RB-09k`/`RB-09l`） | 全文 |
| 2026-11-21 19:42:42 UTC | Codex R21 FAIL（2×P1、0×P2，SHA-256 `df831a7bbd98`） | 彻底闭环 R21 全部 2×P1：① **重构 `RB-08o`（及 `RB-08l`/`RB-08j`）消除复合谓词低估（P1 #1）：** `RB-08l`（`7/8`）负责满 256 表 Case C 跳过 Step C2 守卫分支（隔离/截断/`COMMITTED_LEN`/`PREFIX_CRC32C`/`ACTIVE_SEG==S`/`sealed.len()==256`/零新建 `S+1`）；`RB-08o`（`7/8`）负责满 256 表恢复后段内准入与轮转预变更拒收分支（段内写入读回、推进 `COMMITTED_LEN`、轮转报 `LBR-AGENT-046`、零新建 `S+1`、`events-<S>` 零改、`tail.meta` 零改、`tail.meta.bak` 零改）；同步将 `RB-08j`（`7/8`）拆清既有段与 `tail.meta`/`tail.meta.bak` 三项零改；② **彻底消除纯测试实现卡以严格满足 `G-02`（P1 #2）：** 撤销纯测试 `RB-08p`（将段 `S` 前缀与同步尾解码并回 Step C1 实现卡 `RB-08m` `8/8`，将 `S+1` 追加与解码并回 Step C2 实现卡 `RB-08n` `8/8`），并显式对齐 `RB-08i`（幂等隔离重入/纯已同步尾收敛/空后继收敛实现）、`RB-08q`（`recovered_valid_end==0` 跳过 Step C2 守卫实现）、`RB-09j`（零帧崩溃恢复与 `S+1` 游标衔接实现）、`RB-09k`（零帧跨段回收触发实现）、`RB-09i`（`writer.rs` `max_seg > active_seg` Case D 恢复委托与嵌套排空实现，补齐 `RB-09e` 仅覆盖 Case C 的真实生产路径）及 `RB-09l`（`writer.rs` 满 256 封存表高压逐段即时回收降压与全量排空实现）均为独立生产行为增量与同卡完整验证 | 145 卡 → 144 卡（撤销 `RB-08p` 并回 `RB-08m`/`RB-08n`） | 全文 |
| 2026-11-21 19:48:05 UTC | Codex R22 FAIL（3×P1、0×P2，SHA-256 `a61531861064`） | 彻底闭环 R22 全部 3×P1：① **渐进归属 Step C2 守卫消除三卡重叠（P1 #1）：** `RB-08g`（8/8）实现基础 16 MiB 轮转与共用 `create_segment_file`（未含 256 上界）；`RB-08n`（7/8）实现基础无条件 Step C2 切段（明确不含 0 字节与 256 守卫）；`RB-08q`（4/8）新增 `recovered_valid_end == 0` 跳过 Step C2 守卫；`RB-08j`（7/8）引入 `MAX_SEALED_SEGMENTS = 256` 与洁净 Case A 轮转背压；`RB-08r`（4/8）为 Case D 与 `LTM2::decode` 新增 256 上界；`RB-08l`（7/8）为 Case C 新增 `sealed.len() >= 256` 跳过 Step C2 守卫，零重复归属；② **修复 `RB-08n` 建文件+`0600` 合并低估（P1 #2）：** `RB-08g` 拆清建段、`0600` 与 `fsync_dir`（8/8），`RB-08n` 复用 `create_segment_file` 并降至精确 7/8；③ **对齐 Step ① 恢复持久化与 Step ② 入队零写背压契约并补单调用实测（P1 #3）：** 修订 `ADR-RB-05` #3 Step ② 明确允许 Step ① 先落盘合法崩溃恢复再由 Step ② 拒绝本次待入队新帧（洁净 Case A 零磁盘改动由 `RB-08j` 验证；单次 `enqueue_frame` 先持久化 Step C1 尾损恢复再因满 256 表返回 `LBR-AGENT-046` 且零新帧写入/零新建 `S+1` 由 `RB-08o` 8/8 实测） | 保持 144 卡（更新 `ADR-RB-05`、`RB-08g`/`RB-08n`/`RB-08q`/`RB-08j`/`RB-08l`/`RB-08o`） | 全文 |
| 2026-11-21 19:52:38 UTC | Codex R23 FAIL（2×P1、0×P2，SHA-256 `7d811f954dfd`） | 彻底闭环 R23 仅在 `RB-08o` 检出的 2×P1：① **拆分 `RB-08o` 消除写入+解码合并低估（P1 #1）：** `RB-08o`（`5/8`）聚焦满 256 表恢复后段内准入（`<= 16 MiB`），独立验收返回 `Ok(())`、无损解码读回、推进 `COMMITTED_LEN`、维持 `ACTIVE_SEG == S` 与 `sealed.len() == 256`；② **新增 `RB-08s`（`8/8`）彻底夯实单调用「先 Step ① 恢复再 Step ② 拒收」（P1 #2）：** 在 `segment.rs` 增加新帧写路径插桩计数钩子，逐条单一断言验证返回 `LBR-AGENT-046`、新帧写路径调用次数 `== 0`（证明从未先写入再截断）、`events-<S>.log` 完整字节序列与期望恢复字节逐字节相等（`actual_bytes == expected_bytes`）、`quarantine/` 保存残尾、零新建 `S+1`，以及关闭重开通道后持久化 `COMMITTED_LEN`、`PREFIX_CRC32C` 与 `sealed.len() == 256` | 144 卡 → 145 卡（拆出 `RB-08s`） | 全文 |
| 2026-11-21 20:03:12 UTC | Codex R24 FAIL（2×P1、1×P2，SHA-256 `1f6753c674ae`） | 彻底闭环 R24 全部 2×P1 与 1×P2：① **隔离 256 封存表拒收条件与 256 MiB 总容量上限（P1 #1）：** 在 `RB-08j`（`8/8`）与 `RB-08s`（`8/8`）均以 256×4 KiB 小封存段构造夹具并增加独立原子断言验证 `unconsumed_priority + unconsumed_normal + quarantine + frame_buf.len() <= 256 MiB`，确保 Step ② (a) 永不抢先触发；② **断电持久隔离发布强约束原语严格先于全部截断与删段（P1 #2）：** 修订 `ADR-RB-05` #3（Step ① 清理 `quarantine/*.tmp`、Case C Step C1、Case D 旧段坏尾、Case D 空/半帧孤儿及 Case D 有效后继半帧尾）与 #4 ② 消费端复用，并在 `RB-08` 与 `RB-08d` 之间前置独立生产原语卡 `RB-08t`（`8/8`，供 `RB-08d`/`RB-08f`/`RB-08m`/`RB-08k`/`RB-08l`/`RB-08s` 全链路调用 `quarantine_and_truncate_segment` / `quarantine_and_unlink_orphan`），含顺序插桩与两处断电边界故障注入；③ **修正 `RB-08l` 及关联卡片 `Out of scope` 归属引用（P2 #1）：** 明确区分 `RB-08t`、`RB-08r`、`RB-08o` 与 `RB-08s` | 145 卡 → 146 卡（新增 `RB-08t`） | 全文 |
| 2026-11-21 20:12:19 UTC | Codex R25 未完中止 + R26 FAIL（2×P1、1×P2，SHA-256 `f9d69481e1a0`） | 彻底闭环 R26 全部 2×P1 与 1×P2：① **消除 `RB-08d` 与 `RB-08k` 重叠违反 `G-02`（P1 #1）：** `RB-08d`（`7/8`）纯化为仅实现洁净旧段（`actual_len(S) == committed_len`）+ 洁净有效后继 `S+1` 基础 Case D；`RB-08k`（`8/8`，`split-from=RB-08d`）成为嵌套 Case C→D 旧段未提交尾分支（`max_seg > active_seg && actual_len(S) > committed_len`，调用 `RB-08m::case_c_step_c1`）的唯一独占生产与验收卡；② **补齐有效后继半帧尾及全部调用方顺序插桩与断电边界验证（P1 #2）：** 新增独立生产分支卡 `RB-08u`（`7/8`，位于 `RB-08d` 与 `RB-08f` 之间）独占实现有效后继 `S+1` 合法帧后接未提交半帧经 `RB-08t::quarantine_and_truncate_segment` 持久隔离截断（含调用方顺序插桩与重命名前/目录刷盘后两处崩溃边界），并同步在 `RB-08f`（`8/8`）、`RB-08i`（`8/8`）与 `RB-08k`（`8/8`）补齐调用方顺序插桩与重命名前崩溃验证；③ **先只读分类判定、确认合法可恢复后再清理 `.tmp`（P2 #1）：** 修订 `ADR-RB-05` #3 Step ① 严格先只读扫描分类，损坏态（`0c`/`0d`/`Subcase B2`/`Case E`/前缀失配）立即 fail-closed 保留全部遗留 `tail.meta*.tmp` 与 `quarantine/*.tmp` 零磁盘改动，并在 `RB-08c`（`6/8`）、`RB-08e`（`6/8`）、`RB-08h`（`5/8`）补齐实测 | 146 卡 → 147 卡（新增 `RB-08u`） | 全文 |
| 2026-11-21 20:19:58 UTC | Codex R27 FAIL（3×P1、1×P2，SHA-256 `8cbd0f56cdd2`） | 彻底闭环 R27 全部 3×P1 与 1×P2：① **对齐 `RB-08e` 描述与情况 0b/0d 单副本恢复语义（P1 #1）：** 明确 0 段且恰有 1 份有效初始 `LTM2` `(0,0,0,0,[])` 时镜像修复放行（0b），仅两份副本均非有效 `LTM2` 时拒收（0d）；② **Case D 满 256 封存表有效后继纳入 Phase 1 100% 只读先判（P1 #2）：** 修订 `ADR-RB-05` #3 Step ① Phase 1 与 Case D，在执行任何 `.tmp` 清理、副本镜像修复、已回收孤儿摘除或旧段尾截断前先只读扫后继段，满表有效后继立即 fail-closed 整盘零改动，并在 `RB-08r`（`6/8`）补齐嵌套旧段坏尾 + 遗留 `.tmp` 整盘逐字节快照相等实测；③ **按独立谓词将 `RB-08k` 拆净为 `RB-08k`（`6/8`）+ `RB-08v`（`5/8`）（P1 #3 / `G-03`）：** `RB-08k` 专注嵌套洁净旧尾封存与 `S+1` 字节留存（6 单谓词），新增 `RB-08v` 专注嵌套损坏旧尾持久隔离、调用方顺序插桩、截断、重命名前崩溃与封存（5 单谓词）；④ **共享 `quarantine/` 按通道前缀隔离清理 `.tmp`（P2 #1）：** Phase 2 仅清理本通道 `<lane>/tail.meta*.tmp` 与 `quarantine/<lane>-*.tmp`，并在 `RB-08h`（`6/8` AC6）实测健康 `priority/` 入队原样保留损坏 `normal/` 的 `quarantine/normal-*.tmp` | 147 卡 → 148 卡（新增 `RB-08v`） | 全文 |
| 2026-11-21 20:23:20 UTC | Codex R28 FAIL（1×P1、0×P2，SHA-256 `a8a4057ac17f`） | 彻底闭环 R28 唯一 P1：将 `RB-08v`（`5/8 → 8/8`）由严格不等式 `committed_len < old_valid_end < actual_len(S)` 扩展为与 `ADR-RB-05` #3 Case D 完全对齐的 `committed_len <= old_valid_end < actual_len(S)`，分别以独立单谓词断言验证 `old_valid_end > committed_len`（AC1–AC3）与紧接已确认边界坏尾 `old_valid_end == committed_len`（AC4–AC6）的持久隔离字节、精确截断与 `LTM2.sealed[]` 封存，并保持调用方顺序插桩（AC7）与重命名前崩溃零提前截断（AC8） | 148 卡（不变，更新 `RB-08v`） | `RB-08v`、审计表、追溯表、故障恢复矩阵 |
| 2026-11-21 20:31:37 UTC | Codex R29 FAIL（1×P1、0×P2，SHA-256 `db0c6c37811d`） | 彻底闭环 R29 唯一 P1 并全计划拉网扫清全部同类「错误码 + 磁盘零改动」成对合并卡：① **`RB-08h`（`8/8`）+ 新增 `RB-08x`（`4/8`）：** `RB-08h` 4 夹具逐一拆为错误码断言与全目录+`quarantine/` 逐字节快照相等（8 条 1:1 单谓词）；拆出 `RB-08x` 独占 Phase 2 按通道前缀清理过滤器 `clean_lane_temp_files` 与对称跨通道留存验收；② **`RB-08c`（`6/8`）+ 新增 `RB-08y`（`5/8`）+ `RB-08e`（`6/8`）：** `RB-08c` 纯化为有效双副本 Case A/E/C（错误码与全快照 1:1 拆分）；拆出 `RB-08y` 独占 Subcase B1 单副本 Phase 1 边界先判与镜像修复前 fail-closed；`RB-08e` AC3/AC4 纯化为单错误断言、AC5/AC6 验整盘快照；③ **`RB-10`（`4/8`）+ 新增 `RB-10m`（`8/8`）与 `RB-11`（`5/8`）+ 新增 `RB-11z`（`4/8`）：** 同步将正向/反向 Phase 1 静默门全部拆净为独立生产子轴与 1:1 单谓词断言 | 148 卡 → 152 卡（新增 `RB-08y`/`RB-08x`/`RB-10m`/`RB-11z`） | 全文 |
| 2026-11-21 20:37:54 UTC | Codex R30 FAIL（3×P1、1×P2，SHA-256 `cbe3313f4ecf`） | 彻底闭环 R30 全部 3×P1 与 1×P2：① **WAL 静默改为直调 `PRAGMA journal_mode = DELETE;`（P1 #1）：** 修订 `ADR-RB-07` #1、`RB-10m`、`RB-10a` 与故障恢复矩阵，严禁先调 `wal_checkpoint(TRUNCATE)`，直接由 `locking_mode = EXCLUSIVE` + `PRAGMA journal_mode = DELETE;` 触发 SQLite `sqlite3WalClose` 先争独占 OS+WAL 锁再做内部检查点，多进程实测遇活跃 WAL 读者/写者/空闲连接均立即报 `SQLITE_BUSY` 且 `.db`/`-wal`/`-shm` 100% 逐字节零改动；② **Subcase B1 双向副副本缺失对称卡 `RB-08z`（P1 #2）：** `RB-08y`（`5/8`）负责主副本 `tail.meta` 缺失/损坏，新增 `RB-08z`（`5/8`）对称负责副副本 `tail.meta.bak` 缺失/损坏的前缀位翻转、短段与健康镜像修复；③ **`RB-08x`（`8/8`）补齐双通道内 `tail.meta*.tmp` 清理与跨通道留存（P1 #3）：** 8 条 1:1 断言覆盖双通道 `<lane>/tail.meta*.tmp` 与 `quarantine/<lane>-*.tmp`；④ **`RB-11q` AC1 拆净（P2 #1）：** 拆为错误返回与源库+spool 零改动快照（`4/8 → 5/8`），全计划 153 卡零成对谓词遗漏 | 152 卡 → 153 卡（新增 `RB-08z`） | 全文 |
| 2026-11-21 20:41:23 UTC | Codex R31 FAIL（1×P1、0×P2，SHA-256 `5a68927142dc`） | 彻底闭环 R31 唯一 P1：将 `RB-10m` 拆为两张独立生产轴卡——`RB-10m`（`6/8`）独占 WAL 直接切 `PRAGMA journal_mode=DELETE` 静默门，完整覆盖 WAL 活跃读者、**WAL 活跃写者（`BEGIN IMMEDIATE; INSERT ...`）**与 WAL 空闲连接三类竞争的错误返回及 `.db`/`-wal`/`-shm` 三文件逐字节快照零改动；新增 `RB-10n`（`4/8`，位于 `RB-10m` 与 `RB-10a` 之间）独占在途 `capture-spool/tmp/` 与外部 `capture-queue/append.lock` 竞争拒收 | 153 卡 → 154 卡（新增 `RB-10n`） | 全文 |
| 2026-11-21 20:44:26 UTC | Codex R32 FAIL（1×P1、1×P2，SHA-256 `3d3f300ad3c5`） | 彻底闭环 R32 全部 1×P1 与 1×P2：① **`RB-10n`（`4/8 → 6/8`）及对称 `RB-11z`（`4/8 → 6/8`）补齐全三域快照零改动断言（P1 #1）：** 在途 `capture-spool/tmp/` 拒收独立增加 `capture-queue/` 逐字节快照零改动断言，外部 `capture-queue/append.lock` 拒收独立增加 `capture-spool/` 逐字节快照零改动断言；② **故障恢复矩阵同步补齐 `capture-queue/append.lock` 竞争触发、检测信号与恢复动作（P2 #1）** | 154 卡（不变，更新 `RB-10n`/`RB-11z`） | `RB-10n`、`RB-11z`、审计表、故障恢复矩阵 |
| 2026-11-21 20:47:24 UTC | Codex R33 FAIL（1×P1、0×P2，SHA-256 `41ffa8b76855`） | 彻底闭环 R33 唯一 P1：将 `RB-10n`（`6/8 → 8/8`）、`RB-11z`（`6/8 → 8/8`）及同类 `RB-11q`（`5/8 → 6/8`）中合并的「源库 + spool/queue」快照断言逐域拆为完全独立的单域单谓词断言，并同步更新 Verification 与审计表 | 154 卡（不变，更新 `RB-10n`/`RB-11z`/`RB-11q`） | `RB-10n`、`RB-11z`、`RB-11q`、审计表 |

## 已决议设计决策

实现时若需偏离本节，必须先修订本计划并记录原因，不得在代码中静默改变语义。

### ADR-RB-01: 可叠加 Cargo Feature、编译/测试矩阵与运行时后端分派

- **Status:** Accepted
- **Context:** CI 强制跑 `--all-features` Clippy 与 Rustdoc，且 `libra-migrate` 需同时编译两个后端；单特性构建不能引用未启用依赖；此外 `sea-orm`、`libvault/storage_sqlite` 与 3 个第三方只读 SQLite 解析器在并行开发期需保持可用。
- **Decision:**
  1. **Feature 定义（`Cargo.toml`）：**
     - `default = ["storage-sqlite"]`（`RB-01`..`RB-33`；`REL-RB-02` 家族 `RB-34` 切换为 `default = ["storage-redb", "storage-queue"]` 并经 `RB-34b` 最终二进制全 4 类 Global/System 配置拒收审计后由 `RB-34c` 原子发布，而 `RB-33` 先在 `src/storage/backend.rs` 将启用 `storage-redb` 的官方双后端发行包默认新仓/配置后端切换为 `Redb`）。
     - `storage-sqlite = []`（并行期间 `sea-orm` 保持常驻依赖以确保第三方只读解析器与其它 Agent 零编译回归；`sea-orm` 的最终可选下沉归 `DEFER-RB-02`）。
     - **`storage-redb = ["storage-queue", "dep:redb", "dep:postcard", "dep:crc32c"]`**（`storage-redb` 自动蕴含 `storage-queue`，引入 `redb = "5.0.0"`、`postcard = { version = "1.1", features = ["use-std"] }`、`crc32c = "0.6.9"`；从而彻底消除「启用 `storage-redb` 却未启用 `storage-queue` 导致红库无捕获交付路径」的无效组合，同时 `--features storage-redb,storage-queue` 仍幂等有效）。
     - `storage-queue = ["dep:postcard", "dep:crc32c"]`（可独立随 `storage-sqlite` 编译供队列单测使用；运行时仍受下述 #4 约束，SQLite 仓绝不写入 `capture-queue/`）。
     - 仅当 `not(any(feature = "storage-sqlite", feature = "storage-redb"))` 时 `compile_error!`。
  2. **`cfg` 测试归属矩阵：**
     - 单后端 SQLite 行为测试：`#[cfg(feature = "storage-sqlite")]`，由 `--no-default-features --features storage-sqlite` 验证。
     - 单后端 redb / 队列测试：`#[cfg(feature = "storage-redb")]` / `#[cfg(feature = "storage-queue")]`，由 `--no-default-features --features storage-redb`（验证 `storage-redb` 隐式开启 `storage-queue` 的编译与端到端捕获队列交付）与 `--no-default-features --features storage-redb,storage-queue` 验证；`REL-RB-02`（`RB-34`/`RB-34b`/`RB-34c`）切默认 Feature 后的纯红库发布态端到端测试由 `--features keyring`（启用默认 `storage-redb,storage-queue` + `keyring`，**不启 `storage-sqlite`**）验证。
     - **双后端等价契约与双向迁移测试：** 严格标注 `#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]`，显式由 `--features storage-sqlite,storage-redb,storage-queue` 运行验证。
  3. **运行时后端选择、清单前置强制校验与预集成门控（`StorageBackendKind::resolve`）：**
     - **清单优先（严禁环境变量旁路清单）：** 任何命令在读取 `LIBRA_STORAGE_BACKEND` **之前**，必须先检查仓库目录：
       - ① 若 `.libra/storage-format.json` 存在且 **`finalized == false`** → **无条件 fail-closed 报 `LBR-IO-001`（提示重跑 `libra-migrate` 完成恢复），即使设置了 `LIBRA_STORAGE_BACKEND=redb` 或 `LIBRA_STORAGE_BACKEND=sqlite` 也绝不绕过**；
       - ② 若 `.libra/storage-format.json` 存在且 `finalized == true` → 权威后端为 `manifest.backend`；若 `LIBRA_STORAGE_BACKEND` 指定了不同后端（`override != manifest.backend`），**立即 fail-closed 报 `LBR-CLI-003`（提示先运行 `libra-migrate`）**；
       - ③ 若清单不存在但 `.libra/libra.db` 存在 → 权威后端为 `Sqlite`；若 `LIBRA_STORAGE_BACKEND=redb`，**立即 fail-closed 报 `LBR-CLI-003`（提示先运行 `libra-migrate --to redb`）**；
       - ④ 仅当创建全新仓库（无清单且无 `.libra/libra.db`）时，`LIBRA_STORAGE_BACKEND=redb|sqlite` 方可覆盖编译期默认后端（默认：`RB-01`..`RB-32` 为 `Sqlite`，`RB-33` 起对含 `storage-redb` 的二进制为 `Redb`）。
     - **编译启用与统一编译期预集成门控（`RB-01b`..`RB-29` 生效，`RB-29a` 原子解除，已发行二进制零旁路）：** 在 `src/storage/backend.rs` 定义编译期常量 `pub const REDB_RUNTIME_UNLOCKED: bool = false;`（于 `RB-29a` 在 `REL-RB-01` 家族内原子改为 `true` 并同步切换 `.github/workflows/base.yml` 解锁态 CI 用例）。所选后端未在当前二进制中编译启用，或所选为 `Redb` 但 `REDB_RUNTIME_UNLOCKED == false` 时，已编译发行的 `libra` CLI 立即 fail-closed（`LBR-CLI-003`），且已编译发行的 `libra-migrate` 对全部实盘切换（`--to redb`/`--to sqlite`/`--global`/`--system`）**无条件 fail-closed 返回 `LBR-CLI-003`（忽略 `LIBRA_TEST` 及一切环境变量，绝不存在运行时旁路）**；仅 `--dry-run` 在发行二进制中可用，完整迁移引擎测试通过仅存在于 `#[cfg(test)]` 构建的内部接缝 `run_cli_for_test` 驱动。识别为 `Sqlite` 但仅编译了 `storage-redb` 时 fail-closed 输出 `libra-migrate` 升级指引；识别为 `Redb` 但仅编译了 `storage-sqlite` 时 fail-closed 提示需要 `storage-redb` 或运行 `libra-migrate --to sqlite`。
  4. **队列按所选后端分派：** 因 `storage-redb` 自动启用 `storage-queue`，任何包含 `storage-redb` 的构建（含 `--no-default-features --features storage-redb` 与 `--all-features`）在当前仓库选定后端为 `Redb` 时均恒走 `capture-queue/{priority,normal}/` 队列交付路径；只要仓库选定后端为 `Sqlite`，即便启用了 `storage-queue` 或 `--all-features`，实时 Hook 仍严格走既有 `capture-spool/`，绝不混入 `capture-queue/`。
- **Alternatives considered:** 互斥 `compile_error!` 或 `RB-11c` 提前开放实盘迁移。拒绝：破坏 `--all-features` CI 或导致用户仓库切入不可用后端。
- **Consequences:** 全部特性 × 后端 × 门控组合均有确定性行为与单测覆盖。
- **Revisit when:** 进入 `DEFER-RB-02` 物理移除 SQLite 时。

### ADR-RB-02: 目录布局、沙箱未来代际保护、配置角色回退与权限边界（不改 `shared_perm.rs`）

- **Status:** Accepted
- **Context:** 必须兼容 `plan-20261116` 的 `.libra/private/` 约定、`GC-13` 配置角色隔离、`util.rs`/`clone.rs` 的仓库发现标记以及 `sandbox/policy.rs` 对当前与未来代际数据库文件的全时拒读，且不得修改 `src/utils/shared_perm.rs`。
- **Decision:**
  1. **仓库布局与未来代际目录托管：**
     - `<common_storage>/storage-format.json`（两阶段原子代际清单，记录 `backend`、`finalized`、`generation`、`git_path`、`capture_path`、`backup_dir`、`phase2_progress`、`targets_staged_for_rename`、`target_migration_tag`、`spool_unfenced` 与 `source_digests`）。
     - `<common_storage>/redb/`（专用子目录，托管所有当前与未来代际的 Git 库 `redb/git.gen-<id>.redb` 与 `.tmp`，默认初始代际为 `redb/git.gen-00000001.redb`）。
     - `<common_storage>/private/capture.gen-<id>.redb`（新建 `0600`）、`<common_storage>/private/capture-queue/{priority,normal,quarantine}/`（新建 `0700`/`0600`，`O_NOFOLLOW`），以及**全新 Redb 仓初始化即同步建立的 `<common_storage>/private/capture-spool/`（`0700`）+ `capacity.lock`（`0600`）+ `{tmp,ready,quarantine}` `0400` 普通文件墓碑 + `fsync_dir`**（使全新 Redb 仓自首日起即通过 `spool.rs:330` `ensure_spool` 的 `!is_dir()` 校验物理阻断旧版 SQLite Hook 静默写入）。
     - `<common_storage>/sqlite-backup/<gen>/` 与 `<common_storage>/redb-backup/<gen>/`（迁移备份归档目录）。
     - `<common_storage>/vault.db`（保持 `libvault` 不变）。
     - **发现与沙箱保护：** 新建私有权限由 `src/storage/` 自身设置，不改 `shared_perm.rs`；`util.rs`（`is_valid_storage_dir` / `is_terminal_common_storage` / bare 探测）与 `clone.rs:786`（`REPO_MARKERS`）同步识别 `storage-format.json` 与 `redb` 目录（同时对 `$LIBRA_HOME` 保持零采纳拒绝）；`sandbox/policy.rs:669-681` 将 **`"redb"`、`"private"`、`"storage-format.json"`、`"sqlite-backup"`、`"redb-backup"` 与 `"git.redb"`** 整目录/固定路径钉入 `task_workspace_sensitive_read_paths`——因为整目录拒读规则对沙箱建立**之后**或迁移并发期间在 `.libra/redb/`、`.libra/private/` 内新建的任何未来代际文件（`git.gen-<id>.redb`、`capture.gen-<id>.redb`）自动生效。
  2. **`GlobalConfig` 与 `SystemConfig` 作用域有界锁、先非覆写预占墓碑、首次建库持久化状态机与并发首写串行化协议（`GC-13`，切默认前 SQLite 零行为破坏，切后永久阻断旧版二进制竞态与重建）：**
     - 新格式与作用域切换文件：全局作用域位于 `<config_dir>/libra/`（测试服从 `LIBRA_CONFIG_GLOBAL_DB` 所在目录），系统作用域位于 `/etc/libra/`（测试服从 `LIBRA_CONFIG_SYSTEM_DB` 所在目录），各自包含：`config.redb`、两阶段原子切换清单 **`config-format.json`**（`{backend, finalized, init_phase, generation, legacy_sqlite_tombstone, targets_staged_for_rename, source_db_state, backup_sha256, source_logical_sha256}`）与跨运行时协调锁 **`config-migration.lock`**（`GlobalConfig` `0600`、`SystemConfig` `0644`）。
     - **跨运行时有界锁获取契约（`Sqlite` 与 `Redb` 双运行时路径及 `libra-migrate` 共同参与 `<scope_dir>/config-migration.lock`）：**
       - **有界非无限阻塞获取：** 运行时配置命令统一通过 `ConfigScopeLock::try_lock_shared_bounded(deadline)` / `try_lock_exclusive_bounded(deadline)` 获取锁（`1ms → 25ms` 抖动退避；生产默认 deadline `2s`，单测接缝支持 `Duration::ZERO` 立即非阻塞以确定性验证并发争用）。**在迁移持有 `LOCK_EX` 导致锁获取超期时，运行时命令一律立即 fail-closed 返回类型化 `LBR-IO-001`（提示配置迁移进行中）**，既不无限挂起也不产生双库分叉写入。
       - **只读操作：** 若 `<scope_dir>` 不存在，或目录内无 `config-format.json`、无 `config.db`、无 `config.redb`、无 `config-migration.lock`，直接返回 `NotFound` 且**不在磁盘创建任何目录或锁文件**（完整保留 `ADR-CF-06`）；若锁文件或任一库/清单存在，以只读打开 `config-migration.lock` 执行**有界共享锁 `try_lock_shared_bounded`**（对无写权限的非 root `SystemConfig` 目录，若锁文件尚不存在则只读直查清单与文件存在性不建锁）后完成清单校验与只读打开。
       - **写操作与首次建库（双重检查锁 + 先非覆写 `hard_link` 墓碑预占 + 4 步可恢复发布状态机）：**
         - 若 `config-format.json` 已存在且 `finalized == true` 且 `manifest.backend` 匹配当前运行时，持有界 **`try_lock_shared_bounded`** 完成清单复核与正常写入（允许多进程同后端并发写，并与持 `LOCK_EX` 的迁移严格互斥）。
         - **凡 `config-format.json` 不存在或 `init_phase == "fresh_create"` 的一切写操作（无论 `Sqlite` 还是 `Redb` 运行时），均必须在校验与建库前先获取有界排他锁 `try_lock_exclusive_bounded`（`LOCK_EX`）**，在 `LOCK_EX` 内重新执行校验：
           - **选定新建 `config.db`（`Sqlite`）：** 创建或打开 `config.db` 写入配置后，原子发布 `config-format.json(backend="sqlite", finalized=true, init_phase=null, legacy_sqlite_tombstone=false)` + `fsync_dir` 后释锁（若崩溃于建 `config.db` 后、落清单前，目录仅存合法 `config.db`，下次写入自动自愈补齐清单）。
           - **选定新建 `config.redb`（`Redb`，严格 4 步零覆写可恢复状态机，彻底消除与不持锁旧二进制的竞态）：**
             - **Step F1（建红库前先非覆写 `hard_link` 原子预占 `config.db` 哨兵）：** 先创建并写满固定 31 字节非 SQLite 哨兵 `LIBRA_CONFIG_AUTHORITY_IS_REDB\n` 至 `config.db.tombstone.tmp`、`sync_all()`、设只读权限（`0400` Global / `0444` System，Windows `readonly=true`），随即执行**绝不覆写已存在目标的 `std::fs::hard_link("config.db.tombstone.tmp", "config.db")`**（Unix `link(2)` 与 Windows `CreateHardLinkW` 均在目标存在时原子返回 `AlreadyExists`，**严禁使用会覆盖目标的 `rename`，亦严禁回退为可产生 0 字节窗口的 `create_new`；若底层非常规文件系统不支持硬链返回 `Unsupported`/`ENOSYS`/`EXDEV`，直接清理 `.tmp` 并 fail-closed 报 `LBR-IO-001`**）。**若 `hard_link` 返回 `AlreadyExists`：说明不持锁旧版二进制（`schema.rs:800` `create_new`）刚刚胜出创建了 `config.db`！立即删除 `.tmp`、绝不创建 `config.redb`、绝不覆盖旧库，并 fail-closed 返回 `LBR-CLI-003`（保留旧二进制 100% 写入）**；若 `hard_link` 成功，立刻 `unlink("config.db.tombstone.tmp")` + `fsync_dir`（因为哨兵字节与只读权限在 `hard_link` 前已 `sync_all`，磁盘上永不出现 0 字节可被旧 SQLite 劫持的空 `config.db`！）；
             - **Step F2（建红库前持久化首次创建意图）：** 原子发布 `config-format.json(backend="redb", finalized=false, init_phase="fresh_create", legacy_sqlite_tombstone=true, targets_staged_for_rename=false)` + `fsync_dir`；
             - **Step F3（暂存并原子发布 `config.redb`）：** 在 `config.redb.tmp`（`0600` Global / `0644` System）写入 schema 与首笔配置、`sync_all()`，原子更新 `config-format.json(targets_staged_for_rename=true)` + `fsync_dir`，再执行 `rename("config.redb.tmp", "config.redb")` + `fsync_dir`；
             - **Step F4（终态定稿）：** 原子发布 `config-format.json(backend="redb", finalized=true, init_phase=null, legacy_sqlite_tombstone=true, targets_staged_for_rename=false)` + `fsync_dir` 后释锁。
             - **首次建库全边界幂等崩溃恢复（均在持 `LOCK_EX` 下执行）：**
               - *边界 1（Step F1 `hard_link` 后、Step F2 清单落盘前崩溃）：* `config.db` 存在且内容逐字节等于 `LIBRA_CONFIG_AUTHORITY_IS_REDB\n`、`config.redb` 与清单缺失 → 旧二进制命中 `SQLITE_NOTADB` fail-closed；`Sqlite` 写者报 `LBR-IO-001` 绝不覆盖；后续 `Redb` 写者识别中断的 Step F1，跳过预占直接推进 Step F2–F4 至 `finalized=true`；
               - *边界 2（Step F2 清单 `init_phase="fresh_create", finalized=false, targets_staged_for_rename=false` 后、`rename` 前崩溃）：* 并发读与 `Sqlite` 均报 `LBR-IO-001`；后续 `Redb` 写者核验 `config.db` 哨兵有效、清理残 `.tmp` 并推进 Step F3–F4 至 `finalized=true`；
               - *边界 3（Step F3 `targets_staged_for_rename=true` 且 `config.redb` 已 `rename` 就位、Step F4 `finalized=true` 前崩溃）：* 后续 `Redb` 写者检出有效 `config.redb` 已就位与有效墓碑，**绝不覆写已就位 `config.redb` 数据**，清理过时 `.tmp` 并直接执行 Step F4 发布 `finalized=true` 后再正常追加本次写入（若 `targets_staged_for_rename=true` 但 `config.redb` 仍缺失，则自有效 `config.redb.tmp` 完成 `rename` 或重建至 Step F4）。
       - **离线迁移（`libra-migrate --global/--system`）：** 全程持排他 **`LOCK_EX`**（非阻塞/短超时争用检测），与全部运行时读写严格互斥。
     - **清单前置唯一权威校验（在持上述有界 `LOCK_SH`/`LOCK_EX` 下执行）：**
       - ① 若 `<scope_dir>/config-format.json` 存在且 **`finalized == false`** → 若 `init_phase == "fresh_create"` 且当前是持 `LOCK_EX` 的 `Redb` 写路径则按上述首次建库恢复协议幂等收口；**其余一切情况（全部读操作、全部 `Sqlite` 操作、以及 `init_phase != "fresh_create"` 的离线迁移中间态）无论是否设置 `LIBRA_STORAGE_BACKEND`，一律无条件 fail-closed 报 `LBR-IO-001`**；
       - ② 若 `<scope_dir>/config-format.json` 不存在、但 `config.db` 与 `config.redb` **同时存在**（或仅有 `config.db` 墓碑而当前为只读/`Sqlite` 路径）→ 无条件 fail-closed 报 `LBR-IO-001`（杜绝双文件静默分叉或损坏残态）；
       - ③ 若 `<scope_dir>/config-format.json` 存在且 **`finalized == true`** → 作用域唯一权威后端为 `manifest.backend`（`"sqlite"` 或 `"redb"`）：当 `manifest.backend == "redb"` 且 `legacy_sqlite_tombstone == true` 时，同目录下保留的已围栏/哨兵 `config.db` 墓碑属预期路径围栏（不触发分裂报错，也不回退读旧库）；`manifest.backend == "redb"` 而运行时选 `Sqlite` 时 fail-closed 报 `LBR-CLI-003`；`manifest.backend == "sqlite"` 而运行时选 `Redb` 时按下方分支 B 旧库策略处理，**任何路径绝不把非权威文件作为可写目标打开**；
       - ④ 当 `config-format.json` 不存在且至多存在单一未墓碑化配置库时，按选定运行时后端分派（默认：`RB-01`..`RB-32` 为 `Sqlite`，`RB-33` 切换为 `Redb`；纯单特性二进制固定为所编译后端）：
         - **分支 A — 运行时后端为 `Sqlite`（含 `RB-30`/`RB-32` 默认 SQLite 发行包与纯 `storage-sqlite` 构建）：** 若仅存在 `config.redb` 则 fail-closed 报 `LBR-CLI-003`；否则（存在 `config.db` 或两文件皆空）**100% 保持既有 SQLite 语义**：已有 `config.db` 正常读写，两文件皆空时只读返回 `NotFound` 不建文件（`ADR-CF-06`）、写入在 `LOCK_EX` 下创建并更新 `config.db` 同时原子落盘 `config-format.json(backend="sqlite", finalized=true, legacy_sqlite_tombstone=false)`（绝不创建 `config.redb`，旧版 SQLite 二进制透明忽略 `config-format.json`，对 `RB-33` 前用户零破坏）。
         - **分支 B — 运行时后端为 `Redb`（`RB-29a` 后显式 `LIBRA_STORAGE_BACKEND=redb`、`RB-33` 切默认后，或纯 `storage-redb` 构建）：** `config.redb` 存在且无清单仅在受保护恢复/迁移外拒收并提示修复（确保绝不放过缺墓碑孤立 `config.redb`）；仅存未迁移 `config.db` 时，双特性构建只读透明读取旧 `config.db`、写操作 fail-closed 报 `LBR-CLI-003`（提示 `libra-migrate --global/--system`），纯 `storage-redb` 构建读写均 fail-closed 绝不返回空配置；两文件皆空时只读返回 `NotFound` 不建文件、写入在 `LOCK_EX` 下严格按上述 **Step F1 (`hard_link` 预占墓碑) → Step F2 (`init_phase="fresh_create"`) → Step F3 (`config.redb`) → Step F4 (`finalized=true`)** 建库。
- **Alternatives considered:** 将 `git.redb.gen-<id>` 直接平铺于 `.libra/` 根目录。拒绝：沙箱按子项枚举无法预知未来动态代际名，整目录 `.libra/redb/` 钉死从根本消除竞态。
- **Consequences:** 沙箱并发迁移建代际测试与配置隔离测试全覆盖。
- **Revisit when:** 后续专项权限计划统一重构 `shared_perm.rs` 时（`DEFER-RB-05`）。

### ADR-RB-03: `redb 5.0` `MultiWriter`、严格只读 `RepairAborted` 合同、句柄围栏与资源预算

- **Status:** Accepted
- **Context:** `redb 5.0.0` `Builder::new()` 默认 `ExclusiveWriter` + `1 GiB` 缓存；`SingleWriter` 导致多写进程报 `DatabaseAlreadyOpen`；`open_read_only` 遇未恢复崩溃库返回 `DatabaseError::RepairAborted`；反向迁移需阻断已持打开句柄的旧 `redb` 进程继续写入被废弃 inode。
- **Decision:**
  1. `git.gen-<id>.redb`、`capture.gen-<id>.redb`、`config.redb` 全部使用 **`ConcurrencyMode::MultiWriter`**。
  2. **严格只读 `RepairAborted` 合同（绝不隐式升写）：**
     - 只读命令与只读窗口（`status`、`diff`、`log`、`show`、`grep`、`cat-file`、`rev-parse`、`agent session/status/checkpoint/graph/skill`、不带 `--repair` 的 `doctor`、`src/gui/data` 桌面只读窗口）一律通过 `open_read_only` 打开 `ReadOnlyDatabase`。**只读上下文严禁自动升级为可写修复打开**——若返回 `DatabaseError::RepairAborted`，CLI 必须立即 fail-closed 报 `LBR-IO-001` 并提示运行显式修复命令（`libra fsck` / `libra agent doctor --repair`），桌面窗口映射为明确 `SourceState::Corrupt` 提示而不修改磁盘。
     - **允许触发 `redb` 自动恢复的封闭清单：** 仅限写命令（`commit`、`fetch`、`push`、`merge`、`rebase`、`branch`、`tag`、`config set`、`agent import/clean`、`__capture-queue-drain`）及显式修复命令（`libra fsck`、`libra agent doctor --repair`、`libra-migrate`）。进入前检查写权限（只读挂载直接返回 `LBR-IO-001`），并在 `redb` 多写锁下串行恢复。
  3. **已打开句柄迁移围栏检验（`RB-02b`）：** `Redb*Store` 每次进入 `ReadTransaction` 与 `WriteTransaction` 的首步统一核验 `META_SCHEMA`：`min_reader_version == u32::MAX`（迁移围栏标记）时立即中止事务并返回 `StorageBackendError::RepositoryMigrated`，物理阻断在反向迁移前已打开 fd 的进程继续读写废弃 inode。
  4. **有界写锁与异步隔离：** 写事务统一经 `begin_write_bounded(deadline)` 在 `tokio::task::spawn_blocking` 内循环 `db.try_begin_write()`（`1ms → 25ms` 抖动退避至 deadline），`WriteTransaction::commit()` 亦在 `spawn_blocking` 中执行。
  5. **缓存封顶：** 只读 CLI/GUI `16 MiB`、普通写 CLI `32 MiB`、长驻 `agent bridge`/`daemon`/消费者 `64 MiB`。
- **Alternatives considered:** 只读遇 `RepairAborted` 自动升写或仅靠 `unlink` 期望已打开进程自退。拒绝：违反只读不写盘契约且 POSIX `unlink` 不阻断已持 fd 写者。
- **Consequences:** 崩溃修复边界与反向迁移句柄围栏均可确定性单测实证。
- **Revisit when:** `redb` 提供原生 `begin_write_timeout` 时。

### ADR-RB-04: 对象安全存储 Trait、版本化 `postcard` 编码与 `GC-13` 账本

- **Status:** Accepted
- **Context:** 业务域涵盖 61+25+3 张活跃源表，trait 须对象安全（`Arc<dyn GitStore>` 等），且 `postcard` 无自描述字段名；此外 `agent_bridge_publication` 必须在移动 `HEAD` 的同一仓库事务内落盘（`src/internal/ai/agent_bridge/publication.rs:5-8`）。
- **Decision:**
  1. 模块落位 `crate::storage`（`src/storage/`），暴露 `Arc<dyn ConfigStore>`、`Arc<dyn GitStore>`、`Arc<dyn CaptureStore>`；多步原子写入建模为语义化批/CAS 方法（含 `GitStore::update_ref_cas_with_bridge_publication` 同事务原子绑定 `HEAD` + `reflog` + `agent_bridge_publication`）。
  2. 所有 `redb` Value 编码为 `[schema_ver: u16_le][postcard_payload]`（`src/storage/codec.rs`）。
  3. 复合有序键与二级索引、唯一约束、墓碑拒绝、审计只追加均在 `Redb*Store` 同一个 `WriteTransaction` 内维护。
  4. 每个 `.redb` 文件内置 `META_SCHEMA` 表核验 `(role, format_version, min_reader_version)`；`capture.redb` 过新仅报 `LBR-AGENT-048` 而不阻断 `git.redb`。
- **Alternatives considered:** Value 存 JSON 或将 `agent_bridge_publication` 放进第二事务。拒绝：JSON 体积与 CPU 开销高；拆分事务破坏桥接提交绑定原子性。
- **Consequences:** 结构体演进须递增 `schema_ver` 并附跨版本解码单测。
- **Revisit when:** 出现跨多表动态即席查询需求时。

### ADR-RB-05: Capture 双通道 CRC32C 帧日志（共享容量锁 + 尾损预修 + 持锁刷盘 + 全量去重键 + 原子高水位）

- **Status:** Accepted
- **Context:** 需同时保证：① 多进程并发追加绝不交错；② 跨 `priority/` 与 `normal/` 两个通道的**合并未消费总字节（含本次待写帧长度）**在并发竞争下绝不越过 `256 MiB` 硬上限；③ 进程被 `SIGKILL` 留下半截尾帧后，后续生产者绝不在半帧之后继续追加而造成中段损坏；④ 入队返回 0 即具持久性；⑤ 语义去重键零碰撞；⑥ `SessionEnd` 终态收条严格优先于普通事件（`plan-20261116` `ADR-CQ-05`），且不破坏高水位的严格单调连续性。
- **Decision:**
  1. **双通道目录 + 单一共享容量/追加锁：** `.libra/private/capture-queue/` 下分设 `priority/`（承载 `SessionEnd` 等终态收条）与 `normal/`（承载其它事件）以及 `quarantine/`，各自维护独立 `LTM2` 双副本提交边界元数据 `tail.meta` 与 `tail.meta.bak`（最小未回收段号 `min_live_seg`、当前活跃段号 `active_seg`、活跃段已提交有效字节长度 `committed_len`、已确认前缀累计校验和 `prefix_crc32c`，以及全部未回收轮转段 `min_live_seg <= seg < active_seg` 的逐段认证边界 `sealed[] = [(seg_id, sealed_len, sealed_crc32c)]`）；**双通道共用唯一的 `.libra/private/capture-queue/append.lock`** 串行化跨通道容量预留、前缀校验、崩溃恢复与段追加，杜绝双通道并发竞态穿透总上限。
  2. **16 字节自认证帧头 + 全帧双 CRC32C 线协议、分配前界限护卫与全量去重键：** 使用 `crc32c 0.6.9`（Castagnoli 多项式，标准向量 `crc32c(b"123456789") == 0xe3069283`）。在锁外完成脱敏（`GC-CQ-03`）并组装单块连续缓冲：
     `[MAGIC: 4B = b"LCE1"][FRAME_BODY_LEN: u32_le][HEADER_CRC32C: u32_le][FRAME_CRC32C: u32_le][FLAGS: u8][DEDUP_KEY_LEN: u16_le][DEDUP_KEY_UTF8: dedup_len B][POSTCARD_ENVELOPE: rest B]`（固定 16 字节头 + 变长体 `FRAME_BODY_LEN` 字节，约束 `3 <= FRAME_BODY_LEN <= MAX_FRAME_BODY_LEN` 其中 `MAX_FRAME_BODY_LEN = 3 + 512 + 4 MiB`，单负载上限 `4 MiB`，去重键上限 `512 B`）：
     - **`HEADER_CRC32C`（字节 `8..12`）：** Castagnoli CRC32C 覆盖前 8 字节 `[MAGIC || FRAME_BODY_LEN]`。解码器在**分配任何体缓冲或执行恢复截断之前**，先读取固定 16 字节头，立即核验 `MAGIC == b"LCE1"`、`crc32c(hdr[0..8]) == HEADER_CRC32C` 及 `3 <= FRAME_BODY_LEN <= MAX_FRAME_BODY_LEN`；任一字节（含 `MAGIC`、`FRAME_BODY_LEN` 或 `HEADER_CRC32C`）被篡改立即返回 `FrameDecodeError::CorruptHeader`，**彻底杜绝篡改帧长被误判为段尾半帧截断或触发超大堆分配（OOM）**。
     - **`FRAME_CRC32C`（字节 `12..16`）：** Castagnoli CRC32C 覆盖全帧非校验字段 `[MAGIC || FRAME_BODY_LEN || HEADER_CRC32C || FLAGS || DEDUP_KEY_LEN || DEDUP_KEY_UTF8 || POSTCARD_ENVELOPE]`。仅当 `HEADER_CRC32C` 与长度界限均通过且文件剩余字节 ≥ `FRAME_BODY_LEN` 时才分配读入体并校验 `FRAME_CRC32C`（不符报 `FrameDecodeError::CorruptPayload`）；仅当头校验合法但剩余字节 `< FRAME_BODY_LEN`（或文件尾仅余 `1..15` 字节）时才归类为 `FrameDecodeError::TruncatedTail`，消除 16 字节哈希截断碰撞与头字段未受保护风险。
     - **已确认前缀损坏与未提交尾截断严格区分：** 生产者恢复扫描器与消费者必须参照已验 `tail.meta`/`tail.meta.bak` 所确立的 `(active_seg, committed_len, prefix_crc32c)`：仅当提交边界已被合法元数据确立且坏/半截字节严格位于 `offset >= committed_len`（或 `seg > active_seg` 的未提交后继孤儿段）时，才允许将未提交尾残片隔离至 `quarantine/` 并截断/切新段；**对任何已确认前缀（`offset < committed_len` 或已轮转旧段 `seg < active_seg`）检出 `prefix_crc32c` 失配、`actual_len < committed_len`、`CorruptHeader`、`CorruptPayload` 或 `TruncatedTail`，或者在双副本均损毁无法确立提交边界时检出任何坏帧或截断尾字节，均立即归类为 `CorruptCommittedFrame`（fail-closed 返回 `LBR-IO-001` 且零追加、零截断、零丢弃，完整保留原段供显式 `libra agent doctor --repair` 人工取证）**。
  3. **跨通道原子容量守恒 + `LTM2` 双副本（活跃前缀 + 全量未回收轮转段认证边界）+ 0 段四分法 + 入队前强制校验与持锁耐久提交：**
     - **`LTM2` 双副本 `tail.meta` / `tail.meta.bak` 校验格式、全新空通道预建与原子替换协议：** 每通道维护主副两份完全同构的 `LTM2` 校验结构（长度 `34 + 16 * SEALED_COUNT` 字节）：
       - **30 字节定长头：** `[MAGIC: 4B = b"LTM2"][VERSION: u16_le = 2][MIN_LIVE_SEG: u32_le][ACTIVE_SEG: u32_le][COMMITTED_LEN: u64_le][PREFIX_CRC32C: u32_le][SEALED_COUNT: u32_le]`（约束 `MIN_LIVE_SEG <= ACTIVE_SEG`、`SEALED_COUNT == ACTIVE_SEG - MIN_LIVE_SEG` 且 `SEALED_COUNT <= 256`；`PREFIX_CRC32C` 为活跃段已确认前缀 `events-<ACTIVE_SEG>.log[0..COMMITTED_LEN]` 的 Castagnoli CRC32C，空段初值为 `0`，每次追加按 `crc32c::crc32c_append(prev_prefix_crc, &frame_buf)` 增量推进，轮转新段归零重算）；
       - **未回收轮转段认证表（`SEALED_COUNT × 16` 字节，严格按 `SEG_ID = MIN_LIVE_SEG + i` 升序覆盖全部 `MIN_LIVE_SEG <= SEG_ID < ACTIVE_SEG`）：** 每项为 `[SEG_ID: u32_le][SEALED_LEN: u64_le][SEALED_CRC32C: u32_le]`，持久化记录该轮转段封存时的精确有效字节长度与全段 Castagnoli CRC32C（消除轮转段在精确完整帧边界被截断仍呈 `CleanEof` 而无法检出的漏洞）；
       - **4 字节尾校验：** `[META_CRC32C: u32_le]` 覆盖前述全部 `30 + 16 * SEALED_COUNT` 字节（解码强验文件总长精确等于 `34 + 16 * SEALED_COUNT`、魔数/版本、`SEALED_COUNT == ACTIVE_SEG - MIN_LIVE_SEG`、表项 `SEG_ID` 严格连续及 `META_CRC32C`）。
       - **严禁就地覆写；双副本全序收敛：** 更新时必须先写 `tail.meta.tmp`（`O_NOFOLLOW`、`0600`）→ `sync_data()` → `fs::rename("tail.meta.tmp", "tail.meta")` → `fsync_dir(lane_dir)`，再镜像写 `tail.meta.bak.tmp`（`O_NOFOLLOW`、`0600`）→ `sync_data()` → `fs::rename("tail.meta.bak.tmp", "tail.meta.bak")` → `fsync_dir(lane_dir)`——**两份副本全部落盘并 `fsync_dir` 后才释放 `append.lock` 并向调用方返回 `Ok(())`**。当两份副本均有效时，按字典序 **`(active_seg, committed_len, min_live_seg)`** 取较大者作为权威边界并先镜像同步落后副本（该三元组对一切生产者追加/轮转/恢复与消费者推进 `min_live_seg` 回收均构成严格全序）。
       - **全新空通道预建不变量（杜绝首次入队崩溃触发双缺）：** `init_lane()` 与全新空通道首次入队前（**仅当目录中 `events-*.log` 文件数为 `0` 且 `tail.meta` 与 `tail.meta.bak` 均不存在于磁盘**时），必须在创建或写入首段 `events-00000000.log` **之前**，按上述原子替换协议先落盘并 `fsync_dir(lane_dir)` 初始 34 字节 `LTM2` `(min_live_seg=0, active_seg=0, committed_len=0, prefix_crc32c=0, sealed=[])` 双副本。借此数学保证：任意正常单次或连续崩溃中至少恒有 1 份有效副本存在（归入 Case C/D 或 Subcase B1），绝不产生双副本同时缺失的合法崩溃态。
     - 生产者持共享 `capture-queue/append.lock`（等待受 `min(hook_deadline, 30s)` 约束，超时报 `LBR-AGENT-049`）：
       - ① **先 100% 只读全状态分类（含 Case D 后继预扫描）、确认合法可恢复后再执行本通道限定清理与统一恢复（每次入队前必经，绝不跳过已确认前缀校验）：** **第一阶段（严格 100% 只读分类，严禁执行任何删除、写入、截断、副本镜像同步或孤儿摘除）：** 仅以只读方式扫描目标通道 `<lane>` 目录获得现存 `events-*.log` 列表，用 `O_NOFOLLOW` 只读探测并校验 `tail.meta` / `tail.meta.bak`、校验活跃段存在性与 `[0, committed_len)` `prefix_crc32c`；**且当 `max_seg > active_seg`（Case D）时，第一阶段须在触碰磁盘前先以只读方式扫描后继段 `active_seg + 1 ..= max_seg`（从偏移 `0` 起），核验是否属于 `meta.sealed.len() >= MAX_SEALED_SEGMENTS`（`256`）却存在含 ≥ 1 条完整合法帧后继段的非法破坏态**。**凡第一阶段命中任意 fail-closed 损坏态（情况 `0c`、情况 `0d`、非空 `Subcase B2`、`Case E`、`Case A`/`C`/`D` 已确认前缀长度或 CRC 失配，或 `Case D` 满 256 封存表却含有效后继段），必须立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严格整盘零改动——原样保留目标通道与共享 `quarantine/` 全部现存文件（含未截断旧段 `events-<active_seg>`、落后/损坏元数据、已回收孤儿及一切遗留 `tail.meta*.tmp`、`quarantine/<lane>-*.tmp`）供人工取证！** **第二阶段（仅当第一阶段只读分类确认目标通道 `<lane>` 合法可初始化或可恢复之后）：** 方可执行**严格限定于本通道 `<lane>` 的**清理动作——仅清理 `<lane>/tail.meta*.tmp` 与 **`quarantine/<lane>-*.tmp`**（**严禁触碰或删除另一通道的 `quarantine/<other_lane>-*.tmp` 或未归属本通道的临时文件**），继而执行下述合法初始化、副本镜像修复、已回收孤儿摘除与 Case C/D 恢复动作：
         - **情况 0（目录中 `events-*.log` 文件数为 `0` —— 严格四分法，绝不将唯一活跃段被删伪装为空通道）：**
           - **(0a) 真正全新空通道（`tail.meta` 与 `tail.meta.bak` 均不存在于磁盘 `ENOENT`）：** 按上述预建协议初始化 34 字节 `LTM2` `(0, 0, 0, 0, [])` 双副本 + `fsync_dir`，然后允许创建 `events-00000000.log`；
           - **(0b) 已预建但尚未提交首帧（至少 1 份有效 `LTM2` 且权威副本精确满足 `min_live_seg == 0 && active_seg == 0 && committed_len == 0 && prefix_crc32c == 0 && sealed.is_empty()`）：** 证明历史上从未确认任何帧；若缺/损 1 份副本则按 Subcase B1 镜像修复后允许正常创建 `events-00000000.log`；
           - **(0c) 唯一活跃段或已确认段缺失（至少 1 份有效 `LTM2` 且 `committed_len > 0 || active_seg > 0 || !sealed.is_empty()`）：** 证明存在已被确认的段文件却在磁盘上缺失，**直接归入 Case E 无条件 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）：严禁重置元数据为 `(0,0,0,0,[])`、严禁追加、零磁盘改动**；
           - **(0d) 0 段但存在损坏元数据（`tail.meta` 或 `tail.meta.bak` 至少 1 份存在于磁盘，且两份均非有效 `LTM2`）：** 无法证明通道为空，**无条件 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）：严禁重置元数据、严禁追加、零磁盘改动**；
         - **存在 ≥ 1 个 `events-*.log` 文件时：** 第一阶段只读确定权威 `LTM2`（双副本皆有效时只读取最高 `(active_seg, committed_len, min_live_seg)`；恰有 1 份有效归 **Subcase B1**；双副本均缺失/损坏直接归 **Subcase B2** fail-closed 零磁盘改动），并对存活段 `live_segs`（`seg >= min_live_seg`，记最高段号 `max_seg`）完成前述全部只读分类校验（排除 `Case E`、已确认前缀失配及 `Case D` 满 256 表有效后继）。仅当第一阶段确认合法可恢复后，第二阶段方按序清理本通道 `<lane>/tail.meta*.tmp` 与 `quarantine/<lane>-*.tmp`、镜像同步落后或缺失副本（`Subcase B1`）、安全摘除已回收崩溃孤儿段（`seg < min_live_seg`：`unlink` + `fsync_dir`），并执行对应 **情况 A / C / D** 动作：
           - **情况 B1（单副本缺失或损坏，另一副本完整有效）：** 第一阶段从完整副本只读确立权威 `LTM2` 并先完成 `Case E`/前缀/`Case D` 满表只读检验（任一失败立即 fail-closed 零磁盘改动）；确认合法后于第二阶段按原子替换协议镜像修复损坏/缺失副本并继续执行 **情况 A / C / D**（单副本损毁绝不丢失提交边界或轮转封存表）；
           - **情况 B2（存在 ≥ 1 个 `events-*.log` 文件且 `tail.meta` 与 `tail.meta.bak` 双副本同时缺失或损坏）：** 缺乏独立持久提交边界，任何按文件扫描的自动重建都无法区分「真实完整段」与「在先前完整帧边界被静默截断（仍为 `CleanEof`）或活跃段被截断为 0 字节的受损段」。鉴于空通道预建不变量保证正常崩溃绝不会导致两份已 `fsync_dir` 副本同时损毁，**Subcase B2 一律无条件 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）：严禁自动重建元数据、严禁截断、严禁隔离删除、严禁继续追加（无论现存段是否 `CleanEof`、是否在完整帧边界截断、是否活跃段被截断为 0 字节、还是 `max_seg`/较旧轮转段存在坏字节），完整保留磁盘原状供显式 `libra agent doctor --repair` 人工取证修复**；
           - **情况 E（活跃段缺失、短于或晚于已确认边界：`events-<active_seg>.log` 不存在、`live_segs` 为空且 `committed_len > 0 || active_seg > 0`、`max_seg < active_seg`，或 `actual_len(active_seg) < committed_len`）：** 无论元数据是否变化，均立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），**零重置、零追加、零截断**；
           - **情况 A（正常路径：`max_seg == active_seg` 且 `actual_len(active_seg) == committed_len`）：** 在进入步骤 ② 容量检查**之前**，必须对已确认前缀 `active_seg[0..committed_len]` 计算硬件加速 Castagnoli `crc32c` 并校验 **`== meta.prefix_crc32c`**（16 MiB 单段上限下硬件 CRC32C 耗时约 ≤ 1 ms，常规 KB 级段 < 0.05 ms，完全落在 `5ms` 中位 / `50ms` 硬预算内）；**若失配，立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），零追加、零截断**，校验通过后方进入步骤 ②；
           - **情况 C（同段未提交尾增长：`max_seg == active_seg` 且 `actual_len(active_seg) > committed_len` —— 两步耐久提交 + 受 `MAX_SEALED_SEGMENTS = 256` 保护的可选切段）：** **必须先验证已确认前缀 `crc32c(active_seg[0..committed_len]) == meta.prefix_crc32c`**（失配立即 fail-closed 报 `CorruptCommittedFrame` `LBR-IO-001`，零追加、零截断），通过后再从 `committed_len` 前向扫描校验全部连续完整 CRC32C 合法帧至 `recovered_valid_end >= committed_len`（**100% 保留已 `sync_data` 完整合法帧，绝不触碰 `[0, committed_len)`**），计算 `recovered_crc32c = crc32c_append(meta.prefix_crc32c, active_seg[committed_len..recovered_valid_end])`：
             - **Step C1（先断电持久发布隔离副本、再原地截断清理与 `LTM2` 边界先落盘，绝不阻塞消费排空或恢复）：** 若 `recovered_valid_end < actual_len`，**必须经强约束原语 `quarantine_and_truncate_segment` 先调用 `publish_quarantine_durable`** 将 `[recovered_valid_end, actual_len)` 以确定性键名写入 `quarantine/<lane>-seg<S>-off<offset>-<crc32c>.tmp`（`0600`、`O_NOFOLLOW`）→ `tmp.sync_data()` → 原子 `fs::rename` 至 `.bin` → `fsync_dir(quarantine_dir)`（并记录 `LBR-AGENT-050`；仅在合法可恢复通道第二阶段清理本通道遗留 `quarantine/<lane>-*.tmp`，确定性键名保证重放幂等覆盖零重复膨胀），**仅在 `fsync_dir(quarantine_dir)` 成功返回确保隔离取证副本断电持久之后**，才允许将 `events-<active_seg>.log` 截断至 `recovered_valid_end` + `sync_data()`；无论是否有残尾，立即按原子替换协议提交双副本 `LTM2` 更新 **`(ACTIVE_SEG = active_seg, COMMITTED_LEN = recovered_valid_end, PREFIX_CRC32C = recovered_crc32c)`**（**`sealed.len()` 不变**；若 `recovered_valid_end == actual_len` 则零隔离、零 `LBR-AGENT-050` 且至此结束）；
             - **Step C2（条件切段 —— 严禁生成 0 字节封存段或突破 `MAX_SEALED_SEGMENTS = 256`）：** 仅当曾发生残尾截断（`recovered_valid_end < actual_len`）**且 `recovered_valid_end > 0` 且 `meta.sealed.len() < MAX_SEALED_SEGMENTS`（`256`）** 时，才创建新段 `events-<active_seg+1:08>.log`（`0600`）+ `fsync_dir(lane_dir)` 并原子提交双副本 `LTM2` 向 `sealed[]` 追加 `(active_seg, recovered_valid_end, recovered_crc32c)` 且推进 `ACTIVE_SEG = active_seg + 1, COMMITTED_LEN = 0, PREFIX_CRC32C = 0`。**当 `recovered_valid_end == 0`（全新段首帧即崩溃）或 `meta.sealed.len() == 256` 时，严格停在 Step C1 并跳过 Step C2 轮转**：段 `active_seg` 已 100% 洁净截断并获 `LTM2` 认证，既绝不产生 0 字节封存段或第 257 条 `sealed[]` 表项，亦绝不阻塞消费端排空与后续合法入队；
           - **情况 D（轮转孤儿后继段：`max_seg > active_seg` —— 第一阶段先只读验证旧段前缀并预扫后继判定满表合法性，第二阶段再恢复旧段完整尾并封存）：** **第一阶段（100% 只读，零任何写/删/截断）：** ① 先验证旧段 `active_seg` 满足 `actual_len(active_seg) >= committed_len` 且 `crc32c(active_seg[0..committed_len]) == meta.prefix_crc32c`（不满足立即 fail-closed 报 `CorruptCommittedFrame` `LBR-IO-001`，整盘零改动）；② 再只读扫描未提交后继段 `active_seg + 1 ..= max_seg`（从偏移 `0` 起）：**若后继段含 ≥ 1 条完整 CRC32C 合法帧且 `meta.sealed.len() >= MAX_SEALED_SEGMENTS`（`256`，合规写者在创建 `S+1` 前必已拦截，属非法破坏态），第一阶段即直接 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严禁清理 `.tmp`、严禁隔离或截断旧段未提交尾、严禁修改任何磁盘文件！** **第二阶段（第一阶段确认可恢复后，先清理本通道 `<lane>/tail.meta*.tmp` 与 `quarantine/<lane>-*.tmp`）：** 在确定旧段封存边界前先核查 `actual_len(active_seg)`：若 `actual_len(active_seg) > committed_len`（嵌套 Case C→D），按上述 Step C1 前向恢复 `[committed_len, actual_len(active_seg))` 至 `old_valid_end`（若含损坏残尾则经 `quarantine_and_truncate_segment` 先完成 `publish_quarantine_durable` 含 `sync_data` + `rename` + `fsync_dir(quarantine_dir)` 后再截断 + `sync_data()` 并提交 Step C1 `LTM2`），计算真实封存边界 **`(old_valid_end, old_sealed_crc)`**（**严禁用旧 `committed_len` 封存已保留完整尾帧的旧段 `S`！**）；若 `actual_len(active_seg) == committed_len` 则 `(old_valid_end, old_sealed_crc) = (committed_len, meta.prefix_crc32c)`。随后按第一阶段已确认的后继分类落盘：
             - 若后继段为空或仅有未完整半帧：对非空半帧残字节**必须经强约束原语 `quarantine_and_unlink_orphan` 先调用 `publish_quarantine_durable`**（`.tmp` → `sync_data()` → 原子 `rename` 至 `.bin` → `fsync_dir(quarantine_dir)`，确保断电持久），随后再执行 `unlink` 删除空/损孤儿段并 `fsync_dir(lane_dir)`，最后原子同步双副本 `LTM2` 至 `(ACTIVE_SEG = active_seg, COMMITTED_LEN = old_valid_end, PREFIX_CRC32C = old_sealed_crc)`（不封存旧段、`sealed.len()` 不变）；
             - 若后继段含 ≥ 1 条完整 CRC32C 合法帧（此时第一阶段已保证 `meta.sealed.len() < 256`）：**100% 保留后继段全部已同步合法帧**，仅对其后未提交半帧经 `quarantine_and_truncate_segment` 先完成 `publish_quarantine_durable`（`.tmp` → `sync_data()` → `rename(.bin)` → `fsync_dir(quarantine_dir)`）后再截断并 `sync_data()`，随后在双副本 `LTM2` 原子提交中以真实认证边界向 `sealed[]` 登记旧段 **`(active_seg, old_valid_end, old_sealed_crc)`** 同时推进 `ACTIVE_SEG = S + 1` 与新 `COMMITTED_LEN`/`PREFIX_CRC32C`；
       - ② **新帧预写容量与 `MAX_SEALED_SEGMENTS = 256` 双背压守卫（紧接 Step ① 合法恢复完成后、为本次待入队新帧修改任何磁盘文件之前执行）：** **（Step ① 与 Step ② 磁盘变更边界明确划分：Step ① 对既有单副本缺失、已回收孤儿段或未提交崩溃半帧执行并持久化的合法恢复变更允许且必须在进入 Step ② 前完成，以确保队列对消费端恢复洁净可排空；随后 Step ② 基于恢复后的洁净态对本次待入队新帧做预写守卫：若被拒收，严禁为本次新帧写入任何字节、严禁创建 `S+1`、严禁在恢复后再次修改 `events-<S>` 或 `tail.meta`/`tail.meta.bak`；特别地，对本已洁净的 Case A 通道，全调用严格零磁盘改动。）**(a) 精确计算 `unconsumed_priority_bytes + unconsumed_normal_bytes + quarantine_bytes + frame_buf.len()`，若 **`> 256 MiB`**（严格计入本次待入队帧长），立即 fail-closed 返回可恢复背压 `LBR-AGENT-046` 且对本次新帧零字节写入；(b) 若本次写入将触发 16 MiB 轮转（`committed_len + frame_buf.len() > 16 MiB`）**且 `meta.sealed.len() >= MAX_SEALED_SEGMENTS`（`256`）**，在为本次新帧创建 `S+1` 或修改任何段/元数据文件**之前**立即 fail-closed 返回可恢复背压 **`LBR-AGENT-046`**（本已洁净的 Case A 通道全调用零磁盘改动；若同次调用在 Step ① 先完成了满表 Case C Step C1 尾损恢复，则仅保留 Step ① 合法恢复结果而绝不为本次被拒新帧写盘或建 `S+1`；待消费者排空并回收 ≥ 1 个已封存段后立即自动恢复正常轮转）；
       - ③ 若 `committed_len + frame_buf.len() > 16 MiB`（此时步骤 ② 已保证 `meta.sealed.len() < 256`）则创建新段 `events-<active_seg+1:08>.log`（`0600`）+ `fsync_dir(lane_dir)`，在步骤 ④ 随新段首帧落盘提交双副本 `LTM2` 时同步将旧段 `(active_seg, old_committed_len, old_prefix_crc32c)` 原子写入 `sealed[]` 并推进 `ACTIVE_SEG = active_seg + 1`（新段 `prefix_crc32c` 由 `0` 重算）；
       - ④ 执行单次 `write_all(&frame_buf)` + **`seg.sync_data()`**，随后按上述双副本原子替换协议依次提交 `tail.meta` 与 `tail.meta.bak`（均 `sync_data` + `rename` + `fsync_dir(lane_dir)`）后**才释放 `append.lock` 并返回 `Ok`**。
  4. **单写入者双通道原子高水位、未回收轮转段 `LTM2.sealed[]` 认证校验、严格锁序 `writer.lock → capture-queue/append.lock` 协调与批量提交：** 唯一持 `writer.lock` 的消费者每次循环**先将 `priority/` 排空至已确认边界，再从 `normal/` 取一批（≤ 128 条或 `50ms` 窗口）**，在同一 `CaptureStore::append_capture_batch` 写事务内批量落库、按完整语义 `dedup_key` 幂等去重，并**唯一地在该数据库写事务内原子更新库内 `QUEUE_HIGHWATER_PRIORITY` / `QUEUE_HIGHWATER_NORMAL`**；事务 `commit()` 后镜像写入外部 `highwater` 文件。**与活跃生产者的无竞态锁序协调（杜绝误隔离在途提交、跳帧与段回收竞态）：** 严格遵循全局锁序 **`writer.lock → capture-queue/append.lock`**（与反向迁移全局锁序 `migration.lock → maintenance.lock → capture-spool-consumer.lock → writer.lock → capture-queue/append.lock → capture-spool/capacity.lock → (git_wtx, cap_wtx)` 100% 一致；生产者永远只取 `append.lock`、绝不取 `writer.lock`，故数学无死锁；消费者亦绝不在持有 `append.lock` 期间打开 `CaptureStore` 写事务以免阻塞 Hook）：
     - ① **常规消费强验 `LTM2`、全部未回收轮转段认证边界与高水位单调性：** 消费者在仅持 `writer.lock` 时，先读并校验 `tail.meta`/`tail.meta.bak`（若存在 ≥ 1 段文件且双副本同损、或 `0` 段但存在非零/损坏元数据、或通道元数据相对库内 `QUEUE_HIGHWATER_*` 发生倒退 `active_seg < hw_seg || (active_seg == hw_seg && committed_len < hw_offset)`，均立即 fail-closed 返回 `CorruptCommittedFrame` `LBR-IO-001`）。对每一个尚未消费完毕且未回收的已轮转段 `seg`（`min_live_seg <= seg < active_seg`），消费前必须核对 `LTM2.sealed[seg]`：文件必须存在、**`actual_len(seg) == sealed_len`** 且 **`crc32c(seg[0..sealed_len]) == sealed_crc32c`**——任何缺失、位翻转、尾部额外字节或**精确停在先前完整帧边界的静默截断（即使解码呈 `CleanEof`）**均立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`，绝不隔离丢弃或推进高水位）。对活跃段 `active_seg` 常规消费只读已确认前缀 `[0, committed_len)`；**未持有 `capture-queue/append.lock` 前，消费者严禁将 `offset >= committed_len` 的尾字节视作崩溃残帧、严禁隔离至 `quarantine/`、严禁跳过推进高水位**。
     - ② **未提交尾恢复必先获 `append.lock` 重验并复用生产者 Case C/D 恢复（绝不于 DB 提交前推进高水位），且常规消费严格按段状态与批边界推进高水位：** 当消费者观测到 `active_seg` 存在 `actual_len > committed_len`（或 `max_seg > active_seg`）时，必须按 `writer.lock → capture-queue/append.lock` 获取 `append.lock`，在锁内重新读取并校验 `LTM2` 与文件长度：若此前系活跃生产者正在提交，获取 `append.lock` 将等待其完成 `sync_data` 与双副本提交后释锁，重读即可见到已推进的新 `committed_len` 并正常消费（**零误隔离、零跳帧、零 `LBR-AGENT-050`**）；若在**已持 `append.lock`**（证明当前无活跃生产者）后仍存在真崩溃未提交尾或孤儿后继段，则调用与上述 #3 完全一致的 **生产者 Case C（Step C1 + 条件 Step C2）/ Case D 恢复例程**（先验 `[0, committed_len)` `prefix_crc32c` → 100% 保留全部完整 CRC32C 已同步尾帧至 `recovered_valid_end` → Step C1 经 `quarantine_and_truncate_segment` 先完成 `publish_quarantine_durable` 断电持久隔离 `[recovered_valid_end, actual_len)` 并记 `LBR-AGENT-050`，再截断 `S` 至 `recovered_valid_end` + `sync_data()` 并先提交 `LTM2` 更新 `S` 边界 → 仅当曾截断且 `recovered_valid_end > 0 && sealed.len() < 256` 执行 Step C2 封存切段 `S + 1`；若为纯已同步尾 `recovered_valid_end == actual_len`、`sealed.len() == 256` 或 `recovered_valid_end == 0` 则停在 Step C1 保持 `ACTIVE_SEG == S` → **全程绝不修改或推进库内 `QUEUE_HIGHWATER_*` 或外部 `highwater` 镜像**），随即释放 `append.lock`。此后正常消费循环从既有库内 `QUEUE_HIGHWATER`（如 `(S, hw_offset)`）按每批 ≤ 128 帧继续读取（若 `S < active_seg` 先验 `LTM2.sealed[S]`，若 `S == active_seg` 只读 `[hw_offset, committed_len)`），并在每次 `CaptureStore::append_capture_batch` 写事务内遵循**统一原子高水位推进规则**（设本批结束偏移为 `batch_end_offset`）：**(a) 活跃段不跳段不变量（`S == active_seg`，含常规消费、纯已同步尾恢复、满 256 封存表 Step C1 原地恢复与 0 字节恢复）：** 无论本批是否已追平 `committed_len`（含多批 `> 128` 帧消费的每一批，或 `committed_len == 0` 时无需写库保持 `(S, 0)`），库内 `QUEUE_HIGHWATER_*` **严格只推进至 `(S, batch_end_offset)`（最高停在 `(S, committed_len)`），绝不推进至 `(S + 1, 0)`**，确保后续追加至活跃段 `S` 偏移 `committed_len` 的新帧被无缝连续消费，零跳帧且绝不触发高水位倒退误报；**(b) 已封存段未耗尽多批与末批跨段不变量（`S < active_seg` 且 `hw_offset < sealed_len(S)`，由 `LTM2.sealed[S]` 认证 `sealed_len(S)`）：** 当单段未消费积压帧与已恢复完整同步尾帧合计超过 `128` 帧分多批落库时，所有中间批次（`batch_end_offset < sealed_len(S)`）均原子推进至 `(S, batch_end_offset)`；**仅当 `S < active_seg` 且最终批次精确消费至 `batch_end_offset == sealed_len(S)` 时，才在该最终 `CaptureStore::append_capture_batch` 写事务提交时原子推进 `QUEUE_HIGHWATER_*` 至 `(S + 1, 0)`**；**(c) 已排空活跃段随后被轮转的原子零帧跨段不变量（`S < active_seg` 且 `hw_offset == sealed_len(S)`）：** 若消费者先前已将活跃段 `S` 排空停于 `(S, committed_len)`、随后生产者经 16 MiB 常规轮转或 Case C Step C2 将 `S` 封存并推进至 `S + 1`，导致消费者后续观测到 `S < active_seg` 且 `hw_offset == sealed_len(S)`（旧段已无剩余帧可组成数据批次），消费者**必须先完整核验 `LTM2.sealed[S]`（`actual_len(S) == sealed_len(S)` 且 `crc32c(S[0..sealed_len(S)]) == sealed_crc32c(S)`；任一失配或 `hw_offset > sealed_len(S)` 立即 fail-closed 返回 `CorruptCommittedFrame` `LBR-IO-001` 且绝不推进高水位）**，核验通过后在单次写事务内执行**原子零帧推进 `CaptureStore::append_capture_batch(&[], next_highwater = (S + 1, 0))`** 将库内 `QUEUE_HIGHWATER_*` 由 `(S, sealed_len(S))` 推进至 `(S + 1, 0)` 并镜像写入外部 `highwater`（提交前崩溃重启幂等重验并重提交，提交后镜像前崩溃以库内 `(S + 1, 0)` 为权威直接消费 `S + 1` 并按 ③ 回收 `S`，彻底杜绝排空后轮转卡死旧段与背压堆积）。若在已确认前缀（`offset < committed_len`）或已轮转历史段（`seg < active_seg`）检出任何损坏/截断/长度或 CRC 失配，立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
     - ③ **历史段崩溃安全回收必先获 `append.lock` 并先更新 `LTM2` 再 `unlink`：** 消费者回收已完全越过的历史段 `seg`（满足 `seg == min_live_seg && seg < active_seg && seg < highwater_seg`）前，必须按 `writer.lock → capture-queue/append.lock` 获取 `append.lock`，在锁内复核条件后：① **先按原子替换协议更新 `tail.meta` 与 `tail.meta.bak` 将 `MIN_LIVE_SEG` 推进至 `seg + 1` 并从 `sealed[]` 摘除 `seg`**；② **再执行 `unlink("events-<seg:08>.log")` + `fsync_dir(lane_dir)`** 并立即释放 `append.lock`。若崩溃于 ① 后 ② 前，重启时 `seg < min_live_seg` 明确标识为已回收孤儿文件可安全清理，绝不会在 `sealed[]` 仍登记 `seg` 时提前删文件触发误报，亦彻底杜绝与生产者容量统计/目录扫描竞态。
- **Alternatives considered:** 每通道独立 `append.lock` 且不计本次帧长。拒绝：并发 `priority` 与 `normal` 生产者可双双通过检查并突破 `256 MiB` 硬上限。
- **Consequences:** 合并容量硬上限、终态优先、连续高水位与尾损防护同时满足。
- **Revisit when:** 单机需跨仓库合并守护进程时（`DEFER-RB-03`）。

### ADR-RB-06: 跨库持久化意图重放协议与 `cloud sync` 一致快照

- **Status:** Accepted
- **Context:** 跨库提交若在 `GitStore` 提交后、`CaptureStore` 提交前崩溃，两库代际号失步；单纯重试只读快照无法修复该状态，而后续写入若盲目取 `max(git_gen, cap_gen) + 1` 会静默掩盖丢失的 Capture 记录。
- **Decision:**
  1. **持久化跨库发件箱（`git.redb` 内置 `META_CROSS_STORE_INTENT`）：**
     - 跨库写者始终按 `GitStore → CaptureStore` 锁序（`GC-RB-04`）。
     - **写前强制对账不变量（严禁 `max + 1` 掩盖缺口）：** 任何跨库写者在发起新代际提交前，必须先检查 `git.redb` 的 `META_CROSS_STORE_INTENT` 是否存在 `gen > capture.META_GENERATION` 的未对账意图；**若存在，必须先按 `gen` 严格升序将全部待决意图幂等重放至 `capture.redb` 并把 `capture.META_GENERATION` 推进至 `git.META_GENERATION`**（若 `capture.redb` 不可达/损坏则立即 fail-closed，绝不允许跳过缺口写入新代际）。
     - **两步持久化提交：** ① 在 `git.redb` 写事务内分配 `next_gen = git_gen + 1`，同步写入 Git 变更、`META_GENERATION = next_gen` 与 `META_CROSS_STORE_INTENT[next_gen] = serialized_capture_intent` 并 `commit()`；② 在 `capture.redb` 写事务内按语义幂等键应用该意图并写入 `META_GENERATION = next_gen` 并 `commit()`；③ 后续写事务或显式修复清理 `gen <= capture_gen` 的已对账意图。
  2. **显式修复入口：** `libra agent doctor --repair` 与 `libra fsck` 调用 `cross_store::reconcile_pending_intents()`，对任意半提交故障幂等补齐 `capture.redb` 直至 `capture_gen == git_gen`。
  3. **`cloud sync` 只读一致快照校验协议：** 打开 `git_rtx` 读 `g1` → 打开 `capture_rtx` 读 `c1` → 短开 `git_rtx2` 读 `g2`。仅当 **`g1 == c1 && g1 == g2`** 时冻结导出；若不等重试最多 5 次，超限立即 fail-closed 报 `LBR-IO-001` 并明确提示运行 `libra agent doctor --repair`（绝不导出撕裂快照，也不自动升写）。
- **Alternatives considered:** 裸写 `max(git_gen, cap_gen) + 1` 无发件箱。拒绝：崩溃半提交会永久丢失捕获目录且被后续提交掩盖。
- **Consequences:** 任何两库提交间崩溃均 100% 可检测、可重放、零静默丢失。
- **Revisit when:** 无需重审。

### ADR-RB-07: 对称两阶段回滚日志原子围栏双向迁移（零泄锁备份、防已打开句柄分叉、post-unlink 区分与终态后 Spool 解封）

- **Status:** Accepted
- **Context:** 正向与反向迁移均面临六类高危边界：① `Commands::Agent(_)` 不持 `MaintenanceLock`，且旧 `spool.rs:111-145` 在建临时文件与 rename 进 `ready/` 之间会释放 `capacity.lock`；② 仅在清单发布后再 rename 旧库会留下双活崩溃窗口；③ 裸字节 `pwrite`（哪怕 1 字节）在非原子扇区写设备上不具断电原子性；④ Unix 上对活 SQLite 文件执行常规拷贝/哈希若 `open()` 后 `close()` 任何额外 fd，会静默释放同进程 SQLite 的全部 POSIX `fcntl` 排他锁；⑤ `unlink` 后、`finalized=true` 前崩溃会产生源路径缺席态，必须与意外缺失源严格区分；⑥ 反向迁移若在 `finalized=true` 前解封 `capture-spool`，崩溃后旧 Hook 新接受的 spool 事件会被忽略丢失。
- **Decision:** 正向（`SQLite → redb`）与反向（`redb → SQLite`）采用完全对称的 **两阶段日志原子围栏与持久化进度状态机**（均由 `.libra/storage-format.json` 的 `finalized: bool`、`phase2_progress` 与 `targets_staged_for_rename` 驱动）：
  1. **正向迁移（`SQLite → redb`，`RB-10` + `RB-10m` + `RB-10n` + `RB-10a` + `RB-10f` + `RB-10b` + `RB-10e` + `RB-10g` + `RB-10c` + `RB-10d` + `RB-10h`）：**
     - **Phase 1（WAL 静默与零活跃读者校验、零额外 fd `VACUUM INTO` 锁安全备份、不移锁目录、可无损中止）：**
       - 依次获取排他 `migration.lock`、`maintenance.lock`、`capture-spool-consumer.lock`、`writer.lock`、**不移动的 `.libra/private/capture-spool/capacity.lock`**、**不移动的 `.libra/private/capture-queue/append.lock`**；
       - 检查 `capture-spool/tmp/` 是否存在未过期活跃临时文件（即处于 `spool.rs:119-145` 之间的在途发布者）：**非空即 fail-closed 拒绝迁移，源库零改动**；
       - **WAL 静默与单连接排他锁保持协议（适用于 `libra.db`、`capture.db` 及 `--global`/`--system` `config.db`）：** 为每个源 SQLite 库打开**唯一一个非池化 `SqliteConnection`（`c_mig`，严禁使用连接池）**，配置 `busy_timeout = 0` 并立即执行 **`PRAGMA synchronous = FULL;`** 与 **`PRAGMA locking_mode = EXCLUSIVE;`**。核查 `PRAGMA journal_mode;` 及 `-wal`/`-shm` 伴随文件：若处于 `wal` 模式或存在 `-wal`/`-shm`，**严禁预先调用 `PRAGMA wal_checkpoint(TRUNCATE);`**（防外部空闲 WAL 连接在先检查点篡改 `.db`/`-wal` 后才令切模式失败），改为在 `busy_timeout = 0` + `PRAGMA locking_mode = EXCLUSIVE;` 下**直接执行 `PRAGMA journal_mode = DELETE;`**（SQLite `sqlite3WalClose` 在归并任何 WAL 帧之前先尝试获取排他 OS 锁与全部 WAL 锁：只要存在任何活跃 WAL 读事务、活跃写事务或外部已打开空闲 WAL 连接，SQLite 立即报 `SQLITE_BUSY` 触发 Phase 1 fail-closed 中止 `LBR-IO-001` 且 `.db`/`-wal`/`-shm` 100% 逐字节零改动；仅当零外部连接真正离线时，才由 `journal_mode = DELETE` 内部原子完成独占检查点、写入主 `.db`、消除 `-wal`/`-shm` 并断言返回 `delete`）。随后在 `DELETE` 模式下执行无写 **`BEGIN EXCLUSIVE; ROLLBACK;`**（触发并校验 OS 级 `EXCLUSIVE` 排他锁获取，任何持 `SHARED` 读锁或写锁的外部连接立报 `SQLITE_BUSY`；且在 `PRAGMA locking_mode = EXCLUSIVE` 下，该 POSIX/Windows 排他锁跨 `ROLLBACK` 持续持有直至连接关闭！）；**从此刻直至 Phase 2 `unlink`（仓库库）或只读墓碑定型（配置库）完成期间，严禁在迁移进程内对活 `.db` 路径执行任何额外的 `File::open`、`fs::copy`、`fs::read` 或第二条 `SqliteConnection`（彻底杜绝 Unix `close(fd)` 静默释放 SQLite `fcntl` 锁）！**
       - 只读识别三类源布局（A：未拆库仅 `libra.db` 含 61+23 表；B：已拷贝未摘表，校验 `capture_catalog_copy_receipt` 与主键覆盖，不完整即 fail-closed；C：现行已拆库 `libra.db` 61 活表 + `capture.db` 25 表）；
       - 经已锁 `c_mig` 流式导出（≤ 500 行/批、≤ `64 MiB` 堆峰值）至 `.libra/redb/git.gen-<id>.redb.tmp` 与 `.libra/private/capture.gen-<id>.redb.tmp`，**同步在流式读取过程中经 `c_mig` 直接计算规范逻辑摘要 `source_logical_sha256`**（确定性覆盖 `PRAGMA schema_version` + 全部表名/建表 SQL/行计数/有序规范化行字节，无需另开源 `.db` 文件句柄！），并将 `capture-spool/ready/` 与 `capture-queue/` 待处理帧**只读直灌入 `capture.gen-<id>.redb.tmp`**；
       - 逐表完成行数/主键/抽样 SHA-256 校验，rename `.tmp` → `.libra/redb/git.gen-<id>.redb` / `.libra/private/capture.gen-<id>.redb` 并 `fsync` 文件与父目录；
       - **锁安全 `VACUUM INTO` 备份（零额外活库 fd）：** 直接在已锁 `c_mig` 上执行 SQLite 原生 **`VACUUM INTO '<backup_dir>/<db>.tmp'`** 生成自包含洁净单文件备份；仅对独立新 inode `<backup_dir>/<db>.tmp` 执行 `PRAGMA integrity_check == "ok"`、`fsync`、计算 `backup_sha256` 并原子 `rename` 至 `.libra/sqlite-backup/<gen>/<db>` + `fsync_dir`（绝不打开/关闭活 `.db` 路径！）。同步将 `.libra/private/capture-spool/{ready,quarantine}/`（**保留原目录与 `capacity.lock` 原地不动**）复制至 `.libra/sqlite-backup/<gen>/capture-spool/`、核验逐字节 SHA-256 并 `fsync`。**Phase 1 中止契约：** 任一步失败即释放连接并清理 `.tmp`/`.gen-*`/备份目录——`DELETE` 源库与 spool 100% 逐字节未变；离线 WAL 源库仅在确认零外部连接后经 `PRAGMA journal_mode = DELETE;` 内部独占检查点无损归并消除 `-wal`/`-shm`，逻辑数据 100% 完整可读。
     - **Phase 2（100% SQLite 回滚日志原子围栏 + 持久化 `phase2_progress` 摘除状态机 + 不移锁原地 Spool 墓碑，全程继续持有全部锁与未释 `c_mig`）：**
       - Step 2.1：在仍持全部锁与 `c_mig` 下原子发布 `.libra/storage-format.json`（`backend="redb", finalized=false, generation=<id>, phase2_progress={git_db:"pending", capture_db:"pending"|"absent", spool_fenced:false}, source_digests={git_logical_sha256, git_backup_sha256, cap_logical_sha256, cap_backup_sha256, spool_manifest_sha256, queue_manifest_sha256}` + `fsync(.libra/)`）；
       - Step 2.2（**100% SQLite 回滚日志原子围栏 + 持久化 post-unlink 校验点，零裸字节写、抗断电扇区撕裂、阻断一切依赖 schema 的读写/DDL/`VACUUM`**）：
         - **Step 2.2a（单事务回滚日志原子围栏）：** 对每个源库仍在同一已锁 `c_mig`（持续持 `PRAGMA synchronous = FULL` + `PRAGMA locking_mode = EXCLUSIVE` + `journal_mode = DELETE`）上执行单一原子 SQLite 事务：
           `BEGIN EXCLUSIVE; CREATE TABLE __libra_migrated_fence__ (x INTEGER); PRAGMA writable_schema = ON; UPDATE sqlite_master SET sql = 'LIBRA_MIGRATED_TO_REDB_FENCE_SYNTAX_ERROR' WHERE name = '__libra_migrated_fence__'; PRAGMA schema_version = <sv + 1>; PRAGMA writable_schema = OFF; COMMIT;`
           因为全程走 SQLite 自带 `synchronous = FULL` 回滚日志（`libra.db-journal`），**即便断电造成页 1 物理扇区部分撕裂，SQLite 热日志自动 100% 无损回滚至前置洁净态**；而一旦 `COMMIT` 成功，`schema_version` 与 `file_change_counter` 均已持久递增，且 `locking_mode = EXCLUSIVE` 跨 `COMMIT` 保持排他锁不释放——锁释放后任何迁移前已打开旧连接（冷连接、页缓存已热连接或含缓存预编译语句）或新连接发起**一切依赖 schema 加载或校验的 Libra SQLite 操作**——包括对任意业务表的 `SELECT`（如 `SELECT * FROM reference`）、`sqlite_master` 查询、DML（`INSERT`/`UPDATE`/`DELETE`）、DDL（`CREATE TABLE IF NOT EXISTS agent_bridge_publication(...)`）、`PRAGMA table_info`/`journal_mode=WAL` 及 `VACUUM`——均立即报 **`SQLITE_CORRUPT`（`malformed database schema (__libra_migrated_fence__)`）** fail-closed（注：不加载 `sqlite_master` 的空洞语句如 `SELECT 1` 或不含表访问的空 `BEGIN IMMEDIATE` 不触发 schema 校验且不产生任何业务读/写，一旦执行首条表操作即立报 `SQLITE_CORRUPT`）；
         - **Step 2.2b（持久化 `fenced_ready_to_unlink` → 持锁 `unlink` → `unlinked`）：** 围栏 `COMMIT` 后先原子更新 `.libra/storage-format.json` 将该库标记为 **`phase2_progress.<db> = "fenced_ready_to_unlink"`** + `fsync(.libra/)`；随后 Unix 在仍持 `c_mig` 排他锁下 `unlink(&db_path)` + `fsync_dir(parent)` 再关闭 `c_mig`（Windows 先关闭 `c_mig` 再 `unlink` + `fsync_dir`），随即持久化 **`phase2_progress.<db> = "unlinked"`** + `fsync(.libra/)`；
       - Step 2.3（**不移锁目录原地子项墓碑——锁身份 100% 跨越切换边界，消除 `ensure_spool` 重建目录竞态**）：**严禁移动或重命名 `.libra/private/capture-spool/` 目录**；全程继续持有原地未动的 `.libra/private/capture-spool/capacity.lock`（并发旧 Hook 在 `spool.rs:112` 均阻塞于同一 `capacity.lock` inode）。在持该锁下删除已备份并导入的 `ready/`/`quarantine/` 内文件，随后对 `sub ∈ {tmp, ready, quarantine}` 逐一执行「清空/移除子目录 → 原子创建 `0400` 普通文件墓碑 `.libra/private/capture-spool/<sub>`（若并发 `L105` `ensure_spool` 在 `rmdir` 与 `create_new` 间重建了空子目录则循环重试，因 `capacity.lock` 仍被本进程持有，并发 Hook 无法写入任何文件）」+ `fsync_dir(.libra/private/capture-spool)`，并持久化 `spool_fenced = true`；
       - Step 2.4：原子更新 `.libra/storage-format.json`（`backend="redb", finalized=true` + `fsync(.libra/)`），释放全部锁。
       - **Phase 2 全覆盖 4 态崩溃恢复协议（区分合法 post-unlink 与意外缺失 + 断电热日志回滚 + `capture-spool` 备份∪活目录并集对账）：** 任一新二进制见到 `finalized=false`（无论是否设置 `LIBRA_STORAGE_BACKEND`）均 fail-closed 提示重跑 `libra-migrate`。续跑重获全部静默锁（含同一未动 `capacity.lock` 与 `append.lock`）：
         - ① **每源 SQLite 完备 4 态判定（结合 `phase2_progress.<db>` 与单连接 `c_mig` 检查）：**
           - **态 1（源路径不存在 `ENOENT`）：** 若 `phase2_progress.<db> ∈ {"fenced_ready_to_unlink", "unlinked"}`，判定为**合法正常 `unlink` 后状态**——必须校验 `.libra/sqlite-backup/<gen>/<db>` 存在、`backup_sha256` 匹配且 `PRAGMA integrity_check == "ok"`（备份缺失或校验失败立即 fail-closed `LBR-IO-001`），记录 `"unlinked"`，若其它源漂移需重导则从该已验备份读取；**若 `phase2_progress.<db> == "pending"` 而源路径缺失，判定为意外缺失源，立即 fail-closed 报 `LBR-IO-001`**；
           - **态 2（源存在，正常 SQLite 探查成功 —— 含断电热回滚日志自动回滚后的洁净态）：** 打开单连接 `c_mig`（`synchronous=FULL` + `locking_mode=EXCLUSIVE` + WAL 静默 + `BEGIN EXCLUSIVE`，自动回滚任何断电残留热回滚日志），若正常 `SELECT name FROM sqlite_master` 成功：若 `phase2_progress.<db> ∈ {"fenced_ready_to_unlink", "unlinked"}` 属不变量冲突立即 fail-closed `LBR-IO-001`；若为 `"pending"` 则在 `c_mig` 上计算 `source_logical_sha256`——一致则直接进 Step 2.2a/2.2b；漂移（崩溃后旧写者写入新行）则从活库重导至新代际 `gen+1`、`VACUUM INTO` 刷新备份并更新 `storage-format.json(finalized=false, gen+1)` 后再进 Step 2.2a/2.2b；
           - **态 3（源存在，正常 `SELECT` 报 `malformed database schema` 且经 `PRAGMA writable_schema = ON` 精确核验 `sqlite_master` 含 `name = '__libra_migrated_fence__'` 且 `sql = 'LIBRA_MIGRATED_TO_REDB_FENCE_SYNTAX_ERROR'`）：** 说明回滚日志围栏已原子提交、崩溃于持久化 `fenced_ready_to_unlink` 或 `unlink` 之前，零围栏后写入；校验对应备份 `backup_sha256` 与 `integrity_check` 后持久化 `fenced_ready_to_unlink`、执行 `unlink` + `fsync_dir` 并持久化 `"unlinked"`；
           - **态 4（其它任何 SQLite 损坏/非围栏错误魔数/缺失精确哨兵）：** 立即 fail-closed 报 `LBR-IO-001`，绝不静默丢弃；
         - ② **Spool 间隙崩溃/并发对账：** 若 `capture-spool/{tmp,ready,quarantine}` 尚未全部成为 `0400` 墓碑且活 `capture-spool/{ready,quarantine}/` 出现崩溃后新确认事件，续跑将**备份 `.libra/sqlite-backup/<gen>/capture-spool/` ∪ 活 `.libra/private/capture-spool/`** 按事件去重键取并集重导至新代际 `gen+1`、刷新备份后再于原地 `capacity.lock` 下装齐 3 个 `0400` 子项墓碑；
         - ③ **Queue 对账：** 若 `capture-queue/` 摘要偏离 `queue_manifest_sha256`，同样并入 `gen+1` 重导后再推进至 `finalized=true`。
  2. **对称反向迁移（`redb → SQLite`，`RB-11` + `RB-11i` + `RB-11p` + `RB-11d` + `RB-11j` + `RB-11l` + `RB-11a` + `RB-11m` + `RB-11e` + `RB-11h`）：**
     - **Phase 1（严格锁序排斥既有 Spool 消费者 + `quarantine/` 非空 fail-closed + 无死锁单趟持锁导出 + 待决意图直灌 + 全新 Redb 仓既有活 Spool 静默对账 + `PRAGMA application_id` 打标，源红库 100% 只读不变）：** 严格按明确锁序依次获取排他锁：**`migration.lock` → `maintenance.lock` → `capture-spool-consumer.lock`（`src/internal/ai/capture/drain.rs:87` `CONSUMER_LOCK`，全程持有直至 Phase 2 Step 2.6 结束，彻底排斥并发既有 `drain_storage`/`drain_locked` 将 `ready/` 移入 `quarantine/`）→ `writer.lock` → 不移动的 `capture-queue/append.lock` → 不移动的 `capture-spool/capacity.lock`**（若全新 Redb 仓缺 `.libra/private/capture-spool/` 目录则先按 `0700`/`0600` 建立锁目录再持锁）→ `git.redb` + `capture.redb` 排他 `WriteTransaction`（严格 `Git → Capture` 序，**Phase 1 全程不提交也不嵌套重开写事务，彻底杜绝死锁**）。在已持 `capture-spool-consumer.lock` 与 `capacity.lock` 下：① 检查 `capture-spool/tmp/` 是否存在未过期在途临时文件（非空即 fail-closed `LBR-IO-001`）；② **检查 `capture-spool/quarantine/` 是否非空（存在任何隔离事件文件）：若非空立即在 Phase 1 fail-closed 返回 `LBR-IO-001`（源红库与 `quarantine/` 文件 100% 完好不动，严禁静默清空未恢复隔离事件）**；③ 仅当 `tmp/` 与 `quarantine/` 均为空时，将任意既有活 `capture-spool/ready/` 事件（针对未预装墓碑或被外部改写的全新 Redb 仓）同步归档备份至 `.libra/redb-backup/<gen>/capture-spool/ready/` 并按语义去重键幂等直灌入 `capture.db.tmp`。在已持有的 `(git_wtx, cap_wtx)` 内直接流式导出 61+25 源表至 `libra.db.tmp`/`capture.db.tmp`，**同步从 `git_wtx` 直读全部未对账 `META_CROSS_STORE_INTENT`（`gen > cap_gen`）按升序幂等重放写入目标 `capture.db.tmp`**，并将 `capture-queue/{priority,normal}/` 待消费帧幂等并入 `capture.db.tmp`；在 `fsync` 前向 `capture.db.tmp` 与 `libra.db.tmp` 写入唯一非零 **`PRAGMA application_id = target_migration_tag`**（`u32`，同时写入清单，与旧版二进制新建库默认 `application_id == 0` 严格区分）；逐表校验后计算冻结源 `source_digests`，并将 `git.redb`/`capture.redb` 及 `capture-queue/{priority,normal,quarantine}/` 拷贝并 SHA-256 校验 `fsync` 备份至 `.libra/redb-backup/<gen>/`（保留原目录与 `append.lock` 原地不动；任一步失败仅丢弃 `.tmp` 与未提交写事务，源 `redb` 与队列逐字节未变）。
     - **Phase 2（对称围栏 + 持久化 post-unlink + 主动确立 Spool 墓碑跨越 `finalized=false` + 每次尝试均用非覆写 `hard_link` 先发 `capture.db` 后发 `libra.db` 与 `AlreadyExists` 持锁并轨 + 终态后持久追踪 `spool_unfenced` 解封）：**
       - Step 2.1：原子发布 `storage-format.json(backend="sqlite", finalized=false, spool_unfenced=false, targets_staged_for_rename=false, target_migration_tag=<u32>, phase2_progress={git_redb:"pending", cap_redb:"pending", queue_fenced:false, spool_fenced:false}, source_digests={git_redb_sha256, cap_redb_sha256, queue_manifest_sha256, spool_manifest_sha256})`；
       - Step 2.2：在已持双 `WriteTransaction` 内向旧 `git.redb`/`capture.redb` 写入 `META_SCHEMA` 围栏哨兵（`min_reader_version = u32::MAX`）并 `commit()`，逐库持久化 `fenced_ready_to_unlink` → `unlink` + `fsync_dir` → `unlinked`（已打开旧红库句柄在下一读写事务首步立报 `RepositoryMigrated` fail-closed）；
       - Step 2.3：在仍持原地未动的 `capture-queue/append.lock` 下清除已备份段并将子项 `{priority,normal,quarantine}` 原地替换为 `0400` 普通文件墓碑 + `fsync_dir`，持久化 `queue_fenced = true`；**同时在仍持原地未动的 `capture-spool-consumer.lock` 与 `capture-spool/capacity.lock` 下复核 `capture-spool/quarantine/` 仍为空，随后主动核验并确立 `capture-spool/{tmp,ready,quarantine}` 全部 3 个 `0400` 普通文件墓碑 + `fsync_dir`（既有墓碑保留，缺失/全新 Redb 仓清空已备份 `ready/` 文件后原地安放墓碑）并持久化 `spool_fenced = true`**——从而无论仓来源是旧 SQLite 迁移还是全新 Redb 创建，整个 `finalized == false` 窗口内没有任何旧 SQLite Hook 或既有 spool `drain` 消费者可接受或转移事件！
       - Step 2.4（**正常路径与崩溃恢复统一使用非覆写 `std::fs::hard_link` 先发 `capture.db` 后发 `libra.db`，严禁无条件覆盖型 `rename`，彻底关闭无崩溃与崩溃两类竞争 `capture::store::open` 覆写窗口**）：持久化 `targets_staged_for_rename = true` + `fsync(.libra/)`（此时所有红库、队列与既有 spool 源已 100% 围栏摘除）；随后在**每一次尝试（包括正常 Step 2.4 与崩溃续跑）**执行统一的非覆写发布协议：
         - **① 先发 `.libra/private/capture.db`（非覆写 `hard_link`）：** 执行 **`std::fs::hard_link("capture.db.tmp", ".libra/private/capture.db")`**（若底层文件系统返回 `Unsupported`/`ENOSYS`/`EXDEV` 立即 fail-closed `LBR-IO-001`，绝不降级为覆盖型 `rename`）：
           - *若 `hard_link` 成功：* 立即 `unlink("capture.db.tmp")` + `fsync_dir(.libra/private)`；
           - *若 `hard_link` 返回 `NotFound`（即上次发布已 `hard_link` 并 `unlink("capture.db.tmp")` 后崩溃）：* 核验现存 `.libra/private/capture.db` 存在且 **`PRAGMA application_id == manifest.target_migration_tag`**——匹配则证明该库已在先就位，**绝不覆写**（100% 保留崩溃后旧进程写入）并直接进入 ②；若缺失或 `application_id` 不符则立即 fail-closed `LBR-IO-001`；
           - *若 `hard_link` 返回 `AlreadyExists`（含三种场景：无崩溃竞态旧进程调用 `capture::store::open` 建库、崩溃于 `hard_link` 后 `unlink(tmp)` 前同 inode 残留、或崩溃于竞争库围栏后）：* 立即进入持锁 `PRAGMA application_id` 判定与并轨围栏协议：
             - (a) 现存 `capture.db` 的 **`PRAGMA application_id == manifest.target_migration_tag`** → 证明为本次迁移已就位目标库，**绝不覆写**（100% 保留崩溃后旧进程写入），`unlink` 过时/重复硬链 `capture.db.tmp` + `fsync_dir`；
             - (b) 现存 `capture.db` 的 **`PRAGMA application_id == 0`**（竞争 `capture::store::open` 建库！）→ 打开单连接 `c_mig`（`synchronous=FULL` + `locking_mode=EXCLUSIVE` + WAL 静默；若仍有活跃外部读者/写者立报 `LBR-IO-001` 等待重试），将竞争 `capture.db` 的全部有效行按语义主键/去重键（遵守 `agent_import_tombstone`）**并轨合入 `capture.db.tmp`** + `fsync(capture.db.tmp)`，在 `c_mig` 上对竞争库提交 `__libra_migrated_fence__` 围栏阻断其已打开句柄，再**仅在此刻（持排他锁且竞争 inode 已围栏、零新写入可落盘）**执行原子 `rename("capture.db.tmp", ".libra/private/capture.db")` + `fsync_dir`；
             - (c) 现存 `capture.db` 已带精确 `__libra_migrated_fence__` 哨兵（通过 `PRAGMA writable_schema = ON` 核验；即上次并轨恰崩溃于围栏竞争库后、`rename` 前）且 `capture.db.tmp` 具有 `PRAGMA application_id == target_migration_tag` 并通过 `PRAGMA integrity_check == "ok"` → 幂等执行原子 `rename("capture.db.tmp", ".libra/private/capture.db")` + `fsync_dir`；
             - (d) 其它非零未知 `application_id` 或非哨兵损坏 → 立即 fail-closed `LBR-IO-001`；
         - **② 再发 `.libra/libra.db`（非覆写 `hard_link`）：** 执行 **`std::fs::hard_link("libra.db.tmp", ".libra/libra.db")`**（`Unsupported`/`EXDEV` fail-closed `LBR-IO-001`）：成功则 `unlink("libra.db.tmp")` + `fsync_dir(.libra)`；若返回 `NotFound`（已 `unlink(tmp)` 后崩溃）则核验现存 `libra.db` 存在且 `PRAGMA application_id == manifest.target_migration_tag`（匹配则零覆写推进，否则 fail-closed `LBR-IO-001`）；若返回 `AlreadyExists`，检查 `PRAGMA application_id`——等于 `manifest.target_migration_tag` 则绝不覆写并 `unlink` 过时/重复硬链 `libra.db.tmp`，否则立即 fail-closed `LBR-IO-001`（绝不覆盖异源库）。
       - Step 2.5：原子发布 `storage-format.json(backend="sqlite", finalized=true, spool_unfenced=false)` + `fsync(.libra/)`（仍持有 `migration.lock`、`capture-spool-consumer.lock` 与 `capture-spool/capacity.lock`）；
       - **Step 2.6（终态后解封 Spool 并持久化 `spool_unfenced = true`）：** 仅在 `finalized = true` 落盘后、释放锁前，在 `capture-spool-consumer.lock` 与 `capture-spool/capacity.lock` 下对 `sub ∈ {tmp, ready, quarantine}` 逐一检查 `symlink_metadata().is_file()` 并仅 `unlink` 残留的 `0400` 普通文件墓碑 + `fsync_dir(.libra/private/capture-spool)`，随后原子更新 `storage-format.json(backend="sqlite", finalized=true, spool_unfenced=true)` + `fsync(.libra/)` 再释锁。
       - **反向 Phase 2 全覆盖崩溃恢复：** ① `targets_staged_for_rename == false` 且 `finalized == false`：重获全部锁（含 `capture-spool-consumer.lock`）后对 `git.redb` 与 `capture.redb` 各自按 `phase2_progress`（`pending` 缺文件 fail-closed，`fenced_ready_to_unlink`/`unlinked` 缺文件验对应 `redb-backup` SHA-256 恢复、缺备份/失配 fail-closed）与 `source_digests` 检查漂移、对 `capture-queue` 取 **备份 ∪ 活目录**并集重导（并防御性兼顾异常外部解封的活 `capture-spool/ready/` 事件，若 `quarantine/` 非空则 fail-closed）；② `targets_staged_for_rename == true` 且 `finalized == false`：直接复用上述 Step 2.4 同一非覆写 `hard_link` + `PRAGMA application_id` 判定/竞争并轨协议完成 `capture.db` 与 `libra.db` 发布，再推进 Step 2.5 与 Step 2.6；③ `finalized == true && spool_unfenced == false`（两类 100% 可达状态：**可达态 A — 部分清理崩溃**，留 `quarantine` 墓碑时 `ensure_spool` fail-closed，重跑幂等移除残墓碑并置 `spool_unfenced = true`；**可达态 B — 全 3 墓碑移除+`fsync_dir` 后、`spool_unfenced = true` 落盘前崩溃**，旧 Hook 重建三目录并发布真实活事件到 `capture-spool/ready/`，重跑识别三子项均为目录、**零改动保留活 `capture-spool/ready/` 事件**并置 `spool_unfenced = true`，新事件最终完整入库零丢失）。
  3. **全局与系统配置双向迁移（`RB-11b` + `RB-11k` + `RB-11f` + `RB-11n` + `RB-11g` + `RB-11o` + `RB-11c`，单权威零双写 + 原位保留型 SQLite 路径墓碑 + 反向 `targets_staged_for_rename` 保护）：** 支持 `libra-migrate --global` 与 `libra-migrate --system`（双向 `--to redb|sqlite`），全程持排他 `LOCK_EX` 于 `<scope_dir>/config-migration.lock`：
     - **正向配置迁移（`Sqlite → Redb`，绝不 `unlink(config.db)`，永久原位保留已围栏只读路径墓碑）：** Phase 1 经单连接 `c_mig` 过 WAL 静默门 + `VACUUM INTO` 锁安全备份并仅导出至 `config.redb.tmp`（目标名在旧库围栏前绝不出现）；Phase 2 先原子发布 `config-format.json(backend="redb", finalized=false, legacy_sqlite_tombstone=false, targets_staged_for_rename=false, source_db_state="pending", backup_sha256, source_logical_sha256)`，随后在 `c_mig` 上执行回滚日志原子围栏 `__libra_migrated_fence__` `COMMIT` → 将 `config.db` 权限收紧为只读（`0400` Global / `0444` System）并关闭 `c_mig`（**严禁 `unlink(config.db)`，永久保留已围栏只读 `config.db` 路径墓碑，从而使任何旧版二进制命中 `src/command/config.rs:257` `exists == true` 分支并经 `open_configuration_database` 立报 `SQLITE_CORRUPT` fail-closed，哪怕 `root` 也无法重建第二份 SQLite 库！**）→ 持久化 `source_db_state="fenced_tombstone_retained", legacy_sqlite_tombstone=true, targets_staged_for_rename=true` + `fsync_dir` → 原子 `rename(config.redb.tmp, config.redb)`（`0600`/`0644`）+ `fsync_dir` → 发布 `config-format.json(backend="redb", finalized=true, legacy_sqlite_tombstone=true)` 后释锁。崩溃恢复时若 `targets_staged_for_rename == true` 且 `config.redb` 已存在则绝不覆写、仅核验墓碑并直推 `finalized = true`；若 `source_db_state == "pending"` 而 `config.db` 意外缺失或已围栏备份校验失配则 fail-closed `LBR-IO-001`。
     - **反向配置迁移（`Redb → Sqlite`，对称 `unlink(config.redb)` + `targets_staged_for_rename` 保护已发布 `config.db`）：** Phase 1 持排他写事务备份 `config.redb` 至 `<scope_dir>/redb-backup/<gen>/config.redb`（校验 SHA-256 + `fsync`）并导出至 `config.db.tmp`（`0600`/`0644`）；Phase 2 先发布 `config-format.json(backend="sqlite", finalized=false, targets_staged_for_rename=false, source_db_state="pending", backup_sha256, source_logical_sha256)` → 提交 `META_SCHEMA u32::MAX` 围栏 → 持久化 `source_db_state="fenced_ready_to_unlink"` → `unlink(config.redb)` + `fsync_dir` → 持久化 `source_db_state="unlinked"` → **持久化 `targets_staged_for_rename = true` + `fsync_dir`** →（Windows 上先校验现有 `config.db` 为墓碑后清除 `readonly` 属性）执行原子 `rename(config.db.tmp, config.db)` + `fsync_dir` 覆盖旧路径墓碑 → 发布 `config-format.json(backend="sqlite", finalized=true, legacy_sqlite_tombstone=false)` 后释锁。**反向配置崩溃恢复不变量：** ① `targets_staged_for_rename == false`：`source_db_state == "pending"` 意外缺失 `config.redb` 或 `fenced_ready_to_unlink`/`unlinked` 缺/损 `redb-backup` 均立即 fail-closed `LBR-IO-001`；合法 `fenced_ready_to_unlink`/`unlinked` 校验 `redb-backup` 真实 SHA-256 后推进；② **`targets_staged_for_rename == true` 且 `finalized == false`：** 检查现有 `config.db`——若其仍为可识别墓碑（`__libra_migrated_fence__` 或 `LIBRA_CONFIG_AUTHORITY_IS_REDB\n`）或不存在，则执行原子 `rename(config.db.tmp, config.db)`；**若现有 `config.db` 已是有效未围栏 SQLite 库（说明崩溃发生在 `rename` 之后、`finalized = true` 之前，且旧版二进制可能已向其写入新配置），严禁覆写 `config.db`**，仅清理过时 `config.db.tmp` 并直接推进发布 `config-format.json(backend="sqlite", finalized=true, legacy_sqlite_tombstone=false)`（100% 保留崩溃后旧二进制确认写入）；若现有 `config.db` 既非墓碑亦未通过 `PRAGMA integrity_check` 则 fail-closed `LBR-IO-001`。
     - 在 `RB-29a` 前由编译期常量 `REDB_RUNTIME_UNLOCKED = false` 硬锁定已发行 `libra-migrate` 二进制。
- **Alternatives considered:** 单字节裸写 `pwrite(0, b'X')`、持锁期间常规 `fs::copy` 拷贝源库、在 `finalized=true` 前解封 `capture-spool`，以及在正向配置迁移后 `unlink(config.db)`（旧版二进制在 `config.rs:257` 见到路径缺失会直接新建第二份 SQLite 库造成配置分裂）。全部拒绝。
- **Consequences:** 正向、反向与配置迁移在任意断电扇区撕裂、旧版二进制重入、跨进程争用、双源/双向 `unlink` 后崩溃、反向配置 staged-rename 后旧二进制写入及终态解封中途崩溃下均可确定性恢复，零丢写、零静默分叉。
- **Revisit when:** 物理移除 SQLite 支持时（`DEFER-RB-02`）。

### ADR-RB-08: 平行开发隔离工作区、146 文件家族原子接线与双签名保签适配

- **Status:** Accepted
- **Context:** 实盘核查存在 **146 个**生产接线文件（145 DB 调用方 + 1 沙箱敏感路径文件），且部分内部函数显式接收 SeaORM `ConnectionTrait` / `DatabaseConnection`（如 `head.rs:171,324`、`live_checkpoint.rs:147`、`agent_bridge/publication.rs:10`）；逐卡发版会暴露半接线损坏的 `redb` 后端，直接改主工作区会阻塞并发功能 Agent。
- **Decision:**
  1. **隔离工作区与透明共享边界：** Track A 在独立工作区 `../libra-redb-track-a`（分支 `plan/20261121-redb-storage-concurrency`）执行；Track B 在独立集成工作区 `../libra-redb-integration`（分支 `plan/20261121-redb-integration`）执行。诚实声明唯一跨计划共享触点：Track A 仅 `RB-01`（`Cargo.toml` + `src/lib.rs` 1 行）、`RB-11c`（`Cargo.toml` `[[bin]]`）、`RB-12`（`base.yml` + `tests/INDEX.md`）加 `ER-12` 单一发布者对 `Release write set` / `plan-status.md` 的串行提交；**`RB-01d`..`RB-11o`（110 张连续卡）100% 独占于 `src/storage/**`**。
  2. **双签名保签适配：** 对接收 SeaORM `ConnectionTrait` / `DatabaseConnection` 的既有内部函数，**原样保留既有函数与签名供 SQLite 与其它 Agent 消费**，并行新增 `*_with_store(&dyn GitStore)` / `*_with_capture(&dyn CaptureStore)` 分支，由顶层入口按 `StorageBackendKind` 分发；严禁触碰禁改文件 `db.rs`、`capture/{store,detach}.rs`、`shared_perm.rs`。
  3. **`G-08` 家族 `REL-RB-01` 原子上线：** Track A 全程禁止 CLI 运行时与 `libra-migrate` 实盘切换启用 `Redb`；Track B 全部 146 文件接线（`RB-13`..`RB-28b`）、后端感知队列接入（`RB-29`）、守护/`doctor` 指标（`RB-29d`）、双门控解锁（`RB-29a`）、解锁态二进制测试与 `base.yml` 切换（`RB-29c`）及 `syn 2.0` AST 审计（`RB-29b`）全部作为 **`family child of REL-RB-01`** 仅在本地隔离分支提交，不推 `main`、不发版，直至全部完成并 rebase 最新 `origin/main` 后由 **`RB-30`（`Task type = release`）** 一次原子发布。
- **Alternatives considered:** Track B 逐卡独立发版或破坏性重写 146 文件签名。拒绝：暴露半成品后端且大面积阻塞其它 Agent。
- **Consequences:** 其它 Agent 全开发期自由向 `main` 推送，仅在 `RB-30` 汇流窗串行对账一次。
- **Revisit when:** `REL-RB-01` 集成发布后。

## 全局工程约束

模板 `GC-01`..`GC-13` 全部生效；本计划追加以下工程约束：

- **GC-RB-01 单源不变量：** `src/storage/` 内 `Redb*Store` 与 `Sqlite*Store` 共享领域类型；版本化 `postcard` 编解码、二级索引、墓碑拒绝、审计只追加、`HEAD` 同事务 `agent_bridge_publication` 与 `META_CROSS_STORE_INTENT` 仅维护于 `src/storage/` 单一事实源。
- **GC-RB-02 角色与账本隔离矩阵（`GC-13` 全 5 类角色闭环）：**

| `DatabaseRole` | SQLite 源路径 | redb 目标路径 | SQLite 账本 / redb 账本 | 迁移与兼容规则 |
|---|---|---|---|---|
| `Repository` | `.libra/libra.db` | `.libra/redb/git.gen-<id>.redb`（由 `storage-format.json` 指向） | `schema_versions` / `META_SCHEMA(Repository)` | 61 活跃源表（含 2 张按需桥接发布表）；Phase 1 备份至 `.libra/sqlite-backup/<gen>/`，Phase 2 围栏并 unlink 原 `.db`；整目录 `.libra/redb/` 钉入沙箱拒读 |
| `Capture` | `.libra/private/capture.db`（或旧仓 `libra.db` 内联） | `.libra/private/capture.gen-<id>.redb` | `schema_versions` / `META_SCHEMA(Capture)` | 25 源表；兼容未拆/半拆/已拆 3 布局；`LBR-AGENT-048` 独立隔离 `git.redb` |
| `GlobalConfig` | `<config_dir>/libra/config.db` | `<config_dir>/libra/config.redb`（配 `config-format.json` 与 `config-migration.lock`） | `configuration_schema_versions` / `META_SCHEMA(GlobalConfig)` | 仅显式 `--global` 双向迁移；测试隔离 `LIBRA_CONFIG_GLOBAL_DB`；双运行时先检 `config-format.json`，杜绝双写与静默返回空 |
| `SystemConfig` | `/etc/libra/config.db` | `/etc/libra/config.redb`（配 `config-format.json` 与 `config-migration.lock`） | `configuration_schema_versions` / `META_SCHEMA(SystemConfig)` | 仅显式 `--system` 双向迁移；测试隔离 `LIBRA_CONFIG_SYSTEM_DB`；双运行时先检 `config-format.json`，杜绝双写与静默返回空 |
| `Derived` | 无常驻文件 | 无常驻文件 | 禁止建库或迁移 | `libra-migrate` 显式拒绝 `Derived` 角色 |

- **GC-RB-03 隐私与入队前脱敏：** `capture-queue/{priority,normal}/` 仅落盘已脱敏字节；新建目录 `0700`、段文件/元数据 `0600`（`O_NOFOLLOW`），错误文案禁止泄露绝对私有路径、凭据或会话正文（`ER-11`）。
- **GC-RB-04 跨库与队列锁序及事务内禁令：** 跨库写者严格按 `GitStore → CaptureStore` 取锁、先对账 `META_CROSS_STORE_INTENT` 再递增 `cross_store_gen` 并依次提交；队列消费者执行未提交尾恢复或历史段回收严格遵守 **`writer.lock → capture-queue/append.lock`**（恢复期间绝不提前推进 `QUEUE_HIGHWATER_*`，回收先原子更新 `LTM2` 推进 `MIN_LIVE_SEG` 再 `unlink`，且释放 `append.lock` 后才提交红库写事务，生产者永不取 `writer.lock`）；持有写事务期间严禁网络调用、工作树扫描或子进程等待。
- **GC-RB-05 Feature 组合可编译性：** 每卡必须通过 `--no-default-features --features storage-sqlite`、`--no-default-features --features storage-redb`（隐式启用 `storage-queue`）、`--no-default-features --features storage-redb,storage-queue`、`--features storage-sqlite,storage-redb,storage-queue` 与 `--all-features` 编译。
- **GC-RB-06 禁改文件与两阶段写集守卫：** 全计划严禁修改 `src/internal/db.rs`、`src/internal/ai/capture/{store,detach}.rs`、`src/utils/shared_perm.rs`；`RB-01d`..`RB-11o`（110 张连续独占卡）实现阶段严禁修改其声明 `Implementation write set` 之外的任何文件（不含版本面文件）；`RB-17` 严禁修改 `src/command/init.rs:1-20` 受保护头部与导入块。

## 执行检查必备需求（强制）

`ER-01`..`ER-14` 按 [`plan-template.md`](plan-template.md) `v2.16` 全文执行，并遵循以下协议：

1. **强制门不计入 `G-03` VER：** `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --no-deps --all-features`、`ER-13` 全量 `nextest` 与 `ER-06a` 同卡必需文档同步门均为全局强制门，不计入各卡 `Verification` 分子。
2. **精确 `(new)` 单命令单门验证协议（严格符合 `G-03`）：** 每张含新测试的卡将「精确发现门（`cargo nextest list`）」与「执行门（`cargo nextest run --no-tests=fail`）」分成独立 `Verification` 条目，**每条门仅含环境前缀 `source .env.test &&` 加 1 条验收命令，绝不用 `&&` 串联双门**；每条 VER 显式注明覆盖的 AC 编号。
3. **两阶段严格隔离写集守卫模板（防管道掩码与越权修改版本面）：**
   - **Stage 1（实现提交门，用于 `RB-01d`..`RB-11o` 全部 110 张独占卡，在版本 bump 之前执行）：** 使用 `--untracked-files=all` 展开全部未追踪目录并同时检查暂存与未暂存路径，**白名单仅包含本卡精确 `Implementation write set`（绝不包含 `Cargo.toml`/`Cargo.lock`/`install.*`/计划文档）**：
     ```bash
     bash -euo pipefail -c '
       out="$(libra status --porcelain=v1 --untracked-files=all)"
       while IFS= read -r line; do
         [ -z "$line" ] && continue
         path="${line:3}"
         case "$path" in
           <本卡精确 Implementation write set 文件1>|<本卡精确 Implementation write set 文件2>) ;;
           *) echo "FAIL: out-of-writeset path: $path"; exit 1 ;;
         esac
       done <<< "$out"
       echo "OK: exact implementation writeset"
     '
     ```
   - **Stage 2（`ER-12` 发布者版本 bump 门，仅在独立发布 commit 准备时执行）：** 核验 bump 变更仅触及 `ER-08` 规定版本面（`Cargo.toml` 版本行、`Cargo.lock`、`install.sh`、`install.ps1`、`plan-status.md` 与本文「实施证据汇总」），不混入业务源码改动。
4. **文档与网站验收命令（仅适用于 `RB-30` 与 `RB-33c`，严格遵循现行 `AGENTS.md:135` 保留无关改动流程与 `DEP-RB-03` 硬门）：**
   - ① **`DEP-RB-03` 授权目标硬门与分支预检：** (a) 默认遵循现行 `AGENTS.md:135` 设 `LIBRA_BACKEND="/Volumes/Sea/libra-backend"`；**若 `/Volumes/Sea/libra-backend` 尚未就绪（2026-11-21 本机实测不存在），立即 STOP，绝不擅自回退写入 `/Volumes/Data/gitmono/libra-backend`**——除非交付前已由用户在当前会话新作出明确书面裁定并更新 `DEP-RB-03` 与 `AGENTS.md:135` 显式授权替代路径；(b) 对经 `DEP-RB-03` 授权的 `$LIBRA_BACKEND` 校验 `test -d "$LIBRA_BACKEND/.libra" && test ! -e "$LIBRA_BACKEND/.git"` 且 `cd "$LIBRA_BACKEND" && libra config get remote.origin.url | rg -q 'libra-tools/libra-backend'`；(c) 运行 `cd "$LIBRA_BACKEND" && libra --offline symbolic-ref --short HEAD`（输出必须恰为 `cf`）与 `libra status --short --branch`（首行必须为 `## cf...origin/cf`）、确认**本卡目标文档文件无他人未提交改动**且暂存区为空（保留其它无关未提交改动，不清理、不建分支/PR/worktree）；`libra fetch origin` 后确认 `cf` 未分叉或超前 `origin/cf`（落后仅允 `pull --ff-only`，分叉/超前或任一项失败即 STOP）。
   - ② 运行同步脚本：`python3 "$LIBRA_BACKEND/scripts/sync-libra-command-docs.py" --libra-root "$LIBRA_ROOT" --write --only <agent|init>`，再 `--check` 确认 exit 0（覆盖 `apps/tanstack-app/content/docs/commands/{agent,init}.en.md`）。
   - ③ 前端验证：`cd "$LIBRA_BACKEND" && pnpm typecheck && pnpm build` 均 exit 0。
   - ④ 独立 Claude Code 评审最终网站 diff 获得字面 `VERDICT: PASS`。
   - ⑤ 精确暂存并签名提交：`libra add apps/tanstack-app/content/docs/commands/<target>.en.md` → 核对暂存路径恰为本卡目标文件 → `libra commit -s -m "docs(commands): sync <target> for redb storage"` → 按 `ER-07` 验签 + DCO → `libra push origin cf`。
   - ⑥ 部署核验：验证 Cloudflare 部署就绪后抓取 `https://libra.tools/en/docs/commands/<target>` 确认含本卡新增关键句（curl 失败或缺失即 STOP）。
5. **`RB-29b` / `RB-30` 语法感知逐调用点与调用方可达性审计协议（基于既有 dev 依赖 `syn 2.0`，`Cargo.toml:187`）：**
   - 严禁使用文件级关键字匹配、朴素行扫描或仅靠 `#[cfg(feature = "storage-sqlite")]` 放行替代运行时分发证明；审计守卫实现为 Rust 集成测试模块（位于 `tests/storage_concurrency_stress_test.rs`，使用 `syn::parse_file` + `syn::visit::Visit`）。
   - **Fail-closed 解析与 `#[cfg(test)]` 剥离：** 任一 `.rs` 文件发生语法解析失败或未闭合 `#[cfg(test)]` 块立即导致 `syn::parse_file` 返回错误并使测试失败；仅跳过 AST 上携带精确 `#[cfg(test)]` 属性的项（不受字符串字面量、原始字符串、字符字面量或注释干扰）。
   - **逐调用点 + 全树调用方可达性核查（双特性构建亦零漏网）：** 遍历 `src/` 下除 `src/storage/**`、`src/internal/db*`、`src/internal/model/*` 与 6 个明文审查豁免文件（3 个外部 SQLite 只读解析器 + 3 个禁改文件）外的**每一生产函数与方法**：
     - ① **直接 SQLite 打开/查询调用点（无 `#[cfg(feature = "storage-sqlite")]` 单独豁免）：** 任何直接调用 SQLite/SeaORM 打开器或查询的语句，**必须词法嵌套在显式 `StorageBackendKind::Sqlite` 分发分支内且同一 `match`/`if` 表达式具备配对的 `StorageBackendKind::Redb` 分支**（即使该 `Sqlite` 分支额外带 `#[cfg(feature = "storage-sqlite")]`，运行时 `StorageBackendKind` 分发分支仍为强制前提）。
     - ② **`*_with_conn` 兼容签名全树调用方可达性闭环：** 经审查登记的接收 `ConnectionTrait`/`DatabaseConnection`/`DbConn` 的 `*_with_conn` 保签函数须同时满足：(a) 函数体仅操作传入连接参数、绝不自行打开数据库；(b) 全仓 `src/` AST 中调用该 `*_with_conn` 函数的**每一处调用方**，本身要么也是已登记的 `*_with_conn` 传递函数，要么词法嵌套在含配对 `StorageBackendKind::Redb` 分支的 `StorageBackendKind::Sqlite` 分发分支内——借此证明**任何顶层入口在 `Redb` 运行时绝不可达任何 `*_with_conn` 或 SQLite 路径**。
   - **强制 4 组负向自测：** 同模块必须包含：`syn_guard_rejects_partially_wired_caller`（同文件含 `GitStore` 导入但另含 1 处未受保护 SQLite 调用 → 拒收）、`syn_guard_rejects_cfg_sqlite_without_runtime_dispatch`（仅带 `#[cfg(feature = "storage-sqlite")]` 但缺少运行时 `StorageBackendKind::Sqlite` 分发分支 → 拒收）、`syn_guard_rejects_unwired_caller_of_allowlisted_with_conn`（模块含配对 `foo_with_store` 但某命令入口直接调用 `foo_with_conn` 未经 `StorageBackendKind` 分发 → 拒收）、`syn_guard_fails_closed_on_malformed_rust_input`（语法错误与未闭合 `#[cfg(test)]` → fail-closed 解析报错）。

## 实施顺序

依赖边均为具体任务卡有向无环边（`G-06`）：

**Track A（独立并行存储核心轨，工作区 `../libra-redb-track-a`，114 卡）：**
- `RB-01 -> RB-01d -> RB-01b -> RB-01c -> RB-02 -> RB-02d -> RB-02b -> RB-02c -> RB-02e -> RB-03 -> RB-03j -> RB-03b -> RB-03h -> RB-03d -> RB-03f -> RB-03e -> RB-03i -> RB-03c -> RB-03g -> RB-04 -> RB-04b -> RB-05 -> RB-05b -> RB-06 -> RB-06c -> RB-06d -> RB-06b -> RB-06e -> RB-07 -> RB-07g -> RB-07c -> RB-07f -> RB-07d -> RB-07b -> RB-07e -> RB-08 -> RB-08t -> RB-08d -> RB-08u -> RB-08f -> RB-08b -> RB-08g -> RB-08m -> RB-08n -> RB-08i -> RB-08q -> RB-08k -> RB-08v -> RB-08j -> RB-08r -> RB-08l -> RB-08o -> RB-08s -> RB-08c -> RB-08y -> RB-08z -> RB-08e -> RB-08h -> RB-08x -> RB-09 -> RB-09b -> RB-09d -> RB-09c -> RB-09e -> RB-09f -> RB-09g -> RB-09h -> RB-09j -> RB-09k -> RB-09i -> RB-09l -> RB-10 -> RB-10m -> RB-10n -> RB-10a -> RB-10i -> RB-10f -> RB-10j -> RB-10b -> RB-10l -> RB-10e -> RB-10g -> RB-10c -> RB-10k -> RB-10d -> RB-10h -> RB-11 -> RB-11z -> RB-11q -> RB-11i -> RB-11p -> RB-11d -> RB-11x -> RB-11j -> RB-11y -> RB-11l -> RB-11a -> RB-11r -> RB-11m -> RB-11e -> RB-11s -> RB-11h -> RB-11u -> RB-11b -> RB-11v -> RB-11k -> RB-11w -> RB-11f -> RB-11n -> RB-11g -> RB-11o -> RB-11c -> RB-11t -> RB-12`

**Track B（`REL-RB-01` 家族本地集成轨，工作区 `../libra-redb-integration`，不推 `main` 直到 `RB-30`，30 卡）：**
- Git 门面与命令支路：`RB-12 -> RB-13 -> RB-14 -> RB-15 -> RB-16 -> RB-16b -> RB-17 -> RB-17b -> RB-18 -> RB-18b -> RB-19 -> RB-20 -> RB-20c -> RB-20b`
- Capture/AI/Bridge/GUI/Cloud 支路（与 `RB-17`..`RB-20b` 写集完全不相交，可并行实现）：`RB-16b -> RB-21 -> RB-22 -> RB-23 -> RB-24 -> RB-25 -> RB-25b -> RB-26 -> RB-26b -> RB-27 -> RB-28 -> RB-28b`
- 队列与守护接入、双门控解锁、解锁态 CI 切换、`syn 2.0` 逐调用点审计及家族原子发布：`RB-20b -> RB-29`、`RB-28b -> RB-29`、`RB-29 -> RB-29d -> RB-29a -> RB-29c -> RB-29b -> RB-30`

**Track B 末端（全量双矩阵与 `keyring` CI、双二进制成品打包与切前 `no-release` 审计、`REL-RB-03` 运行时切红库家族与 `REL-RB-02` Cargo 默认切换家族，10 卡）：**
- `RB-30 -> RB-31 -> RB-31b -> RB-32 -> RB-32b -> RB-33 -> RB-33b -> RB-33c -> RB-34 -> RB-34b -> RB-34c`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-RB-01 | `incoming` | 跨计划汇流窗口（仅 `RB-30`） | 同期活跃的 `plan-20261117-*.md`、`plan-20261120-ui-real-data.md`、`LVC-02`/`LVC-06` 与 `issues/*.md` | Coordinator | 在 `RB-30` rebase `plan/20261121-redb-integration` 至 `origin/main` 并合并前，协调一次短暂主线合并窗口以解冲突并重跑全量门 | `docs/development/plan/plan-20261117-global-init-infra.md:17-21`（2026-11-21 核对） | **不阻塞 Track A 也不阻塞 Track B 子卡本地实现**；若 `RB-30` 汇流遇冲突则在本地分支解冲突重验后合并 |
| DEP-RB-02 | `incoming` | 共享文件短窗口 | `Cargo.toml`、`src/lib.rs`、`.github/workflows/{base,release}.yml`、`tests/INDEX.md`、`COMPATIBILITY.md`、`docs/error-codes.md` | Coordinator | `RB-01`、`RB-11c`、`RB-12`、`RB-30`、`RB-31`、`RB-31b`、`RB-32`、`RB-33`、`RB-33c`、`RB-34`、`RB-34c` 推送前取得对应共享文件的单写者串行窗口（`G-10`） | `docs/development/plan/plan-20261117-sequencer-am.md:87`（2026-11-21 核对） | 有在途写者时短暂串行等待并 rebase，不阻塞 `RB-01d`..`RB-11o` 独占开发 |
| DEP-RB-03 | `incoming` | 外部网站检出（仅 `RB-30`、`RB-33c`） | `/Volumes/Sea/libra-backend`（严格遵循现行 `AGENTS.md:135` 权威网站交付检出，`cf` 分支） | Operator / Coordinator | 满足「文档与网站验收命令」第 ① 步：默认 `/Volumes/Sea/libra-backend` 已就绪（含 `.libra` 无 `.git`、`origin` 为 `libra-tools/libra-backend`、`## cf...origin/cf`、保留无关改动、目标文件无冲突）；或于 `RB-30`/`RB-33c` 交付前由用户在当前会话先给出新的明确书面裁定批准改用 `/Volumes/Data/gitmono/libra-backend` 并同步修订本表与 `AGENTS.md:135` 后通过同项校验 | `AGENTS.md:135`；2026-11-21 12:36:00 UTC 现场只读实测 `/Volumes/Sea/libra-backend` 当前不存在而 `/Volumes/Data/gitmono/libra-backend` 位于 `cf` | **不阻塞 Track A（`RB-01`..`RB-12`）与 Track B 本地实现（`RB-13`..`RB-29b`、`RB-33`..`RB-33b`）**；进入 `RB-30` 或 `RB-33c` 时若 `/Volumes/Sea/libra-backend` 未就绪且无新的明确用户书面裁定，或任一分支/身份门失败，立即 **STOP** 等待就绪或用户裁定，严禁擅自写入未授权目录 |

### 发布分组与并发窗口

| ID | 成员 | 唯一发布点 | 窗口规则 | 失败回滚顺序 | 理由 |
|---|---|---|---|---|---|
| `REL-RB-01` | `RB-13`..`RB-29`（含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`）、`RB-29d`、`RB-29a`、`RB-29c`、`RB-29b`（29 张 `family child`）+ `RB-30`（`family release point`） | `RB-30` | 全部 29 张子卡在隔离工作区 `../libra-redb-integration` 本地签名提交，窗口内**不推送 `main`、不 bump、不建 tag**；`RB-30` 统一 rebase `origin/main`、通过 `syn 2.0` 逐调用点审计、验证 `base.yml` 已同步切至解锁态测试、双后端全量验收与网站同步后一次发布集成树 | 未推送前直接放弃或修正本地分支；已发布后默认仍是 `storage-sqlite` 不影响默认用户，问题以新 patch 前滚 | 146 个生产接线文件必须全量接入并同步解锁 CLI、`libra-migrate` 与解锁态 CI，分片发版会暴露半接线损坏的 `redb` 后端（`G-08`） |
| `REL-RB-03` | `RB-33`（`implementation` `family child`）、`RB-33b`（`audit` `family child`）+ `RB-33c`（`release` `family release point`） | `RB-33c` | `RB-33` 与 `RB-33b` 在本地分支串联提交，窗口内**不推送 `main`、不 bump、不建 tag**；必须待 `RB-33b` 在切默认后精确暂存双成品上完成仓库/配置/全新 Redb 仓活 Spool 双向回退发布前审计后，由唯一发布点 `RB-33c` 统一执行 `T-2 + T-3`、满足 `DEP-RB-03` 网站同步与 `patch + 1` 原子发布 | 未推送前直接修正本地分支；已发布后官方发行 `libra` 仍内建双后端可无缝打开 `libra-migrate --to sqlite` 回退产物，并可发新 patch 将 `backend.rs` 默认切回 `Sqlite` | 切换官方发行默认后端必须在发布前于精确暂存双成品上一次性证明切后默认建库与双向 SQLite 回退闭环，绝不允许切默认发版后再补回退审计（`G-08`） |
| `REL-RB-02` | `RB-34`（`implementation` `family child`）、`RB-34b`（`audit` `family child`）+ `RB-34c`（`release` `family release point`） | `RB-34c` | `RB-34` 与 `RB-34b` 在本地分支串联提交，窗口内**不推送 `main`、不 bump、不建 tag**；必须待 `RB-34b` 在最终默认+`keyring` 二进制上完成全部 4 类 Global/System 配置读写拒收审计（含 `config set --system` 零建 `config.redb`）后，由唯一发布点 `RB-34c` 统一执行全量双矩阵 `T-2` 与 `patch + 1` 原子发布 | 未推送前直接修正本地分支；已发布后源码构建可显式传 `--no-default-features --features storage-sqlite,keyring`，并可发新 patch 将 `Cargo.toml` 切回 `default = ["storage-sqlite"]` | 切换 `Cargo.toml` 默认 Feature 必须在发布前于最终默认+`keyring` 二进制上一次性证明仓库、`GlobalConfig` 与 `SystemConfig` 全路径读写拒收，绝不允许读门验收发布后再补系统写门审计（`G-08`） |

- **并发声明：** Track A `RB-01d`..`RB-11o`（110 张连续卡）100% 限于 `src/storage/**`，可在隔离工作区与所有其它 Agent 功能计划无阻塞并发开发；`RB-01`/`RB-11c`/`RB-12` 仅在短提交窗按 `DEP-RB-02` 与共享文件串行（`RB-11t` 仅触及 `src/bin/libra_migrate.rs`）。Track B `RB-13`..`RB-29b`（含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`/`RB-29d`/`RB-29a`/`RB-29c`）在隔离分支本地开发期间不阻塞其它 Agent 推 `main`；其中 `RB-17`/`RB-17b`/`RB-18`/`RB-18b`..`RB-20c`/`RB-20b` 与 `RB-21`..`RB-28b`（含 `RB-25b`/`RB-26b`）写集互不相交，可并行实现、由 `RB-29`/`RB-29d`/`RB-29a`/`RB-29c`/`RB-29b`/`RB-30` 串行合流；`RB-32b` 为 `no-release` 只读审计（随 `RB-33c` 发布门进入用户可见渠道前验收，不占独立版本窗口）；`REL-RB-03`（`RB-33`→`RB-33b`→`RB-33c`）与 `REL-RB-02`（`RB-34`→`RB-34b`→`RB-34c`）按序严格串行并分别由 `RB-33c`、`RB-34c` 一次发布。
- **发布者：** 单一授权发布 Agent（`ER-12`），串行维护 `Release write set` 与 `plan-status.md`。
- **发布窗口顺序（共 120 个发布窗口，`RB-32b` 为 `no-release` 不占发布窗口）：** `RB-01` → `RB-01d` → `RB-01b` → `RB-01c` → `RB-02` → `RB-02d` → `RB-02b` → `RB-02c` → `RB-02e` → `RB-03` → `RB-03j` → `RB-03b` → `RB-03h` → `RB-03d` → `RB-03f` → `RB-03e` → `RB-03i` → `RB-03c` → `RB-03g` → `RB-04` → `RB-04b` → `RB-05` → `RB-05b` → `RB-06` → `RB-06c` → `RB-06d` → `RB-06b` → `RB-06e` → `RB-07` → `RB-07g` → `RB-07c` → `RB-07f` → `RB-07d` → `RB-07b` → `RB-07e` → `RB-08` → `RB-08t` → `RB-08d` → `RB-08u` → `RB-08f` → `RB-08b` → `RB-08g` → `RB-08m` → `RB-08n` → `RB-08i` → `RB-08q` → `RB-08k` → `RB-08v` → `RB-08j` → `RB-08r` → `RB-08l` → `RB-08o` → `RB-08s` → `RB-08c` → `RB-08y` → `RB-08z` → `RB-08e` → `RB-08h` → `RB-08x` → `RB-09` → `RB-09b` → `RB-09d` → `RB-09c` → `RB-09e` → `RB-09f` → `RB-09g` → `RB-09h` → `RB-09j` → `RB-09k` → `RB-09i` → `RB-09l` → `RB-10` → `RB-10m` → `RB-10n` → `RB-10a` → `RB-10i` → `RB-10f` → `RB-10j` → `RB-10b` → `RB-10l` → `RB-10e` → `RB-10g` → `RB-10c` → `RB-10k` → `RB-10d` → `RB-10h` → `RB-11` → `RB-11z` → `RB-11q` → `RB-11i` → `RB-11p` → `RB-11d` → `RB-11x` → `RB-11j` → `RB-11y` → `RB-11l` → `RB-11a` → `RB-11r` → `RB-11m` → `RB-11e` → `RB-11s` → `RB-11h` → `RB-11u` → `RB-11b` → `RB-11v` → `RB-11k` → `RB-11w` → `RB-11f` → `RB-11n` → `RB-11g` → `RB-11o` → `RB-11c` → `RB-11t` → `RB-12` → `RB-30`（承载 `REL-RB-01` 全组）→ `RB-31` → `RB-31b` → `RB-32` → `RB-33c`（承载 `REL-RB-03` 全组，前置 `RB-32b` 切前审计）→ `RB-34c`（承载 `REL-RB-02` 全组）。

### Phase 0: 最小编译骨架、四特性矩阵门、后端门控、`redb` 打开器与两阶段清单核心（Track A — `RB-01`、`RB-01d`、`RB-01b`、`RB-01c`、`RB-02`、`RB-02d`、`RB-02b`、`RB-02c`、`RB-02e` 共 9 卡）

- **目标：** 注册可叠加存储 features（含 `storage-redb` 自动蕴含 `storage-queue`）、`redb`/`postcard`/`crc32c` 可选依赖与 `pub mod storage;` 1 行挂接（`RB-01`），在独占 `src/storage/mod.rs` 内验证双存储关闭 `compile_error!`、全部 4 组特征编译矩阵与 `cfg!(feature = "storage-queue")` 编译期断言（`RB-01d`），交付版本化 `postcard` 编解码与错误类型（`RB-01b`）、`StorageBackendKind` 单后端拒收与编译期预集成门控（`RB-01c`）、`MultiWriter` 打开器与分层缓存/有界写锁（`RB-02`）、严格只读 `RepairAborted` 恢复合同（`RB-02d`）、`META_SCHEMA` 句柄迁移围栏（`RB-02b`）、`.libra/storage-format.json` 两阶段原子清单与代际路径解析（`RB-02c`）以及清单前置优先级阻断冲突环境变量覆盖（`RB-02e`）。
- **进入条件：** Codex 计划审阅 `PASS`；已在独立隔离工作区 `../libra-redb-track-a` 检出分支 `plan/20261121-redb-storage-concurrency`；`DEP-RB-02` 就绪。
- **退出条件：** `RB-01`、`RB-01d`、`RB-01b`、`RB-01c`、`RB-02`、`RB-02d`、`RB-02b`、`RB-02c`、`RB-02e` 全部 9 卡均 `done / complete`。

### Phase 1: 双后端 61+25+3 源表、配置域有界锁/先 `hard_link` 墓碑预占/首次建库崩溃恢复与跨库意图重放独占实现（Track A — `RB-03`..`RB-06e` 共 19 卡）

- **目标：** 100% 在 `src/storage/**` 内交付 `ConfigStore` 核心（`RB-03`）、`GC-13` 双域隔离与权限（`RB-03j`）、`GlobalConfig`/`SystemConfig` 作用域 `config-format.json` 清单校验（`RB-03b`）、`ConfigScopeLock` 有界协调锁与非 root 只读降级（`RB-03h`）、`GlobalConfig` 与 `SystemConfig` 非覆写 `hard_link` 预占墓碑与旧二进制竞态防护（`RB-03d`、`RB-03f`）、首次建库 4 步持久化发布状态机与 Global/System 全边界崩溃恢复（`RB-03e`、`RB-03i`）、双特性 SQLite 零破坏分派（`RB-03c`）与 Redb 遇旧库回退/fail-closed（`RB-03g`）、`GitStore` 核心引用/Reflog/桥接原子绑定（`RB-04`）与对象索引/抹除围栏/注释/序号（`RB-04b`）、仓库工作流/工作树/OpLog 状态表（`RB-05`）与 AI/记忆/治理/桥接投影表及 61 表守卫（`RB-05b`）、`CaptureStore` 25 表批量去重与桥接序列确认（`RB-06`）、同事务墓碑与只追加审计不变量（`RB-06c`）、Unix 私密目录与文件权限/`O_NOFOLLOW`/属主强校验（`RB-06d`），以及 `META_CROSS_STORE_INTENT` 持久化跨库意图重放与显式修复清理协调器（`RB-06b`、`RB-06e`）。
- **进入条件：** `RB-02e` `complete`。
- **退出条件：** `RB-03`..`RB-06e` 全部 19 卡均 `done / complete`，零外部文件改动。

### Phase 2: 双通道双校验 CRC32C 帧日志队列独占实现（Track A — `RB-07`..`RB-09l` 共 43 卡）

- **目标：** 100% 在 `src/storage/queue/**` 内按严格可独立验证顺序与 `G-02` 实现测试同卡原则，先交付帧编解码与纯段层生产者恢复（`RB-07`..`RB-08x`，零前置依赖消费者），再交付单写入者消费者与全部消费端恢复/背压实现（`RB-09`..`RB-09l`）：含 16B 自认证帧头与逐字段拒收（`RB-07`..`RB-07d`）、目录/初始 34B `LTM2` 与 Spool 墓碑（`RB-07b`/`RB-07e`）、双副本提交（`RB-08`）、断电持久隔离发布原语（`RB-08t`）与 Case C/D 恢复（`RB-08d`/`RB-08u`/`RB-08f`）、共享容量锁（`RB-08b`）、16 MiB 常规轮转（`RB-08g`）、Case C Step C1 与 `S` 帧留存解码（`RB-08m`）、Case C Step C2 与 `S+1` 追加解码（`RB-08n`）、Case C 幂等崩溃重入与收敛路径（`RB-08i`）、`recovered_valid_end == 0` 跳过 Step C2 守卫（`RB-08q`）、段层嵌套 Case C→D 旧段尾恢复与封存（`RB-08k`）、段层 256 项轮转背压与槽位释放原语（`RB-08j`）、Case D 超限与解码器上界（`RB-08r`）、满 256 表 Case C 跳过 Step C2 分支（`RB-08l`）、满表恢复后段内准入分支（`RB-08o`）与单次调用先 Step ① 恢复再 Step ② 背压拒收写路径插桩屏障（`RB-08s`）、Case A/E/C 前缀守卫（`RB-08c`）、Subcase B1 主/副双方向单副本先判（`RB-08y`/`RB-08z`）、0 段四分法（`RB-08e`）、Subcase B2 守卫（`RB-08h`）与跨通道临时文件清理过滤器（`RB-08x`）；以及单批量写入者（`RB-09`）、高水位镜像与倒退拒收（`RB-09b`）、`LTM2.sealed[]` 认证边界与安全回收（`RB-09d`）、锁序协调（`RB-09c`）、消费者真崩溃 Case C 委托与零提前高水位（`RB-09e`）、已封存段多批（>128 帧）落库与末批跨段（`RB-09f`）、三类活跃段恢复不跳段多批排空与后续追加（`RB-09g`）、已排空后轮转原子零帧推进与拒收（`RB-09h`）、零帧推进崩溃恢复与 `S+1` 游标衔接（`RB-09j`）、跨段后旧段 `S` 回收触发（`RB-09k`）、消费端 `max_seg > active_seg` Case D 恢复委托与嵌套跨段排空（`RB-09i`），以及满 256 封存表高压逐段即时回收降压与全量排空（`RB-09l`）。
- **进入条件：** `RB-06e` `complete`。
- **退出条件：** `RB-07`..`RB-09l` 全部 43 卡均 `done / complete`。

### Phase 3: 回滚日志原子围栏与零泄锁备份双向迁移器、配置切换引擎与独立 CI 压测门禁（Track A 收口 — `RB-10`..`RB-12` 共 43 卡）

- **目标：** 交付正向 `DELETE` 模式排他静默门（`RB-10`）、WAL 三类竞争直切 `DELETE` 静默门（`RB-10m`）与在途 Spool/队列锁静默门（`RB-10n`）、三布局流式导出与离线 WAL 检查点归并（`RB-10a`）、Phase 1 只读 spool/queue 直灌及流式内存预算（`RB-10i`）、单连接逻辑摘要/`VACUUM INTO` 锁安全备份/跨进程排他锁实证（`RB-10f`）与中止源不变性/`Derived` 拒绝（`RB-10j`）、100% SQLite 回滚日志原子围栏与 post-unlink 进度持久化（`RB-10b`）、Phase 2 跨进程读写独占与新旧二进制拒访（`RB-10l`）、预打开连接冷/热 `SELECT`/`sqlite_master`/`INSERT`/`UPDATE`/`DELETE`/DDL/`VACUUM` 逐语句拒访（`RB-10e`）、不移锁原地 spool 墓碑/终态发布与 `GC-13` 双域隔离（`RB-10g`）、正向断电热日志与围栏前漂移恢复（`RB-10c`）及围栏后未摘除与双源各自 `unlink` 后恢复（`RB-10k`）、正向 `pending` 缺席/非哨兵损坏/备份缺损逐项故障拒收（`RB-10d`）、正向跨源漂移/spool 间隙并集/queue 漂移对账（`RB-10h`）、反向 Phase 1 红库写者与全局锁序静默门（`RB-11`）、队列/在途 Spool 竞争静默门（`RB-11z`）与 `capture-spool-consumer.lock` 排斥并发 `drain`/非空 `quarantine/` fail-closed（`RB-11q`）、反向无死锁单趟导出/意图直灌/`application_id` 打标（`RB-11i`）、反向红库/队列 SHA-256 备份与 Phase 1 中止无损（`RB-11p`）、反向 Phase 2 红库围栏与双库 post-unlink 持久化（`RB-11d`）、反向 Phase 2 原地 `capture-queue` 与主动 `capture-spool` `0400` 墓碑及双 Hook 阻断（`RB-11x`）、反向正常 Step 2.4 非覆写 `hard_link` 先发 `capture.db` 与异源拒收（`RB-11j`）、无崩溃竞争 `capture::store::open` 单连接并轨围栏替换（`RB-11y`）、反向终态发布/Step 2.6 解封/双域 `GC-13` 隔离（`RB-11l`）、反向红库漂移与覆盖拒收（`RB-11a`）及双源 post-unlink 与 `pending` 缺席拒收（`RB-11r`）、反向备份失配/队列间隙/异源 `libra.db` 拒收（`RB-11m`）、反向 `targets_staged_for_rename` 同 tag 非覆写续跑（`RB-11e`）与竞争 `capture::store::open` 并轨围栏替换（`RB-11s`）、全新 Redb 仓活 spool 对账与并发 `drain` 排斥（`RB-11h`）及两类可达 Step 2.6 解封崩溃恢复（`RB-11u`）、全局/系统配置正向迁移与原位保留墓碑（`RB-11b`、`RB-11v`）、全局/系统配置反向迁移（`RB-11k`）与双域有界锁争用（`RB-11w`）、配置正向崩溃恢复（`RB-11f`）与故障拒收（`RB-11n`）、配置反向 `unlink(config.redb)` 恢复（`RB-11g`）及 `targets_staged_for_rename` 旧写者保护（`RB-11o`）、`libra-migrate` CLI 注册与 `--dry-run`/幂等（`RB-11c`）及真实成品全 4 标志不可旁路编译期门控（`RB-11t`），以及独立 `compat-redb-storage` CI 压测门禁与 `compat-offline-core` 零修改比对（`RB-12`）。
- **进入条件：** `RB-09l` `complete`。
- **退出条件：** `RB-10`..`RB-12` 全部 43 卡均 `done / complete`。

### Phase 4: `REL-RB-01` 146 文件原子接线家族、解锁态 CI 过渡与 `syn 2.0` 完备审计（Track B — `RB-13`..`RB-30`，含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`/`RB-29d`/`RB-29a`/`RB-29c`/`RB-29b` 共 30 卡）

- **目标：** 在隔离分支 `../libra-redb-integration` 完成全部 146 个生产接线文件双签名保签接线（24 张接线子卡）、后端感知 Hook 队列与纯 `--features storage-redb` 捕获队列 E2E（`RB-29`）、单写入者守护与 `agent doctor` 双通道高水位/隔离指标（`RB-29d`）、CLI+`libra-migrate` 联合解锁（`RB-29a`）、解锁态二进制往返测试与 `base.yml` 解锁态 CI 选测原子切换及 `compat-offline-core` 零修改比对（`RB-29c`）、`syn 2.0` 语法感知逐调用点完备审计（`RB-29b`），并经 `RB-30` 原子发布集成树（默认仍为 `storage-sqlite`）。
- **进入条件：** `RB-12` `complete`；创建独立集成工作区 `../libra-redb-integration`。
- **退出条件：** `RB-30` `done / complete`（传递闭合 `RB-13`..`RB-29b` 全部 29 张子卡）。

### Phase 5: 全量双矩阵与 `keyring` CI、双二进制协同打包与切前 `no-release` 审计、`REL-RB-03` 运行时切默认家族与 `REL-RB-02` Cargo 默认切换家族（Track B — `RB-31`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c`、`RB-34`、`RB-34b`、`RB-34c` 共 10 卡）

- **目标：** 显式锁定全量 SQLite 与 Redb 离线 CI 矩阵（`RB-31`）并新增纯红库+`keyring` 与官方双后端+`keyring` CI 步骤（`RB-31b`）；更新 `release.yml` 分别按真实特性构建并经独立 `mktemp -d` 暂存双后端 `libra`（含 `keyring`）与 `libra-migrate`（不含 `keyring`）（`RB-32`）并执行 `no-release` 切前互操作与默认 SQLite 不变只读审计（`RB-32b`，随 `RB-33c` 发布前验收）；经 `REL-RB-03` 家族完成官方发行运行时默认切红库（`RB-33` 切换 `src/storage/backend.rs`、验证默认红库建仓/配置与同步仓内文档 + `RB-33b` 精确暂存双成品切后仓库/配置/全新仓活 Spool 双向 SQLite 回退发布前审计 + `RB-33c` 家族唯一原子发布点及按 `DEP-RB-03` 授权的网站同步）；最后经 `REL-RB-02` 家族完成 Cargo 默认 Feature 切红库（`RB-34` 切换 `Cargo.toml` 默认 Feature 与独立目标目录纯默认+`keyring`/SQLite 回退构建及仓库未迁移拒收 + `RB-34b` 最终默认+`keyring` 二进制全 4 类 Global/System 配置读写拒收审计 + `RB-34c` 家族原子发布点）。
- **进入条件：** `RB-30` `complete`。
- **退出条件：** `RB-31`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33c`（传递闭合 `RB-33`、`RB-33b`）、`RB-34c`（传递闭合 `RB-34`、`RB-34b`）均 `done / complete`，计划「完成判据」全勾。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`（`RB-13`..`RB-29b` 含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`/`RB-29d`/`RB-29a`/`RB-29c` 覆写为 `family child of REL-RB-01`；`RB-30` 为 `family release point of REL-RB-01`；`RB-32b` 为 `no-release`；`RB-33` 与 `RB-33b` 覆写为 `family child of REL-RB-03`；`RB-33c` 为 `family release point of REL-RB-03`；`RB-34` 与 `RB-34b` 覆写为 `family child of REL-RB-02`；`RB-34c` 为 `family release point of REL-RB-02`）
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`
- **Migration and rollback 默认:** `N/A`
- **Security and privacy 默认:** 继承 `GC-07`、`GC-11`、`GC-RB-03`
- **Performance budget 默认:** 继承 `GC-10`、`ADR-RB-03`

**默认覆盖：**

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| `RB-10`、`RB-10a`、`RB-10i`、`RB-10f`、`RB-10j`、`RB-10b`、`RB-10l`、`RB-10e`、`RB-10g`、`RB-10c`、`RB-10k`、`RB-10d`、`RB-10h`、`RB-11`、`RB-11q`、`RB-11i`、`RB-11p`、`RB-11d`、`RB-11x`、`RB-11j`、`RB-11y`、`RB-11l`、`RB-11a`、`RB-11r`、`RB-11m`、`RB-11e`、`RB-11s`、`RB-11h`、`RB-11u`、`RB-11b`、`RB-11v`、`RB-11k`、`RB-11w`、`RB-11f`、`RB-11n`、`RB-11g`、`RB-11o`、`RB-11c`、`RB-11t` | `Task type` / `Rollback mode` | `migration` / `forward-only`：双向回滚日志原子围栏离线迁移器，支持 `.libra/sqlite-backup/<gen>/`、`.libra/redb-backup/<gen>/`、配置域原位保留墓碑/反向 staged-rename 保护与双向无损互迁 |
| `RB-13`..`RB-29`（含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`）、`RB-29d`、`RB-29a`、`RB-29c`、`RB-29b` | `Release boundary` / `Version increment` / `Release write set` / `Full-suite trigger` / `C/D coverage from` | `family child of REL-RB-01` / `N/A` / `N/A` / `N/A` / `RB-30`（隔离本地集成提交，由 `RB-30` 原子发布与承接 `T-2` 全量；其中 `RB-29b` 为 `Task type = audit`） |
| `RB-29` | `Rollback mode` | `forward-only`：已写入 `capture-queue/` 的帧通过 `libra __capture-queue-drain` 排空 |
| `RB-29b` | `Task type` / `Rollback mode` | `audit` / `revert`：`syn 2.0` 逐调用点 AST 守卫模块与负向自测（不改生产代码） |
| `RB-30` | `Task type` / `Release boundary` | `release` / `family release point of REL-RB-01`（默认仍为 `storage-sqlite`） |
| `RB-32b` | `Task type` / `Release boundary` / `Version increment` / `Release write set` / `C/D coverage from` / `Rollback mode` | `audit` / `no-release` / `N/A` / `N/A` / `RB-33c` / `revert`：遵循 `G-07` `audit` 不 bump 版本、不产出 artifact，不改生产代码，在切默认前对精确暂存双后端 `libra` 与无 `keyring` `libra-migrate` 成品执行双向互操作与 SQLite 默认不变只读审计（随 `RB-33c` 发布门进入用户可见渠道前验收） |
| `RB-33` | `Release boundary` / `Version increment` / `Release write set` / `Full-suite trigger` / `C/D coverage from` / `Rollback mode` | `family child of REL-RB-03` / `N/A` / `N/A` / `N/A` / `RB-33c` / `forward-only`（与 `RB-33b` 同家族串联验证后由 `RB-33c` 统一原子发布） |
| `RB-33b` | `Task type` / `Release boundary` / `Version increment` / `Release write set` / `Full-suite trigger` / `C/D coverage from` / `Rollback mode` | `audit` / `family child of REL-RB-03` / `N/A` / `N/A` / `N/A` / `RB-33c` / `revert`（不改生产代码，在切默认后精确暂存双成品上实测仓库/配置/全新仓活 Spool 双向 SQLite 回退与 Step 2.6 解封） |
| `RB-33c` | `Task type` / `Release boundary` / `Rollback mode` | `release` / `family release point of REL-RB-03` / `forward-only`（唯一发布点，承接 `REL-RB-03` `T-2 + T-3` 全量双矩阵验收、`DEP-RB-03` 网站同步与 `patch + 1` 发布） |
| `RB-34` | `Release boundary` / `Version increment` / `Release write set` / `Full-suite trigger` / `C/D coverage from` / `Rollback mode` | `family child of REL-RB-02` / `N/A` / `N/A` / `N/A` / `RB-34c` / `forward-only`（与 `RB-34b` 同家族串联验证后由 `RB-34c` 统一原子发布） |
| `RB-34b` | `Task type` / `Release boundary` / `Version increment` / `Release write set` / `Full-suite trigger` / `C/D coverage from` / `Rollback mode` | `audit` / `family child of REL-RB-02` / `N/A` / `N/A` / `N/A` / `RB-34c` / `revert`（不改生产代码，在最终默认+`keyring` 二进制上实测全部 4 类 Global/System 配置读写拒收） |
| `RB-34c` | `Task type` / `Release boundary` / `Rollback mode` | `release` / `family release point of REL-RB-02` / `forward-only`（唯一发布点，承接 `REL-RB-02` `T-2` 全量双矩阵验证与 `patch + 1` 发布） |

**规则 waiver（`EX-*`）：** 无（全部 154 张卡均同时满足 `G-02` 实现与测试同卡交付与 `G-03` 独立 pass/fail 谓词上限，无超限卡；`RB-32b` `audit` 严格遵守 `G-07` `no-release`）。

#### 任务卡粒度审计表

> 计数口径严格遵循 `plan-template.md` `G-03`：每张 `implementation`/`migration` 卡按可独立失败的 pass/fail 谓词总数核算均 ≤ 8（`release` ≤ 12、`audit` ≤ 20），不以长句合并规避上限；`VER` 仅统计本卡特有独立验证门（不含 `ER-04` 强制门与 `ER-06a` 同卡必需文档同步门）；`landing / prod-files` 严格按实际行为生产目录与生产文件数计（不含测试文件、`Cargo.lock` 与 `ER-06a` 随附文档）。

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `RB-01` | implementation | cargo storage features optional deps and lib.rs hook | revert or patch | yes | yes | 8/8 | 8/8 | 3/3 | M | DEP-RB-02 | no-overlap | independent | N/A | N/A |
| `RB-01d` | implementation | 4-way feature matrix compile-error and redb-queue assert | revert isolated module | yes | yes | 6/8 | 6/8 | 1/1 | S | RB-01 | 序列化于 RB-01 | independent | RB-01 | N/A |
| `RB-01b` | implementation | versioned postcard codec and backend error | revert isolated module | yes | yes | 5/8 | 3/8 | 1/3 | S | RB-01d | 序列化于 RB-01d | independent | RB-01 | N/A |
| `RB-01c` | implementation | StorageBackendKind feature dispatch and gate | revert isolated module | yes | yes | 4/8 | 5/8 | 1/2 | S | RB-01b | 序列化于 RB-01b | independent | RB-01b | N/A |
| `RB-02` | implementation | redb multiwriter opener cache and bounded write | revert isolated module | yes | yes | 6/8 | 3/8 | 1/2 | M | RB-01c | 序列化于 RB-01c | independent | RB-01 | N/A |
| `RB-02d` | implementation | strict readonly RepairAborted and explicit repair | revert isolated module | yes | yes | 4/8 | 3/8 | 1/2 | S | RB-02 | 序列化于 RB-02 | independent | RB-02 | N/A |
| `RB-02b` | implementation | meta schema role version and handle fence | revert isolated module | yes | yes | 4/8 | 3/8 | 1/2 | M | RB-02d | 序列化于 RB-02d | independent | RB-02 | N/A |
| `RB-02c` | implementation | 2-phase manifest atomic publish and gen paths | revert isolated module | yes | yes | 5/8 | 3/8 | 1/2 | M | RB-02b | 序列化于 RB-02b | independent | RB-02b | N/A |
| `RB-02e` | implementation | manifest-first precedence over env backend | revert isolated module | yes | yes | 5/8 | 3/8 | 1/3 | S | RB-02c | 序列化于 RB-02c | independent | RB-02c | N/A |
| `RB-03` | implementation | dual-backend ConfigStore crud cas and mvcc | revert isolated module | yes | yes | 5/8 | 4/8 | 3/6 | M | RB-02e | 序列化于 RB-02e | independent | N/A | N/A |
| `RB-03j` | implementation | redb config gc13 isolation and file modes | revert isolated module | yes | yes | 4/8 | 3/8 | 3/6 | S | RB-03 | 序列化于 RB-03 | independent | RB-03 | N/A |
| `RB-03b` | implementation | config scope manifest validation and absent ro | revert isolated module | yes | yes | 4/8 | 3/8 | 3/6 | M | RB-03j | 序列化于 RB-03j | independent | RB-03 | N/A |
| `RB-03h` | implementation | config scope bounded lock and ro-dir fallback | revert isolated module | yes | yes | 4/8 | 4/8 | 3/6 | M | RB-03b | 序列化于 RB-03b | independent | RB-03b | N/A |
| `RB-03d` | implementation | global config first-write hardlink legacy-race | revert isolated module | yes | yes | 5/8 | 5/8 | 3/6 | M | RB-03h | 序列化于 RB-03h | independent | RB-03b | N/A |
| `RB-03f` | implementation | system config first-write hardlink legacy-race | revert isolated module | yes | yes | 4/8 | 4/8 | 3/6 | M | RB-03d | 序列化于 RB-03d | independent | RB-03d | N/A |
| `RB-03e` | implementation | global config fresh-create state crash recovery | revert isolated module | yes | yes | 4/8 | 4/8 | 3/6 | M | RB-03f | 序列化于 RB-03f | independent | RB-03b | N/A |
| `RB-03i` | implementation | system config fresh-create crash recovery modes | revert isolated module | yes | yes | 4/8 | 3/8 | 3/6 | S | RB-03e | 序列化于 RB-03e | independent | RB-03e | N/A |
| `RB-03c` | implementation | config dual-feature sqlite runtime dispatch | revert isolated module | yes | yes | 5/8 | 4/8 | 3/6 | M | RB-03i | 序列化于 RB-03i | independent | RB-03b | N/A |
| `RB-03g` | implementation | config redb runtime legacy config-db policy | revert isolated module | yes | yes | 4/8 | 4/8 | 3/6 | M | RB-03c | 序列化于 RB-03c | independent | RB-03c | N/A |
| `RB-04` | implementation | dual-backend GitStore refs reflog bridge-cas | revert isolated module | yes | yes | 6/8 | 5/8 | 3/4 | M | RB-03g | 序列化于 RB-03g | independent | N/A | N/A |
| `RB-04b` | implementation | dual-backend GitStore obj-index obliterate notes | revert isolated module | yes | yes | 5/8 | 4/8 | 3/4 | M | RB-04 | 序列化于 RB-04 | independent | RB-04 | N/A |
| `RB-05` | implementation | dual-backend GitStore workflow worktree oplog 22 | revert isolated module | yes | yes | 5/8 | 5/8 | 3/4 | M | RB-04b | 序列化于 RB-04b | independent | N/A | N/A |
| `RB-05b` | implementation | dual-backend GitStore ai memory bridge 28 and 61 | revert isolated module | yes | yes | 6/8 | 5/8 | 3/4 | M | RB-05 | 序列化于 RB-05 | independent | RB-05 | N/A |
| `RB-06` | implementation | dual-backend CaptureStore 25 tables batch bridge | revert isolated module | yes | yes | 6/8 | 5/8 | 3/4 | M | RB-05b | 序列化于 RB-05b | independent | N/A | N/A |
| `RB-06c` | implementation | CaptureStore tombstone and append-only invariants | revert isolated module | yes | yes | 5/8 | 3/8 | 2/4 | M | RB-06 | 序列化于 RB-06 | independent | RB-06 | N/A |
| `RB-06d` | implementation | CaptureStore unix private dir/file posture | revert isolated module | yes | yes | 6/8 | 3/8 | 2/4 | S | RB-06c | 序列化于 RB-06c | independent | RB-06c | N/A |
| `RB-06b` | implementation | cross-store durable intent replay and guard | revert isolated module | yes | yes | 4/8 | 4/8 | 2/4 | M | RB-06d | 序列化于 RB-06d | independent | RB-06 | N/A |
| `RB-06e` | implementation | cross-store reconcile snapshot and intent pruning | revert isolated module | yes | yes | 4/8 | 3/8 | 2/4 | S | RB-06b | 序列化于 RB-06b | independent | RB-06b | N/A |
| `RB-07` | implementation | capture-queue 16B header+frame crc32c codec | revert isolated module | yes | yes | 4/8 | 3/8 | 2/3 | M | RB-06e | 序列化于 RB-06e | independent | N/A | N/A |
| `RB-07g` | implementation | capture-queue frame encode bounds and body-len guard | revert isolated module | yes | yes | 4/8 | 3/8 | 2/3 | S | RB-07 | 序列化于 RB-07 | independent | RB-07 | N/A |
| `RB-07c` | implementation | capture-queue 16B header per-field pre-alloc reject | revert isolated module | yes | yes | 4/8 | 3/8 | 2/3 | S | RB-07g | 序列化于 RB-07g | independent | RB-07 | N/A |
| `RB-07f` | implementation | capture-queue seq decoder clean-eof truncated-tail | revert isolated module | yes | yes | 4/8 | 3/8 | 2/3 | S | RB-07c | 序列化于 RB-07c | independent | RB-07c | N/A |
| `RB-07d` | implementation | capture-queue body and frame-crc per-field reject | revert isolated module | yes | yes | 5/8 | 3/8 | 2/3 | S | RB-07f | 序列化于 RB-07f | independent | RB-07c | N/A |
| `RB-07b` | implementation | capture-queue lane posture and init dual-meta | revert isolated module | yes | yes | 5/8 | 3/8 | 2/3 | M | RB-07d | 序列化于 RB-07d | independent | RB-07 | N/A |
| `RB-07e` | implementation | fresh redb capture-spool 0400 tombstones | revert isolated module | yes | yes | 5/8 | 3/8 | 2/3 | S | RB-07b | 序列化于 RB-07b | independent | RB-07b | N/A |
| `RB-08` | implementation | capture-queue LTM2 dual tail-meta and case-c | revert isolated module | yes | yes | 6/8 | 4/8 | 2/3 | M | RB-07e | 序列化于 RB-07e | independent | RB-07 | N/A |
| `RB-08t` | implementation | capture-queue durable quarantine before truncate unlink | revert isolated module | yes | yes | 8/8 | 4/8 | 2/3 | M | RB-08 | 序列化于 RB-08 | independent | RB-08 | N/A |
| `RB-08d` | implementation | capture-queue base case-d clean old-seg and valid succ | revert isolated module | yes | yes | 7/8 | 3/8 | 2/3 | M | RB-08t | 序列化于 RB-08t | independent | RB-08 | N/A |
| `RB-08u` | implementation | case-d valid successor trailing half-frame durable quarantine | revert isolated module | yes | yes | 7/8 | 4/8 | 2/3 | M | RB-08d | 序列化于 RB-08d | independent | RB-08d | N/A |
| `RB-08f` | implementation | capture-queue case-d orphan unlink fsync quarantine | revert isolated module | yes | yes | 8/8 | 3/8 | 2/3 | S | RB-08u | 序列化于 RB-08u | independent | RB-08d | N/A |
| `RB-08b` | implementation | capture-queue cross-lane 256mib cap timeout latency | revert isolated module | yes | yes | 4/8 | 4/8 | 2/3 | M | RB-08f | 序列化于 RB-08f | independent | RB-08 | N/A |
| `RB-08g` | implementation | capture-queue base 16mib rotation and create_segment_file | revert isolated module | yes | yes | 8/8 | 3/8 | 2/3 | S | RB-08b | 序列化于 RB-08b | independent | RB-08b | N/A |
| `RB-08m` | implementation | capture-queue case-c step-c1 repair and s-frame retention | revert isolated module | yes | yes | 8/8 | 3/8 | 2/3 | M | RB-08g | 序列化于 RB-08g | independent | RB-08g | N/A |
| `RB-08n` | implementation | capture-queue base case-c step-c2 seal and s1 append | revert isolated module | yes | yes | 7/8 | 3/8 | 2/3 | M | RB-08m | 序列化于 RB-08m | independent | RB-08m | N/A |
| `RB-08i` | implementation | case-c crash boundaries 1-3 deterministic injection | revert isolated module | yes | yes | 8/8 | 3/8 | 2/3 | S | RB-08n | 序列化于 RB-08n | independent | RB-08g | N/A |
| `RB-08q` | implementation | case-c zero-byte first-frame crash no-seal guard | revert isolated module | yes | yes | 4/8 | 3/8 | 2/3 | S | RB-08i | 序列化于 RB-08i | independent | RB-08i | N/A |
| `RB-08k` | implementation | segment-layer nested case c-to-d clean old-tail seal | revert isolated module | yes | yes | 6/8 | 3/8 | 2/3 | M | RB-08q | 序列化于 RB-08q | independent | RB-08d | N/A |
| `RB-08v` | implementation | segment-layer nested case c-to-d damaged old-tail quarantine | revert isolated module | yes | yes | 8/8 | 4/8 | 2/3 | M | RB-08k | 序列化于 RB-08k | independent | RB-08k | N/A |
| `RB-08j` | implementation | segment-layer 256-cap normal rotation backpressure | revert isolated module | yes | yes | 8/8 | 3/8 | 2/3 | S | RB-08v | 序列化于 RB-08v | independent | RB-08g | N/A |
| `RB-08r` | implementation | segment-layer case-d 256-cap and ltm2 decoder bounds | revert isolated module | yes | yes | 6/8 | 3/8 | 2/3 | S | RB-08j | 序列化于 RB-08j | independent | RB-08j | N/A |
| `RB-08l` | implementation | segment-layer 256-cap case-c skip-step-c2 guard | revert isolated module | yes | yes | 7/8 | 3/8 | 2/3 | M | RB-08r | 序列化于 RB-08r | independent | RB-08j | N/A |
| `RB-08o` | implementation | segment-layer 256-cap post-recovery in-segment admit | revert isolated module | yes | yes | 5/8 | 3/8 | 2/3 | S | RB-08l | 序列化于 RB-08l | independent | RB-08l | N/A |
| `RB-08s` | implementation | segment-layer single-call step1-recover step2-reject barrier | revert isolated module | yes | yes | 8/8 | 3/8 | 2/3 | M | RB-08o | 序列化于 RB-08o | independent | RB-08o | N/A |
| `RB-08c` | implementation | capture-queue valid-meta case-a-e-c prefix guards | revert isolated module | yes | yes | 6/8 | 4/8 | 2/3 | M | RB-08s | 序列化于 RB-08s | independent | RB-08b | N/A |
| `RB-08y` | implementation | capture-queue subcase-b1 single-replica phase1 guard | revert isolated module | yes | yes | 5/8 | 3/8 | 2/3 | S | RB-08c | 序列化于 RB-08c | independent | RB-08c | N/A |
| `RB-08z` | implementation | capture-queue subcase-b1 backup-replica symmetric guard | revert isolated module | yes | yes | 5/8 | 3/8 | 2/3 | S | RB-08y | 序列化于 RB-08y | independent | RB-08y | N/A |
| `RB-08e` | implementation | capture-queue zero-segment 4-way missing-active guard | revert isolated module | yes | yes | 6/8 | 4/8 | 2/3 | S | RB-08z | 序列化于 RB-08z | independent | RB-08c | N/A |
| `RB-08h` | implementation | capture-queue nonempty subcase-b2 dual-loss guard | revert isolated module | yes | yes | 8/8 | 4/8 | 2/3 | S | RB-08e | 序列化于 RB-08e | independent | RB-08e | N/A |
| `RB-08x` | implementation | capture-queue cross-lane quarantine-tmp cleanup filter | revert isolated module | yes | yes | 8/8 | 3/8 | 2/3 | S | RB-08h | 序列化于 RB-08h | independent | RB-08h | N/A |
| `RB-09` | implementation | capture-queue priority-first batch writer and gc | revert isolated module | yes | yes | 5/8 | 4/8 | 2/4 | M | RB-08x | 序列化于 RB-08x | independent | RB-07 | N/A |
| `RB-09b` | implementation | capture-queue writer mirror crash prefix dual-loss lock | revert isolated module | yes | yes | 7/8 | 5/8 | 2/4 | M | RB-09 | 序列化于 RB-09 | independent | RB-09 | N/A |
| `RB-09d` | implementation | capture-queue LTM2-sealed rotated boundary and safe gc | revert isolated module | yes | yes | 5/8 | 4/8 | 2/5 | M | RB-09b | 序列化于 RB-09b | independent | RB-09b | N/A |
| `RB-09c` | implementation | capture-queue consumer append-lock paused-producer mutex | revert isolated module | yes | yes | 4/8 | 4/8 | 2/5 | M | RB-09d | 序列化于 RB-09d | independent | RB-09b | N/A |
| `RB-09e` | implementation | capture-queue consumer tail recovery no-hw-advance guard | revert isolated module | yes | yes | 6/8 | 4/8 | 2/5 | M | RB-09c | 序列化于 RB-09c | independent | RB-09c | N/A |
| `RB-09f` | implementation | capture-queue sealed-seg multibatch recovery ingest and hw | revert isolated module | yes | yes | 5/8 | 3/8 | 2/5 | M | RB-09e | 序列化于 RB-09e | independent | RB-09e | N/A |
| `RB-09g` | implementation | capture-queue active-seg recovery multibatch hw and append | revert isolated module | yes | yes | 6/8 | 5/8 | 2/5 | M | RB-09f | 序列化于 RB-09f | independent | RB-09f | N/A |
| `RB-09h` | implementation | capture-queue drained-rotated empty advance and auth guards | revert isolated module | yes | yes | 6/8 | 4/8 | 2/5 | M | RB-09g | 序列化于 RB-09g | independent | RB-09g | N/A |
| `RB-09j` | implementation | capture-queue empty advance crash and s1 drain | revert isolated module | yes | yes | 5/8 | 4/8 | 2/5 | M | RB-09h | 序列化于 RB-09h | independent | RB-09h | N/A |
| `RB-09k` | implementation | capture-queue empty-advance old-s ordered reclamation | revert isolated module | yes | yes | 4/8 | 3/8 | 2/5 | S | RB-09j | 序列化于 RB-09j | independent | RB-09j | N/A |
| `RB-09i` | implementation | consumer max_seg case-d delegation and nested drain | revert isolated module | yes | yes | 6/8 | 3/8 | 2/5 | M | RB-09k | 序列化于 RB-09k | independent | RB-08k | N/A |
| `RB-09l` | implementation | consumer 256-cap eager reclaim and full-table drain | revert isolated module | yes | yes | 6/8 | 4/8 | 2/5 | M | RB-09i | 序列化于 RB-09i | independent | RB-09i | N/A |
| `RB-10` | migration | forward phase-1 delete-mode exclusive quiescence gate | clean tmp sources intact | yes | yes | 4/8 | 3/8 | 1/2 | S | RB-09l | 序列化于 RB-09l | independent | N/A | N/A |
| `RB-10m` | migration | forward phase-1 wal reader-writer-idle direct delete gate | clean tmp sources intact | yes | yes | 6/8 | 3/8 | 1/2 | M | RB-10 | 序列化于 RB-10 | independent | RB-10 | N/A |
| `RB-10n` | migration | forward phase-1 inflight spool-tmp and queue-lock gate | clean tmp sources intact | yes | yes | 8/8 | 3/8 | 1/2 | S | RB-10m | 序列化于 RB-10m | independent | RB-10m | N/A |
| `RB-10a` | migration | forward phase-1 offline-wal checkpoint and 3-layout export | clean tmp sources intact | yes | yes | 6/8 | 5/8 | 1/2 | M | RB-10n | 序列化于 RB-10n | independent | RB-10 | N/A |
| `RB-10i` | migration | forward phase-1 spool queue ingest stream budget | clean tmp sources intact | yes | yes | 4/8 | 4/8 | 1/2 | S | RB-10a | 序列化于 RB-10a | independent | RB-10a | N/A |
| `RB-10f` | migration | forward phase-1 digest vacuum backup os-lock | clean tmp sources intact | yes | yes | 5/8 | 4/8 | 1/2 | M | RB-10i | 序列化于 RB-10i | independent | RB-10a | N/A |
| `RB-10j` | migration | forward phase-1 backup fsync abort and derived-rej | clean tmp sources intact | yes | yes | 4/8 | 4/8 | 1/2 | S | RB-10f | 序列化于 RB-10f | independent | RB-10f | N/A |
| `RB-10b` | migration | forward phase-2 journaled fence and post-unlink progress | resume idempotent phase-2 | yes | yes | 5/8 | 4/8 | 1/2 | M | RB-10j | 序列化于 RB-10j | independent | RB-10 | N/A |
| `RB-10l` | migration | forward phase-2 os read/write exclusivity and legacy reject | resume idempotent phase-2 | yes | yes | 3/8 | 3/8 | 1/2 | S | RB-10b | 序列化于 RB-10b | independent | RB-10b | N/A |
| `RB-10e` | migration | forward phase-2 open-handle sql stmt rejection | resume idempotent phase-2 | yes | yes | 8/8 | 5/8 | 1/2 | M | RB-10l | 序列化于 RB-10l | independent | RB-10b | N/A |
| `RB-10g` | migration | forward phase-2 spool tombstones finalize gc13 | resume idempotent phase-2 | yes | yes | 5/8 | 5/8 | 1/2 | M | RB-10e | 序列化于 RB-10e | independent | RB-10e | N/A |
| `RB-10c` | migration | forward phase-2 hot-journal and pre-fence drift | resume idempotent phase-2 | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-10g | 序列化于 RB-10g | independent | RB-10b | N/A |
| `RB-10k` | migration | forward phase-2 post-fence and each post-unlink | resume idempotent phase-2 | yes | yes | 4/8 | 5/8 | 1/2 | M | RB-10c | 序列化于 RB-10c | independent | RB-10c | N/A |
| `RB-10d` | migration | forward phase-2 missing non-sentinel backup reject | resume idempotent phase-2 | yes | yes | 6/8 | 4/8 | 1/2 | M | RB-10k | 序列化于 RB-10k | independent | RB-10c | N/A |
| `RB-10h` | migration | forward phase-2 cross-source spool and queue gap | resume idempotent phase-2 | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-10d | 序列化于 RB-10d | independent | RB-10d | N/A |
| `RB-11` | migration | reverse phase-1 redb-writer quiescence and lock-order | clean tmp sources intact | yes | yes | 5/8 | 3/8 | 1/2 | M | RB-10h | 序列化于 RB-10h | independent | N/A | N/A |
| `RB-11z` | migration | reverse phase-1 queue-lock and inflight-spool-tmp gate | clean tmp sources intact | yes | yes | 8/8 | 3/8 | 1/2 | S | RB-11 | 序列化于 RB-11 | independent | RB-11 | N/A |
| `RB-11q` | migration | reverse phase-1 consumer-lock drain quarantine | clean tmp sources intact | yes | yes | 6/8 | 3/8 | 1/2 | S | RB-11z | 序列化于 RB-11z | independent | RB-11 | N/A |
| `RB-11i` | migration | reverse phase-1 export intent queue and app-id | clean tmp sources intact | yes | yes | 6/8 | 4/8 | 1/2 | M | RB-11q | 序列化于 RB-11q | independent | RB-11 | N/A |
| `RB-11p` | migration | reverse phase-1 redb-backup integrity and abort | clean tmp sources intact | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-11i | 序列化于 RB-11i | independent | RB-11i | N/A |
| `RB-11d` | migration | reverse phase-2 redb fence and post-unlink progress | resume idempotent phase-2 | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-11p | 序列化于 RB-11p | independent | RB-11 | N/A |
| `RB-11x` | migration | reverse phase-2 inplace queue and active spool tombstones | resume idempotent phase-2 | yes | yes | 5/8 | 3/8 | 1/2 | S | RB-11d | 序列化于 RB-11d | independent | RB-11d | N/A |
| `RB-11j` | migration | reverse phase-2 hardlink publish and foreign-tag reject | resume idempotent phase-2 | yes | yes | 5/8 | 4/8 | 1/2 | M | RB-11x | 序列化于 RB-11x | independent | RB-11d | N/A |
| `RB-11y` | migration | reverse step24 no-crash competing capture.db merge fence | resume idempotent phase-2 | yes | yes | 5/8 | 3/8 | 1/2 | S | RB-11j | 序列化于 RB-11j | independent | RB-11j | N/A |
| `RB-11l` | migration | reverse phase-2 finalize step26 unfence gc13 | resume idempotent phase-2 | yes | yes | 5/8 | 5/8 | 1/2 | M | RB-11y | 序列化于 RB-11y | independent | RB-11j | N/A |
| `RB-11a` | migration | reverse phase-2 overrides idempotent and drift | resume idempotent phase-2 | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-11l | 序列化于 RB-11l | independent | RB-11 | N/A |
| `RB-11r` | migration | reverse phase-2 both post-unlink and pending miss | resume idempotent phase-2 | yes | yes | 4/8 | 5/8 | 1/2 | M | RB-11a | 序列化于 RB-11a | independent | RB-11a | N/A |
| `RB-11m` | migration | reverse phase-2 backup-reject queue-gap foreign | resume idempotent phase-2 | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-11r | 序列化于 RB-11r | independent | RB-11e | N/A |
| `RB-11e` | migration | reverse phase-2 staged hardlink same-tag resume | resume idempotent phase-2 | yes | yes | 4/8 | 3/8 | 1/2 | S | RB-11m | 序列化于 RB-11m | independent | RB-11a | N/A |
| `RB-11s` | migration | reverse phase-2 competing capture-open reconcile | resume idempotent phase-2 | yes | yes | 4/8 | 3/8 | 1/2 | M | RB-11e | 序列化于 RB-11e | independent | RB-11e | N/A |
| `RB-11h` | migration | reverse fresh-redb live-spool reconcile drain-lock | resume idempotent phase-2 | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-11s | 序列化于 RB-11s | independent | RB-11e | N/A |
| `RB-11u` | migration | reverse phase-2 step26 reachable unfence crashes | resume idempotent phase-2 | yes | yes | 5/8 | 4/8 | 1/2 | S | RB-11h | 序列化于 RB-11h | independent | RB-11h | N/A |
| `RB-11b` | migration | global config forward retained 0400 tombstone | clean tmp or reverse migrate | yes | yes | 4/8 | 3/8 | 1/2 | M | RB-11u | 序列化于 RB-11u | independent | RB-11 | N/A |
| `RB-11v` | migration | system config forward retained 0444 tombstone | clean tmp or reverse migrate | yes | yes | 4/8 | 3/8 | 1/2 | S | RB-11b | 序列化于 RB-11b | independent | RB-11b | N/A |
| `RB-11k` | migration | global-sys config reverse replace tombstone | clean tmp or forward migrate | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-11v | 序列化于 RB-11v | independent | RB-11b | N/A |
| `RB-11w` | migration | global-sys config 4-case bounded lock contention | clean tmp or forward migrate | yes | yes | 4/8 | 3/8 | 1/2 | S | RB-11k | 序列化于 RB-11k | independent | RB-11k | N/A |
| `RB-11f` | migration | global-sys config forward hot-journal staged-rec | resume idempotent phase-2 | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-11w | 序列化于 RB-11w | independent | RB-11b | N/A |
| `RB-11n` | migration | global-sys config forward fault and pending-rej | resume idempotent phase-2 | yes | yes | 6/8 | 4/8 | 1/2 | M | RB-11f | 序列化于 RB-11f | independent | RB-11f | N/A |
| `RB-11g` | migration | global-sys config reverse unlink and fault-reject | resume idempotent phase-2 | yes | yes | 6/8 | 4/8 | 1/2 | M | RB-11n | 序列化于 RB-11n | independent | RB-11b | N/A |
| `RB-11o` | migration | global-sys config reverse staged-rename recovery | resume idempotent phase-2 | yes | yes | 4/8 | 4/8 | 1/2 | M | RB-11g | 序列化于 RB-11g | independent | RB-11g | N/A |
| `RB-11c` | migration | libra-migrate bin registration dry-run reentry | revert binary registration | yes | yes | 5/8 | 5/8 | 3/4 | M | RB-11o,DEP-RB-02 | 序列化于 RB-11o | independent | RB-11b | N/A |
| `RB-11t` | migration | libra-migrate release binary 4-flag pre-unlock | revert bin gate check | yes | yes | 4/8 | 2/8 | 1/1 | S | RB-11c | 序列化于 RB-11c | independent | RB-11c | N/A |
| `RB-12` | implementation | isolated redb ci job stress and job-diff guard | revert workflow job | yes | yes | 6/8 | 7/8 | 1/1 | S | RB-11t,DEP-RB-02 | 序列化于 RB-11t | independent | N/A | N/A |
| `RB-13` | implementation | wire internal core 12 facades to store | abandon branch or revert commit | yes | yes | 6/8 | 4/8 | 1/12 | M | RB-12 | 序列化于 RB-12 | family child | N/A | N/A |
| `RB-14` | implementation | wire oplog change global-mig 10 files | abandon branch or revert commit | yes | yes | 5/8 | 4/8 | 3/10 | M | RB-13 | 序列化于 RB-13 | family child | N/A | N/A |
| `RB-15` | implementation | wire sequencer overlays obliteration 4 files | abandon branch or revert commit | yes | yes | 6/8 | 4/8 | 4/4 | M | RB-14 | 序列化于 RB-14 | family child | N/A | N/A |
| `RB-16` | implementation | wire cli discovery and status cache 3 files | abandon branch or revert commit | yes | yes | 6/8 | 3/8 | 3/3 | M | RB-15 | 序列化于 RB-15 | family child | N/A | N/A |
| `RB-16b` | implementation | wire local transport obj-index path-case 4 files | abandon branch or revert commit | yes | yes | 5/8 | 3/8 | 2/4 | M | RB-16 | 序列化于 RB-16 | family child | RB-16 | N/A |
| `RB-17` | implementation | wire init and clone redb with spool tombstones | abandon branch or revert commit | yes | yes | 6/8 | 3/8 | 1/2 | S | RB-16b | 序列化于 RB-16b | family child | N/A | N/A |
| `RB-17b` | implementation | wire porcelain git commands core 10 files | abandon branch or revert commit | yes | yes | 5/8 | 4/8 | 1/10 | M | RB-17 | 序列化于 RB-17 | family child | RB-17 | N/A |
| `RB-18` | implementation | wire transport maint readonly commands B 10 files | abandon branch or revert commit | yes | yes | 4/8 | 3/8 | 1/10 | M | RB-17b | 序列化于 RB-17b | family child | N/A | N/A |
| `RB-18b` | implementation | wire fsck explicit repair and alternates 2 files | abandon branch or revert commit | yes | yes | 5/8 | 2/8 | 1/2 | S | RB-18 | 序列化于 RB-18 | family child | RB-18 | N/A |
| `RB-19` | implementation | wire merge bisect rebase commands C 12 files | abandon branch or revert commit | yes | yes | 6/8 | 3/8 | 2/12 | M | RB-18b | 序列化于 RB-18b | family child | N/A | N/A |
| `RB-20` | implementation | wire config doctor repair redb runtime 3 files | abandon branch or revert commit | yes | yes | 5/8 | 3/8 | 2/3 | S | RB-19 | 序列化于 RB-19 | family child | N/A | N/A |
| `RB-20c` | implementation | wire dual-feature sqlite runtime global+system config cli | abandon branch or revert commit | yes | yes | 4/8 | 2/8 | 1/1 | S | RB-20 | 序列化于 RB-20 | family child | RB-20 | N/A |
| `RB-20b` | implementation | wire worktree op memory automation hooks 9 files | abandon branch or revert commit | yes | yes | 5/8 | 3/8 | 3/9 | M | RB-20c | 序列化于 RB-20c | family child | RB-20 | N/A |
| `RB-21` | implementation | wire ai capture 12 files excluding store-detach | abandon branch or revert commit | yes | yes | 5/8 | 3/8 | 1/12 | M | RB-16b | no-overlap with RB-17..20b | family child | N/A | N/A |
| `RB-22` | implementation | wire ai history coverage export 9 files | abandon branch or revert commit | yes | yes | 5/8 | 3/8 | 1/9 | M | RB-21 | 序列化于 RB-21 | family child | N/A | N/A |
| `RB-23` | implementation | wire ai memory auto perm proj 10 files | abandon branch or revert commit | yes | yes | 6/8 | 3/8 | 4/10 | M | RB-22 | 序列化于 RB-22 | family child | N/A | N/A |
| `RB-24` | implementation | wire ai hooks mcp observed native 9 files | abandon branch or revert commit | yes | yes | 5/8 | 3/8 | 4/9 | M | RB-23 | 序列化于 RB-23 | family child | N/A | N/A |
| `RB-25` | implementation | wire agent-bridge 4 files and head publication | abandon branch or revert commit | yes | yes | 6/8 | 3/8 | 1/4 | M | RB-24 | 序列化于 RB-24 | family child | N/A | N/A |
| `RB-25b` | implementation | pin redb and backup dirs in sandbox policy 1 file | abandon branch or revert commit | yes | yes | 4/8 | 3/8 | 1/1 | S | RB-25 | 序列化于 RB-25 | family child | RB-25 | N/A |
| `RB-26` | implementation | wire agent cli and mega2-traces 11 files | abandon branch or revert commit | yes | yes | 4/8 | 3/8 | 2/11 | M | RB-25b | 序列化于 RB-25b | family child | N/A | N/A |
| `RB-26b` | implementation | wire agent doctor readonly vs explicit repair 1 file | abandon branch or revert commit | yes | yes | 5/8 | 2/8 | 1/1 | S | RB-26 | 序列化于 RB-26 | family child | RB-26 | N/A |
| `RB-27` | implementation | wire gui non-migrating repo reader and state | abandon branch or revert commit | yes | yes | 6/8 | 3/8 | 1/2 | S | RB-26b | 序列化于 RB-26b | family child | N/A | N/A |
| `RB-28` | implementation | wire cloud sync snapshot metadata 5 files | abandon branch or revert commit | yes | yes | 4/8 | 3/8 | 2/5 | M | RB-27 | 序列化于 RB-27 | family child | N/A | N/A |
| `RB-28b` | implementation | wire cloud restore redb manifest and gen parity 1 file | abandon branch or revert commit | yes | yes | 4/8 | 3/8 | 1/1 | S | RB-28 | 序列化于 RB-28 | family child | RB-28 | N/A |
| `RB-29` | implementation | wire backend-aware hook queue and redb-only drain e2e | forward-only drain or abandon | yes | yes | 5/8 | 3/8 | 2/4 | M | RB-20b,RB-28b | 序列化于 RB-20b,RB-28b | family child | N/A | N/A |
| `RB-29d` | implementation | wire agent daemon spawn-dedup and doctor queue metrics | abandon branch or revert commit | yes | yes | 4/8 | 4/8 | 2/3 | S | RB-29 | 序列化于 RB-29 | family child | RB-29 | N/A |
| `RB-29a` | implementation | unlock REDB_RUNTIME_UNLOCKED cli and libra-migrate gate | abandon branch or revert commit | yes | yes | 4/8 | 3/8 | 1/1 | S | RB-29d | 序列化于 RB-29d | family child | RB-29 | N/A |
| `RB-29c` | implementation | switch base.yml unlocked migrate test and job-diff | abandon branch or revert commit | yes | yes | 4/8 | 5/8 | 1/1 | S | RB-29a | 序列化于 RB-29a | family child | RB-29a | N/A |
| `RB-29b` | audit | syn 2.0 per-call-site reachability audit guard | revert audit test module | yes | yes | 9/20 | 6/20 | 0/0 | S | RB-29c | 序列化于 RB-29c | family child | RB-29 | N/A |
| `RB-30` | release | integrated REL-RB-01 redb release point | immutable-release sqlite default | yes | yes | 8/12 | 9/12 | 0/0 | S | RB-29b,DEP-RB-01,DEP-RB-02,DEP-RB-03 | 序列化于 RB-29b | family release point | N/A | N/A |
| `RB-31` | implementation | full dual-backend offline ci matrix explicit sqlite | revert workflow step | yes | yes | 4/8 | 3/8 | 1/1 | S | RB-30,DEP-RB-02 | 序列化于 RB-30 | independent | N/A | N/A |
| `RB-31b` | implementation | ci redb-only and dual-backend keyring matrix steps | revert workflow step | yes | yes | 4/8 | 2/8 | 1/1 | S | RB-31,DEP-RB-02 | 序列化于 RB-31 | independent | RB-31 | N/A |
| `RB-32` | implementation | release workflow dual-backend binary packaging | revert workflow step | yes | yes | 4/8 | 5/8 | 1/1 | S | RB-31b,DEP-RB-02 | 序列化于 RB-31b | independent | N/A | N/A |
| `RB-32b` | audit | pre-cutover staged dual-artifact interop audit | revert audit test module | yes | yes | 4/20 | 4/20 | 0/0 | S | RB-32 | 序列化于 RB-32 | no-release | RB-32 | N/A |
| `RB-33` | implementation | backend default redb init config and repo docs | patch backend default back and migrate | yes | yes | 5/8 | 5/8 | 1/1 | M | RB-32b,DEP-RB-02 | 序列化于 RB-32b | family child | RB-32 | N/A |
| `RB-33b` | audit | cutover staged dual-artifact sqlite rollback audit | revert audit test module | yes | yes | 4/20 | 4/20 | 0/0 | S | RB-33 | 序列化于 RB-33 | family child | RB-33 | N/A |
| `RB-33c` | release | integrated REL-RB-03 default redb release point | patch backend default back and migrate | yes | yes | 5/12 | 6/12 | 0/0 | S | RB-33b,DEP-RB-02,DEP-RB-03 | 序列化于 RB-33b | family release point | RB-33 | N/A |
| `RB-34` | implementation | cargo default feature redb builds and repo gate | patch cargo default back to sqlite | yes | yes | 5/8 | 5/8 | 1/1 | M | RB-33c,DEP-RB-02 | 序列化于 RB-33c | family child | RB-33 | N/A |
| `RB-34b` | audit | final default+keyring global/system 4-path audit | revert audit test module | yes | yes | 4/20 | 4/20 | 0/0 | S | RB-34 | 序列化于 RB-34 | family child | RB-34 | N/A |
| `RB-34c` | release | integrated REL-RB-02 cargo default release point | patch cargo default back to sqlite | yes | yes | 5/12 | 5/12 | 0/0 | S | RB-34b,DEP-RB-02 | 序列化于 RB-34b | family release point | RB-34 | N/A |
---

### Task RB-01: Cargo 存储 Feature 注册、可选依赖声明与 `src/lib.rs` 1 行入口挂接（Track A，拆出 `RB-01d`、`RB-01b`、`RB-02`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`（2026-11-22 03:39:49 UTC，版本 `0.30.278`，按用户 2026-11-22 指令于 `main` 直接提交推送；AC1–AC8、四特性编译矩阵、fmt、严格 clippy `--all-targets --all-features`、rustdoc、`compat_version_surface_sync`/`compat_matrix_alignment`/`compat_serial_registry` 与 Codex 只读审查 `01a20155-954e-7f10-a16a-d2edb84b0569` `VERDICT: PASS` 全部通过；远端 CI 门待推后观测）

**Description:** 在 `Cargo.toml` 注册可叠加存储特性 `default = ["storage-sqlite"]`、`storage-redb = ["storage-queue", "dep:redb", "dep:postcard", "dep:crc32c"]`（自动蕴含 `storage-queue`）、`storage-queue = ["dep:postcard", "dep:crc32c"]` 及三项可选依赖 `redb = { version = "5.0.0", optional = true }`、`postcard = { version = "1.1", features = ["use-std"], optional = true }`、`crc32c = { version = "0.6.9", optional = true }`（保持 `sea-orm` 原样常驻），在 `src/lib.rs` 仅新增 1 行 `pub mod storage;`，并在 `src/storage/mod.rs` 落位最小占位模块。

**Out of scope:**
- 双存储关闭 `compile_error!` 守卫、四组存储特性组合零告警编译与 `storage-redb` 隐式启用 `storage-queue` 编译期断言：已按 `G-03`/`G-09` 拆出由 `RB-01d` 承接。
- 版本化 `postcard` 编解码与统一错误类型：由 `RB-01b` 承接；`StorageBackendKind` 单后端特性分派与编译期常量 `REDB_RUNTIME_UNLOCKED = false` 门控由 `RB-01c` 承接。
- `redb` 多进程打开器、`META_SCHEMA` 围栏与两阶段原子清单：由 `RB-02`..`RB-02e` 承接。
- Track A 不改既有 `src/internal/**`、`src/command/**`、`src/gui/**`、`src/utils/**` 或 `src/cli.rs`（`GC-RB-06`）。

**Current evidence:**

| 事实（已核实基线 `58ad55a` / `0.30.277`） | 证据 |
|---|---|
| `Cargo.toml` 原 `default = []`，无 `storage-*` feature，无 `crc32c` 依赖 | `Cargo.toml:15-27,103` |
| `src/storage/` 不存在，`src/lib.rs` 未导出 `storage` 模块 | `src/lib.rs:20-27` |

**Acceptance criteria:**
- [x] `Cargo.toml` 定义默认特性 `default = ["storage-sqlite"]`。
- [x] `Cargo.toml` 定义可叠加特性 `storage-redb = ["storage-queue", "dep:redb", "dep:postcard", "dep:crc32c"]`。
- [x] `Cargo.toml` 定义可叠加特性 `storage-queue = ["dep:postcard", "dep:crc32c"]`。
- [x] `Cargo.toml` 声明可选依赖 `redb = { version = "5.0.0", optional = true }`。
- [x] `Cargo.toml` 声明可选依赖 `postcard = { version = "1.1", features = ["use-std"], optional = true }`。
- [x] `Cargo.toml` 声明可选依赖 `crc32c = { version = "0.6.9", optional = true }`。
- [x] `Cargo.toml` 中既有常驻依赖 `sea-orm` 声明保持原样零改动。
- [x] `src/lib.rs` 相对显式前置基线（`PRE_CARD_BASE`，同时核验工作区未暂存态与 `libra add` 已暂存态）的完整变更恰好为新增 1 行 `pub mod storage;`（0 行删除）。

**Verification:**
- [x] `rg -q '^default = \["storage-sqlite"\]$' Cargo.toml`（覆盖 AC1）
- [x] `rg -q '^storage-redb = \["storage-queue", "dep:redb", "dep:postcard", "dep:crc32c"\]$' Cargo.toml`（覆盖 AC2）
- [x] `rg -q '^storage-queue = \["dep:postcard", "dep:crc32c"\]$' Cargo.toml`（覆盖 AC3）
- [x] `rg -q '^redb = \{ version = "5\.0\.0", optional = true \}$' Cargo.toml`（覆盖 AC4）
- [x] `rg -q '^postcard = \{ version = "1\.1", features = \["use-std"\], optional = true \}$' Cargo.toml`（覆盖 AC5）
- [x] `rg -q '^crc32c = \{ version = "0\.6\.9", optional = true \}$' Cargo.toml`（覆盖 AC6）
- [x] `python3 -c 'import os, subprocess; from pathlib import Path; run=lambda c: subprocess.check_output(c, text=True); extract=lambda t: (lambda lines: "".join(lines))(next(([l for l in t.splitlines(True)[i:]][:next(j for j,x in enumerate(t.splitlines(True)[i:],1) if "}" in x)] for i,line in enumerate(t.splitlines(True)) if line.startswith("sea-orm =")), [])); st=run(["libra","status","--porcelain=v1","--","Cargo.toml"]).strip(); base_ref=os.environ.get("PRE_CARD_BASE") or ("HEAD" if st else "HEAD~1"); old=extract(run(["libra","--no-pager","show",f"{base_ref}:Cargo.toml"])); wt=extract(Path("Cargo.toml").read_text(encoding="utf-8")); idx=extract(run(["libra","--no-pager","show",":Cargo.toml"])); assert old and old==wt and old==idx, "sea-orm block modified"'`（开工前捕获 `export PRE_CARD_BASE=$(libra rev-parse HEAD)`；零临时文件、零 `mktemp`、零 `/dev/fd`，纯内存双验工作区与暂存索引 `:Cargo.toml` 的多行 `sea-orm = { ... }` 块相对基线逐字节一致，覆盖 AC7）
- [x] `python3 -c 'import os, subprocess; from pathlib import Path; run=lambda c: subprocess.check_output(c, text=True); ok=lambda old,cur: len(cur)==len(old)+1 and any(cur[:i]==old[:i] and cur[i]=="pub mod storage;\n" and cur[i+1:]==old[i:] for i in range(len(old)+1)); st=run(["libra","status","--porcelain=v1","--","src/lib.rs"]).strip(); base_ref=os.environ.get("PRE_CARD_BASE") or ("HEAD" if st else "HEAD~1"); old=run(["libra","--no-pager","show",f"{base_ref}:src/lib.rs"]).splitlines(True); assert not ok(old, old+["pub mod storage;\n","+extra\n"]) and not ok(old+["-del\n"], old+["pub mod storage;\n"]) and not ok(old, old[:-1]+["pub mod storage;\n"]), "self-test failed"; wt=Path("src/lib.rs").read_text(encoding="utf-8").splitlines(True); idx=run(["libra","--no-pager","show",":src/lib.rs"]).splitlines(True); assert ok(old, wt), "worktree src/lib.rs mismatch"; assert idx==old or ok(old, idx), "staged index src/lib.rs mismatch"'`（开工前捕获 `export PRE_CARD_BASE=$(libra rev-parse HEAD)`，并于 `libra add src/lib.rs` 前的未暂存态与 `libra add src/lib.rs` 后的已暂存态**双态均执行**；零临时文件纯内存完整行列表比对工作区与索引 `:src/lib.rs` 相对基线严格仅插入唯一一行 `pub mod storage;\n` 且内置 `+`/`-` 行与删改行负例自测，彻底消除 unified diff 正则漏检，覆盖 AC8）

**Full-suite trigger:** `T-1: 修改 Cargo.toml 非版本依赖与 features 定义`

**Dependencies:** `DEP-RB-02`（`Cargo.toml` 串行写窗口）

**Deliverables:** `N/A`

**Implementation write set:** `Cargo.toml`、`Cargo.lock`、`src/lib.rs`、`src/storage/mod.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件（行为生产落点 3 个：`Cargo.toml`、`src/lib.rs`、`src/storage/`；3 个生产文件不含 `Cargo.lock`）。

**Docs and compatibility impact:** `N/A`（CLI 运行时保持既有 SQLite 不变）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=cargo storage features optional deps and lib.rs hook; recovery=revert or patch; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=3; prod-files=3; scope=M; deps=DEP-RB-02; writeset=no-overlap; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-01d: 双关 `compile_error!` 守卫、四组 Feature 编译矩阵与 `storage-redb` 隐式队列断言（Track A，自 `RB-01` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`（2026-11-22 03:46:22 UTC，版本 `0.30.279`，于 `main` 直接提交推送；Stage 1 精确 1 文件写集、AC1–AC6、四组特征矩阵、fmt、严格 clippy、rustdoc 与 Codex `01a2015b-e3a7-75f0-8d1a-27bd39e38f6d` `VERDICT: PASS` 全绿）

**Description:** 100% 在独占文件 `src/storage/mod.rs` 内补齐双存储关闭时的 `compile_error!` 守卫、`#[cfg(feature = "storage-redb")]` 编译期断言（确保 `cfg!(feature = "storage-queue")` 恒为真）与条件导出守卫，并逐一验证 4 组独立存储特性组合零告警通过编译。

**Out of scope:**
- 版本化 `postcard` 编解码与错误类型：由 `RB-01b` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-01` #1 要求关闭两存储后端触发 `compile_error!`、一切启用 `storage-redb` 的构建自动蕴含 `storage-queue`，且 4 组存储特征组合均须零告警独立编译通过 | `ADR-RB-01` #1；`GC-RB-05`；`RB-01` |

**Acceptance criteria:**
- [x] 关闭两个存储后端（`--no-default-features`）时 `src/storage/mod.rs` 触发非零退出的 `compile_error!`。
- [x] `--no-default-features --features storage-sqlite` 编译通过零告警。
- [x] `--no-default-features --features storage-redb`（不显式传 `storage-queue`）编译通过零告警。
- [x] `src/storage/mod.rs` 中 `#[cfg(feature = "storage-redb")]` 静态断言确保 `cfg!(feature = "storage-queue")` 恒为真。
- [x] `--no-default-features --features storage-redb,storage-queue` 编译通过零告警。
- [x] `--features storage-sqlite,storage-redb,storage-queue` 双特性叠加编译通过零告警。

**Verification:**
- [x] `bash -euo pipefail -c 'set +e; out="$(cargo check --no-default-features 2>&1)"; status=$?; set -e; test "$status" -ne 0 && printf "%s\n" "$out" | rg -q "storage-sqlite|storage-redb"'`（覆盖 AC1）
- [x] `cargo check --no-default-features --features storage-sqlite`（覆盖 AC2）
- [x] `cargo check --no-default-features --features storage-redb`（覆盖 AC3、AC4）
- [x] `cargo check --no-default-features --features storage-redb,storage-queue`（覆盖 AC5）
- [x] `cargo check --features storage-sqlite,storage-redb,storage-queue`（覆盖 AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 1 文件实现白名单（`src/storage/mod.rs`）。

**Full-suite trigger:** `none`

**Dependencies:** `RB-01`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 1 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=4-way feature matrix compile-error and redb-queue assert; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=1; prod-files=1; scope=S; deps=RB-01; writeset=序列化于 RB-01; release=independent; split-from=RB-01; exception=N/A`

---

### Task RB-01b: 版本化 `postcard` 编解码与统一存储错误类型（Track A，自 `RB-01` 拆出，拆出 `RB-01c`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`（2026-11-22 06:45:49 UTC，版本 `0.30.285`，于 `main` 直接提交推送；Stage 1 精确 3 文件写集、AC1–AC5、`nextest list`/`run`、跨版本显式分发与 `Io` 脱敏 `anyhow` 链回归、4 特性矩阵、fmt、严格 clippy、rustdoc 与 Codex R3 `01a20200-379a-7b71-aaab-73ab6e680f74` `VERDICT: PASS` 全绿）

**Description:** 100% 在独占目录 `src/storage/` 内交付统一存储错误类型 `StorageBackendError`（`error.rs`）与 `[schema_ver: u16_le][postcard_bytes]` 版本化编解码器（`codec.rs`），严格区分无损往返、损坏负载、缺失版本头截断、半截负载截断与未知高版本拒绝。

**Out of scope:**
- `StorageBackendKind` 单后端环境变量拒收与编译期常量 `REDB_RUNTIME_UNLOCKED = false` 门控：已按 `G-03`/`G-09` 拆出由 `RB-01c` 承接。
- 两阶段清单 `storage-format.json` 解析集成：由 `RB-02c`/`RB-02e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-04` #2 要求所有 `redb` 值采用 `[schema_ver: u16_le][postcard_bytes]` 显式版本头封装并对损坏、截断与未知高版本返回类型化错误 | `ADR-RB-04` |

**Acceptance criteria:**
- [x] `storage::codec` 按 `[schema_ver: u16_le][postcard_bytes]` 编码并无损往返解码。
- [x] `storage::codec` 对损坏负载字节返回类型化 `StorageBackendError`。
- [x] `storage::codec` 对不足 2 字节（缺失版本头）截断输入返回类型化 `StorageBackendError`。
- [x] `storage::codec` 对版本头完整但负载半截的截断输入返回类型化 `StorageBackendError`。
- [x] `storage::codec` 对未知高 `schema_ver` 返回类型化 `StorageBackendError`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::codec::tests::versioned_postcard_roundtrip_and_corrupt_reject)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::codec::tests::versioned_postcard_roundtrip_and_corrupt_reject)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单（`src/storage/mod.rs`、`src/storage/error.rs`、`src/storage/codec.rs`）。

**Full-suite trigger:** `none`

**Dependencies:** `RB-01d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/error.rs`、`src/storage/codec.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=versioned postcard codec and backend error; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=1; prod-files=3; scope=S; deps=RB-01d; writeset=序列化于 RB-01d; release=independent; split-from=RB-01; exception=N/A`

---

### Task RB-01c: `StorageBackendKind` 单后端特性分派与编译期预集成门控（Track A，自 `RB-01b` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`（2026-11-22 07:17:48 UTC，版本 `0.30.286`，于 `main` 直接提交推送；Stage 1 精确 2 文件写集、AC1–AC4、三组特性 `nextest list`/`run` 含子进程实测公开 `resolve()` 与非法值占位脱敏、fmt、严格 clippy 与 Codex R2 `01a2021d-77b7-7c90-9146-40618689ff47` `VERDICT: PASS` 全绿）

**Description:** 100% 在独占目录 `src/storage/` 内交付 `src/storage/backend.rs`：实现 `StorageBackendKind` 特性编译门控（单后端拒绝未编译覆盖）与编译期常量 `pub const REDB_RUNTIME_UNLOCKED: bool = false;` 不可旁路预集成门控（忽略 `LIBRA_TEST`）。

**Out of scope:**
- 将 `crate::storage::backend` 接入生产 `libra-migrate`（归 `RB-11c`）与生产 `libra` CLI 入口（归 `RB-16`）：Track A 不触碰 `src/cli.rs`（`GC-RB-06`）。
- 两阶段清单 `storage-format.json` 优先级集成：由 `RB-02e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-01` #3 要求单后端不兼容 env 覆盖 fail-closed 报 `LBR-CLI-003`，且 `REDB_RUNTIME_UNLOCKED = false` 期间不可通过环境变量旁路 | `ADR-RB-01` |

**Acceptance criteria:**
- [x] 纯 `storage-sqlite` 构建收到 `LIBRA_STORAGE_BACKEND=redb` 覆盖时返回 fail-closed 错误 `LBR-CLI-003`。
- [x] 纯 `storage-redb` 构建收到 `LIBRA_STORAGE_BACKEND=sqlite` 覆盖时返回 fail-closed 错误 `LBR-CLI-003`。
- [x] 编译期常量 `REDB_RUNTIME_UNLOCKED = false` 下，`StorageBackendKind::resolve` 对选用 `Redb` 的请求（忽略 `LIBRA_TEST`）返回 `LBR-CLI-003`。
- [x] 编译期常量 `REDB_RUNTIME_UNLOCKED = false` 下，`assert_redb_runtime_unlocked()` 返回 `LBR-CLI-003`（生产 `libra-migrate` 接入在 `RB-11c` 实测、生产 `libra` CLI 接入在 `RB-16` 实测）。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::tests::sqlite_only_rejects_redb_override) | test(=storage::tests::redb_only_rejects_sqlite_override) | test(=storage::backend::tests::dual_feature_resolution_and_pre_release_cli_gate)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(=storage::tests::sqlite_only_rejects_redb_override)'`（new，覆盖 AC1）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::tests::redb_only_rejects_sqlite_override)'`（new，覆盖 AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::backend::tests::dual_feature_resolution_and_pre_release_cli_gate)'`（new，覆盖 AC3、AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单（`src/storage/mod.rs`、`src/storage/backend.rs`）。

**Full-suite trigger:** `none`

**Dependencies:** `RB-01b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/backend.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=StorageBackendKind feature dispatch and gate; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=5/8; landing=1; prod-files=2; scope=S; deps=RB-01b; writeset=序列化于 RB-01b; release=independent; split-from=RB-01b; exception=N/A`

---

### Task RB-02: `redb 5.0` `MultiWriter` 打开器、分层缓存与有界写锁（Track A，自 `RB-01` 拆出，拆出 `RB-02d`、`RB-02b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 100% 在 `src/storage/` 内交付 `redb` 多进程打开器（`opener.rs`）：统一配置 `ConcurrencyMode::MultiWriter` 与分层缓存上限（只读 `16 MiB`、常规写 `32 MiB`、长驻 `64 MiB`），并经 `spawn_blocking` 实现有界 `try_begin_write` 退避与 `commit()`。

**Out of scope:**
- 严格只读 `RepairAborted` 合同与显式可写修复路径：已按 `G-03`/`G-09` 拆出由 `RB-02d` 承接。
- `META_SCHEMA` 账本/迁移围栏与两阶段原子清单 `storage-format.json`：已拆出由 `RB-02b`、`RB-02c` 与 `RB-02e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `redb 5.0.0` 默认 `ExclusiveWriter` + `1 GiB` 缓存，多进程并发打开须显式配置 `MultiWriter` 与按角色缓存上限 | `redb-5.0.0/src/db.rs:2253-2333,2454-2460` |

**Acceptance criteria:**
- [x] 打开器统一配置 `ConcurrencyMode::MultiWriter`。
- [x] 只读打开配置强制缓存上限为 `16 MiB`。
- [x] 常规写打开配置强制缓存上限为 `32 MiB`。
- [x] 长驻进程（`agent bridge`/`daemon`/消费者）打开配置强制缓存上限为 `64 MiB`。
- [x] 同一 `.redb` 文件支持多进程并发打开 `Database` 与 `ReadOnlyDatabase`，不触发 `DatabaseAlreadyOpen`。
- [x] `begin_write_bounded` 在另一进程持写锁超过 deadline 时按期返回类型化忙错误。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::opener::tests::multiwriter_concurrent_open_and_bounded_write_timeout)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::opener::tests::multiwriter_concurrent_open_and_bounded_write_timeout)'`（new，覆盖 AC1–AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单（`src/storage/mod.rs`、`src/storage/opener.rs`）。

**Full-suite trigger:** `none`

**Dependencies:** `RB-01c`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/opener.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=redb multiwriter opener cache and bounded write; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=RB-01c; writeset=序列化于 RB-01c; release=independent; split-from=RB-01; exception=N/A`

---

### Task RB-02d: 严格只读 `RepairAborted` 合同与显式可写修复路径（Track A，自 `RB-02` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 100% 在 `src/storage/` 内于 `opener.rs` 落实 `ADR-RB-03` #2 的严格只读 `RepairAborted` 合同与显式可写修复边界：只读上下文遇 `DatabaseError::RepairAborted` 绝不升写修改磁盘，显式可写/修复上下文在只读挂载 fail-closed、在可写崩溃夹具上并发安全完成恢复。

**Out of scope:**
- `META_SCHEMA` 围栏与清单解析：由 `RB-02b`/`RB-02c` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `redb 5.0.0` `open_read_only` 遇未完成恢复崩溃库返回 `DatabaseError::RepairAborted` | `redb-5.0.0/src/error.rs:466-474`；`ADR-RB-03` #2 |

**Acceptance criteria:**
- [x] 只读上下文遇 `DatabaseError::RepairAborted` 时返回类型化 `LBR-IO-001`（附显式修复指引）。
- [x] 只读上下文遇 `DatabaseError::RepairAborted` 时绝不以可写模式打开或修改 `.redb` 磁盘字节。
- [x] 显式允许的可写/修复上下文在只读挂载文件系统上 fail-closed 报 `LBR-IO-001`。
- [x] 显式允许的可写/修复上下文在可写崩溃夹具上并发安全完成恢复。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::opener::tests::readonly_repair_aborted_ro_fs_and_concurrent_explicit_repair)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::opener::tests::readonly_repair_aborted_ro_fs_and_concurrent_explicit_repair)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单（`src/storage/mod.rs`、`src/storage/opener.rs`）。

**Full-suite trigger:** `none`

**Dependencies:** `RB-02`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/opener.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=strict readonly RepairAborted and explicit repair; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-02; writeset=序列化于 RB-02; release=independent; split-from=RB-02; exception=N/A`

---

### Task RB-02b: `META_SCHEMA` 角色版本校验与句柄迁移围栏（Track A，自 `RB-02` 拆出，拆出 `RB-02c`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 100% 在 `src/storage/` 内交付 `META_SCHEMA` 账本与句柄迁移围栏校验模块（`meta.rs`）：核验角色、最低读取版本与 `min_reader_version == u32::MAX` 迁移围栏哨兵，在每次读/写事务入口物理阻断已打开废弃红库句柄继续读写。

**Out of scope:**
- 两阶段原子清单 `.libra/storage-format.json` 与清单前置优先级解析：已按 `G-03`/`G-09` 拆出由 `RB-02c` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-03` #3 与 `ADR-RB-04` #4 要求每库内置 `META_SCHEMA` 并在每次事务入口检查 `min_reader_version == u32::MAX` 围栏 | `ADR-RB-03`、`ADR-RB-04` |

**Acceptance criteria:**
- [x] `META_SCHEMA` 校验拒绝角色不匹配（如用 `Repository` 打开 `Capture`）的 `.redb` 文件。
- [x] `META_SCHEMA` 校验拒绝 `min_reader_version` 高于当前二进制版本的 `.redb` 文件。
- [x] 读事务入口遇到 `META_SCHEMA` 迁移围栏哨兵（`min_reader_version == u32::MAX`）时立即中止并返回 `StorageBackendError::RepositoryMigrated`。
- [x] 写事务入口遇到 `META_SCHEMA` 迁移围栏哨兵（`min_reader_version == u32::MAX`）时立即中止并返回 `StorageBackendError::RepositoryMigrated`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::meta::tests::meta_schema_role_future_version_and_migration_tombstone_fence)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::meta::tests::meta_schema_role_future_version_and_migration_tombstone_fence)'`（new，覆盖 AC1、AC2、AC3、AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单（`src/storage/mod.rs`、`src/storage/meta.rs`）。

**Full-suite trigger:** `none`

**Dependencies:** `RB-02d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/meta.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=meta schema role version and handle fence; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=RB-02d; writeset=序列化于 RB-02d; release=independent; split-from=RB-02; exception=N/A`

---

### Task RB-02c: 两阶段原子清单 `storage-format.json` 发布与代际路径解析（Track A，自 `RB-02b` 拆出，拆出 `RB-02e`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 100% 在 `src/storage/` 内交付 `.libra/storage-format.json` 两阶段原子清单模块（`manifest.rs`，含 `finalized`、`phase2_progress`、`targets_staged_for_rename`、`target_migration_tag`、`spool_unfenced`、`source_digests` 持久化发布、代际路径解析及缺清单忽略未提交残片）。

**Out of scope:**
- `StorageBackendKind::resolve` 清单前置优先级阻断冲突 `LIBRA_STORAGE_BACKEND` 覆盖：已按 `G-03`/`G-09` 拆出由 `RB-02e` 承接。
- 具体业务表读写：由 `RB-03`..`RB-06` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-02` #1 与 `ADR-RB-07` 要求两阶段清单原子发布、`.libra/redb/` 托管代际路径与忽略未提交残片 | `ADR-RB-02`、`ADR-RB-07` |

**Acceptance criteria:**
- [x] `storage-format.json` 按 `.tmp` → `fsync` → `rename` → `fsync_dir` 原子发布 `finalized = false` 清单（含 `phase2_progress`、`targets_staged_for_rename`、`target_migration_tag`、`spool_unfenced` 与 `source_digests` 字段）。
- [x] `storage-format.json` 按 `.tmp` → `fsync` → `rename` → `fsync_dir` 原子发布 `finalized = true` 终态清单。
- [x] 清单解析器按代际返回 `.libra/redb/git.gen-<id>.redb` 路径。
- [x] 清单解析器按代际返回 `.libra/private/capture.gen-<id>.redb` 路径。
- [x] 清单缺失时解析器绝不选中 `.libra/redb/` 下未提交的 `.redb` 或 `.tmp` 残片。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::manifest::tests::two_phase_finalized_manifest_and_absent_fragment_ignore)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::manifest::tests::two_phase_finalized_manifest_and_absent_fragment_ignore)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单（`src/storage/mod.rs`、`src/storage/manifest.rs`）。

**Full-suite trigger:** `none`

**Dependencies:** `RB-02b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/manifest.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=2-phase manifest atomic publish and gen paths; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=RB-02b; writeset=序列化于 RB-02b; release=independent; split-from=RB-02b; exception=N/A`

---

### Task RB-02e: 清单前置后端解析优先级与冲突 `LIBRA_STORAGE_BACKEND` 覆盖阻断（Track A，自 `RB-02c` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 100% 在 `src/storage/` 内将 `manifest.rs` 接入 `src/storage/backend.rs` 的 `StorageBackendKind::resolve` 清单前置优先级链路，在双特性构建下强制清单与旧库状态严格先于 `LIBRA_STORAGE_BACKEND` 生效。

**Out of scope:**
- 具体业务表读写：由 `RB-03`..`RB-06` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-01` #3 要求清单先于 `LIBRA_STORAGE_BACKEND`：`finalized == false` 无条件 `LBR-IO-001`、冲突覆盖报 `LBR-CLI-003`、仅全新空仓允许覆盖 | `ADR-RB-01` #3 |

**Acceptance criteria:**
- [x] 双特性构建下遇 `finalized == false` 清单时，设置 `LIBRA_STORAGE_BACKEND=redb` 覆盖无条件 fail-closed 报 `LBR-IO-001`。
- [x] 双特性构建下遇 `finalized == false` 清单时，设置 `LIBRA_STORAGE_BACKEND=sqlite` 覆盖无条件 fail-closed 报 `LBR-IO-001`。
- [x] 双特性构建下遇 `finalized == true` 清单时，与 `manifest.backend` 冲突的 `LIBRA_STORAGE_BACKEND` 覆盖 fail-closed 报 `LBR-CLI-003`。
- [x] 双特性构建下遇无清单但存在 `.libra/libra.db` 的旧仓时，`LIBRA_STORAGE_BACKEND=redb` 覆盖 fail-closed 报 `LBR-CLI-003`。
- [x] 仅全新空仓库（无清单且无 `libra.db`）允许 `LIBRA_STORAGE_BACKEND` 覆盖编译期默认后端（仍受门控约束）。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::backend::tests::manifest_first_precedence_blocks_conflicting_env_overrides)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::backend::tests::manifest_first_precedence_blocks_conflicting_env_overrides)'`（new，双特性构建下证明清单前置优先级非缺特性误报，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单（`src/storage/mod.rs`、`src/storage/backend.rs`、`src/storage/manifest.rs`）。

**Full-suite trigger:** `none`

**Dependencies:** `RB-02c`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/backend.rs`、`src/storage/manifest.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=manifest-first precedence over env backend; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=1; prod-files=3; scope=S; deps=RB-02c; writeset=序列化于 RB-02c; release=independent; split-from=RB-02c; exception=N/A`

---

### Task RB-03: `ConfigStore` 与 `MetadataStore` 双后端核心等价与 MVCC（Track A，拆出 `RB-03j`、`RB-03b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`58ff194`）

**Description:** 100% 在独占目录 `src/storage/**` 内定义 `ConfigStore` 与 `MetadataStore` trait（覆盖 4 张仓库配置/元数据表与 3 张配置库源表），完成 `SqliteConfigStore` 与 `RedbConfigStore` 核心配置 CRUD（多值追加、节重命名、删除）、`metadata_kv` CAS 读写等价与 `RedbConfigStore` 多进程无锁只读快照。

**Out of scope:**
- `GC-13` 仓库配置写隔离与 `GlobalConfig`/`SystemConfig` 文件权限位（`0600`/`0644`）：已按 `G-03`/`G-09` 拆出由 `RB-03j` 承接。
- `GlobalConfig`/`SystemConfig` 作用域 `config-format.json`、`config-migration.lock` 并发首写串行化与双/单特性运行时分派：由 `RB-03b`、`RB-03h`、`RB-03d`、`RB-03f`、`RB-03e`、`RB-03i`、`RB-03c` 与 `RB-03g` 承接。
- 修改 `src/internal/config.rs`、`metadata.rs`、`db.rs` 或 `shared_perm.rs`：Track A 禁改（接线归 `RB-13`、`RB-20`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 配置级联覆盖 `Repository`、`GlobalConfig`、`SystemConfig` 及 `config`/`config_kv`/`configuration_schema_versions`/`metadata_kv` | `src/internal/config.rs:47-120`；`src/internal/db/schema.rs:69-115`；`src/internal/metadata.rs:2-95` |

**Acceptance criteria:**
- [x] 双特性构建下 `SqliteConfigStore` 与 `RedbConfigStore` 的多值追加（`add`）读写结果逐项一致。
- [x] 双特性构建下 `SqliteConfigStore` 与 `RedbConfigStore` 的节重命名（`rename_section`）结果逐项一致。
- [x] 双特性构建下 `SqliteConfigStore` 与 `RedbConfigStore` 的键/节删除（`unset`/`remove_section`）结果逐项一致。
- [x] `MetadataStore` 的 `metadata_kv` CAS 读写在 `Sqlite` 与 `Redb` 双后端逐项一致。
- [x] `RedbConfigStore` 支持 8 并发只读进程在 1 写进程更新期间无锁读取一致快照。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::dual_backend_config_and_metadata_equivalence) | test(=storage::config_store::tests::redb_config_mvcc_concurrent_readers)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::dual_backend_config_and_metadata_equivalence)'`（new，覆盖 AC1–AC4）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::redb_config_mvcc_concurrent_readers)'`（new，覆盖 AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-02e`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=dual-backend ConfigStore crud cas and mvcc; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=3; prod-files=6; scope=M; deps=RB-02e; writeset=序列化于 RB-02e; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-03j: `RedbConfigStore` `GC-13` 仓库隔离与 Global/System 权限位守卫（Track A，自 `RB-03` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`890d410` / `v0.30.306`；Codex R1 `PASS`；全部 AC/VER 与 6 文件白名单守卫绿灯）

**Description:** 100% 在独占目录 `src/storage/**` 内落实 `RedbConfigStore` 的 `GC-13` 角色隔离（仓库配置写入严禁触碰隔离 `GlobalConfig`/`SystemConfig` 夹具）以及 Unix 权限位约定（空 `GlobalConfig` 创建 `0600` `config.redb`，空 `SystemConfig` 创建 `0644` `config.redb`）。

**Out of scope:**
- 作用域 `config-format.json` 清单校验、有界锁与首写墓碑预占：由 `RB-03b`、`RB-03h`、`RB-03d`、`RB-03f` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `GC-13` 要求 `Repository` 配置写入严禁修改 `GlobalConfig`/`SystemConfig`；全局配置须 `0600` 私密、系统配置须 `0644` 可读 | `GC-RB-02`；`src/internal/config.rs:47-120` |

**Acceptance criteria:**
- [x] `GC-13` 全局隔离：`Repository` 配置写入严禁创建或修改隔离夹具 `LIBRA_CONFIG_GLOBAL_DB`。
- [x] `GC-13` 系统隔离：`Repository` 配置写入严禁创建或修改隔离夹具 `LIBRA_CONFIG_SYSTEM_DB`。
- [x] `RedbConfigStore` 在空 `GlobalConfig` 目录写入时按精确 `0600` 权限创建 `config.redb`。
- [x] `RedbConfigStore` 在空 `SystemConfig` 目录写入时按精确 `0644` 权限创建 `config.redb`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::redb_config_gc13_repo_isolation_and_scope_file_modes)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::redb_config_gc13_repo_isolation_and_scope_file_modes)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=redb config gc13 isolation and file modes; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=3; prod-files=6; scope=S; deps=RB-03; writeset=序列化于 RB-03; release=independent; split-from=RB-03; exception=N/A`

---

### Task RB-03b: `GlobalConfig`/`SystemConfig` 作用域 `config-format.json` 清单校验与空目录只读零建文件（Track A，自 `RB-03` 拆出，拆出 `RB-03h`、`RB-03d`、`RB-03e`、`RB-03c`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`600b335` / `v0.30.307`；Codex R2 `PASS`；覆盖 pending 清单、双文件分裂、哨兵与真 SQLite `__libra_migrated_fence__` 墓碑校验及空目录只读零建文件）

**Description:** 100% 在独占目录 `src/storage/**` 内落实 `ADR-RB-02` #2 的跨运行时作用域清单前置校验：覆盖 `finalized == false` 中间态拒访、无清单双文件共存分裂拒访、带 `legacy_sqlite_tombstone = true` 的已定稿 `Redb` 清单对共存 `config.db` 墓碑的合法识别，以及空配置目录只读零建文件（`ADR-CF-06`）。

**Out of scope:**
- `ConfigScopeLock` 有界共享/排他协调锁与非 root 只读系统目录缺锁安全降级：已按 `G-03`/`G-09` 拆出由 `RB-03h` 承接。
- 空目录并发首写串行化、先非覆写 `hard_link` 墓碑预占与旧版二进制竞态实测：已按 `G-03`/`G-09` 拆出由 `RB-03d`/`RB-03f` 承接。
- 首次建库 4 步持久化发布状态机（`init_phase = "fresh_create"`）与全边界崩溃恢复：已按 `G-03`/`G-09` 拆出由 `RB-03e` 承接。
- 双特性/单特性旧库读回退与拒写分派矩阵：由 `RB-03c`/`RB-03g` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 配置运行时必须通过清单前置校验拦截中间态与双文件分裂、识别保留型墓碑并维持空目录只读零建文件 | `src/internal/config.rs:47-120`；`ADR-RB-02` #2 |

**Acceptance criteria:**
- [x] 当 `<scope_dir>/config-format.json` 存在且 `finalized == false`（非 `RB-03e` 专属恢复路径）时，运行时打开（忽略 `LIBRA_STORAGE_BACKEND`）立即 fail-closed 报 `LBR-IO-001`。
- [x] 当 `config-format.json` 缺失但 `config.db` 与 `config.redb` 共存时，运行路径 fail-closed 报 `LBR-IO-001`。
- [x] 当 `config-format.json` 为 `backend="redb", finalized=true, legacy_sqlite_tombstone=true` 时，运行时将同目录共存的 `config.db` 墓碑视为合法路径围栏而不误报分裂。
- [x] 空配置目录只读打开直接返回 `NotFound` 且不在磁盘创建任何目录或 `config-migration.lock` 文件（保留 `ADR-CF-06`）。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::config_scope_manifest_pending_dual_file_and_tombstone_acceptance)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::config_scope_manifest_pending_dual_file_and_tombstone_acceptance)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03j`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=config scope manifest validation and absent ro; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=3; prod-files=6; scope=M; deps=RB-03j; writeset=序列化于 RB-03j; release=independent; split-from=RB-03; exception=N/A`

---

### Task RB-03h: `ConfigScopeLock` 有界协调锁与非 root 只读系统目录降级（Track A，自 `RB-03b` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`e5df194` / `v0.30.308`；Codex R1–R3 全项真实 P1 已闭环并绿灯，R4 提出与本卡 AC4 / `ADR-RB-02` #2 明文降级契约冲突的要求，按用户授权跳过后续 Review 直接推进 Stage 2）

**Description:** 100% 在独占目录 `src/storage/**` 内落实 `ConfigScopeLock::try_lock_shared_bounded` / `try_lock_exclusive_bounded` 有界协调锁契约（防止跨进程锁竞争无限挂起）及非 root 无写权限只读 `SystemConfig` 目录缺锁文件时的安全只读降级。

**Out of scope:**
- 空目录并发首写串行化与先非覆写 `hard_link` 墓碑预占：由 `RB-03d`/`RB-03f` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 配置运行时必须通过有界锁获取契约避免无限挂起，并在非 root 只读系统目录无权建锁文件时安全降级为只读直查清单 | `ADR-RB-02` #2 |

**Acceptance criteria:**
- [x] 稳态读写路径通过 `ConfigScopeLock::try_lock_shared_bounded(deadline)` 获取共享锁并允许同后端多进程并发访问。
- [x] 外部持排他 `LOCK_EX` 超过 `deadline`（单测支持 `Duration::ZERO`）时，`try_lock_shared_bounded` 返回类型化 `LBR-IO-001`。
- [x] 外部持锁超过 `deadline`（单测支持 `Duration::ZERO`）时，`try_lock_exclusive_bounded` 返回类型化 `LBR-IO-001`。
- [x] 无写权限的非 root 只读 `SystemConfig` 目录在 `config-migration.lock` 尚不存在时安全降级为只读直查清单，不尝试创建锁文件。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::config_scope_bounded_lock_shared_concurrency_and_timeout_error) | test(=storage::config_store::tests::config_scope_readonly_absent_and_ro_system_dir_fallback)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::config_scope_bounded_lock_shared_concurrency_and_timeout_error)'`（new，覆盖 AC1、AC2、AC3）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::config_scope_readonly_absent_and_ro_system_dir_fallback)'`（new，覆盖 AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=config scope bounded lock and ro-dir fallback; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=3; prod-files=6; scope=M; deps=RB-03b; writeset=序列化于 RB-03b; release=independent; split-from=RB-03b; exception=N/A`

---

### Task RB-03d: `GlobalConfig` 并发首写串行化与先非覆写 `hard_link` 墓碑预占防旧版二进制竞态（Track A，自 `RB-03b` 拆出，拆出 `RB-03f`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`c52cff5` / `v0.30.310`；Codex R4 `PASS`；覆盖 `GlobalConfig` Step F1 非覆写 `hard_link` 墓碑预占、不支持硬链拒收、并发首写串行化、旧二进制胜出零覆写与 `SQLITE_NOTADB` 阻断，及 Redb/SQLite 异步取消持锁回归）

**Description:** 100% 在独占目录 `src/storage/**` 内落实 `ADR-RB-02` #2 Step F1（`GlobalConfig` 作用域与共享 `hard_link` 失败分支）：空 `LIBRA_CONFIG_GLOBAL_DB` 目录首次新建 `Redb` 配置库在持 `LOCK_EX` 内、**创建 `config.redb` 之前**，先将已写满且 `sync_all()` + 设 `0400` 的 31 字节非 SQLite 哨兵 `LIBRA_CONFIG_AUTHORITY_IS_REDB\n` 经**绝不覆盖目标的 `std::fs::hard_link(tmp, config.db)`** 原子预占 `config.db`（不支持硬链返回 `Unsupported`/`EXDEV` 立即清理 `.tmp` 并 fail-closed 报 `LBR-IO-001`；遇 `AlreadyExists` 放弃建红库并完整保留旧二进制写入），并在 `GlobalConfig` 作用域实测新运行时并发首写、旧写者胜出零覆写与墓碑预占后旧二进制阻断。

**Out of scope:**
- `SystemConfig`（`0644`/`0444`）并发首写与旧二进制竞态：已按 `G-03`/`G-09` 拆出由 `RB-03f` 承接。
- 首次建库 4 步状态机的各崩溃边界恢复：由 `RB-03e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 旧版二进制不持 `config-migration.lock`，通过 `schema.rs:800` `OpenOptions::create_new(true)` 创建 `config.db`；若新二进制先建 `config.redb` 再 `rename` 墓碑会静默覆写旧二进制写入，必须先以绝不覆写目标的 `hard_link` 预占 `config.db` | `src/command/config.rs:257-295`；`src/internal/db/schema.rs:785-814`；`ADR-RB-02` #2 Step F1 |

**Acceptance criteria:**
- [x] `GlobalConfig` 空目录首次新建 `Redb` 配置库在持 `LOCK_EX` 内、创建 `config.redb` 之前，经非覆写 `std::fs::hard_link(tmp, config.db)` 原子发布已 `sync_all()` 的 `0400` 哨兵 `LIBRA_CONFIG_AUTHORITY_IS_REDB\n`（永不出现 0 字节 `config.db`）。
- [x] 当底层文件系统对 `hard_link` 返回 `Unsupported`/`ENOSYS`/`EXDEV` 时，清理 `.tmp` 且绝不降级覆盖型 `rename`，立即 fail-closed 报 `LBR-IO-001`。
- [x] **新运行时并发首写竞态（`GlobalConfig`）：** 在空 `LIBRA_CONFIG_GLOBAL_DB` 目录通过栅栏同步并发发起 `Sqlite` 与 `Redb` 写者，仅 1 方建立单权威与 `config-format.json(finalized = true)`，另一方 fail-closed 报 `LBR-CLI-003`。
- [x] **旧二进制胜出零覆写实测（`GlobalConfig`）：** 不持锁旧二进制先经 `create_new` 创建并写入 `config.db` 时，首次 `Redb` 写入命中 `hard_link` `AlreadyExists` 立即 fail-closed 报 `LBR-CLI-003` 且绝不建 `config.redb`，旧写入 100% 保留。
- [x] **墓碑预占后旧二进制阻断实测（`GlobalConfig`）：** `Redb` 先经 `hard_link` 预占 `0400` 墓碑并建库后，旧版 `libra config get/set --global` 经 `config.rs:257` 打开 `config.db` 立报 `SQLITE_NOTADB` fail-closed。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::global_config_hardlink_reserve_and_unsupported_fs_reject) | test(=storage::config_store::tests::global_config_concurrent_first_write_serializes_authority) | test(=storage::config_store::tests::global_config_hardlink_yields_to_legacy_writer_and_blocks_post_reserve)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::global_config_hardlink_reserve_and_unsupported_fs_reject)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::global_config_concurrent_first_write_serializes_authority)'`（new，覆盖 AC3）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::global_config_hardlink_yields_to_legacy_writer_and_blocks_post_reserve)'`（new，覆盖 AC4、AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03h`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=global config first-write hardlink legacy-race; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=3; prod-files=6; scope=M; deps=RB-03h; writeset=序列化于 RB-03h; release=independent; split-from=RB-03b; exception=N/A`

---

### Task RB-03f: `SystemConfig` 并发首写串行化与 `0444` 非覆写 `hard_link` 墓碑预占防旧版二进制竞态（Track A，自 `RB-03d` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`e5f2777` / `v0.30.311`；Codex R1 `PASS`；覆盖 `SystemConfig` `0444`/`0644` 墓碑预占、并发首写串行化、旧二进制胜出零覆写与 `SQLITE_NOTADB` 阻断）

**Description:** 100% 在独占目录 `src/storage/**` 内落实 `SystemConfig` 作用域（隔离 `LIBRA_CONFIG_SYSTEM_DB`）的 `ADR-RB-02` #2 Step F1 首次建库 `0444` 哨兵 `hard_link` 预占与 `0644` `config.redb` 权限姿态，逐项实测系统域并发首写竞态、不持锁旧写者胜出零覆写与墓碑预占后旧二进制阻断。

**Out of scope:**
- 首次建库 4 步状态机的各崩溃边界恢复：由 `RB-03e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `SystemConfig` 具有独立可变库路径（`/etc/libra/config.db` / `LIBRA_CONFIG_SYSTEM_DB`）与 `0644`/`0444` 权限姿态，须独立验收并发首写与旧写者竞态 | `src/command/config.rs:257-295`；`ADR-RB-02` #2 Step F1 |

**Acceptance criteria:**
- [x] `SystemConfig` 空目录首次新建 `Redb` 配置库在持 `LOCK_EX` 内、创建 `0644` `config.redb` 之前，经非覆写 `std::fs::hard_link` 原子预占 `0444` 哨兵 `config.db`。
- [x] **新运行时并发首写竞态（`SystemConfig`）：** 在空 `LIBRA_CONFIG_SYSTEM_DB` 目录并发发起 `Sqlite` 与 `Redb` 写者，仅 1 方建立单权威（`0644`）与 `config-format.json(finalized = true)`，另一方 fail-closed 报 `LBR-CLI-003`。
- [x] **旧二进制胜出零覆写实测（`SystemConfig`）：** 不持锁旧二进制先经 `create_new` 创建并写入系统 `config.db` 时，首次 `Redb` 写入命中 `AlreadyExists` 立即 fail-closed 报 `LBR-CLI-003` 且绝不建 `config.redb`，旧写入 100% 保留。
- [x] **墓碑预占后旧二进制阻断实测（`SystemConfig`）：** `Redb` 先预占 `0444` 墓碑并建库后，旧版 `libra config get/set --system` 打开 `config.db` 立报 `SQLITE_NOTADB` fail-closed。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::system_config_hardlink_reserve_and_concurrent_first_write) | test(=storage::config_store::tests::system_config_hardlink_yields_to_legacy_writer_and_blocks_post_reserve)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::system_config_hardlink_reserve_and_concurrent_first_write)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::system_config_hardlink_yields_to_legacy_writer_and_blocks_post_reserve)'`（new，覆盖 AC3、AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=system config first-write hardlink legacy-race; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=3; prod-files=6; scope=M; deps=RB-03d; writeset=序列化于 RB-03d; release=independent; split-from=RB-03d; exception=N/A`

---

### Task RB-03e: `GlobalConfig` 首次建库持久化状态机与全 3 边界崩溃恢复（Track A，自 `RB-03b` 拆出，拆出 `RB-03i`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`717364c`，`v0.30.312`；Codex R1–R4 全项发现含 Windows 恢复刷盘与回归测试已全部闭环，按用户授权免第五轮复跑直接落地并推送 `origin/main`）

**Description:** 100% 在独占目录 `src/storage/**` 内落实 `ADR-RB-02` #2 的首次 `Redb` 配置建库 4 步可恢复状态机（Step F1 `hard_link` 预占墓碑 → Step F2 发布 `config-format.json(finalized=false, init_phase="fresh_create", legacy_sqlite_tombstone=true, targets_staged_for_rename=false)` → Step F3 `config.redb.tmp` + `targets_staged_for_rename=true` + `rename` → Step F4 `finalized=true`），并在 `GlobalConfig` 作用域逐边界实测 3 处崩溃幂等恢复。

**Out of scope:**
- `SystemConfig`（`LIBRA_CONFIG_SYSTEM_DB`、`0644`/`0444` 权限）3 处崩溃边界恢复：已按 `G-03`/`G-09` 拆出由 `RB-03i` 承接。
- 双特性/单特性旧库分派矩阵：由 `RB-03c` 与 `RB-03g` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 若首次建库在创建 `config.redb` 与发布清单/墓碑之间崩溃且无持久化 `init_phase="fresh_create"` 状态，重启将落入误报损坏或放行缺墓碑孤立红库的死角 | `ADR-RB-02` #2 Step F1–F4 |

**Acceptance criteria:**
- [x] 首次 `Redb` 配置建库在持 `LOCK_EX` 下严格按 Step F1 → Step F2 → Step F3 → Step F4 顺序落盘并完成 `fsync_dir`。
- [x] **`GlobalConfig` 边界 1 崩溃（Step F1 `hard_link` 后、Step F2 清单落盘前）：** 后续 `Redb` 写者识别精确哨兵 `config.db`，幂等收敛至 `0600` `config.redb` + `finalized = true`。
- [x] **`GlobalConfig` 边界 2 崩溃（Step F2 `init_phase="fresh_create", finalized=false, targets_staged_for_rename=false`）：** 后续 `Redb` 写者清理残 `.tmp` 并幂等收敛至 `finalized = true`。
- [x] **`GlobalConfig` 边界 3 崩溃（Step F3 `targets_staged_for_rename=true` 且 `config.redb` 已就位、`finalized=true` 未落盘）：** 续跑零覆写保留已有 `config.redb` 全部键值并晋升 `finalized = true`。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::config_fresh_create_4step_state_machine_order_and_fsync) | test(=storage::config_store::tests::global_config_fresh_create_crash_boundaries_1_2_3)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::config_fresh_create_4step_state_machine_order_and_fsync)'`（new，覆盖 AC1）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::global_config_fresh_create_crash_boundaries_1_2_3)'`（new，覆盖 AC2–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03f`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=global config fresh-create state crash recovery; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=3; prod-files=6; scope=M; deps=RB-03f; writeset=序列化于 RB-03f; release=independent; split-from=RB-03b; exception=N/A`

---

### Task RB-03i: `SystemConfig` 首次建库全 3 边界崩溃恢复与 `0644`/`0444` 权限收敛（Track A，自 `RB-03e` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`14f3403`，`v0.30.313`，2026-11-22 15:59 UTC 于 `main` 提交并推送 `origin/main`；Stage 1 精确 1/6 文件写集 `src/storage/config_store.rs`、AC1–AC4 独立子进程 `LIBRA_CONFIG_SYSTEM_DB` + `umask(0o077)` 下全 3 边界收敛与 `0644`/`0444` 权限实测、fmt、严格 clippy 与 Codex R1 `VERDICT: PASS` 全绿）

**Description:** 100% 在独占目录 `src/storage/**` 内针对隔离夹具 `LIBRA_CONFIG_SYSTEM_DB` 逐项验证首次 `Redb` 系统配置建库全部 3 处崩溃边界恢复，并确保收敛后的 `config.redb`（`0644`）与路径墓碑 `config.db`（`0444`）权限与数据完整性完全满足 `GC-13`。

**Out of scope:**
- 双特性/单特性旧库分派矩阵：由 `RB-03c` 与 `RB-03g` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `SystemConfig` 独立可变目录拥有专属 `0644`/`0444` 权限要求，须独立实证全部 3 处首次建库崩溃恢复边界 | `ADR-RB-02` #2 Step F1–F4；`GC-RB-02` |

**Acceptance criteria:**
- [x] **`SystemConfig` 边界 1 崩溃（Step F1 `hard_link` 后、Step F2 清单落盘前）：** 在 `LIBRA_CONFIG_SYSTEM_DB` 识别精确哨兵 `config.db`，幂等收敛至 `0644` `config.redb` + `0444` 墓碑 + `finalized = true`。
- [x] **`SystemConfig` 边界 2 崩溃（Step F2 `init_phase="fresh_create", finalized=false, targets_staged_for_rename=false`）：** 清理残 `.tmp` 并幂等收敛至 `0644` `config.redb` + `finalized = true`。
- [x] **`SystemConfig` 边界 3 崩溃（Step F3 `targets_staged_for_rename=true` 且 `config.redb` 已就位、`finalized=true` 未落盘）：** 零覆写保留已有 `0644` `config.redb` 数据并晋升 `finalized = true`。
- [x] `SystemConfig` 上述 3 边界恢复完成后，`config.redb` 权限严格为 `0644` 且 `config.db` 墓碑权限严格为 `0444`。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::system_config_fresh_create_crash_boundaries_1_2_3_and_modes)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::system_config_fresh_create_crash_boundaries_1_2_3_and_modes)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03e`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=system config fresh-create crash recovery modes; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=3; prod-files=6; scope=S; deps=RB-03e; writeset=序列化于 RB-03e; release=independent; split-from=RB-03e; exception=N/A`

---

### Task RB-03c: 双特性构建 `Sqlite` 运行时配置零破坏分派（Track A，自 `RB-03b` 拆出，拆出 `RB-03g`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`a380e51`，`v0.30.314`，2026-11-22 16:40 UTC 于 `main` 提交并推送 `origin/main`；Stage 1 精确 3/6 文件写集 `src/storage/{mod,config_store}.rs` 与 `src/storage/sqlite/config_store.rs`、AC1–AC5 双特性 + 纯 `storage-sqlite` 零警告全绿含 pre-ledger 旧表只读与未来 schema `LBR-CONFIG-001` 拒收、纯 `storage-redb` 编译通过、fmt、严格 clippy 与 Codex R3 `VERDICT: PASS` 全绿）

**Description:** 100% 在独占目录 `src/storage/**` 内落实 `ADR-RB-02` #2 分支 A（双特性构建下默认 `Sqlite` 运行时，对应 `RB-30`/`RB-32` 切默认前态）：确保对已有 `config.db` 的读与写均零破坏保持，两文件皆无时在 `LOCK_EX` 下创建 `config.db` + `config-format.json(backend="sqlite", finalized=true, legacy_sqlite_tombstone=false)`（绝不建 `config.redb`），且遇仅存 `config.redb` 或已定稿清单权威不匹配时 fail-closed 返回 `LBR-CLI-003`。

**Out of scope:**
- `Redb` 运行时（双特性与纯 `storage-redb`）遇未迁移 `config.db` 的只读回退与写/读 fail-closed 策略：已按 `G-03`/`G-09` 拆出由 `RB-03g` 承接。
- `src/command/config/` CLI 接线：由 `RB-20` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RB-33` 切默认前双特性 `Sqlite` 运行时对 `config.db` 的读、写与首次新建必须零破坏 | `src/internal/config.rs:47-120`；`ADR-RB-02` #2 |

**Acceptance criteria:**
- [x] 双特性构建 + `Sqlite` 运行时对已有 `config.db` 正常读取。
- [x] 双特性构建 + `Sqlite` 运行时对已有 `config.db` 正常写入（绝不创建 `config.redb`）。
- [x] 双特性构建 + `Sqlite` 运行时在两文件皆无时首次写入在 `LOCK_EX` 下创建 `config.db` 与 `config-format.json(backend="sqlite", finalized=true, legacy_sqlite_tombstone=false)`。
- [x] 双特性构建 + `Sqlite` 运行时遇仅存在 `config.redb`（无 `config.db`）时 fail-closed 报 `LBR-CLI-003`。
- [x] 当 `config-format.json(finalized = true)` 声明的权威后端与当前写运行时不匹配时，非权威写路径 fail-closed 报 `LBR-CLI-003`。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::dual_feature_sqlite_runtime_config_dispatch_and_authority_guard)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::dual_feature_sqlite_runtime_config_dispatch_and_authority_guard)'`（new，覆盖 AC1–AC5）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(=storage::config_store::tests::dual_feature_sqlite_runtime_config_dispatch_and_authority_guard)'`（验证纯 SQLite 构建编译与回归通过，覆盖 AC1–AC3）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03i`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=config dual-feature sqlite runtime dispatch; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=3; prod-files=6; scope=M; deps=RB-03i; writeset=序列化于 RB-03i; release=independent; split-from=RB-03b; exception=N/A`

---

### Task RB-03g: `Redb` 运行时遇未迁移 `config.db` 策略（双特性只读回退/写拒收 + 纯红库读写双拒收）（Track A，自 `RB-03c` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`（`9da4b2a`，`v0.30.316`，2026-11-22 16:57 UTC 于 `main` 提交并推送 `origin/main`；Stage 1 精确 1/6 文件写集 `src/storage/config_store.rs`、AC1–AC4 双特性只读回退/写拒收与纯 `storage-redb` 读写双拒收 `LBR-CLI-003` 全绿、纯 `storage-sqlite` 编译通过、fmt、严格 clippy 与 Codex R1 `VERDICT: PASS` 全绿；`RB-03*` 全系列闭环）

**Description:** 100% 在独占目录 `src/storage/**` 内落实 `ADR-RB-02` #2 分支 B（`Redb` 运行时遇未迁移旧 `config.db`）：在双特性构建下只读透明回退旧值、写操作 fail-closed 提示 `libra-migrate --global/--system`；在纯 `storage-redb` 构建下读操作与写操作均显式 fail-closed 返回 `LBR-CLI-003`，绝不静默返回空配置。

**Out of scope:**
- `src/command/config/` CLI 接线：由 `RB-20` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 切默认后纯 `storage-redb` 构建遇未迁移旧 `config.db` 时读与写均须独立 fail-closed（绝不返回空配置），而双特性构建支持只读回退 | `src/internal/config.rs:47-120`；`ADR-RB-02` #2 |

**Acceptance criteria:**
- [x] 双特性构建 + `Redb` 运行时遇仅存未迁移 `config.db` 时，只读操作透明回退返回旧值。
- [x] 双特性构建 + `Redb` 运行时遇仅存未迁移 `config.db` 时，写操作 fail-closed 报 `LBR-CLI-003` 并提示 `libra-migrate --global/--system`。
- [x] 纯 `storage-redb` 构建遇仅存未迁移 `config.db` 时，读操作 fail-closed 报 `LBR-CLI-003`（绝不返回空配置）。
- [x] 纯 `storage-redb` 构建遇仅存未迁移 `config.db` 时，写操作 fail-closed 报 `LBR-CLI-003`（绝不新建 `config.redb`）。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::dual_feature_redb_runtime_legacy_config_db_read_fallback_and_write_reject) | test(=storage::config_store::tests::redb_only_legacy_config_db_read_and_write_fail_closed)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::config_store::tests::dual_feature_redb_runtime_legacy_config_db_read_fallback_and_write_reject)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::config_store::tests::redb_only_legacy_config_db_read_and_write_fail_closed)'`（new，覆盖 AC3、AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 6 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03c`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/config_store.rs`、`src/storage/sqlite/mod.rs`、`src/storage/sqlite/config_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/config_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 6 个文件（3 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=config redb runtime legacy config-db policy; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=3; prod-files=6; scope=M; deps=RB-03c; writeset=序列化于 RB-03c; release=independent; split-from=RB-03c; exception=N/A`

---

### Task RB-04: `GitStore` 核心引用、Reflog、Worktree 隔离与桥接原子绑定双后端实现（Track A，拆出 `RB-04b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-pending`（2026-11-22 17:39 UTC，`v0.30.318`，于 `main` 直接提交推送；Stage 1 精确 4 文件写集 `src/storage/{mod,git_store}.rs` 与 `src/storage/{sqlite,redb}/git_store.rs`、AC1–AC6 三组特性矩阵含 4 写 + 8 读 12 进程零停顿活体并发线性化压测、桥接符号 `HEAD` 同提交解析拒收与 Reflog 工作树跨界拒收、fmt、严格 clippy 与 Codex R4 `VERDICT: PASS` 全绿）

**Description:** 100% 在独占目录 `src/storage/**` 内定义并双后端实现 `GitStore` 核心引用与 Reflog 域（`reference`、`reflog` 及同事务 `agent_bridge_publication` 绑定），提供工作树作用域局部 `HEAD` 隔离、共享引用可见性与多进程线性化 CAS 读写。

**Out of scope:**
- `object_index`、`object_obliteration`、`notes`、`revision_ordinal`、`revision_ordinal_meta`：已按 `G-03`/`G-09` 拆出由 `RB-04b` 承接。
- 其余 50 张仓库状态表：由 `RB-05`、`RB-05b` 承接。
- 跨库代际与持久化意图发件箱：由 `RB-06b` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 引用与 Reflog 支持 worktree 作用域、符号引用与 CAS 原子更新；`agent_bridge_publication` 须与 `HEAD` 同事务提交 | `src/internal/head.rs:7-171`；`src/internal/reflog.rs:11-90`；`src/internal/ai/agent_bridge/publication.rs:5-26` |

**Acceptance criteria:**
- [x] `update_ref_cas` 在单写事务内原子完成旧值比较、引用更新与 Reflog 追加。
- [x] `update_ref_cas` 旧值不匹配时返回 `CasConflict` 不产生孤立 Reflog 行。
- [x] `update_ref_cas_with_bridge_publication` 在同一 `git.redb` 写事务内原子提交 `HEAD` 移动与 `agent_bridge_publication` 记录，回滚时不留孤行。
- [x] 主工作树与 linked worktree 的局部 `HEAD` 严格按 `worktree_id` 隔离。
- [x] 主工作树与 linked worktree 的非局部共享引用在双后端一致可见。
- [x] 多进程压测：4 写进程循环 `update_ref_cas` + 8 只读进程循环 `list_refs`/`get_ref` 线性化无锁错。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::git_store::tests::dual_backend_core_refs_reflog_and_bridge_cas_equivalence) | test(=storage::git_store::tests::redb_multiprocess_cas_linearizability_and_mvcc_reads) | test(=storage::git_store::tests::sqlite_core_refs_reflog_smoke)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::git_store::tests::dual_backend_core_refs_reflog_and_bridge_cas_equivalence)'`（new，覆盖 AC1–AC5）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::git_store::tests::redb_multiprocess_cas_linearizability_and_mvcc_reads)'`（new，覆盖 AC6）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(=storage::git_store::tests::sqlite_core_refs_reflog_smoke)'`（new，单特性 SQLite 回归）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-03g`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/git_store.rs`、`src/storage/sqlite/git_store.rs`、`src/storage/redb/git_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=dual-backend GitStore refs reflog bridge-cas; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=5/8; landing=3; prod-files=4; scope=M; deps=RB-03g; writeset=序列化于 RB-03g; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-04b: `GitStore` 对象索引、抹除围栏、Notes 与修订序号双后端实现（Track A，自 `RB-04` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内补齐 `GitStore` 核心 7 表的其余 5 张源表（`object_index`、`object_obliteration`、`notes`、`revision_ordinal`、`revision_ordinal_meta`）及同事务二级索引 `OBJECT_BY_PACK`，支持对象索引批量幂等写入、抹除对象读写双向拦截与 SHA-1/SHA-256/全零 OID 往返。

**Out of scope:**
- 其余 50 张仓库状态表：由 `RB-05`、`RB-05b` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `object_obliteration` 须拦截已抹除 OID 读写，`revision_ordinal` 须精确支持 SHA-1/SHA-256 与全零 OID | `src/internal/obliteration/mod.rs:24-90`；`src/internal/note.rs` |

**Acceptance criteria:**
- [x] `upsert_object_index_batch` 在单写事务内幂等写入 ≥ 1,000 条对象索引记录。
- [x] `object_obliteration` 围栏在读路径拦截已抹除对象 OID。
- [x] `object_obliteration` 围栏在写路径拦截已抹除对象 OID。
- [x] `notes` 在双特性等价测试中与 `Sqlite` 后端逐项一致。
- [x] `revision_ordinal` 与 `revision_ordinal_meta`（含 SHA-1、SHA-256 与全零 OID）在双后端等价测试中逐项一致。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::git_store::tests::dual_backend_object_index_obliteration_notes_and_ordinals) | test(=storage::git_store::tests::sqlite_object_index_and_obliteration_smoke)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::git_store::tests::dual_backend_object_index_obliteration_notes_and_ordinals)'`（new，覆盖 AC1–AC5）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(=storage::git_store::tests::sqlite_object_index_and_obliteration_smoke)'`（new，单特性 SQLite 回归）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-04`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/git_store.rs`、`src/storage/sqlite/git_store.rs`、`src/storage/redb/git_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=dual-backend GitStore obj-index obliterate notes; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=3; prod-files=4; scope=M; deps=RB-04; writeset=序列化于 RB-04; release=independent; split-from=RB-04; exception=N/A`

---

### Task RB-05: `GitStore` 核心工作流、Worktree 与 OpLog 状态 22 表双后端实现（Track A，拆出 `RB-05b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内实现「可复现源表清单 A」中 Git 核心工作流、Worktree 租约、OpLog v2、Change 谱系及脏树/稀疏视图共 **22 张**活跃仓库状态表的双后端存储与等价契约。

**Out of scope:**
- 其余 28 张 AI/记忆/治理/桥接发布投影表及 61 表静态清单守卫：已按 `G-03`/`G-09` 拆出由 `RB-05b` 承接。
- 业务调用方接线：由 `RB-13`..`RB-20` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `mutable_state_ownership.rs`（`L45-350`）与 `operation/store.rs` 定义 22 张工作流、Worktree、OpLog 与 Change 活跃状态表 | `src/internal/mutable_state_ownership.rs:45-350`；`src/internal/operation/store.rs:17-180` |

**Acceptance criteria:**
- [x] `sequence_state`、`rebase_state`、`bisect_state` 按 `worktree_id` 隔离并精确往返全零 OID。
- [x] `workspace_record` 与 `worktree_*`（4 表）租约注册、续期与吊销在双后端逐项一致。
- [x] OpLog v2（4 表：`operation`、`operation_parent`、`operation_head`、`operation_journal`）支持单事务原子追加与并发操作头收敛。
- [x] `change_identity`、`change_revision`、`change_predecessor` 与 `ai_operation_link`（4 表）在双后端等价测试中逐项一致。
- [x] `working_dirty*`、`layer*`、`sparse_view*`（6 表）在双后端等价测试中逐项一致。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::git_state_store::tests::dual_backend_22_workflow_worktree_oplog_tables_equivalence) | test(=storage::git_state_store::tests::sqlite_workflow_state_tables_smoke)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::git_state_store::tests::dual_backend_22_workflow_worktree_oplog_tables_equivalence)'`（new，覆盖 AC1–AC5）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::git_state_store::tests::dual_backend_22_workflow_worktree_oplog_tables_equivalence)'`（单特性红库回归，覆盖 AC1–AC5）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(=storage::git_state_store::tests::sqlite_workflow_state_tables_smoke)'`（new，单特性 SQLite 回归）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-04b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/git_state_store.rs`、`src/storage/sqlite/git_state_store.rs`、`src/storage/redb/git_state_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=dual-backend GitStore workflow worktree oplog 22; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=3; prod-files=4; scope=M; deps=RB-04b; writeset=序列化于 RB-04b; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-05b: `GitStore` AI/记忆/治理/桥接投影 28 表双后端实现与 61 源表守卫（Track A，自 `RB-05` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内补齐「可复现源表清单 A」剩余 **28 张**活跃仓库源表（19 张 `ai_*`、5 张 `memory_*`、`approved_permission`/`automation_log`/`source_call_log`、以及 `agent_bridge_publication` 与 `agent_bridge_index_publication` 两阶段发布表）的双后端实现与 61 活跃源表静态清单守卫。

**Out of scope:**
- 业务调用方接线：由 `RB-20`、`RB-23`、`RB-25` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `mutable_state_ownership.rs`（`L290-503`）、`sql/migrations/2026092901..02` 与 `agent_bridge/publication.rs:22,36` 定义其余 28 张 AI/记忆/治理/桥接活跃表 | `src/internal/mutable_state_ownership.rs:290-503`；`src/internal/ai/agent_bridge/publication.rs:22-40` |

**Acceptance criteria:**
- [x] 19 张 `ai_*` 线程、调度与索引表在双后端等价测试中逐项一致。
- [x] 5 张 `memory_*` 记忆表在双后端等价测试中逐项一致。
- [x] `memory_*` 路径与时间二级索引查询（`MEMORY_BY_PATH`）在双后端返回一致排序结果。
- [x] `approved_permission`、`automation_log` 与 `source_call_log`（3 表）在双后端等价测试中逐项一致。
- [x] `agent_bridge_publication` 与 `agent_bridge_index_publication` 两阶段（`prepared` → `published`）状态机在双后端逐项一致。
- [x] 静态表清单断言校验 `GitStore` + `ConfigStore` 映射集合与基线 **61 张** Repository 活跃持久源表逐名相等（不混算 7 张已退役删表与 `META_*` 内部表）。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::git_state_store::tests::dual_backend_28_ai_memory_bridge_tables_equivalence) | test(=storage::git_state_store::tests::repository_61_source_tables_manifest_complete) | test(=storage::git_state_store::tests::sqlite_ai_memory_bridge_tables_smoke)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::git_state_store::tests::dual_backend_28_ai_memory_bridge_tables_equivalence)'`（new，覆盖 AC1–AC5）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::git_state_store::tests::repository_61_source_tables_manifest_complete)'`（new，覆盖 AC6）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(=storage::git_state_store::tests::sqlite_ai_memory_bridge_tables_smoke)'`（new，单特性 SQLite 回归）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-05`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/git_state_store.rs`、`src/storage/sqlite/git_state_store.rs`、`src/storage/redb/git_state_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=dual-backend GitStore ai memory bridge 28 and 61; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=5/8; landing=3; prod-files=4; scope=M; deps=RB-05; writeset=序列化于 RB-05; release=independent; split-from=RB-05; exception=N/A`

---

### Task RB-06: `CaptureStore` 全 25 张源表批量去重与 `AgentBridgeStore` 双后端实现（Track A，拆出 `RB-06c`、`RB-06b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内定义 `CaptureStore` trait，完成 `SqliteCaptureStore` 与 `RedbCaptureStore`（`.libra/private/capture.gen-<id>.redb`）全部 **25 张 Capture 源表**建模、单事务批量去重写入、`AgentBridgeStore` 连续序列确认以及 `capture.redb` 过新版本隔离（`LBR-AGENT-048`），同时显式区隔 `META_*`/`QUEUE_HIGHWATER_*` 与二级索引表。

**Out of scope:**
- `agent_import_tombstone` 重导拦截、审计只追加不变量与 Unix `0700`/`0600`/`O_NOFOLLOW`/属主校验：已按 `G-03`/`G-09` 拆出由 `RB-06c` 承接。
- 跨库持久化意图重放与 `META_GENERATION` 一致快照协议：已拆出由 `RB-06b` 承接。
- 磁盘队列读写逻辑：由 `RB-07`..`RB-09b` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `2026111601_capture_database.sql` 精确定义 25 条 `CREATE TABLE`（2 账本/收据 + 23 业务目录表） | `sql/migrations/2026111601_capture_database.sql:7-453` |

**Acceptance criteria:**
- [x] 静态表清单断言校验 `CaptureStore` 精确覆盖 25 张 Capture 源表，并将 `META_*`/`QUEUE_HIGHWATER_*` 与二级索引表单独登记不混入源表计数。
- [x] 双特性等价测试在 `SqliteCaptureStore` 与 `RedbCaptureStore` 上覆盖全部 25 张 Capture 源表结果逐项一致。
- [x] `append_capture_batch` 在单次写事务内批量写入 ≥ 128 条事件。
- [x] `append_capture_batch` 按完整语义 `dedup_key` 幂等去重，重复帧不产生重复行。
- [x] `AgentBridgeStore::append_and_ack_events` 基于 `(session_id, event_seq)` 复合键单事务推进连续 `acked_seq`。
- [x] `capture.redb` `META_SCHEMA` 过新时仅报 `LBR-AGENT-048` 而不阻断 `GitStore` 正常读写。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::capture_store::tests::dual_backend_25_source_tables_equivalence) | test(=storage::capture_store::tests::redb_bridge_contiguous_ack_and_batch_dedup) | test(=storage::capture_store::tests::redb_capture_future_schema_isolated_from_git_store)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::capture_store::tests::dual_backend_25_source_tables_equivalence)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::capture_store::tests::redb_bridge_contiguous_ack_and_batch_dedup)'`（new，覆盖 AC3、AC4、AC5）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::capture_store::tests::redb_capture_future_schema_isolated_from_git_store)'`（new，覆盖 AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-05b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/capture_store.rs`、`src/storage/sqlite/capture_store.rs`、`src/storage/redb/capture_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=dual-backend CaptureStore 25 tables batch bridge; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=5/8; landing=3; prod-files=4; scope=M; deps=RB-05b; writeset=序列化于 RB-05b; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-06c: `RedbCaptureStore` 同事务墓碑与只追加审计不变量（Track A，自 `RB-06` 拆出，拆出 `RB-06d`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `RedbCaptureStore` 落实数据完整性不变量：`agent_import_tombstone` 同事务重导拒收以及 `agent_audit_log` 与 `agent_workspace_scope_audit` 只追加保护（拒绝覆写与删除）。

**Out of scope:**
- Unix `.libra/private/` 目录与 `capture.gen-<id>.redb` 文件权限位、`O_NOFOLLOW` 符号链接拒收与属主校验：已按 `G-03`/`G-09` 拆出由 `RB-06d` 承接。
- 跨库意图发件箱重放与一致快照：由 `RB-06b` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `2026111601_capture_database.sql` 定义只追加审计触发器与导入墓碑 | `sql/migrations/2026111601_capture_database.sql:620-632,777-787` |

**Acceptance criteria:**
- [x] `RedbCaptureStore` 同事务强制 `agent_import_tombstone` 拒绝已墓碑化身份重导。
- [x] `RedbCaptureStore` 同事务拒绝覆写 `agent_audit_log` 已有主键。
- [x] `RedbCaptureStore` 同事务拒绝删除 `agent_audit_log` 已有主键。
- [x] `RedbCaptureStore` 同事务拒绝覆写 `agent_workspace_scope_audit` 已有主键。
- [x] `RedbCaptureStore` 同事务拒绝删除 `agent_workspace_scope_audit` 已有主键。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::capture_store::tests::redb_tombstone_and_audit_append_only_invariants)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::capture_store::tests::redb_tombstone_and_audit_append_only_invariants)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-06`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/capture_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/capture_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件（2 个生产目录：`src/storage/`、`src/storage/redb/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=CaptureStore tombstone and append-only invariants; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=4; scope=M; deps=RB-06; writeset=序列化于 RB-06; release=independent; split-from=RB-06; exception=N/A`

---

### Task RB-06d: `RedbCaptureStore` Unix 私密目录与库文件安全姿态（Track A，自 `RB-06c` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `RedbCaptureStore` 落实 Unix 私密安全姿态：分别对 `.libra/private/` 目录与 `capture.gen-<id>.redb` 文件强制精确权限位（`0700`/`0600`）、`O_NOFOLLOW` 符号链接拒收与当前有效 UID 属主校验。

**Out of scope:**
- 跨库意图发件箱重放与一致快照：由 `RB-06b` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `GC-RB-03` 要求对 `.libra/private/` 与 `capture.gen-<id>.redb` 分别执行 `0700`/`0600`、`O_NOFOLLOW` 与当前有效 UID 校验 | `GC-RB-03` |

**Acceptance criteria:**
- [x] Unix 创建 `.libra/private/` 目录满足精确 `0700` 权限位。
- [x] Unix 创建 `capture.gen-<id>.redb` 文件满足精确 `0600` 权限位。
- [x] Unix 打开 `.libra/private/` 遇到符号链接时按 `O_NOFOLLOW` fail-closed 拒绝。
- [x] Unix 打开 `capture.gen-<id>.redb` 遇到符号链接时按 `O_NOFOLLOW` fail-closed 拒绝。
- [x] Unix 打开 `.libra/private/` 遇到非当前有效 UID 属主时 fail-closed 拒绝。
- [x] Unix 打开 `capture.gen-<id>.redb` 遇到非当前有效 UID 属主时 fail-closed 拒绝。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::capture_store::tests::redb_private_dir_and_db_unix_permissions_nofollow_and_owner)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::capture_store::tests::redb_private_dir_and_db_unix_permissions_nofollow_and_owner)'`（new，覆盖 AC1–AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-06c`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/capture_store.rs`、`src/storage/redb/mod.rs`、`src/storage/redb/capture_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件（2 个生产目录：`src/storage/`、`src/storage/redb/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=CaptureStore unix private dir/file posture; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=2; prod-files=4; scope=S; deps=RB-06c; writeset=序列化于 RB-06c; release=independent; split-from=RB-06c; exception=N/A`

---

### Task RB-06b: 跨库持久化意图发件箱提交与写前强制重放协调器（Track A，自 `RB-06` 拆出，拆出 `RB-06e`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内实现 `ADR-RB-06` 跨库一致性协调器 `src/storage/cross_store.rs`（及 `git_store.rs`/`capture_store.rs` 事务挂接）：在 `git.redb` 维护 `META_CROSS_STORE_INTENT` 发件箱与单调 `META_GENERATION`，实现两步持久化提交、半提交态只读三步快照 fail-closed（`g1 != c1` 超 5 次重试报 `LBR-IO-001`）、新跨库写入前强制按升序重放待决意图（严禁 `max + 1` 掩盖缺口）及 `capture.redb` 不可达拒推进。

**Out of scope:**
- 显式 `reconcile_pending_intents()` 修复、修复后一致快照与已对账意图清理：已按 `G-03`/`G-09` 拆出由 `RB-06e` 承接。
- 云命令与 `agent doctor --repair` 接线：由 `RB-26`、`RB-28` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 双库独立提交间崩溃会导致代际分歧，需持久化意图与强制写前重放防止后续提交掩盖丢失捕获 | `ADR-RB-06`；`GC-RB-04` |

**Acceptance criteria:**
- [x] 正常跨库提交先在 `git.redb` 原子写入 Git 变更、`META_GENERATION = git_gen + 1` 与 `META_CROSS_STORE_INTENT[gen]`，再在 `capture.redb` 幂等应用该意图并写入同一 `gen`。
- [x] 故障注入（`git.redb` 提交 `gen = N` 后、`capture.redb` 提交前崩溃）：只读三步快照检测 `g1 != c1` 经 5 次重试后 fail-closed 报 `LBR-IO-001`。
- [x] 故障注入后发起下一笔跨库提交（`N + 1`）前，协调器先将未对账意图 `N` 重放至 `capture.redb` 再提交 `N + 1`（完整保留 `N` 与 `N + 1`，绝不掩盖缺口）。
- [x] 故障注入后若 `capture.redb` 不可达或损坏，后续跨库写者立即 fail-closed 拒绝推进新代际。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::cross_store::tests::partial_commit_fault_blocks_snapshot_and_subsequent_writer_replays_first) | test(=storage::cross_store::tests::unreachable_capture_blocks_new_gen)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::cross_store::tests::partial_commit_fault_blocks_snapshot_and_subsequent_writer_replays_first)'`（new，覆盖 AC1–AC3）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::cross_store::tests::unreachable_capture_blocks_new_gen)'`（new，覆盖 AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-06d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/cross_store.rs`、`src/storage/redb/git_store.rs`、`src/storage/redb/capture_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件（2 个生产目录：`src/storage/`、`src/storage/redb/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=cross-store durable intent replay and guard; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=4; scope=M; deps=RB-06d; writeset=序列化于 RB-06d; release=independent; split-from=RB-06; exception=N/A`

---

### Task RB-06e: 跨库显式 `reconcile_pending_intents` 修复、一致快照冻结与已对账意图清理（Track A，自 `RB-06b` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内补齐 `cross_store.rs` 的显式修复入口 `reconcile_pending_intents()`、修复后只读三步一致快照（`g1 == c1 == g2`）冻结、已对账意图（`gen <= capture_gen`）自动清理防无限增长以及重复调用幂等性。

**Out of scope:**
- 云命令与 `agent doctor --repair` 接线：由 `RB-26`、`RB-28` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `libra agent doctor --repair` 与 `libra fsck` 依赖幂等 `reconcile_pending_intents()` 补齐代际并清理已对账意图 | `ADR-RB-06` #2、#3 |

**Acceptance criteria:**
- [x] 显式 `reconcile_pending_intents()` 对半提交状态补齐 `capture.redb` 直至 `capture_gen == git_gen`。
- [x] 完成 `reconcile_pending_intents()` 后，只读三步快照以 `g1 == c1 == g2` 成功冻结导出。
- [x] 连续二次调用 `reconcile_pending_intents()` 保持完全幂等、不重复写入业务行。
- [x] 已对账意图（`gen <= capture_gen`）在后续写事务或修复中自动清理，无无限增长。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::cross_store::tests::explicit_reconcile_idempotent_snapshot_and_intent_pruning)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::cross_store::tests::explicit_reconcile_idempotent_snapshot_and_intent_pruning)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-06b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/cross_store.rs`、`src/storage/redb/git_store.rs`、`src/storage/redb/capture_store.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件（2 个生产目录：`src/storage/`、`src/storage/redb/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=cross-store reconcile snapshot and intent pruning; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=4; scope=S; deps=RB-06b; writeset=序列化于 RB-06b; release=independent; split-from=RB-06b; exception=N/A`

---

### Task RB-07: Capture 队列 16 字节自认证帧头 + 全帧双 CRC32C 编解码与全量去重键（Track A，拆出 `RB-07g`、`RB-07c`、`RB-07b`、`RB-08`、`RB-09`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内实现 `ADR-RB-05` #2 双校验帧线协议编码与无损往返解码核心（`src/storage/queue/frame.rs`）：采用 `crc32c 0.6.9` Castagnoli 多项式实现固定 16 字节自认证帧头 `[MAGIC: 4B = b"LCE1"][FRAME_BODY_LEN: u32_le][HEADER_CRC32C: u32_le][FRAME_CRC32C: u32_le]` + `[FLAGS][DEDUP_KEY_LEN][DEDUP_KEY_UTF8][POSTCARD_ENVELOPE]`（`HEADER_CRC32C` 覆盖前 8 字节，`FRAME_CRC32C` 覆盖全帧非校验字段），并携带完整 UTF-8 语义 `dedup_key`。

**Out of scope:**
- 编码载荷/去重键上下界、`MAX_FRAME_BODY_LEN` 常量与 `FRAME_BODY_LEN < 3` 守卫：已按 `G-03`/`G-09` 拆出由 `RB-07g` 承接。
- 顺序流解码器、分配前界限护卫与已确认帧逐头字段/全负载单字节篡改拒收：由 `RB-07c`、`RB-07f`、`RB-07d` 承接。
- 双通道目录权限姿态与全新 Redb 仓 `capture-spool` 墓碑初始化：由 `RB-07b`、`RB-07e` 承接。
- 共享容量锁并发追加、`LTM2` 双副本与消费高水位：由 `RB-08`..`RB-09e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `Cargo.toml:103` 基线仅含 IEEE `crc32fast`（非 CRC32C），须以 `crc32c 0.6.9` 落实 RFC 3720 双校验 16B 自认证帧头与全量去重键帧格式 | `Cargo.toml:103`；`ADR-RB-05` #2 |

**Acceptance criteria:**
- [x] `frame::crc32c_checksum` 通过 RFC 3720 标准测试向量 `b"123456789" -> 0xe3069283`。
- [x] 帧编码产出固定 16 字节头（含 `MAGIC=b"LCE1"`、`FRAME_BODY_LEN`、覆盖前 8 字节的 `HEADER_CRC32C` 与覆盖全帧的 `FRAME_CRC32C`）。
- [x] 合法编码帧经单帧解码器可 100% 无损往返恢复原始 `flags`、`dedup_key` 与 `postcard` 负载。
- [x] 帧编码携带完整语义 UTF-8 `dedup_key`，两个仅末尾 1 字节不同的长键解码后完整区分无碰撞。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::crc32c_rfc3720_vector_and_16b_dual_crc_roundtrip)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::crc32c_rfc3720_vector_and_16b_dual_crc_roundtrip)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-06e`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/frame.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点：`src/storage/`、`src/storage/queue/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue 16B header+frame crc32c codec; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=3; scope=M; deps=RB-06e; writeset=序列化于 RB-06e; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-07g: Capture 队列帧编码负载/去重键上下界与 `MAX_FRAME_BODY_LEN` 守卫（Track A，自 `RB-07` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `src/storage/queue/frame.rs` 落实帧编码前后上下界守卫：`4 MiB` 单负载上限、`512 B` 去重键上限、`MAX_FRAME_BODY_LEN == 3 + 512 + 4 MiB` 常量不变量以及 `FRAME_BODY_LEN < 3` 非法体长下界拒收。

**Out of scope:**
- 顺序流解码器逐头/逐体字段篡改拒收与 `CleanEof`/`TruncatedTail` 分类：由 `RB-07c`、`RB-07f`、`RB-07d` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-05` #2 强制约束 `3 <= FRAME_BODY_LEN <= 3 + 512 + 4 MiB`，单负载 ≤ `4 MiB`，去重键 ≤ `512 B` | `ADR-RB-05` #2 |

**Acceptance criteria:**
- [x] 负载超过 `4 MiB` 的输入在编码阶段即 fail-closed 拒绝。
- [x] `dedup_key` 超过 `512 B` 的输入在编码阶段即 fail-closed 拒绝。
- [x] `MAX_FRAME_BODY_LEN` 常量精确等于 `3 + 512 + 4 MiB`。
- [x] 非法体长下界（`FRAME_BODY_LEN < 3`）被解码守卫 fail-closed 拒绝。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::frame_encode_limits_and_min_max_body_len_guard)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::frame_encode_limits_and_min_max_body_len_guard)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-07`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/frame.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点：`src/storage/`、`src/storage/queue/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue frame encode bounds and body-len guard; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-07; writeset=序列化于 RB-07; release=independent; split-from=RB-07; exception=N/A`

---

### Task RB-07c: Capture 队列 16 字节帧头逐字段分配前 `CorruptHeader` 拒收（Track A，自 `RB-07` 拆出，拆出 `RB-07f`、`RB-07d`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内实现 `src/storage/queue/frame.rs` 顺序流解码器的固定 16 字节帧头分配前守卫：在任何体内存分配或恢复截断之前先校验 `MAGIC`、`HEADER_CRC32C` 与 `3 <= FRAME_BODY_LEN <= MAX_FRAME_BODY_LEN`，逐字段实测 `MAGIC`、`FRAME_BODY_LEN`（正常界内翻转与超大值翻转分别检验）与 `HEADER_CRC32C` 单字节篡改的分配前 `CorruptHeader` 拒收。

**Out of scope:**
- 多帧 `CleanEof`、半体 `TruncatedTail` 与 `1..15` 字节尾部半头 `TruncatedTail` 分类：已按 `G-03`/`G-09` 拆出由 `RB-07f` 承接。
- 体部与 `FRAME_CRC32C` 5 个字段逐一单字节篡改拒收（`CorruptPayload`）：由 `RB-07d` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 若 `MAGIC`/`FRAME_BODY_LEN` 未受 CRC32C 保护或未在分配前核验界限，单字节篡改帧长可能被误判为段尾半帧截断或引起超大堆分配 | `ADR-RB-05` #2 |

**Acceptance criteria:**
- [x] 已确认帧中 `MAGIC` 任一单字节翻转在分配体缓冲前即被 `HEADER_CRC32C` 拒绝并返回 `FrameDecodeError::CorruptHeader`。
- [x] 已确认帧中 `FRAME_BODY_LEN` 单字节翻转（仍在文件剩余长度内）在分配体缓冲前即被 `HEADER_CRC32C` 拒绝并返回 `FrameDecodeError::CorruptHeader`。
- [x] 已确认帧中 `FRAME_BODY_LEN` 单字节翻转至超大值（超过剩余文件长度或超过 `MAX_FRAME_BODY_LEN`）在分配体缓冲前即被 `HEADER_CRC32C` 拒绝并返回 `FrameDecodeError::CorruptHeader`（绝不误判为 `TruncatedTail`、零超大分配）。
- [x] 已确认帧中 `HEADER_CRC32C` 任一单字节翻转在分配体缓冲前即被拒绝并返回 `FrameDecodeError::CorruptHeader`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::acknowledged_frame_each_header_field_bitflip_rejected_pre_alloc)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::acknowledged_frame_each_header_field_bitflip_rejected_pre_alloc)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-07g`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/frame.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点：`src/storage/`、`src/storage/queue/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue 16B header per-field pre-alloc reject; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-07g; writeset=序列化于 RB-07g; release=independent; split-from=RB-07; exception=N/A`

---

### Task RB-07f: Capture 队列顺序解码器 `CleanEof` 与 `TruncatedTail` 精确分类（Track A，自 `RB-07c` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `frame.rs` 完成顺序帧解码器的流终止精确分类：区分空输入 `CleanEof`、多帧合法流精确文件尾 `CleanEof`、合法 `HEADER_CRC32C` 但剩余体字节不足 `FRAME_BODY_LEN` 的 `TruncatedTail`，以及文件尾仅余 `1..15` 字节不完整帧头的 `TruncatedTail`。

**Out of scope:**
- 体部与 `FRAME_CRC32C` 5 个字段逐一单字节篡改拒收：由 `RB-07d` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 顺序解码器须将合法 EOF、体半截与头半截三类终止状态精确区分，供恢复扫描器严格按提交边界处置 | `ADR-RB-05` #2 |

**Acceptance criteria:**
- [x] 帧顺序解码器对 0 字节空流返回 `CleanEof`。
- [x] 帧顺序解码器对多帧合法流逐帧无损解码并在精确文件尾返回 `CleanEof`。
- [x] 帧顺序解码器对合法 `HEADER_CRC32C` 但剩余字节不足 `FRAME_BODY_LEN` 的半帧严格归类为 `TruncatedTail`。
- [x] 帧顺序解码器对文件尾仅余 `1..15` 字节的不完整帧头严格归类为 `TruncatedTail`（与 `CleanEof` 及 `CorruptHeader` 严格区分）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::seq_decoder_distinguishes_clean_eof_and_truncated_tail_variants)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::seq_decoder_distinguishes_clean_eof_and_truncated_tail_variants)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-07c`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/frame.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点：`src/storage/`、`src/storage/queue/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue seq decoder clean-eof truncated-tail; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-07c; writeset=序列化于 RB-07c; release=independent; split-from=RB-07c; exception=N/A`

---

### Task RB-07d: Capture 队列已确认帧体部与 `FRAME_CRC32C` 逐字段篡改拒收（Track A，自 `RB-07c` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `src/storage/queue/frame.rs` 对头校验通过后的全帧校验路径逐字段落实并实测：`FRAME_CRC32C`、`FLAGS`、`DEDUP_KEY_LEN`、`DEDUP_KEY_UTF8` 与 `POSTCARD_ENVELOPE` 各自任一单字节翻转均精确返回 `FrameDecodeError::CorruptPayload`。

**Out of scope:**
- 双通道目录安全姿态与全新 Redb 仓 `capture-spool` 墓碑初始化：由 `RB-07b` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-05` #2 要求 `FRAME_CRC32C` 覆盖全帧非校验字段并对 5 个体/校验字段各自单字节翻转精确返回 `FrameDecodeError::CorruptPayload` | `ADR-RB-05` #2 |

**Acceptance criteria:**
- [x] 已确认帧中 `FRAME_CRC32C` 任一单字节翻转被拒绝并返回 `FrameDecodeError::CorruptPayload`。
- [x] 已确认帧中 `FLAGS` 任一单字节翻转被 `FRAME_CRC32C` 拒绝并返回 `FrameDecodeError::CorruptPayload`。
- [x] 已确认帧中 `DEDUP_KEY_LEN` 任一单字节翻转被 `FRAME_CRC32C` 拒绝并返回 `FrameDecodeError::CorruptPayload`。
- [x] 已确认帧中 `DEDUP_KEY_UTF8` 任一单字节翻转被 `FRAME_CRC32C` 拒绝并返回 `FrameDecodeError::CorruptPayload`。
- [x] 已确认帧中 `POSTCARD_ENVELOPE` 任一单字节翻转被 `FRAME_CRC32C` 拒绝并返回 `FrameDecodeError::CorruptPayload`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::acknowledged_frame_each_body_and_crc_field_bitflip_rejected)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::frame::tests::acknowledged_frame_each_body_and_crc_field_bitflip_rejected)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-07f`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/frame.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点：`src/storage/`、`src/storage/queue/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue body and frame-crc per-field reject; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-07f; writeset=序列化于 RB-07f; release=independent; split-from=RB-07c; exception=N/A`

---

### Task RB-07b: Capture 队列双通道安全姿态与全新空通道初始 34B `LTM2` 双副本预建（Track A，自 `RB-07` 拆出，拆出 `RB-07e`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内实现 `src/storage/queue/mod.rs` 的双通道安全目录初始化（`0700` 权限、`O_NOFOLLOW` 符号链接拒收、Unix 属主校验），并在全新空通道初始化（`events-*.log` 数为 `0` 且 `tail.meta`/`tail.meta.bak` 均不存在）时先原子落盘并 `fsync_dir` 初始 34 字节 `LTM2` `(min_live_seg=0, active_seg=0, committed_len=0, prefix_crc32c=0, sealed=[])` 双副本 `tail.meta` 与 `tail.meta.bak`（确保首次入队崩溃归入 Case C 而永不产生合法双缺态）。

**Out of scope:**
- 全新 Redb 仓 `capture-spool` `0400` 墓碑初始化与旧 Hook `ensure_spool` 阻断：已按 `G-03`/`G-09` 拆出由 `RB-07e` 承接。
- 并发追加、段轮转与已确认前缀防护：由 `RB-08`..`RB-08e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 全新空通道须先预建初始 34B `LTM2` `(0,0,0,0,[])` 双副本并 `fsync_dir`，确保一切正常崩溃均至少保留 1 份有效元数据边界，从而使 0 段缺失与非空 Subcase B2 均可无条件 fail-closed | `ADR-RB-05` #3；`GC-RB-03` |

**Acceptance criteria:**
- [x] 双通道初始化在 Unix 上以精确 `0700` 权限创建 `priority/`、`normal/`、`quarantine/` 目录。
- [x] 双通道初始化对符号链接队列或通道目录按 `O_NOFOLLOW` fail-closed 拒绝。
- [x] 双通道初始化在 Unix 上对非当前有效 UID 属主目录 fail-closed 拒绝。
- [x] 全新空通道初始化（`0` 个 `events-*.log` 且双副本均不存在）在创建任何段文件之前，先原子创建 `0600` 初始 34B `LTM2` `(0, 0, 0, 0, [])` `tail.meta` 与 `tail.meta.bak`。
- [x] 上述初始双副本创建完成后立即执行 `fsync_dir(lane_dir)` 确保持久落盘。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::queue::tests::dual_lane_directory_permissions_nofollow_owner_and_initial_meta)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::queue::tests::dual_lane_directory_permissions_nofollow_owner_and_initial_meta)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-07d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/frame.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点：`src/storage/`、`src/storage/queue/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue lane posture and init dual-meta; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=3; scope=M; deps=RB-07d; writeset=序列化于 RB-07d; release=independent; split-from=RB-07; exception=N/A`

---

### Task RB-07e: 全新 Redb 仓 `capture-spool` `0400` 墓碑初始化与旧 Hook 阻断（Track A，自 `RB-07b` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `src/storage/queue/mod.rs` 实现 `init_redb_capture_spool_tombstones`：在全新 Redb 仓建立 `.libra/private/capture-spool/`（`0700`）、`capacity.lock`（`0600`）与 `{tmp,ready,quarantine}` `0400` 普通文件墓碑并 `fsync_dir`，从首日起阻断旧版 SQLite Hook `spool.rs:330` `ensure_spool`。

**Out of scope:**
- 并发追加、`LTM2` 双副本元数据更新与恢复：由 `RB-08`..`RB-08h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `spool.rs:330-355` `ensure_spool` 在缺失子目录时自动建目录，故全新 Redb 仓必须自首日建立 3 个 `0400` 普通文件墓碑阻断旧 Hook | `src/internal/ai/capture/spool.rs:330-355`；`ADR-RB-02` #1；`GC-RB-03` |

**Acceptance criteria:**
- [x] `init_redb_capture_spool_tombstones` 在全新 Redb 仓以精确 `0700` 权限创建 `.libra/private/capture-spool/` 目录。
- [x] `init_redb_capture_spool_tombstones` 以精确 `0600` 权限创建 `.libra/private/capture-spool/capacity.lock`。
- [x] `init_redb_capture_spool_tombstones` 在 `capture-spool/{tmp,ready,quarantine}` 原子创建 `0400` 普通文件墓碑。
- [x] 上述墓碑创建完成后立即执行 `fsync_dir` 确保持久落盘。
- [x] 既有旧 SQLite Hook 针对该墓碑化全新 Redb 仓调用 `spool.rs:330` `ensure_spool` 立即 fail-closed。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::queue::tests::fresh_redb_init_spool_tombstones_blocks_legacy_ensure_spool)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::queue::tests::fresh_redb_init_spool_tombstones_blocks_legacy_ensure_spool)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-07b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/frame.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点：`src/storage/`、`src/storage/queue/`）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=fresh redb capture-spool 0400 tombstones; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-07b; writeset=序列化于 RB-07b; release=independent; split-from=RB-07b; exception=N/A`

---

### Task RB-08: Capture 队列并发追加、`LTM2` 双副本 `tail.meta`/`tail.meta.bak` 原子提交与 Case C 同段恢复（Track A，自 `RB-07` 拆出，拆出 `RB-08t`、`RB-08d`、`RB-08b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内实现 `segment.rs` 崩溃安全追加核心：在双通道共享 `.libra/private/capture-queue/append.lock` 内执行单次 `write_all` + `seg.sync_data()`，随后以 `LTM2` 校验结构（30B 头 + `16B × SEALED_COUNT` 轮转封存表 + 4B `META_CRC32C`，双副本按 `(active_seg, committed_len, min_live_seg)` 严格全序收敛）依次经原子替换协议提交 **`tail.meta` 与 `tail.meta.bak` 双副本**后才释锁返回 `Ok`；并实现 Case C 同段未提交有效帧保留与双副本更新（含首帧写入崩溃，因 `RB-07b` 已预建初始 34B `LTM2` 双副本）。

**Out of scope:**
- 断电持久隔离发布强约束原语（先 `.tmp → sync_data → rename → fsync_dir` 再 `truncate`/`unlink`）：已按 `G-03`/`G-09` 拆出由 `RB-08t` 承接；Case D 旧段尾恢复与有效后继保留由 `RB-08d` 承接；Case D 空/半帧孤儿清理由 `RB-08f` 承接。
- 跨通道 `256 MiB` 合并容量边界、超时与延迟预算由 `RB-08b` 承接；16 MiB 段轮转、两步 Case C 尾损预修、Case C 全边界崩溃/嵌套 Case C→D 与 `256` 封存表全链路由 `RB-08g`..`RB-08s` 承接。
- 活跃段 Case A/E/C 位翻转与短段拒收、Subcase B1 单损拒收、0 段文件四分法以及非空 Subcase B2 无条件 fail-closed 守卫：由 `RB-08c`、`RB-08e` 与 `RB-08h` 承接。
- 消费侧批写、`LTM2.sealed[]` 认证边界校验、锁序协调与零跳帧原子高水位恢复：由 `RB-09`..`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LTM2` 双副本 `tail.meta` + `tail.meta.bak`（含 `MIN_LIVE_SEG`/`PREFIX_CRC32C`/`sealed[]`）于释锁返回 `Ok` 前双双落盘，且全新空通道已预建初始 34B `LTM2` 双副本 | `src/internal/ai/capture/spool.rs:111-157`；`ADR-RB-05` #3 |

**Acceptance criteria:**
- [x] 32 并发进程各写入 100 条变长帧（64 B – 64 KiB），全部 3,200 帧 CRC32C 100% 通过且零交错损坏。
- [x] 每次入队在共享 `append.lock` 内执行单次 `write_all` 并完成 `seg.sync_data()` 刷盘。
- [x] 每次入队通过 `tail.meta.tmp` 原子替换并 `fsync_dir(lane_dir)` 提交主副本 `LTM2` `tail.meta`。
- [x] 每次入队在释放 `append.lock` 前通过 `tail.meta.bak.tmp` 原子替换并 `fsync_dir(lane_dir)` 提交镜像副本 `tail.meta.bak`（双副本均有效时按 `(active_seg, committed_len, min_live_seg)` 全序收敛）。
- [x] 故障注入（Case C 同段内于 `seg.sync_data()` 后、元数据提交前崩溃，含全新空通道首帧崩溃且 `[0, committed_len)` 前缀完好）：恢复前向扫描 100% 保留已 `sync_data` 合法帧。
- [x] 上述 Case C 恢复保留已同步合法帧后，原子推进 `tail.meta` 与 `tail.meta.bak` 双副本至新提交边界。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::multiprocess_32_producers_zero_interleave_and_dual_tail_meta) | test(=storage::queue::segment::tests::case_c_same_segment_crash_retains_synced_frames)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::multiprocess_32_producers_zero_interleave_and_dual_tail_meta)'`（new，覆盖 AC1–AC4）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_same_segment_crash_retains_synced_frames)'`（new，覆盖 AC5、AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-07e`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue LTM2 dual tail-meta and case-c; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=RB-07e; writeset=序列化于 RB-07e; release=independent; split-from=RB-07; exception=N/A`

---

### Task RB-08t: Capture 队列断电持久隔离发布强约束原语（先 `.tmp → sync_data → rename → fsync_dir` 再 `truncate`/`unlink`）（Track A，自 `RB-08` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 实现并同卡验收供下游全部恢复路径（`RB-08u` Case D 有效后继半帧尾、`RB-08f` Case D 半帧孤儿、`RB-08m` Case C Step C1 与 `RB-08v` 嵌套 Case C→D 旧段坏尾）统一调用的断电持久隔离取证发布强约束原语：`publish_quarantine_durable(quarantine_dir, id, bytes)`、`quarantine_and_truncate_segment` 与 `quarantine_and_unlink_orphan`。三者按确定性键名 `quarantine/<lane>-seg<S>-off<offset>-<crc32c>.tmp`（复用 `RB-07b` `0600`/`O_NOFOLLOW` 文件打开原语）→ `tmp_file.sync_data()` → 原子 `fs::rename(.tmp -> .bin)` → `fsync_dir(quarantine_dir)` 严格持久化隔离副本，从结构上强制保证上述四步全部完成后方可执行后续 `truncate(events-<S>)` 或 `unlink(orphan_seg)`，在合法可恢复通道第二阶段仅清理本通道遗留 `quarantine/<lane>-*.tmp`（绝不触碰另一通道临时文件），并以顺序插桩与两处断电边界（重命名前崩溃、目录刷盘后截断/删段前崩溃）完成 8 项独立单谓词验收。

**Out of scope:**
- Case D 洁净旧段与洁净有效后继基础推进：由下游 `RB-08d` 承接；有效后继 `S+1` 含未提交半帧尾的持久隔离与截断分支：由下游 `RB-08u` 调用本原语完成。
- Case D 空/半帧孤儿段清理与双副本同步：由下游 `RB-08f` 调用本原语完成。
- Case C Step C1 原地截断刷盘与嵌套 Case C→D 旧段坏尾分支：分别由下游 `RB-08m`/`RB-08i` 与 `RB-08v` 调用本原语完成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 若拷贝隔离副本后未先完成 `tmp.sync_data()` → `rename(.bin)` → `fsync_dir(quarantine_dir)` 即执行 `truncate`/`unlink`，断电可保留截断/删段却丢失隔离取证字节或目录项 | `ADR-RB-05` #3 Case C Step C1、Case D |

**Acceptance criteria:**
- [x] **临时文件 `sync_data()` 严格先于 `rename`：** 顺序插桩断言 `publish_quarantine_durable` 对 `quarantine/<id>.tmp`（复用 `RB-07b` `0600`/`O_NOFOLLOW` 打开原语）执行 `tmp_file.sync_data()` 的完成序号严格小于 `fs::rename(.tmp -> .bin)` 的开始序号。
- [x] **`fsync_dir(quarantine_dir)` 紧接 `rename` 执行：** 顺序插桩断言 `fs::rename(.tmp -> .bin)` 的完成序号严格小于 `fsync_dir(quarantine_dir)` 的开始序号。
- [x] **发布后 `.bin` 字节逐字节相等：** `publish_quarantine_durable` 返回 `Ok` 后，`quarantine/<id>.bin` 完整字节切片精确等于受损残字节（`actual_quar_bytes == damaged_bytes`）。
- [x] **`quarantine_and_truncate_segment` 有序屏障：** 顺序插桩断言 `fsync_dir(quarantine_dir)` 的完成序号严格小于后续 `truncate(events-<S>)` 的开始序号。
- [x] **`quarantine_and_unlink_orphan` 有序屏障：** 顺序插桩断言 `fsync_dir(quarantine_dir)` 的完成序号严格小于后续 `unlink(orphan_seg)` 的开始序号。
- [x] **合法可恢复重命名前崩溃清理本通道遗留 `.tmp`：** 故障注入崩溃于 `rename` 之前，在合法可恢复通道 `<lane>` 重启通过只读分类后自动清理本通道遗留 `quarantine/<lane>-*.tmp`（损坏态与跨通道另一通道 `.tmp` 留存由 `RB-08c`/`RB-08y`/`RB-08z`/`RB-08e`/`RB-08h`/`RB-08x` 独立验收）。
- [x] **重命名前崩溃原段逐字节未损：** 上述 AC6 重命名前崩溃后，原段文件完整字节切片与崩溃前逐字节相等（零提前截断或删段）。
- [x] **目录刷盘后截断/删段前崩溃 `.bin` 持久留存：** 故障注入崩溃于 `fsync_dir(quarantine_dir)` 之后、破坏性 `truncate`/`unlink` 之前，重开通道确认 `quarantine/<id>.bin` 完整字节切片精确等于原受损残字节。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::publish_quarantine_durable_syncs_renames_fsyncs_before_destructive_ops) | test(=storage::queue::segment::tests::publish_quarantine_durable_crash_before_rename_and_after_dir_fsync)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::publish_quarantine_durable_syncs_renames_fsyncs_before_destructive_ops)'`（new，覆盖 AC1–AC5）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::publish_quarantine_durable_crash_before_rename_and_after_dir_fsync)'`（new，覆盖 AC6–AC8）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue durable quarantine before truncate unlink; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=RB-08; writeset=序列化于 RB-08; release=independent; split-from=RB-08; exception=N/A`

---

### Task RB-08d: Capture 队列基础 Case D 洁净旧段先验护卫与洁净有效后继段推进（Track A，自 `RB-08` 拆出，拆出 `RB-08u`、`RB-08f`、`RB-08k`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 实现 Case D 轮转孤儿后继段（`max_seg > active_seg`）在**洁净旧段（`actual_len(S) == committed_len`）+ 洁净有效后继段 `S+1`** 下的基础生产逻辑：旧段 `active_seg` 短段先验拒收、旧段 `prefix_crc32c` 位翻转先验拒收、保留后继段 `events-<S+1>` 全部已同步合法帧、将洁净旧段 `(S, committed_len, prefix_crc32c)` 原子封存至 `LTM2.sealed[]`，并将双副本 `LTM2` 推进至 `(ACTIVE_SEG = S + 1, COMMITTED_LEN = succ_len, PREFIX_CRC32C = succ_crc)`（**本卡不包含有效后继半帧尾隔离截断，由 `RB-08u` 新增；亦不包含 `actual_len(S) > committed_len` 的嵌套 Case C→D 旧段尾恢复分支，该分支待 `RB-08m` 实现 `case_c_step_c1` 后由 `RB-08k` 独占新增**）。

**Out of scope:**
- Case D 有效后继 `S+1` 合法帧后接未提交半帧尾的持久隔离与截断分支：已按 `G-03`/`G-09` 拆出由 `RB-08u` 承接。
- Case D 空孤儿段与半帧孤儿段清理、隔离、目录刷盘与双副本同步：已按 `G-03`/`G-09` 拆出由 `RB-08f` 承接。
- 嵌套 Case C→D（`max_seg > active_seg && actual_len(S) > committed_len`）调用 `RB-08m::case_c_step_c1` 先恢复旧段未提交尾再封存旧段生产分支：待 `RB-08m` 落地后由 `RB-08k` 独占实现与验收；其真实消费端排空后置于 `RB-09i`。
- 跨通道 `256 MiB` 容量上限、16 MiB 轮转、超时与 `256` 封存表全链路：由 `RB-08b`、`RB-08g`、`RB-08j`..`RB-08s` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Case D 轮转孤儿段必须先校验旧段长度与前缀校验和，在洁净旧段与洁净有效后继下完整保留后继合法帧、封存旧段并原子推进双副本 `LTM2` | `ADR-RB-05` #3 Case D |

**Acceptance criteria:**
- [x] **Case D 旧段短段先验拒收：** 当 `max_seg > active_seg` 且旧段 `active_seg` 被外部截短至 `actual_len < committed_len` 时，恢复在触碰任何后继段前立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **Case D 旧段前缀位翻转先验拒收：** 当 `max_seg > active_seg` 且旧段 `active_seg` 在 `[0, committed_len)` 内存在单字节翻转致 `prefix_crc32c` 失配时，恢复在触碰任何后继段前立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **Case D 洁净有效后继帧保留：** 正常轮转（洁净旧段 `actual_len(S) == committed_len`）创建洁净新段 `events-<S+1>` 并完成 `sync_data()` + `fsync_dir` 含合法帧后、元数据提交前崩溃，恢复完整保留 100% 后继合法帧字节于磁盘。
- [x] **Case D 洁净旧段封存至 `LTM2.sealed[]`：** 在保留上述后继合法帧后，恢复将洁净旧段 `(S, committed_len, prefix_crc32c)` 原子写入 `LTM2.sealed[]`。
- [x] **Case D 推进双副本 `LTM2.ACTIVE_SEG`：** 在同一双副本 `LTM2` 提交中将 `tail.meta`/`tail.meta.bak` 的 `ACTIVE_SEG` 推进至 `S + 1`。
- [x] **Case D 推进双副本 `LTM2.COMMITTED_LEN`：** 在同一双副本 `LTM2` 提交中将 `COMMITTED_LEN` 更新为 `events-<S+1>` 合法帧总长度。
- [x] **Case D 推进双副本 `LTM2.PREFIX_CRC32C`：** 在同一双副本 `LTM2` 提交中将 `PREFIX_CRC32C` 更新为精确匹配 `events-<S+1>` 全字节的校验和。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_d_old_seg_prefix_guard_and_valid_successor_recovery)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_d_old_seg_prefix_guard_and_valid_successor_recovery)'`（new，覆盖 AC1–AC7）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08t`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue base case-d clean old-seg and valid succ; recovery=revert isolated module; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=2; prod-files=3; scope=M; deps=RB-08t; writeset=序列化于 RB-08t; release=independent; split-from=RB-08; exception=N/A`

---

### Task RB-08u: Capture 队列 Case D 有效后继段尾损半帧持久隔离、调用方顺序插桩与断电边界收敛（Track A，自 `RB-08d` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 扩展 Case D 有效后继恢复路径，新增**有效后继段 `events-<S+1>` 含 ≥ 1 条完整合法帧后接未提交半帧尾 `[succ_valid_end, actual_len(S+1))`** 的生产分支：在真实调用路径中显式经 `RB-08t::quarantine_and_truncate_segment` 先完成 `publish_quarantine_durable`（`.tmp → sync_data → rename(.bin) → fsync_dir(quarantine_dir)`）再截断 `events-<S+1>` 至精确 `succ_valid_end` 并 `sync_data()`，随后封存旧段并提交双副本 `LTM2`；同卡验证调用方真实顺序插桩及重命名前/目录刷盘后两处断电崩溃边界。

**Out of scope:**
- Case D 空孤儿段与纯半帧孤儿段清理、`unlink` 与双副本同步：由下游 `RB-08f` 承接。
- 嵌套 Case C→D 旧段未提交尾恢复分支（`actual_len(S) > committed_len`）：待 `RB-08m` 落地后由 `RB-08k` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-05` #3 Case D 要求对含合法帧的后继段，其后未提交半帧须先经 `publish_quarantine_durable` 完成目录刷盘后再截断并 `sync_data()`；必须直接在调用方路径验证顺序与两处崩溃边界以防绕过原语 | `ADR-RB-05` #3 Case D；`RB-08t` |

**Acceptance criteria:**
- [x] **有效后继合法帧逐字节保留：** 当 `events-<S+1>` 含 ≥ 1 合法帧后接未提交半帧尾时，恢复后磁盘 `events-<S+1>` 完整字节切片精确等于合法前缀 `[0..succ_valid_end)`。
- [x] **有效后继半帧尾持久隔离内容相等：** 恢复后 `quarantine/<id>.bin` 完整字节切片精确等于原后继段未提交半帧尾 `[succ_valid_end, actual_len(S+1))`。
- [x] **调用方顺序插桩（隔离落盘先于截断 `S+1`）：** 在 Case D 有效后继半帧恢复真实调用路径中，顺序插桩钩子断言 `publish_quarantine_durable` 的 `fsync_dir(quarantine_dir)` 完成序号严格小于 `truncate(events-<S+1>)` 的开始序号。
- [x] **重命名前崩溃后继段零提前截断：** 故障注入崩溃于后继段隔离 `rename` 之前，磁盘 `events-<S+1>` 完整字节切片与崩溃前逐字节一致（零提前截断）。
- [x] **目录刷盘后截断前崩溃 `.bin` 持久留存：** 故障注入崩溃于 `fsync_dir(quarantine_dir)` 之后、`truncate(events-<S+1>)` 之前，重开通道确认 `quarantine/<id>.bin` 完整字节切片精确等于原后继段半帧尾。
- [x] **崩溃重开收敛更新 `LTM2.COMMITTED_LEN`：** 上述崩溃重开恢复收敛后，双副本 `LTM2.COMMITTED_LEN` 精确等于 `succ_valid_end`。
- [x] **崩溃重开收敛更新 `LTM2.PREFIX_CRC32C`：** 上述崩溃重开恢复收敛后，双副本 `LTM2.PREFIX_CRC32C` 精确等于 `crc32c(events-<S+1>[0..succ_valid_end])`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_d_valid_successor_trailing_half_frame_durable_quarantine_ordering) | test(=storage::queue::segment::tests::case_d_valid_successor_trailing_half_frame_crash_boundaries)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_d_valid_successor_trailing_half_frame_durable_quarantine_ordering)'`（new，覆盖 AC1–AC3）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_d_valid_successor_trailing_half_frame_crash_boundaries)'`（new，覆盖 AC4–AC7）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=case-d valid successor trailing half-frame durable quarantine; recovery=revert isolated module; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=RB-08d; writeset=序列化于 RB-08d; release=independent; split-from=RB-08d; exception=N/A`

---

### Task RB-08f: Capture 队列 Case D 空孤儿段与半帧孤儿段逐动作清理、隔离、目录刷盘与双副本同步（Track A，自 `RB-08d` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 实现 Case D 轮转孤儿后继段（`max_seg > active_seg`）中空孤儿段与半帧孤儿段的全部 6 项独立原子清理动作：空孤儿段 `unlink`、空孤儿段 `fsync_dir`、半帧孤儿残字节移入 `quarantine/`、半帧受损孤儿段 `unlink`、半帧受损孤儿段 `fsync_dir`，以及双副本元数据同步至完好 `active_seg`。

**Out of scope:**
- 跨通道 `256 MiB` 容量上限、16 MiB 轮转、超时与未提交尾损预修：由 `RB-08b` 与 `RB-08g` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Case D 空孤儿段与半帧孤儿段的 `unlink`、目录 `fsync_dir`、残片隔离与双副本同步均为可独立失败的子动作，逐项独立验收确保 `G-03` 严格达标 | `ADR-RB-05` #3 Case D |

**Acceptance criteria:**
- [x] **Case D 空孤儿段摘除：** 轮转创建空的孤儿后继段 `events-<S+1>` 后崩溃，恢复执行 `unlink` 删除该空孤儿段文件。
- [x] **Case D 空孤儿段目录刷盘：** 删除上述空孤儿段后，恢复立即执行 `fsync_dir(lane_dir)` 确保删除持久化。
- [x] **Case D 半帧孤儿断电持久隔离：** 轮转创建仅含未提交半帧的孤儿后继段 `events-<S+1>` 后崩溃，恢复经 `RB-08t` `quarantine_and_unlink_orphan` 先调用 `publish_quarantine_durable` 将残字节断电持久发布至 `quarantine/`。
- [x] **Case D 半帧孤儿段摘除：** 在上述 `publish_quarantine_durable` 完成 `fsync_dir(quarantine_dir)` 之后，恢复执行 `unlink` 删除受损孤儿段 `events-<S+1>`。
- [x] **Case D 半帧孤儿段目录刷盘：** 删除上述受损孤儿段后，恢复立即执行 `fsync_dir(lane_dir)` 确保删除持久化。
- [x] **Case D 半帧孤儿后双副本同步：** 摘除上述受损孤儿段并刷盘后，恢复将 `tail.meta` 与 `tail.meta.bak` 原子同步至完好的 `active_seg`。
- [x] **Case D 半帧孤儿调用方顺序插桩：** 在半帧孤儿真实恢复调用路径中，顺序插桩钩子断言 `publish_quarantine_durable` 的 `fsync_dir(quarantine_dir)` 完成序号严格小于 `unlink(events-<S+1>)` 的开始序号。
- [x] **Case D 半帧孤儿重命名前崩溃零提前删段：** 故障注入崩溃于半帧孤儿隔离 `rename` 之前，断言 `fs::read(events-<S+1>)` 成功读回并与崩溃前字节切片逐字节相等。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_d_empty_and_half_frame_orphan_cleanup_quarantine)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_d_empty_and_half_frame_orphan_cleanup_quarantine)'`（new，覆盖 AC1–AC8）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08u`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue case-d orphan unlink fsync quarantine; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08u; writeset=序列化于 RB-08u; release=independent; split-from=RB-08d; exception=N/A`

---

### Task RB-08b: Capture 队列共享锁跨通道 256 MiB 容量边界、超时与延迟预算（Track A，自 `RB-08` 拆出，拆出 `RB-08g`、`RB-08c`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 补齐共享 `capture-queue/append.lock` 下的跨通道合并容量守恒（精确校验 `unconsumed_priority + unconsumed_normal + quarantine + frame_buf.len() <= 256 MiB`）、deadline 超时（`LBR-AGENT-049`）与单帧入队延迟预算门禁（含 Case A 硬件 `PREFIX_CRC32C` 核验）。

**Out of scope:**
- `16 MiB` 段轮转与未提交尾（`offset >= committed_len`）残帧入队前预修：已按 `G-03`/`G-09` 拆出由 `RB-08g` 承接。
- 正常入队前 Case A 位翻转、Case E 短于 `committed_len`、Subcase B1 单损与 Subcase B2 双副本同损无条件 fail-closed 守卫：由 `RB-08c` 与 `RB-08e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-05` 要求单帧与并发跨通道入队均不得穿透 `256 MiB` 合并上限，且含 `PREFIX_CRC32C` 核验的单帧中位延迟 ≤ `5ms` | `ADR-RB-05`；`GC-RB-03` |

**Acceptance criteria:**
- [x] 单帧加入后若 `unconsumed_priority + unconsumed_normal + quarantine + frame_buf.len() > 256 MiB`，立即拒绝入队并返回 `LBR-AGENT-046` 且零字节写入。
- [x] 并发边界测试：剩余容量仅够 1 帧时并发发起 `priority/` 与 `normal/` 生产者，至多 1 个成功、其余报 `LBR-AGENT-046`，总占用绝不超过上限 1 字节。
- [x] 共享 `append.lock` 等待超过调用方 deadline 返回 `LBR-AGENT-049` 且零字节写入。
- [x] 1 KiB 已脱敏帧入队（含 Case A `PREFIX_CRC32C` 校验与双副本提交）在本机 SSD 上中位数耗时 ≤ `5ms`（预算硬上限 `50ms`）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::cross_lane_capacity_boundary_race_and_single_frame_cap) | test(=storage::queue::segment::tests::append_lock_deadline_timeout_and_latency_budget)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::cross_lane_capacity_boundary_race_and_single_frame_cap)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::append_lock_deadline_timeout_and_latency_budget)'`（new，覆盖 AC3、AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08f`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue cross-lane 256mib cap timeout latency; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=RB-08f; writeset=序列化于 RB-08f; release=independent; split-from=RB-08; exception=N/A`

---

### Task RB-08g: Capture 队列基础 16 MiB 常规段轮转、共用 `create_segment_file` 原语与 `LTM2.sealed[]` 原子登记（Track A，自 `RB-08b` 拆出，拆出 `RB-08m`、`RB-08i`、`RB-08j`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 实现 `ADR-RB-05` #3 的基础 16 MiB 常规轮转路径与共用 `create_segment_file(lane_dir, next_seg)` 原语（建新段、强制 `0600` 权限并执行 `fsync_dir(lane_dir)`），于双副本 `LTM2` 原子提交中登记旧段 `(S, old_committed_len, old_prefix_crc32c)` 至 `LTM2.sealed[]`、推进 `ACTIVE_SEG = S + 1` 并从 `0` 重算新段 `PREFIX_CRC32C`，保证新旧段全部帧无损解码读回（`MAX_SEALED_SEGMENTS = 256` 封存表上界尚未在本卡引入，由 `RB-08j`..`RB-08o` 渐进交付）。

**Out of scope:**
- 两步 Case C 未提交尾损预修（Step C1 与基础 Step C2）：已拆出由 `RB-08m`、`RB-08n` 承接。
- Case C 幂等崩溃重入收敛、`recovered_valid_end == 0` 跳过 Step C2 守卫与段层嵌套 Case C→D 恢复：由 `RB-08i`、`RB-08q`、`RB-08k` 承接。
- `MAX_SEALED_SEGMENTS = 256` 封存表上界常量引入、常规轮转背压、Case D/解码器上界、满表 Case C 跳过 Step C2 与单调用恢复+拒收：由 `RB-08j`、`RB-08r`、`RB-08l`、`RB-08o`、`RB-08s` 渐进交付。
- 正常入队前 Case A 位翻转、Case E 短段拒收、0 段四分法与 Subcase B1/B2 守卫：由 `RB-08c`、`RB-08e` 与 `RB-08h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 活跃段超 `16 MiB` 须经共用 `create_segment_file`（`0600` + `fsync_dir`）建新段并在双副本 `LTM2` 原子登记旧段至 `LTM2.sealed[]` | `ADR-RB-05` #3 Step ③、④ |

**Acceptance criteria:**
- [x] **创建新段文件：** 活跃段超过 `16 MiB` 时调用共用 `create_segment_file` 在磁盘创建新段 `events-<S+1:08>.log`。
- [x] **新段精确 `0600` 权限：** `create_segment_file` 创建的 `events-<S+1:08>.log` 在 Unix 上具有精确 `0600` 权限。
- [x] **新段目录持久化：** `create_segment_file` 创建新段后立即执行 `fsync_dir(lane_dir)`。
- [x] **登记旧段至 `LTM2.sealed[]`：** 上述 16 MiB 轮转在提交双副本 `LTM2` 时原子将旧段 `(S, old_committed_len, old_prefix_crc32c)` 写入 `LTM2.sealed[]`。
- [x] **推进 `ACTIVE_SEG`：** 在同一双副本 `LTM2` 提交中将 `ACTIVE_SEG` 推进至 `S + 1`。
- [x] **重置新段 `PREFIX_CRC32C`：** 新段 `PREFIX_CRC32C` 从 `0` 起按新段首帧重算并与 `events-<S+1>[0..committed_len]` 一致。
- [x] **旧段帧无损解码：** 轮转完成后，旧段 `S` 全部旧帧 100% 无损解码读回。
- [x] **新段帧无损解码：** 轮转完成后，新段 `S+1` 全部新帧 100% 无损解码读回。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::rotation_16mib_creates_0600_fsync_and_records_ltm2_sealed)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::rotation_16mib_creates_0600_fsync_and_records_ltm2_sealed)'`（new，覆盖 AC1–AC8）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue base 16mib rotation and create_segment_file; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08b; writeset=序列化于 RB-08b; release=independent; split-from=RB-08b; exception=N/A`

---

### Task RB-08m: Capture 队列 Case C Step C1 未提交尾损隔离、截断刷盘、`LTM2` 边界先落盘与段 `S` 帧无损留存（Track A，自 `RB-08g` 拆出，拆出 `RB-08n`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 实现并同卡验收 `ADR-RB-05` #3 两步 Case C 恢复的 **Step C1**（共用 `case_c_step_c1` 生产例程）：已确认前缀 `[0, committed_len)` 校验通过且段尾存在未提交半帧时，经 `RB-08t` 强约束原语 `quarantine_and_truncate_segment` 先调用 `publish_quarantine_durable` 将 `[recovered_valid_end, actual_len)` 断电持久发布至 `quarantine/` 并记录 `LBR-AGENT-050`，再将 `events-<S>.log` 截断至精确 `recovered_valid_end` 并执行 `sync_data()`，在建任何 `S+1` 前先原子提交双副本 `LTM2` 固定 `COMMITTED_LEN` 与 `PREFIX_CRC32C`，并同卡验证段 `S` 中前缀旧帧与完整同步尾帧均 100% 无损解码留存。

**Out of scope:**
- Case C Step C2 条件创建新段 `S+1`、封存 `LTM2.sealed[]` 及新段追加读回：已拆出由 `RB-08n` 承接。
- Case C 幂等崩溃重入收敛、0 字节首帧跳过 Step C2 守卫与嵌套 Case C→D 段层恢复：由 `RB-08i`、`RB-08q`、`RB-08k` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Case C 必须在建 `S+1` 前先完成 Step C1 隔离、截断、`sync_data()` 与双副本 `LTM2` 边界提交，同时保证段 `S` 前缀旧帧与完整同步尾帧无损保留 | `ADR-RB-05` #3 Case C Step C1 |

**Acceptance criteria:**
- [x] **Step C1 残尾断电持久隔离：** 有效 `LTM2` 且 `[0, committed_len)` 校验通过、尾段存在截断半帧时，经 `RB-08t` `quarantine_and_truncate_segment` 先调用 `publish_quarantine_durable` 将 `[recovered_valid_end, actual_len)` 断电持久发布至 `quarantine/`。
- [x] **Step C1 诊断码：** 上述未提交残尾隔离精确记录诊断码 `LBR-AGENT-050`。
- [x] **Step C1 原地截断：** 在上述 `publish_quarantine_durable` 完成 `fsync_dir(quarantine_dir)` 之后，将 `events-<S>.log` 文件长度截断至精确 `recovered_valid_end`。
- [x] **Step C1 截断后刷盘：** 截断完成后对 `events-<S>.log` 执行 `sync_data()`。
- [x] **Step C1 先提交 `LTM2.COMMITTED_LEN`：** 在创建任何 `S+1` 段文件之前，原子提交双副本 `LTM2` 将 `COMMITTED_LEN` 更新为 `recovered_valid_end`。
- [x] **Step C1 更新 `LTM2.PREFIX_CRC32C`：** 在同一 Step C1 `LTM2` 提交中将 `PREFIX_CRC32C` 更新为精确匹配 `events-<S>.log[0..recovered_valid_end]` 的 `recovered_crc32c`。
- [x] **段 `S` 前缀旧帧无损解码：** Step C1 完成后，段 `S` `[0, old_committed_len)` 内全部崩溃前已确认旧帧 100% 无损解码读回。
- [x] **段 `S` 完整同步尾帧无损解码：** Step C1 完成后，段 `S` `[old_committed_len, recovered_valid_end)` 内全部恢复的完整同步尾帧 100% 无损解码读回。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_step_c1_quarantines_truncates_syncs_commits_ltm2_and_preserves_s)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_step_c1_quarantines_truncates_syncs_commits_ltm2_and_preserves_s)'`（new，覆盖 AC1–AC8）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08g`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue case-c step-c1 repair and s-frame retention; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=3; scope=M; deps=RB-08g; writeset=序列化于 RB-08g; release=independent; split-from=RB-08g; exception=N/A`

---

### Task RB-08n: Capture 队列 Case C Step C2 基础封存切段与新段 `S+1` 追加读回（Track A，自 `RB-08m` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 实现并同卡验收 Case C 恢复的基础 **Step C2** 路径（`seal_recovered_segment_to_next`，在 `RB-08m` Step C1 截断刷盘并提交 `LTM2` 之后对常规非空未满表段执行无条件封存切段）：复用 `RB-08g` 已落地的共用 `create_segment_file(lane_dir, S + 1)` 原语创建新段（继承其已验 `0600` 与 `fsync_dir` 契约），随后原子提交双副本 `LTM2` 封存旧段至 `LTM2.sealed[]`、推进 `ACTIVE_SEG = S + 1` 并分别归零新段 `COMMITTED_LEN` 与 `PREFIX_CRC32C`，同卡验证向 `S+1` 追加新帧返回 `Ok(())` 且无损读回（`recovered_valid_end == 0` 跳过守卫尚未在本卡引入，由 `RB-08q` 新增；`MAX_SEALED_SEGMENTS = 256` 跳过守卫尚未在本卡引入，由 `RB-08l` 新增）。

**Out of scope:**
- Case C 幂等崩溃重入收敛（边界 1–3）：由 `RB-08i` 新增。
- `recovered_valid_end == 0` 首帧 0 字节崩溃跳过 Step C2 守卫分支：由 `RB-08q` 新增。
- 嵌套 Case C→D 旧段尾恢复：由 `RB-08k` 新增。
- `MAX_SEALED_SEGMENTS = 256` 上界常量引入、洁净 Case A 轮转背压、Case D/解码器上界与 Case C 满 256 表跳过 Step C2 守卫分支：由 `RB-08j`、`RB-08r`、`RB-08l`、`RB-08o`、`RB-08s` 渐进新增。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Step C2 在 Step C1 边界持久化后复用 `create_segment_file` 创建新段、写入 `LTM2.sealed[]` 并初始化新活跃段 `S+1` 供后续追加读回 | `ADR-RB-05` #3 Case C Step C2 |

**Acceptance criteria:**
- [x] **Step C2 复用 `create_segment_file` 创建新段：** 在常规非空段完成 Step C1 截断后，Step C2 调用共用 `create_segment_file(lane_dir, S + 1)` 在磁盘生成 `events-<S+1:08>.log`（`0600` 权限与 `fsync_dir` 契约已在 `RB-08g` 共用原语独立验收）。
- [x] **Step C2 封存旧段至 `LTM2.sealed[]`：** 原子提交双副本 `LTM2` 将 `(S, recovered_valid_end, recovered_crc32c)` 写入 `LTM2.sealed[]`。
- [x] **Step C2 推进 `ACTIVE_SEG`：** 在同一双副本 `LTM2` 提交中将 `LTM2.ACTIVE_SEG` 更新为 `S + 1`。
- [x] **Step C2 归零新段 `COMMITTED_LEN`：** 在同一双副本 `LTM2` 提交中将新空段 `S+1` 的 `LTM2.COMMITTED_LEN` 设为 `0`。
- [x] **Step C2 归零新段 `PREFIX_CRC32C`：** 在同一双副本 `LTM2` 提交中将新空段 `S+1` 的 `LTM2.PREFIX_CRC32C` 设为 `0`。
- [x] **新段 `S+1` 后续追加返回 `Ok`：** Step C2 完成后向新活跃段 `S+1` 追加有效新帧返回 `Ok(())`。
- [x] **新段 `S+1` 新帧无损解码：** 上述新追加至 `S+1` 的帧经段层解码器 100% 无损解码读回。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_step_c2_seals_rotates_resets_ltm2_and_appends_s1)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_step_c2_seals_rotates_resets_ltm2_and_appends_s1)'`（new，覆盖 AC1–AC7）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08m`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue base case-c step-c2 seal and s1 append; recovery=revert isolated module; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=2; prod-files=3; scope=M; deps=RB-08m; writeset=序列化于 RB-08m; release=independent; split-from=RB-08m; exception=N/A`

---

### Task RB-08i: Case C 幂等崩溃重入、纯已同步尾收敛与 Step C1 后空后继收敛实现及注入验收（Track A，自 `RB-08g` 拆出，拆出 `RB-08q`、`RB-08k`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 实现并同卡通过确定性故障注入验收两步 Case C 恢复的三条崩溃重入生产路径：① 幂等隔离命名与重入（边界 1：隔离后截断前崩溃重入绝不因既有隔离文件冲突或二次截断丢帧）；② 纯已同步尾洁净收敛分支（边界 2：`recovered_valid_end == actual_len` 零隔离直接推进 `LTM2.COMMITTED_LEN` 并保持 `ACTIVE_SEG == S`）；③ Step C1 后空后继清理收敛分支（边界 3：Case D 删除空 `S+1` 并保持旧段认证边界）。

**Out of scope:**
- 全新段首帧半截崩溃（`recovered_valid_end == 0`）截断回 0 字节且跳过 Step C2 不封存：已按 `G-03`/`G-09` 拆出由 `RB-08q` 承接。
- 显式嵌套 Case C→D（旧 `LTM2` + 增长旧段 `S` + 含有效同步帧 `S+1`）的旧段尾恢复与封存：由 `RB-08k` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 两步 Case C 恢复中间任意单点崩溃（边界 1/2/3）均须幂等收敛且精准保留已恢复边界 | `ADR-RB-05` #3 Case C |

**Acceptance criteria:**
- [x] **Case C 边界 1（隔离后、`truncate(S)` 前崩溃）：** 重启幂等重跑 Case C，100% 保留段 `S` 全部完整同步帧。
- [x] **Case C 边界 2 推进 `COMMITTED_LEN`（`truncate(S)+sync_data()` 后、Step C1 `LTM2` 前崩溃）：** 重启检出 `S` 洁净至 `recovered_valid_end` 并推进 `LTM2.COMMITTED_LEN = recovered_valid_end`。
- [x] **Case C 边界 2 保持 `ACTIVE_SEG == S`：** 上述边界 2 恢复完成后 `LTM2.ACTIVE_SEG` 保持 `S`。
- [x] **Case C 边界 3 删空后继（Step C1 后建空 `S+1`、Step C2 前崩溃）：** Case D 执行 `unlink` 删除空后继文件 `events-<S+1:08>.log`。
- [x] **Case C 边界 3 保持 `ACTIVE_SEG == S`：** 上述边界 3 恢复完成后 `LTM2.ACTIVE_SEG` 保持 `S`。
- [x] **Case C 边界 3 保持 `COMMITTED_LEN`：** 上述边界 3 恢复完成后 `LTM2.COMMITTED_LEN` 保持已认证的 `recovered_valid_end`。
- [x] **Case C Step C1 调用方顺序插桩：** 在 `case_c_step_c1` 真实尾损恢复路径中，顺序插桩钩子断言 `publish_quarantine_durable` 的 `fsync_dir(quarantine_dir)` 完成序号严格小于 `truncate(events-<S>)` 的开始序号。
- [x] **Case C Step C1 重命名前崩溃原段零提前截断：** 故障注入崩溃于 `case_c_step_c1` 隔离 `rename` 之前，确认 `events-<S>.log` 完整字节切片与崩溃前逐字节一致（零提前截断）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_recovery_crash_boundaries_1_2_3_converge)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_recovery_crash_boundaries_1_2_3_converge)'`（new，覆盖 AC1–AC8）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08n`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=case-c crash boundaries 1-3 deterministic injection; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08n; writeset=序列化于 RB-08n; release=independent; split-from=RB-08g; exception=N/A`

---

### Task RB-08q: 全新段首帧 0 字节崩溃（`recovered_valid_end == 0`）跳过 Step C2 守卫实现与验收（Track A，自 `RB-08i` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs`（纯段层，零依赖消费者）在 `RB-08n` 基础无条件 Step C2（`seal_recovered_segment_to_next`）之前**首次新增** `if recovered_valid_end == 0` 条件跳过守卫分支并同卡验收：当全新段首帧半截崩溃致有效尾长为 0 时，执行 Step C1 隔离与截断回 0 字节后提前返回，跳过 `seal_recovered_segment_to_next` 以保持 `LTM2.ACTIVE_SEG == S` 且绝不向 `LTM2.sealed[]` 写入 0 字节封存段。

**Out of scope:**
- 0 字节恢复后消费者高水位保持 `(S, 0)` 与后续追加无缝消费：由 `RB-09g` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `recovered_valid_end == 0` 时若执行 Step C2 将产生 0 字节封存段并浪费 `sealed[]` 槽位，必须严格停在 Step C1 | `ADR-RB-05` #3 Case C Step C2 |

**Acceptance criteria:**
- [x] **0 字节首帧残字节隔离：** 当首帧半截崩溃致 `recovered_valid_end == 0` 时，Step C1 将 `[0, actual_len)` 残字节拷贝隔离至 `quarantine/`。
- [x] **0 字节首帧段截断回 0：** Step C1 将 `events-<S>.log` 截断至精确 0 字节。
- [x] **0 字节首帧跳过 Step C2 保持 `ACTIVE_SEG == S`：** 恢复跳过 Step C2，`LTM2.ACTIVE_SEG` 严格保持 `S`。
- [x] **0 字节首帧零新增封存项：** 恢复完成后 `LTM2.sealed.len()` 严格保持原值（绝不产生 0 字节封存段）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_zero_byte_first_frame_crash_truncates_without_sealing)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_zero_byte_first_frame_crash_truncates_without_sealing)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08i`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=case-c zero-byte first-frame crash no-seal guard; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08i; writeset=序列化于 RB-08i; release=independent; split-from=RB-08i; exception=N/A`

---

### Task RB-08k: 嵌套 Case C→D 旧段洁净已同步尾恢复分支与真实边界封存（Track A，自 `RB-08d` 拆出，拆出 `RB-08v`、`RB-09i`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs`（纯生产者/段层，零依赖尚未实现的消费端 `writer.rs`）**首次独占实现** Case D 嵌套旧段尾恢复生产分支中的**旧段洁净已同步尾路径**（`max_seg > active_seg && old_valid_end == actual_len(S) > committed_len`，此前 `RB-08d` 仅处理 `actual_len(S) == committed_len`）：先验证旧段 `[0, committed_len)` 前缀校验和，再前向扫描 `[committed_len, actual_len(S))` 认证完整已同步尾至 `old_valid_end` 并以真实边界 `(S, old_valid_end, old_sealed_crc)` 封存旧段 `S` 至 `LTM2.sealed[]`，同步推进双副本 `LTM2` 至有效后继段 `S+1`（嵌套旧段含损坏半帧尾的持久隔离与截断分支拆出由下游 `RB-08v` 独占承接）。

**Out of scope:**
- 洁净旧段（`actual_len(S) == committed_len`）基础 Case D 与有效后继半帧尾恢复：分别已由 `RB-08d` 与 `RB-08u` 承接。
- 嵌套 Case C→D 旧段含损坏半帧尾（`old_valid_end < actual_len(S)`）的持久隔离、调用方顺序插桩、截断与重命名前崩溃验证：已按 `G-03`/`G-09` 拆出由下游 `RB-08v` 独占承接。
- 嵌套 Case C→D 恢复后的真实消费者 `LTM2.sealed[S]` 校验与 `capture.redb` 逐类帧排空：后置于 `RB-09i` 承接。
- `LTM2.sealed[]` `256` 封存表全链路（含满表有效后继叠加嵌套旧段尾的 Phase 1 只读整盘零改动拒收）：由 `RB-08j`、`RB-08r`、`RB-08l`、`RB-08o`、`RB-08s` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 嵌套态下旧段 `S` 若已增长出完整已同步尾帧而未先认证恢复，Case D 用旧 `committed_len` 封存 `S` 将导致 `LTM2.sealed[S]` 记录过期边界并阻断排空 | `ADR-RB-05` #3 Case D |

**Acceptance criteria:**
- [x] **嵌套态旧段前缀先验拒收：** 在嵌套态（`actual_len(S) > committed_len`、存在有效 `S+1`）翻转旧段 `S` `[0, committed_len)` 内单字节，Case D 在触碰 `S+1` 前 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **以真实增长边界封存旧段至 `LTM2.sealed[]`：** 旧段 `S` 含完整已同步尾（`old_valid_end == actual_len(S) > committed_len`）时，双副本 `LTM2.sealed[]` 以精确 `(S, old_valid_end, old_sealed_crc)` 封存旧段 `S`（绝不用旧 `committed_len`）。
- [x] **推进 `ACTIVE_SEG = S + 1`：** 双副本 `LTM2.ACTIVE_SEG` 推进至 `S + 1`。
- [x] **写入后继段 `COMMITTED_LEN`：** 双副本 `LTM2.COMMITTED_LEN` 精确等于 `events-<S+1>` 有效帧总字节长度。
- [x] **写入后继段 `PREFIX_CRC32C`：** 双副本 `LTM2.PREFIX_CRC32C` 精确等于 `crc32c(events-<S+1>[0..committed_len])`。
- [x] **后继段 `S+1` 完整字节原样保留：** 恢复后磁盘 `events-<S+1>` 完整字节切片精确等于原已同步有效帧字节（`actual_s1_bytes == expected_s1_bytes`；段层解码契约复用 `RB-07`/`RB-08g` 已验解码器）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::nested_case_c_to_case_d_recovers_old_tail_and_seals_accurate_boundary)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::nested_case_c_to_case_d_recovers_old_tail_and_seals_accurate_boundary)'`（new，覆盖 AC1–AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08q`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=segment-layer nested case c-to-d clean old-tail seal; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=2; prod-files=3; scope=M; deps=RB-08q; writeset=序列化于 RB-08q; release=independent; split-from=RB-08d; exception=N/A`

---

### Task RB-08v: 嵌套 Case C→D 旧段损坏半帧尾（`committed_len <= old_valid_end < actual_len(S)`）持久隔离、调用方顺序插桩与截断封存（Track A，自 `RB-08k` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs`（纯生产者/段层，零依赖消费者）扩展 `RB-08k` 的嵌套 Case C→D 旧段尾恢复路径，完整覆盖**旧段 `S` 含未提交坏尾的全部区间 `committed_len <= old_valid_end < actual_len(S)`**——同时包含（a）前接完整未提交同步尾帧的坏尾（`committed_len < old_valid_end < actual_len(S)`）与（b）紧接已确认边界即出现坏尾、零完整未提交帧的等号边界（`old_valid_end == committed_len < actual_len(S)`）：经 `RB-08m::case_c_step_c1` 调用 `RB-08t::quarantine_and_truncate_segment` 先完成 `publish_quarantine_durable`（含 `fsync_dir(quarantine_dir)`）再将 `events-<S>.log` 原地截断至精确 `old_valid_end` 并 `sync_data()`，随后以真实边界 `(S, old_valid_end, old_sealed_crc)` 封存旧段至 `LTM2.sealed[]`；同卡逐项完成 8 条独立单谓词验收。

**Out of scope:**
- 嵌套 Case C→D 旧段洁净已同步尾（`old_valid_end == actual_len(S) > committed_len`）与后继段 `S+1` 推进：已由 `RB-08k` 承接。
- 满 256 封存表下叠加嵌套旧段坏尾与有效后继段的 Phase 1 只读整盘零改动拒收：由下游 `RB-08r` 承接。
- 嵌套 Case C→D 恢复后的真实消费者全量排空：后置于 `RB-09i` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 嵌套态旧段 `S` 在 `committed_len < old_valid_end < actual_len(S)` 与等号边界 `old_valid_end == committed_len < actual_len(S)` 下均可出现未提交坏尾，截断前均须先经 `publish_quarantine_durable` 目录刷盘再截断并按真实边界封存旧段 | `ADR-RB-05` #3 Case D；`RB-08t`；`RB-08m` |

**Acceptance criteria:**
- [x] **前接同步尾坏尾（`committed_len < old_valid_end < actual_len(S)`）持久隔离字节相等：** 当嵌套态旧段 `S` 含完整未提交尾帧后接坏尾时，`quarantine/<id>.bin` 完整字节切片精确等于原坏尾 `[old_valid_end, actual_len(S))`。
- [x] **前接同步尾坏尾旧段精确截断至 `old_valid_end`：** 上述 AC1 场景恢复后，磁盘 `events-<S>.log` 完整字节切片精确等于 `[0..old_valid_end)`。
- [x] **前接同步尾坏尾以 `(S, old_valid_end, old_sealed_crc)` 封存：** 上述 AC1 场景收敛后，双副本 `LTM2.sealed[]` 以精确 `(S, old_valid_end, old_sealed_crc)` 封存旧段 `S`。
- [x] **紧接已确认边界坏尾（`old_valid_end == committed_len < actual_len(S)`）持久隔离字节相等：** 当嵌套态旧段 `S` 紧接 `committed_len` 即为未提交坏尾（零完整未提交帧）时，`quarantine/<id>.bin` 完整字节切片精确等于原坏尾 `[committed_len, actual_len(S))`。
- [x] **紧接已确认边界坏尾旧段截断回精确 `committed_len`：** 上述 AC4 场景恢复后，磁盘 `events-<S>.log` 完整字节切片精确等于原已确认前缀 `[0..committed_len)`。
- [x] **紧接已确认边界坏尾以 `(S, committed_len, prefix_crc32c)` 封存：** 上述 AC4 场景收敛后，双副本 `LTM2.sealed[]` 以精确 `(S, committed_len, prefix_crc32c)` 封存旧段 `S`。
- [x] **嵌套旧段坏尾调用方顺序插桩：** 在 `committed_len <= old_valid_end < actual_len(S)` 真实恢复路径中，顺序插桩钩子断言每次 `publish_quarantine_durable` 的 `fsync_dir(quarantine_dir)` 完成序号严格小于对应 `truncate(events-<S>)` 的开始序号。
- [x] **嵌套旧段坏尾重命名前崩溃零提前截断：** 故障注入崩溃于旧段坏尾隔离 `rename` 之前，断言 `fs::read(events-<S>.log)` 完整字节切片与崩溃前逐字节相等（零提前截断）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::nested_case_c_to_case_d_old_seg_damaged_tail_durable_quarantine_and_seal) | test(=storage::queue::segment::tests::nested_case_c_to_case_d_old_seg_bad_tail_at_committed_boundary_quarantine_and_seal)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::nested_case_c_to_case_d_old_seg_damaged_tail_durable_quarantine_and_seal)'`（new，覆盖 AC1–AC3、AC7、AC8）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::nested_case_c_to_case_d_old_seg_bad_tail_at_committed_boundary_quarantine_and_seal)'`（new，覆盖 AC4–AC7）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08k`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=segment-layer nested case c-to-d damaged old-tail quarantine; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=RB-08k; writeset=序列化于 RB-08k; release=independent; split-from=RB-08k; exception=N/A`

---

### Task RB-08j: `LTM2.sealed[]` 256 项段层常规轮转预变更背压与槽位释放恢复（Track A，自 `RB-08g` 拆出，拆出 `RB-08r`、`RB-08l`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs`（纯生产者/段层，零依赖尚未实现的消费端 `writer.rs`）**首次引入 `MAX_SEALED_SEGMENTS = 256` 常量**、常规 16 MiB 轮转新帧预写背压守卫（`ADR-RB-05` #3 Step ② (b)）与段层 `reclaim_sealed_segment` 原语：在**本已洁净的 Case A 满表通道**（`actual_len == committed_len`、`sealed.len() == 256`）上，触发 16 MiB 轮转的新帧于修改任何磁盘文件前返回 `LBR-AGENT-046`（逐文件验证零新建 `S+1`、`events-<S>`/`tail.meta`/`tail.meta.bak` 全调用零改动），调用 `reclaim_sealed_segment` 释放槽位后立即恢复建段与登记 `LTM2.sealed[]`。

**Out of scope:**
- Case D 满表有效后继段拒收与 `LTM2` 解码器上界拒收：已按 `G-03`/`G-09` 拆出由 `RB-08r` 承接。
- 满 256 封存表下 Case C 跳过 Step C2（`RB-08l`）、恢复后段内准入 `<= 16 MiB`（`RB-08o`）与单次调用先恢复再拒收屏障（`RB-08s`）：由后续对应卡片承接。
- 真实消费者循环排空并自动回收解除 `256` 项背压的端到端验收：后置于 `RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 常规 16 MiB 轮转必须在建 `S+1` 或修改任何段/元数据前强制 `sealed.len() < 256` 背压并在释放槽位后自动恢复 | `ADR-RB-05` #3 Step ② |

**Acceptance criteria:**
- [x] **总容量 ≤ `256 MiB` 条件隔离断言：** 以 256×4 KiB 小封存段（约 1 MiB）加近 16 MiB 活跃段（另一通道为 0）构造洁净 Case A 夹具，断言 `unconsumed_priority + unconsumed_normal + quarantine + frame_buf.len() <= 256 MiB`（严格排除全局字节上限 Step ② (a) 抢先触发）。
- [x] **洁净 Case A 常规轮转 `256 → 257` 返回 `LBR-AGENT-046`：** 在上述洁净 Case A 满表通道（`sealed.len() == 256`）上，下一入队触发 16 MiB 轮转时返回可恢复背压错误 `LBR-AGENT-046`。
- [x] **预变更背压零新建 `S+1` 文件：** 上述 AC2 拒收后，磁盘目录中未创建任何 `events-<S+1:08>.log` 文件。
- [x] **预变更背压既有活跃段零改动：** 上述 AC2 拒收后，既有 `events-<S>.log` 字节内容与拒收前逐字节一致。
- [x] **预变更背压主副本 `tail.meta` 零改动：** 上述 AC2 拒收后，既有 `tail.meta` 字节内容与拒收前逐字节一致。
- [x] **预变更背压副副本 `tail.meta.bak` 零改动：** 上述 AC2 拒收后，既有 `tail.meta.bak` 字节内容与拒收前逐字节一致。
- [x] **槽位释放后恢复创建 `S+1`：** 调用 `segment::reclaim_sealed_segment` 释放 ≥ 1 个已封存段使 `sealed.len() < 256` 后，同一触发轮转的新入队成功创建 `events-<S+1:08>.log`。
- [x] **恢复轮转登记 `LTM2.sealed[]`：** 上述 AC7 轮转提交双副本 `LTM2` 将旧段 `(S, old_committed_len, old_prefix_crc32c)` 原子登记至 `LTM2.sealed[]`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_to_257_normal_rotation_backpressure_and_slot_release)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_to_257_normal_rotation_backpressure_and_slot_release)'`（new，覆盖 AC1–AC8）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08v`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=segment-layer 256-cap normal rotation backpressure; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08v; writeset=序列化于 RB-08v; release=independent; split-from=RB-08g; exception=N/A`

---

### Task RB-08r: Case D 满 256 封存表后继段拒收与 `LTM2` 解码器结构上界守卫（Track A，自 `RB-08j` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs`（纯生产者/段层，零依赖消费者）落实并实证 `MAX_SEALED_SEGMENTS = 256` 的两类 fail-closed 结构守卫：Case D 在 Phase 1 100% 只读分类阶段即预扫后继段，凡 `sealed.len() == 256` 遇含完整有效帧后继段 `S+1`（即使同时叠加旧段未提交坏尾 `actual_len(S) > committed_len` 与遗留 `.tmp`）均在任何 `.tmp` 清理或旧段尾截断之前立即 fail-closed 报 `CorruptCommittedFrame`（`LBR-IO-001`）、整盘零改动且绝不写入第 257 项；`LTM2` 解码器分别拒收 `SEALED_COUNT > 256` 与 `SEALED_COUNT != ACTIVE_SEG - MIN_LIVE_SEG`。

**Out of scope:**
- 满 256 封存表下 Case C 跳过 Step C2（`RB-08l`）、恢复后段内准入 `<= 16 MiB`（`RB-08o`）与单次调用先恢复再拒收屏障（`RB-08s`）：由后续对应卡片承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 合规写者在创建 `S+1` 前必已受 256 背压拦截，故 Case D 满表遇有效 `S+1` 及解码器超界均须无条件 fail-closed | `ADR-RB-05` #3 Case D、`LTM2` 格式 |

**Acceptance criteria:**
- [x] **Case D 满表有效后继返回 `LBR-IO-001`：** 当 `sealed.len() == 256` 且磁盘存在含完整有效帧的异常后继段 `S+1` 时，Case D 立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **Case D 满表拒收零写入第 257 项：** 上述 AC1 拒收后，磁盘 `LTM2.sealed.len()` 严格保持 `256`（绝不追加第 257 项）。
- [x] **`LTM2` 解码器拒收 `SEALED_COUNT > 256`：** 任何 `SEALED_COUNT > 256` 的损坏元数据均被 `LTM2` 解码器 fail-closed 拒收。
- [x] **`LTM2` 解码器拒收段数不一致：** 任何 `SEALED_COUNT != ACTIVE_SEG - MIN_LIVE_SEG` 的损坏元数据均被 `LTM2` 解码器 fail-closed 拒收。
- [x] **满表有效后继叠加嵌套旧段坏尾返回 `LBR-IO-001`：** 构造 `sealed.len() == 256`、旧段 `S` 含未提交坏尾（`actual_len(S) > committed_len`）、后继段 `S+1` 含合法帧并预置 `tail.meta.tmp` 与 `quarantine/<lane>-*.tmp` 的夹具，Phase 1 只读扫后继即返 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **满表有效后继嵌套损坏态整盘零改动：** 上述 AC5 拒收前后，整个目标通道目录与共享 `quarantine/` 全文件集合及逐字节快照完全相等（`full_disk_snapshot_after == full_disk_snapshot_before`，零删 `.tmp`、零隔离、零截断旧段 `S`、零改 `LTM2`）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_case_d_and_ltm2_decoder_bounds_rejected)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_case_d_and_ltm2_decoder_bounds_rejected)'`（new，覆盖 AC1–AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08j`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=segment-layer case-d 256-cap and ltm2 decoder bounds; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08j; writeset=序列化于 RB-08j; release=independent; split-from=RB-08j; exception=N/A`

---

### Task RB-08l: 满 256 封存表 Case C 跳过 Step C2 守卫分支实现与段层验收（Track A，自 `RB-08j` 拆出，拆出 `RB-08o`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs`（纯生产者/段层，零依赖消费者）基于 `RB-08j` 引入的 `MAX_SEALED_SEGMENTS = 256` 常量，在 `RB-08n` 基础 Step C2 之前**首次新增** `if meta.sealed.len() >= MAX_SEALED_SEGMENTS` 条件跳过守卫分支并同卡验收：满 256 封存表遇半帧尾损崩溃时，调用 `RB-08m` 共用 `case_c_step_c1` 完成隔离、截断、刷盘与 `LTM2` 边界提交后提前返回，跳过 Step C2 轮转以保持 `ACTIVE_SEG == S`、`sealed.len() == 256` 且零新建 `S+1` 文件。

**Out of scope:**
- 满 256 封存表恢复后的段内准入（`<= 16 MiB`）：已拆出由 `RB-08o` 承接；含未恢复半帧尾的单次入队「先 Step ① 恢复再 Step ② `LBR-AGENT-046` 预写拒收」屏障：由 `RB-08s` 承接。
- 满 256 封存表高压下真实消费者逐段即时回收降压与全量排空：后置于 `RB-09g`、`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `sealed.len() == 256` 时 Case C 必须复用共用 Step C1 后严格跳过 Step C2，绝不写入第 257 项或破坏活跃段元数据 | `ADR-RB-05` #3 Case C Step C1/C2 |

**Acceptance criteria:**
- [x] **满表触发共用 Step C1 残尾隔离：** 当 `sealed.len() == 256` 且活跃段 `S` 发生半帧尾损崩溃时，`quarantine/` 精确收到 `[recovered_valid_end, actual_len)` 残字节。
- [x] **满表触发共用 Step C1 截断段 `S`：** `events-<S>.log` 文件长度截断至精确 `recovered_valid_end`（共用 `case_c_step_c1` 含 `RB-08m` 已验 `sync_data()`）。
- [x] **满表 Step C1 更新 `COMMITTED_LEN`：** 双副本 `LTM2.COMMITTED_LEN` 更新为精确 `recovered_valid_end`。
- [x] **满表 Step C1 更新 `PREFIX_CRC32C`：** 双副本 `LTM2.PREFIX_CRC32C` 更新为精确匹配 `events-<S>.log[0..recovered_valid_end]` 的校验和。
- [x] **满表跳过 Step C2 保持 `ACTIVE_SEG == S`：** 恢复完成后 `LTM2.ACTIVE_SEG` 严格保持 `S`。
- [x] **满表跳过 Step C2 保持 `sealed.len() == 256`：** 恢复完成后 `LTM2.sealed.len()` 严格保持 `256`（零第 257 项）。
- [x] **满表跳过 Step C2 零新建 `S+1` 文件：** 磁盘目录中不存在新建的 `events-<S+1:08>.log`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_case_c_skips_step_c2_and_commits_inplace_boundary)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_case_c_skips_step_c2_and_commits_inplace_boundary)'`（new，覆盖 AC1–AC7）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08r`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=segment-layer 256-cap case-c skip-step-c2 guard; recovery=revert isolated module; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=2; prod-files=3; scope=M; deps=RB-08r; writeset=序列化于 RB-08r; release=independent; split-from=RB-08j; exception=N/A`

---

### Task RB-08o: 满 256 封存表恢复后段内入队准入分支（`<= 16 MiB`）实现与验收（Track A，自 `RB-08l` 拆出，拆出 `RB-08s`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs`（纯生产者/段层，零依赖消费者）实现并同卡验收 `append_frame` 在 `sealed.len() == 256`（256×4 KiB 小封存段，合并占用远低于 `256 MiB`）满表恢复后的段内准入路径：当新帧满足 `committed_len + frame_len <= 16 MiB`（不触发轮转）时，放行直接追加至活跃段 `S`、同步推进双副本 `LTM2.COMMITTED_LEN` 并保持 `ACTIVE_SEG == S` 与 `sealed.len() == 256`（不误触发 `LBR-AGENT-046` 轮转背压）。

**Out of scope:**
- 含未恢复崩溃半帧尾且 `sealed.len() == 256` 时单次 `enqueue_frame` 调用先持久化 Step ① 恢复再于 Step ② 返回 `LBR-AGENT-046`（含写路径零写入插桩、前缀逐字节留存与重开持久化校验）：已按 `G-03`/`G-09` 拆出由 `RB-08s` 承接。
- 本已洁净的 Case A 满 256 表通道轮转拒收：已由 `RB-08j` 独立验收。
- 满 256 封存表高压下真实消费者逐段即时回收降压与全量排空：后置于 `RB-09g`、`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `sealed.len() == 256` 满表恢复后未超 `16 MiB` 的段内追加不触发轮转，必须准入落盘而非误报 `LBR-AGENT-046` | `ADR-RB-05` #3 Step ② (b) |

**Acceptance criteria:**
- [x] **满表恢复后段内入队返回 `Ok(())`：** 在 `sealed.len() == 256` 满表恢复后的活跃段 `S` 上，提交满足 `committed_len + frame_len <= 16 MiB` 的新帧返回 `Ok(())`（不误报 `LBR-AGENT-046`）。
- [x] **段内新帧无损解码读回：** 上述 AC1 写入的新帧可自 `events-<S>.log` 100% 无损解码读回。
- [x] **推进 `LTM2.COMMITTED_LEN`：** 上述 AC1 写入将双副本 `LTM2.COMMITTED_LEN` 推进至新段长。
- [x] **维持 `ACTIVE_SEG == S`：** 上述 AC1 写入后 `LTM2.ACTIVE_SEG` 保持 `S`。
- [x] **维持 `sealed.len() == 256`：** 上述 AC1 写入后 `LTM2.sealed.len()` 保持 `256`（零新增封存项）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_post_recovery_insegment_enqueue_admitted)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_post_recovery_insegment_enqueue_admitted)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08l`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=segment-layer 256-cap post-recovery in-segment admit; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08l; writeset=序列化于 RB-08l; release=independent; split-from=RB-08l; exception=N/A`

---

### Task RB-08s: 单次入队调用「先 Step ① 满表恢复再 Step ② 预写拒收」屏障与写路径零写入插桩实现（Track A，自 `RB-08o` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs`（纯生产者/段层，零依赖消费者）实现并同卡验收 `ADR-RB-05` #3 Step ① → Step ② 的严格先后屏障与可注入新帧写路径钩子：在以 256×4 KiB 小封存段（约 1 MiB）加近 16 MiB 活跃段（另一通道为 0）构造、严格满足恢复后合并总容量（含待入队新帧与隔离残尾）`<= 256 MiB`（从而完全排除全局字节上限 Step ② (a) 抢先触发）的满表通道上，当存在未恢复崩溃半帧尾且单次 `enqueue_frame` 待入队新帧满足 `recovered_valid_end + frame_len > 16 MiB` 时，Step ① 先调用 `RB-08t`/`RB-08l` 持久化断电安全隔离与 Case C Step C1 尾损恢复，随后 Step ② 在调用新帧写盘例程**之前**即因 256 封存表上限短路返回 `LBR-AGENT-046`；同卡通过容量隔离断言、写路径零调用插桩、`events-<S>.log` 完整字节逐字节比对与重开校验持久化 `LTM2` 彻底闭环验收。

**Out of scope:**
- 断电持久隔离副本发布（`.tmp → sync_data → rename → fsync_dir` 先于 `truncate`）与隔离残尾字节校验：由前置共用原语 `RB-08t` 与满表恢复卡 `RB-08l` 验收。
- 本已洁净的 Case A 满 256 表通道全调用零改动拒收：已由 `RB-08j` 独立验收。
- 满 256 封存表高压下真实消费者逐段即时回收降压与全量排空：后置于 `RB-09g`、`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 必须显式隔离 `<= 256 MiB` 全局容量上限以证明唯有 256 封存表上限触发 `LBR-AGENT-046`，并以写路径零调用插桩、前缀逐字节比对与重开校验持久化 `LTM2` 严格证明 Step ① 恢复与 Step ② 预写拒收 | `ADR-RB-05` #3 Step ①、Step ② |

**Acceptance criteria:**
- [x] **总容量 ≤ `256 MiB` 条件隔离断言：** 以 256×4 KiB 小封存段构造满表夹具，断言恢复后合并总容量 `unconsumed_priority + unconsumed_normal + quarantine + frame_buf.len() <= 256 MiB`（约 17 MiB，确保 Step ② (a) 全局字节上限绝不触发，唯有 Step ② (b) 256 封存表上限可拒收）。
- [x] **单次调用返回 `LBR-AGENT-046`：** 在上述含未恢复半帧尾且 `sealed.len() == 256` 的通道上，发起单次 `enqueue_frame`（`recovered_valid_end + frame_len > 16 MiB`）返回 `LBR-AGENT-046`。
- [x] **新帧写路径零调用插桩证明：** 通过 `segment.rs` 写入路径可注入插桩钩子断言，上述 AC2 调用期间新帧写盘例程调用次数精确为 `0`（绝未先写入被拒新帧再截断）。
- [x] **已恢复段文件逐字节原样留存：** 上述 AC2 调用返回后，磁盘 `events-<S>.log` 完整字节切片与预期已恢复数据（`[0..recovered_valid_end)`）逐字节相等（`actual_bytes == expected_recovered_bytes`）。
- [x] **零新建 `S+1` 文件：** 上述 AC2 调用返回后，磁盘目录中不存在 `events-<S+1:08>.log`。
- [x] **重开通道校验持久化 `COMMITTED_LEN`：** 关闭句柄并从磁盘重新打开通道加载双副本 `LTM2`，断言持久化 `COMMITTED_LEN == recovered_valid_end`。
- [x] **重开通道校验持久化 `PREFIX_CRC32C`：** 上述重开加载的持久化 `LTM2.PREFIX_CRC32C` 精确等于 `crc32c(&expected_recovered_bytes)`。
- [x] **重开通道校验持久化 `sealed.len() == 256`：** 上述重开加载的持久化 `LTM2.sealed.len()` 严格等于 `256`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_single_call_recovery_then_prewrite_reject_and_reopen)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::sealed_cap_256_single_call_recovery_then_prewrite_reject_and_reopen)'`（new，覆盖 AC1–AC8）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08o`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=segment-layer single-call step1-recover step2-reject barrier; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=3; scope=M; deps=RB-08o; writeset=序列化于 RB-08o; release=independent; split-from=RB-08o; exception=N/A`

---

### Task RB-08c: Capture 队列有效双副本 Case A/E/C 已确认前缀入队前校验与整盘零改动守卫（Track A，自 `RB-08b` 拆出，拆出 `RB-08y`、`RB-08e`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 落实并逐场景实证 `ADR-RB-05` #3 在有效双副本 `tail.meta`/`tail.meta.bak` 下的活跃段已确认前缀 Phase 1 只读 fail-closed 保护：包括元数据未变时正常路径（Case A）对 `[0, committed_len)` 位翻转的入队前拦截、元数据未变时活跃段被外部截短（Case E：`actual_len < committed_len`）的显式拒收，以及 Case C 扫描未提交尾之前的前缀先验校验——每场景均将「返回 `CorruptCommittedFrame`（`LBR-IO-001`）」与「整目录及 `quarantine/` 逐字节快照零改动（含预置 `tail.meta.tmp` 与 `quarantine/<lane>-*.tmp`）」独立成单谓词断言。

**Out of scope:**
- 单副本 `tail.meta` 缺失或损坏时由 `tail.meta.bak` Phase 1 只读确立边界并在镜像修复前拦截前缀损毁/短段（Subcase B1）：已按 `G-03`/`G-09` 拆出由 `RB-08y` 承接。
- 0 段文件四分法、非空通道 Subcase B2 无条件 fail-closed 与 Phase 2 跨通道 `.tmp` 清理过滤器：由 `RB-08e`、`RB-08h` 与 `RB-08x` 承接。
- 消费侧批写、`LTM2.sealed[]` 认证边界校验与高水位推进：由 `RB-09`..`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 仅凭 `actual_len == committed_len` 跳过前缀校验会放过静默位翻转；未显式拒收 `actual_len < committed_len` 会漏判已确认数据截断；Phase 1 损坏态拒收须整盘零改动 | `ADR-RB-05` #2、#3 |

**Acceptance criteria:**
- [x] **Case A 位翻转返回 `CorruptCommittedFrame`：** 保持有效双副本不变且 `actual_len == committed_len`，对 `[0, committed_len)` 任意已确认帧单字节翻转后发起新入队，Phase 1 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **Case A 位翻转整盘快照零改动：** 上述 AC1 夹具（预置 `tail.meta.tmp` 与 `quarantine/<lane>-orphan.tmp`）拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等。
- [x] **Case E 短段返回 `CorruptCommittedFrame`：** 保持有效双副本不变并将活跃段外部截短至 `actual_len < committed_len` 后发起新入队，Phase 1 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **Case E 短段整盘快照零改动：** 上述 AC3 夹具（预置 `tail.meta.tmp` 与 `quarantine/<lane>-orphan.tmp`）拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等。
- [x] **Case C 扫尾前前缀位翻转返回 `CorruptCommittedFrame`：** 活跃段存在 `actual_len > committed_len` 未提交尾且 `[0, committed_len)` 发生单字节翻转时，Phase 1 在扫描未提交尾前返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **Case C 扫尾前前缀位翻转整盘快照零改动：** 上述 AC5 夹具拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::unchanged_meta_case_a_bitflip_and_case_e_short_segment_rejected) | test(=storage::queue::segment::tests::case_c_prefix_bitflip_rejected_zero_mutation_before_tail_scan)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::unchanged_meta_case_a_bitflip_and_case_e_short_segment_rejected)'`（new，覆盖 AC1–AC4）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::case_c_prefix_bitflip_rejected_zero_mutation_before_tail_scan)'`（new，覆盖 AC5、AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08s`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue valid-meta case-a-e-c prefix guards; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=RB-08s; writeset=序列化于 RB-08s; release=independent; split-from=RB-08b; exception=N/A`

---

### Task RB-08y: Capture 队列 Subcase B1 主副本 `tail.meta` 缺失/损坏 Phase 1 权威边界先判与镜像修复（Track A，自 `RB-08c` 拆出，拆出 `RB-08z`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 落实并实证 `ADR-RB-05` #3 Subcase B1 **主副本缺失或损坏方向**（`tail.meta` 缺失或损坏而 `tail.meta.bak` 完整有效）的两阶段恢复不变量（反向副副本 `tail.meta.bak` 缺失或损坏的对称分支拆出由 `RB-08z` 独占承接）：Phase 1 仅只读自幸存有效副本确立权威 `LTM2` 边界并先检验活跃段前缀与长度，若前缀位翻转或短于 `committed_len` 则立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）且整盘零改动（**绝不提前镜像修复缺失/损坏副本**）；仅当 Phase 1 确认段完好后方在 Phase 2 原子镜像修复损坏副本。

**Out of scope:**
- 有效双副本 Case A/E/C 前缀守卫：已由 `RB-08c` 承接。
- Subcase B1 副副本 `tail.meta.bak` 缺失或损坏（`tail.meta` 有效）对称方向：已按 `G-03`/`G-09` 拆出由下游 `RB-08z` 独占承接。
- 0 段文件四分法、非空 Subcase B2 双副本同损与跨通道 `.tmp` 清理：由 `RB-08e`、`RB-08h` 与 `RB-08x` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 单副本损毁时须由幸存副本在 Phase 1 只读确立边界并先校验段完整性，损坏态绝不得在判定失败前提前镜像覆写对端副本或删除 `.tmp` | `ADR-RB-05` #3 Step ①、Subcase B1 |

**Acceptance criteria:**
- [x] **Subcase B1 前缀位翻转返回 `CorruptCommittedFrame`：** 删除或损坏 `tail.meta`（保留完整 `tail.meta.bak`）并翻转 `[0, committed_len)` 内单字节，Phase 1 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **Subcase B1 前缀位翻转整盘快照零改动：** 上述 AC1 夹具（预置 `tail.meta.tmp` 与 `quarantine/<lane>-orphan.tmp`）拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等（零提前镜像修复）。
- [x] **Subcase B1 短段返回 `CorruptCommittedFrame`：** 删除或损坏 `tail.meta`（保留完整 `tail.meta.bak`）并将活跃段截短至 `actual_len < committed_len`，Phase 1 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **Subcase B1 短段整盘快照零改动：** 上述 AC3 夹具拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等（零提前镜像修复）。
- [x] **Subcase B1 健康段 Phase 2 原子镜像修复：** 删除或损坏 `tail.meta`（保留完整 `tail.meta.bak`）而活跃段前缀完好时，Phase 2 原子重建的 `tail.meta` 字节与权威 `tail.meta.bak` 逐字节相等。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::subcase_b1_single_replica_loss_phase1_reject_and_phase2_mirror_repair)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::subcase_b1_single_replica_loss_phase1_reject_and_phase2_mirror_repair)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08c`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue subcase-b1 single-replica phase1 guard; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08c; writeset=序列化于 RB-08c; release=independent; split-from=RB-08c; exception=N/A`

---

### Task RB-08z: Capture 队列 Subcase B1 副副本 `tail.meta.bak` 缺失/损坏 Phase 1 权威边界先判与对称镜像修复（Track A，自 `RB-08y` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 落实并实证 `ADR-RB-05` #3 Subcase B1 **副副本缺失或损坏对称方向**（`tail.meta.bak` 缺失或损坏而主副本 `tail.meta` 完整有效）：Phase 1 仅只读自主副本 `tail.meta` 确立权威 `LTM2` 边界并先检验活跃段前缀与长度，若前缀位翻转或短于 `committed_len` 则立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）且整盘零改动（**绝不提前镜像修复 `tail.meta.bak`**）；仅当 Phase 1 确认段完好后方在 Phase 2 原子镜像修复 `tail.meta.bak`。

**Out of scope:**
- Subcase B1 主副本 `tail.meta` 缺失或损坏（`tail.meta.bak` 有效）方向：已由前置 `RB-08y` 承接。
- 0 段文件四分法、非空 Subcase B2 双副本同损与跨通道 `.tmp` 清理：由 `RB-08e`、`RB-08h` 与 `RB-08x` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 双副本恢复必须对称支持主副本与副副本任一缺失/损坏；若仅实现主副本缺失回退分支而未实现副副本缺失镜像分支，备副本损坏场景将无法正确校验或修复 | `ADR-RB-05` #3 Step ①、Subcase B1 |

**Acceptance criteria:**
- [x] **副副本缺失/损坏 + 前缀位翻转返回 `CorruptCommittedFrame`：** 删除或损坏 `tail.meta.bak`（保留完整 `tail.meta`）并翻转 `[0, committed_len)` 内单字节，Phase 1 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **副副本缺失/损坏 + 前缀位翻转整盘快照零改动：** 上述 AC1 夹具（预置 `tail.meta.bak.tmp` 与 `quarantine/<lane>-orphan.tmp`）拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等（零提前镜像修复 `tail.meta.bak`）。
- [x] **副副本缺失/损坏 + 短段返回 `CorruptCommittedFrame`：** 删除或损坏 `tail.meta.bak`（保留完整 `tail.meta`）并将活跃段截短至 `actual_len < committed_len`，Phase 1 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **副副本缺失/损坏 + 短段整盘快照零改动：** 上述 AC3 夹具拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等（零提前镜像修复 `tail.meta.bak`）。
- [x] **副副本缺失/损坏 + 健康段 Phase 2 原子镜像修复：** 删除或损坏 `tail.meta.bak`（保留完整 `tail.meta`）而活跃段前缀完好时，Phase 2 原子重建的 `tail.meta.bak` 字节与权威 `tail.meta` 逐字节相等。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::subcase_b1_backup_replica_loss_phase1_reject_and_phase2_mirror_repair)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::subcase_b1_backup_replica_loss_phase1_reject_and_phase2_mirror_repair)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08y`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue subcase-b1 backup-replica symmetric guard; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08y; writeset=序列化于 RB-08y; release=independent; split-from=RB-08y; exception=N/A`

---

### Task RB-08e: Capture 队列 0 段文件四分法与唯一活跃段缺失/损坏元数据 fail-closed 守卫（Track A，自 `RB-08c` 拆出，拆出 `RB-08h`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 落实并实证 `ADR-RB-05` #3 情况 0 的严格四分法（彻底封堵「唯一活跃段被删导致 0 个段文件却被重置为空通道」的静默丢数漏洞）：仅当目录中 `events-*.log` 数为 `0` 且 `tail.meta`/`tail.meta.bak` 均不存在于磁盘（情况 0a）时初始化全新 34B `LTM2` `(0,0,0,0,[])` 双副本；若 `0` 段且存在至少 1 份有效初始 `LTM2` 精确等于 `(0,0,0,0,[])`（即使另一副本缺失或损坏，情况 0b），则按 Subcase B1 镜像修复后放行创建首段；若 `0` 段但有效元数据记录非零已提交边界（`committed_len > 0` 或 `active_seg > 0` 或 `!sealed.is_empty()`，情况 0c），或磁盘存在元数据文件但**两份副本均非有效 `LTM2`**（情况 0d），一律归入 Case E 无条件 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`、严禁重置元数据、严禁追加、磁盘零改动）。

**Out of scope:**
- 目录存在 ≥ 1 个 `events-*.log` 文件时的 Subcase B2 双副本同损无条件 fail-closed 守卫（完整帧边界截断、0 字节活跃段、`max_seg`/较旧段损坏）：已按 `G-03`/`G-09` 拆出由 `RB-08h` 承接。
- 消费侧批写、`LTM2.sealed[]` 校验与锁序恢复：由 `RB-09`..`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 仅凭 `events-*.log` 数量为 0 就重置双副本会抹除唯一活跃段被删时的非零已确认边界并绕过 Case E；必须联合检查 `tail.meta`/`tail.meta.bak` 存在性与值域 | `ADR-RB-05` #3 情况 0 |

**Acceptance criteria:**
- [x] **情况 0a — 真正全新空通道初始化：** 当通道目录不含任何 `events-*.log` 且 `tail.meta` 与 `tail.meta.bak` 均不存在（`ENOENT`）时，先原子创建并 `fsync_dir` 初始 34B `LTM2` `(0, 0, 0, 0, [])` 双副本再建首段 `events-00000000.log`。
- [x] **情况 0b — 已预建未首写通道放行：** 当通道目录不含任何 `events-*.log` 且存在有效 `LTM2` 精确等于 `(0, 0, 0, 0, [])`（含仅存 1 份有效副本的 Subcase B1 镜像修复）时，允许正常创建首段并入队。
- [x] **情况 0c 返回 `CorruptCommittedFrame`：** 写入已确认帧后删除全部 `events-*.log`（保留有效 `LTM2` 含 `committed_len > 0` 或 `active_seg > 0`），后续入队归入 Case E 返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **情况 0d 返回 `CorruptCommittedFrame`：** 目录不含任何 `events-*.log`、磁盘存在元数据文件且两副本均非有效 `LTM2` 时，后续入队返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **情况 0c 整盘快照零改动：** 上述 AC3 情况 0c 夹具（预置 `tail.meta.tmp` 与 `quarantine/<lane>-orphan.tmp`）拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等（零重置元数据、零建段、零删 `.tmp`）。
- [x] **情况 0d 整盘快照零改动：** 上述 AC4 情况 0d 夹具（预置 `tail.meta.tmp` 与 `quarantine/<lane>-orphan.tmp`）拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等（零重置元数据、零建段、零删 `.tmp`）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::virgin_and_prebootstrapped_zero_segment_lane_init) | test(=storage::queue::segment::tests::zero_segment_with_committed_or_corrupt_meta_fails_closed)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::virgin_and_prebootstrapped_zero_segment_lane_init)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::zero_segment_with_committed_or_corrupt_meta_fails_closed)'`（new，覆盖 AC3–AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08z`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue zero-segment 4-way missing-active guard; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=3; scope=S; deps=RB-08z; writeset=序列化于 RB-08z; release=independent; split-from=RB-08c; exception=N/A`

---

### Task RB-08h: Capture 队列非空通道 Subcase B2 双副本同损无条件 fail-closed 与整盘零改动守卫（Track A，自 `RB-08e` 拆出，拆出 `RB-08x`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 完整落实并实证 `ADR-RB-05` #3 非空通道 Subcase B2 无条件 fail-closed 不变量：只要目录中存在 ≥ 1 个 `events-*.log` 文件且 `tail.meta` 与 `tail.meta.bak` 同时缺失或损坏，**Phase 1 立即无条件 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）且整盘零改动（零自动重建元数据、零截断、零删段、零删遗留 `tail.meta.tmp`/`quarantine/<lane>-*.tmp`）**——将完整帧边界截断（`CleanEof`）、活跃段 0 字节截断、`max_seg` 帧损坏/截断与较旧轮转段损坏/截断 4 类夹具的「错误码返回」与「整目录及 `quarantine/` 全文件逐字节快照相等」拆分为 8 条 1:1 独立单谓词断言。

**Out of scope:**
- Phase 2 按通道前缀清理过滤器（`clean_lane_temp_files`）及健康通道入队原样保留另一损坏通道 `quarantine/<other_lane>-*.tmp` 的跨通道对称验收：已按 `G-03`/`G-09` 拆出由 `RB-08x` 承接。
- 消费侧批写、`LTM2.sealed[]` 认证边界校验与锁序恢复：由 `RB-09`..`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 双副本同损时按段解码自动重建元数据无法检出完整帧边界截断（仍为 `CleanEof`）或活跃段截断为 0 字节；因 `RB-07b` 已在建首段前预建初始 34B `LTM2` 双副本，故非空目录双副本同损可无条件 fail-closed 且整盘零改动 | `ADR-RB-05` #3 Subcase B2 |

**Acceptance criteria:**
- [x] **完整帧边界截断（`CleanEof`）返回 `CorruptCommittedFrame`：** 多帧写入后删除/损坏双副本并将活跃段精确截短至先前完整帧边界（整段仍 `CleanEof`，并预置 `tail.meta.tmp` 与 `quarantine/<lane>-orphan.tmp`），入队返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **完整帧边界截断整盘快照零改动：** 上述 AC1 夹具拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等（含所有段、元数据与遗留 `.tmp`）。
- [x] **活跃段 0 字节截断返回 `CorruptCommittedFrame`：** 删除/损坏双副本并将现存活跃段 `events-<S>.log` 截短为 0 字节后，入队返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **活跃段 0 字节截断整盘快照零改动：** 上述 AC3 夹具拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等。
- [x] **`max_seg` 帧损坏/末帧截断返回 `CorruptCommittedFrame`：** 删除/损坏双副本且 `max_seg` 存在位翻转或半帧截断时，入队返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **`max_seg` 帧损坏/末帧截断整盘快照零改动：** 上述 AC5 夹具拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等。
- [x] **较旧轮转段损坏/截断返回 `CorruptCommittedFrame`：** 多轮转段并存、删除/损坏双副本且较旧段（`seg < max_seg`）受损或截断（即使 `max_seg` 洁净）时，入队返回 `CorruptCommittedFrame`（`LBR-IO-001`）。
- [x] **较旧轮转段损坏/截断整盘快照零改动：** 上述 AC7 夹具拒收前后，整个通道目录与 `quarantine/` 全文件逐字节快照完全相等。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::dual_metadata_loss_complete_frame_boundary_and_zero_byte_active_fails_closed) | test(=storage::queue::segment::tests::dual_metadata_loss_max_and_older_segment_corrupt_or_truncated_fails_closed)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::dual_metadata_loss_complete_frame_boundary_and_zero_byte_active_fails_closed)'`（new，覆盖 AC1–AC4）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::dual_metadata_loss_max_and_older_segment_corrupt_or_truncated_fails_closed)'`（new，覆盖 AC5–AC8）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08e`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue nonempty subcase-b2 dual-loss guard; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=2; prod-files=3; scope=S; deps=RB-08e; writeset=序列化于 RB-08e; release=independent; split-from=RB-08e; exception=N/A`

---

### Task RB-08x: Capture 队列共享 `quarantine/` 按通道前缀临时文件清理过滤器与跨通道对称隔离（Track A，自 `RB-08h` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `segment.rs` 实现并实证 `ADR-RB-05` #3 Step ① Phase 2 的通道限定临时文件清理过滤器 `clean_lane_temp_files(lane_id, lane_dir, quarantine_dir)`：共享 `quarantine/` 下按确定性前缀 `<lane>-` 严格过滤，健康 `priority/` 入队仅清理 `priority/tail.meta*.tmp` 与 `quarantine/priority-*.tmp`、绝不触碰损坏 `normal/` 的 `quarantine/normal-*.tmp`，反之亦然，完成双通道对称跨通道隔离验收。

**Out of scope:**
- 非空 Subcase B2 双副本同损四夹具拒收与整盘零改动：已由 `RB-08h` 承接。
- 消费侧批写与单写入者高水位：由下游 `RB-09`..`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `priority/` 与 `normal/` 共享 `quarantine/` 目录，Phase 2 必须严格按 `quarantine/<lane>-*.tmp` 前缀限定清理，杜绝健康通道入队误删另一损坏通道的取证临时文件 | `ADR-RB-05` #1、#3 Step ① |

**Acceptance criteria:**
- [x] **健康 `priority/` 清理本通道 `priority/tail.meta*.tmp`：** 当 `priority/` 通过 Phase 1 分类执行入队后，`priority/` 目录中不再存在任何 `tail.meta*.tmp`。
- [x] **健康 `priority/` 原样保留损坏 `normal/` 的 `normal/tail.meta.tmp`：** 当 `normal/` 处于 Subcase B2 损坏态且存在 `normal/tail.meta.tmp` 时，上述 `priority/` 入队后 `normal/tail.meta.tmp` 字节与入队前逐字节相等。
- [x] **健康 `priority/` 清理本通道 `quarantine/priority-*.tmp`：** 上述 `priority/` 入队后，共享目录中不再存在 `quarantine/priority-*.tmp`。
- [x] **健康 `priority/` 原样保留损坏 `normal/` 的 `quarantine/normal-*.tmp`：** 当 `normal/` 处于 Subcase B2 损坏态且存在 `quarantine/normal-seg0-off128-deadbeef.tmp` 时，上述 `priority/` 入队后该文件字节与入队前逐字节相等。
- [x] **健康 `normal/` 清理本通道 `normal/tail.meta*.tmp`：** 当 `normal/` 通过 Phase 1 分类执行入队后，`normal/` 目录中不再存在任何 `tail.meta*.tmp`。
- [x] **健康 `normal/` 原样保留损坏 `priority/` 的 `priority/tail.meta.tmp`：** 当 `priority/` 处于 Subcase B2 损坏态且存在 `priority/tail.meta.tmp` 时，上述 `normal/` 入队后 `priority/tail.meta.tmp` 字节与入队前逐字节相等。
- [x] **健康 `normal/` 清理本通道 `quarantine/normal-*.tmp`：** 上述 `normal/` 入队后，共享目录中不再存在 `quarantine/normal-*.tmp`。
- [x] **健康 `normal/` 原样保留损坏 `priority/` 的 `quarantine/priority-*.tmp`：** 当 `priority/` 处于 Subcase B2 损坏态且存在 `quarantine/priority-seg0-off128-cafebabe.tmp` 时，上述 `normal/` 入队后该文件字节与入队前逐字节相等。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::cross_lane_quarantine_tmp_cleanup_scoped_to_classified_lane_only)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::segment::tests::cross_lane_quarantine_tmp_cleanup_scoped_to_classified_lane_only)'`（new，覆盖 AC1–AC8）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 3 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08h`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue cross-lane quarantine-tmp cleanup filter; recovery=revert isolated module; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-08h; writeset=序列化于 RB-08h; release=independent; split-from=RB-08h; exception=N/A`

---

### Task RB-09: Capture 双通道优先排空批量写入者与锁协调历史段回收（Track A，自 `RB-07` 拆出，拆出 `RB-09b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内实现 `highwater.rs` 与 `writer.rs` 核心消费循环：严格优先排空 `priority/` 通道（`SessionEnd`）后再批量消费 `normal/` 通道（≤ 128 条 / ≤ 50ms 合并进 1 次 `CaptureStore::append_capture_batch` 写事务）、同事务原子推进库内双通道高水位、语义 `dedup_key` 幂等去重，以及遵循 `writer.lock → capture-queue/append.lock` 锁序（不跨红库写事务持 `append.lock`）复核 `seg == min_live_seg && seg < active_seg && seg < highwater_seg` 后先原子推进 `LTM2` `MIN_LIVE_SEG` 摘除 `sealed[]` 再 `unlink` + `fsync_dir` 回收历史段。

**Out of scope:**
- 写入者镜像崩溃恢复、活跃已确认前缀/双损拒收与单写者锁探测：由 `RB-09b` 承接；未回收轮转段 `LTM2.sealed[]` 认证边界校验（含完整帧边界截断）由 `RB-09d` 承接。
- 消费者锁序协调、暂停生产者实测与真崩溃零跳帧原子高水位恢复：由 `RB-09c` 与 `RB-09e` 承接。
- 实时 Hook 入口与 `libra agent daemon` CLI 接线：由 `RB-29` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `plan-20261116` 要求 `SessionEnd` 优先于普通事件；现行 `drain.rs` 逐文件开多事务；历史段回收须与 `append.lock` 协调防止竞态 | `docs/development/plan/plan-20261116-capture-queue.md:155`；`src/internal/ai/capture/drain.rs:15`；`ADR-RB-05` #4 |

**Acceptance criteria:**
- [x] 当 `normal/` 积压 200 条普通事件且 `priority/` 到达 1 条 `SessionEnd` 帧时，写入者严格先完成 `priority/` 落库再消费 `normal/`。
- [x] `priority/` 与 `normal/` 各自高水位严格单调连续无空洞。
- [x] `normal/` 连续到达的 128 条事件合并进 **1 次** `CaptureStore::append_capture_batch` 写事务并同事务更新 `QUEUE_HIGHWATER_NORMAL`。
- [x] 重复完整语义 `dedup_key` 帧重放幂等，只产生一条业务记录。
- [x] 消费者按 `writer.lock → capture-queue/append.lock` 获取 `append.lock`，复核 `seg == min_live_seg && seg < active_seg && seg < highwater_seg` 后先原子更新 `LTM2`（推进 `MIN_LIVE_SEG` 并摘除 `sealed[]`）再 `unlink` + `fsync_dir` 回收历史段。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::queue::writer::tests::priority_lane_drains_before_normal_with_contiguous_highwater) | test(=storage::queue::writer::tests::batch_128_single_tx_dedup_and_segment_gc)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::queue::writer::tests::priority_lane_drains_before_normal_with_contiguous_highwater)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::queue::writer::tests::batch_128_single_tx_dedup_and_segment_gc)'`（new，覆盖 AC3、AC4、AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-08x`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue priority-first batch writer and gc; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=2; prod-files=4; scope=M; deps=RB-08x; writeset=序列化于 RB-08x; release=independent; split-from=RB-07; exception=N/A`

---

### Task RB-09b: Capture 写入者高水位镜像恢复、活跃前缀/双损/高水位倒退拒收与单写者锁探测（Track A，自 `RB-09` 拆出，拆出 `RB-09d`、`RB-09c`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `highwater.rs` 与 `writer.rs` 补齐高水位镜像崩溃恢复、活跃段已确认数据保护与单写者选举：`CaptureStore` 提交后外部 `highwater` 镜像更新前崩溃的零重复恢复，消费者遇活跃段已确认前缀（`offset < committed_len`）损坏、双副本元数据同损或通道元数据倒退落后于库内 `QUEUE_HIGHWATER_*` 均严格 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`，绝不隔离或推进高水位），以及 `writer.lock` 竞争无副作用退出与 `is_writer_active()` 锁探测。

**Out of scope:**
- 未回收已轮转历史段 `LTM2.sealed[]` 认证边界校验（含精确停在完整帧边界的 `CleanEof` 截断拒收）与先更新 `LTM2` 再 `unlink` 崩溃安全回收顺序：已按 `G-03`/`G-09` 拆出由 `RB-09d` 承接。
- 消费者 `writer.lock → capture-queue/append.lock` 锁序协调、暂停活跃生产者并发实测与真崩溃零跳帧原子高水位恢复：由 `RB-09c` 与 `RB-09e` 承接。
- 实时 Hook 与 `libra agent daemon` 接线：由 `RB-29` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 外部 `highwater` 仅作镜像、库内 `QUEUE_HIGHWATER_*` 为权威；消费者在活跃前缀损坏、双副本同损或元数据倒退下均须无条件 fail-closed | `ADR-RB-05` #2、#4 |

**Acceptance criteria:**
- [x] 故障注入：事务提交成功、外部 `highwater` 镜像更新前崩溃，重启后从库内 `QUEUE_HIGHWATER_*` 恢复零重复记录。
- [x] 故障注入：消费者在活跃段已确认前缀（`offset < committed_len`）检出损坏/截断帧时，立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严禁隔离丢弃或推进高水位。
- [x] 故障注入：存在 ≥ 1 个段文件且 `tail.meta`/`tail.meta.bak` 双副本同损时，消费者立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严禁推进高水位。
- [x] 故障注入：目录含 0 个段文件但存在非零已提交边界或损坏元数据时，消费者立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严禁推进高水位。
- [x] 故障注入：通道 `LTM2` 元数据倒退落后于库内 `QUEUE_HIGHWATER_*` 时，消费者立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严禁推进高水位。
- [x] 第二个消费者争抢 `writer.lock` 失败立即无副作用返回 `Ok(())`。
- [x] `is_writer_active()` 复用同一 `writer.lock` 准确探测当前存在活跃持锁者。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::crash_after_commit_before_mirror_zero_dup) | test(=storage::queue::writer::tests::consumer_rejects_committed_prefix_dual_meta_loss_and_hw_regression) | test(=storage::queue::writer::tests::single_writer_lock_contention_and_active_probe)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::crash_after_commit_before_mirror_zero_dup)'`（new，覆盖 AC1）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_rejects_committed_prefix_dual_meta_loss_and_hw_regression)'`（new，覆盖 AC2–AC5）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::single_writer_lock_contention_and_active_probe)'`（new，覆盖 AC6、AC7）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 4 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 4 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue writer mirror crash prefix dual-loss lock; recovery=revert isolated module; complete=yes; self-contained=yes; AC=7/8; VER=5/8; landing=2; prod-files=4; scope=M; deps=RB-09; writeset=序列化于 RB-09; release=independent; split-from=RB-09; exception=N/A`

---

### Task RB-09d: Capture 消费者未回收轮转段 `LTM2.sealed[]` 认证边界校验与崩溃安全回收序（Track A，自 `RB-09b` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs` 与 `segment.rs` 落实并实证 `ADR-RB-05` #4 对所有未回收已轮转段（`min_live_seg <= seg < active_seg`）的持久化认证边界校验：消费前逐段核验 `LTM2.sealed[seg]` 的文件存在、`actual_len == sealed_len` 与 `crc32c(seg[0..sealed_len]) == sealed_crc32c`，精准拦截「未消费轮转段被精确截断在先前完整帧边界（仍解码为 `CleanEof`）」、单字节位翻转、文件缺失或尾部额外字节，全部 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`、严禁推进高水位或回收段）；并实证回收段严格先原子更新 `LTM2`（推进 `MIN_LIVE_SEG` 摘除 `sealed[]`）再 `unlink` + `fsync_dir` 及其崩溃孤儿清理。

**Out of scope:**
- 消费者 `writer.lock → append.lock` 暂停生产者实测与真崩溃 Case C 零跳帧恢复：由 `RB-09c` 与 `RB-09e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 若元数据仅记录活跃段长度与校验和，未消费轮转段在精确完整帧边界被截断后仍呈 `CleanEof` 流，消费者无法察觉缺失尾部；`LTM2.sealed[]` 为每段提供持久化 `(sealed_len, sealed_crc32c)` 边界 | `ADR-RB-05` #3、#4 |

**Acceptance criteria:**
- [x] **未消费轮转段精确完整帧边界截断（`CleanEof`）拒收：** 多帧写入段 `S` 并轮转至 `S+1`（`LTM2.sealed[S]` 已落盘）后，将未消费的 `events-<S>.log` 精确截断至某条先前完整帧边界（整段仍 100% 解码为 `CleanEof`），消费者检出 `actual_len != sealed_len` 立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严禁推进高水位或回收 `S`。
- [x] **未消费轮转段单字节位翻转拒收：** 翻转未消费轮转段 `S < active_seg` 任一字节，消费者检出 `sealed_crc32c` 失配立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严禁推进高水位。
- [x] **未回收轮转段缺失拒收：** 删除 `LTM2.sealed[]` 中登记的未回收轮转段 `S`，消费者立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严禁推进高水位。
- [x] **未回收轮转段尾部多字节拒收：** 向未回收轮转段 `S` 尾部追加额外字节致 `actual_len > sealed_len`，消费者立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`），严禁推进高水位。
- [x] **回收段崩溃安全顺序与孤儿清理：** 故障注入消费者在更新 `LTM2`（推进 `MIN_LIVE_SEG = S + 1` 并摘除 `sealed[S]`）之后、`unlink("events-<S>.log")` 之前崩溃，重启后识别 `S < MIN_LIVE_SEG` 为已回收孤儿文件安全 `unlink` + `fsync_dir` 而不触发误报。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_rejects_rotated_seg_complete_frame_truncation_bitflip_and_missing) | test(=storage::queue::writer::tests::reclamation_updates_ltm2_before_unlink_and_cleans_crash_orphan)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_rejects_rotated_seg_complete_frame_truncation_bitflip_and_missing)'`（new，覆盖 AC1–AC4）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::reclamation_updates_ltm2_before_unlink_and_cleans_crash_orphan)'`（new，覆盖 AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue LTM2-sealed rotated boundary and safe gc; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=2; prod-files=5; scope=M; deps=RB-09b; writeset=序列化于 RB-09b; release=independent; split-from=RB-09b; exception=N/A`

---

### Task RB-09c: Capture 消费者 `writer.lock → append.lock` 锁序协调、暂停生产者并发实测与段回收互斥（Track A，自 `RB-09b` 拆出，拆出 `RB-09e`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs` 与 `segment.rs` 落实并实证 `ADR-RB-05` #4 的消费者-生产者无竞态锁协调协议：严格遵循全局锁序 **`writer.lock → capture-queue/append.lock`**（与反向迁移一致、生产者永不取 `writer.lock` 保证零死锁，消费者绝不跨 `CaptureStore` 写事务持 `append.lock`）；常规消费只读已确认字节 `[0, committed_len)`，未提交尾重验与历史段回收均必先获 `append.lock` 重验元数据，彻底杜绝消费者将暂停中的活跃生产者未提交写误判为崩溃残片隔离或删除活跃段。

**Out of scope:**
- 真崩溃未提交尾在 `append.lock` 内复用生产者 Case C 恢复（保留全部积压与完整同步尾帧、绝不提前推进高水位、仅随 DB 提交原子推进）：已按 `G-03`/`G-09` 拆出由 `RB-09e` 承接。
- 实时 Hook 与 `libra agent daemon` 接线：由 `RB-29` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 若消费者仅持 `writer.lock` 即对 `offset >= committed_len` 执行残尾隔离或回收历史段，将与正在 `append.lock` 内提交的生产者产生竞态导致丢帧或误隔离 | `ADR-RB-05` #4；`GC-RB-04` |

**Acceptance criteria:**
- [x] **全局无死锁锁序：** 消费者仅在持有 `writer.lock` 后获取 `capture-queue/append.lock` 执行未提交尾重验/恢复与历史段回收，并在调用 `CaptureStore::append_capture_batch` 前释放 `append.lock`（生产者永不获取 `writer.lock`）。
- [x] **暂停生产者零误隔离并发实测：** 并发生产者持有 `append.lock` 向活跃段 `offset >= committed_len` 写入未提交字节后暂停于 `LTM2` 提交前；同步运行的消费者最多只消费 `< committed_len` 并阻塞于 `append.lock`，绝不隔离在途字节、零 `LBR-AGENT-050`、零跳过活跃段。
- [x] **暂停生产者恢复提交后 100% 无损入库：** 放行上述暂停生产者完成 `sync_data` 与双副本 `LTM2` 提交并释放 `append.lock` 后，消费者在 `append.lock` 内重读推进后的 `LTM2`，100% 完整消费入库该帧且 `quarantine/` 为空。
- [x] **历史段回收 `append.lock` 互斥实测：** 消费者在 `append.lock` 内复核 `seg == min_live_seg && seg < active_seg && seg < highwater_seg` 后先更新 `LTM2` 再 `unlink` + `fsync_dir`，暂停生产者期间绝不误删 `active_seg` 亦不干扰生产者容量统计。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_waits_for_append_lock_and_does_not_quarantine_paused_producer) | test(=storage::queue::writer::tests::consumer_segment_reclamation_coordinates_with_append_lock)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_waits_for_append_lock_and_does_not_quarantine_paused_producer)'`（new，覆盖 AC1、AC2、AC3）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_segment_reclamation_coordinates_with_append_lock)'`（new，覆盖 AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue consumer append-lock paused-producer mutex; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=5; scope=M; deps=RB-09d; writeset=序列化于 RB-09d; release=independent; split-from=RB-09b; exception=N/A`

---

### Task RB-09e: Capture 消费者真崩溃 Case C 尾损恢复委托与零提前高水位不变量（Track A，自 `RB-09c` 拆出，拆出 `RB-09f`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs` 与 `segment.rs` 落实消费者真崩溃尾损恢复协议（`ADR-RB-05` #4 ②）：当消费者持有 `writer.lock → capture-queue/append.lock` 且验证 `[0, committed_len)` 后仍检出真崩溃残尾（`actual_len > committed_len`）时，直接复用 `RB-08m`/`RB-08n`/`RB-08i`/`RB-08k`/`RB-08l`/`RB-08o` 共享的两步 Case C 恢复例程（隔离非法残尾、记录 `LBR-AGENT-050`、截断刷盘与条件切段），**严格保证释锁前库内 `QUEUE_HIGHWATER_*` 与外部 `highwater` 镜像均零变更、纯已同步尾崩溃零误隔离，且绝不在持有 `append.lock` 期间执行 `CaptureStore` 写事务**。

**Out of scope:**
- 恢复完成释放 `append.lock` 后常规消费循环多批（`> 128` 帧）落库、按段状态精确推进高水位、消费端 `max_seg > active_seg`（Case D / 嵌套 Case C→D）恢复委托、满 256 表高压逐段即时回收与事后安全回收 `S`：已按 `G-03`/`G-09` 拆出由 `RB-09f`..`RB-09l` 承接。
- 实时 Hook 与 `libra agent daemon` 接线：由 `RB-29`、`RB-29d` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 消费者尾损恢复必须复用共享 Case C 恢复并严格禁止在 DB 写事务提交前修改库内或外部高水位 | `ADR-RB-05` #3 Case C、#4 ② |

**Acceptance criteria:**
- [x] **持锁委托共享 Case C 恢复：** 消费者在持有 `writer.lock → append.lock` 且 `[0, committed_len)` 校验通过后检出真崩溃 `actual_len > committed_len` 时，调用共享两步 Case C 恢复例程保留 `[committed_len, recovered_valid_end)` 并仅将 `[recovered_valid_end, actual_len)` 隔离至 `quarantine/`。
- [x] **诊断码记录：** 上述真崩溃尾损隔离精确记录诊断码 `LBR-AGENT-050`。
- [x] **库内高水位零变更：** 尾损恢复释放 `append.lock` 时，库内 `QUEUE_HIGHWATER_*` 严格保持恢复前原值零变更。
- [x] **外部镜像零变更：** 尾损恢复释放 `append.lock` 时，外部 `highwater` 镜像文件严格保持恢复前原值零变更。
- [x] **纯已同步尾零误隔离：** 当 `actual_len > committed_len` 全部由完整 CRC32C 合法帧组成（`recovered_valid_end == actual_len`）时，恢复在 `append.lock` 内推进 `LTM2` `committed_len` 且 `quarantine/` 为空、零 `LBR-AGENT-050`。
- [x] **绝不跨 `append.lock` 写库：** 消费者在尾损恢复持 `append.lock` 期间绝不调用 `CaptureStore::append_capture_batch`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_tail_recovery_delegates_case_c_without_advancing_hw) | test(=storage::queue::writer::tests::consumer_pure_synced_tail_recovery_zero_quarantine_and_no_db_write_under_lock)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_tail_recovery_delegates_case_c_without_advancing_hw)'`（new，覆盖 AC1–AC4、AC6）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_pure_synced_tail_recovery_zero_quarantine_and_no_db_write_under_lock)'`（new，覆盖 AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09c`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue consumer tail recovery no-hw-advance guard; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=5; scope=M; deps=RB-09c; writeset=序列化于 RB-09c; release=independent; split-from=RB-09c; exception=N/A`

---

### Task RB-09f: Capture 消费者已封存段多批（>128 帧）积压与完整同步尾原子落库及末批跨段高水位推进（Track A，自 `RB-09e` 拆出，拆出 `RB-09g`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs` 与 `highwater.rs` 端到端实证 `ADR-RB-05` #4 ② 已封存段（`S < active_seg`，经 Step C2 封存切段至 `S+1`）的常规多批消费闭环：在同一段 `S` 同时包含超过单批 128 帧上限（如 `> 128` 帧）的未消费已确认积压帧 `[hw_offset, committed_len)`、已 `sync_data` 完整尾帧 `[committed_len, sealed_len(S))` 与崩溃半帧场景下，常规消费将全部积压帧与全部恢复完整尾帧 100% 写入 `capture.redb`，所有非末批（`batch_end_offset < sealed_len(S)`）均推进至 `(S, batch_end_offset)`，**仅最终批次精确耗尽 `sealed_len(S)` 时随该 DB 事务原子推进 `QUEUE_HIGHWATER_*` 至 `(S+1, 0)`**，事后再按 `LTM2` 先更新 `MIN_LIVE_SEG` 再 `unlink` 安全回收 `S`。

**Out of scope:**
- 恢复后段 `S` 仍保持活跃（`S == active_seg`：纯已同步尾、满 256 表 Step C1 原地恢复、0 字节恢复）的多批排空、高水位不跳段与后续追加实测：已按 `G-03`/`G-09` 拆出由 `RB-09g` 承接。
- 实时 Hook 与 `libra agent daemon` 接线：由 `RB-29`、`RB-29d` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 已封存段 `S < active_seg` 帧数超过 128 条批上限时，中间批次必须停在 `(S, batch_end_offset)`，仅最终批次消费至 `sealed_len(S)` 时原子推进至 `(S+1, 0)` | `ADR-RB-05` #4 ② (b)、③ |

**Acceptance criteria:**
- [x] **已封存段积压帧 100% 落库：** 构造经 Step C2 封存的段 `S`（`S < active_seg`，合计 `> 128` 帧，含未消费已确认积压帧 `[hw_offset, committed_len)`、完整同步尾帧与崩溃半帧）；经 `RB-09e` 恢复后运行常规消费，全部积压帧 100% 写入 `capture.redb`。
- [x] **已封存段完整同步尾帧 100% 落库：** 在上述同一多批场景下，全部已 `sync_data` 完整尾帧 `[committed_len, sealed_len(S))` 100% 写入 `capture.redb`。
- [x] **多批中间批次绝不提前跳段：** 消费 `S` 中超过 128 帧的前序中间批次（`batch_end_offset < sealed_len(S)`）时，每次 `CaptureStore::append_capture_batch` 事务将 `QUEUE_HIGHWATER_*` 原子推进至 `(S, batch_end_offset)`，绝不提前跳至 `(S+1, 0)`。
- [x] **仅最终耗尽批次原子推进至 `(S+1, 0)`：** 仅当已封存段 `S < active_seg` 的最终批次精确消费至 `batch_end_offset == sealed_len(S)` 时，同一 `CaptureStore::append_capture_batch` 写事务原子将 `QUEUE_HIGHWATER_*` 推进至 `(S+1, 0)`。
- [x] **提交后安全回收已封存段 `S`：** 上述最终 DB 事务提交推进高水位越过 `S` 后，消费者按 `LTM2` 先推进 `MIN_LIVE_SEG = S + 1` 再 `unlink` 安全回收 `events-<S>.log`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_sealed_seg_multibatch_recovery_backlog_tail_and_final_hw_advance)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_sealed_seg_multibatch_recovery_backlog_tail_and_final_hw_advance)'`（new，覆盖 AC1–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09e`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue sealed-seg multibatch recovery ingest and hw; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=5; scope=M; deps=RB-09e; writeset=序列化于 RB-09e; release=independent; split-from=RB-09e; exception=N/A`

---

### Task RB-09g: Capture 消费者活跃段恢复（纯已同步尾、满 256 表、0 字节）不跳段多批排空与后续追加实测（Track A，自 `RB-09f` 拆出，拆出 `RB-09h`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs`、`highwater.rs` 与 `segment.rs` 落实并端到端实证 `ADR-RB-05` #4 ② (a) 活跃段不跳段不变量：凡恢复后段 `S` 仍保持为活跃段（`S == active_seg`）的三类场景——① 纯已同步尾恢复（`recovered_valid_end == actual_len`，`> 128` 帧）、② 满 256 封存表 Step C1 原地恢复（`sealed.len() == 256`，`> 128` 帧）、③ 首帧 0 字节恢复（`recovered_valid_end == 0`）——消费者跨多批（≤ 128 帧）排空时严格将 `QUEUE_HIGHWATER_*` 推进并停于 `(S, batch_end_offset)` / `(S, committed_len)`（0 字节时停于 `(S, 0)`）绝不推进至 `(S+1, 0)`，从而确保后续向 `S` 持续追加的新帧 100% 无缝消费、零跳帧且绝不触发高水位倒退误报。

**Out of scope:**
- 活跃段排空停于 `(S, committed_len)` 后生产者再轮转 `S` 的原子零帧跨段推进、拒收守卫与崩溃恢复：已按 `G-03`/`G-09` 拆出由 `RB-09h`、`RB-09j`、`RB-09k` 承接。
- 嵌套 Case C→D（`RB-08k`）与满 256 表（`RB-08j`/`RB-08l`/`RB-08o`）的真实消费者端到端排空与回收解除背压验收：由 `RB-09i`、`RB-09l` 承接。
- 实时 Hook 与 `libra agent daemon` 接线：由 `RB-29`、`RB-29d` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 纯已同步尾、满 256 封存表与 0 字节恢复均保留 `S` 为活跃段（`S+1` 未必存在），高水位若推进至 `(S+1, 0)` 会导致后续追加到 `S` 被跳过或报高水位倒退 | `ADR-RB-05` #3 Case C、#4 ② (a) |

**Acceptance criteria:**
- [x] **纯已同步尾恢复多批排空不跳段：** 构造纯已同步尾恢复（`recovered_valid_end == actual_len`，保留 `ACTIVE_SEG == S`）且段 `S` 含 `> 128` 条未消费帧；消费者分多批（≤ 128 帧）逐批将 `QUEUE_HIGHWATER_*` 推进为 `(S, batch_end_offset)` 并最终停于 `(S, committed_len)`（绝不跳至 `(S+1, 0)`）。
- [x] **纯已同步尾排空后继续追加无缝消费：** 承接 AC1，立即向活跃段 `S` 追加新帧，消费者从 `(S, old_committed_len)` 无缝消费全部后续新帧落库（零跳帧、零高水位倒退错误）。
- [x] **满 256 表 Step C1 恢复多批排空不跳段：** 构造 `sealed.len() == 256` 且活跃段 `S` 经 Step C1 原地恢复后含 `> 128` 条未消费帧；消费者先依次排空全部 256 个已封存段（每段末批达 `sealed_len` 才跨段），再分多批排空活跃段 `S` 且最终高水位停于 `(S, committed_len)`（绝不跳至 `(S+1, 0)`）。
- [x] **满 256 表排空后继续追加无缝消费：** 承接 AC3，立即向活跃段 `S` 追加新帧，消费者 100% 无缝消费入库（零跳帧、零高水位倒退错误）。
- [x] **0 字节首帧恢复高水位保持 `(S, 0)`：** 构造首帧半截崩溃经 Step C1 截断为 0 字节（`recovered_valid_end == 0`、`ACTIVE_SEG == S`）；消费者运行后库内 `QUEUE_HIGHWATER_*` 严格保持 `(S, 0)`（绝不跳至 `(S+1, 0)`）。
- [x] **0 字节恢复后继续追加无缝消费：** 承接 AC5，立即向活跃段 `S` 追加有效新帧，消费者 100% 无缝消费入库（零跳帧、零高水位倒退错误）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_clean_tail_recovery_multibatch_keeps_active_hw_and_subsequent_append) | test(=storage::queue::writer::tests::consumer_full_256_table_recovery_multibatch_keeps_active_hw_and_subsequent_append) | test(=storage::queue::writer::tests::consumer_zero_byte_recovery_keeps_active_hw_and_subsequent_append)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_clean_tail_recovery_multibatch_keeps_active_hw_and_subsequent_append)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_full_256_table_recovery_multibatch_keeps_active_hw_and_subsequent_append)'`（new，覆盖 AC3、AC4）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_zero_byte_recovery_keeps_active_hw_and_subsequent_append)'`（new，覆盖 AC5、AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09f`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue active-seg recovery multibatch hw and append; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=5/8; landing=2; prod-files=5; scope=M; deps=RB-09f; writeset=序列化于 RB-09f; release=independent; split-from=RB-09f; exception=N/A`

---

### Task RB-09h: 已排空活跃段随后轮转的原子零帧高水位推进（双轮转路径）与推进前认证拒收守卫（Track A，自 `RB-09g` 拆出，拆出 `RB-09j`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs`、`highwater.rs` 与 `segment.rs` 落实并实证 `ADR-RB-05` #4 ② (c) 的原子零帧跨段推进与全部推进前安全守卫：消费者先排空活跃段 `S` 停于 `(S, committed_len)`、随后生产者经 **16 MiB 常规轮转**或 **Case C Step C2** 封存 `S` 切至 `S+1` 令消费者见到 `S < active_seg && hw_offset == sealed_len(S)` 时，先认证 `LTM2.sealed[S]`（逐一拦截位翻转、截断、缺失及 `hw_offset > sealed_len(S)` 越界），核验通过后在单次写事务内执行 `CaptureStore::append_capture_batch(&[], (S + 1, 0))` 推进高水位且零冗余行。

**Out of scope:**
- 零帧推进提交前/提交后镜像前崩溃恢复、跨段后 `S+1` 既有帧与后续追加帧消费及旧段 `S` 安全回收：已按 `G-03`/`G-09` 拆出由 `RB-09j`、`RB-09k` 承接。
- 嵌套 Case C→D（`RB-08k`）与满 256 表（`RB-08j`/`RB-08l`/`RB-08o`）真实消费者排空与回收解除背压：由 `RB-09i`、`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 活跃段排空停于 `(S, committed_len)` 后被生产者轮转，消费者须先逐项认证 `LTM2.sealed[S]` 再以原子零帧事务推进至 `(S+1, 0)` | `ADR-RB-05` #4 ② (c) |

**Acceptance criteria:**
- [x] **16 MiB 轮转交错原子零帧推进：** 消费排空活跃段 `S` 至 `(S, committed_len)` 后，生产者经 16 MiB 常规轮转封存 `S` 至 `S+1`；消费者认证 `LTM2.sealed[S]` 通过并执行 `CaptureStore::append_capture_batch(&[], (S + 1, 0))` 推进库内 `QUEUE_HIGHWATER_*` 至 `(S + 1, 0)`（零冗余业务行）。
- [x] **Case C Step C2 轮转交错原子零帧推进：** 消费排空活跃段 `S` 至 `(S, committed_len)` 后，生产者经 Case C Step C2 封存 `S` 至 `S+1`；消费者认证 `LTM2.sealed[S]` 通过并执行 `CaptureStore::append_capture_batch(&[], (S + 1, 0))` 推进库内 `QUEUE_HIGHWATER_*` 至 `(S + 1, 0)`（零冗余业务行）。
- [x] **已排空封存段位翻转推进前拒收：** 在 `S < active_seg && hw_offset == sealed_len(S)` 状态下翻转 `events-<S>.log` 任意字节，消费者因 `LTM2.sealed[S]` CRC32C 失配 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`，高水位零推进）。
- [x] **已排空封存段截断推进前拒收：** 在 `S < active_seg && hw_offset == sealed_len(S)` 状态下截断 `events-<S>.log`（含精确停在完整帧边界的截断），消费者因 `LTM2.sealed[S]` 长度失配 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`，高水位零推进）。
- [x] **已排空封存段缺失推进前拒收：** 在 `S < active_seg && hw_offset == sealed_len(S)` 状态下删除 `events-<S>.log`，消费者 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`，高水位零推进）。
- [x] **高水位越界 `hw_offset > sealed_len(S)` 拒收：** 若 `S < active_seg` 且库内 `hw_offset > LTM2.sealed[S].sealed_len`，消费者立即 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`，高水位零推进）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_drained_active_then_rotated_16mib_and_case_c_empty_advance) | test(=storage::queue::writer::tests::consumer_drained_rotated_bitflip_truncation_missing_and_oob_hw_rejected)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_drained_active_then_rotated_16mib_and_case_c_empty_advance)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_drained_rotated_bitflip_truncation_missing_and_oob_hw_rejected)'`（new，覆盖 AC3–AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09g`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue drained-rotated empty advance and auth guards; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=5; scope=M; deps=RB-09g; writeset=序列化于 RB-09g; release=independent; split-from=RB-09g; exception=N/A`

---

### Task RB-09j: 原子零帧推进提交前后崩溃恢复与跨段 `S+1` 既有/后续帧消费（Track A，自 `RB-09h` 拆出，拆出 `RB-09k`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs` 与 `highwater.rs` 实现并同卡验收零帧跨段推进后的消费重启恢复与 `S+1` 游标续接路径：在消费启动加载库内权威 `QUEUE_HIGHWATER_*` 自动修复落后的外部 `highwater` 镜像（含提交前幂等重跑与提交后镜像前崩溃恢复），并在零帧事务提交后立即将消费循环读游标切换至 `(S + 1, 0)` 连续消费 `S+1` 既有帧与后续新追加帧，最终活跃高水位精确停于 `(S + 1, committed_len(S+1))`。

**Out of scope:**
- 跨段高水位达到 `(S + 1, 0)` 后已排空旧段 `S` 的 `LTM2` 先更新、`unlink` 与 `fsync_dir` 逐动作安全回收：已按 `G-03`/`G-09` 拆出由 `RB-09k` 承接。
- 嵌套 Case C→D（`RB-08k`）与满 256 表（`RB-08j`/`RB-08l`/`RB-08o`）真实消费者排空与回收解除背压：由 `RB-09i`、`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 原子零帧推进提交前后崩溃须幂等收敛，跨段后须无缝消费 `S+1` 既有帧与后续追加帧并保持活跃高水位不跳段 | `ADR-RB-05` #4 ② (c) |

**Acceptance criteria:**
- [x] **零帧推进提交前崩溃幂等恢复：** 故障注入消费者在 `LTM2.sealed[S]` 认证后、`append_capture_batch(&[], (S + 1, 0))` 提交前崩溃；重启观测 `(S, sealed_len(S))` 后幂等提交 `(S + 1, 0)`。
- [x] **零帧推进提交后镜像前崩溃恢复：** 故障注入 `append_capture_batch(&[], (S + 1, 0))` 提交成功、外部 `highwater` 镜像更新前崩溃；重启以库内权威 `(S + 1, 0)` 重写外部镜像。
- [x] **跨段后消费 `S+1` 既有帧：** 零帧推进至 `(S + 1, 0)` 后，消费者将 `S+1` 中轮转时已存在的全部帧 100% 写入 `capture.redb`。
- [x] **跨段后消费 `S+1` 后续新追加帧：** 承接 AC3，向活跃段 `S+1` 新追加的后续帧被消费者 100% 写入 `capture.redb`。
- [x] **跨段消费后高水位停于活跃 `S+1`：** 承接 AC4，库内 `QUEUE_HIGHWATER_*` 精确停于 `(S + 1, committed_len(S+1))`（绝不跳至 `S + 2`）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_drained_rotated_empty_advance_crash_before_and_after_commit) | test(=storage::queue::writer::tests::consumer_drained_rotated_s1_existing_and_appended_drain)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_drained_rotated_empty_advance_crash_before_and_after_commit)'`（new，覆盖 AC1、AC2）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_drained_rotated_s1_existing_and_appended_drain)'`（new，覆盖 AC3–AC5）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09h`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue empty advance crash and s1 drain; recovery=revert isolated module; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=2; prod-files=5; scope=M; deps=RB-09h; writeset=序列化于 RB-09h; release=independent; split-from=RB-09h; exception=N/A`

---

### Task RB-09k: 原子零帧跨段后已排空旧段 `S` 的崩溃安全逐动作回收（Track A，自 `RB-09j` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs`、`highwater.rs` 与 `segment.rs` 实现并同卡验收原子零帧跨段提交后的旧段回收触发路径：当空批事务将库内 `highwater_seg` 推进至 `S + 1` 后，立即按 `writer.lock → capture-queue/append.lock` 锁序触发已排空旧段 `S` 的有序回收——先原子提交双副本 `LTM2` 推进 `MIN_LIVE_SEG = S + 1` 与摘除 `sealed[S]`，再依次执行 `unlink("events-<S>.log")` 与 `fsync_dir(lane_dir)`。

**Out of scope:**
- 嵌套 Case C→D 与满 256 表真实消费者端到端排空与回收解除背压：由 `RB-09i`、`RB-09l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 零帧跨段使 `S < highwater_seg` 成立后，回收旧段 `S` 必须严格先提交 `LTM2` 再 `unlink` 与 `fsync_dir` | `ADR-RB-05` #4 ② (c)、③ |

**Acceptance criteria:**
- [x] **回收先推进 `LTM2.MIN_LIVE_SEG`：** 库内高水位达到 `(S + 1, 0)` 后，回收在 `unlink` 前先原子提交双副本 `LTM2` 将 `MIN_LIVE_SEG` 推进至 `S + 1`。
- [x] **回收同步摘除 `LTM2.sealed[S]`：** 在同一双副本 `LTM2` 提交中将旧段 `S` 从 `LTM2.sealed[]` 摘除。
- [x] **提交 `LTM2` 后 `unlink` 旧段文件：** 双副本 `LTM2` 落盘后，回收执行 `unlink("events-<S>.log")` 删除磁盘上的旧段文件。
- [x] **`unlink` 后目录持久化：** 删除 `events-<S>.log` 后立即执行 `fsync_dir(lane_dir)` 确保删除持久化。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_drained_rotated_old_s_safe_reclaim_ordered_actions)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_drained_rotated_old_s_safe_reclaim_ordered_actions)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09j`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=capture-queue empty-advance old-s ordered reclamation; recovery=revert isolated module; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=5; scope=S; deps=RB-09j; writeset=序列化于 RB-09j; release=independent; split-from=RB-09j; exception=N/A`

---

### Task RB-09i: 消费端 `max_seg > active_seg`（Case D / 嵌套 Case C→D）恢复委托与跨段消费路径实现（Track A，自 `RB-08k` 拆出，拆出 `RB-09l`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs`、`highwater.rs` 与 `segment.rs` 实现并同卡验收 `ADR-RB-05` #4 ② 中消费端观测到 `max_seg > active_seg`（Case D / 嵌套 Case C→D，补齐 `RB-09e` 仅处理同段 `actual_len > committed_len` Case C 的真实生产路径）的恢复委托与跨段消费逻辑：消费者按 `writer.lock → capture-queue/append.lock` 持锁调用 `RB-08k` 共享 Case D 旧段尾恢复与封存例程（持锁期间绝不推进高水位、绝不写库），释放 `append.lock` 后验证 `LTM2.sealed[S]` 并依次将旧段 `S` 积压帧、恢复完整同步尾帧及后继活跃段 `S+1` 帧 100% 落库，高水位最终精确停于活跃段 `(S + 1, committed_len(S+1))`。

**Out of scope:**
- 满 256 封存表高压下消费端逐段即时回收降压与满表 Case C 全量排空实现：已拆出由 `RB-09l` 承接。
- 实时 Hook 与 `libra agent daemon` 接线：由 `RB-29`、`RB-29d` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RB-09e` 仅在 `writer.rs` 中接线同段 Case C 恢复委托；消费者观测到 `max_seg > active_seg`（含嵌套 Case C→D）时须在 `writer.rs` 独立接线 Case D 恢复委托与跨段消费闭环 | `ADR-RB-05` #3 Case D、#4 ② |

**Acceptance criteria:**
- [x] **消费端委托 Case D 后校验 `LTM2.sealed[S]` 长度通过：** 消费者检测到嵌套 Case C→D 状态并完成 `append.lock` 内 Case D 恢复委托后，校验 `actual_len(S) == sealed_len(S)` 精确通过。
- [x] **消费端校验 `LTM2.sealed[S]` CRC32C 通过：** 上述消费者校验 `crc32c(S[0..sealed_len(S)]) == sealed_crc32c(S)` 精确通过。
- [x] **旧段积压帧落库：** 上述消费者将旧段 `S` 全部未消费积压帧 `[hw_offset, committed_len)` 100% 写入 `capture.redb`。
- [x] **旧段完整同步尾帧落库：** 上述消费者将旧段 `S` 全部恢复完整同步尾帧 `[committed_len, recovered_valid_end)` 100% 写入 `capture.redb`。
- [x] **后继活跃段 `S+1` 帧落库：** 上述消费者将后继活跃段 `S+1` 全部有效帧 100% 写入 `capture.redb`。
- [x] **跨段消费后高水位停于活跃 `S+1`：** 承接 AC5，库内 `QUEUE_HIGHWATER_*` 精确停于 `(S + 1, committed_len(S+1))`（绝不跳至 `S + 2`）。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_delegates_nested_case_d_recovery_and_drains_all_frame_classes)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_delegates_nested_case_d_recovery_and_drains_all_frame_classes)'`（new，覆盖 AC1–AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09k`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=consumer max_seg case-d delegation and nested drain; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=2; prod-files=5; scope=M; deps=RB-09k; writeset=序列化于 RB-09k; release=independent; split-from=RB-08k; exception=N/A`

---

### Task RB-09l: 消费端满 256 封存表高压逐段即时回收降压与满表 Case C 全量排空实现（Track A，自 `RB-09i` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/**` 内于 `writer.rs`、`highwater.rs` 与 `segment.rs` 实现并同卡验收消费端针对 `MAX_SEALED_SEGMENTS = 256` 高压队列的逐段即时回收降压策略：当消费者观测到 `LTM2.sealed.len() == 256` 时，每完成任意单个已封存段提交（推进 `highwater_seg` 越过 `min_live_seg`）即刻触发一次 `writer.lock → append.lock` 回收释放槽位（无须等待全部 256 段排空完毕），第一时间解除生产者常规 16 MiB 轮转 `LBR-AGENT-046` 背压，并支持对满表 Case C 恢复态完成全量 `LTM2` 校验及全部 256 封存段与活跃段 `S` 落库。

**Out of scope:**
- 实时 Hook 与 `libra agent daemon` 接线：由 `RB-29`、`RB-29d` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 满 256 封存表高压下消费者须在跨过首段后即刻逐段回收释放 `sealed[]` 槽位以快速解除生产者 `LBR-AGENT-046` 轮转背压，并完整排空满表 Case C 恢复态 | `ADR-RB-05` #3 Step ②、#4 ③ |

**Acceptance criteria:**
- [x] **高压逐段即时回收释放封存槽位：** 在 `sealed.len() == 256` 触发生产者 `LBR-AGENT-046` 高压态下，消费者每跨过首个已完成封存段即刻回收释放 ≥ 1 槽位使 `LTM2.sealed.len() < 256`。
- [x] **即时降压后生产者恢复创建 `S+1`：** 承接 AC1，先前受阻的 16 MiB 轮转入队立即成功创建 `events-<S+1:08>.log`。
- [x] **即时降压后轮转登记 `LTM2.sealed[]`：** 上述 AC2 轮转提交双副本 `LTM2` 将旧段 `(S, old_committed_len, old_prefix_crc32c)` 原子登记至 `LTM2.sealed[]`。
- [x] **满表 Case C 恢复态消费端 `LTM2` 校验通过：** 承接满表 Step C1 原地恢复与段内新追加帧后，消费者高压排空循环对 `LTM2` 校验精确通过（零错误）。
- [x] **满表全部 256 个封存段落库：** 上述消费者将全部 256 个已封存段的所有帧 100% 写入 `capture.redb`。
- [x] **满表活跃段 `S`（含恢复后追加帧）落库：** 上述消费者将活跃段 `S` 全部帧（含恢复后新追加的段内帧）100% 写入 `capture.redb`。

**Verification:**
- [x] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_eager_reclaim_unblocks_256_rotation) | test(=storage::queue::writer::tests::consumer_drains_full_256_table_case_c_sealed_and_active)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_eager_reclaim_unblocks_256_rotation)'`（new，覆盖 AC1–AC3）
- [x] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=storage::queue::writer::tests::consumer_drains_full_256_table_case_c_sealed_and_active)'`（new，覆盖 AC4–AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 5 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09i`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/queue/mod.rs`、`src/storage/queue/segment.rs`、`src/storage/queue/highwater.rs`、`src/storage/queue/writer.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（2 个目录落点）。

**Docs and compatibility impact:** `N/A`（完全隔离于 `src/storage/**`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=consumer 256-cap eager reclaim and full-table drain; recovery=revert isolated module; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=5; scope=M; deps=RB-09i; writeset=序列化于 RB-09i; release=independent; split-from=RB-09i; exception=N/A`

---

### Task RB-10: 正向迁移 Phase 1 `DELETE` 模式排他静默门（外部写者与活跃读者）（Track A，拆出 `RB-10m`、`RB-10a`、`RB-10b`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `done` / `remote-verified`

**Description:** 100% 在独占目录 `src/storage/` 内实现正向迁移 Phase 1 的单连接 `DELETE` 模式排他静默前置门（`src/storage/migrate.rs`，`#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]`）：以**唯一非池化单连接 `c_mig`** 配置 `busy_timeout = 0`、`PRAGMA synchronous = FULL` 与 `PRAGMA locking_mode = EXCLUSIVE`，经无写 `BEGIN EXCLUSIVE; ROLLBACK;` 在 OS 级校验零外部 SQLite 写者与零活跃 `DELETE` 模式读事务（`SHARED` 锁）。

**Out of scope:**
- WAL 活跃读者/活跃写者/空闲连接直接切 `journal_mode=DELETE` 静默门：已按 `G-03`/`G-09` 拆出由 `RB-10m` 承接；在途 `capture-spool/tmp/` 与 `capture-queue/append.lock` 竞争拒收由 `RB-10n` 承接。
- 离线 WAL 检查点归并至独立主 `.db` 与三布局流式导出：由 `RB-10a` 承接。
- spool/queue 直灌、逻辑摘要、`VACUUM INTO` 备份与 `Derived` 拒绝：由 `RB-10i`、`RB-10f` 与 `RB-10j` 承接。
- 正向 Phase 2 回滚日志原子围栏与崩溃续跑：由 `RB-10b`..`RB-10h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 正向迁移前必须通过非池化单连接无写 `BEGIN EXCLUSIVE` 排斥活跃 SQLite 写者与 `DELETE` 活跃读者且保持源库逐字节不变 | `src/internal/db.rs:270`；`ADR-RB-07` #1 |

**Acceptance criteria:**
- [x] **外部 SQLite 写者持写事务返回 `LBR-IO-001`：** 外部进程持有源库写事务时，静默门在 `BEGIN EXCLUSIVE` 处 fail-closed 返回 `LBR-IO-001`。
- [x] **外部写者拒收源库快照零改动：** 上述 AC1 拒收前后，源 `.db` 文件逐字节快照完全相等。
- [x] **`DELETE` 模式活跃读者持 `SHARED` 锁返回 `LBR-IO-001`：** 外部进程在 `DELETE` 模式下持有活跃读事务时，无写 `BEGIN EXCLUSIVE` fail-closed 返回 `LBR-IO-001`。
- [x] **`DELETE` 模式活跃读者拒收源库快照零改动：** 上述 AC3 拒收前后，源 `.db` 文件逐字节快照完全相等。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_rejects_writer_and_delete_active_reader)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_rejects_writer_and_delete_active_reader)'`（new，覆盖 AC1–AC4）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-09l`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` `DELETE` 模式排他静默前置契约：** 仅针对 `Repository` / `Capture`；`GlobalConfig`/`SystemConfig` 零触碰；存在活跃写者或 `DELETE` 活跃读者时立即 fail-closed 且源库零改动。

**Performance budget:** 静默门无争用耗时 ≤ `50ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-1 delete-mode exclusive quiescence gate; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-09l; writeset=序列化于 RB-09l; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-10m: 正向迁移 Phase 1 WAL 三类竞争直接切 `journal_mode=DELETE` 静默门（活跃读者、活跃写者与空闲连接）（Track A，自 `RB-10` 拆出，拆出 `RB-10n`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `done` / `remote-pending`

**Description:** 100% 在独占目录 `src/storage/` 内扩展正向迁移 Phase 1 静默门（`src/storage/migrate.rs`）：当检测到源库处于 WAL 模式或存在 `-wal`/`-shm` 时，**严禁预先调用 `wal_checkpoint(TRUNCATE)`**，改为在 `busy_timeout = 0` + `locking_mode = EXCLUSIVE` 下**直接执行 `PRAGMA journal_mode = DELETE;`**（SQLite `sqlite3WalClose` 先争独占 OS+WAL 锁再做内部检查点，遇 WAL 活跃读者、**WAL 活跃写者（`BEGIN IMMEDIATE; INSERT ...`）**或外部已打开空闲 WAL 连接均立即报 `SQLITE_BUSY` 且 `.db`/`-wal`/`-shm` 100% 逐字节零改动）——将全部 3 类 WAL 竞争场景的「返回 `LBR-IO-001`」与「三文件逐字节快照零改动」拆分为 6 条 1:1 独立单谓词断言。

**Out of scope:**
- `DELETE` 模式外部写者与活跃读者排他静默门：已由前置 `RB-10` 承接。
- 在途 `capture-spool/tmp/` 与外部 `capture-queue/append.lock` 竞争拒收：已按 `G-03`/`G-09` 拆出由下游 `RB-10n` 承接。
- 离线 WAL 内部独占检查点归并至独立主 `.db` 与三布局流式导出：由 `RB-10a` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `db.rs:270` 未强制 `DELETE` 模式；WAL 下活跃读者、活跃写者或空闲连接均须由直接执行 `PRAGMA journal_mode = DELETE;` 先争独占锁立即拒收，确保 `.db`/`-wal`/`-shm` 零改动 | `src/internal/db.rs:270`；`ADR-RB-07` #1 |

**Acceptance criteria:**
- [x] **WAL 活跃读者返回 `LBR-IO-001`：** 外部进程在 WAL 模式下持有活跃读事务时，直接执行 `PRAGMA journal_mode = DELETE;` fail-closed 返回 `LBR-IO-001`。
- [x] **WAL 活跃读者拒收三文件零改动：** 上述 AC1 拒收前后，源 `.db`、`-wal` 与 `-shm` 文件逐字节快照完全相等。
- [x] **WAL 活跃写者返回 `LBR-IO-001`：** 外部进程在 WAL 模式下持有活跃写事务（`BEGIN IMMEDIATE; INSERT ...`）时，直接执行 `PRAGMA journal_mode = DELETE;` fail-closed 返回 `LBR-IO-001`。
- [x] **WAL 活跃写者拒收三文件零改动：** 上述 AC3 拒收前后，源 `.db`、`-wal` 与 `-shm` 文件逐字节快照完全相等。
- [x] **WAL 空闲连接返回 `LBR-IO-001`：** 外部进程在 WAL 模式下持有已打开空闲连接时，直接执行 `PRAGMA journal_mode = DELETE;` fail-closed 返回 `LBR-IO-001`。
- [x] **WAL 空闲连接拒收三文件零改动：** 上述 AC5 拒收前后，源 `.db`、`-wal` 与 `-shm` 文件逐字节快照完全相等（零预先检查点篡改）。

**Verification:**
- [x] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_rejects_wal_active_reader_writer_and_idle_conn)'`（new，发现门）
- [x] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_rejects_wal_active_reader_writer_and_idle_conn)'`（new，覆盖 AC1–AC6）
- [x] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` WAL 三类竞争静默契约：** 遇 WAL 活跃读者、活跃写者或空闲连接立即经 `journal_mode=DELETE` fail-closed 且三文件逐字节零改动；`GlobalConfig`/`SystemConfig` 零触碰。

**Performance budget:** 静默门无争用耗时 ≤ `50ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-1 wal reader-writer-idle direct delete gate; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=RB-10; writeset=序列化于 RB-10; release=independent; split-from=RB-10; exception=N/A`

---

### Task RB-10n: 正向迁移 Phase 1 在途 `capture-spool/tmp/` 与外部队列锁竞争拒收（Track A，自 `RB-10m` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内补齐正向迁移 Phase 1 的非 SQLite 全域静默前置门（`src/storage/migrate.rs`）：依次获取原地不动的 `capture-spool/capacity.lock` 与 `capture-queue/append.lock`，排斥在途 `capture-spool/tmp/` 发布者（模拟 `spool.rs:119-145`）与占用 `capture-queue/append.lock` 的外部生产者，将两类竞争场景的错误返回与源库、`capture-spool/`、`capture-queue/` 全部三域逐字节快照零改动逐域拆分为 8 条 1:1 独立单谓词断言。

**Out of scope:**
- `DELETE` 模式与 WAL 三类竞争 SQLite 静默门：已由前置 `RB-10` 与 `RB-10m` 承接。
- 离线 WAL 内部独占检查点归并与三布局流式导出：由下游 `RB-10a` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `Commands::Agent(_)` 不持 `MaintenanceLock`，且 `spool.rs:111-145` 在建 `tmp/` 后释放 `capacity.lock`；正向 Phase 1 必须排斥在途 spool 与活跃队列生产者且源库/spool/队列零改动 | `src/cli.rs:2535-2585`；`src/internal/ai/capture/spool.rs:111-157`；`ADR-RB-07` #1 |

**Acceptance criteria:**
- [ ] **在途 `capture-spool/tmp/` 返回 `LBR-IO-001`：** `capture-spool/tmp/` 存在未过期在途临时文件（模拟 `spool.rs:119-145`）时，静默门 fail-closed 返回 `LBR-IO-001`。
- [ ] **在途 `capture-spool/tmp/` 拒收源 SQLite 库零改动：** 上述 AC1 拒收前后，源 SQLite `.db` 文件逐字节快照完全相等。
- [ ] **在途 `capture-spool/tmp/` 拒收 `capture-spool/` 零改动：** 上述 AC1 拒收前后，`capture-spool/` 全文件逐字节快照完全相等。
- [ ] **在途 `capture-spool/tmp/` 拒收 `capture-queue/` 零改动：** 上述 AC1 拒收前后，`capture-queue/` 全文件逐字节快照完全相等。
- [ ] **外部占用 `capture-queue/append.lock` 返回 `LBR-IO-001`：** 共享 `capture-queue/append.lock` 被外部生产者占用时，静默门 fail-closed 返回 `LBR-IO-001`。
- [ ] **外部占用 `append.lock` 拒收源 SQLite 库零改动：** 上述 AC5 拒收前后，源 SQLite `.db` 文件逐字节快照完全相等。
- [ ] **外部占用 `append.lock` 拒收 `capture-queue/` 零改动：** 上述 AC5 拒收前后，`capture-queue/` 全文件逐字节快照完全相等。
- [ ] **外部占用 `append.lock` 拒收 `capture-spool/` 零改动：** 上述 AC5 拒收前后，`capture-spool/` 全文件逐字节快照完全相等。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_rejects_inflight_spool_tmp_and_queue_append_lock)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_rejects_inflight_spool_tmp_and_queue_append_lock)'`（new，覆盖 AC1–AC8）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10m`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 在途 Spool 与队列锁静默契约：** 遇在途 spool 或队列锁争用立即 fail-closed 且零改动；`GlobalConfig`/`SystemConfig` 零触碰。

**Performance budget:** 静默门无争用耗时 ≤ `50ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-1 inflight spool-tmp and queue-lock gate; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-10m; writeset=序列化于 RB-10m; release=independent; split-from=RB-10m; exception=N/A`

---

### Task RB-10a: 正向迁移 Phase 1 离线 WAL 检查点归并与三类旧库布局导出（Track A，自 `RB-10` 拆出，拆出 `RB-10i`、`RB-10f`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现正向迁移 Phase 1 的离线 WAL 无损归并与三类旧库布局导出（`src/storage/migrate.rs`）：在 `RB-10` 已锁单连接 `c_mig`（`locking_mode = EXCLUSIVE`，全程绝不对活 `.db` 路径做额外 `open()`/`close()`）下对离线 WAL 源库直接执行 `PRAGMA journal_mode = DELETE;`（通过 SQLite 内部独占检查点原子归并 WAL 帧并消除 `-wal`/`-shm`，不预先调用 `wal_checkpoint(TRUNCATE)`） 并将已提交行完整落入主 `.db`，随后只读识别布局 A（未拆）、布局 B（已拷未摘，核验 `capture_catalog_copy_receipt`）与布局 C（已拆库），将 61+25 源表无损导出至 `.tmp` 红库且保持源库逻辑完整。

**Out of scope:**
- Phase 1 只读直灌 `capture-spool/ready/` 与 `capture-queue/` 及流式批大小/堆峰值预算：已按 `G-03`/`G-09` 拆出由 `RB-10i` 承接。
- 单连接逻辑摘要、`VACUUM INTO` 锁安全备份、跨进程排他锁实证、中止无损与 `Derived` 拒绝：由 `RB-10f` 与 `RB-10j` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 旧仓可能处于离线 WAL 模式（提交行仅存于 `-wal`）且存在未拆（布局 A）、已拷未摘（布局 B）与已拆（布局 C）三类布局，须在持锁下无损归并并完整导出 | `src/internal/db.rs:270`；`src/internal/ai/capture/store.rs:51-113`；`ADR-RB-07` #1 |

**Acceptance criteria:**
- [ ] 离线 WAL 源库存在仅存于 `-wal` 文件中未刷入主 `.db` 的已提交数据时，`locking_mode=EXCLUSIVE` 下直接执行 `PRAGMA journal_mode=DELETE` 内部独占检查点并彻底消除 `-wal` 与 `-shm` 伴随文件。
- [ ] 上述离线 WAL 归并完成后，独立主 `.db` 文件包含 100% 原 `-wal` 已提交行且可独立可读。
- [ ] 布局 A（未拆库旧仓：仅 `libra.db` 含 61 活表 + 23 捕获表）完整导出至 `.libra/redb/git.gen-<id>.redb.tmp` 与 `.libra/private/capture.gen-<id>.redb.tmp`。
- [ ] 布局 B（已拷贝未摘表双库）在 `capture_catalog_copy_receipt` 完整时成功导出。
- [ ] 布局 B 在 `capture_catalog_copy_receipt` 缺失或不完整时 fail-closed 拒绝导出。
- [ ] 布局 C（现行已拆库）完整导出 61+25 源表至 `.tmp` 红库且源库零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_offline_wal_checkpoint_flushes_to_standalone_main_db) | test(=storage::migrate::tests::forward_phase1_all_three_legacy_layouts) | test(=storage::migrate::tests::forward_phase1_layout_b_incomplete_receipt_reject)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_offline_wal_checkpoint_flushes_to_standalone_main_db)'`（new，覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_all_three_legacy_layouts)'`（new，覆盖 AC3、AC4、AC6）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_layout_b_incomplete_receipt_reject)'`（new，覆盖 AC5）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10n`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 离线 WAL 归并与三布局导出契约：** `Repository`（61 活表）与 `Capture`（25 表）；离线 WAL 仅直接执行无损 `PRAGMA journal_mode=DELETE`（内部独占检查点），`GlobalConfig`/`SystemConfig`/`Derived` 绝不触碰。

**Performance budget:** 流式导出不将整表载入内存。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-1 offline-wal checkpoint and 3-layout export; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=6/8; VER=5/8; landing=1; prod-files=2; scope=M; deps=RB-10n; writeset=序列化于 RB-10n; release=independent; split-from=RB-10; exception=N/A`

---

### Task RB-10i: 正向迁移 Phase 1 只读 Spool/Queue 直灌与流式内存预算（Track A，自 `RB-10a` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `migrate.rs` 补齐正向 Phase 1 对 `capture-spool/ready/` 与 `capture-queue/` 待消费事件的只读直灌（源目录零改动），并实测流式导出单批 ≤ 500 行与额外堆内存峰值 ≤ `64 MiB` 预算门禁。

**Out of scope:**
- 单连接逻辑摘要、`VACUUM INTO` 备份与跨进程排他锁实证：由 `RB-10f` 与 `RB-10j` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 正向 Phase 1 必须在不动源 spool/queue 前提下直灌待消费事件，并严守单批 ≤ 500 行与 ≤ `64 MiB` 堆峰值 | `ADR-RB-07` #1；性能与容量摘要 |

**Acceptance criteria:**
- [ ] Phase 1 只读直灌 `capture-spool/ready/` 全部待消费事件至 `capture.gen-<id>.redb.tmp`（源 spool 文件零改动）。
- [ ] Phase 1 只读直灌 `capture-queue/` 全部待消费帧至 `capture.gen-<id>.redb.tmp`（源队列文件零改动）。
- [ ] 流式导出单批提交严格 ≤ `500` 行。
- [ ] 流式导出大仓夹具额外堆内存峰值严格 ≤ `64 MiB`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_readonly_spool_and_queue_direct_ingest) | test(=storage::migrate::tests::forward_phase1_streaming_batch_memory_budget)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_readonly_spool_and_queue_direct_ingest)'`（new，覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_streaming_batch_memory_budget)'`（new，覆盖 AC3、AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10a`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` Phase 1 直灌与流式契约：** 源 spool 与队列文件零改动；`GlobalConfig`/`SystemConfig`/`Derived` 绝不触碰。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-1 spool queue ingest stream budget; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=S; deps=RB-10a; writeset=序列化于 RB-10a; release=independent; split-from=RB-10a; exception=N/A`

---

### Task RB-10f: 正向迁移 Phase 1 单连接逻辑摘要、`VACUUM INTO` 锁安全备份与跨进程排他锁实证（Track A，自 `RB-10a` 拆出，拆出 `RB-10j`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现正向迁移 Phase 1 的零泄锁逻辑摘要、原生 `VACUUM INTO` 备份、原地 spool 备份与跨进程排他锁保持实证（`src/storage/migrate.rs`）：全程仅经已锁单连接 `c_mig` 计算 `source_logical_sha256` 并执行 `VACUUM INTO` 生成 `.libra/sqlite-backup/<gen>/{libra,capture}.db`（活 `.db` 零额外 `open()`/`close()`），并以独立 OS 子进程贯穿全程分别验证读与写均 100% 报 `SQLITE_BUSY`。

**Out of scope:**
- Phase 1 中止清理后 `DELETE`/WAL 源库不变性、`Derived` 角色拒绝与备份 `fsync` 持久性验收：已按 `G-03`/`G-09` 拆出由 `RB-10j` 承接。
- Phase 2 回滚日志原子围栏与崩溃续跑：由 `RB-10b`、`RB-10e`、`RB-10g`、`RB-10c`、`RB-10k`、`RB-10d`、`RB-10h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Unix 同进程关闭源 `.db` 任意额外 fd 会释放 SQLite POSIX 排他锁（SQLite `howtocorrupt.html`）；`libsqlite3-sys 0.30.1` 原生支持单连接 `VACUUM INTO` | `Cargo.lock:6552`；`ADR-RB-07` #1；`GC-RB-02` |

**Acceptance criteria:**
- [ ] 直接经已锁单连接 `c_mig` 流式计算确定性 `source_logical_sha256`（对活 `.db` 路径零额外 `open()`/`close()`）。
- [ ] 直接由同一已锁单连接 `c_mig` 执行 `VACUUM INTO` 生成 `.libra/sqlite-backup/<gen>/{libra,capture}.db`，并仅对独立备份新 inode 校验 `PRAGMA integrity_check == "ok"` 与 `backup_sha256`。
- [ ] 原地复制 `.libra/private/capture-spool/{ready,quarantine}/` 至 `.libra/sqlite-backup/<gen>/capture-spool/` 并校验逐字节 SHA-256（活 `capture-spool/` 与 `capacity.lock` 原地不动）。
- [ ] **跨进程读锁保持实证：** 独立 OS 子进程贯穿 Phase 1 导出、`VACUUM INTO` 备份与备份哈希全程探测对活 `libra.db`/`capture.db` 的读操作，100% 报 `SQLITE_BUSY` 阻断。
- [ ] **跨进程写锁保持实证：** 独立 OS 子进程贯穿 Phase 1 导出、`VACUUM INTO` 备份与备份哈希全程探测对活 `libra.db`/`capture.db` 的写操作，100% 报 `SQLITE_BUSY` 阻断。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_lock_safe_vacuum_backup_and_inplace_spool_backup) | test(=storage::migrate::tests::forward_phase1_external_os_process_blocked_through_backup_hash)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_lock_safe_vacuum_backup_and_inplace_spool_backup)'`（new，覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_external_os_process_blocked_through_backup_hash)'`（new，覆盖 AC4、AC5）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10i`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 零泄锁备份契约：** 全程经单连接 `c_mig` 计算逻辑摘要并执行 `VACUUM INTO` 与原地 spool 备份，独立 OS 子进程实证全程无泄锁窗口。

**Performance budget:** 单连接 `VACUUM INTO` 备份与哈希不额外增加活库锁争用，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-1 digest vacuum backup os-lock; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-10i; writeset=序列化于 RB-10i; release=independent; split-from=RB-10a; exception=N/A`

---

### Task RB-10j: 正向迁移 Phase 1 备份 `fsync`、中止源不变性与 `Derived` 角色拒绝（Track A，自 `RB-10f` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `migrate.rs` 补齐正向 Phase 1 备份 `fsync` 持久性核验、校验失败中止清理后 `DELETE` 与离线 WAL 源库无损不变性以及 `DatabaseRole::Derived` 显式 fail-closed 拒绝。

**Out of scope:**
- Phase 2 回滚日志原子围栏与崩溃续跑：由 `RB-10b`..`RB-10h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `GC-RB-02` 明确禁止迁移 `DatabaseRole::Derived`；Phase 1 失败中止必须清理临时产物且绝不损伤源库 | `GC-RB-02`；`ADR-RB-07` #1 |

**Acceptance criteria:**
- [ ] Phase 1 生成的 `.libra/sqlite-backup/<gen>/{libra,capture}.db` 与 `capture-spool/` 备份全部完成文件 `fsync` 与目录 `fsync_dir`。
- [ ] 校验不符触发 Phase 1 中止并清理 `.tmp`/`.gen-*`/备份目录后，`DELETE` 源库逐字节未变。
- [ ] 离线 WAL 源库触发 Phase 1 中止并清理 `.tmp`/`.gen-*`/备份目录后，主 `.db` 逻辑数据 100% 完整可读。
- [ ] 迁移入口对 `DatabaseRole::Derived` 显式 fail-closed 拒绝。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_verified_backup_abort_leaves_delete_and_wal_sources_intact) | test(=storage::migrate::tests::forward_phase1_rejects_derived_role)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_verified_backup_abort_leaves_delete_and_wal_sources_intact)'`（new，覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase1_rejects_derived_role)'`（new，覆盖 AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10f`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 中止无损与 `Derived` 拒绝契约：** 显式拒绝 `Derived`，Phase 1 中止后源库与 spool 100% 完好。

**Performance budget:** Phase 1 中止清理耗时 ≤ `100ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-1 backup fsync abort and derived-rej; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=S; deps=RB-10f; writeset=序列化于 RB-10f; release=independent; split-from=RB-10f; exception=N/A`

---

### Task RB-10b: 正向迁移 Phase 2（100% SQLite 回滚日志原子围栏与 post-unlink 进度持久化）（Track A，自 `RB-10` 拆出，拆出 `RB-10l`、`RB-10e`、`RB-10c`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现正向迁移 Phase 2 的 SQLite 回滚日志原子围栏与 `phase2_progress` 摘除状态机（`src/storage/migrate.rs`）：完成 `ADR-RB-07` #1 两阶段清单发布（`finalized = false` 含每源 `phase2_progress = "pending"` 与摘要）、在同一已锁单连接 `c_mig` 上执行 **100% SQLite `synchronous = FULL` 回滚日志原子围栏事务**（`PRAGMA writable_schema = ON` 写入 `sqlite_master` 失效哨兵 `__libra_migrated_fence__` 并递增 `schema_version` → `COMMIT`，零裸字节写、抗断电扇区撕裂），并依次持久化 `fenced_ready_to_unlink`、持锁 `unlink` + `fsync_dir` 与持久化 `unlinked`。

**Out of scope:**
- 独立 OS 子进程贯穿 Phase 1 至 Phase 2 `unlink` 读/写独占实测与新起旧版 SQLite 二进制拒访：已按 `G-03`/`G-09` 拆出由 `RB-10l` 承接。
- 迁移前已打开旧 SQLite 连接的逐语句（冷/热 `SELECT`/`sqlite_master`/`INSERT`/`UPDATE`/`DELETE`/DDL/`VACUUM`）拒访：由 `RB-10e` 承接。
- 不移锁原地 `capture-spool` 墓碑封锁、终态 `finalized = true` 发布与崩溃续跑：由 `RB-10g`、`RB-10c`、`RB-10k`、`RB-10d`、`RB-10h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| SQLite `synchronous = FULL` 回滚日志在非原子扇区写设备上仍保证 100% 断电原子提交，且逐库 `fenced_ready_to_unlink → unlink + fsync_dir → unlinked` 为合法缺席态恢复提供确定性检查点 | `ADR-RB-07` #1 Step 2.2a/2.2b |

**Acceptance criteria:**
- [ ] Phase 2 仅在目标红库与 `VACUUM INTO` 备份均完成 `fsync` 后，在持锁下原子发布 `.libra/storage-format.json`（`backend = "redb", finalized = false`，含每源 `phase2_progress = "pending"` 与完整摘要）。
- [ ] 在同一持续持 `locking_mode = EXCLUSIVE` 的单连接 `c_mig` 上完成 `synchronous = FULL` 回滚日志原子围栏 `__libra_migrated_fence__` 事务提交（递增 `schema_version`）。
- [ ] 围栏提交后先原子持久化 `phase2_progress.<db> = "fenced_ready_to_unlink"` + `fsync(.libra/)`。
- [ ] 在持锁下执行源库 `unlink` 并立即执行父目录 `fsync_dir`。
- [ ] 完成 `unlink` + `fsync_dir` 后原子持久化 `phase2_progress.<db> = "unlinked"` + `fsync(.libra/)`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_journaled_fence_and_post_unlink_progress)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_journaled_fence_and_post_unlink_progress)'`（new，覆盖 AC1–AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`（确认单特性 SQLite 零受扰）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10j`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 正向 Phase 2 回滚日志围栏契约：** 经单一非池化 `c_mig` 完成 `synchronous=FULL` 回滚日志原子围栏 `__libra_migrated_fence__`（零裸字节写、零 POSIX 泄锁）与 `phase2_progress` 摘除持久化。

**Performance budget:** Phase 2 围栏切换在本地 SSD 上耗时 ≤ `500ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-2 journaled fence and post-unlink progress; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-10j; writeset=序列化于 RB-10j; release=independent; split-from=RB-10; exception=N/A`

---

### Task RB-10l: 正向迁移 Phase 2 跨进程读写独占直至 `unlink` 与新起旧版二进制拒访实测（Track A，自 `RB-10b` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `src/storage/migrate.rs` 实证单连接 `c_mig`（`locking_mode = EXCLUSIVE`、零额外 fd 打开关闭）贯穿自 Phase 1 锁获取直至 Phase 2 `unlink` 全程阻断独立 OS 子进程对活 `libra.db`/`capture.db` 的读与写操作，以及 `unlink` 后新起旧版 SQLite 二进制立即 fail-closed。

**Out of scope:**
- 预打开旧连接 8 类 SQL 语句逐一拒访：由 `RB-10e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 必须在独立 OS 子进程中实证自 Phase 1 至 Phase 2 `unlink` 全程无 POSIX `fcntl` 泄锁空窗 | `ADR-RB-07` #1 Step 2.2 |

**Acceptance criteria:**
- [ ] 独立 OS 子进程贯穿自 Phase 1 锁获取直至 Phase 2 `unlink` 完成的全程持续探测对活 `libra.db`/`capture.db` 的读操作，100% 报 `SQLITE_BUSY` 阻断。
- [ ] 独立 OS 子进程贯穿自 Phase 1 锁获取直至 Phase 2 `unlink` 完成的全程持续探测对活 `libra.db`/`capture.db` 的写操作，100% 报 `SQLITE_BUSY` 阻断。
- [ ] 围栏并 `unlink` 后，新起旧版 SQLite 二进制因 `.libra/libra.db` 缺失立即 fail-closed。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_external_os_exclusivity_and_fresh_legacy_reject)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_external_os_exclusivity_and_fresh_legacy_reject)'`（new，覆盖 AC1–AC3）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 正向 Phase 2 排他性验证契约：** 全程零泄锁阻断外部 OS 读/写，且 `unlink` 后新起旧二进制立即 fail-closed。

**Performance budget:** 跨进程探针检测总耗时 ≤ `500ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-2 os read/write exclusivity and legacy reject; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=3/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-10b; writeset=序列化于 RB-10b; release=independent; split-from=RB-10b; exception=N/A`

---

### Task RB-10e: 正向迁移 Phase 2（预打开 SQLite 连接逐语句读/写/DDL/`VACUUM` 拒访）（Track A，自 `RB-10b` 拆出，拆出 `RB-10g`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `src/storage/migrate.rs` 逐语句实证 `__libra_migrated_fence__` 围栏提交后，迁移前已打开的旧 SQLite 连接发起 8 类独立 SQL 语句（冷连接 `SELECT`、热连接 `SELECT`、`sqlite_master` `SELECT`、`INSERT`、`UPDATE`、`DELETE`、`CREATE TABLE IF NOT EXISTS` 与 `VACUUM`）全部立即报 `SQLITE_CORRUPT` fail-closed。

**Out of scope:**
- 不移锁原地 `capture-spool` 墓碑封锁、终态 `finalized = true` 发布与 `GC-13` 双域隔离：已按 `G-03`/`G-09` 拆出由 `RB-10g` 承接。
- Phase 2 崩溃恢复与故障注入：由 `RB-10c`、`RB-10d`、`RB-10h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `agent_bridge/publication.rs:22,36` 在旧连接上按需执行 `CREATE TABLE IF NOT EXISTS`；预打开连接在围栏提交后须逐语句拦截冷/热读、结构读、DML、DDL 与 `VACUUM` | `src/internal/ai/agent_bridge/publication.rs:22,36`；`ADR-RB-07` #1 |

**Acceptance criteria:**
- [ ] 围栏提交后，迁移前已打开的**未查过（冷连接）**旧 SQLite 连接发起业务表 `SELECT * FROM reference` 立即报 `SQLITE_CORRUPT`（`malformed database schema`）fail-closed。
- [ ] 围栏提交后，迁移前已打开的**页缓存已热（热连接）**旧 SQLite 连接发起业务表 `SELECT * FROM reference` 立即报 `SQLITE_CORRUPT`（`malformed database schema`）fail-closed。
- [ ] 围栏提交后，迁移前已打开旧 SQLite 连接发起 `SELECT name, sql FROM sqlite_master` 结构查询立即报 `SQLITE_CORRUPT` fail-closed。
- [ ] 围栏提交后，迁移前已打开旧 SQLite 连接执行 `INSERT` 立即报 `SQLITE_CORRUPT` fail-closed。
- [ ] 围栏提交后，迁移前已打开旧 SQLite 连接执行 `UPDATE` 立即报 `SQLITE_CORRUPT` fail-closed。
- [ ] 围栏提交后，迁移前已打开旧 SQLite 连接执行 `DELETE` 立即报 `SQLITE_CORRUPT` fail-closed。
- [ ] 围栏提交后，迁移前已打开旧 SQLite 连接执行 `CREATE TABLE IF NOT EXISTS agent_bridge_publication(...)` 立即报 `SQLITE_CORRUPT` fail-closed。
- [ ] 围栏提交后，迁移前已打开旧 SQLite 连接执行 `VACUUM` 立即报 `SQLITE_CORRUPT` fail-closed。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_fence_blocks_cold_warm_table_select_and_sqlite_master) | test(=storage::migrate::tests::forward_phase2_fence_blocks_open_handle_dml_ddl_and_vacuum)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_fence_blocks_cold_warm_table_select_and_sqlite_master)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_fence_blocks_open_handle_dml_ddl_and_vacuum)'`（new，覆盖 AC4–AC8）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`（确认单特性 SQLite 零受扰）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10l`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 预打开句柄围栏契约：** 逐语句实证 `__libra_migrated_fence__` 阻断预打开旧连接的冷/热 `SELECT`、`sqlite_master`、`INSERT`/`UPDATE`/`DELETE`、`CREATE TABLE IF NOT EXISTS` 与 `VACUUM`，杜绝句柄分叉。

**Performance budget:** 单次围栏与预打开连接拒访判定耗时 ≤ `200ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-2 open-handle sql stmt rejection; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=8/8; VER=5/8; landing=1; prod-files=2; scope=M; deps=RB-10l; writeset=序列化于 RB-10l; release=independent; split-from=RB-10b; exception=N/A`

---

### Task RB-10g: 正向迁移 Phase 2（不移锁原地 Spool 墓碑、终态发布与 `GC-13` 双域隔离）（Track A，自 `RB-10e` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内完成正向迁移 Phase 2 的原地 `capture-spool` 墓碑封锁、终态清单发布与配置隔离（`src/storage/migrate.rs`）：在原地未动的 `.libra/private/capture-spool/capacity.lock` 下将 `{tmp,ready,quarantine}` 替换为 `0400` 普通文件墓碑 + `fsync_dir`（分别实证先前阻塞于 `spool.rs:112` 与新起于 `spool.rs:105` 的旧 Hook 立即 fail-closed），原子发布 `.libra/storage-format.json(finalized = true)` 并验证 `GC-13` 全局与系统配置双域隔离。

**Out of scope:**
- Phase 2 崩溃恢复与故障注入：由 `RB-10c`、`RB-10d`、`RB-10h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `spool.rs:105-157,330-355` 依赖不移锁 `capacity.lock` 与三目录 `!is_dir()` 拒绝；`GC-13` 要求仓库迁移绝不触碰隔离全局与系统配置库 | `src/internal/ai/capture/spool.rs:105-157,330-355`；`ADR-RB-07` #1；`GC-13` |

**Acceptance criteria:**
- [ ] 在原地未动的 `.libra/private/capture-spool/capacity.lock` 下清除已备份文件并将 `capture-spool/{tmp,ready,quarantine}` 替换为 `0400` 普通文件墓碑 + `fsync_dir`，先前阻塞于 `spool.rs:112` 的旧 Hook 获锁后立即 fail-closed。
- [ ] 上述 `0400` 墓碑就位后，新起于 `spool.rs:105` 的旧 Hook 在 `ensure_spool` 命中 `!is_dir()` 立即 fail-closed。
- [ ] 原子更新 `.libra/storage-format.json`（`backend = "redb", finalized = true`）并 `fsync_dir(.libra)` 后释放全部锁。
- [ ] `GC-13` 全局隔离：正向仓库迁移绝不打开、创建或修改隔离夹具 `LIBRA_CONFIG_GLOBAL_DB`。
- [ ] `GC-13` 系统隔离：正向仓库迁移绝不打开、创建或修改隔离夹具 `LIBRA_CONFIG_SYSTEM_DB`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_unmoved_lock_inplace_spool_tombstones_block_concurrent_hooks) | test(=storage::migrate::tests::forward_phase2_gc13_config_isolation)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_unmoved_lock_inplace_spool_tombstones_block_concurrent_hooks)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_gc13_config_isolation)'`（new，覆盖 AC4、AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`（确认单特性 SQLite 零受扰）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10e`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 原地墓碑与终态发布契约：** 覆盖不移锁 `capacity.lock` 原地 `0400` 墓碑封锁与 `finalized = true` 终态落盘，切后无损反向恢复由 `RB-11`..`RB-11l` 提供；`GlobalConfig`/`SystemConfig` 严格隔离。

**Performance budget:** Phase 2 墓碑替换与终态落盘在本地 SSD 上耗时 ≤ `200ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-2 spool tombstones finalize gc13; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=1; prod-files=2; scope=M; deps=RB-10e; writeset=序列化于 RB-10e; release=independent; split-from=RB-10e; exception=N/A`

---

### Task RB-10c: 正向迁移 Phase 2 环境变量阻断、断电撕裂扇区热日志回滚与围栏前漂移恢复（Track A，自 `RB-10b` 拆出，拆出 `RB-10k`、`RB-10d`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现正向迁移 Phase 2 的前半程崩溃恢复路径（`src/storage/migrate.rs`）：逐项验证 `finalized == false` 下 `redb`/`sqlite` 双环境变量覆盖阻断、断电撕裂扇区热回滚日志（`libra.db-journal`）自动无损回滚续跑，以及围栏前崩溃后旧连接新写入触发 `source_logical_sha256` 漂移重导至 `gen+1`。

**Out of scope:**
- 围栏 `COMMIT` 后 `unlink` 前哨兵核验及紧接 `unlink(libra.db)`/`unlink(capture.db)` 后合法缺席态恢复：已按 `G-03`/`G-09` 拆出由 `RB-10k` 承接。
- `pending` 意外缺失/非哨兵损坏/备份缺损拒收与跨源/spool/queue 漂移对账：由 `RB-10d` 与 `RB-10h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 必须在断电撕裂页 1 扇区（热回滚日志在盘）与围栏前崩溃漂移重导场景下实证零数据丢失 | `ADR-RB-01` #3、`ADR-RB-07` #1 |

**Acceptance criteria:**
- [ ] 每个正向 `finalized == false` 崩溃中断点传入 `LIBRA_STORAGE_BACKEND=redb` 覆盖无条件 fail-closed 返回 `LBR-IO-001`。
- [ ] 每个正向 `finalized == false` 崩溃中断点传入 `LIBRA_STORAGE_BACKEND=sqlite` 覆盖无条件 fail-closed 返回 `LBR-IO-001`。
- [ ] **断电撕裂扇区热日志恢复实测：** 模拟围栏事务提交中途断电（保留热 `libra.db-journal` 且破坏页 1 首个 512 字节扇区），续跑触发 SQLite 热回滚日志自动无损回滚至洁净前置态，核对 `source_logical_sha256` 后完成围栏至 `finalized = true`。
- [ ] **围栏前崩溃漂移重导实测：** `phase2_progress == "pending"` 崩溃且旧连接事后写入新行，续跑检出 `source_logical_sha256` 漂移并从活库重导至 `gen+1` 刷新备份后再围栏，崩溃后新写入 100% 保留。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_powerloss_torn_sector_hot_journal_rollback_and_resume) | test(=storage::migrate::tests::forward_phase2_crash_before_fence_drift_reexports_gen_plus_1)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_powerloss_torn_sector_hot_journal_rollback_and_resume)'`（new，覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_crash_before_fence_drift_reexports_gen_plus_1)'`（new，覆盖 AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10g`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 正向热日志与围栏前漂移续跑契约：** 抗断电撕裂扇区并零丢失保留围栏前崩溃后的旧连接新写入。

**Performance budget:** 续跑恢复与重导保持 ≤ 500 行/批、额外堆峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-2 hot-journal and pre-fence drift; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-10g; writeset=序列化于 RB-10g; release=independent; split-from=RB-10b; exception=N/A`

---

### Task RB-10k: 正向迁移 Phase 2 围栏后未摘除与双源各自 `unlink` 后合法缺席态恢复（Track A，自 `RB-10c` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `migrate.rs` 实现正向迁移 Phase 2 的围栏后与双源独立 `unlink` 崩溃续跑：覆盖围栏 `COMMIT` 后 `unlink` 前通过 `PRAGMA writable_schema = ON` 精确核验 `__libra_migrated_fence__` 哨兵与备份 `backup_sha256` 后 `unlink` + `fsync_dir`，以及紧接 `unlink(libra.db)+fsync_dir` 后与紧接 `unlink(capture.db)+fsync_dir` 后两类独立合法缺席态备份认证收敛。

**Out of scope:**
- `pending` 意外缺失/非哨兵损坏/备份缺损拒收与跨源/spool/queue 漂移对账：由 `RB-10d` 与 `RB-10h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `libra.db` 与 `capture.db` 各自 `unlink` 后的缺席态为两个独立持久化状态，须结合 `phase2_progress` 与 `backup_sha256` 分别实测 | `ADR-RB-07` #1 |

**Acceptance criteria:**
- [ ] 围栏 `COMMIT` 后、`unlink` 前崩溃：续跑通过 `PRAGMA writable_schema = ON` 精确核验 `sqlite_master` 存在 `name='__libra_migrated_fence__'` 且 `sql='LIBRA_MIGRATED_TO_REDB_FENCE_SYNTAX_ERROR'` 与备份 `backup_sha256` 后完成 `unlink` 收敛至 `finalized = true`。
- [ ] 上述围栏后 `unlink` 完成后立即执行 `fsync_dir` 确保摘除持久落盘。
- [ ] **紧接 `unlink(libra.db) + fsync_dir` 后崩溃实测：** `phase2_progress.git_db ∈ {"fenced_ready_to_unlink", "unlinked"}` 且 `finalized == false` 时，续跑校验 `libra.db` 备份 `backup_sha256` + `PRAGMA integrity_check` 后幂等收敛至 `finalized = true`。
- [ ] **紧接 `unlink(capture.db) + fsync_dir` 后崩溃实测：** `phase2_progress.capture_db ∈ {"fenced_ready_to_unlink", "unlinked"}` 且 `finalized == false` 时，续跑校验 `capture.db` 备份 `backup_sha256` + `PRAGMA integrity_check` 后幂等收敛至 `finalized = true`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_crash_after_fence_before_unlink) | test(=storage::migrate::tests::forward_phase2_crash_immediately_after_libra_db_unlink) | test(=storage::migrate::tests::forward_phase2_crash_immediately_after_capture_db_unlink)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_crash_after_fence_before_unlink)'`（new，覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_crash_immediately_after_libra_db_unlink)'`（new，覆盖 AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_crash_immediately_after_capture_db_unlink)'`（new，覆盖 AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10c`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 正向双源 post-unlink 续跑契约：** 分别实测 `libra.db` 与 `capture.db` 摘除后的备份认证收敛，零数据丢失。

**Performance budget:** 续跑恢复耗时 ≤ `500ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-2 post-fence and each post-unlink; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=5/8; landing=1; prod-files=2; scope=M; deps=RB-10c; writeset=序列化于 RB-10c; release=independent; split-from=RB-10c; exception=N/A`

---

### Task RB-10d: 正向迁移 Phase 2 `pending` 意外缺席、非哨兵损坏与备份缺损逐项拒收（Track A，自 `RB-10c` 拆出，拆出 `RB-10h`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现正向迁移 Phase 2 的全量 fail-closed 故障拒收（`src/storage/migrate.rs`）：逐项拦截 `pending` 态 `libra.db` 与 `capture.db` 各自意外缺失、受损页头与非 `__libra_migrated_fence__` 精确哨兵的 schema 损坏，以及已摘除源对应 `.libra/sqlite-backup/<gen>/` 备份文件缺失或 `backup_sha256` 失配。

**Out of scope:**
- 双库围栏间跨源漂移、`capture-spool` 墓碑间隙并集对账与 `capture-queue` 漂移重导：已按 `G-03`/`G-09` 拆出由 `RB-10h` 承接。
- 反向迁移（`redb → SQLite`）：由 `RB-11`..`RB-11h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `pending` 态双源意外缺失、页头/非哨兵 schema 异常损坏及已摘除源备份缺失或摘要失配必须逐项 fail-closed 返回 `LBR-IO-001` | `ADR-RB-07` #1 |

**Acceptance criteria:**
- [ ] 当 `phase2_progress.git_db == "pending"` 而 `libra.db` 意外缺失（非合法已摘除态）时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 当 `phase2_progress.capture_db == "pending"` 而 `capture.db` 意外缺失（非合法已摘除态）时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 当源 `.db` 页头受损（非热日志可回滚态且无 `__libra_migrated_fence__` 精确哨兵）时，续跑立即 fail-closed 返回 `LBR-IO-001`，绝不静默丢弃源数据。
- [ ] 当源 `.db` 存在非 `__libra_migrated_fence__` 精确哨兵的异常 schema 损坏时，续跑立即 fail-closed 返回 `LBR-IO-001`，绝不静默丢弃源数据。
- [ ] 当已合法摘除源对应的 `.libra/sqlite-backup/<gen>/` 备份文件缺失时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 当已合法摘除源对应的 `.libra/sqlite-backup/<gen>/` 备份文件 `backup_sha256` 失配时，续跑立即 fail-closed 返回 `LBR-IO-001`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_unexpected_missing_pending_and_corrupt_non_sentinel_reject) | test(=storage::migrate::tests::forward_phase2_post_unlink_corrupt_backup_fails_closed)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_unexpected_missing_pending_and_corrupt_non_sentinel_reject)'`（new，覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_post_unlink_corrupt_backup_fails_closed)'`（new，覆盖 AC5、AC6）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10k`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 正向故障拦截契约：** 逐项拦截双源 `pending` 意外缺席、页头与非哨兵 schema 损坏及备份缺失/摘要失配，严禁静默丢数。

**Performance budget:** 故障检测与 fail-closed 返回耗时 ≤ `100ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-2 missing non-sentinel backup reject; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-10k; writeset=序列化于 RB-10k; release=independent; split-from=RB-10c; exception=N/A`

---

### Task RB-10h: 正向迁移 Phase 2 跨源漂移、Spool 墓碑间隙并集与 Queue 漂移对账（Track A，自 `RB-10d` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现正向迁移 Phase 2 的多源组合漂移对账（`src/storage/migrate.rs`）：处理 `libra.db` 已合法摘除后 `capture.db` 漂移的备份+活库合并重导、`capture-spool` 子目录移除与 `0400` 墓碑安装间隙崩溃（含无并发 Hook 重建及并发旧 Hook 重建并写入新已确认事件的**备份 ∪ 活目录**并集去重重导），以及 `capture-queue/` 清理前追加新帧的漂移并入。

**Out of scope:**
- 反向迁移（`redb → SQLite`）：由 `RB-11`..`RB-11h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `libra.db` 已摘除而 `capture.db`/`capture-spool`/`capture-queue` 发生崩溃后新写入时，续跑须结合已验备份与活源合并重导至 `gen+1` | `ADR-RB-07` #1 |

**Acceptance criteria:**
- [ ] `libra.db` 已围栏并 `unlink` 后、`capture.db` 围栏前崩溃且旧进程写入 `capture.db`：续跑以已验 `libra.db` 备份 + 活 `capture.db` 重导至 `gen+1` 并完成全部围栏，零数据丢失。
- [ ] 移除 `capture-spool` 子目录与安装 `0400` 墓碑之间的间隙崩溃（无并发旧 Hook 写入）：续跑重获原地 `capacity.lock`，核验备份 `.libra/sqlite-backup/<gen>/capture-spool/` 后装齐 3 个 `0400` 墓碑并收敛至 `finalized = true`。
- [ ] 移除 `capture-spool` 子目录与安装 `0400` 墓碑之间的间隙崩溃且并发旧 Hook 重建目录写入新已确认事件：续跑重获原地 `capacity.lock`，对**备份 `.libra/sqlite-backup/<gen>/capture-spool/` ∪ 活 `capture-spool/ready/`** 按去重键取并集重导至 `gen+1` 并装齐 3 个 `0400` 墓碑，新旧事件 100% 零丢失。
- [ ] `capture-queue/` 清理前崩溃且生产者追加新帧：续跑检出 `queue_manifest_sha256` 漂移，将最新队列帧并入 `gen+1` 重导至 `finalized = true`，零丢帧。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_crash_capture_db_and_queue_post_crash_reexport) | test(=storage::migrate::tests::forward_phase2_spool_tombstone_gap_crash_and_concurrent_hook_reconcile)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_crash_capture_db_and_queue_post_crash_reexport)'`（new，覆盖 AC1、AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::forward_phase2_spool_tombstone_gap_crash_and_concurrent_hook_reconcile)'`（new，覆盖 AC2、AC3）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 多源漂移对账契约：** 覆盖已摘除 `libra.db` 与活 `capture.db`/spool/queue 组合漂移重导，保证零数据丢失与 `GlobalConfig`/`SystemConfig` 严格隔离。

**Performance budget:** 续跑恢复与重导保持 ≤ 500 行/批、额外堆峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=forward phase-2 cross-source spool and queue gap; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-10d; writeset=序列化于 RB-10d; release=independent; split-from=RB-10d; exception=N/A`

---

### Task RB-11: 对称反向迁移 Phase 1 红库写者静默门与全局锁序（Track A，拆出 `RB-11z`、`RB-11q`、`RB-11i`、`RB-11d`、`RB-11a`、`RB-11b`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现对称反向迁移 Phase 1 的严格全局锁序（`migration.lock` → `maintenance.lock` → `capture-spool-consumer.lock` → `writer.lock` → 原地未动 `capture-queue/append.lock` → 原地未动 `capture-spool/capacity.lock` → `(git_wtx, cap_wtx)`）与外部红库写者（`git.redb`/`capture.redb`）静默排斥门（`src/storage/migrate.rs`），将错误码返回与源红库逐字节快照零改动拆为 5 条 1:1 独立单谓词断言。

**Out of scope:**
- 外部队列生产者占用 `capture-queue/append.lock` 与在途 `capture-spool/tmp/` 竞争拒收：已按 `G-03`/`G-09` 拆出由 `RB-11z` 承接。
- `capture-spool-consumer.lock` 排斥并发既有 `drain_storage` 与非空 `capture-spool/quarantine/` fail-closed 零删损保护：由 `RB-11q` 承接。
- 反向 Phase 1 单趟导出、未对账意图直灌、`PRAGMA application_id` 打标与校验备份：由 `RB-11i` 与 `RB-11p` 承接。
- 反向 Phase 2 与配置双向迁移：由 `RB-11d`..`RB-11t` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 反向迁移必须在明确全局锁序下排斥活跃 `git.redb`/`capture.redb` 写者且保持源红库逐字节不变 | `ADR-RB-07` #2 |

**Acceptance criteria:**
- [ ] **外部 `git.redb` 写者返回 `LBR-IO-001`：** 外部红库写者持有 `git.redb` 写事务时，反向静默门 fail-closed 返回 `LBR-IO-001`。
- [ ] **外部 `git.redb` 写者拒收源库零改动：** 上述 AC1 拒收前后，源 `git.redb`/`capture.redb` 逐字节快照完全相等。
- [ ] **外部 `capture.redb` 写者返回 `LBR-IO-001`：** 外部红库写者持有 `capture.redb` 写事务时，反向静默门 fail-closed 返回 `LBR-IO-001`。
- [ ] **外部 `capture.redb` 写者拒收源库零改动：** 上述 AC3 拒收前后，源 `git.redb`/`capture.redb` 逐字节快照完全相等。
- [ ] **严格全局锁序契约：** 反向迁移严格按明确锁序 `migration.lock → maintenance.lock → capture-spool-consumer.lock → writer.lock → capture-queue/append.lock → capture-spool/capacity.lock → (git_wtx, cap_wtx)` 获取锁。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_quiescence_rejects_redb_writers_and_enforces_lock_order)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_quiescence_rejects_redb_writers_and_enforces_lock_order)'`（new，覆盖 AC1–AC5）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-10h`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向 Phase 1 锁序与红库静默契约：** 遇活跃红库写者立即退出，源红库 100% 完好；`GlobalConfig`/`SystemConfig` 严格隔离。

**Performance budget:** 反向静默门无争用耗时 ≤ `50ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-1 redb-writer quiescence and lock-order; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=RB-10h; writeset=序列化于 RB-10h; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-11z: 对称反向迁移 Phase 1 队列锁与在途 `capture-spool/tmp/` 竞争拒收（Track A，自 `RB-11` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内扩展反向迁移 Phase 1 静默门（`src/storage/migrate.rs`），排斥外部队列生产者占用原地 `capture-queue/append.lock` 与在途 `capture-spool/tmp/` 发布者，将两类竞争场景的错误码返回与源红库、`capture-queue/`、`capture-spool/` 全部三域逐字节快照零改动逐域拆分为 8 条 1:1 独立单谓词断言。

**Out of scope:**
- 全局锁序与外部 `git.redb`/`capture.redb` 写者排斥：已由 `RB-11` 承接。
- `capture-spool-consumer.lock` 排斥并发 `drain_storage` 与非空 `quarantine/` fail-closed：由 `RB-11q` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 反向迁移必须排斥活跃队列生产者与在途 `spool.rs:119-145` 发布者且拒收时保持源红库、队列与 spool 逐字节不变 | `src/internal/ai/capture/spool.rs:111-157`；`ADR-RB-07` #2 |

**Acceptance criteria:**
- [ ] **外部占用 `capture-queue/append.lock` 返回 `LBR-IO-001`：** 外部队列生产者占有原地 `capture-queue/append.lock` 时，反向静默门 fail-closed 返回 `LBR-IO-001`。
- [ ] **外部占用 `append.lock` 拒收源红库零改动：** 上述 AC1 拒收前后，源红库（`git.redb`/`capture.redb`）逐字节快照完全相等。
- [ ] **外部占用 `append.lock` 拒收 `capture-queue/` 零改动：** 上述 AC1 拒收前后，`capture-queue/` 全文件逐字节快照完全相等。
- [ ] **外部占用 `append.lock` 拒收 `capture-spool/` 零改动：** 上述 AC1 拒收前后，`capture-spool/` 全文件逐字节快照完全相等。
- [ ] **在途 `capture-spool/tmp/` 返回 `LBR-IO-001`：** `capture-spool/tmp/` 存在未过期在途临时文件时，反向静默门 fail-closed 返回 `LBR-IO-001`。
- [ ] **在途 `capture-spool/tmp/` 拒收源红库零改动：** 上述 AC5 拒收前后，源红库（`git.redb`/`capture.redb`）逐字节快照完全相等。
- [ ] **在途 `capture-spool/tmp/` 拒收 `capture-spool/` 零改动：** 上述 AC5 拒收前后，`capture-spool/` 全文件逐字节快照完全相等。
- [ ] **在途 `capture-spool/tmp/` 拒收 `capture-queue/` 零改动：** 上述 AC5 拒收前后，`capture-queue/` 全文件逐字节快照完全相等。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_rejects_queue_producer_and_inflight_spool_tmp)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_rejects_queue_producer_and_inflight_spool_tmp)'`（new，覆盖 AC1–AC8）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向队列与在途 Spool 静默契约：** 遇队列锁争用或在途 spool 临时文件立即 fail-closed 且零改动；`GlobalConfig`/`SystemConfig` 零触碰。

**Performance budget:** 反向静默门无争用耗时 ≤ `50ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-1 queue-lock and inflight-spool-tmp gate; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-11; writeset=序列化于 RB-11; release=independent; split-from=RB-11; exception=N/A`

---

### Task RB-11q: 对称反向迁移 Phase 1 `capture-spool-consumer.lock` 排除并发 `drain` 与非空 `quarantine/` 保护（Track A，自 `RB-11` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `migrate.rs` 落实并实证反向迁移对 `capture-spool-consumer.lock`（`src/internal/ai/capture/drain.rs:87` `CONSUMER_LOCK`）的全程持有与并发 `drain_storage` 互斥，以及 `capture-spool/quarantine/` 非空时的无条件 fail-closed 零删损保护。

**Out of scope:**
- 反向 Phase 1 单趟导出、意图直灌与备份：由 `RB-11i` 与 `RB-11p` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 既有 spool 消费者 `drain.rs:87-139` 仅通过 `capture-spool-consumer.lock`（`CONSUMER_LOCK`）互斥并将不可用事件自 `ready/` 移至 `quarantine/`；反向迁移必须持该消费者锁并对非空 `quarantine/` fail-closed | `src/internal/ai/capture/drain.rs:85-143`；`ADR-RB-07` #2 |

**Acceptance criteria:**
- [ ] **消费者锁占用返回 `LBR-IO-001`：** 既有 spool 消费者持有 `capture-spool-consumer.lock`（`drain.rs:87`）时，反向静默门 fail-closed 返回 `LBR-IO-001`。
- [ ] **消费者锁拒收源红库零改动：** 上述 AC1 拒收前后，源红库（`git.redb`/`capture.redb`）逐字节快照完全相等。
- [ ] **消费者锁拒收 `capture-spool/` 零改动：** 上述 AC1 拒收前后，`capture-spool/` 全文件逐字节快照完全相等。
- [ ] **并发 `drain` 锁排除回归实测：** 反向迁移持有 `capture-spool-consumer.lock` 期间，并发调用 `drain_storage` 立即感知锁被占用并零移动退出（绝不把 `ready/` 文件移入 `quarantine/`）。
- [ ] **`quarantine/` 非空 fail-closed 拒收：** `capture-spool/quarantine/` 存在任意隔离事件文件时，反向 Phase 1 立即 fail-closed 返回 `LBR-IO-001`。
- [ ] **`quarantine/` 文件 100% 完好留存：** 上述 fail-closed 返回后，`capture-spool/quarantine/` 内全部隔离文件逐字节完好留存（绝不静默清空）。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_consumer_lock_excludes_concurrent_drain_and_rejects_quarantine)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_consumer_lock_excludes_concurrent_drain_and_rejects_quarantine)'`（new，覆盖 AC1–AC6）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11z`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 消费者锁与隔离保护契约：** 持 `capture-spool-consumer.lock` 排斥并发 `drain` 且对非空 `quarantine/` fail-closed 零删损。

**Performance budget:** 锁探测与目录检查耗时 ≤ `20ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-1 consumer-lock drain quarantine; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-11z; writeset=序列化于 RB-11z; release=independent; split-from=RB-11; exception=N/A`

---

### Task RB-11i: 对称反向迁移 Phase 1 无死锁单趟导出、意图直灌与 `PRAGMA application_id` 打标（Track A，自 `RB-11` 拆出，拆出 `RB-11p`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现对称反向迁移 Phase 1 流式导出引擎（`src/storage/migrate.rs`）：在 `RB-11` 已持全部静默锁与 `(git_wtx, cap_wtx)`（严格 `Git → Capture` 序，全程不提交也不嵌套重开写事务，彻底杜绝死锁）内单趟流式导出 61+25 源表至 `libra.db.tmp`/`capture.db.tmp`，直读未对账 `META_CROSS_STORE_INTENT`（`gen > cap_gen`）按升序幂等直灌并只读并入 `capture-queue/{priority,normal}/` 待消费帧，分别向 `capture.db.tmp` 与 `libra.db.tmp` 写入唯一非零 `PRAGMA application_id = target_migration_tag` 并满足单批/堆内存预算。

**Out of scope:**
- 冻结红库与队列 SHA-256 备份、`.tmp` `integrity_check` 与 Phase 1 中止源不变性：已按 `G-03`/`G-09` 拆出由 `RB-11p` 承接。
- 反向 Phase 2 红库围栏、墓碑、非覆写 `hard_link` 发布与崩溃恢复：由 `RB-11d`、`RB-11j`、`RB-11l`、`RB-11a`、`RB-11m`、`RB-11e`、`RB-11h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 反向导出须在持双 `WriteTransaction` 时零死锁直灌半提交意图并分别向两枚 `.tmp` 库写入 `PRAGMA application_id = target_migration_tag` | `ADR-RB-06`、`ADR-RB-07` #2 |

**Acceptance criteria:**
- [ ] 在已持 `(git_wtx, cap_wtx)` 内零嵌套写事务单趟导出全部 61+25 源表与切后新提交至 `libra.db.tmp`/`capture.db.tmp`。
- [ ] 真实半提交意图反向导出：从 `git_wtx` 直读全部未对账 `META_CROSS_STORE_INTENT`（`gen > cap_gen`）按严格升序幂等直灌入 `capture.db.tmp`（零死锁）。
- [ ] 只读直灌 `capture-queue/{priority,normal}/` 待消费帧至 `capture.db.tmp`（源队列文件零改动）。
- [ ] `fsync` 前向 `capture.db.tmp` 写入唯一非零 `PRAGMA application_id = target_migration_tag`。
- [ ] `fsync` 前向 `libra.db.tmp` 写入同一唯一非零 `PRAGMA application_id = target_migration_tag`。
- [ ] 反向流式导出维持单批 ≤ 500 行且额外堆内存峰值 ≤ `64 MiB`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_replays_real_half_committed_intent_deadlock_free) | test(=storage::migrate::tests::reverse_phase1_stamps_application_id_and_ingests_queue_readonly)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_replays_real_half_committed_intent_deadlock_free)'`（new，覆盖 AC1、AC2、AC6）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_stamps_application_id_and_ingests_queue_readonly)'`（new，覆盖 AC3、AC4、AC5）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11q`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向 Phase 1 导出契约：** 单趟持锁无死锁导出 61+25 源表、未对账 `META_CROSS_STORE_INTENT` 与队列帧并打 `PRAGMA application_id` 标记；`GlobalConfig`/`SystemConfig` 严格隔离。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-1 export intent queue and app-id; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11q; writeset=序列化于 RB-11q; release=independent; split-from=RB-11; exception=N/A`

---

### Task RB-11p: 对称反向迁移 Phase 1 红库/队列校验备份、`.tmp` 完整性校验与中止源不变性（Track A，自 `RB-11i` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内完成对称反向迁移 Phase 1 备份与中止不变性（`src/storage/migrate.rs`）：对产出的 `libra.db.tmp`/`capture.db.tmp` 执行 `PRAGMA integrity_check == "ok"` 与 `fsync`，将冻结中的 `git.redb`/`capture.redb` 与 `capture-queue/{priority,normal,quarantine}/`（保留原目录与 `append.lock` 原地不动）拷贝至 `.libra/redb-backup/<gen>/` 完成 SHA-256 校验与 `fsync`，并实测 Phase 1 中止后源红库与 `capture-queue/` 各自逐字节未变。

**Out of scope:**
- 反向 Phase 2 红库围栏与墓碑化：由 `RB-11d` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 反向 Phase 1 须在提交任何围栏前产出经 SHA-256 校验的不可变 `.libra/redb-backup/<gen>/` 并在中止时确保两类源均逐字节不变 | `ADR-RB-07` #2 Phase 1 |

**Acceptance criteria:**
- [ ] `libra.db.tmp` 与 `capture.db.tmp` 导出后均通过 `PRAGMA integrity_check == "ok"` 并完成 `fsync`。
- [ ] 冻结中的 `git.redb`/`capture.redb` 与 `capture-queue/{priority,normal,quarantine}/`（保留原目录与 `append.lock` 原地不动）备份至 `.libra/redb-backup/<gen>/` 并完成 SHA-256 校验与 `fsync`。
- [ ] Phase 1 校验中止并丢弃 `.tmp` 与未提交写事务后，源 `git.redb` 与 `capture.redb` 逐字节未变。
- [ ] Phase 1 校验中止并丢弃 `.tmp` 与未提交写事务后，源 `capture-queue/` 逐字节未变。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_tmp_integrity_and_verified_redb_queue_backup) | test(=storage::migrate::tests::reverse_phase1_abort_preserves_source_redb_and_queue_byte_identical)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_tmp_integrity_and_verified_redb_queue_backup)'`（new，覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase1_abort_preserves_source_redb_and_queue_byte_identical)'`（new，覆盖 AC3、AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11i`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向 Phase 1 备份与中止契约：** 校验备份与 `.tmp` 完整后才可进入 Phase 2；中止零改源红库与队列；`GlobalConfig`/`SystemConfig` 严格隔离。

**Performance budget:** 备份吞吐受本机磁盘带宽约束，额外堆峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-1 redb-backup integrity and abort; recovery=clean tmp sources intact; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11i; writeset=序列化于 RB-11i; release=independent; split-from=RB-11i; exception=N/A`

---

### Task RB-11d: 对称反向迁移 Phase 2 清单发布、红库 `META_SCHEMA` 围栏与双库 post-unlink 持久化（Track A，自 `RB-11` 拆出，拆出 `RB-11x`、`RB-11j`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现对称反向迁移 Phase 2 的 Step 2.1–2.2（`src/storage/migrate.rs`）：持全部静默锁在目标 `.tmp` 与 `redb-backup` 均 `fsync` 后发布 `storage-format.json(backend="sqlite", finalized=false, spool_unfenced=false, targets_staged_for_rename=false, target_migration_tag, phase2_progress, source_digests)`，向旧 `git.redb` 与 `capture.redb` 提交 `META_SCHEMA min_reader_version = u32::MAX` 围栏物理阻断预打开红库句柄，并逐库持久化 `fenced_ready_to_unlink -> unlink + fsync_dir -> unlinked`。

**Out of scope:**
- 原地未动 `capture-queue/append.lock` 下队列 `0400` 墓碑与原地 `capture-spool` 主动 `0400` 墓碑（Step 2.3）：已按 `G-03`/`G-09` 拆出由 `RB-11x` 承接。
- 非覆写 `hard_link` 先发 `capture.db` 后发 `libra.db`（Step 2.4）与终态/Step 2.6 解封：由 `RB-11j` 与 `RB-11l` 承接。
- 反向 Phase 2 崩溃续跑与恢复：由 `RB-11a`、`RB-11r`、`RB-11m`、`RB-11e`、`RB-11s`、`RB-11h`、`RB-11u` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 反向 Phase 2 须先通过 `META_SCHEMA u32::MAX` 阻断预打开红库句柄并逐库持久化 `fenced_ready_to_unlink -> unlinked` | `ADR-RB-03`；`ADR-RB-07` #2 Step 2.1–2.2 |

**Acceptance criteria:**
- [ ] Phase 2 仅在目标 `.tmp` 与 `redb-backup` 均 `fsync` 后发布 `storage-format.json(backend="sqlite", finalized=false, spool_unfenced=false, targets_staged_for_rename=false, target_migration_tag, phase2_progress, source_digests)`。
- [ ] 向 `git.redb` 与 `capture.redb` 提交 `META_SCHEMA min_reader_version = u32::MAX` 围栏后，迁移前已打开的 `Redb*Store` 句柄下一读写立即报 `RepositoryMigrated` fail-closed。
- [ ] 对 `git.redb` 按顺序持久推进 `fenced_ready_to_unlink` → `unlink` + `fsync_dir` → `unlinked`。
- [ ] 对 `capture.redb` 按顺序持久推进 `fenced_ready_to_unlink` → `unlink` + `fsync_dir` → `unlinked`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_fences_open_redb_handles_and_persists_post_unlink)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_fences_open_redb_handles_and_persists_post_unlink)'`（new，覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`（确认单特性 SQLite 零受扰）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11p`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向 Phase 2 红库围栏契约：** 通过 `META_SCHEMA u32::MAX` 阻断预打开红库句柄并持久化双库 post-unlink 校验点；`GlobalConfig`/`SystemConfig` 严格隔离。

**Performance budget:** Phase 2 红库围栏与摘除耗时 ≤ `150ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 redb fence and post-unlink progress; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11p; writeset=序列化于 RB-11p; release=independent; split-from=RB-11; exception=N/A`

---

### Task RB-11x: 对称反向迁移 Phase 2 原地 `capture-queue` 与主动 `capture-spool` `0400` 墓碑及双 Hook 阻断（Track A，自 `RB-11d` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现对称反向迁移 Phase 2 Step 2.3 的双墓碑确立（`src/storage/migrate.rs`）：在原地未动的 `capture-queue/append.lock` 下清除已备份段并将 `capture-queue/{priority,normal,quarantine}` 原地替换为 `0400` 普通文件墓碑 + `fsync_dir`，同时在原地未动的 `capture-spool-consumer.lock` + `capture-spool/capacity.lock` 下主动核验并确立 `capture-spool/{tmp,ready,quarantine}` 全部 3 个 `0400` 普通文件墓碑 + `fsync_dir`（覆盖旧迁移仓与全新 Redb 仓），跨 `finalized = false` 同时阻断红库 Hook 与旧 SQLite Hook。

**Out of scope:**
- 非覆写 `hard_link` 先发 `capture.db` 后发 `libra.db`（Step 2.4）与终态/Step 2.6 解封：由 `RB-11j` 与 `RB-11l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 反向 Phase 2 须在原地锁下装齐 `capture-queue` 与 `capture-spool` 全部 `0400` 墓碑跨越 `finalized = false`，彻底封锁红库 Hook 与旧 SQLite Hook | `src/internal/ai/capture/spool.rs:330-355`；`ADR-RB-07` #2 Step 2.3 |

**Acceptance criteria:**
- [ ] 在原地未动的 `capture-queue/append.lock` 下清除已备份段并将 `capture-queue/{priority,normal,quarantine}` 原地替换为 `0400` 普通文件墓碑 + `fsync_dir`。
- [ ] `capture-queue` `0400` 墓碑就位后，并发红库 Hook 入队立即因 `ENOTDIR` fail-closed。
- [ ] 在原地未动的 `capture-spool-consumer.lock` + `capture-spool/capacity.lock` 下对旧迁移仓核验并保持 `capture-spool/{tmp,ready,quarantine}` 全部 3 个 `0400` 普通文件墓碑 + `fsync_dir`。
- [ ] 在原地未动的 `capture-spool-consumer.lock` + `capture-spool/capacity.lock` 下对全新 Redb 仓主动确立 `capture-spool/{tmp,ready,quarantine}` 全部 3 个 `0400` 普通文件墓碑 + `fsync_dir`。
- [ ] 上述 `capture-spool` `0400` 墓碑跨越 `finalized = false` 期间，并发旧 SQLite Hook 均在 `ensure_spool` 立即 fail-closed。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_inplace_queue_and_active_spool_tombstones_block_hooks)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_inplace_queue_and_active_spool_tombstones_block_hooks)'`（new，覆盖 AC1–AC5）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向 Phase 2 双墓碑契约：** 不移锁原地 `capture-queue` 与主动 `capture-spool` 墓碑跨越 `finalized = false` 封锁双侧 Hook；`GlobalConfig`/`SystemConfig` 严格隔离。

**Performance budget:** Phase 2 双墓碑确立耗时 ≤ `150ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 inplace queue and active spool tombstones; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-11d; writeset=序列化于 RB-11d; release=independent; split-from=RB-11d; exception=N/A`

---

### Task RB-11j: 反向 Phase 2 非覆写 `hard_link` 先发 `capture.db` 后发 `libra.db` 与不支持/异源拒收（Track A，自 `RB-11d` 拆出，拆出 `RB-11y`、`RB-11l`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现对称反向迁移 Phase 2 Step 2.4 目标非覆写发布核心（`src/storage/migrate.rs`）：持久化 `targets_staged_for_rename = true` + `fsync(.libra/)` 后，统一采用**绝不覆写目标的 `std::fs::hard_link`** 先发布 `.libra/private/capture.db`、再发布 `.libra/libra.db`（严禁正常路径无条件覆盖型 `rename`，底层不支持硬链返回 `Unsupported`/`EXDEV` 立即 fail-closed `LBR-IO-001`），且 `libra.db` 遇异源 `application_id` 立即 fail-closed `LBR-IO-001` 绝不覆写。

**Out of scope:**
- 正常 Step 2.4 命中 `AlreadyExists` 的无崩溃竞争 `capture::store::open` 单连接并轨、墓碑遵守、围栏与原子替换：已按 `G-03`/`G-09` 拆出由 `RB-11y` 承接。
- 终态 `finalized = true` 发布、Step 2.6 `spool_unfenced` 解封、旧 Hook 恢复与 `GC-13` 配置隔离：由 `RB-11l` 承接。
- 崩溃中断后的双红库 post-unlink、`targets_staged_for_rename` 崩溃恢复与 Step 2.6 崩溃恢复：由 `RB-11a`..`RB-11u` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 正常 Step 2.4 必须统一先用非覆写 `std::fs::hard_link` 先发 `capture.db` 再发 `libra.db`，不支持硬链与异源库均 fail-closed | `src/internal/ai/capture/store.rs:65-86`；`ADR-RB-07` #2 Step 2.4 |

**Acceptance criteria:**
- [ ] 进入 Step 2.4 前先原子持久化 `targets_staged_for_rename = true` 并执行 `fsync(.libra/)`。
- [ ] 正常 Step 2.4 经非覆写 `std::fs::hard_link("capture.db.tmp", ".libra/private/capture.db")` + `unlink("capture.db.tmp")` + `fsync_dir` 率先发布 `capture.db`。
- [ ] 若底层文件系统对 `hard_link` 返回 `Unsupported`/`ENOSYS`/`EXDEV`，立即 fail-closed 返回 `LBR-IO-001`（绝不降级为覆盖型 `rename`）。
- [ ] 正常 Step 2.4 随后经非覆写 `std::fs::hard_link("libra.db.tmp", ".libra/libra.db")` + `unlink("libra.db.tmp")` + `fsync_dir` 发布 `libra.db`。
- [ ] 若 `libra.db` `hard_link` 命中 `AlreadyExists` 且现存库 `PRAGMA application_id != target_migration_tag`，立即 fail-closed 返回 `LBR-IO-001`（绝不覆写异源库）。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_step24_hardlink_capture_then_libra_and_unsupported_reject) | test(=storage::migrate::tests::reverse_phase2_step24_libra_db_foreign_tag_reject)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_step24_hardlink_capture_then_libra_and_unsupported_reject)'`（new，覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_step24_libra_db_foreign_tag_reject)'`（new，覆盖 AC5）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11x`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向非覆写发布契约：** Step 2.4 统一使用非覆写 `std::fs::hard_link` 先发 `capture.db` 后发 `libra.db`，不支持硬链与异源库均 fail-closed。

**Performance budget:** Step 2.4 正常路径耗时 ≤ `150ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 hardlink publish and foreign-tag reject; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11x; writeset=序列化于 RB-11x; release=independent; split-from=RB-11d; exception=N/A`

---

### Task RB-11y: 反向 Phase 2 正常 Step 2.4 无崩溃竞争 `capture::store::open` 并轨围栏替换实测（Track A，自 `RB-11j` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `src/storage/migrate.rs` 实现并实证正常 Step 2.4 无崩溃竞态闭环：不注入崩溃，在 staging 开始后、正常 Step 2.4 执行前由外部旧进程调用 `capture::store::open` 建出 `application_id == 0` 竞争 `capture.db` 并提交事件；正常 Step 2.4 命中 `hard_link` `AlreadyExists` 后立即转入单连接 `c_mig` 排他并轨（遵守 `agent_import_tombstone`）+ `__libra_migrated_fence__` 围栏竞争库 + 原子替换，确保迁移捕获与竞争旧写入双 100% 保留。

**Out of scope:**
- 终态发布与 Step 2.6 解封：由 `RB-11l` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 即使无崩溃，外部旧进程亦可于正常 Step 2.4 前经 `capture::store::open` 建出 `application_id == 0` 竞争 `capture.db`；正常路径须与恢复路径共享同一无损并轨围栏协议 | `src/internal/ai/capture/store.rs:65-86`；`ADR-RB-07` #2 Step 2.4 |

**Acceptance criteria:**
- [ ] 无崩溃下正常 Step 2.4 `hard_link("capture.db.tmp", ".libra/private/capture.db")` 命中 `AlreadyExists` 且现存库 `application_id == 0` 时，转入单连接 `c_mig` 排他并轨将其合法事件合入 `capture.db.tmp`。
- [ ] 上述并轨严格遵守 `agent_import_tombstone` 拒重导语义，绝不复活已删记录。
- [ ] 并轨完成后在同一单连接 `c_mig` 上对竞争 `capture.db` 提交 `__libra_migrated_fence__` 回滚日志原子围栏。
- [ ] 围栏竞争库后原子 `rename(capture.db.tmp, capture.db)` + `fsync_dir` 完成替换。
- [ ] 替换完成后原迁移捕获数据与无崩溃竞争旧写入双 100% 完整可读。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_normal_step24_hardlink_reconciles_no_crash_competing_capture_open)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_normal_step24_hardlink_reconciles_no_crash_competing_capture_open)'`（new，覆盖 AC1–AC5）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11j`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 无崩溃竞争并轨契约：** 正常 Step 2.4 命中无崩溃竞争 `capture::store::open` 自动无损并轨、遵守墓碑、围栏竞争库并原子替换。

**Performance budget:** 单连接并轨与替换耗时 ≤ `150ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse step24 no-crash competing capture.db merge fence; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-11j; writeset=序列化于 RB-11j; release=independent; split-from=RB-11j; exception=N/A`

---

### Task RB-11l: 反向 Phase 2 终态发布、Step 2.6 Spool 解封、旧 Hook 恢复与 `GC-13` 双域隔离（Track A，自 `RB-11j` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现对称反向迁移 Phase 2 Step 2.5–2.6 收口与配置隔离验收（`src/storage/migrate.rs`）：在 `capture.db` 与 `libra.db` 均就位且仍持 `migration.lock`、`capture-spool-consumer.lock` 与 `capacity.lock` 下原子发布 `storage-format.json(backend="sqlite", finalized=true, spool_unfenced=false)`，随后于 Step 2.6 仅 `unlink` `capture-spool/{tmp,ready,quarantine}` 的 `is_file()` 墓碑 + `fsync_dir` 并持久化 `spool_unfenced = true` 再释锁，验证旧 SQLite Hook 恢复正常入队，并分别断言 `GlobalConfig` 与 `SystemConfig` 夹具零改动。

**Out of scope:**
- 反向崩溃恢复与 Step 2.6 两类可达解封崩溃态：由 `RB-11a`、`RB-11m`、`RB-11e`、`RB-11h` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Spool 墓碑只能在 `finalized = true` 落盘之后解封并持久化 `spool_unfenced = true`，且仓库反向迁移须独立验证 `GlobalConfig` 与 `SystemConfig` 两份可变配置库零改动（`GC-13`） | `ADR-RB-07` #2 Step 2.5–2.6；`GC-13` |

**Acceptance criteria:**
- [ ] 两库就位后在仍持 `migration.lock`、`capture-spool-consumer.lock` 与 `capacity.lock` 下原子发布 `storage-format.json(backend="sqlite", finalized=true, spool_unfenced=false)` + `fsync_dir(.libra)`。
- [ ] **终态后解封 Spool（Step 2.6）：** 仅在 `finalized = true` 落盘后、释锁前，仅 `unlink` `capture-spool/{tmp,ready,quarantine}` 的 `symlink_metadata().is_file()` 普通文件墓碑 + `fsync_dir`，并持久化 `spool_unfenced = true`。
- [ ] Step 2.6 释锁后，旧 SQLite Hook 恢复正常向 `capture-spool/ready/` 入队。
- [ ] `GC-13` 全局隔离：反向仓库迁移绝不打开、创建或修改隔离夹具 `LIBRA_CONFIG_GLOBAL_DB`。
- [ ] `GC-13` 系统隔离：反向仓库迁移绝不打开、创建或修改隔离夹具 `LIBRA_CONFIG_SYSTEM_DB`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_step25_26_finalize_unfence_and_legacy_hook_resume) | test(=storage::migrate::tests::reverse_phase2_gc13_global_and_system_config_isolation)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_step25_26_finalize_unfence_and_legacy_hook_resume)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_gc13_global_and_system_config_isolation)'`（new，覆盖 AC4、AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`（确认单特性 SQLite 零受扰）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11y`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向终态解封与配置隔离契约：** 仅在 `finalized = true` 落盘后安全移除 `is_file()` spool 墓碑并持久化 `spool_unfenced = true`，同时分别验证 `GlobalConfig` 与 `SystemConfig` 零改动。

**Performance budget:** Step 2.5–2.6 正常路径耗时 ≤ `100ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 finalize step26 unfence gc13; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=1; prod-files=2; scope=M; deps=RB-11y; writeset=序列化于 RB-11y; release=independent; split-from=RB-11j; exception=N/A`

---

### Task RB-11a: 反向迁移 Phase 2 环境变量阻断、零外写幂等续跑与围栏前红库漂移重导（Track A，自 `RB-11` 拆出，拆出 `RB-11r`、`RB-11e`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现反向迁移 Phase 2 的前半程红库源状态机恢复（`src/storage/migrate.rs`）：验证 `finalized == false` 下 `redb`/`sqlite` 双环境变量覆盖阻断、零外写摘要一致幂等续跑，以及 `git.redb` 围栏前崩溃且外部继续写入新提交时的 `git_redb_sha256` 漂移检测与无损重导。

**Out of scope:**
- 紧接 `unlink(git.redb)`/`unlink(capture.redb)` 后合法缺席态恢复与双源 `pending` 意外缺失拒收：已按 `G-03`/`G-09` 拆出由 `RB-11r` 承接。
- 反向备份失配拒收、`capture-queue` 墓碑间隙并集、`targets_staged_for_rename` 与 Step 2.6 恢复：由 `RB-11m`、`RB-11e`、`RB-11s`、`RB-11h`、`RB-11u` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 反向 `finalized == false` 必须阻断环境变量覆盖并在围栏前红库写入漂移时作废旧 `.tmp` 重导最新提交 | `ADR-RB-01` #3、`ADR-RB-07` #2 |

**Acceptance criteria:**
- [ ] 每个反向 `finalized == false` 崩溃中断点传入 `LIBRA_STORAGE_BACKEND=redb` 覆盖无条件 fail-closed 返回 `LBR-IO-001`。
- [ ] 每个反向 `finalized == false` 崩溃中断点传入 `LIBRA_STORAGE_BACKEND=sqlite` 覆盖无条件 fail-closed 返回 `LBR-IO-001`。
- [ ] 零外部写入时重跑反向迁移核对 `source_digests` 一致后幂等收敛至 `finalized = true`。
- [ ] `finalized = false` 后、`git.redb` 围栏前崩溃且已打开红库进程向 `git.redb` 写入新提交：续跑检出 `git_redb_sha256` 漂移，作废过时 `.tmp` SQLite 重导最新数据再围栏发布，零数据丢失。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_crash_override_reject_and_idempotent_resume) | test(=storage::migrate::tests::reverse_phase2_crash_before_fence_git_redb_drift_reexports)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_crash_override_reject_and_idempotent_resume)'`（new，覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_crash_before_fence_git_redb_drift_reexports)'`（new，覆盖 AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11l`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向围栏前漂移续跑契约：** 阻断环境变量旁路并零丢失保留围栏前崩溃后的红库新提交。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 overrides idempotent and drift; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11l; writeset=序列化于 RB-11l; release=independent; split-from=RB-11; exception=N/A`

---

### Task RB-11r: 反向迁移 Phase 2 双红库各自 `unlink` 后恢复与 `pending` 意外缺席拒收（Track A，自 `RB-11a` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `migrate.rs` 实现反向迁移 Phase 2 双红库独立 post-unlink 崩溃续跑与 `pending` 意外缺席拦截：分别覆盖紧接 `unlink(git.redb)+fsync_dir` 后崩溃（校验 `git.redb` 备份 SHA-256 并合并活 `capture.redb` 新写重导）、紧接 `unlink(capture.redb)+fsync_dir` 后崩溃（校验双份 `redb-backup` 真实 SHA-256 收敛），以及 `git.redb`/`capture.redb` 各自处于 `pending` 态意外缺失时的 fail-closed 拒收。

**Out of scope:**
- 备份失配拒收、`capture-queue` 墓碑间隙、`targets_staged_for_rename` 与 Step 2.6 恢复：由 `RB-11m`、`RB-11e`、`RB-11s`、`RB-11h`、`RB-11u` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `unlink(git.redb)` 与 `unlink(capture.redb)` 属于两个独立持久化状态，必须分别注入崩溃并结合 `redb-backup` SHA-256 认证与 `pending` 意外缺失拒收逐项实测 | `ADR-RB-07` #2 |

**Acceptance criteria:**
- [ ] **紧接 `unlink(git.redb) + fsync_dir` 后崩溃实测：** `git_redb` 已合法摘除而 `capture.redb` 仍存活且有外部新事件写入，续跑校验 `git.redb` 备份真实 SHA-256 并合并活 `capture.redb` 重导，零数据丢失。
- [ ] **紧接 `unlink(capture.redb) + fsync_dir` 后崩溃实测：** `git.redb` 与 `capture.redb` 两源均已按 `phase2_progress ∈ {"fenced_ready_to_unlink", "unlinked"}` 合法摘除，续跑校验两份 `redb-backup` 真实 SHA-256 后幂等收敛至 `finalized = true`。
- [ ] 当 `phase2_progress.git_redb == "pending"` 而 `git.redb` 意外缺失时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 当 `phase2_progress.cap_redb == "pending"` 而 `capture.redb` 意外缺失时，续跑立即 fail-closed 返回 `LBR-IO-001`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_crash_after_git_redb_unlink_merges_live_capture) | test(=storage::migrate::tests::reverse_phase2_crash_immediately_after_capture_redb_unlink) | test(=storage::migrate::tests::reverse_phase2_unexpected_missing_pending_redb_fails_closed)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_crash_after_git_redb_unlink_merges_live_capture)'`（new，覆盖 AC1）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_crash_immediately_after_capture_redb_unlink)'`（new，覆盖 AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_unexpected_missing_pending_redb_fails_closed)'`（new，覆盖 AC3、AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11a`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向双源 post-unlink 续跑契约：** 分别实测 `git.redb` 与 `capture.redb` 摘除后的真实 SHA-256 备份认证及两源 `pending` 意外缺失拒收。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 both post-unlink and pending miss; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=5/8; landing=1; prod-files=2; scope=M; deps=RB-11a; writeset=序列化于 RB-11a; release=independent; split-from=RB-11a; exception=N/A`

---

### Task RB-11m: 反向迁移 Phase 2 备份缺损拒收、队列墓碑间隙并集与异源 `libra.db` 拒收（Track A，自 `RB-11e` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现反向迁移 Phase 2 故障拒绝与队列间隙恢复（`src/storage/migrate.rs`）：分别实测已摘除源对应 `.libra/redb-backup/<gen>/` 缺失、SHA-256 失配时的 fail-closed `LBR-IO-001` 拦截、`capture-queue` 移除子目录与装 `0400` 墓碑间隙崩溃的备份∪活队列语义去重并集重导，以及 `targets_staged_for_rename = true` 下现存异源 `libra.db`（`PRAGMA application_id != target_migration_tag`）的 fail-closed 拒收。

**Out of scope:**
- `targets_staged_for_rename = true` 下 `capture.db` 发布后 `AlreadyExists`/`NotFound` 续跑与竞争 `capture::store::open` 并轨：由 `RB-11e` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 反向恢复须分别阻断备份缺失、备份篡改与异源 `libra.db` 覆写，并在队列墓碑间隙按备份∪活目录取并集实现零丢帧 | `ADR-RB-07` #2 |

**Acceptance criteria:**
- [ ] 已合法摘除源红库对应的 `.libra/redb-backup/<gen>/` 备份缺失时，反向续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 已合法摘除源红库对应的 `.libra/redb-backup/<gen>/` 备份 SHA-256 失配时，反向续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 移除 `capture-queue` 子目录与安装 `0400` 墓碑之间的间隙崩溃（并发红库 Hook 追加新帧）：续跑重获原地 `append.lock`，对**备份 `.libra/redb-backup/<gen>/capture-queue/` ∪ 活 `capture-queue/{priority,normal}/`** 按语义去重键取并集重导并装齐 3 个 `0400` 墓碑，新旧帧 100% 零丢失。
- [ ] `targets_staged_for_rename = true` 时若现存 `libra.db` 的 `PRAGMA application_id != target_migration_tag`，续跑立即 fail-closed 返回 `LBR-IO-001`（绝不覆写）。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_corrupt_or_missing_backup_and_queue_tombstone_gap_union) | test(=storage::migrate::tests::reverse_phase2_foreign_libra_db_tag_mismatch_fails_closed)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_corrupt_or_missing_backup_and_queue_tombstone_gap_union)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_foreign_libra_db_tag_mismatch_fails_closed)'`（new，覆盖 AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11r`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向备份认证与队列间隙并集契约：** 缺失/失配备份与异源 `libra.db` 均 fail-closed，队列墓碑间隙按备份∪活目录零丢帧收敛。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 backup-reject queue-gap foreign; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11r; writeset=序列化于 RB-11r; release=independent; split-from=RB-11e; exception=N/A`

---

### Task RB-11e: 反向迁移 Phase 2 `targets_staged_for_rename` 同 tag 非覆写 `hard_link` 崩溃续跑（Track A，自 `RB-11a` 拆出，拆出 `RB-11m`、`RB-11s`、`RB-11h`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现反向迁移 Phase 2 `targets_staged_for_rename = true` 中同迁移 tag（`PRAGMA application_id == target_migration_tag`）已发布目标的非覆写 `hard_link` 崩溃续跑（`src/storage/migrate.rs`）：分别覆写验证 `AlreadyExists` 同 inode 残留清理 `.tmp`、`NotFound` 单库已发补齐 `libra.db` 与双库已发零覆写推进 `finalized = true`，100% 保留崩溃后旧 SQLite 进程新写入。

**Out of scope:**
- 崩溃后竞争 `capture::store::open` 建出 `application_id == 0` 竞争 `capture.db` 的单连接并轨、围栏与替换后崩溃续跑：已按 `G-03`/`G-09` 拆出由 `RB-11s` 承接。
- 全新 Redb 仓活 spool 对账与两类可达 Step 2.6 解封崩溃恢复：由 `RB-11h` 与 `RB-11u` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 恢复路径复用 Step 2.4 非覆写 `hard_link`（含 `AlreadyExists`/`NotFound`）+ `PRAGMA application_id` 协议，绝不覆写已对旧进程可见的同 tag 目标库 | `ADR-RB-07` #2 Step 2.4 |

**Acceptance criteria:**
- [ ] `targets_staged_for_rename = true` 且 `capture.db` 已 `hard_link` 发布、`unlink("capture.db.tmp")` 前崩溃（`AlreadyExists` + `application_id == target_migration_tag`）：续跑零覆写清理重复硬链 `capture.db.tmp` 并经 `hard_link` 补齐 `libra.db` 至 `finalized = true`。
- [ ] `targets_staged_for_rename = true` 且 `capture.db.tmp` 已 `unlink`（`NotFound` + `capture.db` `application_id == target_migration_tag`）但 `libra.db` 未发时崩溃：续跑零覆写保留 `capture.db` 并经 `hard_link` 补齐 `libra.db` 至 `finalized = true`。
- [ ] `targets_staged_for_rename = true` 且 `capture.db` 与 `libra.db` 均已发布（两 `.tmp` 触发 `NotFound` 且均带 `target_migration_tag`）但 `finalized = true` 未落盘时崩溃：续跑零覆写保留两库并直接发布 `finalized = true`。
- [ ] 上述 3 处崩溃间隙内由旧 SQLite 进程提交至已发布 `capture.db`/`libra.db` 的全部新写入在收敛后 100% 完整可读。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_staged_hardlink_already_exists_notfound_and_both_published)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_staged_hardlink_already_exists_notfound_and_both_published)'`（new，覆盖 AC1–AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11m`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 同 tag 非覆写续跑契约：** 已对旧进程可见的目标 SQLite 零覆写，崩溃间隙旧写入 100% 保留。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 staged hardlink same-tag resume; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-11m; writeset=序列化于 RB-11m; release=independent; split-from=RB-11a; exception=N/A`

---

### Task RB-11s: 反向迁移 Phase 2 崩溃后竞争 `capture::store::open` 并轨对账与围栏后替换续跑（Track A，自 `RB-11e` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `migrate.rs` 实现反向迁移在 `targets_staged_for_rename = true` 后、`capture.db` 发布前崩溃且外部旧进程调用 `capture::store::open(storage_root)` 建出 `application_id == 0` 竞争 `capture.db` 时的完整单连接 `c_mig` 并轨（遵守 `agent_import_tombstone`）、`__libra_migrated_fence__` 围栏、原子替换以及围栏后替换前崩溃幂等续跑。

**Out of scope:**
- 全新 Redb 仓活 spool 对账与两类可达 Step 2.6 解封崩溃恢复：由 `RB-11h` 与 `RB-11u` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `capture::store::open` 可在 `capture.db` 缺失间隙创建 `application_id == 0` 竞争库，须经单连接并轨、围栏与原子替换保证迁移数据与竞争写入双 100% 保留 | `src/internal/ai/capture/store.rs:65-86`；`ADR-RB-07` #2 Step 2.4 |

**Acceptance criteria:**
- [ ] 崩溃后外部旧进程建出 `application_id == 0` 竞争 `capture.db` 并提交事件：续跑 `hard_link` 命中 `AlreadyExists` 后在单连接 `c_mig` 排他锁下将其合法事件并入 `capture.db.tmp`（严格遵守 `agent_import_tombstone` 拒重导）。
- [ ] 完成上述并轨后，续跑在同一单连接 `c_mig` 上对竞争 `capture.db` 提交 `__libra_migrated_fence__` 回滚日志原子围栏。
- [ ] 围栏竞争库后原子 `rename(capture.db.tmp, capture.db)` 替换并收敛至 `finalized = true`，原迁移捕获数据与竞争旧写入双 100% 保留。
- [ ] 恰崩溃于并轨围栏竞争 `capture.db`（`__libra_migrated_fence__` 已提交）之后、原子 `rename(capture.db.tmp, capture.db)` 之前：续跑识别已围栏竞争库与带 `target_migration_tag` 且通过 `integrity_check` 的 `capture.db.tmp`，幂等完成替换至 `finalized = true`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_competing_capture_store_open_reconciles_and_fences_zero_loss)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_competing_capture_store_open_reconciles_and_fences_zero_loss)'`（new，覆盖 AC1–AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11e`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 竞争 `capture.db` 并轨契约：** 迁移捕获与竞争旧进程写入双 100% 零丢失，围栏后崩溃可幂等收敛。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 competing capture-open reconcile; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=RB-11e; writeset=序列化于 RB-11e; release=independent; split-from=RB-11e; exception=N/A`

---

### Task RB-11h: 反向迁移全新 Redb 仓活 Spool 对账与并发 `drain` 锁保护（Track A，自 `RB-11e` 拆出，拆出 `RB-11u`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现反向迁移针对全新 Redb 仓（无预装 spool 墓碑或被外部改写情形）的 Phase 1 既有活 `capture-spool/ready/` 备份直灌、Phase 2 主动确立 3 个 `0400` 墓碑、全程持 `capture-spool-consumer.lock` 排斥并发 `drain_storage` 移动事件至 `quarantine/`，以及 `finalized == false` 防御性活 spool 并集对账。

**Out of scope:**
- 终态后 Step 2.6 两类可达解封崩溃恢复（可达态 A 部分清理与可达态 B 全 3 墓碑移除后活事件保留）：已按 `G-03`/`G-09` 拆出由 `RB-11u` 承接。
- 全局/系统配置双向迁移与 CLI：由 `RB-11b`..`RB-11t` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `spool.rs:330-355` `ensure_spool` 对缺失目录自动建目录；`drain.rs:87` 通过 `CONSUMER_LOCK` 互斥消费 `ready/`；反向迁移须在 `capture-spool-consumer.lock` 下对账全新 Redb 仓活 spool | `src/internal/ai/capture/spool.rs:330-355`；`src/internal/ai/capture/drain.rs:85-143`；`ADR-RB-07` #2 |

**Acceptance criteria:**
- [ ] 全新 Redb 仓（无预装 spool 墓碑）若 `capture-spool/ready/` 预存既有活事件，反向 Phase 1 在 `capture-spool-consumer.lock` + `capacity.lock` 下将其归档备份并按语义去重键直灌入 `capture.db.tmp`，最终 100% 保留于 `capture.db`。
- [ ] 上述反向迁移进入 Phase 2 时主动确立 `capture-spool/{tmp,ready,quarantine}` 全部 3 个 `0400` 墓碑跨越 `finalized = false`。
- [ ] **活 Spool 对账期间并发 `drain` 互斥实测：** 上述全新 Redb 仓 `capture-spool/ready/` 含待对账与畸形事件时，反向迁移全程持有 `capture-spool-consumer.lock`，并发调用 `drain_storage` 零移动退出（绝不趁隙将事件转入 `quarantine/` 造成逃逸）。
- [ ] 防御性兜底：若 `finalized == false` 期间外部异常清除 spool 墓碑导致活 `capture-spool/ready/` 出现新事件，反向续跑按去重键将其并入 `capture.db.tmp`（绝不丢弃）。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_fresh_redb_reconciles_preexisting_live_spool_and_establishes_tombstones) | test(=storage::migrate::tests::reverse_fresh_redb_live_spool_reconcile_blocks_concurrent_drain_moves)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_fresh_redb_reconciles_preexisting_live_spool_and_establishes_tombstones)'`（new，覆盖 AC1、AC2、AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_fresh_redb_live_spool_reconcile_blocks_concurrent_drain_moves)'`（new，覆盖 AC3）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11s`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 全新 Redb 仓活 Spool 对账契约：** 持 `capture-spool-consumer.lock` 排斥并发 `drain` 并主动建立 3 个 `0400` 墓碑跨越 `finalized = false`。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse fresh-redb live-spool reconcile drain-lock; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11s; writeset=序列化于 RB-11s; release=independent; split-from=RB-11e; exception=N/A`

---

### Task RB-11u: 反向迁移终态后 Step 2.6 两类可达解封崩溃恢复与活事件零丢失（Track A，自 `RB-11h` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `migrate.rs` 逐项验证反向迁移终态后 Step 2.6（`finalized = true, spool_unfenced = false`）的全部两类 100% 可达解封崩溃恢复路径：可达态 A（已移 `tmp`/`ready`、残存 `quarantine` `0400` 墓碑时旧 Hook 阻断并幂等清理收敛）与可达态 B（全 3 墓碑已移除、`spool_unfenced = true` 未落盘时旧 Hook 发布真实活事件，重跑零改动保留目录并完整落库）。

**Out of scope:**
- 全局/系统配置双向迁移与 CLI：由 `RB-11b`..`RB-11t` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `spool.rs:330` `ensure_spool` 对三子目录逐项检查 `!is_dir()`，故 Step 2.6 仅有「仍存至少 1 枚墓碑（可达态 A）」与「全 3 墓碑已移（可达态 B）」两类可达状态 | `src/internal/ai/capture/spool.rs:330-355`；`ADR-RB-07` #2 Step 2.6 |

**Acceptance criteria:**
- [ ] **Step 2.6 可达态 A 旧 Hook 阻断：** 在 `finalized = true, spool_unfenced = false`、已移除 `tmp`/`ready` 墓碑但 `quarantine` `0400` 墓碑仍存时，旧 SQLite Hook 调 `ensure_spool` 立即 fail-closed。
- [ ] **Step 2.6 可达态 A 幂等清理收敛：** 重跑幂等移除残留 `quarantine` `is_file()` 墓碑、执行 `fsync_dir` 并持久化 `spool_unfenced = true`。
- [ ] **Step 2.6 可达态 B 旧 Hook 发布活事件：** 在 `finalized = true, spool_unfenced = false`、全 3 墓碑均已移除 + `fsync_dir` 但 `spool_unfenced = true` 未落盘时注入崩溃，旧 SQLite Hook 成功重建三目录并向 `capture-spool/ready/` 发布真实活事件。
- [ ] **Step 2.6 可达态 B 零改动保留目录：** 重跑识别三子项均为目录后零改动保留活 `capture-spool/ready/` 并持久化 `spool_unfenced = true`。
- [ ] **Step 2.6 可达态 B 活事件 100% 入库：** 上述收敛完成后消费 `capture-spool/ready/`，崩溃窗口新发布的活事件 100% 落入 `capture.db` 零丢失。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_step26_partial_cleanup_blocks_old_hook_then_resumes) | test(=storage::migrate::tests::reverse_phase2_step26_post_unfence_crash_preserves_live_old_hook_event)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_step26_partial_cleanup_blocks_old_hook_then_resumes)'`（new，覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::reverse_phase2_step26_post_unfence_crash_preserves_live_old_hook_event)'`（new，覆盖 AC3–AC5）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11h`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部反向迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 终态后 Step 2.6 解封恢复契约：** 两类可达解封崩溃态均 100% 幂等收敛且零丢失旧 Hook 新发布活事件。

**Performance budget:** 解封续跑耗时 ≤ `50ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=reverse phase-2 step26 reachable unfence crashes; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=2; scope=S; deps=RB-11h; writeset=序列化于 RB-11h; release=independent; split-from=RB-11h; exception=N/A`

---

### Task RB-11b: `GlobalConfig` 正向配置迁移与原位保留型 `0400` 路径墓碑（Track A，自 `RB-11` 拆出，拆出 `RB-11v`、`RB-11k`、`RB-11f`、`RB-11g`、`RB-11c`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现 `GlobalConfig` 正向配置离线迁移正常路径（`src/storage/migrate.rs`）：全程持排他 `LOCK_EX` 于 `<scope_dir>/config-migration.lock`，经单连接 `c_mig` 过 WAL 静默门 + `VACUUM INTO` 备份 + `config-format.json(finalized=false)` + 回滚日志原子围栏 `__libra_migrated_fence__` `COMMIT`，**永久原位保留只读 `0400` 已围栏 `config.db` 路径墓碑（绝不 `unlink`）** 后再 `rename(config.redb.tmp, config.redb)`（`0600`）与发布 `finalized = true, legacy_sqlite_tombstone = true`，物理阻断旧版二进制经 `config.rs:257` 重建第二份 SQLite 配置库，并验证 `GC-13` 全局隔离。

**Out of scope:**
- `SystemConfig` 正向配置迁移与 `0444` 路径墓碑阻断：已按 `G-03`/`G-09` 拆出由 `RB-11v` 承接。
- 全局/系统反向配置迁移正常路径与双域有界锁争用：由 `RB-11k` 与 `RB-11w` 承接。
- 正/反向配置崩溃恢复与 `libra-migrate` CLI：由 `RB-11f`..`RB-11t` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `src/command/config.rs:257-295` 在 `config.db` 不存在时自动创建新 SQLite 库，正向原位保留已围栏只读 `0400` `config.db` 墓碑可 100% 阻断旧版二进制重建分裂全局配置库 | `src/command/config.rs:257-295`；`src/internal/db/schema.rs:785-814`；`ADR-RB-02` #2；`ADR-RB-07` #3 |

**Acceptance criteria:**
- [ ] `migrate_global_config(Sqlite -> Redb)` 在隔离 `LIBRA_CONFIG_GLOBAL_DB` 目录内经单连接 `c_mig` 完成 WAL 静默、`VACUUM INTO` 备份、产出 `0600` `config.redb` 并发布 `config-format.json(backend="redb", finalized=true, legacy_sqlite_tombstone=true)`。
- [ ] `migrate_global_config(Sqlite -> Redb)` 永久原位保留经 `__libra_migrated_fence__` 围栏后的只读 `0400` `config.db` 路径墓碑（绝不 `unlink`）。
- [ ] **旧版二进制路径级阻断实测（`GlobalConfig`）：** 正向迁移后模拟旧版 `libra config get/set --global` 经 `config.rs:257` `exists == true` 打开 `0400` 围栏 `config.db` 墓碑，立即报 `SQLITE_CORRUPT` fail-closed（绝不重建第二份 SQLite 配置库）。
- [ ] `GC-13` 全局迁移隔离：`--global` 迁移绝不触碰隔离 `SystemConfig` 与仓库库。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::gc13_global_config_forward_migration_tombstone_and_legacy_block)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::gc13_global_config_forward_migration_tombstone_and_legacy_block)'`（new，覆盖 AC1–AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11u`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部配置迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 全局配置正向单权威切换契约：** 原位保留 `0400` 已围栏 `config.db` 墓碑物理阻断旧版二进制并保持 `SystemConfig`/仓库严格隔离。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=global config forward retained 0400 tombstone; recovery=clean tmp or reverse migrate; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=RB-11u; writeset=序列化于 RB-11u; release=independent; split-from=RB-11; exception=N/A`

---

### Task RB-11v: `SystemConfig` 正向配置迁移与原位保留型 `0444` 路径墓碑（Track A，自 `RB-11b` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `migrate.rs` 实现 `SystemConfig`（隔离夹具 `LIBRA_CONFIG_SYSTEM_DB`）正向配置离线迁移正常路径：经单连接 `c_mig` 完成 WAL 静默、`VACUUM INTO` 备份、产出 `0644` `config.redb` 与 `config-format.json(backend="redb", finalized=true, legacy_sqlite_tombstone=true)`，永久原位保留 `0444` 已围栏 `config.db` 墓碑以阻断旧版二进制，并验证 `GC-13` 系统迁移隔离。

**Out of scope:**
- 全局/系统反向配置迁移正常路径与双域有界锁争用：由 `RB-11k` 与 `RB-11w` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `SystemConfig` 为独立可变库且具 `0644`/`0444` 权限与独立 `GC-13` 隔离要求，须独立实证正向迁移与旧版二进制阻断 | `ADR-RB-02` #2；`ADR-RB-07` #3；`GC-RB-02` |

**Acceptance criteria:**
- [ ] `migrate_system_config(Sqlite -> Redb)` 在隔离 `LIBRA_CONFIG_SYSTEM_DB` 目录内产出 `0644` `config.redb` 并发布 `config-format.json(backend="redb", finalized=true, legacy_sqlite_tombstone=true)`。
- [ ] `migrate_system_config(Sqlite -> Redb)` 永久原位保留经 `__libra_migrated_fence__` 围栏后的只读 `0444` `config.db` 路径墓碑（绝不 `unlink`）。
- [ ] **旧版二进制路径级阻断实测（`SystemConfig`）：** 正向迁移后模拟旧版 `libra config get/set --system` 打开 `0444` 围栏 `config.db` 墓碑，立即报 `SQLITE_CORRUPT` fail-closed（绝不重建第二份 SQLite 配置库）。
- [ ] `GC-13` 系统迁移隔离：`--system` 迁移绝不触碰隔离 `GlobalConfig` 与仓库库。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::gc13_system_config_forward_migration_tombstone_and_legacy_block)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::gc13_system_config_forward_migration_tombstone_and_legacy_block)'`（new，覆盖 AC1–AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11b`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部配置迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 系统配置正向单权威切换契约：** 原位保留 `0444` 已围栏 `config.db` 墓碑物理阻断旧版二进制并保持 `GlobalConfig`/仓库严格隔离。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=system config forward retained 0444 tombstone; recovery=clean tmp or reverse migrate; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-11b; writeset=序列化于 RB-11b; release=independent; split-from=RB-11b; exception=N/A`


---

### Task RB-11k: `GlobalConfig`/`SystemConfig` 反向配置迁移正常路径与双向 3 表无损往返（Track A，自 `RB-11b` 拆出，拆出 `RB-11w`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现 `GlobalConfig` 与 `SystemConfig` 反向配置迁移正常路径（`src/storage/migrate.rs`）：持排他 `LOCK_EX` 于 `<scope_dir>/config-migration.lock` 执行 `META_SCHEMA u32::MAX` 围栏、`fenced_ready_to_unlink -> unlink(config.redb) + fsync_dir -> unlinked`、`targets_staged_for_rename = true` 原子替换墓碑为 `config.db`（Windows 先清墓碑只读位）并发布 `finalized = true, legacy_sqlite_tombstone = false`，验证双向 3 表 100% 无损往返。

**Out of scope:**
- 双域（Global/System）× 双运行时（Sqlite/Redb）4 组有界锁争用实测：已按 `G-03`/`G-09` 拆出由 `RB-11w` 承接。
- 配置正向与反向崩溃恢复与故障拒收：由 `RB-11f`、`RB-11n`、`RB-11g`、`RB-11o` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-02` #2 与 `ADR-RB-07` #3 要求配置双向无损互迁并原子替换原位墓碑回切 `config.db` | `ADR-RB-02` #2；`ADR-RB-07` #3 |

**Acceptance criteria:**
- [ ] `migrate_global_config(Redb -> Sqlite)` 经 `META_SCHEMA u32::MAX`、`unlink(config.redb) + fsync_dir`、`targets_staged_for_rename = true` 原子替换墓碑产出 `0600` `config.db` 并发布 `finalized = true, legacy_sqlite_tombstone = false`。
- [ ] `migrate_system_config(Redb -> Sqlite)` 通过同一对称反向协议替换墓碑产出 `0644` `config.db` 并发布 `finalized = true, legacy_sqlite_tombstone = false`。
- [ ] 双向迁移无损往返 100% `config`、`config_kv` 与 `configuration_schema_versions` 表行。
- [ ] 单特性 `--no-default-features --features storage-sqlite` 下的 `storage::` 单元测试全绿零受扰。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::gc13_global_and_system_config_reverse_migration_replaces_tombstone)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::gc13_global_and_system_config_reverse_migration_replaces_tombstone)'`（new，覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`（覆盖 AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11v`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部配置迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 配置域反向正常回切契约：** 以 `targets_staged_for_rename` 原子替换原位墓碑回切 `0600`/`0644` `config.db`，3 张配置源表 100% 无损往返。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=global-sys config reverse replace tombstone; recovery=clean tmp or forward migrate; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11v; writeset=序列化于 RB-11v; release=independent; split-from=RB-11b; exception=N/A`

---

### Task RB-11w: `GlobalConfig`/`SystemConfig` 双域 × 双运行时有界锁争用实测（Track A，自 `RB-11k` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内于 `migrate.rs` 针对 `GlobalConfig` 与 `SystemConfig` 两作用域分别实测迁移持排他 `LOCK_EX` 期间，并发 `Sqlite` 与 `Redb` 配置命令全部按有界锁超时契约 fail-closed 返回类型化 `LBR-IO-001`（杜绝迁移窗口无限阻塞或分叉写入）。

**Out of scope:**
- 配置正向与反向崩溃恢复与故障拒收：由 `RB-11f`、`RB-11n`、`RB-11g`、`RB-11o` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ADR-RB-02` #2 要求 Global/System 双域与 Sqlite/Redb 双运行时共同参与有界 `config-migration.lock`，迁移持 `LOCK_EX` 期间并发命令超期立报 `LBR-IO-001` | `ADR-RB-02` #2；`RB-03h` |

**Acceptance criteria:**
- [ ] **`GlobalConfig` 有界锁争用（`Sqlite` 命令）：** 迁移持排他 `LOCK_EX` 期间，并发 `Sqlite` 配置命令按有界锁超时契约 fail-closed 返回 `LBR-IO-001`。
- [ ] **`GlobalConfig` 有界锁争用（`Redb` 命令）：** 迁移持排他 `LOCK_EX` 期间，并发 `Redb` 配置命令按有界锁超时契约 fail-closed 返回 `LBR-IO-001`。
- [ ] **`SystemConfig` 有界锁争用（`Sqlite` 命令）：** 迁移持排他 `LOCK_EX` 期间，隔离 `LIBRA_CONFIG_SYSTEM_DB` 并发 `Sqlite` 配置命令超期 fail-closed 返回 `LBR-IO-001`。
- [ ] **`SystemConfig` 有界锁争用（`Redb` 命令）：** 迁移持排他 `LOCK_EX` 期间，隔离 `LIBRA_CONFIG_SYSTEM_DB` 并发 `Redb` 配置命令超期 fail-closed 返回 `LBR-IO-001`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_migration_bounded_lock_contention_global_and_system_fail_closed)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_migration_bounded_lock_contention_global_and_system_fail_closed)'`（new，覆盖 AC1–AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11k`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部配置迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 双域有界锁争用契约：** Global/System × Sqlite/Redb 全 4 组合在迁移窗口超期立即 fail-closed `LBR-IO-001`。

**Performance budget:** 有界锁争用超时判定单测耗时 ≤ `50ms`（支持 `Duration::ZERO`）。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=global-sys config 4-case bounded lock contention; recovery=clean tmp or forward migrate; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-11k; writeset=序列化于 RB-11k; release=independent; split-from=RB-11k; exception=N/A`


---

### Task RB-11f: `GlobalConfig`/`SystemConfig` 正向配置迁移热日志与 `targets_staged_for_rename` 崩溃恢复（Track A，自 `RB-11b` 拆出，拆出 `RB-11n`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现 `GlobalConfig` 与 `SystemConfig` 正向迁移（`Sqlite → Redb`）双域幂等恢复（`src/storage/migrate.rs`）：分别在 `GlobalConfig` 与 `SystemConfig` 两作用域实测围栏提交中途断电撕裂扇区热回滚日志自动回滚并收敛至 `finalized = true, legacy_sqlite_tombstone = true`，以及 `targets_staged_for_rename = true` 且 `config.redb` 已就位后崩溃零覆写推进。

**Out of scope:**
- 正向配置 `pending` 缺失、备份缺损与 `finalized == false` 并发拒访：已按 `G-03`/`G-09` 拆出由 `RB-11n` 承接。
- 反向配置迁移崩溃恢复：由 `RB-11g` 与 `RB-11o` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 正向配置迁移须在 `GlobalConfig` 与 `SystemConfig` 两作用域各自实测断电热回滚日志恢复与 `targets_staged_for_rename` 已就位 `config.redb` 零覆写保护 | `ADR-RB-07` #3 |

**Acceptance criteria:**
- [ ] **`GlobalConfig` 正向断电热回滚日志恢复：** 保留热 `config.db-journal` 并破坏页 1 首个 512 字节扇区，续跑触发热日志回滚并收敛至 `finalized = true, legacy_sqlite_tombstone = true`（`0600` `config.redb` + 原位 `0400` 已围栏 `config.db` 墓碑，零数据丢失）。
- [ ] **`SystemConfig` 正向断电热回滚日志恢复：** 同上在隔离 `LIBRA_CONFIG_SYSTEM_DB` 验证热日志回滚并收敛至 `finalized = true, legacy_sqlite_tombstone = true`（`0644` `config.redb` + 原位 `0444` 墓碑）。
- [ ] **`GlobalConfig` 正向 `targets_staged_for_rename = true` 崩溃恢复：** `config.redb.tmp` 已 `rename` 为 `config.redb`、`finalized = true` 未落盘时崩溃，续跑检出已就位 `config.redb` 绝不覆写，核验已围栏 `config.db` 墓碑后发布 `finalized = true`。
- [ ] **`SystemConfig` 正向 `targets_staged_for_rename = true` 崩溃恢复：** 同上在隔离 `LIBRA_CONFIG_SYSTEM_DB` 保留已就位 `0644` `config.redb` 并发布 `finalized = true`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_forward_crash_hot_journal_torn_sector_global_and_system) | test(=storage::migrate::tests::config_forward_crash_staged_rename_preserves_redb_global_and_system)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_forward_crash_hot_journal_torn_sector_global_and_system)'`（new，覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_forward_crash_staged_rename_preserves_redb_global_and_system)'`（new，覆盖 AC3、AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11w`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部配置迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 正向配置热日志与 staged-rename 恢复契约：** 在 Global/System 双域逐项验证热日志撕裂自动回滚与 staged-rename 已就位 `config.redb` 零覆写推进。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=global-sys config forward hot-journal staged-rec; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11w; writeset=序列化于 RB-11w; release=independent; split-from=RB-11b; exception=N/A`

---

### Task RB-11n: `GlobalConfig`/`SystemConfig` 正向配置故障拒收与 `finalized == false` 并发拦截（Track A，自 `RB-11f` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现 `GlobalConfig` 与 `SystemConfig` 正向迁移（`Sqlite → Redb`）全类故障 fail-closed 拒收与半途清单并发拒访（`src/storage/migrate.rs`）：分别实测 `source_db_state == "pending"` 时 Global 与 System 意外缺失 `config.db` 拒收、已围栏态 `sqlite-backup` 缺失与 `backup_sha256` 失配拒收，以及残留 `config-format.json(finalized = false)` 时并发 `Sqlite` 与 `Redb` 配置命令各自 fail-closed `LBR-IO-001`。

**Out of scope:**
- 反向配置迁移崩溃恢复：由 `RB-11g` 与 `RB-11o` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 正向配置迁移须对双域意外缺席、备份缺失、备份失配及 `finalized == false` 下双运行时访问全部独立 fail-closed 验收 | `ADR-RB-07` #3 |

**Acceptance criteria:**
- [ ] `GlobalConfig` 正向 `source_db_state == "pending"` 而 `config.db` 意外缺失时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] `SystemConfig` 正向 `source_db_state == "pending"` 而 `config.db` 意外缺失时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 正向配置已围栏态对应的 `sqlite-backup` 缺失时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 正向配置已围栏态对应的 `sqlite-backup` `backup_sha256` 失配时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 迁移崩溃释锁后残留 `config-format.json(finalized = false)` 期间，并发 `Sqlite` 配置命令立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 迁移崩溃释锁后残留 `config-format.json(finalized = false)` 期间，并发 `Redb` 配置命令立即 fail-closed 返回 `LBR-IO-001`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_forward_crash_pending_missing_and_backup_fault_reject) | test(=storage::migrate::tests::config_forward_pending_manifest_rejects_sqlite_and_redb_commands)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_forward_crash_pending_missing_and_backup_fault_reject)'`（new，覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_forward_pending_manifest_rejects_sqlite_and_redb_commands)'`（new，覆盖 AC5、AC6）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11f`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部配置迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 正向配置故障拒收契约：** 覆盖双域 `pending` 缺失、备份缺失/失配与 `finalized == false` 下双运行时并发拒访。

**Performance budget:** 故障检测耗时 ≤ `50ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=global-sys config forward fault and pending-rej; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11f; writeset=序列化于 RB-11f; release=independent; split-from=RB-11f; exception=N/A`

---

### Task RB-11g: `GlobalConfig`/`SystemConfig` 反向配置迁移 `unlink(config.redb)` 恢复与缺席/备份拒收（Track A，自 `RB-11b` 拆出，拆出 `RB-11o`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现 `GlobalConfig` 与 `SystemConfig` 反向迁移（`Redb → Sqlite`）的 `unlink(config.redb)+fsync_dir` 紧后崩溃恢复与源/备份故障拦截（`src/storage/migrate.rs`）：分别在 Global 与 System 双域实测合法已摘除态的 `redb-backup` 真实 SHA-256 校验恢复、双域 `source_db_state == "pending"` 意外缺失 `config.redb` 拒收，以及 `redb-backup` 缺失与 SHA-256 失配的 fail-closed 拦截。

**Out of scope:**
- 反向配置 `targets_staged_for_rename = true` 下已发布 `config.db` 旧二进制写入保护与未替换墓碑续跑：已按 `G-03`/`G-09` 拆出由 `RB-11o` 承接。
- `Cargo.toml` 注册 `[[bin]] libra-migrate` 与 CLI 入口：由 `RB-11c` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 反向配置迁移须在 Global 与 System 双域各自覆盖 `unlink(config.redb)+fsync_dir` 紧后崩溃、`pending` 意外缺席与备份缺损拒收 | `ADR-RB-07` #3 |

**Acceptance criteria:**
- [ ] **`GlobalConfig` 反向紧接 `unlink(config.redb) + fsync_dir` 后崩溃实测：** `source_db_state ∈ {"fenced_ready_to_unlink", "unlinked"}` 时，续跑校验 `redb-backup` 真实 SHA-256 后幂等收敛至 `finalized = true, legacy_sqlite_tombstone = false` 并完整恢复 `0600` `config.db`。
- [ ] **`SystemConfig` 反向紧接 `unlink(config.redb) + fsync_dir` 后崩溃实测：** 同上校验 `redb-backup` 真实 SHA-256 并以 `0644` 权限收敛至 `finalized = true, legacy_sqlite_tombstone = false`。
- [ ] `GlobalConfig` 反向 `source_db_state == "pending"` 而 `config.redb` 意外缺失时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] `SystemConfig` 反向 `source_db_state == "pending"` 而 `config.redb` 意外缺失时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 反向配置已合法摘除 `config.redb` 但对应 `redb-backup` 缺失时，续跑立即 fail-closed 返回 `LBR-IO-001`。
- [ ] 反向配置已合法摘除 `config.redb` 但对应 `redb-backup` SHA-256 失配时，续跑立即 fail-closed 返回 `LBR-IO-001`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_reverse_crash_after_config_redb_unlink_global_and_system) | test(=storage::migrate::tests::config_reverse_crash_pending_missing_and_corrupt_backup_reject)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_reverse_crash_after_config_redb_unlink_global_and_system)'`（new，覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_reverse_crash_pending_missing_and_corrupt_backup_reject)'`（new，覆盖 AC3–AC6）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11n`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部配置迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向配置 post-unlink 与故障拦截契约：** 在 Global/System 双域逐项验证 `unlink(config.redb)` 备份恢复、`pending` 缺席与备份缺损拒收。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=global-sys config reverse unlink and fault-reject; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11n; writeset=序列化于 RB-11n; release=independent; split-from=RB-11b; exception=N/A`

---

### Task RB-11o: `GlobalConfig`/`SystemConfig` 反向配置 `targets_staged_for_rename` 崩溃恢复与旧二进制写入保护（Track A，自 `RB-11g` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 100% 在独占目录 `src/storage/` 内实现 `GlobalConfig` 与 `SystemConfig` 反向迁移（`Redb → Sqlite`）的 `targets_staged_for_rename = true` 崩溃恢复与旧二进制写入零覆写保护（`src/storage/migrate.rs`）：分别在 Global 与 System 双域实测 `rename(config.db.tmp, config.db)` 后、`finalized = true` 前崩溃且旧版二进制已向现网 `config.db` 写入新配置时续跑绝不覆写已发布 `config.db`，以及 `rename` 尚未执行（`config.db` 仍为已围栏墓碑）时双域续跑正确完成原子替换。

**Out of scope:**
- `Cargo.toml` 注册 `[[bin]] libra-migrate` 与 CLI 入口：由 `RB-11c` 承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `targets_staged_for_rename = true` 下须分别覆盖 Global 与 System 的 post-rename 旧二进制写入保留与 pre-rename 墓碑替换 | `ADR-RB-07` #3 |

**Acceptance criteria:**
- [ ] **`GlobalConfig` 反向 post-rename 崩溃 + 旧二进制写入保护：** `rename(config.db.tmp, config.db)` 后、`finalized = true` 前崩溃且旧版 `libra config set --global` 写入新键值，续跑检出有效未围栏 `config.db` **绝不覆写**、清理 `.tmp` 并发布 `finalized = true, legacy_sqlite_tombstone = false`，旧二进制写入 100% 保留。
- [ ] **`SystemConfig` 反向 post-rename 崩溃 + 旧二进制写入保护：** 同上对 `--system` 验证续跑**绝不覆写**已发布 `0644` `config.db`，100% 保留崩溃后旧二进制写入。
- [ ] **`GlobalConfig` 反向 pre-rename 墓碑续跑：** `targets_staged_for_rename = true` 后尚未 `rename`（`config.db` 仍为墓碑）时崩溃，续跑原子 `rename(config.db.tmp, config.db)`（Windows 先清只读位）产出 `0600` `config.db` 并发布 `finalized = true, legacy_sqlite_tombstone = false`。
- [ ] **`SystemConfig` 反向 pre-rename 墓碑续跑：** 同上在隔离 `LIBRA_CONFIG_SYSTEM_DB` 原子替换 `0444` 墓碑产出 `0644` `config.db` 并发布 `finalized = true, legacy_sqlite_tombstone = false`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_reverse_staged_rename_preserves_old_binary_writes_global_and_system) | test(=storage::migrate::tests::config_reverse_staged_rename_before_rename_replaces_tombstone_global_and_system)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_reverse_staged_rename_preserves_old_binary_writes_global_and_system)'`（new，覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::migrate::tests::config_reverse_staged_rename_before_rename_replaces_tombstone_global_and_system)'`（new，覆盖 AC3、AC4）
- [ ] 按「执行检查必备需求」第 3 条 Stage 1 守卫核验本卡精确 2 文件实现白名单。

**Full-suite trigger:** `none`

**Dependencies:** `RB-11g`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/mod.rs`、`src/storage/migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（内部配置迁移引擎）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 反向配置 staged-rename 崩溃恢复契约：** 在 Global/System 双域对已发布 `config.db` 及崩溃后旧二进制写入实现绝对零覆写保护，未替换墓碑幂等完成原子替换。

**Performance budget:** 单批 ≤ 500 行，额外堆内存峰值 ≤ `64 MiB`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=global-sys config reverse staged-rename recovery; recovery=resume idempotent phase-2; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=RB-11g; writeset=序列化于 RB-11g; release=independent; split-from=RB-11g; exception=N/A`

---

### Task RB-11c: `libra-migrate` CLI 二进制注册、`--dry-run`、重入幂等与单元门控（Track A，自 `RB-11b` 拆出，拆出 `RB-11t`）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 新建 `src/bin/libra_migrate.rs` 并在 `Cargo.toml` 注册 `[[bin]] libra-migrate`（`required-features = ["storage-sqlite", "storage-redb"]`），暴露 `--to redb|sqlite`、`--global`、`--system`、`--dry-run` 与重入幂等校验；生产 `main()` 严格绑定编译期常量 `crate::storage::backend::REDB_RUNTIME_UNLOCKED`（`RB-29a` 前为 `false`，零环境变量旁路），完整端到端单测经 `#[cfg(test)]` 专属接缝 `run_cli_for_test` 驱动。

**Out of scope:**
- 拉起真实编译出的 `target/release/libra-migrate` 二进制在带 `LIBRA_TEST=1` 下逐项验证 4 类实盘切换标志（`--to redb`/`--to sqlite`/`--global`/`--system`）不可旁路拒收：已按 `G-03`/`G-09` 拆出由 `RB-11t` 承接。
- 修改既有 `tests/db_migration_test.rs` 或共享用户文档（用户文档随 `RB-33` 交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `src/bin/` 当前不存在；`RB-29a` 前 CLI 仍未全量接线，故已发行 `libra-migrate` 实盘切换必须在编译期不可旁路地门控 | 2026-11-21 核对 `src/bin` 缺席；`ADR-RB-01`、`ADR-RB-07` |

**Acceptance criteria:**
- [ ] `Cargo.toml` 注册 `[[bin]] libra-migrate`（`required-features = ["storage-sqlite", "storage-redb"]`，入口位于 `src/bin/libra_migrate.rs`）且单特性 `--no-default-features --features storage-sqlite` 编译自动跳过零告警。
- [ ] `cargo build --release --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate` 构建通过。
- [ ] `--dry-run` 在已发行二进制上只读输出各表待迁移行数与预计体积（零磁盘写入）。
- [ ] `run_cli_with_gate(..., /*unlocked=*/false)` 单元测试验证锁定门控带 `LIBRA_TEST=1` 返回 `LBR-CLI-003`（`RB-29a` 解锁后 `--bin libra-migrate` 仍恒定为绿）。
- [ ] 对已迁移目标重复运行且未传 `--force` 时输出明确提示退出 0，不覆写现有目标库。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate -E 'test(=cli_migrate_gate_rejects_when_locked_even_with_libra_test_env) | test(=cli_migrate_test_seam_dry_run_reentry_and_bidirectional_roundtrip)'`（new，bin 发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate -E 'test(=cli_migrate_gate_rejects_when_locked_even_with_libra_test_env)'`（new，覆盖 AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate -E 'test(=cli_migrate_test_seam_dry_run_reentry_and_bidirectional_roundtrip)'`（new，经 `#[cfg(test)]` 接缝覆盖 AC3、AC5）
- [ ] `cargo build --release --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`（覆盖 AC2）
- [ ] `cargo check --no-default-features --features storage-sqlite`（验证单特性 SQLite 下 `required-features` 自动跳过 `libra-migrate` 且零告警，覆盖 AC1）

**Full-suite trigger:** `T-1 + T-3: 修改 Cargo.toml 注册新 [[bin]] libra-migrate`

**Dependencies:** `RB-11o`、`DEP-RB-02`（`Cargo.toml` 串行写窗口）

**Deliverables:** `N/A`

**Implementation write set:** `Cargo.toml`、`Cargo.lock`、`src/storage/mod.rs`、`src/storage/migrate.rs`、`src/bin/libra_migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 5 个文件（行为生产落点 3 个：`Cargo.toml`、`src/storage/`、`src/bin/`；4 个生产文件不含 `Cargo.lock`）。

**Docs and compatibility impact:** `N/A`（预集成编译期门控仅 `--dry-run` 预览，正式 CLI 文档随 `RB-33` 发布）。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 二进制注册与干跑契约：** 注册双特性迁移二进制并完成 `--dry-run`、重入幂等与编译期门控单元验证。

**Performance budget:** `--dry-run` 本地统计耗时 ≤ `200ms`。

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=libra-migrate bin registration dry-run reentry; recovery=revert binary registration; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=3; prod-files=4; scope=M; deps=RB-11o,DEP-RB-02; writeset=序列化于 RB-11o; release=independent; split-from=RB-11b; exception=N/A`

---

### Task RB-11t: `libra-migrate` 真实发布成品全 4 标志不可旁路编译期门控实证（Track A，自 `RB-11c` 拆出）

**Task type:** `migration`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在 `RB-29a` 解锁前，拉起真实编译产出的 `target/release/libra-migrate` 二进制并显式注入 `LIBRA_TEST=1`，逐标志验证 `--to redb`、`--to sqlite`、`--global --to redb` 与 `--system --to redb` 全部 4 类实盘切换路径无条件 fail-closed 返回 `LBR-CLI-003` 且零磁盘写入（确认已发行可执行文件零环境变量旁路）。

**Out of scope:**
- 修改 `Cargo.toml` 或 `src/storage/**`：本卡仅完善 `src/bin/libra_migrate.rs` 命令行门控自检与实机进程验收。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 仅靠单元测试无法替代对已编译 `target/release/libra-migrate` 真实进程带 `LIBRA_TEST=1` 的全 4 标志不可旁路验收 | `ADR-RB-01`；`RB-11c` |

**Acceptance criteria:**
- [ ] **不可旁路编译期门控（`--to redb`）：** 在 `RB-29a` 解锁前拉起真实 `target/release/libra-migrate --to redb`（显式设 `LIBRA_TEST=1`），无条件 fail-closed 返回 `LBR-CLI-003` 且零磁盘写入。
- [ ] **不可旁路编译期门控（`--to sqlite`）：** 拉起真实 `target/release/libra-migrate --to sqlite`（显式设 `LIBRA_TEST=1`），无条件 fail-closed 返回 `LBR-CLI-003` 且零磁盘写入。
- [ ] **不可旁路编译期门控（`--global`）：** 拉起真实 `target/release/libra-migrate --global --to redb`（隔离 `LIBRA_CONFIG_GLOBAL_DB`，设 `LIBRA_TEST=1`），无条件 fail-closed 返回 `LBR-CLI-003` 且零磁盘写入。
- [ ] **不可旁路编译期门控（`--system`）：** 拉起真实 `target/release/libra-migrate --system --to redb`（隔离 `LIBRA_CONFIG_SYSTEM_DB`，设 `LIBRA_TEST=1`），无条件 fail-closed 返回 `LBR-CLI-003` 且零磁盘写入。

**Verification:**
- [ ] `cargo build --release --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`（构建真实待验二进制）
- [ ] `bash -euo pipefail -c 'tmp="$(mktemp -d)"; trap "rm -rf -- \"$tmp\"" EXIT; mkdir -p "$tmp/.libra" "$tmp/g" "$tmp/s"; : > "$tmp/.libra/libra.db"; : > "$tmp/g/config.db"; : > "$tmp/s/config.db"; for args in "--repo $tmp --to redb" "--repo $tmp --to sqlite" "--global --to redb" "--system --to redb"; do set +e; out="$(LIBRA_TEST=1 LIBRA_CONFIG_GLOBAL_DB="$tmp/g/config.db" LIBRA_CONFIG_SYSTEM_DB="$tmp/s/config.db" target/release/libra-migrate $args 2>&1)"; st=$?; set -e; test "$st" -ne 0 && printf "%s\n" "$out" | rg -q "LBR-CLI-003"; done; test ! -e "$tmp/.libra/storage-format.json" && test ! -e "$tmp/g/config.redb" && test ! -e "$tmp/s/config.redb"'`（逐项拉起真实 `target/release/libra-migrate` 并设 `LIBRA_TEST=1` 验证全部 4 类标志不可旁路拒收且零写盘，覆盖 AC1–AC4）

**Full-suite trigger:** `none`

**Dependencies:** `RB-11c`

**Deliverables:** `N/A`

**Implementation write set:** `src/bin/libra_migrate.rs`

**Release write set:** `Inherited`

**Files likely touched:** 上列 1 个文件（1 个生产目录 `src/bin/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `forward-only`

**Migration and rollback:** **完整 `GC-13` / `G-11` 实机 4 标志不可旁路契约：** `RB-29a` 解锁前真实编译产物零环境变量旁路、零写盘。

**Performance budget:** 单次拒收退出耗时 ≤ `50ms`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=migration; axis=libra-migrate release binary 4-flag pre-unlock; recovery=revert bin gate check; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=1; scope=S; deps=RB-11c; writeset=序列化于 RB-11c; release=independent; split-from=RB-11c; exception=N/A`

---

### Task RB-12: 独立 `redb` 存储栈 CI 门禁与 32 进程并发压测（Track A 收口）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 新增跨进程并发压测 target `tests/storage_concurrency_stress_test.rs`（内部用例按 `#[cfg(feature = "storage-redb")]` 与 `#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]` 精确门控，含锁定态真实 `env!("CARGO_BIN_EXE_libra-migrate")` 拒收集成测试；同步登记 `tests/INDEX.md`），并在 `.github/workflows/base.yml` 追加独立并行的 `compat-redb-storage` CI job，精确编排 6 条命令：① `--no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`；② `--no-default-features --features storage-redb --lib -E 'test(/^storage::/)'`（同步验证 `storage-redb` 隐式开启 `storage-queue`）；③ `--no-default-features --features storage-redb --test storage_concurrency_stress_test -E 'test(=multiprocess_32_producers_and_concurrent_mvcc_readers_writers)'`；④ `--features storage-sqlite,storage-redb,storage-queue --lib -E 'test(/^storage::/)'`；⑤ `--features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`；⑥ `--features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=dual_feature_migration_and_stress_roundtrip) | test(=pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env)'`（步骤 ⑥ 将在 `RB-29a` 翻开 `REDB_RUNTIME_UNLOCKED` 同卡原子切换至 `post_unlock_libra_migrate_binary_bidirectional_roundtrip`），不改动既有 `compat-offline-core` job（经精确作用域 `diff -u` 相对前置基线验证）。

**Out of scope:**
- 解锁后将 `compat-redb-storage` 步骤 ⑥ 切换至 `post_unlock_libra_migrate_binary_bidirectional_roundtrip`（归 `RB-29a`）与升级 `base.yml` 跑全量 redb CLI 套件（归 `RB-31`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `base.yml` 当前无 `compat-redb-storage` job；`test:` job（`name: compat-offline-core`）位于 `.github/workflows/base.yml:94-293`；`tests/INDEX.md` 未登记存储压测 target | `.github/workflows/base.yml:94-293`；`tests/INDEX.md` |

**Acceptance criteria:**
- [ ] `storage_concurrency_stress_test` 在同一临时仓库并发拉起 32 个双通道入队进程 + 4 个循环 `RedbGitStore` 只读进程 + 2 个循环 `RedbGitStore` CAS 写进程 + 4 个 `RedbCaptureStore` 只读进程，全部退出 0、零锁错、100% 事件入库。
- [ ] `pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env` 拉起真实 `env!("CARGO_BIN_EXE_libra-migrate")` 并设 `LIBRA_TEST=1`，验证预集成锁定态无条件返回 `LBR-CLI-003` 且零写盘。
- [ ] `tests/INDEX.md` 已同步登记 `storage_concurrency_stress_test` 行并通过索引守卫（`GC-06`）。
- [ ] `.github/workflows/base.yml` 新增独立 `compat-redb-storage` job，精确包含上述 6 条命令（含步骤 ⑥ 显式选择 `dual_feature_migration_and_stress_roundtrip` 与 `pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env`）。
- [ ] 既有 `compat-offline-core` job（`jobs.test`）步骤与配置相对前置基线零修改。
- [ ] 按 `ER-04` 现场提取的 `compat-redb-storage` 本地等价命令执行全部退出 0。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=multiprocess_32_producers_and_concurrent_mvcc_readers_writers) | test(=dual_feature_migration_and_stress_roundtrip) | test(=pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --test storage_concurrency_stress_test -E 'test(=multiprocess_32_producers_and_concurrent_mvcc_readers_writers)'`（new，覆盖 AC1、AC6）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=dual_feature_migration_and_stress_roundtrip)'`（new，覆盖 AC4、AC6）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env)'`（new，覆盖 AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`（验证 SQLite 单特性，覆盖 AC6）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --test compat_matrix_alignment`（覆盖 AC3）
- [ ] `python3 -c 'import os, re, subprocess; from pathlib import Path; run=lambda c: subprocess.check_output(c, text=True); extract=lambda t: (lambda ls: "".join(ls))([l for l in (lambda a: a[next(i for i,x in enumerate(a) if x.startswith("  test:")):])(t.splitlines(True))][:next((j for j,x in enumerate((lambda a: a[next(i for i,x in enumerate(a) if x.startswith("  test:")):])(t.splitlines(True))[1:],1) if re.match(r"^  [^ ]",x)), None)]); st=run(["libra","status","--porcelain=v1","--",".github/workflows/base.yml"]).strip(); base_ref=os.environ.get("PRE_CARD_BASE") or ("HEAD" if st else "HEAD~1"); old=extract(run(["libra","--no-pager","show",f"{base_ref}:.github/workflows/base.yml"])); wt=extract(Path(".github/workflows/base.yml").read_text(encoding="utf-8")); assert "name: compat-offline-core" in old and old==wt, "compat-offline-core modified"'`（零临时文件纯内存精确作用域提取 `jobs.test` / `compat-offline-core` 子树相对前置基线逐字节比对，任意改动即失败，覆盖 AC5）

**Full-suite trigger:** `T-1: 修改 .github/workflows/base.yml CI 工作流`

**Dependencies:** `RB-11t`、`DEP-RB-02`

**Deliverables:** `N/A`

**Implementation write set:** `.github/workflows/base.yml`、`tests/storage_concurrency_stress_test.rs`、`tests/INDEX.md`

**Release write set:** `Inherited`

**Files likely touched:** 上列 3 个文件（行为/CI 落点 1 个：`.github/workflows/base.yml`）。

**Docs and compatibility impact:** `N/A`（仅 CI 矩阵与新测试目标，同步 `tests/INDEX.md`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=isolated redb ci job stress and job-diff guard; recovery=revert workflow job; complete=yes; self-contained=yes; AC=6/8; VER=7/8; landing=1; prod-files=1; scope=S; deps=RB-11t,DEP-RB-02; writeset=序列化于 RB-11t; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-13: 接入核心内部门面 12 文件至 `crate::storage`（Track B — `REL-RB-01` 子卡 1/29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区 `../libra-redb-integration` 内，将 `src/internal/` 下 12 个核心存储门面文件（`branch.rs`、`config.rs`、`dirty.rs`、`head.rs`、`metadata.rs`、`notes.rs`、`reflog.rs`、`revision_ordinal.rs`、`tag.rs`、`vault.rs`、`workspace.rs`、`worktree_scope.rs`）按 `ADR-RB-08` 双签名保签模式接入 `crate::storage`：原样保留全部既有 `*_with_conn`（`ConnectionTrait`）签名供 SQLite 与其它 Agent 消费，并行新增 `*_with_store(&dyn GitStore / &dyn ConfigStore)` 路径；`vault.rs` 仅适配 `L263-280` 配置键值调用，保持 `.libra/vault.db` 不变。

**Out of scope:**
- 修改 `src/internal/db.rs`、`src/utils/shared_perm.rs` 或 `.libra/vault.db` 自身 `libvault` 存储：严禁修改（`GC-RB-06`）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `head.rs:171,324` 暴露接收 `ConnectionTrait` 的公共函数；`vault.rs:40,263-280` 分别使用 `vault.db` 与 `ConfigKv` | `src/internal/head.rs:7,171,324`；`src/internal/vault.rs:40,263-280`；`src/internal/worktree_scope.rs:129` |

**Acceptance criteria:**
- [ ] 上列 12 个门面文件在 `Redb` 后端下全部经 `GitStore`/`ConfigStore`/`MetadataStore` 执行，不打开 `.libra/libra.db`。
- [ ] 所有既有 `*_with_conn<C: ConnectionTrait>` 公共签名保持原样且在 `Sqlite` 后端行为不变。
- [ ] `GlobalConfig` 与 `SystemConfig` 在 `config.rs` 中遵循 `ADR-RB-02` #2（先检作用域 `config-format.json`，再按选定运行时后端分派，纯红库 fail-closed 不返回空配置）。
- [ ] `vault.rs` 继续由 `libvault` 管理 `.libra/vault.db`，仅配置键读写路径经 `ConfigStore` 适配。
- [ ] `worktree_scope.rs` 在 `Redb` 仓库上基于同一作用域观测解析 `.libra/redb/git.gen-<id>.redb`（无 `.libra/libra.db` 时正常工作）。
- [ ] `src/internal/db.rs`、`src/utils/shared_perm.rs` 零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::head::tests::redb_head_branch_tag_reflog_notes_and_worktree_scope) | test(=internal::config::tests::redb_config_cascade_global_system_fallback_and_vault_kv)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::head::tests::redb_head_branch_tag_reflog_notes_and_worktree_scope)'`（new，覆盖 AC1、AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::config::tests::redb_config_cascade_global_system_fallback_and_vault_kv)'`（new，覆盖 AC3、AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_global_config_schema_future`（覆盖 AC2；AC6 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`，由 `RB-30` 承接 `T-2` 全量）

**Dependencies:** `RB-12`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/branch.rs`、`src/internal/config.rs`、`src/internal/dirty.rs`、`src/internal/head.rs`、`src/internal/metadata.rs`、`src/internal/notes.rs`、`src/internal/reflog.rs`、`src/internal/revision_ordinal.rs`、`src/internal/tag.rs`、`src/internal/vault.rs`、`src/internal/workspace.rs`、`src/internal/worktree_scope.rs`

**Release write set:** `N/A`（`family child of REL-RB-01`）

**Files likely touched:** 上列 12 个文件（1 个生产目录 `src/internal/`）。

**Docs and compatibility impact:** `N/A`（内部接线，不改变用户面）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`（`family child`）

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire internal core 12 facades to store; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=12; scope=M; deps=RB-12; writeset=序列化于 RB-12; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-14: 接入 OpLog、Change 谱系与全局配置迁移 10 文件（Track B — `REL-RB-01` 子卡 2/29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/internal/operation/{middleware,restore,snapshot,store}.rs`（4 文件）、`src/internal/change/{builder,genealogy,import,resolve,store}.rs`（5 文件）与 `src/internal/config/global_migration.rs`（1 文件）按双签名保签模式接入 `crate::storage`，使 OpLog v2、Change 谱系与全局配置路径检测在 `Redb` 下不触碰 `.libra/libra.db`。

**Out of scope:**
- 修改 `src/internal/db.rs` 或其它子目录：由对应子卡承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `operation/middleware.rs:70`、`operation/snapshot.rs:423` 与 `config/global_migration.rs:4` 直接拼接 `util::DATABASE` / `config.db` | `src/internal/operation/middleware.rs:70`；`src/internal/operation/snapshot.rs:423`；`src/internal/config/global_migration.rs:4` |

**Acceptance criteria:**
- [ ] `operation/{middleware,restore,snapshot,store}.rs` 在 `Redb` 仓库上完成操作记录、快照与恢复，不打开 `.libra/libra.db`。
- [ ] `change/{builder,genealogy,import,resolve,store}.rs` 在 `Redb` 仓库上完成 Change ID 构造、谱系追踪与解析。
- [ ] `config/global_migration.rs` 按 `ADR-RB-02` 区分 `config.redb` 与未迁移 `config.db` 并遵守 `GC-13` 隔离。
- [ ] 既有 SQLite `operation_dag` 路径零回归。
- [ ] 禁改文件（`db.rs`、`capture/{store,detach}.rs`、`shared_perm.rs`）零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::operation::store::tests::redb_oplog_v2_snapshot_and_restore_roundtrip) | test(=internal::change::store::tests::redb_change_genealogy_and_global_migration_compat)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::operation::store::tests::redb_oplog_v2_snapshot_and_restore_roundtrip)'`（new，覆盖 AC1）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::change::store::tests::redb_change_genealogy_and_global_migration_compat)'`（new，覆盖 AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test operation_dag`（覆盖 AC4；AC5 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-13`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/operation/middleware.rs`、`src/internal/operation/restore.rs`、`src/internal/operation/snapshot.rs`、`src/internal/operation/store.rs`、`src/internal/change/builder.rs`、`src/internal/change/genealogy.rs`、`src/internal/change/import.rs`、`src/internal/change/resolve.rs`、`src/internal/change/store.rs`、`src/internal/config/global_migration.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 10 个文件（3 个生产目录：`src/internal/operation/`、`src/internal/change/`、`src/internal/config/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire oplog change global-mig 10 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=3; prod-files=10; scope=M; deps=RB-13; writeset=序列化于 RB-13; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-15: 接入 Sequencer、Layer、Sparse 与 Obliteration 4 模块（Track B — `REL-RB-01` 子卡 3/29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/internal/sequencer/mod.rs`、`src/internal/layer/mod.rs`、`src/internal/sparse/mod.rs` 与 `src/internal/obliteration/mod.rs`（4 目录、4 文件）按双签名保签模式接入 `crate::storage`，使序列器互斥锁、层叠加、稀疏视图与对象抹除围栏在 `Redb` 仓库完整工作。

**Out of scope:**
- 命令层调用方：由 `RB-17`..`RB-20` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `sparse/mod.rs:168`、`obliteration/mod.rs:24`、`sequencer/mod.rs:6` 直接经 SQLite 读写仓库状态表 | `src/internal/sparse/mod.rs:168`；`src/internal/obliteration/mod.rs:24-90`；`src/internal/sequencer/mod.rs:6` |

**Acceptance criteria:**
- [ ] `sequencer/mod.rs` 在 `Redb` 仓库上按 `worktree_id` 隔离保存/恢复 `sequence_state` 并执行对称互斥检查。
- [ ] `layer/mod.rs` 在 `Redb` 仓库上完成 `layer`/`layer_path` 读写与清理。
- [ ] `sparse/mod.rs` 在 `Redb` 仓库上完成 `sparse_view`/`sparse_view_meta` 持久化（不依赖 `libra.db`）。
- [ ] `obliteration/mod.rs` 在 `Redb` 仓库上读写 `object_obliteration` 围栏并拦截抹除对象。
- [ ] SQLite 后端既有 `sequencer_snapshot_roundtrip` 路径零回归。
- [ ] 禁改文件（`db.rs`、`capture/{store,detach}.rs`、`shared_perm.rs`）零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::sequencer::tests::redb_sequencer_mutex_and_worktree_isolation) | test(=internal::sparse::tests::redb_layer_sparse_and_obliteration_roundtrip)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::sequencer::tests::redb_sequencer_mutex_and_worktree_isolation)'`（new，覆盖 AC1）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::sparse::tests::redb_layer_sparse_and_obliteration_roundtrip)'`（new，覆盖 AC2、AC3、AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test sequencer_snapshot_roundtrip`（覆盖 AC5；AC6 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-14`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/sequencer/mod.rs`、`src/internal/layer/mod.rs`、`src/internal/sparse/mod.rs`、`src/internal/obliteration/mod.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 4 个文件（4 个生产目录）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire sequencer overlays obliteration 4 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=4; prod-files=4; scope=M; deps=RB-14; writeset=序列化于 RB-14; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-16: 接入 CLI 启动后端解析、仓库发现工具与 Status 缓存 3 文件（Track B — `REL-RB-01` 子卡 4/29，拆出 `RB-16b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/cli.rs`、`src/utils/util.rs` 与 `src/command/status/cache.rs`（3 目录、3 文件）接入 `crate::storage`，使仓库发现（`util.rs:213-224,475-494,808-835`）同时识别 `storage-format.json` / `.libra/redb/` 与 `libra.db`（继续拒绝 `$LIBRA_HOME` 采纳）、status 缓存经只读 `GitStore` 读写，并将 `src/cli.rs` 入口路由至 `crate::storage::backend`（保持 `RB-29a` 前的编译期门控不变）。

**Out of scope:**
- 本地传输协议、对象索引与路径大小写 4 文件：已按 `G-03`/`G-09` 拆出由 `RB-16b` 承接。
- 修改 `src/utils/shared_perm.rs`（禁改，`GC-RB-06`）。
- 新增 `agent daemon` 子命令归 `RB-29`；解锁 `Redb` CLI/迁移门控归 `RB-29a`。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `util.rs:221,491,832` 仅检查 `libra.db` 判断有效存储目录；`src/cli.rs:98,2535` 尚未接入 `crate::storage::backend` | `src/utils/util.rs:213-224,475-494,808-835`；`src/command/status/cache.rs`；`src/cli.rs:98,2535` |

**Acceptance criteria:**
- [ ] `util.rs` 的 `is_valid_storage_dir` 与 `is_terminal_common_storage` 识别 `storage-format.json`/`.libra/redb/`（无 `libra.db` 时仍正确定位普通仓库）。
- [ ] `util.rs` 的 bare 仓库探测识别红库 bare 目录（无 `libra.db` 时仍正确定位）。
- [ ] `util.rs` 对含 `storage-format.json` 或 `redb/` 的 `$LIBRA_HOME` 仍严格拒绝采纳为仓库目录。
- [ ] `command/status/cache.rs` 在 `Redb` 仓库上经只读 `GitStore` 读写脏树缓存，绝不升写打开。
- [ ] `src/cli.rs` 将生产 CLI 入口的后端解析与 `finalized == false`/未迁移库检查路由至 `crate::storage::backend`。
- [ ] `RB-29a` 解锁前，经 `src/cli.rs` 入口传入 `LIBRA_STORAGE_BACKEND=redb` 的真实 CLI 请求无条件返回 `LBR-CLI-003`（承接 `RB-01b` 生产入口门控验收）。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=utils::util::tests::redb_repo_discovery_and_home_rejection) | test(=command::status::cache::tests::redb_status_cache_readonly_and_cli_gate_entry)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=utils::util::tests::redb_repo_discovery_and_home_rejection)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::status::cache::tests::redb_status_cache_readonly_and_cli_gate_entry)'`（new，覆盖 AC4、AC5、AC6）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-15`

**Deliverables:** `N/A`

**Implementation write set:** `src/cli.rs`、`src/utils/util.rs`、`src/command/status/cache.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 3 个文件（3 个生产目录：`src/`、`src/utils/`、`src/command/status/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire cli discovery and status cache 3 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=3; prod-files=3; scope=M; deps=RB-15; writeset=序列化于 RB-15; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-16b: 接入本地传输协议、客户端对象索引与路径大小写 4 文件（Track B — `REL-RB-01` 子卡 5/29，自 `RB-16` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/internal/protocol/{local_client,local_push}.rs` 与 `src/utils/{client_storage,path_case}.rs`（2 目录、4 文件）接入 `crate::storage`，使本地远端拉取/推送、对象索引与路径大小写判定全面支持 `Redb` 后端且零回归 SQLite 本地推送。

**Out of scope:**
- 修改 `src/utils/shared_perm.rs`（禁改，`GC-RB-06`）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `local_push.rs:49` 与 `path_case.rs:202` 硬编码 `libra.db` | `src/internal/protocol/local_client.rs`；`src/internal/protocol/local_push.rs:49`；`src/utils/client_storage.rs`；`src/utils/path_case.rs:202` |

**Acceptance criteria:**
- [ ] `local_client.rs` 支持从本地 `Redb` 远端仓库拉取，不要求目标存在 `libra.db`。
- [ ] `local_push.rs` 支持向本地 `Redb` 远端仓库推送，不要求目标存在 `libra.db`。
- [ ] `client_storage.rs` 在 `Redb` 仓库上通过 `GitStore` 读写对象索引。
- [ ] `path_case.rs` 在 `Redb` 仓库上通过 `ConfigStore` 读取大小写配置。
- [ ] 既有 SQLite `compat_local_push_no_cwd_switch` 零回归（禁改文件零改动由 `GC-RB-06` 与 `RB-30` diff 门保障）。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::protocol::local_push::tests::redb_local_client_push_object_index_and_path_case)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::protocol::local_push::tests::redb_local_client_push_object_index_and_path_case)'`（new，覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_local_push_no_cwd_switch`（覆盖 AC5）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-16`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/protocol/local_client.rs`、`src/internal/protocol/local_push.rs`、`src/utils/client_storage.rs`、`src/utils/path_case.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 4 个文件（2 个生产目录：`src/internal/protocol/`、`src/utils/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire local transport obj-index path-case 4 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=4; scope=M; deps=RB-16; writeset=序列化于 RB-16; release=family child; split-from=RB-16; exception=N/A`

---

### Task RB-17: 接入 `init` 与 `clone` 红库初始化及全新仓 Spool 墓碑 2 文件（Track B — `REL-RB-01` 子卡 6/29，拆出 `RB-17b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/init.rs` 与 `src/command/clone.rs`（1 目录、2 文件）接入 `crate::storage`，在 `Redb` 后端下初始化 `.libra/redb/git.gen-00000001.redb`、`.libra/private/capture.gen-00000001.redb`、`.libra/storage-format.json` 并同步调用 `init_redb_capture_spool_tombstones` 建立 `0400` spool 墓碑，扩展 `clone.rs:786` `REPO_MARKERS` 识别红库目录，同时严禁改动 `src/command/init.rs:1-20` 受保护头部与导入块（`GC-RB-06`）。

**Out of scope:**
- 其余 10 个核心 Porcelain Git 命令：已按 `G-03`/`G-09` 拆出由 `RB-17b` 承接。
- 修改 `src/command/init.rs:1-20` 顶部受保护导入块或新增用户可见 CLI flag。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `clone.rs:786` `REPO_MARKERS` 仅列 `libra.db`；`init.rs:1-20` 顶部导入块受 `GC-RB-06` 保护 | `src/command/clone.rs:786`；`src/command/init.rs:1-20` |

**Acceptance criteria:**
- [ ] `init` 在 `Redb` 后端下初始化 `.libra/redb/git.gen-00000001.redb`、`.libra/private/capture.gen-00000001.redb` 与 `.libra/storage-format.json`（不创建 `libra.db`）。
- [ ] `init` 在 `Redb` 后端下同步调用 `init_redb_capture_spool_tombstones` 建立 `.libra/private/capture-spool/{tmp,ready,quarantine}` `0400` 墓碑（旧 SQLite Hook 调 `ensure_spool` 立即 fail-closed）。
- [ ] `clone` 在 `Redb` 后端下完成红库初始化并同步建立 `0400` `capture-spool` 墓碑。
- [ ] `clone.rs` `REPO_MARKERS` 识别已初始化红库目录（`storage-format.json`/`redb`）。
- [ ] `src/command/init.rs:1-20` 受保护导入块零改动（`GC-RB-06`）。
- [ ] 既有 SQLite `compat_checkout_branch_startpoint` 路径零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::init::tests::redb_init_and_clone_with_spool_tombstones_and_repo_markers)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::init::tests::redb_init_and_clone_with_spool_tombstones_and_repo_markers)'`（new，覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_checkout_branch_startpoint`（覆盖 AC6；AC5 由 `RB-30` `init.rs:1-20` `diff -u` 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-16b`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/init.rs`、`src/command/clone.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/command/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire init and clone redb with spool tombstones; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-16b; writeset=序列化于 RB-16b; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-17b: 接入核心 Porcelain Git 读写命令组 A 其余 10 文件（Track B — `REL-RB-01` 子卡 7/29，自 `RB-17` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/` 下其余 10 个高频核心命令（`commit.rs`、`branch.rs`、`tag.rs`、`switch.rs`、`reset.rs`、`log.rs`、`reflog.rs`、`remote.rs`、`revision.rs`、`update_ref.rs`）接入 `crate::storage`，确保只读命令（`log`/`reflog`/`revision`）严格走 `open_read_only`（`ADR-RB-03`）并在并发写入下线性化无锁错。

**Out of scope:**
- 传输与维护命令组 B：由 `RB-18` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `log.rs:1567`、`reset.rs:32`、`switch.rs:18,475`、`commit.rs:2394` 直接打开 SQLite | `src/command/log.rs:1567`；`src/command/reset.rs:32`；`src/command/switch.rs:18,475`；`src/command/commit.rs:2394` |

**Acceptance criteria:**
- [ ] `commit`、`branch`、`tag`、`switch`、`reset`、`remote`、`update_ref` 在 `Redb` 仓库上完成全链路写操作，不创建或打开 `.libra/libra.db`。
- [ ] `log`、`reflog`、`revision` 通过 `open_read_only` 无锁执行，在另一进程持续写入期间零锁错。
- [ ] 多进程并发验证：8 进程并发 `log`/`status` + 2 进程循环 `commit`/`update_ref` 全部退出 0 并保持引用线性化。
- [ ] 既有 SQLite `compat_commit_identity_date` 路径零回归。
- [ ] 禁改文件（`db.rs`、`capture/{store,detach}.rs`、`shared_perm.rs`）零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::commit::tests::redb_porcelain_commit_branch_tag_switch_reset_roundtrip) | test(=command::log::tests::redb_multiprocess_readonly_log_with_concurrent_commits)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::commit::tests::redb_porcelain_commit_branch_tag_switch_reset_roundtrip)'`（new，覆盖 AC1）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::log::tests::redb_multiprocess_readonly_log_with_concurrent_commits)'`（new，覆盖 AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_commit_identity_date`（覆盖 AC4；AC5 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-17`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/commit.rs`、`src/command/branch.rs`、`src/command/tag.rs`、`src/command/switch.rs`、`src/command/reset.rs`、`src/command/log.rs`、`src/command/reflog.rs`、`src/command/remote.rs`、`src/command/revision.rs`、`src/command/update_ref.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 10 个文件（1 个生产目录 `src/command/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire porcelain git commands core 10 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=10; scope=M; deps=RB-17; writeset=序列化于 RB-17; release=family child; split-from=RB-17; exception=N/A`

---

### Task RB-18: 接入传输、维护与只读检查命令组 B（10 文件，Track B — `REL-RB-01` 子卡 8/29，拆出 `RB-18b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/` 下 10 个传输、维护与只读检查命令（`fetch.rs`、`push.rs`、`bundle.rs`、`cat_file.rs`、`fast_export.rs`、`fast_import.rs`、`gc.rs`、`grep.rs`、`maintenance.rs`、`control_lock.rs`）接入 `crate::storage`，确保写命令经 `crate::storage` 读写且 `cat_file`/`grep`/`fast_export` 保持严格只读打开（`ADR-RB-03`）。

**Out of scope:**
- `fsck.rs` 显式授权崩溃修复与跨库意图对账，以及 `alternates.rs` 跨后端解析：已按 `G-03`/`G-09` 拆出由 `RB-18b` 承接。
- 其余 `src/command/` 文件：由 `RB-19`、`RB-20`、`RB-20c`、`RB-20b` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `cat_file.rs:1406`、`control_lock.rs:300`、`maintenance.rs:3558` 直接打开 `libra.db` | `src/command/cat_file.rs:1406`；`src/command/control_lock.rs:300`；`src/command/maintenance.rs:3558` |

**Acceptance criteria:**
- [ ] `fetch`、`push`、`bundle`、`fast_import`、`gc`、`maintenance`、`control_lock` 在 `Redb` 仓库上全部经 `crate::storage` 读写（不打开 `libra.db`）。
- [ ] `cat_file`、`grep`、`fast_export` 在 `Redb` 仓库上严格使用 `open_read_only` 执行。
- [ ] 上述 3 个只读命令遇 `DatabaseError::RepairAborted` 时立即 fail-closed 不升写。
- [ ] 既有 SQLite `compat_import_export_roundtrip` 路径零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::maintenance::tests::redb_transport_maintenance_and_readonly_commands)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::maintenance::tests::redb_transport_maintenance_and_readonly_commands)'`（new，覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_import_export_roundtrip`（覆盖 AC4）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-17b`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/fetch.rs`、`src/command/push.rs`、`src/command/bundle.rs`、`src/command/cat_file.rs`、`src/command/fast_export.rs`、`src/command/fast_import.rs`、`src/command/gc.rs`、`src/command/grep.rs`、`src/command/maintenance.rs`、`src/command/control_lock.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 10 个文件（1 个生产目录 `src/command/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire transport maint readonly commands B 10 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=10; scope=M; deps=RB-17b; writeset=序列化于 RB-17b; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-18b: 接入 `fsck` 显式修复对账与 `alternates` 跨后端解析 2 文件（Track B — `REL-RB-01` 子卡 9/29，自 `RB-18` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/fsck.rs` 与 `src/command/alternates.rs`（1 目录、2 文件）接入 `crate::storage`，落实 `fsck.rs` 作为显式授权修复入口完成 `redb` 崩溃恢复与 `cross_store::reconcile_pending_intents()`（只读文件系统 fail-closed 报 `LBR-IO-001`），以及 `alternates.rs` 分别解析指向外部 SQLite 与外部 Redb 仓库。

**Out of scope:**
- 其余 `src/command/` 文件：由 `RB-19`..`RB-20b` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `alternates.rs:119` 与 `fsck.rs` 直接打开 `libra.db` | `src/command/alternates.rs:119`；`src/command/fsck.rs`；`ADR-RB-03`、`ADR-RB-06` |

**Acceptance criteria:**
- [ ] `fsck` 在可写红库上显式完成 `redb` 崩溃恢复。
- [ ] `fsck` 在可写红库上显式执行 `cross_store::reconcile_pending_intents()` 跨库意图对账。
- [ ] `fsck` 在只读挂载文件系统上 fail-closed 报 `LBR-IO-001`。
- [ ] `alternates` 跨库解析支持指向外部 SQLite（`libra.db`）仓库。
- [ ] `alternates` 跨库解析支持指向外部 Redb（`storage-format.json`/`redb/`）仓库。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::fsck::tests::redb_fsck_repair_reconcile_ro_fs_and_alternates)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::fsck::tests::redb_fsck_repair_reconcile_ro_fs_and_alternates)'`（new，覆盖 AC1–AC5）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-18`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/fsck.rs`、`src/command/alternates.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/command/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire fsck explicit repair and alternates 2 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=2; scope=S; deps=RB-18; writeset=序列化于 RB-18; release=family child; split-from=RB-18; exception=N/A`

---

### Task RB-19: 接入 Merge、Rebase、Bisect 与 Cherry-Pick 命令组 C（12 文件，Track B — `REL-RB-01` 子卡 10/29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/{bisect,cherry_pick,merge,merge_base,rebase}.rs`（5 文件）与 `src/command/merge/{conflict,content,output,rename_merge,tree_merge,virtual_base,workdir}.rs`（7 文件）接入 `crate::storage`，使三方合并、重放、二分与拣选在 `Redb` 仓库完整运行并保持稀疏合并门控语义不变。

**Out of scope:**
- `src/command/rebase/state.rs`：归 `RB-20` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `merge.rs:42` 及 `command/merge/*.rs`、`bisect.rs`、`cherry_pick.rs`、`rebase.rs` 直接引用 `get_db_conn_instance` / `sea_orm` | `src/command/merge.rs:42`；`src/command/cherry_pick.rs:43,853` |

**Acceptance criteria:**
- [ ] `merge` 与 `merge_base`（含 7 个 `command/merge/*` 子模块）在 `Redb` 仓库完成快进、三方合并、冲突暂停与恢复。
- [ ] `rebase` 在 `Redb` 仓库完成应用、冲突暂停、`--continue` 与 `--abort`。
- [ ] `cherry_pick` 在 `Redb` 仓库经 `GitStore` 持久化 `sequence_state` 并支持 `--continue`/`--abort`。
- [ ] `bisect` 在 `Redb` 仓库完成 `start`/`good`/`bad`/`reset` 状态往返。
- [ ] 既有 SQLite `compat_bisect_subcommand_surface` 路径零回归。
- [ ] 禁改文件（`db.rs`、`capture/{store,detach}.rs`、`shared_perm.rs`）零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::merge::tests::redb_merge_rebase_cherry_pick_and_bisect_workflows)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::merge::tests::redb_merge_rebase_cherry_pick_and_bisect_workflows)'`（new，覆盖 AC1、AC2、AC3、AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_bisect_subcommand_surface`（覆盖 AC5；AC6 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-18b`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/bisect.rs`、`src/command/cherry_pick.rs`、`src/command/merge.rs`、`src/command/merge_base.rs`、`src/command/rebase.rs`、`src/command/merge/conflict.rs`、`src/command/merge/content.rs`、`src/command/merge/output.rs`、`src/command/merge/rename_merge.rs`、`src/command/merge/tree_merge.rs`、`src/command/merge/virtual_base.rs`、`src/command/merge/workdir.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 12 个文件（2 个生产目录：`src/command/`、`src/command/merge/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire merge bisect rebase commands C 12 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=2; prod-files=12; scope=M; deps=RB-18b; writeset=序列化于 RB-18b; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-20: 接入 Config、Config Doctor 与 Config Repair 命令 `Redb` 运行时路径 3 文件（Track B — `REL-RB-01` 子卡 11/29，拆出 `RB-20c`、`RB-20b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/config.rs`、`src/command/config/doctor.rs` 与 `src/command/config/repair.rs`（2 目录、3 文件）接入 `crate::storage`，按 `ADR-RB-02` #2 分支 B 落实 `Redb` 运行时在仓库/`--global`/`--system` 作用域的配置读写、诊断与拒收策略。

**Out of scope:**
- 双特性构建 + 默认 `Sqlite` 运行时（`RB-30`/`RB-32` 默认态）下 `--global` 与 `--system` 既有 `config.db` 读写与空目录建库零破坏分派：已按 `G-03`/`G-09` 拆出由 `RB-20c` 承接。
- `worktree`、`op`、`memory`、`automation`、`hooks` 与 `rebase/state` 其余 9 文件：由 `RB-20b` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `config.rs:49` 与 `config/repair.rs:187` 直接引用 `util::DATABASE` / `config.db` | `src/command/config.rs:49,257-295`；`src/command/config/repair.rs:187`；`ADR-RB-02` #2 |

**Acceptance criteria:**
- [ ] `config`、`config/doctor` 与 `config/repair` 在 `Redb` 运行时下于 `Redb` 仓库上按 `crate::storage` 正确诊断与读写（不打开 `.libra/libra.db`）。
- [ ] `config` 命令在 `Redb` 运行时下于隔离 `--global` 夹具上按 `ADR-RB-02` #2 分支 B 正确执行读写/拒绝策略。
- [ ] `config` 命令在 `Redb` 运行时下于隔离 `--system` 夹具上按 `ADR-RB-02` #2 分支 B 正确执行读写/拒绝策略。
- [ ] SQLite 既有 `compat_global_config_schema_future` 路径零回归。
- [ ] 禁改文件（`db.rs`、`capture/{store,detach}.rs`、`shared_perm.rs`）零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=command::config::tests::redb_repo_global_system_config_doctor_and_repair)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=command::config::tests::redb_repo_global_system_config_doctor_and_repair)'`（new，覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_global_config_schema_future`（覆盖 AC4；AC5 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-19`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/config.rs`、`src/command/config/doctor.rs`、`src/command/config/repair.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 3 个文件（2 个生产目录：`src/command/`、`src/command/config/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire config doctor repair redb runtime 3 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=3; scope=S; deps=RB-19; writeset=序列化于 RB-19; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-20c: 双特性构建 `Sqlite` 运行时 Global/System Config CLI 分派零破坏接线（Track B — `REL-RB-01` 子卡 12/29，自 `RB-20` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区于 `src/command/config.rs` 落实并逐域实证 `ADR-RB-02` #2 分支 A：双特性构建 + 默认 `Sqlite` 运行时（`RB-30`/`RB-32` 默认态）下，`libra config set --global` 与 `libra config set --system` 经 `config-migration.lock` 校验后分别对既有 `config.db` 正常读写以及在空目录新建 `config.db` + `config-format.json(backend="sqlite", finalized=true)`（绝不创建 `config.redb`）。

**Out of scope:**
- `worktree`、`op`、`memory`、`automation`、`hooks` 与 `rebase/state` 9 文件：由 `RB-20b` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `RB-30`/`RB-32` 双特性默认 SQLite 态须对 `--global` 与 `--system` 各自独立验证既有库与空目录零破坏 | `src/command/config.rs:49,257-295`；`ADR-RB-02` #2 分支 A |

**Acceptance criteria:**
- [ ] 双特性构建 + `Sqlite` 运行时下，`libra config set --global` 经 `config-migration.lock` 校验后对已有 `config.db` 正常读写（绝不创建 `config.redb`）。
- [ ] 双特性构建 + `Sqlite` 运行时下，`libra config set --system` 经 `config-migration.lock` 校验后对已有 `config.db` 正常读写（绝不创建 `config.redb`）。
- [ ] 双特性构建 + `Sqlite` 运行时下，目标全局目录为空时 `libra config set --global` 新建 `config.db` + `config-format.json(backend="sqlite", finalized=true)`（绝不创建 `config.redb`）。
- [ ] 双特性构建 + `Sqlite` 运行时下，目标系统目录为空时 `libra config set --system` 新建 `config.db` + `config-format.json(backend="sqlite", finalized=true)`（绝不创建 `config.redb`）。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=command::config::tests::dual_feature_global_system_config_cli_runtime_dispatch)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=command::config::tests::dual_feature_global_system_config_cli_runtime_dispatch)'`（new，覆盖 AC1–AC4）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-20`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/config.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 1 个文件（1 个生产目录 `src/command/`，与 `RB-20` 串行复用 `src/command/config.rs`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire dual-feature sqlite runtime global+system config cli; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=1; scope=S; deps=RB-20; writeset=序列化于 RB-20; release=family child; split-from=RB-20; exception=N/A`

---

### Task RB-20b: 接入 Worktree、Op、Memory、Automation、Hooks 与 Rebase State 9 文件（Track B — `REL-RB-01` 子卡 13/29，自 `RB-20` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/{op,worktree,worktree-fuse,memory,automation,hooks}.rs`（6 文件）、`src/command/worktree/{doctor,operations}.rs`（2 文件）与 `src/command/rebase/state.rs`（1 文件）共 3 目录、9 文件接入 `crate::storage`，完成 Git 命令侧 100% 接线闭环。

**Out of scope:**
- Agent/AI/Cloud 命令组：由 `RB-21`..`RB-29` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `worktree.rs:633`、`worktree/doctor.rs:751`、`hooks.rs:328`、`memory.rs:114` 直接引用 `util::DATABASE` | `src/command/worktree.rs:633`；`src/command/worktree/doctor.rs:751`；`src/command/hooks.rs:328`；`src/command/memory.rs:114` |

**Acceptance criteria:**
- [ ] `worktree`、`worktree-fuse`、`worktree/{doctor,operations}` 在主工作树与 linked worktree 共享 `.libra/redb/` 下全链路通过。
- [ ] `op`（`log`/`undo`/`redo`/`restore`）与 `rebase/state` 在 `Redb` 仓库上完成操作恢复。
- [ ] `memory`、`automation` 与 `hooks` 在 `Redb` 仓库上经 `crate::storage` 读写（不打开 `.libra/libra.db`）。
- [ ] 既有 SQLite `op_undo_redo` 路径零回归。
- [ ] 禁改文件（`db.rs`、`capture/{store,detach}.rs`、`shared_perm.rs`）零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=command::worktree::tests::redb_worktree_op_memory_automation_and_hooks)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=command::worktree::tests::redb_worktree_op_memory_automation_and_hooks)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test op_undo_redo`（覆盖 AC4；AC5 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-20c`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/op.rs`、`src/command/worktree.rs`、`src/command/worktree-fuse.rs`、`src/command/memory.rs`、`src/command/automation.rs`、`src/command/hooks.rs`、`src/command/worktree/doctor.rs`、`src/command/worktree/operations.rs`、`src/command/rebase/state.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 9 个文件（3 个生产目录：`src/command/`、`src/command/worktree/`、`src/command/rebase/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire worktree op memory automation hooks 9 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=3; prod-files=9; scope=M; deps=RB-20c; writeset=序列化于 RB-20c; release=family child; split-from=RB-20; exception=N/A`

---

### Task RB-21: 接入 AI Capture 目录核心 12 文件（严禁触碰 `store.rs`/`detach.rs`，Track B — `REL-RB-01` 子卡 14/29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/internal/ai/capture/` 下 12 个非禁改生产文件（`catalog.rs`、`checkpoint.rs`、`key.rs`、`live.rs`、`live_checkpoint.rs`、`live_pipeline.rs`、`pending.rs`、`pending_identity.rs`、`pending_payload.rs`、`recovery.rs`、`snapshot.rs`、`summary.rs`）按双签名保签模式接入 `CaptureStore`、`GitStore` 与 `cross_store` 意图协调器：原样保留 `live_checkpoint.rs:147` 等接收 `DatabaseConnection` 的签名，并行新增 `*_with_capture(&dyn CaptureStore)` 路径，且**严禁触碰 `store.rs` 与 `detach.rs`**（与 `RB-17`..`RB-20` 写集完全不相交，可并行实现）。

**Out of scope:**
- `src/internal/ai/capture/store.rs` 与 `detach.rs`：全计划禁改（`GC-RB-06`）。
- 切换实时 Hook 入队至 `capture-queue/`：由 `RB-29` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `live_checkpoint.rs:147` 显式接收 `&sea_orm::DatabaseConnection`；`pending_identity.rs:242` 跨 `libra.db` 与 `capture.db` 发布 | `src/internal/ai/capture/live_checkpoint.rs:147`；`src/internal/ai/capture/pending_identity.rs:242` |

**Acceptance criteria:**
- [ ] 上列 12 文件在 `Redb` 仓库上全部经 `CaptureStore`/`GitStore` 完成会话、检查点、pending 与摘要读写，不打开 `capture.db` 或 `libra.db`。
- [ ] `live_checkpoint.rs:147` 等既有 `DatabaseConnection` 签名原样保留，SQLite 路径行为不变。
- [ ] 跨库检查点发布经 `cross_store` 持久化意图发件箱按 `GitStore → CaptureStore` 顺序提交（严禁跳代掩盖缺口）。
- [ ] `src/internal/ai/capture/store.rs` 与 `src/internal/ai/capture/detach.rs` 零改动。
- [ ] 既有 SQLite `agent_local_capture_smoke_test` 零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::capture::live_checkpoint::tests::redb_capture_catalog_checkpoint_and_cross_store_intent)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::capture::live_checkpoint::tests::redb_capture_catalog_checkpoint_and_cross_store_intent)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test agent_local_capture_smoke_test`（覆盖 AC5；AC4 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-16b`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/ai/capture/catalog.rs`、`src/internal/ai/capture/checkpoint.rs`、`src/internal/ai/capture/key.rs`、`src/internal/ai/capture/live.rs`、`src/internal/ai/capture/live_checkpoint.rs`、`src/internal/ai/capture/live_pipeline.rs`、`src/internal/ai/capture/pending.rs`、`src/internal/ai/capture/pending_identity.rs`、`src/internal/ai/capture/pending_payload.rs`、`src/internal/ai/capture/recovery.rs`、`src/internal/ai/capture/snapshot.rs`、`src/internal/ai/capture/summary.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 12 个文件（1 个生产目录 `src/internal/ai/capture/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire ai capture 12 files excluding store-detach; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=1; prod-files=12; scope=M; deps=RB-16b; writeset=no-overlap with RB-17..20b; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-22: 接入 AI History、Coverage、Import 与 Export 9 文件（Track B — `REL-RB-01` 子卡 15/29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/internal/ai/` 下 9 个非第三方生产文件（`agent_import.rs`、`capture_scope.rs`、`checkpoint_reader.rs`、`commit_recovery.rs`、`coverage_gate.rs`、`export_job.rs`、`history.rs`、`subagent_content.rs`、`traces.rs`）按双签名保签模式接入 `CaptureStore`/`GitStore`，保持第三方读取器 `provider_index_io.rs` 原样不变。

**Out of scope:**
- `src/internal/ai/provider_index_io.rs`：外部 OpenCode SQLite 只读解析器，不替换（非目标）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| 上列 9 个文件读写 `agent_import_*`、`agent_coverage_*`、`agent_export_job`、`agent_subagent_*` 与 `traces` 关联目录 | 2026-11-21 AST 扫描 `src/internal/ai/*.rs` |

**Acceptance criteria:**
- [ ] `agent_import` 与 `export_job` 在 `Redb` 仓库上读写导入身份、墓碑与导出任务，保持墓碑拒绝重导语义。
- [ ] `capture_scope`、`coverage_gate` 与 `subagent_content` 在 `Redb` 仓库上完成声明、冲突与修订查询。
- [ ] `checkpoint_reader`、`commit_recovery`、`history` 与 `traces` 在 `Redb` 仓库上经只读快照查询历史与检查点。
- [ ] `src/internal/ai/provider_index_io.rs` 零改动。
- [ ] 既有 SQLite `agent_capture_migration_test` 路径零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::agent_import::tests::redb_ai_import_coverage_history_and_export_services)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::agent_import::tests::redb_ai_import_coverage_history_and_export_services)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test agent_capture_migration_test`（覆盖 AC5；AC4 由 `RB-30` diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-21`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/ai/agent_import.rs`、`src/internal/ai/capture_scope.rs`、`src/internal/ai/checkpoint_reader.rs`、`src/internal/ai/commit_recovery.rs`、`src/internal/ai/coverage_gate.rs`、`src/internal/ai/export_job.rs`、`src/internal/ai/history.rs`、`src/internal/ai/subagent_content.rs`、`src/internal/ai/traces.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 9 个文件（1 个生产目录 `src/internal/ai/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire ai history coverage export 9 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=1; prod-files=9; scope=M; deps=RB-21; writeset=序列化于 RB-21; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-23: 接入 AI Memory、Automation、Permission 与 Projection 10 文件（Track B — `REL-RB-01` 子卡 16/29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/internal/ai/memory/{derive_commit,error,projection,reader}.rs`（4 文件）、`src/internal/ai/automation/{history,runtime}.rs`（2 文件）、`src/internal/ai/permission/{approved,runtime_cache}.rs`（2 文件）与 `src/internal/ai/projection/{rebuild,resolver}.rs`（2 文件）共 4 目录、10 文件接入 `crate::storage`。

**Out of scope:**
- `src/internal/ai/hooks/`、`mcp_server/`、`observed_agents/`：由 `RB-24` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `automation/runtime.rs:101` 与 `permission/runtime_cache.rs:89` 直接拼接 `util::DATABASE`；`memory/{projection,reader}.rs` 操作 5 张 `memory_*` 表 | `src/internal/ai/automation/runtime.rs:101`；`src/internal/ai/permission/runtime_cache.rs:89`；`src/internal/ai/memory/projection.rs` |

**Acceptance criteria:**
- [ ] `memory/{derive_commit,error,projection,reader}.rs` 在 `Redb` 仓库上完成 5 张 `memory_*` 表的派生、重建与路径搜索。
- [ ] `automation/{history,runtime}.rs` 在 `Redb` 仓库上读写 `automation_log`（不打开 `libra.db`）。
- [ ] `permission/{approved,runtime_cache}.rs` 在 `Redb` 仓库上读写 `approved_permission` 缓存与审批记录。
- [ ] `projection/{rebuild,resolver}.rs` 在 `Redb` 仓库上重建并解析 AI 投影表（`ai_*`）。
- [ ] SQLite 路径既有记忆/权限单元测试零回归。
- [ ] 禁改文件（`db.rs`、`capture/{store,detach}.rs`、`shared_perm.rs`）零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::memory::projection::tests::redb_memory_automation_permission_and_projection_stores)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::memory::projection::tests::redb_memory_automation_permission_and_projection_stores)'`（new，覆盖 AC1、AC2、AC3、AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^internal::ai::(memory|permission)::/)'`（覆盖 AC5；AC6 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-22`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/ai/memory/derive_commit.rs`、`src/internal/ai/memory/error.rs`、`src/internal/ai/memory/projection.rs`、`src/internal/ai/memory/reader.rs`、`src/internal/ai/automation/history.rs`、`src/internal/ai/automation/runtime.rs`、`src/internal/ai/permission/approved.rs`、`src/internal/ai/permission/runtime_cache.rs`、`src/internal/ai/projection/rebuild.rs`、`src/internal/ai/projection/resolver.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 10 个文件（4 个生产目录）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire ai memory auto perm proj 10 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=4; prod-files=10; scope=M; deps=RB-22; writeset=序列化于 RB-22; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-24: 接入 AI Hooks、MCP Server 与 Observed Agents 9 文件（Track B — `REL-RB-01` 子卡 17/29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/internal/ai/hooks/intent.rs`（1 文件）、`src/internal/ai/mcp_server/{exec,stdio}.rs`（2 文件）、`src/internal/ai/observed_agents/{compliance,hermes_export,trust}.rs`（3 文件）与 `src/internal/ai/observed_agents/native_origin/{attest,registry,verify}.rs`（3 文件）共 4 目录、9 文件接入 `crate::storage`，保持第三方外部 SQLite 解析器 `codex_index.rs` 与 `builtin/hermes.rs` 原样不变。

**Out of scope:**
- `src/internal/ai/observed_agents/codex_index.rs` 与 `builtin/hermes.rs`：外部 Codex/Hermes SQLite 只读解析器，不替换（非目标）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `hooks/intent.rs:62`、`mcp_server/{exec:698,stdio:20}`、`native_origin/registry.rs:19` 与 `hermes_export.rs:238` 直接打开 `libra.db`/`capture.db` | `src/internal/ai/hooks/intent.rs:62`；`src/internal/ai/mcp_server/exec.rs:698`；`src/internal/ai/mcp_server/stdio.rs:20`；`src/internal/ai/observed_agents/hermes_export.rs:238`；`src/internal/ai/observed_agents/native_origin/registry.rs:19` |

**Acceptance criteria:**
- [ ] `hooks/intent.rs` 在 `Redb` 仓库上通过 `ConfigStore`/`GitStore` 读取意图配置，不打开 `libra.db`。
- [ ] `mcp_server/{exec,stdio}.rs` 在 `Redb` 仓库上通过 `crate::storage` 处理工具请求与状态持久化。
- [ ] `observed_agents/{compliance,hermes_export,trust}.rs` 与 `native_origin/{attest,registry,verify}.rs` 在 `Redb` 仓库上完成合规导出、信任登记与原生来源校验。
- [ ] 第三方只读解析器 `codex_index.rs` 与 `builtin/hermes.rs` 零改动。
- [ ] SQLite 路径既有 `codex_capture_local_test` 零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::mcp_server::exec::tests::redb_hooks_mcp_observed_and_native_origin_stores)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::mcp_server::exec::tests::redb_hooks_mcp_observed_and_native_origin_stores)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test codex_capture_local_test`（覆盖 AC5；AC4 由 `RB-30` diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-23`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/ai/hooks/intent.rs`、`src/internal/ai/mcp_server/exec.rs`、`src/internal/ai/mcp_server/stdio.rs`、`src/internal/ai/observed_agents/compliance.rs`、`src/internal/ai/observed_agents/hermes_export.rs`、`src/internal/ai/observed_agents/trust.rs`、`src/internal/ai/observed_agents/native_origin/attest.rs`、`src/internal/ai/observed_agents/native_origin/registry.rs`、`src/internal/ai/observed_agents/native_origin/verify.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 9 个文件（4 个生产目录）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire ai hooks mcp observed native 9 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=4; prod-files=9; scope=M; deps=RB-23; writeset=序列化于 RB-23; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-25: 接入 Agent Bridge 4 文件与 `HEAD` 原子发布围栏（Track B — `REL-RB-01` 子卡 18/29，拆出 `RB-25b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/internal/ai/agent_bridge/{ingress,methods,publication,storage}.rs`（1 目录、4 文件）接入 `CaptureStore`/`GitStore`，保留 `publication.rs` 既有 `ConnectionTrait` 签名并新增 `*_with_store` 路径将 `agent_bridge_publication` 与 `HEAD` 移动绑入同一 `git.redb` 写事务，同时支持 `agent_bridge_index_publication` 两阶段状态与长驻并发无锁读。

**Out of scope:**
- `src/internal/ai/sandbox/policy.rs` 整目录代际拒读策略：已按 `G-01`/`G-03`/`G-09` 安全轴拆出由 `RB-25b` 承接。
- 修改 `agent_bridge` 对外 JSON-RPC 协议字段或方法名。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `agent_bridge/publication.rs:5-40` 在 `HEAD` 同事务写 `agent_bridge_publication` 并维护两阶段 `agent_bridge_index_publication` | `src/internal/ai/agent_bridge/publication.rs:5-40`；`src/internal/ai/agent_bridge/storage.rs` |

**Acceptance criteria:**
- [ ] `agent_bridge/{ingress,methods,storage}.rs` 在 `Redb` 仓库上通过 `CaptureStore`/`GitStore` 完成会话、事件序列确认与操作重放，不打开 SQLite。
- [ ] `agent_bridge/publication.rs` 在 `Redb` 下经 `GitStore::update_ref_cas_with_bridge_publication` 同事务原子绑定 `HEAD` 与 `agent_bridge_publication`，回滚无孤行。
- [ ] `agent_bridge/publication.rs` 在 `Redb` 下正确执行 `agent_bridge_index_publication` 的 `prepared` → `published` 两阶段提交。
- [ ] `agent_bridge/publication.rs` 原样保留既有 `ConnectionTrait` 保签函数供 SQLite 与其它 Agent 调用。
- [ ] 并发压测：长驻 `agent_bridge` 写入期间 8 个并发只读查询零锁错。
- [ ] 既有 SQLite `agent_bridge_vcs_test`（与 `agent_bridge_crash_test`）路径零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::agent_bridge::publication::tests::redb_bridge_atomic_head_publication_and_index_two_phase)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::agent_bridge::publication::tests::redb_bridge_atomic_head_publication_and_index_two_phase)'`（new，覆盖 AC1–AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test agent_bridge_vcs_test`（覆盖 AC6）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-24`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/ai/agent_bridge/ingress.rs`、`src/internal/ai/agent_bridge/methods.rs`、`src/internal/ai/agent_bridge/publication.rs`、`src/internal/ai/agent_bridge/storage.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 4 个文件（1 个生产目录 `src/internal/ai/agent_bridge/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire agent-bridge 4 files and head publication; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=1; prod-files=4; scope=M; deps=RB-24; writeset=序列化于 RB-24; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-25b: 接入 Sandbox 整目录红库与备份代际拒读策略 1 文件（Track B — `REL-RB-01` 子卡 19/29，自 `RB-25` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区更新 `src/internal/ai/sandbox/policy.rs:669-681`（1 目录、1 文件），将 **`"redb"`、`"private"`、`"storage-format.json"`、`"sqlite-backup"`、`"redb-backup"` 与 `"git.redb"`** 整目录/固定路径无条件钉入 `task_workspace_sensitive_read_paths`，确保沙箱建立之后或迁移并发期间在 `.libra/redb/`、`.libra/private/` 内新建的所有未来代际文件（`git.gen-<id>.redb`、`capture.gen-<id>.redb`）自动落入拒读边界（`ADR-RB-02` #1）。

**Out of scope:**
- 修改沙箱其余网络/写路径规则或 `shared_perm.rs`。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `sandbox/policy.rs:669-702` 当前仅钉死 `libra.db*`/`vault.db*`/`sessions`，未钉入 `.libra/redb/`、`.libra/private/`、`storage-format.json` 与备份目录 | `src/internal/ai/sandbox/policy.rs:661-702`；`ADR-RB-02` #1 |

**Acceptance criteria:**
- [ ] `sandbox/policy.rs` `task_workspace_sensitive_read_paths` 显式钉入整目录 `"redb"`、`"private"`、`"sqlite-backup"`、`"redb-backup"` 及 `"storage-format.json"`、`"git.redb"`（即使沙箱创建时目录尚未存在）。
- [ ] 沙箱并发迁移测试：在沙箱策略生成之后于 `.libra/redb/` 新建未来代际 `git.gen-00000002.redb`，验证自动落入已钉死目录拒读范围。
- [ ] 沙箱并发迁移测试：在沙箱策略生成之后于 `.libra/private/` 新建未来代际 `capture.gen-00000002.redb`，验证自动落入已钉死目录拒读范围。
- [ ] 既有 SQLite 沙箱敏感路径拒读单元测试零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::sandbox::policy::tests::redb_generation_dir_pinned_against_future_and_concurrent_migration)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=internal::ai::sandbox::policy::tests::redb_generation_dir_pinned_against_future_and_concurrent_migration)'`（new，覆盖 AC1、AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^internal::ai::sandbox::policy::tests::/)'`（覆盖 AC4）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-25`

**Deliverables:** `N/A`

**Implementation write set:** `src/internal/ai/sandbox/policy.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 1 个文件（1 个生产目录 `src/internal/ai/sandbox/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=pin redb and backup dirs in sandbox policy 1 file; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=1; scope=S; deps=RB-25; writeset=序列化于 RB-25; release=family child; split-from=RB-25; exception=N/A`

---

### Task RB-26: 接入 Agent CLI 与 Mega2 Traces 命令组 11 文件（Track B — `REL-RB-01` 子卡 20/29，拆出 `RB-26b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/agent/{bridge,checkpoint,clean,graph,import,mega2_stream,session,skill,status,workspace}.rs`（10 文件）与 `src/command/mega2_traces/mod.rs`（1 文件）共 2 目录、11 文件接入 `crate::storage`，落实只读 Agent 子命令走 `open_read_only`、`capture.redb` `LBR-AGENT-048` 隔离 Git 读路径，以及读写 Agent 子命令经 `CaptureStore`/`GitStore` 执行。

**Out of scope:**
- `src/command/agent/doctor.rs` 只读打开与 `--repair` 显式崩溃恢复/跨库意图对账/只读文件系统拒收：已按 `G-03`/`G-09` 拆出由 `RB-26b` 承接。
- `src/command/agent/mod.rs` 新增 `daemon` 子命令与 `doctor` 队列高水位面板扩展：由 `RB-29` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `agent/bridge.rs:119`、`agent/clean.rs:1097`、`agent/session.rs:800`、`agent/workspace.rs:113,151`、`agent/mega2_stream.rs:18` 直接调 `open_cached_repository(&path::database())` | `src/command/agent/bridge.rs:119`；`src/command/agent/workspace.rs:113`；`src/command/agent/mega2_stream.rs:18` |

**Acceptance criteria:**
- [ ] `agent session/status/checkpoint/graph/skill/workspace` 在 `Redb` 仓库严格经 `open_read_only` 执行，遇 `RepairAborted` fail-closed 不升写。
- [ ] `agent bridge/clean/import/mega2_stream` 与 `mega2_traces` 在 `Redb` 仓库经 `CaptureStore`/`GitStore` 读写，不打开 `libra.db` 或 `capture.db`。
- [ ] `capture.redb` 存在过新 `META_SCHEMA` 时，仅相关 Agent 命令报 `LBR-AGENT-048` 而仓库 Git 读路径保持正常。
- [ ] 既有 SQLite `agent_bridge_workspace_test` 零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::agent::workspace::tests::redb_agent_cli_readonly_rw_and_mega2_traces)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::agent::workspace::tests::redb_agent_cli_readonly_rw_and_mega2_traces)'`（new，覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test agent_bridge_workspace_test`（覆盖 AC4）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-25b`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/agent/bridge.rs`、`src/command/agent/checkpoint.rs`、`src/command/agent/clean.rs`、`src/command/agent/graph.rs`、`src/command/agent/import.rs`、`src/command/agent/mega2_stream.rs`、`src/command/agent/session.rs`、`src/command/agent/skill.rs`、`src/command/agent/status.rs`、`src/command/agent/workspace.rs`、`src/command/mega2_traces/mod.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 11 个文件（2 个生产目录：`src/command/agent/`、`src/command/mega2_traces/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire agent cli and mega2-traces 11 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=11; scope=M; deps=RB-25b; writeset=序列化于 RB-25b; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-26b: 接入 `agent doctor` 只读诊断与 `--repair` 显式崩溃恢复/意图对账 1 文件（Track B — `REL-RB-01` 子卡 21/29，自 `RB-26` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/agent/doctor.rs`（1 目录、1 文件）接入 `crate::storage`，落实不带 `--repair` 的 `agent doctor` 严格经 `open_read_only` 执行且遇 `RepairAborted` fail-closed 不升写，而 `agent doctor --repair` 显式允许触发 `redb` 崩溃恢复与 `cross_store::reconcile_pending_intents()`（只读挂载文件系统 fail-closed 报 `LBR-IO-001`）。

**Out of scope:**
- `agent doctor` 新增队列高水位与 `quarantine/` 指标面板：由 `RB-29` 承接。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `agent/doctor.rs:5026,5093` 直接打开 SQLite 库 | `src/command/agent/doctor.rs:5026,5093`；`ADR-RB-03`、`ADR-RB-06` |

**Acceptance criteria:**
- [ ] 不带 `--repair` 的 `agent doctor` 在 `Redb` 仓库严格经 `open_read_only` 执行，遇 `RepairAborted` fail-closed 不升写。
- [ ] `agent doctor --repair` 在可写红库上显式完成 `redb` 崩溃恢复。
- [ ] `agent doctor --repair` 在可写红库上显式执行 `cross_store::reconcile_pending_intents()` 跨库意图对账。
- [ ] `agent doctor --repair` 在只读挂载文件系统上 fail-closed 报 `LBR-IO-001`。
- [ ] 禁改文件（`db.rs`、`capture/{store,detach}.rs`、`shared_perm.rs`）零改动。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::agent::doctor::tests::redb_agent_doctor_readonly_vs_explicit_repair)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::agent::doctor::tests::redb_agent_doctor_readonly_vs_explicit_repair)'`（new，覆盖 AC1–AC4；AC5 由 `RB-30` 禁改 diff 门终审）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-26`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/agent/doctor.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 1 个文件（1 个生产目录 `src/command/agent/`）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire agent doctor readonly vs explicit repair 1 file; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=1; scope=S; deps=RB-26; writeset=序列化于 RB-26; release=family child; split-from=RB-26; exception=N/A`

---

### Task RB-27: 接入桌面 GUI 无迁移只读仓库探测与状态映射 2 文件（Track B — `REL-RB-01` 子卡 22/29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `d242b90` 新增的 `src/gui/data/repo.rs` 与 `src/gui/data/state.rs`（1 目录、2 文件）接入 `crate::storage`，使桌面窗口根据仓库后端只读探测 `storage-format.json`/`.libra/redb/` 或 `libra.db`、经 `open_read_only`（`16 MiB` 缓存上限）读取 `RepoContext` 且永不触发 schema 迁移或自动升写修复。

**Out of scope:**
- 修改 `src/gui/data/{mod,snapshot,worker}.rs` 或任何桌面 UI 视觉组件。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `src/gui/data/repo.rs:47-88` 硬编码探测 `scope.storage.join(util::DATABASE)` 并经 `open_connection_without_schema_management` 检验 SQLite schema | `src/gui/data/repo.rs:45-88`；`src/gui/data/state.rs:14-100` |

**Acceptance criteria:**
- [ ] `gui::data::repo::read_context` 在 `Redb` 仓库（无 `libra.db`）上经 `open_read_only` 成功返回 `RepoContext`（HEAD、分支、hash kind），不修改磁盘文件。
- [ ] `gui::data::state` 将 `redb` `META_SCHEMA` 过新精确映射至 `SourceState::SchemaNew`。
- [ ] `gui::data::state` 将 `redb` `META_SCHEMA` 过旧精确映射至 `SourceState::SchemaOld`。
- [ ] `gui::data::repo` 遇 `DatabaseError::RepairAborted` 严格不升写，映射为 `SourceState::Corrupt`（附 `libra fsck` 修复提示）。
- [ ] `gui::data::repo` 在只读权限受限目录保留原始文件系统 `ErrorKind` 映射为 `SourceState::Unreadable`。
- [ ] SQLite 仓库下既有 `src/gui/data/tests.rs` 全部用例零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=gui::data::tests::redb_window_readonly_context_and_source_state_mapping)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=gui::data::tests::redb_window_readonly_context_and_source_state_mapping)'`（new，覆盖 AC1–AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^gui::data::tests::/)'`（覆盖 AC6）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-26b`

**Deliverables:** `N/A`

**Implementation write set:** `src/gui/data/repo.rs`、`src/gui/data/state.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 2 个文件（1 个生产目录 `src/gui/data/`）。

**Docs and compatibility impact:** `N/A`（保持既有 `SourceState` 状态契约）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire gui non-migrating repo reader and state; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=1; prod-files=2; scope=S; deps=RB-26b; writeset=序列化于 RB-26b; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-28: 接入 Cloud Sync / Snapshot / Metadata 5 文件与跨库意图一致快照（Track B — `REL-RB-01` 子卡 23/29，拆出 `RB-28b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/cloud.rs` 与 `src/command/cloud/{agent_capture,metadata,snapshot,sync}.rs`（2 目录、5 文件）接入 `crate::storage`，落实 `ADR-RB-06` 跨库意图对账与三步代际一致快照协议（`git_rtx(g1) → capture_rtx(c1) → git_rtx2(g2)`，仅接受 `g1 == c1 && g1 == g2`，最多重试 5 次否则 fail-closed 报 `LBR-IO-001` 指引 `agent doctor --repair`），并保持远端 D1/R2 协议字节不变。

**Out of scope:**
- `src/command/cloud/restore.rs` 红库目标恢复（同步更新 `storage-format.json` 与 `META_GENERATION`）及并发一致快照还原校验：已按 `G-03`/`G-09` 拆出由 `RB-28b` 承接。
- 修改 `src/utils/d1_client.rs` 或远端 Cloudflare D1/R2 schema。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `command/cloud/{sync,snapshot,agent_capture,metadata}.rs` 直接操作 SQLite `libra.db` 与 `capture.db` | `src/command/cloud/sync.rs`；`src/command/cloud/snapshot.rs`；`ADR-RB-06` |

**Acceptance criteria:**
- [ ] `cloud sync` 与 `cloud snapshot` 在 `Redb` 仓库上按 `ADR-RB-06` 校验 `g1 == c1 && g1 == g2` 后才导出快照，绝不导出跨代撕裂快照。
- [ ] 并发写入导致代际反复变化超过 5 次重试时，`cloud snapshot` fail-closed 返回 `LBR-IO-001`（不上传不一致备份）。
- [ ] 故障注入（Git 已提交、Capture 尚未提交的半提交态）：`cloud snapshot` fail-closed 引导运行 `agent doctor --repair`，修复重放 `META_CROSS_STORE_INTENT` 后重跑快照成功。
- [ ] `cloud agent_capture` 与 `cloud metadata` 在 `Redb` 仓库上经 `CaptureStore`/`GitStore` 读写并保持 D1/R2 线协议不变。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::cloud::snapshot::tests::redb_cross_store_gen_consistent_snapshot_and_wire_protocol) | test(=command::cloud::snapshot::tests::redb_partial_commit_fails_closed_then_recovers_after_doctor_repair)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::cloud::snapshot::tests::redb_cross_store_gen_consistent_snapshot_and_wire_protocol)'`（new，覆盖 AC1、AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::cloud::snapshot::tests::redb_partial_commit_fails_closed_then_recovers_after_doctor_repair)'`（new，覆盖 AC2、AC3）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-27`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/cloud.rs`、`src/command/cloud/agent_capture.rs`、`src/command/cloud/metadata.rs`、`src/command/cloud/snapshot.rs`、`src/command/cloud/sync.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 5 个文件（2 个生产目录：`src/command/`、`src/command/cloud/`）。

**Docs and compatibility impact:** `N/A`（不改外部云同步 CLI 与 D1/R2 协议）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire cloud sync snapshot metadata 5 files; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=5; scope=M; deps=RB-27; writeset=序列化于 RB-27; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-28b: 接入 `cloud restore` 红库目标恢复与代际清单更新 1 文件（Track B — `REL-RB-01` 子卡 24/29，自 `RB-28` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区将 `src/command/cloud/restore.rs`（1 目录、1 文件）接入 `crate::storage`，使并发检查点写入期间冻结的一致快照还原到空红库后精确匹配同一代际，并正确更新 `.libra/storage-format.json` 与 `META_GENERATION`，同时保持 SQLite `cloud_storage_backup_test` 零回归。

**Out of scope:**
- 修改远端 Cloudflare D1/R2 协议。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `src/command/cloud/restore.rs:626` 直接还原 SQLite `libra.db` 与 `capture.db` | `src/command/cloud/restore.rs:626`；`ADR-RB-06` |

**Acceptance criteria:**
- [ ] 并发检查点写入期间成功冻结的一致快照经 `cloud restore` 还原到空仓库后，Git ref 与捕获目录精确匹配同一代际。
- [ ] `cloud restore` 在 `Redb` 目标仓库恢复完成后正确更新 `.libra/storage-format.json`。
- [ ] `cloud restore` 在 `Redb` 目标仓库恢复完成后正确更新两库 `META_GENERATION`。
- [ ] 既有 SQLite `cloud_storage_backup_test` 路径零回归。

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::cloud::restore::tests::redb_concurrent_snapshot_restore_manifest_and_gen_parity)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=command::cloud::restore::tests::redb_concurrent_snapshot_restore_manifest_and_gen_parity)'`（new，覆盖 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test cloud_storage_backup_test`（覆盖 AC4）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-28`

**Deliverables:** `N/A`

**Implementation write set:** `src/command/cloud/restore.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 1 个文件（1 个生产目录 `src/command/cloud/`）。

**Docs and compatibility impact:** `N/A`（不改外部云同步 CLI 与 D1/R2 协议）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire cloud restore redb manifest and gen parity 1 file; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=1; scope=S; deps=RB-28; writeset=序列化于 RB-28; release=family child; split-from=RB-28; exception=N/A`

---

### Task RB-29: 接入后端感知 Hook 队列、纯 `storage-redb` 队列 E2E 与 `__capture-queue-drain`（Track B — `REL-RB-01` 子卡 25/29，拆出 `RB-29d`、`RB-29a`、`RB-29b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区于 `live_pipeline.rs`/`drain.rs`/`spool.rs`/`src/cli.rs` 按 `ADR-RB-01` #4 落实运行时队列分派（因 `storage-redb` 自动启用 `storage-queue`，任何含 `storage-redb` 的构建含纯 `--no-default-features --features storage-redb` 在选定仓库后端 == `Redb` 时均恒走双通道 `capture-queue/{priority,normal}/`；SQLite 仓库即便在 `--all-features` 二进制下仍严格走 `capture-spool/`），并新增内部排空命令 `libra __capture-queue-drain`。

**Out of scope:**
- 可选预览守护进程 `libra agent daemon`（防重复派生）、`agent doctor` 双通道高水位/`quarantine/` 指标及随附 `docs/commands/agent.md` 与 `docs/error-codes.md`（`LBR-AGENT-050`）同步：已按 `G-03`/`G-09` 拆出由 `RB-29d` 承接。
- 翻转 `REDB_RUNTIME_UNLOCKED = true`（`RB-29a`）与切换解锁态测试/`base.yml`（`RB-29c`）。
- 修改 `src/internal/ai/capture/{store,detach}.rs`：严禁修改（`GC-RB-06`）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `ADR-RB-01` #1 规定 `storage-redb` 自动蕴含 `storage-queue`，SQLite 仓与 Redb 仓须按选定后端严格分流 `capture-spool/` vs `capture-queue/` | `src/internal/ai/capture/drain.rs:15`；`ADR-RB-01`、`ADR-RB-05` |

**Acceptance criteria:**
- [ ] 在 `--all-features` 二进制中，选定后端为 `Sqlite` 的仓库执行实时 Hook 仍写入 `capture-spool/ready/`，绝不写入 `capture-queue/`。
- [ ] 在纯 `--no-default-features --features storage-redb` 构建（不显式传 `storage-queue`）下，红库仓库实时 Hook 仅写入 `capture-queue/{priority,normal}/`（绝不直接打开 `capture.redb`）。
- [ ] 在上述纯 `--no-default-features --features storage-redb` 构建下，`libra __capture-queue-drain` 将队列帧无损落库 `capture.redb`。
- [ ] `SessionEnd` 钩子路由至 `priority/` 通道（普通事件路由至 `normal/`）。
- [ ] `libra __capture-queue-drain` 严格先将 `priority/` 落库再消费 `normal/`。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=internal::ai::capture::live_pipeline::tests::backend_aware_hook_dispatch_sqlite_vs_redb) | test(=internal::ai::capture::live_pipeline::tests::redb_only_feature_implies_queue_hook_to_capture_e2e)'`（new，lib 发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=internal::ai::capture::live_pipeline::tests::backend_aware_hook_dispatch_sqlite_vs_redb)'`（new，覆盖 AC1、AC4、AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=internal::ai::capture::live_pipeline::tests::redb_only_feature_implies_queue_hook_to_capture_e2e)'`（new，覆盖 AC2、AC3）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-20b`、`RB-28b`

**Deliverables:** `N/A`

**Implementation write set:** `src/cli.rs`、`src/internal/ai/capture/live_pipeline.rs`、`src/internal/ai/capture/drain.rs`、`src/internal/ai/capture/spool.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 4 个生产文件（2 个生产目录：`src/`、`src/internal/ai/capture/`）。

**Docs and compatibility impact:** `N/A`（用户面 `agent daemon`、`doctor` 与 `LBR-AGENT-050` 文档同步随 `RB-29d` 交付）。

**Rollback mode:** `forward-only`

**Migration and rollback:** 回退前运行 `libra __capture-queue-drain` 排空 `.libra/private/capture-queue/{priority,normal}/` 中待处理帧至 `capture.redb`，确保零事件遗失；SQLite 仓库始终未写入 `capture-queue/` 不受影响。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire backend-aware hook queue and redb-only drain e2e; recovery=forward-only drain or abandon; complete=yes; self-contained=yes; AC=5/8; VER=3/8; landing=2; prod-files=4; scope=M; deps=RB-20b,RB-28b; writeset=序列化于 RB-20b,RB-28b; release=family child; split-from=N/A; exception=N/A`

---

### Task RB-29d: 接入单写入者 `agent daemon`、`agent doctor` 队列指标与 `LBR-AGENT-050` 文档同步（Track B — `REL-RB-01` 子卡 26/29，自 `RB-29` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区于 `src/cli.rs`、`src/command/agent/mod.rs`、`src/command/agent/doctor.rs` 新增可选预览守护进程 `libra agent daemon`（活跃时 Hook 跳过 `spawn`、无守护时仅首帧触发单次分离式 `__capture-queue-drain`）与 `agent doctor` 双通道高水位/积压字节/`quarantine/` 诊断面板，并按 `ER-06a` 随附更新 `docs/commands/agent.md`、`docs/commands/zh-CN/agent.md` 与 `docs/error-codes.md`（登记 `LBR-AGENT-050`）。

**Out of scope:**
- 翻转 `REDB_RUNTIME_UNLOCKED = true`（`RB-29a`）与切换解锁态测试/`base.yml`（`RB-29c`）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `docs/error-codes.md:166-169` 已登记 `LBR-AGENT-046..049`，`LBR-AGENT-050` 空闲可用；`command/agent/mod.rs` 尚无 `daemon` 子命令 | `docs/error-codes.md:166-169`；`src/command/agent/mod.rs`；`ADR-RB-05` |

**Acceptance criteria:**
- [ ] 活跃 `libra agent daemon` 存在时，实时 Hook 跳过 `spawn` 后台排空进程。
- [ ] 无活跃 `libra agent daemon` 时，仅由首帧触发单次分离式 `__capture-queue-drain`。
- [ ] `libra agent doctor` 输出 `priority/` 与 `normal/` 各自高水位与积压字节数。
- [ ] `libra agent doctor` 输出 `quarantine/` 计数与诊断码 `LBR-AGENT-050`。（注：`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md` 与 `docs/error-codes.md` 同步属 `ER-06a` 同卡必需文档门，按 `G-03` 不占本卡特有 AC 配额。）

**Verification:**
- [ ] `source .env.test && cargo nextest list --no-default-features --features storage-redb --lib -E 'test(=command::agent::doctor::tests::redb_daemon_drain_priority_and_doctor_queue_telemetry)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb --lib -E 'test(=command::agent::doctor::tests::redb_daemon_drain_priority_and_doctor_queue_telemetry)'`（new，覆盖 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_agent_docs_contract`（`ER-06a` 随附文档契约）
- [ ] `rg -q "LBR-AGENT-050" docs/error-codes.md`（`ER-06a` 错误码登记核验）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-29`

**Deliverables:** `N/A`

**Implementation write set:** `src/cli.rs`、`src/command/agent/mod.rs`、`src/command/agent/doctor.rs`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/error-codes.md`

**Release write set:** `N/A`

**Files likely touched:** 上列 3 个生产文件（2 个生产目录：`src/`、`src/command/agent/`）+ 3 个 `ER-06a` 随附文档文件（不计入 `G-04` 生产计数）。

**Docs and compatibility impact:** 随附更新 `docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`（记录预览 `libra agent daemon` 与 `doctor` 队列指标）与 `docs/error-codes.md`（新增 `LBR-AGENT-050` 及 `LBR-AGENT-046..049` 红库队列说明）；网站 `agent.en.md` 同步在唯一发布点 `RB-30` 随发版交付。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=wire agent daemon spawn-dedup and doctor queue metrics; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=3; scope=S; deps=RB-29; writeset=序列化于 RB-29; release=family child; split-from=RB-29; exception=N/A`

---

### Task RB-29a: 解锁 CLI 与 `libra-migrate` 编译期运行时门控（Track B — `REL-RB-01` 子卡 27/29，自 `RB-29` 拆出，拆出 `RB-29c`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区（全部 146 文件接线与队列接入已就绪后）将 `src/storage/backend.rs` 编译期常量 `REDB_RUNTIME_UNLOCKED` 从 `false` 翻为 `true`（同步更新 `src/storage/backend.rs` 门控单测并新增解锁态与未迁移 SQLite 拒绝测试），联合解锁 CLI `LIBRA_STORAGE_BACKEND=redb` 路径与 `libra-migrate --to redb` 实盘切换入口。

**Out of scope:**
- 将 `tests/storage_concurrency_stress_test.rs` 切换为解锁态双向往返用例并同步更新 `.github/workflows/base.yml` 步骤 ⑥ 与 `compat-offline-core` 零修改比对：已按 `G-03`/`G-09` 拆出由 `RB-29c` 承接。
- 升级 `base.yml` 跑全量 redb CLI 套件与 `keyring`（归 `RB-31`/`RB-31b`）或修改 `Cargo.toml` 默认 Feature（归 `REL-RB-02`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RB-01c` 与 `RB-11t` 将 `REDB_RUNTIME_UNLOCKED = false` 设为唯一编译期门控点；全部 146 文件与队列接线完成后于同家族内解锁 | `ADR-RB-01`、`ADR-RB-08`；`RB-01c`、`RB-11t` |

**Acceptance criteria:**
- [ ] `src/storage/backend.rs` 将 `REDB_RUNTIME_UNLOCKED` 翻为 `true` 并更新编译期门控单元测试断言。
- [ ] 解锁后已编译 CLI 在显式 `LIBRA_STORAGE_BACKEND=redb` 下正常选用 `StorageBackendKind::Redb`。
- [ ] 解锁后 `libra-migrate --to redb` 实盘切换入口解除 `LBR-CLI-003` 预集成阻断且 `--bin libra-migrate` 单测全绿。
- [ ] 未迁移旧 SQLite 仓库在纯 `storage-redb` 二进制上 fail-closed 输出 `libra-migrate` 升级指引。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --lib -E 'test(=storage::backend::tests::unlocked_cli_and_migrate_cutover_with_unmigrated_sqlite_fail_closed)'`（new，lib 发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --lib -E 'test(=storage::backend::tests::unlocked_cli_and_migrate_cutover_with_unmigrated_sqlite_fail_closed)'`（new，覆盖 AC1、AC2、AC4）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`（覆盖 AC3）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`；随 `RB-30` 承接 `T-2` 与远端 `base.yml` CI）

**Dependencies:** `RB-29d`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/backend.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 1 个生产文件（1 个生产目录 `src/storage/`）。

**Docs and compatibility impact:** `N/A`（默认 Feature 仍为 `storage-sqlite`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=unlock REDB_RUNTIME_UNLOCKED cli and libra-migrate gate; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=1; scope=S; deps=RB-29d; writeset=序列化于 RB-29d; release=family child; split-from=RB-29; exception=N/A`

---

### Task RB-29c: 切换解锁态 `libra-migrate` 往返测试并原子更新 `base.yml` 解锁态 CI 选测（Track B — `REL-RB-01` 子卡 28/29，自 `RB-29a` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在隔离集成工作区紧接 `RB-29a` 解锁之后，将 `tests/storage_concurrency_stress_test.rs` 中的预解锁二进制用例 `pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env` 替换为真实解锁态双向往返用例 `post_unlock_libra_migrate_binary_bidirectional_roundtrip`，并同步原子更新 `.github/workflows/base.yml` 的 `compat-redb-storage` 步骤 ⑥ 选测过滤器（精确作用域 `diff -u` 证明 `compat-offline-core` 零修改），确保 `RB-30` 推送集成树触发远端 `base.yml` CI 时精确执行解锁态二进制门并全绿。

**Out of scope:**
- 升级 `base.yml` 跑全量 redb CLI 套件与 `keyring`：归 `RB-31`、`RB-31b`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RB-12` 在 `base.yml` 步骤 ⑥ 显式选择 `pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env`；解锁后必须在同家族内同步更新 `base.yml`，否则 `RB-30` 推送门禁会找不到旧测试名而失败 | `RB-12`；`RB-29a`；`ADR-RB-08` |

**Acceptance criteria:**
- [ ] `tests/storage_concurrency_stress_test.rs` 将锁定态二进制用例替换为 `post_unlock_libra_migrate_binary_bidirectional_roundtrip`，拉起真实 `env!("CARGO_BIN_EXE_libra-migrate")` 完成 `SQLite → redb → 新写入 → SQLite` 端到端双向往返且 100% 一致。
- [ ] `.github/workflows/base.yml` 的 `compat-redb-storage` 步骤 ⑥ 已原子更新为 `test(=dual_feature_migration_and_stress_roundtrip) | test(=post_unlock_libra_migrate_binary_bidirectional_roundtrip)`。
- [ ] `.github/workflows/base.yml` 零残留 `pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env`。
- [ ] 既有 `compat-offline-core` job（`jobs.test`）步骤相对前置基线逐行零修改。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=dual_feature_migration_and_stress_roundtrip) | test(=post_unlock_libra_migrate_binary_bidirectional_roundtrip)'`（new，integration 发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=dual_feature_migration_and_stress_roundtrip) | test(=post_unlock_libra_migrate_binary_bidirectional_roundtrip)'`（new，覆盖 AC1）
- [ ] `rg -q "post_unlock_libra_migrate_binary_bidirectional_roundtrip" .github/workflows/base.yml`（覆盖 AC2）
- [ ] `bash -euo pipefail -c 'if rg -n "pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env" .github/workflows/base.yml; then echo "FAIL: stale pre-unlock test"; exit 1; else rc=$?; if [ "$rc" -ne 1 ]; then exit "$rc"; fi; fi'`（遵循 `plan-template.md:591` 区分退出码 `1` 与 `>1`，覆盖 AC3）
- [ ] `python3 -c 'import os, re, subprocess; from pathlib import Path; run=lambda c: subprocess.check_output(c, text=True); extract=lambda t: (lambda ls: "".join(ls))([l for l in (lambda a: a[next(i for i,x in enumerate(a) if x.startswith("  test:")):])(t.splitlines(True))][:next((j for j,x in enumerate((lambda a: a[next(i for i,x in enumerate(a) if x.startswith("  test:")):])(t.splitlines(True))[1:],1) if re.match(r"^  [^ ]",x)), None)]); st=run(["libra","status","--porcelain=v1","--",".github/workflows/base.yml"]).strip(); base_ref=os.environ.get("PRE_CARD_BASE") or ("HEAD" if st else "HEAD~1"); old=extract(run(["libra","--no-pager","show",f"{base_ref}:.github/workflows/base.yml"])); wt=extract(Path(".github/workflows/base.yml").read_text(encoding="utf-8")); assert "name: compat-offline-core" in old and old==wt, "compat-offline-core modified"'`（零临时文件纯内存精确作用域逐字节比对，覆盖 AC4）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`；随 `RB-30` 承接 `T-2` 与远端 `base.yml` CI）

**Dependencies:** `RB-29a`

**Deliverables:** `N/A`

**Implementation write set:** `.github/workflows/base.yml`、`tests/storage_concurrency_stress_test.rs`

**Release write set:** `N/A`

**Files likely touched:** 上列 1 个 CI 文件（1 个生产目录 `.github/workflows/`）+ 1 个集成测试文件（严格串行于 `RB-12` 与 `RB-31`）。

**Docs and compatibility impact:** `N/A`（默认 Feature 仍为 `storage-sqlite`）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=implementation; axis=switch base.yml unlocked migrate test and job-diff; recovery=abandon branch or revert commit; complete=yes; self-contained=yes; AC=4/8; VER=5/8; landing=1; prod-files=1; scope=S; deps=RB-29a; writeset=序列化于 RB-29a; release=family child; split-from=RB-29a; exception=N/A`

---

### Task RB-29b: `syn 2.0` 语法感知逐调用点与调用方可达性审计守卫（Track B — `REL-RB-01` 子卡 29/29，自 `RB-29` 拆出）

**Task type:** `audit`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在不改动任何生产代码的前提下，利用仓库既有 dev 依赖 `syn = { version = "2.0", features = ["full", "visit", "visit-mut"] }`（`Cargo.toml:187`），于 `tests/storage_concurrency_stress_test.rs` 内实现「执行检查必备需求」第 5 条规定的**语法感知、逐调用点与全树调用方可达性 fail-closed AST 审计守卫**（无 `#[cfg(feature = "storage-sqlite")]` 单独豁免，强制校验 `StorageBackendKind::Sqlite`/`Redb` 运行时分发与 `*_with_conn` 全树调用方可达性）及全部 4 组负向自测。

**Out of scope:**
- 修改任何 `src/**` 生产源码（`audit` 卡只读核查并交付测试守卫）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `Cargo.toml:187` 已声明 `[dev-dependencies] syn = { version = "2.0", features = ["full", "visit", "visit-mut"] }` | `Cargo.toml:187`；`tests/compat/all_production_unwrap_guard.rs:1-101` |

**Acceptance criteria:**
- [ ] AST 守卫使用 `syn::parse_file` + `syn::visit::Visit` 遍历 `src/` 下全部非测试 `.rs` 文件，仅跳过 AST 上携带精确 `#[cfg(test)]` 属性的项（不受字符串、原始字符串、字符字面量或注释干扰）。
- [ ] 直接 SQLite/SeaORM 打开与查询调用点（除 `src/storage/**`、`src/internal/db*`、`src/internal/model/*` 与 6 个明文豁免文件外）无 `#[cfg(feature = "storage-sqlite")]` 单独豁免，必须词法嵌套于含配对 `StorageBackendKind::Redb` 分支的 `StorageBackendKind::Sqlite` 运行时分发分支内。
- [ ] 全仓 `src/` AST 核查每一已登记 `*_with_conn` 保签函数体绝不自行打开数据库。
- [ ] 全仓 `src/` AST 核查每一已登记 `*_with_conn` 保签函数在全树内的每一处调用方均为已登记 `*_with_conn` 传递函数或词法嵌套于含配对 `StorageBackendKind::Redb` 的 `StorageBackendKind::Sqlite` 分发分支内。
- [ ] 负向自测 `syn_guard_rejects_partially_wired_caller` 注入含 `GitStore` 导入但仍留 1 处未受保护 SQLite 调用的合成文件，断言拒收并报告精确函数名。
- [ ] 负向自测 `syn_guard_rejects_cfg_sqlite_without_runtime_dispatch` 注入仅用 `#[cfg(feature = "storage-sqlite")]` 包裹但缺少 `StorageBackendKind` 运行时分发的调用，断言拒收。
- [ ] 负向自测 `syn_guard_rejects_unwired_caller_of_allowlisted_with_conn` 注入模块内存在配对 `foo_with_store` 但某命令入口直接调用 `foo_with_conn` 未经 `StorageBackendKind` 分发的夹具，断言拒收。
- [ ] 负向自测 `syn_guard_fails_closed_on_malformed_rust_input` 注入语法错误与未闭合 `#[cfg(test)] mod tests {` 断言 fail-closed 解析报错。
- [ ] 实仓全树审计 `syn_ast_per_callsite_storage_wiring_completeness_guard` 通过（0 处可达未受保护 SQLite 调用点）。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=syn_ast_per_callsite_storage_wiring_completeness_guard) | test(=syn_guard_rejects_partially_wired_caller) | test(=syn_guard_rejects_cfg_sqlite_without_runtime_dispatch) | test(=syn_guard_rejects_unwired_caller_of_allowlisted_with_conn) | test(=syn_guard_fails_closed_on_malformed_rust_input)'`（new，发现门）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=syn_guard_rejects_partially_wired_caller)'`（new，覆盖 AC1、AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=syn_guard_rejects_cfg_sqlite_without_runtime_dispatch)'`（new，覆盖 AC2、AC6）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=syn_guard_rejects_unwired_caller_of_allowlisted_with_conn)'`（new，覆盖 AC3、AC4、AC7）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=syn_guard_fails_closed_on_malformed_rust_input)'`（new，覆盖 AC8）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=syn_ast_per_callsite_storage_wiring_completeness_guard)'`（new，覆盖 AC9）

**Full-suite trigger:** `N/A`（`family child of REL-RB-01`）

**Dependencies:** `RB-29c`

**Deliverables:** `tests/storage_concurrency_stress_test.rs`（`syn 2.0` 逐调用点与调用方可达性 AST 守卫模块及 4 组负向自测）

**Implementation write set:** `tests/storage_concurrency_stress_test.rs`

**Release write set:** `N/A`

**Files likely touched:** 1 个测试文件（生产代码 0 落点 / 0 文件；1 个审计产物文件 ≤ 5 满足 `G-11` `audit` `S` 上限）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `revert`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-01`

**C/D coverage from:** `RB-30`

**Granularity:** `type=audit; axis=syn 2.0 per-call-site reachability audit guard; recovery=revert audit test module; complete=yes; self-contained=yes; AC=9/20; VER=6/20; landing=0; prod-files=0; scope=S; deps=RB-29c; writeset=序列化于 RB-29c; release=family child; split-from=RB-29; exception=N/A`

---

### Task RB-30: `REL-RB-01` 家族唯一原子发布点（Rebase 汇流、`syn 2.0` 可达性守卫、全量双后端验收与 `agent.en.md` 网站同步）

**Task type:** `release`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 作为 `G-08` 家族 `REL-RB-01`（`RB-13`..`RB-29b`，共 29 张子卡）的唯一发布点，本卡不引入任何新行为代码：协调 `DEP-RB-01` 短汇流窗将 `plan/20261121-redb-integration` rebase 至最新 `origin/main`，在汇流后头提交上执行 `RB-29b` 的 `syn 2.0` 逐调用点与调用方可达性 AST 守卫及全部 4 组负向自测、禁改 4 文件与 `init.rs:1-20` 逐字节对比、解锁态真实 `libra-migrate` 双向往返测试、`base.yml` 解锁态选测校验与 `T-2` 全量双后端测试，完成 `patch + 1` 发布（默认仍为 `storage-sqlite`），并受 `DEP-RB-03` 授权门控按 `AGENTS.md:135` 唯一规范路径 `/Volumes/Sea/libra-backend`（`cf` 分支，保留无关改动；若该路径未就绪且未获用户新明确授权更新 `DEP-RB-03` 与 `AGENTS.md:135` 则 STOP）完成 `agent.en.md` 同步与部署核验。

**Out of scope:**
- 修改 `.github/workflows/base.yml` 升级全量 CI 矩阵（归 `RB-31`）、`release.yml` 双二进制打包（归 `RB-32`）或默认存储切换（归 `REL-RB-03`/`REL-RB-02`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RB-13`..`RB-29b` 覆盖基线 `d242b90` 全仓 146 个生产接线文件与 `syn 2.0` 逐调用点可达性守卫并在隔离分支本地积累；外部网站交付严格受 `AGENTS.md:135`（`/Volumes/Sea/libra-backend`，`cf` 分支）与 `DEP-RB-03` 授权门控约束 | 2026-11-21 146 文件扫描清单；`Cargo.toml:187`；`AGENTS.md:135`；`DEP-RB-01`..`03` |

**Acceptance criteria:**
- [ ] `plan/20261121-redb-integration` 已基于最新 `origin/main` 完成无冲突 rebase。
- [ ] 在最终汇流头提交上 `syn 2.0` 全树逐调用点可达性 AST 守卫与全部 4 组负向自测均通过（0 处可达未受保护 SQLite 调用点）。
- [ ] 禁改 4 文件（`src/internal/db.rs`、`src/internal/ai/capture/{store,detach}.rs`、`src/utils/shared_perm.rs`）与 `origin/main` 逐字节一致。
- [ ] `src/command/init.rs:1-20` 受保护头部与导入块与 `origin/main` 逐字节一致。
- [ ] `Cargo.toml` 默认特性仍保持 `["storage-sqlite"]`。
- [ ] 解锁态真实 `libra-migrate` 二进制端到端双向迁移演练通过（`SQLite → redb → 新写入 → SQLite` 读回 100% 一致）。
- [ ] 当前集成头提交上的 `.github/workflows/base.yml` 已原子切换至 `post_unlock_libra_migrate_binary_bidirectional_roundtrip`（零残留 `pre_unlock` 旧测试名）。
- [ ] 满足 `DEP-RB-03` 授权硬门后，按「执行检查必备需求」第 4 条完成 `agent.en.md` 网站发布流水线验收并线上可访问。（注：`ER-13` `T-2` 全量双后端测试及 `ER-08` 版本发布与远端 CI 绿灯属统一发布基线，按 `G-03` 常规履约不重复占用本卡特有 AC 配额。）

**Verification:**
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=syn_ast_per_callsite_storage_wiring_completeness_guard)'`（覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=syn_guard_rejects_partially_wired_caller) | test(=syn_guard_rejects_cfg_sqlite_without_runtime_dispatch) | test(=syn_guard_rejects_unwired_caller_of_allowlisted_with_conn) | test(=syn_guard_fails_closed_on_malformed_rust_input)'`（覆盖 AC2 负向自测）
- [ ] `bash -euo pipefail -c 'test -z "$(libra diff origin/main -- src/internal/db.rs src/internal/ai/capture/store.rs src/internal/ai/capture/detach.rs src/utils/shared_perm.rs)"'`（覆盖 AC3）
- [ ] `python3 -c 'import subprocess; from pathlib import Path; old=subprocess.check_output(["libra","--no-pager","show","origin/main:src/command/init.rs"], text=True).splitlines(True)[:20]; cur=Path("src/command/init.rs").read_text(encoding="utf-8").splitlines(True)[:20]; assert len(old)==20 and old==cur, "init.rs:1-20 modified"'`（零临时文件纯内存比对，覆盖 AC4）
- [ ] `rg -q '^default = \["storage-sqlite"\]' Cargo.toml`（覆盖 AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=post_unlock_libra_migrate_binary_bidirectional_roundtrip)'`（覆盖 AC6）
- [ ] `rg -q "post_unlock_libra_migrate_binary_bidirectional_roundtrip" .github/workflows/base.yml`（覆盖 AC7 新选测存在）
- [ ] `bash -euo pipefail -c 'if rg -n "pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env" .github/workflows/base.yml; then echo "FAIL: stale pre-unlock test"; exit 1; else rc=$?; if [ "$rc" -ne 1 ]; then exit "$rc"; fi; fi'`（覆盖 AC7 零残留）
- [ ] 先核验 `DEP-RB-03` 授权门控（默认校验 `/Volumes/Sea/libra-backend` 存在且在 `cf` 分支；若缺席且无已落盘用户新授权则立即 STOP），再按「执行检查必备需求」第 4 条完成 `agent.en.md` 网站同步、构建、独立 Claude `VERDICT: PASS`、签名推送 `cf` 与 `curl https://libra.tools/en/docs/commands/agent` 线上核验（覆盖 AC8；并按 `ER-13`/`ER-08` 跑通全量双后端 `T-2` 并登记 `base.yml`/`codeql.yml`/`release.yml` 绿灯 run ID）

**Full-suite trigger:** `T-2: 家族发布点（REL-RB-01 承载全部 29 张子卡的 ER-13 全量双后端测试）`

**Dependencies:** `RB-29b`、`DEP-RB-01`、`DEP-RB-02`、`DEP-RB-03`

**Deliverables:** `N/A`

**Implementation write set:** `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`（受 `DEP-RB-03` 与 `AGENTS.md:135` 规范路径门控的外部 `ER-06a` 网站同步文档，受 `RB-30 -> RB-31 -> RB-31b -> RB-32 -> RB-32b -> RB-33c` 严格串行；本仓生产源码 0 改动）

**Release write set:** `Inherited`

**Files likely touched:** `Release write set`（版本面、`Cargo.lock`、`plan-status.md`、本文「实施证据汇总」）与上列 1 个外部网站文档（本仓生产代码 0 落点 / 0 文件）。

**Docs and compatibility impact:** 受 `DEP-RB-03` 授权门控同步外部网站 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`（承接 `RB-29` 随附文档）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `family release point of REL-RB-01`

**C/D coverage from:** `self`

**Granularity:** `type=release; axis=integrated REL-RB-01 redb release point; recovery=immutable-release sqlite default; complete=yes; self-contained=yes; AC=8/12; VER=9/12; landing=0; prod-files=0; scope=S; deps=RB-29b,DEP-RB-01,DEP-RB-02,DEP-RB-03; writeset=序列化于 RB-29b; release=family release point; split-from=N/A; exception=N/A`

---

### Task RB-31: 升级全量双后端离线 CI 矩阵（显式 SQLite 锁定 + 全量 `compat-redb-queue`）（Track B，拆出 `RB-31b`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在 `.github/workflows/base.yml` 中显式将既有 `compat-offline-core` 锁定为 `--no-default-features --features storage-sqlite`（确保后续 `RB-34` 切换 Cargo 默认 Feature 后 SQLite 全量测试绝不静默失效），并将 `compat-redb-storage` 升级为全量离线 CLI 矩阵 `compat-redb-queue`（`--no-default-features --features storage-redb,storage-queue`），完整保留 `RB-29c` 切好的双特性等价与解锁态双向迁移步骤（`--lib`、`--bin libra-migrate` 与 `post_unlock_libra_migrate_binary_bidirectional_roundtrip`）。

**Out of scope:**
- 在 `.github/workflows/base.yml` 新增纯红库+`keyring` 与官方双后端发行构型+`keyring` 的 `auth_keyring_backend` 验证步骤：已按 `G-03`/`G-09` 拆出由 `RB-31b` 承接。
- 修改 `.github/workflows/release.yml`（归 `RB-32`）或切换默认后端/默认 Feature（归 `REL-RB-03`/`REL-RB-02`）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `base.yml:95-245` `compat-offline-core` 当前未显式指定 `--no-default-features --features storage-sqlite`，默认切换后会静默停止测试 SQLite | `.github/workflows/base.yml:95-245` |

**Acceptance criteria:**
- [ ] `compat-offline-core` 显式传参 `--no-default-features --features storage-sqlite`，不依赖 `Cargo.toml` 的 `default` 特性。
- [ ] `compat-redb-queue` 使用 `--no-default-features --features storage-redb,storage-queue` 运行全量离线核心套件并全绿。
- [ ] `compat-redb-queue` 保留双特性 `--features storage-sqlite,storage-redb,storage-queue` 等价与解锁态双向迁移回归步骤并全绿。
- [ ] 本地等价提取命令与远端 `base.yml` CI 均全绿通过。

**Verification:**
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_matrix_alignment`（覆盖 AC1）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue --test storage_concurrency_stress_test`（覆盖 AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`（覆盖 AC3；AC4 核对远端 `base.yml` run ID）

**Full-suite trigger:** `T-1: 修改 .github/workflows/base.yml 全量双后端离线 CI 矩阵`

**Dependencies:** `RB-30`、`DEP-RB-02`

**Deliverables:** `N/A`

**Implementation write set:** `.github/workflows/base.yml`

**Release write set:** `Inherited`

**Files likely touched:** 1 个 CI 工作流文件（1 落点 / 1 文件）。

**Docs and compatibility impact:** `N/A`（仅 CI 矩阵硬化）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=full dual-backend offline ci matrix explicit sqlite; recovery=revert workflow step; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=1; prod-files=1; scope=S; deps=RB-30,DEP-RB-02; writeset=序列化于 RB-30; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-31b: 新增纯红库与官方双后端 `keyring` 组合 CI 验证步骤（Track B，自 `RB-31` 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在 `.github/workflows/base.yml` 中紧接 `RB-31` 补齐两套含 `keyring` 组合的显式 CI 编译与 `auth_keyring_backend` 验证步骤：纯红库构型 `--no-default-features --features storage-redb,storage-queue,keyring` 与官方双后端发行构型 `--features storage-sqlite,storage-redb,storage-queue,keyring`。

**Out of scope:**
- 修改 `.github/workflows/release.yml`（归 `RB-32`）或切换默认后端/默认 Feature（归 `REL-RB-03`/`REL-RB-02`）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `release.yml:99,131` 当前仅用 `--features keyring`；须在 `RB-32` 打包前先于 `base.yml` 验证红库及双后端叠加 `keyring` 构型 | `.github/workflows/release.yml:99,131`；`Cargo.toml:26,655` |

**Acceptance criteria:**
- [ ] CI 显式验证纯红库 `--no-default-features --features storage-redb,storage-queue,keyring` 编译通过。
- [ ] CI 显式验证纯红库 `--no-default-features --features storage-redb,storage-queue,keyring` 下 `auth_keyring_backend` 全绿。
- [ ] CI 显式验证官方发行构型 `--features storage-sqlite,storage-redb,storage-queue,keyring` 编译通过。
- [ ] CI 显式验证官方发行构型 `--features storage-sqlite,storage-redb,storage-queue,keyring` 下 `auth_keyring_backend` 全绿（且远端 `base.yml` CI 全绿）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-redb,storage-queue,keyring --test auth_keyring_backend`（覆盖 AC1、AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue,keyring --test auth_keyring_backend`（覆盖 AC3、AC4；AC4 同步核对远端 `base.yml` run ID）

**Full-suite trigger:** `T-1: 修改 .github/workflows/base.yml 新增 keyring 存储特征矩阵步骤`

**Dependencies:** `RB-31`、`DEP-RB-02`

**Deliverables:** `N/A`

**Implementation write set:** `.github/workflows/base.yml`

**Release write set:** `Inherited`

**Files likely touched:** 1 个 CI 工作流文件（1 落点 / 1 文件）。

**Docs and compatibility impact:** `N/A`（仅 CI 矩阵硬化）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=ci redb-only and dual-backend keyring matrix steps; recovery=revert workflow step; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=1; scope=S; deps=RB-31,DEP-RB-02; writeset=序列化于 RB-31; release=independent; split-from=RB-31; exception=N/A`

---

### Task RB-32: 升级 `release.yml` 多平台双后端精确成品协同打包（双后端 `libra` + 无 `keyring` `libra-migrate`）（Track B，拆出 `RB-32b`、`RB-33`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在切换默认新仓后端之前升级 `.github/workflows/release.yml`（实盘核查该工作流向 `build/` 暂存单文件平台二进制与 `.sha256`/`.size` 并上传 R2，非 tar 归档）：使全部目标平台按**各自真实发行命令**先后构建并暂存：① 官方双后端 `libra`（`cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue,keyring --bin libra --target <triple>` → `build/libra-<os>-<arch>` + `.sha256`/`.size`，Windows 同步产出 `.exe` 与无后缀双份）；② 官方迁移器 `libra-migrate`（`cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate --target <triple>`，**明确不含 `keyring`** → `build/libra-migrate-<os>-<arch>` + `.sha256`/`.size`，Windows 同理）。

**Out of scope:**
- 精确暂存双成品切前互操作与默认 SQLite 不变端到端审计：已按 `G-03`/`G-09` 拆出由 `RB-32b` 承接（在 `RB-33` 切默认前执行）。
- 切换官方发行运行时默认后端至 `Redb`（归 `REL-RB-03`）与切换 `Cargo.toml` 默认 Feature（归 `REL-RB-02`）。

**Current evidence:**

| 事实（基线 `d242b90`） | 证据 |
|---|---|
| `.github/workflows/release.yml:99-151` 当前仅以 `--features keyring` 构建 `libra` 并向 `build/` 暂存单文件二进制、`.sha256` 与 `.size`（非归档包） | `.github/workflows/release.yml:99-151` |

**Acceptance criteria:**
- [ ] `.github/workflows/release.yml` 在全部平台矩阵以 `--locked --release --features storage-sqlite,storage-redb,storage-queue,keyring --bin libra` 构建并向 `build/` 暂存 `libra-<os>-<arch>`（Windows 含 `.exe` 与无后缀双份）及 `.sha256`/`.size`。
- [ ] `.github/workflows/release.yml` 在全部平台矩阵以 `--locked --release --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`（**不含 `keyring`**）构建并向 `build/` 暂存 `libra-migrate-<os>-<arch>`（Windows 含 `.exe` 与无后缀双份）及 `.sha256`/`.size`。
- [ ] 本地严格复现上述两条独立构建命令并拷贝至 `mktemp -d` 独立暂存目录，核验双产物 `.sha256` 一致（退出时经 `trap` 仅清理该目录）。
- [ ] `gh release create` 触发的远端 `release.yml` 全平台上传产物清单同时包含 `libra-<os>-<arch>`、`libra-migrate-<os>-<arch>` 及其 `.sha256`/`.size` 边车文件。

**Verification:**
- [ ] `cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue,keyring --bin libra`（按 `release.yml` 实参独立构建含 `keyring` 的 `libra` 成品，覆盖 AC3）
- [ ] `cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`（按 `release.yml` 实参独立构建**不含 `keyring`** 的 `libra-migrate` 成品，覆盖 AC3）
- [ ] `rg -q -- "--features storage-sqlite,storage-redb,storage-queue,keyring --bin libra" .github/workflows/release.yml`（覆盖 AC1）
- [ ] `rg -q -- "--features storage-sqlite,storage-redb,storage-queue --bin libra-migrate" .github/workflows/release.yml`（覆盖 AC2）
- [ ] 按 `ER-08` 核验 `gh release create` 触发的远端 `release.yml` 全平台上传产物清单同时包含双后端 `libra`、`libra-migrate` 与 `.sha256`/`.size`（覆盖 AC4）

**Full-suite trigger:** `T-1: 修改 .github/workflows/release.yml 发行流水线`

**Dependencies:** `RB-31b`、`DEP-RB-02`

**Deliverables:** `N/A`

**Implementation write set:** `.github/workflows/release.yml`

**Release write set:** `Inherited`

**Files likely touched:** 上列 1 个发行工作流文件（1 落点 / 1 文件）。

**Docs and compatibility impact:** `N/A`（用户升级与默认切换文档随 `REL-RB-03` 统一发布）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=release workflow dual-backend binary packaging; recovery=revert workflow step; complete=yes; self-contained=yes; AC=4/8; VER=5/8; landing=1; prod-files=1; scope=S; deps=RB-31b,DEP-RB-02; writeset=序列化于 RB-31b; release=independent; split-from=N/A; exception=N/A`

---

### Task RB-32b: 切默认前精确暂存双成品互操作与默认 SQLite 不变审计（Track B，自 `RB-32` 拆出）

**Task type:** `audit`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 在 `REL-RB-03` 切换官方发行运行时默认后端之前（不修改任何生产源码），于 `tests/storage_concurrency_stress_test.rs` 内按 `release.yml` 各自真实特性分别构建并经独立 `mktemp -d` 暂存双后端 `libra`（含 `keyring`）与 `libra-migrate`（**不含 `keyring`**），经 `LIBRA_PACKAGED_BIN` 与 `LIBRA_MIGRATE_PACKAGED_BIN` 核验双 `.sha256` 后实测切前双向互操作以及 `libra init`/`libra config set --global` 默认行为零变化。

**Out of scope:**
- 修改任何 `src/**` 或 `.github/workflows/**` 生产文件（`audit` 卡生产代码 0 落点 / 0 文件）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 两枚发行成品须按各自真实标志独立构建、经 `mktemp -d` 暂存并核验 `.sha256` 后实测切前双向互操作与 SQLite 默认不变 | `.github/workflows/release.yml:99-151`；`RB-32` |

**Acceptance criteria:**
- [ ] **真实暂存成品切前互操作 E2E：** 校验双 `.sha256` 后直接拉起 `LIBRA_PACKAGED_BIN`（含 `keyring`）与 `LIBRA_MIGRATE_PACKAGED_BIN`（**无 `keyring`**）完成：`SQLite 仓 → 成品 libra-migrate --to redb → 成品 libra 读写提交 → 成品 libra-migrate --to sqlite → 成品 libra 读回 100% 一致`。
- [ ] 上述暂存成品 `libra` 在未设 `LIBRA_STORAGE_BACKEND` 时执行 `libra init` 仍默认创建 SQLite `.libra/libra.db`（`RB-33` 前零默认变化）。
- [ ] 上述暂存成品 `libra` 在未设 `LIBRA_STORAGE_BACKEND` 时执行 `libra config set --global` 仍默认创建/写入 SQLite `config.db`。
- [ ] 上述切前默认 `libra config set --global` 执行后目录内绝不创建 `config.redb`。

**Verification:**
- [ ] `cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue,keyring --bin libra`（构建含 `keyring` 的 `libra` 成品）
- [ ] `cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`（构建**不含 `keyring`** 的 `libra-migrate` 成品）
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=packaged_release_dual_backend_libra_and_migrate_coherence_pre_cutover)'`（new，发现门）
- [ ] `bash -euo pipefail -c 'stage="$(mktemp -d "${TMPDIR:-/tmp}/libra-rb32-stage.XXXXXX")"; trap "rm -rf -- \"$stage\"" EXIT; cp target/release/libra "$stage/libra"; cp target/release/libra-migrate "$stage/libra-migrate"; (sha256sum "$stage/libra" 2>/dev/null || shasum -a 256 "$stage/libra") | awk "{print \$1}" > "$stage/libra.sha256"; (sha256sum "$stage/libra-migrate" 2>/dev/null || shasum -a 256 "$stage/libra-migrate") | awk "{print \$1}" > "$stage/libra-migrate.sha256"; source .env.test && LIBRA_PACKAGED_BIN="$stage/libra" LIBRA_MIGRATE_PACKAGED_BIN="$stage/libra-migrate" cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E '\''test(=packaged_release_dual_backend_libra_and_migrate_coherence_pre_cutover)'\'''`（new，使用独立 `mktemp -d` 暂存并 `trap` 仅清理该目录，覆盖 AC1–AC4）

**Full-suite trigger:** `none`

**Dependencies:** `RB-32`

**Deliverables:** `tests/storage_concurrency_stress_test.rs`（切默认前精确暂存双成品互操作与默认 SQLite 审计用例）

**Implementation write set:** `tests/storage_concurrency_stress_test.rs`

**Release write set:** `N/A`

**Files likely touched:** 1 个集成测试文件（生产代码 0 落点 / 0 文件；1 个审计产物文件 ≤ 5 满足 `G-11` `audit` `S` 上限）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `revert`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `no-release`（按 `G-07` `audit` 不 bump 版本、不产 artifact；本审计测试提交直接合入集成分支并在 `RB-33c` 发布门进入用户可见渠道前完成切前验收）

**C/D coverage from:** `RB-33c`

**Granularity:** `type=audit; axis=pre-cutover staged dual-artifact interop audit; recovery=revert audit test module; complete=yes; self-contained=yes; AC=4/20; VER=4/20; landing=0; prod-files=0; scope=S; deps=RB-32; writeset=序列化于 RB-32; release=no-release; split-from=RB-32; exception=N/A`

---

### Task RB-33: 官方发行运行时默认后端切至 `Redb`、默认建仓/配置验收与仓内文档同步（Track B — `REL-RB-03` 子卡 1/2，自 `RB-32` 拆出，拆出 `RB-33b`、`RB-33c`、`RB-34`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 作为 `G-08` 家族 `REL-RB-03`（`RB-33` + `RB-33b` + `RB-33c`）的实现子卡，将 `src/storage/backend.rs` 在启用 `storage-redb` 时的无环境变量覆盖默认后端切换为 `StorageBackendKind::Redb`（纯 `storage-sqlite` 构建仍默认为 `Sqlite`），于精确暂存双后端成品 `libra`（含 `keyring`）验证切后默认红库新仓初始化（含 `0400` spool 墓碑）与默认全局配置墓碑预占建库，并按 `ER-06a` 随附同步仓内 `COMPATIBILITY.md`、`docs/error-codes.md`、`docs/commands/{init,agent}.md`（EN + zh-CN）。

**Out of scope:**
- 切默认后精确暂存双成品仓库/配置/全新仓活 Spool 双向 SQLite 回退与 Step 2.6 解封发布前审计：已按 `G-03`/`G-08`/`G-09` 同家族拆出由 `RB-33b` 承接，并由唯一发布点 `RB-33c` 合并网站同步与 `T-2 + T-3` 原子发布。
- 切换 `Cargo.toml` 默认 Feature 至 `["storage-redb", "storage-queue"]`：归 `REL-RB-02`（`RB-34`..`RB-34c`）。
- 移除 `storage-sqlite` Feature 或 `sea-orm` 依赖：归延后项 `DEFER-RB-02`。

**Current evidence:**

| 事实（区分已核实基线与前置预期状态） | 证据 |
|---|---|
| 前置 `RB-32`/`RB-32b` 已建立并实测 `release.yml` 双后端 `libra`（含 `keyring`）与无 `keyring` `libra-migrate` 精确暂存成品 | `.github/workflows/release.yml:99-151`；`docs/commands/{init,agent}.md`；`COMPATIBILITY.md` |

**Acceptance criteria:**
- [ ] `src/storage/backend.rs` 在启用 `storage-redb` 时将无覆盖默认后端切换为 `Redb`。
- [ ] 纯 `storage-sqlite` 构建（`--no-default-features --features storage-sqlite`）在无覆盖时仍保持默认后端为 `Sqlite`。
- [ ] 经独立 `mktemp -d` 暂存并核验 `.sha256` 的双后端成品 `libra`（含 `keyring`，无环境变量覆盖）执行 `libra init` 默认创建 `.libra/redb/git.gen-00000001.redb`、`.libra/private/capture.gen-00000001.redb` 与 `.libra/storage-format.json`（不创建 `.libra/libra.db`）。
- [ ] 上述默认 `libra init` 同步建立 `.libra/private/capture-spool/{tmp,ready,quarantine}` `0400` 普通文件墓碑。
- [ ] 上述暂存成品 `libra`（无环境变量覆盖）在隔离空目录执行 `libra config set --global` 先以非覆写 `hard_link` 预占 `0400` 路径墓碑 `config.db`，再创建 `config.redb` 与 `config-format.json(backend="redb", finalized=true, legacy_sqlite_tombstone=true)`。（注：仓内 `COMPATIBILITY.md`、`docs/error-codes.md`、`docs/commands/{init,agent}.md` 同步与 `compat_command_docs_examples_section` 属 `ER-06a` 同卡必需文档同步门，按 `G-03` 不计入本卡特有 AC 配额。）

**Verification:**
- [ ] `cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue,keyring --bin libra`（按 `release.yml` 实参构建含 `keyring` 的 `libra` 成品）
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=packaged_release_default_redb_init_and_global_config_cutover)'`（new，发现门）
- [ ] `bash -euo pipefail -c 'stage="$(mktemp -d "${TMPDIR:-/tmp}/libra-rb33-stage.XXXXXX")"; trap "rm -rf -- \"$stage\"" EXIT; cp target/release/libra "$stage/libra"; (sha256sum "$stage/libra" 2>/dev/null || shasum -a 256 "$stage/libra") | awk "{print \$1}" > "$stage/libra.sha256"; source .env.test && LIBRA_PACKAGED_BIN="$stage/libra" cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E '\''test(=packaged_release_default_redb_init_and_global_config_cutover)'\'''`（new，覆盖 AC1、AC3、AC4、AC5）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`（验证纯 SQLite 构建仍默认为 `Sqlite`，覆盖 AC2）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --test compat_command_docs_examples_section`（`ER-06a` 随附文档契约）

**Full-suite trigger:** `N/A`（`family child of REL-RB-03`；随 `RB-33c` 承接 `T-2 + T-3` 全量双矩阵与网站同步）

**Dependencies:** `RB-32b`、`DEP-RB-02`

**Deliverables:** `N/A`

**Implementation write set:** `src/storage/backend.rs`、`tests/storage_concurrency_stress_test.rs`、`COMPATIBILITY.md`、`docs/error-codes.md`、`docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`

**Release write set:** `N/A`

**Files likely touched:** 上列 1 个行为生产落点文件（`src/storage/backend.rs`）+ 1 个测试文件 + 6 个 `ER-06a` 随附仓内文档（不计入 `G-04` 生产计数）。

**Docs and compatibility impact:** 随附更新 `COMPATIBILITY.md`、`docs/error-codes.md`、`docs/commands/{init,agent}.md`（EN + zh-CN）；外部网站 `{init,agent}.en.md` 在同家族发布点 `RB-33c` 满足 `DEP-RB-03` 后同步。

**Rollback mode:** `forward-only`

**Migration and rollback:** 官方 `release.yml` 发行成品 `libra` 内建双后端，切默认后可经同批 `libra-migrate --to sqlite` 无损回退并可发 patch 将 `backend.rs` 切回 `Sqlite`。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-03`

**C/D coverage from:** `RB-33c`

**Granularity:** `type=implementation; axis=backend default redb init config and repo docs; recovery=patch backend default back and migrate; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=1; prod-files=1; scope=M; deps=RB-32b,DEP-RB-02; writeset=序列化于 RB-32b; release=family child; split-from=RB-32; exception=N/A`

---

### Task RB-33b: 切默认后精确暂存双成品仓库/配置/全新仓活 Spool 双向 SQLite 回退发布前审计（Track B — `REL-RB-03` 子卡 2/2，自 `RB-33` 拆出）

**Task type:** `audit`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 作为 `G-08` 家族 `REL-RB-03` 的发布前只读审计子卡（不改动任何生产源码），于 `tests/storage_concurrency_stress_test.rs` 内针对 `RB-33` 切默认后的两枚按 `release.yml` 实参独立构建并经 `mktemp -d` 暂存+`.sha256` 校验的真实成品（含 `keyring` 的 `libra` 与**不含 `keyring`** 的 `libra-migrate`），逐项实测仓库双向 SQLite 回退、全局配置双向 SQLite 回退、全新 Redb 仓活 Spool 对账回退与 Step 2.6 解封旧 Hook 恢复。

**Out of scope:**
- 修改任何 `src/**` 或文档生产文件（`audit` 卡生产代码 0 落点 / 0 文件）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 切默认发布前必须在两枚精确暂存真实成品上证明默认 Redb 写入的新数据与活 Spool 可 100% 无损回退至 SQLite 并被同一成品无缝读回 | `ADR-RB-07`；`RB-32`；`RB-33` |

**Acceptance criteria:**
- [ ] **官方暂存成品仓库双向 SQLite 回退 E2E：** 核验双 `.sha256` 后直接拉起 `LIBRA_PACKAGED_BIN`（含 `keyring`）与 `LIBRA_MIGRATE_PACKAGED_BIN`（**不含 `keyring`**）完成 `成品 libra init (默认 Redb) → 写入新提交与捕获 → 成品 libra-migrate --to sqlite → 同一成品 libra 打开 SQLite 读回 100% 切后新数据`。
- [ ] **官方暂存成品配置双向 SQLite 回退 E2E：** 上述两枚暂存成品完成 `成品 libra config set --global (默认 Redb) → 成品 libra-migrate --global --to sqlite → 同一成品 libra 读回 100% 切后配置`。
- [ ] **官方暂存成品全新 Redb 仓活 Spool 对账回退 E2E：** 构造缺失墓碑且预存 `capture-spool/ready/` 活事件的全新 Redb 仓，拉起暂存成品 `libra-migrate --to sqlite` 完成对账并 100% 落入 `capture.db`。
- [ ] **官方暂存成品回退 Step 2.6 解封旧 Hook 恢复：** 上述回退完成后，旧 SQLite Hook 恢复正常向 `capture-spool/ready/` 入队。

**Verification:**
- [ ] `cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue,keyring --bin libra`（按 `release.yml` 实参独立构建含 `keyring` 的 `libra` 成品）
- [ ] `cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`（按 `release.yml` 实参独立构建**不含 `keyring`** 的 `libra-migrate` 成品）
- [ ] `source .env.test && cargo nextest list --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E 'test(=packaged_release_artifacts_default_redb_cutover_and_sqlite_rollback)'`（new，发现门）
- [ ] `bash -euo pipefail -c 'stage="$(mktemp -d "${TMPDIR:-/tmp}/libra-rb33b-stage.XXXXXX")"; trap "rm -rf -- \"$stage\"" EXIT; cp target/release/libra "$stage/libra"; cp target/release/libra-migrate "$stage/libra-migrate"; (sha256sum "$stage/libra" 2>/dev/null || shasum -a 256 "$stage/libra") | awk "{print \$1}" > "$stage/libra.sha256"; (sha256sum "$stage/libra-migrate" 2>/dev/null || shasum -a 256 "$stage/libra-migrate") | awk "{print \$1}" > "$stage/libra-migrate.sha256"; source .env.test && LIBRA_PACKAGED_BIN="$stage/libra" LIBRA_MIGRATE_PACKAGED_BIN="$stage/libra-migrate" cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E '\''test(=packaged_release_artifacts_default_redb_cutover_and_sqlite_rollback)'\'''`（new，覆盖 AC1–AC4）

**Full-suite trigger:** `N/A`（`family child of REL-RB-03`；随 `RB-33c` 承接 `T-2 + T-3` 全量双矩阵）

**Dependencies:** `RB-33`

**Deliverables:** `tests/storage_concurrency_stress_test.rs`（切默认后精确暂存双成品双向 SQLite 回退与活 Spool 对账审计套件）

**Implementation write set:** `tests/storage_concurrency_stress_test.rs`

**Release write set:** `N/A`

**Files likely touched:** 1 个集成测试文件（生产代码 0 落点 / 0 文件；1 个审计产物文件 ≤ 5 满足 `G-11` `audit` `S` 上限）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `revert`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-03`

**C/D coverage from:** `RB-33c`

**Granularity:** `type=audit; axis=cutover staged dual-artifact sqlite rollback audit; recovery=revert audit test module; complete=yes; self-contained=yes; AC=4/20; VER=4/20; landing=0; prod-files=0; scope=S; deps=RB-33; writeset=序列化于 RB-33; release=family child; split-from=RB-33; exception=N/A`

---

### Task RB-33c: `REL-RB-03` 家族唯一原子发布点（官方发行默认切红库聚合验收、`DEP-RB-03` 网站同步与全量双矩阵发版）（Track B，自 `RB-33` 拆出）

**Task type:** `release`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 作为 `G-08` 家族 `REL-RB-03`（`RB-33` + `RB-33b`）的唯一原子发布点，本卡不引入任何新行为代码：按 `release.yml` 实参独立构建并经 `mktemp -d` 暂存双后端 `libra`（含 `keyring`）与 `libra-migrate`（**不含 `keyring`**）并核验 `.sha256`，统一聚合重跑 `RB-33` 切后默认建仓/配置门、`RB-33b` 全部双向 SQLite 回退与活 Spool 审计及纯 SQLite 默认回归门，完成 `T-2 + T-3` 全量双矩阵验证、`patch + 1` 发布与满足 `DEP-RB-03` 授权硬门后的外部网站 `{init,agent}.en.md` 同步核验。

**Out of scope:**
- 切换 `Cargo.toml` 默认 Feature 至 `["storage-redb", "storage-queue"]`：归 `REL-RB-02`（`RB-34`..`RB-34c`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RB-33`（切默认后端与仓内文档）与 `RB-33b`（精确暂存双成品双向回退审计）必须在同一发布点联合通过并满足 `DEP-RB-03` 网站硬门后方可对外发版 | `G-08`；`REL-RB-03`；`RB-33`；`RB-33b`；`DEP-RB-03` |

**Acceptance criteria:**
- [ ] `src/storage/backend.rs` 启用 `storage-redb` 默认为 `Redb`，且纯 `storage-sqlite` 构建仍默认为 `Sqlite`。
- [ ] 精确暂存双后端成品 `libra`（含 `keyring`）的默认红库 `init`（含 `0400` spool 墓碑）与全局配置墓碑建库门通过（`RB-33` 门）。
- [ ] 精确暂存双成品（`libra` + 无 `keyring` `libra-migrate`）仓库/配置/全新仓活 Spool 双向 SQLite 回退与 Step 2.6 解封审计全绿（`RB-33b` 门）。
- [ ] 仓内 `COMPATIBILITY.md`、`docs/error-codes.md` 与 `docs/commands/{init,agent}.md`（EN + zh-CN）通过 `compat_command_docs_examples_section`。
- [ ] 满足 `DEP-RB-03` 授权硬门后，按「执行检查必备需求」第 4 条完成网站 `{init,agent}.en.md` 发布流水线验收并双页面线上可访问。（注：`ER-13` `T-2 + T-3` 全量双矩阵与 `ER-08` 版本发布/远端 `release.yml` 绿灯属统一发布基线，按 `G-03` 常规履约不重复占用本卡特有 AC 配额。）

**Verification:**
- [ ] `cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue,keyring --bin libra`（构建含 `keyring` 的 `libra` 成品）
- [ ] `cargo build --locked --release --features storage-sqlite,storage-redb,storage-queue --bin libra-migrate`（构建**不含 `keyring`** 的 `libra-migrate` 成品）
- [ ] `bash -euo pipefail -c 'stage="$(mktemp -d "${TMPDIR:-/tmp}/libra-rb33c-stage.XXXXXX")"; trap "rm -rf -- \"$stage\"" EXIT; cp target/release/libra "$stage/libra"; cp target/release/libra-migrate "$stage/libra-migrate"; (sha256sum "$stage/libra" 2>/dev/null || shasum -a 256 "$stage/libra") | awk "{print \$1}" > "$stage/libra.sha256"; (sha256sum "$stage/libra-migrate" 2>/dev/null || shasum -a 256 "$stage/libra-migrate") | awk "{print \$1}" > "$stage/libra-migrate.sha256"; source .env.test && LIBRA_PACKAGED_BIN="$stage/libra" LIBRA_MIGRATE_PACKAGED_BIN="$stage/libra-migrate" cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue --test storage_concurrency_stress_test -E '\''test(=packaged_release_default_redb_init_and_global_config_cutover) | test(=packaged_release_artifacts_default_redb_cutover_and_sqlite_rollback)'\'''`（聚合验证 `RB-33` 与 `RB-33b`，覆盖 AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --lib -E 'test(/^storage::/)'`（覆盖 AC1）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --test compat_command_docs_examples_section`（覆盖 AC4）
- [ ] 先核验 `DEP-RB-03` 授权门控（默认校验 `/Volumes/Sea/libra-backend` 存在且在 `cf` 分支；若缺席且无已落盘用户新授权则立即 STOP），再按「执行检查必备需求」第 4 条完成 `{init,agent}.en.md` 网站同步、构建、独立 Claude `VERDICT: PASS`、签名推送 `cf` 与双页面线上核验（覆盖 AC5；并按 `ER-13`/`ER-08` 跑通 `T-2 + T-3` 与登记远端 `base.yml`/`codeql.yml`/`release.yml` 绿灯 run ID）

**Full-suite trigger:** `T-2 + T-3: REL-RB-03 家族发布点（切换官方发行公开 CLI 默认存储后端为 Redb，ER-13 全量双矩阵与 release.yml 均须全绿）`

**Dependencies:** `RB-33b`、`DEP-RB-02`、`DEP-RB-03`

**Deliverables:** `N/A`

**Implementation write set:** `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`、`/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`（受 `DEP-RB-03` 与 `AGENTS.md:135` 规范路径门控的外部 `ER-06a` 网站同步文档，与 `RB-30` 重叠的 `agent.en.md` 经 `RB-30 -> RB-31 -> RB-31b -> RB-32 -> RB-32b -> RB-33c` 严格串行；本仓生产源码 0 改动）

**Release write set:** `Inherited`

**Files likely touched:** `Release write set`（版本面、`Cargo.lock`、`plan-status.md`、本文「实施证据汇总」）与上列 2 个外部网站文档（本仓生产代码 0 落点 / 0 文件）。

**Docs and compatibility impact:** 随同发布 `RB-33` 更新的仓内文档并受 `DEP-RB-03` 授权门控同步外部网站 `{init,agent}.en.md`。

**Rollback mode:** `forward-only`

**Migration and rollback:** 官方 `release.yml` 发行成品 `libra` 内建双后端，切默认后可经同批 `libra-migrate --to sqlite`（与 `--global --to sqlite`）无损回退并由同一官方 `libra` 读回 100% 数据，同时可发新 patch 将 `backend.rs` 默认切回 `Sqlite`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `family release point of REL-RB-03`

**C/D coverage from:** `self`

**Granularity:** `type=release; axis=integrated REL-RB-03 default redb release point; recovery=patch backend default back and migrate; complete=yes; self-contained=yes; AC=5/12; VER=6/12; landing=0; prod-files=0; scope=S; deps=RB-33b,DEP-RB-02,DEP-RB-03; writeset=序列化于 RB-33b; release=family release point; split-from=RB-33; exception=N/A`

---

### Task RB-34: 切换 `Cargo.toml` 默认 Feature 至 `storage-redb,storage-queue`、双源码构建与仓库门控（Track B — `REL-RB-02` 子卡 1/2，自 `RB-33` 拆出，拆出 `RB-34b`、`RB-34c`）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 作为 `G-08` 家族 `REL-RB-02`（`RB-34` + `RB-34b` + `RB-34c`）的实现子卡，将 `Cargo.toml` 的 crate 默认特性切换为 `default = ["storage-redb", "storage-queue"]`（使 `cargo install` / `cargo build --features keyring` 等源码构建方默认启用 `redb` + 队列而不编译 `storage-sqlite`，同时完整保留 `--no-default-features --features storage-sqlite,keyring` 纯 SQLite 回退构建），并在 `tests/storage_concurrency_stress_test.rs` 验证最终纯默认 + `keyring` 二进制的红库新仓初始化与未迁移 SQLite 仓库 fail-closed 拒收，同步在 `COMPATIBILITY.md` 记录 Cargo 默认特性切换。

**Out of scope:**
- 最终默认 + `keyring` 二进制在隔离未迁移 `GlobalConfig` 与 `SystemConfig` 夹具上的全部 4 类读写（`config get/set --global` 与 `config get/set --system`）端到端审计：已按 `G-03`/`G-08`/`G-09` 同家族拆出由 `RB-34b` 承接，并由唯一发布点 `RB-34c` 合并原子发布。
- 移除 `storage-sqlite` Feature 或 `sea-orm` 依赖：归延后项 `DEFER-RB-02`。

**Current evidence:**

| 事实（区分已核实基线与前置预期状态） | 证据 |
|---|---|
| 已核实基线（`d242b90`）：`Cargo.toml:15` 当前为 `default = []`；前置 `RB-31` 已将 `compat-offline-core` 显式锁定为 `--no-default-features --features storage-sqlite`，`RB-33` 已完成官方发行成品默认后端切换与文档/网站同步 | `Cargo.toml:15`；`.github/workflows/base.yml`；`COMPATIBILITY.md` |

**Acceptance criteria:**
- [ ] `Cargo.toml` 默认特性切换为 `default = ["storage-redb", "storage-queue"]`。
- [ ] 纯默认 + `keyring` 源码构建 `cargo build --locked --release --target-dir target/rb34-default-keyring --features keyring --bin libra`（不含 `storage-sqlite`，独立目标目录）通过。
- [ ] 纯 SQLite 回退源码构建 `cargo build --locked --release --target-dir target/rb34-sqlite-keyring --no-default-features --features storage-sqlite,keyring --bin libra`（独立目标目录，不覆写默认红库二进制）通过。
- [ ] 最终纯默认 + `keyring` 二进制（`target/rb34-default-keyring/release/libra`，校验 SHA-256 与 SQLite 回退产物相异）执行 `libra init` 默认创建 `.libra/redb/git.gen-00000001.redb`、`.libra/private/capture.gen-00000001.redb` 与 `.libra/storage-format.json`（不创建 `.libra/libra.db`）。
- [ ] 最终纯默认 + `keyring` 二进制（`target/rb34-default-keyring/release/libra`）打开未迁移旧 SQLite 仓库（`.libra/libra.db`）时立即 fail-closed 报错并输出 `libra-migrate` 升级指引。

**Verification:**
- [ ] `rg -q '^default = \["storage-redb", "storage-queue"\]' Cargo.toml`（覆盖 AC1）
- [ ] `cargo build --locked --release --target-dir target/rb34-default-keyring --features keyring --bin libra`（输出至独立目标目录 `target/rb34-default-keyring`，覆盖 AC2）
- [ ] `cargo build --locked --release --target-dir target/rb34-sqlite-keyring --no-default-features --features storage-sqlite,keyring --bin libra`（输出至独立目标目录 `target/rb34-sqlite-keyring`，绝不覆写默认二进制，覆盖 AC3）
- [ ] `source .env.test && cargo nextest list --features keyring --test storage_concurrency_stress_test -E 'test(=default_redb_keyring_binary_init_and_unmigrated_repo_fail_closed)'`（new，发现门）
- [ ] `bash -euo pipefail -c 'bin="$PWD/target/rb34-default-keyring/release/libra"; sql_bin="$PWD/target/rb34-sqlite-keyring/release/libra"; test -x "$bin" && test -x "$sql_bin"; sha="$( (sha256sum "$bin" 2>/dev/null || shasum -a 256 "$bin") | awk "{print \$1}" )"; sql_sha="$( (sha256sum "$sql_bin" 2>/dev/null || shasum -a 256 "$sql_bin") | awk "{print \$1}" )"; test "$sha" != "$sql_sha"; source .env.test && LIBRA_DEFAULT_KEYRING_BIN="$bin" LIBRA_DEFAULT_KEYRING_SHA256="$sha" cargo nextest run --no-tests=fail --features keyring --test storage_concurrency_stress_test -E '\''test(=default_redb_keyring_binary_init_and_unmigrated_repo_fail_closed)'\'''`（new，先校验双产物 SHA-256 相异再直接执行 `target/rb34-default-keyring/release/libra` 并核对 `LIBRA_DEFAULT_KEYRING_SHA256`，覆盖 AC4、AC5）

**Full-suite trigger:** `N/A`（`family child of REL-RB-02`；随 `RB-34c` 承接 `T-1 + T-2` 全量双矩阵验证）

**Dependencies:** `RB-33c`、`DEP-RB-02`

**Deliverables:** `N/A`

**Implementation write set:** `Cargo.toml`、`Cargo.lock`、`tests/storage_concurrency_stress_test.rs`、`COMPATIBILITY.md`

**Release write set:** `N/A`

**Files likely touched:** 上列 1 个行为生产落点文件（`Cargo.toml`）+ `Cargo.lock` + 1 个测试文件 + 1 个 `ER-06a` 随附兼容文档（不计入 `G-04` 生产计数）。

**Docs and compatibility impact:** 随附更新 `COMPATIBILITY.md` 记录源码构建 Cargo 默认特性已切换为 `["storage-redb", "storage-queue"]` 与 `--no-default-features --features storage-sqlite,keyring` 回退构型。

**Rollback mode:** `forward-only`

**Migration and rollback:** 源码构建方若需纯 SQLite 回退可直接使用 `--no-default-features --features storage-sqlite,keyring`；全仓若需回滚可发布新 patch 将 `Cargo.toml` 切回 `default = ["storage-sqlite"]`。

**Estimated scope:** `M`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-02`

**C/D coverage from:** `RB-34c`

**Granularity:** `type=implementation; axis=cargo default feature redb builds and repo gate; recovery=patch cargo default back to sqlite; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=1; prod-files=1; scope=M; deps=RB-33c,DEP-RB-02; writeset=序列化于 RB-33c; release=family child; split-from=RB-33; exception=N/A`

---

### Task RB-34b: 最终默认 + `keyring` 二进制未迁移 Global/System 配置全 4 路径拒收发布前审计（Track B — `REL-RB-02` 子卡 2/2，自 `RB-34` 拆出）

**Task type:** `audit`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 作为 `G-08` 家族 `REL-RB-02` 的发布前只读审计子卡（不改动任何生产源码），于 `tests/storage_concurrency_stress_test.rs` 内交付针对**`RB-34` 切默认后独立构建于 `target/rb34-default-keyring/release/libra`（`--features keyring`，不含 `storage-sqlite`）的真实二进制**的全部 4 类未迁移配置 CLI 端到端拒收审计用例，逐一验证 `config get --global`、`config set --global`、`config get --system` 与 **`config set --system`** 全部 fail-closed 报错并输出 `libra-migrate --global/--system` 指引、绝不返回空配置、绝不新建竞争性 `config.redb`。

**Out of scope:**
- 修改任何 `src/**` 或 `Cargo.toml` 生产代码（`audit` 卡生产代码 0 落点 / 0 文件）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 仅凭早期单元测试或只测 `config get --system` 无法证明最终默认+`keyring` 二进制的 `config set --system` 写路径不会建出竞争性 `config.redb`；必须在 `RB-34c` 发布前对独立目标目录产出的最终成品二进制全 4 路径实测 | `ADR-RB-01`、`ADR-RB-02` #2；`RB-34` |

**Acceptance criteria:**
- [ ] 最终默认 + `keyring` 二进制（`target/rb34-default-keyring/release/libra`）在隔离未迁移 `LIBRA_CONFIG_GLOBAL_DB` 夹具上执行 `libra config get --global` fail-closed 并输出 `libra-migrate --global` 指引（绝不返回空配置）。
- [ ] 最终默认 + `keyring` 二进制（`target/rb34-default-keyring/release/libra`）在隔离未迁移 `LIBRA_CONFIG_GLOBAL_DB` 夹具上执行 `libra config set --global` fail-closed 并输出 `libra-migrate --global` 指引（绝不新建 `config.redb`）。
- [ ] 最终默认 + `keyring` 二进制（`target/rb34-default-keyring/release/libra`）在隔离未迁移 `LIBRA_CONFIG_SYSTEM_DB` 夹具上执行 `libra config get --system` fail-closed 并输出 `libra-migrate --system` 指引（绝不返回空配置）。
- [ ] 最终默认 + `keyring` 二进制（`target/rb34-default-keyring/release/libra`）在隔离未迁移 `LIBRA_CONFIG_SYSTEM_DB` 夹具上执行 `libra config set --system` fail-closed 并输出 `libra-migrate --system` 指引（绝不新建 `config.redb`）。

**Verification:**
- [ ] `source .env.test && cargo nextest list --features keyring --test storage_concurrency_stress_test -E 'test(=final_default_keyring_binary_rejects_unmigrated_global_get_and_set) | test(=final_default_keyring_binary_rejects_unmigrated_system_get_and_set)'`（new，发现门）
- [ ] `cargo build --locked --release --target-dir target/rb34-default-keyring --features keyring --bin libra`（在独立目标目录构建最终纯默认+`keyring` 二进制供下述两门实测）
- [ ] `bash -euo pipefail -c 'bin="$PWD/target/rb34-default-keyring/release/libra"; test -x "$bin"; sha="$( (sha256sum "$bin" 2>/dev/null || shasum -a 256 "$bin") | awk "{print \$1}" )"; source .env.test && LIBRA_DEFAULT_KEYRING_BIN="$bin" LIBRA_DEFAULT_KEYRING_SHA256="$sha" cargo nextest run --no-tests=fail --features keyring --test storage_concurrency_stress_test -E '\''test(=final_default_keyring_binary_rejects_unmigrated_global_get_and_set)'\'''`（new，直接拉起 `target/rb34-default-keyring/release/libra` 并核验 SHA-256，覆盖 AC1、AC2）
- [ ] `bash -euo pipefail -c 'bin="$PWD/target/rb34-default-keyring/release/libra"; test -x "$bin"; sha="$( (sha256sum "$bin" 2>/dev/null || shasum -a 256 "$bin") | awk "{print \$1}" )"; source .env.test && LIBRA_DEFAULT_KEYRING_BIN="$bin" LIBRA_DEFAULT_KEYRING_SHA256="$sha" cargo nextest run --no-tests=fail --features keyring --test storage_concurrency_stress_test -E '\''test(=final_default_keyring_binary_rejects_unmigrated_system_get_and_set)'\'''`（new，直接拉起 `target/rb34-default-keyring/release/libra` 并核验 SHA-256，覆盖 AC3、AC4）

**Full-suite trigger:** `N/A`（`family child of REL-RB-02`；随 `RB-34c` 承接 `T-2` 全量双矩阵）

**Dependencies:** `RB-34`

**Deliverables:** `tests/storage_concurrency_stress_test.rs`（最终默认 + `keyring` 二进制未迁移 Global/System 4 路径拒收审计套件）

**Implementation write set:** `tests/storage_concurrency_stress_test.rs`

**Release write set:** `N/A`

**Files likely touched:** 1 个集成测试文件（生产代码 0 落点 / 0 文件；1 个审计产物文件 ≤ 5 满足 `G-11` `audit` `S` 上限）。

**Docs and compatibility impact:** `N/A`。

**Rollback mode:** `revert`

**Estimated scope:** `S`

**Version increment:** `N/A`

**Release boundary:** `family child of REL-RB-02`

**C/D coverage from:** `RB-34c`

**Granularity:** `type=audit; axis=final default+keyring global/system 4-path audit; recovery=revert audit test module; complete=yes; self-contained=yes; AC=4/20; VER=4/20; landing=0; prod-files=0; scope=S; deps=RB-34; writeset=序列化于 RB-34; release=family child; split-from=RB-34; exception=N/A`

---

### Task RB-34c: `REL-RB-02` 家族唯一原子发布点（Cargo 默认 Feature 切红库聚合验收与全量双矩阵发版）（Track B，自 `RB-34` 拆出）

**Task type:** `release`

**Lifecycle / Acceptance:** `pending` / 空

**Description:** 作为 `G-08` 家族 `REL-RB-02`（`RB-34` + `RB-34b`）的唯一原子发布点，本卡不引入任何新行为代码：核对 `Cargo.toml` 已切换为 `default = ["storage-redb", "storage-queue"]`，在独立 `--target-dir` 分别构建默认+`keyring` 二进制（`target/rb34-default-keyring/release/libra`）与纯 SQLite+`keyring` 回退二进制（`target/rb34-sqlite-keyring/release/libra`）并校验二者 SHA-256 相异，再针对 `target/rb34-default-keyring/release/libra` 统一重跑 `RB-34` 仓库门与 `RB-34b` 全部 4 类 Global/System 配置拒收审计（含 `config set --system` 零建 `config.redb`）、验证 `compat-offline-core` 显式 SQLite 锁定不失效，完成 `T-1 + T-2` 全量双矩阵测试与 `patch + 1` 发布。

**Out of scope:**
- 移除 `storage-sqlite` Feature 或 `sea-orm` 依赖：归延后项 `DEFER-RB-02`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RB-34`（Cargo 默认 Feature 切换）与 `RB-34b`（最终默认+`keyring` 二进制 Global/System 全 4 路径拒收审计）必须在独立 `--target-dir` 防覆盖下于同一原子发布点联合验收通过后方可对外发版 | `G-08`；`REL-RB-02`；`RB-34`；`RB-34b` |

**Acceptance criteria:**
- [ ] `Cargo.toml` 默认特性为 `default = ["storage-redb", "storage-queue"]`。
- [ ] 最终默认 + `keyring` 发布二进制构建于独立目标目录 `target/rb34-default-keyring` 且仓库 `init` 与未迁移 SQLite 仓库拒收 E2E 通过（`RB-34` 门）。
- [ ] 最终默认 + `keyring` 发布二进制（`target/rb34-default-keyring/release/libra`，SHA-256 与 SQLite 回退产物相异）对隔离未迁移 `GlobalConfig` 与 `SystemConfig` 的 `get`/`set` 全部 4 路径拒收审计通过且零建 `config.redb`（`RB-34b` 门）。
- [ ] 纯 SQLite + `keyring` 回退源码构建于独立目标目录 `target/rb34-sqlite-keyring` 通过（绝不覆写默认红库产物）。
- [ ] `compat-offline-core` 显式 `--no-default-features --features storage-sqlite` 锁定验证（`compat_matrix_alignment`）在 Cargo 默认 Feature 切换后仍全绿通过。（注：`ER-08` 版本发布与远端 `base.yml`/`codeql.yml`/`release.yml` 绿灯属统一发布基线，按 `G-03` 常规履约不重复占用本卡特有 AC 配额。）

**Verification:**
- [ ] `rg -q '^default = \["storage-redb", "storage-queue"\]' Cargo.toml`（覆盖 AC1）
- [ ] `cargo build --locked --release --target-dir target/rb34-default-keyring --features keyring --bin libra`（输出至独立目标目录 `target/rb34-default-keyring`，覆盖 AC2 构建）
- [ ] `cargo build --locked --release --target-dir target/rb34-sqlite-keyring --no-default-features --features storage-sqlite,keyring --bin libra`（输出至独立目标目录 `target/rb34-sqlite-keyring`，绝不覆写默认红库二进制，覆盖 AC4）
- [ ] `bash -euo pipefail -c 'bin="$PWD/target/rb34-default-keyring/release/libra"; sql_bin="$PWD/target/rb34-sqlite-keyring/release/libra"; test -x "$bin" && test -x "$sql_bin"; sha="$( (sha256sum "$bin" 2>/dev/null || shasum -a 256 "$bin") | awk "{print \$1}" )"; sql_sha="$( (sha256sum "$sql_bin" 2>/dev/null || shasum -a 256 "$sql_bin") | awk "{print \$1}" )"; test "$sha" != "$sql_sha"; source .env.test && LIBRA_DEFAULT_KEYRING_BIN="$bin" LIBRA_DEFAULT_KEYRING_SHA256="$sha" cargo nextest run --no-tests=fail --features keyring --test storage_concurrency_stress_test -E '\''test(=default_redb_keyring_binary_init_and_unmigrated_repo_fail_closed) | test(=final_default_keyring_binary_rejects_unmigrated_global_get_and_set) | test(=final_default_keyring_binary_rejects_unmigrated_system_get_and_set)'\'''`（先核验两独立目标目录成品存在且 SHA-256 相异，再直接拉起 `target/rb34-default-keyring/release/libra` 并核验 `LIBRA_DEFAULT_KEYRING_SHA256` 聚合验证 `RB-34` 与 `RB-34b`，覆盖 AC2、AC3）
- [ ] `source .env.test && cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite --test compat_matrix_alignment`（覆盖 AC5；并按 `ER-08` 登记远端 `base.yml`/`codeql.yml`/`release.yml` 绿灯 run ID）

**Full-suite trigger:** `T-1 + T-2: REL-RB-02 家族发布点（承载 Cargo.toml 默认 Feature 切换与全量双矩阵 ER-13 验收）`

**Dependencies:** `RB-34b`、`DEP-RB-02`

**Deliverables:** `N/A`

**Implementation write set:** `N/A`（本卡无生产源码改动）

**Release write set:** `Inherited`

**Files likely touched:** `Release write set`（版本面、`Cargo.lock`、`plan-status.md`、本文「实施证据汇总」；本仓生产代码 0 落点 / 0 文件）。

**Docs and compatibility impact:** 随同发布 `RB-34` 更新的 `COMPATIBILITY.md`。

**Rollback mode:** `forward-only`

**Migration and rollback:** 源码构建方若需纯 SQLite 回退可直接使用 `--no-default-features --features storage-sqlite,keyring`；全仓若需回滚可发布新 patch 将 `Cargo.toml` 切回 `default = ["storage-sqlite"]`。

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `family release point of REL-RB-02`

**C/D coverage from:** `self`

**Granularity:** `type=release; axis=integrated REL-RB-02 cargo default release point; recovery=patch cargo default back to sqlite; complete=yes; self-contained=yes; AC=5/12; VER=5/12; landing=0; prod-files=0; scope=S; deps=RB-34b,DEP-RB-02; writeset=序列化于 RB-34b; release=family release point; split-from=RB-34; exception=N/A`

---

## 测试矩阵

| 层级 | 覆盖对象 | 命令 / 特性组合 | 对应任务 |
|---|---|---|---|
| 单特性编译与契约 | 纯 SQLite 后端、未支持 env 覆盖拒绝与单特性回归 | `cargo nextest run --no-tests=fail --no-default-features --features storage-sqlite ...` | `RB-01`、`RB-01d`、`RB-01c`、`RB-03c`、`RB-04`、`RB-04b`、`RB-05`、`RB-05b`、`RB-10b`、`RB-10e`、`RB-10g`、`RB-11d`、`RB-11l`、`RB-11k`、`RB-11c`..`RB-31b`、`RB-33`、`RB-33c`、`RB-34`、`RB-34c` |
| 单特性 redb（自动蕴含队列） | `storage-redb` 隐式开启 `storage-queue` 编译与 Hook→queue→drain→`capture.redb` E2E、`MultiWriter`、`RepairAborted` 只读合同、`META_SCHEMA` 围栏、跨库意图重放、16B 双校验 CRC32C 帧头与逐字段拒收、全新空通道预建 34B `LTM2` `(0,0,0,0,[])` 双副本 `tail.meta`/`tail.meta.bak`（0 段四分法防唯一活跃段缺失伪装空通道、Case A/E/C/D、Subcase B1 单损恢复、非空 Subcase B2 双副本同损无条件 fail-closed、`LTM2.sealed[]` 未回收轮转段认证边界拦截完整帧边界 `CleanEof` 截断、先更新 `LTM2` 再 `unlink` 崩溃安全回收）、全新 Redb 仓 Spool 墓碑、共享锁容量边界、`writer.lock → append.lock` 暂停生产者与真崩溃零跳帧原子高水位恢复实测、双通道高水位、146 文件接线 | `cargo nextest run --no-tests=fail --no-default-features --features storage-redb ...`（及幂等 `--features storage-redb,storage-queue`） | `RB-01`..`RB-09l`、`RB-12`..`RB-31b` |
| 双特性等价与双向迁移 | `Sqlite*Store` ↔ `Redb*Store` 全表（61+25+3 源表）等价、配置域有界 `config-migration.lock` + 先非覆写 `hard_link` 墓碑预占防旧版二进制竞态 + `init_phase="fresh_create"` 全边界恢复、WAL 静默与零额外 fd `VACUUM INTO` 锁安全备份、SQLite 回滚日志原子围栏 `__libra_migrated_fence__` + 逐语句预打开连接拒访 + 断电热回滚日志 + 双源/双向 `unlink` 后持久化恢复、反向全程持 `capture-spool-consumer.lock` 排斥并发 `drain` 与 `quarantine/` 非空 fail-closed、反向正常与恢复统一非覆写 `hard_link` 先发 `capture.db`（覆盖 `Ok`/`NotFound`/`AlreadyExists`/`Unsupported` 与无崩溃竞争并轨）、反向配置 `targets_staged_for_rename` 旧写者保护、全新 Redb 仓活 spool 对账与两类可达 Step 2.6 解封恢复 | `cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue ...` | `RB-01`..`RB-06e`、`RB-10`..`RB-12`、`RB-20`、`RB-20c`、`RB-29`、`RB-29d`、`RB-29a`、`RB-29c`、`RB-30`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c` |
| 官方精确暂存成品协同 + 最终默认源码构建 E2E | 按 `release.yml` 各自实参独立构建并经 `mktemp -d` 暂存+`.sha256` 校验后的双后端 `libra`（含 `keyring`）与 `libra-migrate`（**无 `keyring`**）成品互操作（`RB-32`/`RB-32b` 切前 + `REL-RB-03` `RB-33`/`RB-33b`/`RB-33c` 切后全新 Redb 仓 Spool 墓碑/活事件对账与仓库/配置 SQLite 回退 E2E）；以及 `REL-RB-02`（`RB-34`/`RB-34b`/`RB-34c`）独立 `--target-dir` 最终默认 `--features keyring` 二进制仓库 `init`/拒收及全 4 类 Global/System 配置（含 `config set --system`）fail-closed 拒收审计 | `LIBRA_PACKAGED_BIN=... LIBRA_MIGRATE_PACKAGED_BIN=... cargo nextest run --no-tests=fail --features storage-sqlite,storage-redb,storage-queue ...`、`LIBRA_DEFAULT_KEYRING_BIN=... cargo nextest run --no-tests=fail --features keyring ...` | `RB-31`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c`、`RB-34`、`RB-34b`、`RB-34c` |
| 32 进程跨进程压测 | 32 双通道入队 + 4 Git 只读 + 2 Git CAS 写 + 4 Capture 只读 | `cargo nextest run --no-tests=fail --no-default-features --features storage-redb --test storage_concurrency_stress_test` | `RB-12`、`RB-30`、`RB-31` |
| 全量双矩阵 CI（`ER-13`）与基线零改守卫 | `compat-offline-core`（`RB-12`/`RB-29c` 精确作用域 `diff -u` 零修改 → `RB-31` 显式 `--no-default-features --features storage-sqlite`）+ `compat-redb-storage`（`RB-12` 锁定态 → `RB-29c` 解锁态）→ `compat-redb-queue`（`--no-default-features --features storage-redb,storage-queue`）+ `keyring` 双构型（`RB-31b`）+ `--all-features` Clippy/Rustdoc | `.github/workflows/base.yml`、`.github/workflows/release.yml` | `RB-12`、`RB-29c`、`RB-30`、`RB-31`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c`、`RB-34`、`RB-34b`、`RB-34c` |

## 追溯表

| 缺口 / 决策 / 约束 | 覆盖任务卡 | 核心验证门 |
|---|---|---|
| `GAP-RB-01` / `ADR-RB-01` / `ADR-RB-04` | `RB-01`、`RB-01d`、`RB-01b`、`RB-01c`、`RB-02c`、`RB-02e`、`RB-03`..`RB-06e`、`RB-11c`、`RB-11t`、`RB-13`..`RB-29b`（含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`/`RB-29d`/`RB-29a`/`RB-29c`）、`RB-30`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c`、`RB-34`、`RB-34b`、`RB-34c` | `dual_feature_resolution_and_pre_release_cli_gate`、`manifest_first_precedence_blocks_conflicting_env_overrides`、`redb_only_feature_implies_queue_hook_to_capture_e2e`、各双后端等价门、146 文件分区接线门、`syn_ast_per_callsite_storage_wiring_completeness_guard`、`packaged_release_artifacts_default_redb_cutover_and_sqlite_rollback`、`default_redb_keyring_binary_init_and_unmigrated_repo_fail_closed`、`final_default_keyring_binary_rejects_unmigrated_global_get_and_set`、`final_default_keyring_binary_rejects_unmigrated_system_get_and_set` |
| `GAP-RB-02` / `ADR-RB-03` | `RB-02`、`RB-02d`、`RB-02b`、`RB-04`、`RB-06`、`RB-12`、`RB-17`、`RB-17b`、`RB-18`、`RB-18b`、`RB-26`、`RB-26b`、`RB-27` | `multiwriter_concurrent_open_and_bounded_write_timeout`、`readonly_repair_aborted_ro_fs_and_concurrent_explicit_repair`、`meta_schema_role_future_version_and_migration_tombstone_fence`、`storage_concurrency_stress_test` |
| `GAP-RB-03` / `ADR-RB-05` / `GC-RB-03` | `RB-07`、`RB-07g`、`RB-07c`、`RB-07f`、`RB-07d`、`RB-07b`、`RB-07e`、`RB-08`、`RB-08t`、`RB-08d`、`RB-08u`、`RB-08f`、`RB-08b`、`RB-08g`、`RB-08m`、`RB-08n`、`RB-08i`、`RB-08q`、`RB-08k`、`RB-08v`、`RB-08j`、`RB-08r`、`RB-08l`、`RB-08o`、`RB-08s`、`RB-08c`、`RB-08y`、`RB-08z`、`RB-08e`、`RB-08h`、`RB-08x`、`RB-09`、`RB-09b`、`RB-09d`、`RB-09c`、`RB-09e`、`RB-09f`、`RB-09g`、`RB-09h`、`RB-09j`、`RB-09k`、`RB-09i`、`RB-09l`、`RB-29`、`RB-29d` | `crc32c_rfc3720_vector_and_16b_dual_crc_roundtrip`、`frame_encode_limits_and_min_max_body_len_guard`、`acknowledged_frame_each_header_field_bitflip_rejected_pre_alloc`、`seq_decoder_distinguishes_clean_eof_and_truncated_tail_variants`、`acknowledged_frame_each_body_and_crc_field_bitflip_rejected`、`dual_lane_directory_permissions_nofollow_owner_and_initial_meta`、`fresh_redb_init_spool_tombstones_blocks_legacy_ensure_spool`、`case_c_same_segment_crash_retains_synced_frames`、`publish_quarantine_durable_syncs_renames_fsyncs_before_destructive_ops`、`publish_quarantine_durable_crash_before_rename_and_after_dir_fsync`、`case_d_old_seg_prefix_guard_and_valid_successor_recovery`、`case_d_valid_successor_trailing_half_frame_durable_quarantine_ordering`、`case_d_valid_successor_trailing_half_frame_crash_boundaries`、`case_d_empty_and_half_frame_orphan_cleanup_quarantine`、`rotation_16mib_creates_0600_fsync_and_records_ltm2_sealed`、`case_c_step_c1_quarantines_truncates_syncs_commits_ltm2_and_preserves_s`、`case_c_step_c2_seals_rotates_resets_ltm2_and_appends_s1`、`case_c_recovery_crash_boundaries_1_2_3_converge`、`case_c_zero_byte_first_frame_crash_truncates_without_sealing`、`nested_case_c_to_case_d_recovers_old_tail_and_seals_accurate_boundary`、`nested_case_c_to_case_d_old_seg_damaged_tail_durable_quarantine_and_seal`、`nested_case_c_to_case_d_old_seg_bad_tail_at_committed_boundary_quarantine_and_seal`、`sealed_cap_256_to_257_normal_rotation_backpressure_and_slot_release`、`sealed_cap_256_case_d_and_ltm2_decoder_bounds_rejected`、`sealed_cap_256_case_c_skips_step_c2_and_commits_inplace_boundary`、`sealed_cap_256_post_recovery_insegment_enqueue_admitted`、`sealed_cap_256_single_call_recovery_then_prewrite_reject_and_reopen`、`unchanged_meta_case_a_bitflip_and_case_e_short_segment_rejected`、`case_c_prefix_bitflip_rejected_zero_mutation_before_tail_scan`、`subcase_b1_single_replica_loss_phase1_reject_and_phase2_mirror_repair`、`subcase_b1_backup_replica_loss_phase1_reject_and_phase2_mirror_repair`、`virgin_and_prebootstrapped_zero_segment_lane_init`、`zero_segment_with_committed_or_corrupt_meta_fails_closed`、`dual_metadata_loss_complete_frame_boundary_and_zero_byte_active_fails_closed`、`dual_metadata_loss_max_and_older_segment_corrupt_or_truncated_fails_closed`、`cross_lane_quarantine_tmp_cleanup_scoped_to_classified_lane_only`、`consumer_rejects_committed_prefix_dual_meta_loss_and_hw_regression`、`consumer_rejects_rotated_seg_complete_frame_truncation_bitflip_and_missing`、`reclamation_updates_ltm2_before_unlink_and_cleans_crash_orphan`、`consumer_waits_for_append_lock_and_does_not_quarantine_paused_producer`、`consumer_segment_reclamation_coordinates_with_append_lock`、`consumer_tail_recovery_delegates_case_c_without_advancing_hw`、`consumer_pure_synced_tail_recovery_zero_quarantine_and_no_db_write_under_lock`、`consumer_sealed_seg_multibatch_recovery_backlog_tail_and_final_hw_advance`、`consumer_clean_tail_recovery_multibatch_keeps_active_hw_and_subsequent_append`、`consumer_full_256_table_recovery_multibatch_keeps_active_hw_and_subsequent_append`、`consumer_zero_byte_recovery_keeps_active_hw_and_subsequent_append`、`consumer_drained_active_then_rotated_16mib_and_case_c_empty_advance`、`consumer_drained_rotated_bitflip_truncation_missing_and_oob_hw_rejected`、`consumer_drained_rotated_empty_advance_crash_before_and_after_commit`、`consumer_drained_rotated_s1_existing_and_appended_drain`、`consumer_drained_rotated_old_s_safe_reclaim_ordered_actions`、`consumer_delegates_nested_case_d_recovery_and_drains_all_frame_classes`、`consumer_eager_reclaim_unblocks_256_rotation`、`consumer_drains_full_256_table_case_c_sealed_and_active`、`priority_lane_drains_before_normal_with_contiguous_highwater`、`single_writer_lock_contention_and_active_probe`、`redb_only_feature_implies_queue_hook_to_capture_e2e`、`redb_daemon_drain_priority_and_doctor_queue_telemetry` |
| `GAP-RB-04` / `ADR-RB-07` / `GC-RB-02` | `RB-02c`、`RB-02e`、`RB-03b`、`RB-03h`、`RB-03d`、`RB-03f`、`RB-03e`、`RB-03i`、`RB-10`..`RB-10n`、`RB-11`..`RB-11z`、`RB-11c`、`RB-11t`、`RB-12`、`RB-29a`、`RB-29c`、`RB-30`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c` | `forward_phase1_rejects_writer_and_delete_active_reader`、`forward_phase1_rejects_wal_active_reader_writer_and_idle_conn`、`forward_phase1_rejects_inflight_spool_tmp_and_queue_append_lock`、`forward_phase1_offline_wal_checkpoint_flushes_to_standalone_main_db`、`forward_phase1_lock_safe_vacuum_backup_and_inplace_spool_backup`、`forward_phase1_external_os_process_blocked_through_backup_hash`、`forward_phase2_journaled_fence_and_post_unlink_progress`、`forward_phase2_external_os_exclusivity_and_fresh_legacy_reject`、`forward_phase2_fence_blocks_cold_warm_table_select_and_sqlite_master`、`forward_phase2_fence_blocks_open_handle_dml_ddl_and_vacuum`、`forward_phase2_unmoved_lock_inplace_spool_tombstones_block_concurrent_hooks`、`forward_phase2_powerloss_torn_sector_hot_journal_rollback_and_resume`、`forward_phase2_crash_immediately_after_libra_db_unlink`、`forward_phase2_crash_immediately_after_capture_db_unlink`、`forward_phase2_unexpected_missing_pending_and_corrupt_non_sentinel_reject`、`forward_phase2_spool_tombstone_gap_crash_and_concurrent_hook_reconcile`、`reverse_phase1_quiescence_rejects_redb_writers_and_enforces_lock_order`、`reverse_phase1_rejects_queue_producer_and_inflight_spool_tmp`、`reverse_phase1_consumer_lock_excludes_concurrent_drain_and_rejects_quarantine`、`reverse_phase1_replays_real_half_committed_intent_deadlock_free`、`reverse_phase1_tmp_integrity_and_verified_redb_queue_backup`、`reverse_phase2_step24_hardlink_capture_then_libra_and_unsupported_reject`、`reverse_phase2_normal_step24_hardlink_reconciles_no_crash_competing_capture_open`、`reverse_phase2_step25_26_finalize_unfence_and_legacy_hook_resume`、`reverse_phase2_crash_before_fence_drift_and_after_git_redb_unlink`、`reverse_phase2_crash_immediately_after_capture_redb_unlink`、`reverse_phase2_staged_hardlink_already_exists_notfound_and_both_published`、`reverse_phase2_competing_capture_store_open_reconciles_and_fences_zero_loss`、`reverse_fresh_redb_reconciles_preexisting_live_spool_and_establishes_tombstones`、`reverse_fresh_redb_live_spool_reconcile_blocks_concurrent_drain_moves`、`reverse_phase2_step26_partial_cleanup_blocks_old_hook_then_resumes`、`reverse_phase2_step26_post_unfence_crash_preserves_live_old_hook_event`、`gc13_global_config_forward_migration_tombstone_and_legacy_block`、`gc13_system_config_forward_migration_tombstone_and_legacy_block`、`config_migration_bounded_lock_contention_global_and_system_fail_closed`、`config_forward_crash_pending_missing_and_backup_fault_reject`、`config_reverse_crash_after_config_redb_unlink_global_and_system`、`config_reverse_staged_rename_preserves_old_binary_writes_global_and_system`、`cli_migrate_gate_rejects_when_locked_even_with_libra_test_env`、`pre_unlock_libra_migrate_binary_rejects_cutover_with_libra_test_env`、`post_unlock_libra_migrate_binary_bidirectional_roundtrip`、`packaged_release_dual_backend_libra_and_migrate_coherence_pre_cutover` |
| `GAP-RB-05` / `ADR-RB-06` / `ADR-RB-08` / `GC-RB-04`..`06` | `RB-06b`、`RB-06e`、`RB-09c`、`RB-09e`、`RB-09f`、`RB-09g`、`RB-09h`、`RB-09j`、`RB-09k`、`RB-09i`、`RB-09l`、`RB-12`、`RB-25`、`RB-25b`、`RB-28`、`RB-28b`、`RB-29a`、`RB-29c`、`RB-30`、`RB-31`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c`、`RB-34`、`RB-34b`、`RB-34c` | `partial_commit_fault_blocks_snapshot_and_subsequent_writer_replays_first`、`redb_partial_commit_fails_closed_then_recovers_after_doctor_repair`、`redb_bridge_atomic_head_publication_and_index_two_phase`、`redb_generation_dir_pinned_against_future_and_concurrent_migration`、`auth_keyring_backend`、`base.yml` 精确 `compat-offline-core` `diff -u` / `release.yml` |
| `ADR-RB-02`（发现/沙箱未来代际/配置有界锁、先 `hard_link` 墓碑预占、首次建库状态机与运行时分派） | `RB-02c`、`RB-02e`、`RB-03j`、`RB-03b`、`RB-03h`、`RB-03d`、`RB-03f`、`RB-03e`、`RB-03i`、`RB-03c`、`RB-03g`、`RB-07e`、`RB-11b`、`RB-11v`、`RB-11k`、`RB-11w`、`RB-11f`、`RB-11n`、`RB-11g`、`RB-11o`、`RB-13`、`RB-16`、`RB-16b`、`RB-17`、`RB-20`、`RB-20c`、`RB-25b`、`RB-33`、`RB-33b`、`RB-33c`、`RB-34`、`RB-34b`、`RB-34c` | `redb_repo_discovery_and_home_rejection`、`config_scope_manifest_pending_dual_file_and_tombstone_acceptance`、`config_scope_bounded_lock_shared_concurrency_and_timeout_error`、`global_config_hardlink_reserve_and_unsupported_fs_reject`、`system_config_hardlink_reserve_and_concurrent_first_write`、`global_config_fresh_create_crash_boundaries_1_2_3`、`system_config_fresh_create_crash_boundaries_1_2_3_and_modes`、`dual_feature_sqlite_runtime_config_dispatch_and_authority_guard`、`redb_only_legacy_config_db_read_and_write_fail_closed`、`dual_feature_global_system_config_cli_runtime_dispatch`、`redb_generation_dir_pinned_against_future_and_concurrent_migration`、`default_redb_keyring_binary_init_and_unmigrated_repo_fail_closed`、`final_default_keyring_binary_rejects_unmigrated_global_get_and_set`、`final_default_keyring_binary_rejects_unmigrated_system_get_and_set` |

## 里程碑验收与回滚

| 里程碑 | 覆盖任务 | 退出验收判据 | 回滚策略 |
|---|---|---|---|
| **M1（Track A 闭环，114 卡）** | `RB-01`..`RB-12`（含全部 `G-09` 拆分子卡共 114 卡，其中 `RB-01d`..`RB-11o` 连续 110 卡独占 `src/storage/**`） | `storage-redb` 自动蕴含 `storage-queue` + `src/storage/**` 全表双后端（61+25+3）+ 配置域有界 `config-migration.lock` / 先非覆写 `hard_link` 墓碑预占 / `init_phase="fresh_create"` 全边界恢复 / 清单分派 + 跨库持久化意图重放 + 16B 自认证 `HEADER_CRC32C + FRAME_CRC32C` 逐字段拒收、`LTM2` 双副本 `tail.meta`/`tail.meta.bak`（0 段四分法防唯一活跃段缺失伪装空通道、Case A/E/C/D、Subcase B1 单副本恢复、非空 Subcase B2 无条件 fail-closed、`LTM2.sealed[]` 认证全部未回收轮转段长度与 CRC32C 拦截完整帧边界 `CleanEof` 截断、先更新 `LTM2` 再 `unlink` 崩溃安全回收）及 `writer.lock → capture-queue/append.lock` 暂停生产者协调与真崩溃零跳帧原子高水位恢复双通道队列 + 全新 Redb 仓 Spool 墓碑 + WAL 静默门、无额外 fd `VACUUM INTO` 锁安全备份、SQLite 回滚日志原子围栏与逐语句预打开连接拒访、反向全程持 `capture-spool-consumer.lock` 排斥并发 `drain` 与 `quarantine/` 非空 fail-closed、正常与恢复统一非覆写 `hard_link` 先发 `capture.db`、双源双向 post-unlink/反向配置 `targets_staged_for_rename` 旧写者保护与全新 Redb 仓/Step 2.6 解封恢复双向迁移器 `libra-migrate`（编译期 `REDB_RUNTIME_UNLOCKED = false` 全 4 标志不可旁路门控）+ `compat-redb-storage` 6 步 32 进程压测 CI 与 `compat-offline-core` 零修改精确 `diff -u` 全绿；默认 CLI 行为与其它业务目录零受扰 | 任意卡按 `immutable-release` 前滚修补或放弃隔离分支，不影响现网用户 |
| **M2（`REL-RB-01` 家族原子上线，30 卡）** | `RB-13`..`RB-30`（含 `RB-16b`/`RB-17b`/`RB-18b`/`RB-20c`/`RB-20b`/`RB-25b`/`RB-26b`/`RB-28b`/`RB-29d`/`RB-29a`/`RB-29c`/`RB-29b`） | 146 生产接线文件 + 后端感知队列/纯 `--features storage-redb` 捕获队列 E2E/守护 + CLI/`libra-migrate` 联合解锁（`RB-29a`）与解锁态测试/`base.yml` 原子切换（`RB-29c`，含 `compat-offline-core` 精确作用域 `diff -u` 零修改）在隔离分支汇流，经 `RB-29b`/`RB-30` `syn 2.0` 逐调用点 AST 守卫与负向自测、`init.rs:1-20` `diff -u` 门、全量双后端 `T-2`、远端解锁态 `base.yml` 与满足 `DEP-RB-03` 后的 `agent.en.md` 网站同步原子上线；默认仍为 `storage-sqlite` 且 `libra config set --global` 行为零破坏 | 推送前可放弃/修正 `plan/20261121-redb-integration`；推送后默认仍是 SQLite，不波及未显式选择 `redb` 的用户 |
| **M3（全量双矩阵 + 双二进制精确成品发行与 `no-release` 审计 + `REL-RB-03` 运行时默认切红库家族 + `REL-RB-02` Cargo 默认切换家族，10 卡）** | `RB-31`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c`、`RB-34`、`RB-34b`、`RB-34c` | `compat-offline-core` 显式锁定 SQLite、`compat-redb-queue`（`RB-31`）与纯红库/官方双后端 `keyring` CI（`RB-31b`）全绿；`release.yml` 分别按真实特性构建并经独立 `mktemp -d` 暂存双后端 `libra`（含 `keyring`）与 `libra-migrate`（不含 `keyring`）（`RB-32`），完成 `no-release` 切前互操作与 SQLite 默认不变只读审计（`RB-32b`）；`REL-RB-03` 家族（`RB-33`→`RB-33b`→`RB-33c`）切换官方发行运行时默认后端至 `Redb`、完成切后精确暂存双成品仓库/配置/全新仓活 Spool 双向 SQLite 回退与 Step 2.6 解封审计及仓内/网站同步后原子发布；`REL-RB-02` 家族（`RB-34`→`RB-34b`→`RB-34c`）切换 `Cargo.toml` 默认 Feature 至 `storage-redb,storage-queue`，在独立 `--target-dir`（`target/rb34-default-keyring` vs `target/rb34-sqlite-keyring`，SHA-256 相异）防覆盖下于最终默认+`keyring` 二进制上实测仓库初始化/拒收以及 `config get/set --global` 与 `config get/set --system` 全 4 路径拒收（零建 `config.redb`）和纯 SQLite 回退构建后原子发布 | `forward-only`：官方发行 `libra` 仍内建双后端可直接打开 `libra-migrate --to sqlite` 回退仓库/配置，并可发布 patch 切回 `backend.rs` 与 `Cargo.toml` SQLite 默认 |

## 故障恢复矩阵

| 故障场景 | 检测信号 | 恢复动作 | 数据不变量 | 覆盖测试 |
|---|---|---|---|---|
| 生产者写帧、轮转新段或更新 `LTM2` 双副本 `tail.meta`/`tail.meta.bak` 中途崩溃 / 0 段含非零或损坏元数据（唯一活跃段缺失）/ 正常入队前已确认前缀位翻转 / 活跃段短于 `committed_len` / 未消费轮转段精确停在完整帧边界截断 / 单副本损毁 / 非空目录双副本同损 / 消费者与活跃生产者竞态 | 全新空通道先预建 34B `LTM2` `(0,0,0,0,[])` 双副本；0 段四分法联合核验元数据存在性与值域；16B 自认证帧头在分配前校验 `MAGIC`/`HEADER_CRC32C`/`FRAME_BODY_LEN`；共享 `append.lock` 内 Case A/C/D 强制先验 `[0, committed_len)` `prefix_crc32c`，Case E 拦截缺失与短段，Subcase B2 对非空双副本同损无条件拦截；消费端逐段核验 `LTM2.sealed[]` `(sealed_len, sealed_crc32c)` | 有效边界确立且前缀完好时按 Case C 100% 保留 `committed_len` 后完整同步帧至 `recovered_valid_end`，仅隔离 `[recovered_valid_end, actual_len)` 至 `quarantine/` 并封存切段（**消费端恢复绝不提前推进 `QUEUE_HIGHWATER_*`，所有积压与同步尾帧随 `CaptureStore::append_capture_batch` 事务原子落库推进高水位**）；回收段先原子推进 `LTM2` `MIN_LIVE_SEG` 再 `unlink`；**情况 0c/0d、Case A/C/D/E、Subcase B1 前缀损毁、非空 Subcase B2 双损及轮转段 `LTM2.sealed[]` 长度/CRC 失配（含完整帧边界 `CleanEof` 截断）全部无条件 fail-closed 返回 `CorruptCommittedFrame`（`LBR-IO-001`）磁盘零改动** | 已确认帧与完整同步尾帧零丢失、零静默截断穿透、零超大分配、零与活跃生产者竞态误隔离、零高水位提前跳段 | `acknowledged_frame_each_header_field_bitflip_rejected_pre_alloc`（`RB-07c`）；`seq_decoder_distinguishes_clean_eof_and_truncated_tail_variants`（`RB-07f`）；`acknowledged_frame_each_body_and_crc_field_bitflip_rejected`（`RB-07d`）；`dual_lane_directory_permissions_nofollow_owner_and_initial_meta`（`RB-07b`）；`case_c_same_segment_crash_retains_synced_frames`（`RB-08`）；`publish_quarantine_durable_syncs_renames_fsyncs_before_destructive_ops`、`publish_quarantine_durable_crash_before_rename_and_after_dir_fsync`（`RB-08t`）；`case_d_old_seg_prefix_guard_and_valid_successor_recovery`（`RB-08d`）；`case_d_valid_successor_trailing_half_frame_durable_quarantine_ordering`、`case_d_valid_successor_trailing_half_frame_crash_boundaries`（`RB-08u`）；`case_d_empty_and_half_frame_orphan_cleanup_quarantine`（`RB-08f`）；`rotation_16mib_creates_0600_fsync_and_records_ltm2_sealed`（`RB-08g`）；`case_c_step_c1_quarantines_truncates_syncs_commits_ltm2_and_preserves_s`（`RB-08m`）；`case_c_step_c2_seals_rotates_resets_ltm2_and_appends_s1`（`RB-08n`）；`unchanged_meta_case_a_bitflip_and_case_e_short_segment_rejected`、`case_c_prefix_bitflip_rejected_zero_mutation_before_tail_scan`（`RB-08c`）；`subcase_b1_single_replica_loss_phase1_reject_and_phase2_mirror_repair`（`RB-08y`）；`subcase_b1_backup_replica_loss_phase1_reject_and_phase2_mirror_repair`（`RB-08z`）；`virgin_and_prebootstrapped_zero_segment_lane_init`、`zero_segment_with_committed_or_corrupt_meta_fails_closed`（`RB-08e`）；`dual_metadata_loss_complete_frame_boundary_and_zero_byte_active_fails_closed`、`dual_metadata_loss_max_and_older_segment_corrupt_or_truncated_fails_closed`（`RB-08h`）；`cross_lane_quarantine_tmp_cleanup_scoped_to_classified_lane_only`（`RB-08x`）；`consumer_rejects_committed_prefix_dual_meta_loss_and_hw_regression`（`RB-09b`）；`consumer_rejects_rotated_seg_complete_frame_truncation_bitflip_and_missing`、`reclamation_updates_ltm2_before_unlink_and_cleans_crash_orphan`（`RB-09d`）；`consumer_waits_for_append_lock_and_does_not_quarantine_paused_producer`、`consumer_segment_reclamation_coordinates_with_append_lock`（`RB-09c`）；`case_c_recovery_crash_boundaries_1_2_3_converge`（`RB-08i`）；`case_c_zero_byte_first_frame_crash_truncates_without_sealing`（`RB-08q`）；`nested_case_c_to_case_d_recovers_old_tail_and_seals_accurate_boundary`（`RB-08k`）；`nested_case_c_to_case_d_old_seg_damaged_tail_durable_quarantine_and_seal`、`nested_case_c_to_case_d_old_seg_bad_tail_at_committed_boundary_quarantine_and_seal`（`RB-08v`）；`sealed_cap_256_to_257_normal_rotation_backpressure_and_slot_release`（`RB-08j`）；`sealed_cap_256_case_d_and_ltm2_decoder_bounds_rejected`（`RB-08r`）；`sealed_cap_256_case_c_skips_step_c2_and_commits_inplace_boundary`（`RB-08l`）；`sealed_cap_256_post_recovery_insegment_enqueue_admitted`（`RB-08o`）；`sealed_cap_256_single_call_recovery_then_prewrite_reject_and_reopen`（`RB-08s`）；`consumer_tail_recovery_delegates_case_c_without_advancing_hw`、`consumer_pure_synced_tail_recovery_zero_quarantine_and_no_db_write_under_lock`（`RB-09e`）；`consumer_sealed_seg_multibatch_recovery_backlog_tail_and_final_hw_advance`（`RB-09f`）；`consumer_clean_tail_recovery_multibatch_keeps_active_hw_and_subsequent_append`、`consumer_full_256_table_recovery_multibatch_keeps_active_hw_and_subsequent_append`、`consumer_zero_byte_recovery_keeps_active_hw_and_subsequent_append`（`RB-09g`）；`consumer_drained_active_then_rotated_16mib_and_case_c_empty_advance`、`consumer_drained_rotated_bitflip_truncation_missing_and_oob_hw_rejected`（`RB-09h`）；`consumer_drained_rotated_empty_advance_crash_before_and_after_commit`、`consumer_drained_rotated_s1_existing_and_appended_drain`（`RB-09j`）；`consumer_drained_rotated_old_s_safe_reclaim_ordered_actions`（`RB-09k`）；`consumer_delegates_nested_case_d_recovery_and_drains_all_frame_classes`（`RB-09i`）；`consumer_eager_reclaim_unblocks_256_rotation`、`consumer_drains_full_256_table_case_c_sealed_and_active`（`RB-09l`） |
| `priority/` 与 `normal/` 并发逼近 `256 MiB` 容量上限 | 共享目录锁 `capture-queue/append.lock` 内检验 `priority + normal + quarantine + frame_buf.len() > 256 MiB` | 至多放行不超限的帧，其余立即 fail-closed 返回 `LBR-AGENT-046` 且零字节写入 | 双通道总占用绝不越过 `256 MiB` 1 字节 | `cross_lane_capacity_boundary_race_and_single_frame_cap`（`RB-08b`） |
| 消费事务提交成功、外部 `highwater` 镜像更新前崩溃 | 重启后以库内 `QUEUE_HIGHWATER_{PRIORITY,NORMAL}` 为权威 | 从库内权威高水位续读，叠加完整语义 `dedup_key` 幂等去重，重写外部镜像 | 零丢帧、零重复业务行 | `crash_after_commit_before_mirror_zero_dup`（`RB-09b`）、`consumer_sealed_seg_multibatch_recovery_backlog_tail_and_final_hw_advance`（`RB-09f`）、`consumer_clean_tail_recovery_multibatch_keeps_active_hw_and_subsequent_append`（`RB-09g`）、`consumer_drained_rotated_empty_advance_crash_before_and_after_commit`（`RB-09j`） |
| 跨库提交在 `git.redb` 提交后、`capture.redb` 提交前崩溃 | `git.META_GENERATION > capture.META_GENERATION` 且 `META_CROSS_STORE_INTENT` 存未对账意图 | `cloud snapshot` fail-closed 引导 `agent doctor --repair`；任何后续跨库写者**强制先重放待决意图至 `capture.redb`** 才允许递增代际 | 绝不以 `max+1` 静默掩盖丢失捕获；修复后两库代际精确对齐 | `partial_commit_fault_blocks_snapshot_and_subsequent_writer_replays_first`（`RB-06b`）、`redb_partial_commit_fails_closed_then_recovers_after_doctor_repair`（`RB-28`） |
| 只读命令或 GUI 打开未完成恢复的崩溃 `.redb` | `redb::Database::open_read_only` 返回 `DatabaseError::RepairAborted` | 只读上下文严格 fail-closed（CLI 报 `LBR-IO-001`、GUI 映 `SourceState::Corrupt`），提示运行 `libra fsck` / `libra agent doctor --repair` | 只读命令绝不修改磁盘文件 | `readonly_repair_aborted_ro_fs_and_concurrent_explicit_repair`（`RB-02d`）、`redb_window_readonly_context_and_source_state_mapping`（`RB-27`） |
| 正向迁移前存在外部 SQLite 写者（含 WAL 活跃写者）、活跃 `DELETE`/WAL 读者、已打开 WAL 连接、在途旧 Hook（`spool.rs:119-145`）或外部生产者占用 `capture-queue/append.lock`，或 Phase 1 备份/哈希期间外部进程企图乘隙读写 | `locking_mode=EXCLUSIVE` 下直接执行 `PRAGMA journal_mode=DELETE`（绝不先调 `wal_checkpoint(TRUNCATE)`）或无写 `BEGIN EXCLUSIVE` 报 `SQLITE_BUSY`，或 `capture-spool/tmp/` 非空，或 `capture-queue/append.lock` 争用失败；单连接 `c_mig` 全程零额外 `open()`/`close()`，原生 `VACUUM INTO` 仅触碰备份新 inode | Phase 1 立即 fail-closed 返回 `LBR-IO-001` 并释放锁，且源 `.db`/`-wal`/`-shm`、`capture-spool/` 与 `capture-queue/` 100% 逐字节零改动；离线 WAL 经 `PRAGMA journal_mode=DELETE` 内部独占检查点无损归并消除 `-wal`/`-shm` 后经单连接 `c_mig` 完成流式导出与 `VACUUM INTO` 备份 | 活跃读者/写者/在途 spool/队列锁争用零迁移零改动；Phase 1 至 `unlink` 全程独立 OS 子进程零乘隙写入 | `forward_phase1_rejects_writer_and_delete_active_reader`（`RB-10`）；`forward_phase1_rejects_wal_active_reader_writer_and_idle_conn`（`RB-10m`）；`forward_phase1_rejects_inflight_spool_tmp_and_queue_append_lock`（`RB-10n`）；`forward_phase1_lock_safe_vacuum_backup_and_inplace_spool_backup`、`forward_phase1_external_os_process_blocked_through_backup_hash`（`RB-10f`） |
| 正向迁移断电撕裂扇区、围栏前/后崩溃、紧接 `unlink(libra.db)` 或 `unlink(capture.db)` 后崩溃、意外缺失、或 Spool/Queue 间隙崩溃 | 清单前置拦截 `LIBRA_STORAGE_BACKEND`；4 态判定结合 `phase2_progress`（`pending`/`fenced_ready_to_unlink`/`unlinked`）、热回滚日志自动回滚、`__libra_migrated_fence__` 哨兵核验与 `backup_sha256` 校验 | 热日志自动无损回滚；合法已摘除态校验备份无损续跑；`pending` 态双源意外缺失或页头/非哨兵损坏立即 fail-closed `LBR-IO-001`；spool/queue 漂移按备份∪活目录去重重导至 `gen+1` 再装齐原地 `0400` 墓碑 | 抗非原子扇区写断电；已打开旧连接全部 8 类 SQL 语句（冷/热表 `SELECT`/`sqlite_master`/`INSERT`/`UPDATE`/`DELETE`/`CREATE TABLE IF NOT EXISTS`/`VACUUM`）均报 `SQLITE_CORRUPT` fail-closed；零丢写 | `forward_phase2_journaled_fence_and_post_unlink_progress`（`RB-10b`）；`forward_phase2_external_os_exclusivity_and_fresh_legacy_reject`（`RB-10l`）；`forward_phase2_fence_blocks_cold_warm_table_select_and_sqlite_master`、`forward_phase2_fence_blocks_open_handle_dml_ddl_and_vacuum`（`RB-10e`）；`forward_phase2_unmoved_lock_inplace_spool_tombstones_block_concurrent_hooks`（`RB-10g`）；`forward_phase2_powerloss_torn_sector_hot_journal_rollback_and_resume`、`forward_phase2_crash_before_fence_drift_reexports_gen_plus_1`（`RB-10c`）；`forward_phase2_crash_after_fence_before_unlink`、`forward_phase2_crash_immediately_after_libra_db_unlink`、`forward_phase2_crash_immediately_after_capture_db_unlink`（`RB-10k`）；`forward_phase2_unexpected_missing_pending_and_corrupt_non_sentinel_reject`（`RB-10d`）；`forward_phase2_spool_tombstone_gap_crash_and_concurrent_hook_reconcile`（`RB-10h`） |
| 反向迁移遇并发 `drain` 消费者、非空 `quarantine/`、无崩溃或崩溃竞争 `capture::store::open`、`unlink(git.redb)`/`unlink(capture.redb)`、queue 间隙、全新 Redb 仓活 spool，或两类可达 Step 2.6 解封窗口崩溃 | 严格锁序持 `capture-spool-consumer.lock` 排斥 `drain.rs:87`；Phase 1 非空 `quarantine/` fail-closed；正常 Step 2.4 与恢复统一使用非覆写 `std::fs::hard_link` 先发 `capture.db` 后发 `libra.db`（覆盖 `Ok`/`NotFound`/`AlreadyExists`/`Unsupported` 全分支）；Step 2.6 仅在 `finalized=true` 后移 `is_file()` 墓碑并置 `spool_unfenced=true` | 并发 `drain` 零移动；非空 `quarantine/` 100% 留存；双红库按 `phase2_progress` 验 `redb-backup`；queue 与全新 Redb 仓活 spool 并集重导；`application_id == tag` 目标零覆写，竞争 `application_id == 0` `capture.db`（无论是否发生崩溃）均并轨合入 `capture.db.tmp` 并围栏竞争 inode 后替换；Step 2.6 两类可达态均零丢活事件 | 反向全路径（含无崩溃竞态与并发 `drain`）零丢提交、零丢迁移捕获、零丢竞争旧写入、零丢隔离/活 spool 事件 | `reverse_phase1_quiescence_rejects_redb_writers_and_enforces_lock_order`（`RB-11`）；`reverse_phase1_rejects_queue_producer_and_inflight_spool_tmp`（`RB-11z`）；`reverse_phase1_consumer_lock_excludes_concurrent_drain_and_rejects_quarantine`（`RB-11q`）；`reverse_phase2_step24_hardlink_capture_then_libra_and_unsupported_reject`（`RB-11j`）；`reverse_phase2_normal_step24_hardlink_reconciles_no_crash_competing_capture_open`（`RB-11y`）；`reverse_phase2_step25_26_finalize_unfence_and_legacy_hook_resume`（`RB-11l`）；`reverse_phase2_crash_before_fence_git_redb_drift_reexports`（`RB-11a`）；`reverse_phase2_crash_after_git_redb_unlink_merges_live_capture`、`reverse_phase2_crash_immediately_after_capture_redb_unlink`（`RB-11r`）；`reverse_phase2_corrupt_or_missing_backup_and_queue_tombstone_gap_union`（`RB-11m`）；`reverse_phase2_staged_hardlink_already_exists_notfound_and_both_published`（`RB-11e`）；`reverse_phase2_competing_capture_store_open_reconciles_and_fences_zero_loss`（`RB-11s`）；`reverse_fresh_redb_reconciles_preexisting_live_spool_and_establishes_tombstones`、`reverse_fresh_redb_live_spool_reconcile_blocks_concurrent_drain_moves`（`RB-11h`）；`reverse_phase2_step26_partial_cleanup_blocks_old_hook_then_resumes`、`reverse_phase2_step26_post_unfence_crash_preserves_live_old_hook_event`（`RB-11u`） |
| `GlobalConfig`/`SystemConfig` 并发首写竞态、不持锁旧版二进制竞态、首次建库全边界崩溃、有界锁争用、正向热日志、反向 `unlink(config.redb)` 与 `targets_staged_for_rename` 旧二进制写入 | 双运行时共同参与有界 `config-migration.lock`（超期立报 `LBR-IO-001`）+ 建红库前先非覆写 `std::fs::hard_link` 预占 `config.db` 哨兵 + `config-format.json`（含 `init_phase="fresh_create"`、`legacy_sqlite_tombstone`、`targets_staged_for_rename`） | 旧二进制先建则 `hard_link` 命中 `AlreadyExists` 放弃建红库（零覆写旧写入）；首次建库 3 边界崩溃幂等收敛；正向原位保留 `0400`/`0444` 围栏 `config.db` 墓碑令旧二进制报 `SQLITE_CORRUPT`；反向 `targets_staged_for_rename=true` 检出有效 `config.db` 绝不覆写（100% 保留旧二进制写入），`unlink(config.redb)` 后校验 `redb-backup` 恢复 | 零旧二进制写入覆写、零首建不可恢复残态、零旧版二进制重建第二份 SQLite、零反向崩溃覆写旧写者 | `config_scope_bounded_lock_shared_concurrency_and_timeout_error`（`RB-03h`）；`global_config_hardlink_reserve_and_unsupported_fs_reject`（`RB-03d`）；`system_config_hardlink_reserve_and_concurrent_first_write`（`RB-03f`）；`global_config_fresh_create_crash_boundaries_1_2_3`（`RB-03e`）；`system_config_fresh_create_crash_boundaries_1_2_3_and_modes`（`RB-03i`）；`gc13_global_config_forward_migration_tombstone_and_legacy_block`（`RB-11b`）；`gc13_system_config_forward_migration_tombstone_and_legacy_block`（`RB-11v`）；`config_migration_bounded_lock_contention_global_and_system_fail_closed`（`RB-11w`）；`config_forward_crash_hot_journal_torn_sector_global_and_system`（`RB-11f`）；`config_forward_crash_pending_missing_and_backup_fault_reject`（`RB-11n`）；`config_reverse_crash_after_config_redb_unlink_global_and_system`（`RB-11g`）；`config_reverse_staged_rename_preserves_old_binary_writes_global_and_system`（`RB-11o`） |

## 风险登记

| ID | 风险 | 概率 / 影响 | 缓解措施 | 归属卡 |
|---|---|---|---|---|
| RSK-RB-01 | `REL-RB-01` 长期隔离开发后 `RB-30` rebase 至最新 `origin/main` 出现新增 DB 调用方或部分接线遗漏 | 中 / 中 | `RB-29b` 与 `RB-30` 强制执行 `syn 2.0` 语法感知逐调用点 AST 守卫及负向自测、加 `init.rs:1-20` `diff -u` 门，经 `DEP-RB-01` 短汇流窗重跑双后端 `T-2` 全量测试 | `RB-29b`、`RB-30` |
| RSK-RB-02 | `redb 5.0` 在高并发下写事务等待或大读快照引起页面文件增长 | 中 / 中 | 写事务仅持最小提交窗口（`GC-RB-04`），有界退避防饥饿，缓存封顶 `16/32/64 MiB`，长驻读事务禁止跨 I/O 持有 | `RB-02`、`RB-02d`、`RB-09`、`RB-12` |
| RSK-RB-03 | `postcard` 结构演进导致旧数据反序列化失败 | 低 / 高 | 所有值强制 `[schema_ver: u16_le][postcard]` 版本头封装，单源集中于 `src/storage/codec.rs` 并配跨版本解码单测 | `RB-01b`、`RB-03`..`RB-06` |
| RSK-RB-04 | 切默认后 `compat-offline-core` 静默停止回归 SQLite，或发行包 `libra` 与无 `keyring` `libra-migrate` 成品失配，或最终默认二进制系统配置写路径误建竞争性 `config.redb` | 中 / 高 | `RB-12`/`RB-29c` 精确作用域 `diff -u` 守护 `compat-offline-core`；`RB-31`/`RB-31b` 显式锁定 SQLite、Redb、`keyring` 与双后端发行构型 CI；`RB-32`/`RB-32b` 与 `REL-RB-03`（`RB-33`/`RB-33b`/`RB-33c`）分别按 `release.yml` 各自实参构建并经独立 `mktemp -d` 暂存双后端 `libra`（含 `keyring`）与 `libra-migrate`（不含 `keyring`），验 `.sha256` 后以真实暂存成品实测切前互操作与切后双向回退；`REL-RB-02`（`RB-34`/`RB-34b`/`RB-34c`）在独立 `--target-dir` 最终默认+`keyring` 二进制上实测仓库及全部 4 类 Global/System 配置读写拒收后统一发版 | `RB-12`、`RB-29c`、`RB-31`、`RB-31b`、`RB-32`、`RB-32b`、`RB-33`、`RB-33b`、`RB-33c`、`RB-34`、`RB-34b`、`RB-34c` |
| RSK-RB-05 | 第三方外部 SQLite 工具读取、`shared_perm.rs` 禁改边界或任务沙箱未来代际路径泄漏 | 低 / 高 | 两阶段严格写集守卫 + `RB-30` `libra diff origin/main` 硬门禁 + 活跃红库迁入 `.libra/redb/` 整目录钉入 `sandbox/policy.rs`（`RB-25b`） | `RB-01d`..`RB-30` |

## 性能与容量摘要

| 指标 | 目标预算 | 约束机制 | 验证任务 |
|---|---|---|---|
| Hook 单帧入队延迟（1 KiB 已脱敏） | 中位数 ≤ `5ms`，硬上限 ≤ `50ms` | 锁外脱敏 + 共享锁内 Case A 硬件加速 `PREFIX_CRC32C` 核验 + 单次 `write_all` + `sync_data` + `LTM2` 双副本 `tail.meta`/`tail.meta.bak` 原子替换 | `RB-08`、`RB-08t`、`RB-08d`、`RB-08u`、`RB-08f`、`RB-08b`、`RB-08g`、`RB-08m`、`RB-08n`、`RB-08i`、`RB-08q`、`RB-08k`、`RB-08v`、`RB-08j`、`RB-08r`、`RB-08l`、`RB-08o`、`RB-08s`、`RB-08c`、`RB-08y`、`RB-08z`、`RB-08e`、`RB-08h`、`RB-08x`、`RB-29` |
| 单写入者批提交吞吐 | ≤ `128` 条或 `50ms` 窗口合为 `1` 次 `WriteTransaction` | `CaptureStore::append_capture_batch` 同事务推进高水位 | `RB-09` |
| 队列容量与分段边界 | 单帧 ≤ `4 MiB`、去重键 ≤ `512 B`、单段 `16 MiB` 轮转、每通道未回收封存段 ≤ `256` 项、双通道含待写帧合并硬上限 `256 MiB`（超限预变更返回 `LBR-AGENT-046`） | 16B 自认证帧头前置界限护卫 + 单一共享 `capture-queue/append.lock` 原子容量与封存表预变更背压 + Case C 满表原地恢复跳过轮转 + 消费者先更新 `LTM2` 再 `unlink` 历史段 | `RB-07`、`RB-07g`、`RB-07c`、`RB-07f`、`RB-07d`、`RB-08b`、`RB-08g`、`RB-08m`、`RB-08n`、`RB-08i`、`RB-08q`、`RB-08k`、`RB-08v`、`RB-08j`、`RB-08r`、`RB-08l`、`RB-08o`、`RB-08s`、`RB-09`、`RB-09d`、`RB-09c`、`RB-09e`、`RB-09f`、`RB-09g`、`RB-09h`、`RB-09j`、`RB-09k`、`RB-09i`、`RB-09l` |
| `redb` 页缓存内存预算 | 只读 CLI/GUI `16 MiB`、普通写 CLI `32 MiB`、长驻 `bridge`/`daemon` `64 MiB` | `storage::opener` 统一显式 `set_cache_size`（禁止默认 `1 GiB`） | `RB-02`、`RB-27` |
| 离线迁移流式内存 | 每批 ≤ `500` 行，额外堆峰值 ≤ `64 MiB`；Phase 2 围栏切换 ≤ `500ms` | 分批游标流式读写 + 单连接 `VACUUM INTO` 备份与逻辑摘要校验 | `RB-10a`、`RB-10i`、`RB-10f`、`RB-10j`、`RB-10b`、`RB-10l`、`RB-10e`、`RB-10g`、`RB-11i`、`RB-11p`、`RB-11d`、`RB-11x`、`RB-11j`、`RB-11y`、`RB-11l`、`RB-11b` |
| 多进程并发无锁读 | 32 生产者 + 8 只读进程 + 2 写进程零锁错、线性化 | `MultiWriter` + `open_read_only` MVCC + 双通道队列 | `RB-04`、`RB-12`、`RB-17b` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已同步（`RB-33` 记录官方发行 `redb` 默认存储引擎、双后端成品与 `libra-migrate` 双向迁移；`RB-34`/`RB-34c` 补充 Cargo 默认 Feature 切换与源码纯 SQLite 回退构型）。
- [ ] `docs/error-codes.md` 已同步（`RB-29d`/`RB-33`：登记 `LBR-AGENT-050` 并补齐 `LBR-AGENT-046..049` 红库队列说明）。
- [ ] `docs/commands/agent.md` 与 `docs/commands/zh-CN/agent.md` 已同步（`RB-29d`/`RB-33`：`agent daemon`、`doctor` 双通道高水位与存储引擎说明）。
- [ ] `docs/commands/init.md` 与 `docs/commands/zh-CN/init.md` 已同步（`RB-33`：新仓默认创建 `.libra/redb/git.gen-00000001.redb` 与 `storage-format.json`、`LIBRA_STORAGE_BACKEND` 与旧仓迁移指引）。
- [ ] 外部网站 `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/agent.en.md`（`RB-30` & `RB-33c`）与 `init.en.md`（`RB-33c`）满足 `DEP-RB-03` 遵循现行 `AGENTS.md:135`（默认 `/Volumes/Sea/libra-backend`，未就绪且无新书面用户裁定时 STOP）后，按 `AGENTS.md:135` 保留无关改动流程完成同步、构建、独立 Claude `VERDICT: PASS`、签名推送 `cf` 与线上核验。
- [ ] `tests/INDEX.md` 已同步登记 `storage_concurrency_stress_test`（`RB-12`）。
- [ ] `plan-status.md` 与 `plan-long.md` 日期计划索引已按规定节奏同步（首卡开工登记与最终收口）。

## 实施证据汇总

> 按 `v2.12` 仅在本节内联登记每卡提交状态、版本与验证证据，严禁在仓库内创建外部证据目录或文件。

| 任务 | 状态 | Commit SHA | 版本 | 关键验证 / CI / Release 证据 |
|---|---|---|---|---|
| `RB-01` | `done` / `remote-pending` | `57fed0f`（`main`） | `0.30.278` | 2026-11-22 03:39:49 UTC：AC1–AC8（未暂存+已暂存双态）、4 特性编译矩阵（`redb 5.0.0`/`postcard 1.2.0`/`crc32c 0.6.9`）、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo doc --no-deps --all-features`、`compat_version_surface_sync`、`compat_matrix_alignment`、`compat_serial_registry` 全绿；Codex 只读审查 `01a20155-954e-7f10-a16a-d2edb84b0569` `VERDICT: PASS`（P0/P1/P2=0） |
| `RB-01d` | `done` / `remote-pending` | `5f74eba`（`main`） | `0.30.279` | 2026-11-22 03:46:22 UTC：Stage 1 精确 1 文件写集、AC1–AC6（双关 `compile_error!` + 4 特性编译矩阵 + `storage-redb` 隐式 `storage-queue` 断言）、fmt、严格 clippy、rustdoc、`compat_version_surface_sync` 全绿；Codex `01a2015b-e3a7-75f0-8d1a-27bd39e38f6d` `VERDICT: PASS`（P0/P1/P2=0） |
| `RB-01b` | `done` / `remote-pending` | `63088e8`（`main`） | `0.30.285` | 2026-11-22 06:45:49 UTC：Stage 1 精确 3 文件写集、`versioned_postcard_roundtrip_and_corrupt_reject` 与 `io_error_display_and_cli_error_do_not_leak_sensitive_source_text`（含跨版本显式分发与 `anyhow` `{error:#}` 链路脱敏）全绿、4 特性矩阵、fmt、严格 clippy、rustdoc、`compat_version_surface_sync` 全绿；Codex R3 `01a20200-379a-7b71-aaab-73ab6e680f74` `VERDICT: PASS` |
| `RB-01c` | `done` / `remote-pending` | `0ae1cfb`（`main`） | `0.30.286` | 2026-11-22 07:17:48 UTC：Stage 1 精确 2 文件写集、`sqlite_only_rejects_redb_override`/`redb_only_rejects_sqlite_override`/`dual_feature_resolution_and_pre_release_cli_gate`（含独立子进程实测 `resolve()` 与非法值占位脱敏）全绿、fmt、严格 clippy、`compat_version_surface_sync` 全绿；Codex R2 `01a2021d-77b7-7c90-9146-40618689ff47` `VERDICT: PASS` |
| `RB-02` | `done` / `remote-pending` | `16c6ac7`（`main`） | `0.30.287` | 2026-11-22 07:57:32 UTC：Stage 1 精确 2 文件写集、`multiwriter_concurrent_open_and_bounded_write_timeout`（含 `RecordingBuilder` 校验 `MultiWriter` + `16/32/64 MiB`、阻塞池饱和 `Duration::ZERO` 及非零 deadline、多进程并发打开与有界写锁超时、`UpgradeRequired` 按格式版本可操作指引）全绿、4 特性矩阵、fmt、严格 clippy、rustdoc、`compat_version_surface_sync` 全绿；Codex R5 `01a20241-78d9-70d1-be41-35093cc20ca8` `VERDICT: PASS` |
| `RB-02d` | `done` / `remote-pending` | `f60d17e`（`main`） | `0.30.290` | 2026-11-22 08:38:12 UTC：Stage 1 精确 2 文件写集、封闭 `RedbRepairContext` 门控并私有化 `redb_builder_for`、`readonly_repair_aborted_ro_fs_and_concurrent_explicit_repair`（含真实 `CrashBackend` 崩溃夹具严格只读 `RepairAborted` 零改盘、真实 `hdiutil -readonly` 只读挂载 `EROFS` fail-closed 零改盘及 4 进程并发安全修复）全绿、4 特性矩阵、fmt、严格 clippy、rustdoc、`compat_version_surface_sync` 全绿；Codex R3 `01a2025d-810c-7b83-9f63-98f634fb5752` `VERDICT: PASS` |
| `RB-02b` | `done` / `remote-pending` | `4c3cb21`（`main`） | `0.30.292` | 2026-11-22 09:08:13 UTC：Stage 1 精确 2 文件写集、`meta_schema_role_future_version_and_migration_tombstone_fence`（含角色失配/Derived 拒绝、未来版本按角色错误码 `LBR-AGENT-048`/`LBR-CONFIG-001`/`LBR-IO-001`、已有普通表/multimap 表/空 `META_SCHEMA` 防重贴标签及跨进程 `u32::MAX` 围栏+`unlink` 已打开读写句柄阻断）全绿、4 特性矩阵、fmt、严格 clippy、rustdoc、`compat_version_surface_sync` 全绿；Codex R3 `01a20282-120a-7123-bc17-168bae3ff386` `VERDICT: PASS` |
| `RB-02c` | `done` / `remote-pending` | `30c99b4`（`main`） | `0.30.295` | 2026-11-22 09:53:52 UTC：Stage 1 精确 2 文件写集、`two_phase_finalized_manifest_and_absent_fragment_ignore`（含 `ManifestOrigin::{FreshInit,Migration}`、两阶段原子发布 `.tmp`→`fsync`→`rename`→`fsync_dir` + Windows `MOVEFILE_WRITE_THROUGH` + 不支持平台前置 fail-closed、正/反向 Phase 2 状态机与 `targets_staged_for_rename` / 缺/空 JSON 全矩阵校验、代际路径解析及缺清单忽略 `.libra/redb/` 残片）全绿、4 特性矩阵、fmt、严格 clippy、rustdoc、`compat_version_surface_sync` 全绿；Codex R6 `01a202a9-8c29-7c42-a9ae-dadabc15f783` `VERDICT: PASS` |
| `RB-02e` | `done` / `remote-pending` | `c1eac0b`（`main`） | `0.30.297` | 2026-11-22 10:34:14 UTC：Stage 1 精确 3 文件写集、`StorageBackendKind::resolve(common_storage)` 收敛为唯一公开入口 + `detect_storage_authority` + `manifest.rs` 脱敏反序列化、`manifest_first_precedence_blocks_conflicting_env_overrides`（含子进程哨兵+`1 passed; 0 failed` 实测 `finalized=false` 无条件先于 env 报 `LBR-IO-001`、终态清单与旧 `libra.db` 冲突报 `BackendMismatch`/`LBR-CLI-003`、非法/非 UTF-8 环境变量与软链边界、全新空仓覆盖与未解门控）、纯 SQLite/纯 redb/双特性矩阵、fmt、严格 clippy、rustdoc、`compat_version_surface_sync` 全绿；Codex R3 `VERDICT: PASS`（P0/P1/P2=0，Phase 0 全 9 卡闭环） |
| `RB-03` | `done` / `remote-verified` | `58ff194`（`main`） | `0.30.305` | 2026-11-22 13:18:44 UTC：Stage 1 精确 6 文件写集，实现对象安全 `ConfigStore` + `MetadataStore`、`SqliteConfigStore` 与 `RedbConfigStore`（MVCC、原子建库、取消安全）；Codex R1–R14 实质 P1 全闭环，R14 范围外支线按授权跳过 |
| `RB-03j` | `done` / `remote-verified` | `890d410`（`main`） | `0.30.306` | `GC-13` 隔离、`expected_config_file_mode` 与 `0600`/`0644` 权限实测；Codex R1 `VERDICT: PASS` |
| `RB-03b` | `done` / `remote-verified` | `600b335`（`main`） | `0.30.307` | `ConfigFormatManifest`、纯 Rust 只读 SQLite `__libra_migrated_fence__` 校验与 `ADR-CF-06` 零文件 `NotFound`；Codex R2 `VERDICT: PASS` |
| `RB-03h` | `done` / `remote-verified` | `e5df194`（`main`） | `0.30.308` | `ConfigScopeLock`、`ScopeLockedConfigStore` 持锁生命周期与 `faccessat(W_OK\|AT_EACCESS)` 非 root 只读 `SystemConfig` 回退；Codex R1–R3 全闭环，R4 与 AC4 冲突按授权跳过 |
| `RB-03d` | `done` / `remote-verified` | `c52cff5`（`main`） | `0.30.310` | Step F1 非覆写 `hard_link` 墓碑预占、双重检查锁、异步取消持锁安全与 `GlobalConfig` 并发首写/旧二进制竞态实测；Codex R4 `VERDICT: PASS` |
| `RB-03f` | `done` / `remote-verified` | `e5f2777`（`main`） | `0.30.311` | `SqliteConfigStore::create` 精确 `0600`/`0644` 权限与 `SystemConfig` `0444`/`0644` Step F1 预占/并发/旧二进制竞态实测；Codex R1 `VERDICT: PASS` |
| `RB-03e` | `done` / `remote-verified` | `717364c`（`main`） | `0.30.312` | 4 步 `init_phase="fresh_create"` 状态机、Windows `sync_all` 恢复刷盘与 `GlobalConfig` 边界 1/2/3a/3b 崩溃恢复；Codex R1–R4 全部发现闭环 |
| `RB-03i` | `done` / `remote-verified` | `14f3403`（`main`） | `0.30.313` | 2026-11-22 15:59 UTC：Stage 1 精确 1/6 文件写集 `src/storage/config_store.rs`，`system_config_fresh_create_crash_boundaries_1_2_3_and_modes` 在隔离 `LIBRA_CONFIG_SYSTEM_DB` + `umask(0o077)` 子进程实测 `SystemConfig` 边界 1/2/3a/3b 恢复、零覆写保留与严格 `0644`/`0444` 权限，fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R1 `VERDICT: PASS` 全绿 |
| `RB-03c` | `done` / `remote-verified` | `a380e51`（`main`） | `0.30.314` | 2026-11-22 16:40 UTC：Stage 1 精确 3/6 文件写集（`src/storage/{mod,config_store}.rs`、`src/storage/sqlite/config_store.rs`），落实 `ADR-RB-02` #2 分支 A 运行时感知只读分派 `open_locked_config_scope_for_read`、既有 `config.db` 写入清单自愈、`SqliteConfigStore::open_read_only` 的 pre-ledger 旧表容忍与 `inspect_configuration_schema` 未来版本 `LBR-CONFIG-001` 拒收；`dual_feature_sqlite_runtime_config_dispatch_and_authority_guard`（双特性 15/15 + 纯 `storage-sqlite` 零警告全绿）、纯 `storage-redb` 编译、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R3 `VERDICT: PASS` 全绿 |
| `RB-03g` | `done` / `remote-verified` | `9da4b2a`（`main`） | `0.30.316` | 2026-11-22 16:57 UTC：Stage 1 精确 1/6 文件写集 `src/storage/config_store.rs`，落实 `ADR-RB-02` #2 分支 B（双特性 `Redb` 运行时只读透明回退旧 `config.db` + 写拒收 `LBR-CLI-003` 提示 `libra-migrate --global/--system`；纯 `storage-redb` 构建读写双拒收 `LBR-CLI-003` 绝不返回空配置或建 `config.redb`）；`dual_feature_redb_runtime_legacy_config_db_read_fallback_and_write_reject`、`redb_only_legacy_config_db_read_and_write_fail_closed`（双特性与 `--no-default-features --features storage-redb` 全绿）、纯 `storage-sqlite` 编译、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R1 `VERDICT: PASS` 全绿（`RB-03*` 配置作用域全系列 10 卡闭环） |
| `RB-04` | `done` / `remote-verified` | `11ecda0`（`main`） | `0.30.318` | 2026-11-22 17:39 UTC：Stage 1 精确 4 文件写集（`src/storage/{mod,git_store}.rs`、`src/storage/{sqlite,redb}/git_store.rs`），实现对象安全 `GitStore`、`SqliteGitStore`（SQL 下推 `LIMIT`、`BEGIN IMMEDIATE` 单事务 CAS+Reflog+`agent_bridge_publication`）与 `RedbGitStore`（`MultiWriter`、版本化 `[u16_le][postcard]`、合作式写取消安全、符号 `HEAD` 同提交解析拒收与 Reflog 工作树跨界拒收）；`dual_backend_core_refs_reflog_and_bridge_cas_equivalence`、`redb_multiprocess_cas_linearizability_and_mvcc_reads`（4 写 + 8 读共 12 进程零停顿活体并发线性化压测）、`sqlite_core_refs_reflog_smoke`、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R4 `VERDICT: PASS` 全绿 |
| `RB-04b` | `done` / `remote-verified` | `87a299d`（`main`） | `0.30.320` | 2026-11-22 18:30 UTC：Stage 1 精确 4 文件写集（`src/storage/{mod,git_store}.rs`、`src/storage/{sqlite,redb}/git_store.rs`），补齐 `GitStore` 核心 7 表其余 5 张源表（`object_index`、`object_obliteration`、`notes`、`revision_ordinal`、`revision_ordinal_meta`）及同事务二级索引 `OBJECT_BY_PACK` / `revision_ordinal_by_oid`；实现 1,024 条单事务幂等批量写入、读前/读后单调防 TOCTOU `run_fenced_object_read`、跨进程独占事务且取消/运行时关闭安全（`txn` 随 `spawn_blocking` 持有）`run_fenced_object_write`、遗留 5 表 `RB-04` 红库只读兼容重开与安全升级 12 表及残缺半表 fail-closed；`dual_backend_object_index_obliteration_notes_and_ordinals`（含 AC1–AC5、竞争交错、取消安全、运行时关闭与 RB-04 升级回归）、`sqlite_object_index_and_obliteration_smoke`、纯 redb 检查、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R5 `VERDICT: PASS` 全绿 |
| `RB-05` | `done` / `remote-verified` | `b477d04`（`main`） | `0.30.322` | 2026-11-22 19:19 UTC：Stage 1 精确 4 文件写集（`src/storage/{mod,git_state_store}.rs`、`src/storage/{sqlite,redb}/git_state_store.rs`），实现对象安全 `GitStateStore` 及其双后端 `SqliteGitStateStore`/`RedbGitStateStore`，完整覆盖 21 张工作流/工作树/OpLog v2/Change 谱系/脏树/图层/稀疏视图源表 + 同事务二级索引 `change_revision_by_commit` + `git_state_sequences`；实现全零 SHA-1/SHA-256 OID 与 `worktree_id` 隔离、部分唯一工作区租约登记/续期/到期回收/吊销、4 表单事务原子追加兼头收敛 `append_operation_and_converge_heads`（含 6 并发写者真实并发头收敛与回滚原子实测）、SQLite 多表快照读事务（`get_sparse_view`/`get_operation`/`list_operations`）与遗留 `RB-04b` 红库只读重开一致能力基线 `[2, 3, 4]` 及安全升级；`dual_backend_22_workflow_worktree_oplog_tables_equivalence`（双特性 + 纯 `storage-redb,storage-queue`）、`sqlite_workflow_state_tables_smoke`（纯 `storage-sqlite`）、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R2 `VERDICT: PASS` 全绿 |
| `RB-05b` | `done` / `remote-verified` | `023b711`（`main`） | `0.30.325` | 2026-11-22 20:15 UTC：Stage 1 精确 4 文件写集（`src/storage/{mod,git_state_store}.rs`、`src/storage/{sqlite,redb}/git_state_store.rs`），补齐 19 张 `ai_*`（含单事务 `is_latest` 互斥、唯一约束与 `delete_ai_thread` 11 表级联）、5 张 `memory_*`（含非 UTF-8 原始路径、自然键唯一与级联清理）+ 同事务 `MEMORY_BY_PATH` 倒序二级索引（含时间戳更新剪枝与新旧时间窗回归）、3 张治理审计表（`approved_permission`/`automation_log`/`source_call_log`）及 `agent_bridge_publication`/`agent_bridge_index_publication` 严格单调两阶段（`prepared` → `published`，禁跳准备、禁降级、禁改摘要、兼容只读 2 列旧表并与 `RedbGitStore` 同库线级互通）；完成 61 张活跃持久仓库源表静态与落盘双向集合相等守卫（排除 7 张退役表与全部内部/二级索引表）；`dual_backend_28_ai_memory_bridge_tables_equivalence`、`repository_61_source_tables_manifest_complete`（双特性 + 纯 `storage-redb,storage-queue`）、`sqlite_ai_memory_bridge_tables_smoke`（纯 `storage-sqlite`）、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R3 `VERDICT: PASS` 全绿（Repository 全 61 张活跃源表双后端全部闭环） |
| `RB-06` | `done` / `remote-verified` | `2e3c0ee`（`main`） | `0.30.326` | 2026-11-22 21:55 UTC：Stage 1 精确 4 文件写集（`src/storage/{mod,capture_store}.rs`、`src/storage/{sqlite,redb}/capture_store.rs`），实现对象安全 `AgentBridgeStore` 与 `CaptureStore` 全 25 张 Capture 源表双后端（显式区隔 `META_*`/`QUEUE_HIGHWATER_*`/去重/序列与 7 张同事务二级索引表 `sessions_by_time`/`sessions_by_provider`/`checkpoints_by_session`/`events_by_time`/`bridge_events_by_seq`/`bridge_links_by_key`/`usage_stats_by_session_event`）、单事务 160 条事件语义 `dedup_key` 幂等批写与中段失败回滚、`AgentBridgeStore::append_and_ack_events` 基于 `(bridge_session_id, event_seq)` 单事务增量推进连续 `last_acked_seq`（SQLite 递归点查 CTE 零尾扫描 + 200 尾缺口回归）、`agent_coverage_revision` 只追加历史防篡改、无 `U+10FFFF` 上界的开放前缀范围扫描、`SqliteCreatePublishGuard`/`RedbCreatePublishGuard` 取消与发布后异常原子回滚守卫，以及 `capture.redb` 过新 `META_SCHEMA` 仅报 `LBR-AGENT-048` 而不阻断 `RedbGitStore`；双特性 7/7、纯 `storage-redb,storage-queue` 4/4、纯 `storage-sqlite` 2/2、单特性零告警、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R7 `VERDICT: PASS` 全绿 |
| `RB-06c` | `done` / `remote-verified` | `159bbf3`（`main`） | `0.30.327` | 2026-11-22 22:16 UTC：Stage 1 精确 4 文件写集（`src/storage/{mod,capture_store}.rs`、`src/storage/redb/{mod,capture_store}.rs`），在 `RedbCaptureStore` 新增同事务二级索引 `import_tombstones_by_provider` / `import_tombstones_by_session`（含 `open_read_write` 对 `RB-06` 遗留红库自动回填与过期索引剪枝）、同事务拒绝已墓碑化 `(agent_kind, provider_session_id)` 与 `erased_session_id` 重导（覆盖 `upsert_session`/`upsert_checkpoint`/`upsert_import_identity`/`append_capture_batch` 原子回滚含 `dedup_key` 撤销）、以及 `agent_audit_log` / `agent_workspace_scope_audit` 同事务只追加保护（覆写与 `update_*`/`delete_*` 全部 fail-closed 报脱敏错且原行零改动）；纯 `storage-redb,storage-queue` `redb_tombstone_and_audit_append_only_invariants` 1/1、双特性 `storage::capture_store::` 8/8、单特性零告警检查、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R2 `VERDICT: PASS` 全绿 |
| `RB-06d` | `done` / `remote-verified` | `d9da836`（`main`） | `0.30.328` | 2026-11-22 23:04 UTC：Stage 1 精确 4 文件写集（`src/storage/{mod,capture_store}.rs`、`src/storage/redb/{mod,capture_store}.rs`），在 `RedbCaptureStore` 落地全描述符绑定 Unix 私密安全姿态：新建 `.libra/private/`（含缺失祖先目录逐级 `mkdirat` + macOS `fchmodat(AT_SYMLINK_NOFOLLOW)` / Linux `O_PATH` + `/proc/self/fd`）精确 `0700`、新建 `capture.gen-<id>.redb`（`openat(O_CREAT|O_EXCL|O_NOFOLLOW)` + `fchmod(0600)` + `linkat` + `FileBackend::new(file)` 零路径重开 + inode 钉定 `RedbCreatePublishGuard`）精确 `0600`、打开 `.libra/private/` 与 `capture.gen-<id>.redb` 均经 `O_NOFOLLOW|O_NONBLOCK` 拒绝符号链接/FIFO 并按 `libc::geteuid()` 核验属主 UID 且错误文案零路径/UID 泄露；纯 `storage-redb,storage-queue` `redb_private_dir_and_db_unix_permissions_nofollow_and_owner` 1/1（含 `umask(0000/0077/0777)` 含缺失祖先子进程、setgid 剥离、既有父目录零变更、FIFO、5 类确定性替换竞态/早期失败及真实非属主目录/chown 校验）、双特性 9/9、单特性零告警、fmt、严格 clippy、`compat_version_surface_sync` 全绿；Codex R1（5 项）与 R2（4 项）真实发现已全部闭环，R3 仅余同 UID 目录并发下 POSIX 无原生原子 `unlinkat_if_inode`/`mkdirat_fd` 系统调用之范围外延展，依用户授权跳过继续推进 |
| `RB-06b` | `done` / `remote-verified` | `8d3c63c`（`main`） | `0.30.330` | 2026-11-23 00:01 UTC：Stage 1 精确 4 文件写集（`src/storage/{mod,cross_store}.rs`、`src/storage/redb/{git_store,capture_store}.rs`），实现 `CrossStoreCoordinator` 与 `git.redb` 延迟初始化 `meta_generation`/`meta_cross_store_intent`（保持 61 活表清单严格一致）：严格按 `GC-RB-04` `GitStore → CaptureStore` 锁序持 `cap_wtx` 在 `git_wtx.commit()` 前完成全部捕获批语义/外键/墓碑/唯一性/只追加预验（杜绝坏捕获毒化发件箱）、写前按升序重放全部 `(cap_gen + 1)..=git_gen` 待决意图、3-bit 协作式取消边界（`CrossStoreCancelGuard` + `ReplayCommitBoundaryGuard` 覆盖 `git_wtx`/`cap_wtx`/重放提交三处取消点）、`capture.redb` 缺 `meta_generation` 表或单例/只读/墓碑/损毁/缺失一律 fail-closed 报 `LBR-IO-001` 拒推进，以及只读三步一致快照（`g1 == c1 && g1 == g2`，5 次重试耗尽报 `LBR-IO-001`）；纯 `storage-redb,storage-queue` `storage::cross_store::` 6/6（含 AC1–AC4、并发双写与 `g1 != g2` 交错、防毒化、元数据腐败与 4 态异步取消）、双特性 15/15 回归（含 `repository_61_source_tables_manifest_complete`）、单特性零告警、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R5 `VERDICT: PASS` 全绿 |
| `RB-06e` | `done` / `remote-verified` | `2c70c03`（`main`） | `0.30.331` | 2026-11-23 02:10 UTC：Stage 1 精确 3/4 文件写集（`src/storage/{mod,cross_store}.rs`、`src/storage/redb/git_store.rs`），实现显式修复入口 `CrossStoreCoordinator::reconcile_pending_intents()` / `cross_store::reconcile_pending_intents()`、共享升序重放引擎 `replay_pending_intents_in_cap_wtx`、孤儿意图守卫 `ensure_no_orphan_cross_store_intents_in_wtx`、已对账意图自动清理 `prune_reconciled_cross_store_intents_in_wtx`（同时接入显式修复与后续跨库写事务双路径，使 `META_CROSS_STORE_INTENT` 严格有界不无限增长），以及 `map_cross_store_join_err` 全路径 Tokio `JoinError` 固定类别脱敏；纯 `storage-redb,storage-queue` `storage::cross_store::` 8/8（含 `explicit_reconcile_idempotent_snapshot_and_intent_pruning` AC1–AC4 与 7 场景失败/回滚/孤儿/双锁取消/panic 脱敏回归）、双特性 8/8（含 `repository_61_source_tables_manifest_complete`）、单特性零告警、fmt、严格 clippy、`compat_version_surface_sync` 全绿；Codex R1 两项 P1 已全部闭环并经 R2 确认，R2 仅余要求在存储库模块内改写进程全局 `std::panic::set_hook` 抑制默认 stderr 之范围外诉求，依用户授权跳过继续推进（**Phase 1 全 19 卡全部闭环**） |
| `RB-07` | `done` / `remote-verified` | `d8b2702`（`main`） | `0.30.332` | 2026-11-23 02:29 UTC：Stage 1 精确 3 文件写集（`src/storage/mod.rs`、`src/storage/queue/{mod,frame}.rs`），实现 `ADR-RB-05` #2 16 字节自认证帧头（`MAGIC=b"LCE1"` + `FRAME_BODY_LEN` + 覆盖前 8B 的 `HEADER_CRC32C` + 覆盖全帧非校验字段的 `FRAME_CRC32C`）与全量 UTF-8 语义 `dedup_key` 双校验编解码核心（`crc32c_checksum`/`crc32c_append`/`encode_frame`/`encode_postcard_frame`/`decode_frame`/`decode_postcard_payload`），采用显式 `into_storage_error()`（`LBR-IO-001` 脱敏且零 `From` 推导歧义）；`--no-default-features --features storage-redb` `crc32c_rfc3720_vector_and_16b_dual_crc_roundtrip` 1/1（含 RFC 3720 向量 `0xe3069283`、16B 头逐字节校验、无损往返、长 UTF-8 键末字节区分、合法双 CRC 结构非法帧拒收与脱敏转换）、全特性矩阵（`storage-sqlite`/`storage-redb`/`storage-sqlite,storage-queue`）零告警、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R2 `VERDICT: PASS` 全绿 |
| `RB-07g` | `done` / `remote-verified` | `69b7539`（`main`） | `0.30.333` | 2026-11-23 02:50 UTC：Stage 1 精确 3 文件写集（`src/storage/mod.rs`、`src/storage/queue/{mod,frame}.rs`），在 `frame.rs` 落地 `MAX_FRAME_DEDUP_KEY_LEN = 512`、`MAX_FRAME_PAYLOAD_LEN = 4 MiB`、`MIN_FRAME_BODY_LEN = 3`、`MAX_FRAME_BODY_LEN == 3 + 512 + 4 MiB` 编译期断言、`validate_frame_body_len` 分配前上下界守卫、解码后组件级上限守卫（杜绝总长合法帧偷渡超限键/负载）以及有界流式 `BoundedEnvelopeWriter` + `postcard::to_io`（峰值缓冲严格钉定 `4 MiB`、超限立停防 OOM）；`--no-default-features --features storage-redb` `storage::queue::frame::` 2/2（含精确 `512 B + 4 MiB` 往返、超限拒收、`1 GiB` 虚拟流早停、`FRAME_BODY_LEN ∈ {0,1,2}` 与超上限 `CorruptHeader`、组件级超限拒收）、单特性零告警、fmt、严格 clippy、`compat_version_surface_sync` 与 Codex R2 `VERDICT: PASS` 全绿 |
| `RB-07c` | `done` / `remote-verified` | `049350c`（`main`） | `0.30.334` | 2026-11-23 03:07 UTC：Stage 1 精确 3 文件写集（`src/storage/mod.rs`、`src/storage/queue/{mod,frame}.rs`），实现栈上零分配头校验 `parse_and_verify_header`、顺序流解码器 `FrameStreamDecoder`/`FrameStreamItem`（含分块有界读体 `read_body_bounded` 与全错误粘性毒化 `terminal_error` 防重试误报 `CleanEof`/错位读体）；`--no-default-features --features storage-redb` `storage::queue::frame::` 3/3（`acknowledged_frame_each_header_field_bitflip_rejected_pre_alloc` 以 `PreAllocSpyReader` 实证 `MAGIC` 4B / `FRAME_BODY_LEN` 界内与超大两态 4B / `HEADER_CRC32C` 4B / 伪造魔数 / 第 2 帧中段坏头 / 精确 16B 尾坏头全部零体分配、零越界读体且粘性拒收）、单特性零告警、fmt、严格 clippy、`compat_version_surface_sync` 全绿；Codex R1 P1 粘性毒化已闭环并经 R2 确认，R2 所提非帧头路径（`TruncatedTail`/`CorruptPayload`）重试测试归属紧邻下游 `RB-07f`/`RB-07d` 承接，依用户授权跳过继续推进；已推 `origin/main` |
| `RB-07f` | `done` / `remote-verified` | `9c74dfa`（`main`） | `0.30.335` | 2026-11-23 03:36 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/frame.rs`，落实强制可信边界 `FrameStreamDecoder`（`new(reader, total_len)`、`with_known_len`、`from_seekable`）与 `ADR-RB-05` #2 分配前剩余长度强校验（任一构造器在 `remaining < FRAME_BODY_LEN` 含 4 MiB 仅缺末字节场景均零体分配、零读体即报 `TruncatedTail`），同时对未达 `end_offset` 即耗尽的底层流严格 fail-closed 报 `TruncatedTail`（绝不误报 `CleanEof`）并粘性毒化；`--no-default-features --features storage-redb` `storage::queue::frame::` 4/4（`seq_decoder_distinguishes_clean_eof_and_truncated_tail_variants` 覆盖 0B 空流、4 帧多块/1B+EINTR 合法流、半体与 4 MiB 缺 1B、并发缩尾安全网、提前耗尽流、I/O 粘性毒化及 `1..=15`B 合法前缀与非魔数半头穷举）、单特性零告警、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1/R2 全部 P1 均已闭环，R3 获 `VERDICT: PASS`；已推 `origin/main` |
| `RB-07d` | `done` / `remote-verified` | `8315d51`（`main`） | `0.30.336` | 2026-11-23 03:46 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/frame.rs`，在 `verify_and_decode_body` 补齐定长头下限零 panic 防护，并新增 `acknowledged_frame_each_body_and_crc_field_bitflip_rejected` 穷举覆盖 `FRAME_CRC32C`（12..16）、`FLAGS`（16）、`DEDUP_KEY_LEN`（17..19）、`DEDUP_KEY_UTF8`（19..19+K）与 `POSTCARD_ENVELOPE`（19+K..end，含 24 KiB 多块负载首/中/末字节）全部 5 字段逐字节翻转（头校验均合法通过、`decode_frame` 与 `FrameStreamDecoder` 独立/中段含后续帧均报 `CorruptPayload` 并粘性毒化、脱敏 `LBR-IO-001` 零密钥/负载泄露）；`--no-default-features --features storage-redb` `storage::queue::frame::` 5/5、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0）；已推 `origin/main` |
| `RB-07b` | `done` / `remote-verified` | `28ee6b5`（`main`） | `0.30.337` | 2026-11-23 04:18 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/{mod.rs,queue/mod.rs}`，实现 `LTM2` 34B–4130B 双副本线协议编解码（`LaneTailMeta`/`SealedSegmentMeta`/`Ltm2CodecError`）与句柄级零 TOCTOU 双通道初始化（`private/`/`capture-queue/`/`priority/`/`normal/`/`quarantine/` 精确 `0700` + `O_NOFOLLOW` + 有效 UID 属主校验，零按名 `fchmodat`）；经两步 `O_NOFOLLOW`/`O_CREAT\|O_EXCL\|O_NOFOLLOW` 共享 `append.lock`（消除 macOS XNU 并发 `O_CREAT\|O_NOFOLLOW` `ENOENT` 竞态）串行化空通道判新（原始字节匹配 `events-*.log` 含 `events-.log`/`events-\xff.log`）与 34B `0600` `LTM2` `(0,0,0,0,[])` `tail.meta`/`tail.meta.bak` 原子无覆写发布 + 立即 `fsync_dir(lane_dir)`（Windows 走 `MoveFileExW(MOVEFILE_WRITE_THROUGH)`，其它平台 fail-closed）；`--no-default-features --features storage-redb,storage-queue` `storage::queue::` 6/6（`dual_lane_directory_permissions_nofollow_owner_and_initial_meta` 覆盖 AC1–AC5、8 线程并发竞争、持锁写者交错、`fsync_dir` 注入失败、非空通道保留与 `LTM2` 0..=256 边界）、三构型特性矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1（4×P1）与 R2（1×P1）全部闭环，R3 获 `VERDICT: PASS`；已推 `origin/main` |
| `RB-07e` | `done` / `remote-verified` | `7904eda`（`main`） | `0.30.338` | 2026-11-23 04:51 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/{mod.rs,queue/mod.rs}`，实现 `init_redb_capture_spool_tombstones`：在全新 Redb 仓经私密暂存目录原子 `renameat` 整目录发布 `.libra/private/capture-spool/`（`0700`）+ `capacity.lock`（`0600`）+ `{tmp,ready,quarantine}` `0400` 普通文件墓碑（持 `tmp_fd` 跨 `linkat` 并校验 `(st_dev, st_ino)` 消除 TOCTOU；既有目录路径在 `capacity.lock` 排他锁下预检全部 3 槽并附中途竞态 inode 级回滚；全部成功路径含 `AlreadyFenced` 重试与并发返回均无条件同步父目录 `private/` 与 `capture-spool/`；Windows 同步补齐整目录 `MoveFileExW(MOVEFILE_WRITE_THROUGH)`、`CreateFileW+FlushFileBuffers`、只读属性校验修复与有界读）；`--no-default-features --features storage-redb,storage-queue` `storage::queue::` 7/7（`fresh_redb_init_spool_tombstones_blocks_legacy_ensure_spool` 覆盖 AC1–AC5、真实旧 Hook `spool::publish` 立即 fail-closed 阻断、父/子 `fsync_dir` 失败与重试、8 线程并发、中途竞态回滚、活目录拒覆盖、符号链接/异属主拒收）、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1/R2/R3 全部 P1 已闭环并经 R4 确认（R4 仅追加 macOS 本机无法编测的 Windows 专属测试与按句柄回滚建议，已顺手收紧 Windows 有界读并依用户授权跳过继续推进）；已推 `origin/main` |
| `RB-08` | `done` / `remote-verified` | `2b681bd`（`main`） | `0.30.339` | 2026-11-23 05:35 UTC：Stage 1 精确 3 文件写集（`src/storage/mod.rs`、`src/storage/queue/{mod,segment}.rs`），实现 `segment.rs` 并发崩溃安全追加核心：`append_frame`/`append_encoded_frame`/`append_postcard_frame`/`recover_lane`/`read_converged_lane_tail_meta` 在锁外编校验单帧，锁内执行 100% 只读 Phase 1 分类（严格校验 `private/`/`capture-queue/`/`<lane>/` 精确 `0700` 权限、缺失 `append.lock` 先只读分类再独占建锁且失竞态绝不对竞态者宽松锁做 `fchmod`、流式校验 `[0, committed_len)` `prefix_crc32c`、按 `(active_seg, committed_len, min_live_seg)` 严格全序收敛双副本并拒同序不同校验和），Phase 2 清理本通道 `tail.meta*.tmp`、收敛副本与 Case C 同段前向扫描保留 100% 已 `sync_data` 完整帧，随后在 `append.lock` 内单次 `write_all` + `seg.sync_data()` 并依次经 `tail.meta.tmp -> sync_data -> rename -> fsync_dir` 与 `tail.meta.bak.tmp -> sync_data -> rename -> fsync_dir` 落盘双副本后方释锁返回；Windows 同步补齐 `CreateFileW(FILE_FLAG_OPEN_REPARSE_POINT)` 句柄固定（禁 `FILE_SHARE_DELETE` 防锁与活跃段/目录替换）；`--no-default-features --features storage-redb` `storage::queue::` 9/9（`multiprocess_32_producers_zero_interleave_and_dual_tail_meta` 32 真实子进程 × 100 变长帧 64B–64KiB = 3,200 帧 100% CRC32C 通过零交错 + 严格操作序插桩 + 全序收敛，`case_c_same_segment_crash_retains_synced_frames` 覆盖首帧崩溃、多帧已同步尾、备副本滞后、前缀翻转零改动、0755 目录/0644 锁/缺锁竞态整树快照）、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1/R2 全部 Unix P1/P2 已闭环并经 R3 确认（R3 仅余 macOS 本机无法编测的 Windows 祖先句柄固定与去 `FILE_SHARE_DELETE` 及 Windows 竞态测试要求，已落地两处 Windows 句柄加固并依用户授权跳过继续推进）；已推 `origin/main` |
| `RB-08t` | `done` / `remote-verified` | `9081ebe`（`main`） | `0.30.340` | 2026-11-23 06:14 UTC：Stage 1 精确 3 文件写集（`src/storage/mod.rs`、`src/storage/queue/{mod,segment}.rs`），实现断电持久隔离取证发布强约束原语：`QUARANTINE_DIAGNOSTIC_CODE = "LBR-AGENT-050"`、`QuarantinePublishReport`、`quarantine_artifact_id`/`quarantine_artifact_id_from_crc`、`is_lane_quarantine_temp_name`、`publish_quarantine_durable`、`quarantine_and_truncate_segment`、`quarantine_and_unlink_orphan` 与 Phase 2 通道隔离清理 `quarantine/<lane>-*.tmp`；全部公开隔离操作串行于 `queue_root/append.lock`（Unix 以 `AppendLockGuard` 类型级约束 + `O_NOFOLLOW`/`0700` 目录句柄固定 + `0600` 临时文件 + 跨 `renameat` 持 `tmp_file` 校验前后 `(st_dev, st_ino)`；Windows 以目录句柄固定 + `append.lock` + `MoveFileExW(MOVEFILE_WRITE_THROUGH)` + `FlushFileBuffers`）；受损尾与孤儿段采用固定 64 KiB 栈缓冲双遍流式校验与拷贝（零无界堆分配），强保障 `tmp.sync_data() -> rename(.tmp -> .bin) -> fsync_dir(quarantine_dir)` 严格先于 `truncate`/`unlink`；`--no-default-features --features storage-redb` `storage::queue::` 10/10（`publish_quarantine_durable_syncs_renames_fsyncs_before_destructive_ops` 覆盖 AC1–AC5 顺序插桩 + 140 KiB 跨块流式截断/删孤儿 + 8 线程同 ID/异 ID/`recover_lane` 并发竞争 + 路径穿越/符号链接拒收，`publish_quarantine_durable_crash_before_rename_and_after_dir_fsync` 在同一仓双通道覆盖 AC6 同通道重开清理与跨通道 `.tmp` 保留、AC7 重命名前崩溃原段零改动、AC8 三 `.bin` 共存且重放前双通道重开保持原 inode 与精确字节 + 幂等重放 + 重开续写）、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1（3×P1 并发锁/流式有界内存/同通道重开）与 R2（AC8 重放前重开）全部闭环，R3 所提 AC8 三 `.bin` 共存先双通道重开再重放亦已完成并实测 2/2 绿灯，依用户授权跳过 R4 继续推进；已推 `origin/main` |
| `RB-08d` | `done` / `remote-verified` | `4146c06`（`main`） | `0.30.341` | 2026-11-23 06:30 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，实现基础 Case D（洁净旧段 `actual_len(S) == committed_len > 0` + 洁净有效后继段 `events-<S+1>.log`）：Phase 1 100% 只读先验校验旧段长度与 `[0, committed_len)` `prefix_crc32c`（任一失配立即 fail-closed 返回 `CorruptCommittedFrame` `LBR-IO-001` 绝不打开后继段且整盘零改动），再校验唯一连续后继 `S + 1` 并流式只读扫描合法帧、在 Phase 1 先判 `MAX_SEALED_SEGMENTS = 256` 满表破坏态与 0 字节旧段禁封存（均整盘零改动）；Phase 2 100% 保留后继段合法帧、原子封存 `(S, committed_len, prefix_crc32c)` 至 `LTM2.sealed[]` 并通过双副本原子替换协议同步推进 `tail.meta`/`tail.meta.bak` 至 `(ACTIVE_SEG = S + 1, COMMITTED_LEN = succ_len, PREFIX_CRC32C = succ_crc)`（Windows 同步复用固定可写后继句柄免重开 TOCTOU）；`--no-default-features --features storage-redb` `storage::queue::` 11/11（`case_d_old_seg_prefix_guard_and_valid_successor_recovery` 覆盖 AC1 短段/符号链接先验顺序、AC2 位翻转/符号链接先验顺序、AC3–AC7 单次与连续多代 `S=0→1→2` 封存推进 + Subcase B1 + 跨通道 `.tmp` 隔离 + 缺口/多后继/256 满表/0B 旧段整树零改动）、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08u` | `done` / `remote-verified` | `7250dc2`（`main`） | `0.30.342` | 2026-11-23 06:48 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，扩展 Case D 有效后继恢复路径以承接含 ≥ 1 完整合法帧后接未提交半帧尾 `[succ_valid_end, actual_len(S+1))`：Phase 2 引入 `UnixPhase2Context`/`WindowsPhase2Context`（`append.lock` 持锁单重入零死锁），显式调用 `RB-08t::quarantine_and_truncate_segment_at_fds` / `quarantine_and_truncate_segment_windows_locked` 强制完成 `.tmp → sync_data → rename(.bin) → fsync_dir(quarantine_dir)` 持久取证屏障后再截断 `events-<S+1>.log` 至精确 `succ_valid_end` + `sync_data()`，随后封存旧段并提交双副本 `LTM2`（`LaneRecoveryReport.quarantined_artifacts` 记录 `LBR-AGENT-050` 取证回执）；`--no-default-features --features storage-redb` `storage::queue::` 13/13（`case_d_valid_successor_trailing_half_frame_durable_quarantine_ordering` 覆盖 `recover_lane` 与 `append_frame` 两条真实调用路径的 AC1–AC3 顺序插桩 `fsync_dir(quarantine_dir) < truncate(S+1) < tail.meta`/`write_all` + >64KiB 合法帧 + 半帧/坏头尾，`case_d_valid_successor_trailing_half_frame_crash_boundaries` 以真实 `encode_frame` 截断半帧覆盖 AC4 重命名前崩溃零提前截断、AC5 目录刷盘后截断前崩溃 `.bin` 持久留存与零重复、AC6–AC7 双副本磁盘落盘收敛及跨通道 `.tmp` 保留）、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1 零生产代码缺陷且 2 项测试建议已闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0）；已推 `origin/main` |
| `RB-08f` | `done` / `remote-verified` | `a0d1af5`（`main`） | `0.30.343` | 2026-11-23 07:08 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，实现 Case D 空孤儿段与半帧孤儿段逐动作清理、隔离、目录刷盘与双副本同步：Phase 1 归类 `CaseDCleanOrphanSuccessor`（区分 0 字节空孤儿与非空半帧孤儿）；Phase 2 对空孤儿经 `unlink_empty_orphan_at_fd_unix`（校验 `st_size == 0` + inode 固定 `unlinkat` + `fsync_dir(lane_dir)`），对非空半帧孤儿经 `RB-08t::quarantine_and_unlink_orphan_at_fds` 强保 `fsync_dir(quarantine_dir) < unlink(S+1) < fsync_dir(lane_dir)`，随后无条件经双副本原子替换协议同步 `tail.meta`/`tail.meta.bak` 至完好 `active_seg`（不封存旧段）；`recover_lane` 补齐 `lane_dir_synced` 追踪确保 `unlink` 后 `fsync_dir(lane_dir)` 失败重试前必先刷盘持久；Windows 同步补齐 `SetFileInformationByHandle(FileDispositionInfo)` 按已验证 `GENERIC_READ\|DELETE`（禁 `FILE_SHARE_DELETE`）句柄原子删除消除路径重开替换窗口；`--no-default-features --features storage-redb` `storage::queue::segment::` 7/7 + `storage::queue::` 14/14（`case_d_empty_and_half_frame_orphan_cleanup_quarantine` 覆盖 AC1–AC8、`recover_lane` 与 `append_frame` 双真实调用路径顺序插桩、>64KiB 真实 `encode_frame` 半帧孤儿、重命名前崩溃零提前删段、`unlink` 后 `fsync_dir` 失败重试双场景与旧段先验位翻转零改动）、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1 两项 P1（删后目录刷盘失败重试耐久性 + Windows 句柄原子删除）全部闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0）；已推 `origin/main` |
| `RB-08b` | `done` / `remote-verified` | `c805209`（`main`） | `0.30.345` | 2026-11-23 07:59 UTC：Stage 1 精确 3 文件写集（`src/storage/mod.rs`、`src/storage/queue/{mod,segment}.rs`），实现跨通道 `256 MiB` 硬上限合并容量守恒、有界共享 `append.lock` 等待与延迟门禁：新增 `CAPTURE_QUEUE_MAX_TOTAL_BYTES`（`256 MiB`）、`DEFAULT_QUEUE_APPEND_LOCK_TIMEOUT`（`30s`）、`QueueAppendOptions`（运行时强钳制 `<= 256 MiB` 与 `<= 30s`）、`CaptureQueueOccupancy`、`measure_capture_queue_occupancy*` 与 `append_*_with_options` 族接口；在共享 `append.lock` 下于 Step ① 合法恢复后、Step ③/④ 写段建段前精确统计 `unconsumed_priority + unconsumed_normal + quarantine + frame_buf.len()`（全部 `F_DUPFD_CLOEXEC`/`fdopendir` 补齐 `libc::rewinddir` 修复共享目录偏移复扫漏洞；先验 `AT_SYMLINK_NOFOLLOW`/`S_IFREG`/`st_uid` 或 Windows `open_windows_file_nofollow` 再排除 `seg < min_live_seg` 已回收段；异通道元数据双损保守按 `min_live_seg = 0` 全计），超限报 `LBR-AGENT-046` 且零字节写入；抽提 Unix/Windows 共用 `acquire_lock_with_deadline_loop`（首试支持 `Duration::ZERO`，每次睡后重试前先检 `now >= deadline` 杜绝超期获锁，超时报 `LBR-AGENT-049` 且零字节写入）；`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`cross_lane_capacity_boundary_race_and_single_frame_cap` 覆盖真实 `256 MiB` ±1B 边界/零改动、空通道拒建段、已回收段排除、8 线程真实 `256 MiB` 与 5×12 线程并发双通道竞态、损坏异通道保守统计、活跃/已回收目录与符号链接及隔离活跃/已回收/隔离区异属主拒收）、`VER3`（`append_lock_deadline_timeout_and_latency_budget` 覆盖真实持锁超时 `LBR-AGENT-049`/零改动、确定性虚拟时钟超期拒重获、释放恢复及 1 KiB Case A 延迟：保留全部 3 处 `File::sync_data()` 耐久屏障，非 macOS 断言中位 ≤ `5ms`，全平台断言中位与最大均 ≤ `50ms` 硬上限且 Case A `PREFIX_CRC32C`+跨通道统计 ≤ `5ms`；实测本机 Apple Silicon APFS `1×F_FULLFSYNC = 4.15–6.10ms`、`3×F_FULLFSYNC = 16.75–17.71ms` 中位/`21.20ms` 最大）及全部 `storage::queue::` 17/17、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1（4×P1）与 R2（确定性时钟锁循环 + 活跃目录/隔离 UID/Windows 错误对齐）全部技术缺陷闭环并经 R3 确认（R3 仅余 macOS APFS 下 `3×F_FULLFSYNC` 物理上无法兼得 `<= 5ms` 而不降级耐久的矛盾要求，已据实留证并依用户授权跳过推进）；已推 `origin/main` |
| `RB-08g` | `done` / `remote-verified` | `74042ca`（`main`） | `0.30.347` | 2026-11-23 08:25 UTC：Stage 1 精确 3 文件写集（`src/storage/mod.rs`、`src/storage/queue/{mod,segment}.rs`），实现基础 `16 MiB` 常规段轮转与持锁内共享段创建原语：新增并导出 `CAPTURE_QUEUE_SEGMENT_ROTATION_BYTES = 16 MiB`；抽提严格限于 `segment.rs` 持锁路径的共用原语 `create_segment_file_at_fd_unix`（需 `&AppendLockGuard`、`O_CREAT\|O_EXCL\|O_NOFOLLOW`、`fchmod`+`fstat` 强制验证精确 `0600` 与 `0` 字节 + 立即 `fsync_dir(lane_dir)`）与 Windows `create_segment_file_windows_locked`（需 `&File` 锁句柄、`CREATE_NEW` + `flush_windows_dir`），并重构 `open_existing_active_segment_unix` 与 `should_rotate_active_segment`/`build_next_append_meta`（严格保持 `committed_len == 0` 禁封存且 `== 16 MiB` 留本段，`> 16 MiB` 创建 `S+1` 并在双副本 `LTM2` 原子登记 `(S, old_committed_len, old_prefix_crc32c)` 至 `LTM2.sealed[]`、推进 `ACTIVE_SEG = S + 1` 且从 `0` 重算 `PREFIX_CRC32C`）；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`rotation_16mib_creates_0600_fsync_and_records_ltm2_sealed` 实跑真实 `16 MiB` 精确边界不轮转与超 1B 轮转 AC1–AC8、严格 `CreateSegment -> FsyncLaneDirAfterCreateSegment -> SegmentWriteAll -> dual LTM2` 操作序、`< 16 MiB` 跨界轮转 + 轮转后 `AfterSegmentSyncData` 崩溃转 Case D 无损恢复，以及真实持 `append.lock` 争用 `LBR-AGENT-049` 验证下的内部原语建段/重复 `AlreadyExists`/恢复闭环）与其余 `segment.rs` 9/9 回归、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1（收口内部持锁原语禁止公开无锁导出）与 R2（测试真实 `flock` 持锁与争用断言）全部闭环，R3 获 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08m` | `done` / `remote-verified` | `6a287c5`（`main`） | `0.30.348` | 2026-11-23 08:47 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，实现并同卡验收 `ADR-RB-05` #3 Case C **Step C1** 生产例程（`case_c_step_c1_unix` / `case_c_step_c1_windows`）：升级 `Phase1Plan::CaseCRecoverTail`（携带 `has_damaged_tail`）与 `scan_case_c_uncommitted_tail` 在 Phase 1 只读校验 `[0, committed_len)` `prefix_crc32c` 后扫描未提交尾；Phase 2 对含受损残尾的段经 `RB-08t` `quarantine_and_truncate_segment_*` 先断电持久发布 `[recovered_valid_end, actual_len)` 至 `quarantine/` 并记录诊断码 `LBR-AGENT-050`，严格在 `fsync_dir(quarantine_dir)` 之后将 `events-<S>.log` 原地截断至精确 `recovered_valid_end` 并立即执行 `sync_data()`（Unix/Windows 均补齐 `SegmentTruncateStart -> SegmentTruncateComplete -> SegmentSyncData` 插桩序），再在建任何 `S+1` 之前原子提交双副本 `LTM2` 更新 `COMMITTED_LEN = recovered_valid_end` 与 `PREFIX_CRC32C = recovered_crc32c`（保持 `ACTIVE_SEG == S` 与 `sealed[]` 不变，Step C2 由 `RB-08n` 承接）；同步对齐 `RB-08t` AC8 测试在重放前重开 `Normal` 通道自动幂等完成 Step C1；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`case_c_step_c1_quarantines_truncates_syncs_commits_ltm2_and_preserves_s` 覆盖 `recover_lane` AC1–AC8 + >64KiB 同步尾 + 严格顺序插桩、零同步尾与非零同步尾公开 `append_frame` 生产路径、及已提交前缀位翻转零改盘先验拒收）与 `segment.rs` 9/9 回归（零 rustc 警告）、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1 两项测试发现（清理未用变量 + 增补非零同步尾公开 `append_frame` 用例）闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0）；已推 `origin/main` |
| `RB-08n` | `done` / `remote-verified` | `09cb0d6`（`main`） | `0.30.350` | 2026-11-23 09:04 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，实现并同卡验收 `ADR-RB-05` #3 Case C 基础 **Step C2** 生产例程（`build_case_c_step_c2_meta`、`seal_recovered_segment_to_next_unix` / `seal_recovered_segment_to_next_windows`）：在 `RB-08m` Step C1 截断刷盘并提交 Step C1 双副本 `LTM2` 后，仅当存在受损尾截断（`has_damaged_tail == true`）时复用 `RB-08g` 持锁原语 `create_segment_file_at_fd_unix` / `create_segment_file_windows_locked` 创建新段 `events-<S+1:08>.log`（`0600` + `fsync_dir`），并在同次双副本 `LTM2` 提交中封存 `(S, recovered_valid_end, recovered_crc32c)` 至 `LTM2.sealed[]`、推进 `ACTIVE_SEG = S + 1` 并归零新段 `COMMITTED_LEN = 0` 与 `PREFIX_CRC32C = 0`（纯已同步尾 `!has_damaged_tail` 保持 `ACTIVE_SEG == S` 不触发 Step C2）；Windows 同步扩展 `WindowsPhase2ConvergedState::replacement_active_seg` 携带新建 `S+1` 固定句柄替换 Phase 1 旧 `S` 句柄消除 TOCTOU/身份失配；对齐 `RB-08m`（以 `AfterCaseCStepC1MetaBeforeStepC2` 在 Step C1 边界直接验证全部 8 项 AC）与 `RB-08t` AC8 测试；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`case_c_step_c2_seals_rotates_resets_ltm2_and_appends_s1` 覆盖 AC1–AC7、严格操作序 `StepC1 LTM2 < CreateSegment(1) < FsyncLaneDirAfterCreateSegment(1) < StepC2 LTM2`、`0600` 权限、`S+1` 公开追加与双段无损解码、单调用 `append_frame` 触发 Step C1+C2 切至 `S+2` 与纯同步尾不切段）及 `segment.rs` 10/10 回归（零 rustc 警告）、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08i` | `done` / `remote-verified` | `87618ff`（`main`） | `0.30.351` | 2026-11-23 09:26 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，补齐 Unix/Windows 对称确定性崩溃注入点（`AfterCaseCStepC1TruncateSyncBeforeMeta` 与 `AfterStepC2CreateSegmentBeforeMeta`，非测试构建零开销内联空操作）并同卡通过 `case_c_recovery_crash_boundaries_1_2_3_converge` 验收两步 Case C 恢复全部 8 项 AC（覆盖 `recover_lane` 与生产者 `append_frame` 双真实路径）：AC8 重命名前崩溃原段逐字节零提前截断 + 跨通道 `.tmp` 隔离 + 幂等重启收敛；AC1+AC7 边界 1（隔离 `fsync_dir` 后、截断前崩溃）重启零重复 `.bin` 文件且双路径断言 `QuarantineFsyncDirComplete < SegmentTruncateStart`、100% 保留全部前缀与完整已同步尾帧；AC2+AC3 边界 2（截断+`sync_data()` 后、Step C1 `LTM2` 前崩溃，含非零与零已同步尾两子态）重启走纯已同步尾/洁净分支零新隔离推进 `LTM2.COMMITTED_LEN = recovered_valid_end` 并严格保持 `ACTIVE_SEG == S`；AC4–AC6 边界 3（Step C1 后建空 `S+1`、Step C2 `LTM2` 前崩溃）重启经 Case D 断言 `OrphanUnlinkStart(1) < FsyncLaneDirAfterOrphanUnlink(1)`（生产者路径先于写新帧）删除空后继 `events-<S+1>.log`、保持 `ACTIVE_SEG == S` 与认证 `COMMITTED_LEN = recovered_valid_end` 并支持后续无缝续写；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2` 1/1、`segment.rs` 11/11 回归（零 rustc 警告）、三构型矩阵、fmt、严格 `--lib` clippy、`compat_version_surface_sync` 全绿；Codex R1 两项生产者路径测试建议闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0）；已推 `origin/main` |
| `RB-08q` | `done` / `remote-verified` | `6071884`（`main`） | `0.30.352` | 2026-11-23 09:42 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，在 Unix（`phase2_execute_plan_unix`）与 Windows（`phase2_execute_plan_windows`）的 `CaseCRecoverTail` 路径中于 `seal_recovered_segment_to_next_*` 前新增 `step_c1.meta.committed_len > 0` 跳过守卫，并在 `build_case_c_step_c2_meta` 中增加 `committed_len == 0` 防御性拒收：当全新段首帧半截崩溃致 `recovered_valid_end == 0` 时，Step C1 持久隔离 `[0, actual_len)`（`LBR-AGENT-050`）并将 `events-<S>.log` 截断回精确 0 字节后直接停在 Step C1，严格保持 `LTM2.ACTIVE_SEG == S` 与原 `LTM2.sealed.len()` 不变（绝不写入 0 字节封存段）；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`case_c_zero_byte_first_frame_crash_truncates_without_sealing` 覆盖 `S = 0` 空表、`S = 1` 既有 `sealed.len() == 1` 与直接公开 `append_frame` 生产路径 AC1–AC4 及顺序插桩）、`segment.rs` 12/12 回归、三构型矩阵、fmt、`--lib` clippy（`src/storage/` 零警告；上游 `69c5a87` `src/command/diff/options.rs:25` 存在与本卡无关的 `dead_code` 警告）、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08k` | `done` / `remote-verified` | `5f2ccae`（`main`） | `0.30.353` | 2026-11-23 10:02 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，首次独占实现嵌套 Case C→D 旧段洁净已同步尾恢复与真实边界封存（Unix/Windows 双路径对齐）：引入 `NestedOldTailPlan` 扩展 `Phase1Plan::{CaseDRecoverValidSuccessor, CaseDCleanOrphanSuccessor}`，Phase 1 先验旧段 `[0, committed_len)` 前缀（早于打开后继 `S+1`）、预扫后继并在 `classify_case_d_readonly` 中保持 `MAX_SEALED_SEGMENTS = 256` 满表优先于下游 `RB-08v` 损坏旧尾守卫、再以 `effective_old_len` 守卫禁封 0 字节旧段；Phase 2 先经 `case_c_step_c1_*` 提交旧段真实增长边界 `(S, old_valid_end, old_sealed_crc)`，再以精确 `(S, old_valid_end, old_sealed_crc)` 封存旧段至双副本 `LTM2.sealed[]` 并推进至 `S+1`（或清理孤儿后继保持 `S` 的增长边界）；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`nested_case_c_to_case_d_recovers_old_tail_and_seals_accurate_boundary` 覆盖 AC1 前缀翻转+符号链接先后序、AC2–AC6 >64KiB 双段全帧 key/flags/payload 无损解码、`append_frame` 公开多代封存、`committed_len == 0 < old_valid_end`、嵌套+空孤儿、及 Part 5 损坏旧尾 `RB-08v` 守卫与 256 满表优先整盘零改动）、`segment.rs` 13/13 回归、三构型矩阵、fmt、`--lib` clippy（`src/storage/` 零警告）、`compat_version_surface_sync` 全绿；Codex R1 两项测试增强闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0）；已推 `origin/main` |
| `RB-08v` | `done` / `remote-verified` | `08804be`（`main`） | `0.30.354` | 2026-11-23 10:18 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，在 `classify_case_d_readonly` 中解除临时 `RB-08v` 守卫并保留 Phase 1 `MAX_SEALED_SEGMENTS = 256` 满表与 `effective_old_len == 0` 零字节旧段先验 fail-closed 拒收，完整贯通 Unix/Windows 嵌套 Case C→D 旧段损坏半帧尾全区间 `committed_len <= old_valid_end < actual_len(S)` 恢复路径（Phase 2 统一经 `case_c_step_c1_*` → `RB-08t::quarantine_and_truncate_segment_*` 先完成 `publish_quarantine_durable` 含 `fsync_dir(quarantine_dir)` 并记 `LBR-AGENT-050`，再原地截断旧段至精确 `old_valid_end` + `sync_data()` 并提交 Step C1 `LTM2`，最后以精确 `(S, old_valid_end, old_sealed_crc)` 封存旧段至双副本 `LTM2.sealed[]` 并推进至 `S+1` 或清理孤儿后继）；`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`nested_case_c_to_case_d_old_seg_damaged_tail_durable_quarantine_and_seal` 覆盖 `committed_len < old_valid_end < actual_len(S)` AC1–AC3/AC7/AC8、`recover_lane` 与双损 `S`+`S+1` 生产者路径双顺序插桩）、`VER3`（`nested_case_c_to_case_d_old_seg_bad_tail_at_committed_boundary_quarantine_and_seal` 覆盖等号边界 `old_valid_end == committed_len < actual_len(S)` AC4–AC7/AC8、`recover_lane` 与坏头生产者路径及嵌套坏尾+空孤儿）、`segment.rs` 14/14 回归、三构型矩阵、fmt、`--lib` clippy（`src/storage/` 零警告）、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08j` | `done` / `remote-verified` | `9bd463e`（`main`） | `0.30.355` | 2026-11-23 10:49 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，实现 `ADR-RB-05` #3 Step ② (b) 常规 16 MiB 轮转预写 `MAX_SEALED_SEGMENTS = 256` 背压守卫（`enforce_rotation_sealed_cap_limit`，Unix/Windows 及 `build_next_append_meta` 纵深防御，于建 `S+1` 或改任何段/元数据前返回 `StorageBackendError::QueueFull` `LBR-AGENT-046`）与队列内部消费原语 `pub(super) reclaim_sealed_segment` / `reclaim_sealed_segment_at_root`（`ADR-RB-05` #4 ③：要求 `seg_id < highwater_seg && seg_id == min_live_seg && seg_id < active_seg && sealed[0].seg_id == seg_id`，严格在 Phase 2 任何元数据修复或 `.tmp` 清理前只读预检高水位/顺序/目标存在性（缺文件报 `CorruptCommittedFrame` `LBR-IO-001` 绝不静默退役）/非软链非目录/属主身份（Unix UID + Windows `GetSecurityInfo` 核 `TokenUser \|\| TokenOwner`），随后先提交双副本 `LTM2` 再 `unlink` + `fsync_dir(lane_dir)`）；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`sealed_cap_256_to_257_normal_rotation_backpressure_and_slot_release` 覆盖 AC1–AC8、256×4KiB 容量隔离、分叉副本零改动拒收未消费/缺段/软链/异属主、`AfterReclaimMetaBeforeUnlink` 崩溃+恢复清理孤儿、`dual LTM2 < ReclaimSegmentUnlink < FsyncLaneDirAfterReclaim` 顺序插桩与释放槽位后 `256→257` 轮转无损解码）、`segment.rs` 16/16 快速回归、`storage-redb` `--lib` 全零警告（含 `dead_code`）、三构型矩阵、fmt、`--lib` clippy（`src/storage/` 零警告）、`compat_version_surface_sync` 全绿；Codex R1 全部 4 项 P1 闭环并经 R2 确认，R2 追加的 Windows 提权令牌 `TokenOwner` 属主兼容已落地，R2 要求的仅 Windows 可跑回归用例因 macOS 宿主限制依用户授权跳过推进；已推 `origin/main` |
| `RB-08r` | `done` / `remote-verified` | `4326c8a`（`main`） | `0.30.356` | 2026-11-23 11:04 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/{mod,segment}.rs`，在 `LaneTailMeta::decode` 中将 O(1) `sealed_count > MAX_SEALED_SEGMENTS (256)` 校验前置于 `LTM2_MAX_LEN` 长度检查（精准返回 `InvalidInvariant` → `LBR-IO-001`），并在 Unix/Windows Phase 1 `phase1_classify_lane_*` 及 `classify_case_d_readonly` 中新增 `ensure_case_d_sealed_capacity` 在扫完有效后继段后、任何旧段尾扫描或 Phase 2 变更前即刻 fail-closed 拒收满 256 封存表的异常有效后继 `S+1`；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`sealed_cap_256_case_d_and_ltm2_decoder_bounds_rejected` 覆盖 AC1–AC6：洁净满表+有效 `S+1` 返回 `LBR-IO-001` 且零写入第 257 项、带有效 `META_CRC32C` 的 257 项 4146B/头超限/`u32::MAX`/`SEALED_COUNT != ACTIVE_SEG - MIN_LIVE_SEG`/`MIN_LIVE_SEG > ACTIVE_SEG`/跨度超 256/非连续 `seg_id` 解码及盘面双副本拒收、以及满表+嵌套损坏旧段尾+有效/半尾 `S+1`+滞后 `tail.meta.bak`+预置 `tail.meta*.tmp` 与 `quarantine/<lane>-*.tmp` 全树零改动快照相等）、`storage::queue::` 24/24 快速回归、`storage-redb` `--lib` 零警告、三构型矩阵、fmt、`--lib` clippy（`src/storage/` 零警告）、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08l` | `done` / `remote-verified` | `e82e8b5`（`main`） | `0.30.358` | 2026-11-23 11:17 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，新增共用判定 `should_run_case_c_step_c2`（强制 `has_damaged_tail && committed_len > 0 && sealed.len() < MAX_SEALED_SEGMENTS`）并接入 Unix/Windows `CaseCRecoverTail` 路径（同时在 `build_case_c_step_c2_meta` 补齐 `sealed.len() >= MAX_SEALED_SEGMENTS` 纵深拒收）：满 256 封存表遇半帧尾损崩溃时复用 `case_c_step_c1_*` 完成隔离（`LBR-AGENT-050`）、截断、`sync_data()` 与双副本 `LTM2` 边界提交后严格跳过 Step C2，保持 `ACTIVE_SEG == S`、`sealed.len() == 256` 且零新建 `S+1`；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`sealed_cap_256_case_c_skips_step_c2_and_commits_inplace_boundary` 覆盖 AC1–AC7、>64KiB 同步尾 + 等号边界、严格操作序 `QuarantineFsyncDirComplete < SegmentTruncateStart < SegmentSyncData < dual LTM2`、零 `CreateSegment` 事件与 `should_run_case_c_step_c2`/`build_case_c_step_c2_meta` 真值守卫）、`segment.rs` 18/18 快速回归、`storage-redb` `--lib` 零警告、三构型矩阵、fmt、`--lib` clippy（`src/storage/` 零警告）、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08o` | `done` / `remote-verified` | `df4b8c0`（`main`） | `0.30.359` | 2026-11-23 11:39 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，完善并实证满 256 封存表（`256×4 KiB` 小封存段，总容量远低于 `256 MiB`）Case C 恢复后 `committed_len + frame_len <= 16 MiB` 段内入队准入路径（Unix/Windows 共用 `should_rotate_active_segment` + `enforce_rotation_sealed_cap_limit`，不误报 `LBR-AGENT-046`，原位追加并推进双副本 `LTM2.COMMITTED_LEN`/`PREFIX_CRC32C`，维持 `ACTIVE_SEG == S`、`sealed.len() == 256` 与零新建 `S+1`）；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`sealed_cap_256_post_recovery_insegment_enqueue_admitted` 覆盖 AC1–AC5：恢复后常规段内入队、真实满表 Case C 恢复后精确 `== 16 MiB` 等号边界准入、以及未恢复满表 Case C 单次 `append_frame` 恢复+段内准入并逐帧核验 `dedup_key`/`flags`/`payload` 及磁盘双副本 `tail.meta`/`tail.meta.bak` 显式解码相等）、`segment.rs` 19/19 快速回归、`storage-redb` `--lib` 零警告、三构型矩阵、fmt、`--lib` clippy（`src/storage/` 零警告）、`compat_version_surface_sync` 全绿；Codex R1 两项测试加强建议闭环并经 R2 确认 P0=0/P1=0（R2 唯一 P2 显式解码 `raw_bak` 亦已一并补齐并重跑 `VER2` 绿灯）；已推 `origin/main` |
| `RB-08s` | `done` / `remote-verified` | `edb4228`（`main`） | `0.30.362` | 2026-11-23 15:01 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，抽提 Unix/Windows 共用新帧落盘例程 `write_incoming_frame_to_segment` 并接入零发布期开销可注入写盘插桩钩子 `SegmentObserver::with_incoming_frame_write_hook` / `on_incoming_frame_write_attempt`（严格位于 Step ① 恢复与 Step ② (a)/(b) 背压守卫之后）；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`sealed_cap_256_single_call_recovery_then_prewrite_reject_and_reopen` 覆盖 AC1–AC8：256×4KiB 容量隔离含恢复前后 `<= 256 MiB` 双断言、含同步尾与等号边界双态单调用先持久 Step ① 恢复再 Step ② 返回 `LBR-AGENT-046`、可注入钩子精确断言 `0` 次新帧写盘调用且同钩子正向对照入队计 `1` 次、磁盘 `events-00000256.log` 逐字节等于 `expected_recovered_bytes`、零 `events-00000257.log`、重开显式解码 `tail.meta`/`tail.meta.bak` 双副本验证持久化 `COMMITTED_LEN`/`PREFIX_CRC32C`/`sealed.len() == 256` 与公开 `append_encoded_frame` 入口）、`segment.rs` 20/20 快速回归、`storage-redb` `--lib` 零警告、三构型矩阵、fmt、`--lib` clippy（`src/storage/` 零警告）、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08c` | `done` / `remote-verified` | `425c8d6`（`main`） | `0.30.363` | 2026-11-23 15:18 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，为 Unix/Windows Phase 1 分类器补齐零发布开销 `SegmentOpEvent::CaseCTailScanStart` 插桩并同卡实证有效双副本下 Case A/E/C 已确认前缀入队前校验与整盘零改动守卫；`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`unchanged_meta_case_a_bitflip_and_case_e_short_segment_rejected` 覆盖 AC1–AC4：Case A 首/超 64KiB 中段/末字节含缺 `append.lock` 位翻转与 Case E 完整帧 `CleanEof`/半帧/0B 截短均返 `LBR-IO-001` 且通道/`quarantine/`/根树全字节快照零改动）、`VER3`（`case_c_prefix_bitflip_rejected_zero_mutation_before_tail_scan` 覆盖 AC5–AC6：纯同步尾与同步尾+坏半帧双态前缀翻转在 `append`/`recover_lane` 双路径均于 0 次 `CaseCTailScanStart` 前返 `LBR-IO-001` 且整盘零改动，还原 1 比特正向对照触发 1 次扫尾恢复）、`segment.rs` 21/21 快速回归、三构型矩阵与 `src/storage/` 全零告警 Clippy、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08y` | `done` / `remote-verified` | `501938b`（`main`） | `0.30.364` | 2026-11-23 15:36 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，实证 Subcase B1 主副本 `tail.meta` 缺失/损坏（`Deleted`/`CorruptCrc`/`TruncatedGarbage`/`Oversized` 4 态，幸存有效 `tail.meta.bak`）在 Unix/Windows Phase 1 先验前缀与长度守卫及 Phase 2 原子镜像修复；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`subcase_b1_single_replica_loss_phase1_reject_and_phase2_mirror_repair` 覆盖 AC1–AC5：4 态主损+前缀位翻转与 4×2 态主损+`CleanEof`/半帧短段在 `append_frame`/`recover_lane` 双路径均返 `CorruptCommittedFrame` `LBR-IO-001` 且通道/`quarantine/` 全字节快照零改动含预置 `tail.meta*.tmp`/`quarantine/<lane>-*.tmp`、零提前镜像修复；4 态主损+健康段含预置陈旧 `tail.meta.tmp` 在 `recover_lane` 与直接 `append_frame` 均原子重建 `0600` `tail.meta` 逐字节等于权威 `tail.meta.bak`、按 `MetaTmpWriteAndSync < MetaRename < FsyncLaneDirAfterMeta` 仅写主副本且后续追加正常）、`segment.rs` 23/23 快速回归、三构型矩阵与 `src/storage/` 全零告警 Clippy、`compat_version_surface_sync` 全绿；Codex R1 零生产代码缺陷且所提 1 项测试矩阵扩展 P1 已闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08z` | `done` / `remote-verified` | `49d8213`（`main`） | `0.30.365` | 2026-11-23 15:53 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，实证 Subcase B1 副副本 `tail.meta.bak` 缺失/损坏（`Deleted`/`CorruptCrc`/`TruncatedGarbage`/`Oversized` 4 态，幸存有效 `tail.meta`）在 Unix/Windows Phase 1 先验前缀与长度守卫及 Phase 2 对称原子镜像修复；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`subcase_b1_backup_replica_loss_phase1_reject_and_phase2_mirror_repair` 覆盖 AC1–AC5：4 态副损+前缀位翻转含 >64KiB 负载与 4×2 态副损+`CleanEof`/半帧短段在 `append_frame`/`recover_lane` 双路径均返 `CorruptCommittedFrame` `LBR-IO-001` 且通道/`quarantine/` 全字节快照零改动含预置 `tail.meta*.tmp`/`quarantine/<lane>-*.tmp`、零提前镜像修复；4 态副损+健康段含预置陈旧 `tail.meta.bak.tmp` 在 `recover_lane`、带 `with_incoming_frame_write_hook` 的观测直接追加（实证 `MetaTmpWriteAndSync(bak) < MetaRename(bak) < FsyncLaneDirAfterMeta(bak) < SegmentWriteAll` 且写新帧前 `tail.meta.bak` 已重建为原权威 `f1` 字节且零写主副本）、Step ② 紧容量 `LBR-AGENT-046` 预写拒收边界及公开 `append_frame` 均原子重建 `0600` `tail.meta.bak`）、`segment.rs` 24/24 快速回归、三构型矩阵与 `src/storage/` 全零告警 Clippy、`compat_version_surface_sync` 全绿；Codex R1 零生产代码缺陷且所提 1 项直接追加观测/边界验证 P1 已闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08e` | `done` / `remote-verified` | `435b600`（`main`） | `0.30.366` | 2026-11-23 16:14 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，实证 `ADR-RB-05` #3 情况 0 四分法（Unix/Windows 双路径对齐）：`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`virgin_and_prebootstrapped_zero_segment_lane_init` 覆盖 AC1/AC2：情况 0a 真正全新空通道在 `recover_lane`、观测直接追加与 Step ② `max_total_bytes = 1` 预建段拒收边界实证先原子落盘 34B `0600` `LTM2` `(0,0,0,0,[])` 双副本+`fsync_dir` 再建 `events-00000000.log`；情况 0b 已预建空通道含主/副双方向 × 4 损态 Subcase B1 单副本修复，实证修复严格先于 `CreateSegment(0)` 且零重写幸存副本）、`VER3`（`zero_segment_with_committed_or_corrupt_meta_fails_closed` 覆盖 AC3–AC6：情况 0c 删除唯一活跃段/多轮转段/`active_seg > 0` 零长度/非零 `prefix_crc32c` 含单副本与缺 `append.lock`，以及情况 0d 主/副/双损跨 bad-CRC/截断/0B/超长/坏魔数/等序分叉，在 `append_frame`/`recover_lane` 均返 `CorruptCommittedFrame` `LBR-IO-001` 且通道/`quarantine/`/根树全字节快照零改动）、`segment.rs` 25/25 快速回归、三构型矩阵与 `src/storage/` 全零告警 Clippy、`compat_version_surface_sync` 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08h` | `done` / `remote-verified` | `9dac263`（`main`） | `0.30.367` | 2026-11-23 16:39 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，实证非空通道 Subcase B2 双副本同损无条件 fail-closed 与整树零改动守卫（Unix/Windows 双路径对齐）：`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`dual_metadata_loss_complete_frame_boundary_and_zero_byte_active_fails_closed` 覆盖 AC1–AC4：完整帧 `CleanEof` 截短、未改动洁净段对照及 0B 活跃段截短各跑全 7 态双损矩阵含缺 `append.lock`/CRC 损/主副混合/截断/超长）、`VER3`（`dual_metadata_loss_max_and_older_segment_corrupt_or_truncated_fails_closed` 覆盖 AC5–AC8：单/多段 `max_seg` 位翻转/半帧截断及多段较旧段 `< max_seg` 位翻转/半帧/0B/`CleanEof` 截短而 `max_seg` 100% 洁净，各跑全 5 态双损矩阵），全部夹具在 `append_frame` 与 `recover_lane` 双路径均无条件返 `CorruptCommittedFrame` `LBR-IO-001` 且通道/`quarantine/`/队列根树全文件全权限逐字节快照零改动（预置 `tail.meta*.tmp` 与 `quarantine/{normal,priority}-orphan.tmp` 全原样保留）、`segment.rs` 27/27 快速回归、三构型矩阵与 `src/storage/` 全零告警 Clippy、`compat_version_surface_sync` 全绿；Codex R1（洁净对照接全矩阵）与 R2（无条件 `queue_root` 快照对比）两项测试覆盖 P1 全闭环，R3 获 `VERDICT: PASS`（P0=0、P1=0、P2=0）；已推 `origin/main` |
| `RB-08x` | `done` / `remote-verified` | `86b47f5`（`main`） | `0.30.368` | 2026-11-23 17:13 UTC：Stage 1 精确 3 文件写集内仅改动 `src/storage/queue/segment.rs`，将 Phase 2 通道限定临时文件清理收口为持锁统一例程 `clean_lane_temp_files_unix`/`clean_lane_temp_files_windows`（覆盖 append/recover/reclaim 全部入口、`Phase2ConvergedState` 追踪 `cleaned_quarantine_tmp_files`），放开 `is_lane_quarantine_temp_name` 零字符通配（`<lane>-.tmp`）并新增跨平台 UTF-16/WTF-16 前后缀匹配器 `wide_is_lane_meta_temp_name`/`wide_is_lane_quarantine_temp_name`（Windows 全程保留原始 `OsString` 消除孤立代理项静默漏清）；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`cross_lane_quarantine_tmp_cleanup_scoped_to_classified_lane_only` 覆盖 AC1–AC8：正反双方向健康通道入队与 `recover_lane` 以独立原字节目录扫描验证清空本通道 `tail.meta*.tmp` 与 `quarantine/<lane>-*.tmp` 含 `<lane>-.tmp`，原样逐字节保留损坏异通道目录/`tail.meta.tmp`/`quarantine/<other>-*.tmp`/持久 `.bin`/无关文件；含跨平台 UTF-16 `0xD800`/`0xDC00` 孤立代理项单测与 `#[cfg(windows)]` `OsString::from_wide` 通道+隔离双路径集成断言）、`segment.rs` 29/29 快速回归、三构型矩阵与 `src/storage/` 全零告警 Clippy、`compat_version_surface_sync` 全绿；Codex R1 两项生产代码 P1 均已闭环并经 R2 确认（R2 仅追加 macOS 宿主不可执行的 `#[cfg(windows)]` 通道 `tail.meta*.tmp` 孤立代理项测试，已落地补齐并重跑 `VER2` 绿灯，依用户授权跳过 R3 继续推进）；已推 `origin/main` |
| `RB-09` | `done` / `remote-verified` | `4c18db7`（`main`） | `0.30.369` | 2026-11-23 18:17 UTC：Stage 1 精确 4 文件写集（`src/storage/mod.rs`、`src/storage/queue/{mod,highwater,writer}.rs`），新增 `HWM1` 34B CRC32C 双通道高水位镜像模块 `highwater.rs`（库内 `QUEUE_HIGHWATER_{PRIORITY,NORMAL}` 唯一权威；Unix `private_fd` 锚定 `openat(O_NOFOLLOW)` + `0600` + `fsync_dir`；Windows 无 `FILE_SHARE_DELETE` 目录钉扎 + `GetFileInformationByHandle` 句柄重解析校验 + `MoveFileExW(MOVEFILE_WRITE_THROUGH)` + 严格传播 `FlushFileBuffers` 错误）与单写入者双通道优先排空批量消费者 `writer.rs`（`writer.lock → append.lock` 全局无死锁锁序且绝不跨红库写事务持 `append.lock`；`normal/` 50ms 抵达帧合批窗口 + 活跃段与已轮转封存段提交前 `priority/` 即时抢占；`LTM2.sealed[]` 长度与 CRC32C 认证及 `LTM2` 先行推进 `MIN_LIVE_SEG` 再 `unlink`+`fsync_dir` 即时回收）；`--no-default-features --features storage-redb,storage-queue` `VER1` 2/2、`VER2`（`priority_lane_drains_before_normal_with_contiguous_highwater` 5.418s 覆盖 AC1/AC2）、`VER3`（`batch_128_single_tx_dedup_and_segment_gc` 4.722s 覆盖 AC3 预装 128 帧 + 确定性模拟时钟 50ms 窗口抵达 1+127 帧单写事务与 `priority/` 抢占、AC4 完整语义 `dedup_key` 幂等、AC5 轮转段优先抢占与先 `LTM2` 后 `unlink` 回收）、四构型编译与 `src/storage/` 全零告警 Clippy、`compat_version_surface_sync` 全绿；Codex R1（3×P1+1×P2）、R2（2×P1+1×P2）、R3（2×P1）全部生产代码缺陷均已彻底闭环并经 R4 确认零生产缺陷（R4 仅剩 2 条测试项：`RB-09h` 零帧边界抢占与即刻回收边界顺序，依用户授权推进后续卡）；已推 `origin/main` |
| `RB-09b` | `done` / `remote-verified` | `99e81fa`（`main`） | `0.30.370` | 2026-11-23 18:43 UTC：Stage 1 精确 4 文件写集内仅改动 `src/storage/queue/writer.rs`，补齐双通道只读先验校验 `preflight_validate_lane`（在任一通道执行 DB 写事务或镜像写入前先对双通道元数据/高水位单调性/全量未消费封存段与活跃已确认前缀完成零写校验）、全前缀结构校验 `verify_committed_frame_structure`（覆盖超 128 批上限与 `< hw` 已消费段内帧边界与 postcard 封装）、收敛后镜像自愈及 Windows 缺席根目录 `is_writer_active_windows` 对称返回 `Ok(false)`；`--no-default-features --features storage-redb` `VER1` 3/3、`VER2`（`crash_after_commit_before_mirror_zero_dup` 9.208s 覆盖 AC1 双通道 × 4 镜像故障态 `Absent`/`StaleBehind`/`Corrupt`/`StaleAhead` × 0新帧/新帧重启 2×4×2 矩阵零重复与镜像自愈）、`VER3`（`consumer_rejects_committed_prefix_dual_meta_loss_and_hw_regression` 9.121s 覆盖 AC2–AC5 含超 128 帧尾损、`< hw` 帧损、跨通道先验原子拒收、6 态双损、8 态 0 段 Case 0c/0d、5 态 `LTM2` 倒退，全部返回 `CorruptCommittedFrame` `LBR-IO-001`、零 DB 写事务且含 `writer.lock` 的整树全字节快照零改动）、`VER4`（`single_writer_lock_contention_and_active_probe` 0.552s 覆盖 AC6/AC7 跨线程 `writer.lock` 竞争零副作用返回 `Ok` 与 `is_writer_active()` 缺席不建文件/持锁/释放准确探测）、`writer::tests` 5/5 回归、四构型矩阵与 `src/storage/` 全零告警 Clippy、`compat_version_surface_sync` 全绿；Codex R1 四项有效发现全闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09d` | `done` / `remote-verified` | `b192cae`（`main`） | `0.30.371` | 2026-11-23 19:06 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/segment.rs` 与 `src/storage/queue/writer.rs`，将 `preflight_validate_lane` 扩展至全量未回收轮转段 `meta.min_live_seg..meta.active_seg`（涵盖已消费待回收与未消费全段，同时放行 `seg < min_live_seg` 崩溃孤儿免误报），在 `step_lane_once` 历史段回收前补强 `open_and_verify_sealed_segment` 复验，并开放 `#[cfg(test)]` 段层观测器转发与 `ReclaimedOrphanUnlink`/`FsyncLaneDirAfterReclaimedOrphan`（Unix+Windows 同步）经真实 `drain_locked_at_root` 驱动 `AfterReclaimMetaBeforeUnlink` 崩溃注入；`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`consumer_rejects_rotated_seg_complete_frame_truncation_bitflip_and_missing` 0.638s 覆盖双通道 AC1 `FrameStreamDecoder` 实证 `CleanEof` 完整帧边界截断 + AC2 单位翻转含前瞻段 + AC3 段缺失 + AC4 尾部垃圾与合法额外帧 + 已消费未回收段 + 跨通道先验原子零写拒收，全返 `CorruptCommittedFrame` `LBR-IO-001`、0 DB tx、含 `writer.lock` 全树零改动）、`VER3`（`reclamation_updates_ltm2_before_unlink_and_cleans_crash_orphan` 1.269s 覆盖双通道 AC5 `AfterReclaimMetaBeforeUnlink` 含剩余帧/0剩余帧双场景重启 `unlink`+`fsync_dir` 零隔离零误报零重复及健康回收严格序 `AcquireAppendLock < MetaRename/Fsync(tail.meta) < MetaRename/Fsync(tail.meta.bak) < ReclaimSegmentUnlink < FsyncLaneDirAfterReclaim < ReleaseAppendLock`）、`writer::tests` 7/7（11.112s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09c` | `done` / `remote-verified` | `d7ebc42`（`main`） | `0.30.372` | 2026-11-23 20:05 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/segment.rs` 与 `src/storage/queue/writer.rs`，将消费者 `call_reclaim_sealed_segment` 与 `call_recover_lane` 升级为基于 `tokio::task::spawn_blocking` 有界时间片（`CONSUMER_APPEND_LOCK_POLL_SLICE = 25ms` + `5ms` 异步退避重试 `QueueBusy`，杜绝阻塞单线程异步运行时及 30s 超时退出），并将 `WriterLockGuard` 升级为 `Arc<File>` 令牌共享至全部 `spawn_blocking` 闭包（取消/丢弃外部 drain future 时仍牢牢保住 `writer.lock` 直至在途阻塞片退出）；`segment.rs` 补齐 `recover_lane_at_root_with_timeout`、`reclaim_sealed_segment_at_root_with_timeout` 与 `Phase1Classified` 观测事件；`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`consumer_waits_for_append_lock_and_does_not_quarantine_paused_producer` 1.235s 覆盖双通道 AC1–AC3：半帧未写完暂停与 `sync_data` 后提交前暂停双边界、`MissedTickBehavior::Skip` 首 80ms 切片内 >=4 次同运行时异步进度实证、明确 `Phase1Classified("CaseAClean")` 与全记录结构体/去重键无损核验、Unix `flock` + Windows `LockFileEx` 双端 `append.lock` 逐批锁核验，以及 Phase C 取消在途 `spawn_blocking` 屏障下 `writer.lock` 持续互斥回归）、`VER3`（`consumer_segment_reclamation_coordinates_with_append_lock` 0.662s 覆盖双通道 AC4 暂停生产者持 `append.lock` 期间段回收等待、保留并发写入 `committed_len` 的 `LTM2 < unlink < fsync_dir` 耐久序、前后精确容量统计与 `active_seg`/未消费段拒收）、`writer::tests` 9/9（11.235s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1–R4 全量发现逐轮闭环，R5 获 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09e` | `done` / `remote-verified` | `3e1d917`（`main`） | `0.30.373` | 2026-11-23 20:26 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/writer.rs`，新增 `CaptureQueueWriter::recover_lane_uncommitted_tail`（先对双通道做只读先验校验 `preflight_validate_lane`，确认 `[0, committed_len)` 及未回收 `LTM2.sealed[]` 合法后，在 `writer.lock → append.lock` 内委托共享两步 Case C/D 恢复例程，绝不调用 `CaptureStore::append_capture_batch`、绝不改动库内 `QUEUE_HIGHWATER_*` 或外部 `highwater` 镜像）；`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`consumer_tail_recovery_delegates_case_c_without_advancing_hw` 1.152s 覆盖双通道 AC1–AC4/AC6：真实 `drain_locked_at_root` 释锁瞬间挂钩实证 + `recover_lane_uncommitted_tail` 含未消费积压与损坏前缀 fail-closed 护卫，验证 `Phase1Classified("CaseCRecoverTail")`、保留完整同步尾帧、仅隔离 `[recovered_valid_end, actual_len)` 且记录 `LBR-AGENT-050`、释锁时 DB 高水位/外部镜像字节/DB 写事务数及共享 `spy_tracked.calls` 全零变更，以及释锁后 `append.lock` 空闲态落库）、`VER3`（`consumer_pure_synced_tail_recovery_zero_quarantine_and_no_db_write_under_lock` 0.726s 覆盖双通道 AC5/AC6：真实 `SegmentCrashPoint::AfterSegmentSyncData` 注入纯同步尾崩溃，验证原地推进 `LTM2.committed_len` 保留 `active_seg == S`、`quarantine/` 空、零 `LBR-AGENT-050`、持锁期间零 DB 写与镜像/高水位零变更）、`writer::tests` 11/11（12.491s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1 唯一 P2 闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09f` | `done` / `remote-verified` | `c9aafe8`（`main`） | `0.30.374` | 2026-11-23 20:42 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/writer.rs`，扩展测试间谍 `SpyCaptureStore` 记录每批实时 `db_highwater_after`/`tx_delta` 及 `with_after_batch_callback` 盘面落点快照，并新增端到端多批封存消费验收测试 `consumer_sealed_seg_multibatch_recovery_backlog_tail_and_final_hw_advance`（覆盖 `normal/` 与 `priority/` 双通道：非零 `hw_offset` 5 预消费帧 + 135 条已确认积压帧 + 135 条完整同步尾帧 + 半帧崩溃，经 `RB-09e` `recover_lane_uncommitted_tail` 完成 Step C2 封存切段 `0→1` 与 `LBR-AGENT-050` 隔离且高水位零变更后，实证常规多批消费 `>= 3` 批每次 1 笔红库写事务、AC1+AC2 全部 270 帧逐字段落库相等、AC3 所有中间批精确停于 `(0, batch_end_offset)` 绝不提前跳 `(1, 0)` 或提前回收、AC4 仅末批耗尽 `sealed_len(0)` 原子推进至 `(1, 0)` 且外部镜像一致、AC5 提交后按严格序 `AcquireAppendLock < MetaRename/Fsync(tail.meta) < MetaRename/Fsync(tail.meta.bak) < ReclaimSegmentUnlink(0) < FsyncLaneDirAfterReclaim(0) < ReleaseAppendLock` 安全回收旧段）；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2` 5.087s、`writer::tests` 12/12（16.685s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09g` | `done` / `remote-verified` | `4703a19`（`main`） | `0.30.375` | 2026-11-23 20:58 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/writer.rs`，新增 3 项端到端双通道（`normal/` + `priority/`）活跃段不跳段验收测试覆盖 `ADR-RB-05` #4 ② (a) 全部三类保留活跃段 `S == active_seg` 恢复态与后续追加闭环：`--no-default-features --features storage-redb` `VER1` 3/3、`VER2`（`consumer_clean_tail_recovery_multibatch_keeps_active_hw_and_subsequent_append` 3.434s 覆盖 AC1/AC2：`AfterSegmentSyncData` 注入纯已同步尾 190 帧预恢复 + 140 帧直调消费自委托两子场景，每批均停 `(0, batch_end_offset)` 至 `(0, committed_len)` 零跳段零隔离，随后追加 10 帧无缝落库）、`VER3`（`consumer_full_256_table_recovery_multibatch_keeps_active_hw_and_subsequent_append` 17.354s 覆盖 AC3/AC4：`sealed.len() == 256` 含 `seg 255` 130 帧多批封存与活跃 `S=256` 经 Step C1 原地恢复 160 帧，实证 256 封存段逐段末批跨段回收后再多批排空 `S=256` 停于 `(256, committed_len)` 绝不跳 `(257, 0)`，随后追加 5 帧 805 条全字段核验一致）、`VER4`（`consumer_zero_byte_recovery_keeps_active_hw_and_subsequent_append` 1.049s 覆盖 AC5/AC6：`S=0` 全新段与 `S=1` 轮转后段双场景首帧半截崩溃截断至 0B、0 DB 写事务、DB 高水位与镜像严格保持 `(S, 0)`，随后追加 5 帧无缝落库）、`writer::tests` 15/15（36.350s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09h` | `done` / `remote-verified` | `1c83089`（`main`） | `0.30.376` | 2026-11-23 21:13 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/writer.rs`，在 `open_and_verify_sealed_segment` 中前置 `required_boundary > sealed_entry.sealed_len` 快速 O(1) 越界拒收（确保精确返回 `capture queue DB highwater exceeds LTM2 sealed_len` → `CorruptCommittedFrame` `LBR-IO-001`），扩展 `RecordedBatchCall` 追踪每批 `applied_count`/`deduplicated_count`，并新增双通道端到端验收测试：`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`consumer_drained_active_then_rotated_16mib_and_case_c_empty_advance` 1.915s 覆盖 AC1 真实 5×3 MiB + 2 MiB 16 MiB 常规轮转交错与 AC2 Case C Step C2 含非空/空后继两子场景，实证单写事务 `append_capture_batch(&[], (1, 0))` 推进至 `(1, 0)` 且 `item_count=0/applied_count=0/零冗余业务行`）、`VER3`（`consumer_drained_rotated_bitflip_truncation_missing_and_oob_hw_rejected` 0.529s 覆盖 AC3 首/中/末字节位翻转、AC4 `FrameStreamDecoder` 实证完整帧 `CleanEof`/半帧/0B 截断、AC5 删除段文件、AC6 `hw_offset > sealed_len(S)` 越界 `+1/+64` 及 `compute_next_highwater` 单测守卫，全部 fail-closed 返 `LBR-IO-001`、0 DB 写事务、高水位零推进且整树字节快照零改动）、`writer::tests` 17/17（37.411s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1 首轮 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09j` | `done` / `remote-verified` | `8e768df`（`main`） | `0.30.377` | 2026-11-23 21:29 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/writer.rs`，新增严格 `#[cfg(test)]` 零发布开销故障注入点 `WriterCrashPoint::BeforeEmptyAdvanceCommit`（位于 `LTM2.sealed[S]` 认证之后、`append_capture_batch(&[], (S + 1, 0))` 提交之前），并新增两项双通道端到端验收测试：`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`consumer_drained_rotated_empty_advance_crash_before_and_after_commit` 2.167s 覆盖 AC1 提交前崩溃 0 DB 写/高水位与镜像停留 `(0, sealed_len)`/重启幂等提交 `(1, 0)` 零冗余行，及 AC2 提交后镜像前崩溃的空 `S+1`（0 额外 DB 写并自愈镜像至 `(1, 0)`）与非空 `S+1`（直起 `(1, 0)` 无重复空批）双子场景）、`VER3`（`consumer_drained_rotated_s1_existing_and_appended_drain` 2.937s 覆盖 AC3–AC5 在 Case C Step C2 与真实 16 MiB 常规轮转双路径下跨段立即消费 `S+1` 全部 4 条既有帧 + 5 条后续新追加帧逐字段核验一致且高水位精确停于 `(1, committed_len(1))` 绝不跳 `(2, 0)`）、`writer::tests` 19/19（39.378s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1 唯一 P2（测试注入点严格 `#[cfg(test)]` 门控）闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09k` | `done` / `remote-verified` | `c156520`（`main`） | `0.30.378` | 2026-11-23 21:45 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/writer.rs`，新增双通道端到端有序回收验收测试 `consumer_drained_rotated_old_s_safe_reclaim_ordered_actions`：Part 1 经 `AfterReclaimMetaBeforeUnlink` 故障注入在原子零帧跨段提交 `(1, 0)` 后实证 `tail.meta` 与 `tail.meta.bak` 已原子落盘 `MIN_LIVE_SEG = 1`（AC1）并同步摘除 `sealed[0]`（AC2）且 `events-00000000.log` 尚未 `unlink`；Part 2 跨 Case C Step C2（含非零 `S = 0→1` 与 `S = 1→2`）及真实 16 MiB 常规轮转实证 `writer.lock → append.lock` 锁序、双副本 `LTM2` 先落盘再 `unlink`（AC3）且 `FsyncLaneDirAfterReclaim` 紧随 `ReclaimSegmentUnlink` 之后执行（`pos_fsync_reclaim == pos_unlink + 1`，AC4）；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2` 2.264s、`writer::tests` 20/20（40.934s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1 唯一 P1（紧邻索引断言 `pos_fsync_reclaim == pos_unlink + 1`）闭环，R2 获 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09i` | `done` / `remote-verified` | `b2b2a81`（`main`） | `0.30.379` | 2026-11-23 22:17 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/segment.rs` 与 `src/storage/queue/writer.rs`，在 `writer.rs::step_lane_once` 中将 `max_live_seg > meta.active_seg` Case D / 嵌套 Case C→D 恢复委托前置于 Branch A（已封存段消费）与 Branch B（活跃段消费）之前（含先验活跃段已提交前缀只读验证），在 `normal/` 50ms 合批窗口刷新时同步核验 `next_seg_ids` 遇后继立即让出待恢复，并在 `segment.rs` Unix (`recover_under_root_fd_unix`) 与 Windows (`recover_at_root_windows`) 持 `append.lock` 释放前新增权威后验收敛校验并增补 `LaneRecoveryReport.orphan_successor_removed`、彻底移除易受 ABA 误伤的无锁 `last_recovery_signature`；`--no-default-features --features storage-redb` `VER1` 1/1、`VER2`（`consumer_delegates_nested_case_d_recovery_and_drains_all_frame_classes` 2.510s 覆盖 7 大场景：双通道洁净嵌套 Case C→D AC1–AC6 + 0 持锁写库/镜像不变 + 双损嵌套 `RB-08v`/`RB-08u` + 损坏前缀 fail-closed `LBR-IO-001` + 旧封存高水位前置委托 + 50ms 窗中后继让出 + 空孤儿后并发重建 + 外部 `recover_lane` ABA 竞态恢复）、`writer::tests` 21/21（41.969s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1（2×P1）、R2（1×P1）、R3（1×P1）全部闭环，R4 获 `VERDICT: PASS`（P0=0、P1=0、P2=0） |
| `RB-09l` | `done` / `remote-verified` | `c867b25`（`main`） | `0.30.380` | 2026-11-23 22:38 UTC：Stage 1 精确 5 文件写集内仅改动 `src/storage/queue/writer.rs`，新增严格 `#[cfg(test)]` 零发布开销 `WriterObserver::with_after_reclaim_hook` 并落地两项双通道端到端高压验收测试关闭 Phase 2（全 43 卡 100% 闭环）：`--no-default-features --features storage-redb` `VER1` 2/2、`VER2`（`consumer_eager_reclaim_unblocks_256_rotation` 16.545s 覆盖 AC1–AC3：256 小封存段 + 活跃 `S=256` ~15 MiB 隔离字节上限，实证 16 MiB 轮转先报 `QueueFull` `LBR-AGENT-046`、消费跨过首个封存段 `seg 0` 即刻逐段回收将 `sealed.len()` 降至 `255 < 256`（其余 255 段仍在盘未消费）、受阻轮转即刻建出 `events-00000257.log` 并在双副本 `tail.meta`/`tail.meta.bak` 原子登记 `(256, old_committed_len, old_prefix_crc32c)`、随后同一消费循环无缝排空余下 256 封存段及 `seg 257`）、`VER3`（`consumer_drains_full_256_table_case_c_sealed_and_active` 15.908s 覆盖 AC4–AC6：满 256 表 Step C1 原地恢复 + 5 条段内追加后高压排空 0 错通过全量 `LTM2` 校验、`sealed.len()` 严格按 `255..=0` 逐段释放、全 256 封存段 512 帧与活跃 `S=256` 12 帧逐字段核验一致）、`writer::tests` 23/23（64.097s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1 静态审查确认 P0=0/P1=0/P2=0（仅因只读沙箱无法落盘跑测标 FAIL，已由本地全量测试闭环并依用户明确授权跳过复审推进） |
| `RB-10` | `done` / `remote-verified` | `8018887`（`main`） | `0.30.381` | 2026-11-23 23:28 UTC：Phase 3 首卡，Stage 1 精确 2 文件写集（`src/storage/mod.rs`、`src/storage/migrate.rs`，`#[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]`）实现单非池化连接 `c_mig` `DELETE` 模式排他静默门（`quiesce_sqlite_source` / `quiesce_forward_repository_sqlite_sources`）：全程零活 `.db` 裸 `File::open`（依赖无 fd `lstat` + SQLite `unixClose` `pInode->pUnused` 延迟关句柄杜绝 POSIX 泄锁），前置 `PRAGMA locking_mode = EXCLUSIVE`（无 `NeedSchema` 不触盘、强制 `WAL_HEAPMEMORY_MODE` 零 `-shm`）+ 无写 `BEGIN EXCLUSIVE; ROLLBACK;` 持续保持 OS 级 `EXCLUSIVE` 锁 + 持锁态配置并校验 `PRAGMA synchronous = FULL` + `sqlite3_file_control("main", SQLITE_FCNTL_HAS_MOVED)` 句柄-inode 绑定校验 + `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE` 保护离线 WAL 与即刻取消零改动，严格限定 `Repository`/`Capture` 角色零触碰配置库；`--features storage-sqlite,storage-redb,storage-queue` `VER1` 1/1、`VER2`（`forward_phase1_rejects_writer_and_delete_active_reader` 0.117s 覆盖真实跨 OS 进程 `writer_immediate`/`writer_exclusive`/`delete_reader` 双库 AC1–AC4 均精准报于 `BEGIN EXCLUSIVE` `SQLITE_BUSY` `LBR-IO-001` 且零字节改动、5 轮中位数 ≤ 50ms 性能预算、持锁后外部进程读写探针 100% 阻断、离线 WAL/TOCTOU WAL/洁净 WAL/即刻取消/硬链别名/路径持久替换与换回/异步 `Drop`/禁入角色全回归）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1–R5 共 9 项真实发现全部逐轮闭环，R6 残余 2 项分别为 macOS 无法实测的 Windows VFS 场景与要求 `NO_CKPT_ON_CLOSE` 在 `BEGIN EXCLUSIVE` 前同时为 0 和 1 的物理矛盾，依用户明确授权跳过复审并如实记录推进 |
| `RB-10m` | `done` / `remote-pending` | 本提交（`main`） | `0.30.385` | 2026-11-24 03:28 UTC：Stage 1 精确 2 文件写集内仅改动 `src/storage/migrate.rs`，扩展正向迁移 Phase 1 为 Phase A/B 两段式 WAL 静默门：前置 `PRAGMA locking_mode = EXCLUSIVE`（`WAL_HEAPMEMORY_MODE` 零 `-shm` 读写）+ 已验证 `PRAGMA synchronous = FULL` + `SQLITE_FCNTL_FILE_POINTER`/`xRead` 零额外 fd 检查 `.db` 文件头字节 18–19（精准区分真实 WAL `2/2` 与带孤立 `-wal` 的 `DELETE` `1/1` 库），Phase A 先对 `libra.db` 与 `capture.db` 双库均取得零改动 OS 排他锁（绝不预先调 `wal_checkpoint(TRUNCATE)`），Phase B 仅在全仓无竞争后对真实离线 WAL 经持久身份绑定恢复标记（`-libra-wal-shm-cleanup.pending` 绑定 `lstat` `shm_len`/`shm_dev`/`shm_ino`）+ 直接 `PRAGMA journal_mode = DELETE;` 归并并清理过期 `-shm`（崩溃或 IO 失败可续跑恢复，伪造/无标记孤立 `-shm` 或 `-wal` 一律 fail-closed 零改动）；`--features storage-sqlite,storage-redb,storage-queue` `VER1` 1/1、`VER2`（`forward_phase1_rejects_wal_active_reader_writer_and_idle_conn` 0.259s 覆盖双库 WAL 活跃读者/活跃写者/空闲连接三类竞争 AC1–AC6 均报 `LBR-IO-001` `SQLITE_BUSY` 且 `.db`/`-wal`/`-shm`/对端库逐字节零改动、离线 3 文件 WAL 正向控制 ≤ 50ms 归并并保留 OS 排他锁、混合离线 WAL `libra.db` + 4 类竞争 `capture.db` 六文件零改动、崩溃/IO 失败续跑与伪造标记拒收、`DELETE` 孤立 `-wal`/`-wal+-shm`/`-shm` 零改动、洁净 WAL 竞态与取消、WAL 路径替换全回归）、`storage::migrate::tests` 2/2（0.271s）、四构型 Clippy/check 矩阵 `src/storage/` 零告警、`compat_version_surface_sync` 2/2 全绿；Codex R1–R6 全部真实发现逐轮闭环，R7 仅余 2 项 macOS 宿主机不可编译实测的 Windows 专属条目（Windows `-shm` 文件身份与目录刷盘），依用户明确授权跳过复审并如实记录推进 |
| `RB-10n`..`RB-34c`（剩余 81 卡） | `pending` | — | — | Phase 3 `RB-10` 与 `RB-10m` 完成，进入 `RB-10n`（正向迁移 Phase 1 在途 `capture-spool/tmp/` 与外部队列锁竞争拒收） |

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。`FAIL` 必须列出 P0/P1 条目并在下一轮复审关闭。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 | 全文（16 卡版，SHA-256 `48bcc0220edd0ce4ac18543d9659080326119d0cf7f1973c8f51b8e74e0e7ad3`） | FAIL | Codex：P0=0；P1=20（迁移竞态/非原子切库/分叉/三布局、全局系统配置、云快照一致性、队列尾损/刷盘/去重/优先级、CRC32C、`cfg` 矩阵、Hook 分派、漏 80 文件与 `ConnectionTrait`、半接线发版、平行开发与写集守卫、G-03/G-04 计数、G-11 角色矩阵、验证缺口、网站流程、后切换 SQLite CI 与 `keyring`） | 3×P2 全关；刷新至 `d242b90` 补齐 61+25+3 源表与 146 接线文件，拆至 32 卡进入 R2 | Codex exec 只读全文审查（session `01a1fd7b-d9ac-7d10-b814-cef0d951ebd6`，2026-11-21 09:4x UTC） |
| R2 | 全文（32 卡 `d242b90` 刷新版，SHA-256 `c9dc39277fc155b3af3ad4e0e5317bc5960c9d6be77b9f90db866990af059ee7`） | FAIL | Codex：P0=0；P1=10（正向清单/旧句柄/在途 spool、反向缺围栏、跨库半提交掩盖、双通道容量并发穿透、`libra-migrate` 提前放行、4 处 `--test` 名、`RB-01` 负向编译 `pipefail`、G-03 合并谓词、写集守卫误放版本面、纯红库 E2E 缺失） | 2×P2 全关（`.libra/redb/` 沙箱整目录保护与 `RB-30` 可执行门）；按 `G-09` 拆至 37 卡进入 R3 | Codex exec 只读全文审查（session `01a1fda7-43f6-7af2-b349-ead82ab774e6`，2026-11-21 10:30 UTC） |
| R3 | 全文（37 卡版，SHA-256 `908e5f53eb7d0d4fb4a33f358575f5353242f2451002d2c4e91ca0447c91b85f`） | FAIL | Codex：P0=0；P1=5（围栏前崩溃丢写、反向持锁重入死锁、Python 扫描非语法感知、`LIBRA_TEST=1` 运行时旁路、`tail.meta` 非原子替换） | P2=0；拆出 `RB-08b`/`RB-29b` 至 39 卡进入 R4 | Codex exec 只读全文审查（session `01a1fdb8-574a-7952-ba1b-79f64a7b0c6b`，2026-11-21 10:49 UTC） |
| R4 | 全文（39 卡版，SHA-256 `8cfd4a619b4962b63355826e460924fda2f17fe66d62ab5246f7ba072e52c9e3`） | FAIL | Codex：P0=0；P1=7（①`LIBRA_STORAGE_BACKEND` 覆盖绕过清单检查；②轮转新段 `sync_data` 后 `tail.meta` 重命名前崩溃漏发现孤儿后继段；③正向 `source_digests` 漏 `capture-queue/`；④AST 守卫缺运行时分发与 `*_with_conn` 可达性证明；⑤锁定/解锁二进制测试复用同名冲突；⑥DML 触发器不阻断普通 `SELECT`；⑦`RB-30`/`RB-33` 漏列外部网站写集） | P2=0；全部 7×P1 已修订进入 R5 | Codex exec 只读全文审查（session `01a1fdc2-58b6-7530-96f9-cd4b14bfcab0`，2026-11-21 11:00 UTC） |
| R5 | 全文（39 卡版，SHA-256 `6cc941bbc64d3ca02998a549ebe093f7f8e84f313bec4fc922b73687cd503173`） | FAIL | Codex：P0=0；P1=4（①SQLite 删表未阻断旧连接 `CREATE TABLE IF NOT EXISTS` DDL；②WAL 未覆盖 `-wal` 与活跃 WAL 读者；③配置回退破坏切默认前 SQLite 默认行为；④`RB-29` 未同步更新 `base.yml` 解锁态测试名） | P2=0；拆出 `RB-29a` 至 40 卡进入 R6 | Codex exec 只读全文审查（session `01a1fdd0-692e-7440-853f-fda952f30159`，2026-11-21 11:16 UTC） |
| R6 | 全文（40 卡版，SHA-256 `808d6b89dfebb9490f28458d1b54c195b6a7eb5765be5021627a7e203a715172`） | FAIL | Codex：P0=0；P1=7（①100B 多字节裸写非崩溃原子可撕裂；②移走 `capture-spool` 目录与装墓碑之间存在 `ensure_spool` 重建目录/新锁竞态且缺备份∪活目录对账；③配置迁移缺作用域切换清单与共享锁导致双写权威窗口；④`RB-32` 发行包 `libra` 不含 `storage-redb` 打不开 `libra-migrate` 产物；⑤`RB-33` 默认发行包不含 `storage-sqlite` 无法承接 SQLite 回退；⑥`RB-01` 要求生产 CLI 门控但禁改 `cli.rs`；⑦`RB-10`/`RB-10b` 等卡片按行而非严格 `G-03` 独立谓词计数） | P2=0；拆至 45 卡进入 R7 | Codex exec 只读全文审查（session `01a1fddf-4d6e-7ae3-9a41-372e8364158f`，2026-11-21 11:32 UTC） |
| R7 | 全文（45 卡版，SHA-256 `d9ec066e91cd9b1a7ddb9d16f553ad1edfdac9f6fb4b22f87dff14be80cc04ea`） | FAIL | Codex：P0=0；P1=7（①1 字节裸写 `pwrite(0, b'X')` 无法在非原子扇区设备上建立断电原子性；②Phase 1 拷贝/哈希活源库若关闭额外 fd 在 Unix 上静默释放 SQLite POSIX 排他锁；③正向恢复仅判存在文件魔数，漏掉 `unlink` 后正常缺席态与意外缺失区分；④反向迁移在 `finalized=true` 前移除 spool 墓碑，崩溃后旧 Hook 新接受的活 spool 事件会丢失；⑤配置运行时未共同参与 `config-migration.lock`，并发首次建库可发生双写分叉；⑥`RB-11`/`RB-01`/`RB-33` 等仍存多谓词合并超 `G-03` 限额；⑦`RB-32`/`RB-33` 成品测试把无 `keyring` 的 `libra-migrate` 误用含 `keyring` 构型测试且未按暂存成品运行） | P2=0；已闭环全部 7×P1 拆至 51 卡进入 R8 | Codex exec 只读全文审查（session `01a1fdf4-2d1c-7533-b085-9c8831abc8f9`，2026-11-21 11:54 UTC） |
| R8 | 全文（51 卡版，SHA-256 `c6494563fb5794ff5a295939ebe20c749fba4997abee8734edfeae25ccd796ac`） | FAIL | Codex：P0=0；P1=6（①正向配置迁移 `unlink(config.db)` 后，旧版二进制在 `config.rs:257` 见到路径缺失可重建第二份 SQLite 配置库；②反向配置迁移缺持久化 `targets_staged_for_rename` 与不覆写已发布 `config.db` 规则，崩溃后可覆写旧二进制写入；③`RB-10b`/`RB-11a`/`RB-11b` 仍存多独立谓词合并隐性超 `G-03` 限额；④反向 post-unlink 漏测 `unlink(capture.redb)` 与配置反向 `unlink(config.redb)` 及 `pending` 缺席；⑤`RB-11a` 要求残留 `quarantine` 墓碑时旧 Hook 发布新事件，与 `spool.rs:330` `ensure_spool` 三目录全校验矛盾不可达；⑥网站路径 `/Volumes/Data/gitmono/libra-backend` 需相对 `AGENTS.md:135` `/Volumes/Sea/libra-backend` 给出明确权威裁定与防歧义身份硬门） | 2×P2 全关；拆至 55 卡进入 R9 | Codex exec 只读全文审查（session `01a1fe16-6462-7f43-9460-ebb3b4b1f0e2`，2026-11-21 12:22 UTC） |
| R9 | 全文（55 卡版，SHA-256 `ef97406853821229f8f5543556f8d9d9140d6368fa37d4ee721984bdb5b5952e`） | FAIL | Codex：P0=0；P1=5（①首次新建 `Redb` 配置先建红库再 `rename` 墓碑，不持锁旧版二进制可在间隙经 `create_new` 创建 `config.db` 被静默覆盖；②首次建库在建 `config.redb` 与发布清单/墓碑间崩溃无持久化恢复状态；③反向仓库迁移先 `rename(libra.db)` 后 `rename(capture.db)`，间隙崩溃后旧进程可调 `capture::store::open` 建竞争 `capture.db` 导致两难丢数；④反向迁移假定 spool 墓碑已存在，但全新 Redb 仓未预装墓碑也未对账既有活 spool 事件；⑤`RB-03b` AC4 合并 5 个独立谓词导致隐性超 `G-03` 8 条上限） | P2=0；已闭环 #1/#2 拆至 59 卡进入 R10 | Codex exec 只读全文审查（session `01a1fe2e-28b9-7100-b3a4-e1a7b30eb21d`，2026-11-21 12:58 UTC） |
| R10 | 全文（59 卡版，SHA-256 `32de15d6d868ba60ef45e83808b9cbdc60a701934ec3041999993c54b05c4`） | FAIL | Codex：P0=0；P1=4（①正常反向迁移 Step 2.4 仍用覆盖型 `rename(capture.db.tmp, capture.db)`，无崩溃下旧进程调 `capture::store::open` 建库会被静默覆盖；②反向 Phase 1 漏持 `capture-spool-consumer.lock` 且忽略 `quarantine/`，并发 `drain` 可在扫描期间移动事件致丢失；③`RB-07`/`RB-11d` 等仍按清单行而非严格 `G-03` 独立谓词计数；④网站交付路径以历史计划/模板覆盖现行 `AGENTS.md:135` `/Volumes/Sea/libra-backend` 冲突） | P2=1（`RB-32`/`RB-33` 固定 `/tmp/libra-rb*-stage` `rm -rf` 改独立 `mktemp -d` + `trap`）；拆至 71 卡进入 R11 | Codex exec 只读全文审查（session `01a1feb6-8870-75e2-a1b4-2281d57ed67c`，2026-11-21 13:2x UTC） |
| R11 | 全文（71 卡版，SHA-256 `e0397fccf0c553a9e657cd2f8dcf1fcd67ffa428dbee744a6687c7b3b7bc2a4f`） | FAIL | Codex：P0=0；P1=3（①`RB-11j` 等卡片仍在单条 AC 内合并成功/拒绝分支及 Global+System 双独立可变库，按最严格独立可分别失败谓词超 `G-03` 8 条上限；②`ADR-RB-05` 将 `MAGIC` 与 `FRAME_BODY_LEN` 排除于 CRC32C 之外，与 `RB-07` 验收矛盾且已确认帧长篡改可被误作段尾截断；③`--no-default-features --features storage-redb` 允许编译但无队列捕获交付路径） | P2=0；拆至 80 卡进入 R12 | Codex exec 只读全文审查（session `01a1fedb-f066-71a2-a0a8-196c7a1efe10`，2026-11-21 16:07 UTC） |
| R12 | 全文（80 卡版，SHA-256 `e165ec0f83fe3fa3b3bb453f089d19bcf37affa44e6a3246045ab0431e34c9ba`） | FAIL | Codex：P0=0；P1=5（①`RB-07c` AC4 合并 5 个独立字段导致 ≥10 独立谓词超 `G-03` 8 条上限，需按字段/分支级口径重核全卡；②正常队列入队 Case A 跳过 `[0, committed_len)` 前缀校验且 Case C 未显式 fail-closed 拒收 `actual_len < committed_len`；③`tail.meta` 缺损时无法区分已确认帧损坏/截断与未提交尾残片，可能静默丢弃已确认数据；④`RB-34` 最终默认+`keyring` 二进制验收遗漏 `config set --system` 写拒收；⑤`RB-29a` 对 `compat-offline-core` 零修改仅 `rg` 旧测试名，缺相对前置基线的精确作用域 diff 验证） | P2=0；拆至 90 卡进入 R13 | Codex exec 只读全文审查（session `01a1ff02-f21c-7fb2-97f7-eb440e281a3a`，2026-11-21 16:4x UTC） |
| R13 | 全文（90 卡版，SHA-256 `1c6db142befbd7b6802b0ad2353776654cfd6e976cf60eae1126450ee36b4fa0`） | FAIL | Codex：P0=0；P1=3（①`RB-08` 6 行 AC 内合写前缀核验、帧保留、隔离、unlink 与同步等多项独立子断言超 `G-03` 8 条上限；②Subcase B2 双副本缺损仅扫 `max_seg` 漏查较旧未消费轮转段损坏或截断；③`RB-34`/`RB-34c` 依次构建默认红库与纯 SQLite 两个 `--bin libra` 到同一 `target/release/libra`，后者覆写前者导致最终默认二进制实测跑错成品） | P2=0；拆至 92 卡进入 R14 | Codex exec 只读全文审查（session `01a1ff20-14ae-7331-8271-9307d925ef79`，2026-11-21 17:22 UTC） |
| R14 | 全文（92 卡版，SHA-256 `7b4acf25990fdf20f16992e1ff7a28644cd1bdb7b17fe2f0b9a94120c5e1310c`） | FAIL | Codex：P0=0；P1=3（①Subcase B2 双副本同损按段扫描重建无法检出在完整帧边界截断仍为 `CleanEof` 或活跃段截断为 0 字节，致已确认帧可被静默丢弃；②`RB-08d` 7 行 AC 仍将保留+推进、`unlink`+`fsync_dir` 等独立可失败子动作合并致 ≥10 谓词超 `G-03` 8 条上限，需按独立谓词重审全计划；③消费者仅持 `writer.lock` 未与 `append.lock` 协调即隔离未提交尾并推进高水位，可与活跃提交中的生产者竞态误隔离在途写入） | P2=0；已闭环全部 3×P1 并逐卡复审拆至 113 卡待 R15 复审 | Codex exec 只读全文审查（session `01a1ff2f-ee2c-7f43-9dac-d43f192c798b`，2026-11-21 17:39 UTC） |
| R15 | 全文（113 卡版，SHA-256 `86e59df8eddd2b5cbb9b8545f621ac5324324e30bb7f8785e1591383847323c5`） | FAIL | Codex：P0=0；P1=4（①0 个段文件时无条件重置双副本为 `(0,0,0)`，唯一活跃段被删时抹除非零已提交元数据并绕过 Case E；②消费者尾损恢复在 DB 写事务提交前提前推进通道高水位至 `active_seg + 1`，会跳过同段未消费积压帧与 Case C 已同步完整尾帧；③`RB-01` AC1 合并 3 项独立特性定义致全卡 ≥9 谓词超 `G-03` 8 条上限，需按独立谓词重审全计划；④双副本元数据仅校验活跃段而未持久化已轮转未回收段的长度与校验和，未消费轮转段在精确完整帧边界截断仍呈 `CleanEof` 无法检出） | P2=0；已闭环全部 4×P1（升级 `LTM2` 轮转封存表 + 0 段四分法 + 消费者零跳帧原子高水位恢复 + 全计划深度拆至 125 卡）并同步修正 `RB-32b` `G-07` 为 `no-release`，待 R16 复审 | Codex exec 只读全文审查（session `01a1ff50-b86f-7501-927d-3824db2aeb8a`，2026-11-21 18:13 UTC） |
| R16 | 全文（125 卡版，SHA-256 `954744000b07b66e46aade804825f9d64954d625d78bb42040dc17de4d413bff`） | FAIL | Codex：P0=0；P1=4（①Case C 截断保留同步尾并建 `S+1` 后、提交 `LTM2` 前崩溃转入 Case D 时，Case D 以旧 `committed_len` 封存 `S` 导致消费端长度失配阻断排空；②`LTM2.sealed[]` 限 256 项但常规轮转与 Case C 均未检查该上限，停消费期间重复尾损恢复可触发第 257 项溢出破坏元数据或阻塞恢复；③`RB-09e` AC2/3/4 合并多独立动作与 `RB-01` AC4 合并 3 可选依赖及 `sea-orm` 仍超 `G-03` 独立谓词上限并与审计说明自相矛盾；④`RB-01` `src/lib.rs` 验证门使用 `rg -c "^-[^-]"` 在 0 删除洁净态退出 1 必败，且依赖校验用 OR 漏检 `postcard` 与选项） | P2=0；已闭环全部 4×P1（两步 Case C + Case D 封存前恢复旧段尾 + `RB-08i`、`MAX_SEALED_SEGMENTS = 256` 预变更背压与满表原地恢复 + `RB-08j`、全计划重核拆净至 131 卡、修复 `RB-01` 零删除与三依赖独立验证门）待 R17 复审 | Codex exec 只读全文审查（session `01a1ff69-70e2-7450-8372-8f9ceed13da6`，2026-11-21 18:42 UTC） |
| R17 | 全文（131 卡版，SHA-256 `f35a03aa391a2d3cbdc97f67c267ef95e39484aca7c0deb5c75c37086b76ba68`） | FAIL | Codex：P0=0；P1=3（①`ADR-RB-05` #4 ② 与 `RB-09f` AC3 无条件将高水位推至 `(S+1, 0)`，但纯已同步尾、满 256 封存表与 0 字节恢复均保留 `S` 为活跃段，且须兼顾 `≤ 128` 批上限，无条件跳至 `S+1` 会导致后续追加到 `S` 被跳过或报高水位倒退；②`RB-08i` 6 条清单下 AC4（嵌套恢复 4 动作）与 AC5（封存校验+3 类帧落库）独立谓词合计超 `G-03` 8 条上限；③`RB-01` VER8 用 `libra diff -- src/lib.rs` 仅对比工作区与暂存区，暂存删除+未暂存新增可穿透，而暂存洁净新增后空 diff 必败） | P2=0；已闭环全部 3×P1（活跃段不跳段/已封存段末批跨段高水位协议 + `RB-09f`/`RB-09g`、拆净 `RB-08g`/`RB-08i`/`RB-08j` 至 135 卡、修复 `RB-01` 完整双态基线对比并消除全计划 `<(...)` 进程替换）待 R18 复审 | Codex exec 只读全文审查（session `01a1ff79-64d6-7d90-b526-7452c09b1786`，2026-11-21 18:57 UTC） |
| R18 | 全文（135 卡版，SHA-256 `63ef4c643f9dbeb18c432ef9bce3ef14331038b73ae982bfdbb4b15c7cd8e5ef`） | FAIL | Codex：P0=0；P1=4（①活跃段排空停于 `(S, committed_len)` 后再被生产者轮转时 `hw_offset == sealed_len(S)` 且无剩余帧可组成最终批次，缺原子零帧推进致无法跨段消费或回收旧段；②`RB-08k`/`RB-08j`/`RB-08l` 位于 `RB-09`/`RB-09d` 之前却要求运行尚未实现的消费者与回收逻辑，DAG 顺序不可执行；③`RB-08k` 6 条 AC 实际含 ≥10 个独立谓词且 `RB-08l` Step C1 合并多动作，仍低估 `G-03` 独立谓词计数；④`RB-01` VER8 的 `^\+[^+]`/`^-[^-]` 过滤会漏检内容本身以 `+`/`-` 开头的额外增删行；另只读沙箱拒绝 `mktemp -d` 令 VER7 未能实跑） | P2=0；已闭环全部 4×P1（新增 `ADR-RB-05` #4 ② (c) 原子零帧推进与 `RB-09h`、`RB-08k`/`RB-08j`/`RB-08l` 纯化为段层且消费验收后置 `RB-09i`、单谓词拆净至 137 卡、纯内存 `python3` 完整行比对替换 VER8 与全部 `mktemp`）待 R19 复审 | Codex exec 只读全文审查（session `01a1ff85-01fe-7bb3-a8e0-26111457b192`，2026-11-21 19:12 UTC） |
| R19 | 全文（137 卡版，SHA-256 `5d0ff12aec1706d29338083ffab895b6c13985e5e15c5511a0674b66021a2124`） | FAIL | Codex：P0=0；P1=1（`RB-08m`/`RB-08l`/`RB-09h` 仍存在合并可分别失败动作致独立谓词计数超 `G-03` 8 条上限：`RB-08m` 合并 `truncate`+`sync_data`、建文件+`fsync_dir` 与 3 类帧；`RB-08l` 合并 `truncate`+`sync_data` 及三项跳过 Step C2 结果；`RB-09h` 合并双轮转路径、损坏/截断及既有/新追加帧） | P2=1（Phase 3 进入条件残留 `RB-09f` 未对齐 `RB-09i`）；已彻底闭环 1×P1 与 1×P2（拆出 `RB-08n`/`RB-08o`/`RB-09j` 并展开 `RB-08g`/`RB-08i`/`RB-08k`/`RB-08j` 至 140 卡、同步修正 Phase 3 进入条件为 `RB-09i`）待 R20 复审 | Codex exec 只读全文审查（session `01a1ff8d-2940-7c70-85ee-9169b3490bdb`，2026-11-21 19:21 UTC） |
| R20 | 全文（140 卡版，SHA-256 `45763539376e6f649c48ca2572c98adddb757f8cf07713f3efd0d0a3267323c7`） | FAIL | Codex：P0=0；P1=2（①`RB-08n` AC4 合并 `ACTIVE_SEG`/`COMMITTED_LEN`/`PREFIX_CRC32C` 达 9 谓词、`RB-08i` 边界 3 合并 `ACTIVE_SEG`/`COMMITTED_LEN` 达 9 谓词、`RB-09i` 合并建段与登记及校验与两类段入库；②`RB-09j` 回收合并 `MIN_LIVE_SEG`/`unlink`/`fsync_dir`、`RB-08j` 合并背压错与零改动及 Case D 合并拒收与零第 257 项） | P2=0；已将上述全部 5 卡与 `RB-08o` 按逐字段/逐动作 1:1 原子拆净至 145 卡（新增 `RB-08p`/`RB-08q`/`RB-08r`/`RB-09k`/`RB-09l`，全组修订卡 ≤ 7/8，同步更新 Phase 3 与 `RB-10` 依赖至 `RB-09l`）待 R21 复审 | Codex exec 只读全文审查（session `01a1ff94-058e-7410-bd1f-959ac479e9d5`，2026-11-21 19:28 UTC） |
| R21 | 全文（145 卡版，SHA-256 `df831a7bbd983683e147520095b2e118ad0211a611bea5ecbf5c506af97b577d`） | FAIL | Codex：P0=0；P1=2（①`RB-08o` 6 条 AC 合并段内写与推进 `committed_len` 及零新建 `S+1`/既有段零改/`tail.meta*` 零改合计达 9 谓词超 `G-03` 8 条上限；②`RB-08p` 与 `RB-09i` 以独立 `implementation` 发版卡仅做既有实现测试/集成验收违反 `G-02` 实现与测试同卡原则） | P2=0；已闭环全部 2×P1（撤销纯测试 `RB-08p` 并回 `RB-08m`/`RB-08n` 各 8/8、重构 `RB-08l`/`RB-08o`/`RB-08j` 为 7/8 独立行为增量、落实 `RB-09i` 消费端 Case D 委托与 `RB-09l` 高压逐段即时回收生产实现，共 144 卡）待 R22 复审 | Codex exec 只读全文审查（session `01a1ff9a-0c13-71f3-b278-08579c36ee86`，2026-11-21 19:35 UTC） |
| R22 | 全文（144 卡版，SHA-256 `a61531861064784398a3c020ccad6099672e34dbe672fd6ab6adf0467ecdecf3`） | FAIL | Codex：P0=0；P1=3（①`RB-08n` 已声明 `recovered_valid_end > 0 && sealed.len() < 256`，与后续 `RB-08q`/`RB-08l` 重复归属同一 Step C2 守卫；②`RB-08n` AC1 合并建 `S+1` 与 `0600` 权限致实际 9 谓词超 `G-03` 8 条上限；③`ADR-RB-05` #3 Step ① 恢复变更先于 Step ② 256 项背压检查，与“修改任何磁盘文件之前拒绝”及 `RB-08o` 零改动契约冲突且缺单调用恢复+拒收实测） | P2=0；已闭环全部 3×P1（渐进清晰划分 `RB-08g`→`RB-08n`→`RB-08q`→`RB-08j`→`RB-08r`→`RB-08l`→`RB-08o` 零重叠、`RB-08g` 8/8 拆清建段/`0600`/`fsync_dir` 且 `RB-08n` 7/8 复用共用原语、修订 `ADR-RB-05` #3 Step ② 区分 Step ① 合法恢复与 Step ② 新帧零写并在 `RB-08o` 8/8 实测单调用先恢复再拒收）待 R23 复审 | Codex exec 只读全文审查（session `01a1ffa2-5346-79a3-b47d-2fbff8342159`，2026-11-21 19:44 UTC） |
| R23 | 全文（144 卡版，SHA-256 `7d811f954dfda10ec4570d2df761d29fdfd91cf67af2e657d8ff3657fd00e36a`） | FAIL | Codex：P0=0；P1=2（仅在 `RB-08o`：①AC1 合并段内帧写入与无损解码致实际 ≥9 谓词超 `G-03` 上限；②AC5 仅以最终文件长 `== recovered_valid_end` 不足以排除前缀覆写或先写入被拒新帧再截断，且缺重开验证持久化恢复态） | P2=0；已将 `RB-08o` 拆为 `RB-08o`（5/8 段内准入，分离 `Ok(())` 与解码读回）+ `RB-08s`（8/8 单调用 Step ① 恢复→Step ② 预写拒收屏障，含写路径零调用插桩、`actual_bytes == expected_recovered_bytes` 逐字节比对与重开校验持久化 `COMMITTED_LEN`/`PREFIX_CRC32C`/`sealed.len()`，共 145 卡）待 R24 复审 | Codex exec 只读全文审查（session `01a1ffa7-2887-76a0-9d27-2ff0a8eec62c`，2026-11-21 19:49 UTC） |
| R24 | 全文（145 卡版，SHA-256 `1f6753c674ae844dacd96207ea6c7fd6d8b1e8d238e6fd30305805eba3b19c3c`） | FAIL | Codex：P0=0；P1=2（①`RB-08s` 未要求并断言恢复后总容量含新帧 `<= 256 MiB`，全局字节上限 Step ② (a) 可能先报同一 `LBR-AGENT-046` 掩盖缺失的 256 封存表守卫；②Case C Step C1 与 `RB-08m` 未要求在破坏性截断/删段前先对隔离文件 `sync_data()` 并 `fsync_dir(quarantine_dir)`，断电可保留截断却丢失隔离取证字节） | P2=1（`RB-08l` `Out of scope` 将超限拒收误归 `RB-08o` 而非 `RB-08s`）；已彻底闭环全部 2×P1 与 1×P2（在 `RB-08` 与 `RB-08d` 间前置 `RB-08t` 8/8 `publish_quarantine_durable`/`quarantine_and_truncate_segment`/`quarantine_and_unlink_orphan` 断电持久发布强约束原语覆盖全部 4 处隔离路径及两处崩溃边界注入、在 `RB-08j`/`RB-08s` 显式增加 `<= 256 MiB` 条件隔离断言、修正 `RB-08l` 等 `Out of scope`，共 146 卡）待 R25 复审 | Codex exec 只读全文审查（session `01a1ffab-4947-7a91-8713-fb3f41b5174c`，2026-11-21 19:54 UTC） |
| R25 | 全文（146 卡版，SHA-256 `f9d69481e1a0e3f8a6f0182efc3244911bdff3a2901d5d2a939dc0f96931c037`） | FAIL | Codex：已检范围 P0=0、P1=0、P2=0，但因只读沙箱拒绝 bash heredoc 临时文件提前中止，未跑完 146 行审计表与全量 `G-01`..`G-09` 审查，故维持未通过 | 保持冻结文件零改动，改用纯内存 `python3 -c` 指令直接重跑完整 R26 复审 | Codex exec 只读审查（session `01a1ffb4-f273-7bf2-ba35-98047a39a2c3`，2026-11-21 20:05 UTC） |
| R26 | 全文（146 卡版，SHA-256 `f9d69481e1a0e3f8a6f0182efc3244911bdff3a2901d5d2a939dc0f96931c037`） | FAIL | Codex：P0=0；P1=2（①`RB-08d` AC5 已实现并验证嵌套 Case C→D 旧段尾恢复，致 `RB-08k` 成为同一行为的重复验收卡且与 `RB-08d` `Out of scope` 冲突违反 `G-02`；②`ADR-RB-05` #3 Case D 要求对有效后继段 `S+1` 合法帧后的未提交半帧先持久隔离再截断，但下游调用路径缺构造该故障并验证顺序插桩与崩溃边界的验收卡） | P2=1（Step ① 在只读判定损坏态前即清理 `tail.meta*.tmp`/`quarantine/*.tmp`，违背损坏态零磁盘改动）；已彻底闭环全部 2×P1 与 1×P2（纯化 `RB-08d` 7/8 与 `RB-08k` 8/8 零重叠、新增 `RB-08u` 7/8 有效后继半帧尾持久隔离与崩溃边界、全 4 处调用方补齐顺序插桩、`ADR-RB-05` #3 Step ① 改先只读分类再清 `.tmp` 并在 `RB-08c`/`RB-08e`/`RB-08h` 实测，共 147 卡）待 R27 复审 | Codex exec 只读全文审查（session `01a1ffb7-0404-77e0-88b4-1c9d4a642dec`，2026-11-21 20:07 UTC） |
| R27 | 全文（147 卡版，SHA-256 `8cbd0f56cdd24bc2da96c6484c00c78ee0613463b33c6eee520dbc45bee69574`） | FAIL | Codex：P0=0；P1=3（①`RB-08e` 描述将 0 段存在单损元数据笼统归入拒收，与情况 0b 及 AC2 矛盾；②Case D 满 256 封存表有效后继在 Phase 1 仅检旧段前缀即放行 `.tmp` 清理与旧段尾截断，后判非法时已破坏取证证据；③`RB-08k` AC2/AC8 含复合谓词致 8 行实际 ≥10 谓词超 `G-03` 上限） | P2=1（共享 `quarantine/` 清理 `quarantine/*.tmp` 未按通道前缀隔离，健康通道入队可误删另一损坏通道临时文件）；已彻底闭环全部 3×P1 与 1×P2（对齐 `RB-08e` 0b/0d 描述、Case D 满表后继纳入 Phase 1 100% 只读先判并在 `RB-08r` 6/8 验整盘快照零改动、拆净 `RB-08k` 6/8 与 `RB-08v` 5/8 单谓词、Phase 2 限定 `quarantine/<lane>-*.tmp` 并在 `RB-08h` 6/8 验跨通道留存，共 148 卡）待 R28 复审 | Codex exec 只读全文审查（session `01a1ffbd-7032-7ec2-9a7c-13405f5c025a`，2026-11-21 20:14 UTC） |
| R28 | 全文（148 卡版，SHA-256 `a8a4057ac17f4b2132100f8f598e0fe8595e347e8e67b2232d25e8e5b5963509`） | FAIL | Codex：P0=0；P1=1（`RB-08v` 描述仅写 `committed_len < old_valid_end < actual_len(S)`，遗漏坏尾紧接已确认边界的等号分支 `old_valid_end == committed_len < actual_len(S)`；另部分机械检查未全部独立跑完） | P2=0；已闭环唯一 P1（`RB-08v` 扩充为 `8/8` 单谓词完整覆盖 `committed_len <= old_valid_end < actual_len(S)` 含等号边界持久隔离、顺序插桩、截断与封存，148 卡不变）待 R29 全量复审 | Codex exec 只读审查（session `01a1ffc4-515b-7cb0-89db-4f5bbad3a267`，2026-11-21 20:21 UTC） |
| R29 | 全文（148 卡版，SHA-256 `db0c6c37811d1070063d2e1ded68fbbc79138fcfae8312468e74cf63b24332a6`） | FAIL | Codex：P0=0；P1=1（`RB-08h` AC1–AC4 各将 `CorruptCommittedFrame` 错误返回与磁盘原状零改动合并，叠加 AC5/AC6 实计 ≥10 独立谓词超 `G-03` 8 条上限；另未完所有 148 卡语义与机械扫查） | P2=0；已闭环唯一 P1 并全计划拉网扫清全部同类「错误码+零改动」成对合并卡（`RB-08h` 8/8 + 拆出 `RB-08x` 4/8、`RB-08c` 6/8 + 拆出 `RB-08y` 5/8 + `RB-08e` 6/8、`RB-10` 4/8 + 拆出 `RB-10m` 8/8、`RB-11` 5/8 + 拆出 `RB-11z` 4/8，共 152 卡，全计划零成对谓词超限）待 R30 复审 | Codex exec 只读审查（session `01a1ffc7-6744-74b3-87dc-7d08e7e2d7f6`，2026-11-21 20:25 UTC） |
| R30 | 全文（152 卡版，SHA-256 `cbe3313f4ecf3a6242c308c2af2eab2fc2435d757896557d9214b96a105dbb56`） | FAIL | Codex：P0=0；P1=3（①`RB-10m` 先调 `wal_checkpoint(TRUNCATE)` 再调 `journal_mode=DELETE` 可在空闲 WAL 连接导致切模式失败前已改写 `.db`/`-wal`，与零改动契约冲突；②`RB-08y` 仅测删除/损坏 `tail.meta` 方向，缺 `tail.meta.bak` 缺失/损坏对称验收；③`RB-08x` 仅测 `quarantine/<lane>-*.tmp` 未验本通道内 `tail.meta*.tmp` 清理与跨通道留存） | P2=1（`RB-11q` AC1 合并错误返回与零改动致少计 1 谓词）；已彻底闭环全部 3×P1 与 1×P2（WAL 静默改为直调 `PRAGMA journal_mode=DELETE` 先争独占锁并经多进程 SQLite 实测零改动、新增 `RB-08z` 5/8 对称覆盖副副本缺失/损坏、`RB-08x` 扩充至 8/8 覆盖通道内 `tail.meta*.tmp`、`RB-11q` 拆至 5/8，共 153 卡）待 R31 复审 | Codex exec 只读审查（session `01a1ffcf-4195-7700-8b1b-0fae487418ca`，2026-11-21 20:33 UTC） |
| R31 | 全文（153 卡版，SHA-256 `5a68927142dca664f39469122b783ed74140b2e188e10d0aff584f44bd46b5ce`） | FAIL | Codex：P0=0；P1=1（`ADR-RB-07` #1 要求 WAL 活跃写者触发直接 `PRAGMA journal_mode=DELETE` 失败并保持 `.db`/`-wal`/`-shm` 不变，但 `RB-10m` 仅覆盖 WAL 活跃读者与空闲连接，缺 WAL 活跃写者验收；另机械脚本对 `DEP-RB-*` 与跨 AC 引用产生误报） | P2=0；已闭环唯一 P1（将 `RB-10m` 拆为 `RB-10m` 6/8 完整覆盖 WAL 活跃读者/活跃写者/空闲连接三类竞争与三文件零改动，并拆出 `RB-10n` 4/8 专承在途 spool/队列锁竞争，共 154 卡）待 R32 复审 | Codex exec 只读审查（session `01a1ffd4-daf2-7980-99d1-bc77a724a042`，2026-11-21 20:39 UTC） |
| R32 | 全文（154 卡版，SHA-256 `3d3f300ad3c5355ff4905a44b5b206e03e669731890afeae715b6e860a2f3736`） | FAIL | Codex：P0=0；P1=1（`RB-10n` 描述承诺源库、spool 与 queue 三域零改动，但 AC2 未验 `capture-queue/` 快照且 AC4 未验 `capture-spool/` 快照） | P2=1（故障恢复矩阵未同步登记外部 `capture-queue/append.lock` 竞争触发与检测）；已彻底闭环全部 1×P1 与 1×P2（`RB-10n` 与对称 `RB-11z` 均扩充至 `6/8` 补齐三域逐字节快照、同步更新故障恢复矩阵，154 卡不变）待 R33 复审 | Codex exec 只读审查（session `01a1ffd7-fd24-7bd0-ac45-af9c8306b4e1`，2026-11-21 20:43 UTC） |
| R33 | 全文（154 卡版，SHA-256 `41ffa8b76855c75205205b38127c2f917e64e555a10d770727eadaefc089e071`） | FAIL | Codex：P0=0；P1=1（全量机械校验与故障恢复矩阵全部通过，仅 `RB-10n` 与 `RB-11z` AC2/AC5 仍将源库快照与 spool/queue 快照合在同一条断言致实际 8 谓词被记为 `6/8`） | P2=0；已彻底闭环唯一 P1（`RB-10n` 8/8、`RB-11z` 8/8 及同类 `RB-11q` 6/8 全部逐域拆净为 1:1 单域单谓词快照断言，154 卡不变）待 R34 复审 | Codex exec 只读审查（session `01a1ffda-bf45-7350-96c7-9f5cc8580de1`，2026-11-21 20:45 UTC） |
| R34 | 全文（154 卡版，SHA-256 `594acd82f37aadab8da23c1a2806a57c8ee1f45f94a27b27ffaf399e5f4b038e`） | PASS | Codex：P0=0；P1=0（154 卡、154 行 15 列审计表、154 条无环 DAG 边、120 发布窗口、146 唯一接线文件、Phase 1–5 边界、命令约束、陈旧短语、网站 `cf` 门控及 `RB-10n` 8/8、`RB-11z` 8/8、`RB-11q` 6/8 逐域快照拆净全部复核通过） | P2=0；零阻塞项，满足开工前计划准入门禁 | Codex exec 只读审查（session `01a1ffdd-79dc-7591-8f67-44678a2fb712`，2026-11-21 20:48 UTC） |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-RB-01 | 查询命令实时合并 `capture-queue/` 未消费帧 | 延续 `plan-20261116` `ADR-CQ-04` / `DEFER-CQ-02`，保持读路径零队列扫描 | 用户明确需要亚 50ms 队列读可见性 | 未来队列只读投影计划 |
| DEFER-RB-02 | 物理移除 `storage-sqlite` Feature 与 `sea-orm` 仓库存储依赖 | 需先经历稳定发布周期验证 `redb` 生产可靠性且保留第三方 SQLite 读取与 `libvault` | 默认红库稳定运行 ≥ 2 个发布周期且无 P1 回归 | 未来 SQLite 退场清理计划 |
| DEFER-RB-03 | 单机多仓库合并常驻写入守护进程 | 每仓库独立 `libra agent daemon` 已足够收敛 Hook 高并发写入，避免跨仓权限/租户复杂度 | 单机多仓进程内存压力被实测证实 | 未来守护进程多租户计划 |
| DEFER-RB-04 | `redb` 在线压缩（`compact`）集成至 `libra gc` 自动化策略 | 首期通过批写与有界事务控制空间增长，避免与 GC 锁策略耦合 | 实测长期活跃仓库 `.redb` 空闲页占比超 40% | 未来存储维护增强计划 |
| DEFER-RB-05 | 存量 SQLite 并发冲突/权限修复与 bare `redb` 仓库 `shared_perm.rs` 扩展 | 依用户明确指令本计划严禁触碰 `src/internal/db.rs`、`capture/{store,detach}.rs` 与 `src/utils/shared_perm.rs`，确保零干扰其它并发 Agent | 维护者单独立项权限与 SQLite 维护计划 | 未来专项权限与遗留库维护计划 |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [ ] 所有 154 张任务卡满足粒度规则 `G-*`：无未登记的 L 例外、无 XL 卡、无碎片卡、无未登记的合并发布例外、实现写集冲突均已消解；「任务卡粒度审计表」与每卡 `Granularity:` 行 100% 一致，且每卡独立 pass/fail 谓词总数严格满足 `plan-template.md` `G-03` 上限。
- [ ] 全部 146 个生产接线文件均已接入 `crate::storage`，且 4 个禁改文件（`src/internal/db.rs`、`src/internal/ai/capture/{store,detach}.rs`、`src/utils/shared_perm.rs`）与 `src/command/init.rs:1-20` 零改动。
- [ ] 所有非延后任务的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`。
- [ ] 所有任务的 Verification 命令已运行并记录结果（所有 `(new)` 用例均经 `nextest list` 与 `nextest run --no-tests=fail` 实证）。
- [ ] **计划完成门 / 全量收口门（ER-13）**：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --no-deps --all-features`，以及全量矩阵（`--no-default-features --features storage-sqlite`、`--no-default-features --features storage-redb`、`--no-default-features --features storage-redb,storage-queue`、`--features storage-sqlite,storage-redb,storage-queue`、`--features keyring`）全绿。
- [ ] **全量收口门暴露的 Bug 已全部处理**：本计划改动导致的失败已前滚修复并重跑全量至全绿；既有失败附基线复现证据并登记 `FIX-*`/`DEFER-*`。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成（含满足 `DEP-RB-03` 遵循现行 `AGENTS.md:135` 授权硬门后的网站 `$LIBRA_BACKEND` `cf` 保留无关改动交付、独立 Claude Code `VERDICT: PASS` 与线上部署检查）。
- [ ] 每张卡的 `Rollback mode` 已被实际验证（含按 `release.yml` 实参经独立 `mktemp -d` 暂存的双成品完成 `libra-migrate --to redb` / `--to sqlite` 双向无损回退演练）。
- [ ] Codex 最终结论为 `VERDICT: PASS`，P0=0、P1=0 全部关闭。
- [ ] 每张独立发布卡及家族发布点 `RB-30`/`RB-33c`/`RB-34c` 已按 `ER-08` 完成 `patch + 1`、`Cargo.lock` 工具链刷新、`gh release create` 触发 `release.yml` 并取得 `base.yml`/`codeql.yml`/`release.yml` 绿灯证据。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（`G-09`）。
- [ ] 本计划未新建外部证据文件或证据文件夹（`v2.12`）。
- [ ] `plan-status.md` 的计划行与 `plan-long.md` 日期计划索引已同步。
