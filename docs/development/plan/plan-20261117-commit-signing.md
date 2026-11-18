# `libra commit` 语义对齐、签名与 Hook 完善计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Claude R58 与 Codex R58 双审 PASS（受审 SHA-256 前 12 位 `fdc9dc310d3e`），计划可提交；记录时间 2026-11-18 16:12:51 UTC；全部任务 `pending`。本文只规划任务，不宣称实现完成。（批处理例外：按协调者指令，20261117 批次起草阶段暂不修改 `plan-status.md`，由协调者统一登记合入，且在任一任务卡开工前完成登记。**计划文本提交门：**按用户 2026-11-18 指令，计划文本须由 Claude（Subagent）与 Codex 双审，二者对同一字节均给出 `VERDICT: PASS` 方可提交；该门只约束本计划文本的提交，不替代执行期各卡 ER-05 评审门与网站交付的 Claude Code PASS 门。）
> 跟踪范围：36 个 open issues（#757, #759, #760, #761, #762, #763, #764, #765, #766, #767, #768, #769, #770, #771, #772, #773, #774, #776, #777, #778, #779, #781, #783, #784, #786, #787, #789, #790, #791, #792, #793, #794, #982, #1005, #1006, #1032）。

## 文档职责

- **网站交付路径（已确认）:** 网站文档交付检出为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支；`.libra` 仓库，无 `.git`），依据为用户 2026-11-18 11:55:09 UTC 的确认。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`（本机不存在），属已知文档滞后，用户选择不在本批改动 `AGENTS.md`，本计划不修改该文件。每次写入前按 W-1 核验该路径、VCS 类型与 `cf` 分支（非 `cf` 时按 ER-06a 执行 `libra switch cf` 后重查，切换受阻即 STOP），路径或 VCS 类型不符即 STOP，禁止改用其它检出。另：`AGENTS.md:130` 关于「默认由仓库 vault 密钥签名」的表述在 `CS-02` 发布后将滞后；同样按用户指示本计划不改 `AGENTS.md`，登记为跨计划协调事项，由协调者与用户决定其同步时机。

本文解决 `libra commit`（及紧密关联的 `commit-tree` / `cat-file` / `maintenance` 接入点）在 CLI 参数解析、提交签名默认值与外部 GPG/SSH 程序、部分提交（pathspec / `--only` / `--include` / `--patch` / `--pathspec-from-file`）、提交消息清理与 UTF-8 校验、日期与作者元数据、amend 行为、对象序列化、hooks、嵌套操作锁以及 trace2/自动维护方面的 36 个已知 Git 兼容性与行为缺口，目标是分 83 张活动任务卡（73 张 `implementation` 卡 + 10 张 G-08 家族发布点 `REL-CS-00`..`REL-CS-09`；编号 `CS-26` 已废弃，见「任务卡」节首）完成实现、测试、文档与发布闭环。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收。

### 适用范围

- `src/command/commit.rs`、`src/command/commit_tree.rs`、`src/command/cat_file.rs`、`src/command/history_config.rs`（只核对）、`src/command/init.rs`、`src/command/clone.rs`、`src/command/merge.rs`（帮助文本）、`src/command/config.rs`（`resolve_gpg_program`）、`src/command/notes.rs`、`src/command/maintenance.rs`、`src/command/add.rs`、`src/internal/vault.rs`、`src/utils/approxidate.rs`、`src/utils/pathspec/mod.rs`、`src/utils/trace2.rs`（新增）、`src/utils/mod.rs`、`src/utils/test.rs`、`src/cli.rs`、`src/internal/operation/middleware.rs`、`src/internal/operation/middleware/lease.rs`。
- 外部对照基线：pinned Git 上游源码 `/Volumes/Data/gitmono/git` @ `6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（核对日 2026-11-18；以 `git -C /Volumes/Data/gitmono/git show 6de20f6092:<path>` 读取，不以该检出的浮动工作树为准；主要文件 `builtin/commit.c`、`run-command.c`、`builtin/gc.c`、`builtin/cat-file.c`、`builtin/commit-tree.c`、`t/t7900-maintenance.sh`）。
- 对应集成/兼容测试：`command_test`（入口 `tests/command_test.rs` 的 `mod command`，模块 `tests/command/*.rs`）、`compat_commit_amend_no_edit`、`compat_commit_identity_date`、`compat_config_defaults_semantics`、`compat_config_history_defaults`、`compat_declined_interactive_flags`、`compat_libra_hooks_lifecycle`。
- 对应中英文命令文档、开发文档、兼容矩阵与测试索引：`docs/commands/<cmd>.md`、`docs/commands/zh-CN/<cmd>.md`、`docs/development/commands/<cmd>.md`（`<cmd>` ∈ commit、init、clone、merge、cherry-pick、commit-tree、cat-file、maintenance、log、reflog、rev-list、shortlog、archive）、`docs/development/commands/_compatibility.md`、`COMPATIBILITY.md`、`tests/INDEX.md`、`CLAUDE.md`、`.env.test.example` 及网站文档 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`（网站只存 EN，zh-CN 由站点渲染时镜像）。

### 非目标

- 不在本计划中实现 `libra bisect visualize -p -- <path>`（`Bisect::View` 无参数，见 `DEFER-CS-02`）或 `libra log/show --show-signature`（见 `DEFER-CS-03`）。
- 不在本计划中修改 SQLite schema、新增迁移或新增 config 存储结构（不涉及 `sql/**`）；`CS-01a` 的 keyid 选择只在仓库现有签名密钥之间选择，不新增多密钥存储。
- 不在本计划中执行 `.git/hooks` 或 `core.hooksPath`（遵循 PD-08 `.libra/hooks` 沙箱安全边界；见 `DEFER-CS-01`）。

### 成功定义

- 上述 36 个 open issues 中，除明确边界登记项外均在对应任务卡完成实现、核对与 Git 兼容测试验证；其中 `#757` 的 commit 部分提交由 `REL-CS-03`（`CS-15`、`CS-15h`、`CS-15d`、`CS-15f`）完成、下游 `bisect visualize -p -- <path>` 延后至 `DEFER-CS-02`（故 `#757` 在 `DEFER-CS-02` 落地前保持部分完成，不关闭）；`#783` 的 `gpg.program` 解析、签名、验签与 merge 信任门由 `CS-03f`、`CS-03a`、`CS-03j`、`CS-03d`、`CS-03g`、`CS-03l` 完成、下游 `show --show-signature` 延后至 `DEFER-CS-03`（故 `#783` 在 `DEFER-CS-03` 落地前保持部分完成，不关闭）；`#767`、`#778` 因遵循 PD-08（仅执行 `.libra/hooks`，不执行 `.git/hooks`）按 `intentionally-different` 收口，由 `CS-22a` 在 `COMPATIBILITY.md` hooks 行登记（原 `CS-22` 已撤回：`--no-verify` 跳过全部 hooks 是 Libra 有意的完整逃生阀，对齐 Git 属 `DEFER-CS-09`）。
- 全部非延后任务卡达到 `Lifecycle=done` 且 `Acceptance=complete`，计划收口全量门（`fmt` + `clippy` + 双环境 `nextest`）在最终树上全绿。

## 使用规则

- 每次状态推进、快照更新与「修订历史」新增行都必须记录 `YYYY-MM-DD HH:MM:SS UTC` 实际时间戳，不得预填或写未来时间。
- 推进或完成任一任务卡时，须在同一变更中同步更新 [plan-status.md](plan-status.md)（起草阶段不提前修改 `plan-status.md`，由协调者在首卡开工前登记）。
- 后端网站文档交付一律按本计划「网站交付协议（W-1..W-6）」执行，该协议是 ER-06a 与后端 `AGENTS.md`「Libra command documentation delivery」规定流程（sync 脚本 `--write --only` / 复查、`pnpm typecheck`、`pnpm build`、Claude Code PASS、在 `cf` 上签名提交并推送、核验 Cloudflare 部署）的逐步落地，不另设流程。
- 本计划所有测试摘要、门结果、哈希、快照比较结论与评审结论直接写入本计划文件（对应卡 `Current evidence` 或「Codex review log」），不新建任何证据文件或目录，包括仓库外临时目录（v2.12）。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。过期锚点只能作为历史线索。下表锚点于 2026-11-18 按当前工作树核对。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `CommitArgs`（`#[derive(Parser)]`）定义 CLI 选项；`run_commit` / `execute_safe` 为主执行入口 | `src/command/commit.rs:92-279,1028-1098,3003-3047` |
| 签名策略与默认值 | `libra init` 默认写 `vault.signing=true`；`commit_signing_policy` 读 `commit.gpgSign` 后回退 `InheritVault`；`vault_sign_commit` 在非强制时仅当 `vault.signing=true` 才签名；`pgp_sign` 按 `vault.gpg.source` 在生成密钥与导入密钥之间二选一 | `src/command/init.rs:2495-2508`; `src/command/history_config.rs:71-82`; `src/command/commit.rs:3088-3151`; `src/internal/vault.rs:417-427,507-527` |
| CLI 参数解析 | `gpg_sign: Option<Option<String>>` 带 `require_equals = true` 拒绝贴合 `-S<keyid>`，帮助称 KEYID 被忽略；`-m` 为 `Option<String>`，`fixup`/`squash` 与 `message`/`file` 互斥；`--fixup` 未解析 `amend:`/`reword:`；缺少 `--long`、`--interactive`、`--include`、`--patch` | `src/command/commit.rs:97-98,169-181,239-243,1845-1854` |
| 部分提交与 Pathspec | `execute_safe` 以 `StableErrorCode::Unsupported` 拒绝 `--only`、`--pathspec-from-file` 与位置 `paths`；`resolve_commit_template` 未处理 `:(optional)` 前缀 | `src/command/commit.rs:254-267,2016-2049,3008-3016` |
| 消息模板与清理 | `build_verbose_template`、`truncate_at_scissors`、`cleanup_commit_message` 已有基础；`-F` 用 `read_to_string` 拒绝非法 UTF-8；`--signoff` 未去重；`-t` 在 `verbatim` 下仍做未编辑中止 | `src/command/commit.rs:1367-1374,1864-1869,1990-2002,2056-2094,2222-2276` |
| 日期、作者与对象格式 | `date_parser.rs` 未复用 `src/utils/approxidate.rs` 且不支持无时区 ISO；`render_commit_output` 无 `Author:`/`Date:`；`--amend --author` 重置日期；`commit_tree.rs` 单源消息强制补尾随 `\n`；`cat_file.rs` 在 `--batch`/`--batch-check` 与 `--batch-command` 中跳过空行 | `src/internal/log/date_parser.rs:14-66`; `src/command/commit.rs:1397-1405,2903-2964`; `src/command/commit_tree.rs:116-123`; `src/command/cat_file.rs:715-717,906-908` |
| Hooks、锁与 Trace2 | `prepare-commit-msg`/`commit-msg` 已接入 `.libra/hooks`，但 `skip_all_hooks = args.no_verify` 同时跳过 `prepare-commit-msg`（Libra 有意把 `--no-verify` 作为跳过全部 hooks 的逃生阀，见 `DEFER-CS-09`）；operation-v2 `ScopeLease` 阻塞编辑器内嵌套 commit；`DECLINED_INTERACTIVE_FLAGS` 提前拒绝 `commit -p/--patch/--interactive`；`src/` 无 `GIT_TRACE2` 实现；CLI 分发期间持有 `MaintenanceLock::shared`；`maintenance.rs` 有阈值常量但 `run` 无 `--auto`/`--detach` | `src/command/commit.rs:1115,1915-1937`; `src/internal/operation/middleware.rs:22,47-48,125-154,411-416,457,714`; `src/internal/operation/middleware/lease.rs:83`; `src/cli.rs:3005,3017-3027,3768-3770`; `src/command/maintenance.rs:63-64` |
| 用户与开发文档 | 上述命令均有 EN/zh-CN/development 文档；网站 `content/docs/commands/` 只存 `<cmd>.en.md` | `docs/commands/commit.md`; `docs/development/commands/commit.md`; `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md` |
| 测试 | `command_test` 单一二进制（`tests/command_test.rs` → `mod command`），用例全名为 `command::<模块>::<fn>`；compat target 根级用例全名为 `<fn>` | `tests/command_test.rs:10`; `tests/command/mod.rs`; `Cargo.toml:529-611` |
| 外部参照 | pinned Git `6de20f6092`：`builtin/commit.c:371-390`（pathspec 诊断）、`:1296-1306`（reword 冲突）、`:1356-1357`（`--reset-author`）、`:1931-1932`（`exclude_gpgsig`）、`:1965`（`run_auto_maintenance`）；`run-command.c:1956-1993`（`prepare_auto_maintenance`）；`t/t7900-maintenance.sh:66-102` | `git -C /Volumes/Data/gitmono/git show 6de20f6092:<path>`（核对日 2026-11-18） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-01 | `-S<keyid>` 贴合解析、重复 `-m`（含 `--fixup -m`）、`--squash -F`、`--fixup=amend:/reword:`、`--long`、`--interactive` 缺失或冲突 | 与 Git CLI 语义冲突 | `src/command/commit.rs:97-98,169-181,239-243` | `CS-01`, `CS-01a`, `CS-05`, `CS-06`, `CS-07`, `CS-07a`, `CS-07b`, `CS-08`, `CS-09`, `CS-09b`, `CS-09a`, `CS-09c` |
| GAP-02 | 默认签名策略、`gpg.format` 校验、`gpg.program`、SSH 签名与 `--amend` `gpgsig` 语义缺失 | 默认行为与 Git 相反，无法用 SSH/GPG 外部签名 | `src/command/init.rs:2495-2508`; `src/command/commit.rs:1498-1530,3088-3151` | `CS-02`, `CS-02a`, `CS-03`, `CS-03f`, `CS-03a`, `CS-03j`, `CS-03d`, `CS-03g`, `CS-03l`, `CS-03b`, `CS-03c`, `CS-03h`, `CS-03i`, `CS-03k`, `CS-03e`, `CS-04` |
| GAP-03 | 部分提交（`<path>`/`--only`/`--include`/`--patch`/`--pathspec-from-file`）与 `--template :(optional)` 缺失 | 无法部分提交或使用可选模板 | `src/command/commit.rs:2016-2049,3008-3016` | `CS-15`, `CS-15h`, `CS-15d`, `CS-15f`, `CS-15a`, `CS-15b`, `CS-15c`, `CS-15g`, `CS-15e`, `CS-16` |
| GAP-04 | `-v` 尾行、`-F` 非法 UTF-8/non-characters、`--signoff` 去重与空消息校验、`-t` `verbatim` 中止反转、`whitespace`/`scissors` 缺失 | 破坏提交消息格式与非交互场景 | `src/command/commit.rs:1864-1869,1990-2002,2056-2094,2222-2276` | `CS-10`, `CS-11`, `CS-12`, `CS-12a`, `CS-13`, `CS-14`, `CS-14a` |
| GAP-05 | approxidate/无时区 ISO、`--amend --author` 日期、`--reset-author` 约束、`Author:`/`Date:` 输出、`notes.rewrite.amend`、amend 索引残留、`commit-tree`/`cat-file` 字节偏差 | 破坏元数据、notes 与对象大小精确度 | `src/internal/log/date_parser.rs:14-66`; `src/command/commit_tree.rs:116-123`; `src/command/cat_file.rs:715-717` | `CS-17`, `CS-17a`, `CS-18`, `CS-18a`, `CS-18c`, `CS-18b`, `CS-19`, `CS-19c`, `CS-19b`, `CS-19a`, `CS-20`, `CS-21`, `CS-21a` |
| GAP-06 | `prepare-commit-msg` 参数来源偏差、编辑器嵌套 commit 锁阻塞、`trace2` 事件缺失、`maintenance run --auto` 与 commit 自动维护缺失 | 无法嵌套提交、无 trace2 与自动维护 | `src/command/commit.rs:1115,1915-1937`; `src/internal/operation/middleware.rs:411-416`; `src/cli.rs:3768-3770` | `CS-22a`, `CS-23`, `CS-23a`, `CS-23b`, `CS-24`, `CS-25`, `CS-25g`, `CS-25d`, `CS-25c`, `CS-25h`, `CS-25f`, `CS-25a`, `CS-25i`, `CS-25e`, `CS-25b` |

## 与其它计划的关系

跨计划关系只按对方计划的任务卡 ID 引用；对方计划可能正被并行修订，开工前以对方计划当时文本与 `plan-status.md` 为准。

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| [plan-20261117-merge-pull.md](plan-20261117-merge-pull.md) | `MP-26` 修改 `src/command/history_config.rs`（与 `CS-02a` 相交）；`MP-10` 修改 `src/command/commit.rs`；`MP-05` 负责 `SQUASH_MSG`；`MP-08` 消费 `REL-CS-00`（对方登记 `DEP-MP-02`） | incoming `DEP-CS-01`（`MP-05` → `CS-14a`）、`DEP-CS-02`（`MP-26` → `CS-02a`；`MP-10` → `CS-03`）；outgoing `DEP-CS-10`（`REL-CS-00` → `MP-08`） |
| [plan-20261117-sequencer-am.md](plan-20261117-sequencer-am.md) | 消费 `REL-CS-00`（对方 `DEP-SA-01`）与 `REL-CS-01`（对方 `DEP-SA-02`，其 `SA-08g` 依赖本计划 SSH 签名/验签）；对方 `SA-07d` 与本计划 `CS-25a`、`CS-25e` 共享 `src/command/commit.rs`（对方 `DEP-SA-05`） | outgoing `DEP-CS-11`（`REL-CS-00`）、`DEP-CS-12`（`REL-CS-01`，ID 与语义保持向后可引用）、`DEP-CS-13`（`REL-CS-06` 含 `CS-25a`、`CS-25e`，先于 `SA-07d`） |
| [plan-20261117-clone.md](plan-20261117-clone.md) | `clone.rs`、`clone_cli_test.rs`、`cat_file.rs` 等写集与 `CS-02`/`CS-02a`/`CS-15b`/`CS-21a` 相交（对方 `DEP-CL-03`） | incoming `DEP-CS-03`，逐字镜像对方 `DEP-CL-03` 的卡集合 |
| [plan-20261117-plumbing-misc.md](plan-20261117-plumbing-misc.md) | `PM-02`（`config.rs` 与 config 文档）、`PM-09`（`notes.rs`）、`PM-10`（`src/cli.rs`、`tests/command/mod.rs`）、`PM-14`（`cat_file.rs`）与本计划相交（对方 `DEP-PM-07`） | incoming `DEP-CS-04`；`PM-02` 的计划内传递前置（经对方 `DEP-PM-05` 等待 `issues/1048.md#I1048-05`）登记为 `DEP-CS-09` |
| [plan-20261117-checkout-switch-restore.md](plan-20261117-checkout-switch-restore.md) | `CSR-01`/`CSR-02`/`CSR-27` 修改 `src/cli.rs` | incoming `DEP-CS-05` |
| [plan-20261117-diff.md](plan-20261117-diff.md) | `DIFF-45` 修改 `src/utils/pathspec/mod.rs` | incoming `DEP-CS-08` |
| [plan-20261117-global-init-infra.md](plan-20261117-global-init-infra.md) | `GII-01..09`、`GII-15/16/19/20/22/24` 修改 `src/cli.rs`、`init.rs`、`tests/command/init_test.rs`、`init.md`、`config.md` 与 `add.rs`（对方 2026-11-18 已推送版本 `f3316d9`；对方 `DEP-GII-03` 为同批写集窗口） | incoming `DEP-CS-06`（顺序前置）+ `DEP-CS-07`（动态写集窗口） |
| [plan-20261117-worktree-clean-reset-apply.md](plan-20261117-worktree-clean-reset-apply.md) | 其 `DEP-WC-07` 以 2026-11-18 快照把本计划 `CS-09`、`CS-15b`、`CS-24`、`CS-26` 列为 `src/cli.rs` 写者；`CS-26` 已废弃，其 `src/cli.rs` 写入并入 `CS-25a` | 本计划不改写对方文件；该滞后登记为跨计划协调事项，由 Coordinator 通知对方把 `CS-26` 更正为 `CS-25a`；在对方更正前，`DEP-CS-07` 的动态写集判定以本计划当前写集为准 |
| 全部 15 份 `plan-20261117-*.md` | 共享写入 `COMPATIBILITY.md` 及其它可能相交的文件（含网站 `.en.md`） | `DEP-CS-07` 跨计划写集串行窗口（Coordinator 动态判定，覆盖全部实现卡） |
| `COMPATIBILITY.md` | Git 兼容矩阵事实源 | 公开行为变化与有意差异同卡同步，只在既有条目行内修改（追加新陈述或就地替换被推翻的旧陈述）、不增删行（compat ledger 以 `COMPATIBILITY.md:<line>` 绝对行号锚定，`tests/compat-ledger/SURFACES.gen:119`） |
| `AGENTS.md` | `:135` 网站路径滞后（见「文档职责」） | 不修改；按用户 2026-11-18 11:55:09 UTC 确认的路径执行 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 36 个 issue 均有源码或 Git 对照证据，拆为 73 张单轴实现卡 | R31/R32 按单谓词计数拆出 `CS-01a`、`CS-03d`/`03e`/`03f`/`03g`、`CS-07a`/`07b`、`CS-15f`/`15g`、`CS-22a`、`CS-23b`，新增家族 `REL-CS-06` |
| 可行性 | 每卡可由单 Agent 独立执行；跨计划相交由 `DEP-CS-07` 动态窗口与 `DEP-CS-01..06,08,09` 顺序前置保护 | R30 起把逐卡 AC 改为单谓词并直接绑定具名用例 |
| 任务卡粒度 | 无 L/XL；AC/VER 均按独立谓词计数且在 G-03 上限内；10 个家族各有唯一 `release` 发布点 | 删除重复卡 `CS-26`（编号废弃保留）；不设 tests/docs-only 卡（G-02） |
| 依赖与顺序 | 「实施顺序」为单链 DAG，各卡 `Dependencies` 与链上前驱一致 | R30 起移除链外冗余边（`CS-25a` 不再直接依赖 `CS-24`） |
| 完整性 | 每卡含测试、EN/zh/dev/网站文档与 `COMPATIBILITY.md` 判定；性能、故障恢复与回滚已登记 | R30 起公开行为卡统一纳入 `COMPATIBILITY.md` 与 `DEP-CS-07` |
| 安全性 | 签名 fail-closed、SSH 临时文件清理、重入令牌受限、hooks 遵循 PD-08 | 无 |
| 功能正确性 | 关键 Git 语义以 pinned `6de20f6092` 源码行对照（含 `prepare_auto_maintenance` 的 `gc.auto` 回退与 `--[no-]quiet`/`--[no-]detach` 参数） | R30 起修正 `CS-18b` 文案句点、`CS-25`/`CS-25a` 参数与启用条件 |
| 接口兼容 | 错误码沿用既有 `LBR-*`，不新增；`--json` 输出结构保持 | 无 |
| 数据流与控制流 | 部分提交用隔离索引原子提交；amend notes 复制两阶段 + 持锁基线比较；自动维护在共享锁释放后派生 | R30 起 `ADR-CS-03` 改为 dispatch 后统一派生 |
| 性能与容量 | 部分提交 `O(index)`；trace2 未启用零 I/O；自动维护不阻塞提交热路径 | 性能断言收敛到 `CS-15` 的双规模增长比用例 |
| 可靠性与容错 | 失败/中止路径以全量快照（SNAP）判定无残留 | R30 起「保留/不修改/回滚」类 AC 一律映射 SNAP |
| 可维护性 | 单一事实源：签名策略 `history_config.rs`、日期 `approxidate.rs`、pathspec `pathspec/mod.rs` | 无 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 02:55:00 UTC | 初稿创建 | 基于 `agent_2_tasks.json` 起草 24 张任务卡覆盖 36 个 issues | `N/A` | 全文 |
| 2026-11-17 03:05:00 UTC | Claude R1 FAIL + Codex R1 FAIL | 拆出 `CS-03a`/`03b`/`18a`/`25`；修正 `CS-04`/`05`/`07`/`09`/`10`/`11`/`13`/`14`/`15`/`16`/`21`/`22` | `CS-03`→`CS-03,03a,03b`; `CS-18`→`CS-18,18a`; `CS-24`→`CS-24,25` | 实施顺序、粒度表、追溯表 |
| 2026-11-17 03:15:00 UTC | Claude R2 FAIL + Codex R2 FAIL | 拆出 `CS-14a`/`CS-15a`/`15b`/`15c`/`18b`/`21a`；增补 `DEP-CS-01`/`02` 与 `DEFER-CS-02`/`03`；修正 `CS-17` `approxidate.rs` 复用 | `CS-14`→`CS-14,14a`; `CS-15`→`CS-15,15a,15b,15c`; `CS-18`→`CS-18,18b`; `CS-21`→`CS-21,21a` | 实施顺序、粒度表、追溯表、延后项 |
| 2026-11-17 03:25:00 UTC | Codex R3 FAIL | 消除 `DEP-CS-02` 死锁（`CS-01`→`CS-02` 先行，再 `MP-08`→`MP-10`，再 `CS-03`）；对齐 `DEP-CS-01` 与 `MP-05`（仅 `SQUASH_MSG`）；`CS-10` 补齐 ER-06a 文档写集 | `N/A` | 依赖表、`CS-01`、`CS-03`、`CS-10`、`CS-14a` |
| 2026-11-17 03:32:00 UTC | Codex R4 FAIL | 明确 `../libra-backend` 路径开工核实规则；补充 `DEP-CS-02` 单边阻塞门说明 | `N/A` | 使用规则、依赖表 |
| 2026-11-17 03:45:14 UTC | Claude R5 FAIL + Codex R5 FAIL | 登记跨计划写集串行门 `DEP-CS-03/04/05` 及 outgoing `DEP-MP-02`/`DEP-SA-01`/`DEP-SA-02`；修订 `ADR-CS-03`；重锚 `CS-21a`；修正 Phase 0、`#757`/`#770`/`#767`/`#778`、dev 文档与审计表 | `N/A` | 关系表、依赖表、`ADR-CS-01`/`03`、Phase 0、审计表、多张卡、`DEFER-CS-02` |
| 2026-11-17 03:56:00 UTC | Claude R6 FAIL + Codex R6 FAIL | 将 `DEP-CS-03/04/05` 改为严格 start-to-completion 并新增 `DEP-CS-06`（`global-init-infra`）与 `DEP-CS-07`（全部 15 计划 `COMPATIBILITY.md`）；`CS-15b`/`CS-09` 补齐 `cli.rs` `DECLINED_INTERACTIVE_FLAGS` 等写集并将 `--pathspec-from-file`+`--interactive` 冲突迁入 `CS-09`；补充 `#783` 部分闭环口径；新增 `ADR-CS-04` 并重锚 `CS-23` 到 `middleware.rs`/`lease.rs`；`ADR-CS-03`/`CS-25` 明确 `--auto` 阈值与测试隔离，同步修正风险表；修正 `CS-24` AC2/`CLAUDE.md` 与文档收口清单 | `N/A` | 关系表、依赖表、`ADR-CS-03`/`04`、审计表、`CS-03a/09/15b/15c/23/24/25`、风险表、文档清单、日志 |
| 2026-11-17 04:05:00 UTC | Claude R7 FAIL + Codex R7 FAIL | 逐卡补齐 `DEP-CS-03/04/05/06/07` 引用并将 `DEP-CS-03/06` 设为 `incoming` 前置于 `CS-02`（含 `CL-19` `cat_file.rs`、`GII-01` `init_test.rs`、`GII-05` `config.md`、`CL-01`/`PM-10` `cli.rs`）；修正 `ADR-CS-03` 将阈值检查放回 `--auto` 子进程以通过 `t7900#3` 并在 `CS-25` 写集补 `.env.test.example` 与 `CLAUDE.md`；重锚 `CS-23` 到 `middleware/lease.rs:83` 并接入 `OPERATION_SCOPE_LEASE_HELD_ENV`；修正 `CS-15c` AC≤8 与 `#786`/`#789` 精确文案及 `CS-19` `middleware.rs` 租约 | `N/A` | 关系表、依赖表、`ADR-CS-03`/`04`、审计表、`CS-02/03/03a/03b/04/07/09/11/14a/15/15b/15c/19/21/21a/22/23/24/25`、日志 |
| 2026-11-17 04:15:00 UTC | Codex R8 FAIL | 收紧 `ADR-CS-04` 与 `CS-23` 为受限单次重入令牌与生命周期隔离；按 `AGENTS.md` 网站写集改为 `/Volumes/Data/gitmono/libra-backend` | `N/A` | 使用规则、适用范围、`ADR-CS-04`、`CS-23`、各卡网站写集、日志 |
| 2026-11-17 04:50:00 UTC | Claude R9 FAIL + Codex R9 FAIL | `CS-23`/`ADR-CS-04` 原子单次认领与带界等待；`CS-19` 改局部租约；`#786` 文案与 `pathspec/mod.rs` + `DEP-CS-08`；超 8 AC 拆分为 41 卡 | `CS-02`→`02,02a`; `03b`→`03b,03c`; `12`→`12,12a`; `15`→`15,15d`; `15c`→`15c,15e`; `09`→`09,09a`; `25`→`25,25a` | 全文 |
| 2026-11-17 05:00:00 UTC | Codex R10 FAIL | 将 `CS-03b`+`CS-03c` 纳为 `REL-CS-01` 家族发布（42 卡），对齐 `DEP-SA-02` | 新增 `REL-CS-01` | 职责、实施顺序、依赖表、审计表、`CS-03b/03c/REL-CS-01/04`、追溯表、日志 |
| 2026-11-17 05:25:00 UTC | Claude R11 FAIL + Codex R11 FAIL | 将 `CS-02/02a` 纳为 `REL-CS-00`、`CS-09/09a` 纳为 `REL-CS-02`（共 44 卡），登记 `发布分组` 表并规范 `family child` `Rollback mode: revert`/`Version increment: N/A`；`DEP-CS-02` 补 `MP-26`、`DEP-SA-08` 补 `CS-25`→`SA-05`、`DEP-MP-02`/`DEP-SA-01` 对齐 `REL-CS-00`；`CS-03` 改 `LBR-CLI-003`；`ADR-CS-03`/`CS-25a` 移除 `.cargo/config.toml` 改用测试进程环境变量/harness 守卫并遵循 `maintenance.autoDetach`；修正 `CS-15e` 例外、`CS-19` `middleware.rs:389-398` 锁序、`CS-17` `T-1` 与文档、`CS-23` 文档、`CS-02` `prod-files=3`、`REL-CS-*` 去除全局门与 `CS-13`/`24` 验证精确过滤、pinned Git `6de20f6092` | 新增 `REL-CS-00`, `REL-CS-02` | 全文 |
| 2026-11-17 06:00:00 UTC | Codex R12/R13 FAIL | `DEP-MP-02` 明确 `REL-CS-00` 联合交付与 Coordinator 批处理同步边界；`ADR-CS-04`/`CS-23` 明确有界等待超时后终止并确认子进程退出再释放 `ScopeLease` 与超时并发写入测试；`ADR-CS-03`/`CS-25a` 明确专项子进程测试同时移除 `NEXTEST_RUN_ID` 与 `LIBRA_NO_AUTO_MAINTENANCE` | `N/A` | 头部、`ADR-CS-03`/`04`、依赖表、`CS-23`、`CS-25a`、日志 |
| 2026-11-17 07:05:00 UTC | Codex R15 FAIL | `CS-02` 收回 `init_test`/`init_json_test` 夹具以保证家族子卡预发布门控可过；将 `CS-15`/`CS-15d` 纳为 `REL-CS-03`、`CS-15c`/`CS-15e` 纳为 `REL-CS-04`；将 `CS-23` 拆为 `CS-23`+`CS-23a`（均 `<=6` 原子谓词）并纳为 `REL-CS-05`；`CS-17` 补全 `log` 后端文档与 `prod-files=3`；`CS-25a` 写集补入 `tests/command/mod.rs` 与 `src/utils/test.rs` 并覆盖 `cargo test`/`nextest` 双路径；更新 `DEP-SA-08` 与 `M1`–`M3` 家族边界 | 新增 `REL-CS-03`, `REL-CS-04`, `CS-23a`, `REL-CS-05` | 全文 |
| 2026-11-17 12:01:37 UTC | Clone Codex R30 跨计划一致性修复 | `DEP-CS-03` 与 `DEP-CL-03` 统一为仅活动卡的显式集合，纳入 `CL-25` 家族发布点并排除 `CL-11`/`CL-22`/`CL-20`；按用户指令将计划提交门改为 Codex 单审 | N/A | 关系表、依赖表、M0、完成判据 |
| 2026-11-17 12:27:28 UTC | 机械自审：审计表存在但缺少 `CS-23a` 正文 | 新增 `CS-23a` 承接有界等待、超时终止与状态/op-log 隔离；`CS-23` 仅保留单次重入令牌及守卫，二者均改为 `REL-CS-05` 家族子卡并同步 `CS-24` 依赖 | `CS-23` → `CS-23,CS-23a` | `CS-23`、`CS-23a`、`CS-24`、审计表、实施顺序、Review log |
| 2026-11-17 12:27:28 UTC | 机械自审：`REL-CS-05` 有登记但缺正文 | 补齐家族发布点卡片、双环境门及两张子卡验证 | N/A | `REL-CS-05`、修订历史 |
| 2026-11-17 12:27:28 UTC | 机械自审：家族子卡计数 | 拆分 `CS-23` 验收为 7 条独立判据；修正 `CS-23a` 排除强制门后的计数为 4 | N/A | `CS-23`、`CS-23a`、粒度审计表 |
| 2026-11-17 12:27:28 UTC | Codex R18-A FAIL（部分审查） | 明确家族网站部署顺序、令牌认领与撤销原子竞态、超时窗口并发写保护测试，并补齐追溯表 | N/A | 使用规则、ADR-CS-04、CS-23/23a、追溯表 |
| 2026-11-17 12:27:28 UTC | Codex R19-A FAIL/P1 | 补外层崩溃时的写保护、进程身份和终止失败合同及测试；排除依赖 CL-20 的 CL-17 并镜像 Clone；最终网站字节绑定复审；逐过滤器验证非零 | N/A | 依赖表、使用规则、ADR-CS-04、CS-02/23a、REL-CS-00 |
| 2026-11-17 12:27:28 UTC | Codex R20-A FAIL（部分检查） | CS-02a 的三个过滤器补逐项非零及测试计数；该轮输出截断，不作为完整复审 | N/A | CS-02a、修订历史 |
| 2026-11-17 12:27:28 UTC | Codex R21-A1 FAIL/P1 | `DEP-CS-07` 明确家族独占连续窗口，允许家族内前序本地提交作为后继基线，家族外仍等待合入 | N/A | 依赖登记表、修订历史 |
| 2026-11-17 12:27:28 UTC | Codex R22-A1 FAIL/P1 | 家族窗口先满足全部子卡外部前置；CS-02a 增加 clone/merge/cherry-pick 文档写集及逐文件验收、新仓默认未签名具名端到端测试并接入家族发布点 | N/A | 依赖表、CS-02a、REL-CS-00、审计表 |
| 2026-11-17 12:27:28 UTC | Codex R23-A1 FAIL/P1 | CS-02a 验收清单及追溯表逐文件纳入 clone、merge、cherry-pick 文档与具名测试 | N/A | CS-02a、追溯表 |
| 2026-11-17 12:27:28 UTC | Codex R25-A1 FAIL/P1 | CS-02a 增加中文命令索引；PM-02 config 文档完成后才取得 REL-CS-00 窗口，镜像 DEP-PM-07 | N/A | CS-02a、DEP-CS-04、Plumbing DEP-PM-07 |
| 2026-11-17 16:10:00 UTC | Codex R27-A1 FAIL（部分审查） | Phase 1 退出条件明确排除属于 Phase 2 的 CS-07、CS-09、CS-09a；本阶段逐项列卡 | N/A | Phase 1、修订历史 |
| 2026-11-17 16:35:00 UTC | Codex R29-A0 FAIL/P1 | CS-02 Dependencies、Granularity、审计表及 Phase 0 补齐 DEP-CS-04，确保 PM-02 先于家族窗口 | N/A | CS-02、Phase 0、审计表 |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件；`CS-01` 补 config 生命周期与 `T-1` 触发、`CS-03b`/`CS-03c` 双向真实验签、`CS-04` 三项签名验收拆分、`CS-10`/`CS-25` 独立验收拆分、`CS-14`/`CS-14a` 清理语义与 `MERGE_MSG` 来源、`CS-17` 五个调用方逐个精确过滤、性能矩阵补双规模可重复测量与阈值、`REL-CS-00`..`REL-CS-05` 网站独立 Claude Code PASS 门 | N/A | 全文对应章节 |
| 时间不可考（原记 `2026-11-18 10:35:00 UTC` 晚于该行实际写入、不可证实；文件修改时间只表明写入不晚于 2026-11-18 09:43:54 UTC，实际时刻未知） | Codex 复审 FAIL 修复（验证契约与门禁） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 13:14:30 UTC | 用户 2026-11-18 指令（计划提交门改口径） | 计划文本提交门由「Codex 单审」改为「Claude（Subagent）与 Codex 双审，对同一字节均 `VERDICT: PASS` 方可提交」；只改计划文本提交门，执行期 ER-05 与网站 Claude Code PASS 门不变 | N/A | 头部状态、M0、「Codex review log」说明、完成判据 |
| 2026-11-18 13:14:30 UTC | R30 前置清扫（Claude subagent claude-opus-5-5，六类复发 P1 清单） | 结构性重写：①新增 V-1..V-5（nextest 全名为 `command::<模块>::<fn>`，旧 `test(=init_test)`、`test(/^commit_test::/)` 等零匹配过滤器全部改正；skipped 含过滤排除，不作判据）、FX-BASE/SNAP 全量快照、D-1 文档门与 W-1..W-6 网站协议（补上此前被 44 处引用却不存在的「文档与网站验收命令」，按后端 `AGENTS.md` 的 sync/typecheck/build/Claude Code PASS/`cf` 推送/部署核验，进程内比对、不写证据文件）；②删除机械生成的「AC → 断言映射」，每卡 AC 改为单谓词并在 Verification 直接标注覆盖范围，失败类 AC 断言稳定错误码或诊断文案之一；③废弃重复卡 `CS-26`（不复用编号），`src/cli.rs` 派生并入 `CS-25a`，性能测量并入 `CS-15`；④`DEP-CS-07` 扩为覆盖全部实现卡与任何相交文件的 Coordinator 动态窗口，全部公开行为卡纳入 `COMPATIBILITY.md`；⑤`DEP-CS-09` 去除 `/tmp` 脚本与输出引用；outgoing 依赖改用本计划 ID `DEP-CS-10..13`（对方 ID 写在对象列），删除方向为 `none` 的 `DEP-SA-08` 行；⑥按 pinned `6de20f6092` 源码更正 Git 语义：`CS-03` 非法配置码 `LBR-CLI-002`、`CS-11` non-characters 同样按 Latin-1 映射、`CS-12` 只查最后一个 trailer、`CS-13` 不再断言 `--no-edit -t`、`CS-15c` 空列表无 `-i/-o` 为普通提交、`CS-18b` 文案句点与 cherry-pick 例外、`CS-25`/`CS-25a` 的 `--[no-]quiet`/`--[no-]detach` 与 `gc.auto` 回退、`ADR-CS-03` 改为共享维护锁释放后派生；⑦`CS-01` 收窄为在现有生成/导入两把密钥间按 keyid 选钥，删除多密钥存储与 config 生命周期；⑧更正 2026-11-18 09:43:54 UTC 行的时间（原误记 10:35:00 UTC） | `CS-26` → 废弃（并入 `CS-25a`、`CS-25b`、`CS-15`）；`DEP-SA-01/02/07a`、`DEP-MP-02` → `DEP-CS-10..13` | 全文 |
| 2026-11-18 13:34:09 UTC | Claude R30 FAIL（8 项 P1）+ Codex R30 FAIL（分区 A 3、B 9、C 7、D 7 项 P1），受审 SHA-256 `d109fd15b9d5…` | ①G-03 按独立谓词重算：新增 V-5（失败类 AC 只断言一个可观测量）与 V-6 FAIL-ATOMIC（失败路径统一 SNAP-EQ），删除聚合式「均为/均 SNAP-EQ」AC 与枚举式 AC；②拆卡：`CS-01`→`CS-01`（解析）+`CS-01a`（选钥）；`CS-03a`→`CS-03f`（`gpg.program` 解析契约，原 helper 吞错且不展开 `~/`）+`CS-03a`（签名，含 Git `-bsau <key>` 与默认键）+`CS-03d`（真实 `gpg` 验签与 `merge --verify-signatures`）；`CS-03c`→`CS-03c`（SSH 验签与 merge）+`CS-03e`（签名侧诊断），`REL-CS-01` 三子卡且 ID/语义不变；`CS-07`→`CS-07`（语义，补 reword 消息与 SNAP-Δ）+`CS-07a`（冲突诊断）；`CS-15d`→ 新增 `CS-15f`（`--only` 无路径，更正 Git `--amend`/`--fixup` 例外方向）；`CS-15c`→`CS-15c`（解析）+`CS-15g`（含 `--include` 的模式分发）；`CS-22`→`CS-22`+`CS-22a`（hook 参数，`-m` 优先于 `--amend`、`--amend` 传字面 `HEAD`）；`CS-23a`→`CS-23a`（等待/终止/身份）+`CS-23b`（状态与 op-log 隔离）；`CS-25a`/`CS-25b` 组成新家族 `REL-CS-06`（启用与隔离同版，隔离含 `NEXTEST_RUN_ID` 覆盖直接启动器，`CS-25b` 纳入 `COMPATIBILITY.md`）；③`CS-17` 收窄为 `commit --date` 复用 `approxidate.rs`（`parse_date` 调用方与 `config --type=expiry-date` 不变，新增 `DEFER-CS-04`），相对日期以系统时钟为准；④W 协议：家族后继子卡可承接前序草稿，发布点对最终合成内容重跑 W-2..W-4；D 组默认补 CDN 可获取性、W-6 只对有网站写集的卡；⑤定义 MSG 规范化；更正 `CS-04`（AC5 用未签名夹具）、`CS-06`（同目标 `-C`）、`CS-08`（`--porcelain` 覆盖）、`CS-12a`（verbatim 例外）、`CS-14`（整消息常量）、`CS-19`（全量 SNAP-Δ 与多条 notes）、`CS-25`（老化对象夹具）；⑥发布点 AC 收敛为聚合守卫、release note 与状态更新，子卡交接为不计数门 | `CS-01`→`CS-01,CS-01a`；`CS-03a`→`CS-03f,CS-03a,CS-03d`；`CS-03c`→`CS-03c,CS-03e`；`CS-07`→`CS-07,CS-07a`；`CS-15d`→`CS-15d,CS-15f`；`CS-15c`→`CS-15c,CS-15g`；`CS-22`→`CS-22,CS-22a`；`CS-23a`→`CS-23a,CS-23b`；新增 `REL-CS-06` | 全文（实施顺序、依赖登记表、发布分组、Phase、审计表、追溯表、测试矩阵、延后项、完成判据） |
| 2026-11-18 13:42:09 UTC | Claude R31 FAIL（1 项 P1）+ Codex R31 FAIL（A 2、B 5、C 5、D 7 项 P1），受审 SHA-256 `15676752dd88…` | ①`ADR-CS-03`/`ADR-CS-04` 与卡片对齐（`NEXTEST_RUN_ID` 隔离、`REL-CS-06`；`REL-CS-05` 三子卡）；②`DEP-CS-07` 家族窗口扩至 `REL-CS-06`；③SNAP ① 改为 `reference` 表全量（含 `refs/notes`），共用 helper 由 `CS-01a` 交付并带负向自检；V-5 明确「以…失败」为非零退出+单一可观测量，V-6 允许卡内指定基线（`CS-23b` 以嵌套后 S_n 为基线，并改为全量比较与 `LBR-CONFLICT-002`）；④`CS-03a` 要求 `SIG_CREATED`；`CS-03d` 解析状态行并拒绝冲突状态，拆出 `CS-03g`（merge 信任级别 ≥ marginal）；`CS-03f` 补 global/system 级联与上层读取失败；`CS-03c`/`CS-03e`/`CS-22` 失败 AC 补稳定码或失败形式；⑤MSG 常量去掉末尾换行（`CS-05`/`CS-06`）；`CS-07` 改为完整目标消息并补 `amend!` 目标对照，拆出 `CS-07a`（reword 全部冲突）与 `CS-07b`（`-m` 冲突与未知前缀）；`CS-15c` 改为解析函数级验收（不依赖后继卡），CLI 接入并入 `CS-15g`；`CS-15f` 用 SNAP-Δ；⑥`CS-25` 补对象集合不变与逐任务阈值（`gc.auto` 只关 gc 任务）；`CS-25b`、`CS-24` 补用户命令文档与网站；⑦发布点 AC/VER 逐子卡展开，交接门证据不计数；⑧更正 `CS-12a` 锚点、成功定义与 `DEFER-CS-03` 的 `#783` 归属 | `CS-03d`→`CS-03d,CS-03g`；`CS-07a`→`CS-07a,CS-07b` | 依赖登记表、ADR、执行协议、审计表、追溯表、延后项 |
| 2026-11-18 13:48:55 UTC | Claude R32 FAIL（1 项 P1）+ Codex R32 FAIL（A 3、B 4、C 5、D 8 项 P1），受审 SHA-256 `2b05709d60f9…`；另按协调者通知核对已推送的 global-init-infra（`f3316d9`） | ①`CS-03c` 按 Git SSH 语义区分 `Good { trust: Fully }` 与未知签名者 `Good { trust: Undefined }`，merge 对后者拒绝；`CS-03d`+`CS-03g` 组成新家族 `REL-CS-07`，避免中间版本放行未受信任签名；②`CS-19` 拆为 `CS-19`（复制语义，多值/glob 明确警告，登记 `DEFER-CS-05`）+`CS-19a`（锁/CAS 失败语义与警告），组成新家族 `REL-CS-08`；`CS-25` 拆出 `CS-25c`（incremental-repack/gc 条件与取锁等待上界，以 `--json` 任务列表观测），`CS-25` 补对象内容与 ⑤ 分量的独立断言，`ADR-CS-03` 改为逐任务条件；③V-3 共享回归组移入卡内「A 组 focused 门」不计 G-03，各卡 VER 重算；V-4 以最终本地提交树为通过记录；W-6 以本次提交新增句绑定线上部署；④拒绝/hook 失败 AC 统一为「以…失败」形式（`CS-03`/`CS-09`/`CS-15`/`CS-15a`/`CS-15b`/`CS-15d`/`CS-15e`/`CS-18b`/`CS-21a`/`CS-23`/`CS-23a`/`CS-23b`）；⑤`CS-06` 编辑器场景改为完整消息常量；`CS-07` 补 `amend:` 空树提交；`CS-07a` 按 Git 先查路径的顺序改写并补优先级用例；`CS-07a`/`CS-07b`/`CS-15d`/`CS-15e`/`CS-09a` 夹具自含；`CS-15g` AC1 改为与位置 pathspec 提交树相等；`CS-18b` 改为当前 author 身份；`CS-18a` 补非 UTC 偏移；⑥发布点 AC 改为逐子卡标题与状态值核验；⑦`DEP-CS-06` 按对方已推送版本补 `GII-19/20/22/24`，关系表引用对方 `DEP-GII-03`；M1/M3 列全家族发布点 | `CS-19`→`CS-19,CS-19a`；`CS-25`→`CS-25,CS-25c`；新增 `REL-CS-07`、`REL-CS-08` | 依赖登记表、发布分组、ADR、执行协议、Phase、审计表、追溯表、里程碑、延后项 |
| 2026-11-18 14:02:34 UTC | Claude R33 PASS（2 项 P2）+ Codex R33 FAIL（A 2、B 6、C 3、D 10 项 P1），受审 SHA-256 `5ca8699e968d…` | ①V-4 改为被测提交 `T` + 纯记账提交协议（运行前后 `HEAD`/树/状态三项检查，`T` 之后只含本计划文件与 `plan-status.md` 的提交不触发重跑；发布后结果以纯记账提交登记；终止链末端按模板 v2.14 预登记由 `plan-status.md` 例行同步补记）；M0、Review log 与「完成判据」写明 PASS 行是对受审字节 X 的纯记账追加；V-6 扩至 `commit` 预览命令零写入；②`CS-03d` 写集纳入 `src/command/merge.rs`，中间状态只放行 fully/ultimate（fail-closed），`CS-03g` 放宽到 marginal 并新增 marginal 用例（`trust-model direct` 夹具）；`CS-01a` 以 G/I 公钥实际验签并加错误公钥对照；`CS-03e` AC2 命令补 `-S`；`CS-10` diff 段与 `libra diff --cached`（amend 为 `HEAD^`）逐字节比较；`CS-11` 补合法 UTF-8 保真；③`CS-15g` 以显式构造的期望树 E/E′ 比较，`--include` 合并为单一树等式；`CS-15d`、`CS-08` 的预览零写入改由 V-6 承载；④`CS-16` 补 `commit.template` 存在模板；`CS-18a` Verification 覆盖 AC1–AC8，JSON 与改动前基线逐字段相等；`CS-19` 拆为 `CS-19`（单 ref 复制、完整 SNAP-Δ、`refs/notes/` 之外的 ref 按 Git 警告拒绝）+ 新增 `CS-19b`（多值与 glob，删除 `DEFER-CS-05`）+ `CS-19a`，`REL-CS-08` 三子卡；`CS-22` 补 `--no-verify` 跳过 `pre-commit` 及对照；⑤`CS-25` 按 Git `builtin/gc.c:976-1003` 更正 loose 条件为 ≥ 100 并补 99/100 边界、`--auto` 只运行有条件的任务；新增 `CS-25d`（`--[no-]detach`，以暂停钩子证明父命令先返回）；`CS-25c` 更正 pack 条件为 ≥ 5（4/5 夹具）并补锁释放后继续执行；`CS-25a` 补超阈值仓库实际执行与 `child_exit`，新增 `CS-25e`（`gc.autoDetach` 回退），`REL-CS-06` 三子卡；`CS-25b` 以 `GIT_TRACE2_EVENT` 临时文件归因替代系统级 `pgrep` | `CS-19`→`CS-19,CS-19b,CS-19a`；`CS-25`→`CS-25,CS-25d,CS-25c`；`CS-25a`→`CS-25a,CS-25e`；删除 `DEFER-CS-05` | 验证协议、ADR-CS-03、GAP 表、Phase、审计表、追溯表、里程碑、风险登记、性能摘要、延后项、完成判据、Review log |
| 2026-11-18 14:12:42 UTC | Claude R34 FAIL（1 项 P1）+ Codex R34 FAIL（A 3、B 3、C 3、D 7 项 P1），受审 SHA-256 `9f0d8f4d5cf7…` | ①SNAP ④ 以 `symlink_metadata` 记录路径类型与符号链接目标，`CS-01a` 增加对应负向自检；W 时序的依据改为 ER-06a 同卡内容与验收在子卡完成、公开动作随 G-08 家族不推送窗口，删去对模板 v2.16 code-only 条款的引用；loose 阈值表述改为与 Git 默认值一致、不读取 `maintenance.loose-objects.auto`（登记差异）；②`CS-03a` 并入 `REL-CS-07`（三子卡），`CS-03g` 补外部签名提交经 merge 验签的端到端用例；`CS-03d` 新增中间状态 fail-closed 用例（AC8）；新增 `CS-03h`（SSH 验签按提交者时间 `-Overify-time`，`valid-before`/`valid-after` 夹具与 merge 放行/拒绝），`REL-CS-01` 四子卡（ID 与语义不变）；`CS-04` AC5 拆为三条；③新增 `CS-09b`（`--interactive` 与 `-a`/`-i`/`-o` 互斥，`REL-CS-02` 三子卡）；`CS-09a` 以全选与候选列表证明路径限定；`CS-17` 拆出 `CS-17a`（本地 ISO 与夏令时），`CS-17` 以五个输入逐一固定 `config --type=expiry-date` 改动前基线；`CS-15g` 期望树改用独立命令求得；`CS-15b` A 组加入 `add_patch_test`；④`CS-18` 断言完整 author 行并拆分 committer 时间与其余字节；`CS-19b` 按 ref 精确比较并补两夹具 SNAP-Δ；`CS-24` 分别断言 `true`/`false` 的真实退出码；`CS-25c` 只保留任务条件并补 A 组门，新增 `CS-25f`（取锁带界等待，以等待标记与释放前未退出、快照不变证明确实等待）；`CS-25b` AC8 改为在 nextest 下经公共 harness 继承环境的用例（trace 文件位于用例 tempdir），删除操作者侧临时 trace 文件；⑤更正原记 `10:35:00 UTC` 行的时间列为「时间不可考」并给出可证上界 | `CS-03c`→`CS-03c,CS-03h`；`CS-09`→`CS-09,CS-09b`；`CS-17`→`CS-17,CS-17a`；`CS-25c`→`CS-25c,CS-25f`；`CS-03a` 改为 `REL-CS-07` 子卡 | 验证协议（SNAP）、网站协议、ADR-CS-03、GAP 表、发布分组、Phase、审计表、追溯表、风险登记、修订历史（原 10:35:00 行）、Review log |
| 2026-11-18 14:22:20 UTC | Claude R35 PASS（2 项 P2）+ Codex R35 FAIL（A 3、B 3、C 4、D 5 项 P1），受审 SHA-256 `d9acc90e2fd2…` | ①网站时序回到 ER-06a 同卡交付：每张网站写集非空的实现卡（含家族子卡）在本卡内完成 W-1..W-6，家族发布点不再承担网站门，D 组不另列 W-6；子卡网站早于家族 CLI 公开的时差登记风险，故障恢复改为网站前滚撤回；W-1 增加暂存区为空、目标文件受控/未忽略/`--untracked-files=all` 无改动的逐文件预检；②SNAP 新增 ⑦ operation-log（`OperationStore` 全量记录、op heads 与 journal）及统一口径（非预览命令只允许自身登记的 operation 记录与成功时的 heads 推进），`CS-01a` 增加 operation 记录负向自检，`CS-08` 把 `--short`/`--long` 纳入 `src/cli.rs` 预览只读分类；③`CS-03h` 改用 `+0800` 时区与跨有效期边界的作者时间；发布点守卫按子卡新增具名用例补全（`REL-CS-01` 改为三个守卫组、`REL-CS-07` 补 `merge.rs` 单测、`REL-CS-06` 补 harness 用例），构建脚本校验发布点覆盖全部子卡具名用例；`CS-01` 增加经二进制的端到端用例；④`--pathspec-from-file` 与 `--patch` 的互斥移入 `REL-CS-04` 的 `CS-15e`；`CS-07`/`CS-07a`/`CS-07b` 组成新家族 `REL-CS-09`，`CS-07a` 补合并中拒绝 reword；`CS-15a` 补 `-i -a`，`CS-15b` 补 `-p -i`/`-p -o`（删去冗余的输出断言）；⑤`CS-18b` 断言 `--amend`/`-c` 的作者重置；`CS-19a` 以并发写入后的快照为基线；`CS-25d` 补对象集合、内容与反向顺序；`CS-25f` 释放后断言老化对象已打包且对象集合不变，释放场景上界改为 20000 ms；`CS-25b` AC8 改为移除 `NEXTEST_RUN_ID` 后经公共 harness 启动；`CS-16` 断言提交消息 | 新增 `REL-CS-09`（`CS-07`、`CS-07a`、`CS-07b`） | 验证协议（SNAP、V-6）、网站协议、D 组默认、发布分组、Phase 2、审计表、追溯表、里程碑、故障恢复、风险登记、完成判据、Review log |
| 2026-11-18 14:27:54 UTC | Claude R36 PASS（1 项 P2）+ Codex R36 FAIL（A 3、B 4、C 1、D 2 项 P1），受审 SHA-256 `0e582eb82e02…` | ①SNAP ⑦ 改为直接读取 `operation`、`operation_parent`、`operation_head`（完整行含 scope 与 generation）、`operation_journal` 全量与 operation 指针文件字节，统一口径允许被测命令自身 `running` 记录转终态；SNAP helper 及其五项单分量负向自检改为「SNAP helper 交付门」（V-6 基础设施，随 `CS-01a`，不计入 G-03）；W-1 改用 `check-ignore --no-index`；W-6 以 `PIPESTATUS` 分别判定上游读取与下游匹配，并新增「撤回分支」部署判据，故障恢复引用之；②`CS-01a` 增加默认源为 G 时显式选择 I 的验签用例；`CS-03b` 三种选钥形式各用可区分密钥并以只含对应公钥的 allowed signers 验签，另加反向对照；`CS-03h` 增加 `gpg.ssh.revocationFile`（`-r`）与 merge 拒绝被吊销签名者；`CS-03g` 增加 `gpg.minTrustLevel`；③`CS-15d` 的 `--porcelain`/`--short` 改为与 pinned Git 实测常量逐字节一致，`--dry-run` 只断言待提交段；`CS-15e` Out of scope 收窄；④`CS-18b`、`CS-24`、`CS-25a` 改为单一比较谓词；`CS-23b` AC2 与 ⑦ 统一口径对齐 | N/A | 验证协议（SNAP、W-1、W-6）、故障恢复、`CS-01a`、`CS-03b`、`CS-03c`、`CS-03g`、`CS-03h`、`CS-15d`、`CS-15e`、`CS-18b`、`CS-23b`、`CS-24`、`CS-25a`、审计表、Review log |
| 2026-11-18 14:33:40 UTC | Claude R37 FAIL（1 项 P1）+ Codex R37 FAIL（A 2、B 4（其一核实为误报）、C 2、D 3 项 P1），受审 SHA-256 `8710ed59094b…` | ①发布点按模板 B 类型表 `release` 行把聚合守卫（逐个列出本组全部新守卫用例）与 release note 核验列为「ER-04 B 组 release 类型门」（全局强制门，不计入 G-03），卡特有 AC 只保留各子卡状态更新；release note 先以变量捕获 `gh` 输出并单独判定其退出码；②W-2 增加撤回分支（以撤回后文档为同步源，被撤回句不含、保留句仍含）；W-1 与「文档职责」统一为按 ER-06a 切换 `cf`、受阻即 STOP；「文档职责」登记 `AGENTS.md:130` 默认签名表述滞后为跨计划协调事项；③`CS-03a` 成功夹具输出完整 `[GNUPG:] SIG_CREATED` 状态行；`CS-01a`、`CS-03a` 写集与文档加入 `CLAUDE.md`；④`CS-09a` 非法输入、只看状态与立即 EOF 改为失败断言（V-6 快照）；⑤`CS-23b` 拆为 ref/index/worktree 三个独立场景；`CS-25d` 覆盖不带 `--auto` 的 `--detach`；`CS-19` 增加 `notes.rewriteMode` 默认 `concatenate` 合并与非默认模式警告放弃（登记差异） | N/A | 文档职责、网站协议（W-1、W-2）、全部发布点、`CS-01a`、`CS-03a`、`CS-09a`、`CS-19`、`CS-23b`、`CS-25d`、审计表、追溯表、Review log |
| 2026-11-18 14:38:45 UTC | Claude R38 PASS（1 项 P2）+ Codex R38 FAIL（A 2、B 3、C 2、D 3 项 P1），受审 SHA-256 `310f06fc53ff…` | ①SNAP ⑤ 改为 helper 严格枚举 loose 对象与全部 pack index（任一不可解析即失败），SNAP helper 交付门增加损坏索引负向自检；故障恢复矩阵的禁止结果改为可执行终态；W-5 哈希管道以 `PIPESTATUS` 判定；②发布点 B 组门改在子卡交接后、bump 前的合成树上执行（聚合守卫 + 写入本卡的 release note 草稿），bump 后由 C 组 `T-2` 全量重跑守卫，实际 Release 说明改为「发布后核验」并以纯记账提交登记；③`CS-03a` 以 vault 公钥验签证明未配置 `gpg.program` 时的回退；`CS-03d` 以独占 `TMPDIR` 验收成功与失败路径的临时文件清理（真实 `gpg` 验签成功已覆盖参数管道，删去记录型脚本断言）；`CS-03g` 增加大小写与非法 `gpg.minTrustLevel`；`CS-03f` 写明新建测试模块；④`CS-17a` 增加夏令时输入的 `config --type=expiry-date` 基线守卫；⑤`CS-19` 拆出 `CS-19c`（非默认 `notes.rewriteMode` 警告并放弃，三模式各以 SNAP-Δ 验证 notes 全量不变），`REL-CS-08` 四子卡；`CS-23b` index 场景改为编辑器直接写入索引文件；`CS-25c` 改为断言实际效果（pack 数减少、对象保留、不可达对象被清除） | `CS-19`→`CS-19,CS-19c` | 验证协议（SNAP、W-5）、故障恢复、全部发布点、`CS-03a`、`CS-03d`、`CS-03f`、`CS-03g`、`CS-17a`、`CS-19`、`CS-19c`、`CS-23b`、`CS-25c`、GAP 表、审计表、追溯表、Review log |
| 2026-11-18 14:43:47 UTC | Claude R39 FAIL（1 项 P1）+ Codex R39 FAIL（A 1、B 3、C 1、D 3 项 P1），受审 SHA-256 `f27cf19cb281…` | ①发布点按模板 G-03 `release` 行重排：每个聚合守卫用例与 release note（草稿逐字等于固定格式期望文本、实际 Release 说明与草稿逐字节一致）各计一条 AC 与 Verification；子卡交接门及其证据（状态、ER-05、ER-07、`origin/main` OID 与祖先退出码）移到任何 bump 与推送之前；子卡 `Lifecycle` 收口按 ER-04 列为不计数的状态收口；②`CS-03g` 以 `Fully` 下 ultimate 合并成功与 marginal 拒绝成对验证，并把 `gpg.minTrustLevel` 纳入 config 文档与网站；吊销文件从 `CS-03h` 拆为新卡 `CS-03i`（含未吊销对照、merge 拒绝与缺失文件警告，config/merge 文档与网站），`REL-CS-01` 五子卡（ID 与语义不变）；③`CS-25c` gc 夹具预置隔离账本 `first_seen`（不改隔离机制）；`CS-24` 兼容矩阵落点改为既有 commit 行；性能摘要与 `REL-CS-04` 依据措辞更正 | `CS-03h`→`CS-03h,CS-03i` | 全部发布点、`CS-03c`、`CS-03g`、`CS-03h`、`CS-03i`、`CS-24`、`CS-25c`、GAP 表、性能摘要、审计表、追溯表、Review log |
| 2026-11-18 14:47:54 UTC | Claude R40 FAIL（1 项 P1）+ Codex R40 FAIL（A 2、B 2、C 1、D 5 项 P1），受审 SHA-256 `638d236aa119…` | ①SNAP ③ 改为经索引 API 记录除 stat 缓存外的全部字段与标志（含 skip-worktree、intent-to-add）及扩展段，helper 交付门增加两项标志负向自检；W-1 要求 `fetch` 后本地 `cf` 与 `origin/cf` 一致，W-6 推送前要求待推送历史恰为本次提交；②发布点的实际 Release 说明比较改为「各自去掉末尾全部换行后逐字符一致」并只在 shell 变量中比较；③`CS-03i` 增加吊销文件缺失时 merge 成功；`CS-19a` 两个失败场景断言 HEAD 指向新 amend 提交；`CS-25c` 增加 `gc.auto=10` 低于阈值对照；`CS-25d` 以退出码与 `<p>.pid` 为分离执行条件并补非 auto 场景完成断言；`CS-25a` 增加维护子进程失败不影响提交结果（`LIBRA_TEST_MAINTENANCE_FORCE_FAIL`），`--no-quiet` 映射移入 `CS-25e` | N/A | 验证协议（SNAP、W-1、W-6）、全部发布点、`CS-03i`、`CS-19a`、`CS-25a`、`CS-25c`、`CS-25d`、`CS-25e`、审计表、Review log |
| 2026-11-18 14:51:14 UTC | Claude R41 PASS（1 项 P2）+ Codex R41 FAIL（A 1、C 2、D 3 项 P1；B 区 PASS），受审 SHA-256 `fa06e0e4d04e…` | ①W-1 在 shell 变量中记录全量状态基线与本卡写集外路径的哈希，W-2 改为比较同步前后写集外的状态行与哈希不变（保留其它计划的既有改动）；②发布点 Release 说明草稿置于 `<!-- release-note-draft:<ID> begin/end -->` 标记行之间，以 `awk` 提取并检查非空，期望文本由计划内各子卡标题行按登记顺序生成；③`CS-15f` 增加 `--amend --only -m z` 的消息断言；`CS-25a` 失败注入场景设 `autoDetach=false` 并以 trace 的非零 `child_exit` 证明失败已发生，再断言 commit 退出码 0；`CS-20` 写集与文档加入 `COMPATIBILITY.md` commit 行 | N/A | 网站协议（W-1、W-2）、全部发布点、`CS-15f`、`CS-20`、`CS-25a`、审计表、Review log |
| 2026-11-18 14:55:36 UTC | Claude R42 PASS（2 项 P2）+ Codex R42 FAIL（B 3、C 2、D 2 项 P1；A 区 PASS），受审 SHA-256 `5872aab8e3f2…` | ①`CS-03` 增加过渡期 `gpg.format=ssh` 以 `LBR-UNSUPPORTED-001` 拒绝，`CS-03b` 负责把该断言改为签名成功；`CS-06` 按 Git `strcmp` 改为参数字符串比较并增加同对象不同字符串用例；`CS-04` 以完整 committer 行断言提交者身份；②`CS-15g` 增加默认、`--only`、`--include` 三种模式提交后主索引等于期望索引，引号错误用例移入 `CS-15e`；`CS-17` 把支持范围收窄为现有子集加 t7501 句式（`noon`/`midnight` 两个分支），其它自然语言表达式拒绝并登记差异，配置基线改为两个新句式与两个既有形式；③`CS-25a` 以 `CS-25d` 的暂停钩子证明前台维护期间 commit 尚未退出、释放后完成打包，清理阶段等待放在时序断言之后；`CS-21` 增加首段自带尾随换行的多段 `-m`；④W-1 写集外路径按类型记录指纹（缺失/链接目标/文件哈希）；「字段全局默认与例外」与追溯表的 `CS-20` 口径与卡片一致 | N/A | 网站协议（W-1、W-2）、字段默认、`CS-03`、`CS-03b`、`CS-04`、`CS-06`、`CS-15e`、`CS-15g`、`CS-17`、`CS-21`、`CS-25a`、`CS-25d`、审计表、追溯表、Review log |
| 2026-11-18 15:01:14 UTC | Claude R43 FAIL（1 项 P1）+ Codex R43 FAIL（A 2、B 3、C 3、D 2 项 P1），受审 SHA-256 `a670d1866cf1…` | ①`DEP-CS-05` 前置移到实际首个 `src/cli.rs` 写者 `CS-08`（其后写者同样以此为前置）；W-5 恢复 `PIPESTATUS` 判定；SNAP 新增 ⑧ 对象物理布局（loose 路径集合与 pack/idx 文件及哈希）及统一口径，helper 交付门增加「loose 移入 pack」负向自检，`CS-25`/`CS-25c` 的空操作场景明确含 ⑧；②`CS-03a` 拆出 `CS-03j`（非零退出、缺 `SIG_CREATED`、空签名与非签名输出均 fail-closed），`REL-CS-07` 四子卡；新增 `CS-03k`（SSH 多 principal 逐个尝试、`gpg.minTrustLevel` 对 SSH 生效），`REL-CS-01` 六子卡（ID 与语义不变）；③`CS-15` 拆出 `CS-15h`（部分提交删除路径），`REL-CS-03` 四子卡；`CS-14` 增加 `core.commentChar=;` 下非编辑器默认清理；`CS-17` 收窄为专用入口只额外接受 t7501 句式与点分相对日期，其余交给现有解析，并验收 `last friday` 拒绝；④新增 `CS-25g`（`maintenance run --[no-]quiet` 输出与末选项生效） | `CS-03a`→`CS-03a,CS-03j`；`CS-03c`→`CS-03c,CS-03k`；`CS-15`→`CS-15,CS-15h`；`CS-25`→`CS-25,CS-25g` | 验证协议（SNAP、W-5）、依赖登记表、GAP 表、发布分组、Phase、审计表、追溯表、Review log |
| 2026-11-18 15:03:08 UTC | Claude R44 PASS + Codex R44 FAIL（A 区 PASS；B 1、C 3、D 3 项 P1），受审 SHA-256 `a87611a7d69c…` | 记账：本轮为本次双审的第 15 轮（R30–R44），达到协调者规定的轮次上限仍未取得两方对同一字节的 `PASS`，按规则停止修订、不提交；R44 未关闭的 7 项 P1 登记于 Review log，留待后续评审处理 | N/A | 文头、Review log |
| 2026-11-18 15:17:45 UTC | 接续评审（R45 起）：修订 Codex R44 遗留 7 项 P1，并按 BRIEF 六类清单对全部卡清扫 | ①`CS-01` 端到端改用 `vault.gpg.pubkey` 默认公钥的完整 fingerprint 作 keyid（`CS-01a` 落地后仍选中同一把密钥，用例无需改写）；②`CS-15`/`CS-15h` 以 SNAP-Δ 全量比较提交前后主索引（只允许所选路径条目变化），清扫同类把 `CS-15a`/`CS-15b` 的主索引断言改为同一 SNAP-Δ 口径；③`CS-15g` AC5 改为 stdin + `--pathspec-file-nul` 的端到端提交（对应 t7526:52-61）；④`CS-09a` 以 update 后 `status` 渲染的整行与 `diff` 输出的暂存差异行断言两菜单命令（AC6/AC7，以 `git` 实测核对格式），删去原「只执行 status/diff 后 quit」与「stdin 立即 EOF」两条失败用例（EOF 无暂存失败仍由 AC8 覆盖）；⑤`CS-21` AC5 改用真实换行 `$'a\n\n'` 并写明期望消息字节 `61 0a 0a 0a 62 0a`；⑥`CS-25g` 增加 `--no-quiet --quiet` 反序用例；⑦`CS-25c` 拆出 `CS-25h`（gc 条件；`gc.auto=10`/`0` 两个对照各断言 `tasks[]` 不含 `gc` 与运行前后 SNAP-EQ，夹具保证 gc 一旦执行必删两个对象）；⑧清扫发现：成功的 `maintenance run` 按 `src/command/maintenance.rs:312-319` 写入 `maintenance.last-run`，原 `CS-25`/`CS-25c` 的 SNAP-EQ/SNAP-Δ 会令正确实现失败，SNAP 新增「⑥ 的统一口径」只放宽该键，helper 交付门改为 ①–⑧ 并补同名 pack 字节变化与 ⑥ 放宽自检，`CS-01a` 自检描述同步；`CS-14a` 写明 AC4 的 `-F` 消息内容 | `CS-25c`→`CS-25c,CS-25h` | 夹具与快照判据（SNAP、helper 交付门）、`ADR-CS-03`、GAP-06、实施顺序、发布分组、Phase 4、审计表、追溯表、风险登记 |
| 2026-11-18 15:25:15 UTC | Claude R45 PASS（4 项 P2）+ Codex R45 FAIL（A 区 PASS；B 1、C 2、D 3 项 P1），受审 SHA-256 `01f2fa099498…` | ①`CS-03a`/`CS-03j` 改为：Git 只以退出码与独立 `SIG_CREATED` 行判定（`gpg-interface.c:1029-1046`），stdout 须为非空签名块是 Libra fail-closed 加固并登记 `COMPATIBILITY.md` 差异；②`CS-15h` AC1 改为提交树条目集合等于原 HEAD 树去掉 `c`（`b` 为原 HEAD 内容）；③`CS-16` 增加存在模板时（`--template` 与 `commit.template`）提交消息 MSG 为 `msg\ntemplate line` 两条 AC（本机 `git` 实测），夹具写明编辑器插入行的方式；④`CS-24` 增加 `libra status` 的生命周期事件 AC；⑤`CS-25a` 拆出 `CS-25i`（`REL-CS-06` 2/4：默认分离时 commit 在维护暂停期间返回、释放后维护完成、测试钩子 `LIBRA_TEST_MAINTENANCE_SPAWN_PROGRAM` 注入派生失败时 commit 仍成功），`REL-CS-06` 四子卡并增加聚合守卫；⑥清扫：`CS-02` Description 只声称已验收的 `signing:` 行；`CS-04` 增加 AC8（显式 `vault.signing=true` 时 amend 重新签名，Description 已声称）；`CS-18a` 以冗余的 AC5 改验 `-C` 复用消息时输出 ` Date:` 行（Git `builtin/commit.c:694-697,1358-1363`）；处理 R45 Claude P2：helper 的 pack 字节自检限定改写 trailer 校验和且判据为「不得判定相等」，`CS-25c` 删去已移入 `CS-25h` 的 gc 证据行，「成功定义」与 `DEFER-CS-03` 补全卡列表 | `CS-25a`→`CS-25a,CS-25i` | 成功定义、SNAP helper 交付门、`ADR-CS-03`、GAP-06、实施顺序、发布分组（`REL-CS-06`）、Phase 4、审计表、追溯表、风险登记、延后项、Review log |
| 2026-11-18 15:31:58 UTC | Claude R46 PASS（2 项 P2）+ Codex R46 FAIL（A 1、B 1、C 1、D 3 项 P1），受审 SHA-256 `4d2771563f8d…` | ①SNAP ⑧ 增加每个 loose 对象文件字节的 SHA-256，⑧ 统一口径要求既有 loose 文件哈希不变，helper 交付门增加「以不同 zlib 级别重写 loose 文件」负向自检；②`CS-11` 的 non-character 夹具改为同时含 `EF BF BE` 与 `EF B7 90`（U+FDD0），AC4 改为与完整期望字节串比较；③按 `#769` 验收范围收窄 `CS-17`：只接受 t7501 句式，以三个不同月份/序数后缀（`12th`、`1st`、`23rd`）验收，删去点分相对日期，其余 approxidate 表达式登记 `DEFER-CS-06`（`DEFER-CS-05` 编号不复用）；④`CS-18a` 拆出 `CS-18c`（Date 行：显示条件按 Git `author_message || force_date`，新增 `--amend --reset-author` 不显示对照），`CS-18a` 只保留 Author 行与 `--json`/`--quiet` 不变；⑤`CS-25i` 增加派生失败场景 trace 中 `child_start` 与 `code=-1` 的 `child_exit`（Git `run-command.c:740,965-967`）；⑥`CS-25f` 与 `ADR-CS-03`、故障恢复矩阵把「静默退出」收窄为「以退出码 0 退出且不执行维护，输出不作约束」 | `CS-18a`→`CS-18a,CS-18c` | SNAP、helper 交付门、`ADR-CS-03`、GAP-05、实施顺序、Phase 3、审计表、追溯表、故障恢复矩阵、延后项、兼容与文档收口、Review log |
| 2026-11-18 15:38:03 UTC | Claude R47 FAIL（5 项 P1）+ Codex R47 FAIL（A 2、B 1、C 2、D 1 项 P1），受审 SHA-256 `1f905f02152a…` | ①SNAP helper 自检用例改为 `CS-01a` 的 Verification V2 并计入 G-03（`VER=2/8`），交付门措辞同步；②「完成判据」写明收口全量门在全部活动任务卡 `done`/`complete` 之后运行；③`CS-17` AC3 改在 `TZ=Asia/Shanghai` 下验收（期望 `1614009600 +0800`，本机 `git` 实测）；④`CS-09a` 拆出 `CS-09c`（`REL-CS-02` 4/4：update→revert→quit、stdin 立即 EOF、非法输入后 EOF 三种无暂存中止，均限时 30 秒）；⑤`CS-18c` 去掉「当且仅当」，cherry-pick 续提路径登记 `DEFER-CS-07` 与 `COMPATIBILITY.md` 差异；⑥按 Claude R47：`CS-02a` 增加 `commit.gpgSign=true` 单独生效 AC（`REL-CS-00` 守卫同步），`CS-03g` 拆出 `CS-03l`（`REL-CS-07` 5/5：`gpg.minTrustLevel` 大小写、非法值、收紧与 `undefined` 不放宽默认门），`CS-03k` 引用改为 `CS-03l`，`CS-19` 增加成功路径 op-log 不并入 notes 变化，`CS-25` AC7 改验 `maintenance.loose-objects.auto=0` 不生效、原 `gc.auto=0` 对照移入 `CS-25h` AC7，`CS-15g` Description 收窄为由 `CS-15e` 验收的解析诊断 | `CS-09a`→`CS-09a,CS-09c`；`CS-03g`→`CS-03g,CS-03l` | 成功定义、SNAP helper 交付门、GAP-01/02、实施顺序、发布分组（`REL-CS-00`/`REL-CS-07`/`REL-CS-02`）、Phase 0/2、审计表、追溯表、延后项、完成判据、兼容与文档收口、Review log |
| 2026-11-18 15:41:32 UTC | Claude R48 FAIL（2 项 P1）+ Codex R48 FAIL（A 1、B 2、C 3、D 2 项 P1），受审 SHA-256 `a61e6466c23d…` | ①家族子卡序号一律由发布点子卡列表推导（修正 `CS-09`/`CS-09b`/`CS-09a` 为 `1/4`..`3/4`）；②`CS-03j` 的 `fake-gpg-fail` 改为除退出码外其余成功条件全部满足，新增 `fake-gpg-inline`（`SIG_CREATED` 只在行内出现）与 AC5；③`CS-15d` 增加显式 `--only --porcelain` 预览 AC7；④`CS-09a` 增加选择性 `revert` 后只提交 `file` 的 AC7（本机 `git` 同输入实测）；⑤`CS-18c` AC3 改为断言 `-C` 的完整 `Date:` 值，新增 `-c`（编辑器不改内容）AC6；⑥拆分复合 AC：`CS-22a` 第二/第三参数各一条（AC5、AC6），`CS-25i` `child_start` 与 `child_exit code=-1` 各一条（AC5、AC6）；⑦`CS-02a` AC5 夹具写明在 `libra init` 新仓（具备 vault 生成密钥）中运行 | `N/A` | 审计表、追溯表、`REL-CS-02` 子卡字段、Review log |
| 2026-11-18 15:47:16 UTC | Claude R49 FAIL（3 项 P1）+ Codex R49 FAIL（A 1、B 3、C 1、D 3 项 P1），受审 SHA-256 `5ae900be9f54…` | ①事实更正：Libra notes 存于 SQLite `notes` 表（`src/internal/notes.rs:1-5`），SNAP 新增 ⑨ `notes` 表全量行、① 去掉 `refs/notes`、helper 自检改为改 tag ref 与改 `notes` 行；`CS-19`/`CS-19b`/`CS-19c`/`CS-19a` 的 SNAP-Δ 改写为 `notes` 表行变化（撤回 R48 加入的 `CS-19` op-log AC，其前提有误），`CS-19b` glob 在 `notes_ref` 取值上展开；`CS-19a` 以「第二阶段持锁暂停时并发 `notes add` 报 `operation scope lease is already held`」验收竞争窗口互斥，替换未验的 CAS 声称并把 HEAD 断言并入 AC2；故障恢复矩阵同步；②`CS-08` 增加 `--porcelain --short`、`--short --porcelain`；③`CS-03h` 增加 `check-novalidate`（X2）与 `verify`（X1）的 `-Overify-time` argv；④`CS-03c` 改为缺失/不可读 allowed signers 按 Git 退化为 `check-novalidate` 得 `Good { trust: Undefined }`；⑤`CS-15c` 明确非法 UTF-8 条目以错误拒绝（新增 AC8），非 UTF-8 路径支持登记 `DEFER-CS-08`；⑥`CS-25d` 收窄「最终结果相同」为所比较的对象集合；⑦`CS-25f` 新增生产默认上界场景（≤ 35000 ms），删去与 `CS-25` AC5 重复的对象保留 AC；⑧按 Claude R49：`CS-03b` 失败脚本先写出有效 `.sig` 再非零退出，`CS-21a` AC3 改为整段 stdout 字节相等，`CS-24` AC2/AC8 改为事件投影列表相等、AC3 改为单谓词 | `N/A` | SNAP 与 helper 交付门、故障恢复矩阵、延后项、兼容与文档收口、Review log |
| 2026-11-18 15:50:13 UTC | Claude R50 FAIL（1 项 P1）+ Codex R50 FAIL（A、C 区 PASS；B 1、D 2 项 P1），受审 SHA-256 `5dd3e0c97e36…` | ①`CS-03c` 区分 allowed signers 未配置（Git 报错且不进入 `find-principals`，`gpg-interface.c:483-486`，AC5 改为该情形不返回 `Good`）与路径不存在（退化为 `check-novalidate` 得 `Good { trust: Undefined }`，AC4），删去同路径的不可读变体；②`CS-19a` 基线改为第一阶段记录 `notes.rewriteRef` 对应 `notes_ref` 的全部行、第二阶段持锁比较，夹具与 AC3、AC7 的交错时点同步；③`REL-CS-08` 四张子卡增加 A 组共享回归 `command::commit_test::*` 与 `command::notes_test::*`；④`CS-19a` 标题、axis 与 `CS-19`/`CS-19b`/`CS-19c`、`REL-CS-08` 理由、「评审结论」表去掉 CAS 表述；⑤`CS-19`/`CS-25a` 的「三卡」改为「四卡」 | `N/A` | 评审结论表、发布分组（`REL-CS-08`）、审计表、追溯表、Review log |
| 2026-11-18 15:52:38 UTC | Claude R51 PASS + Codex R51 FAIL（A、C 区 PASS；B 1、D 1 项 P1），受审 SHA-256 `4dc4e81a1e1e…` | ①`CS-08` 收窄：只声称 `--long` 隐含 `--dry-run` 及其与 `--short`/`--porcelain` 的最后出现者生效；`--short` 与 `--porcelain` 在 Libra 走同一输出（`src/command/commit.rs:1226-1230`），二者先后不可观测、不在 `#774` 范围，删去 R50 加入的 AC7/AC8，既有差异登记 `COMPATIBILITY.md`；②`CS-25f` 释放场景改为不设置 `LIBRA_TEST_MAINTENANCE_LOCK_WAIT_MS`（生产默认上界 30 秒），使「等待并在释放后继续执行」在生产默认配置下验收 | `N/A` | 审计表、Review log |
| 2026-11-18 15:55:58 UTC | Claude R52 PASS + Codex R52 FAIL（A、B 区 PASS；C 2、D 1 项 P1），受审 SHA-256 `2f9cb62fb8f4…` | ①SNAP 新增「失败路径的 ⑤ 口径」：非零退出时允许新增 blob/tree（及对应 loose 文件）、不允许新增 commit/tag，其余分量照常比较（与 Git 在钩子、签名失败或交互暂存中止后留下不可达对象一致），V-6 与 helper 交付门同步（新增两条自检）；`CS-09c` AC1 注明 `update` 写入的 blob 按该口径保留；②`CS-15c` 收窄为 UTF-8 路径等价，非 UTF-8 条目（任一模式）行为不作约束并由 `DEFER-CS-08` 承接，删去 R50 加入的 AC8；③`CS-20` 改为同卡交付 `commit` 的 EN/zh-CN/开发文档与网站（W-1..W-6），`switch`/`checkout` 文档不变并写明理由，「默认覆盖」与追溯表的 `CS-20` 例外删除 | `N/A` | V-6、SNAP 与 helper 交付门、字段默认、审计表、追溯表、延后项、Review log |
| 2026-11-18 15:59:22 UTC | Claude R53 PASS + Codex R53 FAIL（A 1、B 1、C 2、D 3 项 P1），受审 SHA-256 `7eefdce74c40…` | ①D-1：被本卡行为推翻的既有陈述须就地替换或删除，并以旧关键句零命中验收（`rg` 退出码 1）；`COMPATIBILITY.md` 改为「既有行内修改、不增删行」，关系表与兼容收口同步；②`CS-03a` 只交付调用、选键与成功路径，签名结果判据与失败中止完全由 `CS-03j` 实现并验收（G-02）；③`CS-15c` 增加 `\\`、`\"`、`\n` 转义 AC8；④`CS-09a` 夹具加入预先暂存的 `s`（本机 `git` 实测同样提交），AC1 期望树含 `s`，新增 AC8 SNAP-Δ 全量比较主索引；⑤`CS-22` 把对照并入 AC3 的执行条件，新增 `--no-verify` 下 `post-commit`（AC4）与 amend 的 `post-rewrite amend`（AC8）仍被调用；⑥`CS-19` 增加 concatenate 场景的 SNAP-Δ（AC8）；⑦`CS-25f` 释放场景在 `mark` 出现后继续持锁 5 秒再释放 | `N/A` | D-1、关系表、兼容与文档收口、审计表、Review log |
| 2026-11-18 16:03:51 UTC | Claude R54 FAIL（1 项 P1）+ Codex R54 FAIL（A、B 区 PASS；C 2、D 1 项 P1），受审 SHA-256 `ed73fffd7f6a…` | ①撤回 `CS-22`（编号废弃不复用）：Libra 有意把 `--no-verify` 作为跳过全部 hooks 的完整逃生阀（`tests/compat/libra_hooks_lifecycle_test.rs:163-172`），改为 Git 语义属安全设计决定且不在 `#767`/`#778` 验收内，登记 `DEFER-CS-09`；`#767`/`#778` 的 PD-08 收口并入 `CS-22a`（标题、Description、Docs 同步，依赖改为 `CS-21a`）；GAP-06、事实基线、成功定义、Phase 4、废弃编号、延后项同步；②`CS-15c` AC5/AC6 改用多字节八进制转义 `"\303\251.txt"` → `é.txt`；③`CS-09a` AC8 改为对选择性 revert 场景（AC7）做 SNAP-Δ，同时证明 `other` 索引条目恢复与预先暂存的 `s` 保留 | `CS-22` → 撤回（并入 `CS-22a`） | 成功定义、事实基线、GAP-06、实施顺序、Phase 4、废弃编号、审计表、追溯表、延后项、兼容与文档收口、Review log |
| 2026-11-18 16:06:01 UTC | Claude R55 PASS + Codex R55 FAIL（A、B 区 PASS；C 1、D 1 项 P1），受审 SHA-256 `b18e2fff4f89…` | ①`CS-15g` 的 `list` 改为一行带引号的 `"file\101.t"`（解码为 `fileA.t`，t7526:100-112）加一行 `fileB.t`，使默认、`--only`、`--include` 三种模式的 CLI 用例都经过非 NUL 引号解码；②`CS-25g` AC1/AC4 以「命令退出码 0 且已老化对象均已打包」为执行条件 | `N/A` | Review log |
| 2026-11-18 16:08:14 UTC | Claude R56 FAIL（1 项 P1）+ Codex R56 FAIL（A、D 区 PASS；B 1、C 1 项 P1），受审 SHA-256 `c0542caeccb3…` | ①`CS-03j` 收窄为「错误诊断包含外部程序 stderr」（Git `gpg failed to sign the data:` 后接状态输出，`gpg-interface.c:1041-1046`），不承诺逐字节原样；②`CS-09a` 夹具给 `new` 固定内容 `N1\n`，AC3 改为断言提交树中 `new` 的 blob 内容 | `N/A` | Review log |
| 2026-11-18 16:10:29 UTC | Claude R57 PASS + Codex R57 FAIL（B、C、D 区 PASS；A 1 项 P1），受审 SHA-256 `8fa57921be69…` | SNAP ⑥ 改为只读直接读取 SQLite `config_kv` 表全量行（含密文与 `encrypted` 标志）与旧 `config` 表全量行、不解密，不再以会显示 `<REDACTED>` 的 `libra config list` 采集（`src/command/config.rs:3879-3880`）；helper 交付门增加「只替换一个加密配置行的密文」负向自检 | `N/A` | SNAP 与 helper 交付门、Review log |
| 2026-11-18 16:12:51 UTC | Claude R58 PASS + Codex R58 PASS（A/B/C/D 四区），受审 SHA-256 `fdc9dc310d3e…` | 记账：Claude 与 Codex 对同一受审字节 X（SHA-256 `fdc9dc310d3e08a5cef5d2f56ffbca8153e2f90f508b1273a38fa7094b9621a1`）双审 `PASS`，计划可提交；本行、文头状态行与 Review log 的两条 PASS 行为对 X 的纯记账追加；残余 P2 登记于 Review log | `N/A` | 文头、Review log |

## 已决议设计决策

### ADR-CS-01: 默认不强制签名，保留 `commit.gpgSign` 优先于 `vault.signing` 的显式配置层级

- **Status:** Accepted
- **Context:** 当前 `libra init` 默认写入 `vault.signing=true`（#1005），导致未配置签名的用户每次 `commit` 均触发签名，且影响 `merge` / `cherry-pick` / `commit --amend`（#770）。
- **Decision:** 新仓库 `libra init` 不再写入 `vault.signing=true`（保持未设置）；已存在仓库中已写入的 `vault.signing=true` 保持不变（不做自动迁移或删除）；签名仅在显式 `-S`/`--gpg-sign`、`commit.gpgSign=true` 或显式配置 `vault.signing=true`（含 `src/internal/vault.rs:2068-2076` ADR-VG-12 首次导入密钥时显式开启）时触发。CLI 签名标志最后出现者生效并覆盖配置。
- **Alternatives considered:** 保留 `vault.signing=true` 默认值——拒绝，与 Git 及 #1005/#770 冲突。
- **Consequences:** 需同步更新 `init`、`clone`、`history_config`、`commit` `--no-gpg-sign` 帮助文案、`CLAUDE.md` 与命令文档中的默认签名表述及测试夹具（`REL-CS-00` 家族）。
- **Revisit when:** 引入仓库级强制签名策略开关时。

### ADR-CS-02: 共享 `src/command/commit.rs` 的任务卡采用线性依赖链串行推进

- **Status:** Accepted
- **Context:** 多数卡修改 `src/command/commit.rs`，按 G-10 `Implementation write set` 相交禁止并发。
- **Decision:** 「实施顺序」对全部卡建立单一有向链，实现、评审与发布均串行。
- **Alternatives considered:** 合并为巨型卡——拒绝，违反 G-01/G-04。
- **Consequences:** 串行实现、验收与发布；跨计划相交另由 `DEP-CS-07` 窗口保护。
- **Revisit when:** `commit.rs` 拆分为多子模块文件后。

### ADR-CS-03: `commit` 自动维护在共享维护锁释放后派生，参数与启用条件对齐 Git `prepare_auto_maintenance`

- **Status:** Accepted
- **Context:** `src/cli.rs:3768-3770` 在整个命令分发期间持有 `MaintenanceLock::shared`（RAII `_maintenance_lock`），`commit::execute_safe` 在该作用域内运行；Git `builtin/commit.c:1965` 在提交后调用 `run_auto_maintenance`，其 `prepare_auto_maintenance`（`run-command.c:1956-1985`）按 `maintenance.auto`（未设置时 `gc.auto` 已设置且 `<= 0` 则关闭）决定是否运行，按 `maintenance.autoDetach`（未设置回退 `gc.autoDetach`，默认 true）选择 `--detach`/`--no-detach`，按 `--quiet` 选择 `--quiet`/`--no-quiet`，并以 `run_command` 派生子进程等待其返回（`t/t7900-maintenance.sh:66-145` 以 `GIT_TRACE2_EVENT` 的 `child_start` argv 断言）。
- **Decision:** `commit` 成功写入提交后只登记一次自动维护请求（不在 `execute_safe` 内派生）；`src/cli.rs` 在 `_maintenance_lock` 作用域结束后，若存在该请求且启用条件成立，则派生子进程 `maintenance run --auto (--quiet|--no-quiet) (--detach|--no-detach)` 并等待其返回，围绕该派生写 `CS-24` 的 trace2 `child_start`（argv 首元素为 `"git"`）与 `child_exit`。`maintenance run --auto` 子进程按各任务自身条件判定是否执行（loose-objects：loose 对象总数 ≥ 100，与 Git 默认阈值及 `>=` 比较一致（`builtin/gc.c:976-1003`），但不读取 `maintenance.loose-objects.auto`；incremental-repack：pack 数 ≥ 5，Libra 既有阈值，Git 为 10 且需 multi-pack-index；gc：沿用 `gc.auto`，`<=0` 只关闭 gc 任务；未定义条件的 `prefetch`、`pack-refs`、`commit-graph` 不执行；见 `CS-25`、`CS-25c`、`CS-25h`），没有任务满足条件时立即以退出码 0 退出；`--detach` 时在判定后自行分离，再取 `MaintenanceLock::exclusive`；任何子进程取锁均带界等待（默认总时长 ≤ 30 秒、退避重试），超时以退出码 0 退出、不执行维护且不影响已完成的提交（输出内容不作约束）。预览路径（`--dry-run`/`--porcelain`/`--short`/`--long`）不登记请求。测试隔离：`LIBRA_NO_AUTO_MAINTENANCE=1` 或进程环境含 nextest 设置的 `NEXTEST_RUN_ID` 时不登记请求（后者覆盖 nextest 下任何直接或经 harness 派生的 `libra` 子进程）；公共测试 harness 对其派生的 `libra` 子进程默认注入该变量，只有专项用例显式移除并等待子进程退出；进程内调用 `commit::execute_safe` 的测试本身不经 `src/cli.rs` 派生路径。
- **Alternatives considered:** 在 `execute_safe` 内持共享锁派生并让子进程等待父锁——拒绝，`--no-detach` 前台等待会自等待死锁；在父进程内直接前台维护——拒绝，不产生 `child_start`，与 t7900 不符。
- **Consequences:** `CS-25` 提供 `maintenance run --auto`、`--no-quiet` 与 loose-objects 条件，`CS-25d` 提供 `--[no-]detach`，`CS-25c` 提供 incremental-repack 条件，`CS-25h` 提供 gc 条件，`CS-25f` 提供取锁带界等待；`CS-25a`（启用条件与派生，改 `src/command/commit.rs` 与 `src/cli.rs`）、`CS-25i`（默认分离时序与派生失败不传播，改 `src/cli.rs`）、`CS-25e`（`gc.autoDetach` 回退）与 `CS-25b`（测试与 CI 隔离）组成 `REL-CS-06` 家族同版发布，由发布点跑 `T-2`；有意差异（`LIBRA_NO_AUTO_MAINTENANCE` 与 `NEXTEST_RUN_ID` 关闭自动维护）登记 `COMPATIBILITY.md`。
- **Revisit when:** 引入常驻后台维护守护进程时。

### ADR-CS-04: 编辑器内嵌套 `libra commit` 的受限重入令牌与生命周期隔离（#784）

- **Status:** Accepted
- **Context:** 编辑器内运行嵌套 `libra commit`（#784）受阻于 `src/internal/operation/middleware.rs:411-416` 与 `src/internal/operation/middleware/lease.rs:83` 的 operation-v2 `ScopeLease` 排他租约；若直接将宽泛的 `OPERATION_SCOPE_LEASE_HELD_ENV`（`middleware.rs:22,47-48`）透传给编辑器子进程，编辑器派生的非 `commit` 写命令或编辑器退出后仍存活的后台子孙进程也会绕过 `ScopeLease`，且未校验的 index/worktree/refs 变更会污染外层 operation-log（`middleware.rs:457,714`）。
- **Decision:** 外层 `commit` 启动交互式编辑器前，签发绑定外层 `ScopeLease` 与编辑器生命周期的受限单次重入令牌（环境变量传递 `<lease_id>:<nonce>`，锁目录内有对应令牌守卫文件）。`middleware.rs`/`lease.rs` 仅当（1）子命令为 `Commands::Commit`、（2）通过原子单次认领（`O_CREAT|O_EXCL` 或原子重命名）匹配当前活跃编辑器守卫时放行，并登记认领者的不可复用进程身份（PID + 进程启动时间或等价唯一标识）。编辑器退出或外层结束时原子删除未认领令牌。已认领嵌套 `commit` 仍在运行时，外层在检查 `HEAD`/refs/index/worktree 与释放 `ScopeLease` 前带界等待其退出；超时则按登记身份终止该受控进程树并确认退出，期间持续持有租约；确认退出后检测状态变化，若有嵌套变更则外层以明确错误中止，外层 operation-log 收尾不覆盖、不回滚嵌套提交及其独立 operation-log 条目。
- **Race contract:** 认领、身份登记与编辑器退出撤销在同一进程间锁或等价原子状态机下线性化；只有身份已登记且认领状态对外可见后嵌套进程才可写入。撤销胜出则认领被拒；认领胜出则外层等待登记进程退出；不存在已放行写入却无追踪记录的窗口。
- **Crash and process identity contract:** 外层异常退出时，排他保护须以可跨进程存续的守卫维持到所有受控写入者确认退出，不依赖父进程文件句柄；身份不确定、终止或存活检查失败、子孙仍存活时保持互斥并返回可恢复错误，禁止仅凭 PID 误杀或放行。
- **Alternatives considered:** 宽泛透传 `OPERATION_SCOPE_LEASE_HELD_ENV="1"`——拒绝，会放行非 `commit` 写命令及遗留后台进程；完全释放 `ScopeLease`——拒绝，会放行外部并发写命令。
- **Consequences:** `CS-23`（令牌签发与认领）、`CS-23a`（等待、终止与进程身份）与 `CS-23b`（状态检测与 op-log 隔离）组成 `REL-CS-05` 家族，修改 `middleware.rs`、`lease.rs`、`commit.rs`，由发布点跑 `T-2` 全量。
- **Revisit when:** operation-v2 支持可重入子事务租约时。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01..13`。本计划不触及 SQLite schema（GC-13 仅以「测试使用隔离 `LIBRA_CONFIG_GLOBAL_DB`/`LIBRA_CONFIG_SYSTEM_DB`」形式适用于 FX-BASE）。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01..14`。下列本计划级协议被各卡按 ID 引用，不计入任何卡的 G-03 条目。

### 验证执行协议（V-1..V-6，对全计划每条 `Verification` 强制）

1. **V-1 测试全名：** nextest 以二进制内模块路径命名用例。`command_test`（`tests/command_test.rs` 的 `mod command`）中的用例全名为 `command::<文件模块>::<fn>`（例：`command::commit_test::x`、`command::merge_test::gpg_sign::x`）；独立 `[[test]]` compat target 中的根级用例全名为 `<fn>`，`#[path]` 子模块中的用例为 `<子模块>::<fn>`；`--lib` 用例为 `<crate 内模块路径>::tests::<fn>`。
2. **V-2 具名用例门（卡特有判据）：** 形如 `source .env.test && cargo nextest run --test <target> -E 'test(=<全名>)'`。运行前以同一 `--test`/`--lib` 与同一 `-E` 执行 `cargo nextest list`，列出的用例恰为 1 条且全名逐字一致，否则 STOP。运行后要求退出码 0、实际运行数 = 通过数 = 1、失败数 0；nextest 摘要中的 skipped 计数包含被过滤器排除的用例，不作判据。标 `(new)` 的用例须由本卡新增在所列文件中。
3. **V-3 共享回归组：** 形如 `-E 'test(/^command::<模块>::/)'` 或不带过滤器的整个 compat target。`cargo nextest list` 匹配数须 ≥ 1，运行后退出码 0、实际运行数 = 列出数 = 通过数、失败数 0。共享回归组属 ER-04 A 组 focused 强制门，列在卡内「A 组 focused 门」小节，不计入 G-03 的 Verification 计数，也不作卡特有 AC 证据（除非 Verification 条目写明「覆盖 ACn」，此时计入）。
4. **V-4 记录（被测提交 `T` 与纯记账提交）：** 本卡的通过记录以被测提交 `T`（本卡最终本地提交；家族发布点为 bump 后的提交）为准：在 `T` 上重跑全部 Verification 与 A 组 focused 门，运行前与运行后各执行一次 `libra rev-parse HEAD`、`libra rev-parse 'HEAD^{tree}'` 与 `libra status --porcelain --untracked-files=all`，两次的 `HEAD` 与树都须等于 `T` 及其树，且状态输出除本计划文件与 `docs/development/plan/plan-status.md` 外不得有任何已修改、已暂存或未跟踪条目（被忽略的构建产物不在其列），任一不满足即该次运行无效、须重跑；共享工作区无法满足时，改在仓库外检出 `T` 的独立副本中运行，对副本做同样三项检查（状态输出须为空）。记录内容（UTC 时间、完整命令、退出码、列出数/运行数/通过数/失败数、`T` 与其树 OID）写入本卡 `Current evidence`，由其后的纯记账提交承载：从 `T` 到该记账提交之间每个提交的 `libra show --name-only --format=format: <提交>` 都只含本计划文件与 `docs/development/plan/plan-status.md` 时，被测产品文件未变，不触发重跑（记账提交不必记录自身的 `HEAD`）；除此之外的任何文件变化（含 rebase、家族发布点的 bump）都须在新树上重跑，与是否发生修复无关。只能在推送、发布或 D 组之后取得的结果（Release 说明核验、run ID 与结论、`Lifecycle / Acceptance` 更新）同样以纯记账提交登记；非末端记账提交自身的 CI 结果由下一笔记账提交登记；本计划终止链末端提交（最后一笔纯记账提交）的 D 组结果按模板 v2.14 在此预登记的协议处理：不在该提交自身登记，其 SHA、run ID、结论与观察 UTC 时间由下一次 `plan-status.md` 例行同步补记（搭车既有例行同步；至下一工作日尚无例行同步时，以一笔明确的计划状态例行同步提交补记，该提交属 `plan-status.md` 维护、不属本计划终止链、不重启终止链）。提交前的运行只作过程记录。不另存日志或证据文件。
5. **V-5 失败断言口径：** 失败类 AC 统一写作「以 `<稳定错误码>` 失败」或「以 stderr 含 `<文案>` 的错误失败」，表示一个谓词：命令以非零退出结束且该唯一可观测量匹配，用例按此断言（先断言非零退出）。可观测量只取一个——稳定错误码（`LIBRA_ERROR_JSON=1` 下结构化 stderr 的 `error_code` 字段，`src/utils/error.rs:57`），或在卡的行为轴就是 Git 精确诊断文案时只断言 AC 写明的 stderr 文案；不在同一 AC 合并两者。数值退出码只在 AC 显式写出时断言（`LIBRA_FINE_EXIT_CODES` 可改变数值）。
6. **V-6 失败原子性与预览零写入（FAIL-ATOMIC）：** 凡 AC 描述的失败路径（命令以非零退出结束），以及 `commit` 的预览命令（`--dry-run`、`--porcelain`、`--short`、`--long` 及其组合，无论退出码），覆盖它的用例必须同时断言该命令前后 SNAP-EQ（AC 明写 SNAP-Δ 或另行指定快照基线时——如 `CS-23b` 以嵌套提交完成后的 S_n 为基线——按其规定）；⑤ 按 SNAP 节「失败路径的 ⑤ 口径」、⑦ 按 SNAP 节「⑦ 的统一口径」处理（失败命令可新增自身登记的 operation 记录，不推进 op heads；预览命令 ⑦ 完全相等）并按 V-4 记录；这是本协议对全部失败类与预览类 AC 的统一执行口径，与 V-2 同属全局验证协议，不另计入 G-03。

### 夹具与快照判据（FX / SNAP）

- **FX-BASE：** 每个用例在独立 tempdir 仓库运行，隔离 `HOME`、`LIBRA_CONFIG_GLOBAL_DB`、`LIBRA_CONFIG_SYSTEM_DB`（GC-13），固定 `LIBRA_COMMITTER_NAME`/`LIBRA_COMMITTER_EMAIL` 与 `GIT_*_DATE`，不得读写真实用户配置或真实 `ssh-agent`/GnuPG home。卡内「夹具」写明为区分正确实现与最可能错误实现所需的差异化条件（如作者≠提交者、`TZ` 非 UTC、多来源配置、未配置时的对照）。
- **SNAP（全量快照）：** 对仓库在操作前后各取一次快照 S，S 由以下分量组成，由共用 helper `tests/helpers/snapshot.rs`（`CS-01a` 交付，其交付与负向自检见下文「SNAP helper 交付门」）以同源 Rust API 或 CLI 读取：①仓库 ref 存储的全部记录（SQLite `reference` 表全量行，覆盖 `refs/heads`、`refs/remotes`、`refs/tags`、`refs/stash` 等全部命名空间，不得以只枚举 heads/remotes/tags 的 `show-ref`/`for-each-ref` 输出代替）及 `HEAD` 的符号目标；②`reflog` 表全量条目；③索引：经索引读取 API 记录每个条目除 stat 缓存字段（ctime、mtime、dev、ino、uid、gid、文件大小缓存）之外的全部字段与标志（mode、OID、stage、path、assume-valid、skip-worktree、intent-to-add 及其它扩展标志），以及索引扩展段的存在与内容；④工作区（排除 `.libra/`）以不跟随符号链接的元数据（`symlink_metadata`）枚举的全部路径集合，及每个路径的类型（普通文件/符号链接/目录）、普通文件的字节与可执行位、符号链接的目标字节；⑤全量对象集合（OID、类型、大小）：由 helper 严格枚举全部 loose 对象文件与 `objects/pack/*.idx`，任一索引或对象头不可读、不可解析即快照失败（不得使用会静默跳过不可解析 pack index 的 `libra cat-file --batch-check --batch-all-objects`，`src/command/cat_file.rs:820,873-878`）；⑥本仓库配置存储：SQLite `config_kv` 表全量行（`id`、`key`、`value`（加密项为密文原样）、`encrypted`）与旧 `config` 表全量行，只读直接读取、不解密（不得以把加密值显示为 `<REDACTED>` 的 `libra config list` 代替，`src/command/config.rs:3879-3880`）；⑦operation-log：直接读取 SQLite 表 `operation`、`operation_parent`、`operation_head`（含 `repo_id`、`scope_key`、`op_id`、`generation` 的完整行，不去重、不截断）与 `operation_journal` 的全量行（`sql/migrations/2026090101_operation_v2.sql`），以及工作区 operation 指针文件 `.libra/info/operation-pointer.json` 的字节（`src/internal/operation/working_copy.rs:17,75-76`）；⑧对象物理布局：全部 loose 对象文件的路径集合及各文件字节的 SHA-256，以及 `objects/pack/` 下 pack 与 idx 文件名集合及各文件的 SHA-256（区分对象是 loose 还是已打包，⑤ 只比较逻辑对象集合）；⑨notes 映射：SQLite `notes` 表全量行（`id`、`notes_ref`、`object`、`blob`；Libra 的 notes 只以该表映射加 blob 存储，不建立 `refs/notes/*` ref、notes 树或提交对象，`src/internal/notes.rs:1-5`、`sql/migrations/2026061401_notes.sql`）。不得以 `OperationStore::read_all_heads`（`SELECT DISTINCT … LIMIT 200`，`src/internal/operation/store.rs:965-975`）一类去重或截断的接口代替。**SNAP-EQ** = 前后 S 逐分量完全相等；**SNAP-Δ(X)** = 只有 AC 写明的分量 X 按写明方式变化，其余分量完全相等。**⑦ 的统一口径：** 除预览命令外，SNAP-EQ 与 SNAP-Δ 对 ⑦ 都只放宽一处——被测命令自身登记的 operation（`metadata.command_name` 为被测命令、在本次运行中创建；若基线取于命令运行中途，则含基线时已处于 `running` 的该条记录）可以新增或由 `running` 转为终态，并可新增其 `operation_parent` 与 `operation_journal` 行；仅当被测命令成功完成时，`operation_head` 与 operation 指针文件可按该 operation 推进。失败命令不得改变 `operation_head` 与指针文件；其余 ⑦ 内容必须不变，AC 不重复列出该放宽。预览命令（V-6）的 ⑦ 必须完全相等。**⑧ 的统一口径：** SNAP-Δ 中 ⑧ 只允许出现与 ⑤ 的允许变化一一对应的 loose 文件增删（新对象以 loose 形式写入），既有 loose 文件的 SHA-256 与 pack/idx 集合不变；AC 明写打包或清理效果的维护场景按其 AC 判定，不使用 SNAP-EQ 代替。**失败路径的 ⑤ 口径：** 被测命令以非零退出结束时，SNAP-EQ 与 SNAP-Δ 对 ⑤ 只放宽一处——允许新增 blob 与 tree 对象（及 ⑧ 中与之一一对应的新 loose 文件），不允许新增 commit 或 tag 对象；其余分量（含 ① ref、③ 索引与 ⑨ notes）照常比较，因而这些对象不被任何 ref、索引条目或 notes 行引用（与 Git 在钩子、编辑器、签名失败或交互暂存后中止时留下不可达 blob/tree 一致）；成功命令与预览命令无此放宽，AC 不重复列出该放宽。**⑥ 的统一口径：** 被测命令为 `maintenance run` 且以退出码 0 结束时，SNAP-EQ 与 SNAP-Δ 对 ⑥ 只放宽本仓库键 `maintenance.last-run` 的新增或值变化（成功运行记录时间戳，`src/command/maintenance.rs:312-319` 的既有行为，本计划不改；没有任务满足条件时是否记录不作约束），其余键值必须不变，AC 不重复列出该放宽；取于命令运行中途的快照与其它被测命令的 ⑥ 均无此放宽。
- **SNAP helper 交付门（V-6 基础设施，随 `CS-01a` 交付；其自检用例是 `CS-01a` 的独立验证门，计入该卡 G-03 Verification（V2））：** `tests/helpers/snapshot.rs` 实现 ①–⑨ 与上述各统一口径（失败路径的 ⑤、⑥、⑦、⑧），并由 `command::snapshot_selftest::snapshot_detects_single_component_changes` 逐项负向自检：分别只改变某个 `refs/tags/*` ref 的目标、只改变 `notes` 表某一行的 `blob`、把工作区普通文件换成同内容符号链接、只改变某个 `operation_head` 行的 `generation`、只追加一条 operation 行、只改写 operation 指针文件后，helper 每次都判定两次快照不相等；把一个 `objects/pack/*.idx` 改为不可解析的字节后，helper 取快照以错误失败（不得返回部分对象集合）；只改变某个索引条目的 skip-worktree 标志、只改变某个条目的 intent-to-add 标志、只把一个 loose 对象移入新 pack（⑤ 不变）、只以不同 zlib 压缩级别重写某个 loose 对象文件（路径与解压内容不变）后，helper 判定两次快照不相等；只改写某个 pack 文件末尾 trailer 校验和中的一个字节（文件名与对象头不变）后，helper 判定两次快照不相等或以错误失败（不得判定相等）；在失败口径下只新增一个 blob 时判定相等、只新增一个 commit 对象时判定不相等；只替换一个既有加密配置行（`encrypted=1`）的密文后判定不相等；只改变本仓库 `maintenance.last-run` 的值时，默认口径判定不相等、⑥ 放宽口径判定相等，在放宽口径下另改一个其它配置键时判定不相等；该用例按 V-2 执行并按 V-4 记录。只比计数或抽查部分对象不构成 SNAP 判据。
- **MSG（消息字节规范化）：** 提交对象中头部之后空行以下的消息字节，去掉至多一个末尾 `\n`。Libra `cleanup_commit_message` 以 `join("\n")` 产出、不带 Git 的末尾换行（`src/command/commit.rs:2247-2273`），该存储差异不在本计划范围；凡与 pinned Git 实测常量比较消息字节的 AC 一律比较双方的 MSG，既消除该既有差异，又能检出多余的尾随空行。`commit-tree`（`CS-21`）比较完整对象字节，不适用 MSG。

### 文档门（D-1，ER-06/ER-06a 同卡必需同步，不计入 G-03）

对卡内 `Docs and compatibility impact` 列出的每个仓库内文件：改动前后在进程内计算 `shasum -a 256 <file>` 并把两值抄入本卡 `Current evidence`；用户可见的 `--help` 文本同属 ER-06 必需同步（以 `rg -F` 对 `libra <cmd> --help` 输出验收关键句或零命中）。对每个文件登记至少一条本卡行为关键句，`rg -n -F '<关键句>' <file>` 退出码 0 才算交付（退出码 1 = 缺失即 STOP，>1 = 命令失败即 STOP）；EN 与 zh-CN 两份同时满足。对每个文件中被本卡行为推翻的既有陈述（如 `COMPATIBILITY.md:209` 的「keyid 被忽略」「部分提交被拒绝」），就地替换或删除并登记其旧关键句，`rg -n -F '<旧关键句>' <file>` 退出码 1 才算交付（0 = 旧陈述仍在即 STOP，>1 = 命令失败即 STOP）；无被推翻陈述时在本卡 `Current evidence` 写明「无」。`COMPATIBILITY.md` 只在既有条目行内修改（追加新陈述或就地替换旧陈述），不增删行，改动前后 `wc -l COMPATIBILITY.md` 相等。

### 网站交付协议（W-1..W-6，ER-06a 落地；不写任何快照或证据文件）

网站检出 `B=/Volumes/Data/gitmono/libra-backend`（用户 2026-11-18 11:55:09 UTC 确认；见「文档职责」）。网站文件只有 `apps/tanstack-app/content/docs/commands/<cmd>.en.md`，其内容由后端脚本从本仓库 `docs/commands/<cmd>.md` 同步。

1. **W-1 预检：** `test -d $B/.libra && test ! -e $B/.git`；`cd $B && libra status --short --branch` 首行分支为 `cf`（否则按 ER-06a 执行 `libra switch cf` 后重查，切换受阻即 STOP，与「文档职责」一致）；`libra fetch origin cf` 后 `libra rev-parse cf` 等于 `libra rev-parse refs/remotes/origin/cf`（本地无未推送提交；本地落后时执行 `libra pull --ff-only origin cf` 后重查，超前或分叉即 STOP，不推送他人提交）；网站工作区暂存区为空（`libra status --porcelain --untracked-files=no` 每行首列均为空格；Libra 提交整个暂存区）；对本卡每个网站文件：`libra ls-files --error-unmatch -- <file>` 退出码 0（受版本控制）、`libra check-ignore -q --no-index -- <file>` 退出码 1（忽略规则不匹配该路径）、`libra status --short --untracked-files=all -- <file>` 输出为空（无未提交或未跟踪改动），任一不满足即 STOP，不覆盖；随后在 shell 变量中记录基线 `pre="$(libra status --short --untracked-files=all)"`，并对 `pre` 列出的每个本卡写集外路径在变量中按类型记录指纹：路径不存在（如删除）记为 `absent`，符号链接记录 `readlink` 目标，普通文件记录 `shasum -a 256`（不跟随链接、不写文件；其它计划的既有无关改动按后端 `AGENTS.md` 保留，不处理）。
2. **W-2 同步：** `cd $B && python3 scripts/sync-libra-command-docs.py --libra-root ../libra --write --only <cmds>`，随后 `python3 scripts/sync-libra-command-docs.py --libra-root ../libra --only <cmds>` 退出码 0；再取 `post="$(libra status --short --untracked-files=all)"`：`post` 中本卡网站写集以外的行必须与 `pre` 中同类行逐行相同（不新增、不消失），且这些路径按同一口径重算的指纹与 W-1 记录值相同，否则 STOP；对每个网站文件 `rg -n -F '<D-1 登记的 EN 关键句>' <file>` 退出码 0。**撤回分支（故障恢复前滚撤回时）：** 同步源为撤回后的仓库文档；对每个网站文件，被撤回的 EN 句 `<R>` 的 `rg` 须为 1、应保留的 EN 句 `<K>` 的 `rg` 须为 0（取代上述「D-1 登记的 EN 关键句」检查）；W-3..W-5 对撤回后的内容照常执行。
3. **W-3 前端验证：** `cd $B && pnpm typecheck` 与 `pnpm build` 均退出码 0（后端 `AGENTS.md` Verification 清单）。
4. **W-4 网站评审：** 对 `cd $B && libra diff -- <files>` 的最终内容取得 Claude Code `VERDICT: PASS`；把每个网站文件的 `shasum -a 256` 值抄入本卡 `Current evidence`。
5. **W-5 提交：** 提交前重算每个网站文件 SHA-256 并与 W-4 记录逐一相等（不等则回到 W-2）；`libra add <files>`（精确路径）→ `libra commit -s -m "docs(commands): <cmd> ..."` → 按 ER-07 对同一 `COMMIT_SHA` 校验 `gpgsig` 与 `Signed-off-by`；`libra show --name-only --pretty=format: "$COMMIT_SHA"` 退出码为 0 且其路径集合等于本卡网站写集；对每个文件在 bash 下执行 `libra show "$COMMIT_SHA:<file>" | shasum -a 256`，以 `PIPESTATUS` 要求两段退出码均为 0（否则为命令错误即 STOP）后，哈希须等于 W-4 记录值。
6. **W-6 推送与部署：** 推送前执行 `libra fetch origin cf`，要求 `libra rev-parse "$COMMIT_SHA^"` 等于 `libra rev-parse refs/remotes/origin/cf` 且 `libra rev-parse cf` 等于 `COMMIT_SHA`（待推送历史恰为本次提交；远端已前进或本地含其它未推送提交即 STOP）；`libra push origin cf` 成功后 `libra ls-remote origin refs/heads/cf` 的 OID 等于 `COMMIT_SHA`（远端已前进则按 ER-09 处理，不 force）。部署核验与本次提交绑定，所有比对都在管道内完成、不保存响应，且一律在 bash 下以 `PIPESTATUS` 分别判定上下游：上游 `libra show`/`curl -fsS` 的退出码须为 0（下游 `rg -q` 命中后提前退出造成的 141 视为读取成功），否则为命令错误即 STOP；下游 `rg -q -F` 按 0 = 命中、1 = 不含、>1 = 命令错误判定。**新增分支（默认）：** 对每个网站文件选一条本次提交新增的 EN 句 `<N>`，要求 `libra show "$COMMIT_SHA^:<file>" | rg -q -F '<N>'` 的 `rg` 为 1、`libra show "$COMMIT_SHA:<file>" | rg -q -F '<N>'` 的 `rg` 为 0、`curl -fsS https://libra.tools/en/docs/commands/<cmd> | rg -q -F '<N>'` 的 `rg` 为 0（路由以 `apps/tanstack-app/src/routes/$lang/(root)/docs.$.tsx` 为准）。**撤回分支（故障恢复前滚撤回时）：** 对每个网站文件选一条被撤回的 EN 句 `<R>` 与一条应保留的 EN 句 `<K>`，要求 `<R>` 在 `$COMMIT_SHA^` 中 `rg` 为 0、在 `$COMMIT_SHA` 与线上页面中 `rg` 均为 1，`<K>` 在 `$COMMIT_SHA` 与线上页面中 `rg` 均为 0。未生效按 ER-10 暂时性错误有界重试（≤ 5 次、总 ≤ 30 分钟），超预算置 `blocked`。

**时序：** ER-06a 要求网站文档与行为在同一张任务卡交付，故每张网站写集非空的实现卡都在本卡内完整执行 W-1..W-6：`independent` 卡在本卡 CLI 发布（ER-04 C 组第 ⑨ 步）之后执行；`family child` 在本卡 ER-05 PASS 与本地签名提交之后执行（网站提交随即推送 `cf` 并核验部署，不等待家族发布点）。家族发布点不改写也不重新提交网站文件，不承担网站门；同一家族后继子卡若改同一网站文件，以前序子卡已推送的版本为基线重新执行 W-1..W-6。家族子卡的网站页面因此可能早于家族 CLI 发布公开，该时差登记在「风险登记」。网站写集为空的卡不执行 W。

## 实施顺序

依赖边 `A -> B` 表示 A 必须先于 B；全图为下列单链，各卡 `Dependencies` 中的本计划任务 ID 与链上前驱逐一一致（G-06）。家族内后继子卡依赖前序子卡的 A/B 通过、ER-05 PASS 与本地签名提交，不等待家族发布点。

- `CS-01 -> CS-01a -> CS-02 -> CS-02a -> REL-CS-00 -> CS-03 -> CS-03f -> CS-03a -> CS-03j -> CS-03d -> CS-03g -> CS-03l -> REL-CS-07 -> CS-03b -> CS-03c -> CS-03h -> CS-03i -> CS-03k -> CS-03e -> REL-CS-01 -> CS-04 -> CS-05 -> CS-06 -> CS-08 -> CS-10 -> CS-11 -> CS-12 -> CS-12a -> CS-13 -> CS-14 -> CS-14a -> CS-15 -> CS-15h -> CS-15d -> CS-15f -> REL-CS-03 -> CS-15a -> CS-15b -> CS-15c -> CS-15g -> CS-15e -> REL-CS-04 -> CS-09 -> CS-09b -> CS-09a -> CS-09c -> REL-CS-02 -> CS-07 -> CS-07a -> CS-07b -> REL-CS-09 -> CS-16 -> CS-17 -> CS-17a -> CS-18 -> CS-18b -> CS-18a -> CS-18c -> CS-19 -> CS-19c -> CS-19b -> CS-19a -> REL-CS-08 -> CS-20 -> CS-21 -> CS-21a -> CS-22a -> CS-23 -> CS-23a -> CS-23b -> REL-CS-05 -> CS-24 -> CS-25 -> CS-25g -> CS-25d -> CS-25c -> CS-25h -> CS-25f -> CS-25a -> CS-25i -> CS-25e -> CS-25b -> REL-CS-06`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| `DEP-CS-01` | `incoming` | 跨计划 | `plan-20261117-merge-pull.md#MP-05` | Merge/Pull 计划执行者 | `MP-05` 的 `SQUASH_MSG`（含 `# Conflicts:` 列表）已合入 `main`；`CS-14a` 在其上验证 `scissors` 对 `SQUASH_MSG` 的保留语义（`MERGE_MSG` 由 `CS-14a` 自建夹具，不依赖 `MP-05`） | 对方卡 `MP-05`（按 ID 核对） | 7 日；未就绪时 `CS-14a` 置 `blocked`，超时升级 Coordinator |
| `DEP-CS-02` | `incoming` | 跨计划 | `plan-20261117-merge-pull.md#MP-26`、`#MP-10` | Merge/Pull 计划执行者 | `MP-26` 对 `src/command/history_config.rs` 的修改已合入 `main` 后才开工 `CS-02a`；`MP-10` 对 `src/command/commit.rs` 的修改已合入 `main` 后才开工 `CS-03`（对方顺序为 `REL-CS-00 -> MP-08 -> MP-10 -> CS-03`） | 对方卡 `MP-26`、`MP-10` | 7 日；未合入前 `CS-02a`/`CS-03` 置 `blocked`，超时升级 Coordinator |
| `DEP-CS-03` | `incoming` | 跨计划（start-to-completion） | `plan-20261117-clone.md` 的 `DEP-CL-03` 集合 | Clone 计划执行者 | 逐字镜像对方 `DEP-CL-03`：仅 `CL-01..CL-10`、`CL-12..CL-16`、`CL-18..CL-19`、`CL-21`、`CL-23..CL-29`（含 `CL-25` 家族发布点）合入 `main` 后再开工 `CS-02`、`CS-02a`、`CS-15b`、`CS-21a`；不含已撤回的 `CL-11`/`CL-22` 及为避免经 `CS-02a` 回环而排除的 `CL-20` 与依赖它的 `CL-17` | 对方 `DEP-CL-03` | 7 日；未合入前对应卡置 `blocked` |
| `DEP-CS-04` | `incoming` | 跨计划（start-to-completion） | `plan-20261117-plumbing-misc.md#PM-02`、`#PM-09`、`#PM-10`、`#PM-14` | Plumbing-Misc 计划执行者 | `PM-02`（`config.rs` 与 config 文档）合入 `main` 后才取得 `REL-CS-00` 家族窗口并开工 `CS-02`/`CS-02a`（对方 `DEP-PM-07` 镜像），链上后继 `CS-03` 一并满足；`PM-10`（`src/cli.rs`、`tests/command/mod.rs`）合入后开工 `CS-15b`；`PM-09`（`notes.rs`）合入后开工 `CS-19`；`PM-14`（`cat_file.rs`）合入后开工 `CS-21a` | 对方卡 ID 与 `DEP-PM-07` | 7 日；未合入前对应卡置 `blocked` |
| `DEP-CS-05` | `incoming` | 跨计划（start-to-completion） | `plan-20261117-checkout-switch-restore.md#CSR-01`、`#CSR-02`、`#CSR-27` | Checkout/Switch/Restore 计划执行者 | 三卡对 `src/cli.rs` 的修改已合入 `main` 后再开工 `CS-08`（本计划首张改 `src/cli.rs` 的卡；其后的 `src/cli.rs` 写者如 `CS-15b` 经实施链排在其后，同样以此为前置） | 对方卡 ID | 7 日；未合入前 `CS-08` 与其后的 `src/cli.rs` 写者置 `blocked` |
| `DEP-CS-06` | `incoming` | 跨计划（start-to-completion） | `plan-20261117-global-init-infra.md#GII-01..GII-09`、`#GII-15`、`#GII-16`、`#GII-19`、`#GII-20`、`#GII-22`、`#GII-24`（对方 `DEP-GII-03` 为对应的同批写集窗口） | Global-Init-Infra 计划执行者 | 按对方 2026-11-18 已推送版本（`f3316d9`），上述卡修改 `src/cli.rs`（GII-01/02/03/04/05/09/19/20/22/24）、`src/command/init.rs` 与 `tests/command/init_test.rs`（GII-06..09）、`init.md`/`config.md`、`src/command/add.rs` 与对象索引（GII-15/16）；它们全部合入 `main` 后再开工 `CS-02`（链上后继卡因此一并满足）。对方计划若再增减写这些文件的卡，以开工当日对方文本与 `DEP-CS-07` 的动态写集判定为准 | 对方卡 ID 与 `DEP-GII-03` | 7 日；未合入前 `CS-02` 置 `blocked` |
| `DEP-CS-07` | `incoming` | 跨计划写集串行窗口（Coordinator） | 全部 15 份同日 `plan-20261117-*.md` | Coordinator 全局串行调度 | 本计划每张实现卡开工与评审前，Coordinator 按各同日计划当时的 `Implementation write set` 判定与本卡写集的相交（含 `COMPATIBILITY.md`、`src/cli.rs`、`src/command/commit.rs`、网站 `.en.md` 等全部文件）并分配独占窗口：相交的他计划卡须已合入 `main`（网站文件须已推送 `cf`）或尚未开工，窗口期内他计划不得写相交文件。家族窗口：`REL-CS-00`..`REL-CS-09` 各家族在取得窗口前须先满足全家族子卡的全部家族外前置（如 `REL-CS-00` 的 `DEP-CS-02`/`DEP-CS-03`/`DEP-CS-04`/`DEP-CS-06`/`DEP-CS-09`），自首张子卡开工起连续持有至发布点合入 `main` 并取得 D 组结果（各子卡的 W-6 已在子卡内完成）；家族内前序子卡的本地签名提交可作为后继子卡基线而无需先合入 | Coordinator 窗口登记（时间、窗口 ID、相交文件）写入本卡 `Current evidence` | 7 日；未取得窗口的卡置 `blocked`，严禁跨计划并发实现与评审相交文件（G-10） |
| `DEP-CS-08` | `incoming` | 跨计划（start-to-completion） | `plan-20261117-diff.md#DIFF-45` | Diff 计划执行者 | `DIFF-45` 对 `src/utils/pathspec/mod.rs` 的修改已合入 `main` 后再开工 `CS-15`（链上后继 `CS-15c` 一并满足） | 对方卡 ID | 7 日；未合入前 `CS-15` 置 `blocked` |
| `DEP-CS-09` | `incoming` | 跨计划传递前置（说明性） | `issues/1048.md#I1048-05` → `plan-20261117-plumbing-misc.md` 内 `PM-02` 的前置链（对方 `DEP-PM-05`） | 仓库发布协调者 | `PM-02` 的前置链要求 `I1048-05` 先完成；`CS-02` 开工前核对 `I1048-05` 与 `PM-02` 在各自计划中为 `done`/`complete` 且其提交在 `main`（以对方计划卡状态与 `plan-status.md` 为准，不复写对方开工门）。2026-11-18 起草时 `I1048-05` 为 `pending`，故本链当前未满足 | 对方卡 `I1048-05`、`PM-02` 的 `Lifecycle / Acceptance` | 不设本计划内超时；未满足前 `CS-02`（及 `REL-CS-00` 家族窗口）置 `blocked`，计划 PASS 不代表依赖已满足 |
| `DEP-CS-10` | `outgoing` | 跨计划 | `plan-20261117-merge-pull.md#MP-08`（对方登记 `DEP-MP-02`） | Merge/Pull 计划执行者（接收方） | `REL-CS-00` 合入 `main`（联合交付 `CS-02` 默认不签名与 `CS-02a` 夹具）后供 `MP-08` 消费；`CS-02`/`CS-02a` 不单独推送 | 对方 `DEP-MP-02` | 不阻塞本计划；`REL-CS-00` 延误时由 Coordinator 通知接收方继续等待 |
| `DEP-CS-11` | `outgoing` | 跨计划 | `plan-20261117-sequencer-am.md` 中登记 `DEP-SA-01` 的卡（接收方） | Sequencer/AM 计划执行者 | `REL-CS-00` 合入 `main`：默认不签名与 `tests/command/cherry_pick_test.rs` 的 `CS-02a` 修改可供消费 | 对方 `DEP-SA-01` | 不阻塞本计划 |
| `DEP-CS-12` | `outgoing` | 跨计划 | `plan-20261117-sequencer-am.md` 中登记 `DEP-SA-02` 的卡（当前为 `SA-08g`，接收方） | Sequencer/AM 计划执行者 | `REL-CS-01`（`CS-03b` SSH 签名生成、`CS-03c` SSH 验签、`CS-03e` 签名侧 fail-closed 诊断）合入 `main` 并发布；`REL-CS-01` 的 ID 与「同版发布 SSH 签名、验签与 fail-closed 诊断」语义保持不变以供对方引用（R31 把原 `CS-03c` 的签名侧诊断拆为 `CS-03e`，仍在同一家族内） | 对方 `DEP-SA-02` | 不阻塞本计划 |
| `DEP-CS-13` | `outgoing` | 跨计划写集串行 | `plan-20261117-sequencer-am.md#SA-07d`（对方登记 `DEP-SA-05`） | Sequencer/AM 计划执行者 | `REL-CS-06`（含 `CS-25a`、`CS-25e` 对 `src/command/commit.rs` 与 `src/cli.rs` 的修改）合入 `main` 后对方再开工 `SA-07d` | 对方 `DEP-SA-05` | 不阻塞本计划 |

### 发布分组与并发窗口

| 家族 ID | 成员（按顺序） | 唯一发布点 | 窗口与不提前推送规则 | 失败回滚顺序 | 理由 |
|---|---|---|---|---|---|
| `REL-CS-00` | `CS-02` (1/2), `CS-02a` (2/2) | `REL-CS-00` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-02a` → `CS-02` 本地 `revert`；发布后按 `immutable-release` 前滚 | `CS-02` 改 `init` 默认后 `clone_cli_test.rs:742-752` 等夹具须由 `CS-02a` 同版更新 |
| `REL-CS-07` | `CS-03a` (1/5), `CS-03j` (2/5), `CS-03d` (3/5), `CS-03g` (4/5), `CS-03l` (5/5) | `REL-CS-07` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-03l` → `CS-03g` → `CS-03d` → `CS-03j` → `CS-03a` 本地 `revert`；发布后按 `immutable-release` 前滚 | 外部签名（`CS-03a`）、外部程序验签（`CS-03d`）与 merge 信任门（`CS-03g`）须同版发布：否则中间版本或生成本工具无法验证的签名，或 `merge --verify-signatures` 放行已导入但未受信任的签名 |
| `REL-CS-01` | `CS-03b` (1/6), `CS-03c` (2/6), `CS-03h` (3/6), `CS-03i` (4/6), `CS-03k` (5/6), `CS-03e` (6/6) | `REL-CS-01` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-03e` → `CS-03k` → `CS-03i` → `CS-03h` → `CS-03c` → `CS-03b` 本地 `revert`；发布后按 `immutable-release` 前滚 | `CS-03b`、`CS-03c`、`CS-03h`、`CS-03i`、`CS-03k`、`CS-03e` 为 `family child`，经唯一 `release` 卡统一发布（ID 与「SSH 签名与验签」语义不变）；对方计划 `DEP-SA-02` 以本卡 ID 消费 |
| `REL-CS-03` | `CS-15` (1/4), `CS-15h` (2/4), `CS-15d` (3/4), `CS-15f` (4/4) | `REL-CS-03` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-15f` → `CS-15d` → `CS-15h` → `CS-15` 本地 `revert`；发布后按 `immutable-release` 前滚 | `CS-15` 启用部分提交后必须与 `CS-15d` 的预览与守卫同版发布 |
| `REL-CS-04` | `CS-15c` (1/3), `CS-15g` (2/3), `CS-15e` (3/3) | `REL-CS-04` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-15e` → `CS-15g` → `CS-15c` 本地 `revert`；发布后按 `immutable-release` 前滚 | `CS-15g` 接入 `--pathspec-from-file` 后必须与 `CS-15e` 的互斥守卫同版发布 |
| `REL-CS-02` | `CS-09` (1/4), `CS-09b` (2/4), `CS-09a` (3/4), `CS-09c` (4/4) | `REL-CS-02` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-09c` → `CS-09a` → `CS-09b` → `CS-09` 本地 `revert`；发布后按 `immutable-release` 前滚 | `CS-09` 解禁 `--interactive` 后必须与 `CS-09a` 菜单同版发布，避免中间版本空悬选项 |
| `REL-CS-09` | `CS-07` (1/3), `CS-07a` (2/3), `CS-07b` (3/3) | `REL-CS-09` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-07b` → `CS-07a` → `CS-07` 本地 `revert`；发布后按 `immutable-release` 前滚 | `reword:` 语义（`CS-07`）与其冲突守卫（`CS-07a`、`CS-07b`）须同版发布，否则中间版本会忽略路径、暂存选项或 `-m` 而生成非预期提交 |
| `REL-CS-08` | `CS-19` (1/4), `CS-19c` (2/4), `CS-19b` (3/4), `CS-19a` (4/4) | `REL-CS-08` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-19a` → `CS-19b` → `CS-19c` → `CS-19` 本地 `revert`；发布后按 `immutable-release` 前滚 | 单 ref 复制（`CS-19`）、多值/glob（`CS-19b`）与锁与基线比较失败语义（`CS-19a`）须同版发布，避免中间版本漏复制或在锁竞争下覆盖并发 notes |
| `REL-CS-05` | `CS-23` (1/3), `CS-23a` (2/3), `CS-23b` (3/3) | `REL-CS-05` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-23b` → `CS-23a` → `CS-23` 本地 `revert`；发布后按 `immutable-release` 前滚 | 三张子卡须同版发布以保证令牌认领、等待/终止与状态隔离一致 |
| `REL-CS-06` | `CS-25a` (1/4), `CS-25i` (2/4), `CS-25e` (3/4), `CS-25b` (4/4) | `REL-CS-06` | 子卡只做本地签名提交，不推送 Libra 仓库、不 bump；网站文档按 ER-06a 由各子卡在本卡内完成 W-1..W-6；窗口期禁止插入其它切片；发布点跑 `T-2` 全量并 `patch + 1` | 推送前按 `CS-25b` → `CS-25e` → `CS-25i` → `CS-25a` 本地 `revert`；发布后按 `immutable-release` 前滚 | 启用自动维护（`CS-25a`）、默认分离时序与派生失败不传播（`CS-25i`）、`gc.autoDetach` 回退（`CS-25e`）与测试/CI 隔离（`CS-25b`）须同版发布，避免全量门与 CI 在中间版本派生维护进程 |

其余 35 张 `implementation` 卡均为独立发布切片（G-07、`Release boundary=independent`）。

**并发声明:** 按 G-10 全链串行实现与发布；各 `family child` 在对应 `REL-CS-*` 完成前禁止单独推送或 bump。
**发布者:** Coordinator 指定的单一发布者（ER-12），负责 C 组 bump、构建、安装、提交、推送、`gh release create`、W-5/W-6 与 D 组跟踪。
**发布窗口顺序:** 严格按「实施顺序」逐卡串行发布；家族子卡由其发布点在链上位置统一发布。

### Phase 0: CLI 签名参数、默认不签名与 GPG/SSH 签名链路（`CS-01`..`CS-04`，含 `CS-03d`..`CS-03g`、`REL-CS-00`/`REL-CS-07`/`REL-CS-01`）

**目标:** 完成 `-S<keyid>` 解析与选钥、默认不签名对齐（`REL-CS-00`）、`gpg.format` 校验、`gpg.program` 解析/签名/验签、SSH 签名/验签/诊断（`REL-CS-01`）与 `--amend` 签名丢弃语义。
**进入条件:** 本计划文本双审 PASS 已提交；`CS-01` 需 `DEP-CS-07` 窗口；`CS-02` 起另受 `DEP-CS-03`、`DEP-CS-04`、`DEP-CS-06`、`DEP-CS-09` 门控，`CS-02a` 另需 `DEP-CS-02`。
**退出条件:** `CS-01`、`CS-01a`、`CS-02`、`CS-02a`、`REL-CS-00`、`CS-03`、`CS-03f`、`CS-03a`、`CS-03j`、`CS-03d`、`CS-03g`、`CS-03l`、`REL-CS-07`、`CS-03b`、`CS-03c`、`CS-03h`、`CS-03i`、`CS-03k`、`CS-03e`、`REL-CS-01`、`CS-04` 全部 `done` 且 `complete`。

### Phase 1: CLI 消息源、预览与模板/Cleanup（`CS-05`..`CS-14a`）

**目标:** 补齐重复 `-m`（含 `--fixup -m`）、`--squash -F/-m`、`--long`、`-v` 尾行、`-F` UTF-8、`--signoff` 去重与空消息校验、`-t verbatim`、`--cleanup=whitespace/scissors`。
**进入条件:** `CS-04` 完成。
**退出条件:** `CS-05`、`CS-06`、`CS-08`、`CS-10`、`CS-11`、`CS-12`、`CS-12a`、`CS-13`、`CS-14`、`CS-14a` 全部 `done` 且 `complete`（`CS-07`、`CS-07a`、`CS-09`、`CS-09a` 属 Phase 2）。

### Phase 2: 部分提交、`--interactive`、`--fixup=amend:/reword:` 与模板可选路径（`CS-15`..`CS-16`，含 `REL-CS-03`/`REL-CS-04`/`REL-CS-02`/`REL-CS-09`）

**目标:** 依次实现 `<pathspec>`/`--only` 与预览/无匹配守卫（`REL-CS-03`）、`--include`、`--patch`、`--pathspec-from-file` 与 `#786` 诊断（`REL-CS-04`）、`--interactive`（`REL-CS-02`）、`--fixup=amend:/reword:` 与 `--template :(optional)`。
**进入条件:** `CS-14a` 完成。
**退出条件:** `CS-15`、`CS-15h`、`CS-15d`、`CS-15f`、`REL-CS-03`、`CS-15a`、`CS-15b`、`CS-15c`、`CS-15g`、`CS-15e`、`REL-CS-04`、`CS-09`、`CS-09b`、`CS-09a`、`CS-09c`、`REL-CS-02`、`CS-07`、`CS-07a`、`CS-07b`、`REL-CS-09`、`CS-16` 全部 `done` 且 `complete`。

### Phase 3: 日期、作者、输出、Amend 索引/Notes 与对象字节精度（`CS-17`..`CS-21a`，含 `REL-CS-08`）

**目标:** 补齐 `approxidate` 复用与无时区 ISO、`--amend --author` 日期、`--reset-author` 约束、`Author:`/`Date:` 输出、`notes.rewrite.amend`、amend 索引一致性与 `commit-tree`/`cat-file` 精确字节修复。
**进入条件:** `CS-16` 完成。
**退出条件:** `CS-17`、`CS-17a`、`CS-18`、`CS-18b`、`CS-18a`、`CS-18c`、`CS-19`、`CS-19c`、`CS-19b`、`CS-19a`、`REL-CS-08`、`CS-20`、`CS-21`、`CS-21a` 全部 `done` 且 `complete`。

### Phase 4: Hooks、嵌套操作锁、`trace2` 与自动维护（`CS-22a`..`CS-25b`，含 `REL-CS-05`、`REL-CS-06`）

**目标:** 完成 `prepare-commit-msg` 参数对齐与 hooks 的 PD-08 收口、编辑器内嵌套 commit 锁解除与超时安全恢复（`REL-CS-05`）、`trace2` 事件子系统、`maintenance run --auto` 与 commit 自动维护及其测试隔离（`REL-CS-06`）。
**进入条件:** `CS-21a` 完成。
**退出条件:** `CS-22a`、`CS-23`、`CS-23a`、`CS-23b`、`REL-CS-05`、`CS-24`、`CS-25`、`CS-25g`、`CS-25d`、`CS-25c`、`CS-25h`、`CS-25f`、`CS-25a`、`CS-25i`、`CS-25e`、`CS-25b`、`REL-CS-06` 全部 `done` 且 `complete`，且计划收口全量门通过。

## 任务卡

**废弃编号：** `CS-22`（R55 撤回：原计划把 `--no-verify` 改为只跳过 `pre-commit`/`commit-msg`，与 Libra 把 `--no-verify` 固定为跳过全部 hooks 的有意设计冲突，`tests/compat/libra_hooks_lifecycle_test.rs:163-172`；PD-08 收口并入 `CS-22a`，对齐 Git 登记 `DEFER-CS-09`，编号不复用）；`CS-26`（曾为「固化 commit 测试隔离与自动维护派生验证」）与 `CS-25b` 重复且不在实施顺序中，R30 起废弃并保留编号不复用：其 `src/cli.rs` dispatch 后派生由 `CS-25a` 承接，测试与 CI 隔离由 `CS-25b` 承接，性能双规模测量由 `CS-15` 承接。

### 字段全局默认与例外

- **Release boundary 默认:** `independent`
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（`independent` 卡完成即发布版本与 artifact，恢复需发新版本前滚修复；兼容窗口为相邻一个 minor 版本内保持仓内对象与配置向后兼容，降级通过上一已知良好 tag 重新安装）
- **Migration and rollback 默认:** `N/A：无 SQLite schema 迁移；兼容窗口内降级通过上一已知良好 tag 重新安装`
- **Security and privacy 默认:** `继承 GC-07、GC-11`
- **Performance budget 默认:** `继承 GC-10`
- **Release write set 默认:** `Inherited`
- **D 组默认（全部 `C/D coverage from=self` 的卡；家族子卡继承其发布点的 D）:** ① `.github/workflows/base.yml` 的 `format`、`clippy`、`redundancy`、`test` job，触发为 feature branch PR（`pull_request`，同一 head SHA）及合入后 `push` 到 `main`；② `.github/workflows/codeql.yml` 的 `analyze` job，触发为 PR 与 `push` 到 `main`；③ `.github/workflows/release.yml` 的 `build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` job，触发为 `gh release create v<version>` 推送的 `v*` tag；④ 发布产物 CDN 可获取性：`curl -fsSI https://download.libra.tools/libra/releases/v<version>/libra-linux-amd64` 与 `.../libra-darwin-arm64` 的 HTTP 状态均为 200（只读 HEAD 请求，不保存响应；路径以 `install.sh:14,393` 的 `${BASE_URL}/${VERSION}/libra-<os>-<arch>` 为准）；⑤ 网站部署核验（W-6）不另作 D 组项：网站写集非空的实现卡（含家族子卡）在本卡文档门内完成 W-6。判据：每个 job 的 conclusion 为 `success`（被取消的 run 不算 PASS），按 V-4 记录 run ID、head SHA 与结论；job 集合以执行当日现场读取的 workflow 文件为准（ER-04「仓库配置与 CI 展开规则」）。
- **夹具默认:** FX-BASE；卡内「夹具」只登记差异化条件。

**默认覆盖：** `family child`（38 张）取 `Rollback mode=revert`、`Version increment=N/A`、`Release write set=N/A`、`Full-suite trigger=N/A`；`REL-CS-*`（10 张）取 `Task type=release`；`CS-15c` 的 `Docs and compatibility impact` 判定为 `N/A`（理由见卡内）。
**规则 waiver（`EX-*`）：** 无。

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| CS-01 | implementation | -S<keyid>解析与分离守卫 | 前滚新版本+降级指引 | yes | yes | 8/8 | 2/8 | 1/1 | M | DEP-CS-07 | 链首（DEP-CS-07 窗口） | independent | N/A | N/A |
| CS-01a | implementation | keyid选钥与未知keyid拒绝 | 前滚新版本+降级指引 | yes | yes | 6/8 | 2/8 | 2/2 | M | CS-01, DEP-CS-07 | 序列化于 CS-01 | independent | CS-01 | N/A |
| CS-02 | implementation | init默认不写vault.signing与签名优先级 | 推送前revert本地提交 | yes | yes | 8/8 | 2/8 | 1/1 | M | CS-01a, DEP-CS-03, DEP-CS-04, DEP-CS-06, DEP-CS-07, DEP-CS-09 | 序列化于 CS-01a | family child of REL-CS-00 (1/2) | N/A | N/A |
| CS-02a | implementation | 派生命令默认不签名与签名帮助 | 推送前revert本地提交 | yes | yes | 5/8 | 5/8 | 1/3 | M | CS-02, DEP-CS-02, DEP-CS-03, DEP-CS-04, DEP-CS-07 | 序列化于 CS-02 | family child of REL-CS-00 (2/2) | CS-02 | N/A |
| REL-CS-00 | release | REL-CS-00家族发布点 | 前滚新版本+降级指引 | yes | yes | 9/12 | 9/12 | 0/0 | S | CS-02, CS-02a | 序列化于 CS-02a | family release point of REL-CS-00 | N/A | N/A |
| CS-03 | implementation | gpg.format严格校验 | 前滚新版本+降级指引 | yes | yes | 7/8 | 1/8 | 1/1 | M | REL-CS-00, DEP-CS-02, DEP-CS-04, DEP-CS-07 | 序列化于 REL-CS-00 | independent | N/A | N/A |
| CS-03f | implementation | gpg.program解析契约 | 前滚新版本+降级指引 | yes | yes | 8/8 | 2/8 | 1/1 | M | CS-03, DEP-CS-07 | 序列化于 CS-03 | independent | CS-03a | N/A |
| CS-03a | implementation | gpg.program外部签名 | 推送前revert本地提交 | yes | yes | 6/8 | 1/8 | 1/1 | M | CS-03f, DEP-CS-07 | 序列化于 CS-03f | family child of REL-CS-07 (1/5) | CS-03 | N/A |
| CS-03j | implementation | gpg.program外部签名失败判定 | 推送前revert本地提交 | yes | yes | 5/8 | 1/8 | 1/1 | M | CS-03a, DEP-CS-07 | 序列化于 CS-03a | family child of REL-CS-07 (2/5) | CS-03a | N/A |
| CS-03d | implementation | gpg.program外部验签 | 推送前revert本地提交 | yes | yes | 8/8 | 2/8 | 1/2 | M | CS-03j, DEP-CS-07 | 序列化于 CS-03j | family child of REL-CS-07 (3/5) | CS-03a | N/A |
| CS-03g | implementation | merge验签信任门 | 推送前revert本地提交 | yes | yes | 5/8 | 1/8 | 1/1 | M | CS-03d, DEP-CS-07 | 序列化于 CS-03d | family child of REL-CS-07 (4/5) | CS-03d | N/A |
| CS-03l | implementation | gpg.minTrustLevel信任门 | 推送前revert本地提交 | yes | yes | 4/8 | 1/8 | 1/1 | M | CS-03g, DEP-CS-07 | 序列化于 CS-03g | family child of REL-CS-07 (5/5) | CS-03g | N/A |
| REL-CS-07 | release | REL-CS-07家族发布点 | 前滚新版本+降级指引 | yes | yes | 8/12 | 8/12 | 0/0 | S | CS-03a, CS-03j, CS-03d, CS-03g, CS-03l | 序列化于 CS-03l | family release point of REL-CS-07 | N/A | N/A |
| CS-03b | implementation | SSH签名生成与临时文件清理 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 1/2 | M | REL-CS-07, DEP-CS-07 | 序列化于 REL-CS-07 | family child of REL-CS-01 (1/6) | CS-03 | N/A |
| CS-03c | implementation | SSH签名验证 | 推送前revert本地提交 | yes | yes | 8/8 | 2/8 | 1/1 | M | CS-03b, DEP-CS-07 | 序列化于 CS-03b | family child of REL-CS-01 (2/6) | CS-03b | N/A |
| CS-03h | implementation | SSH验签提交时间 | 推送前revert本地提交 | yes | yes | 8/8 | 2/8 | 1/1 | M | CS-03c, DEP-CS-07 | 序列化于 CS-03c | family child of REL-CS-01 (3/6) | CS-03c | N/A |
| CS-03i | implementation | SSH验签吊销文件 | 推送前revert本地提交 | yes | yes | 5/8 | 2/8 | 1/1 | M | CS-03h, DEP-CS-07 | 序列化于 CS-03h | family child of REL-CS-01 (4/6) | CS-03h | N/A |
| CS-03k | implementation | SSH验签principal与信任门 | 推送前revert本地提交 | yes | yes | 4/8 | 2/8 | 1/2 | M | CS-03i, DEP-CS-07 | 序列化于 CS-03i | family child of REL-CS-01 (5/6) | CS-03c | N/A |
| CS-03e | implementation | SSH签名fail-closed诊断 | 推送前revert本地提交 | yes | yes | 3/8 | 1/8 | 1/1 | M | CS-03k, DEP-CS-07 | 序列化于 CS-03k | family child of REL-CS-01 (6/6) | CS-03c | N/A |
| REL-CS-01 | release | REL-CS-01家族发布点 | 前滚新版本+降级指引 | yes | yes | 12/12 | 12/12 | 0/0 | S | CS-03b, CS-03c, CS-03h, CS-03i, CS-03k, CS-03e | 序列化于 CS-03e | family release point of REL-CS-01 | N/A | N/A |
| CS-04 | implementation | --amend丢弃旧gpgsig与--date | 前滚新版本+降级指引 | yes | yes | 8/8 | 2/8 | 1/1 | M | REL-CS-01, DEP-CS-07 | 序列化于 REL-CS-01 | independent | N/A | N/A |
| CS-05 | implementation | 重复-m与--fixup-m拼接 | 前滚新版本+降级指引 | yes | yes | 5/8 | 1/8 | 1/1 | M | CS-04, DEP-CS-07 | 序列化于 CS-04 | independent | N/A | N/A |
| CS-06 | implementation | --squash消息来源组合与编辑回写 | 前滚新版本+降级指引 | yes | yes | 8/8 | 1/8 | 1/1 | M | CS-05, DEP-CS-07 | 序列化于 CS-05 | independent | N/A | N/A |
| CS-08 | implementation | commit--long预览 | 前滚新版本+降级指引 | yes | yes | 6/8 | 1/8 | 1/2 | M | CS-06, DEP-CS-05, DEP-CS-07 | 序列化于 CS-06 | independent | N/A | N/A |
| CS-10 | implementation | commit-v模板与尾行 | 前滚新版本+降级指引 | yes | yes | 5/8 | 1/8 | 1/1 | M | CS-08, DEP-CS-07 | 序列化于 CS-08 | independent | N/A | N/A |
| CS-11 | implementation | -F消息UTF-8一致化与告警 | 前滚新版本+降级指引 | yes | yes | 8/8 | 1/8 | 1/1 | M | CS-10, DEP-CS-07 | 序列化于 CS-10 | independent | N/A | N/A |
| CS-12 | implementation | --signoff去重与trailer定位 | 前滚新版本+降级指引 | yes | yes | 5/8 | 1/8 | 1/1 | M | CS-11, DEP-CS-07 | 序列化于 CS-11 | independent | N/A | N/A |
| CS-12a | implementation | 仅trailer消息判空 | 前滚新版本+降级指引 | yes | yes | 4/8 | 1/8 | 1/1 | M | CS-12, DEP-CS-07 | 序列化于 CS-12 | independent | CS-12 | N/A |
| CS-13 | implementation | verbatim未编辑模板直接提交 | 前滚新版本+降级指引 | yes | yes | 3/8 | 1/8 | 1/1 | M | CS-12a, DEP-CS-07 | 序列化于 CS-12a | independent | N/A | N/A |
| CS-14 | implementation | whitespace清理与commentChar | 前滚新版本+降级指引 | yes | yes | 7/8 | 1/8 | 1/1 | M | CS-13, DEP-CS-07 | 序列化于 CS-13 | independent | N/A | N/A |
| CS-14a | implementation | scissors清理与cut-line | 前滚新版本+降级指引 | yes | yes | 6/8 | 2/8 | 1/1 | M | CS-14, DEP-CS-01, DEP-CS-07 | 序列化于 CS-14 | independent | CS-14 | N/A |
| CS-15 | implementation | <pathspec>与--only部分提交 | 推送前revert本地提交 | yes | yes | 8/8 | 2/8 | 2/2 | M | CS-14a, DEP-CS-07, DEP-CS-08 | 序列化于 CS-14a | family child of REL-CS-03 (1/4) | N/A | N/A |
| CS-15h | implementation | 部分提交删除路径 | 推送前revert本地提交 | yes | yes | 3/8 | 1/8 | 1/1 | M | CS-15, DEP-CS-07 | 序列化于 CS-15 | family child of REL-CS-03 (2/4) | CS-15 | N/A |
| CS-15d | implementation | 部分提交预览与守卫 | 推送前revert本地提交 | yes | yes | 7/8 | 1/8 | 1/1 | M | CS-15h, DEP-CS-07 | 序列化于 CS-15h | family child of REL-CS-03 (3/4) | CS-15 | N/A |
| CS-15f | implementation | --only无路径规则 | 推送前revert本地提交 | yes | yes | 7/8 | 1/8 | 1/1 | M | CS-15d, DEP-CS-07 | 序列化于 CS-15d | family child of REL-CS-03 (4/4) | CS-15d | N/A |
| REL-CS-03 | release | REL-CS-03家族发布点 | 前滚新版本+降级指引 | yes | yes | 7/12 | 7/12 | 0/0 | S | CS-15, CS-15h, CS-15d, CS-15f | 序列化于 CS-15f | family release point of REL-CS-03 | N/A | N/A |
| CS-15a | implementation | -i/--include部分提交 | 前滚新版本+降级指引 | yes | yes | 7/8 | 1/8 | 1/1 | M | REL-CS-03, DEP-CS-07 | 序列化于 REL-CS-03 | independent | CS-15 | N/A |
| CS-15b | implementation | -p/--patch交互hunk提交 | 前滚新版本+降级指引 | yes | yes | 8/8 | 1/8 | 2/3 | M | CS-15a, DEP-CS-03, DEP-CS-04, DEP-CS-05, DEP-CS-07 | 序列化于 CS-15a | independent | CS-15 | N/A |
| CS-15c | implementation | --pathspec-from-file列表解析 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 1/1 | M | CS-15b, DEP-CS-07, DEP-CS-08 | 序列化于 CS-15b | family child of REL-CS-04 (1/3) | CS-15 | N/A |
| CS-15g | implementation | 文件列表路径模式分发 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 1/1 | M | CS-15c, DEP-CS-07 | 序列化于 CS-15c | family child of REL-CS-04 (2/3) | CS-15c | N/A |
| CS-15e | implementation | #786部分提交互斥诊断 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 1/1 | M | CS-15g, DEP-CS-07 | 序列化于 CS-15g | family child of REL-CS-04 (3/3) | CS-15c | N/A |
| REL-CS-04 | release | REL-CS-04家族发布点 | 前滚新版本+降级指引 | yes | yes | 5/12 | 5/12 | 0/0 | S | CS-15c, CS-15g, CS-15e | 序列化于 CS-15e | family release point of REL-CS-04 | N/A | N/A |
| CS-09 | implementation | 解禁commit--interactive与守卫 | 推送前revert本地提交 | yes | yes | 5/8 | 1/8 | 2/2 | M | REL-CS-04, DEP-CS-07 | 序列化于 REL-CS-04 | family child of REL-CS-02 (1/4) | N/A | N/A |
| CS-09b | implementation | interactive与-a/-i/-o互斥 | 推送前revert本地提交 | yes | yes | 3/8 | 1/8 | 1/1 | M | CS-09, DEP-CS-07 | 序列化于 CS-09 | family child of REL-CS-02 (2/4) | CS-09 | N/A |
| CS-09a | implementation | --interactive菜单暂存与管道提交 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 1/2 | M | CS-09b, DEP-CS-07 | 序列化于 CS-09b | family child of REL-CS-02 (3/4) | CS-09 | N/A |
| CS-09c | implementation | --interactive无暂存中止路径 | 推送前revert本地提交 | yes | yes | 3/8 | 1/8 | 1/2 | M | CS-09a, DEP-CS-07 | 序列化于 CS-09a | family child of REL-CS-02 (4/4) | CS-09a | N/A |
| REL-CS-02 | release | REL-CS-02家族发布点 | 前滚新版本+降级指引 | yes | yes | 7/12 | 7/12 | 0/0 | S | CS-09, CS-09b, CS-09a, CS-09c | 序列化于 CS-09c | family release point of REL-CS-02 | N/A | N/A |
| CS-07 | implementation | --fixup=amend:/reword:语义 | 推送前revert本地提交 | yes | yes | 7/8 | 1/8 | 1/1 | M | REL-CS-02, DEP-CS-07 | 序列化于 REL-CS-02 | family child of REL-CS-09 (1/3) | N/A | N/A |
| CS-07a | implementation | reword冲突诊断 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 1/1 | M | CS-07, DEP-CS-07 | 序列化于 CS-07 | family child of REL-CS-09 (2/3) | CS-07 | N/A |
| CS-07b | implementation | -m与前缀冲突及未知前缀 | 推送前revert本地提交 | yes | yes | 3/8 | 1/8 | 1/1 | M | CS-07a, DEP-CS-07 | 序列化于 CS-07a | family child of REL-CS-09 (3/3) | CS-07a | N/A |
| REL-CS-09 | release | REL-CS-09家族发布点 | 前滚新版本+降级指引 | yes | yes | 5/12 | 5/12 | 0/0 | S | CS-07, CS-07a, CS-07b | 序列化于 CS-07b | family release point of REL-CS-09 | N/A | N/A |
| CS-16 | implementation | :(optional)模板前缀 | 前滚新版本+降级指引 | yes | yes | 7/8 | 1/8 | 1/1 | M | REL-CS-09, DEP-CS-07 | 序列化于 REL-CS-09 | independent | N/A | N/A |
| CS-17 | implementation | commit --date approxidate入口 | 前滚新版本+降级指引 | yes | yes | 8/8 | 2/8 | 2/2 | M | CS-16, DEP-CS-07 | 序列化于 CS-16 | independent | N/A | N/A |
| CS-17a | implementation | commit --date本地ISO与夏令时 | 前滚新版本+降级指引 | yes | yes | 6/8 | 2/8 | 1/2 | M | CS-17, DEP-CS-07 | 序列化于 CS-17 | independent | CS-17 | N/A |
| CS-18 | implementation | --amend--author保留原日期 | 前滚新版本+降级指引 | yes | yes | 5/8 | 1/8 | 1/1 | M | CS-17a, DEP-CS-07 | 序列化于 CS-17a | independent | N/A | N/A |
| CS-18b | implementation | --reset-author使用范围校验 | 前滚新版本+降级指引 | yes | yes | 6/8 | 2/8 | 1/1 | M | CS-18, DEP-CS-07 | 序列化于 CS-18 | independent | CS-18 | N/A |
| CS-18a | implementation | 摘要Author行 | 前滚新版本+降级指引 | yes | yes | 4/8 | 1/8 | 1/1 | M | CS-18b, DEP-CS-07 | 序列化于 CS-18b | independent | CS-18 | N/A |
| CS-18c | implementation | 摘要Date行显示条件 | 前滚新版本+降级指引 | yes | yes | 6/8 | 1/8 | 1/1 | M | CS-18a, DEP-CS-07 | 序列化于 CS-18a | independent | CS-18a | N/A |
| CS-19 | implementation | amend按notes.rewrite复制notes | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 1/2 | M | CS-18c, DEP-CS-04, DEP-CS-07 | 序列化于 CS-18c | family child of REL-CS-08 (1/4) | N/A | N/A |
| CS-19c | implementation | notes.rewriteMode非默认值 | 推送前revert本地提交 | yes | yes | 6/8 | 1/8 | 1/1 | M | CS-19, DEP-CS-07 | 序列化于 CS-19 | family child of REL-CS-08 (2/4) | CS-19 | N/A |
| CS-19b | implementation | notes.rewriteRef多值与glob | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 1/2 | M | CS-19c, DEP-CS-07 | 序列化于 CS-19c | family child of REL-CS-08 (3/4) | CS-19 | N/A |
| CS-19a | implementation | notes复制的锁与基线比较失败语义 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 3/4 | M | CS-19b, DEP-CS-04, DEP-CS-07 | 序列化于 CS-19b | family child of REL-CS-08 (4/4) | CS-19 | N/A |
| REL-CS-08 | release | REL-CS-08家族发布点 | 前滚新版本+降级指引 | yes | yes | 6/12 | 6/12 | 0/0 | S | CS-19, CS-19c, CS-19b, CS-19a | 序列化于 CS-19a | family release point of REL-CS-08 | N/A | N/A |
| CS-20 | implementation | --amend后索引一致性 | 前滚新版本+降级指引 | yes | yes | 5/8 | 1/8 | 1/1 | M | REL-CS-08, DEP-CS-07 | 序列化于 REL-CS-08 | independent | N/A | N/A |
| CS-21 | implementation | commit-tree消息精确字节 | 前滚新版本+降级指引 | yes | yes | 6/8 | 1/8 | 1/1 | M | CS-20, DEP-CS-07 | 序列化于 CS-20 | independent | N/A | N/A |
| CS-21a | implementation | cat-file批处理空行 | 前滚新版本+降级指引 | yes | yes | 5/8 | 1/8 | 1/1 | M | CS-21, DEP-CS-03, DEP-CS-04, DEP-CS-07 | 序列化于 CS-21 | independent | CS-21 | N/A |
| CS-22a | implementation | prepare-commit-msg参数对齐 | 前滚新版本+降级指引 | yes | yes | 6/8 | 1/8 | 1/1 | M | CS-21a, DEP-CS-07 | 序列化于 CS-21a | independent | N/A | N/A |
| CS-23 | implementation | 编辑器嵌套commit重入令牌 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 3/3 | M | CS-22a, DEP-CS-07 | 序列化于 CS-22a | family child of REL-CS-05 (1/3) | N/A | N/A |
| CS-23a | implementation | 外层有界等待与超时终止 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 3/3 | M | CS-23, DEP-CS-07 | 序列化于 CS-23 | family child of REL-CS-05 (2/3) | CS-23 | N/A |
| CS-23b | implementation | 嵌套变更检测与op-log隔离 | 推送前revert本地提交 | yes | yes | 7/8 | 1/8 | 2/2 | M | CS-23a, DEP-CS-07 | 序列化于 CS-23a | family child of REL-CS-05 (3/3) | CS-23a | N/A |
| REL-CS-05 | release | REL-CS-05家族发布点 | 前滚新版本+降级指引 | yes | yes | 5/12 | 5/12 | 0/0 | S | CS-23, CS-23a, CS-23b | 序列化于 CS-23b | family release point of REL-CS-05 | N/A | N/A |
| CS-24 | implementation | GIT_TRACE2_EVENT事件子系统 | 前滚新版本+降级指引 | yes | yes | 8/8 | 1/8 | 2/3 | M | REL-CS-05, DEP-CS-07 | 序列化于 REL-CS-05 | independent | N/A | N/A |
| CS-25 | implementation | maintenance run --auto与loose条件 | 前滚新版本+降级指引 | yes | yes | 8/8 | 1/8 | 1/1 | M | CS-24, DEP-CS-07 | 序列化于 CS-24 | independent | CS-24 | N/A |
| CS-25g | implementation | maintenance--[no-]quiet输出 | 前滚新版本+降级指引 | yes | yes | 4/8 | 1/8 | 1/1 | M | CS-25, DEP-CS-07 | 序列化于 CS-25 | independent | CS-25 | N/A |
| CS-25d | implementation | maintenance--[no-]detach分离语义 | 前滚新版本+降级指引 | yes | yes | 8/8 | 1/8 | 1/1 | M | CS-25g, DEP-CS-07 | 序列化于 CS-25g | independent | CS-25 | N/A |
| CS-25c | implementation | maintenance--auto增量重打包条件 | 前滚新版本+降级指引 | yes | yes | 4/8 | 1/8 | 1/1 | M | CS-25d, DEP-CS-07 | 序列化于 CS-25d | independent | CS-25 | N/A |
| CS-25h | implementation | maintenance--auto的gc条件 | 前滚新版本+降级指引 | yes | yes | 7/8 | 1/8 | 1/1 | M | CS-25c, DEP-CS-07 | 序列化于 CS-25c | independent | CS-25c | N/A |
| CS-25f | implementation | maintenance--auto取锁带界等待 | 前滚新版本+降级指引 | yes | yes | 8/8 | 1/8 | 1/1 | M | CS-25h, DEP-CS-07 | 序列化于 CS-25h | independent | CS-25c | N/A |
| CS-25a | implementation | commit后自动维护派生 | 推送前revert本地提交 | yes | yes | 8/8 | 1/8 | 2/2 | M | CS-25f, DEP-CS-07 | 序列化于 CS-25f | family child of REL-CS-06 (1/4) | CS-25 | N/A |
| CS-25i | implementation | commit自动维护分离时序与派生失败 | 推送前revert本地提交 | yes | yes | 6/8 | 1/8 | 1/1 | M | CS-25a, DEP-CS-07 | 序列化于 CS-25a | family child of REL-CS-06 (2/4) | CS-25a | N/A |
| CS-25e | implementation | 自动维护参数映射 | 推送前revert本地提交 | yes | yes | 4/8 | 1/8 | 1/2 | M | CS-25i, DEP-CS-07 | 序列化于 CS-25i | family child of REL-CS-06 (3/4) | CS-25a | N/A |
| CS-25b | implementation | 自动维护测试与CI隔离 | 推送前revert本地提交 | yes | yes | 8/8 | 2/8 | 1/1 | M | CS-25e, DEP-CS-07 | 序列化于 CS-25e | family child of REL-CS-06 (4/4) | CS-25a | N/A |
| REL-CS-06 | release | REL-CS-06家族发布点 | 前滚新版本+降级指引 | yes | yes | 7/12 | 7/12 | 0/0 | S | CS-25a, CS-25i, CS-25e, CS-25b | 序列化于 CS-25b | family release point of REL-CS-06 | N/A | N/A |

---

### Task CS-01: 修复 `CommitArgs::gpg_sign` (`-S[<keyid>]`) 贴合解析与分离参数守卫（#787, #1006）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 使 `-S<keyid>` 贴合形式与 `--gpg-sign=<keyid>` 正确解析为 `Some(Some(<keyid>))`，裸 `-S`/`--gpg-sign` 解析为 `Some(None)`，分离形式 `-S <path>` 不吞参（`<path>` 留给位置 pathspec）。本卡只改解析；keyid 选钥由 `CS-01a` 承接，在此之前与现状一致地使用默认签名密钥（帮助文本如实说明）。已拆出 `CS-01a`。
**Out of scope:** keyid 选钥与未知 keyid 拒绝（由 `CS-01a` 承接）；`gpg.format` 校验（由 `CS-03` 承接）；`gpg.program`（由 `CS-03a` 承接）；SSH 签名（由 `CS-03b` 承接）；CLI 签名标志与配置的优先级（由 `CS-02` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `gpg_sign: Option<Option<String>>` 带 `require_equals = true`，拒绝贴合形式 | `src/command/commit.rs:239-243` |

**夹具（FX-BASE 之外的差异化条件）:** AC1、AC3–AC8 以 `CommitArgs::try_parse_from` 直接解析，参数向量首元素为 `commit`；AC2 在启用 vault 签名（`vault.signing=true`）的仓库中经 `libra` 二进制运行，keyid 取该仓库 `vault.gpg.pubkey` 公钥经 `pgp` crate 计算的完整 fingerprint（与 `CS-01a` 的选钥规则兼容，避免字面 keyid 在 `CS-01a` 后被 fail-closed 拒绝）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`-Skey` 解析得到 `gpg_sign == Some(Some("key"))`。
- [ ] AC2：经 `libra` 二进制运行的 `libra commit -S<F> -m e2e`（`F` 为仓库配置 `vault.gpg.pubkey` 所存默认签名公钥的完整 fingerprint）生成提交，其 `gpgsig` 用该默认公钥验签通过（本卡阶段 keyid 尚不参与选钥；`CS-01a` 落地后 `F` 仍选中同一把默认密钥，本用例无需改写）。
- [ ] AC3：`-S"ssh-ed25519 AAAA…"` 解析得到 `gpg_sign == Some(Some("ssh-ed25519 AAAA…"))`。
- [ ] AC4：`--gpg-sign=K2` 解析得到 `gpg_sign == Some(Some("K2"))`。
- [ ] AC5：裸 `-S` 解析得到 `gpg_sign == Some(None)`。
- [ ] AC6：裸 `--gpg-sign` 解析得到 `gpg_sign == Some(None)`。
- [ ] AC7：`-S file` 解析得到 `gpg_sign == Some(None)`。
- [ ] AC8：`-S file` 解析得到 `paths == ["file"]`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_sign_keyid_parse_forms)'`（V-2；new；覆盖 AC1、AC3–AC8）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_sign_attached_keyid_e2e)'`（V-2；new；覆盖 AC2）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`-S[<keyid>]` 贴合/分离语义）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=-S<keyid>解析与分离守卫; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DEP-CS-07; writeset=链首（DEP-CS-07 窗口）; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-01a: 按 `-S<keyid>` 在仓库现有签名密钥中选钥并对未知 keyid fail-closed（#787, #1006）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-01` 拆出：keyid 在仓库现有两把签名密钥（vault 生成密钥与 ADR-VG-12 导入密钥）中按完整 fingerprint 或 16 位 long key id（大小写不敏感）选择签名密钥；无匹配时 fail-closed，不回退默认密钥。不新增密钥存储或导入/替换/删除生命周期。本卡同时交付全计划共用的 SNAP 读取 helper（`tests/helpers/snapshot.rs`，`command_test` 与 compat target 以 `#[path]` 引入）及其负向自检。
**Out of scope:** `-S` 解析（由 `CS-01` 承接）；多密钥存储与密钥生命周期（永久非目标：现有 vault 已同时保存生成与导入两把密钥）；`gpg.program`/SSH 下 keyid 的含义（由 `CS-03a`、`CS-03b` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `pgp_sign` 只按 `vault.gpg.source` 在导入密钥与生成密钥之间二选一，无 keyid 入参 | `src/internal/vault.rs:417-427,507-527` |
| 签名失败映射 `LBR-AUTH-001` | `src/command/commit.rs:566-568` |

**夹具（FX-BASE 之外的差异化条件）:** 夹具一：同一仓库同时具备 vault 生成密钥 G 与导入密钥 I，`vault.gpg.source=imported`（默认签名者为 I）；夹具二：同样两把密钥但 `vault.gpg.source=generated`（默认签名者为 G）；I 的测试私钥材料放在 `tests/fixtures/cs-01-signing/`。各 AC 以实际公钥对去除签名的提交载荷验签（`pgp` crate）判定签名者，能区分「忽略 keyid、总用默认密钥」或只改写 issuer 字段的错误实现。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：夹具一中 `libra commit -S<G 的完整 fingerprint> -m a` 生成的提交，其 `gpgsig` 用 G 的公钥验签通过。
- [ ] AC2：夹具一中 `libra commit -S<G 的 16 位 long key id 小写形式> -m b` 生成的提交，其 `gpgsig` 用 G 的公钥验签通过。
- [ ] AC3：夹具二中 `libra commit -S<I 的完整 fingerprint> -m e` 生成的提交，其 `gpgsig` 用 I 的公钥验签通过。
- [ ] AC4：夹具一中 `libra commit -S -m c` 生成的提交，其 `gpgsig` 用 I 的公钥验签通过。
- [ ] AC5：对照：AC1 的提交的 `gpgsig` 用 I 的公钥验签失败。
- [ ] AC6：`libra commit -S<不匹配任何仓库密钥的 keyid> -m d` 以 `LBR-AUTH-001` 失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_sign_keyid_selects_key)'`（V-2；new；覆盖 AC1–AC6）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::snapshot_selftest::snapshot_detects_single_component_changes)'`（V-2；new；SNAP helper 交付门的独立验证门，计入本卡 G-03 Verification；`tests/command/mod.rs` 以 `#[path = "../helpers/snapshot.rs"]` 引入 SNAP helper 并新增该自检模块；逐项覆盖「SNAP helper 交付门」所列全部单分量变化、不可解析 idx 的取快照失败与 ⑥ 放宽口径）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `T-1`：修改共享签名 helper `src/internal/vault.rs`；同时命中 `T-4`：新增被多 target 复用的 SNAP helper（`tests/helpers/snapshot.rs`、`tests/command/mod.rs`）
**Dependencies:** `CS-01`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/internal/vault.rs`, `tests/command/commit_test.rs`, `tests/fixtures/cs-01-signing/`, `tests/helpers/snapshot.rs`, `tests/command/mod.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `CLAUDE.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（keyid 选钥规则与未知 keyid 失败）、`CLAUDE.md`（Commit & PR Conventions 中「the optional key ID is ignored」改为 keyid 按 fingerprint/long key id 选择仓库现有密钥）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=keyid选钥与未知keyid拒绝; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CS-01,DEP-CS-07; writeset=序列化于 CS-01; release=independent; split-from=CS-01; exception=N/A`

---

### Task CS-02: 对齐 `libra init` 默认不写入 `vault.signing` 与提交签名优先级（#1005）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 `ADR-CS-01` 使 `libra init` 不再写入 `vault.signing=true`：`InitOutput.vault_signing` 与人类输出的 `signing:` 行不再宣称默认签名（JSON 为 `false`、人类输出为 `signing: disabled`）；提交签名决策为「最后出现的 CLI 签名标志 > `commit.gpgSign` > 显式 `vault.signing=true` > 默认不签名」；同步更新 `init_test`/`init_json_test` 中依赖旧默认的夹具与 `CLAUDE.md` 默认签名表述。已拆出 `CS-02a`。
**Out of scope:** `history_config`/`clone`/`merge`/`cherry-pick` 帮助文案与多命令夹具（由 `CS-02a` 承接）；`gpg.format`（由 `CS-03` 承接）；`gpg.program`（由 `CS-03a` 承接）；SSH 签名（由 `REL-CS-01` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `libra init` 默认调用 `set_vault_signing_value(database_path, true)`；`InitOutput.vault_signing` 取自 `args.vault`，人类输出据此打印 `signing: enabled` | `src/command/init.rs:540-551,1187,2495-2508` |
| `commit_signing_policy` 只读 `commit.gpgSign`，未设置时 `InheritVault` | `src/command/history_config.rs:71-82` |

**夹具（FX-BASE 之外的差异化条件）:** 每个用例的新仓库在 `libra init` 后都具备可用 vault 生成密钥与 unseal key，使「未签名」只能来自策略而非缺钥；AC5 以同一夹具设 `vault.signing=true` 作为对照，证明密钥可用。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra init` 后本仓库 config 不存在 `vault.signing` 键。
- [ ] AC2：`libra init --json` 输出的 `vault_signing` 字段为 `false`。
- [ ] AC3：`libra init` 人类输出含 `signing: disabled` 行。
- [ ] AC4：未设置任何签名配置且未传签名标志时，`libra commit -m a` 生成的提交对象不含 `gpgsig` 头。
- [ ] AC5：同一夹具设 `vault.signing=true` 后 `libra commit -m b` 生成的提交含 `gpgsig` 头。
- [ ] AC6：`commit.gpgSign=false` 与 `vault.signing=true` 同时设置时，`libra commit -m c` 生成的提交不含 `gpgsig` 头。
- [ ] AC7：`commit.gpgSign=false` 时 `libra commit --no-gpg-sign -S -m d` 生成的提交含 `gpgsig` 头。
- [ ] AC8：`commit.gpgSign=true` 时 `libra commit -S --no-gpg-sign -m e` 生成的提交不含 `gpgsig` 头。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::init_does_not_enable_vault_signing)'`（V-2；new；覆盖 AC1–AC3）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_default_unsigned_and_signing_precedence)'`（V-2；new；覆盖 AC4–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::init_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::init_json_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test compat_config_history_defaults`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-00` 的 `T-2` 覆盖）
**Dependencies:** `CS-01a`, `DEP-CS-03`, `DEP-CS-04`, `DEP-CS-06`, `DEP-CS-07`, `DEP-CS-09`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/init.rs`, `tests/command/init_test.rs`, `tests/command/init_json_test.rs`, `tests/command/commit_test.rs`, `tests/compat/config_history_defaults_test.rs`, `docs/commands/init.md`, `docs/commands/zh-CN/init.md`, `docs/development/commands/init.md`, `CLAUDE.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/init.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-00` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/init.md`、`docs/commands/zh-CN/init.md`、`docs/development/commands/init.md`（默认不签名、`signing: disabled` 与启用方式）、`CLAUDE.md`（Commit & PR Conventions 中「GPG signing … by default」表述）、`COMPATIBILITY.md` init/commit 签名默认行、网站 `init.en.md`（W-1..W-6，`--only init`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-00 (1/2)`
**C/D coverage from:** `REL-CS-00`
**Granularity:** `type=implementation; axis=init默认不写vault.signing与签名优先级; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CS-01a,DEP-CS-03,DEP-CS-04,DEP-CS-06,DEP-CS-07,DEP-CS-09; writeset=序列化于 CS-01a; release=family child of REL-CS-00 (1/2); split-from=N/A; exception=N/A`

---

### Task CS-02a: 对齐 clone/merge/cherry-pick 默认不签名、签名帮助文案与多命令签名夹具（#1005）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-02` 拆出：`clone` 透传的 `InitOutput.vault_signing` 与目标仓配置、`merge` 与 `cherry-pick` 的提交签名均随 `CS-02` 变为默认不签名（决策表不变，由 `history_config.rs` 单一事实源提供）；`commit`（`src/command/commit.rs:268-278`）与 `merge`（`src/command/merge.rs:465-470`）的签名选项帮助不再把 `vault.signing` 描述为默认开启（help 文本按 ER-06 同卡同步）；`command::clone_cli_test::successful_clone_initializes_vault` 与 `merge_test::gpg_sign`、`cherry_pick_test` 中依赖旧默认签名的用例改为显式启用签名。另补验决策表中 `commit.gpgSign=true` 单独生效的分支（`CS-02` 的 AC 已满）。
**Out of scope:** `libra init` 默认值与 commit 决策表（由 `CS-02` 承接）；签名决策表本身（不改变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--no-gpg-sign` 帮助称 `vault.signing` 为 `libra init` 默认；`merge` 帮助称 `vault.signing` default | `src/command/commit.rs:268-278`; `src/command/merge.rs:465-470` |
| `clone` 透传 `vault_signing`；`successful_clone_initializes_vault` 断言克隆后 `vault.signing=true` | `src/command/clone.rs:506-507,1214,1807`; `tests/command/clone_cli_test.rs:717-752` |
| `merge_test::gpg_sign` 的 `divergent_merge_repo(true)` 生成 vault 密钥，多个用例依赖默认签名 | `tests/command/merge_test/gpg_sign.rs:12-23,59-93` |

**夹具（FX-BASE 之外的差异化条件）:** merge 与 cherry-pick 用例使用 `divergent_merge_repo(true)` 一类先执行 `libra config generate-gpg-key` 的夹具（密钥可用性由既有 `command::merge_test::gpg_sign::merge_gpg_sign_config_priority_matches_commit` 在 `vault.signing=true` 下签名成功证明），使「因缺钥而未签名」的错误实现无法通过。AC5 在 `libra init` 生成的新仓中运行（该仓具备可用 vault 生成密钥，`CS-02` 夹具同口径），使「因缺钥而未签名」不会掩盖决策表错误。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra clone <本地源仓库> <目标> --json` 输出的 `vault_signing` 为 `false`。
- [ ] AC2：克隆得到的目标仓 config 不存在 `vault.signing` 键（由改写后的 `successful_clone_initializes_vault` 断言）。
- [ ] AC3：无签名配置的新仓中 `libra merge --no-ff <branch>` 生成的合并提交不含 `gpgsig` 头。
- [ ] AC4：无签名配置的新仓中 `libra cherry-pick <commit>` 生成的提交不含 `gpgsig` 头。
- [ ] AC5：无 `vault.signing`、`commit.gpgSign=true` 且未传签名标志时 `libra commit -m f` 生成的提交含 `gpgsig` 头（决策表 `commit.gpgSign` 分支）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_cli_test::clone_default_vault_signing_unset)'`（V-2；new；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_cli_test::successful_clone_initializes_vault)'`（V-2；既有用例改写；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_default_unsigned_with_vault_key)'`（V-2；new；覆盖 AC3）
- [ ] V4 `source .env.test && cargo nextest run --test command_test -E 'test(=command::cherry_pick_test::cherry_pick_default_unsigned_with_vault_key)'`（V-2；new；覆盖 AC4）
- [ ] V5 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_sign_config_alone_signs)'`（V-2；new；覆盖 AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::gpg_sign::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::cherry_pick_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::clone_cli_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-00` 的 `T-2` 覆盖）
**Dependencies:** `CS-02`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-00`）, `DEP-CS-02`, `DEP-CS-03`, `DEP-CS-04`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/clone.rs`, `src/command/commit.rs`, `src/command/merge.rs`, `tests/command/clone_cli_test.rs`, `tests/command/commit_test.rs`, `tests/command/cherry_pick_test.rs`, `tests/command/merge_test/gpg_sign.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `docs/commands/clone.md`, `docs/commands/zh-CN/clone.md`, `docs/development/commands/clone.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `docs/commands/cherry-pick.md`, `docs/commands/zh-CN/cherry-pick.md`, `docs/development/commands/cherry-pick.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `docs/commands/README.md`, `docs/commands/zh-CN/README.md`, `docs/development/commands/commit-tree.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/clone.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cherry-pick.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-00` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** help 文本：`libra commit --help` 不含 ``the `libra init` default``、`libra merge --help` 不含 ``the `vault.signing` default``（ER-06 help 同步，D-1 以 `rg -F` 对 `--help` 输出零命中验收）；`docs/commands/{commit,clone,merge,cherry-pick,config}.md`、zh-CN 同名文件与 `docs/development/commands/` 同名文件（默认不签名、clone JSON 示例 `vault_signing`）、`docs/commands/README.md` 与 `docs/commands/zh-CN/README.md`（「Security by default」中的默认签名表述）、`docs/development/commands/commit-tree.md`（默认签名表述）、`COMPATIBILITY.md` clone/merge/cherry-pick/commit 签名行、网站 `commit.en.md`、`clone.en.md`、`merge.en.md`、`cherry-pick.en.md`、`config.en.md`（W-1..W-6，`--only commit clone merge cherry-pick config`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-00 (2/2)`
**C/D coverage from:** `REL-CS-00`
**Granularity:** `type=implementation; axis=派生命令默认不签名与签名帮助; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=5/8; VER=5/8; landing=1; prod-files=3; scope=M; deps=CS-02,DEP-CS-02,DEP-CS-03,DEP-CS-04,DEP-CS-07; writeset=序列化于 CS-02; release=family child of REL-CS-00 (2/2); split-from=CS-02; exception=N/A`

---

### Task REL-CS-00: `REL-CS-00` 默认不签名策略与测试夹具家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-02`（1/2）、`CS-02a`（2/2） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-02`、`CS-02a` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `CS-02` 改 `init` 默认后 `clone_cli_test.rs:742-752` 等夹具须由 `CS-02a` 同版更新 | `tests/command/clone_cli_test.rs:742-752`; `plan-template.md` G-08 |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-02`、`CS-02a` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `command::init_test::init_does_not_enable_vault_signing` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit_test::commit_default_unsigned_and_signing_precedence` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::clone_cli_test::clone_default_vault_signing_unset` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：聚合守卫 `command::clone_cli_test::successful_clone_initializes_vault` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC5：聚合守卫 `command::merge_test::gpg_sign::merge_default_unsigned_with_vault_key` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC6：聚合守卫 `command::cherry_pick_test::cherry_pick_default_unsigned_with_vault_key` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC7：聚合守卫 `command::commit_test::commit_gpg_sign_config_alone_signs` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC8：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-00 begin -->` 与 `<!-- release-note-draft:REL-CS-00 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 2 行，顺序同家族登记：`CS-02`、`CS-02a`）。
- [ ] AC9：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::init_test::init_does_not_enable_vault_signing)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_default_unsigned_and_signing_precedence)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_cli_test::clone_default_vault_signing_unset)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 `source .env.test && cargo nextest run --test command_test -E 'test(=command::clone_cli_test::successful_clone_initializes_vault)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC4）
- [ ] V5 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_default_unsigned_with_vault_key)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC5）
- [ ] V6 `source .env.test && cargo nextest run --test command_test -E 'test(=command::cherry_pick_test::cherry_pick_default_unsigned_with_vault_key)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC6）
- [ ] V7 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_sign_config_alone_signs)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC7）
- [ ] V8 手工证据（覆盖 AC8）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-00 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-00 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-02 CS-02a; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V9 手工证据（覆盖 AC9）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-02` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-02a` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-02`, `CS-02a`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-02`、`CS-02a` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-00`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-00家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=9/12; VER=9/12; landing=0; prod-files=0; scope=S; deps=CS-02,CS-02a; writeset=序列化于 CS-02a; release=family release point of REL-CS-00; split-from=N/A; exception=N/A`

---

### Task CS-03: 严格校验 `gpg.format` 大小写与非法值（#781）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在签名路径读取 `gpg.format` 时按 Git `gpg-interface.c:754-760`（pinned `6de20f6092`，`get_format_by_name` 精确 `strcmp`）只接受精确小写 `openpgp`、`x509`、`ssh`；非法大小写或未知值在触发签名时 fail-closed，错误码与既有 commit 非法配置一致（`LBR-CLI-002`，见 `src/command/commit.rs:488-490,497-501`），文案含 Git 同款 `invalid value for 'gpg.format': '<value>'`；`x509` 与（`REL-CS-01` 落地前的）`ssh` 在触发签名时以 `LBR-UNSUPPORTED-001` 拒绝，不回退 vault PGP。未触发签名时不读取 `gpg.format`（与 Git 惰性读取一致）。已拆出 `CS-03a`、`CS-03b`。
**Out of scope:** 外部 `gpg.program`（由 `CS-03a` 承接）；SSH 签名实现（由 `CS-03b` 承接）；X.509 签名后端（永久非目标，本计划只拒绝）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `vault_sign_commit` 不读取 `gpg.format` | `src/command/commit.rs:3088-3151` |
| 既有 commit 非法配置映射 `CliInvalidArguments`（`LBR-CLI-002`） | `src/command/commit.rs:488-490,497-501` |

**夹具（FX-BASE 之外的差异化条件）:** 签名可用的仓库（`vault.signing=true`）；对照：同一非法 `gpg.format` 在未触发签名（`vault.signing` 未设置且无 `-S`）时提交成功，区分「任何提交都读 gpg.format」的错误实现。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`vault.signing=true` 且 `gpg.format=OpEnPgP` 时 `libra commit -m a` 以 stderr 含 Git 同款 `invalid value for 'gpg.format': 'OpEnPgP'` 的错误失败。
- [ ] AC2：AC1 场景的稳定错误码为 `LBR-CLI-002`。
- [ ] AC3：`gpg.format=bogus` 且传 `-S` 时 `libra commit -m b` 以 stderr 含 `invalid value for 'gpg.format': 'bogus'` 的错误失败。
- [ ] AC4：`gpg.format=x509` 且触发签名时 `libra commit -m c` 以 `LBR-UNSUPPORTED-001` 失败。
- [ ] AC5：`gpg.format=openpgp` 且触发签名时生成的提交含 `gpgsig` 头。
- [ ] AC6：`gpg.format=OpEnPgP` 但未触发签名（无 `-S`、`vault.signing` 未设置）时 `libra commit -m d` 生成不含 `gpgsig` 头的提交。
- [ ] AC7：`REL-CS-01` 发布前的过渡期内，`gpg.format=ssh` 且触发签名时 `libra commit -m e` 以 `LBR-UNSUPPORTED-001` 失败（不回退 vault PGP）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_format_strict_values)'`（V-2；new；覆盖 AC1–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `REL-CS-00`, `DEP-CS-02`, `DEP-CS-04`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/{commit,config}.md`、zh-CN 同名文件、`docs/development/commands/{commit,config}.md`（`gpg.format` 合法值、大小写敏感与 x509 不支持）、`COMPATIBILITY.md` commit 签名行、网站 `commit.en.md`、`config.en.md`（W-1..W-6，`--only commit config`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=gpg.format严格校验; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=REL-CS-00,DEP-CS-02,DEP-CS-04,DEP-CS-07; writeset=序列化于 REL-CS-00; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-03f: 定义 `gpg.program` 级联读取与路径解析契约（#783 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03a` 拆出：把 `src/command/config.rs:4718-4726` 的 `resolve_gpg_program` 改为返回 `Result<Option<String>>` 的严格级联读取（local → global → system，与 `history_config.rs` 的 strict 读取同一口径）：未配置返回 `None`，读取失败返回错误而非回退 `gpg`，以 `~/` 开头的值按 `HOME` 展开；不含路径分隔符的值保持程序名，交由进程启动按 `$PATH` 查找。现有调用方（`config` 的 GPG 导入路径）在 `None` 时继续使用 `gpg`，行为不变。
**Out of scope:** 签名与验签接入（由 `CS-03a`、`CS-03d` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有 helper 以 `.ok().flatten()` 吞掉读取错误并 `unwrap_or_else(|| "gpg")`，无法区分未配置、显式 `gpg` 与读取失败，且不展开 `~/` | `src/command/config.rs:4718-4726` |
| strict 级联读取的既有实现 | `src/command/history_config.rs:84-104` |

**夹具（FX-BASE 之外的差异化条件）:** 隔离的 local/global/system 三层配置库（GC-13）；隔离 `HOME`；可将 local 配置库置为不可读以制造读取错误。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：三层均未配置 `gpg.program` 时 helper 返回 `Ok(None)`。
- [ ] AC2：global 为 `g-prog`、local 为 `l-prog` 时 helper 返回 `Ok(Some("l-prog"))`。
- [ ] AC3：local 未配置、global 为 `g-prog`、system 为 `s-prog` 时 helper 返回 `Ok(Some("g-prog"))`。
- [ ] AC4：local 与 global 均未配置、system 为 `s-prog` 时 helper 返回 `Ok(Some("s-prog"))`。
- [ ] AC5：值为 `~/bin/fake-gpg` 时 helper 返回 `Ok(Some("<HOME>/bin/fake-gpg"))`。
- [ ] AC6：local 配置库不可读时 helper 返回 `Err`。
- [ ] AC7：local 未配置且 global 配置库不可读时 helper 返回 `Err`（不静默回退 system 或 `gpg`）。
- [ ] AC8：三层均未配置时，既有 `config` GPG 导入路径调用的程序名仍为 `gpg`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=command::config::tests::resolve_gpg_program_contract)'`（V-2；new，新建于 `src/command/config.rs` 的 `#[cfg(test)] mod tests`（现有测试模块名为 `args_tests` 等，`tests` 模块由本卡新建）；覆盖 AC1–AC7）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::config_test::gpg_import_uses_default_program_when_unset)'`（V-2；new；覆盖 AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::config_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `T-1`：修改共享 helper `resolve_gpg_program` 的返回契约与全部调用方
**Dependencies:** `CS-03`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/config.rs`, `tests/command/config_test.rs`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`（`gpg.program` 级联、`~/` 展开与读取失败）、`COMPATIBILITY.md` config 行、网站 `config.en.md`（W-1..W-6，`--only config`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=gpg.program解析契约; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CS-03,DEP-CS-07; writeset=序列化于 CS-03; release=independent; split-from=CS-03a; exception=N/A`

---

### Task CS-03a: 接入 `gpg.program` 外部 OpenPGP 签名（#783）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03` 拆出：`gpg.format` 为 `openpgp` 或未设置且 `CS-03f` 的解析结果为 `Some(prog)` 时，按 Git `gpg-interface.c:1015-1022` 以 `<prog> --status-fd=2 -bsau <key>` 签名，stdin 为去除签名的提交载荷；`<key>` 依次取 `-S<keyid>`、`user.signingkey`、提交者身份（Git `get_signing_key`，`gpg-interface.c:950-960`）；本卡只交付调用方式、签名键选择与成功路径（程序按约定输出签名块与状态行时写入 `gpgsig`）。解析结果为 `None` 时保持 vault 签名路径。已拆出 `CS-03f`、`CS-03d`。与 `CS-03d`、`CS-03g` 组成 `REL-CS-07` 家族同版发布，使外部签名与外部验签、merge 信任门在同一版本公开。签名结果的成功判据与失败时的中止（非零退出、缺少独立 `SIG_CREATED` 行、空或非签名输出）由同家族 `CS-03j` 实现并验收（家族内中间状态不发布）。
**Out of scope:** `gpg.program` 解析契约（由 `CS-03f` 承接）；外部程序验签（由 `CS-03d` 承接）；SSH 签名（由 `REL-CS-01` 承接）；`log`/`show --show-signature`（`DEFER-CS-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| commit 签名只走 vault PGP | `src/command/commit.rs:3088-3151` |
| Git 外部签名参数与默认签名键 | `gpg-interface.c:950-960,1015-1022` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 测试脚本 `fake-gpg` 放入隔离 `$PATH`，记录 argv 与 stdin 后输出固定 `-----BEGIN PGP SIGNATURE-----` 块，并在 stderr（`--status-fd=2`）输出完整状态行 `[GNUPG:] SIG_CREATED D 1 8 00 1700000000 0123456789ABCDEF0123456789ABCDEF01234567`；`fake-gpg-fail` 以退出码 2 输出固定 stderr；提交者为 `C O <co@example.com>`，与 `user.signingkey=U1` 不同，用于区分键选择错误。另有 `fake-gpg-nostatus`：退出码 0、输出签名块但不输出任何以 `[GNUPG:] SIG_CREATED ` 开头的行。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`gpg.program=fake-gpg` 时 `libra commit -S -m a` 生成的 `gpgsig` 与 `fake-gpg` 输出的签名块逐字节一致。
- [ ] AC2：`fake-gpg` 收到的 stdin 与该提交去除 `gpgsig` 头后的对象字节逐字节一致。
- [ ] AC3：`libra commit -SK2 -m b` 时 `fake-gpg` 收到的 argv 为 `--status-fd=2 -bsau K2`。
- [ ] AC4：未传 keyid 且 `user.signingkey=U1` 时 argv 末两项为 `-bsau U1`。
- [ ] AC5：未传 keyid 且未设 `user.signingkey` 时 argv 末项为 `C O <co@example.com>`。
- [ ] AC6：未配置 `gpg.program`（local/global/system 均无）且 vault 签名密钥可用时，`libra commit -S -m d` 生成的 `gpgsig` 用仓库 vault 公钥对提交载荷验签通过（`fake-gpg` 不参与）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_program_sign)'`（V-2；new；覆盖 AC1–AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-07` 的 `T-2` 覆盖）
**Dependencies:** `CS-03f`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `CLAUDE.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-07` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`gpg.program` 签名、签名键选择）、`CLAUDE.md`（Commit & PR Conventions 中 `-S` 只用仓库 vault 签名的表述，补充配置 `gpg.program` 时改用外部程序）、`COMPATIBILITY.md` commit 签名行（`#783` 部分闭环，见 `DEFER-CS-03`）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-07 (1/5)`
**C/D coverage from:** `REL-CS-07`
**Granularity:** `type=implementation; axis=gpg.program外部签名; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-03f,DEP-CS-07; writeset=序列化于 CS-03f; release=family child of REL-CS-07 (1/5); split-from=CS-03; exception=N/A`

---

### Task CS-03j: `gpg.program` 外部签名的失败判定：非零退出、缺少 `SIG_CREATED`、空签名（#783）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03a` 拆出，实现并验收外部签名结果的判定（`CS-03a` 只交付调用与成功路径）：外部签名须满足 Git `gpg-interface.c:1029-1046` 的两项判据——进程退出码为 0、状态输出含以 `[GNUPG:] SIG_CREATED ` 开头的独立行——并满足 Libra 额外的 fail-closed 加固：stdout 为以 `-----BEGIN PGP SIGNATURE-----` 开头的非空签名块（Git 不检查 stdout 是否为空或是否为签名块，该差异登记 `COMPATIBILITY.md`）；任一不满足即以错误中止提交、不写入提交与 ref，错误诊断包含外部程序的 stderr 输出（Git 以 `gpg failed to sign the data:` 后接状态输出报错，`gpg-interface.c:1041-1046`；AC1 以固定文本验收「包含」，不承诺逐字节原样）。
**Out of scope:** 签名调用、键选择与 vault 回退（由 `CS-03a` 承接）；验签（由 `CS-03d` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 外部签名只检查退出码与独立的 `SIG_CREATED` 行，不检查 stdout 签名内容 | `gpg-interface.c:1029-1046` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 隔离 `$PATH` 中的测试脚本：`fake-gpg-fail` 以退出码 2 结束，但 stdout 输出完整有效的 `-----BEGIN PGP SIGNATURE-----` 签名块、stderr 输出独立的完整 `[GNUPG:] SIG_CREATED ` 状态行与固定诊断文本（除退出码外其余成功条件全部满足，只检查状态行或签名块的实现无法拒绝它）；`fake-gpg-nostatus` 退出码 0、输出签名块但 stderr 不含 `[GNUPG:] SIG_CREATED ` 字样；`fake-gpg-inline` 退出码 0、输出有效签名块，stderr 只在行内含该字样（形如 `note: [GNUPG:] SIG_CREATED D 1 8 00 1700000000 <fpr>`，没有以它开头的行，Git `gpg-interface.c:1034-1038` 要求位于行首）；`fake-gpg-empty` 退出码 0、输出完整 `SIG_CREATED` 状态行但 stdout 为空；`fake-gpg-garbage` 退出码 0、输出完整状态行但 stdout 为不以 `-----BEGIN PGP SIGNATURE-----` 开头的文本。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`gpg.program=fake-gpg-fail` 时 `libra commit -S -m c` 以 stderr 含该程序固定 stderr 文本的错误失败。
- [ ] AC2：`gpg.program=fake-gpg-nostatus` 时 `libra commit -S -m e` 以 `LBR-AUTH-001` 失败。
- [ ] AC3：`gpg.program=fake-gpg-empty` 时 `libra commit -S -m f` 以 `LBR-AUTH-001` 失败。
- [ ] AC4：`gpg.program=fake-gpg-garbage` 时 `libra commit -S -m g` 以 `LBR-AUTH-001` 失败。
- [ ] AC5：`gpg.program=fake-gpg-inline` 时 `libra commit -S -m h` 以 `LBR-AUTH-001` 失败（行内出现的状态字样不算 `SIG_CREATED` 行）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_program_sign_failures)'`（V-2；new；覆盖 AC1–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-07` 的 `T-2` 覆盖）
**Dependencies:** `CS-03a`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-07`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-07` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（外部签名失败判据与错误）、`COMPATIBILITY.md` commit 签名行（登记空签名与非签名输出被拒绝属 Libra 加固、与 Git 不同）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-07 (2/5)`
**C/D coverage from:** `REL-CS-07`
**Granularity:** `type=implementation; axis=gpg.program外部签名失败判定; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-03a,DEP-CS-07; writeset=序列化于 CS-03a; release=family child of REL-CS-07 (2/5); split-from=CS-03a; exception=N/A`

---

### Task CS-03d: 接入 `gpg.program` 外部 OpenPGP 验签与状态解析（#783）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03a` 拆出：`verify_commit_signature`（`src/command/commit.rs:3173-3200`）在 `CS-03f` 解析结果为 `Some(prog)` 时，按 Git `gpg-interface.c:368-380` 把 detached 签名写入临时文件并以 `<prog> --keyid-format=long --status-fd=1 --verify <sigfile> -`（stdin 为去除签名的载荷）验签；按 Git `parse_gpg_output`（`gpg-interface.c:170-250`）解析 `[GNUPG:]` 状态行：只有恰好一条签名结果且为 `GOODSIG`、进程退出码为 0 时返回 `Good`，并把 `TRUST_*` 行解析为信任级别，`CommitSignatureStatus::Good` 改为携带该级别（`Good { trust }`，vault PGP 路径记为 `Fully`）供 `CS-03g` 使用；临时文件在返回前删除。未配置时保持 vault 验签。已拆出 `CS-03g`。与 `CS-03a`、`CS-03g` 组成 `REL-CS-07` 家族同版发布，使 `merge --verify-signatures` 不会在中间版本放行未受信任的外部签名。因 `Good` 改为携带信任级别，本卡同时把 `src/command/merge.rs:1638` 的模式匹配改为只放行 `Good { trust: Fully | Ultimate }`、其余按既有 `BadMergeSignature` 拒绝（比最终的 marginal 门更严，保证家族内中间状态 fail-closed 且可编译；vault 路径记为 `Fully`，行为不变），由 `CS-03g` 放宽到 marginal 并补端到端验收。
**Out of scope:** 外部签名（由 `CS-03a` 承接）；SSH 验签（由 `CS-03c` 承接）；`log/show --show-signature`（`DEFER-CS-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `verify_commit_signature` 只按 vault PGP 校验；`merge --verify-signatures` 以其结果决定是否合并 | `src/command/commit.rs:3156-3200`; `src/command/merge.rs:1634-1645` |
| Git 外部验签参数 | `gpg-interface.c:368-380` @ `6de20f6092` |
| Git 验签后删除临时签名文件 | `gpg-interface.c:614-616` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 隔离 `GNUPGHOME`，`gpg.conf` 含 `trust-model direct`；用真实 `gpg` 生成测试密钥 K（ownertrust ultimate）并签出提交 X（`gpg.program=gpg`）；用不在 keyring 中的密钥 J 签出提交 Y；已导入但 ownertrust 未设置的密钥 U 签出分支 `untrusted` 的提交 Z；`fail-gpg` 输出 `[GNUPG:] GOODSIG` 但退出码为 2；`conflict-gpg` 退出码 0 但同时输出 `GOODSIG` 与 `BADSIG` 两条状态；`TMPDIR` 指向本用例独占的空目录（验签临时文件只能建在其中）。真实 `gpg` 缺失时用例失败而非跳过。真实 `gpg` 对 X 返回 `Good` 即证明传入的签名文件与 stdin 载荷正确（篡改载荷的 AC2 为对照）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`gpg.program=gpg` 时 `verify_commit_signature` 对 X 返回 `Good`。
- [ ] AC2：篡改 X 的消息（签名不变）后返回值不是 `Good`。
- [ ] AC3：对 Y 返回值不是 `Good`。
- [ ] AC4：`gpg.program=fail-gpg` 时对 X 返回值不是 `Good`。
- [ ] AC5：`gpg.program=conflict-gpg` 时对 X 返回值不是 `Good`。
- [ ] AC6：AC1 的调用返回后 `TMPDIR` 目录为空（成功路径删除临时签名文件）。
- [ ] AC7：AC4 的调用返回后 `TMPDIR` 目录为空（外部程序失败路径同样删除）。
- [ ] AC8：`gpg.program=gpg` 时 `verify_merge_commit_signature`（`src/command/merge.rs:1630-1645`）对 Z 返回 `Err(MergeError::BadMergeSignature { .. })`（中间状态 fail-closed）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_uses_gpg_program)'`（V-2；new，`src/command/commit.rs` 新增 `#[cfg(test)] mod tests`；覆盖 AC1–AC7）
- [ ] V2 `source .env.test && cargo nextest run --lib -E 'test(=command::merge::tests::verify_merge_signature_rejects_untrusted_gpg_program)'`（V-2；new，加入 `src/command/merge.rs:10209-10211` 既有的 `#[cfg(test)] mod tests`；覆盖 AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^command::commit::tests::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::gpg_sign::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-07` 的 `T-2` 覆盖）
**Dependencies:** `CS-03j`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-07`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/command/merge.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-07` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（外部程序验签与状态解析）、`COMPATIBILITY.md` commit 验签行（`#783` 部分闭环，见 `DEFER-CS-03`）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-07 (3/5)`
**C/D coverage from:** `REL-CS-07`
**Granularity:** `type=implementation; axis=gpg.program外部验签; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=CS-03j,DEP-CS-07; writeset=序列化于 CS-03j; release=family child of REL-CS-07 (3/5); split-from=CS-03a; exception=N/A`

---

### Task CS-03g: `merge --verify-signatures` 接受外部程序验签并按信任级别放行（#783）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03d` 拆出：`merge --verify-signatures`（`src/command/merge.rs:1634-1645`）把 `CS-03d` 的 fail-closed 门（只放行 fully/ultimate）放宽为 Git 默认的「信任级别至少为 marginal」（Git `commit.c:1370-1392` `verify_merge_signature` 默认的 `TRUST_MARGINAL`，状态行 `TRUST_MARGINAL`/`TRUST_FULLY`/`TRUST_ULTIMATE`），否则以既有 `BadMergeSignature`（`LBR-REPO-003`）拒绝；vault PGP 路径行为不变。`gpg.minTrustLevel` 由同家族 `CS-03l` 承接。已拆出 `CS-03l`。
**Out of scope:** 验签本身与状态解析（由 `CS-03d` 承接）；SSH 签名的信任映射（由 `CS-03c` 承接）；`pull` 不启用验签（`src/command/pull.rs:536` 固定 `verify_signatures: false`），不受影响。`gpg.minTrustLevel`（由 `CS-03l` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `verify_merge_commit_signature` 只看 `Good`/`Bad`/`Unsigned` | `src/command/merge.rs:1630-1645` |
| `BadMergeSignature` 映射 `LBR-REPO-003` | `src/command/merge.rs:987-989` |
| Git merge 验签默认要求信任级别 ≥ marginal | `commit.c:1370-1392` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 隔离 `GNUPGHOME`，其 `gpg.conf` 含 `trust-model direct`（密钥有效性等于 ownertrust）：密钥 K 以 `--import-ownertrust` 设为 ultimate 并签出分支 `ok` 的提交；密钥 M 设为 marginal 并签出分支 `marginal` 的提交；密钥 U 已导入但 ownertrust 未设置并签出分支 `untrusted` 的提交；分支 `tampered` 为篡改消息后的 K 签名提交；`gpg.program=gpg`。真实 `gpg` 缺失时用例失败而非跳过。另以 `gpg.program=gpg`、`user.signingkey=<K 的 fingerprint>` 用 `libra commit -S` 在分支 `ext` 上生成外部签名提交。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra merge --verify-signatures ok` 生成合并提交。
- [ ] AC2：`libra merge --verify-signatures marginal` 生成合并提交。
- [ ] AC3：`libra merge --verify-signatures untrusted` 以 `LBR-REPO-003` 失败。
- [ ] AC4：`libra merge --verify-signatures tampered` 以 `LBR-REPO-003` 失败。
- [ ] AC5：`libra merge --verify-signatures ext`（`ext` 的提交由 `libra commit -S` 经 `gpg.program` 签名）生成合并提交。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_with_gpg_program)'`（V-2；new；覆盖 AC1–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::gpg_sign::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-07` 的 `T-2` 覆盖）
**Dependencies:** `CS-03d`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-07`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test/gpg_sign.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-07` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`（`--verify-signatures` 接受外部程序签名与 marginal 信任级别要求）、`COMPATIBILITY.md` merge 验签行、网站 `merge.en.md`（W-1..W-6，`--only merge`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-07 (4/5)`
**C/D coverage from:** `REL-CS-07`
**Granularity:** `type=implementation; axis=merge验签信任门; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-03d,DEP-CS-07; writeset=序列化于 CS-03d; release=family child of REL-CS-07 (4/5); split-from=CS-03d; exception=N/A`

---

### Task CS-03l: `merge --verify-signatures` 按 `gpg.minTrustLevel` 收紧信任门（#783）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03g` 拆出：按 Git `gpg-interface.c:676-687,765-775`，配置 `gpg.minTrustLevel` 时先转大写再解析（大小写不敏感），非法值以 Git 同款 `invalid value for 'gpg.mintrustlevel'` 报错；合法时签名信任级别低于该值即视为验签失败。该门与 `CS-03g` 的 merge 默认 marginal 门同时生效、取更严者（Git `check_signature` 的 `trust_level < configured_min_trust_level` 与 `verify_merge_signature` 的 `trust_level < TRUST_MARGINAL` 两处判定，`gpg-interface.c:684-685`、`commit.c:1383-1386`），配置低于 marginal 的值不放宽 merge 默认门。
**Out of scope:** marginal 默认门与外部程序验签接入（由 `CS-03g`、`CS-03d` 承接）；`gpg.minTrustLevel` 对 SSH 签名的作用（由 `CS-03k` 验收）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git `gpg.minTrustLevel` 解析与比较 | `gpg-interface.c:676-687,765-775` @ `6de20f6092` |
| Git merge 验签另以 `TRUST_MARGINAL` 判定，两门同时生效 | `commit.c:1370-1392` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 隔离 `GNUPGHOME`，其 `gpg.conf` 含 `trust-model direct`（密钥有效性等于 ownertrust）：密钥 K 以 `--import-ownertrust` 设为 ultimate 并签出分支 `ok` 的提交；密钥 M 设为 marginal 并签出分支 `marginal` 的提交；密钥 U 已导入但 ownertrust 未设置并签出分支 `untrusted` 的提交；分支 `tampered` 为篡改消息后的 K 签名提交；`gpg.program=gpg`。真实 `gpg` 缺失时用例失败而非跳过。另以 `gpg.program=gpg`、`user.signingkey=<K 的 fingerprint>` 用 `libra commit -S` 在分支 `ext` 上生成外部签名提交。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`gpg.minTrustLevel=Fully`（混合大小写）时 `libra merge --verify-signatures ok`（ultimate）生成合并提交。
- [ ] AC2：`gpg.minTrustLevel=Fully` 时 `libra merge --verify-signatures marginal` 以 `LBR-REPO-003` 失败。
- [ ] AC3：`gpg.minTrustLevel=undefined` 时 `libra merge --verify-signatures untrusted` 以 `LBR-REPO-003` 失败（低于 marginal 的配置不放宽 merge 默认门）。
- [ ] AC4：`gpg.minTrustLevel=bogus` 时 `libra merge --verify-signatures ok` 以 stderr 含 `invalid value for 'gpg.mintrustlevel'` 的错误失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_min_trust_level)'`（V-2；new；覆盖 AC1–AC4）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::gpg_sign::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-07` 的 `T-2` 覆盖）
**Dependencies:** `CS-03g`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-07`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/merge.rs`, `tests/command/merge_test/gpg_sign.rs`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-07` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`（`gpg.minTrustLevel` 取值、大小写、非法值报错及与 merge 默认门取更严者）、`COMPATIBILITY.md` merge 验签行、网站 `config.en.md`（W-1..W-6，`--only config`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-07 (5/5)`
**C/D coverage from:** `REL-CS-07`
**Granularity:** `type=implementation; axis=gpg.minTrustLevel信任门; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-03g,DEP-CS-07; writeset=序列化于 CS-03g; release=family child of REL-CS-07 (5/5); split-from=CS-03g; exception=N/A`

---

### Task REL-CS-07: `REL-CS-07` `gpg.program` 外部签名、验签与 merge 信任门家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-03a`（1/5）、`CS-03j`（2/5）、`CS-03d`（3/5）、`CS-03g`（4/5）、`CS-03l`（5/5） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-03a`、`CS-03j`、`CS-03d`、`CS-03g`、`CS-03l` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 外部签名（`CS-03a`）、外部程序验签（`CS-03d`）与 merge 信任门（`CS-03g`）须同版发布：否则中间版本或生成本工具无法验证的签名，或 `merge --verify-signatures` 放行已导入但未受信任的签名 | `src/command/commit.rs:3153-3172`; `src/command/merge.rs:1630-1645`; `commit.c:1370-1392` @ `6de20f6092`; `plan-template.md` G-08 |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-03a`、`CS-03j`、`CS-03d`、`CS-03g`、`CS-03l` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `command::commit_test::commit_gpg_program_sign` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit_test::commit_gpg_program_sign_failures` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::commit::tests::verify_commit_signature_uses_gpg_program` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：聚合守卫 `command::merge::tests::verify_merge_signature_rejects_untrusted_gpg_program` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC5：聚合守卫 `command::merge_test::gpg_sign::merge_verify_signatures_with_gpg_program` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC6：聚合守卫 `command::merge_test::gpg_sign::merge_verify_signatures_min_trust_level` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC7：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-07 begin -->` 与 `<!-- release-note-draft:REL-CS-07 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 5 行，顺序同家族登记：`CS-03a`、`CS-03j`、`CS-03d`、`CS-03g`、`CS-03l`）。
- [ ] AC8：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_program_sign)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_gpg_program_sign_failures)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_uses_gpg_program)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 `source .env.test && cargo nextest run --lib -E 'test(=command::merge::tests::verify_merge_signature_rejects_untrusted_gpg_program)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC4）
- [ ] V5 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_with_gpg_program)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC5）
- [ ] V6 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_min_trust_level)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC6）
- [ ] V7 手工证据（覆盖 AC7）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-07 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-07 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-03a CS-03j CS-03d CS-03g CS-03l; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V8 手工证据（覆盖 AC8）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-03a` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-03j` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-03d` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-03g` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-03l` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-03a`, `CS-03j`, `CS-03d`, `CS-03g`, `CS-03l`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-03a`、`CS-03j`、`CS-03d`、`CS-03g`、`CS-03l` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-07`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-07家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/12; VER=8/12; landing=0; prod-files=0; scope=S; deps=CS-03a,CS-03j,CS-03d,CS-03g,CS-03l; writeset=序列化于 CS-03l; release=family release point of REL-CS-07; split-from=N/A; exception=N/A`

---

### Task CS-03b: 实现 `gpg.format=ssh` 签名生成与临时文件清理（#787）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03` 拆出：`gpg.format=ssh` 时按 Git `gpg-interface.c` 的 SSH 签名流程，以 `-S"<公钥>"`/`-S"key::<公钥>"`（经 `ssh-agent`）或未给 keyid 时的 `user.signingkey`（私钥文件路径，无需 agent）调用 `gpg.ssh.program`（默认 `ssh-keygen`）`-Y sign -n git` 生成 `-----BEGIN SSH SIGNATURE-----` 签名写入 `gpgsig`，成功与失败路径均清理本次创建的全部临时文件。已拆出 `CS-03c`。本卡同时把 `CS-03` 的过渡断言（`gpg.format=ssh` 以 `LBR-UNSUPPORTED-001` 拒绝，用例 `command::commit_test::commit_gpg_format_strict_values`）改为断言 SSH 签名成功生成，并在本卡 `Current evidence` 记录该改动。
**Out of scope:** SSH 验签（由 `CS-03c` 承接）；缺钥与 agent 缺失的 fail-closed 诊断（由 `CS-03e` 承接）；`gpg.ssh.defaultKeyCommand`（尚未排期）；X.509（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `commit.rs` 签名只走 vault PGP，无 SSH 分支 | `src/command/commit.rs:3088-3151` |

**夹具（FX-BASE 之外的差异化条件）:** 测试内生成三把可区分的 ed25519 测试密钥 K1、K2、K3；隔离 `ssh-agent`（独立 `SSH_AUTH_SOCK`）加载 K1、K2 私钥；K3 只有私钥文件；`TMPDIR` 指向本用例独占的空目录；`gpg.ssh.program` 可指向记录 argv 后转调真实 `ssh-keygen` 的包装脚本；AC7 的失败脚本先转调真实 `ssh-keygen` 写出有效的 `.sig` 文件再以退出码 1 结束（除退出码外签名产物齐全，只检查签名文件的实现无法拒绝它）；验签时把提交原始对象写入临时 Git 仓库，以只含指定公钥的 `gpg.ssh.allowedSignersFile` 运行系统 `git verify-commit`；真实 `git` 与 `ssh-keygen` 不可用时用例以失败而非跳过结束。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`gpg.format=ssh` 下 `libra commit -S"key::<K1 公钥>" -m a` 生成的提交，在 allowed signers 只含 K1 时 `git verify-commit` 退出码为 0。
- [ ] AC2：`gpg.format=ssh` 下 `libra commit -S"ssh-ed25519 <K2 公钥>" -m b` 生成的提交，在 allowed signers 只含 K2 时 `git verify-commit` 退出码为 0。
- [ ] AC3：未传 keyid、`user.signingkey` 为 K3 私钥文件路径且未设置 `SSH_AUTH_SOCK` 时，`libra commit -S -m c` 生成的提交在 allowed signers 只含 K3 时 `git verify-commit` 退出码为 0。
- [ ] AC4：`gpg.ssh.program` 指向包装脚本时，脚本记录的参数含 `-Y sign -n git`。
- [ ] AC5：对照：AC2 的提交在 allowed signers 只含 K1 时 `git verify-commit` 退出码非 0。
- [ ] AC6：AC1 签名成功后 `TMPDIR` 目录为空。
- [ ] AC7：`gpg.ssh.program` 以非零退出时 `libra commit -S -m d` 以 `LBR-AUTH-001` 失败。
- [ ] AC8：AC7 的失败路径结束后 `TMPDIR` 目录为空。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_ssh_signing_generation)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-01` 的 `T-2` 覆盖）
**Dependencies:** `REL-CS-07`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/command/config.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-01` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/{commit,config}.md`、zh-CN 同名文件、`docs/development/commands/{commit,config}.md`（`gpg.format=ssh`、`user.signingkey`、`gpg.ssh.program` 与 agent 用法）、`COMPATIBILITY.md` commit 签名行、网站 `commit.en.md`、`config.en.md`（W-1..W-6，`--only commit config`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-01 (1/6)`
**C/D coverage from:** `REL-CS-01`
**Granularity:** `type=implementation; axis=SSH签名生成与临时文件清理; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=REL-CS-07,DEP-CS-07; writeset=序列化于 REL-CS-07; release=family child of REL-CS-01 (1/6); split-from=CS-03; exception=N/A`

---

### Task CS-03c: 实现 `gpg.format=ssh` 签名验证与 `merge --verify-signatures` 接入（#787）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03b` 拆出：`verify_commit_signature` 对 `-----BEGIN SSH SIGNATURE-----` 签名按 Git `gpg-interface.c:400-445` 验证：先以 `gpg.ssh.program -Y find-principals` 在 `gpg.ssh.allowedSignersFile` 中查找签名者，找到时 `-Y verify -n git` 成功记为 `Good`、信任级别 fully；找不到时以 `-Y check-novalidate` 只校验签名本身，成功记为 `Good`、信任级别 undefined；篡改的签名不返回 `Good`；`gpg.ssh.allowedSignersFile` 未配置时按 Git 报 `gpg.ssh.allowedSignersFile needs to be configured and exist for ssh signature verification` 且不调用 `find-principals`/`check-novalidate`，结果不是 `Good`（`gpg-interface.c:483-486`）；已配置但路径不存在时 `find-principals` 失败，按 Git 退化为 `check-novalidate`，签名本身有效即 `Good { trust: Undefined }`（Git `sigc->result` 为 `G`、信任 undefined，整体验签状态失败，`gpg-interface.c:516-535,437-440`），因而被 merge 信任门拒绝。`merge --verify-signatures` 经 `CS-03g` 的信任门因此只接受 allowed signers 中的签名者。按提交者时间验证（`-Overify-time`）由同家族 `CS-03h` 承接。多 principal 逐个尝试与 `gpg.minTrustLevel` 对 SSH 的作用由同家族 `CS-03k` 承接。已拆出 `CS-03e`、`CS-03h`、`CS-03k`。
**Out of scope:** SSH 签名生成（由 `CS-03b` 承接）；签名侧 fail-closed 诊断（由 `CS-03e` 承接）；按提交者时间验证（由 `CS-03h` 承接）；`gpg.ssh.revocationFile`（由 `CS-03i` 承接）；`log/show --show-signature`（`DEFER-CS-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `verify_commit_signature` 只按 vault PGP 校验；`merge --verify-signatures` 以其结果决定是否合并 | `src/command/commit.rs:3156-3200`; `src/command/merge.rs:1634-1645` |
| `BadMergeSignature` 映射 `LBR-REPO-003` | `src/command/merge.rs:987-989` |
| Git SSH 验签结果与信任级别（已知 principal 为 fully，未知为 undefined） | `gpg-interface.c:400-445` @ `6de20f6092` |
| Git 未配置 allowed signers 时直接报错返回；已配置路径不存在时退化为 `check-novalidate` | `gpg-interface.c:483-486,516-535` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** ed25519 测试密钥 A（allowed signers 中登记）与 B（未登记）；用临时 Git 仓库以 `git -c gpg.format=ssh -c user.signingkey=<A 私钥> commit -S` 生成真实 Git SSH 签名提交，原始对象导入 Libra 仓库；真实 `git`/`ssh-keygen` 缺失时用例失败而非跳过。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：allowed signers 含 A 时 `verify_commit_signature` 对 Git 生成的 A 签名提交返回 `Good { trust: Fully }`。
- [ ] AC2：篡改该提交消息（签名不变）后返回值不是 `Good`。
- [ ] AC3：B 签名的提交在 allowed signers 只含 A 时返回 `Good { trust: Undefined }`（签名本身有效，Git `gpg-interface.c:438-441`）。
- [ ] AC4：`gpg.ssh.allowedSignersFile` 指向不存在的文件时对 A 签名提交返回 `Good { trust: Undefined }`（不是 `Fully`）。
- [ ] AC5：未配置 `gpg.ssh.allowedSignersFile` 时对 A 签名提交返回值不是 `Good`（Git 在该情形不进入 `find-principals`/`check-novalidate`）。
- [ ] AC6：`libra merge --verify-signatures <A 签名提交所在分支>` 生成合并提交。
- [ ] AC7：`libra merge --verify-signatures <篡改提交所在分支>` 以 `LBR-REPO-003` 失败。
- [ ] AC8：`libra merge --verify-signatures <B 签名提交所在分支>` 以 `LBR-REPO-003` 失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_ssh_cases)'`（V-2；new，位于 `src/command/commit.rs` 的 `#[cfg(test)] mod tests`；覆盖 AC1–AC5）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_with_ssh)'`（V-2；new；覆盖 AC6–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::gpg_sign::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-01` 的 `T-2` 覆盖）
**Dependencies:** `CS-03b`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-01`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/merge_test/gpg_sign.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-01` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/{commit,merge}.md`、zh-CN 同名文件与 `docs/development/commands/{commit,merge}.md`（SSH 验签、allowed signers、`merge --verify-signatures` 接受 SSH 签名）、`COMPATIBILITY.md` commit 与 merge 验签行、网站 `commit.en.md`、`merge.en.md`（W-1..W-6，`--only commit merge`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-01 (2/6)`
**C/D coverage from:** `REL-CS-01`
**Granularity:** `type=implementation; axis=SSH签名验证; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CS-03b,DEP-CS-07; writeset=序列化于 CS-03b; release=family child of REL-CS-01 (2/6); split-from=CS-03b; exception=N/A`

---

### Task CS-03h: SSH 验签按提交者时间传递 `-Overify-time`（#787）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03c` 拆出：按 Git `gpg-interface.c:499-527`（`parse_payload_metadata`，`gpg-interface.c:625-656`），SSH 验签从提交载荷的 `committer` 行取时间戳，以本地时区格式化为 `%Y%m%d%H%M%S`（`gpg-interface.c:476-480`），作为 `-Overify-time=<t>` 传给 `-Y find-principals`、`-Y check-novalidate` 与 `-Y verify`，使 allowed signers 的 `valid-after`/`valid-before` 按提交时间判定；不在提交时间有效期内的签名者按「找不到 principal」处理（`Good { trust: Undefined }`）。吊销文件由同家族 `CS-03i` 承接。
**Out of scope:** 验签主流程与 merge 接入（由 `CS-03c` 承接）；`gpg.ssh.revocationFile`（由 `CS-03i` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 以提交者时间构造 `-Overify-time` | `gpg-interface.c:476-480,499-527,625-656` @ `6de20f6092` |
| `verify_commit_signature` 入口 | `src/command/commit.rs:3156-3200` |
| `find-principals`、`check-novalidate` 与 `verify` 三处调用都追加同一 `-Overify-time` | `gpg-interface.c:500-527,570-576` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** `TZ=Asia/Shanghai`（+0800）；ed25519 密钥 C 在 allowed signers 中带 `valid-before="20200101"`，密钥 D 带 `valid-after="20300101"`；用临时 Git 仓库以真实 `git commit -S`（`gpg.format=ssh`）生成：C 签名、`GIT_COMMITTER_DATE=2019-06-01T12:00:00Z`、`GIT_AUTHOR_DATE=2020-06-01T12:00:00Z` 的提交 X1（分支 `c2019`）；D 签名、`GIT_COMMITTER_DATE=2026-01-01T00:00:00Z`、`GIT_AUTHOR_DATE=2031-01-01T00:00:00Z` 的提交 X2（分支 `d2026`）；D 签名、`GIT_COMMITTER_DATE=2031-01-01T00:00:00Z`、`GIT_AUTHOR_DATE=2026-01-01T00:00:00Z` 的提交 X3；作者时间均落在有效期边界的另一侧，使误读 author 时间的实现得到相反结果；原始对象导入 Libra 仓库；记录型 `gpg.ssh.program` 包装脚本记录 argv 后转调真实 `ssh-keygen`；真实 `git`/`ssh-keygen` 缺失时用例失败而非跳过。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`verify_commit_signature` 对 X1（提交时 C 在有效期内、当前已过期）返回 `Good { trust: Fully }`。
- [ ] AC2：对 X2（提交时 D 尚未生效）返回 `Good { trust: Undefined }`。
- [ ] AC3：对 X3（提交时 D 已生效、当前尚未生效）返回 `Good { trust: Fully }`。
- [ ] AC4：验证 X1 时包装脚本记录的 `-Y find-principals` argv 含 `-Overify-time=20190601200000`（提交者时间按本地 `+0800` 格式化，非 UTC）。
- [ ] AC5：`libra merge --verify-signatures c2019` 生成合并提交。
- [ ] AC6：`libra merge --verify-signatures d2026` 以 `LBR-REPO-003` 失败。
- [ ] AC7：验证 X2 时包装脚本记录的 `-Y check-novalidate` argv 含 `-Overify-time=20260101080000`（`find-principals` 在提交时间找不到 D 后退化为 `check-novalidate`）。
- [ ] AC8：验证 X1 时包装脚本记录的 `-Y verify` argv 含 `-Overify-time=20190601200000`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_ssh_verify_time)'`（V-2；new，位于 `src/command/commit.rs` 的 `#[cfg(test)] mod tests`；覆盖 AC1–AC4、AC7–AC8）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_ssh_verify_time)'`（V-2；new；覆盖 AC5–AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::gpg_sign::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-01` 的 `T-2` 覆盖）
**Dependencies:** `CS-03c`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-01`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/merge_test/gpg_sign.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-01` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/{commit,merge}.md`、zh-CN 同名文件与 `docs/development/commands/{commit,merge}.md`（SSH 验签按提交时间判定 allowed signers 有效期）、`COMPATIBILITY.md` commit 与 merge 验签行、网站 `commit.en.md`、`merge.en.md`（W-1..W-6，`--only commit merge`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-01 (3/6)`
**C/D coverage from:** `REL-CS-01`
**Granularity:** `type=implementation; axis=SSH验签提交时间; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CS-03c,DEP-CS-07; writeset=序列化于 CS-03c; release=family child of REL-CS-01 (3/6); split-from=CS-03c; exception=N/A`

---

### Task CS-03i: SSH 验签支持 `gpg.ssh.revocationFile` 吊销文件（#787）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03h` 拆出：按 Git `gpg-interface.c:579-587,785-786`，配置 `gpg.ssh.revocationFile` 且文件存在时向 `-Y verify` 追加 `-r <file>`，被吊销的签名者不返回 `Good`；配置但文件不存在时输出 Git 同款警告 `ssh signing revocation file configured but not found: <path>` 并按未配置继续验签。
**Out of scope:** 提交时间有效期（由 `CS-03h` 承接）；验签主流程（由 `CS-03c` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 向 `-Y verify` 传吊销文件，缺失时警告 | `gpg-interface.c:579-587,785-786` @ `6de20f6092` |
| `verify_commit_signature` 入口 | `src/command/commit.rs:3156-3200` |

**夹具（FX-BASE 之外的差异化条件）:** ed25519 密钥 R 登记在 allowed signers 中；用临时 Git 仓库以真实 `git commit -S`（`gpg.format=ssh`）生成 R 签名的提交 X4（分支 `rsig`），原始对象导入 Libra 仓库；吊销文件 `rev-with-r` 含 R 的公钥，`rev-without-r` 只含另一把无关公钥；`missing` 为保证不存在的路径；真实 `git`/`ssh-keygen` 缺失时用例失败而非跳过。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`gpg.ssh.revocationFile=rev-with-r` 时 `verify_commit_signature` 对 X4 返回值不是 `Good`。
- [ ] AC2：对照：`gpg.ssh.revocationFile=rev-without-r` 时对 X4 返回 `Good { trust: Fully }`。
- [ ] AC3：`gpg.ssh.revocationFile=rev-with-r` 时 `libra merge --verify-signatures rsig` 以 `LBR-REPO-003` 失败。
- [ ] AC4：`gpg.ssh.revocationFile=missing` 时 `libra merge --verify-signatures rsig` 的 stderr 含 `ssh signing revocation file configured but not found`。
- [ ] AC5：`gpg.ssh.revocationFile=missing` 时 `libra merge --verify-signatures rsig` 生成合并提交（按未配置继续验签）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_ssh_revocation)'`（V-2；new，位于 `src/command/commit.rs` 的 `#[cfg(test)] mod tests`；覆盖 AC1–AC2）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_ssh_revocation)'`（V-2；new；覆盖 AC3–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::gpg_sign::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-01` 的 `T-2` 覆盖）
**Dependencies:** `CS-03h`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-01`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/merge_test/gpg_sign.rs`, `docs/commands/config.md`, `docs/commands/zh-CN/config.md`, `docs/development/commands/config.md`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-01` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/{config,merge}.md`、zh-CN 同名文件与 `docs/development/commands/{config,merge}.md`（`gpg.ssh.revocationFile` 格式、吊销效果与文件缺失警告）、`COMPATIBILITY.md` merge 验签行、网站 `config.en.md`、`merge.en.md`（W-1..W-6，`--only config merge`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-01 (4/6)`
**C/D coverage from:** `REL-CS-01`
**Granularity:** `type=implementation; axis=SSH验签吊销文件; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CS-03h,DEP-CS-07; writeset=序列化于 CS-03h; release=family child of REL-CS-01 (4/6); split-from=CS-03h; exception=N/A`

---

### Task CS-03k: SSH 验签逐个尝试多个 principal，并受 `gpg.minTrustLevel` 约束（#787）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03c` 拆出：按 Git `gpg-interface.c:537-571`，`-Y find-principals` 返回多个 principal 时逐个以 `-Y verify -I <principal>` 尝试，任一成功即为 `Good { trust: Fully }`；按 `gpg-interface.c:679-685`，`CS-03l` 交付的 `gpg.minTrustLevel` 判定在格式专属验签之后统一比较，对 SSH 签名同样生效。
**Out of scope:** 验签主流程（由 `CS-03c` 承接）；`gpg.minTrustLevel` 解析（由 `CS-03l` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 逐个尝试 principal；最低信任级别统一比较 | `gpg-interface.c:537-571,679-685` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** ed25519 密钥 P 在 allowed signers 中登记为两行：`p1 namespaces="other" <P 公钥>` 在前、`p2 namespaces="git" <P 公钥>` 在后；用临时 Git 仓库以真实 `git commit -S`（`gpg.format=ssh`、allowed signers 只含 `p2` 行）生成 P 签名提交 X5（分支 `psig`）；另有在 allowed signers 中以单行 `a namespaces="git" <A 公钥>` 登记的密钥 A，以同样方式生成 A 签名的提交（分支 `asig`）；原始对象导入 Libra 仓库；真实 `git`/`ssh-keygen` 缺失时用例失败而非跳过。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`verify_commit_signature` 对 X5 返回 `Good { trust: Fully }`（第一个 principal `p1` 验证失败后继续尝试 `p2`）。
- [ ] AC2：`libra merge --verify-signatures psig` 生成合并提交。
- [ ] AC3：`gpg.minTrustLevel=ultimate` 时 `libra merge --verify-signatures asig` 以 `LBR-REPO-003` 失败（SSH 已知签名者为 fully，低于 ultimate）。
- [ ] AC4：对照：`gpg.minTrustLevel=fully` 时 `libra merge --verify-signatures asig` 生成合并提交。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_ssh_multi_principal)'`（V-2；new，位于 `src/command/commit.rs` 的 `#[cfg(test)] mod tests`；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_ssh_trust_rules)'`（V-2；new；覆盖 AC2–AC4）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::merge_test::gpg_sign::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-01` 的 `T-2` 覆盖）
**Dependencies:** `CS-03i`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-01`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/command/merge.rs`, `tests/command/merge_test/gpg_sign.rs`, `docs/commands/merge.md`, `docs/commands/zh-CN/merge.md`, `docs/development/commands/merge.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-01` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/merge.md`、`docs/commands/zh-CN/merge.md`、`docs/development/commands/merge.md`（SSH 多 principal 与 `gpg.minTrustLevel` 对 SSH 签名生效）、`COMPATIBILITY.md` merge 验签行、网站 `merge.en.md`（W-1..W-6，`--only merge`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-01 (5/6)`
**C/D coverage from:** `REL-CS-01`
**Granularity:** `type=implementation; axis=SSH验签principal与信任门; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=CS-03i,DEP-CS-07; writeset=序列化于 CS-03i; release=family child of REL-CS-01 (5/6); split-from=CS-03c; exception=N/A`

---

### Task CS-03e: 实现 SSH 签名缺钥与 agent 缺失的 fail-closed 诊断（#787）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-03c` 拆出：`gpg.format=ssh` 且决定签名时，若既无 `-S<key>` 也无 `user.signingkey`，或签名键为需要 agent 的字面公钥而 `SSH_AUTH_SOCK` 不可用，提交以明确诊断失败，不回退 vault 签名、不创建提交。
**Out of scope:** 签名生成与验签（由 `CS-03b`、`CS-03c` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| commit 签名只走 vault PGP，无 SSH 缺钥分支 | `src/command/commit.rs:3088-3151` |
| 签名失败映射 `LBR-AUTH-001` | `src/command/commit.rs:566-568` |

**夹具（FX-BASE 之外的差异化条件）:** `gpg.format=ssh`；场景一不设 `user.signingkey`；场景二 `-S"key::<公钥>"` 且未设置 `SSH_AUTH_SOCK`；对照：私钥文件路径作为 `user.signingkey` 时同环境签名成功。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`gpg.format=ssh`、未传 keyid 且未设 `user.signingkey` 时 `libra commit -S -m a` 以 stderr 含 `user.signingkey` 的错误失败。
- [ ] AC2：`gpg.format=ssh` 且未设置 `SSH_AUTH_SOCK` 时 `libra commit -S"key::<公钥>" -m b` 以 stderr 含 `ssh-agent` 的错误失败。
- [ ] AC3：对照：`user.signingkey` 为私钥文件路径时同一环境下 `libra commit -S -m c` 生成以 `-----BEGIN SSH SIGNATURE-----` 开头的 `gpgsig`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_ssh_signing_fail_closed)'`（V-2；new；覆盖 AC1–AC3）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-01` 的 `T-2` 覆盖）
**Dependencies:** `CS-03k`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-01`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-01` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（SSH 签名缺钥与 agent 诊断）、`COMPATIBILITY.md` commit 签名行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-01 (6/6)`
**C/D coverage from:** `REL-CS-01`
**Granularity:** `type=implementation; axis=SSH签名fail-closed诊断; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-03k,DEP-CS-07; writeset=序列化于 CS-03k; release=family child of REL-CS-01 (6/6); split-from=CS-03c; exception=N/A`

---

### Task REL-CS-01: `REL-CS-01` `gpg.format=ssh` 签名与验签家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-03b`（1/6）、`CS-03c`（2/6）、`CS-03h`（3/6）、`CS-03i`（4/6）、`CS-03k`（5/6）、`CS-03e`（6/6） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-03b`、`CS-03c`、`CS-03h`、`CS-03i`、`CS-03k`、`CS-03e` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `CS-03b`、`CS-03c`、`CS-03h`、`CS-03i`、`CS-03k`、`CS-03e` 为 `family child`，经唯一 `release` 卡统一发布（ID 与「SSH 签名与验签」语义不变）；对方计划 `DEP-SA-02` 以本卡 ID 消费 | `plan-template.md` G-08; `plan-20261117-sequencer-am.md` `DEP-SA-02` |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-03b`、`CS-03c`、`CS-03h`、`CS-03i`、`CS-03k`、`CS-03e` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `command::commit_test::commit_ssh_signing_generation` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit::tests::verify_commit_signature_ssh_cases` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::merge_test::gpg_sign::merge_verify_signatures_with_ssh` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：聚合守卫 `command::commit::tests::verify_commit_signature_ssh_verify_time` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC5：聚合守卫 `command::merge_test::gpg_sign::merge_verify_signatures_ssh_verify_time` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC6：聚合守卫 `command::commit::tests::verify_commit_signature_ssh_revocation` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC7：聚合守卫 `command::merge_test::gpg_sign::merge_verify_signatures_ssh_revocation` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC8：聚合守卫 `command::commit::tests::verify_commit_signature_ssh_multi_principal` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC9：聚合守卫 `command::merge_test::gpg_sign::merge_verify_signatures_ssh_trust_rules` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC10：聚合守卫 `command::commit_test::commit_ssh_signing_fail_closed` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC11：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-01 begin -->` 与 `<!-- release-note-draft:REL-CS-01 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 6 行，顺序同家族登记：`CS-03b`、`CS-03c`、`CS-03h`、`CS-03i`、`CS-03k`、`CS-03e`）。
- [ ] AC12：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_ssh_signing_generation)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_ssh_cases)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_with_ssh)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_ssh_verify_time)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC4）
- [ ] V5 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_ssh_verify_time)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC5）
- [ ] V6 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_ssh_revocation)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC6）
- [ ] V7 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_ssh_revocation)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC7）
- [ ] V8 `source .env.test && cargo nextest run --lib -E 'test(=command::commit::tests::verify_commit_signature_ssh_multi_principal)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC8）
- [ ] V9 `source .env.test && cargo nextest run --test command_test -E 'test(=command::merge_test::gpg_sign::merge_verify_signatures_ssh_trust_rules)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC9）
- [ ] V10 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_ssh_signing_fail_closed)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC10）
- [ ] V11 手工证据（覆盖 AC11）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-01 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-01 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-03b CS-03c CS-03h CS-03i CS-03k CS-03e; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V12 手工证据（覆盖 AC12）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-03b` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-03c` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-03h` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-03i` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-03k` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-03e` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-03b`, `CS-03c`, `CS-03h`, `CS-03i`, `CS-03k`, `CS-03e`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-03b`、`CS-03c`、`CS-03h`、`CS-03i`、`CS-03k`、`CS-03e` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-01`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-01家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=12/12; VER=12/12; landing=0; prod-files=0; scope=S; deps=CS-03b,CS-03c,CS-03h,CS-03i,CS-03k,CS-03e; writeset=序列化于 CS-03e; release=family release point of REL-CS-01; split-from=N/A; exception=N/A`

---

### Task CS-04: 对齐 `libra commit --amend` 的 `gpgsig` 丢弃与 `--date` 语义（#770）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 对齐 Git `builtin/commit.c:1931-1932`（`exclude_gpgsig`）：`--amend` 始终丢弃原提交的 `gpgsig`，仅在本次签名决策为签名（显式 `-S`、`commit.gpgSign=true` 或显式 `vault.signing=true`）时对新内容重新签名；`--amend --date=<d>` 按 Git t7501「amend commit to fix date」只替换 author 时间。与 #770 原文「保留原 gpgsig」的偏差记入 `COMPATIBILITY.md`。
**Out of scope:** `notes.rewrite.amend`（由 `CS-19` 承接）；amend 后索引一致性（由 `CS-20` 承接）；`--amend --author` 日期保留（由 `CS-18` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| amend 路径按 `signing_policy` 生成 `gpgsig`，未显式排除原签名语义 | `src/command/commit.rs:1498-1530` |
| Git amend 读取原提交额外头时排除 `gpgsig`/`gpgsig-sha256` | `builtin/commit.c:1931-1932` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** AC1–AC4：原提交在 `vault.signing=true` 下创建（含 `gpgsig`），随后删除该配置项。AC5–AC7：另建原提交未签名的仓库（与 t7501 一致），固定 `GIT_AUTHOR_NAME`/`EMAIL`/`DATE` 与 `GIT_COMMITTER_NAME`/`EMAIL`/`DATE` 且作者≠提交者、时区为 `-0700`，使只改 author 时间的正确实现与「同时改 committer 或丢作者」的错误实现可区分。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：未触发签名时 `libra commit --amend -m x` 生成的新提交不含 `gpgsig` 头。
- [ ] AC2：`vault.signing=true` 时 `libra commit --amend --no-gpg-sign -m y` 生成的新提交不含 `gpgsig` 头。
- [ ] AC3：`libra commit --amend -S -m z` 生成的新提交 `gpgsig` 用仓库公钥对新提交载荷验签通过（以 `pgp` crate 校验）。
- [ ] AC4：`commit.gpgSign=true` 时 `libra commit --amend -m w` 生成的新提交 `gpgsig` 用仓库公钥对新提交载荷验签通过。
- [ ] AC5：在 AC5–AC7 的未签名夹具上按 t7501「amend commit to fix date」（`t/t7501-commit-basic-functionality.sh:435-448` @ `6de20f6092`）运行 `libra commit --amend --no-edit --date=<newtick>`：新提交的 author 行等于原 author 行把时间戳与时区替换为 `<newtick>` 的结果（姓名、邮箱不变）。
- [ ] AC6：同一新提交的 committer 行等于由当前固定的 `GIT_COMMITTER_NAME`、`GIT_COMMITTER_EMAIL` 与 `GIT_COMMITTER_DATE` 构成的期望行。
- [ ] AC7：新、原两提交对象去掉 author 与 committer 两行后逐字节一致。
- [ ] AC8：`vault.signing=true`（未传签名标志、未设置 `commit.gpgSign`）时 `libra commit --amend -m v` 生成的新提交 `gpgsig` 用仓库公钥对新提交载荷验签通过（以 `pgp` crate 校验）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test compat_commit_amend_no_edit -E 'test(=amend_drops_old_gpgsig_unless_resigned)'`（V-2；new；覆盖 AC1–AC4、AC8）
- [ ] V2 `source .env.test && cargo nextest run --test compat_commit_amend_no_edit -E 'test(=amend_date_matches_t7501_fix_date)'`（V-2；new；覆盖 AC5–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test compat_commit_amend_no_edit`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `REL-CS-01`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/compat/commit_amend_no_edit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（amend 丢弃旧签名与重签条件）、`COMPATIBILITY.md` commit 行（与 #770 原文偏差）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--amend丢弃旧gpgsig与--date; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=REL-CS-01,DEP-CS-07; writeset=序列化于 REL-CS-01; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-05: 支持重复 `-m <msg>` 及与 `--fixup` 组合拼接（#772）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 将 `CommitArgs::message` 改为 `Vec<String>`（`ArgAction::Append`），多段 `-m` 以空行分段拼接；移除 `fixup` 与 `message` 的错误互斥，使 `commit --fixup <c> -m a -m b` 生成 `fixup! <subject>` 后接各段；保持 Git `die_for_incompatible_opt4(-m, -c, -C, -F)` 的互斥。
**Out of scope:** `--squash` 与 `-F`/`-m` 组合（由 `CS-06` 承接）；`--fixup=amend:`/`reword:` 与 `-m` 的冲突（由 `CS-07` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `message: Option<String>`；`fixup` 声明 `conflicts_with_all = ["message", ...]` | `src/command/commit.rs:97-98,169-170,1845-1858` |
| Git `-m` 与 `-c`/`-C`/`-F` 互斥 | `builtin/commit.c:1345-1348` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 目标提交主题为 `target subject`；期望字节由执行者用 pinned `6de20f6092` 构建的 git 对同一输入实测得到，作为常量写入用例，并在用例注释记录来源命令。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit -m title -m body` 的提交消息 MSG 与固定期望字节 `title\n\nbody` 一致。
- [ ] AC2：`libra commit --fixup <c> -m something -m extra` 的提交消息 MSG 与固定期望字节 `fixup! target subject\n\nsomething\n\nextra` 一致。
- [ ] AC3：`-m` 与 `-F` 同用以 `LBR-CLI-002` 失败。
- [ ] AC4：`-m` 与 `-C <c>` 同用以 `LBR-CLI-002` 失败。
- [ ] AC5：`-m` 与 `-c <c>` 同用以 `LBR-CLI-002` 失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_multiple_m_and_fixup_m)'`（V-2；new；覆盖 AC1–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_autosquash_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-04`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `tests/command/commit_autosquash_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（重复 `-m` 与 `--fixup -m`）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=重复-m与--fixup-m拼接; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-04,DEP-CS-07; writeset=序列化于 CS-04; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-06: 修复 `--squash` 与 `-F`/`-m`/`-C`/`-c` 共存及编辑器修改回写（#764, #791）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git `builtin/commit.c:784-801`：`--squash <c>` 先写 `squash! <subject>\n\n`（当 `-C`/`-c` 的参数字符串与 `--squash` 的参数字符串完全相同时只写 `squash! ` 前缀，与 Git `!strcmp(use_message, squash_message)` 一致；指向同一提交但字符串不同时仍写完整 `squash! <subject>\n\n`），再接 `-m`/`-F`/`-C`/`-c` 提供的内容；无消息选项或使用 `-c` 时打开编辑器且用户修改按清理规则写回；移除 `squash` 与 `message`/`file` 的错误互斥，保留 `--squash` 与 `--fixup` 互斥。
**Out of scope:** `--fixup=amend:/reword:`（由 `CS-07` 承接）；`rebase --autosquash` 执行侧。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `squash` 声明 `conflicts_with_all = ["message", "file", ...]` | `src/command/commit.rs:180-181,1850-1854` |
| Git `--squash` 消息构造与 `--squash`/`--fixup` 互斥 | `builtin/commit.c:784-801,1339-1340` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 被 squash 的提交主题为 `target subject`，另一提交 d 的完整消息为多行文本；`GIT_EDITOR` 脚本在收到的文件末尾追加 `extra` 行；期望字节由执行者用 pinned `6de20f6092` 构建的 git 对同一输入实测得到，作为常量写入用例，并在用例注释记录来源命令。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --squash <c> -F <file>` 的提交消息 MSG 与固定期望字节（`squash! target subject\n\n` + 文件内容）一致。
- [ ] AC2：`libra commit --squash <c> -m a -m b` 的提交消息 MSG 与固定期望字节 `squash! target subject\n\na\n\nb` 一致。
- [ ] AC3：`libra commit --squash <c>`（无消息选项，编辑器追加 `extra`）的提交消息 MSG 与固定期望字节一致（`squash! target subject`、空行、`extra`）。
- [ ] AC4：`libra commit --squash <c> -C <d>` 的提交消息 MSG 与固定期望字节（`squash! target subject\n\n` + d 的完整消息）一致。
- [ ] AC5：`libra commit --squash <c> -C <c>` 的提交消息 MSG 与固定期望字节（`squash! ` + c 的完整消息，主题不重复）一致。
- [ ] AC6：`libra commit --squash <c> -c <d>`（编辑器追加 `extra`）的提交消息 MSG 与固定期望字节一致（`squash! target subject`、空行、d 的完整消息、`extra`）。
- [ ] AC7：`--squash` 与 `--fixup` 同用以 `LBR-CLI-002` 失败。
- [ ] AC8：`libra commit --squash HEAD -C <HEAD 的完整 OID>`（同一提交、不同参数字符串）的提交消息 MSG 与 pinned Git 实测常量一致（走完整 `squash! <subject>` 分支）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_autosquash_test::commit_squash_message_sources_and_editor)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_autosquash_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-05`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_autosquash_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--squash` 与消息选项组合、编辑器回写）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--squash消息来源组合与编辑回写; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-05,DEP-CS-07; writeset=序列化于 CS-05; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-08: 支持 `libra commit --long` 预览输出（#774）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `CommitArgs` 新增 `--long`：`--long` 隐含 `--dry-run`，输出长格式状态预览而不创建提交；`--long` 与 `--short`/`--porcelain` 同时出现时按 Git 最后出现者生效。Libra 现有实现中 `--short` 与 `--porcelain` 走同一输出（`src/command/commit.rs:1226-1230`；Git 中二者在颜色与子目录相对路径上不同），二者之间的先后不可观测，不在本卡范围（`#774` 只涉及 `--long`），该既有差异登记 `COMPATIBILITY.md`。同时把 `src/cli.rs:2239-2244` 的只读分类从 `--dry-run`/`--porcelain` 扩展到 `--short` 与 `--long`，使全部预览命令按 `MutationClass::ReadOnly` 运行、不写 operation-log（V-6 预览零写入含 SNAP ⑦）。
**Out of scope:** 修改 `libra status` 自身的长格式；`--long` 与 `-z` 冲突（Libra `commit` 无 `-z`，无需处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `CommitArgs` 只有 `short`/`porcelain` 预览开关，无 `long` | `src/command/commit.rs:244-247`; `src/command/commit.rs:3019` |
| 预览只读分类只含 `--dry-run`/`--porcelain` | `src/cli.rs:2239-2244` |
| `--short` 与 `--porcelain` 均经 `gather_commit_porcelain` 输出 | `src/command/commit.rs:1226-1230` |

**夹具（FX-BASE 之外的差异化条件）:** 工作区含一个已暂存修改与一个未暂存修改，使长/短格式输出可区分。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --long` 的 stdout 与同一状态下 `libra commit --dry-run` 的 stdout 逐字节一致。
- [ ] AC2：`libra commit --short --long` 的 stdout 与 `libra commit --dry-run` 的 stdout 逐字节一致。
- [ ] AC3：`libra commit --long --short` 的 stdout 与 `libra commit --short` 的 stdout 逐字节一致。
- [ ] AC4：`libra commit --porcelain --long` 的 stdout 与 `libra commit --dry-run` 的 stdout 逐字节一致。
- [ ] AC5：`libra commit --long --porcelain` 的 stdout 与 `libra commit --porcelain` 的 stdout 逐字节一致。
- [ ] AC6：无可提交变更时 `libra commit --long` 的退出码与同一状态下 `libra commit --dry-run` 的退出码相同。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_long_preview)'`（V-2；new；覆盖 AC1–AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（`src/cli.rs` 改动只限命令的只读分类，不涉及命令注册或全局参数，未命中 ER-13 `T-1`）
**Dependencies:** `CS-06`, `DEP-CS-05`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/cli.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--long` 选项）、`COMPATIBILITY.md` commit 行（`--long`；`--short` 与 `--porcelain` 输出相同的既有差异）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=commit--long预览; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=CS-06,DEP-CS-05,DEP-CS-07; writeset=序列化于 CS-06; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-10: 修复 `libra commit -v` 模板与剥离 diff 后的多余尾部空行（#760, #761）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 核对并修复 `build_verbose_template` 与 `truncate_at_scissors`：`-v` 与 `--amend -v` 的编辑器模板只附加一次完整 diff（每个文件一段 `diff --git`），剥离 scissors 线及以下内容后持久化消息无多余尾随空行；`--cleanup=verbatim -v` 同样在 scissors 线截断（Git 在 `verbose` 时总按 cut line 截断）。
**Out of scope:** `--cleanup=scissors` 独立模式（由 `CS-14a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `build_verbose_template` 在初始消息后固定追加 `\n` 与 `#\n` | `src/command/commit.rs:2056-2094,2222-2234` |
| `compat_config_defaults_semantics` 的 `commit_status_verbose_defaults` 只测 dry-run 预览 | `tests/compat/config_defaults_semantics_test.rs:36-37` |

**夹具（FX-BASE 之外的差异化条件）:** 两个已暂存文件各有可区分的具体改动；`--amend -v` 场景中 HEAD 已含一个文件改动且新增暂存另一文件；`GIT_EDITOR` 脚本把收到的模板复制到测试可读位置后只改写 scissors 线以上的正文。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit -v` 的编辑器模板含一行 `# ------------------------ >8 ------------------------`。
- [ ] AC2：该模板中自 scissors 线之后第一行 `diff --git` 起至文件末尾的内容与同一时刻 `libra diff --cached` 的 stdout 逐字节一致。
- [ ] AC3：`libra commit --amend -v` 的编辑器模板中自 scissors 线之后第一行 `diff --git` 起至文件末尾的内容与同一时刻 `libra diff --cached HEAD^` 的 stdout 逐字节一致。
- [ ] AC4：`-v` 提交持久化的消息与同一正文的非 `-v` 提交消息逐字节一致。
- [ ] AC5：`libra commit --cleanup=verbatim -v` 持久化的消息逐字节等于编辑器写回文件中 scissors 线以上的内容。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_verbose_template_and_trailing_lines)'`（V-2；new；覆盖 AC1–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_editor_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics -E 'test(/^commit_status_verbose_defaults::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-08`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_editor_test.rs`, `tests/compat/config_defaults_commit_status_verbose.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`-v`/`--amend -v` 模板与截断）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=commit-v模板与尾行; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-08,DEP-CS-07; writeset=序列化于 CS-08; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-11: `commit -F` 非法 UTF-8 与 non-characters 按 Git 映射并告警（#762, #763）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 把 `-F <file>` 改为按字节读取；按 Git `commit.c:1656-1679`（`ensure_utf8`）对消息中每个不构成合法 UTF-8 的字节（含 non-characters U+xxFFFE/U+xxFFFF、U+FDD0..U+FDEF 的编码字节）按 Latin-1 映射为两字节 UTF-8，并向 stderr 输出 Git 同款警告 `Warning: commit message did not conform to UTF-8.`（`commit.c:1681-1684`），不修改 `git-internal` 的 `String` 字段类型。
**Out of scope:** 把 `git-internal::Commit::message` 改为 `Vec<u8>`（永久非目标）；`i18n.commitEncoding` 非 UTF-8 编码支持（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_final_message` 用 `tokio::fs::read_to_string` 读取 `-F`，非法 UTF-8 直接报错 | `src/command/commit.rs:1864-1869` |
| Git `ensure_utf8` 与 non-character 判定 | `commit.c:1630-1679` @ `6de20f6092` |
| Git 把 U+xxFFFE/U+xxFFFF 与 U+FDD0..U+FDEF 均判为非法并逐字节按 Latin-1 映射 | `commit.c:1640-1645,1656-1677` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 三个 `-F` 文件：含单个 `0xE9` 字节；单行 `a<EF BF BE>b<EF B7 90>c` 加末尾 `\n`（U+FFFE 与 U+FDD0 的合法 UTF-8 编码，Git 均判为 non-character，`commit.c:1640-1645`）；对照文件只含合法 UTF-8，为单行 `中文 emoji 😀` 加一个末尾 `\n`（无行首/行尾空白，使默认 cleanup 不改变内容）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：含 `0xE9` 的文件经 `libra commit -F` 提交时 stderr 含 `Warning: commit message did not conform to UTF-8.`。
- [ ] AC2：该提交消息中原 `0xE9` 位置变为 `0xC3 0xA9`，其余字节不变。
- [ ] AC3：含 `EF BF BE` 与 `EF B7 90` 的文件经 `libra commit -F` 提交时 stderr 含同一警告。
- [ ] AC4：该提交的消息 MSG 等于字节串 `61 C3 AF C2 BF C2 BE 62 C3 AF C2 B7 C2 90 63`（Git `ensure_utf8` 把 non-character 的每个字节按 Latin-1 映射，`commit.c:1656-1677`；只处理 U+xxFFFE/U+xxFFFF 而放过 U+FDD0 的实现会保留 `EF B7 90`）。
- [ ] AC5：只含合法 UTF-8 的文件提交时 stderr 不含该警告。
- [ ] AC6：`libra commit --json -F <含 0xE9 的文件>` 的 stdout 可被解析为 JSON。
- [ ] AC7：`libra commit --quiet -F <含 0xE9 的文件>` 的 stderr 仍含该警告。
- [ ] AC8：只含合法 UTF-8 的文件提交后，提交消息 MSG 与文件去掉至多一个末尾 `\n` 后的字节一致。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_file_message_utf8_conformance)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-10`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`-F` 非 UTF-8 映射与警告）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=-F消息UTF-8一致化与告警; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-10,DEP-CS-07; writeset=序列化于 CS-10; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-12: 修复 `--signoff` 去重与 trailer 定位（#765）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git `builtin/commit.c:929`（`append_signoff(..., 0)`，不带 `APPEND_SIGNOFF_DEDUP`）：仅当消息最后一个 trailer 行已是当前提交者的 `Signed-off-by` 时不再追加；否则在 trailer 块后追加且不产生多余空行；含 `---` 行等边界消息与 pinned Git 实测结果逐字节一致。已拆出 `CS-12a`。
**Out of scope:** `-F` 仅含 `Signed-off-by` 的空消息校验（由 `CS-12a` 承接）；通用 `--trailer` 重排序。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `commit.rs` 无条件经 `append_trailers` 追加 `Signed-off-by` | `src/command/commit.rs:1367-1374,1424-1430` |
| Git commit 以 flags=0 调用 `append_signoff` | `builtin/commit.c:929` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 提交者固定为 `C O <co@example.com>`；他人签名行为 `Signed-off-by: X <x@example.com>`；期望字节由执行者用 pinned `6de20f6092` 构建的 git 对同一输入实测得到，作为常量写入用例，并在用例注释记录来源命令。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：消息最后一个 trailer 行已是 `Signed-off-by: C O <co@example.com>` 时，`--signoff` 后消息字节不变。
- [ ] AC2：消息 trailer 块只含 X 的签名行时，`--signoff` 后消息 MSG 与固定期望字节（X 行后紧接 C O 行，无空行）一致。
- [ ] AC3：C O 的签名行存在但其后还有 X 的签名行时，`--signoff` 后消息 MSG 与固定期望字节（末尾再追加一行 C O）一致。
- [ ] AC4：含独立 `---` 行的消息经 `--signoff` 后的消息 MSG 与固定期望字节一致。
- [ ] AC5：无 trailer 的单段消息经 `--signoff` 后的消息 MSG 与固定期望字节（正文、空行、C O 行，无多余尾随空行）一致。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_signoff_dedup_and_placement)'`（V-2；new；覆盖 AC1–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-11`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--signoff` 去重规则）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--signoff去重与trailer定位; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-11,DEP-CS-07; writeset=序列化于 CS-11; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-12a: 修复 `-F` 仅含空白与 `Signed-off-by` 的空消息校验顺序（#766）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-12` 拆出：按 Git `sequencer.c:1257-1262`（`message_is_empty` 在 `verbatim` 且消息非空时判为非空，否则以 `rest_is_empty` 跳过空白与 `Signed-off-by` 行），非 `verbatim` 清理模式下 `-F <file>` 只含空白与 `Signed-off-by:` trailer 且未传 `--allow-empty-message` 时拒绝提交；`--cleanup=verbatim` 下同一内容照常提交。
**Out of scope:** `--signoff` 去重（由 `CS-12` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 空消息检查只在清理后判断 `trim().is_empty()`，未把仅含 trailer 的消息视为空 | `src/command/commit.rs:1976-1982,2672-2677` |

**夹具（FX-BASE 之外的差异化条件）:** 文件内容为 `\n\nSigned-off-by: C O <co@example.com>\n  \n`；对照文件在 trailer 前含一行正文。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：该文件经 `libra commit -F` 且未传 `--allow-empty-message` 时以 `LBR-REPO-003` 失败（既有 `CommitError::EmptyMessage` 映射，`src/command/commit.rs:522-524`）。
- [ ] AC2：同一文件加 `--allow-empty-message` 时生成提交。
- [ ] AC3：对照文件（含正文）未传 `--allow-empty-message` 时生成提交。
- [ ] AC4：`libra commit --cleanup=verbatim -F <同一文件>` 生成的提交消息 MSG 与文件去掉至多一个末尾 `\n` 后的字节一致。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_trailer_only_message_is_empty)'`（V-2；new；覆盖 AC1–AC4）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-12`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（空消息判定）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=仅trailer消息判空; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-12,DEP-CS-07; writeset=序列化于 CS-12; release=independent; split-from=CS-12; exception=N/A`

---

### Task CS-13: 修复 `libra commit -t` 在 `--cleanup=verbatim` 下未修改模板直接提交（#793）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git `sequencer.c:1269-1287`（`template_untouched` 在 `COMMIT_MSG_CLEANUP_NONE` 且消息非空时返回 0）：`--cleanup=verbatim` 下即使编辑器未修改模板也逐字节提交模板内容；其它清理模式下未修改模板仍以「未编辑」中止。
**Out of scope:** `--template :(optional)<path>`（由 `CS-16` 承接）；`--no-edit -t` 的既有行为（本卡不改变，尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 未编辑模板检查对所有清理模式无条件执行 | `src/command/commit.rs:1990-2002` |
| `TemplateUnedited` 映射 `LBR-REPO-003` 与文案 `aborting commit; you did not edit the message` | `src/command/commit.rs:355-356,519-521` |

**夹具（FX-BASE 之外的差异化条件）:** 模板文件含两行正文与一行 `# comment`；`GIT_EDITOR=true`（不修改内容）；对照用默认清理模式。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --cleanup=verbatim --no-status -t <tmpl>`（编辑器未修改）生成提交。
- [ ] AC2：该提交消息的 MSG 与模板文件去掉至多一个末尾 `\n` 后的字节逐字节一致（含 `# comment` 行）。
- [ ] AC3：默认清理模式下 `libra commit --no-status -t <tmpl>`（编辑器未修改）以 `LBR-REPO-003` 失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_verbatim_unedited_template_commits)'`（V-2；new；覆盖 AC1–AC3）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_editor_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-12a`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_editor_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`verbatim` 与未编辑模板）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=verbatim未编辑模板直接提交; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-12a,DEP-CS-07; writeset=序列化于 CS-12a; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-14: 完善 `--cleanup=whitespace` 与 `core.commentChar`（#794）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** `--cleanup=whitespace` / `commit.cleanup=whitespace` 只规整空白、保留全部注释行；`strip` 按 `core.commentChar`（未设置时为 `#`）删除注释行；`default` 在使用编辑器时等同 `strip`，否则等同 `whitespace`。已拆出 `CS-14a`。
**Out of scope:** `--cleanup=scissors` 与 cut-line（由 `CS-14a` 承接）；`core.commentChar=auto`（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `cleanup_commit_message` 硬编码 `#` 判断注释行，未读取 `core.commentChar` | `src/command/commit.rs:2237-2276` |

**夹具（FX-BASE 之外的差异化条件）:** 输入消息含首尾空行、连续空行、`# y` 行与 `; x` 行；分别在未设置与设置 `core.commentChar=;` 下运行，使「仍硬编码 `#`」的错误实现在 AC3/AC4 失败、「总删除 `#`」的错误实现在 AC5 失败；期望字节由执行者用 pinned `6de20f6092` 构建的 git 对同一输入实测得到，作为常量写入用例，并在用例注释记录来源命令。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --cleanup=whitespace -F <msg>` 的提交消息 MSG 与固定期望字节一致（`# y` 与 `; x` 均保留，首尾空行去除、连续空行合并为一行）。
- [ ] AC2：`commit.cleanup=whitespace` 配置下 `libra commit -F <msg>` 的提交消息 MSG 与 AC1 期望字节一致。
- [ ] AC3：`core.commentChar=;` 且 `--cleanup=strip` 时提交消息 MSG 与固定期望字节一致（删除 `; x`、保留 `# y`）。
- [ ] AC4：`core.commentChar=;`、默认清理并经编辑器写回同一内容时提交消息 MSG 与 AC3 期望字节一致。
- [ ] AC5：未设置 `core.commentChar`、默认清理且不经编辑器（`-F`）时提交消息 MSG 与 AC1 期望字节一致。
- [ ] AC6：未设置 `core.commentChar` 且 `--cleanup=strip` 时提交消息 MSG 与固定期望字节一致（删除 `# y`、保留 `; x`）。
- [ ] AC7：`core.commentChar=;`、默认清理且不经编辑器（`-F`）时提交消息 MSG 与 AC1 期望字节一致（`# y` 与 `; x` 均保留）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_cleanup_whitespace_and_comment_char)'`（V-2；new；覆盖 AC1–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_editor_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-13`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_editor_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（清理模式与 `core.commentChar`）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=whitespace清理与commentChar; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-13,DEP-CS-07; writeset=序列化于 CS-13; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-14a: 完善 `--cleanup=scissors` / `commit.cleanup=scissors` 与 cut-line（#982）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-14` 拆出：`scissors` 在打开编辑器时插入以 `core.commentChar` 开头的 cut-line，并截断 cut-line 及其以下内容、保留其上的注释行；不打开编辑器时（`-F`、`--no-edit`）按 Git `get_cleanup_mode` 退化为 `whitespace`，保留 cut-line 及以下内容。`MERGE_MSG` 由本卡自建夹具写入；`SQUASH_MSG` 消费 `DEP-CS-01`（`MP-05`）。
**Out of scope:** 修改 `src/command/merge.rs` 的消息生成（由 `MP-05` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `append_status_section` 不在 `Scissors` 模式插入 cut-line；`truncate_at_scissors` 硬编码 `#` | `src/command/commit.rs:2098-2107,2222-2234` |

**夹具（FX-BASE 之外的差异化条件）:** `core.commentChar=;`；AC4 的 `-F` 消息文件 `<msg>` 依次为 `body`、`; ------------------------ >8 ------------------------`、`tail` 三行（不打开编辑器时 cut-line 及 `tail` 必须保留，按 cut-line 截断的错误实现只得到 `body`）；编辑器在 cut-line 上方追加 `; keep` 行、下方追加 `drop` 行；冲突合并状态下的 `MERGE_MSG` 由测试写入 `Merge branch 'x'`、空行、cut-line 与 `; Conflicts:` 列表；期望字节由执行者用 pinned `6de20f6092` 构建的 git 对同一输入实测得到，作为常量写入用例，并在用例注释记录来源命令。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --cleanup=scissors`（打开编辑器）的模板含一行 `; ------------------------ >8 ------------------------`。
- [ ] AC2：该提交的最终消息 MSG 与固定期望字节一致（cut-line 及其下方 `drop` 行被截断，其上方 `; keep` 行保留）。
- [ ] AC3：`commit.cleanup=scissors` 配置下经编辑器提交的最终消息与 AC2 的消息逐字节一致。
- [ ] AC4：`libra commit -F <msg> --cleanup=scissors`（不打开编辑器）的提交消息保留 cut-line 及其以下内容，MSG 与固定期望字节一致。
- [ ] AC5：冲突合并状态下 `commit.cleanup=scissors` 时 `libra commit --no-edit` 使用自建 `MERGE_MSG` 生成的提交消息 MSG 与固定期望字节一致。
- [ ] AC6：`DEP-CS-01` 提供的 `SQUASH_MSG` 在 `commit.cleanup=scissors` 下经 `libra commit --no-edit` 生成的提交消息 MSG 与固定期望字节一致。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_cleanup_scissors_editor_and_no_edit)'`（V-2；new；覆盖 AC1–AC5）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_cleanup_scissors_squash_msg)'`（V-2；new；覆盖 AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_editor_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-14`, `DEP-CS-01`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_editor_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`scissors` 模式与无编辑器退化）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=scissors清理与cut-line; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CS-14,DEP-CS-01,DEP-CS-07; writeset=序列化于 CS-14; release=independent; split-from=CS-14; exception=N/A`

---

### Task CS-15: 实现 `<pathspec>...` 与 `--only` (`-o`) 隔离索引部分提交（#757）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 移除 `execute_safe` 对位置 `<pathspec>` 与 `--only` 的拒绝，按 Git `builtin/commit.c` 部分提交流程（`prepare_index` 的 `COMMIT_PARTIAL` 分支）以「HEAD 树 + 所选路径的工作区内容」构造临时索引生成提交，同时把所选路径写入主索引、保留未选路径的已暂存内容；hook 或提交失败时主索引与 HEAD 不变；`pathspec` 解析复用 `src/utils/pathspec/mod.rs`。`bisect visualize -p -- <path>` 属 `DEFER-CS-02`。已拆出 `CS-15a`、`CS-15b`、`CS-15c`、`CS-15d`。工作区删除路径由同家族 `CS-15h` 承接。
**Out of scope:** 预览模式、无匹配与合并中拒绝（由 `CS-15d` 承接）；`--include`（由 `CS-15a` 承接）；`--patch`（由 `CS-15b` 承接）；`--pathspec-from-file`（由 `CS-15c` 承接）；`bisect visualize -p`（`DEFER-CS-02`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `execute_safe` 以 `Unsupported` 拒绝 `--only`/`--pathspec-from-file`/位置路径 | `src/command/commit.rs:3008-3016` |
| Git 部分提交：主索引与临时索引都按所选路径更新，提交取自临时索引 | `builtin/commit.c:497-560` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 跟踪文件 `a`（暂存内容 A1、工作区内容 A2）、`b`（已暂存修改 B1，工作区同 B1）、`./-hello 2`（工作区已修改）；`.libra/hooks/pre-commit` 可切换为 `exit 1`；性能夹具在新仓中生成 1,000 与 10,000 个等大跟踪文件，每次只修改并提交其中 1 个路径。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit -m x -- a` 后 HEAD 树中 `a` 的 blob 内容为 A2。
- [ ] AC2：同一提交的 HEAD 树中 `b` 的 blob 与原 HEAD 中 `b` 相同。
- [ ] AC3：提交后主索引中 `a` 的条目 OID 等于新提交中 `a` 的 OID。
- [ ] AC4：AC1 前后 SNAP-Δ（只允许 ① 当前分支 ref、② 其 reflog、③ 中 `a` 条目的 OID（变为新提交中 `a` 的 OID）与 ⑤ 新增的 A2 blob、新提交及其树对象变化；③ 其余条目——含 `b`（B1）与 `./-hello 2`——的全部字段与标志及扩展段、④ 工作区全量不变）。
- [ ] AC5：`libra commit -o -m x -- a` 在同一夹具副本上生成的提交树 OID 与 AC1 的提交树 OID 相等。
- [ ] AC6：`libra commit -m x -- "./-hello 2"` 生成的提交相对原 HEAD 只改变该路径。
- [ ] AC7：`pre-commit` 以非零退出时 `libra commit -m x -- a` 以 `LBR-REPO-003` 失败（V-6 基线差异：SNAP-Δ 只允许 ⑤ 对象集合新增所选路径工作区内容的 blob）。
- [ ] AC8：性能用例在 1,000 与 10,000 条索引条目下各预热 3 次、计时 10 次，10,000 条的中位耗时不超过 1,000 条中位耗时的 20 倍。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_partial_pathspec_only)'`（V-2；new；覆盖 AC1–AC7）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_partial_pathspec_scaling)'`（V-2；new；覆盖 AC8；记录机器、构建配置与每次耗时）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-03` 的 `T-2` 覆盖）
**Dependencies:** `CS-14a`, `DEP-CS-07`, `DEP-CS-08`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/utils/pathspec/mod.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-03` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（位置 pathspec 与 `--only` 部分提交语义）、`COMPATIBILITY.md` commit 部分提交行（`#757` 部分闭环）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Performance budget:** 部分提交为 `O(index entries)`，不扫描提交历史；AC8 以双规模增长比断言。
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-03 (1/4)`
**C/D coverage from:** `REL-CS-03`
**Granularity:** `type=implementation; axis=<pathspec>与--only部分提交; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CS-14a,DEP-CS-07,DEP-CS-08; writeset=序列化于 CS-14a; release=family child of REL-CS-03 (1/4); split-from=N/A; exception=N/A`

---

### Task CS-15h: 部分提交处理工作区中已删除的跟踪文件（#757）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-15` 拆出：按 Git `builtin/commit.c:535,547`（主索引与临时索引均调用 `add_remove_files`），所选路径在工作区已被删除时，部分提交把该路径从提交树与主索引中移除，未选路径的暂存内容保留。
**Out of scope:** 部分提交的内容修改语义（由 `CS-15` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 部分提交同时处理新增与删除 | `builtin/commit.c:535,547` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 跟踪文件 `c` 在工作区被删除（未暂存删除）；`b` 有已暂存修改 B1（工作区同 B1）；另有与 HEAD 相同的跟踪文件 `d`。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit -m d -- c` 生成的提交树经 `libra ls-tree -r` 列出的条目集合（mode、OID、路径）等于原 HEAD 树的条目集合去掉 `c`（`b` 为原 HEAD 内容而非已暂存的 B1，`d` 不变）。
- [ ] AC2：提交后主索引不含 `c` 的条目。
- [ ] AC3：AC1 前后 SNAP-Δ（只允许 ① 当前分支 ref、② 其 reflog、③ 删除 `c` 条目与 ⑤ 新提交及其树对象变化；③ 其余条目——含 `b`（B1）——的全部字段与标志及扩展段、④ 工作区全量不变）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_partial_pathspec_deleted_file)'`（V-2；new；覆盖 AC1–AC3）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-03` 的 `T-2` 覆盖）
**Dependencies:** `CS-15`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-03`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-03` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（部分提交包含删除）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-03 (2/4)`
**C/D coverage from:** `REL-CS-03`
**Granularity:** `type=implementation; axis=部分提交删除路径; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-15,DEP-CS-07; writeset=序列化于 CS-15; release=family child of REL-CS-03 (2/4); split-from=CS-15; exception=N/A`

---

### Task CS-15d: 补齐 `<pathspec>`/`--only` 预览模式、无匹配与合并中拒绝（#757）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-15` 拆出：`--dry-run`、`--porcelain`、`--short` 搭配 `<pathspec>`/`--only` 只预览、不改主索引；路径不匹配任何已跟踪文件时按 Git `report_path_error` 以 pathspec 错误失败；合并冲突提交过程中拒绝部分提交（Git `cannot do a partial commit during a merge.`）。
**Out of scope:** 隔离索引提交主逻辑（由 `CS-15` 承接）；cherry-pick/rebase 进行中的部分提交拒绝（尚未排期：Libra `commit` 当前不在这两种状态下续提部分提交）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 部分提交入口当前统一以 `Unsupported` 拒绝，无预览、无匹配与合并中分支 | `src/command/commit.rs:3008-3016` |
| Git `list_paths` 无匹配退出 1；合并中拒绝部分提交 | `builtin/commit.c:519-529` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 跟踪文件 `a`（暂存内容 A1、工作区内容 A2）与 `b`（已暂存修改 B1）；未跟踪文件 `u`；另建一个停在冲突合并状态（存在 `MERGE_HEAD`）的仓库。AC2、AC3 的期望 stdout 由执行者在同一夹具上用 pinned `6de20f6092` 构建的 git 实测（`git commit --porcelain -- a`、`git commit --short -- a`、`git commit --only --porcelain -- a`）并写入用例常量，在用例注释记录来源命令；Git 以「HEAD + 所选路径」的临时索引收集完整状态，未选路径与未跟踪文件仍出现在输出中（`builtin/commit.c:497-586`）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --dry-run -- a` 输出的「Changes to be committed」段只列出 `a`。
- [ ] AC2：`libra commit --porcelain -- a` 的 stdout 与 pinned Git 实测常量逐字节一致。
- [ ] AC3：`libra commit --short -- a` 的 stdout 与 pinned Git 实测常量逐字节一致。
- [ ] AC4：`libra commit -m x -- nope`（不存在）以 `LBR-CLI-003` 失败。
- [ ] AC5：`libra commit -m x -- u`（未跟踪）以 `LBR-CLI-003` 失败。
- [ ] AC6：冲突合并状态下 `libra commit -m x -- a` 以 stderr 含 Git 同款 `cannot do a partial commit during a merge.` 的错误失败。
- [ ] AC7：显式 `--only` 的 `libra commit --only --porcelain -- a` 的 stdout 与 pinned Git 对 `git commit --only --porcelain -- a` 的实测常量逐字节一致。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_partial_preview_and_guards)'`（V-2；new；覆盖 AC1–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-03` 的 `T-2` 覆盖）
**Dependencies:** `CS-15h`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-03`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-03` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（部分提交预览、无匹配与合并中拒绝）、`COMPATIBILITY.md` commit 部分提交行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-03 (3/4)`
**C/D coverage from:** `REL-CS-03`
**Granularity:** `type=implementation; axis=部分提交预览与守卫; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-15h,DEP-CS-07; writeset=序列化于 CS-15h; release=family child of REL-CS-03 (3/4); split-from=CS-15; exception=N/A`

---

### Task CS-15f: 实现 `--only` 无路径时的 Git 拒绝与例外（#757, #786）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-15d` 拆出：按 Git `builtin/commit.c:388-390`，无路径的 `--only` 在未带 `--allow-empty`、且不满足「`--amend` 且未带普通 `--fixup=<c>`」时报 `No paths with --include/--only does not make sense.`；`--only --allow-empty` 生成提交树等于 HEAD 树的空提交；`--amend --only` 只改写消息（`--fixup=amend:`/`reword:` 由 `CS-07` 设置 `allow_empty`，不在本卡）。
**Out of scope:** `--include` 无路径（由 `CS-15a` 承接）；`--pathspec-from-file` 空列表（由 `CS-15e` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 部分提交入口当前统一以 `Unsupported` 拒绝 | `src/command/commit.rs:3008-3016` |
| Git 拒绝条件 `!pathspec.nr && (also || (only && !allow_empty && (!amend || (fixup_message && strcmp(fixup_prefix, "amend")))))` | `builtin/commit.c:388-390,1395-1410` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 索引含多个不同状态的条目：已暂存修改 `b`、已暂存新增 `n`、与 HEAD 相同的 `c`；工作区另有未暂存修改 `w`；HEAD 之前另有一个提交 `p` 供普通 `--fixup=<p>` 使用。被 amend 提交的消息为 `orig message`（与 `z` 不同）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --only -m x`（无路径）以 stderr 含 `No paths with --include/--only does not make sense.` 的错误失败。
- [ ] AC2：`libra commit --amend --only --fixup=<p>`（无路径）以 stderr 含同一文案的错误失败。
- [ ] AC3：`libra commit --only --allow-empty -m y`（无路径）生成的提交树等于原 HEAD 树。
- [ ] AC4：AC3 前后 SNAP-Δ（只允许 ① 当前分支 ref、② 其 reflog 与 ⑤ 新提交对象变化；③ 索引与 ④ 工作区全量不变）。
- [ ] AC5：`libra commit --amend --only -m z`（无路径）生成的新提交树等于被 amend 提交的树。
- [ ] AC6：AC5 前后 SNAP-Δ（只允许 ① 当前分支 ref、② 其 reflog 与 ⑤ 新提交对象变化；③ 索引与 ④ 工作区全量不变）。
- [ ] AC7：AC5 生成的新提交的提交消息 MSG 为 `z`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_only_without_paths_rules)'`（V-2；new；覆盖 AC1–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-03` 的 `T-2` 覆盖）
**Dependencies:** `CS-15d`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-03`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-03` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--only` 无路径规则与例外）、`COMPATIBILITY.md` commit 部分提交行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-03 (4/4)`
**C/D coverage from:** `REL-CS-03`
**Granularity:** `type=implementation; axis=--only无路径规则; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-15d,DEP-CS-07; writeset=序列化于 CS-15d; release=family child of REL-CS-03 (4/4); split-from=CS-15d; exception=N/A`

---

### Task REL-CS-03: `REL-CS-03` `<pathspec>`/`--only` 部分提交与预览守卫家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-15`（1/4）、`CS-15h`（2/4）、`CS-15d`（3/4）、`CS-15f`（4/4） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-15`、`CS-15h`、`CS-15d`、`CS-15f` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `CS-15` 启用部分提交后必须与 `CS-15d` 的预览与守卫同版发布 | `src/command/commit.rs:3008-3016`; `plan-template.md` G-08 |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-15`、`CS-15h`、`CS-15d`、`CS-15f` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `command::commit_test::commit_partial_pathspec_only` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit_test::commit_partial_pathspec_scaling` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::commit_test::commit_partial_pathspec_deleted_file` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：聚合守卫 `command::commit_test::commit_partial_preview_and_guards` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC5：聚合守卫 `command::commit_test::commit_only_without_paths_rules` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC6：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-03 begin -->` 与 `<!-- release-note-draft:REL-CS-03 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 4 行，顺序同家族登记：`CS-15`、`CS-15h`、`CS-15d`、`CS-15f`）。
- [ ] AC7：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_partial_pathspec_only)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_partial_pathspec_scaling)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_partial_pathspec_deleted_file)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_partial_preview_and_guards)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC4）
- [ ] V5 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_only_without_paths_rules)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC5）
- [ ] V6 手工证据（覆盖 AC6）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-03 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-03 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-15 CS-15h CS-15d CS-15f; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V7 手工证据（覆盖 AC7）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-15` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-15h` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-15d` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-15f` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-15`, `CS-15h`, `CS-15d`, `CS-15f`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-15`、`CS-15h`、`CS-15d`、`CS-15f` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-03`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-03家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/12; VER=7/12; landing=0; prod-files=0; scope=S; deps=CS-15,CS-15h,CS-15d,CS-15f; writeset=序列化于 CS-15f; release=family release point of REL-CS-03; split-from=N/A; exception=N/A`

---

### Task CS-15a: 实现 `libra commit -i/--include <pathspec>` 部分提交（#786, #790 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-15` 拆出：`-i/--include` 按 Git `prepare_index`（`also && pathspec.nr` 分支）先把所选已跟踪路径的工作区内容加入主索引，再提交整个主索引；未给路径时按 Git 报 `No paths with --include/--only does not make sense.`；与 `-o`/`-a` 互斥。
**Out of scope:** `--patch`（由 `CS-15b` 承接）；`--pathspec-from-file`（由 `REL-CS-04` 承接）；`--interactive` 与 `-i` 的互斥（由 `CS-09` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `CommitArgs` 无 `-i/--include` | `src/command/commit.rs:254-267` |
| Git `also` 分支与选项互斥 | `builtin/commit.c:388-390,455-470,1374-1377` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** `a` 工作区修改未暂存、`b` 已暂存修改、另有与 HEAD 相同的跟踪文件 `c`；`.libra/hooks/pre-commit` 可切换为 `exit 1`。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit -i -m x -- a` 后 HEAD 树中 `a` 的 blob 等于其工作区内容。
- [ ] AC2：同一提交的 HEAD 树中 `b` 的 blob 等于其已暂存内容。
- [ ] AC3：AC1 前后 SNAP-Δ（只允许 ① 当前分支 ref、② 其 reflog、③ 中 `a` 条目的 OID（变为新提交中 `a` 的 OID）与 ⑤ 新增的 `a` 工作区内容 blob、新提交及其树对象变化；③ 其余条目——含 `b` 与 `c`——的全部字段与标志及扩展段、④ 工作区全量不变）。
- [ ] AC4：`libra commit -i -m x`（无路径）以 stderr 含 `No paths with --include/--only does not make sense.` 的错误失败。
- [ ] AC5：`libra commit -i -o -m x -- a` 以 stderr 含 `options '-i/--include' and '-o/--only' cannot be used together` 的错误失败。
- [ ] AC6：`libra commit -i -a -m x -- a` 以 stderr 含 `options '-i/--include' and '-a/--all' cannot be used together` 的错误失败。
- [ ] AC7：`pre-commit` 以非零退出时 `libra commit -i -m x -- a` 以 `LBR-REPO-003` 失败（V-6 基线差异：SNAP-Δ 只允许 ⑤ 对象集合新增 `a` 工作区内容的 blob）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_include_pathspec)'`（V-2；new；覆盖 AC1–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `REL-CS-03`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`-i/--include`）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=-i/--include部分提交; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=REL-CS-03,DEP-CS-07; writeset=序列化于 REL-CS-03; release=independent; split-from=CS-15; exception=N/A`

---

### Task CS-15b: 实现 `libra commit -p/--patch` 交互式 hunk 提交（#786, #790 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-15` 拆出：从 `src/cli.rs` `DECLINED_INTERACTIVE_FLAGS` 的 commit 条目移除 `-p`/`--patch`（`--interactive` 保留到 `CS-09`），在 `CommitArgs` 新增 `-p/--patch`；按 Git `prepare_index` 的 `interactive` 分支，复用 `src/command/add.rs` 的 hunk 选择引擎在主索引副本上暂存所选 hunk，成功后该副本成为主索引并提交（已暂存内容一并提交），失败时主索引不变。
**Out of scope:** `--interactive` 菜单（由 `REL-CS-02` 承接）；`--unified`/`--inter-hunk-context`（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DECLINED_INTERACTIVE_FLAGS` 的 commit 条目拒绝 `-p`/`--patch`/`--interactive` | `src/cli.rs:3005,3017-3027` |
| `add.rs` 已有 `run_add_patch` | `src/command/add.rs:1766-1767` |
| Git `interactive` 分支在锁定的主索引副本上运行 `interactive_add`；`-i`/`-o`/`-a`/`-p` 互斥 | `builtin/commit.c:395-430,1374-1377` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** `a` 有两个相距足够远的未暂存 hunk，`b` 有已暂存修改，另有与 HEAD 相同的跟踪文件 `c`；以非 TTY 管道向 hunk 选择输入 `y\nn\n`；`.libra/hooks/pre-commit` 可切换为 `exit 1`。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：管道输入 `y\nn\n` 时，HEAD 树中 `a` 的 blob 只含第一个 hunk 的改动。
- [ ] AC2：同一提交的 HEAD 树中 `b` 的 blob 等于其已暂存内容。
- [ ] AC3：AC1 前后 SNAP-Δ（只允许 ① 当前分支 ref、② 其 reflog、③ 中 `a` 条目的 OID（变为新提交中 `a` 的 OID，即只含第一个 hunk 的内容）与 ⑤ 新增的该 blob、新提交及其树对象变化；③ 其余条目——含 `b` 与 `c`——的全部字段与标志及扩展段、④ 工作区全量不变（`a` 的工作区仍含两个 hunk））。
- [ ] AC4：管道输入 `n\nn\n` 且无其它已暂存内容时以 `LBR-REPO-003` 失败。
- [ ] AC5：`pre-commit` 以非零退出时 `libra commit -p -m x` 以 `LBR-REPO-003` 失败（V-6 基线差异：SNAP-Δ 只允许 ⑤ 对象集合新增所选 hunk 结果的 blob）。
- [ ] AC6：`libra commit -p -a -m x` 以 `LBR-CLI-002` 失败（Git `die_for_incompatible_opt4`）。
- [ ] AC7：`libra commit -p -i -m x -- a` 以 `LBR-CLI-002` 失败（Git `die_for_incompatible_opt4`）。
- [ ] AC8：`libra commit -p -o -m x -- a` 以 `LBR-CLI-002` 失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_patch_hunk_selection)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test compat_declined_interactive_flags`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::add_patch_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `T-1`：修改 `src/cli.rs` 全局交互标志拦截表
**Dependencies:** `CS-15a`, `DEP-CS-03`, `DEP-CS-04`, `DEP-CS-05`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `src/command/commit.rs`, `src/command/add.rs`, `tests/command/commit_test.rs`, `tests/compat/compat_declined_interactive_flags.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`-p/--patch`）、`docs/development/commands/_compatibility.md`（D15 移除 `commit -p/--patch`）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=-p/--patch交互hunk提交; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=CS-15a,DEP-CS-03,DEP-CS-04,DEP-CS-05,DEP-CS-07; writeset=序列化于 CS-15a; release=independent; split-from=CS-15; exception=N/A`

---

### Task CS-15c: 实现 `--pathspec-from-file` 与 `--pathspec-file-nul` 列表解析（#786）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-15` 拆出：在 `src/utils/pathspec/mod.rs` 实现与 Git `parse_pathspec_file` 等价的列表解析函数（输入字节流与 `nul` 标志，输出路径列表或错误）：非 NUL 模式按行分隔，接受 CRLF 与无尾随分隔符，对以 `"` 开头的行做 C 风格反引号解析且坏引号报 `line is badly quoted`；NUL 模式按 `\0` 分隔且不做反引号解析。Libra 的 pathspec 模型以 UTF-8 字符串表示路径（`src/utils/pathspec/mod.rs:65`），本卡的等价性限于 UTF-8 路径：含非法 UTF-8 字节的列表条目（任一模式）不在本计划范围、行为不作约束，由 `DEFER-CS-08` 承接（Git 以字节处理，该差异由接入 CLI 的 `CS-15g` 同卡登记 `COMPATIBILITY.md`）。CLI 接入与模式分发由 `CS-15g` 承接。
**Out of scope:** CLI 接入、stdin 读取与模式分发（由 `CS-15g` 承接）；互斥诊断（由 `CS-15e`、`CS-09` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `src/utils/pathspec/mod.rs` 无 pathspec 文件解析 | `src/utils/pathspec/mod.rs` |
| Git `parse_pathspec_file` 与坏引号报错 `line is badly quoted` | `pathspec.c:687-710` @ `6de20f6092`; `t/t7526-commit-pathspec-file.sh:52-123` |

**夹具（FX-BASE 之外的差异化条件）:** 以字节串直接调用解析函数；期望值取自 t7526 对应用例。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：LF 输入 `fileA.t\nfileB.t\n` 解析为 `["fileA.t", "fileB.t"]`。
- [ ] AC2：CRLF 输入 `fileA.t\r\nfileB.t\r\n` 解析为 `["fileA.t", "fileB.t"]`。
- [ ] AC3：无尾随分隔符的输入 `fileA.t\nfileB.t` 解析为 `["fileA.t", "fileB.t"]`。
- [ ] AC4：NUL 模式输入 `file a.t\0file\nb.t\0` 解析为 `["file a.t", "file\nb.t"]`。
- [ ] AC5：非 NUL 模式的行 `"\303\251.txt"`（两个连续八进制转义组成一个 UTF-8 字符）解析为 `é.txt`（字节 `C3 A9 2E 74 78 74`；Git `unquote_c_style` 按字节解码，`quote.c:386-425`）。
- [ ] AC6：NUL 模式下同一带引号内容按字面返回 `"\303\251.txt"`（不做反引号解析）。
- [ ] AC7：非 NUL 模式的坏引号行 `"abc` 返回包含 `line is badly quoted` 的错误。
- [ ] AC8：非 NUL 模式的行 `"a\\b\"c\nd"`（含 `\\`、`\"`、`\n` 三种转义）解析为字节串 `a` `\` `b` `"` `c` LF `d`（Git `unquote_c_style`，`quote.c:386-415`）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：`Docs and compatibility impact` 为 `N/A`，理由见该字段。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=utils::pathspec::tests::parse_pathspec_file_forms)'`（V-2；new，`src/utils/pathspec/mod.rs` 的 `#[cfg(test)] mod tests`；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^utils::pathspec::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-04` 的 `T-2` 覆盖）
**Dependencies:** `CS-15b`, `DEP-CS-07`, `DEP-CS-08`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/pathspec/mod.rs`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-04` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `N/A`：本卡只新增尚未接入 CLI 的解析函数，不改变任何用户可见行为；`--pathspec-from-file` 的命令文档、`COMPATIBILITY.md` 与网站由接入它的 `CS-15g` 同卡交付（不执行 W）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-04 (1/3)`
**C/D coverage from:** `REL-CS-04`
**Granularity:** `type=implementation; axis=--pathspec-from-file列表解析; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-15b,DEP-CS-07,DEP-CS-08; writeset=序列化于 CS-15b; release=family child of REL-CS-04 (1/3); split-from=CS-15; exception=N/A`

---

### Task CS-15g: `commit --pathspec-from-file` 接入、stdin 读取与 `--include`/`--only`/默认模式分发（#786）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-15c` 拆出：在 `commit` 接入 `--pathspec-from-file=<file>`（`-` 读 stdin）与 `--pathspec-file-nul`，调用 `CS-15c` 的解析函数；解析错误以解析函数的诊断（如 `line is badly quoted`）失败，由 `CS-15e` 验收；所得路径与位置 pathspec 走相同分发——默认与 `--only` 为部分提交（`CS-15`），`--include` 为「先加入所列路径再提交整个索引」（`CS-15a`）；空列表且未带 `-i`/`-o` 时 `pathspec.nr == 0`，按 Git 作普通提交。
**Out of scope:** 列表解析（由 `CS-15c` 承接）；互斥诊断与空列表配 `-i`/`-o`（由 `CS-15e` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 解析后统一进入 `prepare_index` 的分发 | `builtin/commit.c:371-390,455-560` @ `6de20f6092` |
| Git 以 stdin 与 NUL 分隔列表提交两个路径 | `t/t7526-commit-pathspec-file.sh:52-61` @ `6de20f6092` |
| Git 以带引号的列表行 `"file\101.t"` 成功提交 `fileA.t` | `t/t7526-commit-pathspec-file.sh:100-112` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 按 t7526 setup 构造：`fileA.t`、`fileB.t`、`fileC.t`、`fileD.t` 各有可辨别的工作区修改，`fileE.t` 有已暂存修改；`list` 含两行：带引号的 `"file\101.t"`（C 风格解码后为 `fileA.t`，对应 t7526:100-112）与未加引号的 `fileB.t`，因而 AC1–AC3、AC6–AC8 经 CLI 走非 NUL 引号解码路径（未使用解码结果的实现会得到不存在的路径而失败）；`empty_list` 为空文件；期望树以不经被测代码路径的命令在另一独立副本上求得：E 为执行 `libra restore --staged fileE.t`、`libra add fileA.t fileB.t` 后 `libra write-tree` 的输出（原 HEAD 树仅 `fileA.t`、`fileB.t` 取工作区内容）；E′ 为不撤销 `fileE.t` 暂存、执行 `libra add fileA.t fileB.t` 后 `libra write-tree` 的输出；I 为被测副本在运行前 `libra write-tree` 的输出（当前索引树）。每条比较在同一夹具的独立副本上进行。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --pathspec-from-file=list -m x` 生成的提交树等于 E。
- [ ] AC2：`libra commit --only --pathspec-from-file=list -m x` 生成的提交树等于 E。
- [ ] AC3：`libra commit --include --pathspec-from-file=list -m x` 生成的提交树等于 E′。
- [ ] AC4：`libra commit --pathspec-from-file=empty_list -m x` 生成的提交树等于 I。
- [ ] AC5：`libra commit --pathspec-from-file=- --pathspec-file-nul -m x`（stdin 为字节 `fileA.t\0fileB.t\0`，对应 t7526:52-61）生成的提交树等于 E（同一命令同时覆盖 stdin 读取与 NUL 分隔的 CLI 接入；未把 NUL 标志传给解析函数的实现会得到含 NUL 的单个路径而失败）。
- [ ] AC6：默认模式（AC1）提交后的主索引（SNAP ③）等于独立副本上对原索引执行 `libra add fileA.t fileB.t` 后的索引（所选路径更新为工作区内容，`fileE.t` 的暂存内容保留）。
- [ ] AC7：`--only` 模式（AC2）提交后的主索引等于同一期望索引。
- [ ] AC8：`--include` 模式（AC3）提交后的主索引等于同一期望索引。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_pathspec_from_file_mode_dispatch)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-04` 的 `T-2` 覆盖）
**Dependencies:** `CS-15c`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-04`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-04` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（文件列表与 `--include`/`--only` 的组合）、`COMPATIBILITY.md` commit 行（含非 UTF-8 路径不支持的差异，`DEFER-CS-08`）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-04 (2/3)`
**C/D coverage from:** `REL-CS-04`
**Granularity:** `type=implementation; axis=文件列表路径模式分发; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-15c,DEP-CS-07; writeset=序列化于 CS-15c; release=family child of REL-CS-04 (2/3); split-from=CS-15c; exception=N/A`

---

### Task CS-15e: 补齐 `#786` 部分提交与 `--pathspec-from-file` 互斥精确诊断（#786）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-15c` 拆出：按 Git `builtin/commit.c:371-390` 与 t7526「error conditions」（`:137-161`）精确文案实现 `--pathspec-from-file` 与 `-a`、位置 pathspec 的互斥、`--pathspec-file-nul` 依赖，以及空列表配 `--include`/`--only` 的拒绝。`--only` 无路径的一般规则由 `CS-15f` 承接；与已发布的 `--patch`（`CS-15b`）的互斥由本卡交付（Git `builtin/commit.c:371-373`），`--interactive` 的同一互斥由解禁它的 `CS-09` 同卡交付。另交付文件列表的引号解析错误在命令层的诊断（NUL 模式不解引号、坏引号行）。
**Out of scope:** `--pathspec-from-file` 解析与分发（由 `CS-15c`、`CS-15g` 承接）；`--only` 无路径的一般规则（由 `CS-15f` 承接）；与 `--interactive` 的互斥（由 `CS-09` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 部分提交选项当前统一以 `Unsupported` 拒绝，无 Git 精确诊断 | `src/command/commit.rs:3008-3016` |
| Git 诊断文案与 `--only` 例外条件 | `builtin/commit.c:371-390` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 按 t7526 setup 构造：跟踪文件 `fileA.t`..`fileD.t` 均有工作区修改，`fileE.t` 有已暂存修改；`list` 含 `fileA.t` 一行；`empty_list` 为空文件。`q` 只含带引号行 `"file\101.t"`；`bad` 只含坏引号行 `"abc`。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`--pathspec-from-file=list --all` 以 stderr 含 `options '--pathspec-from-file' and '-a' cannot be used together` 的错误失败。
- [ ] AC2：`--pathspec-from-file=list -m x -- fileA.t` 以 stderr 含 `'--pathspec-from-file' and pathspec arguments cannot be used together` 的错误失败。
- [ ] AC3：只传 `--pathspec-file-nul` 时以 stderr 含 `the option '--pathspec-file-nul' requires '--pathspec-from-file'` 的错误失败。
- [ ] AC4：`--pathspec-from-file=empty_list --include` 以 stderr 含 `No paths with --include/--only does not make sense.` 的错误失败。
- [ ] AC5：`--pathspec-from-file=empty_list --only` 以 stderr 含 `No paths with --include/--only does not make sense.` 的错误失败。
- [ ] AC6：`--pathspec-from-file=list --patch -m x` 以 stderr 含 `options '--pathspec-from-file' and '--interactive/--patch' cannot be used together` 的错误失败。
- [ ] AC7：NUL 模式下 `libra commit --pathspec-from-file=q --pathspec-file-nul -m x` 以 `LBR-CLI-003` 失败（t7526「quotes not compatible with --pathspec-file-nul」）。
- [ ] AC8：`libra commit --pathspec-from-file=bad -m x` 以 stderr 含 `line is badly quoted` 的错误失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_pathspec_from_file_validation)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-04` 的 `T-2` 覆盖）
**Dependencies:** `CS-15g`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-04`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-04` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（互斥诊断与 `--only` 例外）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-04 (3/3)`
**C/D coverage from:** `REL-CS-04`
**Granularity:** `type=implementation; axis=#786部分提交互斥诊断; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-15g,DEP-CS-07; writeset=序列化于 CS-15g; release=family child of REL-CS-04 (3/3); split-from=CS-15c; exception=N/A`

---

### Task REL-CS-04: `REL-CS-04` `--pathspec-from-file` 与 `#786` 互斥守卫家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-15c`（1/3）、`CS-15g`（2/3）、`CS-15e`（3/3） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-15c`、`CS-15g`、`CS-15e` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `CS-15g` 接入 `--pathspec-from-file` 后必须与 `CS-15e` 的互斥守卫同版发布 | `src/command/commit.rs:260-264,3008-3016`; `plan-template.md` G-08 |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-15c`、`CS-15g`、`CS-15e` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `utils::pathspec::tests::parse_pathspec_file_forms` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit_test::commit_pathspec_from_file_mode_dispatch` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::commit_test::commit_pathspec_from_file_validation` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-04 begin -->` 与 `<!-- release-note-draft:REL-CS-04 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 3 行，顺序同家族登记：`CS-15c`、`CS-15g`、`CS-15e`）。
- [ ] AC5：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --lib -E 'test(=utils::pathspec::tests::parse_pathspec_file_forms)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_pathspec_from_file_mode_dispatch)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_pathspec_from_file_validation)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 手工证据（覆盖 AC4）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-04 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-04 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-15c CS-15g CS-15e; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V5 手工证据（覆盖 AC5）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-15c` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-15g` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-15e` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-15c`, `CS-15g`, `CS-15e`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-15c`、`CS-15g`、`CS-15e` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-04`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-04家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/12; VER=5/12; landing=0; prod-files=0; scope=S; deps=CS-15c,CS-15g,CS-15e; writeset=序列化于 CS-15e; release=family release point of REL-CS-04; split-from=N/A; exception=N/A`

---

### Task CS-09: 解禁 `libra commit --interactive` CLI 参数与守卫校验（#773, #786）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 从 `src/cli.rs` `DECLINED_INTERACTIVE_FLAGS` 的 commit 条目移除 `--interactive`（同步 `tests/compat/compat_declined_interactive_flags.rs` 与 `_compatibility.md` D15），在 `CommitArgs` 新增 `--interactive`（无短参数，`-i` 仍为 `--include`），补齐 `--pathspec-from-file` 与 `--interactive` 的 Git 精确互斥诊断（与 `--patch` 的互斥已由 `CS-15e` 交付），以及 `--json` 下拒绝交互模式。与 `-a`/`-i`/`-o` 的互斥由同家族 `CS-09b` 承接。已拆出 `CS-09b`、`CS-09a`。
**Out of scope:** 交互式菜单与管道输入实现（由 `CS-09a` 承接）；`--include`/`--patch`（由 `CS-15a`/`CS-15b` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `DECLINED_INTERACTIVE_FLAGS` 拦截 `commit --interactive`；`CommitArgs` 无 `interactive` | `src/cli.rs:3005,3017-3027`; `src/command/commit.rs:92-279` |
| Git `--pathspec-from-file` 与 `--interactive/--patch` 互斥文案 | `builtin/commit.c:372-373` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** t7526 夹具中的 `list` 文件；`--json` 走机器输出模式。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --interactive -m x` 的输出不含 `interactive commit is not supported`。
- [ ] AC2：`CommitArgs::try_parse_from(["commit", "--interactive"])` 得到 `interactive == true`。
- [ ] AC3：`libra commit -i -m x -- a` 的提交树中 `a` 为其工作区内容（`-i` 仍为 `--include`）。
- [ ] AC4：`--pathspec-from-file=list --interactive` 以 stderr 含 `options '--pathspec-from-file' and '--interactive/--patch' cannot be used together` 的错误失败。
- [ ] AC5：`libra commit --json --interactive -m x` 以 `LBR-CLI-002` 失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_interactive_cli_guards)'`（V-2；new；覆盖 AC1–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test compat_declined_interactive_flags`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；`T-1` 命中的 `src/cli.rs` 改动由 `REL-CS-02` 的 `T-2` 全量覆盖）
**Dependencies:** `REL-CS-04`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `src/command/commit.rs`, `tests/command/commit_test.rs`, `tests/compat/compat_declined_interactive_flags.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `docs/development/commands/_compatibility.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-02` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--interactive` 与互斥规则）、`docs/development/commands/_compatibility.md`（D15 移除 `commit --interactive`）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-02 (1/4)`
**C/D coverage from:** `REL-CS-02`
**Granularity:** `type=implementation; axis=解禁commit--interactive与守卫; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=REL-CS-04,DEP-CS-07; writeset=序列化于 REL-CS-04; release=family child of REL-CS-02 (1/4); split-from=N/A; exception=N/A`

---

### Task CS-09b: `commit --interactive` 与 `-a`/`--include`/`--only` 的互斥诊断（#786）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-09` 拆出：按 Git `builtin/commit.c:1374-1377`（`die_for_incompatible_opt4` 统一检查 `-i`、`-o`、`-a`、`--interactive/-p`），`--interactive` 与 `-a`、`-i/--include`、`-o/--only` 任一同时出现时报错，不进入菜单、不改动索引。
**Out of scope:** `--interactive` 的接入与 pathspec 互斥（由 `CS-09` 承接）；菜单行为（由 `CS-09a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 四选一互斥 | `builtin/commit.c:1374-1377` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 跟踪文件 `file` 有未暂存修改；无其它已暂存内容。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --interactive -a -m x` 以 `LBR-CLI-002` 失败。
- [ ] AC2：`libra commit --interactive --include -m x -- file` 以 `LBR-CLI-002` 失败。
- [ ] AC3：`libra commit --interactive --only -m x -- file` 以 `LBR-CLI-002` 失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_interactive_mode_exclusivity)'`（V-2；new；覆盖 AC1–AC3）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-02` 的 `T-2` 覆盖）
**Dependencies:** `CS-09`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-02`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-02` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--interactive` 与 `-a`/`-i`/`-o` 互斥）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-02 (2/4)`
**C/D coverage from:** `REL-CS-02`
**Granularity:** `type=implementation; axis=interactive与-a/-i/-o互斥; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-09,DEP-CS-07; writeset=序列化于 CS-09; release=family child of REL-CS-02 (2/4); split-from=CS-09; exception=N/A`

---

### Task CS-09a: 实现 `libra commit --interactive` 交互式菜单暂存与管道提交（#773）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-09` 拆出：在 `src/command/add.rs` 实现 Git `add --interactive` 风格菜单（`status`、`update`、`revert`、`add untracked`、`patch`、`diff`、`quit`），支持非 TTY 管道输入；`commit --interactive [<paths>]` 按 Git `prepare_index` 的 `interactive` 分支在主索引副本上运行菜单，成功后提交，有暂存内容时按菜单结果提交；无暂存时的中止路径（`quit`、stdin 立即 EOF、非法输入后 EOF）由同家族 `CS-09c` 承接。已拆出 `CS-09c`。
**Out of scope:** CLI 解禁与参数守卫（由 `CS-09` 承接）；独立的 `libra add -i`（尚未排期，D15 继续拒绝）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `add.rs` 只有 `-p/--patch` hunk 引擎，无菜单驱动 | `src/command/add.rs:1766-1767` |
| Git 菜单 `status` 行格式与 `diff` 显示暂存差异（实测 `printf '2\n1\n\n1\n6\n1\n7\n' | git commit -m s --interactive` 得到上述两行） | `add-interactive.c:600-606,609-619,938-975,1089` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** `file` 为已修改未暂存的跟踪文件（HEAD 内容 `F1\n`，工作区内容 `F2\n`）；`other` 有两处相距足够远、可独立选择的未暂存 hunk；`new` 为内容 `N1\n` 的未跟踪文件；另有已暂存修改的跟踪文件 `s`（暂存内容 S1，工作区同 S1），与菜单操作路径不同；菜单输入经管道提供；不启用颜色（菜单与文件列表按 Git 以方括号标记唯一前缀）。期望树 E_file 在另一独立副本上以 `libra add file` 后 `libra write-tree` 求得（因而含预先暂存的 `s`=S1；本机 `git` 实测 `commit --interactive file` 同样提交预先暂存的路径）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：管道输入 `2\n*\n\n7\n`（update → 全选 → 结束选择 → quit）时 `libra commit -m foo --interactive file` 生成的提交树等于 E_file（`other` 虽有修改，不在限定路径内而不被选中；预先暂存的 `s` 保留在提交中）。
- [ ] AC2：管道输入 `5\n\n7\n`（patch → 结束选择 → quit）时 `libra commit -m p --interactive file` 的 stdout 中 patch 的文件候选列表只列出 `file`。
- [ ] AC3：`add untracked` 选择 `new` 后 quit 时，生成的提交树中 `new` 的 blob 内容为 `N1\n`。
- [ ] AC4：`patch` 命令对 `other` 选择第一个 hunk 后 quit，生成的提交中 `other` 只含该 hunk 的改动。
- [ ] AC5：管道输入 `2\n1\n\n1\n6\n1\n7\n`（update 选第 1 项 `file` → 结束选择 → status → diff 选第 1 项 → quit）时 `libra commit -m s --interactive` 的 stdout 含整行 `  1:        +1/-1      nothing file`（Git `%c%2d: %12s %12s %s` 格式，`add-interactive.c:600-606,1089`；只有 update 之后执行的 `status` 渲染该行：启动时自动输出的行为 `  1:    unchanged        +1/-1 file`，diff 候选列表中同一文件显示为 `[f]ile`，二者均不等于该行）。
- [ ] AC6：同一命令的 stdout 含整行 `+F2`（`diff` 命令以 `diff -p --cached` 显示 `file` 的暂存差异，`add-interactive.c:938-975`；update 后工作区与索引一致，未暂存差异不含该行）。
- [ ] AC7：管道输入 `2\n*\n\n3\n2\n\n7\n`（update 全选 `file` 与 `other` → 结束选择 → revert 选第 2 项 `other` → 结束选择 → quit）时 `libra commit -m v --interactive` 生成的提交树等于 E_file（`other` 回到 HEAD 内容、`file` 的暂存保留；本机 `git` 同输入实测只提交 `file`）。
- [ ] AC8：AC7 前后 SNAP-Δ（只允许 ① 当前分支 ref、② 其 reflog、③ 中 `file` 条目的 OID（变为 F2 的 blob）、⑤ 新增的 F2 blob、`other` 工作区内容的 blob（update 写入、revert 后不再被引用）、新提交及其树对象变化；③ 其余条目——含恢复为原条目的 `other` 与 `s`（S1）——的全部字段与标志、④ 工作区全量不变）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_interactive_piped_staging)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::add_patch_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-02` 的 `T-2` 覆盖）
**Dependencies:** `CS-09b`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-02`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/command/add.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-02` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--interactive` 菜单命令与管道用法）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-02 (3/4)`
**C/D coverage from:** `REL-CS-02`
**Granularity:** `type=implementation; axis=--interactive菜单暂存与管道提交; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=CS-09b,DEP-CS-07; writeset=序列化于 CS-09b; release=family child of REL-CS-02 (3/4); split-from=CS-09; exception=N/A`

---

### Task CS-09c: `commit --interactive` 在无暂存内容时的中止：`quit`、立即 EOF 与非法输入（#773）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-09a` 拆出：`commit --interactive` 的菜单结束时若没有任何暂存内容——用户 `quit`、stdin 立即 EOF，或输入非法菜单项后 EOF——不创建提交并以失败结束，主索引不变；非法菜单项按 Git 向 stderr 输出 `Huh (<输入>)?`（`add-interactive.c:331-335`）。本机 `git` 实测：update 后 revert 再 quit、空 stdin、`9` 后 EOF 三种输入下 `git commit --interactive` 均以退出码 1 结束且不创建提交。
**Out of scope:** 菜单命令本身（由 `CS-09a` 承接）；CLI 解禁与参数守卫（由 `CS-09`、`CS-09b` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 非法菜单项提示输出到 stderr | `add-interactive.c:331-335` @ `6de20f6092` |
| `add.rs` 只有 `-p/--patch` hunk 引擎，无菜单驱动 | `src/command/add.rs:1766-1767` |
| Git 交互 `update` 以写对象方式加入索引，`revert` 只恢复索引条目、不删除已写入的 blob | `add-interactive.c:622-697`; `read-cache.c:721-732,791-794` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** `file` 为已修改未暂存的跟踪文件（HEAD 内容 `F1\n`，工作区内容 `F2\n`）；无其它已暂存内容；菜单输入经管道提供；每个用例以 30 秒为上限等待命令结束，超时即用例失败（检出读取 EOF 后挂起的实现）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：管道输入 `2\n1\n\n3\n1\n\n7\n`（update 选 `file` → 结束选择 → revert 选 `file` → 结束选择 → quit）时 `libra commit -m r --interactive` 以 `LBR-REPO-003` 失败（`update` 已写入的 `F2` blob 按 SNAP 节「失败路径的 ⑤ 口径」允许保留，其余分量 SNAP-EQ）。
- [ ] AC2：stdin 立即 EOF（空输入）时 `libra commit -m e --interactive` 以 `LBR-REPO-003` 失败。
- [ ] AC3：输入非法菜单项 `9` 后 EOF 时 `libra commit -m h --interactive` 以 stderr 含 Git 同款提示 `Huh (9)?` 的错误失败（Git `add-interactive.c:333-334` 输出到 stderr；无暂存内容故不创建提交）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_interactive_abort_without_staging)'`（V-2；new；覆盖 AC1–AC3）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-02` 的 `T-2` 覆盖）
**Dependencies:** `CS-09a`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-02`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/command/add.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-02` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--interactive` 无暂存时的中止与非法输入提示）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-02 (4/4)`
**C/D coverage from:** `REL-CS-02`
**Granularity:** `type=implementation; axis=--interactive无暂存中止路径; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=CS-09a,DEP-CS-07; writeset=序列化于 CS-09a; release=family child of REL-CS-02 (4/4); split-from=CS-09a; exception=N/A`

---

### Task REL-CS-02: `REL-CS-02` `commit --interactive` 交互式暂存家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-09`（1/4）、`CS-09b`（2/4）、`CS-09a`（3/4）、`CS-09c`（4/4） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-09`、`CS-09b`、`CS-09a`、`CS-09c` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `CS-09` 解禁 `--interactive` 后必须与 `CS-09a` 菜单同版发布，避免中间版本空悬选项 | `src/cli.rs:3017-3027`; `plan-template.md` G-08 |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-09`、`CS-09b`、`CS-09a`、`CS-09c` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `command::commit_test::commit_interactive_cli_guards` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit_test::commit_interactive_mode_exclusivity` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::commit_test::commit_interactive_piped_staging` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：聚合守卫 `command::commit_test::commit_interactive_abort_without_staging` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC5：聚合守卫 compat target `compat_declined_interactive_flags` 在子卡交接后、本卡 bump 之前的合成树上按 V-3 口径通过。
- [ ] AC6：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-02 begin -->` 与 `<!-- release-note-draft:REL-CS-02 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 4 行，顺序同家族登记：`CS-09`、`CS-09b`、`CS-09a`、`CS-09c`）。
- [ ] AC7：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_interactive_cli_guards)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_interactive_mode_exclusivity)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_interactive_piped_staging)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_interactive_abort_without_staging)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC4）
- [ ] V5 `source .env.test && cargo nextest run --test compat_declined_interactive_flags`（V-3 口径；聚合守卫，在 bump 前合成树运行；覆盖 AC5）
- [ ] V6 手工证据（覆盖 AC6）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-02 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-02 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-09 CS-09b CS-09a CS-09c; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V7 手工证据（覆盖 AC7）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-09` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-09b` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-09a` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-09c` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-09`, `CS-09b`, `CS-09a`, `CS-09c`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-09`、`CS-09b`、`CS-09a`、`CS-09c` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-02`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-02家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/12; VER=7/12; landing=0; prod-files=0; scope=S; deps=CS-09,CS-09b,CS-09a,CS-09c; writeset=序列化于 CS-09c; release=family release point of REL-CS-02; split-from=N/A; exception=N/A`

---

### Task CS-07: 支持 `--fixup=amend:<commit>` / `--fixup=reword:<commit>` 语义（#789, #790）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git `builtin/commit.c:1378-1410` 解析 `--fixup=[(amend|reword):]<commit>`：两种前缀都生成以 `amend! <subject>` 开头、正文为目标提交消息并允许编辑器修改的提交，且设置 `allow_empty`；`amend:` 提交当前索引，`reword:` 隐含 `--only` 且无路径，提交树等于 HEAD 树、索引与工作区保持不变。冲突诊断由 `CS-07a`、`CS-07b` 承接，三卡经 `REL-CS-09` 同版发布，使 `reword:` 不会在缺少冲突守卫的版本中公开。已拆出 `CS-07a`。
**Out of scope:** 冲突与未知前缀诊断（由 `CS-07a` 承接）；`rebase --autosquash` 执行侧（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `args.fixup` 整体当作提交名解析，无 `amend:`/`reword:` 前缀 | `src/command/commit.rs:1845-1849` |
| Git 前缀解析与 reword 隐含 `only` | `builtin/commit.c:1378-1410` @ `6de20f6092` |
| Git amend 消息：普通目标用 `%B`，目标主题已为 `amend!` 时用 `%b` | `builtin/commit.c:739-751,825-855` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 目标提交 t 的主题为 `target subject`、正文为 `body`；另有已暂存修改 `s` 与未暂存修改 `w`；编辑器脚本在正文末尾追加 `edited` 行；AC7 另用无已暂存变更的独立夹具；期望字节由执行者用 pinned `6de20f6092` 构建的 git 对同一输入实测得到，作为常量写入用例，并在用例注释记录来源命令。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --fixup=amend:<t>`（经编辑器）的提交消息 MSG 与固定期望字节一致（`amend! target subject`、空行、t 的完整消息即主题+空行+正文，再接编辑器追加的 `edited`）。
- [ ] AC2：同一 `amend:` 提交的提交树包含已暂存修改 `s`。
- [ ] AC3：`libra commit --fixup=reword:<t>`（经编辑器）的提交消息 MSG 与 AC1 的期望字节一致。
- [ ] AC4：该 `reword:` 提交的提交树等于原 HEAD 树。
- [ ] AC5：该 `reword:` 提交前后 SNAP-Δ（只允许 ① HEAD 所在分支 ref、② 其 reflog 与 ⑤ 新提交对象变化；③ 索引与 ④ 工作区完全不变）。
- [ ] AC6：目标提交 t2 的主题本身以 `amend! ` 开头时，`libra commit --fixup=amend:<t2>` 的提交消息 MSG 与固定期望字节一致（只接 t2 的正文，Git `prepare_amend_commit` 的 `%b` 分支）。
- [ ] AC7：无已暂存变更的仓库中 `libra commit --fixup=amend:<t>` 生成提交，其树等于原 HEAD 树（`amend:` 同样设置 `allow_empty`）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_autosquash_test::commit_fixup_amend_and_reword)'`（V-2；new；覆盖 AC1–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_autosquash_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-09` 的 `T-2` 覆盖）
**Dependencies:** `REL-CS-02`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_autosquash_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-09` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--fixup=amend:`/`reword:`）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-09 (1/3)`
**C/D coverage from:** `REL-CS-09`
**Granularity:** `type=implementation; axis=--fixup=amend:/reword:语义; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=REL-CS-02,DEP-CS-07; writeset=序列化于 REL-CS-02; release=family child of REL-CS-09 (1/3); split-from=N/A; exception=N/A`

---

### Task CS-07a: 补齐 `--fixup=reword:` 与暂存类选项及路径的冲突诊断（#789, #790）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-07` 拆出：按 Git `builtin/commit.c:1295-1306` 精确文案拒绝合并进行中的 `reword:`（`You are in the middle of a merge -- cannot reword.`），以及 `reword:` 与 `--all`、`--include`、`--only`、`--interactive`、`--patch` 或路径参数同用。`-m` 冲突与未知前缀由 `CS-07b` 承接。
**Out of scope:** 前缀语义（由 `CS-07` 承接）；`-m` 冲突与未知前缀（由 `CS-07b` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git reword 冲突检查 | `builtin/commit.c:1295-1306` @ `6de20f6092` |
| 当前合并守卫只拦 `--amend` | `src/command/commit.rs:1106-1110` |

**夹具（FX-BASE 之外的差异化条件）:** 目标提交 t 的主题为 `target subject`、正文为 `body`，位于 HEAD 之前；工作区有已暂存修改 `s` 与跟踪文件 `a`。另建一个停在冲突合并状态（存在 `MERGE_HEAD`，冲突已解决并暂存）的仓库。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`--fixup=reword:<t> --all` 以 stderr 含 `reword option of '--fixup' and '--patch/--interactive/--all/--include/--only' cannot be used together` 的错误失败。
- [ ] AC2：`--fixup=reword:<t> --include` 以 stderr 含同一文案的错误失败。
- [ ] AC3：`--fixup=reword:<t> --only` 以 stderr 含同一文案的错误失败。
- [ ] AC4：`--fixup=reword:<t> --interactive` 以 stderr 含同一文案的错误失败。
- [ ] AC5：`--fixup=reword:<t> --patch` 以 stderr 含同一文案的错误失败。
- [ ] AC6：`--fixup=reword:<t> -- a` 以 stderr 含 `reword option of '--fixup' and path 'a' cannot be used together` 的错误失败。
- [ ] AC7：`--fixup=reword:<t> --all -- a` 以 stderr 含 `reword option of '--fixup' and path 'a' cannot be used together` 的错误失败（Git 先检查路径，`builtin/commit.c:1302-1303`）。
- [ ] AC8：冲突合并状态下 `libra commit --fixup=reword:<t>` 以 stderr 含 `You are in the middle of a merge -- cannot reword.` 的错误失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_autosquash_test::commit_fixup_reword_conflicts)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_autosquash_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-09` 的 `T-2` 覆盖）
**Dependencies:** `CS-07`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-09`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_autosquash_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-09` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`reword:` 冲突）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-09 (2/3)`
**C/D coverage from:** `REL-CS-09`
**Granularity:** `type=implementation; axis=reword冲突诊断; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-07,DEP-CS-07; writeset=序列化于 CS-07; release=family child of REL-CS-09 (2/3); split-from=CS-07; exception=N/A`

---

### Task CS-07b: 补齐 `-m` 与 `--fixup=amend:/reword:` 冲突及未知前缀诊断（#789, #790）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-07a` 拆出：按 Git `builtin/commit.c:853` 拒绝 `-m` 与 `amend:`/`reword:` 同用（`options '-m' and '--fixup:<prefix>' cannot be used together`），按 `:1404-1406` 拒绝未知前缀（`unknown option: --fixup=<x>:<c>`）；无前缀 `--fixup=<c>` 与 `-m` 的组合保持 `CS-05` 的拼接语义。
**Out of scope:** reword 与暂存类选项冲突（由 `CS-07a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git `-m` 冲突与未知前缀文案 | `builtin/commit.c:853,1404-1406` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 目标提交 t 的主题为 `target subject`、正文为 `body`，位于 HEAD 之前；工作区有已暂存修改 `s` 与跟踪文件 `a`。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`-m x --fixup=amend:<t>` 以 stderr 含 `options '-m' and '--fixup:amend' cannot be used together` 的错误失败。
- [ ] AC2：`-m x --fixup=reword:<t>` 以 stderr 含 `options '-m' and '--fixup:reword' cannot be used together` 的错误失败。
- [ ] AC3：`--fixup=bogus:<t>` 以 stderr 含 `unknown option: --fixup=bogus:` 的错误失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_autosquash_test::commit_fixup_message_and_prefix_errors)'`（V-2；new；覆盖 AC1–AC3）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_autosquash_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-09` 的 `T-2` 覆盖）
**Dependencies:** `CS-07a`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-09`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_autosquash_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-09` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`-m` 与前缀冲突、未知前缀）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-09 (3/3)`
**C/D coverage from:** `REL-CS-09`
**Granularity:** `type=implementation; axis=-m与前缀冲突及未知前缀; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-07a,DEP-CS-07; writeset=序列化于 CS-07a; release=family child of REL-CS-09 (3/3); split-from=CS-07a; exception=N/A`

---

### Task REL-CS-09: `REL-CS-09` `--fixup=amend:`/`reword:` 与冲突诊断家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-07`（1/3）、`CS-07a`（2/3）、`CS-07b`（3/3） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-07`、`CS-07a`、`CS-07b` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `reword:` 语义（`CS-07`）与其冲突守卫（`CS-07a`、`CS-07b`）须同版发布，否则中间版本会忽略路径、暂存选项或 `-m` 而生成非预期提交 | `builtin/commit.c:848-853,1295-1306,1378-1406` @ `6de20f6092`; `plan-template.md` G-08 |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-07`、`CS-07a`、`CS-07b` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `command::commit_autosquash_test::commit_fixup_amend_and_reword` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit_autosquash_test::commit_fixup_reword_conflicts` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::commit_autosquash_test::commit_fixup_message_and_prefix_errors` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-09 begin -->` 与 `<!-- release-note-draft:REL-CS-09 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 3 行，顺序同家族登记：`CS-07`、`CS-07a`、`CS-07b`）。
- [ ] AC5：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_autosquash_test::commit_fixup_amend_and_reword)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_autosquash_test::commit_fixup_reword_conflicts)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_autosquash_test::commit_fixup_message_and_prefix_errors)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 手工证据（覆盖 AC4）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-09 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-09 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-07 CS-07a CS-07b; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V5 手工证据（覆盖 AC5）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-07` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-07a` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-07b` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-07`, `CS-07a`, `CS-07b`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-07`、`CS-07a`、`CS-07b` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-09`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-09家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/12; VER=5/12; landing=0; prod-files=0; scope=S; deps=CS-07,CS-07a,CS-07b; writeset=序列化于 CS-07b; release=family release point of REL-CS-09; split-from=N/A; exception=N/A`

---

### Task CS-16: 支持 `libra commit --template ":(optional)<path>"` 可选模板前缀（#792）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `resolve_commit_template`（覆盖 `-t`/`--template` 与 `commit.template`）解析 Git `:(optional)` 前缀：去前缀后的文件存在则作为模板，不存在则当作未给模板继续提交（t7500 `:40-68,102-108`）。
**Out of scope:** pathspec 匹配引擎（不涉及）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_commit_template` 把 `:(optional)<path>` 当普通文件名打开 | `src/command/commit.rs:2016-2049` |
| Git t7500 optional 模板用例 | `t/t7500-commit-template-squash-signoff.sh:40-68,102-108` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** `GIT_EDITOR` 脚本在收到的文件开头插入一行 `msg`、保留其余内容；存在的模板内容为单行 `template line`；缺失路径在隔离目录下保证不存在；存在模板时的期望 MSG `msg\ntemplate line` 已以本机 `git` 同输入实测（`--template` 与 `commit.template` 两种方式一致）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --template ":(optional)<缺失路径>"` 生成的提交消息 MSG 为 `msg`（缺失模板被当作未给模板）。
- [ ] AC2：`commit.template=":(optional)<缺失路径>"` 时 `libra commit` 生成的提交消息 MSG 为 `msg`。
- [ ] AC3：`libra commit --template ":(optional)<存在路径>"` 时编辑器收到的初始内容以 `template line` 开头。
- [ ] AC4：`commit.template=":(optional)<存在路径>"` 时编辑器收到的初始内容以 `template line` 开头。
- [ ] AC5：无前缀的缺失模板 `libra commit --template <缺失路径>` 以 `LBR-IO-001` 失败。
- [ ] AC6：`libra commit --template ":(optional)<存在路径>"` 生成的提交消息 MSG 为 `msg\ntemplate line`（模板被采用且编辑后的内容被提交）。
- [ ] AC7：`commit.template=":(optional)<存在路径>"` 时 `libra commit` 生成的提交消息 MSG 为 `msg\ntemplate line`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_optional_template_prefix)'`（V-2；new；覆盖 AC1–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_editor_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `REL-CS-09`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_editor_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`:(optional)` 模板）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=:(optional)模板前缀; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=REL-CS-09,DEP-CS-07; writeset=序列化于 REL-CS-09; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-17: `commit --date` 经 `src/utils/approxidate.rs` 专用入口接受 t7501 approxidate 句式（#769, #771）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `commit` 的签名日期解析（`parse_signature_date`，`src/command/commit.rs:820-880`）前新增一个专用入口（与 `parse_expiry_date` 分开，位于 `src/utils/approxidate.rs`），只额外接受 t7501 所用句式 `<noon|midnight> the <序数日> of <英文月份全称>[,] anno domini <四位年>`（`#769` 的验收用例 t7501#58）；其余输入原样交给现有 `parse_signature_date` 路径（行为不变）。其它 Git approxidate 表达式（点分或空格分隔的相对日期如 `2.weeks.ago`、自然语言如 `last friday`）不在本卡范围、行为不变，由 `DEFER-CS-06` 承接，差异登记 `COMPATIBILITY.md`；其中 `last friday` 现以 `LBR-CLI-002` 拒绝并由本卡验收。`parse_date` 与 `parse_expiry_date` 的接受集合保持不变（以改动前实测基线逐输入固定），统一扩展由 `DEFER-CS-04` 承接。无时区 ISO 本地解析与夏令时边界由 `CS-17a` 承接。已拆出 `CS-17a`。
**Out of scope:** `parse_date` 共享调用方的语义统一（`DEFER-CS-04`）；`config --type=expiry-date` 接受集合（不改变）；其它 approxidate 表达式与相对日期（`DEFER-CS-06`）；`Author:`/`Date:` 人类输出（由 `CS-18a`、`CS-18c` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| commit 日期解析回退 `parse_date`，不支持无时区 ISO 与 approxidate 自然语言 | `src/command/commit.rs:820-880`; `src/internal/log/date_parser.rs:14-66` |
| `approxidate.rs` 目前只服务 `config --type=expiry-date`，并刻意拒绝其它形式以免放宽配置值 | `src/utils/approxidate.rs:1-9`; `src/command/config.rs:2694,2717` |
| 非法日期映射 `LBR-CLI-002` | `src/command/commit.rs:342-347,483-486` |

**夹具（FX-BASE 之外的差异化条件）:** AC1、AC2 与 AC4–AC8 在 `TZ=UTC` 下运行，AC3 在 `TZ=Asia/Shanghai`（+0800）下运行，使把该句式一律按 UTC 解释的错误实现可区分；AC1–AC3 的三个输入取不同月份与不同序数后缀（`12th`、`1st`、`23rd`），使只识别单一月份或单一序数的错误实现可区分；三个期望值均以本机 `git` 同输入同时区实测核对。`config --type=expiry-date` 的读取方式：以不带类型的 `libra config set gc.pruneExpire '<输入>'` 原样存储，再以 `--type=expiry-date` 读取 `gc.pruneExpire`，结果记为读取的 stdout（接受）或稳定错误码（拒绝）。基线：执行者在本卡改动前的提交树上对 AC6–AC8 的每个输入按同一方式实测，把结果写入用例常量（所选输入的结果均与当前时钟无关）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`TZ=UTC` 下 `libra commit --amend --no-edit --date "midnight the 12th of october, anno domini 1979"` 的新提交 author 头时间为 `308534400 +0000`（即 t7501 期望的 `Fri Oct 12 00:00:00 1979 +0000`）。
- [ ] AC2：`TZ=UTC` 下 `--date "noon the 1st of january anno domini 2000"`（`noon` 分支、省略逗号）的 author 头时间为 `946728000 +0000`。
- [ ] AC3：`TZ=Asia/Shanghai` 下 `--date "midnight the 23rd of february, anno domini 2021"` 的 author 头时间为 `1614009600 +0800`（按本地时区的午夜解释，Git `date.c` 的 approxidate 以 `localtime_r` 求值）。
- [ ] AC4：`--date seventeen` 以 `LBR-CLI-002` 失败（与 t7501「commit complains about completely bogus dates」一致）。
- [ ] AC5：`--date "last friday"` 以 `LBR-CLI-002` 失败（不支持的自然语言表达式）。
- [ ] AC6：`libra config --type=expiry-date` 对 `midnight the 12th of october, anno domini 1979` 的结果等于改动前基线。
- [ ] AC7：对 `noon the 12th of october anno domini 1979` 的结果等于改动前基线。
- [ ] AC8：对 `2024-05-01 12:30:00` 的结果等于改动前基线。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_date_approxidate)'`（V-2；new；覆盖 AC1–AC5）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::config_test::expiry_date_accepted_set_unchanged)'`（V-2；new；覆盖 AC6–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^utils::approxidate::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::log_test::/)'`（V-3 共享回归组；`parse_date` 未改，调用方无回归）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::rev_list_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `T-1`：改动共享 helper `src/utils/approxidate.rs`（`config --type=expiry-date` 共用）
**Dependencies:** `CS-16`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/approxidate.rs`, `src/command/commit.rs`, `tests/command/commit_test.rs`, `tests/command/config_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--date` 额外接受的 t7501 句式与不支持的 approxidate 形式；`config --type=expiry-date` 接受集合不变）、`COMPATIBILITY.md` commit 日期行（注明 `log` 等共享调用方见 `DEFER-CS-04`）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=commit --date approxidate入口; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=CS-16,DEP-CS-07; writeset=序列化于 CS-16; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-17a: `commit --date` 无时区 ISO 按本地时区解析与夏令时边界（#769, #771）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-17` 拆出：`commit --date` 的入口支持无时区 ISO datetime（`YYYY-MM-DD HH:MM:SS` / `YYYY-MM-DDTHH:MM:SS`）按本地时区解析，返回显式偏移写入提交头；夏令时不存在与重复的本地时间按 pinned Git 实测结果处理。只改提交日期入口，`parse_expiry_date` 的接受集合不变（由 `CS-17` 的基线守卫持续校验）。
**Out of scope:** approxidate 表达式与相对日期（由 `CS-17` 承接）；`parse_date` 与 `parse_expiry_date` 扩展（`DEFER-CS-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `parse_signature_date` 不接受无时区 ISO | `src/command/commit.rs:820-880` |
| approxidate 模块边界 | `src/utils/approxidate.rs:1-9,76-123,159-209` |

**夹具（FX-BASE 之外的差异化条件）:** 分别在 `TZ=Asia/Shanghai`（+0800，无夏令时）与 `TZ=America/New_York`（含夏令时切换）下运行；夏令时边界输入的期望值由执行者用 pinned `6de20f6092` 构建的 git 在 `GIT_TEST_DATE_NOW` 固定的条件下实测并写入用例常量。另在本卡改动前的提交树上，于 `TZ=America/New_York` 下按 `CS-17` 的方式（原样存储后以 `--type=expiry-date` 读取 `gc.pruneExpire`）实测 `2024-03-10 02:30:00` 与 `2024-11-03 01:30:00` 的结果并写入用例常量。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`TZ=Asia/Shanghai` 下 `libra commit --amend --no-edit --date "2024-05-01 12:30:00"` 的 author 头为 `1714537800 +0800`。
- [ ] AC2：同一时区下 `--date "2024-05-01T12:30:00"` 的 author 头为 `1714537800 +0800`。
- [ ] AC3：`TZ=America/New_York` 下不存在的本地时间 `2024-03-10 02:30:00` 的 author 头与 pinned Git 实测常量一致。
- [ ] AC4：同一时区下重复的本地时间 `2024-11-03 01:30:00` 的 author 头与 pinned Git 实测常量一致。
- [ ] AC5：`TZ=America/New_York` 下 `config --type=expiry-date` 对 `2024-03-10 02:30:00` 的结果等于改动前基线。
- [ ] AC6：同一时区下对 `2024-11-03 01:30:00` 的结果等于改动前基线。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_date_local_iso_and_dst)'`（V-2；new；覆盖 AC1–AC4）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::config_test::expiry_date_dst_inputs_unchanged)'`（V-2；new；覆盖 AC5–AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::config_test::expiry_date_accepted_set_unchanged)'`（V-3 共享回归组；`CS-17` 的接受集合基线守卫）
- [ ] `source .env.test && cargo nextest run --lib -E 'test(/^utils::approxidate::/)'`（V-3 共享回归组）

**Full-suite trigger:** `T-1`：改动共享 helper `src/utils/approxidate.rs`（`config --type=expiry-date` 共用）
**Dependencies:** `CS-17`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/utils/approxidate.rs`, `src/command/commit.rs`, `tests/command/commit_test.rs`, `tests/command/config_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--date` 无时区 ISO 按本地时区解释与夏令时边界）、`COMPATIBILITY.md` commit 日期行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=commit --date本地ISO与夏令时; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=2; scope=M; deps=CS-17,DEP-CS-07; writeset=序列化于 CS-17; release=independent; split-from=CS-17; exception=N/A`

---

### Task CS-18: 修复 `--amend --author` 保留原 author date（#768）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 t7501「amend commit to fix author」（`t/t7501-commit-basic-functionality.sh:420-433` @ `6de20f6092`）：`commit --amend --author=<ident>`（无 `--date`）只替换 author 姓名与邮箱，保留原 author 时间与时区；显式 `--date` 时用该日期；`--reset-author` 仍取当前作者时间。已拆出 `CS-18b`、`CS-18a`（其后再拆出 `CS-18c`）。
**Out of scope:** `--reset-author` 使用范围（由 `CS-18b` 承接）；`Author:`/`Date:` 输出（由 `CS-18a`、`CS-18c` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `args.author.is_some()` 时用带当前时间戳的新 author 覆盖原 author | `src/command/commit.rs:1397-1405` |

**夹具（FX-BASE 之外的差异化条件）:** 原提交 author 为 `O A <oa@example.com>`、时间为固定值且时区 `-0700`；committer 身份与时区（`+0200`）均不同；`GIT_AUTHOR_DATE` 与 `GIT_COMMITTER_DATE` 在 amend 时固定为另两个值，使「取 committer 时间」或「取当前时间」的错误实现可区分。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --amend --no-edit --author="New <new@example.com>"` 的新提交 author 行等于 `author New <new@example.com> <原 author 时间戳> -0700`。
- [ ] AC2：同一新提交的 committer 行时间等于 amend 时固定的 `GIT_COMMITTER_DATE`。
- [ ] AC3：新、原两提交对象去掉 author 与 committer 两行后逐字节一致。
- [ ] AC4：`--amend --no-edit --author="New <new@example.com>" --date="2005-04-07T22:13:13 +0200"` 的新提交 author 时间与时区为 `1112904793 +0200`。
- [ ] AC5：`--amend --no-edit --reset-author` 的新提交 author 时间等于固定的 `GIT_AUTHOR_DATE`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test compat_commit_identity_date -E 'test(=amend_author_preserves_author_date)'`（V-2；new；覆盖 AC1–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test compat_commit_identity_date`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-17a`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/compat/commit_identity_date_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--amend --author` 保留日期）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--amend--author保留原日期; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-17a,DEP-CS-07; writeset=序列化于 CS-17a; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-18b: 校验 `--reset-author` 仅能与 `-C`/`-c`/`--amend` 联用（#779）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-18` 拆出：按 Git `builtin/commit.c:1322-1323,1355-1357`，`--reset-author` 未伴随 `-C`/`-c`/`--amend` 且不在 cherry-pick 续提时拒绝（`--reset-author can be used only with -C, -c or --amend.`），与 `--author` 同用拒绝；合法组合与 cherry-pick 冲突后续提不受影响。
**Out of scope:** `--amend` 后索引状态（由 `CS-20` 承接）；rebase 续提中的 `--reset-author`（Libra `rebase` 不经 `commit` 续提，永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `reset_author` 未校验伴随选项 | `src/command/commit.rs:204-205` |
| Git 校验与 cherry-pick 例外 | `builtin/commit.c:1322-1323,1355-1357` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 含两个提交的仓库，其 author 与当前 `GIT_AUTHOR_NAME/EMAIL/DATE` 均不同；`GIT_AUTHOR_*` 与 `GIT_COMMITTER_NAME/EMAIL/DATE` 设为两组不同的值；另一个停在 `libra cherry-pick` 冲突状态、已解决冲突并暂存的仓库。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit -a --reset-author -m done` 以 stderr 含 `--reset-author can be used only with -C, -c or --amend.` 的错误失败。
- [ ] AC2：`libra commit --amend --reset-author --author="X <x@example.com>" -m y` 以 stderr 含 `options '--reset-author' and '--author' cannot be used together` 的错误失败。
- [ ] AC3：`libra commit --amend --reset-author -m z` 生成的新提交 author 行等于由当前 `GIT_AUTHOR_NAME`、`GIT_AUTHOR_EMAIL`、`GIT_AUTHOR_DATE` 构成的期望 author 行（原提交 author 与之不同）。
- [ ] AC4：`libra commit -C <c> --reset-author` 生成的新提交 author 行等于同一期望 author 行（不同于提交者）。
- [ ] AC5：`libra commit -c <c> --reset-author`（编辑器不改内容）生成的新提交 author 行等于同一期望 author 行（`<c>` 的 author 与之不同）。
- [ ] AC6：冲突后续提的 cherry-pick 状态下 `libra commit --reset-author --no-edit` 生成的提交 author 行等于同一期望 author 行。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test compat_commit_identity_date -E 'test(=reset_author_requires_amend_or_reuse)'`（V-2；new；覆盖 AC1–AC5）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::cherry_pick_test::cherry_pick_continue_commit_reset_author)'`（V-2；new；覆盖 AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test compat_commit_identity_date`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-18`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/compat/commit_identity_date_test.rs`, `tests/command/cherry_pick_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--reset-author` 适用范围）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--reset-author使用范围校验; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CS-18,DEP-CS-07; writeset=序列化于 CS-18; release=independent; split-from=CS-18; exception=N/A`

---

### Task CS-18a: author 与提交者身份不同时在提交摘要输出 `Author:` 行（#776）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-18` 拆出：按 Git `sequencer.c:1465-1480`（`print_commit_summary`），author 身份（姓名与邮箱）与 committer 不同时在提交摘要输出 ` Author: <name> <email>`；`--json` 与 `--quiet` 输出不变。` Date:` 行的显示条件由 `CS-18c` 承接。已拆出 `CS-18c`。
**Out of scope:** `--amend --author` 日期保留（由 `CS-18` 承接）；` Date:` 行（由 `CS-18c` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `render_commit_output` 只打印 `[branch hash] subject` 与变更统计 | `src/command/commit.rs:2903-2964` |
| Git 摘要的 Author/Date 条件与格式；t7501「commit mentions forced date in output」 | `sequencer.c:1465-1480`; `builtin/commit.c:694-697`; `t/t7501-commit-basic-functionality.sh:472-475` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** `TZ=UTC`；提交者固定为 `C O <co@example.com>`；对照提交使用与提交者相同的作者身份且无 `--date`。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --author="A U <a@example.com>" -m x` 的人类输出含行 ` Author: A U <a@example.com>`。
- [ ] AC2：作者与提交者相同且无 `--date` 的普通提交，人类输出不含 ` Author:` 行。
- [ ] AC3：`libra commit --json --author="A U <a@example.com>" --date=2010-01-02T03:04:05 -m y` 的 stdout JSON 去掉随提交 OID 变化的 `commit`、`short_id` 字段后，与本卡改动前在同一夹具上实测的 JSON 逐字段相等（改动前实测值写入用例常量；本卡不向 JSON 增加字段）。
- [ ] AC4：`libra commit --quiet --author="A U <a@example.com>" -m z` 的 stdout 为空。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test compat_commit_identity_date -E 'test(=commit_output_author_line)'`（V-2；new；覆盖 AC1–AC4）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_json_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-18b`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/compat/commit_identity_date_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（摘要中的 Author 行）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=摘要Author行; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-18b,DEP-CS-07; writeset=序列化于 CS-18b; release=independent; split-from=CS-18; exception=N/A`

---

### Task CS-18c: 提交摘要按 Git `author_date_is_interesting` 输出 `Date:` 行（#771）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-18a` 拆出：按 Git `builtin/commit.c:694-697,1352-1363`，使用 `--date`，或以 `-C`/`-c`/`--amend` 复用原提交作者且未传 `--reset-author`（`author_message || force_date`）时，在提交摘要输出 ` Date: <%ad>`，按 author 头的时区偏移渲染（`sequencer.c:1465-1480`），其余情形不输出。Git 另在 cherry-pick 冲突续提且未传 `--reset-author` 时设置 `author_message`（`builtin/commit.c:1365-1368`）而显示日期；该路径依赖续提复用被拣选提交的作者，属 cherry-pick 行为轴，不在本卡范围，由 `DEFER-CS-07` 承接并登记 `COMPATIBILITY.md` 差异。
**Out of scope:** ` Author:` 行与 `--json`/`--quiet` 输出（由 `CS-18a` 承接）；`--amend --author` 日期保留（由 `CS-18` 承接）。cherry-pick 冲突续提时的 ` Date:` 行（`DEFER-CS-07`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 以 `author_message || force_date` 判定日期是否显示；`--reset-author` 时不设置 `author_message` | `builtin/commit.c:694-697,1352-1363` @ `6de20f6092` |
| t7501「commit mentions forced date in output」 | `t/t7501-commit-basic-functionality.sh:472-475` @ `6de20f6092` |
| `render_commit_output` 只打印 `[branch hash] subject` 与变更统计 | `src/command/commit.rs:2903-2964` |

**夹具（FX-BASE 之外的差异化条件）:** `TZ=UTC`；提交者固定为 `C O <co@example.com>`；普通对照提交无 `--date`；另有 author 日期为 `1262381645 +0530`、committer 日期为 `-0700` 的被 amend 提交 P（HEAD）；`GIT_AUTHOR_NAME/EMAIL/DATE` 固定为与 P 不同的值。本机 `git` 实测：`git commit --allow-empty -C HEAD` 的输出含 ` Date:`，`git commit --allow-empty -m plain` 的输出不含。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit --amend --no-edit --date=2010-01-02T03:04:05` 的人类输出含行 ` Date: Sat Jan 2 03:04:05 2010 +0000`。
- [ ] AC2：无 `--date`、非 `-C`/`-c`/`--amend` 的普通提交，人类输出不含 ` Date:` 行。
- [ ] AC3：`libra commit --allow-empty -C <P>`（无 `--date`）的人类输出含行 ` Date: Sat Jan 2 03:04:05 2010 +0530`（复用 P 的作者及其日期）。
- [ ] AC4：`libra commit --amend --no-edit`（无 `--date`）的人类输出含行 ` Date: Sat Jan 2 03:04:05 2010 +0530`（按 author 头偏移渲染，不转换为 UTC）。
- [ ] AC5：`libra commit --amend --no-edit --reset-author`（无 `--date`）的人类输出不含 ` Date:` 行（`--reset-author` 不复用原作者，`author_message` 未设置）。
- [ ] AC6：`GIT_EDITOR=true`（不修改内容）时 `libra commit --allow-empty -c <P>`（无 `--date`）的人类输出含行 ` Date: Sat Jan 2 03:04:05 2010 +0530`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test compat_commit_identity_date -E 'test(=commit_output_date_line)'`（V-2；new；覆盖 AC1–AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test compat_commit_identity_date`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-18a`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/compat/commit_identity_date_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（摘要中的 Date 行及其显示条件）、`COMPATIBILITY.md` commit 行（登记 cherry-pick 续提路径差异）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=摘要Date行显示条件; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-18a,DEP-CS-07; writeset=序列化于 CS-18a; release=independent; split-from=CS-18a; exception=N/A`

---

### Task CS-19: 支持 `libra commit --amend` 按 `notes.rewrite.amend` / `notes.rewriteRef` 复制 notes（#777）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git 语义（`notes.rewriteRef` 无默认值、未配置则不复制；`notes.rewrite.amend` 默认 true；`notes-utils.c:120-129` 拒绝 `refs/notes/` 之外的 ref）：amend 成功后在第二个 operation 阶段把原提交在单个精确 `notes.rewriteRef` 上的 note 复制（不移动）到新提交（Libra 的 notes 是 `notes` 表中 `(notes_ref, object) → blob` 映射，复制即为新提交新增一行、指向同一 blob）；目标提交已有 note 时按 `notes.rewriteMode` 的 Git 默认值 `concatenate` 合并（`notes-utils.c:108-118,144`）；`notes.rewriteMode` 的非默认值由 `CS-19c` 承接；`refs/notes/` 之外的值以 Git 同款警告 `Refusing to rewrite notes in <ref> (outside of refs/notes/)` 拒绝。多值与 glob 展开由 `CS-19b` 承接；第二阶段的锁序、持锁基线比较与失败语义由 `CS-19a` 承接；四卡经 `REL-CS-08` 同版发布。已拆出 `CS-19c`、`CS-19b`、`CS-19a`。
**Out of scope:** 多值与 glob 形式的 `notes.rewriteRef`（由 `CS-19b` 承接）；第二阶段的锁、基线比较与失败语义（由 `CS-19a` 承接）；`rebase` 的 `notes.rewrite.rebase`（尚未排期）；`GIT_NOTES_REWRITE_REF`（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| amend 路径只运行 `PostRewrite` hook，不复制 notes | `src/command/commit.rs:1580-1590` |
| repository 先于 worktree 的锁序；`command_may_mutate_shared_refs` 不含 `commit` | `src/internal/operation/middleware.rs:125-154,389-398` |
| Git `notes.rewriteMode` 解析与默认 `concatenate` | `notes-utils.c:108-118,144,178-184` @ `6de20f6092` |
| Libra notes 以 blob 存储、映射写入 SQLite `notes` 表（`notes_ref`、`object`、`blob`，唯一键 `(notes_ref, object)`），不建立 `refs/notes/*` ref、notes 树或提交对象 | `src/internal/notes.rs:1-5,212,567-584`; `sql/migrations/2026061401_notes.sql:1-9` |

**夹具（FX-BASE 之外的差异化条件）:** 原提交在 `refs/notes/commits` 上有 note `n1`，另有无关提交在 `refs/notes/commits` 上带 note `m1`；`notes.rewriteRef=refs/notes/commits`；对照夹具分别设 `notes.rewrite.amend=false`、不设 `notes.rewriteRef`、设 `notes.rewriteRef=refs/heads/main`。另一夹具中，执行者先在独立副本上以固定的 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` 运行同一 amend 求得新提交 OID，再在被测副本上为该 OID 预先添加 note `t1`；期望的合并后 note 字节由 pinned `6de20f6092` 构建的 git 对同一输入实测并写入用例常量。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`notes.rewrite.amend` 未设置时 `libra commit --amend -m x` 后新提交在 `refs/notes/commits` 上的 note 内容为 `n1`。
- [ ] AC2：同一 amend 前后 SNAP-Δ 只含：① 当前分支 ref、② 其 reflog、⑤ 新提交对象、⑨ `notes` 表新增一行 `(refs/notes/commits, <新提交>, <n1 的 blob>)`（原提交上的 `n1` 行与无关提交上的 `m1` 行不变）。
- [ ] AC3：`notes.rewrite.amend=false` 时 amend 前后 SNAP-Δ 只含当前分支 ref、其 reflog 与新提交对象的变化。
- [ ] AC4：未设置 `notes.rewriteRef` 时 amend 前后 SNAP-Δ 只含当前分支 ref、其 reflog 与新提交对象的变化。
- [ ] AC5：`notes.rewriteRef=refs/heads/main` 时 `libra commit --amend -m y` 的 stderr 含 `Refusing to rewrite notes in refs/heads/main (outside of refs/notes/)`。
- [ ] AC6：同一场景 amend 前后 SNAP-Δ 只含当前分支 ref、其 reflog 与新提交对象的变化。
- [ ] AC7：新提交 OID 已有 note `t1` 时，amend 后新提交在 `refs/notes/commits` 上的 note 字节与 pinned Git `concatenate` 实测常量一致。
- [ ] AC8：AC7 的 amend 前后 SNAP-Δ 只含：① 当前分支 ref、② 其 reflog、⑤ 新提交对象与合并后 note 的新 blob、⑨ `(refs/notes/commits, <新提交>)` 行的 `blob` 由 `t1` 的 blob 变为合并后 blob（原提交 `n1` 行与 `m1` 行不变）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_amend_notes_rewrite)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-08` 的 `T-2` 覆盖）
**Dependencies:** `CS-18c`, `DEP-CS-04`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/command/notes.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-08` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（amend 复制 notes 的配置与 `refs/notes/` 限制）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-08 (1/4)`
**C/D coverage from:** `REL-CS-08`
**Granularity:** `type=implementation; axis=amend按notes.rewrite复制notes; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=CS-18c,DEP-CS-04,DEP-CS-07; writeset=序列化于 CS-18c; release=family child of REL-CS-08 (1/4); split-from=N/A; exception=N/A`

---

### Task CS-19c: amend notes 复制对非默认 `notes.rewriteMode` 警告并放弃（#777）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-19` 拆出：`notes.rewriteMode` 为 `overwrite`、`ignore` 或 `cat_sort_uniq`（Git `notes-utils.c:108-118` 支持）时，Libra 不实现这些合并方式：输出警告 `notes.rewriteMode '<mode>' is not supported; notes not rewritten` 并放弃整个复制阶段，不改写 `notes` 表；差异登记 `COMPATIBILITY.md`。
**Out of scope:** 默认 `concatenate` 复制（由 `CS-19` 承接）；多值与 glob（由 `CS-19b` 承接）；锁与基线比较（由 `CS-19a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 支持的 rewrite 模式 | `notes-utils.c:108-118,144` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 原提交在 `refs/notes/commits` 上有 note `n1`；无关提交带 note `m1`；新提交 OID（预先在独立副本上以固定日期求得）已有 note `t1`；`notes.rewriteRef=refs/notes/commits`；三个独立副本分别设 `notes.rewriteMode=overwrite`、`ignore`、`cat_sort_uniq`。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`notes.rewriteMode=overwrite` 时 `libra commit --amend -m x` 的 stderr 含 `notes.rewriteMode 'overwrite' is not supported`。
- [ ] AC2：同一 amend 前后 SNAP-Δ 只含当前分支 ref 及其 reflog 与新提交对象的变化（⑨ `notes` 表全量行不变，`n1`、`m1`、`t1` 原样保留）。
- [ ] AC3：`notes.rewriteMode=ignore` 时 amend 的 stderr 含 `notes.rewriteMode 'ignore' is not supported`。
- [ ] AC4：同一 amend 前后 SNAP-Δ 只含当前分支 ref 及其 reflog 与新提交对象的变化。
- [ ] AC5：`notes.rewriteMode=cat_sort_uniq` 时 amend 的 stderr 含 `notes.rewriteMode 'cat_sort_uniq' is not supported`。
- [ ] AC6：同一 amend 前后 SNAP-Δ 只含当前分支 ref 及其 reflog 与新提交对象的变化。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_amend_notes_rewrite_unsupported_modes)'`（V-2；new；覆盖 AC1–AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-08` 的 `T-2` 覆盖）
**Dependencies:** `CS-19`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-08`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-08` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`notes.rewriteMode` 仅支持默认 `concatenate`，其它值警告并放弃）、`COMPATIBILITY.md` commit 行（差异登记）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-08 (2/4)`
**C/D coverage from:** `REL-CS-08`
**Granularity:** `type=implementation; axis=notes.rewriteMode非默认值; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-19,DEP-CS-07; writeset=序列化于 CS-19; release=family child of REL-CS-08 (2/4); split-from=CS-19; exception=N/A`

---

### Task CS-19b: amend notes 复制支持多值与 glob 形式的 `notes.rewriteRef`（#777）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-19` 拆出：按 Git `notes-utils.c:120-126,178-184`，`notes.rewriteRef` 可配置多条值且每条可为 glob；复制时对每个匹配的 notes ref（glob 在 `notes` 表现有的 `notes_ref` 取值集合上展开）分别把原提交的 note 复制到新提交。
**Out of scope:** 单 ref 语义与命名空间校验（由 `CS-19` 承接）；锁与基线比较（由 `CS-19a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 对每条 `notes.rewriteRef` 展开 glob 并遍历所有 notes 树 | `notes-utils.c:120-126,178-184` @ `6de20f6092` |
| Libra notes 以 blob 存储、映射写入 SQLite `notes` 表（`notes_ref`、`object`、`blob`，唯一键 `(notes_ref, object)`），不建立 `refs/notes/*` ref、notes 树或提交对象 | `src/internal/notes.rs:1-5,212,567-584`; `sql/migrations/2026061401_notes.sql:1-9` |

**夹具（FX-BASE 之外的差异化条件）:** 原提交在 `refs/notes/commits` 上有 note `n1`、在 `refs/notes/review` 上有 note `r1`、在 `refs/notes/other` 上有 note `o1`；另有无关提交在 `refs/notes/commits` 上带 note `m1`。夹具一配置两条 `notes.rewriteRef`（`refs/notes/commits`、`refs/notes/review`）；夹具二配置一条 glob `refs/notes/*`。两夹具相互独立。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：夹具一中 `libra commit --amend -m x` 后新提交在 `refs/notes/commits` 上的 note 内容为 `n1`。
- [ ] AC2：夹具一中新提交在 `refs/notes/review` 上的 note 内容为 `r1`。
- [ ] AC3：夹具一中新提交在 `refs/notes/other` 上无 note。
- [ ] AC4：夹具一 amend 前后 SNAP-Δ 只含：① 当前分支 ref、② 其 reflog、⑤ 新提交对象、⑨ `notes` 表新增两行 `(refs/notes/commits, <新提交>, <n1 的 blob>)` 与 `(refs/notes/review, <新提交>, <r1 的 blob>)`（`refs/notes/other` 的行、原 note 行与 `m1` 行不变）。
- [ ] AC5：夹具二中 `libra commit --amend -m y` 后新提交在 `refs/notes/commits` 上的 note 内容为 `n1`。
- [ ] AC6：夹具二中新提交在 `refs/notes/review` 上的 note 内容为 `r1`。
- [ ] AC7：夹具二中新提交在 `refs/notes/other` 上的 note 内容为 `o1`。
- [ ] AC8：夹具二 amend 前后 SNAP-Δ 只含：① 当前分支 ref、② 其 reflog、⑤ 新提交对象、⑨ `notes` 表为新提交新增三行（分别指向 `n1`、`r1`、`o1` 的 blob）（原 note 行与 `m1` 行不变）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_amend_notes_rewrite_multi_and_glob)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-08` 的 `T-2` 覆盖）
**Dependencies:** `CS-19c`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-08`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/command/notes.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-08` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`notes.rewriteRef` 多值与 glob）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-08 (3/4)`
**C/D coverage from:** `REL-CS-08`
**Granularity:** `type=implementation; axis=notes.rewriteRef多值与glob; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=CS-19c,DEP-CS-07; writeset=序列化于 CS-19c; release=family child of REL-CS-08 (3/4); split-from=CS-19; exception=N/A`

---

### Task CS-19a: amend notes 复制阶段的锁序、持锁基线比较与失败语义（#777）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-19` 拆出：第二阶段以非阻塞 try-acquire 严格按 `src/internal/operation/middleware.rs:389-398`「repository lease 先于 worktree lease」锁序取锁，与 `notes` 命令的全部写入共用同一 repository lease（`notes` 属 `command_may_mutate_shared_refs`，取锁失败报 `operation scope lease is already held`），第一阶段（写入新提交、推进 HEAD 之前）记录 `notes` 表中 `notes.rewriteRef` 对应 `notes_ref` 的全部行作为基线，第二阶段持锁后比较同一 `notes_ref` 的全部行，与基线不一致（含其它提交的 note 变化）即放弃，因而两阶段之间的任何同 ref notes 写入都会被检出；持锁期间 notes 写入被租约排斥；锁竞争或基线变化时输出 `warning: notes rewrite skipped` 并放弃复制，不影响已推进的 HEAD；operation-log 的 amend 条目不吸收第二阶段的 notes 变化。不把 `commit` 加入 `command_may_mutate_shared_refs`（`middleware.rs:125-154`），避免跨编辑器持锁影响 `REL-CS-05`。
**Out of scope:** 复制语义与配置（由 `CS-19` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| repository 先于 worktree 的锁序；`command_may_mutate_shared_refs` 不含 `commit` | `src/internal/operation/middleware.rs:125-154,389-398` |
| `notes` 取 repository lease；租约忙时报 `LeaseBusy` | `src/internal/operation/middleware.rs:125-154,185-195` |

**夹具（FX-BASE 之外的差异化条件）:** 原提交在 `refs/notes/commits` 上有 note `n1`，另有两个无关提交各带 note `m1`、`m2`；`notes.rewriteRef=refs/notes/commits`；以测试钩子在第一阶段记录基线之后、第二阶段开始之前注入受控交错（持有 repository lease，或并发 `libra notes add` 给第三个提交加 note `m3`，同属 `refs/notes/commits`）；另以测试钩子使第二阶段在取得 repository lease 并完成基线比较之后、写入 `notes` 表之前暂停。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：第二阶段开始前 repository lease 被测试持有时，`libra commit --amend -m x` 的 stderr 含 `warning: notes rewrite skipped`。
- [ ] AC2：同一场景 amend 前后 SNAP-Δ（① 当前分支 ref 变为本次 amend 生成的新提交、② 新增其 reflog 条目、⑤ 新增该提交对象；⑨ `notes` 表全量行不变）。
- [ ] AC3：第一阶段记录基线之后、第二阶段开始之前并发 `libra notes add` 为第三个提交写入 note `m3`（同一 `refs/notes/commits`）时，`libra commit --amend -m y` 的 stderr 含 `warning: notes rewrite skipped`。
- [ ] AC4：同一场景以并发 `notes add` 完成后、第二阶段开始前取得的快照 S_c 为基线，amend 结束时相对 S_c 为 SNAP-EQ（第二阶段未写入任何状态；⑦ 按统一口径；`n1`、`m1`、`m2`、`m3` 全部保留）。
- [ ] AC5：第二阶段失败时 operation-log 中 amend 条目的 before/after 只包含 HEAD 与当前分支 ref 的变化。
- [ ] AC6：第二阶段结束（成功或失败）后紧接的 `libra notes add` 立即成功（repository 与 worktree lease 均已释放）。
- [ ] AC7：第二阶段暂停于完成基线比较之后、写入之前时，并发 `libra notes add` 以 stderr 含 `operation scope lease is already held` 的错误失败（竞争窗口内无并发 notes 写入）。
- [ ] AC8：并发 `notes add` 改变基线的场景中，`libra commit --amend -m y` 结束后 HEAD 指向本次 amend 生成的新提交（退出码 0 为执行条件）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_amend_notes_rewrite_contention)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::notes_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-08` 的 `T-2` 覆盖）
**Dependencies:** `CS-19b`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-08`）, `DEP-CS-04`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/command/notes.rs`, `src/internal/operation/middleware.rs`, `src/internal/operation/middleware/lease.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-08` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（复制被跳过时的警告与锁语义）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-08 (4/4)`
**C/D coverage from:** `REL-CS-08`
**Granularity:** `type=implementation; axis=notes复制的锁与基线比较失败语义; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=3; prod-files=4; scope=M; deps=CS-19b,DEP-CS-04,DEP-CS-07; writeset=序列化于 CS-19b; release=family child of REL-CS-08 (4/4); split-from=CS-19; exception=N/A`

---

### Task REL-CS-08: `REL-CS-08` amend notes 复制家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-19`（1/4）、`CS-19c`（2/4）、`CS-19b`（3/4）、`CS-19a`（4/4） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-19`、`CS-19c`、`CS-19b`、`CS-19a` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 单 ref 复制（`CS-19`）、多值/glob（`CS-19b`）与锁与基线比较失败语义（`CS-19a`）须同版发布，避免中间版本漏复制或在锁竞争下覆盖并发 notes | `src/internal/operation/middleware.rs:125-154,389-398`; `plan-template.md` G-08 |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-19`、`CS-19c`、`CS-19b`、`CS-19a` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `command::commit_test::commit_amend_notes_rewrite` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit_test::commit_amend_notes_rewrite_unsupported_modes` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::commit_test::commit_amend_notes_rewrite_multi_and_glob` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：聚合守卫 `command::commit_test::commit_amend_notes_rewrite_contention` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC5：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-08 begin -->` 与 `<!-- release-note-draft:REL-CS-08 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 4 行，顺序同家族登记：`CS-19`、`CS-19c`、`CS-19b`、`CS-19a`）。
- [ ] AC6：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_amend_notes_rewrite)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_amend_notes_rewrite_unsupported_modes)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_amend_notes_rewrite_multi_and_glob)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_amend_notes_rewrite_contention)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC4）
- [ ] V5 手工证据（覆盖 AC5）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-08 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-08 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-19 CS-19c CS-19b CS-19a; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V6 手工证据（覆盖 AC6）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-19` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-19c` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-19b` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-19a` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-19`, `CS-19c`, `CS-19b`, `CS-19a`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-19`、`CS-19c`、`CS-19b`、`CS-19a` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-08`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-08家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/12; VER=6/12; landing=0; prod-files=0; scope=S; deps=CS-19,CS-19c,CS-19b,CS-19a; writeset=序列化于 CS-19a; release=family release point of REL-CS-08; split-from=N/A; exception=N/A`

---

### Task CS-20: 修复 `libra commit --amend` 后索引状态残留导致后续 `checkout` 被阻塞（#779）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 排查并修复 `commit --amend`（含 `-a --reset-author --amend`）完成后 index 条目或 stat 缓存与新 HEAD 不一致，导致紧接的 `libra checkout` / `libra switch` 误报已暂存修改并阻塞切换的问题；真实未暂存修改仍按既有安全检查拒绝。
**Out of scope:** 修改 `checkout`/`switch` 的脏工作区检查契约。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| amend 完成后未刷新 index 与新提交树及工作区 stat 的一致性 | `src/command/commit.rs:1538-1610` |

**夹具（FX-BASE 之外的差异化条件）:** 分支 `other` 与当前分支在文件 `f` 上内容不同；amend 前修改并暂存 `g`；对照：在 `f` 上留有真实未暂存修改。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit -a --reset-author --amend --no-edit` 后 `libra status --porcelain` 输出为空。
- [ ] AC2：`libra commit --amend -m x` 后 `libra status --porcelain` 输出为空。
- [ ] AC3：AC1 之后紧接 `libra switch other` 成功。
- [ ] AC4：AC2 之后紧接 `libra checkout other` 成功。
- [ ] AC5：`f` 有真实未暂存修改时 `libra switch other` 失败，其稳定错误码与本卡改动前在同一夹具上实测的错误码相同（实测值写入用例常量）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test compat_commit_amend_no_edit -E 'test(=amend_does_not_leave_index_dirty_for_checkout)'`（V-2；new；覆盖 AC1–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test compat_commit_amend_no_edit`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `REL-CS-08`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/compat/commit_amend_no_edit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`--amend` 完成后索引与新 HEAD 一致，随后的 `switch`/`checkout` 不会被误判为有未提交改动）、`COMPATIBILITY.md` commit 行（`COMPATIBILITY.md:209` 既有条目内追加，行数不变）、网站 `commit.en.md`（W-1..W-6，`--only commit`）；`switch`/`checkout` 文档不变：二者的脏工作区检查契约未改，本卡只修复 `commit --amend` 留下的索引状态。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=--amend后索引一致性; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=REL-CS-08,DEP-CS-07; writeset=序列化于 REL-CS-08; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-21: 修复 `commit-tree` 单源精确字节换行（#1032）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git `builtin/commit-tree.c:60-90,140-145`：`-m` 消息经 `strbuf_complete_line` 保证以换行结尾、多段 `-m` 以空行分隔；`-F <file>` 与 stdin 原样读取、不追加换行。修复 `src/command/commit_tree.rs:116-123` 对单源消息强制补 `\n` 导致比 Git 多 1 字节的问题。已拆出 `CS-21a`。多段 `-m` 时按 Git `builtin/commit-tree.c:64-67` 直接追加参数并 `strbuf_complete_line`，不先剥除参数自带的尾随换行（修复 `src/command/commit_tree.rs:124-129` 逐段 `trim_end_matches('\n')`）。
**Out of scope:** `cat-file --batch` 空行处理（由 `CS-21a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 单源消息强制 `format!("{single}\n")` | `src/command/commit_tree.rs:116-123` |
| Git `-m` 补全行尾、`-F`/stdin 原样读取 | `builtin/commit-tree.c:60-90,140-145` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 固定 author/committer 身份与时间、固定树；期望字节由执行者用 pinned `6de20f6092` 构建的 git 对同一输入实测得到，作为常量写入用例，并在用例注释记录来源命令。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`printf 'Initial commit' | libra commit-tree <tree>` 生成的提交对象与 pinned Git 同输入的对象逐字节一致（消息无尾随换行）。
- [ ] AC2：`libra commit-tree -F <内容为 Initial commit 且无尾随换行的文件> <tree>` 生成的对象与 AC1 的期望字节一致。
- [ ] AC3：`libra commit-tree -m msg <tree>` 生成的对象与 pinned Git 实测常量一致（消息以单个 `\n` 结尾）。
- [ ] AC4：`libra commit-tree -m a -m b <tree>` 生成的对象与 pinned Git 实测常量一致（消息为 `a\n\nb\n`）。
- [ ] AC5：`libra commit-tree -m $'a\n\n' -m b <tree>`（首个 `-m` 参数为字节 `61 0a 0a`：用例以 `Command::arg` 传入含两个真实换行的字符串；shell 复现须用 bash `$'…'`，双引号内的 `\n` 不是换行）生成的对象与 pinned Git 实测常量逐字节一致（消息字节为 `61 0a 0a 0a 62 0a`，保留参数已有换行、不先剥除；先剥除的错误实现得到 `61 0a 0a 62 0a`）。
- [ ] AC6：`libra cat-file -s <AC1 的 OID>` 输出等于 AC1 期望对象的字节长度。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_tree_test::commit_tree_exact_message_bytes)'`（V-2；new；覆盖 AC1–AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_tree_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-20`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit_tree.rs`, `tests/command/commit_tree_test.rs`, `docs/commands/commit-tree.md`, `docs/commands/zh-CN/commit-tree.md`, `docs/development/commands/commit-tree.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit-tree.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit-tree.md`、`docs/commands/zh-CN/commit-tree.md`、`docs/development/commands/commit-tree.md`（消息换行规则）、`COMPATIBILITY.md` commit-tree 行、网站 `commit-tree.en.md`（W-1..W-6，`--only commit-tree`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=commit-tree消息精确字节; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-20,DEP-CS-07; writeset=序列化于 CS-20; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-21a: 修复 `cat-file --batch` / `--batch-check` 空输入行 `missing` 输出（#1032）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-21` 拆出：`src/command/cat_file.rs:715-717`（`run_batch`）在 `--batch`/`--batch-check` 下遇空输入行按 Git（t1006「--batch-check for an empty line」）输出 ` missing`；`run_batch_command`（`:906-908`）遇空命令行按 Git `builtin/cat-file.c:938` 以 `empty command in input` 失败。
**Out of scope:** `commit-tree` 换行（由 `CS-21` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `run_batch` 与 `run_batch_command` 跳过空白输入行 | `src/command/cat_file.rs:715-717,906-908` |
| batch-command 其它非法输入映射 `LBR-CLI-002` | `src/command/cat_file.rs:929-935` |
| Git t1006 空行用例 | `t/t1006-cat-file.sh:438-446,1307-1311` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 含一个 blob（OID 记为 `<B>`，大小 `<bs>`）与一个提交（OID 记为 `<C>`，大小 `<cs>`）的仓库。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`echo | libra cat-file --batch-check` 的 stdout 逐字节为 ` missing\n`。
- [ ] AC2：`echo | libra cat-file --batch` 的 stdout 逐字节为 ` missing\n`。
- [ ] AC3：输入 `<B>\n\n<C>\n` 时 `libra cat-file --batch-check` 的 stdout 逐字节等于 `<B> blob <bs>\n missing\n<C> commit <cs>\n`。
- [ ] AC4：`echo "" | libra cat-file --batch-command` 以 stderr 含 Git 同款 `empty command in input` 的错误失败。
- [ ] AC5：`printf 'info <oid>\nflush\n' | libra cat-file --batch-command --buffer` 输出的 size 字段等于 `libra cat-file -s <oid>`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::cat_file_test::cat_file_batch_empty_line_missing)'`（V-2；new；覆盖 AC1–AC5）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::cat_file_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-21`, `DEP-CS-03`, `DEP-CS-04`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/cat_file.rs`, `tests/command/cat_file_test.rs`, `docs/commands/cat-file.md`, `docs/commands/zh-CN/cat-file.md`, `docs/development/commands/cat-file.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/cat-file.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/cat-file.md`、`docs/commands/zh-CN/cat-file.md`、`docs/development/commands/cat-file.md`（空输入行行为）、`COMPATIBILITY.md` cat-file 行、网站 `cat-file.en.md`（W-1..W-6，`--only cat-file`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=cat-file批处理空行; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-21,DEP-CS-03,DEP-CS-04,DEP-CS-07; writeset=序列化于 CS-21; release=independent; split-from=CS-21; exception=N/A`

---

### Task CS-22a: 对齐 `prepare-commit-msg` 参数与 Git 消息来源优先级，并登记 hooks 的 PD-08 边界（#767, #778）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 Git `builtin/commit.c:800-908` 的 `hook_arg1/hook_arg2`，`-m`/`-F` 优先于 `--amend`（第二参数 `message`），`-t` 为 `template`，`--amend` 无消息选项时为 `commit` 与字面 `HEAD`；修正 `src/command/commit.rs:2615-2639` 中 `--amend` 优先且传父提交 OID 的偏差。同卡在 `COMPATIBILITY.md` hooks 行（`COMPATIBILITY.md:260`）登记 PD-08 边界：`#767`/`#778` 的上游用例把 hook 写入 `.git/hooks`，Libra 只执行 `.libra/hooks`（`prepare-commit-msg` 的调用与改写、`commit-msg` 的失败中止已由 `compat_libra_hooks_lifecycle` 覆盖），按 `intentionally-different` 收口；`--no-verify` 在 Libra 中是跳过全部 hooks 的完整逃生阀（Git 只跳过 `pre-commit` 与 `commit-msg`），该差异不在本计划改变，由 `DEFER-CS-09` 承接。原 `CS-22` 已撤回。
**Out of scope:** `--no-verify` 对齐 Git（`DEFER-CS-09`）；执行 `.git/hooks` 或 `core.hooksPath`（`DEFER-CS-01`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Libra 在 `--amend` 时优先传 `commit <父 OID>` | `src/command/commit.rs:2615-2639` |
| Git hook 参数 | `builtin/commit.c:800-908` @ `6de20f6092` |
| `COMPATIBILITY.md` 规定仅执行 `.libra/hooks` | `COMPATIBILITY.md:260` |
| `--no-verify` 被既有测试固定为跳过全部 hooks 的完整逃生阀 | `tests/compat/libra_hooks_lifecycle_test.rs:163-172` |

**夹具（FX-BASE 之外的差异化条件）:** `.libra/hooks/prepare-commit-msg` 为把全部参数写入测试文件的脚本。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra commit -m x` 时 `prepare-commit-msg` 的第二个参数为 `message`。
- [ ] AC2：`echo y | libra commit -F -` 时第二个参数为 `message`。
- [ ] AC3：`libra commit --amend -m z` 时第二个参数为 `message`。
- [ ] AC4：`libra commit -t <tmpl>`（编辑器修改正文）时第二个参数为 `template`。
- [ ] AC5：`libra commit --amend --no-edit` 时第二个参数为 `commit`。
- [ ] AC6：同一调用的第三个参数为字面 `HEAD`（而非父提交 OID）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_prepare_commit_msg_hook_args)'`（V-2；new；覆盖 AC1–AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test compat_libra_hooks_lifecycle`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-21a`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`prepare-commit-msg` 参数）、`COMPATIBILITY.md:260` 所在 hooks 行（`prepare-commit-msg` 参数、PD-08 与 `#767`/`#778` 收口、`--no-verify` 跳过全部 hooks 的有意差异）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=prepare-commit-msg参数对齐; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-21a,DEP-CS-07; writeset=序列化于 CS-21a; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-23: 解除编辑器内嵌套 `libra commit` 被操作锁阻塞的问题（#784）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 `ADR-CS-04` 在 `Commands::Commit` 启动外部编辑器时签发受限单次重入令牌：只有 `commit` 子命令经原子单次认领后可在外层 `ScopeLease` 保护下写入，并登记不可复用进程身份，重复认领以 `commit editor reentry token already claimed` 失败、撤销后使用以 `commit editor reentry token is not active` 失败；编辑器退出时原子撤销未认领令牌。已认领后台嵌套 `commit` 的等待、超时终止与状态/op-log 隔离由 `CS-23a` 承接。
**Out of scope:** 非编辑器场景的并发互斥策略（不改变）；`CS-23a` 的等待与终止。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 非 worktree 命令以非等待方式获取 `ScopeLease`，嵌套 commit 以 `operation scope lease is already held` 失败 | `src/internal/operation/middleware.rs:192-196,411-416`; `src/internal/operation/middleware/lease.rs:83,130-153` |
| 外层录制 before/after ref 快照 | `src/internal/operation/middleware.rs:457,714` |

**夹具（FX-BASE 之外的差异化条件）:** `GIT_EDITOR` 为测试脚本，可在编辑期间执行嵌套命令、启动后台进程或在受控点暂停；认领与撤销通过测试钩子在两种先后顺序下交错。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：编辑器脚本内执行的 `libra commit -m nested`（另一已暂存变更）完成后、外层编辑器返回前，HEAD 指向该嵌套提交。
- [ ] AC2：嵌套提交在 operation-log 中有独立条目。
- [ ] AC3：两个编辑器内进程并发认领同一令牌时恰好产生一个嵌套提交。
- [ ] AC4：AC3 中未认领成功的进程以 stderr 含 `commit editor reentry token already claimed` 的错误失败。
- [ ] AC5：编辑器内执行的非 `commit` 写命令 `libra branch x` 以 stderr 含 `operation scope lease is already held` 的错误失败。
- [ ] AC6：外层持锁期间外部（非编辑器子进程）`libra commit` 以 stderr 含 `operation scope lease is already held` 的错误失败。
- [ ] AC7：编辑器退出后才启动的进程携带原令牌执行 `libra commit` 时以 stderr 含 `commit editor reentry token is not active` 的错误失败。
- [ ] AC8：认领与撤销按两种先后顺序受控交错时，均不存在「嵌套写入成功但无认领与身份登记」的结果。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_nested_in_editor_reentry_token)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_editor_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-05` 的 `T-2` 覆盖）
**Dependencies:** `CS-22a`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/internal/operation/middleware.rs`, `src/internal/operation/middleware/lease.rs`, `src/command/commit.rs`, `tests/command/commit_editor_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-05` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（编辑器内允许嵌套 `commit` 及其限制）、`COMPATIBILITY.md` commit 行（#784）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-05 (1/3)`
**C/D coverage from:** `REL-CS-05`
**Granularity:** `type=implementation; axis=编辑器嵌套commit重入令牌; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=3; prod-files=3; scope=M; deps=CS-22a,DEP-CS-07; writeset=序列化于 CS-22a; release=family child of REL-CS-05 (1/3); split-from=N/A; exception=N/A`

---

### Task CS-23a: 外层 `commit` 有界等待、超时终止与进程身份安全（#784）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-23` 拆出：按 `ADR-CS-04` 的竞态与进程身份合同，已认领嵌套 `commit` 仍在运行时外层在检查状态与释放 `ScopeLease` 前带界等待；超时后按登记的不可复用身份终止受控进程树并确认退出，期间持续持租；外层崩溃、PID 复用或终止失败时保持互斥。状态检查与 op-log 隔离由 `CS-23b` 承接。
**Out of scope:** 令牌签发与认领（由 `CS-23` 承接）；嵌套变更检测与 op-log 隔离（由 `CS-23b` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 外层在命令结束时录制 after 快照并释放租约，无嵌套等待 | `src/internal/operation/middleware.rs:411-416,714`; `src/internal/operation/middleware/lease.rs:83` |

**夹具（FX-BASE 之外的差异化条件）:** 嵌套 `commit` 可在写入前受控暂停；等待时限通过测试环境变量 `LIBRA_TEST_COMMIT_NESTED_WAIT_MS` 缩短（仅在测试哨兵 `LIBRA_TEST=1` 时读取，生产环境忽略）；以测试钩子模拟外层被 SIGKILL、登记 PID 被其它进程复用与终止调用失败。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：已认领嵌套 `commit` 暂停期间，外层在其退出前不返回。
- [ ] AC2：等待超时且嵌套进程仍存活时，外层在确认该受控进程树已退出之后才返回。
- [ ] AC3：AC2 的外层以 stderr 含 `timed out waiting for nested commit` 的错误失败。
- [ ] AC4：超时等待与终止期间外部并发 `libra commit` 以 stderr 含 `operation scope lease is already held` 的错误失败。
- [ ] AC5：外层被 SIGKILL 而嵌套写入暂停时，外部并发 `libra commit` 在嵌套进程退出前以 stderr 含 `operation scope lease is already held` 的错误失败。
- [ ] AC6：登记 PID 被其它进程复用时，该进程未收到终止信号。
- [ ] AC7：AC6 场景中外部并发 `libra commit` 以 stderr 含 `operation scope lease is already held` 的错误失败。
- [ ] AC8：终止调用失败时外部并发 `libra commit` 以 stderr 含 `operation scope lease is already held` 的错误失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_nested_editor_wait_and_termination)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_editor_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-05` 的 `T-2` 覆盖）
**Dependencies:** `CS-23`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-05`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/internal/operation/middleware.rs`, `src/internal/operation/middleware/lease.rs`, `src/command/commit.rs`, `tests/command/commit_editor_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-05` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（嵌套提交等待与超时语义）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-05 (2/3)`
**C/D coverage from:** `REL-CS-05`
**Granularity:** `type=implementation; axis=外层有界等待与超时终止; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=3; prod-files=3; scope=M; deps=CS-23,DEP-CS-07; writeset=序列化于 CS-23; release=family child of REL-CS-05 (2/3); split-from=CS-23; exception=N/A`

---

### Task CS-23b: 外层 `commit` 嵌套变更检测与 operation-log 隔离（#784）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-23a` 拆出：确认嵌套进程退出后，外层在租约保护内检测 `HEAD`/refs/index/worktree 的嵌套变更；有变更则外层以 `LBR-CONFLICT-002` 中止，operation-log 收尾不覆盖、不回滚嵌套提交，外层条目不吸收嵌套 ref 变化，收尾完成前持续持租。本卡的失败路径以「嵌套提交完成、外层编辑器返回前」的快照 S_n 为 V-6 基线。
**Out of scope:** 等待与终止（由 `CS-23a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 外层 before/after ref 快照录制点 | `src/internal/operation/middleware.rs:457,714` |

**夹具（FX-BASE 之外的差异化条件）:** 三个相互独立的场景，测试钩子在嵌套动作完成后、外层编辑器返回前取快照 S_n：①ref 场景——嵌套 `libra commit` 只推进 HEAD；②index 场景——编辑器脚本不调用任何 Libra 写命令，直接把预先准备的索引文件字节（仅暂存一个已跟踪文件的修改）复制到 `.libra/index`，模拟外部工具改动（HEAD 与 refs 不变，不涉及 `CS-23` 的令牌）；③worktree 场景——编辑器脚本直接改写一个已跟踪文件（HEAD、refs 与索引不变）；另有组合场景（嵌套提交并额外改写工作区与索引）用于 operation-log 与并发检查；收尾阶段可受控暂停以注入并发写命令。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：ref 场景中外层 `libra commit` 以 `LBR-CONFLICT-002` 失败。
- [ ] AC2：index 场景中外层 `libra commit` 以 `LBR-CONFLICT-002` 失败。
- [ ] AC3：worktree 场景中外层 `libra commit` 以 `LBR-CONFLICT-002` 失败。
- [ ] AC4：组合场景外层中止后的快照以 S_n 为基线 SNAP-EQ（⑦ 按统一口径，只允许外层 commit 自身已登记的 `running` operation 转为终态及其 journal 行；不得覆盖或回滚嵌套产生的 refs、索引与工作区变化）。
- [ ] AC5：组合场景外层中止后嵌套提交的 operation-log 条目仍存在。
- [ ] AC6：组合场景外层 operation-log 条目的 before/after 不含嵌套提交带来的 ref 变化。
- [ ] AC7：外层收尾受控暂停期间外部并发 `libra commit` 以 stderr 含 `operation scope lease is already held` 的错误失败。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_nested_editor_state_and_oplog_isolation)'`（V-2；new；覆盖 AC1–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_editor_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-05` 的 `T-2` 覆盖）
**Dependencies:** `CS-23a`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-05`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/internal/operation/middleware.rs`, `src/command/commit.rs`, `tests/command/commit_editor_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-05` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（嵌套提交后外层中止与 op-log 语义）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-05 (3/3)`
**C/D coverage from:** `REL-CS-05`
**Granularity:** `type=implementation; axis=嵌套变更检测与op-log隔离; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=CS-23a,DEP-CS-07; writeset=序列化于 CS-23a; release=family child of REL-CS-05 (3/3); split-from=CS-23a; exception=N/A`

---

### Task REL-CS-05: `REL-CS-05` 编辑器嵌套提交重入与恢复家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-23`（1/3）、`CS-23a`（2/3）、`CS-23b`（3/3） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-23`、`CS-23a`、`CS-23b` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 三张子卡须同版发布以保证令牌认领、等待/终止与状态隔离一致 | `ADR-CS-04`; `plan-template.md` G-08 |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-23`、`CS-23a`、`CS-23b` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `command::commit_editor_test::commit_nested_in_editor_reentry_token` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit_editor_test::commit_nested_editor_wait_and_termination` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::commit_editor_test::commit_nested_editor_state_and_oplog_isolation` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-05 begin -->` 与 `<!-- release-note-draft:REL-CS-05 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 3 行，顺序同家族登记：`CS-23`、`CS-23a`、`CS-23b`）。
- [ ] AC5：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_nested_in_editor_reentry_token)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_nested_editor_wait_and_termination)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_editor_test::commit_nested_editor_state_and_oplog_isolation)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 手工证据（覆盖 AC4）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-05 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-05 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-23 CS-23a CS-23b; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V5 手工证据（覆盖 AC5）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-23` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-23a` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-23b` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-23`, `CS-23a`, `CS-23b`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-23`、`CS-23a`、`CS-23b` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-05`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-05家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=5/12; VER=5/12; landing=0; prod-files=0; scope=S; deps=CS-23,CS-23a,CS-23b; writeset=序列化于 CS-23b; release=family release point of REL-CS-05; split-from=N/A; exception=N/A`

---

### Task CS-24: 实现 `GIT_TRACE2_EVENT` 最小事件子系统（#759 前置）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 在 `src/utils/trace2.rs`（新增，`src/utils/mod.rs` 注册）与 `src/cli.rs` 实现 `GIT_TRACE2_EVENT=<绝对路径>` 的 JSON 事件追加输出：主命令生命周期 `version`、`start`、`cmd_name`、`exit`，以及供受控子进程使用的 `child_start`（argv 首元素可设为 `"git"` 以兼容 Git `test_subcommand`）与 `child_exit`。未设置时零 I/O；目标不可写时不影响命令结果。已拆出 `CS-25`。
**Out of scope:** `commit` 自动维护接入（由 `CS-25a` 承接）；`GIT_TRACE2_PERF`/`GIT_TRACE2` 普通格式（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `src/` 无 `GIT_TRACE2` 实现（`rg -n GIT_TRACE2 src` 零命中，锚点定位用，非判据） | `src/cli.rs`; `src/utils/mod.rs` |
| Git `test_subcommand` 以 `["git",...]` 匹配 child_start argv | `t/test-lib-functions.sh:1906-1923` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** trace 目标位于本用例独占目录；子进程事件经 `trace2` API 在测试中派生 `true`/`false` 两个子进程。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：设置 `GIT_TRACE2_EVENT=<路径>` 时 `libra commit -m x` 后该文件每行均可解析为 JSON 对象。
- [ ] AC2：该文件各行按行序投影为 `event` 字段（`cmd_name` 行附其 `name`，记作 `cmd_name:<name>`）所得列表等于 `["version","start","cmd_name:commit","exit"]`（测试环境不派生自动维护，见 `CS-25b`）。
- [ ] AC3：经 `trace2` API 派生子进程时所写 `child_start` 事件的 `argv` 首元素为 `"git"`（该事件存在为执行条件）。
- [ ] AC4：与 `true` 子进程 `child_start` 同 `child_id` 的 `child_exit` 事件的 `code` 为 0（该事件存在是执行条件）。
- [ ] AC5：与 `false` 子进程 `child_start` 同 `child_id` 的 `child_exit` 事件的 `code` 为 1，即实际等待到的退出码（该事件存在是执行条件）。
- [ ] AC6：未设置 `GIT_TRACE2_EVENT` 时运行前后本用例独占目录内容不变。
- [ ] AC7：`GIT_TRACE2_EVENT` 指向不可写路径时 `libra commit -m y` 生成提交。
- [ ] AC8：设置 `GIT_TRACE2_EVENT=<路径>` 时 `libra status` 后该文件按同一投影所得列表等于 `["version","start","cmd_name:status","exit"]`（生命周期事件由 `src/cli.rs` 对所有命令统一写入，而非只在 `commit` 中写入）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::trace2_event_emission)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `T-1`：在 `src/cli.rs` 接入全局 trace2 生命周期事件
**Dependencies:** `REL-CS-05`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `src/utils/mod.rs`, `src/utils/trace2.rs`, `tests/command/commit_test.rs`, `CLAUDE.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md` 与 `docs/commands/zh-CN/commit.md` 的环境变量节（`GIT_TRACE2_EVENT` 事件文件与事件集合）、`docs/development/commands/commit.md`、`CLAUDE.md`「Environment Variables / Build & Runtime」、`COMPATIBILITY.md` commit 行（`COMPATIBILITY.md:209` 既有条目内追加 `GIT_TRACE2_EVENT` 事件输出说明，行数不变）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=GIT_TRACE2_EVENT事件子系统; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=3; scope=M; deps=REL-CS-05,DEP-CS-07; writeset=序列化于 REL-CS-05; release=independent; split-from=N/A; exception=N/A`

---

### Task CS-25: 实现 `libra maintenance run --auto` 与 `--no-quiet`，loose-objects 自动条件（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-24` 拆出：按 `ADR-CS-03` 为 `maintenance run` 增加 `--auto` 与 `--no-quiet`（`-q`/`--quiet` 已存在，`src/command/maintenance.rs:89-91`）：与 Git `maybe_run_task`（`builtin/gc.c`）同构，`--auto` 下只执行定义了自动条件且条件成立的任务，未定义条件或条件不成立的任务不执行，也不出现在 `--json` 输出的 `tasks[]` 中（`src/command/maintenance.rs:159-180`）。本卡定义 loose-objects 条件：loose 对象总数 ≥ `DEFAULT_LOOSE_OBJECT_THRESHOLD=100`（与 Git 默认阈值 100 及其 `>=` 比较一致，`builtin/gc.c:976-1003`；与现有 `src/command/maintenance.rs:1041` 判定一致）；Libra 固定该阈值、不读取 `maintenance.loose-objects.auto`（Git 可配置，0 关闭、负值恒成立），差异登记 `COMPATIBILITY.md`，执行时只处理超过两周的对象（`src/command/maintenance.rs:65,1067`，年龄保护不变）。`prefetch`、`pack-refs`、`commit-graph` 在 `--auto` 下不执行（Git 对后两者另有条件，Libra 不实现，差异登记 `COMPATIBILITY.md`）。`--[no-]detach` 由 `CS-25d` 承接（Git 默认不分离，`builtin/gc.c:239,1423`；本卡所有运行均在前台），incremental-repack 条件由 `CS-25c` 承接，gc 条件由 `CS-25h` 承接，取锁等待由 `CS-25f` 承接。`--[no-]quiet` 的输出语义由 `CS-25g` 验收。已拆出 `CS-25g`、`CS-25d`、`CS-25c`、`CS-25a`。
**Out of scope:** `--[no-]detach`（由 `CS-25d` 承接）；incremental-repack 任务条件（由 `CS-25c` 承接）；gc 任务条件（由 `CS-25h` 承接）；取锁带界等待（由 `CS-25f` 承接）；`commit` 派生自动维护（由 `CS-25a` 承接）；`pack-refs`/`commit-graph` 的自动条件与 `maintenance.<task>.auto` 配置（永久非目标，差异登记 `COMPATIBILITY.md`）；重写 gc 压缩算法（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `run` 无 `--auto`/`--no-quiet`；loose-objects 任务在 loose 数 `< 100` 时跳过 | `src/command/maintenance.rs:63-65,84-92,1041` |
| Git `--auto` 只运行有条件且条件成立的任务；loose 条件为 `>=` | `builtin/gc.c:976-1003` 与 `maybe_run_task` @ `6de20f6092` |
| `gc.auto` 解析与关闭语义 | `src/command/gc.rs:330-340` |

**夹具（FX-BASE 之外的差异化条件）:** 均以 `libra` 的对象写入 API 在新仓中构造，loose 对象总数按 `list_loose_objects` 口径计数（含初始化产生的对象）：边界下夹具总数 99（其中 49 个 mtime 回拨到两周 `LOOSE_OBJECT_AGE_SECONDS` 之前）；边界夹具总数恰为 100（50 个已老化）；超阈值夹具总数 120（60 个已老化、60 个为新对象）；三者 pack 数均低于 5。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：边界下夹具上 `libra maintenance run --auto --quiet` 前后 SNAP-EQ（含 ⑧：loose 文件集合与 pack/idx 集合不变）。
- [ ] AC2：边界夹具上 `libra maintenance run --auto` 结束后，50 个已老化对象均不再以 loose 形式存在。
- [ ] AC3：超阈值夹具上 `libra maintenance run --auto --no-quiet` 结束后，60 个已老化对象均不再以 loose 形式存在。
- [ ] AC4：同一运行后 60 个新对象仍以 loose 形式存在。
- [ ] AC5：同一运行前后 SNAP 分量 ⑤（对象 OID/类型/大小集合）完全相等。
- [ ] AC6：同一运行后每个已老化对象按原 OID 读出的内容与运行前逐字节一致。
- [ ] AC7：`maintenance.loose-objects.auto=0` 时超阈值夹具上 `libra maintenance run --auto` 结束后 60 个已老化对象均不再以 loose 形式存在（Libra 固定阈值、不读取该配置；Git 中该值为 0 会关闭任务）。
- [ ] AC8：超阈值夹具副本上 `libra maintenance run --auto --json` 的 `tasks[].task` 恰为 `["loose-objects"]`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::maintenance_test::maintenance_run_auto_loose_objects_condition)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::maintenance_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::gc_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-24`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/maintenance.rs`, `tests/command/maintenance_test.rs`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/maintenance.md`、`docs/commands/zh-CN/maintenance.md`、`docs/development/commands/maintenance.md`（`run --auto`、`--no-quiet`、loose-objects 条件与 `--auto` 下不执行的任务）、`COMPATIBILITY.md` maintenance 行、网站 `maintenance.en.md`（W-1..W-6，`--only maintenance`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=maintenance run --auto与loose条件; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-24,DEP-CS-07; writeset=序列化于 CS-24; release=independent; split-from=CS-24; exception=N/A`

---

### Task CS-25g: `maintenance run --[no-]quiet` 的输出语义（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-25` 拆出：`maintenance run` 的 `-q/--quiet` 与新增的 `--no-quiet` 控制进度输出（`src/command/maintenance.rs:236-238` 的 `Running maintenance task: <task>` 行，经 `info_println` 写 stdout），两者同时出现时最后一个生效（Git `OPT_BOOL` 语义）。
**Out of scope:** 任务条件（由 `CS-25`、`CS-25c`、`CS-25h` 承接）；`--[no-]detach`（由 `CS-25d` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 进度行仅在非 quiet 时输出到 stdout | `src/command/maintenance.rs:236-238,4609-4613` |

**夹具（FX-BASE 之外的差异化条件）:** loose 对象总数 120（60 个已老化）、pack 数低于 5 的仓库，每条在独立副本上运行。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`libra maintenance run --auto --quiet` 的 stdout 不含 `Running maintenance task:`（命令退出码 0 且 60 个已老化对象均不再以 loose 形式存在为执行条件，即维护确已执行）。
- [ ] AC2：`libra maintenance run --auto --no-quiet` 的 stdout 含 `Running maintenance task: loose-objects`。
- [ ] AC3：`libra maintenance run --auto --quiet --no-quiet` 的 stdout 含 `Running maintenance task: loose-objects`（最后一个生效）。
- [ ] AC4：`libra maintenance run --auto --no-quiet --quiet` 的 stdout 不含 `Running maintenance task:`（最后一个生效；命令退出码 0 且 60 个已老化对象均不再以 loose 形式存在为执行条件）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::maintenance_test::maintenance_run_quiet_flags)'`（V-2；new；覆盖 AC1–AC4）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::maintenance_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-25`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/maintenance.rs`, `tests/command/maintenance_test.rs`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/maintenance.md`、`docs/commands/zh-CN/maintenance.md`、`docs/development/commands/maintenance.md`（`--[no-]quiet`）、`COMPATIBILITY.md` maintenance 行、网站 `maintenance.en.md`（W-1..W-6，`--only maintenance`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=maintenance--[no-]quiet输出; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-25,DEP-CS-07; writeset=序列化于 CS-25; release=independent; split-from=CS-25; exception=N/A`

---

### Task CS-25d: 实现 `libra maintenance run --[no-]detach` 分离语义（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-25` 拆出：为 `maintenance run` 增加 `--detach`/`--no-detach`（最后一个生效，默认不分离，与 Git `builtin/gc.c:239,1423` 一致）。`--detach` 与是否带 `--auto` 无关（Git `builtin/gc.c:1423` 只看 `opts->detach`）：带 `--auto` 时在条件判定通过后、不带时直接派生分离的子进程执行所选维护任务（含取 `MaintenanceLock::exclusive`），父命令在维护工作完成前返回、退出码 0；分离进程完成后的 loose 对象集合与独立副本上前台运行的结果相同、逻辑对象集合不变（其余状态与前台运行的比较不在本卡范围）。测试钩子 `LIBRA_TEST_MAINTENANCE_PAUSE_FILE=<p>` 只在测试哨兵 `LIBRA_TEST=1` 时读取（生产环境忽略）：维护进程（分离或前台）在开始打包前把自身 PID 写入 `<p>.pid`，并等待文件 `<p>` 被删除。
**Out of scope:** 任务条件（由 `CS-25`、`CS-25c`、`CS-25h` 承接）；`commit` 派生（由 `CS-25a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `maintenance run` 当前同步执行且无 `--detach` | `src/command/maintenance.rs:84-92` |
| Git 默认不分离，仅 `--detach` 时 `daemonize` | `builtin/gc.c:239,1423` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 超阈值夹具：新仓中 loose 对象总数 120（60 个 mtime 回拨到两周之前、60 个为新对象），pack 数低于 5；`LIBRA_TEST=1` 且 `LIBRA_TEST_MAINTENANCE_PAUSE_FILE=<p>` 时，分离进程启动后先把自身 PID 写入 `<p>.pid`，再在开始打包前等待文件 `<p>` 被删除；测试据 `<p>.pid` 等待分离进程退出。每条比较在同一夹具的独立副本上进行。每个以 `--detach` 生效的场景都以「命令退出码 0 且 `<p>.pid` 已出现」作为分离子进程已启动的执行条件。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：文件 `<p>` 存在时，`libra maintenance run --auto --quiet --detach` 返回时 60 个已老化对象仍全部以 loose 形式存在（退出码 0 与 `<p>.pid` 出现为执行条件）。
- [ ] AC2：随后删除 `<p>` 并等待分离进程退出后，loose 对象集合与独立副本上 `libra maintenance run --auto --no-detach` 运行后的集合相同。
- [ ] AC3：同一时刻 SNAP 分量 ⑤ 与运行前完全相等。
- [ ] AC4：同一时刻每个已老化对象按原 OID 读出的内容与运行前逐字节一致。
- [ ] AC5：`libra maintenance run --auto --detach --no-detach` 返回时 60 个已老化对象均已不再以 loose 形式存在（最后一个生效，前台执行）。
- [ ] AC6：文件 `<p>` 存在时，`libra maintenance run --auto --no-detach --detach` 返回时 60 个已老化对象仍全部以 loose 形式存在（最后一个生效，分离执行；退出码 0 与 `<p>.pid` 出现为执行条件）。
- [ ] AC7：文件 `<p>` 存在时，不带 `--auto` 的 `libra maintenance run --task loose-objects --detach` 返回时 60 个已老化对象仍全部以 loose 形式存在（退出码 0 与 `<p>.pid` 出现为执行条件）。
- [ ] AC8：随后删除 `<p>` 并等待该分离进程退出后，60 个已老化对象均不再以 loose 形式存在。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::maintenance_test::maintenance_run_auto_detach_semantics)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::maintenance_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-25g`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/maintenance.rs`, `tests/command/maintenance_test.rs`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/maintenance.md`、`docs/commands/zh-CN/maintenance.md`、`docs/development/commands/maintenance.md`（`--[no-]detach` 语义与默认值）、`COMPATIBILITY.md` maintenance 行、网站 `maintenance.en.md`（W-1..W-6，`--only maintenance`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=maintenance--[no-]detach分离语义; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-25g,DEP-CS-07; writeset=序列化于 CS-25g; release=independent; split-from=CS-25; exception=N/A`

---

### Task CS-25c: `maintenance run --auto` 的 incremental-repack 任务条件（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-25` 拆出：`--auto` 下 incremental-repack 任务仅在 pack 数 ≥ `DEFAULT_PACK_COUNT_THRESHOLD=5` 时执行（与现有 `src/command/maintenance.rs:1447` 判定一致；Git 默认阈值 10 且要求 multi-pack-index，差异登记 `COMPATIBILITY.md`）；条件不成立时不执行，也不出现在 `--json` 的 `tasks[]` 中。执行了哪些任务以 `libra maintenance run --auto --json` 输出的 `tasks[].task` 观测（`src/command/maintenance.rs:159-180`）。gc 任务条件由 `CS-25h` 承接，取锁带界等待由 `CS-25f` 承接。已拆出 `CS-25h`、`CS-25f`。
**Out of scope:** loose-objects 条件（由 `CS-25` 承接）；`--[no-]detach`（由 `CS-25d` 承接）；gc 任务条件（由 `CS-25h` 承接）；取锁带界等待（由 `CS-25f` 承接）；`commit` 派生（由 `CS-25a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| pack 数阈值常量与 `--json` 任务结果结构 | `src/command/maintenance.rs:64,159-180,1447` |
| Git incremental-repack 条件（阈值 10、需 multi-pack-index） | `builtin/gc.c` `incremental_repack_auto_condition` @ `6de20f6092` |
| `gc.auto` 解析与关闭语义 | `src/command/gc.rs:330-340` |

**夹具（FX-BASE 之外的差异化条件）:** pack 夹具分别含 5 个与 4 个 pack，loose 对象总数低于 100，`gc.auto` 未设置（默认 6700，gc 条件不成立）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：5 个 pack 时 `libra maintenance run --auto` 结束后 pack 数少于 5。
- [ ] AC2：同一运行前后 SNAP 分量 ⑤ 完全相等（对象未丢失）。
- [ ] AC3：4 个 pack 时 `libra maintenance run --auto --json` 的 `tasks[].task` 不含 `incremental-repack`。
- [ ] AC4：同一 4 个 pack 的运行前后 SNAP-EQ（含 ⑧：pack/idx 文件名与哈希不变，即未重写 pack）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::maintenance_test::maintenance_run_auto_incremental_repack_condition)'`（V-2；new；覆盖 AC1–AC4）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::maintenance_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::gc_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-25d`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/maintenance.rs`, `tests/command/maintenance_test.rs`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/maintenance.md`、`docs/commands/zh-CN/maintenance.md`、`docs/development/commands/maintenance.md`（`--auto` 下 incremental-repack 条件）、`COMPATIBILITY.md` maintenance 行、网站 `maintenance.en.md`（W-1..W-6，`--only maintenance`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=maintenance--auto增量重打包条件; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-25d,DEP-CS-07; writeset=序列化于 CS-25d; release=independent; split-from=CS-25; exception=N/A`

---

### Task CS-25h: `maintenance run --auto` 的 gc 任务条件（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-25c` 拆出：`--auto` 下 gc 任务沿用 `gc --auto` 的 `gc.auto` 条件（`src/command/gc.rs:155-164,330-340`：loose 对象数不低于正阈值时执行，`<=0` 时只关闭 gc 任务）；条件不成立时 gc 不执行，也不出现在 `--json` 的 `tasks[]` 中（`src/command/maintenance.rs:159-180`）。
**Out of scope:** incremental-repack 条件（由 `CS-25c` 承接）；loose-objects 条件（由 `CS-25` 承接）；取锁带界等待（由 `CS-25f` 承接）；隔离账本机制本身（不改动）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `gc --auto` 以 loose 对象数与 `gc.auto` 比较，非正阈值不执行 | `src/command/gc.rs:155-164,330-340` |
| maintenance gc 任务按隔离账本只删除超过宽限的不可达对象 | `src/command/maintenance.rs:592-640,733-792` |

**夹具（FX-BASE 之外的差异化条件）:** 新仓、无 pack，loose 对象总数（按 `list_loose_objects` 口径，含初始化产生的对象）少于 10（loose-objects 与 incremental-repack 条件均不成立），其中 2 个为以对象写入 API 写入、不可达且 mtime 回拨到两周之前的 loose 对象；被测运行前在 `.libra/gc-prune-candidates.json` 为这两个对象写入早于当前时间 2 小时的 `first_seen`（模拟已完成首次隔离登记且超过一小时宽限，`src/command/maintenance.rs:733-792`），因此 gc 一旦执行必然删除二者。三个独立副本分别设 `gc.auto=1`、`gc.auto=10`、`gc.auto=0`，账本相同。 另有一个 loose 对象总数 120（60 个 mtime 回拨到两周之前）、无 pack、`gc.auto=0` 的超阈值副本（AC7）。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`gc.auto=1` 时 `libra maintenance run --auto` 结束后两个不可达 loose 对象均不复存在。
- [ ] AC2：同一运行前后 SNAP-Δ 只含 ⑤ 中这两个对象的消失。
- [ ] AC3：`gc.auto=10` 时 `libra maintenance run --auto --json` 的 `tasks[].task` 不含 `gc`（loose 对象数低于正阈值）。
- [ ] AC4：同一 `gc.auto=10` 运行前后 SNAP-EQ。
- [ ] AC5：`gc.auto=0` 时 `libra maintenance run --auto --json` 的 `tasks[].task` 不含 `gc`。
- [ ] AC6：同一 `gc.auto=0` 运行前后 SNAP-EQ。
- [ ] AC7：`gc.auto=0` 时超阈值副本上 `libra maintenance run --auto` 结束后 60 个已老化对象均不再以 loose 形式存在（`gc.auto` 不关闭 loose-objects 任务）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::maintenance_test::maintenance_run_auto_gc_condition)'`（V-2；new；覆盖 AC1–AC7）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::maintenance_test::/)'`（V-3 共享回归组）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::gc_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-25c`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/maintenance.rs`, `tests/command/maintenance_test.rs`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/maintenance.md`、`docs/commands/zh-CN/maintenance.md`、`docs/development/commands/maintenance.md`（`--auto` 下 gc 条件与 `gc.auto<=0` 只关闭 gc）、`COMPATIBILITY.md` maintenance 行、网站 `maintenance.en.md`（W-1..W-6，`--only maintenance`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=maintenance--auto的gc条件; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-25c,DEP-CS-07; writeset=序列化于 CS-25c; release=independent; split-from=CS-25c; exception=N/A`

---

### Task CS-25f: `maintenance run --auto` 取维护锁的带界等待（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-25c` 拆出（gc 条件另拆为 `CS-25h`）：`--auto` 运行取 `MaintenanceLock::exclusive` 时带界等待（总时长 ≤ 30 秒、退避重试）：上界内锁被释放则继续执行维护；超时则以退出码 0 退出且不执行任何维护任务（超时时的输出内容不作约束）。测试钩子 `LIBRA_TEST_MAINTENANCE_LOCK_WAIT_MS`（缩短上界）与 `LIBRA_TEST_MAINTENANCE_LOCK_WAIT_MARK=<f>`（首次取锁失败、进入等待时创建空文件 `<f>`）只在测试哨兵 `LIBRA_TEST=1` 时读取，生产环境忽略。
**Out of scope:** 任务条件（由 `CS-25`、`CS-25c`、`CS-25h` 承接）；`commit` 派生（由 `CS-25a` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 维护任务依赖锁保护发布与删除阶段 | `src/command/maintenance.rs:240,1030-1037` |
| `ADR-CS-03` 要求子进程取锁带界等待、超时静默退出 | `ADR-CS-03` |

**夹具（FX-BASE 之外的差异化条件）:** loose 对象总数 120（60 个 mtime 回拨到两周之前）、pack 数低于 5 的仓库；测试持有 `MaintenanceLock::shared` 制造竞争；`LIBRA_TEST=1`、`LIBRA_TEST_MAINTENANCE_LOCK_WAIT_MS` 只在超时场景设为 2000，释放场景与默认上界场景都不设置（使用生产默认上界 30 秒，因而释放场景验证的是生产默认配置下的等待与继续执行）、`LIBRA_TEST_MAINTENANCE_LOCK_WAIT_MARK=<本用例独占目录>/mark`；被测命令以子进程运行。释放场景在 `mark` 出现后继续持锁 5 秒再释放（明显长于超时场景的 2000 ms 覆盖值，过早放弃的生产默认实现会在释放前退出）；另有默认上界场景：持锁始终不释放。超时、释放与默认上界场景各用独立副本。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：超时场景（锁始终不释放）中 `libra maintenance run --auto --no-detach` 的墙钟耗时 ≥ 2000 ms。
- [ ] AC2：超时场景的墙钟耗时 ≤ 4000 ms。
- [ ] AC3：超时场景的退出码为 0。
- [ ] AC4：超时场景前后 SNAP-EQ。
- [ ] AC5：释放场景中 `mark` 出现 5 秒后、测试释放锁之前，`libra maintenance run --auto --no-detach` 子进程尚未退出。
- [ ] AC6：释放场景中同一时刻（释放锁之前）取得的快照与运行前 SNAP-EQ。
- [ ] AC7：释放场景中测试释放锁并等待子进程退出后，60 个已老化对象均不再以 loose 形式存在。
- [ ] AC8：默认上界场景中 `libra maintenance run --auto --no-detach` 的墙钟耗时 ≤ 35000 ms（生产默认上界 30 秒加调度余量；以退出码 0 结束为执行条件）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::maintenance_test::maintenance_run_auto_lock_wait)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::maintenance_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** `none`（执行阶段只跑本卡 A 组 focused 与 Verification）
**Dependencies:** `CS-25h`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/maintenance.rs`, `tests/command/maintenance_test.rs`, `docs/commands/maintenance.md`, `docs/commands/zh-CN/maintenance.md`, `docs/development/commands/maintenance.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/maintenance.en.md`
**Release write set:** `Inherited`
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/maintenance.md`、`docs/commands/zh-CN/maintenance.md`、`docs/development/commands/maintenance.md`（`--auto` 取锁等待上界与超时以退出码 0 退出、不执行维护）、`COMPATIBILITY.md` maintenance 行、网站 `maintenance.en.md`（W-1..W-6，`--only maintenance`）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `M`
**Version increment:** `patch`
**Release boundary:** `independent`
**C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=maintenance--auto取锁带界等待; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-25h,DEP-CS-07; writeset=序列化于 CS-25h; release=independent; split-from=CS-25c; exception=N/A`

---

### Task CS-25a: 在 `libra commit` 接入自动维护派生（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-25` 拆出：按 `ADR-CS-03`，`commit` 成功写入提交后登记自动维护请求，`src/cli.rs` 在 `_maintenance_lock`（`src/cli.rs:3768-3770`）作用域结束后，若启用条件成立（`maintenance.auto` 为真；未设置时 `gc.auto` 未设置或 `> 0`，Git `run-command.c:1961-1966`）则派生 `maintenance run --auto (--quiet|--no-quiet) (--detach|--no-detach)` 并等待其返回：`commit --quiet` 时传 `--quiet`（未传 `--quiet` 时补 `--no-quiet` 由同家族 `CS-25e` 实现）；`maintenance.autoDetach` 为假时传 `--no-detach`（AC5 以超阈值仓库在 commit 返回时已完成打包间接验证），为真或未设置时传 `--detach`（未设置时回退 `gc.autoDetach` 由同家族 `CS-25e` 承接，Git `run-command.c:1974-1982`）；围绕派生写 `CS-24` 的 `child_start`/`child_exit`。测试与 CI 隔离由同家族 `CS-25b` 承接，四卡经 `REL-CS-06` 同版发布，避免先启用后隔离。与 Git `builtin/commit.c:1965`（不检查 `run_auto_maintenance` 返回值）一致，维护子进程执行失败不影响已写入提交的命令结果；默认分离时 commit 不等待维护工作的时序与子进程无法派生时的不传播由同家族 `CS-25i` 验收。测试钩子 `LIBRA_TEST_MAINTENANCE_FORCE_FAIL=1` 只在 `LIBRA_TEST=1` 时读取，令 `maintenance run --auto` 以非零退出。
**Out of scope:** `maintenance run --auto` 自身（由 `CS-25`、`CS-25d`、`CS-25c`、`CS-25h`、`CS-25f` 承接）；`--no-quiet` 与 `gc.autoDetach` 回退（由 `CS-25e` 承接）；`LIBRA_NO_AUTO_MAINTENANCE` 与测试 harness 隔离（由 `CS-25b` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 提交成功后不调用自动维护 | `src/command/commit.rs:3043-3046` |
| CLI 分发期间持有共享维护锁 | `src/cli.rs:2520,3768-3770` |
| Git 启用条件与参数；t7900 期望 | `run-command.c:1956-1993`; `t/t7900-maintenance.sh:66-145` @ `6de20f6092` |
| Git 提交后不检查自动维护返回值 | `builtin/commit.c:1952-1973` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 用例经 `libra` 二进制子进程运行（移除继承的 `LIBRA_NO_AUTO_MAINTENANCE` 与 `NEXTEST_RUN_ID`），`GIT_TRACE2_EVENT` 指向本用例独占的临时文件；配置组合逐一对应 t7900 用例；AC5–AC6 另用 loose 对象总数 120（60 个 mtime 回拨到两周之前）的超阈值仓库并设 `LIBRA_TEST=1`、`LIBRA_TEST_MAINTENANCE_PAUSE_FILE=<p>`（`CS-25d` 交付的钩子：维护进程在开始打包前写 `<p>.pid` 并等待 `<p>` 被删除）；失败注入场景设 `maintenance.autoDetach=false`、`LIBRA_TEST=1` 与 `LIBRA_TEST_MAINTENANCE_FORCE_FAIL=1`。时序断言完成之后才做清理阶段的进程等待。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：默认配置下 `libra commit --quiet --allow-empty -m 1` 的 trace 含 argv 为 `["git","maintenance","run","--auto","--quiet","--detach"]` 的 `child_start`。
- [ ] AC2：`maintenance.auto=false` 时 trace 不含 `maintenance` 的 `child_start`。
- [ ] AC3：`maintenance.auto` 未设置且 `gc.auto=0` 时 trace 不含 `maintenance` 的 `child_start`。
- [ ] AC4：`maintenance.auto=true` 且 `gc.auto=0` 时 trace 含 AC1 的 `child_start`。
- [ ] AC5：超阈值仓库上 `maintenance.autoDetach=false` 且 `<p>` 存在时，`<p>.pid` 出现后 `libra commit --allow-empty -m 2` 子进程尚未退出（commit 同步等待前台维护）。
- [ ] AC6：随后删除 `<p>` 并等待 commit 退出后，60 个已老化对象均不再以 loose 形式存在（commit 退出码 0 为执行条件）。
- [ ] AC7：失败注入场景中，`libra commit --allow-empty -m 4` 的 trace 里与维护 `child_start` 同 `child_id` 的 `child_exit` 事件的 `code` 非零（失败确已发生；该事件存在是执行条件）。
- [ ] AC8：同一失败注入场景中 `libra commit --allow-empty -m 4` 的退出码为 0（维护失败不传播；HEAD 已指向新提交为执行条件）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_trace2)'`（V-2；new；覆盖 AC1–AC8）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-06` 的 `T-2` 覆盖）
**Dependencies:** `CS-25f`, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/cli.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-06` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（提交后自动维护的启用条件、参数与关闭方式）、`COMPATIBILITY.md` commit 行（对齐 t7900；派生在共享维护锁释放后进行）、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-06 (1/4)`
**C/D coverage from:** `REL-CS-06`
**Granularity:** `type=implementation; axis=commit后自动维护派生; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=2; scope=M; deps=CS-25f,DEP-CS-07; writeset=序列化于 CS-25f; release=family child of REL-CS-06 (1/4); split-from=CS-25; exception=N/A`

---

### Task CS-25i: commit 自动维护的默认分离时序与派生失败不传播（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-25a` 拆出：按 `ADR-CS-03` 与 Git `run-command.c:1974-1982`，`maintenance.autoDetach` 与 `gc.autoDetach` 均未设置时 commit 派生 `maintenance run --auto --detach`，该子进程在条件判定后自行分离（`CS-25d`），commit 只等待该子进程返回、不等待分离出的维护工作；与 Git `builtin/commit.c:1965`（不检查 `run_auto_maintenance` 返回值）一致，维护子进程无法派生（如程序路径不存在）时 commit 仍以成功退出、已写入的提交保留。测试钩子 `LIBRA_TEST_MAINTENANCE_SPAWN_PROGRAM=<路径>` 只在测试哨兵 `LIBRA_TEST=1` 时读取（生产环境忽略），令派生改用该路径作为程序。
**Out of scope:** 启用条件、参数映射与前台等待（由 `CS-25a` 承接）；`gc.autoDetach` 回退与 `--no-quiet`（由 `CS-25e` 承接）；测试与 CI 隔离（由 `CS-25b` 承接）；`maintenance run --detach` 自身的分离实现（由 `CS-25d` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git commit 不检查自动维护的返回值 | `builtin/commit.c:1965` @ `6de20f6092` |
| `maintenance.autoDetach` 与 `gc.autoDetach` 均未设置时传 `--detach` | `run-command.c:1974-1982` @ `6de20f6092` |
| 派生失败时 Git 先写 `child_start` 再写 `code` 为 `-1` 的 `child_exit` | `run-command.c:740,965-967` @ `6de20f6092` |

**夹具（FX-BASE 之外的差异化条件）:** 用例经 `libra` 二进制子进程运行（移除继承的 `LIBRA_NO_AUTO_MAINTENANCE` 与 `NEXTEST_RUN_ID`）并设 `LIBRA_TEST=1`；分离场景用 loose 对象总数 120（60 个 mtime 回拨到两周之前）的超阈值仓库，`maintenance.autoDetach` 与 `gc.autoDetach` 均未设置，`LIBRA_TEST_MAINTENANCE_PAUSE_FILE=<p>`（`CS-25d` 交付的钩子：维护进程在开始打包前写 `<p>.pid` 并等待 `<p>` 被删除）；派生失败场景设 `LIBRA_TEST_MAINTENANCE_SPAWN_PROGRAM=<本用例独占目录内保证不存在的路径>`，`GIT_TRACE2_EVENT` 指向本用例独占的临时文件。各等待上限均为 30 秒，超时即用例失败；清理阶段删除 `<p>` 并等待维护进程退出。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：分离场景中 `<p>` 存在时，`libra commit --allow-empty -m d` 在测试不删除 `<p>` 的情况下以退出码 0 返回（commit 不等待被暂停的维护工作）。
- [ ] AC2：commit 返回后 `<p>.pid` 出现时，60 个已老化对象仍全部以 loose 形式存在（`<p>.pid` 出现为执行条件，证明维护已派生并处于暂停）。
- [ ] AC3：随后删除 `<p>` 并等待 `<p>.pid` 所记进程退出后，60 个已老化对象均不再以 loose 形式存在。
- [ ] AC4：派生失败场景中 `libra commit --allow-empty -m s` 的退出码为 0（维护派生失败不传播；HEAD 已指向本次生成的新提交为执行条件）。
- [ ] AC5：派生失败场景的 trace 含 `maintenance` 的 `child_start`（证明确实尝试派生）。
- [ ] AC6：与该 `child_start` 同 `child_id` 的 `child_exit` 事件的 `code` 为 `-1`（与 Git `run-command.c:740,965-967` 一致；该事件存在为执行条件）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_detach_and_spawn_failure)'`（V-2；new；覆盖 AC1–AC6）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-06` 的 `T-2` 覆盖）
**Dependencies:** `CS-25a`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-06`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/cli.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-06` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（默认分离时 commit 不等待维护工作、维护派生失败不影响提交结果）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-06 (2/4)`
**C/D coverage from:** `REL-CS-06`
**Granularity:** `type=implementation; axis=commit自动维护分离时序与派生失败; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=1; prod-files=1; scope=M; deps=CS-25a,DEP-CS-07; writeset=序列化于 CS-25a; release=family child of REL-CS-06 (2/4); split-from=CS-25a; exception=N/A`

---

### Task CS-25e: commit 自动维护的参数映射：`--no-quiet` 与 `gc.autoDetach` 回退（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-25a` 拆出：按 Git `run-command.c:1974-1982`，派生参数在 `commit` 未传 `--quiet` 时为 `--no-quiet`；`maintenance.autoDetach` 未设置时以 `gc.autoDetach` 决定 `--detach`/`--no-detach`，二者都未设置时为 `--detach`；`maintenance.autoDetach` 已设置时优先于 `gc.autoDetach`。
**Out of scope:** 启用条件、`--[no-]quiet` 与 `maintenance.autoDetach`（由 `CS-25a` 承接）；默认分离时序与派生失败（由 `CS-25i` 承接）；测试隔离（由 `CS-25b` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git 回退顺序 | `run-command.c:1974-1982` @ `6de20f6092`; `t/t7900-maintenance.sh:123-145` |

**夹具（FX-BASE 之外的差异化条件）:** 用例经 `libra` 二进制子进程运行（移除继承的 `LIBRA_NO_AUTO_MAINTENANCE` 与 `NEXTEST_RUN_ID`），`GIT_TRACE2_EVENT` 指向本用例独占的临时文件，断言前等待所有派生的维护进程退出。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：`maintenance.autoDetach` 未设置且 `gc.autoDetach=false` 时 `libra commit --quiet --allow-empty -m a` 的 `child_start` argv 末项为 `--no-detach`。
- [ ] AC2：`maintenance.autoDetach=false` 且 `gc.autoDetach=true` 时 argv 末项为 `--no-detach`。
- [ ] AC3：`maintenance.autoDetach=true` 且 `gc.autoDetach=false` 时 argv 末项为 `--detach`。
- [ ] AC4：未传 `--quiet` 的 `libra commit --allow-empty -m b` 的 `child_start` argv 含 `--no-quiet`。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_autodetach_fallback)'`（V-2；new；覆盖 AC1–AC4）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-06` 的 `T-2` 覆盖）
**Dependencies:** `CS-25i`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-06`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `src/cli.rs`, `tests/command/commit_test.rs`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-06` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`、`docs/development/commands/commit.md`（`maintenance.autoDetach` 与 `gc.autoDetach` 回退）、`COMPATIBILITY.md` commit 行、网站 `commit.en.md`（W-1..W-6，`--only commit`）。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-06 (3/4)`
**C/D coverage from:** `REL-CS-06`
**Granularity:** `type=implementation; axis=自动维护参数映射; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=1; prod-files=2; scope=M; deps=CS-25i,DEP-CS-07; writeset=序列化于 CS-25i; release=family child of REL-CS-06 (3/4); split-from=CS-25a; exception=N/A`

---

### Task CS-25b: `commit` 自动维护的测试与 CI 隔离（#759）

**Task type:** `implementation`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 自 `CS-25a` 拆出：`commit` 在以下任一条件下不登记自动维护请求：`LIBRA_NO_AUTO_MAINTENANCE=1`；进程环境含 nextest 设置的 `NEXTEST_RUN_ID`（覆盖 nextest 下任何直接或经 harness 派生的 `libra` 子进程，包括 `tests/command/commit_editor_test.rs:13`、`tests/command/commit_json_test.rs:21` 一类直接启动器）；预览模式（`--dry-run`/`--porcelain`/`--short`/`--long`，`src/command/commit.rs:3019` 的 `preview`）。公共 harness 另对其派生的子进程注入 `LIBRA_NO_AUTO_MAINTENANCE=1`，覆盖诊断用的 `cargo test`。专项用例（`CS-25a`）显式移除这两个变量。
**Out of scope:** 派生条件与参数（由 `CS-25a`、`CS-25e` 承接）；派生时序与派生失败（由 `CS-25i` 承接）；`.github/workflows/**` 改动（不需要：CI 全量门以 nextest 运行）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 直接启动 `libra` 二进制的测试入口 | `tests/command/commit_editor_test.rs:13`; `tests/command/commit_json_test.rs:21` |
| 预览判定 | `src/command/commit.rs:3019` |

**夹具（FX-BASE 之外的差异化条件）:** AC1–AC7 以直接派生的 `libra` 子进程运行并设置 `GIT_TRACE2_EVENT`（指向用例 tempdir 内的文件），除被测变量外移除 `LIBRA_NO_AUTO_MAINTENANCE` 与 `NEXTEST_RUN_ID`；AC8 经公共 harness 启动，只移除 `NEXTEST_RUN_ID`。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；文档门不计数）
- [ ] AC1：仅设置 `LIBRA_NO_AUTO_MAINTENANCE=1` 时 `libra commit -m x` 的 trace 不含 `maintenance` 的 `child_start`。
- [ ] AC2：仅设置 `NEXTEST_RUN_ID` 时 `libra commit -m y` 的 trace 不含 `maintenance` 的 `child_start`。
- [ ] AC3：两者均未设置时 `libra commit -m z` 的 trace 含 `maintenance` 的 `child_start`（对照）。
- [ ] AC4：两者均未设置时 `libra commit --dry-run` 的 trace 不含 `maintenance` 的 `child_start`。
- [ ] AC5：两者均未设置时 `libra commit --porcelain` 的 trace 不含 `maintenance` 的 `child_start`。
- [ ] AC6：两者均未设置时 `libra commit --short` 的 trace 不含 `maintenance` 的 `child_start`。
- [ ] AC7：两者均未设置时 `libra commit --long` 的 trace 不含 `maintenance` 的 `child_start`。
- [ ] AC8：经公共 harness（`tests/command/mod.rs` 的 `base_libra_command`）启动、且显式移除 `NEXTEST_RUN_ID`（模拟诊断用 `cargo test`）、调用方未设置 `LIBRA_NO_AUTO_MAINTENANCE` 的 `libra commit -m w` 的 trace 不含 `maintenance` 的 `child_start`（由 harness 注入的变量抑制）。
- [ ] 文档门（ER-06/ER-06a，不计入 G-03）：按 D-1 逐文件交付并验收 `Docs and compatibility impact` 所列仓库内文件；所列网站文件按 W-1..W-6 及其时序交付。

**Verification:**（卡特有判据，计入 G-03；按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_isolation)'`（V-2；new；覆盖 AC1–AC7）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_isolation_harness_without_nextest)'`（V-2；new；覆盖 AC8；trace 文件位于用例 tempdir，属测试夹具，随 tempdir 删除）

**A 组 focused 门（ER-04 A 组强制门，不计入 G-03）:**
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::commit_test::/)'`（V-3 共享回归组）

**Full-suite trigger:** N/A（`family child`；C 组全量由 `REL-CS-06` 的 `T-2` 覆盖）
**Dependencies:** `CS-25e`（A/B 通过、ER-05 PASS 与本地签名提交后即可开工，不等待 `REL-CS-06`）, `DEP-CS-07`
**Deliverables:** `N/A`
**Implementation write set:** `src/command/commit.rs`, `tests/command/mod.rs`, `tests/helpers/`, `tests/command/commit_test.rs`, `.env.test.example`, `CLAUDE.md`, `docs/commands/commit.md`, `docs/commands/zh-CN/commit.md`, `docs/development/commands/commit.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`
**Release write set:** `N/A`（`family child` 不 bump、不推送，由 `REL-CS-06` 承载）
**Files likely touched:** 同 `Implementation write set`（估计值，并发判定以写集为准）
**Docs and compatibility impact:** `docs/commands/commit.md`、`docs/commands/zh-CN/commit.md`（`LIBRA_NO_AUTO_MAINTENANCE=1` 关闭提交后自动维护、nextest 下自动关闭）、`docs/development/commands/commit.md`（测试隔离约定）、`CLAUDE.md`（Environment Variables）、`.env.test.example`（`LIBRA_NO_AUTO_MAINTENANCE=1`）、`COMPATIBILITY.md` commit 行（Libra-only 开关，`ADR-CS-03`）、网站 `commit.en.md`（W-1..W-6，`--only commit`）；按 D-1 逐文件验收。
**Rollback mode:** `revert`（推送前回退本地签名提交；家族已发布后按 `immutable-release` 前滚）
**Estimated scope:** `M`
**Version increment:** `N/A`
**Release boundary:** `family child of REL-CS-06 (4/4)`
**C/D coverage from:** `REL-CS-06`
**Granularity:** `type=implementation; axis=自动维护测试与CI隔离; recovery=推送前revert本地提交; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=CS-25e,DEP-CS-07; writeset=序列化于 CS-25e; release=family child of REL-CS-06 (4/4); split-from=CS-25a; exception=N/A`

---

### Task REL-CS-06: `REL-CS-06` commit 自动维护派生与隔离家族发布点

**Task type:** `release`
**Lifecycle / Acceptance:** `pending` / `空`
**Description:** 按 G-08 作为 `CS-25a`（1/4）、`CS-25i`（2/4）、`CS-25e`（3/4）、`CS-25b`（4/4） 的唯一家族发布点，不引入产品行为：子卡交接门通过后，先在含全部子卡本地签名提交的合成树上运行聚合守卫并写入 Release 说明草稿，再执行 ER-08 `patch + 1` 与版本面同步、工具链刷新 `Cargo.lock`、`T-2` 双环境全量（在 bump 后最终树重跑全部守卫）、`cargo build --release` 与安装、签名+DCO 提交、PR（`base.yml` 与 `codeql.yml` 在同一 head SHA 全绿）、合入 `main`、以草稿为说明 `gh release create v<version>` 触发 `release.yml`；网站文档已由各子卡按 ER-06a 在本卡内交付，本卡不改写网站；完成后子卡继承本卡 C/D。
**Out of scope:** 任何源码、文档或网站内容修改（由 `CS-25a`、`CS-25i`、`CS-25e`、`CS-25b` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 启用自动维护（`CS-25a`）、默认分离时序与派生失败不传播（`CS-25i`）、`gc.autoDetach` 回退（`CS-25e`）与测试/CI 隔离（`CS-25b`）须同版发布，避免全量门与 CI 在中间版本派生维护进程 | `ADR-CS-03`; `plan-template.md` G-08 |

**子卡交接门（G-08/ER-04 前提，不计入 G-03；在本卡任何 bump 与推送之前执行并记录）:**
- [ ] `CS-25a`、`CS-25i`、`CS-25e`、`CS-25b` 均为 `in-progress` / `locally-accepted`、ER-05 review `PASS`，各自绑定的提交树 OID 已记录；任一不满足即 STOP。
- [ ] 对各子卡提交 SHA 记录 ER-07 两条校验命令结果；记录当时 `libra rev-parse origin/main` 的 OID，并对每个子卡 SHA 记录 `libra merge-base --is-ancestor <sha> origin/main` 的退出码（期望 1：尚未进入 `main`）；任一不满足即 STOP。

**Acceptance criteria:**（每条为一个独立谓词，计入 G-03；模板 `release` 行把聚合守卫与 release note 计为本卡特有项）
- [ ] AC1：聚合守卫 `command::commit_test::commit_auto_maintenance_trace2` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC2：聚合守卫 `command::commit_test::commit_auto_maintenance_detach_and_spawn_failure` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC3：聚合守卫 `command::commit_test::commit_auto_maintenance_autodetach_fallback` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC4：聚合守卫 `command::commit_test::commit_auto_maintenance_isolation` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC5：聚合守卫 `command::commit_test::commit_auto_maintenance_isolation_harness_without_nextest` 在子卡交接后、本卡 bump 之前的合成树上通过。
- [ ] AC6：本卡 `Current evidence` 中位于 `<!-- release-note-draft:REL-CS-06 begin -->` 与 `<!-- release-note-draft:REL-CS-06 end -->` 两行之间的 `v<version>` Release 说明草稿逐字等于按固定格式生成的期望文本（每行一张子卡，格式为 `- <卡号> <该子卡标题全文>`，共 4 行，顺序同家族登记：`CS-25a`、`CS-25i`、`CS-25e`、`CS-25b`）。
- [ ] AC7：`gh release create` 之后，实际 `v<version>` Release 说明（`gh release view --json body` 的 `body` 字段）与该草稿在各自去掉末尾全部换行后逐字符一致。

**Verification:**（按 V-1..V-6 执行并按 V-4 记录）
- [ ] V1 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_trace2)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC1）
- [ ] V2 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_detach_and_spawn_failure)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC2）
- [ ] V3 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_autodetach_fallback)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC3）
- [ ] V4 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_isolation)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC4）
- [ ] V5 `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_auto_maintenance_isolation_harness_without_nextest)'`（V-2；聚合守卫，在 bump 前合成树运行；覆盖 AC5）
- [ ] V6 手工证据（覆盖 AC6）：以 `draft="$(awk '/^<!-- release-note-draft:REL-CS-06 begin -->$/{f=1;next} /^<!-- release-note-draft:REL-CS-06 end -->$/{f=0} f' docs/development/plan/plan-20261117-commit-signing.md)"` 提取草稿（要求该赋值退出码为 0 且 `[ -n "$draft" ]`，否则为命令错误即 STOP），以 `expected="$(for id in CS-25a CS-25i CS-25e CS-25b; do awk -v id="$id" 'index($0, "### Task " id ": ") == 1 { print "- " id " " substr($0, length("### Task " id ": ") + 1); exit }' docs/development/plan/plan-20261117-commit-signing.md; done)"` 从本计划各子卡标题行按登记顺序构造期望文本（每行 `- <卡号> <标题全文>`），`[ "$draft" = "$expected" ]` 为真即通过；只在 shell 变量中比较，不写文件。
- [ ] V7 手工证据（覆盖 AC7）：先执行 `body="$(gh release view "v${VERSION}" --json body --jq .body)"` 并要求该赋值的退出码（即 `gh` 的退出码）为 0（非 0 为命令错误即 STOP）；再以上一条的同一 `awk` 命令提取 `draft`（要求退出码为 0 且非空；命令替换同样去掉末尾全部换行，与 AC 的归一化一致），`[ "$body" = "$draft" ]` 为真即通过；全程只在 shell 变量中比较，不写文件；结论按 V-4 以纯记账提交登记。

**状态收口（ER-04：D 组全绿后 `Acceptance` 才能到 `complete`；全局规则，不计入 G-03；以纯记账提交登记）:**
- [ ] D 组全绿后把 `CS-25a` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-25i` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-25e` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。
- [ ] D 组全绿后把 `CS-25b` 卡内与 `plan-status.md` 对应行的 `Lifecycle / Acceptance` 更新为 `done` / `complete`，并记录更新时间（`YYYY-MM-DD HH:MM:SS UTC`）。

**Full-suite trigger:** `T-2`：家族发布点；在 bump 后的最终提交树上运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`
**Dependencies:** `CS-25a`, `CS-25i`, `CS-25e`, `CS-25b`
**Deliverables:** 版本面集合（以 `compat_version_surface_sync` 为权威）与工具链刷新的 `Cargo.lock`、`v<version>` GitHub Release
**Implementation write set:** `N/A`（版本面属 Release write set；网站文件由子卡按 W-1..W-6 交付）
**Release write set:** `Inherited`
**Files likely touched:** `Cargo.toml`, `install.sh`, `install.ps1`, `Cargo.lock`
**Docs and compatibility impact:** `N/A`（文档、兼容矩阵与网站由 `CS-25a`、`CS-25i`、`CS-25e`、`CS-25b` 同卡交付）。
**Rollback mode:** `immutable-release`
**Estimated scope:** `S`
**Version increment:** `patch`
**Release boundary:** `family release point of REL-CS-06`
**C/D coverage from:** `self`
**Granularity:** `type=release; axis=REL-CS-06家族发布点; recovery=前滚新版本+降级指引; complete=yes; self-contained=yes; AC=7/12; VER=7/12; landing=0; prod-files=0; scope=S; deps=CS-25a,CS-25i,CS-25e,CS-25b; writeset=序列化于 CS-25b; release=family release point of REL-CS-06; split-from=N/A; exception=N/A`

## 测试矩阵

本表登记计划最终必须覆盖的范围；执行阶段按 ER-13 只跑与本卡相关的行，整表由收口全量门覆盖。

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `approxidate` 复用与无时区 ISO、`verify_commit_signature`（`gpg.program` 与 SSH） | `source .env.test && cargo nextest run --lib -E 'test(/^internal::log::date_parser::/)'`；`source .env.test && cargo nextest run --lib -E 'test(/^utils::approxidate::/)'`；`source .env.test && cargo nextest run --lib -E 'test(/^command::commit::tests::/)'` |
| 集成 | `commit` 参数、签名、部分提交、消息清理、UTF-8、hooks、嵌套锁、notes、trace2、自动维护、`commit-tree`、`cat-file`、`maintenance` | `source .env.test && cargo nextest run --test command_test`（各卡具名用例见追溯表） |
| 兼容 | amend、身份日期、配置默认、交互标志拦截、hooks 生命周期 | `source .env.test && cargo nextest run --test compat_commit_amend_no_edit --test compat_commit_identity_date --test compat_config_defaults_semantics --test compat_config_history_defaults --test compat_declined_interactive_flags --test compat_libra_hooks_lifecycle` |
| 迁移 | `N/A`（无 SQLite schema 变更） | `N/A` |
| 安全 | `gpg.format` 严格校验、SSH 临时文件清理与 fail-closed、重入令牌受限、hooks 遵循 PD-08 | `command::commit_test::commit_gpg_format_strict_values`、`command::commit_test::commit_ssh_signing_generation`、`command::commit_test::commit_ssh_signing_fail_closed`、`command::commit_editor_test::commit_nested_in_editor_reentry_token`、`compat_libra_hooks_lifecycle` |
| 性能 | 部分提交 1,000 与 10,000 条索引条目的中位耗时增长比 ≤ 20 倍（各预热 3 次、计时 10 次，记录机器与构建配置） | `source .env.test && cargo nextest run --test command_test -E 'test(=command::commit_test::commit_partial_pathspec_scaling)'` |
| live/gated | `N/A`（SSH/GPG 用例只用本地 `ssh-keygen`、`git` 与测试脚本，不访问外部服务） | `N/A` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| CS-01 | #787, #1006; `src/command/commit.rs:239-243` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_gpg_sign_keyid_parse_forms`、`command::commit_test::commit_gpg_sign_attached_keyid_e2e` |
| CS-01a | #787, #1006; `src/internal/vault.rs:417-427,507-527` | `src/command/commit.rs`, `src/internal/vault.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_gpg_sign_keyid_selects_key`、`command::snapshot_selftest::snapshot_detects_single_component_changes` |
| CS-02 | #1005; `src/command/init.rs:540-551,1187,2495-2508` | `src/command/init.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::init_test::init_does_not_enable_vault_signing`、`command::commit_test::commit_default_unsigned_and_signing_precedence` |
| CS-02a | #1005; `src/command/commit.rs:268-278`; `src/command/merge.rs:465-470` | `src/command/clone.rs`, `src/command/commit.rs`, `src/command/merge.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::clone_cli_test::clone_default_vault_signing_unset`、`command::clone_cli_test::successful_clone_initializes_vault`、`command::merge_test::gpg_sign::merge_default_unsigned_with_vault_key`、`command::cherry_pick_test::cherry_pick_default_unsigned_with_vault_key`、`command::commit_test::commit_gpg_sign_config_alone_signs` |
| REL-CS-00 | G-08 家族发布点（`CS-02`, `CS-02a`；#1005） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `command::init_test::init_does_not_enable_vault_signing`、`command::commit_test::commit_default_unsigned_and_signing_precedence`、`command::clone_cli_test::clone_default_vault_signing_unset`、`command::clone_cli_test::successful_clone_initializes_vault`、`command::merge_test::gpg_sign::merge_default_unsigned_with_vault_key`、`command::cherry_pick_test::cherry_pick_default_unsigned_with_vault_key`、`command::commit_test::commit_gpg_sign_config_alone_signs` |
| CS-03 | #781; `src/command/commit.rs:3088-3151` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_gpg_format_strict_values` |
| CS-03f | #783; `src/command/config.rs:4718-4726` | `src/command/config.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::config::tests::resolve_gpg_program_contract`、`command::config_test::gpg_import_uses_default_program_when_unset` |
| CS-03a | #783; `src/command/commit.rs:3088-3151` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_gpg_program_sign` |
| CS-03j | #783; `gpg-interface.c:1029-1046` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_gpg_program_sign_failures` |
| CS-03d | #783; `src/command/commit.rs:3156-3200`; `src/command/merge.rs:1634-1645` | `src/command/commit.rs`, `src/command/merge.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit::tests::verify_commit_signature_uses_gpg_program`、`command::merge::tests::verify_merge_signature_rejects_untrusted_gpg_program` |
| CS-03g | #783; `src/command/merge.rs:1630-1645` | `src/command/merge.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::merge_test::gpg_sign::merge_verify_signatures_with_gpg_program` |
| CS-03l | #783; `gpg-interface.c:676-687,765-775` @ `6de20f6092` | `src/command/merge.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::merge_test::gpg_sign::merge_verify_signatures_min_trust_level` |
| REL-CS-07 | G-08 家族发布点（`CS-03a`, `CS-03j`, `CS-03d`, `CS-03g`, `CS-03l`；#783（部分闭环，见 `DEFER-CS-03`）） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `command::commit_test::commit_gpg_program_sign`、`command::commit_test::commit_gpg_program_sign_failures`、`command::commit::tests::verify_commit_signature_uses_gpg_program`、`command::merge::tests::verify_merge_signature_rejects_untrusted_gpg_program`、`command::merge_test::gpg_sign::merge_verify_signatures_with_gpg_program`、`command::merge_test::gpg_sign::merge_verify_signatures_min_trust_level` |
| CS-03b | #787; `src/command/commit.rs:3088-3151` | `src/command/commit.rs`, `src/command/config.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_ssh_signing_generation` |
| CS-03c | #787; `src/command/commit.rs:3156-3200`; `src/command/merge.rs:1634-1645` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit::tests::verify_commit_signature_ssh_cases`、`command::merge_test::gpg_sign::merge_verify_signatures_with_ssh` |
| CS-03h | #787; `gpg-interface.c:476-480,499-527,625-656` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit::tests::verify_commit_signature_ssh_verify_time`、`command::merge_test::gpg_sign::merge_verify_signatures_ssh_verify_time` |
| CS-03i | #787; `gpg-interface.c:579-587,785-786` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit::tests::verify_commit_signature_ssh_revocation`、`command::merge_test::gpg_sign::merge_verify_signatures_ssh_revocation` |
| CS-03k | #787; `gpg-interface.c:537-571,679-685` @ `6de20f6092` | `src/command/commit.rs`, `src/command/merge.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit::tests::verify_commit_signature_ssh_multi_principal`、`command::merge_test::gpg_sign::merge_verify_signatures_ssh_trust_rules` |
| CS-03e | #787; `src/command/commit.rs:3088-3151` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_ssh_signing_fail_closed` |
| REL-CS-01 | G-08 家族发布点（`CS-03b`, `CS-03c`, `CS-03h`, `CS-03i`, `CS-03k`, `CS-03e`；#787） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `command::commit_test::commit_ssh_signing_generation`、`command::commit::tests::verify_commit_signature_ssh_cases`、`command::merge_test::gpg_sign::merge_verify_signatures_with_ssh`、`command::commit::tests::verify_commit_signature_ssh_verify_time`、`command::merge_test::gpg_sign::merge_verify_signatures_ssh_verify_time`、`command::commit::tests::verify_commit_signature_ssh_revocation`、`command::merge_test::gpg_sign::merge_verify_signatures_ssh_revocation`、`command::commit::tests::verify_commit_signature_ssh_multi_principal`、`command::merge_test::gpg_sign::merge_verify_signatures_ssh_trust_rules`、`command::commit_test::commit_ssh_signing_fail_closed` |
| CS-04 | #770; `src/command/commit.rs:1498-1530` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `amend_drops_old_gpgsig_unless_resigned`、`amend_date_matches_t7501_fix_date` |
| CS-05 | #772; `src/command/commit.rs:97-98,169-170,1845-1858` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_multiple_m_and_fixup_m` |
| CS-06 | #764, #791; `src/command/commit.rs:180-181,1850-1854` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_autosquash_test::commit_squash_message_sources_and_editor` |
| CS-08 | #774; `src/command/commit.rs:244-247`; `src/command/commit.rs:3019` | `src/command/commit.rs`, `src/cli.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_long_preview` |
| CS-10 | #760, #761; `src/command/commit.rs:2056-2094,2222-2234` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_editor_test::commit_verbose_template_and_trailing_lines` |
| CS-11 | #762, #763; `src/command/commit.rs:1864-1869` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_file_message_utf8_conformance` |
| CS-12 | #765; `src/command/commit.rs:1367-1374,1424-1430` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_signoff_dedup_and_placement` |
| CS-12a | #766; `src/command/commit.rs:1976-1982,2672-2677` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_trailer_only_message_is_empty` |
| CS-13 | #793; `src/command/commit.rs:1990-2002` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_editor_test::commit_verbatim_unedited_template_commits` |
| CS-14 | #794; `src/command/commit.rs:2237-2276` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_editor_test::commit_cleanup_whitespace_and_comment_char` |
| CS-14a | #982; `src/command/commit.rs:2098-2107,2222-2234` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_editor_test::commit_cleanup_scissors_editor_and_no_edit`、`command::commit_editor_test::commit_cleanup_scissors_squash_msg` |
| CS-15 | #757; `src/command/commit.rs:3008-3016` | `src/command/commit.rs`, `src/utils/pathspec/mod.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_partial_pathspec_only`、`command::commit_test::commit_partial_pathspec_scaling` |
| CS-15h | #757; `builtin/commit.c:535,547` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_partial_pathspec_deleted_file` |
| CS-15d | #757; `src/command/commit.rs:3008-3016` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_partial_preview_and_guards` |
| CS-15f | #757, #786; `src/command/commit.rs:3008-3016` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_only_without_paths_rules` |
| REL-CS-03 | G-08 家族发布点（`CS-15`, `CS-15h`, `CS-15d`, `CS-15f`；#757（部分闭环，见 `DEFER-CS-02`）） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `command::commit_test::commit_partial_pathspec_only`、`command::commit_test::commit_partial_pathspec_scaling`、`command::commit_test::commit_partial_pathspec_deleted_file`、`command::commit_test::commit_partial_preview_and_guards`、`command::commit_test::commit_only_without_paths_rules` |
| CS-15a | #786, #790; `src/command/commit.rs:254-267` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_include_pathspec` |
| CS-15b | #786, #790; `src/cli.rs:3005,3017-3027` | `src/cli.rs`, `src/command/commit.rs`, `src/command/add.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_patch_hunk_selection` |
| CS-15c | #786; `src/utils/pathspec/mod.rs` | `src/utils/pathspec/mod.rs` | `N/A` | `utils::pathspec::tests::parse_pathspec_file_forms` |
| CS-15g | #786; `builtin/commit.c:371-390,455-560` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_pathspec_from_file_mode_dispatch` |
| CS-15e | #786, #786; `src/command/commit.rs:3008-3016` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_pathspec_from_file_validation` |
| REL-CS-04 | G-08 家族发布点（`CS-15c`, `CS-15g`, `CS-15e`；#786（`--interactive` 部分由 `REL-CS-02` 补齐）） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `utils::pathspec::tests::parse_pathspec_file_forms`、`command::commit_test::commit_pathspec_from_file_mode_dispatch`、`command::commit_test::commit_pathspec_from_file_validation` |
| CS-09 | #773, #786; `src/cli.rs:3005,3017-3027`; `src/command/commit.rs:92-279` | `src/cli.rs`, `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_interactive_cli_guards` |
| CS-09b | #786; `builtin/commit.c:1374-1377` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_interactive_mode_exclusivity` |
| CS-09a | #773; `src/command/add.rs:1766-1767` | `src/command/commit.rs`, `src/command/add.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_interactive_piped_staging` |
| CS-09c | #773; `add-interactive.c:331-335` @ `6de20f6092` | `src/command/commit.rs`, `src/command/add.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_interactive_abort_without_staging` |
| REL-CS-02 | G-08 家族发布点（`CS-09`, `CS-09b`, `CS-09a`, `CS-09c`；#773、#786（`--interactive` 部分）） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `command::commit_test::commit_interactive_cli_guards`、`command::commit_test::commit_interactive_mode_exclusivity`、`command::commit_test::commit_interactive_piped_staging`、`command::commit_test::commit_interactive_abort_without_staging`、`compat_declined_interactive_flags`（共享回归组） |
| CS-07 | #789, #790; `src/command/commit.rs:1845-1849` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_autosquash_test::commit_fixup_amend_and_reword` |
| CS-07a | #789, #790; `builtin/commit.c:1295-1306` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_autosquash_test::commit_fixup_reword_conflicts` |
| CS-07b | #789, #790; `builtin/commit.c:853,1404-1406` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_autosquash_test::commit_fixup_message_and_prefix_errors` |
| REL-CS-09 | G-08 家族发布点（`CS-07`, `CS-07a`, `CS-07b`；#789、#790） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `command::commit_autosquash_test::commit_fixup_amend_and_reword`、`command::commit_autosquash_test::commit_fixup_reword_conflicts`、`command::commit_autosquash_test::commit_fixup_message_and_prefix_errors` |
| CS-16 | #792; `src/command/commit.rs:2016-2049` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_editor_test::commit_optional_template_prefix` |
| CS-17 | #769, #771; `src/command/commit.rs:820-880`; `src/internal/log/date_parser.rs:14-66` | `src/utils/approxidate.rs`, `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_date_approxidate`、`command::config_test::expiry_date_accepted_set_unchanged` |
| CS-17a | #769, #771; `src/command/commit.rs:820-880` | `src/utils/approxidate.rs`, `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_date_local_iso_and_dst`、`command::config_test::expiry_date_dst_inputs_unchanged` |
| CS-18 | #768; `src/command/commit.rs:1397-1405` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `amend_author_preserves_author_date` |
| CS-18b | #779; `src/command/commit.rs:204-205` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `reset_author_requires_amend_or_reuse`、`command::cherry_pick_test::cherry_pick_continue_commit_reset_author` |
| CS-18a | #776; `src/command/commit.rs:2903-2964` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `commit_output_author_line` |
| CS-18c | #771; `builtin/commit.c:694-697,1352-1363` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `commit_output_date_line` |
| CS-19 | #777; `src/command/commit.rs:1580-1590` | `src/command/commit.rs`, `src/command/notes.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_amend_notes_rewrite` |
| CS-19c | #777; `notes-utils.c:108-118,144` @ `6de20f6092` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_amend_notes_rewrite_unsupported_modes` |
| CS-19b | #777; `notes-utils.c:120-126,178-184` @ `6de20f6092` | `src/command/commit.rs`, `src/command/notes.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_amend_notes_rewrite_multi_and_glob` |
| CS-19a | #777; `src/internal/operation/middleware.rs:125-154,389-398` | `src/command/commit.rs`, `src/command/notes.rs`, `src/internal/operation/middleware.rs`, `src/internal/operation/middleware/lease.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_amend_notes_rewrite_contention` |
| REL-CS-08 | G-08 家族发布点（`CS-19`, `CS-19c`, `CS-19b`, `CS-19a`；#777） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `command::commit_test::commit_amend_notes_rewrite`、`command::commit_test::commit_amend_notes_rewrite_unsupported_modes`、`command::commit_test::commit_amend_notes_rewrite_multi_and_glob`、`command::commit_test::commit_amend_notes_rewrite_contention` |
| CS-20 | #779; `src/command/commit.rs:1538-1610` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `amend_does_not_leave_index_dirty_for_checkout` |
| CS-21 | #1032; `src/command/commit_tree.rs:116-123` | `src/command/commit_tree.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_tree_test::commit_tree_exact_message_bytes` |
| CS-21a | #1032; `src/command/cat_file.rs:715-717,906-908` | `src/command/cat_file.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::cat_file_test::cat_file_batch_empty_line_missing` |
| CS-22a | #767, #778; `src/command/commit.rs:2615-2639` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_prepare_commit_msg_hook_args` |
| CS-23 | #784; `src/internal/operation/middleware.rs:192-196,411-416`; `src/internal/operation/middleware/lease.rs:83,130-153` | `src/internal/operation/middleware.rs`, `src/internal/operation/middleware/lease.rs`, `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_editor_test::commit_nested_in_editor_reentry_token` |
| CS-23a | #784; `src/internal/operation/middleware.rs:411-416,714`; `src/internal/operation/middleware/lease.rs:83` | `src/internal/operation/middleware.rs`, `src/internal/operation/middleware/lease.rs`, `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_editor_test::commit_nested_editor_wait_and_termination` |
| CS-23b | #784; `src/internal/operation/middleware.rs:457,714` | `src/internal/operation/middleware.rs`, `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_editor_test::commit_nested_editor_state_and_oplog_isolation` |
| REL-CS-05 | G-08 家族发布点（`CS-23`, `CS-23a`, `CS-23b`；#784） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `command::commit_editor_test::commit_nested_in_editor_reentry_token`、`command::commit_editor_test::commit_nested_editor_wait_and_termination`、`command::commit_editor_test::commit_nested_editor_state_and_oplog_isolation` |
| CS-24 | #759; `src/cli.rs`; `src/utils/mod.rs` | `src/cli.rs`, `src/utils/mod.rs`, `src/utils/trace2.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::trace2_event_emission` |
| CS-25 | #759; `src/command/maintenance.rs:63-65,84-92,1041` | `src/command/maintenance.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::maintenance_test::maintenance_run_auto_loose_objects_condition` |
| CS-25g | #759; `src/command/maintenance.rs:236-238,4609-4613` | `src/command/maintenance.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::maintenance_test::maintenance_run_quiet_flags` |
| CS-25d | #759; `src/command/maintenance.rs:84-92` | `src/command/maintenance.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::maintenance_test::maintenance_run_auto_detach_semantics` |
| CS-25c | #759; `src/command/maintenance.rs:64,159-180,1447` | `src/command/maintenance.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::maintenance_test::maintenance_run_auto_incremental_repack_condition` |
| CS-25h | #759; `src/command/gc.rs:155-164,330-340` | `src/command/maintenance.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::maintenance_test::maintenance_run_auto_gc_condition` |
| CS-25f | #759; `src/command/maintenance.rs:240,1030-1037` | `src/command/maintenance.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::maintenance_test::maintenance_run_auto_lock_wait` |
| CS-25a | #759; `src/command/commit.rs:3043-3046` | `src/command/commit.rs`, `src/cli.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_auto_maintenance_trace2` |
| CS-25i | #759; `builtin/commit.c:1965` @ `6de20f6092` | `src/cli.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_auto_maintenance_detach_and_spawn_failure` |
| CS-25e | #759; `run-command.c:1974-1982` @ `6de20f6092`; `t/t7900-maintenance.sh:123-145` | `src/command/commit.rs`, `src/cli.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_auto_maintenance_autodetach_fallback` |
| CS-25b | #759; `tests/command/commit_editor_test.rs:13`; `tests/command/commit_json_test.rs:21` | `src/command/commit.rs` | 仓库文档 + `COMPATIBILITY.md` + 网站（见卡内 Docs） | `command::commit_test::commit_auto_maintenance_isolation`、`command::commit_test::commit_auto_maintenance_isolation_harness_without_nextest` |
| REL-CS-06 | G-08 家族发布点（`CS-25a`, `CS-25i`, `CS-25e`, `CS-25b`；#759） | 版本面集合与 `Cargo.lock` | `N/A`（子卡交付） | `command::commit_test::commit_auto_maintenance_trace2`、`command::commit_test::commit_auto_maintenance_detach_and_spawn_failure`、`command::commit_test::commit_auto_maintenance_autodetach_fallback`、`command::commit_test::commit_auto_maintenance_isolation`、`command::commit_test::commit_auto_maintenance_isolation_harness_without_nextest` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 本计划文本经 Claude（Subagent）与 Codex 对同一受审字节 X 双审 `PASS` 并提交 | 「Codex review log」中引用受审 SHA-256 X 的 PASS 行；PASS 行、文头状态行与对应修订行是双审 PASS 后对 X 的纯记账追加（不改动 X 的其它字节），提交的文件为 X 加这些记账行 | `N/A` |
| M1 | Phase 0 全部卡（含 `REL-CS-00`、`REL-CS-07`、`REL-CS-01`）`done`/`complete` | 各独立卡与家族发布点的 patch 版本与 D 组结果，各卡 W-6 结果 | 前滚新版本 + 降级指引 |
| M2 | Phase 1 与 Phase 2 全部卡（含 `REL-CS-03`、`REL-CS-04`、`REL-CS-02`、`REL-CS-09`）`done`/`complete` | 同上 | 前滚新版本 + 降级指引 |
| M3 | Phase 3 与 Phase 4 全部卡（含 `REL-CS-08`、`REL-CS-05`、`REL-CS-06`）`done`/`complete`，且计划收口全量门全绿 | 收口门的 fmt、clippy、双环境 nextest 结果与最终树 OID | 前滚新版本 + 降级指引 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 部分提交 hook 或提交失败 | 所选路径工作区内容的新 blob（不可达对象） | 返回错误，主索引与 HEAD 原样（SNAP-Δ 只含对象集合） | 污染主索引或提交错误树 |
| `gpg.program` / `ssh-keygen` / `commit-msg` 失败 | 无（临时文件已清理） | 返回错误，不创建提交 | 生成未签名或错误签名的提交、残留临时文件 |
| 编辑器内嵌套 commit 已推进 HEAD | 嵌套提交及其 operation-log 条目 | 外层检测后以错误中止，不覆盖、不回滚嵌套提交 | 覆写嵌套提交或提前释放租约 |
| amend 后 notes 复制失败 | 已推进的 HEAD；`notes` 表原有行 | 警告后放弃复制 | 覆盖并发写入的 note、残留 lease |
| 自动维护锁争用或超时 | 已完成的提交 | 维护子进程以退出码 0 退出、不执行维护 | 死锁、回滚提交或测试残留维护进程 |
| 家族发布点前子卡失败 | 子卡本地签名提交；子卡已推送的网站提交 | 按 `REL-CS-*` 逆序本地 `revert`；网站以新的签名提交前滚撤回该子卡的文档改动，按 W-1..W-5 与 W-6「撤回分支」推送并核验（不 force、不改写历史） | 部分子卡的 Libra 提交单独推送；撤回后线上网站仍含已撤回行为；force push 或改写历史 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| 默认不签名后既有测试依赖隐式签名 | 中：家族发布点全量失败 | `CS-02a` 改写依赖旧默认的断言，`REL-CS-00` 跑 `T-2` 全量 | `REL-CS-00` |
| 自动维护派生与锁、测试或 CI 冲突 | 中：死锁或孤儿进程 | `ADR-CS-03` 在共享锁释放后派生；`CS-25b` 的 harness 注入与以 `GIT_TRACE2_EVENT` 归因的全量运行守卫 | `CS-25`, `CS-25d`, `CS-25c`, `CS-25h`, `CS-25f`, `CS-25a`, `CS-25i`, `CS-25e`, `CS-25b` |
| 重入令牌放行范围过宽 | 高：并发写入破坏仓库 | `ADR-CS-04` 竞态与进程身份合同；`CS-23`/`CS-23a` 故障注入 | `REL-CS-05` |
| 同日计划共享写集被并发修改 | 中：冲突或回滚 | `DEP-CS-07` 动态窗口 + 顺序前置 `DEP-CS-01..06,08,09` | 全部实现卡 |
| 家族子卡网站页面早于家族 CLI 发布公开（ER-06a 同卡交付） | 低：网站短时描述尚未发布的行为 | 家族窗口串行且发布点紧随最后一张子卡；子卡失败时按故障恢复矩阵前滚撤回网站改动 | 全部 `family child` |
| SSH/Git 外部工具在测试环境缺失 | 低：用例失败而非跳过 | 用例显式失败并提示安装；不允许以跳过计为通过（V-2） | `CS-03b`, `CS-03c` |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 部分提交 pathspec 匹配与临时索引 | `O(index entries)` | `O(N)` | 10,000 条中位耗时 ≤ 1,000 条的 20 倍；无提交历史扫描 | `command::commit_test::commit_partial_pathspec_scaling` |
| `GIT_TRACE2_EVENT` 写入 | `O(1)` 追加写 | 有界（每命令固定事件数） | 未设置时零 I/O | `command::commit_test::trace2_event_emission` |
| commit 后自动维护 | 一次子进程派生 | 有界 | 提交等待派生的 `maintenance run --auto` 完成条件判定；分离后的维护工作不阻塞提交返回；无任务满足条件时子进程立即退出 | `command::commit_test::commit_auto_maintenance_trace2`、`command::maintenance_test::maintenance_run_auto_loose_objects_condition`、`command::maintenance_test::maintenance_run_auto_detach_semantics` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 已按各卡 D-1 同步（只在既有条目行内修改，被推翻的旧陈述已替换且旧关键句零命中，行数不变）。
- [ ] `docs/commands/{commit,init,clone,merge,cherry-pick,config,commit-tree,cat-file,maintenance,log,reflog,rev-list,shortlog,archive}.md` 与 `docs/commands/README.md` 已按各卡同步。
- [ ] `docs/commands/zh-CN/` 下对应文件与 `README.md` 已同步。
- [ ] `docs/development/commands/` 下对应文件与 `_compatibility.md`（D15）已同步；`CLAUDE.md` 与 `.env.test.example` 已按 `CS-02`、`CS-24`、`CS-25b` 同步。
- [ ] `docs/error-codes.md`：`N/A`，本计划不新增稳定错误码，只复用既有 `LBR-*`。
- [ ] `tests/INDEX.md`：`N/A`，不新增或重命名 `--test` target（新用例全部加入既有 `command_test`、`compat_*` target 与 `--lib`）；若实施时描述失真则同卡更新。
- [ ] `Cargo.toml` `[[test]]`：`N/A`，同上。
- [ ] 网站 `content/docs/commands/*.en.md` 已按各卡 W-1..W-6 交付并部署。
- [ ] `plan-long.md`：`DEFER-CS-02`/`DEFER-CS-03`/`DEFER-CS-04`/`DEFER-CS-06`/`DEFER-CS-07`/`DEFER-CS-08`/`DEFER-CS-09` 的承接位置已登记，或明确 `N/A`。

## Codex review log

本节记录计划文本的评审轮次。按用户 2026-11-18 指令，计划文本提交门为 Claude（Subagent）与 Codex 双审，二者须对同一受审字节 X 均 `VERDICT: PASS`；登记该结论的 PASS 行、文头状态行与修订行在双审 PASS 之后作为纯记账修改加入并引用 X，提交的文件为 X 加这些记账行。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 (Claude) | `plan-20261117-commit-signing.md` | `FAIL` | 15 项 P1；R2 已大部关闭 | 已在 R2 修正 | `claude -p --model claude-opus-5-5` R1 |
| R1 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 14 项 P1；R2 已大部关闭 | 已在 R2 修正 | `codex exec -m gpt-6.1-terra` R1 |
| R2 (Claude) | `plan-20261117-commit-signing.md` | `FAIL` | 9 项 P1（`CS-01` 分离守卫、`CS-09` 顺序、`CS-15`/`CS-14` 拆分、`DEP-CS-01`/`02`、`CS-17` `approxidate.rs`、`CS-25` `ADR-CS-03` & `T-1`、`DEFER-CS-02`/`03`）；R3 已关闭 | 已在 R3 一并关闭 | `claude -p --model claude-opus-5-5` R2 |
| R2 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 7 项 P1（`CS-15`/`CS-21` 拆分、`DEP-CS-01` 阻塞规则、`#982` `--no-edit` 断言、`CS-10` 回归用例、网站交付条件、`CS-02` 4 landing）；R3 已关闭 | 已在 R3 一并关闭 | `codex exec -m gpt-6.1-terra` R2 |
| R3 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 3 项 P1（`DEP-CS-02` 死锁、`DEP-CS-01` 产物、`CS-10` ER-06a 文档）；R4 已关闭 | 已在 R4 关闭 | `codex exec -m gpt-6.1-terra` R3 |
| R4 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 2 项 P1（后端工作区路径核实口径、`DEP-CS-02` 单边阻塞门）；R5 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R4 |
| R4/R5 (Claude) | `plan-20261117-commit-signing.md` | `FAIL` | 3 项 P1（跨计划写集重叠 G-10、`CS-25`/`ADR-CS-03` 父锁与写集、`CS-21a` 锚点 `715-717` 与 `--batch-command` 语义）；R6 已关闭 | 9 项 P2（`#770`、`#767/#778`、修订历史、审计表计数、outgoing 依赖、`DEP-CS-02` 超时、`ADR-CS-01`、dev 文档、`CS-01` AC2）已全部在 R6 关闭 | `claude -p --model claude-opus-5-5` R5 |
| R5 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 2 项 P1（Phase 0 进入条件死锁、`#757` `bisect visualize` 部分闭环口径）；R6 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R5 |
| R6 (Claude) | `plan-20261117-commit-signing.md` | `FAIL` | 5 项 P1（`global-init-infra`/全部 15 计划 `COMPATIBILITY.md` 写集重叠、`CS-15b`/`CS-09` `DECLINED_INTERACTIVE_FLAGS`、`#783` 部分闭环、`CS-23` operation-v2 `ScopeLease`、`CS-25` `--auto` 阈值与风险表）；R7 已关闭 | 4 项 P2（`CS-24` AC2、`GIT_TRACE2_EVENT` `CLAUDE.md`、收口 dev 文档清单、`CS-23` 锚点）已在 R7 关闭 | `claude -p --model claude-opus-5-5` R6 |
| R6 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 1 项 P1（`DEP-CS-03..05` 改严格 start-to-completion）；R7 已关闭 | 1 项 P2（风险表与 `ADR-CS-03` 一致）已在 R7 关闭 | `codex exec -m gpt-6.1-terra` R6 |
| R7 (Claude) | `plan-20261117-commit-signing.md` | `FAIL` | 4 项 P1（`DEP-CS-04/06/07` 逐卡引用、未登记的 `CL-19`/`GII-01`/`GII-05`/`cli.rs` 门控、`CS-25`/`ADR-CS-03` 阈值位置与 `.env.test.example`、`CS-23` `middleware/lease.rs:83`）；R8 已关闭 | 5 项 P2（`CS-15c` AC≤8 与 `#786`/`#789` 精确文案、`CS-19` `middleware.rs`、`ADR-CS-03` 布尔开关）已在 R8 关闭 | `claude -p --model claude-opus-5-5` R7 |
| R7 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 3 项 P1（`CS-15b` 补 `DEP-CS-03/04`、`DEP-CS-06` 改 `incoming` 前置于 `CS-02`、`DEP-CS-07` 明确 Coordinator 跨计划批处理调度边界）；R8 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R7 |
| R8 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 1 项 P1（`ADR-CS-04`/`CS-23` 宽泛租约绕过改为受限单次重入令牌与生命周期隔离）；R9 已关闭 | 同步按 `AGENTS.md` 将网站写集改为 `/Volumes/Data/gitmono/libra-backend` | `codex exec -m gpt-6.1-terra` R8 |
| R9 (Claude) | `plan-20261117-commit-signing.md` | `FAIL` | 5 项 P1（`CS-19` 改局部租约不碰 `middleware.rs:392-398`、`#786` 精确 Git 文案、`src/utils/pathspec/mod.rs` + `DEP-CS-08` `DIFF-45`、`CS-25a` `cfg!(test)`+`.cargo/config.toml` CI 隔离、超 8 AC 卡拆分为 `CS-02a/03c/15d/15e/09a/25a`）；R10 已关闭 | 9 项 P2（风险表、移除 `PM-09a`、对齐 `DEP-SA-01/02` + `DEP-SA-07a`、`CSR-27`、AC1、卡数 `41`、追溯表、Phase 范围、`CS-12a` 拆分、日志格式）已在 R10 关闭 | `claude -p --model claude-opus-5-5` R9 |
| R9 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 1 项 P1（`CS-23`/`ADR-CS-04` 原子单次认领与外层带界等待已认领后台嵌套 commit + 竞态测试）；R10 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R9 |
| R10 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 2 项 P1（`CS-03b`+`CS-03c` 纳为 `REL-CS-01` 家族发布、`DEP-SA-02` 对齐 `REL-CS-01`）；R11 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R10 |
| R11 (Claude) | `plan-20261117-commit-signing.md` | `FAIL` | 6 项 P1（`CS-02/02a` 纳为 `REL-CS-00`、`DEP-CS-02` 补 `MP-26`、`REL-CS-*` 规范字段与 `发布分组` 表、版本/修订历史对齐、`CS-03` 改 `LBR-CLI-003`、`CS-09/09a` 纳为 `REL-CS-02`）；R12 已关闭 | 12 项 P2（`CS-15e` 例外、`CS-19` 锁序、`CS-25a` `autoDetach`、`CS-17` 共享调用方、`CS-23` 文档、pinned `6de20f6092`、`DEP-CS-06`、`DEP-SA-08`、`CS-02` `prod-files=3`、`REL-CS-*`/`CS-13`/`CS-24` 验证）已在 R12 关闭 | `claude -p --model claude-opus-5-5` R11 |
| R11 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 2 项 P1（`CS-09/09a` 纳为 `REL-CS-02` 家族发布、移除 `.cargo/config.toml` 免污染 `cargo run`）；R12 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R11 |
| R12 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 1 项 P1（`DEP-MP-02` 与 `merge-pull.md:176` 的 `REL-CS-00` vs `CS-02` 跨计划引用对齐）；R13 已在本计划明确 `REL-CS-00` 交付口径与 Coordinator 批处理同步边界 | 无 | `codex exec -m gpt-6.1-terra` R12 |
| R13 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 2 项 P1（`ADR-CS-04`/`CS-23` 明确有界等待超时终止并确认子进程退出后再释放 `ScopeLease` 与超时并发写入测试；`ADR-CS-03`/`CS-25a` 专项测试同时 `env_remove` 移除 `NEXTEST_RUN_ID` 与 `LIBRA_NO_AUTO_MAINTENANCE`）；R14 已关闭 | 无 | `codex exec -m gpt-6.1-terra` R13 |
| R14 (Codex) | `plan-20261117-commit-signing.md` | `FAIL` | 无 | 1 项 P2（`CS-03` `Granularity` `deps`/`writeset` 已同步为 `REL-CS-00`）已在 R15 关闭 | `codex exec -m gpt-6.1-terra` R14 |
| R15–R29 (Codex) | 分区或部分审查 | `FAIL` | 各轮 P1 与处理见「修订历史」2026-11-17 07:05:00 UTC 至 2026-11-18 09:43:54 UTC 各行；本节此前未逐轮登记 | 见修订历史 | `codex exec -m gpt-6.1-terra` |
| R30 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `d109fd15b9d550237f6b923d5532ce484f1921b3ba8626230db70cc3ace90854` | `FAIL` | 8 项 P1：MSG 末尾换行规范化缺失；`CS-08` 断言退出码 1；`CS-17` 依赖未核实 `%ad`；`CS-20` 错误码；`CS-22` `--amend -m` 参数与 Git 相反；`CS-23` 诊断无文案；家族共享网站文件 W-1 冲突；`CS-09` 依赖未定义字段名 —— 均在 R31 关闭 | 3 项 P2（init 提示文案、`gpg.program` 参数、等待时限变量）已在 R31 吸收 | 修订历史 2026-11-18 13:34:09 UTC 行 |
| R30 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 3 项（家族网站阻塞、D 组 W-6 与 N/A 冲突、缺 CDN 判据）；B 9 项（网站阻塞、`CS-04` 签名夹具矛盾、`CS-06` 同目标、`CS-03a` helper 契约、外部验签绑定、merge 文档、G-03 计数、共享回归作 AC 证据、`CS-08` porcelain）；C 7 项（网站阻塞、`CS-15e` 例外方向、`CS-15c` `--include`、`CS-07` reword、`CS-12a` verbatim、G-03 计数、交接核验）；D 7 项（`CS-22`、`CS-17` 时钟、G-03 计数、`CS-25` 老化、隔离晚于启用、`CS-25b` 兼容矩阵、`CS-19` 全量快照）—— 均在 R31 关闭 | C 的 P2（`CS-15a` 的 `-i -a`）登记为残余风险，责任人：计划执行者 | 修订历史 2026-11-18 13:34:09 UTC 行 |
| R31 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `15676752dd887286e85d79c7d27f2abd892957412fffecb27b9978bcadcb88f7` | `FAIL` | 1 项 P1：`ADR-CS-03` 未含 `NEXTEST_RUN_ID` 隔离与 `REL-CS-06` —— R32 关闭 | 3 项 P2（非目标 `CS-01a`、适用范围、评审结论清单）R32 吸收 | 修订历史 2026-11-18 13:42:09 UTC 行 |
| R31 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 2 项（V-6 与嵌套提交冲突、`DEP-CS-07` 漏 `REL-CS-06`）；B 5 项（`CS-03d` 信任级别、`CS-03a` `SIG_CREATED`、MSG 常量、失败 AC 未断言失败、`CS-03f` 级联）；C 5 项（`CS-15c` 依赖后继卡、`CS-07` 完整消息、`CS-15f` 抽查、SNAP 漏 notes ref、发布点逐卡计数）；D 7 项（`CS-25` 对象丢失、`gc.auto` 语义、`CS-23b` 基线、`CS-22` 失败断言、`CS-07a` 漏组合、`CS-23b` 全量、`CS-25b` 用户文档）—— 均在 R32 关闭 | A 的 P2（ADR/成功定义归属）R32 吸收；D 的 P2（系统级 `pgrep` 受他进程干扰）登记为残余风险，责任人：计划执行者 | 修订历史 2026-11-18 13:42:09 UTC 行 |
| R32 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `2b05709d60f92a753ab1281ad030a2c6a30a2d230b54db8dee9d7c39ce433738` | `FAIL` | 1 项 P1：`CS-03c` 对未知 SSH 签名者的结果与 Git 相反且未登记信任级别 —— R33 关闭 | 1 项 P2（`CS-03g` 的 pull 表述）R33 吸收 | 修订历史 2026-11-18 13:48:55 UTC 行 |
| R32 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 3 项（ADR 阈值口径、V-4 被测树、W-6 部署绑定）；B 4 项（`CS-03d` 中间版本放行、`CS-06` 编辑器内容、发布说明/状态核验、VER 计入共享回归）；C 5 项（`CS-07a` 检查顺序、`CS-15g` 未证明列出路径、失败 AC 形式、`CS-07` `allow_empty`、发布说明核验）；D 8 项（`CS-18b` author 身份、失败 AC 形式、`CS-25` 计数与逐任务条件、`CS-19` 多值/glob 与警告、`CS-18a` 非 UTC、发布说明核验、`CS-07b` 夹具自含）—— 均在 R33 关闭 | A 的 P2（M3 列 `REL-CS-06`）与 C 的 P2（`CS-09a` 两 hunk）R33 吸收；D 的 P2（`pgrep` 系统级）维持残余风险 | 修订历史 2026-11-18 13:48:55 UTC 行 |
| R33 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `5ca8699e968d10386c54d9c0579504d552239c3165af9d2b2ccd207052cac7c5` | `PASS` | 无 | 2 项 P2（`CS-25c` gc 计数口径须按 `src/command/gc.rs` 核对、`CS-25b` 系统级 `pgrep`）R34 吸收：`CS-25b` 改为 `GIT_TRACE2_EVENT` 归因检查 | 修订历史 2026-11-18 14:02:34 UTC 行 |
| R33 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 2 项（V-4 记录自引用、M0 同字节与仓内登记冲突）；B 6 项（`CS-03d` 写集不含 `merge.rs` 无法编译、发布点 V-4 自引用、`CS-03e` AC2 命令缺 `-S`、`CS-01a` 只查 issuer、`CS-10` 不验证 diff 内容、`CS-11` 合法 UTF-8 未验证保真）；C 3 项（`CS-15g` 未构造期望树、`--include` 合并断言、预览命令零写入口径）；D 10 项（`CS-16` 配置来源存在模板、`CS-18a` 非 UTC 覆盖与 JSON 不变、`CS-19` 不复制/保留与 `refs/notes/` 限制、`CS-22` `--no-verify` 跳过 pre-commit、`CS-25` 阈值边界与 detach 语义、`CS-25c` 等待后执行、`CS-25a` 实际执行、`CS-25b` 归因）—— 均于 R34 修订行关闭 | 无 | 修订历史 2026-11-18 14:02:34 UTC 行 |
| R34 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `9f0d8f4d5cf71b345c2788d90c823ccc7004d26e39f95feed83b4580c9aa3b32` | `FAIL` | 1 项 P1：`CS-03d` 中间状态 fail-closed 无验收 —— R35 关闭（新增 AC8） | 1 项 P2（`CS-15g` 期望树改用独立命令求得）R35 吸收 | 修订历史 2026-11-18 14:12:42 UTC 行 |
| R34 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 3 项（SNAP 工作区未含文件类型与链接目标、家族网站时序引用 code-only 例外、loose 阈值称与 Git 一致但未处理 `maintenance.loose-objects.auto`）；B 3 项（`CS-03a` 外部签名先于外部验签独立发布、`CS-03c` 未按提交时间 `-Overify-time`、`CS-04` AC5 含三个谓词）；C 3 项（`CS-09` 未测 `--interactive` 与 `-i`/`-o` 互斥、`CS-09a` 路径限定不可区分、`CS-17` 配置解析接受集合只测一个输入）；D 7 项（`CS-18` 作者身份未断言、`CS-19b` glob 只比集合且无 SNAP-Δ、`CS-24` 退出码未断言真实值、`CS-25c` 等待可被绕过、`CS-25c` 缺 A 组门、`CS-25b` 正则漏判带空格 JSON、`CS-25b` 临时 trace 文件属证据文件）—— 均于 R35 修订行关闭 | 1 项 P2（`CS-15b` A 组加 `add_patch_test`）R35 吸收 | 修订历史 2026-11-18 14:12:42 UTC 行 |
| R35 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `d9acc90e2fd27142b33a987584aa0635ae52245b5e8c3caec70f1cc92d38599d` | `PASS` | 无 | 2 项 P2（`CS-25f` 释放场景等待上界、预览只读分类缺 `--short`/`--long`）R36 吸收 | 修订历史 2026-11-18 14:22:20 UTC 行 |
| R35 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 3 项（W-1 未枚举未跟踪/被忽略网站文件、SNAP 不含 operation-log 却称预览零写入、家族网站延后公开缺模板授权）；B 3 项（`CS-03h` 夹具不能区分提交者/作者时间与 UTC 格式化、两个发布点遗漏子卡新守卫、`CS-01` 无端到端用例）；C 4 项（`REL-CS-04` 先于 `--pathspec-from-file`/`--patch` 互斥发布、`CS-07` 先于冲突守卫独立发布、reword 未拒绝合并中、`CS-15a`/`CS-15b` 互斥负例不全）；D 5 项（`CS-18b` 未断言作者重置、`CS-19a` 并发场景无全量基线、`CS-25d` 只比 loose 集合、`CS-25f` 释放后未证明实际执行、`CS-25b` harness 注入被 nextest 掩盖）—— 均于 R36 修订行关闭 | 2 项 P2（追溯表补 `CS-25b` 第二用例、`CS-25d` 反向顺序）R36 吸收 | 修订历史 2026-11-18 14:22:20 UTC 行 |
| R36 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `0e582eb82e02acad668eb007abac747484548aaeb1c1e7ee6f65865f14b59328` | `PASS` | 无 | 1 项 P2（`CS-24`/`CS-25a` 单谓词写法）R37 吸收 | 修订历史 2026-11-18 14:27:54 UTC 行 |
| R36 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 3 项（⑦ 以去重截断接口读取 op heads、W-6 管道把上游读取失败当作不含、网站撤回无法满足新增句判据）；B 4 项（`CS-03c` 未处理 SSH 吊销文件、`CS-03g` 忽略 `gpg.minTrustLevel`、`CS-01a` 未验证显式选择导入密钥、`CS-03b` 两种选钥形式未验证实际签名者）；C 1 项（`CS-15d` 预览输出错误限定为所选路径）；D 2 项（`CS-18b`/`CS-24`/`CS-25a` 复合谓词计数、`CS-23b` 基线与 operation 收尾冲突）—— 均于 R37 修订行关闭 | 2 项 P2（`check-ignore` 对已跟踪文件需 `--no-index`、`CS-15e` Out of scope 收窄）R37 吸收 | 修订历史 2026-11-18 14:27:54 UTC 行 |
| R37 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `8710ed59094b654d325267c6647cc87b83355bc70ce9879ba8b9e84670d67db4` | `FAIL` | 1 项 P1：`AGENTS.md:130` 默认签名表述在 `CS-02` 后滞后且未登记 —— R38 在「文档职责」登记为跨计划协调事项 | 无 | 修订历史 2026-11-18 14:33:40 UTC 行 |
| R37 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 2 项（W-2 关键句检查阻断网站撤回、W-1 切换分支与「文档职责」STOP 冲突）；B 4 项（`CS-02a` 开发文档写集——核实为误报：写集已逐一列出 `docs/development/commands/{commit,clone,merge,cherry-pick,config}.md`；`CS-03a` 成功夹具缺 `SIG_CREATED ` 状态字段；`REL-CS-01` 组合守卫超限；`CS-01a` 未同步 `CLAUDE.md` keyid 表述）；C 2 项（发布说明管道吞掉 `gh` 失败、`CS-09a` 非法输入未断言失败与快照）；D 3 项（`CS-23b` 未分别证明 index/worktree 变化触发中止、`CS-25d` 未覆盖不带 `--auto` 的 `--detach`、`CS-19` 缺目标已有 note 的合并规则）—— 除误报外均于 R38 修订行关闭 | 无 | 修订历史 2026-11-18 14:33:40 UTC 行 |
| R38 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `310f06fc53ffeea6be58c42b298cc5406939538b1375b94550e2ef73d105e745` | `PASS` | 无 | 1 项 P2（W-5 管道上游状态）R39 吸收 | 修订历史 2026-11-18 14:38:45 UTC 行 |
| R38 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 2 项（SNAP ⑤ 所用 `cat-file --batch-all-objects` 静默跳过不可解析 pack index、故障恢复矩阵禁止网站提前公开与家族子卡网站时序冲突）；B 3 项（`CS-03a` 未验证未配置 `gpg.program` 时仍用 vault 签名、`CS-03d` 未验收临时文件清理、`CS-03g` 未覆盖非法与大小写 `gpg.minTrustLevel`）；C 2 项（发布点 B 组门依赖尚未创建的 Release 与 bump 后树、`CS-17a` 配置守卫未覆盖夏令时输入）；D 3 项（`CS-19` 不支持模式未验证且计数超限、`CS-23b` index 场景与令牌协议冲突、`CS-25c` 只检查任务名称）—— 均于 R39 修订行关闭 | 1 项 P2（`CS-03f` 测试模块为新建）R39 吸收 | 修订历史 2026-11-18 14:38:45 UTC 行 |
| R39 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `f27cf19cb281c0727155e9769e9d1f0ed089df11b9eff14ede3251a764d32e90` | `FAIL` | 1 项 P1：发布点聚合守卫与 release note 未按模板 `release` 行计数 —— R40 关闭 | 1 项 P2（`CS-25c` gc 隔离账本）R40 吸收 | 修订历史 2026-11-18 14:43:47 UTC 行 |
| R39 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 1 项（发布点聚合守卫与 release note 未计入 G-03）；B 3 项（`CS-03g` 混合大小写缺成功对照、`CS-03h` 吊销缺未吊销对照、吊销配置缺文档）；C 1 项（交接证据放在发布后，祖先检查与状态必然失败）；D 3 项（交接证据位于发布后、`CS-25c` gc 夹具不满足隔离宽限、`CS-24` 指定不存在的兼容矩阵行）—— 均于 R40 修订行关闭 | 2 项 P2（性能摘要 detach 表述、`REL-CS-04` 依据写错启用卡）R40 吸收 | 修订历史 2026-11-18 14:43:47 UTC 行 |
| R40 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `638d236aa11907ecf6e8172503214b9fda2d0d5e3fe11e67edc92153d9bfc2f4` | `FAIL` | 1 项 P1：`CS-25a` 未约束维护子进程失败后的提交结果 —— R41 关闭 | 无 | 修订历史 2026-11-18 14:47:54 UTC 行 |
| R40 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 2 项（SNAP 索引分量缺标志、网站推送前未检查本地 `cf` 无关超前提交）；B 2 项（Release 说明比较受命令替换去尾换行影响、`CS-03i` 吊销文件缺失时未验证继续验签）；C 1 项（同 B 的 Release 说明比较）；D 5 项（`CS-19a` 未断言 amend 仍成功、`CS-25c` 缺正阈值以下对照、`CS-25d` 反向顺序与非 auto 场景未证明成功与分离、`CS-25a` 维护失败后的提交结果、Release 说明比较）—— 均于 R41 修订行关闭 | 无 | 修订历史 2026-11-18 14:47:54 UTC 行 |
| R41 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `fa06e0e4d04e8d0676dc62110b55cbc5488bb3cd3071f95e9d2b7f1452adca3f` | `PASS` | 无 | 1 项 P2（发布点草稿提取占位）R42 吸收 | 修订历史 2026-11-18 14:51:14 UTC 行 |
| R41 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（B 区 `PASS`） | A 1 项（W-2 把既有无关改动误判为本卡改动）；C 2 项（`CS-15f` 未验证 `--amend --only -m` 改写消息、发布点草稿提取为占位命令）；D 3 项（草稿占位命令、`CS-25a` 失败注入未观察到子进程失败、`CS-20` 公开行为修复缺兼容矩阵）—— 均于 R42 修订行关闭 | 无 | 修订历史 2026-11-18 14:51:14 UTC 行 |
| R42 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `5872aab8e3f20eb9c5bf98e30f7dca255eae60739e52874d143f3c185206e833` | `PASS` | 无 | 2 项 P2（W-1 指纹按类型、`CS-20` 默认覆盖口径）R43 吸收 | 修订历史 2026-11-18 14:55:36 UTC 行 |
| R42 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（A 区 `PASS`） | B 3 项（`CS-03` 过渡期 SSH 拒绝无验收、`CS-06` 同一提交判定应为参数字符串比较、`CS-04` 未保护提交者身份）；C 2 项（`CS-15g` 未验证提交后主索引、`CS-17` 声称支持 approxidate 但验收过窄）；D 2 项（`CS-25a` 夹具先等待子进程退出无法证明同步等待、`CS-21` 多段 `-m` 未覆盖自带尾随换行）—— 均于 R43 修订行关闭 | A 区 2 项 P2 R43 吸收 | 修订历史 2026-11-18 14:55:36 UTC 行 |
| R43 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `a670d1866cf1f7179b0011a01fc1da7efbf4c6bae84e13483ab47bf1a25e4c9a` | `FAIL` | 1 项 P1：W-5 哈希管道未检查上游退出码 —— R44 关闭 | 无 | 修订历史 2026-11-18 15:01:14 UTC 行 |
| R43 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 2 项（`DEP-CS-05` 首个 `src/cli.rs` 写者与实施顺序不符、W-5 管道）；B 3 项（`CS-03a` 缺空签名失败路径、`CS-03c` 未覆盖多 principal、`gpg.minTrustLevel` 未对 SSH 验收）；C 3 项（`CS-15` 未覆盖删除路径、`CS-14` 缺自定义注释字符下的非编辑器默认清理、`CS-17` 声称的日期集合未验收）；D 2 项（`CS-25` `--no-quiet` 无输出验收、空操作维护无法检测对象物理位置变化）—— 均于 R44 修订行关闭 | 无 | 修订历史 2026-11-18 15:01:14 UTC 行 |
| R44 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `a87611a7d69c08c5ee9da1a3b50c7f110aaec10f71b00d8e978495b8fe07be07` | `PASS` | 无 | 1 项 P2（`REL-CS-01` AC 已达 12/12） | 修订历史 2026-11-18 15:03:08 UTC 行 |
| R44 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（A 区 `PASS`） | B 1 项（`CS-01` 端到端用例的 keyid `key` 在 `CS-01a` 后必然失败）；C 3 项（`CS-15`/`CS-15h` 未全量比较未选路径索引、`CS-15g` 缺 `--pathspec-file-nul` 成功场景、`CS-09a` 未验收 `status`/`diff` 输出）；D 3 项（`CS-21` 夹具 `"a\n\n"` 不含真实换行、`CS-25g` 缺 `--no-quiet --quiet` 顺序、`CS-25c` gc 关闭对照只抽查两个对象）—— 未关闭：已达 15 轮上限，停止 | A 区 1 项 P2（SNAP helper 交付门应写 ①–⑧ 并补同名 pack 字节变化自检） | 修订历史 2026-11-18 15:03:08 UTC 行 |
| R45 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `01f2fa09949871a2c289ed6aee870bd84c74e45ec456faf1b77fa293f7fdc2ba` | `PASS` | 无 | 4 项 P2（helper 新增 pack 字节自检未限定改写位置、`CS-25c` 残留 gc 证据行、「成功定义」与 `DEFER-CS-03` 卡列表未含 `CS-15h`/`CS-15f`/`CS-03j`、`REL-CS-01` AC 12/12）—— 前三项 R46 处理 | 修订历史 2026-11-18 15:25:15 UTC 行 |
| R45 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（A 区 `PASS`） | B 1 项（`CS-03a`/`CS-03j` 把「stdout 签名非空」误称为 Git 判据，Git `gpg-interface.c:1029-1046` 只查退出码与 `SIG_CREATED`）；C 2 项（`CS-15h` 未验证提交树排除未选的已暂存 `b`、`CS-16` 存在模板时未验收提交结果）；D 3 项（`CS-24` 生命周期事件只在 `commit` 上验收、`CS-25a` 未验收派生失败不传播、`CS-25a` 未验收默认分离时 commit 不等待维护）—— R46 关闭 | 无 | 修订历史 2026-11-18 15:25:15 UTC 行 |
| R46 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `4d2771563f8d21e3d22b788d94cfc95706a1723e8f2f0c7ed0426f2548fba44e` | `PASS` | 无 | 2 项 P2（`CS-25i` 新测试钩子未列入 `CLAUDE.md` 钩子清单、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 15:31:58 UTC 行 |
| R46 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 1 项（SNAP ⑧ 只记 loose 路径集合，测不出同路径 loose 对象内容被改写）；B 1 项（`CS-11` 未验收 U+FDD0..U+FDEF）；C 1 项（`CS-17` 声称的参数化日期语法只测单一样例）；D 3 项（`CS-18a` 把 `--amend` 本身当作 Date 显示条件而 Git 为 `author_message || force_date`、`--reset-author` 无对照；`CS-25i` 派生失败未证明确实尝试派生；`CS-25f` 声称「静默退出」无输出验收）—— R47 关闭 | 无 | 修订历史 2026-11-18 15:31:58 UTC 行 |
| R47 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `1f905f02152afc821ed36aec9e4123e7c202d156bb07747c08df8ebf76e6fe00` | `FAIL` | 5 项（`CS-02` 未验 `commit.gpgSign=true` 单独生效、`CS-03g` 未验 `gpg.minTrustLevel` 与 marginal 门取更严者、`CS-19` 成功路径 op-log 未验、`CS-25` 未验不读取 `maintenance.loose-objects.auto`、`CS-15g` 声称的错误码映射无验收）—— R48 关闭 | 2 项 P2（`CS-25i` 钩子未列入 `CLAUDE.md`、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 15:38:03 UTC 行 |
| R47 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 2 项（SNAP helper 自检独立验证门未计入 `CS-01a` G-03、收口全量门未写明在全部卡完成后运行）；B 1 项（同 A 第 1 项）；C 2 项（`CS-17` 句式只在 UTC 下验收、`CS-09a` 未验收 stdin 立即 EOF）；D 1 项（`CS-18c` 声称「当且仅当」却遗漏 Git cherry-pick 续提路径）—— R48 关闭 | 无 | 修订历史 2026-11-18 15:38:03 UTC 行 |
| R48 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `a61e6466c23dfb4a37f5d58c9d290c3ab67643cce7221cb48c22e273f485123b` | `FAIL` | 2 项（`REL-CS-02` 前三张子卡序号仍为 `/3`；`CS-22a` AC5、`CS-25i` AC5 为复合断言）—— R49 关闭 | 2 项 P2（`CS-02a` AC5 夹具未写明密钥来源 —— R49 处理；`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 15:41:32 UTC 行 |
| R48 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 1 项（`REL-CS-02` 子卡序号分母不一致）；B 2 项（`CS-03j` 非零退出夹具未满足其余成功条件、无行内伪造 `SIG_CREATED` 的夹具）；C 3 项（`CS-15d` 未验收显式 `--only` 预览、`CS-09a` 未验收选择性 `revert` 的成功提交、同 A 第 1 项）；D 2 项（`CS-18c` `-C` 未验日期值且缺 `-c`、`CS-22a`/`CS-25i` 复合 AC 计数）—— R49 关闭 | 无 | 修订历史 2026-11-18 15:41:32 UTC 行 |
| R49 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `5ae900be9f5456408a0f7f0b183ffccf1d401dc48bf580d6f87f831bbff70c0c` | `FAIL` | 3 项（`CS-03b` 非零退出夹具未写出有效签名、`CS-21a` AC3 与 `CS-24` AC2/AC3/AC8 为复合断言）—— R50 关闭 | 1 项 P2（`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 15:47:16 UTC 行 |
| R49 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 1 项（SNAP 未含 Libra 实际存储 notes 的 SQLite `notes` 表，误把 notes 当作 `refs/notes` ref）；B 3 项（`CS-08` 缺 `--short`/`--porcelain` 相互覆盖、`CS-03h` 未验 `check-novalidate` 与 `verify` 的 `-Overify-time`、`CS-03c` 把缺失/不可读 allowed signers 的 Git 结果误述为非 `Good`）；C 1 项（`CS-15c` 未覆盖非 UTF-8 路径）；D 3 项（`CS-19a` CAS 竞争窗口未验、`CS-25d`「最终结果相同」验收不足、`CS-25f` 生产默认上界未验）—— R50 关闭 | 无 | 修订历史 2026-11-18 15:47:16 UTC 行 |
| R50 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `5dd3e0c97e36dfe8638e0da676ccd7bbc9d450774da9a5bbda16542e7a39a968` | `FAIL` | 1 项（`CS-19a` 标题、axis 及家族各处仍称 CAS，与已撤回的声称不一致）—— R51 关闭 | 2 项 P2（SNAP 未覆盖其它 SQLite 表为残余风险、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 15:50:13 UTC 行 |
| R50 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（A、C 区 `PASS`） | B 1 项（`CS-03c` 混淆 allowed signers 未配置与路径不存在两种 Git 路径）；D 2 项（`CS-19a` 基线时点与 AC3 交错矛盾、`REL-CS-08` 四子卡缺 A 组共享回归门）—— R51 关闭 | A 区 1 项 P2（`CS-19a` 摘要仍称 CAS）、D 区 1 项 P2（`CS-19`/`CS-25a` 仍写「三卡」）—— R51 处理 | 修订历史 2026-11-18 15:50:13 UTC 行 |
| R51 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `4dc4e81a1e1ee6b93029c259411913f2341c513305f5c8dd90943f2877f7e500` | `PASS` | 无 | 2 项 P2（SNAP 未覆盖其它 SQLite 表、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 15:52:38 UTC 行 |
| R51 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（A、C 区 `PASS`） | B 1 项（`CS-08` 的 `--short`/`--porcelain` 在 Libra 走同一输出，AC7/AC8 无法区分先后）；D 1 项（`CS-25f` 释放场景使用测试覆盖值，未验证生产默认配置下的等待与继续执行）—— R52 关闭 | 无 | 修订历史 2026-11-18 15:52:38 UTC 行 |
| R52 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `2f9cb62fb8f4d49f3973624f914701fc5325e95c192b12051542428995b09fef` | `PASS` | 无 | 2 项 P2（SNAP 未覆盖其它 SQLite 表、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 15:55:58 UTC 行 |
| R52 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（A、B 区 `PASS`） | C 2 项（`CS-09c` AC1 的 SNAP-EQ 会拒绝 Git 式 `update` 已写入 blob 的正确实现、`CS-15c` 声称的非法 UTF-8 拒绝未覆盖 NUL 模式）；D 1 项（`CS-20` 用户可见行为修复未同卡交付命令文档与网站）—— R53 关闭 | 无 | 修订历史 2026-11-18 15:55:58 UTC 行 |
| R53 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `7eefdce74c40b041654c3d90a4db45c87309feec1372733c7f28833a9bb2da60` | `PASS` | 无 | 2 项 P2（SNAP 未覆盖其它 SQLite 表、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 15:59:22 UTC 行 |
| R53 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL` | A 1 项（D-1 只允许在 `COMPATIBILITY.md` 既有行追加，无法修正被推翻的旧陈述）；B 1 项（`CS-03a` 声称失败判定却由 `CS-03j` 测试，违反 G-02）；C 2 项（`CS-15c` 只验八进制转义、`CS-09a` 未验预先暂存内容保留）；D 3 项（`CS-22` 未验 `--no-verify` 下 `post-commit`/`post-rewrite` 仍运行、`CS-19` concatenate 场景无全量比较、`CS-25f` 生产默认等待无下界）—— R54 关闭 | 无 | 修订历史 2026-11-18 15:59:22 UTC 行 |
| R54 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `ed73fffd7f6a61fa9928a6e30ca3e6cd3ce2a43d951c0274758fbb6b655cb6fb` | `FAIL` | 1 项（`CS-22` 改变 `--no-verify` 语义与 `compat_libra_hooks_lifecycle` 固定的完整逃生阀设计冲突，且不在 `#767`/`#778` 验收内）—— R55 关闭（撤回 `CS-22`） | 2 项 P2（SNAP 未覆盖其它 SQLite 表、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 16:03:51 UTC 行 |
| R54 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（A、B 区 `PASS`） | C 2 项（`CS-15c` 八进制转义只测 ASCII、`CS-09a` 选择性 revert 未全量比较主索引）；D 1 项（`CS-22` 未验 `--no-verify` 下 `prepare-commit-msg` 的改写与失败，且既有共享回归仍断言 `--no-verify` 跳过全部 hooks）—— R55 关闭 | 无 | 修订历史 2026-11-18 16:03:51 UTC 行 |
| R55 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `b18e2fff4f89e2e9e52a4766a249fa7abb1db2478f4150589e3aafac93f44fd0` | `PASS` | 无 | 2 项 P2（SNAP 未覆盖其它 SQLite 表、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 16:06:01 UTC 行 |
| R55 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（A、B 区 `PASS`） | C 1 项（`CS-15g` 的 CLI 成功用例未覆盖非 NUL 引号解码）；D 1 项（`CS-25g` AC1/AC4 的静默断言可被命令失败满足）—— R56 关闭 | 无 | 修订历史 2026-11-18 16:06:01 UTC 行 |
| R56 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `c0542caeccb3bdf486009c86f1e4a387179b0982238d51cc47cc1e657fd32d03` | `FAIL` | 1 项（`CS-03j` 声称 stderr 原样透传而 AC 只验包含）—— R57 关闭 | 2 项 P2（SNAP 未覆盖其它 SQLite 表、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 16:08:14 UTC 行 |
| R56 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（A、D 区 `PASS`） | B 1 项（同 Claude：`CS-03j` stderr 原样透传无验收）；C 1 项（`CS-09a` `add untracked` 只验路径存在、未验内容）—— R57 关闭 | 无 | 修订历史 2026-11-18 16:08:14 UTC 行 |
| R57 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `8fa57921be695fe61b0a2ac269ef5830ae93246fc9ccb6c7adcb8764d5457d18` | `PASS` | 无 | 2 项 P2（SNAP 未覆盖其它 SQLite 表、`REL-CS-01` AC 12/12） | 修订历史 2026-11-18 16:10:29 UTC 行 |
| R57 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `FAIL`（B、C、D 区 `PASS`） | A 1 项（SNAP ⑥ 以 `libra config list` 采集，加密值被显示为 `<REDACTED>`，测不出密文被改写）—— R58 关闭 | B 区 1 项 P2（`CS-02a` 无签名配置夹具可写得更明确） | 修订历史 2026-11-18 16:10:29 UTC 行 |
| R58 (Claude subagent claude-opus-5-5) | 全文，受审 SHA-256 `fdc9dc310d3e08a5cef5d2f56ffbca8153e2f90f508b1273a38fa7094b9621a1` | `PASS` | 无 | 3 项 P2（SNAP 未覆盖其它 SQLite 表；`REL-CS-01` AC 12/12；`CS-09a` AC3/AC4 未以含 `s` 的完整期望树比较） | 修订历史 2026-11-18 16:12:51 UTC 行 |
| R58 (Codex gpt-6.1-terra，分区 A/B/C/D) | 同一 SHA-256 | `PASS`（A、B、C、D 四区均 `PASS`） | 无 | C 区 1 项 P2（`CS-09a` AC3/AC4 可加完整提交树比较） | 修订历史 2026-11-18 16:12:51 UTC 行 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| `DEFER-CS-01` | `.git/hooks` 与 `core.hooksPath` 兼容执行 | 遵循 PD-08 沙箱安全边界 | 另有安全 ADR 批准时 | `COMPATIBILITY.md` hooks 行 |
| `DEFER-CS-02` | `libra bisect visualize -p -- <path>`（#757 下游；`REL-CS-03` 只完成 commit 侧部分提交，#757 在本项落地前保持部分完成、不关闭） | `Bisect::View` 属 `bisect` 子命令范围 | `bisect` 专项计划立项时 | `plan-long.md` |
| `DEFER-CS-03` | `libra log/show --show-signature`（#783 下游，含 `show --show-signature` 生成 `verify.file`；`CS-03f`/`CS-03a`/`CS-03j`/`CS-03d`/`CS-03g`/`CS-03l` 只完成 `gpg.program` 解析、签名、验签与 merge 信任门，#783 在本项落地前保持部分完成、不关闭） | `log`/`show` 展示选项属独立行为轴 | `log`/`show` 签名展示计划立项时 | `plan-long.md` |
| `DEFER-CS-04` | `parse_date`（`src/internal/log/date_parser.rs`，`log`/`reflog`/`rev-list`/`shortlog`/`archive` 共用）与 `parse_expiry_date` 统一复用 `src/utils/approxidate.rs` 的提交日期入口（approxidate 自然语言与无时区 ISO） | 本计划的 #769/#771 只涉及 `commit --date`；改变五个共享调用方与 `config --type=expiry-date` 的接受集合是独立行为轴，需各自的回归与文档 | 有用户需求或 Git 兼容用例要求这些命令接受同一日期语法时 | `plan-long.md` |
| `DEFER-CS-06` | `commit --date` 的其余 Git approxidate 表达式（点分或空格分隔的相对日期如 `2.weeks.ago`、星期与自然语言如 `last friday`）；`CS-17` 只接受 t7501 句式（编号 `DEFER-CS-05` 已在 R30–R44 期间删除，不复用） | `#769` 的验收只要求 t7501#58 句式；完整 approxidate 语法是独立行为轴，需各自的解析、回归与文档 | 有用户需求或新的跟踪 issue 要求 `commit --date` 接受这些表达式时 | `plan-long.md` |
| `DEFER-CS-07` | cherry-pick 冲突续提时 `libra commit` 摘要的 ` Date:` 行（Git 在续提且未 `--reset-author` 时设置 `author_message = CHERRY_PICK_HEAD`，`builtin/commit.c:1365-1368`）；`CS-18c` 只覆盖 `--date` 与 `-C`/`-c`/`--amend` 复用作者 | `#771` 只要求强制日期的输出；续提路径依赖复用被拣选提交的作者，属 cherry-pick 行为轴 | cherry-pick 续提的作者复用语义对齐时 | `plan-long.md` |
| `DEFER-CS-08` | `commit --pathspec-from-file` 列表中的非 UTF-8 路径（Git 按字节处理）；`CS-15c` 的等价性限于 UTF-8 路径，含非法 UTF-8 字节的条目行为不作约束 | Libra 的 pathspec 模型以 UTF-8 字符串表示路径（`src/utils/pathspec/mod.rs:65`），支持字节路径是跨命令的独立行为轴；`#786` 的验收用例只含 ASCII 路径 | pathspec 模型支持字节路径时 | `plan-long.md` |
| `DEFER-CS-09` | `commit --no-verify` 对齐 Git（只跳过 `pre-commit` 与 `commit-msg`，仍运行 `prepare-commit-msg`、`post-commit`、`post-rewrite`） | Libra 把 `--no-verify` 设计为跳过全部 hooks（任意代码）的完整逃生阀，`tests/compat/libra_hooks_lifecycle_test.rs:163-172` 固定该行为；`#767`/`#778` 的验收不涉及 `--no-verify`；改变它需要安全设计决定 | 另有安全 ADR 批准改变 `--no-verify` 语义时 | `plan-long.md` |

本计划当前不预设任何任务卡延后。若某卡长期 `blocked`，须先修订本计划登记新的 `DEFER-*`（同步 Phase 退出条件、「完成判据」的卡数、里程碑与 issue 闭环口径）并通过计划评审，之后才可收口。

## 完成判据

- [ ] 所有任务卡满足 `G-*`；「任务卡粒度审计表」与各卡 `Granularity` 一致；无 L/XL、无未登记例外。
- [ ] 全部 83 张活动任务卡 `Lifecycle=done` 且 `Acceptance=complete`（ER-04）；无 `blocked`、无 `remote-pending`。
- [ ] 所有 Verification 已按 V-1..V-6 运行并按 V-4 记录；性能用例的双规模、逐次计时与增长比已实际通过并记录。
- [ ] 每张会推送的卡已跑 `cargo +nightly fmt --all --check` 与 `cargo clippy --all-targets --all-features -- -D warnings`；按 ER-13 命中全量触发的卡已在其最终树跑双环境完整 nextest。
- [ ] **计划完成门 / 全量收口门（ER-13）：** 在全部活动任务卡达到 `Lifecycle=done` 且 `Acceptance=complete` 之后，于最终代码树上运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿，并按 V-4 的被测提交 `T` 口径（运行前后三项检查、必要时在独立副本中运行）记录 `T` 的 `libra rev-parse HEAD` 与 `libra rev-parse 'HEAD^{tree}'`；其后只含本计划文件与 `docs/development/plan/plan-status.md` 的纯记账提交不触发重跑，其它任何文件变化都须在新树上重跑。
- [ ] **全量收口门暴露的 Bug 已全部处理（ER-13）：** 本计划导致的失败已前滚修复（必要时新建 `FIX-*` 卡并加依赖边）并重跑全量至全绿；判定为既有失败的用例已附计划基线提交上同样失败的复现证据并登记 `FIX-*` 或 `DEFER-*`。
- [ ] 各卡 D-1 文档门与 W-1..W-6 网站门（家族子卡在本卡内完成）均已完成并记录；网站 Claude Code `PASS` 与计划文本双审互不替代。
- [ ] 每张 `independent` 卡与每个家族发布点已按 ER-08 完成 `patch + 1`、版本面同步、`Cargo.lock` 工具链刷新、签名+DCO 提交、PR 合入与 `gh release create v<version>`，D 组全绿。
- [ ] 每张卡的 `Rollback mode` 已被验证或记录为不可验证的原因。
- [ ] 计划文本双审（Claude 与 Codex 对同一受审字节 X `VERDICT: PASS`）已由 PASS 后的纯记账行登记在「Codex review log」并引用 X；执行期各卡 ER-05 review 均为 `PASS`，P0/P1 全部关闭，P2 有具名接受人。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `#757`、`#783` 按「成功定义」保持部分完成未关闭；其余 issue 在对应卡 `complete` 后关闭。
- [ ] `plan-long.md` 相关状态已同步，或明确 `N/A`。
