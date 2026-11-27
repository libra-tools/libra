# Merge 与 Pull Git 兼容性修复计划（2026-11-17）

**模板版本:** `v2.16`。本计划不在 v2.16 具名 `code-only` 范围内，按普通发布合同执行：全部 53 张任务卡均为 `Task type=implementation`、`Release boundary=independent` 的独立发布切片，各自执行完整 C 组（含 `patch + 1`、`compat_version_surface_sync` 三处版本面同步、工具链刷新 `Cargo.lock`、`gh release create`）与适用 D 组；本计划不设 `REL-*` 家族或合并发布。

**状态:** Claude R30 与 Codex R30 双审 PASS（受审 SHA-256 前 12 位 `a20987cf5fa5`），计划可提交；全部任务卡 `Lifecycle=pending`、`Acceptance` 为空，尚未执行。本文只规划，不宣称实现完成；落地时每张卡必须先按 ER-02 刷新源码锚点。

**审阅合同:** 按用户 2026-11-18 指令，本计划的计划文本提交门为 Claude（Subagent）与 Codex 双审，二者对同一受审字节（同一 SHA-256）均给出 `VERDICT: PASS` 方可提交（取代 2026-11-17「Codex 单审、不再要求 Claude Review」口径，见「修订历史」）。执行期各任务卡的 ER-05 代码评审门与网站交付的独立 Claude Code `VERDICT: PASS` 门照常适用，与计划文本提交门互不替代。

## 文档职责

- **网站交付路径（权威）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支），依据是用户 2026-11-18 11:55:09 UTC 确认：本批计划的网站文档交付检出为该路径（登记为 `DEP-MP-12`，已满足）；该路径也是模板 ER-06a 默认的 Libra checkout 同级 `../libra-backend`（`plan-template.md:332`）。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`，该路径在本机不存在（2026-11-18 核对），属已知文档滞后；用户决定本批不改 `AGENTS.md`，本计划以上述确认为准，不把网站路径归因于 `AGENTS.md`。修订历史 2026-11-18 08:19:08 行所称「用户最新指令」不作为依据，以本条确认为准。2026-11-18 核对：`/Volumes/Data/gitmono/libra-backend` 为 Libra 检出（有 `.libra/`、无 `.git`），分支 `cf`。写入前须按「文档与网站验收命令」第 2 步核验该路径存在、仅含单一 VCS 元数据且分支为 `cf`（不在 `cf` 时按 ER-06a 先 `libra switch cf`，切换受阻即 STOP）；不满足即 STOP，禁止改用其它检出。

本文解决 `libra merge` 与 `libra pull` 在 CLI 参数解析、退出码、消息模板（`SQUASH_MSG`、`--into-name`、`merge.suppressDest`）、输出流（stdout vs stderr）、autostash 引用与恢复、脏工作区判定、多策略与外部策略调度、以及复杂冲突/重命名/目录文件（D/F）合并引擎中与 Git 不一致的 28 个已确认缺口，目标是实现这些场景的 Git 兼容行为、完备测试覆盖与四端文档同步。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收（GC-01）。

### 适用范围

- `libra merge`（`src/command/merge.rs`、`src/command/merge/*.rs`）、`libra pull`（`src/command/pull.rs`）、关联的 `libra commit` autostash 收尾（`src/command/commit.rs`）、`src/internal/pseudo_ref.rs`（`MERGE_AUTOSTASH`）、`libra rev-parse` 伪引用接入（`src/utils/util.rs`）、`libra stash show MERGE_AUTOSTASH`（`src/command/stash.rs`）、`libra rm` 对只有未合并阶段路径的解决（`src/command/remove.rs`）、`libra reflog`（`src/command/reflog.rs`）、`src/command/history_config.rs` 与 `src/utils/error.rs` 的退出码覆写接口。
- 关联测试：`tests/command/merge_test.rs`、`tests/command/pull_test.rs`、`tests/command/reflog_test.rs`、`tests/command/merge_test/`（子模块）（均编入 `command_test` target，测试名形如 `command::merge_test::<fn>`）、`tests/compat/pull_strategy_flags_surface.rs`、`tests/compat/conflict_status_diff_test.rs`、`tests/compat/noninteractive_history_controls_test.rs`、`tests/compat/config_history_defaults_test.rs`、`tests/compat/pseudo_ref_surface.rs`、`tests/compat/compat_worktree_blob_writes.rs`。本计划不新增 `--test` target（不改 `Cargo.toml` `[[test]]`、`tests/INDEX.md`、`tests/compat/README.md`）。
- 关联文档：`docs/commands/{merge,pull,reflog,commit,rev-parse,stash,rm}.md`、`docs/commands/zh-CN/{merge,pull,reflog,commit,rev-parse,stash,rm}.md`、`docs/development/commands/{merge,pull,reflog,commit,rev-parse,stash,rm}.md`、`COMPATIBILITY.md`（`merge` 第 205、212 行，`pull` 第 225 行，`reflog` 第 249 行，`commit` 第 209 行，`rev-parse` 第 217、277 行，`stash` 第 193 行，`rm` 第 186 行；2026-11-18 核对）、`docs/error-codes.md` 及 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{merge,pull,reflog,commit,rev-parse,stash,rm}.en.md`（网站 zh-CN 在渲染时镜像 EN，无独立文件）。

### 非目标

- 不重构 `libra rebase`、`libra cherry-pick`、`libra revert` 的独立冲突引擎（仅在 `pull --rebase` 边界调用既有接口；记入 `DEFER-MP-01`）。
- 不实现 `subtree` 合并策略（记入 `DEFER-MP-02`）。
- 不支持多个 `-s` 中含外部策略的组合（`MP-42` 明确拒绝；记入 `DEFER-MP-03`）。
- 不为 `libra read-tree` 增加 `--reset`/`-u`（`t7606#3` 原样策略脚本所需；记入 `DEFER-MP-04`）。
- 不实现 `diff3`/`zdiff3` 冲突风格中基准段的路径标签（`#1011` Expected 3；记入 `DEFER-MP-05`）。
- 不实现不带参数的 `libra merge`（合并上游；`#1003` AC2 的该形态；记入 `DEFER-MP-06`）。
- 不放行 `--squash` 与最终生效的 `--no-commit` 的组合（Git 允许；记入 `DEFER-MP-07`）。
- 不为外部合并策略增加 Git 没有的超时、进程组或 Windows Job Object 回收机制（`ADR-MP-02`）。
- 不修改 SQLite schema 或迁移 ledger（不触发 GC-13；只操作 `.libra/` 下的 worktree 状态文件与 refs/objects）。
- 不在本计划内改动 `fetch` / `push` / `clone` / 独立 plumbing 命令（由协调者统一排期的其它计划承接）。

### 成功定义

- 覆盖全部 28 个目标 Issue（`#980, #981, #985, #990, #991, #992, #993, #994, #996, #998, #999, #1001, #1002, #1003, #1004, #1007, #1009, #1010, #1011, #1015, #1016, #1018, #1021, #1024, #1025, #1026, #1027, #1028`），每条均有卡特有回归用例与 Git 对照期望（追溯表）。`#1004` 的「未受信任签名」分支在 Libra 不可达（仓库 vault 密钥无信任数据库），按 `MP-32` 以有意差异记录于 `COMPATIBILITY.md`，不另立延后项。`#1027` 的 Acceptance criteria 1–2 由 `MP-15`、`MP-38` 覆盖；AC3（`t7606#3`）以 Libra 形态部分覆盖——`MP-15` 用只更新索引的 `libra read-tree` 策略脚本验收结果树与父提交，原样脚本的 `git read-tree --reset -u` 在 Libra 仓库不可用（无 `.git`，`libra read-tree` 不提供 `--reset`/`-u`），其工作树断言按 `DEFER-MP-04` 延后；Expected behavior 第 3 条（外部策略参与多 `-s` 链）按 `DEFER-MP-03` 延后（`MP-42` 明确拒绝该组合；pinned Git 自身也不能组合两个外部策略）。关闭 `#1027` 时在 issue 中注明并链接 `DEFER-MP-03`、`DEFER-MP-04`。`#981` AC4（冲突 squash 后 `stash list` 显示 autostash）与 pinned Git 不符，`MP-45` 按 Git 把它保存在 `MERGE_AUTOSTASH` 并由 `libra commit` 应用，关闭 `#981` 时注明。`#1003` AC2 中不带参数的 `libra merge`（合并上游）在 Libra 中不受支持，按 `DEFER-MP-06` 延后，关闭 `#1003` 时注明并链接该项。`#980` AC1 中不带 `--cleanup` 的 `commit --no-edit -a` 还依赖 `commit-signing` 计划 `CS-14` 的无编辑器默认清理模式（`DEP-MP-13`），`MP-05` 以显式 `--cleanup=whitespace` 验收消息生成，`CS-14` 未合入时关闭 `#980` 须注明并链接。
- 全部 53 张任务卡逐卡通过 ER-04 A/B/C/D、ER-05 Codex 单审 `VERDICT: PASS`、ER-06/ER-06a 文档与网站同步及 ER-08 发布切片。
- 全部任务卡完成后通过 ER-13 全量收口门（`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`）。

## 使用规则

- 本批次并行起草的多份计划由协调者统一登记至 [`plan-status.md`](plan-status.md) 与 `plan-long.md`；执行期推进任何任务卡时，同一变更必须同步更新 `plan-status.md` 的「计划一览」行（状态、版本或 commit、`YYYY-MM-DD HH:MM:SS UTC`）。
- 不得新建证据目录或证据文件（v2.12，`plan-template.md:74`）。验收摘要、计数、SHA-256 与门结果写入本文对应卡的 `Verification` 记录或「实施证据汇总」。
- 状态推进与「修订历史」统一使用实际 `YYYY-MM-DD HH:MM:SS UTC` 时间。
- 任务 ID 固定；拆分按 G-09 取所属 Phase 的下一个新编号（不重排既有编号）并登记「修订历史」。拆出的卡与其来源卡（含由拆出卡再拆出的卡）在文档与「实施顺序」中组成一段连续的卡片组，组内不夹入其它来源的卡：需要先行的拆出卡排在来源卡之前（如 `MP-35`→`MP-38`→`MP-15`：白名单与未找到诊断先于外部执行；`MP-55`→`MP-14`：`-s octopus` 成为合法取值先于含它的策略链；`MP-50` 的 `libra rm` 先于 `MP-19`），其余排在来源卡之后（如 `MP-15`→`MP-51`→`MP-37`→`MP-42`）；组内顺序与执行顺序以「实施顺序」为准（G-06）。
- 网站文档只在 `/Volumes/Data/gitmono/libra-backend` 的 `cf` 生产分支交付，按「文档与网站验收命令」执行。
- 本计划继承 [plan-template.md](plan-template.md) 的 `GC-01..13`、`ER-01..14`、`G-01..11`。

## 事实基线

> 所有行号和源码锚点核对于 `2026-11-18`（`main` 版本 `0.30.210`），开工当天必须按 ER-02 刷新。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口（CLI） | `Commands::Merge(MergeArgs)` 与 `Commands::Pull(PullArgs)` 在 `cli.rs` 注册并路由到各自的 `execute_safe` | `src/cli.rs:633,693,3972,4083` |
| `MergeArgs` 参数 | `--ff-only`/`--ff`/`--no-ff` 以 `conflicts_with_all` 互斥；`--strategy` 与 `--strategy-option` 互斥；`--strategy` 为封闭 `value_enum`（`ort`/`recursive`/`resolve`/`ours`）；`--no-commit` 与 `--squash` 互斥；无 `--commit` | `src/command/merge.rs:313-333,385-391`；`MergeStrategy::name` `src/command/merge.rs:142-150` |
| FF 模式与配置 | 单个 `--ff`/`--no-ff`/`--ff-only` 覆盖 `merge.ff`，否则读 `merge.ff` | `src/command/merge.rs:1262-1278`；`src/command/history_config.rs:39` |
| `PullArgs` 参数与错误映射 | `--ff-only`/`--ff`/`--no-ff` 均 `conflicts_with_all` 含 `rebase`；`map_merge_error_to_cli` 独立映射 `NonFastForward`、`Conflicts`、`DirtyWorktree`、`UntrackedOverwrite` | `src/command/pull.rs:70-80,1176,1216-1229` |
| 退出码机制 | 合并错误统一 `CliError::failure` + `LBR-CONFLICT-002`（默认 128）；`CliError::with_exit_code` 优先于 `LIBRA_FINE_EXIT_CODES` 与默认映射 | `src/command/merge.rs:1014-1028`；`src/utils/error.rs:1061-1076`；`docs/error-codes.md:60-62` |
| Fast-forward 与状态 | `merge_is_fast_forward` 要求 `!options.no_commit`；`MergeState.strategy: Option<MergeStrategy>`（`#[serde(default)]`），octopus 以 `strategy=None` + 非空 `targets` 表示 | `src/command/merge.rs:2573-2578,4359-4366`；`src/command/merge/state.rs:55-56,79-81,147` |
| 签名验证 | `verify_merge_commit_signature` 只区分 `Good`/`Unsigned`/`Bad`，`Good` 时静默；Libra 只对仓库 vault 密钥验签，无外部 keyring/信任数据库 | `src/command/merge.rs:1634-1647`；`src/command/commit.rs:3156-3171` |
| 多策略链 | `run_merge_with_strategy_fallback` 以零写 dry-run 探测每个策略并打印 `Trying merge strategy <name>...` | `src/command/merge.rs:1417-1468` |
| 外部合并驱动先例 | `merge.<driver>.driver` 外部驱动以 `sh -c` 启动并 `.status()` 等待退出（无超时） | `src/command/merge.rs:5654-5657,5690-5694` |
| Autostash 与伪引用 | `merge/autostash.rs` 以 `merge-autostash.json` sidecar 保存 autostash；`src/internal/pseudo_ref.rs` 未登记 `MERGE_AUTOSTASH` | `src/command/merge/autostash.rs:14`；`src/internal/pseudo_ref.rs:13-32` |
| 测试 target | `merge_test`/`pull_test`/`reflog_test`/`rebase_test` 挂在 `command_test`（`tests/command_test.rs` → `tests/command/mod.rs`）；compat target 名见右列 | `tests/command/mod.rs:1097,1107,1115,1116`；`Cargo.toml` `[[test]]` `compat_config_history_defaults`、`compat_noninteractive_history_controls`、`compat_pull_strategy_flags_surface`、`compat_conflict_status_diff`、`compat_pseudo_ref_surface`、`compat_worktree_blob_writes` |
| 外部参照 | Git 基准 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0-116，核对 `2026-11-17`）；本机 Git 2.54.0 预探值仅作参考 | `/Volumes/Data/gitmono/git` |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-MP-01 | `merge` CLI 参数互斥/覆盖规则（`--ff*`、`--commit`/`mergeOptions`、`--squash`、`--strategy`+`-X`）与 Git 不符 | Git 脚本与常用参数组合被拒绝或误接受 | `#985, #990, #996, #1015`；`src/command/merge.rs:313-333,385-391` | MP-01, MP-26, MP-33, MP-27, MP-28 |
| GAP-MP-02 | `--no-commit` 阻止 fast-forward、`--ff-only` 文案跨 `merge`/`pull` 不一致、冲突与拒绝退出码统一为 128 | 自动化脚本依赖退出码与错误信息失败 | `#992, #1002, #1028`；`src/command/merge.rs:1014-1028,2573-2578`；`src/command/pull.rs:1216-1229` | MP-02, MP-21, MP-22, MP-34, MP-40 |
| GAP-MP-03 | `--edit` 编辑器失败丢弃待决合并；`--into-name` 部分路径被忽略、缺 `merge.suppressDest`、octopus 默认消息格式与 Git 不同 | 编辑中断丢失合并上下文；合并提交标题偏离 Git | `#994, #999`；`src/command/merge.rs:371,380` | MP-03, MP-52, MP-04, MP-49 |
| GAP-MP-04 | `merge --squash` 冲突不写 `SQUASH_MSG`；unborn 分支 reflog 记为 `merge:`；`reflog -1` 多空行 | squash 提交消息与 reflog 审计格式偏离 Git | `#980, #1003`；`src/command/merge.rs:1652-1657,5104` | MP-05, MP-06, MP-29 |
| GAP-MP-05 | 冲突进度与 autostash 提示走 stderr；`--verify-signatures` 缺成功行与 `gpg.minTrustLevel`；`-s resolve` 缺 `Wonderful.`/`In-index merge` | CLI 输出流与诊断文本不匹配 Git | `#981, #1001, #1004`；`src/command/merge/autostash.rs:463,580` | MP-07, MP-08, MP-32, MP-09 |
| GAP-MP-06 | `--autostash` 缺 `MERGE_AUTOSTASH` 伪引用及 `libra commit` 完成应用；脏工作区未按重叠路径过滤；unchanged HEAD 文件被重写 | 本地改动被阻断或丢失，构建缓存因 mtime 失效 | `#991, #993, #1026`；`src/command/merge.rs:4410-4417`；`src/internal/pseudo_ref.rs:13-32` | MP-10, MP-43, MP-48, MP-39, MP-41, MP-45, MP-30, MP-44, MP-11, MP-53, MP-12 |
| GAP-MP-07 | `pull --rebase` ff 矩阵与 `<repo> <refspec>` 上游解析失败；`-s octopus` 被拒、`-s` 链在首个失败后中止；外部 `git-merge-<name>` 策略缺失 | `pull --rebase`、多策略回退及外部策略不可用 | `#998, #1025, #1027`；`src/command/pull.rs:70-80`；`src/command/merge.rs:326,1417-1468` | MP-13, MP-23, MP-55, MP-14, MP-35, MP-38, MP-15, MP-51, MP-37, MP-42 |
| GAP-MP-08 | 冲突标记缺路径标签；D/F、rename/directory、rename/delete、rename/rename D/F、`1to2` 与传递目录重命名冲突处理错误 | 复杂重命名与 D/F 合并损坏工作区或报 `LBR-IO-002` | `#1007, #1009, #1010, #1011, #1016, #1018, #1021, #1024`；`src/command/merge/rename_merge.rs` | MP-16, MP-17, MP-24, MP-18, MP-25, MP-46, MP-50, MP-19, MP-54, MP-20, MP-47 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-20261117-commit-signing.md` | 兄弟计划：`REL-CS-00`（`CS-02/CS-02a` `#1005` 默认不签名）为 `MP-08` 测试前置；`CS-14a` 消费 `MP-05` 的 `SQUASH_MSG`、`CS-02a` 串行于 `MP-26`；`CS-03` 按 `REL-CS-00 -> MP-08 -> MP-10 -> CS-03` 消费 `MP-10` 后的 `commit.rs`；`CS-14`（`#794`）把无编辑器提交的默认清理改为 `whitespace`，是 `#980` AC1 的关闭前提 | 登记 `DEP-MP-02`（incoming，仅挂在 `MP-08`）、`DEP-MP-07`（outgoing，指向 `CS-03`）、`DEP-MP-09`（outgoing，指向 `CS-14a`/`CS-02a`）、`DEP-MP-13`（incoming，`CS-14` 的默认清理模式只约束 `#980` 的关闭，不阻塞任何卡）；只按卡 ID 引用 |
| `plan-20261117-checkout-switch-restore.md` | 兄弟计划：`CSR-21`（`#1008`）消费 D/F 冲突与 `modify/delete` 报告 | 登记 `DEP-MP-03`（outgoing）：`MP-17` 先合入，`CSR-21` 随后按其 `DEP-CSR-04` 消费（单向无环） |
| `plan-20261117-sequencer-am.md` | 兄弟计划：其 `DEP-SA-05` 等待 `MP-10`（`commit.rs`）、`DEP-SA-08` 等待 `MP-30`（`pseudo_ref.rs`）；本计划已按 G-09 自 `MP-10` 拆出 `MP-43`、`MP-48`（二者不写 `commit.rs`），而 `MP-10` 之后的 `MP-41`、`MP-45` 仍修改 `commit.rs` | 登记 `DEP-MP-08`（outgoing）；对方 `DEP-SA-05` 只按卡 ID 引用 `MP-10`，若需要本计划内 `commit.rs` 的最终形态应改为等待 `MP-45`（由对方计划修订，本计划不改他人文件）；在此之前双方对 `commit.rs` 的相交写入由 `DEP-MP-10` 串行窗口保证 |
| `plan-20261117-clone.md` | 兄弟计划：其 `DEP-CL-07` 要求 `CL-20` 先合入 `merge.rs`、`rename_merge.rs`、`tree_merge.rs`，`MP-17` 在其后修改；其 `DEP-CL-10` 与本计划 `MP-30` 共享 `src/utils/util.rs` | 登记 `DEP-MP-11`（incoming，挂在 `MP-17`）；文件级串行由 `DEP-MP-10` 覆盖 |
| `plan-20261117-global-init-infra.md` | 已推送兄弟计划：其 `DEP-GII-03` 规定同批计划在 `COMPATIBILITY.md`、`src/cli.rs`、`src/utils/util.rs`、`src/internal/config.rs` 等共享文件与网站生成产物（`docs-manifest.ts`、`docs-search.ts`、`dist/`）上取得协调者串行窗口 | 本计划以对等的 `DEP-MP-10` 登记同一机制，挂在每张卡上 |
| `plan-20261117-plumbing-misc.md` | 兄弟计划：其 `DEP-PM-03` 等待本计划冲突阶段输出稳定后再改 `ls_files.rs` | 本计划不写 `src/command/ls_files.rs`（任何卡的写集都不含该文件；`MP-25` 只在映射中说明 `ls-files -o` 的既有行为）；对方等待本计划 Phase 4 结束，本计划不登记反向依赖；对方若把本计划记为 `ls_files.rs` 的写者，与事实不符，由协调者转告对方修订 |
| `plan-20261117-describe.md`、`plan-20261117-branch.md` | 兄弟计划：写集含 `src/utils/util.rs` 与 `rev-parse` 文档 | 由 `DEP-MP-10` 串行（`MP-30`） |
| `docs/development/plan/issues/974.md` | 已有 issue 计划：`#974` 合并提交消息生成（共享 `merge.rs`） | 登记 `DEP-MP-04`（incoming 硬前置，挂在 `MP-04`、`MP-49`：`I974-01` 的标题装饰须先合入；`MP-04` 以 Git 的 `merge.suppressDest` 规则取代其「目的地=当前分支即省略后缀」的 ADR 规则） |
| `docs/development/plan/issues/978.md` | 已有 issue 计划：`I978-01` 拥有 `branch.<name>.mergeOptions` 读取器（2026-11-18 核对仍为 `pending`；`src/command/history_config.rs` 尚无该读取器） | 登记 `DEP-MP-05`（incoming，硬前置 `MP-26`/`MP-33`）；本计划不修改 `issues/978.md`：`MP-33` 保持其 `ADR-I978-02`/AC18 对「配置 `--squash` + 显式 `--no-commit`」的 fail-closed（放行属 `DEFER-MP-07`） |
| `docs/development/plan/issues/984.md` | 已有 issue 计划：`#984` `--no-squash`（共享 `MergeArgs` `--squash`） | 登记 `DEP-MP-06`（incoming，挂在 `MP-26`/`MP-33`/`MP-27`） |
| `AGENTS.md:135` | 网站检出路径写作 `/Volumes/Sea/libra-backend`（本机不存在） | 已知文档滞后；以用户 2026-11-18 11:55:09 UTC 确认为准（`DEP-MP-12`），本批不改 `AGENTS.md` |
| `plan-long.md` | 长期兼容性路线图，无新增 LR 编号 | 由协调者在合入时统一登记日期索引 |
| `docs/development/plan/plan-template.md` | 模板规范 v2.16 | 全文遵守 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 28 个实测 Git 兼容缺陷均有明确复现路径 | 拆为 53 张 `implementation` 任务卡，全部 `independent` 发布切片 |
| 可行性 | 每卡聚焦单一行为轴，锚点与测试命令可直接执行 | 卡特有用例一律精确全名过滤并先 `cargo nextest list` |
| 任务卡粒度 | 按 G-04 全部 53 张卡为 `M`，每卡最多一处公开变化；AC/VER 按「任务卡粒度规则」逐条计数 | 审计表与各卡 `Granularity` 逐列一致 |
| 依赖与顺序 | 全计划严格串行（ADR-MP-03）；跨计划依赖以 `DEP-MP-*` 登记 | DAG 无环，符合 G-06/G-10/ER-12 |
| 完整性 | 每卡含实现、卡特有用例、EN+zh+dev+COMPATIBILITY+网站同步与 D 组 | 全部纳入卡内与全局默认 |
| 安全性 | 外部策略名白名单、无 shell `Command::new`、`PATH` 标准查找（`#1027` 的 `PATH=.:$PATH`） | `ADR-MP-02`、`MP-35`、`MP-15` |
| 功能正确性 | 以 `git@6de20f6092` 为规范，期望值经本机 Git 2.54.0 预探并在开工时以 pinned 版本复核 | 「验证执行协议」第 4 条 |
| 接口兼容 | `ADR-MP-01` 与 `src/utils/error.rs:1061-1076` 对齐；`MergeState` 新状态对旧二进制保持可读（`ADR-MP-05`） | `MP-55`、`MP-37` |
| 数据流与控制流 | 停在提交前的普通与三方 `--squash` 合并的 autostash 统一为 `MERGE_AUTOSTASH`，由 `commit`/`--continue`/`--abort` 应用 | `MP-07` 只管输出流，`MP-10`/`MP-43`/`MP-39`/`MP-41`/`MP-45` 管应用与清理 |
| 性能与容量 | 脏工作区检查与 unchanged mtime 跳过只比较受合并影响路径 | `MP-11`、`MP-12` |
| 可靠性与容错 | `--edit` 失败保留状态；未处理策略链恢复 pre-merge index/worktree | 故障恢复矩阵 |
| 可维护性 | 复用 `merge/output.rs`、`merge/autostash.rs`、`merge/conflict.rs`、`pseudo_ref.rs` 与 `with_exit_code` | 禁止重复实现（GC-02） |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 02:55:00 UTC | 初稿创建 | 建立覆盖 28 个 merge/pull 兼容问题的 20 张任务卡计划 | N/A | 全文 |
| 2026-11-17 03:05:00 UTC | Claude R1 FAIL + Codex R1 FAIL | 拆分多轴卡（MP-02→MP-02/21/22、MP-13→MP-13/23、MP-17→MP-17/24、MP-18→MP-18/25）；修正测试命令为 `command_test`；取消并发；补全 D 组、T-1、`immutable-release`、开发文档与缺失 AC | MP-02 → MP-02, MP-21, MP-22; MP-13 → MP-13, MP-23; MP-17 → MP-17, MP-24; MP-18 → MP-18, MP-25 | 实施顺序、并发声明、粒度审计表、追溯表、ADR-MP-01/03、review log |
| 2026-11-17 03:10:00 UTC | Claude R2 FAIL + Codex R2 FAIL | 拆分 MP-01→MP-01/26/27/28 与 MP-06→MP-06/29 并补双向 G-09 标记；公开变更卡改标 `M`；精确核算独立谓词计数；修复 3 个空 unit 测试过滤器；`MP-10` 纳入 `pseudo_ref.rs`（`T-1`）与 `commit` 文档；`MP-14/15` 纳入 `state.rs` 兼容；统一普通/squash autostash 恢复合同；调整 `MP-15` `PATH` 规则以兼容 `#1027`；pin Git `2.56.0@6de20f6092`；登记 `DEP-MP-02` 对接 `plan-20261117-commit-signing.md` | MP-01 → MP-01, MP-26, MP-27, MP-28; MP-06 → MP-06, MP-29 | 实施顺序、依赖登记表、粒度审计表、追溯表、ADR-MP-01/02、各卡正文、review log |
| 2026-11-17 03:16:00 UTC | Claude R3 FAIL + Codex R3 FAIL | 修正 D 组真实 CI job 名（`base.yml` 含 `redundancy` 无 `network-remotes`；`release.yml` 五个真实 job）；`DEP-MP-02` 精确挂在 `MP-08`→`CS-02` 消除环路并新增 `DEP-MP-03` 对接 `CSR-21`；`MP-26` 同步包含 `--squash --commit` 拒绝且 `MP-27` 补全 `#985`；`MP-22` 区分冲突/局部修改退出 `1` 与 `would be overwritten` 退出 `2` 并补 `pull` 验证；`MP-29` 新增 `reflog -1` 非空行测试；`MP-12` 改标 `M`；补全 `MP-08`/`MP-15` P2 与审计表 `landing` 计数 | N/A | D 组、依赖表、MP-08/12/15/22/26/27/29、粒度审计表、review log |
| 2026-11-17 03:30:00 UTC | Codex R4/R5 FAIL | `MP-27` 补全 `--squash --ff-only` 具名验证；`MP-26` 同步卡内 `Granularity`；`MP-26`/`MP-27` 明确 `#985` Git 诊断格式（`fatal: options '--squash' and '--no-ff.'/'--commit.' cannot be used together`） | N/A | MP-26, MP-27, review log |
| 2026-11-17 03:48:00 UTC | Claude R6 FAIL + Codex R6 FAIL | `DEP-MP-03` 改为单向 `outgoing`（消除与 `CSR-21` 双向死锁）；新增 `DEP-MP-04/05/06/07` 协调 `issues/974,978,984` 与 `CS-03`；修正 `ADR-MP-01`/`MP-22` 退出码矛盾（`DirtyWorktree`=1、`UntrackedOverwrite`=2）；新增 `ADR-MP-04` 与 `MP-10` 纳入 `src/utils/util.rs`/`COMPATIBILITY.md`；`MP-15` 明确外部策略超时与回收测试；同步审计表/卡内 `landing`、追溯表、Phase 1 入口、`CS-14a`、`MP-12` 粒度描述与 ER-13 全量失败处置 | N/A | 与其它计划、ADR-MP-01/04、依赖表、审计表、MP-01/07/10/12/15/17/21/22/26/27、追溯表、完成判据、review log |
| 2026-11-17 03:55:00 UTC | Claude R7 PASS (P2) + Codex R7 FAIL | 修正 `完成判据` 明确任何全量失败处置后均须重跑全量至全绿（禁止带红通过）；`MP-15` 将 `LIBRA_MERGE_STRATEGY_TIMEOUT_SECS` 改为默认不限时（Git 兼容）+ 显式配置超时 kill+wait（并同步文档）；统一 10 张卡内 `landing=1` 与审计表对齐；补齐 `Release write set 默认`、`MP-06`/`MP-07` `pull_test`、#984 `--no-squash` 验证与 `MP-11` 退出码复核 | N/A | 全局默认、MP-02/03/05/06/07/11/13/15/16/23/26/27/28/29、完成判据、review log |
| 2026-11-17 04:08:00 UTC | Codex R8 FAIL + Claude R9 FAIL + Codex R9 FAIL | 补齐 `Release write set 默认`（仅 `Cargo.toml`, `Cargo.lock`, `install.sh`, `install.ps1`）与 `codeql.yml` `release/**`；修正 `ADR-MP-01` 与 `MP-22` 按 Git 路径划分退出码（fast-forward 失败退出 `1`，三方合并 `"Your local changes ... would be overwritten"` 退出 `2`，移除 `MP-22` 越界的策略未处理声明）；`MP-16/17/24/18/25/19/20` 因共享 `merge_rebase_trees`/`virtual_base.rs` 标为 `T-1` 并补 `rebase_test::`；收紧 `DEP-MP-05` 为硬前置阻塞；`MP-07` 补 `pull_test` 与精确文本、`MP-06` 补 `pull_test`、`MP-05` 补 `commit.cleanup=scissors`；重算 `MP-26/27/08/15` AC 谓词计数 | N/A | 全局默认、D 组、ADR-MP-01、依赖表、审计表、MP-05/06/07/08/11/15/16..20/22/24..27、review log |
| 2026-11-17 04:19:00 UTC | Codex R11 FAIL + Claude R11 网络中断 | `MP-16..20, MP-24, MP-25` `Verification` 补齐 `rebase_test::` 并更新 `VER=3/8` | N/A | 审计表、MP-16..20/24/25、review log |
| 2026-11-17 04:23:00 UTC | Codex R12 FAIL + Claude R12 FAIL | 统一 `codeql.yml` `push` 触发范围含 `release/**`；按 G-03/G-04 拆出 `MP-32`（`gpg.minTrustLevel`）、`MP-30`（`pseudo_ref.rs` `MergeAutostash` + `tests/compat/pseudo_ref_surface.rs` + `rev-parse` 三端文档）、`MP-31`（外部策略超时控制与 `Security` 覆盖登记）；修正 `ADR-MP-04` 保留 `merge-autostash.json` 单一事实源；`MP-26/27` 改于 `mergeOptions` 解析后校验 `--squash`+`--commit` 并放行 `--no-commit --squash`；`MP-05` 补 `output.rs`；重算 `MP-14`/`MP-22` AC；`兼容与文档收口` 补 `Cargo.toml [[test]]: N/A` | MP-08 → MP-08, MP-32; MP-10 → MP-10, MP-30; MP-15 → MP-15, MP-31 | 全文、ADR-MP-01/03/04、默认覆盖、审计表、MP-05/08/09/10/11/14/15/16/22/26/27/30/31/32、追溯表、M1/M2/M3、文档收口、review log |
| 2026-11-17 04:51:00 UTC | Claude R13 FAIL + Codex R13 FAIL | 修复 `MP-30`→`MP-10` 与 `MP-31`→`MP-15` 自依赖；`MP-26` 拆出 `MP-33`（post-`mergeOptions` `--squash`+`--commit` 拒绝 128 与 `--no-commit --squash` 放行，同步更新 `DEP-MP-05` 覆盖 `I978-01` AC18 并纳入 `history_config.rs` 白名单核验，移除 `MP-27` 重复表述）；`MP-22` 拆出 `MP-34`（拒绝路径退出码 1/2/128）守住 `AC<=8`；`MP-21` 补 `pull_test` 新文案与 `128` 验收；`MP-14`/`MP-31` 写集补 `docs/error-codes.md`；重排 `ADR-MP-03` 先于 `ADR-MP-04` 并更新 `34 张` 统计 | MP-26 → MP-26, MP-33; MP-22 → MP-22, MP-34 | 评审结论、ADR-MP-03/04、DAG、DEP-MP-05、Phase 1/M1、审计表、MP-14/21/22/26/27/30/31/33/34、追溯表、review log |
| 2026-11-17 05:02:00 UTC | Claude R14 FAIL + Codex R14 FAIL | 修复 4 处卡内 `Dependencies`（`MP-33`→`MP-26`、`MP-34`→`MP-22`、`MP-11`→`MP-30`、`MP-16`→`MP-31`）对齐 DAG 与 `Granularity`；`MP-14` 与 `MP-31` 因修改 `docs/error-codes.md` 标为 `T-1`；更新 `L158` 示例为 `MP-30`、`Phase 4` 入口为 `MP-31 完成`、`MP-31` AC2 钉死退出码 `2`、并明确 `MP-07` `autostash.rs` 冲突停止与提示单轴说明 | N/A | 修订历史、ER 示例、Phase 4、MP-07/11/14/16/31/33/34、review log |
| 2026-11-17 05:15:00 UTC | Claude R15b FAIL + Codex R15b FAIL | `MP-26` 同步加 `--squash --commit` fail-closed 拒绝（128）并因修改 `history_config.rs` 标 `T-1`；`MP-33` 写集补 `issues/978.md` 与校验；`MP-15` 拆出 `MP-35`（`MergeState.strategy` 兼容、策略名安全校验与多策略组合）使两卡均守 `AC<=8` 并补 `Security` 覆盖；更新 `DEP-MP-02` 对齐 `REL-CS-00`，新增 `DEP-MP-08`（`MP-30/10`→`SA-05/07d`）与 `DEP-MP-09`（`MP-05/26`→`CS-14a/02a`）；校准 `MP-05/07/08/14/27/34` AC 计数、`MP-06` `pull` 文档、`35 张卡` 与 `完成判据` v2.12/Rollback 条目 | MP-15 → MP-15, MP-35 | 状态行、修订历史、DAG、依赖表、发布分组、Phase 3、审计表、MP-05/06/07/08/14/15/26/27/31/33/34/35、追溯表、完成判据、review log |
| 2026-11-17 05:23:00 UTC | Codex R16 FAIL + Claude R16 网络中断 | 将 `MP-35` 前置至 `MP-15` 之前（`MP-14 -> MP-35 -> MP-15 -> MP-31`），先落地策略名安全校验 `[A-Za-z0-9_.-]+` 与 `MergeState.strategy` 兼容，再由 `MP-15` 开放外部策略执行，消除未校验外部策略提前发布窗口 | N/A | 状态行、修订历史、DAG、Phase 3、审计表、MP-15/35/31、review log |
| 2026-11-17 05:27:00 UTC | Codex R17 FAIL + 跨计划同步 | 将“可与 `MP-14` 多策略回退链组合使用”从 `MP-35` 移至 `MP-15`（`MP-15` `8/8 AC`、`MP-35` `3/8 AC` 仅测校验器单测与 `MergeState` 兼容）；澄清 `MP-26` `Out of scope` 仅排除 `--no-commit --squash` 与 post-`mergeOptions` 矩阵（`MP-26` 自身承担中间版本 `--squash --commit` 128 拒绝）；同步 `CS-14a`→`CS-14`（`commit.cleanup=scissors`）与 `CS-03` | N/A | 状态行、修订历史、依赖表、审计表、MP-26/15/35、review log |
| 2026-11-17 05:35:00 UTC | Codex R18 FAIL + Claude R18 网络中断 | 扩展 `MP-33` 同步更新 `docs/development/plan/issues/978.md` 的 `ADR-I978-02`、`I978-01` AC18、`Verification`、测试断言与 `完成判据`，消除两份计划内部矛盾；将 `MP-15` 与 `MP-31` 纳为 `REL-MP-01` 家族卡（`MP-15`/`MP-31` 为 `family child`，新增 `Task type: release` 发布点 `MP-36`），确保首个执行外部策略进程的公开发布即包含 `GC-09` 超时/信号进程组回收；同步 `ADR-MP-03`、`Phase 3`、`M2` 与 `MP-16` 依赖 | 新增 `MP-36`（`REL-MP-01` 发布点） | 状态行、修订历史、ADR-MP-03、DAG、发布分组、Phase 3、默认覆盖、审计表、MP-33/15/31/36/16、追溯表、M2、review log |
| 2026-11-17 05:39:00 UTC | Codex R19 FAIL | 将 `MP-15` 与 `MP-31` 家族子卡的 `C/D coverage from` 改为 `MP-36`、`Full-suite trigger` 改为 `N/A`（由发布点 `MP-36` 统一跑 `T-2`）；在 `MP-33` 纳入 `src/internal/history_config.rs`（`T-1`）与 `I978-01` 既有代码/测试断言的同步改写，使 `978.md` 与实际代码/测试完全对齐 | N/A | 状态行、修订历史、默认覆盖、审计表、MP-33/15/31、追溯表、review log |
| 2026-11-17 05:45:00 UTC | Codex R20 FAIL | 澄清 `MP-15` 外部策略退出码 `2` 与 `MP-14` 多策略回退链交互（单策略直接以 `2` 失败中止，多策略链中退出码 `2` 先恢复 pre-merge index/worktree 再继续下一策略，全链耗尽才报错 `2`）；补齐 `MP-15` `GIT_INDEX_FILE`/`GIT_DIR`/`GIT_WORK_TREE` 互操作环境变量与可执行脚本验证；强化 `MP-31` 使用独立进程组 `process_group(0)` 并在超时或父进程 `SIGINT`/`SIGTERM` 时对整个进程组 `kill(-pgid, SIGKILL)`+`waitpid` 回收后代进程并补测 | N/A | 状态行、修订历史、MP-15、MP-31、review log |
| 2026-11-17 06:54:00 UTC | Codex R21 FAIL + 跨计划引用校准 | 修正 `MP-33` 写集/描述/触发器中不存在的 `src/internal/history_config.rs` 为真实路径 `src/command/history_config.rs`；修正 `MP-31` 进程回收契约为可实现语义：Unix `process_group(0)` + `kill(-pgid, SIGKILL)` + `waitpid(child_pid)` 回收直接子进程（显式说明通过 `setsid`/`setpgid` 脱离进程组的孙进程不在 `-pgid` 范围内），并补充 Windows `#[cfg(windows)]` Job Object（`CreateJobObjectW` + `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` + `TerminateJobObject` + `child.wait()`，`TerminateProcess` 回退）跨平台实现与验证；同步 `MP-08`/`DEP-MP-09`/`与其它计划` 中 `REL-CS-00` 与 `CS-14a` 引用及 `38 张卡` 评审表统计 | N/A | 状态行、与其它计划、评审结论、修订历史、依赖表、MP-08、MP-31、MP-33、review log |
| 2026-11-17 07:03:00 UTC | Codex R22 FAIL | 强化 `MP-31` Windows Job Object 无竞态隔离与 fail-closed 契约：复用 `crate::utils::windows_process::{CaptureJob, CREATE_SUSPENDED_NO_WINDOW}`（`src/utils/windows_process.rs:25-80`）以 `CREATE_SUSPENDED` 创建子进程并完成 `AssignProcessToJobObject` 后再 `ResumeThread`，若分配失败则在恢复运行前立即 `child.kill()` + `child.wait()` 以退出码 `2` fail-closed 中止（禁止放行未受控子进程）；在 `MP-31` `Verification` 增补 Windows 专属回归测试 `test_merge_custom_external_strategy_windows_job_cleanup`（`VER=3/8`），并同步审计表 | N/A | 状态行、修订历史、审计表、MP-31、review log |
| 2026-11-17 12:27:28 UTC | 用户单审指令 | 计划提交门改为 Codex 单审，不再要求 Claude Review | N/A | 头部、成功定义、M0、完成判据 |
| 2026-11-17 17:00:50 UTC | 机械自审：网站交付合同 | 现行网站路径统一 `/Volumes/Data/gitmono/libra-backend`；补 checkout 校验、按卡同步、前端验证、最终字节 Claude PASS、签名推送部署和家族发布顺序 | N/A | 使用规则、现行网站写集 |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件【2026-11-18 双审 R1 注：本行所称「用户最新指令」「已被取代」为当时无出处的转述，现以用户 2026-11-18 11:55:09 UTC 确认为依据，`AGENTS.md:135` 为已知文档滞后，见「文档职责」与 `DEP-MP-12`】；`MP-14R`/`MP-36` 精确测试名补 `command::` 前缀；`MP-14a` 旧版拒绝与恢复验收由 1 项拆为 7 项；`MP-21`/`MP-22`/`MP-24`/`MP-31`/`MP-32`/`MP-35` 验收按独立判据拆分；`MP-35`/`MP-35a` 与 `MP-31`/`MP-31a`/`MP-31c`/`MP-31b`/`MP-31d` 纳入 `REL-MP-01` 同版发布，进程回收按触发路径拆分终止、等待、退出码与诊断；Windows 证据补本地隔离运行合同并标记 `MP-31d`/`MP-36` blocked；Phase 1/2/3/4 与 M2 计数同步 | N/A | 全文对应章节 |
| 不晚于 2026-11-18 09:43:54 UTC（2026-11-18 双审 R1 更正：原记 `2026-11-18 10:35:00 UTC` 晚于本行实际写入时刻——文件最后修改时间为 09:43:54 UTC——属未来时间；实际写入时刻不可考，改记该可证上界） | Codex 复审 FAIL 修复（验证契约与门禁） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 14:20:14 UTC | 用户 2026-11-18 双审指令 | 计划文本提交门改为 Claude（Subagent）与 Codex 双审，二者对同一受审 SHA-256 均 `VERDICT: PASS` 方可提交，取代 2026-11-17 12:27:28 行的「Codex 单审、不再要求 Claude Review」口径；执行期 ER-05 评审门与网站 Claude Code `PASS` 门不变 | N/A | 头部「审阅合同」、M0、完成判据、Codex review log 说明 |
| 2026-11-18 14:20:14 UTC | 双审第 1 轮前专项清扫（BRIEF 六类清单）与全面重写 | ① 网站检出依据改为用户 2026-11-18 11:55:09 UTC 确认（`DEP-MP-12`，已满足），`AGENTS.md:135` 记为已知文档滞后；更正 10:35:00 未来时间戳行并注释 08:19:08 行；② 撤销外部策略超时与进程组/Windows Job Object 回收设计（`ADR-MP-02` 修订：与 Git 一致同步等待、无超时），撤销以真实旧二进制验证的家族设计（新增 `ADR-MP-05`：`merge-state.json` 不写新 `strategy` 取值、外部策略名入可选字段），据此撤销 `MP-14a`、`MP-14R`、`MP-35a`、`MP-31`、`MP-31a`、`MP-31b`、`MP-31c`、`MP-31d`、`MP-36` 与 `REL-MP-OCTOPUS`/`REL-MP-01`，全部卡改为 `independent`；③ 按 G-09 自 `MP-15` 拆出 `MP-37`（外部策略退出 1/2 与链式回退），`MP-35` 改为策略名白名单与 `Could not find merge strategy`；④ 重写「验证执行协议」（精确全名过滤与 `nextest list`、`S` 只记录、共享回归组不计 VER、进程级隔离、Σ 快照定义）并新增「文档与网站验收命令」第 1–8 步与 W-1；⑤ 全部卡按 Git 2.54.0 预探与 issue 验收重写 AC 与断言映射（单谓词计数、失败类断言稳定码、退出码单列），修正 `MP-07`（squash autostash 不入 `stash list`，移交 `MP-10`）、`MP-11`（删去与 Git 不符的 `--continue` 拒绝）、`MP-08`/`MP-32`（vault 验签视为 `ultimate`）等与 Git 不符的验收；⑥ 删除无关的 `.github/workflows/base.yml` 写集，全部卡写集补 `COMPATIBILITY.md` 与网站生成产物并挂 `DEP-MP-10`（对等 `DEP-GII-03`），新增 `DEP-MP-11`（`CL-20` 先于 `MP-17`）；⑦ 刷新源码锚点（`src/cli.rs:633,693,3972,4083` 等）与卡数统计 | 撤销：`MP-14a`, `MP-14R`, `MP-35a`, `MP-31`, `MP-31a`, `MP-31b`, `MP-31c`, `MP-31d`, `MP-36`；拆分：`MP-15` → `MP-15`, `MP-37` | 全文 |
| 2026-11-18 14:33:15 UTC | 双审 R1：Claude FAIL（7×P1）、Codex FAIL（分区 A 5×P1、B 8×P1、C 6×P1、D 8×P1），受审 SHA-256 `41caa36e25e7…` | ① 按 G-09 拆出 `MP-40`（自 `MP-34`：`pull` 拒绝退出码 FF 1 / 三方 2，补 `pull` 三方路径）、`MP-39`（自 `MP-10`：`--abort`、三方 squash 与可快进 squash 的 autostash 应用与 sidecar 清理）、`MP-38`（自 `MP-35`：`Could not find merge strategy` 与过渡拒绝），`MP-35` 补 `.`、空名、白名单外字符；② `ADR-MP-02` argv 含 `--<xopt>` 并由 `MP-15` AC8 验收；`ADR-MP-04` 改正：可快进 squash 由 Git 立即应用 autostash；③ 拆分合并谓词（`MP-14` targets、`MP-33` HEAD/暂存、`MP-37` 两个状态字段、`MP-25` 内容/缺失），保留类 AC 改为 Σ 全量比较（`MP-11`、`MP-27` 三次拒绝、`MP-32`）；`MP-22` 去掉未验收的恢复断言；④ 夹具自包含（`F-DIV`/`F-CONF` 写全身份与提交），配置值以 `--` 传入（`#990` 复现形式），`MP-26` AC4 改为令牌顺序可区分的 `--no-commit --commit`；pull 夹具改用 `tests/command/pull_test.rs:87-147` 既有 helper，`MP-06` 用例移入 `pull_test`；⑤ 期望值收紧为整行/整输出比较（`MP-08` 签名者、`MP-16` 两侧标记、`MP-17` status 全文与报告行、`MP-18` ls-files 阶段与 blob、`MP-20` file location 整行、`MP-32` 五个阈值及大小写），`MP-13`/`MP-23` 加本地提交内容保留断言并登记被取代的既有用例；⑥ `MP-05` 冲突段写入 `SQUASH_MSG`（由既有 `commit` 种子路径消费，不改 `commit.rs`）；`MP-07` `--json` 失败断言改为 stdout 无人读进度；`MP-09` `-q` 按 Libra 全局 `--quiet` 为空并记有意差异；⑦ 退出码单列 AC 的卡清单改为生成并与 ADR-MP-01 一致，安全矩阵措辞改为「拒绝含路径分隔符的名字、`PATH` 中相对目录照常参与」 | 拆分：`MP-34` → `MP-34`, `MP-40`；`MP-10` → `MP-10`, `MP-39`；`MP-35` → `MP-35`, `MP-38` | 实施顺序、依赖、Phase 1–3、审计表、追溯表、ADR-MP-01/02/04、测试矩阵、风险、里程碑 |
| 2026-11-18 14:39:06 UTC | 双审 R2：Claude FAIL（1×P1、1×P2）、Codex FAIL（分区 A 1×P1、B 4×P1、C 5×P1、D 4×P1），受审 SHA-256 `38a54753b388…` | ① Σ 增加 `MERGE_MSG`，并写明排除 `COMMIT_EDITMSG`（一次性钩子/编辑器缓冲）的理由；② 按 G-09 自 `MP-39` 拆出 `MP-41`（三方 `--squash --autostash`，含干净与冲突两路的保留、`commit` 应用与 sidecar 删除），`MP-39` 只留 `--abort` 与可快进 squash；③ `MP-35` 改为表驱动覆盖全部白名单外字符并以合法全字符类名字验证通过校验（本卡版本 `LBR-UNSUPPORTED-001`，`MP-38` 取代为未找到诊断）；④ 拆分合并谓词（`MP-16` 反向与同路径两侧标记各一条、`MP-17` 移动报告与自动合并报告各一条、`MP-14` AC6 去掉 AC 外的 stderr 断言）；⑤ `MP-33`/`MP-27` squash 暂存改为比较 `ls-files -s` 全文（含 blob），`MP-28` 补 `-Xours` 合并提交断言，`MP-04` 多值与清空语义改用能区分「只读最后一个值」「不清空」的序列，`MP-33` AC8 限定现行规范性正文并保留历史行；⑥ 夹具身份改为显式 `libra config user.name/user.email`（`configure_identity_via_cli` 设为 `Test User`），`MP-07` AC2 补 `Auto-merging`，`MP-11` AC6 改指 AC5，`MP-12` 去掉多余的「工作树与索引一致」条件并补无关本地修改不被改写，`MP-05` 补「squash 不写 `merge-state.json`」锚点 | 拆分：`MP-39` → `MP-39`, `MP-41` | Phase 2、审计表、追溯表、ADR-MP-04、「验证执行协议」第 8 条 |
| 2026-11-18 14:46:22 UTC | 双审 R3：Claude FAIL（3×P1、P2 若干）、Codex FAIL（分区 A 3×P1、B 3×P1、C 4×P1、D 3×P1），受审 SHA-256 `395dfd787dd4…` | ① 外部策略重排：`MP-15` 改为「执行与退出码处理」（退出 0 用脚本暂存的独特树验证、退出 1 冲突状态与 `external_strategy`、退出 2 诊断/退出码/Σ 回滚），按 G-09 拆出 `MP-37`（argv/工作目录/`PATH=.`/`--json`/`-X` 契约）与 `MP-42`（多策略链中由第二个策略记录回绕后的状态），使首个开放执行的版本即含完整失败处理；② 按 G-09 拆出 `MP-43`（`merge --continue` 路径）与 `MP-44`（`stash show -p MERGE_AUTOSTASH`，`#991` AC1）；`MP-10` 改用 `#991`/`t7600` 同文件不重叠夹具，分别断言提交内容 `R15` 与工作树 `R159`，应用冲突时以 `stash show -p stash@{0}` 等于合并前 diff 证明保存的正是该 autostash；③ Σ 改为 `.libra/` 顶层全部普通文件与 `refs/` 树（列明排除项与理由，含 `FETCH_HEAD`）；④ `MP-22` AC4 改为比较冲突索引阶段，并登记被取代的既有 `Some(128)` 冲突断言（`MP-34`/`MP-40` 同理），写集补 `tests/command/merge_test/`；`MP-03` 补合并后索引与两条完成路径的提交内容；⑤ 拆分合并谓词（`MP-18` 两条报告、`MP-25` 报告与无 I/O 错误、`MP-19` 两个工作树文件、`MP-20` 三个阶段），`MP-30` 去掉 AC 外的回归断言，`MP-12` AC4 措辞与映射一致，若干映射改为单一谓词；⑥ 风险登记如实写「非 ASCII 代表字符」 | 拆分：`MP-37` → `MP-37`, `MP-42`；`MP-10` → `MP-10`, `MP-43`；`MP-30` → `MP-30`, `MP-44`；`MP-15`/`MP-37` 轴重排 | 实施顺序、Phase 2–4、审计表、追溯表、ADR-MP-01/02/04/05、「验证执行协议」第 8 条、适用范围（`stash`） |
| 2026-11-18 14:52:01 UTC | 双审 R4：Claude FAIL（1×P1）、Codex FAIL（5 分区：A 1×P1、B 2×P1、C 4×P1、D 5×P1、E 3×P1），受审 SHA-256 `b4e851e05b6c…` | ① Σ 改为覆盖 `libra.db` 中除审计/缓存表（`reflog`、`operation*`、`object_index`）外的全部表与 `.libra/` 下递归全部状态文件（列明排除项与理由）；② `MP-39`/`MP-41` 改用 `#991` 同文件夹具（`R15`/`R159`），以「合并停止时本地修改已收起」「abort 后 Σ 全量相等」「快进 squash 后工作树 `R159`、索引 `R15`」等能区分「未做 autostash」的判据重写；③ `MP-15`/`MP-42` 以脚本 `write-tree` 记录的完整树比较提交树，`MP-42` 由第二个策略在修改前复制索引并记录工作树哈希、与合并前记录逐字节比较；`MP-15` 描述写明等待外部策略期间不持有索引/操作锁；④ `MP-21` 断言完整分叉建议块（Libra 命令名），`MP-28` AC1 改查提交内容，`MP-07` `--json` 失败时 stdout 为空，`MP-12` 补无关修改文件 mtime 不变，`MP-30` AC3 只检索 `DECLARED` 数组，`MP-13` AC4 去掉 AC 外断言，`MP-24`/`MP-25`/`MP-19` 的 ls-files 断言改为 Git 2.54.0 预探的逐字输出（含 blob），`MP-25` 排除任何 `LBR-IO-*`，`MP-20` 夹具写明 `z/b`、`z/c` 内容；⑤ 「验证执行协议」第 6 条的 Unix 限定示例改为可执行脚本与 `GIT_EDITOR=false` | N/A | 「验证执行协议」第 6、8 条、MP-07/12/13/15/19/20/21/24/25/28/30/39/41/42 |
| 2026-11-18 14:58:04 UTC | 双审 R5：Claude PASS（1×P2）、Codex FAIL（5 分区：A 2×P1、B 3×P1、C 2×P1、D 5×P1、E 3×P1），受审 SHA-256 `06421d3ca87b…` | ① 外部策略再分轴：`MP-15` 交付首次开放执行所需的完整调用契约（argv 含 `-X`、工作目录、`PATH=.` 并以脚本暂存树证明确实执行、`--json` 输出）与「任何非 0 退出即 Σ 回滚」的安全失败；`MP-37` 改为退出 1 冲突状态/`external_strategy`/索引保留与 Git 退出码 1/2；`MP-42` 回绕判据拆为索引与工作树（含类型与符号链接）两条；② 按 G-09 自 `MP-41` 拆出 `MP-45`（冲突三方 squash），`MP-10`/`MP-41`/`MP-45` 补「sidecar 记录的正是该本地修改」（`diff --old S^ --new S` 等于合并前 diff），`MP-39` AC1 改为「工作树不含本地修改」（冲突块使行号失效）；③ `MP-21` 断言完整 10 行建议块，登记并改写依赖旧文案的既有断言（写集补两个 compat 测试文件与回归组）；`MP-22` 补 `--squash` 冲突退出 1；`MP-23` 补 `. side` 与 `--no-ff`/`--ff`/`--ff-only` 两序组合（`#998` AC1/AC3），成功判据改为父提交；④ `MP-25` 空文件与重命名内容拆为两条，`MP-20` 补 `z/d` 不在工作树与索引（`t6423` 13b），`MP-16`/`MP-19` 补各自冲突调用的非 0 执行条件；⑤ `DEP-MP-10` 与网站第 2 步统一：网站部分总须持有协调者网站窗口；`ADR-MP-05` 改为准确描述以集成用例检查 `strategy` 字段 | 拆分：`MP-41` → `MP-41`, `MP-45`；`MP-15`/`MP-37` 轴重排 | 依赖登记表、网站第 2 步、ADR-MP-01/04/05、Phase 2–3、审计表、追溯表 |
| 2026-11-18 15:03:28 UTC | 双审 R6：Claude FAIL（1×P1）、Codex FAIL（5 分区：A 1×P1、B 2×P1+1×P2、C 3×P1、D 2×P1、E 3×P1），受审 SHA-256 `4643e8979ccd…` | ① 网站窗口统一为第 2–6 步持有、第 6 步推送后释放（第 7 步只读线上页面）；② `MP-02` 登记并改写 `compat_config_history_defaults` 中两个在可快进夹具上依赖 `--no-commit` 停止的既有用例（写集与回归组同步），`MP-21` 关闭建议时断言整块特征行均不出现；③ sidecar 身份断言写明以实际 OID 作为独立子进程参数调用 `libra diff --old <S>^ --new <S>`；`MP-05` 更正 `MERGE_MSG` 表述与源码锚点（`record_squash_message` 调用点）；`MP-30` 登记并同卡改写 `pseudo_ref_surface` 中以「. `AUTO_MERGE` alone」为结束锚点的解析；`MP-15` 子目录夹具显式 `mkdir sub`；④ `MP-19` 同卡让 `libra rm <path>` 能解决只剩未合并阶段的源路径并以 `rm` + `add` + `--continue` 端到端验收（删去「以移除代替 stage 1」的有意偏离），写集补 `src/command/remove.rs` 与 `rm` 文档；按 G-09 自 `MP-25` 拆出 `MP-46`（三条冲突报告）、自 `MP-20` 拆出 `MP-47`（`y/d` 阶段与 `z/d`、`z/` 清理），`MP-20` 补 `Auto-merging y/d` | 拆分：`MP-25` → `MP-25`, `MP-46`；`MP-20` → `MP-20`, `MP-47` | 依赖登记表、网站第 2 步、适用范围（`rm`）、Phase 4、审计表、追溯表、风险登记 |
| 2026-11-18 15:07:57 UTC | 双审 R7：Claude FAIL（1×P1）、Codex FAIL（5 分区：A 1×P1+1×P2、B 1×P1+1×P2、C 2×P1、D 3×P1、E 2×P1），受审 SHA-256 `5f3ee320b010…` | ① `MP-42` 与 `ADR-MP-02` 按 pinned Git 处理多策略链中退出 1 的候选（回绕后继续、遇干净即采用、全部不干净时以候选 `Using the <s> strategy to prepare resolving by hand.` 并退出 1）；`MP-15` AC8 改为退出 1 与退出 2 都回滚（`MP-37` 登记被取代的退出 1 断言）；② 按 G-09 自 `MP-10` 拆出 `MP-48`（应用冲突转存 `stash list`，提示按 pinned `sequencer.c:4894-4900` 文案），`MP-10`/`MP-43` 补双父合并断言；③ `MP-02` 逐个写明两个 compat 用例的改写方式（`--ff-only` 用例改断言快进）；`MP-30` 登记并改写 `internal::pseudo_ref::tests::the_declared_set_is_the_c5_table`（加 lib 回归组）；`MP-23` AC2/AC4 明确沿用对应 pull 之后的仓库；④ `MP-19` F-1021 改为比较 `ls-files -s` 全文（含 stage 1），常规解决路径以可区分的解决内容与 `write-tree` 比较提交树；`MP-25` 以「正常停在冲突」的正向断言加「无 `LBR-IO-*`」分列；⑤ 「使用规则」写明拆出卡取新编号、在文档与实施顺序中紧随来源卡（Codex 分区 C 关于 G-09「Phase 末尾」位置的意见按此口径不采纳：G-09 约束的是编号追加与不重排，执行顺序以实施顺序为准，且把 `MP-35` 移到 `MP-15` 之后会先开放外部执行再加白名单） | 拆分：`MP-10` → `MP-10`, `MP-48` | 使用规则、ADR-MP-02/04、Phase 2、审计表、追溯表 |
| 2026-11-18 15:24:49 UTC | 双审 R8：Claude FAIL（1×P1）、Codex FAIL（5 分区：A 1×P1、B 3×P1+1×P2、C 5×P1、D 4×P1、E 1×P1），受审 SHA-256 `bc80eb5d951c…` | ① Git 2.54.0 预探发现 pinned `get_strategy` 在找到首个自定义策略后释放命令列表、第二个自定义策略无法查找；`MP-42` 改为「多个 `-s` 中含外部策略时拒绝」并登记 `DEFER-MP-03`（`#1027` Expected behavior 第 3 条延后、关闭时注明），`ADR-MP-02`、非目标、故障恢复矩阵同步，`MP-15`/`MP-37` 的承接说明随之改写；② 按 G-09 自 `MP-04` 拆出 `MP-49`（octopus 默认消息：`and` 列举、`--into-name`、`merge.suppressDest`），自 `MP-19` 拆出并前置 `MP-50`（`libra rm` 移除未合并路径的全部阶段），「使用规则」改为拆出卡与来源卡相邻、需先行者排在来源卡之前；③ `MP-33` AC8 与映射改为同一谓词（`978.md` 修订历史行归 ER-06 文档记账）；`MP-02` AC2 拆为索引与工作树两条；`MP-11` AC2 拆为暂存 `bar` 与保留 `foo`；`MP-12` AC4 收窄为 `status --porcelain` 谓词；`MP-41` 补索引断言；④ `MP-41`/`MP-45` 纳入 `commit` 文档与 `src/command/commit.rs`；`MP-10`/`MP-43`/`MP-39`/`MP-30` 的承接卡指向改正（`MP-48`、`MP-45`）；`MP-06` 夹具不再推进上游并写明 D2 | 拆分：`MP-04` → `MP-04`, `MP-49`；`MP-19` → `MP-50`, `MP-19` | 使用规则、非目标、GAP、ADR-MP-02、DEP-MP-04、Phase 1/3/4、审计表、追溯表、故障恢复矩阵、DEFER |
| 2026-11-18 15:36:56 UTC | 双审 R9：Claude FAIL（1×P1）、Codex FAIL（5 分区：A 1×P1、B 4×P1、C 1×P1、D 3×P1、E 4×P1），受审 SHA-256 `ce0ee67e5f45…` | ① 「使用规则」改为拆出卡与来源卡组成连续卡片组（`MP-35`→`MP-38`→`MP-15`→`MP-51`→`MP-37`→`MP-42` 等），`MP-30` 的 G-09 标记移入 Description、`MP-19` 补「拆出 `MP-50`」；② G-03 口径允许在退出码卡内以 `LIBRA_FINE_EXIT_CODES=1` 另列覆写优先 AC；`MP-27` 三次快照、`MP-34` 四个稳定码、`MP-40` 两个稳定码各拆为单条 AC（`MP-34` 删去与 ER-06 文档门重复的 `error-codes.md` AC 与 fine-exit AC，后者由 `MP-22` 验收）；`MP-26` 增加未覆盖配置的对照 AC；`MP-09` 增加提交树断言；③ 按 G-09 自 `MP-15` 拆出 `MP-51`（`--json`/`--machine` 输出隔离；`MP-15` 版本在该模式过渡拒绝），`MP-15` 退出 1/2 回滚拆为两条、`PATH=.` 场景改为 `t7606#3` 等价脚本并断言结果树等于 `c2^{tree}`，登记 Libra 仓库策略脚本须用 `libra` 命令的有意差异；`MP-35` 为每个被拒绝名字放置可被误执行的同名程序；链中未找到的名字移入 `MP-38`（AC6/AC7），`MP-42` 以 stdout 无 `Trying merge strategy` 与 `<bin>/ran` 不存在证明未尝试任何策略；④ `MP-19` 改为文本走内容合并、二进制取各侧 blob（`merge-ort.c:3020-3068`），新增文本 1to2 AC8，登记被取代的既有 1to2 断言与 `rename_conflict` 库测试回归组；`MP-50` 增加只有 stage 1 的路径（`update-index --index-info` 建立） | 拆分：`MP-15` → `MP-15`, `MP-51` | 使用规则、任务卡粒度规则、ADR-MP-02、GAP、Phase 3、审计表、追溯表 |
| 2026-11-18 15:42:38 UTC | 双审 R10：Claude FAIL（2×P1、1×P2）、Codex FAIL（5 分区：A 1×P1+1×P2、B 1×P1+1×P2、C 2×P1、D 2×P1、E 2×P1），受审 SHA-256 `fdc21929b5ee…` | ① 审计表与 `Granularity` 的 `landing` 改为按生产文件的不同目录数自动计算（模板「落点」= 一个具体目录）；② `MP-02` 增加 `merge.ff=false` 的待提交路径与 `--continue`（AC7/AC8）；`MP-09` AC5 文字与映射一致；`MP-07` 的承接卡分别指向 `MP-10`/`MP-43`/`MP-39`；③ `MP-15` 不再称 `read-tree` 脚本与 `t7606#3` 等价：以只更新索引的脚本验收 `#1027` AC1 的结果树与父提交，`read-tree --reset -u` 与 `t7606#3` 工作树断言登记 `DEFER-MP-04`（非目标、`#1027` 关闭说明同步）；`MP-38`/`MP-42` 改以「目标无法解析时仍先报未找到/组合拒绝」证明先于目标解析与任何策略尝试；④ `MP-25` 以工作树路径清单判定无残留（`ls-files -o` 受 `#1017` 影响）；`MP-19` 写明文本 1to2 的工作树内容由既有用例断言守护 | 无 | 审计表、全部卡 `Granularity`、非目标、DEFER、`#1027` 关闭说明 |
| 2026-11-18 15:48:37 UTC | 双审 R11：Claude FAIL（2×P1）、Codex FAIL（5 分区：A PASS+1×P2、B 1×P1、C 2×P1、D 2×P1、E 1×P1），受审 SHA-256 `d7aef206e84e…` | ① 「验证执行协议」第 8 条：Σ(a) 改为引用/配置/待决操作状态表白名单（`reference`、`config`、`config_kv`、`rebase_state`、`cherry_pick_state`、`revert_sequence`、`sequence_state`、`bisect_state`），操作日志/头、缓存、`sqlite_sequence` 与 AI 表不在 Σ 内；新增「工作树清单」定义并排除测试辅助的 `.libra-test-home/`（`tests/command/mod.rs:353-361`），Σ(c) 改用该清单；Σ(d) 另排除操作日志指针 `info/operation-pointer.json`；② `MP-02` AC3、`MP-25` AC5 改为完整「工作树清单」（含目录与类型）；③ `MP-07` JSON 冲突用例以 stderr JSON 的 `error_code` 为执行条件；`MP-39`/`MP-45` AC1 以 `LBR-CONFLICT-002` 为执行条件；④ `MP-13` AC2 写明沿用 AC1 变基后的仓库（夹具说明同步）；`MP-38` AC6 只陈述诊断，先于目标解析的顺序由 AC7 判定 | 无 | 验证执行协议第 8 条、Phase 1/2/3/4 相关卡 |
| 2026-11-18 15:53:42 UTC | 双审 R12：Claude FAIL（4×P1）、Codex FAIL（5 分区：A 1×P1、B 2×P1、C 1×P1、D 1×P1+1×P2、E 1×P1），受审 SHA-256 `b05405d19f29…` | ① Σ(a) 白名单去掉已被 `2026070401_sequence_state.sql:24-25` 删除的 `cherry_pick_state`/`revert_sequence`（以 `sqlite_master` 核对其余六表存在）；② 按 G-09 自 `MP-03` 拆出 `MP-52`（编辑器无法启动时同样保留可恢复合并，Git 2.54.0 预探 `GIT_EDITOR=/nonexistent/editor`），`MP-03` 收窄为非 0 退出；`MP-04` AC8 改在分离 HEAD 上验收 `#999` AC1 的 `--into-name=main`（并覆盖 `master`）；③ `MP-10`/`MP-43`/`MP-39`/`MP-41`/`MP-45` 的工作树断言改为完整「工作树清单」，F-991C 写明不新增 `h`；④ F-EXT 脚本以 `set -e` 开头，失败脚本改用 `<libra 绝对路径> read-tree` 并记录 `<bin>/tree-<name>`，`MP-15` AC6/AC7 以其等于 `c2^{tree}` 为执行条件；`MP-50` AC6 以 `printf '... %s ...' <k>` 传入 OID | 拆分：`MP-03` → `MP-03`, `MP-52` | 验证执行协议第 8 条、GAP、Phase 1、审计表、追溯表 |
| 2026-11-18 15:58:17 UTC | 双审 R13：Claude FAIL（3×P1）、Codex FAIL（5 分区：A 1×P1、B 1×P1、C 1×P1、D 1×P1、E 1×P1），受审 SHA-256 `d1c6c862d615…` | ① GC-MP-01 的 JSON 约束改为「成功时 stdout 一个 JSON 文档；失败时 stdout 为空、错误 JSON 写到 stderr；`--machine` 为 NDJSON」；② `MP-52` 写明 Libra 经 `sh -c` 启动编辑器，分别验收「编辑器程序不存在（shell 退出 127）」与「`PATH` 中无 `sh`（进程创建失败）」两种形态（加入 `src/command/editor.rs`）；③ `MP-45` 登记 `#981` AC4 与 pinned Git 不符、按 Git 保存在 `MERGE_AUTOSTASH`、提示沿用 Git 原文与恢复路径（`COMPATIBILITY.md` 必改，`#981` 关闭说明同步）；④ `MP-51` AC3 改为 `--machine` 的 stdout 每行可解析为 JSON；`MP-50` 同卡更新 merge 文档与网站页中「`libra rm` 不接受未合并路径」的恢复指引 | 无 | GC-MP-01、Phase 1/2/3/4 相关卡、关闭说明 |
| 2026-11-18 16:04:36 UTC | 双审 R14：Claude FAIL（3×P1）、Codex FAIL（5 分区：A 1×P1、B 2×P1、C 2×P1、D 3×P1、E 3×P1），受审 SHA-256 `1bcdb9f2940a…` | ① 故障恢复矩阵：外部策略中断后的恢复改为 `libra restore --staged --worktree --source HEAD -- .`（无待决状态，与 Git 一致；不再指示 `merge --abort`）；② `MP-52` 形态 B 增加 `libra commit` 完成合并的 AC8；`MP-27` 删去未验收的稳定码承诺；`MP-29` 夹具写明身份与两次实际改动；`MP-45` 的查看命令按发布顺序改为读 sidecar OID 后 `libra show`（`MP-44` 之后另可 `stash show`）；`MP-23` 改为删除并确认 `branch.main.*`；③ `MP-11` 收窄为未暂存修改，按 G-09 拆出 `MP-53`（已暂存修改仍拒绝、冲突合并 `--abort` 保留无关修改，Git 2.54.0 预探），F-993 增加 `conflict_branch`；`MP-18` 增加 `mkdir one` 未跟踪目录变体（AC6/AC7）；④ 按 G-09 自 `MP-19` 拆出 `MP-54`（`#1021` 二进制各侧 blob），`MP-19` 增加 F-1018 的完整 `ls-files -s` 断言；`MP-16` 改正 diff3/zdiff3 理由并登记 `DEFER-MP-05` | 拆分：`MP-11` → `MP-11`, `MP-53`；`MP-19` → `MP-19`, `MP-54` | 非目标、GAP、Phase 2/4、审计表、追溯表、故障恢复矩阵、DEFER |
| 2026-11-18 16:34:19 UTC | 双审 R15：Claude FAIL（3×P1）、Codex FAIL（5 分区：A 1×P1、B 1×P1、C 1×P1、D 2×P1、E 2×P1），受审 SHA-256 `6b1d68679dc7…`；随后按 R1–R15 反复出现的层面做全文专项清扫（issue 原文对照、夹具可运行性与状态隔离、`--json` 失败契约、快照定义与实际 schema、测试 HOME） | ① 故障恢复矩阵：外部策略中断后分别恢复已跟踪路径（`restore`）与策略新建的未跟踪路径（逐路径 `clean -f -- <path>`，禁止不带路径的 `clean`）；② `MP-26` AC2/AC6 改为以 `libra commit` 完成待决合并并逐字核对 `tree`/`parent` 行；`MP-08` AC3–AC5 各自重建夹具并确认确为快进路径；`MP-15` AC8 与「任务卡粒度规则」改为在 `--json` 下解析 stderr 的错误 JSON 断言 `error_code`；③ `MP-51` 改为捕获子进程两路输出、成功时写入 `data.strategy_output`、失败时写入错误报告 `details.strategy_output`，新增 `--json`/`--machine` 失败用例（AC5–AC8），`ADR-MP-02` 同步；④ `MP-50` 把 `docs/error-codes.md` 列入语义核对并钉住 `a` 的 stage 1/2 前置条件；其余卡的 `docs/error-codes.md` N/A 理由集中到 GC-05 条并按实际 `rg` 结果改写；⑤ issue 原文对照：`MP-01` 增加 `#996` 的 `--no-ff --ff-only` 快进、`--ff --no-ff` 与 `merge.ff=only`+`--no-ff`（AC5–AC7）；`MP-33` 改为两种参数顺序与 Σ 不变（`#985` AC2），以 `status --porcelain` 单一输出合并「放行且暂存」两条；`MP-21` 增加 `merge.ff=only`、`pull.ff=only`（`#1002`，AC7/AC8）；`MP-28` 增加 `--strategy=` 与空格分隔 `-X`（`#1015`，AC5/AC6）；`MP-06` 增加 `--ff-only` 形态与非 unborn 对照（`#1003`，AC4/AC5），分支 reflog 断言不再依赖 Libra 显示为 `HEAD@{n}` 的选择子，无参数 `libra merge` 登记 `DEFER-MP-06`；`MP-14` 以一次 `serde_json` 比较合并状态表示两条并增加单目标 `-s octopus`（`#1025` AC1）；`MP-05` 增加 `commit -a`，`#980` 的默认清理依赖登记 `DEP-MP-13`（`CS-14`）；⑥ 夹具：F-* 统一 `libra config vault.signing false`（以子目录为 cwd 的合并在测试 HOME 下找不到 vault 密钥，Libra 0.30.207 实测 `LBR-AUTH-001`）；「验证执行协议」第 8 条新增「映射的执行仓库」与「夹具提交名」（轻量标签）约定，并据此为 `MP-37` AC5、`MP-16` AC3、`MP-11` AC6、`MP-09` AC5、`MP-07` AC3、`MP-14` AC2 显式重建；「工作树清单」更正 `.libra-test-home/` 的实际内容（含 vault 密钥）并排除任一层级的该目录；Σ(a) 改为全部表减去日志/缓存/AI 排除表，Σ(d) 排除 SQLite 旁路文件 | 无 | 「事实基线」后的非目标与成功定义、ADR-MP-02、GC-05、验证执行协议第 8 条、任务卡粒度规则、依赖登记表（`DEP-MP-13`）、故障恢复矩阵、DEFER（`DEFER-MP-06`）、`MP-01`/`MP-26`/`MP-33`/`MP-28`/`MP-21`/`MP-05`/`MP-06`/`MP-08`/`MP-14`/`MP-15`/`MP-51`/`MP-50` 等卡 |
| 2026-11-18 16:41:53 UTC | 双审 R16：Claude FAIL（5×P1，其中 4 项与 Codex 相同）、Codex FAIL（5 分区：A PASS、B 1×P1、C 1×P1、D 1×P1、E 1×P1），受审 SHA-256 `501fb9781612…` | ① `MP-33` AC5/AC6 改以一份 `status --porcelain=v2 --branch --untracked-files=no` 输出（按行集合比较）同时判定 HEAD 仍为 `B` 与暂存的 `g` blob；② `MP-14` AC1 以表驱动覆盖单/双目标 `-s octopus`，新增 AC8：`-s ort -s ours` 在 `ort` 因未跟踪文件将被覆盖而未处理后由 `ours` 实际完成合并（Git 2.54.0 预探），AC6 文字收窄为诊断行；③ `MP-19` AC5、`MP-40` AC2、`MP-07` AC7、`MP-05` AC2 显式新建所属夹具（「验证执行协议」第 8 条规则 (iv)），pull 用例首条映射写明夹具；④ `MP-48` 的文档集加入 `commit` 命令页（`libra commit` 的输出改变）；⑤ F-SQ 改为作者≠提交者且日期/时区不同（显式设置 `GIT_AUTHOR_*`，因 Libra 作者身份会回落到 `GIT_COMMITTER_*`） | 无 | `MP-05`、`MP-07`、`MP-14`、`MP-19`、`MP-33`、`MP-40`、`MP-48` |
| 2026-11-18 16:47:43 UTC | 双审 R17：Claude FAIL（3×P1，含 2 项与 Codex 相同，另驳回 1 项）、Codex FAIL（5 分区：A 1×P1、B 1×P1、C PASS+1×P2、D 1×P1、E PASS），受审 SHA-256 `71d3fff48665…` | ① 故障恢复矩阵：删除策略新建的未跟踪路径一律用 `--literal-pathspecs`，文件先 `-n` 确认恰为该路径，目录先列出其下全部未跟踪文件并确认全属策略再 `-f -d`；② Σ(b) 改为 `libra ls-files -s` 的完整输出（不再比较含 stat 缓存的 `.libra/index` 原始字节）；③ `MP-33` 按「不同语法形式拆为独立 AC」把两种参数顺序与各自的 Σ 拆开（稳定码由 `MP-26` 用例 AC5 在共享回归组中守护，不另立 AC）；④ 按 G-09 自 `MP-14` 前置拆出 `MP-55`（`-s octopus` 显式选择：双目标三父、单目标双父与 `ADR-MP-05` 状态表示），`MP-14` 收窄为策略链（6 条 AC）；`ADR-MP-05`、GAP、Phase 3、风险登记、使用规则同步；⑤ 每卡映射头部显式引用「夹具提交名」「映射的执行仓库」；Codex D 关于 `M` 不可解析的发现按第 8 条「夹具提交名」驳回 | 拆分：`MP-14` → `MP-55`, `MP-14` | 实施顺序、Phase 3、审计表、追溯表、ADR-MP-05、风险登记、故障恢复矩阵、验证执行协议第 8 条 |
| 2026-11-18 16:52:42 UTC | 双审 R18：Claude FAIL（3×P1，含 2 项与 Codex 相同）、Codex FAIL（5 分区：A PASS、B 2×P1、C PASS、D 2×P1、E PASS），受审 SHA-256 `05957f99f7fe…` | ① 修正生成器转义：`MP-28` AC5/AC6、`MP-14` AC1/AC6 与 `MP-50` F-RMU 中的期望文本恢复为字面 `\n`/`\t`；② `MP-26` 把「不创建提交」（HEAD 仍为 `M`）与「随后 `libra commit` 完成为合并提交」拆为 AC2/AC3 与 AC7/AC8；③ `MP-33` 收窄为拒绝路径（两种参数顺序、配置后校验与各自 Σ，5 条 AC），`--squash` 与最终生效 `--no-commit` 的放行登记 `DEFER-MP-07` 并在 `COMPATIBILITY.md` 记为有意差异，不再修订 `issues/978.md`（其 fail-closed 口径保持有效）；④ `MP-55` 增加单目标 `-s octopus` 在两侧都修改同一文件时输出 `Merge with strategy octopus failed.` 并保持 Σ 不变（AC4/AC5，Git 2.54.0 预探），区分被忽略的 `-s octopus`；⑤「夹具提交名」增加例外：pull 远端夹具中由 `git` 创建的命名提交以创建时记录的完整 OID 指代 | 无 | `MP-26`、`MP-33`、`MP-55`、`MP-28`、`MP-14`、`MP-50`、验证执行协议第 8 条、非目标、DEFER、与其它计划的关系、`DEP-MP-05` |
| 2026-11-18 16:57:05 UTC | 双审 R19：Claude FAIL（3×P1，均与 Codex 相同，其中 1 项改用与 Git 一致的修复）、Codex FAIL（5 分区：A PASS、B 1×P1、C PASS、D 1×P1、E 1×P1），受审 SHA-256 `995b5c0c5967…` | ① `MP-02` AC6/AC8 改为逐字核对完成提交的 `tree`/`parent` 行（树须为 `T` 的树）；② `MP-20` 增加对照夹具 F-1024N 与 AC5：目录重命名后真正新增的文件仍报告 `added in` 文案（Git 2.54.0 预探）；③ `MP-55` 明确单目标显式 octopus 只尝试平凡合并：带 `--no-commit` 时与 Git 一样以 `Merge with strategy octopus failed.` 拒绝且 Σ 不变（AC6/AC7，Git 2.54.0 预探、`builtin/merge.c:1699,1725`），只有多目标会停在提交前（AC3）；④「与其它计划的关系」与 `DEP-MP-08` 登记 `commit.rs` 在 `MP-10` 之后仍由 `MP-41`、`MP-45` 修改，并注明本计划不写 `ls_files.rs` | 无 | `MP-02`、`MP-20`、`MP-55`、与其它计划的关系、依赖登记表 |
| 2026-11-18 17:01:04 UTC | 双审 R20：Claude FAIL（2×P1，均与 Codex 相同）、Codex FAIL（5 分区：A PASS、B PASS、C 1×P1、D 1×P1、E PASS），受审 SHA-256 `f49c7a283593…` | ① `MP-48` 增加由 `libra merge --continue` 完成合并时的应用冲突路径（AC4–AC6：提示、`stash list` 中的 autostash 与 sidecar 删除），写集加入 `src/command/merge.rs`；② `MP-55` 把单目标 `--no-commit -s octopus` 的拒绝限定为非快进历史，新增 AC8：可快进时与 Git 一样快进（`builtin/merge.c:1655-1692`，Git 2.54.0 预探） | 无 | `MP-48`、`MP-55` |
| 2026-11-18 17:03:44 UTC | 双审 R21：Claude FAIL（2×P1，均与 Codex 相同）、Codex FAIL（5 分区：A PASS、B 1×P1、C PASS、D 1×P1、E PASS），受审 SHA-256 `505b4489dba4…` | ① `MP-28` AC6 改为逐字核对完成提交的 `tree`/`parent` 行（`-X ours` 的结果与 `M` 内容相同，只比内容测不出未执行合并）；② `MP-55` 把单目标非快进的拒绝条件改为「平凡索引合并无法处理」（如两侧把同一文件改成不同内容），并注明两侧改成相同内容属可处理情形（`builtin/merge.c:743-770,1699-1735`）；AC4 文字同步 | 无 | `MP-28`、`MP-55` |
| 2026-11-18 17:06:42 UTC | 双审 R22：Claude FAIL（2×P1，其中 1 项与 Codex 相同）、Codex FAIL（5 分区：A PASS、B PASS、C PASS、D 1×P1、E PASS），受审 SHA-256 `cd6321191cc2…` | ① `MP-13` 对既有用例 `test_pull_ff_only_conflicts_with_rebase_at_parse_time` 的改写改为「解析通过、在空目录的仓库前置检查以 `LBR-REPO-001` 失败」（该用例没有仓库与分叉历史；非快进拒绝文案仍由 AC4/AC5 判定）；② `MP-35`、`MP-38` 登记既有用例 `merge_strategy_unknown_name_lists_supported_values`（`tests/command/merge_test.rs:4002-4016`）的按第 5 条改写：先改为 `LBR-UNSUPPORTED-001`，再改为未找到诊断，保留 HEAD 不变断言；③ 同类清扫：`MP-11` 登记三个依赖「任何未提交修改都拒绝」或旧文案的既有用例及其按第 5 条的逐个判定规则 | 无 | `MP-13`、`MP-35`、`MP-38`、`MP-11` |
| 2026-11-18 17:10:19 UTC | 双审 R23：Claude FAIL（1×P1，与 Codex 相同）、Codex FAIL（5 分区：A PASS、B PASS、C PASS、D PASS、E 1×P1），受审 SHA-256 `9507ad77c691…` | `MP-54` 把「各侧记录原始 blob」限定为 Git `was_binary_blob` 的条件（内容合并不干净且结果的模式与 OID 等于 HEAD 侧，`merge-ort.c:3028-3053`）；内容合并干净的二进制 1to2 与文本 1to2 仍为同一结果，`tests/command/merge_test/rename_binary.rs:117-121` 的干净对照与模式对照断言保留，只改写被取代的断言 | 无 | `MP-54` |
| 2026-11-18 17:12:44 UTC | 双审 R24：Claude FAIL（1×P1，与 Codex 相同）、Codex FAIL（5 分区：A PASS、B PASS、C PASS+1×P2、D 1×P1+1×P2、E PASS），受审 SHA-256 `8855addddce0…` | ① `MP-30` AC2 的稳定码依据改为 `src/command/rev_parse.rs:643-645,997-1006`（`rev_parse_target_error` 的 `InvalidReference` 分支）；② P2：F-EXT 的 `git-merge-theirs` 改为读取最后一个参数（带 `-X` 时 `$4` 为 `HEAD`）；`MP-09` AC4/AC6/AC7 写明在 `M` 上执行 | 无 | `MP-30`、F-EXT（`MP-35`/`MP-38`/`MP-15`/`MP-51`/`MP-37`/`MP-42`）、`MP-09` |
| 2026-11-18 17:15:55 UTC | 双审 R25：Claude FAIL（3×P1，均与 Codex 相同）、Codex FAIL（5 分区：A PASS、B PASS、C 1×P1、D 1×P1、E 1×P1），受审 SHA-256 `bd993c9bd8de…` | ① `MP-10` AC3、`MP-43` AC1 与同型的 `MP-41` AC6 改为逐字比较 `libra ls-tree -r HEAD`（`file`=`R15` 与 `h`=`m1\n`，测出缺路径的提交）；② `MP-15` Security 改为如实描述 argv：固定字面量、完整 OID 与用户 `-X` 值（各为独立 argv 元素、不经 shell）；③「验证执行协议」第 6 条允许 `run_libra_command_with_stdin`/`run_libra_command_with_stdin_and_env`（`tests/command/mod.rs:417-460`），`MP-50` AC6 改用前者向 `update-index --index-info` 写入记录；④ 同类清扫：`MP-03` AC5/AC7 改为逐字比较完成提交的 `ls-tree -r`，`MP-52` AC4/AC8 改为逐字比较 `tree`/`parent` 行 | 无 | `MP-10`、`MP-43`、`MP-41`、`MP-15`、`MP-50`、`MP-03`、`MP-52`、验证执行协议第 6 条 |
| 2026-11-18 17:19:16 UTC | 双审 R26：Claude FAIL（2×P1，均与 Codex 相同）、Codex FAIL（5 分区：A 1×P1、B PASS、C PASS、D 1×P1、E PASS），受审 SHA-256 `018e5f55072f…` | ① 网站第 2 步 ② 与「文档职责」改为按 ER-06a（`plan-template.md:332`）在不处于 `cf` 时执行 `libra switch cf` 后重检，切换受阻才 STOP；② `MP-51` 明确子进程输出按 UTF-8 有损解码为文本（非 UTF-8 字节替换为 U+FFFD，写入 merge 文档的公开契约），AC2/AC6 文字由「原样记录」改为「解码后的文本」 | 无 | 文档与网站验收命令第 2 步、文档职责、`MP-51` |
| 2026-11-18 17:22:41 UTC | 双审 R27：Claude FAIL（2×P1，均与 Codex 相同）、Codex FAIL（5 分区：A PASS、B 1×P1、C 1×P1、D PASS、E PASS），受审 SHA-256 `7602a46ee95c…` | ① `DEP-MP-04` 改为硬前置：`I974-01` 的标题装饰（`Merge branch '<x>'`）须先合入，`MP-04` 记录其以 Git 的 `merge.suppressDest` 模式匹配取代 `I974-01` ADR「目的地=当前分支即省略后缀」的规则并按第 5 条改写相关断言，Phase 1 进入条件同步；② `MP-39` AC4 改为逐字比较完整 `libra ls-files -s`；③ P2：网站第 2 步 ② 的分支判定改为 `libra --offline symbolic-ref --short HEAD` 恰为 `cf`（`## cf` 前缀也匹配 `cfx` 等分支） | 无 | `DEP-MP-04`、`MP-04`、Phase 1、`MP-39`、文档与网站验收命令第 2 步 |
| 2026-11-18 17:26:01 UTC | 双审 R28：Claude FAIL（4×P1，均与 Codex 相同）、Codex FAIL（5 分区：A 1×P1、B PASS+1×P2、C PASS、D 2×P1、E 1×P1），受审 SHA-256 `0ffbb11a4b93…` | ① 故障恢复矩阵的目录检查也用 `--literal-pathspecs`，并要求列出的每一行以 `<dir>/` 开头；② `MP-23` AC1/AC3/AC5/AC6 与 `MP-13` 的「变基结果」「合并结果」改为读取 `main` 分支（`libra cat-file -p main`），AC2/AC4 与 `MP-13` AC2 改读 `main:<path>`；③ `MP-51` 夹具输出非 UTF-8 字节 0xFF，AC2/AC6 期望 U+FFFD；④ `MP-19` AC4 改为逐字核对 `tree`/`parent <C>`/`parent <B>`；⑤ 同类清扫：`MP-15` AC5 改为逐字核对 `tree <c2^{tree}>`/`parent <c1>`/`parent <c2>`（`#1027` AC1 同时要求父提交） | 无 | 故障恢复矩阵、`MP-13`、`MP-23`、`MP-51`、`MP-19`、`MP-15` |
| 2026-11-18 17:28:44 UTC | 双审 R29：Claude FAIL（2×P1，均与 Codex 相同）、Codex FAIL（5 分区：A 2×P1、B PASS、C PASS、D PASS+1×P2、E PASS），受审 SHA-256 `f59dc9c44170…` | ① 网站第 2 步 ② 在分支判定后显式执行 `libra status --short --branch`（须退出 0 且首行以 `## cf...origin/cf` 开头，两条命令的退出码与输出写入记录）；② 故障恢复矩阵的目录检查改为分别核对短格式的 `??` 状态列与去掉前三个字符后的路径前缀 `<dir>/` | 无 | 文档与网站验收命令第 2 步、故障恢复矩阵 |
| 2026-11-18 17:31:05 UTC | 双审 R30：Claude PASS、Codex PASS（5 分区 A–E 全部 PASS），受审 SHA-256 `a20987cf5fa56689e293c2f7ed6b8bde03d084220bd8b625d85e91b2712e6aeb` | 记账：只更新头部「状态」行并追加本行与「Codex review log」的 R30 行，其余字节与受审字节相同 | 无 | 状态行、修订历史、Codex review log |

## 已决议设计决策

### ADR-MP-01: 冲突与拒绝合并的 Git 退出码经 `with_exit_code` 落地

- **Status:** Accepted（2026-11-18 按 Git 2.54.0 预探与 pinned 源码重写决策细节）
- **Context:** Git 对 `merge`/`pull` 的退出码按路径区分（`git@6de20f6092` `builtin/merge.c`，本机 Git 2.54.0 预探）：内容/树冲突 `1`；fast-forward 检出被本地改动或未跟踪文件阻挡 `1`；三方合并被「would be overwritten」阻挡时策略失败 `2`（伴随 `Merge with strategy <s> failed.`）；所有 `-s` 策略均未处理 `2`；`--ff-only` 无法 fast-forward、参数与状态错误 `128`。Libra 当前把这些合并错误统一映射为 `CliError::failure` + `LBR-CONFLICT-002`（默认 128，`src/command/merge.rs:1014-1028`、`src/command/pull.rs:1216-1229`）。`CliError::with_exit_code` 已优先于 `LIBRA_FINE_EXIT_CODES` 与默认映射（`src/utils/error.rs:1061-1076`，`docs/error-codes.md:60-62`）。
- **Decision:** `MP-22`（冲突 → `1`）、`MP-34`/`MP-40`（`merge`/`pull` 的 FF 阻挡 → `1`、三方阻挡 → `2`）、`MP-14`（策略链全部未处理 → `2`）、`MP-38`（未找到策略 → `1`）、`MP-37`（外部策略冲突 → `1`、未处理 → `2`）在 `merge` 与 `pull` 的错误映射处用 `with_exit_code` 设定 Git 退出码，稳定错误码保持不变；`--ff-only` 拒绝维持默认 `128`（`MP-21`）。每处覆写同卡登记到 `docs/error-codes.md`「Per-command exit-code overrides」小节。
- **Alternatives considered:** 修改 `ConflictOperationBlocked` 的全局默认退出码——拒绝，会改变非 merge/pull 命令的退出码。
- **Consequences:** 上述卡修改 `docs/error-codes.md` 的退出码契约；数值退出码只在退出码本身是行为轴的卡（`MP-22`、`MP-34`、`MP-40`、`MP-14`、`MP-38`、`MP-37`）以单列 AC 断言（「任务卡粒度规则」）。
- **Revisit when:** 全仓统一梳理 Git 退出码对齐矩阵时。

### ADR-MP-02: 外部自定义策略 `git-merge-<name>` 的执行边界（无超时，与 Git 一致）

- **Status:** Accepted（2026-11-18 修订：撤销可选超时与进程组/Job Object 回收设计，见「修订历史」）
- **Context:** `#1027` 要求 `-s <name>` 在非内置时运行 `PATH` 上的 `git-merge-<name>`，验收场景（`t7606#3`）用 `PATH=.:$PATH` 从当前目录解析策略脚本。Git 以 `try_merge_command`（`git@6de20f6092` `merge.c`）同步运行策略、argv 为 `merge-<name> [--<xopt>...] <base>... -- HEAD <remote>...`、不设超时，返回后重读索引；未知策略输出 `Could not find merge strategy '<name>'.` 并以 `1` 退出（`builtin/merge.c` `get_strategy`）。Libra 已有的 `merge.<driver>.driver` 外部驱动同样以 `.status()` 同步等待、无超时（`src/command/merge.rs:5654-5657,5690-5694`）。
- **Decision:** 策略名只接受 `[A-Za-z0-9_.-]+` 且不得等于 `.`/`..`（`MP-35`，先于任何 `PATH` 查找）。外部策略以 `std::process::Command::new(format!("git-merge-{name}"))` 按标准 `PATH` 查找（含 `.`），不经 shell，argv 与 Git 的 `try_merge_command` 相同（各 `-X` 选项以 `--<xopt>` 开头，随后为各合并基 OID、`--`、字面量 `HEAD` 与各目标 OID），工作目录为工作树根，以 `Command::status()` 同步等待并回收子进程（GC-09 的释放语义）；人读模式继承 stdout/stderr；`--json`/`--machine` 模式以 `Command::output()` 捕获子进程的 stdout 与 stderr、不转发到 Libra 的任何输出流，成功时写入 merge JSON 数据的 `strategy_output` 对象、失败时写入错误报告的 `details`，使成功时 stdout 只含 Libra 的 JSON、失败时 stderr 只含一个错误 JSON 文档（GC-MP-01；`MP-51`；`MP-15` 版本在该模式下过渡拒绝）；Libra 仓库没有 `.git`，策略脚本须以 `libra` 命令操作索引（`MP-15` 记为有意差异）；返回后重读索引：`0` = 干净（以索引写树并提交），`1` = 冲突（进入可 `--continue`/`--abort` 的冲突状态），其它 = 未处理（恢复 pre-merge 索引与工作树，单策略时输出 `Merge with strategy <name> failed.` 并以 `2` 退出）；多个 `-s` 中含外部策略时，在全部名字通过白名单与查找之后、任何策略运行之前以 `LBR-UNSUPPORTED-001` 拒绝（`MP-42`、`DEFER-MP-03`）：pinned Git 的 `get_strategy` 找到首个自定义策略后即释放命令列表，第二个自定义策略无法查找（`git@6de20f6092` `builtin/merge.c:228-229`），而「一个外部策略 + 内置策略」链的回绕、候选评分与末位结果复用语义（`builtin/merge.c:1786-1860`）不在 `#1025` 的验收与 `#1027` 的 Acceptance criteria 之内（`#1027` Expected behavior 第 3 条提出，延后实现）。不增加 Git 没有的超时、进程组或 Windows Job Object 机制；父进程被中断时与 Git 一致依赖终端对前台进程组的信号投递。
- **Alternatives considered:** 外部策略参与多策略链（按 `builtin/merge.c:1786-1860` 回绕、评分并复用末位结果）——延后（`DEFER-MP-03`）：仅 `#1027` Expected behavior 第 3 条提出、两个 issue 的验收标准均未要求，Git 自身不能组合两个外部策略，候选评分与重跑语义需要额外的可区分夹具；跳过 `PATH` 中的 `.`——拒绝，直接破坏 `t7606#3`；shell 拼接执行——拒绝，存在命令注入风险；默认或可选超时 + 进程组/Job Object 回收（2026-11-17 R7–R22 设计）——撤销：Git 无此机制、`#1027` 未要求，且其 Windows 证据需要本地不可得的隔离 Windows 环境，使整个外部策略家族长期阻塞。
- **Consequences:** `MP-35`、`MP-38`、`MP-15`、`MP-51`、`MP-37`、`MP-42` 六张独立卡交付；外部策略卡住时用户以中断终止（与 Git 一致），登记于「风险登记」。
- **Revisit when:** 引入 Libra 原生插件策略接口，或有用户报告外部策略挂起导致的实际问题时。

### ADR-MP-03: 全计划严格串行化以消除 G-10 写集冲突

- **Status:** Accepted
- **Context:** 计划内各卡共享 `src/command/merge.rs`、`src/command/pull.rs`、`COMPATIBILITY.md`、`docs/commands/merge.md` 与网站页。
- **Decision:** 取消一切实现阶段并发，全部 53 张任务卡按「实施顺序」的线性 DAG 串行执行并逐卡发布（同一时刻只有一张卡持有发布窗口）；与同批其它计划相交的写集按 `DEP-MP-10` 取得协调者串行窗口（G-10、ER-12）。
- **Alternatives considered:** 拆分 `COMPATIBILITY.md` 由单一汇总卡修改——拒绝（违反 ER-06a 同卡同步要求）。
- **Consequences:** 零 `I–I` 并发冲突，每张卡独立可 review、可发布、可前滚。
- **Revisit when:** 后续拆分独立子命令文件时。

### ADR-MP-04: `MERGE_AUTOSTASH` 单一事实源与 `rev-parse` 显式接入（`#991`）

- **Status:** Accepted
- **Context:** `src/internal/pseudo_ref.rs:13-32` 规定伪引用不单独持有状态，按需从既有 sidecar 或数据库行派生；`rev-parse` 只对显式 opt-in 路径解析伪引用；`COMPATIBILITY.md:277` 与 `rev-parse` 文档目前只记录 `AUTO_MERGE`。Git 在合并直接完成时（`finish()` 收到新 HEAD，含可快进的 `--squash`）立即应用 autostash；在停于提交前时（冲突、`--no-commit`、三方 `--squash`）把它保留为 `MERGE_AUTOSTASH`、不进入 `stash list`，由随后完成合并的 `commit`/`merge --continue` 或 `merge --abort` 应用（`git@6de20f6092` `builtin/merge.c:495-496,539-541,1690,1860-1877`、`builtin/commit.c:1982`；本机 Git 2.54.0 预探）。
- **Decision:** 保留 `merge-autostash.json` 作为 `MERGE_AUTOSTASH` 的唯一事实源：停在提交前的普通与三方 `--squash` 合并的 autostash 都只记录在该 sidecar，`MP-10` 在 `commit`、`MP-43` 在 `--continue` 完成时（应用冲突时转存 `stash list` 由 `MP-48` 承接）、`MP-39` 在 `--abort` 时、`MP-41`/`MP-45` 在干净/冲突三方 squash 后的 `commit` 时应用并清理（可快进的 `--squash` 按 Git 立即应用，`MP-39`）；`MP-30` 在 `pseudo_ref.rs` 新增按需读取该 sidecar 的 `MergeAutostash` 变体（与 `AUTO_MERGE` 同型），并在 `rev-parse` 显式 opt-in 接入，同步 `tests/compat/pseudo_ref_surface.rs`、`COMPATIBILITY.md` 与 `rev-parse` 文档；`MP-44` 让 `stash show` 经同一伪引用读取它。
- **Alternatives considered:** 由 `pseudo_ref.rs` 单独持有 OID——拒绝，违背 `pseudo_ref.rs:13-32` 的设计契约。
- **Consequences:** `MP-10`、`MP-43`、`MP-39`、`MP-41`、`MP-45`、`MP-30`、`MP-44` 各自单轴、`M` 规模。
- **Revisit when:** 扩展其它工作区伪引用时。

### ADR-MP-05: `merge-state.json` 新状态对旧二进制保持可读

- **Status:** Accepted（2026-11-18 新增，取代 2026-11-18 08:19 版的「真实旧版二进制拒绝与升级恢复」家族卡 `MP-14a`/`MP-35a`）
- **Context:** `MergeState`（`src/command/merge/state.rs:55-56`）未设 `deny_unknown_fields`（反序列化忽略未知键，`state.rs:147`），所有新增字段均 `#[serde(default)]`；`strategy: Option<MergeStrategy>` 的取值若出现旧二进制不认识的枚举变体，旧二进制将无法反序列化整个状态文件。octopus 合并已以 `strategy=None` + 非空 `targets` 表示（`src/command/merge.rs:4359-4366`）。
- **Decision:** 新行为不向 `strategy` 写入新的枚举变体：`MP-55` 的显式 `-s octopus` 沿用 `strategy=None` + 非空 `targets`；`MP-37` 的外部策略名写入新增的可选字段 `external_strategy`（`#[serde(default, skip_serializing_if = "Option::is_none")]`），`strategy` 保持 `None`。旧二进制读取此类状态时忽略未知字段，`--continue`、`--abort` 与 `libra commit` 照常工作；唯一差异是旧二进制的 `--restart`（Libra 专有）会以默认策略重放，登记于「风险登记」与发布说明（降级前先完成或中止合并）。各卡以集成用例检查 `merge-state.json` 的 `strategy` 字段缺失或为 `null`（`MP-55` AC3、`MP-37` AC3）。
- **Alternatives considered:** 新增枚举变体并以真实旧二进制验证拒绝与升级恢复——撤销：需要在测试中构建历史版本二进制，不是 L1 确定性门，且引入额外家族发布点。
- **Consequences:** `MP-55`、`MP-14`、`MP-35`、`MP-15`、`MP-37` 均可独立发布。
- **Revisit when:** `MergeState` 需要真正不兼容的格式变化时（届时按 GC-08 设计版本字段）。

## 全局工程约束

继承 [plan-template.md](plan-template.md) `GC-01..13`。补充约束：
- **GC-MP-01:** 所有 stdout/stderr、退出码、冲突标记、reflog 与 `SQUASH_MSG` 格式改动均以 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（核对日期 `2026-11-17`）为基准对齐，不得破坏 `--json` / `--machine` 机器输出结构（`--json` 下成功时 stdout 只含一个 JSON 文档；失败时 stdout 为空，错误报告以一个 JSON 文档写到 stderr——既有契约，`tests/command/pull_test.rs:149-152,752-757`；`--machine` 为 NDJSON）。
- **GC-05 / ER-06a 文档同步:** 每张卡按其 `Docs and compatibility impact` 所列命令同卡更新 `docs/commands/<cmd>.md`、`docs/commands/zh-CN/<cmd>.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`，并对 `docs/development/commands/<cmd>.md`、`COMPATIBILITY.md` 对应行（「适用范围」所列行号）与卡内列出的 `docs/error-codes.md` 逐文件做语义核对：受本卡行为影响即同卡更新（兼容矩阵原地修改、不增删行），不受影响则按「文档与网站验收命令」第 1 步记录理由与未变 SHA-256。**`docs/error-codes.md` N/A 口径：** 卡内把该文件记为 N/A 的前提是本卡不新增稳定码，不新增或修改「Per-command exit-code overrides」小节（`docs/error-codes.md:60-64`）的退出码覆写，也不改变任何稳定码的类别、含义或默认退出码；该文件的稳定码表按类别泛述示例、不枚举本计划改变的具体调用（2026-11-19 以 `rg -n -i 'merge|pull|reflog|stash|rev-parse|squash|strategy' docs/error-codes.md` 核对，命中仅为 `branch -d` 覆写（`:64`）、`LBR-CONFIG-001` 对 `pull` 等配置读取的泛述（`:105`）、`LBR-CONFLICT-001` 的 `merge/rebase conflict still unresolved` 示例（`:107`）、`LBR-UNSUPPORTED-001` 的 gitlink 示例（`:126,322`）与网络错误分类对 `pull` 的泛述（`:654`），均不描述本计划改变的调用），因此卡使原先报错的调用改为成功、或改用既有稳定码报错，都不使该文件失准。改变数值退出码的卡（`MP-22`、`MP-34`、`MP-40`、`MP-14`、`MP-38`、`MP-37`）必须更新该文件；`MP-50` 把它列入逐文件语义核对。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01`..`ER-14`：
- **ER-04 / ER-08 / ER-13 / ER-14:** 每张 `implementation` 卡均为 `independent` 发布切片，完成后按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 权威三处（`Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`；因每卡均修改 `install.sh`/`install.ps1`（ER-13 `T-1`），C 组第 ④ 步除运行本卡 A 组 focused `nextest`（卡特有用例与共享回归组）外，必须跑双环境全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo build --release`，随后签名提交推送 PR；因本工作区为 `.libra` 仓库（无 `.git`），squash merge 生成新提交后，发布前必须执行 `libra fetch` 同步远端 `main`、核验该 squash-merge 提交的 PGP 签名与 `Signed-off-by` DCO trailer、比较树对象 ID：`libra rev-parse '<reviewed-head>^{tree}'` 与 `libra rev-parse '<squash-sha>^{tree}'` 必须逐字相同，且 `base.yml`/`codeql.yml` 在该精确 SHA 全绿；**止步条件：** 任一核验失败立即停止，严禁执行 `gh release create "v${VERSION}"`。
- **ER-04 D 组远端后置门（每张卡适用）:**
  - `.github/workflows/base.yml`（`pull_request`；`push` `main`、`release/**`）：`format`、`clippy`、`redundancy`、`test` 在 PR head SHA 与 squash-merge SHA 全部 green（被 `cancel-superseded-main.yml` 取消的 run 不计 PASS，须取同 SHA 的后续成功 run）。
  - `.github/workflows/codeql.yml`（`pull_request` `main`；`push` `main`、`release/**`）：`analyze` green。
  - `.github/workflows/release.yml`（`push` `v*` tag，由 `gh release create "v${VERSION}" --target "$MERGE_SHA"` 触发）：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 green；并核验 `download.libra.tools` / stable manifest（`HEAD` 200、range `GET` 206）。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **分类：** 每条 `Verification` 标明三类之一——「卡特有用例」（`(new)` 用例，只用精确过滤 `-E 'test(=<完整测试名>)'`，一条只含一个测试）、「卡特有判据（非 nextest 门）」（`rg` 锚点定位加按映射逐项人工核对文档内容，按条目自带判据执行，`rg` 退出码按「文档与网站验收命令」的退出码捕获通则判定）或「共享回归组」（锚定模块的正则过滤 `-E 'test(/^<模块路径>::/)'` 或整个 compat target）。测试名一律写完整路径：集成测试 `command::<module>_test::<fn>`（`tests/command_test.rs` → `tests/command/mod.rs`），库内单元测试为 crate 内完整模块路径（如 `command::merge::tests::<fn>`，以 `--lib` 运行）。
  2. **列举：** 运行任一 nextest 条目前，先以相同 target 与过滤执行 `cargo nextest list`（如 `source .env.test && cargo nextest list --test command_test -E '<同一过滤>'`）并记录列出的测试名：卡特有用例必须恰好列出条目写明的那 1 个完整测试名；共享回归组必须 ≥ 1 条并记录条数 `N`（零匹配时 nextest 以 `error: no tests to run` 失败）。不满足即 STOP。
  3. **运行与判据：** 记录退出码与 nextest 汇总行 `N tests run: P passed, F failed, S skipped`（`N` 为 1 时写作 `1 test run`；某项为 0 时 nextest 可能省略该段，按 0 记）：要求退出码 = 0、`F` = 0、`N` = `P` = 第 2 步列出条数。`S` 是被过滤排除（及未选中的 `#[ignore]`）的测试数，只记录、不作判据。出现 `FLAKY` 须写明，`--retries` 不得掩盖最终失败。卡特有用例必须是 L1 确定性用例（不依赖 L2/L3 环境门，不得以打印 `skipped (...)` 提前返回）。
  4. **AC 绑定与期望来源：** 卡特有用例对所在卡「AC → 断言映射」逐条断言（退出码、stdout/stderr 的精确内容或必含/必不含片段、前后状态），执行记录逐条引用 AC 编号；映射只列判定对应 AC 所需的检查。映射中标注「Git 2.54.0 预探」的值是评审时本机 Git 2.54.0 在同构夹具上的观测，标注「Git 同夹具期望」的值在开工时取得；两类期望都必须在开工时用 `git@6de20f6092` 构建的 Git 在同一夹具重新取得并硬编码进测试，与预探不一致时以 pinned 版本为准并回写本卡 `Current evidence`（超出本卡行为轴时按 ER-03 处理）。Libra 专有行为（`--json` 输出、稳定码 `LBR-*`、`merge-state.json` 字段、`.libra/` 文件）以卡内写明的 Libra 基准为准，不向 Git 取值。
  5. **共享回归组** 属 ER-04 A 组 focused 门，只证明既有行为未回归，不计为卡特有 AC 证据，也不计入 G-03 的 `VER` 分子；判据同第 3 步（`N` 以第 2 步列出为准）。后续卡有意取代前序卡用例所断言的行为时，在该卡内只修改被取代的断言、保留其余断言，并在该卡 `Current evidence` 记录被改断言与理由。
  6. **进程级隔离与平台：** 本计划新增的卡特有用例一律以子进程调用 `libra`（`tests/command/mod.rs` 的 `run_libra_command` / `run_libra_command_with_env`；需要向子进程写入 stdin 时用同文件的 `run_libra_command_with_stdin` / `run_libra_command_with_stdin_and_env`，`tests/command/mod.rs:417-460`），通过显式 `current_dir` 与逐子进程环境变量传入 `GIT_EDITOR`、`PATH`、`LIBRA_FINE_EXIT_CODES` 等设置，不修改测试进程的 cwd/环境，因而不使用 `#[serial(...)]`，不触及跨计划共享的 `tests/SERIAL_REGISTRY.tsv` 与生成的 `.config/nextest.toml`；若实现中确需 `#[serial]`，须先按 ER-03 把这两个文件加入本卡写集并补 `compat_serial_registry` 回归门后再开工。依赖 Unix 语义（可执行 shell 脚本、`GIT_EDITOR=false`）的卡特有用例整条以 `#[cfg(unix)]` 限定（各卡夹具写明）；本计划的测试门（本地三门与 `base.yml` 的 `test` job）在 Unix 上执行。
  7. **记录：** 命令、列出条数、汇总行、退出码与 UTC 时间写入本计划对应卡的 `Verification` 条目（v2.12，不新建证据文件）。
  8. **通用判定定义：** 「HEAD 的父提交」= `libra cat-file -p HEAD` 输出中按出现顺序的全部 `parent` 行 OID 列表（同 `tests/command/merge_test.rs:59-67` 的 `commit_parents`）。「映射的执行仓库」：同一卡特有用例内，(i) 映射以夹具名或场景名开头（如「F-FF；」「F-DIV，先…」「F-998R；」「冲突场景…」），或写「重建」「另建」「另一份重建的…」时，在一份新建的该夹具（场景）仓库上执行；(ii) 写「ACn 之后」「ACn 的调用」「在 ACn 的状态上」「同一合并之后」时检查或沿用 ACn 的仓库、调用输出与状态；(iii) 用例的第一条映射在新建的卡夹具上执行；(iv) 其余映射沿用紧邻上一条映射所在的仓库与状态，且只在该状态仍满足本条映射写明的前提时成立——会使前提失效的状态（已提交的合并、待决的合并或冲突、已改写的配置）一律以 (i) 的「重建」显式隔离。「夹具提交名」：卡内夹具以名字称呼的提交（如 `A`、`B`、`C`、`D`、`M`、`MC`、`O`、`T`、`T1`、`T2`、`U`、`c0`–`c2`）在创建后立即以同名轻量标签 `libra tag <名>` 标记，映射中的 `<名>`、`<名>:<path>`、`<名>^0`、`'<名>^{tree}'`、`libra merge <名>` 与「在 `<名>^0` 上」（`libra switch --detach <名>`）都解析该标签；卡内明确称为分支的名字（如 `main`、`topic`、`side`、`unsigned`、`b1`、`b2`、`rename`、`clean_branch`）是分支，不另建同名标签；某名既被称为分支又被称为提交时以分支为准、该提交即分支的提示。例外：pull 用例中由 `git` 在远端工作目录（`create_remote_fixture` 的 `workdir`，`tests/command/pull_test.rs:87-147`）创建的命名提交（如 `B`、`U1`、`S1`），在创建后立即以该目录中 `git rev-parse HEAD` 记录完整 OID，映射中的 `<名>` 即指该 OID（Libra 仓库 D 取得对象后 `libra rev-parse <OID>` 可用），不在 Git 仓库或 D 中打同名标签；D 中由 `libra` 创建的命名提交（如 `D1`）仍按本条打标签。在非当前分支的提交上继续提交时，可先 `libra switch --detach <父>` 再提交并打标签，或在临时分支上提交——两种做法不改变任何判据（Libra 0.30.207 实测：轻量标签可用于 `rev-parse <名>:<path>`、`'<名>^{tree}'`、`switch --detach` 与 `merge`）。「仓库快照 Σ」= 下列五项的组合，「Σ 逐项相等」指每项逐字节相等：(a) `.libra/libra.db` 中除下列排除表外**全部表**的全部行（每表按主键排序；无主键按全部列排序）。排除表：审计 `reflog`；操作日志与其头/日志 `operation`、`operation_head`、`operation_journal`、`operation_parent` 与 `change_identity`、`change_predecessor`、`change_revision`（每条命令都可能追加或推进）；缓存 `object_index`、`working_dirty`、`working_dirty_meta`、`revision_ordinal`、`revision_ordinal_meta`；SQLite 内部表（`sqlite_` 前缀，如 `sqlite_sequence`）；名称以 `agent_`、`ai_`、`memory_` 开头的 AI/agent 与记忆投影子系统表。因而 Σ(a) 覆盖 `reference`（含 `HEAD`）、`config`、`config_kv`、`rebase_state`、`sequence_state`、`bisect_state`、`notes`、`metadata_kv`、`layer`、`sparse_view`、`workspace_record`、`worktree_*`、`schema_versions` 等全部引用、配置、待决操作状态与元数据表（`sql/sqlite_20260309_init.sql:5,12,19,42`；`sql/migrations/2026070401_sequence_state.sql`（同时删除旧的 `cherry_pick_state`/`revert_sequence`，`:24-25`）、`2026072301_bisect_state_worktree_scope.sql`；2026-11-19 以 Libra 0.30.207 在新建仓库的 `sqlite_master` 核对表集合，并实测被拒绝的 `merge --ff-only <分叉目标>` 与 `merge <未知目标>` 前后只有 `object_index`、`operation*`、`sqlite_sequence` 变化）；开工时若出现上述两类之外的新表，按 ER-02 归类（引用/配置/待决状态/元数据纳入，日志/缓存排除）并记入首个使用 Σ 的卡的 `Current evidence`；(b) 索引的语义内容：`libra ls-files -s` 的完整 stdout（每个条目的模式、OID、阶段与路径，含全部未合并阶段）；不比较 `.libra/index` 原始字节——其中的 stat 缓存在语义不变的索引重写后也会变化（如 `merge --abort` 重写恢复的文件后重建索引），按字节比较会让正确实现失败；本计划夹具不使用 intent-to-add、skip-worktree 等索引扩展标志；(c) 「工作树清单」（定义见下）；(d) `.libra/` 下递归全部普通文件的「相对路径 → 字节」清单，只排除目录 `objects/`、`logs/`、`object-index-repair/`、`object-index-repair-locks/`、`object-index-repair-tmp/` 与文件 `libra.db`（由 (a) 覆盖）及其 SQLite 旁路文件 `libra.db-wal`、`libra.db-shm`、`libra.db-journal`（若存在；随连接与事务变化，内容由 (a) 覆盖）、`vault.db` 及其同类旁路文件（密钥库，合并与拉取不写）、`index`（由 (b) 覆盖）、`conversion-stat-v1`（工作树转换的 stat 缓存，`src/utils/worktree_conversion.rs:429`，只影响性能）、`COMMIT_EDITMSG`（见下）、`info/operation-pointer.json`（`libra op` 操作日志的指针，`src/internal/operation/working_copy.rs:17`，随每次记录的操作推进，与 `operation*` 表同属操作日志）与 `*.lock`（空锁文件），因而覆盖 `merge-state.json`、`merge-autostash.json`、`SQUASH_MSG`、`MERGE_MSG`、`FETCH_HEAD`、`refs/` 等全部待决/操作状态文件（`src/command/merge/state.rs:44`、`src/command/merge/autostash.rs:31`、`src/command/merge.rs:1653,2918`、`src/internal/pseudo_ref.rs:28`）；(e) `libra stash list` 的 stdout。「工作树清单」= 递归列出工作树中除仓库元数据目录 `.libra/` 与任一层级名为 `.libra-test-home` 的目录（含其全部内容）外的全部路径（含目录与符号链接）及其类型，普通文件附字节与可执行位，符号链接附目标；`.libra-test-home/` 是测试辅助函数为每个子进程在其调用 cwd 下建立的隔离 HOME（`tests/command/mod.rs:353-381` `base_libra_command`：`HOME`、`XDG_CONFIG_HOME` 与 `LIBRA_CONFIG_GLOBAL_DB`/`LIBRA_CONFIG_SYSTEM_DB` 都指向其中），含 `.config/` 以及 `libra init` 写入的 vault 解封密钥 `.libra/vault-keys/<uuid>` 等测试辅助状态（Libra 0.30.207 实测），不属于被测仓库内容；以子目录为 cwd 的调用会在该子目录下另建一个。对象库、上述审计/缓存表与 `COMMIT_EDITMSG` 不在 Σ 内：被拒绝或回滚的操作可能留下不可达对象、审计条目、缓存条目，或改写 `COMMIT_EDITMSG`（钩子与编辑器的一次性消息缓冲，`src/command/merge/output.rs:90-92`、`src/command/merge.rs:1594-1595`；每次写入前都被覆盖，没有命令把它当作待决状态读取），它们不改变任何配置、引用值、索引、工作树或待决状态。Σ 由首个需要它的卡在 `tests/command/merge_test.rs` 新增 `pub(super) fn repo_snapshot(repo: &Path)`（已存在则复用；不修改共享的 `tests/command/mod.rs`，避免 ER-13 `T-4`），`pull_test.rs` 经 `super::merge_test::repo_snapshot` 复用。

- **文档与网站验收命令（全部任务卡的 ER-06/ER-06a 同卡门；按序执行，任一步失败即 STOP 且本卡不得进入 `complete`）：**
  - **退出码捕获（本节通用）：** 本节所有按退出码判定的命令（`test`、`rg`、`curl`、`libra merge-base --is-ancestor` 等）一律以 `if <命令>; then rc=0; else rc=$?; fi` 的条件形式捕获退出码（模板「Verification 判定口径」），在 `set -e` / `errexit` 下也不会因预期的非 0 退出（如 `rg` 的「不含」= 1）而提前中止；随后按各步写明的取值判定。
  - **页面与命令对应：** 本卡网站文件为 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`（`<cmd>` 取本卡 `Docs and compatibility impact` 所列命令，均为子命令页，由同步脚本从 `docs/commands/<cmd>.md` 生成），线上页面 `https://libra.tools/en/docs/commands/<cmd>`。
  1. **仓库文档逐文件验收：** 对 `Docs and compatibility impact` 列出的每个仓库内文件记录修改前后 `shasum -a 256 <file>`。EN 与 zh-CN 命令页（`docs/commands/<cmd>.md`、`docs/commands/zh-CN/<cmd>.md`）必须改动（本计划每张卡都改变用户可见行为，ER-06a）：改动后 SHA-256 与改动前不同，且对卡内「文档关键字」执行 `rg -n -F -- '<关键字>' <file>`（退出码 0 = 命中；1 = 缺失 → STOP；>1 = 命令失败 → STOP），并人工确认对应章节描述了本卡行为。`docs/development/commands/<cmd>.md`、`COMPATIBILITY.md` 对应行与卡内列出的 `docs/error-codes.md` 逐文件语义核对，结论为「已更新」（记录前后 SHA-256 与所改段落）或「无需修改」（记录理由与未变 SHA-256），不得为通过机械门添加不合该文件粒度的内容；卡内写明「必须更新」的文件不得以「无需修改」通过。`COMPATIBILITY.md` 先以命令块 W-1 刷新目标行号（结果须等于「适用范围」所列基线行号集合；不等即 STOP 并按 ER-02 重新核对），有改动时断言改动前后 `wc -l < COMPATIBILITY.md` 相等（原地修改）。
  2. **网站前置（第 3 步写入前完成）：** ① `cd /Volumes/Data/gitmono/libra-backend && test -d .libra && test ! -e .git`（退出 0 即仅含 Libra 元数据）；② 当前分支须为 `cf`：`libra --offline symbolic-ref --short HEAD` 以非 0 退出或其 stdout 不恰为 `cf\n` 时，按 ER-06a（`plan-template.md:332`）执行 `(cd /Volumes/Data/gitmono/libra-backend && libra switch cf)` 后重跑本条；切换被脏工作区、缺失分支或其它状态阻止时不得绕过、覆盖或写入，即 STOP 并报告；本已在 `cf` 或切换成功后，再执行 `libra status --short --branch`：须退出 0 且其 stdout 首行以 `## cf...origin/cf` 开头，否则 STOP（两条命令的退出码与输出都写入本卡记录）；记录全部既有无关改动（路径与 SHA-256）并保持不动；③ `libra status --short --no-branch -- <本卡网站文件>` 输出为空（目标文件无他人未提交改动）；④ 暂存区为空：`libra status --porcelain` 输出中不存在首列（暂存区状态）为非空格且非 `?` 的行（Libra 提交整个暂存区，`src/command/commit.rs:253-255`，他人已暂存的改动会随本卡提交被推送）；⑤ 远端对账：`libra fetch origin` 退出 0 后比较 `libra rev-parse HEAD` 与 `libra rev-parse origin/cf`——相等即通过并在本卡记录该 SHA；不等且 `libra merge-base --is-ancestor HEAD origin/cf` 退出 0（本地仅落后）时执行 `libra pull --ff-only origin cf`（须退出 0）后从 ② 重新核验；不等且该命令退出 1（本地有未推送提交或已分叉）即 STOP 并报告归属，不得把他人提交随本卡推送；⑥ 已按 `DEP-MP-10` 取得协调者的网站交付串行窗口（网站生成产物视为与所有同批计划的网站交付必有交集，故第 2–6 步总须持有该窗口；第 7 步只读线上页面，不需窗口）、`DEP-MP-12` 的用户确认未被撤回或改变。任一条不满足或任一命令出现非预期退出码即 STOP 并报告。
     - **续作例外（同一卡在第 3–7 步中断后重入）：** 重入时先执行 ①②，再以本卡已记录的证据识别本卡自己的残留，只放行能逐项对上的下列状态，其余一律按 ③–⑤ STOP：(i) **已写入未暂存**——各本卡网站文件当前 SHA-256 等于第 3 步记录的写后值，暂存区为空，`libra rev-parse HEAD` 等于第 2 步记录的 `origin/cf` SHA：从第 4 步继续；(ii) **已暂存未提交**——`libra status --porcelain` 的暂存路径集合恰为本卡网站文件、这些行的第二列为空格，各文件 SHA-256 等于第 5 步受审值，`HEAD` 同 (i)：从第 6 步的提交继续；(iii) **已提交未推送**——`libra rev-parse HEAD` 等于第 6 步记录的本卡提交 SHA，`libra rev-parse 'HEAD^'` 等于第 2 步记录的 `origin/cf` SHA，且 `libra fetch origin` 后 `libra rev-parse origin/cf` 仍等于该记录值：从第 6 步提交后的核验继续并推送；若崩溃发生在提交之后、记录提交 SHA 之前，则仅当 `libra rev-parse 'HEAD^'` 等于记录的 `origin/cf` SHA、`libra show --name-only --format=format: HEAD` 恰为本卡网站文件、各文件 `libra show HEAD:<path> | shasum -a 256` 等于第 5 步受审值且提交说明为本卡第 6 步的说明时，才把该 `HEAD` 记为本卡提交并按 (iii) 继续；(iv) **已推送**——`libra ls-remote origin refs/heads/cf` 输出恰一行，其第一列 hash 等于记录的本卡提交 SHA：从第 7 步继续。无法归入上述任一状态或任一核对不符即 STOP 并报告。
  3. **网站同步（必须产生变更）：** 先按第 7 步为每个本卡网站文件选定判定句 `M` 并确认零命中；以包含本卡最终 EN 参考文档的 checkout 为 `--libra-root`（默认 `/Volumes/Data/gitmono/libra`；执行前以第 1 步记录的 SHA-256 确认其中 `docs/commands/<cmd>.md` 已是本卡合入 `main` 的字节），执行 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --write --only <cmd>...`（多个命令以空格分隔），随后同一命令去掉 `--write`（只读漂移检查）必须退出 0（1 = 漂移、2 = 安全拒绝，均 STOP）。记录每个本卡网站文件修改前后 SHA-256：本计划每张卡都已在第 1 步改变 EN 命令页，同步后每个本卡网站文件的 SHA-256 必须与修改前不同，且 `libra status --short --no-branch -- <该文件>` 输出非空；任一文件未变即 STOP 并排查（同步未生效或改动未进入 `--libra-root`），不设「网站无变化」通过分支。
  4. **前端验证：** 在 `/Volumes/Data/gitmono/libra-backend` 运行 `pnpm typecheck`（含 `gen:docs`）与 `pnpm build`，均退出 0。`pnpm typecheck` 经 `gen:docs`（`apps/tanstack-app/package.json:12-13`）无条件重写被忽略的生成文件 `apps/tanstack-app/src/generated/docs-manifest.ts` 与 `docs-search.ts`（`scripts/gen-docs-manifest.ts:87,91,524-525,550`），`pnpm build`（`vite build`）重写被忽略的 `apps/tanstack-app/dist/`（`.gitignore:6,87-88`）。**归属与保留策略：** 这三处是网站仓库约定只由生成器 / 构建写入的派生产物（生成文件首行由生成器写入 `AUTO-GENERATED by scripts/gen-docs-manifest.ts — do not edit`，`scripts/gen-docs-manifest.ts:474,539`），每次网站交付都必须重生成，旧内容不予保留；它们已列入每张卡的 `Implementation write set`，并按 `DEP-MP-10` 视为与所有同批计划的网站交付相交：第 4 步只在持有协调者对 `/Volumes/Data/gitmono/libra-backend` 网站交付的串行窗口时执行。执行前逐个检查两个生成文件：不存在时记录 `absent`；存在时首行不是该标注即 STOP 并报告（有人违反约定手工编辑），是则记录其 SHA-256。执行后两者都须存在且首行为该标注（否则判第 4 步失败），并记录其 SHA-256（只记录、不作判据）；不得手工编辑、暂存或提交这三处（第 6 步的暂存与提交文件集合核对保证不会随本卡提交）。
  5. **独立评审：** 对本卡网站改动（`libra diff -- <网站文件>`）取得独立 Claude Code 字面 `VERDICT: PASS`，记录受审文件 SHA-256；评审后任一受审文件 SHA-256 变化即须重审。
  6. **提交与推送：** `libra add <本卡网站文件精确路径>` 后，`libra status --porcelain` 中首列非空格且非 `?` 的路径集合须恰为这些文件（否则 STOP）；`libra commit -s -m "docs(<cmd>): sync <卡 ID>"` 后，`libra show --name-only --format=format: HEAD` 列出的非空行集合须恰为这些文件（否则 STOP，不得推送）；按 ER-07 核验同一提交的 `gpgsig` 与 `Signed-off-by`；记录 `libra rev-parse HEAD` 与 `libra rev-parse 'HEAD^{tree}'`，并以 `libra show HEAD:<path> | shasum -a 256` 确认提交内容等于第 5 步受审 SHA-256；`libra push origin cf` 后 `libra ls-remote origin refs/heads/cf` 须恰输出一行 `<hash>\trefs/heads/cf`（格式见 `src/command/ls_remote.rs:403`），其 `<hash>` 等于 `libra rev-parse HEAD`；推送被拒（远端已前移）即 STOP 并报告，不得强推。任一不符即 STOP。
  7. **部署核验：** **「页面检查」**是本计划核对线上页面的唯一方法，页面内容只保存在 shell 变量中，不写任何文件（v2.12）：先 `if page="$(curl -fsSL '<页面 URL>')"; then rc=0; else rc=$?; fi`，`rc` 非 0 即 STOP（不得视为「不含」；`-f` 使 HTTP ≥ 400 以非 0 退出）；再 `if rg -F -q -- '<判定句>' <<<"$page"; then hit=0; else hit=$?; fi`（以 here-string 直接传入，不经管道），`hit` 为 0 = 含、1 = 不含、>1 判执行失败；每次检查的 `rc`、`rg` 退出码与 UTC 时间写入本卡记录。在第 3 步写入前为每个本卡网站文件选定本卡新增的一句描述本卡行为的唯一纯文本句 `M`（不含反引号、链接、强调等 Markdown 标记，也不含会被 HTML 转义的 `'`、`"`、`&`、`<`、`>`，确保页面 HTML 源逐字包含），并确认零命中：`rg -F -q -- '<M>' <网站文件>` 退出 1（>1 判执行失败），且对应页面的「页面检查」结果为不含；同步后 `rg -F -q -- '<M>' <网站文件>` 须退出 0。推送后按 ER-10 有界轮询（≤ 5 次、总计 ≤ 30 分钟）执行「页面检查」，直到含 `M`，记录命中时间与第 6 步的 `cf` 提交 SHA。若 `M` 在推送前已出现或超预算仍未出现，本卡置 `blocked`，不得标 `complete`。
  8. **时序：** 第 1 步与第 2 步 ①–④⑥ 在本卡 C 组第 ⑦ 步签名提交之前完成（第 1 步的仓库文档改动随本卡 Libra 提交），任一失败则本卡不得提交、合并或发布（`Lifecycle` 置 `blocked`，不进入 `remote-pending`）；第 3–7 步在本卡 Libra PR squash merge（C 组第 ⑧ 步）与 `gh release create`（第 ⑨ 步）之后执行，使网站不先于发布公开新行为（第 3 步开始前重跑第 2 步全部条目）；第 3–7 步未完成前本卡停留在 `remote-pending`。

上列第 1 步引用的命令块（`<cmd>` 替换为本卡命令；在 Libra 仓库根目录执行）：

```bash
# W-1：列出 COMPATIBILITY.md 中 <cmd> 的状态矩阵行号；结果须等于「适用范围」所列基线集合
# （merge=205,212；pull=225；reflog=249；commit=209；rev-parse=217,277；stash=193；rm=186）
awk -F'|' -v c=" <cmd> " '$2==c && $3 ~ /^ (supported|partial|unsupported|intentionally-different) $/ {print NR}' COMPATIBILITY.md
```

## 实施顺序

全串行 DAG（与各卡 `Dependencies` 中的本计划卡 ID 一一对应；`DEP-*` 前置见依赖登记表）：
`MP-01 -> MP-26 -> MP-33 -> MP-27 -> MP-28 -> MP-02 -> MP-21 -> MP-22 -> MP-34 -> MP-40 -> MP-03 -> MP-52 -> MP-04 -> MP-49 -> MP-05 -> MP-06 -> MP-29 -> MP-07 -> MP-08 -> MP-32 -> MP-09 -> MP-10 -> MP-43 -> MP-48 -> MP-39 -> MP-41 -> MP-45 -> MP-30 -> MP-44 -> MP-11 -> MP-53 -> MP-12 -> MP-13 -> MP-23 -> MP-55 -> MP-14 -> MP-35 -> MP-38 -> MP-15 -> MP-51 -> MP-37 -> MP-42 -> MP-16 -> MP-17 -> MP-24 -> MP-18 -> MP-25 -> MP-46 -> MP-50 -> MP-19 -> MP-54 -> MP-20 -> MP-47`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-MP-01 | incoming | 上游基线 | `libra` `main` 当前 checkout | 本计划协调者 | 链首 `MP-01` 开工时 `cargo check` 通过且「事实基线」锚点按 ER-02 可解析（后续各卡开工时的锚点刷新由 ER-02 本身覆盖） | `src/command/merge.rs:263` | 锚点漂移时按 ER-02 刷新后再开工 |
| DEP-MP-02 | incoming | 跨计划前置 | `plan-20261117-commit-signing.md#REL-CS-00`（`CS-02/CS-02a` `#1005`） | 仓库发布协调者 | `REL-CS-00`（提交默认不签名）已合入 `main`，供 `MP-08`/`MP-32` 构造未签名与已签名夹具 | `docs/development/plan/plan-20261117-commit-signing.md` | 未合入前 `MP-08` 保持 `pending` |
| DEP-MP-03 | outgoing | 跨计划协调 | `plan-20261117-checkout-switch-restore.md#CSR-21`（`#1008`） | `checkout-switch-restore` 计划协调者 | `MP-17` 先合入并作为 `merge -s recursive` D/F 与 `modify/delete` 报告唯一事实源，`CSR-21` 随后按其 `DEP-CSR-04` 消费 | `docs/development/plan/plan-20261117-checkout-switch-restore.md` | 不阻塞 `MP-17` 自身发布 |
| DEP-MP-04 | incoming | 已有 issue 计划硬前置 | `docs/development/plan/issues/974.md#I974-01`（`#974` 合并标题按目标类型装饰） | 仓库发布协调者 | `I974-01` 已 `done/complete` 且已合入 `main`：单目标合并标题按类型装饰为 `Merge branch '<x>'`（`MP-04`、`MP-49` 的期望主题依赖该装饰；2026-11-19 核对仍为 `pending`，`src/command/merge_message.rs:9-17` 仍生成 `Merge {upstream} into {head_name}`）；`MP-04` 在其上以 Git 的 `merge.suppressDest` 模式匹配（未配置时 `main`、`master`）取代 `I974-01` ADR 中「目的地=当前分支即省略后缀」的规则（`issues/974.md:130`；Git 在当前分支为 `dev` 时输出 ` into dev`，见 `MP-49` AC5），并按「验证执行协议」第 5 条改写依赖旧规则的断言；`#974` 计划同时须无在途写者 | `docs/development/plan/issues/974.md:3,130`；`src/command/merge_message.rs:9-17` | 未合入前 `MP-04` 及其后续卡（全串行）保持 `pending`；有在途写者时禁止并发写 `merge.rs` |
| DEP-MP-05 | incoming | 已有 issue 计划硬前置 | `docs/development/plan/issues/978.md#I978-01`（`branch.<name>.mergeOptions` 读取器） | 仓库发布协调者 | `I978-01` 已 `done/complete` 且其读取器已在 `src/command/history_config.rs` 合入 `main`；`MP-26` 复用该读取器（GC-02），本计划不修改 `issues/978.md`：`MP-33` 保持其 `ADR-I978-02`/AC18 对「配置 `--squash` + 显式 `--no-commit`」的 fail-closed（放行属 `DEFER-MP-07`） | `docs/development/plan/issues/978.md:3,41-44,90`（2026-11-18 核对：`pending`） | 未合入前 `MP-26` 及其后续卡保持 `pending` |
| DEP-MP-06 | incoming | 已有 issue 计划协调 | `docs/development/plan/issues/984.md`（`#984` `--no-squash`） | 仓库发布协调者 | `#984` 计划无在途写者；`MP-26`/`MP-33`/`MP-27` 只处理 `--commit`、`--squash` 与 `--no-ff` 组合 | `docs/development/plan/issues/984.md` | 有在途写者时相关卡保持 `pending`，禁止并发修改 `MergeArgs` `--squash` 字段 |
| DEP-MP-07 | outgoing | 跨计划协调 | `plan-20261117-commit-signing.md#CS-03` | `commit-signing` 计划协调者 | 按 `REL-CS-00 -> MP-08 -> MP-10 -> CS-03` 顺序由 `MP-10` 完成后交付 `commit.rs` | `docs/development/plan/plan-20261117-commit-signing.md` | 不阻塞 `MP-10` 自身发布 |
| DEP-MP-08 | outgoing | 跨计划协调 | `plan-20261117-sequencer-am.md` 的 `DEP-SA-05`（等待 `MP-10`）与 `DEP-SA-08`（等待 `MP-30`） | `sequencer-am` 计划协调者 | `MP-10` 先完成 `src/command/commit.rs`、`MP-30` 先完成 `src/internal/pseudo_ref.rs` 后再交付对方消费；本计划在 `MP-10` 之后还有 `MP-41`、`MP-45` 修改 `commit.rs`（`MP-43`、`MP-48` 不写该文件），其与对方卡的相交写入由 `DEP-MP-10` 串行 | `docs/development/plan/plan-20261117-sequencer-am.md:272,275` | 不阻塞 `MP-10`/`MP-30` 自身发布 |
| DEP-MP-09 | outgoing | 跨计划协调 | `plan-20261117-commit-signing.md#CS-14a,#CS-02a`（`DEP-CS-01/02`） | `commit-signing` 计划协调者 | `MP-05` 先合入 `SQUASH_MSG` 供 `CS-14a`，`MP-26` 先合入 `merge.rs` 供 `CS-02a` | `docs/development/plan/plan-20261117-commit-signing.md` | 不阻塞 `MP-05`/`MP-26` 自身发布 |
| DEP-MP-10 | incoming | 跨计划共享写集串行 | 同批 `plan-20261117-*.md` 计划中与本卡 `Implementation write set` 相交的在途卡：`COMPATIBILITY.md` 为全部同批计划共享；网站第 4 步改写的生成产物 `apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts` 与 `apps/tanstack-app/dist/` 视为与所有同批计划的网站交付相交；已知源码交集：`src/command/merge.rs`、`src/command/merge/rename_merge.rs`、`src/command/merge/tree_merge.rs`（`plan-20261117-clone.md` `CL-20`）、`src/utils/util.rs`（`CL-23`、`plan-20261117-global-init-infra.md` 的 `GII-*` 写者、`describe`/`branch` 计划）、`src/command/commit.rs`（`commit-signing`、`sequencer-am`）、`src/internal/pseudo_ref.rs`（`sequencer-am`）；其余以开工当日各计划写集为准 | 仓库协调者 | 每张卡开工编辑前，以 `plan-status.md` 与同批各计划的 `Implementation write set` 核对仓库内文件交集：无交集即满足仓库部分，有交集时须取得协调者对相交文件的单写者串行窗口（对等于 `plan-20261117-global-init-infra.md` 的 `DEP-GII-03`）；网站部分因生成产物必有交集，「文档与网站验收命令」第 2–6 步总须持有协调者的网站交付串行窗口。仓库窗口在本卡 Libra 提交合入 `main` 后释放，网站窗口在网站第 6 步推送完成后释放 | 核对结果、交集文件与协调者放行记录写入该卡 `Verification` 记录 | 未满足前该卡保持 `pending`，不编辑相交文件（G-10）；超过 7 日未获窗口即报告协调者，不得并发写入 |
| DEP-MP-11 | incoming | 跨计划顺序前置 | `plan-20261117-clone.md#CL-20`（其 `DEP-CL-07`） | 仓库发布协调者 | `CL-20` 已合入 `main`（`merge.rs`、`rename_merge.rs`、`tree_merge.rs`），`MP-17` 在其后开工 | `docs/development/plan/plan-20261117-clone.md:226` | 未合入前 `MP-17` 及其后续卡保持 `pending` |
| DEP-MP-12 | incoming | 用户确认（已满足） | 网站文档交付检出路径 | 用户 | **已满足**：用户 2026-11-18 11:55:09 UTC 确认本批计划的网站文档交付检出为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支），且本批不修改 `AGENTS.md` | 协调者转达的用户确认（记录于「文档职责」）；`AGENTS.md:135` 的差异为已知文档滞后 | 已满足，不构成开工前置；若用户撤回或改变该确认，各卡网站第 2 步即 STOP——尚未到 C 组第 ⑦ 步的卡置 `blocked`，已完成 C 组的卡停留 `remote-pending`，按新路径修订本计划并经评审后再执行 |
| DEP-MP-13 | incoming | 跨计划 issue 关闭前置（不阻塞任何卡） | `plan-20261117-commit-signing.md#CS-14`（`#794`：未配置 `commit.cleanup` 且不打开编辑器时默认 `whitespace`） | `commit-signing` 计划协调者 | `CS-14` 已合入 `main` 后，不带 `--cleanup` 的 `libra commit --no-edit`（含 `-a`）保留 `MP-05` 写入 `SQUASH_MSG` 的冲突注释段，`#980` AC1 才能原样成立 | `src/command/commit.rs:983`（当前默认 `strip`）；`docs/development/plan/plan-20261117-commit-signing.md`（`CS-14` 描述） | 不阻塞 `MP-05` 发布；`CS-14` 未合入时 `#980` 保持打开，或在关闭时注明并链接 `CS-14` |

### 发布分组与并发窗口

- 本计划不设 `REL-*` 合并发布：53 张卡均为 `Release boundary = independent`，各自走完整 C 组（ER-08 `patch + 1`）与 D 组。

**并发声明:** 全串行（各卡共享 `src/command/merge.rs`、`src/command/pull.rs`、`COMPATIBILITY.md`、命令文档与网站页，G-10）；与同批计划的相交写集按 `DEP-MP-10` 串行。

**发布者:** 本计划单一发布者（按 ER-12 与仓库全局单一发布者协调串行发布）。

**发布窗口顺序:** 按「实施顺序」链逐卡执行，同一时刻只允许一张卡持有发布窗口；一张卡的发布窗口在其 D 组与网站第 7 步完成（`complete`）后释放。

### Phase 1: CLI 参数、退出码、消息与输出契约（21 张卡）

**目标:** 修复 `merge` 参数优先级与配置、`--no-commit` FF、`--ff-only` 跨命令拒绝文案、冲突与拒绝退出码、`--edit` 恢复、`--into-name`/`merge.suppressDest` 与 octopus 默认消息、`SQUASH_MSG`、unborn `initial pull`、`reflog -1`、stdout/stderr 分流、签名成功行与 `gpg.minTrustLevel`、`resolve` 平凡合并诊断。
**进入条件:** DEP-MP-01 核实完成（`DEP-MP-05`/`DEP-MP-06` 只阻断 `MP-26` 及其后续卡，`DEP-MP-04` 阻断 `MP-04` 及其后续卡，`DEP-MP-02` 只阻断 `MP-08`）。
**退出条件:** `MP-01`、`MP-26`、`MP-33`、`MP-27`、`MP-28`、`MP-02`、`MP-21`、`MP-22`、`MP-34`、`MP-40`、`MP-03`、`MP-52`、`MP-04`、`MP-49`、`MP-05`、`MP-06`、`MP-29`、`MP-07`、`MP-08`、`MP-32`、`MP-09` 全部 `Lifecycle=done`、`Acceptance=complete`。

### Phase 2: Autostash、脏工作区、mtime 与 `pull --rebase`（13 张卡）

**目标:** `MERGE_AUTOSTASH` 由 `commit`/`--continue`/`--abort` 应用与清理、`rev-parse`/`stash show` 的 `MERGE_AUTOSTASH`、仅重叠路径拒绝脏工作区、保留未变更文件 `mtime`、`pull --rebase` ff 优先级矩阵与 `<repo> <refspec>` 上游解析。
**进入条件:** `MP-09` 达到 `done/complete`。
**退出条件:** `MP-10`、`MP-43`、`MP-48`、`MP-39`、`MP-41`、`MP-45`、`MP-30`、`MP-44`、`MP-11`、`MP-53`、`MP-12`、`MP-13`、`MP-23` 全部 `Lifecycle=done`、`Acceptance=complete`。

### Phase 3: 策略选择与外部策略（8 张卡）

**目标:** 显式 `-s octopus` 与多策略链逐个尝试、回绕与 `No merge strategy handled the merge.`（退出 `2`）；策略名白名单与 `Could not find merge strategy`；外部 `git-merge-<name>` 执行与退出码、调用契约，以及多个 `-s` 中含外部策略时的拒绝（`DEFER-MP-03`）。
**进入条件:** `MP-23` 达到 `done/complete`。
**退出条件:** `MP-55`、`MP-14`、`MP-35`、`MP-38`、`MP-15`、`MP-51`、`MP-37`、`MP-42` 全部 `Lifecycle=done`、`Acceptance=complete`。

### Phase 4: 重命名与 D/F 冲突引擎修复（11 张卡）

**目标:** 冲突标记路径标签、D/F 冲突、rename/directory、rename/delete、rename/rename D/F（落盘与报告）、`1to2` stage-1（含 `libra rm` 解决路径）与二进制不同 blob、传递目录重命名冲突（报告与阶段）。
**进入条件:** `MP-42` 达到 `done/complete`（`DEP-MP-11` 另阻断 `MP-17`）。
**退出条件:** `MP-16`、`MP-17`、`MP-24`、`MP-18`、`MP-25`、`MP-46`、`MP-50`、`MP-19`、`MP-54`、`MP-20`、`MP-47` 全部 `Lifecycle=done`、`Acceptance=complete`，且「完成判据」的全量收口门通过。

## 任务卡

### 任务卡粒度规则（强制）

继承 [plan-template.md](plan-template.md) 的 `G-01`..`G-11`。本计划的 G-03 计数口径：每条 `Acceptance criteria` 只陈述一条可独立失败的行为谓词与一个规范场景；「AC → 断言映射」只写判定该谓词所需的命令、退出码、输出与前后状态，不得出现 AC 之外的独立行为断言；不同语法形式、不同期望结果、不同触发条件或不同仓库形态一律拆为独立 AC。同一谓词在一组同类路径或对象上的全称断言计为一条，其映射须覆盖该组全部成员；对单一输出与映射中写全的期望文本做一次逐字相等比较计为一条。到达被判定状态所需的前置命令，以及退出码不是该 AC 判定对象时被测命令本身的「退出 0」，都是该 AC 的执行条件（不满足即该 AC 失败），不另计；凡两个可分别失败的状态事实一律拆为两条 AC。失败类 AC 只断言稳定错误码（人读模式：stderr 含 `Error-Code: <code>`；`--json`/`--machine` 下：stderr 去掉首尾空白后恰好解析为一个 JSON 值并断言其 `error_code`，GC-MP-01）或卡内写明的 Git 文案，「被测命令以非 0 退出」是其执行条件，用例必须断言；数值退出码只在退出码本身是本卡行为轴的卡（`MP-22`、`MP-34`、`MP-40`、`MP-14`、`MP-38`、`MP-37`）以单列 AC 断言，默认在 `LIBRA_FINE_EXIT_CODES` 未设置下断言；`ADR-MP-01` 的「`with_exit_code` 覆写优先于 `LIBRA_FINE_EXIT_CODES`」可在这些卡内以 `LIBRA_FINE_EXIT_CODES=1` 另列一条 AC 断言（如 `MP-22` AC2）。`VER` 分子 = 本卡「卡特有用例」与「卡特有判据」条目数（每条一个独立门；「验证执行协议」第 2 条的 `cargo nextest list` 是该门的执行步骤）；共享回归组不计入（「验证执行协议」第 5 条）。ER-06/ER-06a 同卡文档门不计入（G-03）。

### 字段全局默认与例外

- **Release boundary 默认:** `independent`（每卡独立执行完整 C 组含 `patch + 1`、`compat_version_surface_sync` 同步与 `gh release create`，并适用「执行检查必备需求」的 D 组）
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（推送/发布前：本地 `revert` 单提交；推送/发布后：已发布版本与 tag 不可撤回，按 ER-04/ER-10 新建 `FIX-*` 前滚修复并发新 `patch + 1` 版本；兼容窗口与降级指引：同 `0.30.x` 次版本内无 schema 变更，可回退安装上一版本 `v<prev>`；有待决合并状态时先 `libra merge --continue` 或 `--abort` 再降级，见 `ADR-MP-05`）
- **Migration and rollback 默认:** `N/A：无 schema 迁移`
- **Security and privacy 默认:** 继承 GC-07、GC-11
- **Performance budget 默认:** 继承 GC-10
- **Release write set 默认:** `Inherited`（= `Cargo.toml`、`Cargo.lock`、`install.sh`、`install.ps1` 与 release artifact，以 `compat_version_surface_sync` 为权威）
- **Full-suite trigger 默认:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）`
- **Deliverables 默认:** `N/A`
- **Version increment 默认:** `patch`；**C/D coverage from 默认:** `self`

**默认覆盖:** `MP-35`、`MP-15` 另在卡内展开 `Security and privacy`（策略名白名单、无 shell 执行）；`MP-11`、`MP-12` 另在卡内展开 `Performance budget`（只比较受合并影响路径）。

**规则 waiver（`EX-*`）:** 无。

### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| MP-01 | implementation | merge --ff/--no-ff/--ff-only 覆盖 | immutable-release | yes | yes | 7/8 | 1/8 | 1/1 | M | DEP-MP-01,DEP-MP-10 | 序列化于DEP-MP-10 | independent | N/A | N/A |
| MP-26 | implementation | merge --commit 与 mergeOptions --commit | immutable-release | yes | yes | 8/8 | 1/8 | 1/2 | M | MP-01,DEP-MP-05,DEP-MP-06,DEP-MP-10 | 序列化于MP-01 | independent | MP-01 | N/A |
| MP-33 | implementation | --squash 与最终生效 --commit 的配置后拒绝 | immutable-release | yes | yes | 5/8 | 1/8 | 1/2 | M | MP-26,DEP-MP-05,DEP-MP-06,DEP-MP-10 | 序列化于MP-26 | independent | MP-26 | N/A |
| MP-27 | implementation | merge --squash 与 --no-ff 组合拒绝 | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-33,DEP-MP-06,DEP-MP-10 | 序列化于MP-33 | independent | MP-01 | N/A |
| MP-28 | implementation | merge -s 与 -X 组合透传 | immutable-release | yes | yes | 6/8 | 1/8 | 1/1 | M | MP-27,DEP-MP-10 | 序列化于MP-27 | independent | MP-01 | N/A |
| MP-02 | implementation | merge --no-commit fast-forward | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-28,DEP-MP-10 | 序列化于MP-28 | independent | N/A | N/A |
| MP-21 | implementation | --ff-only 拒绝文案与 advice.diverging | immutable-release | yes | yes | 8/8 | 2/8 | 1/2 | M | MP-02,DEP-MP-10 | 序列化于MP-02 | independent | MP-02 | N/A |
| MP-22 | implementation | 冲突合并退出码 1（merge/pull） | immutable-release | yes | yes | 7/8 | 3/8 | 1/2 | M | MP-21,DEP-MP-10 | 序列化于MP-21 | independent | MP-02 | N/A |
| MP-34 | implementation | merge 拒绝退出码（FF 1 / 三方 2） | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-22,DEP-MP-10 | 序列化于MP-22 | independent | MP-22 | N/A |
| MP-40 | implementation | pull 拒绝退出码（FF 1 / 三方 2） | immutable-release | yes | yes | 5/8 | 2/8 | 1/1 | M | MP-34,DEP-MP-10 | 序列化于MP-34 | independent | MP-34 | N/A |
| MP-03 | implementation | merge --edit 编辑器失败可恢复 | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-40,DEP-MP-10 | 序列化于MP-40 | independent | N/A | N/A |
| MP-52 | implementation | merge --edit 编辑器无法运行可恢复 | immutable-release | yes | yes | 8/8 | 1/8 | 1/2 | M | MP-03,DEP-MP-10 | 序列化于MP-03 | independent | MP-03 | N/A |
| MP-04 | implementation | --into-name 与 merge.suppressDest | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-52,DEP-MP-04,DEP-MP-10 | 序列化于MP-52 | independent | N/A | N/A |
| MP-49 | implementation | octopus 默认合并消息 | immutable-release | yes | yes | 5/8 | 1/8 | 1/2 | M | MP-04,DEP-MP-04,DEP-MP-10 | 序列化于MP-04 | independent | MP-04 | N/A |
| MP-05 | implementation | merge --squash SQUASH_MSG 生成 | immutable-release | yes | yes | 6/8 | 1/8 | 2/2 | M | MP-49,DEP-MP-10 | 序列化于MP-49 | independent | N/A | N/A |
| MP-06 | implementation | unborn 分支 initial pull reflog | immutable-release | yes | yes | 5/8 | 2/8 | 1/1 | M | MP-05,DEP-MP-10 | 序列化于MP-05 | independent | N/A | N/A |
| MP-29 | implementation | reflog 限条输出末尾空行 | immutable-release | yes | yes | 2/8 | 1/8 | 1/1 | M | MP-06,DEP-MP-10 | 序列化于MP-06 | independent | MP-06 | N/A |
| MP-07 | implementation | merge/pull 冲突与 autostash 提示输出流 | immutable-release | yes | yes | 7/8 | 2/8 | 2/4 | M | MP-29,DEP-MP-10 | 序列化于MP-29 | independent | N/A | N/A |
| MP-08 | implementation | --verify-signatures 成功输出 | immutable-release | yes | yes | 5/8 | 1/8 | 2/2 | M | MP-07,DEP-MP-02,DEP-MP-10 | 序列化于MP-07 | independent | N/A | N/A |
| MP-32 | implementation | --verify-signatures 的 gpg.minTrustLevel | immutable-release | yes | yes | 5/8 | 1/8 | 1/2 | M | MP-08,DEP-MP-10 | 序列化于MP-08 | independent | MP-08 | N/A |
| MP-09 | implementation | -s resolve 平凡索引内合并诊断 | immutable-release | yes | yes | 7/8 | 1/8 | 2/2 | M | MP-32,DEP-MP-10 | 序列化于MP-32 | independent | N/A | N/A |
| MP-10 | implementation | MERGE_AUTOSTASH 由 commit 完成合并时应用与清理 | immutable-release | yes | yes | 7/8 | 1/8 | 2/3 | M | MP-09,DEP-MP-10 | 序列化于MP-09 | independent | N/A | N/A |
| MP-43 | implementation | MERGE_AUTOSTASH 由 merge --continue 应用与清理 | immutable-release | yes | yes | 5/8 | 1/8 | 2/2 | M | MP-10,DEP-MP-10 | 序列化于MP-10 | independent | MP-10 | N/A |
| MP-48 | implementation | MERGE_AUTOSTASH 应用冲突的转存 | immutable-release | yes | yes | 6/8 | 1/8 | 2/2 | M | MP-43,DEP-MP-10 | 序列化于MP-43 | independent | MP-10 | N/A |
| MP-39 | implementation | MERGE_AUTOSTASH 的 abort 与快进 squash 路径 | immutable-release | yes | yes | 5/8 | 1/8 | 2/2 | M | MP-48,DEP-MP-10 | 序列化于MP-48 | independent | MP-10 | N/A |
| MP-41 | implementation | 干净三方 squash 的 MERGE_AUTOSTASH 保留与应用 | immutable-release | yes | yes | 7/8 | 1/8 | 2/3 | M | MP-39,DEP-MP-10 | 序列化于MP-39 | independent | MP-39 | N/A |
| MP-45 | implementation | 冲突三方 squash 的 MERGE_AUTOSTASH 保留与应用 | immutable-release | yes | yes | 5/8 | 1/8 | 2/3 | M | MP-41,DEP-MP-10 | 序列化于MP-41 | independent | MP-41 | N/A |
| MP-30 | implementation | MERGE_AUTOSTASH 伪引用与 rev-parse | immutable-release | yes | yes | 3/8 | 2/8 | 2/2 | M | MP-45,DEP-MP-10 | 序列化于MP-45 | independent | MP-10 | N/A |
| MP-44 | implementation | stash show 解析 MERGE_AUTOSTASH | immutable-release | yes | yes | 2/8 | 1/8 | 1/1 | M | MP-30,DEP-MP-10 | 序列化于MP-30 | independent | MP-30 | N/A |
| MP-11 | implementation | 脏工作区重叠路径判定 | immutable-release | yes | yes | 7/8 | 1/8 | 2/2 | M | MP-44,DEP-MP-10 | 序列化于MP-44 | independent | N/A | N/A |
| MP-53 | implementation | 已暂存修改拒绝与冲突中止的脏状态边界 | immutable-release | yes | yes | 5/8 | 1/8 | 2/2 | M | MP-11,DEP-MP-10 | 序列化于MP-11 | independent | MP-11 | N/A |
| MP-12 | implementation | 跳过未变更文件的工作区写入 | immutable-release | yes | yes | 6/8 | 1/8 | 2/2 | M | MP-53,DEP-MP-10 | 序列化于MP-53 | independent | N/A | N/A |
| MP-13 | implementation | pull --rebase ff 优先级矩阵 | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-12,DEP-MP-10 | 序列化于MP-12 | independent | N/A | N/A |
| MP-23 | implementation | pull --rebase 显式上游解析 | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-13,DEP-MP-10 | 序列化于MP-13 | independent | MP-13 | N/A |
| MP-55 | implementation | octopus 显式选择 | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-23,DEP-MP-10 | 序列化于MP-23 | independent | MP-14 | N/A |
| MP-14 | implementation | 多策略链回退与未处理退出码 | immutable-release | yes | yes | 6/8 | 1/8 | 2/2 | M | MP-55,DEP-MP-10 | 序列化于MP-55 | independent | N/A | N/A |
| MP-35 | implementation | 策略名白名单校验 | immutable-release | yes | yes | 6/8 | 1/8 | 1/1 | M | MP-14,DEP-MP-10 | 序列化于MP-14 | independent | MP-15 | N/A |
| MP-38 | implementation | 合法外部策略名的查找结果诊断 | immutable-release | yes | yes | 7/8 | 1/8 | 1/1 | M | MP-35,DEP-MP-10 | 序列化于MP-35 | independent | MP-35 | N/A |
| MP-15 | implementation | 外部策略执行与调用契约 | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-38,DEP-MP-10 | 序列化于MP-38 | independent | N/A | N/A |
| MP-51 | implementation | 外部策略结构化输出隔离 | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-15,DEP-MP-10 | 序列化于MP-15 | independent | MP-15 | N/A |
| MP-37 | implementation | 外部策略退出码 1/2 与冲突状态 | immutable-release | yes | yes | 6/8 | 1/8 | 2/2 | M | MP-51,DEP-MP-10 | 序列化于MP-51 | independent | MP-15 | N/A |
| MP-42 | implementation | 含外部策略的多策略链拒绝 | immutable-release | yes | yes | 8/8 | 1/8 | 1/1 | M | MP-37,DEP-MP-10 | 序列化于MP-37 | independent | MP-37 | N/A |
| MP-16 | implementation | 冲突标记路径标签 | immutable-release | yes | yes | 6/8 | 1/8 | 1/2 | M | MP-42,DEP-MP-10 | 序列化于MP-42 | independent | N/A | N/A |
| MP-17 | implementation | recursive D/F 冲突落盘与报告 | immutable-release | yes | yes | 6/8 | 1/8 | 2/3 | M | MP-16,DEP-MP-10,DEP-MP-11 | 序列化于MP-16 | independent | N/A | N/A |
| MP-24 | implementation | rename/directory 冲突落盘与阶段 | immutable-release | yes | yes | 5/8 | 1/8 | 2/3 | M | MP-17,DEP-MP-10 | 序列化于MP-17 | independent | MP-17 | N/A |
| MP-18 | implementation | rename/delete 冲突纯内容保留 | immutable-release | yes | yes | 7/8 | 1/8 | 2/2 | M | MP-24,DEP-MP-10 | 序列化于MP-24 | independent | N/A | N/A |
| MP-25 | implementation | rename/rename D/F 冲突落盘 | immutable-release | yes | yes | 8/8 | 1/8 | 2/3 | M | MP-18,DEP-MP-10 | 序列化于MP-18 | independent | MP-18 | N/A |
| MP-46 | implementation | rename/rename D/F 冲突报告 | immutable-release | yes | yes | 3/8 | 1/8 | 1/1 | M | MP-25,DEP-MP-10 | 序列化于MP-25 | independent | MP-25 | N/A |
| MP-50 | implementation | rm 处理未合并路径 | immutable-release | yes | yes | 6/8 | 1/8 | 1/1 | M | MP-46,DEP-MP-10 | 序列化于MP-46 | independent | MP-19 | N/A |
| MP-19 | implementation | rename/rename(1to2) 源路径阶段 | immutable-release | yes | yes | 5/8 | 1/8 | 2/2 | M | MP-50,DEP-MP-10 | 序列化于MP-50 | independent | N/A | N/A |
| MP-54 | implementation | 二进制 rename/rename(1to2) 各侧 blob | immutable-release | yes | yes | 4/8 | 1/8 | 1/2 | M | MP-19,DEP-MP-10 | 序列化于MP-19 | independent | MP-19 | N/A |
| MP-20 | implementation | 传递目录重命名冲突报告与内容冲突 | immutable-release | yes | yes | 5/8 | 1/8 | 1/1 | M | MP-54,DEP-MP-10 | 序列化于MP-54 | independent | N/A | N/A |
| MP-47 | implementation | 传递目录重命名的索引阶段与路径清理 | immutable-release | yes | yes | 5/8 | 1/8 | 1/1 | M | MP-20,DEP-MP-10 | 序列化于MP-20 | independent | MP-20 | N/A |

---

### Task MP-01: 修复 `merge --ff` / `--no-ff` / `--ff-only` last-one-wins 覆盖规则（#996）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`0.30.216`；卡特有用例 1/1、解析器单测 1/1、共享回归 13/13、版本守卫 2/2；Codex R3 `VERDICT: PASS`；EN/zh/开发/COMPATIBILITY 与网站 `merge.en.md` 同卡同步；按用户 2026-11-20 指令未跑全量、未等待远端 CI）

**Description:** 将 `MergeArgs` 的 `--ff`、`--no-ff`、`--ff-only` 由互相 `conflicts_with_all` 改为 last-one-wins（命令行最后一个 FF 标志生效，并覆盖 `merge.ff` 配置），与 Git 的 `OPT_SET_INT` 语义一致。（按 G-09 自本卡拆出 `MP-26`、`MP-27`、`MP-28`。）

**Out of scope:** `--commit` 与 `branch.<name>.mergeOptions`（由 `MP-26` 承接）；`--squash` 组合（由 `MP-33`、`MP-27` 承接）；`-s` 与 `-X` 组合（由 `MP-28` 承接）；`--ff-only` 拒绝文案（由 `MP-21` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--ff-only`、`--ff`、`--no-ff` 互相 `conflicts_with_all`，组合使用即被 clap 以 `LBR-CLI-002` 拒绝 | `src/command/merge.rs:313-322` |
| 单个 FF 标志覆盖 `merge.ff`，否则读取 `merge.ff` | `src/command/merge.rs:1262-1278`；`src/command/history_config.rs:39` |

**Acceptance criteria:**

- [x] AC1：在 F-FF 上 `libra merge --ff-only --no-ff topic -m m` 创建合并提交（最后的 `--no-ff` 生效）。
- [x] AC2：在 F-FF 上 `libra merge --no-ff --ff topic` 快进（最后的 `--ff` 生效）。
- [x] AC3：在 F-DIV 上 `libra merge --no-ff --ff-only topic` 按最后的 `--ff-only` 拒绝非快进合并。
- [x] AC4：`merge.ff=only` 时，在 F-DIV 上 `libra merge --ff-only --ff topic -m m` 创建合并提交（最后的 `--ff` 同时覆盖前一标志与配置）。
- [x] AC5：在 F-FF 上 `libra merge --no-ff --ff-only topic` 快进（最后的 `--ff-only` 生效；`#996` AC2 前半与 `t7600#44`）。
- [x] AC6：在 F-FF 上 `libra merge --ff --no-ff topic -m m` 创建合并提交（最后的 `--no-ff` 生效）。
- [x] AC7：`merge.ff=only` 时，在 F-FF 上单个 `libra merge --no-ff topic -m m` 创建合并提交（命令行覆盖配置；`#996` AC3 前半）。
- [x] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::ff_flags_last_one_wins` 承载 AC1–AC7；夹具 F-FF（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`），`main` 仍指向 `B`（可 fast-forward）；夹具 F-DIV（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`）；`main` 自 `B` 再提交 `M`（新增 `h`=`m1\n`），与 `topic` 分叉且无冲突；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-FF；命令退出 0；HEAD 的父提交恰为 `[B, T]`（Git 2.54.0 预探：`Merge made by the 'ort' strategy.`，2 个父提交）。
- AC2：F-FF；命令退出 0；`libra rev-parse HEAD` 等于 `T`（Git 2.54.0 预探：`Fast-forward`）。
- AC3：F-DIV；命令以非 0 退出，stderr 含 `Error-Code: LBR-CONFLICT-002`（Git 2.54.0 预探：退出 128、`fatal: Not possible to fast-forward, aborting.`；文案由 `MP-21` 对齐）。
- AC4：F-DIV，先 `libra config merge.ff only`；命令退出 0；HEAD 的父提交恰为 `[M, T]`。
- AC5：F-FF（重建）；命令退出 0；`libra rev-parse HEAD` 等于 `T`（Git 2.54.0 预探：`Fast-forward`；按 `--no-ff` 生效的实现会创建合并提交）。
- AC6：F-FF（重建）；命令退出 0；HEAD 的父提交恰为 `[B, T]`（Git 2.54.0 预探：`Merge made by the 'ort' strategy.`；按 `--ff` 生效的实现会快进到 `T`）。
- AC7：F-FF（重建），先 `libra config merge.ff only`；命令退出 0；HEAD 的父提交恰为 `[B, T]`（Git 2.54.0 预探；按配置 `only` 生效的实现会快进到 `T`。`#996` AC3 后半 `merge.ff=false` + `--ff` 由共享回归组中既有用例 `merge_ff_cli_flag_overrides_false_config`（`tests/compat/config_history_defaults_test.rs:108`）守护）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::ff_flags_last_one_wins)'`（`nextest list` 恰好 1 条；运行 1/1 PASS，2026-11-20 `/tmp/opencode/mp-01-tests-r3.log`）（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::ff_flags_last_one_wins)'` 须恰好列出该 1 条）
- [x] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_config_history_defaults`（整 target，`merge.ff` 等历史默认配置；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_config_history_defaults` 记录 `N` = 13；运行 13/13 PASS）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `DEP-MP-01`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`libra merge --ff-only --no-ff`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge --ff/--no-ff/--ff-only 覆盖; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DEP-MP-01,DEP-MP-10; writeset=序列化于DEP-MP-10; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-26: 新增 `merge --commit` 并接入 `branch.<name>.mergeOptions` 的 `--commit`（#990）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-01` 拆出：为 `MergeArgs` 新增 `--commit`（与 `--no-commit` last-one-wins），并在 `DEP-MP-05`（`I978-01`）合入的 `branch.<name>.mergeOptions` 读取器中接受 `--commit` 令牌（白名单尚未包含时在本卡增补），使命令行 `--commit` 覆盖配置的 `--no-commit`。`--squash` 与最终生效的 `--commit` 组合在本卡先以 clap 层 `LBR-CLI-002` 拒绝（避免中间版本放行无效组合），Git 文案与配置后校验由 `MP-33` 承接。（按 G-09 自本卡拆出 `MP-33`。）

**Out of scope:** `--ff*` 覆盖（由 `MP-01` 承接）；`--squash` 与最终生效的 `--commit` 的配置后拒绝与 Git 文案（由 `MP-33` 承接）；`--squash` 与最终生效的 `--no-commit` 的组合放行（`DEFER-MP-07`）；`--squash --no-ff`（由 `MP-27` 承接）；`mergeOptions` 读取器本身（由 `issues/978.md` 的 `I978-01` 承接，`DEP-MP-05`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `MergeArgs` 无 `--commit`；`--no-commit` 与 `--squash` 在 clap 层互斥 | `src/command/merge.rs:385-391` |
| `history_config.rs` 当前无 `branch.<name>.mergeOptions` 读取器（`I978-01` 未合入） | `src/command/history_config.rs:1-39`；`docs/development/plan/issues/978.md:3,41-44` |

**Acceptance criteria:**

- [ ] AC1：在 F-DIV 上 `libra merge --no-commit --commit topic -m m` 创建合并提交（最后的 `--commit` 生效）。
- [ ] AC2：在 F-DIV 上 `libra merge --commit --no-commit topic` 不创建提交（最后的 `--no-commit` 生效）。
- [ ] AC3：AC2 停下的待决合并可由 `libra commit` 完成为 `M` 与 `T` 的合并提交（合并结果已暂存、待决状态已记录）。
- [ ] AC4：`branch.main.mergeOptions=--no-commit` 时，在 F-DIV 上 `libra merge --commit topic -m m` 创建合并提交（命令行覆盖配置）。
- [ ] AC5：配置值中的 `--commit` 令牌被读取并按 last-one-wins 生效：`branch.main.mergeOptions` 为 `--no-commit --commit` 时，在 F-DIV 上 `libra merge topic -m m` 创建合并提交。
- [ ] AC6：在 F-DIV 上 `libra merge --squash --commit topic` 被拒绝（本卡发布的中间行为）。
- [ ] AC7：`branch.main.mergeOptions=--no-commit` 且命令行无覆盖时，在 F-DIV 上 `libra merge topic -m m` 不创建提交（配置被读取，作为 AC4/AC5 的对照）。
- [ ] AC8：AC7 停下的待决合并可由 `libra commit` 完成为 `M` 与 `T` 的合并提交。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::commit_flag_and_merge_options_commit` 承载 AC1–AC8；夹具 F-DIV（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`）；`main` 自 `B` 再提交 `M`（新增 `h`=`m1\n`），与 `topic` 分叉且无冲突；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-DIV；退出 0；HEAD 的父提交恰为 `[M, T]`（Git 2.54.0 预探）。
- AC2：F-DIV；退出 0（执行条件；Git 2.54.0 预探：`Automatic merge went well; stopped before committing as requested`）；`libra rev-parse HEAD` 等于 `M`（忽略 `--no-commit` 的实现会创建合并提交使 HEAD 前移）。
- AC3：AC2 之后 `libra commit --no-gpg-sign -m done` 退出 0（执行条件）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree ce02ae3e3a7e99e2b8c9a286eafcfbb390725072`、`parent <M>`、`parent <T>`（`<M>`/`<T>` 为完整 OID；该树即 `f`=`base\n`、`g`=`t1\n`、`h`=`m1\n`，Git 2.54.0 预探同夹具 `git merge --commit --no-commit topic` 后 `git commit` 所得；未把合并结果写入索引或未记录待决合并的实现得到不同的 `tree` 行或单个 `parent` 行，提前提交的实现使该 `commit` 因无可提交内容以 `LBR-REPO-003` 失败）。
- AC4：F-DIV，`libra config branch.main.mergeOptions -- --no-commit`（值以 `-` 开头须在 `--` 之后，同 `#990` 复现）；退出 0；HEAD 的父提交恰为 `[M, T]`（Git 2.54.0 预探）。
- AC5：F-DIV，`libra config branch.main.mergeOptions -- '--no-commit --commit'`；退出 0；HEAD 的父提交恰为 `[M, T]`（若 `--commit` 令牌被忽略，前一令牌 `--no-commit` 生效，HEAD 将停在 `M`）。
- AC6：F-DIV；命令以非 0 退出，stderr 含 `Error-Code: LBR-CLI-002`。
- AC7：F-DIV（重建），同 AC4 配置；退出 0（执行条件；Git 2.54.0 预探：`Automatic merge went well; stopped before committing as requested`）；`libra rev-parse HEAD` 等于 `M`（忽略整个配置值的实现会创建合并提交）。
- AC8：AC7 之后 `libra commit --no-gpg-sign -m done` 退出 0（执行条件）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree ce02ae3e3a7e99e2b8c9a286eafcfbb390725072`、`parent <M>`、`parent <T>`（`<M>`/`<T>` 为完整 OID；该树即 `f`=`base\n`、`g`=`t1\n`、`h`=`m1\n`，Git 2.54.0 预探同夹具 `git merge --commit --no-commit topic` 后 `git commit` 所得；未把合并结果写入索引或未记录待决合并的实现得到不同的 `tree` 行或单个 `parent` 行，提前提交的实现使该 `commit` 因无可提交内容以 `LBR-REPO-003` 失败）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::commit_flag_and_merge_options_commit)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::commit_flag_and_merge_options_commit)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_config_history_defaults`（整 target，`merge.*` 历史默认配置；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_config_history_defaults` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改共享配置读取器 src/command/history_config.rs（ER-13 T-1）` | **Dependencies:** `MP-01`, `DEP-MP-05`, `DEP-MP-06`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/history_config.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/history_config.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`--commit`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge --commit 与 mergeOptions --commit; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=MP-01,DEP-MP-05,DEP-MP-06,DEP-MP-10; writeset=序列化于MP-01; release=independent; split-from=MP-01; exception=N/A`

---

### Task MP-33: 在 `mergeOptions` 解析后以 Git 文案拒绝 `--squash` 与最终生效的 `--commit` 组合（#990, #985）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-26` 拆出：把 `MP-26` 在 clap 层对 `--squash` 与 `--commit` 的互斥改为在 `branch.<name>.mergeOptions` 与命令行合并解析**之后**按 Git 规则校验：`--squash` 与最终生效的 `--commit` 组合（与参数顺序无关，也包括 `--squash` 来自 `mergeOptions` 的情形）在任何索引、工作树、引用或待决状态写入之前以 `options '--squash' and '--commit.' cannot be used together` 拒绝；稳定码沿用 `MP-26` 发布的 `LBR-CLI-002`，由共享回归组中 `MP-26` 用例 AC6 的既有断言继续守护，本卡不另立稳定码 AC。`--no-commit` 与 `--squash` 的 clap 互斥（`src/command/merge.rs:385-391`）不变：最终生效为 `--no-commit` 时与 `--squash` 的组合放行属 `DEFER-MP-07`，`issues/978.md` 中 `I978-01` 对「配置 `--squash` + 显式 `--no-commit`」的 fail-closed 规定（`ADR-I978-02`、AC18）因此保持有效、本卡不修改该计划。

**Out of scope:** `--commit` 本身与 `mergeOptions` 的 `--commit` 令牌（由 `MP-26` 承接）；`--squash --no-ff`（由 `MP-27` 承接）；`SQUASH_MSG` 正文（由 `MP-05` 承接）；`--squash` 与最终生效的 `--no-commit` 的组合放行（命令行 `--no-commit --squash`、`--commit --no-commit --squash` 与 `mergeOptions=--squash` + `--no-commit`；Git 允许，Libra 保持拒绝，`DEFER-MP-07`，`COMPATIBILITY.md` 记为有意差异）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `MP-26` 发布后 `--squash` 与 `--commit` 在 clap 层互斥（`LBR-CLI-002`，clap 文案）；`--no-commit` 与 `--squash` 同样 clap 互斥；只在 clap 层校验会被 `mergeOptions` 绕过 | `src/command/merge.rs:385-391`；`MP-26` AC6 |
| Git 在 `--squash` 且 `option_commit > 0` 时 `die("options '%s' and '%s' cannot be used together", "--squash", "--commit.")`，在解析完全部选项与 `mergeOptions` 之后判定，与参数顺序无关 | `git@6de20f6092` `builtin/merge.c:1503-1508` |
| `issues/978.md` 的 `ADR-I978-02` 与 `I978-01` AC18 规定「配置 `--squash` + 显式 `--no-commit`」fail-closed；本卡不放行该组合，与其一致 | `docs/development/plan/issues/978.md:76,90` |

**Acceptance criteria:**

- [ ] AC1：在 F-FF 上 `libra merge --squash --commit topic` 以 Git 文案拒绝。
- [ ] AC2：在 F-FF 上反序的 `libra merge --commit --squash topic` 以同一 Git 文案拒绝（`#985` AC2 的另一参数顺序）。
- [ ] AC3：`branch.main.mergeOptions=--squash` 时，在 F-FF 上 `libra merge --commit topic` 以同一 Git 文案拒绝（配置后校验）。
- [ ] AC4：AC1 的拒绝不改变仓库快照。
- [ ] AC5：AC2 的拒绝不改变仓库快照。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::squash_commit_validated_after_merge_options` 承载 AC1–AC5；夹具 F-FF（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`），`main` 仍指向 `B`（可 fast-forward）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-FF；命令以非 0 退出；stderr 含 `options '--squash' and '--commit.' cannot be used together`（Git 2.54.0 预探：`fatal: options '--squash' and '--commit.' cannot be used together`、退出 128）。
- AC2：F-FF（重建）；命令以非 0 退出；stderr 含 `options '--squash' and '--commit.' cannot be used together`（Git 2.54.0 预探：反序同样输出该行、退出 128）。
- AC3：F-FF（重建），`libra config branch.main.mergeOptions -- --squash`；命令以非 0 退出；stderr 含 `options '--squash' and '--commit.' cannot be used together`（Git 2.54.0 预探）。
- AC4：AC1 调用前后的「仓库快照 Σ」逐项相等（`#985` AC2：HEAD、索引与工作树不变；先暂存或改写工作树再拒绝的实现使 Σ(b)/(c) 变化）。
- AC5：AC2 调用前后的「仓库快照 Σ」逐项相等（同 AC4 的判别理由）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::squash_commit_validated_after_merge_options)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::squash_commit_validated_after_merge_options)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改共享配置读取器 src/command/history_config.rs（ER-13 T-1）` | **Dependencies:** `MP-26`, `DEP-MP-05`, `DEP-MP-06`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/history_config.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/history_config.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`cannot be used together`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`COMPATIBILITY.md` `| merge |` 行必须更新：记录 `--squash` 与最终生效的 `--no-commit` 仍互斥（Git 允许）的有意差异（`DEFER-MP-07`）；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--squash 与最终生效 --commit 的配置后拒绝; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=MP-26,DEP-MP-05,DEP-MP-06,DEP-MP-10; writeset=序列化于MP-26; release=independent; split-from=MP-26; exception=N/A`

---

### Task MP-27: 对齐 `merge --squash` 与 `--no-ff` 组合拒绝及 `--squash --ff-only`（#985）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--squash` 与最终生效的 `--no-ff`/`merge.ff=false` 在任何写入前以 Git 文案拒绝；`--squash --ff-only` 暂存且不移动 HEAD；merge 317/318，唯一失败为既有 crisscross GC 用例。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）

**Description:** 自 `MP-01` 拆出：`--squash` 与最终生效的 `--no-ff`（命令行或 `merge.ff=false`）组合以 Git 文案 `options '--squash' and '--no-ff.' cannot be used together` 在任何写入前拒绝；`--squash --ff-only` 在可快进时照常暂存 squash 结果且不移动 HEAD。

**Out of scope:** `--squash` 与 `--commit`/`--no-commit`（由 `MP-26`/`MP-33` 承接）；`--ff*` 覆盖（由 `MP-01` 承接）；`SQUASH_MSG` 正文（由 `MP-05` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--squash` 未与 `--no-ff` 设互斥或配置后校验 | `src/command/merge.rs:316-320,385-386` |
| Git 在 `--squash` 且 `fast_forward == FF_NO`（含 `merge.ff=false`）时 `die(... "--squash", "--no-ff.")` | `git@6de20f6092` `builtin/merge.c:1503-1505` |

**Acceptance criteria:**

- [ ] AC1：在 F-FF 上 `libra merge --squash --no-ff topic` 以 Git 文案拒绝。
- [ ] AC2：在 F-FF 上 `libra merge --no-ff --squash topic` 以同一文案拒绝（与顺序无关）。
- [ ] AC3：`merge.ff=false` 时，在 F-FF 上 `libra merge --squash topic` 以同一文案拒绝。
- [ ] AC4：AC1 的拒绝不改变仓库快照。
- [ ] AC5：AC2 的拒绝不改变仓库快照。
- [ ] AC6：AC3 的拒绝不改变仓库快照。
- [ ] AC7：在 F-FF 上 `libra merge --squash --ff-only topic` 暂存 squash 结果。
- [ ] AC8：AC7 的调用不移动 HEAD。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::squash_no_ff_rejected_and_ff_only_allowed` 承载 AC1–AC8；夹具 F-FF（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`），`main` 仍指向 `B`（可 fast-forward）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-FF；以非 0 退出；stderr 含 `options '--squash' and '--no-ff.' cannot be used together`（Git 2.54.0 预探，退出 128）。
- AC2：F-FF；以非 0 退出；stderr 含 `options '--squash' and '--no-ff.' cannot be used together`（Git 2.54.0 预探）。
- AC3：F-FF，`libra config merge.ff false`；以非 0 退出；stderr 含 `options '--squash' and '--no-ff.' cannot be used together`（Git 2.54.0 预探）。
- AC4：AC1 调用前后的「仓库快照 Σ」逐项相等。
- AC5：AC2 调用前后的「仓库快照 Σ」逐项相等。
- AC6：AC3 调用前后的「仓库快照 Σ」逐项相等。
- AC7：F-FF；退出 0；`libra ls-files -s` 的 stdout 恰为 `100644 <base> 0\tf\n100644 <t1> 0\tg\n`，`<base>`/`<t1>` 为 `libra hash-object` 对 `base\n`/`t1\n` 的 OID（Git 2.54.0 预探）。
- AC8：AC7 之后 `libra rev-parse HEAD` 等于 `B`（Git 2.54.0 预探：`Squash commit -- not updating HEAD`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`squash_no_ff_rejected_and_ff_only_allowed`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-33`, `DEP-MP-06`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`--squash --no-ff`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge --squash 与 --no-ff 组合拒绝; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-33,DEP-MP-06,DEP-MP-10; writeset=序列化于MP-33; release=independent; split-from=MP-01; exception=N/A`

---

### Task MP-28: 允许 `merge --strategy` 与 `--strategy-option` 同时使用（#1015）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：移除 `--strategy` 与 `--strategy-option` 互斥，recursive/ort + theirs/ours 组合可用；merge 316/317，唯一失败为既有 crisscross GC 用例。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）

**Description:** 自 `MP-01` 拆出：移除 `MergeArgs` 中 `strategy` 与 `strategy_option` 的互斥，使 `-s recursive -Xtheirs` 等组合把 `-X` 选项透传给所选内置策略。

**Out of scope:** 多策略链（由 `MP-14` 承接）与 `-s octopus`（由 `MP-55` 承接）；外部策略（由 `MP-35`/`MP-15` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--strategy` 与 `--strategy-option` 互相 `conflicts_with_all` | `src/command/merge.rs:326,331` |

**Acceptance criteria:**

- [ ] AC1：在 F-CONF 上 `libra merge -s recursive -Xtheirs topic -m m` 以 `theirs` 解决冲突。
- [ ] AC2：AC1 的调用创建合并提交。
- [ ] AC3：在 F-CONF 上 `libra merge -s ort -Xours topic -m m` 以 `ours` 解决冲突。
- [ ] AC4：AC3 的调用创建合并提交。
- [ ] AC5：长选项 `--strategy=recursive` 与空格分隔的 `-X theirs` 组合同样以 `theirs` 解决冲突（`#1015` AC3）。
- [ ] AC6：`-s recursive` 与空格分隔的 `-X ours` 组合以 `ours` 解决冲突并创建合并提交（`#1015` AC2）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::strategy_with_strategy_option` 承载 AC1–AC6；夹具 F-CONF（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；`topic` 自 `B` 提交 `T`（`f`=`theirs\n`）；`main` 自 `B` 提交 `M`（`f`=`ours\n`），合并 `topic` 时 `f` 内容冲突；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-CONF；退出 0（执行条件）；`libra show HEAD:f` 的 stdout 恰为 `theirs\n`（Git 2.54.0 预探）。
- AC2：AC1 之后 HEAD 的父提交恰为 `[M, T]`（Git 2.54.0 预探）。
- AC3：F-CONF（重建）；退出 0；`libra show HEAD:f` 的 stdout 恰为 `ours\n`（Git 2.54.0 预探）。
- AC4：AC3 之后 HEAD 的父提交恰为 `[M, T]`（Git 2.54.0 预探）。
- AC5：F-CONF（重建）；`libra merge --strategy=recursive -X theirs topic -m m` 退出 0（执行条件）；`libra show HEAD:f` 的 stdout 恰为 `theirs\n`（Git 2.54.0 预探：同一调用得 `theirs`、双父提交）。
- AC6：F-CONF（重建）；`libra merge -s recursive -X ours topic -m m` 退出 0（执行条件）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree <Mt>`、`parent <M>`、`parent <T>`（`<Mt>` 为 `libra rev-parse 'M^{tree}'`——`-X ours` 的结果树即 `M` 的树（`f`=`ours\n`）；`<M>`/`<T>` 为完整 OID；Git 2.54.0 预探同形。因结果内容与 `M` 相同，只比较 `f` 的内容无法区分「未执行合并而直接成功」的实现，该实现在此缺少第二个 `parent` 行）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`strategy_with_strategy_option`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-27`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`-s recursive -Xtheirs`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge -s 与 -X 组合透传; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-27,DEP-MP-10; writeset=序列化于MP-27; release=independent; split-from=MP-01; exception=N/A`

---

### Task MP-02: 修复 `merge --no-commit` 在可 fast-forward 时的行为（#992）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--no-commit` 在可快进时仍直接快进；compat 两处夹具按卡修订；merge 315/316，唯一失败为既有 crisscross GC 用例，与本卡无关。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）

**Description:** 使 `merge --no-commit` 在目标可 fast-forward 时与 Git 一致直接快进（更新 HEAD、索引与工作树，不留待决合并），只有 `--no-ff`（或 `merge.ff=false`）时才停在待提交的合并状态。（按 G-09 自本卡拆出 `MP-21`、`MP-22`。）

**Out of scope:** `--ff-only` 拒绝文案（由 `MP-21` 承接）；冲突退出码（由 `MP-22` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `merge_is_fast_forward` 要求 `!options.no_commit`，`--no-commit` 因此不走快进 | `src/command/merge.rs:2573-2578` |
| 既有用例 `compat_config_history_defaults` 的 `no_commit_continue_replays_resolved_message`（`tests/compat/config_history_defaults_test.rs:291`）与 `merge_cli_ff_only_combines_with_squash_and_no_commit`（同文件 `:351`）在可快进夹具上执行 `--no-commit` 后 `merge --continue` 并断言合并提交；本卡按「验证执行协议」第 5 条逐个改写：`no_commit_continue_replays_resolved_message` 改用分叉夹具（不带 `--ff-only`，保留「`--no-commit` 后由 `--continue` 完成并沿用记录的消息」的被测意图）；`merge_cli_ff_only_combines_with_squash_and_no_commit` 中 `--ff-only --no-commit` 在可快进夹具上的断言改为「快进且 HEAD 等于 `feature`，不再执行 `--continue`」（分叉时 `--ff-only` 按 Git `die_ff_impossible` 拒绝），其 squash 部分断言不变；全量门中暴露的其它同类既有断言按同一规则判定、登记到本卡并按 ER-03 补入写集 | `rg -n 'no-commit' tests/compat/config_history_defaults_test.rs`（2026-11-18） |

**Acceptance criteria:**

- [ ] AC1：在 F-FF 上 `libra merge --no-commit topic` 快进 HEAD。
- [ ] AC2：AC1 之后索引与 HEAD 的树一致。
- [ ] AC3：AC1 之后工作树与 HEAD 的树一致。
- [ ] AC4：AC1 之后不存在待决合并。
- [ ] AC5：在 F-FF 上 `libra merge --no-commit --no-ff topic` 不移动 HEAD。
- [ ] AC6：AC5 之后 `libra merge --continue -m m` 完成为 `B` 与 `T` 的合并提交（含被并入的内容）。
- [ ] AC7：`merge.ff=false` 时在 F-FF 上 `libra merge --no-commit topic` 不移动 HEAD。
- [ ] AC8：AC7 之后 `libra merge --continue -m m` 完成为 `B` 与 `T` 的合并提交（含被并入的内容）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::no_commit_fast_forwards` 承载 AC1–AC8；夹具 F-FF（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`），`main` 仍指向 `B`（可 fast-forward）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-FF；退出 0；`libra rev-parse HEAD` 等于 `T`（Git 2.54.0 预探：`Fast-forward`）。
- AC2：AC1 之后 `libra write-tree` 的 stdout 等于 `libra rev-parse 'HEAD^{tree}'` 的 stdout。
- AC3：AC1 之后的「工作树清单」（「验证执行协议」第 8 条）恰为两项：普通文件 `f`=`base\n` 与 `g`=`t1\n`，均不可执行（`T` 的树，无其它路径）。
- AC4：AC1 之后 `.libra/merge-state.json` 不存在。
- AC5：F-FF（重建）；退出 0；`libra rev-parse HEAD` 等于 `B`（Git 2.54.0 预探：`Automatic merge went well; stopped before committing as requested`）。
- AC6：AC5 之后执行该命令退出 0（执行条件）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree <Tt>`、`parent <B>`、`parent <T>`（`<Tt>` 为 `libra rev-parse 'T^{tree}'`，即 `f`=`base\n` 与 `g`=`t1\n` 的合并树；记录两个父提交却未暂存或未提交 `g` 的实现得到不同的 `tree` 行）。
- AC7：F-FF（重建），`libra config merge.ff false`；退出 0；`libra rev-parse HEAD` 等于 `B`（Git 2.54.0 预探：`Automatic merge went well; stopped before committing as requested`）。
- AC8：AC7 之后执行该命令退出 0（执行条件）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree <Tt>`、`parent <B>`、`parent <T>`（同 AC6 的定义与判别理由；Git 2.54.0 预探：完成后为双父提交、树含 `g`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`no_commit_fast_forwards`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_noninteractive_history_controls`（整 target，非交互历史控制（含 `--no-commit`）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_noninteractive_history_controls` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_config_history_defaults`（整 target，本卡同步改写其中两个 `--no-commit` 用例的夹具；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_config_history_defaults` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-28`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `tests/compat/config_history_defaults_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `tests/compat/config_history_defaults_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`--no-commit`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge --no-commit fast-forward; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-28,DEP-MP-10; writeset=序列化于MP-28; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-21: 对齐 `--ff-only` 拒绝文案（`merge` 与 `pull`）及 `advice.diverging`（#1002）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`merge`/`pull --ff-only` 输出 `Not possible to fast-forward, aborting.` 与分叉建议块，保留 LBR-CONFLICT-002 与既有 hints；双 fmt + 严格 clippy 通过。C 组 patch +1。偏差：advice.diverging=false 的抑制未单独接线；既有 crisscross GC 用例失败与本卡无关；用户已取消独立 Claude/Codex 审计与全量测试。）

**Description:** 自 `MP-02` 拆出：`merge --ff-only` 与 `pull --ff-only`（含 `merge.ff=only`、`pull.ff=only` 配置使生效模式为 ff-only 的调用）无法快进时输出 Git 的 `fatal: Not possible to fast-forward, aborting.` 行，并在 `advice.diverging` 未设为 `false` 时于 stderr 输出 Git 的分叉建议块（首行 `Diverging branches can't be fast-forwarded, you need to either:`）；稳定码保持 `LBR-CONFLICT-002`。

**Out of scope:** 冲突与拒绝退出码（由 `MP-22`、`MP-34` 承接）；其它 `advice.*` 键。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `merge` 的 `NonFastForward` 文案为 `non-fast-forward merge refused (...)` 并附 Libra 提示 | `src/command/merge.rs:873,1014-1017` |
| `pull` 的独立映射同样使用该文案；既有用例 `command::pull_test::test_pull_ff_only_diverged_remote_rejects_without_changing_head_or_worktree` 断言 JSON 报告的消息含 `non-fast-forward`（本卡按「验证执行协议」第 5 条只把该断言改为新文案，其余断言保留） | `src/command/pull.rs:1176,1216-1219`；`tests/command/pull_test.rs:718-770` |
| Libra 当前不读取 `advice.*` 配置 | `rg -n 'advice\.diverging' src/` 零命中（2026-11-18） |
| 其它既有断言也依赖旧文案 `non-fast-forward`：`tests/command/merge_test.rs:1650`、`tests/compat/config_history_defaults_test.rs:345,391,466`、`tests/compat/config_defaults_semantics_test.rs:629,731`；本卡按「验证执行协议」第 5 条只把这些断言改为新文案（含稳定码的断言保留） | `rg -n 'non-fast-forward' tests/`（2026-11-18；`merge_test.rs:2276` 断言 `--help` 文字、`merge_test.rs:6501` 只是断言消息文本，均不受影响） |

**Acceptance criteria:**

- [ ] AC1：在 F-DIV 上 `libra merge --ff-only topic` 的 stderr 含 Git 拒绝行。
- [ ] AC2：AC1 的 stderr 含完整的分叉建议块。
- [ ] AC3：`advice.diverging=false` 时 `libra merge --ff-only topic` 不输出分叉建议。
- [ ] AC4：F-PULL 的 D 中 `libra pull --ff-only` 的 stderr 含 Git 拒绝行。
- [ ] AC5：AC4 的 stderr 含完整的分叉建议块。
- [ ] AC6：`advice.diverging=false` 时 `libra pull --ff-only` 不输出分叉建议。
- [ ] AC7：`merge.ff=only` 时在 F-DIV 上不带 FF 标志的 `libra merge topic -m m` 输出同一 Git 拒绝行。
- [ ] AC8：`pull.ff=only` 时 F-PULL 的 D 中不带 FF 标志的 `libra pull` 输出同一 Git 拒绝行。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::ff_only_refusal_message_and_advice` 承载 AC1–AC3、AC7；`(new)` `command::pull_test::ff_only_refusal_message_and_advice` 承载 AC4–AC6、AC8；夹具 F-DIV（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`）；`main` 自 `B` 再提交 `M`（新增 `h`=`m1\n`），与 `topic` 分叉且无冲突；pull 夹具（新用例写在 `tests/command/pull_test.rs`，复用该模块的 `create_remote_fixture`、`push_remote_commit`、`configure_pull_tracking`，`tests/command/pull_test.rs:87-147`）：`create_remote_fixture()` 以 `git` 建裸仓库 `remote.git` 与工作目录 `workdir` 并推送初始提交 `B`（`README.md`=`hello libra\n`），分支名 `<br>` 取其返回值；本地仓库 D = `init_repo_via_cli` + `configure_identity_via_cli`（身份 `Test User <test@example.com>`，pull 用例不依赖身份）+ `configure_pull_tracking(D, remote.git, <br>)` 后 `libra pull` 得到 `B`；`push_remote_commit(workdir, <br>, <path>, <content>, <msg>)` 推进上游，D 中本地提交带 `--no-gpg-sign`；F-PULL：上游 `push_remote_commit` 提交 `U1`（新增 `remote.txt`），D 本地提交 `D1`（新增 `local.txt`），二者分叉；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出；stderr 含一行恰为 `fatal: Not possible to fast-forward, aborting.`（Git 2.54.0 预探，退出 128）。
- AC2：stderr 含子串 `B_ADV` = `hint: Diverging branches can't be fast-forwarded, you need to either:\nhint:\nhint: \tlibra merge --no-ff\nhint:\nhint: or:\nhint:\nhint: \tlibra rebase\nhint:\nhint: Disable this message with "libra config advice.diverging false"\n`（Git 2.54.0 预探的 10 行建议块逐行相同，只把 `git merge`/`git rebase`/`git config set` 换成 Libra 的 `libra merge`/`libra rebase`/`libra config`；`git@6de20f6092` `advice.c:244-255`）。
- AC3：`libra config advice.diverging false` 后同 AC1 调用以非 0 退出；stderr 不含 `Diverging branches can't be fast-forwarded`、`libra merge --no-ff`、`libra rebase`、`advice.diverging` 中的任何一个（建议块全部特征行；Git 2.54.0 预探：整块不输出）。
- AC4：F-PULL（pull 用例的首条映射，新建）；以非 0 退出；stderr 含一行恰为 `fatal: Not possible to fast-forward, aborting.`（Git 2.54.0 预探）。
- AC5：stderr 含 AC2 定义的子串 `B_ADV`（Git 2.54.0 预探）。
- AC6：D 中 `libra config advice.diverging false` 后同 AC4 调用以非 0 退出；stderr 不含 `Diverging branches can't be fast-forwarded`、`libra merge --no-ff`、`libra rebase`、`advice.diverging` 中的任何一个。
- AC7：重建 F-DIV（不设 `advice.diverging`），`libra config merge.ff only`；该调用以非 0 退出；stderr 含一行恰为 `fatal: Not possible to fast-forward, aborting.`（`#1002` Expected 4；Git 2.54.0 预探同形：`git -c merge.ff=only merge topic` 输出同一建议块与该行、退出 128；`git@6de20f6092` `builtin/merge.c:720-721,1759`：`merge.ff=only` 设为 `FF_ONLY` 后进入同一 `die_ff_impossible`）。
- AC8：重建 F-PULL，D 中 `libra config pull.ff only`（不设 `advice.diverging`）；`libra pull` 以非 0 退出；stderr 含一行恰为 `fatal: Not possible to fast-forward, aborting.`（`#1002` AC2；`git@6de20f6092` `builtin/pull.c:1048-1049,1150-1152`：`pull.ff=only` 经 `config_get_ff` 成为 `--ff-only` 后进入同一 `die_ff_impossible`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC3、AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::ff_only_refusal_message_and_advice)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::ff_only_refusal_message_and_advice)'` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC4–AC6、AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::pull_test::ff_only_refusal_message_and_advice)'`（`(new)`，在 `tests/command/pull_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::pull_test::ff_only_refusal_message_and_advice)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::pull_test::/)'`（`command::pull_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_config_history_defaults`（整 target，本卡同步改写其中的旧文案断言；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_config_history_defaults` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_config_defaults_semantics`（整 target，本卡同步改写其中的旧文案断言；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_config_defaults_semantics` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-02`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/pull.rs`, `tests/command/merge_test.rs`, `tests/command/pull_test.rs`, `tests/compat/config_history_defaults_test.rs`, `tests/compat/config_defaults_semantics_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/pull.rs`, `tests/command/merge_test.rs`, `tests/command/pull_test.rs`, `tests/compat/config_history_defaults_test.rs`, `tests/compat/config_defaults_semantics_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`（由 `--only merge pull` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Not possible to fast-forward`。逐文件语义核对：`docs/development/commands/merge.md`、`docs/development/commands/pull.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行；`| pull |` 第 225 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--ff-only 拒绝文案与 advice.diverging; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=MP-02,DEP-MP-10; writeset=序列化于MP-02; release=independent; split-from=MP-02; exception=N/A`

---

### Task MP-22: `merge` 与 `pull` 冲突合并以退出码 `1` 结束（#1028）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-02` 拆出：按 `ADR-MP-01`，`merge`（含 `--squash`）与 `pull` 在内容/树冲突停止时以 `CliError::with_exit_code(1)` 退出（稳定码保持 `LBR-CONFLICT-002`，冲突状态照常保留），并在 `docs/error-codes.md`「Per-command exit-code overrides」登记。（按 G-09 自本卡拆出 `MP-34`。）

**Out of scope:** 脏工作区与覆盖拒绝退出码（由 `MP-34`、`MP-40` 承接）；多策略未处理退出码 `2`（由 `MP-14` 承接）；冲突提示的输出流（由 `MP-07` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `PullMergeError::Conflicts { squash: false }` 经 `CliError::failure` 以 128 退出 | `src/command/merge.rs:1021-1028`；`src/command/pull.rs:1223-1229` |
| `with_exit_code` 优先于 `LIBRA_FINE_EXIT_CODES` | `src/utils/error.rs:1061-1076`；`docs/error-codes.md:60-62` |
| Git 2.54.0 预探：冲突的 `git merge` 与 `git pull` 均退出 1 | `git@6de20f6092` `builtin/merge.c` `suggest_conflicts` |
| 既有用例断言冲突的 `merge`/`pull` 退出 128（例：`tests/command/merge_test.rs:2834,2905,3045,4121`、`tests/command/merge_test/auto_merge.rs:40`、`tests/command/merge_test/quit.rs:17`、`tests/command/pull_test.rs:990`）；本卡按「验证执行协议」第 5 条把其中断言冲突路径退出码的既有断言改为 1、其余断言不变。开工时以 `rg -n 'Some\(128\)' tests/` 结合用例内容逐条判定（只改被测命令是冲突停止的 `merge`/`pull` 的断言），判定清单写入本卡 `Current evidence`，受影响文件按 ER-03 补入写集 | `rg -n 'Some\(128\)' tests/command/merge_test.rs tests/command/merge_test/ tests/command/pull_test.rs`（2026-11-18） |

**Acceptance criteria:**

- [ ] AC1：默认配置下 `libra merge topic`（F-CONF）以退出码 1 结束。
- [ ] AC2：`LIBRA_FINE_EXIT_CODES=1` 时同一冲突合并仍以退出码 1 结束。
- [ ] AC3：冲突退出的 stderr 稳定码保持 `LBR-CONFLICT-002`。
- [ ] AC4：AC1 之后冲突的索引阶段保留。
- [ ] AC5：默认配置下 `libra merge --squash topic`（F-CONF）冲突以退出码 1 结束。
- [ ] AC6：默认配置下 `libra pull`（F-PULLC 的 D，非 rebase）冲突以退出码 1 结束。
- [ ] AC7：`docs/error-codes.md` 登记 merge/pull 冲突退出 1。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::conflict_exits_with_code_1` 承载 AC1–AC5；`(new)` `command::pull_test::conflict_exits_with_code_1` 承载 AC6；夹具 F-CONF（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；`topic` 自 `B` 提交 `T`（`f`=`theirs\n`）；`main` 自 `B` 提交 `M`（`f`=`ours\n`），合并 `topic` 时 `f` 内容冲突；pull 夹具（新用例写在 `tests/command/pull_test.rs`，复用该模块的 `create_remote_fixture`、`push_remote_commit`、`configure_pull_tracking`，`tests/command/pull_test.rs:87-147`）：`create_remote_fixture()` 以 `git` 建裸仓库 `remote.git` 与工作目录 `workdir` 并推送初始提交 `B`（`README.md`=`hello libra\n`），分支名 `<br>` 取其返回值；本地仓库 D = `init_repo_via_cli` + `configure_identity_via_cli`（身份 `Test User <test@example.com>`，pull 用例不依赖身份）+ `configure_pull_tracking(D, remote.git, <br>)` 后 `libra pull` 得到 `B`；`push_remote_commit(workdir, <br>, <path>, <content>, <msg>)` 推进上游，D 中本地提交带 `--no-gpg-sign`；F-PULLC：上游 `push_remote_commit` 把 `README.md` 改为 `up\n`，D 本地提交把 `README.md` 改为 `down\n`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：`LIBRA_FINE_EXIT_CODES` 未设置；退出码恰为 1（Git 2.54.0 预探：1）。
- AC2：重建 F-CONF，子进程环境 `LIBRA_FINE_EXIT_CODES=1`；退出码恰为 1（`with_exit_code` 优先）。
- AC3：AC1 的调用 stderr 含 `Error-Code: LBR-CONFLICT-002`。
- AC4：AC1 之后（AC1 的仓库，不是 AC2 重建的仓库）`libra ls-files -u` 的 stdout 恰为 `100644 <base> 1\tf\n100644 <ours> 2\tf\n100644 <theirs> 3\tf\n`，三者依次为 `libra hash-object` 对 `base\n`、`ours\n`、`theirs\n` 的 OID（Git 2.54.0 预探同形）。
- AC5：重建 F-CONF；退出码恰为 1（Git 2.54.0 预探：squash 冲突亦退出 1）。
- AC6：F-PULLC（pull 用例的首条映射，新建）；退出码恰为 1（Git 2.54.0 预探：1）。
- AC7：`rg -n -F -- 'Per-command exit-code overrides' docs/error-codes.md` 退出 0（锚点定位用）后人工核对该小节新增条目写明「`merge`/`pull` 内容或树冲突退出 1，稳定码 `LBR-CONFLICT-002`」；记录前后 SHA-256。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::conflict_exits_with_code_1)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::conflict_exits_with_code_1)'` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::pull_test::conflict_exits_with_code_1)'`（`(new)`，在 `tests/command/pull_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::pull_test::conflict_exits_with_code_1)'` 须恰好列出该 1 条）
- [ ] 卡特有判据（AC7，非 nextest 门）：`rg -n -F -- 'Per-command exit-code overrides' docs/error-codes.md` 退出 0（锚点定位）后按 AC7 映射核对并记录 SHA-256。
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::pull_test::/)'`（`command::pull_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改 docs/error-codes.md 退出码契约（ER-13 T-1）` | **Dependencies:** `MP-21`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/pull.rs`, `tests/command/merge_test.rs`, `tests/command/merge_test/`, `tests/command/pull_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/pull.rs`, `tests/command/merge_test.rs`, `tests/command/merge_test/`, `tests/command/pull_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`（由 `--only merge pull` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：EN 页 `exit code 1`、zh-CN 页 `退出码 1`。逐文件语义核对：`docs/development/commands/merge.md`、`docs/development/commands/pull.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行；`| pull |` 第 225 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md` 必须更新：在「Per-command exit-code overrides」小节登记本卡的退出码覆写（稳定码不变）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=冲突合并退出码 1（merge/pull）; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=1; prod-files=2; scope=M; deps=MP-21,DEP-MP-10; writeset=序列化于MP-21; release=independent; split-from=MP-02; exception=N/A`

---

### Task MP-34: `merge` 脏工作区与覆盖拒绝的退出码（FF `1`、三方 `2`）（#1028）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-22` 拆出：按 `ADR-MP-01` 与 Git 路径矩阵，`merge` 的 fast-forward 检出被本地已跟踪改动或未跟踪文件阻挡时以退出码 `1` 结束，三方合并被「would be overwritten」阻挡时以退出码 `2` 结束（均经 `with_exit_code`，稳定码不变；`with_exit_code` 优先于 `LIBRA_FINE_EXIT_CODES` 由 `ADR-MP-01` 的同一机制保证并已由 `MP-22` 验收），并按 ER-06 同卡文档门在 `docs/error-codes.md` 登记该矩阵。（按 G-09 自本卡拆出 `MP-40` 承接 `pull` 的同一矩阵。）

**Out of scope:** `pull` 的拒绝退出码（由 `MP-40` 承接）；冲突退出码（由 `MP-22` 承接）；是否拒绝的判定本身（由 `MP-11` 承接）；多策略未处理（由 `MP-14` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DirtyWorktree`、`UntrackedOverwrite` 与冲突共用 `CliError::failure`（128） | `src/command/merge.rs:1021-1028` |
| Git 2.54.0 预探：FF 被已跟踪改动/未跟踪文件阻挡退出 1；三方合并被阻挡退出 2 并输出 `Merge with strategy ort failed.` | `git@6de20f6092` `builtin/merge.c:1838-1849`（未处理策略 `ret = 2`） |
| 既有用例可能断言脏工作区拒绝的 `merge` 退出 128；本卡按「验证执行协议」第 5 条只把被测命令为本卡两类拒绝的退出码断言改为 1/2，判定清单写入本卡 `Current evidence`，受影响文件按 ER-03 补入写集 | `rg -n 'Some\(128\)' tests/command/merge_test.rs tests/command/merge_test/`（2026-11-18） |

**Acceptance criteria:**

- [ ] AC1：FF：工作树 `f` 有未提交修改时 `libra merge topic` 以退出码 1 结束。
- [ ] AC2：AC1 的拒绝稳定码保持 `LBR-CONFLICT-002`。
- [ ] AC3：FF：存在会被覆盖的未跟踪 `n` 时 `libra merge topic` 以退出码 1 结束。
- [ ] AC4：AC3 的拒绝稳定码保持 `LBR-CONFLICT-002`。
- [ ] AC5：三方：工作树 `f` 有未提交修改时 `libra merge topic -m m` 以退出码 2 结束。
- [ ] AC6：AC5 的拒绝稳定码保持 `LBR-CONFLICT-002`。
- [ ] AC7：三方：存在会被覆盖的未跟踪 `n` 时 `libra merge topic -m m` 以退出码 2 结束。
- [ ] AC8：AC7 的拒绝稳定码保持 `LBR-CONFLICT-002`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::refusal_exit_codes_ff_1_three_way_2` 承载 AC1–AC8；夹具 F-RX（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`、`a`=`a0\n`）；`topic` 自 `B` 提交 `T`（`f`=`t1\n` 并新增 `n`=`new\n`），`main` 仍指向 `B`（FF 场景）；三方场景 F-RX3：`main` 自 `B` 再提交 `M`（新增 `m`）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-RX，`echo dirty > f` 后调用；`LIBRA_FINE_EXIT_CODES` 未设置；退出码恰为 1（Git 2.54.0 预探：1，`Your local changes to the following files would be overwritten by merge:`）。
- AC2：AC1 调用的 stderr 含 `Error-Code: LBR-CONFLICT-002`。
- AC3：F-RX（重建），`echo u > n` 后调用；退出码恰为 1（Git 2.54.0 预探：1，`untracked working tree files would be overwritten`）。
- AC4：AC3 调用的 stderr 含 `Error-Code: LBR-CONFLICT-002`。
- AC5：F-RX3，`echo dirty > f`；退出码恰为 2（Git 2.54.0 预探：2，附 `Merge with strategy ort failed.`）。
- AC6：AC5 调用的 stderr 含 `Error-Code: LBR-CONFLICT-002`。
- AC7：F-RX3（重建），`echo u > n`；退出码恰为 2（Git 2.54.0 预探：2）。
- AC8：AC7 调用的 stderr 含 `Error-Code: LBR-CONFLICT-002`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::refusal_exit_codes_ff_1_three_way_2)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::refusal_exit_codes_ff_1_three_way_2)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改 docs/error-codes.md 退出码契约（ER-13 T-1）` | **Dependencies:** `MP-22`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `tests/command/merge_test/`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `tests/command/merge_test/`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：EN 页 `exit code 2`、zh-CN 页 `退出码 2`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md` 必须更新：在「Per-command exit-code overrides」小节登记本卡的退出码覆写（稳定码不变）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge 拒绝退出码（FF 1 / 三方 2）; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-22,DEP-MP-10; writeset=序列化于MP-22; release=independent; split-from=MP-22; exception=N/A`

---

### Task MP-40: `pull` 脏工作区拒绝的退出码（FF `1`、三方 `2`）（#1028）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-34` 拆出：`pull`（非 rebase）在快进检出被本地改动阻挡时以退出码 `1` 结束、在三方合并被「would be overwritten」阻挡时以退出码 `2` 结束（`map_merge_error_to_cli` 中经 `with_exit_code`，稳定码不变），并在 `docs/error-codes.md` 登记。

**Out of scope:** `merge` 的同一矩阵（由 `MP-34` 承接）；`pull --rebase` 的拒绝（`rebase` 引擎，`DEFER-MP-01`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `pull` 的独立映射把 `DirtyWorktree`/`UntrackedOverwrite` 映射为 `CliError::failure`（128） | `src/command/pull.rs:1176,1223-1229` |
| Git 2.54.0 预探：上游可快进且本地 `README.md` 有未提交修改时 `git pull` 退出 1；本地另有提交（三方）时 `git pull --no-rebase` 退出 2；两者 stderr 均含 `Your local changes to the following files would be overwritten by merge:` | `git@6de20f6092` `builtin/pull.c`（调用 `merge`） |
| 既有用例若断言脏工作区拒绝的 `pull` 退出 128（2026-11-18 未发现此类断言，`tests/command/pull_test.rs` 中的 `Some(128)` 均为其它路径）；本卡按「验证执行协议」第 5 条只改被测命令为本卡拒绝路径的退出码断言，判定清单写入本卡 `Current evidence` | `rg -n 'Some\(128\)' tests/command/pull_test.rs`（2026-11-18） |

**Acceptance criteria:**

- [ ] AC1：FF：`libra pull`（F-PULLD）以退出码 1 结束。
- [ ] AC2：三方：`libra pull`（F-PULLD3）以退出码 2 结束。
- [ ] AC3：AC1 的拒绝稳定码保持 `LBR-CONFLICT-002`。
- [ ] AC4：AC2 的拒绝稳定码保持 `LBR-CONFLICT-002`。
- [ ] AC5：`docs/error-codes.md` 登记 pull 的 FF `1` / 三方 `2` 矩阵。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::pull_test::refusal_exit_codes_ff_1_three_way_2` 承载 AC1–AC4；pull 夹具（新用例写在 `tests/command/pull_test.rs`，复用该模块的 `create_remote_fixture`、`push_remote_commit`、`configure_pull_tracking`，`tests/command/pull_test.rs:87-147`）：`create_remote_fixture()` 以 `git` 建裸仓库 `remote.git` 与工作目录 `workdir` 并推送初始提交 `B`（`README.md`=`hello libra\n`），分支名 `<br>` 取其返回值；本地仓库 D = `init_repo_via_cli` + `configure_identity_via_cli`（身份 `Test User <test@example.com>`，pull 用例不依赖身份）+ `configure_pull_tracking(D, remote.git, <br>)` 后 `libra pull` 得到 `B`；`push_remote_commit(workdir, <br>, <path>, <content>, <msg>)` 推进上游，D 中本地提交带 `--no-gpg-sign`；F-PULLD：上游 `push_remote_commit` 把 `README.md` 改为 `up\n`，D 中 `echo dirty > README.md`（FF 场景）；三方场景 F-PULLD3：在此之前 D 另有本地提交 `D1`（新增 `local.txt`）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：`LIBRA_FINE_EXIT_CODES` 未设置；退出码恰为 1（Git 2.54.0 预探：1）。
- AC2：F-PULLD3（新建，不沿用 AC1 的 F-PULLD 仓库）；退出码恰为 2（Git 2.54.0 预探：2，`Your local changes to the following files would be overwritten by merge:`；`builtin/merge.c` 未处理策略 `ret = 2`）。
- AC3：AC1 调用的 stderr 含 `Error-Code: LBR-CONFLICT-002`。
- AC4：AC2 调用的 stderr 含 `Error-Code: LBR-CONFLICT-002`。
- AC5：`rg -n -F -- 'Per-command exit-code overrides' docs/error-codes.md` 退出 0（锚点定位用）后人工核对该小节新增条目写明 `pull` 的 FF 被阻挡退出 1、三方被阻挡退出 2；记录前后 SHA-256。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC4）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::pull_test::refusal_exit_codes_ff_1_three_way_2)'`（`(new)`，在 `tests/command/pull_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::pull_test::refusal_exit_codes_ff_1_three_way_2)'` 须恰好列出该 1 条）
- [ ] 卡特有判据（AC5，非 nextest 门）：`rg -n -F -- 'Per-command exit-code overrides' docs/error-codes.md` 退出 0（锚点定位）后按 AC5 映射核对并记录 SHA-256。
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::pull_test::/)'`（`command::pull_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改 docs/error-codes.md 退出码契约（ER-13 T-1）` | **Dependencies:** `MP-34`, `DEP-MP-10`

**Implementation write set:** `src/command/pull.rs`, `tests/command/pull_test.rs`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/pull.rs`, `tests/command/pull_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`（由 `--only pull` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：EN 页 `exit code 2`、zh-CN 页 `退出码 2`。逐文件语义核对：`docs/development/commands/pull.md` 与 `COMPATIBILITY.md`（`| pull |` 第 225 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md` 必须更新：在「Per-command exit-code overrides」小节登记本卡的退出码覆写（稳定码不变）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=pull 拒绝退出码（FF 1 / 三方 2）; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=MP-34,DEP-MP-10; writeset=序列化于MP-34; release=independent; split-from=MP-34; exception=N/A`

---

### Task MP-03: 修复 `merge --edit` 编辑器失败后的可恢复性（#994）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** `merge --edit` 的编辑器以非 0 退出时，与 Git 一致输出 `Not committing merge; use 'libra commit' to complete the merge.`、HEAD 不动并保留可恢复的待决合并（`merge-state.json` 与已合并的索引/工作树），使 `libra commit`、`libra merge --continue` 或 `libra merge --abort` 均可继续。（按 G-09 自本卡拆出 `MP-52` 承接编辑器无法运行的情形。）

**Out of scope:** 编辑器无法运行（由 `MP-52` 承接）；编辑器发现优先级（`GIT_EDITOR`/`core.editor`/`VISUAL`/`EDITOR`）本身；编辑成功路径。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `edit_merge_message` 失败映射为 `PullMergeError::Editor`，在写入待决状态前返回 | `src/command/merge.rs:1010-1013,2911` |
| Git 2.54.0 预探：`GIT_EDITOR=false git merge --edit topic` 退出 1，stderr 为 `error: there was a problem with the editor 'false'` 与 `Not committing merge; use 'git commit' to complete the merge.`，保留 `MERGE_HEAD`/`MERGE_MSG` 与已合并索引 | `git@6de20f6092` `builtin/merge.c`（`abort_commit`） |

**Acceptance criteria:**

- [ ] AC1：`GIT_EDITOR=false libra merge --edit topic`（F-DIV）的 stderr 含可恢复提示。
- [ ] AC2：AC1 之后 HEAD 不动。
- [ ] AC3：AC1 之后索引保留已合并结果。
- [ ] AC4：AC1 之后 `libra commit --no-edit` 创建合并提交。
- [ ] AC5：AC4 的提交树为完整的合并结果。
- [ ] AC6：AC1 之后 `libra merge --continue` 创建合并提交。
- [ ] AC7：AC6 的提交树为完整的合并结果。
- [ ] AC8：AC1 之后 `libra merge --abort` 恢复合并前状态。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::edit_failure_keeps_resumable_merge` 承载 AC1–AC8；夹具 F-DIV（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`）；`main` 自 `B` 再提交 `M`（新增 `h`=`m1\n`），与 `topic` 分叉且无冲突；编辑器失败以子进程环境 `GIT_EDITOR=false` 注入（用例 `#[cfg(unix)]`）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出；stderr 含 `Not committing merge; use 'libra commit' to complete the merge.`（Git 2.54.0 预探文案中的 `git commit` 按 Libra 命令名替换）。
- AC2：`libra rev-parse HEAD` 等于 `M`。
- AC3：`libra ls-files -s` 的 stdout 恰为 `100644 <base> 0\tf\n100644 <t1> 0\tg\n100644 <m1> 0\th\n`，三者依次为 `libra hash-object` 对 `base\n`、`t1\n`、`m1\n` 的 OID（Git 2.54.0 预探：暂存 `g`）。
- AC4：在 AC1 的状态上执行该命令退出 0；HEAD 的父提交恰为 `[M, T]`（Git 2.54.0 预探：`Merge branch 'topic'`）。
- AC5：AC4 之后 `libra ls-tree -r HEAD` 的 stdout 恰为 `100644 blob <base>\tf\n100644 blob <t1>\tg\n100644 blob <m1>\th\n`（三者依次为 `libra hash-object` 对 `base\n`、`t1\n`、`m1\n` 的 OID；即 `M` 与 `T` 的合并树，缺任一路径或内容不符即失败）。
- AC6：在另一份重建的 AC1 状态上执行（子进程环境 `GIT_EDITOR=true`）退出 0；HEAD 的父提交恰为 `[M, T]`。
- AC7：AC6 之后 `libra ls-tree -r HEAD` 的 stdout 恰为 `100644 blob <base>\tf\n100644 blob <t1>\tg\n100644 blob <m1>\th\n`（三者依次为 `libra hash-object` 对 `base\n`、`t1\n`、`m1\n` 的 OID；即 `M` 与 `T` 的合并树，缺任一路径或内容不符即失败）。
- AC8：在另一份重建的 AC1 状态上，`libra merge --abort` 退出 0；之后的「仓库快照 Σ」与 AC1 调用前的 Σ 逐项相等（Git 2.54.0 预探：HEAD 不变、`g` 不存在、status 干净）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::edit_failure_keeps_resumable_merge)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::edit_failure_keeps_resumable_merge)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_noninteractive_history_controls`（整 target，非交互历史控制；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_noninteractive_history_controls` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-40`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Not committing merge`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge --edit 编辑器失败可恢复; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-40,DEP-MP-10; writeset=序列化于MP-40; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-52: `merge --edit` 编辑器无法运行时同样保留可恢复合并（#994）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-03` 拆出：Libra 经 `sh -c` 启动编辑器（`src/command/editor.rs:120-149`），因此「编辑器无法运行」有两种形态：`GIT_EDITOR` 指向不存在的程序时 shell 以 127 退出；`sh` 本身无法创建（子进程 `PATH` 中没有 `sh`）时 `Command::status()` 返回错误。两种形态都与编辑器以非 0 退出走同一可恢复路径：输出 `Not committing merge; use 'libra commit' to complete the merge.`、HEAD 不动并保留待决合并与已合并的索引，使 `libra commit` 可完成合并（`#994`：编辑器 "cannot be run" 与非 0 退出同属该 issue）。

**Out of scope:** 编辑器以非 0 退出（由 `MP-03` 承接）；编辑器发现优先级本身；`abort_on_failure` 为假的其它调用方（不在本计划范围）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `edit_message` 以 `Command::new("sh").arg("-c")` 启动编辑器，`status()` 的错误与非 0 退出在 `abort_on_failure` 时同归 `EditorError::Aborted`；合并的调用点在写入待决状态前返回 | `src/command/editor.rs:120-149`；`src/command/merge.rs:2090,2911,3198,4332` |
| Git 2.54.0 预探（F-DIV，`GIT_EDITOR=/nonexistent/editor git merge --edit topic`）：stderr 为 `fatal: cannot exec '/nonexistent/editor': No such file or directory`、`error: unable to start editor '/nonexistent/editor'` 与 `Not committing merge; use 'git commit' to complete the merge.`，退出 1，保留 `MERGE_HEAD`/`MERGE_MSG`，HEAD 不动，索引为已合并结果 | `git@6de20f6092` `builtin/merge.c`（`abort_commit`）；`#994` |

**Acceptance criteria:**

- [ ] AC1：形态 A：`libra merge --edit topic`（F-DIV）的 stderr 含可恢复提示。
- [ ] AC2：AC1 之后 HEAD 不动。
- [ ] AC3：AC1 之后索引保留已合并结果。
- [ ] AC4：AC1 之后 `libra commit --no-edit` 完成为 `M` 与 `T` 的合并提交（含完整合并结果）。
- [ ] AC5：形态 B：`sh` 无法创建时 `libra merge --edit topic` 的 stderr 含同一可恢复提示。
- [ ] AC6：AC5 之后 HEAD 不动。
- [ ] AC7：AC5 之后索引保留已合并结果。
- [ ] AC8：AC5 之后 `libra commit --no-edit` 完成为 `M` 与 `T` 的合并提交（含完整合并结果）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::edit_spawn_failure_keeps_resumable_merge` 承载 AC1–AC8；夹具 F-DIV（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`）；`main` 自 `B` 再提交 `M`（新增 `h`=`m1\n`），与 `topic` 分叉且无冲突；形态 A：子进程环境 `GIT_EDITOR=/nonexistent/editor`（绝对路径，不存在）；形态 B：夹具建立完成后，被测 `merge` 子进程的环境 `PATH` 设为一个空的临时目录 `<nosh>`（其中没有 `sh`；`libra` 以绝对路径调用），`GIT_EDITOR=true`；用例 `#[cfg(unix)]`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出；stderr 含 `Not committing merge; use 'libra commit' to complete the merge.`（Git 2.54.0 预探文案中的 `git commit` 按 Libra 命令名替换）。
- AC2：`libra rev-parse HEAD` 等于 `M`（Git 2.54.0 预探）。
- AC3：`libra ls-files -s` 的 stdout 恰为 `100644 <base> 0\tf\n100644 <t1> 0\tg\n100644 <m1> 0\th\n`，三者依次为 `libra hash-object` 对 `base\n`、`t1\n`、`m1\n` 的 OID（Git 2.54.0 预探）。
- AC4：在 AC1 的状态上以默认子进程环境执行该命令，退出 0（执行条件）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree ce02ae3e3a7e99e2b8c9a286eafcfbb390725072`、`parent <M>`、`parent <T>`（该树即 `f`=`base\n`、`g`=`t1\n`、`h`=`m1\n`；Git 2.54.0 预探：`git commit --no-edit` 完成合并）。
- AC5：重建 F-DIV 后按形态 B 执行；以非 0 退出；stderr 含 `Not committing merge; use 'libra commit' to complete the merge.`。
- AC6：`libra rev-parse HEAD` 等于 `M`。
- AC7：`libra ls-files -s` 的 stdout 与 AC3 的期望逐字相等。
- AC8：在 AC5 的状态上以默认子进程环境（恢复默认 `PATH`）执行该命令，退出 0（执行条件）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree ce02ae3e3a7e99e2b8c9a286eafcfbb390725072`、`parent <M>`、`parent <T>`（同 AC4）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::edit_spawn_failure_keeps_resumable_merge)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::edit_spawn_failure_keeps_resumable_merge)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-03`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/editor.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/editor.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Not committing merge`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge --edit 编辑器无法运行可恢复; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=MP-03,DEP-MP-10; writeset=序列化于MP-03; release=independent; split-from=MP-03; exception=N/A`

---

### Task MP-04: 实现 `merge --into-name` 与 `merge.suppressDest`（#999）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 使 `--into-name <name>` 在双头（单目标）合并的全部消息生成路径替换目标分支名，并实现 `merge.suppressDest`（多值 glob；空值清空先前列表；未配置时默认 `main`、`master`）：目标名匹配任一模式时省略 ` into <name>`。显式 `-m`/`-F` 不受影响。（按 G-09 自本卡拆出 `MP-49` 承接 octopus 合并消息。）

**Out of scope:** octopus 合并消息（`resolve_octopus_message`，由 `MP-49` 承接）；`-m`/`-F` 消息本身；`fmt-merge-msg` 命令（不在本计划范围）；分离 HEAD 的引用类型文案（`#999` 关联的 ref-kind 修复，`issues/974.md` 承接，`DEP-MP-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `MergeArgs::into_name` 存在但部分合并路径传 `None`，且无 `merge.suppressDest` 读取 | `src/command/merge.rs:370-371,2076,2152,2950,3192` |
| 当前默认标题为 `Merge {upstream} into {head_name}`，没有 `branch '<x>'` 装饰；装饰由 `I974-01` 交付（`DEP-MP-04` 硬前置），本卡各 AC 的期望主题以其合入为前提。`I974-01` 的 ADR 规定「目的地=当前分支（默认）⇒ 无后缀」，而 Git 只在目标名匹配 `merge.suppressDest`（未配置时 `main`、`master`）时省略，当前分支为 `dev` 时仍输出 ` into dev`；本卡以模式匹配取代该规则，并按「验证执行协议」第 5 条改写 `I974-01` 用例中依赖「当前分支即省略」的断言（判定清单写入本卡 `Current evidence`，受影响的 `tests/command/merge_test/message_options.rs` 等按 ER-03 补入写集） | `src/command/merge_message.rs:9-17`；`docs/development/plan/issues/974.md:130`；`git@6de20f6092` `fmt-merge-msg.c:661-663` |

**Acceptance criteria:**

- [ ] AC1：`libra merge --no-ff --into-name=other topic`（F-DIV）的提交主题为 `Merge branch 'topic' into other`。
- [ ] AC2：`merge.suppressDest=other` 时同一调用的提交主题省略目标。
- [ ] AC3：`merge.suppressDest` 支持 glob。
- [ ] AC4：`merge.suppressDest` 多值中任一匹配即省略（不只看最后一个值）。
- [ ] AC5：`merge.suppressDest` 的空值清空它之前的值。
- [ ] AC6：空值之后追加的值仍生效。
- [ ] AC7：显式 `-m` 不受 `--into-name` 影响。
- [ ] AC8：分离 HEAD 上未配置 `merge.suppressDest` 时，`--into-name` 为默认列表中的 `main` 或 `master` 都省略目标名。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::into_name_and_suppress_dest` 承载 AC1–AC8；夹具 F-DIV（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`）；`main` 自 `B` 再提交 `M`（新增 `h`=`m1\n`），与 `topic` 分叉且无冲突；每个 AC 使用一份重建的 F-DIV（配置互不累积）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：退出 0；`libra log -1 --format=%s` 的 stdout 恰为 `Merge branch 'topic' into other\n`（`#999` AC2）。
- AC2：`libra config merge.suppressDest other`；主题恰为 `Merge branch 'topic'\n`。
- AC3：`libra config merge.suppressDest 'ot*'`；`--into-name=other` 时主题恰为 `Merge branch 'topic'\n`。
- AC4：依次 `libra config --add merge.suppressDest other` 与 `libra config --add merge.suppressDest x`（第一项匹配、最后一项不匹配）；`--into-name=other` 时主题恰为 `Merge branch 'topic'\n`。
- AC5：依次 `--add merge.suppressDest other`、`--add merge.suppressDest ''`、`--add merge.suppressDest y`；`--into-name=other` 时主题恰为 `Merge branch 'topic' into other\n`（`git@6de20f6092` `fmt-merge-msg.c:40-47`：空值 `string_list_clear`；未清空的实现会因 `other` 匹配而省略）。
- AC6：依次 `--add merge.suppressDest x`、`--add merge.suppressDest ''`、`--add merge.suppressDest other`、`--add merge.suppressDest y`；`--into-name=other` 时主题恰为 `Merge branch 'topic'\n`（只看最后一个值的实现会因 `y` 不匹配而保留 ` into other`）。
- AC7：`libra merge --no-ff --into-name=other -m custom topic`；主题恰为 `custom\n`。
- AC8：在两份重建的 F-DIV 上各先 `libra switch --detach M`，不设该配置，分别执行 `libra merge --no-ff --into-name=main topic` 与 `libra merge --no-ff --into-name=master topic`；两次的主题都恰为 `Merge branch 'topic'\n`（`#999` AC1 的 `--into-name=main` 部分；Git 2.54.0 预探同形；`git@6de20f6092` `fmt-merge-msg.c:661-663` 在未见该配置时默认追加 `main`、`master`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::into_name_and_suppress_dest)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::into_name_and_suppress_dest)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_config_history_defaults`（整 target，`merge.*` 历史默认配置；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_config_history_defaults` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-52`, `DEP-MP-04`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`merge.suppressDest`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--into-name 与 merge.suppressDest; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-52,DEP-MP-04,DEP-MP-10; writeset=序列化于MP-52; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-49: octopus 合并默认消息对齐 Git（列举格式、`--into-name` 与 `merge.suppressDest`）（#999）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-04` 拆出：octopus 合并的默认消息按 Git `fmt-merge-msg` 生成：两个目标为 `Merge branches '<b1>' and '<b2>'`，三个及以上为 `Merge branches '<b1>', '<b2>' and '<b3>'` 形式；目标名（`--into-name` 或当前分支）匹配 `MP-04` 实现的 `merge.suppressDest` 列表（未配置时默认 `main`、`master`）时省略 ` into <name>`，否则追加。显式 `-m`/`-F` 不受影响。

**Out of scope:** 双头合并消息（由 `MP-04` 承接）；`merge.log` 的 shortlog 正文（既有行为）；`fmt-merge-msg` 命令（不在本计划范围）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_octopus_message` 以 `, ` 连接全部目标并总是追加 ` into <name>`，不读取 `merge.suppressDest`（Libra 0.30.207 预探，F-OCTM：`libra merge b1 b2` 的主题为 `Merge branches 'b1', 'b2' into main`） | `src/command/merge.rs:2062-2077` |
| Git 2.54.0 预探（F-OCTM）：在 `main` 上 `git merge b1 b2` 的主题为 `Merge branches 'b1' and 'b2'`，`git merge b1 b2 b3` 为 `Merge branches 'b1', 'b2' and 'b3'`，`--into-name=other` 为 `Merge branches 'b1' and 'b2' into other`，`merge.suppressDest=other` 时省略 ` into other`，在 `dev` 上为 `Merge branches 'b1' and 'b2' into dev` | `git@6de20f6092` `fmt-merge-msg.c` |

**Acceptance criteria:**

- [ ] AC1：在 `main` 上 `libra merge b1 b2` 的提交主题为 Git 的两目标格式且省略默认目标名。
- [ ] AC2：三个目标以逗号与 `and` 列举。
- [ ] AC3：`--into-name` 替换 octopus 消息的目标名。
- [ ] AC4：`merge.suppressDest` 匹配时 octopus 消息省略目标名。
- [ ] AC5：目标名不匹配时追加 ` into <当前分支>`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::octopus_message_matches_fmt_merge_msg` 承载 AC1–AC5；夹具 F-OCTM（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`a`=`a0\n`）；`b1`、`b2`、`b3` 各自 `B` 新增 `b1.txt`=`b1\n`、`b2.txt`=`b2\n`、`b3.txt`=`b3\n` 并提交；`main` 自 `B` 再提交 `M`（新增 `m`=`m\n`，与三者无冲突）；每个 AC 使用一份重建的夹具并在 `main` 上执行（AC5 先 `libra switch -c dev`）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：退出 0（执行条件）；`libra log -1 --format=%s` 的 stdout 恰为 `Merge branches 'b1' and 'b2'\n`（Git 2.54.0 预探）。
- AC2：`libra merge b1 b2 b3` 退出 0（执行条件）；主题恰为 `Merge branches 'b1', 'b2' and 'b3'\n`（Git 2.54.0 预探）。
- AC3：`libra merge --into-name=other b1 b2` 退出 0（执行条件）；主题恰为 `Merge branches 'b1' and 'b2' into other\n`（Git 2.54.0 预探）。
- AC4：`libra config merge.suppressDest other` 后 `libra merge --into-name=other b1 b2` 退出 0（执行条件）；主题恰为 `Merge branches 'b1' and 'b2'\n`（Git 2.54.0 预探）。
- AC5：`libra switch -c dev` 后 `libra merge b1 b2` 退出 0（执行条件）；主题恰为 `Merge branches 'b1' and 'b2' into dev\n`（Git 2.54.0 预探）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::octopus_message_matches_fmt_merge_msg)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::octopus_message_matches_fmt_merge_msg)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-04`, `DEP-MP-04`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge_message.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge_message.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Merge branches`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=octopus 默认合并消息; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=MP-04,DEP-MP-04,DEP-MP-10; writeset=序列化于MP-04; release=independent; split-from=MP-04; exception=N/A`

---

### Task MP-05: 对齐 `merge --squash` 的 `SQUASH_MSG`（成功正文、冲突与 `core.commentChar`）（#980）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 对齐 Git 的 squash 提交消息：干净 squash 时 `.libra/SQUASH_MSG` 为 `Squashed commit of the following:` 加被并入提交按新到旧排列的 `show -s`（medium）块；squash 冲突时同样写出 `SQUASH_MSG`，并在其末尾追加以 `core.commentChar` 注释的冲突路径段（Git 把该段写入 `MERGE_MSG` 再由 `commit` 拼接；Libra 的 `MERGE_MSG` 只是 `--edit` 的编辑缓冲（`src/command/merge.rs:2918`），`libra commit` 的消息种子取自 `merge-state.json` 或在其缺失时取 `SQUASH_MSG`（`src/command/commit.rs:1311-1318,1872-1874`），而 squash 不写 `merge-state.json`，因此把冲突段直接写进 `SQUASH_MSG` 即得到与 Git 相同的提交消息，本卡不改 `commit.rs`）；提交成功后删除 `SQUASH_MSG`（既有行为）。

**Out of scope:** 非 squash 合并消息（由 `MP-04`、`issues/974.md` 承接）；`libra commit` 未配置 `commit.cleanup` 时默认 `strip`（`src/command/commit.rs:983`），而 Git 在不打开编辑器（`--no-edit`、`-F`）时默认 `whitespace`、保留注释行：`#980` AC1 的不带 `--cleanup` 的 `commit --no-edit -a` 因此还依赖该默认值修复，它由 `plan-20261117-commit-signing.md` 的 `CS-14`（`#794`，「`default` 在使用编辑器时等同 `strip`，否则等同 `whitespace`」）承接，登记为 `DEP-MP-13`（只约束 `#980` 的关闭，不阻塞本卡发布）；本卡用例显式传 `--cleanup=whitespace`；`commit.cleanup=scissors` 与冲突消息（`plan-20261117-commit-signing.md` 的 `CS-14a`，`DEP-MP-09`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `record_squash_message` 只在 squash 成功收尾处调用（调用点 `src/command/merge.rs:1914,3251,3704,9039`），冲突返回 `PullMergeError::Conflicts { squash: true }` 前不写 `SQUASH_MSG`；正文为 `Squashed commit of the following:` 加 Libra 自有摘要 | `src/command/merge/output.rs:233-240`；`src/command/merge.rs:1914,3251,3704,9039` |
| squash 冲突只留冲突阶段、不写 `merge-state.json`（源码注释「A squash leaves conflict stages but deliberately no merge sidecar」），因此 `libra commit` 没有合并消息种子，改用 `SQUASH_MSG` 作为种子；开工时按 ER-02 逐一核对各 squash 冲突返回路径 | `src/command/merge.rs:2316`；`src/command/commit.rs:1311-1318,1872-1874` |
| Git 2.54.0 预探：冲突 squash 写 `SQUASH_MSG`（正文）与 `MERGE_MSG`（`\n# Conflicts:\n#\tf\n`），`git commit --no-edit` 的消息为二者拼接；`core.commentChar=;` 时注释符为 `;` | `git@6de20f6092` `builtin/merge.c` `suggest_conflicts` |

**Acceptance criteria:**

- [ ] AC1：干净场景 `libra merge --squash topic` 后 `.libra/SQUASH_MSG` 与 Git 正文逐字节相等。
- [ ] AC2：冲突场景 `libra merge --squash topic` 后 `.libra/SQUASH_MSG` 为正文加冲突段。
- [ ] AC3：冲突场景解决后提交的消息为正文加冲突段。
- [ ] AC4：`core.commentChar=;` 时冲突段使用 `;`。
- [ ] AC5：提交成功后 `SQUASH_MSG` 被删除。
- [ ] AC6：冲突场景以 `commit -a` 完成时消息同样为正文加冲突段（`#980` Expected 3 的 `-a` 形态）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::squash_msg_body_and_conflicts` 承载 AC1–AC6；夹具 F-SQ（`init_repo_via_cli` 后 `libra config user.name 'A U Thor'`、`libra config user.email author@example.com` 与 `libra config vault.signing false`；全部提交以子进程环境 `GIT_AUTHOR_NAME='A U Thor'`、`GIT_AUTHOR_EMAIL=author@example.com`、`GIT_AUTHOR_DATE='1700000000 +0800'`、`GIT_COMMITTER_NAME='C O Mitter'`、`GIT_COMMITTER_EMAIL=committer@example.com`、`GIT_COMMITTER_DATE='1700007200 -0500'` 执行并带 `--no-gpg-sign`——作者与提交者的身份、日期与时区都不同（显式设置作者环境变量，因为 Libra 的作者身份在未设 `GIT_AUTHOR_*` 时会回落到 `GIT_COMMITTER_*`，`src/command/commit.rs:692-721`；日期见 `:908-921`），使误用提交者信息的实现在 `Author:`/`Date:` 行失败；Git 2.54.0 预探：同条件下 `SQUASH_MSG` 只用作者身份与作者日期）：`main` 提交 `B`（`f`=`base\n`）；`topic` 自 `B` 提交 `T1`（新增 `g`=`t1\n`，消息 `topic one`）与 `T2`（`f`=`t2\n`，消息 `topic two`）；干净场景 `main` 自 `B` 提交 `M`（新增 `h`）；冲突场景 `main` 自 `B` 提交 `MC`（`f`=`mconf\n`）。`<T1>`/`<T2>` 为 `libra rev-parse` 得到的完整 OID；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：在 `M` 上执行，退出 0；文件字节等于 `Squashed commit of the following:\n\ncommit <T2>\nAuthor: A U Thor <author@example.com>\nDate:   Wed Nov 15 06:13:20 2023 +0800\n\n    topic two\n\ncommit <T1>\nAuthor: A U Thor <author@example.com>\nDate:   Wed Nov 15 06:13:20 2023 +0800\n\n    topic one\n`（Git 2.54.0 预探）。
- AC2：冲突场景（新建，`main` 指向 `MC`）；在 `MC` 上执行，以非 0 退出（冲突）；文件字节等于「`Squashed commit of the following:\n\ncommit <T2>\nAuthor: A U Thor <author@example.com>\nDate:   Wed Nov 15 06:13:20 2023 +0800\n\n    topic two\n\ncommit <T1>\nAuthor: A U Thor <author@example.com>\nDate:   Wed Nov 15 06:13:20 2023 +0800\n\n    topic one\n`」后接 `\n# Conflicts:\n#\tf\n`（Git 2.54.0 预探：正文同 `SQUASH_MSG`，冲突段为 `MERGE_MSG` 内容）。
- AC3：AC2 之后写 `f`=`fixed\n`、`libra add f`、`libra commit --no-edit --cleanup=whitespace`（退出 0）；`libra log -1 --format=%B` 的 stdout 等于 AC2 期望字节再接 `%B` 的结尾换行（Git 2.54.0 预探：`git commit --no-edit` 的消息为正文与冲突段拼接）。
- AC4：重建冲突场景并在合并前 `libra config core.commentChar ';'`；合并后 `.libra/SQUASH_MSG` 以 `\n; Conflicts:\n;\tf\n` 结尾（Git 2.54.0 预探：提交消息以该段结尾）。
- AC5：AC3 之后 `.libra/SQUASH_MSG` 不存在（Git 2.54.0 预探）。
- AC6：重建冲突场景，`libra merge --squash topic` 以非 0 退出（执行条件）后写 `f`=`fixed\n`（不执行 `libra add`），`libra commit -a --no-edit --cleanup=whitespace` 退出 0（执行条件）；`libra log -1 --format=%B` 的 stdout 等于 AC2 期望字节再接 `%B` 的结尾换行（Git 2.54.0 预探：`git commit -a --no-edit` 同样得到该拼接消息）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::squash_msg_body_and_conflicts)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::squash_msg_body_and_conflicts)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-49`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/output.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/output.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`SQUASH_MSG`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge --squash SQUASH_MSG 生成; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-49,DEP-MP-10; writeset=序列化于MP-49; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-06: unborn 分支合并的 reflog 记为 `initial pull`（#1003）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：unborn 合并/fast-forward 的 HEAD 与分支 reflog 记为 `initial pull`；新增 pull reflog 测试；pull 52/52。C 组 patch +1。偏差：`merge` 侧的独立 unborn 测试未单列，由共享路径覆盖；用户已取消独立 Claude/Codex 审计与全量测试。）

**Description:** 向未诞生分支执行 `merge`（及 `pull` 的合并路径）时，HEAD 与分支 reflog 的消息与 Git 一致写为 `initial pull`（替代 `merge: merge <ref>:fast-forward`）。（按 G-09 自本卡拆出 `MP-29`。）

**Out of scope:** `reflog -1` 结尾空行（由 `MP-29` 承接）；非 unborn 合并的 reflog 文案（保持现状，AC5 守护）；`libra reflog <branch>` 把分支 reflog 的选择子也显示为 `HEAD@{n}`（`src/command/reflog.rs:1094`，Libra 0.30.207 实测 `libra reflog -1 main` 输出 `<oid7> HEAD@{0}: ...`；Git 为 `main@{0}`；不在 `#1003` 范围，本卡的分支 reflog 断言不依赖选择子文字）；不带参数的 `libra merge`（合并上游）在 Libra 中不受支持（0.30.207 实测以 `merge requires a branch argument` + `LBR-CLI-002` 拒绝），`#1003` AC2 的该变体按 `DEFER-MP-06` 延后。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| unborn 合并经 `update_head_with_reflog` 以 `merge:` 动作记录；oneline 显示为 `<oid> <selector>: <action>: <message>` | `src/command/merge.rs:5104-5110`；`src/command/reflog.rs:1104,1117-1120` |
| Git 2.54.0 预探：未诞生分支上 `git merge <ref>` 与 `git pull origin main` 后 `git reflog -1` 均为 `<oid7> HEAD@{0}: initial pull`，分支 reflog 同为 `initial pull` | `git@6de20f6092` `builtin/merge.c:1560` |

**Acceptance criteria:**

- [ ] AC1：merge 场景后 HEAD reflog 最新条目为 `initial pull`。
- [ ] AC2：merge 场景后分支 reflog 最新条目为 `initial pull`。
- [ ] AC3：pull 场景后 HEAD reflog 最新条目为 `initial pull`。
- [ ] AC4：`--ff-only` 形态的 unborn 合并同样记为 `initial pull`（`#1003` AC2）。
- [ ] AC5：非 unborn 分支上的合并保持现行 reflog 文案（`#1003` AC3）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::pull_test::unborn_merge_reflog_initial_pull` 承载 AC1–AC2、AC4–AC5；`(new)` `command::pull_test::unborn_pull_reflog_initial_pull` 承载 AC3；pull 夹具（新用例写在 `tests/command/pull_test.rs`，复用该模块的 `create_remote_fixture`、`push_remote_commit`、`configure_pull_tracking`，`tests/command/pull_test.rs:87-147`）：`create_remote_fixture()` 以 `git` 建裸仓库 `remote.git` 与工作目录 `workdir` 并推送初始提交 `B`（`README.md`=`hello libra\n`），分支名 `<br>` 取其返回值；本地仓库 D = `init_repo_via_cli` + `configure_identity_via_cli`（身份 `Test User <test@example.com>`，pull 用例不依赖身份）+ `configure_pull_tracking(D, remote.git, <br>)` （不执行初次 `libra pull`，`main` 保持未诞生）；本卡不调用 `push_remote_commit`，上游 `<br>` 始终指向初始提交 `B`；merge 场景：D 中 `libra fetch origin` 后 `libra merge origin/<br>`；pull 场景：以同一方法另建本地仓库 D2（指向同一 `remote.git`，上游仍为 `B`），在 D2 中 `libra pull`；`<B7>` 为 `B` 的前 7 位十六进制；`<b>` 为 D 的当前分支（`libra init` 默认 `main`，`src/command/init.rs:37`）；全部调用设子进程环境 `LIBRA_PAGER=never`；AC5 另用 夹具 F-FF（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；自 `B` 建分支 `topic` 并提交 `T`（新增 `g`=`t1\n`），`main` 仍指向 `B`（可 fast-forward）（在 `pull_test.rs` 内构造）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：`libra reflog -1` 的 stdout 第一行恰为 `<B7> HEAD@{0}: initial pull`（Git 2.54.0 预探）。
- AC2：`libra reflog -1 <b>` 的 stdout 第一行按首个 `: ` 切分后，前半以 `<B7> ` 开头、后半恰为 `initial pull`（Git 2.54.0 预探整行为 `<B7> <b>@{0}: initial pull`；前半的选择子文字见 Out of scope，不作判据；`libra reflog -1 <b>` 读取分支 reflog 行——Libra 0.30.207 实测 HEAD 与分支 reflog 的最新条目不同时显示分支的条目——因此只改写 HEAD reflog 的实现在此失败）。
- AC3：D2 中 `libra reflog -1` 的 stdout 第一行恰为 `<B7> HEAD@{0}: initial pull`（Git 2.54.0 预探）。
- AC4：以同一方法另建本地仓库 D3（`main` 未诞生，上游仍为 `B`），`libra fetch origin` 后 `libra merge --ff-only origin/<br>` 退出 0（执行条件）；`libra reflog -1` 的 stdout 第一行恰为 `<B7> HEAD@{0}: initial pull`（Git 2.54.0 预探：`git merge --ff-only <ref>` 在未诞生分支上同样记为 `initial pull`）。
- AC5：F-FF；`libra merge topic` 退出 0（执行条件）；`libra reflog -1` 的 stdout 第一行恰为 `<T7> HEAD@{0}: merge: merge topic:fast-forward`（`<T7>` 为 `T` 前 7 位；Libra 0.30.207 现行文案，本卡不改；把所有快进合并都记为 `initial pull` 的实现即失败）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC2、AC4–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::pull_test::unborn_merge_reflog_initial_pull)'`（`(new)`，在 `tests/command/pull_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::pull_test::unborn_merge_reflog_initial_pull)'` 须恰好列出该 1 条）
- [x] 卡特有用例：`unborn_pull_reflog_initial_pull`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::pull_test::/)'`（`command::pull_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-05`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/pull_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/pull_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`（由 `--only merge pull` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`initial pull`。逐文件语义核对：`docs/development/commands/merge.md`、`docs/development/commands/pull.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行；`| pull |` 第 225 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=unborn 分支 initial pull reflog; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=MP-05,DEP-MP-10; writeset=序列化于MP-05; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-29: 修复 `libra reflog -<n>` 输出末尾多余空行（#1003）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：formatter 不再自带尾换行，由 pager 每行统一补一次；新增 `reflog_limit_no_trailing_blank_line`。C 组 patch +1。偏差：`command::reflog_test` 其余 3 个旧 SQLite fixture 用例因默认 redb 失败，超出本卡；用户已取消独立 Claude/Codex 审计与全量测试。）

**Description:** 自 `MP-06` 拆出：`libra reflog -1`、`libra reflog show -n <k>` 的 oneline 输出在最后一条后多出一个空行（已安装 0.30.207 实测），改为与 Git 一致每条一行、无空行。

**Out of scope:** unborn 合并的 reflog 消息（由 `MP-06` 承接）；`--json`/`--machine` 输出结构。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| oneline 条目以 `\n` 连接后再经 `writeln!` 输出；已安装 0.30.207 实测 `libra reflog -1` 的 stdout 为 `<oid7> HEAD@{0}: commit: two\n\n` | `src/command/reflog.rs:1117-1120,1158-1162` |

**Acceptance criteria:**

- [x] AC1：`libra reflog -1` 只输出一行（`reflog_test::reflog_limit_no_trailing_blank_line`）。
- [x] AC2：`libra reflog show -n 2` 不含空行（`reflog_test::reflog_limit_no_trailing_blank_line`）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::reflog_test::reflog_limit_no_trailing_blank_line` 承载 AC1–AC2；夹具 F-RL（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：写 `a`=`1\n`、`libra add a`、`libra commit --no-gpg-sign -m one` 得 `C1`；再写 `a`=`2\n`、`libra add a`、`libra commit --no-gpg-sign -m two` 得 `C2`；`<C2_7>` 为 `C2` 前 7 位；全部调用设子进程环境 `LIBRA_PAGER=never`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：stdout 恰为 `<C2_7> HEAD@{0}: commit: two\n`（Git 2.54.0 预探同格式）。
- AC2：stdout 恰由 2 个非空行组成（每行以 `\n` 结尾，其间与末尾都没有空行）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`reflog_limit_no_trailing_blank_line`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::reflog_test::/)'`（`command::reflog_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-06`, `DEP-MP-10`

**Implementation write set:** `src/command/reflog.rs`, `tests/command/reflog_test.rs`, `docs/commands/reflog.md`, `docs/commands/zh-CN/reflog.md`, `docs/development/commands/reflog.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/reflog.rs`, `tests/command/reflog_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/reflog.md`、`docs/commands/zh-CN/reflog.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md`（由 `--only reflog` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`reflog -1`。逐文件语义核对：`docs/development/commands/reflog.md` 与 `COMPATIBILITY.md`（`| reflog |` 第 249 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=reflog 限条输出末尾空行; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-06,DEP-MP-10; writeset=序列化于MP-06; release=independent; split-from=MP-06; exception=N/A`

---

### Task MP-07: `merge`/`pull` 冲突进度与 autostash 提示改走 stdout（#981）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 把 `merge`（及共用合并路径的 `pull`）的冲突进度（`Auto-merging`、`CONFLICT (...)`、`Squash commit -- not updating HEAD`、`Automatic merge failed; ...`）与 autostash 提示（`Created autostash: <oid7>`、``When finished, apply stashed changes with `git stash pop` ``）按 Git 写到 stdout，停止时不再输出 `Applying autostash resulted in conflicts.`；`--json` 下人读进度不写 stdout（失败报告按既有契约以 JSON 写到 stderr）。autostash 的存放与应用不在本卡（`MP-10`）。

**Out of scope:** autostash 的存放（`MERGE_AUTOSTASH`）与 `commit` 完成时的应用（由 `MP-10` 承接）、`--continue` 完成时的应用（由 `MP-43` 承接）、`--abort` 时的应用（由 `MP-39` 承接）；冲突退出码（由 `MP-22` 承接）；`-s resolve` 的平凡合并诊断（由 `MP-09` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| autostash 提示以 `eprintln!` 输出 | `src/command/merge/autostash.rs:463,471,580` |
| 冲突以 `PullMergeError::Conflicts` 错误返回，人读提示只出现在 stderr 的错误块 | `src/command/merge.rs:1021-1028`；`src/command/pull.rs:1223-1229` |
| Git 2.54.0 预探：冲突合并 stdout 为三行进度；`--squash --autostash` 冲突 stdout 依次为 `Created autostash: <oid7>`、两行进度、`Squash commit -- not updating HEAD`、`Automatic merge failed; ...`、``When finished, apply stashed changes with `git stash pop` `` | `git@6de20f6092` `builtin/merge.c:1858-1877` |

**Acceptance criteria:**

- [ ] AC1：冲突的 `libra merge topic` 的 stdout 为 Git 的三行进度。
- [ ] AC2：AC1 的 stderr 不含冲突进度行。
- [ ] AC3：`libra merge --squash --autostash topic`（`a` 脏）的 stdout 第一行为 autostash 行。
- [ ] AC4：AC3 的 stdout 其余行为 Git 的冲突与提示行。
- [ ] AC5：AC3 的 stderr 不含 autostash 应用冲突提示。
- [ ] AC6：冲突的 `libra pull`（F-PULLC 的 D）的 stdout 为 Git 的三行进度。
- [ ] AC7：`libra --json merge topic`（F-CONF）的 stdout 为空。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::conflict_and_autostash_progress_on_stdout` 承载 AC1–AC5、AC7；`(new)` `command::pull_test::conflict_progress_on_stdout` 承载 AC6；夹具 F-CONF（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 上提交 `B`（`f`=`base\n`）；`topic` 自 `B` 提交 `T`（`f`=`theirs\n`）；`main` 自 `B` 提交 `M`（`f`=`ours\n`），合并 `topic` 时 `f` 内容冲突，另在 `B` 中跟踪 `a`=`a0\n`；autostash 场景先 `echo dirty > a`（与合并路径不重叠）；pull 夹具（新用例写在 `tests/command/pull_test.rs`，复用该模块的 `create_remote_fixture`、`push_remote_commit`、`configure_pull_tracking`，`tests/command/pull_test.rs:87-147`）：`create_remote_fixture()` 以 `git` 建裸仓库 `remote.git` 与工作目录 `workdir` 并推送初始提交 `B`（`README.md`=`hello libra\n`），分支名 `<br>` 取其返回值；本地仓库 D = `init_repo_via_cli` + `configure_identity_via_cli`（身份 `Test User <test@example.com>`，pull 用例不依赖身份）+ `configure_pull_tracking(D, remote.git, <br>)` 后 `libra pull` 得到 `B`；`push_remote_commit(workdir, <br>, <path>, <content>, <msg>)` 推进上游，D 中本地提交带 `--no-gpg-sign`；F-PULLC：上游 `push_remote_commit` 把 `README.md` 改为 `up\n`，D 本地提交把 `README.md` 改为 `down\n`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出；stdout 恰为 `Auto-merging f\nCONFLICT (content): Merge conflict in f\nAutomatic merge failed; fix conflicts and then commit the result.\n`（Git 2.54.0 预探）。
- AC2：stderr 不含 `Auto-merging`、`CONFLICT (content)` 与 `Automatic merge failed` 中的任何一个。
- AC3：重建夹具（AC1 留下的待决冲突不得沿用），构造 autostash 场景（`echo dirty > a`）后调用；以非 0 退出；stdout 第一行匹配 `^Created autostash: [0-9a-f]{7}$`（Git 2.54.0 预探）。
- AC4：stdout 去掉第一行后恰为 `Auto-merging f\nCONFLICT (content): Merge conflict in f\nSquash commit -- not updating HEAD\nAutomatic merge failed; fix conflicts and then commit the result.\nWhen finished, apply stashed changes with `git stash pop`\n`（Git 2.54.0 预探，`#981` 要求 Git 原文）。
- AC5：stderr 不含 `Applying autostash resulted in conflicts.`。
- AC6：F-PULLC（pull 用例的首条映射，新建）；以非 0 退出；stdout 恰为 `Auto-merging README.md\nCONFLICT (content): Merge conflict in README.md\nAutomatic merge failed; fix conflicts and then commit the result.\n`（Git 2.54.0 预探同形；fetch 进度在 stderr）。
- AC7：F-CONF（重建，不沿用 AC3–AC5 的 autostash 仓库）；以非 0 退出，且 stderr 去掉首尾空白后可被 `serde_json` 解析为恰好一个 JSON 值、其 `error_code` 为 `"LBR-CONFLICT-002"`（执行条件：确为内容冲突的失败报告，既有契约见 `tests/command/pull_test.rs:752-753`）；stdout 为空。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5、AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::conflict_and_autostash_progress_on_stdout)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::conflict_and_autostash_progress_on_stdout)'` 须恰好列出该 1 条）
- [ ] 卡特有用例（AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::pull_test::conflict_progress_on_stdout)'`（`(new)`，在 `tests/command/pull_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::pull_test::conflict_progress_on_stdout)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::pull_test::/)'`（`command::pull_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-29`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/autostash.rs`, `src/command/merge/output.rs`, `src/command/pull.rs`, `tests/command/merge_test.rs`, `tests/command/pull_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/autostash.rs`, `src/command/merge/output.rs`, `src/command/pull.rs`, `tests/command/merge_test.rs`, `tests/command/pull_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`（由 `--only merge pull` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Automatic merge failed`。逐文件语义核对：`docs/development/commands/merge.md`、`docs/development/commands/pull.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行；`| pull |` 第 225 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=merge/pull 冲突与 autostash 提示输出流; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=2; prod-files=4; scope=M; deps=MP-29,DEP-MP-10; writeset=序列化于MP-29; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-08: 补全 `merge --verify-signatures` 成功行（#1004）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** `merge --verify-signatures`（及 `merge.verifySignatures=true`）验签通过时，与 Git 一致在 stdout 输出 `Commit <oid7> has a good GPG signature by <signer>`（`<signer>` 为验签所用仓库 vault 密钥的主用户 ID）；`-q` 抑制该行，`--json` 下不输出该文本行。无签名/坏签名的拒绝行为不变。（按 G-09 自本卡拆出 `MP-32`。）

**Out of scope:** 外部 GPG keyring 与信任数据库（Libra 只对仓库 vault 密钥验签，永久非目标）；`gpg.minTrustLevel`（由 `MP-32` 承接）；无签名/坏签名的拒绝文案（`#1004` 已判定正确）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `verify_merge_commit_signature` 在 `Good` 时静默返回 | `src/command/merge.rs:1634-1647` |
| Git 在验签通过且非 quiet 时 `printf("Commit %s has a good GPG signature by %s\n", hex, signer)` | `git@6de20f6092` `commit.c:1394-1396` |

**Acceptance criteria:**

- [ ] AC1：`libra merge --verify-signatures topic` 在 stdout 输出成功行。
- [ ] AC2：`merge.verifySignatures=true` 时 `libra merge topic` 同样输出成功行。
- [ ] AC3：`-q` 抑制成功行。
- [ ] AC4：`--json` 下 stdout 只含 JSON。
- [ ] AC5：无签名目标仍被拒绝。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::verify_signatures_good_line` 承载 AC1–AC5；夹具 F-SIG：以子进程环境 `GIT_COMMITTER_NAME='A U Thor'`、`GIT_COMMITTER_EMAIL=author@example.com` 执行 `libra init`（vault 密钥以 init 时的身份生成，`src/command/init.rs:2446-2460`），再执行 `libra config user.name 'A U Thor'`、`libra config user.email author@example.com` 与 `libra config vault.signing true`；`main` 提交 `B`；`topic` 自 `B` 提交已签名的 `T`（可快进）；`unsigned` 自 `B` 以 `--no-gpg-sign` 提交 `U`；`<T7>` 为 `T` 前 7 位；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-SIG（`HEAD` 为 `B`，`topic` 待快进）；退出 0；stdout 含一行恰为 `Commit <T7> has a good GPG signature by A U Thor <author@example.com>`（`#1004`；签名者为 vault 密钥主用户 ID，按「验证执行协议」第 4 条开工时在本夹具实测核对，不等即按 ER-03 处理）。
- AC2：重建夹具并 `libra config merge.verifySignatures true`；退出 0；stdout 含一行恰为 `Commit <T7> has a good GPG signature by A U Thor <author@example.com>`。
- AC3：重建夹具（`HEAD` 为 `B`，`topic` 待快进）；`libra merge -q --verify-signatures topic` 退出 0 且其后 `libra rev-parse HEAD` 等于 `T`（执行条件：确为验签通过后的快进，而非已是最新）；stdout 不含 `has a good GPG signature`（Git：`verbosity >= 0` 才打印）。
- AC4：重建夹具（`HEAD` 为 `B`，`topic` 待快进）；`libra --json merge --verify-signatures topic` 退出 0 且其后 `libra rev-parse HEAD` 等于 `T`（执行条件：确为验签通过后的快进）；stdout 去掉首尾空白后可被 `serde_json` 解析为恰好一个 JSON 值（混入人读行即解析失败）。
- AC5：重建夹具；`libra merge --verify-signatures unsigned` 以非 0 退出；stderr 含 `Error-Code: LBR-REPO-003`（既有行为，`src/command/merge.rs:986-990`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::verify_signatures_good_line)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::verify_signatures_good_line)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_config_history_defaults`（整 target，`merge.verifySignatures` 等历史默认配置；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_config_history_defaults` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-07`, `DEP-MP-02`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/output.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/output.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`has a good GPG signature`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--verify-signatures 成功输出; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-07,DEP-MP-02,DEP-MP-10; writeset=序列化于MP-07; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-32: `merge --verify-signatures` 读取并校验 `gpg.minTrustLevel`（#1004）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-08` 拆出：验签时读取 `gpg.minTrustLevel`，按 Git 接受 `undefined`/`never`/`marginal`/`fully`/`ultimate`（大小写不敏感）；仓库 vault 密钥验签通过的签名按 `ultimate` 计，因而任一合法阈值都放行，非法值经 `HistoryConfigError::Invalid` 以 `LBR-CLI-002` fail-closed。Git 的「未受信任签名」分支在 Libra 不可达（无信任数据库），在 `COMPATIBILITY.md` 记为有意差异；与 Git 在每次读取配置时都拒绝非法值不同，Libra 只在请求验签时校验该键（同样记入有意差异）。

**Out of scope:** 成功输出行（由 `MP-08` 承接）；外部 keyring 与信任数据库（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `verify_merge_commit_signature` 不读取 `gpg.minTrustLevel`；`CommitSignatureStatus` 只有 `Unsigned`/`Good`/`Bad` | `src/command/merge.rs:1634-1647`；`src/command/commit.rs:3156-3163` |
| Git 以 `xstrdup_toupper` 后匹配 `UNDEFINED`..`ULTIMATE`，非法值报 `invalid value for 'gpg.mintrustlevel'` | `git@6de20f6092` `gpg-interface.c:205-209,222-233,765-776` |
| 非法历史配置值映射为 `LBR-CLI-002` | `src/command/merge.rs:1066-1070` |

**Acceptance criteria:**

- [ ] AC1：五个小写阈值都放行验签合并。
- [ ] AC2：阈值大小写不敏感。
- [ ] AC3：非法阈值以 `LBR-CLI-002` 拒绝。
- [ ] AC4：AC3 的拒绝不改变仓库快照。
- [ ] AC5：阈值不放行无签名提交。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::verify_signatures_min_trust_level` 承载 AC1–AC5；夹具 F-SIG：以子进程环境 `GIT_COMMITTER_NAME='A U Thor'`、`GIT_COMMITTER_EMAIL=author@example.com` 执行 `libra init`（vault 密钥以 init 时的身份生成，`src/command/init.rs:2446-2460`），再执行 `libra config user.name 'A U Thor'`、`libra config user.email author@example.com` 与 `libra config vault.signing true`；`main` 提交 `B`；`topic` 自 `B` 提交已签名的 `T`（可快进）；`unsigned` 自 `B` 以 `--no-gpg-sign` 提交 `U`；`<T7>` 为 `T` 前 7 位；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：对 `undefined`、`never`、`marginal`、`fully`、`ultimate` 各用一份重建的夹具执行 `libra config gpg.minTrustLevel <值>` 与 `libra merge --verify-signatures topic`；五次调用都退出 0（执行条件），每次之后 `libra rev-parse HEAD` 都等于 `T`。
- AC2：对 `UNDEFINED`、`Never`、`MARGINAL`、`Fully`、`ULTIMATE` 各用一份重建的夹具重复 AC1 的步骤；五次调用都退出 0（执行条件），每次之后 `libra rev-parse HEAD` 都等于 `T`。
- AC3：重建夹具，配置 `bogus`；`libra merge --verify-signatures topic` 以非 0 退出，stderr 含 `Error-Code: LBR-CLI-002`。
- AC4：AC3 调用前后的「仓库快照 Σ」逐项相等。
- AC5：重建夹具，配置 `undefined`；`libra merge --verify-signatures unsigned` 以非 0 退出，stderr 含 `Error-Code: LBR-REPO-003`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::verify_signatures_min_trust_level)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::verify_signatures_min_trust_level)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_config_history_defaults`（整 target，历史默认配置；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_config_history_defaults` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改共享配置读取器 src/command/history_config.rs（ER-13 T-1）` | **Dependencies:** `MP-08`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/history_config.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/history_config.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`gpg.minTrustLevel`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`COMPATIBILITY.md` `| merge |` 行必须更新：记录「vault 验签视为 `ultimate`、无未受信任分支、仅验签时校验 `gpg.minTrustLevel`」的有意差异；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--verify-signatures 的 gpg.minTrustLevel; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=MP-08,DEP-MP-10; writeset=序列化于MP-08; release=independent; split-from=MP-08; exception=N/A`

---

### Task MP-09: 补全 `merge -s resolve` 平凡合并诊断（`Wonderful.` / `In-index merge`）（#1001）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** `-s resolve`（Git 中允许平凡合并的策略）在单一目标、单一合并基、索引与 HEAD 一致且非 `--no-commit`/`--ff-only` 时，先尝试「真正平凡的索引内合并」：stdout 输出 `Trying really trivial in-index merge...`，成功则输出 `Wonderful.` 并以 `In-index merge` 完成（非 quiet 时打印），否则输出 `Nope.` 后走原三方路径；`ort`/`recursive` 不走该路径。

**Out of scope:** `ort`/`recursive` 输出格式；`git-merge-resolve` 脚本的 `Trying simple merge.` 等后续文案（尚未排期，`#1001` 只跟踪平凡路径诊断）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `-s resolve` 复用三方路径，无平凡合并诊断 | `src/command/merge.rs:1786,3377,3451`；`src/command/merge/output.rs` |
| Git 平凡路径：`printf("Trying really trivial in-index merge...\n")`、`Nope.`、`merge_trivial` 打印 `Wonderful.` 并以 `In-index merge` 调用 `finish` | `git@6de20f6092` `builtin/merge.c:1000-1007,1699-1731`；`all_strategy` 中 `resolve` 未设 `NO_TRIVIAL` |

**Acceptance criteria:**

- [ ] AC1：平凡场景 `libra merge -s resolve C` 的 stdout 以 Git 三行诊断开头。
- [ ] AC2：平凡场景创建合并提交。
- [ ] AC3：平凡合并的提交树同时含两侧改动。
- [ ] AC4：全局 `-q` 下平凡场景不输出诊断。
- [ ] AC5：非平凡场景 stdout 以 `Trying really trivial in-index merge...` 与 `Nope.` 两行开头。
- [ ] AC6：默认 `ort` 不走平凡路径。
- [ ] AC7：`--json` 下 stdout 只含 JSON。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::resolve_trivial_in_index_diagnostics` 承载 AC1–AC7；夹具 F-RES：`main` 提交 `B`（`a`=`a0\n`、`f`=`f0\n`），再提交 `M`（新增 `m`=`m\n`）；`C` 自 `B` 把 `f` 改为 `fC\n`（平凡：两侧无同一文件修改）；`D` 自 `B` 把 `f` 改为 `fD\n`；非平凡场景中 `main` 自 `M` 再提交 `M2`（`f`=`fM\n`），使两侧都修改 `f`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：在 `M` 上执行，退出 0；stdout 以 `Trying really trivial in-index merge...\nWonderful.\nIn-index merge\n` 开头（Git 2.54.0 预探）。
- AC2：AC1 之后 HEAD 的父提交恰为 `[M, C]`（Git 2.54.0 预探）。
- AC3：AC1 之后 `libra ls-tree -r HEAD` 的 stdout 恰为 `100644 blob <a0>\ta\n100644 blob <fC>\tf\n100644 blob <m>\tm\n`，三者依次为 `libra hash-object` 对 `a0\n`、`fC\n`、`m\n` 的 OID（Git 2.54.0 预探：`git ls-tree -r HEAD` 同形；只保留 `M` 树的错误实现缺 `fC`）。
- AC4：重建夹具，在 `M` 上执行 `libra merge -q -s resolve C`：退出 0；stdout 为空（Libra 全局 `-q/--quiet` 抑制全部标准 stdout，`src/cli.rs:282-286`；Git 2.54.0 预探在 `-q` 下仍打印前两行，记为有意差异）。
- AC5：重建夹具并构造非平凡场景（`main` 自 `M` 提交 `M2`），在 `M2` 上 `libra merge -s resolve D`；stdout 以 `Trying really trivial in-index merge...\nNope.\n` 开头（Git 2.54.0 预探）。
- AC6：重建夹具，在 `M` 上执行 `libra merge C`：退出 0；stdout 不含 `Trying really trivial in-index merge`。
- AC7：重建夹具，在 `M` 上执行 `libra --json merge -s resolve C`：退出 0；stdout 去掉首尾空白后可被 `serde_json` 解析为恰好一个 JSON 值（混入人读行即解析失败）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::resolve_trivial_in_index_diagnostics)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::resolve_trivial_in_index_diagnostics)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-32`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/output.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/output.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`In-index merge`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`COMPATIBILITY.md` `| merge |` 行必须更新：记录 `-s resolve` 平凡合并诊断，以及 `-q` 下不输出这两行（Git 仍输出）的有意差异；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=-s resolve 平凡索引内合并诊断; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-32,DEP-MP-10; writeset=序列化于MP-32; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-10: 由 `libra commit` 应用并清理 `MERGE_AUTOSTASH`（#991）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：既有 `commit` 完成路径已调用 `conclude_merge_after_commit` 应用/清理 held autostash；补齐 AC1–AC7 断言：sidecar 持有、stash list 为空、提交树不含本地修改、工作树恢复、stderr 提示、sidecar 删除、双父提交。C 组 patch +1。偏差：按简化无冲突夹具验证，用户已取消独立审计与全量测试。）

**Description:** 按 `ADR-MP-04`，停在提交前的合并（`--no-commit`、冲突）的 `--autostash` 只记录在 `merge-autostash.json`、不进入 `stash list`；随后完成合并的 `libra commit` 按 Git 三方逻辑把 autostash 应用到合并结果之上、在 stderr 输出 `Applied autostash.` 并删除 sidecar；（按 G-09 自本卡拆出 `MP-30`、`MP-39`、`MP-43`、`MP-48`。）

**Out of scope:** `merge --continue` 完成路径（由 `MP-43` 承接）；应用冲突时转存 `stash list`（由 `MP-48` 承接）；`--abort` 与可快进 `--squash`（由 `MP-39` 承接）；干净三方 `--squash`（由 `MP-41` 承接）；冲突三方 `--squash`（由 `MP-45` 承接）；`rev-parse`/`stash show` 的 `MERGE_AUTOSTASH`（由 `MP-30`、`MP-44` 承接）；冲突与提示的输出流（由 `MP-07` 承接）；`pull --rebase --autostash`（独立 stash push/pop）；`merge --quit` 保留 autostash（`#991` Expected 4）——既有 `run_merge_quit` 已把持有的 autostash 存入 `stash list`（`src/command/merge.rs:4592-4615`），本卡不改。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `merge-autostash.json` 保存 stash 提交 OID，且不在 `refs/stash` | `src/command/merge/autostash.rs:6-31` |
| `#991` 实测：`libra commit` 完成合并时输出 `Applying autostash resulted in conflicts.`、文件未恢复且 autostash 残留为 `stash@{0}` | `#991`；`src/command/merge/autostash.rs:394-480,524-580`；`src/command/commit.rs` |
| Git：`commit` 完成后调用 `apply_autostash_ref("MERGE_AUTOSTASH")`；上游 `t7600` 比较已提交 `file` 为 `result.1-5`、工作树 `file` 为 `result.1-5-9` | `git@6de20f6092` `builtin/commit.c:1982`；`t/t7600-merge.sh:841-866` |

**Acceptance criteria:**

- [ ] AC1：`libra merge --no-commit --autostash topic`（F-991）后 autostash 不在 `stash list`。
- [ ] AC2：AC1 之后 sidecar 记录的正是该本地修改。
- [ ] AC3：AC1 之后 `libra commit -m done` 提交不含本地修改的合并结果。
- [ ] AC4：AC3 之后工作树为合并结果加本地修改。
- [ ] AC5：AC3 的 `commit` 在 stderr 输出 `Applied autostash.`。
- [ ] AC6：AC3 之后 sidecar 被删除。
- [ ] AC7：AC3 的提交为双父合并提交。
- [x] ER-06/ER-06a：既有 `merge`/`commit` 文档已描述 commit 完成合并并应用 held autostash；本次无需新增可见行为文档。

**AC → 断言映射:**（`(new)` `command::merge_test::merge_autostash_applied_by_commit` 承载 AC1–AC7；夹具 F-991（`#991` 形态）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`file`=`1\n2\n3\n4\n5\n6\n7\n8\n9\n`）；`topic` 自 `B` 把第 5 行改为 `five` 提交 `T`；`main` 自 `B` 提交 `M`（新增 `h`=`m1\n`），二者无冲突；调用前把工作树 `file` 第 9 行改为 `nine`（与合并改动不重叠的同文件修改），记合并前 `libra diff` 的 stdout 为 `D0`；`R15` = 第 5 行为 `five` 的内容，`R159` = 第 5 行为 `five` 且第 9 行为 `nine` 的内容；应用冲突变体 F-991X：`topic` 另把第 9 行改为 `NINE`；快进变体 F-991F：`main` 停在 `B`（`topic` 可快进）；冲突变体 F-991C：`main` 自 `B` 提交的 `M` 改为只把第 5 行改为 `FIVE`（不新增 `h`，与 `topic` 冲突）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：退出 0；`libra stash list` 的 stdout 为空（Git 2.54.0 预探）。
- AC2：读取 `.libra/merge-autostash.json` 的 `stash_commit` 字段值记为 `<S>`（文件不存在即判失败）；以子进程参数 `diff`、`--old`、`<S>^`、`--new`、`<S>`（`<S>` 替换为该 OID）执行 `libra`，退出 0（执行条件），stdout 等于 `D0`。
- AC3：退出 0（执行条件）；`libra ls-tree -r HEAD` 的 stdout 恰为 `100644 blob <R15>\tfile\n100644 blob <m1>\th\n`（`<R15>`/`<m1>` 为 `libra hash-object` 对内容 `R15` 与 `m1\n` 的 OID；即 `t7600` 的 `result.1-5` 加 `M` 新增的 `h`，不含本地修改，且不缺路径）。
- AC4：AC3 之后的「工作树清单」（「验证执行协议」第 8 条）恰为普通文件 `file`=`R159` 与 `h`=`m1\n`（均不可执行，无其它路径；`t7600` 的 `result.1-5-9`）。
- AC5：AC3 调用的 stderr 含一行恰为 `Applied autostash.`（Git 2.54.0 预探）。
- AC6：`.libra/merge-autostash.json` 不存在。
- AC7：HEAD 的父提交恰为 `[M, T]`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`merge_autostash_applied_by_commit`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-09`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `src/command/commit.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `src/command/commit.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`（由 `--only merge commit` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Applied autostash`。逐文件语义核对：`docs/development/commands/merge.md`、`docs/development/commands/commit.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行；`| commit |` 第 209 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MERGE_AUTOSTASH 由 commit 完成合并时应用与清理; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=MP-09,DEP-MP-10; writeset=序列化于MP-09; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-43: 由 `merge --continue` 应用并清理 `MERGE_AUTOSTASH`（#991）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--continue` 既有路径已使用预检 held autostash 快照并调用 `resolve_pending_autostash_with`；补齐 AC1–AC5 用例，验证合并树不含本地修改、工作树恢复、stderr `Applied autostash.`、sidecar 删除。C 组 patch +1。偏差：merge 模块 319/320，唯一失败为既有 GC/restart 用例；用户已取消独立审计与全量测试。）

**Description:** 自 `MP-10` 拆出：`libra merge --continue` 完成停在提交前的合并时，与 `libra commit` 一致按三方逻辑应用 `merge-autostash.json` 记录的 autostash、在 stderr 输出 `Applied autostash.` 并删除 sidecar。

**Out of scope:** `libra commit` 完成路径（由 `MP-10` 承接）；应用冲突时转存 `stash list`（由 `MP-48` 承接）；`--abort` 与可快进 `--squash`（由 `MP-39` 承接）；干净三方 `--squash`（由 `MP-41` 承接）；冲突三方 `--squash`（由 `MP-45` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--continue` 的完成路径与 `commit` 分别实现 autostash 收尾 | `src/command/merge.rs:4259-4380`（`run_merge_continue`）；`src/command/merge/autostash.rs:524-580` |
| Git：`merge --continue` 以 `git commit` 完成（同一 `apply_autostash_ref`）；`t7600` 对 `--continue` 路径做同样的 `result.1-5`/`result.1-5-9` 比较 | `git@6de20f6092` `builtin/merge.c`（`--continue` 调用 commit）；`t/t7600-merge.sh:841-866` |

**Acceptance criteria:**

- [ ] AC1：`libra merge --no-commit --autostash topic` 后 `libra merge --continue -m done` 提交不含本地修改的合并结果。
- [ ] AC2：AC1 之后工作树为合并结果加本地修改。
- [ ] AC3：AC1 的 `--continue` 在 stderr 输出 `Applied autostash.`。
- [ ] AC4：AC1 之后 sidecar 被删除。
- [ ] AC5：AC1 的提交为双父合并提交。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::merge_autostash_applied_by_continue` 承载 AC1–AC5；夹具 F-991（`#991` 形态）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`file`=`1\n2\n3\n4\n5\n6\n7\n8\n9\n`）；`topic` 自 `B` 把第 5 行改为 `five` 提交 `T`；`main` 自 `B` 提交 `M`（新增 `h`=`m1\n`），二者无冲突；调用前把工作树 `file` 第 9 行改为 `nine`（与合并改动不重叠的同文件修改），记合并前 `libra diff` 的 stdout 为 `D0`；`R15` = 第 5 行为 `five` 的内容，`R159` = 第 5 行为 `five` 且第 9 行为 `nine` 的内容；应用冲突变体 F-991X：`topic` 另把第 9 行改为 `NINE`；快进变体 F-991F：`main` 停在 `B`（`topic` 可快进）；冲突变体 F-991C：`main` 自 `B` 提交的 `M` 改为只把第 5 行改为 `FIVE`（不新增 `h`，与 `topic` 冲突）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：两条命令均退出 0（执行条件）；`libra ls-tree -r HEAD` 的 stdout 恰为 `100644 blob <R15>\tfile\n100644 blob <m1>\th\n`（`<R15>`/`<m1>` 为 `libra hash-object` 对内容 `R15` 与 `m1\n` 的 OID；即 `t7600` 的 `result.1-5` 加 `M` 新增的 `h`，不含本地修改，且不缺路径）。
- AC2：AC1 之后的「工作树清单」（「验证执行协议」第 8 条）恰为普通文件 `file`=`R159` 与 `h`=`m1\n`（均不可执行，无其它路径；Git 2.54.0 预探：`Applied autostash.`）。
- AC3：`--continue` 调用的 stderr 含一行恰为 `Applied autostash.`。
- AC4：`.libra/merge-autostash.json` 不存在。
- AC5：HEAD 的父提交恰为 `[M, T]`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`merge_autostash_applied_by_continue`（通过）
- [x] 共享回归组：merge_test 320 条，319 通过；唯一失败为既有 `merge_crisscross_gc_reclaims_the_virtual_ancestor_and_restart_recovers`（GC/restart 回归，与本卡无关）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-10`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/autostash.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/autostash.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`merge --continue`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MERGE_AUTOSTASH 由 merge --continue 应用与清理; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-10,DEP-MP-10; writeset=序列化于MP-10; release=independent; split-from=MP-10; exception=N/A`

---

### Task MP-48: autostash 应用冲突时转存 `stash list`（#991）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：autostash re-apply 冲突统一输出 `Your local changes are stashed, however applying them resulted in conflicts.`，保存到 `stash list` 并删除 sidecar；`commit` 与 `merge --continue` 两条完成路径均验证。C 组 patch +1。偏差：用户已取消独立审计与全量测试。）

**Description:** 自 `MP-10` 拆出：由 `libra commit` 或 `libra merge --continue` 完成合并时若 autostash 与合并结果冲突，按 pinned Git 在 stderr 输出以 `Your local changes are stashed, however applying them` 开头的提示并把该 autostash 保存进 `stash list`（不丢弃），同时删除 sidecar。

**Out of scope:** 无冲突时的应用（由 `MP-10`、`MP-43` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#991` 实测：`libra commit` 输出 `Applying autostash resulted in conflicts.` 且 autostash 残留为 `stash@{0}`（Libra 现状是应用失败而非 Git 的三方应用后才冲突） | `#991`；`src/command/merge/autostash.rs:524-580` |
| pinned Git：应用冲突时先 `stash store` 保存，再输出 `Your local changes are stashed, however applying them` 开头的五行提示 | `git@6de20f6092` `sequencer.c:4867-4900`（`apply_save_autostash_oid`） |

**Acceptance criteria:**

- [ ] AC1：由 `libra commit` 完成合并时，autostash 应用冲突即在 stderr 提示冲突。
- [ ] AC2：AC1 的应用冲突时保存进 `stash list` 的正是该 autostash。
- [ ] AC3：AC1 之后 sidecar 被删除。
- [ ] AC4：由 `libra merge --continue` 完成合并时，autostash 应用冲突同样在 stderr 提示冲突（`--continue` 与 `commit` 的完成路径分别实现，`src/command/merge.rs:4259-4380`）。
- [ ] AC5：AC4 的应用冲突时保存进 `stash list` 的正是该 autostash。
- [ ] AC6：AC4 之后 sidecar 被删除。
- [x] ER-06/ER-06a：`merge` 中英文文档已写明冲突 re-apply 的 stderr 提示与 stash 保存；网站同步。

**AC → 断言映射:**（`(new)` `command::merge_test::merge_autostash_apply_conflict_stored` 承载 AC1–AC6；夹具 F-991（`#991` 形态）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`file`=`1\n2\n3\n4\n5\n6\n7\n8\n9\n`）；`topic` 自 `B` 把第 5 行改为 `five` 提交 `T`；`main` 自 `B` 提交 `M`（新增 `h`=`m1\n`），二者无冲突；调用前把工作树 `file` 第 9 行改为 `nine`（与合并改动不重叠的同文件修改），记合并前 `libra diff` 的 stdout 为 `D0`；`R15` = 第 5 行为 `five` 的内容，`R159` = 第 5 行为 `five` 且第 9 行为 `nine` 的内容；应用冲突变体 F-991X：`topic` 另把第 9 行改为 `NINE`；快进变体 F-991F：`main` 停在 `B`（`topic` 可快进）；冲突变体 F-991C：`main` 自 `B` 提交的 `M` 改为只把第 5 行改为 `FIVE`（不新增 `h`，与 `topic` 冲突）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-991X；`libra merge --no-commit --autostash topic` 后 `libra commit -m done` 退出 0（执行条件），stderr 含 `Your local changes are stashed, however applying them`（`git@6de20f6092` `sequencer.c:4894-4900`）。
- AC2：AC1 之后 `libra stash show -p stash@{0}` 的 stdout 等于 `D0`（F-991X 合并前的 `libra diff` 输出）。
- AC3：AC1 之后 `.libra/merge-autostash.json` 不存在。
- AC4：F-991X（重建，不沿用 AC1 的仓库）；`libra merge --no-commit --autostash topic` 后 `libra merge --continue -m done` 退出 0（执行条件），stderr 含 `Your local changes are stashed, however applying them`（Git 的 `merge --continue` 以 `commit` 完成，同一 `apply_autostash_ref` 路径）。
- AC5：AC4 之后 `libra stash show -p stash@{0}` 的 stdout 等于 `D0`。
- AC6：AC4 之后 `.libra/merge-autostash.json` 不存在。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`merge_autostash_apply_conflict_stored`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-43`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`（由 `--only merge commit` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Your local changes are stashed`。逐文件语义核对：`docs/development/commands/merge.md`、`docs/development/commands/commit.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行；`| commit |` 第 209 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MERGE_AUTOSTASH 应用冲突的转存; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-43,DEP-MP-10; writeset=序列化于MP-43; release=independent; split-from=MP-10; exception=N/A`

---

### Task MP-39: `--abort` 与可快进 `--squash` 的 autostash 应用与清理（#991）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：既有实现已覆盖冲突合并 `--abort` 恢复 autostash 与可快进 `--squash --autostash` 立即恢复；补 AC1–AC5 用例，验证本地修改收起/恢复、sidecar 与 stash list 为空、squash 索引不含本地修改。C 组 patch +1。偏差：使用既有冲突/可快进夹具，用户已取消独立审计与全量测试。）

**Description:** 自 `MP-10` 拆出：冲突合并后 `libra merge --abort` 恢复合并前状态时应用 `merge-autostash.json` 记录的 autostash 并删除 sidecar；可快进的 `--squash --autostash` 与 Git 一致在合并结束时立即应用 autostash（`finish` 收到新 HEAD），不留 sidecar。（按 G-09 自本卡拆出 `MP-41` 承接三方 `--squash` 路径。）

**Out of scope:** `commit`/`--continue` 完成普通合并的应用（由 `MP-10`、`MP-43` 承接）；干净三方 `--squash` 的保留与应用（由 `MP-41` 承接）；冲突三方 `--squash` 的保留与应用（由 `MP-45` 承接）；`rev-parse`/`stash show` 的 `MERGE_AUTOSTASH`（由 `MP-30`、`MP-44` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--abort` 路径与 autostash sidecar 的处理 | `src/command/merge/autostash.rs:394-480,524-580`；`src/command/merge.rs`（`run_merge_abort`） |
| Git：`finish()` 在 `new_head` 非空时立即 `apply_autostash_ref`（快进 squash 即如此）；`--abort` 应用 autostash（Git 2.54.0 预探：`Applied autostash.`） | `git@6de20f6092` `builtin/merge.c:495-496,539-541,1690` |

**Acceptance criteria:**

- [ ] AC1：冲突合并停止时本地修改已被收起。
- [ ] AC2：AC1 之后 `libra merge --abort` 完整恢复合并前状态（含本地修改、无 sidecar、无新增 stash）。
- [ ] AC3：可快进的 `--squash --autostash` 在合并结束时立即应用 autostash。
- [ ] AC4：AC3 的索引为 squash 结果、不含本地修改。
- [ ] AC5：AC3 之后不留 sidecar。
- [x] ER-06/ER-06a：既有 `merge` 文档已描述 `--abort` 重新应用 held autostash 与可快进 squash 立即恢复；本次无需新增可见行为文档。

**AC → 断言映射:**（`(new)` `command::merge_test::merge_autostash_abort_and_ff_squash` 承载 AC1–AC5；夹具 F-991（`#991` 形态）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`file`=`1\n2\n3\n4\n5\n6\n7\n8\n9\n`）；`topic` 自 `B` 把第 5 行改为 `five` 提交 `T`；`main` 自 `B` 提交 `M`（新增 `h`=`m1\n`），二者无冲突；调用前把工作树 `file` 第 9 行改为 `nine`（与合并改动不重叠的同文件修改），记合并前 `libra diff` 的 stdout 为 `D0`；`R15` = 第 5 行为 `five` 的内容，`R159` = 第 5 行为 `five` 且第 9 行为 `nine` 的内容；应用冲突变体 F-991X：`topic` 另把第 9 行改为 `NINE`；快进变体 F-991F：`main` 停在 `B`（`topic` 可快进）；冲突变体 F-991C：`main` 自 `B` 提交的 `M` 改为只把第 5 行改为 `FIVE`（不新增 `h`，与 `topic` 冲突）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-991C；`libra merge --autostash topic` 以非 0 退出且 stderr 含 `Error-Code: LBR-CONFLICT-002`（执行条件：确为内容冲突）；工作树 `file` 不含 `nine`（本地修改已存入 autostash，冲突块之后的末行为原值 `9`；Git 2.54.0 预探同形）。
- AC2：`libra merge --abort` 退出 0；之后的「仓库快照 Σ」与 AC1 调用前的 Σ 逐项相等（Σ 含工作树 `file` 的 `nine`、`.libra/` 顶层文件集合与 `stash list`；Git 2.54.0 预探：`Applied autostash.`）。
- AC3：F-991F；`libra merge --squash --autostash topic` 退出 0；「工作树清单」（「验证执行协议」第 8 条）恰为普通文件 `file`=`R159`（不可执行，无其它路径；F-991F 的 `main` 停在 `B`，暂存的 squash 结果与本地修改都可见；Git 2.54.0 预探：stderr `Applied autostash.`）。
- AC4：AC3 之后 `libra ls-files -s` 的 stdout 恰为 `100644 <R15> 0\tfile\n`（`<R15>` 为 `libra hash-object` 对内容 `R15` 的 OID；F-991F 只有路径 `file`，多出路径、错误模式或含本地修改的 blob 都使输出不同）。
- AC5：`.libra/merge-autostash.json` 不存在。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`merge_autostash_abort_and_ff_squash`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-48`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`--squash --autostash`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MERGE_AUTOSTASH 的 abort 与快进 squash 路径; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-48,DEP-MP-10; writeset=序列化于MP-48; release=independent; split-from=MP-10; exception=N/A`

---

### Task MP-41: 干净三方 `--squash --autostash` 保留 autostash 并由 `commit` 应用（#991）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-39` 拆出：干净的三方 `--squash --autostash` 停在提交前时，autostash 只记录在 `merge-autostash.json`、不在合并结束时应用、不进入 `stash list`；随后的 `libra commit` 应用并删除 sidecar（Git 保留 `MERGE_AUTOSTASH` 由 `commit` 应用）。（按 G-09 自本卡拆出 `MP-45` 承接冲突的三方 squash。）

**Out of scope:** 冲突的三方 squash（由 `MP-45` 承接）；`--abort` 与可快进 squash（由 `MP-39` 承接）；普通合并的完成路径（由 `MP-10`、`MP-43` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--squash --autostash` 停止时 Libra 把 autostash 放进 `stash list`（`Your changes are safe in the stash (stash@{0})`） | `#981`；`src/command/merge/autostash.rs:463,471,580` |
| Git：三方 squash 以 `finish(head, remoteheads, NULL, NULL)` 收尾（`new_head` 为空，不应用），`commit` 结束时 `apply_autostash_ref`（Git 2.54.0 预探：干净三方 squash 后本地修改未恢复、`MERGE_AUTOSTASH` 存在，`git commit` 后输出 `Applied autostash.`） | `git@6de20f6092` `builtin/merge.c:1860-1877`；`builtin/commit.c:1982` |

**Acceptance criteria:**

- [ ] AC1：干净三方 `--squash --autostash` 结束时工作树只含 squash 结果、不应用 autostash。
- [ ] AC2：AC1 之后索引恰为 squash 结果。
- [ ] AC3：AC1 之后 sidecar 记录的正是该本地修改。
- [ ] AC4：AC1 之后 autostash 不在 `stash list`。
- [ ] AC5：AC1 之后 `libra commit` 应用 autostash。
- [ ] AC6：AC5 的提交为不含本地修改的 squash 结果。
- [ ] AC7：AC5 之后 sidecar 被删除。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::merge_autostash_clean_three_way_squash` 承载 AC1–AC7；夹具 F-991（`#991` 形态）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`file`=`1\n2\n3\n4\n5\n6\n7\n8\n9\n`）；`topic` 自 `B` 把第 5 行改为 `five` 提交 `T`；`main` 自 `B` 提交 `M`（新增 `h`=`m1\n`），二者无冲突；调用前把工作树 `file` 第 9 行改为 `nine`（与合并改动不重叠的同文件修改），记合并前 `libra diff` 的 stdout 为 `D0`；`R15` = 第 5 行为 `five` 的内容，`R159` = 第 5 行为 `five` 且第 9 行为 `nine` 的内容；应用冲突变体 F-991X：`topic` 另把第 9 行改为 `NINE`；快进变体 F-991F：`main` 停在 `B`（`topic` 可快进）；冲突变体 F-991C：`main` 自 `B` 提交的 `M` 改为只把第 5 行改为 `FIVE`（不新增 `h`，与 `topic` 冲突）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-991；`libra merge --squash --autostash topic` 退出 0（执行条件）；「工作树清单」（「验证执行协议」第 8 条）恰为普通文件 `file`=`R15` 与 `h`=`m1\n`（均不可执行，无其它路径；Git 2.54.0 预探：本地修改尚未恢复）。
- AC2：AC1 之后 `libra ls-files -s` 的 stdout 恰为 `100644 <R15> 0\tfile\n100644 <m1> 0\th\n`，`<R15>`/`<m1>` 为 `libra hash-object` 对内容 `R15` 与 `m1\n` 的 OID（Git 2.54.0 预探：暂存 squash 结果）。
- AC3：读取 `.libra/merge-autostash.json` 的 `stash_commit` 字段值记为 `<S>`（文件不存在即判失败）；以子进程参数 `diff`、`--old`、`<S>^`、`--new`、`<S>`（`<S>` 替换为该 OID）执行 `libra`，退出 0（执行条件），stdout 等于 `D0`。
- AC4：`libra stash list` 的 stdout 为空（Git 2.54.0 预探）。
- AC5：`libra commit --no-edit --cleanup=whitespace` 退出 0（执行条件）；「工作树清单」（「验证执行协议」第 8 条）恰为普通文件 `file`=`R159` 与 `h`=`m1\n`（均不可执行，无其它路径；Git 2.54.0 预探）。
- AC6：AC5 之后 `libra ls-tree -r HEAD` 的 stdout 恰为 `100644 blob <R15>\tfile\n100644 blob <m1>\th\n`（`<R15>`/`<m1>` 为 `libra hash-object` 对内容 `R15` 与 `m1\n` 的 OID；即 squash 结果 `R15` 加 `M` 的 `h`，不含本地修改，且不缺路径）。
- AC7：`.libra/merge-autostash.json` 不存在。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::merge_autostash_clean_three_way_squash)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::merge_autostash_clean_three_way_squash)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-39`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `src/command/commit.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `src/command/commit.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`（由 `--only merge commit` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`--squash --autostash`。逐文件语义核对：`docs/development/commands/merge.md`、`docs/development/commands/commit.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行；`| commit |` 第 209 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=干净三方 squash 的 MERGE_AUTOSTASH 保留与应用; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=MP-39,DEP-MP-10; writeset=序列化于MP-39; release=independent; split-from=MP-39; exception=N/A`

---

### Task MP-45: 冲突三方 `--squash --autostash` 保留 autostash 并由 `commit` 应用（#981, #991）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-41` 拆出：冲突的三方 `--squash --autostash` 停止时，autostash 只记录在 `merge-autostash.json`、不进入 `stash list`（取代 Libra 现把它放进 `stash list` 的行为）；用户解决冲突后的 `libra commit` 应用并删除 sidecar。

**Out of scope:** 干净三方 squash（由 `MP-41` 承接）；冲突提示的输出流（由 `MP-07` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--squash --autostash` 冲突时 Libra 把 autostash 放进 `stash list` | `#981`；`src/command/merge/autostash.rs:463,471,580` |
| Git 2.54.0 预探：冲突 squash 后 `stash list` 为空、`MERGE_AUTOSTASH` 存在；解决后 `git commit --no-edit` 输出 `Applied autostash.` 且工作树含本地修改 | `git@6de20f6092` `builtin/merge.c:1860-1877`；`builtin/commit.c:1982` |
| `#981` AC4 要求冲突 squash 后 `libra stash list` 显示该 autostash、`libra stash pop` 恢复，与 pinned Git 不符：Git 此时 `stash list` 为空（上一行预探），却照常打印 ``When finished, apply stashed changes with `git stash pop` ``（`builtin/merge.c:1875-1876`，`t7600#15` 逐字断言）。本卡按 Git（`ADR-MP-04`）实现、提示按 Git 原文保留（`MP-07`）；用户的恢复路径是解决冲突后由 `libra commit` 自动应用（AC4）；持有期间可读取 `.libra/merge-autostash.json` 的 `stash_commit` 并以 `libra show <该 OID>` 查看，`MP-44` 发布后亦可 `libra stash show -p MERGE_AUTOSTASH`。`#981` AC4 的 `stash list` 部分登记为与 Git 一致的有意差异，关闭 `#981` 时注明 | `git@6de20f6092` `builtin/merge.c:1863-1876`；`t/t7600-merge.sh:339-345`；`#981` |

**Acceptance criteria:**

- [ ] AC1：冲突三方 `--squash --autostash` 停止时本地修改已被收起。
- [ ] AC2：AC1 之后 sidecar 记录的正是该本地修改。
- [ ] AC3：AC1 之后 autostash 不在 `stash list`。
- [ ] AC4：解决冲突后 `libra commit` 应用 autostash。
- [ ] AC5：AC4 之后 sidecar 被删除。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::merge_autostash_conflicted_three_way_squash` 承载 AC1–AC5；夹具 F-991（`#991` 形态）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`file`=`1\n2\n3\n4\n5\n6\n7\n8\n9\n`）；`topic` 自 `B` 把第 5 行改为 `five` 提交 `T`；`main` 自 `B` 提交 `M`（新增 `h`=`m1\n`），二者无冲突；调用前把工作树 `file` 第 9 行改为 `nine`（与合并改动不重叠的同文件修改），记合并前 `libra diff` 的 stdout 为 `D0`；`R15` = 第 5 行为 `five` 的内容，`R159` = 第 5 行为 `five` 且第 9 行为 `nine` 的内容；应用冲突变体 F-991X：`topic` 另把第 9 行改为 `NINE`；快进变体 F-991F：`main` 停在 `B`（`topic` 可快进）；冲突变体 F-991C：`main` 自 `B` 提交的 `M` 改为只把第 5 行改为 `FIVE`（不新增 `h`，与 `topic` 冲突）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-991C；`libra merge --squash --autostash topic` 以非 0 退出且 stderr 含 `Error-Code: LBR-CONFLICT-002`（执行条件：确为内容冲突）；工作树 `file` 不含 `nine`。
- AC2：读取 `.libra/merge-autostash.json` 的 `stash_commit` 字段值记为 `<S>`（文件不存在即判失败）；以子进程参数 `diff`、`--old`、`<S>^`、`--new`、`<S>`（`<S>` 替换为该 OID）执行 `libra`，退出 0（执行条件），stdout 等于 `D0`。
- AC3：`libra stash list` 的 stdout 为空（Git 2.54.0 预探）。
- AC4：把 `file` 写为 `R15`、`libra add file`、`libra commit --no-edit --cleanup=whitespace` 退出 0（执行条件）；「工作树清单」（「验证执行协议」第 8 条）恰为普通文件 `file`=`R159`（不可执行，无其它路径；F-991C 不含 `h`；Git 2.54.0 预探）。
- AC5：`.libra/merge-autostash.json` 不存在。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::merge_autostash_conflicted_three_way_squash)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::merge_autostash_conflicted_three_way_squash)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-41`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `src/command/commit.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/autostash.rs`, `src/command/merge.rs`, `src/command/commit.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`（由 `--only merge commit` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`--squash --autostash`。逐文件语义核对：`docs/development/commands/merge.md`、`docs/development/commands/commit.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行；`| commit |` 第 209 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`COMPATIBILITY.md` `| merge |` 行必须更新：记录冲突 squash 的 autostash 按 Git 保存在 `MERGE_AUTOSTASH`、不进入 `stash list`（与 `#981` AC4 的期望不同、与 Git 一致），以及沿用 Git 原文的 `git stash pop` 提示；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=冲突三方 squash 的 MERGE_AUTOSTASH 保留与应用; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=MP-41,DEP-MP-10; writeset=序列化于MP-41; release=independent; split-from=MP-41; exception=N/A`

---

### Task MP-30: `rev-parse MERGE_AUTOSTASH` 伪引用接入（#991）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：新增 `MERGE_AUTOSTASH` 伪引用，按 sidecar 解析且仅 rev-parse 等 opt-in 读取；compat pseudo-ref 3/3、rev_parse 67/67；新增 merge 用例。C 组 patch +1。偏差：lib pseudo_ref 9/10，唯一失败为旧 SQLite fixture 遇默认 redb；用户已取消独立 Claude/Codex 审计与全量测试。）

**Description:** 自 `MP-10` 拆出：按 `ADR-MP-04` 在 `src/internal/pseudo_ref.rs` 新增 `MergeAutostash` 变体（按需读取 `merge-autostash.json` 的 `stash_commit`，不另存状态），在 `rev-parse` 的显式 opt-in 解析（`src/utils/util.rs` 中与 `AUTO_MERGE` 并列）接入 `MERGE_AUTOSTASH`，并同步 `tests/compat/pseudo_ref_surface.rs` 的 `DECLARED`、`COMPATIBILITY.md` 第 277 行与 `rev-parse` 文档。（按 G-09 自本卡拆出 `MP-44`。）

**Out of scope:** autostash 的应用与清理（由 `MP-10`、`MP-43`、`MP-48`、`MP-39`、`MP-41`、`MP-45` 承接）；`stash show -p MERGE_AUTOSTASH`（`#991` AC1，由 `MP-44` 承接）；其它命令接受该伪引用（尚未排期，`#991` 未要求）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `pseudo_ref.rs` 只声明 7 个伪引用，`AUTO_MERGE` 由 `merge-state.json` 派生 | `src/internal/pseudo_ref.rs:13-32,56,69,82,176` |
| `rev-parse` 只对 `AUTO_MERGE` opt-in 解析 | `src/utils/util.rs:3088-3111`；`src/command/rev_parse.rs:643` |
| `DECLARED: [&str; 7]` 与 `COMPATIBILITY.md` 第 277 行列出完整伪引用集合，二者由该 compat target 互相核对；`documented_pseudo_ref_set()` 以第 277 行中的「. `AUTO_MERGE` alone」作为列表结束锚点。本卡把第 277 行改写为同时声明 `MERGE_AUTOSTASH` 可解析（原地修改、不增删行），并同卡把该解析锚点改为与新句式一致的列表边界，使 `the_rev_parse_row_declares_every_pseudo_ref_name` 等用例按新集合核对（「验证执行协议」第 5 条） | `tests/compat/pseudo_ref_surface.rs:13-21,41-55,56-78`；`COMPATIBILITY.md:277` |
| 库内单元测试 `internal::pseudo_ref::tests::the_declared_set_is_the_c5_table` 精确比较 `PseudoRef::ALL` 的七个名称；本卡按第 5 条把 `MERGE_AUTOSTASH` 加入其期望集合，其余断言不变 | `src/internal/pseudo_ref.rs:452,1041` |

**Acceptance criteria:**

- [ ] AC1：待决 `--autostash` 合并期间 `rev-parse MERGE_AUTOSTASH` 给出 autostash 提交。
- [ ] AC2：合并完成后 `MERGE_AUTOSTASH` 不再解析。
- [ ] AC3：`tests/compat/pseudo_ref_surface.rs` 的 `DECLARED` 数组含 `MERGE_AUTOSTASH`。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::merge_autostash_rev_parse_pseudo_ref` 承载 AC1–AC2；夹具 F-AS（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`a`=`a0\n`、`f`=`base\n`）；`topic` 自 `B` 提交 `T`（`f`=`theirs\n`）；`main` 自 `B` 提交 `M`（新增 `h`=`m1\n`，与 `topic` 无冲突）；冲突变体 F-ASC：`main` 自 `B` 提交 `MC`（`f`=`ours\n`）；应用冲突变体 F-ASX：`topic` 的 `T` 另把 `a` 改为 `a-theirs\n`；快进变体 F-ASF：`main` 停在 `B`（`topic` 可快进）。每个场景调用前执行 `echo dirty > a`（工作树未暂存修改）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-AS；`libra merge --no-commit --autostash topic` 后 `libra rev-parse MERGE_AUTOSTASH` 退出 0，stdout 恰为 `.libra/merge-autostash.json` 中 `stash_commit` 的值加 `\n`。
- AC2：AC1 之后 `libra commit -m done`；`libra rev-parse MERGE_AUTOSTASH` 以非 0 退出，stderr 含 `Error-Code: LBR-CLI-003`（`src/command/rev_parse.rs:643-645,997-1006`：`rev_parse_target_error` 的 `InvalidReference` 分支映射为 `CliInvalidTarget`；缺失 sidecar 时 `MERGE_AUTOSTASH` 须走该分支，开工时按 ER-02 复核）。
- AC3：`awk '/^const DECLARED/,/^\];/' tests/compat/pseudo_ref_surface.rs | rg -F -- '"MERGE_AUTOSTASH"'` 退出 0（只检索 `DECLARED` 数组字面量；1 = 缺失、>1 = 命令失败，均判失败）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`merge_autostash_rev_parse_pseudo_ref`（通过）
- [ ] 卡特有判据（AC3，非 nextest 门）：`awk '/^const DECLARED/,/^\];/' tests/compat/pseudo_ref_surface.rs | rg -F -- '"MERGE_AUTOSTASH"'` 退出 0（1 = 缺失 → STOP；>1 = 命令失败 → STOP）。
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_pseudo_ref_surface`（整 target，伪引用声明面，本卡同步修改其 `DECLARED`（「验证执行协议」第 5 条）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_pseudo_ref_surface` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_parse_test::/)'`（`command::rev_parse_test` 模块族，守护 `rev-parse` 解析；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^internal::pseudo_ref::tests::/)'`（`src/internal/pseudo_ref.rs` 内单元测试，本卡同步改写其期望集合；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改共享解析辅助 src/utils/util.rs 与 src/internal/pseudo_ref.rs（ER-13 T-1）` | **Dependencies:** `MP-45`, `DEP-MP-10`

**Implementation write set:** `src/internal/pseudo_ref.rs`, `src/utils/util.rs`, `tests/command/merge_test.rs`, `tests/compat/pseudo_ref_surface.rs`, `docs/commands/rev-parse.md`, `docs/commands/zh-CN/rev-parse.md`, `docs/development/commands/rev-parse.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/internal/pseudo_ref.rs`, `src/utils/util.rs`, `tests/command/merge_test.rs`, `tests/compat/pseudo_ref_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md`（由 `--only rev-parse` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`MERGE_AUTOSTASH`。逐文件语义核对：`docs/development/commands/rev-parse.md` 与 `COMPATIBILITY.md`（`| rev-parse |` 第 217、277 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`COMPATIBILITY.md` 第 277 行（`| rev-parse |` 伪引用集合）必须更新为含 `MERGE_AUTOSTASH`；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=MERGE_AUTOSTASH 伪引用与 rev-parse; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=MP-45,DEP-MP-10; writeset=序列化于MP-45; release=independent; split-from=MP-10; exception=N/A`

---

### Task MP-44: `stash show` 接受 `MERGE_AUTOSTASH`（#991）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`stash show [-p] MERGE_AUTOSTASH` 读取 merge-autostash sidecar；无 held autostash 时 LBR-CLI-003/129；普通 stash 解析不变；stash 58/58。C 组 patch +1。偏差：用户已取消独立 Claude/Codex 审计与全量测试。）

**Description:** 自 `MP-30` 拆出：`libra stash show [-p] MERGE_AUTOSTASH` 在待决合并持有 autostash 时显示该 autostash（经 `MP-30` 的 `MergeAutostash` 伪引用读取 sidecar），满足 `#991` AC1 与 `t7600` 对 `git stash show -p MERGE_AUTOSTASH` 的比较；没有持有的 autostash 时以 `LBR-CLI-003` 失败。普通 `stash@{n}` 解析不变。

**Out of scope:** `rev-parse MERGE_AUTOSTASH`（由 `MP-30` 承接）；`stash apply/pop/drop MERGE_AUTOSTASH`（Git 亦不支持对其 `drop`，尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `run_show` 经 `resolve_stash_to_commit_hash` 只解析普通 stash 索引 | `src/command/stash.rs:1191-1210,2705-2730` |
| 上游 `t7600` 以 `git stash show -p MERGE_AUTOSTASH` 的输出与合并前 diff 比较 | `git@6de20f6092` `t/t7600-merge.sh:844-847`；`#991` AC1 |

**Acceptance criteria:**

- [ ] AC1：待决 `--autostash` 合并期间 `libra stash show -p MERGE_AUTOSTASH` 显示该 autostash。
- [ ] AC2：没有持有的 autostash 时以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::stash_show_merge_autostash` 承载 AC1–AC2；夹具 F-991（`#991` 形态）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`file`=`1\n2\n3\n4\n5\n6\n7\n8\n9\n`）；`topic` 自 `B` 把第 5 行改为 `five` 提交 `T`；`main` 自 `B` 提交 `M`（新增 `h`=`m1\n`），二者无冲突；调用前把工作树 `file` 第 9 行改为 `nine`（与合并改动不重叠的同文件修改），记合并前 `libra diff` 的 stdout 为 `D0`；`R15` = 第 5 行为 `five` 的内容，`R159` = 第 5 行为 `five` 且第 9 行为 `nine` 的内容；应用冲突变体 F-991X：`topic` 另把第 9 行改为 `NINE`；快进变体 F-991F：`main` 停在 `B`（`topic` 可快进）；冲突变体 F-991C：`main` 自 `B` 提交的 `M` 改为只把第 5 行改为 `FIVE`（不新增 `h`，与 `topic` 冲突）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：`libra merge --no-commit --autostash topic` 后执行，退出 0；stdout 等于 `D0`（`t7600` 同形比较）。
- AC2：F-991 未合并前执行 `libra stash show MERGE_AUTOSTASH`；以非 0 退出，stderr 含 `Error-Code: LBR-CLI-003`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [x] 卡特有用例：`stash_show_merge_autostash`（通过）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::stash_test::/)'`（`command::stash_test` 模块族，守护 `stash show` 既有行为；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-30`, `DEP-MP-10`

**Implementation write set:** `src/command/stash.rs`, `tests/command/merge_test.rs`, `docs/commands/stash.md`, `docs/commands/zh-CN/stash.md`, `docs/development/commands/stash.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/stash.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/stash.md`、`docs/commands/zh-CN/stash.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md`（由 `--only stash` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`MERGE_AUTOSTASH`。逐文件语义核对：`docs/development/commands/stash.md` 与 `COMPATIBILITY.md`（`| stash |` 第 193 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=stash show 解析 MERGE_AUTOSTASH; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-30,DEP-MP-10; writeset=序列化于MP-30; release=independent; split-from=MP-30; exception=N/A`

---

### Task MP-11: 脏工作区仅在路径重叠时拒绝合并，`--abort` 保留无关修改（#993）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 把合并前对**未暂存工作树修改**的检查从「任何未提交修改即拒绝」改为 Git 语义：只有这些修改的路径与本次合并要写入的路径重叠时才拒绝，并在诊断中列出这些路径；无关修改在干净合并与 `merge --abort` 后原样保留。（按 G-09 自本卡拆出 `MP-53` 承接已暂存修改的拒绝与冲突合并的中止。）

**Out of scope:** 已暂存修改仍拒绝与冲突合并的 `--abort`（由 `MP-53` 承接）；退出码（由 `MP-34` 承接）；`--autostash`（由 `MP-10` 承接）；`--continue` 对未暂存修改的既有检查（`ensure_no_unstaged_changes_for_continue`）不在 `#993` 范围内，保持现状。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 合并前对整个工作树执行 `ensure_clean_status`，任何修改都报 `uncommitted changes, cannot merge` | `src/command/merge.rs:1985-1991`；`src/command/merge/workdir.rs` |
| Git 2.54.0 预探（`#993` 复现）：`git merge --no-commit clean_branch` 退出 0，`git status --short` 为 `M  bar` 与 ` M foo`，`git merge --abort` 后 `git diff` 与合并前相同 | `git@6de20f6092` `t/t7611-merge-abort.sh#12` |
| 既有用例依赖「任何未提交修改都拒绝」与旧文案 `uncommitted changes`：`command::merge_test::test_merge_dirty_worktree_refuses_before_state`（与合并无关的未暂存 `tracked.txt` 使 `merge feature` 被拒绝）、`command::merge_test::merge_gitlink_cleanliness_gate_ignores_a_submodule_but_not_a_plain_file`（gitlink 路径上的普通文件）与 `command::merge_test::merge_preserves_symlinks_and_executable_bits_when_it_writes`（`tool.sh` 的模式改动）。本卡按「验证执行协议」第 5 条逐个判定：被测路径不在本次合并的写入集合中的，改为断言合并成功且该修改原样保留（Git 行为）；仍与写入路径重叠而被拒绝的，只把文案断言改为 `Your local changes to the following files would be overwritten by merge:`，其余断言（稳定码、不建待决状态、文件内容保留）不变；`test_merge_continue_refuses_unstaged_resolution_edits` 属 `--continue` 的既有检查（Out of scope），不改。判定结果写入本卡 `Current evidence`，开工时以 `rg -n 'uncommitted changes' tests/` 复核，受影响文件按 ER-03 补入写集 | `tests/command/merge_test.rs:2134-2210,5425-5465,9252-9310`；`rg -n 'uncommitted changes' tests/command/merge_test.rs`（2026-11-19） |

**Acceptance criteria:**

- [ ] AC1：无关修改场景 `libra merge --no-commit clean_branch` 成功。
- [ ] AC2：AC1 之后索引中的 `bar` 为合并结果。
- [ ] AC3：AC1 之后工作树 `foo` 保留无关修改。
- [ ] AC4：AC1 之后记录待决合并。
- [ ] AC5：AC1 之后 `libra merge --abort` 恢复合并前的完整状态（含无关修改）。
- [ ] AC6：重叠修改场景以列出路径的诊断拒绝。
- [ ] AC7：AC6 的拒绝不改变仓库快照。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::dirty_worktree_overlap_only` 承载 AC1–AC7；夹具 F-993（`#993` 复现）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：提交 `initial`（`foo`=`foo\n`、`bar`=`bar\n`、`baz`=`baz\n`）与 `second`（`baz`=`bazz\n`）；`clean_branch` 自 `initial` 提交 `clean`（`bar`=`bart\n`）；`conflict_branch` 自 `initial` 提交 `conflict`（`bar`=`barf\n`、`baz`=`bazf\n`）；`main` 指向 `second`；无关修改场景 `printf 'foo\nxyzzy\n' > foo`；重叠修改场景 `echo local > bar`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：退出 0（Git 2.54.0 预探）。
- AC2：`libra show :bar` 的 stdout 恰为 `bart\n`（Git 2.54.0 预探：`M  bar`）。
- AC3：工作树 `foo` 内容恰为 `foo\nxyzzy\n`（Git 2.54.0 预探：` M foo`）。
- AC4：`.libra/merge-state.json` 存在。
- AC5：`libra merge --abort` 退出 0；之后的「仓库快照 Σ」与 AC1 调用前的 Σ 逐项相等（`foo` 的未提交修改保留；Git 2.54.0 预探：`git diff` 与合并前相同）。
- AC6：重建夹具，构造重叠修改场景（`echo local > bar`，不做无关修改）后 `libra merge --no-commit clean_branch` 以非 0 退出；stderr 含子串 `Your local changes to the following files would be overwritten by merge:\n\tbar\n`（Git 2.54.0 预探：该提示行后紧跟列出路径的行）。
- AC7：AC6 调用前后的「仓库快照 Σ」逐项相等。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::dirty_worktree_overlap_only)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::dirty_worktree_overlap_only)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_noninteractive_history_controls`（整 target，非交互历史控制；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_noninteractive_history_controls` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-44`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/workdir.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/workdir.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`would be overwritten by merge`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Performance budget:** 只比较本次合并要写入/删除的路径集合与工作树改动集合的交集（`O(合并影响路径 + 改动路径)`），不新增全仓内容哈希扫描（GC-10）

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=脏工作区重叠路径判定; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-44,DEP-MP-10; writeset=序列化于MP-44; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-53: 脏状态边界：已暂存修改仍拒绝、冲突合并的 `--abort` 保留无关修改（#993）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-11` 拆出：`MP-11` 只放宽未暂存的工作树修改；索引中有已暂存修改（即使与合并路径无关）时仍按 Git 拒绝合并、列出该路径并保持仓库状态不变（`#993` Expected 3，`t7611` 的 `Clean merge with dirty index fails`）；无关未暂存修改下发生内容冲突时，合并停在冲突并保留该修改，`libra merge --abort` 恢复合并前的完整状态（`#993` AC4，`t7611` 的 `Abort conflicting merge with non-conflicting dirty worktree`）。

**Out of scope:** 未暂存修改的重叠判定（由 `MP-11` 承接）；退出码（由 `MP-34` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#993` Expected 3：已暂存修改保持 Git 的严格拒绝；AC4：`t7611#12` 通过且 `#13`（冲突分支）复核 | `#993`；`git@6de20f6092` `t/t7611-merge-abort.sh:105-169` |
| Git 2.54.0 预探（F-993）：已暂存 `foo` 后 `git merge clean_branch` 的 stderr 为 `error: Your local changes to the following files would be overwritten by merge:`、`  foo`、`Merge with strategy ort failed.`，退出 2；未暂存 `foo` 后 `git merge conflict_branch` 停在 `baz` 的三阶段冲突，`foo` 保留，`git merge --abort` 后只剩 ` M foo` | `git@6de20f6092` `merge-ort.c`（合并前索引检查）；`builtin/merge.c` |

**Acceptance criteria:**

- [ ] AC1：已暂存的无关修改使 `libra merge clean_branch` 以 Git 诊断拒绝。
- [ ] AC2：AC1 的拒绝不改变仓库快照。
- [ ] AC3：无关未暂存修改下 `libra merge conflict_branch` 停在 `baz` 的内容冲突。
- [ ] AC4：AC3 之后无关修改保留在工作树。
- [ ] AC5：AC3 之后 `libra merge --abort` 恢复合并前的完整状态。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::dirty_staged_refusal_and_conflict_abort` 承载 AC1–AC5；夹具 F-993（`#993` 复现）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：提交 `initial`（`foo`=`foo\n`、`bar`=`bar\n`、`baz`=`baz\n`）与 `second`（`baz`=`bazz\n`）；`clean_branch` 自 `initial` 提交 `clean`（`bar`=`bart\n`）；`conflict_branch` 自 `initial` 提交 `conflict`（`bar`=`barf\n`、`baz`=`bazf\n`）；`main` 指向 `second`；无关修改场景 `printf 'foo\nxyzzy\n' > foo`；重叠修改场景 `echo local > bar`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：`printf 'foo\nxyzzy\n' > foo` 并 `libra add foo` 后执行；以非 0 退出；stderr 含子串 `Your local changes to the following files would be overwritten by merge:\n  foo\n`（Git 2.54.0 预探）。
- AC2：AC1 调用前后的「仓库快照 Σ」逐项相等（`t7611`：无待决合并、HEAD 不变、已暂存差异不变）。
- AC3：重建夹具，`printf 'foo\nxyzzy\n' > foo`（不暂存）后执行；以非 0 退出且 stderr 含 `Error-Code: LBR-CONFLICT-002`（执行条件）；`libra ls-files -u` 的 stdout 恰为 `100644 <baz> 1\tbaz\n100644 <bazz> 2\tbaz\n100644 <bazf> 3\tbaz\n`，三者依次为 `libra hash-object` 对 `baz\n`、`bazz\n`、`bazf\n` 的 OID（Git 2.54.0 预探）。
- AC4：工作树 `foo` 内容恰为 `foo\nxyzzy\n`（Git 2.54.0 预探）。
- AC5：`libra merge --abort` 退出 0（执行条件）；之后的「仓库快照 Σ」与 AC3 调用前的 Σ 逐项相等（Git 2.54.0 预探：中止后只剩 ` M foo`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::dirty_staged_refusal_and_conflict_abort)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::dirty_staged_refusal_and_conflict_abort)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_noninteractive_history_controls`（整 target，非交互历史控制；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_noninteractive_history_controls` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-11`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/workdir.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/workdir.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`would be overwritten by merge`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=已暂存修改拒绝与冲突中止的脏状态边界; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-11,DEP-MP-10; writeset=序列化于MP-11; release=independent; split-from=MP-11; exception=N/A`

---

### Task MP-12: 跳过结果与 HEAD 相同的工作区文件重写以保留 mtime（#1026）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 把合并结果写入工作树时，若某路径的结果 blob 与文件模式都与 HEAD 相同，跳过该文件的磁盘写入，保留其 mtime 与工作树内容（与合并无关的本地修改本就不在写入集合内；与写入集合重叠的本地修改由既有脏工作区检查在写入前拒绝）；结果确有变化的路径照常写入。

**Out of scope:** 合并内容计算与冲突落盘（Phase 4）；`add -u` 的 stat 刷新（`#660`，不在本计划范围）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 工作区写入对结果等于 HEAD 的路径仍重写文件 | `src/command/merge/workdir.rs`；`src/command/merge.rs:9395`（`reset_index_and_workdir_to_tree`，调用于 `1913,1942,2020`） |
| Git 2.54.0 / `#1026`：`git merge -s recursive B^0` 后 `b` 的 mtime 不变 | `git@6de20f6092` `t/t6426-merge-skip-unneeded-updates.sh#1` |

**Acceptance criteria:**

- [ ] AC1：`libra merge -s recursive B` 后 `b` 的 mtime 不变。
- [ ] AC2：默认策略 `libra merge B` 后 `b` 的 mtime 不变。
- [ ] AC3：结果有变化的路径照常写入。
- [ ] AC4：AC1 之后 `libra status --porcelain` 不报告任何路径（跳过写入不留下状态差异）。
- [ ] AC5：与合并无关的本地修改文件不被重写。
- [ ] AC6：与合并无关的本地修改内容保留。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::merge_preserves_mtime_for_unchanged_paths` 承载 AC1–AC6；夹具 F-1026（`#1026` 复现）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`O` 中 `b`=`1..10`（每行一个数字）、`c`=`c0\n`；`A`（HEAD 侧）在 `b` 的 `5` 后插入 `5.5` 并在末尾追加 `10.5`；`B` 只在 `5` 后插入 `5.5` 并把 `c` 改为 `cB\n`；`O` 中另有 `d`=`d0\n` 两侧均不修改；合并前在 `A^0` 分离 HEAD 上用 `std::fs::File::set_modified` 把 `b` 的 mtime 设为 `2026-01-01T00:00:00Z`，记为 `T0`；脏文件场景另在合并前 `echo local > d`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：退出 0；`b` 的 `metadata().modified()` 等于 `T0`（`#1026` AC1）。
- AC2：重建夹具；退出 0；`b` 的 mtime 等于 `T0`。
- AC3：AC1 之后工作树 `c` 内容恰为 `cB\n`。
- AC4：AC1 之后 `libra status --porcelain` 的 stdout 为空。
- AC5：重建夹具，`echo local > d` 并把 `d` 的 mtime 设为 `T0`；`libra merge -s recursive B` 退出 0（执行条件）；`d` 的 mtime 等于 `T0`。
- AC6：AC5 之后 `d` 内容恰为 `local\n`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::merge_preserves_mtime_for_unchanged_paths)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::merge_preserves_mtime_for_unchanged_paths)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_worktree_blob_writes`（整 target，工作树 blob 写入；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_worktree_blob_writes` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-53`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/workdir.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/workdir.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`mtime`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Performance budget:** 跳过判定只比较结果条目与 HEAD 条目的 OID/模式（`O(合并影响路径)`），不读取未变更文件内容（GC-10）

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=跳过未变更文件的工作区写入; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-53,DEP-MP-10; writeset=序列化于MP-53; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-13: `pull --rebase` 与 `--ff`/`--no-ff`/`--ff-only` 的 Git 优先级（#998）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 解除 `PullArgs` 中 `--rebase` 与 `--ff`/`--no-ff`/`--ff-only` 的 clap 互斥，按 Git 优先级执行：`--rebase` 胜过 `--ff`/`--no-ff`；`--ff-only` 胜过 `--rebase`（任意顺序）；`pull.ff=only` 胜过 `pull.rebase`；`pull.rebase=true` 胜过命令行 `--ff`/`--no-ff`；`pull.rebase=false` 时由 ff 标志决定。（按 G-09 自本卡拆出 `MP-23`。）

**Out of scope:** `pull --rebase <repo> <refspec>` 与 `.` 上游解析（由 `MP-23` 承接）；全局 `-c` 选项（不在本计划范围）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ff_only`/`ff`/`no_ff` 均 `conflicts_with_all` 含 `rebase` | `src/command/pull.rs:70-80` |
| 既有用例 `command::pull_test::test_pull_ff_only_conflicts_with_rebase_at_parse_time` 断言 `pull --ff-only --rebase` 在解析期以 `LBR-CLI-002` 拒绝；本卡按「验证执行协议」第 5 条把该断言改为「解析通过」：该用例在未初始化的空 `tempdir()` 中运行，没有仓库、远端或分叉历史，解除 clap 互斥后它到达仓库前置检查，改为断言以 `LBR-REPO-001`（`not a libra repository`，Libra 0.30.207 实测 `pull --ff-only`/`pull --rebase` 在空目录中即如此）失败而不再是 `LBR-CLI-002`、stderr 不含 `cannot be used with`；非快进时的 `Not possible to fast-forward, aborting.` 由本卡 AC4/AC5 在分叉夹具上判定，不在该既有用例中断言，并改正 `test_pull_no_rebase_countermands_rebase_at_parse_time` 中「`--rebase` 与 `--no-ff` 冲突」的注释 | `tests/command/pull_test.rs:213-245` |
| `compat_pull_strategy_flags_surface` 锁定 pull 帮助与兼容矩阵中的 `--ff-only`/`--rebase` 暴露面（不断言解析期互斥） | `tests/compat/pull_strategy_flags_surface.rs:50-68,137` |
| Git 优先级表（`#998`，`t/t7601-merge-pull-config.sh#25-26`） | `git@6de20f6092` `builtin/pull.c` |

**Acceptance criteria:**

- [ ] AC1：`libra pull --rebase --no-ff` 执行变基。
- [ ] AC2：AC1 的变基保留本地提交内容。
- [ ] AC3：`libra pull --rebase --ff` 执行变基。
- [ ] AC4：`libra pull --ff-only --rebase` 只尝试快进并拒绝分叉历史。
- [ ] AC5：`libra pull --rebase --ff-only`（反序）同样拒绝。
- [ ] AC6：`pull.ff=only` 与 `pull.rebase=true` 同时配置时 `libra pull` 拒绝分叉历史。
- [ ] AC7：`pull.rebase=true` 时 `libra pull --no-ff` 执行变基。
- [ ] AC8：`pull.rebase=false` 时 `libra pull --no-ff` 执行合并。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::pull_test::pull_rebase_ff_precedence_matrix` 承载 AC1–AC8；pull 夹具（新用例写在 `tests/command/pull_test.rs`，复用该模块的 `create_remote_fixture`、`push_remote_commit`、`configure_pull_tracking`，`tests/command/pull_test.rs:87-147`）：`create_remote_fixture()` 以 `git` 建裸仓库 `remote.git` 与工作目录 `workdir` 并推送初始提交 `B`（`README.md`=`hello libra\n`），分支名 `<br>` 取其返回值；本地仓库 D = `init_repo_via_cli` + `configure_identity_via_cli`（身份 `Test User <test@example.com>`，pull 用例不依赖身份）+ `configure_pull_tracking(D, remote.git, <br>)` 后 `libra pull` 得到 `B`；`push_remote_commit(workdir, <br>, <path>, <content>, <msg>)` 推进上游，D 中本地提交带 `--no-gpg-sign`；F-PR：上游 `push_remote_commit` 提交 `U1`（新增 `u.txt`=`u\n`），D 本地提交 `D1`（新增 `d.txt`=`d1\n`），二者分叉；「变基结果」= D 中 `main` 的父提交（`libra cat-file -p main` 中按序的全部 `parent` 行；直接读分支而非 `HEAD`，使把结果留在分离 HEAD 上、`main` 未前移的实现失败）恰为 `[U1]`（D 的分支为 `libra init` 默认的 `main`，`src/command/init.rs:37`）；「合并结果」= `main` 的父提交（`libra cat-file -p main` 中按序的全部 `parent` 行；直接读分支而非 `HEAD`，使把结果留在分离 HEAD 上、`main` 未前移的实现失败）恰为 `[D1, U1]`；除映射中写明「沿用」者外，每个 AC 用一份重建的夹具；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：退出 0；变基结果成立（`#998`：`--rebase` 胜出）。
- AC2：沿用 AC1 已完成变基的同一仓库（`main` 已是 AC1 判定父提交为 `[U1]` 的变基后提交）；`libra show main:d.txt` 的 stdout 恰为 `d1\n`。
- AC3：重建夹具；退出 0；变基结果成立。
- AC4：重建夹具；以非 0 退出；stderr 含 `Not possible to fast-forward, aborting.`。
- AC5：重建夹具；以非 0 退出；stderr 含 `Not possible to fast-forward, aborting.`。
- AC6：重建夹具，D 中 `libra config pull.ff only` 与 `libra config pull.rebase true`；以非 0 退出；stderr 含 `Not possible to fast-forward, aborting.`。
- AC7：重建夹具，`libra config pull.rebase true`；退出 0；变基结果成立。
- AC8：重建夹具，`libra config pull.rebase false`；退出 0；合并结果成立。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::pull_test::pull_rebase_ff_precedence_matrix)'`（`(new)`，在 `tests/command/pull_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::pull_test::pull_rebase_ff_precedence_matrix)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_pull_strategy_flags_surface`（整 target，pull 策略标志表面；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_pull_strategy_flags_surface` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::pull_test::/)'`（`command::pull_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-12`, `DEP-MP-10`

**Implementation write set:** `src/command/pull.rs`, `tests/command/pull_test.rs`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/pull.rs`, `tests/command/pull_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`（由 `--only pull` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`--rebase --no-ff`。逐文件语义核对：`docs/development/commands/pull.md` 与 `COMPATIBILITY.md`（`| pull |` 第 225 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=pull --rebase ff 优先级矩阵; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-12,DEP-MP-10; writeset=序列化于MP-12; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-23: `pull --rebase <repo> <refspec>` 显式上游解析（含 `.` 与 ff 标志组合）（#998）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-13` 拆出：`libra pull --rebase <repo> <refspec>` 以本次 fetch 得到的 `<refspec>` 结果为变基上游（`<repo>` 为 `.` 时即本地分支），不再要求当前分支配置 `branch.<name>.remote`/`merge`，也不再把 `./side` 当作引用名解析；与 `MP-13` 的 ff 优先级在显式参数形式下同样成立（`#998` AC1、AC3 的 `. side` 形式）。

**Out of scope:** 无显式参数时的 ff 优先级（由 `MP-13` 承接）；tag 作为 refspec 源（`issues/997.md`，不在本计划范围）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--rebase` 路径把 `<repo>/<refspec>` 拼成引用名解析，`. side` 报 `failed to resolve upstream './side'` | `src/command/pull.rs:709-770`；`#998` |

**Acceptance criteria:**

- [ ] AC1：`libra pull --rebase . side` 把 `main` 变基到 `side` 之上。
- [ ] AC2：AC1 的变基保留本地提交内容。
- [ ] AC3：`libra pull --rebase origin side`（无跟踪配置）变基到远端 `side`。
- [ ] AC4：AC3 的变基保留本地提交内容。
- [ ] AC5：`libra pull --rebase --no-ff . side` 执行变基。
- [ ] AC6：`libra pull --rebase --ff . side` 执行变基。
- [ ] AC7：`libra pull --ff-only --rebase . side` 拒绝分叉历史。
- [ ] AC8：`libra pull --rebase --ff-only . side`（反序）拒绝分叉历史。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::pull_test::pull_rebase_explicit_repo_refspec` 承载 AC1–AC8；夹具 F-998（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：仓库 R 中提交 `c0`；`side` 自 `c0` 提交 `c1`（新增 `s`=`s\n`）；`main` 自 `c0` 提交 `c2`（新增 `m`=`m\n`）并检出 `main`，不配置 `branch.main.remote`/`merge`；AC1、AC3、AC5–AC8 各用一份重建的夹具，AC2 与 AC4 分别沿用 AC1 与 AC3 执行后的仓库；远端变体 F-998R：pull 夹具（新用例写在 `tests/command/pull_test.rs`，复用该模块的 `create_remote_fixture`、`push_remote_commit`、`configure_pull_tracking`，`tests/command/pull_test.rs:87-147`）：`create_remote_fixture()` 以 `git` 建裸仓库 `remote.git` 与工作目录 `workdir` 并推送初始提交 `B`（`README.md`=`hello libra\n`），分支名 `<br>` 取其返回值；本地仓库 D = `init_repo_via_cli` + `configure_identity_via_cli`（身份 `Test User <test@example.com>`，pull 用例不依赖身份）+ `configure_pull_tracking(D, remote.git, <br>)` 后 `libra pull` 得到 `B`；`push_remote_commit(workdir, <br>, <path>, <content>, <msg>)` 推进上游，D 中本地提交带 `--no-gpg-sign`，在 `workdir` 中以 `git` 自 `B` 建分支 `side`、提交 `S1`（新增 `s`）并推送到 `remote.git`，D 再本地提交 `D1`（新增 `d.txt`=`d1\n`）后以 `libra config unset branch.main.remote` 与 `libra config unset branch.main.merge` 删除跟踪配置（`configure_pull_tracking` 固定写入 `branch.main.*`，`tests/command/pull_test.rs:141-146`），并以 `libra config list` 的 stdout 不含 `branch.main.remote` 与 `branch.main.merge` 确认（执行条件）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：退出 0（执行条件）；`main` 的父提交（`libra cat-file -p main` 中按序的全部 `parent` 行；直接读分支而非 `HEAD`，使把结果留在分离 HEAD 上、`main` 未前移的实现失败）恰为 `[c1]`（`#998` AC2：3 个提交、0 个合并）。
- AC2：在 AC1 已完成变基的同一仓库中，`libra show main:m` 的 stdout 恰为 `m\n`（`main` 已是 AC1 判定的变基后提交）。
- AC3：F-998R；退出 0（执行条件）；`main` 的父提交（`libra cat-file -p main` 中按序的全部 `parent` 行；直接读分支而非 `HEAD`，使把结果留在分离 HEAD 上、`main` 未前移的实现失败）恰为 `[S1]`。
- AC4：在 AC3 已完成变基的同一仓库中，`libra show main:d.txt` 的 stdout 恰为 `d1\n`。
- AC5：退出 0（执行条件）；`main` 的父提交（`libra cat-file -p main` 中按序的全部 `parent` 行；直接读分支而非 `HEAD`，使把结果留在分离 HEAD 上、`main` 未前移的实现失败）恰为 `[c1]`（`#998` AC1）。
- AC6：退出 0（执行条件）；`main` 的父提交（`libra cat-file -p main` 中按序的全部 `parent` 行；直接读分支而非 `HEAD`，使把结果留在分离 HEAD 上、`main` 未前移的实现失败）恰为 `[c1]`（`#998` AC1）。
- AC7：以非 0 退出；stderr 含 `Not possible to fast-forward, aborting.`（`#998` AC3）。
- AC8：以非 0 退出；stderr 含 `Not possible to fast-forward, aborting.`（`#998` AC3）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::pull_test::pull_rebase_explicit_repo_refspec)'`（`(new)`，在 `tests/command/pull_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::pull_test::pull_rebase_explicit_repo_refspec)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::pull_test::/)'`（`command::pull_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-13`, `DEP-MP-10`

**Implementation write set:** `src/command/pull.rs`, `tests/command/pull_test.rs`, `docs/commands/pull.md`, `docs/commands/zh-CN/pull.md`, `docs/development/commands/pull.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/pull.rs`, `tests/command/pull_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md`（由 `--only pull` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`pull --rebase . side`。逐文件语义核对：`docs/development/commands/pull.md` 与 `COMPATIBILITY.md`（`| pull |` 第 225 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=pull --rebase 显式上游解析; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-13,DEP-MP-10; writeset=序列化于MP-13; release=independent; split-from=MP-13; exception=N/A`

---

### Task MP-55: 支持 `-s octopus` 显式选择（单/多目标）并沿用 `ADR-MP-05` 的待决状态表示（#1025）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-14` 拆出并前置：把 `octopus` 加入 `--strategy` 可选值——两个及以上目标时执行既有 octopus 引擎，单个目标时与 Git 一样：目标是 HEAD 的后代时先按快进处理（与策略无关，`--no-commit` 也快进，见 `MP-02`；`git@6de20f6092` `builtin/merge.c:1655-1692`）；非快进时只尝试平凡的索引内合并（`read_tree_trivial`，`git@6de20f6092` `builtin/merge.c:743-770,1699-1735`）——要创建提交且平凡合并能处理时完成普通双父合并；平凡合并无法处理（例如两侧把同一文件改成不同内容，需要文件级合并）或带 `--no-commit`（Git 此时不走平凡合并路径）时输出 `Merge with strategy octopus failed.` 并保持合并前状态（两侧把同一文件改成相同内容属平凡合并可处理的情形，不拒绝）（不像默认 `ort` 那样进入内容合并、冲突或待提交状态）；因此只有多目标的显式 octopus 会停在提交前，其待决状态仍以 `strategy=None` + 非空 `targets` 表示（`ADR-MP-05`），不向 `strategy` 写入新的枚举变体。多个 `-s`（含 `-s ort -s octopus`）的逐个尝试、回绕与退出 `2` 由随后的 `MP-14` 承接。

**Out of scope:** 多策略链与全部未处理（由 `MP-14` 承接）；单个内置策略未处理时的数值退出码（Git 为 `2`；`ADR-MP-01` 只覆盖其所列卡的路径，本卡不断言数值退出码）；外部策略（由 `MP-35`、`MP-15`、`MP-37` 承接）；`subtree`（`DEFER-MP-02`）；octopus 默认消息（由 `MP-49` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `MergeStrategy` 只有 `ort`/`recursive`/`resolve`/`ours`，`-s octopus` 被 clap 拒绝 | `src/command/merge.rs:142-150,326`；`#1025` |
| octopus 状态以 `strategy=None` + 非空 `targets` 判别 | `src/command/merge.rs:4359-4366`；`src/command/merge/state.rs:55-56,79-81` |
| Git 2.54.0 预探（F-OCT）：`git merge -s octopus b1 b2` 得三父提交；`git merge -s octopus b1` 走平凡合并并得双父提交；`M2` 与 `C` 都修改 `f` 时 `git merge -s octopus C` 输出 `Trying really trivial in-index merge...`/`Nope.` 与 `Merge with strategy octopus failed.`、退出 2、不留合并状态；`git merge --no-commit -s octopus b1` 同样输出 `Merge with strategy octopus failed.`、退出 2；`git merge --no-commit -s octopus b1 b2` 停在提交前且 `MERGE_HEAD` 含两个目标 | `git@6de20f6092` `builtin/merge.c`（`try_merge_strategy`、平凡合并路径） |

**Acceptance criteria:**

- [ ] AC1：`libra merge -s octopus b1 b2 -m oct` 创建三父合并（`#1025` AC1：`octopus` 是合法的 `--strategy` 取值）。
- [ ] AC2：单个目标的 `libra merge -s octopus b1 -m oct` 被接受并创建双父合并（`#1025` AC1 的字面形态）。
- [ ] AC3：显式 octopus 的待决状态采用 `ADR-MP-05` 的表示（不写新的 `strategy` 取值，以 `targets` 记录全部目标）。
- [ ] AC4：单目标 `-s octopus` 在两侧把同一文件改成不同内容（平凡合并无法处理、需要文件级合并）时按 octopus 拒绝，而不是退回默认策略进行内容合并。
- [ ] AC5：AC4 的拒绝不改变仓库快照。
- [ ] AC6：非快进的单目标 `libra merge --no-commit -s octopus b1`（在与 `b1` 分叉的 `M` 上）与 Git 一样以 octopus 拒绝（`--no-commit` 不走平凡合并路径，单目标 octopus 无法处理），因而单目标显式 octopus 不会产生待决状态。
- [ ] AC7：AC6 的拒绝不改变仓库快照。
- [ ] AC8：可快进的单目标 `libra merge --no-commit -s octopus b1` 与 Git 一样快进（快进判定先于策略，`--no-commit` 不阻止快进）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::octopus_explicit_strategy_selection` 承载 AC1–AC8；夹具 F-OCT（`t6424` 形态）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`a`=`a0\n`、`f`=`f0\n`），再提交 `M`（新增 `m`）；`b1` 自 `B` 新增 `x`；`b2` 自 `B` 新增 `y`；`C` 自 `B` 把 `f` 改为 `fC\n`；脏索引场景先 `echo st > a && libra add a`（已暂存、未提交）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：在 `M` 上执行，退出 0（执行条件）；HEAD 的父提交恰为 `[M, b1, b2]`（Git 2.54.0 预探：`Merge made by the 'octopus' strategy.`；把 `-s octopus` 当作未知策略的实现在 clap 层以 `LBR-CLI-002` 失败）。
- AC2：重建夹具，在 `M` 上执行；退出 0（执行条件）；HEAD 的父提交恰为 `[M, b1]`（Git 2.54.0 预探：`git merge -s octopus <单个目标>` 以 `Trying really trivial in-index merge...`、`Wonderful.`、`In-index merge` 完成并创建双父提交；拒绝单目标 `-s octopus` 的实现即失败）。
- AC3：重建夹具；`libra merge --no-commit -s octopus b1 b2` 退出 0（执行条件）；以 `serde_json` 读取 `.libra/merge-state.json`，取其 `strategy`（字段缺失按 `null`）与 `targets` 组成的对象等于 `{"strategy": null, "targets": ["<b1>", "<b2>"]}`（`<b1>`/`<b2>` 为完整 OID，按命令行顺序；一次 `serde_json::Value` 相等比较）。
- AC4：重建夹具，在 `M` 上先提交 `M2`（`f`=`fM\n`，与 `C` 的 `f`=`fC\n` 都修改 `f`），再执行 `libra merge -s octopus C -m oct`；以非 0 退出（执行条件）；stderr 含一行恰为 `Merge with strategy octopus failed.`（Git 2.54.0 预探：stdout `Trying really trivial in-index merge...`、`Nope.`，stderr `error: Merge requires file-level merging` 与该行，退出 2，不留合并状态；忽略 `-s octopus` 而按默认 `ort` 合并的实现会输出 `CONFLICT (content)` 并停在冲突，不输出该行）。
- AC5：AC4 调用前后的「仓库快照 Σ」逐项相等（Git 2.54.0 预探：无 `MERGE_HEAD`、工作树与索引不变；停在冲突的实现使 Σ(b)/(c)/(d) 变化）。
- AC6：重建夹具，在 `M` 上执行；以非 0 退出（执行条件）；stderr 含一行恰为 `Merge with strategy octopus failed.`（Git 2.54.0 预探：同一调用退出 2、不写 `MERGE_HEAD`——`git@6de20f6092` `builtin/merge.c:1699,1725` 只在 `option_commit` 为真时尝试平凡合并；把单目标 octopus 当作普通合并并停在待提交状态的实现即失败）。
- AC7：AC6 调用前后的「仓库快照 Σ」逐项相等（Git 2.54.0 预探：无 `MERGE_HEAD`、索引与工作树不变；留下待决状态或已暂存结果的实现使 Σ(b)/(c)/(d) 变化）。
- AC8：重建夹具，以 `libra switch -c atb B` 切到指向 `B`（`b1` 的祖先）的分支后执行；退出 0（执行条件）；`libra rev-parse HEAD` 等于 `b1` 的完整 OID（Git 2.54.0 预探：同一调用输出 `Fast-forward`、HEAD 前移到 `b1`、不写 `MERGE_HEAD`；把单目标 `--no-commit -s octopus` 一律拒绝的实现即失败）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::octopus_explicit_strategy_selection)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::octopus_explicit_strategy_selection)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-23`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`-s octopus`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=octopus 显式选择; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-23,DEP-MP-10; writeset=序列化于MP-23; release=independent; split-from=MP-14; exception=N/A`

---

### Task MP-14: 多策略链逐个尝试、回绕并在全部未处理时以 `2` 退出（#1025）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 多个 `-s` 时按顺序逐个尝试：除第一个外每次尝试前在 stdout 输出 `Rewinding the tree to pristine...` 与 `Trying merge strategy <name>...`，某策略因本地改动等原因未处理时继续下一个；全部未处理时恢复合并前索引与工作树、stderr 输出 `No merge strategy handled the merge.` 并以 `with_exit_code(2)` 退出（`ADR-MP-01`）。`-s octopus` 的显式选择由前置的 `MP-55` 交付。（按 G-09 自本卡拆出 `MP-55`。）

**Out of scope:** `-s octopus` 的显式选择与单/多目标结果（由 `MP-55` 承接）；外部策略（由 `MP-35`、`MP-15`、`MP-37` 承接）；`subtree`（`DEFER-MP-02`）；全部策略均冲突时选择最佳策略的既有行为（`Using the <s> strategy to prepare resolving by hand.`，`src/command/merge.rs:1455-1462`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 多策略链遇到首个策略的拒绝即返回错误，不尝试后续策略 | `src/command/merge.rs:1417-1446`；`#1025` |
| Git 2.54.0 预探（F-OCT 脏索引）：`git merge -s ort -s octopus C` 的 stdout 为三行 `Trying merge strategy ort...`、`Rewinding the tree to pristine...`、`Trying merge strategy octopus...`，stderr 含 `No merge strategy handled the merge.`，退出 2，`a` 仍为已暂存修改 | `git@6de20f6092` `builtin/merge.c:1786-1849` |

**Acceptance criteria:**

- [ ] AC1：脏索引场景 `libra merge -s ort -s octopus C` 的 stdout 为 Git 的三行。
- [ ] AC2：AC1 的 stderr 报告无策略处理。
- [ ] AC3：AC1 以退出码 2 结束。
- [ ] AC4：AC1 之后仓库快照不变。
- [ ] AC5：脏索引场景 `libra merge -s recursive -s resolve C` 在首个策略未处理后输出第二个策略的尝试行（`#1025` AC3 的诊断；第二个策略确实运行并能完成合并由 AC6 判定）。
- [ ] AC6：首个策略未处理时，链中的下一个策略实际运行并完成合并：`libra merge -s ort -s ours b1 -m m` 在 `ort` 因未跟踪文件将被覆盖而未处理后由 `ours` 创建合并提交。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::strategy_chain_fallback` 承载 AC1–AC6；夹具 F-OCT（`t6424` 形态）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `B`（`a`=`a0\n`、`f`=`f0\n`），再提交 `M`（新增 `m`）；`b1` 自 `B` 新增 `x`；`b2` 自 `B` 新增 `y`；`C` 自 `B` 把 `f` 改为 `fC\n`；脏索引场景先 `echo st > a && libra add a`（已暂存、未提交）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：在 `M` 上执行 `echo st > a && libra add a` 构造脏索引场景后调用；stdout 恰为 `Trying merge strategy ort...\nRewinding the tree to pristine...\nTrying merge strategy octopus...\n`（Git 2.54.0 预探）。
- AC2：stderr 含一行恰为 `No merge strategy handled the merge.`（Git 2.54.0 预探）。
- AC3：`LIBRA_FINE_EXIT_CODES` 未设置；退出码恰为 2（Git 2.54.0 预探）。
- AC4：AC1 调用前后的「仓库快照 Σ」逐项相等（已暂存的 `a` 保留）。
- AC5：重建夹具并构造脏索引场景；stdout 含 `Trying merge strategy resolve...`（Git 2.54.0 预探；`#1025` 第二例）。
- AC6：重建夹具，在 `M` 上写未跟踪文件 `x`=`u\n`（`b1` 新增的 `x` 会覆盖它）；该调用退出 0（执行条件；Git 2.54.0 预探：stdout 依次为 `Trying merge strategy ort...`、`Rewinding the tree to pristine...`、`Trying merge strategy ours...`、`Merge made by the 'ours' strategy.`，`ort` 在 stderr 报告 `x` 将被覆盖）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree <Mt>`、`parent <M>`、`parent <b1>`（`<Mt>` 为 `libra rev-parse 'M^{tree}'`，`<M>`/`<b1>` 为完整 OID；只打印尝试行而未运行 `ours` 的实现不会创建该提交，在首个未处理策略处停止的实现以非 0 退出）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::strategy_chain_fallback)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::strategy_chain_fallback)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改 docs/error-codes.md 退出码契约（ER-13 T-1）` | **Dependencies:** `MP-55`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/output.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/output.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`No merge strategy handled the merge`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md` 必须更新：在「Per-command exit-code overrides」小节登记本卡的退出码覆写（稳定码不变）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=多策略链回退与未处理退出码; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-55,DEP-MP-10; writeset=序列化于MP-55; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-35: 外部策略名白名单（拒绝且不查找、不执行）（#1027）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-15` 拆出并前置：`--strategy` 改为接受任意字符串，内置名（`ort`/`recursive`/`resolve`/`ours`/`octopus`）照常；非内置名先校验白名单 `[A-Za-z0-9_.-]+` 且不等于 `.`/`..`，不满足（含 `/`、`\`、空串、其它任何字符）一律以 `LBR-CLI-002` 拒绝，且在任何 `PATH` 查找或进程创建之前拒绝；通过校验的非内置名在本卡版本中以 `LBR-UNSUPPORTED-001` 拒绝且不做查找（由 `MP-38` 接入查找）。（按 G-09 自本卡拆出 `MP-38`。）

**Out of scope:** 合法名的 `PATH` 查找与未找到诊断（由 `MP-38` 承接）；外部策略执行（由 `MP-15`、`MP-37` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--strategy` 为封闭 `value_enum`，未知名由 clap 以 `invalid value` + `LBR-CLI-002` 拒绝 | `src/command/merge.rs:142-150,326-327`；`#1027` |
| Git 只在 `PATH` 目录列表中按 `git-merge-` 前缀匹配文件名，含路径分隔符的名字不可能匹配 | `git@6de20f6092` `builtin/merge.c` `get_strategy`（`load_command_list("git-merge-", ...)`） |
| 既有用例 `command::merge_test::merge_strategy_unknown_name_lists_supported_values` 断言 `libra merge -s mystery feature` 被 clap 以退出 129、`LBR-CLI-002` 拒绝且 stderr 列出四个内置策略名、HEAD 不变；`mystery` 通过本卡白名单，本卡按「验证执行协议」第 5 条只把其退出码/稳定码/列表断言改为本卡版本的行为（以非 0 退出、stderr 含 `Error-Code: LBR-UNSUPPORTED-001`），HEAD 不变的断言保留（`MP-38` 再按第 5 条把它改为未找到诊断） | `tests/command/merge_test.rs:4002-4016` |

**Acceptance criteria:**

- [ ] AC1：嵌入白名单外的 30 个 ASCII 可打印字符、制表符或非 ASCII 代表字符 `é` 的名字都被拒绝。
- [ ] AC2：策略名 `.` 被拒绝。
- [ ] AC3：策略名 `..` 被拒绝。
- [ ] AC4：空策略名被拒绝。
- [ ] AC5：AC1–AC4 的被拒绝名字都不执行任何程序。
- [ ] AC6：覆盖全部白名单字符类的合法名通过校验。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::strategy_name_whitelist` 承载 AC1–AC6；夹具 F-EXT（`t7606` 形态，用例 `#[cfg(unix)]`）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`c0`（`c0.c`）→ `main` 上 `c1`（`c1.c`）；`c2` 自 `c0` 新增 `c2.c`；临时目录 `<bin>`（工作树外）用于放置可执行脚本，每个脚本以 `#!/bin/sh` 与 `set -e` 开头（任一命令失败即以该命令的非 0 码退出），先把参数逐行写入 `<bin>/args`、把 `pwd` 写入 `<bin>/cwd` 并创建 `<bin>/ran`；`git-merge-theirs` 随后以 `for last; do :; done` 取得最后一个参数（即目标 OID；带 `-X` 时 `$4` 是字面量 `HEAD`），执行 `<libra 绝对路径> read-tree "$last"`、写入 `ext.txt`=`ext\n` 并 `<libra 绝对路径> add ext.txt`，再把 `<libra 绝对路径> write-tree` 的 stdout 写入 `<bin>/tree`，然后 `exit 0`（暂存的树与两个父提交的树都不同）；所有调用以子进程环境 `PATH=<bin>:<原 PATH>`；另为 AC1–AC4 的每个被拒绝名字 `<n>` 准备可被误执行的同名程序（运行即创建 `<bin>/ran`）：`/` 之外的名字在 `<bin>`（位于子进程 `PATH` 上）放置可执行脚本 `git-merge-<n>`（含 `git-merge-a\b`、`git-merge-a;b`、`git-merge-a<制表符>b`、`git-merge-aéb`、`git-merge-.`、`git-merge-..`、`git-merge-`）；含 `/` 的 `a/b` 在工作树根建目录 `git-merge-a` 并放置可执行脚本 `git-merge-a/b`，使其可被误作相对路径执行；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：表驱动：对 ASCII 可打印字符中除 `A-Z`、`a-z`、`0-9`、`_`、`.`、`-` 外的每个字符 `c`（共 30 个，含空格、`/`、`\`、`;`），以及制表符与非 ASCII 字符 `é`，分别执行 `libra merge -s a<c>b c2`（cwd 为工作树根）；每次都以非 0 退出且 stderr 含 `Error-Code: LBR-CLI-002`。
- AC2：`libra merge -s . c2` 以非 0 退出；stderr 含 `Error-Code: LBR-CLI-002`。
- AC3：`libra merge -s .. c2` 以非 0 退出；stderr 含 `Error-Code: LBR-CLI-002`。
- AC4：`libra merge -s '' c2` 以非 0 退出；stderr 含 `Error-Code: LBR-CLI-002`。
- AC5：AC1–AC4 的全部调用之后 `<bin>/ran` 不存在（每个被拒绝名字的同名程序都在 `PATH` 上或作为相对路径 `git-merge-a/b` 可达，任一被执行即创建该文件）。
- AC6：`libra merge -s Az09_.-x c2` 以非 0 退出且 stderr 含 `Error-Code: LBR-UNSUPPORTED-001`（通过白名单、本卡版本不查找；`MP-38` 按「验证执行协议」第 5 条把本条改为未找到诊断）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::strategy_name_whitelist)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::strategy_name_whitelist)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-14`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`git-merge-`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Security and privacy:** 策略名白名单 `[A-Za-z0-9_.-]+` 且不等于 `.`/`..`，在任何 `PATH` 查找或进程创建前校验：拒绝以名字携带路径分隔符执行任意程序；`PATH` 中的相对目录（含 `.`）仍按 Git 语义参与查找（`MP-15`）（GC-07）

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=策略名白名单校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-14,DEP-MP-10; writeset=序列化于MP-14; release=independent; split-from=MP-15; exception=N/A`

---

### Task MP-38: `Could not find merge strategy` 诊断与外部策略执行前的过渡拒绝（#1027）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-35` 拆出：通过白名单的非内置策略名（多个 `-s` 时逐个）在解析合并目标与尝试任何策略之前按 `PATH` 查找 `git-merge-<name>`：找不到时输出 Git 的 `Could not find merge strategy '<name>'.` 并以 `with_exit_code(1)` 退出（稳定码 `LBR-CLI-002`；按「验证执行协议」第 5 条把 `MP-35` 用例 AC6 的 `LBR-UNSUPPORTED-001` 断言改为本诊断）；找到时本卡版本仍以 `LBR-UNSUPPORTED-001` 拒绝且不执行（执行由 `MP-15` 开放并取代该行为）。

**Out of scope:** 名字白名单（由 `MP-35` 承接）；外部策略执行（由 `MP-15`、`MP-37` 承接）；`Available strategies are:` 列表行（Libra 不枚举 `PATH` 上的自定义策略，记为有意差异写入 `COMPATIBILITY.md`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git：未知策略输出 `Could not find merge strategy '%s'.` 与可用策略列表并 `exit(1)`（Git 2.54.0 预探：退出 1） | `git@6de20f6092` `builtin/merge.c` `get_strategy` |
| 合并错误默认以 128/129 退出，`with_exit_code` 可覆写 | `src/utils/error.rs:1061-1076` |
| `MP-35` 改写后的既有用例 `command::merge_test::merge_strategy_unknown_name_lists_supported_values` 断言 `-s mystery` 以 `LBR-UNSUPPORTED-001` 拒绝；`PATH` 上没有 `git-merge-mystery`，本卡按「验证执行协议」第 5 条把该断言改为未找到诊断（stderr 含 `Could not find merge strategy 'mystery'.` 与 `Error-Code: LBR-CLI-002`），HEAD 不变的断言保留；Libra 不列出可用策略（有意差异，见 Out of scope），该用例不再断言内置策略名列表 | `tests/command/merge_test.rs:4002-4016` |

**Acceptance criteria:**

- [ ] AC1：`libra merge -s nosuch c2`（`PATH` 上无该脚本）输出 Git 诊断。
- [ ] AC2：AC1 以退出码 1 结束。
- [ ] AC3：AC1 的稳定码为 `LBR-CLI-002`。
- [ ] AC4：本卡版本中 `PATH` 上找到的合法外部策略被拒绝。
- [ ] AC5：AC4 不执行该策略。
- [ ] AC6：多个 `-s` 中未找到的名字以 Git 诊断报告。
- [ ] AC7：未找到诊断先于合并目标解析（因而先于任何策略尝试）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::strategy_not_found_and_pending_external` 承载 AC1–AC7；夹具 F-EXT（`t7606` 形态，用例 `#[cfg(unix)]`）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`c0`（`c0.c`）→ `main` 上 `c1`（`c1.c`）；`c2` 自 `c0` 新增 `c2.c`；临时目录 `<bin>`（工作树外）用于放置可执行脚本，每个脚本以 `#!/bin/sh` 与 `set -e` 开头（任一命令失败即以该命令的非 0 码退出），先把参数逐行写入 `<bin>/args`、把 `pwd` 写入 `<bin>/cwd` 并创建 `<bin>/ran`；`git-merge-theirs` 随后以 `for last; do :; done` 取得最后一个参数（即目标 OID；带 `-X` 时 `$4` 是字面量 `HEAD`），执行 `<libra 绝对路径> read-tree "$last"`、写入 `ext.txt`=`ext\n` 并 `<libra 绝对路径> add ext.txt`，再把 `<libra 绝对路径> write-tree` 的 stdout 写入 `<bin>/tree`，然后 `exit 0`（暂存的树与两个父提交的树都不同）；所有调用以子进程环境 `PATH=<bin>:<原 PATH>`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出；stderr 含 `Could not find merge strategy 'nosuch'.`（Git 2.54.0 预探）。
- AC2：`LIBRA_FINE_EXIT_CODES` 未设置；退出码恰为 1（Git 2.54.0 预探）。
- AC3：AC1 调用的 stderr 含 `Error-Code: LBR-CLI-002`。
- AC4：`libra merge -s theirs c2` 以非 0 退出，stderr 含 `Error-Code: LBR-UNSUPPORTED-001`（`MP-15` 按「验证执行协议」第 5 条取代本条断言）。
- AC5：AC4 之后 `<bin>/ran` 不存在（`MP-15` 同样取代本条）。
- AC6：重建夹具；`libra merge -s ours -s nosuch c2` 以非 0 退出，stderr 含 `Could not find merge strategy 'nosuch'.`（Git 2.54.0 预探：退出 1、HEAD 不变；Git 在选项解析期查找）。
- AC7：重建夹具；`libra merge -s ours -s nosuch no-such-ref` 以非 0 退出，stderr 含 `Could not find merge strategy 'nosuch'.`（Git 2.54.0 预探：同一调用输出该行并退出 1，而非 `not something we can merge`；任何策略尝试都要先解析合并目标）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::strategy_not_found_and_pending_external)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::strategy_not_found_and_pending_external)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改 docs/error-codes.md 退出码契约（ER-13 T-1）` | **Dependencies:** `MP-35`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Could not find merge strategy`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`COMPATIBILITY.md` `| merge |` 行必须更新：记录未知策略诊断不列出可用策略的有意差异；`docs/error-codes.md` 必须更新：在「Per-command exit-code overrides」小节登记本卡的退出码覆写（稳定码不变）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=合法外部策略名的查找结果诊断; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-35,DEP-MP-10; writeset=序列化于MP-35; release=independent; split-from=MP-35; exception=N/A`

---

### Task MP-15: 执行外部合并策略 `git-merge-<name>`（调用契约与安全失败）（#1027）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 `ADR-MP-02`，通过 `MP-35` 白名单且在 `PATH`（含 `.`）上找到的 `git-merge-<name>` 以 `Command::new` 直接执行（无 shell），argv 与 Git `try_merge_command` 相同（`[--<xopt>...] <base>... -- HEAD <remote>...`，`-X` 选项以其 Git 拼写加 `--` 前缀，OID 为完整十六进制），工作目录为工作树根，`Command::status()` 同步等待——等待期间不持有索引或仓库操作锁，使策略能调用 `libra read-tree`/`libra add` 等命令；人读模式继承 stdout/stderr；结构化输出模式（`--json`，`--machine` 隐含 `--json=ndjson`，`src/utils/output.rs:170-171`）下本卡版本以 `LBR-UNSUPPORTED-001` 拒绝外部策略（输出隔离由 `MP-51` 开放并取代）。返回后重读索引：退出 0 以索引写树并创建合并提交；本卡版本中任何非 0 退出都恢复合并前索引与工作树、输出 `Merge with strategy <name> failed.` 并失败（退出 1 的冲突状态与 Git 退出码由 `MP-37` 细化）。取代 `MP-38` 对单个外部策略的 `LBR-UNSUPPORTED-001` 过渡行为（按「验证执行协议」第 5 条只改写 `MP-38` 用例中被取代的 AC4/AC5 断言）；多个 `-s` 中含外部策略时本卡不执行任何策略，沿用该过渡拒绝（`MP-42` 以专门诊断固化，`DEFER-MP-03`）。Libra 仓库没有 `.git`，策略脚本须以 `libra` 命令操作索引：`#1027` 复现与 `t7606` 中的 `git read-tree --reset -u` 在 Libra 仓库不可用（`libra read-tree` 也不提供 `--reset`/`-u`，`src/command/read_tree.rs:8-10`），本卡以只更新索引的 `libra read-tree` 脚本验收 `#1027` AC1 的结果树与父提交；`t7606#3` 原样脚本所需的 `read-tree --reset -u`（同时更新工作树）及其工作树断言按 `DEFER-MP-04` 延后，该差异写入文档与 `COMPATIBILITY.md`。（按 G-09 自本卡拆出 `MP-35`、`MP-37`、`MP-51`。）

**Out of scope:** 策略名校验与未找到诊断（由 `MP-35`、`MP-38` 承接）；结构化输出模式的输出隔离（由 `MP-51` 承接）；退出 1 的冲突状态、`external_strategy` 持久化与 Git 退出码 1/2（由 `MP-37` 承接）；多个 `-s` 中含外部策略的拒绝（由 `MP-42` 承接，`DEFER-MP-03`）；`libra read-tree --reset -u` 与 `t7606#3` 原样脚本的工作树断言（`DEFER-MP-04`）；超时与进程组回收（`ADR-MP-02` 不采纳）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `MP-38` 发布后找到的外部策略以 `LBR-UNSUPPORTED-001` 拒绝（本卡取代） | `src/command/merge.rs:326-327` |
| Git `try_merge_command`：argv `merge-<s>`、`--<xopt>`、各 base、`--`、`HEAD`、各 remote；同步运行后重读索引（Git 2.54.0 预探：从子目录调用时脚本的 `pwd` 为工作树根，argv 为 `<base> -- HEAD <remote>`；未处理时 `Merge with strategy fail2 failed.`） | `git@6de20f6092` `merge.c` `try_merge_command`；`builtin/merge.c:1786-1849` |
| `t7606#3` 的策略脚本以 `eval git read-tree --reset -u \$$#` 读取最后一个参数，并断言结果树等于 `c2^{tree}`、父提交为 `[c1, c2]`；`#1027` AC1 同样要求结果树等于 `c2^{tree}` | `git@6de20f6092` `t/t7606-merge-custom.sh:20-62`；`#1027` |
| `libra read-tree` 只读入索引，不提供 `-u`/`--reset` | `src/command/read_tree.rs:1-10` |
| 既有外部驱动以 `.status()` 同步等待 | `src/command/merge.rs:5654-5657,5690-5694` |

**Acceptance criteria:**

- [ ] AC1：`libra merge -s theirs c2 -m m` 以外部策略暂存的索引创建提交。
- [ ] AC2：AC1 创建双父合并。
- [ ] AC3：脚本收到 Git 的 argv（含 `-X`）。
- [ ] AC4：脚本的工作目录为工作树根。
- [ ] AC5：`PATH=.:$PATH` 时从当前目录找到策略，并以其暂存的 `c2` 树完成为 `c1` 与 `c2` 的合并提交。
- [ ] AC6：本卡版本中策略退出 1 时恢复合并前索引与工作树。
- [ ] AC7：策略退出 2 时恢复合并前索引与工作树。
- [ ] AC8：本卡版本在结构化输出模式下拒绝外部策略。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::external_strategy_execution_contract` 承载 AC1–AC8；夹具 F-EXT（`t7606` 形态，用例 `#[cfg(unix)]`）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`c0`（`c0.c`）→ `main` 上 `c1`（`c1.c`）；`c2` 自 `c0` 新增 `c2.c`；临时目录 `<bin>`（工作树外）用于放置可执行脚本，每个脚本以 `#!/bin/sh` 与 `set -e` 开头（任一命令失败即以该命令的非 0 码退出），先把参数逐行写入 `<bin>/args`、把 `pwd` 写入 `<bin>/cwd` 并创建 `<bin>/ran`；`git-merge-theirs` 随后以 `for last; do :; done` 取得最后一个参数（即目标 OID；带 `-X` 时 `$4` 是字面量 `HEAD`），执行 `<libra 绝对路径> read-tree "$last"`、写入 `ext.txt`=`ext\n` 并 `<libra 绝对路径> add ext.txt`，再把 `<libra 绝对路径> write-tree` 的 stdout 写入 `<bin>/tree`，然后 `exit 0`（暂存的树与两个父提交的树都不同）；所有调用以子进程环境 `PATH=<bin>:<原 PATH>`；`git-merge-fail1` 与 `git-merge-fail2` 都执行 `<libra 绝对路径> read-tree "$4"`、把 `<libra 绝对路径> write-tree` 的 stdout 写入 `<bin>/tree-<name>`，再把已跟踪的 `c1.c` 改为 `broken\n`、删除已跟踪的 `c0.c`，然后分别 `exit 1` 与 `exit 2`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：退出 0（执行条件）；`libra rev-parse 'HEAD^{tree}'` 的 stdout 等于 `<bin>/tree` 的内容（脚本退出前暂存的完整树，含路径、模式与 blob；该树含 `ext.txt`，与 `c1`、`c2` 的树都不同）。
- AC2：HEAD 的父提交恰为 `[c1, c2]`（`#1027` AC1）。
- AC3：重建夹具；`libra merge -s theirs -X theirs c2 -m m` 退出 0（执行条件）；`<bin>/args` 内容恰为 `--theirs\n<base>\n--\nHEAD\n<c2>\n`（`<base>` 为 `libra merge-base c1 c2` 的完整 OID，`<c2>` 为 `c2` 的完整 OID；`git@6de20f6092` `merge.c` `try_merge_command`）。
- AC4：重建夹具并在工作树根 `mkdir sub`（未跟踪空目录），以 `sub` 为工作目录执行 `libra merge -s theirs c2 -m m`；`<bin>/cwd` 内容等于工作树根的规范路径加 `\n`（Git 2.54.0 预探）。
- AC5：重建夹具，在工作树根放置可执行脚本 `git-merge-theirs`（内容为 `#!/bin/sh` 与 `exec <libra 绝对路径> read-tree "$4"`：与 `t7606#3` 一样读取最后一个参数，但只更新索引——原样脚本的 `--reset -u` 见 `DEFER-MP-04`；`<bin>` 不在 `PATH` 中），子进程环境 `PATH=.:<原 PATH>`，cwd 为工作树根；`libra merge -s theirs c2 -m m` 退出 0（执行条件）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree <c2t>`、`parent <c1>`、`parent <c2>`（`<c2t>` 为 `libra rev-parse 'c2^{tree}'`，`<c1>`/`<c2>` 为完整 OID；`#1027` AC1：结果树等于 `c2^{tree}`、首父为原 HEAD、次父为 `c2`；`t7606#3` 的 `tree.expected` 与 `verify_parents` 比较）。
- AC6：重建夹具；`libra merge -s fail1 c2` 以非 0 退出，且 `<bin>/tree-fail1` 的内容等于 `libra rev-parse 'c2^{tree}'` 的 stdout（执行条件：脚本确已改写索引）；调用前后的「仓库快照 Σ」逐项相等（脚本的 `read-tree`、对 `c1.c` 的修改与对 `c0.c` 的删除都被撤销；`MP-37` 按「验证执行协议」第 5 条以冲突状态断言取代本条）。
- AC7：重建夹具；`libra merge -s fail2 c2` 以非 0 退出，且 `<bin>/tree-fail2` 的内容等于 `libra rev-parse 'c2^{tree}'` 的 stdout（执行条件：脚本确已改写索引）；调用前后的「仓库快照 Σ」逐项相等（脚本对索引、`c1.c` 与 `c0.c` 的改动都被撤销）。
- AC8：重建夹具；`libra --json merge -s theirs c2 -m m` 以非 0 退出（执行条件）；stderr 去掉首尾空白后可被 `serde_json::from_str` 解析为恰好一个 JSON 值，其 `error_code` 为 `"LBR-UNSUPPORTED-001"`（GC-MP-01：`--json` 下错误报告为 stderr 上的一个 JSON 文档，不含人读 `Error-Code:` 头，`src/utils/error.rs:1137-1160`；`MP-51` 按「验证执行协议」第 5 条取代本条）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::external_strategy_execution_contract)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::external_strategy_execution_contract)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-38`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`git-merge-`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`COMPATIBILITY.md` `| merge |` 行必须更新：记录外部策略在 Libra 仓库中须以 `libra` 命令操作索引（无 `.git`，`libra read-tree` 不提供 `--reset`/`-u`）的有意差异；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Security and privacy:** 只执行通过 `MP-35` 白名单的 `git-merge-<name>`（按标准 `PATH` 查找，相对目录与 `.` 照 Git 语义参与）；`Command::new` 直接执行、不经 shell；argv 由固定字面量（`--`、`HEAD`）、完整 OID 与用户经 `-X` 提供的策略选项组成——每个 `-X` 值作为一个独立的 argv 元素 `--<xopt>` 原样传递（与 Git `try_merge_command` 相同，`git@6de20f6092` `merge.c` 中 `strvec_pushf(&cmd.args, "--%s", xopts[i])`），不做 shell 展开或拆分；环境原样继承（与 Git 相同，不注入 Libra 机密）（GC-07）

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=外部策略执行与调用契约; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-38,DEP-MP-10; writeset=序列化于MP-38; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-51: 外部策略在 `--json`/`--machine` 下的输出隔离（#1027）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-15` 拆出：结构化输出模式（`--json`；`--machine` 隐含 `--json=ndjson`，`src/utils/output.rs:170-171`）下执行外部策略时，以 `Command::output()` 捕获子进程的 stdout 与 stderr（两路并发读取、不转发到 Libra 的任何输出流）：策略成功时把二者按 UTF-8 有损解码为文本（非 UTF-8 字节替换为 U+FFFD；这是公开的 JSON 契约，写入 merge 文档，不承诺字节级保留）后写入 merge JSON 数据（`PullMergeSummary`，`src/command/merge.rs:482`）新增的可选对象 `strategy_output`（`{"stdout": ..., "stderr": ...}`，`#[serde(skip_serializing_if = "Option::is_none")]`，非外部策略时省略）；策略失败（非 0 退出，按 `MP-15` 回滚）时以同形对象写入错误报告的 `details.strategy_output`（`CliError::with_detail`，`src/utils/error.rs:1039`）。由此成功时 stdout 只含 Libra 的 JSON，失败时 stdout 为空、stderr 只含一个错误 JSON 文档（GC-MP-01；`src/utils/error.rs:1137-1160`）。取代 `MP-15` 在该模式下的 `LBR-UNSUPPORTED-001` 过渡拒绝（按「验证执行协议」第 5 条只改写 `MP-15` 用例 AC8）。

**Out of scope:** 人读模式的输出继承与调用契约（由 `MP-15` 承接）；退出 1 的冲突状态与退出码 1/2（由 `MP-37` 承接；本卡失败用例使用退出 2 的策略，`MP-37` 之后仍按 `MP-15` 回滚）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `MP-15` 版本在结构化输出模式下以 `LBR-UNSUPPORTED-001` 拒绝外部策略（本卡取代） | `command::merge_test::external_strategy_execution_contract`（`MP-15` 新增） |
| `--machine` 隐含 `--json=ndjson` 与 `--quiet`；`is_json()` 以 `json_format` 判定；成功 JSON 为 `{"ok": true, "command": ..., "data": ...}` 信封 | `src/utils/output.rs:102-119,170-182,330-331` |
| JSON 模式下错误报告作为一个 JSON 文档写到 stderr（`Pretty` 多行、`Compact`/`Ndjson` 单行），`details` 映射进入该文档；既有用例以 `serde_json::from_str` 解析整个 stderr 并断言 `ok`/`error_code` | `src/utils/error.rs:1039-1042,1137-1160`；`tests/command/pull_test.rs:149-152,752-757` |

**Acceptance criteria:**

- [ ] AC1：`--json` 下策略成功时 stdout 只含一个 JSON 值。
- [ ] AC2：`--json` 下策略成功时子进程的两路输出以 UTF-8（有损）解码后的文本记录在 JSON 数据中。
- [ ] AC3：`--json` 下策略成功时子进程输出不转发到 Libra 的 stderr。
- [ ] AC4：`--machine` 下策略成功时 stdout 只含 NDJSON。
- [ ] AC5：`--json` 下策略失败时 stderr 恰为一个错误 JSON 文档。
- [ ] AC6：`--json` 下策略失败时子进程的两路输出以同样解码后的文本记录在错误报告中。
- [ ] AC7：`--json` 下策略失败时 stdout 为空。
- [ ] AC8：`--machine` 下策略失败时 stderr 恰为一行错误 JSON。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::external_strategy_structured_output_isolation` 承载 AC1–AC8；夹具 F-EXT（`t7606` 形态，用例 `#[cfg(unix)]`）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`c0`（`c0.c`）→ `main` 上 `c1`（`c1.c`）；`c2` 自 `c0` 新增 `c2.c`；临时目录 `<bin>`（工作树外）用于放置可执行脚本，每个脚本以 `#!/bin/sh` 与 `set -e` 开头（任一命令失败即以该命令的非 0 码退出），先把参数逐行写入 `<bin>/args`、把 `pwd` 写入 `<bin>/cwd` 并创建 `<bin>/ran`；`git-merge-theirs` 随后以 `for last; do :; done` 取得最后一个参数（即目标 OID；带 `-X` 时 `$4` 是字面量 `HEAD`），执行 `<libra 绝对路径> read-tree "$last"`、写入 `ext.txt`=`ext\n` 并 `<libra 绝对路径> add ext.txt`，再把 `<libra 绝对路径> write-tree` 的 stdout 写入 `<bin>/tree`，然后 `exit 0`（暂存的树与两个父提交的树都不同）；所有调用以子进程环境 `PATH=<bin>:<原 PATH>`；`git-merge-theirs` 在写入 `<bin>/tree` 之前额外执行 `printf 'strategy-out\377\n'` 与 `printf 'strategy-err\377\n' >&2`；失败用的 `git-merge-fail2` 执行 `printf 'fail-out\377\n'`、`printf 'fail-err\377\n' >&2` 后 `exit 2`（`\377` 是非 UTF-8 字节 0xFF，用以判定有损解码契约）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：`libra --json merge -s theirs c2 -m m` 退出 0（执行条件）；stdout 去掉首尾空白后可被 `serde_json::from_str` 解析为恰好一个 JSON 值（混入子进程输出即解析失败）。
- AC2：AC1 的 JSON 值中 `data.strategy_output` 等于 `{"stdout": "strategy-out\ufffd\n", "stderr": "strategy-err\ufffd\n"}`（`serde_json::Value` 相等比较；字节 0xFF 解码为 U+FFFD——严格解码而失败或丢弃该字节的实现即不相等）。
- AC3：AC1 调用的 stderr 既不含 `strategy-out` 也不含 `strategy-err`（按字节检索）。
- AC4：重建夹具；`libra --machine merge -s theirs c2 -m m` 退出 0（执行条件）；stdout 非空，且每个非空行都可被 `serde_json::from_str` 解析为一个 JSON 值（混入的子进程输出行即解析失败）。
- AC5：重建夹具；`libra --json merge -s fail2 c2 -m m` 以非 0 退出（执行条件）；stderr 去掉首尾空白后可被 `serde_json::from_str` 解析为恰好一个 JSON 值，且其 `ok` 为 `false`（子进程输出若被转发到 stderr 即解析失败）。
- AC6：AC5 的错误 JSON 中 `details.strategy_output` 等于 `{"stdout": "fail-out\ufffd\n", "stderr": "fail-err\ufffd\n"}`（同 AC2 的解码规则）。
- AC7：AC5 调用的 stdout 为空（GC-MP-01）。
- AC8：重建夹具；`libra --machine merge -s fail2 c2 -m m` 以非 0 退出（执行条件）；stderr 恰含一个非空行，该行可被 `serde_json::from_str` 解析为一个 JSON 值且其 `ok` 为 `false`。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::external_strategy_structured_output_isolation)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::external_strategy_structured_output_isolation)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-15`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`strategy_output`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=外部策略结构化输出隔离; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-15,DEP-MP-10; writeset=序列化于MP-15; release=independent; split-from=MP-15; exception=N/A`

---

### Task MP-37: 外部策略的冲突状态与 Git 退出码（1 / 2）（#1027）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-15` 拆出：外部策略退出 1 时不再回滚，而是进入冲突状态（`merge-state.json` 记录可选字段 `external_strategy`、`strategy` 保持 `None`，`ADR-MP-05`）并以 `with_exit_code(1)` 退出；退出 2（及其它非 0/1）保留 `MP-15` 的回滚与 `Merge with strategy <name> failed.`，并以 `with_exit_code(2)` 退出（`ADR-MP-01`）。（按 G-09 自本卡拆出 `MP-42`。）

**Out of scope:** 调用契约与回滚（由 `MP-15` 承接）；多个 `-s` 中含外部策略的拒绝（由 `MP-42` 承接，`DEFER-MP-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git：`try_merge_strategy` 返回 1 为冲突（写合并状态、退出 1），≥2 为未处理（`restore_state`、退出 2）（Git 2.54.0 预探：`fail2` 退出 2） | `git@6de20f6092` `builtin/merge.c:1786-1849` |
| `MergeState` 忽略未知字段，可新增可选字段而保持旧二进制可读 | `src/command/merge/state.rs:55-56,147` |
| `MP-15` 用例 AC6 对退出 1 断言回滚；本卡按「验证执行协议」第 5 条只把该 AC6 断言改为本卡的冲突状态断言，AC7（退出 2 回滚）保留 | `command::merge_test::external_strategy_execution_contract`（`MP-15` 新增） |

**Acceptance criteria:**

- [ ] AC1：`libra merge -s fail1 c2` 以退出码 1 结束。
- [ ] AC2：AC1 之后待决状态记录外部策略名。
- [ ] AC3：AC1 之后待决状态不写新的 `strategy` 取值。
- [ ] AC4：AC1 之后索引保留策略的结果。
- [ ] AC5：`libra merge -s fail2 c2` 输出 Git 未处理诊断。
- [ ] AC6：AC5 以退出码 2 结束。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::external_strategy_exit_codes` 承载 AC1–AC6；夹具 F-EXT（`t7606` 形态，用例 `#[cfg(unix)]`）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`c0`（`c0.c`）→ `main` 上 `c1`（`c1.c`）；`c2` 自 `c0` 新增 `c2.c`；临时目录 `<bin>`（工作树外）用于放置可执行脚本，每个脚本以 `#!/bin/sh` 与 `set -e` 开头（任一命令失败即以该命令的非 0 码退出），先把参数逐行写入 `<bin>/args`、把 `pwd` 写入 `<bin>/cwd` 并创建 `<bin>/ran`；`git-merge-theirs` 随后以 `for last; do :; done` 取得最后一个参数（即目标 OID；带 `-X` 时 `$4` 是字面量 `HEAD`），执行 `<libra 绝对路径> read-tree "$last"`、写入 `ext.txt`=`ext\n` 并 `<libra 绝对路径> add ext.txt`，再把 `<libra 绝对路径> write-tree` 的 stdout 写入 `<bin>/tree`，然后 `exit 0`（暂存的树与两个父提交的树都不同）；所有调用以子进程环境 `PATH=<bin>:<原 PATH>`；`git-merge-fail1` 执行 `<libra 绝对路径> read-tree "$4"`、把 `<libra 绝对路径> write-tree` 的 stdout 写入 `<bin>/tree` 后 `exit 1`；`git-merge-fail2` 执行 `<libra 绝对路径> read-tree "$4"` 后 `exit 2`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：`LIBRA_FINE_EXIT_CODES` 未设置；退出码恰为 1（Git：策略返回 1 即冲突）。
- AC2：`.libra/merge-state.json` 的 `external_strategy` 字段为 `"fail1"`（文件不存在即判失败）。
- AC3：`.libra/merge-state.json` 的 `strategy` 字段缺失或为 `null`（`ADR-MP-05`）。
- AC4：`libra write-tree` 的 stdout 等于 `<bin>/tree`。
- AC5：重建夹具（AC1 留下的待决合并不得沿用）；`libra merge -s fail2 c2` 以非 0 退出；stderr 含一行恰为 `Merge with strategy fail2 failed.`（Git 2.54.0 预探）。
- AC6：`LIBRA_FINE_EXIT_CODES` 未设置；退出码恰为 2（Git 2.54.0 预探）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::external_strategy_exit_codes)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::external_strategy_exit_codes)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改 docs/error-codes.md 退出码契约（ER-13 T-1）` | **Dependencies:** `MP-51`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/state.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/state.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Merge with strategy`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md` 必须更新：在「Per-command exit-code overrides」小节登记本卡的退出码覆写（稳定码不变）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=外部策略退出码 1/2 与冲突状态; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-51,DEP-MP-10; writeset=序列化于MP-51; release=independent; split-from=MP-15; exception=N/A`

---

### Task MP-42: 多个 `-s` 中含外部策略时拒绝（`DEFER-MP-03`）（#1025, #1027）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-37` 拆出：按 `ADR-MP-02` 与 `DEFER-MP-03`，多个 `-s` 中只要含一个非内置策略，就在全部策略名通过 `MP-35` 白名单与 `MP-38` 的 `PATH` 查找（未找到的名字按 `MP-38` 先行报告）之后、解析合并目标与任何策略（内置或外部）尝试、运行或探测之前以 `LBR-UNSUPPORTED-001` 拒绝，stderr 输出 `external merge strategy '<name>' cannot be combined with other strategies`（`<name>` 为命令行中第一个非内置策略名）并提示改为单独使用该策略；仓库状态不变。只含内置策略的链（`MP-14`）与单个外部策略（`MP-15`、`MP-37`）不受影响。取代 `MP-15` 发布后该组合沿用的 `MP-38` 过渡拒绝。

**Out of scope:** 外部策略参与多策略链的 Git 语义（回绕、候选评分、末位结果复用与重跑，`git@6de20f6092` `builtin/merge.c:1786-1860`）——`DEFER-MP-03`；单策略执行与退出码（由 `MP-15`、`MP-37` 承接）；只含内置策略的链（由 `MP-14` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| pinned Git 不能在一次调用中使用两个自定义策略：`get_strategy` 找到首个自定义策略后释放命令列表而 `loaded` 仍为 1，第二个自定义名查找失败（Git 2.54.0 预探：`git merge -s fail1 -s fail2 c2` 的 stderr 为 `Could not find merge strategy 'fail2'.` 与 `Available strategies are:.`，退出 1） | `git@6de20f6092` `builtin/merge.c:170-231`（`cmdnames_release` 位于 `228-229`） |
| Git 支持「一个外部策略 + 内置策略」的链（Git 2.54.0 预探：`git merge -s fail2 -s ours c2` 依次输出 `Trying merge strategy fail2...`、`Rewinding the tree to pristine...`、`Trying merge strategy ours...` 并以 `ours` 完成）；`#1025` 的验收与 `#1027` 的 Acceptance criteria 都不含这种组合，`#1027` Expected behavior 第 3 条要求外部策略参与链，按 `DEFER-MP-03` 延后 | `git@6de20f6092` `builtin/merge.c:1786-1860`；`#1025`；`#1027` |
| 链中未找到的策略名由 `MP-38` 在运行任何策略前报告（Git 在选项解析期查找） | `MP-38` AC6/AC7；`git@6de20f6092` `builtin/merge.c` `get_strategy` |

**Acceptance criteria:**

- [ ] AC1：`libra merge -s fail2 -s ours c2` 以 `LBR-UNSUPPORTED-001` 拒绝。
- [ ] AC2：AC1 的 stderr 指明不可组合的外部策略。
- [ ] AC3：AC1 不运行外部策略。
- [ ] AC4：AC1 不改变仓库快照。
- [ ] AC5：外部策略列在内置策略之后时同样拒绝。
- [ ] AC6：组合拒绝先于合并目标解析（因而先于任何策略尝试）。
- [ ] AC7：AC5 不运行外部策略。
- [ ] AC8：AC5 不改变仓库快照。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::external_strategy_chain_rejected` 承载 AC1–AC8；夹具 F-EXT（`t7606` 形态，用例 `#[cfg(unix)]`）（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`c0`（`c0.c`）→ `main` 上 `c1`（`c1.c`）；`c2` 自 `c0` 新增 `c2.c`；临时目录 `<bin>`（工作树外）用于放置可执行脚本，每个脚本以 `#!/bin/sh` 与 `set -e` 开头（任一命令失败即以该命令的非 0 码退出），先把参数逐行写入 `<bin>/args`、把 `pwd` 写入 `<bin>/cwd` 并创建 `<bin>/ran`；`git-merge-theirs` 随后以 `for last; do :; done` 取得最后一个参数（即目标 OID；带 `-X` 时 `$4` 是字面量 `HEAD`），执行 `<libra 绝对路径> read-tree "$last"`、写入 `ext.txt`=`ext\n` 并 `<libra 绝对路径> add ext.txt`，再把 `<libra 绝对路径> write-tree` 的 stdout 写入 `<bin>/tree`，然后 `exit 0`（暂存的树与两个父提交的树都不同）；所有调用以子进程环境 `PATH=<bin>:<原 PATH>`；`git-merge-fail2` 执行 `<libra 绝对路径> read-tree "$4"`，再把已跟踪的 `c1.c` 改为 `broken\n`、删除已跟踪的 `c0.c`，然后 `exit 2`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出；stderr 含 `Error-Code: LBR-UNSUPPORTED-001`。
- AC2：AC1 调用的 stderr 含子串 `external merge strategy 'fail2' cannot be combined with other strategies`。
- AC3：AC1 之后 `<bin>/ran` 不存在。
- AC4：AC1 调用前后的「仓库快照 Σ」逐项相等。
- AC5：重建夹具；`libra merge -s ours -s theirs c2` 以非 0 退出，stderr 含 `Error-Code: LBR-UNSUPPORTED-001`。
- AC6：重建夹具；`libra merge -s ours -s theirs no-such-ref` 以非 0 退出，stderr 含 `external merge strategy 'theirs' cannot be combined with other strategies`（任何策略尝试都要先解析合并目标；先尝试 `ours` 的实现会报目标无法解析）。
- AC7：AC5 之后 `<bin>/ran` 不存在。
- AC8：AC5 调用前后的「仓库快照 Σ」逐项相等。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::external_strategy_chain_rejected)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::external_strategy_chain_rejected)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'`（`src/command/merge.rs` 内单元测试模块；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-37`, `DEP-MP-10`

**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`cannot be combined with other strategies`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`COMPATIBILITY.md` `| merge |` 行必须更新：记录多个 `-s` 中含外部策略时拒绝的有意差异（`DEFER-MP-03`）；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=含外部策略的多策略链拒绝; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-37,DEP-MP-10; writeset=序列化于MP-37; release=independent; split-from=MP-37; exception=N/A`

---

### Task MP-16: 冲突标记附带路径标签（Git 扩展标记）（#1011）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 当冲突两侧的路径不同（重命名）时，冲突标记按 Git 写为 `<<<<<<< <ours-label>:<ours-path>` 与 `>>>>>>> <theirs-label>:<theirs-path>`；同路径冲突保持不带后缀的标签。

**Out of scope:** 冲突风格 `diff3`/`zdiff3` 的基准段标签（`#1011` Expected 3 提出、其编号验收只列两侧标签，按 `DEFER-MP-05` 延后）；其它重命名冲突的落盘（由 `MP-17`..`MP-20` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 冲突标记写出与重标签处未透传两侧路径 | `src/command/merge/conflict.rs:337,566`；`src/command/merge/rename_merge.rs:646` |
| `#1011` AC1–AC3（Git `t/t6402-merge-rename.sh#42-43`） | `git@6de20f6092` `t/t6402-merge-rename.sh` |

**Acceptance criteria:**

- [ ] AC1：在 `rename^0` 上 `libra merge -s recursive main^0` 后 `renamed_file` 含带路径的 ours 标记。
- [ ] AC2：AC1 的 `renamed_file` 含带路径的 theirs 标记。
- [ ] AC3：反向合并的 ours 标记带 HEAD 侧路径。
- [ ] AC4：反向合并的 theirs 标记带另一侧路径。
- [ ] AC5：同路径冲突的 ours 标记不带路径后缀。
- [ ] AC6：同路径冲突的 theirs 标记不带路径后缀。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::conflict_markers_carry_path_labels` 承载 AC1–AC6；夹具 F-1011（`#1011` 复现）：`original_file`=`1..8`（每行一个数字）提交 `A`；`rename` 分支追加 `9` 并 `libra mv original_file renamed_file` 提交 `B`；`main` 追加 `8.5` 提交 `C`；同路径对照：另一仓库中 `p`=`x\n` 的基准上 `main` 改为 `l\n`、分支 `side` 改为 `r\n`，在 `main` 上 `libra merge side`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出（冲突）；`renamed_file` 含一行恰为 `<<<<<<< HEAD:renamed_file`（`#1011` AC1）。
- AC2：`renamed_file` 含一行恰为 `>>>>>>> main^0:original_file`（`#1011` AC1）。
- AC3：重建夹具（AC1 留下的待决冲突不得沿用）；在 `main^0` 上 `libra merge -s recursive rename^0` 以非 0 退出（冲突，执行条件）；`renamed_file` 含一行恰为 `<<<<<<< HEAD:original_file`（`#1011` AC2）。
- AC4：AC3 的 `renamed_file` 含一行恰为 `>>>>>>> rename^0:renamed_file`（`#1011` AC2）。
- AC5：同路径对照的 `libra merge side` 以非 0 退出（冲突，执行条件）；`p` 里唯一一条以 `<<<<<<<` 开头的行恰为 `<<<<<<< HEAD`（`#1011` AC3）。
- AC6：同路径对照中 `p` 里唯一一条以 `>>>>>>>` 开头的行恰为 `>>>>>>> side`（`#1011` AC3）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::conflict_markers_carry_path_labels)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::conflict_markers_carry_path_labels)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-42`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/conflict.rs`, `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/conflict.rs`, `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`HEAD:renamed_file`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=冲突标记路径标签; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=MP-42,DEP-MP-10; writeset=序列化于MP-42; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-17: `-s recursive` D/F 冲突：不遗留 `sub/file`、报告 `modify/delete`、`dir~HEAD` 完整（#1007）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 修复 `-s recursive` 下 D/F 冲突：未冲突的 `sub/file` 不留未合并阶段；`dir/file-in-the-way` 的 `modify/delete` 冲突完整报告；被移到 `dir~HEAD` 的文件经三方合并后内容完整。（按 G-09 自本卡拆出 `MP-24`。）

**Out of scope:** rename/directory 冲突（由 `MP-24` 承接）；复现中的部分提交与脏索引检出前置（`#1007` AC5 的级联问题，不在本卡）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| D/F 冲突在树扁平化与工作区落盘时遗留未合并子文件、漏报 `modify/delete`、截断 `dir~HEAD` | `src/command/merge.rs:5031,7114,8428,8589`；`src/command/merge/tree_merge.rs`；`src/command/merge/workdir.rs` |
| `#1007` AC1–AC4（Git `t/t6402-merge-rename.sh#15`） | `git@6de20f6092` `t/t6402-merge-rename.sh` |

**Acceptance criteria:**

- [ ] AC1：`libra ls-files -u` 与 Git 一致（不含 `sub/file`）。
- [ ] AC2：`libra status --short` 的冲突视图与 Git 一致。
- [ ] AC3：合并输出报告 `modify/delete`。
- [ ] AC4：合并输出报告移到 `dir~HEAD`。
- [ ] AC5：合并输出报告对 `dir~HEAD` 的自动合并。
- [ ] AC6：`dir~HEAD` 内容完整。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::recursive_df_conflict_and_modify_delete` 承载 AC1–AC6；夹具 F-1007（`#1007` 复现，以 Libra 命令或对象级构造直接建立三个提交，绕过与本卡无关的检出级联）：共同提交含 `sub/file`=`1..10`、`dir/file-in-the-way`=`foo\n`；`dir-in-way` 在其上 `sub/file` 追加 `11`、`dir/file-in-the-way` 追加 `more`；`renamed-file-has-no-conflicts` 自共同提交删除 `dir/` 与 `sub/file`，新增文件 `dir`=`1 2 3 4 5555 6 7 8 9 10`（每行一个）；在后者分离 HEAD 上执行 `libra merge --strategy=recursive dir-in-way`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出（冲突）；`libra ls-files -u` 的 stdout 恰为 `100644 <foo> 1\tdir/file-in-the-way\n100644 <foomore> 3\tdir/file-in-the-way\n100644 <dirH> 2\tdir~HEAD\n`，其中 `<foo>`、`<foomore>`、`<dirH>` 依次为 `libra hash-object` 对 `foo\n`、`foo\nmore\n`、`1\n2\n3\n4\n5555\n6\n7\n8\n9\n10\n` 的 OID（Git 2.54.0 预探；`#1007` AC1–AC2）。
- AC2：stdout 恰为 `D  dir\nDU dir/file-in-the-way\nAU dir~HEAD\n`（Git 2.54.0 预探；`#1007` AC2）。
- AC3：合并调用的 stdout 含一行恰为 `CONFLICT (modify/delete): dir/file-in-the-way deleted in HEAD and modified in dir-in-way.  Version dir-in-way of dir/file-in-the-way left in tree.`（Git 2.54.0 预探；`#1007` AC3）。
- AC4：合并调用的 stdout 含一行恰为 `CONFLICT (file/directory): directory in the way of dir from HEAD; moving it to dir~HEAD instead.`（Git 2.54.0 预探；`#1007` AC3）。
- AC5：合并调用的 stdout 含一行恰为 `Auto-merging dir~HEAD`（Git 2.54.0 预探；`#1007` AC3）。
- AC6：`dir~HEAD` 内容恰为 `1\n2\n3\n4\n5555\n6\n7\n8\n9\n10\n11\n`（`#1007` AC4）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::recursive_df_conflict_and_modify_delete)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::recursive_df_conflict_and_modify_delete)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-16`, `DEP-MP-10`, `DEP-MP-11`

**Implementation write set:** `src/command/merge.rs`, `src/command/merge/tree_merge.rs`, `src/command/merge/workdir.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge.rs`, `src/command/merge/tree_merge.rs`, `src/command/merge/workdir.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`dir~HEAD`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=recursive D/F 冲突落盘与报告; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=MP-16,DEP-MP-10,DEP-MP-11; writeset=序列化于MP-16; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-24: rename/directory 冲突：识别 HEAD 侧重命名、不伪造阶段、内容合并进 `newfile~HEAD`（#1016）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-17` 拆出：HEAD 侧把 `file` 重命名为 `newfile`、另一侧在 `newfile/` 建目录时，按 Git 识别重命名、把三方合并结果写到 `newfile~HEAD`，索引只含 `newfile/realfile`（stage 0）与 `newfile~HEAD`（stage 2），不残留 `file` 的伪阶段。

**Out of scope:** 普通 D/F 冲突（由 `MP-17` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| HEAD 侧重命名未检出，产生伪 stage，`newfile~HEAD` 未做内容合并 | `src/command/merge/rename_merge.rs:182`；`src/command/merge.rs:8428`；`src/command/merge/workdir.rs` |
| `#1016` AC1–AC5（Git `t/t6422-merge-rename-corner-cases.sh#7`） | `git@6de20f6092` `t/t6422-merge-rename-corner-cases.sh` |

**Acceptance criteria:**

- [ ] AC1：`libra ls-files -s` 与 Git 一致（恰两条）。
- [ ] AC2：`libra ls-files -u` 与 Git 一致（恰一条）。
- [ ] AC3：`file` 不残留为未跟踪文件。
- [ ] AC4：`newfile~HEAD` 为两侧内容的三方合并。
- [ ] AC5：`newfile/realfile` 与 `newfile~HEAD` 都在磁盘上。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::rename_directory_conflict_newfile_head` 承载 AC1–AC5；夹具 F-1016（`#1016` 复现）：基准 `file`=`1..6`；`right` 追加 `7` 并新增 `newfile/realfile`=`junk\n`；`left-clean` 把 `file` 重命名为 `newfile` 且内容为 `0` 加基准 `file`；在 `left-clean^0` 上 `libra merge -s recursive right^0`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出（冲突）；`libra ls-files -s` 的 stdout 恰为 `100644 <junk> 0\tnewfile/realfile\n100644 <merged> 2\tnewfile~HEAD\n`，`<junk>`/`<merged>` 为 `libra hash-object` 对 `junk\n`、`0\n1\n2\n3\n4\n5\n6\n7\n` 的 OID（Git 2.54.0 预探；`#1016` AC1）。
- AC2：`libra ls-files -u` 的 stdout 恰为 `100644 <merged> 2\tnewfile~HEAD\n`（Git 2.54.0 预探；`#1016` AC2）。
- AC3：`libra ls-files -o` 的 stdout 不含 `file` 行（`#1016` AC3）。
- AC4：`newfile~HEAD` 内容恰为 `0\n1\n2\n3\n4\n5\n6\n7\n`（`#1016` AC4）。
- AC5：两个路径都存在（`#1016` AC5）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::rename_directory_conflict_newfile_head)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::rename_directory_conflict_newfile_head)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-17`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/rename_merge.rs`, `src/command/merge.rs`, `src/command/merge/workdir.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/rename_merge.rs`, `src/command/merge.rs`, `src/command/merge/workdir.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`newfile~HEAD`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=rename/directory 冲突落盘与阶段; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=MP-17,DEP-MP-10; writeset=序列化于MP-17; release=independent; split-from=MP-17; exception=N/A`

---

### Task MP-18: rename/delete 冲突在重命名文件中保留纯内容（#1009）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** rename/delete 冲突时，重命名目标文件保留重命名侧的原内容（不写冲突标记），索引记录对应阶段并输出 Git 的 `CONFLICT (rename/delete)` 报告。（按 G-09 自本卡拆出 `MP-25`。）

**Out of scope:** rename/rename D/F（由 `MP-25` 承接）；`#1009` AC4 所述 `t6402#23` 整体通过所依赖的检出 typechange 级联修复（不在本计划范围；`#1009` Expected 4 的未跟踪目录变体由本卡 AC6/AC7 覆盖）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| rename/delete 冲突向重命名目标写入冲突标记 | `src/command/merge/rename_merge.rs:908,1102`；`src/command/merge.rs:8084` |
| `#1009` AC1–AC3（Git `t/t6402-merge-rename.sh#23`） | `git@6de20f6092` `t/t6402-merge-rename.sh` |

**Acceptance criteria:**

- [ ] AC1：`one` 为 `other` 且无冲突标记。
- [ ] AC2：`two` 为 `stuff` 且无冲突标记。
- [ ] AC3：`libra ls-files -u` 的阶段与 blob 与 Git 一致。
- [ ] AC4：输出 HEAD 侧重命名的 rename/delete 报告。
- [ ] AC5：输出另一侧重命名的 rename/delete 报告。
- [ ] AC6：未跟踪空目录 `one` 挡路时结果不变：工作树只含两个纯内容文件。
- [ ] AC7：AC6 变体的未合并阶段与 AC3 相同。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::rename_delete_keeps_plain_content` 承载 AC1–AC7；夹具 F-1009（`#1009` 复现）：共同提交 `one/file`=`stuff\n`、`two/file`=`other\n`；`rename-two` 删除 `one/` 并 `two/file`→`one`；`rename-one` 删除 `two/` 并 `one/file`→`two`；在 `rename-one^0` 上 `libra merge --strategy=recursive rename-two`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出（冲突）；`one` 内容恰为 `other\n`（`#1009` AC1）。
- AC2：`two` 内容恰为 `stuff\n`（`#1009` AC1）。
- AC3：stdout 恰为 `100644 <other> 1\tone\n100644 <other> 3\tone\n100644 <stuff> 1\ttwo\n100644 <stuff> 2\ttwo\n`，`<stuff>`/`<other>` 为 `libra hash-object` 对 `stuff\n`/`other\n` 的 OID（Git 2.54.0 预探；`#1009` AC2）。
- AC4：合并调用的 stdout 含一行恰为 `CONFLICT (rename/delete): one/file renamed to two in HEAD, but deleted in rename-two.`（Git 2.54.0 预探；`#1009` AC3）。
- AC5：合并调用的 stdout 含一行恰为 `CONFLICT (rename/delete): two/file renamed to one in rename-two, but deleted in HEAD.`（Git 2.54.0 预探；`#1009` AC3）。
- AC6：重建夹具，在 `rename-one^0` 上先 `mkdir one`（未跟踪空目录）再执行同一合并（以非 0 退出，冲突，执行条件）；「工作树清单」恰为普通文件 `one`=`other\n` 与 `two`=`stuff\n`（无其它路径；Git 2.54.0 预探；`t6402` 的 `pair rename to parent of other (D/F conflicts) w/ untracked dir`）。
- AC7：AC6 之后 `libra ls-files -u` 的 stdout 与 AC3 的期望逐字相等（Git 2.54.0 预探）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC7）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::rename_delete_keeps_plain_content)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::rename_delete_keeps_plain_content)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-24`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/rename_merge.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/rename_merge.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`rename/delete`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=rename/delete 冲突纯内容保留; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-24,DEP-MP-10; writeset=序列化于MP-24; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-25: rename/rename D/F 冲突落盘（消除 `LBR-IO-002` 并记录冲突）（#1010）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-18` 拆出：两侧把同一文件分别重命名到对方目录所在路径时，按 Git 把结果移到 `one~HEAD`、`two~second-rename`，完整落盘并记录未合并阶段，不再以 `LBR-IO-002` 中止。（按 G-09 自本卡拆出 `MP-46` 承接该冲突的三条报告。）

**Out of scope:** 该冲突的报告文案（由 `MP-46` 承接）；rename/delete（由 `MP-18` 承接）；`rename/rename(1to2)` 阶段（由 `MP-19` 承接）；复现中的非空目录检出守卫（`#1010` AC4）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 落盘触发 `LBR-IO-002` 且未记录冲突 | `src/command/merge/workdir.rs`；`src/command/merge/rename_merge.rs:1089` |
| `#1010` AC1–AC3（Git `t/t6402-merge-rename.sh#26`） | `git@6de20f6092` `t/t6402-merge-rename.sh` |

**Acceptance criteria:**

- [ ] AC1：合并正常停在冲突。
- [ ] AC2：合并不以 I/O 错误中止。
- [ ] AC3：`libra ls-files -s` 与 Git 一致（恰五条）。
- [ ] AC4：`libra ls-files -u` 与 Git 一致（恰三条）。
- [ ] AC5：工作树中没有残留路径。
- [ ] AC6：`one/file` 与 `two/file` 落盘为空文件。
- [ ] AC7：`one~HEAD` 与 `two~second-rename` 落盘为重命名侧内容。
- [ ] AC8：`original` 不在工作树中。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::rename_rename_df_conflict_materializes` 承载 AC1–AC8；夹具 F-1010（`#1010` 复现，以对象级构造或分离 HEAD 的 `reset --hard` 绕过检出守卫）：共同提交 `original`=`stuff\n`；`first-rename` 新增空文件 `two/file` 并 `original`→`one`；`second-rename` 新增空文件 `one/file` 并 `original`→`two`；在 `first-rename^0` 上 `libra merge --strategy=recursive second-rename`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：合并以非 0 退出（执行条件）；stdout 含一行恰为 `Automatic merge failed; fix conflicts and then commit the result.`（Git 2.54.0 预探）。
- AC2：AC1 的调用 stderr 不含 `Error-Code: LBR-IO-`（`LBR-IO-001`/`LBR-IO-002` 都不允许）。
- AC3：`libra ls-files -s` 的 stdout 恰为 `100644 <empty> 0\tone/file\n100644 <stuff> 2\tone~HEAD\n100644 <stuff> 1\toriginal\n100644 <empty> 0\ttwo/file\n100644 <stuff> 3\ttwo~second-rename\n`，`<empty>`/`<stuff>` 为 `libra hash-object` 对空内容与 `stuff\n` 的 OID（Git 2.54.0 预探；`#1010` AC2）。
- AC4：`libra ls-files -u` 的 stdout 恰为 `100644 <stuff> 2\tone~HEAD\n100644 <stuff> 1\toriginal\n100644 <stuff> 3\ttwo~second-rename\n`（Git 2.54.0 预探；`#1010` AC2）。
- AC5：「工作树清单」（「验证执行协议」第 8 条）的路径与类型恰为：目录 `one`、`two`，普通文件 `one/file`、`one~HEAD`、`two/file`、`two~second-rename`（无其它路径；`#1010` AC2 的「无未跟踪残留」；不用 `libra ls-files -o`：其 `--others` 只以 stage 0 条目排除已跟踪路径，会把仅有 stage 2/3 的 `one~HEAD`、`two~second-rename` 列为未跟踪，属 `#1017`，不在本计划范围，`src/command/ls_files.rs:385-436`）。
- AC6：两个文件都存在且字节长度为 0（`#1010` AC3）。
- AC7：两个文件内容都恰为 `stuff\n`（`#1010` AC3）。
- AC8：`original` 不存在（`#1010` AC3）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC8）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::rename_rename_df_conflict_materializes)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::rename_rename_df_conflict_materializes)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-18`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/rename_merge.rs`, `src/command/merge/workdir.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/rename_merge.rs`, `src/command/merge/workdir.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`two~second-rename`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=rename/rename D/F 冲突落盘; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=MP-18,DEP-MP-10; writeset=序列化于MP-18; release=independent; split-from=MP-18; exception=N/A`

---

### Task MP-46: rename/rename D/F 冲突的三条报告（#1010）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-25` 拆出：该冲突按 Git 在 stdout 报告两条 file/directory 移动与一条 rename/rename 冲突。

**Out of scope:** 落盘与索引阶段（由 `MP-25` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#1010` 实测 Libra 只输出部分报告且以 `LBR-IO-002` 中止 | `#1010`；`src/command/merge/rename_merge.rs:1089` |
| Git 2.54.0 预探：三条报告依次为 `one~HEAD` 的 file/directory、rename/rename、`two~second-rename` 的 file/directory | `git@6de20f6092` `t/t6402-merge-rename.sh#26` |

**Acceptance criteria:**

- [ ] AC1：报告 `one` 处的 file/directory 冲突。
- [ ] AC2：报告 rename/rename 冲突。
- [ ] AC3：报告 `two` 处的 file/directory 冲突。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::rename_rename_df_conflict_reports` 承载 AC1–AC3；夹具 F-1010（同 `MP-25`，以对象级构造或分离 HEAD 的 `reset --hard` 绕过检出守卫）：共同提交 `original`=`stuff\n`；`first-rename` 新增空文件 `two/file` 并 `original`→`one`；`second-rename` 新增空文件 `one/file` 并 `original`→`two`；在 `first-rename^0` 上 `libra merge --strategy=recursive second-rename`（以非 0 退出为执行条件）；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：stdout 含一行恰为 `CONFLICT (file/directory): directory in the way of one from HEAD; moving it to one~HEAD instead.`（Git 2.54.0 预探；`#1010` AC1）。
- AC2：stdout 含一行恰为 `CONFLICT (rename/rename): original renamed to one in HEAD and to two in second-rename.`（Git 2.54.0 预探；`#1010` AC1）。
- AC3：stdout 含一行恰为 `CONFLICT (file/directory): directory in the way of two from second-rename; moving it to two~second-rename instead.`（Git 2.54.0 预探；`#1010` AC1）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC3）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::rename_rename_df_conflict_reports)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::rename_rename_df_conflict_reports)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-25`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`two~second-rename`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=rename/rename D/F 冲突报告; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-25,DEP-MP-10; writeset=序列化于MP-25; release=independent; split-from=MP-25; exception=N/A`

---

### Task MP-50: `libra rm` 移除未合并路径的全部阶段（#1018）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-19` 拆出并前置：`libra rm <path>` 在 `<path>` 只有未合并阶段（无 stage 0；如 modify/delete 留下的 stage 1/2，或 rename/rename(1to2) 源路径仅有的 stage 1）时与 Git 一致移除其全部阶段、删除存在的工作树文件并退出 0；工作树文件缺失时同样成功；与 Git 相同，对未合并路径不做本地修改检查，工作树文件已被修改时也直接删除。stage 0 路径的既有行为（含本地修改与已暂存内容的拒绝）不变。为 `MP-19` 保留的 stage 1 源路径提供常规解决流程。

**Out of scope:** `rename/rename(1to2)` 的阶段构建（由 `MP-19` 承接）；`--cached`、`-n` 与未合并路径的组合以及 `libra add`/`libra restore --staged` 对未合并路径的处理（不在 `#1018` 范围，保持现状）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `libra rm` 只在 stage 0 条目中匹配路径规格（`Index::tracked_files` 即 `tracked_entries(0)`），未合并路径因此报 `pathspec '<path>' did not match any files`；移除时也只删 stage 0 条目（Libra 0.30.207 预探，F-RMU：合并后 `a` 留 stage 1/2、工作树 `a` 存在，`libra rm a` 以 `LBR-CLI-003` 失败） | `src/command/remove.rs:220-229,381-385`；`git-internal` 0.10.4 `src/internal/index.rs:687-692` |
| Git 2.54.0 预探：modify/delete 冲突后 `git rm a`（工作树文件存在，或已被改写为其它内容）输出 `rm 'a'`、退出 0，`a` 的全部阶段与工作树文件都被移除；rename/rename(1to2) 中只有 stage 1、无工作树文件的 `a` 同样被 `git rm a` 移除；以 `git update-index --index-info` 建立的只有 stage 1 的 `s` 被 `git rm s` 移除 | `git@6de20f6092` `builtin/rm.c` |
| `libra update-index --index-info` 接受 `<mode> SP <oid> SP <stage> TAB <path>`，可建立只有 stage 1 的条目（Libra 0.30.207 预探：之后 `libra rm s` 以 `LBR-CLI-003` 失败） | `src/command/update_index.rs:147-156` |

**Acceptance criteria:**

- [ ] AC1：工作树 `a` 存在时 `libra rm a` 移除其全部阶段。
- [ ] AC2：AC1 删除工作树文件 `a`。
- [ ] AC3：工作树 `a` 缺失时 `libra rm a` 移除其全部阶段。
- [ ] AC4：工作树 `a` 已被修改时 `libra rm a` 仍移除其全部阶段。
- [ ] AC5：AC4 删除已修改的工作树文件。
- [ ] AC6：只有 stage 1、工作树无文件的路径被 `libra rm` 移除。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::remove_test::rm_unmerged_path_removes_all_stages` 承载 AC1–AC6；夹具 F-RMU（`init_repo_via_cli` 建库后执行 `libra config user.name 'A U Thor'` 与 `libra config user.email author@example.com` 设定身份——不使用会设为 `Test User <test@example.com>` 的 `configure_identity_via_cli`，`tests/command/mod.rs:686-693`——全部提交带 `--no-gpg-sign`；再 `libra config vault.signing false`，使合并提交也不签名（`libra init` 默认启用 vault 签名，密钥在按调用 cwd 建立的测试 HOME 中，`tests/command/mod.rs:353-381`；以子目录为 cwd 时签名以 `LBR-AUTH-001` 失败，Libra 0.30.207 预探））：`main` 提交 `A`（`a`=`a0\n`、`k`=`k\n`）；`D` 自 `A` 删除 `a` 并提交；`main` 自 `A` 提交 `M`（`a`=`a1\n`）；在 `M` 上 `libra merge D` 产生 modify/delete 冲突（执行条件：以非 0 退出；`libra ls-files -u` 的 stdout 恰为 `100644 <a0> 1\ta\n100644 <a1> 2\ta\n`——`a` 恰有 stage 1 与 stage 2 两条且无 stage 3/stage 0，`<a0>` 为 `libra rev-parse A:a`、`<a1>` 为 `libra rev-parse M:a`；工作树 `a` 存在。Git 2.54.0 与 Libra 0.30.207 预探同形）；`<k>` 为 `libra rev-parse A:k`；AC1/AC2、AC3、AC4/AC5 与 AC6 各使用一份重建的夹具，AC1、AC3、AC4 在执行 `libra rm a` 之前都先满足上述两阶段前置条件；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：`libra rm a` 退出 0（执行条件）；`libra ls-files -s` 的 stdout 恰为 `100644 <k> 0\tk\n`（Git 2.54.0 预探）。
- AC2：AC1 之后 `a` 不存在（Git 2.54.0 预探）。
- AC3：重建夹具，合并后以 `std::fs::remove_file` 删除工作树 `a`；`libra rm a` 退出 0（执行条件）；`libra ls-files -s` 的 stdout 恰为 `100644 <k> 0\tk\n`（Git 2.54.0 预探）。
- AC4：重建夹具，合并后把工作树 `a` 写为 `a2\n`；`libra rm a` 退出 0（执行条件）；`libra ls-files -s` 的 stdout 恰为 `100644 <k> 0\tk\n`（Git 2.54.0 预探：未合并路径不做本地修改检查）。
- AC5：AC4 之后 `a` 不存在（Git 2.54.0 预探）。
- AC6：重建夹具但不执行合并（停在 `M`），以 `run_libra_command_with_stdin` 执行 `libra update-index --index-info`，stdin 为字符串 `100644 <k> 1\ts\n`（`<k>` 替换为该 OID；`\t` 为制表符、末尾换行），建立只有 stage 1 的 `s`（工作树无 `s`；该命令退出 0，执行条件）；`libra rm s` 退出 0（执行条件）；`libra ls-files -s -- s` 的 stdout 为空（Git 2.54.0 预探）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC6）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::remove_test::rm_unmerged_path_removes_all_stages)'`（`(new)`，在 `tests/command/remove_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::remove_test::rm_unmerged_path_removes_all_stages)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::remove_test::/)'`（`command::remove_test` 模块族，守护 `libra rm` 既有行为；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `MP-46`, `DEP-MP-10`

**Implementation write set:** `src/command/remove.rs`, `tests/command/remove_test.rs`, `docs/commands/rm.md`, `docs/commands/zh-CN/rm.md`, `docs/development/commands/rm.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `docs/error-codes.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/remove.rs`, `tests/command/remove_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/rm.md`、`docs/commands/zh-CN/rm.md`、`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only rm merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：EN 页 `unmerged`、zh-CN 页 `未合并`。逐文件语义核对：`docs/development/commands/rm.md`、`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| rm |` 第 186 行；`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md` 列入本卡逐文件语义核对（不是 N/A）：本卡使 `libra rm <未合并路径>` 由 `LBR-CLI-003`（退出 129，`fatal: pathspec '<path>' did not match any files`）失败改为退出 0；预期结论为「无需修改」并记录理由与未变 SHA-256——`docs/error-codes.md:99` 的 `LBR-CLI-003` 行按类别泛述 invalid pathspec 等示例、不枚举 `rm` 对未合并路径的处理，本卡也不新增稳定码或退出码覆写；若开工时该文件已出现针对 `rm` 未合并路径的具体描述，则同卡更新该描述；`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站 `merge.en.md` 中「`libra rm` 不接受未合并路径」的冲突恢复指引（`docs/commands/merge.md:196`、`docs/commands/zh-CN/merge.md:169`、网站 `merge.en.md:199`、`docs/development/commands/merge.md:110`，2026-11-18 以 `rg` 核对）必须同卡改为可用 `libra rm <path>` 移除未合并路径，开发文档保留历史事实并注明由本卡解除。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=rm 处理未合并路径; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-46,DEP-MP-10; writeset=序列化于MP-46; release=independent; split-from=MP-19; exception=N/A`

---

### Task MP-19: `rename/rename(1to2)` 保留源路径 stage 1（#1018）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** `rename/rename(1to2)` 冲突的索引构建按 Git（`git@6de20f6092` `merge-ort.c:3020-3068`）：源路径保留 stage 1（合并基 blob），不再以移除源路径代替；两个目标路径分别记为 stage 2/3——文本内容时两者都记为同一次三方内容合并的结果、工作树文件为该结果（既有行为，本卡保持：索引形态由 AC5 守护，工作树内容由既有用例 `command::merge_test::merge_rename_conflict_1to2_keeps_both_destinations_and_drops_the_source` 中「两个工作树文件内容相同且含两侧改动」的断言守护，本卡改写该用例时保留这些断言）；同时删去 `rename_merge.rs` 中「以移除源路径代替 stage 1」的有意偏离注释，保留的 stage 1 依赖前置的 `MP-50`（`libra rm` 移除未合并路径的全部阶段）以常规流程解决。（按 G-09 自本卡拆出 `MP-50`、`MP-54`。）

**Out of scope:** 二进制 1to2 的各侧 blob 与警告（`#1021`，由 `MP-54` 承接）；传递目录重命名（由 `MP-20` 承接）；`libra rm` 对未合并路径的处理（由 `MP-50` 承接）；`ls-files -o` 计数（`#1017`，不在本计划范围）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 源路径 stage 1 缺失 | `src/command/merge.rs:8923,13870`；`src/command/merge/rename_merge.rs:757`；`#1018` |
| 现行代码以移除源路径代替 stage 1 是有意偏离，理由是 `libra add`/`libra rm`/`libra restore --staged` 无法处理只有 stage 1、工作树无文件的路径（`LBR-CLI-003` 或 `path is unmerged`）；`libra rm` 的缺口由 `MP-50` 关闭 | `src/command/merge/rename_merge.rs:863-893`；`src/command/remove.rs:220-229,381-385` |
| 既有用例断言 1to2 源路径无阶段或以 `add -A` 完成 1to2 的解决；本卡按「验证执行协议」第 5 条只改写被取代的断言（源路径改为断言 stage 1 为合并基 blob；解决流程在 `add` 之前加 `libra rm <源路径>`），其余断言不变。已知候选（开工时按 ER-02 复核）：`command::merge_test::merge_rename_conflict_1to2_keeps_both_destinations_and_drops_the_source`、`command::merge_test::merge_rename_conflict_continue_finishes_the_1to2`、`command::merge_test::squash::merge_rename_conflict_squash_finishes_as_an_ordinary_commit`、`command::merge_test::rename_binary::merge_rename_conflict_binary_1to2_distinguishes_trivial_merge_from_fallback`、`command::merge_test::rename_fold_binary` 的 1to2 用例与库内 `command::merge::rename_conflict::one_to_two_records_both_destinations_and_drops_the_source`；全量门暴露的其它同类既有断言按同一规则判定、登记到本卡并按 ER-03 补入写集 | `tests/command/merge_test.rs:13082-13150,13655-13710`；`tests/command/merge_test/squash.rs:108-140`；`tests/command/merge_test/rename_binary.rs:111-160`；`tests/command/merge_test/rename_fold_binary.rs:71-170`；`src/command/merge.rs:13842-13905` |
| Git 2.54.0 预探（F-1018T）：文本 1to2 的 `git ls-files -s` 为 `1 a`（基准）、`3 b` 与 `2 c`（同一合并结果 blob，内容 `one\n2\n3\n4\nfive\n`），`b`、`c` 工作树均为该结果 | `git@6de20f6092` `merge-ort.c:3020-3031` |

**Acceptance criteria:**

- [ ] AC1：F-1018：`libra ls-files -s` 恰为 `1 a`、`3 b`、`2 c` 三条且 blob 均为基准内容。
- [ ] AC2：F-1018：`libra ls-files -u` 恰为同样三条。
- [ ] AC3：F-1018：`libra rm a` 移除源路径的未合并阶段。
- [ ] AC4：F-1018：常规流程能完成为 `C` 与 `B` 的合并提交且保留已暂存的解决结果。
- [ ] AC5：F-1018T：文本 1to2 的两个目标都记为同一次内容合并的结果，源路径留 stage 1。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::rename_1to2_keeps_source_stage1` 承载 AC1–AC5；夹具 F-1018T（文本 1to2）：`A`：`a`=`1\n2\n3\n4\n5\n`；`B` 自 `A` 把 `a` 重命名为 `b` 且第 1 行改为 `one`；`C` 自 `A` 把 `a` 重命名为 `c` 且第 5 行改为 `five`；在 `C^0` 上 `libra merge -s recursive B^0`（以非 0 退出，冲突，执行条件）。夹具 F-1018（`#1018` 复现）：`A`：`a`=`stuff\n`；`B` 自 `A` `a`→`b`；`C` 自 `A` `a`→`c`；在 `C^0` 上 `libra merge -s recursive B^0`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出（冲突）；`libra ls-files -s` 的 stdout 恰为 `100644 <stuff> 1\ta\n100644 <stuff> 3\tb\n100644 <stuff> 2\tc\n`，`<stuff>` 为 `libra rev-parse A:a`（`#1018` AC1）。
- AC2：同一合并之后 `libra ls-files -u` 的 stdout 与 AC1 的期望逐字相等（`#1018` AC2）。
- AC3：F-1018 的合并之后执行 `libra rm a` 退出 0（执行条件）；`libra ls-files -s -- a` 的 stdout 为空。
- AC4：AC3 之后把 `b` 写为 `resolved-b\n`、`c` 写为 `resolved-c\n` 并 `libra add b c`，记 `libra write-tree` 的 stdout 为 `<T>`，再 `libra merge --continue -m m` 退出 0（执行条件）；`libra cat-file -p HEAD` 的前三行逐字等于 `tree <T>`、`parent <C>`、`parent <B>`（`<C>`/`<B>` 为完整 OID；暂存的解决结果原样进入提交、`a` 不在其中，且提交记录了 `B` 的并入——只保留树的单父提交使第三行缺失）。
- AC5：F-1018T（新建，不沿用 AC1–AC4 的 F-1018 仓库）：在 `C^0` 上 `libra merge -s recursive B^0` 以非 0 退出（冲突，执行条件）后，`libra ls-files -s` 的 stdout 恰为 `100644 <a> 1\ta\n100644 <M> 3\tb\n100644 <M> 2\tc\n`，`<a>` 为 `libra rev-parse A:a`，`<M>` 为 `libra hash-object` 对 `one\n2\n3\n4\nfive\n` 的 OID（Git 2.54.0 预探；把二进制规则误用于文本的实现会在 `b`、`c` 留下各侧原始 blob）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::rename_1to2_keeps_source_stage1)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::rename_1to2_keeps_source_stage1)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --lib -E 'test(/^command::merge::rename_conflict::/)'`（`src/command/merge.rs` 的 `rename_conflict` 单元测试模块，本卡改写其中 1to2 断言；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list --lib` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-50`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/rename_merge.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `tests/command/merge_test/squash.rs`, `tests/command/merge_test/rename_binary.rs`, `tests/command/merge_test/rename_fold_binary.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/rename_merge.rs`, `src/command/merge.rs`, `tests/command/merge_test.rs`, `tests/command/merge_test/squash.rs`, `tests/command/merge_test/rename_binary.rs`, `tests/command/merge_test/rename_fold_binary.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`rename/rename`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=rename/rename(1to2) 源路径阶段; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=MP-50,DEP-MP-10; writeset=序列化于MP-50; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-54: 二进制 `rename/rename(1to2)` 为两侧记录各自 blob（#1021）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-19` 拆出：`rename/rename(1to2)` 中两个目标的内容按 Git 先做三方内容合并；只有当该合并**不干净**且所得结果的模式与 OID 都等于 HEAD 侧（即二进制内容无法合并、只取了一侧，Git `merge-ort.c:3028-3050` 的 `was_binary_blob` 条件 `!clean_merge && merged.mode == side1.mode && merged.oid == side1.oid`）时，才为两个目标路径分别记录各自一侧的原始 blob（stage 2 为 HEAD 侧、stage 3 为另一侧）、工作树文件为各自一侧的版本，并输出 `warning: Cannot merge binary files`；内容合并干净的二进制 1to2（如一侧未改内容、另一侧修改）与文本 1to2 仍把同一合并结果记到两个目标（既有行为，不变）。

**Out of scope:** 源路径 stage 1（由 `MP-19` 承接）；文本 1to2（既有行为，`MP-19` 守护）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 二进制 `1to2` 把同一 blob 写入两侧阶段与工作树 | `src/command/merge/rename_merge.rs:869`；`src/command/merge/conflict.rs:319`；`#1021` |
| 本卡在 `MP-19` 之后：F-1021 的源路径 `orig` 已按 `MP-19` 保留 stage 1；既有 binary 1to2 用例中，内容合并干净的对照（`tests/command/merge_test/rename_binary.rs:117-121` 列出的 `("base", "theirs", "theirs", "theirs")`、`("ours", "base", "ours", "ours")` 等，以及模式对照）断言两个目标为同一 blob，属本卡保留的行为、一律不改；只有满足上述 `was_binary_blob` 条件（两侧都改了内容、无法合并）的断言被本卡取代，按「验证执行协议」第 5 条只改写这些断言（判定清单写入本卡 `Current evidence`） | `tests/command/merge_test/rename_binary.rs:111-160`；`tests/command/merge_test/rename_fold_binary.rs:71-170` |
| Git：内容合并不干净且结果的模式与 OID 都等于 side 1 时（`was_binary_blob`）side 2 改用自身原始条目；否则两侧都记合并结果 | `git@6de20f6092` `merge-ort.c:3028-3053` |

**Acceptance criteria:**

- [ ] AC1：F-1021：`libra ls-files -s` 的阶段与 blob 与 Git 一致。
- [ ] AC2：F-1021：`orig-A` 工作树文件为 HEAD 侧版本。
- [ ] AC3：F-1021：`orig-B` 工作树文件为另一侧版本。
- [ ] AC4：F-1021：输出二进制无法合并的警告。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::rename_1to2_binary_side_blobs` 承载 AC1–AC4；夹具 F-1021（`#1021` 复现）：`.gitattributes`=`* binary\n`；`O`：`orig`=`1..10`；`A` 把 `orig`→`orig-A` 且内容 `1..11`；`B` 把 `orig`→`orig-B` 且内容 `0..10`；在 `A^0` 上 `libra merge -s recursive B^0`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：F-1021 的合并以非 0 退出（冲突，执行条件）；`libra ls-files -s` 的 stdout 恰为 `100644 <attr> 0\t.gitattributes\n100644 <O> 1\torig\n100644 <A> 2\torig-A\n100644 <B> 3\torig-B\n`，四者依次为 `libra rev-parse` 对 `O:.gitattributes`、`O:orig`、`A:orig-A`、`B:orig-B` 的 OID（`#1021` AC1；Git 2.54.0 输出见 `#1021` 复现）。
- AC2：`libra hash-object orig-A` 的 stdout 等于 `libra rev-parse A:orig-A`（`#1021` AC2–AC3）。
- AC3：`libra hash-object orig-B` 的 stdout 等于 `libra rev-parse B:orig-B`（`#1021` AC2–AC3）。
- AC4：合并调用的 stderr 含 `warning: Cannot merge binary files: orig (HEAD:orig-A vs. B^0:orig-B)`（`#1021` AC4）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC4）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::rename_1to2_binary_side_blobs)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::rename_1to2_binary_side_blobs)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-19`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/rename_merge.rs`, `src/command/merge/conflict.rs`, `tests/command/merge_test.rs`, `tests/command/merge_test/rename_binary.rs`, `tests/command/merge_test/rename_fold_binary.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/rename_merge.rs`, `src/command/merge/conflict.rs`, `tests/command/merge_test.rs`, `tests/command/merge_test/rename_binary.rs`, `tests/command/merge_test/rename_fold_binary.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`Cannot merge binary files`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=二进制 rename/rename(1to2) 各侧 blob; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=MP-19,DEP-MP-10; writeset=序列化于MP-19; release=independent; split-from=MP-19; exception=N/A`

---

### Task MP-20: 传递目录重命名：Git 的冲突报告与移动路径上的内容冲突（#1024）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 传递目录重命名场景按 Git 在 stdout 输出 `CONFLICT (file location): x/d renamed to z/d in B^0, inside a directory that was renamed in HEAD, suggesting it should perhaps be moved to y/d.`、`Auto-merging y/d` 与 `CONFLICT (content): Merge conflict in y/d`，并在移动后的 `y/d` 上做三方内容合并、写入带路径标签的冲突标记。（按 G-09 自本卡拆出 `MP-47` 承接索引阶段与路径清理。）

**Out of scope:** `y/d` 的索引阶段与 `z/d`、`z/` 的清理（由 `MP-47` 承接）；非目录重命名冲突（由 `MP-16`..`MP-19` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 输出 `z/d added in B^0 ...` 文案且 `y/d` 直接取另一侧内容、无冲突标记 | `src/command/merge/rename_merge.rs:1142`；`#1024` |
| `#1024` Expected：file location 文案、`Auto-merging y/d` 与 `CONFLICT (content): Merge conflict in y/d`（Git `t/t6423-merge-rename-directories.sh#76`） | `git@6de20f6092` `t/t6423-merge-rename-directories.sh` |

**Acceptance criteria:**

- [ ] AC1：输出 Git 的 file location 文案。
- [ ] AC2：输出对 `y/d` 的自动合并报告。
- [ ] AC3：输出 `y/d` 的内容冲突。
- [ ] AC4：`y/d` 含带路径标签的冲突块。
- [ ] AC5：对照：目录重命名后另一侧在原目录中真正新增的文件仍报告 Git 的 `added in` 文案（`#1024` Expected 1：普通 `added in B^0` 形式保留给真正的新文件）。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::transitive_dir_rename_conflict_reports` 承载 AC1–AC5；夹具 F-1024（`#1024` 复现，以对象级构造建立）：`O`：`x/d`=`1..10`、`z/b`=`b\n`、`z/c`=`c\n`（同 `t6423` 夹具，两侧都不改 `b`、`c` 的内容）；`A`：`z/` 重命名为 `y/`（`y/b`、`y/c`）且 `x/d` 追加 `11`；`B`：保留 `z/b`、`z/c`，删除 `x/`，新增 `z/d`=`1..10` 加 `eleven`；在 `A^0` 上 `libra merge -s recursive B^0`；对照夹具 F-1024N（另建仓库，Libra 命令构造）：`O`：`z/b`=`b\n`、`z/c`=`c\n`；`A`：`z/` 重命名为 `y/`；`B`：自 `O` 新增 `z/e`=`e\n`（无重命名来源的新文件）；在 `A^0` 上 `libra merge -s recursive B^0`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出（冲突，执行条件）；stdout 含一行恰为 `CONFLICT (file location): x/d renamed to z/d in B^0, inside a directory that was renamed in HEAD, suggesting it should perhaps be moved to y/d.`（`#1024`）。
- AC2：stdout 含一行恰为 `Auto-merging y/d`（`#1024` Expected）。
- AC3：stdout 含一行恰为 `CONFLICT (content): Merge conflict in y/d`（`#1024` AC2）。
- AC4：`y/d` 内容恰为 `1..10`（每行一个）后接 `<<<<<<< HEAD:x/d\n11\n=======\neleven\n>>>>>>> B^0:z/d\n`（`#1024` AC3）。
- AC5：F-1024N；合并以非 0 退出（冲突，执行条件）；stdout 含一行恰为 `CONFLICT (file location): z/e added in B^0 inside a directory that was renamed in HEAD, suggesting it should perhaps be moved to y/e.`（Git 2.54.0 预探：退出 1，`y/e` 记为 stage 3；无条件改用 `renamed to` 文案的实现即失败）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::transitive_dir_rename_conflict_reports)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::transitive_dir_rename_conflict_reports)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-54`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`file location`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=传递目录重命名冲突报告与内容冲突; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-54,DEP-MP-10; writeset=序列化于MP-54; release=independent; split-from=N/A; exception=N/A`

---

### Task MP-47: 传递目录重命名：`y/d` 的索引阶段与 `z/d` 清理（#1024）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `MP-20` 拆出：传递目录重命名冲突后 `y/d` 的 stage 1/2/3 分别为基准、HEAD 与另一侧的 blob；`z/d` 不留在工作树，索引中没有 `z/` 下的路径（`t6423` 13b）。

**Out of scope:** 冲突报告与 `y/d` 内容（由 `MP-20` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `#1024` 实测阶段正确但需在报告修复后保持；Git `t6423` 13b 另检查 `z/d` 缺失与已索引路径不含 `z/` | `#1024`；`git@6de20f6092` `t/t6423-merge-rename-directories.sh`（13b：`test_path_is_missing z/d`） |

**Acceptance criteria:**

- [ ] AC1：`y/d` 的 stage 1 为基准 blob。
- [ ] AC2：`y/d` 的 stage 2 为 HEAD 侧 blob。
- [ ] AC3：`y/d` 的 stage 3 为另一侧 blob。
- [ ] AC4：`z/d` 不留在工作树。
- [ ] AC5：索引中没有 `z/` 下的路径。
- [ ] ER-06/ER-06a 同卡强制门（不计入 G-03）：按 `Docs and compatibility impact` 逐文件交付，按「文档与网站验收命令」第 1–8 步验收。

**AC → 断言映射:**（`(new)` `command::merge_test::transitive_dir_rename_stages_and_paths` 承载 AC1–AC5；夹具 F-1024（`#1024` 复现，以对象级构造建立）：`O`：`x/d`=`1..10`、`z/b`=`b\n`、`z/c`=`c\n`（同 `t6423` 夹具，两侧都不改 `b`、`c` 的内容）；`A`：`z/` 重命名为 `y/`（`y/b`、`y/c`）且 `x/d` 追加 `11`；`B`：保留 `z/b`、`z/c`，删除 `x/`，新增 `z/d`=`1..10` 加 `eleven`；在 `A^0` 上 `libra merge -s recursive B^0`；「仓库快照 Σ」「HEAD 的父提交」「夹具提交名」（以字母命名的提交以同名轻量标签标记）「映射的执行仓库」按「验证执行协议」第 8 条定义；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按第 4 条在开工时以 `git@6de20f6092` 复核后硬编码）

- AC1：以非 0 退出（冲突，执行条件）；`libra rev-parse :1:y/d` 等于 `libra rev-parse O:x/d`（`#1024` AC4）。
- AC2：`libra rev-parse :2:y/d` 等于 `libra rev-parse A:x/d`（`#1024` AC4）。
- AC3：`libra rev-parse :3:y/d` 等于 `libra rev-parse B:z/d`（`#1024` AC4）。
- AC4：`z/d` 不存在（`t6423` 13b：`test_path_is_missing z/d`）。
- AC5：`libra ls-files -s -- z` 的 stdout 为空（`t6423` 13b：已索引路径不含 `z/`）。
- 文档 AC（不计入 G-03）→ 「文档与网站验收命令」第 1–8 步。

**Verification:**

- [ ] 卡特有用例（AC1–AC5）：`source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::transitive_dir_rename_stages_and_paths)'`（`(new)`，在 `tests/command/merge_test.rs` 新增；按「验证执行协议」第 1–4 条：`source .env.test && cargo nextest list --test command_test -E 'test(=command::merge_test::transitive_dir_rename_stages_and_paths)'` 须恰好列出该 1 条）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test compat_conflict_status_diff`（整 target，冲突状态报告（status/ls-files 阶段）；按「验证执行协议」第 2、3、5 条，先 `source .env.test && cargo nextest list --test compat_conflict_status_diff` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（`command::merge_test` 模块族；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）
- [ ] 共享回归组（不计入 G-03 VER）：`source .env.test && cargo nextest run --test command_test -E 'test(/^command::rebase_test::/)'`（`command::rebase_test` 模块族，守护与 `rebase` 共享的合并引擎；按「验证执行协议」第 2、3、5 条，先以同一过滤 `cargo nextest list` 记录 `N` ≥ 1）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1，且修改与 `rebase`/`cherry-pick` 共享的合并引擎（ER-13 T-1）` | **Dependencies:** `MP-20`, `DEP-MP-10`

**Implementation write set:** `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-manifest.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/src/generated/docs-search.ts`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/dist/`（网站第 4 步改写的被忽略生成产物，不提交）

**Release write set:** `Inherited` | **Files likely touched:** `src/command/merge/rename_merge.rs`, `tests/command/merge_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`（由 `--only merge` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字：`y/d`。逐文件语义核对：`docs/development/commands/merge.md` 与 `COMPATIBILITY.md`（`| merge |` 第 205、212 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；`docs/error-codes.md`：N/A——本卡不新增稳定码、不新增或修改退出码覆写（理由与核对见「全局工程约束」GC-05 条的「`docs/error-codes.md` N/A 口径」）。按「文档与网站验收命令」第 1–8 步逐文件交付并验收。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=传递目录重命名的索引阶段与路径清理; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=MP-20,DEP-MP-10; writeset=序列化于MP-20; release=independent; split-from=MP-20; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `merge.rs` 内既有单元测试（共享回归组） | `source .env.test && cargo nextest run --lib -E 'test(/^command::merge::tests::/)'` |
| 集成（卡特有） | 各卡 `(new)` 用例，见「追溯表」 | `source .env.test && cargo nextest run --test command_test -E 'test(=command::<module>_test::<fn>)'` |
| 集成（共享回归组） | `merge`/`pull`/`reflog`/`rebase`/`rev-parse`/`stash`/`remove` 模块族 | `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::/)'`（其余模块同形） |
| 兼容 | 历史默认配置、非交互控制、pull 策略标志、冲突状态、伪引用声明面、工作树写入 | `compat_config_history_defaults`、`compat_noninteractive_history_controls`、`compat_pull_strategy_flags_surface`、`compat_conflict_status_diff`、`compat_pseudo_ref_surface`、`compat_worktree_blob_writes`（各以 `source .env.test && cargo nextest run --test <target>` 运行） |
| 迁移 | `N/A`（无 SQLite schema 变更） | `N/A` |
| 安全 | 外部策略名白名单：拒绝含路径分隔符等的名字且不查找、不执行（`MP-35`）；无 shell 执行、按标准 `PATH`（含 `.`）查找（`MP-15`） | `command::merge_test::strategy_name_whitelist`、`command::merge_test::external_strategy_execution_contract` |
| 性能 | 仅比较合并影响路径（`MP-11`）、跳过未变更文件写入（`MP-12`） | `command::merge_test::dirty_worktree_overlap_only`、`command::merge_test::merge_preserves_mtime_for_unchanged_paths` |
| live/gated | 收口阶段双环境全量套件 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 卡特有用例 |
|---|---|---|---|---|
| MP-01 | `#996` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::ff_flags_last_one_wins` |
| MP-26 | `#990` | `src/command/merge.rs`、`src/command/history_config.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::commit_flag_and_merge_options_commit` |
| MP-33 | `#990, #985` | `src/command/merge.rs`、`src/command/history_config.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::squash_commit_validated_after_merge_options` |
| MP-27 | `#985` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::squash_no_ff_rejected_and_ff_only_allowed` |
| MP-28 | `#1015` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::strategy_with_strategy_option` |
| MP-02 | `#992` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::no_commit_fast_forwards` |
| MP-21 | `#1002` | `src/command/merge.rs`、`src/command/pull.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md` | `command::merge_test::ff_only_refusal_message_and_advice`、`command::pull_test::ff_only_refusal_message_and_advice` |
| MP-22 | `#1028` | `src/command/merge.rs`、`src/command/pull.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md` | `command::merge_test::conflict_exits_with_code_1`、`command::pull_test::conflict_exits_with_code_1` |
| MP-34 | `#1028` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::refusal_exit_codes_ff_1_three_way_2` |
| MP-40 | `#1028` | `src/command/pull.rs` | `docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md` | `command::pull_test::refusal_exit_codes_ff_1_three_way_2` |
| MP-03 | `#994` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::edit_failure_keeps_resumable_merge` |
| MP-52 | `#994` | `src/command/merge.rs`、`src/command/editor.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::edit_spawn_failure_keeps_resumable_merge` |
| MP-04 | `#999` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::into_name_and_suppress_dest` |
| MP-49 | `#999` | `src/command/merge.rs`、`src/command/merge_message.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::octopus_message_matches_fmt_merge_msg` |
| MP-05 | `#980` | `src/command/merge.rs`、`src/command/merge/output.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::squash_msg_body_and_conflicts` |
| MP-06 | `#1003` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md` | `command::pull_test::unborn_merge_reflog_initial_pull`、`command::pull_test::unborn_pull_reflog_initial_pull` |
| MP-29 | `#1003` | `src/command/reflog.rs` | `docs/commands/reflog.md`、`docs/commands/zh-CN/reflog.md`、`docs/development/commands/reflog.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/reflog.en.md` | `command::reflog_test::reflog_limit_no_trailing_blank_line` |
| MP-07 | `#981` | `src/command/merge.rs`、`src/command/merge/autostash.rs`、`src/command/merge/output.rs`、`src/command/pull.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md` | `command::merge_test::conflict_and_autostash_progress_on_stdout`、`command::pull_test::conflict_progress_on_stdout` |
| MP-08 | `#1004` | `src/command/merge.rs`、`src/command/merge/output.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::verify_signatures_good_line` |
| MP-32 | `#1004` | `src/command/merge.rs`、`src/command/history_config.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::verify_signatures_min_trust_level` |
| MP-09 | `#1001` | `src/command/merge.rs`、`src/command/merge/output.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::resolve_trivial_in_index_diagnostics` |
| MP-10 | `#991` | `src/command/merge/autostash.rs`、`src/command/merge.rs`、`src/command/commit.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md` | `command::merge_test::merge_autostash_applied_by_commit` |
| MP-43 | `#991` | `src/command/merge.rs`、`src/command/merge/autostash.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::merge_autostash_applied_by_continue` |
| MP-48 | `#991` | `src/command/merge/autostash.rs`、`src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md` | `command::merge_test::merge_autostash_apply_conflict_stored` |
| MP-39 | `#991` | `src/command/merge/autostash.rs`、`src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::merge_autostash_abort_and_ff_squash` |
| MP-41 | `#991` | `src/command/merge/autostash.rs`、`src/command/merge.rs`、`src/command/commit.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md` | `command::merge_test::merge_autostash_clean_three_way_squash` |
| MP-45 | `#981, #991` | `src/command/merge/autostash.rs`、`src/command/merge.rs`、`src/command/commit.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md` | `command::merge_test::merge_autostash_conflicted_three_way_squash` |
| MP-30 | `#991` | `src/internal/pseudo_ref.rs`、`src/utils/util.rs` | `docs/commands/rev-parse.md`、`docs/commands/zh-CN/rev-parse.md`、`docs/development/commands/rev-parse.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rev-parse.en.md` | `command::merge_test::merge_autostash_rev_parse_pseudo_ref` |
| MP-44 | `#991` | `src/command/stash.rs` | `docs/commands/stash.md`、`docs/commands/zh-CN/stash.md`、`docs/development/commands/stash.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/stash.en.md` | `command::merge_test::stash_show_merge_autostash` |
| MP-11 | `#993` | `src/command/merge.rs`、`src/command/merge/workdir.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::dirty_worktree_overlap_only` |
| MP-53 | `#993` | `src/command/merge.rs`、`src/command/merge/workdir.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::dirty_staged_refusal_and_conflict_abort` |
| MP-12 | `#1026` | `src/command/merge/workdir.rs`、`src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::merge_preserves_mtime_for_unchanged_paths` |
| MP-13 | `#998` | `src/command/pull.rs` | `docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md` | `command::pull_test::pull_rebase_ff_precedence_matrix` |
| MP-23 | `#998` | `src/command/pull.rs` | `docs/commands/pull.md`、`docs/commands/zh-CN/pull.md`、`docs/development/commands/pull.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/pull.en.md` | `command::pull_test::pull_rebase_explicit_repo_refspec` |
| MP-55 | `#1025` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::octopus_explicit_strategy_selection` |
| MP-14 | `#1025` | `src/command/merge.rs`、`src/command/merge/output.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::strategy_chain_fallback` |
| MP-35 | `#1027` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::strategy_name_whitelist` |
| MP-38 | `#1027` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::strategy_not_found_and_pending_external` |
| MP-15 | `#1027` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::external_strategy_execution_contract` |
| MP-51 | `#1027` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::external_strategy_structured_output_isolation` |
| MP-37 | `#1027` | `src/command/merge.rs`、`src/command/merge/state.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::external_strategy_exit_codes` |
| MP-42 | `#1025, #1027` | `src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::external_strategy_chain_rejected` |
| MP-16 | `#1011` | `src/command/merge/conflict.rs`、`src/command/merge/rename_merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::conflict_markers_carry_path_labels` |
| MP-17 | `#1007` | `src/command/merge.rs`、`src/command/merge/tree_merge.rs`、`src/command/merge/workdir.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::recursive_df_conflict_and_modify_delete` |
| MP-24 | `#1016` | `src/command/merge/rename_merge.rs`、`src/command/merge.rs`、`src/command/merge/workdir.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::rename_directory_conflict_newfile_head` |
| MP-18 | `#1009` | `src/command/merge/rename_merge.rs`、`src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::rename_delete_keeps_plain_content` |
| MP-25 | `#1010` | `src/command/merge/rename_merge.rs`、`src/command/merge/workdir.rs`、`src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::rename_rename_df_conflict_materializes` |
| MP-46 | `#1010` | `src/command/merge/rename_merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::rename_rename_df_conflict_reports` |
| MP-50 | `#1018` | `src/command/remove.rs` | `docs/commands/rm.md`、`docs/commands/zh-CN/rm.md`、`docs/development/commands/rm.md`、`docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`docs/error-codes.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/rm.en.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::remove_test::rm_unmerged_path_removes_all_stages` |
| MP-19 | `#1018` | `src/command/merge/rename_merge.rs`、`src/command/merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::rename_1to2_keeps_source_stage1` |
| MP-54 | `#1021` | `src/command/merge/rename_merge.rs`、`src/command/merge/conflict.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::rename_1to2_binary_side_blobs` |
| MP-20 | `#1024` | `src/command/merge/rename_merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::transitive_dir_rename_conflict_reports` |
| MP-47 | `#1024` | `src/command/merge/rename_merge.rs` | `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md` | `command::merge_test::transitive_dir_rename_stages_and_paths` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划文本 Claude（Subagent）与 Codex 双审对同一 SHA-256 `VERDICT: PASS`（「审阅合同」） | 本计划「修订历史」与「Codex review log」 | 修订计划文本后重审 |
| M1 | Phase 1（21 张卡）全部 `done/complete` | 各卡 release tag、D 组与网站第 7 步记录 | 发布后按 ER-10 新建 `FIX-*` 前滚新 patch + 降级指引 |
| M2 | Phase 2（13 张卡）全部 `done/complete` | 同 M1 | 同 M1 |
| M3 | Phase 3（8 张卡）全部 `done/complete` | 同 M1 | 同 M1 |
| M4 | Phase 4（11 张卡）全部 `done/complete`，且「完成判据」全量收口门全绿 | 全量 nextest 记录写入「实施证据汇总」 | 前滚修复新开 `FIX-*` 卡 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `--edit` 编辑器失败（`MP-03`） | 待决合并状态与已合并索引/工作树 | 用户执行 `libra commit`、`--continue` 或 `--abort` | 静默丢弃合并状态 |
| autostash 应用冲突（`MP-48`） | autostash 保存进 `stash list` | 用户 `libra stash pop` 手工解决 | 静默丢弃未暂存修改 |
| 多策略全部未处理（`MP-14`） | 无（合并前索引/工作树原样恢复） | 退出 2 + `No merge strategy handled the merge.` | 残留半成品树或误用退出码 |
| 外部策略挂起（`MP-15`） | 子进程仍在运行；中断后可能留下策略已作的索引/工作树改动（含策略新建、未加入索引的未跟踪文件），且没有待决合并状态（与 Git 相同：Git 在策略返回后才写合并状态，`builtin/merge.c:1863-1867`） | 用户以终端中断（前台进程组信号，与 Git 一致）；随后以 `libra status --short` 检查。① 已跟踪路径（含策略经 `libra add` 加入索引的新路径）：`libra restore --staged --worktree --source HEAD -- .` 把索引与工作树恢复到 HEAD（默认 no-overlay 模式同时删除 HEAD 中不存在的已跟踪路径，`docs/commands/restore.md:45`）；合并前的未提交修改会一并被丢弃，须由用户事先保存。② 未跟踪路径：`restore` 不处理未跟踪文件，策略新建而未加入索引的文件在 ① 之后仍以 `??` 列出；用户对照策略脚本的写入行为与自己在中断前已有的未跟踪文件，逐个删除策略创建的路径，且路径一律按字面量处理（`--literal-pathspecs`，否则含 `*`、`?`、`[`、`:` 等元字符的文件名会作为 pathspec 匹配到其它未跟踪文件，`src/command/clean.rs:49-59,281-289`）：文件先执行 `libra clean --literal-pathspecs -n -- '<path>'`，确认输出恰为一行 `Would remove <path>` 后再执行 `libra clean --literal-pathspecs -f -- '<path>'`；目录先以 `libra status --literal-pathspecs --short --untracked-files=all -- '<dir>'` 列出其下全部未跟踪文件（检查与删除使用同一字面量匹配规则；短格式每行为两个字符的状态列 `??`、一个空格与路径，确认每一行的状态列都是 `??`、去掉前三个字符后的路径都以 `<dir>/` 开头），确认它们全部由策略创建后才执行 `libra clean --literal-pathspecs -f -d -- '<dir>'`，否则只按上述文件步骤逐个删除其中由策略创建的文件；不得不带路径或不带 `--literal-pathspecs` 执行 `libra clean -f`，以免删除用户既有的未跟踪文件（Libra 0.30.207 实测：`clean --literal-pathspecs -n -- 'a*'` 只列出 `a*`，不带该选项时同时列出 `ab`）。不使用 `libra merge --abort`（它需要待决合并状态，`src/command/merge.rs:4552`） | Libra 自行改写或放弃已写入状态；以不带路径或非字面量 pathspec 的 `clean` 删除用户既有未跟踪文件 |
| 降级到不认识 `external_strategy` 的旧二进制（`MP-37`） | 待决合并状态 | 降级前先 `libra merge --continue` 或 `--abort`（发布说明写明）；旧二进制的 `--restart` 会以默认策略重放 | 在旧二进制上依赖 `--restart` 重放外部策略 |
| D 组 CI/release 失败 | 已推送主分支提交 | 按 ER-04/ER-10 前滚修复发新 patch | 回退/强推已发布历史 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| 冲突与拒绝退出码由 128 改为 1/2 导致既有脚本或用例断言失败 | 中 | `with_exit_code` 只作用于 merge/pull 映射处；同卡更新受影响用例与 `docs/error-codes.md`；每卡 T-1 全量 | `MP-22`, `MP-34`, `MP-40`, `MP-14`, `MP-38`, `MP-37` |
| 旧二进制读取含 `external_strategy` 的状态时忽略该字段，`--restart` 以默认策略重放 | 低：仅 Libra 专有 `--restart` 跨降级 | `ADR-MP-05`；发布说明要求降级前完成或中止合并；`strategy` 不写新取值，`--continue`/`--abort`/`commit` 不受影响 | `MP-55`, `MP-37` |
| 外部策略无超时，脚本挂起使 `merge` 一直等待 | 低：与 Git 行为一致，且需用户显式 `-s <name>` | `ADR-MP-02`；用户可中断；不新增 Git 没有的超时机制 | `MP-15` |
| `-s` 接受任意字符串后路径注入执行任意程序 | 高（若校验缺失） | 白名单在任何 `PATH` 查找前校验；`MP-35`/`MP-38` 发布时外部执行尚未开放；`MP-35` AC1–AC6 以表驱动覆盖全部 30 个白名单外 ASCII 可打印字符、制表符与非 ASCII 代表字符 `é`（实现按白名单正向判定，任何其它非 ASCII 字符同样被拒绝），以及 `.`、`..`、空名 | `MP-35`, `MP-38`, `MP-15`, `MP-37` |
| D/F 与 rename 边界修复引入回归 | 高：损坏工作区或索引阶段 | 每卡跑 `compat_conflict_status_diff` 与 `merge_test`/`rebase_test` 模块族，期望取自 Git 测试（`t6402`/`t6422`/`t6423`） | `MP-16`..`MP-20`, `MP-24`, `MP-25`, `MP-46`, `MP-47` |
| 与同批计划共享写集（`COMPATIBILITY.md`、`merge.rs`、`util.rs`、`commit.rs`、网站生成产物）并发写入 | 中 | `DEP-MP-10` 串行窗口；`DEP-MP-11` 顺序前置 | 全部任务卡 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 脏工作区重叠检查 | `O(合并影响路径 + 改动路径)` | 仅比较受合并影响路径 | 不增加全仓内容哈希扫描 | `MP-11` |
| 工作区写入跳过未变更文件 | `O(合并影响路径)` | 跳过与 HEAD 相同 OID/模式的文件写入 | 保留原有 `mtime` | `MP-12` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 的 `merge`（205、212）、`pull`（225）、`reflog`（249）、`commit`（209）、`rev-parse`（217、277）、`stash`（193）、`rm`（186）行已随相关任务卡原地同步，行数不变。
- [ ] `docs/commands/{merge,pull,reflog,commit,rev-parse,stash,rm}.md` 与 `docs/commands/zh-CN/` 对应页、`docs/development/commands/` 对应页已同步。
- [ ] `docs/error-codes.md`「Per-command exit-code overrides」已登记 `MP-22`、`MP-34`、`MP-40`、`MP-14`、`MP-38`、`MP-37` 的退出码覆写。
- [ ] `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{merge,pull,reflog,commit,rev-parse,stash,rm}.en.md` 已按各卡网站步骤同步并部署核验。
- [ ] `Cargo.toml` `[[test]]`、`tests/INDEX.md`、`tests/compat/README.md`：`N/A`（不新增或重命名 test target）。
- [ ] `plan-long.md` 与 `plan-status.md` 由协调者统一登记。

## Codex review log

> 2026-11-17 各轮为历史记录，原样保留；2026-11-18 起为计划提交门的 Claude（Subagent）与 Codex 双审，按「审阅合同」两方均对同一受审 SHA-256 `PASS` 方可提交。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 (Claude) | `plan-20261117-merge-pull.md` 初稿 | `FAIL` | P1-1..P1-7 均在 R2/R3 关闭 | 已修复 | Claude R1 review |
| R1 (Codex) | `plan-20261117-merge-pull.md` 初稿 | `FAIL` | P1 均在 R2/R3 关闭 | 已修复 | Codex R1 review |
| R2 (Claude) | `plan-20261117-merge-pull.md` R2 | `FAIL` | 5 项 P1（空 unit 过滤器、`MP-10` `pseudo_ref.rs`/T-1/`commit` 文档、跨计划 `commit-signing` 关系、Git 2.56.0 pin、`state.rs`）均在 R3 关闭 | P2 全部修复 | Claude R2 review |
| R2 (Codex) | `plan-20261117-merge-pull.md` R2 | `FAIL` | 3 项 P1（`#1027` PATH `.`、普通/squash autostash 合同、`G-04` `S`→`M` 与拆卡）均在 R3 关闭 | P2（谓词计数、G-09 双向标记）已修复 | Codex R2 review |
| R3 (Claude) | `plan-20261117-merge-pull.md` R3 | `FAIL` | 5 项 P1（D 组真实 job 名、`CSR-21` 依赖、`DEP-MP-02` 去环、`MP-29` 非空行测试、`MP-22` 退出码与 `pull`）均在 R4 关闭 | P2（`MP-08`/`MP-15`/审计表 `landing`）已修复 | Claude R3 review |
| R3 (Codex) | `plan-20261117-merge-pull.md` R3 | `FAIL` | 1 项 P1（`MP-26` 同步包含 `--squash --commit` 拒绝）在 R4 关闭 | P2（`MP-27` `#985`、`MP-12` `M`+文档）已修复 | Codex R3 review |
| R4 (Codex) | `plan-20261117-merge-pull.md` R4 | `FAIL` | — | P2（`MP-27` 补 `--squash --ff-only` 具名验证、`MP-26` 卡内 `Granularity` 同步）在 R5 关闭 | Codex R4 review |
| R5 (Codex) | `plan-20261117-merge-pull.md` R5 | `FAIL` | — | P2（`#985` Git 诊断格式 `'--commit.'`/`'--no-ff.'` 句号位置）在 R6 关闭 | Codex R5 review |
| R6 (Claude) | `plan-20261117-merge-pull.md` R6 | `FAIL` | 4 项 P1（`DEP-MP-03` 死锁、`ADR-MP-01`/`MP-22` 退出码矛盾、`MP-10` `util.rs`/`COMPATIBILITY.md`、`issues/974,978,984` 协调）均在 R7 关闭 | P2（审计表 `landing`、`CS-14a`/`CS-03`、Phase 1 入口、追溯表、R6 修订行）在 R7 关闭 | Claude R6 review |
| R6 (Codex) | `plan-20261117-merge-pull.md` R6 | `FAIL` | — | P2（`MP-15` 超时与 kill+wait 测试）在 R7 关闭 | Codex R6 review |
| R7 (Claude) | `plan-20261117-merge-pull.md` R7 | `PASS` | 无 P0/P1 | 5 项 P2（10 张卡 `landing=1`、`MP-06/07` `pull_test`、#984、`Release write set 默认`、`MP-15` 环境变量文档）在 R8 关闭 | Claude R7 review |
| R7 (Codex) | `plan-20261117-merge-pull.md` R7 | `FAIL` | 1 项 P1（ER-13 全量门必须重跑至全绿）在 R8 关闭 | P2（`MP-15` 默认不限时以兼容 Git）在 R8 关闭 | Codex R7 review |
| R8 (Codex) | `plan-20261117-merge-pull.md` R8 | `FAIL` | 1 项 P1（`Release write set 默认` 补齐 `install.sh`, `install.ps1`）在 R9 关闭 | — | Codex R8 review |
| R9 (Claude) | `plan-20261117-merge-pull.md` R9 | `FAIL` | 2 项 P1（`#1028` 按路径划分退出码 1/2、`MP-16..20/24/25` 标 `T-1` 与 `rebase_test::`）均在 R10 关闭 | P2（`Release write set` 去重、`DEP-MP-05` 硬阻塞、`MP-07` 文本、AC 重算）在 R10 关闭 | Claude R9 review |
| R9 (Codex) | `plan-20261117-merge-pull.md` R9 | `FAIL` | — | P2（`MP-07` `pull_test`、`MP-05` `commit.cleanup=scissors`）在 R10 关闭 | Codex R9 review |
| R11 (Claude) | `plan-20261117-merge-pull.md` R10/R11 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R12 重审 | — | Claude R11 review |
| R11 (Codex) | `plan-20261117-merge-pull.md` R10/R11 | `FAIL` | 0 项 P0/P1 | 1 项 P2（`MP-16..20, MP-24, MP-25` `Verification` 补齐 `rebase_test::` 与 `VER=3/8`）在 R12 关闭 | Codex R11 review |
| R12 (Claude) | `plan-20261117-merge-pull.md` R12 | `FAIL` | 3 项 P1（`ADR-MP-04` 保留 `merge-autostash.json`、拆出 `MP-30`/`MP-31`/`MP-32` 覆盖 `pseudo_ref_surface` 与 `AC<=8`）均在 R13 关闭 | P2（`MP-05` `output.rs`、`MP-26/27`、`修订历史`、`默认覆盖`、`MP-11`、`MP-14/22`、`Cargo.toml [[test]]`）在 R13 关闭 | Claude R12 review |
| R12 (Codex) | `plan-20261117-merge-pull.md` R12 | `FAIL` | 0 项 P0/P1 | 1 项 P2（`codeql.yml` `push` 补 `release/**`）在 R13 关闭 | Codex R12 review |
| R13 (Claude) | `plan-20261117-merge-pull.md` R13 | `FAIL` | 3 项 P1（修复 `MP-30`/`MP-31` 自依赖、解决 `MP-26`/`I978-01` `--no-commit --squash` 冲突、拆出 `MP-33`/`MP-34` 守 `AC<=8`）均在 R14 关闭 | P2（`MP-21` 新测试、`MP-14`/`31` `error-codes.md`、`128` 退出码、`34 张`、ADR 顺序）在 R14 关闭 | Claude R13 review |
| R13 (Codex) | `plan-20261117-merge-pull.md` R13 | `FAIL` | 1 项 P1（`MP-30` 与 `MP-31` 自依赖修复为 `MP-10` 与 `MP-15`）在 R14 关闭 | — | Codex R13 review |
| R14 (Claude) | `plan-20261117-merge-pull.md` R14 | `FAIL` | 6 项 P1（`MP-33`/`MP-34`/`MP-11`/`MP-16` `Dependencies` 对齐 DAG、`MP-14`/`MP-31` 因 `error-codes.md` 标 `T-1`）均在 R15 关闭 | P2（`L158` `MP-30`、`Phase 4` `MP-31`、`MP-31` 退出码 `2`、`MP-07` 单轴说明）在 R15 关闭 | Claude R14 review |
| R14 (Codex) | `plan-20261117-merge-pull.md` R14 | `FAIL` | 1 项 P1（`MP-33` 自依赖修复为 `MP-26`）在 R15 关闭 | — | Codex R14 review |
| R15 (Claude) | `plan-20261117-merge-pull.md` R15 | `FAIL` | 3 项 P1（`MP-26` `history_config.rs` 标 `T-1`、`MP-15` 拆出 `MP-35` 守 `AC<=8`、新增 `DEP-MP-08` 显式对接 `SA-05`/`SA-07d`）均在 R16 关闭 | P2（`DEP-MP-02` `REL-CS-00`、`DEP-MP-09`、`MP-05/07/08/14/27/34` AC 计数、`R16`、`MP-06` `pull` 文档、`MP-15` `Security`、`完成判据`）在 R16 关闭 | Claude R15b review |
| R15 (Codex) | `plan-20261117-merge-pull.md` R15 | `FAIL` | 1 项 P1（`MP-26` 同步加 `--squash --commit` 拒绝 128 避免中间版本放行）在 R16 关闭 | P2（`MP-33` 写集补 `issues/978.md` 与校验）在 R16 关闭 | Codex R15b review |
| R16 (Claude) | `plan-20261117-merge-pull.md` R16 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R17 重审 | — | Claude R16 review |
| R16 (Codex) | `plan-20261117-merge-pull.md` R16 | `FAIL` | 1 项 P1（将 `MP-35` 前置至 `MP-15` 之前先落地安全校验与 `MergeState` 兼容）在 R17 关闭 | — | Codex R16 review |
| R17 (Codex) | `plan-20261117-merge-pull.md` R17 | `FAIL` | 1 项 P1（将 `MP-14` 多策略组合移入 `MP-15`，`MP-35` 仅测校验器与 `MergeState` 兼容）在 R18 关闭 | P2（澄清 `MP-26` `Out of scope` 与 `CS-14` 同步）在 R18 关闭 | Codex R17 review |
| R18 (Claude) | `plan-20261117-merge-pull.md` R18 | `ERROR` | 网络连接中断（`ECONNRESET`），合并至 R19 重审 | — | Claude R18 review |
| R18 (Codex) | `plan-20261117-merge-pull.md` R18 | `FAIL` | 2 项 P1（`MP-33` 同步更新 `978.md` 的 `ADR-I978-02`/测试/`完成判据` 消除内部矛盾；`MP-15` 与 `MP-31` 纳为 `REL-MP-01` 家族卡与 `MP-36` 发布点，确保首个外部策略公开发布含 `GC-09` 超时与进程组回收）均在 R19 关闭 | — | Codex R18 review |
| R19 (Claude) | `plan-20261117-merge-pull.md` R19 | `ERROR` | 网络中断（未成行） | — | — |
| R19 (Codex) | `plan-20261117-merge-pull.md` R19 | `FAIL` | 2 项 P1（`MP-15`/`MP-31` 家族子卡 `C/D coverage from` 改为 `MP-36`、`MP-33` 纳入 `src/internal/history_config.rs` 与 `I978-01` 代码/测试改写）均在 R20 关闭 | P2（`MP-15`/`MP-31` `Full-suite trigger` 改为 `N/A`）在 R20 关闭 | Codex R19 review |
| R20 (Claude) | `plan-20261117-merge-pull.md` R20 | `ERROR` | 网络中断（未成行） | — | — |
| R20 (Codex) | `plan-20261117-merge-pull.md` R20 | `FAIL` | 2 项 P1（澄清 `MP-15` 退出码 `2` 在 `MP-14` 多策略链中的回退恢复规则；`MP-31` 补 `process_group(0)` 与超时/父进程中断时对整个进程组 `kill(-pgid, SIGKILL)`+`waitpid` 回收后代）均在 R21 关闭 | P2（`MP-15` 补 `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` 互操作脚本验证）在 R21 关闭 | Codex R20 review |
| R21 (Claude) | `plan-20261117-merge-pull.md` R21 | `ERROR` | 网络中断（`ECONNRESET`），合并至 R22 重审 | — | Claude R21 review |
| R21 (Codex) | `plan-20261117-merge-pull.md` R21 | `FAIL` | 3 项 P1（修正 `MP-31` 进程回收契约为 `waitpid(child_pid)` 回收直接子进程+脱离组说明、补充 `MP-31` Windows Job Object 实现策略、修正 `MP-33` 写集为真实 `src/command/history_config.rs`）均在 R22 关闭 | P2（更新评审表 `38 张卡` 与 `35/35 M + 1 S` 统计）在 R22 关闭 | Codex R21 review |
| R1 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `41caa36e25e788e195da900686a33e2b77ec1b9069b77636984646265c5fc678` | `FAIL` | 7×P1：`MP-09` `-q` 与全局 quiet 契约矛盾、`MP-07` `--json` 失败断言不可满足、`MP-22` AC4 双谓词、`MP-04` 未探测却标「预探」、`MP-13` 未登记被取代的既有用例、pull 夹具未用既有 helper（`MP-06` 等） | 无 | 2026-11-18 14:33:15 UTC 修订 |
| R1 (Codex gpt-6.1-terra, 4 分区) | 全文，受审 SHA-256 `41caa36e25e788e195da900686a33e2b77ec1b9069b77636984646265c5fc678` | `FAIL` | A 5×P1（`-X` argv 矛盾、`MP-35`/`MP-14` 合并谓词、快进 squash autostash 时机、安全矩阵措辞）；B 8×P1（配置值需 `--`、`MP-26` AC4 不可区分、F-FF 未定义、`MP-33` 双谓词、`MP-27`/`MP-22` 非全量快照、`MP-34` 缺 pull 三方、`MP-04` 只测 main）；C 6×P1（`MP-05` commit 种子、`MP-08` 签名者、`MP-32` 大小写、`MP-10` sidecar 清理、`MP-11` 非全量快照、`MP-13`/`MP-23` 本地内容丢失）；D 8×P1（`MP-35` 白名单覆盖、`MP-37` 工作树回滚夹具与双谓词、`MP-20`/`MP-25`/`MP-17`/`MP-18`/`MP-16` 断言不足、退出码清单遗漏 `MP-37`） | 无 | 2026-11-18 14:33:15 UTC 修订 |
| R2 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `38a54753b3888e61fbce37feb7c2843de7275f7157bdb32a0de298788f519d71` | `FAIL` | 1×P1：`MP-11` AC6 自引用（应为 AC5） | P2：`MP-17` ls-files 只比行数（已收紧为全文） | 2026-11-18 14:39:06 UTC 修订 |
| R2 (Codex gpt-6.1-terra, 4 分区) | 全文，受审 SHA-256 `38a54753b3888e61fbce37feb7c2843de7275f7157bdb32a0de298788f519d71` | `FAIL` | A 1×P1（Σ 漏 `MERGE_MSG`/`COMMIT_EDITMSG`）；B 4×P1（`MP-04` 多值/清空不可区分、squash 暂存只比路径、`MP-28` `-Xours` 未证合并发生、`MP-33` 全文无矛盾与历史行冲突）；C 5×P1（夹具身份与 helper 不符、`MP-07` 漏 `Auto-merging`、`MP-11` 自引用、`MP-12` 条件未测、`MP-39` 缺干净三方 squash）；D 4×P1（`MP-35` 白名单覆盖不全、`MP-14`/`MP-17` 映射含 AC 外断言、`MP-16` 合并谓词） | 无 | 2026-11-18 14:39:06 UTC 修订 |
| R3 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `395dfd787dd4ad9d92a3338a7b14821bd33d6185cde4f779daf3e45971ed508d` | `FAIL` | 3×P1：`MP-18` AC4、`MP-25` AC1 合并谓词，`MP-30` AC3 映射含 AC 外回归断言 | P2：若干映射以「且」连接执行条件（已改写） | 2026-11-18 14:46:22 UTC 修订 |
| R3 (Codex gpt-6.1-terra, 4 分区) | 全文，受审 SHA-256 `395dfd787dd4ad9d92a3338a7b14821bd33d6185cde4f779daf3e45971ed508d` | `FAIL` | A 3×P1（`MP-15` 开放执行却无失败处理、Σ 漏 `FETCH_HEAD`、风险登记夸大非 ASCII 覆盖）；B 3×P1（`MP-22` 未处理既有 128 断言与冲突阶段、`MP-03` 未证合并内容）；C 4×P1（`MP-10` 未测同文件三方应用、stash 身份、`stash show` 未交付、`MP-12` AC/映射不一致）；D 3×P1（`MP-15` 树不可区分、`MP-37` 只测回绕文案、`MP-19`/`MP-20` 合并阶段谓词） | 无 | 2026-11-18 14:46:22 UTC 修订 |
| R4 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `b4e851e05b6cd86761db7ebaa4a02feb7ecfd9b1632105cc219fe784afb0fd1f` | `FAIL` | 1×P1：外部策略脚本需调用 `libra` 修改索引，`MP-15` 未要求等待期间不持锁 | 无 | 2026-11-18 14:52:01 UTC 修订 |
| R4 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `b4e851e05b6cd86761db7ebaa4a02feb7ecfd9b1632105cc219fe784afb0fd1f` | `FAIL` | A 1×P1（Σ 未含配置/待决操作表与目录）；B 2×P1（`MP-28` 未查提交内容、`MP-21` 只查建议首行）；C 4×P1（`MP-39`/`MP-41` 夹具不能区分未做 autostash、abort 非全量快照、`MP-07` JSON 契约不一致）；D 5×P1（`MP-15`/`MP-42` 只比文件名、`MP-42` 回绕非全量、`MP-12` 未证未重写、`MP-30` 检索范围过宽、`MP-13` AC 外断言）；E 3×P1（`MP-25` 只排除 `LBR-IO-002`、`MP-24`/`MP-25` AC 与映射不一致、`MP-20` 夹具内容未定） | 无 | 2026-11-18 14:52:01 UTC 修订 |
| R5 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `06421d3ca87b9e0d87c347b90ff2c7e5268882e8cb5c9443f01f04e3b5e72f55` | `PASS` | 无 | P2：`MP-10` AC7 依赖 `stash show -p` 与 `diff` 输出头一致（登记残余风险） | — |
| R5 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `06421d3ca87b9e0d87c347b90ff2c7e5268882e8cb5c9443f01f04e3b5e72f55` | `FAIL` | A 2×P1（网站窗口条件矛盾、ADR-MP-05 单元用例表述）；B 3×P1（`MP-21` 旧文案断言与写集、建议块不完整、`MP-22` 漏 squash 冲突）；C 2×P1（`MP-39` 冲突后行号、`MP-41` 未验 sidecar）；D 5×P1（`MP-15` 契约推迟、`MP-42` 回绕非全量且双谓词、`PATH=.` 未证执行、`MP-13`/`MP-23` 缺组合、`MP-23` AC 外断言）；E 3×P1（`MP-20` 漏 `z/d`、`MP-25` 合并谓词、冲突调用缺非 0 条件） | 无 | 2026-11-18 14:58:04 UTC 修订 |
| R6 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `4643e8979ccdf97da772112a1769399e615621e81d6fd2b60ad9ed66db145b6e` | `FAIL` | 1×P1：`DEP-MP-10` 网站窗口释放点与第 2 步持有范围矛盾 | 无 | 2026-11-18 15:03:28 UTC 修订 |
| R6 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `4643e8979ccdf97da772112a1769399e615621e81d6fd2b60ad9ed66db145b6e` | `FAIL` | A 1×P1（网站窗口释放矛盾）；B 2×P1（`MP-02` 既有 compat 用例、`MP-21` 关闭建议断言不全）；C 3×P1（sidecar 断言未替换 OID、`MP-05` `MERGE_MSG` 表述与锚点）；D 2×P1（`MP-30` compat 解析锚点、`MP-15` 缺 `sub`）；E 3×P1（`MP-19` 无常规解决路径、`MP-25` 缺两条 D/F 报告、`MP-20` 缺 `Auto-merging`） | P2：`MP-04` 未覆盖 `-F`（登记残余风险，`-F` 与 `-m` 同走显式消息分支） | 2026-11-18 15:03:28 UTC 修订 |
| R7 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `5f3ee320b01011e7861d845bcb3e59751e8c1e7bb62c9ab29c616fca3755ee71` | `FAIL` | 1×P1：`MP-19` F-1021 未验 stage 1 | 无 | 2026-11-18 15:07:57 UTC 修订 |
| R7 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `5f3ee320b01011e7861d845bcb3e59751e8c1e7bb62c9ab29c616fca3755ee71` | `FAIL` | A 1×P1（链中退出 1 候选）；B 1×P1（`MP-02` `--ff-only` 用例改写矛盾）；C 2×P1（G-09 位置——驳回，见修订历史；`MP-10`/`MP-43` 未验双父）；D 3×P1（`MP-30` 既有单元测试、`MP-23` 内容断言可在变基前通过、`MP-15` 只验退出 2 回滚）；E 2×P1（`MP-19` 解决内容、`MP-25` 正向冲突证据） | P2：外部策略中断后的恢复未演示（风险登记已述与 Git 一致）；`MP-03` 编辑器启动失败另一路径（`GIT_EDITOR=false` 覆盖非 0 退出，启动失败走同一 `EditorError` 分支，登记残余风险） | 2026-11-18 15:07:57 UTC 修订 |
| R8 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `bc80eb5d951c58b328c707eecc43df55906b6e86e49142657f3b93dd375e8494` | `FAIL` | 1×P1：`MP-42` 的数值退出码 AC 不在允许名单（随 `MP-42` 改写消除） | 无 | 2026-11-18 15:24:49 UTC 修订 |
| R8 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `bc80eb5d951c58b328c707eecc43df55906b6e86e49142657f3b93dd375e8494` | `FAIL` | A 1×P1（链中候选重跑与末位结果复用）；B 3×P1（`MP-33` AC8 映射超出 AC、`MP-02` AC2 双谓词、`MP-04` 未验 octopus 消息）；C 5×P1（`MP-06` 夹具推进上游、`MP-43`/`MP-39` 承接卡错指、`MP-41` 未验索引、`MP-41`/`MP-45` 缺 `commit` 文档）；D 4×P1（`MP-11`/`MP-12` 双谓词、`MP-42` 未验候选结果、`MP-42` 退出码名单）；E 1×P1（`MP-19` 含两处公开行为） | B 1×P2（`MP-03` 编辑器启动失败未单独覆盖，保留为残留 P2） | 2026-11-18 15:24:49 UTC 修订 |
| R9 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `ce0ee67e5f4580962a24a36425034a9c97554046b5163174e5ac1a417c683d7a` | `FAIL` | 1×P1：`MP-42` AC5 未证明外部策略未运行 | 无 | 2026-11-18 15:36:56 UTC 修订 |
| R9 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `ce0ee67e5f4580962a24a36425034a9c97554046b5163174e5ac1a417c683d7a` | `FAIL` | A 1×P1（拆出卡相邻规则与 `MP-35`/`MP-38`/`MP-15` 顺序不符）；B 4×P1（`MP-26` 缺配置对照、`MP-27`/`MP-34` 多调用合并为一条 AC、fine-exit 口径与卡矛盾）；C 1×P1（`MP-09` 未验合并树）；D 3×P1（`MP-15` 未覆盖 `#1027` 结果树、`MP-42` 无法证明内置策略未尝试、`MP-35` 只验 `d/x` 未执行）；E 4×P1（`MP-19` 文本/二进制规则、被取代的既有断言、`MP-50` 缺 stage-1-only、`MP-19` 缺 G-09 标记） | 无 | 2026-11-18 15:36:56 UTC 修订 |
| R10 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `fdc21929b5eea4155bf23d959c2df4c9af9b24c36595322e6dceab68f2a57ec3` | `FAIL` | 2×P1：`MP-09` AC5 文字与映射矛盾、`MP-07` 承接卡错指 | 1×P2：`MP-04` AC8 与 `MP-32` AC1/AC2 依赖「同类全称断言」口径 | 2026-11-18 15:42:38 UTC 修订 |
| R10 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `fdc21929b5eea4155bf23d959c2df4c9af9b24c36595322e6dceab68f2a57ec3` | `FAIL` | A 1×P1（16 张卡 `landing` 计数）；B 1×P1（`MP-02` 未验 `merge.ff=false`）；C 2×P1（`MP-09` AC5、`MP-07` 承接卡）；D 2×P1（`MP-15` 等价声明与工作树、`MP-38`/`MP-42` 以提示行缺失证明未尝试）；E 2×P1（`MP-25` 依赖 `#1017` 的 `ls-files -o`、`MP-19` AC8 未验工作树） | A 1×P2（Σ 对 operation 表的排除需复核）；B 1×P2（`MP-03` 编辑器无法启动未单独覆盖） | 2026-11-18 15:42:38 UTC 修订 |
| R11 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `d7aef206e84e24da5a42d86cea5254ac13bce059786e333380b6c9bec03289a0` | `FAIL` | 2×P1：Σ(a) 未排除 operation-v2 头/日志与缓存表、Σ(c) 与精确工作树断言未排除测试 HOME | 无 | 2026-11-18 15:48:37 UTC 修订 |
| R11 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `d7aef206e84e24da5a42d86cea5254ac13bce059786e333380b6c9bec03289a0` | `FAIL` | A PASS；B 1×P1（`MP-02` 工作树断言含测试 HOME）；C 2×P1（`MP-07` JSON 用例未确认冲突、`MP-39`/`MP-45` 未确认冲突）；D 2×P1（`MP-13` AC2 夹具复用矛盾、`MP-38` AC6 顺序谓词）；E 1×P1（`MP-25` 清单漏目录与类型） | A 1×P2（`sqlite_sequence` 随被排除表变化） | 2026-11-18 15:48:37 UTC 修订 |
| R12 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `b05405d19f29229eb92ddd3490ccff41df80b474da13f7cc97388eb7fce9cb04` | `FAIL` | 4×P1：Σ(a) 列入已删除表、失败脚本 `read-tree` 未限定、`MP-50` 字面量 `<k>`、自动暂存卡只验 `file` | 无 | 2026-11-18 15:53:42 UTC 修订 |
| R12 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `b05405d19f29229eb92ddd3490ccff41df80b474da13f7cc97388eb7fce9cb04` | `FAIL` | A 1×P1（Σ(a) 已删除表）；B 2×P1（`MP-03` 编辑器无法启动、`MP-04` 未验分离 HEAD）；C 1×P1（`MP-10`/`MP-43`/`MP-41` 工作树只验 `file`）；D 1×P1（失败脚本 `read-tree` 未限定）；E 1×P1（`MP-50` 字面量 `<k>`） | D 1×P2（`MP-13` 的 `pull.rebase=true` + `--ff` 未单独验收，保留为残留 P2） | 2026-11-18 15:53:42 UTC 修订 |
| R13 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `d1c6c862d615c2334b5f602ba85a865c3fc1919731e900ef5561445ed6628090` | `FAIL` | 3×P1：`MP-52` 夹具未触发进程创建失败、`MP-50` 未同步 merge 文档、`MP-45` 未调和 `#981` AC4 | 无 | 2026-11-18 15:58:17 UTC 修订 |
| R13 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `d1c6c862d615c2334b5f602ba85a865c3fc1919731e900ef5561445ed6628090` | `FAIL` | A 1×P1（GC-MP-01 JSON 约束与 `MP-07` 失败合同矛盾）；B 1×P1（`MP-52` 夹具）；C 1×P1（`MP-45` 与 `#981` AC4 及提示）；D 1×P1（`MP-51` 未验 `--machine` 输出格式）；E 1×P1（`MP-50` 未同步 merge 文档） | 无 | 2026-11-18 15:58:17 UTC 修订 |
| R14 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `1bcdb9f2940a771f90c71879d468a4553a936afd86a78aa0ba9747eb7638efe7` | `FAIL` | 3×P1：`MP-11` 已暂存修改与冲突中止、`MP-19` F-1018 缺 `ls-files -s`、`MP-18` 缺未跟踪目录变体 | 无 | 2026-11-18 16:04:36 UTC 修订 |
| R14 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `1bcdb9f2940a771f90c71879d468a4553a936afd86a78aa0ba9747eb7638efe7` | `FAIL` | A 1×P1（外部策略中断后的 `--abort` 指引无依据）；B 2×P1（`MP-52` 形态 B 未验提交、`MP-27` 稳定码承诺未验）；C 2×P1（`MP-45` 引用后发布的 `stash show`、`MP-29` 夹具不可执行）；D 3×P1（`MP-11` 已暂存修改、`MP-11` 冲突中止、`MP-23` 跟踪键）；E 3×P1（`MP-18` 未跟踪目录、`MP-19` 缺 `ls-files -s`、`MP-16` 理由失实） | 无 | 2026-11-18 16:04:36 UTC 修订 |
| R15 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `6b1d68679dc7393243b447618a3656a84c9e3ee87b9cb4e6e988eae1886bc5e9` | `FAIL` | 3×P1：`MP-15` AC8 在 `--json` 下断言人读 `Error-Code:` 头、`MP-51` 无条件把子进程 stdout 转到 stderr（策略失败时破坏单 JSON 文档）、`MP-50` 的 `docs/error-codes.md` N/A 理由失实且未钉住 stage 1/2 | 无 | 2026-11-18 16:34:19 UTC 修订 |
| R15 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `6b1d68679dc7393243b447618a3656a84c9e3ee87b9cb4e6e988eae1886bc5e9` | `FAIL` | A 1×P1（外部策略中断后的恢复遗留策略新建的未跟踪文件）；B 1×P1（`MP-26` AC2/AC6 空实现可过）；C 1×P1（`MP-08` AC3/AC4 沿用已快进的仓库）；D 2×P1（`MP-15` AC8 JSON 错误格式、`MP-51` 失败时的流隔离）；E 2×P1（`MP-50` 退出码 N/A 理由、stage 前置条件） | 无 | 2026-11-18 16:34:19 UTC 修订 |
| R16 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `501fb9781612ae8d3ea8291c68e93733d9227c37e0942800bc1a1209b2bec526` | `FAIL` | 5×P1：`MP-40` AC2、`MP-07` AC7、`MP-05` AC2 换夹具而未显式新建（第 8 条规则 (iv)）；确认 Codex 的 `MP-14` AC6、`MP-33` AC5/AC6、`MP-19` AC5、`MP-48` 文档集 | 无 | 2026-11-18 16:41:53 UTC 修订 |
| R16 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `501fb9781612ae8d3ea8291c68e93733d9227c37e0942800bc1a1209b2bec526` | `FAIL` | A PASS；B 1×P1（`MP-33` AC5/AC6 未钉住 HEAD 与暂存 blob）；C 1×P1（`MP-48` 缺 `commit` 文档）；D 1×P1（`MP-14` AC6 只断言尝试行）；E 1×P1（`MP-19` AC5 沿用已提交的 F-1018 仓库） | 无 | 2026-11-18 16:41:53 UTC 修订 |
| R17 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `71d3fff48665cc4ae0b05bd78d5cd25995734d8a89ad36ad934c28459e54aeef` | `FAIL` | 3×P1：Σ(b) 按字节比较索引会让正确的 `--abort` 失败；确认 Codex A 的 `clean` pathspec 与 Codex B 的参数顺序计数（同类问题亦见 `MP-14` AC1）；驳回 Codex D（`M` 由第 8 条「夹具提交名」标签解析） | P2：`MP-22` AC4、`MP-38` AC3 显式指明 AC1 仓库 | 2026-11-18 16:47:43 UTC 修订 |
| R17 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `71d3fff48665cc4ae0b05bd78d5cd25995734d8a89ad36ad934c28459e54aeef` | `FAIL` | A 1×P1（恢复步骤的 `clean` 未按字面量处理路径）；B 1×P1（`MP-33` 两种参数顺序合计为一条 AC）；C PASS；D 1×P1（`MP-14` AC8 的 `M` 不可解析——驳回，见 Claude 行）；E PASS | C 1×P2（Σ 的索引字节比较过严，已按 P1 修复） | 2026-11-18 16:47:43 UTC 修订 |
| R18 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `05957f99f7fe0243606dc51f51ff4fc091592ce22662cd190d5be22d89472e09` | `FAIL` | 3×P1：生成器转义使 `MP-28`/`MP-14`/`MP-50` 的期望文本含真实换行/制表符；确认 Codex B（`MP-26`/`MP-33` 合并计数）与 Codex D（`MP-55` 区分度、远端夹具命名） | 无 | 2026-11-18 16:52:42 UTC 修订 |
| R18 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `05957f99f7fe0243606dc51f51ff4fc091592ce22662cd190d5be22d89472e09` | `FAIL` | A PASS；B 2×P1（`MP-33` AC6/AC7、`MP-26` AC2/AC6 把两个状态事实计为一条）；C PASS；D 2×P1（`MP-55` 测不出被忽略的 `-s octopus`、`MP-13`/`MP-23` 的 `git` 远端提交无法按命名规则打标签）；E PASS | 无 | 2026-11-18 16:52:42 UTC 修订 |
| R19 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `995b5c0c596782715812a18b24d06733f4cff4022580f9865e42a1d280da5187` | `FAIL` | 3×P1：确认 Codex B（`MP-02` 完成提交未核对树）、Codex E（`MP-20` 缺 `added in` 对照）与 Codex D（`MP-55` 单目标 `--no-commit`；按 Git 2.54.0 预探改为拒绝路径） | P2：跨计划 `commit.rs`/`ls_files.rs` 写者登记（已修） | 2026-11-18 16:57:05 UTC 修订 |
| R19 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `995b5c0c596782715812a18b24d06733f4cff4022580f9865e42a1d280da5187` | `FAIL` | A PASS；B 1×P1（`MP-02` AC6/AC8）；C PASS；D 1×P1（`MP-55` 单目标待决状态）；E 1×P1（`MP-20` 真新增文件对照） | 无 | 2026-11-18 16:57:05 UTC 修订 |
| R20 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `f49c7a283593e53b0e8dfe8ab7e5115c0fe7ca3509c1e65d7b13b9f3c5866262` | `FAIL` | 2×P1：确认 Codex C（`MP-48` 缺 `--continue` 路径）与 Codex D（`MP-55` 快进的单目标） | 无 | 2026-11-18 17:01:04 UTC 修订 |
| R20 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `f49c7a283593e53b0e8dfe8ab7e5115c0fe7ca3509c1e65d7b13b9f3c5866262` | `FAIL` | A PASS；B PASS；C 1×P1（`MP-48` 只测 `commit` 路径）；D 1×P1（`MP-55` 单目标拒绝未限定非快进）；E PASS | 无 | 2026-11-18 17:01:04 UTC 修订 |
| R21 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `505b4489dba435683aeef0e0650dc21f39a478e374284eedf605ef15f608169e` | `FAIL` | 2×P1：确认 Codex B（`MP-28` AC6）与 Codex D（`MP-55` 拒绝条件过宽） | P2：`MP-55` 可另加「两侧改成相同内容」的成功夹具（卡已 8/8，登记为残余风险） | 2026-11-18 17:03:44 UTC 修订 |
| R21 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `505b4489dba435683aeef0e0650dc21f39a478e374284eedf605ef15f608169e` | `FAIL` | A PASS；B 1×P1（`MP-28` AC6 测不出未执行合并）；C PASS；D 1×P1（`MP-55` 把两侧同改同一文件一律拒绝）；E PASS | 无 | 2026-11-18 17:03:44 UTC 修订 |
| R22 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `cd6321191cc26ceb180b1cd34334b14c308ccd75964bc2a7408d925b65a737cd` | `FAIL` | 2×P1：确认 Codex D（`MP-13` 既有用例改写不可达）；新增 `MP-35`/`MP-38` 未登记会被打破的既有用例 `merge_strategy_unknown_name_lists_supported_values` | 无 | 2026-11-18 17:06:42 UTC 修订 |
| R22 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `cd6321191cc26ceb180b1cd34334b14c308ccd75964bc2a7408d925b65a737cd` | `FAIL` | A PASS；B PASS；C PASS；D 1×P1（`MP-13` 对空目录既有用例的改写无法到达非快进诊断）；E PASS | 无 | 2026-11-18 17:06:42 UTC 修订 |
| R23 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `9507ad77c691d02067f5aa655a8adbd2fa9fb7fb3fb0ad618730b63a1ea25e7d` | `FAIL` | 1×P1：确认 Codex E（`MP-54` 规则缺少 Git 的回退条件） | 无 | 2026-11-18 17:10:19 UTC 修订 |
| R23 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `9507ad77c691d02067f5aa655a8adbd2fa9fb7fb3fb0ad618730b63a1ea25e7d` | `FAIL` | A PASS；B PASS；C PASS；D PASS；E 1×P1（`MP-54` 把所有二进制 1to2 冲突都描述为各侧原始 blob） | 无 | 2026-11-18 17:10:19 UTC 修订 |
| R24 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `8855addddce02e2ee03d95506998b36dbca168ff3fd521180bd11a922a5e88a5` | `FAIL` | 1×P1：确认 Codex D（`MP-30` 错误源码锚点） | P2：F-EXT `$4`、`MP-09` 起点（均已修） | 2026-11-18 17:12:44 UTC 修订 |
| R24 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `8855addddce02e2ee03d95506998b36dbca168ff3fd521180bd11a922a5e88a5` | `FAIL` | A PASS；B PASS；C PASS；D 1×P1（`MP-30` AC2 锚点指向 `not a symbolic ref` 路径）；E PASS | C 1×P2（`MP-09` 重建夹具起点）；D 1×P2（`git-merge-theirs` 在 `-X` 下读到 `HEAD`）——均已修 | 2026-11-18 17:12:44 UTC 修订 |
| R25 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `bd993c9bd8de723f7509eac74cf6b819b76f98fe981a7cf04d6f3ed93ccc88b9` | `FAIL` | 3×P1：确认 Codex C（提交树只查 `file`）、D（`MP-15` argv 安全描述矛盾）、E（stdin 夹具与第 6 条 helper 清单矛盾） | 无 | 2026-11-18 17:15:55 UTC 修订 |
| R25 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `bd993c9bd8de723f7509eac74cf6b819b76f98fe981a7cf04d6f3ed93ccc88b9` | `FAIL` | A PASS；B PASS；C 1×P1（`MP-10`/`MP-43` 提交树）；D 1×P1（`MP-15` Security 与 `-X` 透传矛盾）；E 1×P1（`MP-50` AC6 需要 stdin helper） | 无 | 2026-11-18 17:15:55 UTC 修订 |
| R26 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `018e5f55072fc709b0c0700960a511292a14533eb5f9cace1e85103b2d70e4c1` | `FAIL` | 2×P1：确认 Codex A（网站分支切换与 ER-06a 冲突）与 Codex D（`MP-51` 有损解码与「原样」矛盾） | 无 | 2026-11-18 17:19:16 UTC 修订 |
| R26 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `018e5f55072fc709b0c0700960a511292a14533eb5f9cace1e85103b2d70e4c1` | `FAIL` | A 1×P1（不在 `cf` 时禁止 `libra switch cf`）；B PASS；C PASS；D 1×P1（`MP-51` 编码契约矛盾）；E PASS | 无 | 2026-11-18 17:19:16 UTC 修订 |
| R27 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `7602a46ee95c5e197e555bf947f5ab8c900603bc454a767aff3690b071632957` | `FAIL` | 2×P1：确认 Codex B（`MP-04` 依赖未合入的 `I974-01` 装饰；另发现其后缀规则与 Git 不符，已登记）与 Codex C（`MP-39` AC4 只查单文件） | P2：网站分支判定前缀匹配（已修） | 2026-11-18 17:22:41 UTC 修订 |
| R27 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `7602a46ee95c5e197e555bf947f5ab8c900603bc454a767aff3690b071632957` | `FAIL` | A PASS；B 1×P1（`MP-04` 的期望主题依赖 `DEP-MP-04` 未保证的 `I974-01` 实现）；C 1×P1（`MP-39` AC4 未验完整索引）；D PASS；E PASS | 无 | 2026-11-18 17:22:41 UTC 修订 |
| R28 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `0ffbb11a4b93700a4865d444b1b898c01c9375f42acdee0c054a1bb402c93eba` | `FAIL` | 4×P1：确认 Codex A（目录检查非字面量）、D（`MP-23` 未查分支、`MP-51` 未测有损解码）、E（`MP-19` AC4 未查父提交） | P2：`MP-04`/`MP-49` 的 `-F` 不受影响未单测（`MP-04` 已 8/8，登记为残余风险） | 2026-11-18 17:26:01 UTC 修订 |
| R28 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `0ffbb11a4b93700a4865d444b1b898c01c9375f42acdee0c054a1bb402c93eba` | `FAIL` | A 1×P1（目录检查的 pathspec 非字面量）；B PASS；C PASS；D 2×P1（`MP-23` 分支未前移可过、`MP-51` 有损解码不可测）；E 1×P1（`MP-19` AC4 单父提交可过） | B 1×P2（`-F` 未单测，登记） | 2026-11-18 17:26:01 UTC 修订 |
| R29 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `f59dc9c44170a93b1d868008eb740571307ca368f89ca87e0e24654a68ef5f76` | `FAIL` | 2×P1：确认 Codex A 的两项（网站分支首行判据缺命令、目录检查行格式） | P2：`MP-51` 捕获输出无大小上限（登记为残余风险） | 2026-11-18 17:28:44 UTC 修订 |
| R29 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `f59dc9c44170a93b1d868008eb740571307ca368f89ca87e0e24654a68ef5f76` | `FAIL` | A 2×P1（网站第 2 步首行判据缺 `status` 命令、故障恢复目录检查的行格式判据不可满足）；B PASS；C PASS；D PASS；E PASS | D 1×P2（外部策略输出捕获无上限，登记） | 2026-11-18 17:28:44 UTC 修订 |
| R30 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `a20987cf5fa56689e293c2f7ed6b8bde03d084220bd8b625d85e91b2712e6aeb` | `PASS` | 无 | 残余 P2：`MP-04`/`MP-49` 的 `-F` 未单测；`MP-51` 捕获输出无大小上限；`MP-45` 保留 Git 的 `git stash pop` 提示；`MP-55` 可另加两侧改成相同内容的成功夹具 | 2026-11-18 17:31:05 UTC 记账 |
| R30 (Codex gpt-6.1-terra, 5 分区) | 全文，受审 SHA-256 `a20987cf5fa56689e293c2f7ed6b8bde03d084220bd8b625d85e91b2712e6aeb` | `PASS` | 无（A、B、C、D、E 均 PASS） | C 1×P2（`MP-45` 保留 `git stash pop` 提示，属已登记的 Git 一致选择） | 2026-11-18 17:31:05 UTC 记账 |

## 实施证据汇总

> 按 v2.12 规定，执行与收口证据统一汇总于本节或对应卡的 `Verification` 记录，不新建证据文件。

| 卡 | 日期 | 版本 | 代码评审 | 定向验证 | 备注 |
|---|---|---|---|---|---|
| `MP-01` | 2026-11-20 | `0.30.216` | Codex R3 `VERDICT: PASS`（`/tmp/opencode/codex-mp01-r3.log`） | 卡特有用例 1/1、解析器单测 1/1、共享回归 13/13、版本守卫 2/2 | 网站同步与部署按用户 local C/D 指令执行；未跑全量 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-MP-01 | `rebase` / `cherry-pick` / `revert` 独立冲突引擎对齐 | 超出本计划 28 个 `merge`/`pull` issue 范围 | 另立对应命令计划 | 后续对应计划 |
| DEFER-MP-02 | `subtree` 合并策略 | 不在本批 28 个 issue 中 | 用户明确需求时 | 后续对应计划 |
| DEFER-MP-03 | 外部策略参与多策略链（回绕、候选评分、末位结果复用与重跑，`git@6de20f6092` `builtin/merge.c:1786-1860`） | `#1025` 的验收与 `#1027` 的 Acceptance criteria 不含该组合（`#1027` Expected behavior 第 3 条提出，关闭 `#1027` 时注明并链接本项）；pinned Git 不能查找第二个自定义策略（`builtin/merge.c:228-229`），两个外部策略的链在 Git 中同样不可用；本计划以 `MP-42` 明确拒绝该组合 | 用户提出含外部策略的多策略链需求，或上游修复第二个自定义策略的查找 | 后续对应计划 |
| DEFER-MP-04 | `libra read-tree --reset -u`，使 `t7606#3` 形态的策略脚本（以 `libra read-tree --reset -u` 取代原样的 `eval git read-tree --reset -u \$$#`）同时更新索引与工作树并通过其工作树断言 | `read-tree` 管道命令不在本计划 28 个 `merge`/`pull` issue 的命令范围；Libra 仓库无 `.git`，原样脚本中的 `git` 命令不可用；`MP-15` 以只更新索引的 `libra read-tree` 脚本验收 `#1027` AC1 与 `t7606#3` 的结果树/父提交（关闭 `#1027` 时注明并链接本项） | `read-tree` 增加 `-u`/`--reset` 的计划立项时 | 后续对应计划 |
| DEFER-MP-05 | `merge.conflictStyle=diff3`/`zdiff3` 冲突块基准段的路径标签（`||||||| base:path`） | `#1011` 的编号验收只列两侧标签（`MP-16` 覆盖），基准段标签只在 Expected 3 中提出；其实现牵涉 diff3 基准标签的独立生成路径 | 用户提出 diff3/zdiff3 基准标签需求或对应上游用例纳入验收时 | 后续对应计划 |
| DEFER-MP-06 | 不带参数的 `libra merge`（按 `branch.<name>.remote`/`merge` 合并上游，Git 的 `merge.defaultToUpstream`）及其在未诞生分支上的 `initial pull` reflog | Libra 不支持无参数 `merge`（0.30.207 实测以 `merge requires a branch argument, --continue, --abort, or --quit` + `LBR-CLI-002` 拒绝），这是独立的参数面缺口，不在 `#1003` 的标题行为（reflog 文案）内；`#1003` AC2 的 `merge <ref>` 与 `merge --ff-only <ref>` 形态由 `MP-06` 覆盖，关闭 `#1003` 时注明并链接本项 | 用户提出无参数 `merge` 需求或另立 `merge` 参数面计划时 | 后续对应计划 |
| DEFER-MP-07 | `--squash` 与最终生效的 `--no-commit` 的组合放行（命令行 `--no-commit --squash`、`--commit --no-commit --squash`，以及 `branch.<name>.mergeOptions=--squash` 加命令行 `--no-commit`） | Git 允许（`git@6de20f6092` `builtin/merge.c:1503-1508` 只拒绝最终生效的 `--commit`）；Libra 在 clap 层互斥（`src/command/merge.rs:385-391`），`issues/978.md` 的 `ADR-I978-02`/`I978-01` AC18 规定配置组合 fail-closed；`#985`/`#990` 的验收只要求拒绝 `--squash` 与 `--no-ff`/`--commit` 的组合，不含该放行；`MP-33` 在 `COMPATIBILITY.md` 记为有意差异 | 用户提出该组合需求，或 `issues/978.md` 修订其 fail-closed 口径时 | 后续对应计划 |

本计划没有可撤回或延后的任务卡：全部 53 张卡都是「完成判据」的必需项，28 个 issue 均在其承载卡完成后关闭（`#1004` 的「未受信任签名」分支按 `MP-32` 以有意差异记录，不阻止关闭；`#1027` 的 AC3 工作树部分与 Expected behavior 第 3 条分别按 `DEFER-MP-04`、`DEFER-MP-03` 延后，关闭时在 issue 中注明并链接这两项；`#981` AC4 的 `stash list` 部分按 Git 处理（`MP-45`），关闭时注明；`#1011` Expected 3 按 `DEFER-MP-05` 延后，关闭时注明；`#1003` AC2 的无参数 `libra merge` 形态按 `DEFER-MP-06` 延后，关闭时注明；`#980` AC1 的不带 `--cleanup` 形态另需 `DEP-MP-13`（`CS-14`）合入，未合入时关闭 `#980` 须注明并链接）。

## 完成判据

- [ ] 计划文本 Claude（Subagent）与 Codex 双审对同一 SHA-256 均为 `VERDICT: PASS`（M0）。
- [ ] 全部 53 张任务卡 `Lifecycle=done` 且 `Acceptance=complete`，各卡 `Verification` 已按「验证执行协议」运行并记录。
- [ ] 每张卡均完成 `patch + 1`、版本面同步、`gh release create` 与 D 组，并完成「文档与网站验收命令」第 1–8 步（网站交付取得独立 Claude Code `VERDICT: PASS`；与计划文本双审互不替代）。
- [ ] 所有任务卡满足 `G-*`；「任务卡粒度审计表」与各卡 `Granularity` 一致。
- [ ] 收口阶段全量门在**最终代码树**上通过：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，记录 `libra rev-parse HEAD` 与 `libra rev-parse 'HEAD^{tree}'`；树一旦变化即重跑。按 ER-13，全量暴露的任何失败（含既有失败）均须定位修复，或附计划基线提交上的复现证据并登记 `FIX-*`/`DEFER-*` 后重跑至全绿，不得带着红色结果通过。
- [ ] 「兼容与文档收口」各项完成，且未在仓库新建证据文件或目录（v2.12）。
- [ ] 「修订历史」与 `plan-status.md` 已同步。
