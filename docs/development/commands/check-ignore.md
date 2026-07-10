# check-ignore 命令开发设计

## 命令实现目标

`libra check-ignore` 的目标是：对一组路径，按当前 Git/Libra ignore 来源判定哪些被忽略，并在 `-v` 下给出做出判定的来源文件、行号与模式。它是只读查询，不修改 index 或工作树，对齐 `git check-ignore` 的退出码契约（0=有路径被忽略、1=无、128=用法/仓库错误）。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持来源：`.gitignore`、`.git/info/exclude`、`core.excludesFile` 与 `.libraignore`。同一目录内 `.libraignore` 比 `.gitignore` 优先；更近目录来源优先于祖先；`.git/info/exclude` 与 `core.excludesFile` 为低优先级 fallback。匹配引擎/模式语法与 Git 相同（同一 `ignore` crate 引擎）。
- 已支持：`<pathname>...`、`--stdin`、`-z`（NUL 输入/输出分隔）、`-v/--verbose`（`<source>:<line>:<pattern>\t<path>`）、`-n/--non-matching`（需 `-v`）、`--no-index`（即使路径已被 index 跟踪也按纯模式匹配上报），以及全局 `--json`/`--machine`。
- 未公开：Git 的 `--exclude` / `--exclude-from` / `--exclude-per-directory` 与完整 pathspec magic。
- 行号说明：底层 `ignore` crate 的 `Glob` 暴露 `from()`（来源文件）与 `original()`（模式原文）但**不暴露行号**；`-v` 的行号由 `util::find_pattern_line` 重新扫描来源 ignore 文件、取**末个**去空白后等于该模式的非注释非空行（单文件内最后命中的模式才是裁决规则；best-effort，找不到则省略）。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::CheckIgnore(command::check_ignore::CheckIgnoreArgs)`，dispatch 到 `command::check_ignore::execute_safe`。CLI 名 `check-ignore`（clap 默认 kebab-case 重命名）。
- 源码分层：
  - `src/command/check_ignore.rs`：`CheckIgnoreArgs`（clap 派生）、`execute`/`execute_safe`、`CheckIgnoreEntry`/`CheckIgnoreOutput`（`--json` 序列化）、`classify_path`、`render`/`write_verbose`、`read_stdin_paths`。
  - `src/utils/util.rs`：`check_gitignore_match(work_dir, target) -> Option<IgnoreMatchInfo>` —— 与 `check_gitignore`（返回 bool）共享同一来源顺序（Git 标准来源 + `.libraignore`，同目录 `.libraignore` 高于 `.gitignore`，单文件内最后命中的模式胜出），但返回做出判定的 glob 的 `source/line/pattern/ignored`，保证判定结论与 `check_gitignore` 永不分叉。`IgnoreMatchInfo`、`find_pattern_line` 与 ignore source cache 为配套类型/函数。
- 执行路径：
  1. `util::require_repo()`（不在仓库 → 128）。
  2. 参数校验：`-n` 必须配 `-v`；`--stdin` 与位置路径互斥；二者都没有则报错（均 → 用法错误码）。
  3. 收集路径：位置参数或 `--stdin`（按 `-z` 用 NUL，否则换行分隔；去除空项与尾随 `\r`）。
  4. 除非 `--no-index`，加载 `.libra/index`；已跟踪路径按 Git 语义视为「未忽略」。
  5. 对每个路径：解析为绝对路径后用 `normalize_lexical` 折叠 `.`/`..`（不触碰文件系统），再校验仍位于 worktree 内——逃出 worktree 的路径是致命错误（exit 128，对齐 Git；并规避 `check_gitignore_match` 的 containment 前置，防止读取 worktree 外的 ignore 文件）。index 跟踪查询用归一化后的 repo 相对 key（绝对/相对输入一致）。`--stdin` 读取上限 64 MiB（超限 128）。
  6. 渲染：默认只打印被忽略路径；`-v` 打印来源/行/模式 + 路径；`-n` 追加未匹配路径（空字段）；`-z` 改用 NUL 分隔与终止。`--json` 走 `emit_json_data`。
  7. 退出码：任一路径被忽略 → `Ok(())`（0）；否则 `Err(CliError::silent_exit(1))`（exit 1、无输出）。
- 底层操作对象：`.libra/index`（`Index::load`，跟踪状态查询）、worktree 路径、Git/Libra ignore 来源匹配引擎（经 `util::check_gitignore_match`）。无对象库/refs/网络写入。
- 输出与错误契约：human / `--json` / `--machine` 经 `OutputConfig`；用法错误用 `CliError::command_usage` + `StableErrorCode::CliInvalidArguments`，仓库缺失用 `CliError::repo_not_found()`，无匹配用 `CliError::silent_exit(1)`（不复用现有错误码、不打印）。
- 副作用边界：纯读取；不写 index/对象/refs/工作树。

## 实现历史

- 2026-06-30（GGT-03，`docs/development/commands/grit-gap.md` 阶段 1）：新增 `check-ignore`。抽取 `util::check_gitignore_match` 复用既有 `.libraignore` 引擎（不复制匹配逻辑）；命令实现 `-v`/`-n`/`-z`/`--stdin`/`--no-index` 与 `--json`；同步 `COMPATIBILITY.md`、用户文档与集成测试。
- 2026-07-09（plan-20260708 P1-02）：ignore 来源扩展为 `.gitignore`、`.git/info/exclude`、`core.excludesFile` 与 `.libraignore` 并存，编译缓存按来源/基准目录分离；`check-ignore`、`status`、`add`、`clean` 和 `ls-files --exclude-standard` 共享同一判定。

## 当前状态

- 公开状态：已公开（`src/cli.rs::Commands::CheckIgnore`）。
- Synopsis：`libra check-ignore [-v] [-n] [-z] [--no-index] <pathname>... | --stdin`。
- 测试：`tests/command/check_ignore_test.rs`（位置/`--stdin`/`-v`/`-n`/`-z`/`--no-index`/退出码/`--json`），登记于 `tests/command/mod.rs`。
- 用户文档：`docs/commands/check-ignore.md`。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容差异项 | `--exclude` / `--exclude-from` / `--exclude-per-directory` | 不公开；命令仅查询标准 Git/Libra来源，命令行附加排除模式后续按需再评估并同步矩阵与测试。 |
| 兼容差异项 | 完整 Git pathspec magic | 仅支持普通路径；pathspec 引擎增强独立于本命令。 |
| 精度 | `-v` 行号为扫描重建（非引擎原生） | 取末个匹配行（裁决规则）；引擎暴露行号前维持 best-effort。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 任何行为变更先核对 `check_ignore.rs` / `util::check_gitignore_match`，再同步 `COMPATIBILITY.md`、`docs/commands/check-ignore.md` 与测试。
- 新增公开 flag 必须明确 tier、退出码、JSON/机器输出契约与回归测试。
