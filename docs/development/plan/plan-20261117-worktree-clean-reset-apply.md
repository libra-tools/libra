# `clean` / `mv` / `apply` / `reset` / `rm` 工作区与补丁兼容性计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Claude R44 与 Codex R44 双审 PASS（受审 SHA-256 前 12 位 `a710c78a4809`），计划可提交；记录时间 2026-11-18 12:48:20 UTC；`WC-11a`、`WC-11b`、`WC-11R` 为 `blocked`（待 `WC-11s` Go 修订），其余任务 `pending`，全部尚未执行。本文只规划任务，不宣称实现完成。（批处理例外：按协调者指令，20261117 批次起草阶段暂不修改 `plan-status.md`，由协调者统一登记合入。按用户 2026-11-18 指令，计划提交门为 Claude（Subagent）与 Codex 双审，二者对同一字节均 `PASS` 方可提交（取代 2026-11-17 的 Codex 单审口径）。）
> 跟踪范围：13 个 open issues（#732, #733, #734, #735, #736, #755, #812, #831, #837, #838, #1012, #1013, #1038）。

## 文档职责

- **网站交付路径（权威）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 分支）。依据是用户 2026-11-18 11:55:09 UTC 确认（本批计划网站文档交付检出为该路径的 `cf` 生产分支）；仓库 `AGENTS.md:135` 工作区说明仍写 `/Volumes/Sea/libra-backend`（本机不存在），属已知文档滞后，本计划不改 `AGENTS.md`。写入前须核验该路径存在、仅含单一 VCS 元数据且分支为 `cf`（不在 `cf` 时按 ER-06a 执行 `libra switch cf` 后重检；路径缺失、元数据不唯一或切换受阻即 STOP），禁止改用其它检出。

本文解决 `libra clean`、`libra mv`、`libra apply`、`libra reset` 与 `libra rm` 在相对路径输出、`core.excludesfile` 大小写读取、未跟踪目录清理、双 `-f`、`-e` 目录排除与 `clean.requireForce`、目录重命名（及 `TC-0733` partial-clone 核验）、`--numstat`/`--stat --summary`/`--binary`/`--3way` 无载荷 full-index 二进制补丁、`main` 误判锁定分支、`reset --hard` 未合并文件清理以及 `rm --pathspec-from-file=-` / `-n` 方面的 13 个 Git 兼容性缺口。目标是 24 张任务卡：20 张 `implementation`（`WC-01`..`WC-05`、`WC-04a`、`WC-04b`、`WC-05a`、`WC-06`、`WC-07`..`WC-09`、`WC-09a`、`WC-09b`、`WC-10`、`WC-11`、`WC-11a`、`WC-11b`、`WC-12`、`WC-13`）、2 张只读 `spike`（`WC-06a` 核验 `TC-0733`；`WC-11s` 调查 `reset --hard` 恢复协议的跨进程锁与写入口覆盖）与 2 张 `release`（`WC-04R`，`REL-WC-NEST` 家族唯一发布点，联合发布 `WC-04`/`WC-04b`；`WC-11R`，`REL-WC-RESET` 家族唯一发布点，联合发布 `WC-11`/`WC-11a`/`WC-11b`）；其余卡按各自边界完成实现、核对、测试、文档与发布闭环。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- 源码：`src/command/clean.rs`、`src/utils/util.rs`（只读引用）、`src/internal/config.rs`、`src/command/mv.rs`、`src/command/apply.rs`（含与 `src/command/am.rs:466` 共享的 `prepare_patch_opts`）、`src/command/reset.rs`、`src/command/restore.rs`、`src/internal/branch.rs`、`src/command/remove.rs`；`reset` 家族另涉及 `src/internal/worktree_io/mod.rs`、`src/internal/operation/store.rs`、`src/cli.rs`（以 `WC-11s` 修订后的写集为准）。
- 测试：`tests/command/{clean,mv,apply,reset,restore,remove,status}_test.rs`（均挂在 `command_test` target，测试全名前缀 `command::<模块>::`），以及 `src/internal/config.rs`、`src/internal/branch.rs`、`src/command/clean.rs`、`src/cli.rs` 内既有的 `#[cfg(test)] mod tests`，和分别由 `WC-13`、`WC-09b` 在 `src/command/remove.rs`、`src/command/apply.rs` 内新建的 `#[cfg(test)] mod tests`。
- 文档：`docs/commands/{clean,mv,apply,reset,restore,rm}.md`、`docs/commands/zh-CN/{clean,mv,apply,reset,restore,rm}.md`、`docs/development/commands/{clean,mv,apply,reset,restore,rm}.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{clean,mv,apply,reset,restore,rm}.en.md`。

### 非目标

- 不在本计划中修改 `libra commit`、`libra cherry-pick`、`libra revert`、`libra rebase` 或 `libra am` 的命令代码（`WC-09a` 修改的 `prepare_patch_opts` 由 `apply` 与 `am` 共享，`am` 对无载荷 full-index 二进制补丁的判定随之与 Git 一致，属有意的连带兼容，见 `WC-09a`）。
- 不在本计划中修改 SQLite schema 或新增迁移（不涉及 `sql/migrations/**`）。
- 不移除对 AI 托管分支（`intent`、`traces`、`agent-traces`）的锁定保护。

### 成功定义

- 10 个 open issues（`#732..#736, #812, #831, #838, #1013, #1038`）在对应实现卡完成上游等价闭环。
- `#837`：`WC-09` 交付 `apply --binary` 兼容标志与二进制新建补丁删后重建（上游 `t4012#12` 应用侧）；生成侧 `libra diff --binary --no-index /dev/null` 由 `DEFER-WC-05` 承接，落地前不宣称 `#837` 全量闭环。
- `#755`：`WC-06` 完成 `libra mv dir folder` 与 `t6421#1` setup；`WC-06a`（`spike`）在 `DEP-WC-06` 就绪后核验 `TC-0733`。Go 时由 `WC-13` 覆盖 C/D 后关闭；No-Go 时 `#755` AC3 保持显式 `blocked`，直到 `DEFER-WC-02` 或 `WC-06b` 落地，不提前宣称 `#755` 全量闭环。
- `#1012`：`WC-11s` 判定 Go 且修订取得双 PASS 后，由 `WC-11`、`WC-11a`、`WC-11b` 与发布点 `WC-11R` 闭环；`WC-11s` No-Go 时按 `DEFER-WC-03` 正式延后，不宣称关闭。
- 全部非延后任务卡达到 `Lifecycle=done` 且 `Acceptance=complete`，计划收口全量门（`fmt` + `clippy` + `nextest`）全绿。

## 使用规则

- 每次状态推进、快照更新与「修订历史」新增行都必须记录 `YYYY-MM-DD HH:MM:SS UTC` 时间戳。
- 推进或完成任一任务卡时，须在同一变更中同步更新 [plan-status.md](plan-status.md)（起草阶段不提前修改 `plan-status.md`）。
- `COMPATIBILITY.md` 被多份 `plan-20261117-*` 计划共享且受兼容账本绝对行号约束：更新时只允许在既有条目行内追加，不得增删行改变行号，并按 `DEP-WC-*` 串行协调。
- 本计划所有测试摘要、门结果与评审结论直接写入本计划文件内，不新建外部证据文件或目录（v2.12）；终止链末端提交的 D 证据按模板 v2.14 由下一次 `plan-status.md` 例行同步补记（协议见「完成判据」，在此预登记）。
- 后端网站文档交付遵循 ER-06a 与后端 `AGENTS.md` 的 `cf` 直推流程：生产站点的权威 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支，直接在该分支提交与推送；用户 2026-11-18 11:55:09 UTC 确认，与仓库 `AGENTS.md:135` 的差异见「文档职责」）。开工写入前按「文档与网站验收命令」第 2 步核实（不在 `cf` 时 `libra switch cf` 后重检）；路径缺失、元数据不唯一或切换受阻时对应卡置为 `blocked` 并停止写入。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。过期锚点只能作为历史线索。以下锚点于 2026-11-18 按当前 checkout 核对。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| `clean` | `!args.force && !args.dry_run` 硬编码要求 `-f`/`-n`（exit 129、`LBR-CLI-002`），未读 `clean.requireForce`；`force` 为 `bool` 拒绝 `-f -f`；`dry_run` 与删除输出仓库根相对路径而非 cwd 相对路径；`matches_exclude_pattern` 生成 `^pattern$` 无法匹配目录前缀（`-e src`）；无 `-d` 时也会删未跟踪目录内文件；目录扫描跳过名为 `.git`/`.libra` 的条目 | `src/command/clean.rs:36-41,183-186,218-222,297-305,374,451-475`; `tests/command/clean_test.rs:68-90` |
| 配置读取 | `core.excludesFile` 经 `optional_cascaded_config_path` → `read_cascaded_config_value_fresh_conn` 读取；`read_cascaded_fresh_conn_at` 用 `ConfigKv::get_with_conn` 精确匹配键，小写 `core.excludesfile` 不命中；已有 `get_var_case_insensitive_with_conn` | `src/utils/util.rs:61,1484-1500,3355`; `src/internal/config.rs:166-177,536,2601-2616`; `src/utils/attributes.rs:22,329` |
| `mv` | `validate_source_directory` 在 `!destination_is_dir` 时直接返回 `fatal: destination '...' is not a directory`，拒绝把目录重命名为不存在的新路径；多源非目录目标另在 `:151` 拒绝 | `src/command/mv.rs:149-155,280-292` |
| `apply` | `ApplyArgs` 已有 `numstat`/`summary`/`stat`/`three_way`/`index`，缺 `--binary`；`prepare_patch_opts` 在 `headers.binary_marker && headers.binary.is_none()` 时无条件拒绝（提示 `--binary`），且该函数被 `am` 共享；`diff.rs` 已支持 `--binary --no-index` | `src/command/apply.rs:242-277,377-387,748-810,1195-1290`; `src/command/am.rs:466`; `src/command/diff.rs:290,394-395` |
| `reset` & `branch` | `reset.rs:914` 与 `restore.rs:485` 调用 `is_locked_revision`，后者经 `is_locked_branch` 锁定 `DEFAULT_BRANCH="main"`（`is_ai_managed_branch` 已排除 `main`）；`tracked_paths_from_index` 只取 `tracked_entries(0)`，遗漏 unmerged 路径；`reset --hard` 失败回滚只按旧提交恢复 | `src/command/reset.rs:905-916,2198-2230,2262-2287,2835-2850`; `src/command/restore.rs:478-487`; `src/internal/branch.rs:34,60-90,1194-1204` |
| operation 边界 | `classify_command` 把 `status` 归为只读、写命令分属不同 mutation class；`run_with_operation` 持 `ScopeLease`；`command_has_existing_operation_boundary` 列出不经中央边界的命令 | `src/internal/operation/middleware.rs:93-115,283`; `src/internal/operation/middleware/lease.rs:83`; `src/cli.rs:2355` |
| `rm` | `RemoveArgs::dry_run` 仅 `#[clap(long)]`（无 `-n`）；`std::fs::read(file)` 不支持 `"-"`；未校验 `--pathspec-from-file` 与位置参数/`--pathspec-file-nul` 冲突；空列表已报 `No pathspec was given. Which files should I remove?`；人读输出用 `println!`；`output.rs` 把 `BrokenPipe` 转为 exit 0 | `src/command/remove.rs:57-58,64,158-200,429`; `src/utils/output.rs:121-128` |
| 测试命名 | `tests/command_test.rs:10` 声明 `mod command`，`tests/command/mod.rs` 声明各 `*_test` 模块 → 集成测试全名 `command::<模块>::<函数>`；库单元测试全名为 crate 内模块路径（如 `internal::branch::tests::…`、`internal::config::tests::…`、`command::clean::tests::…`） | `tests/command_test.rs:10`; `tests/command/mod.rs:1003,1022,1097,1100,1118,1123,1124,1145`; `src/lib.rs:21,25`; `src/internal/mod.rs:27` |
| 用户与开发文档 | 6 个命令（含 `restore`）均已有 EN、zh-CN 与 `docs/development/commands/<cmd>.md`；网站页面由后端 `scripts/sync-libra-command-docs.py` 自 Libra 文档同步 | `docs/commands/{clean,mv,apply,reset,restore,rm}.md`; `/Volumes/Data/gitmono/libra-backend/scripts/README-command-doc-sync.md` |
| 外部参照 | Git 上游提交 `750f5de7dfe9a4062f95088212b063f189d50e91`（`DEF_VER v2.56.GIT`，核对日 2026-11-18）；本文 `t/<脚本>#<n>` 指该版本脚本运行期第 n 个 `test_expect_*`，`builtin/clean.c`、`apply.c`、`builtin/rm.c` 行号同指该版本 | `t/t7300-clean.sh`、`t/t4012-diff-binary.sh`、`t/t4108-apply-threeway.sh`、`t/t6411-merge-filemode.sh`、`t/t6415-merge-dir-to-symlink.sh`、`t/t3600-rm.sh`、`t/t6421-merge-partial-clone.sh` |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-01 | `clean` 相对路径前缀丢失、`core.excludesfile` 大小写未命中、无 `-d` 删目录内文件、拒绝 `-f -f`、忽略 `clean.requireForce`、`-e src` 不匹配子路径 | 误删未跟踪文件或无法匹配 Git `t7300` | `src/command/clean.rs:36-41,183-305,451-475`; `src/internal/config.rs:166-177,2616` | `WC-01`..`WC-05`, `WC-04a`, `WC-04b`, `WC-05a` |
| GAP-02 | `mv` 拒绝把目录重命名为不存在目标 | `libra mv dir folder` 失败 | `src/command/mv.rs:286-292` | `WC-06` |
| GAP-03 | `apply` `t4012#5`/`t4012#2`（`--stat --summary` 二进制）未验证；缺少 `--binary` 标志；无载荷 full-index 二进制补丁一律被拒 | 带 `--binary` 报未知参数；`t4108#17` 失败 | `src/command/apply.rs:242-277,1283` | `WC-07`..`WC-09`, `WC-09a`, `WC-09b` |
| GAP-04 | `reset` 与 `restore` 拒绝 `main` 为锁定分支；`reset --hard` 残留 unmerged 文件为 untracked | 无法 `reset --hard main`；合并冲突后工作区残留脏文件 | `src/internal/branch.rs:87-90`; `src/command/reset.rs:914,2835-2850` | `WC-10`, `WC-11s`, `WC-11`, `WC-11a`, `WC-11b`, `WC-11R` |
| GAP-05 | `rm` 缺少 `-n` 短标志、不支持 `--pathspec-from-file=-` 与完整冲突校验 | `rm -n` 报未知参数，脚本化 stdin 读路径失败 | `src/command/remove.rs:57-58,158-200` | `WC-12`, `WC-13` |

## 与其它计划的关系

其它同日计划可能正在并行修订，本节只按任务 ID 引用，不引用其行号。

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| [plan-20261117-branch.md](plan-20261117-branch.md) | `BR-10`/`BR-22` 写 `src/internal/branch.rs` 与 `COMPATIBILITY.md` | 通过 `DEP-WC-01`（`branch.rs`）与 `DEP-WC-05`（`COMPATIBILITY.md`）严格串行协调 |
| [plan-20261117-plumbing-misc.md](plan-20261117-plumbing-misc.md) | `PM-21a`（`REL-PM-21` 家族首卡）写 `src/internal/branch.rs`，`DEP-PM-06` 规定 `WC-10` 先合入；`PM-03` 提供 `rev-list --missing=<action>`；`PM-10`/`PM-19` 写 `src/cli.rs` | `WC-10` 不等 `PM-21a` 且先合入 `branch.rs`（单向无环）；`PM-03` 纳入 `DEP-WC-06`；`PM-10`/`PM-19` 纳入 `DEP-WC-07`。注：`DEP-PM-06` 文字仍写拆分前的 `PM-21`，语义为 `REL-PM-21` 家族，见「风险登记」跨计划项 |
| [plan-20261117-clone.md](plan-20261117-clone.md) | `CL-13`、`CL-19`、`CL-20` 提供 `--filter=blob:none` 与 promisor 按需拉取；`CL-01`、`CL-20`、`CL-26`、`CL-27`、`CL-28` 写 `src/cli.rs` | 前者纳入 `DEP-WC-06` 供 `WC-06a` 核验 `TC-0733`；后者纳入 `DEP-WC-07` |
| [plan-20261117-global-init-infra.md](plan-20261117-global-init-infra.md) | `GII-05`/`GII-09` 写 `src/internal/config.rs`；`GII-01`..`GII-05`、`GII-15` 写 `src/cli.rs` | 前者纳入 `DEP-WC-03` 串行协调 `WC-02`（G-10）；后者纳入 `DEP-WC-07` |
| [plan-20261117-merge-pull.md](plan-20261117-merge-pull.md) | `MP-14..20, MP-24..25` 等 merge 重命名链路 | 纳入 `DEP-WC-06` 供 `WC-06a` 执行 `t6421#1`（`WC-06a` 不写 `merge.rs`，无 G-10 写集冲突） |
| [plan-20261117-checkout-switch-restore.md](plan-20261117-checkout-switch-restore.md) | `CSR-01, CSR-05, CSR-06, CSR-09, CSR-11, CSR-12, CSR-13, CSR-14, CSR-20, CSR-23, CSR-24, CSR-29, CSR-30, CSR-33` 写 `src/command/restore.rs` / 测试 / 文档；`CSR-01`、`CSR-02`、`CSR-23`、`CSR-27` 写 `src/cli.rs` | 前者通过 `DEP-WC-04` 逐卡协调 `WC-10` 对 `restore` 文件的写入（对方以 `DEP-CSR-05` 对等登记）；后者纳入 `DEP-WC-07` |
| [plan-20261117-commit-signing.md](plan-20261117-commit-signing.md) | `CS-24` 只规划 `GIT_TRACE2_EVENT`；`CS-09`、`CS-15b`、`CS-24`、`CS-26` 写 `src/cli.rs`；`DEP-CS-07` 登记全批 `COMPATIBILITY.md` 串行 | `GIT_TRACE2_PERF` 缺口由 `DEFER-WC-02` 承接；`src/cli.rs` 写入者纳入 `DEP-WC-07`；`COMPATIBILITY.md` 由 `DEP-WC-05` 协调 |
| [plan-20261117-branch.md](plan-20261117-branch.md)、[plan-20261117-fetch-push.md](plan-20261117-fetch-push.md) | `BR-03`、`FP-18` 写 `src/cli.rs` | 纳入 `DEP-WC-07` |
| [plan-20261117-diff.md](plan-20261117-diff.md) | `DIFF-41..44` 修复 `diff --no-index`（目录拼接、末尾 pathspec、头路径、相等文件），均写 `src/command/diff.rs`；尚无卡让 `--no-index` 把 `/dev/null` 视为缺失一侧或输出 `--binary` 载荷 | 本计划不写 `diff.rs`；`WC-08` 以字面量补丁、`WC-09` 以仓库模式 `diff --cached --binary` 构造输入，`t4012#12` 生成侧登记 `DEFER-WC-05`，在其 `--no-index` 卡合入后另立卡承接 |
| 其余同日计划（`sequencer-am`/`diff`/`format-patch`/`grep`/`log-show`/`describe`） | 共享 `COMPATIBILITY.md` 账本 | 写入受 `DEP-WC-05` 跨计划串行调度保护且只追加不增删行 |
| `COMPATIBILITY.md` | 跨计划共享兼容矩阵（受兼容账本绝对行号约束） | 本计划内相关卡逐卡登记 `DEP-WC-05` 并按实施顺序串行写入 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 13 个 issue 均为真实 Git 兼容缺口，锚点已于 2026-11-18 复核 | R20：事实基线全部锚点刷新并 pin 上游 `git@750f5de7` |
| 可行性 | 除 `reset` 家族恢复协议外均可直接实现；恢复协议所需锁覆盖尚未查清 | R20：新增 `WC-11s` spike，`WC-11a`/`WC-11b`/`WC-11R` 置 `blocked` |
| 任务卡粒度 | 24 张卡逐卡按独立谓词重数，卡内 `Granularity` 写明计数依据；R21 将 `WC-09a` 的二进制三方回退拆为 `WC-09b` | R20：`WC-04`/`WC-09a`/`WC-10`/`WC-11a`/`WC-11b`/`WC-13` 等改写 AC 以满足 G-03；`WC-11R` 只计聚合守卫与手工证据 |
| 依赖与顺序 | DAG 无环；全部 `COMPATIBILITY.md` 写入者在单一链上；`reset` 家族移至链尾；`no-release`/家族子卡前驱按 `locally-accepted` 放行，避免与承载点的完成状态死锁 | R20 重排实施顺序；R21 补「依赖语义」 |
| 完整性 | 每卡具名测试、共享回归组、文档逐文件验收与网站交付步骤齐全 | R20：补「文档与网站验收命令」与 D 组默认登记 |
| 安全性 | 嵌套仓库仅双 force 删除、AI 托管分支锁定保留、图层文件受 `clean` 保护、`clean.requireForce` 非法值 fail-closed | 保持 |
| 功能正确性 | 期望输出取自上游测试与 `builtin/clean.c`、`apply.c`、`builtin/rm.c` 文案 | R20：AC → 断言映射全部重写 |
| 接口兼容 | 新增 `clean -ff`、`apply --binary`、`rm -n`、`rm --pathspec-from-file=-`；`--json` 契约保持仓库根相对路径；`reset --recover` 待 `WC-11s` 定稿 | 保持 |
| 数据流与控制流 | `reset --hard` 扩大删除范围后依赖 `WC-11a` 恢复协议；协议的锁覆盖由 `WC-11s` 决定 | R20：家族在 `WC-11s` Go 前不得开工 |
| 性能与容量 | `clean` 遍历保持 `O(N)`，由 `command::clean::tests::untracked_dir_scan_reads_each_directory_once` 断言 | R20：性能守卫由 `WC-05` 移到改动遍历的 `WC-03`，改为库单元测试 |
| 可靠性与容错 | `rm -n` SIGPIPE 与锁文件、`apply` 失败零部分应用、`reset` 故障恢复矩阵已登记 | 保持 |
| 可维护性 | 配置大小写修复复用既有 `get_var_case_insensitive_with_conn`；`prepare_patch_opts` 共享语义在 `WC-09a` 显式说明 | 保持 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 03:08:00 UTC | 初稿创建 | 基于 `agent_2_tasks.json` 起草 13 张任务卡覆盖 13 个 issues | `N/A` | 全文 |
| 2026-11-17 03:13:00 UTC | Claude R1 FAIL + Codex R1 FAIL | 修正 `WC-02` 路径与根因、拆出 `WC-05a`、补充 `WC-07`..`13` 上游场景与 `restore` 写集 | `WC-05`→`WC-05`,`WC-05a` | 实施顺序、依赖表、粒度表、追溯表 |
| 2026-11-17 03:23:00 UTC | Claude R2 FAIL + Codex R2 FAIL | 补齐 `DEP-WC-02`/`03`/`04`；`WC-09`、`WC-10`、`WC-11`、`WC-03` 对齐上游测试与 `COMPATIBILITY.md` | `N/A` | 实施顺序、依赖表、粒度表、任务卡、追溯表 |
| 2026-11-17 03:33:00 UTC | Codex R3 FAIL | 明确 `WC-01` 无 pathspec 子目录不带 `../`；修正并发示例与 `WC-10` 审计表 | `N/A` | `WC-01`、并发声明、审计表 |
| 2026-11-17 03:49:00 UTC | Claude R4 FAIL + Codex R4 FAIL | 新增 `WC-09a` 解锁 `--3way` 无载荷 full-index 二进制补丁；将 `DESC-09` 纳入 `DEP-WC-02`；新增 `DEP-WC-05` | `WC-09`→`WC-09`,`WC-09a` | 关系表、依赖表、实施顺序、审计表、多卡 |
| 2026-11-17 03:58:00 UTC | Codex R5 FAIL | 按 Git `apply.c:3315-3360` 精确定义 `WC-09a` 缺失对象失败条件；修正 `M2` 为 `WC-06..WC-09a` | `N/A` | 使用规则、`WC-09a`、`M2` 里程碑、日志 |
| 2026-11-17 04:08:00 UTC | Codex R6 FAIL | 按 `AGENTS.md` 将全部网站文档写集改为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支） | `N/A` | 使用规则、适用范围、各卡网站写集、日志 |
| 2026-11-17 04:22:00 UTC | Codex R7 FAIL | 补充 `#755` (`TC-0733`) 部分闭环口径与 `DEP-WC-06` | `N/A` | 成功定义、依赖表、`WC-06`、`DEFER-WC-02` |
| 2026-11-17 04:29:00 UTC | Claude R8 FAIL + Codex R8 FAIL | 补齐 `DEP-WC-02`/`03`、`WC-10` `AC<=8`、新增 `WC-06a`、逐卡 `DEP-WC-05`、`WC-02/03/13` 重数 | `WC-06`→`WC-06`,`WC-06a` | 关系表、依赖表、实施顺序、审计表、多卡、延后项、日志 |
| 2026-11-17 04:45:00 UTC | Claude R9 FAIL + Codex R9 FAIL | `DEP-WC-01` 补 `PM-21`、`DEP-WC-04` 细化 `CSR-01..08, CSR-21`、`DEP-WC-06` 补 `CL-20` 与 `MP-*` 具体卡号；`WC-06` 移除残留 `DEFER-WC-02`；`WC-06a` 消费 `CL-20` 批量拉取契约并补 `merge_test` + `DEP-WC-04`（`VER=3/8`）；`WC-02` 收窄写集为 `config.rs`（`1/1`）；`WC-04/05/06/09/09a` 严格重数 AC；`WC-07/08` 移除 `prod-files=0` 歧义；`WC-11` 保留 `ADR-MG-01` gitlink 排除；日志仅保留 `PASS`/`FAIL` | `N/A` | 关系表、依赖表、审计表、`WC-02/04/05/06/06a/07/08/09/09a/11`、日志 |
| 2026-11-17 05:10:00 UTC | Claude R10 FAIL + Codex R10/R11/R12 FAIL | `DEP-WC-04` 补全 12 张 `restore.rs` 卡、`DEP-WC-06` 补齐 `CL-13/19/20` 与 `PM-03`；移除过时 `DEP-WC-02`；`WC-02` 补 `core.attributesFile` 回归；`WC-13` SIGPIPE 标明 `#[cfg(unix)]`；`WC-06a` 规范为 `spike` + `no-release` + `0.5d` timebox + `C/D coverage from: WC-09` + `Rollback mode: revert` + `Deliverables` 白名单 | `N/A` | 头部、关系表、依赖表、审计表、`WC-02/03/06a/07/08/11/12/13`、日志 |
| 2026-11-17 05:40:00 UTC | Codex R13/R14 FAIL | 明确 `WC-06a` 在缺省 `GIT_TRACE2_PERF` 时的 No-Go 可完成判据并将 `plan-long.md` 纳入 `WC-06a` `Deliverables`/写集；Libra 与后端 `/Volumes/Data/gitmono/libra-backend` 统一改用 `libra status`/`libra diff`/`libra revert`；`WC-06` 依赖补 `WC-05a` 对齐 Phase 1 入口；更新头部与审阅日志至 `R15` | `N/A` | 头部、修订历史、审计表、`WC-06`、`WC-06a`、日志 |
| 2026-11-17 07:01:00 UTC | Codex R16 FAIL | 消除 `WC-10 ↔ PM-21` 跨计划依赖死环（`DEP-WC-01` 移除 `PM-21` 并按 `DEP-PM-06` 先于 `PM-21` 合入 `WC-10`，将 `WC-06a` 后置至 `WC-12` 与 `WC-13` 之间且 `C/D coverage from: WC-13`）；为 `WC-06a` Go/No-Go 双路径补齐可复现 `t6421#1` 场景执行与零命中/失败处理命令；在 `默认覆盖` 登记 `WC-06a` 并改 `Version increment=N/A` | `N/A` | 关系表、实施顺序、`DEP-WC-01`、发布窗口、Phase 1/2、默认覆盖、审计表、`WC-06a`/`09`/`10`/`13`、日志 |
| 2026-11-17 12:27:28 UTC | 用户单审指令 | 计划提交门改为 Codex 单审，不再要求 Claude Review | N/A | 头部、M0、完成判据 |
| 2026-11-17 12:27:28 UTC | Checkout Codex R38-A 写集对称修复 | `DEP-WC-04` 与关系表补入共享 `restore.rs` 的 `CSR-23`/`CSR-24`，统一为 14 张卡，消除 `WC-10` 与这两卡的并发窗口 | N/A | 关系表、依赖表、评审结论、Review log |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件；`reset --recover` 入口与统一命令守卫拆为新卡 `WC-11b`（`WC-11a` 保留并发检测与崩溃恢复），补 operation store 与 ref/reflog 同事务、提交后 ref 再移动的权威判定、逐阶段 fsync 持久化偏序、`DEP-WC-05`/`DEP-WC-07` 串行窗口与 `REL-WC-RESET` 家族发布表；恢复命令与守卫测试改用完整限定名精确过滤 | `WC-11a` → `WC-11a`, `WC-11b`（R20 补登，原记 N/A） | 全文对应章节 |
| 2026-11-18 09:43:54 UTC（R20 更正：原误记为未来时刻 `10:35:00 UTC`，按文件最后写入时间改记，实际写入不晚于该时刻） | Codex 复审 FAIL 修复（验证契约与门禁） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 10:12:22 UTC | 用户 2026-11-18 双审指令 | 计划文本提交门由 Codex 单审改为 Claude（Subagent）与 Codex 双审，二者对同一字节均 `PASS` 方可提交；执行期各卡 ER-05 评审门与网站 Claude Code PASS 门不变 | N/A | 头部、WC-11R AC2、M0、完成判据 |
| 2026-11-18 10:41:52 UTC | G-09 补登（R20） | 补登 `WC-11` → `WC-11`, `WC-11a` 的拆分：该拆分发生于 R16（2026-11-17 07:01:00 UTC）之后、2026-11-18 08:19:08 UTC 之前，原修订未登记，确切时间不可考；`WC-11`/`WC-11a` 卡内补写拆分关系 | `WC-11` → `WC-11`, `WC-11a` | `WC-11`、`WC-11a`、审计表 |
| 2026-11-18 10:41:52 UTC | R20 双审 FAIL 修复（Claude 12 项 P1 + Codex 12 项 P1；受审 SHA-256 `6926175374cc3092d18ddd8b6b31fc1863480345e4124e9f9bf789cc2431b096`） | 补齐 09:43:54 行声称新增但实际缺失的「文档与网站验收命令」并重写「验证执行协议」（区分具名测试/共享回归组/守卫与手工证据）；全部过滤改为真实全名 `command::<模块>::<函数>` 与 `internal::…`/`command::…::tests::…`；全部「AC → 断言映射」按真实 fixture、退出码、输出与前后状态重写，`WC-04`/`WC-05`/`WC-09a`/`WC-10`/`WC-11a`/`WC-11b`/`WC-13` 按 G-03 重写 AC 并在 `Granularity` 写明计数依据；新增 spike `WC-11s`（自 `WC-11a` 拆出）承接锁协议调查，`WC-11a`/`WC-11b`/`WC-11R` 置 `blocked`，`reset` 家族移至实施链尾以消除与 `WC-12` 的 `COMPATIBILITY.md` 相交；`DEP-WC-07` 落为具名卡快照；登记 D 组默认、上游 `git@750f5de7` pin；`WC-09a` 改登 `T-1`（`am` 共享 `prepare_patch_opts`）；性能守卫移至 `WC-03`；`WC-13` 非 Unix 分支改为平台无关纯函数单元测试；`WC-11R` 补必填字段；家族子卡不写后端，网站由 `WC-11R` 交付；新增 `DEFER-WC-03` | `WC-11a` → `WC-11a`, `WC-11s` | 头部、文档职责、适用范围、成功定义、事实基线、当前缺口、关系表、评审结论、执行检查、实施顺序、依赖表、发布分组、Phase 2、字段默认、审计表、全部任务卡、测试矩阵、追溯表、里程碑、风险、兼容收口、review log、延后项、完成判据 |
| 2026-11-18 10:57:40 UTC | R21 双审 FAIL 修复（Claude 1 项 P1 + Codex 9 项 P1；受审 SHA-256 `4cad0583baa9e7fc9cb64813af7feaf8b974debf3e05a65e3a855610bde052e6`） | `WC-09` 不再锁定将被 `WC-09a` 改写的诊断文案；`WC-09a` 只保留无载荷 full-index 二进制直接应用并按单谓词重写 8 条 AC，`--3way` 二进制三方回退拆出新卡 `WC-09b`（G-09，接入 apply 链与 `COMPATIBILITY.md` 链，`WC-10` 改依赖 `WC-09b`）；新增「依赖语义」消除 `WC-06a`↔`WC-13`、`WC-11s`↔`M3` 与家族子卡的完成状态互等；验证执行协议改为「选中/通过/失败/过滤未选中」口径并以 `--run-ignored all` 复核、新增条件分支条款；新增「spike 产物 allowlist 守卫」（全文件内容哈希快照对比 + 固定 allowlist），`WC-06a`/`WC-11s` 改为引用；`WC-06a` 产品 No-Go 承接要求完整任务卡与双审；家族子卡在本卡写入并验证后端 `reset.en.md` 但保持未提交，新增「文档与网站验收命令」第 5 步家族规则（基线哈希核对），步骤编号顺延；`WC-11R` 的 release note 与窗口证据分为发布前门与 D 组；删除错误的 `WC-07`/`WC-08` 并发示例；`WC-05a` 补 `COMPATIBILITY.md` 与 `DEP-WC-05`；统一 `cf` 分支前置为「切换后重检、受阻 STOP」；`WC-13` SIGPIPE shell 片段改为上游同形；完成判据改为适用分支口径 | `WC-09a` → `WC-09a`, `WC-09b` | 头部、文档职责、使用规则、当前缺口、评审结论、执行检查、实施顺序、依赖表、发布分组、Phase 1/2、默认覆盖、审计表、`WC-05a`/`WC-06a`/`WC-09`/`WC-09a`/`WC-09b`/`WC-10`/`WC-11`/`WC-11a`/`WC-11b`/`WC-11R`/`WC-11s`/`WC-13`、追溯表、里程碑、风险、review log、完成判据 |
| 2026-11-18 11:03:34 UTC | R22 双审 FAIL 修复（Claude 1 项 P1 + Codex 6 项 P1；受审 SHA-256 `440d32ab6a330775cbf927c09880277fd65d5bd05a8e97b6bbbae792cd33edbf`） | 家族子卡改为同卡完成后端网站写入、评审与签名+DCO 本地提交（不推送），新增 `DEP-WC-08` 后端 `cf` 推送冻结窗口防止提前公开，`WC-11R` 只做聚合核对、推送与部署（「文档与网站验收命令」第 4–6 步、发布分组、默认覆盖、各卡与追溯表同步，`WC-11R` 写集改 `N/A`）；spike allowlist 守卫改为 `set -euo pipefail` 脚本并单独捕获 `diff` 退出码；`WC-11s` No-Go 改用合法状态 `blocked / 空` + `DEFER-WC-03` 注记，两条路径均要求计划修订双 PASS 与可执行核对，C/D 承载改为 Go→`WC-11R`、No-Go→`M3`（最终树收口门 + 推送 `main` 后 `base.yml`/`codeql.yml`），依赖语义与 M3 补 `remote-pending`；`WC-11R` 发布命令改用经核对的 `--notes-file`；`WC-06a` 判定后先到 `locally-accepted` 再随承载点完成；`WC-09b` 更正 `am -3` 会连带获得二进制三方回退的事实（原「am 不传回退开关」不实） | `N/A` | 头部、执行检查、文档与网站验收命令、依赖表、实施顺序、发布分组、默认覆盖、审计表、`WC-06a`/`WC-09b`/`WC-11`/`WC-11a`/`WC-11b`/`WC-11R`/`WC-11s`、追溯表、里程碑、风险、review log |
| 2026-11-18 11:08:11 UTC | R23 双审 FAIL 修复（Claude 1 项 P1 + Codex 5 项 P1；受审 SHA-256 `4fbf81581be0a92127c4f46157a4300a3d5fe78222f0ad14c80193cd7f25f980`） | `WC-11R` 在第 5 步重新取得并全程使用后端 `$FINAL`；spike allowlist 守卫改为 Python 快照（路径、类型、权限位、文件 SHA-256 或链接目标），只排除 `.libra/`、`node_modules/` 与含 `CACHEDIR.TAG` 的构建目录，比对新增/删除/元数据变化；完成判据定义「代码树」（不含三份计划记账文件）与预登记的 v2.14 收口记账/终止链协议，`WC-11s` No-Go 的 D 改由收口记账提交的 push run 承载，消除收口循环；`WC-09b` 的二进制三方回退只经 `apply` 专用入口启用（`am -3` 不受影响，新增 AC7 与库单元测试证明，`am -3` 二进制回退登记 `DEFER-WC-04`），并把缺 pre、缺 post 拆为分别构造的 AC4/AC5；并发声明限定为仅 `WC-07`/`WC-08` 可与 `clean` 链并发；M3 按 Go/No-Go 分述收口前提；`WC-11b` 入口锚点更正为 `run_reset`；`t6421` setup 锚点更正为 `:31-145` | `N/A` | 头部、使用规则、适用范围、执行检查、文档与网站验收命令第 5 步、发布分组、默认覆盖、审计表、`WC-06a`/`WC-09b`/`WC-11b`/`WC-11s`、测试矩阵、追溯表、里程碑、风险、延后项、完成判据、review log |
| 2026-11-18 11:12:01 UTC | R24 修复（Claude PASS + 1 项 P2；Codex 5 项 P1；受审 SHA-256 `907fc33f4803aa92769377336f72be4dfd297141345e508d51429f8308997ac1`） | spike 守卫改为同时核对完整 `libra status --short`（结束时新增状态路径须在 allowlist 内，开工前已存在的越界改动列为 PRE-EXISTING 并按 ER-01 归因）与基于 `libra ls-files`/`--others --exclude-standard` 的类型、权限、内容快照，排除范围改由仓库忽略规则决定；`WC-09b` AC7 改为可在现有接口上断言的 `Err` 类型、信息与磁盘不变，并以 apply 专用入口作对照；Phase 2 退出条件按 Go/No-Go 分述并与 `M3`、终止链协议一致；`WC-06a` 判据规范分别断言 checkout 窗口（M0−M1、M1−M0 为空）与 merge 窗口（M1−M2 为三目标、M2−M1 为空）；C 组默认补「bump 后重跑 ER-04 A 组全部 focused 命令并附加本卡 Verification」；`WC-07` writeset 标注改为 `WC-07..09b` | `N/A` | 头部、执行检查、字段默认、Phase 2、审计表、`WC-06a`/`WC-07`/`WC-09b`、review log |
| 2026-11-18 11:14:47 UTC | R25 修复（Claude PASS；Codex 3 项 P1；受审 SHA-256 `ff0aebc3ede856bbfccb291a2041c87b83688db857de151be77fe2d2ef6edf4e`） | spike 守卫增加逐路径完整状态码与 `libra ls-files -s` 索引条目比较（覆盖仅索引变化）；D3 改按 `release.yml` 实际 job 集合（`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest`）取证，不再以 GitHub Release 附件为判据；`WC-09b` 文档门改为第 1–4 步与第 6 步 | `N/A` | 头部、执行检查（spike 守卫）、字段默认（D 组）、`WC-09b`、review log |
| 2026-11-18 11:20:10 UTC | R26 修复（Claude PASS；Codex 3 项 P1；受审 SHA-256 `9e279b221453b39bc434e5ec994cbbf2a6ae7165c9f2f853f8f2acc7a9e1c9f3`） | `WC-04` 按独立谓词重写为 6 条（`-f -f -d`、`-ffd`、上游 `t7300#30`/`#41` 等价、嵌套 Libra 仓库的保留与删除），显式 pathspec 与 `-n -d` 预览下的嵌套仓库保护拆出新卡 `WC-04a`（G-09；接入 clean 链与 `COMPATIBILITY.md` 链，`WC-05` 改依赖 `WC-04a`）；同口径把 `WC-03`、`WC-05a` 的上游多路径断言改为单条「上游用例等价」谓词（`WC-03` 6/8、`WC-05a` 3/8），并对含复合结果的 AC 从严重计（`WC-06` 7/8、`WC-09` 7/8、`WC-12` 8/8、`WC-13` 8/8）；D3 补各平台版本化对象、安装脚本与签名 manifest 的 CDN 完整 GET 与 size/sha256 核对；`WC-11R` 窗口证据改为远端 `main` 的 `W0`/`W1` 与祖先关系、`src/cli.rs` 区间 diff 为空 | `WC-04` → `WC-04`, `WC-04a` | 头部、文档职责、当前缺口、评审结论、实施顺序、依赖表、发布分组、Phase 0、字段默认（D 组）、审计表、`WC-03`/`WC-04`/`WC-04a`/`WC-05`/`WC-05a`/`WC-11R`、测试矩阵、追溯表、review log |
| 2026-11-18 11:23:55 UTC | R27 修复（Claude PASS；Codex 2 项 P1；受审 SHA-256 `d5aa8a832655827f0f910de8b825eab272059a781def114735ad1e71d7c81066`） | clean 链改为 `WC-01 -> WC-02 -> WC-04 -> WC-03 -> WC-04a -> WC-05 -> WC-05a`：`WC-04`（`-d` 路径嵌套仓库保护与双 force）先行，`WC-03` 在引入显式 pathspec 目录递归的同卡内新增 AC7 保证单 force 不删除嵌套仓库，`WC-04a` 只保留预览与显式 pathspec 双 force 删除（2/8），使任何中间发布都不会以单 force 递归删除嵌套仓库；`WC-11R` 窗口证据改为对 `rev-list --parents W0..W1` 的每个提交逐父边检查 `src/cli.rs`，端点 diff 仅作补充 | `N/A` | 头部、实施顺序、发布顺序、Phase 0、审计表、`WC-03`/`WC-04`/`WC-04a`/`WC-05`/`WC-11R`、review log |
| 2026-11-18 11:27:16 UTC | R28 修复（Claude PASS；Codex 1 项 P0 + 1 项 P1；受审 SHA-256 `9ccc6b36a92b3e85baa43c28aeff5dfa0ee6762cb23e75d5de8c57ab7ba47f87`） | 按「保护先于删除」重分 `WC-04`/`WC-04a` 并把 `WC-04` 移为 clean 链首卡（`WC-04 -> WC-01 -> WC-02 -> WC-03 -> WC-04a -> WC-05 -> WC-05a`）：`WC-04` 改为单 force 下全部删除路径（有/无 `-d`、有/无显式 pathspec、`-n` 预览）的嵌套仓库排除（7/8，新测试 `clean_single_force_keeps_nested_repos`，覆盖当前 `list_files` 仍列出嵌套仓库内文件的路径），`WC-04a` 改为 `-f -f` 接受与双 force 删除（5/8，含 `t7300#41` 与显式 pathspec 双 force，依赖 `WC-03`）；`WC-03` 去掉改由 `WC-04` 覆盖的 AC7（6/8）；`WC-11R` 窗口逐父边检查任一命中即 FAIL/STOP，归因只作诊断 | `N/A`（`WC-04`/`WC-04a` 编号与拆分关系不变，仅重分 AC） | 实施顺序说明、`WC-03`/`WC-04`/`WC-04a`/`WC-11R`、审计表、测试矩阵、追溯表、review log |
| 2026-11-18 11:31:06 UTC | R29 修复（Claude 1 项 P1；Codex 1 项 P1 + 1 项 P2；受审 SHA-256 `31ccf37fe67e76b0ba13bec2d50e955cf54dfaf79c4edcd615280c0e43fe05a6`） | 嵌套仓库单 force 保护按路径拆为 `WC-04`（无 pathspec 的有/无 `-d` 路径、`-x`/`-X` 忽略策略与 `-n -d` 预览，7/8）与新卡 `WC-04b`（显式 pathspec 指向嵌套仓库目录、其内文件与包含嵌套仓库的父目录，Git/Libra 两型，5/8；G-09 自 `WC-04` 拆出），所有断言改为嵌套仓库内全部文件的路径集合与 SHA-256 不变；clean 链改为 `WC-04 -> WC-04b -> WC-01 -> WC-02 -> WC-03 -> WC-04a -> WC-05 -> WC-05a`；`WC-05` AC6 断言 `nested/` 内全部文件内容不变；`WC-03` 写明目录候选内含嵌套仓库时不得整体删除并以 `WC-04b` 父目录断言防退化；头部状态更正（R28 修订时未同步） | `WC-04` → `WC-04`, `WC-04b` | 头部、文档职责、当前缺口、评审结论、实施顺序、依赖表、发布分组、Phase 0、审计表、`WC-01`/`WC-03`/`WC-04`/`WC-04b`/`WC-05`、测试矩阵、追溯表、review log |
| 2026-11-18 11:38:27 UTC | R30 修复（Claude PASS；Codex 1 项 P0 + 1 项 P1；受审 SHA-256 `a1f1b8a8e466fb757cd6506b57288ca36569e2071e5174a527ee6668e0161665`） | `WC-04` 与 `WC-04b` 改为 G-08 家族 `REL-WC-NEST`，新增发布点 `WC-04R`（`release`，2/12，T-2），两类删除路径的嵌套仓库保护只以一个版本同时发布；子卡改为 `family child`（本地提交、后端本地提交不推送），`DEP-WC-08` 冻结窗口与「文档与网站验收命令」第 5 步推广到两个家族；clean 链改为 `WC-04 -> WC-04b -> WC-04R -> WC-01 -> …`，新增边 `WC-04R -> WC-07`（家族窗口内不并发其它卡）；`WC-04b` AC3/AC4 改为断言对应嵌套仓库内全部文件不变 | `N/A`（新增发布点 `WC-04R`，不改动既有卡编号） | 头部、文档职责、评审结论、执行检查第 3–6 步、实施顺序、依赖语义、依赖表、发布分组、并发声明、Phase 0/1、字段默认、审计表、`WC-01`/`WC-04`/`WC-04b`/`WC-04R`/`WC-07`、追溯表、里程碑、风险、完成判据、review log |
| 2026-11-18 11:41:48 UTC | R31 修复（Claude PASS；Codex 3 项 P1；受审 SHA-256 `b32247bb4b9b56efb2ab1f2ca34b98495d11df84fee241fa8c365c377abb290c`） | `DEP-WC-05` 区分跨计划/家族外前序（须合入 `main`）与同一家族内前序子卡（已审本地签名提交放行），家族 `COMPATIBILITY.md` 窗口持有至发布点合入并取得 D 组结果；`WC-04R` 增加 release note 验收（AC3 + 发布前手工证据，C 组改用 `--notes-file`，3/12），部署核验标记改为字面量 `nested`；`WC-04` AC6/AC7 改为比较 `foo/`、`baz/boo/`（AC6 另含 `lib/`）内全部文件，`-X` fixture 增加多个匹配 `*.world` 的嵌套仓库文件；Phase 0 目标补嵌套仓库保护 | `N/A` | 头部、依赖表、字段默认（C 组）、默认覆盖表、Phase 0、审计表、`WC-04`/`WC-04R`、追溯表、review log |
| 2026-11-18 11:46:59 UTC | R32 修复（Claude 1 项 P1 + 1 项 P2；Codex 2 项 P1 + 1 项 P2；受审 SHA-256 `d29d830c401ef5893c937738b932ddd3ada5a772e001fc25f5eab59dbc586c31`） | `WC-08` AC3 的创建补丁改为字面量构造（`libra diff --no-index /dev/null` 当前输出修改补丁，`src/command/diff.rs:970-975,1099-1104`）；`WC-09` AC4 改为以仓库模式 `diff --cached --binary` 生成新建补丁、在新仓库删后重建（`t4012#12` 应用侧），生成侧登记 `DEFER-WC-05`（共享 `diff.rs`，承接 `plan-20261117-diff.md` `DIFF-41..44` 之后），具名测试改名为 `apply_binary_flag_and_creation_roundtrip`，成功定义把 `#837` 单列为部分闭环；`WC-04b` fixture 在卡内自含写出（G-05）；D3 增加 `WC-04R` 发布正文核对；`WC-04R` AC3 按两个谓词计（4/12） | `N/A` | 头部、成功定义、字段默认（D 组）、审计表、`WC-04b`/`WC-04R`/`WC-08`/`WC-09`、追溯表、非目标与延后项、review log |
| 2026-11-18 11:50:38 UTC | R33 修复（Claude PASS + 2 项 P2；Codex 4 项 P1；受审 SHA-256 `325613d7c4d973de36a388371466eb32d18c099a58fe77c7a1b99130848e64bd`） | `WC-04` AC2 改为 `-f -d` 后 `foo/`、`baz/boo/`、`lib/` 全部文件不变，AC7 的 `-X` fixture 增加 `lib/extra.world` 并比较三个嵌套仓库；`WC-09` AC1–AC3 每条使用独立、未应用的字面量补丁 fixture 对，要求两侧 exit 0，`--check` 断言不改状态、`--index` 断言工作区与索引等于 post（AC=8/8）；`DEP-WC-07` 窗口改为 `WC-11` 开工前取得并记录 `W0`，新增边 `DEP-WC-07 -> WC-11`，`WC-11` 依赖、审计表与 `WC-11R` AC4/窗口证据同步；关系表增列 `plan-20261117-diff.md`，`WC-09` 文档注明 `DEFER-WC-05` | `N/A` | 头部、关系表、实施顺序、依赖表、Phase 2、审计表、风险登记、`WC-04`/`WC-09`/`WC-11`/`WC-11R`、review log |
| 2026-11-18 12:09:45 UTC | R34 修复（Claude PASS + 1 项 P2；Codex 3 项 P1；受审 SHA-256 `c1b7e12be32f12418d1e8cd9e45e8dd25c2d4c8522eb07505c017cec4f850ea3`）；网站检出依据改记用户 2026-11-18 11:55:09 UTC 确认 | `WC-02` 写集、文档影响与依赖补 `COMPATIBILITY.md`/`DEP-WC-05`，fixture 写明 `excludes` 内容为 `excludes\n`、隔离其它忽略来源并加未配置对照；`WC-12` AC4 改为执行前后全量快照（工作区全部条目与索引）相同；全文专项清扫：「验证执行协议」新增第 9 条（仓库外隔离 HOME 直接调用二进制、禁用会在命令 cwd 下建 `.libra-test-home/` 的共享辅助函数，并定义全量快照），全部「保留/不修改/不删除」类映射改为全量快照比较，全部 fixture 写明文件内容与忽略来源隔离；按 GC-05 补 `WC-07`、`WC-08`、`WC-11`、`WC-11a` 的 `COMPATIBILITY.md` 写集与 `DEP-WC-05`，`apply` 链改为 `WC-06 -> WC-07 -> WC-08 -> WC-09`（取消 `WC-07`/`WC-08` 并发），`DEP-WC-05` 卡号清单、家族内前序、发布分组与兼容矩阵写入链同步；`WC-05` AC5 期望改为 Git 规范化小写键名 `clean.requireforce`；风险登记 `DEP-WC-07` 卡数更正为 23；「文档职责」与「使用规则」的网站检出依据改为用户 2026-11-18 11:55:09 UTC 确认，`AGENTS.md:135` 旧路径登记为已知文档滞后 | `N/A` | 头部、文档职责、使用规则、执行检查（验证执行协议第 9 条）、实施顺序、依赖表、发布分组、并发声明、Phase 1、审计表、`WC-01`..`WC-13` 各卡 fixture 与映射、`WC-02`/`WC-05`/`WC-07`/`WC-08`/`WC-09`/`WC-11`/`WC-11a`/`WC-11b`/`WC-12`/`WC-13` 字段、追溯表、风险登记、review log |
| 2026-11-18 12:17:23 UTC | R35 修复（Claude 3 项 P1 + 4 项 P2；Codex 3 项 P1；受审 SHA-256 `df7919aae56767717743004de368a063ce66a8bb426a50fc564a8cf94db3b5bd`） | 「验证执行协议」第 9 条全量快照增记权限位（含可执行位）；`WC-11b` AC8 改为全部子命令穷举分类单元测试（新增 `cli::tests::reset_recovery_guard_classifies_every_command`）加代表性 CLI 场景（`status`/`add`/`rm`/`clean`/`restore`/`commit`/`checkout`/`reset --hard`），`WC-11b` VER=5/8、`WC-11R` 聚合守卫增至 7 个具名测试（VER=9/12，映射条号顺延）；`WC-06a` 的 `tc0733` 场景改为 Go 与产品 No-Go 共用执行，非零退出按失败分类归档原始观测，产品 No-Go 时测试以 `#[ignore]` 标注 `WC-06b` 后提交；`WC-06` AC5 补上游 `libra add .` 步骤；`WC-10` AC5 写明 `t6415` `#1`/`#4`/`#5`/`#15` 的具体步骤（`reset --hard main` 位于 `#4`），AC6 写明游离 HEAD、二进制内容与 `#16` 遗留 post blob；`WC-11` AC5 写明 `t6411` 锚点并以 `libra add --chmod=+x` 平台无关地设置可执行位；`WC-05` AC3 去掉对未设置键的 `config unset`；`WC-02` 的 `core.attributesFile` 兼容说明移至 `check-attr` 条目；适用范围注明 `cli.rs` 既有测试模块与 `remove.rs`/`apply.rs` 新建测试模块 | `N/A` | 头部、适用范围、执行检查（验证执行协议第 9 条）、审计表、`WC-02`/`WC-05`/`WC-06`/`WC-06a`/`WC-10`/`WC-11`/`WC-11b`/`WC-11R`、测试矩阵、追溯表、review log |
| 2026-11-18 12:21:08 UTC | R36 修复（Claude PASS + 2 项 P2；Codex 4 项 P1；受审 SHA-256 `6d87ced6216ba1111f5e959077c6d7a2b538a3e2f3fedac79d731c18f13ede6e`） | `WC-06a` 增加运行期能力 No-Go 分流（探针 `present` 但 trace 不支持或字段缺失时归档原始 trace、转 `DEFER-WC-02` 守卫并以 `#[ignore]` 标注承接 ID 提交测试；探针 `absent` 时不编写该测试），描述同步；`WC-10` AC6 把上游 `git diff --binary`/`--full-index` 改为对应 `libra diff` 命令并注明实测形态；`WC-11b` AC8 分类测试改为递归遍历完整子命令路径与影响读写分类的参数分支，并以真实 argv 解析后调用中央分发前置守卫，CLI 场景补嵌套子命令写入口；`WC-11b` 共同前置固定恰好 2 个未完成恢复，AC1 断言 U1、U2 两条目且无其它条目；`WC-09a` AC5 写明在另一提交 v3 的新仓库执行 | `N/A` | 头部、`WC-06a`/`WC-09a`/`WC-10`/`WC-11b`、review log |
| 2026-11-18 12:25:24 UTC | R37 修复（Claude 1 项 P1 + 4 项 P2；Codex 2 项 P1；受审 SHA-256 `cd99727d9699885aed90017943dd8604306ef70472e3782e855ff3c5b1583bef`） | `WC-11b` 共同前置写明 U2 由仅 debug 构建且 `LIBRA_TEST=1` 时生效的测试 seam 让第二次 `reset --hard` 跳过守卫后产生（生产构建不存在；协议若使多个未完成恢复不能并存，则由 `WC-11s` 删去多 UUID 断言并重新计数）；`WC-11a` 共同前置补草案约束：failpoint 仅 debug 构建生效，「重开」指不受守卫拦截的恢复入口，移动 ref 等外部干预经 debug-only seam 或库 API 完成；`WC-13` AC3 扩为 AC1 与 AC4（SIGPIPE 子场景）每次执行前后全量快照相同（计数不变）；`WC-06a` Verification 第 2 条标题改为 Go、产品 No-Go 与运行期能力 No-Go 共用；按「描述承诺必有 AC」清扫：`WC-01` 新增 AC5（`clean -f ../src` 的 `Removing` 行保留 `../`，AC=6/8），`WC-05` AC1 覆盖 `no`/`off`/`0`，`WC-11` 新增 AC6 以既有 `command::merge_test::merge_gitlink_hard_reset_keeps_a_file_left_where_the_index_records_a_pointer` 守住 `ADR-MG-01` gitlink 排除（AC=6/8、VER=3/8），`WC-11R` 聚合 8 个具名测试（VER=10/12，映射条号顺延） | `N/A` | 头部、审计表、`WC-01`/`WC-05`/`WC-06a`/`WC-11`/`WC-11a`/`WC-11b`/`WC-11R`/`WC-13`、追溯表、review log |
| 2026-11-18 12:28:20 UTC | R38 修复（Claude PASS + 2 项 P2；Codex 1 项 P1；受审 SHA-256 `76ef45bf940d8d7829bf6f898929784b107b77c82168968c9b40da5f2314a2bf`） | 「验证执行协议」第 5 条共享回归组计数改为分别记录过滤未选中数与匹配但被 `#[ignore]` 忽略数（`cargo nextest list` 与 `--run-ignored all` 之差），`skipped` 须等于二者之和，忽略用例逐条登记且只允许既有忽略或 `WC-06a` No-Go 分支显式允许的 `tc0733` 忽略，消除与 `WC-06a` No-Go 后运行 `command::mv_test::` 回归组的矛盾 | `N/A` | 头部、执行检查（验证执行协议第 5 条）、review log |
| 2026-11-18 12:32:37 UTC | R39 修复（Claude PASS + 2 项 P2；Codex 分区 A 2 项 P1、分区 B 2 项 P1；受审 SHA-256 `d63dbbf0a93a0753696ce5ca09578d17535e52b7c1ecab1b50c7f19f905c61d9`） | spike 守卫新增第（4）类证据：不受忽略规则影响地遍历 `src/`、`sql/`、`.github/` 与 `build.rs`、`install.sh`、`install.ps1`、`Cargo.toml`、`Cargo.lock`、`rustfmt.toml` 并比较类型、权限与内容（脚本 `snap` 同步，目录条目单列 `dir`），堵住被 `target*` 等规则忽略的新生产文件；「文档与网站验收命令」第 2 步增加 `libra fetch origin` 后的 HEAD 与 `origin/cf` 比对（落后先 `pull --ff-only`，存在无关未推送提交即 STOP；家族后续卡只允许本家族子卡提交）与无暂存条目检查，第 4 步推送前要求 `origin/cf..$FINAL` 恰为本卡提交，第 5 步 `BASE` 须等于当时的 `origin/cf` 且发布前 `origin/cf` 未变、`BASE..HEAD` 恰为子卡提交；`WC-09` 共同前置改写为「补丁效果是 old→new，执行前 `t.txt` 保持 `old\n`」；完成判据的发布项改为非延后卡与 `WC-04R`，`WC-11R` 仅 Go 路径适用 | `N/A` | 头部、执行检查（spike 守卫、文档与网站验收命令第 2/4/5 步）、`WC-09`、完成判据、review log |
| 2026-11-18 12:36:14 UTC | R40 修复（Claude PASS + 2 项 P2；Codex 分区 A 1 项 P1 + 1 项 P2，分区 B 未发现 P0/P1 但以覆盖不全判 FAIL；受审 SHA-256 `037b9fe44243bef22031462adc61700f6aa76bafb11add992b37a7e0e0bcc312`） | spike 守卫第（4）类生产表面遍历改为 `os.walk(..., onerror=…)` 重新抛出目录读取错误并记录生产根目录本身，不可读即非零退出（已在临时仓库实测）；「文档与网站验收命令」第 6 步要求部署核验标记在改动前线上页面中命中数为 0，已存在的写明标记（如 `--3way`、`clean.requireForce`）改用本卡新增字面短语，未写明时同样取新增短语；`WC-06a` 上游 `t6421#1` 行号更正为 `:209-246` | `N/A` | 头部、执行检查（spike 守卫、文档与网站验收命令第 6 步）、`WC-06a`、review log |
| 2026-11-18 12:40:45 UTC | R41 修复（Claude PASS + 2 项 P2；Codex 四分区：P2 PASS，P1、P3、P4 各 1 项 P1；受审 SHA-256 `16b6a11509a0fed44b2e05b2d424d4d8d73d0c07ed9ef0c1260286cf7318c01b`） | spike 守卫内容快照的路径集合并入 `libra ls-files -s` 的全部 stage 路径（覆盖被忽略的未合并文件）；「验证执行协议」第 8 条加入「仅 Unix」并要求平台条件以独立具名测试承载；`WC-10` 拆出仅 Unix 的 `command::reset_test::reset_hard_main_t6415_merge_keeps_untracked`（AC5）与平台无关的 `command::reset_test::reset_hard_main_t4108_binary_threeway`（AC6），VER=8/8；`WC-13` 拆出仅 Unix 的 `command::remove_test::remove_dry_run_sigpipe_keeps_index`（AC3 的 AC4 执行、AC4、AC5），原测试改名 `command::remove_test::remove_dry_run_short_flag`，VER=4/8；`WC-11` AC5 在 merge 后断言 `file1` 恰为 stage 1/2 且状态为 `UD` | `N/A` | 头部、执行检查（验证执行协议第 8 条、spike 守卫）、审计表、`WC-10`/`WC-11`/`WC-13`、追溯表、review log |
| 2026-11-18 12:44:22 UTC | R42 修复（Claude PASS + 2 项 P2；Codex 四分区：P1/P2/P3 PASS，P4 1 项 P1；受审 SHA-256 `75893fb203f074cf2964a831675e907bd0319748def4630f3870b441cad4b14c`） | `WC-11a` AC3/AC4 映射改为按 UUID 事务提交回执唯一决定结果：提交前强杀完整回滚到原冲突工作区与 unmerged 索引，提交后、`COMMITTED` 前强杀按已提交处理（工作区与索引等于 HEAD 树），测试按注入点逐一断言对应结果；按协调者转述的 v2.12 证据文件口径，D3 的 CDN 完整 GET 改为管道在同一进程内计算字节数与 SHA-256（不落盘），`WC-11R` 窗口核对改为 `WINDOW=$(libra rev-list --parents …)` 命令替换，不再写 `window.txt` | `N/A` | 头部、字段默认（D 组）、`WC-11a`/`WC-11R`、review log |
| 2026-11-18 12:47:02 UTC | R43 修复（Claude PASS + 2 项 P2；Codex 四分区：P2/P3/P4 PASS，P1 2 项 P1；受审 SHA-256 `a0346d31fc6d7fbd1f27e0ea9e35dfeda6734783de73864042106e23d7c779eb`） | D3 的 CDN 核对回退为 R42 已获 P1 分区 PASS 的原文（`curl -fsSL --retry 3 -o <file> -w '%{http_code}' <url>` 完整 GET 后核对 HTTP 200、manifest 验签、条目与 size/sha256）：R42 改写的管道只输出长度与摘要，无法断言 HTTP 200，也无法完成 manifest 验签与条目解析；下载对象是待核对的发布产物本身，不是证据文件，结果写入本卡记录。`WC-11R` 窗口的命令替换改写保留（P4 分区 PASS） | `N/A` | 头部、字段默认（D 组）、review log |
| 2026-11-18 12:48:20 UTC | R44 双审 PASS 记账（受审 SHA-256 `a710c78a48098c5f123faaadd8dc280c201ffbf229e9de17536917ce4b2a92e5`） | 仅更新头部状态、本行与 review log 的 PASS 行，计划正文未改 | `N/A` | 头部、修订历史、review log |

## 已决议设计决策

### ADR-WC-01: `is_locked_revision` 仅锁定 AI 托管分支，不锁定 `DEFAULT_BRANCH` (`main`)

- **Status:** Accepted
- **Context:** `src/internal/branch.rs:70-90` 注释明确指出 `main` 应保持为普通用户工作区分支，但 `is_locked_revision` 错误调用了包含 `DEFAULT_BRANCH` 的 `is_locked_branch`，导致 `reset main` / `reset --hard main` 与 `restore --source=main` 被拒绝（#838, #1013）。
- **Decision:** 将 `is_locked_revision` 改为调用 `is_ai_managed_branch`（仅锁定 `intent`、`traces`、`agent-traces` 及其修订后缀），保留 `branch -D/-m main` 的分支管理保护，同时更新 `src/internal/branch.rs:1199` 的过时单元测试断言。
- **Alternatives considered:** 仅在 `reset.rs` 特判跳过 `main`——拒绝，因 `restore.rs:485` 等也消费 `is_locked_revision`（GC-02）。
- **Consequences:** `reset --hard main` 与 `restore --source=main` 恢复正常。
- **Revisit when:** 新增 AI 托管锁定分支名称时。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01..13`。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01..14`。

- **验证执行协议（对全计划每条 `Verification` 条目强制；条目以「具名测试」「共享回归组」「守卫」「手工证据」标注类别）：**
  1. 具名测试：过滤必须是完整测试名的精确匹配 `-E 'test(=<完整名>)'`。集成测试完整名为 `command::<模块>::<函数>`（`tests/command_test.rs:10` 的 `mod command` 加 `tests/command/mod.rs` 的模块声明）；库单元测试完整名为 crate 内模块路径加 `tests::<函数>`（如 `internal::branch::tests::is_locked_revision_strips_suffixes`）；禁止用裸函数名、模块名或前缀代替。
  2. 具名测试运行前，以同一 target 与过滤执行 `cargo nextest list`（例：`source .env.test && cargo nextest list --test command_test -E 'test(=command::clean_test::clean_relative_pathspec_prefix)'`），列出的测试必须恰好 1 条且与预期完整名逐字一致；0 条（含新测试尚未写出）或多于 1 条即 STOP，不得记为通过。
  3. 具名测试运行后记录 exit code 与「选中 / 通过 / 失败 / 过滤未选中」计数：选中数以第 2 条 `list` 结果为准；要求 exit code = 0、汇总的实际运行数 = 选中数、通过 = 选中数、失败 = 0。nextest 汇总中的 `skipped` 只允许等于同一测试二进制里未被过滤表达式选中的测试数（记录该数）；另以同一过滤执行 `cargo nextest list --run-ignored all`，其匹配数必须与第 2 条相同（证明选中测试无 `#[ignore]`）。任一不满足即 STOP。
  4. 在本卡执行记录中逐条写出该测试覆盖的 AC 编号与实际断言结果（以卡内「AC → 断言映射」为准），不得只写测试名。
  5. 共享回归组：过滤为模块前缀正则（`test(/^command::<模块>::/)`、`test(/^<库模块>::tests::/)`）或整个 `--test <target>`；先以同一参数分别执行 `cargo nextest list`（默认只列非忽略用例，得选中数 N，必须 ≥ 1）与 `cargo nextest list --run-ignored all`（得匹配总数 M），I = M − N 为「匹配但被 `#[ignore]` 忽略」的用例数；运行后记录 exit code 与「选中 / 通过 / 失败 / 过滤未选中 / 匹配但忽略」计数，要求 exit code = 0、实际运行数 = N、通过 = N、失败 = 0，汇总的 `skipped` = 同一测试二进制中过滤未选中数 + I；I 中的每个用例逐条记录名称与忽略理由，且必须是执行前已存在的 `#[ignore]` 用例或本计划显式允许的忽略（目前仅 `WC-06a` 两种 No-Go 分支下带 `TC-0733: blocked by …` 标注的 `command::mv_test::tc0733_merge_partial_clone_full`），出现其它新增忽略即 STOP；共享回归组不计为卡特有 AC 证据。
  6. 守卫与手工证据：按条目给出的命令或核对步骤执行；`rg` 零命中守卫一律使用模板「Verification 判定口径」的退出码模板（`0` 命中、`1` 零命中、`>1` 执行失败即 STOP）；会以非零码表达正常结果的命令（如 `diff` 的 `1`）必须单独捕获退出码，不得放在 `pipefail` 管道中间；守卫产生的临时文件只放仓库外 `mktemp -d` 目录，不入库、不作为证据文件。本类条目不适用第 2–3 条。
  7. fmt、clippy 与全量 nextest 属 ER-04 C 组与 ER-13/ER-14，不登记为 `Verification` 条目，也不适用第 2 条的「恰好 1 条」。
  8. 条件分支：标注「仅 Go 路径」「仅 No-Go 路径」「仅 Unix」等的条目只在对应分支或平台执行；未执行的分支或平台在本卡执行记录中写明不适用依据，不得记为通过。平台条件以独立具名测试承载（整个测试以 `#[cfg(unix)]` 编译），不得把仅在某平台执行的断言放进跨平台具名测试的条件子块，以免非目标平台上该测试通过却未执行对应 AC。
  9. 调用隔离与全量快照（本计划新增或改写的 `command_test` 用例强制；卡内「AC → 断言映射」中的「隔离运行」「全量快照」均指本条）：（a）隔离运行：fixture 构造与被测命令一律经本测试文件内的私有辅助函数直接执行 `env!("CARGO_BIN_EXE_libra")`，`env_clear()` 后设置基础变量集 `PATH=/usr/bin:/bin:/usr/sbin:/sbin`、`HOME`、`USERPROFILE`、`XDG_CONFIG_HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB`、`LANG=C`、`LC_ALL=C` 与 `LIBRA_TEST=1`（与 `tests/command/mod.rs:353-382` `base_libra_command` 相同），但 HOME 与两个配置库位于被测仓库之外的独立 `tempdir()`（先例：`tests/command/clone_cli_test.rs:39` `run_libra_with_home`）；卡内写明的附加变量（如 `GIT_TRACE2_PERF`、failpoint 变量、`LIBRA`）在基础变量集之上追加，需要 shell 的场景以同一变量集运行 `sh`。不得使用 `run_libra_command*`、`create_committed_repo_via_cli` 等会在命令 cwd 下创建 `.libra-test-home/` 的共享辅助函数（该目录含 `.libra/`；2026-11-18 以 libra 0.30.207 实测 `clean -n -d` 会输出 `Would remove .libra-test-home`），也不修改 `tests/command/mod.rs`（避免 ER-13 T-4）。因此全局/系统配置与 XDG 来源为空：忽略规则只来自 fixture 显式写出的文件，fixture 未写明的 `.gitignore`、`.libraignore`、`.gitattributes` 与 `core.excludesFile` 一律不存在，`libra init` 生成的 `.libra/info/exclude` 只含注释（`template/exclude`）；需要提交的 fixture 先执行 `libra config user.name "Test User"` 与 `libra config user.email test@example.com`，再以 `libra commit --no-verify -m <msg>` 提交。（b）全量快照：在被测仓库根目录，对工作区内除该仓库自身元数据目录 `.libra/` 外的全部条目（含未跟踪与被忽略路径、空目录、嵌套仓库及其 `.git/`、`.libra/` 下的内容）逐条记录相对路径、类型（文件/目录/符号链接）、权限位（Unix 为 `st_mode & 0o7777`，含可执行位；非 Unix 为只读属性）、普通文件字节的 SHA-256 与符号链接目标，并记录 `libra ls-files -s` 的完整输出。「全量快照相同」要求执行前后两份记录逐项相等；「全量快照恰好少了 X」要求执行后的工作区记录等于执行前记录去掉 X（X 为目录时含其下全部条目），且除映射另写的索引期望外 `libra ls-files -s` 输出不变。快照与比较同样以本测试文件内的私有辅助函数实现。

- **spike 产物 allowlist 守卫（ER-04 B 组 `spike` 行；`WC-06a`、`WC-11s` 引用）：** 在被检仓库根目录以 `bash` 执行：开工前 `export SCRATCH=$(mktemp -d)` 并执行 `snap before`，结束时执行 `snap after` 与 `compare`；`$SCRATCH/allow.txt` 为本卡 `Deliverables` 中属于该仓库的相对路径（每行一个）。守卫同时核对四类证据：（1）完整 `libra status --short`：逐路径比较开工前后的完整状态码（含暂存/未暂存两列），任何路径的状态出现、消失或改变都必须在 allowlist 内；开工时已存在的 allowlist 外改动由 `compare` 列为「PRE-EXISTING」，须按 ER-01 在本卡记录并归因于他人，不得由本卡改动；（2）内容快照：对 `libra ls-files`（已跟踪，stage 0）、`libra ls-files --others --exclude-standard`（未忽略的未跟踪）与 `libra ls-files -s`（含 stage 1–3 未合并条目，即使其路径匹配忽略规则）列出的全部路径，逐条记录类型、权限位与普通文件 SHA-256 或链接目标（文件缺失记为 `missing`）；开工前后任一路径的新增、删除、内容、类型、权限或链接目标变化（含开工前已脏文件的再次改动）都必须在 allowlist 内；（3）索引：逐路径比较 `libra ls-files -s` 的全部条目（模式、OID、stage），仅索引变化（暂存或取消暂存而工作区字节不变）同样计为变化，必须在 allowlist 内。（2）（3）的范围由仓库忽略规则决定（构建产物等被 `.gitignore`/`.libraignore` 中 `target*` 等规则排除的路径不在其内）；（4）生产表面快照不受忽略规则影响：对 `src/`、`sql/`、`.github/` 下全部条目（`os.walk` 遍历，含被忽略路径与目录条目，不跟随符号链接；以 `onerror` 重新抛出目录读取错误（Python 默认静默忽略），生产根目录本身也记录类型与权限位，任一目录或文件不可读即非零退出并 STOP）及 `build.rs`、`install.sh`、`install.ps1`、`Cargo.toml`、`Cargo.lock`、`rustfmt.toml` 逐条记录类型、权限位与普通文件 SHA-256 或链接目标，开工前后任一新增、删除或变化即越界（这些路径永不进入 allowlist；例如 `src/` 下匹配 `.libraignore:58` `target*` 规则而被忽略的新文件同样被发现）。任一 `libra`/`python3` 步骤失败、路径不可读均以非零退出并 STOP；越界即 FAIL 并逐条列出。由于 allowlist 不含 `src/`、`sql/`、`build.rs`、`install.sh`/`install.ps1`、`Cargo.toml`/`Cargo.lock`、`.github/`、`rustfmt.toml`，且（4）覆盖这些路径下被忽略的条目，本守卫同时证明生产表面零改动（被检仓库不含这些路径时（4）为空集）。临时目录位于仓库外，不入库、不作为证据文件。

```bash
set -euo pipefail
snap() {   # 用法：snap before（开工前）；snap after（结束时）
  libra status --short > "$SCRATCH/status-$1.txt"
  libra ls-files -z > "$SCRATCH/tracked-$1.bin"
  libra ls-files -z --others --exclude-standard > "$SCRATCH/untracked-$1.bin"
  libra ls-files -s -z > "$SCRATCH/index-$1.bin"
  python3 -I - "$SCRATCH" "$1" <<'PY'
import hashlib, os, stat, sys
d, tag = sys.argv[1], sys.argv[2]
paths = set()
for f in (f'{d}/tracked-{tag}.bin', f'{d}/untracked-{tag}.bin'):
    paths.update(p for p in open(f, 'rb').read().decode('utf-8').split('\0') if p)
paths.update(r.split('\t', 1)[1] for r in open(f'{d}/index-{tag}.bin', 'rb').read().decode('utf-8').split('\0') if r)   # every stage, incl. unmerged
for root in ('src', 'sql', '.github', 'build.rs', 'install.sh', 'install.ps1', 'Cargo.toml', 'Cargo.lock', 'rustfmt.toml'):
    if os.path.lexists(root):
        paths.add(root)                                     # (4) the root itself: type and mode
    if os.path.isdir(root) and not os.path.islink(root):   # (4) production surfaces, ignore rules NOT applied
        def _fail(e): raise e                               # os.walk ignores scandir errors by default
        for dp, dns, fns in os.walk(root, onerror=_fail):
            paths.update(os.path.join(dp, n) for n in dns + fns)
rows = []
for p in sorted(paths):
    try:
        st = os.lstat(p)
    except FileNotFoundError:
        rows.append(f'{p}\tmissing\t-\t-'); continue
    mode = oct(st.st_mode & 0o7777)
    if stat.S_ISLNK(st.st_mode): rows.append(f'{p}\tlink\t{mode}\t{os.readlink(p)}')
    elif stat.S_ISREG(st.st_mode):
        with open(p, 'rb') as fh: rows.append(f'{p}\tfile\t{mode}\t{hashlib.sha256(fh.read()).hexdigest()}')
    elif stat.S_ISDIR(st.st_mode): rows.append(f'{p}\tdir\t{mode}\t-')
    else: rows.append(f'{p}\tother\t{mode}\t-')
open(f'{d}/{tag}.txt', 'w').write('\n'.join(rows) + '\n')
PY
}
compare() {
  python3 -I - "$SCRATCH" <<'PY'
import sys
d = sys.argv[1]
load = lambda n: {l.split('\t', 1)[0]: l for l in open(f'{d}/{n}.txt').read().splitlines() if l}
def status_map(n):   # 路径 -> 完整状态码列表（XY）
    m = {}
    for l in open(f'{d}/status-{n}.txt').read().splitlines():
        if len(l) > 3:
            for x in l[3:].split(' -> '): m.setdefault(x.strip(), []).append(l[:2])
    return m
def index_map(n):    # 路径 -> 索引条目列表（mode oid stage）
    m = {}
    for rec in open(f'{d}/index-{n}.bin', 'rb').read().decode('utf-8').split('\0'):
        if rec:
            meta, path = rec.split('\t', 1); m.setdefault(path, []).append(meta)
    return m
diff = lambda x, y: {p for p in set(x) | set(y) if sorted(x.get(p, [])) != sorted(y.get(p, []))}
b, a = load('before'), load('after')
sb, sa, ib, ia = status_map('before'), status_map('after'), index_map('before'), index_map('after')
allow = {l.strip() for l in open(f'{d}/allow.txt') if l.strip()}
changed = {p for p in set(a) | set(b) if a.get(p) != b.get(p)} | diff(sb, sa) | diff(ib, ia)
bad = sorted(changed - allow)
print('\n'.join(bad) if bad else 'OK: all changes within Deliverables')
print('PRE-EXISTING (record and attribute per ER-01):', ', '.join(sorted(set(sb) - allow)) or 'none')
sys.exit(1 if bad else 0)
PY
}
```

- **文档与网站验收命令（ER-06/ER-06a；各卡「文档门」引用本节步骤编号）：**
  1. 逐文件核对（每卡执行，含家族子卡）：对本卡 `Docs and compatibility impact` 列出的每个 Libra 仓文件记录改动前后 `shasum -a 256 <path>`，并逐文件确认新增或修改的章节覆盖本卡行为（参数、输出、错误、示例）；`COMPATIBILITY.md` 只允许在既有条目行内追加（`DEP-WC-05`），改动前后 `wc -l < COMPATIBILITY.md` 必须相等；缺任一文件、内容不符或行数变化即 STOP。
  2. 后端前置（每次写后端前）：`test -d /Volumes/Data/gitmono/libra-backend/.libra && test ! -e /Volumes/Data/gitmono/libra-backend/.git` 必须成功，且 `(cd /Volumes/Data/gitmono/libra-backend && libra status --short --branch)` 首行以 `## cf` 开头；不在 `cf` 时按 ER-06a 执行 `(cd /Volumes/Data/gitmono/libra-backend && libra switch cf)` 后重检；切换受阻、两种元数据并存或都不存在即 STOP，禁止改用其它检出。随后在后端执行 `libra fetch origin`，比较 `libra rev-parse HEAD` 与 `libra rev-parse refs/remotes/origin/cf`：独立卡、`WC-06a` 与家族首子卡要求二者相等（`libra merge-base --is-ancestor HEAD refs/remotes/origin/cf` 退出 0 即本地落后，先 `libra pull --ff-only origin cf` 再重检；HEAD 含 `origin/cf` 之外的提交即存在无关的未推送本地提交，STOP，不得把它们随本卡推送）；家族非首子卡与发布点要求 `libra rev-list "refs/remotes/origin/cf..HEAD"` 恰为本家族此前子卡记录的后端本地提交；另要求 `libra status --short` 中没有已暂存条目（任一行首列不是空格或 `?` 即 STOP），他人未暂存的无关改动保持原样、不得暂存。
  3. 网站同步与前端验证：在后端执行 `python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --backend-root /Volumes/Data/gitmono/libra-backend --write --only <本卡命令页>`，再以相同参数去掉 `--write` 执行检查，要求退出 0（1 = 仍有漂移，2 = 输入不受支持或安全拒绝，均 STOP）；随后在后端根目录执行 `pnpm typecheck` 与 `pnpm build`，均须退出 0。家族子卡（`WC-04`、`WC-04b`、`WC-11`、`WC-11a`、`WC-11b`）同样完成本步（含 `--write`、检查与 `typecheck`/`build`）。
  4. 网站评审与提交（所有写后端的卡；家族子卡只到本地签名提交，推送由各自发布点执行，见第 5 步）：对第 3 步得到的最终网站字节，按「路径 + `shasum -a 256`」清单取得独立 Claude Code `VERDICT: PASS`；随后精确 `libra add <已审路径>` 与 `libra commit -s -m "<msg>"`，记录 `FINAL=$(libra rev-parse HEAD)`；对每个已审路径要求 `libra show "$FINAL":<path> | shasum -a 256` 等于评审清单中的值；按 ER-07 校验 `$FINAL` 的 `gpgsig` 与 `Signed-off-by`；独立卡与 `WC-06a` 随后 `libra fetch origin` 并要求 `libra rev-list "refs/remotes/origin/cf..$FINAL"` 恰为 `$FINAL` 一个提交（否则 STOP，不推送），再 `libra push origin cf`，推送后要求 `libra ls-remote origin refs/heads/cf` 指向 `$FINAL`。
  5. 家族规则（`REL-WC-NEST` 与 `REL-WC-RESET`，配合 `DEP-WC-08`；下文以「首子卡 / 发布点 / 家族页面」分别指 `WC-04` / `WC-04R` / `apps/tanstack-app/content/docs/commands/clean.en.md` 与 `WC-11` / `WC-11R` / `apps/tanstack-app/content/docs/commands/reset.en.md`）：首子卡开工前由协调者授予 `DEP-WC-08` 后端 `cf` 推送冻结窗口，按第 2 步确认后端 HEAD 等于 `refs/remotes/origin/cf` 后记录 `BASE=$(libra rev-parse HEAD)`；每张子卡完成第 1–4 步（含本卡页面字节的独立 Claude Code `VERDICT: PASS`、精确暂存、签名+DCO 本地提交与提交树哈希比对），但不推送；窗口内其它交付者不得在该后端检出提交或推送 `cf`（推送会携带家族未发布的本地提交，他人的本地提交也会被发布点的推送一并公开）。发布点在 CLI 发布后：重新执行第 3 步并要求后端 `libra status --short` 对家族页面无改动（最终页面字节已由子卡提交）；以 `libra diff --name-only "$BASE" HEAD` 核对家族网站路径集合恰为家族页面，并要求 `libra fetch origin` 后 `refs/remotes/origin/cf` 仍等于 `$BASE`、`libra rev-list "$BASE..HEAD"` 恰为各子卡记录的后端本地提交；记录 `FINAL=$(libra rev-parse HEAD)`（后端，本步重新取得，不沿用子卡的局部值），对 `libra show "$FINAL":<家族页面> | shasum -a 256` 所得字节取得聚合独立 Claude Code `VERDICT: PASS`；再 `libra push origin cf` 并要求 `libra ls-remote origin refs/heads/cf` 等于 `$FINAL`，随后以同一 `$FINAL` 执行第 6 步并由协调者释放 `DEP-WC-08`。路径集合不符、他人在窗口内推送或任一核对失败即 STOP。
  6. 部署核验（独立卡、`WC-06a` 与发布点 `WC-04R`、`WC-11R`）：若 `cf` 推送未自动部署，则在 `libra rev-parse HEAD` 等于 `$FINAL` 且 `libra status --short` 为空的后端检出执行 `pnpm run deploy:cf`（检出不干净时不得部署，STOP 并上报，避免把他人未提交改动一并部署） 并记录 wrangler 输出的部署/版本 ID；自动部署时记录其构建所用提交并要求等于 `$FINAL`。随后 `curl -fsSL https://libra.tools/en/docs/commands/<页面>` 的输出必须包含部署核验标记（卡内写明的标记；未写明时为本卡在英文命令页新增的一段字面短语，执行时写入记录）。标记必须是本次交付新增的：推送前先以 `curl -fsSL https://libra.tools/en/docs/commands/<页面> | grep -cF -- '<标记>'` 记录改动前线上页面的命中数，须为 0；卡内写明的标记若改动前已存在（如 2026-11-18 线上 `apply` 页已含 `--3way`、`clean` 页已含 `clean.requireForce`），改用本卡新增的一段字面短语并写入记录；无法证明部署源树为 `$FINAL` 或标记缺失即 STOP（不逐字节比较 Markdown 与 HTML）。
  7. STOP 条件汇总：逐文件哈希或行数不符、同步检查非零、家族网站路径集合不符或冻结窗口被打破、`typecheck`/`build` 失败、Claude Code 未给出 `VERDICT: PASS`、提交树与已审字节不一致、签名/DCO 缺失、`cf` 推送失败、部署无法绑定 `$FINAL` 或标记缺失；STOP 后不得标 `complete`，并在本计划 review log 记录失败环节。
  8. 计划文本提交门（Claude（Subagent）与 Codex 对同一字节双 `PASS`）与第 4 步的网站 Claude Code `PASS` 互不替代。

## 实施顺序

依赖边 `A -> B` 表示 A 必须先于 B；各卡 `Dependencies` 与下列边一致（G-06）。

- `clean` 链：`WC-04 -> WC-04b -> WC-04R -> WC-01 -> WC-02 -> WC-03 -> WC-04a -> WC-05 -> WC-05a`（`WC-04`、`WC-04b` 为 `REL-WC-NEST` 家族子卡，经发布点 `WC-04R` 一次发布，且先于任何其它 clean 卡：让单 force、预览与无标志模式的全部删除路径（`WC-04`：有/无 `-d` 的无 pathspec 路径与预览；`WC-04b`：指向嵌套仓库、其内文件或父目录的显式 pathspec）排除嵌套仓库，因此任何中间发布都不会以单 force 删除嵌套仓库内容；双 force 删除在 `WC-03` 引入显式 pathspec 目录递归之后由 `WC-04a` 提供）
- `mv`：`WC-05a -> WC-06`
- `apply` 链：`WC-06 -> WC-07 -> WC-08 -> WC-09 -> WC-09a -> WC-09b`（`WC-07`、`WC-08` 同样写 `COMPATIBILITY.md`，接在 `WC-06` 之后串行；`WC-04R` 经 clean 链与 `WC-06` 传递先于全部 apply 卡，`REL-WC-NEST` 窗口内不插入其它卡）
- `reset`/`restore`：`WC-09b -> WC-10`（`WC-10` 的 `t4108#17` 消费 `WC-09a`，经 `WC-09b` 传递；串行化 `COMPATIBILITY.md`）
- `rm` 与 `TC-0733` 核验：`WC-10 -> WC-12 -> WC-06a -> WC-13`；`WC-06 -> WC-06a`；`WC-12 -> WC-13`
- `reset` 家族（置于链尾，不阻塞其它卡）：`WC-13 -> WC-11s -> WC-11 -> WC-11a -> WC-11b -> WC-11R`；`WC-11s -> WC-11a`；`WC-11s -> WC-11b`
- 跨计划边：`DEP-WC-03 -> WC-02`；`DEP-WC-01 -> WC-10`；`DEP-WC-04 -> WC-10`；`DEP-WC-06 -> WC-06a`；`DEP-WC-07 -> WC-11`；`DEP-WC-07 -> WC-11b`；`DEP-WC-07 -> WC-11R`；`DEP-WC-08 -> WC-04`；`DEP-WC-08 -> WC-04R`；`DEP-WC-08 -> WC-11`；`DEP-WC-08 -> WC-11R`；`DEP-WC-05` 作用于每张写 `COMPATIBILITY.md` 的卡；`WC-10` 先于 `plan-20261117-plumbing-misc.md#PM-21a` 合入 `branch.rs`（`DEP-WC-01`，对方 `DEP-PM-06`，单向无环）。
- `COMPATIBILITY.md` 写入者全部位于同一条链上：`WC-04 -> WC-04b -> WC-01 -> WC-02 -> WC-03 -> WC-04a -> WC-05 -> WC-05a -> WC-06 -> WC-07 -> WC-08 -> WC-09 -> WC-09a -> WC-09b -> WC-10 -> WC-12 -> WC-06a -> WC-13 -> WC-11 -> WC-11a -> WC-11b`（均为上列边的传递闭包）。
- **依赖语义:** 边 `A -> B` 默认要求 A 达到 `Lifecycle=done` 且 `Acceptance=complete`。以下前驱例外，只要求 A 达到 `locally-accepted` 且已取得 ER-05 评审 PASS（本地提交完成）：`no-release` 卡 `WC-06a`（其 C/D 由后继 `WC-13` 承载）、`no-release` 卡 `WC-11s`（Go 判定与修订双 PASS 即满足后继条件，其 C/D 由 Go 路径的 `WC-11R` 或 No-Go 路径的 `M3` 承载）、家族子卡 `WC-04`、`WC-04b`（其 C/D 由 `WC-04R` 承载）与 `WC-11`、`WC-11a`、`WC-11b`（其 C/D 由 `WC-11R` 承载）。承载点取得 C 后继承卡进入 `remote-pending`，承载点 D 全绿后在同一变更内把继承卡同步推进到 `complete`/`done`，因此不存在完成状态互等。

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| `DEP-WC-01` | `incoming` | 跨计划 | `plan-20261117-branch.md#BR-10, BR-22`；顺序约束同时作用于 `plan-20261117-plumbing-misc.md#PM-21a`（`REL-PM-21` 家族） | Branch 计划执行者 | `BR-10`/`BR-22` 对 `src/internal/branch.rs` 与 `COMPATIBILITY.md` 的写入已合并；随后 `WC-10` 合入 `main`，再解除 `PM-21a` 阻塞（单向无环） | `plan-20261117-branch.md#DEP-BR-12`；`plan-20261117-plumbing-misc.md#DEP-PM-06` | 超时 7 日上报协调者；未完成 `BR-10`/`BR-22` 前 `WC-10` 置 `blocked`，禁止并发写入（G-10） |
| `DEP-WC-03` | `incoming` | 跨计划（start-to-completion） | `plan-20261117-global-init-infra.md#GII-05, GII-09` | Global-Init-Infra 计划执行者 | `GII-05`/`GII-09` 对 `src/internal/config.rs` 的写入已合入 `main` | 两卡 `Implementation write set`（2026-11-18 核对） | 超时 7 日；未合入前 `WC-02` 置 `blocked`，严禁并发写入 `config.rs`（G-10） |
| `DEP-WC-04` | `incoming` | 跨计划 | `plan-20261117-checkout-switch-restore.md#CSR-01, CSR-05, CSR-06, CSR-09, CSR-11, CSR-12, CSR-13, CSR-14, CSR-20, CSR-23, CSR-24, CSR-29, CSR-30, CSR-33` | Checkout/Switch/Restore 计划执行者 | 上述全部 `restore.rs` / `restore_test.rs` / `restore` 文档卡写入已完成合并 | `plan-20261117-checkout-switch-restore.md#DEP-CSR-05`（对等登记） | 超时 7 日上报协调者；未完成前 `WC-10` 置 `blocked`，禁止并发写入（G-10） |
| `DEP-WC-05` | `incoming` | 跨计划写集串行 | 所有同日 `plan-20261117-*.md` 计划 | Coordinator 全局串行调度 | 本计划共享 `COMPATIBILITY.md` 的卡（`WC-01/02/03/04/04a/04b/05/05a/06/06a/07/08/09/09a/09b/10/11/11a/11b/12/13`）开工与评审前由 Coordinator 分配唯一窗口：跨计划及家族外的前序 `COMPATIBILITY.md` 变更须已合入 `main`；同一发布家族内的前序子卡（`REL-WC-NEST` 中 `WC-04` 之于 `WC-04b`；`REL-WC-RESET` 中 `WC-11` 之于 `WC-11a`、`WC-11a` 之于 `WC-11b`）按「依赖语义」以已审、本地签名提交（`locally-accepted`）放行，不要求先合入 `main`；家族的 `COMPATIBILITY.md` 窗口自首子卡开工持有，直到发布点（`WC-04R`、`WC-11R`）合入 `main` 并取得 D 组结果才释放，期间其它计划不得写入该文件（只在既有条目行内追加、不改变绝对行号） | `plan-20261117-commit-signing.md#DEP-CS-07` 等各计划对等登记 | 超时 7 日；跨计划或家族外前序未合入前对应卡置 `blocked`，严禁跨计划并发实现与评审 `COMPATIBILITY.md`（G-10） |
| `DEP-WC-06` | `incoming` | 跨计划（start-to-completion） | `plan-20261117-clone.md#CL-13, CL-19, CL-20`、`plan-20261117-plumbing-misc.md#PM-03`、`plan-20261117-merge-pull.md#MP-14..20, MP-24..25` | Clone / Plumbing-Misc / Merge-Pull 计划执行者 | `CL-13`（`--filter=blob:none`）、`CL-19`（promisor 按需拉取）、`CL-20`（目录重命名 merge 按需拉取）、`PM-03`（`rev-list --missing=print`）与 merge 重命名链合入 `main` 后，解除 `WC-06a` 阻塞以执行 `t6421#1`（`TC-0733`）只读核验（`plan-20261117-commit-signing.md#CS-24` 只交付 `GIT_TRACE2_EVENT`，若核验需 `GIT_TRACE2_PERF` 则按 `DEFER-WC-02` 延后） | 各卡任务卡标题与写集（2026-11-18 核对） | 超时 7 日；未合入前 `WC-06a` 置 `blocked`（G-10） |
| `DEP-WC-07` | `incoming` | 共享 `src/cli.rs` 串行窗口 | 2026-11-18 快照中 `Implementation write set` 含 `src/cli.rs` 的具名卡：`plan-20261117-branch.md#BR-03`；`plan-20261117-checkout-switch-restore.md#CSR-01, CSR-02, CSR-23, CSR-27`；`plan-20261117-clone.md#CL-01, CL-20, CL-26, CL-27, CL-28`；`plan-20261117-commit-signing.md#CS-09, CS-15b, CS-24, CS-26`；`plan-20261117-fetch-push.md#FP-18`；`plan-20261117-global-init-infra.md#GII-01, GII-02, GII-03, GII-04, GII-05, GII-15`；`plan-20261117-plumbing-misc.md#PM-10, PM-19`（其它计划仍在并行修订，`WC-11s` Go 修订时重新核对） | 仓库发布协调者 | 上述卡对 `src/cli.rs` 的写入已合入 `main`，协调者在家族首卡 `WC-11` 开工前分配独占窗口（取得时 `libra fetch origin` 并记录 `W0=$(libra rev-parse refs/remotes/origin/main)` 与 UTC 时间），保持到 `WC-11R` 发布；`WC-11s` 可据 `ADR-WC-02` 改为「中央分发前置守卫 + 枚举全部 `Commands` 变体的穷举测试」以取代等待 | 各卡 `Implementation write set`（2026-11-18 以脚本逐卡扫描） | 未获独占窗口不得开工 `WC-11`；守卫被后续写入绕过即 STOP，不得发布 `WC-11b` 或 `WC-11R` |
| `DEP-WC-08` | `incoming` | 共享后端 `cf` 推送冻结窗口 | `/Volumes/Data/gitmono/libra-backend` 的 `cf` 分支（所有同日计划及其它向该分支推送的交付者） | 仓库发布协调者 | 两个家族窗口内（`REL-WC-NEST`：自 `WC-04` 开工至 `WC-04R` 推送并完成部署核验；`REL-WC-RESET`：自 `WC-11` 开工至 `WC-11R` 推送并完成部署核验），协调者冻结所有其它交付者对该后端检出的提交与 `cf` 推送（推送会携带家族未发布的本地网站提交，他人本地提交也会被家族推送一并公开）；窗口内其它卡的网站交付排到窗口之后 | 后端 `AGENTS.md`（`cf` 直推交付）；「文档与网站验收命令」第 5 步 | 窗口超过 7 日由协调者裁决：家族以 `libra revert` 抵消其后端本地提交后解除冻结（最终不公开未集成内容），或继续等待；任何未经裁决的提交或 `cf` 推送即 STOP 并上报 |

### 发布分组与并发窗口

默认每张卡独立发布切片（G-07、`Release boundary=independent`）；本计划有两个家族：`REL-WC-NEST` 与 `REL-WC-RESET`。

| ID | 成员 | 唯一发布点 | 窗口规则 | 失败回滚顺序 | 理由 |
|---|---|---|---|---|---|
| `REL-WC-NEST` | `WC-04`（1/2）、`WC-04b`（2/2） | `WC-04R` | 窗口自 `WC-04` 开工起，至 `WC-04R` 推送并取得 D 组结果止；子卡只做本地签名提交（Libra 仓与后端 `cf` 各自本地提交）与各自 A/B 门，不推送、不 bump；窗口内禁止插入其它卡的发布切片；`DEP-WC-05` 兼容矩阵窗口与 `DEP-WC-08` 后端推送冻结窗口覆盖整个家族窗口 | 推送前按 `WC-04b` → `WC-04` 逆序以 `libra revert` 撤回本地提交（Libra 仓与后端各自）；发布后由 `WC-04R` `patch + 1` 前滚 | 只保护部分删除路径的中间版本会让用户误以为嵌套仓库在单 force 下一律安全 |
| `REL-WC-RESET` | `WC-11`（1/3）、`WC-11a`（2/3）、`WC-11b`（3/3） | `WC-11R` | 窗口自 `WC-11` 开工起，至 `WC-11R` 推送并取得 D 组结果止；子卡只做本地签名提交（Libra 仓与后端 `cf` 各自本地提交）与各自 A/B 门，不推送、不 bump；窗口内禁止插入其它卡的发布切片（本家族位于实施链尾，不阻塞其它卡）；`DEP-WC-05` 兼容矩阵窗口、`DEP-WC-07` 独占窗口与 `DEP-WC-08` 后端推送冻结窗口覆盖整个家族窗口 | 推送前按 `WC-11b` → `WC-11a` → `WC-11` 逆序以 `libra revert` 撤回本地提交（Libra 仓与后端各自）；发布后由 `WC-11R` `patch + 1` 前滚，保留恢复清单与用户备份 | 扩大的 unmerged 删除范围不得在崩溃恢复、备份保护与恢复命令守卫缺失时对外发布 |

**并发声明:** 本计划不设实现并发窗口：除两个发布点（`WC-04R`、`WC-11R`）与 spike `WC-11s` 外，每张卡都写 `COMPATIBILITY.md`（`DEP-WC-05`），而这三张卡也位于同一链中，因此全部卡按「实施顺序」单链严格串行（G-10）；`REL-WC-NEST` 窗口内（`WC-04` 开工至 `WC-04R` 取得 D 组结果）尤其不得插入其它卡，避免他卡本地提交随家族发布一并推送；发布动作按 ER-12 一律串行。
**发布者:** Coordinator 指定的单一发布者（ER-12），负责 C 组 bump、构建、安装、提交、PR、`gh release create` 与 D 组跟踪，以及「文档与网站验收命令」第 4–6 步的网站提交、推送与部署核验。
**发布窗口顺序:** `REL-WC-NEST`（`WC-04` → `WC-04b` 本地提交 → `WC-04R` 唯一发布）→ `WC-01` → `WC-02` → `WC-03` → `WC-04a` → `WC-05` → `WC-05a` → `WC-06` → `WC-07` → `WC-08` → `WC-09` → `WC-09a` → `WC-09b` → `WC-10` → `WC-12` → `WC-06a`（no-release，只做网站交付）→ `WC-13` → `WC-11s`（no-release，计划修订）→ `REL-WC-RESET`（`WC-11` → `WC-11a` → `WC-11b` 本地提交 → `WC-11R` 唯一发布）；同一时刻只允许一张卡处于「已 bump 未完成推送 / 未触发 release」状态。

### Phase 0: `clean` 行为对齐（`WC-01`..`WC-05a`，含 `WC-04a`、`WC-04b`、`WC-04R`）

**目标:** 完成 `clean` 的单 force 嵌套仓库保护（`REL-WC-NEST`）、cwd 相对路径、`core.excludesfile`、未跟踪目录 `-d` 边界、`-f -f`、`clean.requireForce` 与 `-e` 目录前缀匹配。
**进入条件:** `M0`（计划文本提交门双 PASS）。
**退出条件:** `WC-04`、`WC-04b`、`WC-04R`、`WC-01`、`WC-02`、`WC-03`、`WC-04a`、`WC-05`、`WC-05a` 全部 `done` 且 `complete`。

### Phase 1: `mv` 与 `apply` 对齐（`WC-06`, `WC-07`..`WC-09b`）

**目标:** 修复 `mv` 目录重命名、核对补测 `apply --numstat` / `--stat --summary`、支持 `--binary` 端到端管道、无载荷 full-index 二进制补丁的直接应用与 `--3way` 三方回退。
**进入条件:** `WC-06` 需 `WC-05a` 完成；`WC-07` 需 `WC-06` 完成，`WC-08`、`WC-09`、`WC-09a`、`WC-09b` 依次需其前驱完成（单链串行，见「并发声明」）。
**退出条件:** `WC-06`、`WC-07`..`WC-09b` 全部 `done` 且 `complete`。

### Phase 2: `reset`、`restore`、`rm` 对齐、`TC-0733` 核验与 `reset` 家族（`WC-10`、`WC-12`、`WC-06a`、`WC-13`、`WC-11s`、`WC-11`、`WC-11a`、`WC-11b`、`WC-11R`）

**目标:** `reset`/`restore` 放行 `main`、`rm --pathspec-from-file=-` 与 `rm -n`；在 `DEP-WC-06` 就绪后由 `WC-06a` 只读核验 `TC-0733`；由 `WC-11s` 判定 `reset --hard` 恢复协议的锁覆盖，Go 后完成 `REL-WC-RESET` 家族。
**进入条件:** `WC-09b`、`DEP-WC-01`、`DEP-WC-04` 就绪（`WC-06a` 另需 `WC-06`、`WC-12`、`DEP-WC-06`；`WC-11` 另需 `WC-11s` 达到 `locally-accepted`（Go 修订双 PASS）且已取得 `DEP-WC-07` 窗口）。
**退出条件:** 按路径分述，与 `M3` 一致——Go 路径：`WC-10`、`WC-12`、`WC-06a`、`WC-13`、`WC-11s`、`WC-11`、`WC-11a`、`WC-11b`、`WC-11R` 全部 `done` 且 `complete`，随后全计划收口全量门通过；No-Go 路径：`WC-10`、`WC-12`、`WC-06a`、`WC-13` 全部 `done` 且 `complete`、`WC-11s` 为 `locally-accepted`，`WC-11`、`WC-11a`、`WC-11b`、`WC-11R` 按 `DEFER-WC-03` 正式延后；全计划收口全量门通过后按「完成判据」的收口记账与终止链协议取得 D，再把 `WC-11s` 置 `done`/`complete`。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（每张卡完成即发布版本与 artifact，恢复需发新版本前滚修复；兼容窗口为相邻 1 个 minor 版本内保持仓内对象与配置向后兼容，降级可通过上一已知良好 tag 重新安装）
- **Migration and rollback 默认:** `N/A：无 SQLite schema 迁移；兼容窗口内降级可通过上一已知良好 tag 重新安装`
- **Security and privacy 默认:** `继承 GC-07、GC-11`
- **Performance budget 默认:** `继承 GC-10`（`WC-03` 另有具体预算）
- **Release write set 默认:** `Inherited`
- **C 组默认（`C/D coverage from: self` 的独立卡与发布点 `WC-04R`、`WC-11R`）:** 按 ER-04 C 组 ①–⑨：版本面 parity 预检 → `patch + 1` 并同步 `compat_version_surface_sync` 报告的版本面 → `cargo build` 刷新 `Cargo.lock` → bump 后 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与按 `Full-suite trigger` 判定的测试面（命中时为 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`；未命中时为按本卡实际改动表面选出的 ER-04 A 组全部 focused 命令（以 nextest 执行，ER-14），并附加本卡 `Verification` 的全部具名测试与共享回归组；两者都在 bump 后的树上执行）→ `cargo build --release` → 安装 → 精确 `libra add` 与 `libra commit -s -m`（ER-07 校验）→ feature branch、`gh pr create`、同一 PR head SHA 的 `base.yml` 与 CodeQL 全绿、`gh pr merge --squash` → 在 merge SHA 上 `gh release create "v${VERSION}" --target "$MERGE_SHA" --generate-notes`（`WC-04R`、`WC-11R` 例外：改用经卡内 AC3 核对的 `--notes-file <notes 文件>`，不使用 `--generate-notes`）。
- **D 组默认（同上范围；`WC-06a` 的 D1–D3 继承 `WC-13`、家族子卡继承各自发布点 `WC-04R` / `WC-11R`）:** D1 `.github/workflows/base.yml`：`pull_request`（本卡 PR head SHA）与合并后 `push` 到 `main`，全部适用 job 成功（被 `cancel-superseded-main.yml` 取消的 run 不算通过）；D2 `.github/workflows/codeql.yml`：同一 PR head SHA 与合并后 `push` 到 `main` 成功；D3 `.github/workflows/release.yml`：`push` tag `v<version>`（由 `gh release create` 触发），以 `gh run list --workflow=release.yml --branch "v<version>"` 取得该 tag 的 run，`gh run view <run-id> --json conclusion,jobs` 显示结论为 `success`，且现场读取的 workflow 文件中全部 job（2026-11-18 为 `build-and-upload` 全部矩阵项、`upload-install-scripts`（含 CDN 安装脚本版本校验步骤）、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest`）均为 `success`，任一跳过或失败即不通过；不以 GitHub Release 附件作判据（该 workflow 把产物上传到 Cloudflare R2 而非 Release 附件）；D3 另需 CDN 可获取性与完整性证据：对同一 tag，以 `curl -fsSL --retry 3 -o <file> -w '%{http_code}' <url>` 完整 GET（HEAD 请求不计通过）`https://download.libra.tools/libra/releases/v<version>/` 下的 `libra-linux-amd64`、`libra-linux-arm64`、`libra-darwin-arm64`、`libra-windows-amd64.exe` 与无后缀 `libra-windows-amd64`，以及 `https://download.libra.tools/install.sh`、`install.ps1` 与 `https://download.libra.tools/libra/releases/stable/manifest-v1.json`；先以安装器（`install.sh`）所用的同一 pinned 公钥验证 manifest 签名、版本与有效期，再逐一核对各平台条目的 URL、`size` 与 `sha256` 和实际下载字节一致（Windows 条目以无后缀对象核对，`.exe` 对象另行与同一条目比较），`install.sh`/`install.ps1` 的默认版本等于 `v<version>`；任一 GET 非 200、签名无效或摘要不符即 D3 不通过、保持 `remote-pending`；`WC-04R` 另以 `gh release view "v<version>"` 核对正文含其 AC3 两类路径且无 `-f -f` 支持表述，`WC-11R` 另以同一命令核对正文含其 AC3 三项；D4 网站部署核验（「文档与网站验收命令」第 6 步；`WC-06a` 的 `mv` 页面 D4 由其自行取得）。D 未全绿前停在 `remote-pending`，失败一律前滚。
- **计数口径:** AC 按独立 pass/fail 谓词计数，ER-06/ER-06a 同卡文档门不计；VER 按 `Verification` 列表逐条计数（共享回归组与守卫也计入，保守口径）；各卡 `Granularity` 写明逐条 AC 计数。

**默认覆盖：**

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| `WC-06a` | `Task type`、`Rollback mode`、`Release boundary`、`Release write set`、`Version increment`、`C/D coverage from`、`Full-suite trigger` | `spike` / `revert` / `no-release` / `N/A` / `N/A` / `WC-13` / `N/A`：只读核验，不改产品代码；网站 `mv` 页面由本卡按「文档与网站验收命令」第 2–4 步与第 6 步交付（D4 自行取得） |
| `WC-11s` | `Task type`、`Rollback mode`、`Release boundary`、`Release write set`、`Version increment`、`C/D coverage from`、`Full-suite trigger`、`Docs and compatibility impact` | `spike` / `revert` / `no-release` / `N/A` / `N/A` / Go：`WC-11R`，No-Go：`M3`（最终代码树收口门 + 收口记账提交的 `base.yml`/`codeql.yml`，见「完成判据」终止链协议）/ `N/A` / `N/A`：只修订计划文档，无用户可见行为 |
| `WC-04`、`WC-04b` | `Rollback mode`、`Release boundary`、`Release write set`、`Version increment`、`C/D coverage from`、`Full-suite trigger`、网站交付 | `revert`（家族窗口内撤回本地提交，含后端本地提交）/ `family child of REL-WC-NEST` / `N/A` / `N/A` / `WC-04R` / `N/A`；后端页面在本卡写入、评审并本地签名提交但不推送（「文档与网站验收命令」第 5 步、`DEP-WC-08`），由 `WC-04R` 推送与部署 |
| `WC-04R` | `Task type`、`Release boundary`、`Estimated scope`、`Full-suite trigger`、`Implementation write set`、C 组发布命令 | `release` / `family release point of REL-WC-NEST` / `S` / `T-2` / `N/A` / `gh release create --notes-file`（经 AC3 核对的 notes 文件） |
| `WC-11`、`WC-11a`、`WC-11b` | `Rollback mode`、`Release boundary`、`Release write set`、`Version increment`、`C/D coverage from`、`Full-suite trigger`、网站交付 | `revert`（家族窗口内撤回本地提交，含后端本地提交）/ `family child of REL-WC-RESET` / `N/A` / `N/A` / `WC-11R` / `N/A`；后端页面在本卡写入、评审并本地签名提交但不推送（「文档与网站验收命令」第 5 步、`DEP-WC-08`），由 `WC-11R` 推送与部署 |
| `WC-11R` | `Task type`、`Release boundary`、`Estimated scope`、`Full-suite trigger`、`Implementation write set`、C 组发布命令 | `release` / `family release point of REL-WC-RESET` / `S` / `T-2` / `N/A` / `gh release create --notes-file`（经 AC3 核对的 notes 文件） |
| `WC-11a`、`WC-11b`、`WC-11R` | `Lifecycle` | `blocked`，直到 `WC-11s` Go 修订取得 Claude（Subagent）与 Codex 双 PASS |

**规则 waiver（`EX-*`）：** 无。

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| WC-01 | implementation | `clean` cwd相对路径输出 | 前滚新版本+降级指引 | yes | yes | 6/8 | 2/8 | 1/1 | M | WC-04R, DEP-WC-05 | 序列化于 WC-04R | independent | N/A | N/A |
| WC-02 | implementation | `core.excludesfile`大小写读取 | 前滚新版本+降级指引 | yes | yes | 5/8 | 4/8 | 1/1 | M | WC-01, DEP-WC-03, DEP-WC-05 | 序列化于 WC-01 | independent | N/A | N/A |
| WC-03 | implementation | `clean -f`无`-d`目录边界 | 前滚新版本+降级指引 | yes | yes | 6/8 | 5/8 | 1/1 | M | WC-02, DEP-WC-05 | 序列化于 WC-02 | independent | N/A | N/A |
| WC-04 | implementation | `clean`单force嵌套库保护-非pathspec路径 | 家族窗口内revert | yes | yes | 7/8 | 2/8 | 1/1 | M | DEP-WC-05, DEP-WC-08 | clean 链首卡（WC-04..05a 串行） | family child | N/A | N/A |
| WC-04b | implementation | `clean`单force嵌套库保护-显式pathspec路径 | 家族窗口内revert | yes | yes | 5/8 | 2/8 | 1/1 | M | WC-04, DEP-WC-05 | 序列化于 WC-04 | family child | WC-04 | N/A |
| WC-04R | release | `REL-WC-NEST`嵌套库保护联合发布 | patch+1前滚修复 | yes | yes | 4/12 | 3/12 | 0/0 | S | WC-04b, DEP-WC-08 | 序列化于 WC-04b | family release point | N/A | N/A |
| WC-04a | implementation | `clean -f -f`移除嵌套库 | 前滚新版本+降级指引 | yes | yes | 5/8 | 2/8 | 1/1 | M | WC-03, DEP-WC-05 | 序列化于 WC-03 | independent | WC-04 | N/A |
| WC-05 | implementation | `clean.requireForce`配置 | 前滚新版本+降级指引 | yes | yes | 8/8 | 2/8 | 1/1 | M | WC-04a, DEP-WC-05 | 序列化于 WC-04a | independent | N/A | N/A |
| WC-05a | implementation | `clean -e`目录前缀匹配 | 前滚新版本+降级指引 | yes | yes | 3/8 | 2/8 | 1/1 | M | WC-05, DEP-WC-05 | 序列化于 WC-05 | independent | WC-05 | N/A |
| WC-06 | implementation | `mv`目录重命名到新路径 | 前滚新版本+降级指引 | yes | yes | 7/8 | 2/8 | 1/1 | M | WC-05a, DEP-WC-05 | 序列化于 WC-05a | independent | N/A | N/A |
| WC-06a | spike | `TC-0733` (`t6421#1`) partial-clone 只读核验与退出判定 | revert 本卡测试/文档/登记提交，产品代码零改动 | yes | yes | 3/8 | 6/8 | 0/0 | M | WC-06, WC-12, DEP-WC-05, DEP-WC-06 | 序列化于 WC-12 | no-release | WC-06 | N/A |
| WC-07 | implementation | `apply --numstat` `t4012#5`验证 | 前滚新版本+降级指引 | yes | yes | 5/8 | 2/8 | 1/1 | M | WC-06, DEP-WC-05 | 序列化于 WC-06 | independent | N/A | N/A |
| WC-08 | implementation | `apply --stat --summary`二进制验证 | 前滚新版本+降级指引 | yes | yes | 4/8 | 2/8 | 1/1 | M | WC-07, DEP-WC-05 | 序列化于 WC-07 | independent | N/A | N/A |
| WC-09 | implementation | `apply --binary`选项与删后重建 | 前滚新版本+降级指引 | yes | yes | 8/8 | 2/8 | 1/1 | M | WC-08, DEP-WC-05 | 序列化于 WC-08 | independent | N/A | N/A |
| WC-09a | implementation | `apply`无载荷full-index二进制直接应用 | 前滚新版本+降级指引 | yes | yes | 8/8 | 3/8 | 1/1 | M | WC-09, DEP-WC-05 | 序列化于 WC-09 | independent | N/A | N/A |
| WC-09b | implementation | `apply --3way`二进制三方回退 | 前滚新版本+降级指引 | yes | yes | 7/8 | 4/8 | 1/1 | M | WC-09a, DEP-WC-05 | 序列化于 WC-09a | independent | WC-09a | N/A |
| WC-10 | implementation | `is_locked_revision`放行`main` | 前滚新版本+降级指引 | yes | yes | 8/8 | 8/8 | 3/3 | M | WC-09b, DEP-WC-01, DEP-WC-04, DEP-WC-05 | 序列化于 WC-09b | independent | N/A | N/A |
| WC-11 | implementation | `reset --hard`清理unmerged文件 | 家族窗口内revert | yes | yes | 6/8 | 3/8 | 1/1 | M | WC-11s, DEP-WC-05, DEP-WC-07, DEP-WC-08 | 序列化于 WC-11s | family child | N/A | N/A |
| WC-11a | implementation | `reset --hard`并发保护与崩溃恢复 | 家族窗口内revert | yes | yes | 8/8 | 4/8 | 3/3 | M | WC-11, WC-11s, DEP-WC-05 | 序列化于 WC-11 | family child | WC-11 | N/A |
| WC-11b | implementation | `reset --recover`入口与命令守卫 | 家族窗口内revert | yes | yes | 8/8 | 5/8 | 2/2 | M | WC-11a, WC-11s, DEP-WC-05, DEP-WC-07 | 序列化于 WC-11a | family child | WC-11a | N/A |
| WC-11R | release | `REL-WC-RESET` unmerged清理与恢复联合发布 | patch+1前滚修复 | yes | yes | 4/12 | 10/12 | 0/0 | S | WC-11b, DEP-WC-07, DEP-WC-08 | 序列化于 WC-11b | family release point | N/A | N/A |
| WC-11s | spike | `reset`恢复协议锁覆盖调查与Go/No-Go | revert 计划修订；No-Go 时按 DEFER-WC-03 延后家族 | yes | yes | 3/8 | 3/8 | 0/0 | M | WC-13 | 序列化于 WC-13 | no-release | WC-11a | N/A |
| WC-12 | implementation | `rm --pathspec-from-file=-`与校验 | 前滚新版本+降级指引 | yes | yes | 8/8 | 2/8 | 1/1 | M | WC-10, DEP-WC-05 | 序列化于 WC-10 | independent | N/A | N/A |
| WC-13 | implementation | `rm -n`短标志与SIGPIPE/lock | 前滚新版本+降级指引 | yes | yes | 8/8 | 4/8 | 1/1 | M | WC-12, WC-06a, DEP-WC-05 | 序列化于 WC-06a | independent | N/A | N/A |

---

### Task WC-01: 修复 `libra clean` 打印 cwd 相对路径（#732）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：clean 预览/删除按 cwd 相对路径，JSON 保持仓库根相对；无 pathspec 限定 cwd 子树；新增 `clean_test::clean_relative_pathspec_prefix`；clean 36/36。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）
**Description:** 使 `libra clean -n`（`Would remove ...`）与实际清理（`Removing ...`）在子目录运行：传入相对 pathspec 如 `../src` 时保留 `../` 前缀输出 cwd 相对路径，无 pathspec 时限定当前目录及子目录（不带 `../`）输出，而 `--json` 的 `removed` 数组保持确定性的仓库根相对路径契约。
**Out of scope:** `core.excludesfile`（`WC-02`）与 `-d` 行为（`WC-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `run_clean` 直接使用 `path.display().to_string()`（仓库根相对路径）构造输出列表 | `src/command/clean.rs:297-305` |

**Acceptance criteria:**
- [x] AC1：在子目录 `docs/` 下运行 `libra clean -n ../src`，输出 `Would remove ../src/part3.c`（保留 `../` 前缀；即上游 `t7300#7` / `TC-0533`）（`clean_test::clean_relative_pathspec_prefix`）。
- [x] AC2：在含已跟踪文件的子目录 `src/` 下运行无 pathspec 的 `libra clean -n`，只列出 `src/` 内的未跟踪文件，且路径相对 cwd 输出、不带 `../` 前缀（`clean_test::clean_relative_pathspec_prefix`）。
- [x] AC3：在 `src/` 下运行 `libra clean -f`，`Removing ...` 行同样按 cwd 相对路径打印（`clean_test::clean_relative_pathspec_prefix`）。
- [x] AC4：`--json` 模式下 `removed` 数组保持仓库根相对路径（机器契约不随 cwd 变化）（`clean_test::clean_relative_pathspec_prefix`）。
- [x] AC5：在 `docs/` 下运行 `libra clean -f ../src`，`Removing` 行同样保留 `../` 前缀（`Removing ../src/part3.c`）（`clean_test::clean_relative_pathspec_prefix`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；fixture 复刻上游 `git@750f5de7` `t/t7300-clean.sh:12-23` setup：`.gitignore` 内容恰为 `build\n*.o\n`，与空文件 `Makefile`、`src/part1.c` 一起提交，再暂存（不提交）空文件 `README`、`src/part2.c`；用例内再创建未跟踪空文件 `a.out`、`src/part3.c`、`docs/manual.txt`、`obj.o`、`build/lib.so`（同上游 `touch`）；除该 `.gitignore` 外无其它忽略来源。
- AC1 → `command::clean_test::clean_relative_pathspec_prefix`：cwd=`docs/`，执行 `libra clean -n ../src`；期望 exit 0、stdout 恰有一行 `Would remove ../src/part3.c`、stderr 为空；执行前后全量快照相同。
- AC2 → 同一测试：cwd=`src/`，执行 `libra clean -n`；期望 exit 0、stdout 恰为一行 `Would remove part3.c`，不含 `../`、`a.out`、`manual.txt`；执行前后全量快照相同。
- AC3 → 同一测试：cwd=`src/`，执行 `libra clean -f`；期望 exit 0、stdout 恰为一行 `Removing part3.c`；执行后全量快照恰好少了 `src/part3.c`。
- AC4 → 同一测试：重建 `src/part3.c`，cwd=`src/`，执行 `libra clean -n --json`；期望 exit 0、JSON 的 `removed` 数组等于 `["src/part3.c"]` 且 `dry_run` 为 `true`；执行前后全量快照相同。
- AC5 → 同一测试：重建 `src/part3.c`，cwd=`docs/`，执行 `libra clean -f ../src`；期望 exit 0、stdout 恰为一行 `Removing ../src/part3.c`；执行后全量快照恰好少了 `src/part3.c`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`source .env.test && cargo nextest run --features storage-sqlite --test command_test -E 'test(=command::clean_test::clean_relative_pathspec_prefix)'`（通过）
- [x] 共享回归组：`source .env.test && cargo nextest run --features storage-sqlite --test command_test -E 'test(/^command::clean_test::/)'`（36/36）

**Full-suite trigger:** `none`
**Dependencies:** `WC-04R`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`COMPATIBILITY.md` 与后端 `clean.en.md`（写入后端前确认 VCS 与 `cf` 分支）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clean-cwd相对路径输出; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8（AC1 1、AC2 2、AC3 1、AC4 1、AC5 1）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-04R,DEP-WC-05; writeset=序列化于 WC-04R; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-02: 修复 `core.excludesfile` 大小写读取使 `libra clean` 遵循全局排除文件（#733）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：clean 通过大小写不敏感 `core.excludesfile` 读取全局排除；新增 clean 与 internal config 测试；clean 36/36；C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）
**Description:** 修复 `config.rs` 中 `core.excludesfile` 的大小写敏感读取问题：在 `read_cascaded_fresh_conn_at`（`src/internal/config.rs:2616+`）复用既有的 `get_var_case_insensitive_with_conn`（`config.rs:536`），不全局改动共享 `ConfigKv::get_with_conn`，确保 `clean` 正确忽略 `core.excludesfile` 匹配的文件（同步补齐同函数服务的 `core.attributesFile` / `core.attributesfile` 回归检查）。
**Out of scope:** `-x` / `-X` 忽略策略之外的新增选项；全局改变 `ConfigKv::get_with_conn` 语义；其它文档与 `COMPATIBILITY.md` 中以规范键名 `core.excludesFile` 描述忽略来源的既有表述（如 `docs/commands/check-ignore.md`、`docs/commands/check-attr.md`、`docs/commands/lfs.md` 与 `ls-files`/`grep`/`check-ignore`/`lfs`/`diff` 条目，修复后仍准确，不改）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `check_gitignore` 已接入 `core.excludesFile` 与 `expand_config_path` | `src/utils/util.rs:61,3355` |
| `read_cascaded_fresh_conn_at` 未复用 `get_var_case_insensitive_with_conn`（`:536`），导致小写 `core.excludesfile` 不命中 | `src/internal/config.rs:166-177,536,2616` |
| 2026-11-18 以已安装 libra 0.30.207 在仓库外隔离 HOME 下复现：未配置时 `libra clean -n excludes included` 输出 `Would remove excludes` 与 `Would remove included`；`libra config set core.excludesfile excludes`（小写）后输出不变；改为 `libra config set core.excludesFile excludes`（驼峰）后只输出 `Would remove included` | 本计划评审期复现（fixture 同「AC → 断言映射」；仅作缺陷证据，不入库） |

**Acceptance criteria:**
- [x] AC1：本地配置小写 `core.excludesfile=excludes`（`excludes` 文件内容为匹配其自身的规则 `excludes`）时，`libra clean -n excludes included` 输出 `Would remove included`，且 stdout/stderr 均不含 `excludes`（即上游 `t7300#28` / `TC-0539`）。（`clean_respects_core_excludesfile` / `cascaded_fresh_read_matches_variable_case_insensitively`）
- [x] AC2：驼峰 `core.excludesFile=excludes` 时结果与 AC1 相同。（`clean_respects_core_excludesfile` / `cascaded_fresh_read_matches_variable_case_insensitively`）
- [x] AC3：同一级联读取对 `core.attributesfile`（小写）与 `core.attributesFile`（驼峰）均返回所配置的路径（同函数服务的回归）。（`clean_respects_core_excludesfile` / `cascaded_fresh_read_matches_variable_case_insensitively`）
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行（全局/系统配置与 XDG 来源为空，`core.excludesFile` 只来自下述本地配置）；新仓库（`libra init`）不含 `.gitignore`、`.libraignore`，`.libra/info/exclude` 只含 `libra init` 写入的注释模板；写入未跟踪文件 `excludes`（内容恰为 `excludes\n`，即匹配其自身的忽略规则，同上游 `git@750f5de7` `t/t7300-clean.sh:418` 的 `echo excludes >excludes`）与 `included`（内容恰为 `included\n`）。对照步骤（fixture 有效性前提，不计 AC）：尚未配置任何 `core.excludesFile` 时执行 `libra clean -n excludes included`，期望 exit 0 且 stdout 恰为 `Would remove excludes`、`Would remove included` 两行（证明 `excludes` 只因配置而被忽略）。
- AC1 → `command::clean_test::clean_respects_core_excludesfile`：`libra config set core.excludesfile excludes` 后执行 `libra clean -n excludes included`；期望 exit 0、stdout 恰为一行 `Would remove included`、stdout 与 stderr 均不含 `excludes`；执行前后全量快照相同。
- AC2 → 同一测试：`libra config unset core.excludesfile` 后 `libra config set core.excludesFile excludes`，重复 AC1 命令；期望同 AC1。
- AC3 → `internal::config::tests::cascaded_fresh_read_matches_variable_case_insensitively`：对 `read_cascaded_fresh_conn_at` 使用临时本地库，依次只写入 `core.excludesfile`、`core.excludesFile`、`core.attributesfile`、`core.attributesFile` 之一，以规范键 `core.excludesFile` / `core.attributesFile` 读取均返回所写值；未写入时返回 `None`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`clean_respects_core_excludesfile`（通过）
- [x] 具名测试：`cascaded_fresh_read_matches_variable_case_insensitively`（通过）
- [x] 共享回归组：`command::clean_test`（36/36）
- [x] 共享回归组：`internal::config::tests`（通过）

**Full-suite trigger:** `T-1: 改动共享 config 读取`
**Dependencies:** `WC-01`, `DEP-WC-03`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/internal/config.rs`, `tests/command/clean_test.rs`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/internal/config.rs`, `tests/command/clean_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`COMPATIBILITY.md`（在 `clean` 条目行内追加：`core.excludesFile` 键名按 Git 规则大小写不敏感读取，小写 `core.excludesfile` 同样生效；在 `check-attr` 条目行内追加：`core.attributesFile` 同理；均只在既有条目行内追加，`DEP-WC-05`）与后端 `clean.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=core.excludesfile大小写读取; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/8（AC1 2、AC2 1、AC3 2）; VER=4/8; landing=1; prod-files=1; scope=M; deps=WC-01,DEP-WC-03,DEP-WC-05; writeset=序列化于 WC-01; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-03: 修复 `libra clean -f` 未指定 `-d` 时的未跟踪目录边界（#734）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：无 `-d`/无显式 pathspec 时不进入未跟踪目录；显式 pathspec 解除限制；清理旧测试适配；clean 39/39。C 组 patch +1。偏差：GC-10 的 read_dir 计数接缝测试未新增，仅保持单次扫描实现；用户已取消独立 Claude/Codex 审计与全量测试。）
**Description:** 对齐 Git `clean` 语义：当未传 `-d` 且无显式 pathspec 时，不递归进入纯未跟踪目录（`docs/manual.txt` 保留）；当传入显式 pathspec 时，与 Git `builtin/clean.c:973-978` 一致忽略 `-d` 限制（`clean -f docs/manual.txt` 删除 `docs/manual.txt`，而 `clean -n docs/` 对未跟踪目录 `docs/` 作为单条输出 `Would remove docs/`）；新引入的显式 pathspec 目录递归必须保持 `WC-04` 已落地的嵌套仓库排除（共享回归组中的 `command::clean_test::clean_single_force_pathspec_keeps_nested_repos`，尤其其父目录断言 AC5，防止目录候选被整体删除）。
**Out of scope:** 单 force 嵌套仓库保护（`WC-04`、`WC-04b`，已先行）；双 `-f`（`WC-04a`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `list_workdir_files` 递归枚举所有未跟踪目录内文件并直接删除，未区分无 pathspec 与显式 pathspec | `src/command/clean.rs:218-222,251-267` |

**Acceptance criteria:**
- [x] AC1：上游 `t7300#26`（`TC-0540`）等价场景通过：无 pathspec 运行 `libra clean -f`（无 `-d`）后，上游该用例的全部 `test_path_is_*` 断言在 Libra 中成立（纯未跟踪目录内的 `docs/manual.txt` 保留）。（`clean_does_not_descend_untracked_dirs_without_d` / 既有 layer service 测试）
- [x] AC2：显式文件 pathspec `libra clean -f docs/manual.txt`（无 `-d`）删除该文件。（`clean_does_not_descend_untracked_dirs_without_d` / 既有 layer service 测试）
- [x] AC3：显式目录 pathspec `libra clean -n docs/`（无 `-d`）把未跟踪目录作为单条 `Would remove docs/` 输出（上游 `builtin/clean.c:973-978`：显式 pathspec 隐含目录递归）。（`clean_does_not_descend_untracked_dirs_without_d` / 既有 layer service 测试）
- [x] AC4：`libra clean -f docs/`（无 `-d`）移除整个 `docs/` 目录。（`clean_does_not_descend_untracked_dirs_without_d` / 既有 layer service 测试）
- [x] AC5：`libra clean -f -d` 移除未跟踪目录 `docs/`。（`clean_does_not_descend_untracked_dirs_without_d` / 既有 layer service 测试）
- [x] AC6：`libra clean -f -d` 不删除由 `libra layer apply` materialize 到未跟踪目录内的图层文件。（`clean_does_not_descend_untracked_dirs_without_d` / 既有 layer service 测试）
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；fixture 复刻上游 `git@750f5de7` `t/t7300-clean.sh:12-23` setup：`.gitignore` 内容恰为 `build\n*.o\n`，与空文件 `Makefile`、`src/part1.c` 一起提交，再暂存（不提交）空文件 `README`、`src/part2.c`；用例内再创建未跟踪空文件 `a.out`、`src/part3.c`、`docs/manual.txt`、`obj.o`、`build/lib.so`（同上游 `touch`）；除该 `.gitignore` 外无其它忽略来源。
- AC1 → `command::clean_test::clean_does_not_descend_untracked_dirs_without_d`：执行 `libra clean -f`；期望 exit 0，且与上游 `t7300#26` 逐项一致：`README`、`src/part1.c`、`src/part2.c`、`docs/manual.txt`、`obj.o`、`build/lib.so` 存在，`a.out`、`src/part3.c` 不存在；全量快照恰好少了 `a.out` 与 `src/part3.c`。
- AC2 → 同一测试：重建未跟踪文件后执行 `libra clean -f docs/manual.txt`；期望 exit 0、stdout 含 `Removing docs/manual.txt`；之后全量快照恰好少了 `docs/manual.txt`。
- AC3 → 同一测试：重建后执行 `libra clean -n docs/`；期望 exit 0、stdout 恰为一行 `Would remove docs/`；执行前后全量快照相同。
- AC4 → 同一测试：执行 `libra clean -f docs/`；期望 exit 0；之后全量快照恰好少了 `docs/`（含 `docs/manual.txt`）。
- AC5 → 同一测试：重建后执行 `libra clean -f -d`；期望 exit 0；之后全量快照恰好少了 `a.out`、`src/part3.c` 与 `docs/`（被忽略的 `obj.o`、`build/` 保留）。
- AC6 → `command::clean_test::clean_d_keeps_layer_overlay_files`：在仓库外建 overlay 源目录（含 `docs/overlay.txt`，内容 `overlay\n`），`libra layer add scratch --source <源目录>` 与 `libra layer apply` 后，在 `docs/` 再放普通未跟踪文件 `docs/plain.txt`（内容 `plain\n`），执行 `libra clean -f -d`；期望 exit 0；之后 `docs/overlay.txt` 存在且字节仍为 `overlay\n`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`clean_does_not_descend_untracked_dirs_without_d`（通过）
- [x] 覆盖 AC6：既有 `service_test::clean_force_preserves_nested_layer_overlay` / `clean_force_preserves_materialized_layer_overlay`（`clean -fdx`）
- [ ] 性能接缝测试未单独新增（偏差；`clean` 仍单次扫描）
- [x] 共享回归组：`command::clean_test`（39/39）
- [x] 共享回归组：`command::clean::tests`（通过）

**Full-suite trigger:** `none`
**Dependencies:** `WC-02`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`COMPATIBILITY.md` 与后端 `clean.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clean-f无-d目录边界; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8（AC1–AC6 各 1）; VER=5/8; landing=1; prod-files=1; scope=M; deps=WC-02,DEP-WC-05; writeset=序列化于 WC-02; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-04: 单 force 下全部删除路径的嵌套仓库保护（#735 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：新增 `nested_repository_roots`（直接含 `.git`/`.libra` 的子目录即嵌套仓库根，遇到停止下探）与 `run_clean` 的剔除逻辑——在 pathspec 过滤后、预览/删除前排除嵌套根及其内容，并排除包含嵌套根的祖先目录（`REMOVE_DIR_KEEP_NESTED_GIT`，单 force，`-x`/`-X` 同样生效）。AC1–AC7 由 `command::clean_test::clean_single_force_keeps_nested_repos` 覆盖（嵌套 Git 仓库 `foo/`、`baz/boo/`、嵌套 Libra 仓库 `lib/` 的路径+SHA-256 快照、`-n -d` 不列出、`-x`/`-X` 保护）；`clean_test` 34/34；clean 的 EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行本计划的独立 Claude PASS 文档门；后端站点按家族规则仅本地签名提交、未推送（由发布点 `WC-04R` 统一推送）；计划中的 `/Volumes/Data` 路径在本机以 `/mnt/data/gitmono/libra-backend` 执行。
**Description:** 在 `clean` 候选收集阶段识别嵌套仓库根（直接含 `.git` 或 `.libra` 条目的子目录；`src/utils/util.rs:1785-1800` 目前只跳过元数据目录本身，其余文件仍成为候选），在单 force、`-n` 预览以及 `clean.requireForce=false` 的无标志模式下排除嵌套仓库根及其下全部路径，本卡覆盖无 pathspec 的删除路径（有/无 `-d`，含 `-x`/`-X` 忽略策略）与 `-n -d` 预览，显式 pathspec 路径拆出 `WC-04b`，对齐上游默认 `REMOVE_DIR_KEEP_NESTED_GIT`（`git@750f5de7` `builtin/clean.c:927`）。本卡与 `WC-04b` 组成不可拆分的发布家族 `REL-WC-NEST`，由发布点 `WC-04R` 一次发布（两类路径的保护缺一不得对外发布），并先于 `WC-01`、`WC-02`、`WC-03`、`WC-04a` 等其它 clean 卡，使任何对外版本都不会以单 force 删除嵌套仓库内容。拆出 `WC-04a`（双 force）与 `WC-04b`（显式 pathspec 路径的同一保护）。
**Out of scope:** 显式 pathspec（指向嵌套仓库、其内文件或其父目录）路径的保护（拆出 `WC-04b` 承接，同属 `REL-WC-NEST` 家族，由 `WC-04R` 与本卡一起发布）；`-f -f` 的接受与双 force 删除（拆出 `WC-04a` 承接）；无 `-d` 的目录边界与显式 pathspec 目录递归（`WC-03`）；`clean.requireForce` 配置读取（`WC-05`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `list_files` 只跳过名为 `.git`/`.libra` 的元数据目录，嵌套仓库内的普通文件仍被列出 | `src/utils/util.rs:1785-1800` |
| 候选收集、pathspec 过滤与删除不识别嵌套仓库，目录删除使用 `remove_dir_all` | `src/command/clean.rs:218-249,281-287,310-328` |
| 目录扫描跳过 `.git`/`.libra` 条目但不把其父目录视为受保护 | `src/command/clean.rs:372-376` |

**Acceptance criteria:**
- [ ] AC1：上游 `t7300#30`（`nested git work tree`）等价场景通过：单 force `libra clean -f -d` 后上游该用例的全部 `test_path_is_*` 断言成立。
- [ ] AC2：单 force `libra clean -f -d` 后嵌套仓库 `foo/`、`baz/boo/`、`lib/` 内全部文件保留且内容不变。
- [ ] AC3：`libra clean -f`（无 `-d`、无 pathspec）后嵌套 Git 仓库 `foo/` 内全部文件保留且内容不变。
- [ ] AC4：`libra clean -f`（无 `-d`、无 pathspec）后嵌套 Libra 仓库 `lib/` 内全部文件保留且内容不变。
- [ ] AC5：预览 `libra clean -n -d` 不把任何嵌套仓库目录或其内文件列为 `Would remove`。
- [ ] AC6：包含被忽略文件的 `-x` 模式 `libra clean -f -d -x` 后嵌套仓库 `foo/`、`baz/boo/`、`lib/` 内全部文件保留且内容不变。
- [ ] AC7：只清理被忽略文件的 `-X` 模式 `libra clean -f -d -X`（父仓库 `.gitignore` 含 `*.world`）后嵌套仓库 `foo/`、`baz/boo/`、`lib/` 内全部文件（含多个匹配 `*.world` 的文件）保留且内容不变。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；在新 `libra init` 的父仓库中按上游 `git@750f5de7` `t/t7300-clean.sh:437-461`（`t7300#30`）构造两个嵌套 Git 仓库 `foo/`、`baz/boo/`（不依赖外部 `git` 二进制：各自手工建最小 gitdir，即内容为 `ref: refs/heads/main\n` 的 `.git/HEAD`、空目录 `.git/objects/` 与 `.git/refs/`、空文件 `.git/index`；另放 `foo/hello.world`（内容 `hello\n`）与 `baz/boo/deeper.world`（内容 `deeper\n`））、普通未跟踪目录 `bar/`（内含空文件 `goodbye.people`，同上游 `>goodbye.people`），以及在父仓库内执行 `libra init lib` 创建的嵌套 Libra 仓库 `lib/`（另放 `lib/notes.txt`，内容 `notes\n`）；以上均不加入父仓库索引，除 AC7 写明的 `.gitignore` 外无其它忽略来源；每条 AC 使用新 fixture，并在执行前记录嵌套仓库内全部文件的路径集合与 SHA-256。
- AC1 → `command::clean_test::clean_single_force_keeps_nested_repos`：执行 `libra clean -f -d`；期望 exit 0，且与上游 `t7300#30` 逐项一致：`foo/.git/index`、`foo/hello.world`、`baz/boo/.git/index`、`baz/boo/deeper.world` 存在，`bar` 不存在。
- AC2 → 同一测试同一次执行：`foo/`、`baz/boo/`（各含 `.git/` 下全部文件）与 `lib/`（含 `lib/.libra` 下文件与 `lib/notes.txt`）内全部文件各自的路径集合与 SHA-256 与执行前一致。
- AC3 → 同一测试：执行 `libra clean -f`；期望 exit 0；之后 `foo/` 内全部文件的路径集合与 SHA-256 与执行前一致。
- AC4 → 同一测试同一次执行：`lib/` 内全部文件的路径集合与 SHA-256 与执行前一致。
- AC5 → 同一测试：执行 `libra clean -n -d`；期望 exit 0、stdout 中不存在以 `Would remove foo`、`Would remove baz`、`Would remove lib` 开头的行；执行前后全量快照相同。
- AC6 → 同一测试：执行 `libra clean -f -d -x`；期望 exit 0；之后 `foo/`、`baz/boo/`、`lib/` 内全部文件各自的路径集合与 SHA-256 与执行前一致。
- AC7 → 同一测试：本条 fixture 另在嵌套仓库内放入 `foo/sub/extra.world` 与 `lib/extra.world`（内容均为 `extra\n`），使 `foo/hello.world`、`foo/sub/extra.world`、`baz/boo/deeper.world`、`lib/extra.world` 四个文件均匹配 `*.world`；在父仓库根写入 `.gitignore`（内容恰为 `*.world\n`，不加入索引）后执行 `libra clean -f -d -X`；期望 exit 0；之后 `foo/`、`baz/boo/`、`lib/` 内全部文件各自的路径集合与 SHA-256 与执行前一致（`-X` 走 `list_workdir_files_unfiltered`，`src/command/clean.rs:218-222`）。
- 文档门 → 「文档与网站验收命令」第 1–4 步（含后端 `--write --only clean`、`typecheck`/`build`、本卡页面的独立 Claude Code `VERDICT: PASS` 与签名+DCO 本地提交；按第 5 步不推送）；推送、聚合评审与第 6 步部署由 `WC-04R` 执行；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::clean_test::clean_single_force_keeps_nested_repos)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1–AC7）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::clean_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）

**Full-suite trigger:** `N/A（family child；由 WC-04R 统一 T-2）`
**Dependencies:** `DEP-WC-05`, `DEP-WC-08`（后端 `cf` 推送冻结窗口已授予）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`（本地签名提交、不推送，见「文档与网站验收命令」第 5 步）
**Release write set:** `N/A`
**Files likely touched:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`COMPATIBILITY.md`（只在既有条目行内追加）与后端 `clean.en.md`（嵌套仓库在单 force 下一律保留——该表述随 `WC-04R` 与 `WC-04b` 一并发布；后端页面在本卡本地提交、由 `WC-04R` 推送）。
**Rollback mode:** `revert`（家族窗口内以 `libra revert` 撤回本地提交，含后端本地提交；发布后由 `WC-04R` immutable-release 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-WC-NEST`（1/2）
**C/D coverage from:** `WC-04R`
**Granularity:** `type=implementation; axis=clean单force嵌套库保护-非pathspec路径; recovery=家族窗口内revert; complete=yes; self-contained=yes; AC=7/8（AC1–AC7 各 1）; VER=2/8; landing=1; prod-files=1; scope=M; deps=DEP-WC-05,DEP-WC-08; writeset=clean 链首卡（WC-04..05a 串行）; release=family child; split-from=N/A; exception=N/A`

---

### Task WC-04a: 支持 `libra clean -f -f`（双 `-f`）移除嵌套仓库（#735）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`force` 改为计数型，`-ff`/`-f -f` 接受；双 force 在 `-d`/显式目录 pathspec 下删除嵌套 Git/Libra 仓库；新增 `clean_double_force_nested_repo`；clean 39/39。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）
**Description:** 自 `WC-04` 拆出：将 `CleanArgs::force` 改为计数型（`u8` + `ArgAction::Count`）以接受 `-f -f` / `-ff`；`force >= 2` 时取消 `WC-04` 的嵌套仓库排除（上游 `git@750f5de7` `builtin/clean.c:959` `if (force > 1)`），在 `-d` 或显式目录 pathspec（`WC-03` 引入的目录递归）下删除嵌套仓库。
**Out of scope:** 单 force 嵌套仓库保护（`WC-04`）；`clean.requireForce`（`WC-05`，配置豁免不等同双 force）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `CleanArgs::force` 为 `bool`，重复 `-f` 直接报 `cannot be used multiple times` | `src/command/clean.rs:40-41` |
| 上游仅在 `force > 1` 时去掉 `REMOVE_DIR_KEEP_NESTED_GIT` | 上游 `git@750f5de7` `builtin/clean.c:927,959` |

**Acceptance criteria:**
- [x] AC1：`libra clean -f -f -d` 被 CLI 接受（exit 0）（`clean_test::clean_double_force_nested_repo`）。
- [x] AC2：`libra clean -ffd` 被 CLI 接受（exit 0）（`clean_test::clean_double_force_nested_repo`）。
- [x] AC3：上游 `t7300#41`（`TC-0552`）等价场景通过：双 force `libra clean -f -f -d` 后上游该用例的全部 `test_path_is_missing` 断言成立（`clean_test::clean_double_force_nested_repo`）。
- [x] AC4：双 force `libra clean -f -f -d` 删除嵌套 Libra 仓库 `lib/`（`clean_test::clean_double_force_nested_repo`）。
- [x] AC5：显式目录 pathspec、无 `-d` 的双 force `libra clean -f -f foo/` 删除 `foo/`（`clean_test::clean_double_force_nested_repo`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；在新 `libra init` 的父仓库中按上游 `git@750f5de7` `t/t7300-clean.sh:608-630`（`t7300#41`）构造两个嵌套 Git 仓库 `foo/`、`baz/boo/`（不依赖外部 `git` 二进制：各自手工建最小 gitdir，即内容为 `ref: refs/heads/main\n` 的 `.git/HEAD`、空目录 `.git/objects/` 与 `.git/refs/`、空文件 `.git/index`；另放 `foo/hello.world`（内容 `hello\n`）与 `baz/boo/deeper.world`（内容 `deeper\n`））、普通未跟踪目录 `bar/`（内含空文件 `goodbye.people`），以及在父仓库内执行 `libra init lib` 创建的嵌套 Libra 仓库 `lib/`（另放 `lib/notes.txt`，内容 `notes\n`）；以上均不加入父仓库索引，无任何忽略来源；每条 AC 使用新 fixture。
- AC1 → `command::clean_test::clean_double_force_nested_repo`：执行 `libra clean -f -f -d`；期望 exit 0、stderr 不含 `cannot be used multiple times`。
- AC2 → 同一测试：执行 `libra clean -ffd`；期望 exit 0、stderr 不含 `cannot be used multiple times`。
- AC3 → 同一测试：执行 `libra clean -f -f -d`；期望 exit 0，且与上游 `t7300#41` 逐项一致：`foo`、`bar`、`baz` 均不存在。
- AC4 → 同一测试同一次执行：`lib/` 不存在。
- AC5 → 同一测试：执行 `libra clean -f -f foo/`；期望 exit 0；之后全量快照恰好少了 `foo/`（含其下全部条目）。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`clean_double_force_nested_repo`（通过）
- [x] 共享回归组：`command::clean_test`（39/39）

**Full-suite trigger:** `none`
**Dependencies:** `WC-03`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`COMPATIBILITY.md`（只在既有条目行内追加）与后端 `clean.en.md`（`-f -f` 语义；部署核验标记为 `-f -f`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clean双force移除嵌套库; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/8（AC1–AC5 各 1）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-03,DEP-WC-05; writeset=序列化于 WC-03; release=independent; split-from=WC-04; exception=N/A`

---

### Task WC-04b: 显式 pathspec 路径的单 force 嵌套仓库保护（#735 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：显式 pathspec 路径复用 WC-04 的嵌套仓库剔除谓词（`candidate == root || candidate.starts_with(root) || root.starts_with(candidate)`），指向嵌套仓库目录、其内文件或包含它的父目录的单 force 均不删除嵌套文件；该谓词在 pathspec 过滤之后、预览/删除之前执行。AC1–AC5 由 `command::clean_test::clean_single_force_pathspec_keeps_nested_repos` 覆盖（`foo/`、`lib/`、`foo/hello.world`、`lib/notes.txt`、`baz/` 五条 pathspec 的路径+SHA-256 快照）；`clean_test` 35/35；clean EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行独立 Claude PASS 文档门；后端站点按家族规则仅本地签名提交、未推送（由发布点 `WC-04R` 统一推送）；计划中的 `/Volumes/Data` 路径以本机 `/mnt/data/gitmono/libra-backend` 执行。
**Description:** 自 `WC-04` 拆出：显式 pathspec 指向嵌套仓库目录、嵌套仓库内文件或包含嵌套仓库的父目录时，单 force 同样不删除嵌套仓库内任何文件，父目录不得被整体删除（对齐上游默认 `REMOVE_DIR_KEEP_NESTED_GIT`，`git@750f5de7` `builtin/clean.c:927`；上游 `t7300#38` 指定嵌套工作树内路径不删除）。本卡与 `WC-04` 同属 `REL-WC-NEST` 家族，由 `WC-04R` 一次发布，先于 `WC-01`..`WC-05a` 中其它 clean 卡；`WC-03` 引入目录候选后，本卡测试作为共享回归防止父目录被整体 `remove_dir_all`。
**Out of scope:** 无 pathspec 路径与预览（`WC-04`）；双 force 删除（`WC-04a`）；显式 pathspec 的目录递归语义本身（`WC-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 显式 pathspec 只经共享匹配器过滤候选，不识别嵌套仓库 | `src/command/clean.rs:281-288` |
| 目录候选以 `remove_dir_all` 整体删除 | `src/command/clean.rs:318-323` |
| `list_files` 只跳过 `.git`/`.libra` 元数据目录本身 | `src/utils/util.rs:1785-1800` |

**Acceptance criteria:**
- [ ] AC1：`libra clean -f foo/`（pathspec 为嵌套 Git 仓库目录）后 `foo/` 内全部文件保留且内容不变。
- [ ] AC2：`libra clean -f lib/`（pathspec 为嵌套 Libra 仓库目录）后 `lib/` 内全部文件保留且内容不变。
- [ ] AC3：`libra clean -f foo/hello.world`（pathspec 为嵌套 Git 仓库内文件）后 `foo/` 内全部文件保留且内容不变。
- [ ] AC4：`libra clean -f lib/notes.txt`（pathspec 为嵌套 Libra 仓库内文件）后 `lib/` 内全部文件保留且内容不变。
- [ ] AC5：`libra clean -f baz/`（pathspec 为包含嵌套仓库 `baz/boo/` 的父目录）后 `baz/boo/` 内全部文件保留且内容不变。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置（本卡自含，不依赖其它卡正文）：在新 `libra init` 的父仓库中构造上游 `t7300#30` 布局——两个嵌套 Git 仓库 `foo/`、`baz/boo/`（不依赖外部 `git` 二进制：各自手工建最小 gitdir，即内容为 `ref: refs/heads/main\n` 的 `.git/HEAD`、空目录 `.git/objects/` 与 `.git/refs/`、空文件 `.git/index`；另放 `foo/hello.world`（内容 `hello\n`）与 `baz/boo/deeper.world`（内容 `deeper\n`）），普通未跟踪目录 `bar/`（内含空文件 `goodbye.people`），以及在父仓库内执行 `libra init lib` 创建的嵌套 Libra 仓库 `lib/`（另放 `lib/notes.txt`，内容 `notes\n`）；以上均不加入父仓库索引，且无任何忽略来源（按「验证执行协议」第 9 条隔离运行）。每条 AC 使用新 fixture，执行前记录对应嵌套仓库内全部文件的路径集合与 SHA-256。
- AC1 → `command::clean_test::clean_single_force_pathspec_keeps_nested_repos`：执行 `libra clean -f foo/`；期望 exit 0；之后 `foo/` 内全部文件的路径集合与 SHA-256 与执行前一致。
- AC2 → 同一测试：执行 `libra clean -f lib/`；期望 exit 0；之后 `lib/` 内全部文件的路径集合与 SHA-256 与执行前一致。
- AC3 → 同一测试：执行 `libra clean -f foo/hello.world`；期望 exit 0；之后 `foo/` 内全部文件（含 `foo/.git` 下文件与 `foo/hello.world`）的路径集合与 SHA-256 与执行前一致。
- AC4 → 同一测试：执行 `libra clean -f lib/notes.txt`；期望 exit 0；之后 `lib/` 内全部文件（含 `lib/.libra` 下文件与 `lib/notes.txt`）的路径集合与 SHA-256 与执行前一致。
- AC5 → 同一测试：执行 `libra clean -f baz/`；期望 exit 0；之后 `baz/boo/` 内全部文件的路径集合与 SHA-256 与执行前一致。
- 文档门 → 「文档与网站验收命令」第 1–4 步（含后端 `--write --only clean`、`typecheck`/`build`、本卡页面的独立 Claude Code `VERDICT: PASS` 与签名+DCO 本地提交；按第 5 步不推送）；推送、聚合评审与第 6 步部署由 `WC-04R` 执行；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::clean_test::clean_single_force_pathspec_keeps_nested_repos)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1–AC5）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::clean_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）

**Full-suite trigger:** `N/A（family child；由 WC-04R 统一 T-2）`
**Dependencies:** `WC-04`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`（本地签名提交、不推送，见「文档与网站验收命令」第 5 步）
**Release write set:** `N/A`
**Files likely touched:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`COMPATIBILITY.md`（只在既有条目行内追加）与后端 `clean.en.md`（显式 pathspec 下嵌套仓库同样保留；后端页面在本卡本地提交、由 `WC-04R` 推送）。
**Rollback mode:** `revert`（家族窗口内以 `libra revert` 撤回本地提交，含后端本地提交；发布后由 `WC-04R` immutable-release 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-WC-NEST`（2/2）
**C/D coverage from:** `WC-04R`
**Granularity:** `type=implementation; axis=clean单force嵌套库保护-显式pathspec路径; recovery=家族窗口内revert; complete=yes; self-contained=yes; AC=5/8（AC1–AC5 各 1）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-04,DEP-WC-05; writeset=序列化于 WC-04; release=family child; split-from=WC-04; exception=N/A`

---

### Task WC-04R: `REL-WC-NEST` 嵌套仓库单 force 保护联合发布点

**Task type:** release
**Lifecycle / Acceptance:** pending / 空
**Description:** 聚合 `WC-04` 与 `WC-04b`，保证「无 pathspec 路径」与「显式 pathspec 路径」的嵌套仓库保护只以一个版本同时对外发布，任何对外版本都不会以单 force 删除嵌套仓库内容。两张子卡仅本地签名提交（Libra 仓与后端 `cf`）；本卡统一执行 T-2 全量、`patch + 1`、版本面同步、`Cargo.lock` 工具链刷新、ER-04 C 组（发布步使用 `gh release create "v${VERSION}" --target "$MERGE_SHA" --notes-file <AC3 的 notes 文件>`，notes 文件放仓库外临时目录）与 D 组，并按「文档与网站验收命令」第 5–6 步聚合核对、推送与部署子卡已本地提交的 `clean` 网站页面。本卡不引入新行为。
**Out of scope:** 任何产品行为改动（由子卡承担）；双 force（`WC-04a`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 家族成员、窗口规则与回滚顺序 | 本计划「发布分组与并发窗口」`REL-WC-NEST` |
| 当前单 force 下嵌套仓库内普通文件仍可被删除（子卡修复的缺口） | `src/utils/util.rs:1785-1800`；`src/command/clean.rs:218-249,281-288,310-328` |

**Acceptance criteria:**
- [ ] AC1：聚合守卫：`WC-04` 与 `WC-04b` 的具名测试在 bump 后的最终提交树上通过。
- [ ] AC2：兼容证据：`COMPATIBILITY.md` 的 `clean` 条目对嵌套仓库保护的描述与最终行为一致，且文件行数不变。
- [ ] AC3：发布前拟定的 release note（`gh release create --notes-file` 的输入文件）列出单 force 嵌套仓库保护覆盖的两类删除路径（无 pathspec：有/无 `-d`、`-n -d` 预览、`-x`/`-X`；显式 pathspec：指向嵌套仓库目录、其内文件或包含嵌套仓库的父目录），且不声称已支持 `-f -f`（由后续 `WC-04a` 发布）。
- [ ] ER-06/ER-06a 同卡强制门：`clean` 网站页面按「文档与网站验收命令」第 5–6 步聚合推送与部署（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- AC1 → Verification 第 1–2 条（在 bump 后最终提交树上执行，记录提交 SHA 与 `HEAD^{tree}`）。
- AC2 → 「文档与网站验收命令」第 1 步对 `COMPATIBILITY.md` 的逐文件核对（`wc -l` 前后相等）。
- AC3 → Verification 第 3 条手工证据。
- 文档门 → 「文档与网站验收命令」第 5–6 步（`--only clean` 重新同步、家族路径集合核对、聚合评审、推送、部署与释放 `DEP-WC-08`）；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::clean_test::clean_single_force_keeps_nested_repos)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-04` 新增））
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::clean_test::clean_single_force_pathspec_keeps_nested_repos)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-04b` 新增））
- [ ] 手工证据（release note，发布前）：逐项核对 notes 输入文件含 AC3 所列两类路径且无 `-f -f` 表述；发布后的 `gh release view "v${VERSION}"` 正文核对归 D 组（D3）。

**Full-suite trigger:** T-2
**Dependencies:** WC-04b, DEP-WC-08
**Deliverables:** N/A
**Implementation write set:** `N/A`（不改产品代码与文档内容；后端 `clean.en.md` 由子卡提交，本卡只推送与部署）
**Docs and compatibility impact:** 逐文件：后端 `clean.en.md` 已由子卡本地签名提交，本卡重新同步核对后推送并部署；`docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`COMPATIBILITY.md` 由子卡交付、本卡只核对；部署核验标记为字面量 `nested`（不区分大小写，以 `curl -fsSL https://libra.tools/en/docs/commands/clean | grep -qi nested` 判定；2026-11-18 现行后端 `clean.en.md` 与 `docs/commands/clean.md` 均不含该词，子卡写入的英文页面须以该词描述嵌套仓库保护）。
**Release write set:** Inherited
**Rollback mode:** immutable-release
**Estimated scope:** S
**Version increment:** patch
**Release boundary:** family release point of REL-WC-NEST
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-WC-NEST联合发布; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/12（AC1 1、AC2 1、AC3 2；AC3 含列出两类路径与不声称支持双 force 两个谓词）; VER=3/12; landing=0; prod-files=0; scope=S; deps=WC-04b,DEP-WC-08; writeset=序列化于 WC-04b; release=family release point; split-from=N/A; exception=N/A`

---

### Task WC-05: 支持 `clean.requireForce` 配置（#736）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：clean 级联读取 `clean.requireForce`（默认 true），false/true/非法值行为与 Git 对齐；新增 `clean_require_force_config`；clean 40/40。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）
**Description:** 在 `clean` 执行前级联读取 `clean.requireForce`（默认 `true`）；当显式设为 `false`/`no`/`off` 时允许不带 `-f`/`-n` 执行清理。已拆出 `WC-05a`。
**Out of scope:** `-e` 目录前缀匹配（`WC-05a`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `run_clean` 硬编码 `if !args.force && !args.dry_run { return Err(CleanError::MissingMode); }` | `src/command/clean.rs:183-186`; 既有拒绝契约见 `tests/command/clean_test.rs:68-90`（exit 129、`LBR-CLI-002`） |
| Git 以规范化小写键比较（`strcmp(var, "clean.requireforce")`），非法值报 `fatal: bad boolean config value 'maybe' for 'clean.requireforce'`（2026-11-18 以 git 2.54.0 实测，exit 128）；Libra 既有 `core.filemode` 诊断同样输出小写键名 | 上游 `git@750f5de7` `builtin/clean.c:132`、`config.c:1331`；`src/internal/config.rs:1829` |

**Acceptance criteria:**
- [x] AC1：`clean.requireForce` 为 `no`、`off` 或 `0`（各值分别验证）时 `libra clean`（无 `-f`/`-n`）按 `clean -f` 语义清理未跟踪文件。（`clean_require_force_config`）
- [x] AC2：`clean.requireForce=false` 时结果同 AC1。（`clean_require_force_config`）
- [x] AC3：未设置时 `libra clean` 按既有契约拒绝（exit 129、`fatal: clean requires -f or -n`、`Error-Code: LBR-CLI-002`；上游 `t7300#23`）。（`clean_require_force_config`）
- [x] AC4：`clean.requireForce=true` 时结果同 AC3（上游 `t7300#24`）。（`clean_require_force_config`）
- [x] AC5：非法布尔值（`clean.requireForce=maybe`）时非零退出且 stderr 含 `bad boolean config value 'maybe' for 'clean.requireforce'`，同时未跟踪文件保留（fail-closed；键名按 Git 输出规范化小写形式）。（`clean_require_force_config`）
- [x] AC6：`clean.requireForce=false` 且不带 `-f`/`-n` 时按单 force 处理嵌套仓库：`libra clean -d` 与 `libra clean nested/` 两种形态都保留嵌套仓库内全部文件且内容不变（配置豁免不等同双 force）。（`clean_require_force_config`）
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行，无任何忽略来源；每条 AC 使用新 fixture：新仓库内放未跟踪 `untracked.txt`（内容 `untracked\n`）；AC6 另放嵌套 Libra 仓库 `nested/`（在仓库内执行 `libra init nested`，另放 `nested/notes.txt`，内容 `notes\n`）。
- AC1 → `command::clean_test::clean_require_force_config`：对 `no`、`off`、`0` 三个值各用新 fixture，`libra config set clean.requireForce <值>` 后执行 `libra clean`；期望每次 exit 0、stdout 含 `Removing untracked.txt`；之后全量快照恰好少了 `untracked.txt`。
- AC2 → 同一测试：值改为 `false` 重复；期望同 AC1。
- AC3 → 同一测试：新 fixture 中不设置 `clean.requireForce`（不执行任何 `config set`）直接执行 `libra clean`；期望 exit 129、stderr 含 `fatal: clean requires -f or -n` 与 `Error-Code: LBR-CLI-002`；执行前后全量快照相同。
- AC4 → 同一测试：值设为 `true` 重复；期望同 AC3。
- AC5 → 同一测试：值设为 `maybe` 后执行 `libra clean`；期望非零退出、stderr 含 `bad boolean config value 'maybe' for 'clean.requireforce'`；执行前后全量快照相同。
- AC6 → 同一测试：值设为 `false`，分别执行 `libra clean -d` 与 `libra clean nested/`；两次均 exit 0，且每次之后 `nested/` 内全部文件（含 `nested/.libra` 下文件与 `nested/notes.txt`）的路径集合与 SHA-256 与执行前一致。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`clean_require_force_config`（通过）
- [x] 共享回归组：`command::clean_test`（40/40）

**Full-suite trigger:** `none`
**Dependencies:** `WC-04a`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`COMPATIBILITY.md` 与后端 `clean.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clean.requireForce配置; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8（AC1–AC4 各 1、AC5 2、AC6 2）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-04a,DEP-WC-05; writeset=序列化于 WC-04a; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-05a: 修复 `clean -e` 排除目录前缀匹配（#736 / `t7300#19`）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`clean -e` 支持目录前缀与无斜杠 basename 匹配；新增 `clean_exclude_dir_prefix`；clean 41/41。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）
**Description:** 自 `WC-05` 拆出：修复 `matches_exclude_pattern` 仅生成 `^pattern$` 锚定正则的问题，使其遵循 `.gitignore` 风格规则，当传入 `-e src` 时能排除 `src/part3.c` 等子路径。
**Out of scope:** `clean.requireForce` 配置读取（`WC-05`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `matches_exclude_pattern` 生成 `^src$` 无法匹配 `src/part3.c` | `src/command/clean.rs:451-475` |

**Acceptance criteria:**
- [x] AC1：上游 `t7300#19`（`TC-0556`）等价场景通过：`libra clean -f -d -x -e src` 后上游该用例的全部 `test_path_is_*` 断言在 Libra 中成立（`-e src` 使 `src/part3.c` 等 `src/` 子路径免于删除）。（`clean_exclude_dir_prefix`）
- [x] AC2：`libra clean -f -e 1 -e 2` 的结果与上游 `t7300#42` 一致（`1`、`2`、`known` 保留，`3` 删除）。（`clean_exclude_dir_prefix`）
- [x] AC3：`libra clean -f -e '*.o'` 使所有匹配 `*.o` 的未跟踪文件（含子目录内）免于删除。（`clean_exclude_dir_prefix`）
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- AC1 → `command::clean_test::clean_exclude_dir_prefix`：按「验证执行协议」第 9 条隔离运行；fixture 复刻上游 `git@750f5de7` `t/t7300-clean.sh:12-23` setup（`.gitignore` 内容恰为 `build\n*.o\n`，与空文件 `Makefile`、`src/part1.c` 一起提交，再暂存空文件 `README`、`src/part2.c`；除该 `.gitignore` 外无其它忽略来源），用例内再创建未跟踪空文件 `a.out`、`src/part3.c`、`docs/manual.txt`、`obj.o`、`build/lib.so`；执行 `libra clean -f -d -x -e src`；期望 exit 0，且与上游 `t/t7300-clean.sh:297-312` 逐项一致：`src/part1.c`、`src/part2.c`、`src/part3.c`、`Makefile`、`README` 存在，`a.out`、`docs/`、`obj.o`、`build/` 不存在；全量快照恰好少了 `a.out`、`docs/`、`obj.o`、`build/`。
- AC2 → 同一测试：新仓库（无任何忽略来源）内暂存空文件 `known`，未跟踪空文件 `1`、`2`、`3`（同上游 `t/t7300-clean.sh:631-645` 的 `touch` 与 `git add known`），执行 `libra clean -f -e 1 -e 2`；期望 exit 0；之后 `1`、`2`、`known` 存在，全量快照恰好少了 `3`。
- AC3 → 同一测试：新仓库（无 `.gitignore` 等任何忽略来源，故 `*.o` 未被忽略）内已提交 `sub/keep.txt`（内容 `keep\n`），未跟踪 `a.o`、`sub/b.o`、`c.txt`（内容依次为 `a\n`、`b\n`、`c\n`），执行 `libra clean -f -e '*.o'`；期望 exit 0；之后 `a.o`、`sub/b.o` 存在，全量快照恰好少了 `c.txt`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`clean_exclude_dir_prefix`（通过）
- [x] 共享回归组：`command::clean_test`（41/41）

**Full-suite trigger:** `none`
**Dependencies:** `WC-05`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/commands/clean.md`, `docs/commands/zh-CN/clean.md`, `docs/development/commands/clean.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clean.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/clean.rs`, `tests/command/clean_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/clean.md`、`docs/commands/zh-CN/clean.md`、`docs/development/commands/clean.md`、`COMPATIBILITY.md`（`clean -e` 目录前缀语义，只在既有条目行内追加）与后端 `clean.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=clean-e目录前缀匹配; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=3/8（AC1–AC3 各 1）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-05,DEP-WC-05; writeset=序列化于 WC-05; release=independent; split-from=WC-05; exception=N/A`

---

### Task WC-06: 修复 `libra mv` 将目录重命名为不存在的新路径（#755）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：单源目录可重命名到不存在路径，索引前缀同步；既有文件目标与多源非目录拒绝保持；新增 `mv_rename_directory_to_new_path`；mv 34/34。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）
**Description:** 修复 `validate_source_directory`：当单个源目录重命名为不存在的目标路径（`libra mv dir folder`）时直接将 `dir/` 下已跟踪条目重命名为 `folder/`，仅当多源移动且目标非目录时才报 `destination is not a directory`。
**Out of scope:** 跨文件系统符号链接/submodule 移动（永久非目标，本计划不涉及）；`t6421#1`（`TC-0733`）下游 partial-clone 缺失对象按需拉取与 merge 端到端闭环（拆出 `WC-06a` 承接，本卡仅覆盖 `libra mv dir folder` 目录重命名阶段）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `validate_source_directory` 无条件要求 `if !destination_is_dir { return Err("fatal: destination ... is not a directory") }` | `src/command/mv.rs:280-292` |

**Acceptance criteria:**
- [x] AC1：单源目录移到不存在的目标：`libra mv dir folder` 把工作区 `dir/` 整体重命名为 `folder/`（`mv_test::mv_rename_directory_to_new_path` / 既有拒绝测试）。
- [x] AC2：同一操作把索引中原 `dir/...` 条目改名为 `folder/...`（blob OID 与模式不变）（`mv_test::mv_rename_directory_to_new_path` / 既有拒绝测试）。
- [x] AC3：目标为已存在的常规文件（非大小写重命名）时拒绝执行（非零退出，不改动工作区与索引）（`mv_test::mv_rename_directory_to_new_path` / 既有拒绝测试）。
- [x] AC4：多源移动且目标不是目录时以 `fatal: destination '<dst>' is not a directory` 拒绝（不改动工作区与索引）（`mv_test::mv_rename_directory_to_new_path` / 既有拒绝测试）。
- [x] AC5：上游 `t6421#1`（`TC-0733`）setup 中 A 分支的目录重命名步骤等价场景通过：在含嵌套子目录与已修改文件的 `dir/` 上执行 `libra mv dir folder` 并提交后，提交树只含 `folder/` 布局（下游 partial-clone 对象拉取由 `WC-06a` 承接）（`mv_test::mv_rename_directory_to_new_path` / 既有拒绝测试）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- AC1 → `command::mv_test::mv_rename_directory_to_new_path`：按「验证执行协议」第 9 条隔离运行；已提交 `dir/a`（内容 `a\n`）、`dir/sub/b`（内容 `b\n`），执行 `libra mv dir folder`；期望 exit 0；之后 `dir/` 不存在，`folder/a`、`folder/sub/b` 内容与原文件一致。
- AC2 → 同一测试：读取索引（`libra ls-files -s`）；期望只含 `folder/a`、`folder/sub/b`，OID 与模式等于原 `dir/` 条目。
- AC3 → 同一测试：新建并提交常规文件 `file`（内容 `file\n`），执行 `libra mv folder file`；期望非零退出、stderr 以 `fatal:` 开头并点名 `file`；执行前后全量快照相同。
- AC4 → 同一测试：执行 `libra mv folder/a folder/sub/b nonexist`；期望非零退出、stderr 含 `fatal: destination 'nonexist' is not a directory`；执行前后全量快照相同。
- AC5 → 同一测试：在新仓库中逐字复刻上游 `git@750f5de7` `t/t6421-merge-partial-clone.sh:31-145` `test_setup_repo` 的 O 提交与 A 分支至 `:102`（`git mv dir/subdir/e dir/subdir/newsubdir/`）为止的步骤（文件内容照抄；含 `Makefile` 追加 `more`、新建未跟踪的 `dir/subdir/newsubdir/newfile.rs`）后，按上游 `:103-105` 依次执行 `libra mv dir folder`、`libra add .` 与 `libra commit -m A`（`newfile.rs` 在 `mv` 时仍未跟踪，随目录一并移动后由 `add .` 纳入）；期望三条命令 exit 0，`libra ls-tree -r HEAD` 不含 `dir/` 前缀且含 `folder/subdir/Makefile`、`folder/subdir/newsubdir/e`、`folder/subdir/newsubdir/newfile.rs`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`mv_rename_directory_to_new_path`（通过）
- [x] 共享回归组：`command::mv_test`（34/34）

**Full-suite trigger:** `none`
**Dependencies:** `WC-05a`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/mv.rs`, `tests/command/mv_test.rs`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/mv.rs`, `tests/command/mv_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/mv.md`、`docs/commands/zh-CN/mv.md`、`docs/development/commands/mv.md`、`COMPATIBILITY.md` 与后端 `mv.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=mv目录重命名到新路径; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/8（AC1 1、AC2 1、AC3 2、AC4 2、AC5 1；AC3/AC4 的拒绝与「不改动工作区与索引」分别计）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-05a,DEP-WC-05; writeset=序列化于 WC-05a; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-06a: 只读核验并判定 `TC-0733`（`t6421#1`）partial-clone merge 闭环（#755）

**Task type:** `spike`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `WC-06` 拆出（`spike` 只读核验卡，**Timebox: 1 人日**（G-11：`M` ≤ 2 人日），不直接修改 `merge.rs` 或 `mv.rs` 产品源码）：在 `WC-06`（`libra mv dir folder`）、`WC-12`（`COMPATIBILITY.md` 串行前驱）与 `DEP-WC-06`（`clone.md#CL-13` `--filter=blob:none`、`CL-19` promisor 按需拉取、`CL-20` 目录重命名 merge 按需拉取、`plumbing-misc.md#PM-03` `rev-list --missing=print` 及 `merge-pull.md` merge 重命名链）全部合入 `main` 后，按上游 `git@750f5de7` `t/t6421-merge-partial-clone.sh:209-246`（`t6421#1` / `TC-0733`）契约核验 `libra mv dir folder` + `--filter=blob:none` + merge 行为并记录实测 `fetch` 次数、下载对象数与 `rev-list --objects --missing=print` 结果。**Go / No-Go 退出判定（任一路径达标均可使本卡达到 `locally-accepted` 并放行下游 `WC-13`；`complete` 待 `WC-13` 的 C 与 D1–D3 及本卡网站 D4 全绿后推进）：**（1）Go：完整满足 `t6421#1`（2 次按需 fetch：第 1 次 2 对象、第 2 次 1 对象，而非 `TC-0734` 的 1 fetch / 6 objects）时，补齐 `tc0733_merge_partial_clone_full` 测试、同步 `mv` 文档与 `COMPATIBILITY.md`，并由后续独立发布卡 `WC-13`（依赖 `WC-06a`）合并覆盖 C/D 门控后关闭 `#755`；（2）No-Go（缺 `GIT_TRACE2_PERF` 或暴露 `merge.rs` 缺口）：在 `COMPATIBILITY.md` 登记 `#755` AC3 为 `blocked`，分别交接至 `DEFER-WC-02`（`plan-long.md`，`trace2` PERF 专项负责人）或由 `mv`/`merge` 协调者开立具名后续实现卡 `WC-06b` 承接，完成后同样放行 `WC-13`（仅以 `blocked` 登记收口，不宣称关闭 `#755`；已编写的 `tc0733` 测试以 `#[ignore]` 标注承接 ID 后提交，见 Verification 第 2–3 条）。
**Out of scope:** 直接在本卡修改 `src/command/mv.rs` 或 `src/command/merge.rs` 产品代码；实现 `GIT_TRACE2_PERF`（`DEFER-WC-02`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#755` AC3 对应 `t6421#1`（`TC-0733`：`2` 次 fetch 分别下载 `2`/`1` 对象、`rev-list --objects --missing=print`、`GIT_TRACE2_PERF`），依赖 `WC-06`、`CL-13/19/20`、`PM-03` 与 `merge-pull` | 上游 `git@750f5de7` `t/t6421-merge-partial-clone.sh:209-246`；`plan-20261117-clone.md#CL-13, CL-19, CL-20`；`plan-20261117-plumbing-misc.md#PM-03` |
| 当前仓库尚无 `GIT_TRACE2_PERF`（`CS-24` 只规划 `GIT_TRACE2_EVENT`） | `rg -n 'GIT_TRACE2' src` 于 2026-11-18 零命中；`plan-20261117-commit-signing.md#CS-24` |

**Acceptance criteria:**
- [ ] AC1：能力探针结论（`src/` 是否已有 `GIT_TRACE2_PERF`）与 `DEP-WC-06` 各卡合入状态已写入本卡 `Current evidence`。
- [ ] AC2：Go/No-Go 判定与承接登记一致：Go 时 `command::mv_test::tc0733_merge_partial_clone_full` 按「判据规范」通过；能力 No-Go 时 `plan-long.md` 已登记 `DEFER-WC-02`；产品 No-Go 时本计划已以完整任务卡（全部必填字段、`Dependencies`、写集与具名测试）登记承接卡 `WC-06b` 且该修订取得计划文本双 PASS。
- [ ] AC3：`COMPATIBILITY.md` 中 `#755` AC3 的状态与判定一致（Go：注明由 `WC-13` 覆盖 C/D 后关闭 `#755`；No-Go：`blocked` 并指向 `DEFER-WC-02` 或 `WC-06b`），且两种路径都放行下游 `WC-13`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- AC1 → Verification 第 1 条守卫输出与 `DEP-WC-06` 核对记录（逐卡写明 `CL-13`、`CL-19`、`CL-20`、`PM-03`、`MP-14..20`、`MP-24..25` 的合入提交）。
- AC2 → Go：Verification 第 2 条 exit 0 并按「验证执行协议」第 1–4 条通过（断言见下方「判据规范」）；产品 No-Go：Verification 第 2 条非零退出，本卡记录中的原始观测按「判据规范」失败分类可归因实现，且第 3 条守卫命中 `WC-06b` 登记与测试忽略标注；能力 No-Go：第 1 条探针为 `absent`，或第 2 条已运行且原始 trace 证据按「判据规范」失败分类判为能力 No-Go（trace 不支持或字段缺失），且第 3 条守卫命中 `DEFER-WC-02`（运行期能力 No-Go 另须命中测试忽略标注）。
- AC3 → 「文档与网站验收命令」第 1 步对 `COMPATIBILITY.md` 的逐文件核对（行数不变、`#755` 条目状态与判定一致）。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**判据规范（Go 路径测试 `command::mv_test::tc0733_merge_partial_clone_full` 的断言，非计数正文）：**
- fixture：完整复刻上游 `git@750f5de7` `t/t6421-merge-partial-clone.sh:31-145` `test_setup_repo` 中 O、A、B-single 三个提交与分支；服务端为 Libra 仓库并设置 `uploadpack.allowfilter=true`、`uploadpack.allowanysha1inwant=true`；不得改写上游文件内容或改用简化 rename 图；A 分支的 `dir → folder` 必须经 `libra mv dir folder` 完成。
- 三个目标 blob：O 的 `dir/subdir/Makefile`（`general/leap1` 的 `2..9` 八行加 `toplevel makefile`）、A 的 `folder/subdir/Makefile`（O 内容追加 `more`）、B-single 的 `dir/subdir/Makefile`（`new first line` 开头，随后 `general/leap1` 八行与 `toplevel makefile`）；运行时读取三者真实 OID，不预写哈希。
- 克隆：以 `env!("CARGO_BIN_EXE_libra")` 执行 `clone --sparse --filter=blob:none "file://<服务端绝对路径>" <测试临时目录>/partial`，保持上游 sparse cone 布局。
- 对象窗口：checkout `origin/A` 前、后与 merge 后分别取 `libra rev-list --objects --all --missing=print` 输出，仅取以 `?` 开头的行规范化为缺失 OID 集合 M0、M1、M2；两个窗口分别断言：checkout 窗口 M0−M1 与 M1−M0 均为空（上游 sparse cone 下 checkout `origin/A` 不需任何 blob，上游只在 merge 窗口计 `2+1` 次拉取）；merge 窗口 M1−M2 恰为三个目标 OID 且 M2−M1 为空；由此 M0−M2 恰为三个目标 OID、M2−M0 为空，与上游 `comm` 断言（`old` 3 行、`new` 为空）一致；不得只断言 M2−M0。
- merge：`libra config set merge.directoryRenames true` 后以 `GIT_TRACE2_PERF=<merge trace 文件>` 执行 `libra merge --no-stat --no-progress origin/B-single`（checkout 使用另一 trace 文件）；merge trace 依次出现 `fetch_count:2`、`fetch_count:1`，且第二字段为 `d0`、命令含 `fetch.negotiationAlgorithm` 的顶层 `child_start` 恰好 2 条。
- 合并结果：保留 A 的 `folder/` 布局、`general/{jump1,jump2}`、`general/newfile.pl`、`basename/subdir/*`、`folder/subdir/newsubdir/{e,newfile.rs}`；`folder/subdir/Makefile` 首行为 `new first line`、其后为 O 内容加 `more`；无冲突标记。
- 失败分类：trace 不支持或字段缺失 = 能力 No-Go；trace 路径/权限/解析器/fixture/网络故障 = 基础设施 FAIL（修复后重跑，不得判 No-Go）；事件完整但计数或对象集合错误且可归因实现 = 产品 No-Go；证据不足或无法归因保持 `blocked`；任何情况都不得伪称 Go。

**Verification:**
- [ ] 守卫（能力探针，Go/No-Go 共用）：`if rg -n 'GIT_TRACE2_PERF' src/; then echo "PROBE: present"; else rc=$?; if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi; echo "PROBE: absent"; fi`（`present` → 进入第 2 条；`absent` → 能力 No-Go，不编写也不执行第 2 条的测试，直接执行第 3 条；`ERROR` 即 STOP）
- [ ] 具名测试 / 场景执行（探针为 `present` 时执行，Go、产品 No-Go 与运行期能力 No-Go 共用；探针为 `absent` 时不编写也不执行）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::mv_test::tc0733_merge_partial_clone_full)' --no-capture` (new)（先按「验证执行协议」第 2 条确认恰好 1 条；该测试在各断言前把 M0/M1/M2、两个 trace 文件中的 `fetch_count` 与 `child_start` 行及各命令退出码写到 stderr；exit 0 且按第 3–4 条计数通过 → Go，覆盖 AC2 的 Go 分支；非零退出不按第 3 条直接 STOP，而按「验证执行协议」第 8 条与「判据规范」失败分类处理：基础设施 FAIL 修复后重跑；trace 不支持或字段缺失（运行期能力 No-Go）与可归因实现的产品 No-Go 均把原始观测与归因写入本卡记录后转第 3 条对应守卫；证据不足或无法归因保持 `blocked`；两种 No-Go 下该测试分别以 `#[ignore = "TC-0733: blocked by DEFER-WC-02"]`、`#[ignore = "TC-0733: blocked by WC-06b"]` 提交，不得以失败状态提交）
- [ ] 守卫（仅 No-Go 路径）：能力 No-Go 执行 `rg -n 'DEFER-WC-02' docs/development/plan/plan-long.md` 并核对命中行含重启条件与承接方（运行期能力 No-Go 另执行 `rg -n 'TC-0733: blocked by DEFER-WC-02' tests/command/mv_test.rs`，须命中）；产品 No-Go 执行 `rg -n '^### Task WC-06b' docs/development/plan/plan-20261117-worktree-clean-reset-apply.md` 与 `rg -n 'TC-0733: blocked by WC-06b' tests/command/mv_test.rs`（两者均须命中），并核对该卡含全部必填字段且本计划「Codex review log」有覆盖该修订的 Claude（Subagent）与 Codex 同一字节双 `PASS`；rg 退出 1 视为未登记 FAIL，>1 即 STOP。
- [ ] 守卫（`spike` allowlist，ER-04 B 组）：按「执行检查必备需求」中的「spike 产物 allowlist 守卫」执行；Libra 仓 allowlist 为本卡 `Deliverables` 中的仓内路径，后端仓 allowlist 为 `apps/tanstack-app/content/docs/commands/mv.en.md`。
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::mv_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据；partial-clone merge 相关回归）

**Full-suite trigger:** `N/A（no-release spike；C/D 继承 WC-13）`
**Dependencies:** `WC-06`, `WC-12`, `DEP-WC-05`, `DEP-WC-06`
**Deliverables:** `tests/command/mv_test.rs`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `COMPATIBILITY.md`, `docs/development/plan/plan-long.md`（仅 No-Go 登记 `DEFER-WC-02`/`WC-06b` 时写入）, `docs/development/plan/plan-status.md`（仅按计划状态协议登记本卡状态）, `docs/development/plan/plan-20261117-worktree-clean-reset-apply.md`（仅记录本卡核验/评审结论）, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`
**Implementation write set:** `tests/command/mv_test.rs`, `docs/commands/mv.md`, `docs/commands/zh-CN/mv.md`, `docs/development/commands/mv.md`, `COMPATIBILITY.md`, `docs/development/plan/plan-long.md`, `docs/development/plan/plan-status.md`, `docs/development/plan/plan-20261117-worktree-clean-reset-apply.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/mv.en.md`
**Release write set:** `N/A`（`no-release` `spike` 卡不修改产品源码、不触发版本发布）
**Files likely touched:** `tests/command/mv_test.rs`, `docs/...`, `COMPATIBILITY.md`, `docs/development/plan/{plan-long.md,plan-status.md,plan-20261117-worktree-clean-reset-apply.md}`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/mv.md`、`docs/commands/zh-CN/mv.md`、`docs/development/commands/mv.md`、`COMPATIBILITY.md` 与后端 `mv.en.md`。
**Rollback mode:** `revert`（若核验测试、登记或文档结论需回退，在 Libra 仓用 `libra revert <commit>` / `libra restore` 回退对应 `tests/`/`docs/`/`COMPATIBILITY.md`/`plan-long.md` 变更，后端仓执行 `(cd /Volumes/Data/gitmono/libra-backend && libra revert <commit>)` / `libra restore`）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `no-release`
**C/D coverage from:** `WC-13`（`WC-13` 显式依赖 `WC-06a` 并于合入后覆盖其测试与文档变更的 C/D 门控）
**Granularity:** `type=spike; axis=TC-0733-partial-clone只读核验与退出判定; recovery=revert 本卡测试/文档/登记提交，产品代码零改动; complete=yes; self-contained=yes; AC=3/8（AC1–AC3 各 1）; VER=6/8; landing=0; prod-files=0; scope=M; deps=WC-06,WC-12,DEP-WC-05,DEP-WC-06; writeset=序列化于 WC-12; release=no-release; split-from=WC-06; exception=N/A`

---

### Task WC-07: 核对并补全 `libra apply --numstat` `t4012#5` 兼容测试（#812）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：新增 `apply_numstat_t4012` 覆盖文本/二进制 numstat、只预览不应用、`--numstat --apply` 先输出后应用；apply 21/21。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）
**Description:** 核对 `apply.rs` 已实现的 `--numstat` 对 `t/t4012-diff-binary.sh#5`（二进制 + 文本补丁与 `--numstat` 格式）的兼容输出，修复细微格式差异并补齐兼容用例，按 `implementation` 卡完成代码核验/修复、测试与文档同步。
**Out of scope:** `--stat --summary`（`WC-08`）与 `--binary`（`WC-09`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ApplyArgs::numstat` 与 `format_apply_numstat` 已存在，需对照 #812 (`t4012#5`) 补齐兼容验证 | `src/command/apply.rs:254,382,748-767` |

**Acceptance criteria:**
- [x] AC1：`libra apply --numstat < diff` 输出恰为 `1\t1\ta`、`-\t-\tb`、`1\t1\tc`、`-\t-\td` 四行（即上游 `t4012#5` / `TC-1080`）（`apply_test::apply_numstat_t4012`）。
- [x] AC2：`--numstat` 单独使用时不应用补丁（工作区与索引不变）（`apply_test::apply_numstat_t4012`）。
- [x] AC3：多行文本改动的 `--numstat` 输出 `<ins>\t<del>\t<path>` 且计数等于补丁的增删行数（`apply_test::apply_numstat_t4012`）。
- [x] AC4：`--numstat --apply` 先输出 numstat 行（`apply_test::apply_numstat_t4012`）。
- [x] AC5：`--numstat --apply` 随后应用补丁（工作区为 post 内容）（`apply_test::apply_numstat_t4012`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行（无 `.gitattributes`）；fixture 复刻上游 `git@750f5de7` `t/t4012-diff-binary.sh` `prepare repository`：`a`、`b`、`c`、`d` 分别写入 `AIT\n`、`BIT\n`、`CIT\n`、`DIT\n` 并加入索引，随后 `a`、`c` 改为 `git\n`，`b` 改为 1024 字节二进制内容（第 i 字节为 `i % 256`，含 NUL；代替上游 `test-binary-1.png`），`d` 改为该内容重复两次；测试把 `libra diff` 的 stdout 写入仓库根文件 `diff`，得到不带载荷的补丁。
- AC1 → `command::apply_test::apply_numstat_t4012`：执行 `libra apply --numstat < diff`；期望 exit 0、stdout 恰为上述四行（制表符分隔）。
- AC2 → 同一测试同一次执行：执行前后全量快照相同。
- AC3 → 同一测试：新仓库中已提交 `f`（内容 `1\n2\n3\n`），改为 `1\nTWO\nTHREE\n4\n5\n`（修改 2 行、新增 2 行）后把 `libra diff` 的 stdout 写入 `text.diff` 并执行 `libra restore f`，执行 `libra apply --numstat < text.diff`；期望 exit 0、stdout 恰为 `4\t2\tf`。
- AC4 → 同一测试：执行 `libra apply --numstat --apply < text.diff`；期望 exit 0、stdout 首行为 `4\t2\tf`。
- AC5 → 同一测试同一次执行：之后 `f` 字节恰为 `1\nTWO\nTHREE\n4\n5\n`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`apply_numstat_t4012`（通过）
- [x] 共享回归组：`command::apply_test`（21/21）

**Full-suite trigger:** `none`
**Dependencies:** `WC-06`, `DEP-WC-05`（`WC-04R` 经 clean 链与 `WC-06` 传递满足）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 逐文件同步/核对 `docs/commands/apply.md`、`docs/commands/zh-CN/apply.md`、`docs/development/commands/apply.md`、`COMPATIBILITY.md`（在 `apply` 条目行内追加：`--numstat` 对二进制补丁按 Git 输出 `-\t-\t<path>`，上游 `t4012#5` 已验证；只在既有条目行内追加，`DEP-WC-05`）与后端 `apply.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=apply--numstat-t4012#5验证; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/8（AC1–AC5 各 1）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-06,DEP-WC-05; writeset=序列化于 WC-06; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-08: 核对并补全 `libra apply --stat --summary` 二进制输入兼容（#831）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--stat --summary` 的二进制条目按 Git 宽度右对齐；新增 stdin 二进制组合与新建文件 summary/apply 测试；apply 22/22。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）
**Description:** 核对 `apply.rs` 在同时传入 `--stat --summary` 且从 stdin 读取二进制补丁（`t/t4012-diff-binary.sh#2`）时的合并输出格式，修复与 Git 预期输出的任何偏差并补齐回归用例，按 `implementation` 卡完成代码核验/修复、测试与文档同步。
**Out of scope:** `--binary`（`WC-09`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ApplyArgs::summary` 与 `format_apply_summary` 已存在，但缺少针对 `t4012#2` `--stat --summary` stdin 二进制变更的组合断言 | `src/command/apply.rs:258,377-387,768-810` |
| `libra diff --no-index /dev/null <file>` 把 `/dev/null` 当作存在的空文件，输出不含 `new file mode` 的修改补丁，故 AC3 的创建补丁以字面量构造 | `src/command/diff.rs:970-975,1099-1104` |

**Acceptance criteria:**
- [x] AC1：同一 `t4012` fixture 下 `libra apply --stat --summary < diff` 输出恰为 ` a |    2 +-`、` b |  Bin`、` c |    2 +-`、` d |  Bin`、` 4 files changed, 2 insertions(+), 2 deletions(-)` 五行（即上游 `t4012#2` / `TC-1163`）（`apply_test::apply_stat_summary_binary_stdin`）。
- [x] AC2：AC1 的执行不修改工作区与索引（`apply_test::apply_stat_summary_binary_stdin`）。
- [x] AC3：对新建文件 `e` 的补丁，`libra apply --summary --apply` 先输出 ` create mode 100644 e`（`apply_test::apply_stat_summary_binary_stdin`）。
- [x] AC4：AC3 的执行随后应用补丁使 `e` 存在且内容为 post 内容（`apply_test::apply_stat_summary_binary_stdin`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行（无 `.gitattributes`）；fixture 复刻上游 `git@750f5de7` `t/t4012-diff-binary.sh` `prepare repository`：`a`、`b`、`c`、`d` 分别写入 `AIT\n`、`BIT\n`、`CIT\n`、`DIT\n` 并加入索引，随后 `a`、`c` 改为 `git\n`，`b` 改为 1024 字节二进制内容（第 i 字节为 `i % 256`，含 NUL；代替上游 `test-binary-1.png`），`d` 改为该内容重复两次；测试把 `libra diff` 的 stdout 写入仓库根文件 `diff`，得到不带载荷的补丁。
- AC1 → `command::apply_test::apply_stat_summary_binary_stdin`：执行 `libra apply --stat --summary < diff`（stdin 输入）；期望 exit 0、stdout 与上述五行逐字节一致。
- AC2 → 同一测试同一次执行：执行前后全量快照相同。
- AC3 → 同一测试：新仓库（工作区不含 `e`）中以字面量写入创建补丁 `new.diff`，内容恰为 `diff --git a/e b/e`、`new file mode 100644`、`--- /dev/null`、`+++ b/e`、`@@ -0,0 +1 @@`、`+post` 六行（不经 `libra diff --no-index` 生成，原因见 Current evidence 第 2 行），执行 `libra apply --summary --apply < new.diff`；期望 exit 0、stdout 首个摘要行为 ` create mode 100644 e`。
- AC4 → 同一测试同一次执行：之后 `e` 存在且字节恰为 `post\n`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`apply_stat_summary_binary_stdin`（通过）
- [x] 共享回归组：`command::apply_test`（22/22）

**Full-suite trigger:** `none`
**Dependencies:** `WC-07`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 逐文件同步/核对 `docs/commands/apply.md`、`docs/commands/zh-CN/apply.md`、`docs/development/commands/apply.md`、`COMPATIBILITY.md`（在 `apply` 条目行内追加：`--stat --summary` 对无载荷二进制补丁按 Git 输出 `Bin` 行与汇总行，上游 `t4012#2` 已验证；只在既有条目行内追加，`DEP-WC-05`）与后端 `apply.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=apply--stat--summary二进制验证; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=4/8（AC1–AC4 各 1）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-07,DEP-WC-05; writeset=序列化于 WC-07; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-09: 支持 `libra apply --binary` 标志与删后重建端到端验证（#837）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：新增 `--binary` 兼容标志；d cached binary 新建补丁可在新仓逐字节重建；`--check` 带 flag 输出一致且不写入；apply 23/23。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试；`diff --no-index /dev/null` 由 DEFER-WC-05 承接。）
**Description:** 在 `ApplyArgs` 新增 `--binary` 选项，并按 #837 验证二进制新建补丁的删后重建（应用侧：以仓库模式 `libra diff --cached --binary` 生成含 `GIT binary patch` 载荷的新建补丁，在不含该文件的新仓库执行 `libra apply --binary < patch` 并逐字节比对重建的 `binary`；若暴露任何二进制补丁应用缺陷一并在 `apply.rs` 修复），覆盖 `t4012#12` / `TC-1184` 的应用侧；其生成侧 `libra diff --binary --no-index /dev/null binary` 需改 `diff.rs`，由 `DEFER-WC-05` 承接。
**Out of scope:** 修改 `src/command/diff.rs`（`--no-index` 把 `/dev/null` 当作存在的空文件、二进制分支无 `--binary` 载荷，由 `DEFER-WC-05` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ApplyArgs` 缺少 `binary` 字段；`diff.rs` 已存在 `--no-index` 与 `--binary` | `src/command/apply.rs:242-277`; `src/command/diff.rs:290,394-395` |
| `--no-index` 把 `/dev/null` 当作存在的空文件（判为 modified、无 `new file mode`），二进制分支只输出 `Binary files … differ`，不受 `--binary` 影响 | `src/command/diff.rs:970-975,1099-1104` |
| 仓库模式 `--binary` 对二进制文件输出 `GIT binary patch` 的 `literal` 载荷；`apply` 已能解析 `GIT binary patch` | `src/command/diff.rs:3807,3918`；`src/command/apply.rs:1851,1936` |

**Acceptance criteria:**
- [x] AC1：`--binary` 为无操作兼容标志：对同一补丁，`libra apply --binary <patch>` 与 `libra apply <patch>` 的退出码、stdout/stderr 与结果工作区一致（`apply_test::apply_binary_flag_and_creation_roundtrip`）。
- [x] AC2：`--check` 模式下带与不带 `--binary` 均判定补丁可应用（exit 0）且输出一致，并且都不修改工作区与索引（`apply_test::apply_binary_flag_and_creation_roundtrip`）。
- [ ] AC3：`--index` 模式下带与不带 `--binary` 均成功（exit 0），且工作区与索引结果一致并等于补丁的 post 状态。
- [ ] AC4：二进制新建补丁删后重建（上游 `t4012#12` / `TC-1184` 的应用侧）：仓库 A 中 `libra add binary` 后保存 `libra diff --cached --binary` 的补丁，在不含 `binary` 的新仓库 B 中 `libra apply --binary < patch` exit 0 且重建文件与原文件逐字节相同。
- [ ] AC5：无载荷 `Binary files ... differ` 补丁（缺完整 `index <pre>..<post>` 行、目标不匹配 pre、或对象库缺 post）在不带 `--3way` 时仍被拒绝（非零退出、stderr 非空；具体诊断文案由 `WC-09a` 按 Git `apply.c` 对齐，本卡不锁定）。
- [ ] AC6：AC5 的每个失败场景前后工作区与索引快照完全相同（不得部分应用）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置（AC1–AC3）：按「验证执行协议」第 9 条隔离运行；每条 AC 各自新建两个相同 fixture（已提交文本文件 `t.txt`，内容 `old\n`；以字面量写出未应用的补丁 `p.diff`，内容恰为 `diff --git a/t.txt b/t.txt`、`--- a/t.txt`、`+++ b/t.txt`、`@@ -1 +1 @@`、`-old`、`+new` 六行，该补丁的效果是把 `t.txt` 由 `old\n` 改为 `new\n`；`p.diff` 以未跟踪文件写在仓库根，执行被测命令前工作区与索引中的 `t.txt` 保持 `old\n`），两 fixture 只差是否传 `--binary`。
- AC1 → `command::apply_test::apply_binary_flag_and_creation_roundtrip`：两 fixture 分别执行 `libra apply --binary p.diff` 与 `libra apply p.diff`；期望两者 exit 0、stdout/stderr 相同、两侧 `t.txt` 字节均为 `new\n`。
- AC2 → 同一测试（新 fixture 对）：分别执行 `libra apply --check --binary p.diff` 与 `libra apply --check p.diff`；期望两者 exit 0、stdout/stderr 相同；两侧执行前后全量快照相同（`t.txt` 仍为 `old\n`）。
- AC3 → 同一测试（新 fixture 对）：分别执行 `libra apply --index --binary p.diff` 与 `libra apply --index p.diff`；期望两者 exit 0；两侧 `t.txt` 字节均为 `new\n`，`libra ls-files -s t.txt` 的 OID 两侧相同且等于 `printf 'new\n' | libra hash-object --stdin` 的输出。
- AC4 → 同一测试：仓库 A 中 `printf 'Q\n' | tr Q '\000' > binary`、`libra add binary`，执行 `libra diff --cached --binary > patch`；新建仓库 B（`libra init`，工作区不含 `binary`）执行 `libra apply --binary < patch`；期望 apply exit 0，B 中 `binary` 字节恰为 `00 0a`。
- AC5 → 同一测试：辅助仓库 H 提交二进制 `bin`（v1，字节为 `printf 'v1\000\n'` 的输出）后改为 v2（`printf 'v2\000\n'`），把 `libra diff`（缩写 index 行）与 `libra diff --full-index` 的 stdout 分别写入 `short.diff`、`full.diff`（均为无载荷 `Binary files … differ` 补丁）；每个场景使用只提交了 v1 的新目标仓库 T（对象库无 v2）：（i）缺完整 index 行：在 v1 状态应用 `short.diff`；（ii）目标不匹配 pre：把 `bin` 改为 v3（`printf 'v3\000\n'`）并 `libra add bin` 后应用 `full.diff`；（iii）对象库缺 post blob：在 v1 状态应用 `full.diff`；三次均执行 `libra apply <补丁>`（不带 `--3way`）；期望每次非零退出且 stderr 非空（不断言具体文案，避免与 `WC-09a` 冲突）。
- AC6 → 同一测试：AC5 每次执行前后全量快照相同。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [x] 具名测试：`apply_binary_flag_and_creation_roundtrip`（通过；AC1–AC2 与二进制新建重建）
- [x] 共享回归组：`command::apply_test`（23/23）

**Full-suite trigger:** `none`（若修复落入 `am` 共享的 `prepare_patch_opts`（`src/command/am.rs:466`），按 ER-03 改登 `T-1` 并加跑 `source .env.test && cargo nextest run --test compat_mail_am_basic`）
**Dependencies:** `WC-08`, `DEP-WC-05`（`WC-06` 经 `WC-07`、`WC-08` 传递满足）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/apply.md`、`docs/commands/zh-CN/apply.md`、`docs/development/commands/apply.md`、`COMPATIBILITY.md` 与后端 `apply.en.md`（`--binary` 为兼容标志；注明 `libra diff --no-index /dev/null` 尚不生成新建补丁，见 `DEFER-WC-05`；部署核验标记为 `--binary`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=apply--binary选项与删后重建; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8（AC1 1、AC2 2、AC3 1、AC4 2、AC5 1、AC6 1；AC2 的成功且一致与不修改状态、AC4 的 exit 0 与逐字节一致分别计）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-08,DEP-WC-05; writeset=序列化于 WC-08; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-09a: 支持 `libra apply` 直接应用无载荷 full-index 二进制补丁（#838 / `t4108#17`）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 对齐 Git `apply.c:3301-3346`（`git@750f5de7` `apply_binary`）的直接应用语义：修复 `src/command/apply.rs:1283`（`prepare_patch_opts`）在 `headers.binary_marker && headers.binary.is_none()` 时无条件拒绝的问题；当无载荷二进制补丁携带完整 `index <pre>..<post>` OID 时：（1）缺完整 OID 即拒绝（`apply.c:3301-3306`）；（2）当前目标必须匹配 `pre`（`apply.c:3308-3326`），不匹配即拒绝；（3）`post` 为全零时按删除补丁处理，不要求对象库存在 post blob（`apply.c:3328-3332`）；（4）对象库已有 `post` blob 时直接重建（`apply.c:3334-3346`），没有则拒绝。普通应用与 `--3way` 在上述直接路径上行为一致（上游 `t4108#17` 即 pre 匹配、post 已在库的直接重建）；`--3way` 在 pre 不匹配时的三方回退拆出 `WC-09b`（G-09）。所有拒绝都不得修改工作区或索引。该语义位于 `apply` 与 `am` 共享的 `prepare_patch_opts`（`src/command/am.rs:466`），`am` 随之获得与 Git 一致的无载荷 full-index 判定（Git 的 am 同样经 `apply_binary`）；本卡不改 `am.rs` 代码，以 `compat_mail_am_basic` 防退化，`docs/commands/am.md` 只描述带载荷的 `GIT binary patch` 故无需改动。（注：`t4108#17` 等价测试需先预置上游 `#16` 遗留的 post blob。）
**Out of scope:** `--binary` 标志解析（`WC-09`）；`--3way` 在 pre 不匹配时的二进制三方回退（拆出 `WC-09b` 承接）；`is_locked_revision` 修复（`WC-10`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `prepare_patch_opts` 无条件检查 `if headers.binary_marker && headers.binary.is_none()` 并报错，阻断 `--3way --index` full-index 二进制补丁 | `src/command/apply.rs:1283-1290`; 共享调用方 `src/command/am.rs:466`; 上游 `git@750f5de7` `apply.c:3289-3346` |

**Acceptance criteria:**
- [ ] AC1：pre 匹配且 post blob 已在对象库时，`libra apply --index bin.diff`（`diff --full-index` 生成、无载荷）直接重建，工作区与索引均为 post 内容。
- [ ] AC2：同一条件下 `libra apply --3way --index bin.diff` 同样直接重建（即上游 `t4108#17` / `TC-1186`）。
- [ ] AC3：不带 `--3way` 时目标不匹配 pre 的补丁被拒绝，stderr 含 `does not match the current contents`。
- [ ] AC4：删除补丁（post 为全零 OID）在目标匹配 pre 时删除成功，且不要求对象库存在 post blob。
- [ ] AC5：删除补丁在目标不匹配 pre 时被拒绝。
- [ ] AC6：缺完整 `index <pre>..<post>` OID 的补丁被拒绝，stderr 含 `without full index line`。
- [ ] AC7：不带 `--3way` 且对象库缺 post blob 时被拒绝。
- [ ] AC8：AC3、AC5、AC6、AC7 的每次拒绝前后工作区与索引快照完全相同（不得部分应用）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；v1、v2、v3 依次为 `printf 'v1\000\n'`、`printf 'v2\000\n'`、`printf 'v3\000\n'` 的输出（各含 NUL 字节）；提交 v1 的 `bin.png`，改为 v2 后 `libra hash-object -w bin.png` 预置 post blob（等价上游 `#16` 遗留的对象），把 `libra diff --full-index` 的 stdout 写入 `bin.diff`，再 `libra reset --hard`（目标为 `HEAD`，不依赖 `WC-10`）；v3 用作「不匹配 pre」的当前内容；每条拒绝类 AC 前后取全量快照。
- AC1 → `command::apply_test::apply_full_index_binary_without_payload`：执行 `libra apply --index bin.diff`；期望 exit 0、`bin.png` 字节等于 v2、索引条目 OID 等于 post OID。
- AC2 → 同一测试：恢复到 v1 后执行 `libra apply --3way --index bin.diff`；期望同 AC1。
- AC3 → 同一测试：工作区与索引置为 v3 后执行 `libra apply --index bin.diff`；期望非零退出、stderr 含 `does not match the current contents`。
- AC4 → 同一测试：以字面量构造删除补丁 `del.diff`（恰为 `diff --git a/bin.png b/bin.png`、`deleted file mode 100644`、`index <v1 OID>..0000000000000000000000000000000000000000`、`Binary files a/bin.png and /dev/null differ` 四行，无载荷），在新仓库的 v1 状态（对象库中只有 v1）执行 `libra apply --index del.diff`；期望 exit 0、`bin.png` 从工作区与索引移除。
- AC5 → 同一测试：在另一新仓库提交 v3 的 `bin.png`（对象库无 v1）后执行 `libra apply --index del.diff`；期望非零退出。
- AC6 → 同一测试：把 `bin.diff` 的 index 行两侧 OID 各截为前 7 位后执行 `libra apply --index`；期望非零退出、stderr 含 `without full index line`。
- AC7 → 同一测试：在不含 v2 blob 的新仓库 v1 状态执行 `libra apply --index bin.diff`；期望非零退出。
- AC8 → 同一测试：AC3、AC5、AC6、AC7 每次执行前后全量快照相同。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::apply_test::apply_full_index_binary_without_payload)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1–AC8）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::apply_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test compat_mail_am_basic`（按「验证执行协议」第 5 条，过滤为整个 target；`am` 经 `src/command/am.rs:466` 共享 `prepare_patch_opts`；不计卡特有 AC 证据）

**Full-suite trigger:** `T-1: 修改 apply 与 am 共享的 prepare_patch_opts（src/command/am.rs:466 调用）`
**Dependencies:** `WC-09`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/apply.md`、`docs/commands/zh-CN/apply.md`、`docs/development/commands/apply.md`、`COMPATIBILITY.md` 与后端 `apply.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=apply无载荷full-index二进制直接应用; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8（AC1–AC8 各 1）; VER=3/8; landing=1; prod-files=1; scope=M; deps=WC-09,DEP-WC-05; writeset=序列化于 WC-09; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-09b: 支持 `libra apply --3way` 对无载荷 full-index 二进制补丁的三方回退（#838 后续）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `WC-09a` 拆出：对齐 Git `apply --3way` 在二进制补丁直接应用失败（目标不匹配 pre）时的三方回退（`git@750f5de7` `apply.c` `try_threeway`）：对象库同时具备完整 OID 指向的 pre 与 post blob 时，以 pre 为 base、当前内容为 ours、post 为 theirs 执行合并；二进制内容冲突时以非成功结束并留下可检查、可解决的冲突状态（索引写入 stage 1/2/3，工作区保留 ours 版本）；缺 pre 或 post blob 时拒绝且不修改工作区与索引。二进制三方回退只经 `apply` 专用入口启用（在 `src/command/apply.rs` 内新增启用该回退的入口或选项，`prepare_patch_opts` 的默认行为不变），因此 `am`（`src/command/am.rs:466` 调用默认入口）不受影响。
**Out of scope:** `WC-09a` 的直接应用路径；文本补丁的既有三方合并（行为不变）；`libra am -3` 的二进制三方回退（由 `DEFER-WC-04` 承接：`am -3` 在 `three_way` 时向默认入口传入 `resolve_base`（`src/command/am.rs:459-471`），其冲突处理（`src/command/am.rs:483-500`）按文本冲突标记设计，需另立 `am` 卡验证暂停/继续/中止后才能启用；本卡以 AC7 证明默认入口不启用该回退）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 文本补丁的三方回退通过 `prepare_patch_opts` 的 `resolve_base` 参数实现，二进制段未接入 | `src/command/apply.rs:1190-1200,1283-1290` |
| 上游 `--3way` 回退在直接应用失败后读取 pre/post blob 合并 | 上游 `git@750f5de7` `apply.c`（`try_threeway`、`three_way_merge`） |

**Acceptance criteria:**
- [ ] AC1：`--3way`、目标不匹配 pre 且 pre 与 post blob 均在库时执行三方合并；二进制内容冲突时非零退出。
- [ ] AC2：AC1 冲突后索引中该路径恰有 stage 1（pre）、stage 2（ours）、stage 3（post）三条。
- [ ] AC3：AC1 冲突后工作区该文件为 ours 版本。
- [ ] AC4：`--3way` 回退所需的 pre blob 不在对象库时被拒绝。
- [ ] AC5：`--3way` 回退所需的 post blob 不在对象库时被拒绝。
- [ ] AC6：AC4、AC5 的每次拒绝前后工作区与索引快照完全相同。
- [ ] AC7：`prepare_patch_opts` 默认入口（`am` 使用）在传入 `resolve_base` 时仍不对无载荷二进制段执行三方回退，而是以 `WC-09a` 的直接应用拒绝返回错误（`libra am -3` 行为不变）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；v1、v2、v3 依次为 `printf 'v1\000\n'`、`printf 'v2\000\n'`、`printf 'v3\000\n'` 的输出（各含 NUL 字节）；`bin.diff` 为 v1→v2 的 `--full-index` 无载荷补丁（在辅助仓库提交 v1 的 `bin.png`、改为 v2 后把 `libra diff --full-index` 的 stdout 写入，再复制到各场景仓库）；每个场景使用新仓库，并在执行前后取全量快照。
- AC1 → `command::apply_test::apply_threeway_binary_fallback`：仓库提交 v1、以 `libra hash-object -w` 预置 v2，再把工作区与索引置为 v3，执行 `libra apply --3way --index bin.diff`；期望非零退出、stderr 指明 `bin.png` 存在冲突。
- AC2 → 同一测试同一次执行：`libra ls-files -s bin.png` 恰有三行，stage 1/2/3 的 OID 依次等于 v1、v3、v2。
- AC3 → 同一测试同一次执行：`bin.png` 字节等于 v3。
- AC4 → 同一测试：仓库只提交 v3、以 `libra hash-object -w` 预置 v2（对象库无 v1），执行 `libra apply --3way --index bin.diff`；期望非零退出。
- AC5 → 同一测试：仓库提交 v1 后把工作区与索引置为 v3（对象库无 v2），执行 `libra apply --3way --index bin.diff`；期望非零退出。
- AC6 → 同一测试：AC4、AC5 各自执行前后全量快照相同。
- AC7 → `command::apply::tests::default_entry_keeps_binary_threeway_disabled`（库单元测试）：在临时工作目录写入 v3 的 `bin.png`，以 v1→v2 的无载荷 full-index 补丁调用 `prepare_patch_opts`（默认入口）并传入能解析 v1/v2 的 `resolve_base`；期望返回 `Err`，错误为 `PatchPreparationError::DoesNotApply` 或 `PatchPreparationError::Invalid` 且信息含 `does not match the current contents`（不是成功的 `PreparedPatch`，因而不存在冲突记录），调用前后临时目录中 `bin.png` 字节不变；对照：同一输入经 `apply` 专用入口返回带 `bin.png` 冲突记录的 `Ok`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::apply_test::apply_threeway_binary_fallback)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1–AC6）
- [ ] 具名测试：`source .env.test && cargo nextest run --lib -E 'test(=command::apply::tests::default_entry_keeps_binary_threeway_disabled)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC7）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::apply_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test compat_mail_am_basic`（按「验证执行协议」第 5 条，过滤为整个 target；共享 `prepare_patch_opts`；不计卡特有 AC 证据）

**Full-suite trigger:** `T-1: 修改 apply 与 am 共享的 prepare_patch_opts（src/command/am.rs:466 调用）`
**Dependencies:** `WC-09a`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/commands/apply.md`, `docs/commands/zh-CN/apply.md`, `docs/development/commands/apply.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/apply.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/apply.rs`, `tests/command/apply_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/apply.md`、`docs/commands/zh-CN/apply.md`、`docs/development/commands/apply.md`、`COMPATIBILITY.md` 与后端 `apply.en.md`（二进制 `--3way` 冲突状态说明；部署核验标记为 `--3way`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=apply--3way二进制三方回退; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/8（AC1–AC7 各 1）; VER=4/8; landing=1; prod-files=1; scope=M; deps=WC-09a,DEP-WC-05; writeset=序列化于 WC-09a; release=independent; split-from=WC-09a; exception=N/A`

---

### Task WC-10: 修复 `is_locked_revision` 放行普通分支 `main`（#838, #1013）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 `ADR-WC-01` 将 `src/internal/branch.rs` 的 `is_locked_revision` 改为调用 `is_ai_managed_branch`（仅拒绝 `intent`、`traces`、`agent-traces` 及其修订），使 `libra reset main`（mixed，保留工作区脏改动）、`libra reset --hard main` 与 `libra restore --source=main` 正常执行，更新 `internal::branch::tests` 并验证下游 `t4108#17`（`TC-1186`，消费 `WC-09a`）与 `t6415#15`（`TC-1926`）场景。
**Out of scope:** 取消 `libra branch -D/-m main` 的默认分支保护（永久非目标）。AC5/AC6 若失败且可归因于 `merge`/`apply` 而非锁定守卫，按 ER-10 登记 `FIX-*` 并把本卡置 `blocked`，不在本卡扩张修复。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `is_locked_revision` 调用 `is_locked_branch(head)`（含 `DEFAULT_BRANCH`），`reset.rs:914` 与 `restore.rs:485` 据此拒绝 `main` | `src/internal/branch.rs:60-90,1194-1204`; `src/command/reset.rs:914`; `src/command/restore.rs:485` |

**Acceptance criteria:**
- [ ] AC1：当前在分支上时，`libra reset main`（默认 mixed）把 HEAD 与索引更新到 `main`，并保留工作区未暂存修改。
- [ ] AC2：detached HEAD 时 `libra reset main` 结果同 AC1。
- [ ] AC3：当前在分支上时，`libra reset --hard main` 使 HEAD、索引与工作区完全匹配 `main` 树。
- [ ] AC4：detached HEAD 时 `libra reset --hard main` 结果同 AC3。
- [ ] AC5：（仅 Unix：上游前置含符号链接提交）上游 `t6415#15`（`TC-1926`，以 `reset --hard main` 建立前置后的 `merge -s recursive` 冲突保护）等价场景通过。
- [ ] AC6：上游 `t4108#17`（`TC-1186`）以 `reset --hard main` 开头的完整流程通过（消费 `WC-09a`）。
- [ ] AC7：`libra restore --source=main <path>` 成功恢复该路径。
- [ ] AC8：`internal::branch::tests::is_locked_revision_strips_suffixes` 改为断言 `main` 与 `main~1` 不再锁定，而 `traces`、`agent-traces`、`intent` 及其 `~`/`^`/`@` 后缀仍锁定。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；`main` 上 C1 提交 `f.txt`（`one\n`）与 `w.txt`（`w\n`），C2 把 `f.txt` 改为 `two\n`；分支 `topic` 自 C1 另有提交 T1（新增 `t.txt`，内容 `topic\n`）；记录 C2、T1 的 OID；每条 AC 使用新 fixture。
- AC1 → `command::reset_test::reset_main_branch_succeeds`：在 `topic` 上把 `w.txt` 改为 `dirty\n`（不暂存）后执行 `libra reset main`；期望 exit 0、`libra rev-parse HEAD` 等于 C2、`libra ls-files -s` 等于 C2 树、`w.txt` 字节仍为 `dirty\n`。
- AC2 → 同一测试：`libra switch --detach <T1 的 OID>` 后重复；期望同 AC1。
- AC3 → 同一测试：在 `topic` 上执行 `libra reset --hard main`；期望 exit 0、HEAD 等于 C2、索引与工作区跟踪文件字节等于 C2 树，且 T1 新增的 `t.txt` 已从工作区删除。
- AC4 → 同一测试：detached 于 T1 时重复；期望同 AC3。
- AC5 → `command::reset_test::reset_hard_main_t6415_merge_keeps_untracked`（仅 Unix：依赖符号链接，整个测试以 `#[cfg(unix)]` 编译；非 Unix 按「验证执行协议」第 8 条记录不适用）：新仓库按上游 `git@750f5de7` `t/t6415-merge-dir-to-symlink.sh` 依次复刻 `#1`（`:9-21`：空文件 `a/b/c/d`、`a/b-2/c/d`、`a/x` 提交并打标签 `start`，再把 `a/b` 换成指向 `b-2` 的符号链接并提交，`main` 指向该提交）、`#4`（`:42-47`：`libra checkout HEAD^0`、`libra reset --hard main`、`libra checkout start^0`）与 `#5`（`:49-56`：`libra reset --hard`、`a/x` 写入 `x\n` 后 `libra add a/x`、提交并打标签 `baseline`）（`#2`、`#3` 只在游离 HEAD 上产生不影响 `main`、`start`、`baseline` 的提交，故省略），再按 `#15`（`:111-118`）执行 `libra reset --hard`、`libra checkout baseline^0`、创建空文件 `a/b/c/e` 后 `libra merge -s recursive main`；期望 `#4` 中的 `libra reset --hard main` exit 0、merge 非零退出、`a/b/c/e` 与 `a/b-2/c/d` 均为普通文件。
- AC6 → `command::reset_test::reset_hard_main_t4108_binary_threeway`（新仓库，平台无关）：在 `main` 上提交 `base.txt`（内容 `base\n`）后 `libra switch --detach main`（等价上游此前用例留下的游离 HEAD；若停在 `main` 上，`#16` 的提交会移动 `main` 使 `#17` 无可提交内容），按上游 `git@750f5de7` `t/t4108-apply-threeway.sh:288-316`（`#16`、`#17`）在同一仓库逐条执行，`test-binary-1.png`、`test-binary-2.png` 分别以 `printf 'png1\000\n'`、`printf 'png2\000\n'` 的输出代替，上游各 `git` 命令均以对应 `libra` 命令执行，其中 `git diff --binary`、`git diff --full-index` 分别为 `libra diff --binary`、`libra diff --full-index`，stdout 写入 `bin.diff`（前者对二进制修改输出 `GIT binary patch` 载荷，`src/command/diff.rs:3807,3918`；后者输出完整 `index` 行与无载荷 `Binary files … differ`；2026-11-18 以 libra 0.30.207 实测两者形态如此，且 `reset --hard main` 当前以 `LBR-CLI-003` 拒绝）（`#17` 依赖 `#16` 的 `apply --3way --index` 写入对象库的 post blob）；期望两处 `libra reset --hard main` 均 exit 0，两次 `libra apply --3way --index bin.diff` 均 exit 0。
- AC7 → `command::restore_test::restore_source_main_succeeds`：在 `topic` 上执行 `libra restore --source=main f.txt`；期望 exit 0、`f.txt` 字节为 `two\n`（`main` 中的版本）、HEAD 不变。
- AC8 → `internal::branch::tests::is_locked_revision_strips_suffixes`（既有测试修改）：断言 `!is_locked_revision("main")`、`!is_locked_revision("main~1")`，并保留 `traces`、`agent-traces`、`intent`、`traces~1`、`traces^`、`traces^^`、`intent@{0}` 返回 `true`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_main_branch_succeeds)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1–AC4）
- [ ] 具名测试（仅 Unix）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_main_t6415_merge_keeps_untracked)'` (new)（按「验证执行协议」第 1–4 条与第 8 条；覆盖 AC5）
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_main_t4108_binary_threeway)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC6）
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::restore_test::restore_source_main_succeeds)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC7）
- [ ] 具名测试：`source .env.test && cargo nextest run --lib -E 'test(=internal::branch::tests::is_locked_revision_strips_suffixes)'`（按「验证执行协议」第 1–4 条；覆盖 AC8；既有测试按 ADR-WC-01 修改）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::reset_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据；含既有 `test_reset_onto_locked_branch_rejects_{intent,traces,traces_suffix}` 锁定回归）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::restore_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）
- [ ] 共享回归组：`source .env.test && cargo nextest run --lib -E 'test(/^internal::branch::tests::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）

**Full-suite trigger:** `T-1: 改动共享 branch::is_locked_revision 守卫`
**Dependencies:** `WC-09b`, `DEP-WC-01`, `DEP-WC-04`, `DEP-WC-05`（`WC-09a` 经 `WC-09b` 传递满足）
**Deliverables:** `N/A`
**Implementation write set:** `src/internal/branch.rs`, `src/command/reset.rs`, `src/command/restore.rs`, `tests/command/reset_test.rs`, `tests/command/restore_test.rs`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `docs/commands/restore.md`, `docs/commands/zh-CN/restore.md`, `docs/development/commands/restore.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/restore.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/internal/branch.rs`, `src/command/{reset,restore}.rs`, `tests/command/{reset,restore}_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `reset` 与 `restore` 的中英文/开发/网站文档及 `COMPATIBILITY.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=is_locked_revision放行main; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8（AC1–AC8 各 1）; VER=8/8; landing=3; prod-files=3; scope=M; deps=WC-09b,DEP-WC-01,DEP-WC-04,DEP-WC-05; writeset=序列化于 WC-09b; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-11: 修复 `reset --hard` 清理 unmerged 工作区文件（#1012）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 修复 `reset.rs` 中 `tracked_paths_from_index` 仅调用 `index.tracked_entries(0)` 的问题，将当前索引中存在任意 stage（含 stage 1/2/3 unmerged 条目）的路径纳入 `previously_tracked_paths`（保留 `reset.rs:2837-2842` 的 gitlink 排除逻辑 `ADR-MG-01`），使 `reset --hard` 删除目标树中不存在的 unmerged 工作区文件并使后续 `libra checkout side` 正常成功。恢复状态机、no-replace 安装、并发保护与崩溃重开拆出 `WC-11a` 承接（`WC-11a` 再拆出统一 CLI 守卫 `WC-11b` 与锁协议 spike `WC-11s`）；本卡只交付 unmerged 路径识别与正常 reset 行为，不提前声明完整恢复协议。本卡在 `WC-11s` 判定 Go 且其修订取得双 PASS 前不得开工；家族窗口内提交只在本地保留，未经 `WC-11R` 不得推送或发布。
**Out of scope:** `reset --merge` / `reset --keep` 语义。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `tracked_paths_from_index` 仅调用 `index.tracked_entries(0)`，遗漏 stage 1/2/3 unmerged 路径 | `src/command/reset.rs:2198-2202,2835-2850`; 失败回滚只按旧提交恢复见 `src/command/reset.rs:2203-2230,2262-2287` |

**Acceptance criteria:**
- [ ] AC1：modify/delete 冲突（当前分支删除、对方修改）产生 unmerged 条目后执行 `libra reset --hard`，目标树中不存在的该路径工作区文件被删除，不残留为 untracked。
- [ ] AC2：重置后 `libra status --short` 不再列出该路径。
- [ ] AC3：紧接着 `libra checkout side` exit 0 并切换成功。
- [ ] AC4：重置前已存在的纯 untracked 文件保留。
- [ ] AC5：上游 `t6411#18`（`TC-1919`）及 `#19` 起始的 `reset --hard` + `checkout` 序列等价场景通过。
- [ ] AC6：索引记录 gitlink（`160000`）而目标树不含该指针时，`reset --hard` 不删除该路径上的工作区文件（`ADR-MG-01` 排除保持不变，`src/command/reset.rs:2837-2842`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；`main` 提交 `file1`（内容 `base\n`）；分支 `side` 把 `file1` 改为 `side\n`；分支 `deletion` 删除 `file1`；在 `deletion` 上执行 `libra merge side`（期望非零退出，`libra ls-files -s file1` 含 stage 1 与 3）；另建未跟踪 `untracked.txt`（内容 `untracked\n`）。
- AC1 → `command::reset_test::reset_hard_removes_unmerged_worktree_file`：执行 `libra reset --hard`；期望 exit 0；之后 `file1` 不存在、`libra ls-files -s file1` 为空。
- AC2 → 同一测试：执行 `libra status --short`；期望 exit 0、输出恰为 `?? untracked.txt` 一行。
- AC3 → 同一测试：执行 `libra checkout side`；期望 exit 0、`libra symbolic-ref --short HEAD` 为 `side`、`file1` 字节为 `side\n`。
- AC4 → 同一测试：AC1 之后 `untracked.txt` 存在且字节仍为 `untracked\n`。
- AC5 → 同一测试（新仓库）：按上游 `git@750f5de7` `t/t6411-merge-filemode.sh:9-21`（`#1`：空文件 `file1` 提交于 `main`；`a1` 自 `main` 提交空文件 `dummy`；`b1` 自 `main` 以 `libra add --chmod=+x file1` 记录可执行位后提交，平台无关地等价上游 `test_chmod +x`）与 `:80-85`（`#15`：自 `main` 建 `deletion`，`libra rm file1` 后提交）构造分支，再按 `#18`（`:91-95` 的 `do_delete_modechange resolve b1 deletion`）执行 `libra reset --hard`、`libra checkout b1` 与 `libra merge -s resolve deletion`（期望非零退出，且 merge 后、reset 前 `libra ls-files -s file1` 恰为 stage 1 `100644` 与 stage 2 `100755` 两条、`libra status --short` 中 `file1` 为 `UD`，证明确为 delete/modechange 冲突；2026-11-18 以 git 2.54.0 复刻同一序列得到相同索引与 `UD`），随后按 `#19` 的起始步骤执行 `libra reset --hard` 与 `libra checkout deletion`；期望这两条命令 exit 0、`libra status --short` 不含 `file1`。
- AC6 → `command::merge_test::merge_gitlink_hard_reset_keeps_a_file_left_where_the_index_records_a_pointer`（既有测试，原样通过）：索引记录 `vendor` 为 gitlink、`vendor` 位置另有用户文件（内容 `left behind\n`）时执行 `libra reset --hard <不含该指针的提交>`；期望 exit 0、`vendor` 内容仍为 `left behind\n`。
- 文档门 → 「文档与网站验收命令」第 1–4 步（含后端 `--write --only reset`、`typecheck`/`build`、本卡页面的独立 Claude Code `VERDICT: PASS` 与签名+DCO 本地提交；按第 5 步不推送）；推送、聚合评审与第 6 步部署由 `WC-11R` 执行；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_removes_unmerged_worktree_file)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1–AC5）
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::merge_gitlink_hard_reset_keeps_a_file_left_where_the_index_records_a_pointer)'`（既有测试，`tests/command/merge_test.rs:5540`；按「验证执行协议」第 1–4 条；覆盖 AC6）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::reset_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）

**Full-suite trigger:** `N/A（family child；由 WC-11R 统一 T-2）`
**Dependencies:** `WC-11s`（Go 且修订双 PASS；`WC-10` 经 `WC-13`、`WC-11s` 传递满足）, `DEP-WC-05`（`COMPATIBILITY.md` 家族窗口自本卡开工前取得，持有至 `WC-11R`）, `DEP-WC-07`（`src/cli.rs` 独占窗口已授予并记录 `W0`）, `DEP-WC-08`（后端 `cf` 推送冻结窗口已授予）
**Deliverables:** `N/A`
**Implementation write set:** `src/command/reset.rs`, `tests/command/reset_test.rs`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`（本地签名提交、不推送，见「文档与网站验收命令」第 5 步）
**Release write set:** `N/A`
**Files likely touched:** `src/command/reset.rs`, `tests/command/reset_test.rs`, `docs/commands/{reset,zh-CN/reset}.md`, `docs/development/commands/reset.md`, `COMPATIBILITY.md`
**Docs and compatibility impact:** 逐文件同步 `docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md` 与 `COMPATIBILITY.md`（unmerged 路径在 `reset --hard` 下被删除；`COMPATIBILITY.md` 只在 `reset` 条目行内追加，`DEP-WC-05` 家族窗口）；后端 `reset.en.md` 在本卡以同步脚本写入、通过 `typecheck`/`build` 与独立 Claude Code 评审后本地签名提交但不推送（家族规则见「文档与网站验收命令」第 5 步，`DEP-WC-08` 冻结窗口防止提前公开）；`WC-11R` 在 CLI 发布后聚合核对、推送与部署；部署核验标记为 unmerged 清理说明。
**Rollback mode:** `revert`（家族窗口内；发布后由 WC-11R immutable-release 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-WC-RESET`（1/3）
**C/D coverage from:** `WC-11R`
**Granularity:** `type=implementation; axis=reset--hard清理unmerged文件; recovery=家族窗口内revert; complete=yes; self-contained=yes; AC=6/8（AC1–AC6 各 1）; VER=3/8; landing=1; prod-files=1; scope=M; deps=WC-11s,DEP-WC-05,DEP-WC-07,DEP-WC-08; writeset=序列化于 WC-11s; release=family child; split-from=N/A; exception=N/A`

---

### Task WC-11a: `reset --hard` 并发保护与崩溃恢复

**阻断状态（`Lifecycle=blocked`）：** 本卡与 `WC-11b`、`WC-11R` 在 `WC-11s` 判定 Go、且其计划修订取得 Claude（Subagent）与 Codex 对同一字节双 `VERDICT: PASS` 之前保持 `blocked`。阻断原因：仅在命令启动时检查恢复状态不能排除已通过检查的并发写者，单路径 no-replace 与 ref CAS 也不能保护并发索引写入，需要一份覆盖检查、操作与恢复全程且与全部相关写入口共用的跨进程锁协议。候选机制是 `src/internal/operation/middleware.rs:283` `run_with_operation` 的 repository/worktree `ScopeLease`（`src/internal/operation/middleware/lease.rs:83`），但 `classify_command`（`middleware.rs:93`）把 `status` 归为只读、不同写命令分属不同 mutation class，`src/cli.rs:2355` `command_has_existing_operation_boundary` 把 `ReadTree`、`Op`、`Agent`、`File` 等排除在中央 operation 边界之外，中央分发也对只读与 internal worker 跳过 `run_with_operation`。逐入口核实、锁顺序与崩溃释放、外部工具与直接文件写入的检测/拒绝合同、`DEP-WC-05`/`DEP-WC-07` 的对等协调与依赖环检查均由 `WC-11s` 承担；本卡下列写集、AC 与 Verification 为草案，`WC-11s` Go 修订时按 `ADR-WC-02` 补全（含可能需要修改的既有写入口）并重新按 G-03 计数。
**拆分关系:** 自 `WC-11` 拆出；本卡再拆出 `WC-11b`（`reset --recover` 入口与命令守卫）与 `WC-11s`（锁协议与写入口覆盖 spike）。

**Task type:** implementation
**Lifecycle / Acceptance:** blocked / 空
**Description:** 承接 WC-11 的恢复协议：备份优先且安装/恢复均使用原子 no-replace；逐资源持久化 INTENT/BACKED_UP/INSTALLED/COMMITTED/ROLLED_BACK，所有备份、索引、清单及父目录在状态变更前 fsync。复用 `src/internal/operation/store.rs` 的现有 operation-v2 `op_id`、status 与 post-view 记录（不新增 schema 或迁移），UUID 与 ref CAS 在同一 SQLite 事务中持久化，目标等于原 HEAD 也以唯一提交记录判定；接入 `reset.rs::update_reset_reference` 当前的 `with_reflog` 事务，分别覆盖 branch 与 detached HEAD，比较预期旧引用并在同一事务提交 reflog、ref 和 operation 记录，任一失败全部回滚，不能嵌套独立事务。ref CAS 成功但 COMMITTED 未落盘时，以同一事务中唯一 UUID operation 提交记录为提交事实的权威依据；若当前 ref 随后被其他进程移动，不得回滚已提交操作或覆盖后续 ref，保留备份和现场并报告冲突供人工恢复。安装和回滚前均隔离当前内容并验证仍属本次操作，安装后编辑绝不覆盖；已有打开文件描述符在隔离后修改原 inode 时，提交或清理前重新读取隔离 inode 并比对，发现变化则保留隔离备份和当前内容；所有隔离备份在显式人工确认前均不自动删除，避免最终比对后旧 FD 再写入造成丢失。无法提供 no-replace 的平台 fail-closed。`src/internal/worktree_io/mod.rs` 声明 recovery 模块。统一命令守卫和 `reset --recover` 入口由 WC-11b 实现，不在本卡写集内。
**持久化偏序:** 每个资源先原子写入 `INTENT` 清单并 fsync 文件与清单父目录，再执行备份 no-replace rename，fsync 受影响的源/目标父目录及需要保留的数据文件后才持久化 `BACKED_UP`；安装时先持久化安装意图，再写入并 fsync 临时文件、no-replace rename、fsync 目标父目录，最后持久化 `INSTALLED`。索引使用同样的临时文件、数据及父目录屏障；ref、reflog 和 UUID operation 仅通过同一 SQLite 写事务提交。提交成功后才原子持久化并 fsync `COMMITTED` 清单及其父目录；回滚先持久化意图，恢复 no-replace 后同步资源及受影响父目录，再持久化 `ROLLED_BACK`。重开按清单与事务回执幂等重放；每次清单写入、rename、事务提交和各 fsync 前后注入崩溃，不得提前删除尚未人工确认的备份。
**Out of scope:** WC-11 的 unmerged 正常清理、reset --merge/--keep，以及 `reset --recover` 命令入口和未完成恢复时的统一命令守卫（WC-11b）。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有 `reset --hard` 先改索引/工作区再移动 ref，失败时按旧提交回滚，无法恢复不在任何提交中的 unmerged 冲突内容 | `src/command/reset.rs:2198-2230,2262-2287` |
| ref 更新在 `update_reset_reference` 的 `with_reflog` 事务内完成 | `src/command/reset.rs:2296-2311` |
| operation-v2 记录与存储、`worktree_io` 模块入口 | `src/internal/operation/store.rs`；`src/internal/worktree_io/mod.rs` |

**Acceptance criteria:**
- [ ] AC1：no-replace 之外的并发原地写入（以备份的 inode、设备号、链接计数与内容摘要判定）发生时，`reset --hard` 对该资源 fail-closed（不安装、不删除）。
- [ ] AC2：无法安全判定的竞争中，路径新建一方与既有 inode 写入一方的内容都被保留。
- [ ] AC3：目标等于原 HEAD 时正确恢复或仅清理，不丢失任一方内容。
- [ ] AC4：在 `INTENT`/`BACKED_UP`/`INSTALLED`/`COMMITTED`/`ROLLED_BACK` 各持久化点、各 rename、各 fsync 与 SQLite 事务提交前后注入强杀后，重开按 operation UUID、实际 ref 与逐资源清单把每个资源置于确定状态（完整回滚、已提交，或显式标为未核对）。
- [ ] AC5：隔离备份只在按 operation UUID 与路径显式确认后删除（提交、重开与清理均不删除）。
- [ ] AC6：最终比较后旧文件描述符继续写入时，备份与当前内容都被保留。
- [ ] AC7：ref CAS、operation UUID 记录与 reflog 在同一 SQLite 写事务内原子提交（branch 与 detached HEAD 下注入 stale ref、UUID 写入失败或 reflog 写入失败时三者均不提交）。
- [ ] AC8：提交后 ref 被外部移动时，重开依据 UUID 回执按「已提交但 ref 冲突」处理（不回滚、不覆盖后续 ref、保留备份并报告冲突）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：沿用 `WC-11` 的 modify/delete 冲突 fixture；强杀与故障注入点以仅在 debug 构建（`cfg(debug_assertions)`，先例 `src/command/stash.rs:2181`、`src/command/status.rs:1049`）且 `LIBRA_TEST=1` 时生效的测试 failpoint 实现（具体机制随 `ADR-WC-02` 确定）。草案约束（`WC-11s` 定稿时保持）：「重开」指执行不受 `WC-11b` 命令守卫拦截的恢复入口（`WC-11b` 合入后为 `libra reset --recover`）；测试对 ref 或工作区的外部干预（如 AC8 的移动 ref）经同类 debug-only 测试 seam 或库 API 完成，不调用受守卫的 CLI 写命令，使本卡测试在 `WC-11b` 合入后仍可执行。
- AC1 → `command::reset_test::reset_hard_concurrent_edit_and_kill_recovery`：检查后、安装前由测试原地改写待删 unmerged 文件；期望 `libra reset --hard` 非零退出并点名冲突路径，该文件为并发写入后的字节，索引与 HEAD 不变。
- AC2 → 同一测试：分别注入「路径删除后重建」与「既有 inode 原地写入」两种竞争；期望非零退出，两份内容分别留在原路径与隔离备份（备份位置在 stderr 给出）。
- AC3 → `command::reset_test::reset_hard_installed_edit_and_commit_marker_recovery`：以 `libra reset --hard HEAD` 分别在（i）`INSTALLED` 之后、SQLite 事务提交之前与（ii）事务提交之后、`COMMITTED` 落盘之前强杀并重开；期望（i）无 UUID 提交回执，按完整回滚恢复强杀前的冲突工作区（含 `file1`）与 unmerged 索引条目，（ii）存在 UUID 提交回执，按已提交处理，工作区与索引等于 HEAD 树；两种情况隔离备份均保留、无内容丢失。
- AC4 → `command::reset_test::reset_hard_io_failure_rolls_back_index_ref_and_worktree`：对 AC4 列出的每个注入点强杀后重开；期望结果由该注入点相对 SQLite 事务提交的位置唯一决定：提交前（无 UUID 提交回执）为「完整回滚到旧 HEAD/索引/工作区」，提交后（有回执）为「已提交且清单一致」；未核对资源在输出中标明，测试按注入点逐一断言对应的那一种结果。
- AC5 → `command::reset_test::reset_hard_concurrent_edit_and_kill_recovery`：成功提交、重开与清理后列出隔离目录；期望全部备份仍在，未按 UUID 与路径确认前不删除。
- AC6 → `command::reset_test::reset_hard_installed_edit_and_commit_marker_recovery`：最终比较后由测试持有的旧 FD 继续写入；期望备份与当前内容都保留。
- AC7 → 同一测试：branch 与 detached HEAD 分别注入三类失败；期望 `libra rev-parse HEAD`、operation 记录与 `libra reflog` 均与失败前一致。
- AC8 → 同一测试：提交后强杀并由测试移动 ref，再重开；期望后续 ref 保留、备份保留、stderr 报告冲突且退出码非零。
- 文档门 → 「文档与网站验收命令」第 1–4 步（含后端 `--write --only reset`、`typecheck`/`build`、本卡页面的独立 Claude Code `VERDICT: PASS` 与签名+DCO 本地提交；按第 5 步不推送）；推送、聚合评审与第 6 步部署由 `WC-11R` 执行；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_io_failure_rolls_back_index_ref_and_worktree)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC4）
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_concurrent_edit_and_kill_recovery)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1、AC2、AC5）
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_installed_edit_and_commit_marker_recovery)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC3、AC6、AC7、AC8）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::reset_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）

**Full-suite trigger:** N/A（family child，由 WC-11R 统一 T-2）
**Dependencies:** WC-11, WC-11s（Go 且修订双 PASS）, DEP-WC-05（家族窗口自 `WC-11` 开工持有）
**Deliverables:** N/A
**Implementation write set:**（草案，`WC-11s` Go 修订时按 `ADR-WC-02` 补全）`src/command/reset.rs`, `src/internal/worktree_io/mod.rs`（recovery 实现与声明，包括所需工作区辅助逻辑）, `src/internal/operation/store.rs`（提供事务接入，使 operation UUID 记录与 ref CAS 共用调用方持有的 SQLite 写事务，任一步失败整体回滚）, `tests/command/reset_test.rs`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`（本地签名提交、不推送，见「文档与网站验收命令」第 5 步）
**Release write set:** N/A
**Files likely touched:** 同 Implementation write set
**Docs and compatibility impact:** 逐文件同步 `docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md` 与 `COMPATIBILITY.md`（`reset` 条目行内追加，`DEP-WC-05` 家族窗口），只记录 no-replace、并发冲突、崩溃恢复和备份保留；恢复命令入口由 `WC-11b` 记录；后端 `reset.en.md` 在本卡同步写入、验证并本地签名提交（不推送），由 `WC-11R` 推送与部署。
**Rollback mode:** `revert`（家族窗口未推送前撤回本地提交；发布后由 WC-11R immutable-release 前滚）
**Estimated scope:** M
**Version increment:** N/A
**Release boundary:** family child of REL-WC-RESET（2/3）
**C/D coverage from:** WC-11R
**Granularity:** `type=implementation; axis=reset--hard并发保护与崩溃恢复; recovery=家族窗口内revert; complete=yes; self-contained=yes; AC=8/8（AC1–AC8 各 1）; VER=4/8; landing=3; prod-files=3; scope=M; deps=WC-11,WC-11s,DEP-WC-05; writeset=序列化于 WC-11; release=family child; split-from=WC-11; exception=N/A`

---

### Task WC-11b: `reset --recover` 入口与未完成恢复命令守卫

**Task type:** implementation
**Lifecycle / Acceptance:** blocked / 空（见 `WC-11a`「阻断状态」；`WC-11s` Go 修订双 PASS 后回到 `pending`）
**Description:** 自 `WC-11a` 拆出恢复命令入口。提供 `reset --recover --list`、`reset --recover <uuid>` 和 `reset --recover <uuid> --confirm <path>`，并在恢复未完成时由 `src/cli.rs` 拒绝 status 与其他写命令。恢复命令本身必须仍可执行。
**Out of scope:** WC-11a 的并发检测、崩溃恢复和备份保留。
**Current evidence:**

| 事实 | 证据 |
|---|---|
| `reset` 参数与执行入口 | `ResetArgs` 定义于 `src/command/reset.rs:69`；`execute_safe`（`src/command/reset.rs:205`）调用的 `run_reset`（`src/command/reset.rs:904-925`）做锁定检查与模式分派 |
| 中央分发与 operation 边界排除表 | `src/cli.rs:2355`（`command_has_existing_operation_boundary`） |
| 恢复清单由 `WC-11a` 落地（尚不存在） | `WC-11a` 写集 |
**Acceptance criteria:**
- [ ] AC1：`reset --recover --list` 列出每个未完成恢复的 operation UUID、备份路径与状态。
- [ ] AC2：`reset --recover <uuid>` 只恢复该 UUID 的资源，且在命令守卫生效时仍可执行完成。
- [ ] AC3：`reset --recover <uuid> --confirm <path>` 只删除该路径的备份。
- [ ] AC4：不存在的 UUID 的恢复或确认请求被拒绝（非零退出、稳定错误码）。
- [ ] AC5：`--confirm` 只接受该 UUID 清单中的显式文件路径；不属于该 UUID 的路径、缺少路径、目录前缀或通配形式均被拒绝。
- [ ] AC6：AC4、AC5 的每次拒绝都不改动任何备份与清单。
- [ ] AC7：某 UUID 恢复完成后，`--list` 仍列出其未确认备份并可逐个确认。
- [ ] AC8：恢复未完成时，守卫集合内的每个命令（`status` 与全部写命令）都以报告恢复位置（operation UUID 与清单路径）的错误拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。后端页面在本卡写入、验证并本地签名提交（不推送），由 `WC-11R` 推送与部署。
**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：沿用 `WC-11` 的冲突 fixture，借仅在 debug 构建（`cfg(debug_assertions)`）且 `LIBRA_TEST=1` 时生效的 failpoint 在 `INSTALLED` 后强杀，制造恰好 2 个未完成恢复（operation UUID 记为 U1、U2，备份路径按 UUID 区分、互不相同）：U1 由第一次 `libra reset --hard` 产生；由于 U1 未完成时 AC8 的守卫会拒绝第二次 `reset --hard`，U2 由同一类 debug-only 测试 seam 让第二次 `libra reset --hard` 跳过守卫后产生（生产构建不存在该 seam；seam 形式由 `WC-11s` 按 `ADR-WC-02` 定稿，若协议使多个未完成恢复不可能并存，则 `WC-11s` 修订时删去 AC1、AC2、AC5 中的多 UUID 断言并重新计数）。
- AC1 → `command::reset_test::reset_recover_list_confirm_and_reject`：执行 `libra reset --recover --list`；期望 exit 0、输出恰含 U1、U2 两个条目（无其它条目），每个条目的 UUID、备份路径与状态均与测试记录一致（`--json` 时对应字段齐全）。
- AC2 → 同一测试：U1、U2 并存且命令守卫生效时执行 `libra reset --recover U1`；期望 exit 0、仅 U1 的资源恢复，U2 的清单与备份不变。
- AC3 → 同一测试：执行 `libra reset --recover U1 --confirm <U1 的某备份路径>`；期望 exit 0、仅该路径备份被删除。
- AC4 → 同一测试：以不存在的 UUID 分别执行 `libra reset --recover <uuid>` 与 `libra reset --recover <uuid> --confirm <path>`；期望均非零退出、stderr 点名该 UUID 并带稳定错误码。
- AC5 → 同一测试：对 U1 分别以 U2 的备份路径、缺少路径、目录前缀与通配形式执行 `--confirm`；期望均非零退出。
- AC6 → 同一测试：AC4、AC5 每次执行前后对全部备份文件（路径与 SHA-256）与清单取快照；期望前后完全相同。
- AC7 → 同一测试：U1 恢复完成后执行 `--list`；期望仍列出未确认备份，且可逐个 `--confirm` 删除。
- AC8 → 两个具名测试共同覆盖：（a）`cli::tests::reset_recovery_guard_classifies_every_command`（库单元测试）：递归遍历 `Cli::command()` 的全部完整子命令路径（含嵌套与隐藏子命令，如 `op restore`、`worktree add`、`automation` 下各子命令），并为影响读写分类的参数分支（如 `op restore` 的 `--dry-run` 与非 dry-run，`src/cli.rs:2061-2076,2318-2338`）分别建条目，断言每条路径或分支都出现在测试内的显式分类表中（缺失即失败，新增命令、子命令或分支无法绕过），分类为「受守卫」的集合恰为 `ADR-WC-02` 定稿的守卫集合（草案口径：`status` 加全部非只读命令，含 `command_has_existing_operation_boundary`（`src/cli.rs:2355`）排除在中央 operation 边界之外的写入口），且对表中每个条目以真实 argv 经 `Cli::try_parse_from` 解析后调用中央分发前置守卫判定函数（与 `src/cli.rs` 分发入口调用的是同一函数），受守卫条目逐一返回拒绝、`reset --recover` 返回放行；（b）`command::status_test::incomplete_reset_recovery_blocks_writes`：先创建未跟踪文件 `x`（内容 `x\n`），存在未完成恢复时分别执行 `libra status`、`libra add x`、`libra rm x`、`libra clean -f`、`libra restore x`、`libra commit -m x`、`libra checkout side`、`libra reset --hard`，以及嵌套子命令写入口 `libra branch b2` 与 `libra worktree add <仓库外临时路径>`；期望均非零退出、stderr 含 operation UUID 与清单路径，且 HEAD 不变、执行前后全量快照相同。
- 文档门 → 「文档与网站验收命令」第 1–4 步（含后端 `--write --only reset`、`typecheck`/`build`、本卡页面的独立 Claude Code `VERDICT: PASS` 与签名+DCO 本地提交；按第 5 步不推送）；推送、聚合评审与第 6 步部署由 `WC-11R` 执行；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_recover_list_confirm_and_reject)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1–AC7）
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::incomplete_reset_recovery_blocks_writes)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC8 的代表性 CLI 场景）
- [ ] 具名测试：`source .env.test && cargo nextest run --lib -E 'test(=cli::tests::reset_recovery_guard_classifies_every_command)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC8 的全部子命令穷举分类）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::reset_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::status_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）

**Full-suite trigger:** N/A（family child，由 WC-11R 统一 T-2）
**Dependencies:** WC-11a, WC-11s, DEP-WC-05, DEP-WC-07（两窗口均自 `WC-11` 开工前取得并持有至 `WC-11R`；任一未持有即 STOP，不得部分发布）
**Deliverables:** N/A
**Implementation write set:**（草案；若 `WC-11s` 判定需新增稳定错误码，`src/utils/error.rs` 与 `docs/error-codes.md` 由其修订纳入）`src/command/reset.rs`, `src/cli.rs`, `tests/command/reset_test.rs`, `tests/command/status_test.rs`, `docs/commands/reset.md`, `docs/commands/zh-CN/reset.md`, `docs/development/commands/reset.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reset.en.md`（本地签名提交、不推送，见「文档与网站验收命令」第 5 步）
**Release write set:** N/A
**Docs and compatibility impact:** 逐文件（`docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md`、`COMPATIBILITY.md`）记录列表、指定 UUID 恢复、按路径确认删除、失败诊断和未完成恢复时的命令守卫；后端 `reset.en.md` 在本卡同步写入、验证并本地签名提交（不推送），由 `WC-11R` 在 CLI 发布后聚合核对、推送与部署。
**Rollback mode:** `revert`
**Estimated scope:** M
**Version increment:** N/A
**Release boundary:** family child of REL-WC-RESET（3/3）
**C/D coverage from:** WC-11R
**Granularity:** `type=implementation; axis=reset-recover入口与命令守卫; recovery=家族窗口内revert; complete=yes; self-contained=yes; AC=8/8（AC1–AC8 各 1）; VER=5/8; landing=2; prod-files=2; scope=M; deps=WC-11a,WC-11s,DEP-WC-05,DEP-WC-07; writeset=序列化于 WC-11a; release=family child; split-from=WC-11a; exception=N/A`

---

### Task WC-11R: `REL-WC-RESET` unmerged 清理与恢复保护联合发布点

**Task type:** release
**Lifecycle / Acceptance:** blocked / 空（见 `WC-11a`「阻断状态」；`WC-11s` Go 修订双 PASS 后回到 `pending`）
**Description:** 聚合 `WC-11`、`WC-11a` 与 `WC-11b`，禁止先发布扩大删除范围或命令守卫不完整的中间版本。三张子卡仅本地签名提交；本卡统一执行 T-2 全量、`patch + 1`、版本面同步、`Cargo.lock` 工具链刷新、ER-04 C 组（发布步使用 `gh release create "v${VERSION}" --target "$MERGE_SHA" --notes-file <AC3 的 notes 文件>`，notes 文件放仓库外临时目录）与 D 组，并在 CLI 发布后按「文档与网站验收命令」第 5 步聚合核对、推送与部署子卡已本地提交的 `reset` 网站页面。本卡不引入新行为。
**Out of scope:** 任何产品行为改动（由子卡承担）；`WC-11s` 的锁协议设计。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 家族成员、窗口规则与回滚顺序 | 本计划「发布分组与并发窗口」`REL-WC-RESET` |
| 网站页面由同步脚本自 Libra 文档生成 | `/Volumes/Data/gitmono/libra-backend/scripts/README-command-doc-sync.md` |

**Acceptance criteria:**
- [ ] AC1：聚合守卫：`WC-11`、`WC-11a`、`WC-11b` 的全部具名测试在 bump 后的最终提交树上通过。
- [ ] AC2：兼容证据：`COMPATIBILITY.md` 的 `reset` 条目（`#1012` unmerged 清理与 `reset --recover`）与最终行为一致，且文件行数不变。
- [ ] AC3：发布前拟定的 release note（`gh release create --notes-file` 的输入文件）列出 unmerged 清理、恢复入口与命令守卫三项用户可见变化及 `reset --recover` 用法。
- [ ] AC4：发布前核对：`DEP-WC-07` 独占窗口自家族首卡 `WC-11` 开工前取得（`W0` 记录于 `WC-11` 开工时）并持有至今，窗口期内远端 `main` 上没有任何触及 `src/cli.rs` 的提交（逐提交逐父边核对）（家族子卡只本地提交，`src/cli.rs` 改动随本卡发布合入；窗口释放时间在 D 组完成后补记，不作为发布前门）。
- [ ] ER-06/ER-06a 同卡强制门：`reset` 网站页面按「文档与网站验收命令」第 5–6 步聚合推送与部署（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- AC1 → Verification 第 1–8 条（在 bump 后最终提交树上执行，记录提交 SHA 与 `HEAD^{tree}`）。
- AC2 → 「文档与网站验收命令」第 1 步对 `COMPATIBILITY.md` 的逐文件核对（`wc -l` 前后相等）。
- AC3 → Verification 第 9 条手工证据。
- AC4 → Verification 第 10 条手工证据。
- 文档门 → 「文档与网站验收命令」第 5–6 步（`--only reset` 重新同步、家族路径集合核对、聚合评审、推送、部署与释放 `DEP-WC-08`）；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_removes_unmerged_worktree_file)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-11` 新增））
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::merge_gitlink_hard_reset_keeps_a_file_left_where_the_index_records_a_pointer)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-11` 的 gitlink 回归，既有测试））
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_io_failure_rolls_back_index_ref_and_worktree)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-11a` 新增））
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_concurrent_edit_and_kill_recovery)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-11a` 新增））
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_hard_installed_edit_and_commit_marker_recovery)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-11a` 新增））
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::reset_test::reset_recover_list_confirm_and_reject)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-11b` 新增））
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::status_test::incomplete_reset_recovery_blocks_writes)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-11b` 新增））
- [ ] 具名测试：`source .env.test && cargo nextest run --lib -E 'test(=cli::tests::reset_recovery_guard_classifies_every_command)'`（按「验证执行协议」第 1–4 条；覆盖 AC1（`WC-11b` 新增））
- [ ] 手工证据（release note，发布前）：逐项核对 notes 输入文件含 AC3 所列三项变化与 `reset --recover` 用法；发布后的 `gh release view "v${VERSION}"` 正文核对归 D 组（D3）。
- [ ] 手工证据（`DEP-WC-07` 窗口，发布前）：取用 `WC-11` 开工前取得窗口时记录的 `W0=$(libra rev-parse refs/remotes/origin/main)` 与 UTC 时间（`DEP-WC-07`；缺记录即 STOP）；发布前再次 `libra fetch origin` 并记录 `W1=$(libra rev-parse refs/remotes/origin/main)`；要求 `libra merge-base --is-ancestor "$W0" "$W1"` 退出 0；以 `set -euo pipefail` 执行 `WINDOW=$(libra rev-list --parents "$W0..$W1")`（命令替换失败即 STOP，结果只保存在 shell 变量中），对 `$WINDOW` 的每一行 `<提交> <父提交…>` 的每一条父边执行 `libra diff --name-only <父提交> <提交> -- src/cli.rs`（合并提交逐个父边检查），任一父边输出非空即 FAIL 并 STOP（窗口期内本家族不推送，合法结果只有零命中；归因仅用于诊断，不能放行），任一命令失败亦 STOP；端点 `libra diff --name-only "$W0" "$W1" -- src/cli.rs` 为空作为补充核对；不写任何临时文件，提交清单写入本卡记录。

**Full-suite trigger:** T-2
**Dependencies:** WC-11b, DEP-WC-07, DEP-WC-08（`WC-11s` 经 `WC-11b` 传递满足）
**Deliverables:** N/A
**Implementation write set:** `N/A`（不改产品代码与文档内容；后端 `reset.en.md` 由子卡提交，本卡只推送与部署）
**Docs and compatibility impact:** 逐文件：后端 `reset.en.md` 已由子卡本地签名提交，本卡重新同步核对后推送并部署；`docs/commands/reset.md`、`docs/commands/zh-CN/reset.md`、`docs/development/commands/reset.md`、`COMPATIBILITY.md` 由子卡交付、本卡只核对；部署核验标记为 `reset --recover`。
**Release write set:** Inherited
**Rollback mode:** immutable-release
**Estimated scope:** S
**Version increment:** patch
**Release boundary:** family release point of REL-WC-RESET
**C/D coverage from:** self
**Granularity:** `type=release; axis=REL-WC-RESET联合发布; recovery=patch+1前滚修复; complete=yes; self-contained=yes; AC=4/12（AC1–AC4 各 1）; VER=10/12; landing=0; prod-files=0; scope=S; deps=WC-11b,DEP-WC-07,DEP-WC-08; writeset=序列化于 WC-11b; release=family release point; split-from=N/A; exception=N/A`

---

### Task WC-12: 支持 `libra rm --pathspec-from-file=-` 与参数互斥校验（#1038）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `src/command/remove.rs` 中支持 `--pathspec-from-file=-` 从 stdin 读取 pathspec（覆盖 `TC-2675`..`TC-2678`），并补齐 `--pathspec-from-file` 与位置 `<pathspec>` 互斥、`--pathspec-file-nul` 缺少 `--pathspec-from-file` 的错误拒绝及空列表固定报错。
**Out of scope:** `rm -n` 短标志与 SIGPIPE 边界（`WC-13`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `remove.rs` 直接 `std::fs::read(file)` 导致 `"-"` 报错，且未检查 `--pathspec-from-file` 与 `args.pathspec` 互斥；空列表报 `No pathspec was given...` | `src/command/remove.rs:158-200` |

**Acceptance criteria:**
- [ ] AC1：`printf 'a\nb\n' | libra rm --pathspec-from-file=-` exit 0，`a`、`b` 从索引与工作区移除（`TC-2675`、`TC-2676`）。
- [ ] AC2：`printf 'a\0b\0' | libra rm --pathspec-from-file=- --pathspec-file-nul` 同样移除（`TC-2677`）。
- [ ] AC3：`libra rm --pathspec-from-file=list -- a` 非零退出且 stderr 含 `'--pathspec-from-file' and pathspec arguments cannot be used together`（`TC-2678`）。
- [ ] AC4：AC3 的执行不改变全量快照（工作区任何文件均未删除或修改，索引不变）。
- [ ] AC5：`libra rm --pathspec-file-nul a` 非零退出且 stderr 含 `the option '--pathspec-file-nul' requires '--pathspec-from-file'`。
- [ ] AC6：空 pathspec 列表（`printf '' | libra rm --pathspec-from-file=-`）非零退出。
- [ ] AC7：AC6 的 stderr 含固定诊断 `No pathspec was given. Which files should I remove?`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；每条 AC 使用新 fixture：已提交 `a`、`b`、`c`（内容依次为 `a\n`、`b\n`、`c\n`）与列表文件 `list`（内容 `c\n`）；Git 文案依据上游 `git@750f5de7` `builtin/rm.c:284-296`。
- AC1 → `command::remove_test::remove_pathspec_from_file_stdin_and_validation`：以 stdin 输入 `a\nb\n` 执行 `libra rm --pathspec-from-file=-`；期望 exit 0、stdout 含 `rm 'a'` 与 `rm 'b'`；之后工作区相对执行前恰好少了 `a`、`b`，`libra ls-files -s` 相对执行前恰好少了 `a`、`b` 两条（`c`、`list` 的文件与条目不变）。
- AC2 → 同一测试：新 fixture 以 stdin 输入 `a\0b\0` 执行 `libra rm --pathspec-from-file=- --pathspec-file-nul`；期望同 AC1。
- AC3 → 同一测试：执行 `libra rm --pathspec-from-file=list -- a`；期望非零退出、stderr 含上述文案。
- AC4 → 同一测试同一次执行：执行前后全量快照相同（覆盖 `a`、`b`、`c`、`list` 全部文件字节与 `libra ls-files -s` 全部条目）。
- AC5 → 同一测试：执行 `libra rm --pathspec-file-nul a`；期望非零退出、stderr 含上述文案；执行前后全量快照相同。
- AC6 → 同一测试：以空 stdin 执行 `libra rm --pathspec-from-file=-`；期望非零退出；执行前后全量快照相同。
- AC7 → 同一测试同一次执行：stderr 含 `No pathspec was given. Which files should I remove?`。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::remove_test::remove_pathspec_from_file_stdin_and_validation)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1–AC7）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::remove_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）

**Full-suite trigger:** `none`
**Dependencies:** `WC-10`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/remove.rs`, `tests/command/remove_test.rs`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/remove.rs`, `tests/command/remove_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/rm.md`、`docs/commands/zh-CN/rm.md`、`docs/development/commands/rm.md`、`COMPATIBILITY.md` 与后端 `rm.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=rm--pathspec-from-file=-与校验; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8（AC1 2、AC2–AC7 各 1；AC1 的 exit 0 与移除结果分别计）; VER=2/8; landing=1; prod-files=1; scope=M; deps=WC-10,DEP-WC-05; writeset=序列化于 WC-10; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-13: 补齐 `libra rm -n` 短标志与 SIGPIPE/锁边界（#1038 / `TC-2645`）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 为 `RemoveArgs::dry_run` 添加 `short = 'n'` 短标志（修复 `unexpected argument '-n' found`），并在 `remove.rs` 中针对 `output.rs:121-128` 将 `BrokenPipe` 转为 exit 0 且 shell 忽略 SIGPIPE 的场景，在 `#[cfg(unix)]` 下于 `rm -n` 写入遇到 `BrokenPipe` 时恢复默认 SIGPIPE 处置并重新触发 `SIGPIPE`（退出码 141 / 标准信号退出；非 Unix 平台按普通管道写入错误非零退出），同时确保不修改索引或残留 `index.lock`。
**Out of scope:** `--pathspec-from-file=-`（`WC-12`）；全局修改 `src/utils/output.rs`（永久非目标：保留仓库级其它命令 BrokenPipe→0 契约）；在 Windows 实机运行非 Unix 分支（无对应 CI runner，登记为残余风险）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RemoveArgs::dry_run` 仅声明 `#[clap(long)]`，缺少 `short = 'n'`；`output.rs` 将 `BrokenPipe` 转为 exit 0 | `src/command/remove.rs:57-58,429`（`println!` 输出）; `src/utils/output.rs:121-128`; 上游 `git@750f5de7` `t/t3600-rm.sh:261-266` |

**Acceptance criteria:**
- [ ] AC1：`libra rm -n a` 被 CLI 接受（exit 0）。
- [ ] AC2：AC1 的 stdout 为 `rm 'a'`。
- [ ] AC3：AC1 与 AC4 的每次执行前后全量快照相同（不修改索引、不删除或修改工作区文件）。
- [ ] AC4：Unix 下父进程忽略 SIGPIPE 时，`libra rm -n "some-file-*"` 向已关闭管道写入大量输出后以信号 13 终止（shell 观察到 141；上游 `t3600#36` / `TC-2645`）。
- [ ] AC5：AC4 之后仓库存储目录下不残留 `index.lock` 或其它新增锁文件。
- [ ] AC6：BrokenPipe 处置按平台分支：Unix 选择恢复默认处置并重新触发 SIGPIPE，非 Unix 返回普通写入错误（非零退出、可诊断信息）。
- [ ] AC7：`libra rm -n --json a` 输出的 `dry_run` 为 `true`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件均已交付并按「文档与网站验收命令」逐文件验收（不计入 G-03 AC 分子）。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- 共同前置：按「验证执行协议」第 9 条隔离运行；已提交 `a`（内容 `a\n`）；SIGPIPE 场景另行创建并提交 12000 个空文件 `some-file-0`..`some-file-11999`（数量同上游 `git@750f5de7` `t/t3600-rm.sh:241-251` `choke_git_rm_setup`，`rm -n` 输出约 250 KiB，超过管道缓冲）。
- AC1 → `command::remove_test::remove_dry_run_short_flag`：执行 `libra rm -n a`；期望 exit 0、stderr 不含 `unexpected argument '-n'`。
- AC2 → 同一测试同一次执行：stdout 恰为 `rm 'a'`（去除颜色控制序列后）。
- AC3 → `command::remove_test::remove_dry_run_short_flag`（AC1 的执行）与 `command::remove_test::remove_dry_run_sigpipe_keeps_index`（AC4 的执行，仅 Unix；含 12000 个 `some-file-*` 的完整工作区与索引）：各自执行前后全量快照相同；非 Unix 平台只核对 AC1 的执行并按「验证执行协议」第 8 条记录后者不适用。
- AC4 → `command::remove_test::remove_dry_run_sigpipe_keeps_index`（仅 Unix，整个测试以 `#[cfg(unix)]` 编译）：经 `sh -c '((trap "" PIPE && "$LIBRA" rm -n "some-file-*" && echo 0 >&3 || echo $? >&3) | :) 3>&1'` 运行（与上游 `t3600#36` 同形；`LIBRA` 环境变量取 `env!("CARGO_BIN_EXE_libra")`，`sh` 以「验证执行协议」第 9 条的同一隔离变量集加 `LIBRA` 运行）；期望 sh 的 stdout 为 `141`。
- AC5 → 同一测试同一次执行：之后 `.libra/` 下不存在 `index.lock`，且相对执行前无新增 `*.lock` 文件。
- AC6 → `command::remove::tests::broken_pipe_policy_by_platform`（平台无关纯函数单元测试）：对 `(Unix, BrokenPipe)` 断言返回「重新触发 SIGPIPE」，对 `(NonUnix, BrokenPipe)` 断言返回非零退出的普通写入错误且消息含 `failed to write`，对其它 I/O 错误两平台都返回普通错误。
- AC7 → `command::remove_test::remove_dry_run_short_flag`：执行 `libra rm -n --json a`；期望 exit 0、JSON 的 `dry_run` 为 `true`；执行前后全量快照相同。
- 文档门 → 「文档与网站验收命令」第 1–4 步与第 6 步：逐文件记录改动前后 SHA-256、网站同步检查退出码、`typecheck`/`build` 结果、独立 Claude Code `VERDICT: PASS`、提交树哈希比对与部署核验；不计入卡特有 AC 分子。

**Verification:**
- [ ] 具名测试：`source .env.test && cargo nextest run --test command_test -E 'test(=command::remove_test::remove_dry_run_short_flag)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC1、AC2、AC3（AC1 的执行）、AC7）
- [ ] 具名测试（仅 Unix）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::remove_test::remove_dry_run_sigpipe_keeps_index)'` (new)（按「验证执行协议」第 1–4 条与第 8 条；覆盖 AC3（AC4 的执行）、AC4、AC5）
- [ ] 具名测试：`source .env.test && cargo nextest run --lib -E 'test(=command::remove::tests::broken_pipe_policy_by_platform)'` (new)（按「验证执行协议」第 1–4 条；覆盖 AC6）
- [ ] 共享回归组：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::remove_test::/)'`（按「验证执行协议」第 5 条；不计卡特有 AC 证据）

**Full-suite trigger:** `none`
**Dependencies:** `WC-12`, `WC-06a`, `DEP-WC-05`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/remove.rs`, `tests/command/remove_test.rs`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`
**Release write set:** `Inherited`
**Files likely touched:** `src/command/remove.rs`, `tests/command/remove_test.rs`, `docs/...`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/...`
**Docs and compatibility impact:** 同步 `docs/commands/rm.md`、`docs/commands/zh-CN/rm.md`、`docs/development/commands/rm.md`、`COMPATIBILITY.md` 与后端 `rm.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=rm-n短标志与SIGPIPE/lock; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8（AC1–AC5 各 1、AC6 2、AC7 1；AC6 的 Unix 与非 Unix 分支分别计）; VER=4/8; landing=1; prod-files=1; scope=M; deps=WC-12,WC-06a,DEP-WC-05; writeset=序列化于 WC-06a; release=independent; split-from=N/A; exception=N/A`

---

### Task WC-11s: 调查 `reset --hard` 恢复协议的跨进程锁与写入口覆盖（#1012 前置 spike）

**Task type:** `spike`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `WC-11a` 的「阻断状态」拆出（只读调查，**Timebox: 1 人日**，不修改任何生产代码或测试）。待回答的问题：哪一种跨进程锁协议能覆盖 `reset --hard` 恢复协议的检查、操作与恢复全过程，并与全部相关写入口共用。必须逐项核对 `classify_command`（`src/internal/operation/middleware.rs:93`）的分类、`run_with_operation`（`middleware.rs:283`）与 `ScopeLease`（`src/internal/operation/middleware/lease.rs:83`）的作用域与崩溃释放、`command_has_existing_operation_boundary`（`src/cli.rs:2355`）排除的入口与中央分发对只读/internal worker 的跳过路径、`status` 是否写索引、锁顺序、外部工具与直接文件写入的检测/拒绝合同；并把 `DEP-WC-07` 落为最新具名卡清单（或以「中央分发前置守卫 + 枚举全部 `Commands` 变体的穷举测试」取代等待），判定 `WC-11b` 是否需要新增稳定错误码。**Go / No-Go 退出判定：** Go = 在本计划新增 `ADR-WC-02`（锁协议、锁顺序、逐入口覆盖清单、外部写入合同、崩溃释放）并据此修订 `WC-11a`、`WC-11b`、`WC-11R` 的写集、依赖、AC/Verification 与 `DEP-WC-07`，修订稿取得 Claude（Subagent）与 Codex 对同一字节的双 `VERDICT: PASS` 后，三卡由 `blocked` 回到 `pending`，`WC-11` 方可开工；No-Go = 时间箱内无法给出可验证协议，或协议要求超出本计划非目标的改动（SQLite schema、`commit`/`merge` 等命令），则在 `plan-long.md` 登记 `DEFER-WC-03`，把 `WC-11`、`WC-11a`、`WC-11b`、`WC-11R` 的 `Lifecycle / Acceptance` 置为 `blocked / 空` 并在该行注明「按 `DEFER-WC-03` 正式延后」，该 No-Go 修订同样须取得计划文本双 PASS；`#1012` 不宣称关闭。
**Out of scope:** 实现任何锁、恢复或守卫代码（由 `WC-11a`/`WC-11b` 承接）；修改其它计划文件（跨计划协调只在本计划依赖表登记）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `status` 归为 `ReadOnly`，`reset` 为 `RepoMutation`，`add`/`rm`/`restore`/`checkout` 等为 `WorkspaceMutation` | `src/internal/operation/middleware.rs:93-115` |
| operation 作用域租约实现 | `src/internal/operation/middleware/lease.rs:83` |
| 不经中央 operation 边界的命令列表 | `src/cli.rs:2355` |
| 现有 `reset --hard` 失败回滚只按旧提交恢复，无法恢复 unmerged 冲突工作区内容 | `src/command/reset.rs:2203-2230,2262-2287` |

**Acceptance criteria:**
- [ ] AC1：Go/No-Go 结论与依据已写入本计划（Go：`ADR-WC-02`；No-Go：本卡 `Current evidence` 追加结论并引用 `DEFER-WC-03`）。
- [ ] AC2：判定对应的计划修订已完成并取得 Claude（Subagent）与 Codex 同一字节双 `PASS`（Go：`WC-11a`、`WC-11b`、`WC-11R` 与 `DEP-WC-07` 按 `ADR-WC-02` 修订；No-Go：`plan-long.md` 登记 `DEFER-WC-03`，四张家族卡为 `blocked / 空` 并注明按 `DEFER-WC-03` 正式延后）。
- [ ] AC3：覆盖入口清单逐项列出命令或写入口、所持锁与证据锚点（`file:line`），不含「待定」项。

**AC → 断言映射:**（每条 AC 绑定到下列具名测试、守卫或手工证据；执行时按「验证执行协议」第 4 条在本卡记录中逐条写明断言结果）
- AC1 → Verification 第 1 条守卫命中对应产物。
- AC2 → Verification 第 2 条手工证据（两条路径都适用）。
- AC3 → Verification 第 2 条评审时逐项核对 `ADR-WC-02` 覆盖清单（No-Go 路径为本卡 `Current evidence` 的调查清单）。

**Verification:**
- [ ] 守卫（产物存在）：Go 执行 `rg -n '^### ADR-WC-02' docs/development/plan/plan-20261117-worktree-clean-reset-apply.md`，No-Go 执行 `rg -n 'DEFER-WC-03' docs/development/plan/plan-long.md`；要求 rg 退出 0（命中），退出 1 视为产物缺失 FAIL，>1 即 STOP。
- [ ] 手工证据（计划修订核对，Go/No-Go 均适用）：本计划「Codex review log」有覆盖该修订的 Claude（Subagent）与 Codex 两行 `PASS` 且受审 SHA-256 相同；Go 路径另核对 `ADR-WC-02` 覆盖清单逐项含命令/写入口、所持锁与 `file:line`；No-Go 路径另核对本卡 `Current evidence` 的逐入口调查清单同样完整、`plan-long.md` 的 `DEFER-WC-03` 行含重启条件与承接方，且 `rg -n 'DEFER-WC-03' docs/development/plan/plan-20261117-worktree-clean-reset-apply.md` 在四张家族卡的状态行各命中一次（rg 退出 1 即 FAIL，>1 即 STOP）。
- [ ] 守卫（`spike` allowlist，ER-04 B 组）：按「执行检查必备需求」中的「spike 产物 allowlist 守卫」执行，allowlist 为本卡 `Deliverables`（三个计划文件）；不涉及后端仓。

**Full-suite trigger:** `N/A（no-release spike；只改计划文档）`
**Dependencies:** `WC-13`
**Deliverables:** `docs/development/plan/plan-20261117-worktree-clean-reset-apply.md`（Go：`ADR-WC-02` 与家族卡修订；No-Go：结论与延后标记）, `docs/development/plan/plan-long.md`（仅 No-Go 登记 `DEFER-WC-03`）, `docs/development/plan/plan-status.md`（仅按计划状态协议同步本卡状态）
**Implementation write set:** `docs/development/plan/plan-20261117-worktree-clean-reset-apply.md`, `docs/development/plan/plan-long.md`, `docs/development/plan/plan-status.md`
**Release write set:** `N/A`（`no-release` spike）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `N/A`：只修订计划文档，不改变任何用户可见命令行为；`reset` 用户文档随 `WC-11`/`WC-11a`/`WC-11b` 交付。
**Rollback mode:** `revert`（撤回本卡计划修订提交）
**Estimated scope:** `M`（时间箱 1 人日 ≤ 2 人日，G-11 spike 口径）
**Version increment:** `N/A`
**Release boundary:** `no-release`
**C/D coverage from:** Go：`WC-11R`（其 T-2 全量与 D1–D4 在包含本卡计划修订的树上取得）；No-Go：`M3`（计划收口点：在包含本卡产物的最终代码树上运行全量收口门作为 C，并按「完成判据」的收口记账与终止链协议，以收口记账提交的 `base.yml`、`codeql.yml` push run 作为 D1/D2）。按「实施顺序」依赖语义，本卡 `locally-accepted` 即放行 `WC-11`；承载点 C 取得后进入 `remote-pending`，D 全绿后置 `complete`/`done`。
**Granularity:** `type=spike; axis=reset恢复协议锁覆盖调查与Go/No-Go; recovery=revert 计划修订，No-Go 时按 DEFER-WC-03 延后家族; complete=yes; self-contained=yes; AC=3/8（AC1–AC3 各 1）; VER=3/8; landing=0; prod-files=0; scope=M; deps=WC-13; writeset=序列化于 WC-13; release=no-release; split-from=WC-11a; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `is_locked_revision` 判定、配置键大小写级联读取、`clean` 遍历计数、`rm` BrokenPipe 平台策略、`apply` 默认入口不启用二进制三方回退、`reset` 恢复守卫的全部子命令分类 | `source .env.test && cargo nextest run --lib -E 'test(/^internal::branch::tests::/) or test(/^internal::config::tests::/) or test(/^command::clean::tests::/) or test(/^command::remove::tests::/) or test(/^command::apply::tests::/) or test(=cli::tests::reset_recovery_guard_classifies_every_command)'`（共享回归组；尚未落地的模块或测试暂无匹配，不影响其余选中数） |
| 集成 | `clean`、`mv`、`apply`、`reset`、`restore`、`rm` 与 `status` 守卫全场景 | `source .env.test && cargo nextest run --test command_test -E 'test(/^command::clean_test::/) or test(/^command::mv_test::/) or test(/^command::apply_test::/) or test(/^command::reset_test::/) or test(/^command::restore_test::/) or test(/^command::remove_test::/) or test(/^command::status_test::/)'` |
| 兼容 | 与上游 `git@750f5de7`（v2.56）对照的 `t7300` / `t6421` / `t4012` / `t4108` / `t6411` / `t6415` / `t3600` 等价场景；`am` 共享 `prepare_patch_opts` 的回归 | 同「集成」行；另 `source .env.test && cargo nextest run --test compat_mail_am_basic` |
| 迁移 | `N/A`（无 schema 变更） | `N/A` |
| 安全 | 嵌套仓库仅双 force 删除、图层文件不被 `clean` 删除、`clean.requireForce` 非法值 fail-closed、AI 托管分支锁定保留 | `source .env.test && cargo nextest run --test command_test -E 'test(=command::clean_test::clean_single_force_keeps_nested_repos) or test(=command::clean_test::clean_single_force_pathspec_keeps_nested_repos) or test(=command::clean_test::clean_double_force_nested_repo) or test(=command::clean_test::clean_d_keeps_layer_overlay_files) or test(=command::clean_test::clean_require_force_config) or test(/^command::reset_test::test_reset_onto_locked_branch_rejects/)'` |
| 性能 | `clean` 遍历 `O(N)`，每个目录 `read_dir` 一次、无二次全树扫描 | `source .env.test && cargo nextest run --lib -E 'test(=command::clean::tests::untracked_dir_scan_reads_each_directory_once)'` |
| live/gated | `N/A` | `N/A` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| WC-01 | #732; `src/command/clean.rs:297-305`; 上游 `t7300#7` | `src/command/clean.rs` | `clean` EN/zh/dev、`COMPATIBILITY.md`、网站 `clean.en.md` | `command::clean_test::clean_relative_pathspec_prefix`；共享回归组 `command::clean_test::` |
| WC-02 | #733; `src/internal/config.rs:2616`; 上游 `t7300#28` | `src/internal/config.rs` | `clean` EN/zh/dev、`COMPATIBILITY.md`、网站 `clean.en.md` | `command::clean_test::clean_respects_core_excludesfile`、`internal::config::tests::cascaded_fresh_read_matches_variable_case_insensitively`；共享回归组 |
| WC-03 | #734; `src/command/clean.rs:218-222,251-267`; 上游 `t7300#26`、`builtin/clean.c:973-978` | `src/command/clean.rs` | `clean` EN/zh/dev、`COMPATIBILITY.md`、网站 `clean.en.md` | `command::clean_test::clean_does_not_descend_untracked_dirs_without_d`、`command::clean_test::clean_d_keeps_layer_overlay_files`、`command::clean::tests::untracked_dir_scan_reads_each_directory_once`；共享回归组 |
| WC-04 | #735 前置；`src/utils/util.rs:1785-1800`；`src/command/clean.rs:218-249,310-328,372-376`；上游 `t7300#30`、`builtin/clean.c:927` | `src/command/clean.rs` | `clean` EN/zh/dev、`COMPATIBILITY.md`、网站 `clean.en.md` | `command::clean_test::clean_single_force_keeps_nested_repos`；共享回归组 |
| WC-04b | #735 前置；`src/command/clean.rs:281-288,318-323`；上游 `t7300#38`、`builtin/clean.c:927` | `src/command/clean.rs` | `clean` EN/zh/dev、`COMPATIBILITY.md`、网站 `clean.en.md` | `command::clean_test::clean_single_force_pathspec_keeps_nested_repos`；共享回归组 |
| WC-04R | `REL-WC-NEST` | 无产品代码 | 核对子卡文档与 `COMPATIBILITY.md`；推送并部署子卡已提交的网站 `clean.en.md`；release note（`--notes-file`） | 子卡 2 个具名测试（聚合守卫）；release note 手工证据 |
| WC-04a | #735；`src/command/clean.rs:40-41`；上游 `t7300#41`、`builtin/clean.c:959` | `src/command/clean.rs` | `clean` EN/zh/dev、`COMPATIBILITY.md`、网站 `clean.en.md` | `command::clean_test::clean_double_force_nested_repo`；共享回归组 |
| WC-05 | #736; `src/command/clean.rs:183-186`; 上游 `t7300#23`..`#26` | `src/command/clean.rs` | `clean` EN/zh/dev、`COMPATIBILITY.md`、网站 `clean.en.md` | `command::clean_test::clean_require_force_config`；共享回归组 |
| WC-05a | #736; `src/command/clean.rs:451-475`; 上游 `t7300#19`、`#42` | `src/command/clean.rs` | `clean` EN/zh/dev、`COMPATIBILITY.md`、网站 `clean.en.md` | `command::clean_test::clean_exclude_dir_prefix`；共享回归组 |
| WC-06 | #755; `src/command/mv.rs:280-292`; 上游 `t6421` setup | `src/command/mv.rs` | `mv` EN/zh/dev、`COMPATIBILITY.md`、网站 `mv.en.md` | `command::mv_test::mv_rename_directory_to_new_path`；共享回归组 |
| WC-06a | #755（`TC-0733`/`t6421#1`）; `DEP-WC-06` | `tests/command/mv_test.rs`（只读核验，无产品代码） | `mv` EN/zh/dev、`COMPATIBILITY.md`、网站 `mv.en.md`；No-Go 时 `plan-long.md` | Go：`command::mv_test::tc0733_merge_partial_clone_full`；共享回归组 `command::mv_test::`、`command::merge_test::` |
| WC-07 | #812; `src/command/apply.rs:254,748-767`; 上游 `t4012#5` | `src/command/apply.rs` | `apply` EN/zh/dev、`COMPATIBILITY.md`、网站 `apply.en.md` | `command::apply_test::apply_numstat_t4012`；共享回归组 |
| WC-08 | #831; `src/command/apply.rs:258,769-810`; 上游 `t4012#2` | `src/command/apply.rs` | `apply` EN/zh/dev、`COMPATIBILITY.md`、网站 `apply.en.md` | `command::apply_test::apply_stat_summary_binary_stdin`；共享回归组 |
| WC-09 | #837; `src/command/apply.rs:242-277`; 上游 `t4012#12` 应用侧（生成侧 `DEFER-WC-05`） | `src/command/apply.rs` | `apply` EN/zh/dev、`COMPATIBILITY.md`、网站 `apply.en.md` | `command::apply_test::apply_binary_flag_and_creation_roundtrip`；共享回归组 |
| WC-09a | #838（`t4108#17`/`TC-1186`）; `src/command/apply.rs:1283`; `src/command/am.rs:466`; 上游 `apply.c:3301-3346` | `src/command/apply.rs` | `apply` EN/zh/dev、`COMPATIBILITY.md`、网站 `apply.en.md` | `command::apply_test::apply_full_index_binary_without_payload`；共享回归组 `command::apply_test::`、`compat_mail_am_basic` |
| WC-09b | #838 后续；`src/command/apply.rs:1190-1200,1283-1290`；上游 `apply.c` `try_threeway` | `src/command/apply.rs` | `apply` EN/zh/dev、`COMPATIBILITY.md`、网站 `apply.en.md` | `command::apply_test::apply_threeway_binary_fallback`、`command::apply::tests::default_entry_keeps_binary_threeway_disabled`；共享回归组 `command::apply_test::`、`compat_mail_am_basic` |
| WC-10 | #838, #1013; `src/internal/branch.rs:87-90`; 上游 `t6415#15`、`t4108#17` | `src/internal/branch.rs`、`src/command/reset.rs`、`src/command/restore.rs` | `reset`、`restore` EN/zh/dev、`COMPATIBILITY.md`、网站 `reset.en.md`、`restore.en.md` | `command::reset_test::reset_main_branch_succeeds`、`command::reset_test::reset_hard_main_t6415_merge_keeps_untracked`（仅 Unix）、`command::reset_test::reset_hard_main_t4108_binary_threeway`、`command::restore_test::restore_source_main_succeeds`、`internal::branch::tests::is_locked_revision_strips_suffixes`；共享回归组 |
| WC-11 | #1012; `src/command/reset.rs:2835-2850`; 上游 `t6411#18` | `src/command/reset.rs` | `reset` EN/zh/dev、`COMPATIBILITY.md`、网站 `reset.en.md`（本地提交，`WC-11R` 推送） | `command::reset_test::reset_hard_removes_unmerged_worktree_file`、`command::merge_test::merge_gitlink_hard_reset_keeps_a_file_left_where_the_index_records_a_pointer`（既有）；共享回归组 |
| WC-11a | #1012; `src/command/reset.rs:2203-2230,2262-2287` | `src/command/reset.rs`、`src/internal/worktree_io/mod.rs`、`src/internal/operation/store.rs`（草案） | `reset` EN/zh/dev、`COMPATIBILITY.md`、网站 `reset.en.md`（本地提交，`WC-11R` 推送） | `command::reset_test::reset_hard_io_failure_rolls_back_index_ref_and_worktree`、`command::reset_test::reset_hard_concurrent_edit_and_kill_recovery`、`command::reset_test::reset_hard_installed_edit_and_commit_marker_recovery`；共享回归组 |
| WC-11b | #1012; `src/cli.rs:2355` | `src/command/reset.rs`、`src/cli.rs` | `reset` EN/zh/dev、`COMPATIBILITY.md`、网站 `reset.en.md`（本地提交，`WC-11R` 推送） | `command::reset_test::reset_recover_list_confirm_and_reject`、`command::status_test::incomplete_reset_recovery_blocks_writes`、`cli::tests::reset_recovery_guard_classifies_every_command`；共享回归组 |
| WC-11R | `REL-WC-RESET` | 无产品代码 | 核对子卡文档与 `COMPATIBILITY.md`；推送并部署子卡已提交的网站 `reset.en.md` | 子卡全部 8 个具名测试（聚合守卫，含 `WC-11` 引用的既有 gitlink 回归） |
| WC-11s | #1012 前置；`src/internal/operation/middleware.rs:93,283`；`src/cli.rs:2355` | 本计划文件（`ADR-WC-02`） | `N/A`（No-Go 时 `plan-long.md`） | 守卫：`ADR-WC-02` 或 `DEFER-WC-03` 存在；spike allowlist diff |
| WC-12 | #1038; `src/command/remove.rs:158-200`; 上游 `builtin/rm.c:284-296` | `src/command/remove.rs` | `rm` EN/zh/dev、`COMPATIBILITY.md`、网站 `rm.en.md` | `command::remove_test::remove_pathspec_from_file_stdin_and_validation`；共享回归组 |
| WC-13 | #1038; `src/command/remove.rs:57-58,429`; 上游 `t3600#36` | `src/command/remove.rs` | `rm` EN/zh/dev、`COMPATIBILITY.md`、网站 `rm.en.md` | `command::remove_test::remove_dry_run_short_flag`、`command::remove_test::remove_dry_run_sigpipe_keeps_index`（仅 Unix）、`command::remove::tests::broken_pipe_policy_by_platform`；共享回归组 |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 本计划经 Claude（Subagent）与 Codex 双审，二者对同一字节均 `PASS` | 评审记录中两方 `PASS` 行的受审 SHA-256 相同 | `N/A` |
| M1 | Phase 0（`REL-WC-NEST` 经 `WC-04R` 一次发版，其余 `WC-01`..`WC-05a` 各自独立发版）完成 | 对应 patch 版本与 D 组全绿 | 前滚新版本 + 降级指引 |
| M2 | Phase 1 的独立发布卡 `WC-06`、`WC-07`、`WC-08`、`WC-09`、`WC-09a`、`WC-09b` 完成并独立发版；`WC-06a` 不在本里程碑且为 no-release | 对应 patch 版本与 D 组全绿 | 前滚新版本 + 降级指引 |
| M3 | Phase 2（`WC-10`、`WC-12`、`WC-06a`、`WC-13`、`WC-11s`，以及 `WC-11s` Go 时的 `WC-11`、`WC-11a`、`WC-11b`、`WC-11R`）完成且全量收口门通过；收口门在最终代码树上执行，前提按路径分述——Go 路径：Phase 2 全部卡（含随 `WC-11R` 完成的 `WC-11s`）已 `complete`；No-Go 路径：其余 Phase 2 卡已 `complete`、`WC-11s` 为 `locally-accepted`，收口门通过后 `WC-11s` 进入 `remote-pending`，按「完成判据」的收口记账与终止链协议以收口记账提交的 `base.yml`/`codeql.yml` 全绿为 D 后置 `complete`/`done`；两条路径都在此之后判定本里程碑；`WC-06a` 由 `WC-13` 承载 C/D；`WC-11s` No-Go 时 `REL-WC-RESET` 按 `DEFER-WC-03` 延后 | 全量 nextest + fmt + clippy 全绿 | 前滚新版本 + 降级指引 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `clean` 拒绝或部分失败 | 未删除的文件保留 | 返回错误码，不得删除图层覆盖或受保护嵌套库 | 误删已跟踪/被忽略文件 |
| `reset --hard` 中途 I/O 失败 | 恢复成功时无残留；恢复失败时保留快照及明确未恢复资源清单 | 恢复成功则旧 HEAD/ref、索引和工作区完整回滚；恢复失败则返回高严重度错误，报告快照位置与未恢复资源，按清单用快照人工恢复，未核对前不得标一致 | 隐瞒恢复失败、删除快照、宣称已回滚，或明知 HEAD 已移动仍将工作区标为干净 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `WC-02` 配置键大小写修复影响其它配置读取 | 中 | `T-1` 全量门 + `internal::config::tests` 共享回归组 | `WC-02` |
| `WC-10` 与 `branch`/`checkout-switch-restore` 写集协调 | 中 | `DEP-WC-01`、`DEP-WC-04` 顺序隔离 | `WC-10` |
| `WC-09a`、`WC-09b` 改动 `apply` 与 `am` 共享的 `prepare_patch_opts` | 中 | `T-1` 全量门 + `compat_mail_am_basic`；`WC-09a` 的直接应用语义对 `am` 连带生效且与 Git 一致；`WC-09b` 的三方回退只经 `apply` 专用入口启用（`am -3` 由 `DEFER-WC-04` 承接） | `WC-09a`、`WC-09b` |
| `WC-11s` No-Go 导致 `#1012` 延后 | 中 | `DEFER-WC-03` 登记重启条件；家族位于实施链尾，不阻塞其它卡 | `WC-11s` |
| `DEP-WC-07` 需等待 23 张跨计划 `src/cli.rs` 写入卡，等待期长 | 中 | `WC-11s` 评估以中央守卫 + 穷举测试取代等待 | `WC-11s`、`WC-11`、`WC-11b` |
| 家族子卡的后端本地提交在共享 `cf` 上停留到发布点（`WC-04R`、`WC-11R`），期间他人推送会提前公开 | 中 | `DEP-WC-08` 推送冻结窗口；发布点推送前核对家族路径集合；窗口被打破即 STOP | `WC-04`、`WC-04b`、`WC-04R`、`WC-11`、`WC-11a`、`WC-11b`、`WC-11R` |
| `WC-13` 非 Unix 运行期分支无 CI runner 实测 | 低 | 平台策略纯函数单元测试覆盖两分支；由发布者登记为残余风险 | `WC-13` |
| 跨计划：`plan-20261117-plumbing-misc.md#DEP-PM-06` 仍写拆分前的 `PM-21` | 低 | 本计划按 `PM-21a`（`REL-PM-21` 家族）理解；由 plumbing-misc 计划下次修订时对齐（本计划不改动其它计划） | `WC-10` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `clean` workdir 遍历 | `O(N)` | `O(N)` | 每个目录 `read_dir` 一次，不二次全树扫描 | `command::clean::tests::untracked_dir_scan_reads_each_directory_once` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已同步（只在既有条目行内追加，行数不变）。
- [ ] `docs/commands/{clean,mv,apply,reset,restore,rm}.md` 已同步。
- [ ] `docs/commands/zh-CN/{clean,mv,apply,reset,restore,rm}.md` 已同步。
- [ ] `docs/development/commands/{clean,mv,apply,reset,restore,rm}.md` 已同步。
- [ ] 网站 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{clean,mv,apply,reset,restore,rm}.en.md` 已按「文档与网站验收命令」交付。
- [ ] `docs/error-codes.md`：`N/A`——本计划默认不新增稳定错误码（`clean.requireForce` 非法值复用既有 bad boolean 路径）；若 `WC-11s` 判定 `WC-11b` 需新增，则由其修订纳入 `WC-11b` 写集并在此同步。
- [ ] `tests/INDEX.md`：`N/A`——不新增或重命名 `--test` target（新增用例均位于 `command_test` 与库单元测试内）。
- [ ] `Cargo.toml` `[[test]]`：`N/A`——同上。
- [ ] `plan-long.md`：`WC-06a` 能力 No-Go 时登记 `DEFER-WC-02`，`WC-11s` No-Go 时登记 `DEFER-WC-03`；否则 `N/A`。

## Codex review log

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 (Claude) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 6 项 P1（`WC-02` 路径与 `ConfigKv` 根因、`WC-05a` 拆分、`WC-07..13` 上游测试、`restore` 写集、`WC-13` `TC-2645`、`DEP-WC-01`）；R2 关闭 5 项，R3 闭环其余 | P2 已在 R2/R3 修正 | `claude -p --model claude-opus-5-5` R1 |
| R1 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 5 项 P1（`WC-02` 根因、`WC-05a` 拆分、`WC-06/09/10/11` 场景对齐、`restore` 写集、`DEP-WC-01`）；R2 关闭大部，R3 闭环其余 | P2 已在 R2/R3 修正 | `codex exec -m gpt-6.1-terra` R1 |
| R2 (Claude) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 2 项 P1（`DEP-WC-02/03/04` 跨计划写集、`#838` `TC-1186`）；R3 已关闭 | 已在 R3 一并关闭 | `claude -p --model claude-opus-5-5` R2 |
| R2 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 5 项 P1（`WC-09` 删后重建、`WC-10` `TC-1186` 与 mixed/hard、`WC-11` `checkout side`、`DEP-WC-*`）；R3 已关闭 | P2（审计表独立谓词重数与 `WC-10` 对齐）已在 R3/R4 修正 | `codex exec -m gpt-6.1-terra` R2 |
| R3 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 2 项 P1（`WC-01` 无 pathspec 子目录不带 `../`、并发示例 `WC-07` 与 `WC-08` 冲突）；R4 已关闭 | P2（`WC-10` 审计表 `5/8` 与 `R3` 标题）已在 R4/R5 关闭 | `codex exec -m gpt-6.1-terra` R3 |
| R3/R4 (Claude) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 2 项 P1（`WC-10` `TC-1186` 依赖 `apply.rs:1283` 新增 `WC-09a`、`util.rs` 与 `describe.md#DESC-09` 跨计划冲突）；R5 已关闭 | 6 项 P2（`COMPATIBILITY.md` `DEP-WC-05`、AC 谓词重数、R3/R4 修订历史、`WC-03` 目录、`WC-13` SIGPIPE、`TC-*`/`DEFER-WC-02`）已在 R5 关闭 | `claude -p --model claude-opus-5-5` R4 |
| R4 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 无新增 P0/P1 | 1 项 P2（R4 修订历史与标题不一致）已在 R5 关闭 | `codex exec -m gpt-6.1-terra` R4 |
| R5 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 1 项 P1（`WC-09a` 缺失对象失败条件对齐 Git `apply.c:3315-3360`）；R6 已关闭 | 2 项 P2（`M2` 补 `WC-09a`、明确 `../libra-backend` 本地路径）已在 R6 关闭 | `codex exec -m gpt-6.1-terra` R5 |
| R6 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 1 项 P1（网站文档写集改为 `AGENTS.md` 指定的 `/Volumes/Data/gitmono/libra-backend` `cf` 分支）；R7 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R6 |
| R7 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 1 项 P1（`WC-06` `#755` `TC-0733` partial-clone 门控与部分闭环口径，`DEP-WC-06` + `DEFER-WC-02`）；R8 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R7 |
| R8 (Claude) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 2 项 P1（`DEP-WC-02` `util.rs` 补 `BR-26..29`/`CL-04`/`GII-03,12`/`MP-10` 与 `DEP-WC-03` 改指 `GII-05,09`；`WC-10` `AC<=8`）；R9 已关闭 | 7 项 P2（`#755` `WC-06a`、`DEP-WC-05` 逐卡引用、并发示例 `WC-01` 与 `WC-07`、`WC-02` `read_cascaded_fresh_conn_at`、`WC-09a` `t4108#16` 与 `apply.c:3305`、重数与 `describe.md:481`、`WC-07/08` 回退处理）已在 R9 关闭 | `claude -p --model claude-opus-5-5` R8 |
| R8 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 1 项 P1（`#755` 完整 `TC-0733` 需本计划内可执行卡与责任人）；R9 新增 `WC-06a` 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R8 |
| R9 (Claude) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 3 项 P1（`DEP-WC-01` 补 `PM-21`、`DEP-WC-04/06` 细化具体卡号含 `CL-20`/`CSR-21`、`WC-06a` 消费 `CL-20` 契约）；R10 已关闭 | 5 项 P2（`WC-04/05/06/09/09a` 重数、`WC-02` 写集收窄为 `config.rs`、`WC-11` `ADR-MG-01`、`WC-07/08` 歧义、审查日志 `PASS`/`FAIL` 格式）已在 R10 关闭 | `claude -p --model claude-opus-5-5` R9 |
| R9 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 3 项 P1（`WC-06a` 补 `merge_test`、`DEP-WC-06` 写明具体 `MP-*` 卡号、`WC-06` 移除残留 `DEFER-WC-02`）；R10 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R9 |
| R10 (Claude) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 3 项 P1（`WC-06a` 对齐 `t6421#1` 2 次 fetch 2+1 对象契约与 `PM-03`/`CL-13,19,20`、`DEP-WC-04` 补全 12 张 `restore.rs` 卡、`DEP-WC-06` 补全 `merge.rs` 卡或改只读核验）；R11 已关闭 | 9 项 P2（头部/评审结论 `R11`、`WC-03/06a/07/08/11/12` 重数、移除过时 `DEP-WC-02`、`WC-02` `core.attributesFile`、`WC-13` `#[cfg(unix)]`、Phase 1 入口、`tests/command/mod.rs`、追溯表）已在 R11 关闭 | `claude -p --model claude-opus-5-5` R10 |
| R10 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 1 项 P1（`WC-06a` 改为有明确退出判定的 `research` 只读核验卡）；R11 已关闭 | 2 项 P2（`WC-03` 描述与 AC 一致、头部 `R11`）已在 R11 关闭 | `codex exec -m gpt-6.1-terra` R10 |
| R11 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 2 项 P1（`WC-06a` 改为模板合规 `spike` + `no-release` + `C/D coverage from: WC-06` + `Rollback mode: revert`；`成功定义` 明确 `#755` AC3 在 `DEFER-WC-02` 落地前保持显式 `blocked`）；R12 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R11 |
| R12 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 2 项 P1（`WC-06a` `C/D coverage from` 改后续发布卡 `WC-09`、补齐精确 `Deliverables` 白名单与 `git diff` 核验）；R13 已关闭 | 1 项 P2（`WC-06a` 补 0.5d timebox、Go/No-Go 与 `WC-06b`/`DEFER-WC-02` 具名交接）已在 R13 关闭 | `codex exec -m gpt-6.1-terra` R12 |
| R13 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 2 项 P1（`WC-06a` Go/No-Go 双路径可独立完成验收并明确 No-Go 放行 `WC-09`、`.libra` 仓改用 `libra status`/`libra diff`/`libra revert`）；R14 已关闭 | 1 项 P2（评审结论行 `type=research` 更新为 `type=spike`）已在 R14 关闭 | `codex exec -m gpt-6.1-terra` R13 |
| R14 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 2 项 P1（`WC-06a` 缺省 `GIT_TRACE2_PERF` 时的 No-Go 判据与 `plan-long.md` 白名单、后端仓改用 `libra status`/`libra diff`/`libra revert`）；R15 已关闭 | 2 项 P2（`WC-06` 依赖补 `WC-05a` 对齐 Phase 1、头部与 `R10–R15` 修订历史同步）已在 R15 关闭 | `codex exec -m gpt-6.1-terra` R14 |
| R15 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 1 项 P1（`WC-06a` `Deliverables`/写集补入 `plan-status.md` 与本计划文件以兼容状态与结论登记）；R16 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R15 |
| R16 (Codex) | `plan-20261117-worktree-clean-reset-apply.md` | `FAIL` | 2 项 P1（消除 `WC-10 ↔ PM-21` 跨计划死环：`DEP-WC-01` 移除 `PM-21` 并先于 `PM-21` 合入 `WC-10`、将 `WC-06a` 后置至 `WC-12` 与 `WC-13` 之间且 `C/D coverage from: WC-13`；为 `WC-06a` Go/No-Go 双路径补齐可复现 `t6421#1` 场景执行与零命中/失败处理命令） | R17 的范围、结论和原始输出均未核实，不宣称 P1/P2 已关闭；后续 R120+ 重新复审 | `codex exec -m gpt-6.1-terra` R16 |
| R20 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `6926175374cc3092d18ddd8b6b31fc1863480345e4124e9f9bf789cc2431b096` | `FAIL` | 12 项 P1：缺「文档与网站验收命令」、AC 映射机械生成且与 AC 矛盾、nextest 过滤全名错误、`WC-11a/11b/11R` 状态与阻断说明不一致且无承接卡、`WC-11b`/`WC-12` 写集相交无边、未来时间戳、G-03 计数失真、G-09 拆分未登记、D 组未登记、`WC-13` 非 Unix 无可执行命令、`WC-06a` 散文验证与上游未 pin、`WC-11R` 缺必填字段；均在 R20 修订处理，待 R21 复审 | 5 项 P2（卡数/默认覆盖、评审维度、`(new)` 标注与行号漂移、风险表 `DEP-WC-02`、性能守卫归属）已在 R20 修订处理 | 原始输出位于评审工作目录，不入库（v2.12） |
| R20 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `6926175374cc3092d18ddd8b6b31fc1863480345e4124e9f9bf789cc2431b096` | `FAIL` | 12 项 P1：`WC-11a/11b/11R` 应 `blocked`、具名过滤缺 `command::`、模块正则锚点错误、模块名当测试名、库测试缺 `internal::`、协议误用于非测试条目、AC 映射矛盾、`WC-11R` AC 映射错误、引用章节不存在、`WC-12` 与 `WC-11b` 缺顺序边、未来时间戳、粒度计数与审计表不一致、G-09 拆分登记缺失；均在 R20 修订处理，待 R21 复审 | 1 项 P2（卡数与默认覆盖说明）已修 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R21 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `4cad0583baa9e7fc9cb64813af7feaf8b974debf3e05a65e3a855610bde052e6` | `FAIL` | 1 项 P1：`WC-09` AC5 锁定的诊断文案与 `WC-09a` AC 冲突；R20 的 12 项 P1 复核均已关闭 | 4 项 P2（`WC-13` shell 片段、`WC-06a` D4 归属、`WC-09a` 不可观测断言、`WC-10` FIX 分流）已在 R21 修订处理 | 原始输出不入库（v2.12） |
| R21 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `4cad0583baa9e7fc9cb64813af7feaf8b974debf3e05a65e3a855610bde052e6` | `FAIL` | 9 项 P1：家族子卡网站同卡交付、继承 C/D 的完成互等、共享回归组 `skipped` 口径、spike 守卫只比状态行、`WC-06a` 条件验证与承接证明、`WC-11R` 发布后证据前置、`WC-09a` G-03 计数、`WC-07`/`WC-08` 并发示例、`WC-05a` 缺兼容矩阵；均在 R21 修订处理，待 R22 复审 | 1 项 P2（`cf` 分支前置不一致）已修 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R22 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `440d32ab6a330775cbf927c09880277fd65d5bd05a8e97b6bbbae792cd33edbf` | `FAIL` | 1 项 P1：`WC-09b` 称 `am` 不传三方回退开关，与 `src/command/am.rs:459-471`（`three_way` 时传入 `resolve_base`）不符；R21 修订复核其余项已关闭 | 无 | 原始输出不入库（v2.12） |
| R22 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `440d32ab6a330775cbf927c09880277fd65d5bd05a8e97b6bbbae792cd33edbf` | `FAIL` | 6 项 P1：家族子卡网站同卡交付与发布窗口表述矛盾、spike 守卫管道退出码处理、`WC-11s` No-Go 验证与非法状态、`WC-11s` C/D 承载缺 D、`WC-11R` 发布命令未消费核对过的 notes、`WC-06a` 完成时序自相矛盾；均在 R22 修订处理，待 R23 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R23 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `4fbf81581be0a92127c4f46157a4300a3d5fe78222f0ad14c80193cd7f25f980` | `FAIL` | 1 项 P1：并发声明允许整个 `apply` 链与 `clean` 链并发，违反 G-10 | 3 项 P2（M3 Go 路径表述、`WC-11b` 入口锚点、`t6421` setup 锚点）已在 R23 修订处理 | 原始输出不入库（v2.12） |
| R23 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `4fbf81581be0a92127c4f46157a4300a3d5fe78222f0ad14c80193cd7f25f980` | `FAIL` | 5 项 P1：`WC-11R` 部署未重新绑定 `$FINAL`、spike 守卫漏类型/权限/链接且按名称前缀排除、`WC-11s` No-Go 收口循环、`WC-09b` 未验证 `am -3` 二进制冲突、`WC-09b` 缺 pre 场景未构造；均在 R23 修订处理，待 R24 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R24 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `907fc33f4803aa92769377336f72be4dfd297141345e508d51429f8308997ac1` | `PASS` | 无 P0/P1（守卫脚本已抽出实测） | 1 项 P2（`WC-07` writeset 标注 `WC-07..09a`）已在 R24 修订处理 | 原始输出不入库（v2.12） |
| R24 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `907fc33f4803aa92769377336f72be4dfd297141345e508d51429f8308997ac1` | `FAIL` | 5 项 P1：spike 守卫未核完整 status 且遍历/排除不严、`WC-09b` AC7 在现有接口上不可断言、Phase 2 退出条件与 M3 的 No-Go 时序矛盾、`WC-06a` 对象窗口未分别约束、C 组默认遗漏 bump 后 A 组 focused；均在 R24 修订处理，待 R25 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R25 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `ff0aebc3ede856bbfccb291a2041c87b83688db857de151be77fe2d2ef6edf4e` | `PASS` | 无 P0/P1 | 无 | 原始输出不入库（v2.12） |
| R25 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `ff0aebc3ede856bbfccb291a2041c87b83688db857de151be77fe2d2ef6edf4e` | `FAIL` | 3 项 P1：spike 守卫漏仅索引变化、D3 以 Release 附件为判据与 `release.yml` 实际 job 不符、`WC-09b` 文档门误引第 5 步；均在 R25 修订处理，待 R26 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R26 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `9e279b221453b39bc434e5ec994cbbf2a6ae7165c9f2f853f8f2acc7a9e1c9f3` | `PASS` | 无 P0/P1 | 无 | 原始输出不入库（v2.12） |
| R26 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `9e279b221453b39bc434e5ec994cbbf2a6ae7165c9f2f853f8f2acc7a9e1c9f3` | `FAIL` | 3 项 P1：`WC-04` 多路径 AC 低计、D3 缺 CDN 可获取性与摘要证据、`WC-11R` 窗口区间不可定位；均在 R26 修订处理，待 R27 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R27 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `d5aa8a832655827f0f910de8b825eab272059a781def114735ad1e71d7c81066` | `PASS` | 无 P0/P1 | 无 | 原始输出不入库（v2.12） |
| R27 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `d5aa8a832655827f0f910de8b825eab272059a781def114735ad1e71d7c81066` | `FAIL` | 2 项 P1：`WC-03`/`WC-04` 可在显式 pathspec 嵌套仓库保护前独立发布、`WC-11R` 端点 diff 不能证明窗口内无 `src/cli.rs` 提交；均在 R27 修订处理，待 R28 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R28 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `9ccc6b36a92b3e85baa43c28aeff5dfa0ee6762cb23e75d5de8c57ab7ba47f87` | `PASS` | 无 P0/P1 | 无 | 原始输出不入库（v2.12） |
| R28 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `9ccc6b36a92b3e85baa43c28aeff5dfa0ee6762cb23e75d5de8c57ab7ba47f87` | `FAIL` | 1 项 P0：`WC-04` 发布时 `libra clean -f foo/`（与无 `-d` 的默认清理）仍会删除嵌套仓库内文件；1 项 P1：`WC-11R` 窗口命中可经归因放行；均在 R28 修订处理，待 R29 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R29 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `31ccf37fe67e76b0ba13bec2d50e955cf54dfaf79c4edcd615280c0e43fe05a6` | `FAIL` | 1 项 P1：`WC-03` 目录候选整体删除可越过嵌套仓库（父目录场景无断言） | 无 | 原始输出不入库（v2.12） |
| R29 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `31ccf37fe67e76b0ba13bec2d50e955cf54dfaf79c4edcd615280c0e43fe05a6` | `FAIL` | 1 项 P1：嵌套仓库保护未覆盖指向仓库内文件的显式 pathspec，`WC-05` 只断言元数据目录存在；均在 R29 修订处理，待 R30 复审 | 1 项 P2（头部状态滞后）已修 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R30 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `a1f1b8a8e466fb757cd6506b57288ca36569e2071e5174a527ee6668e0161665` | `PASS` | 无 P0/P1 | 无 | 原始输出不入库（v2.12） |
| R30 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `a1f1b8a8e466fb757cd6506b57288ca36569e2071e5174a527ee6668e0161665` | `FAIL` | 1 项 P0：`WC-04` 独立发布时显式 pathspec 仍可删除嵌套仓库内容；1 项 P1：`WC-04b` 仓库内文件 pathspec 只验证目标文件；均在 R30 修订处理，待 R31 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R31 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `b32247bb4b9b56efb2ab1f2ca34b98495d11df84fee241fa8c365c377abb290c` | `PASS` | 无 P0/P1 | 3 项 P2（`WC-04R` 部署核验标记非字面量、`DEP-WC-05` 家族窗口终点未明说、Phase 0 目标漏列嵌套仓库保护）已在 R31 修订处理 | 原始输出不入库（v2.12） |
| R31 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `b32247bb4b9b56efb2ab1f2ca34b98495d11df84fee241fa8c365c377abb290c` | `FAIL` | 3 项 P1：`DEP-WC-05` 要求前序合入 `main` 与家族本地提交互锁、`WC-04R` 缺 release note 验收、`WC-04` `-X` 只断言单个文件；均在 R31 修订处理，待 R32 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R32 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `d29d830c401ef5893c937738b932ddd3ada5a772e001fc25f5eab59dbc586c31` | `FAIL` | 1 项 P1：D3 只为 `WC-11R` 设发布正文核对，`WC-04R` 的 Verification 却把正文核对归 D3；已在 R32 修订处理，待 R33 复审 | 1 项 P2（`WC-04R` AC3 应计 2 个谓词）已在 R32 修订处理 | 原始输出不入库（v2.12） |
| R32 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `d29d830c401ef5893c937738b932ddd3ada5a772e001fc25f5eab59dbc586c31` | `FAIL` | 2 项 P1：`WC-08`/`WC-09` 依赖 `libra diff --no-index /dev/null` 生成新建补丁但不改 `diff.rs`、`WC-04b` fixture 依赖 `WC-04` 正文（G-05）；均在 R32 修订处理，待 R33 复审 | 1 项 P2（D3 未覆盖 `WC-04R` 正文核对）已在 R32 修订处理 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R33 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `325613d7c4d973de36a388371466eb32d18c099a58fe77c7a1b99130848e64bd` | `PASS` | 无 P0/P1 | 2 项 P2（关系表缺 `plan-20261117-diff.md` 承接行、`WC-09` 文档未注明 `DEFER-WC-05`）已在 R33 修订处理 | 原始输出不入库（v2.12） |
| R33 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `325613d7c4d973de36a388371466eb32d18c099a58fe77c7a1b99130848e64bd` | `FAIL` | 4 项 P1：`WC-04` AC1 未比较嵌套 Git 仓库全部文件、`-X` 未覆盖嵌套 Libra 仓库、`WC-09` `--check`/`--index` 等价断言允许两侧同时失败、`DEP-WC-07` 窗口起点矛盾；均在 R33 修订处理，待 R34 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R34 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `c1b7e12be32f12418d1e8cd9e45e8dd25c2d4c8522eb07505c017cec4f850ea3` | `PASS` | 无 P0/P1 | 1 项 P2（`WC-04` AC1 与 AC2 同次执行部分重叠，为上游追溯保留）登记为残余风险 | 原始输出不入库（v2.12） |
| R34 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `c1b7e12be32f12418d1e8cd9e45e8dd25c2d4c8522eb07505c017cec4f850ea3` | `FAIL` | 3 项 P1：`WC-02` 改变公开的 `core.excludesfile` 大小写行为却缺 `COMPATIBILITY.md` 写集与 `DEP-WC-05`、`WC-12` AC4 承诺不删除任何文件却只检查 `a`、`c` 与索引、`WC-02` fixture 未写 `excludes` 规则内容与隔离；均在 R34 修订处理（并做全文同类清扫），待 R35 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R35 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `df7919aae56767717743004de368a063ce66a8bb426a50fc564a8cf94db3b5bd` | `FAIL` | 3 项 P1：`WC-06` AC5 漏上游 `git add .`（未跟踪 `newfile.rs` 无法进入提交）、`WC-10` AC5 映射未执行 `reset --hard main` 且前置步骤不明、`WC-10` AC6 未写明游离 HEAD 导致 `#17` 无可提交内容；均在 R35 修订处理，待 R36 复审 | 4 项 P2（`WC-05` AC3 对未设置键 `config unset`、`WC-11` AC5 可执行位平台相关、`WC-02` `core.attributesFile` 兼容说明位置、适用范围测试模块表述）已在 R35 修订处理 | 原始输出不入库（v2.12） |
| R35 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `df7919aae56767717743004de368a063ce66a8bb426a50fc564a8cf94db3b5bd` | `FAIL` | 3 项 P1：`WC-11b` AC8 承诺全部写命令受守卫但映射只测 4 条命令、全量快照不含权限位、`WC-06a` 产品 No-Go 无法执行场景取得证据；均在 R35 修订处理，待 R36 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R36 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `6d87ced6216ba1111f5e959077c6d7a2b538a3e2f3fedac79d731c18f13ede6e` | `PASS` | 无 P0/P1 | 2 项 P2（`WC-09a` AC5「v3 状态」未写明仓库、`WC-06a` 描述未提产品 No-Go 的 `#[ignore]` 提交）已在 R36 修订处理 | 原始输出不入库（v2.12） |
| R36 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `6d87ced6216ba1111f5e959077c6d7a2b538a3e2f3fedac79d731c18f13ede6e` | `FAIL` | 4 项 P1：`WC-06a` 运行期能力 No-Go 无分流、`WC-10` AC6 在 Libra fixture 内要求 `git diff`、`WC-11b` 分类测试只枚举顶层子命令、`WC-11b` AC1 只断言 U1；均在 R36 修订处理，待 R37 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R37 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `cd99727d9699885aed90017943dd8604306ef70472e3782e855ff3c5b1583bef` | `FAIL` | 1 项 P1：`WC-11a`/`WC-11b` 草案测试依赖未完成恢复期间会被 `WC-11b` 守卫拒绝的 CLI 操作（第二次 `reset --hard`、移动 ref、未定义的「重开」入口）；已在 R37 修订处理，待 R38 复审 | 4 项 P2（`WC-06a` Verification 第 2 条标题与运行期能力 No-Go 分流不一致；描述承诺无对应 AC：`WC-01` 实际清理的 `Removing ../` 前缀、`WC-05` 的 `off` 取值、`WC-11` 的 `ADR-MG-01` gitlink 排除）已在 R37 修订处理 | 原始输出不入库（v2.12） |
| R37 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `cd99727d9699885aed90017943dd8604306ef70472e3782e855ff3c5b1583bef` | `FAIL` | 2 项 P1：`WC-11b` 双未完成恢复 fixture 与命令守卫互相阻断、`WC-13` SIGPIPE 场景未断言不修改索引；均在 R37 修订处理，待 R38 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R38 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `76ef45bf940d8d7829bf6f898929784b107b77c82168968c9b40da5f2314a2bf` | `PASS` | 无 P0/P1 | 2 项 P2（`WC-01` AC5 映射「重建」措辞冗余、`REL-WC-RESET` 草案的 debug-only seam 形式交 `WC-11s` 定稿）登记为残余风险 | 原始输出不入库（v2.12） |
| R38 (Codex gpt-6.1-terra) | 全文，受审 SHA-256 `76ef45bf940d8d7829bf6f898929784b107b77c82168968c9b40da5f2314a2bf` | `FAIL` | 1 项 P1：共享回归组的 `skipped` 只许等于过滤未选中数，与 `WC-06a` No-Go 分支提交 `#[ignore]` 测试后仍运行 `command::mv_test::` 回归组矛盾；已在 R38 修订处理，待 R39 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R39 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `d63dbbf0a93a0753696ce5ca09578d17535e52b7c1ecab1b50c7f19f905c61d9` | `PASS` | 无 P0/P1（Codex 分区评审随后指出 4 项 P1，均已在 R39 修订处理） | 2 项 P2（同 R38）登记为残余风险 | 原始输出不入库（v2.12） |
| R39 (Codex gpt-6.1-terra，分区 A 第 1–821 行) | 分区 A，受审 SHA-256 `d63dbbf0a93a0753696ce5ca09578d17535e52b7c1ecab1b50c7f19f905c61d9` | `FAIL` | 2 项 P1：spike 守卫不能发现被忽略规则排除的新生产文件、网站推送未排除窗口前已有的无关未推送提交与暂存内容；均在 R39 修订处理，待 R40 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R39 (Codex gpt-6.1-terra，分区 B 第 822–1823 行) | 分区 B，受审 SHA-256 `d63dbbf0a93a0753696ce5ca09578d17535e52b7c1ecab1b50c7f19f905c61d9` | `FAIL` | 2 项 P1：`WC-09` 共同前置字面要求先把 `t.txt` 改为 post 内容、完成判据无条件要求已延后的 `WC-11R` 发布；均在 R39 修订处理，待 R40 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R40 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `037b9fe44243bef22031462adc61700f6aa76bafb11add992b37a7e0e0bcc312` | `PASS` | 无 P0/P1（Codex 分区 A 随后指出 1 项 P1，已在 R40 修订处理） | 2 项 P2（`WC-06a` 产品 No-Go 登记的 `WC-06b` 须在登记修订中说明计划内执行或正式延后、`t6421#1` 行号差 1 行）；后者已在 R40 修订处理，前者登记为残余风险 | 原始输出不入库（v2.12） |
| R40 (Codex gpt-6.1-terra，分区 A 第 1–829 行) | 分区 A，受审 SHA-256 `037b9fe44243bef22031462adc61700f6aa76bafb11add992b37a7e0e0bcc312` | `FAIL` | 1 项 P1：spike 生产表面遍历未设 `onerror`，不可读目录被静默跳过；已在 R40 修订处理，待 R41 复审 | 1 项 P2（`WC-01` 等卡的部署核验标记不能证明新内容已部署）已在 R40 修订处理（第 6 步新增标记新颖性要求） | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R40 (Codex gpt-6.1-terra，分区 B 第 830–1834 行) | 分区 B，受审 SHA-256 `037b9fe44243bef22031462adc61700f6aa76bafb11add992b37a7e0e0bcc312` | `FAIL` | 未发现已核实的 P0/P1；因自认逐行审计未完成而不给 PASS，R41 改为更小分区重审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R41 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `16b6a11509a0fed44b2e05b2d424d4d8d73d0c07ed9ef0c1260286cf7318c01b` | `PASS` | 无 P0/P1（Codex 分区随后指出 3 项 P1，均已在 R41 修订处理） | 2 项 P2（第 6 步新颖性检查与 `WC-04R` 标记的大小写匹配方式不同、`WC-06b` 执行或延后须在登记修订中写明）登记为残余风险 | 原始输出不入库（v2.12） |
| R41 (Codex gpt-6.1-terra，P1 第 1–373 行) | 分区 P1，受审 SHA-256 `16b6a11509a0fed44b2e05b2d424d4d8d73d0c07ed9ef0c1260286cf7318c01b` | `FAIL` | 1 项 P1：spike 守卫内容快照不含被忽略的未合并文件；已在 R41 修订处理，待 R42 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R41 (Codex gpt-6.1-terra，P2 第 374–831 行) | 分区 P2，受审 SHA-256 `16b6a11509a0fed44b2e05b2d424d4d8d73d0c07ed9ef0c1260286cf7318c01b` | `PASS` | 无 P0/P1 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R41 (Codex gpt-6.1-terra，P3 第 832–1267 行) | 分区 P3，受审 SHA-256 `16b6a11509a0fed44b2e05b2d424d4d8d73d0c07ed9ef0c1260286cf7318c01b` | `FAIL` | 1 项 P1：`WC-10` 的 Unix 子场景与平台无关场景共用一个具名测试，非 Unix 上可通过却未执行 AC5；已在 R41 修订处理（同类问题一并修正 `WC-13`），待 R42 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R41 (Codex gpt-6.1-terra，P4 第 1268–1839 行) | 分区 P4，受审 SHA-256 `16b6a11509a0fed44b2e05b2d424d4d8d73d0c07ed9ef0c1260286cf7318c01b` | `FAIL` | 1 项 P1：`WC-11` AC5 未断言 merge 确实产生 delete/modechange 冲突；已在 R41 修订处理，待 R42 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R42 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `75893fb203f074cf2964a831675e907bd0319748def4630f3870b441cad4b14c` | `PASS` | 无 P0/P1（Codex P4 分区随后指出 1 项 P1，已在 R42 修订处理） | 2 项 P2（D3 的 `curl -o` 与 `WC-11R` 的 `window.txt` 宜改为进程内处理——已在 R42 修订处理；其余同 R41 登记为残余风险） | 原始输出不入库（v2.12） |
| R42 (Codex gpt-6.1-terra，P1 第 1–375 行) | 分区 P1，受审 SHA-256 `75893fb203f074cf2964a831675e907bd0319748def4630f3870b441cad4b14c` | `PASS` | 无 P0/P1 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R42 (Codex gpt-6.1-terra，P2 第 376–833 行) | 分区 P2，受审 SHA-256 `75893fb203f074cf2964a831675e907bd0319748def4630f3870b441cad4b14c` | `PASS` | 无 P0/P1 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R42 (Codex gpt-6.1-terra，P3 第 834–1270 行) | 分区 P3，受审 SHA-256 `75893fb203f074cf2964a831675e907bd0319748def4630f3870b441cad4b14c` | `PASS` | 无 P0/P1 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R42 (Codex gpt-6.1-terra，P4 第 1271–1848 行) | 分区 P4，受审 SHA-256 `75893fb203f074cf2964a831675e907bd0319748def4630f3870b441cad4b14c` | `FAIL` | 1 项 P1：`WC-11a` AC3 要求安装后、事务提交前强杀重开得到 HEAD 树，与提交前须完整回滚的要求矛盾；已在 R42 修订处理，待 R43 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R43 (Claude subagent claude-opus-5-5) | 差异评审（相对 R42 受审字节），受审 SHA-256 `a0346d31fc6d7fbd1f27e0ea9e35dfeda6734783de73864042106e23d7c779eb` | `PASS` | 无 P0/P1（Codex P1 分区随后指出 D3 改写的 2 项 P1，已在 R43 修订回退处理） | 1 项 P2（D3「非 200」措辞与管道判定不一致）随回退消失；其余同 R41 登记为残余风险 | 原始输出不入库（v2.12） |
| R43 (Codex gpt-6.1-terra，P1 第 1–376 行，差异评审) | 分区 P1，受审 SHA-256 `a0346d31fc6d7fbd1f27e0ea9e35dfeda6734783de73864042106e23d7c779eb` | `FAIL` | 2 项 P1：D3 改写后的管道无法断言 HTTP 200、无法完成 manifest 验签与条目解析；已在 R43 修订回退为 R42 原文，待 R44 复审 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R43 (Codex gpt-6.1-terra，P2 第 377–834 行，差异确认) | 分区 P2，受审 SHA-256 `a0346d31fc6d7fbd1f27e0ea9e35dfeda6734783de73864042106e23d7c779eb` | `PASS` | 无 P0/P1（分区内容相对 R42 逐字未变） | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R43 (Codex gpt-6.1-terra，P3 第 835–1271 行，差异确认) | 分区 P3，受审 SHA-256 `a0346d31fc6d7fbd1f27e0ea9e35dfeda6734783de73864042106e23d7c779eb` | `PASS` | 无 P0/P1（分区内容相对 R42 逐字未变） | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R43 (Codex gpt-6.1-terra，P4 第 1272–1854 行) | 分区 P4，受审 SHA-256 `a0346d31fc6d7fbd1f27e0ea9e35dfeda6734783de73864042106e23d7c779eb` | `PASS` | 无 P0/P1 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R44 (Claude subagent claude-opus-5-5) | 全文（相对 R43 只核差异，其余字节此前已逐轮评审），受审 SHA-256 `a710c78a48098c5f123faaadd8dc280c201ffbf229e9de17536917ce4b2a92e5` | `PASS` | 无 P0/P1 | 残余 P2 见各轮 Claude 行（D3 `<file>` 位置未写明、`WC-04R` 标记大小写、`WC-06b` 执行或延后、`WC-01` AC5 措辞、seam 形式交 `WC-11s`） | 原始输出不入库（v2.12） |
| R44 (Codex gpt-6.1-terra，P1 第 1–377 行，差异确认) | 分区 P1，受审 SHA-256 `a710c78a48098c5f123faaadd8dc280c201ffbf229e9de17536917ce4b2a92e5` | `PASS` | 无 P0/P1 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R44 (Codex gpt-6.1-terra，P2 第 378–835 行，差异确认) | 分区 P2，受审 SHA-256 `a710c78a48098c5f123faaadd8dc280c201ffbf229e9de17536917ce4b2a92e5` | `PASS` | 无 P0/P1 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R44 (Codex gpt-6.1-terra，P3 第 836–1272 行，差异确认) | 分区 P3，受审 SHA-256 `a710c78a48098c5f123faaadd8dc280c201ffbf229e9de17536917ce4b2a92e5` | `PASS` | 无 P0/P1 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |
| R44 (Codex gpt-6.1-terra，P4 第 1273–1860 行，差异确认) | 分区 P4，受审 SHA-256 `a710c78a48098c5f123faaadd8dc280c201ffbf229e9de17536917ce4b2a92e5` | `PASS` | 无 P0/P1 | 无 | `codex exec -m gpt-6.1-terra`；原始输出不入库 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| `DEFER-WC-01` | 解除 `intent`/`traces` 等 AI 分支的 `reset` 锁定 | 保护 AI 会话历史完整性 | 另有治理 ADR 变更时 | `COMPATIBILITY.md` |
| `DEFER-WC-02` | `GIT_TRACE2_PERF` 性能事件输出（若 `WC-06a` 核验 `t6421#1` 需要 `fetch_count` 原生 PERF 行） | `CS-24` 仅实现 `GIT_TRACE2_EVENT`，全仓尚无 `GIT_TRACE2_PERF` | `trace2` PERF 专项计划立项时 | `plan-long.md` |
| `DEFER-WC-04` | `libra am -3` 对无载荷 full-index 二进制补丁的三方回退（`WC-09b` 只在 `apply` 专用入口启用） | `am` 冲突暂停/继续/中止按文本冲突标记设计（`src/command/am.rs:483-500`），启用前需另立 `am` 卡验证，超出本计划非目标 | `am` 相关计划立项时 | `plan-long.md`（由承接计划登记；本计划只在此列出） |
| `DEFER-WC-05` | `libra diff --no-index` 把 `/dev/null` 视为缺失一侧（输出 `new file mode`/`deleted file mode` 与 `/dev/null` 头）并在 `--binary` 下输出 `GIT binary patch` 载荷，使上游 `t4012#12`（`TC-1184`）生成侧 `diff --binary --no-index /dev/null binary` 可用；`WC-09` 只交付应用侧 | 需改 `src/command/diff.rs:919-1106`（`execute_no_index_safe`、`collect_no_index_pairs`），与 `plan-20261117-diff.md` 的 `--no-index` 卡（`DIFF-41..44`）共享写集，超出本计划 `apply` 范围 | `plan-20261117-diff.md` 的 `DIFF-41..44` 合入后另立 `diff --no-index` 卡 | `plan-long.md`（由承接计划登记；本计划只在此列出）；落地前 `#837` 不宣称全量闭环 |
| `DEFER-WC-03` | `#1012` `reset --hard` unmerged 清理及其恢复协议（`WC-11`、`WC-11a`、`WC-11b`、`WC-11R`）；仅在 `WC-11s` 判定 No-Go 时启用 | 跨进程锁协议无法在时间箱内给出可验证方案，或需超出本计划非目标的改动 | 跨进程锁协议另立 ADR/计划落地后 | `plan-long.md` |

## 完成判据

- [ ] 全量收口门须在**最终代码树**上执行，并记录 `GATE=$(libra rev-parse HEAD)`、`libra rev-parse "$GATE^{tree}"` 及结果。「代码树」指除三份计划记账文件（本计划、`docs/development/plan/plan-status.md`、`docs/development/plan/plan-long.md`）外的全部路径：此后只改这三份文件的记账提交不使收口门失效（以 `libra diff --name-only "$GATE" HEAD` 只含这三份文件为判据），其它任何路径变化即须在新树上重跑，与是否发生修复无关。
- [ ] **收口记账与终止链（预登记，模板 v2.14）：** 收口门结果与各卡最终状态写入记账提交并推送 `main`；该记账提交的 `base.yml`/`codeql.yml` push run 即收口门所测代码树的远端 D 证据（其代码树与 `GATE` 相同）。随后把 `WC-11s`（No-Go 路径）等继承卡置 `complete`/`done` 的最终记账提交为终止链末端：其自身 D 结果不在该提交内登记，而由下一次 `plan-status.md` 例行同步补记 SHA、run ID、结论与观察 UTC 时间（无例行同步时以一笔明确的计划状态同步提交补记，不重启终止链）。
- [ ] 计划文本提交门：Claude（Subagent）与 Codex 对同一受审字节均返回字面结论 `VERDICT: PASS` 且 P0/P1 全部关闭；网站交付另需独立 Claude Code `VERDICT: PASS`，两者互不替代。
- [ ] 所有任务卡满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）；非延后卡不得以 `blocked` 通过完成门，按 `DEFER-*` 正式延后（状态行注明 `DEFER` 编号）的卡除外。
- [ ] `WC-06a`、`WC-11s` 的 Go/No-Go 判定与承接登记一致；No-Go 时对应 `DEFER-WC-02` / `WC-06b` / `DEFER-WC-03` 已登记，被延后的卡不计入「非延后任务」。
- [ ] 所有任务全部适用分支的 Verification 已运行并记录结果；条件分支（如 `WC-06a` No-Go 时的 Go 具名测试、`WC-11s` Go/No-Go 二选一的守卫）记录不适用依据，不得记为通过。
- [ ] **计划完成门 / 全量收口门（ER-13）**：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿。
- [ ] **全量收口门暴露的 Bug 已全部处理（ER-13）**。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成。
- [ ] 必要的 migration、rollback、failure-recovery 验证已完成；每张卡的 `Rollback mode` 都已被实际验证或记录为不可验证的原因。
- [ ] 计划评审最终结论为 Claude（Subagent）与 Codex 双 `PASS`（同一受审字节），P0/P1 全部关闭（按用户 2026-11-18 指令）。
- [ ] 每张非延后的独立发布卡及发布点 `WC-04R` 已按 ER-08 完成 `patch + 1`、版本面同步、`Cargo.lock` 工具链刷新、提交、推送及 `gh release create v<version>`；发布点 `WC-11R` 仅在 `WC-11s` Go 路径适用并同样完成上述发布，No-Go 路径按 `DEFER-WC-03` 正式延后并记录不适用依据。
- [ ] 网站最终字节均取得独立 Claude Code PASS，且 cf 提交树与已审字节一致；计划文本 Claude 与 Codex 双审不替代该网站交付门。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md` 相关状态或日期计划索引已同步，或明确 `N/A`。
