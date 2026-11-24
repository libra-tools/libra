# `cherry-pick` / `revert` / `rebase` / `am` 序列器与补丁兼容计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Claude R56 与 Codex R56 双审 PASS（受审 SHA-256 前 12 位 `62bc0c5cc683`；Codex R56 为 A 区全量复审加 B–F 区对 R55 全量 PASS 字节的差异确认），计划可提交；全部任务 `pending`，尚未执行。本文只规划任务，不宣称实现完成。（批处理例外：按协调者指令，20261117 批次起草阶段暂不修改 `plan-status.md`，由协调者统一登记合入，且在任一任务卡开工前完成登记。按用户 2026-11-18 指令，计划文本提交门为 Claude（Subagent）与 Codex 双审，二者对同一受审字节（同一 SHA-256）均给出 `VERDICT: PASS` 方可提交；该口径取代 2026-11-17 的 Codex 单审口径。）
> 跟踪范围：9 个 open issues（#725, #726, #727, #728, #729, #730, #731, #780, #907）；本计划完成后这 9 个 issue 均为**部分闭环、保持 open**（见「Issue 闭环登记」）。

## 文档职责

本文解决 `libra cherry-pick`、`libra revert`、`libra rebase`、`libra am` 与关联 `libra show --pretty=format:%B` 的 9 个 Git 兼容缺口：空 cherry-pick 停止态与提示（#725）、`A..B` 提交范围（#726）、重复/互斥 `--rerere-autoupdate`（#726、#727）、`--ff` 选取 root commit（#729）、unborn HEAD 上的 cherry-pick（#730）、`--empty=drop`/`keep` 之后 `show --pretty=format:%B` 的多余尾行（#731）、detached HEAD `revert` 与状态清理、`cherry-pick -`（#728）、`rebase -f/--force-rebase` 及其签名重放前置（#780）、unborn 分支上的 `am`（#907）。目标是以 27 张单轴实现卡与 2 张家族发布点卡 `REL-SA-01`、`REL-SA-02`（共 29 张活动任务卡；`SA-02b`、`SA-05a`、`SA-05b`、`SA-07h`、`SA-07e`、`SA-07f`、`SA-09c`、`SA-09d` 已于 R43 撤回，`SA-01c`、`SA-07d`、`SA-07b` 已于 R47 撤回（同轮撤回的 `SA-09a`、`SA-09e` 已于 R49 恢复），编号均不复用）完成各 issue 验收项的实现、测试、文档与发布闭环。按用户 2026-11-19 05:17:51 UTC 决定，各卡只交付并验收所跟踪 issue 验收项明确要求的行为，并选取能区分正确实现与最可能错误实现的核心断言；各 issue 中「上游测试脚本 / TC 全量通过」类验收项与逐条复现上游脚本全部断言的部分登记 `DEFER-SA-13`..`DEFER-SA-20`；按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论（R47），不属所跟踪 issue 验收项的卡与场景（`SA-01c`、`SA-07d`、`SA-07b`（同轮撤回的 `SA-09a`、`SA-09e` 已于 R49 恢复）、`SA-06` 的非 #731 格式变体）撤回并登记 `DEFER-SA-15`、`DEFER-SA-20`、`DEFER-SA-21`（R49–R52 因 GC-07/GC-08 恢复 `SA-09a`、`SA-09e` 并以 `SA-09g`、`SA-09h`、`SA-09i` 交付中止保护、中断后重试与并发保护、新增 `SA-05c`；实现中新可达而未验收的既有错误或保护路径登记 `DEFER-SA-22`、`DEFER-SA-23`）；相关 issue 在本计划完成后只是部分闭环（「Issue 闭环登记」）。按用户 2026-11-19 06:51:45 UTC 决定，GC-04 的 `--json`、`--machine`、退出码与人读输出按「每个错误类一条代表路径」验收（「验证执行协议」第 11 条）。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

- **网站 checkout（下称 `$LIBRA_BACKEND`）：** `$LIBRA_BACKEND` 为 `/Volumes/Data/gitmono/libra-backend`，在其 `cf` 生产分支直接工作（用户 2026-11-18 11:55:09 UTC 确认）。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`，该路径已过时（本机不存在），登记为已知文档滞后，本计划不改 `AGENTS.md`。执行任何网站步骤前按「文档与网站验收命令」第 2 步核验该检出；核验不通过时 STOP 并向用户报告，不得自行改用其它目录。2026-11-18 现场核对：该路径存在，含 `.libra`、无 `.git`，`libra status --short --branch` 首行为 `## cf...origin/cf`，`apps/tanstack-app/content/docs/commands/` 下含 `cherry-pick`、`revert`、`rebase`、`am`、`show`、`commit` 六个 `.en.md`（R47 起本计划只涉及前五个）。该检出在执行机器上的可用性登记为 `DEP-SA-10`，所有有网站影响的活动卡（除 `SA-09f`、`SA-09b` 外）以它为依赖。`$LIBRA_ROOT` 指本仓库 checkout 的绝对路径。

### 适用范围

- 源码：`src/command/cherry_pick.rs`、`src/command/show.rs`、`src/command/revert.rs`、`src/command/rebase.rs`、`src/command/am.rs`、`src/internal/sequencer/mod.rs`（R47 起不写 `src/command/commit.rs`）。
- 测试：`tests/command/{cherry_pick,revert,rebase}_test.rs`、新增 `tests/command/am_test.rs`（在 `tests/command/mod.rs` 注册，挂在 `command_test` target）、`tests/INDEX.md`；回归 target `compat_mail_am_basic`（`tests/compat/mail_am_basic_test.rs`）、`compat_sequencer_message_author`、`compat_noninteractive_history_controls`；`SA-08a` 的库内用例登记 `tests/SRC_SERIAL_REGISTRY.tsv`（由 `compat_serial_registry` 守护）。
- 文档：`docs/commands/{cherry-pick,show,revert,rebase,am}.md`、`docs/commands/zh-CN/{cherry-pick,show,revert,rebase,am}.md`、`docs/development/commands/{cherry-pick,show,revert,rebase,am}.md`、`docs/error-codes.md`、`COMPATIBILITY.md`、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/{cherry-pick,show,revert,rebase,am}.en.md`。

### 非目标

- 不在本计划中修改 `libra commit` 默认签名策略与 SSH 签名实现（由 `plan-20261117-commit-signing.md` 的 `REL-CS-00`、`REL-CS-01` 承接，本计划以 `DEP-SA-01`、`DEP-SA-02` 消费）。
- 不修改 SQLite schema、不新增迁移（不涉及 `sql/**`）；unborn 状态用既有 `TEXT NOT NULL` 列中的全零 OID 表达（`ADR-SA-02`）。
- 不改变单提交 `cherry-pick -m` 对非合并提交的既有拒绝（Git ≥ 2.21 接受并忽略），登记为 `DEFER-SA-04`。

### 成功定义

- 9 个 issue 的行为验收项（「Issue 闭环登记」的「本计划交付」列）在对应卡以具名测试验收；各 issue 的「上游测试脚本 / TC 全量通过」类验收项与收窄出的专项场景登记 `DEFER-SA-11`..`DEFER-SA-24`（`DEFER-SA-13` 为上游脚本与 TC 门），9 个 issue 在本计划完成后均为部分闭环、保持 open，不得据本计划完成关闭。
- 全部 29 张活动卡达到 `Lifecycle=done` 且 `Acceptance=complete`；计划收口全量门（`fmt` + `clippy` + 双环境 `nextest`）在最终树全绿。
- 每张触及用户可见行为的卡在同卡完成 EN/zh-CN/开发文档、`COMPATIBILITY.md` 与网站页面的交付与验收（ER-06/ER-06a）。

## 使用规则

- 每次状态推进、快照更新与「修订历史」新增行都必须记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时间戳（以 `date -u '+%Y-%m-%d %H:%M:%S UTC'` 取值，不得预填或写未来时间）。
- 推进或完成任一任务卡时，须在同一变更中同步更新 [plan-status.md](plan-status.md)（起草阶段按头部批处理例外不提前修改 `plan-status.md`）。
- `COMPATIBILITY.md` 被多份 `plan-20261117-*` 计划共享且受兼容账本绝对行号约束：只允许在既有条目行内追加，不得增删行改变行号（由「文档与网站验收命令」第 1d 步机械校验）。
- 本计划所有测试摘要、门结果与评审结论直接写入本计划文件，不新建外部证据文件或目录（v2.12）。
- 网站文档交付一律按「文档与网站验收命令」执行：独立发布卡在本卡 CLI 发布后完成网站提交、推送与部署核验；家族子卡只在 `$LIBRA_BACKEND` 准备并本地验证草稿，由所属家族发布点（`SA-08a`/`SA-08b`/`SA-08m`/`SA-08g`/`SA-08h`/`SA-08j` 为 `REL-SA-01`，`SA-09`/`SA-09a`/`SA-09e`/`SA-09g`/`SA-09h`/`SA-09i` 为 `REL-SA-02`；`SA-09f`、`SA-09b` 无网站变化）统一提交、推送与核验；网站无漂移时按「文档与网站验收命令」第 3c 步的替代证据收口。网站门（独立 Claude Code `VERDICT: PASS`）与计划文本提交门（Claude Subagent 与 Codex 双审）互不替代。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。过期锚点只能作为历史线索。以下锚点于 2026-11-18 在本 checkout 现场核对。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 测试命名 | `command_test` 由 `tests/command_test.rs` 的 `mod command;` 汇集 `tests/command/*.rs`，测试完整路径为 `command::<模块>::<函数>`；库内单元测试路径为 `<模块路径>::tests::<函数>`（`src/command/` 下为 `command::<模块>::tests::<函数>`，`src/internal/` 下为 `internal::<模块>::tests::<函数>`） | `tests/command_test.rs:10`; `tests/command/mod.rs:1021,1031,1052,1115,1127,1140` |
| `cherry-pick` 空提交 | 空提交/冗余提交返回 `EmptyCommit`/`RedundantCommit`，映射 `CliInvalidArguments`（`LBR-CLI-002`，Cli 类别默认 exit 129），不保留 sequencer 状态；既有测试 `cherry_pick_redundant_blocked_then_kept` 断言 exit 129 | `src/command/cherry_pick.rs:124-128,217-218,271-276,2055-2057,2379-2384`; `src/utils/error.rs:514-525`; `tests/command/cherry_pick_test.rs:1600-1644` |
| `cherry-pick` 参数 | 逐参数 `resolve_commit`（`util::get_commit_base`）不解析 `A..B`；多提交序列已支持 `-n`/`--ff`；`--rerere-autoupdate`/`--no-rerere-autoupdate` 仅互相 `overrides_with`；控制选项 `conflicts_with_all` 不含 rerere 与 `--keep-redundant-commits`/`--empty` | `src/command/cherry_pick.rs:689-716,750-801,820-827,1077-1083,1102,1148,1180,1957,3036-3038` |
| `cherry-pick` 状态 | `CherryPickState.head_orig: ObjectHash` 必填；detached HEAD 已允许（#528） | `src/command/cherry_pick.rs:940-950,3158-3166`; `tests/command/cherry_pick_test.rs:4696-4700` |
| `cherry-pick --empty=drop` / `show` | `--empty=drop` 不改 HEAD 直接返回；`show --pretty/--format` 自定义格式分支在 `formatter.format` 后无条件 `output.push('\n')`；`--format` 是 `--pretty` 别名 | `src/command/cherry_pick.rs:2386-2393`; `src/command/show.rs:86-94,1145-1150,1164-1165` |
| `revert` | `execute` 对 `Head::Detached(_)` 返回 `RevertError::DetachedHead`（`RepoStateInvalid`/`LBR-REPO-003`）；`RevertArgs` 无 `--reference` | `src/command/revert.rs:65-66,223,326-330,454-456` |
| `commit` | 消息解析 `resolve_final_message` 只接受 merge/squash 种子，无 stopped revert 默认消息；空消息 `CommitError::EmptyMessage` → `RepoStateInvalid`；stopped revert 结束逻辑在 `StoppedSequences` | `src/command/commit.rs:359-360,522-524,1812-1836,2797-2896` |
| `rebase` | `RebaseArgs` 无 `-f`/`--force-rebase`；`create_replayed_commit` 8 个 action 中 Pick/Reword/Edit 复制原消息（含旧 `gpgsig`）、Fixup 复制目标消息（含目标 `gpgsig`）、Amend 经 `amend_replacement_message` 重附旧 `gpgsig`、Squash/FixupKeep/FixupKeepEdit 去掉 `gpgsig`；`--signoff` 分支经 `rejoin_commit_message` 重附 `gpgsig`；全部不按 `commit.gpgSign` 重签；另有两条重放后另行生成提交的路径——交互式消息编辑 `edit_interactive_commit_message`（消息改变时）与 `edit` 停止后 `--continue` 吸收已暂存改动的 `amend_head_from_index_if_staged`——均以 `Commit::new` 生成不签名的新提交；`--apply` 只有诊断性拒绝（`LBR-UNSUPPORTED`，Libra 无 apply 后端） | `src/command/rebase.rs:258-264,842-848,1402-1443,2024-2065,2511-2524,5850-5940,5942-5963` |
| 签名基础 | `vault_sign_commit` 与 `verify_commit_signature`（vault OpenPGP）；`merge --verify-signatures` 是现有 CLI 验签入口 | `src/command/commit.rs:3088-3100,3173-3200`; `src/command/merge.rs:1630-1647` |
| `am` | 硬编码拒绝 unborn HEAD（`am requires an existing HEAD commit`）；`AmState.head_orig`/`expected_head: ObjectHash`、`AmSequenceState.head_orig: String` 必填；`from_sequence` 以 `parse_repo_oid` 严格解析 | `src/command/am.rs:139-175,284-291`; `src/internal/sequencer/mod.rs:118-128` |
| 序列器持久化 | `sequence_state` 列 `head_orig`/`current_oid` 为 `TEXT NOT NULL`，后经 worktree 作用域重建 | `sql/migrations/2026070401_sequence_state.sql:6-15`; `sql/migrations/2026071901_sequencer_worktree_scope.sql` |
| GC 根 / `ORIG_HEAD` | `collect_sequencer_state_roots` 对 `head_orig`/`current_oid` 的全零 OID 跳过遍历（`is_null_oid`），非零 OID 作为根；`pseudo_ref::orig_head` 经 `sequencer::load_for_scope` 把 `sequence_state.head_orig` 原样作为 `ORIG_HEAD`，而 `load_for_scope` 的 `SequenceKind::from_token` 不识别 `am`，故 `am` 暂停行使 `ORIG_HEAD` 解析报 `unknown sequence kind 'am'`（代码阅读结论，开工时实测复核） | `src/command/maintenance.rs:4094-4102,4130-4153`; `src/internal/pseudo_ref.rs:197-212`; `src/internal/sequencer/mod.rs:68-76,235-249` |
| 全零 OID API | `ObjectHash::zero_for_kind(kind)` / `ObjectHash::zero_str(kind)`（git-internal 0.10.x） | `git-internal` 0.10.4（`Cargo.lock`）的 `src/hash.rs:347,356`; 仓内用例 `src/command/update_ref.rs:180` |
| 上一分支解析 | `resolve_previous_checkout_target` 基于 reflog 导航 | `src/command/switch.rs:470-476` |
| 网站同步 | `scripts/sync-libra-command-docs.py`：默认 check（exit 0 对齐/1 漂移/2 拒绝），`--write`，`--only <cmd…>`；前端门 `pnpm typecheck`、`pnpm build`；页面 `https://libra.tools/en/docs/commands/<cmd>` | 2026-11-18 于 `$LIBRA_BACKEND`（`/Volumes/Data/gitmono/libra-backend`）核对：`scripts/README-command-doc-sync.md`、`scripts/sync-libra-command-docs.py:406-413`、`package.json`、`apps/tanstack-app/wrangler.jsonc`；执行时以当时 `$LIBRA_BACKEND` 内同名文件为准 |
| CI | `base.yml`（`pull_request`、`push` main/`release/**`；job `format`/`clippy`/`redundancy`/`test`）；`codeql.yml`（job `analyze`）；`release.yml`（`push` tag `v*`；job `build-and-upload`/`upload-install-scripts`/`update-homebrew-tap`/`verify-homebrew-formula`/`request-stable-manifest`） | `.github/workflows/{base,codeql,release}.yml` |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-01 | 空 cherry-pick 不输出 `The previous cherry-pick is now empty`、不保留停止态，退出码为 usage 129；`--keep-redundant-commits`/`--empty` 与控制选项未互斥 | `t3505#13/#14/#15`（`TC-0426`/`TC-0427`/`TC-0522`）失败 | `src/command/cherry_pick.rs:217-218,689-716,2379-2384` | `SA-01`, `SA-01b`（`t3505#14`/`TC-0427` 的 `--empty` 互斥（原 `SA-01c`，R47 撤回）与 `TC-0522` 的专项验收登记 `DEFER-SA-15`） |
| GAP-02 | `cherry-pick` 不解析 `A..B` | `t3504#7`、`t3508#8/#9`、`t3430#28`（`TC-0429`/`TC-0514`/`TC-0482`/`TC-2332`）失败 | `src/command/cherry_pick.rs:1077-1083,3036-3038` | `SA-02`（非线性历史的遍历序、`t3508#8/#9` 与 `t3430#28` 登记 `DEFER-SA-14`） |
| GAP-03 | 重复 `--rerere-autoupdate` 被拒；rerere 标志与 `--continue` 未互斥；范围上 rerere 三种模式未验证 | `t3504#4–#8`（`TC-0442`/`0443`/`0444`/`0429`/`0468`）失败 | `src/command/cherry_pick.rs:689-716,820-827` | `SA-03`, `SA-03b`（`t3504#4–#6` 登记 `DEFER-SA-16`） |
| GAP-04 | `--ff` 选取 root commit 的结果 OID 与 Git `cp_ff` 不一致；unborn HEAD 失败 | `t3506#10/#11`（`TC-0512`/`TC-0513`）失败 | `src/command/cherry_pick.rs:1957,3158-3166` | `SA-04`, `SA-05c`, `SA-05`（unborn 上非快进 pick、冲突暂停与 `--abort` 登记 `DEFER-SA-17`；born 分支快进的已暂存保护登记 `DEFER-SA-22`） |
| GAP-05 | `show --pretty=format:%B` 多出尾行 | `t3505#16/#17`（`TC-0523`/`TC-0524`）失败 | `src/command/show.rs:1164-1165` | `SA-06`（`TC-0524` 与 #731 以外的格式变体登记 `DEFER-SA-15`） |
| GAP-06 | `revert` 拒绝 detached HEAD；拒绝/中止后残留状态；无 `--reference`；冲突后 `commit -a` 空消息；`cherry-pick -` 不支持 | `t3501#12/#15–#18`、`t7512#42`（`TC-0484`/`2634`–`2637`/`2807`）失败 | `src/command/revert.rs:326-330,454-456`; `src/command/commit.rs:1812-1836` | `SA-07a`, `SA-07`, `SA-07c`（`TC-2637`/`TC-2807`（原 `SA-07b`、`SA-07d`，R47 撤回）登记 `DEFER-SA-21`；revert 停止态完整快照与「已暂存删除」形态登记 `DEFER-SA-18`） |
| GAP-07 | `rebase` 无 `-f`；重放不按签名配置重签且残留旧 `gpgsig` | `t7510#2`、`t7528#1`、`t7030#2`、`t7031#1`、`t3400#31`、`t3415#23`（`TC-0911`/`0931`/`2972`/`2974`/`2289`/`2488`）失败 | `src/command/rebase.rs:258-264,5850-5963` | `SA-08a`, `SA-08b`, `SA-08m`, `SA-08g`, `SA-08h`, `SA-08j`, `REL-SA-01`, `SA-08`（`-f` 与 `--onto`/`--autosquash`/SSH 组合及 `TC-2972` 登记 `DEFER-SA-19`） |
| GAP-08 | `am` 拒绝 unborn 分支，状态无 unborn 表达 | `#907` 失败 | `src/command/am.rs:139-175,284-291` | `SA-09f`, `SA-09b`, `SA-09`, `SA-09a`, `SA-09e`, `SA-09g`, `SA-09h`, `SA-09i`, `REL-SA-02`（unborn 起点暂停后的 `--continue`/`--skip` 与 `ORIG_HEAD` 投影登记 `DEFER-SA-20`） |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| [plan-20261117-commit-signing.md](plan-20261117-commit-signing.md) | `REL-CS-00`（`CS-02` 默认不签名、`CS-02a` 改 `tests/command/cherry_pick_test.rs`）；单链上大多数卡写 `src/command/commit.rs`，链尾为家族发布点 `REL-CS-06`（含 `CS-25a`、`CS-25i`、`CS-25e`、`CS-25b`；对侧 `DEP-CS-13` 约定其先于 `SA-07d`，`SA-07d` 已于 R47 撤回，属对侧待同步的引用，本计划不改他人文件）；`REL-CS-01`（六张子卡 `CS-03b`、`CS-03c`、`CS-03h`、`CS-03i`、`CS-03k`、`CS-03e`）交付 `gpg.format=ssh` 签名、验签与签名侧 fail-closed 诊断；`CS-18b` 亦写 `tests/command/cherry_pick_test.rs`（与本计划写该文件的卡相交） | `DEP-SA-01` → `SA-01`、`SA-04`；`DEP-SA-05` 已于 R47 随 `SA-07d` 撤回（编号不复用）；`DEP-SA-02` → `REL-SA-01` 家族入口 `SA-08a` 与消费方 `SA-08g`（G-10 串行）；`CS-18b` 等其余相交写者由 `DEP-SA-03` 与对侧 `DEP-CS-07` 的协调者串行窗口互斥 |
| [plan-20261117-log-show.md](plan-20261117-log-show.md) | `LS-23`、`LS-33`、`LS-24`、`LS-25`、`LS-34`（均为独立发布切片）写 `src/command/show.rs`、`tests/command/show_test.rs` 与 `show` 文档 | `DEP-SA-06` → `SA-06` |
| [plan-20261117-plumbing-misc.md](plan-20261117-plumbing-misc.md) / [plan-20261117-checkout-switch-restore.md](plan-20261117-checkout-switch-restore.md) | `PM-10` 写 `tests/command/mod.rs`；`CSR-31`、`CSR-32` 仅在需要同步时写 `tests/INDEX.md`（不写时以对侧「无需同步」记录满足） | `DEP-SA-04` → `REL-SA-02` 家族入口 `SA-09f`（`SA-09b` 与消费方 `SA-09` 同样登记） |
| [plan-20261117-merge-pull.md](plan-20261117-merge-pull.md) | `MP-10`、`MP-41`、`MP-45` 写 `src/command/commit.rs`（`MP-43`、`MP-48` 自 `MP-10` 拆出且不写该文件；对侧链上 `MP-45` 是最后一个 `commit.rs` 写者；对侧关系表建议本计划改为等待 `MP-45`，对侧 `DEP-MP-08` 本身登记 `MP-10`、`MP-30` 先交付，`MP-41`、`MP-45` 的相交写入由对侧 `DEP-MP-10` 窗口协调；对侧 `DEP-MP-08` 指向的 `SA-05b` 已于 R43 撤回，属对侧待同步的引用，本计划不改他人文件）；`MP-30` 写 `src/internal/pseudo_ref.rs`（R43 起本计划不再写该文件） | R47 起本计划不再写 `src/command/commit.rs`：原等待 `MP-45` 的 `DEP-SA-05` 随唯一消费卡 `SA-07d` 撤回（编号不复用）；`DEP-SA-08` 已于 R43 随 `SA-05b` 撤回（编号不复用） |
| [issues/1041.md](issues/1041.md) | `I1041-06` 写 `am.rs`/`cherry_pick.rs`/`commit.rs`/`rebase.rs`/`tests/command/mod.rs`；`I1041-07` 写 `revert.rs`/`tests/command/mod.rs` | `DEP-SA-07` → `SA-01`（主链入口，串行保护后续全部卡） |
| [issues/672.md](issues/672.md) / [issues/547.md](issues/547.md) | `I672-01` 写 `am.rs`、`compat_mail_am_basic` 与 `am` 文档；`AM-01`..`AM-11`（由 `FINAL-547` 收口发布）写 `am` 实现、测试与文档 | `DEP-SA-09` → `REL-SA-02` 家族入口 `SA-09f` 与消费方 `SA-09b` |
| 同日其它 `plan-20261117-*` 计划（含 [plan-20261117-global-init-infra.md](plan-20261117-global-init-infra.md) 的 `DEP-GII-03`、[plan-20261117-merge-pull.md](plan-20261117-merge-pull.md) 的 `DEP-MP-10`） | 共享 `COMPATIBILITY.md`；网站交付时 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物 `apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts` 与 `apps/tanstack-app/dist/` 与所有同批计划的网站交付相交；开工当日各计划写集的其它交集（如 `src/internal/sequencer/mod.rs`、`tests/command/mod.rs`、`tests/INDEX.md`、`tests/SRC_SERIAL_REGISTRY.tsv`、`docs/error-codes.md`） | `DEP-SA-03`：与 `DEP-GII-03`、`DEP-MP-10` 同一口径的协调者单写者串行窗口 |
| [plan-long.md](plan-long.md) | `DEFER-SA-01`..`DEFER-SA-06`、`DEFER-SA-08`..`DEFER-SA-24` 承接位置（`DEFER-SA-07` 已于 R35 撤销，编号不复用） | 计划收口时（执行期，按模板「完成判据」）登记重启条件与索引；本计划文本提交不修改该文件 |
| （无承接计划） | 既有缺陷：`libra revert --no-commit` 从索引不等于 HEAD 树（含已暂存改动）的状态开始时，以 HEAD 树重建结果并从新索引中丢弃已暂存而 HEAD 中没有的条目（`src/command/revert.rs:1366-1387`；Git 允许 `revert -n` 从不等于 HEAD 的索引开始并保留这些改动）。#728 等本计划所跟踪 issue 均不要求该场景 | 本计划不修、不测（R43 撤回原 `SA-07h`，其「既有行为补测」前提不成立）；`SA-07a` 不改变 `--no-commit` 路径。同批计划与 `plan-long.md` 均未承接，登记为「无承接计划」，待有 issue 报告或 `revert` 专项立项时由新计划承接 |

跨计划引用只用稳定任务 ID；对侧计划可能正被并行修订，开工时以对侧当前文本核对 ID 的写集与状态。每卡开工前（ER-01/ER-03）由 Coordinator 读取全部在途计划卡的 `Implementation write set`：任何与本卡写集相交、但未在「依赖登记表」登记的在途或待开工卡，都使本卡置为 `blocked`，先补登记 `DEP-*` 或顺序边（G-10 不可豁免），不得并发实现或评审。

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 9 个 issue 均有上游测试锚点与可复现 Libra 失败 | 保持按 issue 行为轴拆卡；按用户 2026-11-19 05:17:51 UTC 决定只验收 issue 验收项的核心断言，上游脚本全量与 TC 门登记 `DEFER-SA-13`（9 个 issue 部分闭环）；R47 按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回不属 issue 验收项的 `SA-01c`、`SA-07d`、`SA-07b`（同轮撤回的 `SA-09a`、`SA-09e` 已于 R49 恢复） 与 `SA-06` 的非 #731 格式变体 |
| 可行性 | 全部改动落在既有命令与序列器内，无 schema 变更 | unborn 状态用全零 OID（`ADR-SA-02`） |
| 任务卡粒度 | R22 取消无具名审批的 `EX-SA-02-RANGE`；R23 拆出 `SA-01b`；R24 改为每条 AC 只陈述一个可独立判定的性质并逐条计数，拆出 `SA-08b`，去掉冗余的 `list` 前置门；R25 把 `SA-08a`/`SA-08b` 组成家族 `REL-SA-01`；R26 拆出 `SA-01c` 并把测试注册检查归入 ER-06 索引同步；R27 按数据/状态轴拆出 `SA-05a`、`SA-09b`；R28 把签名失败语义并回 `SA-08a`（撤销 `SA-08c`），拆出 `SA-05b`、`SA-09c`（`ORIG_HEAD` 投影），发布卡只保留提交前判据、合入与发布后证据归 D 组；R30 把 `SA-07a` 的状态清理拆为 `SA-07e`，把 `pick` 正向签名拆为 `SA-08c`（`SA-08a` 补签名失败后的 `--abort` 恢复与修正配置后 `--continue`），发布卡增加已提交改动集守卫；R31 撤销 `SA-08c`（不复用该编号），`pick` 的 OpenPGP/SSH 签名与 `--signoff` 尾注回到出口所在的 `SA-08a` 同卡验收，签名失败的停止、`--abort` 回退与修正配置后 `--continue` 分别为 `SA-08d`、`SA-08e`、`SA-08f`；R32 把签名失败 fail-closed（非 0 退出、tip 不变、无新增 commit 对象）并回出口所在的 `SA-08a`，撤销 `SA-08e`（并入 `SA-08d`：停止态诊断与 `--abort` 回退），`SA-08f` 补补丁内容保留判据，发布卡改为逐提交核对家族提交序列；R33 改按 `ADR-SA-03` 的原子回滚设计：签名失败在 `SA-08a` 内回滚到命令前状态，不再产生需后续卡收口的停止态，撤销 `SA-08d`、`SA-08f`（编号不复用），`gpg.format=ssh` 与 `--signoff` 分别拆为 `SA-08g`、`SA-08h`；R34 以签名能力预检（`SA-08i`，在任何写入前拒绝）取代原子回滚，出口级签名失败以库内单元判据留在 `SA-08a`，中途签名失败的端到端恢复登记 `DEFER-SA-07`，签名验收改为从 `<upstream>` 快进校验 `topic`（证明是新的重放结果）；R35 撤销 `DEFER-SA-07`：中途签名失败按冲突停止同构处理（`SA-08j` 停止与 `--continue`、`SA-08k` `--abort`，以测试 failpoint 确定性制造），预检限定于会生成提交的路径（`SA-08l`），`SA-08i` 改为逐项比较索引、工作区字节、引用与辅助状态，`SA-09a` 增加真实 `am` 暂停态 GC 用例；R37 按用户 2026-11-19 03:12:32 UTC 批准的 `EX-SA-01`（G-03 门族型验收）把中途签名失败的停止、重试、`--continue` 与 `--abort` 收拢为门族卡 `SA-08j`（多步 `pick` 与 `squash`/`fixup` 折叠两类重放，4 门 44 个判据），撤回 `SA-08k`（编号不复用，并入 `SA-08j`），预检 `SA-08i`、`SA-08l` 移到 `SA-08j` 之前，`SA-08h` 扩为重放后改写（`--signoff` 尾注、交互式消息编辑、`edit` 停止后的修订）先于签名；R38 撤回签名能力预检设计（撤回 `SA-08i`、`SA-08l`，编号不复用；签名失败一律冲突同构停止，与 Git 一致），`SA-08b` 拆出 `SA-08m`（`amend!`、`fixup -C`、`fixup -c`）以逐 action 验收签名开启与关闭两种结果，门族卡 `SA-08j` 拆开合并判据并补失败重试的全量停止态与中止起点判据（64 个判据）；R39 门族卡 `SA-08j` 增加真实签名不可用（G5）与 failpoint 惰性（G6）两门（73 个判据），`SA-09a` 拆出 `SA-09d`（`--abort`），使恢复完成后的状态释放逐条计数；R41 按「每个恢复命令验收成功退出、状态释放与快照」统一口径拆出 `SA-07f`（revert `--abort`）与 `SA-09e`（首封暂停的 `am --abort`），`SA-09a` 改为两条「冲突—恢复」链，`SA-08j` 每个停止态增加 `stopped_sha` 与 HEAD 判据（86 个判据），`TC-0522` 移入 `SA-01c`；R42 把 `TC-0522` 移回 `SA-01`（默认与显式 stop 模式同卡验收，`SA-01c` 只验互斥），多提交空停止的 `--skip`/`--continue`/`--quit` 与 detached HEAD 冲突恢复分别登记 `DEFER-SA-11`、`DEFER-SA-12`，`SA-02` 拆出 `SA-02b`（范围与 `-n`/`--ff`），新增既有行为补测卡 `SA-07h`（`revert --no-commit`），`SA-08j` 增加成功恢复后的最终索引与工作区、G5/G6 的 HEAD 与状态判据并加入无关未跟踪文件（102 个判据）；R43 按用户 2026-11-19 05:17:51 UTC 决定把上游测试复刻类卡收窄到 issue 验收项的核心断言（不扩大 waiver）：撤回 `SA-02b`、`SA-05a`、`SA-05b`、`SA-07h`、`SA-07e`、`SA-07f`、`SA-09c`、`SA-09d`、`SA-09e`（编号不复用），`SA-03` 拆出 `SA-03b`（#727 的重复/混合标志），原 `SA-05a` 的共享编解码函数改由 `REL-SA-02` 家族子卡 `SA-09f` 交付，`SA-09a` 并入原 `SA-09d`/`SA-09e` 的 `--abort` 核心判据，`SA-08j` 为 G2/G4/G5 停止态补工作区判据（108 个判据），收窄出的上游断言登记 `DEFER-SA-13`..`DEFER-SA-20`；R44 因 G-03 的 8 条上限保留 `SA-09e`（unborn 起点首封暂停的 `--abort`，编号未变），`SA-09a` 增加暂停后后代提交的中止保护与状态释放判据，`SA-01b`、`SA-03` 增加点名选项对的冲突诊断判据，`SA-02` 增加中途未跟踪覆盖停止判据（`t3430#28` 登记 `DEFER-SA-14`），`SA-05` 增加未跟踪覆盖保护判据，`SA-06` 增加 `--empty=drop` 不改 HEAD 的判据（`t3505#17` 登记 `DEFER-SA-15`），`SA-07` 定义 orphan/detach 序列并验收无残留状态与 `checkout -b side-branch`，`SA-07c` 单列 `checkout -b side-branch` 成功判据，`SA-08` 补长形式的树序列与签名结果对应 `sixth`/`seventh` 的判据，`SA-09` 改以树与父链判定（作者与消息由既有邮件路径守护），`SA-08j` 更正 R43 插入判据的步骤引用并为 G2/G4 停止态补 `show-ref` 判据（110 个判据）；R45 按 GC-04 统一「拒绝」判据为非 0 退出加点名原因的诊断（clap 冲突的稳定错误码由统一映射承担），新增「验证执行协议」第 11 条说明结构化错误报告即 `--json`/`--machine` 同源包络，`SA-02` 只验收线性范围（非线性遍历序登记 `DEFER-SA-14`），`SA-08` 以 `t7510#2` 验收短形式、以「已是最新」分支验收长形式并都要求 exit 0，`SA-09e` 增加首封暂停后用户 root 提交的中止保护，`SA-09f` 只把恰为 `zero_str(kind)` 的串视为 `None`，`SA-07` 以 `switch --orphan` 验收 orphan 过渡，`SA-07a` 同步 `docs/error-codes.md`，重放后改写路径的二次签名失败登记 `DEFER-SA-08`（不再宣称共用停止路径），unborn 根提交的作者/消息对照登记 `DEFER-SA-20`；R47 按用户 2026-11-19 06:51:45 UTC 决定把 GC-04 改为「每个错误类一条代表路径」：只对本计划新增或改变的错误类在交付卡内选一条代表路径，以退出码、人读输出、`--json`、`--machine` 四条 AC 验收，同类其余路径与只是新可达的既有错误类各断言稳定码（issue 明确要求时加人读诊断），全部按 G-03 计数，删除 R45–R46 不计数的「输出契约门」；`SA-08j` 的门族因 G1 代表路径的 `--json`/`--machine` 增至 112 个判据；同轮撤回 `SA-01c`、`SA-07d`、`SA-07b`（同轮撤回的 `SA-09a`、`SA-09e` 已于 R49 恢复）（编号不复用）；R48 `SA-03b` 的四条停止各断言稳定码；R49 因 GC-07/GC-08 恢复 `SA-09a`、`SA-09e`（各以暂停的稳定码加原 AC1–AC6 计 7 条，原暂停后用户提交的拒绝判据移入重定义的 `SA-09g`），新增 `SA-05c`；R51 新增 `SA-09h`（中断后重试），`SA-09g` 改为统一 fail-closed 判定；R52 新增 `SA-09i`（判定与删除之间的并发移动）；共享回归组、ER-04/ER-06/ER-07 强制门与 D 组不计入 | 29 张活动卡均为 S/M；仅 `SA-08j` 以 `EX-SA-01` 豁免 G-03 条目上限（只豁免计数，不豁免 G-01/G-02/G-05..G-11） |
| 依赖与顺序 | 单链 `SA-01`→…→`REL-SA-02`；跨计划与外部依赖 `DEP-SA-01`..`DEP-SA-10` 只按 ID 引用 | 实施顺序、依赖表、各卡 `Dependencies` 与审计表一致 |
| 完整性 | 每卡含实现、具名测试、文档与网站交付；家族网站由所属发布点（`REL-SA-01`、`REL-SA-02`）发布；全部提交以 `-S` 强制签名 | 「文档与网站验收命令」、「提交签名」 |
| 安全性 | 只经单一出口签名（重放循环、`--continue` 与重放后改写路径）；出口级签名失败不写对象（`SA-08a`），8 个 action 在签名开启与关闭时分别得到有效新签名与无旧签名的结果（`SA-08a`、`SA-08b`、`SA-08m`），重放后的改写都先于签名（`SA-08h`），任一经 `create_replayed_commit` 构造提交的重放步签名失败（含签名不可用）按冲突同构停止，可重试、`--continue` 不丢补丁或 `--abort` 复原（`SA-08j`，与 Git 一致、无预检）；重放后改写路径（交互式消息编辑的二次提交、`edit` 停止后的修订提交）的二次签名失败只保证出口不写对象，其恢复合同登记 `DEFER-SA-08`；不在证据中保存密钥 | `SA-08a`、`SA-08b`、`SA-08m`、`SA-08h`、`SA-08j` 的 AC |
| 功能正确性 | 各 AC 以 Git 对照或明确退出码/错误码/状态断言 | 删除自动生成的无效「AC → 断言映射」 |
| 接口兼容 | `SA-01` 新增 `LBR-CONFLICT-001` 退出码 1 特例并同步 `docs/error-codes.md`；`SA-07a` 在 `LBR-CONFLICT-001` 下增列 revert 已暂存改动拒绝、`SA-08j` 在 `LBR-AUTH-001` 下增列 rebase 重放签名失败停止（均同步 `docs/error-codes.md`）；`SA-07` 删除模块私有枚举变体 `RevertError::DetachedHead`（`revert.rs:61` 为私有 `enum`，不属 `T-3`，错误码表不变） | `SA-01` 登记 `T-1` 全量触发 |
| 数据流与控制流 | 停止态只在可恢复停止时持久化（`ADR-SA-01`）；unborn 用全零 OID（`ADR-SA-02`） | `SA-09f`、`SA-09b` 验证全零编解码与 `am` 字段接线；`SA-09a`、`SA-09e` 验证 unborn 起点 `am` 两种暂停后 `--abort` 的状态释放、符号 HEAD 保留与全量快照复原，`SA-09g` 验证 `head_orig=None` 会话中 HEAD 已不同于暂停点时拒绝中止并保留状态、提交、符号 HEAD、索引与工作区（第二封暂停后用户另行提交情形登记 `DEFER-SA-20`）；`SA-09h` 验证中断后重试，`SA-09i` 验证删除前分支被并发移动时拒绝；unborn 起点暂停后的 `--continue`/`--skip` 登记 `DEFER-SA-20`；revert 停止态保留与清理为既有行为，由 `SA-07a`、`SA-07` 共享回归组中的既有用例守护（`DEFER-SA-18`）；GC 对全零 OID 的跳过为既有逻辑（专项验收 `DEFER-SA-20`） |
| 性能与容量 | 范围展开单次可达遍历、每个提交至多加载一次 | `SA-02` 计数型单元测试 |
| 可靠性与容错 | 冲突与重放步的签名失败都形成可 `--continue`/`--abort` 恢复的停止态，签名失败的 `--continue` 可重试（重放后改写路径的二次签名失败见 `DEFER-SA-08`）；活动 unborn 态禁止直接降级 | 故障恢复矩阵、各卡 `Migration and rollback` |
| 可维护性 | 签名、全零 OID 转换各有单一出口 | `SA-08a`、`ADR-SA-02` |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 03:11:00 UTC | 初稿创建 | 基于 `agent_2_tasks.json` 起草 9 张任务卡覆盖 9 个 issues | `N/A` | 全文 |
| 2026-11-17 03:27:00 UTC | Claude R1 FAIL + Codex R1 FAIL | 修正 `SA-04`、`ADR-SA-02`、`SA-01`/`06`/`07`/`08`/`09` 与 `T-1`/`T-4` | `N/A` | ADR、实施顺序、依赖表、粒度表、任务卡、追溯表 |
| 2026-11-17 03:34:00 UTC | Codex R2 FAIL | 为 `SA-01` 补充 `TC-0427`/`TC-0522`，为 `SA-08` 补充 `DEP-SA-02`、`DEFER-SA-02` 与 `#780` 评论 TC，逐项闭环 R1 P2 | `N/A` | `SA-01`、`SA-08`、依赖表、延后项、日志 |
| 2026-11-17 03:52:00 UTC | Claude R3 FAIL + Codex R3 FAIL | 补齐 `#726` 评论 TC（`SA-02`/`SA-03`）；`SA-06` 纳入 `--empty=keep`；`SA-04` 对齐 OID/`DEP-SA-01`；决断 `ADR-SA-02`；拆细 `SA-07`/`SA-08`；修正测试矩阵与 `DEFER-SA-03` | `SA-07`→`SA-07,07a,07b`; `SA-08`→`SA-08,08a` | 全文、实施顺序、审计表、测试矩阵、延后项、日志 |
| 2026-11-17 03:59:00 UTC | Claude R4 FAIL + Codex R4 FAIL | `SA-01` 接入 `DEP-SA-01`、`SA-09` 新增 `DEP-SA-04`、`SA-07a` 补 `commit.rs` 与 `DEP-SA-05`；`ADR-SA-02` 改用现行表 `sequence_state`、纳入 `AmState.expected_head`/`current_oid` 并限制活动期降级；按 G-09 保留 `SA-07`/`SA-08` 编号并将 `SA-08`/`08a` 纳入 `REL-SA-01`；修正 `SA-01`/`07b` 锚点、`SA-07` `cherry-pick -` 与 `#728` AC3、`SA-04` 备注移出 `COMPATIBILITY.md`、`SA-08a` 单轴重签 | `N/A` | 关系表、依赖表、`ADR-SA-02`、实施顺序、审计表、`SA-01/04/07/07a/07b/08/08a/09`、风险表、日志 |
| 2026-11-17 04:08:00 UTC | Codex R5 FAIL | 同步 `SA-09` 依赖为 `SA-08a, DEP-SA-04`；清理 `SA-04` AC4 中 `COMPATIBILITY.md` 写入 issue 原文差异的冲突要求；将测试矩阵中的 `\|` 过滤表达式拆分为独立的 `&&` 命令 | `N/A` | `SA-04`、`SA-09`、测试矩阵、日志 |
| 2026-11-17 04:12:00 UTC | Codex R6 FAIL | 按 G-08 新增独立 `release` 卡 `REL-SA-01` 并将 `SA-08` 与 `SA-08a` 均设为 `family child`（`Release write set=N/A`、`Version increment=none`），补齐禁止推送窗口与失败恢复顺序；按 `AGENTS.md` 将网站文档写集改为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支） | `N/A` | 使用规则、实施顺序、审计表、`SA-08`、`SA-08a`、`REL-SA-01`、`SA-09`、追溯表、日志 |
| 2026-11-17 04:21:00 UTC | Codex R7 FAIL | 消除 `REL-SA-01` 循环完成门（子卡 `SA-08`/`08a` 先达 pre-release A/B 门控，待 `REL-SA-01` 发布后继承 C/D 覆盖再置 `done/complete`）；将 `SA-08`/`08a` `C/D coverage from` 改为 `REL-SA-01`；`REL-SA-01` 补齐 `T-2` 与 `source .env.test && source .env.live-test` 全量命令 | `N/A` | `SA-08`、`SA-08a`、`REL-SA-01`、日志 |
| 2026-11-17 04:55:00 UTC | Codex R9/R10 FAIL | 修正 `release.yml` 顺序；取消 `REL-SA-01` 改为 `SA-08a` 前置 `SA-08`；修正 `SA-08` 内容寻址 OID 语义；`SA-09` 补 Git 历史比对 | 移除 `REL-SA-01` | 全文 |
| 2026-11-17 05:15:00 UTC | Claude R12/R13 FAIL + Codex R11/R13 FAIL | 补齐 `DEP-SA-07`（`672.md`/`547.md`/`1041.md`）与 `DEP-SA-08`；`ADR-SA-02` 改用 null-OID（`ObjectHash::zero()`）并覆盖 `2026071901_sequencer_worktree_scope.sql` 及 `maintenance.rs`/`pseudo_ref.rs` 适配；`DEP-SA-04` 改 `PM-10:618`/`CSR-31:1244`/`CSR-32:1279`；`DEP-SA-01` 补 `CS-02a`；`DEP-SA-06` 收窄 `LS-23..27`；`SA-07a` 清理前置 `SA-07` 并拆出 `SA-07d`（`commit -a`）；`SA-07c` 归 `#728` AC5/6 并重锚 `switch.rs:473`；`SA-08a` 重锚 `create_replayed_commit`（`:5850-5910`）并将 `TC-0911`/`0931` 移至 `SA-08`；`SA-06` 仅改 `show.rs`；`SA-01` 明示 `LBR-CONFLICT-001`+`T-1`；`SA-09` 显式覆盖降级约束；同步 `show` 文档收口与 `plan-long.md` | `SA-07a`→`SA-07a,07d` | 全文 |
| 2026-11-17 05:35:00 UTC | Codex R15/R16 FAIL | 修复审计表 `SA-07b`/`SA-08a` `AC`/`VER` 列互换；将 `SA-09` 拆分为 `SA-09`（unborn `am` 首提交与多补丁）与 `SA-09a`（unborn `am` 冲突 `--continue`/`--skip`/`--abort` 恢复与暂停态 `maintenance` GC 根/`ORIG_HEAD` 回归门）；更新头部为 `R17` 与 `15` 张卡 | `SA-09`→`SA-09,09a` | 头部、实施顺序、Phase 1、审计表、`SA-09`/`SA-09a`、追溯表、M2、风险表、日志 |
| 2026-11-17 05:50:00 UTC | Claude R17 FAIL + Codex R17 FAIL | `DEP-SA-08` 改等 `MP-30`（`pseudo_ref.rs`）并从 `SA-05` 移除 `maintenance.rs`；`ADR-SA-02` 改用 `ObjectHash::zero_for_kind(kind)` / `zero_str(kind)`；`DEP-SA-07` 接入 `SA-01`；`SA-09`+`SA-09a` 纳入 `REL-SA-02` 家族发布；`SA-08`/`SA-01`/`SA-06`/`SA-07` 原子谓词 `<= 8`；`SA-01` 错误码改 `LBR-CLI-002`；`SA-07` 移除 `cherry_pick.rs` 并补 `T-3`；`SA-03` 补 `COMPATIBILITY.md`；`SA-07d` 补 `commit` 文档；对齐 `DEP-SA-01`（`REL-CS-00`）、`DEP-SA-05`（`DEP-SA-07a`）、`默认覆盖` 与 `实施顺序` | `N/A`（新增 `REL-SA-02`） | 全文 |
| 2026-11-17 06:55:00 UTC | Codex R19 FAIL | 明确 `SA-01` `LBR-CONFLICT-001` 命令级退出码 `1` 特例并纳入 `docs/error-codes.md` 写集与精确退出码验证；明确 `REL-SA-02`（`SA-09`/`SA-09a`）家族窗口内网站 `am.en.md` 仅本地构建与本地 `cf` 提交、由 `REL-SA-02` 发布后统一 `cf` push 与远端 HTTP D 门；将 `DEP-SA-03` 接入 `SA-01`..`SA-09a` 全部 `COMPATIBILITY.md` 写入卡的 `Dependencies`/审计表；修正故障恢复矩阵 `ObjectHash::zero_for_kind(kind)` / `zero_str(kind)` | `N/A` | 依赖表、发布分组、审计表、`SA-01`..`SA-09a`、`REL-SA-02`、追溯表、故障恢复矩阵、日志 |
| 2026-11-17 12:27:28 UTC | 用户单审指令 | 计划提交门改为 Codex 单审，不再要求 Claude Review | N/A | 头部、M0、完成判据 |
| 2026-11-17 17:50:00 UTC | 机械自审：网站交付合同 | 补充权威 checkout 校验、按卡同步与前端验证、最终网站字节独立 Claude Code PASS、签名推送和部署；REL-SA-02 家族最终草稿在 CLI 发布后重新验证审查 | N/A | 使用规则、发布分组 |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（原记「用户最新指令指定」，当时未注明出处；R30 注：该路径已由用户 2026-11-18 11:55:09 UTC 确认；`AGENTS.md:135` 的 `/Volumes/Sea/libra-backend` 已过时且本机不存在，属已知文档滞后），并在「文档职责」写明权威依据与 STOP 条件；`SA-09a` 分别覆盖首封与后续补丁暂停态、跨进程 GC 根枚举与 `ORIG_HEAD`，并在 HEAD 移开后断言 `sequence_state.current_oid` 本身为 GC 根；`REL-SA-02` 要求在最终版本提交树重跑全量门并绑定 SHA；`SA-02` 记录 G-03 豁免阻断与按行为轴拆分的设计；`SA-08a` 记录八个真实重放 action 的签名合同阻断 | N/A | 全文对应章节 |
| 2026-11-18 09:44:18 UTC | Codex 复审 FAIL 修复（验证契约与门禁） | （时间更正：本行原记 `2026-11-18 10:35:00 UTC`，晚于本文件该次写入后的最后修改时间 `2026-11-18 09:44:18 UTC`，属未来时间，R22 按文件修改时间更正；另经 R22 核对，本行所称「文档与网站验收命令」块实际未写入，已在 R22 补写）新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 10:13:32 UTC | 用户双审指令 | 计划文本提交门由 Codex 单审改为 Claude（Subagent）与 Codex 双审，二者对同一受审 SHA-256 均 `VERDICT: PASS` 方可提交；执行期 ER-05 评审门与网站 Claude Code PASS 门不变 | N/A | 头部、使用规则、M0、完成判据 |
| 2026-11-18 10:46:29 UTC | Claude R22 FAIL + Codex R22 FAIL | 按双审 R22 意见整改：①全部 nextest 过滤改为 `command_test` 真实路径 `command::<模块>::<函数>`（库内 `command::<模块>::tests::<函数>`），重写「验证执行协议」区分卡特有用例（list 恰好 1 行 + `--no-tests=fail` 运行）与共享回归组（不计 VER），修复未闭合反引号；②补写此前只被引用、实际不存在的「文档与网站验收命令」（仓库逐文件验收、网站 checkout 前置、独立卡/家族子卡/家族发布点三种网站流程与 D4 部署核验）；③删除自动生成的无效「AC → 断言映射」，各 AC 改为含退出码、错误码、输出与状态的可判定断言并绑定具名测试；④取消无可核验审批的 `EX-SA-02-RANGE`，`SA-02` 收敛为 8 个范围谓词（合并提交预检拒绝、`-m`/其它修订语法登记 `DEFER-SA-04`），`--continue` 与 rerere 互斥及 `t3504#7` 全流程并入 `SA-03`；⑤`SA-08a` 按 `ADR-SA-03` 以单一签名出口覆盖 8 个 rebase action，补签名失败可恢复与 SSH 验证；⑥网站 checkout 改为可核验的两候选判定规则（`AGENTS.md:135` 与 ER-06a 同级目录）；⑦`DEP-SA-07` 改为 `I1041-06`/`I1041-07`，新增 `DEP-SA-09`（`I672-01`、`FINAL-547`），跨计划引用去掉对侧行号；⑧`SA-07a` 新增 revert 已暂存改动拒绝，`SA-07` 去掉无源码依据的「AI 托管分支锁」并改正 `T-3` 误用（`RevertError` 为私有枚举），`SA-09a` 补 `am` 行 `ORIG_HEAD` 投影（新增 `DEFER-SA-05`），`SA-05`/`SA-09a` 的 GC 门改为可执行的 `libra gc --prune=now` 黑盒；⑨家族子卡 `Version increment` 改 `N/A`、`Full-suite trigger` 改 `N/A`；`SA-07d` 标 `T-1`、`SA-08a` 标 `T-5`；⑩重写评审维度表、审计表、测试矩阵、追溯表、故障恢复矩阵、风险表、性能摘要与完成判据；更正 `10:35:00` 未来时间戳并补登 R20–R21 | 无新卡；撤销 `EX-SA-02-RANGE` | 全文 |
| 2026-11-18 11:05:43 UTC | Claude R23 FAIL + Codex R23 FAIL | 按双审 R23 意见整改：①在「验证执行协议」第 0 条定义 G-03 计数口径（一个场景一次整体快照比较计 1 项、不同输入各计 1 项、VER 按独立运行命令计），全部卡按此重写 AC 并重数；②按口径拆出 `SA-01b`（`--keep-redundant-commits`/`--empty` 与四个控制选项互斥，8 项）；`SA-02` 以 `TC-2332` 作线性场景并拆开冲突停止与 `--abort`；`SA-03` 只保留与 `--continue` 互斥，其余控制选项登记 `DEFER-SA-06`；`SA-05` 加入 GC 隔离黑盒的未入根对照对象并限定 unborn pick 的 fixture；`SA-06` 逐个格式输入成项；`SA-07a`/`SA-07` 把 `checkout` 后续步骤单列；`SA-08` 合并同一步骤的 TC 并拆开 `-f`/`--force-rebase`；③`SA-08a` 改为类型约束的单一签名出口（`UnsignedReplayContent` + `finalize_replayed_commit`，8 个 action 由编译器保证经出口），纯函数单元测试 + `pick`/`amend!` 代表性 CLI 场景，补具体 fixture；④`SA-09` 注册检查拆为 3 个独立门并补 SHA-256 往返；`SA-09a` 去掉无法证明根保护的 GC 断言（由 `SA-05` 的 kind 无关黑盒承担）；⑤新增「验证执行协议」第 6 条：所有提交 `libra commit -S -s` 强制签名并做同一提交校验（`DEP-SA-01` 后默认不签名）；网站前置检查允许家族先序子卡已登记 SHA-256 的草稿；⑥`REL-SA-02` 定义 `VERSION` 并按口径重写 AC；⑦更正评审维度表 `T-3` 误述、`plan-long.md` 改述为执行期义务、`nextest list` 行解析规则、D 组标题与 ER-04 第 ⑨ 步引用 | `SA-01` → `SA-01`, `SA-01b` | 全文：头部、实施顺序、发布分组、Phase 0、审计表、`SA-01`..`SA-09a`、`REL-SA-02`、追溯表、风险表、延后项、完成判据 |
| 2026-11-18 11:17:17 UTC | Claude R24 FAIL + Codex R24 FAIL | 按双审 R24 意见整改：①撤销 R23 的「一次快照比较计 1 项」口径，改为每条 AC 只陈述一个可独立判定的性质并逐条计数（「验证执行协议」第 4 条），全部卡按此重写并重数；②删除冗余的 `cargo nextest list` 前置门（`test(=…)` 全等匹配至多 1 个，`--no-tests=fail` 拒绝零匹配），VER 只计实际运行的验证命令；③`SA-08a` 只迁入 `pick`/`reword`/`edit` 并给出逐 action fixture，拆出 `SA-08b`（`fixup`/`squash`/`amend`/`fixup -C`/`fixup -c` 迁入同一出口，含 `--signoff` 先尾注后签名），签名失败的失败瞬间状态与 `--abort` 恢复分列判据；④`SA-09` 注册检查只核对两个具名测试各出现一次，`REL-SA-02` 聚合守卫纳入注册检查；⑤网站 checkout 改为严格按 `AGENTS.md:135`（`/Volumes/Sea/libra-backend`），不存在即 STOP；网站提交前核对暂存集、本地 `cf` 无超前提交与远端基线，推送前复核远端未前进；文档门增加无变化通过路径；部署标记句须推送前零命中以绑定本次部署；⑥全部独立发布卡的 `Full-suite trigger` 登记 `T-1`（C 组 bump 修改 `install.sh`/`install.ps1`）；⑦D 组新增 D4 发布产物下载可获取性（`download.libra.tools`），网站部署改为 D5；⑧完成判据定义最终代码提交 `CODE_SHA`、只改 `docs/development/plan/` 的记账提交不触发重跑，并预登记模板 v2.14 终止链 D 组证据落点；⑨更正测试矩阵的 VER 引用 | `SA-08a` → `SA-08a`, `SA-08b` | 全文：头部、文档职责、验证执行协议、文档与网站验收命令、实施顺序、发布分组、Phase 1、默认覆盖、审计表、全部任务卡、追溯表、测试矩阵、风险表、完成判据 |
| 2026-11-18 11:23:28 UTC | Claude R25 FAIL + Codex R25 FAIL | 按双审 R25 意见整改：①继续把仍含多个性质的 AC 拆成单一性质（`SA-02` AC4、`SA-03`、`SA-07a`/`SA-07` 的状态清理改用 `libra revert --continue` 报 `no revert in progress` 判定、`SA-08` 拆开 `HEAD^`/`HEAD` 验签、发布卡只保留卡特有判据）；②`SA-08a`/`SA-08b` 改为家族子卡，新增唯一发布点 `REL-SA-01`（8 个 rebase action 全部收口后才对外发布），`SA-08` 依赖 `REL-SA-01`，`ADR-SA-03` 后果同步；③新增 `DEP-SA-10`（执行机器上的 `/Volumes/Sea/libra-backend` 网站检出），所有卡以其为依赖；④「验证执行协议」：精确过滤判定不再要求 0 skipped（nextest 把被过滤用例计入 skipped）；多 target 命令按 target 分别记录；第 5 条以逐文件 SHA-256 把被测内容绑定到提交 `TESTED_SHA`；新增第 8 条 SSH 验签的字节级提取规则与改字节负对照；⑤网站流程：`curl` 与 `rg` 分步执行防止下载失败被当成零命中；家族发布点以草稿相对 `cf` 的改动判断，不得走无漂移捷径；⑥发布卡由同一工厂生成，强制门（ER-04/ER-05/ER-07、D 组）在 Description 列出命令且不计入，卡特有判据各有命令；⑦G-09 拆分来源统一（`SA-08` → `SA-08a` → `SA-08b`）；D4/D5 引用统一 | `SA-08a`, `SA-08b` 转为 `REL-SA-01` 家族子卡；新增 `REL-SA-01` | 全文：ADR-SA-03、验证执行协议、文档与网站验收命令、依赖登记表、实施顺序、发布分组、Phase 0/1、默认覆盖、审计表、全部任务卡、追溯表、里程碑、故障恢复矩阵、风险表 |
| 2026-11-18 11:28:34 UTC | Claude R26 FAIL + Codex R26 FAIL | 按双审 R26 意见整改：①拆出 `SA-01c`（`--empty` 与控制选项互斥），`SA-01b`/`SA-01c` 各增「停止态未被触碰」判据；②`SA-02` 撤销范围预检承诺，范围内合并提交沿用现有逐提交 `MergeCommitUnsupported` 语义，AC4 只判错误码，`DEFER-SA-04` 与风险表同步；③`SA-07a` 以 `<gitdir>/revert-state.json` 的存在性判定状态保留与清理（revert 不使用 `sequence_state`）；`SA-07` AC6 改为后续 `checkout` 成功；④`SA-09` 把两个字段、两种哈希的往返拆成独立判据，测试注册检查归入 ER-06/GC-06 索引同步（不计入）；`REL-SA-02` 聚合守卫相应调整；⑤发布卡 tag 与 release note 拆成两个独立门，去掉 `gh pr view <PR>` 占位；⑥「验证执行协议」第 5 条改为在干净工作区的本卡提交 `TESTED_SHA` 上运行全部门（覆盖新增、删除、二进制文件）；第 3 条定义「提交序列/主题序列」比较；⑦网站第 2d 步按所属家族（`REL-SA-01`/`REL-SA-02`）接受先序子卡草稿；全局规则与评审维度表改为按所属发布点交付；完成判据承认第 3c 步无漂移路径；⑧`DEP-SA-10` 增加 7 日等待上限、升级对象与不得改用其它检出的回落；⑨D 组表头纳入 `REL-SA-01`，D4 的 `VERSION` 取值与 D4/D5 引用统一 | `SA-01b` → `SA-01b`, `SA-01c` | 全文：头部、使用规则、评审维度表、验证执行协议、文档与网站验收命令、依赖登记表、实施顺序、发布分组、Phase 0、审计表、`SA-01b`/`SA-01c`/`SA-02`/`SA-07a`/`SA-07`/`SA-09`/`REL-SA-01`/`REL-SA-02`、追溯表、风险表、延后项、完成判据 |
| 2026-11-18 11:34:14 UTC | Claude R27 FAIL + Codex R27 FAIL | 按双审 R27 意见整改：①按「推荐拆分维度」的数据/状态轴拆出 `SA-05a`（`cherry-pick` 状态全零编码、`ORIG_HEAD` 投影与 GC 安全，独立发布、无用户可见变化）与 `SA-09b`（`am` 两字段两种长度的编码/解码 8 个判据，`REL-SA-02` 家族子卡 1/3）；`SA-05`、`SA-09` 只保留行为判据；②拆出 `SA-08c`（签名失败 fail-closed：tip 不变、进行中状态、无新增 commit 对象、`--abort` 恢复），`REL-SA-01` 家族改为 3 张子卡；③「验证执行协议」第 5 条改为按模板顺序在提交前运行 A/B 与 C 组测试，用 `--untracked-files=all` 的被测快照（`hash-object` blob OID、可执行位、删除标记）与提交后 `rev-parse <TESTED_SHA>:<路径>`/`ls-tree` 逐路径核对绑定被测内容；文档门 1a 改为提交前记录、提交后用 `diff --name-only BASE_SHA TESTED_SHA` 核对；④`SA-07a` 以 `$(libra rev-parse --git-dir)/revert-state.json` 断言，linked worktree 路径解析列为越界既有行为；⑤`ADR-SA-02` 说明 BLAKE3 与 SHA-256 全零串相同、按 20/32 字节两种长度覆盖 | `SA-05` → `SA-05a`, `SA-05`；`SA-09` → `SA-09b`, `SA-09`；`SA-08a` → `SA-08c` | 全文：头部、当前缺口、关系表、ADR-SA-02/03、验证执行协议、文档与网站验收命令、依赖登记表、实施顺序、发布分组、Phase 0/1、默认覆盖、审计表、相关任务卡、追溯表、测试矩阵、风险表、性能摘要 |
| 2026-11-18 11:42:17 UTC | Claude R28 FAIL + Codex R28 FAIL | 按双审 R28 意见整改：①撤销 `SA-08c`，签名失败语义（stderr、tip 不变、无新增 commit 对象、`--abort` 恢复）并回签名出口所在的 `SA-08a`（含隔离 HOME/配置与「先建历史后开签名」的确定性 fixture），`SA-08b` 承担其余 7 个 action 与 `--signoff` 签名，`REL-SA-01` 回到 2 张子卡；②在 `src/internal/sequencer/mod.rs` 设唯一编解码函数：`SA-05a` 交付并接入 `cherry-pick`（两种长度 × 编码/解码/非零旧行兼容共 6 判据），拆出 `SA-05b`（全零行的 `ORIG_HEAD` 投影与 GC 隔离黑盒）；`SA-09b` 把 `am` 两字段接入同一函数（含非零旧行兼容）；`SA-09a` 只保留冲突恢复，拆出 `SA-09c`（`am` 行 `ORIG_HEAD` 投影）；③`ORIG_HEAD` 缺失判据改为非 0 退出、stdout 为空与（`am`）stderr 不含 `unknown sequence kind` 分列；④发布卡只保留提交前判据（改动集、版本面、聚合守卫），合入树一致（新增 D6）与 tag/release note（D3）归 D 组；⑤`DEP-SA-10` 只约束有网站影响的卡（`SA-05a`、`SA-05b`、`SA-09b` 除外），D5 只适用于有网站影响的卡，里程碑与完成判据同步；⑥补齐 G-09 双向拆分声明与家族序号 | `SA-05` → `SA-05a`, `SA-05b`, `SA-05`；`SA-09a` → `SA-09a`, `SA-09c`；撤销 `SA-08c` | 全文：头部、关系表、ADR-SA-02/03、依赖登记表、实施顺序、发布分组、D 组登记表、Phase 0/1、默认覆盖、审计表、相关任务卡、追溯表、测试矩阵、里程碑、风险表、完成判据 |
| 2026-11-18 11:46:04 UTC | Claude R29 FAIL + Codex R29 FAIL | 按双审 R29 意见整改：①发布卡按模板顺序拆分：A/B 阶段只核对改动集与家族聚合守卫（集成与库内分列为独立判据），版本面 parity 与 bump 后 `T-2` 全量归 C 组强制门；「验证执行协议」新增第 1b 条定义聚合守卫的并集过滤与判定；②全零编解码统一改用 `parse_hex_for_kind(kind, …)`（`parse_repo_oid` 读进程全局 kind），ADR 与 `SA-05a`/`SA-09b` 同步；③`SA-08b` Description 列全 7 个 action 并在本卡内完整定义 fixture；`SA-08a` 分别定义成功（含 vault、签名历史）与失败（`--vault false`、无签名历史）fixture，失败路径补「`rebase in progress`」与「`--abort` exit 0」判据；④网站无漂移路径仍执行前端验证与独立 Claude Code 评审，只跳过提交、推送与部署；文档门 1c 只在改 `docs/commands/**` 时适用；⑤收口差异检查先判 `libra diff --name-only` 的退出码；⑥`SA-09a` 的 GC 回归改为精确复跑 `SA-05b` 的具名用例；`SA-09` 注册检查在文档门给出具体 `nextest list` 命令；兼容收口清单写明两条网站证据路径 | 无新卡 | 验证执行协议、文档与网站验收命令、ADR-SA-02、`SA-05a`、`SA-08a`、`SA-08b`、`SA-09`、`SA-09a`、`REL-SA-01`、`REL-SA-02`、审计表、追溯表、兼容与文档收口、完成判据 |
| 2026-11-18 11:56:44 UTC | Claude R30 FAIL + Codex R30 FAIL | 按双审 R30 意见整改：①`SA-07a` 只保留已暂存改动前置拒绝，补错误码 `LBR-CONFLICT-001`、HEAD/索引（`libra ls-files --stage`）/工作区不变与无状态文件的独立判据；冲突停止保留、`--continue` 成功后、`--abort` 后与无冲突完成后的 `revert-state.json` 清理拆为 `SA-07e`（含 `--continue` 后状态文件不存在的独立判据）；②两张发布卡新增已提交改动集守卫（`libra diff --name-only "$FAMILY_BASE_SHA" HEAD`，非 0 即 STOP），未提交改动集守卫保留为独立判据；「验证执行协议」新增第 9 条定义 `FAMILY_BASE_SHA`；③`SA-08a` 补签名失败后 `--abort` 恢复原分支 tip、`--abort` 后不再处于 rebase 进行中、修正签名配置后 `--continue` 完成三条独立判据，`pick` 的 OpenPGP/SSH 正向签名拆为家族子卡 `SA-08c`（`DEP-SA-02` 改由 `SA-08c` 消费），`REL-SA-01` 改为三张子卡；④`show` 无条件换行锚点改为 `src/command/show.rs:1164-1165`；⑤家族子卡 `Migration and rollback` 内联完整降级规则，不再写「同 `SA-09`」；`Verification` 表头写明发布点聚合守卫按第 1b 条；⑥按协调者转达的用户确认，「文档职责」与 `DEP-SA-10` 把 `$LIBRA_BACKEND` 定为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支，用户 2026-11-18 11:55:09 UTC 确认），`AGENTS.md:135` 的 `/Volumes/Sea/libra-backend` 登记为已知文档滞后（本计划不改该文件），删除「请用户书面指定替代检出」前置，现场核对该检出满足第 2 步；`2026-11-18 08:19:08 UTC` 修订行补注该路径依据的出处 | `SA-07a` → `SA-07a` + `SA-07e`；`SA-08a` → `SA-08a` + `SA-08c` | ADR-SA-01、ADR-SA-03、验证执行协议、文档与网站验收命令第 4 步、实施顺序、`DEP-SA-02`、发布分组与并发窗口、Phase 1、默认覆盖、`SA-06`、`SA-07a`、`SA-07e`、`SA-07`、`SA-08a`、`SA-08b`、`SA-08c`、`SA-08`、`REL-SA-01`、`REL-SA-02`、`SA-09`、`SA-09a`、`SA-09c`、审计表、测试矩阵、追溯表、风险表、性能摘要、GAP-05..07、评审维度表、文档职责、`DEP-SA-10`、事实基线「网站同步」行 |
| 2026-11-18 12:03:30 UTC | Claude R31 PASS + Codex R31 FAIL | 按 Codex R31 意见整改（并吸收 Claude R31 的 P2）：①撤销 R30 新增的 `SA-08c`（不复用该编号）：`pick` 的 OpenPGP/SSH 正向签名与 `--signoff` 尾注（尾注行与签名有效分列两条 AC）回到出口所在的 `SA-08a` 同卡验收，`DEP-SA-02` 改回由 `SA-08a` 消费；签名失败语义按行为轴拆为 `SA-08d`（非 0 退出、stderr、tip 不变、无新增 commit 对象、进行中状态）、`SA-08e`（`--abort` exit 0、HEAD 挂回原分支、分支 tip、索引、工作区恢复与进行中状态清除）、`SA-08f`（修正配置后 `--continue` exit 0、状态清除、重放结果、HEAD 在原分支），`REL-SA-01` 改为五张子卡；②`SA-08b` AC8 只验 `squash!` 折叠结果签名有效（尾注由 `SA-08a` 验收）；③`SA-09` 补单封补丁结果树等于 Git 对照；④`DEP-SA-06` 改按对侧计划实际写 `show.rs` 的 `LS-23`、`LS-33`、`LS-24`、`LS-25`、`LS-34` 登记并写明可核验产物；⑤卡数统一为 28 张（26 张实现卡 + 2 张 `release` 卡）；⑥`SA-07d` 的状态清理承接改为 `SA-07e`，`SA-07e` 证据补 `src/internal/sequencer/mod.rs:1113`；`SA-09` 的 GC 承接改为 `SA-05b`；去掉 `Security and privacy` 与默认覆盖表中的「同 `SA-…`」写法；`SA-08a` fixture 写明 SSH 测试密钥与 `allowed_signers` 的生成 | `SA-08c`（R30）撤销 → `SA-08a` + `SA-08d` + `SA-08e` + `SA-08f` | 目标、ADR-SA-03、文档与网站验收命令第 4 步、实施顺序、`DEP-SA-02`、`DEP-SA-06`、与其它计划的关系、发布分组与并发窗口、Phase 1、默认覆盖、`SA-07d`、`SA-07e`、`SA-08a`、`SA-08b`、`SA-08d`、`SA-08e`、`SA-08f`、`SA-08`、`REL-SA-01`、`SA-09`、审计表、测试矩阵、追溯表、风险表、性能摘要、GAP-07、评审维度表 |
| 2026-11-18 12:09:16 UTC | Claude R32 PASS + Codex R32 FAIL | 按 Codex R32 意见整改：①签名出口的失败处理回到出口所在的 `SA-08a` 同卡交付与验收（签名失败非 0 退出、tip 不变、无新增 commit 对象），并在 `SA-08a` Description 写明三卡行为边界（`SA-08a`「要么正确签名、要么什么都不写」，`SA-08d` 停止态诊断与 `--abort` 精确回退，`SA-08f` 修正配置后 `--continue`）；撤销 R31 的 `SA-08e`（并入 `SA-08d`，编号不复用），`REL-SA-01` 改为四张子卡；②`SA-08a`、`SA-08b` 与签名失败 fixture 明确从基点 `B` 分叉（`main` 上只改 `u.txt` 的 `<upstream>`、`topic` 上只改 `f.txt` 的 `T`），保证 rebase 必须重放、不走「已是最新」短路（`src/command/rebase.rs:3690-3698`），失败态测试先断言进入签名失败分支；③`SA-08f` 补「`--continue` 之后 `topic:f.txt` 等于 `T:f.txt`」判据；④「验证执行协议」第 9 条改为逐提交核对家族提交序列：9a 提交身份（每个提交为子卡记录的 `COMMIT_SHA` 或单父记账提交）、9b 逐提交改动集、9c 最终改动集、9d 上游前移重建时重新绑定，两张发布卡相应改为 5/6 项判据；⑤`SA-08` 的「已是最新」短路锚点由 `rebase.rs:3416-3422`（gitlink 预检）更正为 `rebase.rs:3690-3698` | `SA-08e`（R31）撤销 → 并入 `SA-08d` | 目标、ADR-SA-03、验证执行协议第 9 条、文档与网站验收命令第 4 步、实施顺序、发布分组与并发窗口、Phase 1、默认覆盖、`SA-08a`、`SA-08b`、`SA-08d`、`SA-08f`、`SA-08`、`REL-SA-01`、`REL-SA-02`、审计表、测试矩阵、追溯表、风险表、GAP-07、评审维度表 |
| 2026-11-18 12:15:53 UTC | Claude R33 PASS + Codex R33 FAIL | 按 Codex R33 意见整改（并吸收 Claude R33 的 P2）：①`ADR-SA-03` 改为签名失败 fail-closed 并原子回滚（复用 `run_rebase_abort`（`src/command/rebase.rs:4533-4665`）恢复 HEAD、原分支、索引与工作区并清除 rebase 状态，非 0 退出并输出诊断；修正配置后重跑同一命令；与 Git 停在进行中状态的差异写入 `COMPATIBILITY.md` 与文档），产生失败的 `SA-08a` 同卡验收全部回滚性质（非 0 退出、stderr、HEAD 挂回 `topic`、`topic` tip 等于 `T_SHA`、`status --short` 干净、无 `rebase in progress`），不再有需要后续卡收口的停止态；撤销 R32 的 `SA-08d`、`SA-08f`（编号不复用），`gpg.format=ssh` 拆为 `SA-08g`（`pick` 与 `squash!` 两条 SSH 验签，消费 `DEP-SA-02`），`--signoff` 拆为 `SA-08h`（不签名时无 `gpgsig`、尾注行、签名有效三条），`REL-SA-01` 子卡为 `SA-08a`/`SA-08b`/`SA-08g`/`SA-08h`；②「验证执行协议」9a 增加独立记账提交定义与完整性方向（单列为发布卡判据），9b 允许子卡提交含 `docs/development/plan/` 记账路径（模板要求同变更更新 `plan-status.md` 与证据），9d 要求重建后各子卡重跑全部门、被测字节变化即重新取得 ER-05 评审，发布点在新最终树上重跑 9a–9c 与聚合守卫；两张发布卡改为 6/7 项判据；③fixture 以 `T_SHA="$(libra rev-parse HEAD)"` 记录待重放提交，不再引用未创建的 `T` ref；④`SA-05` 补 unborn 普通 pick 结果树等于 Git 对照；⑤回滚证据改引 `run_rebase_abort` 实现而非帮助文案 | `SA-08d`、`SA-08f`（R32）撤销 → `SA-08a`（原子回滚）+ `SA-08g` + `SA-08h` | ADR-SA-03、验证执行协议第 9 条、文档与网站验收命令第 4 步、实施顺序、`DEP-SA-02`、发布分组与并发窗口、Phase 1、默认覆盖、`SA-05`、`SA-08a`、`SA-08b`、`SA-08g`、`SA-08h`、`SA-08`、`REL-SA-01`、`REL-SA-02`、审计表、测试矩阵、追溯表、故障恢复矩阵、风险表、GAP-07、评审维度表 |
| 2026-11-18 12:23:47 UTC | Claude R34 PASS + Codex R34 FAIL | 按 Codex R34 意见整改：①`ADR-SA-03` 以签名能力预检取代原子回滚：签名决策为「签名」时在写入任何 rebase 状态、autostash、索引、工作区或引用之前对固定探测载荷试签，失败即非 0 退出且仓库保持原状（新卡 `SA-08i`：非 0 退出、stderr、HEAD 挂在 `topic`、`topic` tip 等于 `T_SHA`、已暂存与未暂存修改原地保留、未产生 stash、无 `kind='rebase'` 序列行、无 `rebase-aux.json`），从而不存在多步回滚、autostash 恢复或回滚失败的合同；出口级签名失败（返回错误、不写对象）以库内单元判据留在 `SA-08a`；预检通过后中途签名失败的端到端恢复登记 `DEFER-SA-07`；②OpenPGP 验签改为在以 `<upstream>` 为起点的临时分支上 `merge --ff-only --verify-signatures topic`，签名用例改用无签名旧历史，证明被验签的是新的重放结果（`SA-08a`、`SA-08b` AC8、`SA-08h`、`SA-08g`）；③「验证执行协议」新增 9e 线性链与子卡顺序（两张发布卡各增 2 项判据），9d 重建后所有子卡重跑全部门并以新基线与新树重新评审、重新校验签名与 DCO；④新增第 10 条合入树发布前门：`gh release create` 之前比较 merge 树与 `TESTED_SHA` 树，不一致即在 merge 树上重跑 C 组门并重新评审，D6 只记录远端证据；⑤「已是最新」短路锚点写明完整条件 | 新增 `SA-08i`；原子回滚撤销 | ADR-SA-03、验证执行协议第 9、10 条、D 组登记表 D6、文档与网站验收命令第 4 步、实施顺序、`DEP-SA-02`、发布分组与并发窗口、Phase 1、默认覆盖、`SA-08a`、`SA-08b`、`SA-08g`、`SA-08h`、`SA-08i`、`SA-08`、`REL-SA-01`、`REL-SA-02`、审计表、测试矩阵、追溯表、故障恢复矩阵、风险表、延期表、完成判据、GAP-07、评审维度表 |
| 2026-11-18 12:31:06 UTC | Claude R35 PASS + Codex R35 FAIL | 按 Codex R35 意见整改：①（P0）撤销 `DEFER-SA-07`：现有内部错误路径在写入索引前停止、`--continue` 会以旧索引生成提交并丢失补丁（`src/command/rebase.rs:4121-4127,4421-4488,5746-5777`），改为中途签名失败按冲突停止同构处理——新卡 `SA-08j`（先写该步合并树到索引与工作区、记录 `stopped_sha`、非 0 退出；`--continue` 以索引树重签且补丁保留；以测试专用 failpoint `LIBRA_TEST_REBASE_SIGN_FAIL` 确定性制造）与 `SA-08k`（该停止态 `--abort` 回到原分支、索引与工作区）；②`SA-08i` 改为逐项比较 `ls-files --stage`、工作区文件字节（`hash-object`）、`show-ref` 与辅助状态，删去不能覆盖 held stash 的 `stash list` 判据；③预检限定于规划阶段判定会生成新提交的路径，新卡 `SA-08l` 验收快进与「已是最新」在签名不可用时仍成功；④第 10 条合入树不一致时在 `MERGE_SHA` 检出上重跑 C 组全部适用步骤（测试、release build、安装）并重新评审，结果绑定同一 `MERGE_SHA`；⑤`SA-09a` 增加真实 `am` 暂停态 GC 用例（GC 后 `--continue` 提交序列等于 Git 对照，未入根对照对象被清除）；⑥`SA-08a` 库内用例写明临时仓库、cwd 与 `#[serial(cwd, env)]` 隔离并把 `tests/SERIAL_REGISTRY.tsv`、`.config/nextest.toml` 纳入写集；`REL-SA-01` 子卡增至八张 | 新增 `SA-08j`、`SA-08k`、`SA-08l`；撤销 `DEFER-SA-07` | ADR-SA-03、验证执行协议第 10 条、文档与网站验收命令第 4 步、实施顺序、`DEP-SA-02`、发布分组与并发窗口、Phase 1、默认覆盖、`SA-08a`、`SA-08b`、`SA-08g`、`SA-08h`、`SA-08i`、`SA-08j`、`SA-08k`、`SA-08l`、`SA-08`、`SA-09a`、`REL-SA-01`、`REL-SA-02`、审计表、测试矩阵、追溯表、故障恢复矩阵、风险表、延期表、完成判据、GAP-07、评审维度表 |
| 2026-11-19 03:33:23 UTC | Claude R36 PASS + Codex R36 FAIL；用户 2026-11-19 03:12:32 UTC 批准 G-03 门族型豁免 `EX-SA-01` | 按 Codex R36 四项 P1、Claude R36 P2 与 R37 前置清扫整改：①（豁免）按 `plan-template.md` 白名单「G-03 条目上限（门族型验收）」登记 `EX-SA-01`（任务 `SA-08j`，Approver 用户（本计划请求者），Review round R37，有效期本计划内，只豁免 G-03 计数）；「规则 waiver」由「无」改为该表，R22 取消的 `EX-SA-02-RANGE` 保留为历史；②（P1-1 顺序）预检 `SA-08i`、`SA-08l` 移到中途失败恢复卡之前，家族顺序改为 `SA-08a`→`SA-08b`→`SA-08g`→`SA-08h`→`SA-08i`→`SA-08l`→`SA-08j`，同步依赖边、各卡 `Dependencies`、发布分组、逆序回滚、发布窗口顺序、Phase 1 与审计表；③（P1-2、P1-3）撤回 `SA-08k`（编号不复用）并入 `SA-08j`，`SA-08j` 改为门族卡（4 门 44 个判据，`44/8@EX-SA-01`，「判据规范（非计数正文）」逐门列命令）：多步 `pick` 与 `squash`/`fixup` 折叠两类重放各自验收停止态的全量索引树（含上游 `u.txt`）、工作区与引用，失败重试，`--continue` 后的父链、树、作者、消息与每个提交的有效签名，以及 `--abort` 后引用、索引与工作区的全量快照复原；failpoint 改为按原提交 OID 选步，注入错误文本取代变量名判定（关闭 Claude R36 P2）；其余 action 的逐 action 门与签名失败停止态 `--skip` 登记 `DEFER-SA-08`；④（P1-4）「验证执行协议」第 10 条：merge 树与受测树不同时，在 `MERGE_SHA` 上重跑本卡卡特有 Verification（发布点为聚合守卫）、共享回归组、文档 compat 门与 C 组全部适用步骤并重新评审，全部绑定同一 `MERGE_SHA`，D6 同步；⑤（R37 前置清扫 P1）`edit_interactive_commit_message`（`reword` 等消息编辑）与 `amend_head_from_index_if_staged`（`edit` 停止后吸收已暂存改动）另以 `Commit::new` 生成不签名提交，绕过单一出口：`ADR-SA-03` 覆盖 rebase 生成提交的全部路径，`SA-08h` 扩为「重放后改写先于签名」并新增两条验收（7/8、4/8）；⑥（R37 前置清扫 P1，六类清单第 1 类「回滚恢复须全量快照」）`SA-05` 的 unborn `cherry-pick --abort` 与 `SA-09a` 的 unborn `am --abort` 原只断言 HEAD 回到 unborn，补「索引快照」「工作区路径集合与字节快照」两条判据（`SA-05` 删去已由 `SA-05b` 在同一投影代码上验收的 stdout 判据，`SA-09a` 保留更具区分力的第二封中止场景），并把真实 `am` 暂停态 GC 用例从 `SA-09a` 移到与 `SA-05b` 同轴的 `SA-09c`（`ORIG_HEAD` 投影与 GC 安全，6/8）；`SA-05` 写明 Git 对 unborn `cherry-pick --abort` 报 `cannot abort from a branch yet to be born` 而 Libra 有意回滚的差异并纳入文档；`SA-07a` 的工作区判据改为全量快照；`DEFER-SA-05` 的承接卡更正为 `SA-09c`；⑦跨计划：`DEP-SA-02` 写明 `REL-CS-01` 的六张子卡，`DEP-SA-05` 改为等待 `REL-CS-06`（commit-signing 单链末端）与 `MP-45`（merge-pull 最后一个 `commit.rs` 写者），`DEP-SA-03` 扩为与 `DEP-GII-03`、`DEP-MP-10` 同口径的仓库与网站串行窗口并加入两张发布卡的依赖；计划卡数 31 → 30 | 撤回 `SA-08k`（并入 `SA-08j`，编号不复用）；新增 `EX-SA-01`、`DEFER-SA-08` | 状态行、文档职责、使用规则、事实基线「rebase」行、GAP-07、与其它计划的关系、评审维度表、ADR-SA-03、验证执行协议第 10 条、文档与网站验收命令第 4 步、实施顺序、`DEP-SA-02`、`DEP-SA-03`、`DEP-SA-05`、发布分组与并发窗口、D6、Phase 1、默认覆盖、规则 waiver、审计表、`SA-05`、`SA-07a`、`SA-09a`、`SA-09c`、`SA-08a`、`SA-08b`、`SA-08g`、`SA-08h`、`SA-08i`、`SA-08l`、`SA-08j`、`SA-08k`（撤回）、`REL-SA-01`、`REL-SA-02`、`SA-08`、测试矩阵、追溯表、故障恢复矩阵、风险表、延期表、完成判据 |
| 2026-11-19 03:57:23 UTC | Claude R37 FAIL + Codex R37 FAIL（6 分区并行） | 按 R37 双审整改：①（设计收拢）撤回签名能力预检（`SA-08i`、`SA-08l` 撤回、编号不复用）：Git 没有该预检，签名失败时停止并保留可恢复状态（Git `sequencer.c` `pick_one_commit`），而要证明「任何写入之前」须覆盖交互入口（`rebase.rs:938-940` 先于 `prepare_rebase_aux` 分流）、autostash 的对象写入与对象库快照（关闭 Codex D 分区三项 P1 与 Claude 指出的 `sequence_state`/`rebase_state` 表误用）；`ADR-SA-03` 改为任一步（含第一步、含签名不可用）签名失败都按冲突同构停止；②`SA-08b` 只测签名关闭时的结果——拆出 `SA-08m`（`amend!`、`fixup -C`、`fixup -c`），两卡对每个 action 分别验收签名关闭（无旧 `gpgsig`）与签名开启（新签名有效）；③门族卡 `SA-08j`（`EX-SA-01`）：拆开合并判据（工作区路径/内容、提交数/父链各自计数），失败重试后逐项比较 HEAD、引用、索引、工作区与 `rebase_state` 整行，两个中止门先断言注入失败与停止步索引，`G2` 改为第一步失败，折叠消息改为明确期望字节，分子 `64/8@EX-SA-01`，waiver 行同步；④家族外部依赖前置：`DEP-SA-02` 改为家族入口 `SA-08a` 的依赖（家族持有 `DEP-SA-03` 窗口时不得等待需要同一窗口的 `REL-CS-01`），`SA-09b` 加 `DEP-SA-04`；`DEP-SA-03` 成为全部卡的依赖并登记 `CS-18b`；⑤第 10 条合入树重跑扩为整套文档门（1a–1e 与卡内登记的索引同步等检查）；⑥`SA-01` 停止态判据改为 `current_oid`；`SA-02` 写明展开后冲突/失败标签不得按原始参数下标取值，并以停止态 `current_oid` 判定；`SA-03` 增加「肯定—否定—否定—肯定」最后生效判据（覆盖 `t3504#8` 第 2 步）；`SA-05` 的 unborn `--abort` 验收成功退出、序列状态释放、HEAD 仍 unborn 与索引/工作区快照；`SA-07a` 加非 0 退出并豁免 `--no-commit`；`SA-07b` 加冲突后 `--continue` 与多提交序列的 Git 消息对照（`--reference` 随停止态保存）；`SA-07` 加成功与 `--continue` 结果树判据，删去由 clippy `dead_code` 强制的零命中守卫与既有用例判据；`SA-08` 的 `TC-2289` 改为 merge 后端形态（Libra 无 apply 后端，`DEFER-SA-03` 承接）；`SA-09c` 的 GC 门限定无待处理邮件，登记 `DEFER-SA-09`（GC 把 `am` 的邮件标签当 OID）；⑦`SA-08h` 两条 AC 的自指标签改为「reword」/「edit」fixture，`ADR-SA-03` Context 补两条改写路径；计划卡数 30 → 29 | 新增 `SA-08m`；撤回 `SA-08i`、`SA-08l`（编号不复用）；新增 `DEFER-SA-09` | 状态行、文档职责、使用规则、事实基线「rebase」行、GAP-07、与其它计划的关系、评审维度表、ADR-SA-03、验证执行协议第 10 条、文档与网站验收命令第 4 步、实施顺序、`DEP-SA-02`、`DEP-SA-03`、发布分组与并发窗口、Phase 1、默认覆盖、规则 waiver、审计表、`SA-01`、`SA-02`、`SA-03`、`SA-05`、`SA-05a`、`SA-05b`、`SA-07a`、`SA-07b`、`SA-07`、`SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j`、`REL-SA-01`、`SA-08`、`SA-09b`、`SA-09c`、测试矩阵、追溯表、故障恢复矩阵、风险表、延期表、完成判据 |
| 2026-11-19 04:07:36 UTC | Claude R38 FAIL + Codex R38 FAIL（6 分区并行） | 按 R38 双审整改：①`SA-08a` 补登家族入口依赖 `DEP-SA-02`，`DEP-SA-04` 行改为家族入口 `SA-09b` 阻断；②`DEP-SA-03`：网站「无漂移」路径以第 3c 步全部核对通过为窗口释放事件；网站生成产物（`docs-manifest.ts`、`docs-search.ts`、`dist/`）列入全部做网站构建的卡与发布点的写集并注明不暂存、不提交；故障恢复矩阵把 `am` 暂停态的 GC 保证限定为无待处理邮件（其余按 `DEFER-SA-09`）；③「验证执行协议」第 3 条的「提交序列等于 Git 对照」扩为（树、author 行、消息）三元组序列，`SA-09` AC2 改用该比较，关闭 `am` 邮件元数据丢失测不出的问题；④`SA-02` 分叉图改为以合并提交 `B` 结尾（与合并提交被拒一致，比较到达 `B` 前的主题序列），范围中途冲突改为解决后 `--continue` exit 0（panic 时状态行已被推进但缺冲突标记，`--continue` 会失败）；⑤`SA-07c` 更正 Git 语义：`revert`/`cherry-pick` 共用 `parse_args`，只把第一个修订参数 `-` 改写为 `@-1`，新增「`-` 不在第一位时以 `LBR-CLI-003` 拒绝」判据，`revert -` 登记 `DEFER-SA-10`；⑥`SA-08h` 写明 `reword`、`fixup -c` 与折叠组末尾 `squash` 经同一调用点（`rebase.rs:4045`）进入 `edit_interactive_commit_message`，签名在函数内完成；⑦门族卡 `SA-08j`：更正 Git 依据（`do_pick_commit` 经 `res = !!res` 归入 `PICK_RESULT_CONFLICTS`，`pick_one_commit` 以 `error_with_patch` 停止），新增 G5（`--vault false` 真实签名不可用：第一步停止、修正配置后 `--continue`）与 G6（移除 `LIBRA_TEST` 时 failpoint 不生效），分子 `73/8@EX-SA-01`、VER 6/8，waiver 行同步；⑧`SA-08` 以 2 个提交的已是最新分支验收全部重放（每个 OID 改变、树序列保留、单父线性链、`--force-rebase` 同义），签名 TC 按「HEAD^ 与 HEAD 各自」合并为每种格式一条；⑨`SA-09a` 拆出 `SA-09d`：`SA-09a` 验收首封/第二封冲突后 `--continue`/`--skip` 的提交序列与状态释放，`SA-09d` 验收第二封冲突后 `--abort` 的成功退出、状态释放、回到 unborn 与索引/工作区快照；计划卡数 29 → 30 | 新增 `SA-09d`；新增 `DEFER-SA-10` | 状态行、文档职责、使用规则、GAP-08、与其它计划的关系、评审维度表、验证执行协议第 3 条、`DEP-SA-03`、`DEP-SA-04`、文档与网站验收命令第 4 步、实施顺序、发布分组与并发窗口、Phase 1、默认覆盖、规则 waiver、审计表、全部做网站构建的卡与发布点的写集、`SA-02`、`SA-07c`、`SA-08a`、`SA-08h`、`SA-08j`、`SA-08`、`SA-09`、`SA-09a`、`SA-09d`、`SA-09c`、`REL-SA-02`、测试矩阵、追溯表、故障恢复矩阵、风险表、延期表、完成判据 |
| 2026-11-19 04:13:52 UTC | Claude R39 PASS + Codex R39 FAIL（6 分区并行） | 按 Codex R39 整改：①`ADR-SA-03` 的 Git 依据同步更正为 `do_pick_commit`（`res = !!res` → `PICK_RESULT_CONFLICTS`）与 `pick_one_commit` 的 `error_with_patch`，签名决策改为「`commit.gpgSign` 优先、其次显式 `vault.signing=true`、都未设不签名」（与 `DEP-SA-01` 一致），`SA-08a` 增加两者都未设时不签名的判据；②`SA-02` 性能门编号统一为 VER7（卡内、测试矩阵、性能表）；追溯表发布点聚合用例数改由脚本按发布卡 VER8 计数生成（`REL-SA-01` 20 个）；③`SA-01` 改以「冗余 `R` + 有效 `V`」多提交 fixture 验收 `todo` 保留与 `--skip` 继续拣选、状态释放（`--quit` 沿用共享路径不另设门）；`SA-01b`、`SA-01c`、`SA-03` 的拒绝判据改为稳定错误码 `LBR-CLI-002`；④`SA-07d` 的默认消息改由 `revert.rs` 的 `stopped_revert_message()` 单一事实源提供，`SA-07b` 只改该函数并新增冲突后 `commit -a` 的 Git 消息对照；`SA-07e` 中止改以索引与工作区快照判定、无冲突 revert 增加成功判据；⑤`SA-08j`：failpoint 改为与既有 `test_failpoint_enabled` 同一「设置了 `LIBRA_TEST` 即生效」判定（G6 验收未设置时不生效），G6 增加树序列判据防止丢步，分子 `74/8@EX-SA-01`；⑥`SA-09d` 覆盖首封（HEAD 仍 unborn，现有 `abort_am` 在 `am.rs:389-392` 拒绝）与第二封两种暂停态的 `--abort` | 无新卡 | 状态行、ADR-SA-03、默认覆盖的安全行、规则 waiver、`SA-01`、`SA-01b`、`SA-01c`、`SA-02`、`SA-03`、`SA-07d`、`SA-07b`、`SA-07e`、`SA-08a`、`SA-08j`、`SA-08`、`SA-09d`、审计表、测试矩阵、追溯表、性能表、完成判据 |
| 2026-11-19 04:24:55 UTC | Claude R40 PASS + Codex R40 FAIL（6 分区并行） | 按 Codex R40 整改（统一口径：每个恢复命令验收成功退出、状态释放、HEAD 与索引/工作区快照；全称判据配合非空计数；保留类快照的 fixture 含无关用户数据）：①`DEP-SA-04` 对 `CSR-31`/`CSR-32` 的 `tests/INDEX.md` 改为「如有修改则在 `main`，无需同步以对侧记录满足」；②`SA-01` 单提交空停止增加状态行 `current_oid` 与 `--abort` 成功退出，多提交 fixture 分别判定 `current_oid=R` 与 `todo=[V]`，`--skip`/`--quit` 沿用共享路径不另设门，`TC-0522` 移入 `SA-01c`；③「验证执行协议」第 3 条元组加父提交个数，`SA-05` 合并无父与树判据，abort fixture 加入字节独特的未跟踪文件并分别判定路径集合与字节；④`SA-07a` 以新增/修改/删除三种已暂存改动分别验收拒绝；⑤`SA-07e` 拆出 `SA-07f`（`--abort` 成功退出、HEAD、索引、工作区路径与字节、状态清理），`SA-07e` 的 `--continue` 与无冲突完成增加成功退出；⑥`SA-07d` 的 `stopped_revert_message()` 只在未收口的停止上返回种子，并验收收口后不复用旧消息；⑦`SA-08a` 增加 `vault.signing=true` 回退与 `commit.gpgSign=false` 优先两条判据，签名决策锚点补 `history_config.rs:71-82`；⑧`SA-08j` 每个停止态增加 `stopped_sha` 与 HEAD 判据，「不移动引用」限定为分支与标签，Git 依据区分 `error_with_patch` 与折叠步的 `error_failed_squash`（86 个判据）；⑨`SA-08` 以 `--help` 同行判定 `--force-rebase` 是 `-f` 的长形式；⑩`SA-09a` 改为两条三封补丁的「冲突—恢复」链，逐个验收恢复命令成功退出、最终提交序列与状态释放；`SA-09d` 只验收第二封暂停的 `--abort`，新增 `SA-09e` 验收首封暂停（当前 HEAD 缺失，`am.rs:389-392` 现拒绝）的 `--abort`；计划卡数 30 → 32 | 新增 `SA-07f`、`SA-09e` | 状态行、文档职责、使用规则、GAP-06、GAP-08、与其它计划的关系、评审维度表、ADR-SA-03、验证执行协议第 3 条、`DEP-SA-03`、`DEP-SA-04`、文档与网站验收命令第 4 步、实施顺序、发布分组与并发窗口、Phase 1、默认覆盖、规则 waiver、审计表、`SA-01`、`SA-01c`、`SA-05`、`SA-07a`、`SA-07e`、`SA-07f`、`SA-07`、`SA-07d`、`SA-07b`、`SA-08a`、`SA-08b`、`SA-08m`、`SA-08j`、`SA-08`、`SA-09a`、`SA-09d`、`SA-09e`、`SA-09c`、`REL-SA-02`、测试矩阵、追溯表、风险表、完成判据 |
| 2026-11-19 04:32:40 UTC | Claude R41 PASS + Codex R41 FAIL（6 分区并行，D 区 PASS） | 按 Codex R41 整改：①网站第 3 步多页面卡（`SA-07d`）对每个页面分别做无漂移判断、标记句零命中与 D5；故障恢复矩阵签名失败行改为「分支与标签引用未动、HEAD 停在失败步之前的重放结果」；`ADR-SA-01` 的中止清理归 `SA-07f`；②`SA-01` 改为默认与显式（`TC-0522`，独立无活动序列的 fixture）两种 stop 模式同卡验收停止态，`--abort` 验收成功退出与状态释放（复原机制沿用既有路径，由 `cherry_pick_abort_restores_head` 守护）；多提交空停止的 `--skip`/`--continue`/`--quit` 登记 `DEFER-SA-11`，`SA-01c` 只验互斥；③`SA-02` 拆出 `SA-02b`（`t3508#8/#9` 的 `-n`/`--ff` 范围组合），范围冲突的 `--continue` 增加最终主题序列、结果树与状态释放判据，性能门改为 VER5；④`SA-05a` 增加 SHA-1/SHA-256 的 `Some(OID)` 写出判据；`SA-05` fixture 写明 unborn 上不能有已暂存文件（Git `do_pick_commit` 的 `error_dirty_index`），以未跟踪 `notes.txt` 验收保留（驳回 Codex B-6 的已暂存数据要求）；⑤新增既有行为补测卡 `SA-07h`（`revert --no-commit` 从非 HEAD 索引开始），`SA-07a` 增加工作区路径集合判据并以 `notes.txt` 验收保留；`SA-07e` 的 `--continue` 增加结果树判据；`SA-07f` fixture 加入 `notes.txt`；`SA-07` 收窄为 detached HEAD 成功路径（成功退出、父、树、分支引用、仍为 detached），冲突后恢复登记 `DEFER-SA-12`；⑥`SA-08j` fixture 加入无关未跟踪文件 `notes.txt`，工作区判据改为一次集合/映射比较，四个成功恢复增加最终索引与工作区判据，G5、G6 增加 HEAD 回到 `topic` 与状态释放判据（102 个判据）；⑦`SA-09a` 两条链中继续进入下一冲突的恢复命令改为验收非 0 退出（Git `am_run` → `die_user_resolve`），完成序列的恢复命令验收 exit 0；计划卡数 32 → 34 | 新增 `SA-02b`、`SA-07h`；新增 `DEFER-SA-11`、`DEFER-SA-12` | 状态行、文档职责、GAP-02、GAP-06、与其它计划的关系、评审维度表、ADR-SA-01、文档与网站验收命令第 3 步、实施顺序、`DEP-SA-03`、发布分组与并发窗口、Phase 0、Phase 1、规则 waiver、审计表、`SA-01`、`SA-01c`、`SA-02`、`SA-02b`、`SA-03`、`SA-05a`、`SA-05`、`SA-07h`、`SA-07a`、`SA-07e`、`SA-07f`、`SA-07`、`SA-08j`、`SA-09a`、测试矩阵、追溯表、故障恢复矩阵、性能表、延期表、完成判据 |
| 2026-11-19 05:45:41 UTC | Claude R42 FAIL + Codex R42 FAIL（6 分区并行，17 项 P1）；用户 2026-11-19 05:17:51 UTC 决定：cherry-pick / revert / am 等「闭环上游测试」卡收窄到 issue 验收项的核心断言，不扩大 waiver | 按用户决定收窄并整改：①各卡只交付并验收所跟踪 issue 验收项明确要求的行为、取能区分正确实现与最可能错误实现的核心断言；各 issue 的「上游脚本 / TC 全量通过」登记 `DEFER-SA-13`，新增「Issue 闭环登记」（9 个 issue 均部分闭环、保持 open），收窄出的场景登记 `DEFER-SA-14`..`DEFER-SA-20`；②撤回 `SA-02b`（`DEFER-SA-14`）、`SA-05a`、`SA-05b`（unborn 非快进 pick 与 `cherry_pick` 行全零编码登记 `DEFER-SA-17`，`DEP-SA-08` 随之撤回）、`SA-07h`（前提不成立：`revert --no-commit` 以 HEAD 树重建并丢弃已暂存条目，`src/command/revert.rs:1366-1387`，登记为「无承接计划」既有缺陷）、`SA-07e`、`SA-07f`（既有行为由既有用例守护，完整快照登记 `DEFER-SA-18`）、`SA-09c`（`DEFER-SA-20`）、`SA-09d`、`SA-09e`（并入 `SA-09a`），编号均不复用；③`SA-01` 只验默认 stop 模式（显式模式 `DEFER-SA-15`）；`SA-03` 收窄为 #726（范围上的 rerere autoupdate、`--continue` 互斥及其 `cannot be used with` 文本、`--abort`），拆出 `SA-03b`（#727 的重复/混合标志最后一个生效，含末位否定时工作区已由 rerere 改写的判据）；`SA-05` 收窄为 #730 的 unborn `--ff` 快进 root commit（分支引用、符号 HEAD、索引与工作区检出）；`SA-07a` 收窄为 `t3501#7` 形态的全量快照验收并补非 0 退出与 HEAD 不变，另以「修改」形态验收错误码；`SA-08` 收窄为 #780 的 `t7510#2`（`-f` 重放与 OpenPGP 重签）并补长形式 `--force-rebase` 在「已是最新」路径上的行为判据（组合登记 `DEFER-SA-19`）；④原 `SA-05a` 的共享编解码函数改由 `REL-SA-02` 家族子卡 `SA-09f` 交付（含非法输入与 kind 不符的解码失败判据），`SA-09b` 补两个字段的 `Some(OID)` 写出判据，`SA-09` 补 exit 0、符号 HEAD 与状态释放，`SA-09a` 改为 unborn 起点暂停后的 `--abort` 出口（第二封暂停与首封暂停，含符号 HEAD 保留），`--continue`/`--skip` 登记 `DEFER-SA-20`；⑤`SA-08a` 的库内用例改登记 `tests/SRC_SERIAL_REGISTRY.tsv`（`tests/SERIAL_CLASSIFY.sh:32-33`：`src/` 用例不进入 `tests/SERIAL_REGISTRY.tsv`、不改 `.config/nextest.toml`），共享回归组加 `compat_serial_registry`；`SA-08j` 为 G2、G4、G5 的停止态补工作区路径与内容判据（102 → 108 个判据，`EX-SA-01` 只更新计数、范围不变）；⑥「文档与网站验收命令」第 3 步写明多页面卡按页分流（无漂移页只做替代核对、有改动页做零命中与逐页部署核对、一次提交一次推送）；同步 ADR-SA-01/ADR-SA-02、依赖边、发布分组与窗口、Phase、里程碑、审计表、追溯表、测试矩阵、故障恢复矩阵、风险、性能、DEFER 与完成判据；活动卡 34 → 27 | 撤回 `SA-02b`、`SA-05a`、`SA-05b`、`SA-07h`、`SA-07e`、`SA-07f`、`SA-09c`、`SA-09d`、`SA-09e`；新增 `SA-03b`、`SA-09f`；撤回 `DEP-SA-08`；新增 `DEFER-SA-13`..`DEFER-SA-20` | 状态行、文档职责、适用范围、成功定义、使用规则、GAP-01..GAP-08、与其它计划的关系、评审维度表、ADR-SA-01、ADR-SA-02、验证执行协议第 9 条、文档与网站验收命令第 1c/3/4 步、实施顺序、依赖登记表、发布分组与并发窗口、Phase 0、Phase 1、字段默认覆盖、规则 waiver、审计表、全部被收窄或撤回的卡、`SA-03b`、`SA-09f`、`SA-08a`、`SA-08j`、`REL-SA-02`、测试矩阵、追溯表、里程碑、故障恢复矩阵、风险、性能表、兼容与文档收口、延后表、Issue 闭环登记、完成判据 |
| 2026-11-19 05:56:20 UTC | Claude R43 FAIL + Codex R43 FAIL（6 分区并行，17 项 P1） | 按双审 R43 整改（收窄口径不变）：①「文档与网站验收命令」与「验证执行协议」第 1 条写明库内用例路径按实际模块（`command::…::tests::…` 或 `internal::…::tests::…`）；②`#729` 的闭环登记把验收项 1、3 映射到 `SA-04` AC2 与 `SA-05`；③`SA-01b` 为四个控制选项分别增加点名选项对的 `cannot be used with` 诊断判据（#725 验收项 3）；`SA-03` 的两条诊断判据改为点名 `'--continue'` 与对应 rerere 标志，「已暂存」改为「已暂存为预录解决结果」（blob 等于录制时的 `R`），`SA-03b` 同步；④`SA-02` 增加范围中途未跟踪覆盖的正常停止判据（覆盖 `args.commits[i]` 越界的另一出口），`t3430#28` 与无效端点不再单列（前者登记 `DEFER-SA-14`）；⑤`SA-05` 要求复用 `ensure_no_untracked_overwrite` 并增加未跟踪覆盖拒绝、字节保留与仍为 unborn 的判据；⑥`SA-06` 增加 `--empty=drop` exit 0 与 HEAD 不变的判据，`--empty=keep`（`t3505#17`）登记 `DEFER-SA-15`；⑦`SA-07` 定义 orphan/detach 序列并验收无 `revert-state.json` 与 `checkout -b side-branch` 成功，`SA-07c` 单列 `checkout -b side-branch` 成功判据；⑧`SA-08g` 签名决策锚点改为 `history_config.rs:71-82` 与 `commit.rs:1144-1148,3088-3100`，SSH 入口注明由 `DEP-SA-02` 交付；⑨`SA-08j` 更正 R43 插入的 G4/G5 工作区判据的步骤引用，并为 G2/G4 停止态补 `show-ref` 判据（110 个判据，`EX-SA-01` 范围不变）；⑩`SA-08` 长形式补树序列判据，`t7510#2` 补「`HEAD^`、`HEAD` 的树序列等于 `sixth`、`seventh`」判据；`SA-09` 改以退出码、无父、树、符号 HEAD、状态释放与三封序列的树序列和父链判定（作者与消息由 `compat_mail_am_basic` 守护，不再以合并元组计 1 项）；`SA-09a` 增加第二封暂停的状态释放与暂停后后代提交的中止保护判据，因 8 条上限保留 `SA-09e`（首封暂停的 `--abort`，R43 曾拟撤回，编号未变）；⑪降级指引改为 unborn 起点暂停先 `libra am --abort`（`--continue` 属 `DEFER-SA-20`）；与其它计划关系表的 `DEP-SA-09` 改指家族入口 `SA-09f`；活动卡 27 → 28 | 恢复 `SA-09e`；其余同 R43 | 状态行、文档职责、成功定义、使用规则、事实基线「测试命名」、GAP-02/05/08、与其它计划的关系、评审维度表、ADR-SA-02、验证执行协议第 1 条、文档与网站验收命令第 4 步、实施顺序、发布分组、Phase 1、字段默认覆盖、规则 waiver、审计表、`SA-01b`、`SA-02`、`SA-03`、`SA-03b`、`SA-05`、`SA-06`、`SA-07`、`SA-07c`、`SA-08g`、`SA-08j`、`SA-08`、`SA-09`、`SA-09a`、`SA-09e`、`REL-SA-02`、测试矩阵、追溯表、里程碑、故障恢复矩阵、风险、延后表、Issue 闭环登记、完成判据 |
| 2026-11-19 06:04:41 UTC | Claude R44 FAIL + Codex R44 FAIL（6 分区并行，E 区 PASS，12 项 P1） | 按双审 R44 整改（收窄口径不变）：①按 GC-04 统一「拒绝」判据：`SA-01b`、`SA-01c` 对四个控制选项分别验收非 0 退出与点名选项对的诊断，`SA-03` 两条 `--continue` 拒绝改为非 0 退出加诊断，`SA-02`、`SA-05`、`SA-09a` 的拒绝/停止判据补非 0 退出；新增「验证执行协议」第 11 条（结构化错误报告即 `--json`/`--machine` 同源包络，clap 冲突的稳定错误码由统一映射承担）；`SA-07a` 的错误码判据写明读取结构化报告，并同步 `docs/error-codes.md`（第 1e 步、`compat_error_codes_doc_sync`）；②`SA-02` 只验收 #726 的线性范围（非线性遍历序与 Git 对照登记 `DEFER-SA-14`），测试矩阵与性能表的 `VER5` 改为 `VER3`；③`SA-07` 在 detached 序列后以 `libra switch --orphan unborn` 验收 orphan 过渡与无 revert 停止态，`checkout -b side-branch` 只由 `SA-07c` 验收；④重放后改写路径（交互式消息编辑的二次提交、`edit` 停止后的修订提交）的二次签名失败不再宣称共用 `SA-08j` 的停止路径，恢复合同与专项门登记 `DEFER-SA-08`（ADR-SA-03、`SA-08h`、`SA-08j`、故障恢复矩阵与风险表同步）；⑤`SA-08` 以 `t7510#2` 验收短形式（exit 0、树序列对应 `sixth`/`seventh`、两次验签）、以「已是最新」分支验收长形式（exit 0、OID 全变、树序列、线性链）；⑥`SA-09e` 增加首封暂停后用户 root 提交的中止保护（非 0 退出、分支引用保留）；`SA-09f` 只把恰为 `zero_str(kind)` 的串视为 `None` 并验收错误长度的全零串被拒（`Some` 写出由 `SA-09b` 验收）；⑦#907 的闭环登记改为验收项 1、2 与 3 的树序列和父链，unborn 根提交与序列的作者/消息对照登记 `DEFER-SA-20`；#726、#728 的登记同步；状态行轮次更正（R44 P2） | 无 | 状态行、GAP-02、评审维度表、ADR-SA-03、验证执行协议第 11 条、`SA-01b`、`SA-01c`、`SA-02`、`SA-03`、`SA-05`、`SA-07`、`SA-07a`、`SA-08h`、`SA-08j`、`SA-08`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09f`、`REL-SA-02`、测试矩阵、追溯表、故障恢复矩阵、风险、性能表、延后表、Issue 闭环登记 |
| 2026-11-19 06:10:49 UTC | Claude R45 FAIL + Codex R45 FAIL（6 分区并行，D 区 PASS，10 项 P1） | 按双审 R45 整改：①「验证执行协议」第 11 条改为 GC-04 四项分别验收：退出码、人读诊断与结构化报告的 `error_code` 由 AC 判定，显式 `--json`/`--machine` 由新增的卡内「输出契约门」判定（列出失败命令，在独立副本上分别加 `--json`、`--machine` 运行，核对退出状态、`ok=false` 包络、`error_code` 与 `exit_code`；与文档门一样不计入 G-03），`SA-01`、`SA-01b`、`SA-01c`、`SA-02`、`SA-03`、`SA-05`、`SA-07a`、`SA-07`、`SA-07c`、`SA-07d`、`SA-08j`、`SA-09a`、`SA-09e` 增列该门；②评审维度表「安全性」「可靠性与容错」、字段默认覆盖的 Security 行与 `REL-SA-01` 理由把签名失败的可恢复范围限定为经 `create_replayed_commit` 构造提交的重放步，改写路径登记 `DEFER-SA-08`；③关系表改正对侧 `DEP-MP-08` 的语义（等待 `MP-45` 是对侧关系表的建议，`DEP-MP-08` 本身登记 `MP-10`、`MP-30`，并注明其指向已撤回的 `SA-05b`）；④`SA-02` 的主题序列改为逐个 OID 以 `libra show --no-patch --pretty=format:%s` 取主题；⑤`SA-07a`「修改」fixture 改以非 0 退出判定；`SA-07c` 为无上一检出记录增加 `LBR-CLI-003` 与 `no previous checkout target is available` 判据，`<X> -` 改以非 0 退出判定；`SA-07d` 两条空消息拒绝改为非 0 退出加诊断；`SA-07` 写明随后的 `checkout --detach` 过渡依赖 unborn 上的非快进 pick 而登记 `DEFER-SA-17`；⑥`SA-09` 的补丁改由 `libra format-patch --root --stdout c1`/`c3` 生成（`c1..c3` 会排除 `c1`）；`SA-09a`、`SA-09e` 写明 Libra 拒绝中止是与 Git（`safe_to_abort` 为假时告警、`am_destroy` 后成功返回）的有意差异并写入文档；状态行轮次更正（R45 P2） | 无 | 状态行、与其它计划的关系、评审维度表、验证执行协议第 11 条、`DEP-SA-05`、字段默认覆盖、发布分组 `REL-SA-01` 理由、`SA-01`、`SA-01b`、`SA-01c`、`SA-02`、`SA-03`、`SA-05`、`SA-07a`、`SA-07`、`SA-07c`、`SA-07d`、`SA-08j`、`REL-SA-01`、`SA-09`、`SA-09a`、`SA-09e`、延后表、Issue 闭环登记 |
| 2026-11-19 06:14:35 UTC | Claude R46 FAIL + Codex R46 FAIL（6 分区并行，D 区 PASS，14 项 P1） | 只登记 R46 结论，正文未改：Codex R46 认定「验证执行协议」第 11 条的「输出契约门」不属模板 G-03 允许排除的 ER-04/ER-06 门，须计入 AC，且各失败路径须分别断言字面稳定码、人读诊断、退出状态与显式 `--json`/`--machine`（GC-04，`plan-template.md:219`）。按此口径，多拒绝路径的卡（`SA-01b`、`SA-01c` 各 4 条路径，`SA-03`、`SA-07a`、`SA-07c`、`SA-08j`、`SA-09a`、`SA-09e`）在 G-03 的 8 条上限、G-02（不得拆出只含测试的卡）与「不扩大 waiver」的用户决定下无法同时满足，需用户就 GC-04 的验收粒度（逐失败路径 × 四种输出，或每个错误类一条代表路径，或显式 `--json`/`--machine` 视为共享渲染器的既有机制）作出决定后再整改；其余 R46 P1（`SA-06` 的 `format:` 与 `--stat` 分隔、`SA-09a`/`SA-09e` 拒绝后保留 `am` 状态、`SA-08j` 签名失败的稳定码与 `docs/error-codes.md`、`SA-02` 停止诊断）随该决定一并处理 | 无 | 状态行、评审记录 |
| 2026-11-19 07:17:05 UTC | 用户 2026-11-19 06:51:45 UTC 决定（GC-04 按「每个错误类一条代表路径」验收）；协调者据用户 2026-11-19 05:17:51 UTC 决定的推论；R46 双审 14 项 P1 | 按用户决定与 R46 意见整改：①（用户决定，2026-11-19 06:51:45 UTC）「验证执行协议」第 11 条重写为 11a–11d：错误类按「稳定码 + 失败机制」划分；本计划新增或改变的错误类在交付卡内选一条代表路径，以退出码、人读输出、`--json`、`--machine` 四条 AC 验收（`--json`/`--machine` 为对（进程退出状态，JSON 三元组）的一次结构化比较）；同类其余路径与只是新可达的既有错误类各以 1 条 AC 断言结构化错误报告的稳定码，issue 原文明确要求人读诊断的再加 1 条；全部按 G-03 计数、守 8 条上限（`SA-08j` 按 `EX-SA-01`）；删除 R45–R46 不计数的「输出契约门」（13 张卡的该门全部移除）；每张活动卡新增「错误类清单」（非计数正文）；②（协调者推论，R47）撤回不属所跟踪 issue 验收项的 `SA-01c`（`t3505#14`）、`SA-07d`（`TC-2807`）、`SA-07b`（`TC-2637`）、`SA-09a`、`SA-09e`（#907 不含暂停与恢复）与 `SA-06` 的 #731 以外格式变体（原 AC4–AC8），编号不复用并注明去向（`DEFER-SA-15` 扩充、`DEFER-SA-20` 扩充、新增 `DEFER-SA-21`）；`DEP-SA-05` 随唯一消费卡 `SA-07d` 撤回、本计划不再写 `src/command/commit.rs`；依赖边改为 `SA-01b -> SA-02`、`SA-07 -> SA-07c`、`SA-09 -> REL-SA-02`；`REL-SA-02` 家族缩为 `SA-09f`、`SA-09b`、`SA-09`（1/3–3/3）；活动卡 28 → 23（21 实现 + 2 发布）；`SA-09` 写明 unborn 起点暂停后 `--continue`/`--skip` 与应用路径共用、`--abort` 遇全零 `head_orig` 时 fail-closed 拒绝（`LBR-REPO-003`，保留状态、不改写引用、索引与工作区），其验收随 `DEFER-SA-20` 延后，迁移指引改为先以 `--continue`/`--skip` 结束会话，并新增对应风险行；③（R46 P1 逐条）`SA-01` 以代表路径四项验收空停止（退出码 1、人读诊断、`--json`、`--machine`，7/8）；`SA-01b`、`SA-03` 的 clap 冲突与 `SA-02`、`SA-03` 的冲突停止、`SA-02`/`SA-05` 的未跟踪覆盖、`SA-07c` 的上一检出不可用与非首位 `-` 均为新可达的既有错误类，各断言稳定码（`SA-01b` 与 `SA-03` 按 #725/#726 原文另加冲突诊断）；`SA-07a` 以代表路径四项验收新错误类（退出码 128），「修改」形态断言稳定码，删除不必要的 HEAD 判据（8/8）；`SA-07` 的 detached 已暂存拒绝断言 `LBR-CONFLICT-001` 并保留人读诊断，删去分支引用判据（8/8）；`SA-07c` 去掉无 issue 依据的人读诊断判据（6/8）；`SA-08j` 规定签名失败以新增 `ReplayErrorKind::Sign` → `LBR-AUTH-001`（退出码 128，与 `libra commit` 同码）报告、重放循环与 `--continue` 同类，代表路径为门 G1（AC1 退出码、AC2 人读、AC111 `--json`、AC112 `--machine`），AC10/AC29/AC44/AC52/AC72/AC87 改断言稳定码，写集与文档门加入 `docs/error-codes.md`（第 1e 步），门族 112 个判据（`EX-SA-01` 行、粒度审计表、完成判据同步）；`SA-06` 写明 `format:` 后接 `--stat`/补丁时保留分隔换行（其 Git 对照随格式变体登记 `DEFER-SA-15`）；④同步状态行、文档职责、适用范围、成功定义、缺口表、关系表、评审维度表、`ADR-SA-02`、依赖边与依赖表、发布分组与窗口顺序、Phase 0/1、字段默认、waiver、审计表、测试矩阵、追溯表、里程碑、故障恢复矩阵、风险登记、兼容与文档收口、延后项、Issue 闭环登记与完成判据 | 撤回 `SA-01c`、`SA-07d`、`SA-07b`、`SA-09a`、`SA-09e`（编号不复用）；`DEP-SA-05` 撤回；新增 `DEFER-SA-21` | 全文 |
| 2026-11-19 07:27:42 UTC | Claude R47 PASS + Codex R47 FAIL（6 分区并行，C、D 区 PASS，7 项 P1） | 按 Codex R47 整改：①「验证执行协议」第 3 条改为只在卡声明提交序列对照时要求 `am` 元数据一致，`SA-09` 的元数据对照归 `DEFER-SA-20`；②第 11b/11c 条定义「人读 stderr」（`parse_cli_error_stderr` 分离出的、结构化 JSON 报告之前的人读部分），全部诊断类 AC（`SA-01` AC1、`SA-01b`、`SA-03` AC5/AC7、`SA-07a` AC2、`SA-07` AC6、`SA-08j` 各门）改判人读部分；③`SA-03b` 四条在首个冲突处停止的命令各断言 `LBR-CONFLICT-001`（同时区分被 clap 拒绝的 `LBR-CLI-002`），删去与暂存判据重复的工作区字节判据与 `--no-rerere-autoupdate` 既有语义判据（8/8）；④（GC-07/GC-08，回应 F-1/F-2）`SA-09` 不再写未验收的 fail-closed 拒绝，改为只规定应用路径的 `None` 处理；新增 `REL-SA-02` 家族子卡 `SA-09g`（4/4）：unborn 起点暂停态的 `libra am --abort` 只释放 `am` 状态、不执行硬重置（与 `git am --quit` 相同、与 `git am --abort` 不同，写入文档与 `COMPATIBILITY.md`），以首封、第二封两种暂停验收成功退出、状态释放、引用与工作区不变（8/8）；Git 语义的复原与 `--continue`/`--skip` 仍登记 `DEFER-SA-20`；降级与故障恢复指引改为先 `--abort` 释放会话，不再推荐未验收的 `--skip`；⑤`SA-09d`/`SA-09a`/`SA-09e` 撤回说明改指 `DEFER-SA-20` 与 `SA-09g`；⑥同步依赖边（`SA-09 -> SA-09g -> REL-SA-02`）、家族成员与逆序、活动卡 23 → 24（22 实现 + 2 发布）、阶段、里程碑、字段默认、`ADR-SA-02`、测试矩阵、追溯表、故障恢复矩阵、风险、Issue 闭环登记与完成判据 | 新增 `SA-09g` | 全文 |
| 2026-11-19 07:42:25 UTC | Claude R48 PASS + Codex R48 FAIL（6 分区并行，D、E 区 PASS，8 项 P1） | 按 Codex R48 整改：①（GC-07/GC-08）撤销 R48 的「`--abort` 只释放状态」方案：其出口与既有提示（「复原原分支」）矛盾，且需额外验收索引、符号 HEAD 与两种暂停的稳定码，不比 Git 语义的复原更小；R47 按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回的 `SA-09a`、`SA-09e` 因 `SA-09` 使 unborn 起点暂停可达、须有已验收的退出而恢复（编号与 Git 语义的复原判据不变，各加暂停稳定码判据计 7 条；原各自的「暂停后用户提交即拒绝中止」判据移入重定义的 `SA-09g`），`SA-09g` 改为暂停后用户另行提交时拒绝 `--abort`（沿用既有中止保护的消息与 `LBR-REPO-003`，两种暂停各验收稳定码、用户提交与 `am` 状态保留，6/8，`M`）；首封暂停时 `--skip` 不可用的限制写入风险与 `DEFER-SA-20`；②新增独立卡 `SA-05c`（先于 `SA-05`）：unborn 上带 `--ff` 且索引非空时以既有 `LocalChangesWouldBeOverwritten`（`LBR-CONFLICT-001`）在任何写入前拒绝，验收稳定码、索引、工作区与 unborn 不变（4/8）；born 分支快进不保护已暂存条目的既有缺陷登记 `DEFER-SA-22`；③`SA-07c` 写明原样透传 `resolve_previous_checkout_target` 的读取失败（`LBR-IO-001`）与记录损坏（`LBR-REPO-002`），其专项验收登记 `DEFER-SA-23`；④同步依赖边（`SA-04 -> SA-05c -> SA-05`、`SA-09 -> SA-09a -> SA-09e -> SA-09g -> REL-SA-02`）、`REL-SA-02` 六张子卡与逆序、活动卡 24 → 27（25 实现 + 2 发布，独立发布 13 张）、阶段、里程碑、字段默认、`ADR-SA-02`、测试矩阵、追溯表、故障恢复矩阵、风险、延后项、Issue 闭环登记与完成判据 | 新增 `SA-05c`；恢复 `SA-09a`、`SA-09e`；重定义 `SA-09g` | 全文 |
| 2026-11-19 07:48:33 UTC | Claude R49 PASS + Codex R49 FAIL（6 分区并行，C、D、E 区 PASS，8 项 P1） | 按 Codex R49 整改：①「验证执行协议」第 11a 条写明「诊断模板」指 `CliError` 消息，提示改写不构成新错误类但须以 1 条「人读 stderr」判据验收；②`SA-09g` 收窄为首封暂停后用户已提交 root 时拒绝 `--abort`（沿用既有中止保护的消息与 `LBR-REPO-003`，提示改为 `the branch was unborn when am paused`），验收暂停稳定码、拒绝稳定码、改写后的提示、用户提交与 `am` 状态保留（5/8），fixture 写明用户提交前先改写 `a.txt`；第二封暂停后用户另行提交的既有保护（代码不变）的专项验收登记 `DEFER-SA-20`；③`SA-09a` 写明 `--abort` 的写入顺序（保护判定 → 空树复原 → 删除分支引用 → 清除状态）与中断后重试合同，新增测试 failpoint `LIBRA_TEST_AM_FAIL_AFTER_ABORT_REF_DELETE` 并以 AC8 验收重试后释放状态（8/8）；④`SA-05c` 增加拒绝后无 `sequence_state` 行的判据（5/8），`SA-05` 写明已拆出 `SA-05c`（G-09）；⑤删去「首封暂停支持解决后的 `--continue`」的说法（`SA-09`、`SA-09e`、风险表），文档只推荐已验收的 `--abort`；⑥同步追溯表、测试矩阵、故障恢复矩阵、`DEFER-SA-20` 与 `REL-SA-02` 聚合守卫 | `SA-09g` 收窄 | 全文 |
| 2026-11-19 07:55:13 UTC | Claude R50 PASS + Codex R50 FAIL（6 分区并行，C、D、E 区 PASS，6 项 P1） | 按 Codex R50 整改：①`SA-09a` 的复原改为「中止保护 → 空树复原索引与工作区 → 同一 SQLite 事务中删除分支引用并清除 `am` 状态」（`Branch::delete_branch_result_with_conn`、`sequencer::clear_am_with_conn`），消除「引用已删而会话仍在」的中间态，从而不再可能在重试时删除中断后新建的提交；删去 R50 的 AC8 与 `LIBRA_TEST_AM_FAIL_AFTER_ABORT_REF_DELETE`（7/8）；②新增 `REL-SA-02` 家族子卡 `SA-09h`（7/7）：测试 failpoint `LIBRA_TEST_AM_FAIL_AFTER_ABORT_RESTORE`（空树复原之后、事务之前）注入后验收分支引用与会话保留，不带注入重试后验收成功退出、回到 unborn、符号 HEAD、索引、工作区与状态释放（8/8）；③`SA-09g` 改为 `head_orig=None` 会话的统一 fail-closed 判定（当前 HEAD 存在且不等于 `expected_head` 即拒绝，沿用既有消息与 `LBR-REPO-003`），以首封暂停后用户提交 root 并留下已暂存与未暂存改动为代表路径，验收暂停码、拒绝码、提示行序列恰为改写后的两条、用户提交、符号 HEAD、索引、工作区与会话保留（8/8）；④`SA-05c` 的提示改为只给出 `nothing was written; unstage the files ...`，以提示行序列判据 AC6 验收（6/8）；⑤同步依赖边（`SA-09g -> SA-09h -> REL-SA-02`）、家族七张子卡与逆序、活动卡 27 → 28（26 实现 + 2 发布）、阶段、里程碑、字段默认、`ADR-SA-02`、测试矩阵、追溯表、故障恢复矩阵、风险、`DEFER-SA-20`、Issue 闭环登记与完成判据 | 新增 `SA-09h`；`SA-09g` 重定义 | 全文 |
| 2026-11-19 07:59:57 UTC | Claude R51 PASS + Codex R51 FAIL（6 分区并行，B–E 区 PASS，3 项 P1） | 按 Codex R51 整改：①`SA-09a` 的删除事务改为条件删除——只在分支引用仍指向 `expected_head`（首封暂停时为仍不存在）时删除并清除 `am` 状态，受影响行数不为 1 即回滚、保留会话并以 `SA-09g` 的同一中止保护错误拒绝，消除判定与删除之间的并发窗口；②新增 `REL-SA-02` 家族第 8/8 张子卡 `SA-09i`（R53 注：本行原写作「子卡 `SA-09i`（8/8）」，指家族位次，其判据数为 4/8）：测试 failpoint `LIBRA_TEST_AM_ABORT_MOVE_BRANCH_TO=<OID>`（复原之后、事务之前移动分支）下验收拒绝码、被移动后的 tip、符号 HEAD 与会话保留（4/8）；③`SA-09h` 改为在注入后验收索引与工作区已复原（证明中断点位于复原之后）而分支引用与会话保留，重试后验收成功退出、回到 unborn、符号 HEAD 与状态释放（8/8）；④同步依赖边（`SA-09h -> SA-09i -> REL-SA-02`）、家族八张子卡与逆序、活动卡 28 → 29（27 实现 + 2 发布）、阶段、里程碑、字段默认、`ADR-SA-02`、测试矩阵、追溯表、故障恢复矩阵、风险、`DEFER-SA-20`、Issue 闭环登记与完成判据 | 新增 `SA-09i` | 全文 |
| 2026-11-19 08:04:06 UTC | Claude R52 PASS + Codex R52 FAIL（6 分区并行，C、D、E 区 PASS，6 项 P1） | 按 Codex R52 整改：①（G-02）实现归属重排：`SA-09a` 只交付复原语义（中止保护判定 → 空树复原 → 以既有 `Branch::delete_branch_result_with_conn` 删除分支引用 → 清除状态），`SA-09h` 交付把删除与清除合并为同一 SQLite 写事务并置于复原之后（含中断后重试的 failpoint 与验收），`SA-09i` 交付事务内的期望 OID 比较（含并发移动的 failpoint 与验收），三卡各自拥有实现与验收；②`SA-09i` 的比较分两支：`expected_head=Some` 时须仍指向该提交才删除，`None` 时须仍不存在并只清除状态，不再以「删除恰 1 行」为条件；删除仍经既有函数（linked worktree 检出保护、分支元数据级联不变，失败即整个事务回滚）；③判定与复原之间并发进程向同一工作区/索引写入未提交改动的互斥不在本计划，登记 `DEFER-SA-24` 并加风险行；④`SA-05c` 的提示改为只说明「索引须为空」的前置条件，不再规定可能失败的 `libra rm --cached`；⑤更正 R52 修订历史中 `SA-09i` 的判据数写法；⑥同步 `DEFER` 范围（..`DEFER-SA-24`）、故障恢复矩阵与 Issue 闭环登记 | `SA-09a`/`SA-09h`/`SA-09i` 职责重排 | 全文 |
| 2026-11-19 08:07:50 UTC | Claude R53 PASS + Codex R53 FAIL（6 分区并行，B–E 区 PASS，2 项 P1） | 按 Codex R53 整改：①`SA-09h` 的测试 failpoint 改为写事务内、删除分支引用之后与清除 `am` 状态之前的 `LIBRA_TEST_AM_FAIL_IN_ABORT_TXN`，注入后验收分支引用与会话都仍在（删除随事务回滚）、索引与工作区已复原，重试后验收成功退出、回到 unborn、符号 HEAD 与状态释放（8/8，具名用例改为 `am_unborn_abort_txn_rollback_then_retry`）；②`SA-09i` 的 failpoint 在引用不存在时创建引用，新增「首封形态」（`expected_head=None`，以 `libra commit-tree` 生成不挂引用的 root `Y`，注入后分支被并发创建）并验收拒绝码、`Y` 与会话保留（7/8，新增具名用例 `am_unborn_first_pause_abort_refuses_when_branch_created_before_txn`）；③同步测试矩阵、故障恢复矩阵与 `REL-SA-02` 聚合守卫 | 无新卡 | `SA-09h`、`SA-09i`、`REL-SA-02`、测试矩阵、追溯表、故障恢复矩阵 |
| 2026-11-19 08:10:35 UTC | Claude R54 PASS + Codex R54 FAIL（6 分区并行，A、B、D、E 区 PASS，3 项 P1） | 按 Codex R54 整改：①`SA-07c` 的标题、描述与追溯行改为只验收 #728 验收项 4、5 的核心断言，验收项 6（上游 `t3501#12` 全量与 `TC-0484`）登记 `DEFER-SA-13`；②`SA-09h` 的 failpoint 在事务内经同一连接确认分支引用已删除后才以 `test-injected am abort failure after deleting the branch ref in the transaction` 失败，新增该消息的人读判据，删去与 `SA-09a` 同路径的重试后符号 HEAD 判据（仍 8/8）；③`SA-09f` 以「SHA-256 下非零 64 位 OID 读回 `Some`」替换「SHA-1 非十六进制输入报错」（后者由 `parse_hex_for_kind` 既有校验承担，仍 8/8） | 无新卡 | `SA-07c`、`SA-09h`、`SA-09f`、追溯表 |
| 2026-11-19 08:13:17 UTC | Claude R55 PASS + Codex R55 FAIL（6 分区并行，B–F 区 PASS，1 项 P1） | 按 Codex R55 A 区意见更正「与其它计划的关系」的 plumbing-misc/checkout-switch-restore 行：`DEP-SA-04` 约束的 `REL-SA-02` 家族入口为 `SA-09f`（`SA-09b` 与消费方 `SA-09` 同样登记），与依赖登记表、实施顺序和发布分组一致；其余正文未改 | 无 | 与其它计划的关系、状态行 |
| 2026-11-19 08:15:49 UTC | Claude R56 PASS + Codex R56 PASS（A 区全量复审、B–F 区差异确认） | 记账：双审对同一受审 SHA-256 `62bc0c5cc683820fb0b88c4ef743642b2fbbc5058743b9ac88bd314d857e02cb` 均 `VERDICT: PASS`，计划可提交；只更新状态行并追加本行与评审记录 PASS 行，正文未改 | 无 | 状态行、评审记录 |

## 已决议设计决策

### ADR-SA-01: 保留 `cherry-pick` / `revert` 原子 sequencer 状态机与可恢复停止边界

- **Status:** Accepted
- **Context:** #725 要求空 cherry-pick 保留停止态以便 `--skip`/`--abort`/`--quit`；#728 指出前置拒绝（dirty-worktree）或 `--abort` 后残留状态会阻塞后续 `checkout`，而普通冲突停止态必须保留以便 `--continue`/`--abort`。
- **Decision:** 仅在进入可恢复停止态（三方合并冲突，或 `EmptyMode::Stop` 的空/冗余停止）时持久化 sequencer 与冲突 sidecar；前置拒绝、不可恢复错误、`--abort`/`--quit` 与成功完成时原子清理全部 sequencer 与冲突 sidecar。
- **Alternatives considered:** 一律清理或一律保留——均无法同时满足可恢复停止与不阻塞 `checkout`。
- **Consequences:** `SA-01` 验证 cherry-pick 停止保留与收口；`SA-07a` 验证 revert 前置拒绝不留状态；revert 冲突停止保留与 `--continue`/`--abort` 后的清理为既有行为，由既有用例 `test_revert_conflict_then_continue`、`test_revert_conflict_then_abort`（`tests/command/revert_test.rs:843,990`）守护（列入 `SA-07a`、`SA-07` 的共享回归组），其完整快照专项验收登记 `DEFER-SA-18`。
- **Revisit when:** sequencer 统一迁移到单一共享状态机时（`DEFER-SA-01`）。

### ADR-SA-02: `am` 的 unborn `head_orig` / `expected_head` 单一持久化编码

- **Status:** Accepted
- **Context:** `sequence_state.head_orig`/`current_oid` 为 `TEXT NOT NULL`（`sql/migrations/2026070401_sequence_state.sql:6-15`，经 `2026071901_sequencer_worktree_scope.sql` 重建），`AmState.head_orig`/`expected_head: ObjectHash`（`src/command/am.rs:140-144`）与 `AmSequenceState.head_orig: String`（`src/internal/sequencer/mod.rs:125`）都假设存在提交，而 `am` 每次开始都先以 `claim_start` 持久化这两个字段（`src/command/am.rs:292-304`），故 #907（unborn 分支上的 `am`）连成功路径也需要表达「无提交」；本计划不改 schema。`cherry-pick` 的 unborn 快进（`SA-05`）不持久化 sequencer 状态，`CherryPickState.head_orig`（`src/command/cherry_pick.rs:3164`）的 unborn 表达随 unborn 非快进 pick 登记 `DEFER-SA-17`。
- **Decision:** `AmState` 的两个字段改为 `Option<ObjectHash>`（`AmSequenceState` 保持 `String` 列映射），`None` 在持久化层统一写成仓库哈希算法的全零 OID：`ObjectHash::zero_str(kind)`（SHA-1 为 40 个 `0`、SHA-256 为 64 个 `0`）；读取时全零 OID 解析为 `None`，其它值经 `crate::internal::object_format::parse_hex_for_kind(kind, …)` 严格解析（旧行兼容，解析失败返回错误；不用读进程全局 kind 的 `parse_repo_oid`，见 `src/internal/object_format.rs:47-57`）。转换函数显式接收 `HashKind` 参数、不读进程全局哈希类型，以便单元测试分别覆盖两种算法而无需 `#[serial(hash_kind)]`。编解码函数只有一份，位于 `src/internal/sequencer/mod.rs`（GC-02 单一事实源），由 `REL-SA-02` 家族子卡 `SA-09f` 交付，`SA-09b` 把 `am` 两个字段接入同一函数；BLAKE3 与 SHA-256 的全零串相同（64 个 `0`），故单元测试按 20/32 字节两种长度覆盖。`collect_sequencer_state_roots` 已跳过全零 OID（`src/command/maintenance.rs:4094-4102`），不改其生产逻辑；`am` 行的 `ORIG_HEAD` 投影现状因 `SequenceKind::from_token` 不识别 `am` 而报错（不会暴露全零 OID，见事实基线「GC 根 / `ORIG_HEAD`」行），二者的专项验收登记 `DEFER-SA-20`。
- **Alternatives considered:** 新增 migration 让列可空——违反不改 schema 的边界；非 OID 哨兵 `"UNBORN"`——会被 GC 根遍历判为 `RepoCorrupt`（`src/command/maintenance.rs:4119-4125`）并被 `pseudo_ref` 暴露为 `ORIG_HEAD`。
- **Consequences:** 存在活动/暂停的 unborn `am` 时禁止直接降级到不识别全零 OID 的旧二进制，且禁止手工删除状态表或目录；用户须先 `libra am --abort` 回到 unborn（`SA-09a`、`SA-09e`；会话中 HEAD 已不同于暂停点时按 `SA-09g` 拒绝，须先移走新提交；中断后再次运行 `--abort` 即完成复原，`SA-09h`；删除前分支被并发移动时拒绝，`SA-09i`；unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再降级（各卡 `Migration and rollback` 重申）。
- **Revisit when:** sequencer 持久化 schema 统一重构时。

### ADR-SA-03: `rebase` 重放提交的签名只经单一出口

- **Status:** Accepted
- **Context:** `create_replayed_commit`（`src/command/rebase.rs:5850-5940`）的 8 个 action 对 `gpgsig` 处理各异（复制、重附或丢弃），均不按 `commit.gpgSign` 重签；另有交互式消息编辑（`edit_interactive_commit_message`，`src/command/rebase.rs:1402-1443`）与 `edit` 停止后吸收已暂存改动（`amend_head_from_index_if_staged`，`src/command/rebase.rs:2024-2065`）两条路径以 `Commit::new` 直接生成不签名提交（事实基线「rebase」行）。逐路径分别修补会再次分叉。
- **Decision:** rebase 生成提交的全部路径——重放循环与 `--continue` 中的 8 个 action，以及重放后另行生成提交的交互式消息编辑（`edit_interactive_commit_message`）与 `edit` 停止后吸收已暂存改动（`amend_head_from_index_if_staged`）——都先构造不含任何 `gpgsig` 的最终提交内容（丢弃来源/目标提交的旧签名，`--signoff` 尾注与消息、树的改写都在签名前完成），再经同一出口按 `libra commit` 的同一签名决策（`commit.gpgSign` 优先；未设时按显式配置的 `vault.signing=true`；两者都未设时不签名，即 `DEP-SA-01` 交付的默认；`gpg.format=ssh` 由 `DEP-SA-02` 交付）签名或不签名。出口在签名失败时返回错误且不写任何对象。任一重放步（含第一步）的出口签名失败——包括签名不可用（如无 unseal key）——都按冲突停止同构处理（对全部经 `create_replayed_commit` 构造提交的重放步共用——重放后改写路径（交互式消息编辑的二次提交、`edit` 停止后的修订提交）的二次签名失败不经该停止路径，出口仍不写对象、命令以非 0 退出，其恢复合同与专项门登记 `DEFER-SA-08`；与 Git 一致：Git 的 sequencer 在提交（含签名）失败时停止并保留可恢复状态、不做签名能力预检——Git 源码 `sequencer.c` 的 `do_pick_commit` 把提交失败经 `res = !!res` 归入 `PICK_RESULT_CONFLICTS`，`pick_one_commit` 对普通步骤以 `error_with_patch`、对 `fixup`/`squash` 折叠步骤以 `error_failed_squash` 停止）：先把该步合并树写入索引与工作区，再记录 `stopped_sha`、保留 rebase 状态并以非 0 退出，不移动任何分支或标签引用（HEAD 停在失败步之前的重放结果，第一步失败时即 `<upstream>`）；`--continue` 以索引树经出口重新签名生成该步提交（补丁不丢失），该步签名再次失败时停止态不变、可重试；`--abort` 复用既有 `run_rebase_abort` 回退。快进与「已是最新」不生成提交、不签名，行为不变。
- **Alternatives considered:** 只修 Pick/Reword/Edit——Fixup/Amend 仍残留无效旧签名；保留旧签名——内容已变，旧签名必然无效；签名失败后原子回滚——需对多步重放、autostash 与回滚自身失败逐一给出恢复合同；沿用现有内部错误路径——该路径在写入索引前停止，`--continue` 会以旧索引生成提交并丢失补丁（`src/command/rebase.rs:4121-4127,4421-4488`）；签名能力预检（R34–R37 曾采用，`SA-08i`、`SA-08l`）——Git 没有该预检（同上 `do_pick_commit`/`pick_one_commit` 路径），且要证明「任何写入之前」须同时覆盖交互入口（`src/command/rebase.rs:938-940` 先于 `prepare_rebase_aux` 分流）、autostash 的对象写入与对象库快照，而冲突同构停止已为签名不可用提供完整恢复路径，R38 撤回（两卡编号不复用）。
- **Consequences:** 出口、`Pick` 接入、OpenPGP 签名与出口级签名失败由 `SA-08a` 交付，`reword`、`edit`、`fixup`、`squash` 由 `SA-08b`、`amend!`、`fixup -C`、`fixup -c` 由 `SA-08m` 迁入，`gpg.format=ssh` 由 `SA-08g`、重放后改写（`--signoff` 尾注、交互式消息编辑、`edit` 停止后的修订）先于签名由 `SA-08h`、签名失败的冲突同构停止、重试、`--continue` 与 `--abort` 由门族卡 `SA-08j`（`EX-SA-01`；原 `SA-08k` 已并入）交付；六卡组成家族 `REL-SA-01`，只在全部就位后对外发布，因此不存在部分收口的发布版本。`SA-08` 的签名 TC 依赖该出口。
- **Revisit when:** 提交签名策略在 `plan-20261117-commit-signing.md` 之外再次变更时。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01..13`（本计划不触及 SQLite schema、连接建构或 migration，GC-13 无适用写集）。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01..14`。执行期 ER-05 代码评审按模板默认由 Codex 给出 `PASS`（本计划不在 v2.16 具名 code-only 范围内）。

### 验证执行协议（全计划强制，适用于每卡 `Verification`）

1. **卡特有用例（计入 G-03 VER）：** 每条形如 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=<完整测试路径>)'`（库内单元测试用 `--lib` 代替 `--test command_test`）。`test(=…)` 是测试名全等匹配，同一测试二进制内测试名唯一，故至多匹配 1 个；`--no-tests=fail` 使零匹配以 exit 4 失败。判定：exit 0 且 nextest 汇总为 1 run / 1 passed / 0 failed（被过滤排除的其它用例计入 nextest 的 skipped，只记录、不作判据）。`command_test` 内的完整路径为 `command::<模块>::<函数>`；库内单元测试为 `<模块路径>::tests::<函数>`——`src/command/` 下为 `command::<模块>::tests::<函数>`，`src/internal/` 下为 `internal::<模块>::tests::<函数>`（如 `SA-09f` 的 `internal::sequencer::tests::…`；事实基线「测试命名」行）。需要列出测试名的检查（`cargo nextest list … -T oneline`）按「每行为 `<binary-id>` 空白 `<test-name>`，取第二个空白分隔字段」解析。
   - 1b. **发布点聚合守卫（本条唯一例外）：** 发布卡可用多个 `test(=…)` 以 `|` 组成的并集过滤一次运行多个已登记的子卡用例；判定为 exit 0、run 数等于过滤中列出的测试数、passed 等于 run、failed 为 0（被过滤的其它用例计入 skipped，只记录）。该命令计 1 个 VER。
2. **共享回归组（ER-04 A 组 focused 门，不计入 G-03 VER，不作为卡特有 AC 证据）：** `source .env.test && cargo nextest run --test <target> --no-tests=fail -E 'test(/^command::<模块>::/)'` 或整 target 运行；要求 exit 0、failed=0；记录 run/passed/skipped（skipped 含被过滤与既有 `#[ignore]` 用例，只记录不作判据）。共享回归组与「文档与网站验收命令」中的 compat 守卫属模板 ER-04/ER-06 强制门，不计入 G-03；一条命令含多个 `--test` target 时按 target 分别记录 run/passed/failed，任一 target failed>0 即 STOP。
3. **Git 对照用例：** 测试内调用真实 `git` 生成对照结果，两侧隔离 `HOME`，设置 `GIT_CONFIG_NOSYSTEM=1`，固定 `GIT_AUTHOR_NAME/EMAIL/DATE` 与 `GIT_COMMITTER_NAME/EMAIL/DATE`，未声明签名的用例两侧均 `commit.gpgSign=false`；测试打印 `git --version`；`git` 不可用即失败，不得 skip。AC 中「提交序列等于 Git 对照」指两侧新提交按顺序逐个比较的（树 OID、父提交个数、author 行、消息）元组序列相等（含提交数相同；author 行为 `cat-file -p` 中以 `author ` 开头的整行，消息为首个空行之后的全部字节；一次序列比较。`am` 用例若声明「提交序列等于 Git 对照」，作者、作者日期与消息来自邮件，故三者都须与 Git 一致；本计划 `SA-09` 只对照树序列与父链，`am` 元数据对照登记 `DEFER-SA-20`；提交者与签名不比较）；「主题序列」指按顺序的提交主题行序列相等。
4. **AC 写法与计数（G-03）：** 每条 AC 只陈述一个可独立判定的性质，按条计数；同一场景需要多个性质时分列多条 AC。测试内部为定位问题附加的诊断断言不构成额外验收谓词，AC 的通过与否只由该条性质决定。验收范围：每张卡只验收本卡新增或改变的行为；本卡不改变的既有机制（如 `cherry-pick --abort` 的硬重置复原、rebase 的折叠语义、`revert --abort` 的 `restore_to_orig_head`）由该卡共享回归组中点名的既有用例守护，不重复计入 AC；恢复类 AC（`--continue`/`--skip`/`--abort`）对本卡新增或改变的恢复路径逐项验收成功或失败退出、状态释放与复原结果。
5. **被测内容绑定与记录（按模板顺序：A/B 与 C 组测试在提交前运行）：** 开工时（ER-01）以 `BASE_SHA="$(libra rev-parse HEAD)"` 记录基线。运行任何验收门前执行 `libra status --short --untracked-files=all`：除 `docs/development/plan/` 下的记账文件外，所列路径必须全部属于本卡 `Implementation write set`（独立卡与发布点在 C 组 bump 后另含 `Release write set`），否则按 ER-01 STOP；对每个现存路径记录 `libra hash-object <路径>` 的 blob OID 与 `ls -l` 的可执行位，对删除路径记录「删除」，形成被测快照。门全部通过且评审 `PASS` 后不得再改这些文件；随后形成本卡提交并以 `TESTED_SHA="$(libra rev-parse HEAD)"` 固定，逐路径核对：现存路径的 `libra rev-parse "$TESTED_SHA:<路径>"` 等于快照中的 blob OID 且 `libra ls-tree "$TESTED_SHA" -- <路径>` 的模式与快照的可执行位一致，删除路径的 `libra rev-parse "$TESTED_SHA:<路径>"` 失败；提交后 `libra status --short --untracked-files=all` 除记账文件外无输出。任一不符即在 `TESTED_SHA` 上重跑全部门。每次运行在卡内记录命令、exit code、nextest 汇总行、被测快照与 `TESTED_SHA`、`libra rev-parse "$TESTED_SHA^{tree}"`；不得以模块回归或全量结果替代卡特有用例。
6. **零命中守卫：** 一律使用模板「Verification 判定口径」的 `rg` 三分支模板（区分 exit 1 与 >1）。
7. **提交签名（ER-07）：** `DEP-SA-01` 交付后 `libra commit` 默认不签名，因此本计划在本仓库与网站仓库的所有提交（含家族子卡本地提交）一律使用 `libra commit -S -s -m "<msg>"`，提交前记录 `libra config get commit.gpgSign` 的值与 `-S` 覆盖的事实；提交后以 `COMMIT_SHA="$(libra rev-parse HEAD)"` 固定 SHA，`libra log -1 --pretty=raw "$COMMIT_SHA" | rg -q '^gpgsig '` 与 `libra log -1 --only-trailers --trailer Signed-off-by "$COMMIT_SHA" | rg -q '^    Signed-off-by: '` 均成功才可推送；签名失败时 `libra commit` 不产生提交，按 ER-10 处理。
8. **SSH 验签规则：** 从 `libra cat-file -p <提交>` 输出的原始提交对象中取以 `gpgsig ` 开头的头行及其后全部以单个空格开头的续行；去掉首行的 `gpgsig ` 前缀与每个续行开头的一个空格，按行写入签名文件；载荷为原始对象删除该头行及其全部续行后的完整字节，经标准输入传给 `ssh-keygen -Y verify -n git -f <allowed_signers> -I <principal> -s <签名文件>`，要求 exit 0；同一检查再以改动载荷 1 个字节的副本运行一次，要求 exit 非 0（证明验签确实检查内容）。`ssh-keygen` 不可用即失败。
9. **家族提交序列与改动集（发布点专用）：** 家族首张子卡（`REL-SA-01` 为 `SA-08a`，`REL-SA-02` 为 `SA-09f`）按第 5 条在卡内记录的 `BASE_SHA` 即该家族的 `FAMILY_BASE_SHA`，在首张子卡开工、尚未形成任何家族提交时记录；每张家族子卡按第 5、7 条在卡内记录其唯一提交的 `COMMIT_SHA`。家族窗口期禁止插入其它切片的提交。发布点在 A/B 阶段执行以下三项，任一命令非 0 退出或任一不满足即 STOP：
   - 9a. **提交身份与完整性：** `libra rev-list "$FAMILY_BASE_SHA..HEAD"` 列出的每个提交 `<c>`，或等于某张家族子卡记录的 `COMMIT_SHA`，或为独立记账提交——`libra cat-file -p <c>` 恰有 1 行以 `parent ` 开头（单父提交），且 `libra diff --name-only "<c>^" "<c>"` 只列出 `docs/development/plan/` 下的路径（本计划文件、`plan-status.md` 等）；另外每张家族子卡最后记录的 `COMMIT_SHA` 都出现在该列表中。
   - 9b. **逐提交改动集：** 对每张家族子卡的 `COMMIT_SHA`（记为 `<c>`），`libra diff --name-only "<c>^" "<c>"` 只列出该子卡 `Implementation write set` 中的本仓库路径或 `docs/development/plan/` 下的记账文件（模板要求卡推进时在同一变更中更新 `plan-status.md` 并把验收证据写入本计划，故子卡提交可含这两类记账路径）。
   - 9c. **最终改动集：** `libra diff --name-only "$FAMILY_BASE_SHA" HEAD` 逐行与家族子卡 `Implementation write set`（本仓库内路径）及 `docs/development/plan/` 比对，差集为空。
   - 9d. **重建：** 若发布前家族提交需移到新的上游基线之上（上游前移后重建），以新基线的 `libra rev-parse` 结果在首张子卡内追加记录新的 `FAMILY_BASE_SHA` 并注明原因；无论各子卡自身文件字节是否变化，每张子卡都在重建后的提交上按第 5 条重新形成被测快照、重跑本卡全部门（卡特有 VER、共享回归组与文档门），并以绑定新 `FAMILY_BASE_SHA` 与新提交树的评审输入重新取得该卡 ER-05 评审 `PASS`，再按第 7 条重新校验重写后提交的签名与 DCO，追加记录新的 `TESTED_SHA` 与 `COMMIT_SHA`；最后发布点在新的最终树上对完整提交序列重新执行 9a–9c、9e 与家族聚合守卫。
   - 9e. **线性链与顺序：** 令 `c=HEAD`，循环直到 `c` 等于 `FAMILY_BASE_SHA`：`libra cat-file -p "$c"` 必须恰有 1 行以 `parent ` 开头，记录 `c` 后令 `c="$(libra rev-parse "$c^")"`；循环中若到达无父提交仍未遇到 `FAMILY_BASE_SHA` 即 STOP。记录到的提交集合必须与 9a 的 `libra rev-list` 输出相同，且家族子卡最后记录的 `COMMIT_SHA` 在该链（自 `FAMILY_BASE_SHA` 向 `HEAD`）上的先后顺序与「发布分组与并发窗口」登记的子卡顺序相同。
10. **合入树发布前门（独立发布卡与家族发布点，C 组；ER-08 发布步之前）：** PR 合入 `main` 后、`gh release create` 之前，执行 `libra fetch origin main` 并取得本卡 PR 的 merge 提交 `MERGE_SHA`（`gh pr view --json mergeCommit --jq .mergeCommit.oid`），比较 `libra rev-parse "$MERGE_SHA^{tree}"` 与第 5 条的 `libra rev-parse "$TESTED_SHA^{tree}"`：相等才可 `gh release create v<version> --target "$MERGE_SHA"`。不相等即停止发布，在 `MERGE_SHA` 的检出上依次重跑并全部通过：①本卡全部卡特有 `Verification`（第 1 条；家族发布点为其第 1b 条聚合守卫——第 9 条的家族提交序列守卫只针对合入前的家族提交，不在 merge 提交上重跑）；②本卡登记的共享回归组（第 2 条；家族发布点为各子卡登记的共享回归组）；③本卡文档门的全部检查（家族发布点为各子卡的文档门）：「文档与网站验收命令」第 1a–1e 步在 merge 树上重做——1a 对每个登记文件比较 `TESTED_SHA` 与 `MERGE_SHA` 中的字节，相同者沿用原逐文件结论并记录，不同者重新逐文件核对覆盖与 1b 的 EN/zh-CN 一致；1c–1e 的 compat 门重跑——以及卡内文档门登记的其它检查（如 `SA-09` 的测试索引同步检查）；④C 组全部适用步骤（本卡 `Full-suite trigger` 登记的全量门、`compat_version_surface_sync`、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo build --release`、按 ER-08 安装并验证安装后的二进制版本）；再以 merge 树为评审输入重新取得 ER-05 评审 `PASS`。①–④ 与评审的每项结果都记录同一 `MERGE_SHA` 与 `libra rev-parse "$MERGE_SHA^{tree}"`，全部通过后才以该 `MERGE_SHA` 为 `--target` 发布；任一失败即不发布，按 ER-10 前滚修复（新的 PR 与新的 `MERGE_SHA`，从本条重新开始）。D6 只记录该比较与重跑结果的远端证据。
11. **输出与错误契约（GC-04；按用户 2026-11-19 06:51:45 UTC 决定「每个错误类一条代表路径」验收，取代 R45–R46 不计入 G-03 的「输出契约门」）：**
   - 11a. **错误类与范围：** 失败按「稳定 `LBR-*` 码 + 失败机制（产生该错误的同一代码出口及其诊断模板）」划分错误类，同一机制在不同输入下的失败是同一类的不同路径。需要代表路径的是本计划新增的错误类，以及本计划改变其稳定码、退出码或诊断模板的既有错误类：每个这样的错误类在交付它的卡内选一条代表路径。本计划的改动只是使某个既有错误类（含本计划前序卡已交付的错误类）在新的输入上可达、且不改变其稳定码、退出码与诊断模板时，不另设代表路径，该输入按 11c 作为其余路径验收。已登记 `DEFER-*` 的失败路径不在本卡验收范围。「诊断模板」指 `CliError` 的消息文本；提示（hint）随输入改写不构成新的错误类，但卡内写明的提示改写须以 1 条「人读 stderr」判据验收。每张活动卡在 `Acceptance criteria` 之后的「错误类清单」（非计数正文）写明：本卡新增或改变的每个错误类（稳定码、退出码、机制）、其代表路径与四项判据所在的 AC、其余路径与其 AC；只命中既有错误类或没有失败路径时也如实写明。
   - 11b. **代表路径四项（各为 1 条 AC）：** ①退出码——默认运行（不加 `--json`/`--machine`；测试以 `env_remove` 清除 `LIBRA_FINE_EXIT_CODES`，退出码取 `CliError::exit_code` 的覆盖值或稳定码的默认值，`src/utils/error.rs:1066-1076`）的进程退出状态等于卡内写明的数值；②人读输出——默认运行的「人读 stderr」（命令以 `CliError` 失败时，指 `parse_cli_error_stderr` 返回的人读部分，即 stderr 末尾结构化 JSON 报告之前的全部文本，`tests/command/mod.rs:516-532`；诊断只出现在 JSON 报告中不算）含卡内写明的诊断文本；③`--json`——在同一 fixture 的独立副本上以全局参数运行 `libra --json <同一子命令与参数>`，（进程退出状态，stderr 整体解析出的 JSON 对象的（`ok`、`error_code`、`exit_code`）三元组）等于（卡内写明的退出码，（`false`、卡内写明的稳定码、同一退出码））；④`--machine`——在另一份独立副本上运行 `libra --machine <同一子命令与参数>`，（进程退出状态，stderr 按行解析出的三元组序列）等于（同一退出码，[（`false`、同一稳定码、同一退出码）]）。③④各为一次结构化比较，解析失败或行数不符即不成立（`--machine` 即 `--json=ndjson` 加安静模式，`src/utils/output.rs:170-182`；显式 JSON 模式的错误包络写 stderr，`src/utils/error.rs:1137-1164`）。
   - 11c. **其余路径（各为 1 条 AC）：** 断言默认运行的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`，`tests/command/mod.rs:516-532`）的 `error_code` 等于该类稳定码（该报告只在命令以 `CliError` 失败时产生，故同一判据也排除成功退出与 panic）；所跟踪 issue 原文明确要求该路径人读诊断的，再加 1 条 AC 断言该次运行的「人读 stderr」含该诊断。
   - 11d. **计数：** 以上 AC 与其它 AC 一样按 G-03 每个独立谓词计 1 项、受 8 条上限约束（唯一例外为 `SA-08j` 的 `EX-SA-01`）；不再设任何不计数的输出模式门。

### 文档与网站验收命令（全计划强制，ER-06/ER-06a；不计入 G-03）

1. **仓库文档逐文件验收（每卡）：**
   - a. 提交前，对本卡「Docs and compatibility impact」所列每个仓库内文件记录相对 `BASE_SHA` 的状态并逐文件核对是否覆盖本卡行为：需要改动的文件必须出现在 `libra status --short --untracked-files=all -- <文件>` 的输出中；已准确覆盖、无需改动的文件走「无变化路径」——`libra hash-object <文件>` 等于 `libra rev-parse "$BASE_SHA:<文件>"`，并在卡内写明覆盖本卡行为的段落位置。提交后以 `libra diff --name-only "$BASE_SHA" "$TESTED_SHA" -- <文件>` 核对：改动文件必须列出、无变化文件必须不列出。每个文件都必须有其中一种记录，缺一即 STOP。
   - b. EN 与 zh-CN 同节同义；示例命令、退出码、错误码与本卡 AC 一致；逐文件记录结论。
   - c. 改 `docs/commands/**` 时：`source .env.test && cargo nextest run --no-tests=fail --test compat_command_docs_examples_section --test compat_help_examples_banner`（exit 0；按 target 分别记录）。只改开发文档的卡（`SA-09f`、`SA-09b`）不适用，记录「不适用：未改 `docs/commands/**`」。
   - d. 改 `COMPATIBILITY.md` 时：改动前后 `wc -l < COMPATIBILITY.md` 输出相等，且 `source .env.test && cargo nextest run --no-tests=fail --test compat_matrix_alignment --test compat_ledger_schema`（exit 0）。
   - e. 改 `docs/error-codes.md` 时：`source .env.test && cargo nextest run --no-tests=fail --test compat_error_codes_doc_sync`（exit 0）。
2. **网站 checkout 前置（每次写网站前，`$LIBRA_BACKEND` 按「文档职责」判定；`DEP-SA-10` 未满足即 STOP）：**
   - a. `cd "$LIBRA_BACKEND" && test -d .libra && test ! -e .git && libra status --short --branch`：首行必须是 `## cf...origin/cf`。
   - b. `cd "$LIBRA_BACKEND" && libra fetch origin cf && BASE="$(libra rev-parse origin/cf)" && [ "$(libra rev-parse HEAD)" = "$BASE" ]`：本地 `cf` 不得含超前提交，否则 STOP（不得推送无关提交）。
   - c. `libra status --short` 中没有已暂存条目。
   - d. 本卡网站文件只允许与 `cf` 一致，或恰为本卡所属家族（`REL-SA-01` 或 `REL-SA-02`）中先序子卡已在其卡内记录 SHA-256 的草稿（逐字节一致）；该规则同样适用于同一家族的后序子卡与家族发布点；其它路径的无关改动保留不动且不得暂存。
3. **独立发布卡网站交付（在本卡 ER-04 C 组第 ⑨ 步按 ER-08 完成 CLI 发布之后执行；本卡涉及多个网站页面时按页分流：a、b 对全部页面执行一次；c 对每个页面分别判断是否漂移——全部页面无漂移时整卡按 c 收口；否则无漂移页面只执行 c 的替代核对（该页现有字节纳入 e 的独立 Claude Code 评审，D5 以该页覆盖本卡行为的现有句子核对），有改动页面执行 e 的标记句零命中（各页选定各自本次新增的标记句、逐页取 `PAGE`）与 h 的逐页部署核对；d 对整个检出执行一次；e 的独立 Claude Code 评审一次覆盖全部页面的最终字节（评审输入逐页写明 SHA-256）；f 只暂存有改动页面并形成一个提交，g 推送一次；任一页面任一步不满足即 STOP）：**
   - a. `cd "$LIBRA_BACKEND" && python3 scripts/sync-libra-command-docs.py --libra-root "$LIBRA_ROOT" --write --only <本卡命令名…>`（exit 0）。
   - b. `cd "$LIBRA_BACKEND" && python3 scripts/sync-libra-command-docs.py --libra-root "$LIBRA_ROOT" --only <本卡命令名…>`（默认 check 模式，exit 0）。
   - c. 若 a 之后 `libra status --short -- <本卡网站文件>` 无输出（网站文件与 `cf` 一致）：记录「网站无漂移」，仍执行 d（`pnpm typecheck`、`pnpm build`）与 e 的独立 Claude Code 评审（评审输入为当前网站文件字节及其 SHA-256，确认其已准确覆盖本卡行为），只跳过 e 的标记句零命中检查与 f–h；D5 改为 `PAGE="$(curl -fsS https://libra.tools/en/docs/commands/<cmd>)"`（curl 失败即 STOP）后 `printf '%s' "$PAGE" | rg -F '<覆盖本卡行为的现有句子>'`（exit 0）。否则继续。
   - d. `cd "$LIBRA_BACKEND" && pnpm typecheck` 与 `cd "$LIBRA_BACKEND" && pnpm build`（均 exit 0）。
   - e. 记录待提交网站文件的 `shasum -a 256`；选定一句由本次改动新增的标记句，先 `PAGE="$(curl -fsS https://libra.tools/en/docs/commands/<cmd>)"`（curl 失败即 STOP），再用三分支零命中模板确认 `printf '%s' "$PAGE" | rg -F '<标记句>'` 为零命中；对这些最终字节取得独立 Claude Code 评审 `VERDICT: PASS`（评审输入写明 SHA-256）。
   - f. `libra add <精确网站文件>`，`libra status --short` 的已暂存条目恰为这些文件；`libra commit -S -s -m "docs(<cmd>): sync <行为> (#<issue>)"`；按「验证执行协议」第 7 条校验同一提交；`[ "$(libra rev-parse HEAD^)" = "$BASE" ]`；提交内各文件 SHA-256 等于 e 中 PASS 的字节。
   - g. 推送前 `libra fetch origin cf && [ "$(libra rev-parse origin/cf)" = "$BASE" ]`（远端前进即 STOP 并从第 2 步重新取证）；`libra push origin cf`；随后 `libra ls-remote origin refs/heads/cf` 显示的 SHA 等于 `$COMMIT_SHA`。
   - h. 部署 D5：`PAGE="$(curl -fsS https://libra.tools/en/docs/commands/<cmd>)"`（curl 失败计为暂时性失败）后 `printf '%s' "$PAGE" | rg -F '<标记句>'` exit 0（e 已证明推送前为零命中，从而绑定本次部署）；暂时性失败按 ER-10 退避重试（≤ 5 次、总计 ≤ 30 分钟），仍失败则停在 `remote-pending` 并 STOP。
4. **家族子卡（`SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i`；`SA-09f`、`SA-09b` 无网站变化）：** 执行 1、2 与 3a、3b、3d，在 `$LIBRA_BACKEND` 留下本地未提交草稿并在卡内记录其 SHA-256（同一家族后序子卡与发布点的第 2d 步以此为允许基线）；不执行 3c、3e–3h，不在 `cf` 提交或推送。
5. **家族发布点（`REL-SA-01`、`REL-SA-02`）：** 在家族 CLI 发布之后，对合并后的最终草稿重新执行 2 与 3a–3h；3c 的判断以 `libra status --short -- <家族网站文件>`（草稿相对已核验的 `cf` 基线）为准，只要草稿有改动就必须执行 3d–3h 完整流程；提交只包含家族子卡登记的网站文件。

## 实施顺序

依赖边（G-06，全部指向具体任务卡；与各卡 `Dependencies` 一致）：

- `SA-01 -> SA-01b`
- `SA-01b -> SA-02`
- `SA-02 -> SA-03`
- `SA-03 -> SA-03b`
- `SA-03b -> SA-04`
- `SA-04 -> SA-05c`
- `SA-05c -> SA-05`
- `SA-05 -> SA-06`
- `SA-06 -> SA-07a`
- `SA-07a -> SA-07`
- `SA-07 -> SA-07c`
- `SA-07c -> SA-08a`
- `SA-08a -> SA-08b`
- `SA-08b -> SA-08m`
- `SA-08m -> SA-08g`
- `SA-08g -> SA-08h`
- `SA-08h -> SA-08j`
- `SA-08j -> REL-SA-01`
- `REL-SA-01 -> SA-08`
- `SA-08 -> SA-09f`
- `SA-09f -> SA-09b`
- `SA-09b -> SA-09`
- `SA-09 -> SA-09a`
- `SA-09a -> SA-09e`
- `SA-09e -> SA-09g`
- `SA-09g -> SA-09h`
- `SA-09h -> SA-09i`
- `SA-09i -> REL-SA-02`

全链串行的理由（已撤回卡 `SA-02b`、`SA-05a`、`SA-05b`、`SA-07h`、`SA-07e`、`SA-07f`、`SA-09c`、`SA-09d` 与 R47 撤回的 `SA-01c`、`SA-07d`、`SA-07b`（同轮撤回的 `SA-09a`、`SA-09e` 已于 R49 恢复） 不在链上）：`cherry_pick.rs`、`COMPATIBILITY.md` 与 `tests/command/cherry_pick_test.rs` 被多卡写入（G-10 的 I–I 相交只能以顺序边消解），且发布按 ER-12 串行。

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| `DEP-SA-01` | `incoming` | 跨计划（完成后开工） | `plan-20261117-commit-signing.md#REL-CS-00`（`CS-02`、`CS-02a`） | Commit/Signing 计划执行者 | `REL-CS-00` 已发布且其提交在 `main`：`libra commit` 默认不签名、`tests/command/cherry_pick_test.rs` 的 `CS-02a` 修改已合入 | 对侧任务卡 `REL-CS-00`/`CS-02`/`CS-02a`（按 ID 核对） | 7 日；未满足时 `SA-01` 为 `blocked`，不得开工 |
| `DEP-SA-02` | `incoming` | 跨计划（完成后开工） | `plan-20261117-commit-signing.md#REL-CS-01`（家族发布点；子卡 `CS-03b`、`CS-03c`、`CS-03h`、`CS-03i`、`CS-03k`、`CS-03e`，按发布点 ID 消费；对侧 `DEP-CS-12`） | Commit/Signing 计划执行者 | `REL-CS-01` 已发布且其提交在 `main`：`gpg.format=ssh` 签名、SSH 验签与签名侧 fail-closed 诊断（#787）可用 | 对侧任务卡 `REL-CS-01` 及其六张子卡（按 ID 核对） | 7 日；未满足时 `REL-SA-01` 家族入口 `SA-08a` 即为 `blocked`，整个家族不得开工（家族自首张子卡起持有 `DEP-SA-03` 的仓库与网站窗口，而 `REL-CS-01` 的子卡写 `COMPATIBILITY.md` 与网站文件、需要同一窗口，故家族不得在持有窗口时等待 `REL-CS-01`；消费方 `SA-08g` 同样登记本项） |
| `DEP-SA-03` | `incoming` | 协调审批（同批共享写集串行窗口） | 协调者按 `plan-20261117-global-init-infra.md#DEP-GII-03` 与 `plan-20261117-merge-pull.md#DEP-MP-10` 同一口径分配的单写者串行窗口：①仓库部分——`COMPATIBILITY.md`（全部同批计划共享）及开工当日与本卡写集相交的其它路径；②网站部分——「文档与网站验收命令」第 2–5 步（`pnpm typecheck`/`pnpm build` 改写的被忽略生成产物 `apps/tanstack-app/src/generated/docs-manifest.ts`、`docs-search.ts` 与 `apps/tanstack-app/dist/` 与所有同批计划的网站交付相交） | Coordinator | 适用于全部 29 张活动卡（各卡 `Dependencies` 均列出本项）。本卡开工编辑前已取得所涉部分的窗口，且前序窗口的相交变更已在 `main`（网站为已推送 `cf`）；仓库窗口在本卡（家族子卡为其家族发布点）的提交合入 `main` 后释放；家族的网站窗口自首张子卡准备本地草稿起持有，至家族发布点按第 5 步推送 `cf` 后释放，独立卡的网站窗口在第 3g 步推送后释放；走第 3c 步「网站无漂移」路径时（独立卡或家族发布点），在该步要求的 `pnpm typecheck`、`pnpm build`、绑定字节的独立 Claude Code `PASS` 与线上 D5 替代核对全部通过并记录后释放 | 协调者窗口登记与交集核对结果（记录在本卡 `Current evidence` 与 `Verification` 记录） | 7 日；未取得窗口的卡为 `blocked`，不得编辑相交文件或运行网站步骤，不得并发实现或评审；超时升级 Coordinator |
| `DEP-SA-04` | `incoming` | 跨计划（完成后开工） | `plan-20261117-plumbing-misc.md#PM-10`；`plan-20261117-checkout-switch-restore.md#CSR-31`、`#CSR-32` | 对应计划执行者 | `PM-10` 已完成且其对 `tests/command/mod.rs` 的修改已在 `main`；`CSR-31`、`CSR-32` 已完成，其写集中「仅需同步时」才改的 `tests/INDEX.md` 若有修改已在 `main`，若无需同步则以对侧卡内记录的「无需同步」证据满足 | 对侧任务卡 ID | 7 日；未满足时 `REL-SA-02` 家族入口 `SA-09f` 即为 `blocked`，整个家族不得开工（家族持有 `DEP-SA-03` 窗口时不等待外部计划；`SA-09b` 与消费方 `SA-09` 同样登记本项） |
| `DEP-SA-05` | `incoming` | 已撤回（R47） | 原 `plan-20261117-commit-signing.md#REL-CS-06`（对侧 `DEP-CS-13`）与 `plan-20261117-merge-pull.md#MP-45`：等待对侧链上全部写 `src/command/commit.rs` 的卡合入 | — | 唯一消费卡 `SA-07d` 已于 R47 按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回（`DEFER-SA-21`），本计划不再写 `src/command/commit.rs`；本项不再约束任何卡，编号保留、不复用；`DEFER-SA-21` 重启时由承接计划按对侧当时状态重新登记 | — | — |
| `DEP-SA-06` | `incoming` | 跨计划（完成后开工） | `plan-20261117-log-show.md#LS-23`、`#LS-33`、`#LS-24`、`#LS-25`、`#LS-34`（均为独立发布切片） | Log/Show 计划执行者 | 五卡对 `src/command/show.rs`、`tests/command/show_test.rs` 与 `show` 文档（`docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`）的修改均已随各自发布合入 `main`（以五卡在对侧计划中的 `done` 状态与各自发布 tag 核对） | 对侧任务卡 ID | 7 日；未满足时 `SA-06` 为 `blocked` |
| `DEP-SA-07` | `incoming` | 跨计划（完成后开工） | `issues/1041.md#I1041-06`、`#I1041-07` | Issue #1041 计划执行者 | `I1041-06`（`am.rs`/`cherry_pick.rs`/`commit.rs`/`rebase.rs`/`tests/command/mod.rs`）与 `I1041-07`（`revert.rs`/`tests/command/mod.rs`）已在 `main` | 对侧任务卡 ID | 7 日；未满足时主链入口 `SA-01` 为 `blocked` |
| `DEP-SA-08` | `incoming` | 已撤回（R43） | 原 `plan-20261117-merge-pull.md#MP-30`（`src/internal/pseudo_ref.rs`） | — | 唯一消费方 `SA-05b` 已于 R43 撤回，本计划不再写 `src/internal/pseudo_ref.rs`；本项不再约束任何卡，编号不复用 | — | — |
| `DEP-SA-10` | `incoming` | 外部前置（交付检出） | 执行机器上可用的网站检出 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支；用户 2026-11-18 11:55:09 UTC 确认；`AGENTS.md:135` 的 `/Volumes/Sea/libra-backend` 为已知文档滞后） | 用户 / 执行机器维护者 | 该路径存在，含 `.libra`、无 `.git`，`libra status --short --branch` 首行为 `## cf...origin/cf`（「文档与网站验收命令」第 2 步） | 2026-11-18 现场核对：路径存在、含 `.libra`、无 `.git`，首行为 `## cf...origin/cf`（当前满足；每张卡开工时按第 2 步重新核验） | 等待上限 7 日；开工核验不通过时所有有网站影响的卡（除 `SA-09f`、`SA-09b` 外的全部活动卡）为 `blocked`，立即向用户（Plan Owner）报告；到期仍未满足则升级给 Coordinator 将本计划整体保持 `blocked`，不得改用其它目录或跳过网站门 |
| `DEP-SA-09` | `incoming` | 跨计划（完成后开工） | `issues/672.md#I672-01`；`issues/547.md#FINAL-547`（收口 `AM-01`..`AM-11`） | Issue #672 / #547 计划执行者 | 上述卡对 `src/command/am.rs`、`tests/compat/mail_am_basic_test.rs` 与 `am` 文档的修改已在 `main` | 对侧任务卡 ID | 7 日；未满足时 `REL-SA-02` 家族入口 `SA-09f` 即为 `blocked`（消费方 `SA-09b` 同样登记本项） |

### 发布分组与并发窗口

| 家族 ID | 成员（按顺序） | 唯一发布点 | 窗口规则 | 失败回滚顺序 | 理由 |
|---|---|---|---|---|---|
| `REL-SA-01` | `SA-08a`（1/6）、`SA-08b`（2/6）、`SA-08m`（3/6）、`SA-08g`（4/6）、`SA-08h`（5/6）、`SA-08j`（6/6；原 `SA-08k` 已于 R37 并入；`SA-08i`、`SA-08l` 已于 R38 撤回） | `REL-SA-01` | 家族的全部外部依赖（`DEP-SA-02`）须在首张子卡 `SA-08a` 开工前满足，家族持有窗口期间不等待外部计划；子卡各自通过 A/B 门与 ER-05 评审后只做本地签名+DCO 提交，不 bump、不推送、不发布；网站只留本地草稿（「文档与网站验收命令」第 4 步）；窗口期禁止插入其它切片，`DEP-SA-03` 的仓库与网站窗口覆盖六张子卡与发布点；`REL-SA-01` 在包含六张子卡最终变更的树上 `patch + 1`、跑 `T-2` 全量后发布，再按第 5 步发布网站 | 推送前按 `SA-08j` → `SA-08h` → `SA-08g` → `SA-08m` → `SA-08b` → `SA-08a` 逆序在本地 `libra revert`，网站丢弃本次未提交草稿；推送后按 `immutable-release` 前滚 | rebase 生成提交的全部路径只有都经单一签名出口、且重放步的签名失败可恢复（重放后改写路径的二次签名失败恢复登记 `DEFER-SA-08`），才满足 `ADR-SA-03`，部分收口的版本不得对外发布（G-02/G-08） |
| `REL-SA-02` | `SA-09f`（1/8）、`SA-09b`（2/8）、`SA-09`（3/8）、`SA-09a`（4/8）、`SA-09e`（5/8）、`SA-09g`（6/8）、`SA-09h`（7/8；R51 新增）、`SA-09i`（8/8；R52 新增）；`SA-09c`、`SA-09d` 已于 R43 撤回；`SA-09a`、`SA-09e` 曾于 R47 撤回、R49 恢复 | `REL-SA-02` | 家族的全部外部依赖（`DEP-SA-04`、`DEP-SA-09`）须在首张子卡 `SA-09f` 开工前满足，家族持有窗口期间不等待外部计划；子卡各自通过 A/B 门与 ER-05 评审后只做本地签名+DCO 提交，不 bump、不推送、不发布；网站只留本地草稿（「文档与网站验收命令」第 4 步）；窗口期禁止插入其它切片，`DEP-SA-03` 的仓库窗口覆盖写入相交文件的子卡、网站窗口覆盖有网站草稿的子卡与发布点；`REL-SA-02` 在包含八张子卡最终变更的树上 `patch + 1`、跑 `T-2` 全量后发布，再按第 5 步发布网站 | 推送前按 `SA-09i` → `SA-09h` → `SA-09g` → `SA-09e` → `SA-09a` → `SA-09` → `SA-09b` → `SA-09f` 逆序在本地 `libra revert`，网站丢弃本次未提交草稿；推送后按 `immutable-release` 前滚 | `SA-09f` 的编解码函数在家族外没有消费者，`SA-09b` 的字段接线在 `SA-09` 移除拒绝前不可达，`SA-09` 使 unborn 起点暂停可达而其 `--abort` 复原、中止保护、中断后重试与并发保护由 `SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 交付；八者只能作为一个整体对外发布（G-02/G-08） |

其余 13 张实现卡（`SA-01`、`SA-01b`、`SA-02`、`SA-03`、`SA-03b`、`SA-04`、`SA-05c`、`SA-05`、`SA-06`、`SA-07a`、`SA-07`、`SA-07c`、`SA-08`）各自为独立发布切片（G-07、`Release boundary=independent`），每张完成后按 ER-08 `patch + 1` 并用 `gh release create` 发布；`REL-SA-01`、`REL-SA-02` 为家族发布点（全计划 29 张活动卡 = 27 张实现卡 + 2 张 `release` 卡；`SA-08k` 已撤回并入 `SA-08j`，`SA-08i`、`SA-08l` 已撤回，R43 撤回 `SA-02b`、`SA-05a`、`SA-05b`、`SA-07h`、`SA-07e`、`SA-07f`、`SA-09c`、`SA-09d`，R47 撤回 `SA-01c`、`SA-07d`、`SA-07b`（同轮撤回的 `SA-09a`、`SA-09e` 已于 R49 恢复），编号均不复用）。

**D 组登记表（独立发布卡与 `REL-SA-01`、`REL-SA-02` 共用；不计入 G-03；`VERSION` 按模板 ER-04 C 组第 ⑨ 步从 bump 后的 `Cargo.toml` 读取）：**

| D 项 | workflow / job | 触发事件与 ref | 判据 |
|---|---|---|---|
| D1 | `base.yml`：`format`、`clippy`、`redundancy`、`test` | `pull_request`（本卡 PR head SHA） | 同一 head SHA 全部 job 成功 |
| D2 | `codeql.yml`：`analyze` | `pull_request`（同一 head SHA） | 成功 |
| D3 | `release.yml`：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` | `gh release create v<version> --target <merge SHA>` 推出的 tag `v*` 的 `push` | 全部 job 成功；`gh release view "v${VERSION}" --json tagName --jq .tagName` 输出 `v${VERSION}`；`gh release view "v${VERSION}" --json body --jq .body` 输出非空 |
| D4 | 发布产物可获取性（R2/CDN） | D3 成功之后 | `curl -fsSIL "https://download.libra.tools/libra/releases/v${VERSION}/libra-linux-amd64"` 与 `…/libra-darwin-arm64` 均 exit 0（`VERSION` 按模板 ER-04 C 组第 ⑨ 步的取值命令从 bump 后的 `Cargo.toml` 读取；路径格式见 `install.sh:14,1171-1178`） |
| D5 | 网站部署（只适用于有网站影响的卡；R43 起全部独立发布卡与两个家族发布点均有网站影响，无网站变化的 `SA-09f`、`SA-09b` 是家族子卡，D 组由 `REL-SA-02` 承担） | 「文档与网站验收命令」第 3g 步推送 `cf` | 第 3h 步通过（或第 3c 步的无漂移核对） |
| D6 | 合入树一致（远端记录） | PR 合入 `main` 之后、`gh release create` 之前（「验证执行协议」第 10 条的发布前门） | 记录第 10 条比较所用的 `MERGE_SHA`、两侧树 OID 与结论；树不一致时还记录在 merge 树上按第 10 条 ①–④ 重跑的全部门结果与重新评审的 `PASS`（均绑定同一 `MERGE_SHA`），并确认 Release 指向的 `--target` 等于该 `MERGE_SHA` |

job 集合以执行当日按模板 ER-04「仓库配置与 CI 展开规则」现场提取为准（上表为 2026-11-18 核对快照）。

**并发声明:** 全链严格串行（G-10）；实现与评审阶段也不并发。
**发布者:** Coordinator 指定的单一发布者（ER-12），负责 C 组 bump/构建/安装/提交/推送/`gh release create`、网站推送与 D 组跟踪。
**发布窗口顺序:** `SA-01` → `SA-01b` → `SA-02` → `SA-03` → `SA-03b` → `SA-04` → `SA-05c` → `SA-05` → `SA-06` → `SA-07a` → `SA-07` → `SA-07c` → `REL-SA-01`（含 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j`）→ `SA-08` → `REL-SA-02`（含 `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i`）。同一时刻只允许一张卡处于「已 bump 未完成推送/未触发 release」。

### Phase 0: `cherry-pick` 序列器、范围与空提交（`SA-01`、`SA-01b`、`SA-02`、`SA-03`、`SA-03b`、`SA-04`、`SA-05c`、`SA-05`、`SA-06`）

**目标:** 空 cherry-pick 停止态、`A..B` 范围、rerere 标志、`--ff` root、unborn 分支上的 `--ff` 快进（及其已暂存条目保护）与 `show --pretty=format:%B` 尾行。

**进入条件:**

- `DEP-SA-01`、`DEP-SA-03`（首个窗口）、`DEP-SA-07`、`DEP-SA-10` 已满足（`SA-06` 另需 `DEP-SA-06`；Phase 0 的卡均有网站影响，均受 `DEP-SA-10` 约束）。

**退出条件:**

- `SA-01`、`SA-01b`、`SA-02`、`SA-03`、`SA-03b`、`SA-04`、`SA-05c`、`SA-05`、`SA-06` 全部 `done` 且 `complete`（已撤回的 `SA-01c`、`SA-02b`、`SA-05a`、`SA-05b` 不在退出条件内）。

### Phase 1: `revert`、`rebase` 与 `am`（`SA-07a`..`REL-SA-02`）

**目标:** `revert` 已暂存改动拒绝与 detached HEAD、`cherry-pick -`、`rebase` 签名重放与 `-f`、unborn 分支上的 `am` 及其暂停后的 `--abort` 复原、中止保护、中断后重试与并发保护。

**进入条件:**

- `SA-06` 为 `done`/`complete`（`REL-SA-01` 家族入口 `SA-08a` 另需 `DEP-SA-02`；`REL-SA-02` 家族入口 `SA-09f` 另需 `DEP-SA-04`、`DEP-SA-09`）。

**退出条件:**

- `SA-07a`、`SA-07`、`SA-07c`、`SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j`、`REL-SA-01`、`SA-08`、`SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i`、`REL-SA-02` 全部 `done` 且 `complete`（已撤回的 `SA-07h`、`SA-07e`、`SA-07f`、`SA-07d`、`SA-07b`、`SA-09c`、`SA-09d` 不在退出条件内），且「完成判据」全量收口门通过。

## 任务卡

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（已发布版本不可撤回，修复以新版本前滚；降级指引：通过上一已知良好 tag 重新安装，兼容窗口为相邻 1 个 patch 版本内不改变仓内对象与配置格式）
- **Migration and rollback 默认:** `N/A：无 SQLite schema 迁移；降级见 Rollback mode`
- **Security and privacy 默认:** `继承 GC-07、GC-11`
- **Performance budget 默认:** `继承 GC-10`
- **Release write set 默认:** `Inherited`（ER-08 版本面集合 + 工具链刷新的 `Cargo.lock` + release artifact）

**默认覆盖**（取非默认值，无需审批）：

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| `SA-09f`、`SA-09b` | `Docs and compatibility impact` | 只改开发文档；用户文档、`COMPATIBILITY.md` 与网站为 `N/A`（无用户可见变化） |
| `SA-02` | `Performance budget` | 范围展开每个提交至多加载一次，计数型单元测试验证 |
| `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` | `Rollback mode`、`Release boundary`、`Version increment`、`Release write set`、`C/D coverage from` | `revert`（家族窗口内本地撤销）；`family child of REL-SA-01`；`N/A`；`N/A`；`REL-SA-01` |
| `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` | `Security and privacy` | 签名只经单一出口；签名失败时出口不写提交对象；经 `create_replayed_commit` 构造提交的重放步按冲突同构停止且不移动分支或标签引用（重放后改写路径的二次签名失败见 `DEFER-SA-08`）；测试 failpoint 只在设置了 `LIBRA_TEST` 的测试进程中生效；证据不记录私钥、口令或签名材料 |
| `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` | `Rollback mode`、`Release boundary`、`Version increment`、`Release write set`、`C/D coverage from`、`Migration and rollback` | `revert`（家族窗口内本地撤销）；`family child of REL-SA-02`；`N/A`；`N/A`；`REL-SA-02`；unborn 起点的暂停 `am` 须先 `libra am --abort` 回到 unborn 再降级（其 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`） |
| `REL-SA-01` | `Task type`、`Release boundary` | `release`；`family release point of REL-SA-01` |
| `REL-SA-02` | `Task type`、`Release boundary`、`Migration and rollback` | `release`；`family release point of REL-SA-02`；unborn 起点的暂停 `am` 须先 `libra am --abort` 回到 unborn 再降级 |

**规则 waiver（`EX-*`，具名审批；按 `plan-template.md` 白名单「G-03 条目上限（门族型验收）」）：** 只有 `EX-SA-01` 一项。R22 取消的 `EX-SA-02-RANGE`（无具名审批）保留为历史记录，不再有效。其余卡的 AC/VER 均在 G-03 上限内（按「验证执行协议」第 4 条逐条计数），全部 `Estimated scope` 为 `S`/`M`，ER-07 签名要求不豁免。

| 例外 ID | 任务 | 豁免项 | 理由与补偿措施 | Approver | Review round | 证据 | 有效期 |
|---|---|---|---|---|---|---|---|
| `EX-SA-01` | `SA-08j` | G-03 条目上限（门族型验收；只豁免 AC 列，`SA-08j` 的 VER 为 6/8 未超限） | 同一恢复轴「`rebase` 重放中途签名失败 → 冲突同构停止 → `--continue`/`--abort` 恢复」上的机械门族：多步 `pick` 与 `squash`/`fixup` 折叠两类重放，各自的停止态（`stopped_sha`、HEAD、全量索引树、工作区、分支与标签引用）、失败重试后的全量停止态（HEAD、引用、索引、工作区与 `rebase_state` 行）、中止起点、`--continue` 后的完整提交序列（父链、树、作者、消息、有效签名）与最终 HEAD、索引、工作区（含无关未跟踪文件保留）与 `--abort` 后的全量快照复原，以及 GC-04 错误类「rebase 重放签名失败停止」代表路径（门 G1）的退出码、人读输出、`--json`、`--machine` 与其余停止路径的稳定码 `LBR-AUTH-001`（「验证执行协议」第 11 条），共 6 门 112 个判据（另含真实签名不可用与 failpoint 惰性两门）；按门拆卡会把同一恢复路径的实现与验收拆散（违反 G-01/G-02 与「碎片卡」反模式；R33–R36 为守 8 条上限拆出 `SA-08j`/`SA-08k` 即造成先后序倒置与覆盖缺口）。准入条件：①每门是一条可复制执行的具名命令（`cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::<门用例>)'`），任一门失败即整卡不达标；②门族在 `SA-08j`「判据规范（非计数正文）— `EX-SA-01`」块逐门枚举（门、判定的 AC 区间与命令）；③分子如实写作 `112/8@EX-SA-01`（卡内 `Granularity` 与粒度审计表一致）；④门族或其 AC 增减时同批更新本行、`SA-08j` 与粒度审计表。本豁免只覆盖 G-03 计数，不豁免 G-01、G-02、G-05..G-11 | 用户（本计划请求者） | R37（批准后首轮） | 用户 2026-11-19 03:12:32 UTC 在协调会话中批准：对 #780 的 rebase 签名家族（`REL-SA-01`）恢复卡授予 G-03 门族型豁免 | 本计划内 |

#### 任务卡粒度审计表

已撤回卡（`SA-02b`、`SA-05a`、`SA-05b`、`SA-07h`、`SA-07e`、`SA-07f`、`SA-09c`、`SA-09d`，R47 撤回的 `SA-01c`、`SA-07d`、`SA-07b`（同轮撤回的 `SA-09a`、`SA-09e` 已于 R49 恢复），以及更早撤回的 `SA-08c`..`SA-08f`、`SA-08i`、`SA-08k`、`SA-08l`）不列入本表。AC 列按「验证执行协议」第 4 条计数；VER 列只计卡特有用例与卡特有手工证据；共享回归组（验证执行协议第 2 条）、ER-04 强制门、ER-06/ER-06a 文档门与 D 组不计入。

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| SA-01 | implementation | 空cherry-pick停止态 | 前滚新版本，降级按默认指引 | yes | yes | 7/8 | 1/8 | 1/1 | M | DEP-SA-01, DEP-SA-03, DEP-SA-07, DEP-SA-10 | 序列化于主链入口 | independent | N/A | N/A |
| SA-01b | implementation | keep-redundant与控制选项互斥 | 前滚新版本，降级按默认指引 | yes | yes | 8/8 | 1/8 | 1/1 | M | SA-01, DEP-SA-03, DEP-SA-10 | 序列化于 SA-01 | independent | SA-01 | N/A |
| SA-02 | implementation | cherry-pick提交范围展开 | 前滚新版本，降级按默认指引 | yes | yes | 8/8 | 3/8 | 1/1 | M | SA-01b, DEP-SA-03, DEP-SA-10 | 序列化于 SA-01b | independent | N/A | N/A |
| SA-03 | implementation | cherry-pick范围rerere与continue互斥 | 前滚新版本，降级按默认指引 | yes | yes | 8/8 | 2/8 | 1/1 | M | SA-02, DEP-SA-03, DEP-SA-10 | 序列化于 SA-02 | independent | N/A | N/A |
| SA-03b | implementation | cherry-pick的rerere标志可重复 | 前滚新版本，降级按默认指引 | yes | yes | 8/8 | 1/8 | 1/1 | M | SA-03, DEP-SA-03, DEP-SA-10 | 序列化于 SA-03 | independent | SA-03 | N/A |
| SA-04 | implementation | cherry-pick--ff对root-commit的OID对齐 | 前滚新版本，降级按默认指引 | yes | yes | 2/8 | 2/8 | 1/1 | M | SA-03b, DEP-SA-01, DEP-SA-03, DEP-SA-10 | 序列化于 SA-03b | independent | N/A | N/A |
| SA-05c | implementation | unborn分支cherry-pick快进的已暂存条目保护 | 前滚新版本，降级按默认指引 | yes | yes | 6/8 | 1/8 | 1/1 | M | SA-04, DEP-SA-03, DEP-SA-10 | 序列化于 SA-04 | independent | SA-05 | N/A |
| SA-05 | implementation | unborn分支上cherry-pick-ff快进root | 前滚新版本，降级按默认指引 | yes | yes | 8/8 | 2/8 | 1/1 | M | SA-05c, DEP-SA-03, DEP-SA-10 | 序列化于 SA-05c | independent | N/A | N/A |
| SA-06 | implementation | show自定义format结尾换行语义 | 前滚新版本，降级按默认指引 | yes | yes | 3/8 | 1/8 | 1/1 | M | SA-05, DEP-SA-03, DEP-SA-06, DEP-SA-10 | 序列化于 SA-05 | independent | N/A | N/A |
| SA-07a | implementation | revert已暂存改动前置拒绝 | 前滚新版本，降级按默认指引 | yes | yes | 8/8 | 1/8 | 1/1 | M | SA-06, DEP-SA-03, DEP-SA-10 | 序列化于 SA-06 | independent | SA-07 | N/A |
| SA-07 | implementation | detached-HEAD上的revert | 前滚新版本，降级按默认指引 | yes | yes | 8/8 | 3/8 | 1/1 | M | SA-07a, DEP-SA-03, DEP-SA-10 | 序列化于 SA-07a | independent | N/A | N/A |
| SA-07c | implementation | cherry-pick上一检出简写 | 前滚新版本，降级按默认指引 | yes | yes | 6/8 | 2/8 | 1/1 | M | SA-07, DEP-SA-03, DEP-SA-10 | 序列化于 SA-07 | independent | SA-07 | N/A |
| SA-08a | implementation | rebase签名出口与pick签名 | 家族窗口内本地revert，推送后前滚 | yes | yes | 7/8 | 3/8 | 1/1 | M | SA-07c, DEP-SA-02, DEP-SA-03, DEP-SA-10 | 序列化于 SA-07c | family child | SA-08 | N/A |
| SA-08b | implementation | rebase的reword/edit/fixup/squash签名出口 | 家族窗口内本地revert，推送后前滚 | yes | yes | 8/8 | 4/8 | 1/1 | M | SA-08a, DEP-SA-03, DEP-SA-10 | 序列化于 SA-08a | family child | SA-08a | N/A |
| SA-08m | implementation | rebase的amend/fixup-C/fixup-c签名出口 | 家族窗口内本地revert，推送后前滚 | yes | yes | 6/8 | 2/8 | 1/1 | M | SA-08b, DEP-SA-03, DEP-SA-10 | 序列化于 SA-08b | family child | SA-08b | N/A |
| SA-08g | implementation | rebase重放SSH格式签名 | 家族窗口内本地revert，推送后前滚 | yes | yes | 2/8 | 2/8 | 1/1 | M | SA-08m, DEP-SA-02, DEP-SA-03, DEP-SA-10 | 序列化于 SA-08m | family child | SA-08a | N/A |
| SA-08h | implementation | rebase重放后改写先于签名 | 家族窗口内本地revert，推送后前滚 | yes | yes | 7/8 | 4/8 | 1/1 | M | SA-08g, DEP-SA-03, DEP-SA-10 | 序列化于 SA-08g | family child | SA-08a | N/A |
| SA-08j | implementation | rebase签名失败的停止与恢复 | 家族窗口内本地revert，推送后前滚 | yes | yes | 112/8@EX-SA-01 | 6/8 | 1/1 | M | SA-08h, DEP-SA-03, DEP-SA-10 | 序列化于 SA-08h | family child | SA-08a | EX-SA-01 |
| REL-SA-01 | release | 发布rebase重放签名出口家族版本 | 前滚新版本 | yes | yes | 9/12 | 9/12 | 0/0 | S | SA-08a, SA-08b, SA-08m, SA-08g, SA-08h, SA-08j, DEP-SA-03, DEP-SA-10 | 序列化于 SA-08j | family release point | N/A | N/A |
| SA-08 | implementation | rebase强制重放 | 前滚新版本，降级按默认指引 | yes | yes | 8/8 | 2/8 | 1/1 | M | REL-SA-01, DEP-SA-03, DEP-SA-10 | 序列化于 REL-SA-01 | independent | N/A | N/A |
| SA-09f | implementation | sequence_state可选OID列全零编解码 | 家族窗口内本地revert，推送后前滚 | yes | yes | 8/8 | 1/8 | 1/1 | M | SA-08, DEP-SA-03, DEP-SA-04, DEP-SA-09 | 序列化于 SA-08 | family child | SA-09 | N/A |
| SA-09b | implementation | am状态字段接入全零编解码 | 家族窗口内本地revert，推送后前滚 | yes | yes | 8/8 | 1/8 | 1/1 | M | SA-09f, DEP-SA-03, DEP-SA-04, DEP-SA-09 | 序列化于 SA-09f | family child | SA-09 | N/A |
| SA-09 | implementation | am在unborn分支应用补丁序列 | 家族窗口内本地revert，推送后前滚 | yes | yes | 8/8 | 2/8 | 1/1 | M | SA-09b, DEP-SA-03, DEP-SA-04, DEP-SA-10 | 序列化于 SA-09b | family child | N/A | N/A |
| SA-09a | implementation | unborn起点第二封暂停后abort回到unborn | 家族窗口内本地revert，推送后前滚 | yes | yes | 7/8 | 1/8 | 1/1 | M | SA-09, DEP-SA-03, DEP-SA-10 | 序列化于 SA-09 | family child | SA-09 | N/A |
| SA-09e | implementation | unborn起点首封暂停后abort | 家族窗口内本地revert，推送后前滚 | yes | yes | 7/8 | 1/8 | 1/1 | M | SA-09a, DEP-SA-03, DEP-SA-10 | 序列化于 SA-09a | family child | SA-09a | N/A |
| SA-09g | implementation | unborn起点am会话的中止保护 | 家族窗口内本地revert，推送后前滚 | yes | yes | 8/8 | 1/8 | 1/1 | M | SA-09e, DEP-SA-03, DEP-SA-10 | 序列化于 SA-09e | family child | SA-09a | N/A |
| SA-09h | implementation | unborn起点am中止的原子化与重试 | 家族窗口内本地revert，推送后前滚 | yes | yes | 8/8 | 1/8 | 1/1 | M | SA-09g, DEP-SA-03, DEP-SA-10 | 序列化于 SA-09g | family child | SA-09a | N/A |
| SA-09i | implementation | unborn起点am中止的引用级并发保护 | 家族窗口内本地revert，推送后前滚 | yes | yes | 7/8 | 2/8 | 1/1 | M | SA-09h, DEP-SA-03, DEP-SA-10 | 序列化于 SA-09h | family child | SA-09a | N/A |
| REL-SA-02 | release | 发布unborn-am家族版本 | 前滚新版本，unborn起点am暂停态先abort再降级 | yes | yes | 9/12 | 9/12 | 0/0 | S | SA-09f, SA-09b, SA-09, SA-09a, SA-09e, SA-09g, SA-09h, SA-09i, DEP-SA-03, DEP-SA-10 | 序列化于 SA-09i | family release point | N/A | N/A |

---

### Task SA-01: 空 `cherry-pick` 停止提示与停止态保持（#725）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：`--empty=stop`（默认）的空重放改为**可恢复停止**——错误文案改为 Git 的 `The previous cherry-pick is now empty …`、稳定码 `LBR-CONFLICT-001`、命令级退出码 1；`RedundantCommit` 分支在 `save`/`claim_start` 持久化 `CherryPickState`（`current_oid` = 被拣选提交、`todo` 为剩余项），`--abort` 走既有硬重置路径清除状态；既有 `cherry_pick_redundant_blocked_then_kept` 与 `test_cherry_pick_empty_modes` 期望同卡更新（stop 后先 `--abort`）。AC1–AC7 由 `command::cherry_pick_test::cherry_pick_empty_stop_message_exit_and_state` 覆盖（人读 stderr、exit 1、`--json`/`--machine` 信封、`sequence_state.current_oid`、`--abort` 后无行）；`cherry_pick_test` 78/78、`--lib cherry_pick` 18/18；cherry-pick EN/zh、`docs/error-codes.md`、开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 重放结果为空/冗余（`RedundantCommit` 路径）且有效 `--empty` 模式为 `stop`（默认；显式 `--empty=stop` 经 `effective_empty_mode` 归为同一 `EmptyMode::Stop`，见 `src/command/cherry_pick.rs:597-617`）时，按 Git 输出 `The previous cherry-pick is now empty`，按 `ADR-SA-01` 作为可恢复停止态保留 sequencer（停止的提交记于 `current_oid`），并以 `LBR-CONFLICT-001` 与命令级退出码 1（`CliError::with_exit_code(1)`，对齐 Git 的 1）结束；该停止态可 `--abort` 收口（#725 验收项 1、2、4）。`--abort` 的复原机制（硬重置回原 HEAD）沿用既有路径，由共享回归组中的既有用例 `cherry_pick_abort_restores_head`（`tests/command/cherry_pick_test.rs:2192`）守护。R43 按用户 2026-11-19 05:17:51 UTC 决定收窄到 #725 的验收项：显式 `--empty=stop`（`t3505#15`、`TC-0522`）的专项验收登记 `DEFER-SA-15`，多提交空停止的 `--skip`/`--continue`/`--quit` 专项验收登记 `DEFER-SA-11`。同卡更新断言旧 exit 129 的 `cherry_pick_redundant_blocked_then_kept`。已拆出 `SA-01b`（`--keep-redundant-commits` 与控制选项互斥）；原拆出的 `SA-01c`（`--empty` 与控制选项互斥）已于 R47 撤回（`DEFER-SA-15`）。
**Out of scope:** `--keep-redundant-commits` 与控制选项互斥（由 `SA-01b` 承接）；`--empty` 与控制选项互斥（原 `SA-01c`，`DEFER-SA-15`）；`A..B` 范围（由 `SA-02` 承接）；`--empty=drop`/`keep` 之后 `show` 的尾行（由 `SA-06` 承接）；原本即为空的提交（`EmptyCommit`/`--allow-empty` 路径）保持现状（永久非目标：#725 只涉及重放后变空）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 冗余停止返回 `RedundantCommit`，映射 `CliInvalidArguments`（exit 129），不输出 Git 提示、不保留状态 | `src/command/cherry_pick.rs:127-128,218,274-276,2379-2384` |
| 既有测试断言冗余提交 exit 129 | `tests/command/cherry_pick_test.rs:1600-1644` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture：已提交仓库中运行 `libra cherry-pick HEAD`（`t3505#13/#14` 第 1 步，#725 的复现命令）；「默认运行」指不加 `--json`/`--machine` 的 `libra cherry-pick HEAD`）

- [ ] AC1：默认运行的人读 stderr含 `The previous cherry-pick is now empty`（代表路径的人读输出）。
- [ ] AC2：默认运行的退出码为 1（代表路径的退出码）。
- [ ] AC3：代表路径的 `--json`：在同一 fixture 的独立副本上运行 `libra --json cherry-pick HEAD`，结果（进程退出状态，stderr 整体解析出的 JSON 对象的（`ok`、`error_code`、`exit_code`）三元组）等于（1，（`false`、`LBR-CONFLICT-001`、1））（一次比较；stderr 不能整体解析为单个 JSON 对象即不成立）。
- [ ] AC4：代表路径的 `--machine`：在另一份独立副本上运行 `libra --machine cherry-pick HEAD`，结果（进程退出状态，stderr 按行解析出的（`ok`、`error_code`、`exit_code`）三元组序列）等于（1，[（`false`、`LBR-CONFLICT-001`、1）]）（一次比较；任一行不能解析为 JSON 对象或行数不为 1 即不成立）。
- [ ] AC5：默认运行之后 `sequence_state` 中 `kind='cherry_pick'` 行的 `current_oid` 等于被拣选提交（可恢复停止态）。
- [ ] AC6：默认运行的停止态下 `libra cherry-pick --abort` exit 0。
- [ ] AC7：AC6 之后无 `sequence_state` 行。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。另含第 1e 步：`docs/error-codes.md` 在 `LBR-CONFLICT-001` 下写明 `cherry-pick` 空停止的命令级退出码 1。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡改变 1 个错误类：E1「空 cherry-pick 停止」——`RedundantCommit` 路径在有效 `--empty` 模式为 `stop` 时由原 `LBR-CLI-002`/退出码 129 的拒绝改为 `LBR-CONFLICT-001`、命令级退出码 1（`CliError::with_exit_code(1)`）、诊断 `The previous cherry-pick is now empty` 的可恢复停止。代表路径：fixture 的 `libra cherry-pick HEAD`；四项判据：退出码 AC2、人读输出 AC1、`--json` AC3、`--machine` AC4。其余路径：无（显式 `--empty=stop` 归入同一类，其专项验收登记 `DEFER-SA-15`）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_empty_stop_message_exit_and_state)'`（new；AC1–AC7）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::cherry_pick_test::/)'`（含更新后的 `cherry_pick_redundant_blocked_then_kept`）；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::cherry_pick::/)'`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）；新增 LBR-CONFLICT-001 命令级退出码特例并修改 docs/error-codes.md`
**Dependencies:** `DEP-SA-01`, `DEP-SA-03`, `DEP-SA-07`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cherry_pick.rs`, `tests/command/cherry_pick_test.rs`, `docs/error-codes.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/error-codes.md`（`LBR-CONFLICT-001` 的 `cherry-pick` 退出码 1 特例）、`docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`（空停止提示与停止态）、`COMPATIBILITY.md`（`cherry-pick` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=空cherry-pick停止态; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=DEP-SA-01,DEP-SA-03,DEP-SA-07,DEP-SA-10; writeset=序列化于主链入口; release=independent; split-from=N/A; exception=N/A`

---

### Task SA-01b: `--keep-redundant-commits` 与 sequencer 控制选项互斥（#725，`t3505#13`）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：四个控制选项的 `conflicts_with_all` 增加 `keep_redundant_commits`；clap 冲突经既有 `classify_parse_error` 映射为 `LBR-CLI-002`，人读诊断行同时点名 `'--keep-redundant-commits'` 与对应控制选项（`cannot be used with`），命令体不执行。AC1–AC8 由 `command::cherry_pick_test::cherry_pick_keep_redundant_rejected_with_controls_tc0426` 覆盖（在空停止态上逐一对 `--continue`/`--skip`/`--abort`/`--quit` 断言稳定码与诊断文本）；`cherry_pick_test` 80/80；cherry-pick EN/zh、开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 自 `SA-01` 拆出：`--keep-redundant-commits` 与 `--continue`、`--skip`、`--abort`、`--quit` 任一同用时，在参数解析阶段以 usage 错误拒绝（clap 冲突约束，稳定错误码 `LBR-CLI-002`，命令体不执行；Git 以 `fatal: cherry-pick: --keep-redundant-commits cannot be used with <op>` 失败）。#725 验收项 3 要求「以对应的 `cannot be used with` 提示失败」，故本卡对每个控制选项分别验收稳定错误码 `LBR-CLI-002` 与点名这两个选项的冲突诊断（clap 参数冲突经 `classify_parse_error` 映射为 `CliError::command_usage`，`src/cli.rs:3176-3221`，属既有错误类，见「错误类清单」）。`--empty` 的同类互斥（`t3505#14`，原 `SA-01c`）已于 R47 撤回并登记 `DEFER-SA-15`。
**Out of scope:** 空停止本身（由 `SA-01` 承接）；`--empty` 与控制选项互斥（原 `SA-01c`，R47 撤回，`DEFER-SA-15`）；rerere 标志与控制选项互斥（`--continue` 由 `SA-03` 承接，其余见 `DEFER-SA-06`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 控制选项的 `conflicts_with_all` 不含 `keep_redundant_commits` | `src/command/cherry_pick.rs:689-716,750-753` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；起点均为：已提交仓库中运行 `libra cherry-pick HEAD` 形成的空停止态；「点名该选项对的冲突诊断」指人读 stderr中有一行同时含 `cannot be used with`、`'--keep-redundant-commits'` 与该控制选项带引号的完整名（如 `'--continue'`））

- [ ] AC1：`libra cherry-pick --keep-redundant-commits --continue`（`TC-0426`，`t3505#13`）的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CLI-002`。
- [ ] AC2：AC1 的人读 stderr含点名该选项对（`--keep-redundant-commits` 与 `--continue`）的冲突诊断。
- [ ] AC3：`libra cherry-pick --keep-redundant-commits --skip` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CLI-002`。
- [ ] AC4：AC3 的人读 stderr含点名该选项对（`--keep-redundant-commits` 与 `--skip`）的冲突诊断。
- [ ] AC5：`libra cherry-pick --keep-redundant-commits --abort` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CLI-002`。
- [ ] AC6：AC5 的人读 stderr含点名该选项对（`--keep-redundant-commits` 与 `--abort`）的冲突诊断。
- [ ] AC7：`libra cherry-pick --keep-redundant-commits --quit` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CLI-002`。
- [ ] AC8：AC7 的人读 stderr含点名该选项对（`--keep-redundant-commits` 与 `--quit`）的冲突诊断。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类。四条路径（`--keep-redundant-commits` 分别与 `--continue`、`--skip`、`--abort`、`--quit`）命中既有错误类「clap 参数冲突」（`LBR-CLI-002`，退出码 129，`classify_parse_error` → `CliError::command_usage`，`src/cli.rs:3176-3221`；本卡只新增冲突约束，不改变该类的稳定码、退出码与诊断模板），按「验证执行协议」第 11c 条作为其余路径：各断言稳定码（AC1、AC3、AC5、AC7）；#725 验收项 3 明确要求每条路径以对应的 `cannot be used with` 提示失败，故各加 1 条人读诊断（AC2、AC4、AC6、AC8）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_keep_redundant_rejected_with_controls_tc0426)'`（new；AC1–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::cherry_pick_test::/)'`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::cherry_pick::/)'`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `SA-01`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cherry_pick.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`（`--keep-redundant-commits` 与控制选项互斥）、`COMPATIBILITY.md`（`cherry-pick` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=keep-redundant与控制选项互斥; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-01,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-01; release=independent; split-from=SA-01; exception=N/A`

---

### Task SA-01c: 已撤回（不得开工）

**Status:** withdrawn。R47 按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回（原：`--empty` 与 `--continue`/`--skip`/`--abort`/`--quit` 互斥，`t3505#14`、`TC-0427`）。#725 的验收项只涉及 `--keep-redundant-commits`（`t3505#13`，`SA-01b`），`--empty` 的同类互斥不属所跟踪 issue 的验收项，登记 `DEFER-SA-15`。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-02: `cherry-pick` 解析 `A..B` 提交范围（#726）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：提交解析支持 `<A>..<B>`——`expand_revision_range` 收集 `A` 的祖先（含 `A`）作为排除集，再从 `B` 沿父链收集并反转（线性历史等价 `rev-list --reverse`）；展开后的提交各自带标签（`labels`），三处 `args.commits[i]` 取标签改为 `labels[i]`，范围中途停止不再越界/panic。AC1–AC5 由 `cherry_pick_range_conflict_stop_then_continue` 覆盖（中途冲突、continue 落提交、主题序列 B/C、解决树一致、状态释放），AC6–AC8 由 `cherry_pick_range_later_untracked_overwrite_stops_cleanly` 覆盖（未跟踪覆盖、`u.txt` 字节不变、`current_oid=C`）；`cherry_pick_test` 82/82；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 在 `cherry_pick.rs` 的提交解析入口（`resolve_commit` 调用处）把 `<A>..<B>` 展开为与 `git rev-list --reverse <A>..<B>` 等价的提交列表（`B` 可达且 `A` 不可达，按 Git 默认遍历序反转），交给现有多提交序列执行；冲突停止复用现有序列逻辑（`-n`、`--ff` 与范围的组合沿用该序列的既有逻辑，其专项验收（`t3508#8/#9`）按用户 2026-11-19 05:17:51 UTC 决定登记 `DEFER-SA-14`）。序列中冲突与失败路径的提示标签改按展开后的提交生成：现有代码在未跟踪文件覆盖（`src/command/cherry_pick.rs:1147`）、冲突（`:1179`）与其它单提交错误（`:1228`）三处以原始参数下标 `args.commits[i]` 取标签，范围展开后下标会越界而 panic，三处统一改为按展开后的提交取标签；验收以冲突与未跟踪文件覆盖两条停止路径代表（其它单提交错误与二者共用同一取标签方式）。范围内的合并提交沿用现有逐提交语义：到达该提交时以现有 `MergeCommitUnsupported`（`LBR-CLI-002`）结束（与 Git 的诊断文本与停止状态差异登记 `DEFER-SA-04`）。无法解析的端点沿用现有 `InvalidCommit`（`LBR-CLI-003`，既有行为）。R43–R45 按用户 2026-11-19 05:17:51 UTC 决定收窄：以 #726 的线性范围形态验收（中途冲突用例的主题序列验证范围的起止边界与顺序）；非线性历史（含合并、相等与交错的提交时间戳）上的遍历序与 Git 对照、范围内合并提交的停止，以及 `t3430#28`（`TC-2332`）登记 `DEFER-SA-14`。单提交参数行为不变。
**Out of scope:** rerere 标志（由 `SA-03`、`SA-03b` 承接）；`-n`/`--ff` 与范围组合的专项验收（`DEFER-SA-14`）；`-m` 用于范围或非合并提交、`A...B`/`^A B` 等其它修订语法（`DEFER-SA-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 逐参数 `resolve_commit` → `util::get_commit_base`，`foo..bar-main` 报 `failed to resolve commit reference`（`LBR-CLI-003`，exit 129） | `src/command/cherry_pick.rs:1077-1083,3036-3038`; issue #726 |
| 多提交序列已支持 `-n` 与 `--ff` | `src/command/cherry_pick.rs:1102,1148,1180,1957` |
| 合并提交未给 `-m` 返回 `MergeCommitUnsupported`（`cherry-picking merge commits is not supported`，`LBR-CLI-002`） | `src/command/cherry_pick.rs:103-104,212,2018` |
| 序列循环的冲突与失败路径以 `args.commits[i]` 取标签（`i` 为展开后提交的下标）；成功的上一步经 `SequenceAdvance::after` 先把状态行推进到下一提交且冲突标记为假 | `src/command/cherry_pick.rs:1122,1147,1179,1228,3093-3115` |
| `--continue` 对无冲突标记的状态行重新拣选 `current_oid` | `src/command/cherry_pick.rs:1699-1722` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；「中途冲突」fixture（`t3504#7` 第 1 步形态，`TC-0429`）：从 `T` 运行 `libra cherry-pick A..C`，`B` 干净、`C` 冲突（`C` 位于展开后序列的第 2 位而原始参数只有 1 个）；「中途未跟踪覆盖」fixture：从 `T` 运行 `libra cherry-pick A..C`，`B` 干净、`C` 新增路径 `u.txt`，而工作区在运行前已有字节独特的未跟踪文件 `u.txt`（`C` 同样位于展开后第 2 位））

- [ ] AC1：「中途冲突」命令的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（在 `C` 处以既有冲突停止正常报错，而非按原始参数下标取标签越界 panic）。
- [ ] AC2：AC1 的停止之后，解决冲突并 `libra add`，`libra cherry-pick --continue` exit 0（冲突经正常出口形成带冲突标记的可恢复停止态；若冲突路径按原始参数下标取标签而 panic，`SequenceAdvance` 已把状态行推进到 `C` 但缺冲突标记，`--continue` 会重新拣选 `C` 并因已暂存的解决结果被拒）。
- [ ] AC3：AC2 之后 `libra rev-list --reverse T..HEAD` 依次输出的提交 OID，逐个以 `libra show --no-patch --pretty=format:%s <OID>` 取主题后，主题序列恰为 `B`、`C`（解决结果已提交，范围不含 `A`、含 `C`）。
- [ ] AC4：AC2 之后 HEAD 的树等于 `--continue` 之前解决后索引的树（`--continue` 前 `libra write-tree` 的输出）。
- [ ] AC5：AC2 之后无 `sequence_state` 行（序列完成并释放状态）。
- [ ] AC6：「中途未跟踪覆盖」命令的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（以 `UntrackedOverwrite` 正常报错；按原始参数下标取标签越界会 panic、不产生结构化错误报告）。
- [ ] AC7：AC6 之后未跟踪文件 `u.txt` 的字节（`libra hash-object`）与命令前相同。
- [ ] AC8：AC6 之后 `sequence_state` 中 `kind='cherry_pick'` 行的 `current_oid` 等于 `C`（停在 `C` 之前、可 `--continue` 的停止态）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。文档写明范围内合并提交沿用逐提交语义及其与 Git 的差异（专项验收见 `DEFER-SA-14`、`DEFER-SA-04`）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类。「中途冲突」停止命中既有错误类「cherry-pick 序列冲突停止」（`CherryPickError::Conflict`，`LBR-CONFLICT-001`，诊断模板 `failed to cherry-pick {commit}: {reason}`，`src/command/cherry_pick.rs:133,280-285`），「中途未跟踪覆盖」停止命中既有错误类「序列中途未跟踪覆盖」（`CherryPickError::UntrackedOverwrite`，`LBR-CONFLICT-001`，`src/command/cherry_pick.rs:293-308`）；本卡只使二者在范围输入上可达并把标签改按展开后的提交取得（诊断模板、稳定码与退出码不变），按「验证执行协议」第 11c 条作为其余路径各断言稳定码（AC1、AC6）；#726 未要求这两条路径的人读诊断。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_range_conflict_stop_then_continue)'`（new；AC1–AC5）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_range_later_untracked_overwrite_stops_cleanly)'`（new；AC6–AC8）
- [ ] VER3 `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=command::cherry_pick::tests::range_expansion_loads_each_commit_once)'`（new；`Performance budget`）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::cherry_pick_test::/)'`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `SA-01b`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cherry_pick.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`（范围支持与合并提交的逐提交语义）、`COMPATIBILITY.md`（`cherry-pick` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`。
**Rollback mode:** `immutable-release`
**Performance budget:** 范围展开为一次可达遍历（visited 集合去重），每个提交对象至多加载一次，加载次数与 reach(B)、reach(A) 的提交数之和成线性；不做墙钟断言。VER3 以计数型对象加载器在 32 提交、含分叉与合并的内存图上断言加载次数等于去重提交数（纯函数单元测试，不依赖 cwd 或进程全局状态）。
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=cherry-pick提交范围展开; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=SA-01b,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-01b; release=independent; split-from=N/A; exception=N/A`

---

### Task SA-02b: 已撤回（不得开工）

**Status:** withdrawn。R43 按用户 2026-11-19 05:17:51 UTC 决定撤回（原：`t3508#8/#9` 的范围与 `-n`、`--ff` 组合补测，R42 自 `SA-02` 拆出）。#726 的验收项只要求范围解析与应用、rerere 与 `--continue` 互斥及 `--abort`，不含 `-n`/`--ff` 组合；该组合沿用既有多提交序列逻辑，其专项验收登记 `DEFER-SA-14`。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-03: `--rerere-autoupdate` 在范围冲突上生效且 rerere 标志与 `--continue` 互斥（#726）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：`--continue` 的 `conflicts_with_all` 增加 `rerere_autoupdate`/`no_rerere_autoupdate`（clap 冲突 → `LBR-CLI-002`，诊断点名两个选项）；范围 pick 复用同一 `cherry_pick_single_commit` 的 rerere 路径，`--rerere-autoupdate` 在首个冲突处按预录结果改写并暂存 `foo`。AC1–AC3 由 `command::cherry_pick_test::cherry_pick_range_rerere_autoupdate_stop_tc0429` 覆盖（`LBR-CONFLICT-001`、`foo` 字节、stage 0 且 blob 等于预录 `R`），AC4–AC8 由 `cherry_pick_continue_rejects_rerere_flags_tc0429` 覆盖（两个标志的稳定码与点名诊断、之后 `--abort` exit 0）；`cherry_pick_test` 84/84；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 使 `--rerere-autoupdate`/`--no-rerere-autoupdate` 与 `--continue` 同用时按 Git 在参数解析阶段拒绝（`--continue` 的 `conflicts_with_all` 加入这两个标志，`src/command/cherry_pick.rs:689-695`），并在 `SA-02` 的范围上验收 `t3504#7`（`TC-0429`）：`libra cherry-pick --rerere-autoupdate foo..bar-main` 在首个冲突处停止时 rerere 以预录解决结果改写并暂存 `foo`，该停止态可 `--abort`（#726 验收项 2–5）。若范围路径未把 rerere 策略传给每个 pick，在本卡修正（GC-01）。R43 按用户 2026-11-19 05:17:51 UTC 决定收窄到 #726 的验收项：重复与混合标志「最后一个生效」（#727）拆为 `SA-03b`；`t3504#4–#6`（`rerere.autoUpdate` 配置、否定标志的范围模式与跨 `--continue` 的第二冲突恢复）登记 `DEFER-SA-16`。
**Out of scope:** 范围展开（由 `SA-02` 承接）；重复/混合 rerere 标志（由 `SA-03b` 承接）；`t3504#4–#6` 与跨 `--continue` 的第二冲突恢复（`DEFER-SA-16`）；rerere 标志与 `--skip`/`--abort`/`--quit` 的互斥（`DEFER-SA-06`）；`rerere` 子命令与 `rerere.enabled` 默认值（永久非目标：本卡只消费既有 rerere 集成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--continue` 的 `conflicts_with_all` 不含 rerere 标志 | `src/command/cherry_pick.rs:689-695` |
| 有效 rerere 策略由 `rerere_autoupdate_override` 计算 | `src/command/cherry_pick.rs:482-485,574-582` |
| rerere 预录与 `--no-rerere-autoupdate` 的既有用例 | `tests/command/cherry_pick_test.rs:2023` |
| `t3504#7` 的断言（`test_cmp foo-expect foo`、`git diff-files --quiet`、两个 `cannot be used with --continue`、`--abort`） | Git 源码 `t/t3504-cherry-pick-rerere.sh` 的 `cherry-pick --continue rejects --rerere-autoupdate` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture（`t3504` 形态，全部在仓库外的隔离 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：设 `rerere.enabled=true`；在 `main` 上依次提交 `foo`（新增文件 `foo`）、`foo-main`（把 `foo` 改为 `foo-main`）、`bar-main`（新增文件 `bar`），自 `foo` 建分支 `dev` 并依次提交 `foo-dev`（把 `foo` 改为 `foo-dev`）、`bar-dev`（新增与 `bar-main` 内容不同的 `bar`）；在 `dev` 上运行 `libra cherry-pick foo-main`（在 `foo` 上冲突），把 `foo` 写为 `foo-resolved`、运行 `libra rerere` 录下解决结果并记录此时 `libra hash-object foo` 的输出 `R` 后 `libra cherry-pick --abort`（与既有用例 `cherry_pick_rerere_autoupdate_flags_override_configured_staging` 相同的预录方式）；「预录解决结果」指 `foo-resolved` 的字节，「`foo` 已暂存为预录解决结果」指 `libra ls-files --stage -- foo` 恰有一个条目，其 stage 为 0 且 blob 等于 `R`（一次比较；未应用 rerere、未暂存或命令被拒都使其不成立）；被测命令都在 `dev`（HEAD 为 `bar-dev`）上运行，范围 `foo..bar-main` 的首个 pick `foo-main` 在 `foo` 上冲突，同一用例内相邻两次运行之间以 `libra cherry-pick --abort` 回到 `bar-dev`）

- [ ] AC1：`t3504#7`（`TC-0429`）`libra cherry-pick --rerere-autoupdate foo..bar-main` 在首个冲突处停止，其结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`。
- [ ] AC2：AC1 之后工作区 `foo` 的字节等于预录解决结果（rerere 已应用）。
- [ ] AC3：AC1 之后 `foo` 已暂存为预录解决结果（rerere autoupdate 生效）。
- [ ] AC4：在 AC1 的停止态下 `libra cherry-pick --continue --rerere-autoupdate` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CLI-002`。
- [ ] AC5：AC4 的人读 stderr中有一行同时含 `cannot be used with`、`'--continue'` 与 `'--rerere-autoupdate'`（诊断点名冲突的两个选项）。
- [ ] AC6：同一停止态下 `libra cherry-pick --continue --no-rerere-autoupdate` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CLI-002`。
- [ ] AC7：AC6 的人读 stderr中有一行同时含 `cannot be used with`、`'--continue'` 与 `'--no-rerere-autoupdate'`。
- [ ] AC8：AC4、AC6 之后 `libra cherry-pick --abort` exit 0。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类。AC1 的范围冲突停止属既有错误类「cherry-pick 序列冲突停止」（`LBR-CONFLICT-001`，`src/command/cherry_pick.rs:133,280-285`）；AC4、AC6 的两条 `--continue` 拒绝命中既有错误类「clap 参数冲突」（`LBR-CLI-002`，退出码 129，`src/cli.rs:3176-3221`；本卡只把两个 rerere 标志加入 `--continue` 的冲突约束）。按「验证执行协议」第 11c 条作为其余路径各断言稳定码（AC1、AC4、AC6）；#726 验收项 3、4 明确要求两条拒绝各以自己的 `cannot be used with` 提示失败，故各加 1 条人读诊断（AC5、AC7）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_range_rerere_autoupdate_stop_tc0429)'`（new；AC1–AC3）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_continue_rejects_rerere_flags_tc0429)'`（new；AC4–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::cherry_pick_test::/)'`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::cherry_pick::/)'`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `SA-02`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cherry_pick.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`（rerere 标志与 `--continue` 互斥、范围上的 rerere autoupdate）、`COMPATIBILITY.md`（`cherry-pick` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=cherry-pick范围rerere与continue互斥; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=SA-02,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-02; release=independent; split-from=N/A; exception=N/A`

---

### Task SA-03b: `--rerere-autoupdate`/`--no-rerere-autoupdate` 可重复且最后一个生效（#727）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：两个 rerere 标志的 clap 定义改为 `overrides_with_all = [rerere_autoupdate, no_rerere_autoupdate]`（含自身），从而可重复传入且命令行最后一个生效（Git `t3504#8` / `TC-0468`）。AC1–AC8 由 `command::cherry_pick_test::cherry_pick_rerere_autoupdate_repeated_last_wins` 覆盖（重复肯定、肯定—否定—肯定、肯定—否定、以及 `rerere.autoUpdate=true` 时重复否定覆盖配置，均以 staged blob 与冲突 stage 断言）；`cherry_pick_test` 85/85；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 使两个 rerere 标志都可重复传入、按命令行顺序最后一个生效（Git 的 `--rerere-autoupdate` 为可重复的布尔选项）：现有 clap 定义只互相 `overrides_with`（`src/command/cherry_pick.rs:820-825`），重复同一标志被拒（`the argument '--rerere-autoupdate' cannot be used multiple times`，`LBR-CLI-002`）；改为各自同时覆盖自身与对方。以与 `SA-03` 相同形态的 `t3504` fixture 验收 `t3504#8`（`TC-0468`，#727 验收项 1–4）。R43 按用户 2026-11-19 05:17:51 UTC 决定自原 `SA-03` 拆出（#727 与 #726 是两处独立的参数定义改动）。
**Out of scope:** rerere 标志与 `--continue` 互斥及范围上的 autoupdate（由 `SA-03` 承接）；`t3504#4–#6`（`DEFER-SA-16`）；`rerere` 子命令与 `rerere.enabled` 默认值（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 两个 rerere 标志只互相 `overrides_with`，重复传入同一标志被 clap 拒绝 | `src/command/cherry_pick.rs:820-825`; issue #727 |
| `t3504#8` 的三段断言（重复肯定、肯定—否定—肯定、肯定—否定） | Git 源码 `t/t3504-cherry-pick-rerere.sh` 的 `cherry-pick --rerere-autoupdate more than once` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture（`t3504` 形态，全部在仓库外的隔离 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：设 `rerere.enabled=true`；在 `main` 上依次提交 `foo`（新增文件 `foo`）、`foo-main`（把 `foo` 改为 `foo-main`）、`bar-main`（新增文件 `bar`），自 `foo` 建分支 `dev` 并依次提交 `foo-dev`（把 `foo` 改为 `foo-dev`）、`bar-dev`（新增与 `bar-main` 内容不同的 `bar`）；在 `dev` 上运行 `libra cherry-pick foo-main`（在 `foo` 上冲突），把 `foo` 写为 `foo-resolved`、运行 `libra rerere` 录下解决结果并记录此时 `libra hash-object foo` 的输出 `R` 后 `libra cherry-pick --abort`（与既有用例 `cherry_pick_rerere_autoupdate_flags_override_configured_staging` 相同的预录方式）；「预录解决结果」指 `foo-resolved` 的字节，「`foo` 已暂存为预录解决结果」指 `libra ls-files --stage -- foo` 恰有一个条目，其 stage 为 0 且 blob 等于 `R`（一次比较；未应用 rerere、未暂存或命令被拒都使其不成立）；被测命令都在 `dev`（HEAD 为 `bar-dev`）上运行，范围 `foo..bar-main` 的首个 pick `foo-main` 在 `foo` 上冲突，同一用例内相邻两次运行之间以 `libra cherry-pick --abort` 回到 `bar-dev`）

- [ ] AC1：`t3504#8`（`TC-0468`）`libra cherry-pick --rerere-autoupdate --rerere-autoupdate foo..bar-main` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（重复标志被接受、在首个冲突处停止；若仍被 clap 拒绝则为 `LBR-CLI-002`）。
- [ ] AC2：AC1 之后 `foo` 已暂存为预录解决结果（rerere 把 `foo` 解决为 `foo-resolved` 并自动暂存）。
- [ ] AC3：`libra cherry-pick --rerere-autoupdate --no-rerere-autoupdate --rerere-autoupdate foo..bar-main` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（混合标志被接受）。
- [ ] AC4：AC3 之后 `foo` 已暂存为预录解决结果（末位肯定标志生效）。
- [ ] AC5：`libra cherry-pick --rerere-autoupdate --no-rerere-autoupdate foo..bar-main` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`。
- [ ] AC6：AC5 之后 `libra ls-files --stage -- foo` 含 stage 1–3 的冲突条目且无 stage 0 条目（末位否定标志生效，未暂存；若首个标志生效则 `foo` 已暂存）。
- [ ] AC7：设 `rerere.autoUpdate=true` 后 `libra cherry-pick --no-rerere-autoupdate --no-rerere-autoupdate foo..bar-main` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（重复否定标志被接受）。
- [ ] AC8：AC7 之后 `libra ls-files --stage -- foo` 含 stage 1–3 的冲突条目且无 stage 0 条目（重复否定标志覆盖配置，未暂存）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类。四条命令（AC1、AC3、AC5、AC7）在重复或混合的 rerere 标志被接受后，于范围 `foo..bar-main` 的首个冲突处命中既有错误类「cherry-pick 序列冲突停止」（`CherryPickError::Conflict`，`LBR-CONFLICT-001`，`src/command/cherry_pick.rs:133,280-285`）；按「验证执行协议」第 11c 条作为其余路径各断言稳定码（同时区分被 clap 拒绝的 `LBR-CLI-002`）；#727 未要求这些路径的人读诊断。`--no-rerere-autoupdate` 下 rerere 仍改写工作区而不暂存是既有语义，由共享回归组中的既有用例 `cherry_pick_rerere_autoupdate_flags_override_configured_staging` 守护。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_rerere_autoupdate_repeated_last_wins)'`（new；AC1–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::cherry_pick_test::/)'`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::cherry_pick::/)'`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `SA-03`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cherry_pick.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`（rerere 标志可重复、最后一个生效）、`COMPATIBILITY.md`（`cherry-pick` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=cherry-pick的rerere标志可重复; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-03,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-03; release=independent; split-from=SA-03; exception=N/A`

---

### Task SA-04: `cherry-pick --ff` 选取 root commit 与 Git `cp_ff` OID 对齐（#729）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：`create_cherry_pick_commit` 在 `format_commit_msg` 前补齐消息末尾换行（共享消息构建会裁掉），使同树/同父/同身份与日期且不签名的拣选提交与 Git **OID 一致**；`--ff` 判定逻辑不变——仅目标为 HEAD 唯一直接父提交时快进，root 目标按普通 pick 生成提交。AC1 由 `command::cherry_pick_test::cherry_pick_ff_root_commit_matches_git_oid_tc0512` 覆盖（git 构建同一仓库后经 `libra init --from-git-repository` 对照 OID），AC2 同测试内验证直接父目标真快进；`cherry_pick_test` 86/86；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 对齐 `t/t3506-cherry-pick-ff.sh#10`（`TC-0512`）：HEAD 存在而目标为 root commit 时 `--ff` 无法快进，按普通 pick 生成新提交，在相同身份/日期且不签名（`DEP-SA-01` 交付的默认）时其 OID 等于 Git 结果；HEAD 恰为目标唯一父提交时仍真正快进。issue #729 原文「应快进」与上游 `t3506#10` 不符，本卡以上游测试为准。开工先以 Git 对照定位 OID 差异来源（签名头、提交者日期、消息尾行）；若无需改 `cherry_pick.rs`，按 GC-01 收敛为补测与文档。
**Out of scope:** unborn HEAD 上的 `--ff`（由 `SA-05` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--ff` 仅当目标为 HEAD 的单父直接子提交时快进，否则普通 pick | `src/command/cherry_pick.rs:1954-1963` |
| `libra commit` 支持 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` | `src/command/commit.rs:910-933` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项）

- [ ] AC1：`t3506#10`（`TC-0512`）HEAD 为 amend 出的另一 root 时 `libra cherry-pick --ff first` 之后 HEAD OID 等于 Git 在同一 fixture、同一 `GIT_AUTHOR_*`/`GIT_COMMITTER_*` 且两侧不签名时的结果。
- [ ] AC2：HEAD 恰为目标唯一父提交时 `libra cherry-pick --ff <target>` 之后 HEAD OID 等于 `<target>`。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类，也没有失败路径。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_ff_root_commit_matches_git_oid_tc0512)'`（new；AC1）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_ff_direct_child_fast_forwards)'`（new；AC2）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::cherry_pick_test::/)'`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `SA-03b`, `DEP-SA-01`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cherry_pick.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`（`--ff` 对 root commit 的语义）、`COMPATIBILITY.md`（`cherry-pick` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=cherry-pick--ff对root-commit的OID对齐; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=SA-03b,DEP-SA-01,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-03b; release=independent; split-from=N/A; exception=N/A`

---

### Task SA-05a: 已撤回（不得开工）

**Status:** withdrawn。R43 按用户 2026-11-19 05:17:51 UTC 决定撤回（原：`cherry-pick` 状态的 unborn 全零编码，独立发布）。#730 只要求 unborn 分支上 `cherry-pick --ff` 快进到 root commit（`SA-05`），该路径不持久化 sequencer 状态，故 `CherryPickState.head_orig` 无需全零编码（unborn 上非快进 pick 与冲突暂停登记 `DEFER-SA-17`）。共享的可选 OID 列编解码函数改由 `REL-SA-02` 家族子卡 `SA-09f` 交付，其唯一消费者为 `SA-09b`（`am` 的 `head_orig`/`expected_head`）。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-05b: 已撤回（不得开工）

**Status:** withdrawn。R43 按用户 2026-11-19 05:17:51 UTC 决定撤回（原：`cherry_pick` 行全零 `head_orig` 的 `ORIG_HEAD` 投影与 GC 黑盒）。`SA-05a` 撤回后 `cherry_pick` 行不再出现全零 `head_orig`，该投影与 GC 黑盒随 unborn 非快进 pick 一并登记 `DEFER-SA-17`；`DEP-SA-08`（`MP-30`）随之撤回，本计划不再写 `src/internal/pseudo_ref.rs`。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-05c: unborn 分支上 `cherry-pick --ff` 遇已暂存条目时拒绝（#730 前置安全）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：新增 `UnbornFfStaged` 变体（消息与稳定码沿用「本地改动会被覆盖」的 `LBR-CONFLICT-001`，提示改为 `nothing was written; the index must be empty before 'libra cherry-pick --ff' can fast-forward an unborn branch`）；`run_cherry_pick` 在解析提交前检查 `args.ff && HEAD unborn && index.tracked_files()` 非空并提前拒绝，不写 sequencer 状态、不改索引/工作区。AC1–AC6 由 `command::cherry_pick_test::cherry_pick_unborn_ff_refuses_staged_entries` 覆盖（错误码、索引快照、工作区哈希映射、HEAD 仍 unborn、无 sequencer 行、唯一 Hint 行）；`cherry_pick_test` 87/87；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** R49 新增（回应 Codex R48 B-1）：既有快进路径在未跟踪覆盖检查之后以 `reset --hard` 检出目标（`src/command/cherry_pick.rs:1956-2002`），不保护已暂存条目；`SA-05` 使该路径在 unborn 分支上可达后，unborn 分支上已暂存的文件会被目标树覆盖或删除。本卡先于 `SA-05` 交付：HEAD 为 unborn、带 `--ff` 且索引非空时，`cherry-pick` 在任何写入之前以既有的 `LocalChangesWouldBeOverwritten`（消息 `your local changes would be overwritten by cherry-pick`、`LBR-CONFLICT-001`，消息与稳定码不变）拒绝，不写入 sequencer 状态；该错误既有的两条提示面向已停止的序列（「提交或储藏后继续」「`--skip`」，`src/command/cherry_pick.rs:321-324`），在此情形不适用，改为只给出 `nothing was written; the index must be empty before 'libra cherry-pick --ff' can fast-forward an unborn branch`（只说明前置条件，不规定可能因「已暂存与工作区内容不同」而失败的清理命令，`src/command/remove.rs:285-288,350-362`）（第 11a 条：提示改写以 1 条人读判据验收）。这比 Git 严格（Git 快进经 `checkout_fast_forward`，只拒绝会被覆盖的本地改动），差异写入 cherry-pick 三份文档与 `COMPATIBILITY.md`。born 分支上快进不保护已暂存条目是既有缺陷，本计划不改变，登记 `DEFER-SA-22`。
**Out of scope:** born 分支上 `--ff` 快进对已暂存条目的保护（既有缺陷，`DEFER-SA-22`）；unborn 上不带 `--ff` 的 pick（`DEFER-SA-17`）；unborn 上的快进本身（由 `SA-05` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 快进路径只检查未跟踪覆盖后 `reset --hard` 到目标 | `src/command/cherry_pick.rs:1956-2002,2936` |
| `LocalChangesWouldBeOverwritten` 的消息与稳定码 | `src/command/cherry_pick.rs:167-168,224,321-324` |
| unborn HEAD 上当前以 `failed to resolve current HEAD` 失败 | `src/command/cherry_pick.rs:2367-2369` |
| Git 快进经 `checkout_fast_forward`，拒绝会被覆盖的本地改动 | Git 源码 `sequencer.c` 的 `fast_forward_to` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture（`t3506` 形态）：在 `main` 上提交 root commit `first`（只新增 `file1`）后 `libra switch --orphan unborn`，`libra rm --cached -r .` 并删除工作区中的已跟踪文件，再新建字节独特的文件 `s.txt`（`first` 中没有该路径）并 `libra add s.txt`；运行前记录 `libra ls-files --stage` 输出与工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）作为「命令前快照」，然后运行 `libra cherry-pick --ff first`）

- [ ] AC1：命令的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（在任何写入前拒绝；当前代码以 `failed to resolve current HEAD` 失败，`SA-05` 若无本卡则会快进并删除 `s.txt`）。
- [ ] AC2：命令之后 `libra ls-files --stage` 的输出等于命令前快照（`s.txt` 仍已暂存）。
- [ ] AC3：命令之后工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）等于命令前快照（`s.txt` 未被删除或改写）。
- [ ] AC4：命令之后 `libra rev-parse HEAD` 失败（分支仍为 unborn）。
- [ ] AC5：命令之后当前作用域的 `sequence_state` 中没有任何行（拒绝不写入 sequencer 状态，后续 `cherry-pick` 不会被 `InProgress` 阻断）。
- [ ] AC6：AC1 的人读 stderr 中以 `Hint: ` 开头的行序列恰为 [`Hint: nothing was written; the index must be empty before 'libra cherry-pick --ff' can fast-forward an unborn branch`]（一次序列比较：改写后的提示存在，且不再出现 `--skip` 等面向已停止序列的提示）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类。命令命中 `cherry-pick` 的既有错误类「本地改动会被覆盖」（`LocalChangesWouldBeOverwritten`，`LBR-CONFLICT-001`，`src/command/cherry_pick.rs:167-168,224,321`，消息与稳定码不变），本卡使其在 unborn 起点的 `--ff` 上可达，按「验证执行协议」第 11c 条断言稳定码（AC1）；#730 未要求其人读诊断；该路径的提示改写按第 11a 条以人读判据 AC6 验收。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_unborn_ff_refuses_staged_entries)'`（new；AC1–AC6）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::cherry_pick_test::/)'`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::cherry_pick::/)'`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `SA-04`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cherry_pick.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`（unborn 分支上 `--ff` 遇已暂存条目时拒绝及其与 Git 的差异）、`COMPATIBILITY.md`（`cherry-pick` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=unborn分支cherry-pick快进的已暂存条目保护; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-04,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-04; release=independent; split-from=SA-05; exception=N/A`

---

### Task SA-05: unborn 分支上 `cherry-pick --ff` 快进到 root commit（#730）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：`--ff` 快进条件扩展为「目标单父且父为 HEAD」或「目标无父且 HEAD unborn」；unborn 情形先按目标创建/更新当前分支引用（`Branch::create_branch_exclusive`/`update_branch`），再复用既有 `reset --hard` 检出索引与工作区，HEAD 保持指向该分支的符号引用；未跟踪覆盖检查仍在任何写入之前。AC1–AC5 由 `cherry_pick_unborn_ff_root_commit_tc0513` 覆盖（exit 0、HEAD==first、symbolic-ref、索引集合、工作区 hash-object 映射），AC6–AC8 由 `cherry_pick_unborn_ff_refuses_untracked_overwrite` 覆盖（`LBR-CONFLICT-001`、未跟踪文件字节不变、HEAD 仍 unborn）；`cherry_pick_test` 89/89；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步；unborn 非快进 pick 保持现状（`DEFER-SA-17`）。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 对齐 `t3506#11`（`TC-0513`）：HEAD 为 unborn 且以 `--ff` 选取无父提交（root commit）时，按 Git 的快进条件（`sequencer.c` `do_pick_commit`：`allow_ff` 且目标无父、当前 HEAD unborn → `fast_forward_to`）把当前分支直接快进到该提交——创建分支引用指向目标、HEAD 仍为指向该分支的符号引用、索引与工作区按目标树检出，不生成新提交（#730 验收项 1–3）；快进前复用既有快进路径的未跟踪文件覆盖检查（`ensure_no_untracked_overwrite`，`src/command/cherry_pick.rs:1967-1984`），目标树会覆盖未跟踪文件时以 `UntrackedOverwrite`（`LBR-CONFLICT-001`）拒绝且不写入任何内容。现有 `--ff` 快进只匹配「单父且父为 HEAD」（`src/command/cherry_pick.rs:1954-1966`），unborn 时在 `src/command/cherry_pick.rs:2367-2369` 以 `failed to resolve current HEAD` 失败。R43 按用户 2026-11-19 05:17:51 UTC 决定收窄到 #730 的验收项：unborn 上的非快进 pick（目标有父或不带 `--ff`）、冲突暂停与 `--abort`（含原 `SA-05a`、`SA-05b` 的全零 `head_orig` 编码、`ORIG_HEAD` 投影与 GC 黑盒）登记 `DEFER-SA-17`，本卡不改这些路径的代码（保持现状）。已拆出 `SA-05c`（unborn 上 `--ff` 遇已暂存条目时拒绝，R49 自本卡拆出并先于本卡交付）；born 分支上快进不保护已暂存条目的既有缺陷登记 `DEFER-SA-22`。
**Out of scope:** unborn 上的非快进 pick、冲突暂停、`--abort` 与相关状态编码（`DEFER-SA-17`）；unborn 分支上的 `am`（由 `SA-09f`、`SA-09b`、`SA-09` 经 `REL-SA-02` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--ff` 只在目标单父且父为 HEAD 时快进 | `src/command/cherry_pick.rs:1954-1966` |
| unborn 上普通 pick 以 `failed to resolve current HEAD` 失败 | `src/command/cherry_pick.rs:2367-2369`; issue #730 |
| Git 对 unborn 上的无父目标快进 | Git 源码 `sequencer.c:2402-2407`（`do_pick_commit`：`(!parent && unborn)` → `fast_forward_to`） |
| `switch --orphan` 建立 unborn 分支，`symbolic-ref HEAD` 在 unborn 分支上输出 `refs/heads/<分支>` | 2026-11-19 以已安装 `libra` 实测（刚 `libra init` 的仓库输出 `refs/heads/main`） |
| 既有快进路径在 `reset --hard` 前调用 `ensure_no_untracked_overwrite`；`UntrackedOverwrite` 映射 `ConflictUnresolved`（`LBR-CONFLICT-001`） | `src/command/cherry_pick.rs:220-224,1967-1984,2936-2939` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture（`t3506` 形态）：在 `main` 上提交 root commit `first`（只新增 `file1`）后 `libra switch --orphan unborn`，`libra rm --cached -r .` 并删除工作区中的已跟踪文件（`t3506#11` 的 `git rm --cached -r .`、`rm -rf *`），然后运行 `libra cherry-pick --ff first`；「未跟踪覆盖」fixture：同样建立 unborn 分支后，在工作区放一个字节独特的未跟踪文件 `file1`（与 `first` 中的路径相同、内容不同），再运行同一命令）

- [ ] AC1：`t3506#11`（`TC-0513`）命令 exit 0。
- [ ] AC2：AC1 之后 `libra rev-parse HEAD` 等于 `first`（未生成新提交）。
- [ ] AC3：AC1 之后 `libra symbolic-ref HEAD` 输出 `refs/heads/unborn`（快进的是当前分支，HEAD 未变为 detached）。
- [ ] AC4：AC1 之后 `libra ls-files --stage` 的（模式、blob OID、stage、路径）条目集合等于 `libra ls-tree -r first` 各条目配 stage 0 所成的集合（索引已检出为 `first` 的树）。
- [ ] AC5：AC1 之后工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）等于 `first` 树中「路径 → blob OID」的映射（工作区已检出）。
- [ ] AC6：「未跟踪覆盖」fixture 命令的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（复用既有快进路径的同一检查与同一错误 `UntrackedOverwrite`）。
- [ ] AC7：AC6 之后未跟踪文件 `file1` 的字节（`libra hash-object`）与命令前相同（未被目标树覆盖）。
- [ ] AC8：AC6 之后 `libra rev-parse HEAD` 失败（分支仍为 unborn，未被快进）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类。「未跟踪覆盖」fixture 命中既有快进路径的既有错误类「写入前未跟踪覆盖」（`CherryPickError::UntrackedOverwrite`，`UntrackedStop::BeforeAnyWrite`，`LBR-CONFLICT-001`，`src/command/cherry_pick.rs:293-308`）；本卡只使其在 unborn 起点可达，按「验证执行协议」第 11c 条作为其余路径断言稳定码（AC6）；#730 未要求该路径的人读诊断。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_unborn_ff_root_commit_tc0513)'`（new；AC1–AC5）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_unborn_ff_refuses_untracked_overwrite)'`（new；AC6–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::cherry_pick_test::/)'`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `SA-05c`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cherry_pick.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`（unborn 分支上 `--ff` 快进到 root commit；unborn 上非快进 pick 仍不支持（`DEFER-SA-17`））、`COMPATIBILITY.md`（`cherry-pick` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=unborn分支上cherry-pick-ff快进root; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=SA-05c,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-05c; release=independent; split-from=N/A; exception=N/A`

---

### Task SA-06: `show --pretty=format:%B` 不追加多余结尾换行（#731）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：`show` 自定义格式分支改为 Git 的分隔语义——`format:` 在 `--no-patch` 时不再追加结尾换行（模板结果即全部输出），其后有补丁/统计块时保留分隔换行；`tformat:`、隐式模板与命名预设保持终止换行。AC1–AC3 由 `command::cherry_pick_test::cherry_pick_empty_drop_show_message_matches_git` 覆盖（`cherry-pick --empty=drop` exit 0、HEAD 不变、`show --no-patch --pretty=format:%B` 与同一 fixture 的 Git stdout 字节一致）；`show_test` + `cherry_pick_test` 132/132；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步；#731 之外的格式变体字节对照登记 `DEFER-SA-15`。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** #731 的多余空行来自 `show` 自定义格式分支在 `formatter.format` 后无条件 `output.push('\n')`，而非 `cherry-pick --empty=drop` 改写了 HEAD（`--empty=drop` 不改 HEAD）。修复 `show.rs`：`format:` 采用 Git 的分隔语义——其后没有补丁或 `--stat` 输出时不追加结尾换行，其后有补丁或 `--stat` 输出时保留分隔换行（`src/command/show.rs:580-705` 的补丁与统计输出直接接在格式化头之后）；`tformat:`、隐式模板（`--format=%B`）与命名预设保持终止换行。这些 #731 以外的格式变体（`tformat:`、隐式模板、默认 `medium`、`oneline`，以及 `format:` 后接 `--stat`/补丁时的分隔）与 Git 的字节对照按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论（R47）登记 `DEFER-SA-15`，本卡只验收 #731 的 `--empty=drop` 场景；以 `--empty=drop` 端到端验收 `t3505#16`（`TC-0523`，#731 验收项 1–3：冗余提交被丢弃、HEAD 不变、`show --pretty=format:%B` 与 Git 一致）。R44 按用户 2026-11-19 05:17:51 UTC 决定收窄：`--empty=keep` 之后的同一输出（`t3505#17`、`TC-0524`）登记 `DEFER-SA-15`，`--format=format:%B` 为 `--pretty=format:%B` 的别名（`src/command/show.rs:86-94`），不单列验收。
**Out of scope:** `cherry-pick` 空停止（由 `SA-01` 承接）；`log` 的格式输出（不修改 `log.rs`）；#731 以外的格式变体与 `format:` 后接 `--stat`/补丁时的分隔与 Git 的字节对照（`DEFER-SA-15`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 自定义格式分支无条件追加换行 | `src/command/show.rs:1164-1165` |
| `--format` 为 `--pretty` 别名 | `src/command/show.rs:86-94` |
| `--empty=drop` 不改 HEAD | `src/command/cherry_pick.rs:2386-2393` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；AC1、AC2 的 fixture 为 `t3505#16` 形态（`fork` 上已含 `main` 的改动，HEAD 为 `fork^0`，两个提交的消息不同），运行前记录 `libra rev-parse HEAD`；AC3 为同一 fixture 上 Libra 与 Git 的 stdout 字节比较）

- [ ] AC1：`t3505#16`（`TC-0523`）`libra cherry-pick --empty=drop main` exit 0。
- [ ] AC2：AC1 之后 `libra rev-parse HEAD` 等于运行前记录的 HEAD（冗余提交被丢弃、未生成提交）。
- [ ] AC3：AC1 之后 `libra show --no-patch --pretty=format:%B HEAD` 的 stdout 等于 Git 在同一 fixture 上 `git cherry-pick --empty=drop main` 之后同一命令的 stdout（无多余尾行）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类，也没有失败路径。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_empty_drop_show_message_matches_git)'`（new；AC1–AC3）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::show_test::/) | test(/^command::cherry_pick_test::/)'`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `SA-05`, `DEP-SA-03`, `DEP-SA-06`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/show.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/show.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`（`format:` 与 `tformat:` 的结尾换行语义）、`COMPATIBILITY.md`（`show` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/show.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=show自定义format结尾换行语义; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-05,DEP-SA-03,DEP-SA-06,DEP-SA-10; writeset=序列化于 SA-05; release=independent; split-from=N/A; exception=N/A`

---

### Task SA-07h: 已撤回（不得开工）

**Status:** withdrawn。R43 按用户 2026-11-19 05:17:51 UTC 决定撤回（原：`revert --no-commit` 从含已暂存改动的索引开始的「既有行为补测」）。其前提不成立：现有 `revert --no-commit` 以 HEAD 树重建结果、从新索引中丢弃已暂存而 HEAD 中没有的条目（`src/command/revert.rs:1366-1387`），并非已具备的 Git 行为；#728 不要求该场景。该既有缺陷登记在「与其它计划的关系」的「无承接计划」行，本计划不修、不测；`SA-07a` 不改变 `--no-commit` 路径。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-07a: `revert` 已暂存改动前置拒绝（#728，`t3501#7`）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：新增 `LocalChangesWouldBeOverwritten`（`your local changes would be overwritten by revert`，`LBR-CONFLICT-001`，退出码 128）；不带 `-n/--no-commit` 时在目标解析后、任何写入前比较索引树与 HEAD 树（已暂存新增/修改/删除都拒绝），不留 revert 状态；`-n` 路径不变。AC1–AC8 由 `command::revert_test::revert_staged_changes_refused_without_state` 覆盖（退出码、人读信息、`--json`、`--machine`、索引/工作区快照、无 `revert-state.json`、已暂存修改同码）；`revert_test` 42/42、`--lib command::revert` 4/4、`compat_noninteractive_history_controls` 33/33、`compat_error_codes_doc_sync` 2/2；`docs/error-codes.md`、revert 文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 先于 `SA-07` 独立交付：为 `revert` 增加与 `cherry-pick` 同类的前置拒绝——不带 `-n`/`--no-commit` 且索引不等于 HEAD 树（含已暂存的新增、修改或删除）时，在解析目标之后、任何写入之前以 `your local changes would be overwritten by revert`（`LBR-CONFLICT-001`，退出码 128）拒绝，不写入 revert 状态，从而不阻塞后续 `checkout -b`（#728 验收项 2）。`--no-commit` 路径不在本卡改变范围（Git 允许其从不等于 HEAD 的索引开始；Libra 该路径丢弃已暂存条目的既有缺陷见「与其它计划的关系」的「无承接计划」行）。revert 冲突停止保留与 `--continue`/`--abort` 后的清理为既有行为，由共享回归组中的既有用例 `test_revert_conflict_then_continue`、`test_revert_conflict_then_abort`（`tests/command/revert_test.rs:843,990`）守护。R43 按用户 2026-11-19 05:17:51 UTC 决定收窄到 #728 的验收项：以 `t3501#7` 的「已暂存新增文件」形态做全量验收，另以「已暂存修改」形态验收拒绝不只针对新增条目；「已暂存删除」形态与 revert 停止态的完整快照专项验收登记 `DEFER-SA-18`（原 `SA-07h`、`SA-07e`、`SA-07f` 撤回）。原拆出的 `SA-07d`（冲突后 `commit -a` 默认消息，`TC-2807`）已于 R47 撤回（`DEFER-SA-21`）。
**Out of scope:** `--no-commit` 路径（不改动；其丢弃已暂存条目的既有缺陷无承接计划）；「已暂存删除」形态与 revert 停止态的完整快照专项验收（`DEFER-SA-18`）；linked worktree 中 revert 状态的路径解析（既有 `request_worktree_gitdir_strict`，本卡不改动；断言统一以 `libra rev-parse --git-dir` 定位）；detached HEAD `revert`（由 `SA-07` 承接）；冲突后 `commit -a` 默认消息与 `revert --reference`（原 `SA-07d`、`SA-07b`，R47 撤回，`DEFER-SA-21`）；`cherry-pick -`（由 `SA-07c` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `revert` 前置检查只有进行中状态、detached HEAD 与未合并索引，没有已暂存改动拒绝 | `src/command/revert.rs:450-464` |
| `cherry-pick` 已有 `LocalChangesWouldBeOverwritten`（`ConflictUnresolved`/`LBR-CONFLICT-001`） | `src/command/cherry_pick.rs:167-168,224,1633-1642`; `src/utils/error.rs:380` |
| revert 停止态持久化为 `<gitdir>/revert-state.json` | `src/command/revert.rs:907,977-985` |
| `revert` 已有 `-n`/`--no-commit`（单提交）；Git 允许其从不等于 HEAD 的索引开始 | `src/command/revert.rs:315,347,466-472,892`; Git 源码 `Documentation/git-revert.adoc`（`--no-commit` 段） |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；两份 fixture 都在分支 HEAD（其提交只修改已跟踪文件 `a.txt`）上准备，工作区另有一个字节独特的未跟踪文件 `notes.txt`：「新增」——`libra add` 一个新文件 `extra_file`（`t3501#7` 形态）；「修改」——修改另一个已跟踪文件 `b.txt` 并 `libra add`；然后运行 `libra revert HEAD`；「新增」fixture 运行前记录 `libra ls-files --stage` 输出与工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）作为「命令前快照」；「默认运行」指不加 `--json`/`--machine` 的 `libra revert HEAD`）

- [ ] AC1：「新增」fixture 默认运行的退出码为 128（代表路径的退出码）。
- [ ] AC2：「新增」fixture 默认运行的人读 stderr含 `your local changes would be overwritten by revert`（代表路径的人读输出：拒绝原因是本地改动）。
- [ ] AC3：代表路径的 `--json`：在「新增」fixture 的独立副本上运行 `libra --json revert HEAD`，结果（进程退出状态，stderr 整体解析出的 JSON 对象的（`ok`、`error_code`、`exit_code`）三元组）等于（128，（`false`、`LBR-CONFLICT-001`、128））（一次比较；stderr 不能整体解析为单个 JSON 对象即不成立）。
- [ ] AC4：代表路径的 `--machine`：在「新增」fixture 的另一份独立副本上运行 `libra --machine revert HEAD`，结果（进程退出状态，stderr 按行解析出的（`ok`、`error_code`、`exit_code`）三元组序列）等于（128，[（`false`、`LBR-CONFLICT-001`、128）]）（一次比较；任一行不能解析为 JSON 对象或行数不为 1 即不成立）。
- [ ] AC5：「新增」fixture 默认运行之后 `libra ls-files --stage` 的输出等于命令前快照（索引未被改写，`extra_file` 仍已暂存）。
- [ ] AC6：「新增」fixture 默认运行之后工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）等于命令前快照（`extra_file`、`notes.txt` 与已跟踪文件均未被改写或删除）。
- [ ] AC7：「新增」fixture 默认运行之后 `"$(libra rev-parse --git-dir)/revert-state.json"` 不存在（不留阻塞后续 `checkout -b` 的状态）。
- [ ] AC8：「修改」fixture 命令的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（拒绝不只针对新增条目；该 fixture 不加拒绝时 revert 会成功）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。另含第 1e 步：`docs/error-codes.md` 在 `LBR-CONFLICT-001` 下写明 `revert` 已暂存改动的前置拒绝。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡新增 1 个错误类：E1「revert 已暂存改动前置拒绝」（`your local changes would be overwritten by revert`，`LBR-CONFLICT-001`，退出码 128）。代表路径：「新增」fixture 的 `libra revert HEAD`；四项判据：退出码 AC1、人读输出 AC2、`--json` AC3、`--machine` AC4。其余路径：「修改」fixture 断言稳定码（AC8）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::revert_test::revert_staged_changes_refused_without_state)'`（new；AC1–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::revert_test::/)'`（含守护既有停止态保留与清理的 `test_revert_conflict_then_continue`、`test_revert_conflict_then_abort`）；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::revert::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls`；`source .env.test && cargo nextest run --no-tests=fail --test compat_error_codes_doc_sync`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）；修改 docs/error-codes.md（LBR-CONFLICT-001 新增用途）`
**Dependencies:** `SA-06`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/revert.rs`, `tests/command/revert_test.rs`, `docs/error-codes.md`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/revert.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/error-codes.md`（`LBR-CONFLICT-001` 增列 `revert` 已暂存改动前置拒绝）、`docs/commands/revert.md`、`docs/commands/zh-CN/revert.md`、`docs/development/commands/revert.md`（已暂存改动拒绝）、`COMPATIBILITY.md`（`revert` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/revert.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=revert已暂存改动前置拒绝; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-06,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-06; release=independent; split-from=SA-07; exception=N/A`

---

### Task SA-07e: 已撤回（不得开工）

**Status:** withdrawn。R43 按用户 2026-11-19 05:17:51 UTC 决定撤回（原：revert 冲突停止保留与 `--continue`/无冲突完成后的状态清理）。这些是既有行为，已由既有用例 `test_revert_conflict_then_continue`（`tests/command/revert_test.rs:843`）守护，列入 `SA-07a`、`SA-07` 的共享回归组（`command::revert_test::` 前缀）；#728 的残留状态来源是 detached HEAD 拒绝与已暂存改动拒绝（由 `SA-07`、`SA-07a` 修复）。初次停止的未合并条目与完整快照的专项验收登记 `DEFER-SA-18`。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-07f: 已撤回（不得开工）

**Status:** withdrawn。R43 按用户 2026-11-19 05:17:51 UTC 决定撤回（原：`revert --abort` 复原 HEAD、索引与工作区并清理停止态）。这是既有行为（`restore_to_orig_head`），已由既有用例 `test_revert_conflict_then_abort`（`tests/command/revert_test.rs:990`）守护，列入 `SA-07a`、`SA-07` 的共享回归组；完整快照的专项验收登记 `DEFER-SA-18`。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-07: detached HEAD 上的 `revert`（#728）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：移除 detached HEAD 拒绝（`RevertError::DetachedHead` 变体、映射与单测断言一并删除）；`update_head` 在 `Head::Detached` 时用 `Head::update_result(Head::Detached(oid))` 提交到 detached 引用，分支路径不变；冲突后的 `--continue`/`--abort` 沿用既有 `restore_to_orig_head`（detached 专项验收登记 `DEFER-SA-12`）。AC1–AC4 由 `revert_detached_head_no_edit_tc2634` 覆盖（exit 0、父提交为原 HEAD、树与 Git `git revert` 一致、`--abbrev-ref` 仍为 `HEAD`），AC5–AC6 由 `revert_detached_staged_changes_refused_for_local_changes` 覆盖（`LBR-CONFLICT-001` + 本地改动信息），AC7–AC8 由 `revert_t3501_orphan_detach_leaves_no_state` 覆盖（`switch --orphan` 成功、无 `revert-state.json`）；`revert_test`+`cherry_pick_test` 135/135、`--lib command::revert` 4/4、`compat_noninteractive_history_controls` 33/33；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 在 `SA-07a` 就绪后，删除 `revert` 对 `Head::Detached(_)` 的拒绝（含模块私有枚举变体 `RevertError::DetachedHead` 及其映射与单测断言；变体若残留而不再构造，会被 ER-04 的 `cargo clippy … -D warnings`（`dead_code`）拒绝），使 detached HEAD 上的 `revert`（含 `--no-edit`）按 Git 提交到 detached HEAD；detached HEAD 上冲突后的 `--continue`/`--abort` 沿用分支上的同一既有路径（`restore_to_orig_head` 经 `update_head` 写回原 OID；分支上由共享回归组中的既有用例 `test_revert_conflict_then_continue`、`test_revert_conflict_then_abort` 守护），其 detached 专项验收登记 `DEFER-SA-12`（#728 的 TC 不含冲突场景）；闭环 #728 验收项 1（`cherry-pick` 一半已由 #528 交付，以共享回归组中的既有用例 `cherry_pick_on_detached_head_moves_the_detached_ref` 佐证，不计 AC）、2、3（orphan 切换之后的 unborn 上非快进 `cherry-pick` 与随后的 `checkout --detach` 过渡（`t3501#8–#9`，其起点依赖前者生成的提交）登记 `DEFER-SA-17`；验收项 4 的 `checkout -b side-branch` 由 `SA-07c` 验收）。已拆出 `SA-07a`、`SA-07c`；原拆出的 `SA-07b`（`--reference`）与 `SA-07a` 拆出的 `SA-07d`（冲突后 `commit -a` 默认消息）已于 R47 撤回（`DEFER-SA-21`）。
**Out of scope:** 已暂存改动拒绝（由 `SA-07a` 承接）；revert 停止态保留与 `--continue`/`--abort` 后的清理（既有行为，由共享回归组的既有用例守护；完整快照专项验收登记 `DEFER-SA-18`）；detached HEAD 上冲突后的 `--continue`/`--abort` 专项验收（`DEFER-SA-12`）；`commit -a` 默认消息与 `--reference`（原 `SA-07d`、`SA-07b`，R47 撤回，`DEFER-SA-21`）；`cherry-pick -`（由 `SA-07c` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| detached HEAD 时返回 `RevertError::DetachedHead`；`RevertError` 为模块私有枚举 | `src/command/revert.rs:61,65-66,223,265,454-456,2080,2160` |
| detached HEAD `cherry-pick` 已工作且有回归用例 | `src/command/cherry_pick.rs:940-950`; `tests/command/cherry_pick_test.rs:4700` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；AC7、AC8 的 fixture（`t3501#4–#8` 在 detached HEAD 上的序列形态）：在 detached HEAD 上先运行 `libra revert --no-edit HEAD`（成功），再 `libra add` 一个新文件 `extra_file` 后运行 `libra revert HEAD`（被已暂存改动拒绝），然后 `libra rm --cached extra_file` 并删除该文件，再运行 `libra switch --orphan unborn`（`t3501#8` 的首步；其后 unborn 分支上的非快进 `cherry-pick` 属 `DEFER-SA-17`））

- [ ] AC1：detached HEAD 上 `libra revert --no-edit HEAD`（`t3501#15`–`#17` 共用前提，`TC-2634`/`2635`/`2636`）exit 0。
- [ ] AC2：AC1 之后 HEAD 的父提交等于原 HEAD。
- [ ] AC3：AC1 之后 HEAD 的树等于 Git 在同一 fixture 的 detached HEAD 上 `git revert --no-edit HEAD` 所得提交的树（「Git 对照」按「验证执行协议」第 3 条）。
- [ ] AC4：AC1 之后 `libra rev-parse --abbrev-ref HEAD` 输出 `HEAD`（仍为 detached）。
- [ ] AC5：detached HEAD 且索引含已暂存改动时 `libra revert HEAD`（`t3501#7` 原形态）的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（拒绝来自已暂存改动检查，而非已删除的 detached HEAD 拒绝 `LBR-REPO-003`）。
- [ ] AC6：AC5 的人读 stderr含 `your local changes would be overwritten by revert`（#728 验收项 2：拒绝原因是本地改动而非 detached HEAD）。
- [ ] AC7：AC7、AC8 的 fixture 中的 `libra switch --orphan unborn` exit 0（前序 revert 与拒绝未留下阻塞分支切换的状态）。
- [ ] AC8：AC7 之后 `"$(libra rev-parse --git-dir)/revert-state.json"` 不存在（序列不留 revert 停止态）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类（本卡删除 detached HEAD 拒绝）。detached HEAD 且索引含已暂存改动的 `libra revert HEAD` 现在命中 `SA-07a` 交付的错误类「revert 已暂存改动前置拒绝」（本卡不改变其稳定码、退出码与诊断；代表路径与四项判据在 `SA-07a`），按「验证执行协议」第 11c 条作为其余路径断言稳定码（AC5）；#728 验收项 2 明确要求该拒绝「原因正确」，故加 1 条人读诊断（AC6）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::revert_test::revert_detached_head_no_edit_tc2634)'`（new；AC1–AC4）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::revert_test::revert_detached_staged_changes_refused_for_local_changes)'`（new；AC5、AC6）
- [ ] VER3 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::revert_test::revert_t3501_orphan_detach_leaves_no_state)'`（new；AC7、AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::revert_test::/) | test(/^command::cherry_pick_test::/)'`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::revert::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）（RevertError 为模块私有枚举，删除变体不属 T-3）`
**Dependencies:** `SA-07a`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/revert.rs`, `tests/command/revert_test.rs`, `docs/commands/revert.md`, `docs/commands/zh-CN/revert.md`, `docs/development/commands/revert.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/revert.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/revert.md`、`docs/commands/zh-CN/revert.md`、`docs/development/commands/revert.md`（detached HEAD 支持）、`COMPATIBILITY.md`（`revert` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/revert.en.md`。`docs/error-codes.md`：`N/A`（`LBR-REPO-003` 仍被其它变体使用，删除私有变体 `RevertError::DetachedHead` 不改码表）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=detached-HEAD上的revert; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=SA-07a,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-07a; release=independent; split-from=N/A; exception=N/A`

---

### Task SA-07d: 已撤回（不得开工）

**Status:** withdrawn。R47 按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回（原：revert 冲突后 `GIT_EDITOR=true libra commit -a` 复用默认 revert 消息，`t7512#42`、`TC-2807`）。#728 的验收项不含该场景，登记 `DEFER-SA-21`；本计划因此不再写 `src/command/commit.rs`，其唯一外部前置 `DEP-SA-05` 不再约束任何卡。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-07b: 已撤回（不得开工）

**Status:** withdrawn。R47 按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回（原：`libra revert --reference`，`t3501#18`、`TC-2637`）。#728 的验收项不含 `--reference`，登记 `DEFER-SA-21`。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-07c: `libra cherry-pick -` 与 `-s -`（#728 验收项 4、5 的核心断言，`t3501#12` 形态）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：仅当第一个修订位置参数恰为 `-` 时改写为上一次检出目标（复用 `switch::resolve_previous_checkout_target` 并直接取其 commit），其余位置仍按普通修订解析；`-s -` 组合生效；导航记录错误原样透传（不可用 `LBR-CLI-003`、读取失败 `LBR-IO-001`、损坏 `LBR-REPO-002`，后两者登记 `DEFER-SA-23`）。AC1–AC3/AC6 由 `cherry_pick_previous_branch_shorthand` 覆盖（`-` 与具名分支同树、无导航记录 `LBR-CLI-003`、非首位 `-` `LBR-CLI-003`）；AC4–AC5 由 `cherry_pick_t3501_12_signoff_dash_tc0484` 覆盖（`checkout -b` 成功、`-s -` 结果与 Git 一致含签名尾注）；`cherry_pick_test` 92/92、`compat_noninteractive_history_controls` 33/33；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计，故未执行计划要求的独立评审门。
**Description:** 自 `SA-07` 拆出：`cherry-pick` 只把**第一个**修订位置参数恰为 `-` 时解析为上一次检出目标（与 Git 一致：`builtin/revert.c` 的 `parse_args` 只把选项之后的 `argv[1]` 的 `-` 改写为 `@{-1}`；复用 `switch.rs` 的 `resolve_previous_checkout_target`，不复制 reflog 逻辑），其余位置的 `-` 按普通修订解析；`-s`/`--signoff` 在前时同样生效；以 `t3501#12`（`TC-0484`）形态的端到端用例验收 #728 验收项 4、5 的核心断言（`checkout -b side-branch` 成功、`cherry-pick -s -` 的结果与 Git 对照一致）；#728 验收项 6（上游 `t3501#12` 全量与 `TC-0484` 通过）登记 `DEFER-SA-13`。`resolve_previous_checkout_target` 返回的错误原样透传（不得折叠为 `LBR-CLI-003`）：上一检出导航记录读取失败为 `LBR-IO-001`（`previous_checkout_read_error`，`src/command/switch.rs:445-454`），记录损坏或目标无效为 `LBR-REPO-002`（`previous_checkout_invalid_error`，`src/command/switch.rs:456-465`），两条路径的专项验收（需构造不可读或损坏的导航记录）登记 `DEFER-SA-23`。
**Out of scope:** `revert -`（Git 经同一 `parse_args` 支持，#728 只要求 `cherry-pick -`，登记 `DEFER-SA-10`）；`A..B` 中的 `-`（永久非目标：Git 不接受）；上一检出导航记录不可读或损坏时的错误透传专项验收（`DEFER-SA-23`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_previous_checkout_target` 基于 reflog 导航返回上一检出目标 | `src/command/switch.rs:470-476` |
| `cherry-pick` 逐参数 `resolve_commit`，`-` 无特殊处理 | `src/command/cherry_pick.rs:1077-1083,3036-3038` |
| Git 的 `revert`/`cherry-pick` 共用 `parse_args`，只把第一个修订参数 `-` 改写为 `@{-1}` | Git 源码 `builtin/revert.c`（`parse_args`：`if (!strcmp(argv[1], "-")) argv[1] = "@{-1}";`） |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项）

- [ ] AC1：在 `main` 上、上一次检出为 `side` 时，`libra cherry-pick -` 生成的提交树等于 `libra cherry-pick side` 在同一起点生成的提交树。
- [ ] AC2：`libra cherry-pick -s -` 生成的提交消息字节等于 Git 对照（含 `Signed-off-by:` 尾注）。
- [ ] AC3：仓库无上一次检出记录时 `libra cherry-pick -` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CLI-003`（复用 `resolve_previous_checkout_target` 的 `previous_checkout_unavailable_error`，`src/command/switch.rs:437-443`；panic 不产生结构化错误报告）。
- [ ] AC4：`t3501#12`（`TC-0484`）等价端到端中的 `libra checkout -b side-branch` exit 0。
- [ ] AC5：AC4 之后同一端到端（`cherry-pick -s -`）的结果文件内容等于 Git 对照。
- [ ] AC6：上一次检出为 `side` 时 `libra cherry-pick <X> -`（`-` 不在第一个修订位置）的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CLI-003`（`-` 按普通修订解析失败，与 Git 的 `bad revision` 一致；若把它当作上一检出目标，命令会成功而无此报告）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类。无上一检出记录时命中既有错误类「上一检出目标不可用」（`previous_checkout_unavailable_error`，`LBR-CLI-003`，`src/command/switch.rs:437-443`；本卡复用、不改其稳定码、退出码与诊断模板），非首位 `-` 命中既有错误类「无效修订」（`CherryPickError::InvalidCommit`，`LBR-CLI-003`，`src/command/cherry_pick.rs:245-247`）；按「验证执行协议」第 11c 条作为其余路径各断言稳定码（AC3、AC6）；#728 未要求这两条路径的人读诊断。另有两条新可达的既有错误类路径——导航记录读取失败（`LBR-IO-001`）与记录损坏（`LBR-REPO-002`）——原样透传，其验收登记 `DEFER-SA-23`（第 11a 条：已登记 DEFER 的路径不在本卡验收范围）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_previous_branch_shorthand)'`（new；AC1–AC3、AC6）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::cherry_pick_test::cherry_pick_t3501_12_signoff_dash_tc0484)'`（new；AC4、AC5）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::cherry_pick_test::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `SA-07`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cherry_pick.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/cherry-pick.md`、`docs/commands/zh-CN/cherry-pick.md`、`docs/development/commands/cherry-pick.md`（`-` 简写）、`COMPATIBILITY.md`（`cherry-pick` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=cherry-pick上一检出简写; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=SA-07,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-07; release=independent; split-from=SA-07; exception=N/A`

---

### Task SA-08a: `rebase` 重放签名单一出口：`pick` 接入、OpenPGP 签名与出口级签名失败（#780 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：新增唯一签名出口 `finalize_replayed_commit(UnsignedReplayContent)`——输入经 `parse_commit_msg` 剥离旧 `gpgsig`，签名策略复用 `commit_signing_policy`（`commit.gpgSign` 优先，未设时 `vault_sign_commit` 检查 `vault.signing`，都未设则不签名）；普通 `pick`（不含 `--signoff`）迁入该出口，签名失败返回错误且调用方在 `Ok` 之前不写对象；其余动作保持原样。AC1/AC5/AC7 由 `rebase_pick_{signing_disabled_drops_gpgsig,default_no_signing,commit_config_wins_over_vault}` 覆盖（`cat-file -p` 无 `gpgsig`），AC2/AC6 由 `rebase_pick_{vault_signing_verifies,inherit_vault_signing_verifies}` 覆盖（`merge --ff-only --verify-signatures` exit 0），AC3/AC4 由库内 `signing_exit_tests::finalize_replayed_commit_without_unseal_key_writes_nothing` 覆盖（错误文本 + 对象数不变，已登记 `tests/SRC_SERIAL_REGISTRY.tsv`）；`rebase_test` 91/91、`--lib command::rebase` 20/20、`compat_serial_registry` 26/26。偏差（记录）：①用户已取消独立 Claude/Codex 审计；②`DEP-SA-01`（`REL-CS-00`）未合入，当前 `libra init` 仍写 `vault.signing=true`，AC5 用例在建仓后显式清除 `commit.gpgSign` 与 `vault.signing` 以验证「都未配置→不签名」；③网站仅生成本地草稿（未提交、未推送），由 `REL-SA-01` 执行。
**Description:** 自 `SA-08` 拆出（`REL-SA-01` 家族子卡 1/6，只做本地签名+DCO 提交，不 bump、不推送、不发布）：按 `ADR-SA-03` 新增唯一签名出口 `finalize_replayed_commit`（输入为不含签名的 `UnsignedReplayContent`，其构造函数剥离消息中的 `gpgsig`；签名决策只调用 `libra commit` 的既有入口：`commit.gpgSign` 优先，未设时按显式配置的 `vault.signing=true`，两者都未设时不签名——`DEP-SA-01` 交付的默认），并把 `create_replayed_commit` 的 `Pick` 分支（不含 `--signoff` 路径）迁入该出口。出口级失败处理：签名失败时出口返回错误且不写任何对象。行为边界：重放中签名失败（含签名不可用）的冲突同构停止、重试、`--continue` 与 `--abort` 由 `SA-08j` 交付。已拆出 `SA-08b`（`reword`、`edit`、`fixup`、`squash`）、`SA-08m`（`amend!`、`fixup -C`、`fixup -c`）、`SA-08g`（`gpg.format=ssh`）、`SA-08h`（`--signoff` 尾注、交互式消息编辑与 `edit` 停止后的修订先于签名）与 `SA-08j`，六卡只在 `REL-SA-01` 合并发布（中间状态不对外发布）；`-f` 由 `SA-08` 交付。
**Out of scope:** `Reword`/`Edit`/`Fixup`/`Squash`（由 `SA-08b` 承接）；`Amend`/`FixupKeep`/`FixupKeepEdit`（由 `SA-08m` 承接）；`gpg.format=ssh`（由 `SA-08g` 承接）；`--signoff` 路径、交互式消息编辑与 `edit` 停止后的修订（由 `SA-08h` 承接）；重放中签名失败的停止态与恢复（由 `SA-08j` 承接）；`-f`/`--force-rebase`（由 `SA-08` 承接）；`--show-current-patch`、`backup-git-rebase-todo`（`DEFER-SA-03`）；`-r`、`-s recursive`（`DEFER-SA-02`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `Pick` 分支复制原消息（含旧 `gpgsig`），不重签 | `src/command/rebase.rs:5861-5867` |
| 库内用例的临时仓库、cwd 与 keyed serial lane 隔离约定 | `CLAUDE.md`「Testing Guidelines」; `tests/SERIAL_CLASSIFY.sh:32-33`; `tests/compat/serial_registry.rs:2253-2300`（`tests/SRC_SERIAL_REGISTRY.tsv`） |
| 「已是最新」短路的完整条件：非 `--root`、无 `--onto`、`base_id == upstream_id`（上游是 HEAD 的祖先）、未给 `--autosquash`，且重放列表未被无关历史根重放预先填充 | `src/command/rebase.rs:3690-3698` |
| 签名决策：`commit_signing_policy` 读 `commit.gpgSign` 返回 `Force`/`Disable`/`InheritVault`（`InheritVault` 时按 `vault.signing`），`libra commit` 在 `commit.rs:1148` 调用；vault 签名与无 unseal key 时的 `vault signing enabled but no unseal key found`；`libra init --vault false` 跳过 vault | `src/command/history_config.rs:71-82`; `src/command/commit.rs:1148,3088-3150`; `src/command/init.rs:393-395` |
| `--verify-signatures` 校验被合并 tip 的签名 | `src/command/merge.rs:1630-1647` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture（全部在隔离的 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：AC1 以默认 `libra init`（含 vault）建仓，`commit.gpgSign=true` 下在 `main` 上建基点 `B`（含已跟踪文件 `f.txt`、`g.txt`、`h.txt`）；从 `B` 建分支 `topic` 并提交恰好 1 个只修改 `f.txt` 的提交，记 `T_SHA="$(libra rev-parse HEAD)"`；切回 `main` 提交只修改 `u.txt` 的 `<upstream>`，再切回 `topic`（`topic` 与 `<upstream>` 分叉于 `B`，`<upstream>` 不是 `topic` 的祖先，rebase 必须重放该提交，不走「已是最新」短路）（待重放提交带 vault 签名），再设 `commit.gpgSign=false` 后在 `topic` 上运行 `libra rebase <upstream>`；AC2 同样以默认 `libra init` 建仓，但在 `commit.gpgSign=false` 下在 `main` 上建基点 `B`（含已跟踪文件 `f.txt`、`g.txt`、`h.txt`）；从 `B` 建分支 `topic` 并提交恰好 1 个只修改 `f.txt` 的提交，记 `T_SHA="$(libra rev-parse HEAD)"`；切回 `main` 提交只修改 `u.txt` 的 `<upstream>`，再切回 `topic`（`topic` 与 `<upstream>` 分叉于 `B`，`<upstream>` 不是 `topic` 的祖先，rebase 必须重放该提交，不走「已是最新」短路）（旧提交无签名，只有新生成的重放结果可能带有效签名），再设 `commit.gpgSign=true` 后运行 `libra rebase <upstream>`；AC5 以与 AC1 同形的 fixture（待重放提交带 vault 签名），但重放前不设置 `commit.gpgSign` 与 `vault.signing`（`DEP-SA-01` 交付后 `libra init` 不写 `vault.signing`；运行前以 `libra config get` 确认两者均未配置）后运行 `libra rebase <upstream>`；AC6、AC7 以同形 fixture（AC6 旧提交无签名）分别在「`commit.gpgSign` 未设、`vault.signing=true`」与「`commit.gpgSign=false`、`vault.signing=true`」下运行 `libra rebase <upstream>`；AC3、AC4 为库内单元用例（`src/command/rebase.rs` 的 `mod tests`）：以 `tempfile::tempdir()` 建临时仓库、`utils::test::ChangeDirGuard` 切换 cwd，隔离 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB`，用例标注 `#[serial(cwd, env)]`（`src/` 下的用例按仓库约定登记到 `tests/SRC_SERIAL_REGISTRY.tsv`：列为 `file`、`fn`、`lane`、`reason`，lane 取 `lane:cwd+env`；守卫 `compat_serial_registry` 的 `src_serial_registry_matches_src_classifier` 要求该行的用例出现在 `SERIAL_CLASSIFY_TREE=src sh tests/SERIAL_CLASSIFY.sh` 的输出中。`src/` 用例不进入 `tests/SERIAL_REGISTRY.tsv`、不改 `.config/nextest.toml`（`tests/SERIAL_CLASSIFY.sh:32-33`）），在该 `libra init --vault false` 且 `commit.gpgSign=true` 的仓库中直接调用 `finalize_replayed_commit`）

- [ ] AC1：`commit.gpgSign=false` 时 `pick` 重放结果的 `libra cat-file -p` 输出不含 `gpgsig` 行。
- [ ] AC2：vault OpenPGP 且 `commit.gpgSign=true` 时：在以 `<upstream>` 为起点的临时分支上 `libra merge --ff-only --verify-signatures topic` exit 0（`topic` 必须是 `<upstream>` 的后代，且其 tip 带有效签名）。
- [ ] AC3：签名不可用时 `finalize_replayed_commit` 返回错误，错误文本含 `vault signing enabled but no unseal key found`。
- [ ] AC4：AC3 的调用前后对象库（`.libra/objects` 下的松散对象与 pack 索引所列对象）没有新增对象。
- [ ] AC5：`commit.gpgSign` 与 `vault.signing` 都未设置时（`DEP-SA-01` 交付的默认）`pick` 重放结果的 `libra cat-file -p` 输出不含 `gpgsig` 行。
- [ ] AC6：`commit.gpgSign` 未设置、`vault.signing=true` 时 `pick` 重放结果：在以 `<upstream>` 为起点的临时分支上 `libra merge --ff-only --verify-signatures topic` exit 0（`topic` 必须是 `<upstream>` 的后代，且其 tip 带有效签名）。
- [ ] AC7：`commit.gpgSign=false` 且 `vault.signing=true` 时 `pick` 重放结果的 `libra cat-file -p` 输出不含 `gpgsig` 行（`commit.gpgSign` 优先）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-01` 执行。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的 CLI 错误类：AC3 判定的是库内出口 `finalize_replayed_commit` 的返回错误；该失败在 CLI 层的错误类「rebase 重放签名失败停止」（`LBR-AUTH-001`）及其代表路径由 `SA-08j` 交付。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_pick_unsigned_strips_stale_gpgsig)'`（new；AC1、AC5、AC7）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_pick_signs_openpgp)'`（new；AC2、AC6）
- [ ] VER3 `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=command::rebase::tests::finalize_replayed_commit_sign_failure_writes_nothing)'`（new；AC3、AC4）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::rebase_test::/) | test(/^command::rebase_interactive_test::/)'`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::rebase::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls --test compat_sequencer_message_author`；`source .env.test && cargo nextest run --no-tests=fail --test compat_serial_registry`。
**D 组:** 继承 `REL-SA-01` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；签名安全边界变更由 REL-SA-01 的 T-2 全量覆盖）`
**Dependencies:** `SA-07c`, `DEP-SA-02`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rebase.rs`, `tests/command/rebase_test.rs`, `tests/SRC_SERIAL_REGISTRY.tsv`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`（签名决策只调用 `commit.rs` 既有入口，不修改 `commit.rs`）
**Docs and compatibility impact:** `docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`（重放签名语义）、`COMPATIBILITY.md`（`rebase` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`（本地草稿，由 `REL-SA-01` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-01` 的 `immutable-release` 前滚）
**Security and privacy:** 签名只经单一出口；出口级签名失败不写提交对象；测试密钥只在临时目录生成，证据不记录私钥、口令或签名原文。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-01`（1/6）
**C/D coverage from:** `REL-SA-01`
**Granularity:** `type=implementation; axis=rebase签名出口与pick签名; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=7/8; VER=3/8; landing=1; prod-files=1; scope=M; deps=SA-07c,DEP-SA-02,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-07c; release=family child; split-from=SA-08; exception=N/A`

---

### Task SA-08b: `rebase` 的 `reword`、`edit`、`fixup`、`squash` 迁入签名出口（#780 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-23：`reword`、`edit`、`fixup`、`squash` 四个分支迁入 `finalize_replayed_commit`——先构造不含签名的内容，再由出口按 `commit.gpgSign`/`vault.signing` 决定签名；关闭签名时不残留来源/目标提交的旧 `gpgsig`，开启时带新有效签名；折叠语义不变。AC1/AC2 由 `rebase_replay_reword_edit_unsigned_strip_stale_gpgsig` 覆盖，AC3/AC4 由 `rebase_replay_fixup_squash_unsigned_strip_stale_gpgsig` 覆盖（`cat-file -p` 无 `gpgsig`），AC5/AC6 由 `rebase_replay_reword_edit_signs_openpgp` 覆盖，AC7/AC8 由 `rebase_replay_fixup_squash_signs_openpgp` 覆盖（`merge --ff-only --verify-signatures` exit 0）；`rebase_test`+`rebase_interactive_test` 101/101、`compat_noninteractive_history_controls`+`compat_sequencer_message_author` 35/35。偏差（记录）：①用户已取消独立 Claude/Codex 审计；②按用户 2026-11-23 指令逐卡提交并推送（卡内「不推送」按本次指令执行提交+推送，发布仍由 `REL-SA-01` 统一）；③本卡 `Version increment: N/A`，未 bump；④网站仅生成本地草稿。
**Description:** 自 `SA-08a` 拆出（`REL-SA-01` 家族子卡 2/6，只做本地签名+DCO 提交；R38 起其余 3 个折叠 action 拆到 `SA-08m`）：把 `create_replayed_commit` 的 `Reword`、`Edit`、`Fixup`、`Squash` 四个分支迁入 `SA-08a` 的 `finalize_replayed_commit`（与 `pick` 相同：先构造不含签名的内容、再经出口签名或不签名），使这四个 action 的提交只能经该出口产生：签名关闭时结果不残留来源/目标提交的旧 `gpgsig`，签名开启时结果带有效的新签名。本卡不改变折叠语义（提交数、父提交、树、消息、作者）；其回归由共享回归组中的既有用例守护（`tests/command/rebase_test.rs:648` `test_rebase_autosquash_folds_fixup_commit`、`:7469` `test_rebase_autosquash_keeps_target_author_and_current_committer`，`tests/command/rebase_interactive_test.rs:528` `test_rebase_i_message_commands_matrix`），每卡执行时须全绿。
**Out of scope:** `Pick`、出口本身、OpenPGP 签名与出口级签名失败（由 `SA-08a` 承接）；`amend!`、`fixup -C`、`fixup -c`（由 `SA-08m` 承接）；`gpg.format=ssh`（由 `SA-08g` 承接）；`--signoff` 尾注、交互式消息编辑与 `edit` 停止后的修订（由 `SA-08h` 承接）；签名失败停止与恢复（由 `SA-08j` 承接）；`-f`（由 `SA-08` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Reword/Edit 复制原消息（含旧 `gpgsig`）、Fixup 复制目标消息（含目标 `gpgsig`）、Squash 剥离但不重签 | `src/command/rebase.rs:5868-5892,5905-5911` |
| `--autosquash` 由消息前缀生成 `fixup!`/`squash!`/`amend!` 对应的 action | `src/command/rebase.rs:2526-2535` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture（在隔离的 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：每条 AC 各用一份独立的同形 fixture——以默认 `libra init`（含 vault）建仓，`commit.gpgSign=true` 下在 `main` 上建基点 `B`，从 `B` 建分支 `topic` 并在其上建出被重放、被折叠与折叠目标提交（均带 vault 签名，即带旧 `gpgsig`），且该 AC 所测 action 的结果是 rebase 之后 `topic` 的 tip；切回 `main` 提交只修改 `u.txt` 的 `<upstream>`（`topic` 与 `<upstream>` 分叉于 `B`，每个被重放或折叠的提交都获得新父提交、必经签名出口）；再按该 AC 设置 `commit.gpgSign` 后在 `topic` 上以 `<upstream>` 为目标 rebase；`reword`、`edit`、`fixup -C`、`fixup -c` 经 `GIT_SEQUENCE_EDITOR` 改 todo 后运行 `libra rebase -i <upstream>`（`reword`/`fixup -c` 以 `GIT_EDITOR=true` 保留消息，`edit` 停止后不改动直接 `libra rebase --continue`）；`fixup`/`squash`/`amend!` 经 `fixup!`/`squash!`/`amend!` 提交以 `libra rebase --autosquash <upstream>` 生成；「签名有效」指在以 `<upstream>` 为起点的临时分支上 `libra merge --ff-only --verify-signatures topic` exit 0（`topic` 必须是 `<upstream>` 的后代，且其 tip 带有效签名））

- [ ] AC1：`commit.gpgSign=false` 时 `reword` 重放结果的 `libra cat-file -p` 输出不含 `gpgsig` 行。
- [ ] AC2：`commit.gpgSign=false` 时 `edit` 重放结果不含 `gpgsig` 行。
- [ ] AC3：`commit.gpgSign=false` 时 `fixup` 折叠结果不含 `gpgsig` 行。
- [ ] AC4：`commit.gpgSign=false` 时 `squash` 折叠结果不含 `gpgsig` 行。
- [ ] AC5：vault OpenPGP、`commit.gpgSign=true` 时 `reword` 重放结果签名有效。
- [ ] AC6：vault OpenPGP、`commit.gpgSign=true` 时 `edit` 重放结果签名有效。
- [ ] AC7：vault OpenPGP、`commit.gpgSign=true` 时 `fixup` 折叠结果签名有效。
- [ ] AC8：vault OpenPGP、`commit.gpgSign=true` 时 `squash` 折叠结果签名有效。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-01` 执行。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的 CLI 错误类；重放签名失败的 CLI 错误类「rebase 重放签名失败停止」及其代表路径由 `SA-08j` 交付。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_reword_edit_unsigned_strip_stale_gpgsig)'`（new；AC1、AC2）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_fixup_squash_unsigned_strip_stale_gpgsig)'`（new；AC3、AC4）
- [ ] VER3 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_reword_edit_signs_openpgp)'`（new；AC5、AC6）
- [ ] VER4 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_fixup_squash_signs_openpgp)'`（new；AC7、AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::rebase_test::/) | test(/^command::rebase_interactive_test::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls --test compat_sequencer_message_author`。
**D 组:** 继承 `REL-SA-01` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；签名安全边界变更由 REL-SA-01 的 T-2 全量覆盖）`
**Dependencies:** `SA-08a`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rebase.rs`, `tests/command/rebase_test.rs`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`（`reword`、`edit`、`fixup`、`squash` 的签名语义）、`COMPATIBILITY.md`（`rebase` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`（本地草稿，由 `REL-SA-01` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-01` 的 `immutable-release` 前滚）
**Security and privacy:** 签名只经单一出口；测试密钥只在临时目录生成，证据不记录私钥、口令或签名原文。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-01`（2/6）
**C/D coverage from:** `REL-SA-01`
**Granularity:** `type=implementation; axis=rebase的reword/edit/fixup/squash签名出口; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=SA-08a,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-08a; release=family child; split-from=SA-08a; exception=N/A`

---

### Task SA-08m: `rebase` 的 `amend!`、`fixup -C`、`fixup -c` 迁入签名出口（#780 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `SA-08b` 拆出（`REL-SA-01` 家族子卡 3/6，只做本地签名+DCO 提交）：把 `create_replayed_commit` 的 `Amend`、`FixupKeep`（`fixup -C`）、`FixupKeepEdit`（`fixup -c`）三个折叠分支迁入 `finalize_replayed_commit`，删除 `amend_replacement_message` 对旧签名的重附；至此 `create_replayed_commit` 的 8 个 action 都只经该出口产生提交。本卡不改变折叠语义，其回归由共享回归组中的既有用例守护（`tests/command/rebase_test.rs:1387` `test_rebase_autosquash_amend_replaces_target_message`，`tests/command/rebase_interactive_test.rs:528` `test_rebase_i_message_commands_matrix` 的 `fixup -C`/`fixup -c` 段 `:632-659`）。
**Out of scope:** `Pick`、`reword`、`edit`、`fixup`、`squash`（由 `SA-08a`、`SA-08b` 承接）；`gpg.format=ssh`（由 `SA-08g` 承接）；重放后改写（由 `SA-08h` 承接）；签名失败停止与恢复（由 `SA-08j` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Amend 经 `amend_replacement_message` 重附旧签名；FixupKeep/FixupKeepEdit 剥离但不重签 | `src/command/rebase.rs:5893-5904,5912-5923,5950-5964` |
| `--autosquash` 由 `amend!` 前缀生成 `Amend` action | `src/command/rebase.rs:2526-2535` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture（在隔离的 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：每条 AC 各用一份独立的同形 fixture——以默认 `libra init`（含 vault）建仓，`commit.gpgSign=true` 下在 `main` 上建基点 `B`，从 `B` 建分支 `topic` 并在其上建出被重放、被折叠与折叠目标提交（均带 vault 签名，即带旧 `gpgsig`），且该 AC 所测 action 的结果是 rebase 之后 `topic` 的 tip；切回 `main` 提交只修改 `u.txt` 的 `<upstream>`（`topic` 与 `<upstream>` 分叉于 `B`，每个被重放或折叠的提交都获得新父提交、必经签名出口）；再按该 AC 设置 `commit.gpgSign` 后在 `topic` 上以 `<upstream>` 为目标 rebase；`reword`、`edit`、`fixup -C`、`fixup -c` 经 `GIT_SEQUENCE_EDITOR` 改 todo 后运行 `libra rebase -i <upstream>`（`reword`/`fixup -c` 以 `GIT_EDITOR=true` 保留消息，`edit` 停止后不改动直接 `libra rebase --continue`）；`fixup`/`squash`/`amend!` 经 `fixup!`/`squash!`/`amend!` 提交以 `libra rebase --autosquash <upstream>` 生成；「签名有效」指在以 `<upstream>` 为起点的临时分支上 `libra merge --ff-only --verify-signatures topic` exit 0（`topic` 必须是 `<upstream>` 的后代，且其 tip 带有效签名））

- [ ] AC1：`commit.gpgSign=false` 时 `amend!` 折叠结果的 `libra cat-file -p` 输出不含 `gpgsig` 行。
- [ ] AC2：`commit.gpgSign=false` 时 `fixup -C` 折叠结果不含 `gpgsig` 行。
- [ ] AC3：`commit.gpgSign=false` 时 `fixup -c` 折叠结果不含 `gpgsig` 行。
- [ ] AC4：vault OpenPGP、`commit.gpgSign=true` 时 `amend!` 折叠结果签名有效。
- [ ] AC5：vault OpenPGP、`commit.gpgSign=true` 时 `fixup -C` 折叠结果签名有效。
- [ ] AC6：vault OpenPGP、`commit.gpgSign=true` 时 `fixup -c` 折叠结果签名有效。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-01` 执行。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的 CLI 错误类；重放签名失败的 CLI 错误类「rebase 重放签名失败停止」及其代表路径由 `SA-08j` 交付。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_amend_fixup_keep_unsigned_strip_stale_gpgsig)'`（new；AC1–AC3）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_amend_fixup_keep_signs_openpgp)'`（new；AC4–AC6）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::rebase_test::/) | test(/^command::rebase_interactive_test::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls --test compat_sequencer_message_author`。
**D 组:** 继承 `REL-SA-01` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；签名安全边界变更由 REL-SA-01 的 T-2 全量覆盖）`
**Dependencies:** `SA-08b`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rebase.rs`, `tests/command/rebase_test.rs`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`（`amend!`、`fixup -C`、`fixup -c` 的签名语义）、`COMPATIBILITY.md`（`rebase` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`（本地草稿，由 `REL-SA-01` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-01` 的 `immutable-release` 前滚）
**Security and privacy:** 签名只经单一出口；测试密钥只在临时目录生成，证据不记录私钥、口令或签名原文。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-01`（3/6）
**C/D coverage from:** `REL-SA-01`
**Granularity:** `type=implementation; axis=rebase的amend/fixup-C/fixup-c签名出口; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=SA-08b,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-08b; release=family child; split-from=SA-08b; exception=N/A`

---

### Task SA-08g: `rebase` 重放提交的 `gpg.format=ssh` 签名（#780 前置，#787）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `SA-08a` 拆出（`REL-SA-01` 家族子卡 4/6，只做本地签名+DCO 提交）：`gpg.format=ssh` 时 `finalize_replayed_commit` 经 `DEP-SA-02` 交付的 SSH 签名入口签名，`pick` 与折叠结果均得到有效 SSH 签名。
**Out of scope:** 出口、OpenPGP 与出口级签名失败（由 `SA-08a` 承接）；其余 action 迁入出口（由 `SA-08b`、`SA-08m` 承接）；`--signoff` 与其它重放后改写（由 `SA-08h` 承接）；签名失败停止与恢复（由 `SA-08j` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 签名决策（`commit_signing_policy`：`commit.gpgSign` → `Force`/`Disable`/`InheritVault`）与 `libra commit` 的调用点；vault OpenPGP 签名入口 `vault_sign_commit`；`gpg.format=ssh` 的签名入口由 `DEP-SA-02`（`REL-CS-01`）交付，开工时按其实际源码位置核对 | `src/command/history_config.rs:71-82`; `src/command/commit.rs:1144-1148,3088-3100` |
| `Pick` 与 `Squash` 分支当前不重签 | `src/command/rebase.rs:5861-5867,5879-5892` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture（在隔离的 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：以默认 `libra init` 建仓，在临时目录以 `ssh-keygen -t ed25519 -N ''` 生成测试密钥，以其公钥生成「验证执行协议」第 8 条所用的 `allowed_signers`；先在 `commit.gpgSign=false` 下建出提交形状（旧提交均无签名，只有新生成的重放或折叠结果可能带有效签名）：在 `main` 上建基点 `B`，从 `B` 建分支 `topic` 并提交 1 个只修改 `f.txt` 的提交（AC2 另在其后提交对应的 `squash!` 提交），切回 `main` 提交只修改 `u.txt` 的 `<upstream>`（`topic` 与 `<upstream>` 分叉于 `B`，rebase 必须重放）；再按 `DEP-SA-02` 交付的 SSH 签名配置项把该密钥设为签名密钥并设 `gpg.format=ssh`、`commit.gpgSign=true`；AC1 在 `topic` 上运行 `libra rebase <upstream>`，AC2 运行 `libra rebase --autosquash <upstream>`；两条 AC 的验签对象均为 rebase 之后 `topic` 的 tip）

- [ ] AC1：`pick` 重放结果：按「验证执行协议」第 8 条的 SSH 验签规则验签通过。
- [ ] AC2：`squash!` 折叠结果：按「验证执行协议」第 8 条的 SSH 验签规则验签通过。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-01` 执行。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的 CLI 错误类；重放签名失败的 CLI 错误类「rebase 重放签名失败停止」及其代表路径由 `SA-08j` 交付。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_pick_signs_ssh)'`（new；AC1）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_squash_signs_ssh)'`（new；AC2）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::rebase_test::/) | test(/^command::rebase_interactive_test::/)'`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::rebase::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls --test compat_sequencer_message_author`。
**D 组:** 继承 `REL-SA-01` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；签名安全边界变更由 REL-SA-01 的 T-2 全量覆盖）`
**Dependencies:** `SA-08m`, `DEP-SA-02`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rebase.rs`, `tests/command/rebase_test.rs`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`（签名决策只调用 `commit.rs` 既有入口，不修改 `commit.rs`）
**Docs and compatibility impact:** `docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`（`gpg.format=ssh` 下的重放签名）、`COMPATIBILITY.md`（`rebase` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`（本地草稿，由 `REL-SA-01` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-01` 的 `immutable-release` 前滚）
**Security and privacy:** 签名只经单一出口；出口级签名失败不写提交对象；测试密钥只在临时目录生成，证据不记录私钥、口令或签名原文。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-01`（4/6）
**C/D coverage from:** `REL-SA-01`
**Granularity:** `type=implementation; axis=rebase重放SSH格式签名; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=SA-08m,DEP-SA-02,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-08m; release=family child; split-from=SA-08a; exception=N/A`

---

### Task SA-08h: `rebase` 重放后的改写先于签名：`--signoff` 尾注、交互式消息编辑与 `edit` 停止后的修订（#780 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `SA-08a` 拆出（`REL-SA-01` 家族子卡 5/6，只做本地签名+DCO 提交）：签名必须覆盖 rebase 生成提交的最终内容（`ADR-SA-03`）。①`--signoff` 时 `Signed-off-by:` 尾注在构造 `UnsignedReplayContent` 时追加、先于 `finalize_replayed_commit` 签名，删除 `rejoin_commit_message` 对旧签名的重附；②交互式消息编辑（`edit_interactive_commit_message`：`reword`、`fixup -c` 与折叠组末尾的 `squash` 消息编辑经同一调用点 `src/command/rebase.rs:4045` 进入该函数）在编辑器改动消息时生成的新提交改经 `finalize_replayed_commit` 签名——签名在该函数内完成，三种触发共用同一提交生成代码，故以 `reword` 验收；③`edit` 停止后 `libra rebase --continue` 吸收已暂存改动（`amend_head_from_index_if_staged`）生成的新提交改经 `finalize_replayed_commit` 签名。三处都是「重放后改写提交内容再生成新提交」的同一类路径，改完后 rebase 生成的每个提交都只经单一出口。改写路径的二次签名失败：出口返回错误且不写对象；其停止态与恢复合同、专项门登记 `DEFER-SA-08`（本卡只验收签名成功路径）。
**Out of scope:** 出口与出口级签名失败（由 `SA-08a` 承接）；其余 action 迁入出口（由 `SA-08b`、`SA-08m` 承接）；`gpg.format=ssh`（由 `SA-08g` 承接）；重放步签名失败的停止与恢复（由 `SA-08j` 承接）；改写路径二次签名失败的停止态与恢复（`DEFER-SA-08`）；用户在 `edit` 停止期间自行运行 `libra commit --amend` 生成的提交（由 `libra commit` 自身的签名决策负责，`--continue` 只采纳该 HEAD）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--signoff` 路径经 `rejoin_commit_message` 重附旧签名，不重签 | `src/command/rebase.rs:5925-5939,5942-5948` |
| `RebaseArgs` 的 `-s`/`--signoff` | `src/command/rebase.rs:371-373,2871-2872` |
| 交互式消息编辑在消息改变时以 `Commit::new` 生成不签名的新提交（`reword`/`fixup -c` 与折叠组末尾的 `squash` 经此路径） | `src/command/rebase.rs:1402-1443,4027-4046` |
| `edit` 停止后 `--continue` 经 `reconcile_interactive_edit_continue` 调用 `amend_head_from_index_if_staged`，以 `Commit::new` 生成不签名的新提交 | `src/command/rebase.rs:2024-2065,2067-2075,4407-4413` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture（在隔离的 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：AC1 以默认 `libra init`（含 vault）建仓，`commit.gpgSign=true` 下在 `main` 上建基点 `B`（含已跟踪文件 `f.txt`、`g.txt`、`h.txt`）；从 `B` 建分支 `topic` 并提交恰好 1 个只修改 `f.txt` 的提交，记 `T_SHA="$(libra rev-parse HEAD)"`；切回 `main` 提交只修改 `u.txt` 的 `<upstream>`，再切回 `topic`（`topic` 与 `<upstream>` 分叉于 `B`，`<upstream>` 不是 `topic` 的祖先，rebase 必须重放该提交，不走「已是最新」短路）（待重放提交带 vault 签名），再设 `commit.gpgSign=false` 后在 `topic` 上运行 `libra rebase --signoff <upstream>`；AC2–AC7 同样建仓，但在 `commit.gpgSign=false` 下建出同形历史（基点 `B`、`topic` 上恰好 1 个只修改 `f.txt` 的提交 `T`、`main` 上只修改 `u.txt` 的 `<upstream>`；旧提交无签名，只有新生成的提交可能带有效签名），再设 `commit.gpgSign=true`：AC2、AC3 运行 `libra rebase --signoff <upstream>`；「reword」fixture（AC4、AC5）以 `GIT_SEQUENCE_EDITOR` 指向把 todo 中 `T` 一行的 `pick` 改为 `reword` 的脚本、以 `GIT_EDITOR` 指向把消息文件整体改写为 `reworded` 的脚本，运行 `libra rebase -i <upstream>`；「edit」fixture（AC6、AC7）以 `GIT_SEQUENCE_EDITOR` 把该行改为 `edit` 运行 `libra rebase -i <upstream>`，停止后把 `f.txt` 改写为 `edited` 加换行并 `libra add f.txt`，再运行 `libra rebase --continue`；各对 AC 针对同一结果（`topic` 的 tip））

- [ ] AC1：`commit.gpgSign=false` 时 `--signoff` 重放结果的 `libra cat-file -p` 输出不含 `gpgsig` 行。
- [ ] AC2：vault OpenPGP 且 `commit.gpgSign=true` 时 `--signoff` 重放结果（`topic` 的 tip）的消息末尾含 `Signed-off-by:` 尾注行。
- [ ] AC3：AC2 的同一次 rebase 之后：在以 `<upstream>` 为起点的临时分支上 `libra merge --ff-only --verify-signatures topic` exit 0（`topic` 必须是 `<upstream>` 的后代，且其 tip 带有效签名）。
- [ ] AC4：「reword」fixture 的 rebase 之后 `topic` tip 的消息首行为 `reworded`（编辑器改写已生效）。
- [ ] AC5：「reword」fixture 的同一次 rebase 之后：在以 `<upstream>` 为起点的临时分支上 `libra merge --ff-only --verify-signatures topic` exit 0（`topic` 必须是 `<upstream>` 的后代，且其 tip 带有效签名）。
- [ ] AC6：「edit」fixture 的 `--continue` 之后 `libra rev-parse topic:f.txt` 等于 `edited` 加换行这一内容的 `libra hash-object` 结果（已暂存改动被吸收）。
- [ ] AC7：「edit」fixture 的同一次 `--continue` 之后：在以 `<upstream>` 为起点的临时分支上 `libra merge --ff-only --verify-signatures topic` exit 0（`topic` 必须是 `<upstream>` 的后代，且其 tip 带有效签名）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-01` 执行。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的 CLI 错误类；重放签名失败的 CLI 错误类「rebase 重放签名失败停止」及其代表路径由 `SA-08j` 交付。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_signoff_unsigned_strips_stale_gpgsig)'`（new；AC1）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_signoff_then_sign_pick)'`（new；AC2、AC3）
- [ ] VER3 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_reword_editor_rewrite_signs)'`（new；AC4、AC5）
- [ ] VER4 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_edit_amend_continue_signs)'`（new；AC6、AC7）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::rebase_test::/) | test(/^command::rebase_interactive_test::/)'`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::rebase::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls --test compat_sequencer_message_author`。
**D 组:** 继承 `REL-SA-01` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；签名安全边界变更由 REL-SA-01 的 T-2 全量覆盖）`
**Dependencies:** `SA-08g`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rebase.rs`, `tests/command/rebase_test.rs`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`（签名决策只调用 `commit.rs` 既有入口，不修改 `commit.rs`）
**Docs and compatibility impact:** `docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`（`--signoff`、交互式消息编辑与 `edit` 停止后的修订均先于签名）、`COMPATIBILITY.md`（`rebase` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`（本地草稿，由 `REL-SA-01` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-01` 的 `immutable-release` 前滚）
**Security and privacy:** 签名只经单一出口，重放后的改写（尾注、消息编辑、`edit` 停止后的修订）都在签名之前完成；测试密钥只在临时目录生成，证据不记录私钥、口令或签名原文。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-01`（5/6）
**C/D coverage from:** `REL-SA-01`
**Granularity:** `type=implementation; axis=rebase重放后改写先于签名; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=SA-08g,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-08g; release=family child; split-from=SA-08a; exception=N/A`

---

### Task SA-08j: `rebase` 重放签名失败的冲突同构停止与重试、`--continue`/`--abort` 恢复（门族卡 `EX-SA-01`；#780 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `SA-08a` 拆出（`REL-SA-01` 家族子卡 6/6，只做本地签名+DCO 提交；R37 起原 `SA-08k`（`--abort` 回退）并入本卡，`SA-08k` 编号撤回、不复用）：任一重放步（含第一步）的出口签名失败时按冲突停止同构处理——先把该步的合并树写入索引与工作区，再记录 `stopped_sha`、保留 rebase 状态并以非 0 退出，不移动任何分支或标签引用（HEAD 停在失败步之前的重放结果，第一步失败时即 `<upstream>`）；`--continue` 以索引树经 `finalize_replayed_commit` 重新签名生成该步提交（补丁不丢失），再按 todo 继续重放；`--continue` 为该步签名再次失败时以非 0 退出且停止态不变（可重试）；`--abort` 复用既有 `run_rebase_abort` 回到 rebase 前的分支、引用、索引与工作区（GC-08）。签名失败在重放循环与 `--continue` 中都以新增的 `ReplayErrorKind::Sign`（`as_str()` 为 `sign`，`stable_code()` 为 `StableErrorCode::AuthMissingCredentials`，即 `LBR-AUTH-001`、退出码 128，与 `libra commit` 签名失败的 `CommitError::VaultSign` 同码，`src/command/commit.rs:566-568`）报告：重放循环经 `ReplayResult::Internal` → `RebaseError::ReplayInternal`（`src/command/rebase.rs:4122-4146`，其 CLI 映射按 `kind.stable_code()` 取码，`src/command/rebase.rs:723-731`），`--continue` 把出口的签名错误映射为同一 `RebaseError::ReplayInternal { kind: ReplayErrorKind::Sign, .. }`（现有映射只区分身份缺失与对象加载，`src/command/rebase.rs:4467-4473`）；`docs/error-codes.md` 在 `LBR-AUTH-001` 下写明 `rebase` 重放签名失败停止。与 Git 一致：Git 的 sequencer 在提交（含签名）失败时停止并保留可恢复状态，不做签名能力预检（Git 源码 `sequencer.c`：`do_pick_commit` 把提交失败经 `res = !!res` 归入 `PICK_RESULT_CONFLICTS`，`pick_one_commit` 对普通步骤以 `error_with_patch`、对 `fixup`/`squash` 折叠步骤以 `error_failed_squash` 停止，两者都保留可恢复状态）。该处理对全部重放 action 经 `create_replayed_commit` 构造提交的步骤共用（`--continue` 按 todo 首项 action 经同一函数构造提交）；重放后改写路径（交互式消息编辑在该步标记完成后的二次提交，`src/command/rebase.rs:4040-4048`；`edit` 停止后 `--continue` 经 `reconcile_interactive_edit_continue` 吸收已暂存改动的修订提交，`src/command/rebase.rs:2067,4412`）的二次签名失败不经该停止路径：出口仍返回错误且不写对象、命令以非 0 退出，其停止态、`--continue`/`--abort` 恢复合同与专项门登记 `DEFER-SA-08`，本计划不声明其恢复语义。本卡以多步 `pick` 与 `squash`/`fixup` 折叠两类重放、第一步与中间步两种位置、注入与真实（无 unseal key）两种签名错误验收（折叠步的提交取折叠目标的父提交、作者与消息，与 `pick` 路径不同）。新增测试专用 failpoint `LIBRA_TEST_REBASE_SIGN_FAIL`（与 `src/command/am.rs:756-758` 的 `test_failpoint_enabled` 同一判定：只在进程设置了 `LIBRA_TEST` 时生效，未设置时完全忽略）：其值等于本步被重放的原提交 OID 时，`finalize_replayed_commit` 在调用签名入口处返回注入错误 `test-injected rebase signing failure`（与真实签名错误同一返回路径），对其它原提交的步骤不生效。本卡验收是同一恢复轴上的门族，G-03 条目上限按 `EX-SA-01` 豁免（只豁免 G-03 计数）。
**Out of scope:** 出口与成功路径（由 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h` 承接）；从签名失败停止态 `--skip`，以及 `reword`、`edit`、`amend!`、`fixup -C`、`fixup -c` 逐 action 的签名失败门（与本卡两类重放共用同一停止与 `--continue` 路径，登记 `DEFER-SA-08`）；带 `--autostash` 时签名失败停止后的 stash 恢复（沿用既有 `run_rebase_abort` 的 autostash 处理，不另设门）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 重放循环：`create_replayed_commit` 出错即返回 `ReplayResult::internal`，此时尚未执行该步的索引与工作区更新 | `src/command/rebase.rs:5746-5762,5774-5777` |
| `Internal` 结果把该步记为 `stopped_sha` 并保存状态 | `src/command/rebase.rs:4121-4127` |
| `--continue` 直接从当前索引建树，以 todo 首项 action 调用 `create_replayed_commit` 创建提交，成功后才从 todo 移除该步、清除 `stopped_sha` 并更新 HEAD（若索引未含该步改动，补丁会被静默丢失） | `src/command/rebase.rs:4421-4445,4454-4488` |
| rebase 状态持久化在 `rebase_state` 表（按 `worktree_id`） | `src/command/rebase/state.rs:484-495,512-524,640-652` |
| 折叠 action（`fixup`/`squash`）以折叠目标的父提交、作者构造提交；`squash` 消息为折叠目标与本提交消息各自 trim 后以一个空行连接，`fixup` 保留折叠目标消息；`--autosquash` 由 `fixup!`/`squash!` 前缀生成折叠 action | `src/command/rebase.rs:5868-5892,2526-2535` |
| `run_rebase_abort` 恢复索引与工作区、把 HEAD 挂回原分支并清除 rebase 状态 | `src/command/rebase.rs:4533-4665` |
| 既有测试 failpoint 模式（`LIBRA_TEST=1` 加具名变量） | `src/command/am.rs:305,756-758` |
| `libra status` 在 rebase 进行中输出 `rebase in progress` 提示 | `src/command/status/scan.rs:29` |
| Git 在提交（含签名）失败时停止并保留可恢复状态，无预检 | Git 源码 `sequencer.c`：`do_pick_commit` 把提交失败经 `res = !!res` 归入 `PICK_RESULT_CONFLICTS`；`pick_one_commit` 对普通步骤调用 `error_with_patch`，对折叠步骤（`is_fixup`）调用 `error_failed_squash` |
| 无 unseal key 时签名报 `vault signing enabled but no unseal key found`；`libra init --vault false` 跳过 vault | `src/command/commit.rs:3088-3114`; `src/command/init.rs:393-395` |
| 测试 harness 为子进程设置 `LIBRA_TEST=1` | `tests/command/mod.rs:375`; `src/utils/pager.rs:20` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；本卡 AC 数超过 G-03 上限，按 `EX-SA-01`（G-03 门族型验收）豁免并如实写作 `112/8@EX-SA-01`，门族与命令见下方「判据规范（非计数正文）」。术语与 fixture（G1–G4、G6 共用①–④；G5 以 `libra init --vault false` 建仓、其余同①–④；全部在仓库外的隔离 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：①建仓：默认 `libra init`（含 vault，签名可用），设 `user.name=Committer`、`user.email=committer@example.com`，在 `commit.gpgSign=false` 下于 `main` 提交基点 `B`（已跟踪 `f.txt`、`g.txt`、`h.txt`、`k.txt`），自 `B` 建分支 `topic` 并按②或③提交（每个提交以 `libra commit --author "<作者>" --date "<互不相同的 Unix 时间戳> +0800"` 设置与提交者不同的作者与非 UTC 时区；原提交均无签名），再切回 `main` 提交只新增 `u.txt`（`B` 中不存在）的 `<upstream>`，最后切回 `topic`；②多步 `pick` 历史（门 G1、G2）：`T1`（只改 `f.txt`，消息 `t1`，作者 `One <one@example.com>`）、`T2`（只改 `g.txt`，消息 `t2`，作者 `Two <two@example.com>`）、`T3`（只改 `h.txt`，消息 `t3`，作者 `Three <three@example.com>`），rebase 命令为 `libra rebase <upstream>`；③折叠历史（门 G3、G4）：`T1`（只改 `f.txt`，消息 `one`，作者 `One <one@example.com>`）、`S`（只改 `g.txt`，消息 `squash! one`，作者 `Two <two@example.com>`）、`X`（只改 `h.txt`，消息 `fixup! one`，作者 `Three <three@example.com>`）、`T4`（只改 `k.txt`，消息 `four`，作者 `Four <four@example.com>`），rebase 命令为 `libra rebase --autosquash <upstream>`（todo 为 `pick T1`、`squash S`、`fixup X`、`pick T4`）；④rebase 前在工作区放一个与各提交无关、字节独特的未跟踪文件 `notes.txt`（用户数据，各门全程不得改动或删除），再记录 rebase 前快照：`libra show-ref` 输出、`libra ls-files --stage` 输出、工作区文件路径集合（不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`，下同）及每个文件的 `libra hash-object`；快照之后设 `commit.gpgSign=true`；「停止态快照」为停止后记录的 `libra rev-parse HEAD`、`libra show-ref`、`libra ls-files --stage`、工作区文件路径集合与各文件 `libra hash-object`，以及 `rebase_state` 中本工作区的整行；⑤「注入 `<c>`」指在该命令前设置 `LIBRA_TEST=1` 与 `LIBRA_TEST_REBASE_SIGN_FAIL=<c 的 OID>`，「去掉 failpoint」指该命令不设置 `LIBRA_TEST_REBASE_SIGN_FAIL`；⑥`M(c)`：`libra ls-tree -r <c>` 的全部条目加上 `libra ls-tree -r <upstream>` 中路径为 `u.txt` 的那一条（topic 各提交不改 `u.txt`，故 `M(c)` 即重放到 `c` 为止的合并结果）；「索引等于 `M(c)`」：`libra ls-files --stage` 的（模式、blob OID、stage、路径）条目集合等于 `M(c)` 各条目配 stage 0 所成的集合（一次集合比较，冲突条目的非 0 stage 也使其不等）；「提交的树等于 `M(c)`」：`libra ls-tree -r <提交>` 的条目集合等于 `M(c)`；⑦「工作区路径与索引一致」：工作区文件路径集合等于 `libra ls-files --stage` 的路径集合再加上 `notes.txt` 这一路径（一次集合比较）；「工作区内容与索引一致」：工作区「路径 → `libra hash-object`」映射等于「索引各路径的 blob OID，加上 `notes.txt` 的快照值」这一期望映射（一次映射比较）；⑧author 行指 `libra cat-file -p` 中以 `author ` 开头的整行，消息指 `libra cat-file -p` 输出首个空行之后的全部字节去掉结尾换行；「折叠期望消息」为 `one`、一个空行、`squash! one`（`squash` 规则连接 `T1` 与 `S` 的消息，`fixup` 保留折叠目标消息，故 `X` 的消息不出现）；⑨「重放结果」指 `libra rev-list --reverse <upstream>..topic` 依次列出的提交；⑩「签名有效」：在以该提交父为起点的临时分支上 `libra merge --ff-only --verify-signatures <提交>` exit 0（各门在 HEAD 与状态断言之后执行）；⑪测试 harness 为每个子进程设置 `LIBRA_TEST=1`（`tests/command/mod.rs:375`），G6 须对该子进程显式移除 `LIBRA_TEST`；「默认运行」指不加 `--json`/`--machine` 的命令）

- [ ] AC1：门 G1：注入 `T2` 运行多步 `pick` 的 rebase 命令（默认运行）的退出码为 128（代表路径的退出码）。
- [ ] AC2：AC1 的人读 stderr含 `test-injected rebase signing failure`（代表路径的人读输出）。
- [ ] AC3：AC1 之后 `libra show-ref` 的输出等于 rebase 前快照（停止不移动任何分支或标签引用）。
- [ ] AC4：AC1 之后 `rebase_state` 中本工作区行的 `stopped_sha` 等于 `T2`。
- [ ] AC5：AC1 之后 HEAD 的树等于 `M(T1)`（HEAD 停在失败步之前的重放结果）。
- [ ] AC6：AC1 之后索引等于 `M(T2)`（该步合并结果已写入索引，含上游新增的 `u.txt`）。
- [ ] AC7：AC1 之后工作区路径与索引一致。
- [ ] AC8：AC1 之后工作区内容与索引一致。
- [ ] AC9：AC1 之后 `libra status` 的输出含 `rebase in progress`。
- [ ] AC10：在 AC1 的停止态（先记录停止态快照）下注入 `T2` 运行 `libra rebase --continue`，其结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-AUTH-001`（该步签名再次失败）。
- [ ] AC11：AC10 之后 `libra rev-parse HEAD` 的输出等于停止态快照。
- [ ] AC12：AC10 之后 `libra show-ref` 的输出等于停止态快照。
- [ ] AC13：AC10 之后 `libra ls-files --stage` 的输出等于停止态快照。
- [ ] AC14：AC10 之后工作区文件路径集合等于停止态快照。
- [ ] AC15：AC10 之后工作区各文件的 `libra hash-object` 等于停止态快照。
- [ ] AC16：AC10 之后 `rebase_state` 中本工作区的行与停止态快照逐列相同（失败的 `--continue` 不消费 todo、不改 `stopped_sha` 与 `current_head`）。
- [ ] AC17：去掉 failpoint 后 `libra rebase --continue` exit 0。
- [ ] AC18：AC17 之后重放结果恰为 3 个提交。
- [ ] AC19：AC17 之后重放结果中第 1 个提交的唯一父提交为 `<upstream>`，其后每个提交的唯一父提交为前一个。
- [ ] AC20：AC17 之后重放结果的树依次等于 `M(T1)`、`M(T2)`、`M(T3)`。
- [ ] AC21：AC17 之后重放结果的 author 行依次等于 `T1`、`T2`、`T3` 的 author 行。
- [ ] AC22：AC17 之后重放结果的消息依次等于 `T1`、`T2`、`T3` 的消息。
- [ ] AC23：AC17 之后 `libra rev-parse --abbrev-ref HEAD` 输出 `topic`。
- [ ] AC24：AC17 之后 `libra status` 的输出不含 `rebase in progress`。
- [ ] AC25：AC17 之后重放结果的每个提交签名有效。
- [ ] AC26：AC17 之后索引等于 `M(T3)`（最终索引等于最终提交的树）。
- [ ] AC27：AC17 之后工作区路径与索引一致。
- [ ] AC28：AC17 之后工作区内容与索引一致。
- [ ] AC29：门 G2：注入 `T1`（第一步）运行多步 `pick` 的 rebase 命令的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-AUTH-001`。
- [ ] AC30：AC29 的人读 stderr含 `test-injected rebase signing failure`。
- [ ] AC31：AC29 之后 `libra show-ref` 的输出等于 rebase 前快照（停止不移动任何分支或标签引用）。
- [ ] AC32：AC29 之后 `rebase_state` 中本工作区行的 `stopped_sha` 等于 `T1`。
- [ ] AC33：AC29 之后 `libra rev-parse HEAD` 等于 `<upstream>`（第一步失败，HEAD 停在新基点）。
- [ ] AC34：AC29 之后索引等于 `M(T1)`（停在第一步，为本门 `--abort` 的起点）。
- [ ] AC35：AC29 之后工作区路径与索引一致。
- [ ] AC36：AC29 之后工作区内容与索引一致。
- [ ] AC37：在 AC29 的停止态下去掉 failpoint 运行 `libra rebase --abort` exit 0。
- [ ] AC38：AC37 之后 `libra rev-parse --abbrev-ref HEAD` 输出 `topic`。
- [ ] AC39：AC37 之后 `libra show-ref` 的输出等于 rebase 前快照。
- [ ] AC40：AC37 之后 `libra ls-files --stage` 的输出等于 rebase 前快照。
- [ ] AC41：AC37 之后工作区文件路径集合等于 rebase 前快照（上游 `u.txt` 不残留）。
- [ ] AC42：AC37 之后工作区各文件的 `libra hash-object` 等于 rebase 前快照。
- [ ] AC43：AC37 之后 `libra status` 的输出不含 `rebase in progress`。
- [ ] AC44：门 G3：注入 `S` 运行折叠 rebase 命令的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-AUTH-001`（`squash` 步签名失败）。
- [ ] AC45：AC44 的人读 stderr含 `test-injected rebase signing failure`。
- [ ] AC46：AC44 之后 `libra show-ref` 的输出等于 rebase 前快照。
- [ ] AC47：AC44 之后 `rebase_state` 中本工作区行的 `stopped_sha` 等于 `S`。
- [ ] AC48：AC44 之后 HEAD 的树等于 `M(T1)`（停在 `pick T1` 的重放结果上）。
- [ ] AC49：AC44 之后索引等于 `M(S)`。
- [ ] AC50：AC44 之后工作区路径与索引一致。
- [ ] AC51：AC44 之后工作区内容与索引一致。
- [ ] AC52：在 AC44 的停止态下注入 `X` 运行 `libra rebase --continue`，其结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-AUTH-001`（`squash` 步经出口签名生成后，`fixup` 步签名失败再次停止）。
- [ ] AC53：AC52 的人读 stderr含 `test-injected rebase signing failure`。
- [ ] AC54：AC52 之后 `rebase_state` 中本工作区行的 `stopped_sha` 等于 `X`。
- [ ] AC55：AC52 之后 HEAD 的树等于 `M(S)`（停在 `squash` 折叠结果上）。
- [ ] AC56：AC52 之后索引等于 `M(X)`。
- [ ] AC57：AC52 之后工作区路径与索引一致。
- [ ] AC58：AC52 之后工作区内容与索引一致。
- [ ] AC59：AC52 之后 `libra show-ref` 的输出等于 rebase 前快照。
- [ ] AC60：去掉 failpoint 后 `libra rebase --continue` exit 0。
- [ ] AC61：AC60 之后重放结果恰为 2 个提交。
- [ ] AC62：AC60 之后重放结果中第 1 个提交的唯一父提交为 `<upstream>`，第 2 个提交的唯一父提交为第 1 个。
- [ ] AC63：AC60 之后重放结果的树依次等于 `M(X)`、`M(T4)`。
- [ ] AC64：AC60 之后重放结果的 author 行依次等于 `T1`、`T4` 的 author 行（折叠提交保留折叠目标的作者）。
- [ ] AC65：AC60 之后重放结果的消息依次等于折叠期望消息与 `T4` 的消息。
- [ ] AC66：AC60 之后 `libra rev-parse --abbrev-ref HEAD` 输出 `topic`。
- [ ] AC67：AC60 之后 `libra status` 的输出不含 `rebase in progress`。
- [ ] AC68：AC60 之后重放结果的每个提交签名有效。
- [ ] AC69：AC60 之后索引等于 `M(T4)`（最终索引等于最终提交的树）。
- [ ] AC70：AC60 之后工作区路径与索引一致。
- [ ] AC71：AC60 之后工作区内容与索引一致。
- [ ] AC72：门 G4：注入 `X` 运行折叠 rebase 命令的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-AUTH-001`（`squash` 步成功、`fixup` 步签名失败）。
- [ ] AC73：AC72 的人读 stderr含 `test-injected rebase signing failure`。
- [ ] AC74：AC72 之后 `libra show-ref` 的输出等于 rebase 前快照（停止不移动任何分支或标签引用）。
- [ ] AC75：AC72 之后 `rebase_state` 中本工作区行的 `stopped_sha` 等于 `X`。
- [ ] AC76：AC72 之后 HEAD 的树等于 `M(S)`。
- [ ] AC77：AC72 之后索引等于 `M(X)`（停在 `fixup` 步，为本门 `--abort` 的起点）。
- [ ] AC78：AC72 之后工作区路径与索引一致。
- [ ] AC79：AC72 之后工作区内容与索引一致。
- [ ] AC80：在 AC72 的停止态下去掉 failpoint 运行 `libra rebase --abort` exit 0。
- [ ] AC81：AC80 之后 `libra rev-parse --abbrev-ref HEAD` 输出 `topic`。
- [ ] AC82：AC80 之后 `libra show-ref` 的输出等于 rebase 前快照。
- [ ] AC83：AC80 之后 `libra ls-files --stage` 的输出等于 rebase 前快照。
- [ ] AC84：AC80 之后工作区文件路径集合等于 rebase 前快照（上游 `u.txt` 不残留）。
- [ ] AC85：AC80 之后工作区各文件的 `libra hash-object` 等于 rebase 前快照。
- [ ] AC86：AC80 之后 `libra status` 的输出不含 `rebase in progress`。
- [ ] AC87：门 G5：真实签名不可用（`--vault false` 仓库、`commit.gpgSign=true`、不设 failpoint）时运行多步 `pick` 的 rebase 命令的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-AUTH-001`。
- [ ] AC88：AC87 的人读 stderr含 `vault signing enabled but no unseal key found`（真实签名错误被透传）。
- [ ] AC89：AC87 之后 `libra show-ref` 的输出等于 rebase 前快照。
- [ ] AC90：AC87 之后 `rebase_state` 中本工作区行的 `stopped_sha` 等于 `T1`。
- [ ] AC91：AC87 之后 `libra rev-parse HEAD` 等于 `<upstream>`。
- [ ] AC92：AC87 之后索引等于 `M(T1)`（第一步即按冲突同构停止，合并结果已写入索引）。
- [ ] AC93：AC87 之后工作区路径与索引一致。
- [ ] AC94：AC87 之后工作区内容与索引一致。
- [ ] AC95：设 `commit.gpgSign=false` 后 `libra rebase --continue` exit 0。
- [ ] AC96：AC95 之后重放结果的树依次等于 `M(T1)`、`M(T2)`、`M(T3)`。
- [ ] AC97：AC95 之后重放结果的每个提交的 `libra cat-file -p` 输出都不含 `gpgsig` 行（按修正后的配置不签名）。
- [ ] AC98：AC95 之后 `libra rev-parse --abbrev-ref HEAD` 输出 `topic`。
- [ ] AC99：AC95 之后 `libra status` 的输出不含 `rebase in progress`。
- [ ] AC100：AC95 之后索引等于 `M(T3)`（最终索引等于最终提交的树）。
- [ ] AC101：AC95 之后工作区路径与索引一致。
- [ ] AC102：AC95 之后工作区内容与索引一致。
- [ ] AC103：门 G6：显式移除 `LIBRA_TEST`、仅设 `LIBRA_TEST_REBASE_SIGN_FAIL=<T2 的 OID>` 运行多步 `pick` 的 rebase 命令 exit 0（failpoint 在非测试进程中不生效）。
- [ ] AC104：AC103 之后重放结果的树依次等于 `M(T1)`、`M(T2)`、`M(T3)`（含 `T2`，未因 failpoint 丢步）。
- [ ] AC105：AC103 之后重放结果的每个提交签名有效。
- [ ] AC106：AC103 之后 `libra rev-parse --abbrev-ref HEAD` 输出 `topic`。
- [ ] AC107：AC103 之后 `libra status` 的输出不含 `rebase in progress`。
- [ ] AC108：AC103 之后索引等于 `M(T3)`（最终索引等于最终提交的树）。
- [ ] AC109：AC103 之后工作区路径与索引一致。
- [ ] AC110：AC103 之后工作区内容与索引一致。
- [ ] AC111：代表路径的 `--json`：在门 G1 的 fixture（①、②、④）的独立副本上以同一 `T2` 注入运行 `libra --json rebase <upstream>`，结果（进程退出状态，stderr 整体解析出的 JSON 对象的（`ok`、`error_code`、`exit_code`）三元组）等于（128，（`false`、`LBR-AUTH-001`、128））（一次比较；stderr 不能整体解析为单个 JSON 对象即不成立）。
- [ ] AC112：代表路径的 `--machine`：在门 G1 的 fixture 的另一份独立副本上以同一 `T2` 注入运行 `libra --machine rebase <upstream>`，结果（进程退出状态，stderr 按行解析出的（`ok`、`error_code`、`exit_code`）三元组序列）等于（128，[（`false`、`LBR-AUTH-001`、128）]）（一次比较；任一行不能解析为 JSON 对象或行数不为 1 即不成立）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d、1e）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-01` 执行。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡新增 1 个错误类：E1「rebase 重放签名失败停止」（新增 `ReplayErrorKind::Sign`，`LBR-AUTH-001`，退出码 128；重放循环与 `--continue` 同类）。代表路径：门 G1 中注入 `T2` 的多步 `pick` rebase 命令 `libra rebase <upstream>`；四项判据：退出码 AC1、人读输出 AC2、`--json` AC111、`--machine` AC112。其余路径（各断言稳定码 `LBR-AUTH-001`）：AC10（G1 失败重试的 `--continue`）、AC29（G2 第一步）、AC44（G3 `squash` 步）、AC52（G3 `--continue` 进入 `fixup` 步）、AC72（G4 `fixup` 步）、AC87（G5 真实签名不可用）；#780 未要求这些路径的人读诊断（各门中既有的 stderr 判据保留）。

**判据规范（非计数正文）— `EX-SA-01`：** 本卡门族为同一恢复轴「重放签名失败 → 冲突同构停止 → 重试、`--continue`/`--abort` 恢复」上的 6 个机械门（两类重放 × 两种恢复动作，加真实签名错误与 failpoint 惰性各一门），每门是一条可复制执行的具名命令，判定该门所列全部 AC；任一门失败即整卡不达标。门族或其 AC 增减时，同批更新「规则 waiver」表 `EX-SA-01` 行、本卡 `Granularity` 与「任务卡粒度审计表」本卡行。

- 门 G1（多步 `pick` × 中间步签名失败停止、失败重试与 `--continue`；判定 AC1–AC28、AC111–AC112）：`source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failure_mid_replay_multi_pick_continue)'`
- 门 G2（多步 `pick` × 第一步签名失败停止与 `--abort`；判定 AC29–AC43）：`source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failure_mid_replay_multi_pick_abort)'`
- 门 G3（`squash`/`fixup` 折叠 × 两次签名失败停止与 `--continue`；判定 AC44–AC71）：`source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failure_mid_replay_fold_continue)'`
- 门 G4（`squash`/`fixup` 折叠 × `fixup` 步签名失败停止与 `--abort`；判定 AC72–AC86）：`source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failure_mid_replay_fold_abort)'`
- 门 G5（真实签名不可用 × 第一步停止与修正配置后 `--continue`；判定 AC87–AC102）：`source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_unavailable_stops_then_continue_after_config_fix)'`
- 门 G6（failpoint 在无 `LIBRA_TEST` 的进程中不生效；判定 AC103–AC110）：`source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failpoint_inert_without_libra_test)'`

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failure_mid_replay_multi_pick_continue)'`（new；门 G1：AC1–AC28、AC111–AC112）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failure_mid_replay_multi_pick_abort)'`（new；门 G2：AC29–AC43）
- [ ] VER3 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failure_mid_replay_fold_continue)'`（new；门 G3：AC44–AC71）
- [ ] VER4 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failure_mid_replay_fold_abort)'`（new；门 G4：AC72–AC86）
- [ ] VER5 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_unavailable_stops_then_continue_after_config_fix)'`（new；门 G5：AC87–AC102）
- [ ] VER6 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_sign_failpoint_inert_without_libra_test)'`（new；门 G6：AC103–AC110）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::rebase_test::/) | test(/^command::rebase_interactive_test::/)'`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::rebase::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls --test compat_sequencer_message_author`；`source .env.test && cargo nextest run --no-tests=fail --test compat_error_codes_doc_sync`。
**D 组:** 继承 `REL-SA-01` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；签名安全边界变更与 `docs/error-codes.md` 的 `T-1` 表面由 REL-SA-01 的 T-2 全量覆盖）`
**Dependencies:** `SA-08h`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rebase.rs`, `tests/command/rebase_test.rs`, `docs/error-codes.md`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`（签名决策只调用 `commit.rs` 既有入口，不修改 `commit.rs`）
**Docs and compatibility impact:** `docs/error-codes.md`（`LBR-AUTH-001` 增列 `rebase` 重放签名失败停止）、`docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`（重放签名失败的停止、重试、`--continue` 与 `--abort`）、`COMPATIBILITY.md`（`rebase` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`（本地草稿，由 `REL-SA-01` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-01` 的 `immutable-release` 前滚）
**Security and privacy:** 签名只经单一出口；签名失败不写提交对象、不移动分支或标签引用；测试 failpoint 只在设置了 `LIBRA_TEST` 的测试进程中生效（G6 验收未设置时不生效）；证据不记录私钥、口令或签名原文。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-01`（6/6）
**C/D coverage from:** `REL-SA-01`
**Granularity:** `type=implementation; axis=rebase签名失败的停止与恢复; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=112/8@EX-SA-01; VER=6/8; landing=1; prod-files=1; scope=M; deps=SA-08h,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-08h; release=family child; split-from=SA-08a; exception=EX-SA-01`

---

### Task REL-SA-01: 发布rebase重放签名出口家族版本（#780）

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** `REL-SA-01` 家族唯一发布点：A/B 阶段在包含 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 最终变更的树上核对家族提交序列（提交身份、逐提交改动集、最终改动集）与未提交改动并跑家族聚合守卫，ER-05 评审后进入 C 组：按 ER-08 `patch + 1`、版本面 parity（`compat_version_surface_sync`）、在 bump 后的树上跑 `T-2` 全量，再提交、经 PR 合入并 `gh release create` 发布；CLI 发布后按「文档与网站验收命令」第 5 步交付家族网站页面。不引入新行为。**强制门（ER-04/ER-05/ER-07，不计入 G-03）：** 子卡 `Acceptance=locally-accepted` 与各自 ER-05 评审 `PASS` 已在子卡记录；子卡本地提交与本卡发布提交都通过「验证执行协议」第 7 条的同一提交签名与 DCO 校验；C 组 bump 后、提交前 `source .env.test && cargo nextest run --no-tests=fail --test compat_version_surface_sync`、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿；PR 合入后、`gh release create` 之前按「验证执行协议」第 10 条执行合入树发布前门；发布之后的证据（D3 的 tag 与 release note、D4 产物可获取性、D5 网站部署、D6 合入树一致的远端记录）全部属 D 组。
**Out of scope:** 修改业务源码或测试（全部在 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `SA-08a`（出口、`Pick`、OpenPGP 签名与出口级签名失败）、`SA-08b`（`reword`、`edit`、`fixup`、`squash`）、`SA-08m`（`amend!`、`fixup -C`、`fixup -c`）、`SA-08g`（`gpg.format=ssh`）、`SA-08h`（`--signoff` 尾注、交互式消息编辑与 `edit` 停止后的修订先于签名）与 `SA-08j`（签名失败的冲突同构停止、重试、`--continue` 与 `--abort`，门族卡 `EX-SA-01`）共同把 rebase 生成提交的全部路径收口到单一签名出口，并闭合经 `create_replayed_commit` 构造提交的重放步的签名失败恢复（重放后改写路径的二次签名失败恢复登记 `DEFER-SA-08`），按 G-02/G-08 只能在同一发布点对外发布（`ADR-SA-03`） | 本计划「发布分组与并发窗口」`REL-SA-01` 行 |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；ER-04/ER-05/ER-07 强制门与合入/发布后的 D 组见 Description，不计入）

- [ ] AC1：A/B 阶段 `FAMILY_BASE_SHA..HEAD` 区间内的每个提交都是家族子卡记录的提交或独立记账提交（无插入的其它提交）。
- [ ] AC2：A/B 阶段每张家族子卡最后记录的 `COMMIT_SHA` 都在该区间内（无遗漏的子卡提交）。
- [ ] AC3：A/B 阶段 `FAMILY_BASE_SHA` 到 `HEAD` 为单父提交组成的线性链（无合并提交、无旁支）。
- [ ] AC4：A/B 阶段家族子卡提交在该链上的顺序与登记的子卡顺序相同。
- [ ] AC5：A/B 阶段每个家族子卡提交只改动该子卡写集内的路径与计划记账文件。
- [ ] AC6：A/B 阶段 `FAMILY_BASE_SHA` 到 `HEAD` 的最终改动集只含家族子卡写集与计划记账文件中的路径。
- [ ] AC7：A/B 阶段 `libra status --short --untracked-files=all` 中的未提交改动只属于家族子卡写集与计划记账文件。
- [ ] AC8：A/B 阶段家族聚合守卫中的集成用例（`SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 在 `command_test` 中的全部卡特有用例）全部通过。
- [ ] AC9：A/B 阶段家族聚合守卫中的库内单元用例全部通过。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档已由 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 交付；本卡按「文档与网站验收命令」第 5 步交付网站页面。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 不适用（发布点不新增行为）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 家族提交身份守卫（「验证执行协议」第 9a 条）：`libra rev-list "$FAMILY_BASE_SHA..HEAD"`（非 0 退出即 STOP）列出的每个提交，或等于 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 中某张子卡卡内最后记录的 `COMMIT_SHA`，或为第 9a 条定义的独立记账提交（A/B 阶段、bump 前执行；AC1）
- [ ] VER2 家族提交完整性守卫（第 9a 条）：`SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 每张子卡卡内最后记录的 `COMMIT_SHA` 都出现在上述 `libra rev-list` 输出中（A/B 阶段、bump 前执行；AC2）
- [ ] VER3 线性链守卫（第 9e 条）：自 `HEAD` 按第 9e 条逐个回溯，每个提交恰有 1 个 `parent` 行，且在恰好走完上述 `libra rev-list` 输出的全部提交后到达 `FAMILY_BASE_SHA`（A/B 阶段、bump 前执行；AC3）
- [ ] VER4 子卡顺序守卫（第 9e 条）：上述回溯链（自 `FAMILY_BASE_SHA` 向 `HEAD`）上 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 的 `COMMIT_SHA` 出现顺序与「发布分组与并发窗口」登记的子卡顺序相同（A/B 阶段、bump 前执行；AC4）
- [ ] VER5 逐提交改动集守卫（第 9b 条）：每个家族子卡提交 `<c>` 的 `libra diff --name-only "<c>^" "<c>"`（非 0 退出即 STOP）只含该子卡 `Implementation write set` 中的本仓库路径或 `docs/development/plan/` 下的记账文件（A/B 阶段、bump 前执行；AC5）
- [ ] VER6 最终改动集守卫（第 9c 条）：`libra diff --name-only "$FAMILY_BASE_SHA" HEAD`（非 0 退出即 STOP）输出的每个路径都属于 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 的 `Implementation write set` 或 `docs/development/plan/` 下的记账文件（逐行比对，差集为空；A/B 阶段、bump 前执行；AC6）
- [ ] VER7 未提交改动集守卫：`libra status --short --untracked-files=all` 输出的每个路径都属于 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 的 `Implementation write set` 或 `docs/development/plan/` 下的记账文件（逐行比对，差集为空；A/B 阶段、bump 前执行；AC7）
- [ ] VER8 聚合守卫（「验证执行协议」第 1b 条）：`source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_replay_pick_unsigned_strips_stale_gpgsig) | test(=command::rebase_test::rebase_replay_pick_signs_openpgp) | test(=command::rebase_test::rebase_replay_reword_edit_unsigned_strip_stale_gpgsig) | test(=command::rebase_test::rebase_replay_fixup_squash_unsigned_strip_stale_gpgsig) | test(=command::rebase_test::rebase_replay_reword_edit_signs_openpgp) | test(=command::rebase_test::rebase_replay_fixup_squash_signs_openpgp) | test(=command::rebase_test::rebase_replay_amend_fixup_keep_unsigned_strip_stale_gpgsig) | test(=command::rebase_test::rebase_replay_amend_fixup_keep_signs_openpgp) | test(=command::rebase_test::rebase_replay_pick_signs_ssh) | test(=command::rebase_test::rebase_replay_squash_signs_ssh) | test(=command::rebase_test::rebase_replay_signoff_unsigned_strips_stale_gpgsig) | test(=command::rebase_test::rebase_replay_signoff_then_sign_pick) | test(=command::rebase_test::rebase_replay_reword_editor_rewrite_signs) | test(=command::rebase_test::rebase_replay_edit_amend_continue_signs) | test(=command::rebase_test::rebase_sign_failure_mid_replay_multi_pick_continue) | test(=command::rebase_test::rebase_sign_failure_mid_replay_multi_pick_abort) | test(=command::rebase_test::rebase_sign_failure_mid_replay_fold_continue) | test(=command::rebase_test::rebase_sign_failure_mid_replay_fold_abort) | test(=command::rebase_test::rebase_sign_unavailable_stops_then_continue_after_config_fix) | test(=command::rebase_test::rebase_sign_failpoint_inert_without_libra_test)'`（A/B 阶段、bump 前；exit 0，20 run / 20 passed / 0 failed；AC8）
- [ ] VER9 聚合守卫（「验证执行协议」第 1b 条）：`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=command::rebase::tests::finalize_replayed_commit_sign_failure_writes_nothing)'`（A/B 阶段、bump 前；exit 0，1 run / 1 passed / 0 failed；AC9）

**共享回归组（不计入 VER）:** 不适用（发布点只执行聚合守卫与 `T-2` 全量）。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-2: family release point`
**Dependencies:** `SA-08a`, `SA-08b`, `SA-08m`, `SA-08g`, `SA-08h`, `SA-08j`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（只提交子卡草稿的最终同步结果，不新增行为）（网站文件：`*.en.md` 为交付文件；`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`，另含版本面（以 `compat_version_surface_sync` 为准：`Cargo.toml`、`install.sh`、`install.ps1`）与工具链刷新的 `Cargo.lock`
**Docs and compatibility impact:** 仓库文档已由 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 交付；本卡按「文档与网站验收命令」第 5 步交付 `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-SA-01`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=发布rebase重放签名出口家族版本; recovery=前滚新版本; complete=yes; self-contained=yes; AC=9/12; VER=9/12; landing=0; prod-files=0; scope=S; deps=SA-08a,SA-08b,SA-08m,SA-08g,SA-08h,SA-08j,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-08j; release=family release point; split-from=N/A; exception=N/A`

---

### Task SA-08: `libra rebase -f` / `--force-rebase`（#780）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 已拆出 `SA-08a`（`SA-08a` 又拆出 `SA-08b`、`SA-08g`、`SA-08h`、`SA-08j`，`SA-08b` 又拆出 `SA-08m`），六者经 `REL-SA-01` 发布。在 `RebaseArgs` 新增 `-f`/`--force-rebase`（同一 clap 参数的短、长形式）：当前分支已是上游后代（现有「已是最新」短路）时仍逐个重放（经签名出口），使 `t7510#2`（`TC-0911`）的 `rebase -f HEAD^^` 在 `commit.gpgSign=true` 下生成重签的 `sixth-signed`、`seventh-signed`（#780 验收项 1、2）。R43 按用户 2026-11-19 05:17:51 UTC 决定收窄到 #780 的验收项：`-f` 与 `--onto`（`TC-2289` 的 merge 后端形态）、`--force-rebase --autosquash`（`TC-2488`）、`gpg.format=ssh`（`TC-0931`、`TC-2974`）及 `t7030#2`（`TC-2972`）的组合沿用既有路径，其专项验收登记 `DEFER-SA-19`；Libra 无 apply 后端：`--apply` 仍按现有诊断以 `LBR-UNSUPPORTED` 拒绝（`DEFER-SA-03`）。
**Out of scope:** 签名出口、SSH 格式、重放后改写与签名失败恢复（由 `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` 经 `REL-SA-01` 承接）；`-f` 与 `--onto`/`--autosquash`/SSH 签名组合的专项验收（`DEFER-SA-19`）；`-r`/`-s recursive`（`DEFER-SA-02`）；apply 后端（`--apply`）、`--show-current-patch` 与 `backup-git-rebase-todo`（`DEFER-SA-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `RebaseArgs` 无 `-f`/`--force-rebase`，传入报 `unexpected argument '-f' found`（`LBR-CLI-002`，exit 129） | `src/command/rebase.rs:258-264`; issue #780 |
| 「已是最新」短路直接返回（不重放）的完整条件：非 `--root`、无 `--onto`、`base_id == upstream_id`（上游是 HEAD 的祖先）、未给 `--autosquash`，且重放列表未被无关历史根重放预先填充 | `src/command/rebase.rs:3690-3698` |
| `--autosquash` 已存在；`--apply` 只有诊断性拒绝（`LBR-UNSUPPORTED`，apply 后端不存在） | `src/command/rebase.rs:325,368,842-848` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；AC1–AC4 的 fixture 为 `t7510#2` 形态：vault OpenPGP，`commit.gpgSign=false` 下依次提交 `sixth`、`seventh`（均无签名、`HEAD^^` 是二者的上游，分支相对 `HEAD^^` 已是最新）后设 `commit.gpgSign=true`；AC5–AC8 的 fixture：分支已是上游后代（`<upstream>` 之上恰有 2 个提交 `F1`、`F2`），运行时 `GIT_COMMITTER_DATE` 与原提交不同）

- [ ] AC1：`t7510#2`（`TC-0911`）`libra rebase -f HEAD^^` exit 0（#780 验收项 1）。
- [ ] AC2：AC1 之后 `libra rev-list --reverse HEAD^^..HEAD` 的树序列等于 `sixth`、`seventh` 的树序列（含个数相同；`sixth-signed`、`seventh-signed` 是这两个提交的重放结果）。
- [ ] AC3：AC1 之后 `HEAD^`（`sixth-signed`）：在以该提交父为起点的临时分支上 `libra merge --ff-only --verify-signatures <提交>` exit 0（原提交无签名，故同时证明已重放）。
- [ ] AC4：AC1 之后 `HEAD`（`seventh-signed`）：在以该提交父为起点的临时分支上 `libra merge --ff-only --verify-signatures <提交>` exit 0。
- [ ] AC5：长形式 `libra rebase --force-rebase <upstream>` exit 0。
- [ ] AC6：AC5 之后 `libra rev-list --reverse <upstream>..HEAD` 列出的每个提交的 OID 都不同于原分支同位置的提交（长形式在「已是最新」路径上同样强制重放）。
- [ ] AC7：AC5 之后该列表的树序列等于 `F1`、`F2` 的树序列（含个数相同，排除空列表或丢提交时 AC6 的空真）。
- [ ] AC8：AC5 之后该列表构成以 `<upstream>` 为起点的单父线性链。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：逐文件按「文档与网站验收命令」第 1（含 1d）、2、3 步交付验收（无需改动的文件按第 1a 步的无变化路径记录）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类，也没有失败路径（`--apply` 的既有 `LBR-UNSUPPORTED` 拒绝不变）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_force_rebase_signed_openpgp_tc0911)'`（new；AC1–AC4）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::rebase_test::rebase_force_rebase_long_form_replays_up_to_date_branch)'`（new；AC5–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::rebase_test::/) | test(/^command::rebase_interactive_test::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_noninteractive_history_controls`。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-1: C 组 bump 修改 install.sh 与 install.ps1（版本面）`
**Dependencies:** `REL-SA-01`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/rebase.rs`, `tests/command/rebase_test.rs`, `docs/commands/rebase.md`, `docs/commands/zh-CN/rebase.md`, `docs/development/commands/rebase.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/rebase.md`、`docs/commands/zh-CN/rebase.md`、`docs/development/commands/rebase.md`（`-f`/`--force-rebase`）、`COMPATIBILITY.md`（`rebase` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/rebase.en.md`。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=rebase强制重放; recovery=前滚新版本，降级按默认指引; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=REL-SA-01,DEP-SA-03,DEP-SA-10; writeset=序列化于 REL-SA-01; release=independent; split-from=N/A; exception=N/A`

---

### Task SA-09f: `sequence_state` 可选 OID 列的全零编解码函数（#907 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** R43 自原 `SA-05a` 改造（原卡撤回，编号不复用）：`REL-SA-02` 家族子卡 1/8，只做本地签名+DCO 提交。在 `src/internal/sequencer/mod.rs` 新增唯一的可选 OID 列编解码函数对（`ADR-SA-02`）：编码显式接收 `HashKind`，`None` → `ObjectHash::zero_str(kind)`、`Some(oid)` → 该 OID 的原字符串；解码只把恰为 `ObjectHash::zero_str(kind)` 的串视为 `None`（长度与 `kind` 不符的全零串按非法值报错），其它值经 `crate::internal::object_format::parse_hex_for_kind(kind, …)` 严格解析为 `Some`，解析失败（非十六进制或长度与 `kind` 不符）返回错误；两者都不读进程全局哈希类型。函数为 `pub`（`src/lib.rs:25` 的 `pub mod internal` 与 `src/internal/mod.rs:55` 的 `pub mod sequencer` 使其处于库的公开路径，接入前不触发 `dead_code`）；唯一消费者 `SA-09b` 在同一家族内接入，家族由 `REL-SA-02` 一次发布（G-02/G-08）。BLAKE3 与 SHA-256 的全零串相同（64 个 `0`），按 20/32 字节两种长度覆盖。
**Out of scope:** `am` 字段接线（由 `SA-09b` 承接）；`cherry-pick` 状态的全零编码与 unborn 非快进 pick（`DEFER-SA-17`）；`ORIG_HEAD` 投影（`DEFER-SA-20`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 全零 OID API；显式 kind 的解析函数（`parse_repo_oid` 读进程全局 kind，不用于本编解码） | `git-internal` 0.10.4 `src/hash.rs:347,356`; `src/internal/object_format.rs:47-57` |
| `sequence_state.head_orig`/`current_oid` 为 `TEXT NOT NULL` | `sql/migrations/2026070401_sequence_state.sql:6-15` |
| `src/internal/sequencer/mod.rs` 已有 `mod tests` | `src/internal/sequencer/mod.rs:1663` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；均为直接调用该函数对的纯函数单元测试（库内路径 `internal::sequencer::tests::<函数>`；不依赖 cwd，不设置进程全局哈希类型，nextest 每个用例独立进程，进程全局哈希类型为默认值）；`Some(<OID>)` 写出由 `SA-09b` 经 `AmState` 两个字段验收；非十六进制输入由 `parse_hex_for_kind` 的既有校验拒绝，不单列判据）

- [ ] AC1：`HashKind::Sha1` 下编码 `None` 得 `ObjectHash::zero_str(Sha1)`（40 个 `0`）。
- [ ] AC2：`HashKind::Sha256` 下编码 `None` 得 `ObjectHash::zero_str(Sha256)`（64 个 `0`）。
- [ ] AC3：`HashKind::Sha1` 下解码 `ObjectHash::zero_str(Sha1)` 得 `None`。
- [ ] AC4：`HashKind::Sha256` 下解码 `ObjectHash::zero_str(Sha256)` 得 `None`。
- [ ] AC5：`HashKind::Sha1` 下解码一个非零 40 位十六进制 OID 串得 `Some(<同一 OID>)`（旧行兼容）。
- [ ] AC6：`HashKind::Sha256` 下解码一个非零 64 位十六进制 OID 串得 `Some(<同一 OID>)`（SHA-256 仓库的已持久化非零 OID 可读回）。
- [ ] AC7：`HashKind::Sha256` 下解码一个非零 40 位十六进制串返回错误（按显式 `kind` 校验长度，不按进程全局哈希类型接受）。
- [ ] AC8：`HashKind::Sha256` 下解码 40 个 `0` 返回错误（只有长度与 `kind` 相符的全零串视为 `None`）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1 步验收；本卡无用户可见行为变化，网站第 2–4 步不适用（ER-06a 判定记录在 `Docs and compatibility impact`）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有 CLI 错误类：AC6–AC8 判定的是库内解码函数的返回错误。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=internal::sequencer::tests::optional_oid_column_zero_encoding_sha1_sha256)'`（new；AC1–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^internal::sequencer::/)'`。
**D 组:** 继承 `REL-SA-02` 的 D 组登记表（D5 由 `REL-SA-02` 的网站交付承担）。

**Full-suite trigger:** `N/A（family child；本卡新增的共享单一事实源函数（T-1）由 REL-SA-02 的 T-2 全量覆盖）`
**Dependencies:** `SA-08`, `DEP-SA-03`, `DEP-SA-04`, `DEP-SA-09`
**Deliverables:** `N/A`
**Implementation write set:** `src/internal/sequencer/mod.rs`, `docs/development/commands/am.md`
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/development/commands/am.md`（`sequence_state` 可选 OID 列的全零编码约定）。`docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`COMPATIBILITY.md` 与网站 `am.en.md`：`N/A`——本卡不改变任何用户可见行为，由 `SA-09` 同卡更新。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-02` 的 `immutable-release` 前滚）
**Migration and rollback:** 无 schema 迁移。存在暂停的 unborn 起点 `am`（`head_orig` 为全零 OID）时禁止直接降级，也禁止手工删除状态行；先 `libra am --abort` 回到 unborn（unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再通过上一已知良好 tag 重新安装。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-02`（1/8）
**C/D coverage from:** `REL-SA-02`
**Granularity:** `type=implementation; axis=sequence_state可选OID列全零编解码; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-08,DEP-SA-03,DEP-SA-04,DEP-SA-09; writeset=序列化于 SA-08; release=family child; split-from=SA-09; exception=N/A`

---

### Task SA-09b: `am` 状态字段接入全零编解码（#907 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `SA-09` 拆出（`REL-SA-02` 家族子卡 2/8，只做本地签名+DCO 提交）：按 `ADR-SA-02` 把 `AmState.head_orig`、`AmState.expected_head` 改为 `Option<ObjectHash>`，`to_sequence`/`from_sequence` 改为调用 `SA-09f` 在 `src/internal/sequencer/mod.rs` 新增的共享编解码函数（`None` ↔ `ObjectHash::zero_str(kind)`，非零值经 `parse_hex_for_kind(kind, …)` 严格解析），不改变用户可见行为（unborn 拒绝由 `SA-09` 移除）。`am` 每次开始都先以 `claim_start` 持久化状态（`src/command/am.rs:303-304`），故 unborn 上的成功路径也需要本卡的全零编码。
**Out of scope:** unborn 分支上的 `am` 行为（由 `SA-09`、`SA-09a`、`SA-09e`、`SA-09g` 承接）；共享编解码函数本身与其两种长度、非法输入覆盖（由 `SA-09f` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `AmState` 字段必填，`to_sequence` 以 `to_string` 写列、`from_sequence` 以 `parse_repo_oid` 严格解析 | `src/command/am.rs:139-175`; `src/internal/sequencer/mod.rs:123-128` |
| `start_am` 在应用任何邮件前以 `claim_start` 持久化 `head_orig`/`expected_head` | `src/command/am.rs:292-304` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；均为经 `AmState::to_sequence`/`from_sequence` 调用 `SA-09f` 共享编解码函数的纯函数单元测试，`HashKind::Sha1`（两种长度与非法输入已由 `SA-09f` 覆盖，本卡验证两个字段在两个方向上的接线））

- [ ] AC1：`head_orig=None` 写成 `head_orig` 列值 `ObjectHash::zero_str(Sha1)`。
- [ ] AC2：`head_orig=Some(<OID>)` 写成 `head_orig` 列值为该 OID 的原字符串。
- [ ] AC3：`head_orig` 列值 `ObjectHash::zero_str(Sha1)` 读回 `head_orig=None`。
- [ ] AC4：`head_orig` 非零列值读回 `Some(<同一 OID>)`（旧行兼容）。
- [ ] AC5：`expected_head=None` 写成 `current_oid` 列值 `ObjectHash::zero_str(Sha1)`。
- [ ] AC6：`expected_head=Some(<OID>)` 写成 `current_oid` 列值为该 OID 的原字符串。
- [ ] AC7：`current_oid` 列值 `ObjectHash::zero_str(Sha1)` 读回 `expected_head=None`。
- [ ] AC8：`current_oid` 非零列值读回 `expected_head=Some(<同一 OID>)`（旧行兼容）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1 步验收；本卡无用户可见行为变化，网站第 2–4 步不适用（ER-06a 判定记录在 `Docs and compatibility impact`）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类（不改变用户可见行为）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=command::am::tests::am_state_unborn_encoding_wiring)'`（new；AC1–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::am::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_mail_am_basic`。
**D 组:** 继承 `REL-SA-02` 的 D 组登记表（D5 由 `REL-SA-02` 的网站交付承担）。

**Full-suite trigger:** `N/A（family child；由 REL-SA-02 的 T-2 全量覆盖）`
**Dependencies:** `SA-09f`, `DEP-SA-03`, `DEP-SA-04`, `DEP-SA-09`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/am.rs`, `docs/development/commands/am.md`
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/development/commands/am.md`（`am` 状态全零编码约定）。`docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`COMPATIBILITY.md` 与网站 `am.en.md`：`N/A`——本卡不改变任何用户可见行为，由 `SA-09` 同卡更新。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-02` 的 `immutable-release` 前滚）
**Migration and rollback:** 无 schema 迁移。存在暂停的 unborn 起点 `am`（`head_orig` 为全零 OID）时禁止直接降级，也禁止手工删除状态行；先 `libra am --abort` 回到 unborn（unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再通过上一已知良好 tag 重新安装。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-02`（2/8）
**C/D coverage from:** `REL-SA-02`
**Granularity:** `type=implementation; axis=am状态字段接入全零编解码; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-09f,DEP-SA-03,DEP-SA-04,DEP-SA-09; writeset=序列化于 SA-09f; release=family child; split-from=SA-09; exception=N/A`

---

### Task SA-09: `libra am` 在 unborn 分支应用单封与多封补丁（#907）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `SA-09b` 的字段接线之上移除 `am requires an existing HEAD commit` 拒绝（`src/command/am.rs:284-291`），使 unborn 分支上首封补丁生成 root commit、落在原分支上，其余补丁继续应用，序列完成后释放状态（#907 验收项 1–3）；新建 `tests/command/am_test.rs` 并在 `tests/command/mod.rs` 注册、同步 `tests/INDEX.md`。本卡为 `REL-SA-02` 家族子卡（3/8）：只做本地签名+DCO 提交，不 bump、不推送、不发布。已拆出 `SA-09b`（状态字段接线）与 `SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i`（unborn 起点暂停后的 `--abort` 复原、中止保护、中断后重试与并发保护；R47 曾撤回 `SA-09a`、`SA-09e`，R49 因 GC-07/GC-08 恢复）；unborn 起点暂停后的 `--continue`/`--skip` 的专项验收登记 `DEFER-SA-20`。本卡对 `None` 的处理限于应用路径：开始时 `head_orig` 与 `expected_head` 均为 `None`，首封补丁以无父提交生成 root、其后各封以前一封的结果为父（`src/command/am.rs:545-546,1201-1205` 的当前 HEAD 比较改为可选值比较）；本卡对提交函数的修改（父为 `None` 时生成 root）也位于首封暂停后 `--continue` 的调用路径上，但 unborn 起点暂停后的 `--continue`/`--skip` 未验收（`DEFER-SA-20`），文档只推荐已验收的 `--abort`；`--abort` 的复原与中止保护见 `SA-09a`、`SA-09e`、`SA-09g`。
**Out of scope:** 状态字段接线（由 `SA-09f`、`SA-09b` 承接）；unborn 起点暂停后的 `--abort` 复原与中止保护（由 `SA-09a`、`SA-09e`、`SA-09g` 承接）；`--continue`/`--skip` 与 `am` 暂停行的 `ORIG_HEAD` 投影（`DEFER-SA-20`）；`format-patch` 邮件格式（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| unborn HEAD 被硬编码拒绝 | `src/command/am.rs:284-291` |
| `AmState` 字段必填，`from_sequence` 严格解析 | `src/command/am.rs:139-175`; `src/internal/sequencer/mod.rs:123-128` |
| `tests/command/` 无 `am_test.rs`；现有 `am` 用例在 `compat_mail_am_basic` | `tests/command/mod.rs`; `Cargo.toml` `[[test]] name = "compat_mail_am_basic"` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；fixture：来源仓库中依次提交 `c1`（新增 `a.txt`）、`c2`（修改 `a.txt`）、`c3`（新增 `b.txt`），以 `libra format-patch --root --stdout c1` 生成单封补丁（只含 `c1`）、以 `libra format-patch --root --stdout c3` 生成三封补丁 mbox（`--root` 包含 root 提交 `c1`，`src/command/format_patch.rs:120-122`；不用会排除左端的 `c1..c3`）；被测仓库为刚 `libra init` 的仓库，当前分支 `main` 为 unborn（`libra symbolic-ref HEAD` 输出 `refs/heads/main`），单封与三封用例各用一份独立的被测仓库；来自邮件的作者、作者日期与消息走既有的邮件→提交路径（`compat_mail_am_basic` 在已有提交的分支上守护），unborn 根提交与序列的这些元数据与 Git 的逐项对照登记 `DEFER-SA-20`，不单列 AC）

- [ ] AC1：`libra am <单封补丁>` exit 0。
- [ ] AC2：AC1 之后 HEAD 为无父提交（root commit）。
- [ ] AC3：AC1 之后 HEAD 的树等于 `c1` 的树（#907 验收项 1 的 `HEAD^{tree}` 等于来源提交的树）。
- [ ] AC4：AC1 之后 `libra symbolic-ref HEAD` 输出 `refs/heads/main`（root 提交落在原分支上，HEAD 未变为 detached）。
- [ ] AC5：AC1 之后 `sequence_state` 中没有 `kind='am'` 的行（序列完成后释放状态）。
- [ ] AC6：`libra am <三封补丁 mbox>` exit 0。
- [ ] AC7：AC6 之后 `libra rev-list --reverse HEAD` 列出的提交的树序列等于 `c1`、`c2`、`c3` 的树序列（含个数相同）。
- [ ] AC8：AC6 之后该列表的首个提交无父、其后每个提交恰有一个父且为列表中的前一个提交（自 root 起的线性链）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-02` 执行。另含测试索引同步（ER-06/GC-06 强制，不计入）：`rg -n '^mod am_test;' tests/command/mod.rs` 恰好 1 处命中；`rg -n 'am_test' tests/INDEX.md` 命中位于 `command_test` 行；`source .env.test && cargo nextest list --test command_test -T oneline -E 'test(=command::am_test::am_unborn_single_patch_creates_root_commit) | test(=command::am_test::am_unborn_series_matches_git_history)'` 按「验证执行协议」第 1 条解析恰好 2 行，第二字段分别等于这两个路径（任一不满足即 STOP；家族最终树由 `REL-SA-02` 的聚合守卫复跑这两个测试）。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类（本卡删除 `am requires an existing HEAD commit` 拒绝；unborn 起点暂停后的 `--abort` 复原与中止保护由 `SA-09a`、`SA-09e`、`SA-09g` 交付）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::am_test::am_unborn_single_patch_creates_root_commit)'`（new；AC1–AC5）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::am_test::am_unborn_series_matches_git_history)'`（new；AC6–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --no-tests=fail --test compat_mail_am_basic`；`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::am::/) | test(/^internal::sequencer::/)'`。
**D 组:** 继承 `REL-SA-02` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；本卡触及的 T-4（tests/command/mod.rs）表面由 REL-SA-02 的 T-2 全量覆盖）`
**Dependencies:** `SA-09b`, `DEP-SA-03`, `DEP-SA-04`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/am.rs`, `tests/command/am_test.rs`, `tests/command/mod.rs`, `tests/INDEX.md`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`docs/development/commands/am.md`（unborn 分支支持与降级前置）、`COMPATIBILITY.md`（`am` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`（本地草稿，由 `REL-SA-02` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-02` 的 `immutable-release` 前滚）
**Migration and rollback:** 无 schema 迁移。存在暂停的 unborn 起点 `am`（`head_orig` 为全零 OID）时禁止直接降级，也禁止手工删除状态行；先 `libra am --abort` 回到 unborn（unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再通过上一已知良好 tag 重新安装。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-02`（3/8）
**C/D coverage from:** `REL-SA-02`
**Granularity:** `type=implementation; axis=am在unborn分支应用补丁序列; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=SA-09b,DEP-SA-03,DEP-SA-04,DEP-SA-10; writeset=序列化于 SA-09b; release=family child; split-from=N/A; exception=N/A`

---

### Task SA-09a: unborn 起点第二封暂停后的 `--abort` 回到 unborn（#907）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `SA-09` 拆出（`REL-SA-02` 家族子卡 4/8；R47 曾按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回，R49 因 GC-07/GC-08 恢复（Codex R47 F-1/F-2、R48 A-1/F-1–F-4：`SA-09` 使 unborn 起点暂停可达，须有已验收的退出；R48 的「`--abort` 只释放状态」方案已撤销））：`SA-09` 移除 unborn 拒绝后，始于 unborn 的 `am` 可能在后续补丁处暂停（HEAD 为首封生成的 root，`head_orig=None`），本卡交付其恢复出口 `libra am --abort`：现有 `abort_am` 以 `head_orig` 硬重置（`src/command/am.rs:408-410`），本卡在 `head_orig=None` 时按 Git `builtin/am.c` 的 `am_abort`（缺失 `ORIG_HEAD` 时以空树 `clean_index` 并删除分支引用，符号 HEAD 保留）复原：成功结束、释放 `am` 序列状态、删除首封生成的分支引用使分支回到 unborn、HEAD 仍为指向原分支的符号引用、索引与工作区复原为 `am` 前状态并保留无关的未跟踪文件；本卡的复原步骤：①中止保护判定（`SA-09g` 交付 `head_orig=None` 会话的统一判定）；②以空树复原索引与工作区；③以既有 `Branch::delete_branch_result_with_conn`（保留其 linked worktree 检出保护与分支元数据级联删除，`src/internal/branch.rs:829-876`）删除首封生成的分支引用；④清除 `am` 状态。③④合并为同一 SQLite 写事务并置于②之后（中断后可重试）由 `SA-09h` 交付，事务内的期望 OID 比较（删除前分支被并发移动时拒绝）由 `SA-09i` 交付；在同一工作区上与 `--abort` 并发写入索引或工作区的其它进程的互斥不在本计划（与既有 `--abort` 相同，`DEFER-SA-24`）；既有的「暂停后分支出现新的后代提交即拒绝中止」保护（`src/command/am.rs:383-407`）在该路径上同样先于删除引用生效（`head_orig=None` 会话中由 `SA-09g` 的统一判定取代：当前 HEAD 存在且不等于 `expected_head` 即拒绝；第二封暂停后用户另行提交情形的专项验收登记 `DEFER-SA-20`）——这是 Libra 既有的有意差异：Git 的 `safe_to_abort` 检测到 HEAD 移动时只告警、清除 am 状态并成功返回、不回退（`builtin/am.c` 的 `am_abort`），Libra 拒绝中止并保留状态与提交；该差异写入 am 三份文档与 `COMPATIBILITY.md`。R43–R44 按用户 2026-11-19 05:17:51 UTC 决定收窄（原 `SA-09d` 并入本卡）：首封暂停（当前 HEAD 缺失）的 `--abort` 由 `SA-09e` 承接；unborn 起点暂停后的 `--continue`/`--skip` 登记 `DEFER-SA-20`，文档写明在其完成前以 `--abort` 退出。只做本地签名+DCO 提交，由 `REL-SA-02` 合并发布。
**Out of scope:** 干净无冲突的 unborn `am`（由 `SA-09` 承接）；首封暂停的 `--abort`（由 `SA-09e` 承接）；`head_orig=None` 会话的统一中止保护（由 `SA-09g` 承接，第二封暂停后用户另行提交情形的专项验收见 `DEFER-SA-20`）；中断后重试（由 `SA-09h` 承接）；判定与删除之间的并发移动（由 `SA-09i` 承接）；unborn 起点暂停后的 `--continue`/`--skip`、`am` 暂停行的 `ORIG_HEAD` 投影与真实暂停态 GC 黑盒（`DEFER-SA-20`）；`am` 暂停期间 `CHERRY_PICK_HEAD`/`REVERT_HEAD` 解析（`DEFER-SA-05`）；尚有待处理邮件时的 GC（`DEFER-SA-09`）；`format-patch`（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有 `abort_am`：暂停后出现后代提交即拒绝（`RepoStateInvalid`），否则以 `head_orig` 硬重置并以单独、可失败的 `sequencer::clear_am()` 释放状态 | `src/command/am.rs:381-414,383-407,408-410,412-414` |
| Git `am --abort` 检测到 HEAD 移动（`safe_to_abort` 为假）时告警、`am_destroy` 后成功返回、不回退；缺失 `ORIG_HEAD`（unborn 起点）时以空树清理索引与工作区并删除分支引用 | Git 源码 `builtin/am.c` 的 `am_abort`（`safe_to_abort`、`am_destroy`、`has_orig_head`、`clean_index`、`delete_ref`） |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；每个命令都在新进程中运行；fixture：刚 `libra init` 的仓库，当前分支 `main` 为 unborn（`libra symbolic-ref HEAD` 输出 `refs/heads/main`），运行 `libra am` 之前在工作区放一个与补丁无关、字节独特的未跟踪文件 `notes.txt`，并记录 `libra ls-files --stage` 输出（空）与工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）作为「am 前快照」；两封补丁的 mbox，首封新增 `a.txt`（干净应用并生成 root 提交 `R`），第二封修改 `a.txt` 中不存在的上下文而暂停（「第二封暂停」））

- [ ] AC1：「第二封暂停」时 `libra am <mbox>` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（第二封无法应用而暂停，fixture 的停止确由补丁冲突造成）。
- [ ] AC2：「第二封暂停」下 `libra am --abort` exit 0。
- [ ] AC3：AC2 之后 `libra rev-parse HEAD` 失败（首封生成的分支引用已删除，分支回到 unborn）。
- [ ] AC4：AC2 之后 `libra symbolic-ref HEAD` 输出 `refs/heads/main`（符号 HEAD 保留，未被删除或改指）。
- [ ] AC5：AC2 之后 `libra ls-files --stage` 的输出等于 am 前快照（已应用补丁与冲突的条目不残留）。
- [ ] AC6：AC2 之后工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）等于 am 前快照（已应用补丁写入的文件被移除，`notes.txt` 及其字节保留）。
- [ ] AC7：AC2 之后 `sequence_state` 中没有 `kind='am'` 的行（序列状态已释放）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-02` 执行。文档含 unborn 起点暂停后以 `--abort` 回到 unborn 的语义、`--continue`/`--skip` 尚未支持（`DEFER-SA-20`）与降级前置。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类。fixture 的「第二封暂停」命中既有错误类「`am` 补丁无法应用」（`am_conflict`，`LBR-CONFLICT-001`，`src/command/am.rs:1339-1343`），按「验证执行协议」第 11c 条作为其余路径断言稳定码（AC1）；#907 未要求其人读诊断。第二封暂停后用户另行提交时的中止拒绝（`LBR-REPO-003`，按 `SA-09g` 的统一判定）的专项验收登记 `DEFER-SA-20`（第 11a 条）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::am_test::am_unborn_second_patch_abort_restores_unborn)'`（new；AC1–AC7）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::am_test::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_mail_am_basic`。
**D 组:** 继承 `REL-SA-02` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；由 REL-SA-02 的 T-2 全量覆盖）`
**Dependencies:** `SA-09`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/am.rs`, `tests/command/am_test.rs`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（网站文件：`*.en.md` 为交付文件；`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`docs/development/commands/am.md`（unborn 起点暂停后的 `--abort`、`--continue`/`--skip` 尚未支持与降级前置）、`COMPATIBILITY.md`（`am` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`（本地草稿，由 `REL-SA-02` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-02` 的 `immutable-release` 前滚）
**Migration and rollback:** 无 schema 迁移。存在暂停的 unborn 起点 `am`（`head_orig` 为全零 OID）时禁止直接降级，也禁止手工删除状态行；先 `libra am --abort` 回到 unborn（unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再通过上一已知良好 tag 重新安装。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-02`（4/8）
**C/D coverage from:** `REL-SA-02`
**Granularity:** `type=implementation; axis=unborn起点第二封暂停后abort回到unborn; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-09,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-09; release=family child; split-from=SA-09; exception=N/A`

---

### Task SA-09d: 已撤回（不得开工）

**Status:** withdrawn。R43 按用户 2026-11-19 05:17:51 UTC 决定撤回并入 `SA-09a`（原：unborn `am` 后续补丁冲突后 `--abort` 回到 unborn）。其核心判据（成功退出、分支回到 unborn、符号 HEAD 保留、索引与工作区复原）在 `SA-09a` 以「第二封暂停」fixture 验收。（`SA-09a` 曾于 R47 撤回、R49 恢复。）本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task SA-09e: unborn 起点首封暂停后的 `--abort`（当前 HEAD 缺失，#907）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `SA-09a` 拆出（`REL-SA-02` 家族子卡 5/8；R43 曾拟并入 `SA-09a`，R44 因 G-03 的 8 条上限保留本卡，编号未变；R47 曾撤回，R49 因 GC-07/GC-08 恢复（Codex R47 F-1/F-2、R48 A-1/F-1–F-4：`SA-09` 使 unborn 起点暂停可达，须有已验收的退出；R48 的「`--abort` 只释放状态」方案已撤销））：unborn 分支上首封补丁即暂停（当前 HEAD 缺失、`head_orig=None`、`expected_head=None`）后，跨进程的 `libra am --abort` 成功结束、释放 `am` 序列状态、分支仍为 unborn 且符号 HEAD 保留，索引与工作区复原为 `am` 前状态并保留无关的未跟踪文件。现有 `abort_am` 在当前 HEAD 缺失时直接拒绝（`HEAD disappeared during am`，`src/command/am.rs:389-392`），本卡改为按 Git 以空树复原（Git `builtin/am.c` 的 `am_abort` 对缺失的当前 HEAD 同样以空树 `clean_index`）；若暂停后用户已在该分支上提交出新的 root（HEAD 由缺失变为存在），沿用 Libra 既有中止保护的语义（其验收由 `SA-09g` 承接）（`src/command/am.rs:383-407`：拒绝中止、保留状态、提交与分支引用）；这与 Git 不同（Git 的 `safe_to_abort` 为假时告警、清除 am 状态并成功返回、不回退），差异与 `SA-09a` 一并写入 am 三份文档与 `COMPATIBILITY.md`。只做本地签名+DCO 提交，由 `REL-SA-02` 合并发布。
**Out of scope:** `--continue`/`--skip`（`DEFER-SA-20`；文档写明 unborn 起点暂停后只支持已验收的 `--abort`，`--continue`/`--skip` 尚未验收）；第二封暂停的 `--abort`（由 `SA-09a` 承接）；首封暂停后用户另行提交时的中止保护（由 `SA-09g` 承接）；`ORIG_HEAD` 投影与 GC（`DEFER-SA-20`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有 `abort_am` 在当前 HEAD 缺失时直接拒绝 | `src/command/am.rs:381-414,389-392` |
| Git `am --abort` 对缺失的当前 HEAD 以空树 `clean_index` | Git 源码 `builtin/am.c` 的 `am_abort`（`has_curr_head`、`clean_index`） |
| Git `am --abort` 在 HEAD 移动时告警、`am_destroy` 后成功返回、不回退 | Git 源码 `builtin/am.c` 的 `am_abort`/`safe_to_abort` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；每个命令都在新进程中运行；fixture：刚 `libra init` 的仓库，当前分支 `main` 为 unborn（`libra symbolic-ref HEAD` 输出 `refs/heads/main`），运行 `libra am` 之前在工作区放一个与补丁无关、字节独特的未跟踪文件 `notes.txt`，并记录 `libra ls-files --stage` 输出（空）与工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）作为「am 前快照」；两封补丁的 mbox，首封修改空树中不存在的文件而暂停（「首封暂停」，当前 HEAD 缺失））

- [ ] AC1：「首封暂停」时 `libra am <mbox>` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（首封无法应用而暂停，fixture 的停止确由补丁冲突造成）。
- [ ] AC2：「首封暂停」下 `libra am --abort` exit 0（当前 HEAD 缺失时不再拒绝）。
- [ ] AC3：AC2 之后 `libra symbolic-ref HEAD` 输出 `refs/heads/main`（符号 HEAD 保留）。
- [ ] AC4：AC2 之后 `libra rev-parse HEAD` 失败（分支仍为 unborn，未写入任何 OID）。
- [ ] AC5：AC2 之后 `sequence_state` 中没有 `kind='am'` 的行（序列状态已释放）。
- [ ] AC6：AC2 之后 `libra ls-files --stage` 的输出等于 am 前快照。
- [ ] AC7：AC2 之后工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）等于 am 前快照（`notes.txt` 及其字节保留）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-02` 执行。文档含首封暂停后 `--abort` 回到 unborn 的语义与降级前置。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类。fixture 的「首封暂停」命中既有错误类「`am` 补丁无法应用」（`am_conflict`，`LBR-CONFLICT-001`，`src/command/am.rs:1339-1343`），按「验证执行协议」第 11c 条作为其余路径断言稳定码（AC1）；#907 未要求其人读诊断。本卡把当前 HEAD 缺失且会话始于 unborn 时 `abort_am` 的既有拒绝（`HEAD disappeared during am`，`src/command/am.rs:389-392`）改为按 Git 以空树复原，该拒绝不再出现于此情形；暂停后用户另行提交时的中止拒绝由 `SA-09g` 验收。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::am_test::am_unborn_first_patch_abort_restores_unborn)'`（new；AC1–AC7）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::am_test::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_mail_am_basic`。
**D 组:** 继承 `REL-SA-02` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；由 REL-SA-02 的 T-2 全量覆盖）`
**Dependencies:** `SA-09a`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/am.rs`, `tests/command/am_test.rs`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（网站文件：`*.en.md` 为交付文件；`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`docs/development/commands/am.md`（unborn 起点首封暂停后的 `--abort`）、`COMPATIBILITY.md`（`am` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`（本地草稿，由 `REL-SA-02` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-02` 的 `immutable-release` 前滚）
**Migration and rollback:** 无 schema 迁移。存在暂停的 unborn 起点 `am`（`head_orig` 为全零 OID）时禁止直接降级，也禁止手工删除状态行；先 `libra am --abort` 回到 unborn（unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再通过上一已知良好 tag 重新安装。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-02`（5/8）
**C/D coverage from:** `REL-SA-02`
**Granularity:** `type=implementation; axis=unborn起点首封暂停后abort; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-09a,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-09a; release=family child; split-from=SA-09a; exception=N/A`

---

### Task SA-09g: unborn 起点会话中 HEAD 已不同于暂停点时拒绝 `libra am --abort`（#907 安全前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** R48 新增、R49–R51 重定义（`REL-SA-02` 家族子卡 6/8，只做本地签名+DCO 提交，由 `REL-SA-02` 合并发布）：`SA-09a`、`SA-09e` 的复原会删除首封生成的分支引用并以空树复原索引与工作区；若暂停后分支已出现用户提交，复原会丢弃它们。既有中止保护只拒绝「当前 HEAD 是 `expected_head` 的后代」（`src/command/am.rs:393-407`），不覆盖 `expected_head=None`（首封暂停）或与暂停点无关的提交。本卡令 `abort_am` 在 `head_orig=None` 的会话中改用统一的 fail-closed 判定：当前 HEAD 存在且不等于 `expected_head`（含首封暂停后用户提交的 root、第二封暂停后的后代或无关提交）即在任何写入前拒绝，沿用既有错误的消息与稳定码（`the branch gained commits since am paused; refusing to discard them with a hard reset`，`RepoStateInvalid`/`LBR-REPO-003`，`src/command/am.rs:397-401`），保留 `am` 状态、用户提交、符号 HEAD、索引与工作区；该会话没有可回退的提交，既有提示（`libra reset --hard <OID>`）改为 [`the branch was unborn when am paused, so there is no earlier tip to reset to; save the new commits on another branch first ('libra branch <name>')`、`the new commits stay reachable via the reflog either way`]（第 11a 条：提示改写以人读判据 AC3 验收）。这与 Git 不同（Git 的 `safe_to_abort` 为假时告警并清除 am 状态、不回退），与 `SA-09a` 的既有有意差异一致，写入 am 三份文档与 `COMPATIBILITY.md`。以首封暂停后用户提交 root 的情形为代表路径全量验收；第二封暂停后用户另行提交的情形（同一判定）的专项验收登记 `DEFER-SA-20`。
**Out of scope:** 无用户提交时的复原路径（由 `SA-09a`、`SA-09e` 承接）；中断后重试（由 `SA-09h` 承接）；第二封暂停后用户另行提交情形的专项验收与 `--continue`/`--skip`、`ORIG_HEAD`/GC（`DEFER-SA-20`）；`head_orig` 非 `None` 会话的既有保护（行为不变，由共享回归组守护）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 既有中止保护：当前 HEAD 不等于 `expected_head` 且为其后代时以 `RepoStateInvalid` 拒绝、提示 `libra reset --hard <expected>`，先于硬重置 | `src/command/am.rs:386-407` |
| `am` 补丁无法应用的暂停错误 | `src/command/am.rs:1339-1343` |
| Git `am --abort` 在 `safe_to_abort` 为假时告警、`am_destroy` 后成功返回、不回退 | Git 源码 `builtin/am.c` 的 `am_abort`/`safe_to_abort` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；每个命令都在新进程中运行；fixture（全部在仓库外的隔离 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：刚 `libra init` 的仓库，当前分支 `main` 为 unborn；两封补丁的 mbox，首封修改空树中不存在的 `a.txt` 而暂停（「首封暂停」，当前 HEAD 缺失）；暂停后写入字节独特的 `a.txt`、`libra add a.txt` 并 `libra commit -m user`（须 exit 0）生成 root 提交 `U`；随后把 `a.txt` 改为另一份字节独特的内容并 `libra add a.txt`（已暂存改动），再把 `a.txt` 改为第三份内容（未暂存改动）；运行 `libra am --abort` 之前记录「中止前快照」：`libra ls-files --stage` 输出与工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定））

- [ ] AC1：「首封暂停」时 `libra am <mbox>` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-CONFLICT-001`（fixture 的停止确由补丁无法应用造成）。
- [ ] AC2：用户提交 `U` 并留下改动之后 `libra am --abort` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-REPO-003`（拒绝中止）。
- [ ] AC3：AC2 的人读 stderr 中以 `Hint: ` 开头的行序列恰为 [`Hint: the branch was unborn when am paused, so there is no earlier tip to reset to; save the new commits on another branch first ('libra branch <name>')`, `Hint: the new commits stay reachable via the reflog either way`]（一次序列比较：改写后的保存建议存在，且不再出现 `libra reset --hard`）。
- [ ] AC4：AC2 之后 `libra rev-parse refs/heads/main` 等于 `U`（用户 root 提交与分支引用保留）。
- [ ] AC5：AC2 之后 `libra symbolic-ref HEAD` 输出 `refs/heads/main`（符号 HEAD 保留）。
- [ ] AC6：AC2 之后 `libra ls-files --stage` 的输出等于中止前快照（已暂存改动保留）。
- [ ] AC7：AC2 之后工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）等于中止前快照（未暂存改动保留）。
- [ ] AC8：AC2 之后 `sequence_state` 中仍有 `kind='am'` 的行（会话未被清除）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-02` 执行。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类（消息与稳定码沿用既有中止保护）。AC1 的暂停命中既有错误类「`am` 补丁无法应用」（`LBR-CONFLICT-001`），AC2 的拒绝命中既有错误类「暂停后出现新提交时拒绝中止」（`LBR-REPO-003`，`src/command/am.rs:393-407`）——本卡把其判定在 `head_orig=None` 的会话中改为「当前 HEAD 存在且不等于 `expected_head`」；两者按第 11c 条各断言稳定码。提示改写按第 11a 条以人读判据 AC3 验收；#907 未要求人读诊断。第二封暂停后用户另行提交情形（同一判定的其余路径）登记 `DEFER-SA-20`。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::am_test::am_unborn_first_pause_user_root_blocks_abort)'`（new；AC1–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::am_test::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_mail_am_basic`。
**D 组:** 继承 `REL-SA-02` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；由 REL-SA-02 的 T-2 全量覆盖）`
**Dependencies:** `SA-09e`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/am.rs`, `tests/command/am_test.rs`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`docs/development/commands/am.md`（unborn 起点会话中 HEAD 已不同于暂停点时拒绝 `--abort` 及其提示，与 Git 的差异）、`COMPATIBILITY.md`（`am` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`（本地草稿，由 `REL-SA-02` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-02` 的 `immutable-release` 前滚）
**Migration and rollback:** 无 schema 迁移。存在暂停的 unborn 起点 `am`（`head_orig` 为全零 OID）时禁止直接降级，也禁止手工删除状态行；先 `libra am --abort` 回到 unborn（unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再通过上一已知良好 tag 重新安装。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-02`（6/8）
**C/D coverage from:** `REL-SA-02`
**Granularity:** `type=implementation; axis=unborn起点am会话的中止保护; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-09e,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-09e; release=family child; split-from=SA-09a; exception=N/A`

---

### Task SA-09h: unborn 起点会话 `libra am --abort` 的删除与清除原子化及中断后重试（#907 GC-08）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** R51 新增（`REL-SA-02` 家族子卡 7/8，只做本地签名+DCO 提交，由 `REL-SA-02` 合并发布；回应 Codex R50 A-1/F-1/F-2）：`SA-09a` 的复原分步执行（空树复原、删除分支引用、清除状态），中断可能留下「分支引用已删而会话仍在」的中间态。本卡交付其原子化：把删除分支引用（既有 `Branch::delete_branch_result_with_conn`，不变式保留）与清除 `am` 状态（`sequencer::clear_am_with_conn`，`src/internal/sequencer/mod.rs:885`）合并为同一 SQLite 写事务并置于空树复原之后；事务前中断时分支引用与 `am` 状态都保持不变（当前 HEAD 仍等于 `expected_head`），再次运行 `--abort` 重复复原（幂等）与事务。本卡新增测试 failpoint `LIBRA_TEST_AM_FAIL_IN_ABORT_TXN`（在该写事务内、删除分支引用之后与清除 `am` 状态之前，先经同一事务连接确认分支引用已不存在，再以消息 `test-injected am abort failure after deleting the branch ref in the transaction` 返回错误，使事务回滚；若确认不成立则以不同消息失败；沿用既有 `test_failpoint_enabled`，只在设置 `LIBRA_TEST` 的进程中生效，`src/command/am.rs:756-758`）并验收：注入后分支引用与会话都仍在（删除随事务回滚，证明删除与清除同属一个事务；若二者分别提交，注入后分支引用已被删除），索引与工作区已复原（证明中断点位于复原之后）；不带注入重试 `--abort` 成功，分支回到 unborn、符号 HEAD 保留并释放会话。事务之前的中断不改动分支引用与会话，重试走同一路径。注入失败是测试专用路径，不属用户可见错误类。
**Out of scope:** 无中断的复原（由 `SA-09a`、`SA-09e` 承接）；中止保护（由 `SA-09g` 承接）；`--continue`/`--skip`（`DEFER-SA-20`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 既有 `am` 测试 failpoint 只在设置 `LIBRA_TEST` 时生效；测试 harness 为子进程设置 `LIBRA_TEST=1` | `src/command/am.rs:305,756-758`; `tests/command/mod.rs:375` |
| 分支删除与 `am` 状态清除都有接受同一连接的 `_with_conn` 变体 | `src/internal/branch.rs:829`; `src/internal/sequencer/mod.rs:885` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；每个命令都在新进程中运行；fixture（全部在仓库外的隔离 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下）：刚 `libra init` 的仓库，当前分支 `main` 为 unborn，运行 `libra am` 之前在工作区放一个字节独特的未跟踪文件 `notes.txt` 并记录 `libra ls-files --stage` 输出（空）与工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）作为「am 前快照」；两封补丁的 mbox，首封新增 `a.txt`（生成 root 提交 `R`），第二封修改 `a.txt` 中不存在的上下文而暂停；先以 `LIBRA_TEST_AM_FAIL_IN_ABORT_TXN=1` 运行一次 `libra am --abort`（「注入中止」），再不带该变量运行 `libra am --abort`（「重试」））

- [ ] AC1：「注入中止」的人读 stderr 含 `test-injected am abort failure after deleting the branch ref in the transaction`（failpoint 在同一事务内确认引用已删除后才输出该消息，证明注入点位于事务内的删除之后）。
- [ ] AC2：「注入中止」之后 `libra rev-parse refs/heads/main` 等于 `R`（事务内已执行的删除随回滚撤销；若删除与清除分别提交，此处引用已不存在）。
- [ ] AC3：「注入中止」之后 `sequence_state` 中仍有 `kind='am'` 的行（会话保留，可重试）。
- [ ] AC4：「注入中止」之后 `libra ls-files --stage` 的输出等于 am 前快照（中断发生在空树复原之后）。
- [ ] AC5：「注入中止」之后工作区「路径 → `libra hash-object`」映射（路径不含 `.libra/` 与测试 harness 在 cwd 下创建的 `.libra-test-home/`；一次映射比较，路径集合与各文件字节同时判定）等于 am 前快照（`notes.txt` 保留）。
- [ ] AC6：「重试」exit 0。
- [ ] AC7：「重试」之后 `libra rev-parse HEAD` 失败（首封生成的分支引用已删除，分支回到 unborn）。
- [ ] AC8：「重试」之后 `sequence_state` 中没有 `kind='am'` 的行。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-02` 执行。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有用户可见的失败路径（「注入中止」是只在设置 `LIBRA_TEST` 的测试进程中生效的 failpoint，不属用户可见错误类），也没有新增或改变的错误类。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::am_test::am_unborn_abort_txn_rollback_then_retry)'`（new；AC1–AC8）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::am_test::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_mail_am_basic`。
**D 组:** 继承 `REL-SA-02` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；由 REL-SA-02 的 T-2 全量覆盖）`
**Dependencies:** `SA-09g`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/am.rs`, `tests/command/am_test.rs`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`docs/development/commands/am.md`（unborn 起点会话 `--abort` 中断后再次运行即完成复原；开发文档另写写入顺序与事务边界）、`COMPATIBILITY.md`（`am` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`（本地草稿，由 `REL-SA-02` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-02` 的 `immutable-release` 前滚）
**Migration and rollback:** 无 schema 迁移。存在暂停的 unborn 起点 `am`（`head_orig` 为全零 OID）时禁止直接降级，也禁止手工删除状态行；先 `libra am --abort` 回到 unborn（unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再通过上一已知良好 tag 重新安装。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-02`（7/8）
**C/D coverage from:** `REL-SA-02`
**Granularity:** `type=implementation; axis=unborn起点am中止的原子化与重试; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=SA-09g,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-09g; release=family child; split-from=SA-09a; exception=N/A`

---

### Task SA-09i: unborn 起点会话 `libra am --abort` 删除前比较分支引用（#907 并发保护）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** R52 新增、R53 重定义（`REL-SA-02` 家族子卡 8/8，只做本地签名+DCO 提交，由 `REL-SA-02` 合并发布；回应 Codex R51 A-1/F-1、R52 F-1/F-3）：`SA-09a` 的中止保护判定先于空树复原与 `SA-09h` 的删除事务；若另一进程在判定之后、事务之前移动了该分支，删除会丢弃新的分支 tip。本卡在 `SA-09h` 的写事务内、调用既有删除函数之前读取并比较当前分支引用：`expected_head=Some(oid)` 时须仍指向 `oid` 才删除；`expected_head=None`（首封暂停）时须仍不存在，此时不调用删除、只清除 `am` 状态（不以「删除恰 1 行」作为条件）；比较不成立时回滚事务、保留会话，并以 `SA-09g` 的同一中止保护错误（`LBR-REPO-003`）拒绝；删除仍经既有 `Branch::delete_branch_result_with_conn`（linked worktree 检出保护与分支元数据级联不变，`src/internal/branch.rs:829-876`），其守卫或级联失败时整个事务回滚（fail-closed）。SQLite 写事务使比较与删除之间不会提交其它写入。本卡新增测试 failpoint `LIBRA_TEST_AM_ABORT_MOVE_BRANCH_TO=<OID>`（在空树复原之后、事务之前把当前分支引用设为给定提交、引用不存在时创建它，模拟并发写入；沿用既有 `test_failpoint_enabled`，`src/command/am.rs:756-758`）以第二封暂停（`expected_head=Some`）与首封暂停（`expected_head=None`）两种形态验收引用级保护。在判定与复原之间并发写入同一工作区或索引的其它进程（未提交改动）的保护不在本计划：复原发生在比较之前，此时被移动后的提交与引用不丢失、可用 `libra reset --hard` 取回其工作区，跨进程工作区写入互斥登记 `DEFER-SA-24`。
**Out of scope:** 无并发移动的复原与原子化（由 `SA-09a`、`SA-09e`、`SA-09h` 承接）；判定前已出现的用户提交（由 `SA-09g` 承接）；并发进程的未提交工作区/索引写入（`DEFER-SA-24`）；`--continue`/`--skip`（`DEFER-SA-20`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有分支删除按名称查询后删除，含 linked worktree 检出保护与分支元数据级联，不比较期望 OID | `src/internal/branch.rs:829-876` |
| 现有中止保护判定先于复原执行 | `src/command/am.rs:386-414` |
| 既有 `am` 测试 failpoint 只在设置 `LIBRA_TEST` 时生效；测试 harness 为子进程设置 `LIBRA_TEST=1` | `src/command/am.rs:756-758`; `tests/command/mod.rs:375` |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；每个命令都在新进程中运行；fixture（全部在仓库外的隔离 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB` 下），两种形态各用一份刚 `libra init` 的被测仓库（当前分支 `main` 为 unborn）：「第二封形态」——两封补丁的 mbox，首封新增 `a.txt`（生成 root 提交 `R`），第二封修改 `a.txt` 中不存在的上下文而暂停；暂停后以 `libra commit --allow-empty -m concurrent` 生成 `R` 的子提交 `X`，再以 `libra update-ref refs/heads/main R` 把分支移回 `R`（`X` 保留为对象），然后以 `LIBRA_TEST_AM_ABORT_MOVE_BRANCH_TO=X` 运行 `libra am --abort`；「首封形态」——两封补丁的 mbox，首封修改空树中不存在的 `a.txt` 而暂停（当前 HEAD 缺失）；暂停后以 `libra commit-tree "$(libra write-tree)" -m concurrent` 生成不挂在任何引用上的 root 提交 `Y`，然后以 `LIBRA_TEST_AM_ABORT_MOVE_BRANCH_TO=Y` 运行 `libra am --abort`）

- [ ] AC1：「第二封形态」的 `libra am --abort` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-REPO-003`（比较不成立，拒绝中止）。
- [ ] AC2：AC1 之后 `libra rev-parse refs/heads/main` 等于 `X`（被并发移动后的分支 tip 未被删除）。
- [ ] AC3：AC1 之后 `libra symbolic-ref HEAD` 输出 `refs/heads/main`（符号 HEAD 保留）。
- [ ] AC4：AC1 之后 `sequence_state` 中仍有 `kind='am'` 的行（事务回滚，会话保留）。
- [ ] AC5：「首封形态」的 `libra am --abort` 的结构化错误报告（`parse_cli_error_stderr` 解析的 `CliErrorReport`）的 `error_code` 为 `LBR-REPO-003`（`expected_head=None` 而分支已被并发创建，拒绝中止）。
- [ ] AC6：AC5 之后 `libra rev-parse refs/heads/main` 等于 `Y`（并发创建的分支引用未被删除）。
- [ ] AC7：AC5 之后 `sequence_state` 中仍有 `kind='am'` 的行（未无条件清除会话）。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档按「文档与网站验收命令」第 1（含 1d）步验收；网站按第 4 步只准备本地草稿，提交、推送与部署核验由 `REL-SA-02` 执行。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 本卡没有新增或改变的错误类：比较不成立时沿用 `SA-09g` 的中止保护错误（消息与 `LBR-REPO-003` 不变，提示为 `SA-09g` 已验收的改写），两种形态按第 11c 条各以 AC1、AC5 断言稳定码；#907 未要求其人读诊断。failpoint 只在测试进程中生效，不构成用户可见错误类。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::am_test::am_unborn_abort_refuses_when_branch_moved_before_delete)'`（new；AC1–AC4）
- [ ] VER2 `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::am_test::am_unborn_first_pause_abort_refuses_when_branch_created_before_txn)'`（new；AC5–AC7）

**共享回归组（不计入 VER）:** `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::am_test::/)'`；`source .env.test && cargo nextest run --no-tests=fail --test compat_mail_am_basic`。
**D 组:** 继承 `REL-SA-02` 的 D 组登记表 D1–D6。

**Full-suite trigger:** `N/A（family child；由 REL-SA-02 的 T-2 全量覆盖）`
**Dependencies:** `SA-09h`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/am.rs`, `tests/command/am_test.rs`, `docs/commands/am.md`, `docs/commands/zh-CN/am.md`, `docs/development/commands/am.md`, `COMPATIBILITY.md`, `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（网站文件仅本地草稿）（`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是网站第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `N/A`（family child）
**Files likely touched:** 同 `Implementation write set`
**Docs and compatibility impact:** `docs/commands/am.md`、`docs/commands/zh-CN/am.md`、`docs/development/commands/am.md`（unborn 起点会话 `--abort` 在删除前分支被移动时拒绝并保留会话）、`COMPATIBILITY.md`（`am` 既有条目行内追加）、`$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`（本地草稿，由 `REL-SA-02` 交付）。
**Rollback mode:** `revert`（家族窗口内未推送前在本地 `libra revert` 本卡提交，网站只丢弃本卡未提交草稿；推送后按 `REL-SA-02` 的 `immutable-release` 前滚）
**Migration and rollback:** 无 schema 迁移。存在暂停的 unborn 起点 `am`（`head_orig` 为全零 OID）时禁止直接降级，也禁止手工删除状态行；先 `libra am --abort` 回到 unborn（unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再通过上一已知良好 tag 重新安装。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-SA-02`（8/8）
**C/D coverage from:** `REL-SA-02`
**Granularity:** `type=implementation; axis=unborn起点am中止的引用级并发保护; recovery=家族窗口内本地revert，推送后前滚; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=SA-09h,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-09h; release=family child; split-from=SA-09a; exception=N/A`

---

### Task SA-09c: 已撤回（不得开工）

**Status:** withdrawn。R43 按用户 2026-11-19 05:17:51 UTC 决定撤回（原：`am` 暂停行的 `ORIG_HEAD` 投影与真实 unborn 暂停态 GC 黑盒）。#907 只要求 unborn 分支上的 `am` 应用补丁序列；`am` 暂停行的 `ORIG_HEAD` 现状以 `unknown sequence kind 'am'` 报错（不会暴露全零 OID），GC 对全零 OID 的跳过是既有逻辑（`src/command/maintenance.rs:4094-4102`）；二者的专项验收登记 `DEFER-SA-20`。本计划不再写 `src/internal/pseudo_ref.rs`。本编号不再拥有写集、验收标准或发布边界，不计入 29 张活动卡，编号不复用。

---

### Task REL-SA-02: 发布unborn-am家族版本（#907）

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** `REL-SA-02` 家族唯一发布点：A/B 阶段在包含 `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 最终变更的树上核对家族提交序列（提交身份、逐提交改动集、最终改动集）与未提交改动并跑家族聚合守卫，ER-05 评审后进入 C 组：按 ER-08 `patch + 1`、版本面 parity（`compat_version_surface_sync`）、在 bump 后的树上跑 `T-2` 全量，再提交、经 PR 合入并 `gh release create` 发布；CLI 发布后按「文档与网站验收命令」第 5 步交付家族网站页面。不引入新行为。**强制门（ER-04/ER-05/ER-07，不计入 G-03）：** 子卡 `Acceptance=locally-accepted` 与各自 ER-05 评审 `PASS` 已在子卡记录；子卡本地提交与本卡发布提交都通过「验证执行协议」第 7 条的同一提交签名与 DCO 校验；C 组 bump 后、提交前 `source .env.test && cargo nextest run --no-tests=fail --test compat_version_surface_sync`、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿；PR 合入后、`gh release create` 之前按「验证执行协议」第 10 条执行合入树发布前门；发布之后的证据（D3 的 tag 与 release note、D4 产物可获取性、D5 网站部署、D6 合入树一致的远端记录）全部属 D 组。
**Out of scope:** 修改业务源码或测试（全部在 `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 共同构成 unborn `am` 的状态编码、字段接线、入口与其暂停态的 `--abort` 复原、中止保护、中断后重试和并发保护（`SA-09f` 的函数没有家族外消费者，`SA-09b` 的接线在 `SA-09` 移除拒绝前不可达，`SA-09` 使 unborn 起点暂停可达而其退出由 `SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 交付），按 G-02/G-08 只能在同一发布点对外发布 | 本计划「发布分组与并发窗口」`REL-SA-02` 行 |

**Acceptance criteria:**（每条只陈述 1 个可独立判定的性质，按 G-03 各计 1 项；ER-04/ER-05/ER-07 强制门与合入/发布后的 D 组见 Description，不计入）

- [ ] AC1：A/B 阶段 `FAMILY_BASE_SHA..HEAD` 区间内的每个提交都是家族子卡记录的提交或独立记账提交（无插入的其它提交）。
- [ ] AC2：A/B 阶段每张家族子卡最后记录的 `COMMIT_SHA` 都在该区间内（无遗漏的子卡提交）。
- [ ] AC3：A/B 阶段 `FAMILY_BASE_SHA` 到 `HEAD` 为单父提交组成的线性链（无合并提交、无旁支）。
- [ ] AC4：A/B 阶段家族子卡提交在该链上的顺序与登记的子卡顺序相同。
- [ ] AC5：A/B 阶段每个家族子卡提交只改动该子卡写集内的路径与计划记账文件。
- [ ] AC6：A/B 阶段 `FAMILY_BASE_SHA` 到 `HEAD` 的最终改动集只含家族子卡写集与计划记账文件中的路径。
- [ ] AC7：A/B 阶段 `libra status --short --untracked-files=all` 中的未提交改动只属于家族子卡写集与计划记账文件。
- [ ] AC8：A/B 阶段家族聚合守卫中的集成用例（`SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 在 `command_test` 中的全部卡特有用例）全部通过。
- [ ] AC9：A/B 阶段家族聚合守卫中的库内单元用例全部通过。
- [ ] 文档门（ER-06/ER-06a，不计入 AC 分子）：仓库文档已由 `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 交付；本卡按「文档与网站验收命令」第 5 步交付网站页面。

**错误类清单（GC-04，「验证执行协议」第 11 条；非计数正文，判据即所列 AC）：** 不适用（发布点不新增行为）。

**Verification:**（按「验证执行协议」执行：nextest 精确过滤按第 1 条、发布点聚合守卫按第 1b 条；每项为 1 个独立门）

- [ ] VER1 家族提交身份守卫（「验证执行协议」第 9a 条）：`libra rev-list "$FAMILY_BASE_SHA..HEAD"`（非 0 退出即 STOP）列出的每个提交，或等于 `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 中某张子卡卡内最后记录的 `COMMIT_SHA`，或为第 9a 条定义的独立记账提交（A/B 阶段、bump 前执行；AC1）
- [ ] VER2 家族提交完整性守卫（第 9a 条）：`SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 每张子卡卡内最后记录的 `COMMIT_SHA` 都出现在上述 `libra rev-list` 输出中（A/B 阶段、bump 前执行；AC2）
- [ ] VER3 线性链守卫（第 9e 条）：自 `HEAD` 按第 9e 条逐个回溯，每个提交恰有 1 个 `parent` 行，且在恰好走完上述 `libra rev-list` 输出的全部提交后到达 `FAMILY_BASE_SHA`（A/B 阶段、bump 前执行；AC3）
- [ ] VER4 子卡顺序守卫（第 9e 条）：上述回溯链（自 `FAMILY_BASE_SHA` 向 `HEAD`）上 `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 的 `COMMIT_SHA` 出现顺序与「发布分组与并发窗口」登记的子卡顺序相同（A/B 阶段、bump 前执行；AC4）
- [ ] VER5 逐提交改动集守卫（第 9b 条）：每个家族子卡提交 `<c>` 的 `libra diff --name-only "<c>^" "<c>"`（非 0 退出即 STOP）只含该子卡 `Implementation write set` 中的本仓库路径或 `docs/development/plan/` 下的记账文件（A/B 阶段、bump 前执行；AC5）
- [ ] VER6 最终改动集守卫（第 9c 条）：`libra diff --name-only "$FAMILY_BASE_SHA" HEAD`（非 0 退出即 STOP）输出的每个路径都属于 `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 的 `Implementation write set` 或 `docs/development/plan/` 下的记账文件（逐行比对，差集为空；A/B 阶段、bump 前执行；AC6）
- [ ] VER7 未提交改动集守卫：`libra status --short --untracked-files=all` 输出的每个路径都属于 `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 的 `Implementation write set` 或 `docs/development/plan/` 下的记账文件（逐行比对，差集为空；A/B 阶段、bump 前执行；AC7）
- [ ] VER8 聚合守卫（「验证执行协议」第 1b 条）：`source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::am_test::am_unborn_single_patch_creates_root_commit) | test(=command::am_test::am_unborn_series_matches_git_history) | test(=command::am_test::am_unborn_second_patch_abort_restores_unborn) | test(=command::am_test::am_unborn_first_patch_abort_restores_unborn) | test(=command::am_test::am_unborn_first_pause_user_root_blocks_abort) | test(=command::am_test::am_unborn_abort_txn_rollback_then_retry) | test(=command::am_test::am_unborn_abort_refuses_when_branch_moved_before_delete) | test(=command::am_test::am_unborn_first_pause_abort_refuses_when_branch_created_before_txn)'`（A/B 阶段、bump 前；exit 0，8 run / 8 passed / 0 failed；AC8）
- [ ] VER9 聚合守卫（「验证执行协议」第 1b 条）：`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=internal::sequencer::tests::optional_oid_column_zero_encoding_sha1_sha256) | test(=command::am::tests::am_state_unborn_encoding_wiring)'`（A/B 阶段、bump 前；exit 0，2 run / 2 passed / 0 failed；AC9）

**共享回归组（不计入 VER）:** 不适用（发布点只执行聚合守卫与 `T-2` 全量）。
**D 组:** 「发布分组与并发窗口」的 D 组登记表 D1–D6。

**Full-suite trigger:** `T-2: family release point`
**Dependencies:** `SA-09f`, `SA-09b`, `SA-09`, `SA-09a`, `SA-09e`, `SA-09g`, `SA-09h`, `SA-09i`, `DEP-SA-03`, `DEP-SA-10`
**Deliverables:** `N/A`
**Implementation write set:** `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-manifest.ts`, `$LIBRA_BACKEND/apps/tanstack-app/src/generated/docs-search.ts`, `$LIBRA_BACKEND/apps/tanstack-app/dist/`（只提交子卡草稿的最终同步结果，不新增行为）（网站文件：`*.en.md` 为交付文件；`docs-manifest.ts`、`docs-search.ts` 与 `dist/` 是第 3d 步 `pnpm typecheck`/`pnpm build` 改写的被忽略生成产物，不暂存、不提交，其与同批计划的相交由 `DEP-SA-03` 网站窗口串行）
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`，另含版本面（以 `compat_version_surface_sync` 为准：`Cargo.toml`、`install.sh`、`install.ps1`）与工具链刷新的 `Cargo.lock`
**Docs and compatibility impact:** 仓库文档已由 `SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 交付；本卡按「文档与网站验收命令」第 5 步交付 `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/am.en.md`。
**Rollback mode:** `immutable-release`
**Migration and rollback:** 无 schema 迁移。存在暂停的 unborn 起点 `am`（`head_orig` 为全零 OID）时禁止直接降级，也禁止手工删除状态行；先 `libra am --abort` 回到 unborn（unborn 起点暂停后的 `--continue`/`--skip` 不在本计划验收范围，`DEFER-SA-20`），确认 `sequence_state` 无 `kind='am'` 行后再通过上一已知良好 tag 重新安装。
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-SA-02`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=发布unborn-am家族版本; recovery=前滚新版本，unborn起点am暂停态先abort再降级; complete=yes; self-contained=yes; AC=9/12; VER=9/12; landing=0; prod-files=0; scope=S; deps=SA-09f,SA-09b,SA-09,SA-09a,SA-09e,SA-09g,SA-09h,SA-09i,DEP-SA-03,DEP-SA-10; writeset=序列化于 SA-09i; release=family release point; split-from=N/A; exception=N/A`

---

## 测试矩阵

执行阶段按 ER-13 只跑与本卡相关的行；整张表由收口阶段的全量门统一覆盖。

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | 全零编解码与 `am` 字段接线、范围展开加载计数 | `SA-02` VER3、`SA-09f` VER1、`SA-09b` VER1（均为 `--lib` 精确过滤，各恰好 1 run / 1 passed） |
| 集成 | 各卡 `Verification` 的具名用例；各卡登记的模块共享回归组 | 各卡 VER 与「共享回归组」行（`command_test`，过滤前缀 `command::<模块>::`） |
| 兼容 | `am`/sequencer/history 既有守卫与文档、矩阵、错误码守卫 | `source .env.test && cargo nextest run --no-tests=fail --test compat_mail_am_basic --test compat_sequencer_message_author --test compat_noninteractive_history_controls --test compat_matrix_alignment --test compat_ledger_schema --test compat_error_codes_doc_sync --test compat_command_docs_examples_section --test compat_help_examples_banner` |
| 迁移 | `N/A`（无 schema 变更；全零 OID 旧行兼容由单元行覆盖） | `N/A` |
| 安全 | 签名单一出口（含重放后改写路径）、8 个 action 签名开启/关闭两种结果、出口级签名失败不写对象、签名失败的冲突同构停止与恢复；unborn 上 `--ff` 的已暂存条目保护；unborn 起点 `am` 暂停后的 `--abort` 复原、中止保护、中断后重试与并发保护 | `SA-05c` VER1；`SA-08a` VER1–VER3；`SA-08b` VER1–VER4；`SA-08m` VER1–VER2；`SA-08g` VER1–VER2；`SA-08h` VER1–VER4；`SA-08j` VER1–VER6；`SA-09a` VER1；`SA-09e` VER1；`SA-09g` VER1；`SA-09h` VER1；`SA-09i` VER1–VER2 |
| 性能 | 范围展开每个提交至多加载一次 | `SA-02` VER3 |
| live/gated | 无 env 门控；SSH 签名用例需要本机 `ssh-keygen`、Git 对照用例需要 `git`，不可用即失败（不 skip） | `SA-08g` VER1–VER2；各 Git 对照用例 |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| SA-01 | #725 验收项 1、2、4；`t3505#13/#14` 首尾步骤；`cherry_pick.rs:217-218,2379-2384` | `cherry_pick.rs` | cherry-pick 三份文档、`docs/error-codes.md`、`COMPATIBILITY.md`、网站 `cherry-pick.en.md` | `command::cherry_pick_test::cherry_pick_empty_stop_message_exit_and_state` |
| SA-01b | #725；`t3505#13`（`TC-0426`）；`cherry_pick.rs:689-716,750-753` | `cherry_pick.rs` | cherry-pick 三份文档、`COMPATIBILITY.md`、网站 | `command::cherry_pick_test::cherry_pick_keep_redundant_rejected_with_controls_tc0426` |
| SA-02 | #726 验收项 1；`t3504#7`（`TC-0429`）；`cherry_pick.rs:1077-1083,1147,1179,1228,1699-1722` | `cherry_pick.rs` | cherry-pick 三份文档、`COMPATIBILITY.md`、网站 | `command::cherry_pick_test::cherry_pick_range_conflict_stop_then_continue`、`command::cherry_pick_test::cherry_pick_range_later_untracked_overwrite_stops_cleanly`、`--lib` `command::cherry_pick::tests::range_expansion_loads_each_commit_once` |
| SA-03 | #726 验收项 2–5；`t3504#7`（`TC-0429`）；`cherry_pick.rs:482-485,574-582,689-695` | `cherry_pick.rs` | cherry-pick 三份文档、`COMPATIBILITY.md`、网站 | `command::cherry_pick_test::cherry_pick_range_rerere_autoupdate_stop_tc0429`、`command::cherry_pick_test::cherry_pick_continue_rejects_rerere_flags_tc0429` |
| SA-03b | #727 验收项 1–4；`t3504#8`（`TC-0468`）；`cherry_pick.rs:820-825` | `cherry_pick.rs` | cherry-pick 三份文档、`COMPATIBILITY.md`、网站 | `command::cherry_pick_test::cherry_pick_rerere_autoupdate_repeated_last_wins` |
| SA-04 | #729；`t3506#10`；`cherry_pick.rs:1954-1962` | `cherry_pick.rs` | cherry-pick 三份文档、`COMPATIBILITY.md`、网站 | `command::cherry_pick_test::cherry_pick_ff_root_commit_matches_git_oid_tc0512`、`command::cherry_pick_test::cherry_pick_ff_direct_child_fast_forwards` |
| SA-05c | #730 前置安全（`SA-05` 使快进在 unborn 上可达）；`cherry_pick.rs:167-168,224,321,1956-2002,2367-2369`；Git `sequencer.c` `fast_forward_to` | `cherry_pick.rs` | cherry-pick 三份文档、`COMPATIBILITY.md`、网站 | `command::cherry_pick_test::cherry_pick_unborn_ff_refuses_staged_entries` |
| SA-05 | #730 验收项 1–3；`t3506#11`（`TC-0513`）；`cherry_pick.rs:1954-1966,2367-2369`；Git `sequencer.c:2402-2407` | `cherry_pick.rs` | cherry-pick 三份文档、`COMPATIBILITY.md`、网站 | `command::cherry_pick_test::cherry_pick_unborn_ff_root_commit_tc0513`、`command::cherry_pick_test::cherry_pick_unborn_ff_refuses_untracked_overwrite` |
| SA-06 | #731 验收项 1–3；`t3505#16`（`TC-0523`）；`show.rs:1164-1165`；`cherry_pick.rs:2386-2393`（其它格式变体 → `DEFER-SA-15`） | `show.rs` | show 三份文档、`COMPATIBILITY.md`、网站 `show.en.md` | `command::cherry_pick_test::cherry_pick_empty_drop_show_message_matches_git` |
| SA-07a | #728 验收项 2（`t3501#7`、`TC-0484` 前置）；`revert.rs:450-464` | `revert.rs` | revert 三份文档、`docs/error-codes.md`、`COMPATIBILITY.md`、网站 `revert.en.md` | `command::revert_test::revert_staged_changes_refused_without_state` |
| SA-07 | #728 AC1–AC3；`t3501#15–#17`；`revert.rs:454-456` | `revert.rs` | revert 三份文档、`COMPATIBILITY.md`、网站 | `command::revert_test::revert_detached_head_no_edit_tc2634`、`command::revert_test::revert_detached_staged_changes_refused_for_local_changes`、`command::revert_test::revert_t3501_orphan_detach_leaves_no_state` |
| SA-07c | #728 验收项 4、5 的核心断言（`t3501#12` 形态；验收项 6 的全量与 `TC-0484` → `DEFER-SA-13`）；`switch.rs:470-476` | `cherry_pick.rs` | cherry-pick 三份文档、`COMPATIBILITY.md`、网站 | `command::cherry_pick_test::cherry_pick_previous_branch_shorthand`、`command::cherry_pick_test::cherry_pick_t3501_12_signoff_dash_tc0484` |
| SA-08a | #780 前置；`rebase.rs:3690-3698,5746-5762,5861-5867`；`commit.rs:3088-3150`；`merge.rs:1630-1647` | `rebase.rs` | rebase 三份文档、`COMPATIBILITY.md`、网站 `rebase.en.md` | `command::rebase_test::rebase_replay_pick_unsigned_strips_stale_gpgsig`、`command::rebase_test::rebase_replay_pick_signs_openpgp`、`--lib` `command::rebase::tests::finalize_replayed_commit_sign_failure_writes_nothing` |
| SA-08b | #780 前置；`rebase.rs:2526-2535,5868-5892,5905-5911` | `rebase.rs` | rebase 三份文档、`COMPATIBILITY.md`、网站 | `command::rebase_test::rebase_replay_reword_edit_unsigned_strip_stale_gpgsig`、`command::rebase_test::rebase_replay_fixup_squash_unsigned_strip_stale_gpgsig`、`command::rebase_test::rebase_replay_reword_edit_signs_openpgp`、`command::rebase_test::rebase_replay_fixup_squash_signs_openpgp` |
| SA-08m | #780 前置；`rebase.rs:2526-2535,5893-5904,5912-5923,5950-5964` | `rebase.rs` | rebase 三份文档、`COMPATIBILITY.md`、网站 | `command::rebase_test::rebase_replay_amend_fixup_keep_unsigned_strip_stale_gpgsig`、`command::rebase_test::rebase_replay_amend_fixup_keep_signs_openpgp` |
| SA-08g | #780 前置、#787；`rebase.rs:5861-5867,5879-5892`；`commit.rs:3088-3150,3173-3200` | `rebase.rs` | rebase 三份文档、`COMPATIBILITY.md`、网站 | `command::rebase_test::rebase_replay_pick_signs_ssh`、`command::rebase_test::rebase_replay_squash_signs_ssh` |
| SA-08h | #780 前置；`rebase.rs:371-373,1402-1443,2024-2075,2871-2872,4027-4046,4407-4413,5925-5948` | `rebase.rs` | rebase 三份文档、`COMPATIBILITY.md`、网站 | `command::rebase_test::rebase_replay_signoff_unsigned_strips_stale_gpgsig`、`command::rebase_test::rebase_replay_signoff_then_sign_pick`、`command::rebase_test::rebase_replay_reword_editor_rewrite_signs`、`command::rebase_test::rebase_replay_edit_amend_continue_signs` |
| SA-08j | #780 前置；`rebase.rs:2526-2535,4121-4127,4421-4488,4533-4665,5746-5777,5868-5892`；`rebase/state.rs:484-495,512-524,640-652`；`am.rs:305,756-758`；`status/scan.rs:29`；`commit.rs:566-568`；Git `sequencer.c` `pick_one_commit` | `rebase.rs` | rebase 三份文档、`docs/error-codes.md`、`COMPATIBILITY.md`、网站 | `command::rebase_test::rebase_sign_failure_mid_replay_multi_pick_continue`、`command::rebase_test::rebase_sign_failure_mid_replay_multi_pick_abort`、`command::rebase_test::rebase_sign_failure_mid_replay_fold_continue`、`command::rebase_test::rebase_sign_failure_mid_replay_fold_abort`、`command::rebase_test::rebase_sign_unavailable_stops_then_continue_after_config_fix`、`command::rebase_test::rebase_sign_failpoint_inert_without_libra_test` |
| REL-SA-01 | #780 前置；`SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j` | 版本面、`Cargo.lock` | 网站 `rebase.en.md` 交付（第 5 步） | `compat_version_surface_sync`、家族提交序列守卫（提交身份与完整性、线性链与顺序、逐提交改动集、最终改动集）、未提交改动集守卫、家族聚合守卫（20 个集成用例与 1 个库内用例）、`T-2` 全量、D 组登记表 D1–D6 |
| SA-08 | #780 验收项 1、2（`t7510#2`、`TC-0911`）；`rebase.rs:258-264,842-848,3690-3698` | `rebase.rs` | rebase 三份文档、`COMPATIBILITY.md`、网站 | `command::rebase_test::rebase_force_rebase_signed_openpgp_tc0911`、`command::rebase_test::rebase_force_rebase_long_form_replays_up_to_date_branch` |
| SA-09f | #907 前置；`src/internal/sequencer/mod.rs`（新增共享编解码）；`object_format.rs:47-57` | `sequencer/mod.rs` | 开发文档 `docs/development/commands/am.md` | `--lib` `internal::sequencer::tests::optional_oid_column_zero_encoding_sha1_sha256` |
| SA-09b | #907 前置；`am.rs:139-175,292-304` | `am.rs` | 开发文档 `docs/development/commands/am.md` | `--lib` `command::am::tests::am_state_unborn_encoding_wiring` |
| SA-09 | #907 验收项 1–3；`am.rs:284-291` | `am.rs` | am 三份文档、`COMPATIBILITY.md`、`tests/INDEX.md`、网站草稿 | `command::am_test::am_unborn_single_patch_creates_root_commit`、`command::am_test::am_unborn_series_matches_git_history`；注册检查见本卡文档门（ER-06/GC-06，`cargo nextest list` 两个具名用例各 1 行） |
| SA-09a | #907（`SA-09` 移除拒绝后的恢复出口：第二封暂停）；`am.rs:381-414,1339-1343`；`branch.rs:829`；`sequencer/mod.rs:885`；Git `builtin/am.c` `am_abort` | `am.rs` | am 三份文档、`COMPATIBILITY.md`、网站草稿 | `command::am_test::am_unborn_second_patch_abort_restores_unborn` |
| SA-09e | #907（首封暂停，当前 HEAD 缺失）；`am.rs:381-414,389-392,1339-1343`；Git `builtin/am.c` `am_abort` | `am.rs` | am 三份文档、`COMPATIBILITY.md`、网站草稿 | `command::am_test::am_unborn_first_patch_abort_restores_unborn` |
| SA-09g | #907 安全前置（`head_orig=None` 会话的统一中止保护）；`am.rs:386-407,1339-1343`；Git `builtin/am.c` `safe_to_abort` | `am.rs` | am 三份文档、`COMPATIBILITY.md`、网站草稿 | `command::am_test::am_unborn_first_pause_user_root_blocks_abort` |
| SA-09h | #907 GC-08（`--abort` 中断后重试）；`am.rs:305,756-758`；`branch.rs:829`；`sequencer/mod.rs:885` | `am.rs` | am 三份文档、`COMPATIBILITY.md`、网站草稿 | `command::am_test::am_unborn_abort_txn_rollback_then_retry` |
| SA-09i | #907 并发保护（判定与删除之间的分支移动）；`am.rs:386-414,756-758`；`branch.rs:829-872` | `am.rs` | am 三份文档、`COMPATIBILITY.md`、网站草稿 | `command::am_test::am_unborn_abort_refuses_when_branch_moved_before_delete`、`command::am_test::am_unborn_first_pause_abort_refuses_when_branch_created_before_txn` |
| REL-SA-02 | #907；`SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` | 版本面、`Cargo.lock` | 网站 `am.en.md` 交付（第 5 步） | `compat_version_surface_sync`、家族提交序列守卫（提交身份与完整性、线性链与顺序、逐提交改动集、最终改动集）、未提交改动集守卫、家族聚合守卫（8 个集成用例与 2 个库内用例）、`T-2` 全量、D 组登记表 D1–D6 |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 本计划经 Claude（Subagent）与 Codex 双审，对同一受审 SHA-256 均 `PASS` | 评审记录 `PASS` | `N/A` |
| M1 | Phase 0（`SA-01`、`SA-01b`、`SA-02`、`SA-03`、`SA-03b`、`SA-04`、`SA-05c`、`SA-05`、`SA-06`）全部 `done`/`complete`，各自独立发布 | 各卡 patch 版本、D 组登记表适用项全绿 | 前滚新版本 |
| M2 | Phase 1（`SA-07a`、`SA-07`、`SA-07c`、`SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j`、`REL-SA-01`、`SA-08`、`SA-09f`、`SA-09b`、`SA-09`、`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i`、`REL-SA-02`）全部 `done`/`complete`，全量收口门通过 | 各卡版本与 `REL-SA-01`、`REL-SA-02` 家族版本、全量 nextest + fmt + clippy | 前滚新版本；unborn 起点的暂停 `am` 先 `libra am --abort` 回到 unborn 再降级 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 空 cherry-pick 停止或冲突停止 | `sequence_state` 行与冲突 sidecar | `--continue`/`--skip`/`--abort`/`--quit` | 状态静默丢失；以 usage 129 结束 |
| 前置拒绝（revert 已暂存改动）或 `--abort` | 无 | 原子清理 sequencer 与 sidecar | 残留状态阻塞 `checkout` |
| unborn 起点 `am` 冲突暂停 | `head_orig` 为全零 OID；首封暂停时 `current_oid` 亦为全零，后续暂停时 `current_oid` 为最近已应用提交 | `libra am --abort` 回到 unborn（`SA-09a`、`SA-09e` 验收：分支引用删除、符号 HEAD 保留、索引与工作区复原，分支引用删除与状态清除在同一 SQLite 写事务中，事务内失败即整体回滚、中断后再次 `--abort` 完成复原（`SA-09h`），事务内比较到分支已被并发移动或创建时回滚并拒绝（`SA-09i`；并发进程的未提交工作区写入保护登记 `DEFER-SA-24`）；会话中 HEAD 已不同于暂停点时按 `SA-09g` 拒绝中止并保留状态、提交、索引与工作区（第二封暂停后用户另行提交情形的专项验收 `DEFER-SA-20`））；`--continue`/`--skip` 不在本计划验收范围（`DEFER-SA-20`），文档要求以 `--abort` 退出；带活动态时禁止直接降级；`am` 暂停且仍有待处理邮件时先 `--abort` 再运行 GC（`DEFER-SA-09`：此时 GC 把 `todo` 邮件标签当 OID 而以 `RepoCorrupt` 失败，属已知缺陷） | 丢失恢复路径；`--abort` 后分支仍指向首封生成的提交或符号 HEAD 被删除/改指；`--abort` 丢弃用户在暂停后的提交；未跟踪用户文件被删除 |
| `rebase` 重放签名失败（任一经 `create_replayed_commit` 构造提交的重放步，含第一步与折叠步；含签名不可用；重放后改写路径的二次签名失败不在此列，见 `DEFER-SA-08`） | 冲突同构停止态（与 Git 一致，无预检）：该步合并树已在索引与工作区，`stopped_sha` 指向该步，分支与标签引用未动，HEAD 停在失败步之前的重放结果（第一步失败时为 `<upstream>`） | 修正后 `libra rebase --continue`（补丁保留，签名再失败则停止态不变、可再试）或 `libra rebase --abort`（引用、索引与工作区回到 rebase 前） | 以旧索引生成提交而丢失补丁；折叠步取错父提交、作者或消息；写入未签名提交；引用前移；`--abort` 后残留上游文件 |
| 家族窗口内（`REL-SA-01` 或 `REL-SA-02` 家族子卡已本地提交、对应 `REL-SA-01`/`REL-SA-02` 未完成） | 未推送的本地提交与网站本地草稿 | 修复后重跑对应发布点，或按子卡逆序本地 `libra revert` 并丢弃网站草稿 | 推送任一子卡提交或网站草稿 |
| 网站已推送 `cf` 但部署 D5 失败 | 已推送的网站提交 | 卡停在 `remote-pending`；按 ER-10 重试，仍失败则前滚修复网站提交 | 宣称完成；强推或改写 `cf` |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| 空 cherry-pick 退出码由 129 改为 1，脚本或既有测试依赖旧码 | 中 | 同卡更新 `cherry_pick_redundant_blocked_then_kept`、`docs/error-codes.md` 与 `COMPATIBILITY.md` | `SA-01` |
| `DEP-SA-01` 交付后 `libra commit` 默认不签名，提交可能缺签名 | 中 | 「验证执行协议」第 7 条：所有提交 `-S` 强制签名并做同一提交校验 | 全部 |
| AC 写成多性质长句而少计 G-03 条目 | 中 | 「验证执行协议」第 4 条：每条 AC 只陈述一个性质；评审按卡逐条核对 | 全部 |
| 范围内合并提交沿用逐提交拒绝，与 Git 的诊断文本和停止状态不同 | 低 | 文档写明差异；`DEFER-SA-04` 承接完整对齐 | `SA-02` |
| 共享 `sequencer/mod.rs` 新增编解码函数影响其它命令 | 中 | 家族由 `REL-SA-02` 的 `T-2` 全量覆盖 | `SA-09f`, `REL-SA-02` |
| 重放后改写路径（交互式消息编辑、`edit` 停止后的修订）的二次签名失败无专项恢复合同 | 中 | 出口不写对象、命令以非 0 退出；恢复合同与专项门登记 `DEFER-SA-08`，文档提示改写前确认签名可用 | `SA-08h`, `REL-SA-01` |
| 收窄后各 issue 只部分闭环，上游脚本或 TC 仍可能失败 | 中 | 「Issue 闭环登记」与 `DEFER-SA-13`..`DEFER-SA-20` 写明延后范围；9 个 issue 保持 open | 全部 |
| `revert --no-commit` 从非 HEAD 索引开始时丢弃已暂存条目（既有缺陷，无承接计划） | 中 | 本计划不改变该路径；登记在「与其它计划的关系」 | `SA-07a` |
| 重放签名行为变化：启用签名的用户 rebase 后提交被重签（含 `reword` 与 `edit` 后的修订），签名不可用时 rebase 在第一步停止（与 Git 一致） | 中 | 单一出口、出口级 fail-closed，文档说明；签名失败按冲突停止同构处理并以门族卡 `SA-08j` 验收 | `SA-08a`, `SA-08b`, `SA-08m`, `SA-08g`, `SA-08h`, `SA-08j`, `REL-SA-01`, `SA-08` |
| 跨计划同文件并发（`cherry_pick.rs`、`am.rs`、`sequencer/mod.rs`、`tests/command/mod.rs`、`COMPATIBILITY.md`） | 高 | `DEP-SA-01`..`DEP-SA-09` 与开工前 Coordinator 写集核对（「与其它计划的关系」） | 全部 |
| 携带全零 OID 活动态降级到旧二进制 | 中 | 各卡 `Migration and rollback`；文档写明降级前置（先 `libra am --abort` 回到 unborn） | `SA-09`, `SA-09a`, `SA-09e`, `SA-09g`, `SA-09h`, `SA-09i`, `REL-SA-02` |
| unborn 起点 `am` 暂停后的 `--continue`/`--skip` 未在本计划验收（首封暂停时 `--skip` 的既有 `reset --hard HEAD` 在 unborn 上不可用） | 中 | `--abort` 两种形态与暂停后用户提交的保护由 `SA-09a`、`SA-09e`、`SA-09g` 验收；am 三份文档写明 unborn 起点暂停后只支持已验收的 `--abort`（`--continue`/`--skip` 尚未验收）；`DEFER-SA-20` 承接 | `SA-09`, `SA-09e`, `REL-SA-02` |
| unborn 分支上 `cherry-pick --ff` 覆盖或删除已暂存条目 | 中 | `SA-05c` 先于 `SA-05` 在任何写入前拒绝并验收索引与工作区不变；born 分支上的同类既有缺陷登记 `DEFER-SA-22` | `SA-05c`, `SA-05` |
| `am --abort` 的判定与复原之间，另一进程向同一工作区或索引写入未提交改动 | 低 | 本计划不新增跨进程的工作区/索引写入互斥（与既有 `--abort` 相同）；引用级的并发移动由 `SA-09i` 在事务内比较后拒绝，提交与引用不丢失；`DEFER-SA-24` 承接 | `SA-09a`, `SA-09i` |
| CI 或本机缺少 `ssh-keygen`/`git` | 低 | 用例失败而非 skip，记录工具版本 | `SA-08g`，各 Git 对照用例 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `cherry-pick A..B` 范围展开 | O(reach(B) 与 reach(A) 的提交数之和) 次对象加载，每个提交至多一次 | 随范围线性 | 加载次数 = 去重提交数（不做墙钟断言） | `SA-02` VER3 |
| unborn 状态全零 OID 转换 | O(1) | 无 | 不增加 DB 读写次数 | `SA-09f` VER1；`SA-09b` VER1 |
| `rebase` 重放签名 | 每个重放提交一次签名 | 随重放提交数线性 | 仅在签名启用时发生，不新增对象读取 | `SA-08a` VER2 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已在 `cherry-pick`、`show`、`revert`、`rebase`、`am` 既有条目行内同步（行数不变）。
- [ ] `docs/commands/{cherry-pick,show,revert,rebase,am}.md` 已同步。
- [ ] `docs/commands/zh-CN/{cherry-pick,show,revert,rebase,am}.md` 已同步。
- [ ] `docs/development/commands/{cherry-pick,show,revert,rebase,am}.md` 已同步。
- [ ] `docs/error-codes.md` 已同步（`SA-01`、`SA-07a`、`SA-08j`）。
- [ ] `tests/INDEX.md` 已同步（`SA-09` 的 `am_test` 模块）。
- [ ] `Cargo.toml` `[[test]]`：`N/A`（`am_test` 是 `command_test` 的模块，不新增 target）。
- [ ] `plan-long.md`：收口时登记 `DEFER-SA-01`..`DEFER-SA-06`、`DEFER-SA-08`..`DEFER-SA-24`（模板「完成判据」规定的执行期义务，由收口执行者完成；不属于本计划文本提交的写集）。
- [ ] 网站 `$LIBRA_BACKEND/apps/tanstack-app/content/docs/commands/{cherry-pick,show,revert,rebase,am}.en.md` 已按「文档与网站验收命令」交付：有改动时经第 3e–3h 步提交、推送并通过 D5；无漂移时按第 3c 步完成前端验证、独立 Claude Code 评审与线上内容核对。

## Codex review log

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 (Claude) | `plan-20261117-sequencer-am.md` | `FAIL` | 1 项 P0（`SA-04` `t3506#10`）+ 7 项 P1（`#728`、`am_test`、unborn 哨兵、`T-1`/`T-4`、锚点、`COMPATIBILITY.md`、`SA-08`）；R2 已关闭 | P2-1 审计表 `writeset`、P2-2 `SA-02` 描述、P2-3 `COMPATIBILITY.md` 行号保护、P2-4 测试退出码与性能预算、P2-5 `RevertError::DetachedHead` 同步已全部在 R2 关闭 | `claude -p --model claude-opus-5-5` R1 |
| R1 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 6 项 P1（`SA-04`、`SA-07` 冲突/清理与 `#728`、`T-1`、`SA-06`、`SA-09`）；R2 已关闭 | P2 独立谓词计数已在 R2 按 G-03 重数并对齐审计表 | `codex exec -m gpt-6.1-terra` R1 |
| R2 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 3 项 P1（`SA-01` `TC-0427`/`TC-0522`、`SA-08` `#780` 评论 TC 与 `DEP-SA-02`/`DEFER-SA-02`、R1 P2 逐项闭环记录）；R3 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R2 |
| R2/R3 (Claude) | `plan-20261117-sequencer-am.md` | `FAIL` | 7 项 P1（`#726` 评论 TC、`#731` `--empty=keep`、`#728` `TC-2637`/`2807`、`SA-04` OID/`DEP-SA-01`、`ADR-SA-02` 决断、`SA-07`/`08` 拆分、`修订历史`）；R4 已关闭 | 6 项 P2（`DEFER-SA-03`、`am_test::`、`docs/error-codes.md`、错误锚点、`plan-status.md` 批处理例外、outgoing `commit-signing`）已在 R4 关闭 | `claude -p --model claude-opus-5-5` R3 |
| R3 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 2 项 P1（`SA-07` AC 超 8、测试矩阵多位置过滤参数）；R4 已关闭 | 1 项 P2（R3 修订历史）已在 R4 关闭 | `codex exec -m gpt-6.1-terra` R3 |
| R4 (Claude) | `plan-20261117-sequencer-am.md` | `FAIL` | 3 项 P1（`SA-01`/`09` 跨计划 G-10 串行、`ADR-SA-02` `sequence_state` 与 `expected_head`/`current_oid`、`SA-07a` `commit.rs` 写集）；R5 已关闭 | 7 项 P2（G-09 保留 `SA-07`/`08`、`SA-08a` 单轴重签、`SA-07` `cherry-pick -`、`RevertArgs` 行号、`SA-01` 锚点、`SA-04` 备注、`#728` AC3）已在 R5 关闭 | `claude -p --model claude-opus-5-5` R4 |
| R4 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 2 项 P1（`ADR-SA-02` 活动期降级安全限制、`SA-08`/`08a` `REL-SA-01` 家族发布）；R5 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R4 |
| R5 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 1 项 P1（`SA-09` 依赖 `SA-08a, DEP-SA-04`）；R6 已关闭 | 2 项 P2（`SA-04` AC4 与测试矩阵拆分）已在 R6 关闭 | `codex exec -m gpt-6.1-terra` R5 |
| R6 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 2 项 P1（`REL-SA-01` 按 G-08 独立 `release` 卡与禁止推送窗口、网站写集改为 `/Volumes/Data/gitmono/libra-backend`）；R7 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R6 |
| R7 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 3 项 P1（`REL-SA-01` 循环完成门、`SA-08`/`08a` `C/D coverage from: REL-SA-01`、`T-2` 补 `source .env.live-test`）；R8 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R7 |
| R8 (Claude) | `plan-20261117-sequencer-am.md` | `FAIL` | 5 项 P1（`SA-06` 重锚 `show.rs:1164-1166` + `DEP-SA-06`、`DEP-SA-04` 对齐 `CSR-25/26/31/32`、`SA-07c` 拆出 `cherry-pick -`、`SA-08/08a` 回滚与依赖语义、`REL-SA-01`+独立卡远端 CI 门）；R9 已关闭 | 4 项 P2（`SA-02` 分叉历史 `A..B`、`--lib command::am::`、`ADR-SA-02` 禁止破坏性清理、`Codex review log` 仅保留 `PASS`/`FAIL`）已在 R9 关闭 | `claude -p --model claude-opus-5-5` R8 |
| R8 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 1 项 P1（`SA-06` 行号 `584-656` 与 `show.rs` 根因重锚）；R9 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R8 |
| R9 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 1 项 P1（`release.yml` 应在合入后打 tag/`gh release create` 触发后再核验全绿，而非前置阻塞）；R10 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R9 |
| R10 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 3 项 P1（`SA-08` 内容寻址 OID 语义、`SA-08a` 前置独立发布替代 `REL-SA-01`、`SA-09` 多补丁与 Git 历史比对）；R11 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R10 |
| R11 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 1 项 P1（`SA-09` 补 `DEP-SA-07` 串行协调 `issues/672.md` 与 `issues/547.md`）；R12 已关闭 | 2 项 P2（移除 `Phase 1`/`M2` 残留 `REL-SA-01`、头部更新为 `R12` 与 `13` 卡）已在 R12 关闭 | `codex exec -m gpt-6.1-terra` R11 |
| R12 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 无 | 2 项 P2（`SA-08` `AC=6/8` 与审计表对齐、`SA-06` 追溯表补齐 `show.md` + `COMPATIBILITY.md`）已在 R13 关闭 | `codex exec -m gpt-6.1-terra` R12 |
| R12 (Claude) | `plan-20261117-sequencer-am.md` | `FAIL` | 5 项 P1（`ADR-SA-02` null-OID 适配 `maintenance.rs`/`pseudo_ref.rs` + `DEP-SA-08`、`DEP-SA-04` 删 `CSR-25/26` 改 `PM-10:618`/`CSR-31:1244`/`CSR-32:1279`、`DEP-SA-01` 补 `CS-02a`、`SA-08a` 改 `Pick` 分叉重放并将 `TC-0911`/`0931` 移至 `SA-08`、`SA-07c` 归 `#728` AC5/6 + `SA-07` 补 detached `cherry-pick`）；R14 已关闭 | 7 项 P2（`switch.rs:473`、`DEP-SA-06` 收窄 `LS-23..27`、`SA-06` `tformat:`/`--format` 保护、`SA-01` `LBR-CONFLICT-001`+`T-1`、`SA-07d` `commit_test`、`SA-01` `writeset`）已在 R14 关闭 | `claude -p --model claude-opus-5-5` R12 |
| R13 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 2 项 P1（`SA-07a` 清理前置 `SA-07` 并拆出 `SA-07d` `commit -a` 消息保留）；R14 已关闭 | 1 项 P2（`SA-09` 显式覆盖 `Migration and rollback` 降级限制）已在 R14 关闭 | `codex exec -m gpt-6.1-terra` R13 |
| R14 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 1 项 P1（`SA-06` 对齐 `show.rs:91` `--format` 别名与 `format:`/`tformat:`/隐式 `%B` 语义）；R15 已关闭 | 2 项 P2（`DEP-SA-05` 与 Phase 1 改指 `SA-07d`、发布分组改 `14` 张卡并移除 `SA-06` 追溯表 `cherry_pick.rs`）已在 R15 关闭 | `codex exec -m gpt-6.1-terra` R14 |
| R15 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 无 | 1 项 P2（审计表 `SA-07b` 与 `SA-08a` 的 `AC`/`VER` 列与各自 `Granularity` 互换）已在 R16 关闭 | `codex exec -m gpt-6.1-terra` R15 |
| R16 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 2 项 P1（`SA-09` 谓词拆分为 `SA-09`+`SA-09a`，并补充暂停态 unborn `am` 的 `maintenance` GC 根与 `ORIG_HEAD` 跨进程回归门）；R17 已关闭 | 1 项 P2（头部与日志版本号对齐为 `R17`）已在 R17 关闭 | `codex exec -m gpt-6.1-terra` R16 |
| R17 (Claude) | `plan-20261117-sequencer-am.md` | `FAIL` | 5 项 P1（`DEP-SA-08` 改等 `MP-30`、`DEP-SA-07` 接入 `SA-01`、`ADR-SA-02`/`SA-05` 移除 `maintenance.rs` 并改用 `ObjectHash::zero_for_kind(kind)`/`zero_str(kind)`、`SA-09`+`SA-09a` 纳入 `REL-SA-02`、`SA-08` 原子谓词 `<=8`）；R18 已关闭 | 11 项 P2（`实施顺序`、`发布分组`、`Phase 0`、`SA-07` 锚点/写集/`T-3`、`SA-01` `LBR-CLI-002`、审计重数、`默认覆盖`、`DEP-SA-01`/`05`、`SA-03` `COMPATIBILITY.md`、`SA-07d` `commit` 文档）已在 R18 关闭 | `claude -p --model claude-opus-5-5` R17 |
| R17 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 1 项 P1（`SA-09`+`SA-09a` 纳入 `REL-SA-02` 合并发布以保证恢复闭环）；R18 已关闭 | 1 项 P2（关系表 lines 65/68 `DEP-SA-05` 改指 `SA-07d`）已在 R18 关闭 | `codex exec -m gpt-6.1-terra` R17 |
| R18 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 无 | 2 项 P2（`SA-08` 审计表 `AC=8/8` 已同步对齐；`plan-status.md` 依批处理指令由 Coordinator 统一登记）已在 R19 关闭/明确 | `codex exec -m gpt-6.1-terra` R18 |
| R19 (Codex) | `plan-20261117-sequencer-am.md` | `FAIL` | 3 项 P1（`SA-01` `LBR-CONFLICT-001` 退出码 `1` 命令级特例与 `docs/error-codes.md` 写集/测试；`REL-SA-02` 家族窗口内网站 `cf` 仅本地提交、发版后由 `REL-SA-02` 推送并核验 HTTP D 门；`DEP-SA-03` 接入 `SA-01`..`SA-09a` 依赖与审计表）；R20 已关闭 | 2 项 P2（`DEP-SA-08` 对侧镜像同步至 `commit-signing.md`；故障恢复矩阵 `ObjectHash::zero_for_kind(kind)` / `zero_str(kind)`）已在 R20 关闭 | `codex exec -m gpt-6.1-terra` R19 |
| R20–R21 (Codex，补登) | `plan-20261117-sequencer-am.md` | `FAIL` | 原轮次明细未登记于本表；据修订历史 `2026-11-18 08:19:08 UTC` 与 `2026-11-18 09:44:18 UTC` 两行，均为 Codex 复审 FAIL 后的修订（网站交付路径、`SA-09a` GC/`ORIG_HEAD`、`REL-SA-02` 最终树、验证契约） | 未登记 | 修订历史对应两行（R22 补登） |
| R22 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `d4cb1abe0e953aad1c8fcafee9e3fc2eec30fba80debe2fae4d844c74059e906` | `FAIL` | 15 项 P1：nextest 过滤缺 `command::` 前缀且模块回归与「恰好 1 匹配」冲突；未闭合反引号；「文档与网站验收命令」悬空引用；自动生成的无效 AC 映射；`SA-02` 阻断与无可核验审批的 `EX-SA-02-RANGE`；`SA-08a` 阻断；网站 checkout 依据不可核验；未来时间戳；评审记录缺 R20/R21 且维度表失真；`DEP-SA-07` 未按任务 ID 且跨计划行号易漂移；家族子卡 `Version increment`/`Full-suite trigger` 与 `T-3` 误用；VER 计数口径不一；`SA-09a` 不可执行的「只读 GC 根枚举」；集成测试无法对私有函数计时；Phase 1 阻断描述自相矛盾。均在 R22 修订关闭 | 3 项 P2（`show.rs`/`maintenance.rs` 行号、D 组缺 job 名、范围内 `-m` 语义未定义）已在 R22 修订关闭 | Claude 独立评审（源码实查） |
| R22 (Codex gpt-6.1-terra) | 受审 SHA-256 `d4cb1abe0e953aad1c8fcafee9e3fc2eec30fba80debe2fae4d844c74059e906` | `FAIL` | 14 项 P1：`SA-02` 阻断仍标完整；`EX-SA-02-RANGE` 审批不可核验；`SA-08a` 合同未完成；精确过滤缺 `command::` 前缀；模块过滤与恰好 1 匹配冲突；命令反引号不闭合；「文档与网站验收命令」不存在；AC 映射无效；`SA-02` 门计数与追溯矛盾；`REL-SA-02` VER 计数；`DEP-SA-07` 未用稳定 ID；网站 checkout 与 `AGENTS.md` 冲突；未来时间戳与乱序；状态与门禁表述失真。均在 R22 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R22 |
| R23 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `53dafd3b95ffac541ee20907e6b6d133d2443c6bfc5defe525d83b9e9946466f` | `FAIL` | 3 项 P1：评审维度表把 `SA-07` 写成删除公开枚举并标 `T-3`；`SA-05` AC2/AC3 的 unborn fixture 未限定（可能冲突导致 AC 不可满足）；`SA-09` VER4 把三个独立检查合计为 1 门。均在 R23 修订关闭 | 4 项 P2（D 组标题重复、`plan-long.md` 行 DEFER 范围与 `DEP-SA-01` 消费方、ER-04 第 ⑨ 步引用、`REL-SA-02` 未定义 `VERSION`）已在 R23 修订关闭 | Claude 独立评审（源码实查） |
| R23 (Codex gpt-6.1-terra) | 受审 SHA-256 `53dafd3b95ffac541ee20907e6b6d133d2443c6bfc5defe525d83b9e9946466f` | `FAIL` | 9 项 P1：AC 按行而非独立谓词计数；`SA-09` VER 少计；网站前置检查阻断家族后序子卡与发布点；网站提交未强制签名；`SA-09a` GC 断言不能证明 sequencer 根保护；`SA-08a` 8 个 action 的 fixture 不可执行；`REL-SA-02` 未定义 `VERSION`；评审维度表 `T-3` 误述；`plan-long.md` 被写成本计划的提交义务。均在 R23 修订关闭 | 1 项 P2（`nextest list` 行解析规则）已在 R23 修订关闭 | `codex exec -m gpt-6.1-terra` R23 |
| R24 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `d7c52dbb2f77445b904e55564b59c515c64ec4877fa78adfec073ec9bc9eab51` | `FAIL` | 2 项 P1：`SA-08a` AC1 在一条 AC 内列两种输入形态，按本计划自身口径应计 2 项；`SA-01b` 以「`SA-01` AC1 的空停止态」跨卡定义 fixture，违反 G-05。均在 R24 修订关闭 | 3 项 P2（`SA-09` 越界描述仍含 GC 门、`REL-SA-02` 理由与评审维度表的 GC 表述过时）已在 R24 修订关闭 | Claude 独立评审（源码实查） |
| R24 (Codex gpt-6.1-terra) | 受审 SHA-256 `d7c52dbb2f77445b904e55564b59c515c64ec4877fa78adfec073ec9bc9eab51` | `FAIL` | 11 项 P1：快照比较计数口径压缩独立谓词；`list` 前置门未计入 VER；`SA-09` 注册门在家族最终树必然失效且发布点未覆盖；网站检出未遵循 `AGENTS.md:135`；网站提交未核对暂存集、超前提交与远端基线；文档门缺无变化通过路径；收口全量门与记账提交自引用；`SA-08a` 8 个 action 覆盖承诺与验收不符；签名失败场景未分别取证；独立发布卡 bump 触及 `install.sh`/`install.ps1` 却登记 `none`；D 组缺产物可获取性且网站标记未绑定本次部署。均在 R24 修订关闭 | 1 项 P2（测试矩阵 VER 引用错位）已在 R24 修订关闭 | `codex exec -m gpt-6.1-terra` R24 |
| R25 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `3d5e61f2d8a166b7027eed8ace454492c79ce1f185301df9c211df0b45108d76` | `FAIL` | 多项 P1：仍有 AC 以「且」合并两个可分别失败的性质（`SA-02` AC4、`SA-03` AC1/AC4/AC6、`SA-07a` AC2/AC4/AC6/AC7、`SA-07` AC1/AC6、`SA-07c` AC3、`SA-08a` AC6/AC7、`SA-08` AC1/AC7/AC8、发布卡 AC1/AC7）；`SA-01` AC4 合并行数与 todo 两个性质。均在 R25 修订关闭 | 无 | Claude 独立评审（逐条 AC 扫描「且/并/都」） |
| R25 (Codex gpt-6.1-terra) | 受审 SHA-256 `3d5e61f2d8a166b7027eed8ace454492c79ce1f185301df9c211df0b45108d76` | `FAIL` | 11 项 P1：精确过滤要求 0 skipped 与 nextest 语义冲突；家族发布点可能走无漂移捷径跳过交付；`HEAD^{tree}` 不能绑定未提交的被测内容；AC 仍合并独立谓词；多 target 命令计数与判定；`SA-08a` 独立发布与 `ADR-SA-03` 矛盾；`curl \| rg` 管道可把下载失败当零命中；SSH 验签缺字节级提取规则；发布卡要求未绑定验证门；交付检出不存在却声称可开工；G-09 拆分来源不一致。均在 R25 修订关闭 | 1 项 P2（D4/D5 引用残留）已在 R25 修订关闭 | `codex exec -m gpt-6.1-terra` R25 |
| R26 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `f251605628363d3a0106353498dfb074045856f63800d9fd83f3ff0570ca38c1` | `FAIL` | 1 项 P1：发布卡 VER4 含未替换占位 `gh pr view <PR>`。已在 R26 修订关闭 | 3 项 P2（D 组表头遗漏 `REL-SA-01`、「提交序列」比较未定义、D4 的 `VERSION` 未定义）已在 R26 修订关闭 | Claude 独立评审 |
| R26 (Codex gpt-6.1-terra) | 受审 SHA-256 `f251605628363d3a0106353498dfb074045856f63800d9fd83f3ff0570ca38c1` | `FAIL` | 10 项 P1：网站 2d 只接受 `REL-SA-02` 家族草稿；被测内容绑定遗漏未跟踪/删除/二进制文件；`SA-09` 往返判据合并两字段；发布卡 tag 与 note 合计一门；`SA-02` AC4 不能证明拒绝行为；`SA-07a` 清理判据不能证明 sidecar 已删；`SA-01b` 未验收停止态不变；`DEP-SA-10` 缺等待上限与回落；全局规则仍称家族网站全由 `REL-SA-02` 发布；无漂移路径与完成判据冲突。均在 R26 修订关闭 | 1 项 P2（兼容收口清单的 D4 引用）已在 R26 修订关闭 | `codex exec -m gpt-6.1-terra` R26 |
| R27 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `d9ec09856b6345093d168611cb6c8f120e71efb0730240ace791aecd8afff23d` | `FAIL` | 2 项 P1：`SA-09` 往返判据编码/解码口径不一致且少计（`SA-05` 同类）；`SA-07a` 以固定 `.libra/revert-state.json` 断言而非当前 worktree gitdir。均在 R27 修订关闭 | 1 项 P2（ADR 未说明 BLAKE3）已在 R27 修订关闭 | Claude 独立评审 |
| R27 (Codex gpt-6.1-terra) | 受审 SHA-256 `d9ec09856b6345093d168611cb6c8f120e71efb0730240ace791aecd8afff23d` | `FAIL` | 6 项 P1：`SA-09` 往返判据少计；提交后测试与模板 A/B→review→C 顺序冲突；文档逐文件验收与提交后干净树冲突；`status --short` 可能隐藏未跟踪文件；`SA-07a` sidecar 路径未按 worktree gitdir；签名失败未证明不写提交对象。均在 R27 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R27 |
| R28 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `fdb464206eaef11b4ca9d7b0bd63909bd5490ddf01166e2178b0f756a47b4e4f` | `FAIL` | 4 项 P1：`SA-08c` 把签名出口的失败语义拆离出口（G-02）；发布卡把合入/发布后证据列为提交前验收；编码卡缺非零旧行判据；`ORIG_HEAD` 缺失只比 stderr 首行。均在 R28 修订关闭 | 1 项 P2（家族序号与双向拆分声明）已在 R28 修订关闭 | Claude 独立评审 |
| R28 (Codex gpt-6.1-terra) | 受审 SHA-256 `fdb464206eaef11b4ca9d7b0bd63909bd5490ddf01166e2178b0f756a47b4e4f` | `FAIL` | 8 项 P1：发布点验收的时序自相矛盾；`DEP-SA-10` 与卡片依赖不一致；`SA-08c` 拆离失败路径违反 G-02；旧行兼容未验收；`ORIG_HEAD` 缺失判据不足；拆分记录不完整；签名失败 fixture 不确定；里程碑/完成判据要求无网站卡通过 D5。均在 R28 修订关闭 | 1 项 P2（家族子卡序号残留）已在 R28 修订关闭 | `codex exec -m gpt-6.1-terra` R28 |
| R29 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `8ddc6cb0c0964a1686f108c8945141e4af927e32ea639546f83d1536fe3fa227` | `FAIL` | 3 项 P1：`SA-08b` Description 只列 5 个 action 且 fixture 跨卡引用；编解码要求显式 kind 却点名读全局 kind 的 `parse_repo_oid`；`SA-08a` 失败 fixture 自相矛盾。均在 R29 修订关闭 | 1 项 P2（追溯表残留「注册列表检查」）已在 R29 修订关闭 | Claude 独立评审 |
| R29 (Codex gpt-6.1-terra) | 受审 SHA-256 `8ddc6cb0c0964a1686f108c8945141e4af927e32ea639546f83d1536fe3fa227` | `FAIL` | 12 项 P1：发布卡验收顺序与模板 A/B→评审→C 冲突；聚合命令违背「恰好一个测试」；`REL-SA-02` AC3 少计；编解码与 `parse_repo_oid` 矛盾；`SA-08b` 漏列 action 且跨卡 fixture；无漂移路径跳过前端验证与评审；收口差异检查未判退出码；`SA-09a` GC 引用错误；`SA-08a` 失败 fixture 矛盾；失败后状态与 abort 结果未验收；`SA-09` 注册检查无命令；文档守卫适用性未写明。均在 R29 修订关闭 | 1 项 P2（收口清单 D5 路径）已在 R29 修订关闭 | `codex exec -m gpt-6.1-terra` R29 |
| R30 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `215564b3abab8544e7f7df5c8fd6d97336662284540990bc4fdfd3dabf7d159d` | `FAIL` | 4 项 P1：家族子卡 `Migration and rollback` 写「同 `SA-09`」跨卡引用；`SA-07a` 未验收 `--continue` 成功后的状态清理与错误码/写入前拒绝；发布卡改动集守卫看不到已提交的家族改动；`SA-08a` 签名失败后的恢复只验 `--abort` 退出码。均在 R30 修订关闭 | 2 项 P2（`show.rs` 锚点、`Verification` 表头未提第 1b 条）已在 R30 修订关闭 | Claude 独立评审 |
| R30 (Codex gpt-6.1-terra) | 受审 SHA-256 `215564b3abab8544e7f7df5c8fd6d97336662284540990bc4fdfd3dabf7d159d` | `FAIL` | 5 项 P1：`SA-07a` 未验收 `--continue` 成功后的状态清理；发布点改动集守卫检查不到已提交的家族改动；`SA-08a` 签名失败后的恢复判据不足；`SA-07a` 的错误码与写入前拒绝无对应验收；`show` 关键行号错误。均在 R30 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R30 |
| R31 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `1ba966552b4b5105b53d9f968060837515358f5682bc7e3d0e4426aa359c93e3` | `PASS` | 无 P0/P1 | 4 项 P2（`SA-07e` 证据缺 `sequencer/mod.rs:1113`、`SA-08c` 未写 SSH 测试密钥生成、「同 `SA-…`」写法、测试矩阵「安全」行混入 `SA-07e`）已在 R31 修订关闭 | Claude 独立评审 |
| R31 (Codex gpt-6.1-terra) | 受审 SHA-256 `1ba966552b4b5105b53d9f968060837515358f5682bc7e3d0e4426aa359c93e3` | `FAIL` | 7 项 P1：`SA-08c` 把出口的正向签名测试拆离实现卡；`SA-08a` 签名失败恢复未验退出码、状态清除与索引/工作区恢复；`SA-09` 单封补丁只验无父提交；`DEP-SA-06` 引用的 `LS-26`/`LS-27` 为 `log` 任务；首段卡数与发布分组矛盾；`SA-07d` 状态清理承接仍指 `SA-07a`；`SA-08b` AC8 合并尾注与签名两个性质。均在 R31 修订关闭 | 1 项 P2（`SA-08c` 编号撤销后复用）已在 R31 修订关闭（撤销且不再复用） | `codex exec -m gpt-6.1-terra` R31 |
| R32 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `d2f73559c542475df28c90fe89fbeb95012873749a65b02f2c544ed66b9fed17` | `PASS` | 无 P0/P1 | 1 项 P2（`SA-08a` AC5 的说明性括注）已在 R32 修订关闭 | Claude 独立评审 |
| R32 (Codex gpt-6.1-terra) | 受审 SHA-256 `d2f73559c542475df28c90fe89fbeb95012873749a65b02f2c544ed66b9fed17` | `FAIL` | 4 项 P1：签名失败 fixture 未保证分叉，可能走「已是最新」短路；`SA-08f` 未验收补丁内容保留；签名出口的失败处理与恢复被拆离出口卡；最终树差异不能证明窗口内无插入提交。均在 R32 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R32 |
| R33 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `680dceffa6a777b413011556f635a12948f93c9341e53d9bdc3d06b87e383423` | `PASS` | 无 P0/P1 | 2 项 P2（`SA-08d` fixture 把 stderr 称作不计数诊断而 AC1 计数；9a 的完整性方向未单列为判据）已在 R33 修订关闭 | Claude 独立评审 |
| R33 (Codex gpt-6.1-terra) | 受审 SHA-256 `680dceffa6a777b413011556f635a12948f93c9341e53d9bdc3d06b87e383423` | `FAIL` | 5 项 P1：签名失败停止态的恢复路径拆离产生停止态的卡；9b 与强制记账写集冲突；上游前移重建未要求重跑受影响的门与评审；fixture 引用未创建的 `T` ref；`SA-05` unborn 普通 pick 未验补丁内容。均在 R33 修订关闭 | 1 项 P2（`--abort` 证据只引帮助文案）已在 R33 修订关闭 | `codex exec -m gpt-6.1-terra` R33 |
| R34 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `4d89e4ca850f95ba25f692326fc36b92435b51c9f0dc3a46d50c239ac15e1a04` | `PASS` | 无 P0/P1 | 1 项 P2（`SA-08h` 只验 `pick` 的 `--signoff`）保留为残余风险 | Claude 独立评审 |
| R34 (Codex gpt-6.1-terra) | 受审 SHA-256 `4d89e4ca850f95ba25f692326fc36b92435b51c9f0dc3a46d50c239ac15e1a04` | `FAIL` | 7 项 P1：原子回滚未覆盖多步、autostash 与回滚失败；「无残留状态」未直接检查序列行与辅助状态；验签可能落在旧提交；家族提交链未验线性与顺序；重建评审条件不足；「已是最新」短路条件描述不全；合入树检查晚于发布。均在 R34 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R34 |
| R35 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `baaed15b49219588091d89551b16e9f962558c4e1dbc6b45db56004a16e363dd` | `PASS` | 无 P0/P1 | 1 项 P2（`DEFER-SA-07` 留下中途签名失败的端到端恢复未验）已在 R35 修订关闭（撤销该延期并新增 `SA-08j`、`SA-08k`） | Claude 独立评审 |
| R35 (Codex gpt-6.1-terra) | 受审 SHA-256 `baaed15b49219588091d89551b16e9f962558c4e1dbc6b45db56004a16e363dd` | `FAIL` | 1 项 P0（预检后中途签名失败经既有路径停止，`--continue` 可能丢失补丁）+ 5 项 P1：`SA-08i` 判据不能证明仓库原状；预检未限定于会生成提交的路径；合入树不一致时未重跑构建与安装；`SA-09a` 以合成 GC 用例代替真实 `am` 暂停态；`SA-08a` 库内 fixture 隔离未定义。均在 R35 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R35 |
| R36 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `a67cbca51710226a860d37d5de58933e63de79070aef1a917cfb6f9ced68164c` | `PASS` | 无 P0/P1 | 1 项 P2（`SA-08j` 以 stderr 含 failpoint 变量名判定，只证明注入路径被触发、不证明签名错误被透传）已在 R37 修订关闭（改为断言注入的签名错误文本） | Claude 独立评审 |
| R36 (Codex gpt-6.1-terra) | 受审 SHA-256 `a67cbca51710226a860d37d5de58933e63de79070aef1a917cfb6f9ced68164c` | `FAIL` | 4 项 P1：`SA-08j`/`SA-08k` 的 fixture 以 `SA-08i` 预检通过为前提却排在其前；`SA-08j` 未证明停止态完整索引树与工作区、`--continue` 后完整树（含 `u.txt`）与有效签名；恢复只测单个 `pick`，未覆盖多步重放与 `squash`/`fixup` 折叠下的 `--continue`/`--abort`；第 10 条 merge 树与受测树不同时只重跑 C 组。均在 R37 修订关闭（用户批准 `EX-SA-01`，门族卡 `SA-08j`） | 无 | `codex exec -m gpt-6.1-terra` R36 |
| R37 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `7274493d74e66aee07047f747ec01f77ac548199ed6f406811722d6d69b93b1c` | `FAIL` | 2 项 P1：`SA-05a`、`SA-05b`、`SA-09b` 未登记 `DEP-SA-03`，`SA-05b` 与 commit-signing `CS-18b` 共写 `tests/command/cherry_pick_test.rs` 而无串行登记；`SA-08i` AC7 断言 `sequence_state` 无 rebase 行，但 rebase 状态存于 `rebase_state` 表（判据恒真）。均在 R38 修订关闭 | 2 项 P2（`SA-08h` AC 自指标签、`ADR-SA-03` Context 未列改写路径）已在 R38 修订关闭 | Claude 独立评审 |
| R37 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `7274493d74e66aee07047f747ec01f77ac548199ed6f406811722d6d69b93b1c` | `FAIL` | 21 项 P1：A 区——家族持窗等待 `REL-CS-01` 可死锁；合入树重跑漏 1a–1b 与 `SA-09` 索引检查；失败重试只核索引。B 区——`SA-01` 停止提交应在 `current_oid`；`SA-02` 冲突路径按原参数下标取标签会越界；`SA-03` 缺「否定后肯定」生效判据；`SA-05` unborn `--abort` 未核成功退出与状态释放。C 区——`SA-07a` 应豁免 `--no-commit`、缺非 0 退出；`SA-07b` 未覆盖冲突后 `--continue` 与多提交序列；`SA-07` 未核结果树。D 区——`SA-08b` 只验签名关闭；`SA-08i` 漏交互入口、未核对象库、锚点 941 应为 959。E 区——`SA-08j` 合并判据少计；重试后停止态未全量比较；中止门未证明起点；折叠消息用同实现对照。F 区——`SA-08` 的 `--apply` 后端不存在；`SA-09c` GC 门漏待处理邮件。均在 R38 修订关闭（`SA-08i` 三项随预检撤回关闭；`SA-07a` 非 0 退出按建议补判据；`SA-09c` 待处理邮件登记 `DEFER-SA-09`） | 无 | `codex exec -m gpt-6.1-terra` R37（分区 A–F） |
| R38 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `ccd063c526b13fc4aba2b5173311decc9e0e4616e77f6a30620673167d37e713` | `FAIL` | 2 项 P1：`SA-08a` 的 `Dependencies`/审计行缺家族入口依赖 `DEP-SA-02`；`DEP-SA-04` 行仍写 `SA-09` 被阻断而 `SA-09b` 已登记该依赖。均在 R39 修订关闭 | 无 | Claude 独立评审 |
| R38 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `ccd063c526b13fc4aba2b5173311decc9e0e4616e77f6a30620673167d37e713` | `FAIL` | 14 项不重复 P1：A 区——`SA-08a` 缺 `DEP-SA-02`（与 D 区重复）；无漂移路径无窗口释放事件；网站生成产物未入写集；故障恢复矩阵的 `am` GC 保证与 `DEFER-SA-09` 矛盾。B 区——`SA-02` 分叉范围与合并提交被拒矛盾；范围冲突判据在 panic 下仍可成立。C 区——`SA-07c` 误称 Git 不支持 `revert -`、`-` 的位置过宽。D 区——`SA-08h` 未验收 `fixup -c` 与折叠组末尾 `squash` 的编辑改写（R39 以同一调用点证据驳回并写明，不加判据）。E 区——Git 依据 `PICK_RESULT_ERROR` 不成立；真实签名错误未经恢复门；failpoint 的 `LIBRA_TEST` 守卫未验。F 区——`SA-09a` 未验成功退出与状态释放；Git 对照只比树、测不出邮件元数据丢失；`SA-08` 只验 HEAD OID 改变。除 D 区一项驳回外均在 R39 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R38（分区 A–F） |
| R39 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `3f1603fc592091dc91431c69d9171e45d89d528de54c50950312bf5dcf3b5197` | `PASS` | 无 P0/P1 | 2 项 P2（`SA-08` AC5「同一 fixture」指代不清，已在 R40 改名；G6 移除 `LIBRA_TEST` 后其它以之判定的便利行为按生产默认运行，登记为残余风险） | Claude 独立评审 |
| R39 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `3f1603fc592091dc91431c69d9171e45d89d528de54c50950312bf5dcf3b5197` | `FAIL` | 11 项不重复 P1：`ADR-SA-03` 仍引 `PICK_RESULT_ERROR`（A/D 区）；`SA-02` 引用不存在的 VER8（A/B 区）；`REL-SA-01` 追溯表聚合数 18 应为 20（A/D 区）；`SA-01` 未验多提交空停止的 `todo` 与 `--skip` 续拣；`SA-01b`/`SA-01c` 未断言错误码；`SA-07b` 冲突后 `commit -a` 未用 `--reference` 消息；`SA-07e` 中止只核 HEAD；签名默认写成 `vault.signing` 默认与 `DEP-SA-01` 矛盾；G6 可在丢步时通过；failpoint 的 `LIBRA_TEST=1` 精确值未验（R40 改为与既有判定一致的「设置即生效」）；`SA-09d` 漏首封暂停的 `--abort`。均在 R40 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R39（分区 A–F） |
| R40 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `762b8250bbf59a09f11490cac870e8970079261a3f7c53ec9c0e92f86645d3c1` | `PASS` | 无 P0/P1 | 2 项 P2（`SA-09d` AC2 缺空格；`SA-08j`「不移动任何引用」措辞应限定分支与标签），已在 R41 修订关闭 | Claude 独立评审 |
| R40 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `762b8250bbf59a09f11490cac870e8970079261a3f7c53ec9c0e92f86645d3c1` | `FAIL` | 16 项 P1：A——`DEP-SA-04` 要求对侧未必修改的 `tests/INDEX.md`。B——`SA-01` 未判 `current_oid`、单提交停止未判状态行与 `--abort` 退出；`SA-05` 快照可为空。C——`SA-07e` 中止未判 HEAD 与退出；`stopped_revert_message()` 可复用已收口消息；`SA-07a` 只测已暂存新增。D——`SA-08a` 未验 `vault.signing` 回退与优先级、签名决策锚点错误；折叠签名判据不能证明折叠发生（R41 驳回：折叠语义未改，由共享回归组既有用例 `rebase_test.rs:648,1387,7469`、`rebase_interactive_test.rs:528,632-659` 守护，并写入 `SA-08b`/`SA-08m` 描述）。E——`stopped_sha` 未判；「不移动引用」与 HEAD 行为不符；折叠步 Git 路径为 `error_failed_squash`。F——`SA-09d` 两种暂停态合计少计；`SA-09a` 未判恢复命令成功退出；`--force-rebase` 判据可在空列表上成立。除 D 区一项驳回外均在 R41 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R40（分区 A–F） |
| R41 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `4ed945d289d76046e82081c156cbcf4ceb4342da74fa9ef2f6cfcea81960fa6f` | `PASS` | 无 P0/P1（Codex 同字节 18 项 P1 经核实除两项外成立，含本人 `SA-09a` 链设计把会进入下一冲突的恢复命令写成 exit 0 的错误） | 无 | Claude 独立评审 |
| R41 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `4ed945d289d76046e82081c156cbcf4ceb4342da74fa9ef2f6cfcea81960fa6f` | `FAIL`（D 区 PASS） | 18 项 P1：A——多页面 D5、恢复矩阵 HEAD 表述、`ADR-SA-01` 归属、追溯表 `TC-0522` 归属；B——`SA-01c` 显式停止起点与验收不足、`SA-01` 恢复不完整、`SA-02` 冲突恢复只查退出码、`SA-05a` 缺 `Some(OID)` 写出、`SA-05` 缺已暂存用户数据（R42 以 Git `error_dirty_index` 证据驳回）；C——`SA-07` detached 中止、`SA-07f` 缺无关数据、`SA-07e` 缺结果树、`SA-07a` 缺路径集合；E——成功恢复缺最终索引/工作区、中止快照缺无关数据、G5/G6 缺 HEAD 与状态；F——`SA-09a` 中间恢复命令退出码与 Git 矛盾。除 B-6 与 B-3 的复原部分驳回外均在 R42 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R41（分区 A–F） |
| R42 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `124f2024d1ad7fd473e32d6272ad4afe1337fefa9ae55f64abb2f7c68e9b5c67` | `FAIL` | 经源码核实 Codex R42 的 D-1（`tests/SERIAL_CLASSIFY.sh:32-33`）、C-1（`src/command/revert.rs:1366-1387`）等 P1 成立；收敛评估：上游测试复刻类卡在 G-03 逐谓词计数与 G-02 禁止只含测试的拆分卡之间无法同时满足，升级用户决定（用户 2026-11-19 05:17:51 UTC 决定收窄到 issue 核心断言） | 无 | Claude 独立评审 |
| R42 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `124f2024d1ad7fd473e32d6272ad4afe1337fefa9ae55f64abb2f7c68e9b5c67` | `FAIL` | 17 项 P1：A——多页面网站一页无漂移、一页有改动时缺分流规则；M1 漏 `SA-02b`；B——`SA-01` 显式模式缺退出码与错误码；`SA-02b` 违反 G-02 且缺工作区/索引判据；`SA-03` 否定模式缺工作区判据、缺跨 `--continue` 的第二冲突；`SA-05` AC2 元组合并计数；`SA-05a` 缺非法格式解码失败；C——`SA-07h` 前提不成立；`SA-07a` 缺非 0 退出与 HEAD 不变；`SA-07e` 缺初次停止判据；D——`SA-08a` 误登记 `tests/SERIAL_REGISTRY.tsv`；E——`SA-08j` G2/G4/G5 停止态缺工作区判据；F——`SA-09b` 缺 `Some(OID)` 写出；`--force-rebase` 长形式未在改变行为的场景验收；unborn `am --abort` 缺符号 HEAD 判据。R43 修订按用户决定处置：A-1、B-7（移入 `SA-09f`）、C-2、D-1、E-1、F-1、F-2、F-3（并入 `SA-09a`）与 B-4（移入 `SA-03b`）修复；A-2、B-1、B-2、B-3、B-5、B-6、C-1、C-3 随收窄撤卡或登记 DEFER 消解 | 无 | `codex exec -m gpt-6.1-terra` R42（分区 A–F） |
| R43 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `28a0069bf522b33aafea00761b70e433cb1d31c322fa477e96b45938116fb56a` | `FAIL` | 3 项 P1：unborn 起点暂停的降级指引指向已延后的 `--continue`；关系表 `DEP-SA-09` 仍指 `SA-09b`；`SA-08j` R43 插入的 G4/G5 判据步骤引用错误（同 Codex E-1）。另经源码核实 Codex R43 的 17 项 P1 均成立 | 无 | Claude 独立评审 |
| R43 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `28a0069bf522b33aafea00761b70e433cb1d31c322fa477e96b45938116fb56a` | `FAIL` | 17 项 P1：A——#729 登记漏验收项 1、3；库内用例路径规则与 `internal::sequencer` 冲突；B——`SA-05` 快进未保护未跟踪文件；`SA-01b` 缺点名选项对的诊断；`SA-03` 诊断判据过宽；`SA-03b` 末位肯定判据可被拒绝的命令满足；`SA-02` 未验收未跟踪覆盖出口的越界；C——`SA-06` 未验收 `--empty=drop` 不改 HEAD；`SA-07` orphan/detach 序列未验收无残留状态；`SA-07c` 未单列 `checkout -b side-branch`；D——`SA-08g` 签名决策锚点错误；E——`SA-08j` G4/G5 插入判据引用其它门的步骤；G2/G4 停止态缺 `show-ref`；F——`SA-09a` 缺后代提交保护与第二封暂停的状态释放；`SA-08` 长形式判据可空真、签名结果未对应 `sixth`/`seventh`；`SA-09` 以合并元组计 1 项。均在 R44 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R43（分区 A–F） |
| R44 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `1d66d7cbdfadee43adafcca5ba25bf35c86dd8014dce2d3b255fe5df1f9d87b8` | `FAIL` | 无本人新增 P1；经源码核实 Codex R44 的 12 项 P1 均成立（GC-04 见 `plan-template.md:219`；改写路径见 `src/command/rebase.rs:2067,4045,4412`） | 状态行轮次标识滞后（同 Codex A-3），R45 更正 | Claude 独立评审 |
| R44 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `1d66d7cbdfadee43adafcca5ba25bf35c86dd8014dce2d3b255fe5df1f9d87b8` | `FAIL`（E 区 PASS） | 12 项 P1：A——`SA-02 VER5` 不存在；#907 登记宣称多封历史一致但未验收根提交元数据；B——拒绝判据未验收非 0 退出（`SA-01b`、`SA-01c`、`SA-03`、`SA-02`、`SA-05`）；C——`SA-07` 未经 orphan 过渡；`SA-07a` 缺 `docs/error-codes.md`；`SA-07a` 缺 `--json`/`--machine` 判据（GC-04）；D——`SA-08h` 改写路径的签名失败不被 `SA-08j` 的恢复合同覆盖；F——`SA-09e` 缺首封暂停后用户 root 提交的保护；全零哨兵可错误长度通过；`SA-08` 未要求命令成功。均在 R45 修订关闭 | 状态行轮次（A-3，P2）已更正 | `codex exec -m gpt-6.1-terra` R44（分区 A–F） |
| R45 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `234b0e26c8451177fe1947fcda956ac14927111855ff7068035a296947dcdc21` | `FAIL` | 本人 1 项 P1：`SA-07d` 两条拒绝与 `SA-07c` 的 `<X> -` 拒绝未验收退出状态；另经源码核实 Codex R45 的 10 项 P1 均成立 | 状态行轮次（同 Codex A-4）R46 更正 | Claude 独立评审 |
| R45 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `234b0e26c8451177fe1947fcda956ac14927111855ff7068035a296947dcdc21` | `FAIL`（D 区 PASS） | 10 项 P1：A——GC-04 的显式 `--json`/`--machine` 未分别验收；评审维度表仍宣称改写路径签名失败可恢复；对侧 `DEP-MP-08` 语义描述不符；B——`SA-02` 的主题序列取自只输出 OID 的 `rev-list`；C——`SA-07a`「修改」拒绝缺退出状态；`SA-07c` 无上一检出记录缺错误码与诊断；`SA-07` 未验收 detach 过渡；E——`REL-SA-01` 仍宣称闭合全部失败恢复；F——`SA-09` 的 `c1..c3` 排除 `c1`；`SA-09e` 误述 Git 在 HEAD 移动时拒绝中止。均在 R46 修订关闭 | 状态行轮次（A-4，P2）已更正 | `codex exec -m gpt-6.1-terra` R45（分区 A–F） |
| R46 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `e6570d10c817e89de47035c91c5f7c73d05b08e81a6fd55d46dff281fde06b67` | `FAIL` | 同意 Codex R46 的 14 项 P1；判定 GC-04 逐失败路径分别验收与 G-03 上限、G-02、不扩大 waiver 三者冲突，需用户决定 GC-04 验收粒度 | 无 | Claude 独立评审 |
| R46 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `e6570d10c817e89de47035c91c5f7c73d05b08e81a6fd55d46dff281fde06b67` | `FAIL`（D 区 PASS） | 14 项 P1：A/B/E——「输出契约门」不计入 G-03 未获模板许可；输出契约门以默认运行为基准，无法发现一致错误的稳定码；B——`SA-05` 拒绝缺稳定码与诊断；`SA-02` 停止缺人读诊断；C——`SA-06` 未验收 `format:` 与 `--stat` 的分隔；`SA-07a`「修改」拒绝、`SA-07` detached 拒绝、`SA-07c` 非首位 `-` 缺稳定码/诊断/退出状态；E——`SA-08j` 未规定签名失败的稳定码且缺 `docs/error-codes.md`；`--continue` 失败路径未纳入输出模式核对；F——`SA-09a`/`SA-09e` 拒绝后未验收保留 `am` 状态；两处拒绝缺诊断与字面稳定码。待用户决定 GC-04 验收粒度后整改 | 状态行轮次（A-3，P2）已更正 | `codex exec -m gpt-6.1-terra` R46（分区 A–F） |
| R47 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `98e86fa0104d88af2c1f10b33fc6274b39875bf6ec053e589f8b51da63f177e2` | `PASS` | 无 P0/P1；核对第 11 条（11a–11d）的落实、23 张活动卡的计数/依赖/审计表、撤回卡的全文去向与源码锚点（`src/utils/error.rs:1066-1076,1137-1164`、`src/command/cherry_pick.rs:133,245-247,280-285,293-308,880-895`、`src/command/rebase.rs:723-731,4122-4146,4467-4473`、`src/command/am.rs:381-410,1335-1337`） | 3 项 P2：unborn 起点暂停恢复未验收（决定所致，已登记风险）；`SA-01b` 排版；对侧 `DEP-CS-13`/`DEP-MP-08` 待对侧同步 | Claude 独立评审 |
| R47 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `98e86fa0104d88af2c1f10b33fc6274b39875bf6ec053e589f8b51da63f177e2` | `FAIL`（C、D 区 PASS） | 7 项 P1：A——「验证执行协议」第 3 条要求 `am` 用例对照作者/日期/消息，与 `SA-09` 的延后矛盾；`SA-09d` 撤回说明仍称其判据由已撤回的 `SA-09a` 验收（F 区同）；B——`SA-03b` 新可达的冲突停止未断言稳定码；E——`SA-08j` 代表路径的人读判据可能只命中 stderr 末尾的 JSON 报告；F——`SA-09` 写明 fail-closed 拒绝却不验收；降级指引推荐未验收且首封暂停时不可执行的 `--skip`。均在 R48 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R47（分区 A–F） |
| R48 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `2af8e40c9835039595ef07b91a31836efa8a933acb2bd533d37478226bed8ae0` | `PASS` | 无 P0/P1；核对第 3、11 条改动、「人读 stderr」在全部诊断类 AC 的落实、`SA-03b` 8/8、`SA-09g` 与源码（`src/command/am.rs:381-420`、`src/internal/sequencer/mod.rs:878`、Git `builtin/am.c`） | 3 项 P2：`SA-09` 关于延后范围的措辞；`SA-09g` 与 Git 的差异需告知协调者；对侧引用待同步 | Claude 独立评审 |
| R48 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `2af8e40c9835039595ef07b91a31836efa8a933acb2bd533d37478226bed8ae0` | `FAIL`（D、E 区 PASS） | 8 项 P1：A/F——`SA-09g` 未验收索引与符号 HEAD 不变；`SA-09g` 有公开行为变化却标 `S`；F——两种暂停的 `am` 冲突停止未断言稳定码；既有提示（「`--abort` 复原原分支」、推荐 `--continue`/`--skip`）与「只释放状态」的出口矛盾；与 `git am --quit` 等同的说法忽略了 rerere 清理；B——`SA-05` 的 unborn 快进可能覆盖已暂存条目；C——`SA-07c` 复用的上一检出解析还会返回读取失败（`LBR-IO-001`）与记录损坏（`LBR-REPO-002`），未登记也未验收。均在 R49 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R48（分区 A–F） |
| R49 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `04fc9754bfa9acfdba018267460d7bef5744b3fda8f29803dd679ba4c2e21d5e` | `PASS` | 无 P0/P1；核对恢复的 `SA-09a`、`SA-09e`、重定义的 `SA-09g`、新增 `SA-05c` 与全文计数/依赖/家族表，源码锚点（`src/command/cherry_pick.rs:167-168,224,321,1956-2002,2367-2369`、`src/command/switch.rs:445-465`、`src/command/am.rs:386-407,1339-1343`、Git `sequencer.c:653-664`） | 3 项 P2：恢复卡需告知协调者；首封暂停 `--skip` 限制；对侧引用待同步 | Claude 独立评审 |
| R49 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `04fc9754bfa9acfdba018267460d7bef5744b3fda8f29803dd679ba4c2e21d5e` | `FAIL`（C、D、E 区 PASS） | 8 项 P1：A/F——风险表与 `SA-09e` 称首封暂停支持未验收的 `--continue`；B——`SA-05c` 未验收拒绝后不留 sequencer 状态；`SA-05` 未按 G-09 写明拆出 `SA-05c`；F——`SA-09g` 改写提示却未按代表路径验收；其第二封 fixture 未作改动就提交、无法生成 `U`；两种 fixture 暂停未断言稳定码；`SA-09a` 的复原未定义中断窗口与重试合同。均在 R50 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R49（分区 A–F） |
| R50 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `d1f8c47278727b4c933a678d13349b40e30a6fb358d1fda20cdfe650b16dbabe` | `PASS` | 无 P0/P1；核对第 11a 条提示口径、`SA-09a` 写入顺序与 failpoint、`SA-09g` 5 条判据、`SA-05c` AC5 与源码（`src/command/am.rs:386-407,756-758`、`tests/command/mod.rs:375`） | 3 项 P2：`SA-09a` AC8 括注宽于判据；恢复卡需告知协调者；对侧引用待同步 | Claude 独立评审 |
| R50 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `d1f8c47278727b4c933a678d13349b40e30a6fb358d1fda20cdfe650b16dbabe` | `FAIL`（C、D、E 区 PASS） | 6 项 P1：A/F——`SA-09a` 的重试判据只看状态行，不能证明注入生效与复原完成；F——引用删除后、状态清除前中断时，用户新建的无关 root 会在重试时被删（既有保护只拒绝后代）；`SA-09g` 未验收拒绝前索引、工作区与符号 HEAD 不变；未验收改写后的保存建议与旧 `reset --hard` 提示的消失；B——`SA-05c` 沿用的既有提示（`--skip` 等）在无停止序列时不可用。均在 R51 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R50（分区 A–F） |
| R51 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `8ca100a6995b2ba6fd5d6a72adc461ae2ca761f53b228113288dae32cccb67df` | `PASS` | 无 P0/P1；核对事务化删除、`SA-09g` 统一判定、`SA-09h` 与全文 28 卡计数，源码（`src/internal/branch.rs:829`、`src/internal/sequencer/mod.rs:878-895`、`src/utils/error.rs:1339-1341`） | 2 项 P2：恢复卡需告知协调者；对侧引用待同步 | Claude 独立评审 |
| R51 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `8ca100a6995b2ba6fd5d6a72adc461ae2ca761f53b228113288dae32cccb67df` | `FAIL`（B–E 区 PASS） | 3 项 P1：A/F——中止保护判定与删除事务之间存在并发窗口，按名称无条件删除会丢弃并发移动后的分支 tip；F——`SA-09h` 不能证明中断发生在复原之后。均在 R52 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R51（分区 A–F） |
| R52 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `5e12efe961c3ee933c7938a99a4f947b9db154fa6c831f623eca091b78091c67` | `PASS` | 无 P0/P1；核对条件删除、`SA-09h`/`SA-09i` 与 29 卡计数，源码（`src/internal/branch.rs:829-876`、`src/command/commit.rs:113,1252-1260`、`src/command/update_ref.rs:41-48`） | 2 项 P2：活动卡增至 29 需告知协调者；对侧引用待同步 | Claude 独立评审 |
| R52 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `5e12efe961c3ee933c7938a99a4f947b9db154fa6c831f623eca091b78091c67` | `FAIL`（C、D、E 区 PASS） | 6 项 P1：A——`SA-09h`、`SA-09i` 只加测试而实现在 `SA-09a`（G-02 碎片）；R52 修订历史中 `SA-09i` 的判据数写法矛盾；B——`SA-05c` 提示的 `libra rm --cached` 在已暂存与工作区内容不同时会失败；F——首封暂停时「删除恰 1 行」的条件使中止不可能成功；判定与复原之间的并发工作区写入无保护；自写的条件删除绕过既有删除函数的 linked worktree 保护与元数据级联。均在 R53 修订关闭（并发工作区写入登记 `DEFER-SA-24`） | 无 | `codex exec -m gpt-6.1-terra` R52（分区 A–F） |
| R53 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `3ac4f42a47c96d329d3eeb67b6359bc5ea9ecaef853013585484bcaf6ac8156f` | `PASS` | 无 P0/P1；核对实现归属重排、两支比较、`DEFER-SA-24` 与源码（`src/command/remove.rs:285-288,350-362`、`src/internal/branch.rs:829-876`） | 2 项 P2：活动卡增至 29 需告知协调者；对侧引用待同步 | Claude 独立评审 |
| R53 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `3ac4f42a47c96d329d3eeb67b6359bc5ea9ecaef853013585484bcaf6ac8156f` | `FAIL`（B–E 区 PASS） | 2 项 P1（A、F 区各报同两项）：`SA-09h` 的 failpoint 位于事务之前，无法发现删除与清除分别提交的实现；`SA-09i` 未验收 `expected_head=None`（首封暂停）时分支被并发创建的比较分支。均在 R54 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R53（分区 A–F） |
| R54 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `4dccb17fe669724097731d9f9856d814d312a85d84851274ce8bea8cb81d7351` | `PASS` | 无 P0/P1；核对 `SA-09h` 事务内 failpoint、`SA-09i` 两种形态与聚合守卫（8 个集成 + 2 个库内用例），源码（`src/command/commit_tree.rs`、`src/command/write_tree.rs`、`src/internal/branch.rs:829`） | 2 项 P2：活动卡增至 29 需告知协调者；对侧引用待同步 | Claude 独立评审 |
| R54 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `4dccb17fe669724097731d9f9856d814d312a85d84851274ce8bea8cb81d7351` | `FAIL`（A、B、D、E 区 PASS） | 3 项 P1：C——`SA-07c` 的标题、描述与追溯行仍称闭环上游 `t3501#12`/`TC-0484`（#728 验收项 6 已登记 `DEFER-SA-13`）；F——`SA-09h` 的判据不能证明 failpoint 位于事务内的删除之后；`SA-09f` 缺 SHA-256 非零 OID 的正向读回判据。均在 R55 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R54（分区 A–F） |
| R55 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `5e49bd6919d5086bbbd4c542293693f0db360da244aad5f67ddce2e9b32101df` | `PASS` | 无 P0/P1；核对 `SA-07c` 收窄措辞、`SA-09h` failpoint 消息判据、`SA-09f` SHA-256 正向读回与追溯行 | 2 项 P2：活动卡增至 29 需告知协调者；对侧引用待同步 | Claude 独立评审 |
| R55 (Codex gpt-6.1-terra，6 分区并行) | 受审 SHA-256 `5e49bd6919d5086bbbd4c542293693f0db360da244aad5f67ddce2e9b32101df` | `FAIL`（B–F 区 PASS） | 1 项 P1（A 区）：「与其它计划的关系」仍把 `SA-09b` 写成 `DEP-SA-04` 约束的 `REL-SA-02` 家族入口，与依赖表、实施顺序和发布分组的 `SA-09f` 不一致。已在 R56 修订关闭 | 无 | `codex exec -m gpt-6.1-terra` R55（分区 A–F） |
| R56 (Claude subagent claude-opus-5-5) | 受审 SHA-256 `62bc0c5cc683820fb0b88c4ef743642b2fbbc5058743b9ac88bd314d857e02cb` | `PASS` | 无 P0/P1；R55 已全量 PASS，R56 只改状态行、「与其它计划的关系」中 `DEP-SA-04` 的家族入口（`SA-09f`）与记账行，核对其与依赖登记表、实施顺序、`REL-SA-02` 家族行及 `SA-09f`/`SA-09b`/`SA-09` 的 `Dependencies` 一致 | 2 项 P2：活动卡由 23 增至 29（unborn 起点 `am` 暂停的 GC-07/GC-08 安全卡）需告知协调者；对侧 `DEP-CS-13`/`DEP-MP-08` 待对侧同步 | Claude 差异确认评审 |
| R56 (Codex gpt-6.1-terra，A 区全量 + B–F 区差异确认) | 受审 SHA-256 `62bc0c5cc683820fb0b88c4ef743642b2fbbc5058743b9ac88bd314d857e02cb` | `PASS` | 无 P0/P1：A 区（非卡片章节）全量复审 PASS；B–F 区核对任务卡区段与 R55（B–F 区已全量 PASS）逐字节一致、差异只含状态行、第 83 行与记账行，PASS | 无 | `codex exec -m gpt-6.1-terra` R56（A、BF） |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| `DEFER-SA-01` | 合并 `cherry-pick` 与 `revert` 为单一 sequencer 引擎 | 当前各自已有稳定持久化边界 | 另有重构 ADR 时 | `plan-long.md` |
| `DEFER-SA-02` | `rebase -r`/`--rebase-merges` 与 `-s recursive --strategy-option=theirs`（`TC-2371` 的非 `-f` 部分） | 独立的 rebase 合并策略轴 | `rebase` 策略专项计划立项时 | `plan-long.md` |
| `DEFER-SA-03` | `TC-2289` 残余 apply 后端（`rebase --apply`，现以 `LBR-UNSUPPORTED` 拒绝）与 `--show-current-patch`，`TC-2488` 残余 `backup-git-rebase-todo` 检查 | 非 `-f` 标志轴（apply 后端是独立的 rebase 后端实现，`issues/523` 只要求诊断） | `rebase` 交互与状态检查专项立项时 | `plan-long.md` |
| `DEFER-SA-04` | `cherry-pick -m` 用于非合并提交与范围（Git ≥ 2.21 接受）；范围含合并提交时与 Git 一致的诊断文本与停止状态；`A...B`、`^A B` 等其它修订语法 | 独立的修订语法与 mainline 轴；`SA-02` 沿用现有逐提交拒绝 | 有 issue 要求或 `cherry-pick` 修订语法专项立项时 | `plan-long.md` |
| `DEFER-SA-05` | `am` 暂停期间 `CHERRY_PICK_HEAD`/`REVERT_HEAD` 等其它 `load_for_scope` 消费者因 `unknown sequence kind 'am'` 报错 | 非 unborn `am` 轴（`am` 行的 `ORIG_HEAD` 投影随 `SA-09c` 撤回登记 `DEFER-SA-20`） | 伪引用或 sequencer 状态专项立项时 | `plan-long.md` |
| `DEFER-SA-06` | `--rerere-autoupdate`/`--no-rerere-autoupdate` 与 `--skip`/`--abort`/`--quit` 的互斥（Git `verify_opt_compatible` 的其余组合） | `#726`/`#727` 只要求 `--continue`（`t3504#7`） | 有 issue 要求或 `cherry-pick` 选项互斥专项立项时 | `plan-long.md` |
| `DEFER-SA-08` | 从 `rebase` 签名失败停止态 `--skip`；`reword`、`edit`、`amend!`、`fixup -C`、`fixup -c` 逐 action 的中途签名失败门；重放后改写路径（交互式消息编辑在该步标记完成后的二次提交、`edit` 停止后 `--continue` 吸收已暂存改动的修订提交）二次签名失败的停止态、`--continue`/`--abort` 恢复合同与专项门（需能单独命中二次签名的注入点）（`DEFER-SA-07` 已于 R35 撤销，编号不复用） | 这些 action 的重放步与 `SA-08j` 验收的多步 `pick`、`squash`/`fixup` 折叠共用同一停止与 `--continue` 路径（`--continue` 按 todo 首项 action 经 `create_replayed_commit` 构造提交）；重放后改写路径不经该路径（`src/command/rebase.rs:2067,4040-4048,4412`），其失败后的恢复语义本计划不声明；#780 只要求签名重放与 `-f`，逐 action 门与 `--skip` 不在其诉求内 | 有 issue 报告这些 action 的签名失败恢复缺陷，或 `rebase` 状态机专项立项时 | `plan-long.md` |
| `DEFER-SA-09` | `am` 暂停且尚有待处理邮件时 `libra gc` 失败：GC 把 `sequence_state.todo` 每行当 OID 严格解析（`src/command/maintenance.rs:4155-4164`），而 `am` 的 `todo` 存邮件来源标签（`src/command/am.rs:155-158`） | 与 unborn 无关的既有 GC/`am` 状态缺陷，#907 只要求 unborn 分支上的 `am`；修复需改 `maintenance.rs` 的 GC 根解析（按 `kind` 区分 `todo` 语义） | 有 issue 报告该 GC 失败，或 sequencer 状态/GC 专项立项时 | `plan-long.md` |
| `DEFER-SA-10` | `libra revert -`（Git 的 `revert`/`cherry-pick` 共用 `builtin/revert.c` 的 `parse_args`，把第一个修订参数 `-` 改写为 `@{-1}`） | #728 只要求 `cherry-pick -`（AC5/AC6），`SA-07c` 不改 `revert` | 有 issue 要求 `revert -`，或 `revert` 参数解析专项立项时 | `plan-long.md` |
| `DEFER-SA-11` | 多提交空停止（`cherry-pick <R> <V>` 在冗余 `R` 处停止）的 `--skip`/`--continue`/`--quit` 专项验收（`todo` 保留与续拣） | #725 的上游用例（`t3505#13/#14/#15`）只要求空停止提示、停止态与 `--abort`；`--skip`/`--continue`/`--quit` 沿用既有共享序列路径 | 有 issue 要求或 `cherry-pick` 序列恢复专项立项时 | `plan-long.md` |
| `DEFER-SA-12` | detached HEAD 上 revert 冲突后的 `--continue`/`--abort` 专项验收 | #728 的 TC（`TC-2634`..`2636`、`TC-0484`）不含冲突场景；该路径与分支上相同经 `restore_to_orig_head`/`update_head`，分支上由既有用例 `test_revert_conflict_then_continue`、`test_revert_conflict_then_abort` 守护（`SA-07a`、`SA-07` 共享回归组） | 有 issue 报告 detached HEAD 上 revert 冲突恢复缺陷时 | `plan-long.md` |
| `DEFER-SA-13` | 外部上游 Git 测试脚本与 TC 门：各 issue 验收项中「上游单个测试全量通过 / 对应 TC 通过」类条目——#725 的 `t3505#13`/`TC-0426`、#726 的 `t3504#7`/`TC-0429`、#727 的 `t3504#8`/`TC-0468`、#728 的 `t3501#12`/`TC-0484`、#729 的 `t3506#10`/`TC-0512`、#730 的 `t3506#11`/`TC-0513`、#731 的 `t3505#16`/`TC-0523`、#780 的 `t7510#2`/`TC-0911`、#907 的 `t4150`「am into an unborn branch」——以及逐条复现这些上游脚本（含同文件前序用例的 fixture 链）全部断言 | 按用户 2026-11-19 05:17:51 UTC 决定：各卡以同形 focused 具名用例交付各 issue 的行为验收项并只取核心断言，外部 harness（`cases/tc*/`）与上游脚本全量运行不在本计划完成门内；相关 issue 在本计划完成后为部分闭环、保持 open | 本计划收口后由 CLI / Git-compat 维护者在外部 harness 运行上述上游用例与 TC；全部通过后方可关闭对应 issue | `plan-long.md` |
| `DEFER-SA-14` | `t3508#8/#9`（`TC-0514`/`TC-0482`）：范围与 `--ff`、`-n` 组合的专项验收（原 `SA-02b`，R43 撤回）；`t3430#28`（`TC-2332`）线性范围 `E..G` 的专项验收（原 `SA-02` AC1，R44 收窄）；非线性历史（含合并、相等与交错的提交时间戳）上的范围遍历序与 Git 对照（R45 收窄） | #726 的验收项不含 `-n`/`--ff` 组合与 `t3430#28`（范围起止边界已由 `SA-02` 中途冲突用例的主题序列验证）；二者沿用既有多提交序列逻辑（`src/command/cherry_pick.rs:1102,1148,1180,1957`） | 有 issue 报告范围与 `-n`/`--ff` 组合缺陷，或 `cherry-pick` 范围专项立项时 | `plan-long.md` |
| `DEFER-SA-15` | `t3505#15`（`TC-0522`）：显式 `--empty=stop` 形成空停止态的专项验收；`t3505#17`（`TC-0524`）：`--empty=keep` 之后 `show --pretty=format:%B` 的输出（原 `SA-06` AC2，R44 收窄）；`t3505#14`（`TC-0427`）：`--empty` 与 `--continue`/`--skip`/`--abort`/`--quit` 互斥（原 `SA-01c`，R47 撤回）；`show` 的 #731 以外格式变体——`tformat:`、隐式模板（`--format=%B`）、默认 `medium`、`oneline` 与 `format:` 后接 `--stat`/补丁时的分隔——与 Git 的字节对照（原 `SA-06` AC4–AC8，R47 收窄） | #725 只涉及默认模式与 `--keep-redundant-commits`（`t3505#13`），#731 只涉及 `--empty=drop`（`t3505#16`）；`show` 的格式修复对 `keep` 生成的提交同样生效；显式模式经 `effective_empty_mode` 归入同一 `EmptyMode::Stop`（`src/command/cherry_pick.rs:597-617`），`SA-01` 只验收默认模式；R47 按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回不属 issue 验收项的部分 | 有 issue 报告显式 `--empty=stop`、`--empty` 选项互斥或 `show` 其它格式输出缺陷，或 `cherry-pick --empty`/`show` 格式专项立项时 | `plan-long.md` |
| `DEFER-SA-16` | `t3504#4–#6`（`TC-0442`/`TC-0443`/`TC-0444`）：范围上 `--rerere-autoupdate`、`rerere.autoUpdate=true`、`--no-rerere-autoupdate` 三种模式的完整流程，含跨 `--continue` 的第二个冲突（`bar`）的 rerere 恢复与暂存策略 | #726/#727 的验收项只涉及 `t3504#7`/`#8`；三种模式共用 `rerere_autoupdate_override`（`src/command/cherry_pick.rs:574-582`） | 有 issue 报告范围上 rerere 模式缺陷，或 rerere 专项立项时 | `plan-long.md` |
| `DEFER-SA-17` | unborn HEAD 上的非快进 `cherry-pick`（目标有父，或不带 `--ff`；含 `t3501#8–#9` 中 `switch --orphan` 之后的 unborn pick 与以其结果为起点的 `checkout --detach` 过渡）、其冲突暂停与 `--abort`，以及为此所需的 `CherryPickState.head_orig` 全零编码、`cherry_pick` 行的 `ORIG_HEAD` 投影与 GC 黑盒（原 `SA-05a`、`SA-05b` 与原 `SA-05` 的普通 pick/中止部分，R43 撤回）；含 Git 对 unborn 上 `cherry-pick --abort` 的处理（`cannot abort from a branch yet to be born`）与 Libra 应取语义的决策 | #730 只要求 unborn 分支上 `cherry-pick --ff` 快进到 root commit（`SA-05`），该路径不持久化 sequencer 状态；这些路径保持现状（`src/command/cherry_pick.rs:2367-2369` 的 `failed to resolve current HEAD`） | 有 issue 要求 unborn 上的普通 pick，或 `cherry-pick` unborn 专项立项时 | `plan-long.md` |
| `DEFER-SA-18` | `revert` 已暂存改动拒绝的「已暂存删除」形态专项验收；revert 冲突停止、`--continue`、`--abort` 的完整快照专项验收（初次停止的退出码与未合并条目、`--continue` 后父与树、`--abort` 后 HEAD/索引/工作区全量快照；原 `SA-07e`、`SA-07f`，R43 撤回） | #728 的残留状态来源是 detached HEAD 拒绝与已暂存改动拒绝（`SA-07`、`SA-07a`）；停止态保留与清理是既有行为，由既有用例 `test_revert_conflict_then_continue`、`test_revert_conflict_then_abort`（`tests/command/revert_test.rs:843,990`）守护；「删除」形态与「新增」「修改」共用同一索引对 HEAD 树的比较 | 有 issue 报告 revert 停止态或「删除」形态缺陷，或 `revert` 状态机专项立项时 | `plan-long.md` |
| `DEFER-SA-19` | `rebase -f` 与其它选项组合的专项验收：`t3400#31`（`TC-2289`）merge 后端形态的 `-f --onto init HEAD^` 冲突停止、`t3415#23`（`TC-2488`）`-i --force-rebase --autosquash`、`t7528#1`/`t7031#1`（`TC-0931`/`TC-2974`）`gpg.format=ssh` 下 `rebase -f HEAD^^` 的验签、`t7030#2`（`TC-2972`），以及 `rebase --help` 中 `-f`/`--force-rebase` 的选项行 | #780 只要求 `rebase -f HEAD^^` 可用且 `sixth-signed`/`seventh-signed` 为重签提交（`t7510#2`）；`-f` 与 `--onto`/`--autosquash` 的组合沿用既有路径，SSH 签名由 `SA-08g` 在出口层验收 | 有 issue 报告这些组合缺陷，或 `rebase` 选项专项立项时 | `plan-long.md` |
| `DEFER-SA-20` | 第二封暂停后用户另行提交时（`SA-09g` 统一判定的其余路径）中止拒绝的专项验收；unborn 起点 `am` 暂停后的 `--continue`/`--skip`（含首封暂停时 `--skip` 在 unborn 上的复原；首封暂停以解决结果生成 root 或跳过使下一封成为 root、后续暂停的继续与跳过；原 `SA-09a` 的两条「冲突—恢复」链）；`am` 暂停行的 `ORIG_HEAD` 投影（全零 `head_orig` 视为无 `ORIG_HEAD`、非零值作为 `ORIG_HEAD`）与真实 unborn 暂停态的 GC 黑盒（原 `SA-09c`，R43 撤回）；unborn 根提交与多封序列的作者、作者日期与消息与 Git 的逐项对照（R45 收窄；既有邮件→提交路径由 `compat_mail_am_basic` 在已有提交的分支上守护） | #907 只要求 unborn 分支上的 `am` 应用单封与多封补丁（`SA-09`）；unborn 起点暂停后的 `--abort` 复原、中止保护、中断后重试与并发保护由 `SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 交付（R47 曾撤回 `SA-09a`、`SA-09e`，R49 因 GC-07/GC-08 恢复）；`am` 暂停行的 `ORIG_HEAD` 现状以 `unknown sequence kind 'am'` 报错（`DEFER-SA-05` 同源），GC 对全零 OID 的跳过为既有逻辑（`src/command/maintenance.rs:4094-4102`） | 有 issue 要求 unborn 起点暂停后的 `--continue`/`--skip` 或 `am` 的 `ORIG_HEAD`，或 `am` 状态机专项立项时 | `plan-long.md` |
| `DEFER-SA-21` | `libra revert --reference`（`t3501#18`、`TC-2637`：消息格式、序列与 `--continue`、冲突后 `commit -a`；原 `SA-07b`）与 revert 冲突后 `GIT_EDITOR=true libra commit -a` 复用默认 revert 消息（`t7512#42`、`TC-2807`；原 `SA-07d`，需修改 `src/command/commit.rs` 的消息解析） | #728 的验收项（detached HEAD revert、已暂存改动拒绝、状态清理、`checkout -b side-branch`、`cherry-pick -s -`）不含这两个场景；R47 按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回 `SA-07b`、`SA-07d`，随之撤回只服务 `SA-07d` 的 `DEP-SA-05` | 有 issue 要求 `revert --reference` 或冲突后 `commit -a` 的默认消息，或 `revert`/`commit` 消息专项立项时；重启时按对侧当时状态重新登记 `commit.rs` 写者的等待依赖 | `plan-long.md` |
| `DEFER-SA-22` | born 分支上 `cherry-pick --ff` 快进对已暂存条目的保护：既有快进路径在未跟踪覆盖检查后直接 `reset --hard` 到目标（`src/command/cherry_pick.rs:1956-2002`），会丢弃已暂存而目标中没有或内容不同的条目（Git 的 `checkout_fast_forward` 只拒绝会被覆盖的本地改动） | 既有缺陷，所跟踪 issue 不涉及；`SA-05` 使快进在 unborn 上可达，其 unborn 情形由 `SA-05c` 以「索引非空即拒绝」保护（R49，回应 Codex R48 B-1） | 有 issue 报告 born 分支快进丢失已暂存条目，或 `cherry-pick --ff` 专项立项时 | `plan-long.md` |
| `DEFER-SA-23` | `cherry-pick -` 在上一检出导航记录读取失败（`LBR-IO-001`，`previous_checkout_read_error`）或记录损坏、目标无效（`LBR-REPO-002`，`previous_checkout_invalid_error`）时的错误透传专项验收（`src/command/switch.rs:445-465,473`） | `SA-07c` 复用 `resolve_previous_checkout_target` 并原样透传其错误；构造不可读或损坏的导航记录需要专门的故障注入，#728 不涉及（R49，回应 Codex R48 C-1） | 有 issue 报告这两类错误被折叠或误报，或 `switch -`/`cherry-pick -` 专项立项时 | `plan-long.md` |
| `DEFER-SA-24` | unborn 起点会话 `am --abort` 的判定与空树复原之间，另一进程向同一工作区或索引写入未提交改动时的保护（跨进程的工作区/索引写入互斥） | 本计划沿用既有 `--abort` 的并发模型（既有 `abort_am` 同样先判定后 `reset --hard`，`src/command/am.rs:386-411`）；引用级并发移动由 `SA-09i` 保护（R53，回应 Codex R52 F-2）；#907 不涉及 | 有 issue 报告并发写入在 `am --abort` 中丢失，或仓库级写锁专项立项时 | `plan-long.md` |

### Issue 闭环登记

按用户 2026-11-19 05:17:51 UTC 决定，本计划只交付并验收下表「本计划交付」列的验收项；「延后」列的条目由所列 `DEFER-SA-*` 承接。9 个 issue 在本计划完成后均为**部分闭环、保持 open**，不得据本计划完成关闭，直至对应 DEFER（至少 `DEFER-SA-13`）完成。R47 按协调者据用户 2026-11-19 05:17:51 UTC 决定的推论撤回 `SA-01c`、`SA-07d`、`SA-07b`（同轮撤回的 `SA-09a`、`SA-09e` 已于 R49 恢复） 与 `SA-06` 的非 #731 格式变体，其去向见下表「延后」列。

| Issue | 本计划交付（issue 验收项编号） | 承接卡 | 延后 | 本计划完成后 |
|---|---|---|---|---|
| #725 | 1（空停止提示）、2（停止态保持）、3（`--keep-redundant-commits` 与四个控制选项互斥）、4（`--abort` 成功） | `SA-01`、`SA-01b` | 5 → `DEFER-SA-13`；`t3505#15` 与 `t3505#14`（`--empty` 与控制选项互斥，原 `SA-01c`）→ `DEFER-SA-15`；多提交空停止的 `--skip`/`--continue`/`--quit` → `DEFER-SA-11` | 部分闭环，保持 open |
| #726 | 1（线性范围的解析与应用；非线性遍历序 → `DEFER-SA-14`）、2（`--rerere-autoupdate` 在范围冲突上生效）、3、4（`--continue` 与两个 rerere 标志互斥）、5（`--abort` 成功） | `SA-02`、`SA-03` | 6 → `DEFER-SA-13`；`t3508#8/#9`、`t3430#28` → `DEFER-SA-14`；`t3504#4–#6` → `DEFER-SA-16`；其余控制选项互斥 → `DEFER-SA-06`；范围语法与 `-m` → `DEFER-SA-04` | 部分闭环，保持 open |
| #727 | 1–4（重复标志被接受、范围应用、rerere 解决 `foo`、混合序列最后一个生效） | `SA-03b` | 5 → `DEFER-SA-13` | 部分闭环，保持 open |
| #728 | 1（`revert` 在 detached HEAD 上可用；`cherry-pick` 部分已由 #528 交付）、2（已暂存改动拒绝原因正确且不留状态）、3（detached 上的 revert 与拒绝之后 `switch --orphan` 成功且无 revert 停止态：`SA-07`；其后 unborn 上非快进 `cherry-pick` 与随后的 `checkout --detach` 过渡（`t3501#8–#9`）→ `DEFER-SA-17`）、4（`checkout -b side-branch` 成功：`SA-07c`）、5（`cherry-pick -s -`） | `SA-07a`、`SA-07`、`SA-07c` | 6 → `DEFER-SA-13`；`cherry-pick -` 的导航记录读取/损坏错误透传 → `DEFER-SA-23`；`TC-2807`、`TC-2637`（原 `SA-07d`、`SA-07b`）→ `DEFER-SA-21`；「已暂存删除」形态与 revert 停止态完整快照 → `DEFER-SA-18`；detached 冲突恢复 → `DEFER-SA-12`；`revert -` → `DEFER-SA-10` | 部分闭环，保持 open |
| #729 | 1、3（可以快进时快进、不新建提交：HEAD 恰为目标唯一父提交时 `SA-04` AC2，unborn 分支上快进 root commit 时 `SA-05`）；2（HEAD 不是目标父提交、无法快进时，结果 OID 等于 Git 的 `cp_ff`：`SA-04` AC1）。issue 把 `t3506#10` 的场景表述为「快进」，与上游测试（已有 HEAD 为另一 root、只能普通 pick）不符，按上游语义交付（见 `SA-04` Description） | `SA-04`、`SA-05` | 4 → `DEFER-SA-13` | 部分闭环，保持 open |
| #730 | 1–3（unborn 分支上 `cherry-pick --ff first` 可用、HEAD 为 `first`） | `SA-05`（`SA-05c` 交付 unborn 上 `--ff` 的已暂存条目保护，属安全前置） | 4 → `DEFER-SA-13`；unborn 上非快进 pick → `DEFER-SA-17`；born 分支快进的已暂存保护 → `DEFER-SA-22` | 部分闭环，保持 open |
| #731 | 1–3（`--empty=drop` 丢弃冗余提交、HEAD 不变且其消息无多余尾行） | `SA-06` | 4 → `DEFER-SA-13`；`t3505#17`（`--empty=keep`）与 #731 以外的 `show` 格式变体 → `DEFER-SA-15` | 部分闭环，保持 open |
| #780 | 1（`rebase -f HEAD^^` 可用并保持签名行为）、2（`sixth-signed`/`seventh-signed` 为重签提交） | `SA-08a`、`SA-08b`、`SA-08m`、`SA-08g`、`SA-08h`、`SA-08j`（经 `REL-SA-01`）、`SA-08` | 3 → `DEFER-SA-13`；`-f` 的其它组合 → `DEFER-SA-19`；签名失败停止态 `--skip` → `DEFER-SA-08` | 部分闭环，保持 open |
| #907 | 1、2（unborn 上 `am` exit 0、生成树等于来源的 root、不再拒绝）；3 的多封序列树序列与父链（作者、作者日期与消息的逐项对照 → `DEFER-SA-20`） | `SA-09f`、`SA-09b`、`SA-09`（经 `REL-SA-02`；`SA-09a`、`SA-09e`、`SA-09g`、`SA-09h`、`SA-09i` 交付 `SA-09` 使之可达的 unborn 起点暂停态的 `--abort` 复原、中止保护、中断后重试与并发保护，属 GC-07/GC-08 安全出口、不对应 issue 验收项） | 4 → `DEFER-SA-13`；unborn 起点暂停后的 `--continue`/`--skip` 与 `ORIG_HEAD` → `DEFER-SA-20`；`--abort` 期间并发进程的工作区写入互斥 → `DEFER-SA-24` | 部分闭环，保持 open |

## 完成判据

- [ ] 计划文本提交门：Claude（Subagent）与 Codex 对同一受审 SHA-256 均返回字面结论 `VERDICT: PASS` 且 P0/P1 全部关闭（按用户 2026-11-18 指令）；执行期各卡 ER-05 评审与网站 Claude Code `VERDICT: PASS` 另行取得，互不替代。
- [ ] 全部活动卡满足粒度规则 `G-*`：无 L/XL、无碎片卡、无未登记合并发布、写集冲突均以顺序边消解；「任务卡粒度审计表」与各卡 `Granularity` 一致；唯一的 G-03 超限为 `SA-08j` 的 `112/8@EX-SA-01`，其门族与 `EX-SA-01` 行一致。
- [ ] 全部 29 张活动卡（不含已撤回的 `SA-02b`、`SA-05a`、`SA-05b`、`SA-07h`、`SA-07e`、`SA-07f`、`SA-09c`、`SA-09d`、R47 撤回的 `SA-01c`、`SA-07d`、`SA-07b`（同轮撤回的 `SA-09a`、`SA-09e` 已于 R49 恢复） 及更早撤回的 `SA-08c`..`SA-08f`、`SA-08i`、`SA-08k`、`SA-08l`）的 acceptance criteria 已满足，且 `Lifecycle=done` 且 `Acceptance=complete`（ER-04）；无 `blocked` 遗留。
- [ ] 全部活动卡的 `Verification` 已按「验证执行协议」运行并在卡内记录（命令、exit code、nextest 汇总、被测树）。
- [ ] 每张独立发布卡已按 ER-08 完成 `patch + 1`、版本面同步、`Cargo.lock` 工具链刷新、提交、PR、合入与 `gh release create v<version>`，D 组登记表适用项全绿（D5 只适用于有网站影响的卡；无网站变化的 `SA-09f`、`SA-09b` 是家族子卡，D 组由 `REL-SA-02` 承担）；`REL-SA-01`、`REL-SA-02` 同样完成。
- [ ] 网站交付门：每张触及用户可见行为的卡（家族子卡经 `REL-SA-01`/`REL-SA-02`）已按「文档与网站验收命令」完成同步，并且要么完成 `pnpm typecheck`、`pnpm build`、独立 Claude Code `VERDICT: PASS`、签名+DCO 提交、`cf` 推送与 D5 部署核验，要么按第 3c 步记录「网站无漂移」并完成替代 D5 核对；任一环节失败即 STOP，不得标 `complete`。
- [ ] **计划完成门 / 全量收口门（ER-13）**：在全部 29 张活动卡完成后的最终代码提交 `CODE_SHA`（最后一个改动 `docs/development/plan/` 以外路径的提交）的树上运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，全部通过；记录 `CODE_SHA` 与 `libra rev-parse "$CODE_SHA^{tree}"`；代码树一旦变化即重跑。
- [ ] **最终代码树与记账提交（预登记，模板 v2.12/v2.14）：** 收口后只改动 `docs/development/plan/` 下文件的记账提交不触发重跑，判定为先执行 `OUT="$(libra diff --name-only "$CODE_SHA" HEAD)"` 且其 exit 0（非 0 即 STOP，不得当作无改动），再确认 `$OUT` 的每一行都以 `docs/development/plan/` 开头；终止链末端记账提交的 D 组结果按模板 v2.14 由下一次 `plan-status.md` 例行同步补记（本条即该协议的预登记）。
- [ ] **全量收口门暴露的 Bug 已全部处理（ER-13）**：本计划引起的失败已前滚修复并重跑全量至全绿；判定为既有失败的用例附「计划基线提交上同样失败」的复现证据并登记 `FIX-*` 或 `DEFER-*`。
- [ ] 必要的 docs/compat/error-code/test-index 更新已完成（「兼容与文档收口」全部勾选）。
- [ ] 每张活动卡的 `Rollback mode` 与 `Migration and rollback` 已实际验证或记录不可验证原因（含 unborn 活动态降级前置）。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md` 已登记 `DEFER-SA-01`..`DEFER-SA-06`、`DEFER-SA-08`..`DEFER-SA-24` 与日期计划索引（执行期义务，按模板「完成判据」由收口执行者完成）。
- [ ] **Issue 闭环（部分闭环）：** 按「Issue 闭环登记」，9 个 issue 在本计划完成时只关闭其「本计划交付」列的验收项，均保持 open 并在 issue 中同步进展与延后项（`DEFER-SA-13` 等）；不得据本计划完成关闭任何一个。
