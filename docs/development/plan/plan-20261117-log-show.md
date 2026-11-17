# log 与 show Git 兼容补齐计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：计划已修订（R15），尚未执行；记录时间 2026-11-17 04:56:00 UTC，全部任务 pending。
> 跟踪范围：GitHub open issues #833、#857、#858、#862、#949–#956、#958–#970、#972–#973（共 27 项）。

## 文档职责

本文解决 `libra log` 与 `libra show` 相对于 Git（基线 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`，v2.56.0-116）的 27 项已登记兼容缺口，目标是使 `log` 与 `show` 在装饰解析与着色、遍历选项、`-L`/`--follow`、`--output`、TTY 控制序列、空格式串与 diff 渲染选项透传上与 Git 保持一致。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收（GC-01）。

### 适用范围

- 命令与 CLI：`libra log` 与 `libra show` 参数解析、装饰与格式化渲染、diff 选项透传（`src/command/log.rs`、`src/command/log/config.rs`、`src/command/log/decorate_filter.rs`、`src/command/show.rs`、`src/command/diff.rs`、`src/utils/pager.rs`、`src/cli.rs`）。
- 配置与环境变量：`log.decorate`、`log.excludeDecoration`、`log.initialDecorationSet`、`color.decorate.*`、`diff.color.commit`、`GIT_REPLACE_REF_BASE`。
- 测试与文档：`tests/command/log_test.rs`、`tests/command/show_test.rs`、`tests/command/diff_test.rs`（均属 `command_test` 目标）、`tests/compat/pretty_format_placeholders.rs`、`docs/commands/{log,show,diff}.md`、`docs/commands/zh-CN/{log,show,diff}.md`、`docs/development/commands/{log,show,diff}.md`、`COMPATIBILITY.md`（第 196、199、227 行原地更新）及 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/{log,show,diff}.en.md`。

### 非目标

- 不重复实现 `issues/534.md` 已完成并关闭的 `LO-00..LO-09` 能力；对 `#952` 与 `#958` 按 GC-01 核实剩余失败项后再修复。
- 不改变 `libra diff` 命令自身的默认参数与 JSON schema，不修改 `CommitDiffOptions`（避免触及 `src/command/format_patch.rs`）。
- 不实现 `--expand-tabs[=<n>]`、`--mailmap` 或 `--show-signature`（未在 27 项 issue 范围内）。

### 成功定义

- 27 个关联 issue 全部在「追溯表」与任务卡中闭环，对应行为与边界测试通过。
- `docs/commands/{log,show}.md`、`docs/commands/zh-CN/{log,show}.md`、`docs/development/commands/{log,show}.md`、`COMPATIBILITY.md` 与后端网站文档同步反映新增 flags 与 config。
- 全部任务卡完成后通过 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 及 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

## 使用规则

- 每次状态推进与「修订历史」写入都记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时间。
- 推进或完成任务卡时，执行者须在同一变更中同步更新 [`plan-status.md`](plan-status.md)（本计划入库时的初始登记行由协调者随本批计划合入一并登记）。
- 本文全部任务卡均未开工；任务 ID（`LS-01`..`LS-31`）固定，拆分时按 G-09 在所属 Phase 末尾追加新编号并登记「修订历史」。
- 本计划不单独新建证据目录或证据文件（v2.12），所有门禁结果、SHA 与测试摘要写在本计划内。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。以下锚点核对于 `2026-11-17 03:40:00 UTC`（`Cargo.toml` = `0.30.207`）。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `LogArgs`（172-486）、`determine_decorate_option`（1036-1075）、`apply_follow_and_line_filters`（1981-2028）、`CommitDiffOptions`（4254）；`ShowArgs`（73-187）、`show_commit`（580-707）；`diff.rs` 的 `parse_diff_filter`（2228）、`apply_diff_filter`（2265）、`run_textconv`（3403）当前均为私有；`log.rs` 当前使用 `git_internal::diff` 而非 `command::diff` | `src/command/log.rs:172-5070`、`src/command/show.rs:25-28,73-1810`、`src/command/diff.rs:129-511,2228,2265,2574,3403`、`src/command/log/{config,decorate_filter}.rs`、`src/utils/pager.rs` |
| 数据/状态 | 只读 commit/tree/blob/tag/notes/replace refs/config，不修改 SQLite/HEAD/index；`--output=<file>` 将新增文件写入目标路径 | `src/command/log.rs:1065,2188`、`src/command/show.rs:315` |
| 用户命令 | `Commands::Log(command::log::LogArgs)`、`Commands::Show(command::show::ShowArgs)` | `src/cli.rs:396,493,3481,3845,3898` |
| 机器输出 | `LogOutput { commits: Vec<LogCommitEntry>, total }`、`ShowOutput` enum（`commit`/`tag`/`tree`/`blob`） | `src/command/log.rs:542-576`、`src/command/show.rs:189-259` |
| 错误码 | 复用 `LBR-CLI-002`（`CliInvalidArguments`）、`LBR-CLI-003`（`CliInvalidTarget`）、`LBR-IO-002`（`IoWriteFailed`）、`LBR-REPO-001` | `src/utils/error.rs`、`docs/error-codes.md` |
| 文档 | 中英文命令页、开发文档及兼容矩阵第 196、199 行已存在（网站交付严格遵循 `AGENTS.md:135` 指定的授权生产检出 `/Volumes/Sea/libra-backend` `cf` 分支；路径不可用时 fail-closed 停止网站交付，除非用户明确授权替代检出；通过 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only <cmd> --libra-root /Volumes/Data/gitmono/libra` 同步） | `docs/commands/{log,show}.md`、`docs/commands/zh-CN/{log,show}.md`、`docs/development/commands/{log,show}.md`、`COMPATIBILITY.md:196,199` |
| 测试 | `tests/command/log_test.rs`（1074）与 `tests/command/show_test.rs`（1140）挂载于 `command_test`；compat：`compat_pretty_format_placeholders`、`compat_shallow_walk_helper`、`compat_rev_parse_peel_selectors` | `tests/command/mod.rs:1074,1140`、`Cargo.toml:500,572,604`、`tests/INDEX.md:74,81` |
| 外部参照 | 上游 Git 测试树 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0-116）；已完成计划 `issues/534.md`（`LO-00..LO-09` done/complete） | `docs/development/plan/issues/534.md:3`（2026-11-17 核对） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-LS-01 | `#949`：`log --decorate` 丢弃嵌套 annotated-tag 解引用链且 `%D`/`%d` 输出为空 | 嵌套 tag 无法显示装饰 | `src/command/log.rs:2327,2441` | LS-01 |
| GAP-LS-02 | `#950`：`log.decorate=true` 不启用装饰，且 `--pretty=raw` 受配置装饰 | 配置布尔值无效且 raw 格式污染 | `src/command/log.rs:1020-1075` | LS-02 |
| GAP-LS-03 | `#951`：忽略 `log.excludeDecoration`，无值 key 不报缺失值诊断 | 无法从配置排除装饰且非法配置静默 | `src/command/log.rs:646-672` | LS-03 |
| GAP-LS-04 | `#952`：`--decorate-refs` 无前缀通配、HEAD 过滤与 ref 顺序剩余失败（`534` 后仍 OPEN） | 与 Git 装饰过滤仍存在边界差异 | `src/command/log/decorate_filter.rs:15-55`、`issues/534.md:3` | LS-04 |
| GAP-LS-05 | `#953`：`log` 拒绝 `--simplify-by-decoration` | 无法按装饰简化历史图 | `src/command/log.rs:172-486` | LS-05 |
| GAP-LS-06 | `#954`：`log` 拒绝 `--clear-decorations` 且忽略 `log.initialDecorationSet` | 无法重置初始装饰集 | `src/command/log.rs:172-486` | LS-06 |
| GAP-LS-07 | `#959`：`log --decorate --color` 忽略 `color.decorate.*` 与 `diff.color.commit` | 装饰与提交头颜色不可配置 | `src/command/log.rs:1123` | LS-07 |
| GAP-LS-08 | `#960`：`log --decorate` 省略 `replaced` 标记且忽略 `GIT_REPLACE_REF_BASE` | 替换对象无装饰提示 | `src/command/log.rs:2441` | LS-08 |
| GAP-LS-09 | `#958`：`--no-abbrev` 在全对象名输出上仍有开放失败项（`534` 后仍 OPEN） | 部分输出路径仍被截断 | `src/command/log.rs:190-191`、`issues/534.md:3` | LS-09 |
| GAP-LS-10 | `#956`：`log` 拒绝 `--no-walk` 与 `--tags` | 无法仅展示指定提交或全部 tag | `src/command/log.rs:172-486` | LS-10 |
| GAP-LS-11 | `#965`：`log` 拒绝 `--before` / `--after`（仅 `--since`/`--until` 可用） | Git 时间过滤别名不可用 | `src/command/log.rs:228-233` | LS-11 |
| GAP-LS-12 | `#969`：`log` 拒绝 `-R`（issue 标题误称 `--reverse` 短别名，Git 实为反转 diff 输入方向） | 常用反向 diff 旗标报错 | `src/command/log.rs:172-486,4245-4275` | LS-12 |
| GAP-LS-13 | `#955`：`log` 在 TTY 下输出多余 `CR` / `ESC[m` / `CR ESC[K` 序列（涉及 `log.rs` 分页写入与 `pager.rs`） | 交互终端或伪终端测试输出脏字节 | `src/command/log.rs:2325,2667-2683`、`src/utils/pager.rs:95-103` | LS-13 |
| GAP-LS-14 | `#972`：`log` 忽略空 `--format=`/`--pretty=` 并打印默认提交头 | 与 Git 空格式输出空行不符 | `src/command/log.rs:234-240` | LS-14 |
| GAP-LS-15 | `#963`：`log` 拒绝 `--output=<file>` | 无法直接将 log 写入文件 | `src/command/log.rs:172-486` | LS-15 |
| GAP-LS-16 | `#962`：`log --follow` 要求接 `FILE` 参数而非布尔标志 | `libra log --follow -- <file>` 被拒绝 | `src/command/log.rs:469-475` | LS-16 |
| GAP-LS-17 | `#961`：`log` 接受 `-L` 但忽略输出与 pathspec/`--follow` 互斥校验 | 行级历史无有效输出与校验 | `src/command/log.rs:477-480,1894,1981` | LS-17 |
| GAP-LS-18 | `#968`：`log` 拒绝 `--find-object=<object>` | 无法按对象 OID 搜索历史 | `src/command/log.rs:172-486` | LS-18 |
| GAP-LS-19 | `#833`：`log` 拒绝 `--cc` | 无法查看合并提交的紧凑组合差异 | `src/command/log.rs:172-486` | LS-19 |
| GAP-LS-20 | `#964`：`log` 拒绝 `--raw`、`--numstat` 与 `--dirstat` | 常用 diff 统计格式不可用 | `src/command/log.rs:172-486` | LS-20 |
| GAP-LS-21 | `#967`：`log` 不接受 diff 输出选项（`-w`、`-b`、`--no-prefix`、`--full-index`、`-U`、`--src-prefix`、`--dst-prefix`） | 无法控制 log diff 空白与前缀 | `src/command/log.rs:172-486` | LS-21 |
| GAP-LS-22 | `#970`：`log` 拒绝 `--color-moved` 与 `--color-moved-ws`（`diff.rs` 已有 `--color-moved`，但尚未实现 `--color-moved-ws`） | 无法使用移动块着色与空白模式 | `src/command/log.rs:172-486`、`src/command/diff.rs:438-445` | LS-22 |
| GAP-LS-23 | `#857`：`show` 拒绝 `--patience` 与 `--anchored` | 无法在 `show` 指定对比算法 | `src/command/show.rs:73-187`、`src/command/diff.rs:131-153` | LS-23 |
| GAP-LS-24 | `#858`：`show` 拒绝 `--color-moved` 与 `--word-diff` 组合 | 合法 diff 选项组合在 `show` 报错 | `src/command/show.rs:73-187` | LS-24 |
| GAP-LS-25 | `#862`：`show` 拒绝 `--textconv` / `--no-textconv` | 无法在 `show` 控制 textconv 过滤器 | `src/command/show.rs:73-187`、`src/command/diff.rs:497-511,3403` | LS-25 |
| GAP-LS-26 | `#966`：`log` 与 `show` 拒绝 `--word-diff` 与 `--color-words` | 无法词级差异高亮 | `src/command/log.rs:172-486`、`src/command/show.rs:73-187`、`src/command/diff.rs:179` | LS-26 |
| GAP-LS-27 | `#973`：`log` 与 `show` 拒绝 `--diff-filter` | 无法按状态过滤变更文件 | `src/command/log.rs:172-486`、`src/command/show.rs:73-187`、`src/command/diff.rs:273,2228,2265` | LS-27 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `issues/534.md` | 已完成关闭（`LO-00..LO-09` done/complete） | 复用其既有装饰/abbrev 框架，仅修复 `#952`、`#958` 剩余开放项 |
| `issues/1040.md` / `plan-20261117-format-patch.md` | 均触及 `src/command/log.rs` | 通过 `DEP-LS-01` 协调 `src/command/log.rs` 写锁串行（每卡开工前逐卡重新核验）；本计划不修改 `CommitDiffOptions`，不触及 `src/command/format_patch.rs` |
| `issues/478.md`（`LG-02/04/05/06/08`）/ `issues/1055.md`（`I1055-02`） | 触及 `src/command/show.rs`、`tests/command/show_test.rs`、`docs/commands/show.md`、`COMPATIBILITY.md:199`（见 `plan-status.md:1930` `DEP-I1055-478-SHOW`） | 纳入 `DEP-LS-01` 全写集逐卡互斥核验（涉及 `show.rs` 或共享文档/测试的卡开工前须确认对方无 `in-progress`/`blocked` 写者，否则 fail-closed 等待） |
| `issues/529.md`（`FP-05`/`FP-07`） | 触及 `src/command/diff/`（`FP-05` 提升私有 `mod render` 可见性，`FP-07` 从 `render_diff_output` 抽取独立 `numstat` 渲染函数；`FP-06` 写 `format_patch.rs`/`cli.rs` 与文档） | 纳入 `DEP-LS-02` 协调串行，且明确 **`LS-20` 须在 `529.md` `FP-05`/`FP-07` `done`/`complete` 后开工**，直接复用其 `pub(crate) mod render` 与独立 `numstat` 渲染函数，不重复实现（遵守 `GC-FP-01`） |
| `plan-20260927.md`（CM-08/09） / `plan-20261117-format-patch.md`（FPG-22/27） | 触及 `src/command/diff/`（含 `diff.rs`、`diff/render.rs`、`diff/options.rs`、`diff/compare.rs`） | 通过 `DEP-LS-02` 协调 `src/command/diff/` 写锁串行（每卡开工前逐卡重新核验） |
| `COMPATIBILITY.md` | 第 196 行（`log`）、第 199 行（`show`） | 仅原地修改对应行，不增删行 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 27 项 issue 均指向明确的 `log`/`show` Git 兼容缺口 | 拆分为 31 张单轴卡 `LS-01`..`LS-31`（覆盖 27 项 issue，含 `LS-21` 拆出 `LS-28..30`、`LS-22` 拆出 `LS-31`） |
| 可行性 | 集中在 `log.rs`、`show.rs`、`diff/`（`diff.rs`、`diff/render.rs`、`diff/options.rs`、`diff/compare.rs`）适配复用及 `pager.rs` | 通过 `DEP-LS-01`/`DEP-LS-02` 逐卡核验避开跨计划写锁 |
| 任务卡粒度 | 31 张卡均为单行为轴、`AC ≤ 8`、`VER ≤ 8`、`scope ∈ {S, M}` | 无 L/XL 卡，无多轴卡 |
| 依赖与顺序 | 线性 DAG `DEP-LS-01 -> LS-01 -> ... -> LS-23 -> LS-26 -> LS-24 -> LS-25 -> LS-27`（叠加 `DEP-LS-02` 至涉 `src/command/diff/` 卡） | 无环、无 I–I 并发冲突 |
| 完整性 | 每卡含 `nextest` 精确 filter、ER-13 `T-1`/`T-3` 显式触发、D 组真实 CI job、EN/zh/dev/后端脚本/`COMPATIBILITY.md` | 符合 ER-04、ER-06/ER-06a、ER-08、ER-13、ER-14 |
| 安全性 | `--output=<file>` 路径检查、TTY 控制字符清理、`textconv` 信任边界说明 | 继承 GC-07、GC-11 |
| 功能正确性 | 依据 `git@6de20f6092` 逐项验证边界条件（含 `-R` 反转 diff 输入、`-L :fn:file` 函数边界追踪） | 消除模糊判据 |
| 接口兼容 | `LogOutput` / `ShowOutput` JSON 保持向后兼容；`LS-16` 修正 `--follow` 为 Git 兼容无参标志 | 登记 `T-3` 与兼容说明 |
| 数据流与控制流 | `--output=<file>` 先渲染或安全写入目标文件 | 失败返回 `LBR-IO-002` |
| 性能与容量 | `--find-object`、`-L`、`--simplify-by-decoration` 有界遍历 | 继承 GC-10 |
| 可靠性与容错 | `Result` + `CliError` 返回可操作诊断，无生产 `unwrap()` | 继承 GC-11 |
| 可维护性 | 在 `src/command/diff/` 抽取 `pub(crate)` 共享适配层供 `log.rs`/`show.rs` 复用，不修改 `CommitDiffOptions` | 继承 GC-02 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 03:24:00 UTC | 初始成稿 | 建立 `plan-20261117-log-show.md`，映射全部 27 项 issue 至 `LS-01`..`LS-27` | N/A | 全文 |
| 2026-11-17 03:32:00 UTC | Codex R1 FAIL | 修正 `LS-12` `#969` `-R` 语义、全卡显式 `T-1`、`LS-25` `textconv` 边界 | N/A | 缺口表、LS-12、LS-25、Review log |
| 2026-11-17 03:40:00 UTC | Claude R1 FAIL + Codex R2 FAIL | ① 明确新增 `LogDiffOptions` 桥接而不改 `CommitDiffOptions`（免触 `format_patch.rs`）；② 写集与 `DEP-LS-02` 改为精确的 `src/command/diff.rs`；③ `LS-23`/`24`/`25` 写集补入 `src/command/log.rs`；④ `LS-22` 明确 `--color-moved-ws` 新增；⑤ 调整顺序 `LS-23 -> LS-26 -> LS-24 -> LS-25 -> LS-27`；⑥ `LS-13` 补入 `pager.rs`；⑦ 修正 `LS-21` AC 计数、`LS-23`/`LS-04`/`LS-12`/`LS-16`/`LS-25` | N/A | 实施顺序、依赖表、粒度表、LS-04/12/13/16/21/22/23/24/25/26/27、Review log |
| 2026-11-17 03:50:02 UTC | Claude R3 FAIL + Codex R3 FAIL | ① 将 `DEP-LS-02` 与 `ADR-LS-01` 及 `LS-20..LS-27` 写集对齐至 `src/command/diff/`（`diff.rs`、`diff/render.rs:61,503,782`、`diff/options.rs:28`）；② 修复 `LS-04,11,12,16..22,25..27` 的 `nextest` 回归 filter 为真实 `test_log_*`/`test_diff_*` 子串；③ `LS-12` 移除多余 `diff.rs`/`DEP-LS-02`，`LS-13` 移除误引 `diff.rs:2574` 并锚定 `log.rs:2325,2667-2683` 与 `pager.rs:95-103`；④ `LS-17` 补函数边界与范围外隔离验收，`LS-26` 补有效正则改词边界及优先级验收；⑤ `DEP-LS-01`/`DEP-LS-02` 改为每卡开工前逐卡重新核验写锁 | N/A | ADR-LS-01/02、实施顺序、依赖表、粒度表、LS-04/11/12/13/16..27、追溯表、风险表、Review log |
| 2026-11-17 04:01:04 UTC | Codex R4 FAIL | 按 G-04 口径修正 `LS-20..LS-27` 的 `landing` 行为落点计数（仅计生产文件，`LS-20..22` 为 3、`LS-23..26` 为 4、`LS-27` 为 3，全部 `≤ 4` 符合 `M` 上限） | N/A | 粒度表、LS-20..27、Review log |
| 2026-11-17 04:10:15 UTC | Codex R5 P2 | 按 G-04 口径将 `LS-01..LS-19` 的 `landing` 落点数与 `prod-files` 对齐（排除随附测试文件，`LS-01..27` 全部 `landing == prod-files ≤ 4`） | N/A | 粒度表、LS-01..19、Review log |
| 2026-11-17 04:12:43 UTC | Claude R6 FAIL + Codex R6 FAIL | ① 按 G-04/G-09 将 `LS-21`（#967）拆分为 4 张单轴卡：`LS-21`（`-w`/`-b`）、`LS-28`（前缀）、`LS-29`（`--full-index`）、`LS-30`（`-U`/`--unified`），各卡 `prod-files=3, landing=3`；② 在 `ADR-LS-01` 与 `DEP-LS-02` 补入 `src/command/diff/compare.rs`；③ 全部 `LS-01..LS-30` 的 `Dependencies` 与粒度表补齐 `DEP-LS-01` 与 `实施顺序` 严格一致；④ 修正 `LS-17`（`log.rs:1907`）、`LS-22`（`diff.rs:5456` `moved_line_bodies`）P2 锚点 | LS-21 → LS-21, LS-28, LS-29, LS-30 | 实施顺序、ADR-LS-01、DEP-LS-02、粒度表、LS-02..30、追溯表、Review log |
| 2026-11-17 04:16:00 UTC | Claude R7 FAIL + Codex R7 FAIL | ① `DEP-LS-02` 补入 `src/command/diff/compare.rs` 并将 `DEP-LS-02`/`ADR-LS-01` 覆盖范围更新为 `LS-20..LS-30`；② 完成 G-09 全文同步（`DEP-LS-01 -> LS-01..LS-30`、评审结论 30 张卡、M2 含 `LS-28..30`、风险表 `LS-01..LS-30`、`LS-22` 审计表 `序列化于 LS-30`）；③ 修复 `LS-02..27` `DEP-LS-01` 缺失闭合反引号；④ `LS-20` 证据/描述补 `diff/render.rs:503`，`LS-30` 补入 `diff/options.rs:29`（`landing=4, prod-files=4, M`） | N/A | 评审结论、ADR-LS-01、实施顺序、依赖表、粒度表、LS-02..30、M2、风险表、Review log |
| 2026-11-17 04:24:35 UTC | Claude R8 FAIL + Codex R8 FAIL | ① 将 `M2` 里程碑同步为 `LS-13..LS-22, LS-28..LS-30`；② 将 `LS-20` 卡片描述与证据补入 `diff/render.rs:503`；③ 将 `LS-30` 追溯表补入 `diff/options.rs:29`；④ 更新第 35/142 行卡数为 30 张并修正 `ADR-LS-01` `render_word_diff` 函数名；⑤ 清理修订历史空行 | N/A | 规则、ADR-LS-01、LS-20、追溯表、M2、Review log |
| 2026-11-17 04:27:50 UTC | Claude R9 FAIL + Codex R9 P2 | 补全 `LS-20` 卡片描述与证据表中的 `src/command/diff/render.rs:503`（`format_diff_raw` 当前 `pub(super)`，按 `ADR-LS-01` 升为 `pub(crate)`） | N/A | LS-20、Review log |
| 2026-11-17 04:33:00 UTC | Claude R10 FAIL + Codex R10 FAIL | ① `DEP-LS-02` 与「与其它计划的关系」补入 `issues/529.md`（`FP-05`/`FP-06`/`FP-07`）跨计划写锁并明确 `LS-20` 在 `FP-05`/`FP-07` `done`/`complete` 后开工，同时修正 `plan-status.md:1545-1546` 行号；② `ADR-LS-02`/`DEP-LS-01`/`DEP-LS-02` 扩展为对每卡完整 `Implementation write set`（含 `show.rs`、测试、docs、`COMPATIBILITY.md`）逐卡 fail-closed 互斥核验，并补入 `issues/478.md`/`issues/1055.md`（`plan-status.md:1930`）；③ 统一网站交付路径至 `AGENTS.md:135` `/Volumes/Sea/libra-backend`（`cf` 分支）及相邻 `../libra-backend` 远端身份核验；④ 修正 `ADR-LS-01`（`diff.rs:5` 私有 `mod render;` 需升为 `pub(crate)`）与 `LS-20`（`numstat` 位于 `render_diff_output` `render.rs:278`，`format_diff_dirstat` 已为 `pub(crate)` 且 `log` 已有 `compute_commit_dirstat` `log.rs:3416`）；⑤ 更新文头时间 | N/A | 文头、事实基线、关系表、ADR-LS-01/02、GC-05、DEP-LS-01/02、LS-20、Review log |
| 2026-11-17 04:37:00 UTC | Codex R11 FAIL（Claude R11 `ECONNRESET`） | 严格对齐 `AGENTS.md:135`，移除对相邻检出 `../libra-backend` 的回退，将全计划网站交付路径、写集与命令统一为 `/Volumes/Sea/libra-backend`（`cf` 分支；不可用时 fail-closed 停止网站交付，除非用户明确授权替代检出） | N/A | 文头、事实基线、GC-05、LS-01..LS-30、兼容收口、Review log |
| 2026-11-17 04:43:00 UTC | Codex R13 FAIL（Claude R12 `ECONNRESET`） | 将 `LS-29` 的 `scope` 从 `S` 修正为 `M`（`landing=3, prod-files=3` 且新增公开 `--full-index` 参数，符合 G-04），并同步粒度表、卡内元数据与文头时间 | N/A | 文头、粒度表、LS-29、Review log |
| 2026-11-17 04:56:00 UTC | Claude R15 FAIL（Codex R15 PASS） | ① 将 `diff --color-moved-ws` 从 `LS-22` 拆出为前置单轴卡 `LS-31`（`LS-30 -> LS-31 -> LS-22`，每卡仅改单个公开命令接口，符合 G-04/ER-06a），补齐 `diff` 文档/网站 `diff.en.md`/`COMPATIBILITY.md:227`/`diff_test.rs`；② 修正关系表 `issues/529.md` 为 `FP-05`/`FP-07`；③ 修正 `LS-25` `Out of scope` 为 `diff.rs:584,590`（`for_service`） | LS-22 → LS-31, LS-22 | 文头、适用范围、关系表、实施顺序、Phase 1、粒度表、LS-31/LS-22/LS-25、追溯表、兼容收口、Review log |

## 已决议设计决策

### ADR-LS-01: `log` / `show` 复用 `src/command/diff/` 的 `pub(crate)` 适配层（不改 `CommitDiffOptions`）

- **Status:** Accepted
- **Context:** #857、#858、#862、#964、#966、#967、#970、#973 要求 `log`/`show` 支持 `diff` 能力；当前 `src/command/diff.rs:5` 将 `mod render;` 声明为私有模块，其内 `render_word_diff`（`diff/render.rs:61`）、`format_diff_raw`（`diff/render.rs:503`）、`colorize_diff`（`diff/render.rs:782`）为 `pub(super)`（而 `format_diff_dirstat` 在 `diff/render.rs:412` 已为 `pub(crate)`），`resolve_diff_config`（`diff/options.rs:28`）位于 `src/command/diff/` 子模块，`-U<n>`/`-w`/`-b`/`--full-index` 位于 `src/command/diff/compare.rs:405-417,591`，而 `parse_diff_filter`（2228）、`apply_diff_filter`（2265）、`run_textconv`（3403）位于 `src/command/diff.rs`；同时 `CommitDiffOptions`（`log.rs:4254`）被 `format_patch.rs:1852` 逐字段构造。
- **Decision:** 在 `src/command/diff/`（`diff.rs`、`diff/render.rs`、`diff/options.rs`、`diff/compare.rs`）将 `mod render`（由 `issues/529.md` `FP-05` 提升或开工前核实为 `pub(crate)`）及上述逻辑抽取为 `pub(crate)` 共享入口，并在 `src/command/log.rs` 引入独立的 `LogDiffOptions` 适配调用它们，保持既有 `CommitDiffOptions` 签名不变（不触及 `src/command/format_patch.rs`）。
- **Alternatives considered:** 直接向 `CommitDiffOptions` 追加字段（会破坏 `format_patch.rs` 编译并引发跨计划写锁冲突，拒绝）。
- **Consequences:** 触及 `src/command/diff/` 任一文件的卡（`LS-20..LS-30`）均须依赖 `DEP-LS-02` 并将其实际改动文件列入 `Implementation write set`。
- **Revisit when:** `plan-20260927` CM-08/CM-09 与 `format-patch` 均合入后可统一整合。

### ADR-LS-02: `log.rs` / `show.rs` / `diff/` 共享写集串行化与逐卡锁核验

- **Status:** Accepted
- **Context:** `LS-01`..`LS-30` 写 `src/command/log.rs`，部分卡同时写 `src/command/show.rs` 与 `src/command/diff/`；单靠 `LS-01` 前一次核验无法覆盖 30 张卡逐卡发布窗口期内的外部并发写者。
- **Decision:** 采用线性依赖链，且**每张卡在开工前**都必须按其完整 `Implementation write set`（含 `src/command/log.rs`、`src/command/show.rs`、`src/command/diff/`、对应 `tests/command/*`、`docs/commands/*`、`COMPATIBILITY.md:196,199` 与网站页）重新核验 `DEP-LS-01`（及适用时的 `DEP-LS-02`）：确认 `plan-status.md` 中对上述全部文件均无其它 `in-progress`/`blocked` 写者；若写锁归属不清或存在并发写者则 fail-closed 禁止开工，待释放并 rebase 后再取得写锁执行（G-10）。
- **Alternatives considered:** 仅在 `LS-01` 前一次性检查（无法覆盖后续卡窗口，拒绝）。
- **Consequences:** 串行交付，全执行窗口零写集冲突。
- **Revisit when:** 后续拆分 `log/` 子模块后可放宽。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01`..`GC-13`。特别强调：
- **GC-05 / ER-06a 文档同步:** 每张卡必须同卡更新受影响的 `docs/commands/{log,show}.md`、`docs/commands/zh-CN/{log,show}.md`、`docs/development/commands/{log,show}.md`、`COMPATIBILITY.md`（第 196 / 199 行原地修改），并严格按 `AGENTS.md:135` 在授权生产网站仓库 `/Volumes/Sea/libra-backend`（`cf` 分支）完成同步、前端校验、Claude Code `PASS`、提交推送与 Cloudflare 部署核验；若 `/Volumes/Sea/libra-backend` 不可用，fail-closed 停止网站交付，除非用户明确授权替代检出。执行命令：`cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only <log|show> --libra-root /Volumes/Data/gitmono/libra`。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01`..`ER-14`：
- **ER-04 / ER-08 / ER-13 / ER-14:** 每张 `implementation` 卡均为 `independent` 发布切片，完成后按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 权威三处（`Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`；因每卡均修改 `install.sh`/`install.ps1`（`T-1`），C 组第 ④ 步除运行本卡 A 组 focused `nextest` 外，必须跑双环境全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`、`fmt`、`clippy`、`cargo build --release`，随后签名提交推送 PR，squash merge 后 `gh release create v<version>`。
- **ER-04 D 组远端后置门（每张 `independent` 卡默认适用）:**
  - `base.yml`（PR / `push` `main`）：`format`、`clippy`、`redundancy`、`test` 全部 green。
  - `codeql.yml`（PR / `push` `main`）：CodeQL 扫描 green。
  - `release.yml`（`push` `v*` tag，由 `gh release create "v${VERSION}"` 触发）：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 green；并核验 `download.libra.tools` / stable manifest（`HEAD` 200、range `GET` 206）。

## 实施顺序

- `DEP-LS-01 -> LS-01..LS-31`（逐卡开工前重新核验 `src/command/log.rs` 等完整写集跨计划写锁；`LS-31` 仅涉 `diff` 时按 `DEP-LS-02` 核验 `src/command/diff/` 与 `diff` 文档）
- Phase 0: `LS-01 -> LS-02 -> LS-03 -> LS-04 -> LS-05 -> LS-06 -> LS-07 -> LS-08 -> LS-09 -> LS-10 -> LS-11 -> LS-12`
- Phase 1: `LS-12 -> LS-13 -> LS-14 -> LS-15 -> LS-16 -> LS-17 -> LS-18 -> LS-19 -> LS-20 -> LS-21 -> LS-28 -> LS-29 -> LS-30 -> LS-31 -> LS-22`（其中 `DEP-LS-02 -> LS-20, LS-21, LS-28, LS-29, LS-30, LS-31, LS-22`）
- Phase 2: `LS-22 -> LS-23 -> LS-26 -> LS-24 -> LS-25 -> LS-27`（其中 `DEP-LS-02 -> LS-23, LS-26, LS-24, LS-25, LS-27`）

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-LS-01 | incoming | 跨计划全写集互斥锁 | `issues/1040.md`、`plan-20261117-format-patch`（`FPG-11`/`22`/`27`）、`issues/478.md`（`LG-02/04/05/06/08`）、`issues/1055.md`（`I1055-02`）及 `plan-status.md` 登记的其它在途写者 | 协调者 | **每张 `LS-*` 卡开工前**重新核实 `plan-status.md` 中该卡**完整 `Implementation write set`**（含 `src/command/log.rs`、`src/command/show.rs`、`tests/command/{log,show,diff}_test.rs`、`docs/commands/{log,show,diff}.md`、`COMPATIBILITY.md:196,199,227` 与网站页）均无 `in-progress`/`blocked` 写者，取得独占写锁并于本卡推送后释放 | `docs/development/plan/issues/1040.md`、`docs/development/plan/plan-status.md:1930` | fail-closed：若有并发写者或归属不清，禁止开工，等待释放并 rebase 后再核验 |
| DEP-LS-02 | incoming | 跨计划写锁与顺序前置 | `issues/529.md`（`FP-05`/`FP-07`）、`plan-20260927#CM-08`/`CM-09`、`plan-20261117-format-patch`（`FPG-22`/`27`） | 协调者 | **每张涉 `src/command/diff/` 的卡（`LS-20..LS-31`）开工前**重新核实 `plan-status.md` 中 `src/command/diff/`（含 `diff.rs`、`diff/render.rs`、`diff/options.rs`、`diff/compare.rs`）无在途写者；且 **`LS-20` 须待 `issues/529.md` `FP-05`（公开 `mod render`）与 `FP-07`（抽取独立 `numstat` 渲染函数）`done`/`complete` 后方可开工** | `docs/development/plan/plan-status.md:1545-1546`、`docs/development/plan/issues/529.md:507,1392,1511` | fail-closed：若有在途写者或 `FP-05`/`FP-07` 未完成，等待释放/完成后 rebase 再开工 |

### 发布分组与并发窗口

- **并发声明:** 全串行（共享 `src/command/log.rs`、`src/command/show.rs` 与 `src/command/diff/` 写集，依据 G-10 禁止并发；每张卡开工前均须重新核验 `DEP-LS-01` 及适用时的 `DEP-LS-02`）。
- **发布者:** 单一指定发布者（协调者指派的执行 Agent）。
- **发布窗口顺序:** 按 `LS-01` → … → `LS-21` → `LS-28` → `LS-29` → `LS-30` → `LS-31` → `LS-22` → `LS-23` → `LS-26` → `LS-24` → `LS-25` → `LS-27` 依次执行 C 组 bump 与 `gh release create`。

### Phase 0: `log` 装饰与遍历基础（LS-01..LS-12）

**目标:** 修复 `log` 嵌套 tag 装饰、`log.decorate=true`、`log.excludeDecoration`、`#952` 装饰过滤、`--simplify-by-decoration`、`--clear-decorations`、`color.decorate.*`、`replaced` 标记、`#958` `--no-abbrev`、`--no-walk`/`--tags`、`--before`/`--after` 与 `-R`。

**进入条件:** `DEP-LS-01` 核实通过，工作区干净。

**退出条件:** `LS-01`..`LS-12` 全部 `done` / `complete`。

### Phase 1: `log` 输出、过滤与行追踪（LS-13..LS-22，含拆出 LS-28..LS-31）

**目标:** 修复 `log` TTY 脏序列、空 `--format=`/`--pretty=`、`--output=<file>`、无参 `--follow`、`-L` 行级历史、`--find-object`、`--cc`、`--raw`/`--numstat`/`--dirstat`、通用 diff 输出选项、`diff --color-moved-ws` 与 `log --color-moved`/`--color-moved-ws`。

**进入条件:** `LS-12` 完成。

**退出条件:** `LS-13`..`LS-22` 及 `LS-28`..`LS-31` 全部 `done` / `complete`。

### Phase 2: `log`/`show` diff 选项对齐与 `show` 补齐（LS-23, LS-26, LS-24, LS-25, LS-27）

**目标:** 补齐 `show --patience`/`--anchored`、`log`/`show --word-diff`/`--color-words`、`show --color-moved --word-diff`、`show --textconv` 与 `log`/`show --diff-filter`，完成全量收口门。

**进入条件:** `LS-22` 完成。

**退出条件:** `LS-23`、`LS-26`、`LS-24`、`LS-25`、`LS-27` 全部 `done` / `complete`，计划完成判据全量门全绿。

## 任务卡

### 任务卡粒度规则（强制）

继承 [plan-template.md](plan-template.md) 的 `G-01`..`G-11`。

### 字段全局默认与例外

- **Release boundary 默认:** `independent`（每卡独立执行完整 C 组含 `patch + 1`、`compat_version_surface_sync` 同步与 `gh release create`，并适用 ER-04 D 组：`base.yml`、`codeql.yml`、`release.yml`）
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（推送/发布前：本地 `revert` 单提交；推送/发布后：已发布版本与 tag 不可撤回，按 ER-04/ER-10 新建 `FIX-*` 前滚修复并发新 `patch + 1` 版本；兼容窗口与降级指引：同 `0.30.x` 次版本内无 schema 变更，可回退安装上一版本 `v<prev>`）
- **Migration and rollback 默认:** `N/A：无 schema 迁移`
- **Security and privacy 默认:** 继承 GC-07、GC-11（路径边界校验、无裸 `unwrap()`）
- **Performance budget 默认:** 继承 GC-10（无无界全库遍历）
- **Release write set 默认:** `Inherited`
- **Full-suite trigger 默认:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）`
- **Deliverables 默认:** `N/A`

**默认覆盖:** 无。

**规则 waiver（`EX-*`）:** 无。

### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| LS-01 | implementation | 嵌套 annotated-tag 装饰与 `%D`/`%d` | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | DEP-LS-01 | 序列化于 DEP-LS-01 | independent | N/A | N/A |
| LS-02 | implementation | `log.decorate=true` 与 `--pretty=raw` | immutable-release | yes | yes | 2/8 | 2/8 | 2/2 | M | LS-01,DEP-LS-01 | 序列化于 LS-01 | independent | N/A | N/A |
| LS-03 | implementation | `log.excludeDecoration` 与无值诊断 | immutable-release | yes | yes | 2/8 | 2/8 | 2/2 | M | LS-02,DEP-LS-01 | 序列化于 LS-02 | independent | N/A | N/A |
| LS-04 | implementation | `#952` `--decorate-refs` 剩余边界修复 | immutable-release | yes | yes | 2/8 | 2/8 | 2/2 | M | LS-03,DEP-LS-01 | 序列化于 LS-03 | independent | N/A | N/A |
| LS-05 | implementation | `log --simplify-by-decoration` | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-04,DEP-LS-01 | 序列化于 LS-04 | independent | N/A | N/A |
| LS-06 | implementation | `--clear-decorations` 与 `initialDecorationSet` | immutable-release | yes | yes | 2/8 | 2/8 | 2/2 | M | LS-05,DEP-LS-01 | 序列化于 LS-05 | independent | N/A | N/A |
| LS-07 | implementation | `color.decorate.*` 与 `diff.color.commit` | immutable-release | yes | yes | 2/8 | 2/8 | 2/2 | M | LS-06,DEP-LS-01 | 序列化于 LS-06 | independent | N/A | N/A |
| LS-08 | implementation | `replaced` 标记与 `GIT_REPLACE_REF_BASE` | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-07,DEP-LS-01 | 序列化于 LS-07 | independent | N/A | N/A |
| LS-09 | implementation | `#958` `--no-abbrev` 剩余边界修复 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-08,DEP-LS-01 | 序列化于 LS-08 | independent | N/A | N/A |
| LS-10 | implementation | `log --no-walk` 与 `--tags` | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-09,DEP-LS-01 | 序列化于 LS-09 | independent | N/A | N/A |
| LS-11 | implementation | `log --before`/`--after` 日期过滤别名 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-10,DEP-LS-01 | 序列化于 LS-10 | independent | N/A | N/A |
| LS-12 | implementation | `log -R` 反转 diff 输入方向 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-11,DEP-LS-01 | 序列化于 LS-11 | independent | N/A | N/A |
| LS-13 | implementation | `log` TTY 多余 `CR`/`ESC` 序列修复 | immutable-release | yes | yes | 2/8 | 2/8 | 2/2 | M | LS-12,DEP-LS-01 | 序列化于 LS-12 | independent | N/A | N/A |
| LS-14 | implementation | 空 `--format=`/`--pretty=` 行为对齐 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-13,DEP-LS-01 | 序列化于 LS-13 | independent | N/A | N/A |
| LS-15 | implementation | `log --output=<file>` 写入文件 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-14,DEP-LS-01 | 序列化于 LS-14 | independent | N/A | N/A |
| LS-16 | implementation | `log --follow` 无参布尔标志 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-15,DEP-LS-01 | 序列化于 LS-15 | independent | N/A | N/A |
| LS-17 | implementation | `log -L` 行级历史输出与互斥校验 | immutable-release | yes | yes | 5/8 | 2/8 | 1/1 | M | LS-16,DEP-LS-01 | 序列化于 LS-16 | independent | N/A | N/A |
| LS-18 | implementation | `log --find-object=<object>` | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-17,DEP-LS-01 | 序列化于 LS-17 | independent | N/A | N/A |
| LS-19 | implementation | `log --cc` 合并提交组合差异 | immutable-release | yes | yes | 2/8 | 2/8 | 1/1 | M | LS-18,DEP-LS-01 | 序列化于 LS-18 | independent | N/A | N/A |
| LS-20 | implementation | `log --raw`/`--numstat`/`--dirstat` | immutable-release | yes | yes | 3/8 | 2/8 | 3/3 | M | LS-19,DEP-LS-02,DEP-LS-01 | 序列化于 LS-19 | independent | N/A | N/A |
| LS-21 | implementation | `log` diff 空白忽略（`-w`/`-b`） | immutable-release | yes | yes | 3/8 | 2/8 | 3/3 | M | LS-20,DEP-LS-01,DEP-LS-02 | 序列化于 LS-20 | independent | N/A | N/A |
| LS-28 | implementation | `log` diff 路径前缀（`--no-prefix`/`--src-prefix`/`--dst-prefix`） | immutable-release | yes | yes | 3/8 | 2/8 | 3/3 | M | LS-21,DEP-LS-01,DEP-LS-02 | 序列化于 LS-21 | independent | LS-21 | N/A |
| LS-29 | implementation | `log` diff `--full-index` | immutable-release | yes | yes | 1/8 | 2/8 | 3/3 | M | LS-28,DEP-LS-01,DEP-LS-02 | 序列化于 LS-28 | independent | LS-21 | N/A |
| LS-30 | implementation | `log` diff `-U`/`--unified` | immutable-release | yes | yes | 1/8 | 2/8 | 4/4 | M | LS-29,DEP-LS-01,DEP-LS-02 | 序列化于 LS-29 | independent | LS-21 | N/A |
| LS-31 | implementation | `diff` 新增 `--color-moved-ws` 空白模式 | immutable-release | yes | yes | 2/8 | 2/8 | 2/2 | M | LS-30,DEP-LS-01,DEP-LS-02 | 序列化于 LS-30 | independent | LS-22 | N/A |
| LS-22 | implementation | `log` 复用 `--color-moved` 与 `--color-moved-ws` | immutable-release | yes | yes | 2/8 | 2/8 | 3/3 | M | LS-31,DEP-LS-01,DEP-LS-02 | 序列化于 LS-31 | independent | N/A | N/A |
| LS-23 | implementation | `show --patience` 与 `--anchored` | immutable-release | yes | yes | 2/8 | 2/8 | 4/4 | M | LS-22,DEP-LS-02,DEP-LS-01 | 序列化于 LS-22 | independent | N/A | N/A |
| LS-26 | implementation | `log`/`show --word-diff` 与 `--color-words` | immutable-release | yes | yes | 3/8 | 2/8 | 4/4 | M | LS-23,DEP-LS-02,DEP-LS-01 | 序列化于 LS-23 | independent | N/A | N/A |
| LS-24 | implementation | `show --color-moved` 与 `--word-diff` 组合 | immutable-release | yes | yes | 2/8 | 2/8 | 4/4 | M | LS-26,DEP-LS-02,DEP-LS-01 | 序列化于 LS-26 | independent | N/A | N/A |
| LS-25 | implementation | `show --textconv`/`--no-textconv` | immutable-release | yes | yes | 3/8 | 3/8 | 4/4 | M | LS-24,DEP-LS-02,DEP-LS-01 | 序列化于 LS-24 | independent | N/A | N/A |
| LS-27 | implementation | `log`/`show --diff-filter` | immutable-release | yes | yes | 2/8 | 2/8 | 3/3 | M | LS-25,DEP-LS-02,DEP-LS-01 | 序列化于 LS-25 | independent | N/A | N/A |

---

### Task LS-01: 修复 `log --decorate` 嵌套 annotated-tag 链与 `%D`/`%d` 输出（#949）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 递归解引用指向 tag 对象的 annotated tag 链（直到 commit/非 tag 目标），使 `log --decorate`、`%D` 与 `%d` 完整显示所有可达标签而非丢弃或输出空串。

**Out of scope:** `log.decorate` 配置与 `--pretty=raw`（由 `LS-02` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `log.rs` 装饰收集仅解引用单层 tag，嵌套 annotated tag 链未绑定到目标 commit | `src/command/log.rs:2327,2441` |

**Acceptance criteria:**

- [ ] 指向另一 annotated tag 的 tag 在 `log --decorate`、`%D` 与 `%d` 中均正确显示。
- [ ] 自引用或超深 tag 链有界终止（GC-10），不无限循环。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::decorate_nested_annotated_tags)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=嵌套annotated-tag装饰与%D/%d; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=DEP-LS-01; writeset=序列化于DEP-LS-01; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-02: 修复 `log.decorate=true` 与 `--pretty=raw` 装饰规则（#950）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 修复 `str_to_decorate_option` / `determine_decorate_option` 使 `log.decorate=true`（Git 布尔真值）启用 `short` 装饰，并在 `--pretty=raw`（及 `format.pretty=raw`）时不应用 `log.decorate` 配置装饰（匹配 Git）。

**Out of scope:** `log.excludeDecoration`（由 `LS-03` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `determine_decorate_option` 读取 `log.decorate` 后未将布尔 `true` 映射为 `Short`，且未在 `--pretty=raw` 下抑制配置装饰 | `src/command/log.rs:1020-1075` |

**Acceptance criteria:**

- [ ] `log.decorate=true`、`yes`、`on` 启用 `short` 装饰；非法值报 `LBR-CLI-002`。
- [ ] `--pretty=raw` 下不输出 `log.decorate` 配置带来的装饰（显式 `--decorate` 仍可开启）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::log_decorate_bool_true_and_pretty_raw)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-01`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log.decorate=true与--pretty=raw; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=LS-01,DEP-LS-01; writeset=序列化于LS-01; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-03: 支持 `log.excludeDecoration` 配置与无值 key 诊断（#951）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 读取多值配置 `log.excludeDecoration` 并将其与 `--decorate-refs-exclude` 一并参与装饰过滤；当配置文件存在无值 `log.excludeDecoration` 时按 Git 规则返回缺失值诊断错误。

**Out of scope:** `--clear-decorations`（由 `LS-06` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `decorate_name_allowed` 仅读 CLI `args.decorate_refs_exclude`，未读 `log.excludeDecoration` | `src/command/log.rs:650-672` |

**Acceptance criteria:**

- [ ] `log.excludeDecoration` 配置的引用模式被正确排除在装饰之外。
- [ ] 无值 `log.excludeDecoration` 产生 `missing value for 'log.excludeDecoration'` 类错误（`LBR-CLI-002`）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::log_exclude_decoration_config)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-02`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log.excludeDecoration与无值诊断; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=LS-02,DEP-LS-01; writeset=序列化于LS-02; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-04: 核实 `534` 后 `#952` `--decorate-refs` 剩余边界并修复（#952）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 GC-01 核实 `issues/534.md`（`LO-05..LO-08`）已合入的 `--decorate-refs` / `--decorate-refs-exclude` 匹配、HEAD 过滤与逆序排列，针对 `#952` 仍未关闭的边界用例（与 `git@6de20f6092` 对照复现）补齐修复与回归测试。

**Out of scope:** `--clear-decorations`（由 `LS-06` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/534.md` 记录 `#952` 在 `LO-09` 后仍有失败项或未发帖保持 OPEN | `issues/534.md:3,11`、`src/command/log/decorate_filter.rs:15-55` |

**Acceptance criteria:**

- [ ] 无前缀 glob（如 `--decorate-refs=*tag*`）、`HEAD` 过滤与 ref 排列顺序均与 `git@6de20f6092` 一致。
- [ ] 新增覆盖 `#952` 剩余开放场景的回归测试通过。
- [ ] ER-06/ER-06a 同卡强制门：逐文件核对并同步所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::decorate_refs_issue_952_remaining)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::test_log_decorate)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-03`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `src/command/log/decorate_filter.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/decorate_filter.rs`, `tests/command/log_test.rs`

**Docs and compatibility impact:** 逐文件核对并同步 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=#952_--decorate-refs剩余边界修复; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=LS-03,DEP-LS-01; writeset=序列化于LS-03; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-05: 实现 `log --simplify-by-decoration`（#953）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--simplify-by-decoration` 并实现提交历史简化：仅保留带有装饰引用或分叉/合并拓扑关键点的提交并重写父子边。

**Out of scope:** `--full-history` / `--dense` 等其他简化模式。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `simplify_by_decoration` 字段 | `src/command/log.rs:172-486` |

**Acceptance criteria:**

- [ ] `libra log --simplify-by-decoration` 仅输出带有效装饰或关键分叉/合并点的提交。
- [ ] 与 `--decorate-refs` / `--decorate-refs-exclude` 组合时按过滤后的装饰集简化。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::simplify_by_decoration)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_shallow_walk_helper`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-04`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--simplify-by-decoration; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-04,DEP-LS-01; writeset=序列化于LS-04; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-06: 支持 `--clear-decorations` 与 `log.initialDecorationSet`（#954）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--clear-decorations`（清除先前的 `--decorate-refs` / `--decorate-refs-exclude` 与 `log.initialDecorationSet` 初始集合）并支持 `log.initialDecorationSet`（`all` / `useful`）配置。

**Out of scope:** 不改变 `--decorate-refs` 自身模式语法。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `clear_decorations`，`log/config.rs` 未读 `log.initialDecorationSet` | `src/command/log.rs:207-214`、`src/command/log/config.rs:1-156` |

**Acceptance criteria:**

- [ ] `log.initialDecorationSet`（`all`/`useful`）生效，非法值报 `LBR-CLI-002`。
- [ ] `--clear-decorations` 清除此前装饰集选择，后续 `--decorate-refs` 重新累加。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::clear_decorations_and_initial_set)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-05`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--clear-decorations与initialDecorationSet; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=LS-05,DEP-LS-01; writeset=序列化于LS-05; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-07: 支持 `color.decorate.*` 与 `diff.color.commit` 着色配置（#959）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 读取 `color.decorate.{branch,remoteBranch,tag,stash,HEAD,grafted}` 与 `diff.color.commit`（以及 `color.diff.commit`），在 `--decorate --color` / `--color=always` 下按配置着色而非硬编码 ANSI 颜色。

**Out of scope:** 不改变非装饰区域的默认 diff hunk 配色。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `log.rs` 使用硬编码装饰颜色（如 `Tag => yellow`），未读 `color.decorate.*` / `diff.color.commit` | `src/command/log.rs:1123` |

**Acceptance criteria:**

- [ ] `color.decorate.{branch,remoteBranch,tag,stash,HEAD,grafted}` 控制对应装饰引用 ANSI 颜色。
- [ ] `diff.color.commit` 控制 `commit <oid>` 行的 ANSI 颜色。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::color_decorate_and_commit_config)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-06`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=color.decorate.*与diff.color.commit; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=LS-06,DEP-LS-01; writeset=序列化于LS-06; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-08: `log --decorate` 显示 `replaced` 标记与支持 `GIT_REPLACE_REF_BASE`（#960）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `log --decorate` / `%d` / `%D` 中为存在 replace ref 的提交输出 `replaced` 装饰标记，并支持通过 `GIT_REPLACE_REF_BASE` 环境变量自定义 replace ref 前缀（默认 `refs/replace/`）。

**Out of scope:** 不改变 `libra replace` 写入逻辑。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `log.rs` 装饰收集未检查 replace refs 与 `GIT_REPLACE_REF_BASE` | `src/command/log.rs:2441` |

**Acceptance criteria:**

- [ ] 存在 `refs/replace/<oid>` 时 `log --decorate` 输出 `(replaced)` 标记。
- [ ] 设置 `GIT_REPLACE_REF_BASE=refs/custom-replace/` 时按该前缀识别 `replaced` 标记。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::decorate_replaced_and_env_base)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-07`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=replaced标记与GIT_REPLACE_REF_BASE; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-07,DEP-LS-01; writeset=序列化于LS-07; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-09: 核实 `534` 后 `#958` `--no-abbrev` 剩余边界并修复（#958）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 GC-01 核实 `issues/534.md` 已落地的 `--no-abbrev` 行为，复现 `#958` 仍保持 OPEN 的未闭合失败场景（与 `git@6de20f6092` 对照），补齐缺失输出路径的全长 OID 渲染并添加回归测试。

**Out of scope:** JSON `short_hash` 保持 7 位不变（`COMPATIBILITY.md:196`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/534.md` 记录 `#958` 在 `LO-09` 后仍保持 OPEN | `issues/534.md:3,11`、`src/command/log.rs:190-191` |

**Acceptance criteria:**

- [ ] 复现 `#958` 开放用例并在 `--no-abbrev` 下输出完整 OID（含 `%h`/`%p`/`--parents`/`--children`/`--raw` 等）。
- [ ] 新增对应回归测试通过且不破坏 `--abbrev=<n>` 最后出现胜出语义。
- [ ] ER-06/ER-06a 同卡强制门：逐文件核对并同步所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::no_abbrev_issue_958_remaining)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-08`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件核对并同步 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=#958_--no-abbrev剩余边界修复; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-08,DEP-LS-01; writeset=序列化于LS-08; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-10: 实现 `log --no-walk` 与 `--tags`（#956）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--no-walk[=sorted|unsorted]`（仅展示给定提交，不沿父链遍历）与 `--tags[=<pattern>]`（将匹配 `refs/tags/` 的提交加入起点）。

**Out of scope:** `--branches`/`--remotes`（不在本卡范围内）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `no_walk` 与 `tags` 字段 | `src/command/log.rs:172-486` |

**Acceptance criteria:**

- [ ] `libra log --no-walk A B` 仅显示 `A` 与 `B` 本身而不遍历其祖先。
- [ ] `libra log --tags --no-walk` 展示所有 tag 指向的提交（支持 `--tags=<glob>`）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::no_walk_and_tags)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_shallow_walk_helper`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-09`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--no-walk与--tags; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-09,DEP-LS-01; writeset=序列化于LS-09; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-11: 支持 `log --before` / `--after` 日期过滤别名（#965）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 为 `LogArgs::since` 添加 `visible_alias = "after"`，为 `LogArgs::until` 添加 `visible_alias = "before"`，使其与 `--since`/`--until` 完全等价。

**Out of scope:** 不改变既有日期解析格式。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs::since`/`until` 未注册 `after`/`before` 别名 | `src/command/log.rs:228-233` |

**Acceptance criteria:**

- [ ] `--after=<date>` 与 `--since=<date>` 行为一致；`--before=<date>` 与 `--until=<date>` 行为一致。
- [ ] 非法日期返回与 `--since`/`--until` 相同的 `LBR-CLI-002` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::before_and_after_aliases)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::test_log_json_since)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-10`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--before/--after别名; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-10,DEP-LS-01; writeset=序列化于LS-10; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-12: 支持 `log -R` 反转 diff 输入方向（#969）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `-R`（在 `src/command/log.rs` 的 `generate_diff_with_options` 中反转 diff 比较双方输入方向，即交换新旧两侧内容，与 `git log -R` / `git diff -R` 一致，**不改变**提交遍历顺序）；注：#969 标题误称 `--reverse` 短别名，本卡按 Git 权威语义实现并记录澄清以关闭 #969。

**Out of scope:** `--reverse` 提交顺序保持不变；不修改 `src/command/diff.rs`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `-R` 旗标；`generate_diff_with_options` 未接收反转标志 | `src/command/log.rs:172-486,4245-4275` |

**Acceptance criteria:**

- [ ] `libra log -R -p` 保持提交从新到旧的顺序不变，仅将每个提交的 diff 方向反转（旧/新侧互换，匹配 `git log -R`）。
- [ ] `libra log --reverse` 仍按从旧到新顺序输出提交。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_reverse_flag_r)'`（`(new)`，同时验证提交顺序不变与 patch 方向反转）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::test_log_reverse)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-11`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_-R反转diff输入方向; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-11,DEP-LS-01; writeset=序列化于LS-11; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-13: 修复 `log` 在 TTY 下输出多余 `CR`/`ESC[m`/`CR ESC[K` 序列（#955）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 PTY 夹具下复现 `libra log` 输出中的多余 `\r`、`ESC[m` 与 `\r\x1b[K` 控制序列（排查 `src/command/log.rs:2325,2667-2683` 经 `Pager::with_config` 写入 `src/utils/pager.rs:95-103` `less -R -F` / stdout 路径时的行尾换行与 ANSI 序列残留），清除非期望终端转义残留。

**Out of scope:** 不禁用显式 `--color=always` 应有的正常颜色转义。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `log.rs` 通过 `Pager::with_config` 向 `less -R -F` 或 stdout 写入；需在 PTY 夹具下定位多余 `\r`/`ESC[m`/`\r\x1b[K` 来源 | `src/command/log.rs:2325,2667-2683`、`src/utils/pager.rs:95-103` |

**Acceptance criteria:**

- [ ] PTY/TTY 环境下运行 `libra log` 无多余 `\r`、孤立 `ESC[m` 或 `\r\x1b[K` 残留。
- [ ] 非 TTY 管道输出保持原有字节不变。
- [ ] ER-06/ER-06a 同卡强制门：逐文件核对并同步所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::tty_output_no_stray_escape_sequences)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-12`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `src/utils/pager.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/utils/pager.rs`, `tests/command/log_test.rs`

**Docs and compatibility impact:** 逐文件核对并同步 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_TTY多余控制序列修复; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=LS-12,DEP-LS-01; writeset=序列化于LS-12; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-14: 修复空 `--format=` / `--pretty=` 值输出空行而非默认提交头（#972）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 修复 `log --format=` 与 `log --pretty=`（以及 `format:`/`tformat:` 后为空串）被当作未设置而退回默认 `medium` 提交头的问题，对齐 Git 为每个提交仅输出空行（`tformat:` 时为空串）。

**Out of scope:** `format.pretty=""` 配置仍保持 `log/config.rs:71` 的报错校验。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `log.rs` 将空 `format`/`pretty` 字符串视为无自定义格式并打印默认头 | `src/command/log.rs:234-240` |

**Acceptance criteria:**

- [ ] `libra log --format=` 与 `libra log --pretty=` 对每个匹配提交不打印默认 `commit`/`Author:`/`Date:` 头（与 `git@6de20f6092` 一致）。
- [ ] `libra log --format="" --stat` 仅输出空行与 diffstat。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::empty_format_or_pretty_omits_header)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-13`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=空--format/--pretty行为对齐; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-13,DEP-LS-01; writeset=序列化于LS-13; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-15: 实现 `log --output=<file>`（#963）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--output=<file>`，将人类可读或 `--json` 输出写入指定文件而非 stdout（禁止写入 `.libra/` 内部目录，失败路径返回 `LBR-IO-002`）。

**Out of scope:** 不改变 `format-patch --output`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `output` 参数 | `src/command/log.rs:172-486` |

**Acceptance criteria:**

- [ ] `libra log --output=out.log` 将完整输出写入 `out.log`，stdout 为空。
- [ ] 目标路径不可写或为目录时返回 `LBR-IO-002` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::output_to_file)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-14`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--output=<file>; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-14,DEP-LS-01; writeset=序列化于LS-14; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-16: 将 `log --follow` 改为无参布尔标志（#962）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 将 `LogArgs::follow` 从 `Option<String>`（`--follow <FILE>`）改为 Git 兼容的布尔标志 `bool`（`--follow`），从位置 pathspec 解析唯一跟踪文件，当给出 0 个或多于 1 个路径时按 Git 规则报错（`--follow requires exactly one pathspec`）。注：此项为修正旧 Libra 非 Git 兼容签名（#962）的缺陷修复，在 `0.30.x` 保持 `patch` 递增并触发 `T-3` 全量测试。

**Out of scope:** `-L` 与 `--follow` 互斥校验（由 `LS-17` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs::follow: Option<String>` 强制要求 `--follow <FILE>` 参数 | `src/command/log.rs:470-471,2212-2214` |

**Acceptance criteria:**

- [ ] `libra log --follow -- <file>` 与 `libra log --follow <file>` 正常跟踪重命名历史。
- [ ] `libra log --follow` 无路径或给出多个路径时返回 `LBR-CLI-002` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档与既有 `log_test` 用例。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::follow_boolean_flag_single_pathspec)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::test_log_follow)'`

**Full-suite trigger:** `T-3: 修改公开 CLI 参数 --follow 签名（由 Option<String> 改为 bool）及 T-1 版本面` | **Dependencies:** `LS-15`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--follow无参布尔标志; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-15,DEP-LS-01; writeset=序列化于LS-15; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-17: 实现 `log -L` 行级历史输出与 pathspec/`--follow` 校验（#961）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 修复 `log -L <start>,<end>:<file>` / `-L :<funcname>:<file>` 仅过滤提交却不输出行级 diff hunk 的缺陷，并增加 `-L` 与位置 pathspec 或 `--follow` 同时使用时的互斥错误校验。

**Out of scope:** 复杂跨文件函数重命名追踪。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `log.rs` 已解析 `line_range` 并调 `commit_affects_line_range`，但未渲染对应行范围 diff 且缺互斥校验 | `src/command/log.rs:479-480,1894,1981-2030` |

**Acceptance criteria:**

- [ ] `libra log -L 1,5:file.txt` 与 `-L :fn_name:file.txt` 输出演进提交及对应行范围的 diff hunk（对照 `git log -L`）。
- [ ] `-L :fn_name:file.txt` 识别函数边界（移除 `src/command/log.rs:1907` 退化为整文件的实现），随历史变更向父提交映射行号范围，仅修改函数体外行的提交不进入结果。
- [ ] 函数名未匹配到时返回明确 `LBR-CLI-003` 错误。
- [ ] `-L` 与位置 pathspec 或 `--follow` 共用时返回 `LBR-CLI-002` 错误。
- [ ] 非法 `-L` 语法或不存在的文件返回明确 `LBR-CLI-002` / `LBR-CLI-003` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::line_range_output_and_validation)'`（`(new)`，含范围内/范围外/行号移动/未匹配函数对照验证）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::test_log_line_range)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-16`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_-L行级历史输出与校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-16,DEP-LS-01; writeset=序列化于LS-16; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-18: 实现 `log --find-object=<object>`（#968）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--find-object=<object>`（可重复），筛选出使指定 blob/tree OID 在父树与提交树之间出现次数发生变化的提交。

**Out of scope:** `--pickaxe-all`（仅当后续需要时另开）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 仅有 `-S`/`-G` 文本 pickaxe，无 `find_object` | `src/command/log.rs:400-408` |

**Acceptance criteria:**

- [ ] `libra log --find-object=<oid>` 准确命中引入、修改或删除该对象 OID 的提交。
- [ ] 无效 `<oid>` 返回 `LBR-CLI-003` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::find_object)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::test_log_pickaxe)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-17`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--find-object; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-17,DEP-LS-01; writeset=序列化于LS-17; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-19: 实现 `log --cc` 合并提交紧凑组合差异（#833）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--cc`（dense combined diff），对合并提交仅输出在所有父提交中均发生变更的文件及其 `@@@` 组合 hunk，单父提交仍正常输出普通 patch。

**Out of scope:** `-c` 非紧凑组合差异或 `--combined-all-paths`（仅实现 `--cc`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `--cc` 字段 | `src/command/log.rs:172-486` |

**Acceptance criteria:**

- [ ] `libra log --cc` 对合并提交输出 `diff --cc` 紧凑组合差异（仅保留各方均改动的冲突/手动解决文件）。
- [ ] 对非合并提交输出与 `-p` 一致的普通 diff。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::log_cc_combined_diff)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::test_log_first_parent)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-18`, `DEP-LS-01`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--cc组合差异; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=1; prod-files=1; scope=M; deps=LS-18,DEP-LS-01; writeset=序列化于LS-18; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-20: `log` 支持 `--raw`、`--numstat` 与 `--dirstat` 差异格式选项（#964）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 补齐 `--raw` 与 `--numstat`（`--dirstat` 在 `log.rs` 已有 `compute_commit_dirstat` (`log.rs:3416`) + `utils::diff_format::format_dirstat`，且 `format_diff_dirstat` (`diff/render.rs:412`) 已为 `pub(crate)`，仅核实对齐）。按 `ADR-LS-01` 与 `DEP-LS-02` 等待 `issues/529.md` `FP-05`/`FP-07` `done`/`complete` 后直接复用其公开的 `pub(crate) mod render` 与独立 `numstat` 渲染函数（当前 `numstat` 为 `diff/render.rs:278` `render_diff_output` 内分支，不重复实现），并将 `src/command/diff/render.rs:503` 的 `format_diff_raw`（当前为 `pub(super)`）提升为 `pub(crate)` 供 `LogDiffOptions` 复用，且均参与 `-s`/`--no-patch` 顺序清除。

**Out of scope:** 不改变 `diff` 命令自身格式，不修改 `CommitDiffOptions`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 已有 `dirstat`/`dirstat_by_file` 与 `compute_commit_dirstat`（`log.rs:3416`），缺少 `raw` 与 `numstat`；`numstat` 当前为 `render_diff_output`（`diff/render.rs:278`）内分支（由 `529.md` `FP-07` 抽取）；`format_diff_raw` 位于 `diff/render.rs:503`（`pub(super)`），`format_diff_dirstat` 位于 `diff/render.rs:412`（已 `pub(crate)`） | `src/command/log.rs:201-280,3416`、`src/command/diff/render.rs:278,412,503` |

**Acceptance criteria:**

- [ ] `libra log --raw` 输出与 `git@6de20f6092` 一致的 `:oldmode newmode oldsha newsha status\tpath` 行。
- [ ] `libra log --numstat` 输出 `<ins>\t<del>\t<path>` 行。
- [ ] `--raw`/`--numstat`/`--dirstat` 均可被后续 `-s`/`--no-patch` 清除。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::raw_numstat_dirstat_formats)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::diff_test::test_diff_dirstat)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-19`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--raw/--numstat/--dirstat; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=LS-19,DEP-LS-02,DEP-LS-01; writeset=序列化于LS-19; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-21: `log` 透传 diff 空白忽略选项（`-w` / `-b`）（#967；拆出 LS-28、LS-29、LS-30）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `-w`/`--ignore-all-space` 与 `-b`/`--ignore-space-change` 并经 `LogDiffOptions` 复用 `src/command/diff/compare.rs:405-417` 与 `src/command/diff.rs:4728` 空白归一化对比逻辑。

**Out of scope:** `--no-prefix`/`--src-prefix`/`--dst-prefix`（`LS-28`）、`--full-index`（`LS-29`）、`-U`/`--unified=<n>`（`LS-30`）；不修改 `CommitDiffOptions`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 未暴露 `-w`/`-b`；空白忽略对比位于 `diff/compare.rs:405-417` 与 `diff.rs:4728` | `src/command/log.rs:172-486`、`src/command/diff/compare.rs:405-417`、`src/command/diff.rs:4728` |

**Acceptance criteria:**

- [ ] `-w` / `--ignore-all-space` 忽略所有空白变更。
- [ ] `-b` / `--ignore-space-change` 忽略空白数量变更。
- [ ] `LogDiffOptions` 默认值保持既有 `log -p` 输出向后兼容。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_ignore_whitespace_passthrough)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::test_log_patch)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-20`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_diff空白忽略(-w/-b); recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=LS-20,DEP-LS-01,DEP-LS-02; writeset=序列化于LS-20; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-31: `diff` 新增 `--color-moved-ws` 空白模式（#970；自 LS-22 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `src/command/diff.rs` 与 `src/command/diff/render.rs:782` 新增 `--color-moved-ws=<modes>`（`no`、`ignore-space-at-eol`、`ignore-space-change`、`ignore-all-space`、`allow-indentation-change`）解析与移动块空白归一化引擎，供 `libra diff` 直接使用并作为 `pub(crate)` 能力留待 `LS-22` 在 `log` 复用。

**Out of scope:** `LogArgs` 参数暴露（由 `LS-22` 处理）；`show --color-moved --word-diff` 组合（由 `LS-24` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 已有 `--color-moved` / `--no-color-moved`，`colorize_diff` 位于 `diff/render.rs:782`、`moved_line_bodies` 位于 `diff.rs:5456`，但尚未实现 `--color-moved-ws` | `src/command/diff.rs:438-445,5456`、`src/command/diff/render.rs:782` |

**Acceptance criteria:**

- [ ] `libra diff --color=always --color-moved --color-moved-ws=<mode>` 正确处理各空白忽略模式（`no`、`ignore-space-at-eol`、`ignore-space-change`、`ignore-all-space`、`allow-indentation-change`）。
- [ ] 非法 `--color-moved-ws` 值返回 `LBR-CLI-002` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新 `docs/commands/diff.md`、`docs/commands/zh-CN/diff.md`、`docs/development/commands/diff.md`、`COMPATIBILITY.md:227` 与 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::diff_test::color_moved_ws_modes)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::diff_test::test_diff_color_moved)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-30`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/diff_test.rs`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/diff_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/diff.md`、`docs/commands/zh-CN/diff.md`、`docs/development/commands/diff.md`、`COMPATIBILITY.md:227`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only diff --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=diff_--color-moved-ws空白模式; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=2; scope=M; deps=LS-30,DEP-LS-01,DEP-LS-02; writeset=序列化于LS-30; release=independent; split-from=LS-22; exception=N/A`

---

### Task LS-22: `log` 支持 `--color-moved` 与 `--color-moved-ws`（#970；拆出 LS-31）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--color-moved[=<mode>]`、`--no-color-moved` 与 `--color-moved-ws=<modes>`，经 `LogDiffOptions` 复用 `LS-31` 已交付的 `src/command/diff.rs` 与 `src/command/diff/render.rs:782` 移动块着色与空白忽略引擎。

**Out of scope:** `diff` 命令自身 `--color-moved-ws` 解析与文档（已由 `LS-31` 交付）；`show --color-moved --word-diff` 组合（由 `LS-24` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 缺 `--color-moved` / `--no-color-moved` / `--color-moved-ws`；`diff` 侧引擎由 `LS-31` 暴露为 `pub(crate)` | `src/command/log.rs:172-486`、`src/command/diff.rs:438-445,5456`、`src/command/diff/render.rs:782` |

**Acceptance criteria:**

- [ ] `libra log -p --color=always --color-moved` 与 `--color-moved-ws=<mode>` 高亮跨 hunk/跨文件移动行。
- [ ] 非法 `--color-moved` / `--color-moved-ws` 值返回 `LBR-CLI-002` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::color_moved_and_ws)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::diff_test::test_diff_color_moved)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-31`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--color-moved与--color-moved-ws复用; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=LS-31,DEP-LS-01,DEP-LS-02; writeset=序列化于LS-31; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-28: `log` 透传 diff 路径前缀选项（`--no-prefix`, `--src-prefix`, `--dst-prefix`）（#967；自 LS-21 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--no-prefix`、`--src-prefix=<p>` 与 `--dst-prefix=<p>`，经 `LogDiffOptions` 复用 `src/command/diff/options.rs:28,124-160` 路径前缀解析与渲染逻辑。

**Out of scope:** `-w`/`-b`（`LS-21`）、`--full-index`（`LS-29`）、`-U`（`LS-30`）；不修改 `CommitDiffOptions`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 未暴露前缀选项；前缀配置位于 `diff/options.rs:28,124-160` | `src/command/log.rs:172-486`、`src/command/diff/options.rs:28,124-160` |

**Acceptance criteria:**

- [ ] `--no-prefix` 省略 `a/` 与 `b/` 路径前缀。
- [ ] `--src-prefix=<p>` 替换旧侧路径前缀。
- [ ] `--dst-prefix=<p>` 替换新侧路径前缀。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_prefix_options_passthrough)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::diff_test::test_diff_relative)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-21`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_diff路径前缀(--no-prefix/--src-prefix/--dst-prefix); recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=LS-21,DEP-LS-01,DEP-LS-02; writeset=序列化于LS-21; release=independent; split-from=LS-21; exception=N/A`

---

### Task LS-29: `log` 透传 `--full-index` 输出完整对象哈希（#967；自 LS-21 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--full-index`，经 `LogDiffOptions` 复用 `src/command/diff/compare.rs:591` 在 `index` 头行输出完整 40/64 位对象哈希。

**Out of scope:** `--abbrev` 任意长度配置（已由 `LS-09` 处理）；不修改 `CommitDiffOptions`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 未暴露 `--full-index`；`index` 哈希格式位于 `diff/compare.rs:591` | `src/command/log.rs:172-486`、`src/command/diff/compare.rs:591` |

**Acceptance criteria:**

- [ ] `--full-index` 在 `libra log -p --full-index` 的 `index` 行输出完整对象哈希（匹配 `git log -p --full-index`）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_full_index_passthrough)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::test_log_patch)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-28`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_diff_--full-index; recovery=immutable-release; complete=yes; self-contained=yes; AC=1/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=LS-28,DEP-LS-01,DEP-LS-02; writeset=序列化于LS-28; release=independent; split-from=LS-21; exception=N/A`

---

### Task LS-30: `log` 透传 `-U` / `--unified=<n>` 控制 hunk 上下文行数（#967；自 LS-21 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `-U<n>` / `--unified=<n>`，经 `LogDiffOptions` 复用 `src/command/diff/options.rs:29`（`args.unified`）与 `src/command/diff/compare.rs:405-417` 控制 `log -p` 的 hunk 上下文行数。

**Out of scope:** `-w`/`-b`（`LS-21`）、路径前缀（`LS-28`）、`--full-index`（`LS-29`）；不修改 `CommitDiffOptions`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 未暴露 `-U`/`--unified`；`args.unified` 解析与上下文行数控制位于 `diff/options.rs:29` 与 `diff/compare.rs:405-417` | `src/command/log.rs:172-486`、`src/command/diff/options.rs:29`、`src/command/diff/compare.rs:405-417` |

**Acceptance criteria:**

- [ ] `-U<n>` / `--unified=<n>` 准确控制 `log -p` 的 hunk 上下文行数（含 `-U0` 零上下文与非法负值 `LBR-CLI-002`）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_unified_context_passthrough)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::diff_test::test_diff_unified)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-29`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md:196`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_diff_-U/--unified上下文行数; recovery=immutable-release; complete=yes; self-contained=yes; AC=1/8; VER=2/8; landing=4; prod-files=4; scope=M; deps=LS-29,DEP-LS-01,DEP-LS-02; writeset=序列化于LS-29; release=independent; split-from=LS-21; exception=N/A`

---

### Task LS-23: `show` 支持 `--patience` 与 `--anchored`（#857）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `ShowArgs` 新增 `--patience` 与 `--anchored=<text>`，经 `log.rs` `LogDiffOptions` 透传至 `src/command/diff.rs` 比较引擎（复用 `diff.rs` 既有 `patience` / `anchored`）。

**Out of scope:** 不改变 `show` 非提交对象（blob/tree/tag）输出。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 已实现 `--patience` 与 `--anchored`，`ShowArgs` 未暴露；`show.rs` 通过 `log::generate_diff` 生成差异 | `src/command/diff.rs:131-153`、`src/command/show.rs:25-28,73-187` |

**Acceptance criteria:**

- [ ] `libra show --patience` 生成与 `libra diff --patience` 一致的 patience 差异。
- [ ] `libra show --anchored=<text>` 生成与 `libra diff --anchored=<text>` 一致的锚点差异。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::patience_and_anchored)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::show_test::)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-22`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`COMPATIBILITY.md:199`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only show --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=show_--patience与--anchored; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=4; prod-files=4; scope=M; deps=LS-22,DEP-LS-02,DEP-LS-01; writeset=序列化于LS-22; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-26: `log` 与 `show` 支持 `--word-diff`、`--word-diff-regex` 与 `--color-words`（#966）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 为 `LogArgs` 与 `ShowArgs` 统一接入 `--word-diff[=<mode>]`、`--word-diff-regex=<regex>` 与 `--color-words[=<regex>]`，按 `ADR-LS-01` 经 `LogDiffOptions` 复用 `src/command/diff/render.rs:61`（`render_word_diff`）词级 diff 引擎。

**Out of scope:** `--color-moved` 与 `--word-diff` 在 `show` 中的组合行为（由随后的 `LS-24` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 支持 `word_diff`/`word_diff_regex`，渲染在 `diff/render.rs:61`，`LogArgs` 与 `ShowArgs` 均未暴露 | `src/command/diff.rs:179,192,876`、`src/command/diff/render.rs:61`、`src/command/log.rs:172-486`、`src/command/show.rs:73-187` |

**Acceptance criteria:**

- [ ] `libra log -p --word-diff` / `--color-words` 与 `libra show --word-diff` / `--color-words` 正确输出词级差异。
- [ ] `log` 与 `show` 均验证有效 `--word-diff-regex=<regex>` 与 `--color-words=<regex>` 实际改变词边界划分（含 `--color-words=<regex>` 隐含 `--word-diff=color` 及后写优先级，对照 `git log`/`git show`）。
- [ ] 非法 `--word-diff` 模式或非法正则返回 `LBR-CLI-002` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新 `log` 与 `show` 所列全部文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::word_diff_and_color_words) | test(=command::show_test::word_diff_and_color_words)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::diff_test::test_diff_word_diff)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-23`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/log.rs`, `src/command/show.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `tests/command/show_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/show.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/{log,show}.md`、`docs/commands/zh-CN/{log,show}.md`、`docs/development/commands/{log,show}.md`、`COMPATIBILITY.md:196,199`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）分别运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra` 与 `--only show`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log/show_--word-diff与--color-words; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=4; prod-files=4; scope=M; deps=LS-23,DEP-LS-02,DEP-LS-01; writeset=序列化于LS-23; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-24: `show` 支持 `--color-moved` 与 `--word-diff` 组合（#858）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LS-26` 已为 `show` 接入 `--word-diff` 的基础上，在 `ShowArgs` 接入 `--color-moved[=<mode>]`、`--no-color-moved`、`--color-moved-ws` 并启用与 `--word-diff` / `--color-words` 的组合输出（匹配 `git show` 与 `libra diff --color-moved --word-diff`）。

**Out of scope:** 独立的 `--word-diff` 实现（已由 `LS-26` 完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ShowArgs` 缺少 `color_moved` 字段；`show.rs` 通过 `log::generate_diff` 生成差异 | `src/command/show.rs:25-28,73-187` |

**Acceptance criteria:**

- [ ] `libra show --color-moved --word-diff` 正常执行且不再被 CLI 拒绝。
- [ ] 输出与 `libra diff --color-moved --word-diff` 语义一致。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::color_moved_with_word_diff)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::show_test::)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-26`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`COMPATIBILITY.md:199`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only show --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=show_--color-moved与--word-diff组合; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=4; prod-files=4; scope=M; deps=LS-26,DEP-LS-02,DEP-LS-01; writeset=序列化于LS-26; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-25: `show` 支持 `--textconv` 与 `--no-textconv`（#862）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `ShowArgs` 新增 `--textconv`（默认对有 `diff.<driver>.textconv` 配置的提交 diff 启用）与 `--no-textconv`，经 `log.rs` 复用 `src/command/diff.rs:3403` 的 `run_textconv` 命令执行路径（受信任的仓库/用户配置 `diff.<driver>.textconv` 经临时文件执行外部过滤器，无独立进程沙箱隔离，非零退出或缺失命令按 `DiffError::TextconvFailed` fail-closed 返回错误）。

**Out of scope:** 不改变 `diff.rs:584,590` 内部的 `for_service`（`no_textconv: true`）plumbing 辅助构造函数。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 已实现 `--textconv`/`--no-textconv` 与 `run_textconv`，`ShowArgs` 未暴露 | `src/command/diff.rs:497-511,3403`、`src/command/show.rs:25-28,73-187` |

**Acceptance criteria:**

- [ ] `libra show --textconv` 对配置了 `diff.<driver>.textconv` 的文件执行过滤器并展示转换后文本 diff。
- [ ] `libra show --no-textconv` 跳过过滤器并按原始字节/二进制提示展示。
- [ ] 过滤器返回非零退出码或命令缺失时，`libra show` 以非零退出码与明确 `TextconvFailed` 诊断 fail-closed 失败。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::textconv_and_no_textconv)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::textconv_failure_and_missing_command)'`（`(new)`，验证非零退出与缺失命令传播）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::diff_test::test_diff_textconv)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-24`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`COMPATIBILITY.md:199`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only show --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=show_--textconv与--no-textconv; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=3/8; landing=4; prod-files=4; scope=M; deps=LS-24,DEP-LS-02,DEP-LS-01; writeset=序列化于LS-24; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-27: `log` 与 `show` 支持 `--diff-filter`（#973）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 为 `LogArgs` 与 `ShowArgs` 接入 `--diff-filter=<filter>`，按 `ADR-LS-01` 复用 `src/command/diff.rs` 抽取的 `pub(crate)` `parse_diff_filter` / `apply_diff_filter` 过滤提交的变更文件列表与 diff 输出（`log` 同时过滤无匹配变更文件的提交）。

**Out of scope:** 不改变 `diff --diff-filter` 语义。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 已实现私有的 `parse_diff_filter`（2228）与 `apply_diff_filter`（2265），`LogArgs`/`ShowArgs` 未暴露 | `src/command/diff.rs:273,2228,2265`、`src/command/log.rs:172-486`、`src/command/show.rs:73-187` |

**Acceptance criteria:**

- [ ] `libra log --diff-filter=A` 与 `libra show --diff-filter=M` 仅输出符合过滤状态的文件及其变更提交。
- [ ] 非法 `--diff-filter` 字符返回 `LBR-CLI-002` 错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新 `log` 与 `show` 所列全部文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_filter) | test(=command::show_test::diff_filter)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::diff_test::test_diff_status_detection)'`

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-25`, `DEP-LS-01`, `DEP-LS-02`

**Implementation write set:** `src/command/log.rs`, `src/command/show.rs`, `src/command/diff.rs`, `tests/command/log_test.rs`, `tests/command/show_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/show.rs`, `src/command/diff.rs`, `tests/command/log_test.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/{log,show}.md`、`docs/commands/zh-CN/{log,show}.md`、`docs/development/commands/{log,show}.md`、`COMPATIBILITY.md:196,199`，并在 `/Volumes/Sea/libra-backend`（`cf` 分支）分别运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only log --libra-root /Volumes/Data/gitmono/libra` 与 `--only show`。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log/show_--diff-filter; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=3; prod-files=3; scope=M; deps=LS-25,DEP-LS-02,DEP-LS-01; writeset=序列化于LS-25; release=independent; split-from=N/A; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `log/config.rs`、`log/decorate_filter.rs`、`show.rs` `build_raw_lines` | `source .env.test && cargo nextest run --lib log`、`source .env.test && cargo nextest run --lib show` |
| 集成 | 全部 27 项 issue 行为用例 | `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::) | test(command::show_test::)'` |
| 兼容 | pretty 占位符、浅遍历、peel 选择器、配置默认值 | `source .env.test && cargo nextest run --test compat_pretty_format_placeholders --test compat_shallow_walk_helper --test compat_rev_parse_peel_selectors` |
| 迁移 | N/A（无 SQLite schema 变更） | N/A |
| 安全 | `--output=<file>` 路径校验、TTY 控制字符清理、`textconv` 失败传播 | `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::) | test(command::show_test::)'` |
| 性能 | `-L` / `--simplify-by-decoration` / `--find-object` 有界遍历 | `source .env.test && cargo nextest run --test command_test -E 'test(command::log_test::)'` |
| live/gated | 每卡 T-1 与收口阶段双环境全量回归 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| LS-01 | #949; `log.rs:2327,2441` | `src/command/log.rs` | `log.md` EN/zh/dev + `COMPATIBILITY.md:196` + 后端 | `command::log_test::decorate_nested_annotated_tags` `(new)` |
| LS-02 | #950; `log.rs:1020-1075` | `src/command/log.rs`, `log/config.rs` | 同上 | `command::log_test::log_decorate_bool_true_and_pretty_raw` `(new)` |
| LS-03 | #951; `log.rs:650-672` | `src/command/log.rs`, `log/config.rs` | 同上 | `command::log_test::log_exclude_decoration_config` `(new)` |
| LS-04 | #952; `decorate_filter.rs:15-55` | `src/command/log.rs`, `log/decorate_filter.rs` | 同上 | `command::log_test::decorate_refs_issue_952_remaining` `(new)` |
| LS-05 | #953; `log.rs:172-486` | `src/command/log.rs` | 同上 | `command::log_test::simplify_by_decoration` `(new)` |
| LS-06 | #954; `log/config.rs` | `src/command/log.rs`, `log/config.rs` | 同上 | `command::log_test::clear_decorations_and_initial_set` `(new)` |
| LS-07 | #959; `log.rs:1123` | `src/command/log.rs`, `log/config.rs` | 同上 | `command::log_test::color_decorate_and_commit_config` `(new)` |
| LS-08 | #960; `log.rs:2441` | `src/command/log.rs` | 同上 | `command::log_test::decorate_replaced_and_env_base` `(new)` |
| LS-09 | #958; `log.rs:190-191` | `src/command/log.rs` | 同上 | `command::log_test::no_abbrev_issue_958_remaining` `(new)` |
| LS-10 | #956; `log.rs:172-486` | `src/command/log.rs` | 同上 | `command::log_test::no_walk_and_tags` `(new)` |
| LS-11 | #965; `log.rs:228-233` | `src/command/log.rs` | 同上 | `command::log_test::before_and_after_aliases` `(new)` |
| LS-12 | #969; `log.rs:172-486,4245-4275` | `src/command/log.rs` | 同上 | `command::log_test::diff_reverse_flag_r` `(new)` |
| LS-13 | #955; `log.rs:2325,2667-2683`, `pager.rs:95-103` | `src/command/log.rs`, `src/utils/pager.rs` | 同上 | `command::log_test::tty_output_no_stray_escape_sequences` `(new)` |
| LS-14 | #972; `log.rs:234-240` | `src/command/log.rs` | 同上 | `command::log_test::empty_format_or_pretty_omits_header` `(new)` |
| LS-15 | #963; `log.rs:172-486` | `src/command/log.rs` | 同上 | `command::log_test::output_to_file` `(new)` |
| LS-16 | #962; `log.rs:470-471,2212` | `src/command/log.rs` | 同上 | `command::log_test::follow_boolean_flag_single_pathspec` `(new)` |
| LS-17 | #961; `log.rs:479,1981` | `src/command/log.rs` | 同上 | `command::log_test::line_range_output_and_validation` `(new)` |
| LS-18 | #968; `log.rs:400-408` | `src/command/log.rs` | 同上 | `command::log_test::find_object` `(new)` |
| LS-19 | #833; `log.rs:172-486` | `src/command/log.rs` | 同上 | `command::log_test::log_cc_combined_diff` `(new)` |
| LS-20 | #964; `log.rs:201-280`, `diff/render.rs:503` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs` | 同上 | `command::log_test::raw_numstat_dirstat_formats` `(new)` |
| LS-21 | #967; `diff/compare.rs:405-417` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs` | 同上 | `command::log_test::diff_ignore_whitespace_passthrough` `(new)` |
| LS-28 | #967; `diff/options.rs:28,124-160` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs` | 同上 | `command::log_test::diff_prefix_options_passthrough` `(new)` |
| LS-29 | #967; `diff/compare.rs:591` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs` | 同上 | `command::log_test::diff_full_index_passthrough` `(new)` |
| LS-30 | #967; `diff/options.rs:29`, `diff/compare.rs:405-417` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `src/command/diff/compare.rs` | 同上 | `command::log_test::diff_unified_context_passthrough` `(new)` |
| LS-31 | #970; `diff.rs:438-445,5456`, `diff/render.rs:782` | `src/command/diff.rs`, `src/command/diff/render.rs` | `diff.md` EN/zh/dev + `COMPATIBILITY.md:227` + 后端 | `command::diff_test::color_moved_ws_modes` `(new)` |
| LS-22 | #970; `log.rs:172-486`, `diff.rs:438-445`, `diff/render.rs:782` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs` | `log.md` EN/zh/dev + `COMPATIBILITY.md:196` + 后端 | `command::log_test::color_moved_and_ws` `(new)` |
| LS-23 | #857; `diff.rs:131-153`, `diff/options.rs:28` | `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs` | `show.md` EN/zh/dev + `COMPATIBILITY.md:199` + 后端 | `command::show_test::patience_and_anchored` `(new)` |
| LS-26 | #966; `diff.rs:179,192`, `diff/render.rs:61` | `src/command/log.rs`, `src/command/show.rs`, `src/command/diff.rs`, `src/command/diff/render.rs` | `log.md`/`show.md` EN/zh/dev + `COMPATIBILITY.md:196,199` + 后端 | `command::{log,show}_test::word_diff_and_color_words` `(new)` |
| LS-24 | #858; `show.rs:73-187` | `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs` | `show.md` EN/zh/dev + `COMPATIBILITY.md:199` + 后端 | `command::show_test::color_moved_with_word_diff` `(new)` |
| LS-25 | #862; `diff.rs:497-511,3403` | `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs` | 同上 | `command::show_test::textconv_and_no_textconv`, `textconv_failure_and_missing_command` `(new)` |
| LS-27 | #973; `diff.rs:273,2228,2265` | `src/command/log.rs`, `src/command/show.rs`, `src/command/diff.rs` | `log.md`/`show.md` EN/zh/dev + `COMPATIBILITY.md:196,199` + 后端 | `command::{log,show}_test::diff_filter` `(new)` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划评审通过且 `DEP-LS-01` 核实通过 | Claude & Codex `PASS` | N/A |
| M1 | Phase 0（`LS-01`..`LS-12`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 发布前 revert；发布后按 ER-10 新建 `FIX-*` 前滚发新版本 |
| M2 | Phase 1（`LS-13`..`LS-22`、`LS-28`..`LS-31`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 同上 |
| M3 | Phase 2（`LS-23`..`LS-27`）全部 `done`/`complete` + 全量收口门通过 | 全量 `nextest` + `fmt` + `clippy` 全绿 | 同上 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `log --output=<file>` 写入失败 | 无或部分目标文件（若非原子写） | 优先使用原子写入或返回 `LBR-IO-002` | 静默成功（exit 0） |
| 远端 D 组 CI 失败 | 已推送版本 tag 与提交 | 按 ER-04/ER-10 新建 `FIX-*` 前滚修复 | 回退已推送提交或删除已发布 tag |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `LS-16` 将 `--follow` 从带参改回布尔标志可能影响旧测试脚本 | 中：需同步更新既有 `--follow <file>` 测试 | 同卡更新 `log_test` 与中英文文档 | LS-16 |
| `LS-12` `#969` 标题误称 `--reverse` 短别名但 Git `-R` 反转 diff 输入方向 | 低：若按标题误实现会与 Git 冲突 | 明确按 `git log -R` 实现并澄清关闭 #969 | LS-12 |
| `src/command/log.rs` / `src/command/diff/` 跨计划写锁冲突 | 高：合并冲突 | 每卡开工前重新核验 `DEP-LS-01` 与（若涉 `diff/`）`DEP-LS-02` | LS-01..LS-30 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 历史遍历与 diff 渲染 | `O(N_commits)` | 受 `-n` / `--max-count` 与既有浅历史边界约束 | 继承 GC-10 | `command::log_test` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 第 196、199、227 行已原地同步。
- [ ] `docs/commands/{log,show,diff}.md` 已同步。
- [ ] `docs/commands/zh-CN/{log,show,diff}.md` 已同步。
- [ ] `docs/development/commands/{log,show,diff}.md` 已同步。
- [ ] `/Volumes/Sea/libra-backend` 已通过 `sync-libra-command-docs.py --write --only log/show/diff --libra-root /Volumes/Data/gitmono/libra` 同步。
- [ ] `docs/error-codes.md`：`N/A`（复用现有 `LBR-CLI-002`/`003`、`LBR-IO-002`）。
- [ ] `tests/INDEX.md` 已核对（仅扩充既有 `command_test`）。
- [ ] `Cargo.toml` `[[test]]`：`N/A`（未新增 `--test` target）。
- [ ] `plan-long.md`：`N/A`（由协调者统一维护）。

## Codex review log

| Round | Reviewer | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|---|
| R1 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `CommitDiffOptions`/`format_patch.rs` 漏写集、`diff.rs` vs `diff/`、`show` 漏 `log.rs`、`--color-moved-ws` 未实现、`LS-24`/`LS-26` 重叠、`LS-13` 无根因锚点 | `LS-21`/`23`/`04`/`12`/`16` 计数、边界与依赖修正 | 2026-11-17 03:40 修订 |
| R1 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `LS-12` `#969` `-R` 误作 `--reverse` | `Full-suite trigger` 显式写 `T-1`；`LS-25` 修正 `textconv` 信任边界 | R2 修订 |
| R2 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P0/P1: 未完成全量复审 | P2: `LS-25` 缺失败传播验证 | R3 修订 |
| R3 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `diff.rs` vs `diff/` 写集、`nextest` 子串 filter 零匹配 | P2: `LS-12` 去 `diff.rs`/`DEP-LS-02`、`LS-13` 去 `diff.rs:2574` 并锚定 `log.rs`/`pager.rs` | R4 修订 |
| R3 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `LS-17` 函数范围验收不足、`DEP-LS-01` 须逐卡核验、`LS-26` 有效正则验收 | 无 | R4 修订 |
| R4 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R5 重跑 |
| R4 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `LS-23..27` `landing` 计数误含测试文件超 `M ≤ 4` 上限（G-04） | 无 | R5 修订 |
| R5 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R6 重跑 |
| R5 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | 无 P0/P1（未完成全量语义复审） | P2: `LS-01,05,08-19` `landing` 误多计测试文件 | R6 修订 |
| R6 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `LS-21` 写集错列 `render.rs` 漏 `options.rs`/`compare.rs` 且跨 `>4` 生产文件上限；`DEP-LS-02` 漏 `diff/compare.rs` | P2: `LS-17` `log.rs:1907`、`LS-22` `diff.rs:5456`、`LS-20` `render.rs:503` | R7 修订 |
| R6 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `LS-21` 合并多轴违背 G-04；`LS-02..27` 缺 `DEP-LS-01` 与 `实施顺序` 不一致 | 无 | R7 修订 |
| R7 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `DEP-LS-02` 漏 `compare.rs` 且范围漏 `LS-28..30`；G-09 实施顺序/M2/风险表/评审结论未同步 | P2: `LS-22` 表写 `LS-21`、未闭合反引号、`LS-20` 证据、`LS-30` 漏 `options.rs:29` | R8 修订 |
| R7 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | 未完成全量审阅 | `LS-22` 审计表写 `LS-21`（卡内为 `LS-30`） | R8 修订 |
| R8 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `M2` 里程碑漏 `LS-28..LS-30` | P2: `LS-20` 证据、`LS-30` 追溯表、30 卡计数、`render_word_diff` | R9 修订 |
| R8 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `M2` 里程碑漏 `LS-28..LS-30` | P2: `LS-30` 追溯表漏 `diff/options.rs` | R9 修订 |
| R9 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | 无 P0/P1 | P2: `LS-20` 卡片描述与证据漏 `diff/render.rs:503` | R10 修订 |
| R9 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `PASS` | 无 P0/P1 | P2: `LS-20` 卡片证据漏 `diff/render.rs:503` | R10 修订 |
| R10 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: 缺 `issues/529.md` `FP-05`/`06`/`07` 跨计划写锁与顺序（`LS-20` 复用 `render`/`numstat`） | P2: `LS-20` `numstat`/`dirstat` 描述、`ADR-LS-01` 私有 `mod render`、`DEP-LS-02` 行号 `1545-1546`、文头 R9 | R10 修订 |
| R10 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: ① `DEP-LS-01`/`02` 跨计划写锁漏 `show.rs` 与全写集（`plan-status.md:1930`）；② 网站路径 `../libra-backend` 与 `AGENTS.md:135` `/Volumes/Sea/libra-backend` 不一致 | 无 | R10 修订 |
| R11 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R12 重跑 |
| R11 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: 网站交付仍允许回退相邻检出 `../libra-backend`（须严格统一为 `/Volumes/Sea/libra-backend`） | 无 | R11 修订 |
| R12 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R14 重跑 |
| R12 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: 未完成全量复审（未发现新缺陷） | 无 | R13 重跑 |
| R13 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `LS-29` `scope=S` 超出 G-04 上限（3 落点且新增公开参数，应为 `M`） | 无 | R13 修订 |
| R14 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R15 重跑 |
| R14 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: 未完成全量复审（未发现新缺陷；已确认 R13 `LS-29 scope=M` 与 `/Volumes/Sea/libra-backend` 已修订） | 无 | R15 重跑 |
| R15 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `FAIL` | P1: `LS-22` 同时改 `diff`/`log` 两公开命令且缺 `diff` 文档/网站/`diff_test`（拆出 `LS-31`） | P2: 关系表 `529` 写 `FP-06`、`LS-25` `diff.rs:584,590` | R15 修订 |
| R15 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `PASS` | 无 P0/P1 | 无 | R15 复审通过 |
| R16 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-log-show.md` | `PASS` | 无 P0/P1 | P2（7 项：ADR-LS-01/02 与概览 `LS-30`/`diff` 同步、LS-31 逗号分隔/`allow-indentation-change` 细则、LS-22 写集精简；均已登记为执行期可控残余风险） | R16 复审通过 |
| R16 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-log-show.md` | `PASS` | 无 P0/P1 | P2（概览第 103 行 DAG 摘要未列出 `LS-31`，已登记为可接受残余风险） | R16 复审通过 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-LS-01 | `log --expand-tabs` / `--mailmap` / `--show-signature` | 不在本次 27 项 issue 范围内 | 后续新 issue 需求 | `COMPATIBILITY.md` |

## 完成判据

- [ ] 所有任务卡满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）。
- [ ] 全部任务卡完成后运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿，且修复全量暴露的全部 Bug（ER-13）。
- [ ] Claude 与 Codex 最终评审结论均为 `PASS`，P0/P1 全部关闭。
- [ ] 每张 `independent` 发布卡已执行 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新、提交推送及 `gh release create` 完成并取得 D 组证据（ER-08）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
