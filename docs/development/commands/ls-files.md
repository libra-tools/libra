# `libra ls-files`

## 命令实现目标

`libra ls-files` 提供公开的 Git 兼容索引/工作树路径列举入口。当前目标是覆盖常用脚本和 AI 安全只读场景：缓存索引列表、已修改/已删除筛选、stage 样式输出、未跟踪文件列举、Git/Libra ignore 来源感知过滤、pathspec、`--error-unmatch`、`-z` 文本输出、`-t` 状态标签、`-u`/`--unmerged` 冲突条目筛选，以及标准 JSON / machine envelope。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：默认 cached listing、`--cached` / `-c`、`--deleted` / `-d`、`--modified` / `-m`、`--stage` / `-s`、`--abbrev[=<n>]`（在 `-s`/`--stage` 输出里把对象名截断为 n 位 hex，bare 即 7；取值用 `=` 形式 `require_equals`，故 bare 不会吞掉 pathspec；定长截断而非最短唯一前缀）、`--others` / `-o`、`--others --exclude-standard`、`-i` / `--ignored`（只列出被忽略的集合：`-i -o` 列出被忽略的未跟踪文件——`-o` 的反集；`-i -c` 列出匹配 exclude 模式的已跟踪文件；二者按 per-file exclude 判定（custom `-x`/`-X` 优先于 standard Git/Libra ignore 来源）；要求配 `-o`/`-c` 且需 `--exclude-standard` 或显式 `-x`/`-X` pattern，否则退出码 128，与 git 一致）、`<pathspec>...`（经 `utils::pathspec::PathspecSet` 支持普通路径/目录前缀、默认通配符、`:(top)`、`:(exclude)`、`:(icase)`、`:(literal)`、`:(glob)`）、`--error-unmatch`（任一正向 pathspec 无匹配时退出 1 并保留 `LBR-CLI-003` 诊断）、`-z`、`-t`（状态标签 H/R/C/?/M）、`-u` / `--unmerged`（仅冲突条目）、`--full-name`（接受为 no-op；Libra 始终输出仓库根相对路径）、`-x`/`--exclude <pattern>` 与 `-X`/`--exclude-from <file>`（显式 exclude pattern 源，gitignore 语法；过滤 `--others` 列表并计入 `-i` 的 ignored 集；经 `util::build_exclude_matcher` 编入内存 `Gitignore` 匹配器）、`--eol`（每个 cached 条目前缀行尾信息 `i/<eol> w/<eol> attr/<attr>`：`<eol>` 为 index blob（`i/`）与工作树文件（`w/`）的 `lf`/`crlf`/`mixed`/`none`/`-text`，经 `classify_eol` 判定；行格式 `i/%-5s w/%-5s attr/%-17s\t` 与 git 字节一致；line-ending attribute 报告尚未实现，故 `attr/` 恒为空；尊重 `-z`）、`--json` 和 `--machine`。
- 语义说明：pathspec 从调用者当前工作目录解析；`:(top)` 强制从仓库根解析；解析到仓库外的 pathspec 会被拒绝。
- 暂未公开：resolve-undo、killed/debug output、sparse-checkout integration。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::LsFiles` 公开顶层命令，dispatch 到 `src/command/ls_files.rs::execute_safe`。
- 源码分层：参数模型为 `LsFilesArgs`；结果条目由 `FileEntry` 表示；输出统一走 `OutputConfig`、human text、`--json` 和 `--machine` 路径。
- 执行路径：命令只读加载 `.libra/index`，按 state filter 和 pathspec 收集索引/工作树条目。`--modified` 对工作树文件计算 blob hash 并与索引 hash 比较；`--others` 扫描工作树并可通过 `--exclude-standard` 套用 Git/Libra ignore 来源。
- 副作用边界：该命令不得写入索引、对象库、refs、reflog、SQLite/D1、工作树或远端；AI/MCP `run_libra_vcs ls-files` 也按只读命令分类。

## 实现历史

- 2026-06-13 `8d4fb969`：引入基础索引列举实现轮廓。
- 2026-06-20 PR #415：公开 `ls-files` 顶层命令，补齐 pathspec、`--error-unmatch`、`-z`、AI/MCP 只读安全覆盖、用户文档和兼容矩阵。
- 2026-07-09（plan-20260708 P0-06）：直接 stdout 写入改走全局 `stdout_write_error` 映射，下游提前关闭管道时静默正常终止，不打印 panic/backtrace/`Broken pipe` 诊断。回归覆盖：`compat_broken_pipe_output`。
- 2026-07-09（plan-20260708 P0-11）：`--deleted` / `--modified` 的工作树存在性判断改用 `symlink_metadata`，dangling tracked symlink 不再被误列为 deleted；symlink target 变化继续按 blob hash 差异列为 modified。回归覆盖：`compat_symlink_basic`。
- 2026-07-09（plan-20260708 P1-01）：`ls-files` pathspec 过滤切到共享 `src/utils/pathspec/`，新增 `top`/`exclude`/`icase`/`literal`/`glob` magic 与子目录相对语义守卫。回归覆盖：`compat_pathspec_magic`。
- 2026-07-09（plan-20260708 P1-03）：`--error-unmatch` 的 unmatched positive pathspec 从 Libra CLI usage 退出改为 Git-like 退出 1，同时保留 `LBR-CLI-003` 诊断与提示。回归覆盖：`compat_machine_porcelain_contract`。

## 当前状态

- 公开状态：已公开。
- 用户文档：`docs/commands/ls-files.md` 和 `docs/commands/zh-CN/ls-files.md`。
- 兼容矩阵：`COMPATIBILITY.md` 顶层命令表登记为 `partial`。
- P0-01 后，`ls-files -t` 与 `-u` 都会遍历 unmerged stage 1/2/3：`-u` 输出 stage-style 行，`-t` 对每个冲突 stage 输出 `M <path>`，不再因默认 stage 0 视图隐藏冲突路径。回归测试：`compat_conflict_status_diff`。
- P0-11 后，tracked symlink 由 `symlink_metadata` 判定存在，`ls-files --deleted` 不会把 dangling symlink 当作缺失；`--modified` 通过 symlink target bytes 对比 index blob。
- P1-01 后，普通 pathspec、默认通配符和 `:(top)` / `:(exclude)` / `:(icase)` / `:(literal)` / `:(glob)` magic 均由共享 matcher 处理；`--error-unmatch` 只检查正向 pathspec，exclude-only pathspec 不触发未匹配错误。
- P1-03 后，`--error-unmatch <missing>` 对未匹配的正向 pathspec 退出 1；stderr 继续携带 `LBR-CLI-003` 以保持 Libra 稳定错误码契约。
- 回归测试：`tests/command_test.rs` 的 `command::ls_files_test::` 覆盖 CLI 行为；`tests/ai_libra_vcs_safety_test.rs` 覆盖 AI/MCP 只读安全；compat 文档测试覆盖 help、用户文档和命令索引同步。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| ✅ 已实现 | 状态标签 `-t` | 在每行路径前加状态标签（`H`=cached、`R`=removed/deleted、`C`=modified/changed、`?`=other/untracked、`M`=unmerged），由 `status_tag(&FileEntry.status)` 映射，格式与 `git ls-files -t` 一致。Libra 不建模 skip-worktree/killed，故不产出 `S`/`K`。带集成测试（`ls_files_t_prefixes_status_tags`）。 |
| ✅ 已实现 | `-u` / `--unmerged` | 仅列出冲突（stage 1/2/3）条目，输出 stage 样式（`<mode> <hash> <stage>\t<path>`），与 `git ls-files -u` 一致；冲突条目 `status` 现统一为 `unmerged`（stage>0），`-t` 下标为 `M`。带集成测试（`ls_files_u_shows_unmerged_conflict_entries`，经真实 merge 冲突构造）。 |
| ✅ 已实现（intentionally-different） | `--full-name` | 接受 Git 的 `--full-name` 标志为 no-op：Libra 的 ls-files 始终输出仓库根相对路径（即 Git `--full-name` 的形式），不按 cwd 子目录裁剪，因此该标志不改变行为，仅为脚本兼容而接受。带集成测试（`ls_files_full_name_accepted_as_noop`）。 |
| ✅ Ignored listing | `-i`/`--ignored`（`-i -o` 被忽略未跟踪、`-i -c` 匹配 exclude 的已跟踪；要求 `-o`/`-c` 且需 `--exclude-standard` 或显式 `-x`/`-X` pattern）已实现，带集成测试 `test_ls_files_ignored`。 | 与 git 一致。 |
| ✅ Explicit exclude source | `-x`/`--exclude <pattern>`、`-X`/`--exclude-from <file>` 已实现。`collect_ls_files_exclude_patterns` 收集 inline `-x` + 各 `-X` 文件的非空非注释行，`util::build_exclude_matcher` 编为内存 `Gitignore`；`--others` 列表丢弃匹配项（叠加 `--exclude-standard`），`-i` 模式下 custom pattern 计入 ignored 集（`-i` 可仅用 `-x`，无需 `--exclude-standard`）。`util::exclude_matcher_verdict` 返回三态 `Option<bool>`（`Some(true)`=排除、`Some(false)`=显式 `!` 重纳、`None`=无 custom 匹配），自顶向下走祖先目录实现 git 的 parent-dominance（父目录被排除后子项不能被 `!` 重新纳入）。source 优先级 `-x` > `-X` > standard Git/Libra ignore 来源：`-X` 文件行先、inline `-x` 后（last-match-wins）；`is_excluded` 先看 custom verdict，仅 `None` 才回落到 `--exclude-standard` 的标准来源，故 inline `-x !pat` 可覆盖 `.gitignore`/`.libraignore`。带集成测试 `test_ls_files_exclude_pattern_and_file`、`test_ls_files_ignored_with_custom_exclude`、`test_ls_files_exclude_directory_pattern`、`test_ls_files_exclude_parent_dominance`、`test_ls_files_exclude_inline_overrides_file`。 | 与 git 一致。 |
| ✅ 已实现 | `--eol`（行尾信息列） | 已公开：`eol_column` 为每个条目生成 `i/%-5s w/%-5s attr/%-17s\t` 并在渲染时插到 path 之前（与 `-t`/`-s`/`--stage` 组合：列位于 tag/stage 前缀之后、path 之前，与 git 一致）。`classify_eol` 复刻 git `convert.c` text-stat：**二进制（`-text`）** = 含 NUL ∥ lone CR（不构成 CRLF 的 CR）∥ 非可打印字节过多（`printable>>7 < nonprintable`，BS/TAB/ESC/FF 计为可打印）；否则 CRLF+lone-LF→`mixed`、仅 CRLF→`crlf`、仅 lone-LF→`lf`、无→`none`。i/ 取 index blob（`load_object::<Blob>` by hash），w/ 取工作树文件（`fs::read`），缺失留空；`attr/` 当前恒空（line-ending attribute 报告尚未实现，尽管 attributes 来源已用于 filter/diff/export-ignore）；尊重 `-z`。与 `git ls-files --eol`（含 `-t --eol`/`-s --eol`）字节一致。带集成测试 `test_ls_files_eol_classifies_line_endings`（含 lone-CR/控制字符二进制判定 + `-t`/`-s` 组合）。 |
| Resolve metadata | resolve-undo、killed/debug output 未公开。 | 继续列为兼容缺口。 |
| Sparse checkout | 未接入 Git sparse-checkout 语义。 | Libra 当前不维护对应状态；需要单独设计后再公开。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)。
- 行为变更必须同步 `COMPATIBILITY.md`、`docs/commands/ls-files.md`、`docs/commands/zh-CN/ls-files.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON / machine 输出契约、AI/MCP 安全分类和回归测试。
