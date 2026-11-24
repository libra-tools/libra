# Plumbing 与杂项命令 Git 兼容性修复计划（2026-11-17）

**模板版本:** `v2.16`。本计划不在 v2.16 具名 `code-only` 范围内，按普通发布合同执行：独立 `implementation`/`migration` 卡各自执行完整 C 组（含 `patch + 1`、`compat_version_surface_sync` 版本面同步、工具链刷新 `Cargo.lock`、`gh release create`）与适用 D 组；`family child` 不独立 bump、推送或发布，由其家族唯一的 `release` 发布点统一执行完整 C/D 并向子卡提供覆盖。

**状态:** Claude R40 与 Codex R40 双审 PASS（受审 SHA-256 前 12 位 `b9fbc2619f28`），计划可提交；全部任务 pending，尚未执行（落地时每张卡必须先按 ER-02 刷新源码锚点）。

**审阅合同:** 按用户 2026-11-18 指令，本计划的计划文本提交门为 Claude（Subagent）与 Codex 双审，二者对同一受审字节（同一 SHA-256）均给出 `VERDICT: PASS` 方可提交（取代 2026-11-17「Codex 单审、不再要求 Claude Review」的口径）。执行期各卡的 ER-05 代码 review 与网站交付的独立 Claude Code `PASS` 门不受本条影响，与计划文本提交门互不替代。

**跟踪范围:** GitHub open issues #711、#717、#747、#758、#839、#848、#947、#988、#989、#995、#1017、#1020、#1022、#1029、#1033、#1034、#1035（共 17 项）。其中 #1020 整体延后（`DEFER-PM-04`，维持 `issues/498.md` 的 `ADR-TT-01`），其余 16 项由 40 张活动卡承接；各 issue 的闭环程度见「成功定义」。

## 文档职责

- **网站交付路径（权威）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支），依据是用户 2026-11-18 11:55:09 UTC 确认：本批计划的网站文档交付检出为该路径（记为 `DEP-PM-12`，已满足）；该路径同时是模板 ER-06a 默认的 Libra checkout 同级 `../libra-backend`。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`，该路径在本机不存在（2026-11-18 核对），属已知文档滞后；用户决定本批不改 `AGENTS.md`，本计划以上述确认为准。修订历史 2026-11-18 08:19:08 行所称「用户最新指令」不作为依据，以本条确认为准。2026-11-18 核对：`/Volumes/Data/gitmono/libra-backend` 为 Libra 检出（有 `.libra/`、无 `.git`），`libra --offline symbolic-ref --short HEAD` 输出 `cf`。写入前须按「文档与网站验收命令」第 2 步核验；不满足即 STOP，禁止改用其它检出。

本文解决 Libra 底层 plumbing 与辅助命令（`symbolic-ref`、`config`、`rev-list`、`replace`、`rev-parse`、`blame`、`notes`、新增 `var`、`ls-files`、`cat-file`、`check-attr`、`shortlog`、`check-ignore`、`check-mailmap`）相对于 Git（基线 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`，v2.56.0-116）的 17 项已登记兼容缺口，目标是实现 Git 兼容行为、完备的回归测试与四端文档（EN、zh-CN、开发文档、网站）同步。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收（GC-01）。

### 适用范围

- 源码与生产文件（由全部活动卡的 `Implementation write set` 汇总，按路径排序）：`Cargo.toml`、`sql/migrations/2026111702_symbolic_ref.sql`、`sql/migrations/README.md`、`src/cli.rs`、`src/command/blame.rs`、`src/command/cat_file.rs`、`src/command/check_attr.rs`、`src/command/check_ignore.rs`、`src/command/check_mailmap.rs`、`src/command/checkout.rs`、`src/command/commit.rs`、`src/command/config.rs`、`src/command/ls_files.rs`、`src/command/mod.rs`、`src/command/notes.rs`、`src/command/replace.rs`、`src/command/rev_list.rs`、`src/command/rev_list_output.rs`、`src/command/rev_parse.rs`、`src/command/shortlog.rs`、`src/command/symbolic_ref.rs`、`src/command/var.rs`、`src/internal/branch.rs`、`src/internal/db/migration.rs`、`src/internal/db/schema.rs`、`src/internal/head.rs`、`src/internal/notes.rs`、`src/utils/attributes.rs`、`src/utils/util.rs`；另含 ER-08 版本面集合（Release write set）。`Cargo.toml` 只在 `PM-07` 改 `similar` 版本要求；不改 `sql/sqlite_20260309_init.sql`。
- 测试（同样由各卡写集汇总；`tests/command/*_test.rs` 均属 `command_test` target）：`.config/nextest.toml`、`tests/SERIAL_REGISTRY.tsv`、`tests/command/blame_test.rs`、`tests/command/cat_file_test.rs`、`tests/command/check_attr_test.rs`、`tests/command/check_ignore_test.rs`、`tests/command/check_mailmap_test.rs`、`tests/command/config_test.rs`、`tests/command/ls_files_test.rs`、`tests/command/mod.rs`、`tests/command/notes_test.rs`、`tests/command/replace_test.rs`、`tests/command/rev_list_test.rs`、`tests/command/rev_parse_test.rs`、`tests/command/shortlog_test.rs`、`tests/command/symbolic_ref_test.rs`、`tests/command/var_test.rs`、`tests/compat/help_examples_banner.rs`、`tests/db_migration_test.rs`；只读回归 target `compat_conflict_status_diff`、`compat_global_config_schema_future`、`compat_help_flag_descriptions`、`compat_command_docs_examples_section`、`compat_matrix_alignment`、`compat_serial_registry`、`compat_version_surface_sync`。
- 文档：各命令 `docs/commands/<cmd>.md`、`docs/commands/zh-CN/<cmd>.md`、`docs/development/commands/<cmd>.md`、`docs/development/commands/_compatibility.md`（`PM-01`）、`docs/development/commands/README.md` 与 `docs/commands/README.md`（`PM-10`）、`COMPATIBILITY.md`（各命令行原地修改；`PM-10` 新增 `var` 行）及网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`（`PM-10` 另含 `meta.en.json`、`index.en.mdx`）。

### 非目标

- 不重构 `merge`/`pull`/`fetch`/`push`/`clone` 主命令工作流（由协调者排期的其它计划承接）。
- 除 `PM-01`（`#711` 按 `ADR-PM-03` 新增 Repository 迁移，GC-13）外，其余任务卡不修改 SQLite schema 或迁移 ledger。
- 不实现 `tag` 对 blob/tree/tag 对象打标签（`#1020`，`DEFER-PM-04`：会产生 Libra `clone` 无法传输的仓库，维持 `ADR-TT-01`）。
- 不改变 Libra 分页器对 `GIT_PAGER`/`core.pager`/`PAGER` 的处理（`DEFER-PM-05`）、不新增系统级属性文件（`DEFER-PM-06`）、不新增全局 `-c` 选项（`DEFER-PM-08`）。

### 成功定义

- 16 项 issue 的计划内范围全部由活动卡交付并通过各卡验收；可完全闭环（全部验收条目由本计划卡覆盖）的 issue：#711（`PM-01`、`PM-21a`、`PM-32`）、#747（`PM-03`、`PM-33`，后者依赖 `DEP-PM-08`）、#839（`PM-05`）、#947（`PM-06`）、#848（`PM-07`）、#989（`PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39`、`PM-24`；`--quit` 为 Libra 扩展）、#1017（`PM-11`）、#1022（`PM-12`）、#1029（`PM-14`）、#1033（`PM-15`、`PM-27`、`PM-28`、`PM-18`）、#1034（`PM-16`、`PM-30`、`PM-19`、`PM-40`、`PM-35`）、#1035（`PM-17`、`PM-31`、`PM-20`、`PM-18`）。
- 只部分闭环、保持 open 的 issue（剩余子项由对应 `DEFER-*` 承接）：#717（`PM-02`；`checkout --orphan` 子项 `DEFER-PM-03`）、#758（`PM-04`、`PM-26`；`t6050` 其余阻断 `DEFER-PM-07`）、#988（`PM-08`、`PM-34`；全局 `-c` 与 `--ref` 展开 `DEFER-PM-08`）、#995（`PM-10`、`PM-10a`、`PM-37`、`PM-22`、`PM-36`、`PM-25`；`GIT_PAGER` 的 Git 优先级 `DEFER-PM-05`）。#1020 不闭环（`DEFER-PM-04`）。
- 每张非延后卡 `Lifecycle=done` 且 `Acceptance=complete`，计划级全量收口门全绿（见「完成判据」）。

## 使用规则

- 本批次并行起草的多份计划由协调者统一串行登记至 [`plan-status.md`](plan-status.md) 与 `plan-long.md`；执行期推进任何任务卡时，同一变更必须同步更新 `plan-status.md` 的「计划一览」行（状态、版本或 commit、`YYYY-MM-DD HH:MM:SS UTC`）。
- 不得在 `docs/development/plan/` 或仓库其它位置新建证据目录或证据文件（v2.12）；验收摘要、计数、SHA-256、run ID、门结果写入本文「实施证据汇总」或对应卡。
- 状态推进与「修订历史」统一使用 `YYYY-MM-DD HH:MM:SS UTC`（v2.11）。
- 网站文档只在 `DEP-PM-12` 指定的检出交付，步骤见「文档与网站验收命令」。
- 本计划继承 [plan-template.md](plan-template.md) 的 `GC-01..13`、`ER-01..14`、`G-01..11`。

## 事实基线

> 所有行号和源码锚点核对于 `2026-11-18`（源码版本 `0.30.210`）；开工当天必须按 ER-02 刷新。标注「Git 2.54.0 预探」的取值来自评审时本机 Git 2.54.0 在同构夹具上的观测，执行时按「验证执行协议」第 4 项以 pinned Git 复核。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| CLI 注册 | 13 个既有子命令已注册；`Commands::Var` 不存在；新命令登记点为 `enum Commands`、Command Groups 行、`command_scope` 与分派 | `src/cli.rs:65,344,2166-2201,3492` |
| 符号引用与 ref 存储 | `symbolic-ref` 只接受 `HEAD`；`reference.kind` 限 `Branch`/`Tag`/`Head`；远端 HEAD 为带 `remote` 的 `Head` 行 | `src/command/symbolic_ref.rs:97-113,152-162`；`sql/sqlite_20260309_init.sql:19-29`；`src/internal/head.rs:436-466` |
| 迁移 | Repository 迁移尖端 `2026103101`；`I1048-05` 占用 `2026111701` | `src/internal/db/migration.rs:2464-2470,3189-3199`；`docs/development/plan/issues/1048.md` |
| 配置与对象查询 | `config --default` 须显式 get；`rev-list` 无 `--missing` 且缺失 blob 被静默输出；`replace` 子串过滤与首错中止；`rev-parse --short` 要求对象存在且无 `--show-object-format`；`cat-file` 单位置参数 | `src/command/config.rs:1963-1968`；`src/command/rev_list.rs:719-781`；`src/command/replace.rs:319-351,389-406,493-512`；`src/command/rev_parse.rs:643-650,874-898`；`src/command/cat_file.rs:174-175` |
| 历史与 notes | `blame` 无 `--minimal`；`notes --ref` 带默认值、忽略 `GIT_NOTES_REF`/`core.notesRef`；manual 合并冲突整体中止、无 `--commit`/`--abort` | `src/command/blame.rs:54,534-546`；`src/command/notes.rs:37-39,137-145,303`；`src/internal/notes.rs:485-526` |
| 索引、属性与忽略 | `ls-files -o` 只排除 stage 0；无标志列表只列 stage 0；属性无 `[attr]` 宏、无 XDG 默认、键名大小写敏感；`check-ignore` 按仓库根解析、读完 stdin 才处理、缺 `--quiet` 规则 | `src/command/ls_files.rs:313-321,390-395`；`src/utils/attributes.rs:974-988,1061-1086`；`src/internal/config.rs:166-177`；`src/command/check_ignore.rs:113-139,195-199,274-308` |
| shortlog | 人读格式与 Git 不同；单值 `--group`；无 `--date`/`-<n>`/mailmap | `src/command/shortlog.rs:153-157,557-607,749-805`；`src/cli.rs:1056-1119` |
| 测试 | 命令测试均属 `command_test`（`tests/command_test.rs` → `tests/command/mod.rs`）；子进程 helper `run_libra_command*` 逐子进程隔离 HOME/XDG/全局库 | `tests/command/mod.rs:353-392,985-1165`；`tests/INDEX.md:44` |
| 外部参照 | Git 基准 | `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（`v2.56.0-116`，核对日期 2026-11-18；本机 Git 2.54.0 用于预探） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-PM-01 | `symbolic-ref` 只支持 `HEAD`；`config --default` 无隐式读取；无 `var` 命令 | `t2018#2`、`t2017#6`、`t7614`/`t7006` 脚本失败 | `#711, #717, #995` | PM-01, PM-21a, PM-32, PM-02, PM-10, PM-10a, PM-37, PM-22, PM-36, PM-25 |
| GAP-PM-02 | `rev-list --missing`、`replace` 列表/删除、`rev-parse --short`/`--show-object-format`、`cat-file <type> <object>` 偏差 | 部分克隆诊断、`t6050`、`t4034`、`t0001`、`t7604` 失败 | `#747, #758, #839, #947, #1029` | PM-03, PM-33, PM-04, PM-26, PM-05, PM-06, PM-14 |
| GAP-PM-03 | `blame --minimal`、notes 默认 ref、notes 手工合并状态机、shortlog 格式/分组/mailmap 缺失 | `t8015`、`t3310`、`t4201`/`t4203` 失败 | `#848, #988, #989, #1034` | PM-07, PM-08, PM-34, PM-09, PM-29, PM-38, PM-23, PM-39, PM-24, PM-16, PM-30, PM-19, PM-40, PM-35 |
| GAP-PM-04 | `ls-files` 冲突条目、属性宏/全局来源/名字校验/次序、`check-ignore` 路径/流式/quiet 偏差；`tag` 拒绝非 commit 对象 | `t6422`、`t6423`、`t0003`、`t1306`、`t0008` 失败 | `#1017, #1022, #1033, #1035, #1020` | PM-11, PM-12, PM-15, PM-27, PM-28, PM-18, PM-17, PM-31, PM-20；#1020 → `DEFER-PM-04` |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-20261117-clone.md` | `CL-14` 同写 `src/command/rev_parse.rs`；`CL-13`/`CL-19`/`CL-29` 提供部分克隆、promisor 与 `t5616#33` 夹具（对方 `CL-29` 写作「供 `PM-03`」）；`CL-19` 同写 `cat_file.rs` | `DEP-PM-02`（`PM-06` 先于 `CL-14`）、`DEP-PM-08`（`CL-13`/`CL-19`/`CL-29` 先于 `PM-33`，`CL-19` 先于 `PM-14`）。`t5616#33` 的验收已于 R20 自 `PM-03` 拆入 `PM-33`，对方所称「供 `PM-03`」即指 `PM-33`；`PM-03` 本身不再等待 clone 计划 |
| `plan-20261117-merge-pull.md` | 其 Phase 4「重命名与 D/F 冲突引擎修复」改变冲突时写入索引的 stage 条目（2026-11-18 工作区快照中 Phase 4 为 `MP-16`、`MP-17`、`MP-24`、`MP-18`、`MP-25`、`MP-46`、`MP-50`、`MP-19`、`MP-54`、`MP-20`、`MP-47` 共 11 张卡；该计划尚在评审，提交版本的卡集若不同，本计划按 ER-03 修订）；该计划不写 `src/command/ls_files.rs`（其 `MP-25` 已避开 `ls-files -o`，`#1017` 由本计划 `PM-11` 承接），其关系表写明「对方等待本计划 Phase 4 结束」 | `DEP-PM-03`（上列 11 张具名卡先于 `PM-11`/`PM-12`，单向；本计划与其无共享写集，只是顺序前置） |
| `plan-20261117-global-init-infra.md` | `GII-12` 同写 `rev_parse.rs`（`GII-04` 也写，在对方严格串行链上先于 `GII-12`）；`GII-23` 写 `src/command/checkout.rs` 与 checkout 文档/网站页、`GII-21` 写 checkout 文档/网站页（均与 `PM-21a` 相交）；对方 `DEP-GII-03` 要求与同批在途卡相交的写集取得协调者串行窗口，并把网站交付（含第 4 步改写的被忽略生成产物）视为与所有同批计划相交 | `DEP-PM-10`（`GII-12` 先于 `PM-05`，因而也晚于 `GII-04`）；checkout 文件经 `DEP-PM-15` 写锁互斥；网站交付经 `DEP-PM-16` 串行窗口；`src/cli.rs`、`src/utils/util.rs` 经 `DEP-PM-13`，`COMPATIBILITY.md` 经 `DEP-PM-11` |
| `plan-20261117-branch.md`、`plan-20261117-worktree-clean-reset-apply.md` | `BR-10`/`BR-22` 与 `WC-10` 写 `src/internal/branch.rs` | `DEP-PM-06`：`BR-10`/`BR-22` → `WC-10` → `PM-21a`。worktree 计划 `DEP-WC-01` 与关系表引用的「`PM-21a`（`REL-PM-21` 家族首卡）」：`PM-21a` 仍是本计划写 `branch.rs` 的唯一卡，顺序一致；`REL-PM-21` 家族已于 2026-11-18 撤回（`PM-21a` 改为独立发布卡，见修订历史），该处「家族」标注已过时但不影响顺序。映射：对方 `DEP-WC-01`、关系表与 `DEP-WC-01` 所称「`REL-PM-21` 家族」「`REL-PM-21` 家族首卡」一律指本计划独立卡 `PM-21a`（`independent`，自身执行完整 C/D 组，无家族发布点）；其「解除 `PM-21a` 阻塞」即解除 `PM-21a` 的开工阻塞；对方关系表注所称「`DEP-PM-06` 文字仍写拆分前的 `PM-21`」已过时，本计划 `DEP-PM-06` 现写 `PM-21a`。对方 `DEP-WC-06` 消费的 `PM-03` 交付四种 `--missing` 动作（以不读取对象内容的存在性判定报告缺失）；部分克隆上 `allow-promisor` 的 promisor 归因与「不触发按需拉取」的验收在本计划链末的 `PM-33`（等待 `DEP-PM-08`）。对方 `DEP-WC-07` 所列 `PM-10`/`PM-19`（`src/cli.rs`）在本计划经 `DEP-PM-13` 取锁。branch 计划 `DEP-BR-12` 所称 `PM-21` 即 `PM-21a` |
| `plan-20261117-checkout-switch-restore.md` | 该计划（尚在评审，按 ID 引用、以其提交版本为准）大量卡写 `src/command/checkout.rs`、`docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md`、网站 `checkout.en.md` 与 `COMPATIBILITY.md` 的 `checkout` 行（2026-11-18 工作区快照中为 `CSR-02`、`CSR-27`、`CSR-27a`、`CSR-03`–`CSR-20`、`CSR-28`、`CSR-29`、`CSR-30`、`CSR-33`、`CSR-24`、`CSR-10a`、`CSR-15a` 等）；本计划 `PM-21a` 同写这些文件 | `DEP-PM-15`：两计划写这些文件的卡互斥（独占写锁，不设先后顺序边）；`COMPATIBILITY.md` 另经 `DEP-PM-11`；`PM-21a` 开工时按 ER-02 刷新 `checkout.rs` 锚点（`:567`、`:625-640` 可能已被对方改动） |
| `plan-20261117-commit-signing.md` | `DEP-CS-04` 等待 `PM-02`（`config.rs` 与 config 文档）、`PM-09`（`notes.rs`）、`PM-10`（`src/cli.rs`、`tests/command/mod.rs`）、`PM-14`（`cat_file.rs`）合入；`DEP-CS-09` 为经本计划 `DEP-PM-05` 到 `PM-02` 的说明性传递前置 | 本计划卡号与写集保持其所依赖的语义：`PM-09` 属 `REL-PM-01` 家族，其改动随 `PM-24` 发布才合入 `main`，且 `PM-29`、`PM-23` 也写 `notes.rs`，故 `CS-19` 实际须等 `PM-24`；`PM-14` 已移至本计划串行链末尾（等待 `DEP-PM-08`）。本计划内到 `PM-02` 的传递链为 `PM-01 → PM-21a → PM-32 → PM-02` |
| `plan-20261117-describe.md` | `DEP-DESC-UTIL` 将 `PM-05`/`PM-06` 列为 `rev_parse_test.rs` 与 rev-parse 文档写者 | 写集不变 |
| `plan-20261117-grep.md` | `DEP-GR-06` 等待 `PM-15` 与 `PM-18` 后才写 `src/utils/attributes.rs` | 本计划 `attributes.rs` 写者为 `PM-15` → `PM-28` → `PM-18`（串行），`PM-18` 是最后一个，故该等待覆盖新增的 `PM-28`（`DEP-PM-09`） |
| `plan-20261117-sequencer-am.md` | `DEP-SA-04` 等待 `PM-10` 对 `tests/command/mod.rs` 的修改合入 | 写集不变 |
| `docs/development/plan/issues/1048.md` | `I1048-05` 占用迁移版本 `2026111701` | `DEP-PM-05` |
| `docs/development/plan/issues/498.md` | `ADR-TT-01` 拒绝非 commit 目标打标签；`DEFER-TT-01`/`DEFER-TT-05` 为传输侧缺陷 | 维持；#1020 → `DEFER-PM-04` |
| `docs/development/plan/issues/541.md` | `update-ref` 非 `refs/heads/*` 支持 | `t6050` 的 `update-ref refs/replace/*` 阻断记入 `DEFER-PM-07` |
| `plan-long.md`、`plan-status.md` | 长期路线图与执行视图 | 由协调者登记；执行期按「使用规则」同步 |
| `docs/development/plan/plan-template.md` | 模板 v2.16 | 全文遵守 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 17 个实测缺口均有 Git 复现路径；#1020 因传输阻断延后 | 40 张活动卡；`PM-13a`/`PM-13b`/`REL-PM-13` 撤回并记 `DEFER-PM-04` |
| 可行性 | 每卡单一行为轴，夹具在 `command_test`/`db_migration_test` 内自建 | 新增用例逐条精确过滤 |
| 任务卡粒度 | 按 G-03 独立谓词计数（不同语句、选项、错误条件分条），失败路径不变量提升到卡字段，AC 均 ≤ 8 | 拆出 `PM-26`..`PM-40` |
| 依赖与顺序 | 全串行链，跨计划边全部登记为 `DEP-*` 且单向 | 见「实施顺序」 |
| 完整性 | 实现、测试、EN/zh/开发/网站文档与 `COMPATIBILITY.md` 同卡 | 「文档与网站验收命令」 |
| 安全性 | `var -l` 不回显配置；路径越界 fail-closed | ADR-PM-01；`PM-17` |
| 功能正确性 | 以 pinned Git 为准，Libra 有意差异逐卡登记 | 各卡 `Out of scope` 与 `COMPATIBILITY.md` |
| 接口兼容 | `PM-10` 新命令、`PM-16` 人读格式变更有 ADR；`--json` 不变 | ADR-PM-01、ADR-PM-04 |
| 数据流与控制流 | 符号引用互斥由触发器强制；notes 合并以 CAS 幂等提交 | ADR-PM-03、ADR-PM-02 |
| 性能与容量 | `check-ignore --stdin` 逐行流式；宏展开非递归 | `PM-31`、`PM-15` |
| 可靠性与容错 | 迁移 forward-only；notes 合并崩溃窗口可重试 | 故障恢复矩阵 |
| 可维护性 | 复用 `commit` 身份解析、`editor`、`check_mailmap`、属性引擎单一事实源 | GC-02 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 03:30:00 UTC | 初稿创建 | 建立覆盖 17 个 plumbing/杂项兼容问题的 17 张任务卡计划 | N/A | 全文 |
| 2026-11-17 03:58:00 UTC | Claude R1 FAIL + Codex R1 FAIL | `PM-01` 收窄为 `HEAD` 与 `refs/remotes/<remote>/HEAD` 并纳入 `head.rs`；`PM-03` 补全 `allow-any`/`allow-promisor`；`DEP-PM-02` 改为单向 `outgoing`（`PM-06` 先合入，消除环路）；`PM-09` 纳入 `internal/notes.rs` 并对齐 `NOTES_MERGE_PARTIAL`/`NOTES_MERGE_REF` 与 GC-08 崩溃恢复；`PM-10` 修正 `2/3`、补 `T-4`/`INDEX.md`/`VAR_EXAMPLES`/`compat_matrix_alignment`；`PM-13` 明确非 commit 附注标签与 `--points-at`/`--contains`；按 G-01/G-09 拆出 `PM-18`/`PM-19`/`PM-20` 并修正 `shortlog.rs:157,190` 基线；补全全部 20 卡精确 `file:line` 锚点 | PM-15 → PM-15, PM-18; PM-16 → PM-16, PM-19; PM-17 → PM-17, PM-20 | ADR-PM-02、依赖表、实施顺序、Phase 2/3、审计表、PM-01..PM-20、追溯表、完成判据、review log |
| 2026-11-17 04:15:00 UTC | Claude R2 FAIL + Codex R2 FAIL | `PM-01` 与 `PM-09` 补标 `T-1`（共享 `head.rs`/`notes.rs`）；新增 `ADR-PM-03` 覆盖 `ADR-PL-08` 并通过 `sql/sqlite_20261117_symbolic_ref.sql`（GC-13）支持 `#711` `refs/heads/a-branch` 与 `-d`；重写 `ADR-PM-02`/`PM-09` 对齐 SQLite `notes` 表事务与 GC-08 崩溃恢复；`PM-16` 明确重复 `--group` 各自贡献与 `--no-group` 重置；`M3` 补 `PM-11..PM-20`；`Release write set 默认` 去重为 4 项；`PM-10` 补 `GIT_ATTR_SYSTEM`/`GLOBAL`/`PAGER`、3 个帮助/文档守卫并重算 `AC`；`PM-15/18/17` 收窄至剩余缺口；`PM-03` 定义 `allow-promisor`；补 `DEP-PM-03/04` 与具名发布者 | N/A | 修订历史、ADR-PM-02/03、依赖表、全局默认、PM-01/03/09/10/15/16/17/18、M3、review log |
| 2026-11-17 04:22:00 UTC | Codex R3 FAIL + Claude R3 网络中断 | `PM-01` 补齐 GC-13 迁移注册表/role/`schema_versions` ledger/bootstrap parity 与故障注入测试并修正 `适用范围`/`非目标`/`测试矩阵` 矛盾；`PM-03` `--missing=allow-promisor` 改为校验预期 promisor 对象来源；`ADR-PM-02`/`PM-09` 增加已提交未清理崩溃窗口的持久完成标记与幂等恢复 | N/A | 适用范围、非目标、ADR-PM-02/03、PM-01/03/09、测试矩阵、review log |
| 2026-11-17 04:32:00 UTC | Claude R4 FAIL + Codex R4 FAIL | `PM-01` 拆分为 `PM-01`（`migration`/`forward-only`，`sql/migrations/2026111701_symbolic_ref.sql` 独立 `symbolic_ref` 表 + `migration.rs::repository_migrations()` + `Repository` 隔离测试与 `UnsupportedFuture` 声明）与 `PM-21`（`symbolic-ref` + `branch.rs` + `t2018#2`）；`PM-10` 拆出 `PM-22` 保持 `AC <= 8`；`PM-09` 改用同事务 SQLite 回执并补崩溃窗口具名测试与 `manual` 兼容性说明；补齐 `PM-13` AC、`PM-15` 去 `--cached`、`PM-19` `check_mailmap.rs`、`默认覆盖`、`DEP-PM-03`、`测试矩阵` 与 `性能` 行 | PM-01 → PM-01, PM-21; PM-10 → PM-10, PM-22 | 全文多处、审计表、默认覆盖、PM-01/09/10/13/15/19/21/22、追溯表、review log |
| 2026-11-17 04:43:00 UTC | Claude R5 FAIL + Codex R5 FAIL | `PM-01` 改用 `2026111702_symbolic_ref.sql` 避开 `issues/1048.md` 的 `2026111701`、遵循 migrations-only 原则不改 `sqlite_20260309_init.sql`（改更新 `sql/migrations/README.md` 与 `tests/db_migration_test.rs`）、标 `T-1, T-3` 与 `(new)` 测试；修复 `PM-21` 自依赖为 `PM-01` 并记入 `DEFER-PM-02`（其余 ref 读路径）；将 `VAR_EXAMPLES`/Command Groups/3 个文档守卫迁入 `PM-10`（`PM-22` 承接剩余 6 个变量）确保 `PM-10` 可独立发布；`PM-09` 明确以 `config` 表 `notes.merge.last_commit_receipt` 键作同事务回执；补 `DEP-PM-05/06/07`、`Phase 3` `PM-22`、`PM-11` Granularity、`PM-15` 标题与 `PM-02` `compat_config_defaults_semantics` | N/A | 适用范围、ADR-PM-02/03、依赖表、Phase 3、默认覆盖、审计表、PM-01/21/02/09/10/22/11/13/14/15、非目标、review log |
| 2026-11-17 04:49:00 UTC | Codex R6 FAIL | `ADR-PM-02` 与 `PM-09` 改用按次生成唯一 UUID `merge_id` 与工作区作用域 `config` 键 `notes.merge.receipt.<worktree_id>.<merge_id>`（消除跨次/跨工作区冲突并增补并发恢复测试）；`Phase 1` 退出条件补齐 `PM-21` | N/A | ADR-PM-02、Phase 1、PM-09、review log |
| 2026-11-17 04:56:00 UTC | Codex R7 FAIL + Claude R6 网络中断 | 修复 `ADR-PM-02` 与 `PM-09` 清理顺序为“先删 `NOTES_MERGE_WORKTREE`/`NOTES_MERGE_REF` → 再删 DB 回执 `notes.merge.receipt.<worktree_id>.<merge_id>` → 最后删 `NOTES_MERGE_STATE`”，消除孤立 DB 回执与全路径崩溃窗口；收紧 `PM-21` 标题与描述仅声明 `symbolic-ref`（任意合法符号引用读写与 `-d`）及 `checkout -f`（`Branch::find_branch_result`），不宣称 `show-ref`/`for-each-ref`/`branch` 列表读取（显式记入 `DEFER-PM-02` 与 `COMPATIBILITY.md`） | N/A | 修订历史、ADR-PM-02、PM-21、PM-09、review log |
| 2026-11-17 05:01:00 UTC | Codex R8 FAIL | 在 `ADR-PM-02` 与 `PM-09` 增加 `NOTES_MERGE_STATE` 原子持久化 `phase=pending\|committed`（提交事务后先原子重写 `phase=committed` 再清理），明确 `--quit` 同样按 DB 回执/`phase=committed` 同步清理回执或收尾（绝不留下孤立 DB 行），且 `phase=pending` 且无回执时若 `NOTES_MERGE_WORKTREE` 缺失或损坏则 fail-closed 报错并补充对应测试 | N/A | 修订历史、ADR-PM-02、PM-09、review log |
| 2026-11-17 05:12:00 UTC | Claude R9 FAIL + Codex R9b FAIL | 修正 `ADR-PM-02`/`PM-09` `--quit` 在 `phase=pending`+DB 回执窗口必须先原子写 `phase=committed` 再删回执并补崩溃注入测试；`DEP-PM-05` 显式建立 `I1048-05 -> PM-01` 有向边与合并重编号；新增 `DEP-PM-08`（`CL-13, CL-19 -> PM-03`）覆盖 `#747` `t5616#33`/`TC-0625`；`PM-05` 对齐 `#839` 缩写未入库 40-hex OID；`PM-19` 标 `T-1` 并纳入 `check_mailmap` 文档与测试；`PM-08` 纳入 `src/internal/notes.rs` 与 `T-1` 并补 `get-ref`/`-c`；补齐 `#717`/`#758`/`#989`/`#995`/`#1020`/`#1029` 子项与延后说明；修正 `R10` 状态行、时间正序、`codeql.yml` `release/**` 与 `DEP-PM-03/04/06/07` 顺序边 | N/A | 状态行、修订历史、ADR-PM-02、D 组、依赖表、审计表、PM-02/03/04/05/08/09/10/13/14/19、DEFER-PM-03、review log |
| 2026-11-17 05:25:00 UTC | Claude R10 FAIL + Codex R10 FAIL | `ADR-PM-02` 增加 `base_notes_ref_oid` 事务内 CAS 并发更新检查与测试；将 `PM-09` 拆为 `PM-09`（`--commit` 事务+CAS+崩溃恢复，`6/8 AC`）与 `PM-23`（`--abort`/`--quit` 与互斥，`5/8 AC`）；将 `GIT_COMMITTER_*` 环境变量覆盖移入 `PM-22`（`7/8 AC`）并精算 `PM-10`（`7/8 AC`）；移除造成 `PM-06`/`CL-14` 环路的 `DEP-PM-04` 并从 `PM-09/13/14` 移除 `outgoing` `DEP-PM-07`；`PM-01` 写集补 `src/internal/db/schema.rs` 并说明 `2026111601` 属 capture DB；同步 `codeql.yml` 全局默认、`PM-03/13/19` 审计计数与 `DEFER-PM-02/03` 承接 | PM-09 → PM-09, PM-23 | 状态行、成功定义、修订历史、ADR-PM-02、DAG、依赖表、发布分组、Phase 2、全局默认、审计表、PM-01/03/09/23/10/22/13/14/19、追溯表、M2、DEFER-PM-02/03、review log |
| 2026-11-17 05:28:00 UTC | Codex R11 FAIL + Claude R11 网络中断 | 将 `ADR-PM-02` 与 `PM-09` 的 CAS 基线从不存在的 commit-backed notes tree OID 改为基于 SQLite `notes` 表扁平行的规范摘要 `base_notes_digest`（在冲突物化时与 `notes merge --commit` 的 `BEGIN IMMEDIATE` 事务内对 `SELECT object, blob FROM notes WHERE notes_ref = ? ORDER BY object` 计算 SHA-256，因所有 notes 写路径均直接变更该表行，无需改动其余写路径即可捕获任意并发 `notes add/remove/append/copy/prune` 更新并补并发回归测试）；同步 `PM-21` `AC=5/8` | N/A | 状态行、修订历史、ADR-PM-02、审计表、PM-21、PM-09、review log |
| 2026-11-17 05:33:00 UTC | Codex R12 FAIL + Claude R12 网络中断 | 修复 `ADR-PM-02` 与 `PM-09` `notes merge` 读取目标 notes 与计算 `base_notes_digest` 的快照竞态：强制在**同一 SQLite `BEGIN` 读事务快照**内一次性读取目标 `notes_ref` 全部 `(object, blob)` 行，同时用于冲突物化与 `base_notes_digest` 计算，消除读后写窗口并补并发回归测试；更新「评审结论与修订记录」为 23 张卡 | N/A | 状态行、评审结论、修订历史、ADR-PM-02、PM-09、review log |
| 2026-11-17 05:38:00 UTC | Codex R13 FAIL | 修正 `PM-18` `core.attributesFile` 与 XDG 回退语义：显式配置 `core.attributesFile` 时仅加载该文件、不叠加加载 XDG 默认路径，未配置时回退至 `$XDG_CONFIG_HOME/git/attributes`（默认 `~/.config/git/attributes`），并补充显式配置不加载 XDG 的回归断言 | N/A | 状态行、修订历史、PM-18、review log |
| 2026-11-17 05:41:00 UTC | Codex R14 FAIL | 统一 `PM-01` 迁移后的发布后恢复指引：在 `Rollback mode 默认`、`PM-21` 与 `M1` 中明确已迁移至 `2026111702` 的仓库禁止降级回 `PM-01` 之前旧二进制（`UnsupportedFuture`），`PM-21` 及后续卡仅能前滚 `patch + 1`（或回退至不早于 `PM-01` 的同 schema 窗口二进制）；补充 `ADR-PM-02` 与 `PM-09` 对混合冲突/非冲突条目的保留与同事务提交验收及回归测试 | N/A | 状态行、修订历史、ADR-PM-02、全局默认、PM-21、PM-09、M1、review log |
| 2026-11-17 06:59:00 UTC | Claude R15 FAIL + Codex R15 FAIL | `PM-09`+`PM-23` 纳为 `REL-PM-01` 家族卡并新增 `Task type: release` 发布点 `PM-24`；`PM-01` 补 `COMPATIBILITY.md`/`_compatibility.md` 降级不兼容与恢复文档并核实 `AC=6/8`；`PM-15`/`PM-18` 纳入 `src/utils/attributes.rs`、`T-1` 与 `DEP-PM-09`；`DEP-PM-06` 将 `WC-10` 改为 `outgoing`（`PM-21 -> WC-10`）消除环路；`DEP-PM-08` 明确 `CL-19 -> PM-03` 及 `CL-19 -> PM-14` 无环；`PM-19` 纳入 `src/cli.rs` 与 `T-1`；`PM-10` 补 `docs/development/commands/README.md` 与 website 索引文件；新增 `DEP-PM-10`（`GII-12 -> PM-05`）；`PM-22` 拆出 `PM-25` 守 `AC<=8`；同步 P2 锚点/测试名/审计表/风险/故障恢复/review log | PM-22 → PM-22, PM-25; 新增 PM-24 | 全文、状态行、适用范围、DAG、依赖表、发布分组、Phase 2/3、审计表、PM-01/10/14/15/18/19/20/21/22/23/24/25、追溯表、M2/M3、风险/恢复、review log |
| 2026-11-17 12:01:37 UTC | Clone Codex R30 跨计划一致性修复 | `DEP-PM-08` 与 `PM-03` 增列 `CL-29`，由该卡先交付 `sparse:oid` 与 `t5616#33` 夹具，再验收 `rev-list --missing`；保持 `CL-19 -> PM-03` 与 `CL-19 -> PM-14` 无环，并按用户指令改为 Codex 单审提交门 | N/A | 状态行、成功定义、依赖表、`PM-03`、M0、完成判据 |
| 2026-11-17 12:27:28 UTC | Branch Codex R49-B 跨计划顺序修复 | `DEP-PM-06` 与 `DEP-BR-12`/`DEP-WC-01` 统一为 `BR-10`/`BR-22` → `WC-10` → `PM-21`，消除两份计划相反等待边 | N/A | 状态行、依赖表、Review log |
| 2026-11-17 17:00:50 UTC | 机械自审：网站交付合同 | 现行网站路径统一 `/Volumes/Data/gitmono/libra-backend`；补 checkout 校验、按卡同步、前端验证、最终字节 Claude PASS、签名推送部署和家族发布顺序 | N/A | 使用规则、现行网站写集 |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件；`PM-01` 迁移注册拆为两项独立验收；`PM-10`/`PM-22`/`PM-25` 验收拆分；`PM-17` 流式与内存验收固定 2 秒首行输出、有效 128 字节路径×10,000/1,000,000 与 RSS 增长上限；`PM-19` 改为整个 revision walk 总数上限；`PM-09`/`PM-23` 补工作树逐文件与逐级目录 fsync 屏障、`--quit` 删除顺序与跨命令交错验证；`PM-21a`..`PM-21c` 全量触发改由 `REL-PM-21` 的 T-2 统一执行；`REL-PM-13` 精确测试名补 `command::` 前缀 | N/A | 全文对应章节 |
| 2026-11-18 09:43:54 UTC | Codex 复审 FAIL 修复（验证契约与门禁）〔时间更正：本行原记 `2026-11-18 10:35:00 UTC`，晚于文件实际写入时刻（文件 mtime `2026-11-18 09:43:54 UTC`），2026-11-18 R19 按 mtime 如实更正；本行所述「文档与网站验收命令」块当时未落入文件，R19 补齐〕 | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 13:56:55 UTC | 用户 2026-11-18 双审指令与 2026-11-18 11:55:09 UTC 网站路径确认 | 计划文本提交门由「Codex 单审、不再要求 Claude Review」改为 Claude（Subagent）与 Codex 对同一 SHA-256 双审 PASS（头部「审阅合同」、M0、完成判据）；网站路径依据改为用户 2026-11-18 11:55:09 UTC 确认并登记 `DEP-PM-12`（已满足）与 `AGENTS.md:135` 已知滞后，删除无出处的「用户最新指令」依据；修订历史中原记 `2026-11-18 10:35:00 UTC`（未来时间）一行按文件 mtime 更正为 `09:43:54`，并把误置于 ADR-PM-03 之后的三行修订记录移回本表 | N/A | 头部、文档职责、使用规则、依赖登记表、里程碑、完成判据、修订历史 |
| 2026-11-18 13:56:55 UTC | Claude R19 预审重构（批次「反复出现的 Codex P1」六类清单清扫，逐卡以源码、pinned Git 源码与本机 Git 2.54.0 实测重核） | (1) ADR-PM-03 改由 `PM-01` 迁移内触发器强制符号引用与实体分支互斥，撤回逐写入口改造卡与 `REL-PM-21` 家族，`PM-21a` 改为 `independent`，`DEP-PM-06` 改指 `PM-21a`；(2) ADR-PM-02 重写为「状态文件 + 前值/目标值 CAS 幂等提交」，删除 DB 回执、`phase`、`CREATING` 标记与逐文件 fsync 协议，`notes merge --quit` 明示为 Libra 扩展（Git 2.56 无此选项）；(3) 新增 ADR-PM-04（`shortlog` 人读输出改 Git 格式）与 ADR-PM-05（`#1020` 维持 `ADR-TT-01`，`DEFER-PM-04`）；(4) 按 G-03 独立谓词重写全部 AC 与映射，失败路径不变量提升为卡字段与「验证执行协议」第 4 项的全量快照定义，失败类 AC 只断言稳定码；(5) 补齐此前被引用但不存在的「文档与网站验收命令」与 W-1/W-2 命令块，W-1 限定顶层命令表；(6) 全部 `Verification` 改为精确 `test(=…)` 新增用例与带预检的共享回归组，修正损坏的引号与「模块过滤恰 1 匹配」矛盾；(7) 依据实测更正事实：`#839` 只有不在库的全长 OID 失败、`cat-file HEAD` 退出 129、`config --default` 与 `--list`/`--remove-section` 同用的现状、`ls-files`/`tag`/`notes`/`check-*`/`shortlog` 锚点；(8) 新增 `DEP-PM-11`（`COMPATIBILITY.md` 写锁）、`DEP-PM-12`、`DEP-PM-13`（`src/cli.rs` 等共享文件写锁），`DEP-PM-07` 对齐各接收方计划引用 | `PM-21b1`、`PM-21b2`、`PM-21b3`、`PM-21c`、`REL-PM-21` → 撤回；`PM-13a`、`PM-13b`、`REL-PM-13` → 撤回（`DEFER-PM-04`）；`PM-04` → `PM-04`、`PM-26`；`PM-09` → `PM-09`、`PM-29`（`PM-23` 保留）；`PM-10a` 改承身份覆盖（含原 `PM-22` 的提交者覆盖），`PM-22` 改承编辑器变量，`PM-25` 改承分页器/默认分支/属性变量与 `-l` 全表并移到 `PM-18` 之后；`PM-15` → `PM-15`、`PM-27`、`PM-28`；`PM-16` → `PM-16`、`PM-30`（`PM-19` 保留）；`PM-17` → `PM-17`、`PM-31`（`PM-20` 保留）；`PM-18` 扩至忽略来源 | 全文：ADR、执行检查、实施顺序、依赖登记表、发布分组、Phase、默认与撤回表、审计表、全部任务卡、测试矩阵、追溯表、里程碑、恢复矩阵、风险、性能、兼容收口、延后项、完成判据 |
| 2026-11-18 14:11:02 UTC | Claude R19 FAIL + Codex R19 A/B/C/D FAIL（受审 SHA-256 `c9b7ae198d60…`） | 依评审修订：(1) 按 G-03「不同语句/选项/取值/错误条件分条」重数，拆出 `PM-32`（远端 HEAD 符号引用，自 `PM-21a`）、`PM-33`（部分克隆上的 `--missing`，自 `PM-03`，承接 `DEP-PM-08`）、`PM-35`（shortlog 格式分组与 `--date`，自 `PM-30`）、`PM-36`（`GIT_SEQUENCE_EDITOR`，自 `PM-22`），`PM-01`、`PM-04`、`PM-06`、`PM-07`、`PM-10`、`PM-27` 改为逐谓词 AC，`PM-27` 规模改 `M`；(2) `PM-01` 列全 6 处尖端断言并加 `schema.rs` 测试写集，文档门给出 `PM-01` 专用第 1 步规则、第 8 项改为只执行第 1 步；(3) ADR-PM-02 增加 `NOTES_MERGE_LOCK` 排他互斥及 `PM-09`/`PM-29`/`PM-23` 锁用例，`PM-09` 保持 `LBR-CONFLICT-002` 并改写两个既有 notes 合并用例，`PM-29` 崩溃窗口夹具同时保存/恢复两条状态路径，`PM-23` 的 `--quit` 以与 `--abort` 结果相等断言；(4) `DEP-PM-11` 改为家族级写锁；第 1 步对新建文件记 absent；性能摘要收窄到可观测判据；关系表按 commit-signing 现行 `DEP-CS-09` 与 clone `CL-29` 改写；(5) 夹具：`PM-11`/`PM-12`/`PM-14`/`PM-03`/`PM-04`/`PM-26`/`PM-07` 改用 `init_repo_via_cli` 新建仓库，`.gitignore` 改为未跟踪，`PM-26` 以不在库 OID 命名的替换引用区分「解析成功但无替换」，`PM-33` 以完整源仓库对象流为期望，`PM-08` 增加全局回退与本地优先，`PM-30` 增加跨分组提交内去重，`PM-16` 增加 `--json` 基准比对，`PM-20` 改写既有 `z_uses_nul_delimiters`，`PM-10a`/`PM-22`/`PM-25`/`PM-27`/`PM-28` 夹具内联（G-05）；(6) 锚点更正：`symbolic_ref.rs:193-203`、`notes.rs:866-870`、`local_client.rs:631-670`、`ls_files.rs:313-321`、依赖 crate 路径改为 `$CARGO_HOME/registry/...`；`PM-03` 存在性检查改用 `exist_checked_many` | `PM-21a` → `PM-21a`、`PM-32`；`PM-03` → `PM-03`、`PM-33`；`PM-30` → `PM-30`、`PM-35`；`PM-22` → `PM-22`、`PM-36`；`PM-14` 与 `PM-33` 移至串行链末尾 | 全文：执行检查、实施顺序、依赖登记表、发布窗口、Phase、默认与撤回表、审计表、受影响任务卡、测试矩阵、追溯表、恢复矩阵、风险、性能、完成判据、review log |
| 2026-11-18 14:28:35 UTC | Claude R20 FAIL + Codex R20 A/B/C/D FAIL（受审 SHA-256 `4624b50b42f2…`） | 依评审修订：(1) 按 G-09 自 `PM-08` 拆出 `PM-34`（`GIT_NOTES_REF` 环境变量），`PM-08` 收窄为 `core.notesRef` 级联（本地 → 全局 → 系统，新增系统级与显式 `--ref` 优先的验收）；(2) G-03：`PM-01` AC7 改为单一集合相等（含 `Capture` 角色），删除合并两库两表的原 AC8；`PM-24` 只计三条祖先谓词，C/D、ER-08 与网站第 2–7 步改列不计分子的「强制门」（VER 2/12）；`PM-09` AC4 改为完整状态结构相等；(3) 「notes 快照」纳入 `id`（`AUTOINCREMENT`），`PM-08`/`PM-34` AC2 与 `PM-29` AC1/AC3 改为「去掉新增行后相等且新增行取值如期」；(4) `DEP-PM-11`/`DEP-PM-13`/`DEP-PM-09` 只以持锁者为排他障碍，等待者按 `plan-status.md` 等待条目的时间先后取得；(5) shortlog：`PM-16` 明确 stdin 模式同改 Git 格式并新增 stdin 验收、`--json` 字段与条目次序不变（改用 F16o 基准）、补全钉住旧格式的既有用例；`PM-30` 文档只写本卡功能；`PM-19` AC3 改用能区分 `-n` 的 F16o `-ns -4`；`PM-35` 的 `%aN`/`%cN` 改为经 mailmap 映射（与 Git 一致，新增两条 mailmap 验收）并移到 `PM-19` 之后；(6) `PM-14` AC7 改为附注标签剥离 `cat-file commit v1`，单参数拒绝移入 Out of scope；`PM-10a` 精确期望补 `\n`；测试矩阵迁移用例名更正；ADR-PM-03/ADR-PM-05、`DEP-PM-05`/`DEP-PM-07` 与 R20 串行链同步；`PM-29`/`PM-23` 锁失败的不变量含锁文件保持；`PM-18` 期望路径先规范化 | `PM-08` → `PM-08`、`PM-34`；`PM-35` 移到 `PM-19` 之后 | 实施顺序、依赖登记表、发布窗口、Phase 2/3、默认覆盖表、任务卡粒度审计表、ADR-PM-03/04/05、验证执行协议第 4 项、受影响任务卡、测试矩阵、追溯表、完成判据、review log |
| 2026-11-18 14:36:39 UTC | Claude R21 FAIL + Codex R21 A/B/C/D FAIL（受审 SHA-256 `493600f5bfdc…`） | 依评审修订：(1) `PM-01` 按 GC-13 恢复隔离配置库验收（`ConfigDbFixture`、`#[serial(env)]`，`tests/SERIAL_REGISTRY.tsv` 与 `.config/nextest.toml` 进入写集并纳入 `DEP-PM-13`），以「配置库隔离快照」单谓词取代角色集合 AC，并按 G-11 新增预置同名视图的失败注入 AC，用例改为 `symbolic_ref_migration_upgrade_contract` 与 `symbolic_ref_triggers_enforce_exclusivity`；(2) ADR-PM-02 与故障恢复矩阵三行补「持锁期间崩溃同时残留陈旧 `NOTES_MERGE_LOCK`，先确认无进程并手工删除」；(3) `PM-23` 改为 `--commit`/`--abort`/`--quit` 两两互斥且不得与 `-s` 或 `<notes-ref>` 同用，`-s` 与 `<notes-ref>` 同用的策略合并不变；`PM-29` AC2 与 `PM-23` AC1 改为「notes 合并状态快照」等于 `(absent, absent)` 的单谓词；`PM-29` 夹具注明 AC6 不执行合并；(4) `PM-24` 新增 release note 验收（AC4–AC8、`--notes-file`、发布后正文核对）与兼容证据门，提交后以 `libra diff -s --exit-code HEAD -- <版本面>` 证明受测字节即提交字节；(5) `PM-25` 的 `GIT_PAGER` 明确为启用分页时启动的命令（同 Git `git_pager(…, 1)`），AC8 固定作者与提交者日期；(6) `PM-33` 新增源可达时输出不变与本地对象清单不变的验收，证明不按需拉取；(7) mailmap 夹具注明 `.mailmap` 不提交，F16o 写全作者身份；「验证执行协议」第 6 项说明非 nextest 门同为两门 | N/A | 依赖登记表（`DEP-PM-13`）、实施顺序、默认覆盖表、ADR-PM-02、验证执行协议第 6 项、`PM-01`、`PM-16`、`PM-19`、`PM-23`、`PM-24`、`PM-25`、`PM-29`、`PM-33`、`PM-35`、任务卡粒度审计表、测试矩阵、故障恢复矩阵、review log |
| 2026-11-18 14:45:18 UTC | Claude R22 PASS + Codex R22 A/B/C/D FAIL（受审 SHA-256 `541d114b7e7c…`） | 依评审修订：(1)「验证执行协议」第 3、5 项与全部 Verification 改为「运行 = 通过 = 1（或 N）、失败 = 0」，nextest 的 skipped 计数含被过滤排除的用例，只记录不作判据，并禁止卡特有用例以 `#[ignore]` 或环境门控绕过断言；(2) 开头活动卡数改由生成值填写（{本版 37 张}）；(3) `PM-01` 失败注入改为先以 `run_pending_up_to` 应用到 `2026111702` 的紧邻前一版本（预期 `I1048-05` 的 `2026111701`）再注入，避免前序迁移的独立事务提交破坏快照；(4) `PM-24` 把三条祖先检查列为 Verification 门（VER 8/12）；(5) `PM-28` 登记次序改按 Git 读取次序（根 → `info/attributes` → 子目录），新增 F28i 与 AC4；(6) 按 G-09 重划 `var` 身份卡：`PM-10` 只以配置身份与当前时刻交付（文档注明尚未接入环境变量），`PM-10a` 改为作者侧（提取 `commit.rs` 作者专用入口，只校验作者侧日期，含交叉日期隔离验收，`src/command/commit.rs` 纳入写集与 `DEP-PM-13`），新增 `PM-37` 承接提交者侧（`create_committer_signature`）；ADR-PM-01 同步 | `PM-10a` → `PM-10a`、`PM-37` | 文档职责、验证执行协议第 3/5 项、全部卡 Verification 措辞、ADR-PM-01、依赖登记表（`DEP-PM-13`）、实施顺序、发布窗口、Phase 2、`PM-01`、`PM-10`、`PM-10a`、`PM-37`、`PM-24`、`PM-28`、审计表、追溯表、完成判据、review log |
| 2026-11-18 14:48:53 UTC | Claude R23 PASS + Codex R23 A/B/D FAIL、C PASS（受审 SHA-256 `546f780cd617…`） | 依评审修订：(1)「验证执行协议」第 6 项与全部失败类映射改为以「退出码非 0」为前提再断言 stderr 稳定码（与成功类 `exit 0` 前提对称）；(2)「文档与网站验收命令」第 6 步的远端前进恢复区分父提交已有页面（`restore`）与新建页面（`PM-10` 的 `var.en.md`：核对受审哈希后删除未跟踪文件）；(3) `PM-01` AC2 映射要求关闭并重开连接后再次 `run_pending`，回归门合并为 `--test db_migration_test --test compat_serial_registry`（VER 仍 8/8），第 7 项登记 `PM-01` 为唯一 `#[serial(env)]` 例外；(4) `PM-31`、`PM-20` 夹具：需 stdin 的非流式调用改经 `run_libra_command_with_stdin_and_env`，流式用例的 `base_libra_command` 同样设置三个隔离环境变量 | N/A | 验证执行协议第 6/7 项、文档与网站验收命令第 6 步、全部卡 AC 映射说明、`PM-01`、`PM-20`、`PM-31`、review log |
| 2026-11-18 14:56:57 UTC | Claude R24 PASS + Codex R24 A/B/C FAIL、D PASS（受审 SHA-256 `b42908c6a2a3…`） | 依评审修订：(1) ADR-PM-02 改为「提交开始标记 `NOTES_MERGE_COMMITTING` 下只确认不重放」：首次提交先建标记，崩溃后重试只在各行仍为目标值时清理成功，否则 `LBR-CONFLICT-002` 拒绝，杜绝覆盖崩溃后他人的写入；按 G-09 自 `PM-29` 拆出 `PM-38`（重试语义与三个崩溃场景验收），`PM-29` 保留首次提交与错误路径，`PM-09` 新增「只残留标记时拒绝新合并」，`PM-23` 的 `--abort` 一并清理标记，「notes 合并状态快照」扩为含标记的三元组，`REL-PM-01` 家族改为四张子卡，`PM-24` 增加 `PM-38` 祖先验收；(2) ER-06a 同卡交付：家族子卡改为同卡写入、评审并在后端 `cf` 本地签名提交 `notes.en.md`（不推送），`PM-24` 复核、一次推送并部署核验，新增 `DEP-PM-14` 后端推送冻结窗口与撤回步骤；(3) `DEP-PM-03` 改为等待 merge-pull 计划 Phase 4 的七张卡（该计划不写 `ls_files.rs`），关系表同步；(4) `PM-01` 按模板「一条命令中的多个 `--test` 分别计门」移除 `--lib` 回归组（单元尖端断言由 T-1/T-3 全量覆盖），VER 8/8；(5) `PM-03` 的 `allow-promisor` 在无 promisor 支持时明确 fail-closed 等同 `error`，promisor 归因实现与「不按需拉取」文档移入 `PM-33` | `PM-29` → `PM-29`、`PM-38` | ADR-PM-02、验证执行协议第 4 项与「文档与网站验收命令」第 8 项、依赖登记表（`DEP-PM-03`、`DEP-PM-11`、`DEP-PM-14`）、实施顺序、发布分组（`REL-PM-01`）、默认覆盖表、`PM-01`、`PM-03`、`PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-24`、`PM-33`、审计表、追溯表、里程碑、故障恢复矩阵、风险登记、完成判据、review log |
| 2026-11-18 14:58:28 UTC | R25 送审前自查（Claude） | Verification 计数改为按模板「一条命令中的多个 `--test` target 分别计」逐 target 计门（`PM-01` 的两 target 回归门计 4 门，VER 8/8），同步「验证执行协议」第 6 项与任务卡粒度审计表说明；因受审字节变化，已发出的 R25 Codex 调用中止、以本版重新送审 | N/A | 验证执行协议第 6 项、任务卡粒度审计表、`PM-01` |
| 2026-11-18 15:03:27 UTC | Claude R25 PASS + Codex R25 A/B/C/D FAIL（受审 SHA-256 `19377e9f9b7e…`） | 依评审修订：(1)「文档与网站验收命令」第 8 项：家族子卡第 3 步改为核对本卡本地签名提交中的 EN 文档字节（不要求已合入 `main`），`PM-24` 以复核方式重跑第 3 步（页面可不变，但须等于最后受审字节），不同才重审并追加本地提交；(2) `PM-01` 明确只有 `symbolic_ref_migration_upgrade_contract` 标 `#[serial(env)]`；(3) 按 G-09 自 `PM-23` 拆出 `PM-39`（控制形式与选项互斥，含 `--abort --quit`、`--abort <ref>`、`--commit <ref>`），`PM-23` 的 `--quit` 与部分残留场景各拆为状态与 notes 两条独立谓词，`REL-PM-01` 改为五张子卡，`PM-24` 增加 `PM-39` 祖先验收；(4) `PM-10`、`PM-10a`、`PM-37` 夹具固定 `TZ=Asia/Shanghai`，身份输出精确断言本地偏移 `+0800`；(5) `PM-33` 的「不按需拉取」改述为可观测承诺（源可达时不取回、源不可达时不报错，经 `exist_checked_many` 不读取对象内容），传输层请求计数登记为尚未排期 | `PM-23` → `PM-23`、`PM-39` | 文档与网站验收命令第 8 项、实施顺序、发布分组（`REL-PM-01`）、默认覆盖表、`DEP-PM-11`、`PM-01`、`PM-10`、`PM-10a`、`PM-37`、`PM-23`、`PM-39`、`PM-24`、`PM-33`、审计表、追溯表、里程碑、风险登记、完成判据、review log |
| 2026-11-18 15:04:03 UTC | R26 送审前自查（Claude） | `PM-24` 的写集串行前驱随家族扩展改为最后一张子卡 `PM-39`（粒度行与审计表同步）；因受审字节变化，已发出的 R26 Codex 调用中止、以本版重新送审 | N/A | `PM-24`、任务卡粒度审计表 |
| 2026-11-18 15:07:53 UTC | Claude R26 PASS + Codex R26 A/B/C/D FAIL（受审 SHA-256 `7ae68654b24b…`） | 依评审修订：(1) ADR-PM-02 与故障恢复矩阵把「建标记后、事务前崩溃」改为条件结论（各行已等于目标值则只清理并成功，否则拒绝），`PM-38` 新增 F38b 与 AC6 覆盖目标值等于前值的确认成功；(2)「文档与网站验收命令」第 8 项把 `PM-24` 复核改为显式分支：哈希相等直接推送，不等则重审并追加本地提交、以新受审哈希为基准；(3) 默认覆盖表的 release note 验收编号更正为 AC6–AC10；(4) `PM-21a` 改为新增只供 `checkout` 使用的 `Branch::find_branch_or_symref_result_with_conn`（`checkout.rs:567` 改调），既有 `Branch::find_branch_result*` 与经其解析的 `rev-parse`/`log` 不变，ADR-PM-03 与风险登记同步；(5) `PM-10a` 改为新增复用私有 helper 的作者入口，不修改 `create_commit_signatures`；(6) `PM-33` 写明 promisor 归因规则并新增 AC6（只被本地新提交引用的缺失对象在 `allow-promisor` 下失败）；(7) `PM-14` AC2 写明 stdout 字节为 `hello\n` | N/A | ADR-PM-02、ADR-PM-03、文档与网站验收命令第 8 项、默认覆盖表、`PM-10a`、`PM-14`、`PM-21a`、`PM-33`、`PM-38`、审计表、故障恢复矩阵、风险登记、review log |
| 2026-11-18 15:08:17 UTC | R27 送审前自查（Claude） | `PM-38` 夹具「每个用例先执行合并并改写解决文件」限定为 AC1–AC5（AC6 使用 F38b）；因受审字节变化，已发出的 R27 Codex 调用中止、以本版重新送审 | N/A | `PM-38` |
| 2026-11-18 15:14:08 UTC | Claude R27 PASS + Codex R27 A/B/C/D FAIL（受审 SHA-256 `1eb04c9c51b4…`） | 依评审修订：(1)「适用范围」的源码与测试清单改由全部活动卡写集自动汇总（补齐 `checkout.rs`、`commit.rs`、`head.rs`、`schema.rs`、`SERIAL_REGISTRY.tsv` 等），家族子卡列表补全五张；(2) notes 卡的 blob 期望按 Libra 实际字节去掉尾随换行（`-m` 内容无换行，`src/command/notes.rs:612-653`），冲突文件格式写明「某侧内容不以换行结尾时补 `\n`」；(3) `PM-21a` 写明检出符号引用分支时 HEAD 分离（有意差异：Git 附着到链尾引用，Git 2.54.0 预探），新增 AC8 断言 `branch --show-current` 的 `detached`；(4) `PM-38` 新增 AC7（F38b 成功清理前后 notes 快照不变）；(5) `PM-33` 归因规则改述为 Libra 有意差异（不再声称等价于 Git `is_promisor_object`），并以 Git 预探登记差异情形与其当前不可构造的原因；(6) `PM-18` AC5 明确在 AC1 的仓库与全局规则生效状态下执行 | N/A | 适用范围、`PM-08`、`PM-09`、`PM-18`、`PM-21a`、`PM-23`、`PM-29`、`PM-33`、`PM-34`、`PM-38`、`PM-39`、审计表、review log |
| 2026-11-18 15:17:26 UTC | Claude R28 PASS + Codex R28 A/B/C/D FAIL（受审 SHA-256 `015da348a553…`） | 依评审修订：(1) 故障恢复矩阵的「`--commit` 期间他人修改」行收窄为行级 CAS 合同（既非前值也非目标值才拒绝；已是目标值或无关行不构成冲突），与 ADR-PM-02 一致；(2) `PM-03` 夹具为三个文件指定不同内容，并在取基线前断言 blob OID 两两不同、基线恰含各路径行；(3) `PM-24` 祖先门的输入说明补全 `PM39`；(4) `PM-16` 规定人读路径先按新规则排序再依次应用 `--min-count`/`--reverse`/`--top`、JSON 路径整体不变，新增 AC8（F16o 上 `-s --reverse --top 2`） | N/A | `PM-03`、`PM-16`、`PM-24`、审计表、故障恢复矩阵、review log |
| 2026-11-18 15:23:48 UTC | Claude R29 PASS + Codex R29 A/B/C/D FAIL（受审 SHA-256 `ca40bf1c040c…`） | 依评审修订：(1)「配置快照」扩为本地、隔离全局与隔离系统三个库的 `config_kv` 与 `config` 两表（缺失记 absent）；(2) W-2 捕获 `HTTPError` 以退出码 3 报告非 200，并更正退出码说明；(3) `PM-07` 改以端到端区分夹具 F07g（确定性生成器、两版本 SHA-256 断言；LCS 684 行，Git `blame --minimal` 684，现行有界 `Myers` 683）验收 `--minimal`、默认路径、`--no-minimal` 末次生效与 `-w` 分支，删去只测选择函数的单元用例；(4) `PM-38` 以 `notes` 表 `RAISE(ABORT)` 写入探针使「不写任何行」可观测；(5) `PM-29`、`PM-09` 的 Out of scope 把选项互斥指向 `PM-39`；(6) `PM-33` 更正「无法删除远端跟踪引用」的错误前提（`libra remote prune` 可删除），新增 AC7 以 `side` 分支与 `remote prune` 构造并验收 Libra 归因差异 | N/A | 验证执行协议第 4 项、W-2 命令块与第 7 步说明、`PM-07`、`PM-09`、`PM-29`、`PM-33`、`PM-38`、测试矩阵、审计表、review log |
| 2026-11-18 15:26:43 UTC | Claude R30 PASS + Codex R30 A/B/C/D FAIL（受审 SHA-256 `cfea01490e30…`） | 依评审修订：(1) `PM-21a` 把 `checkout` 的 EN/zh、开发文档、网站页与 `COMPATIBILITY.md` `checkout` 行纳入同卡写集与文档验收（符号引用分支检出为分离 HEAD 的有意差异）；(2) `PM-01` 的逐文件文档验收加入 `sql/migrations/README.md`（第 1 步 `PM-01` 规则同步）；(3) `PM-10` 的编辑器承接卡分列 `PM-22`/`PM-36`；(4) `PM-10a`、`PM-37` 明确 `user.useConfigOnly` 只忽略身份环境变量、日期仍生效，各新增 AC8；(5) `PM-27` AC3 改为 stdin 保持打开时 10 秒内以 `LBR-CLI-002` 失败；(6) `PM-18` AC6 期望补 `\n` | N/A | 文档与网站验收命令第 1 步、`PM-01`、`PM-10`、`PM-10a`、`PM-18`、`PM-21a`、`PM-27`、`PM-37`、审计表、review log |
| 2026-11-18 15:29:51 UTC | Claude R31 PASS + Codex R31 A/B/D FAIL、C PASS（受审 SHA-256 `955dde67ad7b…`） | 依评审修订：(1) 迁移版本号参数化：`PM-01` 与 `DEP-PM-05` 规定重排时先按 ER-03 修订本计划中该版本号的全部引用（含文档门 `rg` 关键字）并经评审；(2) `PM-21a` AC2 改为 `rev-parse HEAD` 成功且等于夹具记录的 C2 OID；`PM-01` 写明 `reference` 的 UPDATE 触发器按更新后的行判定 `name`/`kind`/`remote` 任一列的变化；(3) `PM-33` 夹具与 AC1–AC3 纳入 `only-one` 规格文件自身也被 sparse 过滤省略（Git 2.54.0 预探），D 为无检出的 `--bare` 部分克隆；(4) ADR-PM-04 改为人读与 JSON 各走一条收尾路径，与 `PM-16` 一致 | N/A | ADR-PM-04、文档与网站验收命令第 1 步、`DEP-PM-05`、`PM-01`、`PM-21a`、`PM-33`、review log |
| 2026-11-18 15:33:19 UTC | Claude R32 PASS + Codex R32 A/B/D FAIL、C PASS（受审 SHA-256 `31cabedec0fc…`） | 依评审修订：(1) 完成判据的网站第 7 步改为「所有适用网站交付的卡」，写明 `PM-01` 只执行第 1 步、家族子卡由 `PM-24` 覆盖；(2) `PM-21a` 把其它命名空间的符号引用改为延后至 `DEFER-PM-02`（不再称永久非目标），与 ADR-PM-03、`DEFER-PM-02` 一致；(3) `PM-09` 的拆卡承接改为 `PM-29`（再拆 `PM-38`）与 `PM-23`（再拆 `PM-39`）；(4) `PM-33` 按 bare 克隆的实际布局改用 `D/objects/` 清单与对象路径，AC6 改以 `libra fast-import` 在 bare D 写入只被 `refs/heads/local` 引用的 blob（本机实测可用），并记录 Cs 的 OID 与 Libra bare 克隆建立远端跟踪引用的事实 | N/A | `PM-09`、`PM-21a`、`PM-33`、完成判据、review log |
| 2026-11-18 15:45:05 UTC | Claude R33 PASS + Codex R33 A/D FAIL、B/C PASS（受审 SHA-256 `6f963346b81b…`）；接续 Agent 修订 | 依评审修订：(1) GC-PM-02 改为按「本卡命令集合」原地改写：单命令卡恰改一行，多命令卡 `PM-21a`、`PM-18`、`PM-19` 各恰改两行，逐行 W-1 定位、两行改完后整体行数不变；(2) 「文档与网站验收命令」开头定义本卡命令集合与页面集合，第 1–7 步对每个命令或页面逐一执行与判定（逐页预检、哈希、暂存与提交核对、逐命令 W-2 部署比对），`PM-18`/`PM-19` 卡内写明两命令的 `--only`，`PM-18` 写集去掉重复的 `COMPATIBILITY.md`（R34 清扫发现 `PM-18` 同属多命令卡）；(3) `PM-33` 在 prune 前断言 `refs/remotes/origin/side` 指向 Cs 并新增 AC7（prune 前 `allow-promisor <Cs>` 成功），原 AC7 改为 AC8，prune 后断言该引用已不存在（`8/8`）；(4) `PM-16` 新增夹具 F16j，AC6 改为同时带 `--min-count`/`--reverse`/`--top`、且新旧排序给出不同结果的 JSON 调用与改动前基线逐字比较，Description 收窄为「JSON 路径保留改动前的收尾实现」；(5) 跨计划：新增 `DEP-PM-15`（checkout 源码、三份文档与网站页上与 checkout-switch-restore 计划及 `GII-21`/`GII-23` 的写锁，`PM-21a` 依赖）与 `DEP-PM-16`（网站检出交付串行窗口，对应 `DEP-GII-03`），关系表补 checkout-switch-restore 行、worktree 计划「`REL-PM-21` 家族」→ `PM-21a` 的映射与 `DEP-WC-06`/`PM-03`/`PM-33` 说明；`PM-33` Out of scope 去掉重复句号 | N/A | GC-PM-02、文档与网站验收命令、关系表、依赖登记表、实施顺序、`PM-21a`、`PM-18`、`PM-19`、`PM-16`、`PM-33`、审计表、review log |
| 2026-11-18 15:57:21 UTC | Claude R34 FAIL + Codex R34 A PASS、B/C/D/E/F FAIL（六分区；受审 SHA-256 `f801590cbef9…`） | 依评审修订（凡卡内承诺的行为要么由 AC 验收、要么在卡内写明「不单独验收」及理由）：(1) `PM-01` 以只改 `kind` 的 UPDATE 拒绝取代 runner 幂等 AC2（触发器声明为不带列清单的 `BEFORE UPDATE`；幂等改由 `db_migration_test` 既有 runner 用例守护）；(2) `PM-21a` 只解析一层（链式解析入 `DEFER-PM-02`，ADR-PM-03 同步），以「检出后 `f.txt` 为 `c2\n`」取代非法分支名 AC6（非法名沿用 `validate_branch_name`）；(3) `PM-05` 新增在库冲突对象缩写 AC8；`PM-12` 新增 `--json` 逐 stage AC6；`PM-19` 新增提交者分组 mailmap AC8；`PM-29` 新增「已是目标值的行跳过」AC8；`PM-31` 新增 `-z` 流式 AC7；`PM-36` 新增未配置回落 AC4–AC5；`PM-38` 写明 `STATE` 缺失时按 `PM-29` 以 `LBR-REPO-003` 失败并新增 AC8（ADR-PM-02 同步）；`PM-39` 新增三种互斥组合 AC6–AC8；(4) `PM-10` 的 AC1/AC2 改在冲突的身份与日期环境变量下验收「只用配置身份」（`PM-10a`/`PM-37` 同卡删去这些环境变量，验证执行协议第 5 项登记），AC5 改为仓库外 `LBR-REPO-001`；(5) `PM-15` 不再承诺子目录宏告警，AC2 改为跨来源同名宏与全局宏（新夹具 F15p，Git 2.54.0 预探）；(6) `PM-20`/`PM-22`/`PM-25` 的「无输出」收窄为 stdout 为空；`PM-03`/`PM-02`/`PM-08`/`PM-14`/`PM-23`/`PM-26` 写明不单独验收的同路径情形；`PM-18` AC8 写明 `core.excludesFile` 取值；`PM-27` AC3 映射去重 | N/A | ADR-PM-02、ADR-PM-03、验证执行协议第 5 项、`PM-01`、`PM-21a`、`PM-03`、`PM-05`、`PM-10`、`PM-10a`、`PM-37`、`PM-12`、`PM-15`、`PM-19`、`PM-20`、`PM-22`、`PM-25`、`PM-29`、`PM-31`、`PM-36`、`PM-38`、`PM-39`、`PM-02`、`PM-08`、`PM-14`、`PM-23`、`PM-26`、`PM-27`、`PM-18`、`DEFER-PM-02`、审计表、review log |
| 2026-11-18 16:03:50 UTC | Claude R35 FAIL + Codex R35 A–F FAIL（受审 SHA-256 `8441ab870f25…`） | 依评审修订：(1) 网站第 2 步 (d) 与第 7 步的 `libra status --short` 加 `--no-branch`（`status.branch=true` 时不输出分支头）；(2) 全量收口门以受测 `HEAD`/`HEAD^{tree}` 为基线，此后只改本计划、`plan-status.md`、`plan-long.md` 的证据提交不触发重跑（以 `libra diff --name-only` 核对）；(3) `PM-05` AC8 改为在库对象缩写保持不变的回归守护（改动前已给出 `1c5ab`）；(4) `PM-29`（与 ADR-PM-02、故障恢复矩阵）改为事务内先核对全部行、全部通过后才写入，AC4 在「写入探针」在位时验收冲突仍以 `LBR-CONFLICT-002` 失败；(5) `PM-39` 改为「取锁之前」拒绝并在 AC1 以他人持锁验收；(6) `PM-10a`/`PM-37` 的 AC1/AC2 改为同时设置两种角色变量以区分优先次序；(7) `PM-14` 的互斥校验置于全部模式分派之前，AC8 改用 `--ai-list-types`；(8) 写明不单独验收的同路径情形及理由：`PM-38` 标记存在而 `STATE` 不可解析、`PM-15` 同文件多个宏定义/内置 `binary` 重定义/unset 宏、`PM-18` 忽略引擎 HOME 回退与 HOME/XDG 皆无、`PM-21a` 悬空目标、`PM-03` `require_equals`、`PM-04` `-l` 默认模式、`PM-08` 其余子命令、`PM-10` `command_scope`、`PM-10a`/`PM-37` 的 `LIBRA_COMMITTER_NAME` 链位、`PM-22` 其余编辑器次序、`PM-28` 全局与 `.libra_attributes` 登记次序、`PM-25` 无候选的 `GIT_ATTR_GLOBAL`、`PM-19` shortlog 大小写不敏感 | N/A | 文档与网站验收命令第 2、7 步、完成判据、ADR-PM-02、故障恢复矩阵、`PM-05`、`PM-29`、`PM-38`、`PM-39`、`PM-10`、`PM-10a`、`PM-37`、`PM-14`、`PM-15`、`PM-18`、`PM-21a`、`PM-03`、`PM-04`、`PM-08`、`PM-22`、`PM-28`、`PM-25`、`PM-19`、review log |
| 2026-11-18 16:09:12 UTC | Claude R36 FAIL + Codex R36 A/C PASS、B/D/E/F FAIL（受审 SHA-256 `21bec1e9388f…`） | 依评审修订：(1) `PM-21a` 同卡加强既有 `symbolic_ref_delete_matrix`（补一次提交使 `main` 成为实体分支行、补引用快照断言，原期望不变；验证执行协议第 5 项登记）；(2) `PM-04` 同卡改正 `-l` 帮助文本（措辞不单独验收）；(3) `PM-10a`/`PM-37` 不单独验收的链位改以「原样复用、不修改 helper」为理由（删去不成立的 `commit_test` 覆盖说法）；(4) `PM-27` 以 `-` 开头的属性名在命令行上先被 clap 拒绝、不单独验收，并改正现行实现次序的证据；(5) `PM-33` bare 克隆远端跟踪引用的证据锚点改为 `src/command/clone.rs:184-188,1610-1613,1981-1987` 并附本机复核方式；(6) 验证执行协议第 4 项写明「不读」类表述只以「不写」快照验收、「不读」由 ER-05 核对；`PM-20` 写明多个参数错误同时出现时的校验先后不可观测；`PM-10` Out of scope 措辞改为「环境变量的接入」；(7) 协调者转告：`DEP-PM-03` 与关系表改为等待 merge-pull 计划 Phase 4 退出条件所列全部卡（2026-11-18 工作区快照 11 张，以其提交版本为准），并写明该计划不写 `ls_files.rs`（其 `MP-25` 已避开 `ls-files -o`） | N/A | 验证执行协议第 4、5 项、关系表、`DEP-PM-03`、`PM-21a`、`PM-04`、`PM-10`、`PM-10a`、`PM-37`、`PM-20`、`PM-27`、`PM-33`、review log |
| 2026-11-18 16:15:02 UTC | Claude R37 FAIL + Codex R37 D PASS、A/B/C/E/F FAIL（受审 SHA-256 `dd0c147b9009…`） | 依评审修订：(1) `DEP-PM-03` 与关系表改为具名依赖 merge-pull 的 11 张 Phase 4 卡（G-06；卡集取自 2026-11-18 工作区快照，对方提交版本若不同按 ER-03 修订）；(2) `PM-21a` AC1 改为先创建再改写同一符号引用的目标后读取；(3) `PM-04` 的 `-l` 默认模式改以「默认 `*` 走同一 wildmatch 过滤」为不单独验收理由；(4) notes 各卡夹具把 `blob(<文本>)` 定义为内容为该字节的 blob 的对象 ID（与 `notes.blob` 列及 `NOTES_MERGE_STATE` 一致）；(5) `PM-15` 规定属性引擎全部求值入口（含 `attribute_states_beneath_with_context`）改调同一求值函数，`check-attr` 之外调用方的宏效果不单独验收；(6) 按 G-01/G-04 把 `PM-19` 拆为 `PM-19`（`-<n>`/`--max-count`，`4/8`）与新卡 `PM-40`（mailmap 规范化，`4/8`，多命令卡 {`shortlog`、`check-mailmap`}），串行链改为 `PM-30 -> PM-19 -> PM-40 -> PM-35`，`PM-35` 改依赖 `PM-40` 的 mailmap 单一事实源，GC-PM-02、网站命令集合、成功定义、缺口表、Phase 3、发布窗口、审计表与追溯表同步（活动卡 40 张） | N/A | 关系表、`DEP-PM-03`、GC-PM-02、文档与网站验收命令、成功定义、当前缺口、实施顺序、Phase 3、发布窗口、`PM-21a`、`PM-04`、`PM-08`、`PM-34`、`PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39`、`PM-15`、`PM-16`、`PM-30`、`PM-19`、`PM-40`、`PM-35`、审计表、追溯表、review log |
| 2026-11-18 16:18:29 UTC | Claude R38 FAIL + Codex R38 A/C/D/E/F PASS、B FAIL（受审 SHA-256 `8ccf09299100…`） | 依评审修订：`PM-04` AC1 的模式改为含 `?` 与尾部 `*` 的 Q（C1 全 OID 第 1 位、`?`、第 3–8 位、`*`；夹具断言 C2 不匹配），使只支持尾部 `*` 前缀匹配的实现无法通过（Git 2.54.0 预探） | N/A | `PM-04`、review log |
| 2026-11-18 16:21:31 UTC | Claude R39 FAIL + Codex R39 PASS（B 分区全文复审 PASS、A/C/D/E/F 差异确认 PASS；受审 SHA-256 `fd75edf25b29…`） | 依 Claude R39 修订：测试矩阵「迁移」行删去已不由 `PM-01` 验收的「幂等」，改注「已应用迁移不重复应用属 runner 通用行为，由 `db_migration_test` 整 target 回归组中的既有 runner 用例守护」，与 `PM-01` Out of scope 一致 | N/A | 测试矩阵、review log |
| 2026-11-18 16:23:14 UTC | Claude R40 PASS + Codex R40 PASS（受审 SHA-256 `b9fbc2619f28…`） | 记账：计划文本提交门（Claude 与 Codex 双审）对同一字节通过，计划可提交；本行与对应 review log 行之外无改动 | N/A | 状态行、修订历史、review log |

## 已决议设计决策

实现时若需偏离本节，必须先修改计划并说明原因，不得在代码中静默改语义。

### ADR-PM-01: `libra var` 的取值来源与范围（`#995`）

- **Status:** Accepted（2026-11-18 R19 修订）
- **Context:** `#995` 要求 `libra var` 与 `git var` 对齐；Libra 的身份、编辑器、分页器、默认分支与属性路径解析各有既有实现，且部分与 Git 不同。
- **Decision:** `src/command/var.rs` 只调用 Libra 既有的单一事实源报告「Libra 实际会使用的值」：身份用 `commit` 的分角色签名解析（含环境变量覆盖与交叉回退）：`PM-10` 先以配置身份（`resolve_user_identity_sources` 的配置字段）与当前时刻交付命令，`PM-10a` 把 `GIT_AUTHOR_IDENT` 切换到自 `create_commit_signatures` 提取的作者专用入口（只校验作者侧日期），`PM-37` 把 `GIT_COMMITTER_IDENT` 切换到 `create_committer_signature`，两者之间发布的版本在文档中如实说明尚未接入的环境变量；编辑器用 `editor::resolve_editor`/`resolve_sequence_editor`；分页器用 `utils::pager` 的实际命令；默认分支用 `init` 的解析；全局属性路径用 `PM-18` 的来源解析。变量名精确匹配 Git 变量表；`-l` 按 Git 变量表次序列出可解析变量，但**不**像 Git 那样先列出全部配置项（Libra 配置含 vault/加密条目，GC-07）。命令需要仓库。
- **Alternatives considered:** 在 `var.rs` 内重写一套 Git 语义解析——拒绝：会与 `libra commit` 等实际行为分叉（GC-02）。
- **Consequences:** 与 Git 的差异（交叉回退、`EMAIL` 先于配置、默认分支 `main`、分页器、`TERM` 与 `VISUAL`、仓库外不可用、`-l` 不含配置）逐项记入 `COMPATIBILITY.md` 的 `var` 行；`src/cli.rs` 注册命中 ER-13 `T-1`。
- **Revisit when:** 分页器或身份解析对齐 Git（`DEFER-PM-05`）时。

### ADR-PM-02: `notes merge` 手工解决状态与 `--commit` 提交（`#989`）

- **Status:** Accepted（2026-11-18 R19 重写，取代此前基于 DB 回执、`phase` 字段、`CREATING` 标记与逐文件 fsync 的方案）
- **Context:** Libra notes 存于 SQLite `notes` 表的扁平行（`src/internal/notes.rs:1-5`），没有 Git 的 notes 提交与 `NOTES_MERGE_PARTIAL`；`#989` 允许 Libra 原生位置，但要求同等信息与语义。
- **Decision:** 冲突合并不改 `notes` 表，只在当前工作区 gitdir（`util::request_worktree_gitdir()`）下写状态：先以排他 `create_dir` 声明 `NOTES_MERGE_WORKTREE/`（已存在即拒绝新合并，`LBR-REPO-003`），再写每个冲突对象的 `<oid>` 文件（Git 冲突标记格式），最后以 `utils::atomic_write::write_atomic` 原子写 `NOTES_MERGE_STATE`（版本化格式，记录目标 ref、来源 ref、待复制条目 `(object, 来源 blob)` 与冲突条目 `(object, 目标侧前值, 来源 blob)`）。`STATE` 存在即「合并进行中」；只有 `WORKTREE` 而无 `STATE` 视为创建中途崩溃的残留，只允许 `--abort`/`--quit` 清理。`--commit` 由文件内容算出每个冲突对象的目标值（文件被删则保留前值）。首次提交先以排他创建空文件 `NOTES_MERGE_COMMITTING`（提交开始标记），再在单个写事务内先核对全部行的「当前值 ∈ {前值, 目标值}」、全部通过后才写入（前值行写为目标值、目标值行跳过）；任一行为其它值时不写入任何行、回滚事务、删除标记并以 `LBR-CONFLICT-002` 失败（状态保留）；提交后依次删除 `STATE`、`WORKTREE` 与标记。启动时标记已存在说明上次提交在事务前后崩溃，此时无法区分「未提交」与「已提交后被他人改回前值」，故只核对不重放：全部行都已是目标值则清理并成功，否则以 `LBR-CONFLICT-002` 失败并保留状态，由用户 `--abort` 后重新合并（fail-closed，绝不覆盖崩溃后他人的写入）。崩溃窗口：建标记前崩溃——重试照常；建标记后、事务提交前崩溃——重试时若各行当前值已等于目标值（例如只有冲突条目且解决文件被删除）则只清理并成功，否则被拒，`--abort` 后重新合并；提交后、删 `STATE` 前崩溃——若各行仍为目标值则重试幂等成功，若已被改动则重试被拒；删 `STATE` 后崩溃——`WORKTREE` 与/或标记残留只阻止新合并（此时 `--commit` 因 `STATE` 缺失以 `LBR-REPO-003` 失败），`--abort` 清理（`notes` 已提交、不受影响）。新合并在存在任一残留（含只有标记）时以 `LBR-REPO-003` 拒绝。上述崩溃都发生在持锁期间，因此同时残留陈旧 `NOTES_MERGE_LOCK`：用户先确认无相关进程并手工删除该锁，再执行所述重试或清理。`--abort`/`--quit` 只删除状态，不改 `notes` 表；`--quit` 为 Libra 扩展（Git 2.56 无此选项）。所有 `notes merge` 形式（新合并、`--commit`、`--abort`、`--quit`）在读取或修改状态前以排他创建 `NOTES_MERGE_LOCK` 取得同一工作区的互斥（同 Git 的 `*.lock` 文件惯例），结束时（成功或失败）删除；锁已存在即以 `LBR-CONFLICT-002` 失败且不读写任何状态；进程崩溃留下的陈旧锁由用户确认无相关进程后手工删除，错误提示给出其路径。
- **Alternatives considered:** 同事务 DB 回执 + `phase` 字段 + 逐文件/逐目录 fsync 与 lease（R1–R18 方案）——拒绝：协议面过大、不可在卡内完整验收；以「目标值即回执」直接重放 CAS——拒绝：崩溃后若他人把行改回前值，重放会静默覆盖其写入（R24 评审），故改为提交开始标记下只确认不重放；把冲突内容存进新表——拒绝：需要迁移且失去可编辑的工作区文件契约；不加锁——拒绝：并发的 `--commit`、`--abort` 与新合并可交错删除彼此的状态文件。
- **Consequences:** `notes merge` 默认 manual 策略的语义由「整体中止」改为「物化状态」（稳定码仍为 `LBR-CONFLICT-002`），记入 `COMPATIBILITY.md`；陈旧 `NOTES_MERGE_LOCK` 需手工删除，登记于故障恢复矩阵。
- **Revisit when:** Libra notes 改为提交历史存储，或需要跨进程合并锁时。

### ADR-PM-03: 非 HEAD 符号引用的存储与互斥（覆盖 `issues/479.md` 的 `ADR-PL-08`）（`#711`）

- **Status:** Accepted（2026-11-18 R19 修订：互斥改由迁移内触发器强制，撤回逐写入口事务改造的 `PM-21b1`/`PM-21b2`/`PM-21b3`/`PM-21c` 与 `REL-PM-21` 家族）
- **Context:** `symbolic-ref` 现只支持 `HEAD`（`ADR-PL-08`）；`#711`/`t2018#2` 需要 `refs/heads/a-branch → refs/remotes/origin/branch1` 并连续 `checkout -f a-branch`。`reference.kind` 的 CHECK 约束无法表达符号引用；远端 HEAD 已以 `Head` 行存储。
- **Decision:** `PM-01` 新增 Repository 迁移 `2026111702_symbolic_ref.sql`：独立 `symbolic_ref(name PRIMARY KEY, target)` 表只存 `refs/heads/<branch>` 形态的符号引用，并以 `RAISE(ABORT)` 触发器在数据库层禁止它与同名本地实体分支行并存——无论哪个写入口（`branch`、`fast-import`、`cloud restore`、`op restore` 等）都无法绕过，故不改各写入口代码。命令层：`PM-21a` 在 `HEAD` 之外支持 `refs/heads/<branch>` 名字，并新增只供 `checkout` 使用的 `Branch::find_branch_or_symref_result_with_conn`，在实体分支缺失时解析同名符号引用（只解析一层，链式解析延后至 `DEFER-PM-02`），既有 `Branch::find_branch_result*` 不变；`PM-32` 支持 `refs/remotes/<remote>/HEAD`（读写既有 `Head` 远端行）。
- **Alternatives considered:** 修改 `reference.kind` CHECK——拒绝：需重建表并波及全部 `reference::ActiveModel` 构造点；逐写入口在同一事务内检查互斥（原 `PM-21b1`/`PM-21b2`/`PM-21b3`）——拒绝：入口多、需为每个入口注入故障点，且新增入口时易遗漏。
- **Consequences:** 以实体写入覆盖符号引用分支名的操作（例如在符号引用分支上提交、`op restore` 回到含同名实体分支的快照）会被触发器 fail-closed 拒绝，记入 `DEFER-PM-02` 与风险表；`PM-01` 为 forward-only，迁移后的仓库不可再被旧二进制打开（`UnsupportedFuture`）。
- **Revisit when:** 重构 ref 存储后端或需要其它命名空间的符号引用时。

### ADR-PM-04: `shortlog` 人读输出改用 Git 格式（`#1034`）

- **Status:** Accepted（2026-11-18 R19 新增）
- **Context:** `#1034` 的全部上游用例都断言 Git 的 `%6d\t%s` 与 `Name (N):` 格式；Libra 现输出 `"   N  Name"` 且主题新到旧、按小写名排序。
- **Decision:** `PM-16` 把人读输出（含从 stdin 读取 `git log` 输出的模式）改为 Git 格式与字节序排序；`--json` 的字段与条目次序都保持改动前的实现不变，作为脚本的稳定接口——人读与 JSON 各走一条收尾路径：人读路径先按新规则排序，再依次应用 `--min-count`、`--reverse`、`--top`；JSON 路径的排序与这三个选项保持原实现；版本仍为 `patch`（人读输出的 Git 兼容修正，不改机器接口）。
- **Alternatives considered:** 保留旧格式只补参数——拒绝：`#1034` 的验收不可能通过。
- **Consequences:** 既有钉住旧格式的用例在 `PM-16` 同卡更新；`COMPATIBILITY.md` 与文档示例同步。
- **Revisit when:** 需要恢复旧格式的显式选项时。

### ADR-PM-05: `#1020` 维持 `ADR-TT-01`，整体延后

- **Status:** Accepted（2026-11-18 R19 新增）
- **Context:** `issues/498.md` 的 `ADR-TT-01` 以「源仓库含 tree/blob 标签时 `libra clone` 失败」为由拒绝非 commit 目标打标签；本计划评审期实测其传输侧缺陷（`src/internal/protocol/local_client.rs:631-670` 只按 commit 遍历 want，`DEFER-TT-01`/`DEFER-TT-05`）仍在。
- **Decision:** 撤回 `PM-13a`、`PM-13b`、`REL-PM-13`，`#1020` 记入 `DEFER-PM-04`，待传输侧修复立项后重启。
- **Alternatives considered:** 在本计划内一并修复传输——拒绝：属 clone/fetch 传输行为轴，超出本计划范围与写集。
- **Consequences:** `#1020` 保持 open；原依赖 `REL-PM-13` 的 `PM-14` 改为依赖串行链中的前一张卡（R20 起为 `PM-20`）。
- **Revisit when:** `DEFER-TT-01`/`DEFER-TT-05` 的传输修复立项。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01`..`GC-13`。补充：
- **GC-PM-01 Git 基准:** 输出格式、退出语义与解析行为以 `git@6de20f6092`（v2.56.0-116）为准；与之不同的 Libra 行为必须在卡内 `Out of scope` 或 `Description` 写明为有意差异并记入 `COMPATIBILITY.md`，不得静默偏离。`--json`/`--machine` 契约不因 Git 对齐而改变（`PM-16` 等只改人读输出）。
- **GC-PM-02 `COMPATIBILITY.md` 原地修改:** 各卡只改写本卡命令集合（定义见「文档与网站验收命令」开头）中各命令在 `COMPATIBILITY.md` 顶层命令表的状态行，原地改写、不增删行、不触及其它行：单命令卡（含只改 `symbolic-ref` 行的 `PM-01`）恰改一行；多命令卡 `PM-21a`（`symbolic-ref`、`checkout`）、`PM-18`（`check-attr`、`check-ignore`）与 `PM-40`（`shortlog`、`check-mailmap`）恰改其两行，每行写入前分别以命令块 W-1 定位（每次恰好输出一个行号），两行都改完后断言改动前后 `wc -l < COMPATIBILITY.md` 相等。唯一的增行例外是 `PM-10` 在顶层命令表 `update-ref` 行之后插入 `var` 行（`tests/compat-ledger` 钉住的行号均 ≤ 180，插入点在其后，不影响 ledger）。每次写入前取得 `DEP-PM-11`。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01`..`ER-14`：
- **ER-04 C 组 / ER-08 / ER-13 / ER-14:** 每张 `independent` 卡（含 `migration` 卡 `PM-01`）完成后按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 报告的版本面（当前 `Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`；因每次 bump 都修改 `install.sh`/`install.ps1`（ER-13 `T-1`），C 组第 ④ 步在已 bump 的树上运行本卡 A 组 focused 命令（以 nextest 执行）之外，必须运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，再 `cargo build --release`、签名提交（ER-07）、推送分支、PR、squash merge 后 `gh release create "v${VERSION}" --target "$MERGE_SHA"`。`family child`（`PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39`）只跑本卡 A 组 focused 与 fmt/clippy 并本地签名提交，C/D 由 `PM-24` 承担（T-2）。
- **ER-04 D 组远端后置门（每张会推送的卡适用，`family child` 继承 `PM-24` 的 D 组）:**
  1. `.github/workflows/base.yml`（`pull_request`；`push` 到 `main`、`release/**`）：jobs `format`、`clippy`、`redundancy`、`test` 在 PR head SHA 与合并后 `main` SHA 上均为 success。
  2. `.github/workflows/codeql.yml`（`pull_request` 到 `main`；`push` 到 `main`、`release/**`）：job `analyze` 在同上两个 SHA 上为 success。
  3. `.github/workflows/release.yml`（`push` `v*` tag，由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发）：jobs `build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 均为 success。
  逐 job 记录 run ID、head SHA 与结论；被取消或未触发的 run 不算通过。
- **ER-05:** 执行期每张卡的代码 review 按模板 ER-05 进行（本计划未具名 v2.16 code-only，审查者口径不变）；它与本计划的计划文本提交门（头部「审阅合同」）不是同一道门。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **卡特有用例**（标 `(new)`）：每条 Verification 只含一个测试，过滤式为 `-E 'test(=<完整测试路径>)'` 的精确匹配——`command_test` 中的测试为 `command::<模块>::<函数>`，`db_migration_test` 中为 `<函数>`，`--lib` 单元测试为 `<模块路径>::tests::<函数>`；不得用前缀、正则或 `|` 并集代替。
  2. 卡特有用例运行前先以同一前缀与过滤执行 `cargo nextest list`（例：`source .env.test && cargo nextest list --test command_test -E 'test(=command::symbolic_ref_test::symbolic_ref_generic_read_write_delete)'`），列出的测试必须恰为 1 个且名字与预期逐字一致，否则 STOP。
  3. 卡特有用例运行后记录 exit code 与 nextest 摘要的「运行 / 通过 / 失败 / skipped」计数，要求 exit code = 0、运行 = 通过 = 1、失败 = 0，任一不满足即 STOP；摘要中的 skipped 包含被过滤表达式排除的其它用例，只记录、不作判据。被选中的卡特有用例不得以 `#[ignore]` 或环境门控提前返回的方式绕过断言（本计划卡特有用例均不设环境门控）。共享回归组不适用第 2–3 项，只按第 5 项判定。
  4. **断言与快照：** 测试必须对卡内「AC → 断言映射」逐条断言，执行记录逐条引用 AC 编号。逐字期望文本中只有 `\n`、`\t`、`\0` 三个转义分别表示换行、制表符与 NUL，其余反斜杠均为字面字节。各卡「失败路径不变量」引用的快照定义如下，比较一律在被测调用前后各取一次、要求逐字相等：
     - 「引用快照」：`.libra/libra.db` 中 `reference` 表全部行（按 `id` 排序、全部列）；`PM-01` 之后的库另含 `symbolic_ref` 表全部行（按 `name` 排序）。
     - 「配置快照」：`.libra/libra.db`、子进程 `LIBRA_CONFIG_GLOBAL_DB` 所指隔离全局库与 `LIBRA_CONFIG_SYSTEM_DB` 所指隔离系统库这三个库中 `config_kv` 与 `config` 两表的全部行（按 `id` 排序、全部列；库或表不存在记为 absent）。
     - 「notes 快照」：`.libra/libra.db` 中 `notes` 表全部行（按 `id` 排序、全部列 `id`、`notes_ref`、`object`、`blob`；表定义 `sql/migrations/2026061401_notes.sql:1-7`，`id` 为 `AUTOINCREMENT`，故新插入行的 `id` 必不在调用前快照中，原地更新的行保持原 `id`）。
     - 「notes 合并状态快照」：当前工作区 gitdir（主工作区为 `.libra/`）下三元组（`NOTES_MERGE_STATE` 的字节 SHA-256，`NOTES_MERGE_WORKTREE/` 内「相对路径 → 字节 SHA-256」清单，`NOTES_MERGE_COMMITTING` 的字节 SHA-256），任一路径不存在记为 absent。
     - 「replace 快照」：`.libra/refs/replace/` 内「文件名 → 字节 SHA-256」清单（目录不存在记为 absent）。
     卡内「在任何读写前拒绝」「不读写任何状态」一类表述中，可验收的部分是「不写」——由「失败路径不变量」的快照逐字相等验收；「不读」是实现约束，由 ER-05 代码评审核对，不单独验收。凡卡特有用例中以稳定码失败的调用，其前后必须满足本卡「失败路径不变量」字段（该字段写 `N/A` 的卡被测命令只读，不登记保留类 AC）。标注「Git 2.54.0 预探」的期望是评审时本机 Git 2.54.0 在同构夹具上的观测，标注「Git 同夹具期望」的值在开工时取得；两类期望都必须在开工时用 `git@6de20f6092` 构建的 Git 在同一夹具重新取得并硬编码进测试，与预探不一致时以 pinned 版本为准并回写本卡 `Current evidence`（超出本卡行为轴时按 ER-03 处理）。Libra 专有行为（稳定码、`--json` 信封、有意差异）以卡内写明的 Libra 基准为准。
  5. **共享回归组**（标「共享回归组」）：可为整个 target、`test(=…)` 精确匹配或 `test(/^…/)` 正则族；运行前以同一前缀与过滤 `cargo nextest list` 记录匹配数 N（必须 ≥ 1；卡内给出预期 N 时必须相等），运行后要求 exit code = 0、运行 = 通过 = N、失败 = 0（skipped 同第 3 项只记录）。结果只作回归证据，不计为卡特有 AC 证据，但计入该卡 `VER` 分子。后续卡按其 Description 取代前序卡用例所断言的行为时（本计划仅 `PM-28` 取代 `PM-15` 与 `all_reports_only_set_attributes` 的 `-a` 次序、`PM-16` 取代既有 shortlog 旧格式断言、`PM-25` 取代 `PM-10` 的 `-l` 两行断言、`PM-09` 在两个既有 notes 合并用例的 manual 冲突之后删除新增的合并状态再继续、`PM-20` 把既有 `z_uses_nul_delimiters` 改为经 `--stdin` 输入并保留其 NUL 输出断言、`PM-10a` 与 `PM-37` 分别删去 `PM-10` AC2 与 AC1 调用中设置的作者侧与提交者侧环境变量（其「忽略环境变量」断言被取代，期望值不变）；另 `PM-21a` 加强既有 `symbolic_ref_delete_matrix`（补一次提交使 `main` 成为实体分支行、补引用快照断言，原有期望不变）），该卡同卡只改被取代的断言、保留其余断言，并在本卡 `Current evidence` 记录改动。
  6. **计数口径（G-03）：** 每条 Verification 对其每个验证 target 含两个独立门——`cargo nextest list` 预检与运行判定（非 nextest 条目，即 `PM-24` 的 release note 门与祖先门，同样由写明的预检与运行判定两门组成）；按模板「一条命令中的多个 `--test` target 分别计」，含多个 `--test` 的条目逐 target 计。故各卡 `VER` 分子 = 2 × Σ（各条目的 `--test` target 数，无 `--test` 的条目按 1 计）。一条 AC 只承载一个可独立失败的可观测谓词；不同行为、语句、选项、取值、命令或错误条件一律分条计数，不以表驱动用例合并计数。AC 文本与其映射表述同一个谓词，映射只给出该谓词的精确判定值，不得加入 AC 之外的判据；成功类映射中的 `exit 0` 是读取输出的前提。失败类 AC 一律写成「以 `<LBR-*>` 失败」，其映射以「退出码非 0」为前提（与成功类的 `exit 0` 前提对称，退出码为 0 即判失败）并断言 stderr 含该稳定码，不断言具体数值——数值退出码通常由稳定码派生（`src/utils/error.rs:514-527,1061-1076`），但可被 `with_exit_code` 与 `LIBRA_FINE_EXIT_CODES` 改变；按 Git 语义本就以特定退出码表示结果而非错误的调用（`check-ignore` 的 1、`var` 的 1）在 AC 中单独写明退出码。
  7. **进程隔离与平台：** 新增 `command_test` 用例一律经 `tests/command/mod.rs` 的 `run_libra_command*`/`base_libra_command` 以子进程调用 `libra`（逐子进程清空环境并隔离 HOME、`XDG_CONFIG_HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB`），以显式 `current_dir` 与逐子进程环境变量传入工作目录与 `GIT_*`、`TERM`、`LIBRA_PAGER` 等设置，不修改测试进程的 cwd/环境，因而不使用 `#[serial(...)]`，不触及 `tests/SERIAL_REGISTRY.tsv` 与生成的 `.config/nextest.toml`；子目录 cwd 的用例必须显式传入固定的 HOME/`XDG_CONFIG_HOME`/`LIBRA_CONFIG_GLOBAL_DB`（否则 helper 会随 cwd 改变它们）。`db_migration_test` 用例沿用其 `ConfigDbFixture` 作用域守卫；唯一例外是 `PM-01` 的 `symbolic_ref_migration_upgrade_contract`：GC-13 要求的 `ConfigDbFixture` 修改进程环境，故标 `#[serial(env)]`，`tests/SERIAL_REGISTRY.tsv` 与 `.config/nextest.toml` 已在其写集、`compat_serial_registry` 已在其回归门。其它卡若实现中确需 `#[serial]`，须先按 ER-03 把上述两个文件加入本卡写集、补 `compat_serial_registry` 回归门后再开工。本计划用例不依赖 Unix 专有语义；测试门在 Unix 上执行。

- **文档与网站验收命令（ER-06/ER-06a 同卡强制门；各卡「文档 AC」引用；按序执行，任一步失败即 STOP 且本卡不得进入 `complete`）：** **本卡命令集合与页面集合：** 本卡命令集合 = 本卡 `Docs and compatibility impact` 中要改动（或新建）其命令页 `docs/commands/<cmd>.md` 的命令（`docs/commands/README.md` 是命令索引，不计），默认只有一个；多命令卡为 `PM-21a` = {`symbolic-ref`、`checkout`}、`PM-18` = {`check-attr`、`check-ignore`}、`PM-40` = {`shortlog`、`check-mailmap`}；`PM-01` 无命令页，其集合 {`symbolic-ref`} 只用于 `COMPATIBILITY.md` 行（第 1 步末尾）；`PM-24` 无本卡集合，按第 8 项复核家族的 `notes` 页。本卡页面集合 = 命令集合中每个命令的网站页 `apps/tanstack-app/content/docs/commands/<cmd>.en.md`（`PM-10` 另含 `meta.en.json`、`index.en.mdx`；`PM-01` 为空）。下列各步中的 `<cmd>`、`<cmd>.en.md`、「本卡目标页面」与第 7 步的 URL 一律对集合中的**每个**命令或页面逐一执行、逐一判定，任一不满足即 STOP：第 1 步对每个命令的 EN/zh 命令页分别做存在性、文档关键字（卡内为关键字注明适用页面时只在所注页面检查，否则在每个命令的 EN/zh 页检查）与改动前后 SHA-256 不同的检查，并对每个命令行分别执行 W-1；第 2 步 (d) 逐页检查；第 3 步以一次 `--only` 列出命令集合的全部命令（去掉 `--write` 的漂移检查同样），逐命令核对 `docs/commands/<cmd>.md` 的字节，逐页记录改动前后 SHA-256 并逐页要求改动后不同；第 5 步的受审字节为页面集合全部页面的字节，任一页变化须重审；第 6 步提交信息列出全部命令（如 `docs(commands): sync symbolic-ref, checkout for PM-21a`），逐页核对 `libra show <提交>:<页面路径> | shasum -a 256` 等于第 5 步该页受审字节，远端前进时的恢复对页面集合中的每个页面执行；第 7 步对命令集合中的每个命令分别以本地与生产 URL 执行 W-2，每一对 SHA-256 都必须相等。
  1. **仓库文档逐文件验收：** 对本卡 `Docs and compatibility impact` 所列每个仓库文件记录改动前后 `shasum -a 256 <file>`（改动前不存在的新建文件——仅 `PM-10` 的 `docs/commands/var.md`、`docs/commands/zh-CN/var.md`、`docs/development/commands/var.md`——改动前记为 absent，改动后须存在并记录 SHA-256）；以 `test -f docs/commands/<cmd>.md && test -f docs/commands/zh-CN/<cmd>.md` 确认 EN/zh 双份存在；对卡内「文档关键字」在 EN/zh 两个命令页执行 `rg -n -F -- '<关键字>' <file>`（退出码 0 = 命中；1 = 缺失 → STOP；>1 = 命令失败 → STOP），且这两页改动后 SHA-256 必须与改动前不同；`docs/development/commands/<cmd>.md` 与 `COMPATIBILITY.md` 对应行逐文件做语义核对并更新（记录改动前后 SHA-256 与所改段落）。`COMPATIBILITY.md` 先以命令块 W-1 刷新目标行号（必须恰好输出一个行号；`PM-10` 插入前为零行、插入后恰一行），原地修改时断言改动前后 `wc -l < COMPATIBILITY.md` 相等，`PM-10` 插入时断言恰增加 1 行。`PM-01` 不改命令页、无文档关键字：其第 1 步只验收 `COMPATIBILITY.md`（W-1 定位 `symbolic-ref` 行、行数不变、该行改动后 `rg -n -F -- '2026111702'` 命中）、`docs/development/commands/_compatibility.md`（改动前后 SHA-256 不同且 `rg -n -F -- '2026111702'` 命中）与 `sql/migrations/README.md`（改动前后 SHA-256 不同且 `rg -n -F -- '2026111702_symbolic_ref'` 命中），不适用命令页与关键字检查；若 `DEP-PM-05` 要求重排迁移版本，上述 `rg` 关键字随本计划按 ER-03 修订后的版本号一并更新。
  2. **网站检出预检：** 前置：(i) `DEP-PM-12`（用户 2026-11-18 11:55:09 UTC 确认）仍有效、未被撤回或改变，否则 STOP；(ii) 已按 `DEP-PM-16` 持有该网站检出的交付窗口，未持有时不执行本步及后续各步，按 `DEP-PM-16` 登记等待。按序：(a) `test -d /Volumes/Data/gitmono/libra-backend/.libra && test ! -e /Volumes/Data/gitmono/libra-backend/.git`，不满足即 STOP；(b) `cd /Volumes/Data/gitmono/libra-backend && libra --offline symbolic-ref --short HEAD`，输出不是 `cf` 时按 ER-06a 执行 `libra switch cf`（被脏工作区等阻止即 STOP）后重跑 (b)；(c) 此后输出须恰为 `cf`，且 `libra status --short --branch` 首行以 `## cf...origin/cf` 开头，否则 STOP；(d) 本卡目标页面（`apps/tanstack-app/content/docs/commands/<cmd>.en.md`，`PM-10` 另含 `meta.en.json`、`index.en.mdx`）若已有未提交改动（`libra status --short --no-branch -- <页面路径>` 输出非空；`--no-branch` 使 `status.branch=true` 配置下不输出分支头，`src/command/status/input.rs:52-55,819-822`）即 STOP；随后 `libra fetch origin`，要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`——本地落后时只允许 `libra pull --ff-only origin cf` 快进后重验，领先或分叉即 STOP；`libra status --porcelain` 中不得有任何已暂存条目，否则 STOP。已有的无关改动保留、不暂存。
  3. **同步与漂移检查：** 以包含本卡最终 EN 参考文档的 checkout 为 `--libra-root`（默认 `/Volumes/Data/gitmono/libra`；执行前以第 1 步记录的 SHA-256 确认其中 `docs/commands/<cmd>.md` 已是本卡合入 `main` 的字节），执行 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --write --only <cmd…>`（`PM-10` 为 `--only var index`），随后同一命令去掉 `--write` 必须 exit 0（1 = 漂移、2 = 安全拒绝，均 STOP）；记录每个目标页面改动前后 SHA-256（`PM-10` 的 `var.en.md` 改动前为 absent），且改动后必须与改动前不同。
  4. **前端校验：** `cd /Volumes/Data/gitmono/libra-backend && pnpm typecheck` 与 `pnpm build` 分别记录退出码，均须为 0。
  5. **网站评审：** 对第 3 步记录的最终页面字节取得独立 Claude Code `VERDICT: PASS`；评审后页面字节变化须重审。
  6. **精确提交与推送：** 只 `libra add` 本卡目标页面 → `libra status --porcelain` 的已暂存条目必须恰为本卡页面集合，否则 STOP → `libra commit -s -m "docs(commands): sync <cmd> for <卡 ID>"` → 按 ER-07 对同一提交校验 `gpgsig` 与 `Signed-off-by` → `libra show <提交>:<页面路径> | shasum -a 256` 等于第 5 步受审字节 → 推送前再次 `libra fetch origin`，要求 `libra rev-parse <提交>^` 等于 `libra rev-parse refs/remotes/origin/cf` → `libra push origin cf`，推送后 `libra ls-remote origin refs/heads/cf` 指向该提交。远端已前进时不推送、不 force、不改写他人提交，按以下恢复后从第 2 步重新开始：`libra reset --mixed <提交>^` → 对父提交中已存在的页面执行 `libra restore --staged --worktree -- <这些页面路径>`；父提交中不存在的新建页面（仅 `PM-10` 的 `var.en.md`）在 reset 后为未跟踪文件，先以 `shasum -a 256` 确认其字节等于第 5 步受审字节再 `rm` 该文件（不触及其它未跟踪或已改动文件；哈希不符即 STOP）→ `libra pull --ff-only origin cf`；重新生成的页面字节与第 5 步受审字节不同时须重新取得第 5 步 `PASS`。
  7. **部署核验（`<article>` HTML 逐字节比对）：** 基准构建前先过干净性门：`cd /Volumes/Data/gitmono/libra-backend && libra status --short --no-branch --ignored --untracked-files=all` 的输出中，除后端根目录 `docs/` 下的条目与下列构建产物/依赖/缓存的忽略条目外不得有任何条目：`node_modules/`、`apps/tanstack-app/node_modules/`、`apps/tanstack-app/dist/`、`apps/tanstack-app/.wrangler/`、`apps/tanstack-app/src/generated/docs-manifest.ts`、`apps/tanstack-app/src/generated/docs-search.ts`、`scripts/__pycache__/`、`test-results/` 与 `apps/tanstack-app/.dev.vars`；后端根目录不得存在任何 `.env*` 文件；并要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`。任一不满足即 STOP（不清理、不暂存他人改动）。随后以 `pnpm preview:cf`（`apps/tanstack-app/package.json:9`：先构建再 `wrangler dev --port 7001`）作基准，对本地 `http://localhost:7001/en/docs/commands/<cmd>` 与生产 `https://libra.tools/en/docs/commands/<cmd>` 分别执行命令块 W-2：它在内存中取得同一次响应，要求 HTTP 200、`<article>` 存在且文本非空，输出 `<article>` 内部 HTML 原始字节的 SHA-256 与字节数；退出码非 0（1 = 请求失败，如连接、超时或解码错误；2 = 缺 `<article>` 或文本为空；3 = HTTP 状态非 200，含 `urlopen` 对 4xx/5xx 抛出的 `HTTPError`）即 STOP。两份 SHA-256 必须相等；不相等即 STOP。只把两份哈希、字节数与观察 UTC 时间写入本计划，不落盘任何响应内容；用后停止本地预览进程。部署传播延迟按 ER-10 暂时性错误有界重试（≤ 5 次、≤ 30 分钟），超预算置 `blocked`。
  8. **时序与家族：** 独立卡（`PM-01` 除外）：第 1–2 步在本卡 C 组第 ⑦ 步签名提交之前完成，任一失败则本卡不得提交、合并或发布（`Lifecycle=blocked`）；第 3–7 步在本卡 PR squash merge 与 `gh release create` 之后执行（开始前重跑第 2 步），使网站不先于发布公开新行为，未完成前本卡停留在 `remote-pending`。`REL-PM-01` 家族（ER-06a 同卡交付）：子卡 `PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39` 各自在本卡 Libra 仓库本地签名提交之前完成第 1–2 步、之后完成第 3–5 步——第 2 步 (d) 的「`libra rev-parse HEAD` 等于 `refs/remotes/origin/cf`」放宽为「`HEAD` 等于 origin/cf 之上恰好依次叠加本家族此前子卡的后端本地提交」，`notes.en.md` 的未提交改动必须为空；第 3 步的 `--libra-root` 为含本卡本地签名提交的 Libra checkout，其「已是本卡合入 `main` 的字节」核对改为「等于本卡本地签名提交中的字节」（`libra show <本卡提交>:docs/commands/notes.md | shasum -a 256` 与第 1 步记录相等）——再执行第 6 步到签名提交与 `gpgsig`/`Signed-off-by` 校验为止（只 `libra add` `notes.en.md`，提交信息 `docs(commands): sync notes for <卡 ID>`，不推送），并记录后端提交 ID 与第 5 步受审字节的 SHA-256。发布点 `PM-24` 在家族 `gh release create` 之后：按同样放宽重跑第 2 步；以复核方式重跑第 3 步（`--libra-root` 为含家族合并结果的 `main`；`--write` 后页面可以不变，不适用第 3 步「改动后必须与改动前不同」，去掉 `--write` 的漂移检查须 exit 0），随后按页面 SHA-256 分支：等于最后一张子卡第 5 步受审字节的 SHA-256 时直接进入推送；不等时不视为失败，而是重做第 4–5 步（对新字节取得 `PASS`）并只 `libra add` 该页面追加一次本地签名提交，此后以新的受审 SHA-256 作为推送与第 7 步的基准；第 4 步；随后执行第 6 步的推送部分——推送前要求家族首个后端提交的父提交等于 `refs/remotes/origin/cf`，一次 `libra push origin cf` 推送全部家族后端提交——与第 7 步部署核验。子卡的文档 AC 在 `PM-24` 第 7 步通过后才算满足（子卡因此与 `PM-24` 一同进入 `complete`）。家族发布前失败时，后端本地提交按 `DEP-PM-14` 撤回。`PM-01` 不改命令页与网站页：只在签名提交前执行第 1 步（按第 1 步末尾的 `PM-01` 规则），不执行第 2–7 步。

上列第 1、7 步引用的命令块（`<cmd>` 依次替换为本卡命令集合中的每个命令；`<URL>` 为第 7 步的本地或生产页面地址）：

```bash
# W-1：定位 COMPATIBILITY.md「## Top-level commands」首张表中 <cmd> 的状态行；必须恰好输出一个行号
awk -F'|' -v c=" <cmd> " '/^## Top-level commands/{s=1;next} s&&/^\|/{t=1} s&&t&&/^$/{exit} s&&t&&$2==c&&$3~/^ (supported|partial|unsupported|intentionally-different) $/{print NR}' COMPATIBILITY.md

# W-2：在内存中取得 <URL> 的同一次响应，输出 <article> 内部 HTML 原始字节（不做规范化）的「SHA-256 字节数」；退出码须为 0，不落盘响应
python3 -I - "<URL>" <<'PY'
import hashlib, html, re, sys, urllib.error, urllib.request
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
m = re.search(r"<article>(.*?)</article>", s, re.S)
if not m:
    sys.exit(2)
a = m.group(1)
if not " ".join(html.unescape(re.sub(r"<[^>]+>", " ", a)).split()):
    sys.exit(2)
b = a.encode("utf-8")
print(hashlib.sha256(b).hexdigest(), len(b))
PY
```

## 实施顺序

全串行 DAG（与各卡 `Dependencies` 一致；`A -> B` 表示 A 先于 B）：
- 跨计划与审批前置：`DEP-PM-05 -> PM-01`；`DEP-PM-06 -> PM-21a`；`DEP-PM-08 -> PM-33`；`DEP-PM-08 -> PM-14`；`DEP-PM-10 -> PM-05`；`DEP-PM-03 -> PM-11`；`DEP-PM-03 -> PM-12`；`DEP-PM-09 -> PM-15`、`PM-28`、`PM-18`；`DEP-PM-13 -> PM-01`、`PM-07`、`PM-10`、`PM-10a`、`PM-18`、`PM-19`；`DEP-PM-15 -> PM-21a`；`DEP-PM-11` 门控每张写 `COMPATIBILITY.md` 的卡；`DEP-PM-12` 门控全部卡的网站步骤；`DEP-PM-16` 门控每次执行网站第 2–7 步（不写入各卡 `Dependencies`，同 `DEP-PM-12`）；`DEP-PM-14` 覆盖 `REL-PM-01` 家族的后端推送冻结窗口。
- Phase 1: `PM-01 -> PM-21a -> PM-32 -> PM-02 -> PM-03 -> PM-04 -> PM-26 -> PM-05 -> PM-06`
- Phase 2: `PM-06 -> PM-07 -> PM-08 -> PM-34 -> PM-09 -> PM-29 -> PM-38 -> PM-23 -> PM-39 -> PM-24 -> PM-10 -> PM-10a -> PM-37 -> PM-22 -> PM-36`
- Phase 3: `PM-36 -> PM-11 -> PM-12 -> PM-15 -> PM-27 -> PM-28 -> PM-18 -> PM-25 -> PM-16 -> PM-30 -> PM-19 -> PM-40 -> PM-35 -> PM-17 -> PM-31 -> PM-20 -> PM-14 -> PM-33`
- 家族内部：`PM-09 -> PM-29 -> PM-38 -> PM-23 -> PM-39 -> PM-24`（`PM-24` 另直接依赖 `PM-09`、`PM-29`、`PM-38`、`PM-23`）。

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-PM-01 | incoming | 上游基线 | `libra` `main` 当前 checkout | 本计划协调者 | `cargo check` 通过且「事实基线」锚点可解析 | `src/cli.rs:344` | 开工前按 ER-02 刷新锚点；锚点失效即先修订计划 |
| DEP-PM-02 | outgoing | 跨计划顺序交付 | `plan-20261117-clone.md#CL-14`（`#750`） | `clone` 计划协调者 | `PM-06` 先合入 `src/command/rev_parse.rs` 与 `COMPATIBILITY.md`，随后 `CL-14` 按其 `DEP-CL-02` 合入（单向） | `docs/development/plan/plan-20261117-clone.md` | 不阻塞 `PM-06`；接收方未接手时 `CL-14` 自行 rebase |
| DEP-PM-03 | incoming | 跨计划顺序前置（冲突索引内容稳定） | `plan-20261117-merge-pull.md#MP-16,MP-17,MP-24,MP-18,MP-25,MP-46,MP-50,MP-19,MP-54,MP-20,MP-47`（其 Phase 4「重命名与 D/F 冲突引擎修复」的 11 张卡，改变冲突时写入索引的 stage 条目；卡集取自 2026-11-18 工作区快照，该计划尚在评审，其提交版本的卡集若不同，本计划按 ER-03 修订本行与相关引用并重新评审；该计划不写 `src/command/ls_files.rs`——其 `MP-25` 已避开 `ls-files -o`——其关系表亦声明对方等待其 Phase 4 结束） | 仓库发布协调者 | 上列 11 张卡各自在 `main` 达到 `done/complete` 后才开工 `PM-11`/`PM-12`，使本计划固化的 `ls-files` 冲突输出与 `compat_conflict_status_diff` 期望建立在稳定的冲突索引上（单向） | `docs/development/plan/plan-20261117-merge-pull.md`（关系表与 Phase 4 退出条件）；`plan-status.md` | 未完成时 `PM-11`/`PM-12` 置 `blocked` |
| DEP-PM-04 | — | 撤回 | 原 `PM-06`/`CL-14` 双向等待边 | — | 2026-11-17 R10 撤回（消除环路），编号不复用 | 修订历史 2026-11-17 05:25:00 行 | — |
| DEP-PM-05 | incoming | 迁移顺序硬前置 | `docs/development/plan/issues/1048.md#I1048-05`（`2026111701_namespace_reference.sql`） | 仓库发布协调者 | `I1048-05` 已合入 `main`；若届时 `repository_migrations()` 已有更高版本，`PM-01` 按 ER-02 把迁移版本重排到当前尖端之上（同步改文件名、README 与 `PM-01` 所列全部尖端断言，并先按 ER-03 修订本计划中该版本号的全部引用——AC、文档门 `rg` 关键字与恢复说明——经评审后执行），绝不让高版本先于低版本 | `docs/development/plan/issues/1048.md`；`src/internal/db/migration.rs:2464-2470` | `I1048-05` 未合入前 `PM-01` 置 `blocked` |
| DEP-PM-06 | incoming | 跨计划顺序前置（共享 `src/internal/branch.rs`） | `plan-20261117-branch.md#BR-10,BR-22` 与 `plan-20261117-worktree-clean-reset-apply.md#WC-10`（对方 `DEP-BR-12`、`DEP-WC-01`） | 仓库发布协调者 | 统一顺序 `BR-10`/`BR-22` → `WC-10` → `PM-21a`：`WC-10` 在 `main` 达到 `done/complete` 后才开工 `PM-21a`；`PM-21a` 不反向阻塞 `WC-10`（单向无环） | `plan-20261117-worktree-clean-reset-apply.md`；`plan-20261117-branch.md` | `WC-10` 未完成时 `PM-21a` 置 `blocked`，禁止并发写 `branch.rs` |
| DEP-PM-07 | outgoing | 跨计划顺序交付 | `plan-20261117-commit-signing.md`（`DEP-CS-04`、`DEP-CS-09`）、`plan-20261117-describe.md`（`DEP-DESC-UTIL`）、`plan-20261117-sequencer-am.md`（`DEP-SA-04`）、`plan-20261117-grep.md`（`DEP-GR-06`） | 各接收方计划协调者 | 本计划按「实施顺序」交付：`PM-02`（`config.rs`/config 文档）、`PM-09`（随 `PM-24` 发布）、`PM-10`（`src/cli.rs`、`tests/command/mod.rs`）、`PM-14`（`cat_file.rs`）、`PM-05`/`PM-06`（`rev_parse` 测试与文档）、`PM-18`（最后一个 `attributes.rs` 写者）；`DEP-CS-09` 是经 `DEP-PM-05` 到 `PM-02` 的说明性传递前置，本计划内该链为 `PM-01 → PM-21a → PM-32 → PM-02` | 「与其它计划的关系」 | 不阻塞本计划卡自身发布；接收方按其 `DEP-*` 等待 |
| DEP-PM-08 | incoming | 跨计划前置 | `plan-20261117-clone.md#CL-13,CL-19,CL-29`（部分克隆、promisor 读取、`sparse:oid` 与 `t5616#33` 夹具） | `clone` 计划协调者 | `CL-13`、`CL-19`、`CL-29` 在 `main` 达到 `done/complete` 后开工 `PM-33`（其 F33 夹具与 promisor 归因消费它们；clone 计划中写作「供 `PM-03`」之处即指自 `PM-03` 拆出的 `PM-33`）；`CL-19` 达到 `done/complete` 后开工 `PM-14`（共享 `cat_file.rs`）；单向无环。两卡排在本计划串行链末尾，未就绪时不阻塞其余卡 | `docs/development/plan/plan-20261117-clone.md` | 未完成时 `PM-14`/`PM-33` 置 `blocked` |
| DEP-PM-09 | outgoing | 跨计划顺序交付（共享 `src/utils/attributes.rs`） | `plan-20261117-grep.md#GR-21`（`DEP-GR-06`）、`plan-20261117-diff.md` 中写 `attributes.rs` 的卡 | 接收方计划协调者 | 本计划 `attributes.rs` 写者依次为 `PM-15`、`PM-28`、`PM-18`；`PM-18` 达到 `done/complete` 即释放，接收方此后再写；本计划开工上述卡前取得 `attributes.rs` 独占写锁（持锁者与等待者的判定、等待登记与取得顺序同 `DEP-PM-11`） | `src/utils/attributes.rs:974-988,1061-1086` | 存在其它持锁者时本计划对应卡置 `blocked` 并登记等待，禁止并发写（G-10） |
| DEP-PM-10 | incoming | 跨计划顺序前置 | `plan-20261117-global-init-infra.md#GII-12`（共享 `src/command/rev_parse.rs`） | 仓库发布协调者 | `GII-12` 在 `main` 达到 `done/complete` 后开工 `PM-05`（单向：`GII-12 -> PM-05 -> PM-06 -> CL-14`） | `docs/development/plan/plan-20261117-global-init-infra.md` | 未完成时 `PM-05` 置 `blocked` |
| DEP-PM-11 | incoming | 跨计划共享文件写锁 | `COMPATIBILITY.md`（同批 `clone`、`branch`、`grep`、`describe`、`worktree`、`diff` 等计划均写该文件） | 仓库发布协调者 | 每张写 `COMPATIBILITY.md` 的独立卡开工前取得该文件的独占写锁。只有**持锁者**（已在 `plan-status.md` 以开工条目登记锁定该文件、尚未登记释放的卡）构成排他障碍；因锁置 `blocked` 的等待者不持锁、不阻挡其它卡。等待者在 `plan-status.md` 登记等待条目，锁释放后按等待条目的 UTC 时间先后取得（时间相同按计划文件名字典序，同一计划内按「实施顺序」）。持锁卡推送合入后登记释放；`REL-PM-01` 家族由 `PM-09` 开工时一次取得，`PM-29`、`PM-38`、`PM-23`、`PM-39`、`PM-24` 顺序沿用同一把锁，`PM-24` 合入或家族按逆序回滚完成后登记释放；写入遵守 GC-PM-02 | `plan-status.md`；GC-PM-02 | 存在其它持锁者时置 `blocked` 并登记等待，不得并发修改 |
| DEP-PM-12 | incoming | 审批（已满足） | 用户对网站文档交付检出路径的确认 | 用户 | **已满足**：用户 2026-11-18 11:55:09 UTC 确认本批网站文档交付检出为 `/Volumes/Data/gitmono/libra-backend`（`cf`）；各卡「文档与网站验收命令」第 2 步只复核其未被撤回或改变 | 用户 2026-11-18 11:55:09 UTC 确认（记于「文档职责」）；`AGENTS.md:135`（`/Volumes/Sea/libra-backend`，本机不存在，已知文档滞后，本批不改） | 若用户撤回或改变：尚未到 C 组第 ⑦ 步的卡不提交并置 `blocked`，已完成 C 组的卡停留 `remote-pending`；按新路径修订本计划并经评审后再执行，不得改用其它检出 |
| DEP-PM-13 | incoming | 跨计划共享单一事实源写锁 | `src/cli.rs`（`PM-10`、`PM-19`）、`tests/command/mod.rs` 与 `tests/compat/help_examples_banner.rs`（`PM-10`）、`Cargo.toml` 依赖行（`PM-07`）、`src/utils/util.rs`（`PM-18`）、`src/command/commit.rs`（`PM-10a`）——同批多份计划写这些文件 | 仓库发布协调者 | 上述卡开工前逐文件取得独占写锁（另含 `PM-01` 的 `tests/SERIAL_REGISTRY.tsv` 与生成的 `.config/nextest.toml`）；持锁者与等待者的判定、等待登记与取得顺序同 `DEP-PM-11`；本卡推送合入后登记释放 | `plan-status.md` | 存在其它持锁者时置 `blocked` 并登记等待（G-10） |
| DEP-PM-14 | outgoing | 跨计划共享资源窗口（后端 `cf` 推送冻结） | 网站检出 `/Volumes/Data/gitmono/libra-backend` 的 `cf` 分支；同批其它计划的网站同步步骤 | 仓库发布协调者 | `REL-PM-01` 首个子卡（`PM-09`）在后端本地签名提交前于 `plan-status.md` 登记冻结开始，至 `PM-24` 推送全部家族后端提交并通过第 7 步、或家族撤回完成后登记结束；窗口内后端 `cf` 领先 `origin/cf` 的提交只能是本家族子卡的 `notes.en.md` 提交，其它计划与本计划其它卡不得在该检出提交或推送（各卡第 2 步「`HEAD` 等于 `origin/cf`」预检本就会 STOP）。家族发布前失败时撤回：`libra reset --mixed <家族首个后端提交>^` 后 `libra restore --staged --worktree -- apps/tanstack-app/content/docs/commands/notes.en.md`，确认 `HEAD` 等于 `origin/cf` 与该页无改动后登记结束 | `plan-status.md`；「文档与网站验收命令」第 8 项 | 窗口持续期间其它网站交付等待；家族任一卡 `blocked` 超过 ER-10 预算时按撤回步骤释放窗口 |
| DEP-PM-15 | incoming | 跨计划共享文件写锁（checkout） | `src/command/checkout.rs`、`docs/commands/checkout.md`、`docs/commands/zh-CN/checkout.md`、`docs/development/commands/checkout.md` 与网站 `apps/tanstack-app/content/docs/commands/checkout.en.md`：本计划只有 `PM-21a` 写；同批 `plan-20261117-checkout-switch-restore.md` 中写这些文件的 `CSR-*` 卡与 `plan-20261117-global-init-infra.md#GII-21,GII-23` 亦写（按 ID 引用，以各计划提交版本为准） | 仓库发布协调者 | `PM-21a` 开工前逐文件取得独占写锁；持锁者与等待者的判定、等待登记与取得顺序同 `DEP-PM-11`；源码与三份仓库文档在 `PM-21a` 推送合入后登记释放，网站页 `checkout.en.md` 在其「文档与网站验收命令」第 7 步通过后登记释放。两计划之间只互斥、不设先后顺序边；`PM-21a` 开工时按 ER-02 刷新 `checkout.rs` 锚点 | `plan-status.md`；「与其它计划的关系」 | 存在其它持锁者时 `PM-21a` 置 `blocked` 并登记等待，禁止并发写（G-10） |
| DEP-PM-16 | incoming | 跨计划共享资源窗口（网站检出交付串行） | 网站检出 `/Volumes/Data/gitmono/libra-backend` 的交付操作：「文档与网站验收命令」第 2–7 步（同步、`pnpm typecheck`/`pnpm build` 改写被忽略的 `apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts` 与 `apps/tanstack-app/dist/`、提交推送、`pnpm preview:cf`）；同批其它计划的网站交付（`plan-20261117-global-init-infra.md` 的 `DEP-GII-03` 把网站交付视为与所有同批计划相交） | 仓库发布协调者 | 每次执行第 2 步起至本次执行的最后一步结束（含第 6 步远端前进后的恢复与重做；家族子卡至其第 6 步本地签名提交完成）期间持有该检出的独占交付窗口，结束后登记释放；持锁者与等待者的判定、等待登记与取得顺序同 `DEP-PM-11`；与 `DEP-PM-14` 并存（后者另在家族窗口内冻结其它后端提交与推送） | `plan-status.md`；「文档与网站验收命令」第 2 步 | 存在其它持有者时本卡网站步骤等待：独立卡签名提交前的第 2 步受阻时置 `blocked`，发布后的第 2–7 步受阻时停留 `remote-pending`，家族子卡受阻时置 `blocked` |

### 发布分组与并发窗口

| ID | 成员 | 唯一发布点 | 窗口规则 | 失败回滚顺序 | 理由 |
|---|---|---|---|---|---|
| REL-PM-01 | `PM-09`（`family child`，1/5）→ `PM-29`（2/5）→ `PM-38`（3/5）→ `PM-23`（4/5）→ `PM-39`（5/5） | `PM-24`（`Task type = release`） | 五张子卡各自 review、各自通过 A 组门与 fmt/clippy，并在 Libra 仓库与后端 `cf` 各自本地签名提交（后端只提交 `notes.en.md`），**禁止**在 `PM-24` 之前推送任一仓库、打 tag 或发布；窗口期不插入其它卡，后端推送冻结见 `DEP-PM-14`；`PM-24` 统一执行 `patch + 1`、版本面同步、T-2 全量、`cargo build --release`、`gh release create` 与网站第 8 项的发布点步骤 | 发布前失败按 `PM-24` → `PM-39` → `PM-23` → `PM-38` → `PM-29` → `PM-09` 逆序本地 revert 并复跑 `notes_test` 回归组确认恢复，后端本地提交按 `DEP-PM-14` 撤回；发布后按 `immutable-release` 前滚新 patch | 只发布状态创建而无 `--commit`/`--abort` 的中间版本会让用户陷入无法完结的合并状态 |
| REL-PM-21 | — | — | **已撤回**（2026-11-18 R19）：互斥改由 `PM-01` 触发器强制，`PM-21a` 改为 `independent`（R20 再拆出 `PM-32`）；`PM-21b1`/`PM-21b2`/`PM-21b3`/`PM-21c` 撤回，编号不复用 | — | ADR-PM-03 |
| REL-PM-13 | — | — | **已撤回**（2026-11-18 R19）：`PM-13a`/`PM-13b` 撤回，`#1020` 记入 `DEFER-PM-04`，编号不复用 | — | ADR-PM-05 |

**并发声明:** 全串行（共享 `COMPATIBILITY.md`、`notes.rs`、`rev_parse.rs`、`ls_files.rs`、`attributes.rs`、`check_ignore.rs`、`shortlog.rs`、`var.rs` 等写集，G-10）。

**发布者:** 协调者指派的单一发布代理（ER-12：发布串行，禁止多 Agent 并发发布）。

**发布窗口顺序:** `PM-01` → `PM-21a` → `PM-32` → `PM-02` → `PM-03` → `PM-04` → `PM-26` → `PM-05` → `PM-06` → `PM-07` → `PM-08` → `PM-34` → `REL-PM-01`（`PM-09` → `PM-29` → `PM-38` → `PM-23` → `PM-39` → `PM-24`） → `PM-10` → `PM-10a` → `PM-37` → `PM-22` → `PM-36` → `PM-11` → `PM-12` → `PM-15` → `PM-27` → `PM-28` → `PM-18` → `PM-25` → `PM-16` → `PM-30` → `PM-19` → `PM-40` → `PM-35` → `PM-17` → `PM-31` → `PM-20` → `PM-14` → `PM-33`（与实施 DAG 一致；同一时刻只允许一张卡或一个家族持有发布窗口）。

### Phase 1: 引用、配置与对象解析（`PM-01`、`PM-21a`、`PM-32`、`PM-02`、`PM-03`、`PM-04`、`PM-26`、`PM-05`、`PM-06`）

**目标:** `symbolic-ref` 的 `refs/heads/*` 与远端 HEAD 符号引用、`config --default`、`rev-list --missing`（本地缺失对象）、`replace` 列表与删除、`rev-parse --short` 与 `--show-object-format`。
**进入条件:** `DEP-PM-01` 基线核实完成；`DEP-PM-05` 满足。
**退出条件:** `PM-01, PM-21a, PM-32, PM-02, PM-03, PM-04, PM-26, PM-05, PM-06` 全部 `done`/`complete`。

### Phase 2: 历史、notes 与 `var`（`PM-07`、`PM-08`、`PM-34`、`PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39`、`PM-24`、`PM-10`、`PM-10a`、`PM-37`、`PM-22`、`PM-36`）

**目标:** `blame --minimal`、notes 默认 ref、`REL-PM-01` notes 合并状态机、`libra var`。
**进入条件:** `PM-06` 为 `done`/`complete`。
**退出条件:** `PM-07, PM-08, PM-34, PM-09, PM-29, PM-38, PM-23, PM-39, PM-24, PM-10, PM-10a, PM-37, PM-22, PM-36` 全部 `done`/`complete`。

### Phase 3: 索引、对象、属性、shortlog 与忽略（`PM-11`、`PM-12`、`PM-15`、`PM-27`、`PM-28`、`PM-18`、`PM-25`、`PM-16`、`PM-30`、`PM-19`、`PM-40`、`PM-35`、`PM-17`、`PM-31`、`PM-20`、`PM-14`、`PM-33`）

**目标:** `ls-files` 冲突条目、属性宏/名字/次序/全局来源、`var` 其余变量、`shortlog` 格式/分组/mailmap、`check-ignore` 路径/流式/quiet，以及等待 `DEP-PM-08` 的 `cat-file <type> <object>` 与部分克隆上的 `rev-list --missing`。
**进入条件:** `PM-36` 为 `done`/`complete`。
**退出条件:** `PM-11, PM-12, PM-15, PM-27, PM-28, PM-18, PM-25, PM-16, PM-30, PM-19, PM-40, PM-35, PM-17, PM-31, PM-20, PM-14, PM-33` 全部 `done`/`complete`，且「完成判据」的全量收口门通过。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Release write set 默认:** 版本面集合（以 `compat_version_surface_sync` 为权威；当前 `Cargo.toml`、`install.sh`、`install.ps1`）+ 工具链刷新的 `Cargo.lock` + `gh release create` 产物；各卡写 `Inherited` 即继承
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（推送前可本地 revert 本卡提交；合入并发布后只前滚新 `patch + 1`。`PM-01` 之后：已迁移到 `2026111702` 的仓库不得用早于 `PM-01` 的二进制打开，降级指引为升级到最新版本或从迁移前快照恢复）
- **Migration and rollback 默认:** `N/A：无 SQLite schema 迁移`
- **Security and privacy 默认:** `继承 GC-07、GC-11`
- **Performance budget 默认:** `继承 GC-10`
- **失败路径不变量默认:** `N/A`（被测命令只读；见「验证执行协议」第 4 项）

**默认覆盖:**

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| `PM-01` | `Task type`、`Rollback mode`、`Migration and rollback` | `migration`、`forward-only`、`2026111702_symbolic_ref.sql`（迁移不可逆，前滚修复） |
| `PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39` | `Release boundary`、`Version increment`、`Release write set`、`C/D coverage from` | `family child of REL-PM-01`、`N/A`、`N/A`、`PM-24` |
| `PM-24` | `Task type`、`Release boundary`、C 组发布命令 | `release`、`family release point of REL-PM-01`、`gh release create "v${VERSION}" --target "$MERGE_SHA" --title "v${VERSION}" --notes-file "$NOTES_FILE"`（经本卡 AC6–AC10 核对的 release note，取代 `--generate-notes`） |
| `PM-01`、`PM-21a`、`PM-32`、`PM-02`、`PM-04`、`PM-26`、`PM-08`、`PM-34`、`PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39`、`PM-33` | 失败路径不变量 | 各卡字段写明（写入类命令） |

**规则 waiver（`EX-*`）:** 无。

**撤回的卡号（保留引用，不复用）:**

| 卡号 | 撤回时间 | 去向 |
|---|---|---|
| `PM-21b1`、`PM-21b2`、`PM-21b3`、`PM-21c` | 2026-11-18 R19 | 互斥改由 `PM-01` 触发器强制（ADR-PM-03），逐入口改造不再需要；联合回归并入 `PM-21a`、`PM-32` 与 `PM-01` 的用例 |
| `REL-PM-21` | 2026-11-18 R19 | 家族解散，`PM-21a` 改为 `independent`；其他计划引用 `REL-PM-21` 处按 `PM-21a` 理解 |
| `PM-13a`、`PM-13b`、`REL-PM-13`（及拆分前的 `PM-13`） | 2026-11-18 R19 | `#1020` 整体延后，`DEFER-PM-04`（ADR-PM-05） |
| `PM-21`（拆分前编号） | 2026-11-17 | 拆为 `PM-21a` 等（见修订历史），现由 `PM-21a`（写 `src/internal/branch.rs`）与 `PM-32` 承接 |

### 任务卡粒度审计表

> `landing / prod-files` 只计生产代码落点与生产文件（G-04；不含随附测试、文档与版本面）。`VER` = 2 × Verification 条目所含验证 target 数之和（一条命令含多个 `--test` 时逐 target 计，其余条目按 1 计；「验证执行协议」第 6 项）。

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| PM-01 | migration | symbolic_ref表与实体分支互斥触发器前向迁移 | forward-only | yes | yes | 8/8 | 8/8 | 2/2 | M | DEP-PM-05,DEP-PM-11,DEP-PM-13 | no-overlap（串行链起点） | independent | N/A | N/A |
| PM-21a | implementation | symbolic-ref的refs/heads符号引用与checkout解析 | immutable-release | yes | yes | 8/8 | 8/8 | 2/3 | M | PM-01,DEP-PM-06,DEP-PM-11,DEP-PM-15 | 串行于 PM-01 | independent | PM-01 | N/A |
| PM-32 | implementation | symbolic-ref远端HEAD读写删除 | immutable-release | yes | yes | 5/8 | 6/8 | 2/2 | M | PM-21a,DEP-PM-11 | 串行于 PM-21a | independent | PM-21a | N/A |
| PM-02 | implementation | config --default隐式读取与非读动作拒绝 | immutable-release | yes | yes | 7/8 | 8/8 | 1/1 | M | PM-32,DEP-PM-11 | 串行于 PM-32 | independent | N/A | N/A |
| PM-03 | implementation | rev-list --missing缺失对象动作 | immutable-release | yes | yes | 7/8 | 4/8 | 1/2 | M | PM-02,DEP-PM-11 | 串行于 PM-02 | independent | N/A | N/A |
| PM-04 | implementation | replace列表glob与模式参数校验 | immutable-release | yes | yes | 8/8 | 8/8 | 1/1 | M | PM-03,DEP-PM-11 | 串行于 PM-03 | independent | N/A | N/A |
| PM-26 | implementation | replace -d删除语义 | immutable-release | yes | yes | 7/8 | 8/8 | 1/1 | M | PM-04,DEP-PM-11 | 串行于 PM-04 | independent | PM-04 | N/A |
| PM-05 | implementation | rev-parse --short不在库全长OID缩写 | immutable-release | yes | yes | 8/8 | 6/8 | 1/1 | M | PM-26,DEP-PM-10,DEP-PM-11 | 串行于 PM-26 | independent | N/A | N/A |
| PM-06 | implementation | rev-parse --show-object-format查询模式 | immutable-release | yes | yes | 8/8 | 8/8 | 1/1 | M | PM-05,DEP-PM-11 | 串行于 PM-05 | independent | N/A | N/A |
| PM-07 | implementation | blame --minimal最小差异行映射 | immutable-release | yes | yes | 8/8 | 8/8 | 2/2 | M | PM-06,DEP-PM-11,DEP-PM-13 | 串行于 PM-06 | independent | N/A | N/A |
| PM-08 | implementation | notes默认ref的core.notesRef配置解析 | immutable-release | yes | yes | 8/8 | 6/8 | 1/1 | M | PM-07,DEP-PM-11 | 串行于 PM-07 | independent | N/A | N/A |
| PM-34 | implementation | notes默认ref的GIT_NOTES_REF环境变量 | immutable-release | yes | yes | 6/8 | 6/8 | 1/1 | M | PM-08,DEP-PM-11 | 串行于 PM-08 | independent | PM-08 | N/A |
| PM-09 | implementation | notes merge manual冲突状态物化与未完结拒绝 | immutable-release | yes | yes | 8/8 | 6/8 | 2/2 | M | PM-34,DEP-PM-11 | 串行于 PM-34 | family child | N/A | N/A |
| PM-29 | implementation | notes merge --commit首次提交CAS与状态清理 | immutable-release | yes | yes | 8/8 | 6/8 | 2/2 | M | PM-09,DEP-PM-11 | 串行于 PM-09 | family child | PM-09 | N/A |
| PM-38 | implementation | notes merge --commit崩溃后重试确认 | immutable-release | yes | yes | 8/8 | 6/8 | 2/2 | M | PM-29,DEP-PM-11 | 串行于 PM-29 | family child | PM-29 | N/A |
| PM-23 | implementation | notes merge --abort/--quit | immutable-release | yes | yes | 8/8 | 6/8 | 2/2 | M | PM-38,DEP-PM-11 | 串行于 PM-38 | family child | PM-09 | N/A |
| PM-39 | implementation | notes merge控制形式与选项互斥 | immutable-release | yes | yes | 8/8 | 4/8 | 1/1 | M | PM-23,DEP-PM-11 | 串行于 PM-23 | family child | PM-23 | N/A |
| PM-24 | release | REL-PM-01 notes merge家族统一发布 | immutable-release | yes | yes | 10/12 | 8/12 | 0/0 | S | PM-09,PM-29,PM-38,PM-23,PM-39 | 串行于 PM-39 | family release point | N/A | N/A |
| PM-10 | implementation | var命令注册与身份变量查询 | immutable-release | yes | yes | 8/8 | 8/8 | 2/3 | M | PM-24,DEP-PM-11,DEP-PM-13 | 串行于 PM-24 | independent | N/A | N/A |
| PM-10a | implementation | var作者身份环境变量 | immutable-release | yes | yes | 8/8 | 6/8 | 2/2 | M | PM-10,DEP-PM-11,DEP-PM-13 | 串行于 PM-10 | independent | PM-10 | N/A |
| PM-37 | implementation | var提交者身份环境变量 | immutable-release | yes | yes | 8/8 | 4/8 | 1/1 | M | PM-10a,DEP-PM-11 | 串行于 PM-10a | independent | PM-10a | N/A |
| PM-22 | implementation | var编辑器变量 | immutable-release | yes | yes | 6/8 | 4/8 | 1/1 | M | PM-37,DEP-PM-11 | 串行于 PM-37 | independent | PM-10 | N/A |
| PM-36 | implementation | var序列编辑器变量 | immutable-release | yes | yes | 5/8 | 4/8 | 1/1 | M | PM-22,DEP-PM-11 | 串行于 PM-22 | independent | PM-22 | N/A |
| PM-11 | implementation | ls-files -o排除冲突路径 | immutable-release | yes | yes | 4/8 | 6/8 | 1/1 | M | PM-36,DEP-PM-03,DEP-PM-11 | 串行于 PM-36 | independent | N/A | N/A |
| PM-12 | implementation | ls-files缓存列表逐stage输出冲突条目 | immutable-release | yes | yes | 6/8 | 6/8 | 1/1 | M | PM-11,DEP-PM-03,DEP-PM-11 | 串行于 PM-11 | independent | N/A | N/A |
| PM-15 | implementation | 属性引擎宏展开与Git优先级求值 | immutable-release | yes | yes | 8/8 | 8/8 | 1/1 | M | PM-12,DEP-PM-09,DEP-PM-11 | 串行于 PM-12 | independent | N/A | N/A |
| PM-27 | implementation | check-attr属性名校验 | immutable-release | yes | yes | 6/8 | 4/8 | 1/1 | M | PM-15,DEP-PM-11 | 串行于 PM-15 | independent | PM-15 | N/A |
| PM-28 | implementation | check-attr -a登记次序输出 | immutable-release | yes | yes | 4/8 | 6/8 | 2/2 | M | PM-27,DEP-PM-09,DEP-PM-11 | 串行于 PM-27 | independent | PM-15 | N/A |
| PM-18 | implementation | 全局属性与忽略来源解析 | immutable-release | yes | yes | 8/8 | 8/8 | 1/2 | M | PM-28,DEP-PM-09,DEP-PM-11,DEP-PM-13 | 串行于 PM-28 | independent | N/A | N/A |
| PM-25 | implementation | var环境与路径类变量及-l全表 | immutable-release | yes | yes | 8/8 | 4/8 | 1/1 | M | PM-18,DEP-PM-11 | 串行于 PM-18 | independent | PM-22 | N/A |
| PM-16 | implementation | shortlog人读输出格式与排序 | immutable-release | yes | yes | 8/8 | 6/8 | 1/1 | M | PM-25,DEP-PM-11 | 串行于 PM-25 | independent | N/A | N/A |
| PM-30 | implementation | shortlog多分组与提交内去重 | immutable-release | yes | yes | 6/8 | 6/8 | 1/1 | M | PM-16,DEP-PM-11 | 串行于 PM-16 | independent | PM-16 | N/A |
| PM-19 | implementation | shortlog提交数限制 | immutable-release | yes | yes | 4/8 | 4/8 | 2/2 | M | PM-30,DEP-PM-11,DEP-PM-13 | 串行于 PM-30 | independent | PM-16 | N/A |
| PM-40 | implementation | shortlog与check-mailmap的mailmap规范化 | immutable-release | yes | yes | 4/8 | 6/8 | 1/2 | M | PM-19,DEP-PM-11 | 串行于 PM-19 | independent | PM-19 | N/A |
| PM-35 | implementation | shortlog格式分组与--date | immutable-release | yes | yes | 5/8 | 6/8 | 1/1 | M | PM-40,DEP-PM-11 | 串行于 PM-40 | independent | PM-30 | N/A |
| PM-17 | implementation | check-ignore当前目录相对路径 | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | PM-35,DEP-PM-11 | 串行于 PM-35 | independent | N/A | N/A |
| PM-31 | implementation | check-ignore --stdin流式与C引号 | immutable-release | yes | yes | 7/8 | 6/8 | 1/1 | M | PM-17,DEP-PM-11 | 串行于 PM-17 | independent | PM-17 | N/A |
| PM-20 | implementation | check-ignore quiet与参数规则 | immutable-release | yes | yes | 7/8 | 4/8 | 1/1 | M | PM-31,DEP-PM-11 | 串行于 PM-31 | independent | PM-17 | N/A |
| PM-14 | implementation | cat-file类型与对象双参数原始输出 | immutable-release | yes | yes | 8/8 | 8/8 | 1/1 | M | PM-20,DEP-PM-08,DEP-PM-11 | 串行于 PM-20 | independent | N/A | N/A |
| PM-33 | implementation | rev-list --missing部分克隆promisor归因 | immutable-release | yes | yes | 8/8 | 4/8 | 1/1 | M | PM-14,DEP-PM-08,DEP-PM-11 | 串行于 PM-14 | independent | PM-03 | N/A |

---

### Task PM-01: 新增 `symbolic_ref` 表与实体分支互斥触发器的前向 SQLite 迁移（GC-13）（#711）

**Task type:** `migration`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 ADR-PM-03 新增前向迁移 `sql/migrations/2026111702_symbolic_ref.sql`（版本号以开工时按 `DEP-PM-05` 核对的结果为准；若需重排，先按 ER-03 修订本计划中该版本号的全部引用——文件名、AC、文档门的 `rg` 关键字与恢复说明——并经评审后再执行）：创建 `symbolic_ref(name TEXT PRIMARY KEY, target TEXT NOT NULL)` 表（只存 `refs/heads/<branch>` 形态的非 HEAD 符号引用；`HEAD` 与 `refs/remotes/<remote>/HEAD` 仍存于 `reference` 表 `Head` 行），并以 4 个 `RAISE(ABORT)` 触发器在数据库层强制「本地实体分支行 `reference(kind='Branch', remote IS NULL, name=N)` 与符号引用行 `symbolic_ref(name='refs/heads/'||N)` 不得同时存在」（`reference` 的 INSERT 与 UPDATE、`symbolic_ref` 的 INSERT 与 UPDATE 各一；`reference` 的 UPDATE 触发器声明为不带 `OF` 列清单的 `BEFORE UPDATE ON reference`，对任一列的更新都按更新后的整行判定（本卡以只改 `kind` 的 AC2 与只改 `name` 的 AC4 验收；只改 `remote` 的更新走同一触发器，不单独验收），`symbolic_ref` 的 UPDATE 触发器按更新后的 `name` 判定）；只登记到 `DatabaseRole::Repository` 的 `repository_migrations()`，同步 `sql/migrations/README.md`，并把全部钉住仓库迁移尖端版本 `2026103101` 的既有断言改为新尖端（`src/internal/db/migration.rs:3660-3662` 的 runner 长度与最大版本、`src/internal/db/schema.rs:1069`、`tests/db_migration_test.rs:375,431,453,488`）；`src/internal/db/migration.rs`/`schema.rs` 中本卡更新的单元测试尖端断言由 C 组第 ④ 步的 T-1/T-3 全量 nextest 覆盖（模板 G-03：一条命令中的多个 `--test` target 分别计门，本卡回归门已占满 VER 上限）；新增用例中只有 `symbolic_ref_migration_upgrade_contract`（使用 `ConfigDbFixture` 修改进程环境）标 `#[serial(env)]`，`symbolic_ref_triggers_enforce_exclusivity` 不改进程环境、不加标注；前者并在 `tests/SERIAL_REGISTRY.tsv` 登记、以 `sh tests/NEXTEST_GROUPS.sh` 重新生成 `.config/nextest.toml`；不改 bootstrap `sql/sqlite_20260309_init.sql`，不改 `reference` 表结构与任何写入口代码。
**Out of scope:** `symbolic-ref` 命令与 `checkout` 解析（由 `PM-21a` 承接）；其余 ref 读写路径消费 `symbolic_ref` 表（`DEFER-PM-02`）；`GlobalConfig`/`SystemConfig`/`Derived`/`Capture` 角色 schema（永久非目标：本迁移只登记于 Repository 角色，GC-13 角色判定见 Description）；已应用迁移在再次 `run_pending` 时不重复应用（runner 按 `schema_versions` 回执跳过的通用行为，由回归组 `db_migration_test` 整 target 中的既有 runner 用例守护，不单独验收）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `reference.kind` 受 CHECK 限制为 `Branch`/`Tag`/`Head`，无法表达 `refs/heads/*` 符号引用；`(name, kind)` 唯一索引只约束同 kind | `sql/sqlite_20260309_init.sql:19-29,51-55` |
| 远端 HEAD 已以 `Head` 行带 `remote` 列存储（`Branch(name)` 或 `Detached(commit)`） | `src/internal/head.rs:436-466` |
| Repository 迁移登记于 `repository_migrations()`，当前尖端 `2026103101`；`builtin_migrations()` 取 Repository 角色集 | `src/internal/db/migration.rs:2464-2470,3189-3199` |
| 钉住尖端 `2026103101` 的既有断言共 6 处（本卡同卡更新） | `src/internal/db/migration.rs:3655-3662`；`src/internal/db/schema.rs:1069`；`tests/db_migration_test.rs:375,431,453,488` |
| 旧二进制打开更高版本库按 `UnsupportedFuture` 拒绝（通用路径，已有植入未来版本的拒绝用例） | `src/internal/db/schema.rs:390-396`；`tests/db_migration_test.rs:5597-5625` |
| `issues/1048.md` 的 `I1048-05` 占用 `2026111701`（`DEP-PM-05`） | `docs/development/plan/issues/1048.md` |
| 隔离配置库夹具（设置并恢复 5 个配置相关环境变量） | `src/utils/test.rs:233-275`；用法见 `tests/db_migration_test.rs:36-130` |
| 迁移在单事务内执行，失败不写版本回执（通用 runner 行为，已有非法 DDL 用例） | `src/internal/db/migration.rs:294,422`；`tests/db_migration_test.rs:1565-1583` |

**夹具:** F01：(a) 只应用到 `2026103101` 的旧库（含 1 行 `reference(kind='Branch', name='main', remote NULL)`）再以 `run_pending` 升级；(b) 已应用全部 Repository 迁移的新库，按各 AC 先插入所需的实体分支行、标签行与 `symbolic_ref` 行（这些前置插入本身必须成功，排除「触发器拒绝一切写入」的错误实现）；(c) `ConfigDbFixture`（`src/utils/test.rs:233-275`：临时 `LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB`、HOME、`XDG_CONFIG_HOME`，以作用域守卫恢复原进程环境；用例标 `#[serial(env)]`），以 `db::create_database_for_role` 建立两库并向各自 `config_kv` 插入一行 `(key='test.key', value='v', encrypted=0)`；(d) 失败注入：新建库以 `builtin_runner()` 的 `run_pending_up_to(conn, <前一版本>)`（`src/internal/db/migration.rs:376-380`）只应用到 `2026111702` 之前最后一个已登记的 Repository 迁移（开工时按 `DEP-PM-05` 核对，预期为 `I1048-05` 的 `2026111701`；因每个迁移独立事务提交，注入点前不得留有其它待应用迁移），再执行 `CREATE VIEW symbolic_ref AS SELECT 1 AS name`（SQLite 下 `CREATE TABLE symbolic_ref` 报 `view symbolic_ref already exists`，`CREATE TABLE IF NOT EXISTS` 虽跳过，但随后在其上建 `BEFORE` 触发器报 `cannot create BEFORE trigger on view`，故迁移必然失败）。「配置库隔离快照」= 两库各自 `sqlite_master` 全部行与其中每个表的全部行（按 rowid 排序、全部列）；「迁移库快照」= 被升级库的 `sqlite_master` 全部行与 `schema_versions`、`reference` 两表全部行（按 rowid 排序、全部列）。`src/internal/db/schema.rs` 只改其 `#[cfg(test)]` 断言，不计生产文件。

**Acceptance criteria:**
- [ ] **AC1** 从 `2026103101` 升级的旧库在升级前后 `reference` 表全部行（`id` 与全部列）逐行相等。
- [ ] **AC2** 已有符号引用 `refs/heads/a` 与标签行 `reference(kind='Tag', name='a', remote NULL)` 时，只改 `kind` 的 `UPDATE reference SET kind='Branch'`（作用于该标签行）被触发器拒绝。
- [ ] **AC3** 已有符号引用 `refs/heads/a` 时，`INSERT reference(kind='Branch', name='a', remote NULL)` 被触发器拒绝。
- [ ] **AC4** 已有符号引用 `refs/heads/a` 与实体分支 `b` 时，把 `b` 行 `UPDATE` 改名为 `a` 被触发器拒绝。
- [ ] **AC5** 已有本地实体分支 `main` 时，`INSERT symbolic_ref(name='refs/heads/main', …)` 被触发器拒绝。
- [ ] **AC6** 已有实体分支 `main` 与符号引用 `refs/heads/c` 时，把 `refs/heads/c` 行 `UPDATE` 改名为 `refs/heads/main` 被触发器拒绝。
- [ ] **AC7** 在 `ConfigDbFixture` 隔离环境中，把 F01(a) 旧库以 `run_pending` 升级到新尖端并以 `db::establish_connection_for_role` 重新打开 GlobalConfig 与 SystemConfig 两库之后，「配置库隔离快照」与升级前逐字相等。
- [ ] **AC8** 在已应用到 `2026111702` 之前最后一个已登记 Repository 迁移、并预置同名视图 `symbolic_ref`（`CREATE VIEW symbolic_ref AS SELECT 1 AS name`）的库上 `run_pending` 失败后，「迁移库快照」与调用前逐字相等。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列三个文件（`COMPATIBILITY.md`、`docs/development/commands/_compatibility.md`、`sql/migrations/README.md`）按「文档与网站验收命令」第 1 步逐文件验收（本卡无命令页与网站页改动）。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `symbolic_ref_migration_upgrade_contract`（`db_migration_test`）：升级前后 `SELECT * FROM reference ORDER BY id` 结果逐行相等
- AC2 → `symbolic_ref_triggers_enforce_exclusivity`（`db_migration_test`）：该语句返回含触发器消息的 SQLite 错误（只声明 `UPDATE OF name` 的触发器不会拦截它）
- AC3 → `symbolic_ref_triggers_enforce_exclusivity`（`db_migration_test`）：该语句返回含触发器消息的 SQLite 错误
- AC4 → `symbolic_ref_triggers_enforce_exclusivity`（`db_migration_test`）：该语句返回含触发器消息的 SQLite 错误
- AC5 → `symbolic_ref_triggers_enforce_exclusivity`（`db_migration_test`）：该语句返回含触发器消息的 SQLite 错误
- AC6 → `symbolic_ref_triggers_enforce_exclusivity`（`db_migration_test`）：该语句返回含触发器消息的 SQLite 错误
- AC7 → `symbolic_ref_migration_upgrade_contract`（`db_migration_test`）：`run_pending` 与两次打开均成功（前提）；前后「配置库隔离快照」逐字相等
- AC8 → `symbolic_ref_migration_upgrade_contract`（`db_migration_test`）：`run_pending` 返回 `Err`（前提）；调用前后「迁移库快照」逐字相等（无 `2026111702` 回执、无新增触发器或表）
- 文档 AC → 「文档与网站验收命令」第 1 步

**失败路径不变量:** 被触发器拒绝的语句执行前后 `reference` 与 `symbolic_ref` 两表全部行逐行相等；其余用例只读或为迁移本身。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test db_migration_test -E 'test(=symbolic_ref_migration_upgrade_contract)'` `(new)`：覆盖 AC1、AC7–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test db_migration_test -E 'test(=symbolic_ref_triggers_enforce_exclusivity)'` `(new)`：覆盖 AC2–AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test db_migration_test --test compat_serial_registry`（共享回归组：两个整 target——`db_migration_test` 的迁移注册表与既有迁移回归（含 `every_migration_sql_file_is_referenced_by_the_registry`、`builtin_migrations_register_current_schema_migrations` 与本卡更新的四处尖端断言），以及 `compat_serial_registry`（本卡新增 `#[serial(env)]` 用例后 `tests/SERIAL_REGISTRY.tsv` 与 `tests/SERIAL_CLASSIFY.sh` 分类一致、`.config/nextest.toml` 已按 `sh tests/NEXTEST_GROUPS.sh` 重新生成）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先 `source .env.test && cargo nextest list --test db_migration_test --test compat_serial_registry` 记录 N，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1, T-3: `sql/**` 迁移与 `src/internal/db/migration.rs`（ER-13 T-1）；新增迁移（`Task type = migration`，ER-13 T-3）；另含版本面 `install.sh`/`install.ps1`
**Dependencies:** `DEP-PM-05`、`DEP-PM-11`、`DEP-PM-13`
**Deliverables:** `N/A`
**Implementation write set:** `sql/migrations/2026111702_symbolic_ref.sql`, `sql/migrations/README.md`, `src/internal/db/migration.rs`, `src/internal/db/schema.rs`, `tests/db_migration_test.rs`, `COMPATIBILITY.md`, `docs/development/commands/_compatibility.md`, `tests/SERIAL_REGISTRY.tsv`, `.config/nextest.toml`
**Release write set:** `Inherited`
**Files likely touched:** `sql/migrations/2026111702_symbolic_ref.sql`, `src/internal/db/migration.rs`, `src/internal/db/schema.rs`, `tests/db_migration_test.rs`
**Docs and compatibility impact:** `COMPATIBILITY.md` 的 `symbolic-ref` 行（W-1 定位，基线第 204 行）原地补记「新增 Repository 迁移 `2026111702`；迁移后的仓库不可被早于本卡的旧二进制打开（`UnsupportedFuture`），只能前滚」；`docs/development/commands/_compatibility.md` 同步该降级不兼容与恢复说明（快照恢复或前滚 `patch + 1`）。`sql/migrations/README.md` 登记 `2026111702_symbolic_ref.sql`（版本、名称、Repository 角色与 forward-only）。无用户命令行为变化：`docs/commands/symbolic-ref.md` EN/zh 与网站页在 `PM-21a` 同步（本卡不改，理由：本卡不改变任何命令输出）。
**Rollback mode:** `forward-only`
**Migration and rollback:** 前向增量迁移 `2026111702_symbolic_ref.sql`，无 down；数据库角色仅 `Repository`（GC-13 allowlist：`repository_migrations()`；`GlobalConfig`/`SystemConfig`/`Derived`/`Capture` 排除，无 bootstrap 与 top-up 变更）。数据不变量：本地实体分支与 `refs/heads/` 符号引用同名不可并存（触发器强制）。前滚、失败注入与配置库隔离验证：`source .env.test && cargo nextest run --test db_migration_test -E 'test(=symbolic_ref_migration_upgrade_contract)'`（AC1、AC7–AC8）。用户影响：迁移后的仓库不能再用早于本卡的旧二进制打开（`UnsupportedFuture`），只能升级或从迁移前快照恢复。
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=migration; axis=symbolic_ref表与实体分支互斥触发器前向迁移; recovery=推送前 revert 本卡提交；发布后只前滚新 patch（forward-only，已迁移仓库不降级）; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=2; scope=M; deps=DEP-PM-05,DEP-PM-11,DEP-PM-13; writeset=no-overlap（串行链起点）; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-21a: 支持 `symbolic-ref` 读写/删除 `refs/heads/*` 符号引用与 `checkout` 经符号引用分支解析（#711；拆出 PM-32）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 消费 `PM-01` 的 `symbolic_ref` 表：`src/command/symbolic_ref.rs` 在 `HEAD`（行为不变）之外接受 `refs/heads/<branch>` 形态的名字——写入目标须为合法的 `refs/heads/*` 或 `refs/remotes/<remote>/<branch>` 全名（允许悬空），读取输出目标全名，`-d` 删除该行；创建前在同一写事务内检查同名本地实体分支（`PM-01` 触发器为第二道防线），其它命名空间的名字与目标以 `LBR-CLI-003` 拒绝；名字与目标中分支名部分的合法性沿用既有 `validate_branch_name`（`is_valid_git_branch_name`，`src/command/symbolic_ref.rs:193-212`）。在 `src/internal/branch.rs` 新增只供 `checkout` 使用的 `Branch::find_branch_or_symref_result_with_conn`：本地实体分支行不存在时解析同名 `refs/heads/*` 符号引用（只解析一层：其目标为远端跟踪引用或本地实体分支时取该引用的提交；目标本身仍是符号引用的链式情形按「分支不存在」的既有失败路径处理，链式解析延后至 `DEFER-PM-02`）；`src/command/checkout.rs:567` 的目标提交解析改调它，使 `checkout -f <symref-branch>` 连续两次成功（`t2018#2`）；`:626` 的实体分支判定不变，故符号引用名走既有 `checkout_detached`，HEAD 分离到解析出的提交（有意差异：Git 把 HEAD 附着到符号引用链尾的引用，如 `refs/remotes/origin/branch1`，Git 2.54.0 预探；Libra 的 HEAD 只能附着本地实体分支）。既有 `Branch::find_branch_result`/`find_branch_result_with_conn` 不修改，故经它们解析分支的 `rev-parse`、`log` 等路径（`src/command/rev_parse.rs:682-696`、`src/utils/util.rs:2064,2205-2208`）行为不变。（按 G-09 拆出 `PM-32` 承接 `refs/remotes/<remote>/HEAD` 的读写删除。）
**Out of scope:** `refs/remotes/<remote>/HEAD`（`PM-32`）；`branch` 列表/删除/改名、`for-each-ref`、`show-ref`、`rev-parse`/`log` 等其余 ref 读写路径消费 `symbolic_ref`、检出后附着到符号引用分支或其链尾引用并在其上提交（本卡检出一律分离 HEAD）、`symbolic-ref HEAD` 对二级符号引用的递归展开（`DEFER-PM-02`）；`refs/tags/*`、`refs/notes/*` 等其它命名空间的符号引用（本计划不实现，延后至 `DEFER-PM-02`，见 ADR-PM-03 的 Revisit when）；不单独验收的同路径情形：目标引用尚不存在（悬空，写入只校验目标全名的形态，与 AC1 同一写入校验）、目标为本地实体分支（与远端跟踪目标走同一写入校验与同一检出解析调用，本卡以远端跟踪目标验收）、非法分支名（如 `refs/heads/a..b`，沿用既有 `validate_branch_name` 以 `LBR-CLI-003` 拒绝）；`-d HEAD` 与 `-d <实体分支>` 的既有拒绝（行为不变，由本卡加强后的 `symbolic_ref_delete_matrix` 回归守护：`main` 为实体分支行时 `-d refs/heads/main` 失败且引用快照不变）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `validate_name` 只接受 `HEAD`，其余名字报 `unsupported symbolic ref`（LBR-CLI-003）；`-d` 一律拒绝 | `src/command/symbolic_ref.rs:97-113,152-162` |
| 目标只接受 `refs/heads/<branch>` | `src/command/symbolic_ref.rs:193-203`（`branch_name_from_full_ref`） |
| `checkout` 经 `Branch::find_branch_result` 解析分支名；该函数只查 `reference` 行 | `src/command/checkout.rs:567`；`src/internal/branch.rs:429-453` |
| 既有 `-d` 拒绝矩阵（`-d HEAD` 与 `-d refs/heads/main` 均 128、`not a symbolic ref` 等文案）；该用例只 `init` 未提交，`main` 尚是未出生分支（无实体分支行）。本卡同卡在其中先提交一次使 `main` 成为实体分支行，并在 `-d refs/heads/main` 前后断言「引用快照」逐字相等；原有断言（退出码与文案）保持不变（「验证执行协议」第 5 项） | `tests/command/symbolic_ref_test.rs:192-240`（`symbolic_ref_delete_matrix`）；`src/command/init.rs:2323-2356` |
| Git：`git symbolic-ref refs/heads/a-branch refs/remotes/origin/branch1` 后连续两次 `checkout -f a-branch` 均成功；`symbolic-ref -d refs/heads/a-branch` 删除后再读报 `not a symbolic ref`（Git 2.54.0 预探） | `git@6de20f6092:t/t2018-checkout-branch.sh:89-99` |
| `rev-parse` 与修订解析经 `Branch::find_branch_result` 解析分支（本卡不改该函数） | `src/command/rev_parse.rs:682-696`；`src/utils/util.rs:2064,2205-2208` |
| `checkout` 以 `Branch::find_branch_result` 判定实体分支，否则走 `checkout_detached` | `src/command/checkout.rs:625-640` |
| `branch --show-current` 的 JSON 含 `detached` 字段 | `src/command/branch.rs:170-177` |
| 目标分支名合法性校验 `validate_branch_name`（`is_valid_git_branch_name`） | `src/command/symbolic_ref.rs:193-212` |

**夹具:** F21：源仓库 S（`init_repo_via_cli` 新建仓库并 `configure_identity_via_cli`（`tests/command/mod.rs:636,687`；不使用会额外提交 `tracked.txt` 的 `create_committed_repo_via_cli`）；`main` 上提交 C1 写 `f.txt`=`c1\n`，新建分支 `branch1` 并提交 C2 把 `f.txt` 改为 `c2\n`）；工作仓库 W（同法新建并提交 C0 写 `w.txt`）：`libra remote add -m branch1 origin <S 路径>`、`libra fetch origin`（得到 `refs/remotes/origin/branch1` = C2、`refs/remotes/origin/main` = C1，远端 HEAD 行指向 `branch1`），并 `libra tag v1` 打在 C0 上。全部经 `run_libra_command*` 子进程执行；建好 S 后以 `libra rev-parse branch1` 记录 C2 的 OID。

**Acceptance criteria:**
- [ ] **AC1** `libra symbolic-ref refs/heads/a-branch refs/remotes/origin/main` 后再以 `libra symbolic-ref refs/heads/a-branch refs/remotes/origin/branch1` 改写其目标，随后 `libra symbolic-ref refs/heads/a-branch` 输出改写后的目标。
- [ ] **AC2** 创建 `refs/heads/a-branch` 后连续两次 `libra checkout -f a-branch`，第二次之后 `libra rev-parse HEAD` 等于 C2。
- [ ] **AC3** `libra symbolic-ref -d refs/heads/a-branch` 之后再读取该名字以 `LBR-CLI-003` 失败。
- [ ] **AC4** 目标名已有本地实体分支时 `libra symbolic-ref refs/heads/main refs/remotes/origin/branch1` 以 `LBR-CLI-003` 失败。
- [ ] **AC5** 名字 `refs/tags/x` 的写入（`libra symbolic-ref refs/tags/x refs/heads/main`）以 `LBR-CLI-003` 失败。
- [ ] **AC6** 第二次 `libra checkout -f a-branch` 之后工作区 `f.txt` 的字节为 `c2\n`（检出确实把工作区更新到 C2，而非只移动 HEAD）。
- [ ] **AC7** 目标为标签（`libra symbolic-ref refs/heads/x refs/tags/v1`）以 `LBR-CLI-003` 失败。
- [ ] **AC8** 第二次 `libra checkout -f a-branch` 之后 HEAD 为分离状态：`libra --json branch --show-current` 的 `data.detached` 为 `true`。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`refs/heads/<branch>`（`symbolic-ref` 的 EN/zh 页）与 `symbolic-ref`（`checkout` 的 EN/zh 页）。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::symbolic_ref_test::symbolic_ref_heads_names_read_write_delete`：三次调用均 exit 0（前提，第二次为对已有符号引用的改写）；读取 stdout 逐字等于 `refs/remotes/origin/branch1\n`（只支持首次插入的实现会在第二次写入时失败）
- AC2 → `command::symbolic_ref_test::checkout_force_symref_branch_twice`：两次 checkout 与 `libra rev-parse HEAD` 均 exit 0（前提）；`rev-parse HEAD` 的 stdout 逐字等于夹具记录的 C2 OID 加 `\n`
- AC3 → `command::symbolic_ref_test::symbolic_ref_heads_names_read_write_delete`：删除 exit 0（前提）；再读的退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC4 → `command::symbolic_ref_test::symbolic_ref_heads_names_read_write_delete`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC5 → `command::symbolic_ref_test::symbolic_ref_heads_names_read_write_delete`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC6 → `command::symbolic_ref_test::checkout_force_symref_branch_twice`：两次 checkout 均 exit 0（前提）；工作区 `f.txt` 存在且字节逐字等于 `c2\n`（W 在检出前没有 `f.txt`）
- AC7 → `command::symbolic_ref_test::symbolic_ref_heads_names_read_write_delete`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC8 → `command::symbolic_ref_test::checkout_force_symref_branch_twice`：exit 0（前提）；解析 stdout 的 JSON，`data.detached` 等于 `true`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** 以稳定码失败的调用前后「引用快照」逐字相等。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::symbolic_ref_test::symbolic_ref_heads_names_read_write_delete)'` `(new)`：覆盖 AC1、AC3–AC5、AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::symbolic_ref_test::checkout_force_symref_branch_twice)'` `(new)`：覆盖 AC2、AC6、AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::symbolic_ref_test::/)'`（共享回归组：`command::symbolic_ref_test` 模块族（含既有 `symbolic_ref_delete_matrix`：本卡为其补一次提交使 `main` 成为实体分支行、补「引用快照」前后相等断言，其 HEAD/实体分支拒绝的原有断言保持不变）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::checkout_test::/)'`（共享回归组：`command::checkout_test` 模块族，守护 `checkout` 分支名解析改用新 helper；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: `src/internal/branch.rs` 为分支解析的共享单一事实源（新增 helper，GC-02，ER-13 T-1）；另含版本面 `install.sh`/`install.ps1`
**Dependencies:** `PM-01`、`DEP-PM-06`、`DEP-PM-11`、`DEP-PM-15`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/symbolic_ref.rs`, `src/internal/branch.rs`, `src/command/checkout.rs`, `tests/command/symbolic_ref_test.rs`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`, `docs/commands/checkout.md`, `docs/commands/zh-CN/checkout.md`, `docs/development/commands/checkout.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/checkout.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/checkout.rs`, `src/command/symbolic_ref.rs`, `src/internal/branch.rs`, `tests/command/symbolic_ref_test.rs`
**Docs and compatibility impact:** `docs/commands/symbolic-ref.md` 与 `docs/commands/zh-CN/symbolic-ref.md`：`refs/heads/<branch>` 名字、目标约束、`-d` 语义与 `checkout` 解析；`docs/development/commands/symbolic-ref.md`：ADR-PM-03 设计与 `DEFER-PM-02` 边界；`COMPATIBILITY.md` `symbolic-ref` 行（W-1，基线第 204 行）原地改写支持面与有意差异（其它命名空间拒绝、`symbolic-ref HEAD` 不递归展开、检出符号引用分支时 HEAD 分离）；`docs/commands/checkout.md` 与 `docs/commands/zh-CN/checkout.md`：检出符号引用分支（`refs/heads/*` 指向其它引用）时解析到目标提交并使 HEAD 分离（有意差异：Git 附着到链尾引用）；`docs/development/commands/checkout.md` 同步；`COMPATIBILITY.md` 的 `checkout` 行（W-1 定位）原地补记该有意差异；网站页 `symbolic-ref.en.md` 与 `checkout.en.md` 按「文档与网站验收命令」对本卡命令集合 {`symbolic-ref`、`checkout`} 逐页同步（第 3 步 `--only symbolic-ref checkout`）；checkout 的源码、三份文档与网站页另受 `DEP-PM-15` 跨计划写锁约束。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=symbolic-ref的refs/heads符号引用与checkout解析; recovery=推送前 revert 本卡提交；发布后只前滚新 patch（已迁移 2026111702 的仓库不可降级至 PM-01 之前的二进制）; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=3; scope=M; deps=PM-01,DEP-PM-06,DEP-PM-11,DEP-PM-15; writeset=串行于 PM-01; release=independent; split-from=PM-01; exception=N/A`

---

### Task PM-32: 支持 `symbolic-ref` 读写/删除 `refs/remotes/<remote>/HEAD`（#711；自 PM-21a 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：`symbolic-ref` 支持 `refs/remotes/<remote>/HEAD`——`remote_head_remote` 解析名称，`run_remote_head` 读（`Head::remote_current_result`，缺失/非分支 → `LBR-CLI-003`）、写（仅接受同一远端 `refs/remotes/<remote>/<branch>`，其它远端或 `refs/heads/*` → `LBR-CLI-003`，为有意差异）、删（新增 `Head::delete_remote_head`，事务内 `delete_many` 远端 HEAD 行）；本地 `HEAD` 行为不变。AC1–AC5 由 `command::symbolic_ref_test::symbolic_ref_remote_head_read_write_delete` 覆盖；`symbolic_ref_test` 11/11；EN/zh 文档与 `COMPATIBILITY.md` 的 `symbolic-ref` 行同步。
**Description:** 自 `PM-21a` 拆出：`src/command/symbolic_ref.rs` 接受 `refs/remotes/<remote>/HEAD`：读取既有 `Head` 远端行并输出 `refs/remotes/<remote>/<branch>`；写入时目标须为 `refs/remotes/<同一 remote>/<branch>`（Libra 远端 HEAD 只存同一远端的分支名，其它目标以 `LBR-CLI-003` 拒绝，有意差异）；`-d` 删除该远端 HEAD 行（必要时在 `src/internal/head.rs` 增加删除远端 HEAD 的 helper）。
**Out of scope:** `refs/heads/*` 符号引用与 `checkout`（`PM-21a`）；远端 HEAD 指向分离提交时的写入（尚未排期）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `validate_name` 只接受 `HEAD` | `src/command/symbolic_ref.rs:152-162` |
| 远端 HEAD 读写 API 与 `remote add -m` 写入 | `src/internal/head.rs:436-466`；`src/command/remote.rs:846-856` |
| Git：`git symbolic-ref refs/remotes/origin/HEAD` 输出 `refs/remotes/origin/<branch>`；`-d` 删除（Git 2.54.0 预探） | `git@6de20f6092:builtin/symbolic-ref.c`；`t/t2018-checkout-branch.sh:89-99` |

**夹具:** F21：源仓库 S（`init_repo_via_cli` 新建仓库并 `configure_identity_via_cli`（`tests/command/mod.rs:636,687`；不使用会额外提交 `tracked.txt` 的 `create_committed_repo_via_cli`）；`main` 上提交 C1 写 `f.txt`=`c1\n`，新建分支 `branch1` 并提交 C2 把 `f.txt` 改为 `c2\n`）；工作仓库 W（同法新建并提交 C0 写 `w.txt`）：`libra remote add -m branch1 origin <S 路径>`、`libra fetch origin`（得到 `refs/remotes/origin/branch1` = C2、`refs/remotes/origin/main` = C1，远端 HEAD 行指向 `branch1`），并 `libra tag v1` 打在 C0 上。全部经 `run_libra_command*` 子进程执行。

**Acceptance criteria:**
- [ ] **AC1** `libra symbolic-ref refs/remotes/origin/HEAD` 输出 `refs/remotes/origin/branch1`。
- [ ] **AC2** `libra symbolic-ref refs/remotes/origin/HEAD refs/remotes/origin/main` 后再读输出 `refs/remotes/origin/main`。
- [ ] **AC3** `libra symbolic-ref -d refs/remotes/origin/HEAD` 之后再读取以 `LBR-CLI-003` 失败。
- [ ] **AC4** 为 `refs/remotes/origin/HEAD` 写入其它远端的目标 `refs/remotes/upstream/x` 以 `LBR-CLI-003` 失败。
- [ ] **AC5** 为 `refs/remotes/origin/HEAD` 写入本地分支目标 `refs/heads/main` 以 `LBR-CLI-003` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`refs/remotes/<remote>/HEAD`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::symbolic_ref_test::symbolic_ref_remote_head_read_write_delete`：exit 0；stdout 逐字等于 `refs/remotes/origin/branch1\n`
- AC2 → `command::symbolic_ref_test::symbolic_ref_remote_head_read_write_delete`：两次调用均 exit 0；再读 stdout 逐字等于 `refs/remotes/origin/main\n`
- AC3 → `command::symbolic_ref_test::symbolic_ref_remote_head_read_write_delete`：删除 exit 0（前提）；再读的退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC4 → `command::symbolic_ref_test::symbolic_ref_remote_head_read_write_delete`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC5 → `command::symbolic_ref_test::symbolic_ref_remote_head_read_write_delete`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** 以稳定码失败的调用前后「引用快照」逐字相等。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::symbolic_ref_test::symbolic_ref_remote_head_read_write_delete)'` `(new)`：覆盖 AC1–AC5（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::symbolic_ref_test::/)'`（共享回归组：`command::symbolic_ref_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::remote_test::/)'`（共享回归组：`command::remote_test` 模块族，守护远端 HEAD 行的既有读写；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: `src/internal/head.rs` 为 HEAD 读写的共享单一事实源（GC-02，ER-13 T-1）；另含版本面 `install.sh`/`install.ps1`
**Dependencies:** `PM-21a`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/symbolic_ref.rs`, `src/internal/head.rs`, `tests/command/symbolic_ref_test.rs`, `docs/commands/symbolic-ref.md`, `docs/commands/zh-CN/symbolic-ref.md`, `docs/development/commands/symbolic-ref.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/symbolic-ref.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/symbolic_ref.rs`, `src/internal/head.rs`, `tests/command/symbolic_ref_test.rs`
**Docs and compatibility impact:** `docs/commands/symbolic-ref.md` 与 zh-CN：`refs/remotes/<remote>/HEAD` 的读写删除与目标约束；`docs/development/commands/symbolic-ref.md` 同步；`COMPATIBILITY.md` `symbolic-ref` 行（W-1，基线第 204 行）原地补记；网站页 `symbolic-ref.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=symbolic-ref远端HEAD读写删除; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=5/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=PM-21a,DEP-PM-11; writeset=串行于 PM-21a; release=independent; split-from=PM-21a; exception=N/A`

---

### Task PM-02: 支持 `libra config --default` 未显式传 `--get` 时隐式读取（#717）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `src/command/config.rs` 的 `resolve_command_typed` 中，当给出 `--default` 且没有任何显式动作（无 `--get*`/`--list`/`--unset*`/`--add`/`--remove-section`/`--rename-section`/`--import`/`edit`/`path`）且只有 `<key>` 一个位置参数时，按 Git `builtin/config.c` 的隐式 GET 走既有 `ResolvedCommand::Get` 路径（键缺失时输出默认值）；`--default` 与任何写动作、`--list` 或额外位置值同用时改为命令用法错误 `LBR-CLI-002`，且在执行任何读写前拒绝（修复当前 `--list` 忽略 `--default`、`--remove-section` 照常删除的行为）。
**Out of scope:** `--default` 与 `--get-all`/`--get-regexp` 同用（Git 拒绝、Libra 现接受，保持现状并记入 `COMPATIBILITY.md` 有意差异，理由：`test_config_get_all_with_default` 等既有用户行为）；Libra 专有 `-d` 短选项（保持）；`--default` 与 AC5–AC7 之外的其余写动作（`--unset*`、`--add`、`--rename-section`、`--import`、`edit`、`path`）同用（与 AC6、AC7 走同一前置校验分支，不单独验收）；`#717` 的 `checkout --orphan` 与 `HEAD^` 子项（`DEFER-PM-03`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--default` 不带 get 模式时报 `error: --default (-d) can only be used with --get, --get-all, or --get-regexp`（LBR-INTERNAL-001、exit 2） | `src/command/config.rs:1963-1968` |
| `--list`（`:1874`）与 `--remove-section`（`:1886`）分支先于该检查执行，故 `--default x --remove-section s` 会真的删除 section | `src/command/config.rs:1874-1930` |
| `handle_get` 在键缺失且有 default 时输出默认值 | `src/command/config.rs:3047-3058` |
| Git 隐式动作与 `--default is only applicable to --get`（129） | `git@6de20f6092:builtin/config.c:1415-1423,1443-1446`；Git 2.54.0 预探 |

**夹具:** F02：`libra init` 仓库；`libra config core.foo present`；`libra config --add multi.v one` 与 `libra config --add multi.v two`；`libra config section.key v`（供 `--remove-section section`）。

**Acceptance criteria:**
- [ ] **AC1** `libra config --default fallback missing.key` 输出 `fallback`。
- [ ] **AC2** `libra config --default "" branch.gamma.merge`（`t2017#6` 形式）输出一个空行。
- [ ] **AC3** `libra config --default fallback core.foo` 输出已有值 `present`。
- [ ] **AC4** `libra config --default fallback multi.v`（多值键）输出最后一个值 `two`。
- [ ] **AC5** `libra config --default fallback new.key v`（隐式设置形式）以 `LBR-CLI-002` 失败。
- [ ] **AC6** `libra config --default fallback --remove-section section` 以 `LBR-CLI-002` 失败。
- [ ] **AC7** `libra config --default fallback --list` 以 `LBR-CLI-002` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--default`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::config_test::config_default_implies_get`：exit 0；stdout 逐字等于 `fallback\n`
- AC2 → `command::config_test::config_default_implies_get`：exit 0；stdout 逐字等于 `\n`
- AC3 → `command::config_test::config_default_implies_get`：exit 0；stdout 逐字等于 `present\n`
- AC4 → `command::config_test::config_default_implies_get`：exit 0；stdout 逐字等于 `two\n`（Git 2.54.0 预探）
- AC5 → `command::config_test::config_default_rejected_with_non_get_actions`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC6 → `command::config_test::config_default_rejected_with_non_get_actions`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC7 → `command::config_test::config_default_rejected_with_non_get_actions`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** 以稳定码失败的调用前后「配置快照」逐字相等。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::config_test::config_default_implies_get)'` `(new)`：覆盖 AC1–AC4（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::config_test::config_default_rejected_with_non_get_actions)'` `(new)`：覆盖 AC5–AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::config_test::/)'`（共享回归组：`command::config_test` 模块族（含 `test_config_get_all_with_default`、`test_config_get_with_default` 等既有 `--default` 用例）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_global_config_schema_future`（共享回归组：整 target，既有 `config --default ... doctor --global-schema` 必须仍失败的守卫；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先 `source .env.test && cargo nextest list --test compat_global_config_schema_future` 记录 N，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-32`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/config.rs`, `tests/command/config_test.rs`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/config.rs`, `tests/command/config_test.rs`
**Docs and compatibility impact:** `docs/commands/config.md` 与 zh-CN：`--default` 的隐式读取、与写动作/`--list` 同用报错，以及 Libra 保留的 `--get-all`/`--get-regexp` 与 `-d` 差异；`docs/development/commands/config.md` 同步解析优先级；`COMPATIBILITY.md` `config` 行（W-1，基线第 246 行）原地补记；网站页 `config.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=config --default隐式读取与非读动作拒绝; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=7/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=PM-32,DEP-PM-11; writeset=串行于 PM-32; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-03: 支持 `libra rev-list --missing=<action>`（error/allow-any/print/allow-promisor）（#747；拆出 PM-33）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 为 `RevListArgs` 新增 `--missing=<action>`（`require_equals`，取值 `error`/`allow-any`/`print`/`allow-promisor`，默认 `error`），并在 `--objects` 遍历中对待输出对象以 `ClientStorage::exist_checked_many` 做批量、有界并发的存在性检查（沿用其既有分层存储与 `LIBRA_READ_POLICY` 语义，存储错误按错误上报而非当作缺失）：`error` 遇缺失对象以 `LBR-REPO-002` 失败（同时修复当前缺失 blob 被当作普通行静默输出的问题）；`allow-any` 跳过缺失对象；`print` 在全部普通对象行之后输出 `?<oid>` 行；`allow-promisor`：本卡交付时 Libra 尚无部分克隆与 promisor 概念（`src/command/clone.rs:191-198,266-272`），故对任何缺失对象都与 `error` 相同、以 `LBR-REPO-002` 失败（fail-closed）；部分克隆中按 promisor 归因跳过与「不按需拉取」由 `PM-33` 在 `DEP-PM-08` 交付后实现并验收。（按 G-09 拆出 `PM-33` 承接部分克隆中的 promisor 归因实现与 `t5616#33` 验收。）
**Out of scope:** 部分克隆中的 promisor 归因、不按需拉取及其验收（`PM-33`）；`--missing=print-info`、`--missing-only`、`--exclude-promisor-objects`、`-z` 下的 `missing=yes`（尚未排期，重启条件：后续 rev-list 部分克隆计划）；`--missing print`（以空格分隔取值）因 `require_equals` 由 clap 拒绝（clap 既有语义，不单独验收）；存储层错误（`exist_checked_many` 返回 `Err`）按错误上报而非当作缺失：这是该 API 的既有契约（`src/utils/client_storage.rs:2669-2687`），本卡只消费其结果、不新增存储故障注入设施，不单独验收；未知取值（Git 静默当作 `error`，Libra 以 `LBR-CLI-002` 拒绝，有意差异，见 AC7）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RevListArgs` 无 `missing` 字段，`--missing` 被 clap 拒绝（LBR-CLI-002） | `src/command/rev_list.rs:53-251`；`src/cli.rs:3213-3222` |
| `collect_tree_objects` 先输出 tree 再加载，blob 条目不做存在性检查直接输出；tree 加载失败报 `LBR-REPO-002` | `src/command/rev_list.rs:719-781`（`:732-743`、`:762-770`） |
| 对象行渲染 `{oid} {path}`，根 tree 为 `<oid> ` 带尾随空格；无 `?` 行 | `src/command/rev_list_output.rs:161-198` |
| 批量存在性检查 API（存储错误保留为错误） | `src/utils/client_storage.rs:2669-2687` |
| Git 动作枚举、`print` 在对象流之后输出 `?<oid>`、`allow-promisor` 要求 `is_promisor_object` | `git@6de20f6092:builtin/rev-list.c:106-113,161-239,524-557`；`packfile.c:2061-2079`；Git 2.54.0 预探 |

**夹具:** F03：`init_repo_via_cli` 新建仓库并 `configure_identity_via_cli`（`tests/command/mod.rs:636,687`；不使用会额外提交 `tracked.txt` 的 `create_committed_repo_via_cli`）；提交 1 写 `one.t`=`one\n`，提交 2 增加 `two.t`=`two\n` 与 `sub/deep.t`=`deep\n`（三者内容不同）；先断言三个 blob OID 两两不同，再以 `libra rev-list --objects HEAD` 记录完整基线 B 并断言其恰含 `one.t`、`two.t`、`sub`、`sub/deep.t` 各一行，再删除 `two.t` blob 的松散对象文件（`tests/command/mod.rs:565` `loose_object_path`）。F03b：在 F03 上再删除 tree `sub` 的松散对象。

**Acceptance criteria:**
- [ ] **AC1** F03 上 `libra rev-list --objects HEAD`（默认动作）以 `LBR-REPO-002` 失败。
- [ ] **AC2** F03 上 `libra rev-list --objects --missing=error HEAD` 以 `LBR-REPO-002` 失败。
- [ ] **AC3** F03 上 `--missing=allow-any` 的 stdout 等于基线 B 删去 `<two.t oid> two.t` 一行。
- [ ] **AC4** F03 上 `--missing=print` 的 stdout 等于基线 B 删去 `<two.t oid> two.t` 一行后再追加 `?<two.t oid>` 一行。
- [ ] **AC5** F03b 上 `--missing=print` 的 stdout 等于基线 B 删去 `sub`、`sub/deep.t`、`two.t` 三行后再追加 `?<sub tree oid>` 与 `?<two.t oid>` 两行（这两行之间的次序不限）。
- [ ] **AC6** F03（无 promisor 配置）上 `--missing=allow-promisor` 以 `LBR-REPO-002` 失败。
- [ ] **AC7** `--missing=bogus` 以 `LBR-CLI-002` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--missing=`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::rev_list_test::rev_list_missing_actions_on_local_missing_objects`：退出码非 0（前提）；stderr 含 `LBR-REPO-002`
- AC2 → `command::rev_list_test::rev_list_missing_actions_on_local_missing_objects`：退出码非 0（前提）；stderr 含 `LBR-REPO-002`
- AC3 → `command::rev_list_test::rev_list_missing_actions_on_local_missing_objects`：exit 0；stdout 逐字等于该期望文本
- AC4 → `command::rev_list_test::rev_list_missing_actions_on_local_missing_objects`：exit 0；stdout 逐字等于该期望文本（Git 2.54.0 预探：`?` 行位于全部对象行之后）
- AC5 → `command::rev_list_test::rev_list_missing_actions_on_local_missing_objects`：exit 0；stdout 去掉最后两行后逐字等于前半期望，且最后两行组成的集合等于这两个 `?` 行
- AC6 → `command::rev_list_test::rev_list_missing_actions_on_local_missing_objects`：退出码非 0（前提）；stderr 含 `LBR-REPO-002`
- AC7 → `command::rev_list_test::rev_list_missing_actions_on_local_missing_objects`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_list_test::rev_list_missing_actions_on_local_missing_objects)'` `(new)`：覆盖 AC1–AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_list_test::/)'`（共享回归组：`command::rev_list_test` 模块族（含 `test_rev_list_objects_errors_on_corrupt_subtree` 等 `--objects` 用例）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-02`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rev_list.rs`, `src/command/rev_list_output.rs`, `tests/command/rev_list_test.rs`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/rev_list.rs`, `src/command/rev_list_output.rs`, `tests/command/rev_list_test.rs`
**Docs and compatibility impact:** `docs/commands/rev-list.md` 与 zh-CN：`--missing=<action>` 四个取值、`?<oid>` 输出位置、缺失对象默认失败（修正 `:57` 所述行为）、`allow-promisor` 在尚无 promisor 支持时等同 `error`；`docs/development/commands/rev-list.md` 同步；`COMPATIBILITY.md` `rev-list` 行（W-1，基线第 218 行）原地补记并修正「缺失 blob 仍逐字匹配 Git」表述；网站页 `rev-list.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=rev-list --missing缺失对象动作; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=PM-02,DEP-PM-11; writeset=串行于 PM-02; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-04: 修复 `libra replace` 列表 glob 与模式/参数校验（#758；拆出 PM-26）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git `builtin/replace.c` 修正 `src/command/replace.rs` 的列表与模式判定：`-l [<pattern>]` 以 wildmatch 整名匹配（默认 `*`），取代子串过滤，并同步把 `ReplaceArgs` 中 `-l` 的帮助文本由「optionally filtered by a substring」改为描述整名 wildmatch（`src/command/replace.rs:155-170`）；`-l` 只允许一个 pattern；无模式且恰一个位置参数、`-l` 与 `-d` 同用、`--format` 用于非列表模式、`-f` 用于非写入模式均在任何读写前以 `LBR-CLI-002` 拒绝。（按 G-09 拆出 `PM-26` 承接 `-d` 删除语义。）
**Out of scope:** `-d` 的解析、逐项继续与错误语义（`PM-26`）；`update-ref refs/replace/*`、`cat-file` 尊重 replace、`--no-replace-objects` 等 `t6050` 其余阻断项（`DEFER-PM-07`）；`-l` 帮助文本的措辞（随实现同卡修改，由 ER-05 代码评审与文档门核对，不单独验收）；`-l` 不带模式时列出全部替换引用（实现以默认模式 `*` 走与 AC1 相同的 wildmatch 过滤，`*` 匹配任意名字，不单独验收；既有 `replace_list_shows_the_replaced_object` 只守护单个引用的列出）；`-f -l` 等其余 `-f` 误用组合（同一校验分支，不单独验收）；`--format=bogus` 的 Git 退出码 255（Libra 保持 clap 用法错误 `LBR-CLI-002`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ReplaceArgs` 无任何 `conflicts_with`/arg group | `src/command/replace.rs:155-197` |
| `run` 的分派顺序：`delete` 先于 `list`，`args.len() <= 1` 即列表 | `src/command/replace.rs:319-351` |
| 列表以 `name.contains(p)` 子串过滤 | `src/command/replace.rs:493-512` |
| Git：`-l` 用 wildmatch（`list_replace_refs`）；`only one pattern can be given with -l`、`bad number of arguments`、`-d`/`-l` 互斥、`--format cannot be used when not listing`、`-f only makes sense when writing a replacement` 均 129 | `git@6de20f6092:builtin/replace.c:77-105,545-639`；Git 2.54.0 预探 |

**夹具:** F04：`init_repo_via_cli` 新建仓库并 `configure_identity_via_cli`（`tests/command/mod.rs:636,687`；不使用会额外提交 `tracked.txt` 的 `create_committed_repo_via_cli`）；`main` 上提交 C1、C2、C3，分支 `side` 上提交 X1、X2；`libra replace <C1> <X1>`、`libra replace <C2> <X2>`（全 OID）。P = C1 全 OID 的前 8 位十六进制（夹具断言 C2 不以 P 开头，否则换用前 10 位）。Q = C1 全 OID 的第 1 位、字符 `?`、C1 全 OID 的第 3–8 位与字符 `*` 依次拼接（夹具断言 C2 的全 OID 不与 Q 匹配，即其第 1 位或第 3–8 位与 C1 不同；否则把定长段延长到第 3–10 位）。

**Acceptance criteria:**
- [ ] **AC1** `libra replace -l '<Q>'`（含 `?` 与尾部 `*` 的 wildmatch 模式）只输出 C1。
- [ ] **AC2** `libra replace -l P`（无通配符）输出为空。
- [ ] **AC3** `libra replace -l a b` 以 `LBR-CLI-002` 失败。
- [ ] **AC4** `libra replace <C1>`（无模式、单个位置参数）以 `LBR-CLI-002` 失败。
- [ ] **AC5** `libra replace -l -d <C2>` 以 `LBR-CLI-002` 失败。
- [ ] **AC6** `libra replace --format=medium -d <C1>` 以 `LBR-CLI-002` 失败。
- [ ] **AC7** `libra replace -f`（无其它模式）以 `LBR-CLI-002` 失败。
- [ ] **AC8** `libra replace -f -d <C1>` 以 `LBR-CLI-002` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：wildmatch。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::replace_test::replace_list_uses_wildmatch`：exit 0；stdout 逐字等于 `<C1>\n`（Git 2.54.0 预探：`?` 匹配任一字符；只把尾部 `*` 当前缀匹配、其余按字面比较的实现输出为空）
- AC2 → `command::replace_test::replace_list_uses_wildmatch`：exit 0；stdout 为空（Git 2.54.0 预探：wildmatch 整名匹配）
- AC3 → `command::replace_test::replace_rejects_invalid_mode_combinations`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC4 → `command::replace_test::replace_rejects_invalid_mode_combinations`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC5 → `command::replace_test::replace_rejects_invalid_mode_combinations`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC6 → `command::replace_test::replace_rejects_invalid_mode_combinations`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC7 → `command::replace_test::replace_rejects_invalid_mode_combinations`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC8 → `command::replace_test::replace_rejects_invalid_mode_combinations`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** 以稳定码失败的调用前后「replace 快照」逐字相等。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::replace_test::replace_list_uses_wildmatch)'` `(new)`：覆盖 AC1–AC2（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::replace_test::replace_rejects_invalid_mode_combinations)'` `(new)`：覆盖 AC3–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::replace_test::/)'`（共享回归组：`command::replace_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::replace::tests::/)'`（共享回归组：`src/command/replace.rs` 单元测试；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-03`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/replace.rs`, `tests/command/replace_test.rs`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/replace.rs`, `tests/command/replace_test.rs`
**Docs and compatibility impact:** `docs/commands/replace.md` 与 zh-CN（zh-CN 现仍写 `--format`/`--edit`/`--graft` 未实现，同卡改正）：`-l` glob 语义与各模式校验；`docs/development/commands/replace.md` 改正「子串过滤、`--format` 延后」陈旧表述；`COMPATIBILITY.md` `replace` 行（W-1，基线第 236 行）原地改写（同时消除其「`--format` 延后」与注释列的自相矛盾）；网站页 `replace.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=replace列表glob与模式参数校验; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=PM-03,DEP-PM-11; writeset=串行于 PM-03; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-26: 修复 `libra replace -d` 删除语义（任意对象短 OID、逐项继续）（#758；自 PM-04 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-04` 拆出：`replace -d <object>...` 按 Git `for_each_replace_name` 逐项处理——以不 peel 的通用对象解析（任意类型短 OID；全长 40/64 位十六进制不要求对象存在）得到 OID，删除 `refs/replace/<oid>` 并输出 `Deleted replace ref '<oid>'`；某项无法解析或无替换时记录错误并继续处理后续项，全部处理后以 `LBR-CLI-003` 失败（退出码沿用 `fatal()` 的 128）。
**Out of scope:** 列表与模式校验（`PM-04`）；Git 对部分失败的退出码 1（有意差异：Libra 以稳定码 `LBR-CLI-003`、退出码 128 报告，见 `COMPATIBILITY.md`）；`t6050` 其余阻断项（`DEFER-PM-07`）；SHA-256 仓库的 64 位全长 OID 与 tree/commit/tag 短 OID（与 AC1 的 blob 短 OID、AC6 的 40 位全长 OID 走同一不 peel 解析，不单独验收）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `delete` 对每项先 `resolve_any`，首个失败即中止；无替换报 `fatal: no replacement for <oid>` | `src/command/replace.rs:389-406` |
| `resolve_any`：全长 hex 要求对象存在；否则走 `util::get_commit_base`（peel 到 commit），blob/tree 短 OID 失败 | `src/command/replace.rs:546-555`；`src/utils/util.rs:3160` |
| Git：短 OID 删除 blob 替换成功；`-d <未替换> <已替换>` 删除后者并报 `replace ref '<oid>' not found`（exit 1）；`-d nosuchref` 报 `failed to resolve`；不存在的全长 hex 报 `not found` | `git@6de20f6092:builtin/replace.c:110-152`；Git 2.54.0 预探 |

**夹具:** F26：`init_repo_via_cli` 新建仓库并 `configure_identity_via_cli`（`tests/command/mod.rs:636,687`；不使用会额外提交 `tracked.txt` 的 `create_committed_repo_via_cli`）；`main` 上提交 C1、C2、C3，分支 `side` 上提交 X1、X2；`libra replace <C1> <X1>`、`libra replace <C2> <X2>`（全 OID）；另以 `libra hash-object -w --stdin` 写 blob B1（`b1\n`）与 B2（`b2\n`）并 `libra replace <B1 全 OID> <B2 全 OID>`；S = B1 全 OID 前 8 位（夹具断言其唯一）；Z = `0000000000000000000000000000000000000001`（不在库），夹具直接写入替换引用文件 `.libra/refs/replace/<Z>`，内容为 `<X1 全 OID>\n`（与 `libra replace` 写出的格式相同，`src/command/replace.rs:385`）。

**Acceptance criteria:**
- [ ] **AC1** `libra replace -d S` 之后 `.libra/refs/replace/<B1>` 不存在。
- [ ] **AC2** `libra replace -d <C1> <C2>` 依次报告两项删除。
- [ ] **AC3** `libra replace -d <C3> <C1>`（C3 无替换）以 `LBR-CLI-003` 失败。
- [ ] **AC4** AC3 的调用之后 `.libra/refs/replace/<C1>` 不存在（错误项之后的项仍被处理）。
- [ ] **AC5** `libra replace -d nosuchref` 以 `LBR-CLI-003` 失败。
- [ ] **AC6** `libra replace -d <Z>`（不在库的全长 OID，但存在以它命名的替换引用）之后 `.libra/refs/replace/<Z>` 不存在。
- [ ] **AC7** `libra replace -d`（无参数）以 `LBR-CLI-002` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`Deleted replace ref`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::replace_test::replace_delete_resolves_any_object_short_oid`：exit 0（前提）；该文件不存在
- AC2 → `command::replace_test::replace_delete_resolves_any_object_short_oid`：exit 0；stdout 逐字等于 `Deleted replace ref '<C1>'\nDeleted replace ref '<C2>'\n`
- AC3 → `command::replace_test::replace_delete_continues_and_reports_errors`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC4 → `command::replace_test::replace_delete_continues_and_reports_errors`：该文件不存在
- AC5 → `command::replace_test::replace_delete_continues_and_reports_errors`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC6 → `command::replace_test::replace_delete_resolves_any_object_short_oid`：exit 0（前提）；该文件不存在
- AC7 → `command::replace_test::replace_delete_continues_and_reports_errors`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** 以稳定码失败的调用前后「replace 快照」逐字相等；唯一例外是 AC3 的调用，其后快照恰为调用前快照删去 `<C1>` 一项。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::replace_test::replace_delete_resolves_any_object_short_oid)'` `(new)`：覆盖 AC1–AC2、AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::replace_test::replace_delete_continues_and_reports_errors)'` `(new)`：覆盖 AC3–AC5、AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::replace_test::/)'`（共享回归组：`command::replace_test` 模块族（含既有 `replace_delete_missing_is_an_error` 的 128 断言，保持不变）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::replace::tests::/)'`（共享回归组：`src/command/replace.rs` 单元测试（含 `delete_mutation_path_invalidates_the_cache`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-04`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/replace.rs`, `tests/command/replace_test.rs`, `docs/commands/replace.md`, `docs/commands/zh-CN/replace.md`, `docs/development/commands/replace.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/replace.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/replace.rs`, `tests/command/replace_test.rs`
**Docs and compatibility impact:** `docs/commands/replace.md` 与 zh-CN：`-d` 接受任意对象短 OID、逐项继续、部分失败的稳定码与 128 退出码；`docs/development/commands/replace.md` 同步；`COMPATIBILITY.md` `replace` 行（W-1，基线第 236 行）原地补记退出码差异；网站页 `replace.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=replace -d删除语义; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=7/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=PM-04,DEP-PM-11; writeset=串行于 PM-04; release=independent; split-from=PM-04; exception=N/A`

---

### Task PM-05: 修复 `rev-parse --short` 对不在库中的全长 OID 的缩写（#839）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 修正 `src/command/rev_parse.rs` 的 `--short` 路径：当 spec 为当前哈希长度的全长十六进制 OID 时不再要求对象存在（`--verify` 不带 `--short` 时的存在性证明保持不变），并把缩写计算改为 Git `repo_find_unique_abbrev` 语义——从请求长度（默认 7，下限 4、上限为哈希长度）起逐位加长，直到库中没有**其它**对象共享该前缀；对象在库与否都适用（AC2 为不在库的全长 OID，AC8 为在库对象）；长度下限 4 与上限的钳制不单独验收。
**Out of scope:** `--verify <全长 hex>`（不带 `--short`）的存在性证明（保持现状，`COMPATIBILITY.md` 已登记的有意差异）；`--short` 无参数时默认 HEAD、`--short=abc` 报错、`core.abbrev`（尚未排期，重启条件：后续 rev-parse 兼容计划）；`--show-object-format`（`PM-06`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--verify`/`--short` 调用 `util::require_object_exists_typed`，不在库的全长 OID 被拒 | `src/command/rev_parse.rs:643-650`；`src/utils/util.rs:2164-2170` |
| `resolve_short_commit` 只在前缀唯一命中该对象时缩写，否则回落全长 hex；无下限 4 | `src/command/rev_parse.rs:874-898` |
| 单修订模式把解析错误统一改写为 `Needed a single revision`（LBR-CLI-003、128） | `src/command/rev_parse.rs:301-327` |
| Git：`--short <未写入 OID>` → 7 位；`--short=4 1c5a000…000` 在库中有两个 `1c5a` 前缀对象时 → `1c5a0`；sha256 库中 40 位 hex → `Needed a single revision` | `git@6de20f6092:builtin/rev-parse.c:882-893`；Git 2.54.0 预探 |

**夹具:** F05：SHA-1 仓库，以 `libra hash-object -w --stdin` 写入 `ambiguity fixture blob 307\n`（`1c5ab359df64c2b76ea748b81060685d892f0add`）与 `ambiguity fixture blob 367\n`（`1c5afe2981eb1ad5aae24ac8ae5600f54f291244`）；U = `d1dac3c4cf349b414147a37961aed67bd3ae0f8c`（`pre simple\n` 的 blob OID，不写入）。F05s：`create_committed_repo_with_format("sha256")` 仓库，U256 = `ee9c70802320d33d54542885b52dcc8bcccc5c62d41716ad7b66602bb60614b3`（同内容的 SHA-256 OID，不写入）。夹具先断言 U 与 U256 不在库中。

**Acceptance criteria:**
- [ ] **AC1** F05 上 `libra rev-parse --short U` 输出 `d1dac3c`。
- [ ] **AC2** F05 上 `libra rev-parse --short=4 1c5a000000000000000000000000000000000000` 输出 `1c5a0`（逐位加长到无其它对象共享）。
- [ ] **AC3** F05 上 `libra rev-parse --verify --short U` 输出 `d1dac3c`。
- [ ] **AC4** F05s 上 `libra rev-parse --short U256` 输出 `ee9c708`。
- [ ] **AC5** F05s 上 `libra rev-parse --short U`（40 位 hex 用于 SHA-256 库）以 `LBR-CLI-003` 失败。
- [ ] **AC6** F05 上 `libra rev-parse --short nosuchref` 以 `LBR-CLI-003` 失败。
- [ ] **AC7** F05 上 `libra rev-parse --short 1c5a`（两个对象共享的歧义前缀）以 `LBR-CLI-003` 失败。
- [ ] **AC8** F05 上 `libra rev-parse --short=4 1c5ab359df64c2b76ea748b81060685d892f0add`（在库对象，与另一在库对象共享前缀 `1c5a`）输出 `1c5ab`。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--short`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::rev_parse_test::rev_parse_short_abbreviates_absent_full_oid`：exit 0；stdout 逐字等于 `d1dac3c\n`
- AC2 → `command::rev_parse_test::rev_parse_short_abbreviates_absent_full_oid`：exit 0；stdout 逐字等于 `1c5a0\n`（Git 2.54.0 预探）
- AC3 → `command::rev_parse_test::rev_parse_short_abbreviates_absent_full_oid`：exit 0；stdout 逐字等于 `d1dac3c\n`
- AC4 → `command::rev_parse_test::rev_parse_short_abbreviates_absent_full_oid`：exit 0；stdout 逐字等于 `ee9c708\n`
- AC5 → `command::rev_parse_test::rev_parse_short_rejects_invalid_specs`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC6 → `command::rev_parse_test::rev_parse_short_rejects_invalid_specs`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC7 → `command::rev_parse_test::rev_parse_short_rejects_invalid_specs`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC8 → `command::rev_parse_test::rev_parse_short_abbreviates_absent_full_oid`：exit 0；stdout 逐字等于 `1c5ab\n`（Git 2.54.0 预探；改动前的实现对在库对象已给出同一结果，本 AC 守护改用统一缩写算法后在库对象的缩写保持不变）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::rev_parse_short_abbreviates_absent_full_oid)'` `(new)`：覆盖 AC1–AC4、AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::rev_parse_short_rejects_invalid_specs)'` `(new)`：覆盖 AC5–AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`（共享回归组：`command::rev_parse_test` 模块族（含 `test_short_oid_after_repack_matrix`、`test_ambiguous_short_oid_hint_matrix`、`test_rev_parse_verify_unresolvable_exits_128`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-26`、`DEP-PM-10`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rev_parse.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/rev_parse.rs`, `tests/command/rev_parse_test.rs`
**Docs and compatibility impact:** `docs/commands/rev-parse.md` 与 zh-CN：`--short` 对不在库全长 OID 的缩写与加长规则、`--verify` 单独使用时仍证明存在；`docs/development/commands/rev-parse.md` 修正 `:19` 的「`--short` 证明存在」表述；`COMPATIBILITY.md` `rev-parse` 顶层命令表行（W-1 取第一张表中的行，基线第 217 行；第 277 行游离行不改）原地补记；网站页 `rev-parse.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=rev-parse --short不在库全长OID缩写; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=PM-26,DEP-PM-10,DEP-PM-11; writeset=串行于 PM-26; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-06: 支持 `rev-parse --show-object-format[=storage|input|output|compat]`（#947）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 为 `RevParseArgs` 新增查询模式 `--show-object-format[=<mode>]`（`require_equals`，与 `spec` 互斥，同其它查询模式一样不解析修订）：`storage`（默认）、`input`、`output` 输出当前仓库 `core.objectformat`，`compat` 输出空行（Libra 无兼容哈希），其它取值以 `LBR-CLI-002` 拒绝；`--json` 输出 `mode` 为 `show_object_format` 的既有查询信封。
**Out of scope:** `--show-ref-format`/`--show-ref-storage-format`（`plan-20261117-clone.md` 的 `CL-14`）；`blake3` 仓库（Libra 扩展，同样原样输出 `core.objectformat`，不单独验收）、子目录执行（沿用查询模式既有的仓库发现）、空取值 `--show-object-format=` 与和修订参数同用时的 clap 互斥（Git 会按参数顺序交错输出，有意差异），均不单独验收；全局 `-C`（尚未排期，重启条件：#686 立项）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RevParseArgs` 无该字段；`spec` 的 `allow_hyphen_values` 使未知 `--flag` 被当作修订并以 LBR-CLI-003 失败 | `src/command/rev_parse.rs:59-169`（`:160-161`） |
| 查询模式分支与 JSON 输出结构 `{mode, input, value}`（如 `show_toplevel`） | `src/command/rev_parse.rs:171-176,501-540` |
| 对象格式取值 `sha1`/`sha256`/`blake3` | `src/internal/object_format.rs:20-32` |
| Git：无值即 `storage`；接受 `storage`/`compat`/`input`/`output`；`compat` 无兼容算法时输出空行；其它值 `fatal: unknown mode for --show-object-format`（128） | `git@6de20f6092:builtin/rev-parse.c:1117-1135`；Git 2.54.0 预探 |

**夹具:** F06：`create_committed_repo_with_format`（`tests/command/mod.rs:652`）构造的 `sha1` 与 `sha256` 两个仓库。

**Acceptance criteria:**
- [ ] **AC1** `sha1` 仓库中 `libra rev-parse --show-object-format` 输出 `sha1`。
- [ ] **AC2** `sha256` 仓库中 `libra rev-parse --show-object-format` 输出 `sha256`。
- [ ] **AC3** `sha256` 仓库中 `--show-object-format=storage` 输出 `sha256`。
- [ ] **AC4** `sha256` 仓库中 `--show-object-format=input` 输出 `sha256`。
- [ ] **AC5** `sha256` 仓库中 `--show-object-format=output` 输出 `sha256`。
- [ ] **AC6** `--show-object-format=compat` 输出一个空行。
- [ ] **AC7** `--show-object-format=bogus` 以 `LBR-CLI-002` 失败。
- [ ] **AC8** `libra --json rev-parse --show-object-format`（`sha1` 仓库）输出单个 JSON 信封，其 `data` 恰为 `{"mode":"show_object_format","input":null,"value":"sha1"}`。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--show-object-format`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::rev_parse_test::rev_parse_show_object_format_modes`：exit 0；stdout 逐字等于 `sha1\n`
- AC2 → `command::rev_parse_test::rev_parse_show_object_format_modes`：exit 0；stdout 逐字等于 `sha256\n`
- AC3 → `command::rev_parse_test::rev_parse_show_object_format_modes`：exit 0；stdout 逐字等于 `sha256\n`
- AC4 → `command::rev_parse_test::rev_parse_show_object_format_modes`：exit 0；stdout 逐字等于 `sha256\n`
- AC5 → `command::rev_parse_test::rev_parse_show_object_format_modes`：exit 0；stdout 逐字等于 `sha256\n`
- AC6 → `command::rev_parse_test::rev_parse_show_object_format_modes`：exit 0；stdout 逐字等于 `\n`（Git 2.54.0 预探）
- AC7 → `command::rev_parse_test::rev_parse_show_object_format_rejects_invalid_mode`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC8 → `command::rev_parse_test::rev_parse_show_object_format_modes`：exit 0（前提）；stdout 解析为一个 JSON 对象后 `data` 等于该对象
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::rev_parse_show_object_format_modes)'` `(new)`：覆盖 AC1–AC6、AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_parse_test::rev_parse_show_object_format_rejects_invalid_mode)'` `(new)`：覆盖 AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`（共享回归组：`command::rev_parse_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::rev_parse::tests::/)'`（共享回归组：`src/command/rev_parse.rs` 参数解析单元测试；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-05`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rev_parse.rs`, `tests/command/rev_parse_test.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/rev_parse.rs`, `tests/command/rev_parse_test.rs`
**Docs and compatibility impact:** `docs/commands/rev-parse.md` 与 zh-CN：新查询模式、取值、`blake3` 为 Libra 扩展、与修订参数互斥及 JSON 模式名（补入 `:136` 模式列表）；`docs/development/commands/rev-parse.md` 同步；`COMPATIBILITY.md` `rev-parse` 顶层命令表行（W-1 取第一张表中的行，基线第 217 行）原地补记；网站页 `rev-parse.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=rev-parse --show-object-format查询模式; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=PM-05,DEP-PM-11; writeset=串行于 PM-05; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-07: 支持 `libra blame --minimal` / `--no-minimal`（#848）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 为 `BlameArgs` 新增可取反的 `--minimal`（`--no-minimal` 取消，后者覆盖前者，与 Git `XDF_NEED_MINIMAL` 的末次生效一致）；启用时 `blame` 的父子行映射改用 `similar::Algorithm::RawMyers`（最小编辑脚本），默认仍走 `git_internal::diff::compute_diff`（`similar` 的有界 Myers）。为使用 `RawMyers` 把 `Cargo.toml` 的 `similar` 版本要求由 `3.1.1` 提到 `3.2.0`（`Cargo.lock` 已解析为 3.2.0，由工具链刷新而非手改）。`Cargo.toml` 同属发布写集，按 G-10 的 I–R 规则只在本卡的串行发布窗口内修改。
**Out of scope:** `--diff-algorithm` 与 `diff.algorithm` 配置（`t8015` #3–#5、#7，尚未排期，重启条件：后续 blame 兼容计划）；`diff`/`format-patch` 中 `--minimal` 注释所述「后端已保证最短编辑脚本」的陈旧表述（随后续 diff 计划修正，本卡不改 `diff.rs`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `BlameArgs` 无 `--minimal`；clap 报 `unexpected argument '--minimal'`（LBR-CLI-002） | `src/command/blame.rs:54-140` |
| 行映射调用 `compute_diff`（固定 `Algorithm::Myers`） | `src/command/blame.rs:534-546`；`$CARGO_HOME/registry/src/index.crates.io-*/git-internal-0.10.4/src/diff.rs:58-60,489` |
| `similar` 3.2.0 的 `Myers` 在高代价搜索时做有界非最小切分，`RawMyers` 保留最小语义；`Cargo.toml` 要求 `3.1.1` | `Cargo.toml:99`；`$CARGO_HOME/registry/src/index.crates.io-*/similar-3.2.0/src/types.rs:23-36` |
| Git：`--minimal` 设 `XDF_NEED_MINIMAL`，`--no-minimal` 清除，末次生效；`--minimal=1` 报错（129） | `git@6de20f6092:builtin/blame.c:870-881,1000-1002` |
| `t8015#6`：`file.txt` 由 `x x x x`（Commit_1）改为 `x x x A B C D x E F G`（Commit_2）；`--minimal` 的逐行作者为 C1 C1 C1 C2 C2 C2 C2 C1 C2 C2 C2 | `git@6de20f6092:t/t8015-blame-diff-algorithm.sh:5-55`；Git 2.54.0 预探 |
| `blame` 的父子行映射在默认与 `-w` 两个分支都调用 `git_internal::diff::compute_diff`（其内部为 `similar` 的 `Algorithm::Myers`） | `src/command/blame.rs:530-546`；`git-internal-0.10.4/src/diff.rs:50-60,489-491` |
| `similar` 3.2.0：`Myers` 带 Git 式有界搜索等启发式，可能给出非最小脚本；`RawMyers` 关闭这些启发式 | `similar-3.2.0/src/algorithms/mod.rs:55-100` |

**夹具:** F07：`init_repo_via_cli` 新建仓库并 `configure_identity_via_cli`；`t8015` 同构——Commit_1（`libra commit --author 'Commit_1 <c1@x>'`）写入 `file.txt` 为 4 行 `x`；Commit_2（`--author 'Commit_2 <c2@x>'`）把它改为 `x x x A B C D x E F G` 共 11 行。 F07g：同一仓库另建文件 `g.txt`——提交 P（`libra commit --author 'P <p@x>'`）写入 old，提交 Q（`--author 'Q <q@x>'`）改为 new；old/new 由确定性生成器产生：`x: u64 = 1`，`next()` 依次执行 `x ^= x << 13; x ^= x >> 7; x ^= x << 17` 并返回 `x`；old 为 1000 行，第 i 行为 `L{next() % 200}`；new 先按序遍历 old、`next() % 3 != 0` 时保留该行，再重复 333 次：`p = next() % (new.len() + 1)`、`v = next() % 200`、在下标 p 处插入 `L{v}`；每行以 `\n` 结尾时 old 为 1000 行、SHA-256 `46b9dc84583da4c49605498905318dd3f8963e48a9186997ef4d21d565508c30`，new 为 1017 行、SHA-256 `5c3ffa835f837a33acfa6ab3c4883c94d544d7ba74d11d2540cdd671ae18fbd2`（测试先断言两者）。该夹具上两版本最长公共子序列为 684 行（Git 2.54.0 `git diff --no-index --minimal --numstat` 为 333 增、316 删；`git blame --minimal` 归于 P 的行为 684），`similar` 3.2.0 的有界 `Myers` 只找到 683 行公共行（本机现行 `libra blame g.txt` 归于 P 的行为 683），故能区分两种算法。

**Acceptance criteria:**
- [ ] **AC1** `libra blame --minimal file.txt` 的逐行作者序列为 `Commit_1 Commit_1 Commit_1 Commit_2 Commit_2 Commit_2 Commit_2 Commit_1 Commit_2 Commit_2 Commit_2`。
- [ ] **AC2** `libra blame file.txt --minimal`（选项在文件之后）的 stdout 与 AC1 调用的 stdout 逐字相同。
- [ ] **AC3** F07g 上 `libra blame --minimal g.txt` 归于作者 `P` 的行数为 684（最小编辑脚本）。
- [ ] **AC4** F07g 上 `libra blame g.txt`（默认）归于作者 `P` 的行数为 683（默认路径仍为有界 `Myers`，行为不变）。
- [ ] **AC5** F07g 上 `libra blame --minimal --no-minimal g.txt` 归于 `P` 的行数为 683（后者覆盖前者）。
- [ ] **AC6** F07g 上 `libra blame --no-minimal --minimal g.txt` 归于 `P` 的行数为 684。
- [ ] **AC7** `libra blame --minimal=1 file.txt` 以 `LBR-CLI-002` 失败。
- [ ] **AC8** F07g 上 `libra blame -w --minimal g.txt`（空白归一化分支）归于 `P` 的行数为 684。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--minimal`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::blame_test::blame_minimal_matches_git_attribution`：exit 0；按行抽取作者名得到的序列逐项等于上述 11 项（Git 2.54.0 预探，`t8015#6`）
- AC2 → `command::blame_test::blame_minimal_matches_git_attribution`：exit 0；两次 stdout 逐字相等
- AC3 → `command::blame_test::blame_minimal_uses_shortest_edit_script`：exit 0；按行抽取作者名，等于 `P` 的行数为 684
- AC4 → `command::blame_test::blame_minimal_uses_shortest_edit_script`：exit 0；按行抽取作者名，等于 `P` 的行数为 683
- AC5 → `command::blame_test::blame_minimal_uses_shortest_edit_script`：exit 0；按行抽取作者名，等于 `P` 的行数为 683
- AC6 → `command::blame_test::blame_minimal_uses_shortest_edit_script`：exit 0；按行抽取作者名，等于 `P` 的行数为 684
- AC7 → `command::blame_test::blame_minimal_matches_git_attribution`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC8 → `command::blame_test::blame_minimal_uses_shortest_edit_script`：exit 0；按行抽取作者名，等于 `P` 的行数为 684
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::blame_test::blame_minimal_matches_git_attribution)'` `(new)`：覆盖 AC1–AC2、AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::blame_test::blame_minimal_uses_shortest_edit_script)'` `(new)`：覆盖 AC3–AC6、AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::blame_test::/)'`（共享回归组：`command::blame_test` 模块族（含更新字段后的三处 `BlameArgs{…}` 字面量所在用例）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_help_flag_descriptions`（共享回归组：整 target，可见新 flag 须带描述的帮助守卫；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先 `source .env.test && cargo nextest list --test compat_help_flag_descriptions` 记录 N，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: `Cargo.toml` 非版本行（`similar` 版本要求）与版本面 `install.sh`/`install.ps1`（ER-13 T-1）
**Dependencies:** `PM-06`、`DEP-PM-11`、`DEP-PM-13`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/blame.rs`, `Cargo.toml`, `tests/command/blame_test.rs`, `docs/commands/blame.md`, `docs/commands/zh-CN/blame.md`, `docs/development/commands/blame.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/blame.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/blame.rs`, `Cargo.toml`, `tests/command/blame_test.rs`
**Docs and compatibility impact:** `docs/commands/blame.md` 与 zh-CN：`--minimal`/`--no-minimal`；`docs/development/commands/blame.md` 修订 `:66` 行映射算法说明；`COMPATIBILITY.md` `blame` 行（W-1，基线第 234 行）原地补记；网站页 `blame.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=blame --minimal最小差异行映射; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=2; scope=M; deps=PM-06,DEP-PM-11,DEP-PM-13; writeset=串行于 PM-06; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-08: `libra notes` 默认 notes ref 遵循 `core.notesRef`（#988；拆出 PM-34）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 把 `NotesArgs.ref_` 改为可选，未给出 `--ref` 时以 `core.notesRef` 作为默认 notes ref（复用 `log` 的级联读取 `read_cascaded_config_value_strict`：本地 → 全局 → 系统，变量名大小写不敏感），未配置时为 `refs/notes/commits`；配置值按原样使用（不做 `--ref` 的短名展开）：`notes get-ref` 原样打印，其余子命令遇到不以 `refs/notes/` 开头的值在任何读写前以 `LBR-CLI-002` 拒绝（Git：`refusing to … notes in <ref> (outside of refs/notes/)`）。显式 `--ref` 优先于配置，其规范化保持现状。（按 G-09 拆出 `PM-34` 承接 `GIT_NOTES_REF` 环境变量。）
**Out of scope:** `GIT_NOTES_REF` 环境变量（`PM-34`）；全局 `-c <key>=<value>` 选项（Libra 无此全局 flag，测试改用 `libra config` 写入；尚未排期，重启条件：全局 `-c` 立项）；`--ref=refs/heads/x` 展开为 `refs/notes/refs/heads/x` 的 Git 行为（TC-2062）与 notes 在 `for-each-ref` 中可见（Libra notes 存于 SQLite `notes` 表）（`DEFER-PM-08`）；`notes merge` 状态机（`REL-PM-01`）；配置值不以 `refs/notes/` 开头时 `add` 之外的其余读写子命令（`list`、`show`、`remove` 等）的拒绝（与 AC7 共用唯一选取点 `src/command/notes.rs:303` 之后的同一校验，不单独验收）；`core.notesRef` 变量名大小写不敏感（复用的 `read_cascaded_config_value_strict` 既有语义，由 `log` 的既有用例守护，不单独验收）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--ref` 为带默认值 `refs/notes/commits` 的字符串，无法区分「未给出」 | `src/command/notes.rs:37-39` |
| 唯一选取点 `normalize_notes_ref(&args.ref_)` | `src/command/notes.rs:303` |
| 默认常量、短名规范化与 `refs/notes/` 前缀校验 helper | `src/internal/notes.rs:13,19-37` |
| 非法 notes ref 映射为 `LBR-CLI-002` | `src/command/notes.rs:866-870` |
| `log` 读取 `core.notesRef` 的级联读取（本地 → 全局 → 系统，大小写不敏感） | `src/command/log/config.rs:51-58`；`src/internal/config.rs:1872-1934` |
| 测试子进程的全局与系统配置库均隔离在仓库目录内 | `tests/command/mod.rs:353-375` |
| Git：`--ref` > `core.notesRef`（本地 > 全局 > 系统）> 默认；配置值非 `refs/notes/` 时 `get-ref` 原样输出、`add`/`list` 等以 128 拒绝 | `git@6de20f6092:notes.c:1002-1013`、`builtin/notes.c:418-436,1108-1125`；Git 2.54.0 预探 |

**夹具:** F08：`create_committed_repo_via_cli` 仓库，提交 C；每条 AC 使用独立新建的仓库。本地配置以 `libra config core.notesRef <值>` 写入，全局与系统配置分别以 `libra config --global core.notesRef <值>` 与 `libra config --system core.notesRef <值>` 写入（`base_libra_command` 为每个子进程把 `LIBRA_CONFIG_GLOBAL_DB` 与 `LIBRA_CONFIG_SYSTEM_DB` 指向该仓库目录下的隔离库）；子进程不设 `GIT_NOTES_REF`。 `libra notes add -m <文本>` 的 blob 内容即该文本、无尾随换行：`src/command/notes.rs:612-653`、`src/internal/notes.rs:182-205`；下文 `blob(<文本>)` 均指内容为该字节的 blob 的对象 ID（`notes` 表的 `blob` 列与 `NOTES_MERGE_STATE` 记录的都是该 OID：`Blob::from_content` 后存入 `blob.id`，`src/internal/notes.rs:199-217`）。

**Acceptance criteria:**
- [ ] **AC1** 本地 `core.notesRef=refs/notes/custom` 时 `libra notes get-ref` 输出 `refs/notes/custom`。
- [ ] **AC2** 该配置下 `libra notes add -m x C` 前后「notes 快照」之差恰为新增 1 行 `(refs/notes/custom, <C>, <blob(x)>)`（其 `id` 不在调用前快照中）。
- [ ] **AC3** 只有全局 `core.notesRef=refs/notes/glob` 时 `libra notes get-ref` 输出 `refs/notes/glob`。
- [ ] **AC4** 本地 `refs/notes/custom` 与全局 `refs/notes/glob` 同时存在时 `libra notes get-ref` 输出 `refs/notes/custom`。
- [ ] **AC5** 只有系统级 `core.notesRef=refs/notes/sys` 时 `libra notes get-ref` 输出 `refs/notes/sys`。
- [ ] **AC6** 本地 `core.notesRef=refs/notes/custom` 时显式 `libra notes --ref opt get-ref` 输出 `refs/notes/opt`。
- [ ] **AC7** 本地 `core.notesRef=refs/heads/x` 时 `libra notes add -m y C` 以 `LBR-CLI-002` 失败。
- [ ] **AC8** 本地 `core.notesRef=refs/heads/x` 时 `libra notes get-ref` 原样输出 `refs/heads/x`。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`core.notesRef`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::notes_test::notes_default_ref_follows_config_scopes`：exit 0；stdout 逐字等于 `refs/notes/custom\n`
- AC2 → `command::notes_test::notes_default_ref_follows_config_scopes`：exit 0（前提）；调用后快照去掉该新增行后与调用前快照逐字相等，且新增行的三列取值如上
- AC3 → `command::notes_test::notes_default_ref_follows_config_scopes`：exit 0；stdout 逐字等于 `refs/notes/glob\n`
- AC4 → `command::notes_test::notes_default_ref_follows_config_scopes`：exit 0；stdout 逐字等于 `refs/notes/custom\n`
- AC5 → `command::notes_test::notes_default_ref_follows_config_scopes`：exit 0；stdout 逐字等于 `refs/notes/sys\n`（Git 2.54.0 预探）
- AC6 → `command::notes_test::notes_default_ref_follows_config_scopes`：exit 0；stdout 逐字等于 `refs/notes/opt\n`
- AC7 → `command::notes_test::notes_config_ref_outside_refs_notes`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC8 → `command::notes_test::notes_config_ref_outside_refs_notes`：exit 0；stdout 逐字等于 `refs/heads/x\n`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** 以稳定码失败的调用前后「notes 快照」逐字相等。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_default_ref_follows_config_scopes)'` `(new)`：覆盖 AC1–AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_config_ref_outside_refs_notes)'` `(new)`：覆盖 AC7–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（共享回归组：`command::notes_test` 模块族（含 `get_ref_prints_active_notes_ref` 与 `error_*_invalid_ref` 系列）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-07`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/notes.rs`, `tests/command/notes_test.rs`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/notes.rs`, `tests/command/notes_test.rs`
**Docs and compatibility impact:** `docs/commands/notes.md` 与 zh-CN：未给出 `--ref` 时的 `core.notesRef` 级联（本地 → 全局 → 系统）、原样使用与非 `refs/notes/` 拒绝；`docs/development/commands/notes.md` 同步；`COMPATIBILITY.md` `notes` 行（W-1，基线第 220 行）原地补记；网站页 `notes.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=notes默认ref的core.notesRef配置解析; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=PM-07,DEP-PM-11; writeset=串行于 PM-07; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-34: `libra notes` 默认 notes ref 遵循 `GIT_NOTES_REF` 环境变量（#988；自 PM-08 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-08` 拆出：未给出 `--ref` 时，已设置的 `GIT_NOTES_REF` 环境变量优先于 `core.notesRef` 成为默认 notes ref（Git 优先级 `--ref` > `GIT_NOTES_REF` > `core.notesRef` > `refs/notes/commits`）；取值按原样使用：`notes get-ref` 原样打印，其余子命令遇到不以 `refs/notes/` 开头的值在任何读写前以 `LBR-CLI-002` 拒绝（与 `PM-08` 对配置值的处理一致）。
**Out of scope:** `core.notesRef` 的级联读取（`PM-08`）；`GIT_NOTES_REF` 为空字符串时的行为（不单独验收；尚未排期，重启条件：后续 notes 计划）；`--ref` 展开与 notes 在 `for-each-ref` 中可见（`DEFER-PM-08`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `PM-08` 之后默认 notes ref 只来自 `--ref` 与 `core.notesRef`；选取点 `normalize_notes_ref(&args.ref_)` | `src/command/notes.rs:303` |
| `refs/notes/` 前缀校验 helper 与 `LBR-CLI-002` 映射 | `src/internal/notes.rs:30-37`；`src/command/notes.rs:866-870` |
| 测试 helper 逐子进程传入环境变量 | `tests/command/mod.rs:390-395`（`run_libra_command_with_env`） |
| Git：`GIT_NOTES_REF` 位于 `--ref` 与 `core.notesRef` 之间；取值非 `refs/notes/` 时 `get-ref` 原样输出、`add`/`list` 以 128 拒绝 | `git@6de20f6092:notes.c:1002-1013`、`builtin/notes.c:418-436,1108-1125`；Git 2.54.0 预探 |

**夹具:** F34：`create_committed_repo_via_cli` 仓库，提交 C；每条 AC 使用独立新建的仓库；本地配置以 `libra config core.notesRef refs/notes/custom` 写入（AC1、AC3）；`GIT_NOTES_REF` 经 `run_libra_command_with_env` 逐子进程传入。 `libra notes add -m <文本>` 的 blob 内容即该文本、无尾随换行：`src/command/notes.rs:612-653`、`src/internal/notes.rs:182-205`；下文 `blob(<文本>)` 均指内容为该字节的 blob 的对象 ID（`notes` 表的 `blob` 列与 `NOTES_MERGE_STATE` 记录的都是该 OID：`Blob::from_content` 后存入 `blob.id`，`src/internal/notes.rs:199-217`）。

**Acceptance criteria:**
- [ ] **AC1** `GIT_NOTES_REF=refs/notes/env` 且本地 `core.notesRef=refs/notes/custom` 时 `libra notes get-ref` 输出 `refs/notes/env`。
- [ ] **AC2** `GIT_NOTES_REF=refs/notes/env` 时 `libra notes add -m z C` 前后「notes 快照」之差恰为新增 1 行 `(refs/notes/env, <C>, <blob(z)>)`（其 `id` 不在调用前快照中）。
- [ ] **AC3** `GIT_NOTES_REF=refs/notes/env` 且本地 `core.notesRef=refs/notes/custom` 时显式 `libra notes --ref opt get-ref` 输出 `refs/notes/opt`。
- [ ] **AC4** `GIT_NOTES_REF=refs/heads/y` 时 `libra notes add -m z C` 以 `LBR-CLI-002` 失败。
- [ ] **AC5** `GIT_NOTES_REF=refs/heads/y` 时 `libra notes list` 以 `LBR-CLI-002` 失败。
- [ ] **AC6** `GIT_NOTES_REF=refs/heads/y` 时 `libra notes get-ref` 原样输出 `refs/heads/y`。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`GIT_NOTES_REF`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::notes_test::notes_default_ref_follows_env`：exit 0；stdout 逐字等于 `refs/notes/env\n`（Git 2.54.0 预探）
- AC2 → `command::notes_test::notes_default_ref_follows_env`：exit 0（前提）；调用后快照去掉该新增行后与调用前快照逐字相等，且新增行的三列取值如上
- AC3 → `command::notes_test::notes_default_ref_follows_env`：exit 0；stdout 逐字等于 `refs/notes/opt\n`（Git 2.54.0 预探）
- AC4 → `command::notes_test::notes_env_ref_outside_refs_notes`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC5 → `command::notes_test::notes_env_ref_outside_refs_notes`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC6 → `command::notes_test::notes_env_ref_outside_refs_notes`：exit 0；stdout 逐字等于 `refs/heads/y\n`（Git 2.54.0 预探）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** 以稳定码失败的调用前后「notes 快照」逐字相等。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_default_ref_follows_env)'` `(new)`：覆盖 AC1–AC3（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_env_ref_outside_refs_notes)'` `(new)`：覆盖 AC4–AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（共享回归组：`command::notes_test` 模块族（含 `PM-08` 的两个用例与 `get_ref_prints_active_notes_ref`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-08`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/notes.rs`, `tests/command/notes_test.rs`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/notes.rs`, `tests/command/notes_test.rs`
**Docs and compatibility impact:** `docs/commands/notes.md` 与 zh-CN：`GIT_NOTES_REF` 的优先级、原样使用与非 `refs/notes/` 拒绝；`docs/development/commands/notes.md` 同步；`COMPATIBILITY.md` `notes` 行（W-1，基线第 220 行）原地补记；网站页 `notes.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=notes默认ref的GIT_NOTES_REF环境变量; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=PM-08,DEP-PM-11; writeset=串行于 PM-08; release=independent; split-from=PM-08; exception=N/A`

---

### Task PM-09: `notes merge` manual 冲突物化 `NOTES_MERGE_*` 状态并拒绝未完结时的新合并（#989；拆出 PM-29、PM-23）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 ADR-PM-02：`-s manual`（默认）合并出现冲突时不再整体中止，而是不改 `notes` 表，在当前工作区 gitdir 下以排他 `create_dir` 声明 `NOTES_MERGE_WORKTREE/`、写入每个冲突对象的 `<oid>` 文件（Git 冲突标记格式），最后以原子替换写入 `NOTES_MERGE_STATE`（目标 ref、来源 ref、待复制条目与冲突条目的前值/来源值），仍以现有的 `LBR-CONFLICT-002` 失败；`NOTES_MERGE_WORKTREE` 已存在（无论有无 STATE）时新的 `notes merge <ref>` 在任何写入前以 `LBR-REPO-003` 拒绝。（按 G-09 拆出 `PM-29` 承接 `--commit`（其再拆出 `PM-38` 承接崩溃后重试）、`PM-23` 承接 `--abort`/`--quit`（其再拆出 `PM-39` 承接控制形式与选项互斥）。）本卡实现的命令形式遵守 ADR-PM-02 的锁规则：所有 `notes merge` 形式在读取或修改合并状态前以排他创建 `NOTES_MERGE_LOCK`（当前工作区 gitdir）取得互斥，结束时（成功或失败）删除；锁已存在即以 `LBR-CONFLICT-002` 失败且不读写任何状态（陈旧锁由用户确认无进程后手工删除，提示给出路径）。存在任何合并残留（`NOTES_MERGE_STATE`、`NOTES_MERGE_WORKTREE/` 或 `PM-29` 的提交开始标记 `NOTES_MERGE_COMMITTING`）时新合并以 `LBR-REPO-003` 拒绝。冲突文件中某侧内容不以换行结尾时补一个 `\n`。
**Out of scope:** `--commit`（`PM-29`）；`--abort`/`--quit`（`PM-23`）；控制形式与选项互斥（`PM-39`）；非 manual 策略与无冲突合并（行为不变，由 `notes_test` 回归组守护）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| manual 策略遇冲突整体中止（`MergeConflict`），无手工解决状态 | `src/internal/notes.rs:485-526` |
| 其余策略在单写事务内以「仍为预期值」CAS 应用 | `src/internal/notes.rs:553-602` |
| `MergeConflict` 映射为 `LBR-CONFLICT-002` | `src/command/notes.rs:893-897` |
| 既有用例在 manual 冲突后继续执行其它策略的合并（新行为下会被未完结状态拒绝），本卡同卡在这两处之后删除新增的合并状态再继续，其余断言（含 `LBR-CONFLICT-002`、目标 notes 不变、非冲突条目未写入）保留 | `tests/command/notes_test.rs:1382-1460`（`test_notes_merge_strategies_copy_and_manual_conflict`）；`:1521-1563`（`test_notes_merge_ours_cat_sort_uniq_and_manual_code`） |
| 每工作区 gitdir 与原子写 helper | `src/utils/util.rs:1106,1127`；`src/utils/atomic_write.rs:71` |
| Git：冲突时目标 ref 不变、写 `NOTES_MERGE_WORKTREE/<oid>`（冲突标记）、未完结时新合并报 `You have not concluded your previous notes merge`（128） | `git@6de20f6092:builtin/notes.c:960-1010`、`notes-merge.c:280-300`；Git 2.54.0 预探 |

**夹具:** F09：`create_committed_repo_via_cli` 仓库，提交 C1 与其子提交 C2；`libra notes add -m 'x note' C1`（`refs/notes/commits`）；`libra notes --ref other add -m 'y note' C1` 与 `libra notes --ref other add -m 'only other' C2`（`libra notes add -m <文本>` 的 blob 内容即该文本、无尾随换行：`src/command/notes.rs:612-653`、`src/internal/notes.rs:182-205`；下文 `blob(<文本>)` 均指内容为该字节的 blob 的对象 ID（`notes` 表的 `blob` 列与 `NOTES_MERGE_STATE` 记录的都是该 OID：`Blob::from_content` 后存入 `blob.id`，`src/internal/notes.rs:199-217`））。冲突文件期望字节 W1 = `<<<<<<< refs/notes/commits\nx note\n=======\ny note\n>>>>>>> refs/notes/other\n`（Git 2.54.0 预探的标记与次序；冲突文件格式为 `<<<<<<< <目标 ref>\n`、目标侧内容、`=======\n`、来源侧内容、`>>>>>>> <来源 ref>\n`，某侧内容不以 `\n` 结尾时在其后补一个 `\n`——Libra 的 notes blob 无尾随换行，补齐后与 Git 在带换行 blob 上的字节一致）。合并状态位于当前工作区 gitdir（`util::request_worktree_gitdir()`，主工作区即 `.libra/`）。

**Acceptance criteria:**
- [ ] **AC1** `libra notes merge refs/notes/other` 以 `LBR-CONFLICT-002` 失败（与现有 manual 冲突的稳定码相同）。
- [ ] **AC2** 该调用后 `NOTES_MERGE_WORKTREE/<C1>` 的字节等于 W1。
- [ ] **AC3** 该调用前后「notes 快照」逐字相等（目标 ref 未被修改，非冲突的 C2 条目也未写入）。
- [ ] **AC4** 该调用后 `NOTES_MERGE_STATE` 按其版本化格式解析得到的完整内容等于：目标 ref `refs/notes/commits`、来源 ref `refs/notes/other`、冲突条目恰为 `[(C1, blob(x note), blob(y note))]`、待复制条目恰为 `[(C2, blob(only other))]`。
- [ ] **AC5** 状态存在时再次 `libra notes merge refs/notes/other` 以 `LBR-REPO-003` 失败。
- [ ] **AC6** 只残留 `NOTES_MERGE_WORKTREE/`（删除 `NOTES_MERGE_STATE` 模拟创建中途崩溃）时新的 `libra notes merge refs/notes/other` 以 `LBR-REPO-003` 失败。
- [ ] **AC7** 工作区 gitdir 下已有 `NOTES_MERGE_LOCK` 时 `libra notes merge refs/notes/other` 以 `LBR-CONFLICT-002` 失败。
- [ ] **AC8** 只残留 `NOTES_MERGE_COMMITTING`（无 `NOTES_MERGE_STATE` 与 `NOTES_MERGE_WORKTREE/`）时新的 `libra notes merge refs/notes/other` 以 `LBR-REPO-003` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`NOTES_MERGE_WORKTREE`；家族子卡同卡执行第 1–5 步与第 6 步的后端本地签名提交（不推送），推送与第 7 步部署核验由 `PM-24` 执行（第 8 项）。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::notes_test::notes_merge_manual_conflict_materializes_state`：退出码非 0（前提）；stderr 含 `LBR-CONFLICT-002`
- AC2 → `command::notes_test::notes_merge_manual_conflict_materializes_state`：文件存在且字节逐字等于 W1
- AC3 → `command::notes_test::notes_merge_manual_conflict_materializes_state`：前后「notes 快照」逐字相等
- AC4 → `command::notes_test::notes_merge_manual_conflict_materializes_state`：读取并解析该文件，所得结构与上述期望逐项相等
- AC5 → `command::notes_test::notes_merge_refuses_while_state_or_lock_exists`：退出码非 0（前提）；stderr 含 `LBR-REPO-003`
- AC6 → `command::notes_test::notes_merge_refuses_while_state_or_lock_exists`：退出码非 0（前提）；stderr 含 `LBR-REPO-003`
- AC7 → `command::notes_test::notes_merge_refuses_while_state_or_lock_exists`：退出码非 0（前提）；stderr 含 `LBR-CONFLICT-002`
- AC8 → `command::notes_test::notes_merge_refuses_while_state_or_lock_exists`：退出码非 0（前提）；stderr 含 `LBR-REPO-003`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 5 步及第 6 步的后端本地签名提交（推送与第 7 步由 `PM-24` 执行，第 8 项）

**失败路径不变量:** 以稳定码失败的调用（AC1 除外，其写入即 AC2/AC4 所述状态文件）前后「notes 快照」与「notes 合并状态快照」逐字相等；AC7 的调用之后 `NOTES_MERGE_LOCK` 仍为夹具写入的原文件（命令不得删除他人持有的锁）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_merge_manual_conflict_materializes_state)'` `(new)`：覆盖 AC1–AC4（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_merge_refuses_while_state_or_lock_exists)'` `(new)`：覆盖 AC5–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（共享回归组：`command::notes_test` 模块族（含非 manual 策略与无冲突合并的既有用例）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** N/A（`family child of REL-PM-01`，不 bump、不发布；全量与版本面由 `PM-24` 的 T-2 统一执行；本卡只跑 focused 与 fmt/clippy）
**Dependencies:** `PM-34`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/notes.rs`, `src/internal/notes.rs`, `tests/command/notes_test.rs`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`
**Release write set:** `N/A`
**Files likely touched:** `src/command/notes.rs`, `src/internal/notes.rs`, `tests/command/notes_test.rs`
**Docs and compatibility impact:** `docs/commands/notes.md` 与 zh-CN：manual 冲突产生 `NOTES_MERGE_*` 状态、冲突文件格式与未完结拒绝；`docs/development/commands/notes.md`：ADR-PM-02 状态格式；`COMPATIBILITY.md` `notes` 行（W-1，基线第 220 行）原地改写 manual 策略语义；网站页 `notes.en.md` 由本卡同卡写入、评审并在后端 `cf` 本地签名提交（不推送；推送与部署核验由 `PM-24` 执行，见「文档与网站验收命令」第 8 项与 `DEP-PM-14`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-PM-01`
**C/D coverage from:** `PM-24`
**Granularity:** `type=implementation; axis=notes merge manual冲突状态物化与未完结拒绝; recovery=发布前按 REL-PM-01 逆序本地 revert；发布后随 PM-24 前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=PM-34,DEP-PM-11; writeset=串行于 PM-34; release=family child; split-from=N/A; exception=N/A`

---

### Task PM-29: 实现 `notes merge --commit`：首次提交的 CAS 与状态清理（#989；自 PM-09 拆出；拆出 PM-38）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-09` 拆出：`libra notes merge --commit` 读取 `NOTES_MERGE_STATE`（缺失以 `LBR-REPO-003`、不可解析以 `LBR-REPO-002` 失败），对每个冲突对象取 `NOTES_MERGE_WORKTREE/<oid>` 的字节作为解决结果（文件被删除则保留目标侧原值）；首次提交（不存在提交开始标记）时先以排他创建空文件 `NOTES_MERGE_COMMITTING`，再在单个写事务内先核对全部行的「当前值 ∈ {前值, 目标值}」，全部通过后才写入（处于前值的行写为目标值、已是目标值的行跳过）；任一行为其它值时不写入任何行、回滚事务、删除该标记并以 `LBR-CONFLICT-002` 失败且保留状态（写入阶段出现的数据库错误由同一事务回滚，属 SQLite 事务既有语义，不单独验收）；事务提交后依次删除 `NOTES_MERGE_STATE`、`NOTES_MERGE_WORKTREE/` 与 `NOTES_MERGE_COMMITTING`。已存在标记时的重试语义由 `PM-38` 承接。本卡实现的命令形式遵守 ADR-PM-02 的锁规则：所有 `notes merge` 形式在读取或修改合并状态前以排他创建 `NOTES_MERGE_LOCK`（当前工作区 gitdir）取得互斥，结束时（成功或失败）删除；锁已存在即以 `LBR-CONFLICT-002` 失败且不读写任何状态（陈旧锁由用户确认无进程后手工删除，提示给出路径）。（按 G-09 拆出 `PM-38`。）
**Out of scope:** 已存在 `NOTES_MERGE_COMMITTING` 时的重试与崩溃恢复（`PM-38`）；冲突物化（`PM-09`）；`--abort`/`--quit`（`PM-23`）；控制形式与选项互斥（`PM-39`）；Git notes 提交对象与 `NOTES_MERGE_PARTIAL` 提交（Libra notes 为扁平行，无 notes 提交历史，ADR-PM-02）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 既有合并的写事务 CAS 模式（`ON CONFLICT DO NOTHING` 与 `AND blob = ?`） | `src/internal/notes.rs:553-602` |
| `MergeRaced` 映射为 `LBR-CONFLICT-002` | `src/command/notes.rs:902-905` |
| Git：`--commit` 把 `NOTES_MERGE_WORKTREE` 中每个 `<oid>` 文件作为解决后的 note，连同自动合并部分一次提交；无合并时 `failed to read ref NOTES_MERGE_PARTIAL`（128） | `git@6de20f6092:notes-merge.c:682-740`、`builtin/notes.c:817-870`；Git 2.54.0 预探 |

**夹具:** F09：`create_committed_repo_via_cli` 仓库，提交 C1 与其子提交 C2；`libra notes add -m 'x note' C1`（`refs/notes/commits`）；`libra notes --ref other add -m 'y note' C1` 与 `libra notes --ref other add -m 'only other' C2`（`libra notes add -m <文本>` 的 blob 内容即该文本、无尾随换行：`src/command/notes.rs:612-653`、`src/internal/notes.rs:182-205`；下文 `blob(<文本>)` 均指内容为该字节的 blob 的对象 ID（`notes` 表的 `blob` 列与 `NOTES_MERGE_STATE` 记录的都是该 OID：`Blob::from_content` 后存入 `blob.id`，`src/internal/notes.rs:199-217`））。冲突文件期望字节 W1 = `<<<<<<< refs/notes/commits\nx note\n=======\ny note\n>>>>>>> refs/notes/other\n`（Git 2.54.0 预探的标记与次序；冲突文件格式为 `<<<<<<< <目标 ref>\n`、目标侧内容、`=======\n`、来源侧内容、`>>>>>>> <来源 ref>\n`，某侧内容不以 `\n` 结尾时在其后补一个 `\n`——Libra 的 notes blob 无尾随换行，补齐后与 Git 在带换行 blob 上的字节一致）。合并状态位于当前工作区 gitdir（`util::request_worktree_gitdir()`，主工作区即 `.libra/`）。 除 AC5 外，每个用例先执行 `libra notes merge refs/notes/other` 得到 `PM-09` 的冲突状态，并记录此时「notes 快照」S0；AC5 使用另一个未执行合并的 F09 仓库。 「写入探针」（AC4）：以测试侧数据库连接（写法同 `tests/command/agent_clean_test.rs:34-50` 与 `tests/command/agent_erasure_test.rs:17-23`）在 `.libra/libra.db` 安装 `BEFORE INSERT`、`BEFORE UPDATE`、`BEFORE DELETE ON notes` 三个 `SELECT RAISE(ABORT, 'pm29-no-write')` 触发器，调用结束后删除；探针在位时任何对 `notes` 表的写入都会使命令以非 `LBR-CONFLICT-002` 的数据库错误失败。

**Acceptance criteria:**
- [ ] **AC1** 把 `NOTES_MERGE_WORKTREE/<C1>` 改为 `resolved\n` 后执行 `libra notes merge --commit`，其后「notes 快照」等于 S0 将 `(refs/notes/commits, C1)` 行（`id` 不变）的 blob 换为 `blob(resolved\n)`，并新增一行 `(refs/notes/commits, C2, blob(only other))`（其 `id` 不在 S0 中）。
- [ ] **AC2** 成功 `--commit` 后「notes 合并状态快照」等于 `(absent, absent, absent)`。
- [ ] **AC3** 删除 `NOTES_MERGE_WORKTREE/<C1>` 后执行 `--commit`，其后「notes 快照」等于 S0 新增一行 `(refs/notes/commits, C2, blob(only other))`（其 `id` 不在 S0 中；C1 行保持 `x note`）。
- [ ] **AC4** 状态存在期间执行 `libra notes add -f -m changed C1`，随后安装「写入探针」，再执行 `--commit` 以 `LBR-CONFLICT-002` 失败（核对先于任何写入：先写 C2 再核对 C1 的实现会被探针拦截而以其它错误失败）。
- [ ] **AC5** 无合并状态时 `libra notes merge --commit` 以 `LBR-REPO-003` 失败。
- [ ] **AC6** `NOTES_MERGE_STATE` 被改写为非法内容时 `--commit` 以 `LBR-REPO-002` 失败。
- [ ] **AC7** 合并状态存在且已有 `NOTES_MERGE_LOCK` 时 `libra notes merge --commit` 以 `LBR-CONFLICT-002` 失败。
- [ ] **AC8** 状态存在期间先执行 `libra notes add -m 'only other' C2`（C2 行当前值即目标值）并记录其后的「notes 快照」S8，再把 `NOTES_MERGE_WORKTREE/<C1>` 改为 `resolved\n` 后执行 `--commit`，其后「notes 快照」等于 S8 将 `(refs/notes/commits, C1)` 行（`id` 不变）的 blob 换为 `blob(resolved\n)`（已是目标值的 C2 行被跳过而非判为冲突）。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--commit`；家族子卡同卡执行第 1–5 步与第 6 步的后端本地签名提交（不推送），推送与第 7 步部署核验由 `PM-24` 执行（第 8 项）。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::notes_test::notes_merge_commit_applies_resolution`：exit 0（前提）；快照去掉新增行后与「S0 中 C1 行 blob 替换后的快照」逐字相等，且新增行三列取值如上
- AC2 → `command::notes_test::notes_merge_commit_applies_resolution`：「notes 合并状态快照」逐字等于 `(absent, absent, absent)`
- AC3 → `command::notes_test::notes_merge_commit_applies_resolution`：exit 0（前提）；快照去掉新增行后与 S0 逐字相等，且新增行三列取值如上
- AC4 → `command::notes_test::notes_merge_commit_cas_and_errors`：写入探针在位（前提）；退出码非 0（前提）；stderr 含 `LBR-CONFLICT-002`
- AC5 → `command::notes_test::notes_merge_commit_cas_and_errors`：退出码非 0（前提）；stderr 含 `LBR-REPO-003`
- AC6 → `command::notes_test::notes_merge_commit_cas_and_errors`：退出码非 0（前提）；stderr 含 `LBR-REPO-002`
- AC7 → `command::notes_test::notes_merge_commit_cas_and_errors`：退出码非 0（前提）；stderr 含 `LBR-CONFLICT-002`
- AC8 → `command::notes_test::notes_merge_commit_applies_resolution`：exit 0（前提）；快照逐字等于「S8 中 C1 行 blob 替换后的快照」
- 文档 AC → 「文档与网站验收命令」第 1 步至第 5 步及第 6 步的后端本地签名提交（推送与第 7 步由 `PM-24` 执行，第 8 项）

**失败路径不变量:** 以稳定码失败的调用前后「notes 快照」与「notes 合并状态快照」逐字相等（AC4 的前值以并发 `notes add` 之后的快照为准；AC4 回滚时删除本次创建的提交开始标记）；AC7 的调用之后 `NOTES_MERGE_LOCK` 仍为夹具写入的原文件。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_merge_commit_applies_resolution)'` `(new)`：覆盖 AC1–AC3、AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_merge_commit_cas_and_errors)'` `(new)`：覆盖 AC4–AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（共享回归组：`command::notes_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** N/A（`family child of REL-PM-01`，全量与版本面由 `PM-24` 的 T-2 统一执行；本卡只跑 focused 与 fmt/clippy）
**Dependencies:** `PM-09`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/notes.rs`, `src/internal/notes.rs`, `tests/command/notes_test.rs`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`
**Release write set:** `N/A`
**Files likely touched:** `src/command/notes.rs`, `src/internal/notes.rs`, `tests/command/notes_test.rs`
**Docs and compatibility impact:** `docs/commands/notes.md` 与 zh-CN：`--commit` 的解决方式（文件内容即结果、删除文件保留原值）、提交开始标记与并发修改时拒绝；`docs/development/commands/notes.md`：ADR-PM-02 提交与崩溃窗口；`COMPATIBILITY.md` `notes` 行（W-1，基线第 220 行）原地补记；网站页 `notes.en.md` 由本卡同卡写入、评审并在后端 `cf` 本地签名提交（不推送；推送与部署核验由 `PM-24` 执行，见「文档与网站验收命令」第 8 项与 `DEP-PM-14`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-PM-01`
**C/D coverage from:** `PM-24`
**Granularity:** `type=implementation; axis=notes merge --commit首次提交CAS与状态清理; recovery=发布前按 REL-PM-01 逆序本地 revert；发布后随 PM-24 前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=PM-09,DEP-PM-11; writeset=串行于 PM-09; release=family child; split-from=PM-09; exception=N/A`

---

### Task PM-38: `notes merge --commit` 的崩溃后重试：提交开始标记下只确认、不重放（#989；自 PM-29 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-29` 拆出：`libra notes merge --commit` 先按 `PM-29` 读取 `NOTES_MERGE_STATE`（缺失以 `LBR-REPO-003`、不可解析以 `LBR-REPO-002` 失败，与标记是否存在无关；此时残留按 ADR-PM-02 由 `--abort` 清理）；`STATE` 可解析且 `NOTES_MERGE_COMMITTING` 已存在时（上次提交在事务前后崩溃，无法区分「未提交」与「已提交后又被他人改回」），不再写入任何行，只核对：全部冲突条目与待复制条目的当前值都已等于目标值时按 `PM-29` 的次序清理三个状态路径并成功（包括「事务前崩溃」而各行目标值恰等于当前值的情形，例如只有冲突条目且解决文件被删除）；否则以 `LBR-CONFLICT-002` 失败且保留全部状态，用户以 `--abort` 放弃后重新合并（fail-closed，杜绝覆盖崩溃后他人的写入）。
**Out of scope:** 首次提交的 CAS 与清理（`PM-29`）；标记存在而 `NOTES_MERGE_STATE` 不可解析时以 `LBR-REPO-002` 失败（`STATE` 的读取与解析先于标记检查，与 `PM-29` AC6 为同一解析调用，不单独验收）；`--abort`/`--quit` 对标记的清理（`PM-23`）；自动区分「未提交」与「已提交后被改回」（需要与 notes 写入同事务的持久回执，属后续存储模型改造，尚未排期）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `PM-29` 的首次提交先建标记、事务内 CAS、再按序清理；既有合并的写事务 CAS 模式 | `src/internal/notes.rs:553-602` |
| `MergeRaced` 映射为 `LBR-CONFLICT-002` | `src/command/notes.rs:902-905` |
| `notes add -f` 以 upsert 写入、不受合并锁保护（崩溃后他人改写的来源） | `src/internal/notes.rs:180-240` |
| Git：`--commit` 以「旧值」检查更新 notes ref，ref 已被他人移动时失败而非覆盖 | `git@6de20f6092:builtin/notes.c:817-870`、`notes-merge.c:682-740` |
| 测试以 `RAISE(ABORT)` 触发器探测/拒绝写入的既有写法 | `tests/command/agent_clean_test.rs:34-50`；`tests/command/agent_erasure_test.rs:17-23` |

**夹具:** F09：`create_committed_repo_via_cli` 仓库，提交 C1 与其子提交 C2；`libra notes add -m 'x note' C1`（`refs/notes/commits`）；`libra notes --ref other add -m 'y note' C1` 与 `libra notes --ref other add -m 'only other' C2`（`libra notes add -m <文本>` 的 blob 内容即该文本、无尾随换行：`src/command/notes.rs:612-653`、`src/internal/notes.rs:182-205`；下文 `blob(<文本>)` 均指内容为该字节的 blob 的对象 ID（`notes` 表的 `blob` 列与 `NOTES_MERGE_STATE` 记录的都是该 OID：`Blob::from_content` 后存入 `blob.id`，`src/internal/notes.rs:199-217`））。冲突文件期望字节 W1 = `<<<<<<< refs/notes/commits\nx note\n=======\ny note\n>>>>>>> refs/notes/other\n`（Git 2.54.0 预探的标记与次序；冲突文件格式为 `<<<<<<< <目标 ref>\n`、目标侧内容、`=======\n`、来源侧内容、`>>>>>>> <来源 ref>\n`，某侧内容不以 `\n` 结尾时在其后补一个 `\n`——Libra 的 notes blob 无尾随换行，补齐后与 Git 在带换行 blob 上的字节一致）。合并状态位于当前工作区 gitdir（`util::request_worktree_gitdir()`，主工作区即 `.libra/`）。 AC1–AC5 的用例先执行 `libra notes merge refs/notes/other` 并把 `NOTES_MERGE_WORKTREE/<C1>` 改为 `resolved\n`。「提交后崩溃」模拟：复制保存 `NOTES_MERGE_STATE` 与 `NOTES_MERGE_WORKTREE/`，执行一次成功的 `--commit` 并记录其后的「notes 快照」S1，再把两个状态路径原样放回并创建空文件 `NOTES_MERGE_COMMITTING`。「事务前崩溃」模拟：不执行 `--commit`，直接创建空文件 `NOTES_MERGE_COMMITTING`，并记录此时的「notes 快照」S3。 F38b（AC6）：另一 F09 同构仓库但不写 `refs/notes/other` 上 C2 的 note（只有 C1 冲突、无待复制条目）；执行合并后删除 `NOTES_MERGE_WORKTREE/<C1>`（目标值即前值 `x note`），不执行 `--commit`，直接创建空文件 `NOTES_MERGE_COMMITTING`，并记录此时的「notes 快照」S3。 「写入探针」：重试前以测试侧数据库连接（写法同 `tests/command/agent_clean_test.rs:34-50` 与 `tests/command/agent_erasure_test.rs:17-23`）在 `.libra/libra.db` 安装 `BEFORE INSERT`、`BEFORE UPDATE`、`BEFORE DELETE ON notes` 三个 `SELECT RAISE(ABORT, 'pm38-no-write')` 触发器，调用结束后删除；探针在位时任何对 `notes` 表的写入都会使命令失败。

**Acceptance criteria:**
- [ ] **AC1** 「提交后崩溃」模拟并安装写入探针后再次执行 `--commit`，其后「notes 快照」等于 S1（未写入任何行）。
- [ ] **AC2** 「提交后崩溃」模拟后再次执行 `--commit`，其后「notes 合并状态快照」等于 `(absent, absent, absent)`。
- [ ] **AC3** 「提交后崩溃」模拟后执行 `libra notes add -f -m 'x note' C1`（改回原值），再执行 `--commit` 以 `LBR-CONFLICT-002` 失败。
- [ ] **AC4** 「提交后崩溃」模拟后执行 `libra notes remove C2`（删除已复制条目），再执行 `--commit` 以 `LBR-CONFLICT-002` 失败。
- [ ] **AC5** 「事务前崩溃」模拟（F09，至少一行目标值不等于当前值）后执行 `--commit` 以 `LBR-CONFLICT-002` 失败（fail-closed）。
- [ ] **AC6** F38b 上安装写入探针后执行 `--commit`，其后「notes 合并状态快照」等于 `(absent, absent, absent)`（全部行当前值已等于目标值，只清理并成功）。
- [ ] **AC7** F38b 上该（探针在位的）`--commit` 之后「notes 快照」等于 S3（只清理、不写任何行）。
- [ ] **AC8** 只残留 `NOTES_MERGE_WORKTREE/` 与 `NOTES_MERGE_COMMITTING`（删除 `NOTES_MERGE_STATE`，模拟删 STATE 后崩溃）时 `libra notes merge --commit` 以 `LBR-REPO-003` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`NOTES_MERGE_COMMITTING`；家族子卡同卡执行第 1–5 步与第 6 步的后端本地签名提交（不推送），推送与第 7 步部署核验由 `PM-24` 执行（第 8 项）。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::notes_test::notes_merge_commit_retry_after_crash`：写入探针在位、exit 0（前提）；「notes 快照」逐字等于 S1
- AC2 → `command::notes_test::notes_merge_commit_retry_after_crash`：exit 0（前提）；「notes 合并状态快照」逐字等于 `(absent, absent, absent)`
- AC3 → `command::notes_test::notes_merge_commit_retry_refuses_unconfirmed_rows`：退出码非 0（前提）；stderr 含 `LBR-CONFLICT-002`
- AC4 → `command::notes_test::notes_merge_commit_retry_refuses_unconfirmed_rows`：退出码非 0（前提）；stderr 含 `LBR-CONFLICT-002`
- AC5 → `command::notes_test::notes_merge_commit_retry_refuses_unconfirmed_rows`：退出码非 0（前提）；stderr 含 `LBR-CONFLICT-002`
- AC6 → `command::notes_test::notes_merge_commit_retry_after_crash`：写入探针在位、exit 0（前提）；「notes 合并状态快照」逐字等于 `(absent, absent, absent)`
- AC7 → `command::notes_test::notes_merge_commit_retry_after_crash`：写入探针在位、exit 0（前提）；「notes 快照」逐字等于 S3
- AC8 → `command::notes_test::notes_merge_commit_retry_refuses_unconfirmed_rows`：退出码非 0（前提）；stderr 含 `LBR-REPO-003`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 5 步及第 6 步的后端本地签名提交（推送与第 7 步由 `PM-24` 执行，第 8 项）

**失败路径不变量:** 以稳定码失败的调用前后「notes 快照」与「notes 合并状态快照」逐字相等（AC3、AC4 的前值以改写之后的快照为准）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_merge_commit_retry_after_crash)'` `(new)`：覆盖 AC1–AC2、AC6–AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_merge_commit_retry_refuses_unconfirmed_rows)'` `(new)`：覆盖 AC3–AC5、AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（共享回归组：`command::notes_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** N/A（`family child of REL-PM-01`，全量与版本面由 `PM-24` 的 T-2 统一执行；本卡只跑 focused 与 fmt/clippy）
**Dependencies:** `PM-29`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/notes.rs`, `src/internal/notes.rs`, `tests/command/notes_test.rs`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`
**Release write set:** `N/A`
**Files likely touched:** `src/command/notes.rs`, `src/internal/notes.rs`, `tests/command/notes_test.rs`
**Docs and compatibility impact:** `docs/commands/notes.md` 与 zh-CN：崩溃后重试 `--commit` 只确认不重放、无法确认时拒绝并提示 `--abort`；`docs/development/commands/notes.md`：ADR-PM-02 的提交开始标记与崩溃窗口；`COMPATIBILITY.md` `notes` 行（W-1，基线第 220 行）原地补记；网站页 `notes.en.md` 由本卡同卡写入、评审并在后端 `cf` 本地签名提交（不推送；推送与部署核验由 `PM-24` 执行，见「文档与网站验收命令」第 8 项与 `DEP-PM-14`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-PM-01`
**C/D coverage from:** `PM-24`
**Granularity:** `type=implementation; axis=notes merge --commit崩溃后重试确认; recovery=发布前按 REL-PM-01 逆序本地 revert；发布后随 PM-24 前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=PM-29,DEP-PM-11; writeset=串行于 PM-29; release=family child; split-from=PM-29; exception=N/A`

---

### Task PM-23: 实现 `notes merge --abort` / `--quit`（#989；自 PM-09 拆出；拆出 PM-39）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-09` 拆出：`libra notes merge --abort` 删除当前工作区的 `NOTES_MERGE_STATE`、`NOTES_MERGE_WORKTREE/` 与 `NOTES_MERGE_COMMITTING`（只有部分残留时同样清理），不改 `notes` 表；`--quit` 为 Libra 扩展（Git 2.56 无此选项），语义与 `--abort` 相同；无任何合并状态时 `--abort`/`--quit` 以 `LBR-REPO-003` 失败。本卡实现的命令形式遵守 ADR-PM-02 的锁规则：所有 `notes merge` 形式在读取或修改合并状态前以排他创建 `NOTES_MERGE_LOCK`（当前工作区 gitdir）取得互斥，结束时（成功或失败）删除；锁已存在即以 `LBR-CONFLICT-002` 失败且不读写任何状态（陈旧锁由用户确认无进程后手工删除，提示给出路径）。（按 G-09 拆出 `PM-39` 承接控制形式与选项互斥。）
**Out of scope:** 冲突物化（`PM-09`）；`--commit`（`PM-29`、`PM-38`）；控制形式与选项互斥（`PM-39`）；不单独验收的同路径情形：`--quit` 在无合并状态与锁已存在时的失败（与 `--abort` 共用同一状态检查与取锁代码，本卡以 `--abort` 验收）、AC5 之外的其余部分残留组合（同一逐路径清理）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `notes merge` 只有 `<OTHER_REF>` 与 `-s/--strategy` | `src/command/notes.rs:137-145` |
| Git：`--abort` 删除 `NOTES_MERGE_*`；无合并时报 `failed to remove 'git notes merge' worktree`（1）；`--quit` 为未知选项（129） | `git@6de20f6092:builtin/notes.c:894-940`；Git 2.54.0 预探 |

**夹具:** F09：`create_committed_repo_via_cli` 仓库，提交 C1 与其子提交 C2；`libra notes add -m 'x note' C1`（`refs/notes/commits`）；`libra notes --ref other add -m 'y note' C1` 与 `libra notes --ref other add -m 'only other' C2`（`libra notes add -m <文本>` 的 blob 内容即该文本、无尾随换行：`src/command/notes.rs:612-653`、`src/internal/notes.rs:182-205`；下文 `blob(<文本>)` 均指内容为该字节的 blob 的对象 ID（`notes` 表的 `blob` 列与 `NOTES_MERGE_STATE` 记录的都是该 OID：`Blob::from_content` 后存入 `blob.id`，`src/internal/notes.rs:199-217`））。冲突文件期望字节 W1 = `<<<<<<< refs/notes/commits\nx note\n=======\ny note\n>>>>>>> refs/notes/other\n`（Git 2.54.0 预探的标记与次序；冲突文件格式为 `<<<<<<< <目标 ref>\n`、目标侧内容、`=======\n`、来源侧内容、`>>>>>>> <来源 ref>\n`，某侧内容不以 `\n` 结尾时在其后补一个 `\n`——Libra 的 notes blob 无尾随换行，补齐后与 Git 在带换行 blob 上的字节一致）。合并状态位于当前工作区 gitdir（`util::request_worktree_gitdir()`，主工作区即 `.libra/`）。 需要冲突状态的用例先执行 `libra notes merge refs/notes/other`，并以执行该合并之前的「notes 快照」为 S0。 部分残留用例（AC5–AC6）在冲突状态下删除 `NOTES_MERGE_STATE` 并创建空文件 `NOTES_MERGE_COMMITTING`，以此时的「notes 快照」为 S2。

**Acceptance criteria:**
- [ ] **AC1** 冲突状态下 `libra notes merge --abort` 之后「notes 合并状态快照」等于 `(absent, absent, absent)`。
- [ ] **AC2** 该 `--abort` 之后「notes 快照」等于 S0。
- [ ] **AC3** 同一场景改用 `libra notes merge --quit` 之后「notes 合并状态快照」等于 `(absent, absent, absent)`。
- [ ] **AC4** 该 `--quit` 之后「notes 快照」等于 S0。
- [ ] **AC5** 部分残留（只有 `NOTES_MERGE_WORKTREE/` 与 `NOTES_MERGE_COMMITTING`）时 `--abort` 之后「notes 合并状态快照」等于 `(absent, absent, absent)`。
- [ ] **AC6** 该部分残留场景的 `--abort` 之后「notes 快照」等于 S2。
- [ ] **AC7** 无合并状态时 `libra notes merge --abort` 以 `LBR-REPO-003` 失败。
- [ ] **AC8** 冲突状态下已有 `NOTES_MERGE_LOCK` 时 `libra notes merge --abort` 以 `LBR-CONFLICT-002` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--abort`；家族子卡同卡执行第 1–5 步与第 6 步的后端本地签名提交（不推送），推送与第 7 步部署核验由 `PM-24` 执行（第 8 项）。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::notes_test::notes_merge_abort_and_quit_discard_state`：exit 0（前提）；「notes 合并状态快照」逐字等于 `(absent, absent, absent)`
- AC2 → `command::notes_test::notes_merge_abort_and_quit_discard_state`：「notes 快照」逐字等于 S0
- AC3 → `command::notes_test::notes_merge_abort_and_quit_discard_state`：exit 0（前提）；「notes 合并状态快照」逐字等于 `(absent, absent, absent)`
- AC4 → `command::notes_test::notes_merge_abort_and_quit_discard_state`：「notes 快照」逐字等于 S0
- AC5 → `command::notes_test::notes_merge_abort_and_quit_discard_state`：exit 0（前提）；「notes 合并状态快照」逐字等于 `(absent, absent, absent)`
- AC6 → `command::notes_test::notes_merge_abort_and_quit_discard_state`：「notes 快照」逐字等于 S2
- AC7 → `command::notes_test::notes_merge_abort_errors_and_lock`：退出码非 0（前提）；stderr 含 `LBR-REPO-003`
- AC8 → `command::notes_test::notes_merge_abort_errors_and_lock`：退出码非 0（前提）；stderr 含 `LBR-CONFLICT-002`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 5 步及第 6 步的后端本地签名提交（推送与第 7 步由 `PM-24` 执行，第 8 项）

**失败路径不变量:** 以稳定码失败的调用前后「notes 快照」与「notes 合并状态快照」逐字相等；AC8 的调用之后 `NOTES_MERGE_LOCK` 仍为夹具写入的原文件。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_merge_abort_and_quit_discard_state)'` `(new)`：覆盖 AC1–AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_merge_abort_errors_and_lock)'` `(new)`：覆盖 AC7–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（共享回归组：`command::notes_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** N/A（`family child of REL-PM-01`，全量与版本面由 `PM-24` 的 T-2 统一执行；本卡只跑 focused 与 fmt/clippy）
**Dependencies:** `PM-38`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/notes.rs`, `src/internal/notes.rs`, `tests/command/notes_test.rs`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`
**Release write set:** `N/A`
**Files likely touched:** `src/command/notes.rs`, `src/internal/notes.rs`, `tests/command/notes_test.rs`
**Docs and compatibility impact:** `docs/commands/notes.md` 与 zh-CN：`--abort`、Libra 扩展 `--quit`、部分残留的清理与无合并时的错误；`docs/development/commands/notes.md` 同步；`COMPATIBILITY.md` `notes` 行（W-1，基线第 220 行）原地登记 `--quit` 为 Libra 扩展；网站页 `notes.en.md` 由本卡同卡写入、评审并在后端 `cf` 本地签名提交（不推送；推送与部署核验由 `PM-24` 执行，见「文档与网站验收命令」第 8 项与 `DEP-PM-14`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-PM-01`
**C/D coverage from:** `PM-24`
**Granularity:** `type=implementation; axis=notes merge --abort/--quit; recovery=发布前按 REL-PM-01 逆序本地 revert；发布后随 PM-24 前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=PM-38,DEP-PM-11; writeset=串行于 PM-38; release=family child; split-from=PM-09; exception=N/A`

---

### Task PM-39: `notes merge` 控制形式与选项互斥（#989；自 PM-23 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-23` 拆出：`--commit`、`--abort`、`--quit` 三种控制形式两两互斥，且都不得与 `-s/--strategy` 或 `<notes-ref>` 位置参数同用，违者在取锁之前（因而先于任何状态读写——状态读写都在持锁之后进行）以 `LBR-CLI-002` 拒绝（Git：`cannot mix --commit, --abort or -s/--strategy` 与 `too many arguments`）；`-s/--strategy` 与 `<notes-ref>` 同用仍是正常的策略合并，行为不变（既有 `--strategy=theirs refs/notes/other` 用例由 `notes_test` 回归组守护）。
**Out of scope:** 三种控制形式各自的语义（`PM-29`、`PM-38`、`PM-23`）；`--quit` 与 Git 选项集合的其它组合（`--quit` 为 Libra 扩展，只按本卡规则互斥）；`--quit refs/notes/other`（与 AC4、AC5 共用同一「控制形式不得带位置参数」校验，不单独验收）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `notes merge` 只有 `<OTHER_REF>` 与 `-s/--strategy` | `src/command/notes.rs:137-145` |
| Git：`--commit --abort` 与 `-s ours --commit` 报 `cannot mix ...`（129）；`--commit`/`--abort` 带位置参数报 `too many arguments`（129） | `git@6de20f6092:builtin/notes.c:799-815,894-940`；Git 2.54.0 预探 |
| 既有策略合并用例 `--strategy=theirs refs/notes/other` 成功（本卡不得使其失败） | `tests/command/notes_test.rs:1458-1465`（`test_notes_merge_strategies_copy_and_manual_conflict`） |

**夹具:** F09：`create_committed_repo_via_cli` 仓库，提交 C1 与其子提交 C2；`libra notes add -m 'x note' C1`（`refs/notes/commits`）；`libra notes --ref other add -m 'y note' C1` 与 `libra notes --ref other add -m 'only other' C2`（`libra notes add -m <文本>` 的 blob 内容即该文本、无尾随换行：`src/command/notes.rs:612-653`、`src/internal/notes.rs:182-205`；下文 `blob(<文本>)` 均指内容为该字节的 blob 的对象 ID（`notes` 表的 `blob` 列与 `NOTES_MERGE_STATE` 记录的都是该 OID：`Blob::from_content` 后存入 `blob.id`，`src/internal/notes.rs:199-217`））。冲突文件期望字节 W1 = `<<<<<<< refs/notes/commits\nx note\n=======\ny note\n>>>>>>> refs/notes/other\n`（Git 2.54.0 预探的标记与次序；冲突文件格式为 `<<<<<<< <目标 ref>\n`、目标侧内容、`=======\n`、来源侧内容、`>>>>>>> <来源 ref>\n`，某侧内容不以 `\n` 结尾时在其后补一个 `\n`——Libra 的 notes blob 无尾随换行，补齐后与 Git 在带换行 blob 上的字节一致）。合并状态位于当前工作区 gitdir（`util::request_worktree_gitdir()`，主工作区即 `.libra/`）。 每个用例先执行 `libra notes merge refs/notes/other` 得到冲突状态。 AC1 另在调用前于工作区 gitdir 创建空文件 `NOTES_MERGE_LOCK`（模拟他人持锁）。

**Acceptance criteria:**
- [ ] **AC1** 工作区 gitdir 下已有 `NOTES_MERGE_LOCK` 时 `libra notes merge --commit --abort` 以 `LBR-CLI-002` 失败（选项校验先于取锁：先取锁的实现会以锁冲突的 `LBR-CONFLICT-002` 失败）。
- [ ] **AC2** `libra notes merge --abort --quit` 以 `LBR-CLI-002` 失败。
- [ ] **AC3** `libra notes merge -s ours --commit` 以 `LBR-CLI-002` 失败。
- [ ] **AC4** `libra notes merge --abort refs/notes/other` 以 `LBR-CLI-002` 失败。
- [ ] **AC5** `libra notes merge --commit refs/notes/other` 以 `LBR-CLI-002` 失败。
- [ ] **AC6** `libra notes merge --commit --quit` 以 `LBR-CLI-002` 失败。
- [ ] **AC7** `libra notes merge -s ours --abort` 以 `LBR-CLI-002` 失败。
- [ ] **AC8** `libra notes merge -s ours --quit` 以 `LBR-CLI-002` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--strategy`；家族子卡同卡执行第 1–5 步与第 6 步的后端本地签名提交（不推送），推送与第 7 步部署核验由 `PM-24` 执行（第 8 项）。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::notes_test::notes_merge_mode_validation`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC2 → `command::notes_test::notes_merge_mode_validation`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC3 → `command::notes_test::notes_merge_mode_validation`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC4 → `command::notes_test::notes_merge_mode_validation`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC5 → `command::notes_test::notes_merge_mode_validation`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC6 → `command::notes_test::notes_merge_mode_validation`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC7 → `command::notes_test::notes_merge_mode_validation`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC8 → `command::notes_test::notes_merge_mode_validation`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 5 步及第 6 步的后端本地签名提交（推送与第 7 步由 `PM-24` 执行，第 8 项）

**失败路径不变量:** 以稳定码失败的调用前后「notes 快照」与「notes 合并状态快照」逐字相等（在取锁之前拒绝）；AC1 的调用之后 `NOTES_MERGE_LOCK` 仍为夹具写入的原文件。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::notes_test::notes_merge_mode_validation)'` `(new)`：覆盖 AC1–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（共享回归组：`command::notes_test` 模块族（含既有 `--strategy=theirs refs/notes/other` 策略合并用例）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** N/A（`family child of REL-PM-01`，全量与版本面由 `PM-24` 的 T-2 统一执行；本卡只跑 focused 与 fmt/clippy）
**Dependencies:** `PM-23`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/notes.rs`, `src/internal/notes.rs`, `tests/command/notes_test.rs`, `docs/commands/notes.md`, `docs/commands/zh-CN/notes.md`, `docs/development/commands/notes.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`
**Release write set:** `N/A`
**Files likely touched:** `src/command/notes.rs`, `tests/command/notes_test.rs`
**Docs and compatibility impact:** `docs/commands/notes.md` 与 zh-CN：`--commit`/`--abort`/`--quit` 与 `-s`、`<notes-ref>` 的互斥规则；`docs/development/commands/notes.md` 同步；`COMPATIBILITY.md` `notes` 行（W-1，基线第 220 行）原地补记；网站页 `notes.en.md` 由本卡同卡写入、评审并在后端 `cf` 本地签名提交（不推送；推送与部署核验由 `PM-24` 执行，见「文档与网站验收命令」第 8 项与 `DEP-PM-14`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-PM-01`
**C/D coverage from:** `PM-24`
**Granularity:** `type=implementation; axis=notes merge控制形式与选项互斥; recovery=发布前按 REL-PM-01 逆序本地 revert；发布后随 PM-24 前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=PM-23,DEP-PM-11; writeset=串行于 PM-23; release=family child; split-from=PM-23; exception=N/A`

---

### Task PM-24: `REL-PM-01` 家族发布点（`notes merge` 状态机：PM-09、PM-29、PM-38、PM-23、PM-39）（#989）

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** `REL-PM-01` 唯一发布点：不引入新行为，在包含 `PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39` 五张子卡最终提交的树上执行 ER-08 `patch + 1`、版本面同步与工具链刷新 `Cargo.lock`、T-2 全量、`cargo build --release`、签名提交、PR、合并与 `gh release create`（以本卡 release note 作为 `--notes-file`），随后按「文档与网站验收命令」第 8 项的家族规则复核子卡已在后端本地提交的 `notes.en.md`、一次推送并做部署核验。
**Out of scope:** 任何生产代码或测试改动（发现缺陷时回到对应子卡修复并重跑其门，或按 ER-10 建 `FIX-*`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 家族窗口与逆序回滚 | 本计划「发布分组与并发窗口」`REL-PM-01` |
| 版本面集合权威 | `tests/compat/version_surface_sync.rs`（target `compat_version_surface_sync`） |

**夹具:** 无（发布点卡）。

**Acceptance criteria:**
- [ ] **AC1** `PM-09` 的本地签名提交是 `PM-24` 本地签名 bump 提交（squash merge 前的分支提交，下称「发布分支提交」）的祖先。
- [ ] **AC2** `PM-29` 的本地签名提交是发布分支提交的祖先。
- [ ] **AC3** `PM-38` 的本地签名提交是发布分支提交的祖先。
- [ ] **AC4** `PM-23` 的本地签名提交是发布分支提交的祖先。
- [ ] **AC5** `PM-39` 的本地签名提交是发布分支提交的祖先。
- [ ] **AC6** release note `NOTES_FILE`（仓库外临时目录中的 `gh release create --notes-file` 输入文件）含字面 `NOTES_MERGE_STATE`。
- [ ] **AC7** release note 含字面 `notes merge --commit`。
- [ ] **AC8** release note 含字面 `notes merge --abort`。
- [ ] **AC9** release note 含字面 `--quit is a Libra extension`。
- [ ] **AC10** release note 含字面 `NOTES_MERGE_LOCK`。
- [ ] **文档 AC**（ER-06/ER-06a，不计入 AC 分子）：本卡无新增仓库文档；子卡已同卡写入并在后端本地提交的 `notes.en.md` 按「文档与网站验收命令」第 8 项由本卡复核、推送并做部署核验（属下方「强制门」）。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `libra merge-base --is-ancestor <PM-09 提交> <发布分支提交>` exit 0
- AC2 → `libra merge-base --is-ancestor <PM-29 提交> <发布分支提交>` exit 0
- AC3 → `libra merge-base --is-ancestor <PM-38 提交> <发布分支提交>` exit 0
- AC4 → `libra merge-base --is-ancestor <PM-23 提交> <发布分支提交>` exit 0
- AC5 → `libra merge-base --is-ancestor <PM-39 提交> <发布分支提交>` exit 0
- AC6 → `grep -qF -- 'NOTES_MERGE_STATE' "$NOTES_FILE"` exit 0
- AC7 → `grep -qF -- 'notes merge --commit' "$NOTES_FILE"` exit 0
- AC8 → `grep -qF -- 'notes merge --abort' "$NOTES_FILE"` exit 0
- AC9 → `grep -qF -- '--quit is a Libra extension' "$NOTES_FILE"` exit 0
- AC10 → `grep -qF -- 'NOTES_MERGE_LOCK' "$NOTES_FILE"` exit 0
- 文档 AC → 「文档与网站验收命令」第 8 项的发布点规则（`notes` 页第 2–4、6、7 步）

**强制门（ER-04 C/D 组、ER-08、ER-13 T-2、ER-06a；不计入 AC/VER 分子）：** 「执行检查必备需求」中「ER-04 C 组 / ER-08 / ER-13 / ER-14」条的全部步骤——`patch + 1`、按 `compat_version_surface_sync` 同步版本面（`source .env.test && cargo nextest run --test compat_version_surface_sync` 通过）、工具链刷新 `Cargo.lock`、在已 bump 的工作树上运行 fmt、clippy 与 T-2 双环境全量 nextest（此后至签名提交前不再修改工作树）、`cargo build --release`、签名提交（提交后记录 `libra rev-parse HEAD` 与 `libra rev-parse HEAD^{tree}`，并以 `libra diff -s --exit-code HEAD -- Cargo.toml Cargo.lock install.sh install.ps1` exit 0 确认受测的版本面与 lockfile 字节即提交字节）、PR、squash merge、`gh release create "v${VERSION}" --target "$MERGE_SHA" --title "v${VERSION}" --notes-file "$NOTES_FILE"`（取代默认的 `--generate-notes`）；同节「D 组远端后置门」第 1–3 项的全部 job，另以 `gh release view "v${VERSION}" --json body --jq .body` 核对正文与 `NOTES_FILE` 内容一致；网站 `notes.en.md` 按「文档与网站验收命令」第 8 项的家族规则执行第 2–4 步复核、第 6 步的一次推送与第 7 步部署核验。任一失败即 STOP，本卡与五张子卡不得进入 `complete`。

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（共享回归组：家族聚合守卫：五张子卡新增的全部 notes 用例与既有 notes 用例；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_matrix_alignment`（共享回归组：整 target，兼容证据：`COMPATIBILITY.md` 与 CLI 命令集合对齐；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先 `source .env.test && cargo nextest list --test compat_matrix_alignment` 记录 N，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `test -s "$NOTES_FILE"` 与 `sh -c 'for k in NOTES_MERGE_STATE "notes merge --commit" "notes merge --abort" "--quit is a Libra extension" NOTES_MERGE_LOCK; do grep -qF -- "$k" "$1" || exit 1; done' _ "$NOTES_FILE"`（release note 门：前者为预检、后者为运行判定，两门均须 exit 0；覆盖 AC6–AC10；在 C 组第 ⑨ 步 `gh release create` 之前执行）
- [ ] `sh -c 'for c in "$@"; do libra rev-parse --verify "$c^{commit}" >/dev/null || exit 1; done' _ "$PM09" "$PM29" "$PM38" "$PM23" "$PM39" "$REL"` 与 `sh -c 'for c in "$1" "$2" "$3" "$4" "$5"; do libra merge-base --is-ancestor "$c" "$6" || exit 1; done' _ "$PM09" "$PM29" "$PM38" "$PM23" "$PM39" "$REL"`（祖先门：`PM09`/`PM29`/`PM38`/`PM23`/`PM39` 依次为 `PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39` 五张子卡的本地签名提交 ID、`REL` 为发布分支提交 ID，均取自各卡执行记录；前者为预检、后者为运行判定，两门均须 exit 0，非零即 STOP；覆盖 AC1–AC5；在 C 组签名提交之后、推送之前执行）

**Full-suite trigger:** T-2: `Task type = release` 的家族发布点（ER-13 T-2；另含版本面 T-1）
**Dependencies:** `PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39`
**Deliverables:** `N/A`
**Implementation write set:** N/A（发布点卡不改实现文件；网站 `notes.en.md` 的最终字节由子卡草稿经第 3 步同步产生）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md`
**Docs and compatibility impact:** 无新增仓库文档改动（子卡已同卡交付）；网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/notes.en.md` 由子卡同卡写入并在后端本地提交，本卡按「文档与网站验收命令」第 8 项复核、推送并做部署核验。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-PM-01`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-PM-01 notes merge家族统一发布; recovery=发布前按 PM-24 → PM-39 → PM-23 → PM-38 → PM-29 → PM-09 逆序本地 revert（后端本地提交同序撤回，见 DEP-PM-14）；发布后前滚新 patch; complete=yes; self-contained=yes; AC=10/12; VER=8/12; landing=0; prod-files=0; scope=S; deps=PM-09,PM-29,PM-38,PM-23,PM-39; writeset=串行于 PM-39; release=family release point; split-from=N/A; exception=N/A`

---

### Task PM-10: 新增 `libra var` 命令：身份变量、`-l`、`--json` 与注册/帮助守卫（#995；拆出 PM-10a、PM-22、PM-25）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：新增 `src/command/var.rs` 并在 `src/cli.rs` 注册 `Commands::Var`（`VAR_EXAMPLES`、`Commands::Var => ReadOnly`、`Maintenance And Plumbing` 分组行, 并加 `src/command/mod.rs`）；身份只取 `resolve_user_identity_sources(LocalIdentityTarget::CurrentRepo)` 的 `config_name`/`config_email`（忽略 `env_*`），时间戳/时区取 `Signature::new` 的本地时刻，再按字段拼一行 ident（`Signature` 的 `Display` 是多行对象头，故不直接用）；`-l` 输出两行 `NAME=value`（跳过错解身份），`--json` → `{name,value}`；未知/缺少名称 `LBR-CLI-002`、身份缺失 `LBR-AUTH-001`、仓库外 `LBR-REPO-001`。AC1–AC7 由 `command::var_test::{var_ident_ignores_identity_and_date_env,var_list_reports_both_idents_in_order,var_rejects_unknown_and_missing_repo,var_json_and_missing_identity}` 覆盖（4/4 通过）；EN/zh/开发文档、`docs/commands/README.md` 与 `COMPATIBILITY.md` 的 `var` 行同步。
**Description:** 按 ADR-PM-01 新增 `src/command/var.rs` 并在 `src/cli.rs` 注册 `Commands::Var`（`VAR_EXAMPLES`、Command Groups「Maintenance And Plumbing」行、`command_scope` 只读分组）：`libra var GIT_COMMITTER_IDENT` / `GIT_AUTHOR_IDENT` 以配置身份（`resolve_user_identity_sources` 的 `config_name`/`config_email`，即 `user.name`/`user.email` 的本地 → 全局级联，GC-02 单一事实源）与当前时刻、本地时区偏移（`TZ` 决定）输出 `Name <email> <unix-ts> <±hhmm>`；本卡忽略身份与日期环境变量（`GIT_*`、`EMAIL`、`LIBRA_COMMITTER_*`）的取值——实现只使用 `resolve_user_identity_sources` 返回结果中的配置字段 `config_name`/`config_email`（不使用其 `env_*` 字段）与当前时刻，AC1、AC2 在设置了冲突的环境变量时验收这一点——该限制写入本卡文档，由 `PM-10a`（作者）与 `PM-37`（提交者）改接 `commit` 的分角色签名解析；`-l` 按 Git 变量表顺序逐行输出 `NAME=value`（只列本卡支持的两个身份变量、跳过无法解析的变量，不输出配置项）；未知变量名、无参数或多参数以 `LBR-CLI-002` 失败；身份缺失以 `LBR-AUTH-001` 失败；`--json` 输出 `{name, value}`。需要仓库（仓库外 `LBR-REPO-001`，有意差异）。（按 G-09 拆出 `PM-10a` 作者侧环境变量（其再拆出 `PM-37` 提交者侧）、`PM-22` 编辑器变量、`PM-25` 其余变量与 `-l` 全表。）
**Out of scope:** 身份与日期环境变量的接入（作者侧 `PM-10a`、提交者侧 `PM-37`；本卡只验收其被忽略，见 AC1、AC2）；`GIT_EDITOR`（`PM-22`）与 `GIT_SEQUENCE_EDITOR`（`PM-36`）；`GIT_PAGER`/`GIT_DEFAULT_BRANCH`/`GIT_ATTR_*` 与 `-l` 全表（`PM-25`）；`-l` 输出配置项（永久非目标，理由：Libra 配置含 vault/加密条目，不在 `var` 中回显，GC-07）；`GIT_SHELL_PATH`、`GIT_CONFIG_SYSTEM`/`GIT_CONFIG_GLOBAL`（尚未排期，重启条件：#995 后续需要）；无参数、多参数与小写变量名等其它非法调用（与 AC4 同为用法错误 `LBR-CLI-002`，不单独验收）；`command_scope` 只读分组的登记（注册细节，由 ER-05 代码评审与 T-1 全量门核对，不单独验收）；`var --help` 的 EXAMPLES 段（由 `compat_help_examples_banner` 回归守护）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 无 `var` 命令（`libra var` 报 `LBR-CLI-001`） | `src/cli.rs:344`（`enum Commands`）；`src/command/mod.rs:132-133` |
| 新命令登记点：Command Groups 行、只读 `command_scope`、分派 | `src/cli.rs:65,2166-2201,3492` |
| 配置身份来源：`config_name`/`config_email` 只取 `user.name`/`user.email` 的配置级联（环境变量另存于 `env_*` 字段）；`commit` 将身份缺失映射为 `LBR-AUTH-001` | `src/internal/config.rs:1735-1751,2344-2362`；`src/command/commit.rs:467-469` |
| 帮助/文档/矩阵守卫：`compat_help_examples_banner`（硬编码 `VISIBLE_COMMANDS`）、`compat_command_docs_examples_section`、`compat_matrix_alignment`、`cli::tests::root_after_help_lists_every_visible_command` | `tests/compat/help_examples_banner.rs:53-121`；`tests/compat/command_docs_examples_section.rs`；`tests/compat/matrix_alignment.rs`；`src/cli.rs:5045-5072` |
| Git 变量表顺序与 `-l` 先打印配置项；未知变量 `usage: git var (-l | <variable>)`（129） | `git@6de20f6092:builtin/var.c:22,123-173,196-215`；Git 2.54.0 预探 |
| `resolve_user_identity_sources` 同时读出配置字段（`config_*`）与环境变量字段（`env_*`） | `src/internal/config.rs:1735-1751,2344-2362` |
| 仓库外运行需要仓库的命令以 `LBR-REPO-001` 失败 | `src/utils/error.rs:376,826`；`docs/error-codes.md:100` |

**夹具:** F10：`create_committed_repo_via_cli` 仓库（`tests/command/mod.rs:696`）；`libra config user.name 'C O Mitter'`、`libra config user.email c@d`；除 AC1、AC2 写明的环境变量外，子进程环境不含任何身份与日期变量；全部子进程另设 `TZ=Asia/Shanghai`（无夏令时，本地偏移固定为 `+0800`；`Signature::new` 以 `chrono::Local` 取本地偏移，`git-internal-0.10.4/src/internal/object/signature.rs:241-258`）。F10x：同法建库后 `libra config --unset user.name` 与 `libra config --unset user.email`（隔离全局库中亦无身份）。 F10o：`tempfile::tempdir()` 新建的空目录（其自身与祖先目录均无 `.libra`），以显式 `current_dir` 运行。AC3、AC6 的子进程不设任何身份与日期环境变量。

**Acceptance criteria:**
- [ ] **AC1** 子进程另设 `GIT_COMMITTER_NAME='Env Name'`、`GIT_COMMITTER_EMAIL=env@x`、`GIT_COMMITTER_DATE='1700000000 +0000'` 时，`libra var GIT_COMMITTER_IDENT` 的 stdout 仍属于集合 {`C O Mitter <c@d> <T> +0800\n` ∣ t0 ≤ T ≤ t1}（t0/t1 为该次调用前后的 Unix 秒；`+0800` 为夹具 `TZ` 下的本地偏移）。
- [ ] **AC2** 子进程另设 `GIT_AUTHOR_NAME='Env Name'`、`GIT_AUTHOR_EMAIL=env@x`、`GIT_AUTHOR_DATE='1700000000 +0000'` 时，`libra var GIT_AUTHOR_IDENT` 的 stdout 仍属于同形集合（以该次调用前后的 t0/t1 计）。
- [ ] **AC3** `libra var -l` 恰输出两行，依次为 `GIT_COMMITTER_IDENT=<v1>` 与 `GIT_AUTHOR_IDENT=<v2>`，其中 v1、v2 各自属于 AC1 形式的集合（以该次 `-l` 调用前后的 t0/t1 计，去掉换行）。
- [ ] **AC4** `libra var BOGUS` 以 `LBR-CLI-002` 失败。
- [ ] **AC5** 在不属于任何 Libra 仓库的空临时目录（F10o）中 `libra var GIT_COMMITTER_IDENT` 以 `LBR-REPO-001` 失败（需要仓库，有意差异）。
- [ ] **AC6** `libra --json var GIT_COMMITTER_IDENT` 的 JSON 信封中 `data` 恰为 `{"name":"GIT_COMMITTER_IDENT","value":v}`，v 属于 AC1 形式的集合（以该次调用计，去掉换行）。
- [ ] **AC7** F10x 中 `libra var GIT_COMMITTER_IDENT` 以 `LBR-AUTH-001` 失败。
- [ ] **AC8** F10x 中 `libra var -l` 的 stdout 为空。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`GIT_COMMITTER_IDENT`；另按「验证执行协议」第 5 项运行文档守卫 `source .env.test && cargo nextest run --test compat_command_docs_examples_section` 与 `source .env.test && cargo nextest run --test compat_matrix_alignment`（各自先 `cargo nextest list` 记录 N ≥ 1，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录），作为 `docs/commands/var.md` Examples 段、`COMPATIBILITY.md` `var` 行与 `docs/development/commands/README.md` `var` 行的逐文件验收证据。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::var_test::var_ident_list_and_json`：exit 0（前提）；stdout 属于该集合
- AC2 → `command::var_test::var_ident_list_and_json`：exit 0（前提）；stdout 属于该集合
- AC3 → `command::var_test::var_ident_list_and_json`：exit 0（前提）；stdout 恰两行且次序、前缀与两值的集合归属如上
- AC4 → `command::var_test::var_rejects_invalid_usage_and_missing_identity`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC5 → `command::var_test::var_rejects_invalid_usage_and_missing_identity`：退出码非 0（前提）；stderr 含 `LBR-REPO-001`
- AC6 → `command::var_test::var_ident_list_and_json`：exit 0（前提）；stdout 解析为一个 JSON 对象后 `data` 满足上式
- AC7 → `command::var_test::var_rejects_invalid_usage_and_missing_identity`：退出码非 0（前提）；stderr 含 `LBR-AUTH-001`
- AC8 → `command::var_test::var_rejects_invalid_usage_and_missing_identity`：exit 0（前提）；stdout 为空
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::var_test::var_ident_list_and_json)'` `(new)`：覆盖 AC1–AC3、AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::var_test::var_rejects_invalid_usage_and_missing_identity)'` `(new)`：覆盖 AC4–AC5、AC7–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(=cli::tests::root_after_help_lists_every_visible_command)'`（共享回归组：Command Groups 行包含 `var`；预期 N = 1；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_help_examples_banner`（共享回归组：整 target，`VISIBLE_COMMANDS` 已加入 `var` 后的 EXAMPLES 守卫；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先 `source .env.test && cargo nextest list --test compat_help_examples_banner` 记录 N，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1, T-4: `src/cli.rs` 命令注册（T-1）；`tests/command/mod.rs` 共享测试基建（T-4）；另含版本面 `install.sh`/`install.ps1`
**Dependencies:** `PM-24`、`DEP-PM-11`、`DEP-PM-13`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/var.rs`, `src/command/mod.rs`, `src/cli.rs`, `tests/command/var_test.rs`, `tests/command/mod.rs`, `tests/compat/help_examples_banner.rs`, `docs/commands/var.md`, `docs/commands/zh-CN/var.md`, `docs/development/commands/var.md`, `docs/development/commands/README.md`, `docs/commands/README.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/var.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/meta.en.json`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/index.en.mdx`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/var.rs`, `src/command/mod.rs`, `src/cli.rs`, `tests/command/var_test.rs`, `tests/command/mod.rs`, `tests/compat/help_examples_banner.rs`
**Docs and compatibility impact:** 新建 `docs/commands/var.md` 与 `docs/commands/zh-CN/var.md`（含 `## Examples`，并写明本版本的身份只取配置、尚不读取身份与日期环境变量）、`docs/development/commands/var.md`；`docs/development/commands/README.md` 公开命令表与 `docs/commands/README.md` Low-Level 表各加 `var` 行；`COMPATIBILITY.md` 顶层命令表在 `update-ref` 行（基线第 243 行，ledger 锚点均 ≤ 180 行）之后插入一行 `| var | partial | … |`；网站新建 `var.en.md` 并以 `--only var index` 同步 `meta.en.json` 与 `index.en.mdx`。`tests/INDEX.md` 的 `command_test` 行已覆盖全部 `command::*` 模块，不需新增行（理由：`tests/INDEX.md:44` 为整 target 描述）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=var命令注册与身份变量查询; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=2; prod-files=3; scope=M; deps=PM-24,DEP-PM-11,DEP-PM-13; writeset=串行于 PM-24; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-10a: `libra var GIT_AUTHOR_IDENT` 接入作者侧身份与日期环境变量（#995；自 PM-10 拆出；拆出 PM-37）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：`src/command/commit.rs` 新增 `pub(crate) create_author_signature()`——复用既有私有 `resolve_author_identity`/`env_date`/`apply_signature_date`，只解析作者身份与 `GIT_AUTHOR_DATE`；`create_commit_signatures` 与 `commit` 路径未改（`commit_test` 63/63）。`libra var GIT_AUTHOR_IDENT` 改走该入口，提交者侧仍用 PM-10 的配置-only 路径（留待 PM-37）。AC1–AC8 由 `command::var_test::var_author_ident_uses_author_env` 覆盖（含 `user.useConfigOnly` 只抑制身份环境变量、只校验作者侧日期）；`var_test` 5/5；EN/zh/开发文档与 `COMPATIBILITY.md` 的 `var` 行同步。注：PM-10 的 AC2（作者侧忽略环境变量）已被本卡取代，回归测试改名为 `var_committer_ident_ignores_env_until_pm37` 只保留提交者侧断言。
**Description:** 自 `PM-10` 拆出：在 `src/command/commit.rs` 新增 `pub(crate)` 的作者专用入口，复用同文件既有的私有 helper `resolve_author_identity`、`env_date` 与 `apply_signature_date`（GC-02）只构造作者签名；`create_commit_signatures` 与 `commit` 的代码路径不修改（解析与错误优先顺序原样保留），`libra var GIT_AUTHOR_IDENT` 改用该入口：作者名按 `GIT_AUTHOR_NAME` > `GIT_COMMITTER_NAME` > `LIBRA_COMMITTER_NAME` > `user.name`、邮箱按 `GIT_AUTHOR_EMAIL` > `GIT_COMMITTER_EMAIL` > `EMAIL` > `LIBRA_COMMITTER_EMAIL` > `user.email`（Libra 既有次序，含交叉回退与环境变量先于配置，均为有意差异，`var` 报告 `libra commit` 实际会写入的作者），`user.useConfigOnly` 为真时只忽略身份（名字与邮箱）环境变量、日期环境变量仍生效（同 `commit`，`src/command/commit.rs:724-794,896-937`）；`GIT_AUTHOR_DATE` 生效、非法时以 `LBR-CLI-002` 失败；只校验作者侧日期，非法的 `GIT_COMMITTER_DATE` 不影响 `GIT_AUTHOR_IDENT`（同 Git `builtin/var.c` 的 `git_author_info`）。（按 G-09 拆出 `PM-37` 承接 `GIT_COMMITTER_IDENT`。）
**Out of scope:** `GIT_COMMITTER_IDENT` 的环境变量（`PM-37`）；`author.*`/`committer.*` 配置、Git 的 `$EMAIL` 低于配置次序、身份自动探测（Libra 身份解析的既有差异，尚未排期）；作者名链中的 `LIBRA_COMMITTER_NAME` 与作者邮箱链中的 `GIT_COMMITTER_EMAIL`/`LIBRA_COMMITTER_EMAIL`（`author_env_identity` 链 `src/command/commit.rs:692-706` 的其余链位；本卡按 Description 原样复用、不修改该 helper，不单独验收）；编辑器与其它变量（`PM-22`、`PM-25`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 作者环境变量链与交叉回退 | `src/command/commit.rs:692-706`（`author_env_identity`）、`:760-794`（`resolve_author_identity`） |
| `create_commit_signatures` 先解析作者与提交者身份、再依次应用 `GIT_AUTHOR_DATE` 与 `GIT_COMMITTER_DATE`（后者非法同样使整个调用失败） | `src/command/commit.rs:896-924` |
| 非法日期映射 `LBR-CLI-002` | `src/command/commit.rs:483-486` |
| Git：`GIT_AUTHOR_IDENT` 只读作者侧变量（`git_author_info`） | `git@6de20f6092:builtin/var.c:24-32`、`ident.c:456-589`；Git 2.54.0 预探 |
| `PM-10` 的 AC2 在设置作者侧环境变量时断言仍输出配置身份；本卡起作者侧环境变量生效，故同卡删去该调用设置的作者侧环境变量（期望值不变），其余断言保留（「验证执行协议」第 5 项） | `command::var_test::var_ident_list_and_json`（`PM-10`） |

**夹具:** F10a：`create_committed_repo_via_cli` 仓库（`tests/command/mod.rs:696`）；`libra config user.name 'Cfg Name'`、`libra config user.email cfg@x`；各用例只设置 AC 中列出的环境变量（`run_libra_command_with_env`），子进程环境不含其它身份变量；全部子进程另设 `TZ=Asia/Shanghai`（无夏令时，本地偏移固定为 `+0800`；`Signature::new` 以 `chrono::Local` 取本地偏移，`git-internal-0.10.4/src/internal/object/signature.rs:241-258`）。

**Acceptance criteria:**
- [ ] **AC1** 同时设 `GIT_AUTHOR_NAME='A U Thor'` 与 `GIT_COMMITTER_NAME='C O Mitter'` 时 `libra var GIT_AUTHOR_IDENT` 匹配 `^A U Thor <cfg@x> \d+ \+0800\n$`（作者侧变量优先）。
- [ ] **AC2** 同时设 `GIT_AUTHOR_EMAIL=a@b` 与 `GIT_COMMITTER_EMAIL=c@d` 时 `GIT_AUTHOR_IDENT` 匹配 `^Cfg Name <a@b> \d+ \+0800\n$`（作者侧变量优先）。
- [ ] **AC3** 只设 `GIT_AUTHOR_DATE='1700000000 +0800'` 时 `GIT_AUTHOR_IDENT` 恰为 `Cfg Name <cfg@x> 1700000000 +0800\n`。
- [ ] **AC4** `GIT_AUTHOR_DATE=garbage` 时 `libra var GIT_AUTHOR_IDENT` 以 `LBR-CLI-002` 失败。
- [ ] **AC5** `GIT_AUTHOR_DATE='1700000000 +0800'` 且 `GIT_COMMITTER_DATE=garbage` 时 `GIT_AUTHOR_IDENT` 恰为 `Cfg Name <cfg@x> 1700000000 +0800\n`（只校验作者侧日期）。
- [ ] **AC6** 只设 `GIT_COMMITTER_NAME='C O Mitter'` 时 `GIT_AUTHOR_IDENT` 匹配 `^C O Mitter <cfg@x> \d+ \+0800\n$`（Libra 交叉回退）。
- [ ] **AC7** 只设 `EMAIL=e@f` 时 `GIT_AUTHOR_IDENT` 匹配 `^Cfg Name <e@f> \d+ \+0800\n$`（Libra 环境变量先于配置）。
- [ ] **AC8** `libra config user.useConfigOnly true` 后同时设 `GIT_AUTHOR_NAME='A U Thor'` 与 `GIT_AUTHOR_DATE='1700000000 +0800'` 时 `libra var GIT_AUTHOR_IDENT` 恰为 `Cfg Name <cfg@x> 1700000000 +0800\n`（只忽略身份环境变量，日期仍生效）。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`GIT_AUTHOR_DATE`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::var_test::var_author_ident_env_overrides`：exit 0；stdout 匹配该正则
- AC2 → `command::var_test::var_author_ident_env_overrides`：exit 0；stdout 匹配该正则
- AC3 → `command::var_test::var_author_ident_env_overrides`：exit 0；stdout 逐字等于 `Cfg Name <cfg@x> 1700000000 +0800\n`
- AC4 → `command::var_test::var_author_ident_env_overrides`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC5 → `command::var_test::var_author_ident_env_overrides`：exit 0；stdout 逐字等于 `Cfg Name <cfg@x> 1700000000 +0800\n`
- AC6 → `command::var_test::var_author_ident_env_overrides`：exit 0；stdout 匹配该正则
- AC7 → `command::var_test::var_author_ident_env_overrides`：exit 0；stdout 匹配该正则
- AC8 → `command::var_test::var_author_ident_env_overrides`：exit 0；stdout 逐字等于 `Cfg Name <cfg@x> 1700000000 +0800\n`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::var_test::var_author_ident_env_overrides)'` `(new)`：覆盖 AC1–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::var_test::/)'`（共享回归组：`command::var_test` 模块族（含 `PM-10` 用例）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（共享回归组：`command::commit_test` 模块族：新增作者入口后 `commit` 的身份与日期解析保持（`create_commit_signatures` 未改）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-10`、`DEP-PM-11`、`DEP-PM-13`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/var.rs`, `src/command/commit.rs`, `tests/command/var_test.rs`, `docs/commands/var.md`, `docs/commands/zh-CN/var.md`, `docs/development/commands/var.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/var.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/var.rs`, `src/command/commit.rs`, `tests/command/var_test.rs`
**Docs and compatibility impact:** `docs/commands/var.md` 与 zh-CN：`GIT_AUTHOR_IDENT` 的作者侧环境变量、日期格式、只校验作者侧日期与 Libra 交叉回退/`EMAIL` 次序差异；`docs/development/commands/var.md` 同步；`PM-10` 文档中「尚不读取环境变量」的说明改为只对 `GIT_COMMITTER_IDENT` 成立（直至 `PM-37`）；`COMPATIBILITY.md` `var` 行（W-1）原地补记差异；网站页 `var.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=var作者身份环境变量; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=PM-10,DEP-PM-11,DEP-PM-13; writeset=串行于 PM-10; release=independent; split-from=PM-10; exception=N/A`

---

### Task PM-37: `libra var GIT_COMMITTER_IDENT` 接入提交者侧身份与日期环境变量（#995；自 PM-10a 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-10a` 拆出：`libra var GIT_COMMITTER_IDENT` 改用 `commit` 的提交者专用入口 `create_committer_signature`（`src/command/commit.rs:926-937`，只解析提交者身份与 `GIT_COMMITTER_DATE`）：提交者名按 `GIT_COMMITTER_NAME` > `GIT_AUTHOR_NAME` > `LIBRA_COMMITTER_NAME` > `user.name`、邮箱按 `GIT_COMMITTER_EMAIL` > `GIT_AUTHOR_EMAIL` > `EMAIL` > `LIBRA_COMMITTER_EMAIL` > `user.email`（Libra 既有次序与交叉回退，有意差异），`user.useConfigOnly` 为真时只忽略身份（名字与邮箱）环境变量、日期环境变量仍生效（同 `commit`，`src/command/commit.rs:724-794,896-937`）；`GIT_COMMITTER_DATE` 生效、非法时以 `LBR-CLI-002` 失败；非法的 `GIT_AUTHOR_DATE` 不影响 `GIT_COMMITTER_IDENT`。
**Out of scope:** `GIT_AUTHOR_IDENT`（`PM-10a`）；提交者名链中的 `LIBRA_COMMITTER_NAME` 与提交者邮箱链中的 `GIT_AUTHOR_EMAIL`/`LIBRA_COMMITTER_EMAIL`（`committer_env_identity` 链 `src/command/commit.rs:708-722` 的其余链位；本卡调用既有 `create_committer_signature`、不修改 `src/command/commit.rs`，不单独验收）；`author.*`/`committer.*` 配置与身份自动探测（尚未排期）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 提交者环境变量链与交叉回退 | `src/command/commit.rs:708-722`（`committer_env_identity`）、`:724-758`（`resolve_committer_identity`） |
| `create_committer_signature` 只解析提交者身份与 `GIT_COMMITTER_DATE` | `src/command/commit.rs:926-937` |
| Git：`GIT_COMMITTER_IDENT` 只读提交者侧变量（`git_committer_info`） | `git@6de20f6092:builtin/var.c:24-32`、`ident.c:456-589`；Git 2.54.0 预探 |
| `PM-10` 的 AC1 在设置提交者侧环境变量时断言仍输出配置身份；本卡起提交者侧环境变量生效，故同卡删去该调用设置的提交者侧环境变量（期望值不变），其余断言保留（「验证执行协议」第 5 项） | `command::var_test::var_ident_list_and_json`（`PM-10`） |

**夹具:** F37：`create_committed_repo_via_cli` 仓库（`tests/command/mod.rs:696`）；`libra config user.name 'Cfg Name'`、`libra config user.email cfg@x`；各用例只设置 AC 中列出的环境变量（`run_libra_command_with_env`），子进程环境不含其它身份变量；全部子进程另设 `TZ=Asia/Shanghai`（无夏令时，本地偏移固定为 `+0800`；`Signature::new` 以 `chrono::Local` 取本地偏移，`git-internal-0.10.4/src/internal/object/signature.rs:241-258`）。

**Acceptance criteria:**
- [ ] **AC1** 同时设 `GIT_COMMITTER_NAME='C O Mitter'` 与 `GIT_AUTHOR_NAME='A U Thor'` 时 `libra var GIT_COMMITTER_IDENT` 匹配 `^C O Mitter <cfg@x> \d+ \+0800\n$`（提交者侧变量优先）。
- [ ] **AC2** 同时设 `GIT_COMMITTER_EMAIL=c@d` 与 `GIT_AUTHOR_EMAIL=a@b` 时 `GIT_COMMITTER_IDENT` 匹配 `^Cfg Name <c@d> \d+ \+0800\n$`（提交者侧变量优先）。
- [ ] **AC3** 只设 `GIT_COMMITTER_DATE='1700000000 -0700'` 时 `GIT_COMMITTER_IDENT` 恰为 `Cfg Name <cfg@x> 1700000000 -0700\n`。
- [ ] **AC4** `GIT_COMMITTER_DATE=garbage` 时 `libra var GIT_COMMITTER_IDENT` 以 `LBR-CLI-002` 失败。
- [ ] **AC5** `GIT_COMMITTER_DATE='1700000000 -0700'` 且 `GIT_AUTHOR_DATE=garbage` 时 `GIT_COMMITTER_IDENT` 恰为 `Cfg Name <cfg@x> 1700000000 -0700\n`（只校验提交者侧日期）。
- [ ] **AC6** 只设 `GIT_AUTHOR_NAME='A U Thor'` 时 `GIT_COMMITTER_IDENT` 匹配 `^A U Thor <cfg@x> \d+ \+0800\n$`（Libra 交叉回退）。
- [ ] **AC7** 只设 `EMAIL=e@f` 时 `GIT_COMMITTER_IDENT` 匹配 `^Cfg Name <e@f> \d+ \+0800\n$`（Libra 环境变量先于配置）。
- [ ] **AC8** `libra config user.useConfigOnly true` 后同时设 `GIT_COMMITTER_NAME='C O Mitter'` 与 `GIT_COMMITTER_DATE='1700000000 -0700'` 时 `libra var GIT_COMMITTER_IDENT` 恰为 `Cfg Name <cfg@x> 1700000000 -0700\n`（只忽略身份环境变量，日期仍生效）。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`GIT_COMMITTER_DATE`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::var_test::var_committer_ident_env_overrides`：exit 0；stdout 匹配该正则
- AC2 → `command::var_test::var_committer_ident_env_overrides`：exit 0；stdout 匹配该正则
- AC3 → `command::var_test::var_committer_ident_env_overrides`：exit 0；stdout 逐字等于 `Cfg Name <cfg@x> 1700000000 -0700\n`
- AC4 → `command::var_test::var_committer_ident_env_overrides`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC5 → `command::var_test::var_committer_ident_env_overrides`：exit 0；stdout 逐字等于 `Cfg Name <cfg@x> 1700000000 -0700\n`
- AC6 → `command::var_test::var_committer_ident_env_overrides`：exit 0；stdout 匹配该正则
- AC7 → `command::var_test::var_committer_ident_env_overrides`：exit 0；stdout 匹配该正则
- AC8 → `command::var_test::var_committer_ident_env_overrides`：exit 0；stdout 逐字等于 `Cfg Name <cfg@x> 1700000000 -0700\n`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::var_test::var_committer_ident_env_overrides)'` `(new)`：覆盖 AC1–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::var_test::/)'`（共享回归组：`command::var_test` 模块族（含 `PM-10`、`PM-10a` 用例）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-10a`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/var.rs`, `tests/command/var_test.rs`, `docs/commands/var.md`, `docs/commands/zh-CN/var.md`, `docs/development/commands/var.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/var.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/var.rs`, `tests/command/var_test.rs`
**Docs and compatibility impact:** `docs/commands/var.md` 与 zh-CN：`GIT_COMMITTER_IDENT` 的提交者侧环境变量、日期格式、只校验提交者侧日期与 Libra 交叉回退/`EMAIL` 次序差异（删去 `PM-10` 文档中「尚不读取环境变量」的限制说明）；`docs/development/commands/var.md` 同步；`COMPATIBILITY.md` `var` 行（W-1）原地补记差异；网站页 `var.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=var提交者身份环境变量; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=PM-10a,DEP-PM-11; writeset=串行于 PM-10a; release=independent; split-from=PM-10a; exception=N/A`

---

### Task PM-22: 完善 `libra var` 的 `GIT_EDITOR`（#995；自 PM-10 拆出；拆出 PM-36）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-10` 拆出：`libra var GIT_EDITOR` 复用 `src/command/editor.rs` 的 `resolve_editor`（`GIT_EDITOR` > `core.editor` > `VISUAL` > `EDITOR`）；全部未配置时按 Git：`TERM` 未设置或为 `dumb` 则以退出码 1 结束且 stdout 为空（stderr 不作承诺），否则输出 `vi`。（按 G-09 拆出 `PM-36` 承接 `GIT_SEQUENCE_EDITOR`。）
**Out of scope:** `GIT_SEQUENCE_EDITOR`（`PM-36`）；`resolve_editor` 链中 AC1–AC3 之外的相对次序（`core.editor` 先于 `VISUAL`、只设 `EDITOR`）——本卡原样复用且不修改该函数（`src/command/editor.rs:38-57`），不单独验收；`TERM=dumb` 时跳过 `VISUAL` 的 Git 规则（有意差异：`var` 报告 `resolve_editor` 的实际结果，`commit`/`tag` 编辑器启动同样不看 `TERM`）；`-l` 中的编辑器行（`PM-25` 的 `-l` 全表）；`GIT_PAGER`（`PM-25`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_editor`：`GIT_EDITOR` > `core.editor` > `VISUAL` > `EDITOR`，空值跳过；`resolve_sequence_editor` 链 | `src/command/editor.rs:38-57,103-112` |
| Git：`TERM` 未设或 dumb 且无配置时退出 1；否则 `vi`；序列编辑器回退到编辑器结果 | `git@6de20f6092:editor.c:27-58`；Git 2.54.0 预探 |

**夹具:** F22：`create_committed_repo_via_cli` 仓库（`tests/command/mod.rs:696`）；`libra config user.name 'C O Mitter'`、`libra config user.email c@d`；环境变量（含 `TERM`）经 `run_libra_command_with_env` 逐子进程传入，测试 helper 默认清空环境故未列出的 `TERM` 为未设置。

**Acceptance criteria:**
- [ ] **AC1** `core.editor=emacs` 且 `GIT_EDITOR=nano` 时 `libra var GIT_EDITOR` 输出 `nano`。
- [ ] **AC2** 只有 `core.editor=emacs` 时输出 `emacs`。
- [ ] **AC3** `TERM=xterm`、`VISUAL=vim`、`EDITOR=ed` 时输出 `vim`。
- [ ] **AC4** `TERM=xterm` 且无任何编辑器配置时输出 `vi`。
- [ ] **AC5** 无任何编辑器配置且 `TERM` 未设置时 `libra var GIT_EDITOR` 的结果（退出码, stdout）为 (1, 空)。
- [ ] **AC6** 无任何编辑器配置且 `TERM=dumb` 时 `libra var GIT_EDITOR` 的结果（退出码, stdout）为 (1, 空)。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`GIT_EDITOR`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::var_test::var_editor_variable`：exit 0；stdout 逐字等于 `nano\n`
- AC2 → `command::var_test::var_editor_variable`：exit 0；stdout 逐字等于 `emacs\n`
- AC3 → `command::var_test::var_editor_variable`：exit 0；stdout 逐字等于 `vim\n`
- AC4 → `command::var_test::var_editor_variable`：exit 0；stdout 逐字等于 `vi\n`
- AC5 → `command::var_test::var_editor_variable`：（退出码, stdout）二元组等于 (1, 空串)
- AC6 → `command::var_test::var_editor_variable`：（退出码, stdout）二元组等于 (1, 空串)
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::var_test::var_editor_variable)'` `(new)`：覆盖 AC1–AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::var_test::/)'`（共享回归组：`command::var_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-37`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/var.rs`, `tests/command/var_test.rs`, `docs/commands/var.md`, `docs/commands/zh-CN/var.md`, `docs/development/commands/var.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/var.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/var.rs`, `tests/command/var_test.rs`
**Docs and compatibility impact:** `docs/commands/var.md` 与 zh-CN：编辑器变量的优先级、`TERM` 规则与 `VISUAL` 差异；`docs/development/commands/var.md` 同步；`COMPATIBILITY.md` `var` 行（W-1）原地补记；网站页 `var.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=var编辑器变量; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=PM-37,DEP-PM-11; writeset=串行于 PM-37; release=independent; split-from=PM-10; exception=N/A`

---

### Task PM-36: 完善 `libra var` 的 `GIT_SEQUENCE_EDITOR`（#995；自 PM-22 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-22` 拆出：`libra var GIT_SEQUENCE_EDITOR` 复用 `src/command/editor.rs` 的 `resolve_sequence_editor`（`GIT_SEQUENCE_EDITOR` > `sequence.editor` > `GIT_EDITOR` 的解析结果）；全部未配置时与 `GIT_EDITOR` 的回落规则一致。
**Out of scope:** `GIT_EDITOR` 本身（`PM-22`）；未配置且 `TERM=dumb` 的回落（与 `TERM` 未设置同一分支，`PM-22` AC6 已覆盖 `GIT_EDITOR` 的该分支，不单独验收）；`-l` 中的序列编辑器行（`PM-25`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_sequence_editor` 的解析链 | `src/command/editor.rs:103-112` |
| Git：`GIT_SEQUENCE_EDITOR` 环境变量 > `sequence.editor` > 编辑器结果 | `git@6de20f6092:editor.c:48-58`；Git 2.54.0 预探 |

**夹具:** F36：`create_committed_repo_via_cli` 仓库（`tests/command/mod.rs:696`）；`libra config user.name 'C O Mitter'`、`libra config user.email c@d`；环境变量经 `run_libra_command_with_env` 逐子进程传入，未列出的变量为未设置。

**Acceptance criteria:**
- [ ] **AC1** `GIT_SEQUENCE_EDITOR=seq` 且 `sequence.editor=other` 时 `libra var GIT_SEQUENCE_EDITOR` 输出 `seq`。
- [ ] **AC2** 只有 `sequence.editor=other` 时输出 `other`。
- [ ] **AC3** 无序列编辑器配置且 `GIT_EDITOR=nano` 时输出 `nano`。
- [ ] **AC4** 无任何编辑器与序列编辑器配置且 `TERM=xterm` 时 `libra var GIT_SEQUENCE_EDITOR` 输出 `vi`。
- [ ] **AC5** 无任何编辑器与序列编辑器配置且 `TERM` 未设置时 `libra var GIT_SEQUENCE_EDITOR` 的结果（退出码, stdout）为 (1, 空)。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`GIT_SEQUENCE_EDITOR`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::var_test::var_sequence_editor_variable`：exit 0；stdout 逐字等于 `seq\n`
- AC2 → `command::var_test::var_sequence_editor_variable`：exit 0；stdout 逐字等于 `other\n`
- AC3 → `command::var_test::var_sequence_editor_variable`：exit 0；stdout 逐字等于 `nano\n`
- AC4 → `command::var_test::var_sequence_editor_variable`：exit 0；stdout 逐字等于 `vi\n`（Git 2.54.0 预探）
- AC5 → `command::var_test::var_sequence_editor_variable`：（退出码, stdout）二元组等于 (1, 空串)（Git 2.54.0 预探）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::var_test::var_sequence_editor_variable)'` `(new)`：覆盖 AC1–AC5（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::var_test::/)'`（共享回归组：`command::var_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-22`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/var.rs`, `tests/command/var_test.rs`, `docs/commands/var.md`, `docs/commands/zh-CN/var.md`, `docs/development/commands/var.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/var.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/var.rs`, `tests/command/var_test.rs`
**Docs and compatibility impact:** `docs/commands/var.md` 与 zh-CN：`GIT_SEQUENCE_EDITOR` 的优先级；`docs/development/commands/var.md` 同步；`COMPATIBILITY.md` `var` 行（W-1）原地补记；网站页 `var.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=var序列编辑器变量; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=PM-22,DEP-PM-11; writeset=串行于 PM-22; release=independent; split-from=PM-22; exception=N/A`

---

### Task PM-11: 修复 `libra ls-files -o` 排除处于冲突 stage 的索引路径（#1017）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** `src/command/ls_files.rs` 中 `-o`（含 `-i -o`）用于剔除已跟踪路径的集合改为包含索引中**任一 stage** 的路径（Git `index_name_is_other`：精确存在或以未合并状态存在都不是 other），使冲突路径不再被当作未跟踪或已忽略文件列出。
**Out of scope:** 无标志列表的 stage 行（`PM-12`）；`-o` 与 `-u`/`-s`/`-t` 混用时 Libra 的排序与占位格式差异（既有差异，尚未排期，重启条件：后续 ls-files 输出对齐计划）；`--deduplicate`（尚未排期）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `tracked` 集合只取 `index.tracked_entries(0)`（stage 0） | `src/command/ls_files.rs:390-395` |
| `-i -o` 复用同一集合 | `src/command/ls_files.rs:420-424` |
| Git：未合并路径不是 other | `git@6de20f6092:read-cache.c:3455-3471`、`builtin/ls-files.c:166-176`；Git 2.54.0 预探 |

**夹具:** F11：`init_repo_via_cli` 新建仓库并 `configure_identity_via_cli`（`tests/command/mod.rs:636,687`；不使用会额外提交 `tracked.txt` 的 `create_committed_repo_via_cli`）；基线提交写 `f`=`base\n`、`g.log`=`base\n`、`k`=`keep\n`；分支 `side` 把 `f`、`g.log` 改为 `side\n` 并提交；回到 `main` 把二者改为 `main\n` 并提交；`libra merge side` 产生冲突（命令以冲突码失败属预期，夹具随后断言 `libra ls-files -u` 恰列出 `f` 与 `g.log` 各 3 个 stage）；之后在工作区新建**未跟踪**的 `.gitignore`（内容 `*.log\n`）、`u.txt` 与被忽略的 `ign.log`。F11b：同法新建另一仓库，`main` 与 `side` 各自新增内容不同的 `b`（add/add 冲突，stage 2/3），合并后无未跟踪文件。

**Acceptance criteria:**
- [ ] **AC1** F11 上 `libra ls-files -o` 输出恰为 `.gitignore`、`ign.log`、`u.txt` 三行。
- [ ] **AC2** F11 上 `libra ls-files -o --exclude-standard` 输出恰为 `.gitignore`、`u.txt` 两行。
- [ ] **AC3** F11 上 `libra ls-files -i -o --exclude-standard` 输出恰为 `ign.log`（冲突中的 `g.log` 不出现）。
- [ ] **AC4** F11b 上 `libra ls-files -o` 输出为空。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：unmerged。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::ls_files_test::ls_files_others_excludes_unmerged_paths`：exit 0；stdout 逐字等于 `.gitignore\nign.log\nu.txt\n`（Git 2.54.0 预探）
- AC2 → `command::ls_files_test::ls_files_others_excludes_unmerged_paths`：exit 0；stdout 逐字等于 `.gitignore\nu.txt\n`
- AC3 → `command::ls_files_test::ls_files_others_excludes_unmerged_paths`：exit 0；stdout 逐字等于 `ign.log\n`
- AC4 → `command::ls_files_test::ls_files_others_excludes_unmerged_paths`：exit 0；stdout 为空
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::ls_files_test::ls_files_others_excludes_unmerged_paths)'` `(new)`：覆盖 AC1–AC4（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::ls_files_test::/)'`（共享回归组：`command::ls_files_test` 模块族（含 `ls_files_u_shows_unmerged_conflict_entries`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_conflict_status_diff`（共享回归组：整 target，冲突路径在 status/ls-files/diff 间一致性守卫；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 N，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-36`、`DEP-PM-03`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/ls_files.rs`, `tests/command/ls_files_test.rs`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/ls_files.rs`, `tests/command/ls_files_test.rs`
**Docs and compatibility impact:** `docs/commands/ls-files.md` 与 zh-CN：`-o` 不列出冲突路径；`docs/development/commands/ls-files.md` 同步；`COMPATIBILITY.md` `ls-files` 行（W-1，基线第 195 行）原地补记；网站页 `ls-files.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=ls-files -o排除冲突路径; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=4/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=PM-36,DEP-PM-03,DEP-PM-11; writeset=串行于 PM-36; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-12: 修复无标志 `libra ls-files` 与 `-v` 对冲突条目逐 stage 输出（#1022）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** `src/command/ls_files.rs` 的 stage 选择改为：除 `-u` 只列 1/2/3 外，缓存列表（无标志、`-c`、`-z`、`--json`）与 `-v` 都遍历 0/1/2/3 全部 stage，使冲突路径按 Git 每个 stage 输出一行；`-v` 与 `-t` 一致标记 `M`。
**Out of scope:** `-d`/`-m` 的逐 stage 比较、`--error-unmatch <冲突路径>` 只打印一次的 Git 怪癖、`--deduplicate`（尚未排期，重启条件：后续 ls-files 计划）；`-o` 排除（`PM-11`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| stage 选择：`-u` → [1,2,3]；`--stage`/`-s`/`-t` → [0..3]；其余 → [0]（`-v` 即 `show_valid` 不在条件中） | `src/command/ls_files.rs:313-321`；`:104-108` |
| Git：`show_files` 遍历全部缓存条目 | `git@6de20f6092:builtin/ls-files.c:401-480`；Git 2.54.0 预探 |
| `--json` 输出 `FileEntry{path, hash, mode, stage, status}` 数组，与人读输出共用同一条目收集路径 | `src/command/ls_files.rs:144-159,313-331,572-580` |

**夹具:** F12：`init_repo_via_cli` 新建仓库并 `configure_identity_via_cli`（`tests/command/mod.rs:636,687`；不使用会额外提交 `tracked.txt` 的 `create_committed_repo_via_cli`）；基线提交写 `f`=`base\n` 与 `k`=`keep\n`；分支 `side` 把 `f` 改为 `side\n` 并提交；回到 `main` 把 `f` 改为 `main\n` 并提交；`libra merge side` 产生 `f` 的 stage 1/2/3（夹具断言 `libra ls-files -u` 恰 3 行）；无未跟踪文件。F12b：同法新建另一仓库，基线提交只写 `k`，`main` 与 `side` 各自新增内容不同的 `b`（add/add 冲突，stage 2/3）。

**Acceptance criteria:**
- [ ] **AC1** F12 上 `libra ls-files` 输出 `f`、`f`、`f`、`k` 四行。
- [ ] **AC2** F12 上 `libra ls-files -c` 输出与 AC1 相同。
- [ ] **AC3** F12 上 `libra ls-files -z` 输出 `f\0f\0f\0k\0`。
- [ ] **AC4** F12b 上 `libra ls-files` 输出 `b`、`b`、`k` 三行。
- [ ] **AC5** F12 上 `libra ls-files -v` 输出 `M f` 三行与 `H k`。
- [ ] **AC6** F12 上 `libra --json ls-files` 的 `data` 数组中 (`path`, `stage`) 对的多重集合恰为 {(`f`,1), (`f`,2), (`f`,3), (`k`,0)}。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：stage。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::ls_files_test::ls_files_lists_each_unmerged_stage`：exit 0；stdout 逐字等于 `f\nf\nf\nk\n`（Git 2.54.0 预探）
- AC2 → `command::ls_files_test::ls_files_lists_each_unmerged_stage`：exit 0；stdout 逐字等于 `f\nf\nf\nk\n`
- AC3 → `command::ls_files_test::ls_files_lists_each_unmerged_stage`：exit 0；stdout 字节逐字等于 `f\0f\0f\0k\0`
- AC4 → `command::ls_files_test::ls_files_lists_each_unmerged_stage`：exit 0；stdout 逐字等于 `b\nb\nk\n`
- AC5 → `command::ls_files_test::ls_files_lists_each_unmerged_stage`：exit 0；stdout 逐字等于 `M f\nM f\nM f\nH k\n`
- AC6 → `command::ls_files_test::ls_files_lists_each_unmerged_stage`：exit 0（前提）；解析 stdout 的 JSON，`data` 各元素的 (`path`, `stage`) 组成的多重集合逐项等于该集合（与元素次序无关）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::ls_files_test::ls_files_lists_each_unmerged_stage)'` `(new)`：覆盖 AC1–AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::ls_files_test::/)'`（共享回归组：`command::ls_files_test` 模块族（含 `-s`/`-u`/`-t` 既有用例）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_conflict_status_diff`（共享回归组：整 target，冲突路径一致性守卫；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 N，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-11`、`DEP-PM-03`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/ls_files.rs`, `tests/command/ls_files_test.rs`, `docs/commands/ls-files.md`, `docs/commands/zh-CN/ls-files.md`, `docs/development/commands/ls-files.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/ls-files.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/ls_files.rs`, `tests/command/ls_files_test.rs`
**Docs and compatibility impact:** `docs/commands/ls-files.md` 与 zh-CN：冲突条目逐 stage 输出与 `-v` 行为；`docs/development/commands/ls-files.md` 同步；`COMPATIBILITY.md` `ls-files` 行（W-1，基线第 195 行）原地修正「`-v` 等于 `-t`」等表述；网站页 `ls-files.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=ls-files缓存列表逐stage输出冲突条目; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=PM-11,DEP-PM-03,DEP-PM-11; writeset=串行于 PM-11; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-15: 属性引擎支持 `[attr]` 宏、内置 `binary` 与 Git 优先级求值（#1033；拆出 PM-27、PM-28）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git `attr.c`（`fill`/`fill_one`/`determine_macros`）改写 `src/utils/attributes.rs` 的求值：解析 `[attr]<name> <assignments>` 宏定义（只在根 `.gitattributes`/`.libra_attributes`、`info/attributes`、`core.attributesFile` 中生效，子目录文件中的宏定义被忽略；Git 另向 stderr 告警，Libra 是否告警不作承诺、不单独验收）；内置宏 `[attr]binary -diff -merge -text` 位于最低优先级；同名宏以最高优先级来源中的定义为准（`info/attributes` > 根文件 > `core.attributesFile` > 内置，同文件内后者为准，Git `determine_macros`）；求值从最高优先级来源、文件内末行到首行进行，每个属性「先赋值者胜」，宏只在被设为 true 时展开（实现以显式工作栈展开，不依赖调用栈深度），`!attr` 占位并阻断低优先级赋值；`src/utils/attributes.rs` 的全部求值入口（`attribute_state_for_path`、`all_attribute_states_for_path_from`、`try_all_attribute_states_beneath`、`attribute_states_beneath_with_context`，`:273,312,440,514`）改为调用同一个新的求值函数，`check-attr` 及全部共享调用方（merge、diff、lfs、status、conversion、snapshot）因此一致地看到宏展开结果。`-a` 输出次序保持字母序（`PM-28` 改为 Git 登记序）。
**Out of scope:** 属性名合法性校验（`PM-27`）；`check-attr` 之外调用方（conversion、snapshot 等）的宏效果（经同一求值函数，入口统一由 ER-05 代码评审核对，并由下列共享调用方回归组守护其既有行为，不单独验收）；不单独验收的同路径情形：用户以 `[attr]binary` 重定义内置宏（与 AC2 同一「最高优先级定义为准」规则）、同一文件内同名宏的多个定义（`determine_macros` 在同一来源内同样自末行向首行取首个定义，与 AC2 同一遍历）、宏被设为 unset/unspecified（`-mybin`、`!mybin`）时不展开（与 AC1–AC5 同一「只在 set 时展开」条件，`!mybin` 的阻断由 AC6 验收）；`-a` 的 Git 登记次序（`PM-28`）；XDG/HOME 全局属性文件回退与 `core.attributesFile` 键名大小写（`PM-18`）；系统属性文件（`DEFER-PM-06`）；`archive` 的独立 `tree_attributes.rs`（不使用本引擎，保持）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 唯一解析器无 `[attr]` 处理，`[attr]mybin` 被编译为 glob | `src/utils/attributes.rs:1061-1086` |
| 求值为来源正向遍历、末次赋值胜 | `src/utils/attributes.rs:273-295,312-381,412-435,514-593` |
| 调用方：merge、diff/funcname、lfs、snapshot、conversion/status | `src/command/merge.rs:5849,6175`；`src/command/diff/compare.rs:523,529`；`src/utils/lfs.rs:45`；`src/internal/operation/snapshot.rs:991,1015`；`src/utils/worktree_conversion.rs:196,389`；`src/command/status_io_worker.rs:255` |
| Git 求值与宏展开、`[attr]` 仅限宏允许的来源、内置 `binary` | `git@6de20f6092:attr.c:638-641,910-948,1092-1158`；Git 2.54.0 预探 |
| Git `determine_macros` 自栈顶（最高优先级）向下取每个宏的首个定义 | `git@6de20f6092:attr.c`（`determine_macros`）；Git 2.54.0 预探：同夹具 `git check-attr foo bar -- z.p` 输出 `foo: info`、`bar: g` |
| 属性求值入口：`attribute_state_for_path`、`all_attribute_states_for_path_from`、`try_all_attribute_states_beneath` 与带上下文的 `attribute_states_beneath_with_context`（后者有独立的规则遍历循环） | `src/utils/attributes.rs:273,312,440,514-593` |

**夹具:** F15：仓库根 `.gitattributes` 依次为 `[attr]mybin -diff -text`、`*.dat mybin`、`[attr]outer inner -merge`、`[attr]inner foo=bar`、`*.n outer`、`*.b binary`、`*.b2 mybin`、`*.b2 diff`、`*.e mybin`、`*.e !mybin`；`sub/.gitattributes` 为 `[attr]subm foo` 与 `*.s subm`；F15c：只含 `file a0` 及 `[attr]a0 a1` … `[attr]a2999 a3000`、`[attr]a3000 -text` 的 `.gitattributes`（3001 层宏链）。查询路径无需真实存在。 F15p（AC2）：另一仓库，以 `libra config core.attributesFile <仓库外绝对路径 G>` 配置全局属性文件 G = `[attr]prio foo=global\n[attr]gm bar=g\n`，根 `.gitattributes` = `[attr]prio foo=root\n*.p prio gm\n`，`.libra/info/attributes` = `[attr]prio foo=info\n`。

**Acceptance criteria:**
- [ ] **AC1** `libra check-attr diff x.dat` 输出 `x.dat: diff: unset`。
- [ ] **AC2** F15p 上 `libra check-attr foo bar -- z.p` 输出 `z.p: foo: info` 与 `z.p: bar: g` 两行（同名宏取 `info/attributes` 的定义；只在 `core.attributesFile` 中定义的宏可用）。
- [ ] **AC3** `libra check-attr -a f.b` 输出内置 `binary` 展开的四行。
- [ ] **AC4** `libra check-attr -a y.n`（宏引用宏）输出 `foo: bar`、`inner: set`、`merge: unset`、`outer: set` 四行。
- [ ] **AC5** `libra check-attr -a x.b2`（宏之后的显式 `diff`）输出 `diff: set`、`mybin: set`、`text: unset` 三行。
- [ ] **AC6** `libra check-attr -a x.e`（`!mybin` 阻断此前的 `mybin`）输出为空。
- [ ] **AC7** `libra check-attr foo sub/x.s`（子目录文件中的宏定义不生效）输出 `sub/x.s: foo: unspecified`。
- [ ] **AC8** F15c 上 `libra check-attr text file` 输出 `file: text: unset`（`t0003#55` 的 3001 层宏链）。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`[attr]`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::check_attr_test::check_attr_expands_macros_with_git_precedence`：exit 0；stdout 逐字等于 `x.dat: diff: unset\n`
- AC2 → `command::check_attr_test::check_attr_expands_macros_with_git_precedence`：exit 0；stdout 逐字等于 `z.p: foo: info\nz.p: bar: g\n`（Git 2.54.0 预探；只认根文件宏的实现输出 `foo: root` 与 `bar: unspecified`，先定义者胜的实现输出 `foo: global`）
- AC3 → `command::check_attr_test::check_attr_expands_macros_with_git_precedence`：exit 0；stdout 逐字等于 `f.b: binary: set\nf.b: diff: unset\nf.b: merge: unset\nf.b: text: unset\n`
- AC4 → `command::check_attr_test::check_attr_expands_macros_with_git_precedence`：exit 0；stdout 逐字等于 `y.n: foo: bar\ny.n: inner: set\ny.n: merge: unset\ny.n: outer: set\n`
- AC5 → `command::check_attr_test::check_attr_expands_macros_with_git_precedence`：exit 0；stdout 逐字等于 `x.b2: diff: set\nx.b2: mybin: set\nx.b2: text: unset\n`
- AC6 → `command::check_attr_test::check_attr_expands_macros_with_git_precedence`：exit 0；stdout 为空（Git 2.54.0 预探）
- AC7 → `command::check_attr_test::check_attr_expands_macros_with_git_precedence`：exit 0；stdout 逐字等于 `sub/x.s: foo: unspecified\n`
- AC8 → `command::check_attr_test::check_attr_expands_macros_with_git_precedence`：exit 0；stdout 逐字等于 `file: text: unset\n`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::check_attr_test::check_attr_expands_macros_with_git_precedence)'` `(new)`：覆盖 AC1–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::check_attr_test::/)'`（共享回归组：`command::check_attr_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^utils::attributes::/)'`（共享回归组：`src/utils/attributes.rs` 单元测试（beneath cache）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::(merge_test|diff_test|lfs_test|status_test)::/)'`（共享回归组：共享属性引擎的 merge/diff/lfs/status 调用方回归；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: `src/utils/attributes.rs` 为 merge/diff/lfs/status 共享的单一事实源（GC-02，ER-13 T-1）；另含版本面 `install.sh`/`install.ps1`
**Dependencies:** `PM-12`、`DEP-PM-09`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/attributes.rs`, `tests/command/check_attr_test.rs`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/utils/attributes.rs`, `tests/command/check_attr_test.rs`
**Docs and compatibility impact:** `docs/commands/check-attr.md` 与 zh-CN：宏定义、内置 `binary`、`!` 语义、宏只在根/info/全局来源生效，并删去「宏未提供」及「`--cached`/`--source` 未提供」的陈旧句（`:105-106` 与 zh `:85`）；`docs/development/commands/check-attr.md` 同步（`:12`、`:49`）；`COMPATIBILITY.md` `check-attr` 行（W-1，基线第 262 行）原地改写并消除自相矛盾；网站页 `check-attr.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=属性引擎宏展开与Git优先级求值; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=PM-12,DEP-PM-09,DEP-PM-11; writeset=串行于 PM-12; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-27: `libra check-attr` 属性名合法性校验与 `--all` 冲突检查（#1033；自 PM-15 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-15` 拆出：`src/command/check_attr.rs` 在读取任何路径或 stdin 之前校验每个属性名符合 Git `attr_name_valid`（非空、只含 `[-A-Za-z0-9_.]`、不以 `-` 开头），不合法以 `LBR-CLI-002` 失败；`--all` 与 `--` 之前的属性名同用（Git：`Attributes and --all both specified`）同样以 `LBR-CLI-002` 失败。
**Out of scope:** 宏求值（`PM-15`）；以 `-` 开头的属性名（在命令行上先被 clap 当作未知选项、以同一稳定码 `LBR-CLI-002` 拒绝，Git 同样无法经命令行把它作为属性名传入，不单独验收）；Git 的退出码 255（有意差异：Libra 用法错误为 `LBR-CLI-002`）；子目录下相对路径解析（既有差异，尚未排期，重启条件：后续 check-attr 计划）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 只拒绝空属性列表或空路径，不校验名字；`-foo` 形式的参数被 clap 当作未知选项（`unexpected argument`，`LBR-CLI-002`） | `src/command/check_attr.rs:50-56,147-152` |
| `--all` 时所有位置参数都当作路径 | `src/command/check_attr.rs:118-136` |
| Git：`error: <name>: not a valid attribute name`（255）；`--all` 与属性同用 129 | `git@6de20f6092:builtin/check-attr.c`；Git 2.54.0 预探 |
| 现行实现先 `read_stdin_paths` 读取 stdin，之后才检查属性列表与路径列表是否为空（尚无属性名合法性校验），未关闭的 stdin 会使其阻塞 | `src/command/check_attr.rs:95-165` |

**夹具:** F27：仓库根 `.gitattributes` 依次为 `[attr]mybin -diff -text`、`*.dat mybin`、`[attr]outer inner -merge`、`[attr]inner foo=bar`、`*.n outer`、`*.b binary`、`*.b2 mybin`、`*.b2 diff`、`*.e mybin`、`*.e !mybin`；`sub/.gitattributes` 为 `[attr]subm foo` 与 `*.s subm`；F27c：只含 `file a0` 及 `[attr]a0 a1` … `[attr]a2999 a3000`、`[attr]a3000 -text` 的 `.gitattributes`（3001 层宏链）。查询路径无需真实存在。

**Acceptance criteria:**
- [ ] **AC1** `libra check-attr '' x` 以 `LBR-CLI-002` 失败。
- [ ] **AC2** `libra check-attr '' -- x` 以 `LBR-CLI-002` 失败。
- [ ] **AC3** stdin 保持打开（不写入、不关闭）时 `libra check-attr --stdin ''` 在 10 秒内以 `LBR-CLI-002` 失败（校验先于读取 stdin）。
- [ ] **AC4** `libra check-attr 'a b' -- x`（含空格）以 `LBR-CLI-002` 失败。
- [ ] **AC5** `libra check-attr 'a!' x`（含非法字符 `!`）以 `LBR-CLI-002` 失败。
- [ ] **AC6** `libra check-attr --all diff -- x.dat` 以 `LBR-CLI-002` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：not a valid attribute name。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::check_attr_test::check_attr_rejects_invalid_attribute_names`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC2 → `command::check_attr_test::check_attr_rejects_invalid_attribute_names`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC3 → `command::check_attr_test::check_attr_rejects_invalid_attribute_names`：以 `tests/command/mod.rs` 的 `base_libra_command` 启动子进程、stdin 为管道且保持打开；进程在 10 秒内退出（前提，超时即判失败并终止子进程）；退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC4 → `command::check_attr_test::check_attr_rejects_invalid_attribute_names`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC5 → `command::check_attr_test::check_attr_rejects_invalid_attribute_names`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC6 → `command::check_attr_test::check_attr_rejects_invalid_attribute_names`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::check_attr_test::check_attr_rejects_invalid_attribute_names)'` `(new)`：覆盖 AC1–AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::check_attr_test::/)'`（共享回归组：`command::check_attr_test` 模块族（含 `missing_attribute_is_a_usage_error`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-15`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/check_attr.rs`, `tests/command/check_attr_test.rs`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/check_attr.rs`, `tests/command/check_attr_test.rs`
**Docs and compatibility impact:** `docs/commands/check-attr.md` 与 zh-CN：属性名规则与 `--all` 冲突；`docs/development/commands/check-attr.md` 同步；`COMPATIBILITY.md` `check-attr` 行（W-1，基线第 262 行）原地补记；网站页 `check-attr.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=check-attr属性名校验; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=PM-15,DEP-PM-11; writeset=串行于 PM-15; release=independent; split-from=PM-15; exception=N/A`

---

### Task PM-28: `libra check-attr -a` 按 Git 属性登记次序输出（#1033；自 PM-15 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-15` 拆出：`-a` 的输出次序由字母序改为 Git 全局属性字典的登记次序——内置 `binary`、`diff`、`merge`、`text` 最先，其余按 Git 读取属性文件的先后及文件内出现次序首次出现时登记——全局（`core.attributesFile`）→ 根目录（`.gitattributes` 后 `.libra_attributes`）→ `info/attributes` → 查询路径上自浅至深的子目录文件；该读取次序不同于 `PM-15` 的匹配优先级栈（`info/attributes` 匹配优先级最高，却先于子目录文件登记）；`src/utils/attributes.rs` 的全量结果改用保序映射。
**Out of scope:** 单属性查询输出（次序不受影响）；同一次调用查询多个路径时登记表在路径间的累计（沿用 Git 的进程级字典语义，不单独验收）；宏求值（`PM-15`）；全局来源先于根目录登记、根目录内 `.gitattributes` 先于 `.libra_attributes` 登记（与 AC4 同一按读取次序登记的实现，不单独验收）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--all` 结果为 `BTreeMap`（字母序） | `src/command/check_attr.rs:196-207`；`src/utils/attributes.rs:412-435` |
| 既有用例钉住字母序 | `tests/command/check_attr_test.rs`（`all_reports_only_set_attributes`） |
| Git：`-a` 按属性字典登记次序；`t0003#55` 期望 `text: unset` 后接 `a0`…`a3000` | `git@6de20f6092:attr.c:~1356`、`t/t0003-attributes.sh`；Git 2.54.0 预探 |
| Git 读取次序：内置 → 系统 → 全局 → 根 → info，随后逐级子目录，最后把 info 帧推回栈顶；属性编号按首次登记分配 | `git@6de20f6092:attr.c:233-253,912-943,1005-1041` |
| Libra 来源栈（匹配优先级自低到高）：全局 → 各级目录 → `info/attributes` | `src/utils/attributes.rs:974-988`；`src/utils/util.rs:314-324` |

**夹具:** F28：仓库根 `.gitattributes` 依次为 `[attr]mybin -diff -text`、`*.dat mybin`、`[attr]outer inner -merge`、`[attr]inner foo=bar`、`*.n outer`、`*.b binary`、`*.b2 mybin`、`*.b2 diff`、`*.e mybin`、`*.e !mybin`；`sub/.gitattributes` 为 `[attr]subm foo` 与 `*.s subm`；F28c：只含 `file a0` 及 `[attr]a0 a1` … `[attr]a2999 a3000`、`[attr]a3000 -text` 的 `.gitattributes`（3001 层宏链）。查询路径无需真实存在。 F28l：另一仓库，`.libra_attributes` 内容 `*.bin filter=lfs diff=lfs merge=lfs -text`（同 `all_reports_only_set_attributes` 夹具）。 F28i：另一仓库，根 `.gitattributes` 为 `*.s rootattr`，`.libra/info/attributes` 为 `*.s infoattr`，`sub/.gitattributes` 为 `*.s subattr`。

**Acceptance criteria:**
- [ ] **AC1** F28 上 `libra check-attr -a x.dat` 输出次序为 `diff`、`text`、`mybin`。
- [ ] **AC2** F28c 上 `libra check-attr -a file` 首行为 `file: text: unset`，其后依次为 `file: a0: set` 至 `file: a3000: set`。
- [ ] **AC3** F28l 上 `libra check-attr -a a.bin` 输出次序为 `diff`、`merge`、`text`、`filter`。
- [ ] **AC4** F28i 上 `libra check-attr -a sub/x.s` 按读取次序输出 `rootattr`、`infoattr`、`subattr`。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--all`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::check_attr_test::check_attr_all_uses_registration_order`：exit 0；stdout 逐字等于 `x.dat: diff: unset\nx.dat: text: unset\nx.dat: mybin: set\n`（Git 2.54.0 预探）
- AC2 → `command::check_attr_test::check_attr_all_uses_registration_order`：exit 0；stdout 逐字等于该 3002 行期望
- AC3 → `command::check_attr_test::check_attr_all_uses_registration_order`：exit 0；stdout 逐字等于 `a.bin: diff: lfs\na.bin: merge: lfs\na.bin: text: unset\na.bin: filter: lfs\n`（同内容写入 `.gitattributes` 时 Git 2.54.0 预探的次序）
- AC4 → `command::check_attr_test::check_attr_all_uses_registration_order`：exit 0；stdout 逐字等于 `sub/x.s: rootattr: set\nsub/x.s: infoattr: set\nsub/x.s: subattr: set\n`（Git 2.54.0 预探，info 文件位于 `.git/info/attributes`；按匹配优先级登记的错误实现会输出 `rootattr`、`subattr`、`infoattr`）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::check_attr_test::check_attr_all_uses_registration_order)'` `(new)`：覆盖 AC1–AC4（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::check_attr_test::/)'`（共享回归组：`command::check_attr_test` 模块族（`all_reports_only_set_attributes` 与 `PM-15` 的 `-a` 断言按本卡被取代的次序更新）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^utils::attributes::/)'`（共享回归组：`src/utils/attributes.rs` 单元测试；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: `src/utils/attributes.rs` 共享单一事实源（ER-13 T-1）；另含版本面 `install.sh`/`install.ps1`
**Dependencies:** `PM-27`、`DEP-PM-09`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/attributes.rs`, `src/command/check_attr.rs`, `tests/command/check_attr_test.rs`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/utils/attributes.rs`, `src/command/check_attr.rs`, `tests/command/check_attr_test.rs`
**Docs and compatibility impact:** `docs/commands/check-attr.md` 与 zh-CN：`-a` 输出次序；`docs/development/commands/check-attr.md` 同步；`COMPATIBILITY.md` `check-attr` 行（W-1，基线第 262 行）原地补记；网站页 `check-attr.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=check-attr -a登记次序输出; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=4/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=PM-27,DEP-PM-09,DEP-PM-11; writeset=串行于 PM-27; release=independent; split-from=PM-15; exception=N/A`

---

### Task PM-18: 全局属性与忽略来源：`core.attributesFile`/`core.excludesFile` 键名大小写与 XDG 默认文件（#1033、#1035）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git 统一全局来源解析：在 `src/utils/util.rs` 增加单一 helper 解析 `xdg_config_home(<file>)`（`XDG_CONFIG_HOME` 非空时用 `$XDG_CONFIG_HOME/git/<file>`，否则 `$HOME/.config/git/<file>`，二者皆无则无全局文件）；`optional_cascaded_config_path` 对变量名做大小写不敏感匹配（`core.attributesfile`/`core.excludesfile` 等拼写生效）；`core.attributesFile` 未配置时属性引擎回退读取 `xdg_config_home("attributes")`，`core.excludesFile` 未配置时忽略引擎回退读取 `xdg_config_home("ignore")`；显式配置时不再读取 XDG 默认文件。
**Out of scope:** 系统属性文件与 `GIT_ATTR_NOSYSTEM`（`DEFER-PM-06`）；宏求值（`PM-15`）；忽略引擎在 `XDG_CONFIG_HOME` 为空时的 HOME 回退（与属性引擎共用本卡新增的同一 `xdg_config_home` helper：其 HOME 分支由 AC2 验收，忽略引擎调用该 helper 由 AC6 验收，不单独验收）；`XDG_CONFIG_HOME` 与 `HOME` 皆无时不读取全局文件（测试 helper 总为子进程设置 HOME，不单独验收）；`check-ignore` 子目录路径、`--stdin`、`--quiet`（`PM-17`、`PM-31`、`PM-20`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 键名精确匹配（`Key.eq(key)`）；调用方以 `core.attributesFile`、`core.excludesFile` 查询 | `src/internal/config.rs:166-177`；`src/utils/attributes.rs:22`；`src/utils/util.rs:61` |
| 已有按变量名大小写不敏感的 helper | `src/internal/config.rs:200-215` |
| `optional_cascaded_config_path` 与 `expand_config_path`（无 XDG 回退） | `src/utils/util.rs:1484-1539` |
| 属性来源与忽略来源组装处 | `src/utils/attributes.rs:974-988`；`src/utils/util.rs:3491-3516` |
| Git `xdg_config_home_for` 与「显式配置时不读 XDG」 | `git@6de20f6092:path.c:1545-1560`、`attr.c:882-889`；Git 2.54.0 预探 |

**夹具:** F18：无 `.gitattributes` 的仓库，HOME 与 `XDG_CONFIG_HOME` 经 `run_libra_command_with_env` 指向仓库目录之外、先经 `canonicalize` 的测试临时目录（绝对路径，期望中的 `<XDG 绝对路径>` 即传入的同一字符串；`XDG_CONFIG_HOME=""` 表示未设置）；`$XDG/git/attributes`=`f attr_f`、`$HOME/.config/git/attributes`=`f attr_f=home`、`$HOME/my_attrs`=`f attr_f=test`；`$XDG/git/ignore`=`xdgignored`、仓库根 `global-excludes`=`globalone`。 AC1 与 AC5 在同一仓库依次执行（AC5 前不配置 `core.attributesFile`，此时 AC1 已断言全局规则生效）；AC2–AC4 与 AC6–AC8 各用独立新建的仓库；AC8 的仓库以 `libra config core.excludesFile global-excludes` 配置（该文件只含 `globalone`，不含 `xdgignored`）。

**Acceptance criteria:**
- [ ] **AC1** 只有 `$XDG/git/attributes` 时 `libra check-attr -a f` 输出 `f: attr_f: set`。
- [ ] **AC2** `XDG_CONFIG_HOME=""` 时读取 `$HOME/.config/git/attributes`：输出 `f: attr_f: home`。
- [ ] **AC3** `libra config core.attributesfile ~/my_attrs`（小写键名）后输出 `f: attr_f: test`。
- [ ] **AC4** `core.attributesFile` 指向不存在文件且 `$XDG/git/attributes` 仍在时输出为空。
- [ ] **AC5** 在 AC1 的仓库与状态下（`$XDG/git/attributes` 生效、未配置 `core.attributesFile`）写入仓库 `.gitattributes`=`f -attr_f` 后，`libra check-attr -a f` 输出 `f: attr_f: unset`（本地覆盖全局）。
- [ ] **AC6** 只有 `$XDG/git/ignore` 时 `libra check-ignore -v xdgignored` 输出 `<XDG 绝对路径>/git/ignore:1:xdgignored\txdgignored\n`。
- [ ] **AC7** `libra config core.excludesfile global-excludes`（小写键名）后 `libra check-ignore -v globalone` 输出 `global-excludes:1:globalone\tglobalone`。
- [ ] **AC8** `core.excludesFile` 已配置时 `$XDG/git/ignore` 不生效：`libra check-ignore xdgignored` 的结果（退出码, stdout）为 (1, 空)。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`XDG_CONFIG_HOME`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::check_attr_test::check_attr_global_sources_follow_git`：exit 0；stdout 逐字等于 `f: attr_f: set\n`
- AC2 → `command::check_attr_test::check_attr_global_sources_follow_git`：exit 0；stdout 逐字等于 `f: attr_f: home\n`
- AC3 → `command::check_attr_test::check_attr_global_sources_follow_git`：exit 0；stdout 逐字等于 `f: attr_f: test\n`
- AC4 → `command::check_attr_test::check_attr_global_sources_follow_git`：exit 0；stdout 为空（Git 2.54.0 预探：显式配置时不读 XDG）
- AC5 → `command::check_attr_test::check_attr_global_sources_follow_git`：exit 0；stdout 逐字等于 `f: attr_f: unset\n`
- AC6 → `command::check_ignore_test::check_ignore_global_sources_follow_git`：exit 0；stdout 逐字等于 `<XDG 绝对路径>/git/ignore:1:xdgignored\txdgignored\n`
- AC7 → `command::check_ignore_test::check_ignore_global_sources_follow_git`：exit 0；stdout 逐字等于 `global-excludes:1:globalone\tglobalone\n`
- AC8 → `command::check_ignore_test::check_ignore_global_sources_follow_git`：（退出码, stdout）二元组等于 (1, 空串)
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::check_attr_test::check_attr_global_sources_follow_git)'` `(new)`：覆盖 AC1–AC5（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::check_ignore_test::check_ignore_global_sources_follow_git)'` `(new)`：覆盖 AC6–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::check_attr_test::/)'`（共享回归组：`command::check_attr_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::check_ignore_test::/)'`（共享回归组：`command::check_ignore_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: `src/utils/util.rs` 的 `optional_cascaded_config_path` 与新增 XDG helper 为共享单一事实源（ER-13 T-1）；另含版本面 `install.sh`/`install.ps1`
**Dependencies:** `PM-28`、`DEP-PM-09`、`DEP-PM-11`、`DEP-PM-13`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/util.rs`, `src/utils/attributes.rs`, `tests/command/check_attr_test.rs`, `tests/command/check_ignore_test.rs`, `docs/commands/check-attr.md`, `docs/commands/zh-CN/check-attr.md`, `docs/development/commands/check-attr.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-attr.en.md`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/utils/util.rs`, `src/utils/attributes.rs`, `tests/command/check_attr_test.rs`, `tests/command/check_ignore_test.rs`
**Docs and compatibility impact:** `docs/commands/check-attr.md`、`docs/commands/check-ignore.md` 及其 zh-CN：全局来源的 XDG/HOME 回退、显式配置优先、键名大小写不敏感；对应 `docs/development/commands/*.md` 同步；`COMPATIBILITY.md` 的 `check-attr`（基线第 262 行）与 `check-ignore`（基线第 264 行）两行（W-1）原地改写（修正 `core.excludesFile` 只认驼峰拼写的事实）；网站页 `check-attr.en.md`、`check-ignore.en.md` 按「文档与网站验收命令」对本卡命令集合 {`check-attr`、`check-ignore`} 逐页同步（第 3 步 `--only check-attr check-ignore`）；文档关键字在两个命令的 EN/zh 页都检查。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=全局属性与忽略来源解析; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=2; scope=M; deps=PM-28,DEP-PM-09,DEP-PM-11,DEP-PM-13; writeset=串行于 PM-28; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-25: 完善 `libra var` 的 `GIT_PAGER`、`GIT_DEFAULT_BRANCH`、`GIT_ATTR_*` 与 `-l` 全表（#995；自 PM-22 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-22` 拆出：`GIT_PAGER` 报告 Libra 启用分页时会启动的分页命令（与 Git `var` 以 `git_pager(…, 1)` 假定输出到终端一致，不判断当前 stdout 是否为终端、`LIBRA_TEST` 或启动失败回退；`src/utils/pager.rs`：`LIBRA_PAGER=never` 时 `cat`，否则 `less -R -F`；不读取 `GIT_PAGER`/`core.pager`/`PAGER`，有意差异，`DEFER-PM-05`）；`GIT_DEFAULT_BRANCH` 报告 `libra init` 实际采用的值（`init.defaultBranch`，否则 Libra 默认 `main`，有意差异）；`GIT_ATTR_GLOBAL` 复用 `PM-18` 的全局属性来源解析（`core.attributesFile` 展开后的路径，否则 XDG/HOME 默认路径，皆无则退出 1）；`GIT_ATTR_SYSTEM` 因 Libra 无系统属性文件恒以退出码 1 结束且 stdout 为空（stderr 不作承诺，`DEFER-PM-06`）；`-l` 按 Git 变量表次序输出全部可解析变量。
**Out of scope:** `GIT_SHELL_PATH`、`GIT_CONFIG_SYSTEM`/`GIT_CONFIG_GLOBAL`（尚未排期，重启条件：#995 后续需要）；Libra 分页器遵循 Git 分页器变量（`DEFER-PM-05`）；`GIT_ATTR_GLOBAL` 在既无 `core.attributesFile` 又无 HOME/`XDG_CONFIG_HOME` 时以退出码 1 结束（测试 helper 总为子进程设置 HOME，与 AC7 同一「无值即退出 1、stdout 为空」输出路径，不单独验收）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 分页器只读 `LIBRA_PAGER`，命令固定 `less -R -F` | `src/utils/pager.rs:19,30-39,95-107` |
| `init` 默认分支：`--initial-branch` > `init.defaultBranch` > `main` | `src/command/init.rs:37,609-638` |
| 全局属性来源解析（`PM-18` 交付） | `src/utils/attributes.rs:974-988`；`src/utils/util.rs:1484-1539` |
| Git：`GIT_PAGER` 默认 `less`、`GIT_DEFAULT_BRANCH` 默认 `master`、`GIT_ATTR_SYSTEM` 在 `GIT_ATTR_NOSYSTEM` 时退出 1；`-l` 按变量表次序 | `git@6de20f6092:builtin/var.c:44-51,123-173`、`refs.c:712-741`、`attr.c:874-894`；Git 2.54.0 预探 |
| 分页器自动模式只在 stdout 为终端且未设 `LIBRA_TEST` 时启动，启动失败回退 stdout | `src/utils/pager.rs:57-71,182-186` |

**夹具:** F25：`create_committed_repo_via_cli` 仓库（`tests/command/mod.rs:696`）；`libra config user.name 'C O Mitter'`、`libra config user.email c@d`；HOME 与 `XDG_CONFIG_HOME` 指向仓库目录之外的测试临时目录（绝对路径），`TERM`、`LIBRA_PAGER`、`GIT_PAGER`、`PAGER` 经 `run_libra_command_with_env` 逐子进程传入，未列出的变量为未设置；AC8 的 `-l` 调用与 7 次单独查询的全部子进程另设 `GIT_AUTHOR_DATE='1700000000 +0000'` 与 `GIT_COMMITTER_DATE='1700000000 +0000'`，使身份行不随调用时刻变化。

**Acceptance criteria:**
- [ ] **AC1** 设置 `GIT_PAGER=most`、`PAGER=more` 时 `libra var GIT_PAGER` 仍输出 `less -R -F`。
- [ ] **AC2** `LIBRA_PAGER=never` 时 `libra var GIT_PAGER` 输出 `cat`。
- [ ] **AC3** 未配置 `init.defaultBranch` 时 `libra var GIT_DEFAULT_BRANCH` 输出 `main`。
- [ ] **AC4** `libra config init.defaultBranch trunk` 后输出 `trunk`。
- [ ] **AC5** `core.attributesFile=~/my_attrs` 时 `libra var GIT_ATTR_GLOBAL` 输出 `<HOME>/my_attrs`。
- [ ] **AC6** 无 `core.attributesFile` 且 `XDG_CONFIG_HOME=<X>` 时输出 `<X>/git/attributes`。
- [ ] **AC7** `libra var GIT_ATTR_SYSTEM` 的结果（退出码, stdout）为 (1, 空)。
- [ ] **AC8** `TERM=xterm`、作者与提交者日期固定、无 `core.attributesFile`、`XDG_CONFIG_HOME=<X>` 时 `libra var -l` 依次输出 `GIT_COMMITTER_IDENT`、`GIT_AUTHOR_IDENT`、`GIT_EDITOR`、`GIT_SEQUENCE_EDITOR`、`GIT_PAGER`、`GIT_DEFAULT_BRANCH`、`GIT_ATTR_GLOBAL` 七行（不含 `GIT_ATTR_SYSTEM`）。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`GIT_DEFAULT_BRANCH`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::var_test::var_environment_variables_and_full_list`：exit 0；stdout 逐字等于 `less -R -F\n`
- AC2 → `command::var_test::var_environment_variables_and_full_list`：exit 0；stdout 逐字等于 `cat\n`
- AC3 → `command::var_test::var_environment_variables_and_full_list`：exit 0；stdout 逐字等于 `main\n`
- AC4 → `command::var_test::var_environment_variables_and_full_list`：exit 0；stdout 逐字等于 `trunk\n`
- AC5 → `command::var_test::var_environment_variables_and_full_list`：exit 0；stdout 逐字等于 `<HOME>/my_attrs\n`
- AC6 → `command::var_test::var_environment_variables_and_full_list`：exit 0；stdout 逐字等于 `<X>/git/attributes\n`
- AC7 → `command::var_test::var_environment_variables_and_full_list`：（退出码, stdout）二元组等于 (1, 空串)
- AC8 → `command::var_test::var_environment_variables_and_full_list`：exit 0；stdout 恰 7 行，各行以 `<NAME>=` 开头且次序如上，取值分别与单独查询该变量的 stdout（去换行）逐字相等
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::var_test::var_environment_variables_and_full_list)'` `(new)`：覆盖 AC1–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::var_test::/)'`（共享回归组：`command::var_test` 模块族（`PM-10` 的 `-l` 两行断言按本卡被取代的全表更新）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-18`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/var.rs`, `tests/command/var_test.rs`, `docs/commands/var.md`, `docs/commands/zh-CN/var.md`, `docs/development/commands/var.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/var.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/var.rs`, `tests/command/var_test.rs`
**Docs and compatibility impact:** `docs/commands/var.md` 与 zh-CN：分页器/默认分支/属性路径变量、`-l` 全表与有意差异；`docs/development/commands/var.md` 同步；`COMPATIBILITY.md` `var` 行（W-1）原地补记；网站页 `var.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=var环境与路径类变量及-l全表; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=PM-18,DEP-PM-11; writeset=串行于 PM-18; release=independent; split-from=PM-22; exception=N/A`

---

### Task PM-16: `libra shortlog` 人读输出对齐 Git 格式与排序（#1034；拆出 PM-30、PM-19）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 ADR-PM-04 把 `src/command/shortlog.rs` 的人读输出（含从 stdin 读取 `git log` 输出的模式）改为 Git `shortlog.c` 格式：`-s` 为 `%6d\t<name>`；非 `-s` 为 `<name> (<n>):` 段头、每条主题前 6 个空格、主题按插入的逆序（仓库遍历即由旧到新）、每段后空一行；人读输出的分组按完整键（含 `-e` 的邮箱）以字节序（`strcmp`）排序，`-n` 为按计数降序的稳定排序。人读输出先按上述新规则排序，再依次应用 `--min-count`、`--reverse`、`--top`（次序同 `src/command/shortlog.rs:584-595`）；`--json` 路径保留改动前的收尾实现（同一小写名排序与 `--min-count` → `--reverse` → `--top` 处理，字段不变），即人读与 JSON 各走一条收尾路径；AC6 以同时带这三个选项、且新旧排序在夹具上给出不同结果的 JSON 调用与改动前基线逐字比较。（按 G-09 拆出 `PM-30` 承接多分组与去重，`PM-19` 承接 `-<n>`/`--max-count`（其再拆出 `PM-40` 承接 mailmap）。）
**Out of scope:** 分组选项（`PM-30`、`PM-35`）；提交数限制（`PM-19`）与 mailmap（`PM-40`）；`-w` 的换行宽度与缩进规则（不变；既有 `shortlog_wrap_wraps_long_subjects` 只断言宽度与缩进，不改并由回归组守护）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 段头渲染为 `"{:>width$}  {name}"`（宽度 ≥ 4），主题新到旧，无 `Name (N):` 与空行 | `src/command/shortlog.rs:749-805` |
| 排序按小写名或 (计数降序, 小写名)，`-e` 同名不同邮箱时次序不稳定 | `src/command/shortlog.rs:557-607`（`:579-581`） |
| 既有用例钉住旧格式（本卡同卡只改其旧格式期望） | `tests/command/shortlog_test.rs`（`test_shortlog_basic`、`test_shortlog_numbered`、`test_shortlog_summary`、`test_shortlog_email`、`test_shortlog_combined_flags`、`test_shortlog_date_filter`、`test_shortlog_reads_piped_log_input`） |
| Git 格式 `%6d\t%s` 与 `%s (%d):`、主题旧到新、`strcmp` 排序 | `git@6de20f6092:builtin/shortlog.c:493-530`；Git 2.54.0 预探 |
| stdin 模式（无修订参数且 stdin 非终端）与仓库遍历共用 `finalize_shortlog` 排序与 `render_shortlog_output` 人读渲染 | `src/command/shortlog.rs:432-442,550,557,701-746,749` |
| `--min-count`、`--reverse`、`--top` 在排序之后依次应用（Libra 扩展选项） | `src/command/shortlog.rs:141-151,584-595` |
| `--json` 由 `emit_json_data` 输出 `finalize_shortlog` 的结果；改动前 JSON 与人读共用其小写名排序及 `--min-count`、`--reverse`、`--top` 处理 | `src/command/shortlog.rs:402-407,557-607` |

**夹具:** F16m：`libra init` 仓库，三次提交（`libra commit --author`，提交者经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`，作者与提交者日期经 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` 取 `<epoch> +0000`）：① 作者 `Alice <alice@example.com>`、提交者 `Carol <carol@example.com>`、`1112875200`、主题 `first`；② 作者 `Bob <bob@example.com>`、提交者 Carol、`1146484800`、主题 `second`，正文含 `Co-authored-by: Alice <alice@example.com>` 与 `Co-authored-by: Dave <dave@example.com>` 两条 trailer；③ 作者 `Alice <alice@other.example>`、提交者 `Dan <dan@example.com>`、`1180785600`、主题 `third`。除 AC7 外，全部调用均为 `libra shortlog … HEAD` 且 stdin 为空。 F16o：另一 `libra init` 仓库，四次提交的作者（`libra commit --author`）依次为 `Zed <z@x>`、`Amy <a@x>`、`Zed <z@x>`、`bob <b@x>`（提交者统一为 `C <c@x>`）。 F16s（AC7）：`create_committed_repo_via_cli` 仓库（其自身提交在 stdin 模式下被忽略），以 `run_libra_command_with_stdin` 执行不带修订参数的 `libra shortlog`；stdin 文本为三段 `git log --pretty=short` 格式记录，依次为 `commit 111…1`/`Author: Alice <alice@example.com>`/主题 `first`、`commit 222…2`/`Author: Bob <bob@example.com>`/主题 `second`、`commit 333…3`/`Author: Alice <alice@example.com>`/主题 `third`（每段主题行前 4 个空格、段间空行）。 F16j（AC6）：另一 `libra init` 仓库，七次提交的作者（`libra commit --author`）依次为 `Zed <z@x>`、`amy <a@x>`、`Bob <b@x>`、`Zed <z@x>`、`amy <a@x>`、`Bob <b@x>`、`cat <c@x>`（提交者统一为 `C <c@x>`），即 `Zed`、`amy`、`Bob` 各 2 次、`cat` 1 次；`--min-count 2` 去掉 `cat`，改动前实现按小写名排序（`amy`、`Bob`、`Zed`）后反转并截取得到 `Zed`、`Bob`，而按新的字节序（`Bob`、`Zed`、`amy`）则得到 `amy`、`Zed`，忽略 `--min-count`、`--reverse` 或 `--top` 任一项也都会得到不同结果。AC6 的基准 JSON 在本卡改动前的实现上以同一调用录得并硬编码进测试；录得后先核对其 `data.authors` 各项的 `name` 依次为 `Zed`、`Bob`（不符即 STOP，按 ER-03 修订夹具）。

**Acceptance criteria:**
- [ ] **AC1** F16m 上 `libra shortlog HEAD` 输出 Git 段格式。
- [ ] **AC2** F16m 上 `libra shortlog -s HEAD` 输出 `%6d\t` 摘要行。
- [ ] **AC3** F16o 上 `libra shortlog -s HEAD` 按字节序排序（大写先于小写）。
- [ ] **AC4** F16o 上 `libra shortlog -n -s HEAD` 按计数降序稳定排序。
- [ ] **AC5** F16m 上 `libra shortlog -e -s HEAD` 按完整键确定性排序。
- [ ] **AC6** F16j 上 `libra --json shortlog -s --min-count 2 --reverse --top 2 HEAD` 的 stdout 与本卡改动前的实现在同一夹具上以同一调用得到的基准 JSON 逐字相同。
- [ ] **AC7** 不带修订参数的 `libra shortlog` 读取 F16s 的 stdin 时输出 Git 段格式（主题按输入次序的逆序）。
- [ ] **AC8** F16o 上 `libra shortlog -s --reverse --top 2 HEAD` 在新排序之后再反转并截取前两项。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`%6d`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::shortlog_test::shortlog_output_matches_git_format`：exit 0；stdout 逐字等于 `Alice (2):\n      first\n      third\n\nBob (1):\n      second\n\n`（Git 2.54.0 预探）
- AC2 → `command::shortlog_test::shortlog_output_matches_git_format`：exit 0；stdout 逐字等于 `     2\tAlice\n     1\tBob\n`
- AC3 → `command::shortlog_test::shortlog_output_matches_git_format`：exit 0；stdout 逐字等于 `     1\tAmy\n     2\tZed\n     1\tbob\n`
- AC4 → `command::shortlog_test::shortlog_output_matches_git_format`：exit 0；stdout 逐字等于 `     2\tZed\n     1\tAmy\n     1\tbob\n`
- AC5 → `command::shortlog_test::shortlog_output_matches_git_format`：exit 0；stdout 逐字等于 `     1\tAlice <alice@example.com>\n     1\tAlice <alice@other.example>\n     1\tBob <bob@example.com>\n`
- AC6 → `command::shortlog_test::shortlog_output_matches_git_format`：exit 0；stdout 逐字等于开工时在改动前的实现上录得的基准 JSON
- AC7 → `command::shortlog_test::shortlog_stdin_output_matches_git_format`：exit 0；stdout 逐字等于 `Alice (2):\n      third\n      first\n\nBob (1):\n      second\n\n`（Git 2.54.0 预探）
- AC8 → `command::shortlog_test::shortlog_output_matches_git_format`：exit 0；stdout 逐字等于 `     1\tbob\n     2\tZed\n`（若先按旧的小写名排序、反转、截取后再在渲染层排序，则为 `     2\tZed\n     1\tbob\n`）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::shortlog_test::shortlog_output_matches_git_format)'` `(new)`：覆盖 AC1–AC6、AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::shortlog_test::shortlog_stdin_output_matches_git_format)'` `(new)`：覆盖 AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::shortlog_test::/)'`（共享回归组：`command::shortlog_test` 模块族（旧格式断言按本卡被取代的格式更新，`--json` 用例 `test_shortlog_json_output_has_author_summary` 保持不变）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-25`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/shortlog.rs`, `tests/command/shortlog_test.rs`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/shortlog.rs`, `tests/command/shortlog_test.rs`
**Docs and compatibility impact:** `docs/commands/shortlog.md` 与 zh-CN：示例输出改为 Git 格式（`:61-95` 一带）、人读排序规则、stdin 模式同格式、`--json` 不变；`docs/development/commands/shortlog.md`：ADR-PM-04；`COMPATIBILITY.md` `shortlog` 行（W-1，基线第 198 行）原地改写；网站页 `shortlog.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=shortlog人读输出格式与排序; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=PM-25,DEP-PM-11; writeset=串行于 PM-25; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-30: `libra shortlog` 多重 `--group`、`--no-group` 与提交内去重（#1034；自 PM-16 拆出；拆出 PM-35）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-16` 拆出：`--group` 可重复，各分组对每个提交产生的键合并后计数，且同一提交内重复的键（同一 trailer 重复值，或不同分组产生的相同名字）只计一次（Git `strset` 去重）；`--no-group` 清空此前的分组，分组为空时回落到 `author`；未知分组类型以 `LBR-CLI-002` 失败。（按 G-09 拆出 `PM-35` 承接 `format:`/DWIM 格式分组与 `--date`。）
**Out of scope:** 格式分组与 `--date`（`PM-35`）；`--group` 类型名大小写不敏感与 `-c` 与 `--group` 取并集（尚未排期，重启条件：后续 shortlog 计划）；`-<n>`（`PM-19`）与 mailmap（`PM-40`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--group` 为单值 `Option<String>`，无 `--no-group`/`--date` | `src/command/shortlog.rs:95-178`（`:153-157`） |
| 分组类型解析：精确 `author`/`committer`/`trailer:`，其它报错 | `src/command/shortlog.rs:181-214` |
| trailer 分组不做提交内去重 | `src/command/shortlog.rs:221-252,543-546` |
| `with_date_mode` 与 `format:` 时间渲染 | `src/internal/log/formatter.rs:75-78,667-695` |
| Git 多分组、`--no-group`、DWIM 格式、提交内 strset 去重、`unknown group type`（129） | `git@6de20f6092:builtin/shortlog.c`；Git 2.54.0 预探 |

**夹具:** F16m：`libra init` 仓库，三次提交（`libra commit --author`，提交者经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`，作者与提交者日期经 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` 取 `<epoch> +0000`）：① 作者 `Alice <alice@example.com>`、提交者 `Carol <carol@example.com>`、`1112875200`、主题 `first`；② 作者 `Bob <bob@example.com>`、提交者 Carol、`1146484800`、主题 `second`，正文含 `Co-authored-by: Alice <alice@example.com>` 与 `Co-authored-by: Dave <dave@example.com>` 两条 trailer；③ 作者 `Alice <alice@other.example>`、提交者 `Dan <dan@example.com>`、`1180785600`、主题 `third`。全部调用均为 `libra shortlog … HEAD` 且 stdin 为空。 F30t：另一仓库，根提交之后两次提交的正文 trailer 分别为（`Repeated: Foo`、`Repeated: Foo`）与（`Repeated: Foo`、`Repeated: Bar`）。F30x：另一仓库，提交 ① 作者与提交者均为 `Same <s@x>`，提交 ② 作者 `Ann <a@x>`、提交者 `Same <s@x>`。

**Acceptance criteria:**
- [ ] **AC1** F16m 上 `-s --group=author --group=committer` 输出四行。
- [ ] **AC2** F16m 上 `-s --group=author --no-group --group=committer` 只按提交者分组。
- [ ] **AC3** F16m 上 `-s --no-group` 回落到按作者分组。
- [ ] **AC4** `--group=bogus` 以 `LBR-CLI-002` 失败。
- [ ] **AC5** F30t 上 `-s --group=trailer:repeated` 对提交内重复值只计一次。
- [ ] **AC6** F30x 上 `-s --group=author --group=committer` 对同一提交内两个分组产生的相同名字只计一次。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--no-group`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::shortlog_test::shortlog_group_options_match_git`：exit 0；stdout 逐字等于 `     2\tAlice\n     1\tBob\n     2\tCarol\n     1\tDan\n`（Git 2.54.0 预探）
- AC2 → `command::shortlog_test::shortlog_group_options_match_git`：exit 0；stdout 逐字等于 `     2\tCarol\n     1\tDan\n`
- AC3 → `command::shortlog_test::shortlog_group_options_match_git`：exit 0；stdout 逐字等于 `     2\tAlice\n     1\tBob\n`
- AC4 → `command::shortlog_test::shortlog_group_options_match_git`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC5 → `command::shortlog_test::shortlog_group_dedups_within_commit`：exit 0；stdout 逐字等于 `     1\tBar\n     2\tFoo\n`（Git 2.54.0 预探）
- AC6 → `command::shortlog_test::shortlog_group_dedups_within_commit`：exit 0；stdout 逐字等于 `     1\tAnn\n     2\tSame\n`（Git 2.54.0 预探）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::shortlog_test::shortlog_group_options_match_git)'` `(new)`：覆盖 AC1–AC4（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::shortlog_test::shortlog_group_dedups_within_commit)'` `(new)`：覆盖 AC5–AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::shortlog_test::/)'`（共享回归组：`command::shortlog_test` 模块族（含 `group_trailer_groups_by_trailer_value`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-16`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/shortlog.rs`, `tests/command/shortlog_test.rs`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/shortlog.rs`, `tests/command/shortlog_test.rs`
**Docs and compatibility impact:** `docs/commands/shortlog.md` 与 zh-CN：`--group` 可重复、`--no-group` 与回落、提交内去重与未知分组类型报错；`docs/development/commands/shortlog.md` 同步；`COMPATIBILITY.md` `shortlog` 行（W-1，基线第 198 行）原地补记；网站页 `shortlog.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=shortlog多分组与提交内去重; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=PM-16,DEP-PM-11; writeset=串行于 PM-16; release=independent; split-from=PM-16; exception=N/A`

---

### Task PM-19: `libra shortlog` 的 `-<n>`/`--max-count`（#1034；自 PM-16 拆出；拆出 PM-40）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-16` 拆出：在 `src/cli.rs` 的 `rewrite_short_number_args` 增加 `shortlog`，把 `-<n>` 重写为新增的 `--max-count=<n>`（不影响 `-n` 即 `--numbered`），`--max-count` 限制整个修订遍历的提交总数。（按 G-01/G-04 拆出 `PM-40` 承接 mailmap 规范化。）
**Out of scope:** `slog` 别名的 `-<n>` 重写（尚未排期）；mailmap 规范化（`PM-40`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `-<n>` 重写只作用于 `log`、`reflog`、`format-patch`，按子命令名精确匹配 | `src/cli.rs:1056-1119,3493-3497` |
| shortlog `-n` 即 `--numbered`，无 `--max-count` | `src/command/shortlog.rs:98-100` |
| Git：`-<n>` 限制提交总数 | `git@6de20f6092:builtin/shortlog.c`；Git 2.54.0 预探 |

**夹具:** F16m：`libra init` 仓库，三次提交（`libra commit --author`，提交者经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`，作者与提交者日期经 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` 取 `<epoch> +0000`）：① 作者 `Alice <alice@example.com>`、提交者 `Carol <carol@example.com>`、`1112875200`、主题 `first`；② 作者 `Bob <bob@example.com>`、提交者 Carol、`1146484800`、主题 `second`，正文含 `Co-authored-by: Alice <alice@example.com>` 与 `Co-authored-by: Dave <dave@example.com>` 两条 trailer；③ 作者 `Alice <alice@other.example>`、提交者 `Dan <dan@example.com>`、`1180785600`、主题 `third`。全部调用均为 `libra shortlog … HEAD` 且 stdin 为空。 F16o：另一 `libra init` 仓库，四次提交的作者（`libra commit --author`）依次为 `Zed <z@x>`、`Amy <a@x>`、`Zed <z@x>`、`bob <b@x>`（提交者统一为 `C <c@x>`）。

**Acceptance criteria:**
- [ ] **AC1** F16m 上 `libra shortlog -s -1 HEAD` 只统计最新提交。
- [ ] **AC2** F16m 上 `libra shortlog -2 HEAD` 只统计最近两次提交。
- [ ] **AC3** F16o 上 `libra shortlog -ns -4 HEAD` 中 `-n` 仍为 `--numbered`（按计数降序）。
- [ ] **AC4** F16m 上 `libra shortlog -s --max-count=1 HEAD` 只统计最新提交。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--max-count`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::shortlog_test::shortlog_max_count_shorthand`：exit 0；stdout 逐字等于 `     1\tAlice\n`（Git 2.54.0 预探）
- AC2 → `command::shortlog_test::shortlog_max_count_shorthand`：exit 0；stdout 逐字等于 `Alice (1):\n      third\n\nBob (1):\n      second\n\n`
- AC3 → `command::shortlog_test::shortlog_max_count_shorthand`：exit 0；stdout 逐字等于 `     2\tZed\n     1\tAmy\n     1\tbob\n`（Git 2.54.0 预探；`-n` 若被忽略则次序为 `Amy`、`Zed`、`bob`）
- AC4 → `command::shortlog_test::shortlog_max_count_shorthand`：exit 0；stdout 逐字等于 `     1\tAlice\n`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::shortlog_test::shortlog_max_count_shorthand)'` `(new)`：覆盖 AC1–AC4（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::shortlog_test::/)'`（共享回归组：`command::shortlog_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: `src/cli.rs` 全局参数重写（ER-13 T-1）；另含版本面 `install.sh`/`install.ps1`
**Dependencies:** `PM-30`、`DEP-PM-11`、`DEP-PM-13`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `src/command/shortlog.rs`, `tests/command/shortlog_test.rs`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/cli.rs`, `src/command/shortlog.rs`, `tests/command/shortlog_test.rs`
**Docs and compatibility impact:** `docs/commands/shortlog.md` 与 zh-CN：`-<n>`/`--max-count` 与 `-n` 的区别；`docs/development/commands/shortlog.md` 同步；`COMPATIBILITY.md` `shortlog` 行（W-1，基线第 198 行）原地补记；网站页 `shortlog.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=shortlog提交数限制; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=PM-30,DEP-PM-11,DEP-PM-13; writeset=串行于 PM-30; release=independent; split-from=PM-16; exception=N/A`

---

### Task PM-40: `libra shortlog` 与 `check-mailmap` 的 mailmap 规范化（#1034；自 PM-19 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-19` 拆出：把 `src/command/check_mailmap.rs` 的 mailmap 解析与解析结果查询提升为 `pub(crate)` 单一事实源，读取顺序为工作区 `.mailmap` 再 `mailmap.file`（后者覆盖前者），姓名与邮箱都按 ASCII 大小写不敏感匹配，并在 shortlog 分组键（作者/提交者）上应用。
**Out of scope:** `-<n>`/`--max-count`（`PM-19`）；`mailmap.blob`、`log`/`blame` 的 mailmap 显示（`COMPATIBILITY.md` 既有延后项，保持）；格式分组占位符 `%aN`/`%cN` 的 mailmap 映射（`PM-35`）；shortlog 分组键上的大小写不敏感匹配（与 `check-mailmap` 共用本卡提升的同一查询函数，以 AC3 验收，不单独验收）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| mailmap 解析私有、只读 `<workdir>/.mailmap`、姓名大小写敏感匹配 | `src/command/check_mailmap.rs:44-50,116-196` |
| Git：`.mailmap` 后读 `mailmap.file` 且后者覆盖；姓名与邮箱均 `strcasecmp` | `git@6de20f6092:mailmap.c:53-76,209-235`；Git 2.54.0 预探 |

**夹具:** F16m：`libra init` 仓库，三次提交（`libra commit --author`，提交者经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`，作者与提交者日期经 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` 取 `<epoch> +0000`）：① 作者 `Alice <alice@example.com>`、提交者 `Carol <carol@example.com>`、`1112875200`、主题 `first`；② 作者 `Bob <bob@example.com>`、提交者 Carol、`1146484800`、主题 `second`，正文含 `Co-authored-by: Alice <alice@example.com>` 与 `Co-authored-by: Dave <dave@example.com>` 两条 trailer；③ 作者 `Alice <alice@other.example>`、提交者 `Dan <dan@example.com>`、`1180785600`、主题 `third`。全部调用均为 `libra shortlog … HEAD` 且 stdin 为空。 F19m：F16m 在工作区写入未跟踪的 `.mailmap`（不 `libra add`、不提交，F16m 仍为三次提交），内容为（`Alice Proper <alice@proper.example> <alice@other.example>` 与 `Alice Proper <alice@proper.example> Alice <alice@example.com>`）。F19f：F16m 在工作区写入未跟踪的 `.mailmap`（不 `libra add`、不提交，F16m 仍为三次提交），内容为（`Robert <robert@x.example> <bob@example.com>`）与绝对路径文件 `<tmp>/other.map`（`Bobby <bobby@x.example> <bob@example.com>`），并 `libra config mailmap.file <tmp>/other.map`。 F19c：F16m 在工作区写入未跟踪的 `.mailmap`（不 `libra add`、不提交），内容为（`Caroline <caroline@x.example> <carol@example.com>`）。

**Acceptance criteria:**
- [ ] **AC1** F19m 上 `libra shortlog -s -e HEAD` 应用 `.mailmap`。
- [ ] **AC2** F19f 上 `libra shortlog -s -e HEAD` 中 `mailmap.file` 覆盖 `.mailmap`。
- [ ] **AC3** F19m 上 `libra check-mailmap "alice <ALICE@EXAMPLE.COM>"` 按大小写不敏感匹配映射。
- [ ] **AC4** F19c 上 `libra shortlog -s -e -c HEAD` 以 mailmap 映射后的提交者分组。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`mailmap.file`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::shortlog_test::shortlog_and_check_mailmap_apply_mailmap`：exit 0；stdout 逐字等于 `     2\tAlice Proper <alice@proper.example>\n     1\tBob <bob@example.com>\n`
- AC2 → `command::shortlog_test::shortlog_and_check_mailmap_apply_mailmap`：exit 0；stdout 逐字等于 `     1\tAlice <alice@example.com>\n     1\tAlice <alice@other.example>\n     1\tBobby <bobby@x.example>\n`
- AC3 → `command::shortlog_test::shortlog_and_check_mailmap_apply_mailmap`：exit 0；stdout 逐字等于 `Alice Proper <alice@proper.example>\n`
- AC4 → `command::shortlog_test::shortlog_and_check_mailmap_apply_mailmap`：exit 0；stdout 逐字等于 `     2\tCaroline <caroline@x.example>\n     1\tDan <dan@example.com>\n`（Git 2.54.0 预探）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::shortlog_test::shortlog_and_check_mailmap_apply_mailmap)'` `(new)`：覆盖 AC1–AC4（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::shortlog_test::/)'`（共享回归组：`command::shortlog_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::check_mailmap_test::/)'`（共享回归组：`command::check_mailmap_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: `check_mailmap.rs` 提升为 shortlog 共用的单一事实源（GC-02，ER-13 T-1）；另含版本面 `install.sh`/`install.ps1`
**Dependencies:** `PM-19`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/shortlog.rs`, `src/command/check_mailmap.rs`, `tests/command/shortlog_test.rs`, `tests/command/check_mailmap_test.rs`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`, `docs/commands/check-mailmap.md`, `docs/commands/zh-CN/check-mailmap.md`, `docs/development/commands/check-mailmap.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-mailmap.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/shortlog.rs`, `src/command/check_mailmap.rs`, `tests/command/shortlog_test.rs`, `tests/command/check_mailmap_test.rs`
**Docs and compatibility impact:** `docs/commands/shortlog.md`、`docs/commands/check-mailmap.md` 及其 zh-CN：mailmap 来源次序与大小写不敏感匹配（修正 check-mailmap 文档 `:62-63` 所述「`mailmap.file` 未支持」）；对应 `docs/development/commands/*.md` 同步；`COMPATIBILITY.md` 的 `shortlog`（基线第 198 行）与 `check-mailmap`（基线第 263 行）两行（W-1）原地改写；网站页 `shortlog.en.md`、`check-mailmap.en.md` 按「文档与网站验收命令」对本卡命令集合 {`shortlog`、`check-mailmap`} 逐页同步（第 3 步 `--only shortlog check-mailmap`）；文档关键字在两个命令的 EN/zh 页都检查。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=shortlog与check-mailmap的mailmap规范化; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=4/8; VER=6/8; landing=1; prod-files=2; scope=M; deps=PM-19,DEP-PM-11; writeset=串行于 PM-19; release=independent; split-from=PM-19; exception=N/A`

---

### Task PM-35: `libra shortlog` 格式分组（`format:` 与 DWIM `%`）与 `--date`（#1034；自 PM-30 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-30` 拆出：`--group=format:<fmt>` 与不带 `format:` 但含 `%` 的 DWIM 形式按 `CommitFormatter` 渲染分组键，新增 `--date=<mode>` 经 `CommitFormatter::with_date_mode` 作用于其中的日期占位符；`%aN`/`%cN` 在 shortlog 格式分组中经 `PM-40` 的 mailmap 单一事实源映射作者/提交者名（与 Git 一致）。
**Out of scope:** 多分组与去重（`PM-30`）；`%aE`/`%cE` 等其它 mailmap 感知占位符（尚未排期，重启条件：后续 shortlog/log 格式计划）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 分组类型只识别 `author`/`committer`/`trailer:` | `src/command/shortlog.rs:181-214` |
| `with_date_mode` 与 `format:` 时间渲染；未知占位符原样输出 | `src/internal/log/formatter.rs:75-78,453-459,667-695` |
| Git DWIM：含 `%` 的分组值视为 `format:` | `git@6de20f6092:builtin/shortlog.c`；Git 2.54.0 预探 |
| Git：格式分组的 `%cN`/`%aN` 应用 mailmap | `git@6de20f6092:builtin/shortlog.c`；Git 2.54.0 预探 |

**夹具:** F16m：`libra init` 仓库，三次提交（`libra commit --author`，提交者经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`，作者与提交者日期经 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` 取 `<epoch> +0000`）：① 作者 `Alice <alice@example.com>`、提交者 `Carol <carol@example.com>`、`1112875200`、主题 `first`；② 作者 `Bob <bob@example.com>`、提交者 Carol、`1146484800`、主题 `second`，正文含 `Co-authored-by: Alice <alice@example.com>` 与 `Co-authored-by: Dave <dave@example.com>` 两条 trailer；③ 作者 `Alice <alice@other.example>`、提交者 `Dan <dan@example.com>`、`1180785600`、主题 `third`。全部调用均为 `libra shortlog … HEAD` 且 stdin 为空。 F35m：F16m 在工作区写入未跟踪的 `.mailmap`（不 `libra add`、不提交，F16m 仍为三次提交），内容为（`Carol Proper <carol@proper.example> <carol@example.com>` 与 `Robert <robert@x.example> <bob@example.com>`）。

**Acceptance criteria:**
- [ ] **AC1** F16m 上 `-s --group=format:%cN` 按提交者名分组。
- [ ] **AC2** F16m 上 `-s --group=%cn --group=%an`（DWIM 格式）输出四行。
- [ ] **AC3** F16m 上 `-s --date=format:%Y "--group=format:%cN (%cd)"` 按提交者与年份分组。
- [ ] **AC4** F35m 上 `-s --group=format:%cN` 以 mailmap 映射后的提交者名分组。
- [ ] **AC5** F35m 上 `-s --group=format:%aN` 以 mailmap 映射后的作者名分组。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--date`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::shortlog_test::shortlog_format_groups_and_date`：exit 0；stdout 逐字等于 `     2\tCarol\n     1\tDan\n`（Git 2.54.0 预探）
- AC2 → `command::shortlog_test::shortlog_format_groups_and_date`：exit 0；stdout 逐字等于 `     2\tAlice\n     1\tBob\n     2\tCarol\n     1\tDan\n`
- AC3 → `command::shortlog_test::shortlog_format_groups_and_date`：exit 0；stdout 逐字等于 `     1\tCarol (2005)\n     1\tCarol (2006)\n     1\tDan (2007)\n`
- AC4 → `command::shortlog_test::shortlog_format_groups_apply_mailmap`：exit 0；stdout 逐字等于 `     2\tCarol Proper\n     1\tDan\n`（Git 2.54.0 预探）
- AC5 → `command::shortlog_test::shortlog_format_groups_apply_mailmap`：exit 0；stdout 逐字等于 `     2\tAlice\n     1\tRobert\n`（Git 2.54.0 预探）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::shortlog_test::shortlog_format_groups_and_date)'` `(new)`：覆盖 AC1–AC3（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::shortlog_test::shortlog_format_groups_apply_mailmap)'` `(new)`：覆盖 AC4–AC5（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::shortlog_test::/)'`（共享回归组：`command::shortlog_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-40`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/shortlog.rs`, `tests/command/shortlog_test.rs`, `docs/commands/shortlog.md`, `docs/commands/zh-CN/shortlog.md`, `docs/development/commands/shortlog.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/shortlog.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/shortlog.rs`, `tests/command/shortlog_test.rs`
**Docs and compatibility impact:** `docs/commands/shortlog.md` 与 zh-CN：`format:`/DWIM 格式分组、`--date` 与 `%aN`/`%cN` 的 mailmap 映射；`docs/development/commands/shortlog.md` 同步；`COMPATIBILITY.md` `shortlog` 行（W-1，基线第 198 行）原地补记；网站页 `shortlog.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=shortlog格式分组与--date; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=5/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=PM-40,DEP-PM-11; writeset=串行于 PM-40; release=independent; split-from=PM-30; exception=N/A`

---

### Task PM-17: `libra check-ignore` 相对当前目录解析路径并原样回显（#1035；拆出 PM-20、PM-31）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** `src/command/check_ignore.rs` 的 `classify_path` 把相对路径按**调用时的当前目录**（而非仓库根）解析后再做仓库内规范化，输出时回显用户给出的原始路径（`-v` 的来源仍相对仓库根）；越出仓库根的路径以 `LBR-CLI-003` 失败。（按 G-09 拆出 `PM-20` 承接 `--quiet`/`-z` 规则、`PM-31` 承接 `--stdin` 流式与引号。）
**Out of scope:** `--stdin` 流式与 C 引号（`PM-31`）；`--quiet`/`-z`/否定匹配退出码（`PM-20`）；全局来源（`PM-18`）；`check-attr` 的子目录路径解析（尚未排期）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 相对路径以 `workdir.join(path)`（仓库根）解析 | `src/command/check_ignore.rs:182-209`（`:195-199`） |
| 越界报 `path '<p>' is outside the repository`（LBR-CLI-003） | `src/command/check_ignore.rs:202-209` |
| Git：从 `a/` 执行时路径相对 cwd、输出原样回显 | `git@6de20f6092:builtin/check-ignore.c`；`t/t0008-ignores.sh`（TC-3599–3601、3603）；Git 2.54.0 预探 |

**夹具:** F17：仓库根 `.gitignore`=`one\nignored-*\n`、`a/.gitignore`=`two*\n*three\n`、`a/b/.gitignore`=`!on*\n!two\n`（`t0008` 同构）；所有调用经 `run_libra_command_with_env` 显式传入固定的 HOME、`XDG_CONFIG_HOME` 与 `LIBRA_CONFIG_GLOBAL_DB`（不随 cwd 变化）。 本卡用例的 cwd 为 `a/`。

**Acceptance criteria:**
- [ ] **AC1** `libra check-ignore --verbose 3-three three-not-this-one` 输出 `a/.gitignore:2:*three\t3-three`。
- [ ] **AC2** `libra check-ignore 3-three ../one ../not-ignored` 输出 `3-three` 与 `../one`。
- [ ] **AC3** `libra check-ignore -v -n 3-three ../not-ignored` 输出命中行与 `::` 行。
- [ ] **AC4** `libra check-ignore ../../outside` 以 `LBR-CLI-003` 失败。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：current directory。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::check_ignore_test::check_ignore_resolves_paths_from_cwd`：exit 0；stdout 逐字等于 `a/.gitignore:2:*three\t3-three\n`（Git 2.54.0 预探）
- AC2 → `command::check_ignore_test::check_ignore_resolves_paths_from_cwd`：exit 0；stdout 逐字等于 `3-three\n../one\n`
- AC3 → `command::check_ignore_test::check_ignore_resolves_paths_from_cwd`：exit 0；stdout 逐字等于 `a/.gitignore:2:*three\t3-three\n::\t../not-ignored\n`
- AC4 → `command::check_ignore_test::check_ignore_resolves_paths_from_cwd`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::check_ignore_test::check_ignore_resolves_paths_from_cwd)'` `(new)`：覆盖 AC1–AC4（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::check_ignore_test::/)'`（共享回归组：`command::check_ignore_test` 模块族（含 `path_escaping_the_worktree_is_a_fatal_error`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-35`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/check_ignore.rs`, `tests/command/check_ignore_test.rs`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/check_ignore.rs`, `tests/command/check_ignore_test.rs`
**Docs and compatibility impact:** `docs/commands/check-ignore.md` 与 zh-CN：路径相对当前目录、原样回显；`docs/development/commands/check-ignore.md` 同步；`COMPATIBILITY.md` `check-ignore` 行（W-1，基线第 264 行）原地补记；网站页 `check-ignore.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=check-ignore当前目录相对路径; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=PM-35,DEP-PM-11; writeset=串行于 PM-35; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-31: `libra check-ignore --stdin` 逐行流式处理与 C 引号（#1035；自 PM-17 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-17` 拆出：`--stdin` 改为逐行（或 `-z` 时逐 NUL 记录）读取，每读入一条即判定、写出并 flush，不再读到 EOF 才处理；非 `-z` 时以 `"` 开头的输入行按 Git C 引号规则解引号（不合法以 `LBR-CLI-002` 失败），输出路径按 `quote_c_style` 仅在需要时加引号；`-z` 时既不解引号也不加引号。
**Out of scope:** 路径基准（`PM-17`）；`--quiet` 与 `-z` 规则（`PM-20`）；`check-attr --stdin` 的流式（尚未排期）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `read_stdin_paths` 以 `read_to_end`（上限 64 MiB）读完再处理；不解引号；丢弃空行 | `src/command/check_ignore.rs:274-308` |
| 全部判定完成后一次渲染，路径不加引号 | `src/command/check_ignore.rs:159-165,310-377` |
| Git：逐行判定并 `maybe_flush_or_die`；`line is badly quoted`；输出 `quote_c_style` | `git@6de20f6092:builtin/check-ignore.c`；`t/t0008-ignores.sh`（TC-3584、TC-3598）；Git 2.54.0 预探 |

**夹具:** F17：仓库根 `.gitignore`=`one\nignored-*\n`、`a/.gitignore`=`two*\n*three\n`、`a/b/.gitignore`=`!on*\n!two\n`（`t0008` 同构）；所有调用显式传入固定的 HOME、`XDG_CONFIG_HOME` 与 `LIBRA_CONFIG_GLOBAL_DB`（不随 cwd 变化）：无 stdin 输入的调用经 `run_libra_command_with_env`，需 stdin 输入的非流式调用经 `run_libra_command_with_stdin_and_env`（`tests/command/mod.rs:439-466`，以管道写入给定字节后关闭 stdin）。 本卡用例 cwd 为仓库根；`stdin.txt` 依次为 `one`、`"a/b/one two"`、`"a/b/one\"three"`、`not-ignored`、`"a/space dir/x"` 五行。流式用例以 `tests/command/mod.rs` 的 `base_libra_command` 构造子进程（同样设置上述三个环境变量，stdin/stdout 为管道，stdin 在读取期间保持打开），每条读取设 10 秒超时。

**Acceptance criteria:**
- [ ] **AC1** `libra check-ignore -n -v --stdin` 子进程在写入 `one\n` 且 stdin 仍打开时，10 秒内输出首行 `.gitignore:1:one\tone`。
- [ ] **AC2** 随后写入 `two\n`（stdin 仍打开），10 秒内输出第二行 `::\ttwo`。
- [ ] **AC3** `libra check-ignore -v --stdin < stdin.txt` 输出三行（解引号后匹配、按需加引号）。
- [ ] **AC4** `libra check-ignore -v -n --stdin < stdin.txt` 另输出两条 `::` 行。
- [ ] **AC5** stdin 为 `"bad` 时 `libra check-ignore --stdin` 以 `LBR-CLI-002` 失败。
- [ ] **AC6** NUL 分隔输入 `one`、`a/b/one"x`、`not-ignored` 时 `libra check-ignore -z -v --stdin` 输出不加引号的 NUL 字段序列。
- [ ] **AC7** `libra check-ignore -z -v --stdin` 子进程在写入 `one` 加一个 NUL 且 stdin 仍打开时，10 秒内输出字段 `.gitignore`、`1`、`one`、`one`（各后接一个 NUL）。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--stdin`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::check_ignore_test::check_ignore_stdin_streams_each_line`：在关闭 stdin 之前读到的第一行逐字等于 `.gitignore:1:one\tone`
- AC2 → `command::check_ignore_test::check_ignore_stdin_streams_each_line`：在关闭 stdin 之前读到的第二行逐字等于 `::\ttwo`
- AC3 → `command::check_ignore_test::check_ignore_stdin_c_quoting`：exit 0；stdout 逐字等于 `.gitignore:1:one\tone\na/b/.gitignore:1:!on*\ta/b/one two\na/b/.gitignore:1:!on*\t"a/b/one\"three"\n`（Git 2.54.0 预探）
- AC4 → `command::check_ignore_test::check_ignore_stdin_c_quoting`：exit 0；stdout 逐字等于 AC3 期望后接 `::\tnot-ignored\n::\ta/space dir/x\n`
- AC5 → `command::check_ignore_test::check_ignore_stdin_c_quoting`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC6 → `command::check_ignore_test::check_ignore_stdin_c_quoting`：exit 0；stdout 字节逐字等于字段 `.gitignore`、`1`、`one`、`one`、`a/b/.gitignore`、`1`、`!on*`、`a/b/one"x` 各自后接一个 NUL 的拼接
- AC7 → `command::check_ignore_test::check_ignore_stdin_streams_each_line`：在关闭 stdin 之前读到的前四个 NUL 结尾字段依次逐字等于 `.gitignore`、`1`、`one`、`one`（Git 2.54.0 预探）
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::check_ignore_test::check_ignore_stdin_streams_each_line)'` `(new)`：覆盖 AC1–AC2、AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::check_ignore_test::check_ignore_stdin_c_quoting)'` `(new)`：覆盖 AC3–AC6（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::check_ignore_test::/)'`（共享回归组：`command::check_ignore_test` 模块族（含 `stdin_reads_pathnames`、`z_uses_nul_delimiters`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-17`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/check_ignore.rs`, `tests/command/check_ignore_test.rs`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/check_ignore.rs`, `tests/command/check_ignore_test.rs`
**Docs and compatibility impact:** `docs/commands/check-ignore.md` 与 zh-CN：`--stdin` 逐行流式、C 引号输入与输出、`-z` 不引号；`docs/development/commands/check-ignore.md` 同步；`COMPATIBILITY.md` `check-ignore` 行（W-1，基线第 264 行）原地补记；网站页 `check-ignore.en.md` 同步。
**Rollback mode:** `immutable-release`
**Performance budget:** GC-10：逐行读取、判定并 flush；可观测判据为 AC1–AC2（stdin 仍打开时即输出）。不再整体缓存 stdin 由 ER-05 代码评审核对，本卡不设自动化内存门。
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=check-ignore --stdin流式与C引号; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=7/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=PM-17,DEP-PM-11; writeset=串行于 PM-17; release=independent; split-from=PM-17; exception=N/A`

---

### Task PM-20: `libra check-ignore --quiet`、`-z` 参数规则与 `-v` 否定匹配退出码（#1035；自 PM-17 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-17` 拆出：按 Git 次序在任何判定前校验——`-z` 不带 `--stdin`、`--quiet` 带多个路径、`--quiet` 与 `--verbose` 同用均以 `LBR-CLI-002` 失败（`--quiet` 消费 `src/cli.rs` 的全局 `-q/--quiet`，不新增本地标志；与 `--stdin` 同用合法）；`--quiet` 单路径时 stdout 为空（stderr 不作承诺）、忽略退出 0、未忽略退出 1；`-v` 下命中否定规则（`!pattern`）也计为「有匹配」并退出 0。
**Out of scope:** 路径基准（`PM-17`）；`--stdin` 流式（`PM-31`）；多个参数错误同时出现时的校验先后（均以同一稳定码 `LBR-CLI-002` 失败，不可观测，不单独验收）；Git `fatal:` 措辞（Libra 保持 `LBR-CLI-002` 与 `error:` 前缀，有意差异）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 只校验 `-n` 需 `-v`、`--stdin` 与路径互斥、无路径；缺 `-z`/`--quiet` 规则 | `src/command/check_ignore.rs:113-139` |
| 全局 `-q/--quiet` 只抑制 stdout | `src/cli.rs:285-286`；`src/command/check_ignore.rs:324` |
| 退出码只看 `entry.ignored`，`-v` 下否定匹配退出 1 | `src/command/check_ignore.rs:163-172` |
| Git 参数规则次序与 `-v` 下否定匹配计数 | `git@6de20f6092:builtin/check-ignore.c`；`t/t0008-ignores.sh:250-280`（TC-3595）；Git 2.54.0 预探 |
| 既有用例 `z_uses_nul_delimiters` 以 `-z` 不带 `--stdin` 断言成功；本卡同卡把它改为经 `--stdin` 输入同样两条路径，保留其 NUL 输出断言 | `tests/command/check_ignore_test.rs:124-130` |

**夹具:** F17：仓库根 `.gitignore`=`one\nignored-*\n`、`a/.gitignore`=`two*\n*three\n`、`a/b/.gitignore`=`!on*\n!two\n`（`t0008` 同构）；所有调用显式传入固定的 HOME、`XDG_CONFIG_HOME` 与 `LIBRA_CONFIG_GLOBAL_DB`（不随 cwd 变化）：无 stdin 输入的调用经 `run_libra_command_with_env`，需 stdin 输入的非流式调用经 `run_libra_command_with_stdin_and_env`（`tests/command/mod.rs:439-466`，以管道写入给定字节后关闭 stdin）。 本卡用例 cwd 为仓库根。

**Acceptance criteria:**
- [ ] **AC1** `libra check-ignore --quiet one` 的 stdout 为空并以 0 退出。
- [ ] **AC2** `libra check-ignore --quiet not-ignored` 的结果（退出码, stdout）为 (1, 空)。
- [ ] **AC3** `libra check-ignore --quiet one two` 以 `LBR-CLI-002` 失败。
- [ ] **AC4** `libra check-ignore --quiet -v one` 以 `LBR-CLI-002` 失败。
- [ ] **AC5** `libra check-ignore -z one`（无 `--stdin`）以 `LBR-CLI-002` 失败。
- [ ] **AC6** `libra check-ignore -v a/b/one`（命中否定规则 `!on*`）输出该匹配行并以 0 退出。
- [ ] **AC7** `printf 'one\n' | libra check-ignore --quiet --stdin` 合法且 stdout 为空。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`--quiet`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::check_ignore_test::check_ignore_quiet_and_argument_rules`：exit 0；stdout 为空
- AC2 → `command::check_ignore_test::check_ignore_quiet_and_argument_rules`：（退出码, stdout）二元组等于 (1, 空串)
- AC3 → `command::check_ignore_test::check_ignore_quiet_and_argument_rules`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC4 → `command::check_ignore_test::check_ignore_quiet_and_argument_rules`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC5 → `command::check_ignore_test::check_ignore_quiet_and_argument_rules`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC6 → `command::check_ignore_test::check_ignore_quiet_and_argument_rules`：exit 0；stdout 逐字等于 `a/b/.gitignore:1:!on*\ta/b/one\n`（Git 2.54.0 预探）
- AC7 → `command::check_ignore_test::check_ignore_quiet_and_argument_rules`：exit 0；stdout 为空
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::check_ignore_test::check_ignore_quiet_and_argument_rules)'` `(new)`：覆盖 AC1–AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::check_ignore_test::/)'`（共享回归组：`command::check_ignore_test` 模块族（含 `non_matching_requires_verbose_and_lists_unmatched`、`whitelisted_path_is_not_ignored`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-31`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/check_ignore.rs`, `tests/command/check_ignore_test.rs`, `docs/commands/check-ignore.md`, `docs/commands/zh-CN/check-ignore.md`, `docs/development/commands/check-ignore.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/check-ignore.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/check_ignore.rs`, `tests/command/check_ignore_test.rs`
**Docs and compatibility impact:** `docs/commands/check-ignore.md` 与 zh-CN：`--quiet`、`-z` 规则与 `-v` 否定匹配退出码；`docs/development/commands/check-ignore.md` 同步；`COMPATIBILITY.md` `check-ignore` 行（W-1，基线第 264 行）原地补记；网站页 `check-ignore.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=check-ignore quiet与参数规则; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=PM-31,DEP-PM-11; writeset=串行于 PM-31; release=independent; split-from=PM-17; exception=N/A`

---

### Task PM-14: 支持 `libra cat-file <type> <object>` 双位置参数形式（#1029）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** `CatFileArgs` 接受第二个位置参数：未给出 `-t`/`-s`/`-p`/`-e`/`--batch*`/`--ai*` 且恰有两个位置参数时进入 `<type> <object>` 模式——`<type>` 为 `blob`/`tree`/`commit`/`tag` 之一（其它以 `LBR-CLI-002` 失败），对象按 Git `odb_read_object_peeled` 语义剥离（tag→目标、commit→tree），输出该类型对象的**原始字节**（tree 为二进制格式，不是 `-p` 列表），无法剥离到该类型时以 `LBR-CLI-003` 失败；模式标志（`-t`/`-s`/`-p`/`-e`/`--batch*`/`--ai*`）与两个位置参数同用时，在任何模式分派之前以 `LBR-CLI-002` 失败；只有一个位置参数且无模式时保持现有的 `LBR-CLI-002` 拒绝（`src/command/cat_file.rs:381-390`）。
**Out of scope:** 不可解析对象名的退出码（Libra 129 对 Git 128，`COMPATIBILITY.md` 既有有意差异，保持）；`cat-file` 尊重 replace（`DEFER-PM-07`）；`--batch` 协议；`-p`/`-t`/`-s`/`-e`/`--batch*` 与其余 `--ai*` 标志与两个位置参数同用（与 AC8 同一位于模式分派之前的互斥校验，不单独验收）；只有一个位置参数且无模式（`libra cat-file HEAD`）的拒绝（现有行为已以 `LBR-CLI-002` 失败且本卡不改，不单独验收）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 只有一个位置参数 `object` | `src/command/cat_file.rs:174-175`（`CatFileArgs` 起于 `:91`） |
| 缺模式时报 `one of '-t', '-s', '-p', '-e' or an --ai* flag is required`（LBR-CLI-002） | `src/command/cat_file.rs:381-390` |
| `-p` 对 commit/tag 输出原始字节、对 tree 输出格式化列表 | `src/command/cat_file.rs:1300-1381` |
| 剥离 helper（`^{type}` 后缀路径） | `src/utils/util.rs:2484,2681-2702` |
| Git：类型不符 `git cat-file <name>: bad file`（128）；未知类型 `invalid object type`（128）；单参数 `only two arguments allowed in <type> <object> mode, not 1`（129）；模式加两参数 `too many arguments`（129） | `git@6de20f6092:builtin/cat-file.c:133-291,1344-1368`；Git 2.54.0 预探 |
| `--ai*` 模式在普通对象处理之前单独分派并忽略位置参数 | `src/command/cat_file.rs:156-175,331-345` |

**夹具:** F14：`init_repo_via_cli` 新建仓库并 `configure_identity_via_cli`（`tests/command/mod.rs:636,687`；不使用会额外提交 `tracked.txt` 的 `create_committed_repo_via_cli`）；提交 C 只含 `a.txt`=`hello\n`；附注标签 `v1` 指向 C（`libra tag -a -m annot v1`）。期望 tree 原始字节 T = `100644 a.txt\0` 后接 `a.txt` blob OID 的 20 字节二进制值。

**Acceptance criteria:**
- [ ] **AC1** `libra cat-file commit HEAD` 的 stdout 与 `libra cat-file -p HEAD` 的 stdout 字节相同。
- [ ] **AC2** `libra cat-file blob <a.txt 的 blob OID>` 的 stdout 字节为 `hello\n`。
- [ ] **AC3** `libra cat-file tree HEAD`（commit 剥离到 tree）输出原始 tree 字节 T。
- [ ] **AC4** `libra cat-file tag v1` 的 stdout 与 `libra cat-file -p v1` 的 stdout 字节相同。
- [ ] **AC5** `libra cat-file blob HEAD`（commit 不能剥离为 blob）以 `LBR-CLI-003` 失败。
- [ ] **AC6** `libra cat-file bogus HEAD` 以 `LBR-CLI-002` 失败。
- [ ] **AC7** `libra cat-file commit v1`（附注标签剥离到其指向的提交）的 stdout 与 `libra cat-file -p HEAD` 的 stdout 字节相同。
- [ ] **AC8** `libra cat-file --ai-list-types commit HEAD` 以 `LBR-CLI-002` 失败（互斥校验位于全部模式分派之前，`--ai*` 模式也不例外）。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`<type> <object>`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::cat_file_test::cat_file_type_object_form_prints_raw_bytes`：两次调用 exit 0；stdout 字节逐字相等
- AC2 → `command::cat_file_test::cat_file_type_object_form_prints_raw_bytes`：exit 0；stdout 逐字等于 `hello\n`
- AC3 → `command::cat_file_test::cat_file_type_object_form_prints_raw_bytes`：exit 0；stdout 字节逐字等于 T
- AC4 → `command::cat_file_test::cat_file_type_object_form_prints_raw_bytes`：两次调用 exit 0；stdout 字节逐字相等
- AC5 → `command::cat_file_test::cat_file_type_object_form_rejects_invalid_usage`：退出码非 0（前提）；stderr 含 `LBR-CLI-003`
- AC6 → `command::cat_file_test::cat_file_type_object_form_rejects_invalid_usage`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- AC7 → `command::cat_file_test::cat_file_type_object_form_prints_raw_bytes`：两次调用 exit 0；stdout 字节逐字相等
- AC8 → `command::cat_file_test::cat_file_type_object_form_rejects_invalid_usage`：退出码非 0（前提）；stderr 含 `LBR-CLI-002`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** N/A（被测命令只读；见「验证执行协议」第 4 项）

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::cat_file_test::cat_file_type_object_form_prints_raw_bytes)'` `(new)`：覆盖 AC1–AC4、AC7（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::cat_file_test::cat_file_type_object_form_rejects_invalid_usage)'` `(new)`：覆盖 AC5–AC6、AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::cat_file_test::/)'`（共享回归组：`command::cat_file_test` 模块族（含 `test_cat_file_mutual_exclusion`）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::cat_file::tests::/)'`（共享回归组：`src/command/cat_file.rs` 参数解析单元测试（含 `test_help_mentions_mode_relationships`，其帮助文本随本卡更新）；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-20`、`DEP-PM-08`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cat_file.rs`, `tests/command/cat_file_test.rs`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/cat_file.rs`, `tests/command/cat_file_test.rs`
**Docs and compatibility impact:** `docs/commands/cat-file.md` 与 zh-CN：新增 `<type> <object>` 形式、剥离规则与错误（同时更新 `[OPTIONS] [OBJECT]` 概要）；`docs/development/commands/cat-file.md` 同步；`COMPATIBILITY.md` `cat-file` 行（W-1，基线第 265 行）原地补记；网站页 `cat-file.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=cat-file类型与对象双参数原始输出; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=8/8; landing=1; prod-files=1; scope=M; deps=PM-20,DEP-PM-08,DEP-PM-11; writeset=串行于 PM-20; release=independent; split-from=N/A; exception=N/A`

---

### Task PM-33: `rev-list --missing` 在部分克隆上按 promisor 归因（`t5616#33`）（#747；自 PM-03 拆出）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `PM-03` 拆出：实现 promisor 归因——在 `DEP-PM-08` 交付的部分克隆中，`--missing=allow-promisor` 跳过 promisor 归因的预期缺失对象（Libra 归因规则——Libra 没有 Git 的 promisor 包标记，故与 Git `is_promisor_object` 按 promisor 包内对象及其直接引用判定不同，属有意差异：缺失对象当且仅当可从某个 `remote.<name>.promisor=true` 远端（`CL-13` 写入的配置）的远端跟踪引用 `refs/remotes/<name>/*` 所指提交到达时视为预期缺失；只被本地新建对象引用的缺失对象按 `error` 以 `LBR-REPO-002` 失败），`--missing=print` 以 `?<oid>` 报告它们；两种动作只以不读取对象内容的 `ClientStorage::exist_checked_many` 判定存在性（`PM-03` 的检查路径），因而不经过 `CL-19` 在读取缺失对象时触发的按需拉取（Git 对这些动作 `fetch_if_missing = 0`）；本卡承诺并验收的可观测结果是：promisor 源可达时缺失对象不会被取回到本地对象库、输出仍报告其缺失，源不可达时也不因拉取失败而报错。
**Out of scope:** 非部分克隆仓库上的四种动作（`PM-03`）；对传输层请求次数的直接计数（需 `CL-19` 提供可观测钩子，尚未排期，重启条件：clone 计划交付拉取计数或 trace 事件）；部分克隆与 `sparse:oid` 传输本身（`plan-20261117-clone.md` 的 `CL-13`/`CL-19`/`CL-29`）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| Libra 尚无 promisor/部分克隆概念；`--filter` 现被忽略 | `src/command/clone.rs:191-198,266-272` |
| `t5616#33`：`clone --filter=sparse:oid=main:only-one` 后 `rev-list --objects --missing=print HEAD` 含 `^<one.t>` 行与 `^?<two.t>` 行 | `git@6de20f6092:t/t5616-partial-clone.sh:457-480` |
| Git：`allow-promisor` 对非 promisor 对象 `die("unexpected missing ...")` | `git@6de20f6092:builtin/rev-list.c:205-239` |
| Git：`is_promisor_object` 只认 promisor 包内对象及其直接引用；删除远端跟踪引用后仍跳过，移除 promisor 远端后则失败 | `git@6de20f6092:packfile.c:2008-2081`；Git 2.54.0 预探 |
| `libra remote prune <name>` 删除失效的远端跟踪引用 | `src/command/remote.rs:1290-1370`；`docs/commands/remote.md` |
| Git：sparse 过滤器省略所有未匹配的 blob（含规格文件 `only-one` 自身） | `git@6de20f6092:list-objects-filter.c:472-539`；Git 2.54.0 预探 |
| bare 仓库中 `libra fast-import` 写入提交与引用（本机实测：写入后 `rev-list --objects refs/heads/local` 列出提交、tree 与 blob） | `src/command/fast_import.rs`；`docs/commands/fast-import.md` |
| Libra 的 `--bare` 克隆仍建立 `refs/remotes/origin/*` 远端跟踪引用（只有 `--mirror` 按原名映射；本机实测 `libra clone --bare` 后 `libra show-ref` 列出 `refs/heads/main`、`refs/remotes/origin/main`、`refs/remotes/origin/HEAD` 等；Git 的 bare 克隆不建），故本卡归因规则在 bare D 上可用；执行时以同一 `libra show-ref` 只读复核 | `src/command/clone.rs:184-188,1610-1613,1981-1987` |

**夹具:** F33：`DEP-PM-08` 中 `CL-29` 交付的 `t5616#33` 同构夹具——完整源仓库 S（含 `one.t`、`two.t`、`only-one`）与其 `--bare` 部分克隆 D（`only-one` 内容为 `/one.t`；只有 `one.t` 在库，`two.t` 与 `only-one` 自身都是预期 promisor 缺失——sparse 规则不选中 `only-one`；无检出，故不会因检出触发按需拉取）；先在 S 中执行 `libra rev-list --objects HEAD` 得到完整对象流 E，克隆前另在 S 建分支 `side` 并提交 Cs（记录其 OID；增加 `three.t`=`three\n`，部分克隆中该 blob 缺失），D 因此带有 `refs/remotes/origin/side`；随后在 S 仍可达时执行 AC3–AC5 的两次调用（并在其前后各记录一次 D 的「本地对象清单」= `D/objects/` 下全部文件「相对路径 → SHA-256」清单（bare 克隆的 `libra.db` 与 `objects/` 位于 D 根部，`docs/commands/clone.md:145-157`、`src/command/clone.rs:1288-1299`；清单前先断言该目录存在）），接着断言 D 中 `libra show-ref` 的输出含 `<Cs 的 OID> refs/remotes/origin/side` 一行（`src/command/show_ref_render.rs:68` 的 `<hash> <refname>` 格式；不满足即测试失败）后执行 AC7 的调用；然后执行 AC8 的准备：在 S 中删除分支 `side`，在 D 中执行 `libra remote prune origin`（删除失效的 `refs/remotes/origin/side`，`remote.origin.promisor` 配置保留），并断言此后 D 中 `libra show-ref` 的输出不含 `refs/remotes/origin/side`（不满足即测试失败）；再把 S 目录改名使 D 的 promisor 源不可达后执行 AC1–AC2。 AC6 在 AC1–AC5 之后执行：在 D 中以 `libra fast-import` 读入 stdin 流 `blob\nmark :1\ndata 6\nlocal\n\ncommit refs/heads/local\nmark :2\ncommitter L <l@x> 1700000000 +0000\ndata 2\nl\nM 100644 :1 local.t\n\n`（bare 仓库无工作区，故以 fast-import 写入只被本地引用 `refs/heads/local` 引用、不来自任何 promisor 拉取的 blob `local.t`），再删除该 blob 的松散对象文件 `D/objects/<OID 前 2 位>/<其余位>`（删除前断言其存在；`tests/command/mod.rs:565` 的 `loose_object_path` 只适用于非 bare 仓库）。

**Acceptance criteria:**
- [ ] **AC1** D 上 `libra rev-list --objects --missing=print HEAD` 的 stdout 等于 E 删去 `<only-one oid> only-one` 与 `<two.t oid> two.t` 两行后，按两者在 E 中的次序追加 `?<only-one oid>` 与 `?<two.t oid>` 两行。
- [ ] **AC2** D 上 `--missing=allow-promisor` 的 stdout 等于 E 删去 `<only-one oid> only-one` 与 `<two.t oid> two.t` 两行。
- [ ] **AC3** S 可达时 D 上 `libra rev-list --objects --missing=print HEAD` 的 stdout 仍等于 AC1 的期望（未按需拉取 `only-one`、`two.t`）。
- [ ] **AC4** S 可达时 D 上 `--missing=allow-promisor` 的 stdout 仍等于 AC2 的期望。
- [ ] **AC5** S 可达时上述两次调用前后 D 的「本地对象清单」逐字相等。
- [ ] **AC6** AC6 夹具上 `libra rev-list --objects --missing=allow-promisor refs/heads/local` 以 `LBR-REPO-002` 失败（非 promisor 归因的缺失对象不被跳过）。
- [ ] **AC7** AC8 准备之前（D 的 `refs/remotes/origin/side` 仍指向 Cs）`libra rev-list --objects --missing=allow-promisor <Cs>` 以 exit 0 成功（`three.t` 可从 promisor 远端跟踪引用到达，按预期缺失跳过）。
- [ ] **AC8** AC8 准备之后（prune 已删除 `refs/remotes/origin/side`）`libra rev-list --objects --missing=allow-promisor <Cs>` 以 `LBR-REPO-002` 失败（Libra 归因规则：`three.t` 已不可从任何 promisor 远端跟踪引用到达；有意差异——Git 2.54.0 预探在同类情形下按 promisor 包跳过并 exit 0）。
- [ ] **文档 AC**（ER-06/ER-06a 同卡强制门，不计入 AC 分子）：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」逐文件验收；文档关键字：`allow-promisor`。

**AC → 断言映射:**（AC 与映射表述同一谓词；成功类映射中的 `exit 0` 为读取输出的前提；失败类映射以退出码非 0 为前提并断言 stderr 含稳定码）
- AC1 → `command::rev_list_test::rev_list_missing_on_partial_clone_fixture`：exit 0；stdout 逐字等于该期望（含 `one.t` 等全部在库对象行；Git 2.54.0 预探同构 `--bare --filter=sparse:oid=main:only-one` 克隆的输出即此形状）
- AC2 → `command::rev_list_test::rev_list_missing_on_partial_clone_fixture`：exit 0；stdout 逐字等于该期望
- AC3 → `command::rev_list_test::rev_list_missing_on_partial_clone_fixture`：exit 0；stdout 逐字等于 AC1 的期望
- AC4 → `command::rev_list_test::rev_list_missing_on_partial_clone_fixture`：exit 0；stdout 逐字等于 AC2 的期望
- AC5 → `command::rev_list_test::rev_list_missing_on_partial_clone_fixture`：两次调用前后「本地对象清单」逐字相等
- AC6 → `command::rev_list_test::rev_list_missing_on_partial_clone_fixture`：退出码非 0（前提）；stderr 含 `LBR-REPO-002`
- AC7 → `command::rev_list_test::rev_list_missing_on_partial_clone_fixture`：退出码为 0（本 AC 的谓词即调用成功）
- AC8 → `command::rev_list_test::rev_list_missing_on_partial_clone_fixture`：退出码非 0（前提）；stderr 含 `LBR-REPO-002`
- 文档 AC → 「文档与网站验收命令」第 1 步至第 7 步

**失败路径不变量:** 以稳定码失败的 AC6、AC8 调用前后 D 的「本地对象清单」逐字相等（被测命令只读）。

**Verification:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::rev_list_test::rev_list_missing_on_partial_clone_fixture)'` `(new)`：覆盖 AC1–AC8（按「验证执行协议」第 1–3 项：先以同一前缀与过滤 `cargo nextest list` 恰列出该 1 个测试，运行后 exit 0、运行 = 通过 = 1、失败 = 0；nextest 摘要的 skipped 含被过滤排除的用例，只记录）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_list_test::/)'`（共享回归组：`command::rev_list_test` 模块族；N 以运行前 list 结果为准（≥ 1）；按「验证执行协议」第 5 项先以同一前缀与过滤 `cargo nextest list` 核对，要求 exit 0、运行 = 通过 = N、失败 = 0，skipped 只记录；不计为卡特有 AC 证据）

**Full-suite trigger:** T-1: 版本面集合同步修改 `install.sh` / `install.ps1`（ER-08；ER-13 T-1）
**Dependencies:** `PM-14`、`DEP-PM-08`、`DEP-PM-11`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rev_list.rs`, `tests/command/rev_list_test.rs`, `docs/commands/rev-list.md`, `docs/commands/zh-CN/rev-list.md`, `docs/development/commands/rev-list.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-list.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/rev_list.rs`, `tests/command/rev_list_test.rs`
**Docs and compatibility impact:** `docs/commands/rev-list.md` 与 zh-CN：部分克隆中 `allow-promisor`/`print` 的 promisor 归因（以远端跟踪引用可达性判定的有意差异）与不触发按需拉取（改写 `PM-03` 文档中「`allow-promisor` 等同 `error`」的说明）；`docs/development/commands/rev-list.md` 同步；`COMPATIBILITY.md` `rev-list` 行（W-1，基线第 218 行）原地补记；网站页 `rev-list.en.md` 同步。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=rev-list --missing部分克隆promisor归因; recovery=推送前 revert 本卡提交；发布后前滚新 patch; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=PM-14,DEP-PM-08,DEP-PM-11; writeset=串行于 PM-14; release=independent; split-from=PM-03; exception=N/A`

---

## 测试矩阵

本表登记计划最终必须覆盖的内容；执行阶段按 ER-13 只跑与本卡相关的行，整表由收口全量门覆盖。

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `replace`/`rev-parse`/`cat-file` 参数解析与 `migration`/`schema` 尖端断言 | 各卡 `--lib` 共享回归组（如 `source .env.test && cargo nextest run --lib -E 'test(/^command::cat_file::tests::/)'`）；`PM-01` 的尖端单元断言由 T-1/T-3 全量覆盖 |
| 集成 | 各命令行为与 Git 2.56（`git@6de20f6092`）对照 | `source .env.test && cargo nextest run --test command_test -E '<各卡精确过滤>'` |
| 兼容 | 冲突索引一致性、帮助 EXAMPLES、命令文档 Examples、兼容矩阵与 CLI 对齐、帮助 flag 描述、版本面 | `compat_conflict_status_diff`、`compat_help_examples_banner`、`compat_command_docs_examples_section`、`compat_matrix_alignment`、`compat_help_flag_descriptions`、`compat_version_surface_sync` |
| 迁移 | `2026111702` 保留既有行、配置库隔离（GC-13）、失败注入、触发器互斥（已应用迁移不重复应用属 runner 通用行为，由 `db_migration_test` 整 target 回归组中的既有 runner 用例守护，`PM-01` 不单独验收） | `source .env.test && cargo nextest run --test db_migration_test -E 'test(=symbolic_ref_migration_upgrade_contract)'` 与 `-E 'test(=symbolic_ref_triggers_enforce_exclusivity)'` |
| 安全 | 越界路径 fail-closed（`PM-17`）；`var -l` 不回显配置（`PM-10` AC3）；非 `refs/notes/` 默认 ref 拒绝（`PM-08`、`PM-34`） | 各卡精确过滤 |
| 性能 | `check-ignore --stdin` 在 EOF 前逐行输出；`t0003#55` 3001 层宏链 | `command::check_ignore_test::check_ignore_stdin_streams_each_line`；`command::check_attr_test::check_attr_expands_macros_with_git_precedence` |
| live/gated | 收口阶段双环境全量 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| PM-01 | `#711` | `sql/migrations/2026111702_symbolic_ref.sql`、`src/internal/db/migration.rs`、`src/internal/db/schema.rs` | 见本卡 `Docs and compatibility impact` | `symbolic_ref_migration_upgrade_contract`、`symbolic_ref_triggers_enforce_exclusivity` |
| PM-21a | `#711` | `src/command/checkout.rs`、`src/command/symbolic_ref.rs`、`src/internal/branch.rs` | 见本卡 `Docs and compatibility impact` | `command::symbolic_ref_test::symbolic_ref_heads_names_read_write_delete`、`command::symbolic_ref_test::checkout_force_symref_branch_twice` |
| PM-32 | `#711` | `src/command/symbolic_ref.rs`、`src/internal/head.rs` | 见本卡 `Docs and compatibility impact` | `command::symbolic_ref_test::symbolic_ref_remote_head_read_write_delete` |
| PM-02 | `#717` | `src/command/config.rs` | 见本卡 `Docs and compatibility impact` | `command::config_test::config_default_implies_get`、`command::config_test::config_default_rejected_with_non_get_actions` |
| PM-03 | `#747` | `src/command/rev_list.rs`、`src/command/rev_list_output.rs` | 见本卡 `Docs and compatibility impact` | `command::rev_list_test::rev_list_missing_actions_on_local_missing_objects` |
| PM-04 | `#758` | `src/command/replace.rs` | 见本卡 `Docs and compatibility impact` | `command::replace_test::replace_list_uses_wildmatch`、`command::replace_test::replace_rejects_invalid_mode_combinations` |
| PM-26 | `#758` | `src/command/replace.rs` | 见本卡 `Docs and compatibility impact` | `command::replace_test::replace_delete_resolves_any_object_short_oid`、`command::replace_test::replace_delete_continues_and_reports_errors` |
| PM-05 | `#839` | `src/command/rev_parse.rs` | 见本卡 `Docs and compatibility impact` | `command::rev_parse_test::rev_parse_short_abbreviates_absent_full_oid`、`command::rev_parse_test::rev_parse_short_rejects_invalid_specs` |
| PM-06 | `#947` | `src/command/rev_parse.rs` | 见本卡 `Docs and compatibility impact` | `command::rev_parse_test::rev_parse_show_object_format_modes`、`command::rev_parse_test::rev_parse_show_object_format_rejects_invalid_mode` |
| PM-07 | `#848` | `src/command/blame.rs`、`Cargo.toml` | 见本卡 `Docs and compatibility impact` | `command::blame_test::blame_minimal_matches_git_attribution`、`command::blame_test::blame_minimal_uses_shortest_edit_script` |
| PM-08 | `#988` | `src/command/notes.rs` | 见本卡 `Docs and compatibility impact` | `command::notes_test::notes_default_ref_follows_config_scopes`、`command::notes_test::notes_config_ref_outside_refs_notes` |
| PM-34 | `#988` | `src/command/notes.rs` | 见本卡 `Docs and compatibility impact` | `command::notes_test::notes_default_ref_follows_env`、`command::notes_test::notes_env_ref_outside_refs_notes` |
| PM-09 | `#989` | `src/command/notes.rs`、`src/internal/notes.rs` | 见本卡 `Docs and compatibility impact` | `command::notes_test::notes_merge_manual_conflict_materializes_state`、`command::notes_test::notes_merge_refuses_while_state_or_lock_exists` |
| PM-29 | `#989` | `src/command/notes.rs`、`src/internal/notes.rs` | 见本卡 `Docs and compatibility impact` | `command::notes_test::notes_merge_commit_applies_resolution`、`command::notes_test::notes_merge_commit_cas_and_errors` |
| PM-38 | `#989` | `src/command/notes.rs`、`src/internal/notes.rs` | 见本卡 `Docs and compatibility impact` | `command::notes_test::notes_merge_commit_retry_after_crash`、`command::notes_test::notes_merge_commit_retry_refuses_unconfirmed_rows` |
| PM-23 | `#989` | `src/command/notes.rs`、`src/internal/notes.rs` | 见本卡 `Docs and compatibility impact` | `command::notes_test::notes_merge_abort_and_quit_discard_state`、`command::notes_test::notes_merge_abort_errors_and_lock` |
| PM-39 | `#989` | `src/command/notes.rs` | 见本卡 `Docs and compatibility impact` | `command::notes_test::notes_merge_mode_validation` |
| PM-24 | `#989` | 版本面集合、`Cargo.lock`、网站 `notes.en.md` | 见本卡 `Docs and compatibility impact` | 家族聚合守卫（`command::notes_test` 模块族） |
| PM-10 | `#995` | `src/command/var.rs`、`src/command/mod.rs`、`src/cli.rs` | 见本卡 `Docs and compatibility impact` | `command::var_test::var_ident_list_and_json`、`command::var_test::var_rejects_invalid_usage_and_missing_identity` |
| PM-10a | `#995` | `src/command/var.rs`、`src/command/commit.rs` | 见本卡 `Docs and compatibility impact` | `command::var_test::var_author_ident_env_overrides` |
| PM-37 | `#995` | `src/command/var.rs` | 见本卡 `Docs and compatibility impact` | `command::var_test::var_committer_ident_env_overrides` |
| PM-22 | `#995` | `src/command/var.rs` | 见本卡 `Docs and compatibility impact` | `command::var_test::var_editor_variable` |
| PM-36 | `#995` | `src/command/var.rs` | 见本卡 `Docs and compatibility impact` | `command::var_test::var_sequence_editor_variable` |
| PM-11 | `#1017` | `src/command/ls_files.rs` | 见本卡 `Docs and compatibility impact` | `command::ls_files_test::ls_files_others_excludes_unmerged_paths` |
| PM-12 | `#1022` | `src/command/ls_files.rs` | 见本卡 `Docs and compatibility impact` | `command::ls_files_test::ls_files_lists_each_unmerged_stage` |
| PM-15 | `#1033` | `src/utils/attributes.rs` | 见本卡 `Docs and compatibility impact` | `command::check_attr_test::check_attr_expands_macros_with_git_precedence` |
| PM-27 | `#1033` | `src/command/check_attr.rs` | 见本卡 `Docs and compatibility impact` | `command::check_attr_test::check_attr_rejects_invalid_attribute_names` |
| PM-28 | `#1033` | `src/utils/attributes.rs`、`src/command/check_attr.rs` | 见本卡 `Docs and compatibility impact` | `command::check_attr_test::check_attr_all_uses_registration_order` |
| PM-18 | `#1033` | `src/utils/util.rs`、`src/utils/attributes.rs` | 见本卡 `Docs and compatibility impact` | `command::check_attr_test::check_attr_global_sources_follow_git`、`command::check_ignore_test::check_ignore_global_sources_follow_git` |
| PM-25 | `#995` | `src/command/var.rs` | 见本卡 `Docs and compatibility impact` | `command::var_test::var_environment_variables_and_full_list` |
| PM-16 | `#1034` | `src/command/shortlog.rs` | 见本卡 `Docs and compatibility impact` | `command::shortlog_test::shortlog_output_matches_git_format`、`command::shortlog_test::shortlog_stdin_output_matches_git_format` |
| PM-30 | `#1034` | `src/command/shortlog.rs` | 见本卡 `Docs and compatibility impact` | `command::shortlog_test::shortlog_group_options_match_git`、`command::shortlog_test::shortlog_group_dedups_within_commit` |
| PM-19 | `#1034` | `src/cli.rs`、`src/command/shortlog.rs` | 见本卡 `Docs and compatibility impact` | `command::shortlog_test::shortlog_max_count_shorthand` |
| PM-40 | `#1034` | `src/command/shortlog.rs`、`src/command/check_mailmap.rs` | 见本卡 `Docs and compatibility impact` | `command::shortlog_test::shortlog_and_check_mailmap_apply_mailmap` |
| PM-35 | `#1034` | `src/command/shortlog.rs` | 见本卡 `Docs and compatibility impact` | `command::shortlog_test::shortlog_format_groups_and_date`、`command::shortlog_test::shortlog_format_groups_apply_mailmap` |
| PM-17 | `#1035` | `src/command/check_ignore.rs` | 见本卡 `Docs and compatibility impact` | `command::check_ignore_test::check_ignore_resolves_paths_from_cwd` |
| PM-31 | `#1035` | `src/command/check_ignore.rs` | 见本卡 `Docs and compatibility impact` | `command::check_ignore_test::check_ignore_stdin_streams_each_line`、`command::check_ignore_test::check_ignore_stdin_c_quoting` |
| PM-20 | `#1035` | `src/command/check_ignore.rs` | 见本卡 `Docs and compatibility impact` | `command::check_ignore_test::check_ignore_quiet_and_argument_rules` |
| PM-14 | `#1029` | `src/command/cat_file.rs` | 见本卡 `Docs and compatibility impact` | `command::cat_file_test::cat_file_type_object_form_prints_raw_bytes`、`command::cat_file_test::cat_file_type_object_form_rejects_invalid_usage` |
| PM-33 | `#747` | `src/command/rev_list.rs` | 见本卡 `Docs and compatibility impact` | `command::rev_list_test::rev_list_missing_on_partial_clone_fixture` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划文本 Claude 与 Codex 双审 PASS（同一 SHA-256）；`DEP-PM-01` 基线核实 | 本计划 Codex review log 与修订历史 | 修改计划文档 |
| M1 | Phase 1 全部卡 `done`/`complete` | 各卡 release tag 与 D 组记录 | 前滚新 patch；`PM-01` 之后已迁移仓库不降级到 `PM-01` 之前的二进制 |
| M2 | Phase 2 全部卡 `done`/`complete`（`PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39` 由 `PM-24` 的 release 覆盖） | 各卡 release tag；`PM-24` 的 T-2 全量记录 | 发布前按 `REL-PM-01` 逆序 revert；发布后前滚 |
| M3 | Phase 3 全部卡 `done`/`complete`，全量收口门全绿 | 全量 nextest 记录写入「实施证据汇总」 | 前滚修复；越界缺陷建 `FIX-*` |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `PM-01` 迁移执行中断 | 迁移事务未提交（`schema_versions` 无 `2026111702`） | 重新打开仓库由 runner 重试，幂等完成 | 半建表、越过 `I1048-05` 版本、触及 global/system 配置库 |
| 实体写入与符号引用同名（任意写入口） | 无（触发器使该语句失败） | 用户先 `libra symbolic-ref -d refs/heads/<name>` 再重试；`op restore` 遇冲突同理 | 同名实体分支行与符号引用行并存 |
| `notes merge` 冲突状态创建中途崩溃 | 只有 `NOTES_MERGE_WORKTREE/`（无 `STATE`）与陈旧 `NOTES_MERGE_LOCK` | 确认无相关进程后手工删除陈旧 `NOTES_MERGE_LOCK`，再 `libra notes merge --abort`（或 `--quit`）清理 | 新合并覆盖残留；`notes` 表被修改 |
| `notes merge --commit` 建立提交开始标记后、事务提交前崩溃 | 完整状态文件、`NOTES_MERGE_COMMITTING` 与陈旧 `NOTES_MERGE_LOCK` | 确认无相关进程后手工删除陈旧锁，再重试 `--commit`：各行当前值已等于目标值（如解决结果全部等于前值且无待复制条目）则只清理并成功，否则以 `LBR-CONFLICT-002` 拒绝（无法区分未提交与已提交后被改回），用户 `--abort` 后重新合并 | 重放写入 |
| `notes merge --commit` 事务提交后、删除 `STATE` 前崩溃 | 完整状态文件、`NOTES_MERGE_COMMITTING` 与陈旧 `NOTES_MERGE_LOCK` | 确认无相关进程后手工删除陈旧锁，再重试 `--commit`：各行仍为目标值则只清理并成功；崩溃后已被他人改动则以 `LBR-CONFLICT-002` 拒绝并保留状态，用户 `--abort` | 重复写入、覆盖崩溃后他人的写入或回退已提交的 notes |
| `notes merge --commit` 删除 `STATE` 后、清理完成前崩溃 | `NOTES_MERGE_WORKTREE/` 与/或 `NOTES_MERGE_COMMITTING`，以及陈旧 `NOTES_MERGE_LOCK` | 确认无相关进程后手工删除陈旧 `NOTES_MERGE_LOCK`，再 `--abort` 清理（notes 已提交，不受影响） | 残留被当作进行中的合并再次提交 |
| `notes merge --commit` 期间他人把受影响行改成既非前值也非目标值 | 状态保留 | 核对全部行后发现即不写入任何行、以 `LBR-CONFLICT-002` 失败并回滚事务；用户 `--abort` 后重新合并。按 ADR-PM-02 的行级 CAS，他人恰好写入目标值的行视为已完成而跳过，改动本次合并不涉及的行不构成冲突 | 覆盖他人写入或部分应用 |
| 持有 `NOTES_MERGE_LOCK` 的进程崩溃 | 陈旧 `NOTES_MERGE_LOCK` | 后续 `notes merge` 各形式以 `LBR-CONFLICT-002` 拒绝并提示路径；用户确认无相关进程后手工删除该文件 | 自动抢占他人持有的锁 |
| D 组 CI/release 失败 | 已推送主分支提交 | 按 ER-04/ER-10 前滚修复并发新 patch | 回退或强推已发布历史 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `2026111702` 与 `I1048-05` 或其它新迁移版本冲突，或已迁移仓库被旧二进制打开 | 高：迁移被跳过或 `UnsupportedFuture` | `DEP-PM-05` 重排规则；`COMPATIBILITY.md` 与 `_compatibility.md` 记录不可降级 | `PM-01` |
| 触发器使既有写入口（提交到符号引用分支、`op restore` 等）在罕见同名场景下失败 | 中：操作 fail-closed | 文档说明并给出恢复动作；完整支持记入 `DEFER-PM-02` | `PM-01`、`PM-21a` |
| 符号引用分支解析波及 `checkout` 之外的调用方 | 中：其它命令看到符号引用分支 | 新增 helper 只由 `checkout` 调用，既有 `Branch::find_branch_result*` 不变；T-1 全量门；`checkout_test` 回归组 | `PM-21a` |
| 同一工作区并发执行 `notes merge` 的不同形式 | 中：可能互删状态文件 | `NOTES_MERGE_LOCK` 排他互斥（ADR-PM-02）；数据库侧另有 CAS | `PM-09`、`PM-29`、`PM-38`、`PM-23`、`PM-39` |
| `--commit` 崩溃后他人改写 notes，重试覆盖其写入 | 高：静默丢失写入 | 提交开始标记下只确认不重放，无法确认即拒绝（ADR-PM-02） | `PM-29`、`PM-38` |
| 属性引擎改写影响 merge/diff/lfs/status | 高：共享单一事实源 | T-1 全量；`PM-15` 的调用方回归组 | `PM-15`、`PM-28`、`PM-18` |
| `shortlog` 人读格式变更影响依赖旧格式的脚本 | 中 | ADR-PM-04；`--json` 不变；文档与兼容矩阵说明 | `PM-16` |
| 新命令 `var` 与同批计划同时修改 `src/cli.rs`、`tests/command/mod.rs` | 中：写集冲突 | `DEP-PM-13` 独占写锁 | `PM-10`、`PM-19` |
| `similar` 版本要求提升影响其它 diff 调用 | 低：`Cargo.lock` 已为 3.2.0，`Myers` 路径不变 | T-1 全量 | `PM-07` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `check-ignore --stdin` | O(行长 × 规则数) | 逐行处理（不整体缓存由 ER-05 评审核对） | stdin 仍打开时每行即输出 | `PM-31` AC1–AC2 |
| 属性宏展开 | O(展开属性数) | 与宏链深度线性 | `t0003#55` 的 3001 层宏链正常完成 | `PM-15` AC8 |
| `rev-list --missing` 存在性检查 | 批量、有界并发（`exist_checked_many`） | 与对象数线性 | promisor 源不可达时仍正确输出 | `PM-33` AC1–AC2 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md`：各卡按 GC-PM-02 原地更新对应命令行，`PM-10` 新增 `var` 行。
- [ ] `docs/commands/<cmd>.md` 与 `docs/commands/zh-CN/<cmd>.md`：各卡同步；`PM-10` 新建 `var.md` 两份。
- [ ] `docs/development/commands/<cmd>.md`：各卡同步；`PM-01` 更新 `_compatibility.md`；`PM-10` 更新 `README.md`。
- [ ] `docs/error-codes.md`：`N/A`（不新增稳定码）。
- [ ] `tests/INDEX.md`：`N/A`（不新增 `--test` target；`var_test` 属既有 `command_test`，`tests/INDEX.md:44` 为整 target 描述）。
- [ ] `Cargo.toml` `[[test]]`：`N/A`（不新增 target）。
- [ ] 网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`（`PM-10` 另含 `meta.en.json`、`index.en.mdx`）：按「文档与网站验收命令」同步。
- [ ] `plan-long.md` 与 `plan-status.md`：由协调者登记，执行期按「使用规则」同步。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`（历史行中的 `ERROR` 为当时网络中断的原样记录）。R1–R15 为 2026-11-17 的历史轮次，原样保留；R16–R18 的评审记录未写入本表（修订历史 2026-11-18 08:19:08 与 09:43:54 两行只记载其修复），R19 起按 2026-11-18 双审合同逐轮登记 Claude 与 Codex 两行，`Evidence` 列记受审 SHA-256。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
| R1 (Claude) | `plan-20261117-plumbing-misc.md` 初稿 | `FAIL` | 7 项 P1（`PM-01` schema 约束、`PM-03` `allow-any`/`allow-promisor`、`DEP-PM-02` 去环、`PM-09` `internal/notes.rs` 与 `NOTES_MERGE_PARTIAL`、`PM-10` `2/3`+`T-4`+`INDEX.md`、`PM-13` 附注 tag+`--points-at`、拆出 `PM-18`/`19`/`20`）均在 R2 关闭 | P2（全部精确锚点、`PM-02` 定向测试、`Phase 2`、`review log`）在 R2 关闭 | Claude R1 review |
| R1 (Codex) | `plan-20261117-plumbing-misc.md` 初稿 | `FAIL` | P1（`PM-01`、`PM-15`/`16`/`17` 拆卡、`shortlog.rs:157,190` 基线、精确 `file:line` 锚点）均在 R2 关闭 | — | Codex R1 review |
| R2 (Claude) | `plan-20261117-plumbing-misc.md` R2 | `FAIL` | 2 项 P1（`PM-01`/`09` `T-1`、`PM-09`/`ADR-PM-02` SQLite `notes` 表事务）均在 R3 关闭 | P2（`M3`、`Release write set`、`PM-10` AC/守卫、`PM-15/18/17` 收窄、`PM-03`/`01`/`DEP-PM-03/04`/发布者）在 R3 关闭 | Claude R2 review |
| R2 (Codex) | `plan-20261117-plumbing-misc.md` R2 | `FAIL` | 2 项 P1（`#711` `refs/heads/a-branch` GC-13 迁移、`PM-16` 重复 `--group` 各自贡献）均在 R3 关闭 | P2（`M3` `PM-11..PM-20`）在 R3 关闭 | Codex R2 review |
| R3 (Claude) | `plan-20261117-plumbing-misc.md` R3 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R4 重审 | — | Claude R3 review |
| R3 (Codex) | `plan-20261117-plumbing-misc.md` R3 | `FAIL` | 3 项 P1（`PM-01` GC-13 迁移全链路接线与自相矛盾修复、`PM-03` 预期 promisor 对象校验、`PM-09` 已提交未清理崩溃窗口持久完成标记与幂等恢复）均在 R4 关闭 | — | Codex R3 review |
| R4 (Claude) | `plan-20261117-plumbing-misc.md` R4 | `FAIL` | 5 项 P1（`PM-01` 拆 `migration` 卡与正确 `sql/migrations/` 路径及 `PM-21`+`branch.rs`、`PM-10` 拆出 `PM-22` 守 `AC<=8`、`PM-09` 崩溃窗口具名测试与 `manual` 兼容性）均在 R5 关闭 | P2（`PM-13/15/19`、`默认覆盖`、`DEP-PM-03`、`测试矩阵`、`性能`）在 R5 关闭 | Claude R4 review |
| R4 (Codex) | `plan-20261117-plumbing-misc.md` R4 | `FAIL` | 3 项 P1（`PM-09` 同事务 SQLite 回执消除崩溃窗口、`PM-01` GC-13 `Repository` 白名单隔离与 `symbolic_ref` 独立表不改 `reference` 实体）均在 R5 关闭 | — | Codex R4 review |
| R5 (Claude) | `plan-20261117-plumbing-misc.md` R5 | `FAIL` | 3 项 P1（`PM-01` 改 `2026111702` 避开 `1048.md`、`PM-21` 自依赖修复、`PM-10`/`PM-22` 重分使 `PM-10` 自足）均在 R6 关闭 | P2（`T-1, T-3`、`README.md`、`DEFER-PM-02`、`DEP-PM-05/06/07`、`PM-11`/`15`/`02`/Phase 3）在 R6 关闭 | Claude R5 review |
| R5 (Codex) | `plan-20261117-plumbing-misc.md` R5 | `FAIL` | 2 项 P1（`PM-21` 自依赖修复、`PM-09` 明确 `config` 表 `notes.merge.last_commit_receipt` 键）均在 R6 关闭 | P2（`PM-01` 标 `(new)` 测试）在 R6 关闭 | Codex R5 review |
| R6 (Codex) | `plan-20261117-plumbing-misc.md` R6 | `FAIL` | 1 项 P1（`ADR-PM-02`/`PM-09` 改用按工作区与唯一 `merge_id` 作用域化的回执键并补并发恢复测试）在 R7 关闭 | P2（`Phase 1` 退出条件补 `PM-21`）在 R7 关闭 | Codex R6 review |
| R6 (Claude) | `plan-20261117-plumbing-misc.md` R6 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R8 重审 | — | Claude R6 review |
| R7 (Codex) | `plan-20261117-plumbing-misc.md` R7 | `FAIL` | 2 项 P1（`ADR-PM-02`/`PM-09` 清理顺序改为先删 `WORKTREE`/`REF` → 再删 DB 回执 → 最后删 `STATE` 避免孤立 DB 行；`PM-21` 显式收窄范围至 `symbolic-ref` + `checkout -f`，不宣称 `show-ref`/`for-each-ref`/`branch` 列表读取）均在 R8 关闭 | — | Codex R7 review |
| R8 (Codex) | `plan-20261117-plumbing-misc.md` R8 | `FAIL` | 2 项 P1（`ADR-PM-02`/`PM-09` 增加 `phase=pending\|committed` 原子持久化区分未提交缺失与已提交清理；明确 `--quit` 在提交清理窗口同步删除 DB 回执不留孤立行）均在 R9 关闭 | — | Codex R8 review |
| R9 (Claude) | `plan-20261117-plumbing-misc.md` R9 | `FAIL` | 4 项 P1（`DEP-PM-05` `I1048-05 -> PM-01` 有向边与重编号、`PM-03` 增 `DEP-PM-08` 对接 `CL-13/19`、`PM-05` 对齐 `#839` 缩写未入库 OID、`PM-19` 标 `T-1` 与 `check-mailmap` 文档测试）均在 R10 关闭 | P2（`PM-08` `internal/notes.rs`、`#717`/`#758`/`#988`/`#989`/`#995`/`#1020`/`#1029`、`R10`/时间正序、`codeql.yml` `release/**`、`DEP-PM-03/04/06/07`）在 R10 关闭 | Claude R9 review |
| R9 (Codex) | `plan-20261117-plumbing-misc.md` R9 | `FAIL` | 1 项 P1（`--quit` 在 `phase=pending`+DB 回执窗口先原子写 `phase=committed` 再删回执并补崩溃测试）在 R10 关闭 | P2（行 5 `R10` 与修订历史时间正序）在 R10 关闭 | Codex R9b review |
| R10 (Claude) | `plan-20261117-plumbing-misc.md` R10 | `FAIL` | 2 项 P1（移除造成 `PM-06`/`CL-14` 环路的 `DEP-PM-04`、拆出 `PM-23` 与调整 `PM-10/22` 守住 `AC<=8`）均在 R11 关闭 | P2（`codeql.yml`、`PM-01` `schema.rs`/`2026111601`、`DEP-PM-07`、`PM-03/13/19` AC 计数、`DEFER-PM-02/03`）在 R11 关闭 | Claude R10 review |
| R10 (Codex) | `plan-20261117-plumbing-misc.md` R10 | `FAIL` | 1 项 P1（`ADR-PM-02`/`PM-09` 增加 `base_notes_ref_oid` 事务内 CAS 检查与测试）在 R11 关闭 | P2（移除 `PM-09/13/14` 卡内 `outgoing` `DEP-PM-07`）在 R11 关闭 | Codex R10 review |
| R11 (Claude) | `plan-20261117-plumbing-misc.md` R11 | `ERROR` | 网络中断 `ECONNRESET` | — | Claude R11 review |
| R11 (Codex) | `plan-20261117-plumbing-misc.md` R11 | `FAIL` | 1 项 P1（`notes merge` CAS 缺乏 commit-backed tree 值，R12 改用 `SELECT object, blob FROM notes WHERE notes_ref = ? ORDER BY object` 的确定性 `base_notes_digest` 与 `BEGIN IMMEDIATE` 事务内比对 + 并发 `add/remove` 测试）在 R12 关闭 | P2（`PM-21` `AC=4/8` 修正为 `5/8`）在 R12 关闭 | Codex R11 review |
| R12 (Claude) | `plan-20261117-plumbing-misc.md` R12 | `ERROR` | 网络中断 `ECONNRESET` | — | Claude R12 review |
| R12 (Codex) | `plan-20261117-plumbing-misc.md` R12 | `FAIL` | 1 项 P1（`ADR-PM-02`/`PM-09` 读取目标 notes 与计算 `base_notes_digest` 绑定同一 SQLite `BEGIN` 读事务快照并补并发测试）在 R13 关闭 | P2（`评审结论` 同步为 23 张卡）在 R13 关闭 | Codex R12 review |
| R13 (Claude) | `plan-20261117-plumbing-misc.md` R13 | `ERROR` | 网络中断（未成行） | — | — |
| R13 (Codex) | `plan-20261117-plumbing-misc.md` R13 | `FAIL` | 1 项 P1（`PM-18` `core.attributesFile` 显式配置时不叠加加载 XDG 默认路径、仅作未配置回退并补断言）在 R14 关闭 | — | Codex R13 review |
| R14 (Claude) | `plan-20261117-plumbing-misc.md` R14 | `ERROR` | 网络中断（与 R13 同次未成行） | — | — |
| R14 (Codex) | `plan-20261117-plumbing-misc.md` R14 | `FAIL` | 1 项 P1（统一 `PM-01` 迁移后的发布后恢复指引，禁止已迁移 `2026111702` 仓库降级至 `PM-01` 前旧二进制）在 R15 关闭 | P2（`ADR-PM-02`/`PM-09` 补充混合冲突/非冲突条目保留提交与验收）在 R15 关闭 | Codex R14 review |
| R15 (Claude) | `plan-20261117-plumbing-misc.md` R15 | `FAIL` | 6 项 P1（`PM-15`/`18` 纳入 `src/utils/attributes.rs`、`T-1` 与 `DEP-PM-09`；`DEP-PM-06` `WC-10` 改 `outgoing` 去环；`PM-19` 纳入 `src/cli.rs` 与 `T-1`；`PM-10` 补 `docs/development/commands/README.md`；新增 `DEP-PM-10` 协调 `GII-12`；`PM-22` 拆出 `PM-25` 且 `PM-01` 核实 `AC=6/8`）均在 R16 关闭 | P2（`symbolic_ref_delete_matrix`、`PM-15/19` 审计计数、`DEP-PM-03`、`PM-20` 全局 `-q`、`PM-05`、`PM-10` 网站索引、`兼容收口`、`故障恢复`/`风险`）在 R16 关闭 | Claude R15 review |
| R15 (Codex) | `plan-20261117-plumbing-misc.md` R15 | `FAIL` | 3 项 P1（`DEP-PM-08` 明确 `CL-19 -> PM-03` 及 `CL-19 -> PM-14` 无环；`PM-09`+`PM-23` 纳为 `REL-PM-01` 家族卡与 `PM-24` 发布点；`PM-01` 补 `COMPATIBILITY.md`/`_compatibility.md` 降级不兼容与恢复文档）均在 R16 关闭 | P2（`PM-15` `AC=2/8` 同步）在 R16 关闭 | Codex R15 review |
| R19 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R19 预审重构后） | `FAIL` | P1：`PM-09` 新稳定码与未完结拒绝会打破两个既有 notes 合并用例；`PM-03` 本地存在性检查不适用分层存储；`PM-29` 崩溃窗口夹具不可重现；5 处锚点偏差与依赖 crate 路径不可解析；`PM-18` XDG 须在仓库外；另同意 Codex 的夹具与计数问题。R20 全部关闭 | P2：`PM-07` 需写明 `Cargo.toml` 的 I–R 处理（R20 关闭） | 受审 SHA-256 `c9b7ae198d60b45a252a553f3941cae650e8cd271d706768372ac37128bbbb21` |
| R19 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：家族 `COMPATIBILITY.md` 写锁死锁、notes 状态无锁交错、新建文档改动前哈希、`PM-01` 网站时序矛盾、性能摘要不可证、`DEP-CS-09` 描述过时；B：`PM-01` 漏列尖端断言与文档门冲突、多卡 AC 低估（表驱动合并）、`PM-21a` 远端 HEAD 删除无验收、`PM-26` 不在库 OID 不可区分、`PM-03` 部分克隆比对可漏对象、`PM-08` 缺本地/全局优先级；C：`PM-29` 崩溃夹具、`PM-11` 已提交 `.gitignore`、`PM-12` 遗漏 `tracked.txt`、`PM-10` 新建文档哈希/AC4 计数/`-l` 时间窗；D：`PM-20` 与既有 `-z` 用例冲突、`PM-30` 跨分组去重、`PM-16` JSON 保持无基准、跨卡夹具引用、`PM-27` 规模。R20 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R20 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R20） | `FAIL` | P1：ADR-PM-03/ADR-PM-05 与 R20 串行链不一致；`DEP-PM-05`/`DEP-PM-07` 文字过时；`PM-29`/`PM-23` 锁失败未断言锁文件保持；`PM-18` 期望路径未规范化；`PM-10` Out of scope 断句；另同意 Codex R20 全部 P1。R21 全部关闭 | 无 | 受审 SHA-256 `4624b50b42f22ab2400d63b412045d8c70572f9afca7228436d2644e71a5242a` |
| R20 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：测试矩阵迁移用例名错、共享写锁等待者互相阻塞、ADR-PM-05 依赖与 DAG 矛盾；B：`PM-01` AC7/AC8 合并多谓词、`PM-08` 漏系统级、notes 快照缺 `id`、`PM-09` AC4 只核二元组；C：`PM-24` 把强制门计入 AC/VER、`PM-10a` 精确期望缺 `\n`；D：`PM-30` 提前写 `PM-35` 文档、`PM-19` 的 `-n` 不可区分、`PM-16` JSON 承诺矛盾且 stdin 未覆盖、`PM-35` 有意差异不可验证、`PM-14` 未测标签剥离、`PM-19` 引用错卡。R21 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R21 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R21） | `FAIL` | P1：`PM-29` 夹具「每个用例先执行合并」与 AC6「无合并状态」矛盾；F19m/F19f/F35m 未写明 `.mailmap` 不提交（提交会多出一次提交并使全部预探期望失效）。R22 全部关闭 | P2：F16o 作者邮箱未写明（R22 补齐） | 受审 SHA-256 `493600f5bfdc0bcd8d06c4bffa182cd966317128eeecb592d9f76fafac406b2e` |
| R21 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：ADR-PM-02 崩溃窗口与故障恢复矩阵漏陈旧锁清理；B：`PM-01` 缺 GC-13 隔离配置库验收与 G-11 失败注入；C：`PM-23` 误拒 `-s <notes-ref>` 策略合并、`PM-29` AC2 与 `PM-23` AC1 合计两路径、`PM-24` 在提交前记录 HEAD、`PM-24` 缺 release note；D：`PM-25`「实际分页器」与自动模式不符、`PM-25` AC8 跨调用时间戳不确定、`PM-33` 无法证明未按需拉取。R22 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R22 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R22） | `PASS` | 无 | P2：`PM-24` release note 验收只核固定字面，内容质量交 ER-05 评审 | 受审 SHA-256 `541d114b7e7cca99201630a57b688fbf5617a79d3034d4403345fff5bdbd5dc5` |
| R22 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：「跳过 = 0」会拒绝成功的过滤运行（skipped 含过滤排除）、开头活动卡数仍写 35；B：`PM-01` 失败注入夹具停在 `2026103101`，前序 `2026111701` 独立提交会破坏快照相等；C：`PM-10` 复用已读环境变量的解析器与 `PM-10a` 的交付边界矛盾、作者查询复用双签名函数会受非法 `GIT_COMMITTER_DATE` 影响、`PM-24` 祖先检查未进 Verification；D：`PM-28` 把登记次序等同于优先级次序（Git 先读 `info/attributes` 再读子目录）。R23 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R23 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R23） | `PASS` | 无 | P2：`PM-10a`/`PM-37` 把邮箱链其余链位与 `user.useConfigOnly` 交 `commit_test` 回归组守护（已写入 Out of scope） | 受审 SHA-256 `546f780cd61713d0872c686055c6a39ea47f6d426de0757d1fa343e44b4b0c6e` |
| R23 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL`（A/B/D FAIL，C PASS） | A：失败类映射只断言稳定码、退出码为 0 也能通过；网站第 6 步恢复无法处理父提交中不存在的新建页面；B：`PM-01` 缺 `compat_serial_registry` 回归门、AC2 映射漏「重开连接」；D：`PM-31`/`PM-20` 指定的 `run_libra_command_with_env` 无法提供 stdin。R24 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R24 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R24） | `PASS` | 无 | 无 | 受审 SHA-256 `b42908c6a2a3e377b025dd9016e7672ee0c1499e8c30c85f376c21c116c0fe1c` |
| R24 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL`（A/B/C FAIL，D PASS） | A：`--commit` 崩溃后重放 CAS 会覆盖他人改回前值的写入、`DEP-PM-03` 所列卡集与 merge-pull 计划不符；B：`PM-01` 一条命令两个 `--test` 应分别计门（VER 超限）、`PM-03` 独立发布承诺 promisor 行为与 `PM-33` 实施范围冲突；C：家族子卡把网站文档同步推迟到发布点，违反 ER-06a 同卡交付。R25 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R25 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R25 重发版） | `PASS` | 无 | 无 | 受审 SHA-256 `19377e9f9b7ec530e3c1441491e592e2285034df69b510d2cba38cf2ddc02921`（首次发出的 `42c8e10d6fdb…` 因送审前自查修正 VER 计数而中止） |
| R25 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：`PM-24` 复跑第 3 步无变化却要求哈希改变、家族子卡第 3 步要求文档已合入 `main`；B：`PM-01` 两个新用例的 `#[serial(env)]` 要求与第 7 项「唯一例外」矛盾；C：`PM-23` AC3 二元组合并两个谓词、部分残留场景未验 notes 不变、`PM-10` 时区只校验格式；D：`PM-33` 无法观测是否调用了按需拉取。R26 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R26 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R26 重发版） | `PASS` | 无 | 无 | 受审 SHA-256 `7ae68654b24bd35fe9bce9f1430e7f41de5ccb9b7a42a13713840e26b91428c3`（首次发出的 `201870258336…` 因送审前自查修正 `PM-24` 串行前驱而中止） |
| R26 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：建标记后事务前崩溃的矩阵结论与确认算法矛盾、`PM-24` 复核对哈希不等同时要求 STOP 与继续、默认覆盖表 release note 编号错；B：`PM-21a` 改共享 `find_branch_result*` 会波及 `rev-parse`；C：同 A 的复核矛盾、`PM-10a` 提取作者入口可能改变 `commit` 错误优先顺序而无比较验收；D：`PM-33` 缺非 promisor 缺失对象的负例、`PM-14` AC2 文本缺 `\n`。R27 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R27 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R27 重发版） | `PASS` | 无 | 无 | 受审 SHA-256 `1eb04c9c51b4f499d3571563cbbf15ad8eba0b4a0139f31f181c841d81e38725`（首次发出的 `7a0d7d524bad…` 因送审前自查修正 `PM-38` 夹具范围而中止） |
| R27 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：适用范围的源码清单漏列卡片要改的文件；B：notes 卡 blob 期望带尾随换行而 `-m` 实际不带、`PM-21a` 检出符号引用分支会走分离路径而 AC 无法区分；C：`PM-38` F38b 成功清理未验 notes 不变；D：`PM-33` 把远端跟踪引用可达性误称为 Git 归因规则、`PM-18` AC5 未保证全局规则生效 | P2：全局家族子卡列表过时（R28 一并修正） | 受审 SHA-256 同上（四个分区均针对该字节） |
| R28 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R28） | `PASS` | 无 | 无 | 受审 SHA-256 `015da348a5531cb48d7ee2f1ff1dfaabf7e72718e2635e89f3a4df17510e9052` |
| R28 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：恢复矩阵对「期间他人修改」承诺一律拒绝，与 ADR-PM-02 行级 CAS 矛盾；B：`PM-03` 夹具未保证三个文件 blob 不同；C：`PM-24` 祖先门输入说明漏 `PM39`；D：`PM-16` 渲染层排序与 `--reverse`/`--top` 的既有次序冲突。R29 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R29 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R29） | `PASS` | 无 | 无 | 受审 SHA-256 `ca40bf1c040c515a4c32bb67618374f45ad265a4eb2ccfaee52e74b41e6849e5` |
| R29 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：「配置快照」漏全局 `config` 表与系统库、W-2 的 `HTTPError` 退出码与说明不符；B：`PM-07` 只测算法选择函数，无法证明 blame 实际改用 `RawMyers`；C：`PM-38`「不写任何行」无法被快照比较观测、`PM-29` 把选项互斥指向 `PM-23`；D：`PM-33` 称 Libra 无法删除远端跟踪引用，与 `remote prune` 事实不符。R30 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R30 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R30） | `PASS` | 无 | 无 | 受审 SHA-256 `cfea01490e309450904782cccfc058421420e7dd604ed00d8c2db9387b444891` |
| R30 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL` | A：`PM-21a` 改变 `checkout` 行为却未同卡更新 `checkout` 文档；B：`PM-01` 要求同步的 `sql/migrations/README.md` 不在逐文件文档验收内；C：`PM-10` 把 `GIT_SEQUENCE_EDITOR` 指向 `PM-22`、`PM-10a`/`PM-37` 的 `useConfigOnly` 表述与复用的日期解析冲突；D：`PM-27` 无法证明校验先于读取 stdin、`PM-18` AC6 期望缺 `\n`。R31 全部关闭 | 无 | 受审 SHA-256 同上（四个分区均针对该字节） |
| R31 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R31） | `PASS` | 无 | 无 | 受审 SHA-256 `955dde67ad7b192cdc48910ea3f8091b9721202b4ef117a016187d97f5c40fb3` |
| R31 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL`（A/B/D FAIL，C PASS） | A：迁移重排规则与文档门固定的 `2026111702` 字面冲突；B：`PM-21a` AC2 映射在两次 `rev-parse` 都失败时也能通过；D：`PM-33` 期望漏了同样被 sparse 过滤的 `only-one` blob、`PM-16` 与 ADR-PM-04「只在渲染层排序」冲突。R32 全部关闭 | B：`PM-01` 未覆盖经 `kind`/`remote` 更新造成的冲突（R32 在 Description 写明触发器按更新后的行判定；未增 AC，AC 已满 8） | 受审 SHA-256 同上（四个分区均针对该字节） |
| R32 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R32） | `PASS` | 无 | P2：`PM-01` 只以改名形式验收 `reference` UPDATE 冲突 | 受审 SHA-256 `31cabedec0fcdb2beb5ed6e9f32f4de32dc32b77b288c636933e08249b301736` |
| R32 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL`（A/B/D FAIL，C PASS） | A：完成判据要求 `PM-01` 也通过网站第 7 步、其它命名空间符号引用同时被称为永久非目标与延后项；B：`PM-09` 对互斥校验的承接卡前后矛盾；D：`PM-33` 对 bare 克隆使用了 `.libra/objects` 路径与 `loose_object_path`。R33 全部关闭 | B：`PM-01` UPDATE 触发器另两列的变化未单独验收（与 Claude 同一 P2，AC 已满） | 受审 SHA-256 同上（四个分区均针对该字节） |
| R33 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R33） | `PASS` | 无 | P2：`PM-01` 只以改名形式验收 `reference` UPDATE 冲突 | 受审 SHA-256 `6f963346b81baa03516aa68a306b999e83d62138c0fb6eb1f02d2451c32bd61b` |
| R33 (Codex gpt-6.1-terra，A/B/C/D 四分区) | 同上 | `FAIL`（A/D FAIL，B/C PASS） | A：GC-PM-02 只许改一行而 `PM-21a`/`PM-19` 须改两行、网站第 2–7 步未定义多页面卡的逐页预检、暂存提交核对与部署比对；D：`PM-33` AC7 缺 prune 前的正向断言、`PM-16` 的 JSON 基线未带 `--min-count`/`--reverse`/`--top`。R34 全部关闭（另把同属多命令卡的 `PM-18` 一并纳入） | B：`PM-01` UPDATE 触发器另两列的变化未单独验收（与 Claude 同一 P2，AC 已满） | 受审 SHA-256 同上（四个分区均针对该字节） |
| R34 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R34） | `FAIL` | 认同 Codex R34 的各项 P1；另：`PM-18` AC8 未写明独立仓库中 `core.excludesFile` 的取值。R35 全部关闭 | P2：`PM-27` AC3 映射重复前提、`PM-01` 多余空格、若干同路径承诺未声明「不单独验收」、「无输出」措辞宽于 stdout 断言（R35 一并修订） | 受审 SHA-256 `f801590cbef98a65e3a7f69e52e4903c15885b2d32191a5f40db8c9a7676c2a4` |
| R34 (Codex gpt-6.1-terra，A–F 六分区) | 同上 | `FAIL`（A PASS，B/C/D/E/F FAIL） | B：`PM-01` 未验收 `kind`/`remote` 列更新、`PM-21a` 未验收链式/本地目标与检出后工作区、`PM-03` 未验收存储错误、`PM-05` 未验收在库冲突对象缩写；C：`PM-38` 在 `STATE` 缺失而标记残留时与 `PM-29`/ADR-PM-02 矛盾；D：`PM-39` 漏验互斥组合、`PM-10` 无法验证忽略环境变量且未验收仓库要求、`PM-36` 未验收未配置回落、`PM-22`「无输出」未查 stderr；E：`PM-12` 未验收 `--json`、`PM-15` 未验收子目录宏告警与跨来源宏；F：`PM-19` 未验收提交者分组 mailmap、`PM-31` 未验收 `-z` 流式。R35 以新增 AC 或收窄承诺（写明不单独验收）全部关闭 | 无 | 受审 SHA-256 同上（六个分区均针对该字节） |
| R35 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R35） | `FAIL` | 认同 Codex R35 的各项 P1；另清扫出一批承诺了却未验收、也未声明「不单独验收」的同路径情形（见 R36 修订历史行 (8)）。R36 全部关闭 | 无 | 受审 SHA-256 `8441ab870f25cb604344583a71e044dffdca661e73643370c388acb0d653c65a` |
| R35 (Codex gpt-6.1-terra，A–F 六分区) | 同上 | `FAIL`（A–F 均 FAIL） | A：网站短格式 status 在 `status.branch=true` 时误判、收口门记录证据即触发重跑；B：`PM-05` AC8 关于改动前实现的说法与源码不符；C：`PM-29` 无法证明写入后整体回滚、`PM-38` 未覆盖标记存在而 `STATE` 不可解析；D：`PM-39` 无法验证先于取锁、`PM-10a`/`PM-37` 未区分角色变量优先次序；E：`PM-15` 未验收同文件宏定义次序、`PM-18` 未验收忽略引擎 HOME 回退；F：`PM-14` 未验收 `--ai*` 与两个位置参数互斥。R36 全部关闭 | 无 | 受审 SHA-256 同上（六个分区均针对该字节） |
| R36 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R36） | `FAIL` | 认同 Codex R36 的各项 P1；另：`DEP-PM-03` 只列 merge-pull Phase 4 的 7 张卡，而对方工作区 Phase 4 退出条件已列 11 张（协调者转告）。R37 全部关闭 | 「不读」类表述只能部分观测（R37 在验证执行协议第 4 项统一说明） | 受审 SHA-256 `21bec1e9388f91866866089dd50cb22943ae9e2ab8cf80ec2fabfb7304d676b6` |
| R36 (Codex gpt-6.1-terra，A–F 六分区) | 同上 | `FAIL`（A/C PASS，B/D/E/F FAIL） | B：`PM-21a` 所引删除矩阵只覆盖未出生的 `main`、`PM-04` 未改 `-l` 帮助文本；D：`PM-10a`/`PM-37` 所称 `commit_test` 覆盖不存在；E：`PM-27` 未验收「不以 `-` 开头」且现行次序证据有误；F：`PM-33` bare 克隆远端跟踪引用锚点错误。R37 全部关闭 | 无 | 受审 SHA-256 同上（六个分区均针对该字节） |
| R37 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R37） | `FAIL` | 认同 Codex R37 的各项 P1。R38 全部关闭 | 无 | 受审 SHA-256 `dd0c147b900922c36dc9a5ed5792b7ecb57cab46883ab69cbf5984fab6727fb1` |
| R37 (Codex gpt-6.1-terra，A–F 六分区) | 同上 | `FAIL`（D PASS，A/B/C/E/F FAIL） | A：`DEP-PM-03` 以对方动态 Phase 退出条件代替具名前置卡（G-06）；B：`PM-21a` 未验收改写已有符号引用、`PM-04` 所称默认列出全部的回归守护只覆盖单个引用；C：notes 卡把 `blob(<文本>)` 定义为字节而库中存 OID；E：`PM-15` 未覆盖 conversion 的独立求值路径；F：`PM-19` 一张 M 卡含两处独立公开行为变化（G-01/G-04）。R38 全部关闭 | 无 | 受审 SHA-256 同上（六个分区均针对该字节） |
| R38 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R38） | `FAIL` | 认同 Codex R38 B 的 P1。R39 关闭 | 无 | 受审 SHA-256 `8ccf092991000feaf293413ea98567bbf60ba4925f25211699b3361f8bbf76e8` |
| R38 (Codex gpt-6.1-terra，A–F 六分区) | 同上 | `FAIL`（A/C/D/E/F PASS，B FAIL） | B：`PM-04` 的 `P*` 模式无法区分完整 wildmatch 与只支持尾部 `*` 的前缀匹配。R39 关闭 | 无 | 受审 SHA-256 同上（六个分区均针对该字节） |
| R39 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R39） | `FAIL` | 测试矩阵「迁移」行仍称 `PM-01` 用例覆盖「幂等」，与 R35 起 `PM-01` 不单独验收幂等相矛盾。R40 关闭 | 无 | 受审 SHA-256 `fd75edf25b29a198350475c744fc7eca53feb9d52fc60f125db77f01689c4acd` |
| R39 (Codex gpt-6.1-terra，B 全文 + A/C/D/E/F 差异确认) | 同上 | `PASS`（B 分区对本字节全文复审 PASS；A/C/D/E/F 在 R38 字节上全文 PASS，并对 R38→R39 差异确认 PASS） | 无 | P2：R39 受审哈希尚未写入本表（本行补记） | 受审 SHA-256 同上 |
| R40 (Claude subagent claude-opus-5-5) | `plan-20261117-plumbing-misc.md` 全文（R40） | `PASS` | 无 | P2：`PM-01` 只以改 `kind` 与改 `name` 两种 UPDATE 验收（只改 `remote` 声明为不单独验收）；多张卡写明了不单独验收的同路径情形及理由 | 受审 SHA-256 `b9fbc2619f281a6e4a7accda01b00d46fa27c066ce6bf7f1e0291a8fb83a52d5` |
| R40 (Codex gpt-6.1-terra，A–F 差异确认) | 同上 | `PASS`（A/C/D/E/F 于 R38 字节全文 PASS、B 于 R39 字节全文 PASS，R38→R39、R39→R40 差异均确认 PASS，六个分区据此对本字节 PASS） | 无 | 无 | 受审 SHA-256 同上 |

## 实施证据汇总

> 按 v2.12，执行与收口证据统一写入本节，不新建独立证据文件。尚无执行记录。

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-PM-01 | 未列入本批 17 个 issue 的其它 plumbing 子命令对齐 | 超出范围 | 后续按需立项 | 后续对应计划（尚未排期） |
| DEFER-PM-02 | `symbolic_ref` 表的其余消费者：`branch` 列表/删除/改名、`for-each-ref`、`show-ref`、`rev-parse`/`log` 对符号引用分支的解析、在符号引用分支上提交（现被触发器拒绝）、`symbolic-ref HEAD` 对二级符号引用的递归展开、`checkout` 对链式符号引用（目标本身仍是符号引用）的解析、`op` 日志快照对 `symbolic_ref` 的覆盖、`refs/tags/*` 等其它命名空间的符号引用 | `#711` 只要求 `symbolic-ref` 与 `checkout -f`（ADR-PM-03） | 需要全量 ref 遍历支持符号引用时 | 后续 ref 存储计划（尚未排期） |
| DEFER-PM-03 | `#717` 的 `checkout --orphan` 与 `branch.autosetupmerge`、`HEAD^` 子项 | 属 `checkout`/`rev-parse` 行为轴 | 规划 `checkout --orphan` 兼容修复时 | 后续 checkout 兼容计划（尚未排期） |
| DEFER-PM-04 | `#1020`：`tag` 对 blob/tree/tag 对象创建轻量与附注标签，及相应 `--points-at`/`--contains` 查询（撤回的 `PM-13a`/`PM-13b`/`REL-PM-13`） | 会产生 Libra `clone` 无法传输的仓库，`issues/498.md` 的 `ADR-TT-01` 仍有效（ADR-PM-05） | `DEFER-TT-01`/`DEFER-TT-05` 的传输修复立项 | 后续 tag/传输计划（尚未排期）；`#1020` 保持 open |
| DEFER-PM-05 | Libra 分页器遵循 `GIT_PAGER`/`core.pager`/`PAGER`（`#995` 验收 2 的分页器部分、TC-3592..3594） | 改变全局分页器行为，超出 `var` 范围（`CLAUDE.md` 现定「分页命令恒为 `less -R -F`」） | 分页器行为对齐立项时 | 后续分页器计划（尚未排期）；`#995` 保持 open |
| DEFER-PM-06 | 系统级属性文件与 `GIT_ATTR_NOSYSTEM`（`var GIT_ATTR_SYSTEM` 现恒退出 1） | Libra 无系统属性来源 | 需要系统级属性时 | 后续属性计划（尚未排期） |
| DEFER-PM-07 | `#758`/`t6050` 中 `replace.rs` 之外的阻断：`update-ref refs/replace/*`、`cat-file` 尊重 replace、`--no-replace-objects`/`GIT_NO_REPLACE_OBJECTS`、签名提交 | 分属 `update-ref`（`issues/541.md`）、`cat-file`、签名计划 | 对应计划交付后重跑 `t6050` | `issues/541.md` 与后续计划；`#758` 保持 open |
| DEFER-PM-08 | `#988` 的全局 `-c <key>=<value>` 选项、`--ref=refs/heads/x` 展开为 `refs/notes/refs/heads/x`（TC-2062）、notes 在 `for-each-ref` 中可见 | 全局选项与 notes 存储模型属其它行为轴 | 全局 `-c` 立项时 | 后续 CLI 计划（尚未排期）；`#988` 保持 open |

## 完成判据

- [ ] 所有活动任务卡（40 张，不含已撤回卡号与 `DEFER-*`）满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 所有活动卡 `Lifecycle=done` 且 `Acceptance=complete`（ER-04）；家族子卡随 `PM-24` 的 C/D 覆盖一同完成。
- [ ] 所有任务的 `Verification` 命令已按「验证执行协议」运行并在「实施证据汇总」记录结果。
- [ ] **全量收口门（ER-13）:** 在全部活动卡完成后的最终代码树上运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿，并记录受测的 `libra rev-parse HEAD` 与 `libra rev-parse HEAD^{tree}`（受测基线）。此后只修改证据记录文件（本计划、`docs/development/plan/plan-status.md`、`docs/development/plan/plan-long.md`）的提交不触发重跑——以 `libra diff --name-only <受测 HEAD> HEAD` 的输出只含这三个路径核对；输出含其它任何路径即重跑全量收口门。
- [ ] **全量收口门暴露的 Bug 已全部处理（ER-13）:** 本计划引起的失败已前滚修复并重跑全量至全绿；判定为既有失败的已附计划基线 commit 上同样失败的复现证据并登记 `FIX-*` 或 `DEFER-*`。
- [ ] 文档、兼容矩阵与网站按「兼容与文档收口」完成；所有适用网站交付的卡均通过第 7 步部署核验（`PM-01` 按「文档与网站验收命令」第 8 项只执行第 1 步；家族子卡由 `PM-24` 的第 7 步覆盖）。
- [ ] 每张会发版卡已 `patch + 1`、版本面一致、`Cargo.lock` 仅由工具链刷新，并以 `gh release create v<version>` 触发 `release.yml` 取得 D 组绿色证据（ER-08）。
- [ ] 计划文本提交门：Claude（Subagent）与 Codex 对同一 SHA-256 均为 `VERDICT: PASS`（头部「审阅合同」）；执行期每卡 ER-05 review 均 PASS；网站交付的 Claude Code `PASS` 另行取得，三者互不替代。
- [ ] issue 关闭：只关闭「成功定义」所列可完全闭环的 issue，且须在其全部承接卡完成后重跑 issue 所列上游用例；部分闭环的 issue 只同步进展并保持 open。
- [ ] 「修订历史」记录了成稿后的全部规范性变更（G-09）；`plan-status.md` 与 `plan-long.md` 已同步。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
