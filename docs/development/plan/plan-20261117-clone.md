# Clone Git 兼容性修复计划（2026-11-17）

**模板版本:** `v2.16`。本计划不在 v2.16 具名 `code-only` 范围内，按普通发布合同执行：独立 `implementation` 卡与家族发布点执行完整 C 组（含 `patch + 1`、`compat_version_surface_sync`、`Cargo.lock`、`gh release create`）与适用 D 组。家族子卡 `CL-04`、`CL-23`、`CL-28` 只本地提交，不单独推送，C/D 继承 `CL-25`。

**状态:** Codex R37-A、R41-B、R47-C 分区及跨计划依赖修订复审 PASS，计划可提交。最近修订 `2026-11-17 12:27:28 UTC`。全部任务卡 `Lifecycle=pending`、`Acceptance` 为空。本文只规划，不宣称实现完成；落地时每张卡必须先按 ER-02 刷新源码锚点。

## 文档职责

本文解决 `libra clone`（及关联 `rev-parse --show-ref-storage-format`）在 CLI 标志解析（`--progress` 布尔语义、`--no-reject-shallow`、`-n`、`--separate-git-dir`、`--revision`、`--no-hardlinks`、`-c <key=value>`、`--ref-storage-format`、`--sparse`）、本地硬链接与 `<path>.git` 回退、部分克隆 `--filter`（多值 `combine:`、`sparse:oid`、无效 sparse filter、promisor 对象读取与按需 lazy fetch 与进度）、`--reference`/`--dissociate` alternates 借用、`--single-branch` 游离 HEAD 分支创建、以及默认 `clone` 持久化 `remote.<name>.fetch` 中与 Git 不一致的 17 个已确认缺口，目标是实现 Git 兼容行为、完备测试覆盖与四端文档同步。

### 适用范围

- `libra clone`（`src/command/clone.rs`）、关联的 `src/command/rev_parse.rs`（`--show-ref-storage-format`）、`src/command/fetch.rs`（`--revision` / `--filter` / `--reference` 传输）、`src/internal/protocol/local_client.rs`、对象读取 promisor 回退路径与 `src/cli.rs`。
- 关联测试：`tests/command/clone_test.rs`、`tests/command/clone_cli_test.rs`、`tests/command/rev_parse_test.rs`（均通过 `--test command_test` 运行）、`tests/compat/clone_shallow_integrity_test.rs`（target `compat_clone_shallow_integrity`）、`tests/INDEX.md`。
- 关联文档：`docs/commands/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.md`、`docs/commands/zh-CN/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.md`、`docs/development/commands/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.md`、`COMPATIBILITY.md`、`docs/error-codes.md` 及 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/{clone,rev-parse,status,log,cat-file,branch,commit,add,checkout,switch,reset,restore,merge,pull,rebase,cherry-pick,stash}.en.md`（写前按 ER-06a 核验 `.libra` 与 `.git` 及 `cf` 分支）。

### 非目标

- 不重构 `libra fetch` / `libra push` 的独立参数解析（由协调者排期的 `fetch-push` 计划承接）。
- 不修改 SQLite schema 或迁移 ledger（不触发 GC-13 迁移）。
- 不在本计划内实现 Git `reftable` 后端存储（`--ref-storage-format` 仅映射已实现的 SQLite ref 格式并拒绝未知格式）。
- 不在本计划内新增独立的 `libra sparse-checkout` 子命令（`--sparse` 通过既有的 `sparse_view.rs` 稀疏视图与 cone 检出实现）。

### 成功定义

- 覆盖全部 17 个目标 Issue（`#737, #738, #739, #740, #741, #742, #743, #744, #746, #748, #750, #751, #752, #753, #754, #756, #876`），其中 `#750` 只交付 CLI 校验与如实报告 `sqlite`（`files`/`reftable` 在 `DEFER-CL-01`），`#876` 只交付 `remote.<name>.fetch` 持久化（`fetch --prune` 端到端在 `DEFER-CL-03`）。其余 15 个 Issue 全量闭环并附 `Git 2.56.0@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd` 对照验证。
- 每张任务卡通过 ER-04 A/B/C/D 四组门禁、ER-05 Codex 单审 `VERDICT: PASS`、ER-06a 文档同步及 ER-08 发布切片。
- 全部任务卡完成后通过 ER-13 全量收口门（`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`）。

## 使用规则

- 本批次并行起草的多份计划由协调者统一串行登记至 [`plan-status.md`](plan-status.md) 与 `plan-long.md`；执行期推进任何任务卡时，同一变更必须同步更新 `plan-status.md` 的「计划一览」行（状态、版本或 commit、`YYYY-MM-DD HH:MM:SS UTC`）。
- 不得在 `docs/development/plan/` 下新建证据目录或证据文件。验收摘要写入本文「实施证据汇总」。
- 状态推进与「修订历史」统一使用 `YYYY-MM-DD HH:MM:SS UTC` 时间格式。
- 本计划继承 [plan-template.md](plan-template.md) 的 `GC-01..13`、`ER-01..14`、`G-01..11`。

## 事实基线

> 所有行号和源码锚点核对于 `2026-11-17 03:20:00 UTC`，开工当天必须按 ER-02 刷新。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口（CLI） | `Commands::Clone(CloneArgs)` 在 `cli.rs` 注册并路由到 `execute_safe`；全局 `--progress <mode>` 占用布尔 `--progress`（仅 `index-pack` 有 `rewrite_index_pack_progress_args`） | `src/cli.rs:292-302,351,1121,1318,3819` |
| `CloneArgs` 标志缺口 | 缺 `--no-reject-shallow`、`-n`、`--separate-git-dir`、`--revision`、`--no-hardlinks`、`--ref-storage-format`、`-c`、`--sparse`；`--filter` 为 `Option<String>` 且为 no-op | `src/command/clone.rs:64-236` |
| 本地克隆与 Alternates | `-l`/`--local` 总是复制对象而不硬链接；`--reference` 与 `--dissociate` 被忽略；`<path>.git` 回退解析缺失；无 promisor 支持 | `src/command/clone.rs:124-182,250-269` |
| `single-branch` 与 `remote.<name>.fetch` | `--single-branch` 从游离 HEAD 克隆错误创建 `refs/remotes/origin/main`；`remote add` 已写 `+refs/heads/*:refs/remotes/<name>/*`（`remote.rs:822-834`），但默认 `clone` 仅在 single-branch/mirror 时写 | `src/command/clone.rs:76-87`；`src/command/remote.rs:822-834` |
| `rev-parse` 与 `init` 格式 | `RevParseArgs` 缺 `--show-ref-storage-format`；`init --ref-format` 仅接受 `strict`/`filesystem`（ADR-IN-02） | `src/command/rev_parse.rs:59`；`src/command/init.rs:374,1674-1686` |
| 稀疏视图边界 | 历史决策 D10 曾拒绝 `sparse-checkout`，当前仅存在 `sparse_view.rs`（`COMPATIBILITY.md:166,172,375`） | `COMPATIBILITY.md:166,172,375`；`src/command/clone.rs:220` |
| 测试与文档 | `clone_test` 与 `clone_cli_test` 挂在 `command_test` target 下；compat target 为 `compat_clone_shallow_integrity` | `tests/command/mod.rs:1024-1025`；`tests/INDEX.md:80`；`docs/development/commands/clone.md` |
| 外部参照 | Git 2.56.0 基准行为 | `git/git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（核对日期 `2026-11-17`） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-CL-01 | `clone` CLI 缺 `--progress` 布尔、`-n`、`--no-reject-shallow`、`--separate-git-dir`、`--revision`、`-c`（含 `clone.defaultRemoteName`）、`--ref-storage-format`、`--sparse` | 常见 Git clone 脚本与测试在 CLI 解析层失败 | `#737, #738, #739, #740, #741, #750, #752, #756`；`src/cli.rs:302`；`src/command/clone.rs:64-236` | CL-01..CL-06, CL-14, CL-16 |
| GAP-CL-02 | `-l/--local` 不硬链接、`--no-hardlinks` 缺失、`<path>.git`（含 `file://`）回退解析缺失、`--reference`/`--dissociate` 被忽略 | 本地克隆性能与对象借用/独立语义缺失 | `#742, #743, #744, #751`；`src/command/clone.rs:124-182` | CL-08, CL-07, CL-09, CL-10, CL-18 |
| GAP-CL-03 | `--filter` 仅单值且忽略过滤（无 `combine:`、无 `sparse:oid`、无无效 sparse 报错、无 promisor/lazy fetch 与 `Receiving objects` 进度） | 部分克隆与错误过滤校验失效 | `#746, #748, #754`；`src/command/clone.rs:191-198,269` | CL-12, CL-13, CL-24, CL-29, CL-19, CL-21 |
| GAP-CL-04 | `--single-branch` 游离 HEAD 创建错误分支、默认 `clone` 未写入 `remote.<name>.fetch` | 游离 HEAD 克隆与后续 `fetch --prune` 越界修剪 | `#753, #876`；`src/command/clone.rs:76`；`src/command/remote.rs:822-834` | CL-15, CL-17 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-20261117-plumbing-misc.md` | 兄弟计划：`PM-06`（`#947`）先于 `CL-14`（`#750`）合入 `src/command/rev_parse.rs` | 登记单向依赖 `DEP-CL-02`（`PM-06` 先于 `CL-14`），无循环 |
| `plan-20261117-commit-signing.md` | 兄弟计划：同样涉及 `src/command/clone.rs` 与 `tests/command/clone_cli_test.rs` | 登记 `DEP-CL-03`，按 ER-12 串行合入 |
| `plan-20261117-fetch-push.md` | 兄弟计划：同样涉及 `src/command/fetch.rs` 与 `COMPATIBILITY.md` | 登记 `DEP-CL-04`，按 ER-12 串行合入 |
| `plan-long.md` | 长期兼容性路线图 | 由协调者在合入时统一登记日期索引 |
| `docs/development/plan/plan-template.md` | 模板规范 v2.16 | 全文严格遵守 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 17 个实测 Git 兼容缺陷均有明确复现路径 | 按单一行为轴拆分为 27 张活动任务卡（含 `CL-18..CL-29`，`CL-22` 已撤回，其中 `CL-25` 为 `REL-CL-01` 家族发布点，`CL-28` 为指针布局写命令拒绝子卡，`CL-29` 为 `sparse:oid` 下游契约卡，原 `CL-11` 已并入 `CL-13`；`CL-` 前缀作用域仅限本计划，与 `issues/474.md` 无关） |
| 可行性 | 每卡聚焦单一行为轴，锚点与测试命令均可直接执行 | 修正 `CL-02`、`CL-16` 不存在测试过滤器并重写旧 no-op 测试 |
| 任务卡粒度 | 所有卡均含公开行为变更，按 G-04 标为 `M`；`landing` 仅计生产目录 | 逐条按独立谓词核算 `AC`/`VER`（≤ 8） |
| 依赖与顺序 | 全计划严格串行，`CL-08` 先于 `CL-07`；`DEP-CL-02/03/04` 单向无环 | DAG 无环，符合 G-10/ER-12 |
| 完整性 | 补齐 `--progress` 重定向、`--bare --revision`、`file://<path>.git`、`clone.defaultRemoteName` 等 AC | 全部纳入卡内与 `COMPATIBILITY.md` |
| 安全性 | `--separate-git-dir`、`--reference`、`-c`、`<path>.git` 校验路径边界与不泄露凭据 | 继承 GC-07/GC-09 |
| 功能正确性 | 严格对齐 `Git 2.56.0@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd` | 新增 `ADR-CL-03`/`ADR-CL-04` 解决 D10 与 `ref-format` 边界 |
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
| 2026-11-17 12:27:28 UTC | Codex R31 FAIL | 将全部任务卡、文档影响和网站写集中的 `../libra-backend` 统一替换为唯一网站工作区 `/Volumes/Sea/libra-backend`，避免写入错误 checkout；恢复独立于计划 Codex 单审的网站 Claude Code PASS 门 | N/A | 全部网站写集与文档影响、Review log |
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
- **Decision:** 将部分克隆拆分为 6 张递进单轴卡并按 `CL-19 -> CL-12 -> CL-13 -> CL-24 -> CL-29 -> CL-21` 顺序推进，确保每个独立发布版本自洽：`CL-19`（对象读取层缺失时触发 promisor lazy fetch，通过预置 promisor 夹具自洽）、`CL-12`（无效 `sparse:` 精确校验与目录清理）、`CL-13`（多 `--filter` `Vec` 解析、`--filter=blob:none` 去重协商、`remote.<remote_name>.promisor=true`/`extensions.partialClone=<remote_name>` 写入，并在 `CL-24` 落地前对 `combine:*` 与多种不同 `--filter` 组合 fail-closed 拒绝，避免出现“接受 `--filter` 却静默全量克隆”的半成品）、`CL-24`（`combine:blob:none+tree:1` 树深过滤传输与远端无能力安全回退）、`CL-29`（`sparse:oid` 过滤与预期缺失对象边界，供 `PM-03` 的 `t5616#33`/`TC-0625` 消费）、`CL-21`（`Receiving objects` stderr 进度与真实 `clone -> cat-file` 端到端 lazy fetch 收口）。
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
- **Consequences:** `CL-14` 写集仅含 `clone.rs` 与 `rev_parse.rs`，不影响 `init.rs` 且存储语义诚实。
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
- **GC-CL-01:** 所有 CLI 参数、进度输出、错误码与仓库布局均以 `Git 2.56.0@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（核对日期 `2026-11-17`）为基准对齐，不破坏 `--json` 契约。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) `ER-01..14`。
- 每张 `implementation` 卡执行阶段按 ER-13 跑 A 组 focused 测试 + `cargo +nightly fmt --all --check` + `cargo clippy --all-targets --all-features -- -D warnings`；命中 `T-1`..`T-6`（如 `CL-01`、`CL-08`、`CL-13`、`CL-19`）时跑全量 nextest。
- 独立发布卡和家族发布点推送后必须取得 D 组远端门证据。家族子卡 `CL-04`、`CL-23`、`CL-28` 不单独推送，继承 `CL-25` 的 D 组证据：
  1. `.github/workflows/base.yml`（jobs: `format`, `clippy`, `redundancy`, `test`；`pull_request`/`push` `main`, `release/**`；PR/main head SHA 全绿）；
  2. `.github/workflows/codeql.yml`（job: `analyze`；`pull_request` `main`，`push` `main`, `release/**`；PR/main head SHA 全绿）；
  3. `.github/workflows/release.yml`（jobs: `build-and-upload`, `upload-install-scripts`, `update-homebrew-tap`, `verify-homebrew-formula`, `request-stable-manifest`；`push` `v*` tag 由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发；全部 job 与 R2 上传全绿）。
- **网站文档交付默认（ER-06a，适用于本计划所有命中命令文档的卡）：** 网站目标固定为 `/Volumes/Sea/libra-backend` 的 `cf` 生产分支，不得切换其它 checkout 或另建分支/PR/worktree；写前识别仓库类型、当前分支与脏状态并保留无关改动。每张相关卡必须在同一交付内运行实际网站文档 sync/check、相关前端 typecheck/test/build，并取得覆盖最终网站文档变更的 Claude Code `VERDICT: PASS`（该网站交付门独立于计划 Codex 单审）。独立卡随后精确提交并推送 `cf`，记录后端 commit、Cloudflare 部署 run 与生产 URL/正文核验；家族子卡只完成本地网站变更、验证与精确 `cf` 提交，由 `CL-25` 在家族统一发布后串行推送并核验部署，避免提前公开未集成能力。任一门失败均保持该卡或发布点未验收，不得以 Libra 主仓 release 成功替代网站交付。
- 全部任务卡完成后，收口阶段必须执行一次完整 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

## 实施顺序

全串行 DAG（匹配各卡 `Dependencies`）：
`CL-01 -> CL-02 -> CL-03 -> CL-04 -> CL-23 -> CL-28 -> CL-25 -> CL-05 -> CL-06 -> CL-08 -> CL-07 -> CL-09 -> CL-10 -> CL-18 -> CL-19 -> CL-12 -> CL-13 -> CL-24 -> CL-29 -> CL-21 -> CL-14 -> CL-15 -> CL-26 -> CL-27 -> CL-16 -> CL-20 -> CL-17`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-CL-01 | incoming | 上游基线 | `libra` `main` 当前 checkout | 本计划协调者 | `cargo check` 通过且基线锚点可解析 | `src/command/clone.rs:1` | 开工前按 ER-02 刷新锚点 |
| DEP-CL-02 | incoming | 跨计划前置 | `plan-20261117-plumbing-misc.md#PM-06`（`#947`） | 仓库发布协调者 | `PM-06`（`rev-parse --show-object-format`）先合入 `src/command/rev_parse.rs` 后再合入 `CL-14`（单向无环） | `src/command/rev_parse.rs:59` | 若 `PM-06` 延迟，`CL-14` 等待其合入后 rebase |
| DEP-CL-03 | outgoing | 跨计划协调 | `plan-20261117-commit-signing.md#CS-02,#CS-02a,#CS-15b,#CS-21a`（`DEP-CS-03`） | `commit-signing` 计划协调者 | 仅下列活动卡先合入：`CL-01..CL-10`、`CL-12..CL-16`、`CL-18..CL-19`、`CL-21`、`CL-23..CL-29`（含 `CL-25` 家族发布点；明确排除已撤回的 `CL-11`/`CL-22`，以及为避免经 `CS-02a` 回环而排除的 `CL-20` 与依赖它的 `CL-17`）。`DEP-CS-03` 必须逐字镜像该集合，不以区间展开撤回卡 | `docs/development/plan/plan-20261117-commit-signing.md` | 不阻塞本计划 |
| DEP-CL-04 | incoming | 跨计划顺序前置 | `plan-20261117-fetch-push.md#FP-07,#FP-15` | 仓库发布协调者 | `FP-07` 与 `FP-15` 先合入 `src/command/fetch.rs` 与 `COMPATIBILITY.md`，随后本计划 `CL-05/10/13/19/20/21/24/29` 按顺序 rebase 并合入（明确单向顺序，禁止并发） | `docs/development/plan/plan-20261117-fetch-push.md` | 若 `FP-07`/`FP-15` 未合入，本计划涉及 `fetch.rs` 的卡等待其窗口完成 |
| DEP-CL-05 | incoming | 外部闭环前置 | 外部 issue `#541` 与 `plan-20261117-fetch-push.md#FP-15` | `fetch-push` 与 `#541` 协调者 | 只门控 `DEFER-CL-03` 的 `fetch --prune` 端到端。`CL-17` 不依赖本行即可完成配置持久化 | `#876` | `#541` 与 `FP-15` 都进入 `main` 后才重启 `DEFER-CL-03` |
| DEP-CL-06 | outgoing | 跨计划协调 | `plan-20261117-{checkout-switch-restore,global-init-infra,plumbing-misc}.md` | 仓库发布协调者 | `CL-01`、`CL-28`、`CL-26`、`CL-27`、`CL-20` 修改 `src/cli.rs` 后按 ER-12 串行交付给后续修改 `src/cli.rs` 的计划 | `src/cli.rs:1` | 禁止并发修改 `src/cli.rs` |
| DEP-CL-07 | outgoing | 跨计划协调 | `plan-20261117-merge-pull.md#MP-17` | 仓库发布协调者 | `CL-20` 先合入 `merge.rs`、`rename_merge.rs`、`tree_merge.rs`。`CL-20` 开工前只确认没有在途写者，不等待 `MP-17`。`MP-17` 在 `CL-20` 合入之后再改这些文件 | `docs/development/plan/plan-20261117-merge-pull.md` | 禁止并发修改这些文件 |
| DEP-CL-08 | outgoing | 跨计划协调 | `plan-20261117-plumbing-misc.md#PM-14` | `plumbing-misc` 协调者 | `CL-19` 共享 `src/command/cat_file.rs` 按 ER-12 串行合入 | `src/command/cat_file.rs:1` | 禁止并发修改 `cat_file.rs` |
| DEP-CL-09 | incoming | 跨计划协调 | `plan-20261117-global-init-infra.md` | 仓库发布协调者 | `CL-14` 共享 `src/command/rev_parse.rs` 按 ER-12 串行合入 | `src/command/rev_parse.rs:59` | 禁止并发修改 `rev_parse.rs` |
| DEP-CL-10 | incoming | 跨计划协调 | `plan-20261117-{global-init-infra,branch,describe,merge-pull}.md` | 仓库发布协调者 | `CL-23` 共享 `src/utils/util.rs` 按 ER-12 串行合入 | `src/utils/util.rs:278,314,791,1269,1464` | 禁止并发修改 `src/utils/util.rs` |
| DEP-CL-11 | incoming | 跨计划协调 | `plan-20261117-{checkout-switch-restore,global-init-infra}.md` | 仓库发布协调者 | `CL-16` 共享 `src/command/status.rs` 按 ER-12 串行合入 | `src/command/status.rs:563` | 禁止并发修改 `src/command/status.rs` |
| DEP-CL-12 | incoming | 跨计划文档写集 | 其它计划里会改命令文档的卡 | 仓库发布协调者 | `CL-28` 改写集里的每份命令文档前，核验 `plan-status.md` 没有该文件的在途写者 | `docs/commands/checkout.md:1` | 有在途写者时阻塞，禁止并发改同一命令文档 |

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
- **D-group remote checks 默认:** `base.yml`（jobs: `format`, `clippy`, `redundancy`, `test`；`pull_request`/`push` `main`, `release/**`）、`codeql.yml`（job: `analyze`；`pull_request` `main`，`push` `main`, `release/**`）、`release.yml`（jobs: `build-and-upload`, `upload-install-scripts`, `update-homebrew-tap`, `verify-homebrew-formula`, `request-stable-manifest`；`push` `v*` tag 由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发，全部通过）
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
| CL-01 | implementation | clone --progress 布尔语义 | immutable-release | yes | yes | 4/8 | 2/8 | 2/2 | M | none | 串行链起点 | independent | N/A | N/A |
| CL-02 | implementation | clone -n 短标志支持 | immutable-release | yes | yes | 3/8 | 2/8 | 1/1 | M | CL-01 | 串行于 CL-01 | independent | N/A | N/A |
| CL-03 | implementation | clone --no-reject-shallow | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | CL-02 | 串行于 CL-02 | independent | N/A | N/A |
| CL-04 | implementation | clone --separate-git-dir 与 --bare 互斥 | immutable-release | yes | yes | 4/8 | 2/8 | 1/1 | M | CL-03 | 串行于 CL-03 | family child | N/A | N/A |
| CL-23 | implementation | pointer 发现与四个基本命令 | immutable-release | yes | yes | 6/8 | 2/8 | 2/2 | M | CL-04,DEP-CL-10 | 串行于 CL-04 | family child | CL-04 | N/A |
| CL-28 | implementation | 指针布局默认拒绝未放行命令 | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | CL-23,DEP-CL-12 | 串行于 CL-23 | family child | CL-23 | N/A |
| CL-25 | release | REL-CL-01 指针布局家族发布点 | immutable-release | yes | yes | 3/12 | 1/12 | 0/0 | S | CL-04,CL-23,CL-28 | 串行于 CL-28 | family release point | N/A | N/A |
| CL-05 | implementation | clone --revision 单修订克隆 | immutable-release | yes | yes | 4/8 | 2/8 | 2/3 | M | CL-25,DEP-CL-04 | 串行于 CL-25 | independent | N/A | N/A |
| CL-06 | implementation | clone -c &lt;key=value&gt; 配置覆盖 | immutable-release | yes | yes | 7/8 | 2/8 | 1/1 | M | CL-05 | 串行于 CL-05 | independent | N/A | N/A |
| CL-08 | implementation | clone -l/--local 硬链接本地对象 | immutable-release | yes | yes | 5/8 | 4/8 | 2/2 | M | CL-06 | 串行于 CL-06 | independent | N/A | N/A |
| CL-07 | implementation | clone --no-hardlinks 标志 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | CL-08 | 串行于 CL-08 | independent | N/A | N/A |
| CL-09 | implementation | clone &lt;path&gt;.git 后缀回退 | immutable-release | yes | yes | 3/8 | 2/8 | 2/2 | M | CL-07 | 串行于 CL-07 | independent | N/A | N/A |
| CL-10 | implementation | clone --reference alternates 借用 | immutable-release | yes | yes | 5/8 | 2/8 | 2/3 | M | CL-09,DEP-CL-04 | 串行于 CL-09 | independent | N/A | N/A |
| CL-18 | implementation | clone --dissociate 解除 alternates | immutable-release | yes | yes | 3/8 | 3/8 | 1/1 | M | CL-10 | 串行于 CL-10 | independent | CL-10 | N/A |
| CL-19 | implementation | promisor 缺失对象读取 lazy fetch | immutable-release | yes | yes | 5/8 | 2/8 | 4/5 | M | CL-18,DEP-CL-04,DEP-CL-08 | 串行于 CL-18 | independent | CL-13 | N/A |
| CL-12 | implementation | clone 无效 sparse filter 拒绝 | immutable-release | yes | yes | 6/8 | 2/8 | 2/3 | M | CL-19 | 串行于 CL-19 | independent | N/A | N/A |
| CL-13 | implementation | clone --filter=blob:none 与 promisor 配置 | immutable-release | yes | yes | 8/8 | 2/8 | 2/4 | M | CL-12,DEP-CL-04 | 串行于 CL-12 | independent | N/A | N/A |
| CL-24 | implementation | clone combine:blob:none+tree:1 过滤与回退 | immutable-release | yes | yes | 5/8 | 2/8 | 2/4 | M | CL-13,DEP-CL-04 | 串行于 CL-13 | independent | CL-13 | N/A |
| CL-29 | implementation | clone sparse:oid 过滤与预期缺失边界 | immutable-release | yes | yes | 5/8 | 2/8 | 2/4 | M | CL-24,DEP-CL-04 | 串行于 CL-24 | independent | CL-24 | N/A |
| CL-21 | implementation | clone --filter 进度与端到端 lazy fetch | immutable-release | yes | yes | 3/8 | 2/8 | 2/2 | M | CL-29,DEP-CL-04 | 串行于 CL-29 | independent | CL-13 | N/A |
| CL-14 | implementation | --ref-storage-format 与 rev-parse | immutable-release | yes | yes | 4/8 | 2/8 | 1/2 | M | CL-21,DEP-CL-02,DEP-CL-09 | 串行于 CL-21 | independent | N/A | N/A |
| CL-15 | implementation | --single-branch 游离 HEAD 处理 | immutable-release | yes | yes | 3/8 | 2/8 | 1/1 | M | CL-14 | 串行于 CL-14 | independent | N/A | N/A |
| CL-26 | implementation | 稀疏工作区 checkout/switch/reset/restore 护栏 | immutable-release | yes | yes | 4/8 | 1/8 | 1/1 | M | CL-15 | 串行于 CL-15 | independent | CL-16 | N/A |
| CL-27 | implementation | 稀疏工作区 merge/pull/rebase/cherry-pick/stash 护栏 | immutable-release | yes | yes | 5/8 | 1/8 | 1/1 | M | CL-26 | 串行于 CL-26 | independent | CL-26 | N/A |
| CL-16 | implementation | clone --sparse cone 物化与 status 适配 | immutable-release | yes | yes | 8/8 | 2/8 | 3/3 | M | CL-27,DEP-CL-11 | 串行于 CL-27 | independent | N/A | N/A |
| CL-20 | implementation | clone --sparse 部分克隆与目录重命名合并 | immutable-release | yes | yes | 7/8 | 4/8 | 3/6 | M | CL-16,DEP-CL-04,DEP-CL-07 | 串行于 CL-16 | independent | CL-16 | N/A |
| CL-17 | implementation | 默认 clone 写 remote.&lt;name&gt;.fetch | immutable-release | yes | yes | 3/8 | 2/8 | 1/1 | M | CL-20 | 串行于 CL-20 | independent | N/A | N/A |
| CL-22 | superseded | 已并入 DEFER-CL-03，不得开工 | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | 无写集 | none | CL-17 | N/A |

---

### Task CL-01: 修复 `libra clone --progress` 布尔解析（含重定向 stderr 输出 `%`）（#737）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 ADR-CL-01 修复 `libra clone --progress <url>` 将 `<url>` 当成 `--progress` 参数值消费的问题，同时兼容 `clone.rs:111-112` 既有的 `--no-progress` 处理器（不将其提前改写为全局 `--progress=none` 以免绕过原有处理），并确保 `--progress` 在 stderr 重定向至非 TTY 时仍输出 `%` 进度。
**Out of scope:** 非 `clone` 子命令的全局 `--progress=<mode>` 语义（永久非目标）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#737` 全局 `--progress` 消费后续位置参数 `<url>`；仅 `index-pack` 有 `rewrite_index_pack_progress_args` | `src/cli.rs:292-302,1121,1318,1366` |

**Acceptance criteria:**
- [ ] `libra clone --progress <url> [dir]` 正确解析 `<url>` 而不消费位置参数。
- [ ] `libra clone --progress` 在 stderr 被重定向到文件/管道时仍输出含 `%` 的进度行。
- [ ] `libra clone --no-progress <url>` 抑制进度输出。
- [ ] 全局 `--progress=json` / `--progress=none` 显式赋值形式继续有效。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_boolean_progress_does_not_consume_url` `(new，含 stderr 重定向 % 进度与 --no-progress 用例)`
- [ ] `source .env.test && cargo nextest run --lib cli::tests::index_pack_progress_rewrite`

**Full-suite trigger:** `T-1: 修改 src/cli.rs 全局参数预处理`
**Dependencies:** `无`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --progress 布尔语义; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=none; writeset=串行链起点; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-02: 支持 `libra clone -n` 短标志（`--no-checkout`）（#739）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 为 `CloneArgs::no_checkout` 添加 `short = 'n'`，支持 `libra clone -n <url>` 跳过工作区检出并输出精确的单行 `Cloning into ...`。
**Out of scope:** `--no-checkout` 内部检出跳过逻辑（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#739` `CloneArgs::no_checkout` 仅有 `long = "no-checkout"`，缺少 `short = 'n'` | `src/command/clone.rs:116-117` |

**Acceptance criteria:**
- [ ] `libra clone -n <url> <dir>` 创建仓库与 refs/objects 但不检出工作区文件。
- [ ] 输出恰好包含一行 `Cloning into '<dir>'...` 诊断行。
- [ ] `libra clone --help` 显示 `-n, --no-checkout`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_short_n_skips_checkout` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_default_branch`

**Full-suite trigger:** `none`
**Dependencies:** `CL-01`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -n 短标志支持; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CL-01; writeset=串行于 CL-01; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-03: 支持 `libra clone --no-reject-shallow` 否定标志（#738）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 为 `CloneArgs` 新增 `--no-reject-shallow` 与 `--reject-shallow` 互相 `overrides_with`（last-one-wins）。
**Out of scope:** `--reject-shallow` 自身检测逻辑（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#738` `CloneArgs::reject_shallow` 缺少对应的 `--no-reject-shallow` | `src/command/clone.rs:144-145` |

**Acceptance criteria:**
- [ ] `libra clone --reject-shallow --no-reject-shallow <shallow-repo>` 成功克隆浅源仓库。
- [ ] `libra clone --no-reject-shallow --reject-shallow <shallow-repo>` 拒绝浅源仓库。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_no_reject_shallow_overrides_reject_shallow` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_reject_shallow_local_git_source`

**Full-suite trigger:** `none`
**Dependencies:** `CL-02`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --no-reject-shallow; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CL-02; writeset=串行于 CL-02; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-04: 支持 `libra clone --separate-git-dir` 并输出与 `--bare` 互斥信息（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 ADR-CL-05 为 `CloneArgs` 新增 `--separate-git-dir <dir>`，将仓库元数据目录初始化至 `<dir>` 并在 `<workdir>/.libra` 写入 `gitdir: <dir>` 指针文件（复用 `src/utils/convert.rs:242-252` 与 `src/utils/util.rs:265`），同时在 `--bare`/`--mirror` 与 `--separate-git-dir` 组合时输出 Git 兼容的互斥错误提示并清理失败新建目录。（按 G-09 拆出 `CL-23` 承接指针发现，拆出 `CL-28` 承接写命令 fail-closed。）
**Out of scope:** 指针发现与四个基本命令（`CL-23`）；写命令 fail-closed（`CL-28`）；`libra init --separate-git-dir`（保留 D25）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#740` `CloneArgs` 缺少 `--separate-git-dir` 及 `--bare` 互斥错误信息 | `src/command/clone.rs:89-91,1592`；`src/utils/convert.rs:242-252` |

**Acceptance criteria:**
- [ ] `libra clone --separate-git-dir <gitdir> <url> <workdir>` 将仓库数据写入 `<gitdir>` 并在 `<workdir>/.libra` 写入 `gitdir: <gitdir>` 指针文件（2 项独立校验）。
- [ ] `--bare`（或 `--mirror`）与 `--separate-git-dir` 组合时明确拒绝并输出 Git 精确互斥错误 `options '--bare' and '--separate-git-dir' cannot be used together`。
- [ ] 目标目录初始化失败时清理新建目录不留残余状态。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_separate_git_dir_and_bare_mutex` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_bare_repository`

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，合并后由 CL-25 统一执行 T-2 全量门）`
**Dependencies:** `CL-03`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `N/A`（`REL-CL-01` 子卡不独立发版，统一在 `CL-25` 发布）
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/development/commands/_compatibility.md`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=clone --separate-git-dir 与 --bare 互斥; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CL-03; writeset=串行于 CL-03; release=family child; split-from=N/A; exception=N/A`

---

### Task CL-23: 适配 `util.rs`/`alternates.rs` 指针布局并使四个基本命令可用（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-04` 拆出（G-09）。在 `src/utils/util.rs:278,314-324,791-803,1269,1464` 与 `src/command/alternates.rs:91` 解析 `.libra` `gitdir:` 指针，并使 `status`、`log`、`cat-file` 与无参数 `branch` 在该布局下退出 0。会改 ref 的 `branch` 以及其余子命令由 `CL-28` 默认拒绝。
**Out of scope:** 默认拒绝（`CL-28`）；会改 ref 的 `branch`（`CL-28`）；`libra init --separate-git-dir`（保留 D25）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#740` 需要 `util.rs` 与 `alternates.rs:91` 消费 `gitdir:` 指针 | `src/utils/util.rs:278,314-324,791-803,1269,1464`；`src/command/alternates.rs:91` |

**Acceptance criteria:**
- [ ] `util.rs` 的 `try_get_paths_full` 正确解析 `.libra` `gitdir:` 指针。
- [ ] `alternates.rs:91` 正确解析 `.libra` `gitdir:` 指针。
- [ ] `--separate-git-dir` 克隆仓库中 `libra status` 退出 0。
- [ ] 同一仓库中 `libra log` 退出 0。
- [ ] 同一仓库中 `libra cat-file -t HEAD` 退出 0。
- [ ] 同一仓库中无参数的 `libra branch` 退出 0。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收（不计入 G-03）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_separate_git_dir_pointer_commands` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test status_test::`

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，由 CL-25 统一执行 T-2）`
**Dependencies:** `CL-04`、`DEP-CL-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/util.rs`, `src/command/alternates.rs`, `tests/command/clone_test.rs`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Release write set:** `N/A`
**Files likely touched:** `src/utils/util.rs`, `src/command/alternates.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`, `docs/commands/branch.md`, `docs/commands/zh-CN/branch.md`, `docs/development/commands/branch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/branch.en.md`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`（网站文件写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (2/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=pointer 发现与四个基本命令; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CL-04,DEP-CL-10; writeset=串行于 CL-04; release=family child; split-from=CL-04; exception=N/A`

---

### Task CL-28: 在指针布局下拒绝未适配的写命令（#740）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-23` 拆出（G-09）。在 `src/cli.rs` `command_preflight`（:1755+）中，当 `.libra` 是 `gitdir:` 指针时默认拒绝。放行名单只有：`clone`；`status`；`log`；`cat-file`；不改 alternates 存储的 `alternates` 子命令；无参数的 `branch`；以及不打开仓库的 `init`、`upgrade`、`ui`、`login`、`whoami`、`logout`、`completions`。会修改 alternates 存储的子命令不在放行名单里。其余 `Commands::*` 变体退出非零，并且不改索引、`HEAD` 或 ref。`_compatibility.md` 的表必须列出每一个变体。本卡是 `REL-CL-01` 子卡 3/4，不单独推送。
**Out of scope:** 指针发现与只读基本命令（`CL-23`）；把被拒绝的命令改造成真正的指针布局（`DEFER-CL-02`）；稀疏 cone 护栏（`CL-26`/`CL-27`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 未适配写命令若在指针布局下继续执行，会把数据写到错误目录。`restore` 会读并保存索引 | `src/cli.rs:1755`；`src/command/restore.rs:788,934` |

**Acceptance criteria:**
- [ ] `.libra` 为 `gitdir:` 指针时，`libra restore .` 退出非零。
- [ ] 上一条命令结束后，索引文件字节与执行前相同。
- [ ] 同一仓库中 `libra commit` 退出非零。
- [ ] 上一条命令结束后，`HEAD` 与执行前相同。
- [ ] 同一仓库中 `libra branch newbranch` 退出非零。
- [ ] 上一条命令结束后，通过 ref API/数据库查询确认 `refs/heads/newbranch` 不存在，并与拒绝前 ref 状态逐项一致，禁止仅按工作树中文件是否存在判断。
- [ ] 同一仓库中 `libra checkout other` 退出非零。
- [ ] 拒绝夹具覆盖 `Commands` 的每一个变体，放行名单之外的变体都退出非零。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收（不计入 G-03）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_separate_git_dir_default_deny` `(new；通过 ref API/数据库查询比较拒绝前后 ref 状态，并覆盖全部 Commands 变体)`

**Full-suite trigger:** `N/A（REL-CL-01 家族子卡，由 CL-25 统一执行 T-2）`
**Dependencies:** `CL-23`、`DEP-CL-12`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/add.md`, `docs/commands/zh-CN/add.md`, `docs/development/commands/add.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`, `docs/commands/agent.md`, `docs/commands/zh-CN/agent.md`, `docs/development/commands/agent.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`, `docs/commands/archive.md`, `docs/commands/zh-CN/archive.md`, `docs/development/commands/archive.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`, `docs/commands/auth.md`, `docs/commands/zh-CN/auth.md`, `docs/development/commands/auth.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`, `docs/commands/automation.md`, `docs/commands/zh-CN/automation.md`, `docs/development/commands/automation.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`, `docs/commands/bisect.md`, `docs/commands/zh-CN/bisect.md`, `docs/development/commands/bisect.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`, `docs/commands/blame.md`, `docs/commands/zh-CN/blame.md`, `docs/development/commands/blame.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`, `docs/commands/bundle.md`, `docs/commands/zh-CN/bundle.md`, `docs/development/commands/bundle.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`, `docs/commands/cache.md`, `docs/commands/zh-CN/cache.md`, `docs/development/commands/cache.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`, `docs/commands/check-mailmap.md`, `docs/commands/zh-CN/check-mailmap.md`, `docs/development/commands/check-mailmap.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`, `docs/commands/cloud.md`, `docs/commands/zh-CN/cloud.md`, `docs/development/commands/cloud.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `docs/commands/commit-tree.md`, `docs/commands/zh-CN/commit-tree.md`, `docs/development/commands/commit-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `docs/commands/count-objects.md`, `docs/commands/zh-CN/count-objects.md`, `docs/development/commands/count-objects.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`, `docs/commands/credential.md`, `docs/commands/zh-CN/credential.md`, `docs/development/commands/credential.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`, `docs/commands/deps.md`, `docs/commands/zh-CN/deps.md`, `docs/development/commands/deps.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`, `docs/commands/diff-files.md`, `docs/commands/zh-CN/diff-files.md`, `docs/development/commands/diff-files.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`, `docs/commands/diff-index.md`, `docs/commands/zh-CN/diff-index.md`, `docs/development/commands/diff-index.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`, `docs/commands/diff-tree.md`, `docs/commands/zh-CN/diff-tree.md`, `docs/development/commands/diff-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`, `docs/commands/dirty.md`, `docs/commands/zh-CN/dirty.md`, `docs/development/commands/dirty.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`, `docs/commands/fast-export.md`, `docs/commands/zh-CN/fast-export.md`, `docs/development/commands/fast-export.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`, `docs/commands/fast-import.md`, `docs/commands/zh-CN/fast-import.md`, `docs/development/commands/fast-import.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `docs/commands/file.md`, `docs/commands/zh-CN/file.md`, `docs/development/commands/file.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`, `docs/commands/fsck.md`, `docs/commands/zh-CN/fsck.md`, `docs/development/commands/fsck.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`, `docs/commands/gc.md`, `docs/commands/zh-CN/gc.md`, `docs/development/commands/gc.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`, `docs/commands/grep.md`, `docs/commands/zh-CN/grep.md`, `docs/development/commands/grep.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`, `docs/commands/hash-object.md`, `docs/commands/zh-CN/hash-object.md`, `docs/development/commands/hash-object.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`, `docs/commands/hooks.md`, `docs/commands/zh-CN/hooks.md`, `docs/development/commands/hooks.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`, `docs/commands/hydrate.md`, `docs/commands/zh-CN/hydrate.md`, `docs/development/commands/hydrate.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`, `docs/commands/index-pack.md`, `docs/commands/zh-CN/index-pack.md`, `docs/development/commands/index-pack.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`, `docs/commands/investigate.md`, `docs/commands/zh-CN/investigate.md`, `docs/development/commands/investigate.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`, `docs/commands/layer.md`, `docs/commands/zh-CN/layer.md`, `docs/development/commands/layer.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`, `docs/commands/lfs.md`, `docs/commands/zh-CN/lfs.md`, `docs/development/commands/lfs.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`, `docs/commands/logfile.md`, `docs/commands/zh-CN/logfile.md`, `docs/development/commands/logfile.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`, `docs/commands/ls-remote.md`, `docs/commands/zh-CN/ls-remote.md`, `docs/development/commands/ls-remote.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`, `docs/commands/ls-tree.md`, `docs/commands/zh-CN/ls-tree.md`, `docs/development/commands/ls-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`, `docs/commands/mailinfo.md`, `docs/commands/zh-CN/mailinfo.md`, `docs/development/commands/mailinfo.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`, `docs/commands/media.md`, `docs/commands/zh-CN/media.md`, `docs/development/commands/media.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`, `docs/commands/mega2.md`, `docs/commands/zh-CN/mega2.md`, `docs/development/commands/mega2.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`, `docs/commands/memory.md`, `docs/commands/zh-CN/memory.md`, `docs/development/commands/memory.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `docs/commands/merge-base.md`, `docs/commands/zh-CN/merge-base.md`, `docs/development/commands/merge-base.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`, `docs/commands/merge-file.md`, `docs/commands/zh-CN/merge-file.md`, `docs/development/commands/merge-file.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`, `docs/commands/mergetool.md`, `docs/commands/zh-CN/mergetool.md`, `docs/development/commands/mergetool.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`, `docs/commands/metadata.md`, `docs/commands/zh-CN/metadata.md`, `docs/development/commands/metadata.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`, `docs/commands/op.md`, `docs/commands/zh-CN/op.md`, `docs/development/commands/op.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`, `docs/commands/open.md`, `docs/commands/zh-CN/open.md`, `docs/development/commands/open.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`, `docs/commands/pack-objects.md`, `docs/commands/zh-CN/pack-objects.md`, `docs/development/commands/pack-objects.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`, `docs/commands/read-tree.md`, `docs/commands/zh-CN/read-tree.md`, `docs/development/commands/read-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `docs/commands/reflog.md`, `docs/commands/zh-CN/reflog.md`, `docs/development/commands/reflog.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`, `docs/commands/remote.md`, `docs/commands/zh-CN/remote.md`, `docs/development/commands/remote.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`, `docs/commands/repack.md`, `docs/commands/zh-CN/repack.md`, `docs/development/commands/repack.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`, `docs/commands/rerere.md`, `docs/commands/zh-CN/rerere.md`, `docs/development/commands/rerere.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`, `docs/commands/review.md`, `docs/commands/zh-CN/review.md`, `docs/development/commands/review.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/development/commands/revision.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`, `docs/commands/sandbox.md`, `docs/commands/zh-CN/sandbox.md`, `docs/development/commands/sandbox.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`, `docs/commands/send-email.md`, `docs/commands/zh-CN/send-email.md`, `docs/development/commands/send-email.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`, `docs/commands/service.md`, `docs/commands/zh-CN/service.md`, `docs/development/commands/service.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`, `docs/commands/show-ref.md`, `docs/commands/zh-CN/show-ref.md`, `docs/development/commands/show-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`, `docs/commands/sparse-view.md`, `docs/commands/zh-CN/sparse-view.md`, `docs/development/commands/sparse-view.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`, `docs/commands/tag.md`, `docs/commands/zh-CN/tag.md`, `docs/development/commands/tag.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`, `docs/commands/update-index.md`, `docs/commands/zh-CN/update-index.md`, `docs/development/commands/update-index.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`, `docs/commands/update-ref.md`, `docs/commands/zh-CN/update-ref.md`, `docs/development/commands/update-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`, `docs/commands/verify-pack.md`, `docs/commands/zh-CN/verify-pack.md`, `docs/development/commands/verify-pack.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`, `docs/commands/worktree.md`, `docs/commands/zh-CN/worktree.md`, `docs/development/commands/worktree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`, `docs/commands/write-tree.md`, `docs/commands/zh-CN/write-tree.md`, `docs/development/commands/write-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Release write set:** `N/A`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/add.md`, `docs/commands/zh-CN/add.md`, `docs/development/commands/add.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/add.en.md`, `docs/commands/agent.md`, `docs/commands/zh-CN/agent.md`, `docs/development/commands/agent.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/agent.en.md`, `docs/commands/alternates.md`, `docs/commands/zh-CN/alternates.md`, `docs/development/commands/alternates.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/alternates.en.md`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/am.en.md`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`, `docs/commands/archive.md`, `docs/commands/zh-CN/archive.md`, `docs/development/commands/archive.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/archive.en.md`, `docs/commands/auth.md`, `docs/commands/zh-CN/auth.md`, `docs/development/commands/auth.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/auth.en.md`, `docs/commands/automation.md`, `docs/commands/zh-CN/automation.md`, `docs/development/commands/automation.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/automation.en.md`, `docs/commands/bisect.md`, `docs/commands/zh-CN/bisect.md`, `docs/development/commands/bisect.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/bisect.en.md`, `docs/commands/blame.md`, `docs/commands/zh-CN/blame.md`, `docs/development/commands/blame.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`, `docs/commands/bundle.md`, `docs/commands/zh-CN/bundle.md`, `docs/development/commands/bundle.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/bundle.en.md`, `docs/commands/cache.md`, `docs/commands/zh-CN/cache.md`, `docs/development/commands/cache.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cache.en.md`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`, `docs/commands/check-mailmap.md`, `docs/commands/zh-CN/check-mailmap.md`, `docs/development/commands/check-mailmap.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`, `docs/commands/cloud.md`, `docs/commands/zh-CN/cloud.md`, `docs/development/commands/cloud.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cloud.en.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `docs/commands/commit-tree.md`, `docs/commands/zh-CN/commit-tree.md`, `docs/development/commands/commit-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `docs/commands/count-objects.md`, `docs/commands/zh-CN/count-objects.md`, `docs/development/commands/count-objects.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/count-objects.en.md`, `docs/commands/credential.md`, `docs/commands/zh-CN/credential.md`, `docs/development/commands/credential.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/credential.en.md`, `docs/commands/deps.md`, `docs/commands/zh-CN/deps.md`, `docs/development/commands/deps.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/deps.en.md`, `docs/commands/describe.md`, `docs/commands/zh-CN/describe.md`, `docs/development/commands/describe.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/describe.en.md`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`, `docs/commands/diff-files.md`, `docs/commands/zh-CN/diff-files.md`, `docs/development/commands/diff-files.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff-files.en.md`, `docs/commands/diff-index.md`, `docs/commands/zh-CN/diff-index.md`, `docs/development/commands/diff-index.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff-index.en.md`, `docs/commands/diff-tree.md`, `docs/commands/zh-CN/diff-tree.md`, `docs/development/commands/diff-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff-tree.en.md`, `docs/commands/dirty.md`, `docs/commands/zh-CN/dirty.md`, `docs/development/commands/dirty.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/dirty.en.md`, `docs/commands/fast-export.md`, `docs/commands/zh-CN/fast-export.md`, `docs/development/commands/fast-export.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fast-export.en.md`, `docs/commands/fast-import.md`, `docs/commands/zh-CN/fast-import.md`, `docs/development/commands/fast-import.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fast-import.en.md`, `docs/commands/fetch.md`, `docs/commands/zh-CN/fetch.md`, `docs/development/commands/fetch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fetch.en.md`, `docs/commands/file.md`, `docs/commands/zh-CN/file.md`, `docs/development/commands/file.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/file.en.md`, `docs/commands/for-each-ref.md`, `docs/commands/zh-CN/for-each-ref.md`, `docs/development/commands/for-each-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/for-each-ref.en.md`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`, `docs/commands/fsck.md`, `docs/commands/zh-CN/fsck.md`, `docs/development/commands/fsck.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/fsck.en.md`, `docs/commands/gc.md`, `docs/commands/zh-CN/gc.md`, `docs/development/commands/gc.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/gc.en.md`, `docs/commands/grep.md`, `docs/commands/zh-CN/grep.md`, `docs/development/commands/grep.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/grep.en.md`, `docs/commands/hash-object.md`, `docs/commands/zh-CN/hash-object.md`, `docs/development/commands/hash-object.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/hash-object.en.md`, `docs/commands/hooks.md`, `docs/commands/zh-CN/hooks.md`, `docs/development/commands/hooks.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/hooks.en.md`, `docs/commands/hydrate.md`, `docs/commands/zh-CN/hydrate.md`, `docs/development/commands/hydrate.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/hydrate.en.md`, `docs/commands/index-pack.md`, `docs/commands/zh-CN/index-pack.md`, `docs/development/commands/index-pack.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/index-pack.en.md`, `docs/commands/investigate.md`, `docs/commands/zh-CN/investigate.md`, `docs/development/commands/investigate.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/investigate.en.md`, `docs/commands/layer.md`, `docs/commands/zh-CN/layer.md`, `docs/development/commands/layer.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/layer.en.md`, `docs/commands/lfs.md`, `docs/commands/zh-CN/lfs.md`, `docs/development/commands/lfs.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/lfs.en.md`, `docs/commands/logfile.md`, `docs/commands/zh-CN/logfile.md`, `docs/development/commands/logfile.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/logfile.en.md`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`, `docs/commands/ls-remote.md`, `docs/commands/zh-CN/ls-remote.md`, `docs/development/commands/ls-remote.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/ls-remote.en.md`, `docs/commands/ls-tree.md`, `docs/commands/zh-CN/ls-tree.md`, `docs/development/commands/ls-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/ls-tree.en.md`, `docs/commands/mailinfo.md`, `docs/commands/zh-CN/mailinfo.md`, `docs/development/commands/mailinfo.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/mailinfo.en.md`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`, `docs/commands/media.md`, `docs/commands/zh-CN/media.md`, `docs/development/commands/media.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/media.en.md`, `docs/commands/mega2.md`, `docs/commands/zh-CN/mega2.md`, `docs/development/commands/mega2.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`, `docs/commands/memory.md`, `docs/commands/zh-CN/memory.md`, `docs/development/commands/memory.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `docs/commands/merge-base.md`, `docs/commands/zh-CN/merge-base.md`, `docs/development/commands/merge-base.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/merge-base.en.md`, `docs/commands/merge-file.md`, `docs/commands/zh-CN/merge-file.md`, `docs/development/commands/merge-file.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/merge-file.en.md`, `docs/commands/mergetool.md`, `docs/commands/zh-CN/mergetool.md`, `docs/development/commands/mergetool.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/mergetool.en.md`, `docs/commands/metadata.md`, `docs/commands/zh-CN/metadata.md`, `docs/development/commands/metadata.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/metadata.en.md`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`, `docs/commands/op.md`, `docs/commands/zh-CN/op.md`, `docs/development/commands/op.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/op.en.md`, `docs/commands/open.md`, `docs/commands/zh-CN/open.md`, `docs/development/commands/open.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/open.en.md`, `docs/commands/pack-objects.md`, `docs/commands/zh-CN/pack-objects.md`, `docs/development/commands/pack-objects.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/pack-objects.en.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `docs/commands/push.md`, `docs/commands/zh-CN/push.md`, `docs/development/commands/push.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`, `docs/commands/read-tree.md`, `docs/commands/zh-CN/read-tree.md`, `docs/development/commands/read-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/read-tree.en.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `docs/commands/reflog.md`, `docs/commands/zh-CN/reflog.md`, `docs/development/commands/reflog.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`, `docs/commands/remote.md`, `docs/commands/zh-CN/remote.md`, `docs/development/commands/remote.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/remote.en.md`, `docs/commands/repack.md`, `docs/commands/zh-CN/repack.md`, `docs/development/commands/repack.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/repack.en.md`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`, `docs/commands/rerere.md`, `docs/commands/zh-CN/rerere.md`, `docs/development/commands/rerere.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rerere.en.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revert.en.md`, `docs/commands/review.md`, `docs/commands/zh-CN/review.md`, `docs/development/commands/review.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/review.en.md`, `docs/commands/revision.md`, `docs/commands/zh-CN/revision.md`, `docs/development/commands/revision.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/revision.en.md`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`, `docs/commands/sandbox.md`, `docs/commands/zh-CN/sandbox.md`, `docs/development/commands/sandbox.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/sandbox.en.md`, `docs/commands/send-email.md`, `docs/commands/zh-CN/send-email.md`, `docs/development/commands/send-email.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/send-email.en.md`, `docs/commands/service.md`, `docs/commands/zh-CN/service.md`, `docs/development/commands/service.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/service.en.md`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`, `docs/commands/show-ref.md`, `docs/commands/zh-CN/show-ref.md`, `docs/development/commands/show-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/show-ref.en.md`, `docs/commands/sparse-view.md`, `docs/commands/zh-CN/sparse-view.md`, `docs/development/commands/sparse-view.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/sparse-view.en.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`, `docs/commands/tag.md`, `docs/commands/zh-CN/tag.md`, `docs/development/commands/tag.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`, `docs/commands/update-index.md`, `docs/commands/zh-CN/update-index.md`, `docs/development/commands/update-index.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/update-index.en.md`, `docs/commands/update-ref.md`, `docs/commands/zh-CN/update-ref.md`, `docs/development/commands/update-ref.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/update-ref.en.md`, `docs/commands/verify-pack.md`, `docs/commands/zh-CN/verify-pack.md`, `docs/development/commands/verify-pack.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/verify-pack.en.md`, `docs/commands/worktree.md`, `docs/commands/zh-CN/worktree.md`, `docs/development/commands/worktree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/worktree.en.md`, `docs/commands/write-tree.md`, `docs/commands/zh-CN/write-tree.md`, `docs/development/commands/write-tree.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/write-tree.en.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`。`_compatibility.md` 的拒绝表与 `Commands` 枚举一一对应。网站文件写前核验 `cf` 分支。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CL-01 (3/4)`
**C/D coverage from:** `CL-25`
**Granularity:** `type=implementation; axis=指针布局默认拒绝未放行命令; recovery=发布前按 REL-CL-01 逆序本地 revert，发布后按 CL-25 immutable-release 前滚; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CL-23,DEP-CL-12; writeset=串行于 CL-23; release=family child; split-from=CL-23; exception=N/A`

---

### Task CL-25: 发布 `REL-CL-01`（`clone --separate-git-dir` 指针布局家族发布点）（#740）

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 聚合 `REL-CL-01` 家族子卡 `CL-04`、`CL-23` 与 `CL-28`（不引入新生产代码变更），执行 `patch + 1`、`compat_version_surface_sync` 三处版本面同步、工具链刷新 `Cargo.lock`、全量 `T-2` 门禁、`cargo build --release` 与 `gh release create`。对外发布指针布局、四个只读基本命令，以及 `CL-28` 对未放行子命令的默认拒绝。
**Out of scope:** 任何新增功能代码实现；`libra init --separate-git-dir`（`DEFER-CL-02`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `REL-CL-01` 需在 `CL-04`、`CL-23` 与 `CL-28` 均完成本地提交后统一版本化发布 | `Cargo.toml`；`install.sh`；`install.ps1` |

**Acceptance criteria:**
- [ ] `CL-04`、`CL-23` 与 `CL-28` 都已通过各自 review 与 ER-04 A 组门禁并完成本地提交。
- [ ] 完成 `patch + 1`、`compat_version_surface_sync` 三处版本面同步、工具链刷新 `Cargo.lock` 与 `cargo build --release`。
- [ ] 全量 `T-2` 与 `gh release create` / `release.yml` 远端门全部通过。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_separate_git_dir`

**Full-suite trigger:** `T-2: REL-CL-01 家族发布点全量门`
**Dependencies:** `CL-04`, `CL-23`, `CL-28`
**Deliverables:** `Release artifacts & tag for REL-CL-01`
**Implementation write set:** `N/A`
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`
**Docs and compatibility impact:** `N/A`（仅家族版本化发布与 release notes 汇总，无新增/变更用户命令语义）
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CL-01`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CL-01 指针布局家族发布; recovery=发布前按 CL-25 -> CL-28 -> CL-23 -> CL-04 逆序本地 revert，发布后前滚新 patch; complete=yes; self-contained=yes; AC=3/12; VER=1/12; landing=0; prod-files=0; scope=S; deps=CL-04,CL-23,CL-28; writeset=串行于 CL-28; release=family release point; split-from=N/A; exception=N/A`

---

### Task CL-05: 支持 `libra clone --revision=<rev>` 单修订克隆（#741）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 为 `CloneArgs` 新增 `--revision=<rev>`（支持分支、tag 或完整 OID），在 `clone.rs` 与 `fetch.rs`/`protocol` 层仅拉取指定单个修订并检出为游离 HEAD（`--bare --revision` 下不保留任何 refs 且不写 `remote.origin.fetch`），并与 `--branch`/`--mirror` 互斥。
**Out of scope:** `--depth` 浅历史截断本身（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#741` `CloneArgs` 缺少 `--revision` 标志且 `fetch` 仅协商分支/tag | `src/command/clone.rs:64-96`；`src/command/fetch.rs` |

**Acceptance criteria:**
- [ ] `libra clone --revision=<rev> <url> <dir>` 仅拉取目标修订并以游离 HEAD 检出。
- [ ] `libra clone --bare --revision=<rev> <url> <dir>` 克隆后不保留任何 refs 且不写入 `remote.origin.fetch`。
- [ ] `--revision` 与 `--branch`、`--mirror` 组合时按 Git 规则拒绝。
- [ ] 目标修订在远端不存在时返回明确错误并清理新建目录。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_revision_single_rev` `(new)`
- [ ] `source .env.test && cargo nextest run --test compat_clone_shallow_integrity`

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs 与 src/internal/protocol/mod.rs`
**Dependencies:** `CL-25`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --revision 单修订克隆; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=3; scope=M; deps=CL-25,DEP-CL-04; writeset=串行于 CL-25; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-06: 支持 `libra clone -c <key=value>` 配置覆盖与 `clone.defaultRemoteName`（#752）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 为 `CloneArgs` 新增可重复 `-c` / `--config <key=value>`，在初始化目标仓库配置与执行初始 fetch/checkout 前写入并生效，同时支持 `clone.defaultRemoteName` 并确保显式 `--origin` 优先于 `-c clone.defaultRemoteName`。
**Out of scope:** 非 `clone` 子命令的全局配置覆盖。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#752` `CloneArgs` 缺少 `-c`/`--config` 参数且未读取 `clone.defaultRemoteName` | `src/command/clone.rs:64-236` |

**Acceptance criteria:**
- [ ] `libra clone -c <key=value> <url> <dir>` 将配置持久化写入新克隆仓库并立即影响本次克隆流程（覆盖 `clone.defaultRemoteName`、`core.*`、`remote.<name>.*`、`fetch.*`、`user.*`）。
- [ ] 支持多次传入 `-c`，按出现顺序生效。
- [ ] 支持 `clone.defaultRemoteName`，且显式 `-o`/`--origin` 优先于 `-c clone.defaultRemoteName=<name>`。
- [ ] 非法 `<key=value>` 格式（无 `=` 或空键名）返回 CLI 参数错误。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_config_override_dash_c` `(new)`
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`

**Full-suite trigger:** `none`
**Dependencies:** `CL-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -c <key=value> 配置覆盖; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CL-05; writeset=串行于 CL-05; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-08: 实现 `libra clone -l` / `--local` 硬链接本地对象（#743）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 当从本地文件系统仓库执行 `libra clone`（默认或 `-l`/`--local`，未传 `--no-local`）且源与目标在同一文件系统时，使用 `std::fs::hard_link` 链接松散/pack 对象文件（跨文件系统或硬链接失败时安全回退复制；保持既有 `clone.rs:125` 对 `--depth`/`--filter` 本地路径警告语义并在测试中以 Git 对照验证）。对象目录或对象文件为符号链接时 fail-closed 拒绝，禁止跟随链接读取仓库外内容。（先于 `CL-07` 落地以使 `--no-hardlinks` 可实测验证。）
**Out of scope:** `--no-hardlinks` 标志（`CL-07`）与 `--reference`（`CL-10`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#743` `CloneArgs::local` 文档明言始终复制从不硬链接 | `src/command/clone.rs:124-130`；`src/internal/protocol/local_client.rs` |

**Acceptance criteria:**
- [ ] 同文件系统正常夹具中，**所有符合硬链接条件的对象文件**均实际硬链接成功，源、目标设备号与 inode 逐一相同且 `nlink > 1`；至少包含一个松散对象和一个 pack 对象作为正控制，禁止仅链接这两个对象而复制其余可链接对象。
- [ ] 跨文件系统或 `--no-local` 时自动复制对象。
- [ ] 同文件系统硬链接失败时逐对象安全回退复制并保留完整内容；失败对象不得要求共享 inode，成功对象仍按上一条验收。
- [ ] 源对象目录或对象文件为符号链接时 fail-closed 拒绝克隆，不得跟随链接读取仓库外内容，且目标不得包含外部文件。
- [ ] 完整本地路径下 `--depth`/`--filter` 与 Git 对照行为一致。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_local_hardlinks_objects_on_same_fs` `(new；正控制：逐一覆盖所有符合硬链接条件的对象并比较设备号/inode，至少含一个松散对象和一个 pack 对象，禁止其余对象被复制)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_local_hardlink_failure_falls_back_to_copy` `(new；同文件系统故障注入，验证失败对象复制内容完整且回退后不误报 inode 共享)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_local_rejects_symlinked_object_paths` `(new；对象目录和对象文件分别指向仓库外路径，验证拒绝且无外部内容进入目标)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_local_flag_accepted_for_local_source`

**Full-suite trigger:** `T-1: 修改共享局部传输模块 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-06`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支，更新「从不硬链接」说明）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone -l/--local 硬链接本地对象; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=CL-06; writeset=串行于 CL-06; release=independent; split-from=N/A; exception=N/A`

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

**Acceptance criteria:**
- [ ] `libra clone --no-hardlinks <local-repo> <dir>` 在同文件系统下复制对象（目标对象 inode 与源不同，硬链接计数为 1）。
- [ ] 与 `-l` 组合时 `--no-hardlinks` 抑制硬链接。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_no_hardlinks_copies_objects` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_local_hardlinks_objects_on_same_fs`

**Full-suite trigger:** `none`
**Dependencies:** `CL-08`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --no-hardlinks 标志; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CL-08; writeset=串行于 CL-08; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-09: 支持 `libra clone` 的 `<path>.git` 后缀回退解析（含 `file://`）（#744）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 当 `libra clone <path>` 或 `libra clone file://<path>` 指定的源路径不存在（或非仓库）时，按 Git 约定回退探测 `<path>.git`（以及 `<path>/.git`），且 `file://` URL 保持不创建硬链接语义。
**Out of scope:** HTTP/SSH 远端 URL 重定向（永久非目标）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#744` 本地路径与 `file://` URL 解析未尝试 `<path>.git` 后缀回退 | `src/command/clone.rs`；`src/internal/protocol/local_client.rs` |

**Acceptance criteria:**
- [ ] 当仅存在 `foo.git` 裸仓库时，`libra clone foo` 成功克隆到 `foo` 目录。
- [ ] `libra clone file:///.../foo` 同样回退解析 `foo.git` 并使用传输复制对象（不创建硬链接）。
- [ ] `<path>` 与 `<path>.git` 均不存在时返回明确本地仓库不存在错误。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_dot_git_suffix_fallback_and_file_url` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_bare_and_mirror_default_names_and_layout`

**Full-suite trigger:** `T-1: 修改共享传输 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone <path>.git 后缀回退; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CL-07; writeset=串行于 CL-07; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-10: 实现 `clone --reference` / `--reference-if-able` alternates 借用（#751）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 复用既有 alternates 支持（`src/command/alternates.rs` 与 `clone.rs:1554-1735` `--shared` 注册逻辑）使 `--reference <ref>`（及 `--reference-if-able`）真正注册 `objects/info/alternates` 并在 `fetch` / 协商阶段借用参考库对象以跳过重复传输（并将过时测试 `test_clone_object_alternates_flags_are_noops` 重命名/重写为 `test_clone_reference_alternates_borrows_objects` 与 `test_clone_reference_if_able_and_invalid_repo`）。（按 G-09 自本卡拆出 `CL-18` 处理 `--dissociate`。）
**Out of scope:** `--dissociate`（由 `CL-18` 承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#751` `--reference` 当前仅打印警告并全量复制对象；`clone.rs:1554-1735` 已有 alternates 注册辅助 | `src/command/clone.rs:147-182,250-260,1554-1735`；`src/command/alternates.rs`；`tests/command/clone_test.rs:1118` |

**Acceptance criteria:**
- [ ] `libra clone --reference <ref> <src> <dst>`（含 `--no-local --reference`）写入 `objects/info/alternates` 并在协商阶段借用参考库对象，减少传输字节与本地存储；移除 `<ref>` 后目标仓库 `fsck` 报错失败。
- [ ] `--reference-if-able` 在参考库不可用时不报错继续克隆。
- [ ] `--reference` 指向无效仓库时 fail-closed 报错并清理新建目录。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_reference_alternates_borrows_objects` `(new，重命名替代旧 no-op 测试)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_reference_if_able_and_invalid_repo` `(new)`

**Full-suite trigger:** `T-1: 修改共享传输 src/command/fetch.rs 与 src/internal/protocol/local_client.rs`
**Dependencies:** `CL-09`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --reference alternates 借用; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=3; scope=M; deps=CL-09,DEP-CL-04; writeset=串行于 CL-09; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-18: 实现 `clone --dissociate` 拷贝借用对象并解除 alternates（#751）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-10` 拆出：在 `clone --reference <ref> --dissociate` 与 `clone --shared --dissociate` 完成后，分别补全本地缺失对象拷贝并移除 `objects/info/alternates`，保证两种克隆库都完全独立。
**Out of scope:** `--reference` 初始借用协商（`CL-10`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#751` `CloneArgs::dissociate` 当前仅在 `--shared` 下覆盖且未执行对象补全拷贝与 alternates 解除 | `src/command/clone.rs:178-182` |

**Acceptance criteria:**
- [ ] `libra clone --reference <ref> --dissociate <src> <dst>` 补全本地对象拷贝并移除 `objects/info/alternates`。
- [ ] 删除 `<ref>` 仓库后，`<dst>` 仍完整可读通过校验。
- [ ] `libra clone --shared --dissociate <src> <dst>` 同样完成对象补全拷贝与 alternates 解除，删除源仓库后目标完整可读。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_dissociate_keeps_repo_independent` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_shared_dissociate_detaches_alternates` `(new；删除源仓库后验证目标完整可读且 alternates 已解除)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_reference_alternates_borrows_objects`

**Full-suite trigger:** `none`
**Dependencies:** `CL-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --dissociate 解除 alternates; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=3/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=CL-10; writeset=串行于 CL-10; release=independent; split-from=CL-10; exception=N/A`

---

### Task CL-12: 校验并拒绝无效 `sparse:` filter 规格（#748）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `clone --filter=sparse:...` 时由传输/协议与本地客户端层（`local_client.rs`、`protocol/mod.rs`、`clone.rs`）校验 sparse filter 表达式（覆盖 `#748` 的两类可解析失败情形：指向不存在 OID 与指向非 blob 对象），输出 Git 兼容错误消息并清理新建目标目录（将旧 `test_clone_unsupported_fetch_optimizations_warn_and_full_clone` 重命名为 `test_clone_rejects_invalid_sparse_filter`）。
**Out of scope:** promisor lazy fetch（`CL-19`）；多 `--filter` 组合与协商（`CL-13`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#748` `clone` 忽略 `--filter` 内容，对无效 `sparse:` 过滤器静默成功 | `src/command/clone.rs:191-198`；`src/internal/protocol/local_client.rs`；`tests/command/clone_test.rs:1376` |

**Acceptance criteria:**
- [ ] `libra clone --no-local --bare --filter=sparse:oid=main:no-such-name` 与指向非 blob 对象均返回非零错误，分别输出 Git 精确错误 `unable to access sparse blob in` 与 `unable to parse sparse filter data in`。
- [ ] 无效 `sparse:path=` 语法同样被拒绝。
- [ ] 在本卡自己的发布提交上，本卡不实现的其它 `--filter` 值退出非零，不得静默全量克隆。`CL-13` 发布后 `blob:none` 不再算未实现。`CL-24` 发布后 `combine:blob:none+tree:1` 不再算未实现。最终树只拒绝仍然未实现的 filter。
- [ ] 在本卡自己的发布提交上，远端未广告 `filter` 能力时退出非零，不得静默全量克隆。这条断言不要求在 `CL-24` 之后的最终树上仍成立。
- [ ] 失败时清理新建目标目录，不残留损坏仓库。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_rejects_invalid_sparse_filter` `(new，重命名替代旧 no-op 测试)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_default_branch`

**Full-suite trigger:** `T-1: 修改共享协议层 src/internal/protocol/local_client.rs 与 mod.rs`
**Dependencies:** `CL-19`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/protocol/local_client.rs`, `src/internal/protocol/mod.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone 无效 sparse filter 拒绝; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=3; scope=M; deps=CL-19; writeset=串行于 CL-19; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-13: 实现 `--filter=blob:none` 协商、promisor 配置写入与未实现组合 fail-closed（#746, #754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 将 `CloneArgs::filter` 从 `Option<String>` 改为 `Vec<String>` 以便识别重复 `--filter`：在本卡先支持单个或重复同值的 `--filter=blob:none`（对实际远端名 `<remote_name>` 尊重 `-o`/`--origin` 与 `CL-06` 的 `clone.defaultRemoteName`，写入 `remote.<remote_name>.promisor=true`、`remote.<remote_name>.partialclonefilter` 与 `extensions.partialClone=<remote_name>`，并在本地与远端传输层 `local_client.rs:889+`、`fetch.rs`、`protocol/mod.rs` 应用 `blob:none` 过滤协商）；在 `CL-24` 落地前，若传入 `combine:*` 或多种不同 `--filter` 组合则明确 `fail-closed` 拒绝（避免中间发布版本静默忽略组合过滤）。（按 G-09 拆出 `CL-19` 先行落地对象读取层按需 lazy fetch，拆出 `CL-24` 承接 `combine:blob:none+tree:1` 传输与无能力回退，拆出 `CL-21` 承接 `Receiving objects` 进度与端到端 lazy fetch 验收。）
**Out of scope:** 对象读取触发的按需 lazy fetch（`CL-19`）；`combine:blob:none+tree:1` 根树过滤与远端无能力回退（`CL-24`）；`Receiving objects` 进度与端到端 `cat-file` 验证（`CL-21`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#754` `#746` `CloneArgs::filter` 为 `Option<String>` 且无 promisor 配置写入；本地测试走 `local_client.rs:889+` | `src/command/clone.rs:191-198,269`；`src/command/fetch.rs`；`src/internal/protocol/local_client.rs:889` |

**Acceptance criteria:**
- [ ] `CloneArgs::filter` 改为 `Vec<String>`：单个或重复相同 `--filter=blob:none` 去重为 `blob:none`（1 项）；在本卡自己的发布提交上，传入 `combine:*` 或多种不同 `--filter` 组合时 fail-closed 返回明确错误（1 项）。`CL-24` 发布后这条拒绝不再要求在最终树上成立。
- [ ] `libra clone --filter=blob:none` 对实际远端名 `<remote_name>`（含 `-o`/`clone.defaultRemoteName`）写入 `remote.<remote_name>.promisor=true`、`remote.<remote_name>.partialclonefilter=blob:none` 与 `extensions.partialClone=<remote_name>`（3 项配置独立校验）。
- [ ] `--filter=blob:none` 传输协商初始不下载多余 blob 对象（1 项）。
- [ ] 在本卡自己的发布提交上，单个尚未实现的 `--filter` 值（不是 `blob:none`）退出非零。`CL-24` 发布后，只拒绝仍然未实现的 filter；`combine:blob:none+tree:1` 不再算未实现。
- [ ] 在本卡自己的发布提交上，远端未广告 `filter` 能力时退出非零，不得静默全量克隆。这条断言不要求在 `CL-24` 之后的最终树上仍成立。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收（不计入 G-03）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_filter_promisor_config_and_combine` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_json_reports_fetch_transfer_counts`

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs、src/internal/protocol/mod.rs 与 local_client.rs`
**Dependencies:** `CL-12`、`DEP-CL-04`（协调 `plan-20261117-fetch-push.md#FP-07,#FP-15` 共享 `fetch.rs`）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --filter=blob:none 与 promisor 配置; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=4; scope=M; deps=CL-12,DEP-CL-04; writeset=串行于 CL-12; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-24: 实现 `combine:blob:none+tree:1` 过滤传输与无能力回退（#746, #754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-13` 拆出：在 `local_client.rs`、`fetch.rs`、`protocol/mod.rs` 与 `clone.rs` 应用 `combine:blob:none+tree:1` 树深过滤（仅下载根 tree 与 commits），并实现远端未广告 `filter` 能力时的安全回退全量克隆与提示。
**Out of scope:** promisor 配置写入（`CL-13`）；`Receiving objects` 进度与端到端 `cat-file` 验证（`CL-21`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#746` `#754` 传输层尚未应用 `combine:blob:none+tree:1` 树深过滤与远端无能力回退提示 | `src/command/clone.rs:191-198`；`src/command/fetch.rs`；`src/internal/protocol/local_client.rs:889` |

**Acceptance criteria:**
- [ ] `libra clone --filter=blob:none --filter=tree:1` 仅下载根 tree 与 commits，且传输跟踪包含 `--filter=combine:blob:none+tree:1`。
- [ ] `libra clone --filter=combine:blob:none+tree:1` 同样仅下载根 tree 与 commits。
- [ ] 远端未广告 `filter` 能力时按 Git 语义安全回退全量克隆并输出警告提示（2 项独立校验）。同时改写 `CL-12` 的「远端未广告 filter 则退出非零」断言，以及 `CL-13` 里「远端无能力则退出非零」和「拒绝 `combine:*`」的测试，使它们改断言本卡的最终契约。`CL-12` 对无效 `sparse:` 的拒绝断言保留。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_combine_tree_depth_and_fallback` `(new；并改写 CL-12 无能力远端断言与 CL-13 过渡断言)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_json_reports_fetch_transfer_counts`

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs、src/internal/protocol/mod.rs 与 local_client.rs`
**Dependencies:** `CL-13`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone combine:blob:none+tree:1 过滤与回退; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=4; scope=M; deps=CL-13,DEP-CL-04; writeset=串行于 CL-13; release=independent; split-from=CL-13; exception=N/A`

---

### Task CL-29: 实现 `sparse:oid` 过滤与预期缺失对象边界（#748；`PM-03`/`t5616#33` 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-24` 拆出：在 `clone.rs`、`fetch.rs`、`protocol/mod.rs` 与 `local_client.rs` 实现 `--filter=sparse:oid=<object-expression>` 的传输、稀疏对象闭包与 promisor 预期缺失元数据。对象表达式必须解析为包含稀疏匹配规则的 blob（不是 tree）；使部分克隆能够表达并保留 Git `sparse:oid` 过滤边界，供 `PM-03` 的 `t5616#33`/`TC-0625` `--missing=print|allow-promisor|error` 验收消费。
**Out of scope:** `rev-list --missing=<action>` 命令行为（`PM-03`）；`Receiving objects` 进度与最终 `cat-file` 收口（`CL-21`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有 `CL-12`/`CL-13` 对其它 filter fail-closed，`CL-24` 仅交付 `combine:blob:none+tree:1`；`PM-03` 需要 `sparse:oid` 的预期缺失对象夹具，且 `CL-12` 已明确 sparse 规则 blob 边界 | `src/command/clone.rs:191-198`；`plan-20261117-plumbing-misc.md#PM-03`；Git `2.56.0@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd` `t/t5616-partial-clone.sh#33` |

**Acceptance criteria:**
- [ ] `libra clone --filter=sparse:oid=<object-expression>` 在对象表达式解析为规则 blob 后按 Git sparse 语义传输过滤规格并保留对应 promisor 预期缺失对象集合，不以全量克隆冒充实现。
- [ ] 对象表达式缺失、指向非 blob 或 blob 稀疏规则解析失败时退出非零且不遗留半成品目标目录；以 Git 对照验证这些拒绝路径。
- [ ] 对过滤后预期缺失对象记录可验证的 promisor/稀疏边界，后续 `cat-file` 可按 OID lazy fetch，非预期本地缺失仍按 fail-closed 处理。
- [ ] 提供 `t5616#33` 同形夹具，明确 `sparse:oid` 产生的缺失对象 OID 与 promised/promisor 归属，供 `PM-03` 直接复用并验证 `t5616#33`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_sparse_oid_filter_and_missing_boundary` `(new；含有效/缺失/非 blob/规则损坏的对象表达式、过滤对象集合、lazy-fetch 边界与 Git 对照)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_combine_tree_depth_and_fallback`

**Full-suite trigger:** `T-1: 修改共享传输与协议核心 src/command/fetch.rs、src/internal/protocol/mod.rs 与 local_client.rs`
**Dependencies:** `CL-24`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `src/internal/protocol/mod.rs`, `src/internal/protocol/local_client.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone sparse:oid 过滤与预期缺失边界; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=4; scope=M; deps=CL-24,DEP-CL-04; writeset=串行于 CL-24; release=independent; split-from=CL-24; exception=N/A`

---

### Task CL-21: 实现部分克隆 `Receiving objects` 进度与端到端 lazy fetch 验证（#754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-13` 拆出：在部分克隆传输阶段向 stderr 输出 `Receiving objects` 进度（非 `--no-progress`/`--quiet`），并打通真实 `libra clone --filter=blob:none file://...` 后执行 `libra cat-file -p <rev>:<path>` 端到端按需 lazy fetch 完整链路。
**Out of scope:** `--filter` 协商与 promisor 配置写入（`CL-13`）；`cat-file` 单对象读写层底层接线（`CL-19`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#754` 部分克隆传输缺少 `Receiving objects` 进度且需验证 `clone --filter=blob:none` → `cat-file -p` 端到端链路 | `src/command/clone.rs:191-198,269`；`src/command/fetch.rs`；`src/internal/protocol/local_client.rs` |

**Acceptance criteria:**
- [ ] 部分克隆传输阶段向 stderr 输出 `Receiving objects` 进度；传入 `--no-progress` 或 `--quiet` 时抑制该进度输出（2 项独立校验）。
- [ ] 真实 `libra clone --filter=blob:none file://...` 后执行 `libra cat-file -p <rev>:<path>` 端到端触发按需 lazy fetch 并完整输出对象内容（`Receiving objects` 单元逻辑由 `CL-19` 覆盖，本卡验收真实 `clone -> cat-file` 全链路）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_partial_progress_and_e2e_lazy_fetch` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test cat_file_test::`

**Full-suite trigger:** `T-1: 修改共享传输 src/command/fetch.rs`
**Dependencies:** `CL-29`、`DEP-CL-04`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md`、`docs/commands/zh-CN/clone.md`、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --filter 进度与端到端 lazy fetch; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CL-29,DEP-CL-04; writeset=串行于 CL-29; release=independent; split-from=CL-13; exception=N/A`

---

### Task CL-19: 实现 promisor 缺失对象读取触发按需 lazy fetch（#754）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-13` 拆出并前置至 `CL-12`/`CL-13` 之前：在对象读取与存储回退层（`src/utils/storage/local.rs`、`src/utils/object_ext.rs`、`src/command/fetch.rs`、`src/internal/protocol/local_client.rs`、`src/command/cat_file.rs`）实现 promisor 按 `extensions.partialClone` / `remote.<name>.promisor=true` 按需拉取单对象（按 OID，并在 `local.rs`/`object_ext.rs` 内直接执行 OID 完整性校验；复用现有 `LocalClient` 与 `ProtocolClient` 传输接口），使 `libra cat-file -p <rev>:<path>` 在预置 promisor 夹具中自动 lazy fetch 缺失对象并输出 `Receiving objects` 进度，且在 `--offline` 或 `LIBRA_READ_POLICY=local` 下 fail-closed 不发起网络请求。
**Out of scope:** `clone --filter` CLI 解析与写入 promisor 配置（`CL-13`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#754` 当前对象存储层与 `cat-file` 在遇到缺失 promisor 对象时直接报 NotFound（`hydrate.rs` 在私有 `hydrate_one` 中内联校验 OID，本卡在 `local.rs`/`object_ext.rs` 独立实现） | `src/command/clone.rs:269`；`src/utils/storage/local.rs`；`src/utils/object_ext.rs` |

**Acceptance criteria:**
- [ ] 在预置 `extensions.partialClone=<remote_name>` 与 `remote.<remote_name>.promisor=true`（含非 `origin` 自定义远端名）及缺失 blob 的仓库夹具中，执行 `libra cat-file -p <rev>:<path>` 自动按 OID lazy fetch 缺失 blob 并输出内容与 `Receiving objects` 进度。
- [ ] 非 promisor 仓库缺失对象或启用 `--offline` / `LIBRA_READ_POLICY=local` 时 fail-closed 报错，不触发任何隐式网络请求（GC-07）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_partial_cat_file_triggers_lazy_fetch` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test cat_file_test::`

**Full-suite trigger:** `T-1: 修改共享对象存储读取核心 src/utils/storage/local.rs 与 object_ext.rs`
**Dependencies:** `CL-18`、`DEP-CL-04`、`DEP-CL-08`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/storage/local.rs`, `src/utils/object_ext.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `src/command/cat_file.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/utils/storage/local.rs`, `src/utils/object_ext.rs`, `src/command/fetch.rs`, `src/internal/protocol/local_client.rs`, `src/command/cat_file.rs`
**Docs and compatibility impact:** 同步 `docs/commands/{clone,cat-file}.md` EN+zh、`docs/development/commands/{clone,cat-file}.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/{clone,cat-file}.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=promisor 缺失对象读取 lazy fetch; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=4; prod-files=5; scope=M; deps=CL-18,DEP-CL-04,DEP-CL-08; writeset=串行于 CL-18; release=independent; split-from=CL-13; exception=N/A`

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

**Acceptance criteria:**
- [ ] `libra clone --ref-storage-format=sqlite <url> <dir>` 与 `libra clone --mirror --ref-storage-format=sqlite <url> <dir>` 成功克隆（不改变 `init --ref-format=files` 拒绝行为）。
- [ ] `libra rev-parse --show-ref-storage-format`（含在 `--mirror` 裸仓库内）如实输出 `sqlite`。
- [ ] 传入 `files`、`reftable` 或未知 `--ref-storage-format` 时 fail-closed 返回明确错误（提示当前使用 SQLite ref 存储，`files`/`reftable` 见 `DEFER-CL-01`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_ref_storage_format_and_rev_parse` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test rev_parse_test::`

**Full-suite trigger:** `none`
**Dependencies:** `CL-21`、`DEP-CL-02`、`DEP-CL-09`（等待 `plan-20261117-plumbing-misc.md#PM-06` 先合入 `rev_parse.rs`）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/rev_parse.rs`, `tests/command/clone_test.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/command/rev_parse.rs`
**Docs and compatibility impact:** 同步 `docs/commands/{clone,rev-parse}.md` EN+zh、`docs/development/commands/{clone,rev-parse}.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/{clone,rev-parse}.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--ref-storage-format 与 rev-parse; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=CL-21,DEP-CL-02,DEP-CL-09; writeset=串行于 CL-21; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-15: 修复 `clone --single-branch` 从游离 HEAD 克隆错误创建 `refs/remotes/origin/main`（#753）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 当源仓库处于游离 HEAD 且执行 `libra clone --single-branch`（未指定 `--branch`）时，仅更新游离 HEAD / `FETCH_HEAD` 而不虚构 `refs/remotes/origin/main`，且后续执行 `libra fetch` 同样不会创建 `refs/remotes/origin/main`。
**Out of scope:** 正常有分支 HEAD 的 `--single-branch` 克隆（由既有实现承接）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#753` 源仓库游离 HEAD 时 `--single-branch` 仍回退创建 `refs/remotes/origin/main` | `src/command/clone.rs:76-87` |

**Acceptance criteria:**
- [ ] 从游离 HEAD 仓库执行 `libra clone --single-branch <src> <dst>` 不创建 `refs/remotes/origin/main`。
- [ ] 在该克隆仓库后续执行 `libra fetch` 后仍不创建 `refs/remotes/origin/main`。
- [ ] 显式指定 `--branch <name>` 时仍正常拉取并创建对应跟踪分支。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_single_branch_from_detached_head` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_default_branch_single_branch`

**Full-suite trigger:** `none`
**Dependencies:** `CL-14`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--single-branch 游离 HEAD 处理; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CL-14; writeset=串行于 CL-14; release=independent; split-from=N/A; exception=N/A`

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

**Acceptance criteria:**
- [ ] `core.sparseCheckout=true` 时 `libra checkout` 被 `command_preflight` 拒绝。
- [ ] `core.sparseCheckout=true` 时 `libra switch` 被 `command_preflight` 拒绝。
- [ ] `core.sparseCheckout=true` 时 `libra reset` 被 `command_preflight` 拒绝。
- [ ] `core.sparseCheckout=true` 时 `libra restore` 被 `command_preflight` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：本卡「Docs and compatibility impact」所列文件已更新并逐文件验收（不计入 G-03）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_sparse_preflight_checkout_switch_reset_restore` `(new)`

**Full-suite trigger:** `T-1: 修改全局命令预检 src/cli.rs`
**Dependencies:** `CL-15`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `docs/commands/switch.md`, `docs/commands/zh-CN/switch.md`, `docs/development/commands/switch.md`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/switch.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 同步 `checkout`/`switch`/`reset`/`restore` 的 EN+zh+dev 与网站 `*.en.md`，以及 `docs/development/commands/_compatibility.md` 与 `COMPATIBILITY.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=稀疏检出类命令 fail-closed 护栏; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CL-15; writeset=串行于 CL-15; release=independent; split-from=CL-16; exception=N/A`

---

### Task CL-27: 在 `cli.rs` 为稀疏工作区的历史类命令增加 `fail-closed` 护栏（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-26` 拆出（G-09）：在同一 `command_preflight` 护栏上覆盖 `merge`、`pull`、`rebase`、`cherry-pick`、`stash`。只有目录重命名合并的解禁由 `CL-20` 承接，其它 `merge` 继续拒绝。
**Out of scope:** `checkout`/`switch`/`reset`/`restore`（`CL-26`）；`merge` 稀疏 lazy fetch 解禁（`CL-20`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` 的 `merge`/`pull`/`rebase`/`cherry-pick`/`stash` 尚未适配 `core.sparseCheckout=true` | `src/cli.rs:1755` |

**Acceptance criteria:**
- [ ] 在本卡自己的发布提交上，`core.sparseCheckout=true` 时 `libra merge` 被 `command_preflight` 拒绝。`CL-20` 发布后这条拒绝不再要求在最终树上成立。
- [ ] `core.sparseCheckout=true` 时 `libra pull` 被 `command_preflight` 拒绝。
- [ ] `core.sparseCheckout=true` 时 `libra rebase` 被 `command_preflight` 拒绝。
- [ ] `core.sparseCheckout=true` 时 `libra cherry-pick` 被 `command_preflight` 拒绝。
- [ ] `core.sparseCheckout=true` 时 `libra stash` 被 `command_preflight` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：本卡「Docs and compatibility impact」所列文件已更新并逐文件验收（不计入 G-03）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash` `(new)`

**Full-suite trigger:** `T-1: 修改全局命令预检 src/cli.rs`
**Dependencies:** `CL-26`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/clone_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/rebase.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `tests/command/clone_test.rs`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 同步 `merge`/`pull`/`rebase`/`cherry-pick`/`stash` 的 EN+zh+dev 与网站 `*.en.md`，以及 `COMPATIBILITY.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=稀疏历史类命令 fail-closed 护栏; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CL-26; writeset=串行于 CL-26; release=independent; split-from=CL-26; exception=N/A`

---

### Task CL-16: 支持 `libra clone --sparse` 工作区 cone 物化与 skip-worktree 初始化（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 ADR-CL-03 为 `CloneArgs` 新增 `--sparse`（并同步更新 `clone.rs:220` 帮助文本移除 `NOT --sparse (declined D10)`），局部修订 D10（同步 `COMPATIBILITY.md` 与 `docs/development/commands/_compatibility.md`），在 `src/command/clone.rs`、`src/internal/sparse/mod.rs`（复用既有 `sparse_view` 与索引 skip-worktree，无 SQLite schema 变更）初始化 `core.sparseCheckout=true`、cone 规则、工作区顶层文件物化及 skip-worktree 索引位；在 `src/command/status.rs:563` 仅为 `status` 跳过 `warn_sparse_checkout_unsupported_once`（保留 `add.rs:532`/`commit.rs:3017`/`diff.rs:851` 的既有告警，不改变 `add`/`commit`/`diff` 行为）；并在 `clone.rs` 对 `--sparse --filter=...` 组合暂时 `fail-closed` 拒绝（直至 `CL-20` 解禁，避免发布未完成的稀疏部分克隆组合）。（G-09：本卡拆出 `CL-20`、`CL-26`、`CL-27`。）
**Out of scope:** 未适配命令 `fail-closed` 护栏（`CL-26`）；`--sparse --filter=blob:none` 组合及目录重命名合并 lazy fetch（`CL-20`）；`checkout`/`switch`/`reset` 全量 cone 过滤扩展（`DEFER-CL-04`）；独立 `libra sparse-checkout` 子命令（永久非目标）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` `CloneArgs` 缺少 `--sparse` 标志且 `clone.rs:220` 帮助仍写 `NOT --sparse`；`status.rs:563` 调用 `warn_sparse_checkout_unsupported_once`（`add.rs:532`/`commit.rs:3017`/`diff.rs:851` 仍保留）；`sparse_view.rs` 为只读视图且 D10 记于 `_compatibility.md:175-197` | `src/command/clone.rs:220-235`；`src/command/status.rs:563,1248-1265`；`src/internal/sparse/mod.rs:1`；`docs/development/commands/_compatibility.md:175-197`；`COMPATIBILITY.md:166,172,375` |

**Acceptance criteria:**
- [ ] `libra clone --sparse <url> <dir>` 把 `core.sparseCheckout` 设为 `true`。
- [ ] 同一命令写入 cone 模式规则。
- [ ] `clone.rs:220` 的帮助文本不再包含 `NOT --sparse`。
- [ ] 工作区只物化顶层文件。
- [ ] 被排除的子目录索引条目带有 skip-worktree 位。
- [ ] `libra status` 不再输出 `core.sparseCheckout=true is not supported`。
- [ ] `libra add` 仍输出既有 sparse 告警。
- [ ] 在本卡自己的发布提交上，`libra clone --sparse --filter=blob:none` 退出非零。`CL-20` 发布后这条拒绝不再要求在最终树上成立。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件已更新并逐文件验收（不计入 G-03）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_sparse_cone_materializes_top_level` `(new，含 --sparse --filter fail-closed 断言)`
- [ ] `source .env.test && cargo nextest run --test command_test status_test::`

**Full-suite trigger:** `T-1: 修改共享稀疏核心 src/internal/sparse/mod.rs`
**Dependencies:** `CL-27`、`DEP-CL-11`（协调 `src/command/status.rs` 共享写集）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/internal/sparse/mod.rs`, `src/command/status.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/status.md`, `docs/commands/zh-CN/status.md`, `docs/development/commands/status.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/status.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/internal/sparse/mod.rs`, `src/command/status.rs`, `tests/command/clone_test.rs`, `docs/development/commands/_compatibility.md`
**Docs and compatibility impact:** 同步 `docs/commands/{clone,status}.md` EN+zh+dev、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/{clone,status}.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --sparse cone 物化与 status 适配; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=CL-27,DEP-CL-11; writeset=串行于 CL-27; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-20: 实现 `clone --sparse` 部分克隆与目录重命名合并按需 lazy fetch（#756）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CL-16` 拆出：在 `src/command/clone.rs` 解禁 `--sparse --filter=blob:none` 组合并在 `src/cli.rs` `command_preflight` 只为目录重命名合并解开 `core.sparseCheckout=true` 护栏，内容修改、删除和冲突合并继续拒绝，同时在 `src/command/merge.rs`、`src/command/merge/rename_merge.rs`、`src/command/merge/tree_merge.rs` 与 `src/command/fetch.rs` 中新增批量多 OID `fetch_objects_batch` 预取 helper 并打通 `clone --sparse --filter=blob:none`（cone 模式）及目录重命名合并时的单次批量按需 lazy fetch（对应 Git 2.56.0 `TC-0734` 行为：`1 fetch` 取回缺失的 `6 objects`），确保稀疏工作区合并时不会遗漏重命名目标文件。
**Out of scope:** `--sparse` 基础 cone 初始化与 skip-worktree（`CL-16`）；`checkout`/`switch`/`reset` 护栏（`CL-26`/`DEFER-CL-04`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#756` 部分克隆结合 `--sparse` 进行目录重命名合并时需按需批量补全被折叠/重命名的树与 blob（`1 fetch` / `6 objects`）；目录重命名合并逻辑位于 `rename_merge.rs` 与 `tree_merge.rs` | `src/command/clone.rs:220-235`；`src/cli.rs:1755`；`src/command/merge.rs`；`src/command/merge/rename_merge.rs`；`src/command/merge/tree_merge.rs` |

**Acceptance criteria:**
- [ ] `libra clone --sparse --filter=blob:none` 不再被 fail-closed 拒绝。
- [ ] 稀疏仓库中的目录重命名合并不再被 `command_preflight` 拒绝。
- [ ] 该目录重命名合并只发起 1 次 fetch。
- [ ] 该次 fetch 取回 6 个对象。
- [ ] 重命名目标文件出现在合并结果中。
- [ ] 非稀疏部分克隆合并缺失对象时按需获取并成功合并。
- [ ] 离线模式下这条 lazy fetch 被拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收（不计入 G-03）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_sparse_partial_merge_lazy_fetch clone_test::test_non_sparse_partial_merge_fetches_missing_objects` `(new；另断言对象齐全的普通仓库不发起多余 fetch)`
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_sparse_cone_materializes_top_level clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash`（改写过渡期断言：`--sparse --filter=blob:none` 与稀疏目录重命名 `merge` 不再被拒绝，其余护栏断言保留）
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_sparse_merge_rejected_cases_preserve_state`（new；逐项覆盖内容修改、删除、冲突合并均退出非零，并比较拒绝前后工作区、索引和 refs 不变）
- [ ] `source .env.test && cargo nextest run --test command_test merge_test::`

**Full-suite trigger:** `T-1: 修改共享传输 src/command/fetch.rs、合并引擎 rename_merge.rs/tree_merge.rs 与 src/cli.rs`
**Dependencies:** `CL-16`、`DEP-CL-04`、`DEP-CL-07`（只核验 `merge.rs` 没有在途写者；本卡先合入，不等待 `MP-17`）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/cli.rs`, `src/command/merge.rs`, `src/command/merge/rename_merge.rs`, `src/command/merge/tree_merge.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `src/cli.rs`, `src/command/merge.rs`, `src/command/merge/rename_merge.rs`, `src/command/merge/tree_merge.rs`, `src/command/fetch.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/{clone,merge}.md` EN+zh、`docs/development/commands/{clone,merge}.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/{clone,merge}.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clone --sparse 部分克隆与目录重命名合并; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=3; prod-files=6; scope=M; deps=CL-16,DEP-CL-04,DEP-CL-07; writeset=串行于 CL-16; release=independent; split-from=CL-16; exception=N/A`

---

### Task CL-17: 确保默认 `libra clone` 正确持久化 `remote.<name>.fetch`（#876）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 修复默认 `libra clone` 未写入标准 `remote.<name>.fetch=+refs/heads/*:refs/remotes/<name>/*` 配置的缺口（当前仅 `--single-branch` 在 `clone.rs:2043-2054` 写入，`remote add` 已在 `remote.rs:822-834` 写入；`--revision` 裸克隆按 `CL-05` 例外不写入），避免后续向 `remote.<name>.fetch` 追加 refspec 后 `fetch --prune` 发生越界修剪。
**Out of scope:** `fetch --prune` 端到端（`#541` 与 `FP-15` 未就绪）。该场景只登记在 `DEFER-CL-03`，本卡不预置 `#[ignore]` 测试，也不把启用该测试拆成另一张实现卡。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#876` `remote add` 已写 `remote.<name>.fetch`，但 `clone.rs:2043-2054` 仅在 `--single-branch` 写入而默认 `clone` 遗漏 | `src/command/clone.rs:2043-2054`；`src/command/remote.rs:822-834` |

**Acceptance criteria:**
- [ ] 默认 `libra clone`（及 `--branch`）持久化标准 `remote.<remote_name>.fetch=+refs/heads/*:refs/remotes/<remote_name>/*` 配置条目。
- [ ] `clone --bare --revision` 保持 `CL-05` 约定不写 `remote.<remote_name>.fetch`。
- [ ] 向 `remote.<name>.fetch` 追加第二个 refspec 后，原默认条目仍在。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列文件均已更新并逐文件验收。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test clone_test::test_clone_persists_remote_fetch_refspec` `(new)`
- [ ] `source .env.test && cargo nextest run --test command_test remote_test::`

**Full-suite trigger:** `none`
**Dependencies:** `CL-20`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `tests/command/clone_test.rs`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clone.rs`, `tests/command/clone_test.rs`
**Docs and compatibility impact:** 同步 `docs/commands/clone.md` EN+zh、`docs/development/commands/clone.md`、`COMPATIBILITY.md` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`（写前核验 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=默认 clone 写 remote.<name>.fetch; recovery=推送前 revert，发布后前滚新 patch+降级指引; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CL-20; writeset=串行于 CL-20; release=independent; split-from=N/A; exception=N/A`

---

### Task CL-22: 已撤回（不得开工）

**Status:** superseded。`#876` 的 `fetch --prune` 端到端不在本计划单独发版。配置持久化由 `CL-17` 交付；依赖 `#541` 与 `FP-15` 的端到端只记在 `DEFER-CL-03`。本编号不再拥有写集、验收标准或发布边界。

---


## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | CLI 参数重写、`--filter` 语法解析与 `combine:` 构造 | `source .env.test && cargo nextest run --lib cli::tests::index_pack_progress_rewrite`、`source .env.test && cargo nextest run --test command_test clone_test::test_clone_filter_promisor_config_and_combine` |
| 集成 | `clone` 标志、硬链接、`.git` 回退、alternates、部分克隆、`--sparse`、稀疏护栏、`remote.<name>.fetch` | `source .env.test && cargo nextest run --test command_test clone_test::` |
| 兼容 | 浅克隆完整性、配置默认覆盖、兼容账本与命令矩阵对齐 | `source .env.test && cargo nextest run --test compat_clone_shallow_integrity --test compat_config_defaults_semantics --test compat_ledger_schema --test compat_matrix_alignment` |
| 迁移 | `N/A`（无 SQLite schema 变更） | `N/A` |
| 安全 | 无效 `sparse:` filter 拒绝、失败清理新建目录 | `source .env.test && cargo nextest run --test command_test clone_test::test_clone_rejects_invalid_sparse_filter` |
| 性能 | `-l/--local` 同文件系统硬链接复用 inode | `source .env.test && cargo nextest run --test command_test clone_test::test_clone_local_hardlinks_objects_on_same_fs` |
| live/gated | 收口阶段双环境全量套件 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| CL-01 | `#737` | `src/cli.rs`, `src/command/clone.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::`, `lib cli::tests` |
| CL-02 | `#739` | `src/command/clone.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-03 | `#738` | `src/command/clone.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-04 | `#740` | `src/command/clone.rs` | `clone.md` EN+zh+dev, `_compatibility.md`, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-23 | `#740` | `src/utils/util.rs`, `src/command/alternates.rs` | `status`/`log`/`cat-file`/`branch`/`clone` EN+zh+dev+网站, `_compatibility.md`, `COMPATIBILITY.md` | `clone_test::test_clone_separate_git_dir_pointer_commands`, `status_test::` |
| CL-28 | `#740` | `src/cli.rs` | 本卡 `Implementation write set` 中的全部命令文档，外加 `_compatibility.md` 全变体拒绝表与 `COMPATIBILITY.md` | `clone_test::test_clone_separate_git_dir_default_deny` |
| CL-25 | `#740` | `REL-CL-01` 发布面（`Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`） | 随版本发布说明同步 | `command_test clone_test::`, `T-2` 全量 |
| CL-05 | `#741` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::`, `compat_clone_shallow_integrity` |
| CL-06 | `#752` | `src/command/clone.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::`, `compat_config_defaults_semantics` |
| CL-08 | `#743` | `src/command/clone.rs`, `local_client.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-07 | `#742` | `src/command/clone.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-09 | `#744` | `src/command/clone.rs`, `local_client.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-10 | `#751` | `src/command/clone.rs`, `fetch.rs`, `local_client.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-18 | `#751` | `src/command/clone.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-19 | `#754` | `src/utils/storage/local.rs`, `object_ext.rs`, `fetch.rs`, `local_client.rs`, `cat_file.rs` | `{clone,cat-file}.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::`, `cat_file_test::` |
| CL-12 | `#748` | `src/command/clone.rs`, `local_client.rs`, `protocol/mod.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-13 | `#746, #754` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs`, `local_client.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-24 | `#746, #754` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs`, `local_client.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-29 | `#748`；`PM-03`/`t5616#33` | `src/command/clone.rs`, `fetch.rs`, `protocol/mod.rs`, `local_client.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::test_clone_sparse_oid_filter_and_missing_boundary` |
| CL-21 | `#754` | `src/command/clone.rs`, `fetch.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::`, `cat_file_test::` |
| CL-14 | `#750` | `src/command/clone.rs`, `rev_parse.rs` | `{clone,rev-parse}.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::`, `rev_parse_test::` |
| CL-15 | `#753` | `src/command/clone.rs` | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::` |
| CL-26 | `#756` | `src/cli.rs` | `checkout`/`switch`/`reset`/`restore` EN+zh+dev+website, `_compatibility.md`, `COMPATIBILITY.md` | `clone_test::test_sparse_preflight_checkout_switch_reset_restore` |
| CL-27 | `#756` | `src/cli.rs` | `merge`/`pull`/`rebase`/`cherry-pick`/`stash` EN+zh+dev+website, `COMPATIBILITY.md` | `clone_test::test_sparse_preflight_merge_pull_rebase_cherry_pick_stash` |
| CL-16 | `#756` | `src/command/clone.rs`, `src/internal/sparse/mod.rs`, `src/command/status.rs` | `{clone,status}.md` EN+zh+dev, `_compatibility.md`, `COMPATIBILITY.md`, website | `command_test clone_test::`, `status_test::` |
| CL-20 | `#756` | `src/command/clone.rs`, `src/cli.rs`, `merge.rs`, `rename_merge.rs`, `tree_merge.rs`, `fetch.rs` | `{clone,merge}.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::`, `merge_test::` |
| CL-17 | `#876` | `src/command/clone.rs`（`remote add` 已写于 `remote.rs:822-834`） | `clone.md` EN+zh+dev, `COMPATIBILITY.md`, website | `command_test clone_test::`, `remote_test::` |
| CL-22 | 已撤回 | 无写集 | 无 | 端到端见 `DEFER-CL-03` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 基线锚点核对、Codex 计划单审 PASS | 本计划文件与 review log | 修改计划文档 |
| M1 | Phase 1（`CL-01..CL-04, CL-23, CL-28, CL-25, CL-05..CL-06`）全部完成并发布 | 各独立卡/`REL-CL-01` release tag + nextest focused/T-1/T-2 全量绿 | 发布后前滚新 patch + 降级指引 |
| M2 | Phase 2（5 张卡）全部完成并发布 | 各卡 release tag + nextest focused/T-1 全量绿 | 发布后前滚新 patch + 降级指引 |
| M3 | Phase 3（13 张活动卡，不含已撤回的 `CL-22`）全部完成，全量收口门全绿。`DEFER-CL-03` 不计入这 13 张 | 全量 nextest 证据写入本计划 | 前滚修复新开 `FIX-*` 卡 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `--separate-git-dir` / `--revision` / 无效 `--filter` 失败 | 无残留 | 删除新建的目标目录与 gitdir | 残留半成品 `.libra` 目录 |
| `--dissociate` 拷贝中断 | 未完成的新目录 | 失败回滚清理新建目录，不损坏源 `--reference` | 修改或删除源 `--reference` 仓库 |
| D 组 CI/release 失败 | 已推送主分支提交 | 按 ER-04/ER-10 前滚修复发新 patch | 回退/强推已发布历史 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `src/cli.rs` 全局 `--progress` 预处理影响其它子命令 | 中：可能误改非 `clone` 命令参数 | 仅针对 `clone`（与既有 `index-pack`）触发重写并跑 `T-1` 全量 | `CL-01` |
| 部分克隆 `--filter` 与远端能力协商不匹配 | 中：旧远端不支持 `filter` | 按 Git 约定在远端未广告 `filter` 时安全回退全量克隆并提示 | `CL-24` |
| 稀疏护栏漏掉某个未适配命令 | 中：`core.sparseCheckout=true` 时命令仍改写工作区 | `CL-27` 先给 `merge`/`pull`/`rebase`/`cherry-pick`/`stash` 过渡期拒绝。`CL-20` 只解禁目录重命名合并，其它 `merge` 继续拒绝。`pull`/`rebase`/`cherry-pick`/`stash` 继续受 `CL-27` 保护。`CL-26` 继续保护检出类四命令 | `CL-26`, `CL-27`, `CL-20` |
| 新增子命令漏出默认拒绝 | 中：新的 `Commands` 变体可能写错目录 | `CL-28` 的夹具必须覆盖每一个变体；不在放行名单中的变体退出非零且不改数据 | `CL-28` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `-l/--local` 硬链接 | `O(objects + fallback bytes)` | 硬链接成功的对象无需复制字节；回退复制的对象按其字节数占用额外空间 | 成功硬链接对象在同一文件系统共享 inode；失败时允许复制回退 | `CL-08` |
| `--filter=blob:none` | 过滤协商成功时处理 `O(commits + trees)` 个对象，传输和存储另计选中对象字节数；全量回退时随完整对象集及字节数增长 | 协商成功时初始不传输 blob，按需 lazy fetch；远端未广告 filter 时回退全量传输并保存完整对象集 | 依远端能力与回退对象集确定 | `CL-13, CL-19, CL-24` |

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
| R31 (Codex gpt-6.1-terra) | `plan-20261117-clone.md` | FAIL | P1: 各卡网站写集仍使用 `../libra-backend`，与唯一 `/Volumes/Sea/libra-backend` 目标冲突；已统一全部 284 处路径。该轮未完成全文复审，R32 不得据此推定其它维度通过 | R32 完整复审 | Codex R31 只读评审（部分覆盖） |
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

## 实施证据汇总

> 按 v2.12 规定，未来执行与收口证据统一汇总于本节，不在计划目录新建独立证据文件。

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-CL-01 | Git `files` 松散 ref 与 `reftable` 二进制 ref 存储后端实现（含 `#750` `files` 后端断言） | 当前仓库 refs 存储于既有 SQLite（`libra.db`），`CL-14` 如实报告 `sqlite` | 独立规划多 ref 存储后端（`files`/`reftable`）计划时 | 后续多 ref 后端计划（Owner: `CLI / Storage 维护者`） |
| DEFER-CL-02 | `libra init --separate-git-dir`（D25），以及把 `CL-28` 已拒绝的子命令改造成真正的指针布局 | D25 保留 `init` 拒绝。`CL-28` 已经默认拒绝未放行子命令，所以发布后它们不会写错目录；真正可用要等后续改造 | 后续计划逐命令放开指针布局时 | 后续对应计划 |
| DEFER-CL-03 | `#876` 在追加 refspec 后跑 `fetch --prune` 的端到端 | 依赖外部 `#541` 与 `plan-20261117-fetch-push.md#FP-15`（`DEP-CL-05`）。本计划不预置 `#[ignore]` 测试，也不为此单开发版 | `#541` 与 `FP-15` 都进入 `main` 后，由后续计划补该端到端 | 后续收口计划 |
| DEFER-CL-04 | `checkout`/`switch`/`reset` 等工作区写入命令全量支持 `clone --sparse` cone 过滤 | `CL-16`/`CL-20` 仅覆盖 `clone --sparse` 物化、`status.rs` 告警消除与 `merge` lazy fetch | 后续扩展工作区稀疏检出全命令兼容时 | 后续对应计划 |

## 完成判据

- [ ] 所有任务卡满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务的 `Lifecycle=done` 且 `Acceptance=complete`。
- [ ] 所有任务的 `Verification` 命令已运行并记录结果。`CL-12` 与 `CL-13` 的「远端未广告 filter 则退出非零」，以及 `CL-13` 对 `combine:*` 的拒绝，只保留在各自发布提交的证据里。`CL-24` 发布后改写这些测试。最终树以 `CL-24` 的回退全量契约为准。`CL-27` 对稀疏 `merge` 的拒绝和 `CL-16` 对 `--sparse --filter=blob:none` 的拒绝同样只保留各自发布提交的证据。最终树只把目录重命名合并从稀疏 `merge` 拒绝中放开，其它 `merge` 仍拒绝。`CL-12`「其它未实现 `--filter` 退出非零」只保留该卡发布提交的证据。
- [ ] 收口阶段全量门通过：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`（按 ER-13：无论暴露本计划缺陷或既有失败，均须定位修复或附基线复现证据并登记 `FIX-*`/`DEFER-*` 后重跑全量至全绿，不得带着红色全量结果通过完成门）。
- [ ] `#876` 的 `fetch --prune` 端到端留在 `DEFER-CL-03`，不阻塞本计划活动卡完成。
- [ ] 所有任务卡 `Rollback mode`（`immutable-release`）与恢复验证已核验。
- [ ] 必要的 docs / compat / test-index 更新已完成，且未在 `docs/development/plan/` 新建独立证据文件（v2.12 规则）。
- [ ] 每个相关发布切片均已完成 `/Volumes/Sea/libra-backend` `cf` 的网站文档 sync/check、相关前端验证与覆盖最终网站变更的 Claude Code `VERDICT: PASS`（独立于计划 Codex 单审）；独立卡已提交推送 `cf` 并核验 Cloudflare 部署与生产正文，家族子卡的本地 `cf` 提交已由 `CL-25` 统一推送并完成同等部署核验。
- [ ] Codex review 最终结论为 `VERDICT: PASS`，P0/P1 全部关闭（按用户 2026-11-17 指令，计划提交门不再要求 Claude Review）。
- [ ] 所有独立卡已完成 `patch + 1`、版本面同步与 `gh release create` 发布。
- [ ] 「修订历史」与 `plan-status.md` 已同步。
