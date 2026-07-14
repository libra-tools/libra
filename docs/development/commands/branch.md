# `libra branch` 开发设计

## 命令实现目标

`libra branch` 的目标是列出、创建、删除、复制、重命名和管理本地分支及其上游信息。实现需要适配 Libra 的 SQLite refs 存储，保护锁定分支，支持过滤和描述信息，并对 Git 中尚未实现或被接受但忽略的排序/格式参数明确标注。

## 对比 Git 与兼容性

- 兼容级别：`partial`。创建、列出、删除、重命名、复制（`-c`/`-C`/`--copy`，连同上游配置，保留源分支）、上游设置/清除、contains/no-contains、points-at（共用严格 revision resolver；附注 tag/完整 tag ref 递归 peel 到 commit）、merged/no-merged、`--sort`（`refname`/`version:refname`/`committerdate`/`creatordate`/`authordate`/`objectsize`——日期键按 tip 提交的 committer 时间（`authordate` 为 author 时间）升序、`objectsize` 按 tip 对象字节大小升序、`objectname` 按 tip 提交对象 id 字典序，可加 `-` 反转，相等按 refname tiebreak；commit/对象加载失败退化为 0）、ignore-case、`--column[=<always|auto|never>]`/`--no-column`（列式列表布局；`--no-column` 撤销先前的 `--column`，last-wins，默认每行一个故单独为 no-op）、`-v`/`--verbose`（每个分支附带 tip sha 与提交 subject；`-vv` 额外显示上游 tracking 段 `[<upstream>: ahead N, behind M]`）和 `--edit-description [<branch>]`（在配置的编辑器中编辑 `branch.<name>.description`，空/仅注释的缓冲会清除该描述；默认当前分支，detached HEAD 无分支参数则报错）和自定义 `--format=<fmt>`（经 for-each-ref atom 引擎渲染每个分支，复用 `for_each_ref::render_ref_format_lines`，取代默认 `* name` 列表及 `-v`/`--column`）已支持；for-each-ref sort key 已覆盖 `authordate`/`objectsize`/`objectname`。

- 当前矩阵承诺常用 Git 行为已支持；新增语义必须同步矩阵、用户文档和测试。


## 设计方案

- 入口与分发：已公开接入 `src/cli.rs::Commands`；已由 `src/command/mod.rs` 导出。CLI 层在 `src/cli.rs` 把解析后的参数交给命令模块，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/branch.rs`。参数/子命令类型包括：`BranchArgs`；输出、错误或状态类型包括：`BranchOutput`、`BranchListEntry`；主要执行函数包括：`execute`、`execute_safe`、`set_upstream`、`set_upstream_safe`、`set_upstream_safe_with_output`、`create_branch`、`create_branch_safe`、`list_branches`、`filter_branches`、`is_valid_git_branch_name`（其中 `set_upstream_safe_with_output` 被 `switch`/`push`/`checkout` 等其他命令复用；命令级 `list_branches` 仅为向后兼容的便捷封装，生产代码中除 `branch.rs` 自身外没有调用方，只被测试代码 `tests/command/checkout_test.rs` 调用——switch/push/checkout 等命令使用的是 store 层的 `Branch::list_branches_result`，而非该命令级函数）。
- 源码意图：源码模块注释说明该命令由 `run_branch` 分发到创建、删除、列表、重命名和上游跟踪 helper，并把 branch store 错误映射为命令级错误。
- 执行路径：`execute_safe` 负责 CLI 安全包装、错误映射和输出配置；核心领域逻辑集中在 `set_upstream_safe`、`create_branch_safe`；对象路径会解析 revision 并读写 blob/tree/commit/tag 等对象；引用路径会读取或更新 SQLite refs、HEAD 与 reflog；数据库路径会通过 SeaORM/SQLite 或 D1 客户端持久化元数据。

- 流程图：以下流程图按当前源码分层展示主路径和底层对象边界，便于维护者把代码入口、执行函数和副作用范围对应起来。

```mermaid
flowchart TD
    A["入口与分发<br/>src/cli.rs::Commands"] --> B["源码分层<br/>src/command/branch.rs"]
    B --> C["参数模型<br/>BranchArgs"]
    C --> D["执行路径<br/>execute / execute_safe / set_upstream_safe"]
    D --> E["底层对象<br/>Commit / Branch / Head / .libra/libra.db"]
    D --> F["输出与错误<br/>BranchOutput / BranchListEntry"]
    E --> G["副作用边界<br/>写入分支需先预检"]
```

- 底层操作对象：`Commit`（提交对象、父提交关系和提交消息载荷）；`Branch` / branch store（SQLite refs 上的分支读写、过滤和上游关系）；`Head`（SQLite 中的 HEAD 指向、当前分支和 detached 状态）；SeaORM / `.libra/libra.db`（配置、refs、reflog、AI/发布元数据等 SQLite 表）；`ObjectHash`（SHA-1/SHA-256 对象 ID 和 revision 解析结果）；`ConfigKv`（配置键值持久化行）
- 输出与错误契约：人类输出、`--json` / `--machine` 输出和 quiet/verbose 分支必须继续走现有 `OutputConfig` / `emit_json_data` / `CliError` 路径；新增失败模式要补稳定错误码、用户提示和回归测试。
- 副作用边界：凡是写入索引、对象库、refs/HEAD、reflog、SQLite/D1、工作树或远端的路径，都必须先完成参数校验和 dry-run/预检分支，再执行持久化，避免部分写入后静默成功。

### branch diff（lore.md 1.12）

- 纯糖层：`execute_diff_safe` 解析默认值（subject=当前分支，base=`branch.<n>.remote/.merge` 上游）→ 未知侧转 `branch_not_found_error`（建议）→ 组装 argv（两点走 `--old/--new` 免歧义步行；`--merge-base` 三点粘连复用引擎 merge-base）→ `diff_plumbing::delegate_to_diff`（共享抽取；plumbing 保留其 128 解析覆盖，branch 走默认 129）。与 `diff A..B` 字节一致由测试钉住。保留字防护：clap `args_conflicts_with_subcommands` 在有 flags 时**不报错而是回落位置参数**（审阅 spike 实证），故 execute 处 `new_branch=="diff"` 一律拒绝（129 + 逃生口提示），绝不静默创建。

### branch reset（lore.md 1.13）

- 首个策略层消费者：`with_operation_log` 单事务内 fail-closed 重查 `is_protected_with_conn`/`is_archived_with_conn`（1.5 合同；垃圾值即受保护；元数据读错误即拒绝回滚）+ 重查 `Head::current_with_conn`（并发 switch 竞态，审阅 must-fix）→ `update_branch_with_conn` + `Reflog::insert_single_entry`（update-ref 先例——不经 `insert` 以免伪造 HEAD 条目；未调 ensure_reflog_table_exists：bootstrap SQL 建表，遗留缺表则整体事务 fail-closed——刻意决定）。哨兵字符串（LIBRA_POLICY_*）穿透 DbErr::Custom 在事务外还原类型化错误，保住 LBR-POLICY-001。无 `--force`（风险矩阵 lore.md:301：阻断+明确错误；旁路走显式 metadata unset，可审计）。**update-ref 同步纳管**（否则策略旁路）；其保持 plumbing 语义（可动 checked-out 分支，git 对齐，已在 COMPATIBILITY 注明）。main 允许 reset（默认分支锁保护删除/改名身份而非尖端移动；`libra reset` 本就可动 checked-out main）——可翻转的刻意决定。同参 5s 去重窗（operation-log 全局语义）拒绝重复 reset，测试钉住。未竟：拒绝事件接入 §7.8 审计 sink（现为 tracing::warn）；`--expect` CAS 延后（`update-ref <ref> <new> <old>` 已覆盖脚本化 CAS）。

## 实现历史

- 本节依据本地 main 分支提交历史重写，筛选与该命令实现、测试或文档路径直接相关的提交；以下是归纳后的实现脉络。
- 2025-11-19 `256bfe62`（`feat: add -all  subcommands for branch command (#58)`）：基础实现节点：add -all  subcommands for branch command (#58)；当前实现的主要轮廓可追溯到该提交。
- 2026-06-06 `7e94b815`（`feat(switch): add -C/--force-create (create or reset branch then switch)`）：功能演进：add -C/--force-create (create or reset branch then switch)；该节点扩展了当前命令可用的参数或行为。
- 2026-06-04 `f54123ea`（`feat(branch): decline --track/--no-track, stub --sort/--format, mark compatibility partial [decision-reversal supported->partial] (v0.17.1296)`）：功能演进：decline --track/--no-track, stub --sort/--format, mark compatibility partial [decision-reversal supported->partial] (v0.17.1296)；该节点明确拒绝了 `--track/--no-track` 并仅对 `--sort/--format` 作 stub 标注，当时未新增可用参数。**后续演进**：`--sort` 已实现 `refname`/`version:refname` 及 `committerdate`/`creatordate`/`authordate`/`objectsize`/`objectname` 键（见顶部兼容级别与公开参数）；自定义 `--format=<fmt>` 也已实现（复用 for-each-ref atom 引擎，见缺口表“✅ 已实现”）。本文顶部兼容级别以 `COMPATIBILITY.md` 现行矩阵为准，当前仍为 `partial`。
- 2026-06-04 `07fbf023`（`fix(branch): launch editor via shlex (no shell), reject self-copy/self-rename, harden reflog timestamp (codex review r2) (v0.17.1298)`）：实现修正：launch editor via shlex (no shell), reject self-copy/self-rename, harden reflog timestamp (codex review r2) (v0.17.1298)；该节点把边界行为、错误处理或兼容差异纳入当前实现约束。
- 2026-07-11（plan-20260708 P1-05d，sort 片）：`branch.sort` 配置默认接入严格 local→global→system 级联（`configured_branch_sort`，`--sort` 优先）。刻意与 flag 的副作用区分：配置默认在 `has_commit_filters`/`show_unborn_head` 判定之后解析，因而不隐含 `--list`、不抑制 unborn-HEAD 行（Git 行为）。无效配置值 → `BranchError::InvalidSortConfig`（`LBR-CLI-002`，报错点名 `branch.sort`），读取失败 → `SortConfigRead`（`LBR-IO-001`），均在任何列表输出前。已记录收窄：重复配置值只应用胜出 scope 的最后一个（Git 叠成多键排序）。回归：`compat_config_defaults_semantics` 的 `branch_sort_config_orders_list_and_cli_wins`（含配置下创建分支、多值 last-wins）、`branch_sort_config_keeps_unborn_head_line`（unborn 仓库仍显示当前分支行）、`sort_config_read_failure_is_io_error_before_listing`（不可读 global 库 → LBR-IO-001 点名键）。
- 历史结论：当前文档应以这些提交之后的代码、测试和兼容矩阵为准；更早的迁移式文档只保留为背景，不再作为事实来源。

## 当前状态

- 公开状态：已公开；模块状态：已导出。
- 用户文档：`docs/commands/branch.md`。
- Synopsis：`libra branch [-l] [-r] [-a] [--contains [<commit>]] [--no-contains [<commit>]] [--points-at <object>] [--merged [<commit>]] [--no-merged [<commit>]] [--sort <key>] [--ignore-case]` / `libra branch [<new_branch>] [<commit_hash>]` / `libra branch (-d | -D) <branch>` / `libra branch -m [<old_branch>] <new_branch>` / `libra branch (-c | -C) [<old_branch>] <new_branch>` / `libra branch -u <upstream>` / `libra branch --unset-upstream [<branch>]` / `libra branch --show-current`。列表形式额外接受 `[--column[=<MODE>]]`、`[--no-column]` 与 `[-v | --verbose]`（可重复 `-vv`）。
- 公开参数/子命令包括：`[<new_branch>] [<commit_hash>]`、`-l, --list`、`-d, --delete <DELETE_SAFE>`、`-D, --delete-force <DELETE>`、`-u, --set-upstream-to <UPSTREAM>`、`--unset-upstream [<BRANCH>]`、`--show-current`、`-m, --move <OLD_BRANCH> <NEW_BRANCH>`、`-r, --remotes`、`-a, --all`、`--contains [<commit>]`、`--no-contains [<commit>]`、`--points-at <object>`、`--merged [<commit>]`、`--no-merged [<commit>]`、`--sort <key>`、`-c, --copy <OLD> <NEW>`、`-C, --copy-force <OLD> <NEW>`、`--ignore-case`、`--format <format>`、`--column[=<MODE>]`、`--no-column`、`-v, --verbose`（`ArgAction::Count`）。`--format=<fmt>`（query 组）在 List 人类输出下经 `for_each_ref::render_ref_format_lines` 渲染（取代 `* name`/`-v`/`--column`，JSON/quiet 不受影响）。`-v`/`--verbose`（verbose>=1）在 List 人类输出每行追加 ` <短sha> <subject>`（`branch_verbose_suffix` 经 `parse_commit_msg` 取首行 subject）；`-vv`（verbose>=2）在 sha 与 subject 之间额外插入上游段 ` [<upstream>: ahead N, behind M]`（`branch_upstream_segment` 读 `branch.<n>.remote`/`.merge` → `refs/remotes/<remote>/<merge-short>` 经 `get_target_commit` 解析 + `status::compute_ahead_behind`；remote-tracking ref 未 fetch 时省略计数仅显示 `[<upstream>]`，无上游时不插入）；render_branch_output/branch_verbose_suffix 为此改为 async；非列表动作与 JSON/quiet 不受影响；`-v` 优先于 `--column`（后者为纯名列布局）。`--column`（`always`/`auto`/`never`，bare 即 `always`；模式经 `tag::resolve_column_enabled` 校验、宽度经 `column_layout_width`）在 List 人类输出里用 `format_branch_columns` 把条目（current 分支带 `*` 前缀、纯名无颜色以保证列宽计算）按 column-major 排布；JSON/quiet 与非列表动作不受影响。`--no-column`（= `--column=never`）经 clap `overrides_with` 撤销先前的 `--column`（last-wins，读 `column` 字段，`no_column` 不直接读取），分支默认每行一个故单独为 no-op。`-c`/`-C`（`copy_branch_impl`）在 `<old>` 的提交处创建 `<new>` 并复制上游配置（`branch.<old>.remote`/`.merge`→`<new>`），保留源分支、不移动 HEAD；`-c` 在目标已存在时报 `AlreadyExists`，`-C` 覆盖。一参数形式复制当前分支。`--merged`/`--no-merged`（缺省 HEAD）复用 `log::get_reachable_commits` 计算目标可达集合，保留（或排除）tip 在该集合内（即已合并入目标）的分支，是 `--contains` 的反方向。`--sort <key>`（`refname`/`version:refname`/`v:refname`，加 `committerdate`/`creatordate`/`authordate` 日期键、`objectsize` 与 `objectname`，前导 `-` 反转）在 `collect_branch_output` 内由 `sort_branch_entries` 排序条目（人类与 JSON 输出一致，且不再按 current-first 排），未知 key 报 `LBR-CLI-002`；dash-leading 值需用 `--sort=-refname`。日期键：`sort_branch_entries` 先用 `ObjectHash::from_str`+`load_object::<Commit>` 把各 tip-commit hash 预载成 `HashMap<hash,i64>`（→`committer.timestamp`；加载失败→0），再按时间戳升序排序、refname tiebreak。


## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| ✅ 已实现 | 复制分支 `-c` / `-C` / `--copy` | `copy_branch_impl` 在源分支提交处创建目标并复制上游配置；保留源、不移动 HEAD；`-c` 目标存在则报错，`-C` 覆盖；一参数形式复制当前分支。带集成测试（`branch_copy_duplicates_branch_with_config`）。 |
| ✅ 已实现 | `--edit-description [<branch>]`（描述编辑）。`BranchArgs.edit_description: Option<String>`（`num_args=0..=1`，空=当前分支）；`edit_description_impl` 用 `editor::resolve_editor`+`edit_message` 打开 seeded（当前描述+注释块）缓冲，经 `clean_branch_description`（剥离 `#` 行并 trim）后写入/清除 `branch.<name>.description`。detached HEAD 无参数 → `detached_head_branch_error`；无编辑器且非 TTY → `BranchError::NoEditor`。带单元测试（`clean_branch_description_*`/`edit_description_flag_*`）+ 端到端 fake-editor 测试（`branch_edit_description_sets_then_unsets_via_editor`）。 |
| ✅ 已实现（部分） | 自定义格式与其余 sort key | 自定义 `--format=<fmt>` 已实现：`render_branch_output` 在 `--format` 下把（已排序的）分支映射为 `(refname, objectname)` 对——本地 `refs/heads/<name>`，远程（`plain_name != name`）`refs/remotes/<plain_name>`——交给 `for_each_ref::render_ref_format_lines`（复用全部 atom、`%(align)`/`%(if)`、`%(color)` 与尾部 reset），取代默认 `* name` 列表及 `-v`/`--column`，无 `--shell` 引用。带集成测试 `branch_format_renders_for_each_ref_atoms`。`--sort` 现支持 `refname`/`version:refname`/`committerdate`/`creatordate`/`authordate`/`objectsize`/`objectname`（`authordate` 用 `commit.author.timestamp`，`objectsize` 经 `objects_storage().get` 取 tip 对象字节大小预载，`objectname` 直接按 tip 提交 hex hash 字典序比较——等长 hex 与 git 二进制 oid 顺序一致；均与日期键一样升序+refname tiebreak，加载失败退化 0）。带集成测试 `test_branch_sort_by_committer_date`（含 authordate/objectsize/objectname 正反向断言，aaa 用长消息使其对象更大；objectname 用 `--format=%(objectname) %(refname:short)` 读 hash 后与 hash 排序期望对比，确定性）。 |
| ✅ 已实现 | 详细列表 `-v` / `-vv` / `--verbose` | `branch_verbose_suffix` 在 List 输出追加 ` <短sha> <subject>`；`-vv` 经 `branch_upstream_segment` 额外插入上游 tracking `[<upstream>: ahead N, behind M]`（复用 `status::compute_ahead_behind`）。带集成测试（`branch_verbose_shows_sha_and_subject` + `branch_vv_shows_upstream_segment`）。 |
| 跟踪设置 | `--track` / `--no-track` 已在 `f54123ea` 明确 decline，当前 `BranchArgs` 无对应定义。 | 已声明拒绝；不提供该参数。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
