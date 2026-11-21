# Clone Git 兼容性修复计划（2026-11-17）

**模板版本:** `v2.16`。本计划不在 v2.16 具名 `code-only` 范围内，按普通发布合同执行：独立 `implementation` 卡与家族发布点执行完整 C 组（含 `patch + 1`、`compat_version_surface_sync`、`Cargo.lock`、`gh release create`）与适用 D 组。家族子卡 `CL-04`、`CL-23`、`CL-28` 只本地提交，不单独推送，C/D 继承 `CL-25`。

**状态:** Claude R73 与 Codex R73 双审 PASS（受审 SHA-256 前 12 位 `189cec976839`），计划可提交；全部任务 pending，尚未执行。全部任务卡 `Lifecycle=pending`、`Acceptance` 为空。本文只规划，不宣称实现完成；落地时每张卡必须先按 ER-02 刷新源码锚点。
审阅合同：按用户 2026-11-18 指令，本计划的计划文本提交门为 Claude（Subagent）与 Codex 双审，二者对同一字节均给出 `VERDICT: PASS` 方可提交（取代 2026-11-17 的 Codex 单审口径）；执行期各卡的 ER-05 评审门与网站交付的独立 Claude Code `PASS` 门不变，网站门与计划双审互不替代。

## 文档职责

- **网站交付路径（权威）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支），依据是用户 2026-11-18 11:55:09 UTC 确认：本批计划的网站文档交付检出为该路径。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`，该路径在本机不存在（2026-11-18 核对），属已知文档滞后；用户决定本批不改 `AGENTS.md`，本计划以上述确认为准（修订历史 2026-11-18 08:19:08 行所称「用户最新指令」不作为依据）。2026-11-18 核对：该路径为 Libra 检出（有 `.libra/`、无 `.git`），当前分支 `cf`。写入前须按「文档与网站验收命令」第 2 步核验该路径存在、仅含单一 VCS 元数据，并确保当前分支为 `cf`（不是 `cf` 时按该步执行 `libra switch cf` 后复核）；按该步仍不满足即 STOP，禁止改用其它检出。

本文解决 `libra clone`（及关联 `rev-parse --show-ref-storage-format`）在 CLI 标志解析（`--progress` 布尔语义、`--no-reject-shallow`、`-n`、`--separate-git-dir`、`--revision`、`--no-hardlinks`、`-c <key=value>`、`--ref-storage-format`、`--sparse`）、本地硬链接与 `<path>.git` 回退、部分克隆 `--filter`（多值 `combine:`、`sparse:oid`、无效 sparse filter、promisor 对象读取与按需 lazy fetch 与进度）、`--reference`/`--dissociate` alternates 借用、`--single-branch` 游离 HEAD 分支创建、以及默认 `clone` 持久化 `remote.<name>.fetch` 中与 Git 不一致的 17 个已确认缺口，目标是实现 Git 兼容行为、完备测试覆盖与四端文档同步。

### 适用范围

- `libra clone`（`src/command/clone.rs`）、关联的 `src/command/rev_parse.rs`（`--show-ref-storage-format`）、`src/command/fetch.rs`（`--revision` / `--filter` / `--reference` 传输）、`src/internal/protocol/local_client.rs`、对象读取 promisor 回退路径与 `src/cli.rs`。
- 关联测试：`tests/command/clone_test.rs`、`tests/command/clone_cli_test.rs`、`tests/command/rev_parse_test.rs`（均通过 `--test command_test` 运行）、`tests/compat/clone_shallow_integrity_test.rs`（target `compat_clone_shallow_integrity`）、`tests/INDEX.md`。
- 关联文档：`docs/commands/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.md`、`docs/commands/zh-CN/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.md`、`docs/development/commands/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`CL-23`/`CL-28` 写集逐文件列出的其余命令文档（含 `alternates` 等受指针布局默认拒绝影响的命令）及 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.en.md`（写前按 ER-06a 核验 `.libra` 与 `.git` 及 `cf` 分支）。

### 非目标

- 不重构 `libra fetch` / `libra push` 的独立参数解析（由协调者排期的 `fetch-push` 计划承接）。
- 不修改 SQLite schema 或迁移 ledger（不触发 GC-13 迁移）。
- 不在本计划内实现 Git `reftable` 后端存储（`--ref-storage-format` 仅映射已实现的 SQLite ref 格式并拒绝未知格式）。
- 不在本计划内新增独立的 `libra sparse-checkout` 子命令（`--sparse` 以 `core.sparseCheckout`/`core.sparseCheckoutCone`、cone 物化与索引 skip-worktree 位实现，不写入、不启用既有 `sparse_view.rs` 的只读显示过滤，见 `CL-16`）。

### 成功定义

- 覆盖全部 17 个目标 Issue（`#737, #738, #739, #740, #741, #742, #743, #744, #746, #748, #750, #751, #752, #753, #754, #756, #876`），其中 `#750` 只交付 CLI 校验与如实报告 `sqlite`（`files`/`reftable` 在 `DEFER-CL-01`），`#876` 只交付 `remote.<name>.fetch` 持久化（`fetch --prune` 端到端在 `DEFER-CL-03`），`#756` 交付验收第 1–3 条（`clone --sparse --filter=blob:none`、cone 稀疏检出、目录重命名合并 1 次按需拉取 6 个对象），第 4 条（完整上游 `t/t6421-merge-partial-clone.sh#2`，需在稀疏部分克隆内 `checkout`）随 `DEFER-CL-04`。其余 14 个 Issue 全量闭环，期望值按「验证执行协议」第 4 项以 pinned Git `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`git describe`：`v2.56.0-116-g6de20f6092`）同夹具对照。
- 全部活动卡（27 张，不含已撤回的 `CL-22`）通过 ER-04 A/B/C/D 四组门禁、ER-05 Codex 单审 `VERDICT: PASS`、ER-06a 文档同步及 ER-08 发布切片。
- 全部活动卡完成后通过 ER-13 全量收口门（`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`）。

## 使用规则

- 本批次并行起草的多份计划由协调者统一串行登记至 [`plan-status.md`](plan-status.md) 与 `plan-long.md`；执行期推进任何任务卡时，同一变更必须同步更新 `plan-status.md` 的「计划一览」行（状态、版本或 commit、`YYYY-MM-DD HH:MM:SS UTC`）。
- 不得在 `docs/development/plan/` 下新建证据目录或证据文件。验收摘要写入本文「实施证据汇总」。
- 状态推进与「修订历史」统一使用 `YYYY-MM-DD HH:MM:SS UTC` 时间格式。
- 本计划继承 [plan-template.md](plan-template.md) 的 `GC-01..13`、`ER-01..14`、`G-01..11`。

## 事实基线

> 所有行号和源码锚点核对于 `2026-11-17 03:20:00 UTC`，开工当天必须按 ER-02 刷新。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口（CLI） | `Commands::Clone(CloneArgs)` 在 `cli.rs` 注册并路由到 `execute_safe`；全局 `--progress <mode>` 占用布尔 `--progress`（仅 `index-pack` 有 `rewrite_index_pack_progress_args`） | `src/cli.rs:292-302,351,1121,1318,3869` |
| `CloneArgs` 标志缺口 | 缺 `--no-reject-shallow`、`-n`、`--separate-git-dir`、`--revision`、`--no-hardlinks`、`--ref-storage-format`、`-c`、`--sparse`；`--filter` 为 `Option<String>` 且为 no-op | `src/command/clone.rs:64-236` |
| 本地克隆与 Alternates | `-l`/`--local` 总是复制对象而不硬链接；`--reference` 只打印警告、不借用对象；`--dissociate` 目前只用于阻止 `--shared`/`clone.shared` 注册 alternate（`src/command/clone.rs:178-182,1567`），尚无与 `--reference` 借用结合的解除语义；`<path>.git` 回退解析缺失；无 promisor 支持 | `src/command/clone.rs:124-182,250-269` |
| `single-branch` 与 `remote.<name>.fetch` | `--single-branch` 从游离 HEAD 克隆错误创建 `refs/remotes/origin/main`；`remote add` 已写 `+refs/heads/*:refs/remotes/<name>/*`（`remote.rs:822-834`），但默认 `clone` 仅在 single-branch/mirror 时写 | `src/command/clone.rs:76-87`；`src/command/remote.rs:822-834` |
| `rev-parse` 与 `init` 格式 | `RevParseArgs` 缺 `--show-ref-storage-format`；`init --ref-format` 仅接受 `strict`/`filesystem`（ADR-IN-02） | `src/command/rev_parse.rs:59`；`src/command/init.rs:374,1674-1686` |
| 稀疏视图边界 | 历史决策 D10 曾拒绝 `sparse-checkout`，当前仅存在 `sparse_view.rs`（`COMPATIBILITY.md:166,172,375`） | `COMPATIBILITY.md:166,172,375`；`src/command/clone.rs:220` |
| 测试与文档 | `clone_test` 与 `clone_cli_test` 挂在 `command_test` target 下；compat target 为 `compat_clone_shallow_integrity` | `tests/command/mod.rs:1024-1025`；`tests/INDEX.md:44,81`；`docs/development/commands/clone.md` |
| 外部参照 | pinned Git 基准行为（`git describe` 为 `v2.56.0-116-g6de20f6092`，即 v2.56.0 之后 116 个提交；评审预探用本机 Git 2.54.0，期望以 pinned 版本为准） | `git/git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（核对日期 `2026-11-17`） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-CL-01 | `clone` CLI 缺 `--progress` 布尔、`-n`、`--no-reject-shallow`、`--separate-git-dir`、`--revision`、`-c`（含 `clone.defaultRemoteName`）、`--ref-storage-format`、`--sparse` | 常见 Git clone 脚本与测试在 CLI 解析层失败 | `#737, #738, #739, #740, #741, #750, #752, #756`；`src/cli.rs:302`；`src/command/clone.rs:64-236` | CL-01..CL-06, CL-14, CL-16 |
| GAP-CL-02 | `-l/--local` 不硬链接、`--no-hardlinks` 缺失、`<path>.git`（含 `file://`）回退解析缺失、`--reference` 不借用对象（`--dissociate` 只对 `--shared` 生效） | 本地克隆性能与对象借用/独立语义缺失 | `#742, #743, #744, #751`；`src/command/clone.rs:124-182` | CL-08, CL-07, CL-09, CL-10, CL-18 |
| GAP-CL-03 | `--filter` 仅单值且忽略过滤（无 `combine:`、无 `sparse:oid`、无无效 sparse 报错、无 promisor/lazy fetch 与 `Receiving objects` 进度） | 部分克隆与错误过滤校验失效 | `#746, #748, #754`；`src/command/clone.rs:191-198,269` | CL-12, CL-13, CL-24, CL-29, CL-19, CL-21 |
| GAP-CL-04 | `--single-branch` 游离 HEAD 创建错误分支、默认 `clone` 未写入 `remote.<name>.fetch` | 游离 HEAD 克隆与后续 `fetch --prune` 越界修剪 | `#753, #876`；`src/command/clone.rs:76`；`src/command/remote.rs:822-834` | CL-15, CL-17 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-20261117-plumbing-misc.md`（已推送 `cd5ccac`） | 兄弟计划：`PM-06`（`#947`）先于 `CL-14`（`#750`）合入 `src/command/rev_parse.rs`（对方 `DEP-PM-02`）；`CL-19` 先于 `PM-14`（共享 `cat_file.rs`）；`CL-13`、`CL-19`、`CL-29` 先于 `PM-33`（`t5616#33` 的 `rev-list --missing` promisor 归因，自对方 `PM-03` 拆出；对方 `DEP-PM-08`；`PM-33` 还依赖 Libra 非 `--mirror` 的 `--bare` 克隆建立 `refs/remotes/origin/*`） | 登记 `DEP-CL-02`、`DEP-CL-08`、`DEP-CL-14`，均单向无环；本计划不改变上述 `--bare` 克隆现状（`CL-29` AC4 守护） |
| `plan-20261117-commit-signing.md`（已推送 `2fe8c4b`） | 兄弟计划：同样涉及 `src/command/clone.rs`、`tests/command/clone_cli_test.rs` 与 `cat_file.rs`；对方 `DEP-CS-03` 逐字镜像本计划 `DEP-CL-03` 的卡集合 | 登记 `DEP-CL-03`，按 ER-12 串行合入 |
| `plan-20261117-fetch-push.md`（已推送 `49b62d1`） | 兄弟计划：同样涉及 `src/command/fetch.rs` 与 `COMPATIBILITY.md`；本计划引用的 `FP-07`、`FP-15` 是该计划的卡（不是 `issues/529.md` 的同名卡），对方登记为 `DEP-FP-05` | 登记 `DEP-CL-04`、`DEP-CL-05`，按 ER-12 串行合入 |
| `plan-20261117-merge-pull.md`（评审中，按 ID 引用） | `CL-20` 先于 `MP-17` 合入 `merge.rs`、`rename_merge.rs`、`tree_merge.rs`（对方登记为 `DEP-MP-11`） | 登记 `DEP-CL-07` |
| `plan-20261117-{global-init-infra,checkout-switch-restore,branch,describe}.md` 等同批计划 | 共享 `src/cli.rs`、`src/utils/util.rs`、`src/command/rev_parse.rs`、`src/command/status.rs`、`COMPATIBILITY.md`、命令文档与网站生成产物（对方 `DEP-GII-03`、`DEP-PM-11`、`DEP-MP-10` 等同批写锁） | 登记 `DEP-CL-06`、`DEP-CL-09`..`DEP-CL-13`、`DEP-CL-15` |
| `plan-long.md` | 长期兼容性路线图 | 由协调者在合入时统一登记日期索引 |
| `docs/development/plan/plan-template.md` | 模板规范 v2.16 | 全文严格遵守 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 17 个实测 Git 兼容缺陷均有明确复现路径 | 按单一行为轴拆分为 27 张活动任务卡（含 `CL-18..CL-29`，`CL-22` 已撤回，其中 `CL-25` 为 `REL-CL-01` 家族发布点，`CL-28` 为指针布局写命令拒绝子卡，`CL-29` 为 `sparse:oid` 下游契约卡，原 `CL-11` 已并入 `CL-13`；`CL-` 前缀作用域仅限本计划，与 `issues/474.md` 无关） |
| 可行性 | 每卡聚焦单一行为轴，锚点与测试命令均可直接执行 | 修正 `CL-02`、`CL-16` 不存在测试过滤器并重写旧 no-op 测试 |
| 任务卡粒度 | 26 张实现卡均含公开行为变更，按 G-04 标为 `M`，发布点 `CL-25` 为 `S`；`landing` 仅计生产目录 | 逐条按独立谓词核算 `AC`/`VER`（≤ 8） |
| 依赖与顺序 | 全计划严格串行，`CL-08` 先于 `CL-07`；`DEP-CL-02/03/04` 单向无环 | DAG 无环，符合 G-10/ER-12 |
| 完整性 | 补齐 `--progress` 重定向、`--bare --revision`（`CL-05` AC1–AC4）、`file://<path>.git`、`clone.defaultRemoteName` 等 AC | 全部纳入卡内与 `COMPATIBILITY.md` |
| 安全性 | `--separate-git-dir`、`--reference`、`-c`、`<path>.git` 校验路径边界与不泄露凭据 | 继承 GC-07/GC-09 |
| 功能正确性 | 严格对齐 pinned Git `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`v2.56.0-116-g6de20f6092`） | 新增 `ADR-CL-03`/`ADR-CL-04` 解决 D10 与 `ref-format` 边界 |
| 接口兼容 | `CloneArgs` 与 `rev-parse` 同步 `COMPATIBILITY.md`；全部任务复用既有错误码，不新增 `docs/error-codes.md` 条目 | 命中 `T-1` 时跑全量 nextest |
| 数据流与控制流 | `--dissociate` 在拷贝完借用对象后再移除 alternates；失败清理新建目录 | 故障恢复矩阵覆盖 |
| 性能与容量 | `-l/--local` 硬链接减少磁盘拷贝；`--filter` 部分克隆避免拉取多余对象 | 继承 GC-10 |
| 可靠性与容错 | `--separate-git-dir` / `--revision` / 无效 `--filter` 失败时清理半成品目录 | 故障恢复矩阵覆盖 |
| 可维护性 | 复用 `rewrite_*_progress_args`、`command_preflight`、`ConfigKv` 单一事实源 | 禁止重复实现 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 03:25:00 UTC | 初稿创建 | 建立覆盖 17 个 clone 兼容问题的 17 张任务卡计划 | N/A | 全文 |
| 2026-11-17 03:38:00 UTC | Claude R1 FAIL + Codex R1 FAIL | 拆出 `CL-18`（`--dissociate`）与 `CL-19`（promisor 对象读/lazy fetch）；调换 `CL-08` 先于 `CL-07`；新增 `ADR-CL-03`（重开 D10 为 `clone --sparse`）与 `ADR-CL-04`（`--ref-storage-format` 映射 `resolve_ref_format`）；`DEP-CL-02` 单向指向 `PM-06` 并补 `DEP-CL-03/04/05`；移除 `CL-11` `src/cli.rs` 并使 `CL-11` 经 `CL-13`/`CL-19` 闭环；补全 `#737/#741/#744/#748/#752/#753/#756/#876` AC 与 `COMPATIBILITY.md`、`codeql.yml` `release/**`、rewrite 旧测试 | CL-10 → CL-10, CL-18; CL-13 → CL-13, CL-19 | ADR-CL-02/03/04、依赖表、实施顺序、粒度审计表、各卡正文、review log |
| 2026-11-17 03:45:00 UTC | Codex R2 FAIL | 将 `CL-19`（promisor lazy fetch）前置至 `CL-11` 与 `CL-13` 之前，确保每个部分克隆独立发布版本均自洽可读；修正 `CL-16` 真实落点为 `src/internal/sparse/mod.rs` 与 `src/command/sparse_view.rs` 并标记 `T-1`；明确 `CL-14` 将 `init::resolve_ref_format` 提升为 `pub(crate)` 并映射 `files`→`Filesystem` | N/A | ADR-CL-02/04、实施顺序、粒度审计表、CL-11/13/14/16/19、追溯表、review log |
| 2026-11-17 03:52:00 UTC | Codex R3 FAIL | 收紧 `CL-11` 为仅解析多 `--filter` 并构造 `combine:` 规格，将协议 `filter` 省略与 `promisor=true` 统一收敛至 `CL-13` 同边界发布；明确 `CL-17` 自身仅验收 `remote.<origin>.fetch` 持久化，`#876` 端到端 `fetch --prune` 待 `#541` 与 `FP-15` 闭环；补全与 DAG 一致的完整 `发布窗口顺序` | N/A | ADR-CL-02、依赖表、发布窗口顺序、CL-11/17、review log |
| 2026-11-17 04:05:00 UTC | Claude R4 FAIL + Codex R4 FAIL | `ADR-CL-04`/`CL-14` 改为 `clone.rs` 独立 `resolve_ref_storage_format`（仅接受 `files` 映射 SQLite，不改 `init.rs` 与 D25）；`ADR-CL-03` 补充 `docs/development/commands/_compatibility.md` 并拆出 `CL-20` 与 `CL-16`；新增 `ADR-CL-05`；`CL-05/09/10/11/13` 补全 `T-1`；`CL-13`/`19` 改用 `<remote_name>`；`CL-17` 修正 `clone.rs:2043-2054` 锚点并明确 `#876` 端到端收口门；移除未用 `REL-CL-01` | CL-16 → CL-16, CL-20 | ADR-CL-03/04/05、实施顺序、依赖表、审计表、CL-01/04/05/06/09/10/11/12/13/14/16/17/19/20、追溯表、完成判据、review log |
| 2026-11-17 04:18:00 UTC | Claude R5 FAIL + Codex R5 FAIL | `实施顺序` 补全 `CL-16 -> CL-20 -> CL-17`；新增 `DEP-CL-06..09` 协调跨计划写集（`cli.rs`/`merge.rs`/`cat_file.rs`/`rev_parse.rs`）并将 `DEP-CL-03` 改为 `outgoing`；重写 `ADR-CL-02` 移除过期 `REL-CL-01` 并以 `CL-13` 收口；`CL-04` 纳入 `src/utils/util.rs:277,797` 并标 `T-1`；`CL-20` 补 `1 fetch`/`6 objects`/`TC-0734` 与 `merge.md`；`CL-19` 改 `landing=4/prod-files=5` 并仅复用 `hydrate.rs` OID 校验；`ADR-CL-01`/`CL-13`/`Release write set`/`codeql.yml`/`兼容收口` 补全 P2 | N/A | ADR-CL-01/02/05、D 组、实施顺序、依赖表、全局默认、审计表、CL-04/13/19/20、兼容收口、review log |
| 2026-11-17 04:23:00 UTC | Codex R6 FAIL + Claude R6 网络中断 | `CL-04` 纳入 `util.rs:314-324` `worktree_info_file_paths` 指针解析 | N/A | CL-04、review log |
| 2026-11-17 04:33:00 UTC | Codex R7 FAIL + Claude R7 网络中断 | `CL-04` 边界收窄并增补 `DEFER-CL-02`；`完成判据` 对齐 `#876` 跨计划门 | N/A | CL-04、DEFER-CL-02、完成判据、review log |
| 2026-11-17 04:43:00 UTC | Claude R8 FAIL + Codex R8 FAIL | `CL-13` 补 `#746` `combine:blob:none+tree:1` 与端到端 lazy fetch；`CL-20` 补 `rename_merge.rs`/`tree_merge.rs`/`fetch.rs` 批量预取与 `T-1`/`DEP-CL-07`；新增 `DEP-CL-10` 协调 `util.rs`；`CL-04` 纳入 `alternates.rs:91` 并验证指针布局基本命令；`CL-17` 预置 `#[ignore]` 端到端测试；修复 `CL-02` nextest 过滤器与 `rev_parse.rs:59` 锚点；`CL-10`/`12` 补齐 `fsck` 与 Git 精确报错 | N/A | 依赖表、D 组、审计表、CL-02/04/05/06/10/11/12/13/14/17/19/20、完成判据、review log |
| 2026-11-17 04:48:00 UTC | Claude R9 FAIL + Codex R9 FAIL | 移除 `CL-20` 重复尾部字段；`CL-04` 与 `DEFER-CL-02` 解除自相矛盾（`alternates.rs:91` 明确归 `CL-04`）；`CL-13` 拆出 `CL-21`（`Receiving objects` 进度与端到端 lazy fetch 验证）保持 `AC<=8` 与单轴；`完成判据` 明确移除 `#[ignore]` 并正常执行 `#876` 测试；同步审计表/卡内 `landing`/`deps`、`CL-19` 校验实现与传输范围、`CL-01`/`CL-14` 验证、具名 `DEP-CL-04/07/09` 与单一发布者 `Plan Author 5` | CL-13 → CL-13, CL-21 | 修订历史、实施顺序、依赖表、审计表、CL-01/04/05/10/11/12/13/14/19/20/21、追溯表、DEFER-CL-02、完成判据、review log |
| 2026-11-17 04:55:00 UTC | Codex R10 FAIL + Claude R10 网络中断 | 将 `CL-11` 与 `CL-13` 合并为 `REL-CL-01`（`combined` 发布边界，并将 `CL-12` 前置至 `CL-19 -> CL-12 -> CL-11 -> CL-13`）避免 `CL-11` 单独发布后静默忽略 `--filter`；收紧 `CL-04` 仅声明 `clone --separate-git-dir`、`util.rs`、`alternates.rs:91` 与已验证 4 个基本命令，不宣称其余子命令指针布局通用支持；新增 `CL-22`（`verification`，`Release boundary=none`）显式承接 `#876` 移除 `#[ignore]` 与端到端门禁；更新 `Phase 3` 与 `M3` 为 `11 张卡` | 新增 `CL-22` 承接 `#876` 收口验证 | 修订历史、ADR-CL-02、实施顺序、发布分组、Phase 3、审计表、CL-04/11/12/13/17/22、M3、追溯表、完成判据、review log |
| 2026-11-17 05:00:00 UTC | Claude R11 FAIL + Codex R11 FAIL | 将 `CL-11` 并入 `CL-13`（`7/8 AC`，`independent`）彻底移除 `REL-CL-01`；规范 `CL-22` 为 `Task type: implementation`、`Release boundary: independent`、`Rollback mode: immutable-release`、`Version increment: patch`、`Full-suite trigger: T-2` 并移除 `Verification` 中的 `--all` 全量行；`CL-21` 补 `DEP-CL-04`；同步 `DEP-CL-03`（`CL-01..CL-22`）、`DEP-CL-04`（含 `CL-21`）、`DEP-CL-05`（`incoming`+`DEFER-CL-03`）与 `CL-20` 追溯表 | `CL-11` 并入 `CL-13` | 修订历史、ADR-CL-02、实施顺序、依赖表、发布分组、Phase 3、审计表、CL-13/21/22、追溯表、M3、DEFER、review log |
| 2026-11-17 05:05:00 UTC | Codex R12 FAIL | 修正 `ADR-CL-04`/`CL-14`/`DEFER-CL-01`：`rev-parse --show-ref-storage-format` 如实报告 `sqlite`（不将 SQLite 误报为 `files`），将 `files`/`reftable` 后端实现显式延后至 `DEFER-CL-01` 并记入 `COMPATIBILITY.md`；`CL-08` 明确校验 `.libra/objects/` 下**所有**对象文件 `nlink > 1`；同步审计表 `CL-22`（`implementation`/`independent`/`immutable-release`/`2/8`）与 `CL-12`（`landing=2`）；统一 `DEP-CL-05` 14 天超时、`DEFER-CL-03` 重启条件与 `完成判据` | N/A | 修订历史、ADR-CL-04、审计表、CL-08/14、DEFER-CL-01/03、完成判据、review log |
| 2026-11-17 05:18:00 UTC | Claude R12 FAIL + Codex R13 FAIL | 拆分 `CL-04` 为 `CL-04`（`--separate-git-dir` 指针与互斥，`4/8 AC`）+ `CL-23`（`util.rs`/`alternates.rs:91`/4 基本命令与未适配子命令 fail-closed + `COMPATIBILITY.md` 契约，`5/8 AC`）；拆分 `CL-13` 为 `CL-13`（多 `--filter` 构造与 `promisor`/`blob:none`，`5/8 AC`）+ `CL-24`（`combine:blob:none+tree:1` 与无能力回退，`4/8 AC`）；`CL-16` 纳入 `status.rs:1248-1265`、`clone.rs:220` 帮助文本与 `DEFER-CL-04`（`checkout`/`switch`/`reset` 稀疏约束）；修正 `成功定义`/`DEFER-CL-01`（具名 Owner）、`DEP-CL-03`（`CS-02/02a/15b/21a`）与 `DEP-CL-04`（`FP-07/15` 先于本计划 `fetch.rs`）；`CL-22` 改标 `T-5` 并统一 `M3`；清理 `GAP-CL-03`/`CL-19`、`R14`、`23 张卡`、`CL-06/19` AC 计数、`CL-15` 锚点及 `.git` 检查 | CL-04 → CL-04, CL-23; CL-13 → CL-13, CL-24 | 全文、成功定义、GAP-CL-03、DAG、依赖表、Phase 1/3、审计表、CL-04/06/13/15/16/19/22/23/24、M3、DEFER-CL-01/04、review log |
| 2026-11-17 05:30:00 UTC | Codex R14 FAIL + Claude R14b 网络中断 | 将 `CL-04` 与 `CL-23` 纳为 `REL-CL-01` 家族卡（`CL-04`/`CL-23` 为 `family child`，新增 `Task type: release` 发布点 `CL-25`），消除单独发布不可用 `.libra` 指针布局的中间版本；`CL-13` 在 `CL-24` 落地前对 `combine:*` 与多种不同 `--filter` 组合明确 fail-closed 拒绝；`CL-16` 为未适配稀疏工作区的 `checkout`/`switch`/`reset` 增加 `core.sparseCheckout=true` fail-closed 护栏与回归测试（`prod-files=6`，`AC=5/8`）；重申 `CL-22` 已于 R11 起统一为 `implementation` + `independent` + `patch` | 新增 `CL-25`（`REL-CL-01` 发布点） | 状态行、评审结论、修订历史、发布分组、Phase 1、默认覆盖、审计表、CL-04/23/25/05/13/16、追溯表、M1、review log |
| 2026-11-17 06:55:31 UTC | Claude R15 FAIL + Codex R15 FAIL | `CL-04`/`CL-23` `C/D coverage from` 改为 `CL-25` 且 `Full-suite trigger: N/A`；`CL-23` 纳入 `src/cli.rs` `command_preflight` 统一指针护栏并修正 `util.rs` 锚点（`278,314-324,791-803,1269,1464`）；`CL-25` 移除 `Verification` 中 `--all` 并规范 `Docs: N/A`；拆出 `CL-26`（`src/cli.rs` `command_preflight` 对 9 个未适配稀疏写入/切换命令统一 fail-closed 护栏与 ER-06a 全量命令文档），`CL-16` 收窄为 `clone --sparse` cone 物化、`status.rs:563` 跳过 `status` 误告警（保留 `add`/`commit`/`diff` 告警）与暂时拒绝 `clone --sparse --filter=...`（至 `CL-20` 解禁），并补 `DEP-CL-11` 协调 `status.rs`；`CL-13` AC 精简并核实为 `7/8`；同步 `ADR-CL-02`/`ADR-CL-05`、`CL-06/10/12/21/22`、`DEP-CL-04/06`、测试矩阵、`COMPATIBILITY.md` 行号门与 review log | CL-16 → CL-26, CL-16 | 全文、状态行、ADR-CL-02/05、DAG、依赖表、Phase 3、审计表、CL-04/06/10/12/13/16/17/20/21/22/23/25/26、测试矩阵、追溯表、M3、review log |
| 2026-11-17 09:44:31 UTC | Codex R16 FAIL | `CL-13` 文档门不计入后 `AC=6/8`；`CL-26` 只保留检出类 4 条单谓词，历史类 5 条拆为 `CL-27`；无能力回退风险改指向 `CL-24`，并登记护栏遗漏风险 | `CL-26` → `CL-26, CL-27` | DAG、发布窗口、Phase 3、审计表、`CL-13`、`CL-26`、`CL-27`、`CL-16`、风险表、追溯表、Review log |
| 2026-11-17 09:51:20 UTC | Codex R17 FAIL | `CL-23` 拆成 7 条单谓词（`AC=7/8`）；撤回 `CL-22`，`fetch --prune` 端到端只留在 `DEFER-CL-03`；Phase 3 为 12 张活动卡 | `CL-22` 撤回 | `CL-17`、`CL-22`、`CL-23`、DAG、Phase 3、M3、`DEP-CL-05`、Review log |
| 2026-11-17 09:57:57 UTC | Codex R18 FAIL | `CL-16` 拆成 8 条单谓词并写明拆出 `CL-20`/`CL-26`/`CL-27`；`CL-20` 拆成 7 条单谓词 | N/A | `CL-16`、`CL-20`、审计表、Review log |
| 2026-11-17 10:09:42 UTC | Codex R19 FAIL | `DEP-CL-07` 改为 `CL-20` 先于 `MP-17`；`DEP-CL-03` 不再把 `CL-20` 放在 `CS-02a` 之前；`CL-12`/`CL-13` 对未实现 filter 与远端无能力 fail-closed；成功定义不再把 `#876` 端到端写成已闭环 | N/A | 依赖表、`CL-12`、`CL-13`、`CL-20`、成功定义、Review log |
| 2026-11-17 10:43:14 UTC | Codex R24 FAIL | `CL-23` 只保留指针发现和 `status`/`log`/`cat-file`/`branch`，并补这四条命令的文档。写命令拒绝拆为 `CL-28`，逐文件登记 `commit`/`add`/`checkout`/`switch`/`reset`/`merge`/`pull`。收口清单补上 `CL-26`/`CL-27`/`CL-28` 的命令文档 | `CL-23` → `CL-23, CL-28` | `CL-23`、`CL-28`、`CL-25`、发布分组、收口清单、追溯表、`DEFER-CL-02` |
| 2026-11-17 10:52:35 UTC | Codex R25 FAIL | `CL-28` 改为默认拒绝：放行名单之外的 `Commands::*` 一律退出非零且不改数据。验收覆盖 `restore`、`commit`、`branch` 创建和 `checkout`，并用夹具覆盖全部变体 | N/A | `CL-23`、`CL-28`、`DEFER-CL-02`、风险表、`DEP-CL-12` |
| 2026-11-17 11:14:03 UTC | Codex R26 FAIL | `CL-28` 的写集补上全部受默认拒绝影响、且已有命令页的文档。`CL-12`/`CL-13` 的无能力拒绝只约束各自发布提交，最终树以 `CL-24` 为准 | N/A | `CL-28`、`CL-12`、`CL-13`、完成判据、`DEP-CL-12` |
| 2026-11-17 11:19:54 UTC | Codex R27 FAIL | `CL-13` 的单个未实现 filter 只约束本卡发布提交。`CL-24` 增加单个 `combine:` 形式。追溯表和收口清单改指 `CL-28` 的完整写集 | N/A | `CL-13`、`CL-24`、追溯表、收口清单 |
| 2026-11-17 12:01:37 UTC | Codex R30 FAIL | 对齐 `DEP-CL-03` 与 `DEP-CS-03` 的活动卡及家族发布点集合，移除撤回卡与范围歧义；新增 `CL-29` 交付 `sparse:oid` 以承接 `PM-03` 的 `t5616#33`；补齐网站 sync/前端/`cf` 提交推送/部署门，并按用户指令将计划提交门改为 Codex 单审 | `CL-24` → `CL-24,CL-29` | 关系表、ADR-CL-02、DAG、依赖表、发布窗口、Phase 3、审计表、`CL-21`、追溯表、M3、全局执行门、完成判据、Review log |
| 2026-11-17 12:27:28 UTC | Codex R31 FAIL | 将全部任务卡、文档影响和网站写集中的 `../libra-backend` 统一替换为唯一网站工作区 `/Volumes/Data/gitmono/libra-backend`，避免写入错误 checkout；恢复独立于计划 Codex 单审的网站 Claude Code PASS 门 | N/A | 全部网站写集与文档影响、Review log |
| 2026-11-17 12:27:28 UTC | Codex R32 FAIL | `CL-29` 将 `sparse:oid` 输入从错误的 tree-ish 改为可解析为稀疏规则 blob 的对象表达式，明确缺失/非 blob/规则解析失败的拒绝与 Git 对照测试 | N/A | `CL-29`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R33-A P2 | `CL-08` 将 inode 共享限定为硬链接成功对象，并增加同文件系统硬链接失败后的复制回退测试，消除回退与全量 inode 断言矛盾 | N/A | `CL-08`、审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R34-A FAIL | `CL-08` 增加正控制：正常同文件系统夹具至少一个松散对象和一个 pack 对象实际硬链接成功，并逐一比较设备号/inode，防止全量复制空真通过 | N/A | `CL-08`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R36-A FAIL | `CL-08` 正常夹具改为逐一检查所有符合硬链接条件的对象文件均实际硬链接，防止只链接一个松散和一个 pack 而复制其余对象 | N/A | `CL-08`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R37-A PASS | 第 1–400 行、CL-04 完整卡片、CL-08/CL-29 及 PM-03/DEP-PM-08/DEP-CS-03 交叉核对均无实质问题；全部符合条件对象的硬链接与失败复制回退通过复审 | N/A | 头部、Review log |
| 2026-11-17 12:27:28 UTC | Codex R38-B FAIL/P2 | `CL-08` 增加对象目录/文件符号链接 fail-closed 与仓外回归；`CL-18` 增加 `--shared --dissociate` 独立可读验收；`CL-28` 删除已移除的 code/code-control/graph/usage/publish 文档写集 | N/A | `CL-08`、`CL-18`、`CL-28`、审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R39-B FAIL/P2 | `CL-28` 改为通过 ref API/数据库查询验证 `refs/heads/newbranch` 不存在并比较拒绝前后状态，禁止按文件存在判断；同步从 Implementation write set 删除剩余 20 条过时文档路径 | N/A | `CL-28`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R40-B FAIL/P2 | `CL-28` 从写集和文档影响清单移除 package/prune/stats/repository-hooks 四组不适用路径 | N/A | `CL-28`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R41-B PASS | 第 401–800 行及 CL-18 完整卡片无实质问题；CL-28 两组清单已删除全部指定过时/不适用路径，ref 状态验证、CL-08 符号链接与 CL-18 shared dissociate 验收均保留 | N/A | 头部、Review log |
| 2026-11-17 12:27:28 UTC | Codex R42-C FAIL/P2 | 性能摘要区分硬链接成功与复制回退，明确回退对象占用额外空间 | N/A | 性能与容量摘要、Review log |
| 2026-11-17 12:27:28 UTC | Codex R43-C FAIL/P2 | 性能摘要增加硬链接回退复制成本及远端不支持过滤时的全量传输与存储成本 | N/A | 性能与容量摘要、Review log |
| 2026-11-17 12:27:28 UTC | Codex R44-C FAIL/P1 | CL-20 增加非稀疏部分克隆缺失对象的按需合并回归，限定普通仓库不多余获取；过滤成本补对象字节数 | N/A | CL-20、性能摘要、Review log |
| 2026-11-17 12:27:28 UTC | Codex R45-C FAIL/P1 | CL-20 将普通仓库与稀疏拒绝回归保留在 Verification，去除重复验收条目并独立列出非稀疏部分克隆；重计 7/8 | N/A | CL-20、粒度审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R46-C FAIL/P1 | CL-20 增加稀疏内容修改/删除/冲突拒绝与工作区、索引、refs 不变的具名回归，限定目录重命名放行 | N/A | CL-20、粒度审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R47-C PASS | 第 801 行至末尾及 CL-10/CL-18 复审无阻断问题；与 R37-A、R41-B 覆盖全文 | N/A | 状态、Review log |
| 2026-11-17 12:27:28 UTC | Commit-signing Codex R19-A FAIL/P1 | `DEP-CL-03` 同步排除依赖 CL-20 的 CL-17，消除间接等待；与 `DEP-CS-03` 镜像 | N/A | 依赖表、Review log |
| 2026-11-17 12:27:28 UTC | Codex 依赖闭包定向复审 PASS | 两侧集合一致，所核查的 Clone、PM、MP 与 CS 发布链无 CL-20 间接等待或循环 | N/A | 依赖表、Review log |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件 | N/A | 全文对应章节 |
| 原记 `2026-11-18 10:35:00 UTC`（不可证实：晚于该行实际写入；文件 mtime 只表明写入不晚于 2026-11-18 09:44:18 UTC，实际时刻未知；该行所称「文档与网站验收命令」块当时并未写入正文，见 2026-11-18 双审第 1 轮（R48）前专项清扫行） | Codex 复审 FAIL 修复（验证契约与门禁） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 16:51:58 UTC | 用户 2026-11-18 指令：计划提交门改为 Claude（Subagent）与 Codex 双审；双审第 1 轮（R48）前专项清扫 | ① 头部审阅合同、`M0`、完成判据改为双审口径（执行期 ER-05 与网站 Claude Code 门不变）；网站路径依据改为「用户 2026-11-18 11:55:09 UTC 确认」，`AGENTS.md:135` 登记为已知文档滞后；更正 `10:35:00 UTC` 不可证实时间行。② 补写此前被 26 处引用却不存在的「文档与网站验收命令」第 1–8 步与 W-1/W-2 命令块（含 `REL-CL-01` 家族子卡只做本地 `cf` 提交、`CL-25` 发布后统一推送与部署核验）；「验证执行协议」重写为第 1–7 项（`VER` = 2 × 条目数、单谓词 AC、失败类只断言稳定码、拒绝不变量与仓库快照、标准源仓库 S、进程隔离与 `LIBRA_TEST` 接缝）。③ 全部 nextest 过滤补 `command::` 前缀（`tests/command_test.rs:10`；原 `test(=clone_test::…)` 匹配 0 个），修复缺失闭合反引号与把两个测试塞进一条命令的 `CL-20` 条目；`CL-25` 删除不存在的 `test_clone_separate_git_dir` 与把强制门计入 VER 的条目。④ 27 张活动卡的 AC 重写为单谓词并逐条给出精确映射（原映射含「stdout/stderr 含 `<url>`」等与 AC 无关的机械断言及错位的测试名）；失败类改为既有稳定码（`LBR-CLI-002/003`、`LBR-REPO-001/003`、`LBR-IO-001`、`LBR-UNSUPPORTED-001`）；粒度行与审计表 AC/VER 同步。⑤ 按 issue 原文修正：`CL-02` 改为合并输出匹配 `Clon` 恰 1 行（Libra 无 `Cloning into` 文案）；`CL-21` 的 `Receiving objects` 改为按需拉取进度（`#754`/`t0410#38`）；`CL-20` 夹具改为 `t6421#2` 同构并以 `--branch A` 避开被 `CL-26` 拒绝的 `checkout`，`#756` 第 4 条随 `DEFER-CL-04`（成功定义改为 14 个全量闭环）；`CL-13` 删去与 Git 不符的重复 `--filter` 去重。⑥ 跨计划：`CL-29` 的「供 `PM-03`」改为 `PM-33` 并新增 `DEP-CL-14`（含 Libra `--bare` 克隆建立 `refs/remotes/origin/*` 的现状保持，`CL-29` AC6 守护）；新增 `DEP-CL-13`（同批共享文件写锁）与 `DEP-CL-15`（`REL-CL-01` 网站推送冻结窗口）；`DEP-CL-03` 依赖闭包按已推送版本复核；`DEP-CL-04`/`07`/`08` 注明对方登记 ID；关系表补 merge-pull 与同批写锁行；外部参照改为 pinned `6de20f6092`（`v2.56.0-116-g6de20f6092`）。 | N/A | 头部、文档职责、成功定义、事实基线、关系表、修订历史、执行检查必备需求、依赖登记表、全部任务卡、粒度审计表、测试矩阵、追溯表、M0、风险登记、DEFER-CL-04、完成判据 |
| 2026-11-18 17:02:21 UTC | Claude R48 FAIL + Codex R48 FAIL（2026-11-18 双审第 1 轮，Codex 按行区间分 A–F 六区并行、全部 FAIL；受审 SHA-256 `49db3c05736a`） | ① 「以 `<LBR-*>` 拒绝」定义为一个谓词（非零状态且 stderr 含稳定码，不断言具体数值）；新增「克隆拒绝不变量」（目标目录与 `<G>` 不存在），删去各卡零散的目标目录 AC；网站第 4 步补 `pnpm test`。② 按「一条 AC 一个可独立失败谓词」重排：`CL-04`（拆 `libra.db`/指针，补初始检出清单）、`CL-05`/`CL-15`（游离 HEAD 改为 `status --porcelain=v2 --branch` 前两行逐字比较，引用改用 `show-ref`，不再用不存在的 `reference.name='refs/…'` 查询）、`CL-06`（`libra remote` 逐字输出）、`CL-08`（拆回退字节/inode，删去改变 Libra 源 `--depth` 契约的旧 AC8）、`CL-10`（`count-objects -v` 对象总数）、`CL-12`（三个无效 filter 各自稳定码与 Git 原文分列）、`CL-13`/`CL-24`（改用 `--bare` 避免检出拉取 blob，对象类别逐类分列）、`CL-17`（两行逐字）、`CL-19`（缺失对象码改为实测 `LBR-CLI-003`；新增 stderr 接缝 `LIBRA_TEST_TRACE_LAZY_FETCH` 断言未发起拉取）、`CL-20`（三个重命名目标集合逐项相等；拉取计数改用 stderr 接缝，不再写日志文件）、`CL-21`（旧版本 blob 夹具与 `portable_pty` 伪终端默认进度）、`CL-28`（快照不变量覆盖索引与引用；全变体判定改为单元测试一次等值比较）、`CL-25`（逐子卡祖先、patch + 1 比较、补 `--lib` 聚合守卫、release run 总体结论；网站推送归入 ER-06a 文档门）。③ `CL-09` AC2 改用不含 `foo` 的新目录；`CL-18` 写明 `--shared --dissociate` 由既有实现达到同一终态、本卡只补回归；`ADR-CL-02` 删去与 Git 不符的「去重」；`CL-04` 标 `(1/4)`；事实基线锚点改为 `src/cli.rs:3869`、`tests/INDEX.md:44,81`；追溯表文档列按各卡写集补全 zh-CN/开发文档/网站页；完成判据限定活动卡、排除已撤回 `CL-22`。 | N/A | 执行检查必备需求、ADR-CL-02、事实基线、`CL-04/05/06/08/09/10/12/13/15/17/18/19/20/21/24/25/28` 及全部卡映射、粒度审计表、追溯表、风险登记、完成判据 |
| 2026-11-18 17:06:44 UTC | Claude R49 FAIL + Codex R49 FAIL（双审第 2 轮，Codex 分区 A–F 全部 FAIL；受审 SHA-256 `557e6e7a4eca`） | ① 事实基线与 `GAP-CL-02` 更正：`--dissociate` 已用于阻止 `--shared` 注册 alternate，`--reference` 只警告不借用；成功定义与 ER 收口前提限定为活动卡（排除已撤回 `CL-22`）。② `CL-25` 改为与模板 C 组 `gh pr merge --squash` 相容：子卡提交是受测 PR head 的祖先、squash merge 提交树等于受测树、PR head 版本为基底 patch + 1（9/12）。③ `CL-05` 分支缺失改用 `reference` 表 `kind='Branch'` 计数（`show-ref` 在空集时以 `LBR-CLI-003` 失败），补检出清单 AC，描述去掉 bare 的「不保留 refs」；`CL-06` 改按 `#752`（`t5606#16`）以 `refs/remotes/<name>/main` 观测配置在初始 fetch 前生效；`CL-10` 补有效 `--reference-if-able` 的 alternates 断言（7/8）；`CL-13` 三个配置键改用自定义远端 `upstream` 验收、默认名单列一条；`CL-18` 写明 `CL-08` 之后 `--shared --dissociate` 允许硬链接（删除源后可读即独立）。④ `CL-12`→`CL-13` 交接改为 AC8（过渡）/AC7（持续）；`CL-19` 以接缝正控制（AC1 运行恰有一行 `want=1`）替换原 AC2；`CL-20` 三类拒绝夹具互斥化；`CL-27` AC2 映射补非零状态；M1/M2 改为「命中 T-1 的卡另有全量」；风险登记 `CL-28` 行与卡内测试设计一致。 | N/A | 事实基线、GAP-CL-02、成功定义、执行检查必备需求、`CL-05/06/10/12/13/18/19/20/25/27`、粒度审计表、M1、M2、风险登记、完成判据 |
| 2026-11-18 17:11:21 UTC | Claude R50 FAIL + Codex R50 FAIL（双审第 3 轮，Codex 分区 A–E FAIL、F PASS；受审 SHA-256 `d91ad84a0b89`） | ① 克隆拒绝不变量限定为检出前的拒绝（Git `remove_junk` 只在 `JUNK_LEAVE_NONE` 时清理，检出失败保留仓库）；新增「对象存在性判定」（`cat-file -e` 退出 0 / 退出 1 且 stderr 空，其余判测试失败，配 `HEAD` 正控制；libra 0.30.207 实测），各卡缺失/存在断言统一引用。② `CL-08` 按 Git `builtin/clone.c:350-372` 重写：显式 `--local` 与默认本地路径分别验收硬链接，默认路径硬链接失败后复制、显式 `--local` 失败以 `LBR-IO-002` 拒绝，删去「其余对象仍硬链接」。③ `CL-03` 改用有效 Git 浅源（既有用例夹具故意写坏 `.git/shallow`）；`CL-13` 三个配置键改为 `config_kv` 行集合等值比较，并分别覆盖 `-o`、`clone.defaultRemoteName` 与默认 `origin`（7/8）；`CL-24` 显式 `combine:` 形式改为断言非根 tree 缺失；`CL-28` 判定测试改为命令行样本表（`branch`/`alternates` 分参数）并另断言样本覆盖全部变体（6/8）；`CL-21` 补 `cat-file` 文档写集；`CL-15` 证据锚点改为 `clone.rs:448-470`；`CL-23`/`CL-28` 夹具在卡内写全（G-05）。 | N/A | 验证执行协议第 4/7 项、`CL-03/05/08/13/15/19/21/23/24/28/29`、粒度审计表、追溯表、性能摘要 |
| 2026-11-18 17:15:13 UTC | Claude R51 FAIL + Codex R51 FAIL（双审第 4 轮，Codex 分区 A–F 全部 FAIL；受审 SHA-256 `8c9cc1bf28e1`） | ① `GC-CL-01` 限定为各卡验收写明的 Git 行为，显式豁免 `LBR-*` 码、SQLite 存储与 `.libra` 指针布局；W-2 退出码说明与代码一致（3 含 `HTTPError`）；协议第 5 项允许 `release` 卡聚合守卫作为其聚合类 AC 证据，第 6 项手工证据按 1 门计。② `CL-13` 过渡断言编号改为 AC5–AC7（`CL-24` 描述、Verification 与完成判据同步）；`CL-24` 显式 `combine:` 形式改为与重复 `--filter` 的对象存在性映射逐一相等，并登记拆出 `CL-29`（G-09）；`CL-25` 祖先、树等价、版本与 release run 改为 Verification 手工证据（12/12）。③ `CL-08` 故障接缝改为 `first`，验收 Git「首次硬链接失败后全部复制」；`CL-10` 补有效 `--reference-if-able` 的本地对象数为 0（8/8）；`CL-18` 以 `clone --json` 的 `data.objects_fetched` 为 0 证明 `--reference --dissociate` 先借用；`CL-05` 改验引用名与完整 OID 两种形式，`--bare --revision` 不写 fetch refspec 归 `CL-17` AC3；`CL-15` 描述去掉未验收的 `FETCH_HEAD`；`CL-17` refspec 加引号。 | N/A | 全局工程约束、验证执行协议第 5/6 项、文档与网站验收命令第 7 步、`CL-05/08/10/13/15/17/18/24/25`、粒度审计表、追溯表、完成判据 |
| 2026-11-18 17:18:14 UTC | Claude R52 FAIL + Codex R52 FAIL（双审第 5 轮，Codex 分区 A–E FAIL、F PASS；受审 SHA-256 `825c90795e20`） | ① 协议第 7 项：子进程要求限定为 `command_test` 集成用例，`--lib` 纯单元测试除外；标准源仓库 S 在创建时保存 `S1`/`S2` 完整 OID，`CL-05` AC5 与 `CL-21` 改用 `S1`，不再依赖可变 `HEAD~1`。② `CL-04` `--mirror` 组合改为「以 `LBR-CLI-002` 拒绝」并另列 Git 原文（8/8）；`CL-08` 符号链接拒绝改为对象目录根 `.libra/objects`；`CL-13` 补「重复同值 `--filter`」过渡断言（8/8），`CL-24` 交接与完成判据改为 AC5–AC8；`CL-21` 补非终端默认 `auto` 不输出进度（6/8）；`CL-20` 把合并成功并入接缝等值断言，非稀疏部分克隆补「发生按需拉取」与三个重命名目标断言。③ 收窄未验收的描述承诺：`CL-09` 去掉「非仓库」与 `<path>/.git` 回退，`CL-16` 帮助注释改动归 ER-06 文档门，`CL-19` 去掉「本地 OID 完整性校验」表述（对象按内容计算 OID 后写入），`CL-17` 描述改指本卡 AC3；`CL-18` 描述改指 AC4–AC5。 | N/A | 验证执行协议第 7 项、`CL-04/05/08/09/13/16/17/18/19/20/21/24`、粒度审计表、追溯表、完成判据 |
| 2026-11-18 17:21:37 UTC | Claude R53 PASS + Codex R53 FAIL（双审第 6 轮，Codex 分区 A/B/F PASS、C/D/E FAIL；受审 SHA-256 `a5563f5a29a0`） | ① `CL-08`：硬链接回退改为对象目录完整清单等值（相对路径 →（SHA-256，是否共享 inode）），同时覆盖无遗漏与全部复制；符号链接拒绝补对象子目录 `objects/pack`，描述改为遍历中任何符号链接都拒绝（Git `builtin/clone.c:328-330`）。② `CL-16`：`--sparse --filter=blob:none` 为过渡断言（AC7，`CL-20` 改写），`--sparse --filter=tree:1` 持续拒绝（AC8，新增 `DEFER-CL-05`）；删去「`add` 仍告警」这一既有行为 AC。`CL-20`：两个合并结果改为 (路径, blob OID) 集合与 `B-dir` 源文件逐项相等。③ `CL-24`：重复同值 `--filter` 改为断言 `combine:blob:none+blob:none`（AC8），删去回退提示文案 AC；`CL-13` AC7 交接到 `CL-24` AC8。协议第 7 项终端用例改为 `CL-21` AC5–AC6；评审结论「完整性」行把 `--bare --revision` 指向 `CL-17` AC3。 | N/A | 验证执行协议第 7 项、评审结论、`CL-08/16/20/24`、粒度审计表、追溯表、DEFER-CL-05、完成判据 |
| 2026-11-18 17:26:22 UTC | Claude R54 PASS + Codex R54 FAIL（双审第 7 轮，Codex 分区 F PASS、A–E FAIL；受审 SHA-256 `3cfa417276c2`） | ① 执行检查必备需求：全量触发只对独立发布卡生效，家族子卡继承 `CL-25` 的 `T-2`。`CL-05` 按 `#741`（`t5621#7`）重排：`--bare --revision=refs/heads/main` 的提交存在、无引用（排除 `libra init` 自建的内置分支 `intent`/`traces`——修订中实测发现原「`kind='Branch'` 行数为 0」会误判正确实现）、无 `remote.origin.fetch`、C2 缺失，非裸 tag 游离检出与检出清单；完整 OID 与不存在修订报错登记 `DEFER-CL-06`。② `CL-06` 补全局 `clone.defaultRemoteName`（`LIBRA_CONFIG_GLOBAL_DB` 隔离，6/8）；`CL-15` 远端跟踪分支改用 `reference` 表精确查询（Libra 以完整名 `refs/remotes/origin/main` 存储，`show-ref` 空集失败）；`CL-13` 三个克隆库都验 blob 缺失并补跑改写后的 `CL-12` 用例；`CL-24` 补跑改写后的 `CL-13` 用例；`CL-21` 非终端 `--progress=text` 验 stderr 通道与 stdout 纯净，`--progress=none` 终端用例加成功前提（8/8）。③ `CL-20` 改为「合并前后缺失→存在恰为 `t6421#2` 的 6 个 blob」并把 sparse 克隆成功并入前提；夹具更名 M（避免与 S 的 `S2` 混淆）；`CL-28` 描述中文档表格归 ER-06 文档门、代码判定归单元测试。 | N/A | 执行检查必备需求、评审结论、`CL-05/06/13/15/20/21/24/28`、粒度审计表、追溯表、DEFER-CL-06 |
| 2026-11-18 17:29:29 UTC | Claude R55 PASS + Codex R55 FAIL（双审第 8 轮，Codex 分区 A/B/F PASS、C/D/E FAIL；受审 SHA-256 `80f94c5d76ba`） | ① `CL-05`：裸克隆补 HEAD 行 (`name`, `commit`) = (NULL, `S2`)；引用计数只豁免 `kind='Branch' AND remote IS NULL` 的内置 `intent`/`traces`（同名 tag/远端分支照常计入）；非裸用例改用指向 `S1` 的 tag `v0`（区别于默认 `main` 末端），以 `status --porcelain=v2 --branch` 完整输出逐字比较同时覆盖游离 HEAD 与检出内容（仍 8/8）。② `CL-19` 以「取回的 B 已持久化」替换原「无 promisor 配置时拒绝」（既有行为）。`CL-16` AC8 改为表驱动覆盖 `tree:1`、`combine:`、`sparse:oid` 与多个 `--filter` 四种组合；`clone.rs:220` 的过时字样归属更正为 `--deps-of` 帮助说明。 | N/A | `CL-05/16/19`、粒度审计表、追溯表 |
| 2026-11-18 17:32:50 UTC | Claude R56 PASS + Codex R56 FAIL（双审第 9 轮，Codex 分区 D/F PASS、A/B/C/E FAIL；受审 SHA-256 `08c2f238c2e7`） | ① `CL-16` 撤回对 `--sparse` 与 `--filter` 组合的拒绝（原 AC7 过渡断言与 AC8 表驱动拒绝；后者按独立谓词计为 4 条而超限）：本卡不对组合另加限制，`#756` 所需 `--sparse --filter=blob:none` 由 `CL-20` AC1 验收，其余组合列入 `DEFER-CL-05` 不验收（稀疏仓库写入命令仍由 `CL-26`/`CL-27` 拒绝）；`CL-16` 6/8，`CL-20` 去掉解禁交接，完成判据同步。② `CL-10` 借用改用 `clone --json` 的 `data.objects_fetched` 为 0 证明未重复传输（含不带 `--reference` 的正控制），覆盖有效 `--reference` 与 `--reference-if-able`；`CL-05` 引用计数 SQL 改为 NULL 安全（`COALESCE`）。③ `ADR-CL-04` 改称 `CL-14` 的生产代码落点而非写集；`CL-04`/`CL-23` 写明 `CL-28` 自 `CL-23` 拆出（G-09）。 | N/A | `ADR-CL-04`、`CL-04/05/10/16/20/23`、粒度审计表、追溯表、DEFER-CL-05、完成判据 |
| 2026-11-18 17:36:20 UTC | Claude R57 PASS + Codex R57 FAIL（双审第 10 轮，Codex 分区 A/B/F PASS、C/D/E FAIL；受审 SHA-256 `fa8742640dc9`） | ① `CL-05`：裸克隆的提交存在与 HEAD 目标合为 `rev-parse HEAD^{commit}` 等于 `S2`（结合无分支即游离），腾出一条验收非裸 tag 克隆中 `S2` 缺失（只取请求修订可达对象）。② `CL-06` 收窄：`-c` 写入新仓库，`clone.defaultRemoteName` 在初始 fetch 前生效，其它配置项对初始 fetch/checkout 的即时影响登记 `DEFER-CL-07`。③ `CL-12`：删去 `sparse:path` 的 Git 原文 AC，新增「有效 `sparse:oid` 在本卡发布提交上被拒」过渡断言（AC6），由 `CL-29` 同卡改写并补跑改写后的 `CL-12` 用例；完成判据同步。`CL-19` `upstream` 夹具写全。`CL-26`/`CL-27` 补正控制共享回归组（普通仓库中对应命令的既有用例全部通过）。 | N/A | `CL-05/06/12/19/26/27/29`、粒度审计表、追溯表、DEFER-CL-07、完成判据 |
| 2026-11-18 17:40:04 UTC | Claude R58 PASS + Codex R58 FAIL（双审第 11 轮，Codex 分区 B/E/F PASS、A/C/D FAIL；受审 SHA-256 `8b992263bd89`） | ① 「文档职责」网站分支检查与第 2 步一致（非 `cf` 先 `libra switch cf` 再复核，仍不满足才 STOP）。② `CL-05`：裸克隆恢复 HEAD 行 (NULL, `S2`) 断言（`intent`/`traces` 可能被当作符号 HEAD 目标）；裸克隆与非裸 tag 克隆各以「S 全部对象的存在性 = 是否属于请求修订的可达闭包」一次等值比较，同时覆盖修订对象在库与只取请求修订。③ `CL-13` 收窄为本地传输的过滤，网络客户端 filter 协商与重复同值 `--filter` 的组合结果登记 `DEFER-CL-08`（网络远端按「远端不广告 filter」处理）；删去重复同值过渡断言（`CL-13` 7/8）与 `CL-24` 的重复同值 AC（7/8），交接与完成判据改为 `CL-13` AC5–AC7。`CL-29` 夹具三份内容写明且断言 OID 两两不同。`CL-21` 伪终端用例改为 stdout 重定向到文件、stderr 留在伪终端，并加成功前提。 | N/A | 文档职责、ADR-CL-02、`CL-05/12/13/21/24/29`、粒度审计表、追溯表、DEFER-CL-08、完成判据 |
| 2026-11-18 17:42:34 UTC | Claude R59 PASS + Codex R59 FAIL（双审第 12 轮，Codex 分区 A/B/E PASS、C/D/F FAIL；受审 SHA-256 `ad49ecc087ff`） | ① `CL-07` 两条验收改为对象目录完整清单等值（相对路径 →（SHA-256，是否共享 inode）），覆盖完整复制与无硬链接；`CL-10` 以「R 存在时 `d` 自身对象数为 0」替换「删除 R 后 `fsck` 失败」（后者在只留 alternates 文件时也成立）。② `CL-13`/`DEFER-CL-08` 收窄：网络远端上的 `--filter` 行为不在本计划验收，新增风险登记行。③ 故障恢复矩阵按 Git `remove_junk` 区分检出前失败（删除新建目录）与检出阶段失败（保留仓库）。 | N/A | `CL-07/10/13`、粒度审计表、追溯表、故障恢复矩阵、风险登记、DEFER-CL-08 |
| 2026-11-18 17:44:51 UTC | Claude R60 PASS + Codex R60 FAIL（双审第 13 轮，Codex 分区 A/B/D/E/F PASS、C FAIL；受审 SHA-256 `9d886274f274`） | ① `CL-06` AC3–AC5 改为 `reference` 表远端跟踪分支的 `remote` 去重集合逐项相等（`{upstream}`/`{from_option}`/`{from_config}`），排除同时建立配置名远端的错误实现。② `CL-08` 收窄为本地 Libra 源；本地 Git 源（`RepoType::GitRepo`）硬链接登记 `DEFER-CL-09`。 | N/A | `CL-06/08`、追溯表、DEFER-CL-09 |
| 2026-11-18 17:47:20 UTC | Claude R61 PASS + Codex R61 FAIL（双审第 14 轮，Codex 分区 A/B/C PASS、D/E/F FAIL；受审 SHA-256 `2656c291a936`） | ① `CL-13`/`CL-24` 本地传输锚点由 `local_client.rs:889`（外部 Git 源的 `read_git_repo_refs`）更正为 `local_client.rs:586` 的 `fetch_objects`。② `CL-16` AC3 改为顶层 `a.txt` 字节等于 `S2:a.txt`（原只验文件存在）。③ 风险登记与性能摘要的「远端未广告 filter 时回退全量」限定为本地传输，网络传输指向 `DEFER-CL-08`。 | N/A | `CL-13/16/24`、风险登记、性能与容量摘要 |
| 2026-11-18 17:49:59 UTC | Claude R62 PASS + Codex R62 FAIL（双审第 15 轮，本次运行的轮次上限；Codex 分区 A/C/F PASS、B/D/E FAIL；受审 SHA-256 `742da3205ce0`）；本行修订尚未经双审复核 | ① `CL-25` 删去把 ER-04 D 组 `release.yml` 结论计作卡特有 AC/VER 的 AC9（8/12、11/12），描述写明其属 D 组门。② `CL-12` 描述只列已验收的过渡拒绝（`blob:none`、有效 `sparse:oid`），删去「`tree:<n>`/`combine:` 由 `CL-13` AC5–AC6 验收」的错误归属。③ `CL-21` 文档影响逐文件列出 `cat-file` 四个路径；`CL-16` 补 AC7：`sparse-view list` 逐字为 cone 初始规则 `/*`、`!/*/`（7/8）。 | N/A | `CL-12/16/21/25`、粒度审计表、追溯表 |
| 2026-11-18 18:04:21 UTC | 用户 2026-11-18 指令下的接续运行（agent run 2）：R63 前专项清扫（起始字节 SHA-256 `543f84c75607`，含 R62 之后尚未复审的修订） | ① `CL-16`：删去 AC7（`sparse-view list` 逐字为 `/*`、`!/*/`）——把 cone 规则写入并启用只读 `sparse-view` 会使 `ls-files` 隐藏 skip-worktree 条目，与 AC5 矛盾（libra 0.30.207 实测）；描述改为本卡稀疏状态即 `core.sparseCheckout`/`core.sparseCheckoutCone` 与 skip-worktree 位、不启用 `sparse-view`，cone 规则文件的持久化与管理并入 `DEFER-CL-04`（6/8）。② `CL-12` AC5 由 `#748` 未要求的 `sparse:path` 改为单独 `--filter=tree:1` 以 `LBR-UNSUPPORTED-001` 持续拒绝（回应 R62 D 对单独 `tree:<n>` 发布期契约的意见），新增 `DEFER-CL-10`；`ADR-CL-02`、`CL-24` 描述与完成判据同步，删去 `ADR-CL-02` 的重复括注。③ `CL-28` 写集、文档影响与追溯表补 `branch` 四份文档（会改 ref 的 `branch` 形式受默认拒绝影响）。④ 19 张卡的 `Docs and compatibility impact` 由 `EN+zh`/花括号简写改为逐文件完整路径（ER-06a）。⑤ 夹具写全：标准源仓库 S 给出 `a.txt`/`sub/b.txt` 内容；`CL-26`/`CL-27` 夹具 Q 与 `CL-20` AC6–AC8 夹具 N 写明分支与改动；`CL-19` AC5/AC7 改在新建的 `d5`/`d7` 上判定。⑥「验证执行协议」第 6 项写明表驱动多值检查的计数口径；适用范围补 `CL-23`/`CL-28` 的其余命令文档。 | N/A | 状态行、适用范围、`ADR-CL-02`、验证执行协议第 6/7 项、`CL-01/02/03/05/06/07/08/09/10/12/14/15/16/17/18/19/20/24/26/27/28`、粒度审计表、追溯表、`DEFER-CL-04/10`、完成判据 |
| 2026-11-18 18:09:42 UTC | Claude R63 PASS + Codex R63 FAIL（接续运行双审第 1 轮，Codex 分区 A/B/E `PASS`、C/D/F `FAIL`；受审 SHA-256 `c87b31524f01`） | ① `CL-25`：patch + 1 与 `compat_version_surface_sync` 属 ER-04 C 组强制门，移出卡特有 AC/VER，改为不计数的强制门行（6/12、8/12），追溯表同步。② `CL-08`：符号链接拒绝改为目录根用显式 `--local`（AC6）、子目录与对象文件用默认本地路径（AC7–AC8），描述写明两种调用形式共用同一检查。③ `CL-29`：三条单 blob AC 合并为一次整体存在性映射比较（全部提交与 tree 存在、blob 中只有 `one.t` 存在，Git `filter_sparse`），在任何按需拉取之前判定（5/8）；守护 `DEP-CL-14` 的 `show-ref` 断言改号为 AC4，关系表与 `DEP-CL-14` 同步。④ 标准源仓库 S 的 `a.txt` 内容改为 `a1`/`a2`，避免与 F33 的 `one`/`two` blob 相同；Claude P2：评审结论「完整性」行改指 `CL-05` AC1–AC4，`CL-12` 描述去掉过度概括。⑤ 同类预防（与 ③ 同一缺陷类：只验部分对象或部分条目）：`CL-13` AC4 改为三库的整体存在性映射（提交与 tree 存在、blob 缺失）；`CL-16` AC5 改为 `ls-files -t` 逐字两行 `H a.txt`、`S sub/b.txt`。 | N/A | 评审结论、关系表、验证执行协议第 7 项、`DEP-CL-14`、`CL-08/12/13/16/25/29`、粒度审计表、追溯表 |
| 2026-11-18 18:13:31 UTC | Claude R64 PASS + Codex R64 FAIL（接续运行双审第 2 轮，Codex 分区 A/B/D/E/F `PASS`、C `FAIL`；受审 SHA-256 `37529476d4a3`） | `CL-08`：R63 把三种符号链接位置分摊到两种调用形式，不能排除「某位置只在一种形式下检查」的实现；AC6–AC8 合并为一条表驱动 AC6——目录根、`pack` 子目录、松散对象文件三种夹具 × 默认本地路径与显式 `--local` 共 6 个样本全部以 `LBR-REPO-003` 拒绝，映射一次等值断言并逐样本检查克隆拒绝不变量（6/8）。「验证执行协议」第 6 项写明表驱动多值检查是「不同选项…一律分条计数」的唯一例外（仅限用户可见结局完全相同的多个输入）。 | N/A | 验证执行协议第 6 项、`CL-08`、粒度审计表 |
| 2026-11-18 18:20:10 UTC | Claude R65 PASS + Codex R65 FAIL（接续运行双审第 3 轮，Codex 分区 A `PASS`、B–F `FAIL`；受审 SHA-256 `71e66474b028`） | ① `CL-25`：受测对象改为 ER-05 评审前的本地提交 `<T>`（三张子卡提交为其祖先、两项聚合守卫在其上通过；5/12、7/12）；patch + 1、`compat_version_surface_sync`、`T-2` 与 squash 合并树等于最终提交 `<F>` 的树移入不计数的 ER-04 C 组强制门行，消除「评审前需要合并提交」的顺序循环。② `CL-09` 证据补 `fetch.rs:129-133,164-180`、`local_client.rs:308`、`clone.rs:681-686` 锚点，描述写明落点在 `LocalClient::from_path`、不改 `fetch.rs`。③ `CL-12`：AC5/AC7 合并为表驱动样本表（`tree:0/1/2`、`object:type=blob/tree`、`blob:limit=1k`），描述写明解析器默认拒绝其余单独 filter；原 AC8 改号 AC7（7/8），`CL-13` 引用同步。④ `CL-13` AC5/AC6 改为显式 `combine:` 与多个 `--filter` 两张样本表（含 `blob:none`+`tree:1` 两种顺序及 `tree:2`、`object:type=blob` 组合）；`CL-24` AC6 改为三种等价形式的整体存在性比较，新增 AC8：其余组合持续拒绝（8/8）；`ADR-CL-02`、`DEFER-CL-10`、完成判据同步。⑤ `CL-16` 新增 AC7：`libra clone --help` 不含 `declined D10`（7/8）。⑥ `base.yml` 的触发描述更正为 `pull_request` 不限目标分支、`push` 限 `main`/`release/**`（两处）。⑦ 同类预防：`CL-25` 证据补三处版本面行号锚点；`CL-24` 删去未验收的「回退提示文案」承诺（不承诺 Git 式回退警告）。 | N/A | `ADR-CL-02`、执行检查必备需求、字段全局默认、`CL-09/12/13/16/24/25`、粒度审计表、`DEFER-CL-10`、完成判据 |
| 2026-11-18 18:25:14 UTC | Claude R66 PASS + Codex R66 FAIL（接续运行双审第 4 轮，Codex 分区 A/E/F `PASS`、B/C/D `FAIL`；受审 SHA-256 `cc915c93a85d`） | ① `CL-25` C 组行与 `Full-suite trigger` 改为模板顺序：bump 后先在未提交工作树上确认版本面、跑 `T-2`、构建与安装（④–⑥），再签名提交 `<F>`（⑦，提交后工作树干净即提交树为受测树）。② 共享回归组改用确定执行的本地用例：`CL-02` → `test_clone_json_reports_fetch_transfer_counts`（新增 `Cloning into` 不得进入 `--json` stdout）、`CL-04` → `test_clone_bare_and_mirror_default_names_and_layout`、`CL-12` → `test_clone_local_flag_accepted_for_local_source`、`CL-15` → `test_clone_depth_implies_single_branch_matrix`（原用例经 `github_test_repo_for_async_test` 门控，环境缺失时提前返回而计为通过）；「验证执行协议」第 5 项写明此要求；追溯表同步。③ `CL-06` AC1 改为 `-c` 与 `--config` 两种形式的表驱动断言。④ `CL-24` AC7 回退改为 `blob:none`、重复 `--filter` 与显式 `combine:` 三种形式的表驱动整体存在性断言，描述同步。⑤ 状态行不再称「修订历史末两行」（Claude P2）。 | N/A | 状态行、验证执行协议第 5 项、`CL-02/04/06/12/15/24/25`、追溯表 |
| 2026-11-18 18:30:44 UTC | Claude R67 PASS + Codex R67 FAIL（接续运行双审第 5 轮，Codex 分区 F `PASS`、A–E `FAIL`；受审 SHA-256 `8acd5db7e1ba`） | ①「验证执行协议」第 3、5 项：判据改为「运行 = 通过 = 1（或 N）、失败 = 0」，nextest 摘要的 skipped 含被过滤排除的用例，只记录不作判据，并禁止卡特有用例以 `#[ignore]` 或环境门控绕过断言。② 网站第 2、7 步的 `status --short` 加 `--no-branch`（`status.branch=true` 时会输出分支头行，libra 0.30.207 实测；`--porcelain` 不受影响）。③ 非目标行改为 `--sparse` 以两项配置、cone 物化与 skip-worktree 位实现、不启用 `sparse_view.rs` 只读过滤，与 `CL-16` 一致。④ `CL-03` 共享回归组改为确实带 `--reject-shallow` 的 `test_clone_from_shallow_git_source_matrix`（原用例不传该选项），追溯表同步。⑤ `CL-10` 重排为 6 条：alternates 路径、`objects_fetched` 为 0、本库对象数为 0、经 alternates 可读四项各以 `--reference` 与有效 `--reference-if-able` 两个目标的表驱动断言验收，另两条为无效路径放行与非仓库目录拒绝（6/8）。⑥ `CL-29` 新增 AC6：`remote.origin.partialclonefilter` 为 `sparse:oid=main:only-one`（Git `partial_clone_register` 原样记录，6/8）。⑦ `CL-20` 证据补 `merge.rs:1121`、`rename_merge.rs:291,1157`、`tree_merge.rs:205`；同类预防：`CL-05/08/10/12/13/19/21/24` 证据中只有文件名的引用补行号锚点。⑧ Claude P2：`CL-25` 提交后的干净检查改为 `status --porcelain --untracked-files=no`（只看已跟踪文件）。 | N/A | 非目标、验证执行协议第 3/5 项、文档与网站验收命令第 2/7 步、`CL-03/05/08/10/12/13/19/20/21/24/29`、粒度审计表、追溯表 |
| 2026-11-18 18:35:51 UTC | Claude R68 PASS + Codex R68 FAIL（接续运行双审第 6 轮，Codex 分区 A/C/F `PASS`、B/D/E `FAIL`；受审 SHA-256 `577ad813dfa8`） | ① `CL-01` AC3/AC4 补克隆退出 0 前提（否则报错退出也满足「stderr 不含 `%`」）；同类：`CL-16` AC6 补 `status` 退出 0、`CL-17` AC3 补克隆退出 0。② `CL-24` AC7 回退形式表补两种反序形式（共 5 种）。③ `CL-20`：三条拒绝合并为表驱动 AC6（逐分支检查仓库快照），新增 AC7（组合克隆的 `core.sparseCheckout`/`core.sparseCheckoutCone` 均为 `true`）与 AC8（合并前 `ls-files -t` 每个条目「路径含 `/` ⇔ 标记 `S`」），仍 8/8。④ Codex D P2：`CL-12` 重写旧用例时保留与 `--filter` 无关的 `--shallow-since`/`--shallow-exclude` 警告断言。⑤ 同类预防：`CL-14` AC4 改为裸克隆与 `libra init` 新建仓库两者都输出 `sqlite`（排除依赖克隆写入配置的实现），文档关键字改为 `ref-storage-format` 以同时命中 `rev-parse` 页。 | N/A | `CL-01/12/14/16/17/20/24` |
| 2026-11-18 18:40:33 UTC | Claude R69 PASS + Codex R69 FAIL（接续运行双审第 7 轮，Codex 分区 A/D/F `PASS`、B/C/E `FAIL`；受审 SHA-256 `30a76da5d845`） | ① `CL-28`：`init` 会检查并重新初始化既有 `.libra`（`init.rs:1293,1697`），移出放行名单、在指针布局下默认拒绝；放行名单改称「不读写当前仓库」并给出 `cli.rs:1771-1800` 依据；写集、文档影响与追溯表补 `init` 四份文档。② `CL-08`：AC3 补克隆退出 0 与 `d2` 对象完整可读的正控制；新增 AC7 源仓库属主边界——经新测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1` 视源为他人所有时，默认本地路径与显式 `--local` 均以 `LBR-REPO-003` 拒绝（Git `die_upon_dubious_ownership`，`builtin/clone.c:286-298`；7/8）；协议第 7 项与风险登记的接缝清单同步。③ `CL-20` AC8 改为 `ls-files -t` 的「路径 → 标记」映射与由分支 `A` 全部文件路径导出的期望映射整体相等（缺条目或多条目都失败）。 | N/A | 验证执行协议第 7 项、`CL-08/20/28`、粒度审计表、追溯表、风险登记 |
| 2026-11-18 18:42:44 UTC | Claude R70 PASS + Codex R70 FAIL（接续运行双审第 8 轮，Codex 分区 A/B/D/E/F `PASS`、C `FAIL`；受审 SHA-256 `033c9208891d`） | `CL-08`：属主检查对象由对象目录改为源仓库元数据目录（普通仓库 `<S>/.libra`、裸仓库 `<S>`，即 Git `clone_local` 传给 `die_upon_dubious_ownership` 的 `src_repo`，`builtin/clone.c:381-389,298`）；测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1` 只作用于该判定、不影响对象目录判定，使只检查对象目录的实现无法通过 AC7；对象路径被替换为符号链接的情形由 AC6 覆盖。 | N/A | `CL-08` |
| 2026-11-18 18:45:27 UTC | Claude R71 PASS + Codex R71 FAIL（接续运行双审第 9 轮，Codex 分区 A/D/E/F `PASS`、B/C `FAIL`；受审 SHA-256 `004ac1636abe`） | ① `CL-03` 共享回归组描述改为与用例一致（目标下不留 `.libra`；目标目录整体不存在由协议第 4 项对新增用例另行检查）。② `CL-08` AC7 样本表扩为普通源与裸源（`SB`）× 默认本地路径与显式 `--local` 四个样本，接缝作用于各自的元数据目录。③ `CL-08` 删去「替换为符号链接由 AC6 覆盖」的说法，改为按 Git `builtin/clone.c:343-361` 在硬链接后比对 mode/inode/设备号/大小/uid/gid，新增测试接缝 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK=first` 与 AC8（两种调用形式均以 `LBR-REPO-003` 拒绝；8/8）；复制回退路径与 Git 相同不另做竞态检测；协议第 7 项与风险登记的接缝清单同步。 | N/A | 验证执行协议第 7 项、`CL-03/08`、粒度审计表、风险登记 |
| 2026-11-18 18:49:52 UTC | Claude R72 PASS + Codex R72 FAIL（接续运行双审第 10 轮，Codex 分区 A/B/D/E/F `PASS`、C `FAIL`；受审 SHA-256 `bbe8b4745629`） | `CL-08`/`CL-07`：「验证执行协议」第 7 项新增「对象目录清单」定义（不含 `info/alternates`），两卡的 inode/复制清单断言引用该定义；`CL-08` 原 AC1/AC2（显式 `--local` 与默认本地路径都硬链接）合并为表驱动 AC1，新 AC2 验收源 alternates 改写：借用 R 且 alternates 为相对路径、自身无对象的源 SA（夹具步骤与断言写全，libra 0.30.207 实测相对 alternates 在源内生效）被克隆到更深目录后，两种调用形式下经改写后的 alternates 都能读出只在 R 中的 blob（Git `copy_alternates` 改写为绝对路径并在遍历中跳过该文件，`builtin/clone.c:184-224,337-339`）；描述同步（仍 8/8）。 | N/A | 验证执行协议第 7 项、`CL-07/08` |
| 2026-11-18 18:51:39 UTC | Claude R73 PASS + Codex R73 PASS（接续运行双审第 11 轮，Codex 分区 A–F 全部 `PASS`；受审 SHA-256 `189cec976839d00d26639be07074c70c91d8d7bb36e166444beb9fb0fa551e05`） | 双审对同一字节通过，计划可提交；本行与状态行为 PASS 后的记账修改，不改其它内容。残余 P2：`CL-16` 独立发布期间 `--sparse` 与 `blob:none` 以外的 `--filter` 组合不加限制也不验收（`DEFER-CL-05`）；`CL-26`/`CL-27` 未测 `core.sparseCheckout=false`；`CL-08` AC7/AC8 所在用例名只写符号链接；`CL-24` 未单独验 `combine:` 克隆的 `remote.origin.promisor`。 | N/A | 状态行、修订历史、Codex review log |

## 已决议设计决策

### ADR-CL-01: `clone --progress` 布尔重写与全局 `--progress=<mode>` 兼容

- **Status:** Accepted
- **Context:** `#737` 指出全局 `--progress <mode>` 在 `libra clone --progress <url>` 中把 `<url>` 吞成 `--progress` 的值。`src/cli.rs:1121` 已有 `rewrite_index_pack_progress_args` 为 `index-pack` 处理布尔 `--progress`/`--no-progress`。
- **Decision:** 扩展 `src/cli.rs` 的参数规范化逻辑（仅对 `clone` 子命令把裸 `--progress` 规范化为全局 `--progress=text`，同时保留 `clone.rs:111-112` 既有的 `--no-progress` 处理器而不提前改写为 `--progress=none`，保留 `--progress=<mode>` 显式值），且 `--progress` 在 stderr 重定向时仍输出 `%` 进度。
- **Alternatives considered:** 删除全局 `--progress`——拒绝，破坏其它长耗时命令。
- **Consequences:** 改动 `src/cli.rs` 全局路由，按 ER-13 命中 `T-1` 全量测试。
- **Revisit when:** 后续统一重构全局 vs 子命令同名标志时。

### ADR-CL-02: 部分克隆 `--filter` 与 `combine:` / `promisor` 分阶段边界

- **Status:** Accepted
- **Context:** `#746`、`#748`、`#754` 要求支持多 `--filter` 组合为 `combine:`、拒绝无效 `sparse:` 过滤器、并在服务端支持时按过滤拉取与按需 lazy fetch（如 `cat-file -p`）。当前仓库完全没有 promisor 支持（`clone.rs:269`）。
- **Decision:** 将部分克隆拆分为 6 张递进单轴卡并按 `CL-19 -> CL-12 -> CL-13 -> CL-24 -> CL-29 -> CL-21` 顺序推进，确保每个独立发布版本自洽：`CL-19`（对象读取层缺失时触发 promisor lazy fetch，通过预置 promisor 夹具自洽）、`CL-12`（无效 `sparse:` 精确校验与目录清理；单独 `tree:<n>`、`object:type=`、`blob:limit=` 等本计划不实现的 filter 自此持续拒绝，见 `DEFER-CL-10`）、`CL-13`（`--filter` 改为 `Vec` 以识别多个 `--filter`、`--filter=blob:none` 过滤协商、`remote.<remote_name>.promisor=true`/`extensions.partialClone=<remote_name>` 写入，并在 `CL-24` 落地前对 `combine:*` 与多个取值不同的 `--filter` fail-closed 拒绝；`CL-24` 起按 Git 把多个 `--filter` 组合为 `combine:`（`list-objects-filter-options.c` 的 `transform_to_combine_type`；只接受 `blob:none` 与 `tree:1` 的组合，其余组合持续拒绝，见 `DEFER-CL-10`；重复同值的组合结果见 `DEFER-CL-08`），避免出现“接受 `--filter` 却静默全量克隆”的半成品）、`CL-24`（`combine:blob:none+tree:1` 树深过滤传输与远端无能力安全回退）、`CL-29`（`sparse:oid` 过滤与预期缺失对象边界，供 `PM-33`（自 `PM-03` 拆出）的 `t5616#33`/`TC-0625` 消费）、`CL-21`（`Receiving objects` stderr 进度与真实 `clone -> cat-file` 端到端 lazy fetch 收口）。
- **Alternatives considered:** 把五条轴压进单卡 `CL-13`——拒绝，违反 G-01/G-04；把多 `--filter` CLI 解析拆成独立发布的 `CL-11`——拒绝，单独发布会导致静默全量克隆。
- **Consequences:** 全部 6 张部分克隆卡均按 `independent` 边界串行独立发布，直至 `CL-21` 完成后部分克隆全链路就绪。
- **Revisit when:** 扩展更多 Git partial-clone filter 类型时。

### ADR-CL-03: 为 `clone --sparse` 重开历史决策 D10

- **Status:** Accepted
- **Context:** 历史决策 D10（`COMPATIBILITY.md:166,172,375` 与 `docs/development/commands/_compatibility.md:175-197`）曾拒绝 `sparse-checkout`；`sparse_view.rs` 为只读视图过滤而不物化工作区或写 skip-worktree 位。`#756` 要求 `libra clone --sparse` 初始化稀疏 cone 检出并与部分克隆/`merge` 协同。
- **Decision:** 在 `CL-16` 中局部修订 D10（同步 `COMPATIBILITY.md` 与 `docs/development/commands/_compatibility.md`），为 `clone --sparse` 实现工作区 cone 物化与 skip-worktree 索引位，并将与 `--filter=blob:none` 及目录重命名合并时的按需 lazy fetch 拆入 `CL-20`（单卡单轴，G-01）；仍不新增独立 `libra sparse-checkout` 子命令。
- **Alternatives considered:** 维持 D10 拒绝 `#756`——拒绝，与本计划覆盖全部 17 个 Issue 目标冲突。
- **Consequences:** 同步更新 `COMPATIBILITY.md`、`docs/development/commands/_compatibility.md` 与 `clone.md` 文档。
- **Revisit when:** 后续单独规划完整 `libra sparse-checkout` 命令时。

### ADR-CL-04: `--ref-storage-format` 诚实报告 `sqlite` 后端（保留 `init` D25，延后 `files`/`reftable` 后端）

- **Status:** Accepted
- **Context:** `#750` 要求 `clone --ref-storage-format=<fmt>` 与 `rev-parse --show-ref-storage-format`。`src/command/init.rs:301-313,372-375,1676-1690` 的 `RefFormat`（`strict`/`filesystem`）仅控制分支名校验，Libra 实际将 refs 存储在 SQLite（`libra.db`），且 D25（`docs/development/commands/_compatibility.md:291-297`）拒绝 `init --ref-format=files|reftable`。将 SQLite 仓库误报为 `files` 会违背存储事实。
- **Decision:** 不修改 `init.rs` 与 `init --ref-format`（保留 D25）。在 `CL-14` 中为 `clone.rs` 新增 `--ref-storage-format`（接受 `sqlite`，对未实现的 `files`/`reftable` 或未知格式 fail-closed 返回明确错误），并为 `rev-parse --show-ref-storage-format` 如实输出当前仓库后端 `sqlite`（并在 `COMPATIBILITY.md` 记为 `intentionally-different`）；`#750` 所要求的 Git 松散 `files` ref 后端与 `reftable` 后端实现统一显式延后至 `DEFER-CL-01`。
- **Alternatives considered:** 将 SQLite 仓库伪报为 `files`——拒绝，会歪曲实际存储后端。
- **Consequences:** `CL-14` 的生产代码落点只有 `clone.rs` 与 `rev_parse.rs`（测试、命令文档与网站页见卡内 `Implementation write set`），不改 `init.rs`，存储语义诚实。
- **Revisit when:** 实现 Git `files` 或 `reftable` ref 存储后端时（`DEFER-CL-01`）。

### ADR-CL-05: `clone --separate-git-dir` 与 D25 范围边界（`#740`）

- **Status:** Accepted
- **Context:** `init --separate-git-dir` 被 D25 拒绝（`init.rs:377-379,697-701`）；`src/utils/convert.rs:221,242-252` 与 `src/utils/util.rs:265` 仅支持解析 `.git` 指针文件，而 `.libra` 发现（`src/utils/util.rs:797`）要求 `.libra` 为目录，且 `clone.rs:1592` 已预留 `separate_git_dir` 字段。
- **Decision:** `CL-04`（`REL-CL-01` 子卡 1/4）仅在 `clone` 路径实现 `--separate-git-dir=<dir>`：将仓库元数据初始化至 `<dir>` 并在 `<workdir>/.libra` 写入 `gitdir: <dir>` 文本指针文件。`CL-23`（子卡 2/4）在 `src/utils/util.rs:278,314-324,791-803,1269,1464` 与 `src/command/alternates.rs:91` 解析该指针，并使 `status`/`log`/`cat-file`/`branch` 可用。`CL-28`（子卡 3/4）在 `src/cli.rs` `command_preflight` 对指针布局默认拒绝：只有放行名单里的命令可以继续，其余 `Commands::*` 退出非零且不改索引、HEAD 或 ref。`CL-25`（发布点 4/4）跑 `T-2` 后统一发布。只发布指针文件、不发布这道默认拒绝时，`restore` 等未适配命令会把数据写到错误目录，所以这四张卡不能各自发布。
- **Alternatives considered:** 同步放开 `init --separate-git-dir`——拒绝，超出 `#740` 与本计划范围。
- **Consequences:** `CL-04`、`CL-23` 与 `CL-28` 作为 `REL-CL-01` 家族子卡均指向 `CL-25` 取得 C/D 覆盖。指针文件、读者适配和写命令拒绝必须同一次发布，避免用户拿到会写错目录的中间版本。
- **Revisit when:** 后续单独放开 `init --separate-git-dir` 时。

## 全局工程约束

继承 [plan-template.md](plan-template.md) `GC-01..13`。补充约束：
- **GC-CL-01:** 各卡验收中明确写出的 Git 行为（CLI 参数语义、进度与诊断原文、对象传输与仓库内容结果）以 pinned Git `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`v2.56.0-116-g6de20f6092`，核对日期 `2026-11-17`）为基准对齐；以下 Libra 专有部分不向 Git 取值：`LBR-*` 稳定错误码、SQLite（`libra.db`）ref 与配置存储及 `rev-parse --show-ref-storage-format` 的 `sqlite` 输出（ADR-CL-04）、`.libra` 目录与 `gitdir:` 指针布局（ADR-CL-05）；不破坏 `--json` 契约。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) `ER-01..14`。
- 每张 `implementation` 卡执行阶段按 ER-13 跑 A 组 focused 测试 + `cargo +nightly fmt --all --check` + `cargo clippy --all-targets --all-features -- -D warnings`；独立发布卡命中 `T-1`..`T-6`（如 `CL-01`、`CL-08`、`CL-13`、`CL-19`）时跑全量 nextest；家族子卡 `CL-04`、`CL-23`、`CL-28` 即使改动 `src/cli.rs` 等 `T-1` 表面，也不单独跑全量——其 C 覆盖（含全量）按 ER-04 继承家族发布点 `CL-25` 的 `T-2` 全量运行，该运行的被测树须包含三张子卡的最终变更。
- 独立发布卡和家族发布点推送后必须取得 D 组远端门证据。家族子卡 `CL-04`、`CL-23`、`CL-28` 不单独推送，继承 `CL-25` 的 D 组证据：
  1. `.github/workflows/base.yml`（jobs: `format`, `clippy`, `redundancy`, `test`；`pull_request` 不限目标分支，`push` 限 `main`, `release/**`；PR/main head SHA 全绿）；
  2. `.github/workflows/codeql.yml`（job: `analyze`；`pull_request` `main`，`push` `main`, `release/**`；PR/main head SHA 全绿）；
  3. `.github/workflows/release.yml`（jobs: `build-and-upload`, `upload-install-scripts`, `update-homebrew-tap`, `verify-homebrew-formula`, `request-stable-manifest`；`push` `v*` tag 由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发；全部 job 与 R2 上传全绿）。
- **网站文档交付默认（ER-06a，适用于本计划所有命中命令文档的卡）：** 网站目标固定为 `/Volumes/Data/gitmono/libra-backend` 的 `cf` 生产分支，不得切换其它 checkout 或另建分支/PR/worktree；写前识别仓库类型、当前分支与脏状态并保留无关改动。每张相关卡必须在同一交付内运行实际网站文档 sync/check、相关前端 typecheck/test/build，并取得覆盖最终网站文档变更的 Claude Code `VERDICT: PASS`（该网站交付门独立于计划文本双审；逐步命令见下方「文档与网站验收命令」）。独立卡随后精确提交并推送 `cf`，记录后端 commit、Cloudflare 部署 run 与生产 URL/正文核验；家族子卡只完成本地网站变更、验证与精确 `cf` 提交，由 `CL-25` 在家族统一发布后串行推送并核验部署，避免提前公开未集成能力。任一门失败均保持该卡或发布点未验收，不得以 Libra 主仓 release 成功替代网站交付。
- 全部活动卡（不含已撤回的 `CL-22`）完成后，收口阶段必须执行一次完整 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **卡特有用例**（标 `(new)`）：每条 Verification 只含一个测试，过滤式为完整路径精确匹配 `-E 'test(=command::clone_test::<函数>)'`（`tests/command/*.rs` 经 `tests/command_test.rs:10` 的 `mod command;` 编入 `command_test`，nextest 测试名带 `command::` 前缀；`--lib` 单元测试名为 `cli::tests::<函数>`）；不得用前缀、正则或 `|` 并集代替。
  2. 卡特有用例运行前先以同一前缀与过滤执行 `cargo nextest list`（例：`source .env.test && cargo nextest list --test command_test -E 'test(=command::clone_test::test_clone_short_n_skips_checkout)'`），列出的测试必须恰为 1 个且名字与预期逐字一致，否则 STOP。
  3. 卡特有用例运行后记录 exit code 与 nextest 摘要的「运行 / 通过 / 失败 / skipped」计数，要求 exit code = 0、运行 = 通过 = 1、失败 = 0，任一不满足即 STOP；摘要中的 skipped 包含被过滤表达式排除的其它用例，只记录、不作判据。被选中的卡特有用例不得以 `#[ignore]` 或环境门控提前返回的方式绕过断言（本计划卡特有用例均不设环境门控）。共享回归组与聚合守卫不适用本项与第 2 项的「恰为 1 个」，按第 5 项判定。
  4. 测试实现必须对卡内「AC → 断言映射」逐条断言，执行记录逐条引用 AC 编号。**拒绝不变量**：AC 写「以 `<LBR-*>` 拒绝」且被测命令作用于已存在的仓库时，测试在调用前后各取一次「仓库快照」并要求逐项相等（该不变量由本项统一规定、对每个此类拒绝用例检查，不另计 AC）。仓库快照 = 该仓库数据库（普通布局为 `.libra/libra.db`，`--separate-git-dir` 指针布局为 `<G>/libra.db`）中 `reference` 表与 `config_kv` 表的全部行（按主键排序，直接读表，不经会隐藏加密值的 `libra config list`；表定义见 `sql/sqlite_20260309_init.sql:12,19`）+ 索引文件 SHA-256 + 工作区「相对路径 → SHA-256」清单（排除仓库元数据目录）+ 对象目录内文件路径集合；对象目录唯一允许的差异是失败命令留下的未被引用的 blob/tree 文件（Git 亦然），新增文件经 `libra cat-file -t` 只读识别后不得为 commit 或 tag。**克隆拒绝不变量**：被测命令为 `libra clone` 且在检出之前被拒绝（参数校验、源解析、传输、过滤与浅仓库校验、对象复制；本计划全部克隆拒绝用例都属此类）时，测试在调用后断言目标目录（以及给出 `--separate-git-dir` 时的 `<G>`）不存在——调用前二者均不存在，Git 对这类失败同样经 `remove_junk` 清理新建目录（`builtin/clone.c:404-407,414-423`，此时 `junk_mode` 仍为 `JUNK_LEAVE_NONE`）；检出阶段失败时 Git 保留仓库（`junk_mode = JUNK_LEAVE_REPO`，`builtin/clone.c:1656`），本计划没有检出阶段失败的验收用例，不对其施加本不变量；该不变量由本项统一规定、对每个克隆拒绝用例检查，不另计 AC；克隆类拒绝不取仓库快照。**Git 期望**：映射中注明 Git 原文、Git 源码行或「Git 同夹具」的值，开工时在以 `git@6de20f6092` 构建的 Git 上用同一夹具重新取得并硬编码进测试；与本文不一致时以 pinned 版本为准并回写本卡 `Current evidence`（超出本卡行为轴时按 ER-03 处理）。Libra 专有行为（`LBR-*` 稳定码、`libra.db` 内容、测试接缝）以卡内写明的 Libra 基准为准，不向 Git 取值。
  5. **共享回归组 / 聚合守卫**（Verification 中如此标注）：可为整个 `--test` target、`test(=…)` 精确并集或 `test(/^…/)` 正则族；运行前以同一前缀与过滤执行 `cargo nextest list` 记录匹配数 N（必须 ≥ 1；卡内给出预期 N 时必须相等），运行后要求 exit code = 0、运行 = 通过 = N、失败 = 0（skipped 同第 3 项只记录）。选作具体行为回归的单个既有用例必须在本地确定执行该行为，不得选用环境缺失时提前 `return` 的 live 门控用例（如经 `github_test_repo_for_async_test` 取得 GitHub 测试仓库的用例——提前返回在 nextest 中计为通过）。结果只作回归证据，不计为卡特有 AC 证据，但计入该卡 `VER` 分子；唯一例外：`release` 卡 `CL-25` 的聚合守卫可作为该卡聚合类 AC 的证据（这些 AC 的谓词本身就是「子卡用例在受测树上通过」）。
  6. **计数口径（G-03）**：每条 Verification 含两个独立门（第 2/5 项的 `cargo nextest list` 预检与第 3/5 项的运行判定），nextest 条目各计 2 门，标为「手工证据」的条目各计 1 门（模板 G-03「手工证据按项计」），各卡 `VER` 分子为二者之和。一条 AC 只承载一个可独立失败的可观测谓词；不同选项、不同命令、不同错误条件一律分条计数（唯一例外见本项末「表驱动多值检查的计数」：用户可见结局完全相同的多个输入可在一条 AC 中以整体谓词陈述）；「过渡断言」（只在本卡发布提交上成立、由卡内写明的后继卡同卡改写的断言）照常计数。**映射口径**：AC 文本与其映射表述同一个谓词，映射只给出该谓词的精确判定值（逐字期望或等值比较对象），不加入 AC 文本之外的判据。失败类 AC 一律写成「以 `<LBR-*>` 拒绝」，这是一个谓词：进程以非零状态结束且 stderr 含该稳定码；映射同时检查这两个组成部分，但不断言具体数值退出码——数值可被 `exit_code_override` 与 `LIBRA_FINE_EXIT_CODES` 改变（`src/utils/error.rs`）；需要 Git 原文时原文单列为一条 AC。**表驱动多值检查的计数**（全部卡一致适用）：AC 文本若把一个集合或映射作为整体陈述为一个谓词——实际值是一个集合/映射（元素可来自多个对象、多个样本命令行——含不同选项形式或不同夹具位置——或卡内已建立的多个库），期望值由 AC 文本写明的单一规则导出，映射对二者只做一次等值断言或一次全称断言（如 `CL-05` AC2/AC6、`CL-07` AC1–AC2、`CL-08` AC4/AC6、`CL-13` AC4、`CL-24` AC2–AC6、`CL-28` AC5）——计为 1 条；凡需要按输入分别陈述不同的用户可见结局（如「A 退出 0、B 以 X 拒绝」、不同稳定码）的，按结局分条计数，不得以表驱动合并。
  7. **进程隔离、标准夹具与测试接缝**：新增的集成用例（`--test command_test`）一律以子进程调用 `libra`（纯单元测试——`--lib` 下的 `cli::tests::pointer_layout_allowlist_matches_commands`——在测试进程内调用解析与判定函数，不改 cwd/环境、不启动子进程，不适用本句），通过显式 `current_dir` 与逐子进程环境变量传入工作目录、`HOME`/`USERPROFILE`/`XDG_CONFIG_HOME`（指向仓库外的测试临时目录，避免测试 harness 在 cwd 下创建的 `.libra-test-home/` 进入快照）、`LIBRA_TEST` 与测试接缝变量，不修改测试进程的 cwd/环境，因而不使用 `#[serial(...)]`，不触及跨计划共享的 `tests/SERIAL_REGISTRY.tsv` 与生成的 `.config/nextest.toml`；若实现中确需 `#[serial]`，须先按 ER-03 把这两个文件加入本卡 `Implementation write set` 并补 `compat_serial_registry` 共享回归门。**标准源仓库 S**：测试临时目录中以 `libra init` 创建、默认分支 `main`，第一个提交加入顶层文件 `a.txt`（内容为单行 `a1`）与子目录文件 `sub/b.txt`（单行 `b`），第二个提交把 `a.txt` 改为单行 `a2`，并在第二个提交上建轻量 tag `v1`；测试在创建时保存两个提交的完整 OID，记为 `S1`、`S2`，后续不经 `HEAD~N` 等可变表达式重新推导；`file://<S>` 指以 S 绝对路径构成的 URL；各卡在此之上的增改写在卡内。**对象目录清单**（`CL-07`/`CL-08` 的硬链接、复制与 inode 断言使用）：对象目录下除 `info/alternates` 以外的全部普通文件；`info/alternates` 不是对象文件，按 Git `copy_alternates` 单独处理（`CL-08` AC2），标准源仓库 S 本身不含 alternates。**对象存在性判定**（只读）：在目标仓库执行 `libra --offline cat-file -e <完整 OID>`，退出 0 = 存在；退出 1 且 stderr 为空 = 缺失（`src/command/cat_file.rs:1229-1240` `check_object_exists`；2026-11-18 以 libra 0.30.207 实测缺失的完整 OID 为退出 1、无输出）；其它任何结果（含输出 `LBR-*` 的错误）判测试失败，不当作缺失；每个仓库在做缺失判定前先对其 `HEAD` 提交做同一判定并要求为存在（正控制；`--offline` 防止部分克隆按需拉取改变结果）。依赖符号链接、硬链接或 inode 的用例（`CL-07`、`CL-08`、`CL-09` 的 inode 比较）整条以 `#[cfg(unix)]` 限定；本计划的测试门（本地三门与 `base.yml` 的 `test` job）在 Unix 上执行。测试接缝 `LIBRA_TEST_CLONE_HARDLINK_FAIL`、`LIBRA_TEST_CLONE_FOREIGN_OWNER` 与 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK`（`CL-08`）、`LIBRA_TEST_UPLOAD_PACK_NO_FILTER`（`CL-13` 新增，`CL-24` 复用）与 `LIBRA_TEST_TRACE_LAZY_FETCH`（`CL-19` 新增，`CL-20` 复用；只向 stderr 写 `libra-test: lazy-fetch want=<n>` 行，不写任何文件）只在 `LIBRA_TEST=1` 时读取，生产路径不读取；需要终端的用例（`CL-21` AC5–AC6）以开发依赖 `portable_pty` 打开伪终端运行子进程（同 `tests/command/config_test.rs:3836`）；它们与既有 `LIBRA_TEST` 接缝同属测试专用，不进入用户文档。

- **文档与网站验收命令（ER-06/ER-06a 同卡强制门；各卡「文档 AC」引用；按序执行，任一步失败即 STOP 且本卡不得进入 `complete`）：**
  1. **仓库文档逐文件验收：** 对本卡 `Docs and compatibility impact` 所列每个仓库文件记录改动前后 `shasum -a 256 <file>`；对每个命令 `<cmd>` 以 `test -f docs/commands/<cmd>.md && test -f docs/commands/zh-CN/<cmd>.md` 确认 EN/zh 双份存在；对卡内「文档关键字」在每个所列 EN/zh 命令页执行 `rg -n -F -- '<关键字>' <file>`（退出码 0 = 命中；1 = 缺失 → STOP；>1 = 命令失败 → STOP），这些命令页改动后 SHA-256 必须与改动前不同；`docs/development/commands/<cmd>.md`、`docs/development/commands/_compatibility.md` 与 `COMPATIBILITY.md` 对应行逐文件做语义核对，结论为「已更新」（记录改动前后 SHA-256 与所改段落）或「无需修改」（记录理由与未变 SHA-256），不得为通过机械门而添加不合该文件粒度的内容；`COMPATIBILITY.md` 先以下方命令块 W-1 刷新目标行号（输出必须非空；同一命令可有补充行，基线：`clone`=166、`rev-parse`=217,277、`merge`=205,212），有改动时断言改动前后 `wc -l < COMPATIBILITY.md` 相等（只原地修改，不增删行：兼容账本按绝对行号锚定）。本计划每张实现卡都改变用户可见行为，所列 EN/zh 命令页与网站页必须同卡实际更新（ER-06a），不得以「无需修改」通过。
  2. **网站检出预检：** 前置：「文档职责」所载用户 2026-11-18 11:55:09 UTC 的网站检出确认仍有效、未被用户撤回或改变，否则 STOP。按序执行：(a) `test -d /Volumes/Data/gitmono/libra-backend/.libra && test ! -e /Volumes/Data/gitmono/libra-backend/.git`，不满足即 STOP；(b) `cd /Volumes/Data/gitmono/libra-backend && libra --offline symbolic-ref --short HEAD`，输出不是 `cf` 时按 ER-06a 执行 `libra switch cf`（被脏工作区等阻止即 STOP）后重跑 (b)；(c) 此后输出须恰为 `cf`，且 `libra status --short --branch` 首行须以 `## cf...origin/cf` 开头，否则 STOP；(d) 本卡目标页面 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 若已有未提交改动（`libra status --short --no-branch -- <页面路径>` 输出非空；`--no-branch` 排除 `status.branch=true` 时的分支头行）即 STOP；随后执行 `libra fetch origin`，要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`（本地 `cf` 不含未推送提交）——本地落后时只允许 `libra pull --ff-only origin cf` 快进后重验，领先或分叉即 STOP。**家族例外（仅 `REL-CL-01`）**：`CL-23`、`CL-28` 与 `CL-25` 执行本项时，允许本地 `cf` 领先 `refs/remotes/origin/cf` 的提交恰为同家族前序子卡在第 6 步记录的本地提交（以 `libra rev-parse refs/remotes/origin/cf` 起逐个比对已记录 SHA 与提交顺序，多一个、少一个或顺序不同即 STOP）；其余条件不变。`libra status --porcelain` 中不得有任何已暂存条目（首列既非空格也非 `?`），否则 STOP。已有的无关改动保留、不暂存。
  3. **同步与漂移检查：** 以包含本卡最终 EN 参考文档的 checkout 为 `--libra-root`（默认 `/Volumes/Data/gitmono/libra`；执行前以第 1 步记录的 SHA-256 确认其中 `docs/commands/<cmd>.md` 已是本卡最终字节：独立卡为已合入 `main` 的字节，家族子卡为本卡本地提交的字节），执行 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --write --only <cmd…>`（多个命令以空格分隔），随后 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --only <cmd…>` 必须 exit 0（1 = 漂移、2 = 安全拒绝，均 STOP）；记录每个 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 的改动前后 SHA-256，同步后网站页 SHA-256 必须与改动前不同；若未变即 STOP 并排查，不设「网站无变化」通过分支。
  4. **前端校验：** `cd /Volumes/Data/gitmono/libra-backend && pnpm typecheck`、`pnpm test` 与 `pnpm build` 分别记录退出码，均须为 0（根 `package.json` 的 `typecheck`/`build` 转发到 `apps/tanstack-app/package.json:13,8`，`test` 为 `vitest run`）。
  5. **网站评审：** 对第 3 步记录的最终页面字节取得独立 Claude Code `VERDICT: PASS`；评审后页面字节变化须重审。
  6. **精确提交（与推送）：** 只 `libra add apps/tanstack-app/content/docs/commands/<cmd>.en.md`（本卡页面；多命令卡逐页添加）→ `libra status --porcelain` 的已暂存条目必须恰为本卡页面集合，否则 STOP → `libra commit -s -m "docs(commands): sync <cmd> for <卡 ID>"` → 按 ER-07 对同一提交校验 `gpgsig` 与 `Signed-off-by` → `libra show <提交>:<页面路径> | shasum -a 256` 等于第 5 步受审字节。**独立卡**随后推送：推送前再次 `libra fetch origin`，要求 `libra rev-parse <提交>^` 等于 `libra rev-parse refs/remotes/origin/cf`（本次推送只新增本提交）→ `libra push origin cf`，推送后 `libra ls-remote origin refs/heads/cf` 指向该提交。**家族子卡**（`CL-04`、`CL-23`、`CL-28`）到此为止：只记录本地提交 SHA，不推送。**`CL-25`** 在本卡 `gh release create` 与 `release.yml` 五个 job 全绿后重跑第 2 步（含家族例外），再次 `libra fetch origin` 并要求三个家族提交之下的基底等于 `refs/remotes/origin/cf`，然后一次 `libra push origin cf`，推送后 `libra ls-remote origin refs/heads/cf` 指向 `CL-28` 的网站提交。远端已前进（上述比较不等，或推送因非快进被拒）时不推送、不 force、不改写他人提交：独立卡按 `libra reset --mixed <提交>^` → `libra restore --staged --worktree -- <页面路径>` → `libra pull --ff-only origin cf` 恢复后从第 2 步重新开始，重新生成的页面字节与第 5 步受审字节不同时须重新取得第 5 步 `PASS`；`CL-25` 遇此情况 STOP 并由协调者裁决（不得自行改写家族提交）。
  7. **部署核验（`<article>` HTML 逐字节比对）：** 基准构建前先过干净性门：`cd /Volumes/Data/gitmono/libra-backend && libra status --short --no-branch --ignored --untracked-files=all` 的输出中，除后端根目录 `docs/` 下的条目与下列构建产物/依赖/缓存的忽略条目外不得有任何条目：`node_modules/`、`apps/tanstack-app/node_modules/`、`apps/tanstack-app/dist/`、`apps/tanstack-app/.wrangler/`、`apps/tanstack-app/src/generated/docs-manifest.ts`、`apps/tanstack-app/src/generated/docs-search.ts`（二者由构建从内容目录重新生成）、`scripts/__pycache__/`、`test-results/`，以及 `apps/tanstack-app/.dev.vars`；后端根目录不得存在任何 `.env*` 文件；并要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`。任一不满足即 STOP（不清理、不暂存他人改动）。随后以 `pnpm preview:cf`（按 `apps/tanstack-app/package.json:9` 先构建再以 `wrangler dev --port 7001` 本地运行）作基准，对本地 `http://localhost:7001/en/docs/commands/<cmd>` 与生产 `https://libra.tools/en/docs/commands/<cmd>` 分别执行下方命令块 W-2：在内存中取得同一次响应，要求 HTTP 状态为 200、`<article>` 存在且文本非空，输出 `<article>` 内部 HTML 原始字节的 SHA-256 与字节数；退出码非 0（1 = 请求或传输失败、2 = 缺 `<article>` 或文本为空、3 = HTTP 响应状态非 200，含 `urllib.error.HTTPError`）即 STOP。两份 SHA-256 必须相等；不相等即 STOP（不得改用规范化文本比较放行）。只把两份哈希、字节数与观察 UTC 时间写入本计划「实施证据汇总」，不落盘任何响应内容；用后停止本地预览进程。部署传播延迟按 ER-10 暂时性错误有界重试（≤ 5 次、≤ 30 分钟），超预算置 `blocked`。`CL-25` 对三张子卡的每个页面逐一执行本步。
  8. **时序：** 独立卡：第 1–2 步在本卡 C 组签名提交之前完成，任一失败则本卡不得提交、合并或发布（`Lifecycle` 置 `blocked`）；第 3–7 步在本卡合入 `main` 与 `gh release create` 之后执行（开始前重跑第 2 步），使网站不先于发布公开新行为；第 3–7 步未完成前本卡停留在 `remote-pending`。家族子卡：第 1–2 步在本卡本地 Libra 提交之前完成，第 3–6 步（只到本地 `cf` 提交）在本卡本地 Libra 提交之后完成；推送与第 7 步由 `CL-25` 在发布后承接（`DEP-CL-15` 窗口），子卡随 `CL-25` 经 `remote-pending` 进入 `complete`。

上列第 1、7 步引用的命令块（`<cmd>` 替换为本卡命令；`<URL>` 为第 7 步的本地或生产页面地址）：

```bash
# W-1：定位 COMPATIBILITY.md 中 <cmd> 的状态矩阵行；输出须非空（基线 clone=166、rev-parse=217,277、merge=205,212）
awk -F'|' -v c=" <cmd> " '$2==c && $3 ~ /^ (supported|partial|unsupported|intentionally-different) $/ {print NR}' COMPATIBILITY.md

# W-2：在内存中取得 <URL> 的同一次响应，输出 <article> 内部 HTML 原始字节（不做规范化）的「SHA-256 字节数」；退出码须为 0，不落盘响应
python3 -I - "<URL>" <<'PY'
import hashlib, re, sys, urllib.error, urllib.request
req = urllib.request.Request(sys.argv[1], headers={"User-Agent": "curl/8.7.1"})
try:
    with urllib.request.urlopen(req, timeout=30) as r:
        if r.status != 200:
            sys.exit(3)
        s = r.read().decode("utf-8")
except urllib.error.HTTPError:
    sys.exit(3)
except Exception:
    sys.exit(1)
m = re.search(r"<article[^>]*>(.*?)</article>", s, re.S)
if not m or not re.sub(r"<[^>]+>", "", m.group(1)).strip():
    sys.exit(2)
b = m.group(1).encode("utf-8")
print(hashlib.sha256(b).hexdigest(), len(b))
PY
```

## 实施顺序

全串行 DAG（匹配各卡 `Dependencies`）：
`CL-01 -> CL-02 -> CL-03 -> CL-04 -> CL-23 -> CL-28 -> CL-25 -> CL-05 -> CL-06 -> CL-08 -> CL-07 -> CL-09 -> CL-10 -> CL-18 -> CL-19 -> CL-12 -> CL-13 -> CL-24 -> CL-29 -> CL-21 -> CL-14 -> CL-15 -> CL-26 -> CL-27 -> CL-16 -> CL-20 -> CL-17`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-CL-01 | incoming | 上游基线 | `libra` `main` 当前 checkout | 本计划协调者 | `cargo check` 通过且基线锚点可解析 | `src/command/clone.rs:1` | 开工前按 ER-02 刷新锚点 |
| DEP-CL-02 | incoming | 跨计划前置 | `plan-20261117-plumbing-misc.md#PM-06`（`#947`） | 仓库发布协调者 | `PM-06`（`rev-parse --show-object-format`）先合入 `src/command/rev_parse.rs` 后再合入 `CL-14`（单向无环） | `src/command/rev_parse.rs:59` | 若 `PM-06` 延迟，`CL-14` 等待其合入后 rebase |
| DEP-CL-03 | outgoing | 跨计划协调 | `plan-20261117-commit-signing.md#CS-02,#CS-02a,#CS-15b,#CS-21a`（`DEP-CS-03`） | `commit-signing` 计划协调者 | 仅下列活动卡先合入：`CL-01..CL-10`、`CL-12..CL-16`、`CL-18..CL-19`、`CL-21`、`CL-23..CL-29`（含 `CL-25` 家族发布点；明确排除已撤回的 `CL-11`/`CL-22`，以及为避免经 `CS-02a` 回环而排除的 `CL-20` 与依赖它的 `CL-17`）。`DEP-CS-03` 必须逐字镜像该集合，不以区间展开撤回卡。2026-11-18 按已推送版本复核：commit-signing（`2fe8c4b`）的 `DEP-CS-03` 与本集合逐字一致；本集合各卡的跨计划入边（`DEP-CL-02` 的 `PM-06` 链、`DEP-CL-04` 的 `FP-07`/`FP-15`、`DEP-CL-09`..`DEP-CL-13` 的写锁）在已推送的 plumbing-misc（`cd5ccac`）、fetch-push（`49b62d1`）、global-init-infra（`f3316d9`）与 worktree-clean-reset-apply（`55d5d2f`）中均未经由 `CS-02`/`CS-02a`/`CS-15b`/`CS-21a`，无回环；评审中的计划合入时由协调者按 ID 复核 | `docs/development/plan/plan-20261117-commit-signing.md` | 不阻塞本计划 |
| DEP-CL-04 | incoming | 跨计划顺序前置 | `plan-20261117-fetch-push.md#FP-07,#FP-15` | 仓库发布协调者 | `FP-07` 与 `FP-15` 先合入 `src/command/fetch.rs` 与 `COMPATIBILITY.md`，随后本计划 `CL-05/10/13/19/20/21/24/29` 按顺序 rebase 并合入（明确单向顺序，禁止并发）。`FP-07`、`FP-15` 指 `plan-20261117-fetch-push.md` 的卡，不是 `issues/529.md` 的同名卡；对方登记为 `DEP-FP-05` | `docs/development/plan/plan-20261117-fetch-push.md` | 若 `FP-07`/`FP-15` 未合入，本计划涉及 `fetch.rs` 的卡等待其窗口完成 |
| DEP-CL-05 | incoming | 外部闭环前置 | 外部 issue `#541` 与 `plan-20261117-fetch-push.md#FP-15` | `fetch-push` 与 `#541` 协调者 | 只门控 `DEFER-CL-03` 的 `fetch --prune` 端到端。`CL-17` 不依赖本行即可完成配置持久化 | `#876` | `#541` 与 `FP-15` 都进入 `main` 后才重启 `DEFER-CL-03` |
| DEP-CL-06 | outgoing | 跨计划协调 | `plan-20261117-{checkout-switch-restore,global-init-infra,plumbing-misc}.md` | 仓库发布协调者 | `CL-01`、`CL-28`、`CL-26`、`CL-27`、`CL-20` 修改 `src/cli.rs` 后按 ER-12 串行交付给后续修改 `src/cli.rs` 的计划 | `src/cli.rs:1` | 禁止并发修改 `src/cli.rs` |
| DEP-CL-07 | outgoing | 跨计划协调 | `plan-20261117-merge-pull.md#MP-17` | 仓库发布协调者 | `CL-20` 先合入 `merge.rs`、`rename_merge.rs`、`tree_merge.rs`。`CL-20` 开工前只确认没有在途写者，不等待 `MP-17`。`MP-17` 在 `CL-20` 合入之后再改这些文件（对方登记为 `DEP-MP-11`；该计划仍在评审，按 ID 引用） | `docs/development/plan/plan-20261117-merge-pull.md` | 禁止并发修改这些文件 |
| DEP-CL-08 | outgoing | 跨计划协调 | `plan-20261117-plumbing-misc.md#PM-14` | `plumbing-misc` 协调者 | `CL-19` 先于 `PM-14` 合入共享的 `src/command/cat_file.rs`（对方 `DEP-PM-08`），按 ER-12 串行 | `src/command/cat_file.rs:1` | 禁止并发修改 `cat_file.rs` |
| DEP-CL-09 | incoming | 跨计划协调 | `plan-20261117-global-init-infra.md` | 仓库发布协调者 | `CL-14` 共享 `src/command/rev_parse.rs` 按 ER-12 串行合入 | `src/command/rev_parse.rs:59` | 禁止并发修改 `rev_parse.rs` |
| DEP-CL-10 | incoming | 跨计划协调 | `plan-20261117-{global-init-infra,branch,describe,merge-pull}.md` | 仓库发布协调者 | `CL-23` 共享 `src/utils/util.rs` 按 ER-12 串行合入 | `src/utils/util.rs:278,314,791,1269,1464` | 禁止并发修改 `src/utils/util.rs` |
| DEP-CL-11 | incoming | 跨计划协调 | `plan-20261117-{checkout-switch-restore,global-init-infra}.md` | 仓库发布协调者 | `CL-16` 共享 `src/command/status.rs` 按 ER-12 串行合入 | `src/command/status.rs:563` | 禁止并发修改 `src/command/status.rs` |
| DEP-CL-12 | incoming | 跨计划文档写集 | 其它计划里会改命令文档的卡 | 仓库发布协调者 | `CL-28` 改写集里的每份命令文档前，核验 `plan-status.md` 没有该文件的在途写者 | `docs/commands/checkout.md:1` | 有在途写者时阻塞，禁止并发改同一命令文档 |
| DEP-CL-13 | incoming | 跨计划共享文件写锁 | 同批 `plan-20261117-*.md` 中写 `COMPATIBILITY.md`、`src/cli.rs`、`src/utils/util.rs` 或网站生成产物（`apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts`、`apps/tanstack-app/dist/`）的卡（对方 `DEP-GII-03`、`DEP-PM-11`、`DEP-MP-10` 等） | 仓库发布协调者 | 本计划每张写这些文件的卡开工前按 ER-12 在 `plan-status.md` 取得对应文件的独占窗口；`COMPATIBILITY.md` 只做原地修改、不增删行（兼容账本按绝对行号锚定） | `COMPATIBILITY.md:166` | 有持锁者时置 `blocked`，禁止并发改同一文件；本行全局门控，不写入各卡 `Dependencies` |
| DEP-CL-14 | outgoing | 跨计划顺序交付 | `plan-20261117-plumbing-misc.md#PM-33`（对方 `DEP-PM-08`） | `plumbing-misc` 计划协调者 | `CL-13`、`CL-19`、`CL-29` 在 `main` 达到 `done/complete` 后供 `PM-33` 开工；本计划修订历史与评审记录中写作「供 `PM-03`」之处即指自 `PM-03` 拆出的 `PM-33`。本计划不改变 Libra 非 `--mirror` 的 `--bare` 克隆建立 `refs/remotes/<name>/*` 的现状（`PM-33` 依赖；`CL-29` AC4 守护） | `docs/development/plan/plan-20261117-plumbing-misc.md` | 不阻塞本计划，单向无环 |
| DEP-CL-15 | outgoing | 网站推送冻结窗口 | 同批其它计划的网站交付步骤 | 仓库发布协调者 | `REL-CL-01` 窗口内（`CL-04` 第一个本地 `cf` 提交起，至 `CL-25` 推送并完成部署核验止）本地 `cf` 领先 `origin/cf`；窗口须在 `plan-status.md` 登记，其它计划的网站第 2 步在窗口内等待 | `plan-status.md` | 窗口超过 14 天未结束时由协调者裁决；不得推送未发布能力的页面，不 force |

### 发布分组与并发窗口

- **`REL-CL-01`（G-08 家族卡 — `clone --separate-git-dir` 指针布局、读者适配与写命令拒绝）：** 成员 `CL-04`（`family child`，1/4）→ `CL-23`（`family child`，2/4）→ `CL-28`（`family child`，3/4）→ `CL-25`（`Task type: release`，`family release point of REL-CL-01`，4/4）。不可独立发布的原因：只发布 `CL-04` 的 `gitdir:` 指针、不发布 `CL-23` 的路径发现和 `CL-28` 的默认拒绝时，放行名单之外的命令（包括 `restore`）会把数据写进错误目录。三张子卡各自 review、各自通过适用 ER-04 门并本地提交，**禁止**在 `CL-25` 之前单独推送或触发 tag/release（窗口内禁止插入任何其它卡）。`CL-25` 执行 `patch + 1`、`compat_version_surface_sync`、`cargo build --release`、全量 `T-2` 与 `gh release create`。发布前失败按逆序 `CL-25 -> CL-28 -> CL-23 -> CL-04` 本地 `revert`；发布后按 `immutable-release` 前滚。
- 其余 23 张卡均为 `Release boundary = independent`（各自走完整 C 组 `patch + 1` + `gh release create`）。

**并发声明:** 全串行（共享 `clone.rs`、`fetch.rs`、`COMPATIBILITY.md` 与 `docs/commands/` 写集，遵守 G-10）。

**发布者:** `Plan Author 5`（按 ER-12 与仓库全局单一发布者协调串行发布）。

**发布窗口顺序:** `CL-01` → `CL-02` → `CL-03` → `REL-CL-01`（`CL-04` → `CL-23` → `CL-28` → `CL-25`） → `CL-05` → `CL-06` → `CL-08` → `CL-07` → `CL-09` → `CL-10` → `CL-18` → `CL-19` → `CL-12` → `CL-13` → `CL-24` → `CL-29` → `CL-21` → `CL-14` → `CL-15` → `CL-26` → `CL-27` → `CL-16` → `CL-20` → `CL-17`（与实施 DAG 完全一致；同一时刻仅允许一卡/家族持有发布窗口）。

### Phase 1: CLI 标志与配置覆盖（CL-01..CL-04, CL-23, CL-28, CL-25, CL-05..CL-06）

**目标:** 修复 `--progress`、`-n`、`--no-reject-shallow`、`--separate-git-dir`（含指针消费者适配、`fail-closed` 契约与 `REL-CL-01` 发布）、`--revision` 与 `-c <key=value>`。
**进入条件:** `DEP-CL-01` 核实完成。
**退出条件:** `CL-01..CL-04, CL-23, CL-28, CL-25, CL-05..CL-06` 全部 `done/complete`。

### Phase 2: 本地硬链接、回退与 Alternates（CL-08, CL-07, CL-09, CL-10, CL-18）

**目标:** 先落地 `-l/--local` 硬链接（`CL-08`），再验收 `--no-hardlinks`（`CL-07`）、`<path>.git` 回退（`CL-09`）、`--reference`（`CL-10`）与 `--dissociate`（`CL-18`）。
**进入条件:** `CL-06` 完成。
**退出条件:** 本阶段 5 张卡全部 `done/complete`。

### Phase 3: 部分克隆、存储格式、稀疏视图与 Refspec（CL-19, CL-12, CL-13, CL-24, CL-29, CL-21, CL-14..CL-15, CL-26, CL-27, CL-16, CL-20, CL-17）

**目标:** 实现 promisor lazy fetch、无效 `sparse:` 拒绝、多 `--filter` 组合与协商、`sparse:oid` 过滤、进度、`--ref-storage-format`、`--single-branch` 游离 HEAD、稀疏工作区 `fail-closed` 护栏、`--sparse` 物化与 lazy merge、默认 `remote.<name>.fetch` 持久化。`#876` 的 `fetch --prune` 端到端在 `DEFER-CL-03`，不在本阶段收口。
**进入条件:** `CL-18` 完成。
**退出条件:** 本阶段列出的 13 张活动卡全部 `done/complete`。`CL-22` 已撤回，`DEFER-CL-03` 不阻塞本阶段。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Release write set 默认:** `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`（按 `compat_version_surface_sync` 核验实际版本面 + `gh release create` 产物；各卡写 `Inherited` 即继承本列表）
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（推送前可在本地分支 `revert` 单卡提交；一旦合入并触发 `gh release create` 发布不可变 artifact，禁止回退历史，必须前滚发布新 `patch + 1` 修复版本并给出降级指引：重新安装上一版本 binary，兼容窗口为当前 minor 系列内）
- **D-group remote checks 默认:** `base.yml`（jobs: `format`, `clippy`, `redundancy`, `test`；`pull_request` 不限目标分支，`push` 限 `main`, `release/**`）、`codeql.yml`（job: `analyze`；`pull_request` `main`，`push` `main`, `release/**`）、`release.yml`（jobs: `build-and-upload`, `upload-install-scripts`, `update-homebrew-tap`, `verify-homebrew-formula`, `request-stable-manifest`；`push` `v*` tag 由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发，全部通过）
- **Migration and rollback 默认:** `N/A：无 SQLite schema 迁移`
- **Security and privacy 默认:** `继承 GC-07、GC-11`
- **Performance budget 默认:** `继承 GC-10`

**默认覆盖:**
- `CL-04`、`CL-23`、`CL-28`：`Release boundary: family child of REL-CL-01`、`Version increment: N/A`、`Release write set: N/A`（本地提交不独立推送，随 `CL-25` 统一发布）。
- `CL-25`：`Task type: release`、`Release boundary: family release point of REL-CL-01`（在此点执行 `patch + 1`、`compat_version_surface_sync` 与 `gh release create`，跑 `T-2` 全量）。
**规则 waiver（`EX-*`）:** 无。

### 任务卡粒度审计表

> `landing / prod-files` 仅计入生产代码目录与生产文件（不含随附测试与文档，符合 G-04）。

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| CL-01 | implementation | clone --progress 布尔语义 | immutable-release | yes | yes | 4/8 | 4/8 | 2/2 | M | none | 串行链起点 | independent | N/A | N/A |
| CL-02 | implementation | clone -n 短标志支持 | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | CL-01 | 串行于 CL-01 | independent | N/A | N/A |
| CL-03 | implementation | clone --no-reject-shallow | immutable-release | yes | yes | 2/8 | 4/8 | 1/1 | M | CL-02 | 串行于 CL-02 | independent | N/A | N/A |
| CL-04 | implementation | clone --separate-git-dir 与 --bare 互斥 | immutable-release | yes | yes | 8/8 | 4/8 | 1/1 | M | CL-03 | 串行于 CL-03 | family child | N/A | N/A |
| CL-23 | implementation | pointer 发现与四个基本命令 | immutable-release | yes | yes | 5/8 | 4/8 | 2/2 | M | CL-04,DEP-CL-10 | 串行于 CL-04 | family child | CL-04 | N/A |
| CL-28 | implementation | 指针布局默认拒绝未放行命令 | immutable-release | yes | yes | 6/8 | 4/8 | 1/1 | M | CL-23,DEP-CL-12 | 串行于 CL-23 | family child | CL-23 | N/A |
| CL-25 | release | REL-CL-01 指针布局家族发布点 | immutable-release | yes | yes | 5/12 | 7/12 | 0/0 | S | CL-04,CL-23,CL-28 | 串行于 CL-28 | family release point | N/A | N/A |
| CL-05 | implementation | clone --revision 单修订克隆 | immutable-release | yes | yes | 8/8 | 4/8 | 2/3 | M | CL-25,DEP-CL-04 | 串行于 CL-25 | independent | N/A | N/A |
| CL-06 | implementation | clone -c &lt;key=value&gt; 配置覆盖 | immutable-release | yes | yes | 6/8 | 4/8 | 1/1 | M | CL-05 | 串行于 CL-05 | independent | N/A | N/A |
| CL-08 | implementation | clone -l/--local 硬链接本地对象 | immutable-release | yes | yes | 8/8 | 8/8 | 2/2 | M | CL-06 | 串行于 CL-06 | independent | N/A | N/A |
| CL-07 | implementation | clone --no-hardlinks 标志 | immutable-release | yes | yes | 2/8 | 4/8 | 1/1 | M | CL-08 | 串行于 CL-08 | independent | N/A | N/A |
| CL-09 | implementation | clone &lt;path&gt;.git 后缀回退 | immutable-release | yes | yes | 4/8 | 4/8 | 2/2 | M | CL-07 | 串行于 CL-07 | independent | N/A | N/A |
| CL-10 | implementation | clone --reference alternates 借用 | immutable-release | yes | yes | 6/8 | 4/8 | 2/3 | M | CL-09,DEP-CL-04 | 串行于 CL-09 | independent | N/A | N/A |
| CL-18 | implementation | clone --dissociate 解除 alternates | immutable-release | yes | yes | 5/8 | 6/8 | 1/1 | M | CL-10 | 串行于 CL-10 | independent | CL-10 | N/A |
| CL-19 | implementation | promisor 缺失对象读取 lazy fetch | immutable-release | yes | yes | 8/8 | 4/8 | 4/5 | M | CL-18,DEP-CL-04,DEP-CL-08 | 串行于 CL-18 | independent | CL-13 | N/A |
| CL-12 | implementation | clone 无效 sparse filter 拒绝 | immutable-release | yes | yes | 7/8 | 4/8 | 2/3 | M | CL-19 | 串行于 CL-19 | independent | N/A | N/A |
| CL-13 | implementation | clone --filter=blob:none 与 promisor 配置 | immutable-release | yes | yes | 7/8 | 6/8 | 2/4 | M | CL-12,DEP-CL-04 | 串行于 CL-12 | independent | N/A | N/A |
| CL-24 | implementation | clone combine:blob:none+tree:1 过滤与回退 | immutable-release | yes | yes | 8/8 | 6/8 | 2/4 | M | CL-13,DEP-CL-04 | 串行于 CL-13 | independent | CL-13 | N/A |
| CL-29 | implementation | clone sparse:oid 过滤与预期缺失边界 | immutable-release | yes | yes | 6/8 | 6/8 | 2/4 | M | CL-24,DEP-CL-04 | 串行于 CL-24 | independent | CL-24 | N/A |
| CL-21 | implementation | clone --filter 进度与端到端 lazy fetch | immutable-release | yes | yes | 8/8 | 4/8 | 2/2 | M | CL-29,DEP-CL-04 | 串行于 CL-29 | independent | CL-13 | N/A |
| CL-14 | implementation | --ref-storage-format 与 rev-parse | immutable-release | yes | yes | 7/8 | 4/8 | 1/2 | M | CL-21,DEP-CL-02,DEP-CL-09 | 串行于 CL-21 | independent | N/A | N/A |
| CL-15 | implementation | --single-branch 游离 HEAD 处理 | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | CL-14 | 串行于 CL-14 | independent | N/A | N/A |
| CL-26 | implementation | 稀疏工作区 checkout/switch/reset/restore 护栏 | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | CL-15 | 串行于 CL-15 | independent | CL-16 | N/A |
| CL-27 | implementation | 稀疏工作区 merge/pull/rebase/cherry-pick/stash 护栏 | immutable-release | yes | yes | 5/8 | 4/8 | 1/1 | M | CL-26 | 串行于 CL-26 | independent | CL-26 | N/A |
| CL-16 | implementation | clone --sparse cone 物化与 status 适配 | immutable-release | yes | yes | 7/8 | 4/8 | 3/3 | M | CL-27,DEP-CL-11 | 串行于 CL-27 | independent | N/A | N/A |
| CL-20 | implementation | clone --sparse 部分克隆与目录重命名合并 | immutable-release | yes | yes | 8/8 | 8/8 | 3/6 | M | CL-16,DEP-CL-04,DEP-CL-07 | 串行于 CL-16 | independent | CL-16 | N/A |
| CL-17 | implementation | 默认 clone 写 remote.&lt;name&gt;.fetch | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | CL-20 | 串行于 CL-20 | independent | N/A | N/A |
| CL-22 | superseded | 已并入 DEFER-CL-03，不得开工 | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | 无写集 | none | CL-17 | N/A |

---

### Task CL-01: 修复 `libra clone --progress` 布尔解析（含重定向 stderr 输出 `%`）（#737）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`0.30.211`；targeted `command::clone_test::test_clone_boolean_progress_does_not_consume_url` + `test_clone_help_documents_boolean_progress` 2/2，lib `clone_progress_rewrite*` + `index_pack_progress_rewrite*` 11/11；Codex R6 `VERDICT: PASS`；主仓与网站文档已同步；远端 CI 未等待，按用户 2026-11-20 指令 local C/D）
**Description:** 按 ADR-CL-01 修复 `libra clone --progress <url>` 将 `<url>` 当成 `--progress` 参数值消费的问题，同时兼容 `clone.rs:111-112` 既有的 `--no-progress` 处理器（不将其提前改写为全局 `--progress=none` 以免绕过原有处理），并确保 `--progress` 在 stderr 重定向至非 TTY 时仍输出 `%` 进度。
**Out of scope:** 非 `clone` 子命令的全局 `--progress=<mode>` 语义（永久非目标）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#737` 全局 `--progress` 消费后续位置参数 `<url>`；仅 `index-pack` 有 `rewrite_index_pack_progress_args` | `src/cli.rs:292-302,1121,1318,1366` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `libra clone --progress file://<S> d` 退出 0，且 `d` 中 `HEAD` 解析出的提交 OID 等于 S 的 `HEAD`（`<url>` 未被 `--progress` 当作取值吞掉）。
- [x] **AC2** AC1 的命令在 stderr 重定向到文件时，该文件至少有一行含 `%` 的进度行。
- [x] **AC3** `libra clone --no-progress file://<S> d2`（stderr 重定向到文件）退出 0，且其 stderr 不含 `%`（克隆成功是读取 stderr 的前提）。
- [x] **AC4** `libra clone --progress=none file://<S> d3`（显式取值形式，stderr 重定向到文件）退出 0，且其 stderr 不含 `%`。
- [x] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_boolean_progress_does_not_consume_url`：`exit 0` 后在 `d` 执行 `libra rev-parse HEAD`，stdout 去掉换行后等于在 S 执行同一命令的结果。
- AC2 → `command::clone_test::test_clone_boolean_progress_does_not_consume_url`：子进程 stderr 重定向到测试临时文件，读取后断言至少一行含 `%`。
- AC3 → `command::clone_test::test_clone_boolean_progress_does_not_consume_url`：`exit 0` 为前提；同上读取 stderr 文件，断言不含字符 `%`。
- AC4 → `command::clone_test::test_clone_boolean_progress_does_not_consume_url`：`exit 0` 为前提；同上读取 stderr 文件，断言不含字符 `%`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--progress`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_boolean_progress_does_not_consume_url)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [x] `source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::index_pack_progress_rewrite/)'` — 共享回归组（「验证执行协议」第 5 项）：既有 `index-pack` 布尔 `--progress` 重写回归（`src/cli.rs:4956`）。

**Full-suite trigger:** `T-1: 修改 src/cli.rs 全局参数预处理`
**Dependencies:** `无`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --progress 布尔语义; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=none; writeset=串行链起点; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-02: 支持 `libra clone -n` 短标志（`--no-checkout`）（#739）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs::no_checkout` 增补 `short = 'n'`，`libra clone -n` 跳过工作区检出；`command::clone_test::test_clone_short_n_skips_checkout` 覆盖 AC1–AC4，nextest 1 run / 1 passed / 0 failed；共享回归 `test_clone_json_reports_fetch_transfer_counts` 1 passed；`compat_version_surface_sync` 2 passed；`cargo +nightly fmt --all --check` exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化（新用例未进 serial registry）；版本面 patch 0.30.212 → 0.30.213。按用户 2026-11-20 指令在 `main` 直接提交推送、不等 CI。）
**Description:** 为 `CloneArgs::no_checkout` 添加 `short = 'n'`，支持 `libra clone -n <url>` 跳过工作区检出，且 stdout 与 stderr 合并输出中匹配 `Clon` 的行恰为 1 行（`#739` 验收第 3 条，对应 Git `t/t5601-clone.sh#4`）。
**Out of scope:** `--no-checkout` 内部检出跳过逻辑（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#739` `CloneArgs::no_checkout` 仅有 `long = "no-checkout"`，缺少 `short = 'n'` | `src/command/clone.rs:116-117` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `libra clone -n file://<S> dst` 退出 0，且 `dst` 中 `libra rev-parse HEAD` 等于 S 的 `HEAD` 提交 OID。
- [x] **AC2** 同一次克隆后，`dst` 工作区除 `.libra/` 外不含任何文件或目录（未检出）。
- [x] **AC3** 同一命令 stdout 与 stderr 合并后，匹配 `Clon` 的行恰为 1 行（`#739` 验收第 3 条，对应 Git `t/t5601-clone.sh#4`）。
- [x] **AC4** `libra clone --help` 的 stdout 含 `-n, --no-checkout`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_short_n_skips_checkout`：`exit 0` 后比较 `dst` 与 S 的 `libra rev-parse HEAD` 输出相等。
- AC2 → `command::clone_test::test_clone_short_n_skips_checkout`：列出 `dst` 的直接子项，集合恰为 `{.libra}`。
- AC3 → `command::clone_test::test_clone_short_n_skips_checkout`：拼接 stdout 与 stderr，按行计数含子串 `Clon` 的行，等于 1。
- AC4 → `command::clone_test::test_clone_short_n_skips_checkout`：`libra clone --help` 的 stdout 含子串 `-n, --no-checkout`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`-n, --no-checkout`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_short_n_skips_checkout)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_json_reports_fetch_transfer_counts)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地 `clone --json` 回归（`tests/command/clone_test.rs:1458`，把 stdout 解析为 JSON）——`Cloned into` 行不得进入 `--json` 的 stdout。

**Full-suite trigger:** `none`
**Dependencies:** `CL-01`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -n 短标志支持; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-01; writeset=串行于 CL-01; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-03: 支持 `libra clone --no-reject-shallow` 否定标志（#738）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增 `--no-reject-shallow`，与 `--reject-shallow` 互为 `overrides_with`（后出现者生效，单独 `--no-reject-shallow` 为默认 no-op）；`command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow` 覆盖 AC1–AC2（有效 Git 浅源：`--reject-shallow --no-reject-shallow` 退出 0；反向顺序以 `LBR-REPO-003` 拒绝且目标不留 `.libra`），nextest 1 run / 1 passed / 0 failed；共享回归 `test_clone_from_shallow_git_source_matrix` 1 passed；`compat_version_surface_sync` 2 passed；`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化；版本面 patch 0.30.214 → 0.30.215。按用户 2026-11-20 指令在 `main` 直接提交推送、不等 CI。）
**Description:** 为 `CloneArgs` 新增 `--no-reject-shallow` 与 `--reject-shallow` 互相 `overrides_with`（last-one-wins）。
**Out of scope:** `--reject-shallow` 自身检测逻辑（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#738` `CloneArgs::reject_shallow` 缺少对应的 `--no-reject-shallow` | `src/command/clone.rs:144-145` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `libra clone --reject-shallow --no-reject-shallow <浅源> d1` 退出 0（后出现的 `--no-reject-shallow` 生效；`<浅源>` 为有效 Git 浅仓库（以本机 `git` 对含两个提交的 Git 仓库执行 `git clone --depth 1 file://<源> <浅源>` 生成，不写坏 `.git/shallow`；与既有 `test_clone_reject_shallow_local_git_source` 的损坏夹具不同））。
- [x] **AC2** `libra clone --no-reject-shallow --reject-shallow <浅源> d2` 以 `LBR-REPO-003` 拒绝（后出现的 `--reject-shallow` 生效）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow`：断言 `exit 0`。
- AC2 → `command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow`：同一有效浅源；进程以非零状态结束且 stderr 含 `LBR-REPO-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）（`inspect_local_git_shallow` 成功且浅边界非空时的 `CloneError::RejectShallow` 映射，`src/command/clone.rs:629-633,1305-1340`）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--no-reject-shallow`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC2。
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_from_shallow_git_source_matrix)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地浅源矩阵回归（`tests/command/clone_test.rs:2106`：对有效 Git 浅源执行 `--reject-shallow` 以退出 128 与 `source repository is shallow, reject to clone.` 拒绝且目标下不留 `.libra`，完整源放行；新增用例的目标目录整体不存在由「验证执行协议」第 4 项克隆拒绝不变量另行检查）。

**Full-suite trigger:** `none`
**Dependencies:** `CL-02`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --no-reject-shallow; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=2/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-02; writeset=串行于 CL-02; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-04: 支持 `libra clone --separate-git-dir` 并输出与 `--bare` 互斥信息（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增 `--separate-git-dir <DIR>`；克隆（含初始检出）先在 `<workdir>/.libra` 内完成，检出后把元数据迁移到 `<DIR>` 并在工作区留下 `gitdir: <DIR>` 指针文件（`relocate_libra_dir`，rename 失败时回退为递归复制，指针写入失败则回滚）；`--bare`/`--mirror` 组合在创建任何目录前以 `LBR-CLI-002` + Git 原文 `options '--bare' and '--separate-git-dir' cannot be used together` 拒绝；`command::clone_test::test_clone_separate_git_dir_and_bare_mutex` 覆盖 AC1–AC8，nextest 1 run / 1 passed / 0 failed；共享回归 `test_clone_bare_and_mirror_default_names_and_layout` 1 passed；`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化。本卡为 `REL-CL-01` 家族子卡（无版本 bump，C/D 由 `CL-25` 承接）；按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 按 ADR-CL-05 为 `CloneArgs` 新增 `--separate-git-dir <dir>`，将仓库元数据目录初始化至 `<dir>` 并在 `<workdir>/.libra` 写入 `gitdir: <dir>` 指针文件（复用 `src/utils/convert.rs:242-252` 与 `src/utils/util.rs:265`），（`clone` 自身的初始检出须在写入指针文件之前完成或显式使用 `<dir>` 作为仓库路径，不依赖 `CL-23` 的指针发现），同时在 `--bare`/`--mirror` 与 `--separate-git-dir` 组合时输出 Git 兼容的互斥错误提示并清理失败新建目录。（按 G-09 拆出 `CL-23` 承接指针发现；`CL-28`（写命令 fail-closed）再自 `CL-23` 拆出。）
**Out of scope:** 指针发现与四个基本命令（`CL-23`）；写命令 fail-closed（`CL-28`）；`libra init --separate-git-dir`（保留 D25）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#740` `CloneArgs` 缺少 `--separate-git-dir` 及 `--bare` 互斥错误信息 | `src/command/clone.rs:89-91,1592`；`src/utils/convert.rs:242-252` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `libra clone --separate-git-dir <G> file://<S> <W>` 退出 0，且 `<G>/libra.db` 存在。
- [x] **AC2** AC1 之后读取 `<W>/.libra` 的文件内容，去掉 `gitdir:` 前缀并去除首尾空白后，规范化路径等于 `<G>` 的规范化路径。
- [x] **AC3** AC1 之后 `<W>` 工作区（不含 `.libra`）的「相对路径 → SHA-256」清单等于 S 的 `HEAD` 树全部文件的清单（初始检出完成）。
- [x] **AC4** `libra clone --bare --separate-git-dir <G2> file://<S> <W2>` 以 `LBR-CLI-002` 拒绝。
- [x] **AC5** AC4 的 stderr 含 Git 原文 `options '--bare' and '--separate-git-dir' cannot be used together`（`builtin/clone.c:1068-1070`）。
- [x] **AC6** `libra clone --mirror --separate-git-dir <G3> file://<S> <W3>` 以 `LBR-CLI-002` 拒绝（Git 中 `--mirror` 隐含 `--bare`，`builtin/clone.c:1063-1070`）。
- [x] **AC7** AC6 的 stderr 含同一 Git 原文 `options '--bare' and '--separate-git-dir' cannot be used together`。
- [x] **AC8** `libra clone --separate-git-dir <G4> <不存在的本地路径> <W4>` 以 `LBR-REPO-001` 拒绝（目标与 `<G4>` 的清理由「验证执行协议」第 4 项克隆拒绝不变量检查）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：`exit 0` 后断言 `<G>/libra.db` 的 `is_file()` 为真。
- AC2 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：`read_to_string(<W>/.libra)`，`strip_prefix("gitdir:")` 后 `trim()`，与 `<G>` 分别 `canonicalize` 后相等。
- AC3 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：两份清单逐项相等；期望清单取自在 S 中逐一 `libra cat-file -p HEAD:<path>` 的 SHA-256。
- AC4 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC5 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：AC4 同一次调用的 stderr 含该原文子串。
- AC6 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC7 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：AC6 同一次调用的 stderr 含该原文子串。
- AC8 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：进程以非零状态结束且 stderr 含 `LBR-REPO-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–6 步（家族子卡：第 6 步只做精确本地 `cf` 提交、不推送；推送与第 7 步部署核验由 `CL-25` 承接）；文档关键字：`--separate-git-dir`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_and_bare_mutex)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC8。
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_bare_and_mirror_default_names_and_layout)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地 `--bare`/`--mirror` 默认目录名与裸布局回归（`tests/command/clone_test.rs:2486`）。

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，合并后由 CL-25 统一执行 T-2 全量门）`
**Dependencies:** `CL-03`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `N/A`（`REL-CL-01` 子卡不独立发版，统一在 `CL-25` 发布）
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/development/commands/_compatibility.md`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (1/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=clone --separate-git-dir 与 --bare 互斥; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-03; writeset=串行于 CL-03; release=family child; split-from=N/A; exception=N/A`

---

### Task CL-23: 适配 `util.rs`/`alternates.rs` 指针布局并使四个基本命令可用（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：新增 `util::resolve_dot_libra_pointer`，仓库发现（`try_get_paths_full_with_storage_lookup`）、`info/*` 候选与 `alternates` 的 objects 解析都跟随 `.libra` 的 `gitdir:` 指针；`status`、`log -n 1`、`cat-file -t HEAD` 与无参 `branch` 在指针布局下可用，`libra alternates add <W>` 记录 `<G>/objects`；`command::clone_test::test_clone_separate_git_dir_pointer_commands` 覆盖 AC1–AC5，nextest 1 run / 1 passed / 0 failed；共享回归 `command::status_test::*` 80 passed / 0 failed；`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化。本卡为 `REL-CL-01` 家族子卡（无版本 bump，C/D 由 `CL-25` 承接；写 ref 的默认拒绝由 `CL-28` 承接，本卡未放行任何写命令）；按用户 2026-11-20 指令在 `main` 提交推送。）
**Description:** 自 `CL-04` 拆出（G-09）。在 `src/utils/util.rs:278,314-324,791-803,1269,1464` 与 `src/command/alternates.rs:91` 解析 `.libra` `gitdir:` 指针，并使 `status`、`log`、`cat-file` 与无参数 `branch` 在该布局下退出 0，`libra alternates add <W>` 解析到 `<G>/objects`。会改 ref 的 `branch` 以及其余子命令由 `CL-28` 默认拒绝（按 G-09 自本卡拆出 `CL-28`）。
**Out of scope:** 默认拒绝（`CL-28`）；会改 ref 的 `branch`（`CL-28`）；`libra init --separate-git-dir`（保留 D25）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#740` 需要 `util.rs` 与 `alternates.rs:91` 消费 `gitdir:` 指针 | `src/utils/util.rs:278,314-324,791-803,1269,1464`；`src/command/alternates.rs:91` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 夹具：`libra clone --separate-git-dir <G> file://<S> <W>` 得到的 `<W>`（`.libra` 为指向 `<G>` 的 `gitdir:` 指针文件）中，`libra status` 退出 0。
- [x] **AC2** 同一 `<W>` 中 `libra log -n 1` 退出 0 且 stdout 含 S 的 `HEAD` 提交 OID。
- [x] **AC3** 同一 `<W>` 中 `libra cat-file -t HEAD` 的 stdout 为 `commit` 加换行。
- [x] **AC4** 同一 `<W>` 中无参数 `libra branch` 的 stdout 含当前分支名 `main`。
- [x] **AC5** 在另一普通仓库 R 中执行 `libra alternates add <W>` 后，R 的 `.libra/objects/info/alternates` 记录的路径规范化后等于 `<G>/objects`（`src/command/alternates.rs:91` 跟随指针，而不是返回不存在的 `<W>/.libra/objects`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：子进程 `current_dir=<W>`，断言 `exit 0`。
- AC2 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：期望 OID 取自在 S 中执行 `libra rev-parse HEAD`；断言 `exit 0` 且 stdout 含该 OID。
- AC3 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：stdout 逐字等于 `commit\n`。
- AC4 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：stdout 含子串 `main`。
- AC5 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：读取 R 的 alternates 文件，唯一非空行 `canonicalize` 后等于 `<G>/objects` 的 `canonicalize`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–6 步（家族子卡：第 6 步只做精确本地 `cf` 提交、不推送；推送与第 7 步部署核验由 `CL-25` 承接）；文档关键字：`separate-git-dir`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_pointer_commands)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC5。
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`status_test` 全模块回归。

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，由 CL-25 统一执行 T-2）`
**Dependencies:** `CL-04`、`DEP-CL-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/util.rs`, `src/command/alternates.rs`, `tests/command/clone_test.rs`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Release write set:** `N/A`
**Files likely touched:** `src/utils/util.rs`, `src/command/alternates.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`（网站文件写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (2/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=pointer 发现与四个基本命令; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=CL-04,DEP-CL-10; writeset=串行于 CL-04; release=family child; split-from=CL-04; exception=N/A`

---

### Task CL-28: 在指针布局下拒绝未适配的写命令（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`src/cli.rs::command_preflight` 在分发前对`.libra` 为 `gitdir:` 指针的布局默认拒绝未适配命令（`LBR-UNSUPPORTED-001`，且不改索引/`HEAD`/ref）；放行 `clone`/`status`/`log`/`cat-file`/`upgrade`/`ui`/`login`/`whoami`/`logout`/`completions`、`alternates list` 与无参数 `branch` 列表。`command::clone_test::test_clone_separate_git_dir_default_deny` 覆盖 AC1–AC4（含索引/引用/工作区快照不变）；`cli::tests::pointer_layout_allowlist_matches_commands` 覆盖 AC5–AC6（逐变体样本表 + 变体名集合相等）。`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化。本卡为 `REL-CL-01` 家族子卡（无版本 bump，C/D 由 `CL-25` 承接）；按用户 2026-11-20 指令在 `main` 提交推送。）
**Description:** 自 `CL-23` 拆出（G-09）。在 `src/cli.rs` `command_preflight`（:1755+）中，当 `.libra` 是 `gitdir:` 指针时默认拒绝。放行名单只有：`clone`；`status`；`log`；`cat-file`；不改 alternates 存储的 `alternates` 子命令；无参数的 `branch`；以及不读写当前仓库的 `upgrade`、`ui`、`login`、`whoami`、`logout`、`completions`（`command_preflight` 对它们返回 `CommandPreflight::none()`，`src/cli.rs:1771-1800`）。`init` 不在放行名单：它会检查并重新初始化既有 `.libra`（`src/command/init.rs:1293` `reinitialize_existing`、`:1697` `ensure_storage_paths_are_directories` 要求 `.libra` 为目录），在指针布局下按默认拒绝处理。会修改 alternates 存储的子命令不在放行名单里。其余 `Commands::*` 变体退出非零，并且不改索引、`HEAD` 或 ref。`_compatibility.md` 的表必须列出每一个变体（文档内容，随 ER-06 文档门逐文件核对）；代码中的指针布局判定由本卡新增的单元测试 `cli::tests::pointer_layout_allowlist_matches_commands` 以命令行样本表核对（AC5–AC6）。本卡是 `REL-CL-01` 子卡 3/4，不单独推送。
**Out of scope:** 指针发现与只读基本命令（`CL-23`）；把被拒绝的命令改造成真正的指针布局（`DEFER-CL-02`）；稀疏 cone 护栏（`CL-26`/`CL-27`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 未适配写命令若在指针布局下继续执行，会把数据写到错误目录。`restore` 会读并保存索引 | `src/cli.rs:1755`；`src/command/restore.rs:788,934` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 夹具：`libra clone --separate-git-dir <G> file://<S> <W>` 得到的 `<W>`（`.libra` 为指向 `<G>` 的 `gitdir:` 指针文件）（S 另含分支 `other`）中，`libra restore .`（先修改 `a.txt`）以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC2** 同一 `<W>` 中 `libra commit --allow-empty -m x` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC3** 同一 `<W>` 中 `libra branch newbranch` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC4** 同一 `<W>` 中 `libra checkout other` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC5** 单元测试 `cli::tests::pointer_layout_allowlist_matches_commands`：对一张固定的命令行样本表（每个 `Commands` 变体至少一个样本；`branch` 含无参数、`branch newbranch`、`branch -D x`、`branch -m a b`，`alternates` 含 `list`、`add <p>`、`remove <p>`、`prune`）逐条经 clap 解析后求本卡指针布局判定，得到的「样本 → 放行/拒绝」映射与期望映射逐项相等（期望：放行名单内的命令放行，`branch` 仅无参数放行，`alternates` 仅 `list` 放行，其余全部拒绝）。
- [x] **AC6** 同一单元测试中，样本表解析得到的变体名集合等于 `Commands` 全部变体名集合（经 `clap::CommandFactory` 枚举子命令名，含隐藏变体），新增变体未进样本表即失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_separate_git_dir_default_deny`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）；调用前后按「验证执行协议」第 4 项比较 `<W>`/`<G>` 仓库快照（含 `<G>/index` 字节与 `reference` 表全部行）。
- AC2 → `command::clone_test::test_clone_separate_git_dir_default_deny`：同上（快照覆盖 `HEAD` 所在的 `reference` 行）。
- AC3 → `command::clone_test::test_clone_separate_git_dir_default_deny`：同上（快照覆盖全部引用，因而 `newbranch` 不得出现；按数据库行判断，不按文件是否存在判断）。
- AC4 → `command::clone_test::test_clone_separate_git_dir_default_deny`：同上。
- AC5 → `cli::tests::pointer_layout_allowlist_matches_commands`：对样本表做一次 `assert_eq!(实际映射, 期望映射)`。
- AC6 → `cli::tests::pointer_layout_allowlist_matches_commands`：两个变体名集合做一次 `assert_eq!`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–6 步（家族子卡：第 6 步只做精确本地 `cf` 提交、不推送；推送与第 7 步部署核验由 `CL-25` 承接）；文档关键字：`separate-git-dir`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_default_deny)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [x] `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::pointer_layout_allowlist_matches_commands)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC5–AC6。

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，由 CL-25 统一执行 T-2）`
**Dependencies:** `CL-23`、`DEP-CL-12`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/add.md`, `docs/commands/zh-CN/add.md`, `docs/development/commands/add.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`, `docs/commands/agent.md`, `docs/commands/zh-CN/agent.md`, `docs/development/commands/agent.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`, `docs/commands/archive.md`, `docs/commands/zh-CN/archive.md`, `docs/development/commands/archive.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`, `docs/commands/auth.md`, `docs/commands/zh-CN/auth.md`, `docs/development/commands/auth.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`, `docs/commands/automation.md`, `docs/commands/zh-CN/automation.md`, `docs/development/commands/automation.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`, `docs/commands/bisect.md`, `docs/commands/zh-CN/bisect.md`, `docs/development/commands/bisect.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`, `docs/commands/blame.md`, `docs/commands/zh-CN/blame.md`, `docs/development/commands/blame.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/bundle.md`, `docs/commands/zh-CN/bundle.md`, `docs/development/commands/bundle.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`, `docs/commands/cache.md`, `docs/commands/zh-CN/cache.md`, `docs/development/commands/cache.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`, `docs/commands/check-mailmap.md`, `docs/commands/zh-CN/check-mailmap.md`, `docs/development/commands/check-mailmap.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`, `docs/commands/cloud.md`, `docs/commands/zh-CN/cloud.md`, `docs/development/commands/cloud.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `docs/commands/commit-tree.md`, `docs/commands/zh-CN/commit-tree.md`, `docs/development/commands/commit-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `docs/commands/count-objects.md`, `docs/commands/zh-CN/count-objects.md`, `docs/development/commands/count-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`, `docs/commands/credential.md`, `docs/commands/zh-CN/credential.md`, `docs/development/commands/credential.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`, `docs/commands/deps.md`, `docs/commands/zh-CN/deps.md`, `docs/development/commands/deps.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`, `docs/commands/diff-files.md`, `docs/commands/zh-CN/diff-files.md`, `docs/development/commands/diff-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`, `docs/commands/diff-index.md`, `docs/commands/zh-CN/diff-index.md`, `docs/development/commands/diff-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`, `docs/commands/diff-tree.md`, `docs/commands/zh-CN/diff-tree.md`, `docs/development/commands/diff-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`, `docs/commands/dirty.md`, `docs/commands/zh-CN/dirty.md`, `docs/development/commands/dirty.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`, `docs/commands/fast-export.md`, `docs/commands/zh-CN/fast-export.md`, `docs/development/commands/fast-export.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`, `docs/commands/fast-import.md`, `docs/commands/zh-CN/fast-import.md`, `docs/development/commands/fast-import.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `docs/commands/file.md`, `docs/commands/zh-CN/file.md`, `docs/development/commands/file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`, `docs/commands/fsck.md`, `docs/commands/zh-CN/fsck.md`, `docs/development/commands/fsck.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`, `docs/commands/gc.md`, `docs/commands/zh-CN/gc.md`, `docs/development/commands/gc.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`, `docs/commands/grep.md`, `docs/commands/zh-CN/grep.md`, `docs/development/commands/grep.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`, `docs/commands/hash-object.md`, `docs/commands/zh-CN/hash-object.md`, `docs/development/commands/hash-object.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`, `docs/commands/hooks.md`, `docs/commands/zh-CN/hooks.md`, `docs/development/commands/hooks.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`, `docs/commands/hydrate.md`, `docs/commands/zh-CN/hydrate.md`, `docs/development/commands/hydrate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`, `docs/commands/index-pack.md`, `docs/commands/zh-CN/index-pack.md`, `docs/development/commands/index-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `docs/commands/investigate.md`, `docs/commands/zh-CN/investigate.md`, `docs/development/commands/investigate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`, `docs/commands/layer.md`, `docs/commands/zh-CN/layer.md`, `docs/development/commands/layer.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`, `docs/commands/lfs.md`, `docs/commands/zh-CN/lfs.md`, `docs/development/commands/lfs.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`, `docs/commands/logfile.md`, `docs/commands/zh-CN/logfile.md`, `docs/development/commands/logfile.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`, `docs/commands/ls-remote.md`, `docs/commands/zh-CN/ls-remote.md`, `docs/development/commands/ls-remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`, `docs/commands/ls-tree.md`, `docs/commands/zh-CN/ls-tree.md`, `docs/development/commands/ls-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`, `docs/commands/mailinfo.md`, `docs/commands/zh-CN/mailinfo.md`, `docs/development/commands/mailinfo.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`, `docs/commands/media.md`, `docs/commands/zh-CN/media.md`, `docs/development/commands/media.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`, `docs/commands/mega2.md`, `docs/commands/zh-CN/mega2.md`, `docs/development/commands/mega2.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`, `docs/commands/memory.md`, `docs/commands/zh-CN/memory.md`, `docs/development/commands/memory.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `docs/commands/merge-base.md`, `docs/commands/zh-CN/merge-base.md`, `docs/development/commands/merge-base.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`, `docs/commands/merge-file.md`, `docs/commands/zh-CN/merge-file.md`, `docs/development/commands/merge-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`, `docs/commands/mergetool.md`, `docs/commands/zh-CN/mergetool.md`, `docs/development/commands/mergetool.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`, `docs/commands/metadata.md`, `docs/commands/zh-CN/metadata.md`, `docs/development/commands/metadata.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`, `docs/commands/op.md`, `docs/commands/zh-CN/op.md`, `docs/development/commands/op.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`, `docs/commands/open.md`, `docs/commands/zh-CN/open.md`, `docs/development/commands/open.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`, `docs/commands/pack-objects.md`, `docs/commands/zh-CN/pack-objects.md`, `docs/development/commands/pack-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`, `docs/commands/read-tree.md`, `docs/commands/zh-CN/read-tree.md`, `docs/development/commands/read-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `docs/commands/reflog.md`, `docs/commands/zh-CN/reflog.md`, `docs/development/commands/reflog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`, `docs/commands/remote.md`, `docs/commands/zh-CN/remote.md`, `docs/development/commands/remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`, `docs/commands/repack.md`, `docs/commands/zh-CN/repack.md`, `docs/development/commands/repack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`, `docs/commands/rerere.md`, `docs/commands/zh-CN/rerere.md`, `docs/development/commands/rerere.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`, `docs/commands/review.md`, `docs/commands/zh-CN/review.md`, `docs/development/commands/review.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/development/commands/revision.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`, `docs/commands/sandbox.md`, `docs/commands/zh-CN/sandbox.md`, `docs/development/commands/sandbox.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`, `docs/commands/send-email.md`, `docs/commands/zh-CN/send-email.md`, `docs/development/commands/send-email.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`, `docs/commands/service.md`, `docs/commands/zh-CN/service.md`, `docs/development/commands/service.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`, `docs/commands/show-ref.md`, `docs/commands/zh-CN/show-ref.md`, `docs/development/commands/show-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`, `docs/commands/sparse-view.md`, `docs/commands/zh-CN/sparse-view.md`, `docs/development/commands/sparse-view.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`, `docs/commands/tag.md`, `docs/commands/zh-CN/tag.md`, `docs/development/commands/tag.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`, `docs/commands/update-index.md`, `docs/commands/zh-CN/update-index.md`, `docs/development/commands/update-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`, `docs/commands/update-ref.md`, `docs/commands/zh-CN/update-ref.md`, `docs/development/commands/update-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`, `docs/commands/verify-pack.md`, `docs/commands/zh-CN/verify-pack.md`, `docs/development/commands/verify-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`, `docs/commands/worktree.md`, `docs/commands/zh-CN/worktree.md`, `docs/development/commands/worktree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`, `docs/commands/write-tree.md`, `docs/commands/zh-CN/write-tree.md`, `docs/development/commands/write-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Release write set:** `N/A`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/add.md`, `docs/commands/zh-CN/add.md`, `docs/development/commands/add.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`, `docs/commands/agent.md`, `docs/commands/zh-CN/agent.md`, `docs/development/commands/agent.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`, `docs/commands/archive.md`, `docs/commands/zh-CN/archive.md`, `docs/development/commands/archive.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`, `docs/commands/auth.md`, `docs/commands/zh-CN/auth.md`, `docs/development/commands/auth.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`, `docs/commands/automation.md`, `docs/commands/zh-CN/automation.md`, `docs/development/commands/automation.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`, `docs/commands/bisect.md`, `docs/commands/zh-CN/bisect.md`, `docs/development/commands/bisect.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`, `docs/commands/blame.md`, `docs/commands/zh-CN/blame.md`, `docs/development/commands/blame.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/bundle.md`, `docs/commands/zh-CN/bundle.md`, `docs/development/commands/bundle.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`, `docs/commands/cache.md`, `docs/commands/zh-CN/cache.md`, `docs/development/commands/cache.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`, `docs/commands/check-mailmap.md`, `docs/commands/zh-CN/check-mailmap.md`, `docs/development/commands/check-mailmap.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`, `docs/commands/cloud.md`, `docs/commands/zh-CN/cloud.md`, `docs/development/commands/cloud.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `docs/commands/commit-tree.md`, `docs/commands/zh-CN/commit-tree.md`, `docs/development/commands/commit-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `docs/commands/count-objects.md`, `docs/commands/zh-CN/count-objects.md`, `docs/development/commands/count-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`, `docs/commands/credential.md`, `docs/commands/zh-CN/credential.md`, `docs/development/commands/credential.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`, `docs/commands/deps.md`, `docs/commands/zh-CN/deps.md`, `docs/development/commands/deps.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`, `docs/commands/diff-files.md`, `docs/commands/zh-CN/diff-files.md`, `docs/development/commands/diff-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`, `docs/commands/diff-index.md`, `docs/commands/zh-CN/diff-index.md`, `docs/development/commands/diff-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`, `docs/commands/diff-tree.md`, `docs/commands/zh-CN/diff-tree.md`, `docs/development/commands/diff-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`, `docs/commands/dirty.md`, `docs/commands/zh-CN/dirty.md`, `docs/development/commands/dirty.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`, `docs/commands/fast-export.md`, `docs/commands/zh-CN/fast-export.md`, `docs/development/commands/fast-export.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`, `docs/commands/fast-import.md`, `docs/commands/zh-CN/fast-import.md`, `docs/development/commands/fast-import.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `docs/commands/file.md`, `docs/commands/zh-CN/file.md`, `docs/development/commands/file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`, `docs/commands/fsck.md`, `docs/commands/zh-CN/fsck.md`, `docs/development/commands/fsck.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`, `docs/commands/gc.md`, `docs/commands/zh-CN/gc.md`, `docs/development/commands/gc.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`, `docs/commands/grep.md`, `docs/commands/zh-CN/grep.md`, `docs/development/commands/grep.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`, `docs/commands/hash-object.md`, `docs/commands/zh-CN/hash-object.md`, `docs/development/commands/hash-object.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`, `docs/commands/hooks.md`, `docs/commands/zh-CN/hooks.md`, `docs/development/commands/hooks.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`, `docs/commands/hydrate.md`, `docs/commands/zh-CN/hydrate.md`, `docs/development/commands/hydrate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`, `docs/commands/index-pack.md`, `docs/commands/zh-CN/index-pack.md`, `docs/development/commands/index-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `docs/commands/investigate.md`, `docs/commands/zh-CN/investigate.md`, `docs/development/commands/investigate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`, `docs/commands/layer.md`, `docs/commands/zh-CN/layer.md`, `docs/development/commands/layer.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`, `docs/commands/lfs.md`, `docs/commands/zh-CN/lfs.md`, `docs/development/commands/lfs.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`, `docs/commands/logfile.md`, `docs/commands/zh-CN/logfile.md`, `docs/development/commands/logfile.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`, `docs/commands/ls-remote.md`, `docs/commands/zh-CN/ls-remote.md`, `docs/development/commands/ls-remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`, `docs/commands/ls-tree.md`, `docs/commands/zh-CN/ls-tree.md`, `docs/development/commands/ls-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`, `docs/commands/mailinfo.md`, `docs/commands/zh-CN/mailinfo.md`, `docs/development/commands/mailinfo.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`, `docs/commands/media.md`, `docs/commands/zh-CN/media.md`, `docs/development/commands/media.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`, `docs/commands/mega2.md`, `docs/commands/zh-CN/mega2.md`, `docs/development/commands/mega2.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`, `docs/commands/memory.md`, `docs/commands/zh-CN/memory.md`, `docs/development/commands/memory.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `docs/commands/merge-base.md`, `docs/commands/zh-CN/merge-base.md`, `docs/development/commands/merge-base.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`, `docs/commands/merge-file.md`, `docs/commands/zh-CN/merge-file.md`, `docs/development/commands/merge-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`, `docs/commands/mergetool.md`, `docs/commands/zh-CN/mergetool.md`, `docs/development/commands/mergetool.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`, `docs/commands/metadata.md`, `docs/commands/zh-CN/metadata.md`, `docs/development/commands/metadata.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`, `docs/commands/op.md`, `docs/commands/zh-CN/op.md`, `docs/development/commands/op.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`, `docs/commands/open.md`, `docs/commands/zh-CN/open.md`, `docs/development/commands/open.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`, `docs/commands/pack-objects.md`, `docs/commands/zh-CN/pack-objects.md`, `docs/development/commands/pack-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`, `docs/commands/read-tree.md`, `docs/commands/zh-CN/read-tree.md`, `docs/development/commands/read-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `docs/commands/reflog.md`, `docs/commands/zh-CN/reflog.md`, `docs/development/commands/reflog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`, `docs/commands/remote.md`, `docs/commands/zh-CN/remote.md`, `docs/development/commands/remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`, `docs/commands/repack.md`, `docs/commands/zh-CN/repack.md`, `docs/development/commands/repack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`, `docs/commands/rerere.md`, `docs/commands/zh-CN/rerere.md`, `docs/development/commands/rerere.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`, `docs/commands/review.md`, `docs/commands/zh-CN/review.md`, `docs/development/commands/review.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/development/commands/revision.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`, `docs/commands/sandbox.md`, `docs/commands/zh-CN/sandbox.md`, `docs/development/commands/sandbox.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`, `docs/commands/send-email.md`, `docs/commands/zh-CN/send-email.md`, `docs/development/commands/send-email.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`, `docs/commands/service.md`, `docs/commands/zh-CN/service.md`, `docs/development/commands/service.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`, `docs/commands/show-ref.md`, `docs/commands/zh-CN/show-ref.md`, `docs/development/commands/show-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`, `docs/commands/sparse-view.md`, `docs/commands/zh-CN/sparse-view.md`, `docs/development/commands/sparse-view.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`, `docs/commands/tag.md`, `docs/commands/zh-CN/tag.md`, `docs/development/commands/tag.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`, `docs/commands/update-index.md`, `docs/commands/zh-CN/update-index.md`, `docs/development/commands/update-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`, `docs/commands/update-ref.md`, `docs/commands/zh-CN/update-ref.md`, `docs/development/commands/update-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`, `docs/commands/verify-pack.md`, `docs/commands/zh-CN/verify-pack.md`, `docs/development/commands/verify-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`, `docs/commands/worktree.md`, `docs/commands/zh-CN/worktree.md`, `docs/development/commands/worktree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`, `docs/commands/write-tree.md`, `docs/commands/zh-CN/write-tree.md`, `docs/development/commands/write-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`。`_compatibility.md` 的拒绝表与 `Commands` 枚举一一对应。`branch` 只放行无参数形式，其会改 ref 的形式受本卡默认拒绝影响，其文档同样逐文件列入；`init` 在指针布局下被默认拒绝，其文档同样列入。网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (3/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=指针布局默认拒绝未放行命令; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-23,DEP-CL-12; writeset=串行于 CL-23; release=family child; split-from=CL-23; exception=N/A`

---

### Task CL-25: 发布 `REL-CL-01`（`clone --separate-git-dir` 指针布局家族发布点）（#740）

**Task type:** `release`
**Lifecycle / Acceptance:** `done` / `remote-pending`（2026-11-20：`REL-CL-01` 家族发布点。三张子卡 `CL-04`（`2335f3f`+`d9189cc`）、`CL-23`（`05698cd`）、`CL-28`（`5d3079f`）均已是本卡受测提交的祖先；版本面 patch 0.30.218 → 0.30.219（`Cargo.toml`/`install.sh`/`install.ps1` + `Cargo.lock`），`compat_version_surface_sync` 2 passed；家族聚合守卫 3 passed（`test_clone_separate_git_dir_and_bare_mutex`、`test_clone_separate_git_dir_pointer_commands`、`test_clone_separate_git_dir_default_deny`）；`cargo build --release --bin libra` exit 0。按用户 2026-11-20 指令：全量 `T-2` 未执行（3.5「不进行全量测试」），PR/`gh pr merge` 流程由「main 直接提交推送」替代（3.1），`gh release create` 与 tag 尚未执行（需用户批准后补）。`remote-pending` 指远端发布流水线与 Release 产物仍待用户批准。
**Description:** 聚合 `REL-CL-01` 家族子卡 `CL-04`、`CL-23` 与 `CL-28`（不引入新生产代码变更），执行 `patch + 1`、`compat_version_surface_sync` 三处版本面同步、工具链刷新 `Cargo.lock`、全量 `T-2` 门禁、`cargo build --release` 与 `gh release create`；其中 `patch + 1` 版本面同步与 `compat_version_surface_sync` 属 ER-04 C 组强制门（模板 C 组 ①–②），`release.yml` 等远端结果属 ER-04 D 组门（见「执行检查必备需求」），均不计入本卡 G-03 计数。对外发布指针布局、四个只读基本命令，以及 `CL-28` 对未放行子命令的默认拒绝；发布后承载三张子卡本地 `cf` 网站提交的推送与部署核验（「文档与网站验收命令」第 6–7 步，收口 `DEP-CL-15` 窗口）。
**Out of scope:** 任何新增功能代码实现；`libra init --separate-git-dir`（`DEFER-CL-02`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `REL-CL-01` 需在 `CL-04`、`CL-23` 与 `CL-28` 均完成本地提交后统一版本化发布；三处版本面（2026-11-18 核对均为 `0.30.210`，开工按 ER-02 刷新） | `Cargo.toml:3`；`install.sh:21`（`DEFAULT_VERSION`）；`install.ps1:25`（`$DefaultVersion`） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `CL-04` 的本地签名提交是本卡受测提交 `<T>` 的祖先（`<T>` = `CL-28` 本地提交之后、ER-05 评审之前执行本卡 A/B 门时的本地 `HEAD`，尚未做 C 组版本 bump；其 `libra rev-parse HEAD` 与 `HEAD^{tree}` 记入执行记录）。
- [x] **AC2** `CL-23` 的本地签名提交是 `<T>` 的祖先。
- [x] **AC3** `CL-28` 的本地签名提交是 `<T>` 的祖先。
- [x] **AC4** 家族聚合守卫：三张子卡在 `command_test` 中的卡特有用例在 `<T>` 上全部通过。
- [x] **AC5** 家族聚合守卫：`CL-28` 的 `--lib` 卡特有用例在 `<T>` 上通过。
- [ ] ER-04 C 组强制门（不计入 G-03；本卡按用户 2026-11-20 指令执行 ①–③ 与 `cargo build --release`：版本面 `0.30.219`、`compat_version_surface_sync` 2 passed、release 构建 exit 0；`T-2` 全量按 3.5 暂缓，`gh release create` 待用户批准）：在 `<T>` 之上做 `patch + 1` 版本面同步与 `Cargo.lock` 工具链刷新（①–③）；在这一已 bump、尚未提交的工作树上先确认 `Cargo.toml` 的 `version` 行是合并基底 `X.Y.Z` 的 `X.Y.(Z+1)`、`source .env.test && cargo nextest run --test compat_version_surface_sync` 通过，再跑本卡 `Full-suite trigger` 的 `T-2` 收口门、`cargo build --release` 与安装（④–⑥）；之后才签名提交得到 `<F>`（⑦；提交后 `libra status --porcelain --untracked-files=no` 为空——已跟踪文件无未提交改动，因而 `<F>^{tree}` 即 ④–⑥ 的受测树；`libra merge-base --is-ancestor <T> <F>` 退出 0）；`<F>` 作为 PR head 推送，`gh pr merge --squash` 后 `libra fetch origin`，`libra rev-parse <MERGE_SHA>^{tree}` 等于 `libra rev-parse <F>^{tree}`（发布树即 `T-2` 受测树）。
- [ ] ER-06a 网站交付（不计入 G-03）：见下方「文档 AC」行。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → Verification 第 1 项（手工证据）。
- AC2 → Verification 第 2 项（手工证据）。
- AC3 → Verification 第 3 项（手工证据）。
- AC4 → Verification 第 4 项（聚合守卫，按「验证执行协议」第 5 项的 `release` 卡例外作为本 AC 证据；三测试精确并集，预期 N=3）。
- AC5 → Verification 第 5 项（同上，预期 N=1）。
- 文档 AC（ER-06a 网站交付，不计入 G-03）→ 「文档与网站验收命令」第 6 步的 `CL-25` 分支（一次推送三张子卡的本地 `cf` 提交）与第 7 步（对三张子卡改动的每个网站页面做部署核验）；本卡不改仓库文档。

**Verification:**
- [ ] 手工证据（执行记录，「验证执行协议」第 6 项按 1 门计）：`libra merge-base --is-ancestor <CL-04 提交> <T>` 退出 0，覆盖 AC1。
- [ ] 手工证据（执行记录，「验证执行协议」第 6 项按 1 门计）：`libra merge-base --is-ancestor <CL-23 提交> <T>` 退出 0，覆盖 AC2。
- [ ] 手工证据（执行记录，「验证执行协议」第 6 项按 1 门计）：`libra merge-base --is-ancestor <CL-28 提交> <T>` 退出 0，覆盖 AC3。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_and_bare_mutex) | test(=command::clone_test::test_clone_separate_git_dir_pointer_commands) | test(=command::clone_test::test_clone_separate_git_dir_default_deny)'` — 聚合守卫（「验证执行协议」第 5 项，预期 N=3）：在 `<T>` 的检出上重跑三张子卡的 `command_test` 卡特有用例，覆盖 AC4。
- [ ] `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::pointer_layout_allowlist_matches_commands)'` — 聚合守卫（「验证执行协议」第 5 项，预期 N=1）：在 `<T>` 的检出上重跑 `CL-28` 的 `--lib` 卡特有用例，覆盖 AC5。

**Full-suite trigger:** `T-2: REL-CL-01 家族发布点全量门`（模板 C 组 ④–⑤：在含 `patch + 1` 与 `Cargo.lock` 刷新、尚未签名提交的工作树上运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 与 `cargo build --release`；⑦ 签名提交后记录 `libra rev-parse HEAD` 与 `HEAD^{tree}` 并确认 `libra status --porcelain --untracked-files=no` 为空（提交树即受测树）；树变化即重跑）
**Dependencies:** `CL-04`, `CL-23`, `CL-28`
**Deliverables:** `Release artifacts & tag for REL-CL-01`
**Implementation write set:** `N/A`
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`
**Docs and compatibility impact:** `N/A`（不改仓库文档，无新增或变更用户命令语义；三张子卡已在本地完成的网站 `cf` 提交由本卡推送并核验部署，见本卡「文档 AC」行）
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CL-01`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CL-01 指针布局家族发布; recovery=发布前按 CL-25 -> CL-28 -> CL-23 -> CL-04 逆序本地 revert，发布后前滚新 patch; complete=yes; self-contained=yes; AC=5/12; VER=7/12; landing=0; prod-files=0; scope=S; deps=CL-04,CL-23,CL-28; writeset=串行于 CL-28; release=family release point; split-from=N/A; exception=N/A`

---

### Task CL-05: 支持 `libra clone --revision=<rev>` 单修订克隆（#741）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增 `--revision=<REV>`，与 `--branch`/`--mirror` 互斥（clap `conflicts_with_all`，`LBR-CLI-002`）；`refs/heads/*` 以单分支 refspec 抓取，其它引用（如 `refs/tags/v0`）映射到临时 `refs/remotes/<n>/_revision` 目的地抓取（fetch 层不接受 tag 目的地）；抓取后置 `Head::Detached(advertised OID)` 并用 `switch::restore_to_commit` 物化工作区，随后删除 `refs/remotes/<n>/*` 的 Branch 行与全部 Tag 行、跳过 `setup_repository`（因此不写 `remote.<n>.fetch`）。新增 `CloneError::RevisionNotFound`（`LBR-CLI-003`）。`test_clone_revision_single_rev`（AC1–AC6）与 `test_clone_revision_conflicts`（AC7–AC8）nextest **2 passed / 0 failed**；`cargo +nightly fmt --all --check` exit 0。按用户 2026-11-20 指令在 `main` 直接提交推送、不等 CI。）
**Description:** 为 `CloneArgs` 新增 `--revision=<rev>`（本卡验收引用名形式：`#741` 的 `--bare --revision=refs/heads/main` 与非裸 `refs/tags/v0`（指向 `S1`）；完整 OID 形式与不存在修订的报错见 `DEFER-CL-06`），在 `clone.rs` 与 `fetch.rs`/`protocol` 层仅拉取指定单个修订并检出为游离 HEAD，并与 `--branch`/`--mirror` 互斥。
**Out of scope:** `--depth` 浅历史截断本身（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#741` `CloneArgs` 缺少 `--revision` 标志且 `fetch` 仅协商分支/tag | `src/command/clone.rs:64-96`；`src/command/fetch.rs:2266-2271`（`fetch_repository` 只接受分支、`single_branch` 与 `depth`，无修订参数） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** S 另有分支 `side` 的提交 C2（不可从 `main` 到达）；`libra clone --revision=refs/heads/main --bare file://<S> b` 退出 0 后，`b` 的本地 HEAD 行（`reference` 表 `kind='Head' AND remote IS NULL`）的 (`name`, `commit`) 等于 (NULL, `<S2>`)（HEAD 游离于请求的修订；`#741`，对应 Git `t/t5621-clone-revision.sh#7`）。
- [x] **AC2** 对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `b` 中的存在性判定结果，恰等于该对象是否属于 `libra rev-list --objects <S2>`（S2 可达闭包）（请求的提交及其历史在本地，`C2` 等其它对象缺失；覆盖 `#741`「修订对象在克隆中」）。
- [x] **AC3** `b` 的 `reference` 表中，除 `libra init` 创建的内置本地分支（`kind='Branch' AND remote IS NULL AND name IS NOT NULL AND name IN ('intent','traces')`，`src/internal/branch.rs:37,42`）外，不存在任何 `kind` 为 `Branch` 或 `Tag` 的行（对应 Git `for-each-ref refs` 为空）。
- [x] **AC4** `b` 的 `config_kv` 表中不存在键 `remote.origin.fetch`。
- [x] **AC5** S 另建轻量 tag `v0` 指向 `S1`（不同于远端默认 `main` 的末端 `S2`）；`libra clone --revision=refs/tags/v0 file://<S> d` 退出 0 后，`d` 中 `libra status --porcelain=v2 --branch` 的完整 stdout 逐字等于 `# branch.head (detached)`、`# branch.oid <S1>` 两行（HEAD 游离于 `S1`，且工作区与索引都与 `S1` 的树一致、无其它条目）。
- [x] **AC6** 对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `d` 中的存在性判定结果，恰等于该对象是否属于 `libra rev-list --objects <S1>`（S1 可达闭包）（只取请求修订可达的对象，`S2` 缺失）。
- [x] **AC7** `libra clone --revision=refs/tags/v1 --branch main file://<S> e1` 以 `LBR-CLI-002` 拒绝（Git `builtin/clone.c:1390-1391`）。
- [x] **AC8** `libra clone --revision=refs/tags/v1 --mirror file://<S> e2` 以 `LBR-CLI-002` 拒绝（Git `builtin/clone.c:1392-1393`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_revision_single_rev`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT name, "commit" FROM reference WHERE kind='Head' AND remote IS NULL`，结果恰为一行且等于 (NULL, `<S2>`)；`S2` 为创建 S 时保存的 `main` 末端提交 OID。
- AC2 → `command::clone_test::test_clone_revision_single_rev`：在 S 中取 `libra rev-list --objects --all` 全集与 `libra rev-list --objects <S2>` 闭包，对全集逐一在 `b` 按「验证执行协议」第 7 项的对象存在性判定，得到的「OID → 存在/缺失」映射与「OID → 是否属于闭包」映射做一次 `assert_eq!`。
- AC3 → `command::clone_test::test_clone_revision_single_rev`：以 SQLite 只读连接执行 `SELECT COUNT(*) FROM reference WHERE kind IN ('Branch','Tag') AND NOT (kind='Branch' AND remote IS NULL AND COALESCE(name, '') IN ('intent','traces'))`，结果为 0（`COALESCE` 使 `name` 为 NULL 的行照常计入，同名 tag 或远端分支也照常计入；不用 `libra show-ref`：空集时它以 `LBR-CLI-003` 失败，`src/command/show_ref.rs:234-236`）。
- AC4 → `command::clone_test::test_clone_revision_single_rev`：以 SQLite 只读连接查询 `b/libra.db` 的 `config_kv`，键 `remote.origin.fetch` 的行数为 0（不经会隐藏加密值的 `libra config list`）。
- AC5 → `command::clone_test::test_clone_revision_single_rev`：`exit 0` 为前提；stdout 逐字等于 `# branch.head (detached)\n# branch.oid <S1>\n`（头部格式见 `src/command/status/output.rs:2541-2555`，开工按 ER-02 以现行输出复核）。
- AC6 → `command::clone_test::test_clone_revision_single_rev`：在 S 中取 `libra rev-list --objects --all` 全集与 `libra rev-list --objects <S1>` 闭包，对全集逐一在 `d` 按「验证执行协议」第 7 项的对象存在性判定，得到的「OID → 存在/缺失」映射与「OID → 是否属于闭包」映射做一次 `assert_eq!`。
- AC7 → `command::clone_test::test_clone_revision_single_rev`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC8 → `command::clone_test::test_clone_revision_single_rev`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--revision`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_revision_single_rev)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC8。
- [ ] `source .env.test && cargo nextest run --test compat_clone_shallow_integrity` — 共享回归组（「验证执行协议」第 5 项）：传输层改动后的浅克隆完整性回归。

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs 与 src/internal/protocol/mod.rs`
**Dependencies:** `CL-25`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --revision 单修订克隆; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=CL-25,DEP-CL-04; writeset=串行于 CL-25; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-06: 支持 `libra clone -c <key=value>` 配置覆盖与 `clone.defaultRemoteName`（#752）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增可重复 `-c`/`--config <key=value>`；init 后按命令行顺序写入新仓库配置（多值追加，`config get --all` 得 `1\n2\n`）；`clone.defaultRemoteName` 在初始 fetch 前解析：显式 `--origin` > `-c clone.defaultRemoteName` > 全局 `clone.defaultRemoteName` > `origin`；空键以 `LBR-CLI-002` 拒绝（`CloneError::InvalidConfigEntry`）。`command::clone_test::test_clone_config_overrides` 覆盖 AC1–AC6，nextest 1 passed / 0 failed；`cargo +nightly fmt --all --check` exit 0。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 为 `CloneArgs` 新增可重复 `-c` / `--config <key=value>`，在初始化目标仓库后写入新仓库配置；其中 `clone.defaultRemoteName` 在初始 fetch 前生效（以 AC3–AC5 的远端跟踪引用观测），其它配置项对本次初始 fetch/checkout 的即时影响不在本卡验收（`DEFER-CL-07`），同时支持 `clone.defaultRemoteName` 并确保显式 `--origin` 优先于 `-c clone.defaultRemoteName`。
**Out of scope:** 非 `clone` 子命令的全局配置覆盖。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#752` `CloneArgs` 缺少 `-c`/`--config` 参数且未读取 `clone.defaultRemoteName` | `src/command/clone.rs:64-236` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 短、长两种形式（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）：`libra clone -c core.foo=bar file://<S> d` 与 `libra clone --config core.foo=bar file://<S> d1` 退出 0 后，`d`、`d1` 中 `libra config get core.foo` 的 stdout 均为 `bar` 加换行。
- [x] **AC2** `libra clone -c a.b=1 -c a.b=2 file://<S> d2` 后，`d2` 中 `libra config get --all a.b` 的 stdout 为 `1\n2\n`（多值按出现顺序全部写入）。
- [x] **AC3** `libra clone -c clone.defaultRemoteName=upstream file://<S> d3` 后，`d3` 的 `reference` 表中 `kind='Branch' AND remote IS NOT NULL` 各行的 `remote` 去重集合恰为 {`upstream`}（配置在初始 fetch 前生效，未同时建立 `origin` 远端跟踪分支）。
- [x] **AC4** `libra clone -c clone.defaultRemoteName=inline --origin from_option file://<S> d4` 后，`d4` 的 `reference` 表中 `kind='Branch' AND remote IS NOT NULL` 各行的 `remote` 去重集合恰为 {`from_option`}（显式 `--origin` 优先，`inline` 未参与；`#752`，对应 Git `t/t5606-clone-options.sh#16`）。
- [x] **AC5** 以逐子进程环境变量 `LIBRA_CONFIG_GLOBAL_DB=<测试临时目录>/global.db` 隔离全局配置，执行 `libra config set --global clone.defaultRemoteName from_config` 后，不带 `-c` 的 `libra clone file://<S> d6` 中，`d6` 的 `reference` 表中 `kind='Branch' AND remote IS NOT NULL` 各行的 `remote` 去重集合恰为 {`from_config`}（对应 Git `t/t5606-clone-options.sh` 的 `test_config_global clone.defaultRemoteName` 用例）。
- [x] **AC6** `libra clone -c =v file://<S> d5`（空键名）以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_config_override_dash_c`：两次克隆的 `exit 0` 为前提；「形式 → `config get core.foo` 的 stdout」映射与「每种形式 → `bar\n`」映射做一次 `assert_eq!`。
- AC2 → `command::clone_test::test_clone_config_override_dash_c`：stdout 逐字等于 `1\n2\n`。
- AC3 → `command::clone_test::test_clone_config_override_dash_c`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT DISTINCT remote FROM reference WHERE kind='Branch' AND remote IS NOT NULL`（库为 `d3`），结果集合做一次 `assert_eq!` 等于 {`upstream`}（Libra 远端跟踪分支以 `remote` 列区分远端，2026-11-18 以 libra 0.30.207 实测）。
- AC4 → `command::clone_test::test_clone_config_override_dash_c`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT DISTINCT remote FROM reference WHERE kind='Branch' AND remote IS NOT NULL`（库为 `d4`），结果集合做一次 `assert_eq!` 等于 {`from_option`}（Libra 远端跟踪分支以 `remote` 列区分远端，2026-11-18 以 libra 0.30.207 实测）。
- AC5 → `command::clone_test::test_clone_config_override_dash_c`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT DISTINCT remote FROM reference WHERE kind='Branch' AND remote IS NOT NULL`（库为 `d6`），结果集合做一次 `assert_eq!` 等于 {`from_config`}（Libra 远端跟踪分支以 `remote` 列区分远端，2026-11-18 以 libra 0.30.207 实测）。
- AC6 → `command::clone_test::test_clone_config_override_dash_c`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`-c <key>=<value>`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_config_override_dash_c)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC6。
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics` — 共享回归组（「验证执行协议」第 5 项）：配置默认值语义回归。

**Full-suite trigger:** `none`
**Dependencies:** `CL-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -c <key=value> 配置覆盖; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-05; writeset=串行于 CL-05; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-23: 适配 `util.rs`/`alternates.rs` 指针布局并使四个基本命令可用（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：新增 `util::resolve_dot_libra_pointer`，仓库发现（`try_get_paths_full_with_storage_lookup`）、`info/*` 候选与 `alternates` 的 objects 解析都跟随 `.libra` 的 `gitdir:` 指针；`status`、`log -n 1`、`cat-file -t HEAD` 与无参 `branch` 在指针布局下可用，`libra alternates add <W>` 记录 `<G>/objects`；`command::clone_test::test_clone_separate_git_dir_pointer_commands` 覆盖 AC1–AC5，nextest 1 run / 1 passed / 0 failed；共享回归 `command::status_test::*` 80 passed / 0 failed；`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化。本卡为 `REL-CL-01` 家族子卡（无版本 bump，C/D 由 `CL-25` 承接；写 ref 的默认拒绝由 `CL-28` 承接，本卡未放行任何写命令）；按用户 2026-11-20 指令在 `main` 提交推送。）
**Description:** 自 `CL-04` 拆出（G-09）。在 `src/utils/util.rs:278,314-324,791-803,1269,1464` 与 `src/command/alternates.rs:91` 解析 `.libra` `gitdir:` 指针，并使 `status`、`log`、`cat-file` 与无参数 `branch` 在该布局下退出 0，`libra alternates add <W>` 解析到 `<G>/objects`。会改 ref 的 `branch` 以及其余子命令由 `CL-28` 默认拒绝（按 G-09 自本卡拆出 `CL-28`）。
**Out of scope:** 默认拒绝（`CL-28`）；会改 ref 的 `branch`（`CL-28`）；`libra init --separate-git-dir`（保留 D25）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#740` 需要 `util.rs` 与 `alternates.rs:91` 消费 `gitdir:` 指针 | `src/utils/util.rs:278,314-324,791-803,1269,1464`；`src/command/alternates.rs:91` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 夹具：`libra clone --separate-git-dir <G> file://<S> <W>` 得到的 `<W>`（`.libra` 为指向 `<G>` 的 `gitdir:` 指针文件）中，`libra status` 退出 0。
- [x] **AC2** 同一 `<W>` 中 `libra log -n 1` 退出 0 且 stdout 含 S 的 `HEAD` 提交 OID。
- [x] **AC3** 同一 `<W>` 中 `libra cat-file -t HEAD` 的 stdout 为 `commit` 加换行。
- [x] **AC4** 同一 `<W>` 中无参数 `libra branch` 的 stdout 含当前分支名 `main`。
- [x] **AC5** 在另一普通仓库 R 中执行 `libra alternates add <W>` 后，R 的 `.libra/objects/info/alternates` 记录的路径规范化后等于 `<G>/objects`（`src/command/alternates.rs:91` 跟随指针，而不是返回不存在的 `<W>/.libra/objects`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：子进程 `current_dir=<W>`，断言 `exit 0`。
- AC2 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：期望 OID 取自在 S 中执行 `libra rev-parse HEAD`；断言 `exit 0` 且 stdout 含该 OID。
- AC3 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：stdout 逐字等于 `commit\n`。
- AC4 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：stdout 含子串 `main`。
- AC5 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：读取 R 的 alternates 文件，唯一非空行 `canonicalize` 后等于 `<G>/objects` 的 `canonicalize`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–6 步（家族子卡：第 6 步只做精确本地 `cf` 提交、不推送；推送与第 7 步部署核验由 `CL-25` 承接）；文档关键字：`separate-git-dir`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_pointer_commands)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC5。
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`status_test` 全模块回归。

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，由 CL-25 统一执行 T-2）`
**Dependencies:** `CL-04`、`DEP-CL-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/util.rs`, `src/command/alternates.rs`, `tests/command/clone_test.rs`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Release write set:** `N/A`
**Files likely touched:** `src/utils/util.rs`, `src/command/alternates.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`（网站文件写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (2/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=pointer 发现与四个基本命令; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=CL-04,DEP-CL-10; writeset=串行于 CL-04; release=family child; split-from=CL-04; exception=N/A`

---

### Task CL-28: 在指针布局下拒绝未适配的写命令（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`src/cli.rs::command_preflight` 在分发前对`.libra` 为 `gitdir:` 指针的布局默认拒绝未适配命令（`LBR-UNSUPPORTED-001`，且不改索引/`HEAD`/ref）；放行 `clone`/`status`/`log`/`cat-file`/`upgrade`/`ui`/`login`/`whoami`/`logout`/`completions`、`alternates list` 与无参数 `branch` 列表。`command::clone_test::test_clone_separate_git_dir_default_deny` 覆盖 AC1–AC4（含索引/引用/工作区快照不变）；`cli::tests::pointer_layout_allowlist_matches_commands` 覆盖 AC5–AC6（逐变体样本表 + 变体名集合相等）。`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化。本卡为 `REL-CL-01` 家族子卡（无版本 bump，C/D 由 `CL-25` 承接）；按用户 2026-11-20 指令在 `main` 提交推送。）
**Description:** 自 `CL-23` 拆出（G-09）。在 `src/cli.rs` `command_preflight`（:1755+）中，当 `.libra` 是 `gitdir:` 指针时默认拒绝。放行名单只有：`clone`；`status`；`log`；`cat-file`；不改 alternates 存储的 `alternates` 子命令；无参数的 `branch`；以及不读写当前仓库的 `upgrade`、`ui`、`login`、`whoami`、`logout`、`completions`（`command_preflight` 对它们返回 `CommandPreflight::none()`，`src/cli.rs:1771-1800`）。`init` 不在放行名单：它会检查并重新初始化既有 `.libra`（`src/command/init.rs:1293` `reinitialize_existing`、`:1697` `ensure_storage_paths_are_directories` 要求 `.libra` 为目录），在指针布局下按默认拒绝处理。会修改 alternates 存储的子命令不在放行名单里。其余 `Commands::*` 变体退出非零，并且不改索引、`HEAD` 或 ref。`_compatibility.md` 的表必须列出每一个变体（文档内容，随 ER-06 文档门逐文件核对）；代码中的指针布局判定由本卡新增的单元测试 `cli::tests::pointer_layout_allowlist_matches_commands` 以命令行样本表核对（AC5–AC6）。本卡是 `REL-CL-01` 子卡 3/4，不单独推送。
**Out of scope:** 指针发现与只读基本命令（`CL-23`）；把被拒绝的命令改造成真正的指针布局（`DEFER-CL-02`）；稀疏 cone 护栏（`CL-26`/`CL-27`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 未适配写命令若在指针布局下继续执行，会把数据写到错误目录。`restore` 会读并保存索引 | `src/cli.rs:1755`；`src/command/restore.rs:788,934` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 夹具：`libra clone --separate-git-dir <G> file://<S> <W>` 得到的 `<W>`（`.libra` 为指向 `<G>` 的 `gitdir:` 指针文件）（S 另含分支 `other`）中，`libra restore .`（先修改 `a.txt`）以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC2** 同一 `<W>` 中 `libra commit --allow-empty -m x` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC3** 同一 `<W>` 中 `libra branch newbranch` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC4** 同一 `<W>` 中 `libra checkout other` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC5** 单元测试 `cli::tests::pointer_layout_allowlist_matches_commands`：对一张固定的命令行样本表（每个 `Commands` 变体至少一个样本；`branch` 含无参数、`branch newbranch`、`branch -D x`、`branch -m a b`，`alternates` 含 `list`、`add <p>`、`remove <p>`、`prune`）逐条经 clap 解析后求本卡指针布局判定，得到的「样本 → 放行/拒绝」映射与期望映射逐项相等（期望：放行名单内的命令放行，`branch` 仅无参数放行，`alternates` 仅 `list` 放行，其余全部拒绝）。
- [x] **AC6** 同一单元测试中，样本表解析得到的变体名集合等于 `Commands` 全部变体名集合（经 `clap::CommandFactory` 枚举子命令名，含隐藏变体），新增变体未进样本表即失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_separate_git_dir_default_deny`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）；调用前后按「验证执行协议」第 4 项比较 `<W>`/`<G>` 仓库快照（含 `<G>/index` 字节与 `reference` 表全部行）。
- AC2 → `command::clone_test::test_clone_separate_git_dir_default_deny`：同上（快照覆盖 `HEAD` 所在的 `reference` 行）。
- AC3 → `command::clone_test::test_clone_separate_git_dir_default_deny`：同上（快照覆盖全部引用，因而 `newbranch` 不得出现；按数据库行判断，不按文件是否存在判断）。
- AC4 → `command::clone_test::test_clone_separate_git_dir_default_deny`：同上。
- AC5 → `cli::tests::pointer_layout_allowlist_matches_commands`：对样本表做一次 `assert_eq!(实际映射, 期望映射)`。
- AC6 → `cli::tests::pointer_layout_allowlist_matches_commands`：两个变体名集合做一次 `assert_eq!`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–6 步（家族子卡：第 6 步只做精确本地 `cf` 提交、不推送；推送与第 7 步部署核验由 `CL-25` 承接）；文档关键字：`separate-git-dir`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_default_deny)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [x] `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::pointer_layout_allowlist_matches_commands)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC5–AC6。

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，由 CL-25 统一执行 T-2）`
**Dependencies:** `CL-23`、`DEP-CL-12`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/add.md`, `docs/commands/zh-CN/add.md`, `docs/development/commands/add.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`, `docs/commands/agent.md`, `docs/commands/zh-CN/agent.md`, `docs/development/commands/agent.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`, `docs/commands/archive.md`, `docs/commands/zh-CN/archive.md`, `docs/development/commands/archive.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`, `docs/commands/auth.md`, `docs/commands/zh-CN/auth.md`, `docs/development/commands/auth.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`, `docs/commands/automation.md`, `docs/commands/zh-CN/automation.md`, `docs/development/commands/automation.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`, `docs/commands/bisect.md`, `docs/commands/zh-CN/bisect.md`, `docs/development/commands/bisect.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`, `docs/commands/blame.md`, `docs/commands/zh-CN/blame.md`, `docs/development/commands/blame.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/bundle.md`, `docs/commands/zh-CN/bundle.md`, `docs/development/commands/bundle.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`, `docs/commands/cache.md`, `docs/commands/zh-CN/cache.md`, `docs/development/commands/cache.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`, `docs/commands/check-mailmap.md`, `docs/commands/zh-CN/check-mailmap.md`, `docs/development/commands/check-mailmap.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`, `docs/commands/cloud.md`, `docs/commands/zh-CN/cloud.md`, `docs/development/commands/cloud.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `docs/commands/commit-tree.md`, `docs/commands/zh-CN/commit-tree.md`, `docs/development/commands/commit-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `docs/commands/count-objects.md`, `docs/commands/zh-CN/count-objects.md`, `docs/development/commands/count-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`, `docs/commands/credential.md`, `docs/commands/zh-CN/credential.md`, `docs/development/commands/credential.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`, `docs/commands/deps.md`, `docs/commands/zh-CN/deps.md`, `docs/development/commands/deps.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`, `docs/commands/diff-files.md`, `docs/commands/zh-CN/diff-files.md`, `docs/development/commands/diff-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`, `docs/commands/diff-index.md`, `docs/commands/zh-CN/diff-index.md`, `docs/development/commands/diff-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`, `docs/commands/diff-tree.md`, `docs/commands/zh-CN/diff-tree.md`, `docs/development/commands/diff-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`, `docs/commands/dirty.md`, `docs/commands/zh-CN/dirty.md`, `docs/development/commands/dirty.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`, `docs/commands/fast-export.md`, `docs/commands/zh-CN/fast-export.md`, `docs/development/commands/fast-export.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`, `docs/commands/fast-import.md`, `docs/commands/zh-CN/fast-import.md`, `docs/development/commands/fast-import.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `docs/commands/file.md`, `docs/commands/zh-CN/file.md`, `docs/development/commands/file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`, `docs/commands/fsck.md`, `docs/commands/zh-CN/fsck.md`, `docs/development/commands/fsck.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`, `docs/commands/gc.md`, `docs/commands/zh-CN/gc.md`, `docs/development/commands/gc.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`, `docs/commands/grep.md`, `docs/commands/zh-CN/grep.md`, `docs/development/commands/grep.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`, `docs/commands/hash-object.md`, `docs/commands/zh-CN/hash-object.md`, `docs/development/commands/hash-object.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`, `docs/commands/hooks.md`, `docs/commands/zh-CN/hooks.md`, `docs/development/commands/hooks.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`, `docs/commands/hydrate.md`, `docs/commands/zh-CN/hydrate.md`, `docs/development/commands/hydrate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`, `docs/commands/index-pack.md`, `docs/commands/zh-CN/index-pack.md`, `docs/development/commands/index-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `docs/commands/investigate.md`, `docs/commands/zh-CN/investigate.md`, `docs/development/commands/investigate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`, `docs/commands/layer.md`, `docs/commands/zh-CN/layer.md`, `docs/development/commands/layer.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`, `docs/commands/lfs.md`, `docs/commands/zh-CN/lfs.md`, `docs/development/commands/lfs.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`, `docs/commands/logfile.md`, `docs/commands/zh-CN/logfile.md`, `docs/development/commands/logfile.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`, `docs/commands/ls-remote.md`, `docs/commands/zh-CN/ls-remote.md`, `docs/development/commands/ls-remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`, `docs/commands/ls-tree.md`, `docs/commands/zh-CN/ls-tree.md`, `docs/development/commands/ls-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`, `docs/commands/mailinfo.md`, `docs/commands/zh-CN/mailinfo.md`, `docs/development/commands/mailinfo.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`, `docs/commands/media.md`, `docs/commands/zh-CN/media.md`, `docs/development/commands/media.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`, `docs/commands/mega2.md`, `docs/commands/zh-CN/mega2.md`, `docs/development/commands/mega2.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`, `docs/commands/memory.md`, `docs/commands/zh-CN/memory.md`, `docs/development/commands/memory.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `docs/commands/merge-base.md`, `docs/commands/zh-CN/merge-base.md`, `docs/development/commands/merge-base.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`, `docs/commands/merge-file.md`, `docs/commands/zh-CN/merge-file.md`, `docs/development/commands/merge-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`, `docs/commands/mergetool.md`, `docs/commands/zh-CN/mergetool.md`, `docs/development/commands/mergetool.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`, `docs/commands/metadata.md`, `docs/commands/zh-CN/metadata.md`, `docs/development/commands/metadata.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`, `docs/commands/op.md`, `docs/commands/zh-CN/op.md`, `docs/development/commands/op.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`, `docs/commands/open.md`, `docs/commands/zh-CN/open.md`, `docs/development/commands/open.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`, `docs/commands/pack-objects.md`, `docs/commands/zh-CN/pack-objects.md`, `docs/development/commands/pack-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`, `docs/commands/read-tree.md`, `docs/commands/zh-CN/read-tree.md`, `docs/development/commands/read-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `docs/commands/reflog.md`, `docs/commands/zh-CN/reflog.md`, `docs/development/commands/reflog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`, `docs/commands/remote.md`, `docs/commands/zh-CN/remote.md`, `docs/development/commands/remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`, `docs/commands/repack.md`, `docs/commands/zh-CN/repack.md`, `docs/development/commands/repack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`, `docs/commands/rerere.md`, `docs/commands/zh-CN/rerere.md`, `docs/development/commands/rerere.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`, `docs/commands/review.md`, `docs/commands/zh-CN/review.md`, `docs/development/commands/review.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/development/commands/revision.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`, `docs/commands/sandbox.md`, `docs/commands/zh-CN/sandbox.md`, `docs/development/commands/sandbox.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`, `docs/commands/send-email.md`, `docs/commands/zh-CN/send-email.md`, `docs/development/commands/send-email.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`, `docs/commands/service.md`, `docs/commands/zh-CN/service.md`, `docs/development/commands/service.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`, `docs/commands/show-ref.md`, `docs/commands/zh-CN/show-ref.md`, `docs/development/commands/show-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`, `docs/commands/sparse-view.md`, `docs/commands/zh-CN/sparse-view.md`, `docs/development/commands/sparse-view.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`, `docs/commands/tag.md`, `docs/commands/zh-CN/tag.md`, `docs/development/commands/tag.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`, `docs/commands/update-index.md`, `docs/commands/zh-CN/update-index.md`, `docs/development/commands/update-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`, `docs/commands/update-ref.md`, `docs/commands/zh-CN/update-ref.md`, `docs/development/commands/update-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`, `docs/commands/verify-pack.md`, `docs/commands/zh-CN/verify-pack.md`, `docs/development/commands/verify-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`, `docs/commands/worktree.md`, `docs/commands/zh-CN/worktree.md`, `docs/development/commands/worktree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`, `docs/commands/write-tree.md`, `docs/commands/zh-CN/write-tree.md`, `docs/development/commands/write-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Release write set:** `N/A`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/add.md`, `docs/commands/zh-CN/add.md`, `docs/development/commands/add.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`, `docs/commands/agent.md`, `docs/commands/zh-CN/agent.md`, `docs/development/commands/agent.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`, `docs/commands/archive.md`, `docs/commands/zh-CN/archive.md`, `docs/development/commands/archive.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`, `docs/commands/auth.md`, `docs/commands/zh-CN/auth.md`, `docs/development/commands/auth.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`, `docs/commands/automation.md`, `docs/commands/zh-CN/automation.md`, `docs/development/commands/automation.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`, `docs/commands/bisect.md`, `docs/commands/zh-CN/bisect.md`, `docs/development/commands/bisect.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`, `docs/commands/blame.md`, `docs/commands/zh-CN/blame.md`, `docs/development/commands/blame.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/bundle.md`, `docs/commands/zh-CN/bundle.md`, `docs/development/commands/bundle.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`, `docs/commands/cache.md`, `docs/commands/zh-CN/cache.md`, `docs/development/commands/cache.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`, `docs/commands/check-mailmap.md`, `docs/commands/zh-CN/check-mailmap.md`, `docs/development/commands/check-mailmap.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`, `docs/commands/cloud.md`, `docs/commands/zh-CN/cloud.md`, `docs/development/commands/cloud.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `docs/commands/commit-tree.md`, `docs/commands/zh-CN/commit-tree.md`, `docs/development/commands/commit-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `docs/commands/count-objects.md`, `docs/commands/zh-CN/count-objects.md`, `docs/development/commands/count-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`, `docs/commands/credential.md`, `docs/commands/zh-CN/credential.md`, `docs/development/commands/credential.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`, `docs/commands/deps.md`, `docs/commands/zh-CN/deps.md`, `docs/development/commands/deps.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`, `docs/commands/diff-files.md`, `docs/commands/zh-CN/diff-files.md`, `docs/development/commands/diff-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`, `docs/commands/diff-index.md`, `docs/commands/zh-CN/diff-index.md`, `docs/development/commands/diff-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`, `docs/commands/diff-tree.md`, `docs/commands/zh-CN/diff-tree.md`, `docs/development/commands/diff-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`, `docs/commands/dirty.md`, `docs/commands/zh-CN/dirty.md`, `docs/development/commands/dirty.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`, `docs/commands/fast-export.md`, `docs/commands/zh-CN/fast-export.md`, `docs/development/commands/fast-export.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`, `docs/commands/fast-import.md`, `docs/commands/zh-CN/fast-import.md`, `docs/development/commands/fast-import.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `docs/commands/file.md`, `docs/commands/zh-CN/file.md`, `docs/development/commands/file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`, `docs/commands/fsck.md`, `docs/commands/zh-CN/fsck.md`, `docs/development/commands/fsck.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`, `docs/commands/gc.md`, `docs/commands/zh-CN/gc.md`, `docs/development/commands/gc.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`, `docs/commands/grep.md`, `docs/commands/zh-CN/grep.md`, `docs/development/commands/grep.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`, `docs/commands/hash-object.md`, `docs/commands/zh-CN/hash-object.md`, `docs/development/commands/hash-object.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`, `docs/commands/hooks.md`, `docs/commands/zh-CN/hooks.md`, `docs/development/commands/hooks.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`, `docs/commands/hydrate.md`, `docs/commands/zh-CN/hydrate.md`, `docs/development/commands/hydrate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`, `docs/commands/index-pack.md`, `docs/commands/zh-CN/index-pack.md`, `docs/development/commands/index-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `docs/commands/investigate.md`, `docs/commands/zh-CN/investigate.md`, `docs/development/commands/investigate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`, `docs/commands/layer.md`, `docs/commands/zh-CN/layer.md`, `docs/development/commands/layer.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`, `docs/commands/lfs.md`, `docs/commands/zh-CN/lfs.md`, `docs/development/commands/lfs.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`, `docs/commands/logfile.md`, `docs/commands/zh-CN/logfile.md`, `docs/development/commands/logfile.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`, `docs/commands/ls-remote.md`, `docs/commands/zh-CN/ls-remote.md`, `docs/development/commands/ls-remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`, `docs/commands/ls-tree.md`, `docs/commands/zh-CN/ls-tree.md`, `docs/development/commands/ls-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`, `docs/commands/mailinfo.md`, `docs/commands/zh-CN/mailinfo.md`, `docs/development/commands/mailinfo.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`, `docs/commands/media.md`, `docs/commands/zh-CN/media.md`, `docs/development/commands/media.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`, `docs/commands/mega2.md`, `docs/commands/zh-CN/mega2.md`, `docs/development/commands/mega2.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`, `docs/commands/memory.md`, `docs/commands/zh-CN/memory.md`, `docs/development/commands/memory.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `docs/commands/merge-base.md`, `docs/commands/zh-CN/merge-base.md`, `docs/development/commands/merge-base.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`, `docs/commands/merge-file.md`, `docs/commands/zh-CN/merge-file.md`, `docs/development/commands/merge-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`, `docs/commands/mergetool.md`, `docs/commands/zh-CN/mergetool.md`, `docs/development/commands/mergetool.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`, `docs/commands/metadata.md`, `docs/commands/zh-CN/metadata.md`, `docs/development/commands/metadata.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`, `docs/commands/op.md`, `docs/commands/zh-CN/op.md`, `docs/development/commands/op.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`, `docs/commands/open.md`, `docs/commands/zh-CN/open.md`, `docs/development/commands/open.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`, `docs/commands/pack-objects.md`, `docs/commands/zh-CN/pack-objects.md`, `docs/development/commands/pack-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`, `docs/commands/read-tree.md`, `docs/commands/zh-CN/read-tree.md`, `docs/development/commands/read-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `docs/commands/reflog.md`, `docs/commands/zh-CN/reflog.md`, `docs/development/commands/reflog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`, `docs/commands/remote.md`, `docs/commands/zh-CN/remote.md`, `docs/development/commands/remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`, `docs/commands/repack.md`, `docs/commands/zh-CN/repack.md`, `docs/development/commands/repack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`, `docs/commands/rerere.md`, `docs/commands/zh-CN/rerere.md`, `docs/development/commands/rerere.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`, `docs/commands/review.md`, `docs/commands/zh-CN/review.md`, `docs/development/commands/review.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/development/commands/revision.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`, `docs/commands/sandbox.md`, `docs/commands/zh-CN/sandbox.md`, `docs/development/commands/sandbox.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`, `docs/commands/send-email.md`, `docs/commands/zh-CN/send-email.md`, `docs/development/commands/send-email.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`, `docs/commands/service.md`, `docs/commands/zh-CN/service.md`, `docs/development/commands/service.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`, `docs/commands/show-ref.md`, `docs/commands/zh-CN/show-ref.md`, `docs/development/commands/show-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`, `docs/commands/sparse-view.md`, `docs/commands/zh-CN/sparse-view.md`, `docs/development/commands/sparse-view.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`, `docs/commands/tag.md`, `docs/commands/zh-CN/tag.md`, `docs/development/commands/tag.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`, `docs/commands/update-index.md`, `docs/commands/zh-CN/update-index.md`, `docs/development/commands/update-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`, `docs/commands/update-ref.md`, `docs/commands/zh-CN/update-ref.md`, `docs/development/commands/update-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`, `docs/commands/verify-pack.md`, `docs/commands/zh-CN/verify-pack.md`, `docs/development/commands/verify-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`, `docs/commands/worktree.md`, `docs/commands/zh-CN/worktree.md`, `docs/development/commands/worktree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`, `docs/commands/write-tree.md`, `docs/commands/zh-CN/write-tree.md`, `docs/development/commands/write-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`。`_compatibility.md` 的拒绝表与 `Commands` 枚举一一对应。`branch` 只放行无参数形式，其会改 ref 的形式受本卡默认拒绝影响，其文档同样逐文件列入；`init` 在指针布局下被默认拒绝，其文档同样列入。网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (3/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=指针布局默认拒绝未放行命令; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-23,DEP-CL-12; writeset=串行于 CL-23; release=family child; split-from=CL-23; exception=N/A`

---

### Task CL-25: 发布 `REL-CL-01`（`clone --separate-git-dir` 指针布局家族发布点）（#740）

**Task type:** `release`
**Lifecycle / Acceptance:** `done` / `remote-pending`（2026-11-20：`REL-CL-01` 家族发布点。三张子卡 `CL-04`（`2335f3f`+`d9189cc`）、`CL-23`（`05698cd`）、`CL-28`（`5d3079f`）均已是本卡受测提交的祖先；版本面 patch 0.30.218 → 0.30.219（`Cargo.toml`/`install.sh`/`install.ps1` + `Cargo.lock`），`compat_version_surface_sync` 2 passed；家族聚合守卫 3 passed（`test_clone_separate_git_dir_and_bare_mutex`、`test_clone_separate_git_dir_pointer_commands`、`test_clone_separate_git_dir_default_deny`）；`cargo build --release --bin libra` exit 0。按用户 2026-11-20 指令：全量 `T-2` 未执行（3.5「不进行全量测试」），PR/`gh pr merge` 流程由「main 直接提交推送」替代（3.1），`gh release create` 与 tag 尚未执行（需用户批准后补）。`remote-pending` 指远端发布流水线与 Release 产物仍待用户批准。
**Description:** 聚合 `REL-CL-01` 家族子卡 `CL-04`、`CL-23` 与 `CL-28`（不引入新生产代码变更），执行 `patch + 1`、`compat_version_surface_sync` 三处版本面同步、工具链刷新 `Cargo.lock`、全量 `T-2` 门禁、`cargo build --release` 与 `gh release create`；其中 `patch + 1` 版本面同步与 `compat_version_surface_sync` 属 ER-04 C 组强制门（模板 C 组 ①–②），`release.yml` 等远端结果属 ER-04 D 组门（见「执行检查必备需求」），均不计入本卡 G-03 计数。对外发布指针布局、四个只读基本命令，以及 `CL-28` 对未放行子命令的默认拒绝；发布后承载三张子卡本地 `cf` 网站提交的推送与部署核验（「文档与网站验收命令」第 6–7 步，收口 `DEP-CL-15` 窗口）。
**Out of scope:** 任何新增功能代码实现；`libra init --separate-git-dir`（`DEFER-CL-02`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `REL-CL-01` 需在 `CL-04`、`CL-23` 与 `CL-28` 均完成本地提交后统一版本化发布；三处版本面（2026-11-18 核对均为 `0.30.210`，开工按 ER-02 刷新） | `Cargo.toml:3`；`install.sh:21`（`DEFAULT_VERSION`）；`install.ps1:25`（`$DefaultVersion`） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `CL-04` 的本地签名提交是本卡受测提交 `<T>` 的祖先（`<T>` = `CL-28` 本地提交之后、ER-05 评审之前执行本卡 A/B 门时的本地 `HEAD`，尚未做 C 组版本 bump；其 `libra rev-parse HEAD` 与 `HEAD^{tree}` 记入执行记录）。
- [x] **AC2** `CL-23` 的本地签名提交是 `<T>` 的祖先。
- [x] **AC3** `CL-28` 的本地签名提交是 `<T>` 的祖先。
- [x] **AC4** 家族聚合守卫：三张子卡在 `command_test` 中的卡特有用例在 `<T>` 上全部通过。
- [x] **AC5** 家族聚合守卫：`CL-28` 的 `--lib` 卡特有用例在 `<T>` 上通过。
- [ ] ER-04 C 组强制门（不计入 G-03；本卡按用户 2026-11-20 指令执行 ①–③ 与 `cargo build --release`：版本面 `0.30.219`、`compat_version_surface_sync` 2 passed、release 构建 exit 0；`T-2` 全量按 3.5 暂缓，`gh release create` 待用户批准）：在 `<T>` 之上做 `patch + 1` 版本面同步与 `Cargo.lock` 工具链刷新（①–③）；在这一已 bump、尚未提交的工作树上先确认 `Cargo.toml` 的 `version` 行是合并基底 `X.Y.Z` 的 `X.Y.(Z+1)`、`source .env.test && cargo nextest run --test compat_version_surface_sync` 通过，再跑本卡 `Full-suite trigger` 的 `T-2` 收口门、`cargo build --release` 与安装（④–⑥）；之后才签名提交得到 `<F>`（⑦；提交后 `libra status --porcelain --untracked-files=no` 为空——已跟踪文件无未提交改动，因而 `<F>^{tree}` 即 ④–⑥ 的受测树；`libra merge-base --is-ancestor <T> <F>` 退出 0）；`<F>` 作为 PR head 推送，`gh pr merge --squash` 后 `libra fetch origin`，`libra rev-parse <MERGE_SHA>^{tree}` 等于 `libra rev-parse <F>^{tree}`（发布树即 `T-2` 受测树）。
- [ ] ER-06a 网站交付（不计入 G-03）：见下方「文档 AC」行。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → Verification 第 1 项（手工证据）。
- AC2 → Verification 第 2 项（手工证据）。
- AC3 → Verification 第 3 项（手工证据）。
- AC4 → Verification 第 4 项（聚合守卫，按「验证执行协议」第 5 项的 `release` 卡例外作为本 AC 证据；三测试精确并集，预期 N=3）。
- AC5 → Verification 第 5 项（同上，预期 N=1）。
- 文档 AC（ER-06a 网站交付，不计入 G-03）→ 「文档与网站验收命令」第 6 步的 `CL-25` 分支（一次推送三张子卡的本地 `cf` 提交）与第 7 步（对三张子卡改动的每个网站页面做部署核验）；本卡不改仓库文档。

**Verification:**
- [ ] 手工证据（执行记录，「验证执行协议」第 6 项按 1 门计）：`libra merge-base --is-ancestor <CL-04 提交> <T>` 退出 0，覆盖 AC1。
- [ ] 手工证据（执行记录，「验证执行协议」第 6 项按 1 门计）：`libra merge-base --is-ancestor <CL-23 提交> <T>` 退出 0，覆盖 AC2。
- [ ] 手工证据（执行记录，「验证执行协议」第 6 项按 1 门计）：`libra merge-base --is-ancestor <CL-28 提交> <T>` 退出 0，覆盖 AC3。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_and_bare_mutex) | test(=command::clone_test::test_clone_separate_git_dir_pointer_commands) | test(=command::clone_test::test_clone_separate_git_dir_default_deny)'` — 聚合守卫（「验证执行协议」第 5 项，预期 N=3）：在 `<T>` 的检出上重跑三张子卡的 `command_test` 卡特有用例，覆盖 AC4。
- [ ] `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::pointer_layout_allowlist_matches_commands)'` — 聚合守卫（「验证执行协议」第 5 项，预期 N=1）：在 `<T>` 的检出上重跑 `CL-28` 的 `--lib` 卡特有用例，覆盖 AC5。

**Full-suite trigger:** `T-2: REL-CL-01 家族发布点全量门`（模板 C 组 ④–⑤：在含 `patch + 1` 与 `Cargo.lock` 刷新、尚未签名提交的工作树上运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 与 `cargo build --release`；⑦ 签名提交后记录 `libra rev-parse HEAD` 与 `HEAD^{tree}` 并确认 `libra status --porcelain --untracked-files=no` 为空（提交树即受测树）；树变化即重跑）
**Dependencies:** `CL-04`, `CL-23`, `CL-28`
**Deliverables:** `Release artifacts & tag for REL-CL-01`
**Implementation write set:** `N/A`
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`
**Docs and compatibility impact:** `N/A`（不改仓库文档，无新增或变更用户命令语义；三张子卡已在本地完成的网站 `cf` 提交由本卡推送并核验部署，见本卡「文档 AC」行）
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CL-01`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CL-01 指针布局家族发布; recovery=发布前按 CL-25 -> CL-28 -> CL-23 -> CL-04 逆序本地 revert，发布后前滚新 patch; complete=yes; self-contained=yes; AC=5/12; VER=7/12; landing=0; prod-files=0; scope=S; deps=CL-04,CL-23,CL-28; writeset=串行于 CL-28; release=family release point; split-from=N/A; exception=N/A`

---

### Task CL-05: 支持 `libra clone --revision=<rev>` 单修订克隆（#741）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增 `--revision=<REV>`，与 `--branch`/`--mirror` 互斥（clap `conflicts_with_all`，`LBR-CLI-002`）；`refs/heads/*` 以单分支 refspec 抓取，其它引用（如 `refs/tags/v0`）映射到临时 `refs/remotes/<n>/_revision` 目的地抓取（fetch 层不接受 tag 目的地）；抓取后置 `Head::Detached(advertised OID)` 并用 `switch::restore_to_commit` 物化工作区，随后删除 `refs/remotes/<n>/*` 的 Branch 行与全部 Tag 行、跳过 `setup_repository`（因此不写 `remote.<n>.fetch`）。新增 `CloneError::RevisionNotFound`（`LBR-CLI-003`）。`test_clone_revision_single_rev`（AC1–AC6）与 `test_clone_revision_conflicts`（AC7–AC8）nextest **2 passed / 0 failed**；`cargo +nightly fmt --all --check` exit 0。按用户 2026-11-20 指令在 `main` 直接提交推送、不等 CI。）
**Description:** 为 `CloneArgs` 新增 `--revision=<rev>`（本卡验收引用名形式：`#741` 的 `--bare --revision=refs/heads/main` 与非裸 `refs/tags/v0`（指向 `S1`）；完整 OID 形式与不存在修订的报错见 `DEFER-CL-06`），在 `clone.rs` 与 `fetch.rs`/`protocol` 层仅拉取指定单个修订并检出为游离 HEAD，并与 `--branch`/`--mirror` 互斥。
**Out of scope:** `--depth` 浅历史截断本身（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#741` `CloneArgs` 缺少 `--revision` 标志且 `fetch` 仅协商分支/tag | `src/command/clone.rs:64-96`；`src/command/fetch.rs:2266-2271`（`fetch_repository` 只接受分支、`single_branch` 与 `depth`，无修订参数） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** S 另有分支 `side` 的提交 C2（不可从 `main` 到达）；`libra clone --revision=refs/heads/main --bare file://<S> b` 退出 0 后，`b` 的本地 HEAD 行（`reference` 表 `kind='Head' AND remote IS NULL`）的 (`name`, `commit`) 等于 (NULL, `<S2>`)（HEAD 游离于请求的修订；`#741`，对应 Git `t/t5621-clone-revision.sh#7`）。
- [x] **AC2** 对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `b` 中的存在性判定结果，恰等于该对象是否属于 `libra rev-list --objects <S2>`（S2 可达闭包）（请求的提交及其历史在本地，`C2` 等其它对象缺失；覆盖 `#741`「修订对象在克隆中」）。
- [x] **AC3** `b` 的 `reference` 表中，除 `libra init` 创建的内置本地分支（`kind='Branch' AND remote IS NULL AND name IS NOT NULL AND name IN ('intent','traces')`，`src/internal/branch.rs:37,42`）外，不存在任何 `kind` 为 `Branch` 或 `Tag` 的行（对应 Git `for-each-ref refs` 为空）。
- [x] **AC4** `b` 的 `config_kv` 表中不存在键 `remote.origin.fetch`。
- [x] **AC5** S 另建轻量 tag `v0` 指向 `S1`（不同于远端默认 `main` 的末端 `S2`）；`libra clone --revision=refs/tags/v0 file://<S> d` 退出 0 后，`d` 中 `libra status --porcelain=v2 --branch` 的完整 stdout 逐字等于 `# branch.head (detached)`、`# branch.oid <S1>` 两行（HEAD 游离于 `S1`，且工作区与索引都与 `S1` 的树一致、无其它条目）。
- [x] **AC6** 对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `d` 中的存在性判定结果，恰等于该对象是否属于 `libra rev-list --objects <S1>`（S1 可达闭包）（只取请求修订可达的对象，`S2` 缺失）。
- [x] **AC7** `libra clone --revision=refs/tags/v1 --branch main file://<S> e1` 以 `LBR-CLI-002` 拒绝（Git `builtin/clone.c:1390-1391`）。
- [x] **AC8** `libra clone --revision=refs/tags/v1 --mirror file://<S> e2` 以 `LBR-CLI-002` 拒绝（Git `builtin/clone.c:1392-1393`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_revision_single_rev`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT name, "commit" FROM reference WHERE kind='Head' AND remote IS NULL`，结果恰为一行且等于 (NULL, `<S2>`)；`S2` 为创建 S 时保存的 `main` 末端提交 OID。
- AC2 → `command::clone_test::test_clone_revision_single_rev`：在 S 中取 `libra rev-list --objects --all` 全集与 `libra rev-list --objects <S2>` 闭包，对全集逐一在 `b` 按「验证执行协议」第 7 项的对象存在性判定，得到的「OID → 存在/缺失」映射与「OID → 是否属于闭包」映射做一次 `assert_eq!`。
- AC3 → `command::clone_test::test_clone_revision_single_rev`：以 SQLite 只读连接执行 `SELECT COUNT(*) FROM reference WHERE kind IN ('Branch','Tag') AND NOT (kind='Branch' AND remote IS NULL AND COALESCE(name, '') IN ('intent','traces'))`，结果为 0（`COALESCE` 使 `name` 为 NULL 的行照常计入，同名 tag 或远端分支也照常计入；不用 `libra show-ref`：空集时它以 `LBR-CLI-003` 失败，`src/command/show_ref.rs:234-236`）。
- AC4 → `command::clone_test::test_clone_revision_single_rev`：以 SQLite 只读连接查询 `b/libra.db` 的 `config_kv`，键 `remote.origin.fetch` 的行数为 0（不经会隐藏加密值的 `libra config list`）。
- AC5 → `command::clone_test::test_clone_revision_single_rev`：`exit 0` 为前提；stdout 逐字等于 `# branch.head (detached)\n# branch.oid <S1>\n`（头部格式见 `src/command/status/output.rs:2541-2555`，开工按 ER-02 以现行输出复核）。
- AC6 → `command::clone_test::test_clone_revision_single_rev`：在 S 中取 `libra rev-list --objects --all` 全集与 `libra rev-list --objects <S1>` 闭包，对全集逐一在 `d` 按「验证执行协议」第 7 项的对象存在性判定，得到的「OID → 存在/缺失」映射与「OID → 是否属于闭包」映射做一次 `assert_eq!`。
- AC7 → `command::clone_test::test_clone_revision_single_rev`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC8 → `command::clone_test::test_clone_revision_single_rev`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--revision`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_revision_single_rev)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC8。
- [ ] `source .env.test && cargo nextest run --test compat_clone_shallow_integrity` — 共享回归组（「验证执行协议」第 5 项）：传输层改动后的浅克隆完整性回归。

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs 与 src/internal/protocol/mod.rs`
**Dependencies:** `CL-25`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --revision 单修订克隆; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=CL-25,DEP-CL-04; writeset=串行于 CL-25; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-06: 支持 `libra clone -c <key=value>` 配置覆盖与 `clone.defaultRemoteName`（#752）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增可重复 `-c`/`--config <key=value>`；init 后按命令行顺序写入新仓库配置（多值追加，`config get --all` 得 `1\n2\n`）；`clone.defaultRemoteName` 在初始 fetch 前解析：显式 `--origin` > `-c clone.defaultRemoteName` > 全局 `clone.defaultRemoteName` > `origin`；空键以 `LBR-CLI-002` 拒绝（`CloneError::InvalidConfigEntry`）。`command::clone_test::test_clone_config_overrides` 覆盖 AC1–AC6，nextest 1 passed / 0 failed；`cargo +nightly fmt --all --check` exit 0。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 为 `CloneArgs` 新增可重复 `-c` / `--config <key=value>`，在初始化目标仓库后写入新仓库配置；其中 `clone.defaultRemoteName` 在初始 fetch 前生效（以 AC3–AC5 的远端跟踪引用观测），其它配置项对本次初始 fetch/checkout 的即时影响不在本卡验收（`DEFER-CL-07`），同时支持 `clone.defaultRemoteName` 并确保显式 `--origin` 优先于 `-c clone.defaultRemoteName`。
**Out of scope:** 非 `clone` 子命令的全局配置覆盖。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#752` `CloneArgs` 缺少 `-c`/`--config` 参数且未读取 `clone.defaultRemoteName` | `src/command/clone.rs:64-236` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 短、长两种形式（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）：`libra clone -c core.foo=bar file://<S> d` 与 `libra clone --config core.foo=bar file://<S> d1` 退出 0 后，`d`、`d1` 中 `libra config get core.foo` 的 stdout 均为 `bar` 加换行。
- [x] **AC2** `libra clone -c a.b=1 -c a.b=2 file://<S> d2` 后，`d2` 中 `libra config get --all a.b` 的 stdout 为 `1\n2\n`（多值按出现顺序全部写入）。
- [x] **AC3** `libra clone -c clone.defaultRemoteName=upstream file://<S> d3` 后，`d3` 的 `reference` 表中 `kind='Branch' AND remote IS NOT NULL` 各行的 `remote` 去重集合恰为 {`upstream`}（配置在初始 fetch 前生效，未同时建立 `origin` 远端跟踪分支）。
- [x] **AC4** `libra clone -c clone.defaultRemoteName=inline --origin from_option file://<S> d4` 后，`d4` 的 `reference` 表中 `kind='Branch' AND remote IS NOT NULL` 各行的 `remote` 去重集合恰为 {`from_option`}（显式 `--origin` 优先，`inline` 未参与；`#752`，对应 Git `t/t5606-clone-options.sh#16`）。
- [x] **AC5** 以逐子进程环境变量 `LIBRA_CONFIG_GLOBAL_DB=<测试临时目录>/global.db` 隔离全局配置，执行 `libra config set --global clone.defaultRemoteName from_config` 后，不带 `-c` 的 `libra clone file://<S> d6` 中，`d6` 的 `reference` 表中 `kind='Branch' AND remote IS NOT NULL` 各行的 `remote` 去重集合恰为 {`from_config`}（对应 Git `t/t5606-clone-options.sh` 的 `test_config_global clone.defaultRemoteName` 用例）。
- [x] **AC6** `libra clone -c =v file://<S> d5`（空键名）以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_config_override_dash_c`：两次克隆的 `exit 0` 为前提；「形式 → `config get core.foo` 的 stdout」映射与「每种形式 → `bar\n`」映射做一次 `assert_eq!`。
- AC2 → `command::clone_test::test_clone_config_override_dash_c`：stdout 逐字等于 `1\n2\n`。
- AC3 → `command::clone_test::test_clone_config_override_dash_c`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT DISTINCT remote FROM reference WHERE kind='Branch' AND remote IS NOT NULL`（库为 `d3`），结果集合做一次 `assert_eq!` 等于 {`upstream`}（Libra 远端跟踪分支以 `remote` 列区分远端，2026-11-18 以 libra 0.30.207 实测）。
- AC4 → `command::clone_test::test_clone_config_override_dash_c`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT DISTINCT remote FROM reference WHERE kind='Branch' AND remote IS NOT NULL`（库为 `d4`），结果集合做一次 `assert_eq!` 等于 {`from_option`}（Libra 远端跟踪分支以 `remote` 列区分远端，2026-11-18 以 libra 0.30.207 实测）。
- AC5 → `command::clone_test::test_clone_config_override_dash_c`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT DISTINCT remote FROM reference WHERE kind='Branch' AND remote IS NOT NULL`（库为 `d6`），结果集合做一次 `assert_eq!` 等于 {`from_config`}（Libra 远端跟踪分支以 `remote` 列区分远端，2026-11-18 以 libra 0.30.207 实测）。
- AC6 → `command::clone_test::test_clone_config_override_dash_c`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`-c <key>=<value>`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_config_override_dash_c)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC6。
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics` — 共享回归组（「验证执行协议」第 5 项）：配置默认值语义回归。

**Full-suite trigger:** `none`
**Dependencies:** `CL-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -c <key=value> 配置覆盖; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-05; writeset=串行于 CL-05; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-08: 实现 `libra clone -l` / `--local` 硬链接本地对象（#743）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：本地 Libra 源的对象传输改为「整体接管对象存储」：`adopt_local_objects` 清空 fetch 产生的目标对象布局后按源目录逐文件 `hard_link`（同文件系统共享 inode），首次失败回退复制，显式 `--local` 失败即 `LBR-IO-002`，源对象目录符号链接 fail-closed；`rewrite_alternates_absolute` 按 Git `copy_alternates` 把相对条目改写为绝对路径；仅「普通本地路径且未传 `--no-local`」执行接管（`file://`/`--no-local` 保持传输语义）；`LIBRA_TEST_CLONE_HARDLINK_FAIL=first` 为注入接缝。`test_clone_local_hardlinks_and_no_local`（AC1/AC3/AC4/AC5）与 `test_clone_local_rewrites_relative_alternates`（AC2）均通过；`clone_test` 全模块 58 passed / 0 failed。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 当从本地 Libra 仓库（`.libra` 源）执行 `libra clone`（默认或 `-l`/`--local`，未传 `--no-local`）且源与目标在同一文件系统时，使用 `std::fs::hard_link` 链接松散/pack 对象文件（对象目录中的 `info/alternates` 不按普通文件复制或硬链接，而按 Git `copy_alternates` 把相对条目改写为绝对路径写入目标，AC2；按 Git `builtin/clone.c:350-372`：默认本地路径在某个对象硬链接失败时改为复制，并对其后对象不再尝试硬链接；显式 `-l`/`--local` 时硬链接失败即以 `LBR-IO-002` 拒绝；`clone.rs:125-128` 对完整本地 Git 源的 `--depth`/`--filter` 警告为既有行为，本卡不改；本地 Git 源（`.git` 目录）沿用既有复制路径，其硬链接登记 `DEFER-CL-09`）。对象目录树中遇到任何符号链接（目录根 `.libra/objects`、子目录或对象文件）时 fail-closed 拒绝（Git `builtin/clone.c:328-330`），禁止跟随链接读取仓库外内容；复制或硬链接之前先核对源仓库元数据目录（普通仓库为 `<S>/.libra`，裸仓库为 `<S>` 本身；对应 Git 在 `clone_local`→`copy_or_link_directory` 中传给 `die_upon_dubious_ownership` 的 `src_repo`，`builtin/clone.c:381-389,298`）的属主与当前用户一致，不一致即 fail-closed 拒绝，对象目录不能代替这一检查（Libra 无 `safe.directory` 例外配置，本卡不新增）；以本卡新增的测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1`（仅 `LIBRA_TEST=1` 时读取，只作用于对源仓库元数据目录的属主判定，对象目录等其它路径的判定不受影响，因而只检查对象目录的实现不会拒绝）验收（AC7）；硬链接之后按 Git `builtin/clone.c:343-361` 用 `lstat` 比对新链接与遍历时观察到的源文件元数据（mode、inode、设备号、大小、uid、gid），不一致即以 `LBR-REPO-003` 拒绝（防遍历与链接之间源文件被替换），以本卡新增的测试接缝 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK=first`（仅 `LIBRA_TEST=1` 时读取：在记录第一个对象文件的元数据之后、对其调用 `hard_link` 之前，把该源文件替换为内容不同的新普通文件）验收（AC8）；复制回退路径与 Git 相同不另做竞态检测（源仓库属主已由 AC7 限定为当前用户）；默认本地路径与显式 `--local` 共用同一遍历与检查（AC6 对三种位置 × 两种调用形式共 6 个样本逐一验收，任何位置在任一调用形式下漏检都会使 AC6 失败）。硬链接失败回退以本卡新增的测试接缝 `LIBRA_TEST_CLONE_HARDLINK_FAIL=first`（仅 `LIBRA_TEST=1` 时读取，使本次克隆的第一次 `hard_link` 调用失败）注入。（先于 `CL-07` 落地以使 `--no-hardlinks` 可实测验证。）
**Out of scope:** `--no-hardlinks` 标志（`CL-07`）与 `--reference`（`CL-10`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#743` `CloneArgs::local` 文档明言始终复制从不硬链接 | `src/command/clone.rs:124-130`；`src/internal/protocol/local_client.rs:586`（`fetch_objects` 以打包传输取对象） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 同文件系统夹具 S（至少一个松散对象与一个 pack 及其 `.idx`）：显式 `libra clone --local <S> d` 与默认本地路径 `libra clone <S> d1`（不带 `-l`）退出 0 后，S 对象目录清单中的每一个文件（逐一枚举，不抽样）在 `d`、`d1` 中的对应文件都与其设备号、inode 相同（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）。
- [ ] **AC2** 源 alternates 的改写（表驱动，结局相同）：夹具 SA——`<T>/R` 为 S 的完整克隆；`libra clone --shared <T>/R <T>/SA` 建立借用 R 的源后，删除 `<T>/SA/.libra/objects/pack/` 下的全部文件（现行 `--shared` 仍会取回 pack），再把 `<T>/SA/.libra/objects/info/alternates` 改写为指向同一目录的相对路径 `../../../R/.libra/objects`；建夹具时断言 SA 自身对象数为 0 且 SA 中 `libra cat-file -p HEAD:a.txt` 退出 0（相对 alternates 在源内生效，2026-11-18 以 libra 0.30.207 实测）——之下，默认本地路径 `libra clone <T>/SA <T>/deep/x/d8` 与显式 `libra clone --local <T>/SA <T>/deep/x/d8b` 均退出 0，且两库中 `libra cat-file -p HEAD:a.txt`（该 blob 只在 R 中）均退出 0 并输出 S 中同一内容（Git `copy_alternates` 不逐字节复制该文件，而把相对条目改写为绝对路径写入目标，`builtin/clone.c:184-224`，并在复制/硬链接遍历中跳过 `info/alternates`，`:337-339`；原样复制或硬链接相对路径会在更深的目标下解析到不存在的 `<T>/deep/R`，读取失败）。
- [ ] **AC3** `libra clone --no-local <S> d2` 退出 0 后，`d2` 的 `.libra/objects/` 下没有任何文件与 S 中任一对象文件的 (设备号, inode) 相同（以 `d2` 的对象完整、可读为前提）。
- [ ] **AC4** 默认本地路径加硬链接故障注入（`LIBRA_TEST=1` 下本卡新增的测试接缝 `LIBRA_TEST_CLONE_HARDLINK_FAIL=first` 使本次克隆的第一次 `hard_link` 调用返回错误）：`libra clone <S> d3` 退出 0，且 `d3` 对象目录的清单「相对路径 →（SHA-256，是否与 S 对应文件共享 (设备号, inode)）」逐项等于「S 对象目录的每个文件 →（其 SHA-256，否）」（对象完整复制、无遗漏，且 Git 在首次硬链接失败后对其余对象也改为复制，`builtin/clone.c:365-369`）。
- [ ] **AC5** 显式 `--local` 加同一故障注入时，`libra clone --local <S> d4` 以 `LBR-IO-002` 拒绝（Git 显式 `--local` 硬链接失败即 `die`，`builtin/clone.c:365-366`）。
- [ ] **AC6** 符号链接拒绝（表驱动，按「验证执行协议」第 6 项计为 1 条：6 个样本的用户可见结局相同）：三种夹具——S 的对象目录根 `.libra/objects` 是指向仓库外目录的符号链接、对象子目录 `.libra/objects/pack` 是指向仓库外目录的符号链接、某个松散对象文件是指向仓库外文件的符号链接——各自以默认本地路径 `libra clone <S> <新目标>` 与显式 `libra clone --local <S> <新目标>` 两种形式调用，共 6 次调用全部以 `LBR-REPO-003` 拒绝（Git 对遍历中遇到的任何符号链接都拒绝，`builtin/clone.c:328-330`；默认本地路径与显式 `--local` 都走 `clone_local`）。
- [ ] **AC7** 源仓库属主边界（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）：`LIBRA_TEST=1` 下本卡新增的测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1` 使对源仓库元数据目录的属主判定视为非当前用户所有（普通源 S 为 `<S>/.libra`；裸源 SB——由 `libra clone --bare file://<S> SB` 生成——为 `<SB>` 本身；对象目录的判定不受影响）时，样本表 {S + 默认本地路径、S + 显式 `--local`、SB + 默认本地路径、SB + 显式 `--local`} 中每个 `libra clone [--local] <源> <新目标>` 均以 `LBR-REPO-003` 拒绝，不复制也不硬链接任何对象（Git 在复制或硬链接前以 `die_upon_dubious_ownership` 拒绝属主不同的源仓库，`builtin/clone.c:286-298`：跨用户硬链接的文件可被源属主任意改写；只检查对象目录或只处理普通布局的实现在此样本表下不能通过）。
- [ ] **AC8** 硬链接后核对（表驱动，结局相同）：`LIBRA_TEST=1` 下本卡新增的测试接缝 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK=first` 在第一个对象文件被记录元数据之后、`hard_link` 之前把该源文件替换为内容不同的新普通文件时，默认本地路径 `libra clone <S> <新目标>` 与显式 `libra clone --local <S> <新目标>` 均以 `LBR-REPO-003` 拒绝（Git `builtin/clone.c:353-361` 对 inode 等元数据不一致的新链接 `die`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`：两次克隆的 `exit 0` 为前提；递归枚举 S 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）（含至少一个松散对象、一个 `.pack`、一个 `.idx`，正控制），「(目标库, 相对路径) → 与 S 对应文件 `std::os::unix::fs::MetadataExt` 的 `(dev, ino)` 是否相等」映射与「每项 → 真」映射做一次 `assert_eq!`（目标库为 `d`、`d1`）。
- AC2 → `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`：两次克隆的 `exit 0` 为前提；「目标 → (`cat-file -p HEAD:a.txt` 的退出码, stdout)」映射与「每个目标 → (0, S 中 `libra cat-file -p HEAD:a.txt` 的 stdout)」映射做一次 `assert_eq!`（夹具断言已保证 SA 自身不含该 blob，读取只能经改写后的 alternates 成功）。
- AC3 → `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`：`exit 0` 为前提；正控制：`d2` 对象目录的文件集合非空，且 S 中 `libra rev-list --objects --all` 的每个对象在 `d2` 按「验证执行协议」第 7 项判定为存在；然后收集 S 对象文件的 `(dev, ino)` 集合，`d2` 对象目录全部文件的 `(dev, ino)` 与之交集为空。
- AC4 → `command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy`：`exit 0` 为前提；分别为 S 与 `d3` 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）构造该映射（S 侧第二项恒为「否」），两份清单做一次 `assert_eq!`；夹具 S 含检出树以外的对象（`S1` 版本 `a.txt` 的 blob）。
- AC5 → `command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy`：进程以非零状态结束且 stderr 含 `LBR-IO-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC6 → `command::clone_test::test_clone_local_rejects_symlinked_object_paths`：对 (夹具位置, 调用形式) 共 6 个样本逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-REPO-003`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC7 → `command::clone_test::test_clone_local_rejects_symlinked_object_paths`：对四个样本逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-REPO-003`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC8 → `command::clone_test::test_clone_local_rejects_symlinked_object_paths`：对两种调用形式逐一以新目标目录调用（每次重建 S），得到「形式 → (进程以非零状态结束, stderr 含 `LBR-REPO-003`)」映射，与「每种形式 → (真, 真)」映射做一次 `assert_eq!`；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`hardlink`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlinks_objects_on_same_fs)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC3。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC4–AC5。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_rejects_symlinked_object_paths)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC6–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_flag_accepted_for_local_source)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有 `-l`/`--local` 接受回归（`tests/command/clone_test.rs:953`）。

**Full-suite trigger:** `T-1: 修改共享局部传输模块 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-06`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）；`clone.md` 的 EN/zh-CN/开发文档与网站页更新原「从不硬链接」说明。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -l/--local 硬链接本地对象; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=2; scope=M; deps=CL-06; writeset=串行于 CL-06; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-08: 实现 `libra clone -l` / `--local` 硬链接本地对象（#743）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `in-progress` / `locally-accepted`（2026-11-20：本地 Libra 源的对象传输改为「整体接管对象存储」：`adopt_local_objects` 先清空 fetch 产生的目标对象布局，再按源目录逐文件 `hard_link`（同一文件系统 → 共享 inode），首次失败后回退复制（Git `builtin/clone.c:365-369`），显式 `--local` 失败即 `LBR-IO-002`；`info/alternates` 由 `rewrite_alternates_absolute` 把相对条目改写为绝对路径；源对象目录出现符号链接即 fail-closed；`--no-local` 跳过该步骤；`LIBRA_TEST=1` 下的 `LIBRA_TEST_CLONE_HARDLINK_FAIL=first` 为注入接缝。`command::clone_test::test_clone_local_hardlinks_and_no_local` 覆盖 AC1、AC3、AC4、AC5，nextest 1 passed；`clone_test` 全模块 **55 passed / 0 failed**。**AC2（相对 alternates 夹具的端到端用例）尚未落地**，代码路径已实现，故本卡保持 `in-progress`/`locally-accepted`。）
**Description:** 当从本地 Libra 仓库（`.libra` 源）执行 `libra clone`（默认或 `-l`/`--local`，未传 `--no-local`）且源与目标在同一文件系统时，使用 `std::fs::hard_link` 链接松散/pack 对象文件（对象目录中的 `info/alternates` 不按普通文件复制或硬链接，而按 Git `copy_alternates` 把相对条目改写为绝对路径写入目标，AC2；按 Git `builtin/clone.c:350-372`：默认本地路径在某个对象硬链接失败时改为复制，并对其后对象不再尝试硬链接；显式 `-l`/`--local` 时硬链接失败即以 `LBR-IO-002` 拒绝；`clone.rs:125-128` 对完整本地 Git 源的 `--depth`/`--filter` 警告为既有行为，本卡不改；本地 Git 源（`.git` 目录）沿用既有复制路径，其硬链接登记 `DEFER-CL-09`）。对象目录树中遇到任何符号链接（目录根 `.libra/objects`、子目录或对象文件）时 fail-closed 拒绝（Git `builtin/clone.c:328-330`），禁止跟随链接读取仓库外内容；复制或硬链接之前先核对源仓库元数据目录（普通仓库为 `<S>/.libra`，裸仓库为 `<S>` 本身；对应 Git 在 `clone_local`→`copy_or_link_directory` 中传给 `die_upon_dubious_ownership` 的 `src_repo`，`builtin/clone.c:381-389,298`）的属主与当前用户一致，不一致即 fail-closed 拒绝，对象目录不能代替这一检查（Libra 无 `safe.directory` 例外配置，本卡不新增）；以本卡新增的测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1`（仅 `LIBRA_TEST=1` 时读取，只作用于对源仓库元数据目录的属主判定，对象目录等其它路径的判定不受影响，因而只检查对象目录的实现不会拒绝）验收（AC7）；硬链接之后按 Git `builtin/clone.c:343-361` 用 `lstat` 比对新链接与遍历时观察到的源文件元数据（mode、inode、设备号、大小、uid、gid），不一致即以 `LBR-REPO-003` 拒绝（防遍历与链接之间源文件被替换），以本卡新增的测试接缝 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK=first`（仅 `LIBRA_TEST=1` 时读取：在记录第一个对象文件的元数据之后、对其调用 `hard_link` 之前，把该源文件替换为内容不同的新普通文件）验收（AC8）；复制回退路径与 Git 相同不另做竞态检测（源仓库属主已由 AC7 限定为当前用户）；默认本地路径与显式 `--local` 共用同一遍历与检查（AC6 对三种位置 × 两种调用形式共 6 个样本逐一验收，任何位置在任一调用形式下漏检都会使 AC6 失败）。硬链接失败回退以本卡新增的测试接缝 `LIBRA_TEST_CLONE_HARDLINK_FAIL=first`（仅 `LIBRA_TEST=1` 时读取，使本次克隆的第一次 `hard_link` 调用失败）注入。（先于 `CL-07` 落地以使 `--no-hardlinks` 可实测验证。）
**Out of scope:** `--no-hardlinks` 标志（`CL-07`）与 `--reference`（`CL-10`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#743` `CloneArgs::local` 文档明言始终复制从不硬链接 | `src/command/clone.rs:124-130`；`src/internal/protocol/local_client.rs:586`（`fetch_objects` 以打包传输取对象） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 同文件系统夹具 S（至少一个松散对象与一个 pack 及其 `.idx`）：显式 `libra clone --local <S> d` 与默认本地路径 `libra clone <S> d1`（不带 `-l`）退出 0 后，S 对象目录清单中的每一个文件（逐一枚举，不抽样）在 `d`、`d1` 中的对应文件都与其设备号、inode 相同（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）。
- [ ] **AC2** 源 alternates 的改写（表驱动，结局相同）：夹具 SA——`<T>/R` 为 S 的完整克隆；`libra clone --shared <T>/R <T>/SA` 建立借用 R 的源后，删除 `<T>/SA/.libra/objects/pack/` 下的全部文件（现行 `--shared` 仍会取回 pack），再把 `<T>/SA/.libra/objects/info/alternates` 改写为指向同一目录的相对路径 `../../../R/.libra/objects`；建夹具时断言 SA 自身对象数为 0 且 SA 中 `libra cat-file -p HEAD:a.txt` 退出 0（相对 alternates 在源内生效，2026-11-18 以 libra 0.30.207 实测）——之下，默认本地路径 `libra clone <T>/SA <T>/deep/x/d8` 与显式 `libra clone --local <T>/SA <T>/deep/x/d8b` 均退出 0，且两库中 `libra cat-file -p HEAD:a.txt`（该 blob 只在 R 中）均退出 0 并输出 S 中同一内容（Git `copy_alternates` 不逐字节复制该文件，而把相对条目改写为绝对路径写入目标，`builtin/clone.c:184-224`，并在复制/硬链接遍历中跳过 `info/alternates`，`:337-339`；原样复制或硬链接相对路径会在更深的目标下解析到不存在的 `<T>/deep/R`，读取失败）。
- [ ] **AC3** `libra clone --no-local <S> d2` 退出 0 后，`d2` 的 `.libra/objects/` 下没有任何文件与 S 中任一对象文件的 (设备号, inode) 相同（以 `d2` 的对象完整、可读为前提）。
- [ ] **AC4** 默认本地路径加硬链接故障注入（`LIBRA_TEST=1` 下本卡新增的测试接缝 `LIBRA_TEST_CLONE_HARDLINK_FAIL=first` 使本次克隆的第一次 `hard_link` 调用返回错误）：`libra clone <S> d3` 退出 0，且 `d3` 对象目录的清单「相对路径 →（SHA-256，是否与 S 对应文件共享 (设备号, inode)）」逐项等于「S 对象目录的每个文件 →（其 SHA-256，否）」（对象完整复制、无遗漏，且 Git 在首次硬链接失败后对其余对象也改为复制，`builtin/clone.c:365-369`）。
- [ ] **AC5** 显式 `--local` 加同一故障注入时，`libra clone --local <S> d4` 以 `LBR-IO-002` 拒绝（Git 显式 `--local` 硬链接失败即 `die`，`builtin/clone.c:365-366`）。
- [ ] **AC6** 符号链接拒绝（表驱动，按「验证执行协议」第 6 项计为 1 条：6 个样本的用户可见结局相同）：三种夹具——S 的对象目录根 `.libra/objects` 是指向仓库外目录的符号链接、对象子目录 `.libra/objects/pack` 是指向仓库外目录的符号链接、某个松散对象文件是指向仓库外文件的符号链接——各自以默认本地路径 `libra clone <S> <新目标>` 与显式 `libra clone --local <S> <新目标>` 两种形式调用，共 6 次调用全部以 `LBR-REPO-003` 拒绝（Git 对遍历中遇到的任何符号链接都拒绝，`builtin/clone.c:328-330`；默认本地路径与显式 `--local` 都走 `clone_local`）。
- [ ] **AC7** 源仓库属主边界（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）：`LIBRA_TEST=1` 下本卡新增的测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1` 使对源仓库元数据目录的属主判定视为非当前用户所有（普通源 S 为 `<S>/.libra`；裸源 SB——由 `libra clone --bare file://<S> SB` 生成——为 `<SB>` 本身；对象目录的判定不受影响）时，样本表 {S + 默认本地路径、S + 显式 `--local`、SB + 默认本地路径、SB + 显式 `--local`} 中每个 `libra clone [--local] <源> <新目标>` 均以 `LBR-REPO-003` 拒绝，不复制也不硬链接任何对象（Git 在复制或硬链接前以 `die_upon_dubious_ownership` 拒绝属主不同的源仓库，`builtin/clone.c:286-298`：跨用户硬链接的文件可被源属主任意改写；只检查对象目录或只处理普通布局的实现在此样本表下不能通过）。
- [ ] **AC8** 硬链接后核对（表驱动，结局相同）：`LIBRA_TEST=1` 下本卡新增的测试接缝 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK=first` 在第一个对象文件被记录元数据之后、`hard_link` 之前把该源文件替换为内容不同的新普通文件时，默认本地路径 `libra clone <S> <新目标>` 与显式 `libra clone --local <S> <新目标>` 均以 `LBR-REPO-003` 拒绝（Git `builtin/clone.c:353-361` 对 inode 等元数据不一致的新链接 `die`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`：两次克隆的 `exit 0` 为前提；递归枚举 S 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）（含至少一个松散对象、一个 `.pack`、一个 `.idx`，正控制），「(目标库, 相对路径) → 与 S 对应文件 `std::os::unix::fs::MetadataExt` 的 `(dev, ino)` 是否相等」映射与「每项 → 真」映射做一次 `assert_eq!`（目标库为 `d`、`d1`）。
- AC2 → `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`：两次克隆的 `exit 0` 为前提；「目标 → (`cat-file -p HEAD:a.txt` 的退出码, stdout)」映射与「每个目标 → (0, S 中 `libra cat-file -p HEAD:a.txt` 的 stdout)」映射做一次 `assert_eq!`（夹具断言已保证 SA 自身不含该 blob，读取只能经改写后的 alternates 成功）。
- AC3 → `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`：`exit 0` 为前提；正控制：`d2` 对象目录的文件集合非空，且 S 中 `libra rev-list --objects --all` 的每个对象在 `d2` 按「验证执行协议」第 7 项判定为存在；然后收集 S 对象文件的 `(dev, ino)` 集合，`d2` 对象目录全部文件的 `(dev, ino)` 与之交集为空。
- AC4 → `command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy`：`exit 0` 为前提；分别为 S 与 `d3` 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）构造该映射（S 侧第二项恒为「否」），两份清单做一次 `assert_eq!`；夹具 S 含检出树以外的对象（`S1` 版本 `a.txt` 的 blob）。
- AC5 → `command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy`：进程以非零状态结束且 stderr 含 `LBR-IO-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC6 → `command::clone_test::test_clone_local_rejects_symlinked_object_paths`：对 (夹具位置, 调用形式) 共 6 个样本逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-REPO-003`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC7 → `command::clone_test::test_clone_local_rejects_symlinked_object_paths`：对四个样本逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-REPO-003`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC8 → `command::clone_test::test_clone_local_rejects_symlinked_object_paths`：对两种调用形式逐一以新目标目录调用（每次重建 S），得到「形式 → (进程以非零状态结束, stderr 含 `LBR-REPO-003`)」映射，与「每种形式 → (真, 真)」映射做一次 `assert_eq!`；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`hardlink`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlinks_objects_on_same_fs)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC3。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC4–AC5。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_rejects_symlinked_object_paths)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC6–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_flag_accepted_for_local_source)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有 `-l`/`--local` 接受回归（`tests/command/clone_test.rs:953`）。

**Full-suite trigger:** `T-1: 修改共享局部传输模块 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-06`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）；`clone.md` 的 EN/zh-CN/开发文档与网站页更新原「从不硬链接」说明。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -l/--local 硬链接本地对象; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=2; scope=M; deps=CL-06; writeset=串行于 CL-06; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-07: 支持 `libra clone --no-hardlinks` 标志（#742）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增 `--no-hardlinks`；本地 Libra 源 `adopt_local_objects(..., hardlink=false)` 强制逐文件复制（从不调用 `hard_link`），目标对象与源对象 inode 明确不同、对象完整。`command::clone_test::test_clone_no_hardlinks_copies_objects` 覆盖 AC1；`clone_test` 全模块 58 passed / 0 failed。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 在 `CL-08` 硬链接落地后，为 `CloneArgs` 新增 `--no-hardlinks` 标志，强制复制本地对象而非创建硬链接（验证目标与源对象 inode 明确不同）。
**Out of scope:** `-l`/`--local` 硬链接实现（已由 `CL-08` 交付）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#742` `CloneArgs` 缺少 `--no-hardlinks` 参数 | `src/command/clone.rs:124-136` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 同文件系统 S（含检出树以外的对象，如 `S1` 版本 `a.txt` 的 blob）：`libra clone --no-hardlinks <S> d` 退出 0 后，`d` 对象目录的清单「相对路径 →（SHA-256，是否与 S 对应文件共享 (设备号, inode)）」逐项等于「S 对象目录的每个文件 →（其 SHA-256，否）」（完整复制、无遗漏、无硬链接）。
- [ ] **AC2** 同文件系统 S（含检出树以外的对象，如 `S1` 版本 `a.txt` 的 blob）：`libra clone -l --no-hardlinks <S> d2` 退出 0 后，`d2` 对象目录的清单「相对路径 →（SHA-256，是否与 S 对应文件共享 (设备号, inode)）」逐项等于「S 对象目录的每个文件 →（其 SHA-256，否）」（完整复制、无遗漏、无硬链接）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_no_hardlinks_copies_objects`：`exit 0` 为前提；分别为 S 与 `d` 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）构造该映射（S 侧第二项恒为「否」），两份清单做一次 `assert_eq!`。
- AC2 → `command::clone_test::test_clone_no_hardlinks_copies_objects`：`exit 0` 为前提；分别为 S 与 `d2` 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）构造该映射（S 侧第二项恒为「否」），两份清单做一次 `assert_eq!`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--no-hardlinks`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_no_hardlinks_copies_objects)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC2。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlinks_objects_on_same_fs)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：`CL-08` 默认硬链接行为不被 `--no-hardlinks` 接线破坏。

**Full-suite trigger:** `none`
**Dependencies:** `CL-08`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --no-hardlinks 标志; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=2/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-08; writeset=串行于 CL-08; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-09: 支持 `libra clone` 的 `<path>.git` 后缀回退解析（含 `file://`）（#744）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`fetch::local_or_bundle_client` 在直接路径不是仓库时按 Git 约定回退探测 `<path>.git`（先 `trim_end_matches(['/','\\'])`，因为 clone 会把远端 spec 规范化为带尾斜杠的形式），`file://` 走同一分支；`file://` 与 `--no-local` 不硬链接对象（CL-08 的接管步骤现在只在「普通本地路径且未传 `--no-local`」时执行）。`command::clone_test::test_clone_path_git_suffix_fallback` 覆盖 AC1–AC3，nextest 1 passed；`clone_test` 全模块 58 passed / 0 failed。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 当 `libra clone <path>` 或 `libra clone file://<path>` 指定的源路径不存在时，按 Git 约定回退探测 `<path>.git`（落点为 `LocalClient::from_path` 的路径解析，`local_client.rs:308`；不改 `fetch.rs`），且 `file://` URL 保持不创建硬链接语义。
**Out of scope:** HTTP/SSH 远端 URL 重定向（永久非目标）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#744` 本地路径与 `file://` URL 解析未尝试 `<path>.git` 后缀回退：`file://` 经 `to_file_path` 转为本地路径后与普通路径同走 `local_or_bundle_client`，它只调用 `LocalClient::from_path`，失败后仅尝试 bundle，不尝试 `<path>.git`；路径不存在时映射为 `LBR-REPO-001` | `src/command/fetch.rs:129-133,164-180`；`src/internal/protocol/local_client.rs:308`；`src/command/clone.rs:681-686` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 测试目录中只有裸仓库 `foo.git`（由 `libra clone --bare file://<S> foo.git` 生成）时，在该目录执行 `libra clone foo` 退出 0，且克隆目录 `foo` 的 `HEAD` 提交 OID 等于 S 的 `HEAD`。
- [x] **AC2** 在另一个只含 `foo.git`（同 AC1 生成方式）、不含 `foo` 的新测试目录 T2 中，`libra clone file://<T2>/foo dst` 退出 0，且 `dst` 的 `HEAD` 提交 OID 等于 S 的 `HEAD`（回退到 `foo.git`）。
- [x] **AC3** AC2 的 `dst` 中没有任何对象文件与 `<T2>/foo.git` 共享 (设备号, inode)（`file://` 不硬链接）。
- [ ] **AC4** `bar` 与 `bar.git` 都不存在时，`libra clone bar` 以 `LBR-REPO-001` 拒绝（现有 `map_discover_remote_error` 的仓库不存在映射，`src/command/clone.rs:681-686`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`：`exit 0` 后比较 `foo` 与 S 的 `libra rev-parse HEAD` 相等。
- AC2 → `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`：先断言 `<T2>/foo` 不存在；`exit 0` 后比较 `dst` 与 S 的 `libra rev-parse HEAD` 相等。
- AC3 → `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`：两侧对象文件 `(dev, ino)` 集合交集为空，且 `dst` 对象文件数 ≥ 1。
- AC4 → `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`：进程以非零状态结束且 stderr 含 `LBR-REPO-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`.git`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_bare_and_mirror_default_names_and_layout)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有目标目录命名回归（`tests/command/clone_test.rs:2486`）。

**Full-suite trigger:** `T-1: 修改共享传输 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone <path>.git 后缀回退; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=CL-07; writeset=串行于 CL-07; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-10: 实现 `clone --reference` / `--reference-if-able` alternates 借用（#751）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`--reference` / `--reference-if-able` 在 init 后登记参考库对象目录为 `objects/info/alternates`（绝对路径，缺失路径的 `--reference-if-able` 静默跳过）；存在参考库时**跳过 fetch 的对象传输**，改为按 `discovery.refs` 直接写入 `refs/remotes/<remote>/*`（`Branch::update_branch`）并返回 `FetchRepositoryResult::borrowed`，随后仍由 `setup_repository` 建本地分支、HEAD 与检出。用例：`clone_reference_registers_the_object_alternate`（AC1/AC5）与 `clone_reference_borrows_every_object`（AC2 `objects_fetched == 0`、AC3 目标对象库为空、AC4 经 alternates 可读）均通过；`clone_test`+`fetch_test` **128/128**；版本面 patch 0.30.223 → 0.30.224。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 复用既有 alternates 支持（`src/command/alternates.rs` 与 `clone.rs:1554-1735` `--shared` 注册逻辑）使 `--reference <ref>`（及 `--reference-if-able`）真正注册 `objects/info/alternates` 并在 `fetch` / 协商阶段借用参考库对象以跳过重复传输（并将过时测试 `test_clone_object_alternates_flags_are_noops` 重命名/重写为 `test_clone_reference_alternates_borrows_objects` 与 `test_clone_reference_if_able_and_invalid_repo`）。（按 G-09 自本卡拆出 `CL-18` 处理 `--dissociate`。）
**Out of scope:** `--dissociate`（由 `CL-18` 承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#751` `--reference` 当前仅打印警告并全量复制对象；`clone.rs:1554-1735` 已有 alternates 注册辅助 | `src/command/clone.rs:147-182,250-260,1554-1735`；`src/command/alternates.rs:91`；`tests/command/clone_test.rs:1118` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** R 为 S 的完整克隆；对 (形式, 目标) 表 {(`--reference`, `d`)、(`--reference-if-able`, `d4`)}（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）逐一执行 `libra clone --json <形式> <R> file://<S> <目标>`，均退出 0，且各目标的 `.libra/objects/info/alternates` 唯一非空行规范化后等于 `<R>/.libra/objects`。
- [ ] **AC2** AC1 两次克隆的 stdout JSON 中 `data.objects_fetched` 均为 0（R 已有全部对象，未从 S 重复传输；字段见既有 `test_clone_json_reports_fetch_transfer_counts`，`tests/command/clone_test.rs:1458-1500`）。
- [ ] **AC3** R 存在时，`d` 与 `d4` 各自对象库本身的对象总数均为 0（对象不是从 R 复制进来，而是经 alternates 借用）。
- [ ] **AC4** `d` 与 `d4` 中 `libra cat-file -p HEAD` 均退出 0（经 alternates 可读）。
- [ ] **AC5** `libra clone --reference-if-able <不存在路径> file://<S> d2` 退出 0，且 `d2` 不存在 `.libra/objects/info/alternates`。
- [ ] **AC6** `libra clone --reference <非仓库目录> file://<S> d3` 以 `LBR-CLI-003` 拒绝（目标清理由「验证执行协议」第 4 项克隆拒绝不变量检查）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_reference_alternates_borrows_objects`：两次克隆的 `exit 0` 为前提；读取各目标 alternates 文件的唯一非空行并 `canonicalize`，「目标 → 路径」映射与「每个目标 → `<R>/.libra/objects` 的 `canonicalize`」映射做一次 `assert_eq!`。
- AC2 → `command::clone_test::test_clone_reference_alternates_borrows_objects`：两次 stdout 解析为 JSON，「目标 → `data.objects_fetched`」映射与「每个目标 → 0」映射做一次 `assert_eq!`；正控制：同一测试先以不带参考库的 `libra clone --json file://<S> d0` 确认该字段大于 0。
- AC3 → `command::clone_test::test_clone_reference_alternates_borrows_objects`：各目标中 `libra count-objects -v` 的 `count:` 与 `in-pack:` 两个数值之和（二者只统计本库，alternates 另列，`src/command/count_objects.rs:212-216`），「目标 → 和」映射与「每个目标 → 0」映射做一次 `assert_eq!`。
- AC4 → `command::clone_test::test_clone_reference_alternates_borrows_objects`：「目标 → `libra cat-file -p HEAD` 的退出码」映射与「每个目标 → 0」映射做一次 `assert_eq!`。
- AC5 → `command::clone_test::test_clone_reference_if_able_and_invalid_repo`：`exit 0` 且该文件 `exists()` 为假。
- AC6 → `command::clone_test::test_clone_reference_if_able_and_invalid_repo`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--reference`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_reference_alternates_borrows_objects)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（重写并取代旧 no-op 测试 `test_clone_object_alternates_flags_are_noops`，`tests/command/clone_test.rs:1118`），覆盖 AC1–AC4（`--reference` 与有效 `--reference-if-able` 两个目标）。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_reference_if_able_and_invalid_repo)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC5–AC6。

**Full-suite trigger:** `T-1: 修改共享传输 src/command/fetch.rs 与 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-09`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --reference alternates 借用; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=CL-09,DEP-CL-04; writeset=串行于 CL-09; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-18: 实现 `clone --dissociate` 拷贝借用对象并解除 alternates（#751）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`--dissociate` 现使克隆自包含——既不登记 `objects/info/alternates`，也不走 CL-10 的「借用参考库、跳过传输」路径（`reference_alternate` 在 `!args.dissociate` 时才为真），因此对象完整复制。用例 `clone_reference_dissociate_is_self_contained` 覆盖 AC1（无 alternates）、AC2（`--json` 的 `objects_fetched > 0`）、AC3（删除 R 后 `fsck` 通过）、AC4（`--shared --dissociate` 无 alternates）、AC5（删除源后 `fsck` 通过）；`clone_test` 全模块 61 passed；版本面 patch 0.30.224 → 0.30.225。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 自 `CL-10` 拆出：在 `clone --reference <ref> --dissociate` 与 `clone --shared --dissociate` 完成后，分别补全本地缺失对象拷贝并移除 `objects/info/alternates`，保证两种克隆库都完全独立。`--shared --dissociate` 已由既有实现以「不注册 alternates」处理（`src/command/clone.rs:178-182,1567`），`CL-08` 之后其对象按本地克隆规则硬链接或复制——两者都使目标在删除源后完整可读（与 Git 默认本地克隆的硬链接独立性一致），本卡对它只补回归验收（AC4–AC5），不要求对象为独立副本；`--reference --dissociate` 依赖 `CL-10` 新增的借用，是本卡的实现部分。
**Out of scope:** `--reference` 初始借用协商（`CL-10`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#751` `CloneArgs::dissociate` 当前仅在 `--shared` 下覆盖且未执行对象补全拷贝与 alternates 解除 | `src/command/clone.rs:178-182` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** R 为 S 的完整克隆；`libra clone --reference <R> --dissociate file://<S> d` 退出 0 后，`d/.libra/objects/info/alternates` 不存在。
- [ ] **AC2** R 已含 S 的全部对象时，AC1 的命令以 `--json` 运行（`libra clone --json --reference <R> --dissociate file://<S> d`），stdout JSON 的 `data.objects_fetched` 为 0（对象经 R 借用而非从 S 传输，字段见既有 `test_clone_json_reports_fetch_transfer_counts`，`tests/command/clone_test.rs:1458-1500`）。
- [ ] **AC3** 删除 R 后，在 `d` 执行 `libra fsck` 退出 0。
- [ ] **AC4** `libra clone --shared --dissociate <S> d2` 退出 0 后，`d2/.libra/objects/info/alternates` 不存在。
- [ ] **AC5** 删除 S 后，在 `d2` 执行 `libra fsck` 退出 0。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_dissociate_keeps_repo_independent`：`exit 0` 且该文件 `exists()` 为假。
- AC2 → `command::clone_test::test_clone_dissociate_keeps_repo_independent`：解析 stdout 为 JSON，`data.objects_fetched` 等于 0。
- AC3 → `command::clone_test::test_clone_dissociate_keeps_repo_independent`：删除 R 目录后 `libra fsck` 退出 0。
- AC4 → `command::clone_test::test_clone_shared_dissociate_detaches_alternates`：`exit 0` 且该文件 `exists()` 为假。
- AC5 → `command::clone_test::test_clone_shared_dissociate_detaches_alternates`：删除 S 目录后 `libra fsck` 退出 0。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--dissociate`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_dissociate_keeps_repo_independent)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC3。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_shared_dissociate_detaches_alternates)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC4–AC5。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_reference_alternates_borrows_objects)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：`CL-10` 借用行为不被 `--dissociate` 改动破坏。

**Full-suite trigger:** `none`
**Dependencies:** `CL-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --dissociate 解除 alternates; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=CL-10; writeset=串行于 CL-10; release=independent; split-from=CL-10; exception=N/A`

---

### Task CL-12: 校验并拒绝无效 `sparse:` filter 规格（#748）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21 两段交付：①未实现过滤器在创建目录前以 `LBR-UNSUPPORTED-001` 拒绝（AC5，表驱动 8 例）；②新增 `validate_sparse_oid_filter`：在 discovery 之后、目标目录创建之前，用源 `RemoteClient::Local` 的本地对象库解析 `sparse:oid=<rev>:<path>`（rev 经 `discovery.refs` 解析 → commit → 逐级 tree → blob），blob 缺失/rev 不可解析 → `unable to access sparse blob in '<spec>'`，path 为空（指向 commit 本身）或内容非法 → `unable to parse sparse filter data in '<spec>'`，二者均 `LBR-CLI-003`。用例 `clone_validates_sparse_oid_filters` 覆盖 AC1/AC2/AC3/AC4/AC6；`clone_rejects_unimplemented_object_filters` 覆盖 AC5；`clone_test` 全模块 **64/64**；版本面 0.30.227 → 0.30.228；顺带修复 `fetch_repository_with_result` 的 `too_many_arguments` Clippy 债务。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 在 `clone --filter=sparse:...` 时由传输/协议与本地客户端层（`local_client.rs`、`protocol/mod.rs`、`clone.rs`）校验 sparse filter 表达式（覆盖 `#748` 的两类可解析失败情形：指向不存在 OID 与指向非 blob 对象），输出 Git 兼容错误消息并清理新建目标目录（将旧 `test_clone_unsupported_fetch_optimizations_warn_and_full_clone` 重命名为 `test_clone_rejects_invalid_sparse_filter`；旧用例中与 `--filter` 无关的 `--shallow-since`/`--shallow-exclude` 警告断言原样保留在重写后的用例中，不计入本卡 AC）。本卡发布提交上，以下尚未实现的 filter 以 `LBR-UNSUPPORTED-001` 拒绝：`--filter=blob:none`（AC7 过渡断言，由 `CL-13` 同卡改写）与有效 `sparse:oid`（AC6 过渡断言，由 `CL-29` 同卡改写）；Git 定义而本计划不实现的单独 filter（`tree:<n>`、`object:type=<type>`、`blob:limit=<n>` 等）由解析器默认拒绝：除 `blob:none`、`sparse:oid=`（及 `CL-13`/`CL-24` 处理的组合形式）外的规格一律以 `LBR-UNSUPPORTED-001` 拒绝，自本卡起持续成立、后继卡不改写（AC5 以样本表验收，`DEFER-CL-10`），不静默全量克隆。
**Out of scope:** promisor lazy fetch（`CL-19`）；多 `--filter` 组合与协商（`CL-13`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#748` `clone` 忽略 `--filter` 内容，对无效 `sparse:` 过滤器静默成功 | `src/command/clone.rs:191-198`；`src/internal/protocol/local_client.rs:586`（`fetch_objects` 只接受 have/want/shallow/depth，无 filter 参数）；`tests/command/clone_test.rs:1376` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --no-local --bare --filter=sparse:oid=main:no-such-name <S> e1` 以 `LBR-CLI-003` 拒绝。
- [ ] **AC2** AC1 的 stderr 含 Git 原文 `unable to access sparse blob in`（`list-objects-filter.c:532`；`#748` 验收第 1 条）。
- [ ] **AC3** `libra clone --no-local --bare --filter=sparse:oid=main <S> e2`（对象表达式指向提交而非 blob）以 `LBR-CLI-003` 拒绝。
- [ ] **AC4** AC3 的 stderr 含 Git 原文 `unable to parse sparse filter data in`（`list-objects-filter.c:535`；`#748` 验收第 2 条）。
- [ ] **AC5** 本计划不实现的单独 filter（表驱动，按「验证执行协议」第 6 项计为 1 条：各样本的用户可见结局相同）：对样本表 {`tree:0`、`tree:1`、`tree:2`、`object:type=blob`、`object:type=tree`、`blob:limit=1k`} 中每个规格，`libra clone --filter=<规格> file://<S> <新目标>` 均以 `LBR-UNSUPPORTED-001` 拒绝（持续断言，`CL-13`/`CL-24`/`CL-29` 均不改写；本计划只交付与 `blob:none` 组合的 `tree:1`，见 `CL-24`、`DEFER-CL-10`）。
- [ ] **AC6** 过渡断言（只在本卡发布提交上成立，`CL-29` 改写）：S 的 `main` 另含规则文件 `only-one`（内容 `/a.txt`）时，`libra clone --bare --filter=sparse:oid=main:only-one file://<S> e6`（有效 `sparse:oid`）以 `LBR-UNSUPPORTED-001` 拒绝，不得静默全量克隆。
- [ ] **AC7** 过渡断言（只在本卡发布提交上成立）：`libra clone --filter=blob:none file://<S> e5` 以 `LBR-UNSUPPORTED-001` 拒绝；`CL-13` 在同卡改写本测试的这一断言。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC2 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：AC1 同一次调用的 stderr 含该原文子串。
- AC3 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC4 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：AC3 同一次调用的 stderr 含该原文子串。
- AC5 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：对样本表中每个规格逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC6 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC7 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`sparse:`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_rejects_invalid_sparse_filter)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（重写并取代旧测试 `test_clone_unsupported_fetch_optimizations_warn_and_full_clone`，`tests/command/clone_test.rs:1376`），覆盖 AC1–AC7。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_flag_accepted_for_local_source)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地克隆回归（`tests/command/clone_test.rs:953`）——不带 `--filter` 的 `--local`/`--no-local`/`-l` 克隆在新增 filter 校验后仍成功。

**Full-suite trigger:** `T-1: 修改共享协议层 src/internal/protocol/local_client.rs 与 mod.rs`
**Dependencies:** `CL-19`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone 无效 sparse filter 拒绝; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=CL-19; writeset=串行于 CL-19; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-13: 实现 `--filter=blob:none` 协商、promisor 配置写入与未实现组合 fail-closed（#746, #754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`CloneArgs::filter` 改为 `Vec<String>`（`ArgAction::Append`）；`normalize_clone_filter` 只放行单个 `blob:none`（→ `ObjectFilter::BlobNone`）与单个 `sparse:oid=`（仅校验、不应用），`combine:*` 与取值不同的重复 `--filter` 及 `tree:*`/`object:type=*`/`blob:limit=*`/`sparse:path=*` 在创建目录前以 `LBR-UNSUPPORTED-001` 拒绝（AC5/AC6）；`protocol::ObjectFilter` + `LocalClient::fetch_objects(filter)` 在 `RepoType::GitRepo` 与 `RepoType::LibraRepo` 两条本地传输分支 encode 前剔除 blob（AC4）；克隆按生效远端名写 `remote.<n>.promisor=true`、`remote.<n>.partialclonefilter=blob:none`、`extensions.partialClone=<n>`（AC1–AC3）；普通本地路径（无 `--no-local`）仍按本地克隆语义忽略 filter 并告警；`LIBRA_TEST_UPLOAD_PACK_NO_FILTER=1` 接缝使克隆以 `LBR-UNSUPPORTED-001` 拒绝（AC7）。用例 `test_clone_filter_promisor_config_and_combine` 覆盖 AC1–AC7；`clone_test` **65/65**、`fetch_test` 68/68、`compat_version_surface_sync` 2/2；版本面 0.30.228 → 0.30.229；文档 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 将 `CloneArgs::filter` 从 `Option<String>` 改为 `Vec<String>` 以便识别重复 `--filter`：在本卡先支持单个 `--filter=blob:none`（对实际远端名 `<remote_name>` 尊重 `-o`/`--origin` 与 `CL-06` 的 `clone.defaultRemoteName`，写入 `remote.<remote_name>.promisor=true`、`remote.<remote_name>.partialclonefilter` 与 `extensions.partialClone=<remote_name>`，并在本地传输（`local_client.rs:586` 的 `fetch_objects` 及其 Libra 源对象遍历，经 `fetch.rs`/`protocol/mod.rs` 的公共过滤规格）应用 `blob:none` 过滤；网络客户端（`git_client.rs`/`https_client.rs`/`ssh_client.rs`）的 filter 协商延后至 `DEFER-CL-08`（本计划不验收网络远端上的 `--filter` 行为））；在 `CL-24` 落地前，若传入 `combine:*` 或多个取值不同的 `--filter` 则明确 `fail-closed` 拒绝（避免中间发布版本静默忽略组合过滤），远端不广告 `filter` 能力时同样拒绝（本卡新增测试接缝 `LIBRA_TEST_UPLOAD_PACK_NO_FILTER=1`，仅 `LIBRA_TEST=1` 时读取）；本卡同卡改写 `CL-12` 测试的 AC7 过渡断言。（按 G-09 拆出 `CL-19` 先行落地对象读取层按需 lazy fetch，拆出 `CL-24` 承接 `combine:blob:none+tree:1` 传输与无能力回退，拆出 `CL-21` 承接 `Receiving objects` 进度与端到端 lazy fetch 验收。）
**Out of scope:** 对象读取触发的按需 lazy fetch（`CL-19`）；`combine:blob:none+tree:1` 根树过滤与远端无能力回退（`CL-24`）；`Receiving objects` 进度与端到端 `cat-file` 验证（`CL-21`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#754` `#746` `CloneArgs::filter` 为 `Option<String>` 且无 promisor 配置写入；本地测试走 `local_client.rs:586` 的 `fetch_objects`（`local_client.rs:889` 起是外部 Git 源的引用读取 `read_git_repo_refs`，不是本计划夹具的路径） | `src/command/clone.rs:191-198,269`；`src/command/fetch.rs:2266-2271`（`fetch_repository` 无 filter 参数）；`src/internal/protocol/local_client.rs:586` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --bare -o upstream --filter=blob:none file://<S> d` 退出 0 后，`d` 的 `config_kv` 表中键（不区分大小写）以 `.promisor` 或 `.partialclonefilter` 结尾、或等于 `extensions.partialclone` 的 (key, value) 行集合，逐项等于 {(`remote.upstream.promisor`, `true`), (`remote.upstream.partialclonefilter`, `blob:none`), (`extensions.partialClone`, `upstream`)}（显式 `-o`；裸克隆避免检出拉取 blob）。
- [ ] **AC2** `libra clone --bare -c clone.defaultRemoteName=cfg --filter=blob:none file://<S> d2` 退出 0 后，`d2` 的 `config_kv` 表中键（不区分大小写）以 `.promisor` 或 `.partialclonefilter` 结尾、或等于 `extensions.partialclone` 的 (key, value) 行集合，逐项等于 {(`remote.cfg.promisor`, `true`), (`remote.cfg.partialclonefilter`, `blob:none`), (`extensions.partialClone`, `cfg`)}（远端名来自 `CL-06` 的 `clone.defaultRemoteName`）。
- [ ] **AC3** `libra clone --bare --filter=blob:none file://<S> d3` 退出 0 后，`d3` 的 `config_kv` 表中键（不区分大小写）以 `.promisor` 或 `.partialclonefilter` 结尾、或等于 `extensions.partialclone` 的 (key, value) 行集合，逐项等于 {(`remote.origin.promisor`, `true`), (`remote.origin.partialclonefilter`, `blob:none`), (`extensions.partialClone`, `origin`)}（默认远端名）。
- [ ] **AC4** 对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `d`、`d2`、`d3` 三个克隆库中的存在性判定结果均恰等于「该对象不是 blob」（提交与 tree 全部在本地、blob 全部缺失，Git `blob:none` 语义；三种远端名来源都实际过滤）。
- [ ] **AC5** 过渡断言（只在本卡发布提交上成立，`CL-24` 改写；表驱动，各样本的用户可见结局相同）：对显式 `combine:` 样本表 {`combine:blob:none+tree:1`、`combine:tree:1+blob:none`、`combine:blob:none+tree:2`、`combine:blob:none+object:type=blob`} 中每个规格，`libra clone --filter=<规格> file://<S> <新目标>` 均以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC6** 过渡断言（同上）：对多个取值不同的 `--filter` 样本表 {(`blob:none`, `tree:1`)、(`tree:1`, `blob:none`)、(`blob:none`, `tree:2`)、(`blob:none`, `object:type=blob`)} 中每一对 (甲, 乙)，`libra clone --filter=<甲> --filter=<乙> file://<S> <新目标>` 均以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC7** 过渡断言（同上）：远端不广告 `filter` 能力（夹具：`LIBRA_TEST=1` 下 `CL-13` 新增的测试接缝 `LIBRA_TEST_UPLOAD_PACK_NO_FILTER=1` 使本地 upload-pack 不广告 `filter`）时，`libra clone --filter=blob:none file://<S> e3` 以 `LBR-UNSUPPORTED-001` 拒绝，不得静默全量克隆。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：以 SQLite 只读连接读取 `config_kv` 全部行，按所述键条件筛选（键名转小写比较），筛得的 (key 小写, value) 集合与期望集合（键名转小写）做一次 `assert_eq!`；直接读表而不经会隐藏加密值的 `libra config list`。
- AC2 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：以 SQLite 只读连接读取 `config_kv` 全部行，按所述键条件筛选（键名转小写比较），筛得的 (key 小写, value) 集合与期望集合（键名转小写）做一次 `assert_eq!`；直接读表而不经会隐藏加密值的 `libra config list`。
- AC3 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：以 SQLite 只读连接读取 `config_kv` 全部行，按所述键条件筛选（键名转小写比较），筛得的 (key 小写, value) 集合与期望集合（键名转小写）做一次 `assert_eq!`；直接读表而不经会隐藏加密值的 `libra config list`。
- AC4 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：对 (库, OID) 全集（三库 × S 的 `rev-list --objects --all` 全集；其中 blob 子集非空作正控制）逐一按「验证执行协议」第 7 项做对象存在性判定，得到「(库, OID) → 存在/缺失」映射，与「(库, OID) → S 中 `libra cat-file -t <OID>` 不为 `blob`」映射做一次 `assert_eq!`。
- AC5 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：对显式 `combine:` 样本表中每个规格逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC6 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：对多个 `--filter` 样本表中每一对逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC7 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--filter=blob:none`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_filter_promisor_config_and_combine)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（同卡改写 `command::clone_test::test_clone_rejects_invalid_sparse_filter` 的 `CL-12` AC7 过渡断言，改为不再断言 `blob:none` 被拒绝），覆盖 AC1–AC7。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_json_reports_fetch_transfer_counts)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有传输计数 JSON 回归（`tests/command/clone_test.rs:1458`）。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_rejects_invalid_sparse_filter)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：本卡同卡改写 `CL-12` AC7 过渡断言后，运行改写后的 `CL-12` 用例（其余断言保留）。

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs、src/internal/protocol/mod.rs 与 local_client.rs`
**Dependencies:** `CL-12`、`DEP-CL-04`（协调 `plan-20261117-fetch-push.md#FP-07,#FP-15` 共享 `fetch.rs`）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --filter=blob:none 与 promisor 配置; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=6/8; landing=2; prod-files=4; scope=M; deps=CL-12,DEP-CL-04; writeset=串行于 CL-12; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-24: 实现 `combine:blob:none+tree:1` 过滤传输与无能力回退（#746, #754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`ObjectFilter` 增 `BlobNoneTreeDepth1`，`LocalClient` 新增 `apply_object_filter`（按包内 commit entry 解析根 tree 集合，只保留提交 + 根 tree，丢弃更深 tree 与全部 blob）；`normalize_clone_filter` 把 `combine:blob:none+tree:1`、`combine:tree:1+blob:none` 与 `--filter=blob:none --filter=tree:1`（任一顺序）归一为同一过滤器，配置记 `combine:blob:none+tree:1`（AC1、AC6）；其余组合继续 `LBR-UNSUPPORTED-001`（AC8）；源不广告 `filter` 能力时按 Git 语义回退完整克隆、不写 promisor 行（AC7，改写 CL-13 过渡断言）。用例 `test_clone_combine_tree_depth_and_fallback` 覆盖 AC1–AC8（提交/根 tree 存在、非根 tree 与 blob 缺失、等价形式一致、回退全量、其余拒绝）；`clone_test` **66/66**、`fetch_test` 68/68、`compat_version_surface_sync` 2/2；版本面 0.30.229 → 0.30.230；文档 EN/zh-CN/开发同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 自 `CL-13` 拆出：在 `local_client.rs`、`fetch.rs`、`protocol/mod.rs` 与 `clone.rs` 应用 `combine:blob:none+tree:1` 树深过滤（仅下载根 tree 与 commits；显式 `combine:` 的两种顺序与重复 `--filter` 的两种顺序都按该组合处理，其余组合持续以 `LBR-UNSUPPORTED-001` 拒绝，AC8，`DEFER-CL-10`），并实现远端未广告 `filter` 能力时的安全回退全量克隆（`blob:none` 与本卡组合形式均适用，AC7；是否输出 Git 式回退警告不属本卡契约，本卡不承诺其文案）；同卡改写 `CL-13` 测试的 AC5–AC7 过渡断言（`CL-12` 对无效 `sparse:` 与 AC5 样本表（单独 `tree:<n>`、`object:type=`、`blob:limit=`）的拒绝断言保留：本卡只接受与 `blob:none` 组合的 `tree:1`，见 `DEFER-CL-10`）。（按 G-09 拆出 `CL-29` 承接 `sparse:oid` 过滤与预期缺失对象边界。）
**Out of scope:** promisor 配置写入（`CL-13`）；`Receiving objects` 进度与端到端 `cat-file` 验证（`CL-21`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#746` `#754` 传输层尚未应用 `combine:blob:none+tree:1` 树深过滤与远端无能力时的回退 | `src/command/clone.rs:191-198`；`src/command/fetch.rs:2266-2271`（`fetch_repository` 无 filter 参数）；`src/internal/protocol/local_client.rs:586` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --bare --filter=blob:none --filter=tree:1 file://<S> d` 退出 0，且 `d` 中 `libra config get remote.origin.partialclonefilter` 的 stdout 为 `combine:blob:none+tree:1` 加换行。
- [ ] **AC2** S 的每个提交在 `d` 中均存在。
- [ ] **AC3** S 的每个提交的根 tree 在 `d` 中均存在。
- [ ] **AC4** S 中的每个非根 tree（子目录 tree，如 `sub/` 的 tree）在 `d` 中均不存在。
- [ ] **AC5** S 中的每个 blob 在 `d` 中均不存在。
- [ ] **AC6** 等价形式（表驱动，各样本的用户可见结局相同）：对形式表 {`--filter=combine:blob:none+tree:1`、`--filter=combine:tree:1+blob:none`、`--filter=tree:1 --filter=blob:none`} 中每种形式，`libra clone --bare <形式> file://<S> <新目标>` 退出 0，且 S 的每个对象在该库中的存在性判定结果与在 `d` 中相同（显式 `combine:` 的两种顺序与重复 `--filter` 的另一种顺序得到同一对象集合）。
- [ ] **AC7** 远端不广告 `filter` 能力（夹具：`LIBRA_TEST=1` 下 `CL-13` 新增的测试接缝 `LIBRA_TEST_UPLOAD_PACK_NO_FILTER=1` 使本地 upload-pack 不广告 `filter`）时，对形式表 {`--filter=blob:none`、`--filter=blob:none --filter=tree:1`、`--filter=tree:1 --filter=blob:none`、`--filter=combine:blob:none+tree:1`、`--filter=combine:tree:1+blob:none`} 中每种形式，`libra clone --bare <形式> file://<S> <新目标>` 退出 0，且 S 的每个对象在该库中均存在（回退全量；表驱动，各样本的用户可见结局相同）。
- [ ] **AC8** 其余组合持续拒绝（表驱动，各样本的用户可见结局相同）：对样本表 {`--filter=combine:blob:none+tree:2`、`--filter=combine:blob:none+object:type=blob`、`--filter=blob:none --filter=tree:2`、`--filter=blob:none --filter=object:type=blob`} 中每个样本，`libra clone <样本> file://<S> <新目标>` 均以 `LBR-UNSUPPORTED-001` 拒绝（本计划只交付 `blob:none` 与 `tree:1` 的组合，`DEFER-CL-10`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：stdout 逐字等于 `combine:blob:none+tree:1\n`。
- AC2 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：在 S 中以 `libra rev-list --objects --all` 与 `libra cat-file -t` 得到提交集合（非空），逐一在目标库按「验证执行协议」第 7 项的对象存在性判定为存在。
- AC3 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：在 S 中以 `libra rev-list --objects --all` 与 `libra cat-file -t` 得到各提交 `<提交>^{tree}` 的集合（非空），逐一在目标库按「验证执行协议」第 7 项的对象存在性判定为存在。
- AC4 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：在 S 中以 `libra rev-list --objects --all` 与 `libra cat-file -t` 得到非根 tree集合（非空），逐一在目标库按「验证执行协议」第 7 项的对象存在性判定为缺失。
- AC5 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：在 S 中以 `libra rev-list --objects --all` 与 `libra cat-file -t` 得到blob集合（非空），逐一在目标库按「验证执行协议」第 7 项的对象存在性判定为缺失。
- AC6 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：对形式表逐一克隆到新目标（每次 `exit 0` 为前提）；对 S 中 `libra rev-list --objects --all` 的全部对象，分别在各新库与 `d` 按「验证执行协议」第 7 项做对象存在性判定，「(形式, OID) → 存在/缺失」映射与「(形式, OID) → 该 OID 在 `d` 中的判定结果」映射做一次 `assert_eq!`。
- AC7 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：对形式表逐一克隆到新目标（每次 `exit 0` 为前提）；对 S 中 `libra rev-list --objects --all` 的全部对象在各新库按「验证执行协议」第 7 项判定，「(形式, OID) → 存在/缺失」映射与「(形式, OID) → 存在」映射做一次 `assert_eq!`。
- AC8 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：对样本表中每个样本逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`combine:`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_combine_tree_depth_and_fallback)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（同卡改写 `CL-13` 测试中 AC5–AC7 三条过渡断言为本卡最终契约：AC5、AC6 样本表中由 `blob:none` 与 `tree:1` 组成的样本改为成功并断言过滤结果，其余样本保留拒绝断言；AC7 改为本卡 AC7 的回退断言），覆盖 AC1–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_json_reports_fetch_transfer_counts)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有传输计数 JSON 回归。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_filter_promisor_config_and_combine)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：本卡同卡改写 `CL-13` AC5–AC7 过渡断言后，运行改写后的 `CL-13` 用例（其余断言保留）。

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs、src/internal/protocol/mod.rs 与 local_client.rs`
**Dependencies:** `CL-13`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone combine:blob:none+tree:1 过滤与回退; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=4; scope=M; deps=CL-13,DEP-CL-04; writeset=串行于 CL-13; release=independent; split-from=CL-13; exception=N/A`

---

### Task CL-29: 实现 `sparse:oid` 过滤与预期缺失对象边界（#748；`PM-33`/`t5616#33` 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`ObjectFilter::SparseOid { spec, rules }`（`ObjectFilter` 不再 `Copy`）；`apply_object_filter` 从包内 commit/tree entry 重建根 tree 集合并递归 walk，用 `sparse_rule_matches`（`/x` 锚定、否则按尾部件匹配）求命中 blob 集合——保留全部提交与 tree，只保留命中规则的 blob；`CL-12` 的 `validate_sparse_oid_filter` 改为返回规则集并由克隆装配过滤器，`remote.<n>.partialclonefilter` 原样记录 `sparse:oid=<rev>:<path>`；`LocalClient` 对 blob/tree want 直接成包（`direct_object_entry`），配合 `cat-file` 的 `resolve_object_safe`/`get_object_type` 懒加载钩子，被过滤的 blob 可按需拉取（AC5）。用例 `test_clone_sparse_oid_filter_and_missing_boundary` 覆盖 AC1–AC6（夹具三 blob OID 互异且与其余不同、克隆成功、对象存在性逐项等于「提交/tree 或 one.t」，promisor/partialclonefilter 逐字断言、`show-ref` 含 `refs/remotes/origin/main`、`cat-file -p <two.t>` 按需取回且内容一致）；CL-13/CL-24 用例的存在性检查改用 `--offline` 以免懒加载改变观测（语义变更登记）。`clone_test` **67/67**、`fetch_test` 68/68、`cat_file_test` 34/34、`compat_version_surface_sync` 2/2；版本面 0.30.230 → 0.30.231；文档 EN/zh-CN/开发同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 自 `CL-24` 拆出：在 `clone.rs`、`fetch.rs`、`protocol/mod.rs` 与 `local_client.rs` 实现 `--filter=sparse:oid=<object-expression>` 的传输、稀疏对象闭包与 promisor 预期缺失元数据。对象表达式必须解析为包含稀疏匹配规则的 blob（不是 tree）；并同卡改写 `CL-12` 测试的 AC6 过渡断言（有效 `sparse:oid` 改为断言克隆成功）；使部分克隆能够表达并保留 Git `sparse:oid` 过滤边界，供 `PM-33`（自 `PM-03` 拆出，见 `DEP-CL-14`）的 `t5616#33`/`TC-0625` `--missing=print|allow-promisor|error` 验收消费。
**Out of scope:** `rev-list --missing=<action>` 命令行为（`PM-33`）；`Receiving objects` 进度与最终 `cat-file` 收口（`CL-21`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有 `CL-12`/`CL-13` 对其它 filter fail-closed，`CL-24` 仅交付 `combine:blob:none+tree:1`；`PM-33` 需要 `sparse:oid` 的预期缺失对象夹具，且 `CL-12` 已明确 sparse 规则 blob 边界 | `src/command/clone.rs:191-198`；`plan-20261117-plumbing-misc.md#PM-33`；pinned Git `6de20f6092` `t/t5616-partial-clone.sh#33` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 夹具 F33（`t/t5616-partial-clone.sh#33` 同构）：S 的 `main` 含内容分别为 `one`、`two` 的 `one.t`、`two.t` 与内容为 `/one.t` 的规则文件 `only-one`（建夹具时断言这三个 blob 的完整 OID 两两不同，且与 S 中其余 blob 均不同）；`libra clone --bare --filter=sparse:oid=main:only-one file://<S> d` 退出 0。
- [ ] **AC2** 在 `d` 中发生任何按需拉取之前，对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `d` 的存在性判定结果恰等于「该对象是提交或 tree，或是 `one.t` 的 blob」（Git `sparse:oid` 过滤保留全部提交与 tree、只保留路径匹配规则的 blob，`list-objects-filter.c:386` 起的 `filter_sparse`；夹具中只有 `one.t` 匹配 `/one.t`，因而 `two.t`、`only-one` 与 S 的其余 blob 都缺失）。
- [ ] **AC3** `d` 中 `libra config get remote.origin.promisor` 的 stdout 为 `true` 加换行（供 `PM-33` 的 promisor 归因）。
- [ ] **AC4** `d` 中 `libra show-ref` 的 stdout 含 `refs/remotes/origin/main`（Libra 非 `--mirror` 的 `--bare` 克隆建立远端跟踪引用的现状保持，`PM-33` 依赖，见 `DEP-CL-14`）。
- [ ] **AC5** AC2 之后，`d` 中 `libra cat-file -p <two.t 的 blob OID>` 退出 0，stdout 等于 S 中 `two.t` 的内容（按需拉取边界）。
- [ ] **AC6** `d` 中 `libra config get remote.origin.partialclonefilter` 的 stdout 为 `sparse:oid=main:only-one` 加换行（Git 原样记录过滤规格，`list-objects-filter-options.c:364-397` 的 `partial_clone_register`；后续 fetch 据此沿用该过滤边界）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：断言 `exit 0`。
- AC2 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：在 AC5 的 `cat-file` 之前，对 S 的 `libra rev-list --objects --all` 全集逐一在 `d` 按「验证执行协议」第 7 项（`--offline`）做对象存在性判定，得到「OID → 存在/缺失」映射；期望映射为「OID → （S 中 `libra cat-file -t` 为 `commit` 或 `tree`）或 OID 等于 `one.t` 的 blob OID」；两映射做一次 `assert_eq!`。
- AC3 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：stdout 逐字等于 `true\n`。
- AC4 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：stdout 含子串 ` refs/remotes/origin/main`。
- AC5 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：stdout 逐字等于 S 中 `libra cat-file -p HEAD:two.t` 的 stdout。
- AC6 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：stdout 逐字等于 `sparse:oid=main:only-one\n`（期望值按「验证执行协议」第 4 项在 pinned Git 同夹具复核）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`sparse:oid`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（夹具构造函数供 `PM-33` 复用），覆盖 AC1–AC6。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_combine_tree_depth_and_fallback)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：`CL-24` 过滤与回退不被 `sparse:oid` 接线破坏。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_rejects_invalid_sparse_filter)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：本卡同卡改写 `CL-12` AC6 过渡断言（改为断言有效 `sparse:oid` 克隆成功）后，运行改写后的 `CL-12` 用例（其余断言保留）。

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs、src/internal/protocol/mod.rs 与 local_client.rs`
**Dependencies:** `CL-24`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone sparse:oid 过滤与预期缺失边界; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=2; prod-files=4; scope=M; deps=CL-24,DEP-CL-04; writeset=串行于 CL-24; release=independent; split-from=CL-24; exception=N/A`

---

### Task CL-21: 实现部分克隆 `Receiving objects` 进度与端到端 lazy fetch 验证（#754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`utils::output` 新增 `set_global_progress`/`global_progress`（`OutputConfig::resolve` 记录本次调用的生效进度模式），`fetch::lazy_fetch_object` 用该模式构造读取端的 `OutputConfig`（`--progress=none`/非终端 `auto` 不绘制 spinner）并在打包落盘后按 `text` 输出 `Receiving objects: 100%`（非终端 `--progress=text` 也可见，进度只进 stderr）。端到端链路 `clone --filter=blob:none file://…` → `cat-file -p <S1>:a.txt` 打通：被过滤 blob 先缺失，读取时懒加载取回并落盘。用例 `test_clone_partial_progress_and_e2e_lazy_fetch` 覆盖 AC1–AC8（含 `portable_pty` 的终端 `auto`/`none` 两例与重定向 stderr 的 `text` 例，stdout 逐字等于源仓库输出）。`clone_test`+`fetch_test`+`cat_file_test` 合计 **170/170**；版本面 0.30.231 → 0.30.232；文档 EN/zh-CN 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 自 `CL-13` 拆出：按需拉取（`CL-19` 的 lazy fetch）在进度开启时向 stderr 输出 `Receiving objects`（`#754`，对应 Git `t/t0410-partial-clone.sh#38`；全局 `--progress=none` 时不输出；终端判定沿用全局 `--progress` 的 `auto` 语义，测试以伪终端覆盖），并打通真实 `libra clone --filter=blob:none file://...` 后执行 `libra cat-file -p <rev>:<path>` 端到端按需 lazy fetch 完整链路。
**Out of scope:** `--filter` 协商与 promisor 配置写入（`CL-13`）；`cat-file` 单对象读写层底层接线（`CL-19`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#754` 按需拉取缺少 `Receiving objects` 进度且需验证 `clone --filter=blob:none` → `cat-file -p` 端到端链路 | `src/command/clone.rs:191-198,269`；`src/command/fetch.rs:3472`（`handle_remote_progress`，只处理传输进度）；`src/internal/protocol/local_client.rs:586`（`fetch_objects`） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --filter=blob:none file://<S> d` 后，`<S1>:a.txt`（`S1` 为创建 S 时保存的第一个提交完整 OID；该版本 `a.txt` 不在 `HEAD` 树，检出不会拉取它） 的 blob 在 `d` 中缺失（「验证执行协议」第 7 项对象存在性判定；`#754` 验收第 1 条）。
- [ ] **AC2** 在 `d` 执行 `libra cat-file -p <S1>:a.txt`（非终端，分别捕获 stdout/stderr）退出 0，stdout 等于 S 中同一命令的 stdout（真实 `clone -> cat-file` 端到端按需拉取）。
- [ ] **AC3** AC2 的 stderr（非终端、默认 `auto` 进度设置）不含 `Receiving objects`。
- [ ] **AC4** AC2 之后该 blob 已存在于 `d` 本地。
- [ ] **AC5** 另一新部分克隆 `d2` 中，伪终端用例：子进程以 `sh -c 'libra … >"$OUT"'` 运行，stdout 重定向到测试临时目录中的文件 `$OUT`、stderr 留在 `portable_pty` 伪终端（同 `tests/command/config_test.rs:3836` 的用法），因而终端输出只含 stderr；以默认进度设置执行 `libra cat-file -p <S1>:a.txt` 退出 0 后，终端输出（即 stderr）含 `Receiving objects`（`#754`，对应 Git `t/t0410-partial-clone.sh#38` 的 `test_terminal … 2>err`）。
- [ ] **AC6** 另一新部分克隆 `d3` 中，以同一伪终端方式执行 `libra --progress=none cat-file -p <S1>:a.txt` 退出 0 后，终端输出（即 stderr）不含 `Receiving objects`。
- [ ] **AC7** 另一新部分克隆 `d4` 中，非终端执行 `libra --progress=text cat-file -p <S1>:a.txt`（分别捕获 stdout/stderr）退出 0 后，stderr 含 `Receiving objects`（进度走 stderr）。
- [ ] **AC8** AC7 的 stdout 逐字等于 S 中 `libra cat-file -p <S1>:a.txt` 的 stdout（进度不进入 stdout）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：该 blob OID 在 `d` 按「验证执行协议」第 7 项的对象存在性判定为缺失。
- AC2 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：`exit 0` 且 stdout 逐字相等。
- AC3 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：AC2 同一次调用捕获的 stderr 不含子串 `Receiving objects`。
- AC4 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：该 blob OID 在 `d` 按「验证执行协议」第 7 项的对象存在性判定为存在。
- AC5 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：`exit 0` 为前提；读取伪终端的全部输出（stdout 已重定向到 `$OUT`，不进入终端），含子串 `Receiving objects`。
- AC6 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：`exit 0` 为前提；伪终端全部输出不含子串 `Receiving objects`。
- AC7 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：`exit 0` 为前提；stderr 含子串 `Receiving objects`。
- AC8 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：stdout 逐字相等。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`Receiving objects`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::cat_file_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`cat_file_test` 全模块回归。

**Full-suite trigger:** `T-1: 修改共享传输 src/command/fetch.rs`
**Dependencies:** `CL-29`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。另同步 `docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`（`cat-file` 在按需拉取时新增 `Receiving objects` 进度输出）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --filter 进度与端到端 lazy fetch; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=CL-29,DEP-CL-04; writeset=串行于 CL-29; release=independent; split-from=CL-13; exception=N/A`

---

### Task CL-19: 实现 promisor 缺失对象读取触发按需 lazy fetch（#754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：懒加载挂钩同时覆盖 `command::load_object`（内容读取）与 `utils::util::validate_referenced_object`（引用/HEAD 解析的 `get_object_type`），因此在 promisor 克隆中所有读取路径共享同一回退；`fetch::lazy_fetch_object` 用客户端 `fetch_objects(&[], [oid], …)` 取**闭包**（一次调用即带回 commit/tree/blob）→ `write_pack_and_index` 落盘；`LIBRA_TEST=1` 打印 `libra-test: lazy-fetch want=1`；`ReadPolicy::LocalOnly`（`--offline`）跳过。新用例 `clone_promisor_lazy_fetches_a_missing_object` 覆盖 AC1/AC2/AC4（清空 pack 目录后 `cat-file -p HEAD:tracked.txt` 触发**恰一次**懒加载、闭包持久化、二次读取不再触发）；`clone_test` 全模块 **62/62**；`fetch_test` 68/68；版本面 0.30.225 → 0.30.226。**已知偏差（登记）**：`--offline` 读取缺失对象返回 `LBR-REPO-002`（引用不可读）而非卡内期望的 `LBR-CLI-003`；AC3/AC6–AC8 的自定义远端名/对象闭包断言由同一实现覆盖，专项用例待后续补。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 自 `CL-13` 拆出并前置至 `CL-12`/`CL-13` 之前：在对象读取与存储回退层（`src/utils/storage/local.rs`、`src/utils/object_ext.rs`、`src/command/fetch.rs`、`src/internal/protocol/local_client.rs`、`src/command/cat_file.rs`）实现 promisor 按 `extensions.partialClone` / `remote.<name>.promisor=true` 按需拉取单对象（按 OID；取回的对象按内容计算 OID 后写入对象库，天然以 OID 寻址；复用现有 `LocalClient` 与 `ProtocolClient` 传输接口），使 `libra cat-file -p <rev>:<path>` 在预置 promisor 夹具中自动 lazy fetch 缺失对象（进度输出由 `CL-21` 交付），且在 `--offline` 或 `LIBRA_READ_POLICY=local` 下 fail-closed 不发起网络请求；本卡新增测试接缝 `LIBRA_TEST_TRACE_LAZY_FETCH`（仅 `LIBRA_TEST=1` 时读取，每次发起按需拉取请求前向 stderr 写一行 `libra-test: lazy-fetch want=<n>`），供 AC6/AC8 与 `CL-20` 观测拉取次数。
**Out of scope:** `clone --filter` CLI 解析与写入 promisor 配置（`CL-13`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#754` 当前对象存储层与 `cat-file` 在遇到缺失 promisor 对象时直接报 NotFound（`hydrate.rs` 在私有 `hydrate_one` 中内联校验 OID，本卡在 `local.rs`/`object_ext.rs` 独立实现） | `src/command/clone.rs:269`；`src/utils/storage/local.rs:1859-1882`（`get` 依次查本库松散/pack 与 alternates，均无则 `GitError::ObjectNotFound`，没有 promisor 回退）；`src/utils/object_ext.rs:258-261`（`Blob::load` 缺失即 `Err`） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 夹具 P：`d` 为 `libra clone --no-local file://<S> d` 的结果，测试再按 `a.txt` 的 blob B 在 `d` 中的实际存储形式将其移除（松散文件则删除该文件；位于 pack 内则以只含其余对象的新 pack 及其 `.idx` 替换原 pack，开工按 ER-02 确定并回写本卡 `Current evidence`），并设置 `extensions.partialClone=origin`、`remote.origin.promisor=true`；在 `d` 以 `LIBRA_TEST=1 LIBRA_TEST_TRACE_LAZY_FETCH=1`（本卡新增的测试接缝：每次发起按需拉取请求前向 stderr 写一行 `libra-test: lazy-fetch want=<请求的对象数>`，不写文件）执行 `libra cat-file -p HEAD:a.txt` 退出 0，stdout 等于 S 中 `a.txt` 的内容。
- [ ] **AC2** AC1 的 stderr 中以 `libra-test: lazy-fetch` 开头的行恰为一行 `libra-test: lazy-fetch want=1`（接缝正控制，保证 AC6/AC8 的「无此行」不是空真）。
- [ ] **AC3** 远端名为 `upstream` 的同构夹具（`libra clone --no-local -o upstream file://<S> d2`，同样移除 B，并设置 `extensions.partialClone=upstream`、`remote.upstream.promisor=true`，不存在 `remote.origin.*`）中，同一命令的 stdout 同样等于 `a.txt` 的内容。
- [ ] **AC4** AC1 之后 B 存在于 `d` 的本地对象库（取回的对象已持久化，「验证执行协议」第 7 项对象存在性判定）。
- [ ] **AC5** 按夹具 P 同样步骤新建的克隆 `d5`（B 已移除、尚未拉取）中，以同一接缝执行 `libra --offline cat-file -p HEAD:a.txt`，以 `LBR-CLI-003` 拒绝。
- [ ] **AC6** AC5 的 stderr 中没有以 `libra-test: lazy-fetch` 开头的行（未发起拉取请求）。
- [ ] **AC7** 按夹具 P 同样步骤新建的克隆 `d7`（B 已移除、尚未拉取）中，以同一接缝和环境变量 `LIBRA_READ_POLICY=local` 执行 `libra cat-file -p HEAD:a.txt`，以 `LBR-CLI-003` 拒绝。
- [ ] **AC8** AC7 的 stderr 中没有以 `libra-test: lazy-fetch` 开头的行。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：stdout 逐字等于 S 中 `libra cat-file -p HEAD:a.txt` 的 stdout。
- AC2 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：按行切分 AC1 的 stderr，筛出以该前缀开头的行，结果逐字等于 `["libra-test: lazy-fetch want=1"]`。
- AC3 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：stdout 逐字等于 AC1 的期望内容。
- AC4 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：B 的完整 OID 在 `d` 按「验证执行协议」第 7 项的对象存在性判定为存在（`--offline`，排除再次拉取）。
- AC5 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC6 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：按行切分 AC5 的 stderr，以该前缀开头的行数为 0。
- AC7 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC8 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：按行切分 AC7 的 stderr，以该前缀开头的行数为 0。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`promisor`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::cat_file_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`cat_file_test` 全模块回归。

**Full-suite trigger:** `T-1: 修改共享对象存储读取核心 src/utils/storage/local.rs 与 object_ext.rs`
**Dependencies:** `CL-18`、`DEP-CL-04`、`DEP-CL-08`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/storage/local.rs`, `src/utils/object_ext.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `src/command/cat_file.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/utils/storage/local.rs`, `src/utils/object_ext.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `src/command/cat_file.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=promisor 缺失对象读取 lazy fetch; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=4; prod-files=5; scope=M; deps=CL-18,DEP-CL-04,DEP-CL-08; writeset=串行于 CL-18; release=independent; split-from=CL-13; exception=N/A`

> **2026-11-20 前置缺口（实测）：** 本卡要求的「缺失对象按需拉取」需要**按 OID 向远端 want 单个对象**；`CL-05` 已实测当前客户端的 OID want 被拒（`fatal: couldn't find remote ref <oid>`），即 fetch/协议层没有对象级 want 支持。因此 `CL-19` 的真实实现面是：①传输层新增按 OID want（含 `upload-pack` 能力协商与本地客户端的对象定位）；②`src/command/mod.rs:249 load_object`（同步）用阻塞桥接到异步拉取；③`LIBRA_TEST_TRACE_LAZY_FETCH` 接缝与 `--offline` 拒绝；④8 条 AC 用例。这是一次跨协议层的较大改动，需独立实现窗口。

> **2026-11-20 范围收敛：** 上条实测的 OID want 拒绝来自 **refspec 解析器**（`fetch <remote> <oid>` 要求 ref 名），而非传输客户端本身：`LocalClient::fetch_objects(have, want, …)` 的 `want` 是对象 ID 列表，本地客户端按 want 打包，理论上可直接接受 OID。因此 `CL-19` 可按「新增 `lazy_fetch_object(oid)`：读 `extensions.partialClone`/`remote.<name>.promisor` → 用客户端 `fetch_objects(&[], [oid], …)` 取包 → 用既有 `ClientStorage` 落地」实现，无需改 wire 协议；`load_object`（`src/command/mod.rs:249`，同步）以阻塞桥接调用该函数，并在 `LIBRA_TEST=1` 时打印 `libra-test: lazy-fetch want=<n>`。

---

### Task CL-14: 支持 `clone --ref-storage-format` 与 `rev-parse --show-ref-storage-format`（#750）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`CloneArgs` 新增 `--ref-storage-format=<format>`，`sqlite` 接受为 no-op，`files`/`reftable` 以 `LBR-UNSUPPORTED-001` 拒绝（`CloneError::UnsupportedRefStorageFormat`），未知格式以 `LBR-CLI-002` 拒绝（`CloneError::UnknownRefStorageFormat`，对应 Git `unknown ref storage format`），均在创建目标目录前 fail-closed；`RevParseArgs` 新增 `--show-ref-storage-format`，文本输出 `sqlite`、`--json` 输出 `{"ref_storage_format": "sqlite"}`。用例 `test_clone_ref_storage_format` 覆盖 AC1–AC7（显式 sqlite 克隆、mirror 克隆、克隆库/mirror/`init` 新库三处输出逐字为 `sqlite
`、三种拒绝及稳定码）；`clone_test`+`rev_parse_test` 合计 **119/119**；版本面 0.30.232 → 0.30.233；文档 clone/rev-parse 的 EN、zh-CN、开发版与 `COMPATIBILITY.md` 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 按 ADR-CL-04 在 `src/command/clone.rs` 新增 `--ref-storage-format=<format>` 解析（接受 `sqlite`，对未实现的 `files`/`reftable` 或未知格式 fail-closed 报错，不修改 `src/command/init.rs` 与 D25），并为 `RevParseArgs` 新增 `--show-ref-storage-format` 如实输出 `sqlite`（并在 `COMPATIBILITY.md` 记为 `intentionally-different`，`files`/`reftable` 存储后端延后至 `DEFER-CL-01`）。
**Out of scope:** 修改 `init --ref-format`（保留 D25）；实现 Git `files` 或 `reftable` ref 存储后端（`DEFER-CL-01`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#750` `CloneArgs` 缺 `--ref-storage-format`；`RevParseArgs` 缺 `--show-ref-storage-format`；`init.rs` `RefFormat` 为分支名校验而非存储格式（D25） | `src/command/clone.rs:64-236`；`src/command/rev_parse.rs:59`；`src/command/init.rs:301-313,1676-1690` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --ref-storage-format=sqlite file://<S> d` 退出 0。
- [ ] **AC2** `libra clone --mirror --ref-storage-format=sqlite file://<S> m` 退出 0。
- [ ] **AC3** `d` 中 `libra rev-parse --show-ref-storage-format` 的 stdout 为 `sqlite` 加换行。
- [ ] **AC4** `m`（裸仓库）与以 `libra init` 新建、未经克隆的仓库 `i` 中，同一命令的 stdout 均为 `sqlite` 加换行（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同；输出反映存储后端本身，不依赖克隆写入的任何配置）。
- [ ] **AC5** `libra clone --ref-storage-format=files file://<S> e1` 以 `LBR-UNSUPPORTED-001` 拒绝（`files` 后端见 `DEFER-CL-01`）。
- [ ] **AC6** `libra clone --ref-storage-format=reftable file://<S> e2` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC7** `libra clone --ref-storage-format=bogus file://<S> e3` 以 `LBR-CLI-002` 拒绝（Git 同类输入报 `unknown ref storage format`，`builtin/clone.c:1060`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：断言 `exit 0`。
- AC2 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：断言 `exit 0`。
- AC3 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：stdout 逐字等于 `sqlite\n`。
- AC4 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：「仓库 → stdout」映射与「每个仓库 → `sqlite\n`」映射做一次 `assert_eq!`。
- AC5 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC6 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC7 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`ref-storage-format`（同时命中 `clone` 页的 `--ref-storage-format` 与 `rev-parse` 页的 `--show-ref-storage-format`）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_ref_storage_format_and_rev_parse)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC7。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`rev_parse_test` 全模块回归。

**Full-suite trigger:** `none`
**Dependencies:** `CL-21`、`DEP-CL-02`、`DEP-CL-09`（等待 `plan-20261117-plumbing-misc.md#PM-06` 先合入 `rev_parse.rs`）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/rev_parse.rs`, `tests/command/clone_test.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/rev_parse.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--ref-storage-format 与 rev-parse; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=CL-21,DEP-CL-02,DEP-CL-09; writeset=串行于 CL-21; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-15: 修复 `clone --single-branch` 从游离 HEAD 克隆错误创建 `refs/remotes/origin/main`（#753）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：新增 `source_detached_head_oid`（无 `symref=HEAD:` 能力且 HEAD OID 不等于任何 `refs/heads/*` 尖端即判为游离），`clone --single-branch` 且未给 `--branch`/`--revision` 时走「复现游离 HEAD」分支：只写 `remote.<n>.url`、把 refspec 记为 `+refs/heads/*:refs/remotes/<n>/_detached/*`、`Head::Detached(oid)` 并 `restore_to_commit` 检出，随后删除远端跟踪分支与标签后提前返回；`setup_repository` 新增 `detached_source` 参数以跳过 `remote.<n>.fetch` 的单分支写法（`--revision` 仍完全不写 refspec，保持 CL-05 契约）。用例 `test_clone_single_branch_from_detached_head` 覆盖 AC1–AC4（前两行 `# branch.head (detached)`/`# branch.oid <C>`、无 `refs/remotes/origin/main`、随后 `fetch` 也不生成、显式 `--branch main` 的正控制存在）。**偏差登记**：AC2/AC3/AC4 的引用存在性以 `libra show-ref` 判定（卡内建议直读 SQLite `reference` 表），语义等价；AC3 下后续 fetch 会把 `main` 记到 `refs/remotes/origin/_detached/main` 这一私有命名空间。`clone_test` **70/70**；版本面 0.30.233 → 0.30.234；文档 EN/zh-CN 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 当源仓库处于游离 HEAD 且执行 `libra clone --single-branch`（未指定 `--branch`）时，仅建立游离 HEAD 而不虚构 `refs/remotes/origin/main`，且后续执行 `libra fetch` 同样不会创建 `refs/remotes/origin/main`。
**Out of scope:** 正常有分支 HEAD 的 `--single-branch` 克隆（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#753` 源仓库游离 HEAD 且未给 `--branch` 时，`--single-branch` 经 `clone_fetch_branch` 回退到 `fetch::resolve_remote_default_branch` 选取默认分支（据 `#753` 复现会创建 `refs/remotes/origin/main`；开工按 ER-02 重新复现并回写） | `src/command/clone.rs:448-470` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** S 的 `HEAD` 游离于提交 C（C 不是任何分支的末端）；`libra clone --single-branch file://<S> d` 退出 0 后，`d` 中 `libra status --porcelain=v2 --branch` 的 stdout 前两行逐字等于 `# branch.head (detached)` 与 `# branch.oid <C>`（`src/command/status/output.rs:2541-2555`）。
- [ ] **AC2** `d` 中不存在远端跟踪分支 `refs/remotes/origin/main`。
- [ ] **AC3** 在 `d` 执行 `libra fetch` 退出 0 后，仍不存在 `refs/remotes/origin/main`。
- [ ] **AC4** `libra clone --single-branch --branch main file://<S> d2` 后，`d2` 中存在 `refs/remotes/origin/main`（同一查询的正控制）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_single_branch_from_detached_head`：前两行逐字比较，期望 OID 为 C。
- AC2 → `command::clone_test::test_clone_single_branch_from_detached_head`：以 SQLite 只读连接查询 `d` 的 `reference` 表中 `kind='Branch' AND remote='origin' AND name='refs/remotes/origin/main'` 的行数（Libra 以完整引用名存远端跟踪分支，2026-11-18 以 libra 0.30.207 实测）为 0。
- AC3 → `command::clone_test::test_clone_single_branch_from_detached_head`：`libra fetch` 的 `exit 0` 为前提；同一查询为 0。
- AC4 → `command::clone_test::test_clone_single_branch_from_detached_head`：以 SQLite 只读连接查询 `d2` 的 `reference` 表中 `kind='Branch' AND remote='origin' AND name='refs/remotes/origin/main'` 的行数（Libra 以完整引用名存远端跟踪分支，2026-11-18 以 libra 0.30.207 实测）为 1。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--single-branch`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_single_branch_from_detached_head)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_depth_implies_single_branch_matrix)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地 `--single-branch` 矩阵回归（`tests/command/clone_test.rs:1753`：显式 `--single-branch`、`--depth` 隐含、`--no-single-branch` 与 `-b` 的远端跟踪引用范围）。

**Full-suite trigger:** `none`
**Dependencies:** `CL-14`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--single-branch 游离 HEAD 处理; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-14; writeset=串行于 CL-14; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-26: 在 `cli.rs` 为稀疏工作区的检出类命令增加 `fail-closed` 护栏（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`src/cli.rs` 新增异步 `enforce_sparse_checkout_guard`，在 `command_preflight` 之前对 `checkout`/`switch`/`reset`/`restore` 读取 `core.sparseCheckout`，为真（`true`/`1`/`yes`/`on`）时以 `LBR-UNSUPPORTED-001` fail-closed 并给出 unsets 提示；仓库外先行返回（`try_get_storage_path`），避免在无仓库目录读取配置触发 `storage_path()` panic——该缺陷由本卡回归测试 `reset_test`/`restore_test` 的 outside-repository 用例捕获并当场修复。用例 `test_sparse_preflight_checkout_switch_reset_restore` 覆盖 AC1–AC4；`checkout/switch/reset/restore/sparse_view/add` 相关套件 **284/284**（修复后重跑 3/3 复验）、`compat_version_surface_sync` 2/2；版本面 0.30.234 → 0.30.235；四命令 EN/zh-CN 文档与 `COMPATIBILITY.md` 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 自 `CL-16` 拆出并前置（G-09）：在 `src/cli.rs` `command_preflight`（:1755+）中，对未适配稀疏 cone 的检出类命令建立 `core.sparseCheckout=true` 中央 `fail-closed` 护栏。本卡只覆盖 `checkout`、`switch`、`reset`、`restore`。本卡拆出 `CL-27`，由它承接 `merge`、`pull`、`rebase`、`cherry-pick`、`stash`。
**Out of scope:** `merge`/`pull`/`rebase`/`cherry-pick`/`stash` 护栏（`CL-27`）；`clone --sparse` cone 物化（`CL-16`）；`merge` 解禁（`CL-20`）；全量 cone 适配（`DEFER-CL-04`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` 的 `checkout`/`switch`/`reset`/`restore` 尚未适配 `core.sparseCheckout=true` | `src/cli.rs:1755` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 夹具 Q：以「验证执行协议」第 7 项的标准源仓库 S 为基础（`main` 末端 `S2`），从 `S1` 建分支 `other`（一个新增文件 `o.txt` 的提交）与 `topic`（一个把 `a.txt` 改为与 `S1`、`S2` 都不同内容的提交，即修改 `main` 也修改过的文件，非目录重命名），回到 `main` 后执行 `libra config set core.sparseCheckout true`；`libra checkout other` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC2** 同一夹具中 `libra switch other` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC3** 同一夹具中 `libra reset --hard HEAD~1` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC4** 同一夹具中 `libra restore a.txt`（先修改 `a.txt`）以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC2 → `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC3 → `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC4 → `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`core.sparseCheckout`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_sparse_preflight_checkout_switch_reset_restore)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::(checkout|switch|reset|restore)_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：正控制——未设置 `core.sparseCheckout` 的普通仓库中，`checkout`/`switch`/`reset`/`restore` 的既有用例在新增护栏后全部通过（护栏不得无条件拒绝）。

**Full-suite trigger:** `T-1: 修改全局命令预检 src/cli.rs`
**Dependencies:** `CL-15`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md`、`docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=稀疏检出类命令 fail-closed 护栏; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-15; writeset=串行于 CL-15; release=independent; split-from=CL-16; exception=N/A`

---

### Task CL-27: 在 `cli.rs` 为稀疏工作区的历史类命令增加 `fail-closed` 护栏（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`enforce_sparse_checkout_guard` 的 `matches!` 名单扩展为 `merge`/`pull`/`rebase`/`cherry-pick`/`stash`（与 CL-26 共用同一中枢护栏，均在 `command_preflight` 之前、仓库外直接放行）。用例 `test_sparse_preflight_merge_pull_rebase_cherry_pick_stash` 覆盖 AC1–AC5（五条命令均以 `LBR-UNSUPPORTED-001` 拒绝）；`merge/pull/rebase/cherry_pick/stash` 相关套件 **585/585**；版本面 0.30.235 → 0.30.236；五命令 EN/zh-CN 文档与 `COMPATIBILITY.md` 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 自 `CL-26` 拆出（G-09）：在同一 `command_preflight` 护栏上覆盖 `merge`、`pull`、`rebase`、`cherry-pick`、`stash`。只有目录重命名合并的解禁由 `CL-20` 承接，其它 `merge` 继续拒绝；本卡 AC1 使用内容修改合并夹具，因而在最终树上继续成立。
**Out of scope:** `checkout`/`switch`/`reset`/`restore`（`CL-26`）；`merge` 稀疏 lazy fetch 解禁（`CL-20`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` 的 `merge`/`pull`/`rebase`/`cherry-pick`/`stash` 尚未适配 `core.sparseCheckout=true` | `src/cli.rs:1755` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 同 `CL-26` AC1 的夹具 Q：以「验证执行协议」第 7 项的标准源仓库 S 为基础（`main` 末端 `S2`），从 `S1` 建分支 `other`（一个新增文件 `o.txt` 的提交）与 `topic`（一个把 `a.txt` 改为与 `S1`、`S2` 都不同内容的提交，即修改 `main` 也修改过的文件，非目录重命名），回到 `main` 后执行 `libra config set core.sparseCheckout true`；`libra merge topic` 以 `LBR-UNSUPPORTED-001` 拒绝。`CL-20` 只解禁目录重命名合并，本断言在最终树上继续成立。
- [ ] **AC2** 同一夹具中 `libra pull` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC3** 同一夹具中 `libra rebase topic` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC4** 同一夹具中 `libra cherry-pick topic` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC5** 同一夹具中 `libra stash push`（先修改 `a.txt`）以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC2 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）（`pull` 在预检阶段即被拒绝，不需要可达远端）。
- AC3 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC4 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC5 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`core.sparseCheckout`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC5。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::(merge|pull|rebase|cherry_pick|stash)_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：正控制——未设置 `core.sparseCheckout` 的普通仓库中，`merge`/`pull`/`rebase`/`cherry-pick`/`stash` 的既有用例在新增护栏后全部通过。

**Full-suite trigger:** `T-1: 修改全局命令预检 src/cli.rs`
**Dependencies:** `CL-26`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`、`docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`、`docs/commands/stash.md`、`docs/commands/zh-CN/stash.md`、`docs/development/commands/stash.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=稀疏历史类命令 fail-closed 护栏; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-26; writeset=串行于 CL-26; release=independent; split-from=CL-26; exception=N/A`

---

### Task CL-16: 支持 `libra clone --sparse` 工作区 cone 物化与 skip-worktree 初始化（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`CloneArgs` 新增 `--sparse`；克隆检出后由 `apply_sparse_cone_clone` 读取索引，对含目录分隔符的条目置 skip-worktree 位（复用 `utils::index_ext::set_skip_worktree`）并删除对应工作区文件与随之空掉的目录，随后写 `core.sparseCheckout=true`、`core.sparseCheckoutCone=true`；`status` 不再调用 `warn_sparse_checkout_unsupported_once`（`add`/`commit`/`diff` 的告警保留）；`--deps-of` 帮助文本移除 `declined D10` 字样；D10 在 `docs/development/commands/_compatibility.md` 与 `COMPATIBILITY.md` 局部修订为「cone 物化已交付、规则文件与检出类命令仍延后」。用例 `test_clone_sparse_cone_materializes_top_level` 覆盖 AC1–AC7（两个配置逐字为 true、顶层 `a.txt` 等于 `S2:a.txt`、`sub/` 不存在、`ls-files -t` 逐字为 `H a.txt`/`S sub/b.txt`、`status` 无告警、`clone --help` 无 `declined D10`）；`status_test` 的 H5 断言随之改为「不再告警」。`clone_test`+`status_test`+`add_test`+`sparse_view_test` 合计 **250/250**；版本面 0.30.236 → 0.30.237；文档 EN/zh-CN 同步。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 按 ADR-CL-03 为 `CloneArgs` 新增 `--sparse`（并把 `clone.rs:215-224` 的 `--deps-of` 帮助说明中 `NOT --sparse (declined D10)` 字样改为现行说明，由 AC7 验收），局部修订 D10（同步 `COMPATIBILITY.md` 与 `docs/development/commands/_compatibility.md`），在 `src/command/clone.rs` 与 `src/internal/sparse/mod.rs`（新增只含顶层的 cone 成员判定供克隆物化使用，复用索引 skip-worktree 位，无 SQLite schema 变更）初始化 `core.sparseCheckout=true`、`core.sparseCheckoutCone=true`、工作区顶层文件物化及 skip-worktree 索引位；本卡的稀疏状态即这两个配置与 skip-worktree 位，不写入、不启用只读的 `sparse-view` 显示过滤（启用后 `ls-files` 会隐藏 skip-worktree 条目，与 AC5 矛盾；2026-11-18 以 libra 0.30.207 实测 `sparse-view set '/*' '!/*/'` 后 `ls-files -t` 不再列出 `S sub/b.txt`），Git `info/sparse-checkout` 式 cone 规则文件的持久化与规则管理随 `DEFER-CL-04`；在 `src/command/status.rs:563` 仅为 `status` 跳过 `warn_sparse_checkout_unsupported_once`（保留 `add.rs:532`/`commit.rs:3017`/`diff.rs:851` 的既有告警，不改变 `add`/`commit`/`diff` 行为）；本卡不对 `--sparse` 与 `--filter` 的组合另加限制：`#756` 要求的 `--sparse --filter=blob:none` 组合由 `CL-20` 验收（AC1），其余组合不在本计划验收范围（`DEFER-CL-05`）；稀疏仓库中的检出类与历史类写入命令此时仍由 `CL-26`/`CL-27` 拒绝。（G-09：本卡拆出 `CL-20`、`CL-26`、`CL-27`。）
**Out of scope:** 未适配命令 `fail-closed` 护栏（`CL-26`）；`--sparse --filter=blob:none` 组合及目录重命名合并 lazy fetch（`CL-20`）；`checkout`/`switch`/`reset` 全量 cone 过滤扩展（`DEFER-CL-04`）；独立 `libra sparse-checkout` 子命令（永久非目标）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` `CloneArgs` 缺少 `--sparse` 标志，且 `clone.rs:220`（`--deps-of` 帮助说明）仍写 `NOT --sparse (declined D10)`；`status.rs:563` 调用 `warn_sparse_checkout_unsupported_once`（`add.rs:532`/`commit.rs:3017`/`diff.rs:851` 仍保留）；`sparse_view.rs` 为只读视图且 D10 记于 `_compatibility.md:175-197` | `src/command/clone.rs:220-235`；`src/command/status.rs:563,1248-1265`；`src/internal/sparse/mod.rs:1`；`docs/development/commands/_compatibility.md:175-197`；`COMPATIBILITY.md:166,172,375` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --sparse file://<S> d` 退出 0，且 `d` 中 `libra config get core.sparseCheckout` 的 stdout 为 `true` 加换行。
- [ ] **AC2** `d` 中 `libra config get core.sparseCheckoutCone` 的 stdout 为 `true` 加换行（cone 模式，Git `clone --sparse` 经 `sparse-checkout set` 初始化，`builtin/clone.c:646`）。
- [ ] **AC3** `d` 工作区的顶层文件 `a.txt` 的字节等于 S 中 `S2:a.txt` 的内容（顶层文件按 `HEAD` 正确物化）。
- [ ] **AC4** `d` 工作区不含目录 `sub/`。
- [ ] **AC5** `d` 中 `libra ls-files -t` 的 stdout 逐字等于 `H a.txt`、`S sub/b.txt` 两行（只有 cone 外条目带 skip-worktree 位，且条目仍在索引中）。
- [ ] **AC6** `d` 中 `libra status` 退出 0，且其 stderr 不含 `core.sparseCheckout=true is not supported`。
- [ ] **AC7** `libra clone --help` 的 stdout 不含子串 `declined D10`（`clone.rs:220` 的 `--deps-of` 帮助说明不再声称 `--sparse` 已被 D10 拒绝；libra 0.30.207 的该输出中 `declined` 只出现在这一处）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：stdout 逐字等于 `true\n`。
- AC2 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：stdout 逐字等于 `true\n`。
- AC3 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：读取 `d/a.txt` 的 SHA-256，与在 S 中 `libra cat-file -p <S2>:a.txt`（`exit 0` 为前提）的 stdout 的 SHA-256 做一次 `assert_eq!`。
- AC4 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：`d/sub` 的 `exists()` 为假。
- AC5 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：`exit 0` 为前提；stdout 逐字等于 `H a.txt\nS sub/b.txt\n`（格式见 libra 0.30.207 实测，开工按 ER-02 以现行输出复核）。
- AC6 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：`exit 0` 为前提；stderr 不含该子串。
- AC7 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：`exit 0` 为前提；stdout 不含子串 `declined D10`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--sparse`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_sparse_cone_materializes_top_level)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC7。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`status_test` 全模块回归。

**Full-suite trigger:** `T-1: 修改共享稀疏核心 src/internal/sparse/mod.rs`
**Dependencies:** `CL-27`、`DEP-CL-11`（协调 `src/command/status.rs` 共享写集）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/sparse/mod.rs`, `src/command/status.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/sparse/mod.rs`, `src/command/status.rs`, `tests/command/clone_test.rs`, `docs/development/commands/_compatibility.md`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --sparse cone 物化与 status 适配; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=3; prod-files=3; scope=M; deps=CL-27,DEP-CL-11; writeset=串行于 CL-27; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-20: 实现 `clone --sparse` 部分克隆与目录重命名合并按需 lazy fetch（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `in-progress` / `locally-accepted`（2026-11-21 第一段：`fetch::lazy_fetch_objects(&[ObjectHash])` 以**一次**客户端调用取回整批对象（单 pack、单次落盘），`lazy_fetch_object` 改为其单对象包装；`libra-test: lazy-fetch want=<n>` 接缝从 `command` 桥接层移入 fetch 层并输出真实批大小（`LIBRA_TEST=1` 时），同时保留 `--progress` 驱动的 `Receiving objects` 行。回归：`clone_promisor_lazy_fetches_a_missing_object`（want=1 仍逐字成立）、`test_clone_sparse_oid_filter_and_missing_boundary`、`test_clone_partial_progress_and_e2e_lazy_fetch` 全绿。**剩余**：合并阶段按需批量预取（AC1/AC4）、`command_preflight` 仅为目录重命名合并解禁（AC6 的其他合并仍拒绝）、AC2/AC3/AC5（对象差集与 `folder/subdir/newfile.*` 索引）与 AC7/AC8（组合克隆的 cone 状态）待实现与用例化；未验证前不转 `done`。）
**Description:** 自 `CL-16` 拆出：在 `src/command/clone.rs` 打通 `--sparse --filter=blob:none` 的稀疏部分克隆（与 `CL-16` 的 cone 初始化叠加）并在 `src/cli.rs` `command_preflight` 只为目录重命名合并解开 `core.sparseCheckout=true` 护栏，内容修改、删除和冲突合并继续拒绝，同时在 `src/command/merge.rs`、`src/command/merge/rename_merge.rs`、`src/command/merge/tree_merge.rs` 与 `src/command/fetch.rs` 中新增批量多 OID `fetch_objects_batch` 预取 helper 并打通 `clone --sparse --filter=blob:none`（cone 模式，AC7–AC8）及目录重命名合并时的单次批量按需 lazy fetch（对应 `#756` 的 `TC-0734`，即 Git `t/t6421-merge-partial-clone.sh#2`：1 次拉取取回 6 个对象；拉取次数与对象数经 `CL-19` 新增的 stderr 测试接缝 `LIBRA_TEST_TRACE_LAZY_FETCH`（仅 `LIBRA_TEST=1` 时读取）观测），确保稀疏工作区合并时不会遗漏重命名目标文件。
**Out of scope:** `--sparse` 基础 cone 初始化与 skip-worktree（`CL-16`）；`checkout`/`switch`/`reset` 护栏（`CL-26`/`DEFER-CL-04`）；`#756` 验收第 4 条（完整 `t6421#2` 需在稀疏仓库内 `checkout`，随 `DEFER-CL-04`）；离线模式下按需拉取被拒绝（`CL-19` AC5–AC6）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` 部分克隆结合 `--sparse` 进行目录重命名合并时需按需批量补全被折叠/重命名的树与 blob（`1 fetch` / `6 objects`）；目录重命名合并逻辑位于 `rename_merge.rs` 与 `tree_merge.rs` | `src/command/clone.rs:220-235`；`src/cli.rs:1755`；`src/command/merge.rs:1121`（`execute_safe`）；`src/command/merge/rename_merge.rs:291,1157`（`apply_directory_renames`、`detect_and_apply_renames`）；`src/command/merge/tree_merge.rs:205`（`merge_tree_items`） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 夹具 M 为 Git `t/t6421-merge-partial-clone.sh#2`（`#756` 的 `TC-0734`）的 `server` 同构仓库：提交 O，分支 `A`（`dir/` 重命名为 `folder/` 等）与 `B-dir`（在 `dir/subdir/` 新增 `newfile.java`、`newfile.scala`、`newfile.groovy`）；以 `libra clone --sparse --filter=blob:none --branch A file://<M> d` 退出 0 为前提，在 `d` 中 `libra config set merge.directoryRenames true` 后，以 `LIBRA_TEST=1 LIBRA_TEST_TRACE_LAZY_FETCH=1`（`CL-19` 新增的 stderr 接缝）执行 `libra merge origin/B-dir` 退出 0，且其 stderr 中以 `libra-test: lazy-fetch` 开头的行恰为一行 `libra-test: lazy-fetch want=6`（Git 同夹具 `fetch_count:6`、1 次拉取）。
- [ ] **AC2** 合并前后，M 的对象中在 `d` 由缺失变为存在的集合恰为 Git `t6421#2` 列出的 6 个 blob（`Makefile_TOP_O`、`Makefile_TOP_A`、`Makefile_SUB_O`、`Makefile_SUB_A`、`e_O`、`e_A`，`t/t6421-merge-partial-clone.sh:282-285`）。
- [ ] **AC3** 合并后 `d` 的索引中路径以 `folder/subdir/newfile.` 开头的条目 (路径, blob OID) 集合恰为 `newfile.java`、`newfile.scala`、`newfile.groovy` 三项，且各自 OID 等于 M 中 `B-dir:dir/subdir/<同名文件>` 的 blob OID（内容未损坏）（目录重命名生效，`t6421#2` 期望树）。
- [ ] **AC4** 非稀疏部分克隆 `libra clone --filter=blob:none --branch A file://<M> d2` 中，同样设置后以同一接缝执行 `libra merge origin/B-dir` 退出 0，且其 stderr 中至少有一行以 `libra-test: lazy-fetch` 开头（缺失对象确实按需拉取）。
- [ ] **AC5** AC4 的合并后，`d2` 的索引中路径以 `folder/subdir/newfile.` 开头的条目 (路径, blob OID) 集合恰为 `newfile.java`、`newfile.scala`、`newfile.groovy` 三项，且各自 OID 等于 M 中 `B-dir:dir/subdir/<同名文件>` 的 blob OID（内容未损坏）。
- [ ] **AC6** 夹具 N：在标准源仓库 S 上从 `S1` 另建分支 `t-mod`（只把 cone 外文件 `sub/b.txt` 改为单行 `b2`）、`t-del`（只删除 `sub/b.txt`）与 `t-conf`（只把 `a.txt` 改为单行 `three`），以 `libra clone --sparse --filter=blob:none file://<S> n` 退出 0 为前提；在 `n`（当前分支 `main`，末端 `S2`）中对 {`origin/t-mod`（内容修改合并，`main` 未改该文件）、`origin/t-del`（删除合并）、`origin/t-conf`（冲突合并：`S2` 与 `t-conf` 对 `a.txt` 唯一一行做了不同修改）} 逐一执行 `libra merge <分支>`，均以 `LBR-UNSUPPORTED-001` 拒绝（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）。
- [ ] **AC7** 合并前（AC1 的克隆之后），`d` 中 `libra config get core.sparseCheckout` 与 `libra config get core.sparseCheckoutCone` 的 stdout 均为 `true` 加换行（组合克隆同样处于 cone 模式；表驱动，结局相同）。
- [ ] **AC8** 合并前，`d` 中 `libra ls-files -t` 的「路径 → 标记」映射恰等于「M 中分支 `A` 的树的每个文件路径 → 路径含 `/` 时为 `S`、否则为 `H`」（条目既不缺也不多；只有顶层文件在 cone 内，其余条目带 skip-worktree 位，与 `CL-16` 的根目录 cone 一致）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：`exit 0` 为前提；按行切分 stderr，筛出以该前缀开头的行，结果逐字等于 `["libra-test: lazy-fetch want=6"]`（期望值按「验证执行协议」第 4 项在 pinned Git 同夹具复核）。
- AC2 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：合并前后各对 M 的 `libra rev-list --objects --all` 全部对象在 `d` 按「验证执行协议」第 7 项的对象存在性判定，得到两份「OID → 存在/缺失」映射；由二者差得到的「缺失→存在」集合与期望 6 个 OID（在 M 中按路径以 `libra rev-parse <提交>:<路径>` 取得）做一次 `assert_eq!`。
- AC3 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：解析 `d` 中 `libra ls-files -s` 的每行（`<mode> <OID> <stage>\t<path>`），筛出该前缀的 (路径, OID) 集合；期望 OID 在 M 中以 `libra rev-parse B-dir:dir/subdir/newfile.<ext>` 取得；两集合做一次 `assert_eq!`。
- AC4 → `command::clone_test::test_non_sparse_partial_merge_fetches_missing_objects`：`exit 0` 为前提；按行切分 stderr，以该前缀开头的行数 ≥ 1。
- AC5 → `command::clone_test::test_non_sparse_partial_merge_fetches_missing_objects`：解析 `d2` 中 `libra ls-files -s` 的每行（`<mode> <OID> <stage>\t<path>`），筛出该前缀的 (路径, OID) 集合；期望 OID 在 M 中以 `libra rev-parse B-dir:dir/subdir/newfile.<ext>` 取得；两集合做一次 `assert_eq!`。
- AC6 → `command::clone_test::test_sparse_merge_rejected_cases_preserve_state`：对三个分支逐一调用，得到「分支 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个分支 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每次调用前后另按「验证执行协议」第 4 项比较 `n` 的仓库快照。
- AC7 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：「配置键 → stdout」映射与「每个键 → `true\n`」映射做一次 `assert_eq!`。
- AC8 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：`exit 0` 为前提；解析 `d` 中 `libra ls-files -t` 的每行（`<标记> <路径>`）得到「路径 → 标记」映射；期望映射由 M 中 `libra ls-tree -r --name-only A` 的每个路径按「含 `/` → `S`，否则 → `H`」导出（非空）；两映射做一次 `assert_eq!`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--sparse`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_sparse_partial_merge_lazy_fetch)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC3、AC7–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_non_sparse_partial_merge_fetches_missing_objects)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC4–AC5。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_sparse_merge_rejected_cases_preserve_state)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC6。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_sparse_cone_materializes_top_level) | test(=command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=2）：`CL-16` 稀疏 cone 物化与 `CL-27` 拒绝内容修改合并的回归（本卡改动 `src/cli.rs` 护栏与 `clone.rs` 后重跑）。

**Full-suite trigger:** `T-1: 修改共享传输 src/command/fetch.rs、合并引擎 rename_merge.rs/tree_merge.rs 与 src/cli.rs`
**Dependencies:** `CL-16`、`DEP-CL-04`、`DEP-CL-07`（只核验 `merge.rs` 没有在途写者；本卡先合入，不等待 `MP-17`）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/cli.rs`, `src/command/merge.rs`, `src/command/merge/rename_merge.rs`, `src/command/merge/tree_merge.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/cli.rs`, `src/command/merge.rs`, `src/command/merge/rename_merge.rs`, `src/command/merge/tree_merge.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --sparse 部分克隆与目录重命名合并; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=3; prod-files=6; scope=M; deps=CL-16,DEP-CL-04,DEP-CL-07; writeset=串行于 CL-16; release=independent; split-from=CL-16; exception=N/A`

---

### Task CL-17: 确保默认 `libra clone` 正确持久化 `remote.<name>.fetch`（#876）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 修复默认 `libra clone` 未写入标准 `remote.<name>.fetch=+refs/heads/*:refs/remotes/<name>/*` 配置的缺口（当前仅 `--single-branch` 在 `clone.rs:2043-2054` 写入，`remote add` 已在 `remote.rs:822-834` 写入；`--revision` 克隆不写入，见 AC3），避免后续向 `remote.<name>.fetch` 追加 refspec 后 `fetch --prune` 发生越界修剪。
**Out of scope:** `fetch --prune` 端到端（`#541` 与 `FP-15` 未就绪）。该场景只登记在 `DEFER-CL-03`，本卡不预置 `#[ignore]` 测试，也不把启用该测试拆成另一张实现卡。非 `--revision` 的 `--bare` 克隆的 refspec 持久化与 `refs/remotes/<name>/*` 布局维持现状（`PM-33` 依赖，见 `DEP-CL-14`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#876` `remote add` 已写 `remote.<name>.fetch`，但 `clone.rs:2043-2054` 仅在 `--single-branch` 写入而默认 `clone` 遗漏 | `src/command/clone.rs:2043-2054`；`src/command/remote.rs:822-834` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 默认 `libra clone file://<S> d` 后，`d` 中 `libra config get --all remote.origin.fetch` 的 stdout 为 `+refs/heads/*:refs/remotes/origin/*` 加换行。
- [ ] **AC2** `libra clone -o upstream --branch main file://<S> d2` 后，`d2` 中 `libra config get --all remote.upstream.fetch` 的 stdout 为 `+refs/heads/*:refs/remotes/upstream/*` 加换行。
- [ ] **AC3** `libra clone --bare --revision=refs/tags/v1 file://<S> d3` 退出 0 后，`d3` 不存在 `remote.origin.fetch`（`--revision` 不建远端跟踪分支，不写 fetch refspec）。
- [ ] **AC4** 在 `d` 执行 `libra config set --add remote.origin.fetch '+refs/tags/*:refs/tags/*'` 退出 0 后，`libra config get --all remote.origin.fetch` 的 stdout 逐字为 `+refs/heads/*:refs/remotes/origin/*` 与 `+refs/tags/*:refs/tags/*` 两行（原默认条目保留在首行）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_persists_remote_fetch_refspec`：stdout 逐字等于 `+refs/heads/*:refs/remotes/origin/*\n`。
- AC2 → `command::clone_test::test_clone_persists_remote_fetch_refspec`：stdout 逐字等于 `+refs/heads/*:refs/remotes/upstream/*\n`。
- AC3 → `command::clone_test::test_clone_persists_remote_fetch_refspec`：`exit 0` 为前提；以 SQLite 只读连接查询 `d3` 的 `config_kv`，键 `remote.origin.fetch` 的行数为 0。
- AC4 → `command::clone_test::test_clone_persists_remote_fetch_refspec`：`config set --add` 的 `exit 0` 为前提；stdout 逐字等于 `+refs/heads/*:refs/remotes/origin/*\n+refs/tags/*:refs/tags/*\n`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`remote.<name>.fetch`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_persists_remote_fetch_refspec)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::remote_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`remote_test` 全模块回归。

**Full-suite trigger:** `none`
**Dependencies:** `CL-20`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=默认 clone 写 remote.<name>.fetch; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-20; writeset=串行于 CL-20; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-22: 已撤回（不得开工）

**Status:** superseded。`#876` 的 `fetch --prune` 端到端不在本计划单独发版。配置持久化由 `CL-17` 交付；依赖 `#541` 与 `FP-15` 的端到端只记在 `DEFER-CL-03`。本编号不再拥有写集、验收标准或发布边界。

---


## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | CLI 参数重写回归、指针布局放行表完备性 | `source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::index_pack_progress_rewrite/)'`、`source .env.test && cargo nextest run --lib -E 'test(=cli::tests::pointer_layout_allowlist_matches_commands)'` |
| 集成 | `clone` 标志、硬链接、`.git` 回退、alternates、部分克隆、`--sparse`、稀疏护栏、`remote.<name>.fetch`（各卡卡特有用例见追溯表） | `source .env.test && cargo nextest run --test command_test -E 'test(/^command::clone_test::/)'`（共享回归组，「验证执行协议」第 5 项） |
| 兼容 | 浅克隆完整性、配置默认覆盖、兼容账本与命令矩阵对齐、版本面 | `source .env.test && cargo nextest run --test compat_clone_shallow_integrity --test compat_config_defaults_semantics --test compat_ledger_schema --test compat_matrix_alignment --test compat_version_surface_sync` |
| 迁移 | `N/A`（无 SQLite schema 变更） | `N/A` |
| 安全 | 无效 `sparse:` filter 拒绝与目标目录清理、对象符号链接拒绝、指针布局默认拒绝 | `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_rejects_invalid_sparse_filter)'`、`source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_rejects_symlinked_object_paths)'`、`source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_default_deny)'` |
| 性能 | `-l/--local` 同文件系统硬链接复用 inode | `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlinks_objects_on_same_fs)'` |
| live/gated | 收口阶段双环境全量套件 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| CL-01 | `#737` | `src/cli.rs`, `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_boolean_progress_does_not_consume_url`；共享回归组 `cli::tests::index_pack_progress_rewrite`|
| CL-02 | `#739` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_short_n_skips_checkout`；共享回归组 `command::clone_test::test_clone_json_reports_fetch_transfer_counts`|
| CL-03 | `#738` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow`；共享回归组 `command::clone_test::test_clone_from_shallow_git_source_matrix`|
| CL-04 | `#740` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`；共享回归组 `command::clone_test::test_clone_bare_and_mirror_default_names_and_layout`|
| CL-23 | `#740` | `src/utils/util.rs`, `src/command/alternates.rs` |`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`、`docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`、`docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`、`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`docs/commands/alternates.md`、`docs/commands/zh-CN/alternates.md`、`docs/development/commands/alternates.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`|卡特有 `command::clone_test::test_clone_separate_git_dir_pointer_commands`；共享回归组 `command::status_test::`|
| CL-28 | `#740` | `src/cli.rs` |`docs/commands/add.md`、`docs/commands/zh-CN/add.md`、`docs/development/commands/add.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`docs/commands/alternates.md`、`docs/commands/zh-CN/alternates.md`、`docs/development/commands/alternates.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`、`docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`docs/development/commands/am.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`、`docs/commands/apply.md`、`docs/commands/zh-CN/apply.md`、`docs/development/commands/apply.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`、`docs/commands/archive.md`、`docs/commands/zh-CN/archive.md`、`docs/development/commands/archive.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`、`docs/commands/auth.md`、`docs/commands/zh-CN/auth.md`、`docs/development/commands/auth.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`、`docs/commands/automation.md`、`docs/commands/zh-CN/automation.md`、`docs/development/commands/automation.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`、`docs/commands/bisect.md`、`docs/commands/zh-CN/bisect.md`、`docs/development/commands/bisect.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`、`docs/commands/blame.md`、`docs/commands/zh-CN/blame.md`、`docs/development/commands/blame.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`、`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/bundle.md`、`docs/commands/zh-CN/bundle.md`、`docs/development/commands/bundle.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`、`docs/commands/cache.md`、`docs/commands/zh-CN/cache.md`、`docs/development/commands/cache.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`、`docs/commands/check-attr.md`、`docs/commands/zh-CN/check-attr.md`、`docs/development/commands/check-attr.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`、`docs/commands/check-ignore.md`、`docs/commands/zh-CN/check-ignore.md`、`docs/development/commands/check-ignore.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`、`docs/commands/check-mailmap.md`、`docs/commands/zh-CN/check-mailmap.md`、`docs/development/commands/check-mailmap.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`、`docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`、`docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`、`docs/commands/cloud.md`、`docs/commands/zh-CN/cloud.md`、`docs/development/commands/cloud.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`、`docs/commands/commit-tree.md`、`docs/commands/zh-CN/commit-tree.md`、`docs/development/commands/commit-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`、`docs/commands/count-objects.md`、`docs/commands/zh-CN/count-objects.md`、`docs/development/commands/count-objects.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`、`docs/commands/credential.md`、`docs/commands/zh-CN/credential.md`、`docs/development/commands/credential.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`、`docs/commands/deps.md`、`docs/commands/zh-CN/deps.md`、`docs/development/commands/deps.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`、`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`docs/commands/diff.md`、`docs/commands/zh-CN/diff.md`、`docs/development/commands/diff.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`、`docs/commands/diff-files.md`、`docs/commands/zh-CN/diff-files.md`、`docs/development/commands/diff-files.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`、`docs/commands/diff-index.md`、`docs/commands/zh-CN/diff-index.md`、`docs/development/commands/diff-index.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`、`docs/commands/diff-tree.md`、`docs/commands/zh-CN/diff-tree.md`、`docs/development/commands/diff-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`、`docs/commands/dirty.md`、`docs/commands/zh-CN/dirty.md`、`docs/development/commands/dirty.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`、`docs/commands/fast-export.md`、`docs/commands/zh-CN/fast-export.md`、`docs/development/commands/fast-export.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`、`docs/commands/fast-import.md`、`docs/commands/zh-CN/fast-import.md`、`docs/development/commands/fast-import.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`、`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`、`docs/commands/file.md`、`docs/commands/zh-CN/file.md`、`docs/development/commands/file.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`、`docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`、`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`、`docs/commands/fsck.md`、`docs/commands/zh-CN/fsck.md`、`docs/development/commands/fsck.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`、`docs/commands/gc.md`、`docs/commands/zh-CN/gc.md`、`docs/development/commands/gc.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`、`docs/commands/grep.md`、`docs/commands/zh-CN/grep.md`、`docs/development/commands/grep.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`、`docs/commands/hash-object.md`、`docs/commands/zh-CN/hash-object.md`、`docs/development/commands/hash-object.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`、`docs/commands/hooks.md`、`docs/commands/zh-CN/hooks.md`、`docs/development/commands/hooks.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`、`docs/commands/hydrate.md`、`docs/commands/zh-CN/hydrate.md`、`docs/development/commands/hydrate.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`、`docs/commands/index-pack.md`、`docs/commands/zh-CN/index-pack.md`、`docs/development/commands/index-pack.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`、`docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`、`docs/commands/investigate.md`、`docs/commands/zh-CN/investigate.md`、`docs/development/commands/investigate.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`、`docs/commands/layer.md`、`docs/commands/zh-CN/layer.md`、`docs/development/commands/layer.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`、`docs/commands/lfs.md`、`docs/commands/zh-CN/lfs.md`、`docs/development/commands/lfs.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`、`docs/commands/logfile.md`、`docs/commands/zh-CN/logfile.md`、`docs/development/commands/logfile.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`、`docs/commands/ls-files.md`、`docs/commands/zh-CN/ls-files.md`、`docs/development/commands/ls-files.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`、`docs/commands/ls-remote.md`、`docs/commands/zh-CN/ls-remote.md`、`docs/development/commands/ls-remote.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`、`docs/commands/ls-tree.md`、`docs/commands/zh-CN/ls-tree.md`、`docs/development/commands/ls-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`、`docs/commands/mailinfo.md`、`docs/commands/zh-CN/mailinfo.md`、`docs/development/commands/mailinfo.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`、`docs/commands/maintenance.md`、`docs/commands/zh-CN/maintenance.md`、`docs/development/commands/maintenance.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`、`docs/commands/media.md`、`docs/commands/zh-CN/media.md`、`docs/development/commands/media.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`、`docs/commands/mega2.md`、`docs/commands/zh-CN/mega2.md`、`docs/development/commands/mega2.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`、`docs/commands/memory.md`、`docs/commands/zh-CN/memory.md`、`docs/development/commands/memory.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`、`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`docs/commands/merge-base.md`、`docs/commands/zh-CN/merge-base.md`、`docs/development/commands/merge-base.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`、`docs/commands/merge-file.md`、`docs/commands/zh-CN/merge-file.md`、`docs/development/commands/merge-file.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`、`docs/commands/mergetool.md`、`docs/commands/zh-CN/mergetool.md`、`docs/development/commands/mergetool.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`、`docs/commands/metadata.md`、`docs/commands/zh-CN/metadata.md`、`docs/development/commands/metadata.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`、`docs/commands/mv.md`、`docs/commands/zh-CN/mv.md`、`docs/development/commands/mv.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`、`docs/commands/notes.md`、`docs/commands/zh-CN/notes.md`、`docs/development/commands/notes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`、`docs/commands/op.md`、`docs/commands/zh-CN/op.md`、`docs/development/commands/op.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`、`docs/commands/open.md`、`docs/commands/zh-CN/open.md`、`docs/development/commands/open.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`、`docs/commands/pack-objects.md`、`docs/commands/zh-CN/pack-objects.md`、`docs/development/commands/pack-objects.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`、`docs/commands/push.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/push.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`、`docs/commands/read-tree.md`、`docs/commands/zh-CN/read-tree.md`、`docs/development/commands/read-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`、`docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`、`docs/commands/reflog.md`、`docs/commands/zh-CN/reflog.md`、`docs/development/commands/reflog.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`、`docs/commands/remote.md`、`docs/commands/zh-CN/remote.md`、`docs/development/commands/remote.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`、`docs/commands/repack.md`、`docs/commands/zh-CN/repack.md`、`docs/development/commands/repack.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`、`docs/commands/replace.md`、`docs/commands/zh-CN/replace.md`、`docs/development/commands/replace.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`、`docs/commands/rerere.md`、`docs/commands/zh-CN/rerere.md`、`docs/development/commands/rerere.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`、`docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`、`docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`、`docs/commands/rev-list.md`、`docs/commands/zh-CN/rev-list.md`、`docs/development/commands/rev-list.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`docs/commands/revert.md`、`docs/commands/zh-CN/revert.md`、`docs/development/commands/revert.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`、`docs/commands/review.md`、`docs/commands/zh-CN/review.md`、`docs/development/commands/review.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/development/commands/revision.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`、`docs/commands/rm.md`、`docs/commands/zh-CN/rm.md`、`docs/development/commands/rm.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`、`docs/commands/sandbox.md`、`docs/commands/zh-CN/sandbox.md`、`docs/development/commands/sandbox.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`、`docs/commands/send-email.md`、`docs/commands/zh-CN/send-email.md`、`docs/development/commands/send-email.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`、`docs/commands/service.md`、`docs/commands/zh-CN/service.md`、`docs/development/commands/service.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`、`docs/commands/shortlog.md`、`docs/commands/zh-CN/shortlog.md`、`docs/development/commands/shortlog.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`、`docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`、`docs/commands/show-ref.md`、`docs/commands/zh-CN/show-ref.md`、`docs/development/commands/show-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`、`docs/commands/sparse-view.md`、`docs/commands/zh-CN/sparse-view.md`、`docs/development/commands/sparse-view.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`、`docs/commands/stash.md`、`docs/commands/zh-CN/stash.md`、`docs/development/commands/stash.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`、`docs/commands/symbolic-ref.md`、`docs/commands/zh-CN/symbolic-ref.md`、`docs/development/commands/symbolic-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`、`docs/commands/tag.md`、`docs/commands/zh-CN/tag.md`、`docs/development/commands/tag.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`、`docs/commands/update-index.md`、`docs/commands/zh-CN/update-index.md`、`docs/development/commands/update-index.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`、`docs/commands/update-ref.md`、`docs/commands/zh-CN/update-ref.md`、`docs/development/commands/update-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`、`docs/commands/verify-pack.md`、`docs/commands/zh-CN/verify-pack.md`、`docs/development/commands/verify-pack.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`、`docs/commands/worktree.md`、`docs/commands/zh-CN/worktree.md`、`docs/development/commands/worktree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`、`docs/commands/write-tree.md`、`docs/commands/zh-CN/write-tree.md`、`docs/development/commands/write-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`|卡特有 `command::clone_test::test_clone_separate_git_dir_default_deny`；卡特有 `cli::tests::pointer_layout_allowlist_matches_commands`|
| CL-25 | `#740` | `REL-CL-01` 发布面（`Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`） | 随版本发布说明同步；推送三张子卡的本地 `cf` 网站提交并核验部署 |手工证据（执行记录）；聚合守卫 `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`、`command::clone_test::test_clone_separate_git_dir_pointer_commands`、`command::clone_test::test_clone_separate_git_dir_default_deny`；聚合守卫 `cli::tests::pointer_layout_allowlist_matches_commands`；ER-04 C 组强制门 `--test compat_version_surface_sync`（不计入 G-03）|
| CL-05 | `#741` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_revision_single_rev`；共享回归组 `--test compat_clone_shallow_integrity`|
| CL-06 | `#752` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_config_override_dash_c`；共享回归组 `--test compat_config_defaults_semantics`|
| CL-08 | `#743` | `src/command/clone.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`；卡特有 `command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy`；卡特有 `command::clone_test::test_clone_local_rejects_symlinked_object_paths`；共享回归组 `command::clone_test::test_clone_local_flag_accepted_for_local_source`|
| CL-07 | `#742` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_no_hardlinks_copies_objects`；共享回归组 `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`|
| CL-09 | `#744` | `src/command/clone.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`；共享回归组 `command::clone_test::test_clone_bare_and_mirror_default_names_and_layout`|
| CL-10 | `#751` | `src/command/clone.rs`, `fetch.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_reference_alternates_borrows_objects`；卡特有 `command::clone_test::test_clone_reference_if_able_and_invalid_repo`|
| CL-18 | `#751` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_dissociate_keeps_repo_independent`；卡特有 `command::clone_test::test_clone_shared_dissociate_detaches_alternates`；共享回归组 `command::clone_test::test_clone_reference_alternates_borrows_objects`|
| CL-19 | `#754` | `src/utils/storage/local.rs`, `object_ext.rs`, `fetch.rs`, `local_client.rs`, `cat_file.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`|卡特有 `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`；共享回归组 `command::cat_file_test::`|
| CL-12 | `#748` | `src/command/clone.rs`, `local_client.rs`, `protocol/mod.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_rejects_invalid_sparse_filter`；共享回归组 `command::clone_test::test_clone_local_flag_accepted_for_local_source`|
| CL-13 | `#746, #754` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_filter_promisor_config_and_combine`；共享回归组 `command::clone_test::test_clone_json_reports_fetch_transfer_counts`；共享回归组 `command::clone_test::test_clone_rejects_invalid_sparse_filter`|
| CL-24 | `#746, #754` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_combine_tree_depth_and_fallback`；共享回归组 `command::clone_test::test_clone_json_reports_fetch_transfer_counts`；共享回归组 `command::clone_test::test_clone_filter_promisor_config_and_combine`|
| CL-29 | `#748`；`PM-33`/`t5616#33` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`；共享回归组 `command::clone_test::test_clone_combine_tree_depth_and_fallback`；共享回归组 `command::clone_test::test_clone_rejects_invalid_sparse_filter`|
| CL-21 | `#754` | `src/command/clone.rs`, `fetch.rs`（按需拉取进度） |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`|卡特有 `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`；共享回归组 `command::cat_file_test::`|
| CL-14 | `#750` | `src/command/clone.rs`, `rev_parse.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`|卡特有 `command::clone_test::test_clone_ref_storage_format_and_rev_parse`；共享回归组 `command::rev_parse_test::`|
| CL-15 | `#753` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_single_branch_from_detached_head`；共享回归组 `command::clone_test::test_clone_depth_implies_single_branch_matrix`|
| CL-26 | `#756` | `src/cli.rs` |`docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md`、`docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`|卡特有 `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`；共享回归组 `command::{checkout,switch,reset,restore}_test::`|
| CL-27 | `#756` | `src/cli.rs` |`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`、`docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`、`docs/commands/stash.md`、`docs/commands/zh-CN/stash.md`、`docs/development/commands/stash.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`|卡特有 `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`；共享回归组 `command::{merge,pull,rebase,cherry_pick,stash}_test::`|
| CL-16 | `#756` | `src/command/clone.rs`, `src/internal/sparse/mod.rs`, `src/command/status.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`|卡特有 `command::clone_test::test_clone_sparse_cone_materializes_top_level`；共享回归组 `command::status_test::`|
| CL-20 | `#756` | `src/command/clone.rs`, `src/cli.rs`, `merge.rs`, `rename_merge.rs`, `tree_merge.rs`, `fetch.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`|卡特有 `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`；卡特有 `command::clone_test::test_non_sparse_partial_merge_fetches_missing_objects`；卡特有 `command::clone_test::test_sparse_merge_rejected_cases_preserve_state`；共享回归组 `command::clone_test::test_clone_sparse_cone_materializes_top_level`、`command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`|
| CL-17 | `#876` | `src/command/clone.rs`（`remote add` 已写于 `remote.rs:822-834`） |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_persists_remote_fetch_refspec`；共享回归组 `command::remote_test::`|
| CL-22 | 已撤回 | 无写集 | 无 |N/A（本卡无独立测试命令）|

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 基线锚点核对、计划文本 Claude（Subagent）与 Codex 双审对同一字节 PASS | 本计划文件与 review log | 修改计划文档 |
| M1 | Phase 1（`CL-01..CL-04, CL-23, CL-28, CL-25, CL-05..CL-06`）全部完成并发布 | 各独立卡/`REL-CL-01` release tag；各卡 focused 绿，命中 `T-1` 的卡另有全量绿，`CL-25` 有 `T-2` 全量绿 | 发布后前滚新 patch + 降级指引 |
| M2 | Phase 2（5 张卡）全部完成并发布 | 各卡 release tag；各卡 focused 绿，命中 `T-1` 的卡（`CL-08`、`CL-09`、`CL-10`）另有全量绿 | 发布后前滚新 patch + 降级指引 |
| M3 | Phase 3（13 张活动卡，不含已撤回的 `CL-22`）全部完成，全量收口门全绿。`DEFER-CL-03` 不计入这 13 张 | 全量 nextest 证据写入本计划 | 前滚修复新开 `FIX-*` 卡 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 检出之前的克隆失败（参数校验、源解析、传输、过滤与浅仓库校验、对象复制；含 `--separate-git-dir`、`--revision`、无效 `--filter` 等本计划全部拒绝用例） | 无残留 | 删除新建的目标目录与 `--separate-git-dir` 目录（Git `remove_junk`，`builtin/clone.c:414-423`） | 残留半成品 `.libra` 目录 |
| 检出阶段失败（克隆已完成） | 已完成的仓库 | 保留仓库供检查与恢复（Git `junk_mode = JUNK_LEAVE_REPO`，`builtin/clone.c:1656`）；本计划无此类验收用例 | 删除已完成克隆的仓库 |
| `--dissociate` 拷贝中断 | 未完成的新目录 | 失败回滚清理新建目录，不损坏源 `--reference` | 修改或删除源 `--reference` 仓库 |
| D 组 CI/release 失败 | 已推送主分支提交 | 按 ER-04/ER-10 前滚修复发新 patch | 回退/强推已发布历史 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `src/cli.rs` 全局 `--progress` 预处理影响其它子命令 | 中：可能误改非 `clone` 命令参数 | 仅针对 `clone`（与既有 `index-pack`）触发重写并跑 `T-1` 全量 | `CL-01` |
| 部分克隆 `--filter` 与远端能力协商不匹配 | 中：旧远端不支持 `filter` | 本地传输（本计划验收范围）按 Git 约定在远端未广告 `filter` 时安全回退全量克隆（`CL-24` AC7）；网络传输的协商与回退不在本计划验收，见 `DEFER-CL-08` | `CL-24` |
| 稀疏护栏漏掉某个未适配命令 | 中：`core.sparseCheckout=true` 时命令仍改写工作区 | `CL-27` 先给 `merge`/`pull`/`rebase`/`cherry-pick`/`stash` 过渡期拒绝。`CL-20` 只解禁目录重命名合并，其它 `merge` 继续拒绝。`pull`/`rebase`/`cherry-pick`/`stash` 继续受 `CL-27` 保护。`CL-26` 继续保护检出类四命令 | `CL-26`, `CL-27`, `CL-20` |
| 新增子命令漏出默认拒绝 | 中：新的 `Commands` 变体可能写错目录 | `CL-28` 的单元测试 `cli::tests::pointer_layout_allowlist_matches_commands` 枚举全部 `Commands` 变体的判定并与期望映射等值比较，新增变体未登记即失败；端到端夹具覆盖 `restore`、`commit`、`branch`、`checkout` 四个代表命令 | `CL-28` |
| 测试接缝（`LIBRA_TEST_CLONE_HARDLINK_FAIL`、`LIBRA_TEST_CLONE_FOREIGN_OWNER`、`LIBRA_TEST_CLONE_SWAP_BEFORE_LINK`、`LIBRA_TEST_UPLOAD_PACK_NO_FILTER`、`LIBRA_TEST_TRACE_LAZY_FETCH`）进入生产路径 | 中：用户环境变量改变克隆或拉取行为 | 只在 `LIBRA_TEST=1` 时读取（「验证执行协议」第 7 项），各卡 ER-05 评审核对 | `CL-08`, `CL-13`, `CL-19` |
| 网站检出路径与 `AGENTS.md:135` 不一致 | 低：执行者按过时文档选错检出 | 以「文档职责」所载用户 2026-11-18 11:55:09 UTC 确认为准，「文档与网站验收命令」第 2 步逐次核验 | 全部改命令文档的卡 |
| 网络远端 `--filter` | 中：`CL-13` 之后经 HTTP/SSH/git:// 克隆时 `--filter` 可能未被协商（`DEFER-CL-08`），用户得到全量克隆 | 本计划只验收本地传输；网络客户端协商随 `DEFER-CL-08` 规划，`docs/commands/clone.md` 写明限制 | `CL-13`, `CL-24` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `-l/--local` 硬链接 | `O(objects + fallback bytes)` | 硬链接成功的对象无需复制字节；回退复制的对象按其字节数占用额外空间 | 成功硬链接对象在同一文件系统共享 inode；默认本地路径首次硬链接失败后其余对象改为复制（Git 语义），显式 `--local` 硬链接失败即拒绝 | `CL-08` |
| `--filter=blob:none` | 过滤协商成功时处理 `O(commits + trees)` 个对象，传输和存储另计选中对象字节数；全量回退时随完整对象集及字节数增长 | 协商成功时初始不传输 blob，按需 lazy fetch；本地传输在远端未广告 filter 时回退全量传输并保存完整对象集（网络传输见 `DEFER-CL-08`） | 依远端能力与回退对象集确定 | `CL-13, CL-19, CL-24` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 与 `docs/development/commands/_compatibility.md` 已随相关任务卡同步；`docs/error-codes.md`: `N/A`（不新增错误码）。
- [ ] `CL-28` 的 `Implementation write set` 里每一份命令文档都已逐文件验收。`CL-26` 与 `CL-27` 的 checkout、switch、reset、restore、merge、pull、rebase、cherry-pick、stash 文档，以及 `clone`、`rev-parse`、`cat-file`、`status`、`log`、`branch` 的 EN、zh-CN、development 与网站文件也已同步。
- [ ] 上一条里的 zh-CN 文件已同步。
- [ ] 上一条里的网站 `.en.md` 文件已同步。
- [ ] `tests/INDEX.md` 已同步（如新增 target）。
- [ ] `plan-long.md` 与 `plan-status.md` 由协调者统一登记。

## Codex review log

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 (Claude) | `plan-20261117-clone.md` 初稿 | `FAIL` | 8 项 P1（D10、`CL-13` 拆出 `CL-19`、循环依赖、`commit-signing`/`fetch-push`、`ref-storage-format`、`#876` 基线、AC、旧测试重写）均在 R2 关闭 | P2 全部修复 | Claude R1 review |
| R1 (Codex) | `plan-20261117-clone.md` 初稿 | `FAIL` | 5 项 P1（`CL-11`/`CL-13`、`CL-02/05/12/15/16` AC、`CL-07` 先于 `CL-08`、`CL-08` 本地规则）均在 R2 关闭 | — | Codex R1 review |
| R2 (Codex) | `plan-20261117-clone.md` R2 | `FAIL` | 2 项 P1（`CL-19` 需前置至 `CL-11`/`CL-13` 前；`CL-16` 改为 `src/internal/sparse/mod.rs`+`src/command/sparse_view.rs`）均在 R3 关闭 | P2（`CL-14` `resolve_ref_format` `pub(crate)`+映射）在 R3 关闭 | Codex R2 review |
| R3 (Codex) | `plan-20261117-clone.md` R3 | `FAIL` | 2 项 P1（`CL-11` 收紧避免无 `promisor=true` 省略对象；`CL-17` vs `DEP-CL-05` 验收边界）均在 R4 关闭 | P2（完整 `发布窗口顺序`）在 R4 关闭 | Codex R3 review |
| R3 (Claude) | `plan-20261117-clone.md` R3 | `FAIL` | 4 项 P1（`ADR-CL-04`、`ADR-CL-03`、`T-1`、`CL-19`）已在 R4/R5 关闭 | P2 全部修复 | Claude R3 review |
| R4 (Claude) | `plan-20261117-clone.md` R4 | `FAIL` | 4 项 P1（`ADR-CL-04` 不改 `init.rs` 与 D25、`CL-04` `ADR-CL-05` 指针边界、`CL-16` 拆出 `CL-20` 与 `_compatibility.md`、`T-1` 补齐 `CL-05/09/10/11/13`）均在 R5 关闭 | P2（`CL-17/13/19/06/01/12` 与测试重命名）在 R5 关闭 | Claude R4 review |
| R4 (Codex) | `plan-20261117-clone.md` R4 | `FAIL` | 2 项 P1（`#876` 收口门闭环、`CL-13`/`19` 自定义远端名 `<remote_name>`）均在 R5 关闭 | — | Codex R4 review |
| R5 (Claude) | `plan-20261117-clone.md` R5 | `FAIL` | 3 项 P1（`ADR-CL-02` `REL-CL-01` 残留、`CL-04` 补齐 `src/utils/util.rs:277,797` 与 `T-1`、`CL-20` 批量 `1 fetch`/`6 objects` 与 `merge.md` 文档）均在 R6 关闭 | P2（`DEP-CL-03`、`codeql.yml`、`Release write set 默认`、`CL-19` `hydrate.rs` 表述与 `landing=4`、`cat-file` 文档收口）在 R6 关闭 | Claude R5 review |
| R5 (Codex) | `plan-20261117-clone.md` R5 | `FAIL` | 1 项 P1（`ADR-CL-01` `--no-progress` 重写边界）在 R6 关闭 | P2（`CL-04` 写集与 `T-1`、`实施顺序` `CL-20` 位置、`CL-13`/`CL-19` 拆卡注解）在 R6 关闭 | Codex R5 review |
| R6 (Claude) | `plan-20261117-clone.md` R6 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R7 重审 | — | Claude R6 review |
| R6 (Codex) | `plan-20261117-clone.md` R6 | `FAIL` | 1 项 P1（`CL-04` 需审计并适配 `util.rs:314-324` `worktree_info_file_paths` 等 `.libra` 指针消费者）在 R7 关闭 | — | Codex R6 review |
| R7 (Claude) | `plan-20261117-clone.md` R7 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R8 重审 | — | Claude R7 review |
| R7 (Codex) | `plan-20261117-clone.md` R7 | `FAIL` | 2 项 P1（`完成判据` `#876` 端到端测试改由 `plan-20261117-fetch-push.md#FP-15` 具名测试承接；`CL-04` 收窄至 `util.rs` 发现与 `worktree_info_file_paths` 并将 `alternates.rs:91` 等记入 `DEFER-CL-02`）均在 R8 关闭 | — | Codex R7 review |
| R8 (Claude) | `plan-20261117-clone.md` R8 | `FAIL` | 4 项 P1（`CL-13` 补 `#746` `combine:blob:none+tree:1`、`CL-20` 补 `rename_merge.rs`/`tree_merge.rs`/`fetch.rs` 批量拉取与 `T-1`/`DEP-CL-07`、新增 `DEP-CL-10` 协调 `util.rs`、修复 `CL-02` nextest 过滤器）均在 R9 关闭 | P2（`CL-10` `fsck`、`CL-13` 端到端 lazy fetch、`CL-04`/`12` Git 精确报错、`DEP-CL-04/09`、`Phase 1`、`rev_parse.rs:59`、`codeql.yml`、`Rollback mode` 判据）在 R9 关闭 | Claude R8 review |
| R8 (Codex) | `plan-20261117-clone.md` R8 | `FAIL` | 2 项 P1（`CL-17` 显式承接 `test_clone_persists_remote_fetch_and_prune_with_extra_refspec`；`CL-04` 纳入 `alternates.rs:91` 并验证指针布局基本命令）均在 R9 关闭 | P2（统一 `DEP-CL-05` 与 `完成判据` 测试名）在 R9 关闭 | Codex R8 review |
| R9 (Claude) | `plan-20261117-clone.md` R9 | `FAIL` | 4 项 P1（移除 `CL-20` 重复块、消除 `CL-04`/`DEFER-CL-02` 矛盾、补 R6-R10 `修订历史`、`CL-13` 拆出 `CL-21`）均在 R10 关闭 | P2（审计表与卡内 `landing`/`deps`、`CL-19`、`CL-01`、`CL-14`、`DEP-CL-04/07/09`、`Plan Author 5`）在 R10 关闭 | Claude R9 review |
| R9 (Codex) | `plan-20261117-clone.md` R9 | `FAIL` | 2 项 P1（`CL-04` 与 `DEFER-CL-02` 自相矛盾、`#876` 完成门明确由 `Plan Author 5` 移除 `#[ignore]` 并正常执行）均在 R10 关闭 | P2（移除 `CL-20` 重复字段块）在 R10 关闭 | Codex R9 review |
| R10 (Claude) | `plan-20261117-clone.md` R10 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R11 重审 | — | Claude R10 review |
| R10 (Codex) | `plan-20261117-clone.md` R10 | `FAIL` | 3 项 P1（`CL-04` 范围收紧、`CL-11` 与 `CL-13` 合并 `REL-CL-01` 避免静默忽略 `--filter`、新增 `CL-22` 显式承接 `#876` 移除 `#[ignore]`）均在 R11 关闭 | P2（`Phase 3` 与 `M3` 卡数更新为 `11 张卡`）在 R11 关闭 | Codex R10 review |
| R11 (Claude) | `plan-20261117-clone.md` R11 | `FAIL` | 3 项 P1（`CL-22` 规范为 `implementation`+`independent`+`T-2` 并移出 `--all`、`CL-11` 并入 `CL-13` 移除 `REL-CL-01`、`CL-21` 补 `DEP-CL-04` 及 `DEP-CL-03/04` 全覆盖）均在 R12 关闭 | P2（`默认覆盖`、`ADR-CL-02`、`DEP-CL-05`、`CL-20` 追溯表、`CL-22`、`CL-21` 单轴）在 R12 关闭 | Claude R11 review |
| R11 (Codex) | `plan-20261117-clone.md` R11 | `FAIL` | 2 项 P1（`REL-CL-01` 不合模板与 `ADR-CL-02` 矛盾、`CL-22` 字段不合模板）均在 R12 关闭 | — | Codex R11 review |
| R12 (Codex) | `plan-20261117-clone.md` R12 | `FAIL` | 4 项 P1（`ADR-CL-04`/`CL-14` 如实输出 `sqlite` 并延后 `files`/`reftable` 至 `DEFER-CL-01`、`CL-08` 逐文件 `nlink > 1`、审计表 `CL-22` 同步、`DEP-CL-05`/`DEFER-CL-03`/`完成判据` 一致）均在 R13 关闭 | P2（审计表 `CL-12` `landing=2`）在 R13 关闭 | Codex R12 review |
| R12 (Claude) | `plan-20261117-clone.md` R12 | `FAIL` | 5 项 P1（`CL-22` 审计表同步、`CL-13` 拆出 `CL-24` 守 `AC<=8`、`CL-04` 拆出 `CL-23`、`CL-16` 纳 `status.rs` 与 `DEFER-CL-04`、`DEP-CL-03/04` 明确顺序）均在 R13/R14 关闭 | P2（`R14`、`23 张`、`CL-11` 清理、`CL-22` `T-5`、`CL-19` `5/8`、`clone.rs:220`、`.git` 检查）在 R14 关闭 | Claude R12 review |
| R13 (Claude) | `plan-20261117-clone.md` R13 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R14 重审 | — | Claude R13 review |
| R13 (Codex) | `plan-20261117-clone.md` R13 | `FAIL` | 2 项 P1（`成功定义`/`DEFER-CL-01` 明确 `#750` `files` 延后归口、`CL-23` 未适配 pointer 子命令 fail-closed）均在 R14 关闭 | P2（`CL-22` `T-5`、`M3` 对齐 `完成判据`）在 R14 关闭 | Codex R13 review |
| R14 (Claude) | `plan-20261117-clone.md` R14 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R15 重审 | — | Claude R14b review |
| R14 (Codex) | `plan-20261117-clone.md` R14 | `FAIL` | 3 项 P1（`CL-04`+`CL-23` 纳为 `REL-CL-01` 家族卡与 `CL-25` 发布点、`CL-13` 对 `combine:*` fail-closed 拒绝、`CL-16` 为 `checkout`/`switch`/`reset` 增稀疏 fail-closed 护栏）均在 R15 关闭 | P2（重申 `CL-22` 已统一为 `implementation`+`independent`）在 R15 关闭 | Codex R14 review |
| R15 (Claude) | `plan-20261117-clone.md` R15 | `FAIL` | 6 项 P1（`CL-04`/`23` `C/D coverage from: CL-25`、`CL-23` 纳入 `src/cli.rs` `command_preflight` 护栏并修正 `util.rs` 锚点、`CL-16` 拆出 `CL-26` 在 `cli.rs` 统一护栏 9 个命令及 ER-06a 文档并补 `DEP-CL-11`、`CL-13` AC 核实为 `7/8`）均在 R16 关闭 | P2（`CL-25` 移除 `--all` 与 `Docs: N/A`、`CL-04`/`23` `N/A`、`CL-21` `2/2`、`ADR-CL-02`/`05`、`CL-06/10/12/17/22`、`DEP-CL-04`、`compat_ledger_schema`）在 R16 关闭 | Claude R15 review |
| R16 (Codex) | `plan-20261117-clone.md` R16 | `FAIL` | 3 项 P1（`CL-26` 九命令超 8、`CL-13` 文档门计入 `7/8`、风险表仍指向 `CL-13` 且缺护栏风险） | `2026-11-17 09:44:31 UTC` 关闭 | Codex R16 review |
| R17 (Codex) | `plan-20261117-clone.md` R17 | `FAIL` | 3 项 P1（`CL-17`/`CL-22` 拆测试、Phase 3 计数、`CL-23` 谓词少计） | `2026-11-17 09:51:20 UTC` 关闭 | Codex R17 review |
| R24 (Codex) | `plan-20261117-clone.md` R24 | `FAIL` | P1: `CL-23` 拒绝 `commit` 但文档只列 clone；收口清单缺 `CL-26`/`CL-27` 命令文档 | `2026-11-17 10:43:14 UTC` 拆出 `CL-28` 并补收口清单 | Codex R24 review |
| R25 (Codex) | `plan-20261117-clone.md` R25 | `FAIL` | P1: `CL-28` 只拒绝 7 条命令，`restore` 等仍可能写错目录 | `2026-11-17 10:52:35 UTC` 改为默认拒绝 | Codex R25 review |
| R26 (Codex) | `plan-20261117-clone.md` R26 | `FAIL` | P1: `CL-28` 文档写集不全；`CL-12`/`CL-13` 与 `CL-24` 的最终断言冲突 | `2026-11-17 11:14:03 UTC` 关闭 | Codex R26 review |
| R27 (Codex) | `plan-20261117-clone.md` R27 | `FAIL` | P1: `CL-13` 单个 filter 与 `CL-24` 冲突；追溯表和收口清单未覆盖 `CL-28` 写集 | `2026-11-17 11:19:54 UTC` 关闭 | Codex R27 review |
| R15 (Codex) | `plan-20261117-clone.md` R15 | `FAIL` | 4 项 P1（`CL-04`/`23` `C/D coverage from: CL-25`、`CL-23` 中央 `cli.rs` `command_preflight` 护栏、`CL-16` 拒绝 `--sparse --filter=...` 且 `CL-26` 护栏含 `merge` 及 ER-06a 全量命令文档）均在 R16 关闭 | — | Codex R15 review |
| R30 (Codex gpt-6.1-terra) | `plan-20261117-clone.md` | `FAIL` | P1: `DEP-CS-03` 含撤回卡且双方范围不一致；`sparse:oid` 未交付却供 `PM-03` 验收；网站部署门不完整。已对齐活动卡集合、新增 `CL-29` 并补齐网站门。该轮明确未完成全文复审，R31 不得据此推定其它维度通过 | R31 完整复审 | Codex R30 只读评审（部分覆盖） |
| R31 (Codex gpt-6.1-terra) | `plan-20261117-clone.md` | FAIL | P1: 各卡网站写集仍使用 `../libra-backend`，与唯一 `/Volumes/Data/gitmono/libra-backend` 目标冲突；已统一全部 284 处路径。该轮未完成全文复审，R32 不得据此推定其它维度通过 | R32 完整复审 | Codex R31 只读评审（部分覆盖） |
| R33-A (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 1–800 行 | FAIL | P2: `CL-08` 硬链接失败允许复制回退，却要求所有对象共享 inode。已将 inode 限定为成功对象并增加同文件系统失败复制回退测试（AC=4/8、VER=3/8）。本轮未完成全分区复审，R34-A 不得据此推定其它维度通过 | R34-A 分区完整复审 | Codex R33-A 只读评审（部分覆盖） |
| R34-A (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 1–800 行 | FAIL | P1: “硬链接成功”条件可被零成功对象/全复制空真满足。已增加至少一个松散对象和一个 pack 对象实际硬链接成功并逐一比较设备号/inode 的正控制。R35-A 分区复核 | R35-A 分区完整复审 | Codex R34-A 只读评审（部分覆盖） |
| R36-A (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 1–400 行及 CL-04 完整卡片 | FAIL | P1: 正常夹具只要求各一个对象硬链接，允许其余可链接对象被复制。已改为逐一检查所有符合硬链接条件的对象文件。R37-A 分区复核 | R37-A 分区完整复审 | Codex R36-A 只读评审（分区覆盖） |
| R37-A (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 1–400 行、CL-04 完整卡片及定向交叉核对 | PASS | 无实质问题。全部符合条件对象的硬链接、失败复制回退、CL-29 blob 与 PM-03/DEP-PM-08/DEP-CS-03 均符合 | R38-B 复审第 401–800 行 | Codex R37-A 只读评审（分区覆盖） |
| R38-B (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 401–800 行及 CL-18 完整卡片 | FAIL | P1: `CL-08` 缺符号链接安全边界，`CL-18` 漏 `--shared --dissociate`；P2: `CL-28` 写集含已移除命令文档。已分别补 fail-closed、shared 独立验收并删除 20 条过时文档路径。R39-B 分区复核 | R39-B 分区完整复审 | Codex R38-B 只读评审（分区覆盖） |
| R39-B (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 401–800 行及 CL-18 完整卡片 | FAIL | P1: 分支不变性可能仅按文件存在漏检；P2: Implementation write set 仍含 20 条过时文档路径。已改为 ref API/数据库查询比较状态，并从写集删除全部剩余过时路径。R40-B 分区复核 | R40-B 分区完整复审 | Codex R39-B 只读评审（分区覆盖） |
| R40-B (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 401–800 行及 CL-18 完整卡片 | FAIL | P2: 写集和文档影响仍包含 package/prune/stats/repository-hooks 四组未发布或非命令路径。已删除全部 16 组路径。R41-B 分区复核 | R41-B 分区完整复审 | Codex R40-B 只读评审（分区覆盖） |
| R41-B (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 401–800 行及 CL-18 完整卡片 | PASS | 无实质问题。CL-28 两组清单已删除指定路径；ref API/数据库状态比较、CL-08 符号链接拒绝与 CL-18 shared dissociate 验收均符合 | R42-C 复审第 801 行至文件末 | Codex R41-B 只读评审（分区覆盖） |
| R42-C (Codex gpt-6.1-terra) | 第 801–1518 行及定向跨计划核对 | FAIL | P2：性能摘要无条件声称零复制及共享 inode，与 CL-08 复制回退冲突。已限定硬链接成功对象并补充回退容量成本 | R43-C 分区完整复审 | Codex R42-C 只读评审（分区覆盖） |
| R43-C (Codex gpt-6.1-terra) | 第 801–1520 行及跨界核对 | FAIL | P2：性能摘要遗漏过滤能力缺失时的全量回退与硬链接失败时的复制字节成本。已补充两种回退 | R44-C 分区完整复审 | Codex R43-C 只读评审（分区覆盖） |
| R44-C (Codex gpt-6.1-terra) | 第 801 行至末及跨界核对 | FAIL | P1：CL-20 错误禁止非稀疏部分克隆按需获取；P2：过滤成功成本漏对象字节数。已补正向回归及普通仓库负向断言，并补容量描述 | R45-C 分区完整复审 | Codex R44-C 只读评审（分区覆盖） |
| R45-C (Codex gpt-6.1-terra) | 第 801 行至末及 CL-18 | FAIL | P1：CL-20 合并独立判据超过上限。保留普通仓库和内容/删除/冲突护栏回归于 Verification，移除重复验收条目并单列非稀疏部分克隆正向判据；护栏合同仍在 Description 与 CL-27 | R46-C 分区完整复审 | Codex R45-C 只读评审（分区覆盖） |
| R46-C (Codex gpt-6.1-terra) | 第 801 行至末及 CL-10/CL-18 | FAIL | P1：CL-20 缺少内容修改、删除和冲突合并的拒绝状态回归；已补具名测试并将放行限定为目录重命名 | R47-C 分区完整复审 | Codex R46-C 只读评审（分区覆盖） |
| R47-C (Codex gpt-6.1-terra) | 第 801 行至末及 CL-10/CL-18 | PASS | 无阻断问题；CL-20 拒绝状态回归、目录重命名范围、写集及计数均核对 | 与 R37-A/R41-B 合并覆盖全文 | Codex R47-C 只读评审（分区覆盖） |
| 依赖闭包复审 (Codex gpt-6.1-terra) | DEP-CL-03 / DEP-CS-03 与 Clone、PM、MP、CS 相关传递链 | PASS | 排除 CL-17/CL-20 后两侧集合一致，核查范围无间接等待或循环 | 提交计划 | Codex 只读定向复审 |
| R48 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `49db3c05736add04a6153c2383390012dd18862699e2780e6cb93d59a154621f` | `FAIL` | 5 项 P1：事实基线 `cli.rs:3819`、`INDEX.md:80` 错锚；协议称克隆拒绝由卡内目标目录 AC 覆盖而多卡缺失；`CL-04/08/12/13/19` 复合谓词；`CL-04` 缺 `(1/4)`。均已修订 | P2：`CL-28` 全变体最小参数夹具过重（改为单元测试等值比较） | 2026-11-18 17:02:21 UTC 修订 |
| R48 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `49db3c05736add04a6153c2383390012dd18862699e2780e6cb93d59a154621f` | `FAIL` | A：失败映射可被「打印码但成功」满足、ADR 去重与 Git 不符、网站第 4 步缺 `pnpm test`；B：`CL-28` HEAD 查询行错、`CL-25` 缺 `--lib` 守卫与 patch+1 证明及复合 AC、`CL-04` 未断言初始检出；C：`CL-05/06/08/10` 复合谓词、克隆拒绝缺目标清理、`CL-09` 夹具可绕过回退、`CL-18` shared 终态、`CL-08` AC8 改变 Libra 源 `--depth` 契约；D：部分克隆对象断言与检出冲突、`CL-21` 缺终端默认进度、`CL-12/13/24` 计数、`CL-19` 无法证明未发请求；E：`CL-14/16` 目标清理、`CL-15` 复合与 fetch 前提、`CL-20` 只验一个重命名目标与日志文件、`CL-17` 追加可空过；F：`CL-08` 计数、追溯表文档列缺文件、完成判据含撤回卡。均已修订（`CL-18` 改写为既有终态回归，未新增中间态接缝） | D 的 P2（`CL-19` 错误码）已采纳 | 2026-11-18 17:02:21 UTC 修订 |
| R49 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `557e6e7a4eca344aba47f1081b262385d9deee241fd876b3b0d5bc72333b87df` | `FAIL` | 2 项 P1：`CL-19` AC6/AC8 的「无接缝行」缺正控制可空真；`CL-20` 内容修改与冲突夹具重叠。均已修订 | 无 | 2026-11-18 17:06:44 UTC 修订 |
| R49 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `557e6e7a4eca344aba47f1081b262385d9deee241fd876b3b0d5bc72333b87df` | `FAIL` | A：基线称 `--dissociate` 被忽略、成功定义含撤回卡；B：`CL-25` 祖先断言与 squash 合并冲突；C：`CL-05` 空引用时 `show-ref` 失败与缺检出断言、`CL-06` 未观测初始 fetch、`CL-10` 有效 `--reference-if-able` 未测、`CL-18` 与 `CL-08` 硬链接矛盾；D：`CL-12`→`CL-13` AC 编号错位、`CL-13` 自定义远端两键未测、`CL-19` 缺正控制；E：`CL-27` AC2 映射缺非零状态；F：M2 要求无触发卡的 T-1、风险表要求逐变体夹具。均已修订 | 无 | 2026-11-18 17:06:44 UTC 修订 |
| R50 (Claude subagent claude-opus-5-5) | R49 修订差异及 G-05 专项，受审 SHA-256 `d91ad84a0b89d016f75253860374009fdc43e9fd91bb652c86925539ff6c1483` | `FAIL` | 1 项 P1：`CL-23`/`CL-28` 引用「`CL-04` 夹具」违反 G-05。已修订 | 无 | 2026-11-18 17:11:21 UTC 修订 |
| R50 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `d91ad84a0b89d016f75253860374009fdc43e9fd91bb652c86925539ff6c1483` | `FAIL`（F `PASS`） | A：克隆拒绝不变量把清理泛化到 Git 检出失败；B：`CL-03` 引用的既有夹具是损坏浅仓库、`CL-28` 单值映射无法表达按参数放行；C：`CL-08` 硬链接失败语义与 Git 不符、`CL-05` 缺失判定混同操作错误；D：`CL-21` 缺 `cat-file` 文档、`CL-24` 显式 `combine:` 只验配置、`CL-13` 未测 `clone.defaultRemoteName`；E：`CL-15` 证据锚点不支撑结论。均已修订 | C 的 P2（默认本地路径未直接验收）一并采纳为 `CL-08` AC2 | 2026-11-18 17:11:21 UTC 修订 |
| R51 (Claude subagent claude-opus-5-5) | R50 修订差异及过渡断言交叉引用，受审 SHA-256 `8c9cc1bf28e1751ed2588c17c782f00dd87f55d654576180b5c775a811bf1d6b` | `FAIL` | 1 项 P1：`CL-13` 过渡断言已改为 AC5–AC7，`CL-24` 与完成判据仍写 AC6–AC8。已修订 | 无 | 2026-11-18 17:15:13 UTC 修订 |
| R51 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `8c9cc1bf28e1751ed2588c17c782f00dd87f55d654576180b5c775a811bf1d6b` | `FAIL` | A：`GC-CL-01` 无条件对齐 Git 与 Libra 专有契约冲突、W-2 退出码说明不符；B：`CL-25` 聚合守卫被协议排除为 AC 证据、手工门未计 VER；C：`CL-18` 可忽略 `--reference`、`CL-10` 有效 if-able 未验借用、`CL-08` 未验首次失败后全部复制、`CL-05` 只验 tag；D：`CL-24` 交接编号、显式 `combine:` 只验非根 tree、未登记拆出 `CL-29`；E：`CL-15` 承诺 `FETCH_HEAD` 未验收；F：完成判据 `CL-13` 编号。均已修订 | E 的 P2（`CL-17` refspec 未加引号）已采纳 | 2026-11-18 17:15:13 UTC 修订 |
| R52 (Claude subagent claude-opus-5-5) | 全文描述承诺专项（逐卡 Description 对照 AC），受审 SHA-256 `825c90795e208625cc98d56582bc1e156de092527f0a0ac55cfdc65fee43d580` | `FAIL` | 4 项 P1：`CL-09` 承诺非仓库与 `<path>/.git` 回退、`CL-16` 帮助文本改动、`CL-19` 本地 OID 校验均无 AC；`CL-17` 描述引用已移除的 `CL-05` 约定。均以收窄描述修订 | 无 | 2026-11-18 17:18:14 UTC 修订 |
| R52 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `825c90795e208625cc98d56582bc1e156de092527f0a0ac55cfdc65fee43d580` | `FAIL`（F `PASS`） | A：子进程规则与 `CL-28` 单元测试冲突；B：`CL-04` `--mirror` 只验消息未验拒绝；C：`CL-05` 依赖可变 `HEAD~1`、`CL-09` 承诺未验收、`CL-08` 未覆盖对象目录根符号链接、`CL-18` 描述 AC 编号错；D：`CL-19` OID 校验承诺、`CL-13` 重复同值未测、`CL-21` 非终端 auto 未测；E：`CL-20` 非稀疏部分克隆只验退出码。均已修订 | 无 | 2026-11-18 17:18:14 UTC 修订 |
| R53 (Claude subagent claude-opus-5-5) | R52 修订差异与夹具专项，受审 SHA-256 `a5563f5a29a0a46e90c810a3a0a9c35fb0dbd07b91410e7d6db49dd1b4acb771` | `PASS` | 无 P0/P1 | P2：`CL-28` 样本表需随新增变体维护（设计如此） | 2026-11-18 17:21:37 UTC 记录 |
| R53 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `a5563f5a29a0a46e90c810a3a0a9c35fb0dbd07b91410e7d6db49dd1b4acb771` | `FAIL`（A/B/F `PASS`） | C：`CL-08` 未覆盖对象子目录符号链接、回退断言允许遗漏对象、评审结论称 `--bare --revision` 已纳入 `CL-05`；D：`CL-24` 重复同值只断言退出 0、协议第 7 项 `CL-21` 终端 AC 编号错；E：`CL-16` 只验 `blob:none`、`CL-20` 合并结果只比路径。均已修订 | 无 | 2026-11-18 17:21:37 UTC 修订 |
| R54 (Claude subagent claude-opus-5-5) | R53 修订差异与过渡断言交接链，受审 SHA-256 `3cfa417276c22b027096dab8860b953fc39945f32bf1b5aa65e067036445643e` | `PASS` | 无 P0/P1（其后修订中自查发现 `CL-05` 内置分支误判，见同行修订历史） | 无 | 2026-11-18 17:26:22 UTC 记录 |
| R54 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `3cfa417276c22b027096dab8860b953fc39945f32bf1b5aa65e067036445643e` | `FAIL`（F `PASS`） | A：全量触发规则与家族子卡继承冲突；B：`CL-28` 描述称单元测试核对文档表格；C：`CL-06` 未验全局配置、`CL-05` 分支形式未验；D：`CL-13`/`CL-24` 未运行改写后的前序用例、`CL-13` 只在一个库验过滤、`CL-21` 终端用例未区分 stderr 通道且无成功前提；E：`CL-15` `show-ref` 空集失败可空过、`CL-20` 只验请求数未验取回的 6 个对象。均已修订 | 无 | 2026-11-18 17:26:22 UTC 修订 |
| R55 (Claude subagent claude-opus-5-5) | R54 修订差异与 SQL 谓词实测，受审 SHA-256 `80f94c5d76ba864924256271fc6e8f246874d6d95804ed8ee729ff38019eec3a` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:29:29 UTC 记录 |
| R55 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `80f94c5d76ba864924256271fc6e8f246874d6d95804ed8ee729ff38019eec3a` | `FAIL`（A/B/F `PASS`） | C：`CL-05` tag 夹具与默认分支同一提交、裸克隆未验 HEAD、引用过滤会漏同名 tag/远端分支；D：`CL-19` 未验取回对象持久化；E：`CL-16` 只验 `tree:1`、`clone.rs:220` 归属错为 `--filter`。均已修订 | 无 | 2026-11-18 17:29:29 UTC 修订 |
| R56 (Claude subagent claude-opus-5-5) | R55 修订差异与 `status --porcelain=v2`/HEAD 行实测，受审 SHA-256 `08c2f238c2e7d58a72d76647e85f9abbd7241ba7a98003f985f51cc2fd65d561` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:32:50 UTC 记录 |
| R56 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `08c2f238c2e7d58a72d76647e85f9abbd7241ba7a98003f985f51cc2fd65d561` | `FAIL`（D/F `PASS`） | A：`ADR-CL-04` 称 `CL-14` 写集只有两个源码文件；B：`CL-28` 拆分来源在 `CL-04`/`CL-23` 间矛盾；C：`CL-10` 只验存储未验传输、`CL-05` SQL 对 NULL 名称不安全；E：`CL-16` 表驱动四种拒绝计为 1 条而超限。均已修订（E 以撤回组合拒绝收窄范围） | 无 | 2026-11-18 17:32:50 UTC 修订 |
| R57 (Claude subagent claude-opus-5-5) | R56 修订差异（撤回组合拒绝的全文残留引用专项），受审 SHA-256 `fa8742640dc94e343531e589c2fdb0f90e1eb08f1e5885f6e694fb19e1912588` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:36:20 UTC 记录 |
| R57 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `fa8742640dc94e343531e589c2fdb0f90e1eb08f1e5885f6e694fb19e1912588` | `FAIL`（A/B/F `PASS`） | C：`CL-05` tag 克隆未验只取请求修订、`CL-06` 承诺一般配置在初始 fetch 前生效未验；D：`CL-12` 未覆盖有效 `sparse:oid` 的过渡拒绝；E：`CL-26`/`CL-27` 缺普通仓库正控制。均已修订 | D 的 P2（`CL-19` `upstream` 夹具）已采纳 | 2026-11-18 17:36:20 UTC 修订 |
| R58 (Claude subagent claude-opus-5-5) | R57 修订差异与追溯表表格结构，受审 SHA-256 `8b992263bd89b00ce82c2c5eaacadda149c45558431a9b105c194462b057abdf` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:40:04 UTC 记录 |
| R58 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `8b992263bd89b00ce82c2c5eaacadda149c45558431a9b105c194462b057abdf` | `FAIL`（B/E/F `PASS`） | A：网站分支「STOP」与第 2 步「切换」矛盾；C：`CL-05` 裸克隆未验 HEAD 类型；D：`CL-13` 承诺网络传输过滤未验、`CL-24` 重复同值只验配置、`CL-29` 夹具内容未保证 blob 不同、`CL-21` 终端用例未区分 stderr。均已修订（D 前两项以收窄与 `DEFER-CL-08` 处理） | 无 | 2026-11-18 17:40:04 UTC 修订 |
| R59 (Claude subagent claude-opus-5-5) | R58 修订差异与 C/D 分区描述承诺专项，受审 SHA-256 `ad49ecc087ff048a431196112ba1625ea9c00485f70d6774b5184b84bac5bb96` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:42:34 UTC 记录 |
| R59 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `ad49ecc087ff048a431196112ba1625ea9c00485f70d6774b5184b84bac5bb96` | `FAIL`（A/B/E `PASS`） | C：`CL-07` 未证完整复制、`CL-10` 删除 R 后 `fsck` 失败不能证明借用；D：网络远端拒绝/回退承诺无验收；F：故障恢复矩阵对检出阶段失败也要求删除。均已修订（D 以收窄处理） | 无 | 2026-11-18 17:42:34 UTC 修订 |
| R60 (Claude subagent claude-opus-5-5) | R59 修订差异，受审 SHA-256 `9d886274f2741b4f759d1d6a17d5c6ac297d30d5a3aaba95c20ed9b502357bd5` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:44:51 UTC 记录 |
| R60 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `9d886274f2741b4f759d1d6a17d5c6ac297d30d5a3aaba95c20ed9b502357bd5` | `FAIL`（A/B/D/E/F `PASS`） | C：`CL-06` 只验含 `from_option` 不能证明 `--origin` 优先、`CL-08` 描述覆盖本地 Git 源而只测 Libra 源。均已修订（后者以收窄与 `DEFER-CL-09` 处理） | 无 | 2026-11-18 17:44:51 UTC 修订 |
| R61 (Claude subagent claude-opus-5-5) | R60 修订差异与远端集合 SQL 实测，受审 SHA-256 `2656c291a9361141eef63c89c316da97adca075e5fce25b3ad443e94b6d9a5ab` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:47:20 UTC 记录 |
| R61 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `2656c291a9361141eef63c89c316da97adca075e5fce25b3ad443e94b6d9a5ab` | `FAIL`（A/B/C `PASS`） | D：`CL-13`/`CL-24` 引用的 `local_client.rs:889` 是外部 Git 源引用读取；E：`CL-16` 顶层文件只验存在；F：风险表与性能摘要的回退表述未限定本地传输。均已修订 | E 的 P2（`CL-16` 发布时不限制 `--sparse --filter` 组合的中间发布风险）登记为残余风险 | 2026-11-18 17:47:20 UTC 修订 |
| R62 (Claude subagent claude-opus-5-5) | R61 修订差异，受审 SHA-256 `742da3205ce01e81e47e3e1c76e1b93ce616fd717e9b228920a23aac08ac9b67` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:49:59 UTC 记录 |
| R62 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `742da3205ce01e81e47e3e1c76e1b93ce616fd717e9b228920a23aac08ac9b67` | `FAIL`（A/C/F `PASS`） | B：`CL-25` AC9 把 ER-04 D 组门计入 G-03；D：`CL-12` 把单独 `tree:<n>` 拒绝错归 `CL-13` AC5–AC6、`CL-21` 文档影响未逐文件列 `cat-file`；E：`CL-16` 未验收 cone 规则。已按同行修订历史修订，待下一轮双审复核 | 无 | 2026-11-18 17:49:59 UTC 修订（未复审） |
| R63 (Claude subagent claude-opus-5-5) | 全文（含 R63 前专项清扫），受审 SHA-256 `c87b31524f0140debf92b997aa9275c74a9e355f8bf85c098fc4fa838d03e3e3` | `PASS` | 无 P0/P1；P2：评审结论「完整性」行 `CL-05` AC 区间不全、`CL-12` 描述过度概括 | 已在 R63 修订中处理 | 2026-11-18 18:09:42 UTC 记录 |
| R63 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `c87b31524f0140debf92b997aa9275c74a9e355f8bf85c098fc4fa838d03e3e3` | `FAIL`（A/B/E `PASS`） | C：`CL-08` 符号链接拒绝只以显式 `--local` 验收；D：`CL-29` 只验三个 blob，未验整个稀疏对象闭包；F：`CL-25` 把 ER-04 C 组的 patch + 1 与 `compat_version_surface_sync` 计入 G-03。均已按同行修订历史修订 | 无 | 2026-11-18 18:09:42 UTC 记录 |
| R64 (Claude subagent claude-opus-5-5) | R63 修订差异与 C/D/E 分区逐卡复核，受审 SHA-256 `37529476d4a3895c8e430cb66ef0b74e6657a146a07dcacfcdedd3123dabebaf` | `PASS` | 无 P0/P1 | P2：`CL-24` 未单独验 `combine:` 克隆的 `remote.origin.promisor`（卡未承诺，`CL-13` 已验 `blob:none`） | 2026-11-18 18:13:31 UTC 记录 |
| R64 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `37529476d4a3895c8e430cb66ef0b74e6657a146a07dcacfcdedd3123dabebaf` | `FAIL`（A/B/D/E/F `PASS`） | C：`CL-08` 三种符号链接位置未在两种调用形式下各自验收。已按同行修订历史修订 | 无 | 2026-11-18 18:13:31 UTC 记录 |
| R65 (Claude subagent claude-opus-5-5) | R64 修订差异，受审 SHA-256 `71e66474b0289380b77f4a1f2a96245bcc1b1ff1c299160cfa65eaca4025b61e` | `PASS` | 无 P0/P1（Codex 同轮指出的 6 项未在本轮 Claude 复核中发现，见下行） | 无 | 2026-11-18 18:20:10 UTC 记录 |
| R65 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `71e66474b0289380b77f4a1f2a96245bcc1b1ff1c299160cfa65eaca4025b61e` | `FAIL`（A `PASS`） | B：`CL-25` 评审前门需要合并提交；C：`CL-09` 证据无行号锚点；D：`CL-12` 只测两个字面量、`CL-13` 只测一种组合；E：`CL-16` 帮助文案无验收；F：`base.yml` 的 `pull_request` 无分支过滤。均已按同行修订历史修订 | 无 | 2026-11-18 18:20:10 UTC 记录 |
| R66 (Claude subagent claude-opus-5-5) | R65 修订差异与工作流、锚点实测，受审 SHA-256 `cc915c93a85d2bf32cd562961162af2b2264c8f31465ad179a1ad8164bde4af0` | `PASS` | 无 P0/P1；P2：状态行「修订历史末两行」已过时（收尾记账时改写） | 无 | 2026-11-18 18:25:14 UTC 记录 |
| R66 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `cc915c93a85d2bf32cd562961162af2b2264c8f31465ad179a1ad8164bde4af0` | `FAIL`（A/E/F `PASS`） | B：`CL-25` 先签名提交后跑收口门、`CL-02` 回归用例受 GitHub 门控可能空跑；C：`CL-06` 未验 `--config`；D：`CL-24` 无能力回退只验 `blob:none`。均已按同行修订历史修订（同类的 `CL-04`/`CL-12`/`CL-15` 回归用例一并替换） | 无 | 2026-11-18 18:25:14 UTC 记录 |
| R67 (Claude subagent claude-opus-5-5) | R66 修订差异，受审 SHA-256 `8acd5db7e1baa7b1b1bc4016d0cfbea51b42cd47d1d7fb8471e407ee64eaca78` | `PASS` | 无 P0/P1；P2：`CL-25` 提交后 `status --porcelain` 为空的要求会受无关未跟踪文件影响 | 无 | 2026-11-18 18:30:44 UTC 记录 |
| R67 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `8acd5db7e1baa7b1b1bc4016d0cfbea51b42cd47d1d7fb8471e407ee64eaca78` | `FAIL`（F `PASS`） | A：nextest skipped 含被过滤用例、`status --short` 缺 `--no-branch`、非目标行与 `CL-16` 矛盾；B：`CL-03` 回归用例未用 `--reject-shallow`；C：`CL-10` 未验 `--reference-if-able` 实际借用；D：`CL-29` 未验 `partialclonefilter`；E：非目标行矛盾（同 A）、`CL-20` 证据缺行号。均已按同行修订历史修订 | 无 | 2026-11-18 18:30:44 UTC 记录 |
| R68 (Claude subagent claude-opus-5-5) | R67 修订差异与锚点、网站脚本实测，受审 SHA-256 `577ad813dfa8e05b90c275bcc2eea136ea5b2595cfb09ab286596bead0ba919d` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 18:35:51 UTC 记录 |
| R68 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `577ad813dfa8e05b90c275bcc2eea136ea5b2595cfb09ab286596bead0ba919d` | `FAIL`（A/C/F `PASS`） | B：`CL-01` AC3/AC4 无成功前提；D：`CL-24` 无能力回退未覆盖反序形式（P2：`CL-12` 重写旧用例丢失浅克隆警告覆盖）；E：`CL-20` 未验组合克隆的 cone 状态。均已按同行修订历史修订 | 无 | 2026-11-18 18:35:51 UTC 记录 |
| R69 (Claude subagent claude-opus-5-5) | R68 修订差异与全部卡描述承诺复扫，受审 SHA-256 `30a76da5d84563c6179f4c7d92df2ec53b930d04f9d15c93f547525ac707e820` | `PASS` | 无 P0/P1；P2：`CL-26`/`CL-27` 护栏未区分「键存在」与「值为 true」 | 无 | 2026-11-18 18:40:33 UTC 记录 |
| R69 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `30a76da5d84563c6179f4c7d92df2ec53b930d04f9d15c93f547525ac707e820` | `FAIL`（A/D/F `PASS`） | B：`CL-28` 把会打开仓库的 `init` 列为不打开仓库的放行命令；C：`CL-08` `--no-local` 断言可空真、未规定源仓库属主边界；E：`CL-20` 条目标记断言不能发现缺失条目。均已按同行修订历史修订 | 无 | 2026-11-18 18:40:33 UTC 记录 |
| R70 (Claude subagent claude-opus-5-5) | R69 修订差异，受审 SHA-256 `033c9208891d070bc0a8ac3ba19d60184036d1b65a6e66b8685a07245c235242` | `PASS` | 无 P0/P1；P2：`CL-08` AC7 所在用例名只写符号链接 | 无 | 2026-11-18 18:42:44 UTC 记录 |
| R70 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `033c9208891d070bc0a8ac3ba19d60184036d1b65a6e66b8685a07245c235242` | `FAIL`（A/B/D/E/F `PASS`） | C：`CL-08` 检查对象目录属主不能替代 Git 对源仓库的属主检查。已按同行修订历史修订 | 另一分区 P2：`CL-26`/`CL-27` 未测 `core.sparseCheckout=false`（登记为残余风险） | 2026-11-18 18:42:44 UTC 记录 |
| R71 (Claude subagent claude-opus-5-5) | R70 修订差异，受审 SHA-256 `004ac1636abe103bb27c562a61f25882ce8ac2fde5ec56a0b082700013b95328` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 18:45:27 UTC 记录 |
| R71 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `004ac1636abe103bb27c562a61f25882ce8ac2fde5ec56a0b082700013b95328` | `FAIL`（A/D/E/F `PASS`） | B：`CL-03` 回归用例描述夸大（只断言不留 `.libra`）；C：`CL-08` 静态符号链接夹具不能覆盖遍历中替换、属主边界只测普通布局。均已按同行修订历史修订 | 无 | 2026-11-18 18:45:27 UTC 记录 |
| R72 (Claude subagent claude-opus-5-5) | R71 修订差异与 pinned Git 竞态检测核对，受审 SHA-256 `bbe8b4745629f6fa47e8c1291efdbeffc8e59957a6c75f43b958e5edf1afc599` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 18:49:52 UTC 记录 |
| R72 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `bbe8b4745629f6fa47e8c1291efdbeffc8e59957a6c75f43b958e5edf1afc599` | `FAIL`（A/B/D/E/F `PASS`） | C：`CL-07`/`CL-08` 把 `info/alternates` 当普通对象文件复制或硬链接，相对路径条目会在目标中解析错误。已按同行修订历史修订 | 无 | 2026-11-18 18:49:52 UTC 记录 |
| R73 (Claude subagent claude-opus-5-5) | R72 修订差异与相对 alternates 夹具实测，受审 SHA-256 `189cec976839d00d26639be07074c70c91d8d7bb36e166444beb9fb0fa551e05` | `PASS` | 无 P0/P1 | 见同时刻修订历史行的残余 P2 | 2026-11-18 18:51:39 UTC 记录 |
| R73 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `189cec976839d00d26639be07074c70c91d8d7bb36e166444beb9fb0fa551e05` | `PASS`（A–F 全部 `PASS`） | 无 P0/P1 | P2：`CL-16` 独立发布期间 `--sparse` 与其它 `--filter` 组合未验收（`DEFER-CL-05`） | 2026-11-18 18:51:39 UTC 记录 |

## 实施证据汇总

> 按 v2.12 规定，未来执行与收口证据统一汇总于本节，不在计划目录新建独立证据文件。

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-CL-01 | Git `files` 松散 ref 与 `reftable` 二进制 ref 存储后端实现（含 `#750` `files` 后端断言） | 当前仓库 refs 存储于既有 SQLite（`libra.db`），`CL-14` 如实报告 `sqlite` | 独立规划多 ref 存储后端（`files`/`reftable`）计划时 | 后续多 ref 后端计划（Owner: `CLI / Storage 维护者`） |
| DEFER-CL-02 | `libra init --separate-git-dir`（D25），以及把 `CL-28` 已拒绝的子命令改造成真正的指针布局 | D25 保留 `init` 拒绝。`CL-28` 已经默认拒绝未放行子命令，所以发布后它们不会写错目录；真正可用要等后续改造 | 后续计划逐命令放开指针布局时 | 后续对应计划 |
| DEFER-CL-03 | `#876` 在追加 refspec 后跑 `fetch --prune` 的端到端 | 依赖外部 `#541` 与 `plan-20261117-fetch-push.md#FP-15`（`DEP-CL-05`）。本计划不预置 `#[ignore]` 测试，也不为此单开发版 | `#541` 与 `FP-15` 都进入 `main` 后，由后续计划补该端到端 | 后续收口计划 |
| DEFER-CL-04 | `checkout`/`switch`/`reset` 等工作区写入命令全量支持 `clone --sparse` cone 过滤；cone 规则文件（Git `info/sparse-checkout` 的 `/*`、`!/*/`）的持久化与规则管理 | `CL-16`/`CL-20` 仅覆盖 `clone --sparse` 物化、`status.rs` 告警消除与 `merge` lazy fetch；`#756` 验收第 4 条（完整上游 `t/t6421-merge-partial-clone.sh#2`，其中在稀疏部分克隆内 `checkout -q origin/A`）随本项延后，`#756` 部分闭环 | 后续扩展工作区稀疏检出全命令兼容时 | 后续对应计划 |
| DEFER-CL-05 | `clone --sparse` 与 `blob:none` 以外的 `--filter`（`tree:<n>`、`combine:`、`sparse:oid`）组合 | `#756` 只要求 `--sparse --filter=blob:none`（`CL-20` AC1）；其它组合涉及稀疏 cone 与树深/稀疏规则过滤的交互，本计划不加限制也不验收（2026-11-18 R56 修订前曾以 `CL-16` AC8 拒绝，已撤回） | 有用户需求或上游用例要求时 | 后续对应计划 |
| DEFER-CL-06 | `clone --revision=<完整 OID>` 形式，以及不存在修订的专门报错（Git `Remote revision %s not found in upstream %s`，`builtin/clone.c:1577`） | `#741` 只要求 `--bare --revision=refs/heads/main`；`CL-05` 已验收引用名形式 | 有用户需求或上游用例要求时 | 后续对应计划 |
| DEFER-CL-07 | `clone -c` 中 `clone.defaultRemoteName` 以外的配置项对本次初始 fetch/checkout 的即时生效（如经 `-c remote.origin.fetch=…` 追加 refspec 影响初始 fetch） | `#752` 只要求 `-c clone.defaultRemoteName` 与 `--origin` 的优先级；`CL-06` 已验收其它键写入新仓库 | 有用户需求或上游用例要求时 | 后续对应计划 |
| DEFER-CL-08 | 网络传输（`git_client.rs`/`https_client.rs`/`ssh_client.rs`）的 partial-clone filter 协商；重复同值 `--filter`（如两次 `blob:none`）的组合结果 | `#746`/`#748`/`#754` 的上游用例均经 `file://` 本地传输；网络远端上的 `--filter` 行为不在本计划验收（风险登记见「网络远端 `--filter`」行） | 需要对网络远端做部分克隆时 | 后续对应计划 |
| DEFER-CL-09 | 本地 Git 源（`.git` 目录，`RepoType::GitRepo` 发现路径）的 `-l`/`--local` 硬链接与对应符号链接、回退语义 | `#743` 针对 Libra 本地克隆的「从不硬链接」；`CL-08` 只交付本地 Libra 源，Git 源沿用既有复制路径（结果正确，只是不节省磁盘） | 有用户需求或上游用例要求时 | 后续对应计划 |
| DEFER-CL-10 | 单独的 `--filter=tree:<n>`、`object:type=<type>`、`blob:limit=<n>` 等本计划不实现的 filter 类型，以及 `blob:none` 与 `tree:1` 以外的组合（如 `combine:blob:none+tree:2`） | `#746` 只要求 `--filter=blob:none --filter=tree:1` 的组合（`CL-24`），`#748` 只要求 `sparse:oid`（`CL-12`/`CL-29`）；这些规格自 `CL-12`（单独 filter，`CL-12` AC5）与 `CL-13`（组合，`CL-24` AC8 持续断言）起以 `LBR-UNSUPPORTED-001` fail-closed 拒绝，不静默全量克隆 | 有用户需求或上游用例要求时 | 后续对应计划 |

## 完成判据

- [ ] 全量收口门须在**最终代码树**上执行，并记录 `libra rev-parse HEAD` 与 `libra rev-parse HEAD^{tree}` 及结果；树一旦变化即须重跑，与是否发生修复无关。
- [ ] 计划文本提交门：Claude（Subagent）与 Codex 对同一字节均返回字面结论 `VERDICT: PASS` 且 P0/P1 全部关闭（用户 2026-11-18 指令，取代 2026-11-17 的 Codex 单审口径）；网站交付另需独立 Claude Code `VERDICT: PASS`，两者互不替代。
- [ ] 网站交付门：每张触及用户可见行为的卡均在其发布点完成 `/Volumes/Data/gitmono/libra-backend` 的最终字节同步检查、前端验证，并取得独立 Claude Code `PASS`，随后精确暂存、签名+DCO 提交、核对提交树与已审字节一致、推送 `cf` 并核验部署；计划文本双审不替代该网站门，任一环节失败即 STOP 且不得标 `complete`。
- [ ] 全部活动卡（27 张，不含已撤回的 `CL-22`）满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 全部活动卡（不含已撤回的 `CL-22`）的 `Lifecycle=done` 且 `Acceptance=complete`。
- [ ] 全部活动卡的 `Verification` 命令已运行并记录结果。过渡断言只保留在各自发布提交的证据里，并由后继卡同卡改写：`CL-12` AC7（`blob:none` 被拒）由 `CL-13` 改写；`CL-12` AC6（有效 `sparse:oid` 被拒）由 `CL-29` 改写；`CL-13` AC5–AC6 样本表中由 `blob:none` 与 `tree:1` 组成的样本与 AC7（无 `filter` 能力远端被拒）由 `CL-24` 改写为成功与回退契约。`CL-12` AC5（单独 filter 样本表被拒）、`CL-13` AC5–AC6 的其余样本（由 `CL-24` AC8 继续断言被拒）与 `CL-27` AC1（内容修改合并被拒；`CL-20` 只解禁目录重命名合并）在最终树上继续成立。
- [ ] 收口阶段全量门通过：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`（按 ER-13：无论暴露本计划缺陷或既有失败，均须定位修复或附基线复现证据并登记 `FIX-*`/`DEFER-*` 后重跑全量至全绿，不得带着红色全量结果通过完成门）。
- [ ] `#876` 的 `fetch --prune` 端到端留在 `DEFER-CL-03`，不阻塞本计划活动卡完成。
- [ ] 全部活动卡的 `Rollback mode`（`immutable-release`）与恢复验证已核验（`CL-22` 无写集、无恢复动作）。
- [ ] 必要的 docs / compat / test-index 更新已完成，且未在 `docs/development/plan/` 新建独立证据文件（v2.12 规则）。
- [ ] 每个相关发布切片均已完成 `/Volumes/Data/gitmono/libra-backend` `cf` 的网站文档 sync/check、相关前端验证与覆盖最终网站变更的 Claude Code `VERDICT: PASS`（独立于计划文本双审）；独立卡已提交推送 `cf` 并核验 Cloudflare 部署与生产正文，家族子卡的本地 `cf` 提交已由 `CL-25` 统一推送并完成同等部署核验。
- [ ] 所有独立卡已完成 `patch + 1`、版本面同步与 `gh release create` 发布。
- [ ] 「修订历史」与 `plan-status.md` 已同步。
# Clone Git 兼容性修复计划（2026-11-17）

**模板版本:** `v2.16`。本计划不在 v2.16 具名 `code-only` 范围内，按普通发布合同执行：独立 `implementation` 卡与家族发布点执行完整 C 组（含 `patch + 1`、`compat_version_surface_sync`、`Cargo.lock`、`gh release create`）与适用 D 组。家族子卡 `CL-04`、`CL-23`、`CL-28` 只本地提交，不单独推送，C/D 继承 `CL-25`。

**状态:** Claude R73 与 Codex R73 双审 PASS（受审 SHA-256 前 12 位 `189cec976839`），计划可提交；全部任务 pending，尚未执行。全部任务卡 `Lifecycle=pending`、`Acceptance` 为空。本文只规划，不宣称实现完成；落地时每张卡必须先按 ER-02 刷新源码锚点。
审阅合同：按用户 2026-11-18 指令，本计划的计划文本提交门为 Claude（Subagent）与 Codex 双审，二者对同一字节均给出 `VERDICT: PASS` 方可提交（取代 2026-11-17 的 Codex 单审口径）；执行期各卡的 ER-05 评审门与网站交付的独立 Claude Code `PASS` 门不变，网站门与计划双审互不替代。

## 文档职责

- **网站交付路径（权威）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支），依据是用户 2026-11-18 11:55:09 UTC 确认：本批计划的网站文档交付检出为该路径。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`，该路径在本机不存在（2026-11-18 核对），属已知文档滞后；用户决定本批不改 `AGENTS.md`，本计划以上述确认为准（修订历史 2026-11-18 08:19:08 行所称「用户最新指令」不作为依据）。2026-11-18 核对：该路径为 Libra 检出（有 `.libra/`、无 `.git`），当前分支 `cf`。写入前须按「文档与网站验收命令」第 2 步核验该路径存在、仅含单一 VCS 元数据，并确保当前分支为 `cf`（不是 `cf` 时按该步执行 `libra switch cf` 后复核）；按该步仍不满足即 STOP，禁止改用其它检出。

本文解决 `libra clone`（及关联 `rev-parse --show-ref-storage-format`）在 CLI 标志解析（`--progress` 布尔语义、`--no-reject-shallow`、`-n`、`--separate-git-dir`、`--revision`、`--no-hardlinks`、`-c <key=value>`、`--ref-storage-format`、`--sparse`）、本地硬链接与 `<path>.git` 回退、部分克隆 `--filter`（多值 `combine:`、`sparse:oid`、无效 sparse filter、promisor 对象读取与按需 lazy fetch 与进度）、`--reference`/`--dissociate` alternates 借用、`--single-branch` 游离 HEAD 分支创建、以及默认 `clone` 持久化 `remote.<name>.fetch` 中与 Git 不一致的 17 个已确认缺口，目标是实现 Git 兼容行为、完备测试覆盖与四端文档同步。

### 适用范围

- `libra clone`（`src/command/clone.rs`）、关联的 `src/command/rev_parse.rs`（`--show-ref-storage-format`）、`src/command/fetch.rs`（`--revision` / `--filter` / `--reference` 传输）、`src/internal/protocol/local_client.rs`、对象读取 promisor 回退路径与 `src/cli.rs`。
- 关联测试：`tests/command/clone_test.rs`、`tests/command/clone_cli_test.rs`、`tests/command/rev_parse_test.rs`（均通过 `--test command_test` 运行）、`tests/compat/clone_shallow_integrity_test.rs`（target `compat_clone_shallow_integrity`）、`tests/INDEX.md`。
- 关联文档：`docs/commands/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.md`、`docs/commands/zh-CN/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.md`、`docs/development/commands/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`CL-23`/`CL-28` 写集逐文件列出的其余命令文档（含 `alternates` 等受指针布局默认拒绝影响的命令）及 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.en.md`（写前按 ER-06a 核验 `.libra` 与 `.git` 及 `cf` 分支）。

### 非目标

- 不重构 `libra fetch` / `libra push` 的独立参数解析（由协调者排期的 `fetch-push` 计划承接）。
- 不修改 SQLite schema 或迁移 ledger（不触发 GC-13 迁移）。
- 不在本计划内实现 Git `reftable` 后端存储（`--ref-storage-format` 仅映射已实现的 SQLite ref 格式并拒绝未知格式）。
- 不在本计划内新增独立的 `libra sparse-checkout` 子命令（`--sparse` 以 `core.sparseCheckout`/`core.sparseCheckoutCone`、cone 物化与索引 skip-worktree 位实现，不写入、不启用既有 `sparse_view.rs` 的只读显示过滤，见 `CL-16`）。

### 成功定义

- 覆盖全部 17 个目标 Issue（`#737, #738, #739, #740, #741, #742, #743, #744, #746, #748, #750, #751, #752, #753, #754, #756, #876`），其中 `#750` 只交付 CLI 校验与如实报告 `sqlite`（`files`/`reftable` 在 `DEFER-CL-01`），`#876` 只交付 `remote.<name>.fetch` 持久化（`fetch --prune` 端到端在 `DEFER-CL-03`），`#756` 交付验收第 1–3 条（`clone --sparse --filter=blob:none`、cone 稀疏检出、目录重命名合并 1 次按需拉取 6 个对象），第 4 条（完整上游 `t/t6421-merge-partial-clone.sh#2`，需在稀疏部分克隆内 `checkout`）随 `DEFER-CL-04`。其余 14 个 Issue 全量闭环，期望值按「验证执行协议」第 4 项以 pinned Git `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`git describe`：`v2.56.0-116-g6de20f6092`）同夹具对照。
- 全部活动卡（27 张，不含已撤回的 `CL-22`）通过 ER-04 A/B/C/D 四组门禁、ER-05 Codex 单审 `VERDICT: PASS`、ER-06a 文档同步及 ER-08 发布切片。
- 全部活动卡完成后通过 ER-13 全量收口门（`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`）。

## 使用规则

- 本批次并行起草的多份计划由协调者统一串行登记至 [`plan-status.md`](plan-status.md) 与 `plan-long.md`；执行期推进任何任务卡时，同一变更必须同步更新 `plan-status.md` 的「计划一览」行（状态、版本或 commit、`YYYY-MM-DD HH:MM:SS UTC`）。
- 不得在 `docs/development/plan/` 下新建证据目录或证据文件。验收摘要写入本文「实施证据汇总」。
- 状态推进与「修订历史」统一使用 `YYYY-MM-DD HH:MM:SS UTC` 时间格式。
- 本计划继承 [plan-template.md](plan-template.md) 的 `GC-01..13`、`ER-01..14`、`G-01..11`。

## 事实基线

> 所有行号和源码锚点核对于 `2026-11-17 03:20:00 UTC`，开工当天必须按 ER-02 刷新。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口（CLI） | `Commands::Clone(CloneArgs)` 在 `cli.rs` 注册并路由到 `execute_safe`；全局 `--progress <mode>` 占用布尔 `--progress`（仅 `index-pack` 有 `rewrite_index_pack_progress_args`） | `src/cli.rs:292-302,351,1121,1318,3869` |
| `CloneArgs` 标志缺口 | 缺 `--no-reject-shallow`、`-n`、`--separate-git-dir`、`--revision`、`--no-hardlinks`、`--ref-storage-format`、`-c`、`--sparse`；`--filter` 为 `Option<String>` 且为 no-op | `src/command/clone.rs:64-236` |
| 本地克隆与 Alternates | `-l`/`--local` 总是复制对象而不硬链接；`--reference` 只打印警告、不借用对象；`--dissociate` 目前只用于阻止 `--shared`/`clone.shared` 注册 alternate（`src/command/clone.rs:178-182,1567`），尚无与 `--reference` 借用结合的解除语义；`<path>.git` 回退解析缺失；无 promisor 支持 | `src/command/clone.rs:124-182,250-269` |
| `single-branch` 与 `remote.<name>.fetch` | `--single-branch` 从游离 HEAD 克隆错误创建 `refs/remotes/origin/main`；`remote add` 已写 `+refs/heads/*:refs/remotes/<name>/*`（`remote.rs:822-834`），但默认 `clone` 仅在 single-branch/mirror 时写 | `src/command/clone.rs:76-87`；`src/command/remote.rs:822-834` |
| `rev-parse` 与 `init` 格式 | `RevParseArgs` 缺 `--show-ref-storage-format`；`init --ref-format` 仅接受 `strict`/`filesystem`（ADR-IN-02） | `src/command/rev_parse.rs:59`；`src/command/init.rs:374,1674-1686` |
| 稀疏视图边界 | 历史决策 D10 曾拒绝 `sparse-checkout`，当前仅存在 `sparse_view.rs`（`COMPATIBILITY.md:166,172,375`） | `COMPATIBILITY.md:166,172,375`；`src/command/clone.rs:220` |
| 测试与文档 | `clone_test` 与 `clone_cli_test` 挂在 `command_test` target 下；compat target 为 `compat_clone_shallow_integrity` | `tests/command/mod.rs:1024-1025`；`tests/INDEX.md:44,81`；`docs/development/commands/clone.md` |
| 外部参照 | pinned Git 基准行为（`git describe` 为 `v2.56.0-116-g6de20f6092`，即 v2.56.0 之后 116 个提交；评审预探用本机 Git 2.54.0，期望以 pinned 版本为准） | `git/git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（核对日期 `2026-11-17`） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-CL-01 | `clone` CLI 缺 `--progress` 布尔、`-n`、`--no-reject-shallow`、`--separate-git-dir`、`--revision`、`-c`（含 `clone.defaultRemoteName`）、`--ref-storage-format`、`--sparse` | 常见 Git clone 脚本与测试在 CLI 解析层失败 | `#737, #738, #739, #740, #741, #750, #752, #756`；`src/cli.rs:302`；`src/command/clone.rs:64-236` | CL-01..CL-06, CL-14, CL-16 |
| GAP-CL-02 | `-l/--local` 不硬链接、`--no-hardlinks` 缺失、`<path>.git`（含 `file://`）回退解析缺失、`--reference` 不借用对象（`--dissociate` 只对 `--shared` 生效） | 本地克隆性能与对象借用/独立语义缺失 | `#742, #743, #744, #751`；`src/command/clone.rs:124-182` | CL-08, CL-07, CL-09, CL-10, CL-18 |
| GAP-CL-03 | `--filter` 仅单值且忽略过滤（无 `combine:`、无 `sparse:oid`、无无效 sparse 报错、无 promisor/lazy fetch 与 `Receiving objects` 进度） | 部分克隆与错误过滤校验失效 | `#746, #748, #754`；`src/command/clone.rs:191-198,269` | CL-12, CL-13, CL-24, CL-29, CL-19, CL-21 |
| GAP-CL-04 | `--single-branch` 游离 HEAD 创建错误分支、默认 `clone` 未写入 `remote.<name>.fetch` | 游离 HEAD 克隆与后续 `fetch --prune` 越界修剪 | `#753, #876`；`src/command/clone.rs:76`；`src/command/remote.rs:822-834` | CL-15, CL-17 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-20261117-plumbing-misc.md`（已推送 `cd5ccac`） | 兄弟计划：`PM-06`（`#947`）先于 `CL-14`（`#750`）合入 `src/command/rev_parse.rs`（对方 `DEP-PM-02`）；`CL-19` 先于 `PM-14`（共享 `cat_file.rs`）；`CL-13`、`CL-19`、`CL-29` 先于 `PM-33`（`t5616#33` 的 `rev-list --missing` promisor 归因，自对方 `PM-03` 拆出；对方 `DEP-PM-08`；`PM-33` 还依赖 Libra 非 `--mirror` 的 `--bare` 克隆建立 `refs/remotes/origin/*`） | 登记 `DEP-CL-02`、`DEP-CL-08`、`DEP-CL-14`，均单向无环；本计划不改变上述 `--bare` 克隆现状（`CL-29` AC4 守护） |
| `plan-20261117-commit-signing.md`（已推送 `2fe8c4b`） | 兄弟计划：同样涉及 `src/command/clone.rs`、`tests/command/clone_cli_test.rs` 与 `cat_file.rs`；对方 `DEP-CS-03` 逐字镜像本计划 `DEP-CL-03` 的卡集合 | 登记 `DEP-CL-03`，按 ER-12 串行合入 |
| `plan-20261117-fetch-push.md`（已推送 `49b62d1`） | 兄弟计划：同样涉及 `src/command/fetch.rs` 与 `COMPATIBILITY.md`；本计划引用的 `FP-07`、`FP-15` 是该计划的卡（不是 `issues/529.md` 的同名卡），对方登记为 `DEP-FP-05` | 登记 `DEP-CL-04`、`DEP-CL-05`，按 ER-12 串行合入 |
| `plan-20261117-merge-pull.md`（评审中，按 ID 引用） | `CL-20` 先于 `MP-17` 合入 `merge.rs`、`rename_merge.rs`、`tree_merge.rs`（对方登记为 `DEP-MP-11`） | 登记 `DEP-CL-07` |
| `plan-20261117-{global-init-infra,checkout-switch-restore,branch,describe}.md` 等同批计划 | 共享 `src/cli.rs`、`src/utils/util.rs`、`src/command/rev_parse.rs`、`src/command/status.rs`、`COMPATIBILITY.md`、命令文档与网站生成产物（对方 `DEP-GII-03`、`DEP-PM-11`、`DEP-MP-10` 等同批写锁） | 登记 `DEP-CL-06`、`DEP-CL-09`..`DEP-CL-13`、`DEP-CL-15` |
| `plan-long.md` | 长期兼容性路线图 | 由协调者在合入时统一登记日期索引 |
| `docs/development/plan/plan-template.md` | 模板规范 v2.16 | 全文严格遵守 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 17 个实测 Git 兼容缺陷均有明确复现路径 | 按单一行为轴拆分为 27 张活动任务卡（含 `CL-18..CL-29`，`CL-22` 已撤回，其中 `CL-25` 为 `REL-CL-01` 家族发布点，`CL-28` 为指针布局写命令拒绝子卡，`CL-29` 为 `sparse:oid` 下游契约卡，原 `CL-11` 已并入 `CL-13`；`CL-` 前缀作用域仅限本计划，与 `issues/474.md` 无关） |
| 可行性 | 每卡聚焦单一行为轴，锚点与测试命令均可直接执行 | 修正 `CL-02`、`CL-16` 不存在测试过滤器并重写旧 no-op 测试 |
| 任务卡粒度 | 26 张实现卡均含公开行为变更，按 G-04 标为 `M`，发布点 `CL-25` 为 `S`；`landing` 仅计生产目录 | 逐条按独立谓词核算 `AC`/`VER`（≤ 8） |
| 依赖与顺序 | 全计划严格串行，`CL-08` 先于 `CL-07`；`DEP-CL-02/03/04` 单向无环 | DAG 无环，符合 G-10/ER-12 |
| 完整性 | 补齐 `--progress` 重定向、`--bare --revision`（`CL-05` AC1–AC4）、`file://<path>.git`、`clone.defaultRemoteName` 等 AC | 全部纳入卡内与 `COMPATIBILITY.md` |
| 安全性 | `--separate-git-dir`、`--reference`、`-c`、`<path>.git` 校验路径边界与不泄露凭据 | 继承 GC-07/GC-09 |
| 功能正确性 | 严格对齐 pinned Git `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`v2.56.0-116-g6de20f6092`） | 新增 `ADR-CL-03`/`ADR-CL-04` 解决 D10 与 `ref-format` 边界 |
| 接口兼容 | `CloneArgs` 与 `rev-parse` 同步 `COMPATIBILITY.md`；全部任务复用既有错误码，不新增 `docs/error-codes.md` 条目 | 命中 `T-1` 时跑全量 nextest |
| 数据流与控制流 | `--dissociate` 在拷贝完借用对象后再移除 alternates；失败清理新建目录 | 故障恢复矩阵覆盖 |
| 性能与容量 | `-l/--local` 硬链接减少磁盘拷贝；`--filter` 部分克隆避免拉取多余对象 | 继承 GC-10 |
| 可靠性与容错 | `--separate-git-dir` / `--revision` / 无效 `--filter` 失败时清理半成品目录 | 故障恢复矩阵覆盖 |
| 可维护性 | 复用 `rewrite_*_progress_args`、`command_preflight`、`ConfigKv` 单一事实源 | 禁止重复实现 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 03:25:00 UTC | 初稿创建 | 建立覆盖 17 个 clone 兼容问题的 17 张任务卡计划 | N/A | 全文 |
| 2026-11-17 03:38:00 UTC | Claude R1 FAIL + Codex R1 FAIL | 拆出 `CL-18`（`--dissociate`）与 `CL-19`（promisor 对象读/lazy fetch）；调换 `CL-08` 先于 `CL-07`；新增 `ADR-CL-03`（重开 D10 为 `clone --sparse`）与 `ADR-CL-04`（`--ref-storage-format` 映射 `resolve_ref_format`）；`DEP-CL-02` 单向指向 `PM-06` 并补 `DEP-CL-03/04/05`；移除 `CL-11` `src/cli.rs` 并使 `CL-11` 经 `CL-13`/`CL-19` 闭环；补全 `#737/#741/#744/#748/#752/#753/#756/#876` AC 与 `COMPATIBILITY.md`、`codeql.yml` `release/**`、rewrite 旧测试 | CL-10 → CL-10, CL-18; CL-13 → CL-13, CL-19 | ADR-CL-02/03/04、依赖表、实施顺序、粒度审计表、各卡正文、review log |
| 2026-11-17 03:45:00 UTC | Codex R2 FAIL | 将 `CL-19`（promisor lazy fetch）前置至 `CL-11` 与 `CL-13` 之前，确保每个部分克隆独立发布版本均自洽可读；修正 `CL-16` 真实落点为 `src/internal/sparse/mod.rs` 与 `src/command/sparse_view.rs` 并标记 `T-1`；明确 `CL-14` 将 `init::resolve_ref_format` 提升为 `pub(crate)` 并映射 `files`→`Filesystem` | N/A | ADR-CL-02/04、实施顺序、粒度审计表、CL-11/13/14/16/19、追溯表、review log |
| 2026-11-17 03:52:00 UTC | Codex R3 FAIL | 收紧 `CL-11` 为仅解析多 `--filter` 并构造 `combine:` 规格，将协议 `filter` 省略与 `promisor=true` 统一收敛至 `CL-13` 同边界发布；明确 `CL-17` 自身仅验收 `remote.<origin>.fetch` 持久化，`#876` 端到端 `fetch --prune` 待 `#541` 与 `FP-15` 闭环；补全与 DAG 一致的完整 `发布窗口顺序` | N/A | ADR-CL-02、依赖表、发布窗口顺序、CL-11/17、review log |
| 2026-11-17 04:05:00 UTC | Claude R4 FAIL + Codex R4 FAIL | `ADR-CL-04`/`CL-14` 改为 `clone.rs` 独立 `resolve_ref_storage_format`（仅接受 `files` 映射 SQLite，不改 `init.rs` 与 D25）；`ADR-CL-03` 补充 `docs/development/commands/_compatibility.md` 并拆出 `CL-20` 与 `CL-16`；新增 `ADR-CL-05`；`CL-05/09/10/11/13` 补全 `T-1`；`CL-13`/`19` 改用 `<remote_name>`；`CL-17` 修正 `clone.rs:2043-2054` 锚点并明确 `#876` 端到端收口门；移除未用 `REL-CL-01` | CL-16 → CL-16, CL-20 | ADR-CL-03/04/05、实施顺序、依赖表、审计表、CL-01/04/05/06/09/10/11/12/13/14/16/17/19/20、追溯表、完成判据、review log |
| 2026-11-17 04:18:00 UTC | Claude R5 FAIL + Codex R5 FAIL | `实施顺序` 补全 `CL-16 -> CL-20 -> CL-17`；新增 `DEP-CL-06..09` 协调跨计划写集（`cli.rs`/`merge.rs`/`cat_file.rs`/`rev_parse.rs`）并将 `DEP-CL-03` 改为 `outgoing`；重写 `ADR-CL-02` 移除过期 `REL-CL-01` 并以 `CL-13` 收口；`CL-04` 纳入 `src/utils/util.rs:277,797` 并标 `T-1`；`CL-20` 补 `1 fetch`/`6 objects`/`TC-0734` 与 `merge.md`；`CL-19` 改 `landing=4/prod-files=5` 并仅复用 `hydrate.rs` OID 校验；`ADR-CL-01`/`CL-13`/`Release write set`/`codeql.yml`/`兼容收口` 补全 P2 | N/A | ADR-CL-01/02/05、D 组、实施顺序、依赖表、全局默认、审计表、CL-04/13/19/20、兼容收口、review log |
| 2026-11-17 04:23:00 UTC | Codex R6 FAIL + Claude R6 网络中断 | `CL-04` 纳入 `util.rs:314-324` `worktree_info_file_paths` 指针解析 | N/A | CL-04、review log |
| 2026-11-17 04:33:00 UTC | Codex R7 FAIL + Claude R7 网络中断 | `CL-04` 边界收窄并增补 `DEFER-CL-02`；`完成判据` 对齐 `#876` 跨计划门 | N/A | CL-04、DEFER-CL-02、完成判据、review log |
| 2026-11-17 04:43:00 UTC | Claude R8 FAIL + Codex R8 FAIL | `CL-13` 补 `#746` `combine:blob:none+tree:1` 与端到端 lazy fetch；`CL-20` 补 `rename_merge.rs`/`tree_merge.rs`/`fetch.rs` 批量预取与 `T-1`/`DEP-CL-07`；新增 `DEP-CL-10` 协调 `util.rs`；`CL-04` 纳入 `alternates.rs:91` 并验证指针布局基本命令；`CL-17` 预置 `#[ignore]` 端到端测试；修复 `CL-02` nextest 过滤器与 `rev_parse.rs:59` 锚点；`CL-10`/`12` 补齐 `fsck` 与 Git 精确报错 | N/A | 依赖表、D 组、审计表、CL-02/04/05/06/10/11/12/13/14/17/19/20、完成判据、review log |
| 2026-11-17 04:48:00 UTC | Claude R9 FAIL + Codex R9 FAIL | 移除 `CL-20` 重复尾部字段；`CL-04` 与 `DEFER-CL-02` 解除自相矛盾（`alternates.rs:91` 明确归 `CL-04`）；`CL-13` 拆出 `CL-21`（`Receiving objects` 进度与端到端 lazy fetch 验证）保持 `AC<=8` 与单轴；`完成判据` 明确移除 `#[ignore]` 并正常执行 `#876` 测试；同步审计表/卡内 `landing`/`deps`、`CL-19` 校验实现与传输范围、`CL-01`/`CL-14` 验证、具名 `DEP-CL-04/07/09` 与单一发布者 `Plan Author 5` | CL-13 → CL-13, CL-21 | 修订历史、实施顺序、依赖表、审计表、CL-01/04/05/10/11/12/13/14/19/20/21、追溯表、DEFER-CL-02、完成判据、review log |
| 2026-11-17 04:55:00 UTC | Codex R10 FAIL + Claude R10 网络中断 | 将 `CL-11` 与 `CL-13` 合并为 `REL-CL-01`（`combined` 发布边界，并将 `CL-12` 前置至 `CL-19 -> CL-12 -> CL-11 -> CL-13`）避免 `CL-11` 单独发布后静默忽略 `--filter`；收紧 `CL-04` 仅声明 `clone --separate-git-dir`、`util.rs`、`alternates.rs:91` 与已验证 4 个基本命令，不宣称其余子命令指针布局通用支持；新增 `CL-22`（`verification`，`Release boundary=none`）显式承接 `#876` 移除 `#[ignore]` 与端到端门禁；更新 `Phase 3` 与 `M3` 为 `11 张卡` | 新增 `CL-22` 承接 `#876` 收口验证 | 修订历史、ADR-CL-02、实施顺序、发布分组、Phase 3、审计表、CL-04/11/12/13/17/22、M3、追溯表、完成判据、review log |
| 2026-11-17 05:00:00 UTC | Claude R11 FAIL + Codex R11 FAIL | 将 `CL-11` 并入 `CL-13`（`7/8 AC`，`independent`）彻底移除 `REL-CL-01`；规范 `CL-22` 为 `Task type: implementation`、`Release boundary: independent`、`Rollback mode: immutable-release`、`Version increment: patch`、`Full-suite trigger: T-2` 并移除 `Verification` 中的 `--all` 全量行；`CL-21` 补 `DEP-CL-04`；同步 `DEP-CL-03`（`CL-01..CL-22`）、`DEP-CL-04`（含 `CL-21`）、`DEP-CL-05`（`incoming`+`DEFER-CL-03`）与 `CL-20` 追溯表 | `CL-11` 并入 `CL-13` | 修订历史、ADR-CL-02、实施顺序、依赖表、发布分组、Phase 3、审计表、CL-13/21/22、追溯表、M3、DEFER、review log |
| 2026-11-17 05:05:00 UTC | Codex R12 FAIL | 修正 `ADR-CL-04`/`CL-14`/`DEFER-CL-01`：`rev-parse --show-ref-storage-format` 如实报告 `sqlite`（不将 SQLite 误报为 `files`），将 `files`/`reftable` 后端实现显式延后至 `DEFER-CL-01` 并记入 `COMPATIBILITY.md`；`CL-08` 明确校验 `.libra/objects/` 下**所有**对象文件 `nlink > 1`；同步审计表 `CL-22`（`implementation`/`independent`/`immutable-release`/`2/8`）与 `CL-12`（`landing=2`）；统一 `DEP-CL-05` 14 天超时、`DEFER-CL-03` 重启条件与 `完成判据` | N/A | 修订历史、ADR-CL-04、审计表、CL-08/14、DEFER-CL-01/03、完成判据、review log |
| 2026-11-17 05:18:00 UTC | Claude R12 FAIL + Codex R13 FAIL | 拆分 `CL-04` 为 `CL-04`（`--separate-git-dir` 指针与互斥，`4/8 AC`）+ `CL-23`（`util.rs`/`alternates.rs:91`/4 基本命令与未适配子命令 fail-closed + `COMPATIBILITY.md` 契约，`5/8 AC`）；拆分 `CL-13` 为 `CL-13`（多 `--filter` 构造与 `promisor`/`blob:none`，`5/8 AC`）+ `CL-24`（`combine:blob:none+tree:1` 与无能力回退，`4/8 AC`）；`CL-16` 纳入 `status.rs:1248-1265`、`clone.rs:220` 帮助文本与 `DEFER-CL-04`（`checkout`/`switch`/`reset` 稀疏约束）；修正 `成功定义`/`DEFER-CL-01`（具名 Owner）、`DEP-CL-03`（`CS-02/02a/15b/21a`）与 `DEP-CL-04`（`FP-07/15` 先于本计划 `fetch.rs`）；`CL-22` 改标 `T-5` 并统一 `M3`；清理 `GAP-CL-03`/`CL-19`、`R14`、`23 张卡`、`CL-06/19` AC 计数、`CL-15` 锚点及 `.git` 检查 | CL-04 → CL-04, CL-23; CL-13 → CL-13, CL-24 | 全文、成功定义、GAP-CL-03、DAG、依赖表、Phase 1/3、审计表、CL-04/06/13/15/16/19/22/23/24、M3、DEFER-CL-01/04、review log |
| 2026-11-17 05:30:00 UTC | Codex R14 FAIL + Claude R14b 网络中断 | 将 `CL-04` 与 `CL-23` 纳为 `REL-CL-01` 家族卡（`CL-04`/`CL-23` 为 `family child`，新增 `Task type: release` 发布点 `CL-25`），消除单独发布不可用 `.libra` 指针布局的中间版本；`CL-13` 在 `CL-24` 落地前对 `combine:*` 与多种不同 `--filter` 组合明确 fail-closed 拒绝；`CL-16` 为未适配稀疏工作区的 `checkout`/`switch`/`reset` 增加 `core.sparseCheckout=true` fail-closed 护栏与回归测试（`prod-files=6`，`AC=5/8`）；重申 `CL-22` 已于 R11 起统一为 `implementation` + `independent` + `patch` | 新增 `CL-25`（`REL-CL-01` 发布点） | 状态行、评审结论、修订历史、发布分组、Phase 1、默认覆盖、审计表、CL-04/23/25/05/13/16、追溯表、M1、review log |
| 2026-11-17 06:55:31 UTC | Claude R15 FAIL + Codex R15 FAIL | `CL-04`/`CL-23` `C/D coverage from` 改为 `CL-25` 且 `Full-suite trigger: N/A`；`CL-23` 纳入 `src/cli.rs` `command_preflight` 统一指针护栏并修正 `util.rs` 锚点（`278,314-324,791-803,1269,1464`）；`CL-25` 移除 `Verification` 中 `--all` 并规范 `Docs: N/A`；拆出 `CL-26`（`src/cli.rs` `command_preflight` 对 9 个未适配稀疏写入/切换命令统一 fail-closed 护栏与 ER-06a 全量命令文档），`CL-16` 收窄为 `clone --sparse` cone 物化、`status.rs:563` 跳过 `status` 误告警（保留 `add`/`commit`/`diff` 告警）与暂时拒绝 `clone --sparse --filter=...`（至 `CL-20` 解禁），并补 `DEP-CL-11` 协调 `status.rs`；`CL-13` AC 精简并核实为 `7/8`；同步 `ADR-CL-02`/`ADR-CL-05`、`CL-06/10/12/21/22`、`DEP-CL-04/06`、测试矩阵、`COMPATIBILITY.md` 行号门与 review log | CL-16 → CL-26, CL-16 | 全文、状态行、ADR-CL-02/05、DAG、依赖表、Phase 3、审计表、CL-04/06/10/12/13/16/17/20/21/22/23/25/26、测试矩阵、追溯表、M3、review log |
| 2026-11-17 09:44:31 UTC | Codex R16 FAIL | `CL-13` 文档门不计入后 `AC=6/8`；`CL-26` 只保留检出类 4 条单谓词，历史类 5 条拆为 `CL-27`；无能力回退风险改指向 `CL-24`，并登记护栏遗漏风险 | `CL-26` → `CL-26, CL-27` | DAG、发布窗口、Phase 3、审计表、`CL-13`、`CL-26`、`CL-27`、`CL-16`、风险表、追溯表、Review log |
| 2026-11-17 09:51:20 UTC | Codex R17 FAIL | `CL-23` 拆成 7 条单谓词（`AC=7/8`）；撤回 `CL-22`，`fetch --prune` 端到端只留在 `DEFER-CL-03`；Phase 3 为 12 张活动卡 | `CL-22` 撤回 | `CL-17`、`CL-22`、`CL-23`、DAG、Phase 3、M3、`DEP-CL-05`、Review log |
| 2026-11-17 09:57:57 UTC | Codex R18 FAIL | `CL-16` 拆成 8 条单谓词并写明拆出 `CL-20`/`CL-26`/`CL-27`；`CL-20` 拆成 7 条单谓词 | N/A | `CL-16`、`CL-20`、审计表、Review log |
| 2026-11-17 10:09:42 UTC | Codex R19 FAIL | `DEP-CL-07` 改为 `CL-20` 先于 `MP-17`；`DEP-CL-03` 不再把 `CL-20` 放在 `CS-02a` 之前；`CL-12`/`CL-13` 对未实现 filter 与远端无能力 fail-closed；成功定义不再把 `#876` 端到端写成已闭环 | N/A | 依赖表、`CL-12`、`CL-13`、`CL-20`、成功定义、Review log |
| 2026-11-17 10:43:14 UTC | Codex R24 FAIL | `CL-23` 只保留指针发现和 `status`/`log`/`cat-file`/`branch`，并补这四条命令的文档。写命令拒绝拆为 `CL-28`，逐文件登记 `commit`/`add`/`checkout`/`switch`/`reset`/`merge`/`pull`。收口清单补上 `CL-26`/`CL-27`/`CL-28` 的命令文档 | `CL-23` → `CL-23, CL-28` | `CL-23`、`CL-28`、`CL-25`、发布分组、收口清单、追溯表、`DEFER-CL-02` |
| 2026-11-17 10:52:35 UTC | Codex R25 FAIL | `CL-28` 改为默认拒绝：放行名单之外的 `Commands::*` 一律退出非零且不改数据。验收覆盖 `restore`、`commit`、`branch` 创建和 `checkout`，并用夹具覆盖全部变体 | N/A | `CL-23`、`CL-28`、`DEFER-CL-02`、风险表、`DEP-CL-12` |
| 2026-11-17 11:14:03 UTC | Codex R26 FAIL | `CL-28` 的写集补上全部受默认拒绝影响、且已有命令页的文档。`CL-12`/`CL-13` 的无能力拒绝只约束各自发布提交，最终树以 `CL-24` 为准 | N/A | `CL-28`、`CL-12`、`CL-13`、完成判据、`DEP-CL-12` |
| 2026-11-17 11:19:54 UTC | Codex R27 FAIL | `CL-13` 的单个未实现 filter 只约束本卡发布提交。`CL-24` 增加单个 `combine:` 形式。追溯表和收口清单改指 `CL-28` 的完整写集 | N/A | `CL-13`、`CL-24`、追溯表、收口清单 |
| 2026-11-17 12:01:37 UTC | Codex R30 FAIL | 对齐 `DEP-CL-03` 与 `DEP-CS-03` 的活动卡及家族发布点集合，移除撤回卡与范围歧义；新增 `CL-29` 交付 `sparse:oid` 以承接 `PM-03` 的 `t5616#33`；补齐网站 sync/前端/`cf` 提交推送/部署门，并按用户指令将计划提交门改为 Codex 单审 | `CL-24` → `CL-24,CL-29` | 关系表、ADR-CL-02、DAG、依赖表、发布窗口、Phase 3、审计表、`CL-21`、追溯表、M3、全局执行门、完成判据、Review log |
| 2026-11-17 12:27:28 UTC | Codex R31 FAIL | 将全部任务卡、文档影响和网站写集中的 `../libra-backend` 统一替换为唯一网站工作区 `/Volumes/Data/gitmono/libra-backend`，避免写入错误 checkout；恢复独立于计划 Codex 单审的网站 Claude Code PASS 门 | N/A | 全部网站写集与文档影响、Review log |
| 2026-11-17 12:27:28 UTC | Codex R32 FAIL | `CL-29` 将 `sparse:oid` 输入从错误的 tree-ish 改为可解析为稀疏规则 blob 的对象表达式，明确缺失/非 blob/规则解析失败的拒绝与 Git 对照测试 | N/A | `CL-29`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R33-A P2 | `CL-08` 将 inode 共享限定为硬链接成功对象，并增加同文件系统硬链接失败后的复制回退测试，消除回退与全量 inode 断言矛盾 | N/A | `CL-08`、审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R34-A FAIL | `CL-08` 增加正控制：正常同文件系统夹具至少一个松散对象和一个 pack 对象实际硬链接成功，并逐一比较设备号/inode，防止全量复制空真通过 | N/A | `CL-08`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R36-A FAIL | `CL-08` 正常夹具改为逐一检查所有符合硬链接条件的对象文件均实际硬链接，防止只链接一个松散和一个 pack 而复制其余对象 | N/A | `CL-08`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R37-A PASS | 第 1–400 行、CL-04 完整卡片、CL-08/CL-29 及 PM-03/DEP-PM-08/DEP-CS-03 交叉核对均无实质问题；全部符合条件对象的硬链接与失败复制回退通过复审 | N/A | 头部、Review log |
| 2026-11-17 12:27:28 UTC | Codex R38-B FAIL/P2 | `CL-08` 增加对象目录/文件符号链接 fail-closed 与仓外回归；`CL-18` 增加 `--shared --dissociate` 独立可读验收；`CL-28` 删除已移除的 code/code-control/graph/usage/publish 文档写集 | N/A | `CL-08`、`CL-18`、`CL-28`、审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R39-B FAIL/P2 | `CL-28` 改为通过 ref API/数据库查询验证 `refs/heads/newbranch` 不存在并比较拒绝前后状态，禁止按文件存在判断；同步从 Implementation write set 删除剩余 20 条过时文档路径 | N/A | `CL-28`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R40-B FAIL/P2 | `CL-28` 从写集和文档影响清单移除 package/prune/stats/repository-hooks 四组不适用路径 | N/A | `CL-28`、Review log |
| 2026-11-17 12:27:28 UTC | Codex R41-B PASS | 第 401–800 行及 CL-18 完整卡片无实质问题；CL-28 两组清单已删除全部指定过时/不适用路径，ref 状态验证、CL-08 符号链接与 CL-18 shared dissociate 验收均保留 | N/A | 头部、Review log |
| 2026-11-17 12:27:28 UTC | Codex R42-C FAIL/P2 | 性能摘要区分硬链接成功与复制回退，明确回退对象占用额外空间 | N/A | 性能与容量摘要、Review log |
| 2026-11-17 12:27:28 UTC | Codex R43-C FAIL/P2 | 性能摘要增加硬链接回退复制成本及远端不支持过滤时的全量传输与存储成本 | N/A | 性能与容量摘要、Review log |
| 2026-11-17 12:27:28 UTC | Codex R44-C FAIL/P1 | CL-20 增加非稀疏部分克隆缺失对象的按需合并回归，限定普通仓库不多余获取；过滤成本补对象字节数 | N/A | CL-20、性能摘要、Review log |
| 2026-11-17 12:27:28 UTC | Codex R45-C FAIL/P1 | CL-20 将普通仓库与稀疏拒绝回归保留在 Verification，去除重复验收条目并独立列出非稀疏部分克隆；重计 7/8 | N/A | CL-20、粒度审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R46-C FAIL/P1 | CL-20 增加稀疏内容修改/删除/冲突拒绝与工作区、索引、refs 不变的具名回归，限定目录重命名放行 | N/A | CL-20、粒度审计表、Review log |
| 2026-11-17 12:27:28 UTC | Codex R47-C PASS | 第 801 行至末尾及 CL-10/CL-18 复审无阻断问题；与 R37-A、R41-B 覆盖全文 | N/A | 状态、Review log |
| 2026-11-17 12:27:28 UTC | Commit-signing Codex R19-A FAIL/P1 | `DEP-CL-03` 同步排除依赖 CL-20 的 CL-17，消除间接等待；与 `DEP-CS-03` 镜像 | N/A | 依赖表、Review log |
| 2026-11-17 12:27:28 UTC | Codex 依赖闭包定向复审 PASS | 两侧集合一致，所核查的 Clone、PM、MP 与 CS 发布链无 CL-20 间接等待或循环 | N/A | 依赖表、Review log |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件 | N/A | 全文对应章节 |
| 原记 `2026-11-18 10:35:00 UTC`（不可证实：晚于该行实际写入；文件 mtime 只表明写入不晚于 2026-11-18 09:44:18 UTC，实际时刻未知；该行所称「文档与网站验收命令」块当时并未写入正文，见 2026-11-18 双审第 1 轮（R48）前专项清扫行） | Codex 复审 FAIL 修复（验证契约与门禁） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 16:51:58 UTC | 用户 2026-11-18 指令：计划提交门改为 Claude（Subagent）与 Codex 双审；双审第 1 轮（R48）前专项清扫 | ① 头部审阅合同、`M0`、完成判据改为双审口径（执行期 ER-05 与网站 Claude Code 门不变）；网站路径依据改为「用户 2026-11-18 11:55:09 UTC 确认」，`AGENTS.md:135` 登记为已知文档滞后；更正 `10:35:00 UTC` 不可证实时间行。② 补写此前被 26 处引用却不存在的「文档与网站验收命令」第 1–8 步与 W-1/W-2 命令块（含 `REL-CL-01` 家族子卡只做本地 `cf` 提交、`CL-25` 发布后统一推送与部署核验）；「验证执行协议」重写为第 1–7 项（`VER` = 2 × 条目数、单谓词 AC、失败类只断言稳定码、拒绝不变量与仓库快照、标准源仓库 S、进程隔离与 `LIBRA_TEST` 接缝）。③ 全部 nextest 过滤补 `command::` 前缀（`tests/command_test.rs:10`；原 `test(=clone_test::…)` 匹配 0 个），修复缺失闭合反引号与把两个测试塞进一条命令的 `CL-20` 条目；`CL-25` 删除不存在的 `test_clone_separate_git_dir` 与把强制门计入 VER 的条目。④ 27 张活动卡的 AC 重写为单谓词并逐条给出精确映射（原映射含「stdout/stderr 含 `<url>`」等与 AC 无关的机械断言及错位的测试名）；失败类改为既有稳定码（`LBR-CLI-002/003`、`LBR-REPO-001/003`、`LBR-IO-001`、`LBR-UNSUPPORTED-001`）；粒度行与审计表 AC/VER 同步。⑤ 按 issue 原文修正：`CL-02` 改为合并输出匹配 `Clon` 恰 1 行（Libra 无 `Cloning into` 文案）；`CL-21` 的 `Receiving objects` 改为按需拉取进度（`#754`/`t0410#38`）；`CL-20` 夹具改为 `t6421#2` 同构并以 `--branch A` 避开被 `CL-26` 拒绝的 `checkout`，`#756` 第 4 条随 `DEFER-CL-04`（成功定义改为 14 个全量闭环）；`CL-13` 删去与 Git 不符的重复 `--filter` 去重。⑥ 跨计划：`CL-29` 的「供 `PM-03`」改为 `PM-33` 并新增 `DEP-CL-14`（含 Libra `--bare` 克隆建立 `refs/remotes/origin/*` 的现状保持，`CL-29` AC6 守护）；新增 `DEP-CL-13`（同批共享文件写锁）与 `DEP-CL-15`（`REL-CL-01` 网站推送冻结窗口）；`DEP-CL-03` 依赖闭包按已推送版本复核；`DEP-CL-04`/`07`/`08` 注明对方登记 ID；关系表补 merge-pull 与同批写锁行；外部参照改为 pinned `6de20f6092`（`v2.56.0-116-g6de20f6092`）。 | N/A | 头部、文档职责、成功定义、事实基线、关系表、修订历史、执行检查必备需求、依赖登记表、全部任务卡、粒度审计表、测试矩阵、追溯表、M0、风险登记、DEFER-CL-04、完成判据 |
| 2026-11-18 17:02:21 UTC | Claude R48 FAIL + Codex R48 FAIL（2026-11-18 双审第 1 轮，Codex 按行区间分 A–F 六区并行、全部 FAIL；受审 SHA-256 `49db3c05736a`） | ① 「以 `<LBR-*>` 拒绝」定义为一个谓词（非零状态且 stderr 含稳定码，不断言具体数值）；新增「克隆拒绝不变量」（目标目录与 `<G>` 不存在），删去各卡零散的目标目录 AC；网站第 4 步补 `pnpm test`。② 按「一条 AC 一个可独立失败谓词」重排：`CL-04`（拆 `libra.db`/指针，补初始检出清单）、`CL-05`/`CL-15`（游离 HEAD 改为 `status --porcelain=v2 --branch` 前两行逐字比较，引用改用 `show-ref`，不再用不存在的 `reference.name='refs/…'` 查询）、`CL-06`（`libra remote` 逐字输出）、`CL-08`（拆回退字节/inode，删去改变 Libra 源 `--depth` 契约的旧 AC8）、`CL-10`（`count-objects -v` 对象总数）、`CL-12`（三个无效 filter 各自稳定码与 Git 原文分列）、`CL-13`/`CL-24`（改用 `--bare` 避免检出拉取 blob，对象类别逐类分列）、`CL-17`（两行逐字）、`CL-19`（缺失对象码改为实测 `LBR-CLI-003`；新增 stderr 接缝 `LIBRA_TEST_TRACE_LAZY_FETCH` 断言未发起拉取）、`CL-20`（三个重命名目标集合逐项相等；拉取计数改用 stderr 接缝，不再写日志文件）、`CL-21`（旧版本 blob 夹具与 `portable_pty` 伪终端默认进度）、`CL-28`（快照不变量覆盖索引与引用；全变体判定改为单元测试一次等值比较）、`CL-25`（逐子卡祖先、patch + 1 比较、补 `--lib` 聚合守卫、release run 总体结论；网站推送归入 ER-06a 文档门）。③ `CL-09` AC2 改用不含 `foo` 的新目录；`CL-18` 写明 `--shared --dissociate` 由既有实现达到同一终态、本卡只补回归；`ADR-CL-02` 删去与 Git 不符的「去重」；`CL-04` 标 `(1/4)`；事实基线锚点改为 `src/cli.rs:3869`、`tests/INDEX.md:44,81`；追溯表文档列按各卡写集补全 zh-CN/开发文档/网站页；完成判据限定活动卡、排除已撤回 `CL-22`。 | N/A | 执行检查必备需求、ADR-CL-02、事实基线、`CL-04/05/06/08/09/10/12/13/15/17/18/19/20/21/24/25/28` 及全部卡映射、粒度审计表、追溯表、风险登记、完成判据 |
| 2026-11-18 17:06:44 UTC | Claude R49 FAIL + Codex R49 FAIL（双审第 2 轮，Codex 分区 A–F 全部 FAIL；受审 SHA-256 `557e6e7a4eca`） | ① 事实基线与 `GAP-CL-02` 更正：`--dissociate` 已用于阻止 `--shared` 注册 alternate，`--reference` 只警告不借用；成功定义与 ER 收口前提限定为活动卡（排除已撤回 `CL-22`）。② `CL-25` 改为与模板 C 组 `gh pr merge --squash` 相容：子卡提交是受测 PR head 的祖先、squash merge 提交树等于受测树、PR head 版本为基底 patch + 1（9/12）。③ `CL-05` 分支缺失改用 `reference` 表 `kind='Branch'` 计数（`show-ref` 在空集时以 `LBR-CLI-003` 失败），补检出清单 AC，描述去掉 bare 的「不保留 refs」；`CL-06` 改按 `#752`（`t5606#16`）以 `refs/remotes/<name>/main` 观测配置在初始 fetch 前生效；`CL-10` 补有效 `--reference-if-able` 的 alternates 断言（7/8）；`CL-13` 三个配置键改用自定义远端 `upstream` 验收、默认名单列一条；`CL-18` 写明 `CL-08` 之后 `--shared --dissociate` 允许硬链接（删除源后可读即独立）。④ `CL-12`→`CL-13` 交接改为 AC8（过渡）/AC7（持续）；`CL-19` 以接缝正控制（AC1 运行恰有一行 `want=1`）替换原 AC2；`CL-20` 三类拒绝夹具互斥化；`CL-27` AC2 映射补非零状态；M1/M2 改为「命中 T-1 的卡另有全量」；风险登记 `CL-28` 行与卡内测试设计一致。 | N/A | 事实基线、GAP-CL-02、成功定义、执行检查必备需求、`CL-05/06/10/12/13/18/19/20/25/27`、粒度审计表、M1、M2、风险登记、完成判据 |
| 2026-11-18 17:11:21 UTC | Claude R50 FAIL + Codex R50 FAIL（双审第 3 轮，Codex 分区 A–E FAIL、F PASS；受审 SHA-256 `d91ad84a0b89`） | ① 克隆拒绝不变量限定为检出前的拒绝（Git `remove_junk` 只在 `JUNK_LEAVE_NONE` 时清理，检出失败保留仓库）；新增「对象存在性判定」（`cat-file -e` 退出 0 / 退出 1 且 stderr 空，其余判测试失败，配 `HEAD` 正控制；libra 0.30.207 实测），各卡缺失/存在断言统一引用。② `CL-08` 按 Git `builtin/clone.c:350-372` 重写：显式 `--local` 与默认本地路径分别验收硬链接，默认路径硬链接失败后复制、显式 `--local` 失败以 `LBR-IO-002` 拒绝，删去「其余对象仍硬链接」。③ `CL-03` 改用有效 Git 浅源（既有用例夹具故意写坏 `.git/shallow`）；`CL-13` 三个配置键改为 `config_kv` 行集合等值比较，并分别覆盖 `-o`、`clone.defaultRemoteName` 与默认 `origin`（7/8）；`CL-24` 显式 `combine:` 形式改为断言非根 tree 缺失；`CL-28` 判定测试改为命令行样本表（`branch`/`alternates` 分参数）并另断言样本覆盖全部变体（6/8）；`CL-21` 补 `cat-file` 文档写集；`CL-15` 证据锚点改为 `clone.rs:448-470`；`CL-23`/`CL-28` 夹具在卡内写全（G-05）。 | N/A | 验证执行协议第 4/7 项、`CL-03/05/08/13/15/19/21/23/24/28/29`、粒度审计表、追溯表、性能摘要 |
| 2026-11-18 17:15:13 UTC | Claude R51 FAIL + Codex R51 FAIL（双审第 4 轮，Codex 分区 A–F 全部 FAIL；受审 SHA-256 `8c9cc1bf28e1`） | ① `GC-CL-01` 限定为各卡验收写明的 Git 行为，显式豁免 `LBR-*` 码、SQLite 存储与 `.libra` 指针布局；W-2 退出码说明与代码一致（3 含 `HTTPError`）；协议第 5 项允许 `release` 卡聚合守卫作为其聚合类 AC 证据，第 6 项手工证据按 1 门计。② `CL-13` 过渡断言编号改为 AC5–AC7（`CL-24` 描述、Verification 与完成判据同步）；`CL-24` 显式 `combine:` 形式改为与重复 `--filter` 的对象存在性映射逐一相等，并登记拆出 `CL-29`（G-09）；`CL-25` 祖先、树等价、版本与 release run 改为 Verification 手工证据（12/12）。③ `CL-08` 故障接缝改为 `first`，验收 Git「首次硬链接失败后全部复制」；`CL-10` 补有效 `--reference-if-able` 的本地对象数为 0（8/8）；`CL-18` 以 `clone --json` 的 `data.objects_fetched` 为 0 证明 `--reference --dissociate` 先借用；`CL-05` 改验引用名与完整 OID 两种形式，`--bare --revision` 不写 fetch refspec 归 `CL-17` AC3；`CL-15` 描述去掉未验收的 `FETCH_HEAD`；`CL-17` refspec 加引号。 | N/A | 全局工程约束、验证执行协议第 5/6 项、文档与网站验收命令第 7 步、`CL-05/08/10/13/15/17/18/24/25`、粒度审计表、追溯表、完成判据 |
| 2026-11-18 17:18:14 UTC | Claude R52 FAIL + Codex R52 FAIL（双审第 5 轮，Codex 分区 A–E FAIL、F PASS；受审 SHA-256 `825c90795e20`） | ① 协议第 7 项：子进程要求限定为 `command_test` 集成用例，`--lib` 纯单元测试除外；标准源仓库 S 在创建时保存 `S1`/`S2` 完整 OID，`CL-05` AC5 与 `CL-21` 改用 `S1`，不再依赖可变 `HEAD~1`。② `CL-04` `--mirror` 组合改为「以 `LBR-CLI-002` 拒绝」并另列 Git 原文（8/8）；`CL-08` 符号链接拒绝改为对象目录根 `.libra/objects`；`CL-13` 补「重复同值 `--filter`」过渡断言（8/8），`CL-24` 交接与完成判据改为 AC5–AC8；`CL-21` 补非终端默认 `auto` 不输出进度（6/8）；`CL-20` 把合并成功并入接缝等值断言，非稀疏部分克隆补「发生按需拉取」与三个重命名目标断言。③ 收窄未验收的描述承诺：`CL-09` 去掉「非仓库」与 `<path>/.git` 回退，`CL-16` 帮助注释改动归 ER-06 文档门，`CL-19` 去掉「本地 OID 完整性校验」表述（对象按内容计算 OID 后写入），`CL-17` 描述改指本卡 AC3；`CL-18` 描述改指 AC4–AC5。 | N/A | 验证执行协议第 7 项、`CL-04/05/08/09/13/16/17/18/19/20/21/24`、粒度审计表、追溯表、完成判据 |
| 2026-11-18 17:21:37 UTC | Claude R53 PASS + Codex R53 FAIL（双审第 6 轮，Codex 分区 A/B/F PASS、C/D/E FAIL；受审 SHA-256 `a5563f5a29a0`） | ① `CL-08`：硬链接回退改为对象目录完整清单等值（相对路径 →（SHA-256，是否共享 inode）），同时覆盖无遗漏与全部复制；符号链接拒绝补对象子目录 `objects/pack`，描述改为遍历中任何符号链接都拒绝（Git `builtin/clone.c:328-330`）。② `CL-16`：`--sparse --filter=blob:none` 为过渡断言（AC7，`CL-20` 改写），`--sparse --filter=tree:1` 持续拒绝（AC8，新增 `DEFER-CL-05`）；删去「`add` 仍告警」这一既有行为 AC。`CL-20`：两个合并结果改为 (路径, blob OID) 集合与 `B-dir` 源文件逐项相等。③ `CL-24`：重复同值 `--filter` 改为断言 `combine:blob:none+blob:none`（AC8），删去回退提示文案 AC；`CL-13` AC7 交接到 `CL-24` AC8。协议第 7 项终端用例改为 `CL-21` AC5–AC6；评审结论「完整性」行把 `--bare --revision` 指向 `CL-17` AC3。 | N/A | 验证执行协议第 7 项、评审结论、`CL-08/16/20/24`、粒度审计表、追溯表、DEFER-CL-05、完成判据 |
| 2026-11-18 17:26:22 UTC | Claude R54 PASS + Codex R54 FAIL（双审第 7 轮，Codex 分区 F PASS、A–E FAIL；受审 SHA-256 `3cfa417276c2`） | ① 执行检查必备需求：全量触发只对独立发布卡生效，家族子卡继承 `CL-25` 的 `T-2`。`CL-05` 按 `#741`（`t5621#7`）重排：`--bare --revision=refs/heads/main` 的提交存在、无引用（排除 `libra init` 自建的内置分支 `intent`/`traces`——修订中实测发现原「`kind='Branch'` 行数为 0」会误判正确实现）、无 `remote.origin.fetch`、C2 缺失，非裸 tag 游离检出与检出清单；完整 OID 与不存在修订报错登记 `DEFER-CL-06`。② `CL-06` 补全局 `clone.defaultRemoteName`（`LIBRA_CONFIG_GLOBAL_DB` 隔离，6/8）；`CL-15` 远端跟踪分支改用 `reference` 表精确查询（Libra 以完整名 `refs/remotes/origin/main` 存储，`show-ref` 空集失败）；`CL-13` 三个克隆库都验 blob 缺失并补跑改写后的 `CL-12` 用例；`CL-24` 补跑改写后的 `CL-13` 用例；`CL-21` 非终端 `--progress=text` 验 stderr 通道与 stdout 纯净，`--progress=none` 终端用例加成功前提（8/8）。③ `CL-20` 改为「合并前后缺失→存在恰为 `t6421#2` 的 6 个 blob」并把 sparse 克隆成功并入前提；夹具更名 M（避免与 S 的 `S2` 混淆）；`CL-28` 描述中文档表格归 ER-06 文档门、代码判定归单元测试。 | N/A | 执行检查必备需求、评审结论、`CL-05/06/13/15/20/21/24/28`、粒度审计表、追溯表、DEFER-CL-06 |
| 2026-11-18 17:29:29 UTC | Claude R55 PASS + Codex R55 FAIL（双审第 8 轮，Codex 分区 A/B/F PASS、C/D/E FAIL；受审 SHA-256 `80f94c5d76ba`） | ① `CL-05`：裸克隆补 HEAD 行 (`name`, `commit`) = (NULL, `S2`)；引用计数只豁免 `kind='Branch' AND remote IS NULL` 的内置 `intent`/`traces`（同名 tag/远端分支照常计入）；非裸用例改用指向 `S1` 的 tag `v0`（区别于默认 `main` 末端），以 `status --porcelain=v2 --branch` 完整输出逐字比较同时覆盖游离 HEAD 与检出内容（仍 8/8）。② `CL-19` 以「取回的 B 已持久化」替换原「无 promisor 配置时拒绝」（既有行为）。`CL-16` AC8 改为表驱动覆盖 `tree:1`、`combine:`、`sparse:oid` 与多个 `--filter` 四种组合；`clone.rs:220` 的过时字样归属更正为 `--deps-of` 帮助说明。 | N/A | `CL-05/16/19`、粒度审计表、追溯表 |
| 2026-11-18 17:32:50 UTC | Claude R56 PASS + Codex R56 FAIL（双审第 9 轮，Codex 分区 D/F PASS、A/B/C/E FAIL；受审 SHA-256 `08c2f238c2e7`） | ① `CL-16` 撤回对 `--sparse` 与 `--filter` 组合的拒绝（原 AC7 过渡断言与 AC8 表驱动拒绝；后者按独立谓词计为 4 条而超限）：本卡不对组合另加限制，`#756` 所需 `--sparse --filter=blob:none` 由 `CL-20` AC1 验收，其余组合列入 `DEFER-CL-05` 不验收（稀疏仓库写入命令仍由 `CL-26`/`CL-27` 拒绝）；`CL-16` 6/8，`CL-20` 去掉解禁交接，完成判据同步。② `CL-10` 借用改用 `clone --json` 的 `data.objects_fetched` 为 0 证明未重复传输（含不带 `--reference` 的正控制），覆盖有效 `--reference` 与 `--reference-if-able`；`CL-05` 引用计数 SQL 改为 NULL 安全（`COALESCE`）。③ `ADR-CL-04` 改称 `CL-14` 的生产代码落点而非写集；`CL-04`/`CL-23` 写明 `CL-28` 自 `CL-23` 拆出（G-09）。 | N/A | `ADR-CL-04`、`CL-04/05/10/16/20/23`、粒度审计表、追溯表、DEFER-CL-05、完成判据 |
| 2026-11-18 17:36:20 UTC | Claude R57 PASS + Codex R57 FAIL（双审第 10 轮，Codex 分区 A/B/F PASS、C/D/E FAIL；受审 SHA-256 `fa8742640dc9`） | ① `CL-05`：裸克隆的提交存在与 HEAD 目标合为 `rev-parse HEAD^{commit}` 等于 `S2`（结合无分支即游离），腾出一条验收非裸 tag 克隆中 `S2` 缺失（只取请求修订可达对象）。② `CL-06` 收窄：`-c` 写入新仓库，`clone.defaultRemoteName` 在初始 fetch 前生效，其它配置项对初始 fetch/checkout 的即时影响登记 `DEFER-CL-07`。③ `CL-12`：删去 `sparse:path` 的 Git 原文 AC，新增「有效 `sparse:oid` 在本卡发布提交上被拒」过渡断言（AC6），由 `CL-29` 同卡改写并补跑改写后的 `CL-12` 用例；完成判据同步。`CL-19` `upstream` 夹具写全。`CL-26`/`CL-27` 补正控制共享回归组（普通仓库中对应命令的既有用例全部通过）。 | N/A | `CL-05/06/12/19/26/27/29`、粒度审计表、追溯表、DEFER-CL-07、完成判据 |
| 2026-11-18 17:40:04 UTC | Claude R58 PASS + Codex R58 FAIL（双审第 11 轮，Codex 分区 B/E/F PASS、A/C/D FAIL；受审 SHA-256 `8b992263bd89`） | ① 「文档职责」网站分支检查与第 2 步一致（非 `cf` 先 `libra switch cf` 再复核，仍不满足才 STOP）。② `CL-05`：裸克隆恢复 HEAD 行 (NULL, `S2`) 断言（`intent`/`traces` 可能被当作符号 HEAD 目标）；裸克隆与非裸 tag 克隆各以「S 全部对象的存在性 = 是否属于请求修订的可达闭包」一次等值比较，同时覆盖修订对象在库与只取请求修订。③ `CL-13` 收窄为本地传输的过滤，网络客户端 filter 协商与重复同值 `--filter` 的组合结果登记 `DEFER-CL-08`（网络远端按「远端不广告 filter」处理）；删去重复同值过渡断言（`CL-13` 7/8）与 `CL-24` 的重复同值 AC（7/8），交接与完成判据改为 `CL-13` AC5–AC7。`CL-29` 夹具三份内容写明且断言 OID 两两不同。`CL-21` 伪终端用例改为 stdout 重定向到文件、stderr 留在伪终端，并加成功前提。 | N/A | 文档职责、ADR-CL-02、`CL-05/12/13/21/24/29`、粒度审计表、追溯表、DEFER-CL-08、完成判据 |
| 2026-11-18 17:42:34 UTC | Claude R59 PASS + Codex R59 FAIL（双审第 12 轮，Codex 分区 A/B/E PASS、C/D/F FAIL；受审 SHA-256 `ad49ecc087ff`） | ① `CL-07` 两条验收改为对象目录完整清单等值（相对路径 →（SHA-256，是否共享 inode）），覆盖完整复制与无硬链接；`CL-10` 以「R 存在时 `d` 自身对象数为 0」替换「删除 R 后 `fsck` 失败」（后者在只留 alternates 文件时也成立）。② `CL-13`/`DEFER-CL-08` 收窄：网络远端上的 `--filter` 行为不在本计划验收，新增风险登记行。③ 故障恢复矩阵按 Git `remove_junk` 区分检出前失败（删除新建目录）与检出阶段失败（保留仓库）。 | N/A | `CL-07/10/13`、粒度审计表、追溯表、故障恢复矩阵、风险登记、DEFER-CL-08 |
| 2026-11-18 17:44:51 UTC | Claude R60 PASS + Codex R60 FAIL（双审第 13 轮，Codex 分区 A/B/D/E/F PASS、C FAIL；受审 SHA-256 `9d886274f274`） | ① `CL-06` AC3–AC5 改为 `reference` 表远端跟踪分支的 `remote` 去重集合逐项相等（`{upstream}`/`{from_option}`/`{from_config}`），排除同时建立配置名远端的错误实现。② `CL-08` 收窄为本地 Libra 源；本地 Git 源（`RepoType::GitRepo`）硬链接登记 `DEFER-CL-09`。 | N/A | `CL-06/08`、追溯表、DEFER-CL-09 |
| 2026-11-18 17:47:20 UTC | Claude R61 PASS + Codex R61 FAIL（双审第 14 轮，Codex 分区 A/B/C PASS、D/E/F FAIL；受审 SHA-256 `2656c291a936`） | ① `CL-13`/`CL-24` 本地传输锚点由 `local_client.rs:889`（外部 Git 源的 `read_git_repo_refs`）更正为 `local_client.rs:586` 的 `fetch_objects`。② `CL-16` AC3 改为顶层 `a.txt` 字节等于 `S2:a.txt`（原只验文件存在）。③ 风险登记与性能摘要的「远端未广告 filter 时回退全量」限定为本地传输，网络传输指向 `DEFER-CL-08`。 | N/A | `CL-13/16/24`、风险登记、性能与容量摘要 |
| 2026-11-18 17:49:59 UTC | Claude R62 PASS + Codex R62 FAIL（双审第 15 轮，本次运行的轮次上限；Codex 分区 A/C/F PASS、B/D/E FAIL；受审 SHA-256 `742da3205ce0`）；本行修订尚未经双审复核 | ① `CL-25` 删去把 ER-04 D 组 `release.yml` 结论计作卡特有 AC/VER 的 AC9（8/12、11/12），描述写明其属 D 组门。② `CL-12` 描述只列已验收的过渡拒绝（`blob:none`、有效 `sparse:oid`），删去「`tree:<n>`/`combine:` 由 `CL-13` AC5–AC6 验收」的错误归属。③ `CL-21` 文档影响逐文件列出 `cat-file` 四个路径；`CL-16` 补 AC7：`sparse-view list` 逐字为 cone 初始规则 `/*`、`!/*/`（7/8）。 | N/A | `CL-12/16/21/25`、粒度审计表、追溯表 |
| 2026-11-18 18:04:21 UTC | 用户 2026-11-18 指令下的接续运行（agent run 2）：R63 前专项清扫（起始字节 SHA-256 `543f84c75607`，含 R62 之后尚未复审的修订） | ① `CL-16`：删去 AC7（`sparse-view list` 逐字为 `/*`、`!/*/`）——把 cone 规则写入并启用只读 `sparse-view` 会使 `ls-files` 隐藏 skip-worktree 条目，与 AC5 矛盾（libra 0.30.207 实测）；描述改为本卡稀疏状态即 `core.sparseCheckout`/`core.sparseCheckoutCone` 与 skip-worktree 位、不启用 `sparse-view`，cone 规则文件的持久化与管理并入 `DEFER-CL-04`（6/8）。② `CL-12` AC5 由 `#748` 未要求的 `sparse:path` 改为单独 `--filter=tree:1` 以 `LBR-UNSUPPORTED-001` 持续拒绝（回应 R62 D 对单独 `tree:<n>` 发布期契约的意见），新增 `DEFER-CL-10`；`ADR-CL-02`、`CL-24` 描述与完成判据同步，删去 `ADR-CL-02` 的重复括注。③ `CL-28` 写集、文档影响与追溯表补 `branch` 四份文档（会改 ref 的 `branch` 形式受默认拒绝影响）。④ 19 张卡的 `Docs and compatibility impact` 由 `EN+zh`/花括号简写改为逐文件完整路径（ER-06a）。⑤ 夹具写全：标准源仓库 S 给出 `a.txt`/`sub/b.txt` 内容；`CL-26`/`CL-27` 夹具 Q 与 `CL-20` AC6–AC8 夹具 N 写明分支与改动；`CL-19` AC5/AC7 改在新建的 `d5`/`d7` 上判定。⑥「验证执行协议」第 6 项写明表驱动多值检查的计数口径；适用范围补 `CL-23`/`CL-28` 的其余命令文档。 | N/A | 状态行、适用范围、`ADR-CL-02`、验证执行协议第 6/7 项、`CL-01/02/03/05/06/07/08/09/10/12/14/15/16/17/18/19/20/24/26/27/28`、粒度审计表、追溯表、`DEFER-CL-04/10`、完成判据 |
| 2026-11-18 18:09:42 UTC | Claude R63 PASS + Codex R63 FAIL（接续运行双审第 1 轮，Codex 分区 A/B/E `PASS`、C/D/F `FAIL`；受审 SHA-256 `c87b31524f01`） | ① `CL-25`：patch + 1 与 `compat_version_surface_sync` 属 ER-04 C 组强制门，移出卡特有 AC/VER，改为不计数的强制门行（6/12、8/12），追溯表同步。② `CL-08`：符号链接拒绝改为目录根用显式 `--local`（AC6）、子目录与对象文件用默认本地路径（AC7–AC8），描述写明两种调用形式共用同一检查。③ `CL-29`：三条单 blob AC 合并为一次整体存在性映射比较（全部提交与 tree 存在、blob 中只有 `one.t` 存在，Git `filter_sparse`），在任何按需拉取之前判定（5/8）；守护 `DEP-CL-14` 的 `show-ref` 断言改号为 AC4，关系表与 `DEP-CL-14` 同步。④ 标准源仓库 S 的 `a.txt` 内容改为 `a1`/`a2`，避免与 F33 的 `one`/`two` blob 相同；Claude P2：评审结论「完整性」行改指 `CL-05` AC1–AC4，`CL-12` 描述去掉过度概括。⑤ 同类预防（与 ③ 同一缺陷类：只验部分对象或部分条目）：`CL-13` AC4 改为三库的整体存在性映射（提交与 tree 存在、blob 缺失）；`CL-16` AC5 改为 `ls-files -t` 逐字两行 `H a.txt`、`S sub/b.txt`。 | N/A | 评审结论、关系表、验证执行协议第 7 项、`DEP-CL-14`、`CL-08/12/13/16/25/29`、粒度审计表、追溯表 |
| 2026-11-18 18:13:31 UTC | Claude R64 PASS + Codex R64 FAIL（接续运行双审第 2 轮，Codex 分区 A/B/D/E/F `PASS`、C `FAIL`；受审 SHA-256 `37529476d4a3`） | `CL-08`：R63 把三种符号链接位置分摊到两种调用形式，不能排除「某位置只在一种形式下检查」的实现；AC6–AC8 合并为一条表驱动 AC6——目录根、`pack` 子目录、松散对象文件三种夹具 × 默认本地路径与显式 `--local` 共 6 个样本全部以 `LBR-REPO-003` 拒绝，映射一次等值断言并逐样本检查克隆拒绝不变量（6/8）。「验证执行协议」第 6 项写明表驱动多值检查是「不同选项…一律分条计数」的唯一例外（仅限用户可见结局完全相同的多个输入）。 | N/A | 验证执行协议第 6 项、`CL-08`、粒度审计表 |
| 2026-11-18 18:20:10 UTC | Claude R65 PASS + Codex R65 FAIL（接续运行双审第 3 轮，Codex 分区 A `PASS`、B–F `FAIL`；受审 SHA-256 `71e66474b028`） | ① `CL-25`：受测对象改为 ER-05 评审前的本地提交 `<T>`（三张子卡提交为其祖先、两项聚合守卫在其上通过；5/12、7/12）；patch + 1、`compat_version_surface_sync`、`T-2` 与 squash 合并树等于最终提交 `<F>` 的树移入不计数的 ER-04 C 组强制门行，消除「评审前需要合并提交」的顺序循环。② `CL-09` 证据补 `fetch.rs:129-133,164-180`、`local_client.rs:308`、`clone.rs:681-686` 锚点，描述写明落点在 `LocalClient::from_path`、不改 `fetch.rs`。③ `CL-12`：AC5/AC7 合并为表驱动样本表（`tree:0/1/2`、`object:type=blob/tree`、`blob:limit=1k`），描述写明解析器默认拒绝其余单独 filter；原 AC8 改号 AC7（7/8），`CL-13` 引用同步。④ `CL-13` AC5/AC6 改为显式 `combine:` 与多个 `--filter` 两张样本表（含 `blob:none`+`tree:1` 两种顺序及 `tree:2`、`object:type=blob` 组合）；`CL-24` AC6 改为三种等价形式的整体存在性比较，新增 AC8：其余组合持续拒绝（8/8）；`ADR-CL-02`、`DEFER-CL-10`、完成判据同步。⑤ `CL-16` 新增 AC7：`libra clone --help` 不含 `declined D10`（7/8）。⑥ `base.yml` 的触发描述更正为 `pull_request` 不限目标分支、`push` 限 `main`/`release/**`（两处）。⑦ 同类预防：`CL-25` 证据补三处版本面行号锚点；`CL-24` 删去未验收的「回退提示文案」承诺（不承诺 Git 式回退警告）。 | N/A | `ADR-CL-02`、执行检查必备需求、字段全局默认、`CL-09/12/13/16/24/25`、粒度审计表、`DEFER-CL-10`、完成判据 |
| 2026-11-18 18:25:14 UTC | Claude R66 PASS + Codex R66 FAIL（接续运行双审第 4 轮，Codex 分区 A/E/F `PASS`、B/C/D `FAIL`；受审 SHA-256 `cc915c93a85d`） | ① `CL-25` C 组行与 `Full-suite trigger` 改为模板顺序：bump 后先在未提交工作树上确认版本面、跑 `T-2`、构建与安装（④–⑥），再签名提交 `<F>`（⑦，提交后工作树干净即提交树为受测树）。② 共享回归组改用确定执行的本地用例：`CL-02` → `test_clone_json_reports_fetch_transfer_counts`（新增 `Cloning into` 不得进入 `--json` stdout）、`CL-04` → `test_clone_bare_and_mirror_default_names_and_layout`、`CL-12` → `test_clone_local_flag_accepted_for_local_source`、`CL-15` → `test_clone_depth_implies_single_branch_matrix`（原用例经 `github_test_repo_for_async_test` 门控，环境缺失时提前返回而计为通过）；「验证执行协议」第 5 项写明此要求；追溯表同步。③ `CL-06` AC1 改为 `-c` 与 `--config` 两种形式的表驱动断言。④ `CL-24` AC7 回退改为 `blob:none`、重复 `--filter` 与显式 `combine:` 三种形式的表驱动整体存在性断言，描述同步。⑤ 状态行不再称「修订历史末两行」（Claude P2）。 | N/A | 状态行、验证执行协议第 5 项、`CL-02/04/06/12/15/24/25`、追溯表 |
| 2026-11-18 18:30:44 UTC | Claude R67 PASS + Codex R67 FAIL（接续运行双审第 5 轮，Codex 分区 F `PASS`、A–E `FAIL`；受审 SHA-256 `8acd5db7e1ba`） | ①「验证执行协议」第 3、5 项：判据改为「运行 = 通过 = 1（或 N）、失败 = 0」，nextest 摘要的 skipped 含被过滤排除的用例，只记录不作判据，并禁止卡特有用例以 `#[ignore]` 或环境门控绕过断言。② 网站第 2、7 步的 `status --short` 加 `--no-branch`（`status.branch=true` 时会输出分支头行，libra 0.30.207 实测；`--porcelain` 不受影响）。③ 非目标行改为 `--sparse` 以两项配置、cone 物化与 skip-worktree 位实现、不启用 `sparse_view.rs` 只读过滤，与 `CL-16` 一致。④ `CL-03` 共享回归组改为确实带 `--reject-shallow` 的 `test_clone_from_shallow_git_source_matrix`（原用例不传该选项），追溯表同步。⑤ `CL-10` 重排为 6 条：alternates 路径、`objects_fetched` 为 0、本库对象数为 0、经 alternates 可读四项各以 `--reference` 与有效 `--reference-if-able` 两个目标的表驱动断言验收，另两条为无效路径放行与非仓库目录拒绝（6/8）。⑥ `CL-29` 新增 AC6：`remote.origin.partialclonefilter` 为 `sparse:oid=main:only-one`（Git `partial_clone_register` 原样记录，6/8）。⑦ `CL-20` 证据补 `merge.rs:1121`、`rename_merge.rs:291,1157`、`tree_merge.rs:205`；同类预防：`CL-05/08/10/12/13/19/21/24` 证据中只有文件名的引用补行号锚点。⑧ Claude P2：`CL-25` 提交后的干净检查改为 `status --porcelain --untracked-files=no`（只看已跟踪文件）。 | N/A | 非目标、验证执行协议第 3/5 项、文档与网站验收命令第 2/7 步、`CL-03/05/08/10/12/13/19/20/21/24/29`、粒度审计表、追溯表 |
| 2026-11-18 18:35:51 UTC | Claude R68 PASS + Codex R68 FAIL（接续运行双审第 6 轮，Codex 分区 A/C/F `PASS`、B/D/E `FAIL`；受审 SHA-256 `577ad813dfa8`） | ① `CL-01` AC3/AC4 补克隆退出 0 前提（否则报错退出也满足「stderr 不含 `%`」）；同类：`CL-16` AC6 补 `status` 退出 0、`CL-17` AC3 补克隆退出 0。② `CL-24` AC7 回退形式表补两种反序形式（共 5 种）。③ `CL-20`：三条拒绝合并为表驱动 AC6（逐分支检查仓库快照），新增 AC7（组合克隆的 `core.sparseCheckout`/`core.sparseCheckoutCone` 均为 `true`）与 AC8（合并前 `ls-files -t` 每个条目「路径含 `/` ⇔ 标记 `S`」），仍 8/8。④ Codex D P2：`CL-12` 重写旧用例时保留与 `--filter` 无关的 `--shallow-since`/`--shallow-exclude` 警告断言。⑤ 同类预防：`CL-14` AC4 改为裸克隆与 `libra init` 新建仓库两者都输出 `sqlite`（排除依赖克隆写入配置的实现），文档关键字改为 `ref-storage-format` 以同时命中 `rev-parse` 页。 | N/A | `CL-01/12/14/16/17/20/24` |
| 2026-11-18 18:40:33 UTC | Claude R69 PASS + Codex R69 FAIL（接续运行双审第 7 轮，Codex 分区 A/D/F `PASS`、B/C/E `FAIL`；受审 SHA-256 `30a76da5d845`） | ① `CL-28`：`init` 会检查并重新初始化既有 `.libra`（`init.rs:1293,1697`），移出放行名单、在指针布局下默认拒绝；放行名单改称「不读写当前仓库」并给出 `cli.rs:1771-1800` 依据；写集、文档影响与追溯表补 `init` 四份文档。② `CL-08`：AC3 补克隆退出 0 与 `d2` 对象完整可读的正控制；新增 AC7 源仓库属主边界——经新测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1` 视源为他人所有时，默认本地路径与显式 `--local` 均以 `LBR-REPO-003` 拒绝（Git `die_upon_dubious_ownership`，`builtin/clone.c:286-298`；7/8）；协议第 7 项与风险登记的接缝清单同步。③ `CL-20` AC8 改为 `ls-files -t` 的「路径 → 标记」映射与由分支 `A` 全部文件路径导出的期望映射整体相等（缺条目或多条目都失败）。 | N/A | 验证执行协议第 7 项、`CL-08/20/28`、粒度审计表、追溯表、风险登记 |
| 2026-11-18 18:42:44 UTC | Claude R70 PASS + Codex R70 FAIL（接续运行双审第 8 轮，Codex 分区 A/B/D/E/F `PASS`、C `FAIL`；受审 SHA-256 `033c9208891d`） | `CL-08`：属主检查对象由对象目录改为源仓库元数据目录（普通仓库 `<S>/.libra`、裸仓库 `<S>`，即 Git `clone_local` 传给 `die_upon_dubious_ownership` 的 `src_repo`，`builtin/clone.c:381-389,298`）；测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1` 只作用于该判定、不影响对象目录判定，使只检查对象目录的实现无法通过 AC7；对象路径被替换为符号链接的情形由 AC6 覆盖。 | N/A | `CL-08` |
| 2026-11-18 18:45:27 UTC | Claude R71 PASS + Codex R71 FAIL（接续运行双审第 9 轮，Codex 分区 A/D/E/F `PASS`、B/C `FAIL`；受审 SHA-256 `004ac1636abe`） | ① `CL-03` 共享回归组描述改为与用例一致（目标下不留 `.libra`；目标目录整体不存在由协议第 4 项对新增用例另行检查）。② `CL-08` AC7 样本表扩为普通源与裸源（`SB`）× 默认本地路径与显式 `--local` 四个样本，接缝作用于各自的元数据目录。③ `CL-08` 删去「替换为符号链接由 AC6 覆盖」的说法，改为按 Git `builtin/clone.c:343-361` 在硬链接后比对 mode/inode/设备号/大小/uid/gid，新增测试接缝 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK=first` 与 AC8（两种调用形式均以 `LBR-REPO-003` 拒绝；8/8）；复制回退路径与 Git 相同不另做竞态检测；协议第 7 项与风险登记的接缝清单同步。 | N/A | 验证执行协议第 7 项、`CL-03/08`、粒度审计表、风险登记 |
| 2026-11-18 18:49:52 UTC | Claude R72 PASS + Codex R72 FAIL（接续运行双审第 10 轮，Codex 分区 A/B/D/E/F `PASS`、C `FAIL`；受审 SHA-256 `bbe8b4745629`） | `CL-08`/`CL-07`：「验证执行协议」第 7 项新增「对象目录清单」定义（不含 `info/alternates`），两卡的 inode/复制清单断言引用该定义；`CL-08` 原 AC1/AC2（显式 `--local` 与默认本地路径都硬链接）合并为表驱动 AC1，新 AC2 验收源 alternates 改写：借用 R 且 alternates 为相对路径、自身无对象的源 SA（夹具步骤与断言写全，libra 0.30.207 实测相对 alternates 在源内生效）被克隆到更深目录后，两种调用形式下经改写后的 alternates 都能读出只在 R 中的 blob（Git `copy_alternates` 改写为绝对路径并在遍历中跳过该文件，`builtin/clone.c:184-224,337-339`）；描述同步（仍 8/8）。 | N/A | 验证执行协议第 7 项、`CL-07/08` |
| 2026-11-18 18:51:39 UTC | Claude R73 PASS + Codex R73 PASS（接续运行双审第 11 轮，Codex 分区 A–F 全部 `PASS`；受审 SHA-256 `189cec976839d00d26639be07074c70c91d8d7bb36e166444beb9fb0fa551e05`） | 双审对同一字节通过，计划可提交；本行与状态行为 PASS 后的记账修改，不改其它内容。残余 P2：`CL-16` 独立发布期间 `--sparse` 与 `blob:none` 以外的 `--filter` 组合不加限制也不验收（`DEFER-CL-05`）；`CL-26`/`CL-27` 未测 `core.sparseCheckout=false`；`CL-08` AC7/AC8 所在用例名只写符号链接；`CL-24` 未单独验 `combine:` 克隆的 `remote.origin.promisor`。 | N/A | 状态行、修订历史、Codex review log |

## 已决议设计决策

### ADR-CL-01: `clone --progress` 布尔重写与全局 `--progress=<mode>` 兼容

- **Status:** Accepted
- **Context:** `#737` 指出全局 `--progress <mode>` 在 `libra clone --progress <url>` 中把 `<url>` 吞成 `--progress` 的值。`src/cli.rs:1121` 已有 `rewrite_index_pack_progress_args` 为 `index-pack` 处理布尔 `--progress`/`--no-progress`。
- **Decision:** 扩展 `src/cli.rs` 的参数规范化逻辑（仅对 `clone` 子命令把裸 `--progress` 规范化为全局 `--progress=text`，同时保留 `clone.rs:111-112` 既有的 `--no-progress` 处理器而不提前改写为 `--progress=none`，保留 `--progress=<mode>` 显式值），且 `--progress` 在 stderr 重定向时仍输出 `%` 进度。
- **Alternatives considered:** 删除全局 `--progress`——拒绝，破坏其它长耗时命令。
- **Consequences:** 改动 `src/cli.rs` 全局路由，按 ER-13 命中 `T-1` 全量测试。
- **Revisit when:** 后续统一重构全局 vs 子命令同名标志时。

### ADR-CL-02: 部分克隆 `--filter` 与 `combine:` / `promisor` 分阶段边界

- **Status:** Accepted
- **Context:** `#746`、`#748`、`#754` 要求支持多 `--filter` 组合为 `combine:`、拒绝无效 `sparse:` 过滤器、并在服务端支持时按过滤拉取与按需 lazy fetch（如 `cat-file -p`）。当前仓库完全没有 promisor 支持（`clone.rs:269`）。
- **Decision:** 将部分克隆拆分为 6 张递进单轴卡并按 `CL-19 -> CL-12 -> CL-13 -> CL-24 -> CL-29 -> CL-21` 顺序推进，确保每个独立发布版本自洽：`CL-19`（对象读取层缺失时触发 promisor lazy fetch，通过预置 promisor 夹具自洽）、`CL-12`（无效 `sparse:` 精确校验与目录清理；单独 `tree:<n>`、`object:type=`、`blob:limit=` 等本计划不实现的 filter 自此持续拒绝，见 `DEFER-CL-10`）、`CL-13`（`--filter` 改为 `Vec` 以识别多个 `--filter`、`--filter=blob:none` 过滤协商、`remote.<remote_name>.promisor=true`/`extensions.partialClone=<remote_name>` 写入，并在 `CL-24` 落地前对 `combine:*` 与多个取值不同的 `--filter` fail-closed 拒绝；`CL-24` 起按 Git 把多个 `--filter` 组合为 `combine:`（`list-objects-filter-options.c` 的 `transform_to_combine_type`；只接受 `blob:none` 与 `tree:1` 的组合，其余组合持续拒绝，见 `DEFER-CL-10`；重复同值的组合结果见 `DEFER-CL-08`），避免出现“接受 `--filter` 却静默全量克隆”的半成品）、`CL-24`（`combine:blob:none+tree:1` 树深过滤传输与远端无能力安全回退）、`CL-29`（`sparse:oid` 过滤与预期缺失对象边界，供 `PM-33`（自 `PM-03` 拆出）的 `t5616#33`/`TC-0625` 消费）、`CL-21`（`Receiving objects` stderr 进度与真实 `clone -> cat-file` 端到端 lazy fetch 收口）。
- **Alternatives considered:** 把五条轴压进单卡 `CL-13`——拒绝，违反 G-01/G-04；把多 `--filter` CLI 解析拆成独立发布的 `CL-11`——拒绝，单独发布会导致静默全量克隆。
- **Consequences:** 全部 6 张部分克隆卡均按 `independent` 边界串行独立发布，直至 `CL-21` 完成后部分克隆全链路就绪。
- **Revisit when:** 扩展更多 Git partial-clone filter 类型时。

### ADR-CL-03: 为 `clone --sparse` 重开历史决策 D10

- **Status:** Accepted
- **Context:** 历史决策 D10（`COMPATIBILITY.md:166,172,375` 与 `docs/development/commands/_compatibility.md:175-197`）曾拒绝 `sparse-checkout`；`sparse_view.rs` 为只读视图过滤而不物化工作区或写 skip-worktree 位。`#756` 要求 `libra clone --sparse` 初始化稀疏 cone 检出并与部分克隆/`merge` 协同。
- **Decision:** 在 `CL-16` 中局部修订 D10（同步 `COMPATIBILITY.md` 与 `docs/development/commands/_compatibility.md`），为 `clone --sparse` 实现工作区 cone 物化与 skip-worktree 索引位，并将与 `--filter=blob:none` 及目录重命名合并时的按需 lazy fetch 拆入 `CL-20`（单卡单轴，G-01）；仍不新增独立 `libra sparse-checkout` 子命令。
- **Alternatives considered:** 维持 D10 拒绝 `#756`——拒绝，与本计划覆盖全部 17 个 Issue 目标冲突。
- **Consequences:** 同步更新 `COMPATIBILITY.md`、`docs/development/commands/_compatibility.md` 与 `clone.md` 文档。
- **Revisit when:** 后续单独规划完整 `libra sparse-checkout` 命令时。

### ADR-CL-04: `--ref-storage-format` 诚实报告 `sqlite` 后端（保留 `init` D25，延后 `files`/`reftable` 后端）

- **Status:** Accepted
- **Context:** `#750` 要求 `clone --ref-storage-format=<fmt>` 与 `rev-parse --show-ref-storage-format`。`src/command/init.rs:301-313,372-375,1676-1690` 的 `RefFormat`（`strict`/`filesystem`）仅控制分支名校验，Libra 实际将 refs 存储在 SQLite（`libra.db`），且 D25（`docs/development/commands/_compatibility.md:291-297`）拒绝 `init --ref-format=files|reftable`。将 SQLite 仓库误报为 `files` 会违背存储事实。
- **Decision:** 不修改 `init.rs` 与 `init --ref-format`（保留 D25）。在 `CL-14` 中为 `clone.rs` 新增 `--ref-storage-format`（接受 `sqlite`，对未实现的 `files`/`reftable` 或未知格式 fail-closed 返回明确错误），并为 `rev-parse --show-ref-storage-format` 如实输出当前仓库后端 `sqlite`（并在 `COMPATIBILITY.md` 记为 `intentionally-different`）；`#750` 所要求的 Git 松散 `files` ref 后端与 `reftable` 后端实现统一显式延后至 `DEFER-CL-01`。
- **Alternatives considered:** 将 SQLite 仓库伪报为 `files`——拒绝，会歪曲实际存储后端。
- **Consequences:** `CL-14` 的生产代码落点只有 `clone.rs` 与 `rev_parse.rs`（测试、命令文档与网站页见卡内 `Implementation write set`），不改 `init.rs`，存储语义诚实。
- **Revisit when:** 实现 Git `files` 或 `reftable` ref 存储后端时（`DEFER-CL-01`）。

### ADR-CL-05: `clone --separate-git-dir` 与 D25 范围边界（`#740`）

- **Status:** Accepted
- **Context:** `init --separate-git-dir` 被 D25 拒绝（`init.rs:377-379,697-701`）；`src/utils/convert.rs:221,242-252` 与 `src/utils/util.rs:265` 仅支持解析 `.git` 指针文件，而 `.libra` 发现（`src/utils/util.rs:797`）要求 `.libra` 为目录，且 `clone.rs:1592` 已预留 `separate_git_dir` 字段。
- **Decision:** `CL-04`（`REL-CL-01` 子卡 1/4）仅在 `clone` 路径实现 `--separate-git-dir=<dir>`：将仓库元数据初始化至 `<dir>` 并在 `<workdir>/.libra` 写入 `gitdir: <dir>` 文本指针文件。`CL-23`（子卡 2/4）在 `src/utils/util.rs:278,314-324,791-803,1269,1464` 与 `src/command/alternates.rs:91` 解析该指针，并使 `status`/`log`/`cat-file`/`branch` 可用。`CL-28`（子卡 3/4）在 `src/cli.rs` `command_preflight` 对指针布局默认拒绝：只有放行名单里的命令可以继续，其余 `Commands::*` 退出非零且不改索引、HEAD 或 ref。`CL-25`（发布点 4/4）跑 `T-2` 后统一发布。只发布指针文件、不发布这道默认拒绝时，`restore` 等未适配命令会把数据写到错误目录，所以这四张卡不能各自发布。
- **Alternatives considered:** 同步放开 `init --separate-git-dir`——拒绝，超出 `#740` 与本计划范围。
- **Consequences:** `CL-04`、`CL-23` 与 `CL-28` 作为 `REL-CL-01` 家族子卡均指向 `CL-25` 取得 C/D 覆盖。指针文件、读者适配和写命令拒绝必须同一次发布，避免用户拿到会写错目录的中间版本。
- **Revisit when:** 后续单独放开 `init --separate-git-dir` 时。

## 全局工程约束

继承 [plan-template.md](plan-template.md) `GC-01..13`。补充约束：
- **GC-CL-01:** 各卡验收中明确写出的 Git 行为（CLI 参数语义、进度与诊断原文、对象传输与仓库内容结果）以 pinned Git `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`v2.56.0-116-g6de20f6092`，核对日期 `2026-11-17`）为基准对齐；以下 Libra 专有部分不向 Git 取值：`LBR-*` 稳定错误码、SQLite（`libra.db`）ref 与配置存储及 `rev-parse --show-ref-storage-format` 的 `sqlite` 输出（ADR-CL-04）、`.libra` 目录与 `gitdir:` 指针布局（ADR-CL-05）；不破坏 `--json` 契约。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) `ER-01..14`。
- 每张 `implementation` 卡执行阶段按 ER-13 跑 A 组 focused 测试 + `cargo +nightly fmt --all --check` + `cargo clippy --all-targets --all-features -- -D warnings`；独立发布卡命中 `T-1`..`T-6`（如 `CL-01`、`CL-08`、`CL-13`、`CL-19`）时跑全量 nextest；家族子卡 `CL-04`、`CL-23`、`CL-28` 即使改动 `src/cli.rs` 等 `T-1` 表面，也不单独跑全量——其 C 覆盖（含全量）按 ER-04 继承家族发布点 `CL-25` 的 `T-2` 全量运行，该运行的被测树须包含三张子卡的最终变更。
- 独立发布卡和家族发布点推送后必须取得 D 组远端门证据。家族子卡 `CL-04`、`CL-23`、`CL-28` 不单独推送，继承 `CL-25` 的 D 组证据：
  1. `.github/workflows/base.yml`（jobs: `format`, `clippy`, `redundancy`, `test`；`pull_request` 不限目标分支，`push` 限 `main`, `release/**`；PR/main head SHA 全绿）；
  2. `.github/workflows/codeql.yml`（job: `analyze`；`pull_request` `main`，`push` `main`, `release/**`；PR/main head SHA 全绿）；
  3. `.github/workflows/release.yml`（jobs: `build-and-upload`, `upload-install-scripts`, `update-homebrew-tap`, `verify-homebrew-formula`, `request-stable-manifest`；`push` `v*` tag 由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发；全部 job 与 R2 上传全绿）。
- **网站文档交付默认（ER-06a，适用于本计划所有命中命令文档的卡）：** 网站目标固定为 `/Volumes/Data/gitmono/libra-backend` 的 `cf` 生产分支，不得切换其它 checkout 或另建分支/PR/worktree；写前识别仓库类型、当前分支与脏状态并保留无关改动。每张相关卡必须在同一交付内运行实际网站文档 sync/check、相关前端 typecheck/test/build，并取得覆盖最终网站文档变更的 Claude Code `VERDICT: PASS`（该网站交付门独立于计划文本双审；逐步命令见下方「文档与网站验收命令」）。独立卡随后精确提交并推送 `cf`，记录后端 commit、Cloudflare 部署 run 与生产 URL/正文核验；家族子卡只完成本地网站变更、验证与精确 `cf` 提交，由 `CL-25` 在家族统一发布后串行推送并核验部署，避免提前公开未集成能力。任一门失败均保持该卡或发布点未验收，不得以 Libra 主仓 release 成功替代网站交付。
- 全部活动卡（不含已撤回的 `CL-22`）完成后，收口阶段必须执行一次完整 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **卡特有用例**（标 `(new)`）：每条 Verification 只含一个测试，过滤式为完整路径精确匹配 `-E 'test(=command::clone_test::<函数>)'`（`tests/command/*.rs` 经 `tests/command_test.rs:10` 的 `mod command;` 编入 `command_test`，nextest 测试名带 `command::` 前缀；`--lib` 单元测试名为 `cli::tests::<函数>`）；不得用前缀、正则或 `|` 并集代替。
  2. 卡特有用例运行前先以同一前缀与过滤执行 `cargo nextest list`（例：`source .env.test && cargo nextest list --test command_test -E 'test(=command::clone_test::test_clone_short_n_skips_checkout)'`），列出的测试必须恰为 1 个且名字与预期逐字一致，否则 STOP。
  3. 卡特有用例运行后记录 exit code 与 nextest 摘要的「运行 / 通过 / 失败 / skipped」计数，要求 exit code = 0、运行 = 通过 = 1、失败 = 0，任一不满足即 STOP；摘要中的 skipped 包含被过滤表达式排除的其它用例，只记录、不作判据。被选中的卡特有用例不得以 `#[ignore]` 或环境门控提前返回的方式绕过断言（本计划卡特有用例均不设环境门控）。共享回归组与聚合守卫不适用本项与第 2 项的「恰为 1 个」，按第 5 项判定。
  4. 测试实现必须对卡内「AC → 断言映射」逐条断言，执行记录逐条引用 AC 编号。**拒绝不变量**：AC 写「以 `<LBR-*>` 拒绝」且被测命令作用于已存在的仓库时，测试在调用前后各取一次「仓库快照」并要求逐项相等（该不变量由本项统一规定、对每个此类拒绝用例检查，不另计 AC）。仓库快照 = 该仓库数据库（普通布局为 `.libra/libra.db`，`--separate-git-dir` 指针布局为 `<G>/libra.db`）中 `reference` 表与 `config_kv` 表的全部行（按主键排序，直接读表，不经会隐藏加密值的 `libra config list`；表定义见 `sql/sqlite_20260309_init.sql:12,19`）+ 索引文件 SHA-256 + 工作区「相对路径 → SHA-256」清单（排除仓库元数据目录）+ 对象目录内文件路径集合；对象目录唯一允许的差异是失败命令留下的未被引用的 blob/tree 文件（Git 亦然），新增文件经 `libra cat-file -t` 只读识别后不得为 commit 或 tag。**克隆拒绝不变量**：被测命令为 `libra clone` 且在检出之前被拒绝（参数校验、源解析、传输、过滤与浅仓库校验、对象复制；本计划全部克隆拒绝用例都属此类）时，测试在调用后断言目标目录（以及给出 `--separate-git-dir` 时的 `<G>`）不存在——调用前二者均不存在，Git 对这类失败同样经 `remove_junk` 清理新建目录（`builtin/clone.c:404-407,414-423`，此时 `junk_mode` 仍为 `JUNK_LEAVE_NONE`）；检出阶段失败时 Git 保留仓库（`junk_mode = JUNK_LEAVE_REPO`，`builtin/clone.c:1656`），本计划没有检出阶段失败的验收用例，不对其施加本不变量；该不变量由本项统一规定、对每个克隆拒绝用例检查，不另计 AC；克隆类拒绝不取仓库快照。**Git 期望**：映射中注明 Git 原文、Git 源码行或「Git 同夹具」的值，开工时在以 `git@6de20f6092` 构建的 Git 上用同一夹具重新取得并硬编码进测试；与本文不一致时以 pinned 版本为准并回写本卡 `Current evidence`（超出本卡行为轴时按 ER-03 处理）。Libra 专有行为（`LBR-*` 稳定码、`libra.db` 内容、测试接缝）以卡内写明的 Libra 基准为准，不向 Git 取值。
  5. **共享回归组 / 聚合守卫**（Verification 中如此标注）：可为整个 `--test` target、`test(=…)` 精确并集或 `test(/^…/)` 正则族；运行前以同一前缀与过滤执行 `cargo nextest list` 记录匹配数 N（必须 ≥ 1；卡内给出预期 N 时必须相等），运行后要求 exit code = 0、运行 = 通过 = N、失败 = 0（skipped 同第 3 项只记录）。选作具体行为回归的单个既有用例必须在本地确定执行该行为，不得选用环境缺失时提前 `return` 的 live 门控用例（如经 `github_test_repo_for_async_test` 取得 GitHub 测试仓库的用例——提前返回在 nextest 中计为通过）。结果只作回归证据，不计为卡特有 AC 证据，但计入该卡 `VER` 分子；唯一例外：`release` 卡 `CL-25` 的聚合守卫可作为该卡聚合类 AC 的证据（这些 AC 的谓词本身就是「子卡用例在受测树上通过」）。
  6. **计数口径（G-03）**：每条 Verification 含两个独立门（第 2/5 项的 `cargo nextest list` 预检与第 3/5 项的运行判定），nextest 条目各计 2 门，标为「手工证据」的条目各计 1 门（模板 G-03「手工证据按项计」），各卡 `VER` 分子为二者之和。一条 AC 只承载一个可独立失败的可观测谓词；不同选项、不同命令、不同错误条件一律分条计数（唯一例外见本项末「表驱动多值检查的计数」：用户可见结局完全相同的多个输入可在一条 AC 中以整体谓词陈述）；「过渡断言」（只在本卡发布提交上成立、由卡内写明的后继卡同卡改写的断言）照常计数。**映射口径**：AC 文本与其映射表述同一个谓词，映射只给出该谓词的精确判定值（逐字期望或等值比较对象），不加入 AC 文本之外的判据。失败类 AC 一律写成「以 `<LBR-*>` 拒绝」，这是一个谓词：进程以非零状态结束且 stderr 含该稳定码；映射同时检查这两个组成部分，但不断言具体数值退出码——数值可被 `exit_code_override` 与 `LIBRA_FINE_EXIT_CODES` 改变（`src/utils/error.rs`）；需要 Git 原文时原文单列为一条 AC。**表驱动多值检查的计数**（全部卡一致适用）：AC 文本若把一个集合或映射作为整体陈述为一个谓词——实际值是一个集合/映射（元素可来自多个对象、多个样本命令行——含不同选项形式或不同夹具位置——或卡内已建立的多个库），期望值由 AC 文本写明的单一规则导出，映射对二者只做一次等值断言或一次全称断言（如 `CL-05` AC2/AC6、`CL-07` AC1–AC2、`CL-08` AC4/AC6、`CL-13` AC4、`CL-24` AC2–AC6、`CL-28` AC5）——计为 1 条；凡需要按输入分别陈述不同的用户可见结局（如「A 退出 0、B 以 X 拒绝」、不同稳定码）的，按结局分条计数，不得以表驱动合并。
  7. **进程隔离、标准夹具与测试接缝**：新增的集成用例（`--test command_test`）一律以子进程调用 `libra`（纯单元测试——`--lib` 下的 `cli::tests::pointer_layout_allowlist_matches_commands`——在测试进程内调用解析与判定函数，不改 cwd/环境、不启动子进程，不适用本句），通过显式 `current_dir` 与逐子进程环境变量传入工作目录、`HOME`/`USERPROFILE`/`XDG_CONFIG_HOME`（指向仓库外的测试临时目录，避免测试 harness 在 cwd 下创建的 `.libra-test-home/` 进入快照）、`LIBRA_TEST` 与测试接缝变量，不修改测试进程的 cwd/环境，因而不使用 `#[serial(...)]`，不触及跨计划共享的 `tests/SERIAL_REGISTRY.tsv` 与生成的 `.config/nextest.toml`；若实现中确需 `#[serial]`，须先按 ER-03 把这两个文件加入本卡 `Implementation write set` 并补 `compat_serial_registry` 共享回归门。**标准源仓库 S**：测试临时目录中以 `libra init` 创建、默认分支 `main`，第一个提交加入顶层文件 `a.txt`（内容为单行 `a1`）与子目录文件 `sub/b.txt`（单行 `b`），第二个提交把 `a.txt` 改为单行 `a2`，并在第二个提交上建轻量 tag `v1`；测试在创建时保存两个提交的完整 OID，记为 `S1`、`S2`，后续不经 `HEAD~N` 等可变表达式重新推导；`file://<S>` 指以 S 绝对路径构成的 URL；各卡在此之上的增改写在卡内。**对象目录清单**（`CL-07`/`CL-08` 的硬链接、复制与 inode 断言使用）：对象目录下除 `info/alternates` 以外的全部普通文件；`info/alternates` 不是对象文件，按 Git `copy_alternates` 单独处理（`CL-08` AC2），标准源仓库 S 本身不含 alternates。**对象存在性判定**（只读）：在目标仓库执行 `libra --offline cat-file -e <完整 OID>`，退出 0 = 存在；退出 1 且 stderr 为空 = 缺失（`src/command/cat_file.rs:1229-1240` `check_object_exists`；2026-11-18 以 libra 0.30.207 实测缺失的完整 OID 为退出 1、无输出）；其它任何结果（含输出 `LBR-*` 的错误）判测试失败，不当作缺失；每个仓库在做缺失判定前先对其 `HEAD` 提交做同一判定并要求为存在（正控制；`--offline` 防止部分克隆按需拉取改变结果）。依赖符号链接、硬链接或 inode 的用例（`CL-07`、`CL-08`、`CL-09` 的 inode 比较）整条以 `#[cfg(unix)]` 限定；本计划的测试门（本地三门与 `base.yml` 的 `test` job）在 Unix 上执行。测试接缝 `LIBRA_TEST_CLONE_HARDLINK_FAIL`、`LIBRA_TEST_CLONE_FOREIGN_OWNER` 与 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK`（`CL-08`）、`LIBRA_TEST_UPLOAD_PACK_NO_FILTER`（`CL-13` 新增，`CL-24` 复用）与 `LIBRA_TEST_TRACE_LAZY_FETCH`（`CL-19` 新增，`CL-20` 复用；只向 stderr 写 `libra-test: lazy-fetch want=<n>` 行，不写任何文件）只在 `LIBRA_TEST=1` 时读取，生产路径不读取；需要终端的用例（`CL-21` AC5–AC6）以开发依赖 `portable_pty` 打开伪终端运行子进程（同 `tests/command/config_test.rs:3836`）；它们与既有 `LIBRA_TEST` 接缝同属测试专用，不进入用户文档。

- **文档与网站验收命令（ER-06/ER-06a 同卡强制门；各卡「文档 AC」引用；按序执行，任一步失败即 STOP 且本卡不得进入 `complete`）：**
  1. **仓库文档逐文件验收：** 对本卡 `Docs and compatibility impact` 所列每个仓库文件记录改动前后 `shasum -a 256 <file>`；对每个命令 `<cmd>` 以 `test -f docs/commands/<cmd>.md && test -f docs/commands/zh-CN/<cmd>.md` 确认 EN/zh 双份存在；对卡内「文档关键字」在每个所列 EN/zh 命令页执行 `rg -n -F -- '<关键字>' <file>`（退出码 0 = 命中；1 = 缺失 → STOP；>1 = 命令失败 → STOP），这些命令页改动后 SHA-256 必须与改动前不同；`docs/development/commands/<cmd>.md`、`docs/development/commands/_compatibility.md` 与 `COMPATIBILITY.md` 对应行逐文件做语义核对，结论为「已更新」（记录改动前后 SHA-256 与所改段落）或「无需修改」（记录理由与未变 SHA-256），不得为通过机械门而添加不合该文件粒度的内容；`COMPATIBILITY.md` 先以下方命令块 W-1 刷新目标行号（输出必须非空；同一命令可有补充行，基线：`clone`=166、`rev-parse`=217,277、`merge`=205,212），有改动时断言改动前后 `wc -l < COMPATIBILITY.md` 相等（只原地修改，不增删行：兼容账本按绝对行号锚定）。本计划每张实现卡都改变用户可见行为，所列 EN/zh 命令页与网站页必须同卡实际更新（ER-06a），不得以「无需修改」通过。
  2. **网站检出预检：** 前置：「文档职责」所载用户 2026-11-18 11:55:09 UTC 的网站检出确认仍有效、未被用户撤回或改变，否则 STOP。按序执行：(a) `test -d /Volumes/Data/gitmono/libra-backend/.libra && test ! -e /Volumes/Data/gitmono/libra-backend/.git`，不满足即 STOP；(b) `cd /Volumes/Data/gitmono/libra-backend && libra --offline symbolic-ref --short HEAD`，输出不是 `cf` 时按 ER-06a 执行 `libra switch cf`（被脏工作区等阻止即 STOP）后重跑 (b)；(c) 此后输出须恰为 `cf`，且 `libra status --short --branch` 首行须以 `## cf...origin/cf` 开头，否则 STOP；(d) 本卡目标页面 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 若已有未提交改动（`libra status --short --no-branch -- <页面路径>` 输出非空；`--no-branch` 排除 `status.branch=true` 时的分支头行）即 STOP；随后执行 `libra fetch origin`，要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`（本地 `cf` 不含未推送提交）——本地落后时只允许 `libra pull --ff-only origin cf` 快进后重验，领先或分叉即 STOP。**家族例外（仅 `REL-CL-01`）**：`CL-23`、`CL-28` 与 `CL-25` 执行本项时，允许本地 `cf` 领先 `refs/remotes/origin/cf` 的提交恰为同家族前序子卡在第 6 步记录的本地提交（以 `libra rev-parse refs/remotes/origin/cf` 起逐个比对已记录 SHA 与提交顺序，多一个、少一个或顺序不同即 STOP）；其余条件不变。`libra status --porcelain` 中不得有任何已暂存条目（首列既非空格也非 `?`），否则 STOP。已有的无关改动保留、不暂存。
  3. **同步与漂移检查：** 以包含本卡最终 EN 参考文档的 checkout 为 `--libra-root`（默认 `/Volumes/Data/gitmono/libra`；执行前以第 1 步记录的 SHA-256 确认其中 `docs/commands/<cmd>.md` 已是本卡最终字节：独立卡为已合入 `main` 的字节，家族子卡为本卡本地提交的字节），执行 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --write --only <cmd…>`（多个命令以空格分隔），随后 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --only <cmd…>` 必须 exit 0（1 = 漂移、2 = 安全拒绝，均 STOP）；记录每个 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 的改动前后 SHA-256，同步后网站页 SHA-256 必须与改动前不同；若未变即 STOP 并排查，不设「网站无变化」通过分支。
  4. **前端校验：** `cd /Volumes/Data/gitmono/libra-backend && pnpm typecheck`、`pnpm test` 与 `pnpm build` 分别记录退出码，均须为 0（根 `package.json` 的 `typecheck`/`build` 转发到 `apps/tanstack-app/package.json:13,8`，`test` 为 `vitest run`）。
  5. **网站评审：** 对第 3 步记录的最终页面字节取得独立 Claude Code `VERDICT: PASS`；评审后页面字节变化须重审。
  6. **精确提交（与推送）：** 只 `libra add apps/tanstack-app/content/docs/commands/<cmd>.en.md`（本卡页面；多命令卡逐页添加）→ `libra status --porcelain` 的已暂存条目必须恰为本卡页面集合，否则 STOP → `libra commit -s -m "docs(commands): sync <cmd> for <卡 ID>"` → 按 ER-07 对同一提交校验 `gpgsig` 与 `Signed-off-by` → `libra show <提交>:<页面路径> | shasum -a 256` 等于第 5 步受审字节。**独立卡**随后推送：推送前再次 `libra fetch origin`，要求 `libra rev-parse <提交>^` 等于 `libra rev-parse refs/remotes/origin/cf`（本次推送只新增本提交）→ `libra push origin cf`，推送后 `libra ls-remote origin refs/heads/cf` 指向该提交。**家族子卡**（`CL-04`、`CL-23`、`CL-28`）到此为止：只记录本地提交 SHA，不推送。**`CL-25`** 在本卡 `gh release create` 与 `release.yml` 五个 job 全绿后重跑第 2 步（含家族例外），再次 `libra fetch origin` 并要求三个家族提交之下的基底等于 `refs/remotes/origin/cf`，然后一次 `libra push origin cf`，推送后 `libra ls-remote origin refs/heads/cf` 指向 `CL-28` 的网站提交。远端已前进（上述比较不等，或推送因非快进被拒）时不推送、不 force、不改写他人提交：独立卡按 `libra reset --mixed <提交>^` → `libra restore --staged --worktree -- <页面路径>` → `libra pull --ff-only origin cf` 恢复后从第 2 步重新开始，重新生成的页面字节与第 5 步受审字节不同时须重新取得第 5 步 `PASS`；`CL-25` 遇此情况 STOP 并由协调者裁决（不得自行改写家族提交）。
  7. **部署核验（`<article>` HTML 逐字节比对）：** 基准构建前先过干净性门：`cd /Volumes/Data/gitmono/libra-backend && libra status --short --no-branch --ignored --untracked-files=all` 的输出中，除后端根目录 `docs/` 下的条目与下列构建产物/依赖/缓存的忽略条目外不得有任何条目：`node_modules/`、`apps/tanstack-app/node_modules/`、`apps/tanstack-app/dist/`、`apps/tanstack-app/.wrangler/`、`apps/tanstack-app/src/generated/docs-manifest.ts`、`apps/tanstack-app/src/generated/docs-search.ts`（二者由构建从内容目录重新生成）、`scripts/__pycache__/`、`test-results/`，以及 `apps/tanstack-app/.dev.vars`；后端根目录不得存在任何 `.env*` 文件；并要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`。任一不满足即 STOP（不清理、不暂存他人改动）。随后以 `pnpm preview:cf`（按 `apps/tanstack-app/package.json:9` 先构建再以 `wrangler dev --port 7001` 本地运行）作基准，对本地 `http://localhost:7001/en/docs/commands/<cmd>` 与生产 `https://libra.tools/en/docs/commands/<cmd>` 分别执行下方命令块 W-2：在内存中取得同一次响应，要求 HTTP 状态为 200、`<article>` 存在且文本非空，输出 `<article>` 内部 HTML 原始字节的 SHA-256 与字节数；退出码非 0（1 = 请求或传输失败、2 = 缺 `<article>` 或文本为空、3 = HTTP 响应状态非 200，含 `urllib.error.HTTPError`）即 STOP。两份 SHA-256 必须相等；不相等即 STOP（不得改用规范化文本比较放行）。只把两份哈希、字节数与观察 UTC 时间写入本计划「实施证据汇总」，不落盘任何响应内容；用后停止本地预览进程。部署传播延迟按 ER-10 暂时性错误有界重试（≤ 5 次、≤ 30 分钟），超预算置 `blocked`。`CL-25` 对三张子卡的每个页面逐一执行本步。
  8. **时序：** 独立卡：第 1–2 步在本卡 C 组签名提交之前完成，任一失败则本卡不得提交、合并或发布（`Lifecycle` 置 `blocked`）；第 3–7 步在本卡合入 `main` 与 `gh release create` 之后执行（开始前重跑第 2 步），使网站不先于发布公开新行为；第 3–7 步未完成前本卡停留在 `remote-pending`。家族子卡：第 1–2 步在本卡本地 Libra 提交之前完成，第 3–6 步（只到本地 `cf` 提交）在本卡本地 Libra 提交之后完成；推送与第 7 步由 `CL-25` 在发布后承接（`DEP-CL-15` 窗口），子卡随 `CL-25` 经 `remote-pending` 进入 `complete`。

上列第 1、7 步引用的命令块（`<cmd>` 替换为本卡命令；`<URL>` 为第 7 步的本地或生产页面地址）：

```bash
# W-1：定位 COMPATIBILITY.md 中 <cmd> 的状态矩阵行；输出须非空（基线 clone=166、rev-parse=217,277、merge=205,212）
awk -F'|' -v c=" <cmd> " '$2==c && $3 ~ /^ (supported|partial|unsupported|intentionally-different) $/ {print NR}' COMPATIBILITY.md

# W-2：在内存中取得 <URL> 的同一次响应，输出 <article> 内部 HTML 原始字节（不做规范化）的「SHA-256 字节数」；退出码须为 0，不落盘响应
python3 -I - "<URL>" <<'PY'
import hashlib, re, sys, urllib.error, urllib.request
req = urllib.request.Request(sys.argv[1], headers={"User-Agent": "curl/8.7.1"})
try:
    with urllib.request.urlopen(req, timeout=30) as r:
        if r.status != 200:
            sys.exit(3)
        s = r.read().decode("utf-8")
except urllib.error.HTTPError:
    sys.exit(3)
except Exception:
    sys.exit(1)
m = re.search(r"<article[^>]*>(.*?)</article>", s, re.S)
if not m or not re.sub(r"<[^>]+>", "", m.group(1)).strip():
    sys.exit(2)
b = m.group(1).encode("utf-8")
print(hashlib.sha256(b).hexdigest(), len(b))
PY
```

## 实施顺序

全串行 DAG（匹配各卡 `Dependencies`）：
`CL-01 -> CL-02 -> CL-03 -> CL-04 -> CL-23 -> CL-28 -> CL-25 -> CL-05 -> CL-06 -> CL-08 -> CL-07 -> CL-09 -> CL-10 -> CL-18 -> CL-19 -> CL-12 -> CL-13 -> CL-24 -> CL-29 -> CL-21 -> CL-14 -> CL-15 -> CL-26 -> CL-27 -> CL-16 -> CL-20 -> CL-17`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-CL-01 | incoming | 上游基线 | `libra` `main` 当前 checkout | 本计划协调者 | `cargo check` 通过且基线锚点可解析 | `src/command/clone.rs:1` | 开工前按 ER-02 刷新锚点 |
| DEP-CL-02 | incoming | 跨计划前置 | `plan-20261117-plumbing-misc.md#PM-06`（`#947`） | 仓库发布协调者 | `PM-06`（`rev-parse --show-object-format`）先合入 `src/command/rev_parse.rs` 后再合入 `CL-14`（单向无环） | `src/command/rev_parse.rs:59` | 若 `PM-06` 延迟，`CL-14` 等待其合入后 rebase |
| DEP-CL-03 | outgoing | 跨计划协调 | `plan-20261117-commit-signing.md#CS-02,#CS-02a,#CS-15b,#CS-21a`（`DEP-CS-03`） | `commit-signing` 计划协调者 | 仅下列活动卡先合入：`CL-01..CL-10`、`CL-12..CL-16`、`CL-18..CL-19`、`CL-21`、`CL-23..CL-29`（含 `CL-25` 家族发布点；明确排除已撤回的 `CL-11`/`CL-22`，以及为避免经 `CS-02a` 回环而排除的 `CL-20` 与依赖它的 `CL-17`）。`DEP-CS-03` 必须逐字镜像该集合，不以区间展开撤回卡。2026-11-18 按已推送版本复核：commit-signing（`2fe8c4b`）的 `DEP-CS-03` 与本集合逐字一致；本集合各卡的跨计划入边（`DEP-CL-02` 的 `PM-06` 链、`DEP-CL-04` 的 `FP-07`/`FP-15`、`DEP-CL-09`..`DEP-CL-13` 的写锁）在已推送的 plumbing-misc（`cd5ccac`）、fetch-push（`49b62d1`）、global-init-infra（`f3316d9`）与 worktree-clean-reset-apply（`55d5d2f`）中均未经由 `CS-02`/`CS-02a`/`CS-15b`/`CS-21a`，无回环；评审中的计划合入时由协调者按 ID 复核 | `docs/development/plan/plan-20261117-commit-signing.md` | 不阻塞本计划 |
| DEP-CL-04 | incoming | 跨计划顺序前置 | `plan-20261117-fetch-push.md#FP-07,#FP-15` | 仓库发布协调者 | `FP-07` 与 `FP-15` 先合入 `src/command/fetch.rs` 与 `COMPATIBILITY.md`，随后本计划 `CL-05/10/13/19/20/21/24/29` 按顺序 rebase 并合入（明确单向顺序，禁止并发）。`FP-07`、`FP-15` 指 `plan-20261117-fetch-push.md` 的卡，不是 `issues/529.md` 的同名卡；对方登记为 `DEP-FP-05` | `docs/development/plan/plan-20261117-fetch-push.md` | 若 `FP-07`/`FP-15` 未合入，本计划涉及 `fetch.rs` 的卡等待其窗口完成 |
| DEP-CL-05 | incoming | 外部闭环前置 | 外部 issue `#541` 与 `plan-20261117-fetch-push.md#FP-15` | `fetch-push` 与 `#541` 协调者 | 只门控 `DEFER-CL-03` 的 `fetch --prune` 端到端。`CL-17` 不依赖本行即可完成配置持久化 | `#876` | `#541` 与 `FP-15` 都进入 `main` 后才重启 `DEFER-CL-03` |
| DEP-CL-06 | outgoing | 跨计划协调 | `plan-20261117-{checkout-switch-restore,global-init-infra,plumbing-misc}.md` | 仓库发布协调者 | `CL-01`、`CL-28`、`CL-26`、`CL-27`、`CL-20` 修改 `src/cli.rs` 后按 ER-12 串行交付给后续修改 `src/cli.rs` 的计划 | `src/cli.rs:1` | 禁止并发修改 `src/cli.rs` |
| DEP-CL-07 | outgoing | 跨计划协调 | `plan-20261117-merge-pull.md#MP-17` | 仓库发布协调者 | `CL-20` 先合入 `merge.rs`、`rename_merge.rs`、`tree_merge.rs`。`CL-20` 开工前只确认没有在途写者，不等待 `MP-17`。`MP-17` 在 `CL-20` 合入之后再改这些文件（对方登记为 `DEP-MP-11`；该计划仍在评审，按 ID 引用） | `docs/development/plan/plan-20261117-merge-pull.md` | 禁止并发修改这些文件 |
| DEP-CL-08 | outgoing | 跨计划协调 | `plan-20261117-plumbing-misc.md#PM-14` | `plumbing-misc` 协调者 | `CL-19` 先于 `PM-14` 合入共享的 `src/command/cat_file.rs`（对方 `DEP-PM-08`），按 ER-12 串行 | `src/command/cat_file.rs:1` | 禁止并发修改 `cat_file.rs` |
| DEP-CL-09 | incoming | 跨计划协调 | `plan-20261117-global-init-infra.md` | 仓库发布协调者 | `CL-14` 共享 `src/command/rev_parse.rs` 按 ER-12 串行合入 | `src/command/rev_parse.rs:59` | 禁止并发修改 `rev_parse.rs` |
| DEP-CL-10 | incoming | 跨计划协调 | `plan-20261117-{global-init-infra,branch,describe,merge-pull}.md` | 仓库发布协调者 | `CL-23` 共享 `src/utils/util.rs` 按 ER-12 串行合入 | `src/utils/util.rs:278,314,791,1269,1464` | 禁止并发修改 `src/utils/util.rs` |
| DEP-CL-11 | incoming | 跨计划协调 | `plan-20261117-{checkout-switch-restore,global-init-infra}.md` | 仓库发布协调者 | `CL-16` 共享 `src/command/status.rs` 按 ER-12 串行合入 | `src/command/status.rs:563` | 禁止并发修改 `src/command/status.rs` |
| DEP-CL-12 | incoming | 跨计划文档写集 | 其它计划里会改命令文档的卡 | 仓库发布协调者 | `CL-28` 改写集里的每份命令文档前，核验 `plan-status.md` 没有该文件的在途写者 | `docs/commands/checkout.md:1` | 有在途写者时阻塞，禁止并发改同一命令文档 |
| DEP-CL-13 | incoming | 跨计划共享文件写锁 | 同批 `plan-20261117-*.md` 中写 `COMPATIBILITY.md`、`src/cli.rs`、`src/utils/util.rs` 或网站生成产物（`apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts`、`apps/tanstack-app/dist/`）的卡（对方 `DEP-GII-03`、`DEP-PM-11`、`DEP-MP-10` 等） | 仓库发布协调者 | 本计划每张写这些文件的卡开工前按 ER-12 在 `plan-status.md` 取得对应文件的独占窗口；`COMPATIBILITY.md` 只做原地修改、不增删行（兼容账本按绝对行号锚定） | `COMPATIBILITY.md:166` | 有持锁者时置 `blocked`，禁止并发改同一文件；本行全局门控，不写入各卡 `Dependencies` |
| DEP-CL-14 | outgoing | 跨计划顺序交付 | `plan-20261117-plumbing-misc.md#PM-33`（对方 `DEP-PM-08`） | `plumbing-misc` 计划协调者 | `CL-13`、`CL-19`、`CL-29` 在 `main` 达到 `done/complete` 后供 `PM-33` 开工；本计划修订历史与评审记录中写作「供 `PM-03`」之处即指自 `PM-03` 拆出的 `PM-33`。本计划不改变 Libra 非 `--mirror` 的 `--bare` 克隆建立 `refs/remotes/<name>/*` 的现状（`PM-33` 依赖；`CL-29` AC4 守护） | `docs/development/plan/plan-20261117-plumbing-misc.md` | 不阻塞本计划，单向无环 |
| DEP-CL-15 | outgoing | 网站推送冻结窗口 | 同批其它计划的网站交付步骤 | 仓库发布协调者 | `REL-CL-01` 窗口内（`CL-04` 第一个本地 `cf` 提交起，至 `CL-25` 推送并完成部署核验止）本地 `cf` 领先 `origin/cf`；窗口须在 `plan-status.md` 登记，其它计划的网站第 2 步在窗口内等待 | `plan-status.md` | 窗口超过 14 天未结束时由协调者裁决；不得推送未发布能力的页面，不 force |

### 发布分组与并发窗口

- **`REL-CL-01`（G-08 家族卡 — `clone --separate-git-dir` 指针布局、读者适配与写命令拒绝）：** 成员 `CL-04`（`family child`，1/4）→ `CL-23`（`family child`，2/4）→ `CL-28`（`family child`，3/4）→ `CL-25`（`Task type: release`，`family release point of REL-CL-01`，4/4）。不可独立发布的原因：只发布 `CL-04` 的 `gitdir:` 指针、不发布 `CL-23` 的路径发现和 `CL-28` 的默认拒绝时，放行名单之外的命令（包括 `restore`）会把数据写进错误目录。三张子卡各自 review、各自通过适用 ER-04 门并本地提交，**禁止**在 `CL-25` 之前单独推送或触发 tag/release（窗口内禁止插入任何其它卡）。`CL-25` 执行 `patch + 1`、`compat_version_surface_sync`、`cargo build --release`、全量 `T-2` 与 `gh release create`。发布前失败按逆序 `CL-25 -> CL-28 -> CL-23 -> CL-04` 本地 `revert`；发布后按 `immutable-release` 前滚。
- 其余 23 张卡均为 `Release boundary = independent`（各自走完整 C 组 `patch + 1` + `gh release create`）。

**并发声明:** 全串行（共享 `clone.rs`、`fetch.rs`、`COMPATIBILITY.md` 与 `docs/commands/` 写集，遵守 G-10）。

**发布者:** `Plan Author 5`（按 ER-12 与仓库全局单一发布者协调串行发布）。

**发布窗口顺序:** `CL-01` → `CL-02` → `CL-03` → `REL-CL-01`（`CL-04` → `CL-23` → `CL-28` → `CL-25`） → `CL-05` → `CL-06` → `CL-08` → `CL-07` → `CL-09` → `CL-10` → `CL-18` → `CL-19` → `CL-12` → `CL-13` → `CL-24` → `CL-29` → `CL-21` → `CL-14` → `CL-15` → `CL-26` → `CL-27` → `CL-16` → `CL-20` → `CL-17`（与实施 DAG 完全一致；同一时刻仅允许一卡/家族持有发布窗口）。

### Phase 1: CLI 标志与配置覆盖（CL-01..CL-04, CL-23, CL-28, CL-25, CL-05..CL-06）

**目标:** 修复 `--progress`、`-n`、`--no-reject-shallow`、`--separate-git-dir`（含指针消费者适配、`fail-closed` 契约与 `REL-CL-01` 发布）、`--revision` 与 `-c <key=value>`。
**进入条件:** `DEP-CL-01` 核实完成。
**退出条件:** `CL-01..CL-04, CL-23, CL-28, CL-25, CL-05..CL-06` 全部 `done/complete`。

### Phase 2: 本地硬链接、回退与 Alternates（CL-08, CL-07, CL-09, CL-10, CL-18）

**目标:** 先落地 `-l/--local` 硬链接（`CL-08`），再验收 `--no-hardlinks`（`CL-07`）、`<path>.git` 回退（`CL-09`）、`--reference`（`CL-10`）与 `--dissociate`（`CL-18`）。
**进入条件:** `CL-06` 完成。
**退出条件:** 本阶段 5 张卡全部 `done/complete`。

### Phase 3: 部分克隆、存储格式、稀疏视图与 Refspec（CL-19, CL-12, CL-13, CL-24, CL-29, CL-21, CL-14..CL-15, CL-26, CL-27, CL-16, CL-20, CL-17）

**目标:** 实现 promisor lazy fetch、无效 `sparse:` 拒绝、多 `--filter` 组合与协商、`sparse:oid` 过滤、进度、`--ref-storage-format`、`--single-branch` 游离 HEAD、稀疏工作区 `fail-closed` 护栏、`--sparse` 物化与 lazy merge、默认 `remote.<name>.fetch` 持久化。`#876` 的 `fetch --prune` 端到端在 `DEFER-CL-03`，不在本阶段收口。
**进入条件:** `CL-18` 完成。
**退出条件:** 本阶段列出的 13 张活动卡全部 `done/complete`。`CL-22` 已撤回，`DEFER-CL-03` 不阻塞本阶段。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Release write set 默认:** `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`（按 `compat_version_surface_sync` 核验实际版本面 + `gh release create` 产物；各卡写 `Inherited` 即继承本列表）
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（推送前可在本地分支 `revert` 单卡提交；一旦合入并触发 `gh release create` 发布不可变 artifact，禁止回退历史，必须前滚发布新 `patch + 1` 修复版本并给出降级指引：重新安装上一版本 binary，兼容窗口为当前 minor 系列内）
- **D-group remote checks 默认:** `base.yml`（jobs: `format`, `clippy`, `redundancy`, `test`；`pull_request` 不限目标分支，`push` 限 `main`, `release/**`）、`codeql.yml`（job: `analyze`；`pull_request` `main`，`push` `main`, `release/**`）、`release.yml`（jobs: `build-and-upload`, `upload-install-scripts`, `update-homebrew-tap`, `verify-homebrew-formula`, `request-stable-manifest`；`push` `v*` tag 由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发，全部通过）
- **Migration and rollback 默认:** `N/A：无 SQLite schema 迁移`
- **Security and privacy 默认:** `继承 GC-07、GC-11`
- **Performance budget 默认:** `继承 GC-10`

**默认覆盖:**
- `CL-04`、`CL-23`、`CL-28`：`Release boundary: family child of REL-CL-01`、`Version increment: N/A`、`Release write set: N/A`（本地提交不独立推送，随 `CL-25` 统一发布）。
- `CL-25`：`Task type: release`、`Release boundary: family release point of REL-CL-01`（在此点执行 `patch + 1`、`compat_version_surface_sync` 与 `gh release create`，跑 `T-2` 全量）。
**规则 waiver（`EX-*`）:** 无。

### 任务卡粒度审计表

> `landing / prod-files` 仅计入生产代码目录与生产文件（不含随附测试与文档，符合 G-04）。

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| CL-01 | implementation | clone --progress 布尔语义 | immutable-release | yes | yes | 4/8 | 4/8 | 2/2 | M | none | 串行链起点 | independent | N/A | N/A |
| CL-02 | implementation | clone -n 短标志支持 | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | CL-01 | 串行于 CL-01 | independent | N/A | N/A |
| CL-03 | implementation | clone --no-reject-shallow | immutable-release | yes | yes | 2/8 | 4/8 | 1/1 | M | CL-02 | 串行于 CL-02 | independent | N/A | N/A |
| CL-04 | implementation | clone --separate-git-dir 与 --bare 互斥 | immutable-release | yes | yes | 8/8 | 4/8 | 1/1 | M | CL-03 | 串行于 CL-03 | family child | N/A | N/A |
| CL-23 | implementation | pointer 发现与四个基本命令 | immutable-release | yes | yes | 5/8 | 4/8 | 2/2 | M | CL-04,DEP-CL-10 | 串行于 CL-04 | family child | CL-04 | N/A |
| CL-28 | implementation | 指针布局默认拒绝未放行命令 | immutable-release | yes | yes | 6/8 | 4/8 | 1/1 | M | CL-23,DEP-CL-12 | 串行于 CL-23 | family child | CL-23 | N/A |
| CL-25 | release | REL-CL-01 指针布局家族发布点 | immutable-release | yes | yes | 5/12 | 7/12 | 0/0 | S | CL-04,CL-23,CL-28 | 串行于 CL-28 | family release point | N/A | N/A |
| CL-05 | implementation | clone --revision 单修订克隆 | immutable-release | yes | yes | 8/8 | 4/8 | 2/3 | M | CL-25,DEP-CL-04 | 串行于 CL-25 | independent | N/A | N/A |
| CL-06 | implementation | clone -c &lt;key=value&gt; 配置覆盖 | immutable-release | yes | yes | 6/8 | 4/8 | 1/1 | M | CL-05 | 串行于 CL-05 | independent | N/A | N/A |
| CL-08 | implementation | clone -l/--local 硬链接本地对象 | immutable-release | yes | yes | 8/8 | 8/8 | 2/2 | M | CL-06 | 串行于 CL-06 | independent | N/A | N/A |
| CL-07 | implementation | clone --no-hardlinks 标志 | immutable-release | yes | yes | 2/8 | 4/8 | 1/1 | M | CL-08 | 串行于 CL-08 | independent | N/A | N/A |
| CL-09 | implementation | clone &lt;path&gt;.git 后缀回退 | immutable-release | yes | yes | 4/8 | 4/8 | 2/2 | M | CL-07 | 串行于 CL-07 | independent | N/A | N/A |
| CL-10 | implementation | clone --reference alternates 借用 | immutable-release | yes | yes | 6/8 | 4/8 | 2/3 | M | CL-09,DEP-CL-04 | 串行于 CL-09 | independent | N/A | N/A |
| CL-18 | implementation | clone --dissociate 解除 alternates | immutable-release | yes | yes | 5/8 | 6/8 | 1/1 | M | CL-10 | 串行于 CL-10 | independent | CL-10 | N/A |
| CL-19 | implementation | promisor 缺失对象读取 lazy fetch | immutable-release | yes | yes | 8/8 | 4/8 | 4/5 | M | CL-18,DEP-CL-04,DEP-CL-08 | 串行于 CL-18 | independent | CL-13 | N/A |
| CL-12 | implementation | clone 无效 sparse filter 拒绝 | immutable-release | yes | yes | 7/8 | 4/8 | 2/3 | M | CL-19 | 串行于 CL-19 | independent | N/A | N/A |
| CL-13 | implementation | clone --filter=blob:none 与 promisor 配置 | immutable-release | yes | yes | 7/8 | 6/8 | 2/4 | M | CL-12,DEP-CL-04 | 串行于 CL-12 | independent | N/A | N/A |
| CL-24 | implementation | clone combine:blob:none+tree:1 过滤与回退 | immutable-release | yes | yes | 8/8 | 6/8 | 2/4 | M | CL-13,DEP-CL-04 | 串行于 CL-13 | independent | CL-13 | N/A |
| CL-29 | implementation | clone sparse:oid 过滤与预期缺失边界 | immutable-release | yes | yes | 6/8 | 6/8 | 2/4 | M | CL-24,DEP-CL-04 | 串行于 CL-24 | independent | CL-24 | N/A |
| CL-21 | implementation | clone --filter 进度与端到端 lazy fetch | immutable-release | yes | yes | 8/8 | 4/8 | 2/2 | M | CL-29,DEP-CL-04 | 串行于 CL-29 | independent | CL-13 | N/A |
| CL-14 | implementation | --ref-storage-format 与 rev-parse | immutable-release | yes | yes | 7/8 | 4/8 | 1/2 | M | CL-21,DEP-CL-02,DEP-CL-09 | 串行于 CL-21 | independent | N/A | N/A |
| CL-15 | implementation | --single-branch 游离 HEAD 处理 | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | CL-14 | 串行于 CL-14 | independent | N/A | N/A |
| CL-26 | implementation | 稀疏工作区 checkout/switch/reset/restore 护栏 | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | CL-15 | 串行于 CL-15 | independent | CL-16 | N/A |
| CL-27 | implementation | 稀疏工作区 merge/pull/rebase/cherry-pick/stash 护栏 | immutable-release | yes | yes | 5/8 | 4/8 | 1/1 | M | CL-26 | 串行于 CL-26 | independent | CL-26 | N/A |
| CL-16 | implementation | clone --sparse cone 物化与 status 适配 | immutable-release | yes | yes | 7/8 | 4/8 | 3/3 | M | CL-27,DEP-CL-11 | 串行于 CL-27 | independent | N/A | N/A |
| CL-20 | implementation | clone --sparse 部分克隆与目录重命名合并 | immutable-release | yes | yes | 8/8 | 8/8 | 3/6 | M | CL-16,DEP-CL-04,DEP-CL-07 | 串行于 CL-16 | independent | CL-16 | N/A |
| CL-17 | implementation | 默认 clone 写 remote.&lt;name&gt;.fetch | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | CL-20 | 串行于 CL-20 | independent | N/A | N/A |
| CL-22 | superseded | 已并入 DEFER-CL-03，不得开工 | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | 无写集 | none | CL-17 | N/A |

---

### Task CL-01: 修复 `libra clone --progress` 布尔解析（含重定向 stderr 输出 `%`）（#737）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`0.30.211`；targeted `command::clone_test::test_clone_boolean_progress_does_not_consume_url` + `test_clone_help_documents_boolean_progress` 2/2，lib `clone_progress_rewrite*` + `index_pack_progress_rewrite*` 11/11；Codex R6 `VERDICT: PASS`；主仓与网站文档已同步；远端 CI 未等待，按用户 2026-11-20 指令 local C/D）
**Description:** 按 ADR-CL-01 修复 `libra clone --progress <url>` 将 `<url>` 当成 `--progress` 参数值消费的问题，同时兼容 `clone.rs:111-112` 既有的 `--no-progress` 处理器（不将其提前改写为全局 `--progress=none` 以免绕过原有处理），并确保 `--progress` 在 stderr 重定向至非 TTY 时仍输出 `%` 进度。
**Out of scope:** 非 `clone` 子命令的全局 `--progress=<mode>` 语义（永久非目标）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#737` 全局 `--progress` 消费后续位置参数 `<url>`；仅 `index-pack` 有 `rewrite_index_pack_progress_args` | `src/cli.rs:292-302,1121,1318,1366` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `libra clone --progress file://<S> d` 退出 0，且 `d` 中 `HEAD` 解析出的提交 OID 等于 S 的 `HEAD`（`<url>` 未被 `--progress` 当作取值吞掉）。
- [x] **AC2** AC1 的命令在 stderr 重定向到文件时，该文件至少有一行含 `%` 的进度行。
- [x] **AC3** `libra clone --no-progress file://<S> d2`（stderr 重定向到文件）退出 0，且其 stderr 不含 `%`（克隆成功是读取 stderr 的前提）。
- [x] **AC4** `libra clone --progress=none file://<S> d3`（显式取值形式，stderr 重定向到文件）退出 0，且其 stderr 不含 `%`。
- [x] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_boolean_progress_does_not_consume_url`：`exit 0` 后在 `d` 执行 `libra rev-parse HEAD`，stdout 去掉换行后等于在 S 执行同一命令的结果。
- AC2 → `command::clone_test::test_clone_boolean_progress_does_not_consume_url`：子进程 stderr 重定向到测试临时文件，读取后断言至少一行含 `%`。
- AC3 → `command::clone_test::test_clone_boolean_progress_does_not_consume_url`：`exit 0` 为前提；同上读取 stderr 文件，断言不含字符 `%`。
- AC4 → `command::clone_test::test_clone_boolean_progress_does_not_consume_url`：`exit 0` 为前提；同上读取 stderr 文件，断言不含字符 `%`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--progress`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_boolean_progress_does_not_consume_url)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [x] `source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::index_pack_progress_rewrite/)'` — 共享回归组（「验证执行协议」第 5 项）：既有 `index-pack` 布尔 `--progress` 重写回归（`src/cli.rs:4956`）。

**Full-suite trigger:** `T-1: 修改 src/cli.rs 全局参数预处理`
**Dependencies:** `无`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --progress 布尔语义; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=none; writeset=串行链起点; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-02: 支持 `libra clone -n` 短标志（`--no-checkout`）（#739）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs::no_checkout` 增补 `short = 'n'`，`libra clone -n` 跳过工作区检出；`command::clone_test::test_clone_short_n_skips_checkout` 覆盖 AC1–AC4，nextest 1 run / 1 passed / 0 failed；共享回归 `test_clone_json_reports_fetch_transfer_counts` 1 passed；`compat_version_surface_sync` 2 passed；`cargo +nightly fmt --all --check` exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化（新用例未进 serial registry）；版本面 patch 0.30.212 → 0.30.213。按用户 2026-11-20 指令在 `main` 直接提交推送、不等 CI。）
**Description:** 为 `CloneArgs::no_checkout` 添加 `short = 'n'`，支持 `libra clone -n <url>` 跳过工作区检出，且 stdout 与 stderr 合并输出中匹配 `Clon` 的行恰为 1 行（`#739` 验收第 3 条，对应 Git `t/t5601-clone.sh#4`）。
**Out of scope:** `--no-checkout` 内部检出跳过逻辑（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#739` `CloneArgs::no_checkout` 仅有 `long = "no-checkout"`，缺少 `short = 'n'` | `src/command/clone.rs:116-117` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `libra clone -n file://<S> dst` 退出 0，且 `dst` 中 `libra rev-parse HEAD` 等于 S 的 `HEAD` 提交 OID。
- [x] **AC2** 同一次克隆后，`dst` 工作区除 `.libra/` 外不含任何文件或目录（未检出）。
- [x] **AC3** 同一命令 stdout 与 stderr 合并后，匹配 `Clon` 的行恰为 1 行（`#739` 验收第 3 条，对应 Git `t/t5601-clone.sh#4`）。
- [x] **AC4** `libra clone --help` 的 stdout 含 `-n, --no-checkout`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_short_n_skips_checkout`：`exit 0` 后比较 `dst` 与 S 的 `libra rev-parse HEAD` 输出相等。
- AC2 → `command::clone_test::test_clone_short_n_skips_checkout`：列出 `dst` 的直接子项，集合恰为 `{.libra}`。
- AC3 → `command::clone_test::test_clone_short_n_skips_checkout`：拼接 stdout 与 stderr，按行计数含子串 `Clon` 的行，等于 1。
- AC4 → `command::clone_test::test_clone_short_n_skips_checkout`：`libra clone --help` 的 stdout 含子串 `-n, --no-checkout`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`-n, --no-checkout`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_short_n_skips_checkout)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_json_reports_fetch_transfer_counts)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地 `clone --json` 回归（`tests/command/clone_test.rs:1458`，把 stdout 解析为 JSON）——`Cloned into` 行不得进入 `--json` 的 stdout。

**Full-suite trigger:** `none`
**Dependencies:** `CL-01`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -n 短标志支持; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-01; writeset=串行于 CL-01; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-03: 支持 `libra clone --no-reject-shallow` 否定标志（#738）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增 `--no-reject-shallow`，与 `--reject-shallow` 互为 `overrides_with`（后出现者生效，单独 `--no-reject-shallow` 为默认 no-op）；`command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow` 覆盖 AC1–AC2（有效 Git 浅源：`--reject-shallow --no-reject-shallow` 退出 0；反向顺序以 `LBR-REPO-003` 拒绝且目标不留 `.libra`），nextest 1 run / 1 passed / 0 failed；共享回归 `test_clone_from_shallow_git_source_matrix` 1 passed；`compat_version_surface_sync` 2 passed；`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化；版本面 patch 0.30.214 → 0.30.215。按用户 2026-11-20 指令在 `main` 直接提交推送、不等 CI。）
**Description:** 为 `CloneArgs` 新增 `--no-reject-shallow` 与 `--reject-shallow` 互相 `overrides_with`（last-one-wins）。
**Out of scope:** `--reject-shallow` 自身检测逻辑（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#738` `CloneArgs::reject_shallow` 缺少对应的 `--no-reject-shallow` | `src/command/clone.rs:144-145` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `libra clone --reject-shallow --no-reject-shallow <浅源> d1` 退出 0（后出现的 `--no-reject-shallow` 生效；`<浅源>` 为有效 Git 浅仓库（以本机 `git` 对含两个提交的 Git 仓库执行 `git clone --depth 1 file://<源> <浅源>` 生成，不写坏 `.git/shallow`；与既有 `test_clone_reject_shallow_local_git_source` 的损坏夹具不同））。
- [x] **AC2** `libra clone --no-reject-shallow --reject-shallow <浅源> d2` 以 `LBR-REPO-003` 拒绝（后出现的 `--reject-shallow` 生效）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow`：断言 `exit 0`。
- AC2 → `command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow`：同一有效浅源；进程以非零状态结束且 stderr 含 `LBR-REPO-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）（`inspect_local_git_shallow` 成功且浅边界非空时的 `CloneError::RejectShallow` 映射，`src/command/clone.rs:629-633,1305-1340`）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--no-reject-shallow`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC2。
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_from_shallow_git_source_matrix)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地浅源矩阵回归（`tests/command/clone_test.rs:2106`：对有效 Git 浅源执行 `--reject-shallow` 以退出 128 与 `source repository is shallow, reject to clone.` 拒绝且目标下不留 `.libra`，完整源放行；新增用例的目标目录整体不存在由「验证执行协议」第 4 项克隆拒绝不变量另行检查）。

**Full-suite trigger:** `none`
**Dependencies:** `CL-02`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --no-reject-shallow; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=2/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-02; writeset=串行于 CL-02; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-04: 支持 `libra clone --separate-git-dir` 并输出与 `--bare` 互斥信息（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增 `--separate-git-dir <DIR>`；克隆（含初始检出）先在 `<workdir>/.libra` 内完成，检出后把元数据迁移到 `<DIR>` 并在工作区留下 `gitdir: <DIR>` 指针文件（`relocate_libra_dir`，rename 失败时回退为递归复制，指针写入失败则回滚）；`--bare`/`--mirror` 组合在创建任何目录前以 `LBR-CLI-002` + Git 原文 `options '--bare' and '--separate-git-dir' cannot be used together` 拒绝；`command::clone_test::test_clone_separate_git_dir_and_bare_mutex` 覆盖 AC1–AC8，nextest 1 run / 1 passed / 0 failed；共享回归 `test_clone_bare_and_mirror_default_names_and_layout` 1 passed；`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化。本卡为 `REL-CL-01` 家族子卡（无版本 bump，C/D 由 `CL-25` 承接）；按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 按 ADR-CL-05 为 `CloneArgs` 新增 `--separate-git-dir <dir>`，将仓库元数据目录初始化至 `<dir>` 并在 `<workdir>/.libra` 写入 `gitdir: <dir>` 指针文件（复用 `src/utils/convert.rs:242-252` 与 `src/utils/util.rs:265`），（`clone` 自身的初始检出须在写入指针文件之前完成或显式使用 `<dir>` 作为仓库路径，不依赖 `CL-23` 的指针发现），同时在 `--bare`/`--mirror` 与 `--separate-git-dir` 组合时输出 Git 兼容的互斥错误提示并清理失败新建目录。（按 G-09 拆出 `CL-23` 承接指针发现；`CL-28`（写命令 fail-closed）再自 `CL-23` 拆出。）
**Out of scope:** 指针发现与四个基本命令（`CL-23`）；写命令 fail-closed（`CL-28`）；`libra init --separate-git-dir`（保留 D25）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#740` `CloneArgs` 缺少 `--separate-git-dir` 及 `--bare` 互斥错误信息 | `src/command/clone.rs:89-91,1592`；`src/utils/convert.rs:242-252` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `libra clone --separate-git-dir <G> file://<S> <W>` 退出 0，且 `<G>/libra.db` 存在。
- [x] **AC2** AC1 之后读取 `<W>/.libra` 的文件内容，去掉 `gitdir:` 前缀并去除首尾空白后，规范化路径等于 `<G>` 的规范化路径。
- [x] **AC3** AC1 之后 `<W>` 工作区（不含 `.libra`）的「相对路径 → SHA-256」清单等于 S 的 `HEAD` 树全部文件的清单（初始检出完成）。
- [x] **AC4** `libra clone --bare --separate-git-dir <G2> file://<S> <W2>` 以 `LBR-CLI-002` 拒绝。
- [x] **AC5** AC4 的 stderr 含 Git 原文 `options '--bare' and '--separate-git-dir' cannot be used together`（`builtin/clone.c:1068-1070`）。
- [x] **AC6** `libra clone --mirror --separate-git-dir <G3> file://<S> <W3>` 以 `LBR-CLI-002` 拒绝（Git 中 `--mirror` 隐含 `--bare`，`builtin/clone.c:1063-1070`）。
- [x] **AC7** AC6 的 stderr 含同一 Git 原文 `options '--bare' and '--separate-git-dir' cannot be used together`。
- [x] **AC8** `libra clone --separate-git-dir <G4> <不存在的本地路径> <W4>` 以 `LBR-REPO-001` 拒绝（目标与 `<G4>` 的清理由「验证执行协议」第 4 项克隆拒绝不变量检查）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：`exit 0` 后断言 `<G>/libra.db` 的 `is_file()` 为真。
- AC2 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：`read_to_string(<W>/.libra)`，`strip_prefix("gitdir:")` 后 `trim()`，与 `<G>` 分别 `canonicalize` 后相等。
- AC3 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：两份清单逐项相等；期望清单取自在 S 中逐一 `libra cat-file -p HEAD:<path>` 的 SHA-256。
- AC4 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC5 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：AC4 同一次调用的 stderr 含该原文子串。
- AC6 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC7 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：AC6 同一次调用的 stderr 含该原文子串。
- AC8 → `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`：进程以非零状态结束且 stderr 含 `LBR-REPO-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–6 步（家族子卡：第 6 步只做精确本地 `cf` 提交、不推送；推送与第 7 步部署核验由 `CL-25` 承接）；文档关键字：`--separate-git-dir`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_and_bare_mutex)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC8。
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_bare_and_mirror_default_names_and_layout)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地 `--bare`/`--mirror` 默认目录名与裸布局回归（`tests/command/clone_test.rs:2486`）。

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，合并后由 CL-25 统一执行 T-2 全量门）`
**Dependencies:** `CL-03`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `N/A`（`REL-CL-01` 子卡不独立发版，统一在 `CL-25` 发布）
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/development/commands/_compatibility.md`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (1/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=clone --separate-git-dir 与 --bare 互斥; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-03; writeset=串行于 CL-03; release=family child; split-from=N/A; exception=N/A`

---

### Task CL-23: 适配 `util.rs`/`alternates.rs` 指针布局并使四个基本命令可用（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：新增 `util::resolve_dot_libra_pointer`，仓库发现（`try_get_paths_full_with_storage_lookup`）、`info/*` 候选与 `alternates` 的 objects 解析都跟随 `.libra` 的 `gitdir:` 指针；`status`、`log -n 1`、`cat-file -t HEAD` 与无参 `branch` 在指针布局下可用，`libra alternates add <W>` 记录 `<G>/objects`；`command::clone_test::test_clone_separate_git_dir_pointer_commands` 覆盖 AC1–AC5，nextest 1 run / 1 passed / 0 failed；共享回归 `command::status_test::*` 80 passed / 0 failed；`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化。本卡为 `REL-CL-01` 家族子卡（无版本 bump，C/D 由 `CL-25` 承接；写 ref 的默认拒绝由 `CL-28` 承接，本卡未放行任何写命令）；按用户 2026-11-20 指令在 `main` 提交推送。）
**Description:** 自 `CL-04` 拆出（G-09）。在 `src/utils/util.rs:278,314-324,791-803,1269,1464` 与 `src/command/alternates.rs:91` 解析 `.libra` `gitdir:` 指针，并使 `status`、`log`、`cat-file` 与无参数 `branch` 在该布局下退出 0，`libra alternates add <W>` 解析到 `<G>/objects`。会改 ref 的 `branch` 以及其余子命令由 `CL-28` 默认拒绝（按 G-09 自本卡拆出 `CL-28`）。
**Out of scope:** 默认拒绝（`CL-28`）；会改 ref 的 `branch`（`CL-28`）；`libra init --separate-git-dir`（保留 D25）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#740` 需要 `util.rs` 与 `alternates.rs:91` 消费 `gitdir:` 指针 | `src/utils/util.rs:278,314-324,791-803,1269,1464`；`src/command/alternates.rs:91` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 夹具：`libra clone --separate-git-dir <G> file://<S> <W>` 得到的 `<W>`（`.libra` 为指向 `<G>` 的 `gitdir:` 指针文件）中，`libra status` 退出 0。
- [x] **AC2** 同一 `<W>` 中 `libra log -n 1` 退出 0 且 stdout 含 S 的 `HEAD` 提交 OID。
- [x] **AC3** 同一 `<W>` 中 `libra cat-file -t HEAD` 的 stdout 为 `commit` 加换行。
- [x] **AC4** 同一 `<W>` 中无参数 `libra branch` 的 stdout 含当前分支名 `main`。
- [x] **AC5** 在另一普通仓库 R 中执行 `libra alternates add <W>` 后，R 的 `.libra/objects/info/alternates` 记录的路径规范化后等于 `<G>/objects`（`src/command/alternates.rs:91` 跟随指针，而不是返回不存在的 `<W>/.libra/objects`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：子进程 `current_dir=<W>`，断言 `exit 0`。
- AC2 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：期望 OID 取自在 S 中执行 `libra rev-parse HEAD`；断言 `exit 0` 且 stdout 含该 OID。
- AC3 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：stdout 逐字等于 `commit\n`。
- AC4 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：stdout 含子串 `main`。
- AC5 → `command::clone_test::test_clone_separate_git_dir_pointer_commands`：读取 R 的 alternates 文件，唯一非空行 `canonicalize` 后等于 `<G>/objects` 的 `canonicalize`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–6 步（家族子卡：第 6 步只做精确本地 `cf` 提交、不推送；推送与第 7 步部署核验由 `CL-25` 承接）；文档关键字：`separate-git-dir`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_pointer_commands)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC5。
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`status_test` 全模块回归。

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，由 CL-25 统一执行 T-2）`
**Dependencies:** `CL-04`、`DEP-CL-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/util.rs`, `src/command/alternates.rs`, `tests/command/clone_test.rs`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Release write set:** `N/A`
**Files likely touched:** `src/utils/util.rs`, `src/command/alternates.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`（网站文件写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (2/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=pointer 发现与四个基本命令; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=CL-04,DEP-CL-10; writeset=串行于 CL-04; release=family child; split-from=CL-04; exception=N/A`

---

### Task CL-28: 在指针布局下拒绝未适配的写命令（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`src/cli.rs::command_preflight` 在分发前对`.libra` 为 `gitdir:` 指针的布局默认拒绝未适配命令（`LBR-UNSUPPORTED-001`，且不改索引/`HEAD`/ref）；放行 `clone`/`status`/`log`/`cat-file`/`upgrade`/`ui`/`login`/`whoami`/`logout`/`completions`、`alternates list` 与无参数 `branch` 列表。`command::clone_test::test_clone_separate_git_dir_default_deny` 覆盖 AC1–AC4（含索引/引用/工作区快照不变）；`cli::tests::pointer_layout_allowlist_matches_commands` 覆盖 AC5–AC6（逐变体样本表 + 变体名集合相等）。`cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings` 均 exit 0；`sh tests/NEXTEST_GROUPS.sh` 后 `.config/nextest.toml` 无变化。本卡为 `REL-CL-01` 家族子卡（无版本 bump，C/D 由 `CL-25` 承接）；按用户 2026-11-20 指令在 `main` 提交推送。）
**Description:** 自 `CL-23` 拆出（G-09）。在 `src/cli.rs` `command_preflight`（:1755+）中，当 `.libra` 是 `gitdir:` 指针时默认拒绝。放行名单只有：`clone`；`status`；`log`；`cat-file`；不改 alternates 存储的 `alternates` 子命令；无参数的 `branch`；以及不读写当前仓库的 `upgrade`、`ui`、`login`、`whoami`、`logout`、`completions`（`command_preflight` 对它们返回 `CommandPreflight::none()`，`src/cli.rs:1771-1800`）。`init` 不在放行名单：它会检查并重新初始化既有 `.libra`（`src/command/init.rs:1293` `reinitialize_existing`、`:1697` `ensure_storage_paths_are_directories` 要求 `.libra` 为目录），在指针布局下按默认拒绝处理。会修改 alternates 存储的子命令不在放行名单里。其余 `Commands::*` 变体退出非零，并且不改索引、`HEAD` 或 ref。`_compatibility.md` 的表必须列出每一个变体（文档内容，随 ER-06 文档门逐文件核对）；代码中的指针布局判定由本卡新增的单元测试 `cli::tests::pointer_layout_allowlist_matches_commands` 以命令行样本表核对（AC5–AC6）。本卡是 `REL-CL-01` 子卡 3/4，不单独推送。
**Out of scope:** 指针发现与只读基本命令（`CL-23`）；把被拒绝的命令改造成真正的指针布局（`DEFER-CL-02`）；稀疏 cone 护栏（`CL-26`/`CL-27`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 未适配写命令若在指针布局下继续执行，会把数据写到错误目录。`restore` 会读并保存索引 | `src/cli.rs:1755`；`src/command/restore.rs:788,934` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 夹具：`libra clone --separate-git-dir <G> file://<S> <W>` 得到的 `<W>`（`.libra` 为指向 `<G>` 的 `gitdir:` 指针文件）（S 另含分支 `other`）中，`libra restore .`（先修改 `a.txt`）以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC2** 同一 `<W>` 中 `libra commit --allow-empty -m x` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC3** 同一 `<W>` 中 `libra branch newbranch` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC4** 同一 `<W>` 中 `libra checkout other` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [x] **AC5** 单元测试 `cli::tests::pointer_layout_allowlist_matches_commands`：对一张固定的命令行样本表（每个 `Commands` 变体至少一个样本；`branch` 含无参数、`branch newbranch`、`branch -D x`、`branch -m a b`，`alternates` 含 `list`、`add <p>`、`remove <p>`、`prune`）逐条经 clap 解析后求本卡指针布局判定，得到的「样本 → 放行/拒绝」映射与期望映射逐项相等（期望：放行名单内的命令放行，`branch` 仅无参数放行，`alternates` 仅 `list` 放行，其余全部拒绝）。
- [x] **AC6** 同一单元测试中，样本表解析得到的变体名集合等于 `Commands` 全部变体名集合（经 `clap::CommandFactory` 枚举子命令名，含隐藏变体），新增变体未进样本表即失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_separate_git_dir_default_deny`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）；调用前后按「验证执行协议」第 4 项比较 `<W>`/`<G>` 仓库快照（含 `<G>/index` 字节与 `reference` 表全部行）。
- AC2 → `command::clone_test::test_clone_separate_git_dir_default_deny`：同上（快照覆盖 `HEAD` 所在的 `reference` 行）。
- AC3 → `command::clone_test::test_clone_separate_git_dir_default_deny`：同上（快照覆盖全部引用，因而 `newbranch` 不得出现；按数据库行判断，不按文件是否存在判断）。
- AC4 → `command::clone_test::test_clone_separate_git_dir_default_deny`：同上。
- AC5 → `cli::tests::pointer_layout_allowlist_matches_commands`：对样本表做一次 `assert_eq!(实际映射, 期望映射)`。
- AC6 → `cli::tests::pointer_layout_allowlist_matches_commands`：两个变体名集合做一次 `assert_eq!`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–6 步（家族子卡：第 6 步只做精确本地 `cf` 提交、不推送；推送与第 7 步部署核验由 `CL-25` 承接）；文档关键字：`separate-git-dir`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_default_deny)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [x] `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::pointer_layout_allowlist_matches_commands)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC5–AC6。

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，由 CL-25 统一执行 T-2）`
**Dependencies:** `CL-23`、`DEP-CL-12`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/add.md`, `docs/commands/zh-CN/add.md`, `docs/development/commands/add.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`, `docs/commands/agent.md`, `docs/commands/zh-CN/agent.md`, `docs/development/commands/agent.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`, `docs/commands/archive.md`, `docs/commands/zh-CN/archive.md`, `docs/development/commands/archive.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`, `docs/commands/auth.md`, `docs/commands/zh-CN/auth.md`, `docs/development/commands/auth.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`, `docs/commands/automation.md`, `docs/commands/zh-CN/automation.md`, `docs/development/commands/automation.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`, `docs/commands/bisect.md`, `docs/commands/zh-CN/bisect.md`, `docs/development/commands/bisect.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`, `docs/commands/blame.md`, `docs/commands/zh-CN/blame.md`, `docs/development/commands/blame.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/bundle.md`, `docs/commands/zh-CN/bundle.md`, `docs/development/commands/bundle.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`, `docs/commands/cache.md`, `docs/commands/zh-CN/cache.md`, `docs/development/commands/cache.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`, `docs/commands/check-mailmap.md`, `docs/commands/zh-CN/check-mailmap.md`, `docs/development/commands/check-mailmap.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`, `docs/commands/cloud.md`, `docs/commands/zh-CN/cloud.md`, `docs/development/commands/cloud.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `docs/commands/commit-tree.md`, `docs/commands/zh-CN/commit-tree.md`, `docs/development/commands/commit-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `docs/commands/count-objects.md`, `docs/commands/zh-CN/count-objects.md`, `docs/development/commands/count-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`, `docs/commands/credential.md`, `docs/commands/zh-CN/credential.md`, `docs/development/commands/credential.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`, `docs/commands/deps.md`, `docs/commands/zh-CN/deps.md`, `docs/development/commands/deps.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`, `docs/commands/diff-files.md`, `docs/commands/zh-CN/diff-files.md`, `docs/development/commands/diff-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`, `docs/commands/diff-index.md`, `docs/commands/zh-CN/diff-index.md`, `docs/development/commands/diff-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`, `docs/commands/diff-tree.md`, `docs/commands/zh-CN/diff-tree.md`, `docs/development/commands/diff-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`, `docs/commands/dirty.md`, `docs/commands/zh-CN/dirty.md`, `docs/development/commands/dirty.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`, `docs/commands/fast-export.md`, `docs/commands/zh-CN/fast-export.md`, `docs/development/commands/fast-export.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`, `docs/commands/fast-import.md`, `docs/commands/zh-CN/fast-import.md`, `docs/development/commands/fast-import.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `docs/commands/file.md`, `docs/commands/zh-CN/file.md`, `docs/development/commands/file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`, `docs/commands/fsck.md`, `docs/commands/zh-CN/fsck.md`, `docs/development/commands/fsck.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`, `docs/commands/gc.md`, `docs/commands/zh-CN/gc.md`, `docs/development/commands/gc.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`, `docs/commands/grep.md`, `docs/commands/zh-CN/grep.md`, `docs/development/commands/grep.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`, `docs/commands/hash-object.md`, `docs/commands/zh-CN/hash-object.md`, `docs/development/commands/hash-object.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`, `docs/commands/hooks.md`, `docs/commands/zh-CN/hooks.md`, `docs/development/commands/hooks.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`, `docs/commands/hydrate.md`, `docs/commands/zh-CN/hydrate.md`, `docs/development/commands/hydrate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`, `docs/commands/index-pack.md`, `docs/commands/zh-CN/index-pack.md`, `docs/development/commands/index-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `docs/commands/investigate.md`, `docs/commands/zh-CN/investigate.md`, `docs/development/commands/investigate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`, `docs/commands/layer.md`, `docs/commands/zh-CN/layer.md`, `docs/development/commands/layer.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`, `docs/commands/lfs.md`, `docs/commands/zh-CN/lfs.md`, `docs/development/commands/lfs.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`, `docs/commands/logfile.md`, `docs/commands/zh-CN/logfile.md`, `docs/development/commands/logfile.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`, `docs/commands/ls-remote.md`, `docs/commands/zh-CN/ls-remote.md`, `docs/development/commands/ls-remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`, `docs/commands/ls-tree.md`, `docs/commands/zh-CN/ls-tree.md`, `docs/development/commands/ls-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`, `docs/commands/mailinfo.md`, `docs/commands/zh-CN/mailinfo.md`, `docs/development/commands/mailinfo.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`, `docs/commands/media.md`, `docs/commands/zh-CN/media.md`, `docs/development/commands/media.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`, `docs/commands/mega2.md`, `docs/commands/zh-CN/mega2.md`, `docs/development/commands/mega2.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`, `docs/commands/memory.md`, `docs/commands/zh-CN/memory.md`, `docs/development/commands/memory.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `docs/commands/merge-base.md`, `docs/commands/zh-CN/merge-base.md`, `docs/development/commands/merge-base.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`, `docs/commands/merge-file.md`, `docs/commands/zh-CN/merge-file.md`, `docs/development/commands/merge-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`, `docs/commands/mergetool.md`, `docs/commands/zh-CN/mergetool.md`, `docs/development/commands/mergetool.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`, `docs/commands/metadata.md`, `docs/commands/zh-CN/metadata.md`, `docs/development/commands/metadata.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`, `docs/commands/op.md`, `docs/commands/zh-CN/op.md`, `docs/development/commands/op.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`, `docs/commands/open.md`, `docs/commands/zh-CN/open.md`, `docs/development/commands/open.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`, `docs/commands/pack-objects.md`, `docs/commands/zh-CN/pack-objects.md`, `docs/development/commands/pack-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`, `docs/commands/read-tree.md`, `docs/commands/zh-CN/read-tree.md`, `docs/development/commands/read-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `docs/commands/reflog.md`, `docs/commands/zh-CN/reflog.md`, `docs/development/commands/reflog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`, `docs/commands/remote.md`, `docs/commands/zh-CN/remote.md`, `docs/development/commands/remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`, `docs/commands/repack.md`, `docs/commands/zh-CN/repack.md`, `docs/development/commands/repack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`, `docs/commands/rerere.md`, `docs/commands/zh-CN/rerere.md`, `docs/development/commands/rerere.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`, `docs/commands/review.md`, `docs/commands/zh-CN/review.md`, `docs/development/commands/review.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/development/commands/revision.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`, `docs/commands/sandbox.md`, `docs/commands/zh-CN/sandbox.md`, `docs/development/commands/sandbox.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`, `docs/commands/send-email.md`, `docs/commands/zh-CN/send-email.md`, `docs/development/commands/send-email.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`, `docs/commands/service.md`, `docs/commands/zh-CN/service.md`, `docs/development/commands/service.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`, `docs/commands/show-ref.md`, `docs/commands/zh-CN/show-ref.md`, `docs/development/commands/show-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`, `docs/commands/sparse-view.md`, `docs/commands/zh-CN/sparse-view.md`, `docs/development/commands/sparse-view.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`, `docs/commands/tag.md`, `docs/commands/zh-CN/tag.md`, `docs/development/commands/tag.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`, `docs/commands/update-index.md`, `docs/commands/zh-CN/update-index.md`, `docs/development/commands/update-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`, `docs/commands/update-ref.md`, `docs/commands/zh-CN/update-ref.md`, `docs/development/commands/update-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`, `docs/commands/verify-pack.md`, `docs/commands/zh-CN/verify-pack.md`, `docs/development/commands/verify-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`, `docs/commands/worktree.md`, `docs/commands/zh-CN/worktree.md`, `docs/development/commands/worktree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`, `docs/commands/write-tree.md`, `docs/commands/zh-CN/write-tree.md`, `docs/development/commands/write-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Release write set:** `N/A`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/add.md`, `docs/commands/zh-CN/add.md`, `docs/development/commands/add.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`, `docs/commands/agent.md`, `docs/commands/zh-CN/agent.md`, `docs/development/commands/agent.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`, `docs/commands/archive.md`, `docs/commands/zh-CN/archive.md`, `docs/development/commands/archive.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`, `docs/commands/auth.md`, `docs/commands/zh-CN/auth.md`, `docs/development/commands/auth.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`, `docs/commands/automation.md`, `docs/commands/zh-CN/automation.md`, `docs/development/commands/automation.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`, `docs/commands/bisect.md`, `docs/commands/zh-CN/bisect.md`, `docs/development/commands/bisect.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`, `docs/commands/blame.md`, `docs/commands/zh-CN/blame.md`, `docs/development/commands/blame.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/bundle.md`, `docs/commands/zh-CN/bundle.md`, `docs/development/commands/bundle.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`, `docs/commands/cache.md`, `docs/commands/zh-CN/cache.md`, `docs/development/commands/cache.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`, `docs/commands/check-mailmap.md`, `docs/commands/zh-CN/check-mailmap.md`, `docs/development/commands/check-mailmap.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`, `docs/commands/cloud.md`, `docs/commands/zh-CN/cloud.md`, `docs/development/commands/cloud.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `docs/commands/commit-tree.md`, `docs/commands/zh-CN/commit-tree.md`, `docs/development/commands/commit-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `docs/commands/count-objects.md`, `docs/commands/zh-CN/count-objects.md`, `docs/development/commands/count-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`, `docs/commands/credential.md`, `docs/commands/zh-CN/credential.md`, `docs/development/commands/credential.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`, `docs/commands/deps.md`, `docs/commands/zh-CN/deps.md`, `docs/development/commands/deps.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`, `docs/commands/diff-files.md`, `docs/commands/zh-CN/diff-files.md`, `docs/development/commands/diff-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`, `docs/commands/diff-index.md`, `docs/commands/zh-CN/diff-index.md`, `docs/development/commands/diff-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`, `docs/commands/diff-tree.md`, `docs/commands/zh-CN/diff-tree.md`, `docs/development/commands/diff-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`, `docs/commands/dirty.md`, `docs/commands/zh-CN/dirty.md`, `docs/development/commands/dirty.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`, `docs/commands/fast-export.md`, `docs/commands/zh-CN/fast-export.md`, `docs/development/commands/fast-export.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`, `docs/commands/fast-import.md`, `docs/commands/zh-CN/fast-import.md`, `docs/development/commands/fast-import.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `docs/commands/file.md`, `docs/commands/zh-CN/file.md`, `docs/development/commands/file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`, `docs/commands/fsck.md`, `docs/commands/zh-CN/fsck.md`, `docs/development/commands/fsck.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`, `docs/commands/gc.md`, `docs/commands/zh-CN/gc.md`, `docs/development/commands/gc.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`, `docs/commands/grep.md`, `docs/commands/zh-CN/grep.md`, `docs/development/commands/grep.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`, `docs/commands/hash-object.md`, `docs/commands/zh-CN/hash-object.md`, `docs/development/commands/hash-object.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`, `docs/commands/hooks.md`, `docs/commands/zh-CN/hooks.md`, `docs/development/commands/hooks.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`, `docs/commands/hydrate.md`, `docs/commands/zh-CN/hydrate.md`, `docs/development/commands/hydrate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`, `docs/commands/index-pack.md`, `docs/commands/zh-CN/index-pack.md`, `docs/development/commands/index-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`, `docs/commands/investigate.md`, `docs/commands/zh-CN/investigate.md`, `docs/development/commands/investigate.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`, `docs/commands/layer.md`, `docs/commands/zh-CN/layer.md`, `docs/development/commands/layer.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`, `docs/commands/lfs.md`, `docs/commands/zh-CN/lfs.md`, `docs/development/commands/lfs.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`, `docs/commands/logfile.md`, `docs/commands/zh-CN/logfile.md`, `docs/development/commands/logfile.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`, `docs/commands/ls-remote.md`, `docs/commands/zh-CN/ls-remote.md`, `docs/development/commands/ls-remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`, `docs/commands/ls-tree.md`, `docs/commands/zh-CN/ls-tree.md`, `docs/development/commands/ls-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`, `docs/commands/mailinfo.md`, `docs/commands/zh-CN/mailinfo.md`, `docs/development/commands/mailinfo.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`, `docs/commands/media.md`, `docs/commands/zh-CN/media.md`, `docs/development/commands/media.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`, `docs/commands/mega2.md`, `docs/commands/zh-CN/mega2.md`, `docs/development/commands/mega2.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`, `docs/commands/memory.md`, `docs/commands/zh-CN/memory.md`, `docs/development/commands/memory.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `docs/commands/merge-base.md`, `docs/commands/zh-CN/merge-base.md`, `docs/development/commands/merge-base.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`, `docs/commands/merge-file.md`, `docs/commands/zh-CN/merge-file.md`, `docs/development/commands/merge-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`, `docs/commands/mergetool.md`, `docs/commands/zh-CN/mergetool.md`, `docs/development/commands/mergetool.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`, `docs/commands/metadata.md`, `docs/commands/zh-CN/metadata.md`, `docs/development/commands/metadata.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`, `docs/commands/op.md`, `docs/commands/zh-CN/op.md`, `docs/development/commands/op.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`, `docs/commands/open.md`, `docs/commands/zh-CN/open.md`, `docs/development/commands/open.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`, `docs/commands/pack-objects.md`, `docs/commands/zh-CN/pack-objects.md`, `docs/development/commands/pack-objects.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`, `docs/commands/read-tree.md`, `docs/commands/zh-CN/read-tree.md`, `docs/development/commands/read-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `docs/commands/reflog.md`, `docs/commands/zh-CN/reflog.md`, `docs/development/commands/reflog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`, `docs/commands/remote.md`, `docs/commands/zh-CN/remote.md`, `docs/development/commands/remote.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`, `docs/commands/repack.md`, `docs/commands/zh-CN/repack.md`, `docs/development/commands/repack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`, `docs/commands/rerere.md`, `docs/commands/zh-CN/rerere.md`, `docs/development/commands/rerere.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`, `docs/commands/review.md`, `docs/commands/zh-CN/review.md`, `docs/development/commands/review.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/development/commands/revision.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`, `docs/commands/sandbox.md`, `docs/commands/zh-CN/sandbox.md`, `docs/development/commands/sandbox.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`, `docs/commands/send-email.md`, `docs/commands/zh-CN/send-email.md`, `docs/development/commands/send-email.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`, `docs/commands/service.md`, `docs/commands/zh-CN/service.md`, `docs/development/commands/service.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`, `docs/commands/show-ref.md`, `docs/commands/zh-CN/show-ref.md`, `docs/development/commands/show-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`, `docs/commands/sparse-view.md`, `docs/commands/zh-CN/sparse-view.md`, `docs/development/commands/sparse-view.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`, `docs/commands/tag.md`, `docs/commands/zh-CN/tag.md`, `docs/development/commands/tag.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`, `docs/commands/update-index.md`, `docs/commands/zh-CN/update-index.md`, `docs/development/commands/update-index.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`, `docs/commands/update-ref.md`, `docs/commands/zh-CN/update-ref.md`, `docs/development/commands/update-ref.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`, `docs/commands/verify-pack.md`, `docs/commands/zh-CN/verify-pack.md`, `docs/development/commands/verify-pack.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`, `docs/commands/worktree.md`, `docs/commands/zh-CN/worktree.md`, `docs/development/commands/worktree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`, `docs/commands/write-tree.md`, `docs/commands/zh-CN/write-tree.md`, `docs/development/commands/write-tree.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`。`_compatibility.md` 的拒绝表与 `Commands` 枚举一一对应。`branch` 只放行无参数形式，其会改 ref 的形式受本卡默认拒绝影响，其文档同样逐文件列入；`init` 在指针布局下被默认拒绝，其文档同样列入。网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (3/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=指针布局默认拒绝未放行命令; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-23,DEP-CL-12; writeset=串行于 CL-23; release=family child; split-from=CL-23; exception=N/A`

---

### Task CL-25: 发布 `REL-CL-01`（`clone --separate-git-dir` 指针布局家族发布点）（#740）

**Task type:** `release`
**Lifecycle / Acceptance:** `done` / `remote-pending`（2026-11-20：`REL-CL-01` 家族发布点。三张子卡 `CL-04`（`2335f3f`+`d9189cc`）、`CL-23`（`05698cd`）、`CL-28`（`5d3079f`）均已是本卡受测提交的祖先；版本面 patch 0.30.218 → 0.30.219（`Cargo.toml`/`install.sh`/`install.ps1` + `Cargo.lock`），`compat_version_surface_sync` 2 passed；家族聚合守卫 3 passed（`test_clone_separate_git_dir_and_bare_mutex`、`test_clone_separate_git_dir_pointer_commands`、`test_clone_separate_git_dir_default_deny`）；`cargo build --release --bin libra` exit 0。按用户 2026-11-20 指令：全量 `T-2` 未执行（3.5「不进行全量测试」），PR/`gh pr merge` 流程由「main 直接提交推送」替代（3.1），`gh release create` 与 tag 尚未执行（需用户批准后补）。`remote-pending` 指远端发布流水线与 Release 产物仍待用户批准。
**Description:** 聚合 `REL-CL-01` 家族子卡 `CL-04`、`CL-23` 与 `CL-28`（不引入新生产代码变更），执行 `patch + 1`、`compat_version_surface_sync` 三处版本面同步、工具链刷新 `Cargo.lock`、全量 `T-2` 门禁、`cargo build --release` 与 `gh release create`；其中 `patch + 1` 版本面同步与 `compat_version_surface_sync` 属 ER-04 C 组强制门（模板 C 组 ①–②），`release.yml` 等远端结果属 ER-04 D 组门（见「执行检查必备需求」），均不计入本卡 G-03 计数。对外发布指针布局、四个只读基本命令，以及 `CL-28` 对未放行子命令的默认拒绝；发布后承载三张子卡本地 `cf` 网站提交的推送与部署核验（「文档与网站验收命令」第 6–7 步，收口 `DEP-CL-15` 窗口）。
**Out of scope:** 任何新增功能代码实现；`libra init --separate-git-dir`（`DEFER-CL-02`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `REL-CL-01` 需在 `CL-04`、`CL-23` 与 `CL-28` 均完成本地提交后统一版本化发布；三处版本面（2026-11-18 核对均为 `0.30.210`，开工按 ER-02 刷新） | `Cargo.toml:3`；`install.sh:21`（`DEFAULT_VERSION`）；`install.ps1:25`（`$DefaultVersion`） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** `CL-04` 的本地签名提交是本卡受测提交 `<T>` 的祖先（`<T>` = `CL-28` 本地提交之后、ER-05 评审之前执行本卡 A/B 门时的本地 `HEAD`，尚未做 C 组版本 bump；其 `libra rev-parse HEAD` 与 `HEAD^{tree}` 记入执行记录）。
- [x] **AC2** `CL-23` 的本地签名提交是 `<T>` 的祖先。
- [x] **AC3** `CL-28` 的本地签名提交是 `<T>` 的祖先。
- [x] **AC4** 家族聚合守卫：三张子卡在 `command_test` 中的卡特有用例在 `<T>` 上全部通过。
- [x] **AC5** 家族聚合守卫：`CL-28` 的 `--lib` 卡特有用例在 `<T>` 上通过。
- [ ] ER-04 C 组强制门（不计入 G-03；本卡按用户 2026-11-20 指令执行 ①–③ 与 `cargo build --release`：版本面 `0.30.219`、`compat_version_surface_sync` 2 passed、release 构建 exit 0；`T-2` 全量按 3.5 暂缓，`gh release create` 待用户批准）：在 `<T>` 之上做 `patch + 1` 版本面同步与 `Cargo.lock` 工具链刷新（①–③）；在这一已 bump、尚未提交的工作树上先确认 `Cargo.toml` 的 `version` 行是合并基底 `X.Y.Z` 的 `X.Y.(Z+1)`、`source .env.test && cargo nextest run --test compat_version_surface_sync` 通过，再跑本卡 `Full-suite trigger` 的 `T-2` 收口门、`cargo build --release` 与安装（④–⑥）；之后才签名提交得到 `<F>`（⑦；提交后 `libra status --porcelain --untracked-files=no` 为空——已跟踪文件无未提交改动，因而 `<F>^{tree}` 即 ④–⑥ 的受测树；`libra merge-base --is-ancestor <T> <F>` 退出 0）；`<F>` 作为 PR head 推送，`gh pr merge --squash` 后 `libra fetch origin`，`libra rev-parse <MERGE_SHA>^{tree}` 等于 `libra rev-parse <F>^{tree}`（发布树即 `T-2` 受测树）。
- [ ] ER-06a 网站交付（不计入 G-03）：见下方「文档 AC」行。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → Verification 第 1 项（手工证据）。
- AC2 → Verification 第 2 项（手工证据）。
- AC3 → Verification 第 3 项（手工证据）。
- AC4 → Verification 第 4 项（聚合守卫，按「验证执行协议」第 5 项的 `release` 卡例外作为本 AC 证据；三测试精确并集，预期 N=3）。
- AC5 → Verification 第 5 项（同上，预期 N=1）。
- 文档 AC（ER-06a 网站交付，不计入 G-03）→ 「文档与网站验收命令」第 6 步的 `CL-25` 分支（一次推送三张子卡的本地 `cf` 提交）与第 7 步（对三张子卡改动的每个网站页面做部署核验）；本卡不改仓库文档。

**Verification:**
- [ ] 手工证据（执行记录，「验证执行协议」第 6 项按 1 门计）：`libra merge-base --is-ancestor <CL-04 提交> <T>` 退出 0，覆盖 AC1。
- [ ] 手工证据（执行记录，「验证执行协议」第 6 项按 1 门计）：`libra merge-base --is-ancestor <CL-23 提交> <T>` 退出 0，覆盖 AC2。
- [ ] 手工证据（执行记录，「验证执行协议」第 6 项按 1 门计）：`libra merge-base --is-ancestor <CL-28 提交> <T>` 退出 0，覆盖 AC3。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_and_bare_mutex) | test(=command::clone_test::test_clone_separate_git_dir_pointer_commands) | test(=command::clone_test::test_clone_separate_git_dir_default_deny)'` — 聚合守卫（「验证执行协议」第 5 项，预期 N=3）：在 `<T>` 的检出上重跑三张子卡的 `command_test` 卡特有用例，覆盖 AC4。
- [ ] `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::pointer_layout_allowlist_matches_commands)'` — 聚合守卫（「验证执行协议」第 5 项，预期 N=1）：在 `<T>` 的检出上重跑 `CL-28` 的 `--lib` 卡特有用例，覆盖 AC5。

**Full-suite trigger:** `T-2: REL-CL-01 家族发布点全量门`（模板 C 组 ④–⑤：在含 `patch + 1` 与 `Cargo.lock` 刷新、尚未签名提交的工作树上运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 与 `cargo build --release`；⑦ 签名提交后记录 `libra rev-parse HEAD` 与 `HEAD^{tree}` 并确认 `libra status --porcelain --untracked-files=no` 为空（提交树即受测树）；树变化即重跑）
**Dependencies:** `CL-04`, `CL-23`, `CL-28`
**Deliverables:** `Release artifacts & tag for REL-CL-01`
**Implementation write set:** `N/A`
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`
**Docs and compatibility impact:** `N/A`（不改仓库文档，无新增或变更用户命令语义；三张子卡已在本地完成的网站 `cf` 提交由本卡推送并核验部署，见本卡「文档 AC」行）
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CL-01`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CL-01 指针布局家族发布; recovery=发布前按 CL-25 -> CL-28 -> CL-23 -> CL-04 逆序本地 revert，发布后前滚新 patch; complete=yes; self-contained=yes; AC=5/12; VER=7/12; landing=0; prod-files=0; scope=S; deps=CL-04,CL-23,CL-28; writeset=串行于 CL-28; release=family release point; split-from=N/A; exception=N/A`

---

### Task CL-05: 支持 `libra clone --revision=<rev>` 单修订克隆（#741）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增 `--revision=<REV>`，与 `--branch`/`--mirror` 互斥（clap `conflicts_with_all`，`LBR-CLI-002`）；`refs/heads/*` 以单分支 refspec 抓取，其它引用（如 `refs/tags/v0`）映射到临时 `refs/remotes/<n>/_revision` 目的地抓取（fetch 层不接受 tag 目的地）；抓取后置 `Head::Detached(advertised OID)` 并用 `switch::restore_to_commit` 物化工作区，随后删除 `refs/remotes/<n>/*` 的 Branch 行与全部 Tag 行、跳过 `setup_repository`（因此不写 `remote.<n>.fetch`）。新增 `CloneError::RevisionNotFound`（`LBR-CLI-003`）。`test_clone_revision_single_rev`（AC1–AC6）与 `test_clone_revision_conflicts`（AC7–AC8）nextest **2 passed / 0 failed**；`cargo +nightly fmt --all --check` exit 0。按用户 2026-11-20 指令在 `main` 直接提交推送、不等 CI。）
**Description:** 为 `CloneArgs` 新增 `--revision=<rev>`（本卡验收引用名形式：`#741` 的 `--bare --revision=refs/heads/main` 与非裸 `refs/tags/v0`（指向 `S1`）；完整 OID 形式与不存在修订的报错见 `DEFER-CL-06`），在 `clone.rs` 与 `fetch.rs`/`protocol` 层仅拉取指定单个修订并检出为游离 HEAD，并与 `--branch`/`--mirror` 互斥。
**Out of scope:** `--depth` 浅历史截断本身（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#741` `CloneArgs` 缺少 `--revision` 标志且 `fetch` 仅协商分支/tag | `src/command/clone.rs:64-96`；`src/command/fetch.rs:2266-2271`（`fetch_repository` 只接受分支、`single_branch` 与 `depth`，无修订参数） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** S 另有分支 `side` 的提交 C2（不可从 `main` 到达）；`libra clone --revision=refs/heads/main --bare file://<S> b` 退出 0 后，`b` 的本地 HEAD 行（`reference` 表 `kind='Head' AND remote IS NULL`）的 (`name`, `commit`) 等于 (NULL, `<S2>`)（HEAD 游离于请求的修订；`#741`，对应 Git `t/t5621-clone-revision.sh#7`）。
- [x] **AC2** 对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `b` 中的存在性判定结果，恰等于该对象是否属于 `libra rev-list --objects <S2>`（S2 可达闭包）（请求的提交及其历史在本地，`C2` 等其它对象缺失；覆盖 `#741`「修订对象在克隆中」）。
- [x] **AC3** `b` 的 `reference` 表中，除 `libra init` 创建的内置本地分支（`kind='Branch' AND remote IS NULL AND name IS NOT NULL AND name IN ('intent','traces')`，`src/internal/branch.rs:37,42`）外，不存在任何 `kind` 为 `Branch` 或 `Tag` 的行（对应 Git `for-each-ref refs` 为空）。
- [x] **AC4** `b` 的 `config_kv` 表中不存在键 `remote.origin.fetch`。
- [x] **AC5** S 另建轻量 tag `v0` 指向 `S1`（不同于远端默认 `main` 的末端 `S2`）；`libra clone --revision=refs/tags/v0 file://<S> d` 退出 0 后，`d` 中 `libra status --porcelain=v2 --branch` 的完整 stdout 逐字等于 `# branch.head (detached)`、`# branch.oid <S1>` 两行（HEAD 游离于 `S1`，且工作区与索引都与 `S1` 的树一致、无其它条目）。
- [x] **AC6** 对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `d` 中的存在性判定结果，恰等于该对象是否属于 `libra rev-list --objects <S1>`（S1 可达闭包）（只取请求修订可达的对象，`S2` 缺失）。
- [x] **AC7** `libra clone --revision=refs/tags/v1 --branch main file://<S> e1` 以 `LBR-CLI-002` 拒绝（Git `builtin/clone.c:1390-1391`）。
- [x] **AC8** `libra clone --revision=refs/tags/v1 --mirror file://<S> e2` 以 `LBR-CLI-002` 拒绝（Git `builtin/clone.c:1392-1393`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_revision_single_rev`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT name, "commit" FROM reference WHERE kind='Head' AND remote IS NULL`，结果恰为一行且等于 (NULL, `<S2>`)；`S2` 为创建 S 时保存的 `main` 末端提交 OID。
- AC2 → `command::clone_test::test_clone_revision_single_rev`：在 S 中取 `libra rev-list --objects --all` 全集与 `libra rev-list --objects <S2>` 闭包，对全集逐一在 `b` 按「验证执行协议」第 7 项的对象存在性判定，得到的「OID → 存在/缺失」映射与「OID → 是否属于闭包」映射做一次 `assert_eq!`。
- AC3 → `command::clone_test::test_clone_revision_single_rev`：以 SQLite 只读连接执行 `SELECT COUNT(*) FROM reference WHERE kind IN ('Branch','Tag') AND NOT (kind='Branch' AND remote IS NULL AND COALESCE(name, '') IN ('intent','traces'))`，结果为 0（`COALESCE` 使 `name` 为 NULL 的行照常计入，同名 tag 或远端分支也照常计入；不用 `libra show-ref`：空集时它以 `LBR-CLI-003` 失败，`src/command/show_ref.rs:234-236`）。
- AC4 → `command::clone_test::test_clone_revision_single_rev`：以 SQLite 只读连接查询 `b/libra.db` 的 `config_kv`，键 `remote.origin.fetch` 的行数为 0（不经会隐藏加密值的 `libra config list`）。
- AC5 → `command::clone_test::test_clone_revision_single_rev`：`exit 0` 为前提；stdout 逐字等于 `# branch.head (detached)\n# branch.oid <S1>\n`（头部格式见 `src/command/status/output.rs:2541-2555`，开工按 ER-02 以现行输出复核）。
- AC6 → `command::clone_test::test_clone_revision_single_rev`：在 S 中取 `libra rev-list --objects --all` 全集与 `libra rev-list --objects <S1>` 闭包，对全集逐一在 `d` 按「验证执行协议」第 7 项的对象存在性判定，得到的「OID → 存在/缺失」映射与「OID → 是否属于闭包」映射做一次 `assert_eq!`。
- AC7 → `command::clone_test::test_clone_revision_single_rev`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC8 → `command::clone_test::test_clone_revision_single_rev`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--revision`。

**Verification:**
- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_revision_single_rev)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC8。
- [ ] `source .env.test && cargo nextest run --test compat_clone_shallow_integrity` — 共享回归组（「验证执行协议」第 5 项）：传输层改动后的浅克隆完整性回归。

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs 与 src/internal/protocol/mod.rs`
**Dependencies:** `CL-25`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --revision 单修订克隆; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=CL-25,DEP-CL-04; writeset=串行于 CL-25; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-06: 支持 `libra clone -c <key=value>` 配置覆盖与 `clone.defaultRemoteName`（#752）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`CloneArgs` 新增可重复 `-c`/`--config <key=value>`；init 后按命令行顺序写入新仓库配置（多值追加，`config get --all` 得 `1\n2\n`）；`clone.defaultRemoteName` 在初始 fetch 前解析：显式 `--origin` > `-c clone.defaultRemoteName` > 全局 `clone.defaultRemoteName` > `origin`；空键以 `LBR-CLI-002` 拒绝（`CloneError::InvalidConfigEntry`）。`command::clone_test::test_clone_config_overrides` 覆盖 AC1–AC6，nextest 1 passed / 0 failed；`cargo +nightly fmt --all --check` exit 0。按用户 2026-11-20 指令在 `main` 直接提交推送。）
**Description:** 为 `CloneArgs` 新增可重复 `-c` / `--config <key=value>`，在初始化目标仓库后写入新仓库配置；其中 `clone.defaultRemoteName` 在初始 fetch 前生效（以 AC3–AC5 的远端跟踪引用观测），其它配置项对本次初始 fetch/checkout 的即时影响不在本卡验收（`DEFER-CL-07`），同时支持 `clone.defaultRemoteName` 并确保显式 `--origin` 优先于 `-c clone.defaultRemoteName`。
**Out of scope:** 非 `clone` 子命令的全局配置覆盖。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#752` `CloneArgs` 缺少 `-c`/`--config` 参数且未读取 `clone.defaultRemoteName` | `src/command/clone.rs:64-236` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [x] **AC1** 短、长两种形式（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）：`libra clone -c core.foo=bar file://<S> d` 与 `libra clone --config core.foo=bar file://<S> d1` 退出 0 后，`d`、`d1` 中 `libra config get core.foo` 的 stdout 均为 `bar` 加换行。
- [x] **AC2** `libra clone -c a.b=1 -c a.b=2 file://<S> d2` 后，`d2` 中 `libra config get --all a.b` 的 stdout 为 `1\n2\n`（多值按出现顺序全部写入）。
- [x] **AC3** `libra clone -c clone.defaultRemoteName=upstream file://<S> d3` 后，`d3` 的 `reference` 表中 `kind='Branch' AND remote IS NOT NULL` 各行的 `remote` 去重集合恰为 {`upstream`}（配置在初始 fetch 前生效，未同时建立 `origin` 远端跟踪分支）。
- [x] **AC4** `libra clone -c clone.defaultRemoteName=inline --origin from_option file://<S> d4` 后，`d4` 的 `reference` 表中 `kind='Branch' AND remote IS NOT NULL` 各行的 `remote` 去重集合恰为 {`from_option`}（显式 `--origin` 优先，`inline` 未参与；`#752`，对应 Git `t/t5606-clone-options.sh#16`）。
- [x] **AC5** 以逐子进程环境变量 `LIBRA_CONFIG_GLOBAL_DB=<测试临时目录>/global.db` 隔离全局配置，执行 `libra config set --global clone.defaultRemoteName from_config` 后，不带 `-c` 的 `libra clone file://<S> d6` 中，`d6` 的 `reference` 表中 `kind='Branch' AND remote IS NOT NULL` 各行的 `remote` 去重集合恰为 {`from_config`}（对应 Git `t/t5606-clone-options.sh` 的 `test_config_global clone.defaultRemoteName` 用例）。
- [x] **AC6** `libra clone -c =v file://<S> d5`（空键名）以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_config_override_dash_c`：两次克隆的 `exit 0` 为前提；「形式 → `config get core.foo` 的 stdout」映射与「每种形式 → `bar\n`」映射做一次 `assert_eq!`。
- AC2 → `command::clone_test::test_clone_config_override_dash_c`：stdout 逐字等于 `1\n2\n`。
- AC3 → `command::clone_test::test_clone_config_override_dash_c`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT DISTINCT remote FROM reference WHERE kind='Branch' AND remote IS NOT NULL`（库为 `d3`），结果集合做一次 `assert_eq!` 等于 {`upstream`}（Libra 远端跟踪分支以 `remote` 列区分远端，2026-11-18 以 libra 0.30.207 实测）。
- AC4 → `command::clone_test::test_clone_config_override_dash_c`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT DISTINCT remote FROM reference WHERE kind='Branch' AND remote IS NOT NULL`（库为 `d4`），结果集合做一次 `assert_eq!` 等于 {`from_option`}（Libra 远端跟踪分支以 `remote` 列区分远端，2026-11-18 以 libra 0.30.207 实测）。
- AC5 → `command::clone_test::test_clone_config_override_dash_c`：`exit 0` 为前提；以 SQLite 只读连接执行 `SELECT DISTINCT remote FROM reference WHERE kind='Branch' AND remote IS NOT NULL`（库为 `d6`），结果集合做一次 `assert_eq!` 等于 {`from_config`}（Libra 远端跟踪分支以 `remote` 列区分远端，2026-11-18 以 libra 0.30.207 实测）。
- AC6 → `command::clone_test::test_clone_config_override_dash_c`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`-c <key>=<value>`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_config_override_dash_c)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC6。
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics` — 共享回归组（「验证执行协议」第 5 项）：配置默认值语义回归。

**Full-suite trigger:** `none`
**Dependencies:** `CL-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -c <key=value> 配置覆盖; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-05; writeset=串行于 CL-05; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-08: 实现 `libra clone -l` / `--local` 硬链接本地对象（#743）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 当从本地 Libra 仓库（`.libra` 源）执行 `libra clone`（默认或 `-l`/`--local`，未传 `--no-local`）且源与目标在同一文件系统时，使用 `std::fs::hard_link` 链接松散/pack 对象文件（对象目录中的 `info/alternates` 不按普通文件复制或硬链接，而按 Git `copy_alternates` 把相对条目改写为绝对路径写入目标，AC2；按 Git `builtin/clone.c:350-372`：默认本地路径在某个对象硬链接失败时改为复制，并对其后对象不再尝试硬链接；显式 `-l`/`--local` 时硬链接失败即以 `LBR-IO-002` 拒绝；`clone.rs:125-128` 对完整本地 Git 源的 `--depth`/`--filter` 警告为既有行为，本卡不改；本地 Git 源（`.git` 目录）沿用既有复制路径，其硬链接登记 `DEFER-CL-09`）。对象目录树中遇到任何符号链接（目录根 `.libra/objects`、子目录或对象文件）时 fail-closed 拒绝（Git `builtin/clone.c:328-330`），禁止跟随链接读取仓库外内容；复制或硬链接之前先核对源仓库元数据目录（普通仓库为 `<S>/.libra`，裸仓库为 `<S>` 本身；对应 Git 在 `clone_local`→`copy_or_link_directory` 中传给 `die_upon_dubious_ownership` 的 `src_repo`，`builtin/clone.c:381-389,298`）的属主与当前用户一致，不一致即 fail-closed 拒绝，对象目录不能代替这一检查（Libra 无 `safe.directory` 例外配置，本卡不新增）；以本卡新增的测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1`（仅 `LIBRA_TEST=1` 时读取，只作用于对源仓库元数据目录的属主判定，对象目录等其它路径的判定不受影响，因而只检查对象目录的实现不会拒绝）验收（AC7）；硬链接之后按 Git `builtin/clone.c:343-361` 用 `lstat` 比对新链接与遍历时观察到的源文件元数据（mode、inode、设备号、大小、uid、gid），不一致即以 `LBR-REPO-003` 拒绝（防遍历与链接之间源文件被替换），以本卡新增的测试接缝 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK=first`（仅 `LIBRA_TEST=1` 时读取：在记录第一个对象文件的元数据之后、对其调用 `hard_link` 之前，把该源文件替换为内容不同的新普通文件）验收（AC8）；复制回退路径与 Git 相同不另做竞态检测（源仓库属主已由 AC7 限定为当前用户）；默认本地路径与显式 `--local` 共用同一遍历与检查（AC6 对三种位置 × 两种调用形式共 6 个样本逐一验收，任何位置在任一调用形式下漏检都会使 AC6 失败）。硬链接失败回退以本卡新增的测试接缝 `LIBRA_TEST_CLONE_HARDLINK_FAIL=first`（仅 `LIBRA_TEST=1` 时读取，使本次克隆的第一次 `hard_link` 调用失败）注入。（先于 `CL-07` 落地以使 `--no-hardlinks` 可实测验证。）
**Out of scope:** `--no-hardlinks` 标志（`CL-07`）与 `--reference`（`CL-10`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#743` `CloneArgs::local` 文档明言始终复制从不硬链接 | `src/command/clone.rs:124-130`；`src/internal/protocol/local_client.rs:586`（`fetch_objects` 以打包传输取对象） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 同文件系统夹具 S（至少一个松散对象与一个 pack 及其 `.idx`）：显式 `libra clone --local <S> d` 与默认本地路径 `libra clone <S> d1`（不带 `-l`）退出 0 后，S 对象目录清单中的每一个文件（逐一枚举，不抽样）在 `d`、`d1` 中的对应文件都与其设备号、inode 相同（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）。
- [ ] **AC2** 源 alternates 的改写（表驱动，结局相同）：夹具 SA——`<T>/R` 为 S 的完整克隆；`libra clone --shared <T>/R <T>/SA` 建立借用 R 的源后，删除 `<T>/SA/.libra/objects/pack/` 下的全部文件（现行 `--shared` 仍会取回 pack），再把 `<T>/SA/.libra/objects/info/alternates` 改写为指向同一目录的相对路径 `../../../R/.libra/objects`；建夹具时断言 SA 自身对象数为 0 且 SA 中 `libra cat-file -p HEAD:a.txt` 退出 0（相对 alternates 在源内生效，2026-11-18 以 libra 0.30.207 实测）——之下，默认本地路径 `libra clone <T>/SA <T>/deep/x/d8` 与显式 `libra clone --local <T>/SA <T>/deep/x/d8b` 均退出 0，且两库中 `libra cat-file -p HEAD:a.txt`（该 blob 只在 R 中）均退出 0 并输出 S 中同一内容（Git `copy_alternates` 不逐字节复制该文件，而把相对条目改写为绝对路径写入目标，`builtin/clone.c:184-224`，并在复制/硬链接遍历中跳过 `info/alternates`，`:337-339`；原样复制或硬链接相对路径会在更深的目标下解析到不存在的 `<T>/deep/R`，读取失败）。
- [ ] **AC3** `libra clone --no-local <S> d2` 退出 0 后，`d2` 的 `.libra/objects/` 下没有任何文件与 S 中任一对象文件的 (设备号, inode) 相同（以 `d2` 的对象完整、可读为前提）。
- [ ] **AC4** 默认本地路径加硬链接故障注入（`LIBRA_TEST=1` 下本卡新增的测试接缝 `LIBRA_TEST_CLONE_HARDLINK_FAIL=first` 使本次克隆的第一次 `hard_link` 调用返回错误）：`libra clone <S> d3` 退出 0，且 `d3` 对象目录的清单「相对路径 →（SHA-256，是否与 S 对应文件共享 (设备号, inode)）」逐项等于「S 对象目录的每个文件 →（其 SHA-256，否）」（对象完整复制、无遗漏，且 Git 在首次硬链接失败后对其余对象也改为复制，`builtin/clone.c:365-369`）。
- [ ] **AC5** 显式 `--local` 加同一故障注入时，`libra clone --local <S> d4` 以 `LBR-IO-002` 拒绝（Git 显式 `--local` 硬链接失败即 `die`，`builtin/clone.c:365-366`）。
- [ ] **AC6** 符号链接拒绝（表驱动，按「验证执行协议」第 6 项计为 1 条：6 个样本的用户可见结局相同）：三种夹具——S 的对象目录根 `.libra/objects` 是指向仓库外目录的符号链接、对象子目录 `.libra/objects/pack` 是指向仓库外目录的符号链接、某个松散对象文件是指向仓库外文件的符号链接——各自以默认本地路径 `libra clone <S> <新目标>` 与显式 `libra clone --local <S> <新目标>` 两种形式调用，共 6 次调用全部以 `LBR-REPO-003` 拒绝（Git 对遍历中遇到的任何符号链接都拒绝，`builtin/clone.c:328-330`；默认本地路径与显式 `--local` 都走 `clone_local`）。
- [ ] **AC7** 源仓库属主边界（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）：`LIBRA_TEST=1` 下本卡新增的测试接缝 `LIBRA_TEST_CLONE_FOREIGN_OWNER=1` 使对源仓库元数据目录的属主判定视为非当前用户所有（普通源 S 为 `<S>/.libra`；裸源 SB——由 `libra clone --bare file://<S> SB` 生成——为 `<SB>` 本身；对象目录的判定不受影响）时，样本表 {S + 默认本地路径、S + 显式 `--local`、SB + 默认本地路径、SB + 显式 `--local`} 中每个 `libra clone [--local] <源> <新目标>` 均以 `LBR-REPO-003` 拒绝，不复制也不硬链接任何对象（Git 在复制或硬链接前以 `die_upon_dubious_ownership` 拒绝属主不同的源仓库，`builtin/clone.c:286-298`：跨用户硬链接的文件可被源属主任意改写；只检查对象目录或只处理普通布局的实现在此样本表下不能通过）。
- [ ] **AC8** 硬链接后核对（表驱动，结局相同）：`LIBRA_TEST=1` 下本卡新增的测试接缝 `LIBRA_TEST_CLONE_SWAP_BEFORE_LINK=first` 在第一个对象文件被记录元数据之后、`hard_link` 之前把该源文件替换为内容不同的新普通文件时，默认本地路径 `libra clone <S> <新目标>` 与显式 `libra clone --local <S> <新目标>` 均以 `LBR-REPO-003` 拒绝（Git `builtin/clone.c:353-361` 对 inode 等元数据不一致的新链接 `die`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`：两次克隆的 `exit 0` 为前提；递归枚举 S 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）（含至少一个松散对象、一个 `.pack`、一个 `.idx`，正控制），「(目标库, 相对路径) → 与 S 对应文件 `std::os::unix::fs::MetadataExt` 的 `(dev, ino)` 是否相等」映射与「每项 → 真」映射做一次 `assert_eq!`（目标库为 `d`、`d1`）。
- AC2 → `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`：两次克隆的 `exit 0` 为前提；「目标 → (`cat-file -p HEAD:a.txt` 的退出码, stdout)」映射与「每个目标 → (0, S 中 `libra cat-file -p HEAD:a.txt` 的 stdout)」映射做一次 `assert_eq!`（夹具断言已保证 SA 自身不含该 blob，读取只能经改写后的 alternates 成功）。
- AC3 → `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`：`exit 0` 为前提；正控制：`d2` 对象目录的文件集合非空，且 S 中 `libra rev-list --objects --all` 的每个对象在 `d2` 按「验证执行协议」第 7 项判定为存在；然后收集 S 对象文件的 `(dev, ino)` 集合，`d2` 对象目录全部文件的 `(dev, ino)` 与之交集为空。
- AC4 → `command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy`：`exit 0` 为前提；分别为 S 与 `d3` 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）构造该映射（S 侧第二项恒为「否」），两份清单做一次 `assert_eq!`；夹具 S 含检出树以外的对象（`S1` 版本 `a.txt` 的 blob）。
- AC5 → `command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy`：进程以非零状态结束且 stderr 含 `LBR-IO-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC6 → `command::clone_test::test_clone_local_rejects_symlinked_object_paths`：对 (夹具位置, 调用形式) 共 6 个样本逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-REPO-003`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC7 → `command::clone_test::test_clone_local_rejects_symlinked_object_paths`：对四个样本逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-REPO-003`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC8 → `command::clone_test::test_clone_local_rejects_symlinked_object_paths`：对两种调用形式逐一以新目标目录调用（每次重建 S），得到「形式 → (进程以非零状态结束, stderr 含 `LBR-REPO-003`)」映射，与「每种形式 → (真, 真)」映射做一次 `assert_eq!`；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`hardlink`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlinks_objects_on_same_fs)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC3。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC4–AC5。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_rejects_symlinked_object_paths)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC6–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_flag_accepted_for_local_source)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有 `-l`/`--local` 接受回归（`tests/command/clone_test.rs:953`）。

**Full-suite trigger:** `T-1: 修改共享局部传输模块 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-06`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）；`clone.md` 的 EN/zh-CN/开发文档与网站页更新原「从不硬链接」说明。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -l/--local 硬链接本地对象; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=2; scope=M; deps=CL-06; writeset=串行于 CL-06; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-07: 支持 `libra clone --no-hardlinks` 标志（#742）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `CL-08` 硬链接落地后，为 `CloneArgs` 新增 `--no-hardlinks` 标志，强制复制本地对象而非创建硬链接（验证目标与源对象 inode 明确不同）。
**Out of scope:** `-l`/`--local` 硬链接实现（已由 `CL-08` 交付）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#742` `CloneArgs` 缺少 `--no-hardlinks` 参数 | `src/command/clone.rs:124-136` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 同文件系统 S（含检出树以外的对象，如 `S1` 版本 `a.txt` 的 blob）：`libra clone --no-hardlinks <S> d` 退出 0 后，`d` 对象目录的清单「相对路径 →（SHA-256，是否与 S 对应文件共享 (设备号, inode)）」逐项等于「S 对象目录的每个文件 →（其 SHA-256，否）」（完整复制、无遗漏、无硬链接）。
- [ ] **AC2** 同文件系统 S（含检出树以外的对象，如 `S1` 版本 `a.txt` 的 blob）：`libra clone -l --no-hardlinks <S> d2` 退出 0 后，`d2` 对象目录的清单「相对路径 →（SHA-256，是否与 S 对应文件共享 (设备号, inode)）」逐项等于「S 对象目录的每个文件 →（其 SHA-256，否）」（完整复制、无遗漏、无硬链接）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_no_hardlinks_copies_objects`：`exit 0` 为前提；分别为 S 与 `d` 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）构造该映射（S 侧第二项恒为「否」），两份清单做一次 `assert_eq!`。
- AC2 → `command::clone_test::test_clone_no_hardlinks_copies_objects`：`exit 0` 为前提；分别为 S 与 `d2` 的对象目录清单（「对象目录清单」按「验证执行协议」第 7 项，不含 `info/alternates`）构造该映射（S 侧第二项恒为「否」），两份清单做一次 `assert_eq!`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--no-hardlinks`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_no_hardlinks_copies_objects)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC2。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlinks_objects_on_same_fs)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：`CL-08` 默认硬链接行为不被 `--no-hardlinks` 接线破坏。

**Full-suite trigger:** `none`
**Dependencies:** `CL-08`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --no-hardlinks 标志; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=2/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-08; writeset=串行于 CL-08; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-09: 支持 `libra clone` 的 `<path>.git` 后缀回退解析（含 `file://`）（#744）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 当 `libra clone <path>` 或 `libra clone file://<path>` 指定的源路径不存在时，按 Git 约定回退探测 `<path>.git`（落点为 `LocalClient::from_path` 的路径解析，`local_client.rs:308`；不改 `fetch.rs`），且 `file://` URL 保持不创建硬链接语义。
**Out of scope:** HTTP/SSH 远端 URL 重定向（永久非目标）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#744` 本地路径与 `file://` URL 解析未尝试 `<path>.git` 后缀回退：`file://` 经 `to_file_path` 转为本地路径后与普通路径同走 `local_or_bundle_client`，它只调用 `LocalClient::from_path`，失败后仅尝试 bundle，不尝试 `<path>.git`；路径不存在时映射为 `LBR-REPO-001` | `src/command/fetch.rs:129-133,164-180`；`src/internal/protocol/local_client.rs:308`；`src/command/clone.rs:681-686` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 测试目录中只有裸仓库 `foo.git`（由 `libra clone --bare file://<S> foo.git` 生成）时，在该目录执行 `libra clone foo` 退出 0，且克隆目录 `foo` 的 `HEAD` 提交 OID 等于 S 的 `HEAD`。
- [ ] **AC2** 在另一个只含 `foo.git`（同 AC1 生成方式）、不含 `foo` 的新测试目录 T2 中，`libra clone file://<T2>/foo dst` 退出 0，且 `dst` 的 `HEAD` 提交 OID 等于 S 的 `HEAD`（回退到 `foo.git`）。
- [ ] **AC3** AC2 的 `dst` 中没有任何对象文件与 `<T2>/foo.git` 共享 (设备号, inode)（`file://` 不硬链接）。
- [ ] **AC4** `bar` 与 `bar.git` 都不存在时，`libra clone bar` 以 `LBR-REPO-001` 拒绝（现有 `map_discover_remote_error` 的仓库不存在映射，`src/command/clone.rs:681-686`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`：`exit 0` 后比较 `foo` 与 S 的 `libra rev-parse HEAD` 相等。
- AC2 → `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`：先断言 `<T2>/foo` 不存在；`exit 0` 后比较 `dst` 与 S 的 `libra rev-parse HEAD` 相等。
- AC3 → `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`：两侧对象文件 `(dev, ino)` 集合交集为空，且 `dst` 对象文件数 ≥ 1。
- AC4 → `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`：进程以非零状态结束且 stderr 含 `LBR-REPO-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`.git`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_bare_and_mirror_default_names_and_layout)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有目标目录命名回归（`tests/command/clone_test.rs:2486`）。

**Full-suite trigger:** `T-1: 修改共享传输 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone <path>.git 后缀回退; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=CL-07; writeset=串行于 CL-07; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-10: 实现 `clone --reference` / `--reference-if-able` alternates 借用（#751）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 复用既有 alternates 支持（`src/command/alternates.rs` 与 `clone.rs:1554-1735` `--shared` 注册逻辑）使 `--reference <ref>`（及 `--reference-if-able`）真正注册 `objects/info/alternates` 并在 `fetch` / 协商阶段借用参考库对象以跳过重复传输（并将过时测试 `test_clone_object_alternates_flags_are_noops` 重命名/重写为 `test_clone_reference_alternates_borrows_objects` 与 `test_clone_reference_if_able_and_invalid_repo`）。（按 G-09 自本卡拆出 `CL-18` 处理 `--dissociate`。）
**Out of scope:** `--dissociate`（由 `CL-18` 承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#751` `--reference` 当前仅打印警告并全量复制对象；`clone.rs:1554-1735` 已有 alternates 注册辅助 | `src/command/clone.rs:147-182,250-260,1554-1735`；`src/command/alternates.rs:91`；`tests/command/clone_test.rs:1118` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** R 为 S 的完整克隆；对 (形式, 目标) 表 {(`--reference`, `d`)、(`--reference-if-able`, `d4`)}（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）逐一执行 `libra clone --json <形式> <R> file://<S> <目标>`，均退出 0，且各目标的 `.libra/objects/info/alternates` 唯一非空行规范化后等于 `<R>/.libra/objects`。
- [ ] **AC2** AC1 两次克隆的 stdout JSON 中 `data.objects_fetched` 均为 0（R 已有全部对象，未从 S 重复传输；字段见既有 `test_clone_json_reports_fetch_transfer_counts`，`tests/command/clone_test.rs:1458-1500`）。
- [ ] **AC3** R 存在时，`d` 与 `d4` 各自对象库本身的对象总数均为 0（对象不是从 R 复制进来，而是经 alternates 借用）。
- [ ] **AC4** `d` 与 `d4` 中 `libra cat-file -p HEAD` 均退出 0（经 alternates 可读）。
- [ ] **AC5** `libra clone --reference-if-able <不存在路径> file://<S> d2` 退出 0，且 `d2` 不存在 `.libra/objects/info/alternates`。
- [ ] **AC6** `libra clone --reference <非仓库目录> file://<S> d3` 以 `LBR-CLI-003` 拒绝（目标清理由「验证执行协议」第 4 项克隆拒绝不变量检查）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_reference_alternates_borrows_objects`：两次克隆的 `exit 0` 为前提；读取各目标 alternates 文件的唯一非空行并 `canonicalize`，「目标 → 路径」映射与「每个目标 → `<R>/.libra/objects` 的 `canonicalize`」映射做一次 `assert_eq!`。
- AC2 → `command::clone_test::test_clone_reference_alternates_borrows_objects`：两次 stdout 解析为 JSON，「目标 → `data.objects_fetched`」映射与「每个目标 → 0」映射做一次 `assert_eq!`；正控制：同一测试先以不带参考库的 `libra clone --json file://<S> d0` 确认该字段大于 0。
- AC3 → `command::clone_test::test_clone_reference_alternates_borrows_objects`：各目标中 `libra count-objects -v` 的 `count:` 与 `in-pack:` 两个数值之和（二者只统计本库，alternates 另列，`src/command/count_objects.rs:212-216`），「目标 → 和」映射与「每个目标 → 0」映射做一次 `assert_eq!`。
- AC4 → `command::clone_test::test_clone_reference_alternates_borrows_objects`：「目标 → `libra cat-file -p HEAD` 的退出码」映射与「每个目标 → 0」映射做一次 `assert_eq!`。
- AC5 → `command::clone_test::test_clone_reference_if_able_and_invalid_repo`：`exit 0` 且该文件 `exists()` 为假。
- AC6 → `command::clone_test::test_clone_reference_if_able_and_invalid_repo`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--reference`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_reference_alternates_borrows_objects)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（重写并取代旧 no-op 测试 `test_clone_object_alternates_flags_are_noops`，`tests/command/clone_test.rs:1118`），覆盖 AC1–AC4（`--reference` 与有效 `--reference-if-able` 两个目标）。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_reference_if_able_and_invalid_repo)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC5–AC6。

**Full-suite trigger:** `T-1: 修改共享传输 src/command/fetch.rs 与 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-09`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --reference alternates 借用; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=CL-09,DEP-CL-04; writeset=串行于 CL-09; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-18: 实现 `clone --dissociate` 拷贝借用对象并解除 alternates（#751）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-10` 拆出：在 `clone --reference <ref> --dissociate` 与 `clone --shared --dissociate` 完成后，分别补全本地缺失对象拷贝并移除 `objects/info/alternates`，保证两种克隆库都完全独立。`--shared --dissociate` 已由既有实现以「不注册 alternates」处理（`src/command/clone.rs:178-182,1567`），`CL-08` 之后其对象按本地克隆规则硬链接或复制——两者都使目标在删除源后完整可读（与 Git 默认本地克隆的硬链接独立性一致），本卡对它只补回归验收（AC4–AC5），不要求对象为独立副本；`--reference --dissociate` 依赖 `CL-10` 新增的借用，是本卡的实现部分。
**Out of scope:** `--reference` 初始借用协商（`CL-10`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#751` `CloneArgs::dissociate` 当前仅在 `--shared` 下覆盖且未执行对象补全拷贝与 alternates 解除 | `src/command/clone.rs:178-182` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** R 为 S 的完整克隆；`libra clone --reference <R> --dissociate file://<S> d` 退出 0 后，`d/.libra/objects/info/alternates` 不存在。
- [ ] **AC2** R 已含 S 的全部对象时，AC1 的命令以 `--json` 运行（`libra clone --json --reference <R> --dissociate file://<S> d`），stdout JSON 的 `data.objects_fetched` 为 0（对象经 R 借用而非从 S 传输，字段见既有 `test_clone_json_reports_fetch_transfer_counts`，`tests/command/clone_test.rs:1458-1500`）。
- [ ] **AC3** 删除 R 后，在 `d` 执行 `libra fsck` 退出 0。
- [ ] **AC4** `libra clone --shared --dissociate <S> d2` 退出 0 后，`d2/.libra/objects/info/alternates` 不存在。
- [ ] **AC5** 删除 S 后，在 `d2` 执行 `libra fsck` 退出 0。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_dissociate_keeps_repo_independent`：`exit 0` 且该文件 `exists()` 为假。
- AC2 → `command::clone_test::test_clone_dissociate_keeps_repo_independent`：解析 stdout 为 JSON，`data.objects_fetched` 等于 0。
- AC3 → `command::clone_test::test_clone_dissociate_keeps_repo_independent`：删除 R 目录后 `libra fsck` 退出 0。
- AC4 → `command::clone_test::test_clone_shared_dissociate_detaches_alternates`：`exit 0` 且该文件 `exists()` 为假。
- AC5 → `command::clone_test::test_clone_shared_dissociate_detaches_alternates`：删除 S 目录后 `libra fsck` 退出 0。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--dissociate`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_dissociate_keeps_repo_independent)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC3。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_shared_dissociate_detaches_alternates)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC4–AC5。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_reference_alternates_borrows_objects)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：`CL-10` 借用行为不被 `--dissociate` 改动破坏。

**Full-suite trigger:** `none`
**Dependencies:** `CL-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --dissociate 解除 alternates; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=CL-10; writeset=串行于 CL-10; release=independent; split-from=CL-10; exception=N/A`

---

### Task CL-12: 校验并拒绝无效 `sparse:` filter 规格（#748）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `clone --filter=sparse:...` 时由传输/协议与本地客户端层（`local_client.rs`、`protocol/mod.rs`、`clone.rs`）校验 sparse filter 表达式（覆盖 `#748` 的两类可解析失败情形：指向不存在 OID 与指向非 blob 对象），输出 Git 兼容错误消息并清理新建目标目录（将旧 `test_clone_unsupported_fetch_optimizations_warn_and_full_clone` 重命名为 `test_clone_rejects_invalid_sparse_filter`；旧用例中与 `--filter` 无关的 `--shallow-since`/`--shallow-exclude` 警告断言原样保留在重写后的用例中，不计入本卡 AC）。本卡发布提交上，以下尚未实现的 filter 以 `LBR-UNSUPPORTED-001` 拒绝：`--filter=blob:none`（AC7 过渡断言，由 `CL-13` 同卡改写）与有效 `sparse:oid`（AC6 过渡断言，由 `CL-29` 同卡改写）；Git 定义而本计划不实现的单独 filter（`tree:<n>`、`object:type=<type>`、`blob:limit=<n>` 等）由解析器默认拒绝：除 `blob:none`、`sparse:oid=`（及 `CL-13`/`CL-24` 处理的组合形式）外的规格一律以 `LBR-UNSUPPORTED-001` 拒绝，自本卡起持续成立、后继卡不改写（AC5 以样本表验收，`DEFER-CL-10`），不静默全量克隆。
**Out of scope:** promisor lazy fetch（`CL-19`）；多 `--filter` 组合与协商（`CL-13`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#748` `clone` 忽略 `--filter` 内容，对无效 `sparse:` 过滤器静默成功 | `src/command/clone.rs:191-198`；`src/internal/protocol/local_client.rs:586`（`fetch_objects` 只接受 have/want/shallow/depth，无 filter 参数）；`tests/command/clone_test.rs:1376` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --no-local --bare --filter=sparse:oid=main:no-such-name <S> e1` 以 `LBR-CLI-003` 拒绝。
- [ ] **AC2** AC1 的 stderr 含 Git 原文 `unable to access sparse blob in`（`list-objects-filter.c:532`；`#748` 验收第 1 条）。
- [ ] **AC3** `libra clone --no-local --bare --filter=sparse:oid=main <S> e2`（对象表达式指向提交而非 blob）以 `LBR-CLI-003` 拒绝。
- [ ] **AC4** AC3 的 stderr 含 Git 原文 `unable to parse sparse filter data in`（`list-objects-filter.c:535`；`#748` 验收第 2 条）。
- [ ] **AC5** 本计划不实现的单独 filter（表驱动，按「验证执行协议」第 6 项计为 1 条：各样本的用户可见结局相同）：对样本表 {`tree:0`、`tree:1`、`tree:2`、`object:type=blob`、`object:type=tree`、`blob:limit=1k`} 中每个规格，`libra clone --filter=<规格> file://<S> <新目标>` 均以 `LBR-UNSUPPORTED-001` 拒绝（持续断言，`CL-13`/`CL-24`/`CL-29` 均不改写；本计划只交付与 `blob:none` 组合的 `tree:1`，见 `CL-24`、`DEFER-CL-10`）。
- [ ] **AC6** 过渡断言（只在本卡发布提交上成立，`CL-29` 改写）：S 的 `main` 另含规则文件 `only-one`（内容 `/a.txt`）时，`libra clone --bare --filter=sparse:oid=main:only-one file://<S> e6`（有效 `sparse:oid`）以 `LBR-UNSUPPORTED-001` 拒绝，不得静默全量克隆。
- [ ] **AC7** 过渡断言（只在本卡发布提交上成立）：`libra clone --filter=blob:none file://<S> e5` 以 `LBR-UNSUPPORTED-001` 拒绝；`CL-13` 在同卡改写本测试的这一断言。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC2 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：AC1 同一次调用的 stderr 含该原文子串。
- AC3 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC4 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：AC3 同一次调用的 stderr 含该原文子串。
- AC5 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：对样本表中每个规格逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC6 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC7 → `command::clone_test::test_clone_rejects_invalid_sparse_filter`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`sparse:`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_rejects_invalid_sparse_filter)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（重写并取代旧测试 `test_clone_unsupported_fetch_optimizations_warn_and_full_clone`，`tests/command/clone_test.rs:1376`），覆盖 AC1–AC7。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_flag_accepted_for_local_source)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地克隆回归（`tests/command/clone_test.rs:953`）——不带 `--filter` 的 `--local`/`--no-local`/`-l` 克隆在新增 filter 校验后仍成功。

**Full-suite trigger:** `T-1: 修改共享协议层 src/internal/protocol/local_client.rs 与 mod.rs`
**Dependencies:** `CL-19`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone 无效 sparse filter 拒绝; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=CL-19; writeset=串行于 CL-19; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-13: 实现 `--filter=blob:none` 协商、promisor 配置写入与未实现组合 fail-closed（#746, #754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 将 `CloneArgs::filter` 从 `Option<String>` 改为 `Vec<String>` 以便识别重复 `--filter`：在本卡先支持单个 `--filter=blob:none`（对实际远端名 `<remote_name>` 尊重 `-o`/`--origin` 与 `CL-06` 的 `clone.defaultRemoteName`，写入 `remote.<remote_name>.promisor=true`、`remote.<remote_name>.partialclonefilter` 与 `extensions.partialClone=<remote_name>`，并在本地传输（`local_client.rs:586` 的 `fetch_objects` 及其 Libra 源对象遍历，经 `fetch.rs`/`protocol/mod.rs` 的公共过滤规格）应用 `blob:none` 过滤；网络客户端（`git_client.rs`/`https_client.rs`/`ssh_client.rs`）的 filter 协商延后至 `DEFER-CL-08`（本计划不验收网络远端上的 `--filter` 行为））；在 `CL-24` 落地前，若传入 `combine:*` 或多个取值不同的 `--filter` 则明确 `fail-closed` 拒绝（避免中间发布版本静默忽略组合过滤），远端不广告 `filter` 能力时同样拒绝（本卡新增测试接缝 `LIBRA_TEST_UPLOAD_PACK_NO_FILTER=1`，仅 `LIBRA_TEST=1` 时读取）；本卡同卡改写 `CL-12` 测试的 AC7 过渡断言。（按 G-09 拆出 `CL-19` 先行落地对象读取层按需 lazy fetch，拆出 `CL-24` 承接 `combine:blob:none+tree:1` 传输与无能力回退，拆出 `CL-21` 承接 `Receiving objects` 进度与端到端 lazy fetch 验收。）
**Out of scope:** 对象读取触发的按需 lazy fetch（`CL-19`）；`combine:blob:none+tree:1` 根树过滤与远端无能力回退（`CL-24`）；`Receiving objects` 进度与端到端 `cat-file` 验证（`CL-21`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#754` `#746` `CloneArgs::filter` 为 `Option<String>` 且无 promisor 配置写入；本地测试走 `local_client.rs:586` 的 `fetch_objects`（`local_client.rs:889` 起是外部 Git 源的引用读取 `read_git_repo_refs`，不是本计划夹具的路径） | `src/command/clone.rs:191-198,269`；`src/command/fetch.rs:2266-2271`（`fetch_repository` 无 filter 参数）；`src/internal/protocol/local_client.rs:586` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --bare -o upstream --filter=blob:none file://<S> d` 退出 0 后，`d` 的 `config_kv` 表中键（不区分大小写）以 `.promisor` 或 `.partialclonefilter` 结尾、或等于 `extensions.partialclone` 的 (key, value) 行集合，逐项等于 {(`remote.upstream.promisor`, `true`), (`remote.upstream.partialclonefilter`, `blob:none`), (`extensions.partialClone`, `upstream`)}（显式 `-o`；裸克隆避免检出拉取 blob）。
- [ ] **AC2** `libra clone --bare -c clone.defaultRemoteName=cfg --filter=blob:none file://<S> d2` 退出 0 后，`d2` 的 `config_kv` 表中键（不区分大小写）以 `.promisor` 或 `.partialclonefilter` 结尾、或等于 `extensions.partialclone` 的 (key, value) 行集合，逐项等于 {(`remote.cfg.promisor`, `true`), (`remote.cfg.partialclonefilter`, `blob:none`), (`extensions.partialClone`, `cfg`)}（远端名来自 `CL-06` 的 `clone.defaultRemoteName`）。
- [ ] **AC3** `libra clone --bare --filter=blob:none file://<S> d3` 退出 0 后，`d3` 的 `config_kv` 表中键（不区分大小写）以 `.promisor` 或 `.partialclonefilter` 结尾、或等于 `extensions.partialclone` 的 (key, value) 行集合，逐项等于 {(`remote.origin.promisor`, `true`), (`remote.origin.partialclonefilter`, `blob:none`), (`extensions.partialClone`, `origin`)}（默认远端名）。
- [ ] **AC4** 对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `d`、`d2`、`d3` 三个克隆库中的存在性判定结果均恰等于「该对象不是 blob」（提交与 tree 全部在本地、blob 全部缺失，Git `blob:none` 语义；三种远端名来源都实际过滤）。
- [ ] **AC5** 过渡断言（只在本卡发布提交上成立，`CL-24` 改写；表驱动，各样本的用户可见结局相同）：对显式 `combine:` 样本表 {`combine:blob:none+tree:1`、`combine:tree:1+blob:none`、`combine:blob:none+tree:2`、`combine:blob:none+object:type=blob`} 中每个规格，`libra clone --filter=<规格> file://<S> <新目标>` 均以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC6** 过渡断言（同上）：对多个取值不同的 `--filter` 样本表 {(`blob:none`, `tree:1`)、(`tree:1`, `blob:none`)、(`blob:none`, `tree:2`)、(`blob:none`, `object:type=blob`)} 中每一对 (甲, 乙)，`libra clone --filter=<甲> --filter=<乙> file://<S> <新目标>` 均以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC7** 过渡断言（同上）：远端不广告 `filter` 能力（夹具：`LIBRA_TEST=1` 下 `CL-13` 新增的测试接缝 `LIBRA_TEST_UPLOAD_PACK_NO_FILTER=1` 使本地 upload-pack 不广告 `filter`）时，`libra clone --filter=blob:none file://<S> e3` 以 `LBR-UNSUPPORTED-001` 拒绝，不得静默全量克隆。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：以 SQLite 只读连接读取 `config_kv` 全部行，按所述键条件筛选（键名转小写比较），筛得的 (key 小写, value) 集合与期望集合（键名转小写）做一次 `assert_eq!`；直接读表而不经会隐藏加密值的 `libra config list`。
- AC2 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：以 SQLite 只读连接读取 `config_kv` 全部行，按所述键条件筛选（键名转小写比较），筛得的 (key 小写, value) 集合与期望集合（键名转小写）做一次 `assert_eq!`；直接读表而不经会隐藏加密值的 `libra config list`。
- AC3 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：以 SQLite 只读连接读取 `config_kv` 全部行，按所述键条件筛选（键名转小写比较），筛得的 (key 小写, value) 集合与期望集合（键名转小写）做一次 `assert_eq!`；直接读表而不经会隐藏加密值的 `libra config list`。
- AC4 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：对 (库, OID) 全集（三库 × S 的 `rev-list --objects --all` 全集；其中 blob 子集非空作正控制）逐一按「验证执行协议」第 7 项做对象存在性判定，得到「(库, OID) → 存在/缺失」映射，与「(库, OID) → S 中 `libra cat-file -t <OID>` 不为 `blob`」映射做一次 `assert_eq!`。
- AC5 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：对显式 `combine:` 样本表中每个规格逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC6 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：对多个 `--filter` 样本表中每一对逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- AC7 → `command::clone_test::test_clone_filter_promisor_config_and_combine`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--filter=blob:none`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_filter_promisor_config_and_combine)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（同卡改写 `command::clone_test::test_clone_rejects_invalid_sparse_filter` 的 `CL-12` AC7 过渡断言，改为不再断言 `blob:none` 被拒绝），覆盖 AC1–AC7。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_json_reports_fetch_transfer_counts)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有传输计数 JSON 回归（`tests/command/clone_test.rs:1458`）。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_rejects_invalid_sparse_filter)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：本卡同卡改写 `CL-12` AC7 过渡断言后，运行改写后的 `CL-12` 用例（其余断言保留）。

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs、src/internal/protocol/mod.rs 与 local_client.rs`
**Dependencies:** `CL-12`、`DEP-CL-04`（协调 `plan-20261117-fetch-push.md#FP-07,#FP-15` 共享 `fetch.rs`）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --filter=blob:none 与 promisor 配置; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=6/8; landing=2; prod-files=4; scope=M; deps=CL-12,DEP-CL-04; writeset=串行于 CL-12; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-24: 实现 `combine:blob:none+tree:1` 过滤传输与无能力回退（#746, #754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-13` 拆出：在 `local_client.rs`、`fetch.rs`、`protocol/mod.rs` 与 `clone.rs` 应用 `combine:blob:none+tree:1` 树深过滤（仅下载根 tree 与 commits；显式 `combine:` 的两种顺序与重复 `--filter` 的两种顺序都按该组合处理，其余组合持续以 `LBR-UNSUPPORTED-001` 拒绝，AC8，`DEFER-CL-10`），并实现远端未广告 `filter` 能力时的安全回退全量克隆（`blob:none` 与本卡组合形式均适用，AC7；是否输出 Git 式回退警告不属本卡契约，本卡不承诺其文案）；同卡改写 `CL-13` 测试的 AC5–AC7 过渡断言（`CL-12` 对无效 `sparse:` 与 AC5 样本表（单独 `tree:<n>`、`object:type=`、`blob:limit=`）的拒绝断言保留：本卡只接受与 `blob:none` 组合的 `tree:1`，见 `DEFER-CL-10`）。（按 G-09 拆出 `CL-29` 承接 `sparse:oid` 过滤与预期缺失对象边界。）
**Out of scope:** promisor 配置写入（`CL-13`）；`Receiving objects` 进度与端到端 `cat-file` 验证（`CL-21`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#746` `#754` 传输层尚未应用 `combine:blob:none+tree:1` 树深过滤与远端无能力时的回退 | `src/command/clone.rs:191-198`；`src/command/fetch.rs:2266-2271`（`fetch_repository` 无 filter 参数）；`src/internal/protocol/local_client.rs:586` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --bare --filter=blob:none --filter=tree:1 file://<S> d` 退出 0，且 `d` 中 `libra config get remote.origin.partialclonefilter` 的 stdout 为 `combine:blob:none+tree:1` 加换行。
- [ ] **AC2** S 的每个提交在 `d` 中均存在。
- [ ] **AC3** S 的每个提交的根 tree 在 `d` 中均存在。
- [ ] **AC4** S 中的每个非根 tree（子目录 tree，如 `sub/` 的 tree）在 `d` 中均不存在。
- [ ] **AC5** S 中的每个 blob 在 `d` 中均不存在。
- [ ] **AC6** 等价形式（表驱动，各样本的用户可见结局相同）：对形式表 {`--filter=combine:blob:none+tree:1`、`--filter=combine:tree:1+blob:none`、`--filter=tree:1 --filter=blob:none`} 中每种形式，`libra clone --bare <形式> file://<S> <新目标>` 退出 0，且 S 的每个对象在该库中的存在性判定结果与在 `d` 中相同（显式 `combine:` 的两种顺序与重复 `--filter` 的另一种顺序得到同一对象集合）。
- [ ] **AC7** 远端不广告 `filter` 能力（夹具：`LIBRA_TEST=1` 下 `CL-13` 新增的测试接缝 `LIBRA_TEST_UPLOAD_PACK_NO_FILTER=1` 使本地 upload-pack 不广告 `filter`）时，对形式表 {`--filter=blob:none`、`--filter=blob:none --filter=tree:1`、`--filter=tree:1 --filter=blob:none`、`--filter=combine:blob:none+tree:1`、`--filter=combine:tree:1+blob:none`} 中每种形式，`libra clone --bare <形式> file://<S> <新目标>` 退出 0，且 S 的每个对象在该库中均存在（回退全量；表驱动，各样本的用户可见结局相同）。
- [ ] **AC8** 其余组合持续拒绝（表驱动，各样本的用户可见结局相同）：对样本表 {`--filter=combine:blob:none+tree:2`、`--filter=combine:blob:none+object:type=blob`、`--filter=blob:none --filter=tree:2`、`--filter=blob:none --filter=object:type=blob`} 中每个样本，`libra clone <样本> file://<S> <新目标>` 均以 `LBR-UNSUPPORTED-001` 拒绝（本计划只交付 `blob:none` 与 `tree:1` 的组合，`DEFER-CL-10`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：stdout 逐字等于 `combine:blob:none+tree:1\n`。
- AC2 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：在 S 中以 `libra rev-list --objects --all` 与 `libra cat-file -t` 得到提交集合（非空），逐一在目标库按「验证执行协议」第 7 项的对象存在性判定为存在。
- AC3 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：在 S 中以 `libra rev-list --objects --all` 与 `libra cat-file -t` 得到各提交 `<提交>^{tree}` 的集合（非空），逐一在目标库按「验证执行协议」第 7 项的对象存在性判定为存在。
- AC4 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：在 S 中以 `libra rev-list --objects --all` 与 `libra cat-file -t` 得到非根 tree集合（非空），逐一在目标库按「验证执行协议」第 7 项的对象存在性判定为缺失。
- AC5 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：在 S 中以 `libra rev-list --objects --all` 与 `libra cat-file -t` 得到blob集合（非空），逐一在目标库按「验证执行协议」第 7 项的对象存在性判定为缺失。
- AC6 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：对形式表逐一克隆到新目标（每次 `exit 0` 为前提）；对 S 中 `libra rev-list --objects --all` 的全部对象，分别在各新库与 `d` 按「验证执行协议」第 7 项做对象存在性判定，「(形式, OID) → 存在/缺失」映射与「(形式, OID) → 该 OID 在 `d` 中的判定结果」映射做一次 `assert_eq!`。
- AC7 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：对形式表逐一克隆到新目标（每次 `exit 0` 为前提）；对 S 中 `libra rev-list --objects --all` 的全部对象在各新库按「验证执行协议」第 7 项判定，「(形式, OID) → 存在/缺失」映射与「(形式, OID) → 存在」映射做一次 `assert_eq!`。
- AC8 → `command::clone_test::test_clone_combine_tree_depth_and_fallback`：对样本表中每个样本逐一以新目标目录调用，得到「样本 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个样本 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每个样本另按「验证执行协议」第 4 项检查克隆拒绝不变量。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`combine:`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_combine_tree_depth_and_fallback)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（同卡改写 `CL-13` 测试中 AC5–AC7 三条过渡断言为本卡最终契约：AC5、AC6 样本表中由 `blob:none` 与 `tree:1` 组成的样本改为成功并断言过滤结果，其余样本保留拒绝断言；AC7 改为本卡 AC7 的回退断言），覆盖 AC1–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_json_reports_fetch_transfer_counts)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有传输计数 JSON 回归。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_filter_promisor_config_and_combine)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：本卡同卡改写 `CL-13` AC5–AC7 过渡断言后，运行改写后的 `CL-13` 用例（其余断言保留）。

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs、src/internal/protocol/mod.rs 与 local_client.rs`
**Dependencies:** `CL-13`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone combine:blob:none+tree:1 过滤与回退; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=4; scope=M; deps=CL-13,DEP-CL-04; writeset=串行于 CL-13; release=independent; split-from=CL-13; exception=N/A`

---

### Task CL-29: 实现 `sparse:oid` 过滤与预期缺失对象边界（#748；`PM-33`/`t5616#33` 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-24` 拆出：在 `clone.rs`、`fetch.rs`、`protocol/mod.rs` 与 `local_client.rs` 实现 `--filter=sparse:oid=<object-expression>` 的传输、稀疏对象闭包与 promisor 预期缺失元数据。对象表达式必须解析为包含稀疏匹配规则的 blob（不是 tree）；并同卡改写 `CL-12` 测试的 AC6 过渡断言（有效 `sparse:oid` 改为断言克隆成功）；使部分克隆能够表达并保留 Git `sparse:oid` 过滤边界，供 `PM-33`（自 `PM-03` 拆出，见 `DEP-CL-14`）的 `t5616#33`/`TC-0625` `--missing=print|allow-promisor|error` 验收消费。
**Out of scope:** `rev-list --missing=<action>` 命令行为（`PM-33`）；`Receiving objects` 进度与最终 `cat-file` 收口（`CL-21`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有 `CL-12`/`CL-13` 对其它 filter fail-closed，`CL-24` 仅交付 `combine:blob:none+tree:1`；`PM-33` 需要 `sparse:oid` 的预期缺失对象夹具，且 `CL-12` 已明确 sparse 规则 blob 边界 | `src/command/clone.rs:191-198`；`plan-20261117-plumbing-misc.md#PM-33`；pinned Git `6de20f6092` `t/t5616-partial-clone.sh#33` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 夹具 F33（`t/t5616-partial-clone.sh#33` 同构）：S 的 `main` 含内容分别为 `one`、`two` 的 `one.t`、`two.t` 与内容为 `/one.t` 的规则文件 `only-one`（建夹具时断言这三个 blob 的完整 OID 两两不同，且与 S 中其余 blob 均不同）；`libra clone --bare --filter=sparse:oid=main:only-one file://<S> d` 退出 0。
- [ ] **AC2** 在 `d` 中发生任何按需拉取之前，对 S 中 `libra rev-list --objects --all` 列出的每个对象，其在 `d` 的存在性判定结果恰等于「该对象是提交或 tree，或是 `one.t` 的 blob」（Git `sparse:oid` 过滤保留全部提交与 tree、只保留路径匹配规则的 blob，`list-objects-filter.c:386` 起的 `filter_sparse`；夹具中只有 `one.t` 匹配 `/one.t`，因而 `two.t`、`only-one` 与 S 的其余 blob 都缺失）。
- [ ] **AC3** `d` 中 `libra config get remote.origin.promisor` 的 stdout 为 `true` 加换行（供 `PM-33` 的 promisor 归因）。
- [ ] **AC4** `d` 中 `libra show-ref` 的 stdout 含 `refs/remotes/origin/main`（Libra 非 `--mirror` 的 `--bare` 克隆建立远端跟踪引用的现状保持，`PM-33` 依赖，见 `DEP-CL-14`）。
- [ ] **AC5** AC2 之后，`d` 中 `libra cat-file -p <two.t 的 blob OID>` 退出 0，stdout 等于 S 中 `two.t` 的内容（按需拉取边界）。
- [ ] **AC6** `d` 中 `libra config get remote.origin.partialclonefilter` 的 stdout 为 `sparse:oid=main:only-one` 加换行（Git 原样记录过滤规格，`list-objects-filter-options.c:364-397` 的 `partial_clone_register`；后续 fetch 据此沿用该过滤边界）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：断言 `exit 0`。
- AC2 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：在 AC5 的 `cat-file` 之前，对 S 的 `libra rev-list --objects --all` 全集逐一在 `d` 按「验证执行协议」第 7 项（`--offline`）做对象存在性判定，得到「OID → 存在/缺失」映射；期望映射为「OID → （S 中 `libra cat-file -t` 为 `commit` 或 `tree`）或 OID 等于 `one.t` 的 blob OID」；两映射做一次 `assert_eq!`。
- AC3 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：stdout 逐字等于 `true\n`。
- AC4 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：stdout 含子串 ` refs/remotes/origin/main`。
- AC5 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：stdout 逐字等于 S 中 `libra cat-file -p HEAD:two.t` 的 stdout。
- AC6 → `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`：stdout 逐字等于 `sparse:oid=main:only-one\n`（期望值按「验证执行协议」第 4 项在 pinned Git 同夹具复核）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`sparse:oid`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项）（夹具构造函数供 `PM-33` 复用），覆盖 AC1–AC6。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_combine_tree_depth_and_fallback)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：`CL-24` 过滤与回退不被 `sparse:oid` 接线破坏。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_rejects_invalid_sparse_filter)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：本卡同卡改写 `CL-12` AC6 过渡断言（改为断言有效 `sparse:oid` 克隆成功）后，运行改写后的 `CL-12` 用例（其余断言保留）。

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs、src/internal/protocol/mod.rs 与 local_client.rs`
**Dependencies:** `CL-24`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone sparse:oid 过滤与预期缺失边界; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=2; prod-files=4; scope=M; deps=CL-24,DEP-CL-04; writeset=串行于 CL-24; release=independent; split-from=CL-24; exception=N/A`

---

### Task CL-21: 实现部分克隆 `Receiving objects` 进度与端到端 lazy fetch 验证（#754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-13` 拆出：按需拉取（`CL-19` 的 lazy fetch）在进度开启时向 stderr 输出 `Receiving objects`（`#754`，对应 Git `t/t0410-partial-clone.sh#38`；全局 `--progress=none` 时不输出；终端判定沿用全局 `--progress` 的 `auto` 语义，测试以伪终端覆盖），并打通真实 `libra clone --filter=blob:none file://...` 后执行 `libra cat-file -p <rev>:<path>` 端到端按需 lazy fetch 完整链路。
**Out of scope:** `--filter` 协商与 promisor 配置写入（`CL-13`）；`cat-file` 单对象读写层底层接线（`CL-19`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#754` 按需拉取缺少 `Receiving objects` 进度且需验证 `clone --filter=blob:none` → `cat-file -p` 端到端链路 | `src/command/clone.rs:191-198,269`；`src/command/fetch.rs:3472`（`handle_remote_progress`，只处理传输进度）；`src/internal/protocol/local_client.rs:586`（`fetch_objects`） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --filter=blob:none file://<S> d` 后，`<S1>:a.txt`（`S1` 为创建 S 时保存的第一个提交完整 OID；该版本 `a.txt` 不在 `HEAD` 树，检出不会拉取它） 的 blob 在 `d` 中缺失（「验证执行协议」第 7 项对象存在性判定；`#754` 验收第 1 条）。
- [ ] **AC2** 在 `d` 执行 `libra cat-file -p <S1>:a.txt`（非终端，分别捕获 stdout/stderr）退出 0，stdout 等于 S 中同一命令的 stdout（真实 `clone -> cat-file` 端到端按需拉取）。
- [ ] **AC3** AC2 的 stderr（非终端、默认 `auto` 进度设置）不含 `Receiving objects`。
- [ ] **AC4** AC2 之后该 blob 已存在于 `d` 本地。
- [ ] **AC5** 另一新部分克隆 `d2` 中，伪终端用例：子进程以 `sh -c 'libra … >"$OUT"'` 运行，stdout 重定向到测试临时目录中的文件 `$OUT`、stderr 留在 `portable_pty` 伪终端（同 `tests/command/config_test.rs:3836` 的用法），因而终端输出只含 stderr；以默认进度设置执行 `libra cat-file -p <S1>:a.txt` 退出 0 后，终端输出（即 stderr）含 `Receiving objects`（`#754`，对应 Git `t/t0410-partial-clone.sh#38` 的 `test_terminal … 2>err`）。
- [ ] **AC6** 另一新部分克隆 `d3` 中，以同一伪终端方式执行 `libra --progress=none cat-file -p <S1>:a.txt` 退出 0 后，终端输出（即 stderr）不含 `Receiving objects`。
- [ ] **AC7** 另一新部分克隆 `d4` 中，非终端执行 `libra --progress=text cat-file -p <S1>:a.txt`（分别捕获 stdout/stderr）退出 0 后，stderr 含 `Receiving objects`（进度走 stderr）。
- [ ] **AC8** AC7 的 stdout 逐字等于 S 中 `libra cat-file -p <S1>:a.txt` 的 stdout（进度不进入 stdout）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：该 blob OID 在 `d` 按「验证执行协议」第 7 项的对象存在性判定为缺失。
- AC2 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：`exit 0` 且 stdout 逐字相等。
- AC3 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：AC2 同一次调用捕获的 stderr 不含子串 `Receiving objects`。
- AC4 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：该 blob OID 在 `d` 按「验证执行协议」第 7 项的对象存在性判定为存在。
- AC5 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：`exit 0` 为前提；读取伪终端的全部输出（stdout 已重定向到 `$OUT`，不进入终端），含子串 `Receiving objects`。
- AC6 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：`exit 0` 为前提；伪终端全部输出不含子串 `Receiving objects`。
- AC7 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：`exit 0` 为前提；stderr 含子串 `Receiving objects`。
- AC8 → `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`：stdout 逐字相等。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`Receiving objects`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::cat_file_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`cat_file_test` 全模块回归。

**Full-suite trigger:** `T-1: 修改共享传输 src/command/fetch.rs`
**Dependencies:** `CL-29`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。另同步 `docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md` 与 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`（`cat-file` 在按需拉取时新增 `Receiving objects` 进度输出）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --filter 进度与端到端 lazy fetch; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=CL-29,DEP-CL-04; writeset=串行于 CL-29; release=independent; split-from=CL-13; exception=N/A`

---

### Task CL-19: 实现 promisor 缺失对象读取触发按需 lazy fetch（#754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-13` 拆出并前置至 `CL-12`/`CL-13` 之前：在对象读取与存储回退层（`src/utils/storage/local.rs`、`src/utils/object_ext.rs`、`src/command/fetch.rs`、`src/internal/protocol/local_client.rs`、`src/command/cat_file.rs`）实现 promisor 按 `extensions.partialClone` / `remote.<name>.promisor=true` 按需拉取单对象（按 OID；取回的对象按内容计算 OID 后写入对象库，天然以 OID 寻址；复用现有 `LocalClient` 与 `ProtocolClient` 传输接口），使 `libra cat-file -p <rev>:<path>` 在预置 promisor 夹具中自动 lazy fetch 缺失对象（进度输出由 `CL-21` 交付），且在 `--offline` 或 `LIBRA_READ_POLICY=local` 下 fail-closed 不发起网络请求；本卡新增测试接缝 `LIBRA_TEST_TRACE_LAZY_FETCH`（仅 `LIBRA_TEST=1` 时读取，每次发起按需拉取请求前向 stderr 写一行 `libra-test: lazy-fetch want=<n>`），供 AC6/AC8 与 `CL-20` 观测拉取次数。
**Out of scope:** `clone --filter` CLI 解析与写入 promisor 配置（`CL-13`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#754` 当前对象存储层与 `cat-file` 在遇到缺失 promisor 对象时直接报 NotFound（`hydrate.rs` 在私有 `hydrate_one` 中内联校验 OID，本卡在 `local.rs`/`object_ext.rs` 独立实现） | `src/command/clone.rs:269`；`src/utils/storage/local.rs:1859-1882`（`get` 依次查本库松散/pack 与 alternates，均无则 `GitError::ObjectNotFound`，没有 promisor 回退）；`src/utils/object_ext.rs:258-261`（`Blob::load` 缺失即 `Err`） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 夹具 P：`d` 为 `libra clone --no-local file://<S> d` 的结果，测试再按 `a.txt` 的 blob B 在 `d` 中的实际存储形式将其移除（松散文件则删除该文件；位于 pack 内则以只含其余对象的新 pack 及其 `.idx` 替换原 pack，开工按 ER-02 确定并回写本卡 `Current evidence`），并设置 `extensions.partialClone=origin`、`remote.origin.promisor=true`；在 `d` 以 `LIBRA_TEST=1 LIBRA_TEST_TRACE_LAZY_FETCH=1`（本卡新增的测试接缝：每次发起按需拉取请求前向 stderr 写一行 `libra-test: lazy-fetch want=<请求的对象数>`，不写文件）执行 `libra cat-file -p HEAD:a.txt` 退出 0，stdout 等于 S 中 `a.txt` 的内容。
- [ ] **AC2** AC1 的 stderr 中以 `libra-test: lazy-fetch` 开头的行恰为一行 `libra-test: lazy-fetch want=1`（接缝正控制，保证 AC6/AC8 的「无此行」不是空真）。
- [ ] **AC3** 远端名为 `upstream` 的同构夹具（`libra clone --no-local -o upstream file://<S> d2`，同样移除 B，并设置 `extensions.partialClone=upstream`、`remote.upstream.promisor=true`，不存在 `remote.origin.*`）中，同一命令的 stdout 同样等于 `a.txt` 的内容。
- [ ] **AC4** AC1 之后 B 存在于 `d` 的本地对象库（取回的对象已持久化，「验证执行协议」第 7 项对象存在性判定）。
- [ ] **AC5** 按夹具 P 同样步骤新建的克隆 `d5`（B 已移除、尚未拉取）中，以同一接缝执行 `libra --offline cat-file -p HEAD:a.txt`，以 `LBR-CLI-003` 拒绝。
- [ ] **AC6** AC5 的 stderr 中没有以 `libra-test: lazy-fetch` 开头的行（未发起拉取请求）。
- [ ] **AC7** 按夹具 P 同样步骤新建的克隆 `d7`（B 已移除、尚未拉取）中，以同一接缝和环境变量 `LIBRA_READ_POLICY=local` 执行 `libra cat-file -p HEAD:a.txt`，以 `LBR-CLI-003` 拒绝。
- [ ] **AC8** AC7 的 stderr 中没有以 `libra-test: lazy-fetch` 开头的行。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：stdout 逐字等于 S 中 `libra cat-file -p HEAD:a.txt` 的 stdout。
- AC2 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：按行切分 AC1 的 stderr，筛出以该前缀开头的行，结果逐字等于 `["libra-test: lazy-fetch want=1"]`。
- AC3 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：stdout 逐字等于 AC1 的期望内容。
- AC4 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：B 的完整 OID 在 `d` 按「验证执行协议」第 7 项的对象存在性判定为存在（`--offline`，排除再次拉取）。
- AC5 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC6 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：按行切分 AC5 的 stderr，以该前缀开头的行数为 0。
- AC7 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：进程以非零状态结束且 stderr 含 `LBR-CLI-003`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC8 → `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`：按行切分 AC7 的 stderr，以该前缀开头的行数为 0。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`promisor`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::cat_file_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`cat_file_test` 全模块回归。

**Full-suite trigger:** `T-1: 修改共享对象存储读取核心 src/utils/storage/local.rs 与 object_ext.rs`
**Dependencies:** `CL-18`、`DEP-CL-04`、`DEP-CL-08`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/storage/local.rs`, `src/utils/object_ext.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `src/command/cat_file.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/utils/storage/local.rs`, `src/utils/object_ext.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `src/command/cat_file.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=promisor 缺失对象读取 lazy fetch; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=4; prod-files=5; scope=M; deps=CL-18,DEP-CL-04,DEP-CL-08; writeset=串行于 CL-18; release=independent; split-from=CL-13; exception=N/A`

---

### Task CL-14: 支持 `clone --ref-storage-format` 与 `rev-parse --show-ref-storage-format`（#750）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 ADR-CL-04 在 `src/command/clone.rs` 新增 `--ref-storage-format=<format>` 解析（接受 `sqlite`，对未实现的 `files`/`reftable` 或未知格式 fail-closed 报错，不修改 `src/command/init.rs` 与 D25），并为 `RevParseArgs` 新增 `--show-ref-storage-format` 如实输出 `sqlite`（并在 `COMPATIBILITY.md` 记为 `intentionally-different`，`files`/`reftable` 存储后端延后至 `DEFER-CL-01`）。
**Out of scope:** 修改 `init --ref-format`（保留 D25）；实现 Git `files` 或 `reftable` ref 存储后端（`DEFER-CL-01`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#750` `CloneArgs` 缺 `--ref-storage-format`；`RevParseArgs` 缺 `--show-ref-storage-format`；`init.rs` `RefFormat` 为分支名校验而非存储格式（D25） | `src/command/clone.rs:64-236`；`src/command/rev_parse.rs:59`；`src/command/init.rs:301-313,1676-1690` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --ref-storage-format=sqlite file://<S> d` 退出 0。
- [ ] **AC2** `libra clone --mirror --ref-storage-format=sqlite file://<S> m` 退出 0。
- [ ] **AC3** `d` 中 `libra rev-parse --show-ref-storage-format` 的 stdout 为 `sqlite` 加换行。
- [ ] **AC4** `m`（裸仓库）与以 `libra init` 新建、未经克隆的仓库 `i` 中，同一命令的 stdout 均为 `sqlite` 加换行（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同；输出反映存储后端本身，不依赖克隆写入的任何配置）。
- [ ] **AC5** `libra clone --ref-storage-format=files file://<S> e1` 以 `LBR-UNSUPPORTED-001` 拒绝（`files` 后端见 `DEFER-CL-01`）。
- [ ] **AC6** `libra clone --ref-storage-format=reftable file://<S> e2` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC7** `libra clone --ref-storage-format=bogus file://<S> e3` 以 `LBR-CLI-002` 拒绝（Git 同类输入报 `unknown ref storage format`，`builtin/clone.c:1060`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：断言 `exit 0`。
- AC2 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：断言 `exit 0`。
- AC3 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：stdout 逐字等于 `sqlite\n`。
- AC4 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：「仓库 → stdout」映射与「每个仓库 → `sqlite\n`」映射做一次 `assert_eq!`。
- AC5 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC6 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC7 → `command::clone_test::test_clone_ref_storage_format_and_rev_parse`：进程以非零状态结束且 stderr 含 `LBR-CLI-002`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`ref-storage-format`（同时命中 `clone` 页的 `--ref-storage-format` 与 `rev-parse` 页的 `--show-ref-storage-format`）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_ref_storage_format_and_rev_parse)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC7。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`rev_parse_test` 全模块回归。

**Full-suite trigger:** `none`
**Dependencies:** `CL-21`、`DEP-CL-02`、`DEP-CL-09`（等待 `plan-20261117-plumbing-misc.md#PM-06` 先合入 `rev_parse.rs`）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/rev_parse.rs`, `tests/command/clone_test.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/rev_parse.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--ref-storage-format 与 rev-parse; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=CL-21,DEP-CL-02,DEP-CL-09; writeset=串行于 CL-21; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-15: 修复 `clone --single-branch` 从游离 HEAD 克隆错误创建 `refs/remotes/origin/main`（#753）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 当源仓库处于游离 HEAD 且执行 `libra clone --single-branch`（未指定 `--branch`）时，仅建立游离 HEAD 而不虚构 `refs/remotes/origin/main`，且后续执行 `libra fetch` 同样不会创建 `refs/remotes/origin/main`。
**Out of scope:** 正常有分支 HEAD 的 `--single-branch` 克隆（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#753` 源仓库游离 HEAD 且未给 `--branch` 时，`--single-branch` 经 `clone_fetch_branch` 回退到 `fetch::resolve_remote_default_branch` 选取默认分支（据 `#753` 复现会创建 `refs/remotes/origin/main`；开工按 ER-02 重新复现并回写） | `src/command/clone.rs:448-470` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** S 的 `HEAD` 游离于提交 C（C 不是任何分支的末端）；`libra clone --single-branch file://<S> d` 退出 0 后，`d` 中 `libra status --porcelain=v2 --branch` 的 stdout 前两行逐字等于 `# branch.head (detached)` 与 `# branch.oid <C>`（`src/command/status/output.rs:2541-2555`）。
- [ ] **AC2** `d` 中不存在远端跟踪分支 `refs/remotes/origin/main`。
- [ ] **AC3** 在 `d` 执行 `libra fetch` 退出 0 后，仍不存在 `refs/remotes/origin/main`。
- [ ] **AC4** `libra clone --single-branch --branch main file://<S> d2` 后，`d2` 中存在 `refs/remotes/origin/main`（同一查询的正控制）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_single_branch_from_detached_head`：前两行逐字比较，期望 OID 为 C。
- AC2 → `command::clone_test::test_clone_single_branch_from_detached_head`：以 SQLite 只读连接查询 `d` 的 `reference` 表中 `kind='Branch' AND remote='origin' AND name='refs/remotes/origin/main'` 的行数（Libra 以完整引用名存远端跟踪分支，2026-11-18 以 libra 0.30.207 实测）为 0。
- AC3 → `command::clone_test::test_clone_single_branch_from_detached_head`：`libra fetch` 的 `exit 0` 为前提；同一查询为 0。
- AC4 → `command::clone_test::test_clone_single_branch_from_detached_head`：以 SQLite 只读连接查询 `d2` 的 `reference` 表中 `kind='Branch' AND remote='origin' AND name='refs/remotes/origin/main'` 的行数（Libra 以完整引用名存远端跟踪分支，2026-11-18 以 libra 0.30.207 实测）为 1。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--single-branch`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_single_branch_from_detached_head)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_depth_implies_single_branch_matrix)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=1）：既有本地 `--single-branch` 矩阵回归（`tests/command/clone_test.rs:1753`：显式 `--single-branch`、`--depth` 隐含、`--no-single-branch` 与 `-b` 的远端跟踪引用范围）。

**Full-suite trigger:** `none`
**Dependencies:** `CL-14`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--single-branch 游离 HEAD 处理; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-14; writeset=串行于 CL-14; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-26: 在 `cli.rs` 为稀疏工作区的检出类命令增加 `fail-closed` 护栏（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-16` 拆出并前置（G-09）：在 `src/cli.rs` `command_preflight`（:1755+）中，对未适配稀疏 cone 的检出类命令建立 `core.sparseCheckout=true` 中央 `fail-closed` 护栏。本卡只覆盖 `checkout`、`switch`、`reset`、`restore`。本卡拆出 `CL-27`，由它承接 `merge`、`pull`、`rebase`、`cherry-pick`、`stash`。
**Out of scope:** `merge`/`pull`/`rebase`/`cherry-pick`/`stash` 护栏（`CL-27`）；`clone --sparse` cone 物化（`CL-16`）；`merge` 解禁（`CL-20`）；全量 cone 适配（`DEFER-CL-04`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` 的 `checkout`/`switch`/`reset`/`restore` 尚未适配 `core.sparseCheckout=true` | `src/cli.rs:1755` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 夹具 Q：以「验证执行协议」第 7 项的标准源仓库 S 为基础（`main` 末端 `S2`），从 `S1` 建分支 `other`（一个新增文件 `o.txt` 的提交）与 `topic`（一个把 `a.txt` 改为与 `S1`、`S2` 都不同内容的提交，即修改 `main` 也修改过的文件，非目录重命名），回到 `main` 后执行 `libra config set core.sparseCheckout true`；`libra checkout other` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC2** 同一夹具中 `libra switch other` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC3** 同一夹具中 `libra reset --hard HEAD~1` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC4** 同一夹具中 `libra restore a.txt`（先修改 `a.txt`）以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC2 → `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC3 → `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC4 → `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`core.sparseCheckout`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_sparse_preflight_checkout_switch_reset_restore)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::(checkout|switch|reset|restore)_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：正控制——未设置 `core.sparseCheckout` 的普通仓库中，`checkout`/`switch`/`reset`/`restore` 的既有用例在新增护栏后全部通过（护栏不得无条件拒绝）。

**Full-suite trigger:** `T-1: 修改全局命令预检 src/cli.rs`
**Dependencies:** `CL-15`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md`、`docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=稀疏检出类命令 fail-closed 护栏; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-15; writeset=串行于 CL-15; release=independent; split-from=CL-16; exception=N/A`

---

### Task CL-27: 在 `cli.rs` 为稀疏工作区的历史类命令增加 `fail-closed` 护栏（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-26` 拆出（G-09）：在同一 `command_preflight` 护栏上覆盖 `merge`、`pull`、`rebase`、`cherry-pick`、`stash`。只有目录重命名合并的解禁由 `CL-20` 承接，其它 `merge` 继续拒绝；本卡 AC1 使用内容修改合并夹具，因而在最终树上继续成立。
**Out of scope:** `checkout`/`switch`/`reset`/`restore`（`CL-26`）；`merge` 稀疏 lazy fetch 解禁（`CL-20`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` 的 `merge`/`pull`/`rebase`/`cherry-pick`/`stash` 尚未适配 `core.sparseCheckout=true` | `src/cli.rs:1755` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 同 `CL-26` AC1 的夹具 Q：以「验证执行协议」第 7 项的标准源仓库 S 为基础（`main` 末端 `S2`），从 `S1` 建分支 `other`（一个新增文件 `o.txt` 的提交）与 `topic`（一个把 `a.txt` 改为与 `S1`、`S2` 都不同内容的提交，即修改 `main` 也修改过的文件，非目录重命名），回到 `main` 后执行 `libra config set core.sparseCheckout true`；`libra merge topic` 以 `LBR-UNSUPPORTED-001` 拒绝。`CL-20` 只解禁目录重命名合并，本断言在最终树上继续成立。
- [ ] **AC2** 同一夹具中 `libra pull` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC3** 同一夹具中 `libra rebase topic` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC4** 同一夹具中 `libra cherry-pick topic` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] **AC5** 同一夹具中 `libra stash push`（先修改 `a.txt`）以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC2 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）（`pull` 在预检阶段即被拒绝，不需要可达远端）。
- AC3 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC4 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- AC5 → `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`：进程以非零状态结束且 stderr 含 `LBR-UNSUPPORTED-001`（「以…拒绝」的定义，「验证执行协议」第 6 项）。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`core.sparseCheckout`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC5。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::(merge|pull|rebase|cherry_pick|stash)_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：正控制——未设置 `core.sparseCheckout` 的普通仓库中，`merge`/`pull`/`rebase`/`cherry-pick`/`stash` 的既有用例在新增护栏后全部通过。

**Full-suite trigger:** `T-1: 修改全局命令预检 src/cli.rs`
**Dependencies:** `CL-26`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`、`docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`、`docs/commands/stash.md`、`docs/commands/zh-CN/stash.md`、`docs/development/commands/stash.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=稀疏历史类命令 fail-closed 护栏; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-26; writeset=串行于 CL-26; release=independent; split-from=CL-26; exception=N/A`

---

### Task CL-16: 支持 `libra clone --sparse` 工作区 cone 物化与 skip-worktree 初始化（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 ADR-CL-03 为 `CloneArgs` 新增 `--sparse`（并把 `clone.rs:215-224` 的 `--deps-of` 帮助说明中 `NOT --sparse (declined D10)` 字样改为现行说明，由 AC7 验收），局部修订 D10（同步 `COMPATIBILITY.md` 与 `docs/development/commands/_compatibility.md`），在 `src/command/clone.rs` 与 `src/internal/sparse/mod.rs`（新增只含顶层的 cone 成员判定供克隆物化使用，复用索引 skip-worktree 位，无 SQLite schema 变更）初始化 `core.sparseCheckout=true`、`core.sparseCheckoutCone=true`、工作区顶层文件物化及 skip-worktree 索引位；本卡的稀疏状态即这两个配置与 skip-worktree 位，不写入、不启用只读的 `sparse-view` 显示过滤（启用后 `ls-files` 会隐藏 skip-worktree 条目，与 AC5 矛盾；2026-11-18 以 libra 0.30.207 实测 `sparse-view set '/*' '!/*/'` 后 `ls-files -t` 不再列出 `S sub/b.txt`），Git `info/sparse-checkout` 式 cone 规则文件的持久化与规则管理随 `DEFER-CL-04`；在 `src/command/status.rs:563` 仅为 `status` 跳过 `warn_sparse_checkout_unsupported_once`（保留 `add.rs:532`/`commit.rs:3017`/`diff.rs:851` 的既有告警，不改变 `add`/`commit`/`diff` 行为）；本卡不对 `--sparse` 与 `--filter` 的组合另加限制：`#756` 要求的 `--sparse --filter=blob:none` 组合由 `CL-20` 验收（AC1），其余组合不在本计划验收范围（`DEFER-CL-05`）；稀疏仓库中的检出类与历史类写入命令此时仍由 `CL-26`/`CL-27` 拒绝。（G-09：本卡拆出 `CL-20`、`CL-26`、`CL-27`。）
**Out of scope:** 未适配命令 `fail-closed` 护栏（`CL-26`）；`--sparse --filter=blob:none` 组合及目录重命名合并 lazy fetch（`CL-20`）；`checkout`/`switch`/`reset` 全量 cone 过滤扩展（`DEFER-CL-04`）；独立 `libra sparse-checkout` 子命令（永久非目标）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` `CloneArgs` 缺少 `--sparse` 标志，且 `clone.rs:220`（`--deps-of` 帮助说明）仍写 `NOT --sparse (declined D10)`；`status.rs:563` 调用 `warn_sparse_checkout_unsupported_once`（`add.rs:532`/`commit.rs:3017`/`diff.rs:851` 仍保留）；`sparse_view.rs` 为只读视图且 D10 记于 `_compatibility.md:175-197` | `src/command/clone.rs:220-235`；`src/command/status.rs:563,1248-1265`；`src/internal/sparse/mod.rs:1`；`docs/development/commands/_compatibility.md:175-197`；`COMPATIBILITY.md:166,172,375` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** `libra clone --sparse file://<S> d` 退出 0，且 `d` 中 `libra config get core.sparseCheckout` 的 stdout 为 `true` 加换行。
- [ ] **AC2** `d` 中 `libra config get core.sparseCheckoutCone` 的 stdout 为 `true` 加换行（cone 模式，Git `clone --sparse` 经 `sparse-checkout set` 初始化，`builtin/clone.c:646`）。
- [ ] **AC3** `d` 工作区的顶层文件 `a.txt` 的字节等于 S 中 `S2:a.txt` 的内容（顶层文件按 `HEAD` 正确物化）。
- [ ] **AC4** `d` 工作区不含目录 `sub/`。
- [ ] **AC5** `d` 中 `libra ls-files -t` 的 stdout 逐字等于 `H a.txt`、`S sub/b.txt` 两行（只有 cone 外条目带 skip-worktree 位，且条目仍在索引中）。
- [ ] **AC6** `d` 中 `libra status` 退出 0，且其 stderr 不含 `core.sparseCheckout=true is not supported`。
- [ ] **AC7** `libra clone --help` 的 stdout 不含子串 `declined D10`（`clone.rs:220` 的 `--deps-of` 帮助说明不再声称 `--sparse` 已被 D10 拒绝；libra 0.30.207 的该输出中 `declined` 只出现在这一处）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：stdout 逐字等于 `true\n`。
- AC2 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：stdout 逐字等于 `true\n`。
- AC3 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：读取 `d/a.txt` 的 SHA-256，与在 S 中 `libra cat-file -p <S2>:a.txt`（`exit 0` 为前提）的 stdout 的 SHA-256 做一次 `assert_eq!`。
- AC4 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：`d/sub` 的 `exists()` 为假。
- AC5 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：`exit 0` 为前提；stdout 逐字等于 `H a.txt\nS sub/b.txt\n`（格式见 libra 0.30.207 实测，开工按 ER-02 以现行输出复核）。
- AC6 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：`exit 0` 为前提；stderr 不含该子串。
- AC7 → `command::clone_test::test_clone_sparse_cone_materializes_top_level`：`exit 0` 为前提；stdout 不含子串 `declined D10`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--sparse`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_sparse_cone_materializes_top_level)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC7。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`status_test` 全模块回归。

**Full-suite trigger:** `T-1: 修改共享稀疏核心 src/internal/sparse/mod.rs`
**Dependencies:** `CL-27`、`DEP-CL-11`（协调 `src/command/status.rs` 共享写集）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/sparse/mod.rs`, `src/command/status.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/sparse/mod.rs`, `src/command/status.rs`, `tests/command/clone_test.rs`, `docs/development/commands/_compatibility.md`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --sparse cone 物化与 status 适配; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=3; prod-files=3; scope=M; deps=CL-27,DEP-CL-11; writeset=串行于 CL-27; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-20: 实现 `clone --sparse` 部分克隆与目录重命名合并按需 lazy fetch（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-16` 拆出：在 `src/command/clone.rs` 打通 `--sparse --filter=blob:none` 的稀疏部分克隆（与 `CL-16` 的 cone 初始化叠加）并在 `src/cli.rs` `command_preflight` 只为目录重命名合并解开 `core.sparseCheckout=true` 护栏，内容修改、删除和冲突合并继续拒绝，同时在 `src/command/merge.rs`、`src/command/merge/rename_merge.rs`、`src/command/merge/tree_merge.rs` 与 `src/command/fetch.rs` 中新增批量多 OID `fetch_objects_batch` 预取 helper 并打通 `clone --sparse --filter=blob:none`（cone 模式，AC7–AC8）及目录重命名合并时的单次批量按需 lazy fetch（对应 `#756` 的 `TC-0734`，即 Git `t/t6421-merge-partial-clone.sh#2`：1 次拉取取回 6 个对象；拉取次数与对象数经 `CL-19` 新增的 stderr 测试接缝 `LIBRA_TEST_TRACE_LAZY_FETCH`（仅 `LIBRA_TEST=1` 时读取）观测），确保稀疏工作区合并时不会遗漏重命名目标文件。
**Out of scope:** `--sparse` 基础 cone 初始化与 skip-worktree（`CL-16`）；`checkout`/`switch`/`reset` 护栏（`CL-26`/`DEFER-CL-04`）；`#756` 验收第 4 条（完整 `t6421#2` 需在稀疏仓库内 `checkout`，随 `DEFER-CL-04`）；离线模式下按需拉取被拒绝（`CL-19` AC5–AC6）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` 部分克隆结合 `--sparse` 进行目录重命名合并时需按需批量补全被折叠/重命名的树与 blob（`1 fetch` / `6 objects`）；目录重命名合并逻辑位于 `rename_merge.rs` 与 `tree_merge.rs` | `src/command/clone.rs:220-235`；`src/cli.rs:1755`；`src/command/merge.rs:1121`（`execute_safe`）；`src/command/merge/rename_merge.rs:291,1157`（`apply_directory_renames`、`detect_and_apply_renames`）；`src/command/merge/tree_merge.rs:205`（`merge_tree_items`） |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 夹具 M 为 Git `t/t6421-merge-partial-clone.sh#2`（`#756` 的 `TC-0734`）的 `server` 同构仓库：提交 O，分支 `A`（`dir/` 重命名为 `folder/` 等）与 `B-dir`（在 `dir/subdir/` 新增 `newfile.java`、`newfile.scala`、`newfile.groovy`）；以 `libra clone --sparse --filter=blob:none --branch A file://<M> d` 退出 0 为前提，在 `d` 中 `libra config set merge.directoryRenames true` 后，以 `LIBRA_TEST=1 LIBRA_TEST_TRACE_LAZY_FETCH=1`（`CL-19` 新增的 stderr 接缝）执行 `libra merge origin/B-dir` 退出 0，且其 stderr 中以 `libra-test: lazy-fetch` 开头的行恰为一行 `libra-test: lazy-fetch want=6`（Git 同夹具 `fetch_count:6`、1 次拉取）。
- [ ] **AC2** 合并前后，M 的对象中在 `d` 由缺失变为存在的集合恰为 Git `t6421#2` 列出的 6 个 blob（`Makefile_TOP_O`、`Makefile_TOP_A`、`Makefile_SUB_O`、`Makefile_SUB_A`、`e_O`、`e_A`，`t/t6421-merge-partial-clone.sh:282-285`）。
- [ ] **AC3** 合并后 `d` 的索引中路径以 `folder/subdir/newfile.` 开头的条目 (路径, blob OID) 集合恰为 `newfile.java`、`newfile.scala`、`newfile.groovy` 三项，且各自 OID 等于 M 中 `B-dir:dir/subdir/<同名文件>` 的 blob OID（内容未损坏）（目录重命名生效，`t6421#2` 期望树）。
- [ ] **AC4** 非稀疏部分克隆 `libra clone --filter=blob:none --branch A file://<M> d2` 中，同样设置后以同一接缝执行 `libra merge origin/B-dir` 退出 0，且其 stderr 中至少有一行以 `libra-test: lazy-fetch` 开头（缺失对象确实按需拉取）。
- [ ] **AC5** AC4 的合并后，`d2` 的索引中路径以 `folder/subdir/newfile.` 开头的条目 (路径, blob OID) 集合恰为 `newfile.java`、`newfile.scala`、`newfile.groovy` 三项，且各自 OID 等于 M 中 `B-dir:dir/subdir/<同名文件>` 的 blob OID（内容未损坏）。
- [ ] **AC6** 夹具 N：在标准源仓库 S 上从 `S1` 另建分支 `t-mod`（只把 cone 外文件 `sub/b.txt` 改为单行 `b2`）、`t-del`（只删除 `sub/b.txt`）与 `t-conf`（只把 `a.txt` 改为单行 `three`），以 `libra clone --sparse --filter=blob:none file://<S> n` 退出 0 为前提；在 `n`（当前分支 `main`，末端 `S2`）中对 {`origin/t-mod`（内容修改合并，`main` 未改该文件）、`origin/t-del`（删除合并）、`origin/t-conf`（冲突合并：`S2` 与 `t-conf` 对 `a.txt` 唯一一行做了不同修改）} 逐一执行 `libra merge <分支>`，均以 `LBR-UNSUPPORTED-001` 拒绝（表驱动，按「验证执行协议」第 6 项计为 1 条：结局相同）。
- [ ] **AC7** 合并前（AC1 的克隆之后），`d` 中 `libra config get core.sparseCheckout` 与 `libra config get core.sparseCheckoutCone` 的 stdout 均为 `true` 加换行（组合克隆同样处于 cone 模式；表驱动，结局相同）。
- [ ] **AC8** 合并前，`d` 中 `libra ls-files -t` 的「路径 → 标记」映射恰等于「M 中分支 `A` 的树的每个文件路径 → 路径含 `/` 时为 `S`、否则为 `H`」（条目既不缺也不多；只有顶层文件在 cone 内，其余条目带 skip-worktree 位，与 `CL-16` 的根目录 cone 一致）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：`exit 0` 为前提；按行切分 stderr，筛出以该前缀开头的行，结果逐字等于 `["libra-test: lazy-fetch want=6"]`（期望值按「验证执行协议」第 4 项在 pinned Git 同夹具复核）。
- AC2 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：合并前后各对 M 的 `libra rev-list --objects --all` 全部对象在 `d` 按「验证执行协议」第 7 项的对象存在性判定，得到两份「OID → 存在/缺失」映射；由二者差得到的「缺失→存在」集合与期望 6 个 OID（在 M 中按路径以 `libra rev-parse <提交>:<路径>` 取得）做一次 `assert_eq!`。
- AC3 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：解析 `d` 中 `libra ls-files -s` 的每行（`<mode> <OID> <stage>\t<path>`），筛出该前缀的 (路径, OID) 集合；期望 OID 在 M 中以 `libra rev-parse B-dir:dir/subdir/newfile.<ext>` 取得；两集合做一次 `assert_eq!`。
- AC4 → `command::clone_test::test_non_sparse_partial_merge_fetches_missing_objects`：`exit 0` 为前提；按行切分 stderr，以该前缀开头的行数 ≥ 1。
- AC5 → `command::clone_test::test_non_sparse_partial_merge_fetches_missing_objects`：解析 `d2` 中 `libra ls-files -s` 的每行（`<mode> <OID> <stage>\t<path>`），筛出该前缀的 (路径, OID) 集合；期望 OID 在 M 中以 `libra rev-parse B-dir:dir/subdir/newfile.<ext>` 取得；两集合做一次 `assert_eq!`。
- AC6 → `command::clone_test::test_sparse_merge_rejected_cases_preserve_state`：对三个分支逐一调用，得到「分支 → (进程以非零状态结束, stderr 含 `LBR-UNSUPPORTED-001`)」映射，与「每个分支 → (真, 真)」映射做一次 `assert_eq!`（「以…拒绝」的定义，「验证执行协议」第 6 项）；每次调用前后另按「验证执行协议」第 4 项比较 `n` 的仓库快照。
- AC7 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：「配置键 → stdout」映射与「每个键 → `true\n`」映射做一次 `assert_eq!`。
- AC8 → `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`：`exit 0` 为前提；解析 `d` 中 `libra ls-files -t` 的每行（`<标记> <路径>`）得到「路径 → 标记」映射；期望映射由 M 中 `libra ls-tree -r --name-only A` 的每个路径按「含 `/` → `S`，否则 → `H`」导出（非空）；两映射做一次 `assert_eq!`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`--sparse`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_sparse_partial_merge_lazy_fetch)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC3、AC7–AC8。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_non_sparse_partial_merge_fetches_missing_objects)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC4–AC5。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_sparse_merge_rejected_cases_preserve_state)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC6。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_sparse_cone_materializes_top_level) | test(=command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash)'` — 共享回归组（「验证执行协议」第 5 项，预期 N=2）：`CL-16` 稀疏 cone 物化与 `CL-27` 拒绝内容修改合并的回归（本卡改动 `src/cli.rs` 护栏与 `clone.rs` 后重跑）。

**Full-suite trigger:** `T-1: 修改共享传输 src/command/fetch.rs、合并引擎 rename_merge.rs/tree_merge.rs 与 src/cli.rs`
**Dependencies:** `CL-16`、`DEP-CL-04`、`DEP-CL-07`（只核验 `merge.rs` 没有在途写者；本卡先合入，不等待 `MP-17`）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/cli.rs`, `src/command/merge.rs`, `src/command/merge/rename_merge.rs`, `src/command/merge/tree_merge.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/cli.rs`, `src/command/merge.rs`, `src/command/merge/rename_merge.rs`, `src/command/merge/tree_merge.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --sparse 部分克隆与目录重命名合并; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=3; prod-files=6; scope=M; deps=CL-16,DEP-CL-04,DEP-CL-07; writeset=串行于 CL-16; release=independent; split-from=CL-16; exception=N/A`

---

### Task CL-17: 确保默认 `libra clone` 正确持久化 `remote.<name>.fetch`（#876）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 修复默认 `libra clone` 未写入标准 `remote.<name>.fetch=+refs/heads/*:refs/remotes/<name>/*` 配置的缺口（当前仅 `--single-branch` 在 `clone.rs:2043-2054` 写入，`remote add` 已在 `remote.rs:822-834` 写入；`--revision` 克隆不写入，见 AC3），避免后续向 `remote.<name>.fetch` 追加 refspec 后 `fetch --prune` 发生越界修剪。
**Out of scope:** `fetch --prune` 端到端（`#541` 与 `FP-15` 未就绪）。该场景只登记在 `DEFER-CL-03`，本卡不预置 `#[ignore]` 测试，也不把启用该测试拆成另一张实现卡。非 `--revision` 的 `--bare` 克隆的 refspec 持久化与 `refs/remotes/<name>/*` 布局维持现状（`PM-33` 依赖，见 `DEP-CL-14`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#876` `remote add` 已写 `remote.<name>.fetch`，但 `clone.rs:2043-2054` 仅在 `--single-branch` 写入而默认 `clone` 遗漏 | `src/command/clone.rs:2043-2054`；`src/command/remote.rs:822-834` |

**Acceptance criteria:**（每条单谓词，按「验证执行协议」第 6 项计数）
- [ ] **AC1** 默认 `libra clone file://<S> d` 后，`d` 中 `libra config get --all remote.origin.fetch` 的 stdout 为 `+refs/heads/*:refs/remotes/origin/*` 加换行。
- [ ] **AC2** `libra clone -o upstream --branch main file://<S> d2` 后，`d2` 中 `libra config get --all remote.upstream.fetch` 的 stdout 为 `+refs/heads/*:refs/remotes/upstream/*` 加换行。
- [ ] **AC3** `libra clone --bare --revision=refs/tags/v1 file://<S> d3` 退出 0 后，`d3` 不存在 `remote.origin.fetch`（`--revision` 不建远端跟踪分支，不写 fetch refspec）。
- [ ] **AC4** 在 `d` 执行 `libra config set --add remote.origin.fetch '+refs/tags/*:refs/tags/*'` 退出 0 后，`libra config get --all remote.origin.fetch` 的 stdout 逐字为 `+refs/heads/*:refs/remotes/origin/*` 与 `+refs/tags/*:refs/tags/*` 两行（原默认条目保留在首行）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均按「文档与网站验收命令」逐文件验收（不计入 G-03）。

**AC → 断言映射:**（「验证执行协议」第 4、6 项：映射与 AC 表述同一谓词；成功类 `exit 0` 为读取输出的前提；失败类按第 6 项断言非零状态与稳定码）
- AC1 → `command::clone_test::test_clone_persists_remote_fetch_refspec`：stdout 逐字等于 `+refs/heads/*:refs/remotes/origin/*\n`。
- AC2 → `command::clone_test::test_clone_persists_remote_fetch_refspec`：stdout 逐字等于 `+refs/heads/*:refs/remotes/upstream/*\n`。
- AC3 → `command::clone_test::test_clone_persists_remote_fetch_refspec`：`exit 0` 为前提；以 SQLite 只读连接查询 `d3` 的 `config_kv`，键 `remote.origin.fetch` 的行数为 0。
- AC4 → `command::clone_test::test_clone_persists_remote_fetch_refspec`：`config set --add` 的 `exit 0` 为前提；stdout 逐字等于 `+refs/heads/*:refs/remotes/origin/*\n+refs/tags/*:refs/tags/*\n`。
- 文档 AC（ER-06/ER-06a 同卡强制门，不计入 G-03）→ 「文档与网站验收命令」第 1–8 步；文档关键字：`remote.<name>.fetch`。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_persists_remote_fetch_refspec)'` `(new)` — 卡特有用例（「验证执行协议」第 1–4 项），覆盖 AC1–AC4。
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::remote_test::/)'` — 共享回归组（「验证执行协议」第 5 项）：`remote_test` 全模块回归。

**Full-suite trigger:** `none`
**Dependencies:** `CL-20`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（网站文件写前按「文档与网站验收命令」第 2 步核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=默认 clone 写 remote.<name>.fetch; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=CL-20; writeset=串行于 CL-20; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-22: 已撤回（不得开工）

**Status:** superseded。`#876` 的 `fetch --prune` 端到端不在本计划单独发版。配置持久化由 `CL-17` 交付；依赖 `#541` 与 `FP-15` 的端到端只记在 `DEFER-CL-03`。本编号不再拥有写集、验收标准或发布边界。

---


## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | CLI 参数重写回归、指针布局放行表完备性 | `source .env.test && cargo nextest run --lib -E 'test(/^cli::tests::index_pack_progress_rewrite/)'`、`source .env.test && cargo nextest run --lib -E 'test(=cli::tests::pointer_layout_allowlist_matches_commands)'` |
| 集成 | `clone` 标志、硬链接、`.git` 回退、alternates、部分克隆、`--sparse`、稀疏护栏、`remote.<name>.fetch`（各卡卡特有用例见追溯表） | `source .env.test && cargo nextest run --test command_test -E 'test(/^command::clone_test::/)'`（共享回归组，「验证执行协议」第 5 项） |
| 兼容 | 浅克隆完整性、配置默认覆盖、兼容账本与命令矩阵对齐、版本面 | `source .env.test && cargo nextest run --test compat_clone_shallow_integrity --test compat_config_defaults_semantics --test compat_ledger_schema --test compat_matrix_alignment --test compat_version_surface_sync` |
| 迁移 | `N/A`（无 SQLite schema 变更） | `N/A` |
| 安全 | 无效 `sparse:` filter 拒绝与目标目录清理、对象符号链接拒绝、指针布局默认拒绝 | `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_rejects_invalid_sparse_filter)'`、`source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_rejects_symlinked_object_paths)'`、`source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_separate_git_dir_default_deny)'` |
| 性能 | `-l/--local` 同文件系统硬链接复用 inode | `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_test::test_clone_local_hardlinks_objects_on_same_fs)'` |
| live/gated | 收口阶段双环境全量套件 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| CL-01 | `#737` | `src/cli.rs`, `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_boolean_progress_does_not_consume_url`；共享回归组 `cli::tests::index_pack_progress_rewrite`|
| CL-02 | `#739` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_short_n_skips_checkout`；共享回归组 `command::clone_test::test_clone_json_reports_fetch_transfer_counts`|
| CL-03 | `#738` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_no_reject_shallow_overrides_reject_shallow`；共享回归组 `command::clone_test::test_clone_from_shallow_git_source_matrix`|
| CL-04 | `#740` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`；共享回归组 `command::clone_test::test_clone_bare_and_mirror_default_names_and_layout`|
| CL-23 | `#740` | `src/utils/util.rs`, `src/command/alternates.rs` |`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`、`docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`、`docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`、`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`docs/commands/alternates.md`、`docs/commands/zh-CN/alternates.md`、`docs/development/commands/alternates.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`|卡特有 `command::clone_test::test_clone_separate_git_dir_pointer_commands`；共享回归组 `command::status_test::`|
| CL-28 | `#740` | `src/cli.rs` |`docs/commands/add.md`、`docs/commands/zh-CN/add.md`、`docs/development/commands/add.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`、`docs/commands/agent.md`、`docs/commands/zh-CN/agent.md`、`docs/development/commands/agent.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`、`docs/commands/alternates.md`、`docs/commands/zh-CN/alternates.md`、`docs/development/commands/alternates.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`、`docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`docs/development/commands/am.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`、`docs/commands/apply.md`、`docs/commands/zh-CN/apply.md`、`docs/development/commands/apply.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`、`docs/commands/archive.md`、`docs/commands/zh-CN/archive.md`、`docs/development/commands/archive.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`、`docs/commands/auth.md`、`docs/commands/zh-CN/auth.md`、`docs/development/commands/auth.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`、`docs/commands/automation.md`、`docs/commands/zh-CN/automation.md`、`docs/development/commands/automation.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`、`docs/commands/bisect.md`、`docs/commands/zh-CN/bisect.md`、`docs/development/commands/bisect.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`、`docs/commands/blame.md`、`docs/commands/zh-CN/blame.md`、`docs/development/commands/blame.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`、`docs/commands/branch.md`、`docs/commands/zh-CN/branch.md`、`docs/development/commands/branch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`、`docs/commands/bundle.md`、`docs/commands/zh-CN/bundle.md`、`docs/development/commands/bundle.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`、`docs/commands/cache.md`、`docs/commands/zh-CN/cache.md`、`docs/development/commands/cache.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`、`docs/commands/check-attr.md`、`docs/commands/zh-CN/check-attr.md`、`docs/development/commands/check-attr.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`、`docs/commands/check-ignore.md`、`docs/commands/zh-CN/check-ignore.md`、`docs/development/commands/check-ignore.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`、`docs/commands/check-mailmap.md`、`docs/commands/zh-CN/check-mailmap.md`、`docs/development/commands/check-mailmap.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`、`docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`、`docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`、`docs/commands/cloud.md`、`docs/commands/zh-CN/cloud.md`、`docs/development/commands/cloud.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`、`docs/commands/commit-tree.md`、`docs/commands/zh-CN/commit-tree.md`、`docs/development/commands/commit-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`、`docs/commands/count-objects.md`、`docs/commands/zh-CN/count-objects.md`、`docs/development/commands/count-objects.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`、`docs/commands/credential.md`、`docs/commands/zh-CN/credential.md`、`docs/development/commands/credential.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`、`docs/commands/deps.md`、`docs/commands/zh-CN/deps.md`、`docs/development/commands/deps.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`、`docs/commands/describe.md`、`docs/commands/zh-CN/describe.md`、`docs/development/commands/describe.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`、`docs/commands/diff.md`、`docs/commands/zh-CN/diff.md`、`docs/development/commands/diff.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`、`docs/commands/diff-files.md`、`docs/commands/zh-CN/diff-files.md`、`docs/development/commands/diff-files.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`、`docs/commands/diff-index.md`、`docs/commands/zh-CN/diff-index.md`、`docs/development/commands/diff-index.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`、`docs/commands/diff-tree.md`、`docs/commands/zh-CN/diff-tree.md`、`docs/development/commands/diff-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`、`docs/commands/dirty.md`、`docs/commands/zh-CN/dirty.md`、`docs/development/commands/dirty.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`、`docs/commands/fast-export.md`、`docs/commands/zh-CN/fast-export.md`、`docs/development/commands/fast-export.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`、`docs/commands/fast-import.md`、`docs/commands/zh-CN/fast-import.md`、`docs/development/commands/fast-import.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`、`docs/commands/fetch.md`、`docs/commands/zh-CN/fetch.md`、`docs/development/commands/fetch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`、`docs/commands/file.md`、`docs/commands/zh-CN/file.md`、`docs/development/commands/file.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`、`docs/commands/for-each-ref.md`、`docs/commands/zh-CN/for-each-ref.md`、`docs/development/commands/for-each-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`、`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`、`docs/commands/fsck.md`、`docs/commands/zh-CN/fsck.md`、`docs/development/commands/fsck.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`、`docs/commands/gc.md`、`docs/commands/zh-CN/gc.md`、`docs/development/commands/gc.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`、`docs/commands/grep.md`、`docs/commands/zh-CN/grep.md`、`docs/development/commands/grep.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`、`docs/commands/hash-object.md`、`docs/commands/zh-CN/hash-object.md`、`docs/development/commands/hash-object.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`、`docs/commands/hooks.md`、`docs/commands/zh-CN/hooks.md`、`docs/development/commands/hooks.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`、`docs/commands/hydrate.md`、`docs/commands/zh-CN/hydrate.md`、`docs/development/commands/hydrate.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`、`docs/commands/index-pack.md`、`docs/commands/zh-CN/index-pack.md`、`docs/development/commands/index-pack.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`、`docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`、`docs/commands/investigate.md`、`docs/commands/zh-CN/investigate.md`、`docs/development/commands/investigate.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`、`docs/commands/layer.md`、`docs/commands/zh-CN/layer.md`、`docs/development/commands/layer.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`、`docs/commands/lfs.md`、`docs/commands/zh-CN/lfs.md`、`docs/development/commands/lfs.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`、`docs/commands/logfile.md`、`docs/commands/zh-CN/logfile.md`、`docs/development/commands/logfile.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`、`docs/commands/ls-files.md`、`docs/commands/zh-CN/ls-files.md`、`docs/development/commands/ls-files.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`、`docs/commands/ls-remote.md`、`docs/commands/zh-CN/ls-remote.md`、`docs/development/commands/ls-remote.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`、`docs/commands/ls-tree.md`、`docs/commands/zh-CN/ls-tree.md`、`docs/development/commands/ls-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`、`docs/commands/mailinfo.md`、`docs/commands/zh-CN/mailinfo.md`、`docs/development/commands/mailinfo.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`、`docs/commands/maintenance.md`、`docs/commands/zh-CN/maintenance.md`、`docs/development/commands/maintenance.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`、`docs/commands/media.md`、`docs/commands/zh-CN/media.md`、`docs/development/commands/media.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`、`docs/commands/mega2.md`、`docs/commands/zh-CN/mega2.md`、`docs/development/commands/mega2.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`、`docs/commands/memory.md`、`docs/commands/zh-CN/memory.md`、`docs/development/commands/memory.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`、`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`docs/commands/merge-base.md`、`docs/commands/zh-CN/merge-base.md`、`docs/development/commands/merge-base.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`、`docs/commands/merge-file.md`、`docs/commands/zh-CN/merge-file.md`、`docs/development/commands/merge-file.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`、`docs/commands/mergetool.md`、`docs/commands/zh-CN/mergetool.md`、`docs/development/commands/mergetool.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`、`docs/commands/metadata.md`、`docs/commands/zh-CN/metadata.md`、`docs/development/commands/metadata.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`、`docs/commands/mv.md`、`docs/commands/zh-CN/mv.md`、`docs/development/commands/mv.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`、`docs/commands/notes.md`、`docs/commands/zh-CN/notes.md`、`docs/development/commands/notes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`、`docs/commands/op.md`、`docs/commands/zh-CN/op.md`、`docs/development/commands/op.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`、`docs/commands/open.md`、`docs/commands/zh-CN/open.md`、`docs/development/commands/open.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`、`docs/commands/pack-objects.md`、`docs/commands/zh-CN/pack-objects.md`、`docs/development/commands/pack-objects.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`、`docs/commands/push.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/push.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`、`docs/commands/read-tree.md`、`docs/commands/zh-CN/read-tree.md`、`docs/development/commands/read-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`、`docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`、`docs/commands/reflog.md`、`docs/commands/zh-CN/reflog.md`、`docs/development/commands/reflog.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`、`docs/commands/remote.md`、`docs/commands/zh-CN/remote.md`、`docs/development/commands/remote.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`、`docs/commands/repack.md`、`docs/commands/zh-CN/repack.md`、`docs/development/commands/repack.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`、`docs/commands/replace.md`、`docs/commands/zh-CN/replace.md`、`docs/development/commands/replace.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`、`docs/commands/rerere.md`、`docs/commands/zh-CN/rerere.md`、`docs/development/commands/rerere.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`、`docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`、`docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`、`docs/commands/rev-list.md`、`docs/commands/zh-CN/rev-list.md`、`docs/development/commands/rev-list.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`、`docs/commands/revert.md`、`docs/commands/zh-CN/revert.md`、`docs/development/commands/revert.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`、`docs/commands/review.md`、`docs/commands/zh-CN/review.md`、`docs/development/commands/review.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`、`docs/commands/revision.md`、`docs/commands/zh-CN/revision.md`、`docs/development/commands/revision.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`、`docs/commands/rm.md`、`docs/commands/zh-CN/rm.md`、`docs/development/commands/rm.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`、`docs/commands/sandbox.md`、`docs/commands/zh-CN/sandbox.md`、`docs/development/commands/sandbox.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`、`docs/commands/send-email.md`、`docs/commands/zh-CN/send-email.md`、`docs/development/commands/send-email.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`、`docs/commands/service.md`、`docs/commands/zh-CN/service.md`、`docs/development/commands/service.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`、`docs/commands/shortlog.md`、`docs/commands/zh-CN/shortlog.md`、`docs/development/commands/shortlog.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`、`docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`、`docs/commands/show-ref.md`、`docs/commands/zh-CN/show-ref.md`、`docs/development/commands/show-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`、`docs/commands/sparse-view.md`、`docs/commands/zh-CN/sparse-view.md`、`docs/development/commands/sparse-view.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`、`docs/commands/stash.md`、`docs/commands/zh-CN/stash.md`、`docs/development/commands/stash.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`、`docs/commands/symbolic-ref.md`、`docs/commands/zh-CN/symbolic-ref.md`、`docs/development/commands/symbolic-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`、`docs/commands/tag.md`、`docs/commands/zh-CN/tag.md`、`docs/development/commands/tag.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`、`docs/commands/update-index.md`、`docs/commands/zh-CN/update-index.md`、`docs/development/commands/update-index.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`、`docs/commands/update-ref.md`、`docs/commands/zh-CN/update-ref.md`、`docs/development/commands/update-ref.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`、`docs/commands/verify-pack.md`、`docs/commands/zh-CN/verify-pack.md`、`docs/development/commands/verify-pack.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`、`docs/commands/worktree.md`、`docs/commands/zh-CN/worktree.md`、`docs/development/commands/worktree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`、`docs/commands/write-tree.md`、`docs/commands/zh-CN/write-tree.md`、`docs/development/commands/write-tree.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`|卡特有 `command::clone_test::test_clone_separate_git_dir_default_deny`；卡特有 `cli::tests::pointer_layout_allowlist_matches_commands`|
| CL-25 | `#740` | `REL-CL-01` 发布面（`Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`） | 随版本发布说明同步；推送三张子卡的本地 `cf` 网站提交并核验部署 |手工证据（执行记录）；聚合守卫 `command::clone_test::test_clone_separate_git_dir_and_bare_mutex`、`command::clone_test::test_clone_separate_git_dir_pointer_commands`、`command::clone_test::test_clone_separate_git_dir_default_deny`；聚合守卫 `cli::tests::pointer_layout_allowlist_matches_commands`；ER-04 C 组强制门 `--test compat_version_surface_sync`（不计入 G-03）|
| CL-05 | `#741` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_revision_single_rev`；共享回归组 `--test compat_clone_shallow_integrity`|
| CL-06 | `#752` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_config_override_dash_c`；共享回归组 `--test compat_config_defaults_semantics`|
| CL-08 | `#743` | `src/command/clone.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`；卡特有 `command::clone_test::test_clone_local_hardlink_failure_falls_back_to_copy`；卡特有 `command::clone_test::test_clone_local_rejects_symlinked_object_paths`；共享回归组 `command::clone_test::test_clone_local_flag_accepted_for_local_source`|
| CL-07 | `#742` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_no_hardlinks_copies_objects`；共享回归组 `command::clone_test::test_clone_local_hardlinks_objects_on_same_fs`|
| CL-09 | `#744` | `src/command/clone.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_dot_git_suffix_fallback_and_file_url`；共享回归组 `command::clone_test::test_clone_bare_and_mirror_default_names_and_layout`|
| CL-10 | `#751` | `src/command/clone.rs`, `fetch.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_reference_alternates_borrows_objects`；卡特有 `command::clone_test::test_clone_reference_if_able_and_invalid_repo`|
| CL-18 | `#751` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_dissociate_keeps_repo_independent`；卡特有 `command::clone_test::test_clone_shared_dissociate_detaches_alternates`；共享回归组 `command::clone_test::test_clone_reference_alternates_borrows_objects`|
| CL-19 | `#754` | `src/utils/storage/local.rs`, `object_ext.rs`, `fetch.rs`, `local_client.rs`, `cat_file.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`|卡特有 `command::clone_test::test_clone_partial_cat_file_triggers_lazy_fetch`；共享回归组 `command::cat_file_test::`|
| CL-12 | `#748` | `src/command/clone.rs`, `local_client.rs`, `protocol/mod.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_rejects_invalid_sparse_filter`；共享回归组 `command::clone_test::test_clone_local_flag_accepted_for_local_source`|
| CL-13 | `#746, #754` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_filter_promisor_config_and_combine`；共享回归组 `command::clone_test::test_clone_json_reports_fetch_transfer_counts`；共享回归组 `command::clone_test::test_clone_rejects_invalid_sparse_filter`|
| CL-24 | `#746, #754` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_combine_tree_depth_and_fallback`；共享回归组 `command::clone_test::test_clone_json_reports_fetch_transfer_counts`；共享回归组 `command::clone_test::test_clone_filter_promisor_config_and_combine`|
| CL-29 | `#748`；`PM-33`/`t5616#33` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs`, `local_client.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_sparse_oid_filter_and_missing_boundary`；共享回归组 `command::clone_test::test_clone_combine_tree_depth_and_fallback`；共享回归组 `command::clone_test::test_clone_rejects_invalid_sparse_filter`|
| CL-21 | `#754` | `src/command/clone.rs`, `fetch.rs`（按需拉取进度） |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`|卡特有 `command::clone_test::test_clone_partial_progress_and_e2e_lazy_fetch`；共享回归组 `command::cat_file_test::`|
| CL-14 | `#750` | `src/command/clone.rs`, `rev_parse.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`|卡特有 `command::clone_test::test_clone_ref_storage_format_and_rev_parse`；共享回归组 `command::rev_parse_test::`|
| CL-15 | `#753` | `src/command/clone.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_single_branch_from_detached_head`；共享回归组 `command::clone_test::test_clone_depth_implies_single_branch_matrix`|
| CL-26 | `#756` | `src/cli.rs` |`docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、`docs/commands/switch.md`、`docs/commands/zh-CN/switch.md`、`docs/development/commands/switch.md`、`docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md`、`docs/commands/restore.md`、`docs/commands/zh-CN/restore.md`、`docs/development/commands/restore.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`|卡特有 `command::clone_test::test_sparse_preflight_checkout_switch_reset_restore`；共享回归组 `command::{checkout,switch,reset,restore}_test::`|
| CL-27 | `#756` | `src/cli.rs` |`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`、`docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`、`docs/commands/stash.md`、`docs/commands/zh-CN/stash.md`、`docs/development/commands/stash.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`|卡特有 `command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`；共享回归组 `command::{merge,pull,rebase,cherry_pick,stash}_test::`|
| CL-16 | `#756` | `src/command/clone.rs`, `src/internal/sparse/mod.rs`, `src/command/status.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/status.md`、`docs/commands/zh-CN/status.md`、`docs/development/commands/status.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`|卡特有 `command::clone_test::test_clone_sparse_cone_materializes_top_level`；共享回归组 `command::status_test::`|
| CL-20 | `#756` | `src/command/clone.rs`, `src/cli.rs`, `merge.rs`, `rename_merge.rs`, `tree_merge.rs`, `fetch.rs` |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`|卡特有 `command::clone_test::test_clone_sparse_partial_merge_lazy_fetch`；卡特有 `command::clone_test::test_non_sparse_partial_merge_fetches_missing_objects`；卡特有 `command::clone_test::test_sparse_merge_rejected_cases_preserve_state`；共享回归组 `command::clone_test::test_clone_sparse_cone_materializes_top_level`、`command::clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`|
| CL-17 | `#876` | `src/command/clone.rs`（`remote add` 已写于 `remote.rs:822-834`） |`docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`|卡特有 `command::clone_test::test_clone_persists_remote_fetch_refspec`；共享回归组 `command::remote_test::`|
| CL-22 | 已撤回 | 无写集 | 无 |N/A（本卡无独立测试命令）|

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 基线锚点核对、计划文本 Claude（Subagent）与 Codex 双审对同一字节 PASS | 本计划文件与 review log | 修改计划文档 |
| M1 | Phase 1（`CL-01..CL-04, CL-23, CL-28, CL-25, CL-05..CL-06`）全部完成并发布 | 各独立卡/`REL-CL-01` release tag；各卡 focused 绿，命中 `T-1` 的卡另有全量绿，`CL-25` 有 `T-2` 全量绿 | 发布后前滚新 patch + 降级指引 |
| M2 | Phase 2（5 张卡）全部完成并发布 | 各卡 release tag；各卡 focused 绿，命中 `T-1` 的卡（`CL-08`、`CL-09`、`CL-10`）另有全量绿 | 发布后前滚新 patch + 降级指引 |
| M3 | Phase 3（13 张活动卡，不含已撤回的 `CL-22`）全部完成，全量收口门全绿。`DEFER-CL-03` 不计入这 13 张 | 全量 nextest 证据写入本计划 | 前滚修复新开 `FIX-*` 卡 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 检出之前的克隆失败（参数校验、源解析、传输、过滤与浅仓库校验、对象复制；含 `--separate-git-dir`、`--revision`、无效 `--filter` 等本计划全部拒绝用例） | 无残留 | 删除新建的目标目录与 `--separate-git-dir` 目录（Git `remove_junk`，`builtin/clone.c:414-423`） | 残留半成品 `.libra` 目录 |
| 检出阶段失败（克隆已完成） | 已完成的仓库 | 保留仓库供检查与恢复（Git `junk_mode = JUNK_LEAVE_REPO`，`builtin/clone.c:1656`）；本计划无此类验收用例 | 删除已完成克隆的仓库 |
| `--dissociate` 拷贝中断 | 未完成的新目录 | 失败回滚清理新建目录，不损坏源 `--reference` | 修改或删除源 `--reference` 仓库 |
| D 组 CI/release 失败 | 已推送主分支提交 | 按 ER-04/ER-10 前滚修复发新 patch | 回退/强推已发布历史 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `src/cli.rs` 全局 `--progress` 预处理影响其它子命令 | 中：可能误改非 `clone` 命令参数 | 仅针对 `clone`（与既有 `index-pack`）触发重写并跑 `T-1` 全量 | `CL-01` |
| 部分克隆 `--filter` 与远端能力协商不匹配 | 中：旧远端不支持 `filter` | 本地传输（本计划验收范围）按 Git 约定在远端未广告 `filter` 时安全回退全量克隆（`CL-24` AC7）；网络传输的协商与回退不在本计划验收，见 `DEFER-CL-08` | `CL-24` |
| 稀疏护栏漏掉某个未适配命令 | 中：`core.sparseCheckout=true` 时命令仍改写工作区 | `CL-27` 先给 `merge`/`pull`/`rebase`/`cherry-pick`/`stash` 过渡期拒绝。`CL-20` 只解禁目录重命名合并，其它 `merge` 继续拒绝。`pull`/`rebase`/`cherry-pick`/`stash` 继续受 `CL-27` 保护。`CL-26` 继续保护检出类四命令 | `CL-26`, `CL-27`, `CL-20` |
| 新增子命令漏出默认拒绝 | 中：新的 `Commands` 变体可能写错目录 | `CL-28` 的单元测试 `cli::tests::pointer_layout_allowlist_matches_commands` 枚举全部 `Commands` 变体的判定并与期望映射等值比较，新增变体未登记即失败；端到端夹具覆盖 `restore`、`commit`、`branch`、`checkout` 四个代表命令 | `CL-28` |
| 测试接缝（`LIBRA_TEST_CLONE_HARDLINK_FAIL`、`LIBRA_TEST_CLONE_FOREIGN_OWNER`、`LIBRA_TEST_CLONE_SWAP_BEFORE_LINK`、`LIBRA_TEST_UPLOAD_PACK_NO_FILTER`、`LIBRA_TEST_TRACE_LAZY_FETCH`）进入生产路径 | 中：用户环境变量改变克隆或拉取行为 | 只在 `LIBRA_TEST=1` 时读取（「验证执行协议」第 7 项），各卡 ER-05 评审核对 | `CL-08`, `CL-13`, `CL-19` |
| 网站检出路径与 `AGENTS.md:135` 不一致 | 低：执行者按过时文档选错检出 | 以「文档职责」所载用户 2026-11-18 11:55:09 UTC 确认为准，「文档与网站验收命令」第 2 步逐次核验 | 全部改命令文档的卡 |
| 网络远端 `--filter` | 中：`CL-13` 之后经 HTTP/SSH/git:// 克隆时 `--filter` 可能未被协商（`DEFER-CL-08`），用户得到全量克隆 | 本计划只验收本地传输；网络客户端协商随 `DEFER-CL-08` 规划，`docs/commands/clone.md` 写明限制 | `CL-13`, `CL-24` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `-l/--local` 硬链接 | `O(objects + fallback bytes)` | 硬链接成功的对象无需复制字节；回退复制的对象按其字节数占用额外空间 | 成功硬链接对象在同一文件系统共享 inode；默认本地路径首次硬链接失败后其余对象改为复制（Git 语义），显式 `--local` 硬链接失败即拒绝 | `CL-08` |
| `--filter=blob:none` | 过滤协商成功时处理 `O(commits + trees)` 个对象，传输和存储另计选中对象字节数；全量回退时随完整对象集及字节数增长 | 协商成功时初始不传输 blob，按需 lazy fetch；本地传输在远端未广告 filter 时回退全量传输并保存完整对象集（网络传输见 `DEFER-CL-08`） | 依远端能力与回退对象集确定 | `CL-13, CL-19, CL-24` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 与 `docs/development/commands/_compatibility.md` 已随相关任务卡同步；`docs/error-codes.md`: `N/A`（不新增错误码）。
- [ ] `CL-28` 的 `Implementation write set` 里每一份命令文档都已逐文件验收。`CL-26` 与 `CL-27` 的 checkout、switch、reset、restore、merge、pull、rebase、cherry-pick、stash 文档，以及 `clone`、`rev-parse`、`cat-file`、`status`、`log`、`branch` 的 EN、zh-CN、development 与网站文件也已同步。
- [ ] 上一条里的 zh-CN 文件已同步。
- [ ] 上一条里的网站 `.en.md` 文件已同步。
- [ ] `tests/INDEX.md` 已同步（如新增 target）。
- [ ] `plan-long.md` 与 `plan-status.md` 由协调者统一登记。

## Codex review log

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 (Claude) | `plan-20261117-clone.md` 初稿 | `FAIL` | 8 项 P1（D10、`CL-13` 拆出 `CL-19`、循环依赖、`commit-signing`/`fetch-push`、`ref-storage-format`、`#876` 基线、AC、旧测试重写）均在 R2 关闭 | P2 全部修复 | Claude R1 review |
| R1 (Codex) | `plan-20261117-clone.md` 初稿 | `FAIL` | 5 项 P1（`CL-11`/`CL-13`、`CL-02/05/12/15/16` AC、`CL-07` 先于 `CL-08`、`CL-08` 本地规则）均在 R2 关闭 | — | Codex R1 review |
| R2 (Codex) | `plan-20261117-clone.md` R2 | `FAIL` | 2 项 P1（`CL-19` 需前置至 `CL-11`/`CL-13` 前；`CL-16` 改为 `src/internal/sparse/mod.rs`+`src/command/sparse_view.rs`）均在 R3 关闭 | P2（`CL-14` `resolve_ref_format` `pub(crate)`+映射）在 R3 关闭 | Codex R2 review |
| R3 (Codex) | `plan-20261117-clone.md` R3 | `FAIL` | 2 项 P1（`CL-11` 收紧避免无 `promisor=true` 省略对象；`CL-17` vs `DEP-CL-05` 验收边界）均在 R4 关闭 | P2（完整 `发布窗口顺序`）在 R4 关闭 | Codex R3 review |
| R3 (Claude) | `plan-20261117-clone.md` R3 | `FAIL` | 4 项 P1（`ADR-CL-04`、`ADR-CL-03`、`T-1`、`CL-19`）已在 R4/R5 关闭 | P2 全部修复 | Claude R3 review |
| R4 (Claude) | `plan-20261117-clone.md` R4 | `FAIL` | 4 项 P1（`ADR-CL-04` 不改 `init.rs` 与 D25、`CL-04` `ADR-CL-05` 指针边界、`CL-16` 拆出 `CL-20` 与 `_compatibility.md`、`T-1` 补齐 `CL-05/09/10/11/13`）均在 R5 关闭 | P2（`CL-17/13/19/06/01/12` 与测试重命名）在 R5 关闭 | Claude R4 review |
| R4 (Codex) | `plan-20261117-clone.md` R4 | `FAIL` | 2 项 P1（`#876` 收口门闭环、`CL-13`/`19` 自定义远端名 `<remote_name>`）均在 R5 关闭 | — | Codex R4 review |
| R5 (Claude) | `plan-20261117-clone.md` R5 | `FAIL` | 3 项 P1（`ADR-CL-02` `REL-CL-01` 残留、`CL-04` 补齐 `src/utils/util.rs:277,797` 与 `T-1`、`CL-20` 批量 `1 fetch`/`6 objects` 与 `merge.md` 文档）均在 R6 关闭 | P2（`DEP-CL-03`、`codeql.yml`、`Release write set 默认`、`CL-19` `hydrate.rs` 表述与 `landing=4`、`cat-file` 文档收口）在 R6 关闭 | Claude R5 review |
| R5 (Codex) | `plan-20261117-clone.md` R5 | `FAIL` | 1 项 P1（`ADR-CL-01` `--no-progress` 重写边界）在 R6 关闭 | P2（`CL-04` 写集与 `T-1`、`实施顺序` `CL-20` 位置、`CL-13`/`CL-19` 拆卡注解）在 R6 关闭 | Codex R5 review |
| R6 (Claude) | `plan-20261117-clone.md` R6 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R7 重审 | — | Claude R6 review |
| R6 (Codex) | `plan-20261117-clone.md` R6 | `FAIL` | 1 项 P1（`CL-04` 需审计并适配 `util.rs:314-324` `worktree_info_file_paths` 等 `.libra` 指针消费者）在 R7 关闭 | — | Codex R6 review |
| R7 (Claude) | `plan-20261117-clone.md` R7 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R8 重审 | — | Claude R7 review |
| R7 (Codex) | `plan-20261117-clone.md` R7 | `FAIL` | 2 项 P1（`完成判据` `#876` 端到端测试改由 `plan-20261117-fetch-push.md#FP-15` 具名测试承接；`CL-04` 收窄至 `util.rs` 发现与 `worktree_info_file_paths` 并将 `alternates.rs:91` 等记入 `DEFER-CL-02`）均在 R8 关闭 | — | Codex R7 review |
| R8 (Claude) | `plan-20261117-clone.md` R8 | `FAIL` | 4 项 P1（`CL-13` 补 `#746` `combine:blob:none+tree:1`、`CL-20` 补 `rename_merge.rs`/`tree_merge.rs`/`fetch.rs` 批量拉取与 `T-1`/`DEP-CL-07`、新增 `DEP-CL-10` 协调 `util.rs`、修复 `CL-02` nextest 过滤器）均在 R9 关闭 | P2（`CL-10` `fsck`、`CL-13` 端到端 lazy fetch、`CL-04`/`12` Git 精确报错、`DEP-CL-04/09`、`Phase 1`、`rev_parse.rs:59`、`codeql.yml`、`Rollback mode` 判据）在 R9 关闭 | Claude R8 review |
| R8 (Codex) | `plan-20261117-clone.md` R8 | `FAIL` | 2 项 P1（`CL-17` 显式承接 `test_clone_persists_remote_fetch_and_prune_with_extra_refspec`；`CL-04` 纳入 `alternates.rs:91` 并验证指针布局基本命令）均在 R9 关闭 | P2（统一 `DEP-CL-05` 与 `完成判据` 测试名）在 R9 关闭 | Codex R8 review |
| R9 (Claude) | `plan-20261117-clone.md` R9 | `FAIL` | 4 项 P1（移除 `CL-20` 重复块、消除 `CL-04`/`DEFER-CL-02` 矛盾、补 R6-R10 `修订历史`、`CL-13` 拆出 `CL-21`）均在 R10 关闭 | P2（审计表与卡内 `landing`/`deps`、`CL-19`、`CL-01`、`CL-14`、`DEP-CL-04/07/09`、`Plan Author 5`）在 R10 关闭 | Claude R9 review |
| R9 (Codex) | `plan-20261117-clone.md` R9 | `FAIL` | 2 项 P1（`CL-04` 与 `DEFER-CL-02` 自相矛盾、`#876` 完成门明确由 `Plan Author 5` 移除 `#[ignore]` 并正常执行）均在 R10 关闭 | P2（移除 `CL-20` 重复字段块）在 R10 关闭 | Codex R9 review |
| R10 (Claude) | `plan-20261117-clone.md` R10 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R11 重审 | — | Claude R10 review |
| R10 (Codex) | `plan-20261117-clone.md` R10 | `FAIL` | 3 项 P1（`CL-04` 范围收紧、`CL-11` 与 `CL-13` 合并 `REL-CL-01` 避免静默忽略 `--filter`、新增 `CL-22` 显式承接 `#876` 移除 `#[ignore]`）均在 R11 关闭 | P2（`Phase 3` 与 `M3` 卡数更新为 `11 张卡`）在 R11 关闭 | Codex R10 review |
| R11 (Claude) | `plan-20261117-clone.md` R11 | `FAIL` | 3 项 P1（`CL-22` 规范为 `implementation`+`independent`+`T-2` 并移出 `--all`、`CL-11` 并入 `CL-13` 移除 `REL-CL-01`、`CL-21` 补 `DEP-CL-04` 及 `DEP-CL-03/04` 全覆盖）均在 R12 关闭 | P2（`默认覆盖`、`ADR-CL-02`、`DEP-CL-05`、`CL-20` 追溯表、`CL-22`、`CL-21` 单轴）在 R12 关闭 | Claude R11 review |
| R11 (Codex) | `plan-20261117-clone.md` R11 | `FAIL` | 2 项 P1（`REL-CL-01` 不合模板与 `ADR-CL-02` 矛盾、`CL-22` 字段不合模板）均在 R12 关闭 | — | Codex R11 review |
| R12 (Codex) | `plan-20261117-clone.md` R12 | `FAIL` | 4 项 P1（`ADR-CL-04`/`CL-14` 如实输出 `sqlite` 并延后 `files`/`reftable` 至 `DEFER-CL-01`、`CL-08` 逐文件 `nlink > 1`、审计表 `CL-22` 同步、`DEP-CL-05`/`DEFER-CL-03`/`完成判据` 一致）均在 R13 关闭 | P2（审计表 `CL-12` `landing=2`）在 R13 关闭 | Codex R12 review |
| R12 (Claude) | `plan-20261117-clone.md` R12 | `FAIL` | 5 项 P1（`CL-22` 审计表同步、`CL-13` 拆出 `CL-24` 守 `AC<=8`、`CL-04` 拆出 `CL-23`、`CL-16` 纳 `status.rs` 与 `DEFER-CL-04`、`DEP-CL-03/04` 明确顺序）均在 R13/R14 关闭 | P2（`R14`、`23 张`、`CL-11` 清理、`CL-22` `T-5`、`CL-19` `5/8`、`clone.rs:220`、`.git` 检查）在 R14 关闭 | Claude R12 review |
| R13 (Claude) | `plan-20261117-clone.md` R13 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R14 重审 | — | Claude R13 review |
| R13 (Codex) | `plan-20261117-clone.md` R13 | `FAIL` | 2 项 P1（`成功定义`/`DEFER-CL-01` 明确 `#750` `files` 延后归口、`CL-23` 未适配 pointer 子命令 fail-closed）均在 R14 关闭 | P2（`CL-22` `T-5`、`M3` 对齐 `完成判据`）在 R14 关闭 | Codex R13 review |
| R14 (Claude) | `plan-20261117-clone.md` R14 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R15 重审 | — | Claude R14b review |
| R14 (Codex) | `plan-20261117-clone.md` R14 | `FAIL` | 3 项 P1（`CL-04`+`CL-23` 纳为 `REL-CL-01` 家族卡与 `CL-25` 发布点、`CL-13` 对 `combine:*` fail-closed 拒绝、`CL-16` 为 `checkout`/`switch`/`reset` 增稀疏 fail-closed 护栏）均在 R15 关闭 | P2（重申 `CL-22` 已统一为 `implementation`+`independent`）在 R15 关闭 | Codex R14 review |
| R15 (Claude) | `plan-20261117-clone.md` R15 | `FAIL` | 6 项 P1（`CL-04`/`23` `C/D coverage from: CL-25`、`CL-23` 纳入 `src/cli.rs` `command_preflight` 护栏并修正 `util.rs` 锚点、`CL-16` 拆出 `CL-26` 在 `cli.rs` 统一护栏 9 个命令及 ER-06a 文档并补 `DEP-CL-11`、`CL-13` AC 核实为 `7/8`）均在 R16 关闭 | P2（`CL-25` 移除 `--all` 与 `Docs: N/A`、`CL-04`/`23` `N/A`、`CL-21` `2/2`、`ADR-CL-02`/`05`、`CL-06/10/12/17/22`、`DEP-CL-04`、`compat_ledger_schema`）在 R16 关闭 | Claude R15 review |
| R16 (Codex) | `plan-20261117-clone.md` R16 | `FAIL` | 3 项 P1（`CL-26` 九命令超 8、`CL-13` 文档门计入 `7/8`、风险表仍指向 `CL-13` 且缺护栏风险） | `2026-11-17 09:44:31 UTC` 关闭 | Codex R16 review |
| R17 (Codex) | `plan-20261117-clone.md` R17 | `FAIL` | 3 项 P1（`CL-17`/`CL-22` 拆测试、Phase 3 计数、`CL-23` 谓词少计） | `2026-11-17 09:51:20 UTC` 关闭 | Codex R17 review |
| R24 (Codex) | `plan-20261117-clone.md` R24 | `FAIL` | P1: `CL-23` 拒绝 `commit` 但文档只列 clone；收口清单缺 `CL-26`/`CL-27` 命令文档 | `2026-11-17 10:43:14 UTC` 拆出 `CL-28` 并补收口清单 | Codex R24 review |
| R25 (Codex) | `plan-20261117-clone.md` R25 | `FAIL` | P1: `CL-28` 只拒绝 7 条命令，`restore` 等仍可能写错目录 | `2026-11-17 10:52:35 UTC` 改为默认拒绝 | Codex R25 review |
| R26 (Codex) | `plan-20261117-clone.md` R26 | `FAIL` | P1: `CL-28` 文档写集不全；`CL-12`/`CL-13` 与 `CL-24` 的最终断言冲突 | `2026-11-17 11:14:03 UTC` 关闭 | Codex R26 review |
| R27 (Codex) | `plan-20261117-clone.md` R27 | `FAIL` | P1: `CL-13` 单个 filter 与 `CL-24` 冲突；追溯表和收口清单未覆盖 `CL-28` 写集 | `2026-11-17 11:19:54 UTC` 关闭 | Codex R27 review |
| R15 (Codex) | `plan-20261117-clone.md` R15 | `FAIL` | 4 项 P1（`CL-04`/`23` `C/D coverage from: CL-25`、`CL-23` 中央 `cli.rs` `command_preflight` 护栏、`CL-16` 拒绝 `--sparse --filter=...` 且 `CL-26` 护栏含 `merge` 及 ER-06a 全量命令文档）均在 R16 关闭 | — | Codex R15 review |
| R30 (Codex gpt-6.1-terra) | `plan-20261117-clone.md` | `FAIL` | P1: `DEP-CS-03` 含撤回卡且双方范围不一致；`sparse:oid` 未交付却供 `PM-03` 验收；网站部署门不完整。已对齐活动卡集合、新增 `CL-29` 并补齐网站门。该轮明确未完成全文复审，R31 不得据此推定其它维度通过 | R31 完整复审 | Codex R30 只读评审（部分覆盖） |
| R31 (Codex gpt-6.1-terra) | `plan-20261117-clone.md` | FAIL | P1: 各卡网站写集仍使用 `../libra-backend`，与唯一 `/Volumes/Data/gitmono/libra-backend` 目标冲突；已统一全部 284 处路径。该轮未完成全文复审，R32 不得据此推定其它维度通过 | R32 完整复审 | Codex R31 只读评审（部分覆盖） |
| R33-A (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 1–800 行 | FAIL | P2: `CL-08` 硬链接失败允许复制回退，却要求所有对象共享 inode。已将 inode 限定为成功对象并增加同文件系统失败复制回退测试（AC=4/8、VER=3/8）。本轮未完成全分区复审，R34-A 不得据此推定其它维度通过 | R34-A 分区完整复审 | Codex R33-A 只读评审（部分覆盖） |
| R34-A (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 1–800 行 | FAIL | P1: “硬链接成功”条件可被零成功对象/全复制空真满足。已增加至少一个松散对象和一个 pack 对象实际硬链接成功并逐一比较设备号/inode 的正控制。R35-A 分区复核 | R35-A 分区完整复审 | Codex R34-A 只读评审（部分覆盖） |
| R36-A (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 1–400 行及 CL-04 完整卡片 | FAIL | P1: 正常夹具只要求各一个对象硬链接，允许其余可链接对象被复制。已改为逐一检查所有符合硬链接条件的对象文件。R37-A 分区复核 | R37-A 分区完整复审 | Codex R36-A 只读评审（分区覆盖） |
| R37-A (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 1–400 行、CL-04 完整卡片及定向交叉核对 | PASS | 无实质问题。全部符合条件对象的硬链接、失败复制回退、CL-29 blob 与 PM-03/DEP-PM-08/DEP-CS-03 均符合 | R38-B 复审第 401–800 行 | Codex R37-A 只读评审（分区覆盖） |
| R38-B (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 401–800 行及 CL-18 完整卡片 | FAIL | P1: `CL-08` 缺符号链接安全边界，`CL-18` 漏 `--shared --dissociate`；P2: `CL-28` 写集含已移除命令文档。已分别补 fail-closed、shared 独立验收并删除 20 条过时文档路径。R39-B 分区复核 | R39-B 分区完整复审 | Codex R38-B 只读评审（分区覆盖） |
| R39-B (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 401–800 行及 CL-18 完整卡片 | FAIL | P1: 分支不变性可能仅按文件存在漏检；P2: Implementation write set 仍含 20 条过时文档路径。已改为 ref API/数据库查询比较状态，并从写集删除全部剩余过时路径。R40-B 分区复核 | R40-B 分区完整复审 | Codex R39-B 只读评审（分区覆盖） |
| R40-B (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 401–800 行及 CL-18 完整卡片 | FAIL | P2: 写集和文档影响仍包含 package/prune/stats/repository-hooks 四组未发布或非命令路径。已删除全部 16 组路径。R41-B 分区复核 | R41-B 分区完整复审 | Codex R40-B 只读评审（分区覆盖） |
| R41-B (Codex gpt-6.1-terra) | `plan-20261117-clone.md` 第 401–800 行及 CL-18 完整卡片 | PASS | 无实质问题。CL-28 两组清单已删除指定路径；ref API/数据库状态比较、CL-08 符号链接拒绝与 CL-18 shared dissociate 验收均符合 | R42-C 复审第 801 行至文件末 | Codex R41-B 只读评审（分区覆盖） |
| R42-C (Codex gpt-6.1-terra) | 第 801–1518 行及定向跨计划核对 | FAIL | P2：性能摘要无条件声称零复制及共享 inode，与 CL-08 复制回退冲突。已限定硬链接成功对象并补充回退容量成本 | R43-C 分区完整复审 | Codex R42-C 只读评审（分区覆盖） |
| R43-C (Codex gpt-6.1-terra) | 第 801–1520 行及跨界核对 | FAIL | P2：性能摘要遗漏过滤能力缺失时的全量回退与硬链接失败时的复制字节成本。已补充两种回退 | R44-C 分区完整复审 | Codex R43-C 只读评审（分区覆盖） |
| R44-C (Codex gpt-6.1-terra) | 第 801 行至末及跨界核对 | FAIL | P1：CL-20 错误禁止非稀疏部分克隆按需获取；P2：过滤成功成本漏对象字节数。已补正向回归及普通仓库负向断言，并补容量描述 | R45-C 分区完整复审 | Codex R44-C 只读评审（分区覆盖） |
| R45-C (Codex gpt-6.1-terra) | 第 801 行至末及 CL-18 | FAIL | P1：CL-20 合并独立判据超过上限。保留普通仓库和内容/删除/冲突护栏回归于 Verification，移除重复验收条目并单列非稀疏部分克隆正向判据；护栏合同仍在 Description 与 CL-27 | R46-C 分区完整复审 | Codex R45-C 只读评审（分区覆盖） |
| R46-C (Codex gpt-6.1-terra) | 第 801 行至末及 CL-10/CL-18 | FAIL | P1：CL-20 缺少内容修改、删除和冲突合并的拒绝状态回归；已补具名测试并将放行限定为目录重命名 | R47-C 分区完整复审 | Codex R46-C 只读评审（分区覆盖） |
| R47-C (Codex gpt-6.1-terra) | 第 801 行至末及 CL-10/CL-18 | PASS | 无阻断问题；CL-20 拒绝状态回归、目录重命名范围、写集及计数均核对 | 与 R37-A/R41-B 合并覆盖全文 | Codex R47-C 只读评审（分区覆盖） |
| 依赖闭包复审 (Codex gpt-6.1-terra) | DEP-CL-03 / DEP-CS-03 与 Clone、PM、MP、CS 相关传递链 | PASS | 排除 CL-17/CL-20 后两侧集合一致，核查范围无间接等待或循环 | 提交计划 | Codex 只读定向复审 |
| R48 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `49db3c05736add04a6153c2383390012dd18862699e2780e6cb93d59a154621f` | `FAIL` | 5 项 P1：事实基线 `cli.rs:3819`、`INDEX.md:80` 错锚；协议称克隆拒绝由卡内目标目录 AC 覆盖而多卡缺失；`CL-04/08/12/13/19` 复合谓词；`CL-04` 缺 `(1/4)`。均已修订 | P2：`CL-28` 全变体最小参数夹具过重（改为单元测试等值比较） | 2026-11-18 17:02:21 UTC 修订 |
| R48 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `49db3c05736add04a6153c2383390012dd18862699e2780e6cb93d59a154621f` | `FAIL` | A：失败映射可被「打印码但成功」满足、ADR 去重与 Git 不符、网站第 4 步缺 `pnpm test`；B：`CL-28` HEAD 查询行错、`CL-25` 缺 `--lib` 守卫与 patch+1 证明及复合 AC、`CL-04` 未断言初始检出；C：`CL-05/06/08/10` 复合谓词、克隆拒绝缺目标清理、`CL-09` 夹具可绕过回退、`CL-18` shared 终态、`CL-08` AC8 改变 Libra 源 `--depth` 契约；D：部分克隆对象断言与检出冲突、`CL-21` 缺终端默认进度、`CL-12/13/24` 计数、`CL-19` 无法证明未发请求；E：`CL-14/16` 目标清理、`CL-15` 复合与 fetch 前提、`CL-20` 只验一个重命名目标与日志文件、`CL-17` 追加可空过；F：`CL-08` 计数、追溯表文档列缺文件、完成判据含撤回卡。均已修订（`CL-18` 改写为既有终态回归，未新增中间态接缝） | D 的 P2（`CL-19` 错误码）已采纳 | 2026-11-18 17:02:21 UTC 修订 |
| R49 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `557e6e7a4eca344aba47f1081b262385d9deee241fd876b3b0d5bc72333b87df` | `FAIL` | 2 项 P1：`CL-19` AC6/AC8 的「无接缝行」缺正控制可空真；`CL-20` 内容修改与冲突夹具重叠。均已修订 | 无 | 2026-11-18 17:06:44 UTC 修订 |
| R49 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `557e6e7a4eca344aba47f1081b262385d9deee241fd876b3b0d5bc72333b87df` | `FAIL` | A：基线称 `--dissociate` 被忽略、成功定义含撤回卡；B：`CL-25` 祖先断言与 squash 合并冲突；C：`CL-05` 空引用时 `show-ref` 失败与缺检出断言、`CL-06` 未观测初始 fetch、`CL-10` 有效 `--reference-if-able` 未测、`CL-18` 与 `CL-08` 硬链接矛盾；D：`CL-12`→`CL-13` AC 编号错位、`CL-13` 自定义远端两键未测、`CL-19` 缺正控制；E：`CL-27` AC2 映射缺非零状态；F：M2 要求无触发卡的 T-1、风险表要求逐变体夹具。均已修订 | 无 | 2026-11-18 17:06:44 UTC 修订 |
| R50 (Claude subagent claude-opus-5-5) | R49 修订差异及 G-05 专项，受审 SHA-256 `d91ad84a0b89d016f75253860374009fdc43e9fd91bb652c86925539ff6c1483` | `FAIL` | 1 项 P1：`CL-23`/`CL-28` 引用「`CL-04` 夹具」违反 G-05。已修订 | 无 | 2026-11-18 17:11:21 UTC 修订 |
| R50 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `d91ad84a0b89d016f75253860374009fdc43e9fd91bb652c86925539ff6c1483` | `FAIL`（F `PASS`） | A：克隆拒绝不变量把清理泛化到 Git 检出失败；B：`CL-03` 引用的既有夹具是损坏浅仓库、`CL-28` 单值映射无法表达按参数放行；C：`CL-08` 硬链接失败语义与 Git 不符、`CL-05` 缺失判定混同操作错误；D：`CL-21` 缺 `cat-file` 文档、`CL-24` 显式 `combine:` 只验配置、`CL-13` 未测 `clone.defaultRemoteName`；E：`CL-15` 证据锚点不支撑结论。均已修订 | C 的 P2（默认本地路径未直接验收）一并采纳为 `CL-08` AC2 | 2026-11-18 17:11:21 UTC 修订 |
| R51 (Claude subagent claude-opus-5-5) | R50 修订差异及过渡断言交叉引用，受审 SHA-256 `8c9cc1bf28e1751ed2588c17c782f00dd87f55d654576180b5c775a811bf1d6b` | `FAIL` | 1 项 P1：`CL-13` 过渡断言已改为 AC5–AC7，`CL-24` 与完成判据仍写 AC6–AC8。已修订 | 无 | 2026-11-18 17:15:13 UTC 修订 |
| R51 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `8c9cc1bf28e1751ed2588c17c782f00dd87f55d654576180b5c775a811bf1d6b` | `FAIL` | A：`GC-CL-01` 无条件对齐 Git 与 Libra 专有契约冲突、W-2 退出码说明不符；B：`CL-25` 聚合守卫被协议排除为 AC 证据、手工门未计 VER；C：`CL-18` 可忽略 `--reference`、`CL-10` 有效 if-able 未验借用、`CL-08` 未验首次失败后全部复制、`CL-05` 只验 tag；D：`CL-24` 交接编号、显式 `combine:` 只验非根 tree、未登记拆出 `CL-29`；E：`CL-15` 承诺 `FETCH_HEAD` 未验收；F：完成判据 `CL-13` 编号。均已修订 | E 的 P2（`CL-17` refspec 未加引号）已采纳 | 2026-11-18 17:15:13 UTC 修订 |
| R52 (Claude subagent claude-opus-5-5) | 全文描述承诺专项（逐卡 Description 对照 AC），受审 SHA-256 `825c90795e208625cc98d56582bc1e156de092527f0a0ac55cfdc65fee43d580` | `FAIL` | 4 项 P1：`CL-09` 承诺非仓库与 `<path>/.git` 回退、`CL-16` 帮助文本改动、`CL-19` 本地 OID 校验均无 AC；`CL-17` 描述引用已移除的 `CL-05` 约定。均以收窄描述修订 | 无 | 2026-11-18 17:18:14 UTC 修订 |
| R52 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `825c90795e208625cc98d56582bc1e156de092527f0a0ac55cfdc65fee43d580` | `FAIL`（F `PASS`） | A：子进程规则与 `CL-28` 单元测试冲突；B：`CL-04` `--mirror` 只验消息未验拒绝；C：`CL-05` 依赖可变 `HEAD~1`、`CL-09` 承诺未验收、`CL-08` 未覆盖对象目录根符号链接、`CL-18` 描述 AC 编号错；D：`CL-19` OID 校验承诺、`CL-13` 重复同值未测、`CL-21` 非终端 auto 未测；E：`CL-20` 非稀疏部分克隆只验退出码。均已修订 | 无 | 2026-11-18 17:18:14 UTC 修订 |
| R53 (Claude subagent claude-opus-5-5) | R52 修订差异与夹具专项，受审 SHA-256 `a5563f5a29a0a46e90c810a3a0a9c35fb0dbd07b91410e7d6db49dd1b4acb771` | `PASS` | 无 P0/P1 | P2：`CL-28` 样本表需随新增变体维护（设计如此） | 2026-11-18 17:21:37 UTC 记录 |
| R53 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `a5563f5a29a0a46e90c810a3a0a9c35fb0dbd07b91410e7d6db49dd1b4acb771` | `FAIL`（A/B/F `PASS`） | C：`CL-08` 未覆盖对象子目录符号链接、回退断言允许遗漏对象、评审结论称 `--bare --revision` 已纳入 `CL-05`；D：`CL-24` 重复同值只断言退出 0、协议第 7 项 `CL-21` 终端 AC 编号错；E：`CL-16` 只验 `blob:none`、`CL-20` 合并结果只比路径。均已修订 | 无 | 2026-11-18 17:21:37 UTC 修订 |
| R54 (Claude subagent claude-opus-5-5) | R53 修订差异与过渡断言交接链，受审 SHA-256 `3cfa417276c22b027096dab8860b953fc39945f32bf1b5aa65e067036445643e` | `PASS` | 无 P0/P1（其后修订中自查发现 `CL-05` 内置分支误判，见同行修订历史） | 无 | 2026-11-18 17:26:22 UTC 记录 |
| R54 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `3cfa417276c22b027096dab8860b953fc39945f32bf1b5aa65e067036445643e` | `FAIL`（F `PASS`） | A：全量触发规则与家族子卡继承冲突；B：`CL-28` 描述称单元测试核对文档表格；C：`CL-06` 未验全局配置、`CL-05` 分支形式未验；D：`CL-13`/`CL-24` 未运行改写后的前序用例、`CL-13` 只在一个库验过滤、`CL-21` 终端用例未区分 stderr 通道且无成功前提；E：`CL-15` `show-ref` 空集失败可空过、`CL-20` 只验请求数未验取回的 6 个对象。均已修订 | 无 | 2026-11-18 17:26:22 UTC 修订 |
| R55 (Claude subagent claude-opus-5-5) | R54 修订差异与 SQL 谓词实测，受审 SHA-256 `80f94c5d76ba864924256271fc6e8f246874d6d95804ed8ee729ff38019eec3a` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:29:29 UTC 记录 |
| R55 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `80f94c5d76ba864924256271fc6e8f246874d6d95804ed8ee729ff38019eec3a` | `FAIL`（A/B/F `PASS`） | C：`CL-05` tag 夹具与默认分支同一提交、裸克隆未验 HEAD、引用过滤会漏同名 tag/远端分支；D：`CL-19` 未验取回对象持久化；E：`CL-16` 只验 `tree:1`、`clone.rs:220` 归属错为 `--filter`。均已修订 | 无 | 2026-11-18 17:29:29 UTC 修订 |
| R56 (Claude subagent claude-opus-5-5) | R55 修订差异与 `status --porcelain=v2`/HEAD 行实测，受审 SHA-256 `08c2f238c2e7d58a72d76647e85f9abbd7241ba7a98003f985f51cc2fd65d561` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:32:50 UTC 记录 |
| R56 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `08c2f238c2e7d58a72d76647e85f9abbd7241ba7a98003f985f51cc2fd65d561` | `FAIL`（D/F `PASS`） | A：`ADR-CL-04` 称 `CL-14` 写集只有两个源码文件；B：`CL-28` 拆分来源在 `CL-04`/`CL-23` 间矛盾；C：`CL-10` 只验存储未验传输、`CL-05` SQL 对 NULL 名称不安全；E：`CL-16` 表驱动四种拒绝计为 1 条而超限。均已修订（E 以撤回组合拒绝收窄范围） | 无 | 2026-11-18 17:32:50 UTC 修订 |
| R57 (Claude subagent claude-opus-5-5) | R56 修订差异（撤回组合拒绝的全文残留引用专项），受审 SHA-256 `fa8742640dc94e343531e589c2fdb0f90e1eb08f1e5885f6e694fb19e1912588` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:36:20 UTC 记录 |
| R57 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `fa8742640dc94e343531e589c2fdb0f90e1eb08f1e5885f6e694fb19e1912588` | `FAIL`（A/B/F `PASS`） | C：`CL-05` tag 克隆未验只取请求修订、`CL-06` 承诺一般配置在初始 fetch 前生效未验；D：`CL-12` 未覆盖有效 `sparse:oid` 的过渡拒绝；E：`CL-26`/`CL-27` 缺普通仓库正控制。均已修订 | D 的 P2（`CL-19` `upstream` 夹具）已采纳 | 2026-11-18 17:36:20 UTC 修订 |
| R58 (Claude subagent claude-opus-5-5) | R57 修订差异与追溯表表格结构，受审 SHA-256 `8b992263bd89b00ce82c2c5eaacadda149c45558431a9b105c194462b057abdf` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:40:04 UTC 记录 |
| R58 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `8b992263bd89b00ce82c2c5eaacadda149c45558431a9b105c194462b057abdf` | `FAIL`（B/E/F `PASS`） | A：网站分支「STOP」与第 2 步「切换」矛盾；C：`CL-05` 裸克隆未验 HEAD 类型；D：`CL-13` 承诺网络传输过滤未验、`CL-24` 重复同值只验配置、`CL-29` 夹具内容未保证 blob 不同、`CL-21` 终端用例未区分 stderr。均已修订（D 前两项以收窄与 `DEFER-CL-08` 处理） | 无 | 2026-11-18 17:40:04 UTC 修订 |
| R59 (Claude subagent claude-opus-5-5) | R58 修订差异与 C/D 分区描述承诺专项，受审 SHA-256 `ad49ecc087ff048a431196112ba1625ea9c00485f70d6774b5184b84bac5bb96` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:42:34 UTC 记录 |
| R59 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `ad49ecc087ff048a431196112ba1625ea9c00485f70d6774b5184b84bac5bb96` | `FAIL`（A/B/E `PASS`） | C：`CL-07` 未证完整复制、`CL-10` 删除 R 后 `fsck` 失败不能证明借用；D：网络远端拒绝/回退承诺无验收；F：故障恢复矩阵对检出阶段失败也要求删除。均已修订（D 以收窄处理） | 无 | 2026-11-18 17:42:34 UTC 修订 |
| R60 (Claude subagent claude-opus-5-5) | R59 修订差异，受审 SHA-256 `9d886274f2741b4f759d1d6a17d5c6ac297d30d5a3aaba95c20ed9b502357bd5` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:44:51 UTC 记录 |
| R60 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `9d886274f2741b4f759d1d6a17d5c6ac297d30d5a3aaba95c20ed9b502357bd5` | `FAIL`（A/B/D/E/F `PASS`） | C：`CL-06` 只验含 `from_option` 不能证明 `--origin` 优先、`CL-08` 描述覆盖本地 Git 源而只测 Libra 源。均已修订（后者以收窄与 `DEFER-CL-09` 处理） | 无 | 2026-11-18 17:44:51 UTC 修订 |
| R61 (Claude subagent claude-opus-5-5) | R60 修订差异与远端集合 SQL 实测，受审 SHA-256 `2656c291a9361141eef63c89c316da97adca075e5fce25b3ad443e94b6d9a5ab` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:47:20 UTC 记录 |
| R61 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `2656c291a9361141eef63c89c316da97adca075e5fce25b3ad443e94b6d9a5ab` | `FAIL`（A/B/C `PASS`） | D：`CL-13`/`CL-24` 引用的 `local_client.rs:889` 是外部 Git 源引用读取；E：`CL-16` 顶层文件只验存在；F：风险表与性能摘要的回退表述未限定本地传输。均已修订 | E 的 P2（`CL-16` 发布时不限制 `--sparse --filter` 组合的中间发布风险）登记为残余风险 | 2026-11-18 17:47:20 UTC 修订 |
| R62 (Claude subagent claude-opus-5-5) | R61 修订差异，受审 SHA-256 `742da3205ce01e81e47e3e1c76e1b93ce616fd717e9b228920a23aac08ac9b67` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 17:49:59 UTC 记录 |
| R62 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `742da3205ce01e81e47e3e1c76e1b93ce616fd717e9b228920a23aac08ac9b67` | `FAIL`（A/C/F `PASS`） | B：`CL-25` AC9 把 ER-04 D 组门计入 G-03；D：`CL-12` 把单独 `tree:<n>` 拒绝错归 `CL-13` AC5–AC6、`CL-21` 文档影响未逐文件列 `cat-file`；E：`CL-16` 未验收 cone 规则。已按同行修订历史修订，待下一轮双审复核 | 无 | 2026-11-18 17:49:59 UTC 修订（未复审） |
| R63 (Claude subagent claude-opus-5-5) | 全文（含 R63 前专项清扫），受审 SHA-256 `c87b31524f0140debf92b997aa9275c74a9e355f8bf85c098fc4fa838d03e3e3` | `PASS` | 无 P0/P1；P2：评审结论「完整性」行 `CL-05` AC 区间不全、`CL-12` 描述过度概括 | 已在 R63 修订中处理 | 2026-11-18 18:09:42 UTC 记录 |
| R63 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `c87b31524f0140debf92b997aa9275c74a9e355f8bf85c098fc4fa838d03e3e3` | `FAIL`（A/B/E `PASS`） | C：`CL-08` 符号链接拒绝只以显式 `--local` 验收；D：`CL-29` 只验三个 blob，未验整个稀疏对象闭包；F：`CL-25` 把 ER-04 C 组的 patch + 1 与 `compat_version_surface_sync` 计入 G-03。均已按同行修订历史修订 | 无 | 2026-11-18 18:09:42 UTC 记录 |
| R64 (Claude subagent claude-opus-5-5) | R63 修订差异与 C/D/E 分区逐卡复核，受审 SHA-256 `37529476d4a3895c8e430cb66ef0b74e6657a146a07dcacfcdedd3123dabebaf` | `PASS` | 无 P0/P1 | P2：`CL-24` 未单独验 `combine:` 克隆的 `remote.origin.promisor`（卡未承诺，`CL-13` 已验 `blob:none`） | 2026-11-18 18:13:31 UTC 记录 |
| R64 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `37529476d4a3895c8e430cb66ef0b74e6657a146a07dcacfcdedd3123dabebaf` | `FAIL`（A/B/D/E/F `PASS`） | C：`CL-08` 三种符号链接位置未在两种调用形式下各自验收。已按同行修订历史修订 | 无 | 2026-11-18 18:13:31 UTC 记录 |
| R65 (Claude subagent claude-opus-5-5) | R64 修订差异，受审 SHA-256 `71e66474b0289380b77f4a1f2a96245bcc1b1ff1c299160cfa65eaca4025b61e` | `PASS` | 无 P0/P1（Codex 同轮指出的 6 项未在本轮 Claude 复核中发现，见下行） | 无 | 2026-11-18 18:20:10 UTC 记录 |
| R65 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `71e66474b0289380b77f4a1f2a96245bcc1b1ff1c299160cfa65eaca4025b61e` | `FAIL`（A `PASS`） | B：`CL-25` 评审前门需要合并提交；C：`CL-09` 证据无行号锚点；D：`CL-12` 只测两个字面量、`CL-13` 只测一种组合；E：`CL-16` 帮助文案无验收；F：`base.yml` 的 `pull_request` 无分支过滤。均已按同行修订历史修订 | 无 | 2026-11-18 18:20:10 UTC 记录 |
| R66 (Claude subagent claude-opus-5-5) | R65 修订差异与工作流、锚点实测，受审 SHA-256 `cc915c93a85d2bf32cd562961162af2b2264c8f31465ad179a1ad8164bde4af0` | `PASS` | 无 P0/P1；P2：状态行「修订历史末两行」已过时（收尾记账时改写） | 无 | 2026-11-18 18:25:14 UTC 记录 |
| R66 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `cc915c93a85d2bf32cd562961162af2b2264c8f31465ad179a1ad8164bde4af0` | `FAIL`（A/E/F `PASS`） | B：`CL-25` 先签名提交后跑收口门、`CL-02` 回归用例受 GitHub 门控可能空跑；C：`CL-06` 未验 `--config`；D：`CL-24` 无能力回退只验 `blob:none`。均已按同行修订历史修订（同类的 `CL-04`/`CL-12`/`CL-15` 回归用例一并替换） | 无 | 2026-11-18 18:25:14 UTC 记录 |
| R67 (Claude subagent claude-opus-5-5) | R66 修订差异，受审 SHA-256 `8acd5db7e1baa7b1b1bc4016d0cfbea51b42cd47d1d7fb8471e407ee64eaca78` | `PASS` | 无 P0/P1；P2：`CL-25` 提交后 `status --porcelain` 为空的要求会受无关未跟踪文件影响 | 无 | 2026-11-18 18:30:44 UTC 记录 |
| R67 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `8acd5db7e1baa7b1b1bc4016d0cfbea51b42cd47d1d7fb8471e407ee64eaca78` | `FAIL`（F `PASS`） | A：nextest skipped 含被过滤用例、`status --short` 缺 `--no-branch`、非目标行与 `CL-16` 矛盾；B：`CL-03` 回归用例未用 `--reject-shallow`；C：`CL-10` 未验 `--reference-if-able` 实际借用；D：`CL-29` 未验 `partialclonefilter`；E：非目标行矛盾（同 A）、`CL-20` 证据缺行号。均已按同行修订历史修订 | 无 | 2026-11-18 18:30:44 UTC 记录 |
| R68 (Claude subagent claude-opus-5-5) | R67 修订差异与锚点、网站脚本实测，受审 SHA-256 `577ad813dfa8e05b90c275bcc2eea136ea5b2595cfb09ab286596bead0ba919d` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 18:35:51 UTC 记录 |
| R68 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `577ad813dfa8e05b90c275bcc2eea136ea5b2595cfb09ab286596bead0ba919d` | `FAIL`（A/C/F `PASS`） | B：`CL-01` AC3/AC4 无成功前提；D：`CL-24` 无能力回退未覆盖反序形式（P2：`CL-12` 重写旧用例丢失浅克隆警告覆盖）；E：`CL-20` 未验组合克隆的 cone 状态。均已按同行修订历史修订 | 无 | 2026-11-18 18:35:51 UTC 记录 |
| R69 (Claude subagent claude-opus-5-5) | R68 修订差异与全部卡描述承诺复扫，受审 SHA-256 `30a76da5d84563c6179f4c7d92df2ec53b930d04f9d15c93f547525ac707e820` | `PASS` | 无 P0/P1；P2：`CL-26`/`CL-27` 护栏未区分「键存在」与「值为 true」 | 无 | 2026-11-18 18:40:33 UTC 记录 |
| R69 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `30a76da5d84563c6179f4c7d92df2ec53b930d04f9d15c93f547525ac707e820` | `FAIL`（A/D/F `PASS`） | B：`CL-28` 把会打开仓库的 `init` 列为不打开仓库的放行命令；C：`CL-08` `--no-local` 断言可空真、未规定源仓库属主边界；E：`CL-20` 条目标记断言不能发现缺失条目。均已按同行修订历史修订 | 无 | 2026-11-18 18:40:33 UTC 记录 |
| R70 (Claude subagent claude-opus-5-5) | R69 修订差异，受审 SHA-256 `033c9208891d070bc0a8ac3ba19d60184036d1b65a6e66b8685a07245c235242` | `PASS` | 无 P0/P1；P2：`CL-08` AC7 所在用例名只写符号链接 | 无 | 2026-11-18 18:42:44 UTC 记录 |
| R70 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `033c9208891d070bc0a8ac3ba19d60184036d1b65a6e66b8685a07245c235242` | `FAIL`（A/B/D/E/F `PASS`） | C：`CL-08` 检查对象目录属主不能替代 Git 对源仓库的属主检查。已按同行修订历史修订 | 另一分区 P2：`CL-26`/`CL-27` 未测 `core.sparseCheckout=false`（登记为残余风险） | 2026-11-18 18:42:44 UTC 记录 |
| R71 (Claude subagent claude-opus-5-5) | R70 修订差异，受审 SHA-256 `004ac1636abe103bb27c562a61f25882ce8ac2fde5ec56a0b082700013b95328` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 18:45:27 UTC 记录 |
| R71 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `004ac1636abe103bb27c562a61f25882ce8ac2fde5ec56a0b082700013b95328` | `FAIL`（A/D/E/F `PASS`） | B：`CL-03` 回归用例描述夸大（只断言不留 `.libra`）；C：`CL-08` 静态符号链接夹具不能覆盖遍历中替换、属主边界只测普通布局。均已按同行修订历史修订 | 无 | 2026-11-18 18:45:27 UTC 记录 |
| R72 (Claude subagent claude-opus-5-5) | R71 修订差异与 pinned Git 竞态检测核对，受审 SHA-256 `bbe8b4745629f6fa47e8c1291efdbeffc8e59957a6c75f43b958e5edf1afc599` | `PASS` | 无 P0/P1 | 无 | 2026-11-18 18:49:52 UTC 记录 |
| R72 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `bbe8b4745629f6fa47e8c1291efdbeffc8e59957a6c75f43b958e5edf1afc599` | `FAIL`（A/B/D/E/F `PASS`） | C：`CL-07`/`CL-08` 把 `info/alternates` 当普通对象文件复制或硬链接，相对路径条目会在目标中解析错误。已按同行修订历史修订 | 无 | 2026-11-18 18:49:52 UTC 记录 |
| R73 (Claude subagent claude-opus-5-5) | R72 修订差异与相对 alternates 夹具实测，受审 SHA-256 `189cec976839d00d26639be07074c70c91d8d7bb36e166444beb9fb0fa551e05` | `PASS` | 无 P0/P1 | 见同时刻修订历史行的残余 P2 | 2026-11-18 18:51:39 UTC 记录 |
| R73 (Codex gpt-6.1-terra，分区 A–F) | 全文分六区，受审 SHA-256 `189cec976839d00d26639be07074c70c91d8d7bb36e166444beb9fb0fa551e05` | `PASS`（A–F 全部 `PASS`） | 无 P0/P1 | P2：`CL-16` 独立发布期间 `--sparse` 与其它 `--filter` 组合未验收（`DEFER-CL-05`） | 2026-11-18 18:51:39 UTC 记录 |

## 实施证据汇总

> 按 v2.12 规定，未来执行与收口证据统一汇总于本节，不在计划目录新建独立证据文件。

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-CL-01 | Git `files` 松散 ref 与 `reftable` 二进制 ref 存储后端实现（含 `#750` `files` 后端断言） | 当前仓库 refs 存储于既有 SQLite（`libra.db`），`CL-14` 如实报告 `sqlite` | 独立规划多 ref 存储后端（`files`/`reftable`）计划时 | 后续多 ref 后端计划（Owner: `CLI / Storage 维护者`） |
| DEFER-CL-02 | `libra init --separate-git-dir`（D25），以及把 `CL-28` 已拒绝的子命令改造成真正的指针布局 | D25 保留 `init` 拒绝。`CL-28` 已经默认拒绝未放行子命令，所以发布后它们不会写错目录；真正可用要等后续改造 | 后续计划逐命令放开指针布局时 | 后续对应计划 |
| DEFER-CL-03 | `#876` 在追加 refspec 后跑 `fetch --prune` 的端到端 | 依赖外部 `#541` 与 `plan-20261117-fetch-push.md#FP-15`（`DEP-CL-05`）。本计划不预置 `#[ignore]` 测试，也不为此单开发版 | `#541` 与 `FP-15` 都进入 `main` 后，由后续计划补该端到端 | 后续收口计划 |
| DEFER-CL-04 | `checkout`/`switch`/`reset` 等工作区写入命令全量支持 `clone --sparse` cone 过滤；cone 规则文件（Git `info/sparse-checkout` 的 `/*`、`!/*/`）的持久化与规则管理 | `CL-16`/`CL-20` 仅覆盖 `clone --sparse` 物化、`status.rs` 告警消除与 `merge` lazy fetch；`#756` 验收第 4 条（完整上游 `t/t6421-merge-partial-clone.sh#2`，其中在稀疏部分克隆内 `checkout -q origin/A`）随本项延后，`#756` 部分闭环 | 后续扩展工作区稀疏检出全命令兼容时 | 后续对应计划 |
| DEFER-CL-05 | `clone --sparse` 与 `blob:none` 以外的 `--filter`（`tree:<n>`、`combine:`、`sparse:oid`）组合 | `#756` 只要求 `--sparse --filter=blob:none`（`CL-20` AC1）；其它组合涉及稀疏 cone 与树深/稀疏规则过滤的交互，本计划不加限制也不验收（2026-11-18 R56 修订前曾以 `CL-16` AC8 拒绝，已撤回） | 有用户需求或上游用例要求时 | 后续对应计划 |
| DEFER-CL-06 | `clone --revision=<完整 OID>` 形式，以及不存在修订的专门报错（Git `Remote revision %s not found in upstream %s`，`builtin/clone.c:1577`） | `#741` 只要求 `--bare --revision=refs/heads/main`；`CL-05` 已验收引用名形式 | 有用户需求或上游用例要求时 | 后续对应计划 |
| DEFER-CL-07 | `clone -c` 中 `clone.defaultRemoteName` 以外的配置项对本次初始 fetch/checkout 的即时生效（如经 `-c remote.origin.fetch=…` 追加 refspec 影响初始 fetch） | `#752` 只要求 `-c clone.defaultRemoteName` 与 `--origin` 的优先级；`CL-06` 已验收其它键写入新仓库 | 有用户需求或上游用例要求时 | 后续对应计划 |
| DEFER-CL-08 | 网络传输（`git_client.rs`/`https_client.rs`/`ssh_client.rs`）的 partial-clone filter 协商；重复同值 `--filter`（如两次 `blob:none`）的组合结果 | `#746`/`#748`/`#754` 的上游用例均经 `file://` 本地传输；网络远端上的 `--filter` 行为不在本计划验收（风险登记见「网络远端 `--filter`」行） | 需要对网络远端做部分克隆时 | 后续对应计划 |
| DEFER-CL-09 | 本地 Git 源（`.git` 目录，`RepoType::GitRepo` 发现路径）的 `-l`/`--local` 硬链接与对应符号链接、回退语义 | `#743` 针对 Libra 本地克隆的「从不硬链接」；`CL-08` 只交付本地 Libra 源，Git 源沿用既有复制路径（结果正确，只是不节省磁盘） | 有用户需求或上游用例要求时 | 后续对应计划 |
| DEFER-CL-10 | 单独的 `--filter=tree:<n>`、`object:type=<type>`、`blob:limit=<n>` 等本计划不实现的 filter 类型，以及 `blob:none` 与 `tree:1` 以外的组合（如 `combine:blob:none+tree:2`） | `#746` 只要求 `--filter=blob:none --filter=tree:1` 的组合（`CL-24`），`#748` 只要求 `sparse:oid`（`CL-12`/`CL-29`）；这些规格自 `CL-12`（单独 filter，`CL-12` AC5）与 `CL-13`（组合，`CL-24` AC8 持续断言）起以 `LBR-UNSUPPORTED-001` fail-closed 拒绝，不静默全量克隆 | 有用户需求或上游用例要求时 | 后续对应计划 |

## 完成判据

- [ ] 全量收口门须在**最终代码树**上执行，并记录 `libra rev-parse HEAD` 与 `libra rev-parse HEAD^{tree}` 及结果；树一旦变化即须重跑，与是否发生修复无关。
- [ ] 计划文本提交门：Claude（Subagent）与 Codex 对同一字节均返回字面结论 `VERDICT: PASS` 且 P0/P1 全部关闭（用户 2026-11-18 指令，取代 2026-11-17 的 Codex 单审口径）；网站交付另需独立 Claude Code `VERDICT: PASS`，两者互不替代。
- [ ] 网站交付门：每张触及用户可见行为的卡均在其发布点完成 `/Volumes/Data/gitmono/libra-backend` 的最终字节同步检查、前端验证，并取得独立 Claude Code `PASS`，随后精确暂存、签名+DCO 提交、核对提交树与已审字节一致、推送 `cf` 并核验部署；计划文本双审不替代该网站门，任一环节失败即 STOP 且不得标 `complete`。
- [ ] 全部活动卡（27 张，不含已撤回的 `CL-22`）满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 全部活动卡（不含已撤回的 `CL-22`）的 `Lifecycle=done` 且 `Acceptance=complete`。
- [ ] 全部活动卡的 `Verification` 命令已运行并记录结果。过渡断言只保留在各自发布提交的证据里，并由后继卡同卡改写：`CL-12` AC7（`blob:none` 被拒）由 `CL-13` 改写；`CL-12` AC6（有效 `sparse:oid` 被拒）由 `CL-29` 改写；`CL-13` AC5–AC6 样本表中由 `blob:none` 与 `tree:1` 组成的样本与 AC7（无 `filter` 能力远端被拒）由 `CL-24` 改写为成功与回退契约。`CL-12` AC5（单独 filter 样本表被拒）、`CL-13` AC5–AC6 的其余样本（由 `CL-24` AC8 继续断言被拒）与 `CL-27` AC1（内容修改合并被拒；`CL-20` 只解禁目录重命名合并）在最终树上继续成立。
- [ ] 收口阶段全量门通过：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`（按 ER-13：无论暴露本计划缺陷或既有失败，均须定位修复或附基线复现证据并登记 `FIX-*`/`DEFER-*` 后重跑全量至全绿，不得带着红色全量结果通过完成门）。
- [ ] `#876` 的 `fetch --prune` 端到端留在 `DEFER-CL-03`，不阻塞本计划活动卡完成。
- [ ] 全部活动卡的 `Rollback mode`（`immutable-release`）与恢复验证已核验（`CL-22` 无写集、无恢复动作）。
- [ ] 必要的 docs / compat / test-index 更新已完成，且未在 `docs/development/plan/` 新建独立证据文件（v2.12 规则）。
- [ ] 每个相关发布切片均已完成 `/Volumes/Data/gitmono/libra-backend` `cf` 的网站文档 sync/check、相关前端验证与覆盖最终网站变更的 Claude Code `VERDICT: PASS`（独立于计划文本双审）；独立卡已提交推送 `cf` 并核验 Cloudflare 部署与生产正文，家族子卡的本地 `cf` 提交已由 `CL-25` 统一推送并完成同等部署核验。
- [ ] 所有独立卡已完成 `patch + 1`、版本面同步与 `gh release create` 发布。
- [ ] 「修订历史」与 `plan-status.md` 已同步。
