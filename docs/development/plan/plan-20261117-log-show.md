# log 与 show Git 兼容补齐计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Claude R41 与 Codex R41 双审 PASS（受审 SHA-256 前 12 位 `859bb860dec8`），计划可提交；网站检出路径已由用户 2026-11-18 11:55:09 UTC 确认（`DEP-LS-03` 已满足）；全部任务 pending，尚未执行；记录时间 2026-11-18 12:37:13 UTC。
> 审阅合同：按用户 2026-11-18 指令，本计划的计划文本提交门为 Claude（Subagent）与 Codex 双审，二者对同一受审字节（同一 SHA-256）均给出 `VERDICT: PASS` 方可提交（取代 2026-11-17「Codex 单审、不再要求 Claude Review」口径）；网站交付仍须独立的 Claude Code `PASS`，与计划文本提交门不可互相替代。
> 跟踪范围：GitHub open issues #833、#857、#858、#862、#949–#956、#958–#970、#972–#973（共 27 项）。

## 文档职责

- **网站交付路径（权威）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支），依据是用户 2026-11-18 11:55:09 UTC 确认：本批计划的网站文档交付检出为该路径（记为 `DEP-LS-03`，已满足）；该路径同时是模板 ER-06a 默认的 Libra checkout 同级 `../libra-backend`（`plan-template.md:332`）。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`，该路径在本机不存在（2026-11-18 核对），属已知文档滞后；用户决定本批不改 `AGENTS.md`，本计划以上述确认为准，也不把该路径归因于 `AGENTS.md`。修订历史 2026-11-18 08:19:08 行所称「用户最新指令」不作为依据，以本条确认为准。2026-11-18 核对：`/Volumes/Data/gitmono/libra-backend` 为 Libra 检出（有 `.libra/`、无 `.git`），分支 `cf`。写入前须按「文档与网站验收命令」第 2 步核验该路径存在、仅含单一 VCS 元数据且（必要时按该步切换后）分支为 `cf`；按该步仍不满足即 STOP，禁止改用其它检出。

本文解决 `libra log` 与 `libra show` 相对于 Git（基线 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`，v2.56.0-116）的 27 项已登记兼容缺口，目标是使 `log` 与 `show` 在装饰解析与着色、遍历选项、`-L`/`--follow`、`--output`、TTY 控制序列、空格式串与 diff 渲染选项透传上与 Git 保持一致。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收（GC-01）。

### 适用范围

- 命令与 CLI：`libra log` 与 `libra show` 参数解析、装饰与格式化渲染、diff 选项透传（`src/command/log.rs`、`src/command/log/config.rs`、`src/command/log/decorate_filter.rs`、`src/command/show.rs`、`src/command/diff.rs`、`src/command/diff/{render,options,compare}.rs`、`src/utils/pager.rs`；`src/cli.rs` 的 `Log`/`Show`/`Diff` 注册不变）。
- 配置与环境变量：`log.decorate`、`log.excludeDecoration`、`log.initialDecorationSet`、`color.decorate.*`、`color.diff.commit`（及旧名 `diff.color.commit`）、`GIT_REPLACE_REF_BASE`。
- 测试与文档：`tests/command/log_test.rs`、`tests/command/show_test.rs`、`tests/command/diff_test.rs`（均属 `command_test` 目标）、`tests/compat/pretty_format_placeholders_test.rs`（及只读回归 `compat_config_defaults_semantics`、`compat_shallow_walk_helper`、`compat_diff_review_options`）、`docs/commands/{log,show,diff}.md`、`docs/commands/zh-CN/{log,show,diff}.md`、`docs/development/commands/{log,show,diff}.md`、`COMPATIBILITY.md`（第 196、199、227 行原地更新）及 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/{log,show,diff}.en.md`。

### 非目标

- 不重复实现 `issues/534.md` 已完成并关闭的 `LO-00..LO-09` 能力；对 `#952` 与 `#958` 按 GC-01 核实剩余失败项后再修复。
- 不改变 `libra diff` 命令自身的默认参数与 JSON schema，不修改 `CommitDiffOptions`（避免触及 `src/command/format_patch.rs`）。
- 不实现 `--expand-tabs[=<n>]`、`--mailmap` 或 `--show-signature`（未在 27 项 issue 范围内）。

### 成功定义

- 27 个关联 issue 全部在「追溯表」与任务卡中闭环，对应行为与边界测试通过。
- `docs/commands/{log,show,diff}.md`、`docs/commands/zh-CN/{log,show,diff}.md`、`docs/development/commands/{log,show,diff}.md`、`COMPATIBILITY.md` 与后端网站文档同步反映新增 flags 与 config。
- 全部任务卡完成后通过 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 及 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

## 使用规则

- 每次状态推进与「修订历史」写入都记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时间。
- 推进或完成任务卡时，执行者须在同一变更中同步更新 [`plan-status.md`](plan-status.md)（本计划入库时的初始登记行由协调者随本批计划合入一并登记）。
- 本文全部任务卡均未开工；任务 ID（`LS-01`..`LS-37`）固定，拆分时按 G-09 在所属 Phase 末尾追加新编号并登记「修订历史」。
- 本计划不单独新建证据目录或证据文件（v2.12），所有门禁结果、SHA 与测试摘要写在本计划内。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。以下锚点初次核对于 `2026-11-17 03:40:00 UTC`（`Cargo.toml` = `0.30.207`），并在 2026-11-18 双审 R17–R27 期间对当时检出（`Cargo.toml` = `0.30.210`）逐一复核其内容与行号范围。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `LogArgs`（172-486）、`determine_decorate_option`（1036-1075）、`apply_follow_and_line_filters`（1981-2028）、`CommitDiffOptions`（4254）；`ShowArgs`（73-187）、`show_commit`（580-707）；`diff.rs` 的 `parse_diff_filter`（2228）、`apply_diff_filter`（2265）、`run_textconv`（3403）当前均为私有；`log.rs` 当前使用 `git_internal::diff` 而非 `command::diff` | `src/command/log.rs:172-5070`、`src/command/show.rs:25-28,73-1810`、`src/command/diff.rs:129-511,2228,2265,3403`、`src/command/log/{config,decorate_filter}.rs`、`src/utils/pager.rs` |
| 数据/状态 | 只读 commit/tree/blob/tag/notes/replace refs/config，不修改 SQLite/HEAD/index；`--output=<file>` 将新增文件写入目标路径 | `src/command/log.rs:1065,2188`、`src/command/show.rs:315` |
| 用户命令 | `Commands::Log(command::log::LogArgs)`（396）、`Commands::Show(command::show::ShowArgs)`（493）；`log` 选项顺序经 `command::log::diff_format_positions` 恢复（3531），分发于 `Commands::Log`（3895）与 `Commands::Show`（3948） | `src/cli.rs:396,493,3531,3895,3948`、`src/command/log.rs:2073` |
| 机器输出 | `LogOutput { commits: Vec<LogCommitEntry>, total }`、`ShowOutput` enum（`commit`/`tag`/`tree`/`blob`） | `src/command/log.rs:542-576`、`src/command/show.rs:189-259` |
| 错误码 | 复用 `LBR-CLI-002`（`CliInvalidArguments`，默认 exit 129）、`LBR-CLI-003`（`CliInvalidTarget`，默认 exit 129）、`LBR-IO-001`（`IoReadFailed`，默认 exit 128；textconv 失败经 `diff.rs:761-765` 映射）、`LBR-IO-002`（`IoWriteFailed`，默认 exit 128）、`LBR-REPO-001`（以上为默认退出码；`LIBRA_FINE_EXIT_CODES` 与显式覆盖见 `src/utils/error.rs:514-545,1068-1076`） | `src/utils/error.rs:374-393`、`docs/error-codes.md:98-120`、`src/command/diff.rs:675-676,761-765` |
| 文档 | `log`/`show`/`diff` 的中英文命令页、开发文档及兼容矩阵第 196、199、227 行已存在；网站页 `apps/tanstack-app/content/docs/commands/{log,show,diff}.en.md` 存在（网站 zh-CN 渲染时镜像 EN）。网站交付路径以「文档职责」为准（`/Volumes/Data/gitmono/libra-backend`，`cf`，用户 2026-11-18 11:55:09 UTC 确认，即 `DEP-LS-03`；`AGENTS.md:135` 的 `/Volumes/Sea/libra-backend` 本机不存在，属已知文档滞后）；同步脚本 `scripts/sync-libra-command-docs.py` 默认只读漂移检查（exit 0 一致 / 1 漂移 / 2 拒绝），`--write --only <cmd…>` 仅在原生 Libra `cf` 上写入 | `docs/commands/{log,show,diff}.md`、`docs/commands/zh-CN/{log,show,diff}.md`、`docs/development/commands/{log,show,diff}.md`、`COMPATIBILITY.md:196,199,227`、`/Volumes/Data/gitmono/libra-backend/scripts/README-command-doc-sync.md` |
| 测试 | `tests/command/{diff,log,show}_test.rs` 经 `tests/command_test.rs` 的 `mod command` 挂载于 `command_test`（nextest 名形如 `command::log_test::<fn>`）；compat：`compat_pretty_format_placeholders`、`compat_config_defaults_semantics`、`compat_shallow_walk_helper`、`compat_diff_review_options`、`compat_rev_parse_peel_selectors` | `tests/command_test.rs:10`、`tests/command/mod.rs:1043,1074,1140`、`Cargo.toml:500,572,576,600,604`、`tests/INDEX.md:74,75,82,101,102` |
| 外部参照 | 上游 Git 测试树 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0-116）；已完成计划 `issues/534.md`（`LO-00..LO-09` done/complete） | `docs/development/plan/issues/534.md:3`（2026-11-17 核对） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-LS-01 | `#949`：`log --decorate` 丢弃嵌套 annotated-tag 解引用链且 `%D`/`%d` 输出为空 | 嵌套 tag 无法显示装饰 | `src/command/log.rs:4202-4242` | LS-01 |
| GAP-LS-02 | `#950`：`log.decorate=true` 不启用装饰，且 `--pretty=raw` 受配置装饰 | 配置布尔值无效且 raw 格式污染 | `src/command/log.rs:1020-1075` | LS-02 |
| GAP-LS-03 | `#951`：忽略 `log.excludeDecoration`，无值 key 不报缺失值诊断（Libra 配置值 `NOT NULL`、无无值键形态，该诊断不适用，按有意差异登记） | 无法从配置排除装饰 | `src/command/log.rs:646-672` | LS-03 |
| GAP-LS-04 | `#952`：`--decorate-refs` 无前缀通配、HEAD 过滤与 ref 顺序剩余失败（`534` 后仍 OPEN） | 与 Git 装饰过滤仍存在边界差异 | `src/command/log/decorate_filter.rs:15-55`、`issues/534.md:3` | LS-04 |
| GAP-LS-05 | `#953`：`log` 拒绝 `--simplify-by-decoration` | 无法按装饰简化历史图 | `src/command/log.rs:172-486` | LS-05 |
| GAP-LS-06 | `#954`：`log` 拒绝 `--clear-decorations` 且忽略 `log.initialDecorationSet` | 无法重置初始装饰集 | `src/command/log.rs:172-486` | LS-06 |
| GAP-LS-07 | `#959`：`log --decorate --color` 忽略 `color.decorate.*` 与 `diff.color.commit` | 装饰与提交头颜色不可配置 | `src/command/log.rs:1120-1126` | LS-07 |
| GAP-LS-08 | `#960`：`log --decorate` 省略 `replaced` 标记且忽略 `GIT_REPLACE_REF_BASE` | 替换对象无装饰提示 | `src/command/log.rs:4202-4242` | LS-08 |
| GAP-LS-09 | `#958`：`--no-abbrev` 在全对象名输出上仍有开放失败项（`534` 后仍 OPEN） | 部分输出路径仍被截断 | `src/command/log.rs:190-191`、`issues/534.md:3` | LS-09 |
| GAP-LS-10 | `#956`：`log` 拒绝 `--no-walk` 与 `--tags` | 无法仅展示指定提交或全部 tag | `src/command/log.rs:172-486` | LS-10 |
| GAP-LS-11 | `#965`：`log` 拒绝 `--before` / `--after`（仅 `--since`/`--until` 可用） | Git 时间过滤别名不可用 | `src/command/log.rs:228-233` | LS-11 |
| GAP-LS-12 | `#969`：`log` 拒绝 `-R`（issue 标题误称 `--reverse` 短别名，Git 实为反转 diff 输入方向） | 常用反向 diff 旗标报错 | `src/command/log.rs:172-486,4245-4275` | LS-12 |
| GAP-LS-13 | `#955`：`log` 在 TTY 下输出多余 `CR` / `ESC[m` / `CR ESC[K` 序列（涉及 `log.rs` 分页写入与 `pager.rs`） | 交互终端或伪终端测试输出脏字节 | `src/command/log.rs:2325,2667-2683`、`src/utils/pager.rs:95-103` | LS-13 |
| GAP-LS-14 | `#972`：`log` 忽略空 `--format=`/`--pretty=` 并打印默认提交头 | 与 Git 空格式不输出任何提交头字节不符 | `src/command/log.rs:2334-2344,3096-3099` | LS-14 |
| GAP-LS-15 | `#963`：`log` 拒绝 `--output=<file>` | 无法直接将 log 写入文件 | `src/command/log.rs:172-486` | LS-15、LS-36、LS-37 |
| GAP-LS-16 | `#962`：`log --follow` 要求接 `FILE` 参数而非布尔标志 | `libra log --follow -- <file>` 被拒绝 | `src/command/log.rs:469-475` | LS-16 |
| GAP-LS-17 | `#961`：`log` 接受 `-L` 但忽略输出与 pathspec/`--follow` 互斥校验 | 行级历史无有效输出与校验 | `src/command/log.rs:477-480,1894,1981` | LS-17、LS-32 |
| GAP-LS-18 | `#968`：`log` 拒绝 `--find-object=<object>` | 无法按对象 OID 搜索历史 | `src/command/log.rs:172-486` | LS-18 |
| GAP-LS-19 | `#833`：`log` 拒绝 `--cc` | 无法查看合并提交的紧凑组合差异 | `src/command/log.rs:172-486` | LS-19 |
| GAP-LS-20 | `#964`：`log` 拒绝 `--raw`、`--numstat` 与 `--dirstat` | 常用 diff 统计格式不可用 | `src/command/log.rs:172-486` | LS-20 |
| GAP-LS-21 | `#967`：`log` 不接受 diff 输出选项（`-w`、`-b`、`--no-prefix`、`--full-index`、`-U`、`--src-prefix`、`--dst-prefix`） | 无法控制 log diff 空白与前缀 | `src/command/log.rs:172-486` | LS-21、LS-28、LS-29、LS-30 |
| GAP-LS-22 | `#970`：`log` 拒绝 `--color-moved` 与 `--color-moved-ws`（`diff.rs` 已有 `--color-moved`，但尚未实现 `--color-moved-ws`） | 无法使用移动块着色与空白模式 | `src/command/log.rs:172-486`、`src/command/diff.rs:438-445` | LS-31、LS-35、LS-22 |
| GAP-LS-23 | `#857`：`show` 拒绝 `--patience` 与 `--anchored` | 无法在 `show` 指定对比算法 | `src/command/show.rs:73-187`、`src/command/diff.rs:131-153` | LS-23 |
| GAP-LS-24 | `#858`：`show` 拒绝 `--color-moved` 与 `--word-diff` 组合 | 合法 diff 选项组合在 `show` 报错 | `src/command/show.rs:73-187` | LS-24 |
| GAP-LS-25 | `#862`：`show` 拒绝 `--textconv` / `--no-textconv` | 无法在 `show` 控制 textconv 过滤器 | `src/command/show.rs:73-187`、`src/command/diff.rs:497-511,3403` | LS-25 |
| GAP-LS-26 | `#966`：`log` 与 `show` 拒绝 `--word-diff` 与 `--color-words` | 无法词级差异高亮 | `src/command/log.rs:172-486`、`src/command/show.rs:73-187`、`src/command/diff.rs:179` | LS-26、LS-33 |
| GAP-LS-27 | `#973`：`log` 与 `show` 拒绝 `--diff-filter` | 无法按状态过滤变更文件 | `src/command/log.rs:172-486`、`src/command/show.rs:73-187`、`src/command/diff.rs:273,2228,2265` | LS-27、LS-34 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `issues/534.md` | 已完成关闭（`LO-00..LO-09` done/complete） | 复用其既有装饰/abbrev 框架，仅修复 `#952`、`#958` 剩余开放项 |
| `issues/1040.md` / `plan-20261117-format-patch.md` | 均触及 `src/command/log.rs` | 通过 `DEP-LS-01` 协调 `src/command/log.rs` 写锁串行（每卡开工前逐卡重新核验）；本计划不修改 `CommitDiffOptions`，不触及 `src/command/format_patch.rs` |
| `issues/478.md`（`LG-02/04/05/06/08`）/ `issues/1055.md`（`I1055-02`） | 触及 `src/command/show.rs`、`tests/command/show_test.rs`、`docs/commands/show.md`、`COMPATIBILITY.md:199`（见 `plan-status.md:1930` `DEP-I1055-478-SHOW`） | 纳入 `DEP-LS-01` 全写集逐卡互斥核验（涉及 `show.rs` 或共享文档/测试的卡开工前须确认对方无 `in-progress`/`blocked` 写者，否则 fail-closed 等待） |
| `issues/529.md`（`FP-05`/`FP-07`） | 触及 `src/command/diff/`（`FP-05` 提升私有 `mod render` 可见性，`FP-07` 从 `render_diff_output` 抽取独立 `numstat` 渲染函数；`FP-06` 写 `format_patch.rs`/`cli.rs` 与文档） | 纳入 `DEP-LS-02` 协调串行，且明确 **`LS-20` 须在 `529.md` `FP-05`/`FP-07` `done`/`complete` 后开工**，直接复用其 `pub(crate) mod render` 与独立 `numstat` 渲染函数，不重复实现（遵守 `GC-FP-01`） |
| `plan-20260927.md`（CM-08/09） / `plan-20261117-format-patch.md`（FPG-22/27） | 触及 `src/command/diff/`（含 `diff.rs`、`diff/render.rs`、`diff/options.rs`、`diff/compare.rs`） | 通过 `DEP-LS-02` 协调 `src/command/diff/` 写锁串行（每卡开工前逐卡重新核验） |
| `COMPATIBILITY.md` | 第 196 行（`log`）、第 199 行（`show`）、第 227 行（`diff`） | 仅原地修改对应行，不增删行 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 27 项 issue 均指向明确的 `log`/`show` Git 兼容缺口 | 拆分为 37 张单轴卡 `LS-01`..`LS-37`（覆盖 27 项 issue，含 `LS-21` 拆出 `LS-28..30`、`LS-22` 拆出 `LS-31`、`LS-17` 拆出 `LS-32`、`LS-26` 拆出 `LS-33`、`LS-27` 拆出 `LS-34`、`LS-31` 拆出 `LS-35`、`LS-15` 拆出 `LS-36` 与 `LS-37`） |
| 可行性 | 集中在 `log.rs`、`show.rs`、`diff/`（`diff.rs`、`diff/render.rs`、`diff/options.rs`、`diff/compare.rs`）适配复用及 `pager.rs` | 通过 `DEP-LS-01`/`DEP-LS-02` 逐卡核验避开跨计划写锁 |
| 任务卡粒度 | 37 张卡均为单行为轴、每卡至多一个公开命令接口变化、`AC ≤ 8`（按独立谓词计数，见「验证执行协议」第 6 项）、`VER ≤ 8`、`scope = M`、`landing` 按目录计 | 无 L/XL 卡，无多轴卡 |
| 依赖与顺序 | 线性 DAG `DEP-LS-01 -> LS-01 -> LS-02 -> LS-03 -> LS-04 -> LS-05 -> LS-06 -> LS-07 -> LS-08 -> LS-09 -> LS-10 -> LS-11 -> LS-12 -> LS-13 -> LS-14 -> LS-15 -> LS-37 -> LS-36 -> LS-16 -> LS-17 -> LS-32 -> LS-18 -> LS-19 -> LS-20 -> LS-21 -> LS-28 -> LS-29 -> LS-30 -> LS-31 -> LS-35 -> LS-22 -> LS-23 -> LS-26 -> LS-33 -> LS-24 -> LS-25 -> LS-27 -> LS-34`（叠加 `DEP-LS-02` 至涉 `src/command/diff/` 的 `LS-20..LS-31` 与 `LS-35`；`DEP-LS-03` 至全部卡的网站步骤） | 无环、无 I–I 并发冲突 |
| 完整性 | 每卡含 `nextest` 精确 filter、ER-13 `T-1` 显式触发（`LS-16` 另按 `T-3` 口径记录）、D 组真实 CI job、EN/zh/dev/后端脚本/`COMPATIBILITY.md` | 符合 ER-04、ER-06/ER-06a、ER-08、ER-13、ER-14 |
| 安全性 | `--output=<file>` 的 `.libra/` 防护（路径预检 + 描述符级 `(dev, inode)` 核验，截断前拒绝；威胁模型限于误指定的静态目标，竞态回归见 `DEFER-LS-02`）、TTY 控制字符清理、`textconv` 信任边界说明 | 继承 GC-07、GC-11 |
| 功能正确性 | 依据 `git@6de20f6092` 源码与同构夹具逐项验证边界条件（含 `-R` 反转 diff 输入、`-L :fn:file` 函数边界、空 `--format=` 零字节、`log.decorate` 非法值与 `log.excludeDecoration` 无值键的 Git 非致命语义） | 消除模糊判据 |
| 接口兼容 | `LogOutput` / `ShowOutput` JSON 保持向后兼容；`LS-16` 修正 `--follow` 为 Git 兼容无参标志并保留已弃用的 `--follow=<FILE>` 写法，`patch` 版本内不破坏既有脚本（ER-08） | 登记 `T-3` 口径与 `COMPATIBILITY.md` 有意差异 |
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
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件 | N/A | 全文对应章节 |
| 原记 `2026-11-18 10:35:00 UTC`（不可证实：晚于该行实际写入；文件 mtime 只表明写入不晚于 2026-11-18 09:43:54 UTC，实际时刻未知，见 R18 纠错行） | Codex 复审 FAIL 修复（验证契约与门禁） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 10:17:10 UTC | 用户 2026-11-18 指令（计划提交门口径） | 计划文本提交门由「Codex 单审」改为「Claude（Subagent）与 Codex 双审，二者对同一受审字节均 `VERDICT: PASS` 方可提交」；仅改文头审阅合同与完成判据中的计划文本提交门措辞，不改各卡 ER-05 评审门与网站交付 Claude Code `PASS` 门 | N/A | 文头审阅合同、完成判据 |
| 2026-11-18 10:39:24 UTC | Claude R17 FAIL + Codex R17 FAIL（2026-11-18 双审第 1 轮，受审 SHA-256 `129d461e8719`） | ① 新增真实存在的「文档与网站验收命令」7 步（逐文件 SHA-256 与关键字、网站检出/`cf` 预检、同步与只读漂移检查、`pnpm typecheck`/`pnpm build`、Claude Code PASS、精确签名提交与推送核对、部署 HTTP 核验），各卡 `Docs and compatibility impact` 逐文件列出网站页；② 重写「验证执行协议」：卡特有用例一卡一测试精确 `test(=…)`、共享回归组改为精确并集/正则族/整 target 并按 N 计数，补只读不变量与 Git 期望取得方式、AC 独立谓词计数口径；全部回归门改写，去掉 `\|` 并集的卡特有门；③ 全部卡重写 AC 与「AC → 断言映射」为逐条可观测期望（exit code、stdout/stderr 精确内容），删除机械生成的矛盾文案；④ 按 Git 源码与 2.54.0 预探纠正 LS-02（`format.pretty=raw` 不抑制、非法值不报错）、LS-03（无值键非致命）、LS-06（仅 `all` 生效）、LS-14（空格式零字节）、LS-25（`LBR-IO-001`/exit 128）；⑤ 按 G-03/G-04 拆卡：`LS-17` 拆出 `LS-32`（`-L` 参数校验）、`LS-26` 拆出 `LS-33`（`show` 词级 diff）、`LS-27` 拆出 `LS-34`（`show --diff-filter`），重算全部 AC/VER 与按目录计的 landing；⑥ `ADR-LS-01` 明确 `LogDiffOptions` 由 `LS-12` 引入且不改 `CommitDiffOptions`/`generate_diff_with_options`；`LS-16` 保留已弃用 `--follow=<FILE>` 以免 patch 破坏性变更；⑦ 更正网站路径归因（`AGENTS.md:135` 原文为 `/Volumes/Sea/libra-backend`），恢复被改写的 04:33/04:37 修订行与 R10/R11/R14 Codex 评审行原文；更正未来时间戳；GC-05 按卡命令选择 `log`/`show`/`diff`；追溯表恢复卡特有测试；⑧ 2026-11-18 09:43:54 行所述「文档与网站验收命令」块与 `test(/^X::/)` 统一实际未写入，本行补齐 | LS-17 → LS-17, LS-32；LS-26 → LS-26, LS-33；LS-27 → LS-27, LS-34 | 文头、文档职责、适用范围、事实基线、缺口表、评审结论、ADR-LS-01/02、GC-05、执行检查必备需求、实施顺序、发布窗口、Phase 1/2、粒度审计表、全部任务卡、测试矩阵、追溯表、里程碑、风险表、兼容收口、Review log |
| 2026-11-18 10:48:35 UTC | Claude R18 FAIL + Codex R18 FAIL（2026-11-18 双审第 2 轮，受审 SHA-256 `f1841e1cf0e4`） | ① 完成判据把全量收口门绑定最终代码树（`docs/development/plan/` 以外路径），纯计划记账提交不触发重跑，并按 v2.14 预登记终止链末端提交 D 组证据落点；② 网站验收：页面无字节变化时以漂移检查 exit 0 + 未变 SHA-256 为通过路径，完成判据同口径；部署核验改为匹配本次新增的唯一文本，不再依赖可为 N/A 的关键字；③ 测试矩阵的 `\|` 并集命令移入代码块，表格不再含需转义的命令；④ 所有比较父提交的卡（LS-21..LS-26、LS-28..LS-30、LS-33）夹具显式给出基线提交 B 与目标提交，LS-18/LS-27 夹具补齐被删除文件的来源；⑤ 按独立谓词重写合并 AC：LS-02/03/06/13/15/22/23/24/25/26/28/33/34 改为单一可观测谓词与逐字期望，`LS-31` 拆出 `LS-35`（`allow-indentation-change` 及互斥）；LS-26/LS-33 的 `--color-words` 与 `--word-diff=<mode>` 组合沿用 `libra diff` 现有优先级（`diff.rs:1381-1388` 恒以 `--color-words` 优先），与 Git 后写者胜出的差异登记于 Out of scope；⑥ 09:43:54 行时间改回原记录值并标注不可证实，不再以 mtime 上界冒充实际时间（本行为纠错记录）；⑦ 网站检出授权：依据改为可核验的模板 ER-06a 同级默认，新增 `DEP-LS-03`（用户书面确认网站检出路径）作为全部卡网站步骤前置 | LS-31 → LS-31, LS-35 | 文头、文档职责、事实基线、缺口表、关系表、评审结论、修订历史、ADR-LS-01/02、文档与网站验收命令、实施顺序、依赖登记表、发布窗口、Phase 1、粒度审计表、全部任务卡、测试矩阵、追溯表、里程碑、风险表、完成判据、Review log |
| 2026-11-18 10:54:22 UTC | Claude R19 FAIL + Codex R19 FAIL（2026-11-18 双审第 3 轮，受审 SHA-256 `ac499ff03e88`） | ① 网站无字节变化的通过路径增加远端 `cf` 同字节与生产页命中证据，目标页面有未提交改动即 STOP；② `DEP-LS-03` 与网站预检（第 1–2 步）移到 C 组第 ⑦ 步签名提交之前，失败则不得合并或发布；③ 部署核验按差异类型（新增/仅删除/仅重排）选取部署指纹，删除与重排均可执行；④ `LS-31` 增加 `ignore-all-space,no` 的清除断言；⑤ 按独立谓词重排 `LS-15`：失败码与 `.libra/` 不写入分条计数，`--json` 写文件拆为 `LS-36`（`LS-15` 期间该组合以 `LBR-CLI-002` 拒绝，`LS-36` 同卡删除 `output_to_file` 中被取代的拒绝断言，「验证执行协议」第 4 项只读不变量例外补入 `LS-36`）；`LS-16` AC5、`LS-35` AC1 去掉附带谓词，`LS-35` 删去被模块族覆盖的重复回归门；⑥ 发布窗口与 DAG 摘要改为逐卡显式列出 | LS-15 → LS-15, LS-36 | 文头、文档职责、缺口表、评审结论、ADR-LS-02、文档与网站验收命令、实施顺序、依赖登记表、发布窗口、Phase 1、粒度审计表、LS-15/16/31/35/36、追溯表、里程碑、风险表、完成判据、Review log |
| 2026-11-18 10:59:10 UTC | Claude R20 FAIL + Codex R20 FAIL（2026-11-18 双审第 4 轮，受审 SHA-256 `f68d318b14d9`） | ① 部署核验对同一次响应先判 `curl` 退出码与 HTTP 200 再按 `rg` 0/1/>1 判定，删除型指纹不再受管道掩盖；② 无网站字节变化分支改为远端 `cf` 同字节 + 本计划已记录部署指纹的生产复核，无指纹的基线页明确不由本计划背书，第 4–7 步记「本卡无网站提交」；③ `LS-31` 每条 AC 只断言一个变体（基线只看 Vall，逗号组合只看 Vchg），新增 `ignore-all-space,no` 清除断言；④ `.libra/` 防护从 `LS-15` 拆出为 `LS-37`：拒绝码、路径集合不变、已有内部文件 SHA-256 不变、符号链接拒绝与目标不变分条验收，风险表登记 `LS-15`→`LS-37` 单版本窗口；⑤ 「验证执行协议」第 4 项限定 Git 期望取得规则只用于显式标注的断言，Libra 专有行为以 Libra 基准为准，断言映射说明按卡是否涉及 Git 生成；⑥ 第 3 步第二条命令补全 `cd` 使其自包含（Codex 所称缺 `--libra-root` 经核对两条命令均已携带，未复现） | LS-15 → LS-15, LS-36, LS-37 | 文头、使用规则、缺口表、评审结论、ADR-LS-02、验证执行协议、文档与网站验收命令、实施顺序、发布窗口、Phase 1、粒度审计表、LS-15/31/36/37 与全部卡断言映射说明、追溯表、里程碑、风险表、完成判据、Review log |
| 2026-11-18 11:03:46 UTC | Claude R21 FAIL + Codex R21 FAIL（2026-11-18 双审第 5 轮，受审 SHA-256 `39166b226ced`） | ① `.libra/` 防护并回 `LS-15`（首个暴露 `--output` 的版本即生效）：已有文件、新路径、符号链接三种目标分条拒绝，另以拒绝后 `.libra/`「路径 → SHA-256」清单不变作单一终态谓词；`LS-37` 改为目录/不可写父目录的 `LBR-IO-002` 稳定诊断，风险表同步；② 网站第 2 步要求 `fetch` 后本地 `cf` 与 `refs/remotes/origin/cf` 同一提交（只允许 `pull --ff-only` 快进）且无已暂存条目；第 6 步要求暂存集恰为本卡页面、推送前远端仍等于提交父提交，确保只推送本卡提交；③ 去掉空输出也能通过的断言：`LS-27` AC2 改为与 Git 期望逐字相等，`LS-09`（夹具改为含 M 的 4 提交）/`LS-11`/`LS-17`/`LS-19`/`LS-20`/`LS-29` 的「不含」型期望补上必含内容或行数 | N/A | 文头、Phase 1、文档与网站验收命令、验证执行协议、LS-09/11/15/17/19/20/27/29/36/37、追溯表、风险表、Review log |
| 2026-11-18 11:07:47 UTC | Claude R22 FAIL + Codex R22 FAIL（2026-11-18 双审第 6 轮，受审 SHA-256 `522d3e84c2e5`） | ① `LS-15` 改为同目录临时文件 + 原子 `rename` 写入，硬链接目标只替换目录项、不截断 `.libra/` 内部文件，夹具与清单终态断言纳入硬链接 H，故障恢复矩阵同步；② `LS-02` AC3、`LS-06` AC2 的 stdout 与 stderr 比较各拆为两条（`LS-02` 7/8、`LS-06` 6/8）；③ `LS-09` 的 `%p`、`--parents`、`--children` 期望逐行列出每个 OID 的数量与长度；④ `LS-11` 错误等价拆为退出码相同与 stderr 逐字相同两条（5/8）；⑤ 网站无字节变化分支不再允许「基线页面不背书」，改为按该页最近一次修改提交选取部署指纹并在生产页核验；`LS-09` AC3/AC4 改为「每个 OID 字段均完整」的单一谓词，`LS-10` AC3 期望改为恰 3 行；⑥ 「验证执行协议」第 4 项界定只读不变量为被测命令对引用与 HEAD 的不变性，测试夹具在工作区外临时目录的受控写入（`LS-25` 标记文件）不在此列 | N/A | 文头、验证执行协议、文档与网站验收命令、LS-02/06/09/11/15/25、粒度审计表、故障恢复矩阵、完成判据、Review log |
| 2026-11-18 11:11:05 UTC | Claude R23 FAIL + Codex R23 FAIL（2026-11-18 双审第 7 轮，受审 SHA-256 `ac847b8a6cbd`） | ① `LS-34` AC2 去掉 `libra show` 不存在的 `-p`（`show` 默认输出 patch）；② 「验证执行协议」第 4 项的只读不变量改为完整引用快照（`reference` 表全部行 + `.libra/refs/` 树哈希清单），并明确允许的写入：`--output` 目标、`run_textconv` 的系统临时文件、`LS-25` 过滤器在工作区外临时目录的标记文件；`LS-28` AC1 明确 `index` 行不在路径行断言内；③ 部署指纹统一为：正向指纹取最新版本或其前最近一次含新增行提交 K 的新增文本并以 `libra log -S` 证明由 K 首次引入且未被删除，K 之后的删除/重排提交另加负向/顺序指纹，无变化分支同规则核验 | N/A | 文头、验证执行协议、文档与网站验收命令、LS-34、完成判据、Review log |
| 2026-11-18 11:16:10 UTC | Claude R24 FAIL + Codex R24 FAIL（2026-11-18 双审第 8 轮，受审 SHA-256 `7dbacd7d6ce4`） | ① 部署核验改为整页正文比对：本地 `pnpm preview:cf` 构建当前 `cf` 字节与生产页各取一次响应，比较 `<article>` 规范化正文哈希，对新增/删除/重排/无变化均可执行，取代需 `libra log -S` 唯一性的指纹；无字节变化分支同法核验；② 文档关键字只用于 EN/zh 命令页（须命中且改变），开发文档与 `COMPATIBILITY.md` 行改为逐文件语义核对，可记录「无需修改」理由；③ 「验证执行协议」第 4 项的断言与引用快照只约束新增卡特有用例，共享回归组按原样运行；④ 期望中「X 且 Y」型复合判定改为单一谓词：`LS-11` 改为逐字期望并把两个 help 别名分条（6/8），`LS-12` AC1、`LS-17` AC4 改为与 Git 期望逐字相等，`LS-19` AC2、`LS-34` AC2 改为文件集合相等，`LS-20` AC4–AC6 改为与仅 `-s`/`--no-patch` 的输出逐字相等 | N/A | 文头、验证执行协议、文档与网站验收命令、LS-11/12/17/19/20/34、粒度审计表、完成判据、Review log |
| 2026-11-18 11:19:11 UTC | Claude R25 FAIL + Codex R25 FAIL（2026-11-18 双审第 9 轮，受审 SHA-256 `ef8c58e6889c`） | ① 部署核验的正文提取改为命令替换后单独检查退出码与非空，再计算哈希，提取失败不再因管道被掩盖；② 基准构建前增加干净性门：除后端根 `docs/`（不参与站点构建）外工作区无任何改动或未跟踪文件，且本地 `cf` 等于 `refs/remotes/origin/cf`，保证 `pnpm preview:cf` 构建的即已验证提交字节；③ `LS-28` 的 `--no-prefix` 三条路径行分为三条 AC（5/8）；④ `LS-18` 按 pinned Git 文件对语义（旧侧或新侧 OID 命中，树条目参与）重写描述、证据与 AC，夹具加入对象总数不变的路径移动提交 | N/A | 文头、文档与网站验收命令、LS-18/28、粒度审计表、追溯表、Review log |
| 2026-11-18 11:23:10 UTC | Claude R26 FAIL + Codex R26 FAIL（2026-11-18 双审第 10 轮，受审 SHA-256 `dfc6ba4d7efd`） | ① 第 1 步 `COMPATIBILITY.md` 行定位改为锚定状态列的模式并要求恰好命中一行（原模式在另一张表的同名行上重复命中；Codex 所称双反斜杠经 `cat -A` 核对不成立）；② `LS-26`/`LS-33` 夹具改为 `foo_bar baz qux` → `foo_BAR baz QUX`（使 `[A-Za-z]+` 正则可区分），AC1–AC6 改为对 `@@` 之后正文的逐字期望（取自 `libra 0.30.207` 预探，plain/porcelain/regex 另与 Git 2.54.0 一致），不再允许两边空输出相等；③ `LS-15` 撤回临时文件 + `rename`，改为与 Git `xfopen(path, "w")` 相同的原地截断写入（保留 inode/权限），硬链接防护改为目标硬链接数大于 1 时比较 `(dev, inode)` 并拒绝，AC 改为 I/N/L/H 四种拒绝 + 清单终态；覆盖语义（内容、inode、权限）与失败诊断移入 `LS-37`（5/8）；故障恢复矩阵与风险表同步 | N/A | 文头、文档与网站验收命令、LS-15/26/33/37、粒度审计表、故障恢复矩阵、风险表、Review log |
| 2026-11-18 11:27:10 UTC | Claude R27 FAIL + Codex R27 FAIL（2026-11-18 双审第 11 轮，受审 SHA-256 `59c28a03e5a3`） | ① 「验证执行协议」第 4 项的允许写入扩展到 `LS-37` 的 `--output` 目标与 `less` 测试桩标记文件；② `LS-15` 防护改为路径预检 + 不截断打开（已存在只写不创建、新建独占创建）+ 描述符级 `(dev, inode)` 核验后经同一描述符截断，不在检查后按路径重开；明确威胁模型，竞态回归登记 `DEFER-LS-02`；评审结论安全性行同步；③ 行为修复卡 `LS-01/04/09/13/14` 改为必须同卡更新 EN/zh 与网站页（设文档关键字），第 1 步删去「行为修复卡无需修改」通路；④ 「不启动分页器」从 `LS-15` 移到 `LS-37` 并以 `less` 测试桩标记文件验收（`LS-37` 6/8），风险表同步；⑤ 事实基线注明 2026-11-18 对 `0.30.210` 检出的复核；⑥ 「验证执行协议」新增第 7 项：新增用例以子进程 + 显式 `current_dir`/逐子进程环境实现、不用 `#[serial]`，不触及共享的 `tests/SERIAL_REGISTRY.tsv`；兼容收口的漂移检查命令补全 `cd` | N/A | 文头、事实基线、评审结论、验证执行协议、文档与网站验收命令、LS-01/04/09/13/14/15/37、粒度审计表、风险表、非目标与延后项、Review log |
| 2026-11-18 11:31:01 UTC | Claude R28 PASS + Codex R28 FAIL（2026-11-18 双审第 12 轮，受审 SHA-256 `dc21ba9f1954`） | Codex R28 的两项 P1（兼容矩阵定位命令含双反斜杠、正文提取命令含反斜杠转义的双引号）经 `od -c`/`grep -o` 核对受审字节不成立：文件为单反斜杠 `\|`、`encoding="utf-8"`、`"$resp"`，且两条命令实测可用；为消除转义视图造成的歧义，两条命令移入第 8 步之后的命令块：W-1 改用不含反斜杠的 `awk` 定位（实测 log/show/diff 分别只输出 196/199/227），W-2 改为 heredoc 形式的 Python 提取（实测线上页 rc=0、34833 字符，缺 `<article>` 的页面 rc=2） | N/A | 文头、文档与网站验收命令、Review log |
| 2026-11-18 11:34:27 UTC | Claude R29 PASS + Codex R29 FAIL（2026-11-18 双审第 13 轮，受审 SHA-256 `951238f55a51`） | ① 部署核验不再用 `mktemp` 落盘响应：命令块 W-2 改为在内存中取得同一次响应（通用 User-Agent）、校验 HTTP 200 与非空 `<article>`、输出正文 SHA-256 与字符数（线上页实测 rc=0），计划只记录哈希、字符数与时间；② 基准构建干净性门改用 `libra status --short --ignored --untracked-files=all`，只允许后端根 `docs/` 与列明的构建产物/依赖/缓存忽略条目（含说明不影响文档正文的 `.dev.vars`），并禁止根目录 `.env*`（vite `envDir`） | N/A | 文头、文档与网站验收命令、Review log |
| 2026-11-18 11:37:51 UTC | Claude R30 FAIL + Codex R30 FAIL（2026-11-18 双审第 14 轮，受审 SHA-256 `04db9392d94c`） | ① `LS-03` 区分 Git 无值键与空串：Libra 配置值 `NOT NULL`（`sql/sqlite_20260309_init.sql:15`）且 `libra config --add` 必须给值（实测），无值键缺值诊断在 Libra 不适用并登记有意差异；空串值按 Git 空排除模式验收（Git 2.54.0 预探 `(HEAD)`），AC 改为 4 条，缺口表同步；② 删除网站「无字节变化」通过分支：各卡第 1 步必改 EN 命令页，同步后网站页必须改变，否则 STOP，每卡完成第 4–7 步，第 7 步与完成判据同口径；③ 兼容收口的 `COMPATIBILITY.md` 条目与第 1 步「无需修改」口径对齐；④ 修订历史 R17/R18 行内代码中的 `\|` 补转义，避免 GFM 表格断列 | N/A | 文头、缺口表、文档与网站验收命令、LS-03、粒度审计表、完成判据、兼容与文档收口、Review log |
| 2026-11-18 11:41:42 UTC | Claude R31 PASS + Codex R31 FAIL（2026-11-18 双审第 15 轮，受审 SHA-256 `4d05f3bd2d10`；本次运行轮次上限） | 按 Codex R31 两项 P1 修订（**未经评审**，需新一轮双审）：① 「验证执行协议」第 4 项允许后续卡在行为被取代时只删除前序用例中被取代的断言（仅 `LS-36` 对 `LS-15` 的 `output_to_file`），并记录理由，`LS-36` 回归门描述同步；② 第 6 项明确每条 Verification 含 `nextest list` 预检与运行判定两个独立门，全部卡 `VER` 分子改按 2 × 条目数重算（最大 `LS-25` 8/8），粒度审计表同步 | N/A | 文头、验证执行协议、LS-36、全部卡 Granularity、粒度审计表、Review log |
| 2026-11-18 11:55:13 UTC | Claude R32 FAIL + Codex R32 FAIL（2026-11-18 双审第 16 轮、续接运行第 1 轮；Codex 按行区间分 A/B/C/D 四区并行评审，A/B/C FAIL、D PASS；受审 SHA-256 `c916505a9a8d`） | ① GC-05 与各卡 `Docs and compatibility impact` 改为与第 1 步同一口径：EN/zh 命令页与网站页必须同卡更新，开发文档与 `COMPATIBILITY.md` 行逐文件语义核对（受影响即更新，否则记录理由与未变 SHA-256；`LS-03`/`LS-15`/`LS-16` 须登记有意差异，该行必须更新），兼容收口清单同步；② 部署核验 W-2 改为比较 `<article>` 内部 HTML 原始字节的 SHA-256 与字节数（链接目标、属性与 `pre`/`code` 空白均参与；线上 `log` 页实测 rc=0），第 7 步与完成判据同步；③ `DEP-LS-03` 未取得确认时在 C 组第 ⑦ 步前置 `blocked`（不进入 `remote-pending`），第 8 步同步；④ 故障恢复矩阵改为「路径预检在打开前、描述符与硬链接核验在打开后截断前」；⑤ `LS-14` 标题改为「仍输出默认提交头」，证据锚点改为 `log.rs:2334-2344,3096-3099` 并登记既有单元测试 `test_parse_pretty_format_presets`（缺口表、追溯表同步）；⑥ `LS-36` 例外引用改为第 4 项，`Current evidence` 记录被删断言与理由；⑦ `LS-04` 把 HEAD 的 include/exclude 拆为两条 AC（4/8，补 Git 2.54.0 预探值）；⑧ `LS-37`、`LS-25` 各补一条正向对照 AC（分页器桩、textconv 过滤器被调用；均 7/8）；⑨ `LS-16` 写集补 `src/command/log/config.rs`（`config.rs:38` 读 `args.follow.is_some()`；landing/prod-files 2/2）；⑩ 事实基线 `src/cli.rs` 过时锚点改为 `396,493,3531,3895,3948`，删去无关的 `diff.rs:2574`；`LS-37` 证据锚点改为 `log.rs:2190-2192,2325`；`LS-16` 触发字段去掉嵌套反引号 | N/A | 文头、事实基线、缺口表、GC-05、文档与网站验收命令、W-2、依赖登记表、全部卡 `Docs and compatibility impact`、LS-04/14/16/25/36/37、粒度审计表、追溯表、故障恢复矩阵、兼容与文档收口、完成判据、Review log |
| 2026-11-18 11:56:40 UTC | 用户 2026-11-18 11:55:09 UTC 确认网站检出路径（经本批评审协调者转达） | `DEP-LS-03` 由「执行前须取得的书面确认」改为已满足：用户 2026-11-18 11:55:09 UTC 确认本批计划的网站文档交付检出为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支）；「文档职责」、事实基线文档行、第 2 步前置、实施顺序、依赖登记表与完成判据同步；`AGENTS.md:135` 的 `/Volumes/Sea/libra-backend` 登记为已知文档滞后（用户决定本批不改 `AGENTS.md`）；08:19:08 行所称「用户最新指令」不再作为依据（该历史行保留原文） | N/A | 文头、文档职责、事实基线、文档与网站验收命令第 2 步、实施顺序、依赖登记表、完成判据 |
| 2026-11-18 12:03:02 UTC | Claude R33 PASS + Codex R33 FAIL（2026-11-18 双审第 17 轮；Codex 分区 A/B/C FAIL、D PASS；受审 SHA-256 `6bc1a31b1cb6`） | ① 评审结论「完整性」行改为每卡显式 `T-1`、仅 `LS-16` 另记 `T-3` 口径；② 「验证执行协议」第 6 项补「映射计数口径」：成功类 `exit 0` 为读取前提，失败类「exit N + `LBR-*`」为「以该稳定码失败」单一谓词（退出码默认由稳定码派生，`error.rs:514-527,1068-1076`），映射不得加入 AC 外判据，形态约束写成单一等值比较；③ `LS-01` AC4 映射删去额外的 stderr 判据；`LS-09` AC1–AC6 改为对 `%H`/展开文本 E(…)/`--abbrev=7` 输出的单一逐字等值；`LS-10` 夹具固定提交时间、AC3/AC4 改为行多重集等值、AC6 写明 `LBR-CLI-002`；`LS-16` 把稳定码与 Git 诊断文本拆为独立 AC（7/8）；`LS-25` AC6/AC7 映射删去额外诊断文本；`LS-29` 改为 `index` 行列表逐字等值；④ `LS-15` 打开步骤改为以逐级跟随符号链接后的最终目标为准（悬空符号链接与 Git `fopen` 一样创建目标），消除「跟随符号链接」与「不跟随最终符号链接」的矛盾；`LS-37` 补悬空符号链接创建 AC8（8/8，Git 2.54.0 预探），协议第 4 项允许写入与风险表同步；⑤ `LS-22`/`LS-24` 夹具补只在 `ignore-all-space` 下识别为移动的空白变体块 Q | N/A | 文头、评审结论、验证执行协议、LS-01/09/10/15/16/22/24/25/29/37、粒度审计表、风险登记、Review log |
| 2026-11-18 12:06:47 UTC | Claude R34 PASS + Codex R34 FAIL（2026-11-18 双审第 18 轮；Codex 分区 A/B/C FAIL、D PASS；受审 SHA-256 `811090a6393c`） | ① 网站第 2 步改为按序 (a) 单一 VCS 元数据 → (b) 当前分支非 `cf` 时先 `libra switch cf` → (c) 再要求 `cf` 与 `## cf...origin/cf` → (d) 页面与同步检查，消除「先要求 `cf` 后才允许切换」的矛盾，「文档职责」同步；② 第 6 步补远端前进/推送被拒时的恢复：`reset --mixed <提交>^` 撤销本卡未推送提交、只丢弃本卡页面字节、`pull --ff-only` 后从第 2 步重来，页面字节变化须重审；③ `LS-05` 描述按 Git 文档改为「被装饰提交必选、Git 可额外显示未装饰提交以保持有意义的历史」，pathspec 组合登记为范围外；④ `LS-10` AC4 文本改为恰输出 `v1`/`v2` 所指提交各一次，与映射一致；`LS-11` AC3 映射改为两次退出码相等；⑤ 「验证执行协议」第 7 项补平台范围：`LS-13`/`LS-15`/`LS-25`/`LS-37` 的 Unix 语义用例以 `#[cfg(unix)]` 限定，非 Unix 不启动分页器（`pager.rs:108-111`）；`LS-15` 非 Unix 平台以 `beneath.rs` 同口径文件标识核验、取不到即 fail-closed，风险登记补一行；⑥ P2：`LS-17` AC5 映射改为单一输出段等值，`LS-23` 夹具补锚点敏感条件 | N/A | 文头、文档职责、文档与网站验收命令第 2/6 步、验证执行协议第 7 项、LS-05/10/11/15/17/23、风险登记、Review log |
| 2026-11-18 12:13:11 UTC | Claude R35 PASS + Codex R35 FAIL（2026-11-18 双审第 19 轮；Codex 分区 A/B/C/D 均 FAIL；受审 SHA-256 `f960a35a9add`） | ① 「验证执行协议」第 2/3 项限定为卡特有用例，共享回归组只按第 5 项；② 第 6 项「映射口径」改为：AC 文本与映射表述同一谓词；失败类 AC 一律写成「以 `<LBR-*>` 拒绝/失败」且映射只断言稳定码，不再断言数值退出码（`exit_code_override`/`LIBRA_FINE_EXIT_CODES` 可使二者独立失败），全部 30 条失败映射删去 `exit 128/129`，对应 AC 文本写明稳定码；③ `LS-06` 夹具不再假定 `libra notes` 生成 `refs/notes/commits`（预探：notes 存 SQLite、`update-ref` 只支持 `refs/heads/*`、`clone` 不导入 notes 引用），改为在 `reference` 表直接写入 `refs/notes/commits` 行（与 `collect_notes_reference_tips` 读取形态一致，测试有直接写表先例），全部调用以 C 为目标；④ AC 文本与映射逐条对齐：`LS-01` AC1、`LS-02` AC1/2/5/6/7、`LS-03` AC1–4、`LS-04` AC1、`LS-08` AC1、`LS-10` AC1/2/3/5、`LS-12` AC3、`LS-14` AC4/5、`LS-16` AC1、`LS-17` AC2/AC6、`LS-27` AC1、`LS-29` AC1/2、`LS-34` AC3；`LS-17` AC5 改为独立的数字范围调用（不再是 AC4 输出的子段）；`LS-30` AC3 的 `libra diff` 一致性改为背景说明 | N/A | 文头、验证执行协议、LS-01/02/03/04/06/08/10/12/14/15/16/17/18/22/24/25/26/27/29/30/31/32/33/34/35/37、Review log |
| 2026-11-18 12:16:31 UTC | Claude R36 FAIL + Codex R36 FAIL（2026-11-18 双审第 20 轮；Codex 分区 A/B/C/D 均 FAIL；受审 SHA-256 `5dd4aeac43ff`） | ① 引用快照纳入 `notes` 表全部行（Libra 自身 notes 存于该表，`sql/migrations/2026061401_notes.sql:1-7`）；② `LS-36` 被删断言记录去掉已撤销的 `exit 129`；③ `LS-07` 改为对 `libra log -1 --decorate=short --color=always` 首行（含 SGR）与 Git 同夹具期望逐字等值（Git 把 `tag: ` 前缀与 tag 名分段着色，`log-tree.c`）；④ `LS-21` AC1/AC2 映射写明对长短两种拼写的表驱动调用；⑤ AC 文本与映射的字节级对齐：`LS-34` AC1、`LS-01` AC2/AC3、`LS-11` AC1/AC2 的行尾 `\n`，`LS-26`/`LS-33` 的正文期望统一为「`@@` 行之后的正文」并含行尾 `\n`，`LS-01` AC4、`LS-13` AC3 文本写出映射所用的字面量；⑥ `LS-06`（Claude 发现）：夹具中 `kind = 'Branch'` 的 `refs/notes/commits` 行当前会被当作本地分支装饰，使「R 出现」类 AC 空过；补 AC7（默认装饰恰为 `(HEAD -> main)`，7/8）并在描述与证据写明须按完整引用名识别 notes 命名空间、Git 默认过滤不含 `refs/notes/` | N/A | 文头、验证执行协议第 4 项、LS-01/06/07/11/13/21/26/33/34/36、粒度审计表、Review log |
| 2026-11-18 12:19:56 UTC | Claude R37 PASS + Codex R37 FAIL（2026-11-18 双审第 21 轮；Codex 分区 A PASS、B/C/D FAIL；受审 SHA-256 `e929be6b198d`） | ① AC 文本与映射的比较范围对齐：`LS-06` AC5、`LS-05` AC1–AC3（补明确命令与 `refs/tags/v2` 模式）、`LS-19` AC1/AC3、`LS-20` AC1–AC3、`LS-21` AC1/AC2、`LS-27` AC3、`LS-30` AC1/AC2 改为「stdout 逐字等于／与…逐字相同」；② `LS-35` 按 Git 有序解析改写：`no` 清除其前模式，解析结束后同时含 `allow-indentation-change` 与 `ignore-*` 才报错（`diff.c:333-352`，Git 2.54.0 预探），补 AC3（`ignore-all-space,no,allow-indentation-change` 合法）与 AC4（`allow-indentation-change,no` 清除），4/8；③ P2：`LS-21` AC3、`LS-13` AC2 的「修复前基线」明确为本卡开工前的 Libra 源码版本在同一夹具上的输出 | N/A | 文头、LS-05/06/13/19/20/21/27/30/35、粒度审计表、Review log |
| 2026-11-18 12:27:37 UTC | Claude R38 PASS + Codex R38 FAIL（2026-11-18 双审第 22 轮；Codex 分区 B PASS、A/C/D FAIL；受审 SHA-256 `3ae0ad4bff14`） | ① `LS-15`/`LS-37` 按 G-02 重切：`LS-15` 只开放新目标（悬空符号链接按 Git `fopen` 创建目标）、`.libra/` 新路径防护、新目标创建失败诊断与 `--json` 组合拒绝，既有目标以 `LBR-IO-002` 拒绝（8/8）；`LS-37` 开放既有目标的 Git 原地截断覆盖（内容/inode/权限）并同卡验收既有目标的 `.libra/` 防护（I/L/H 与清单）与目录目标诊断（8/8，测试改名 `output_overwrite_and_protection`），按第 4 项例外删除 `LS-15` AC5 的拒绝断言；删去「带 `--output` 不启动分页器」：Git `cmd_log_init_finish` 无条件 `setup_pager`（`builtin/log.c:388`），Libra 维持现有分页器策略；协议第 4/7 项、风险登记、`DEFER-LS-02`、追溯表、`M-SEC` 与 `LS-36` 引用同步；② 性能与容量摘要改为如实描述：`-n` 只对 `eligible_date_priority_yield` 判定的普通遍历提前停止，过滤遍历走完可达历史，上界为每提交至多访问一次；③ `LS-33` 重启条件写出原文，不再引用 `LS-26`；④ P2：`LS-02` 补 `--format=raw` 抑制配置装饰的 AC8（8/8，Git 2.54.0 预探） | N/A | 文头、验证执行协议第 4/7 项、LS-02/15/33/36/37、粒度审计表、测试矩阵、追溯表、风险登记、性能与容量摘要、非目标与延后项、Review log |
| 2026-11-18 12:31:58 UTC | Claude R39 PASS + Codex R39 FAIL（2026-11-18 双审第 23 轮；Codex 分区 B PASS、A/C/D FAIL；受审 SHA-256 `d5a166ccca44`） | ① 故障恢复矩阵按发布窗口区分：`LS-15`～`LS-37` 之间部分写入文件须删除或改用新目标后重跑，`LS-37` 起重跑原地覆盖；描述符级核验限定于 `LS-37` 起；② 性能摘要计入排除端（`^A`/`A..B`）祖先闭包：上界为正向与排除端各自可达集合之并（`expand_excluded_commits`、`walk_date_priority` 锚点）；③ `LS-15` 不再称 Git「无条件启动分页器」：`cmd_log_init_finish` 总调用 `setup_pager`，是否启动取决于分页器配置与终端（`pager.c`）；④ `LS-25` 与事实基线错误码行注明 128/129 为默认退出码，`LIBRA_FINE_EXIT_CODES` 与显式覆盖另按错误协议；⑤ P2：`LS-10` AC1 输入改为 `C1 C3` 以区分排序与输入顺序；`LS-11` 补「两种拼写各以 `LBR-CLI-002` 拒绝」AC3（7/8），协议第 6 项示例改指 `LS-11` AC4 | N/A | 文头、事实基线、验证执行协议第 6 项、LS-10/11/15/25、粒度审计表、故障恢复矩阵、性能与容量摘要、Review log |
| 2026-11-18 12:34:39 UTC | Claude R40 PASS + Codex R40 FAIL（2026-11-18 双审第 24 轮；Codex 分区 B/C/D PASS、A FAIL；受审 SHA-256 `a35b9d89fd28`） | ① `LS-11` AC4 改为真正的数值退出码断言：默认错误码配置下（子进程不设 `LIBRA_FINE_EXIT_CODES`）两种拼写的退出码均为 129，协议第 6 项示例同步写明「限定错误码配置」；② P2：`LS-01` AC4 改为按 `, ` 切分后逐项完整匹配 `tag: d1`…`tag: d200`，避免 `tag: d1` 以子串命中 `tag: d10` | N/A | 文头、验证执行协议第 6 项、LS-01/11、Review log |
| 2026-11-18 12:37:13 UTC | Claude R41 PASS + Codex R41 PASS（2026-11-18 双审第 25 轮；Codex 分区 A/B/C/D 均 PASS；受审 SHA-256 `859bb860dec875ebc7889a82e042146bfc8d749df10782e60f8055f90caafdd6`） | 记账：Claude 与 Codex 对同一受审字节均给出 `VERDICT: PASS`，计划文本提交门满足；本次只更新文头状态行并新增本行与 Review log 的 R41 两行，正文未改 | N/A | 文头、修订历史、Review log |

## 已决议设计决策

### ADR-LS-01: `log` / `show` 复用 `src/command/diff/` 的 `pub(crate)` 适配层（不改 `CommitDiffOptions`）

- **Status:** Accepted
- **Context:** #857、#858、#862、#964、#966、#967、#970、#973 要求 `log`/`show` 支持 `diff` 能力；当前 `src/command/diff.rs:5` 将 `mod render;` 声明为私有模块，其内 `render_word_diff`（`diff/render.rs:61`）、`format_diff_raw`（`diff/render.rs:503`）、`colorize_diff`（`diff/render.rs:782`）为 `pub(super)`（而 `format_diff_dirstat` 在 `diff/render.rs:412` 已为 `pub(crate)`），`resolve_diff_config`（`diff/options.rs:28`）位于 `src/command/diff/` 子模块，`-U<n>`/`-w`/`-b`/`--full-index` 位于 `src/command/diff/compare.rs:405-417,591`，而 `parse_diff_filter`（2228）、`apply_diff_filter`（2265）、`run_textconv`（3403）位于 `src/command/diff.rs`；同时 `CommitDiffOptions`（`log.rs:4254`）被 `format_patch.rs:1852` 逐字段构造。
- **Decision:** 在 `src/command/diff/`（`diff.rs`、`diff/render.rs`、`diff/options.rs`、`diff/compare.rs`）将 `mod render`（由 `issues/529.md` `FP-05` 提升或开工前核实为 `pub(crate)`）及上述逻辑抽取为 `pub(crate)` 共享入口，并在 `src/command/log.rs` 引入独立的 `pub(crate) struct LogDiffOptions` 与 log/show 共用的 diff 生成入口调用它们。`LogDiffOptions` 由第一张需要 log 专用 diff 控制的卡 `LS-12`（`-R`，初始字段 `reverse`）引入，后续卡只向其追加字段；既有 `generate_diff` / `generate_diff_with_options` 的签名与 `CommitDiffOptions` 的字段保持不变（`format_patch.rs:950,1852-1853` 以结构体字面量构造，不触及 `src/command/format_patch.rs`）。
- **Alternatives considered:** 直接向 `CommitDiffOptions` 追加字段（会破坏 `format_patch.rs` 编译并引发跨计划写锁冲突，拒绝）。
- **Consequences:** 触及 `src/command/diff/` 任一文件的卡（`LS-20..LS-31` 与 `LS-35`，共 13 张）均须依赖 `DEP-LS-02` 并将其实际改动文件列入 `Implementation write set`；`LS-33`、`LS-34` 只复用前序卡已提升的 `pub(crate)` 入口，不写 `src/command/diff/`。
- **Revisit when:** `plan-20260927` CM-08/CM-09 与 `format-patch` 均合入后可统一整合。

### ADR-LS-02: `log.rs` / `show.rs` / `diff/` 共享写集串行化与逐卡锁核验

- **Status:** Accepted
- **Context:** 除 `LS-31`、`LS-35` 外的 35 张卡都写 `src/command/log.rs`，部分卡同时写 `src/command/show.rs` 与 `src/command/diff/`；单靠 `LS-01` 前一次核验无法覆盖 37 张卡逐卡发布窗口期内的外部并发写者。
- **Decision:** 采用线性依赖链，且**每张卡在开工前**都必须按其完整 `Implementation write set`（含 `src/command/log.rs`、`src/command/show.rs`、`src/command/diff/`、对应 `tests/command/*`、`docs/commands/*`、`COMPATIBILITY.md:196,199,227` 与网站页）重新核验 `DEP-LS-01`（及适用时的 `DEP-LS-02`）：确认 `plan-status.md` 中对上述全部文件均无其它 `in-progress`/`blocked` 写者；若写锁归属不清或存在并发写者则 fail-closed 禁止开工，待释放并 rebase 后再取得写锁执行（G-10）。
- **Alternatives considered:** 仅在 `LS-01` 前一次性检查（无法覆盖后续卡窗口，拒绝）。
- **Consequences:** 串行交付，全执行窗口零写集冲突。
- **Revisit when:** 后续拆分 `log/` 子模块后可放宽。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01`..`GC-13`。特别强调：
- **GC-05 / ER-06a 文档同步:** 每张卡按其 `Docs and compatibility impact` 所列命令（`log`、`show` 或 `diff`，各卡逐文件列明）同卡更新 `docs/commands/<cmd>.md`、`docs/commands/zh-CN/<cmd>.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`，并对 `docs/development/commands/<cmd>.md` 与 `COMPATIBILITY.md` 对应行（`log` 196 / `show` 199 / `diff` 227）逐文件做语义核对：受本卡行为影响即同卡更新（兼容矩阵原地修改、不增删行；卡内要求登记有意差异的 `LS-03`、`LS-15`、`LS-16` 必须更新 `| log |` 行），不受影响则按第 1 步记录理由与未变 SHA-256；均按「文档与网站验收命令」第 1–7 步在「文档职责」指定的授权网站检出（`cf` 分支）完成同步、前端校验、Claude Code `PASS`、提交推送与部署核验；该检出不可用时 fail-closed 停止网站交付，除非用户明确授权替代检出。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01`..`ER-14`：
- **ER-04 / ER-08 / ER-13 / ER-14:** 每张 `implementation` 卡均为 `independent` 发布切片，完成后按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 权威三处（`Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`；因每卡均修改 `install.sh`/`install.ps1`（`T-1`），C 组第 ④ 步除运行本卡 A 组 focused `nextest` 外，必须跑双环境全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`、`fmt`、`clippy`、`cargo build --release`，随后签名提交推送 PR，squash merge 后 `gh release create v<version>`。
- **ER-04 D 组远端后置门（每张 `independent` 卡默认适用）:**
  - `base.yml`（PR / `push` `main`）：`format`、`clippy`、`redundancy`、`test` 全部 green。
  - `codeql.yml`（PR / `push` `main`）：CodeQL 扫描 green。
  - `release.yml`（`push` `v*` tag，由 `gh release create "v${VERSION}"` 触发）：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 green；并核验 `download.libra.tools` / stable manifest（`HEAD` 200、range `GET` 206）。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：**
  1. **卡特有用例**（标 `(new)`）：每条 Verification 只含一个测试，过滤式为 `-E 'test(=command::<模块>::<函数>)'` 的完整路径精确匹配；不得用前缀、正则或 `|` 并集代替。
  2. 卡特有用例运行前先以同一前缀与过滤执行 `cargo nextest list`（例：`source .env.test && cargo nextest list --test command_test -E 'test(=command::log_test::decorate_nested_annotated_tags)'`），列出的测试必须恰为 1 个且名字与预期逐字一致，否则 STOP。
  3. 卡特有用例运行后记录 exit code 与「匹配 / 通过 / 失败 / 跳过」四计数，要求 exit code = 0、匹配 = 通过 = 1、失败 = 0、跳过 = 0，任一不满足即 STOP。共享回归组不适用第 2–3 项的「恰为 1 个」与「匹配 = 通过 = 1」，其预检与判定只按第 5 项执行。
  4. 测试实现必须对卡内「AC → 断言映射」逐条断言（exit code 与 stdout/stderr 的精确内容或必含/必不含片段），执行记录逐条引用 AC 编号。只读不变量：被测命令不得改变任何引用——所有 log/show/diff 用例在调用前后比较「引用快照」并要求逐字相等；引用快照 = `.libra/libra.db` 中 `reference` 表全部行（按主键排序，表定义见 `sql/sqlite_20260309_init.sql:19`）+ `notes` 表全部行（按主键 `id` 排序，比较 `notes_ref`、`object`、`blob` 三列；Libra 自身 notes 存于该表，定义见 `sql/migrations/2026061401_notes.sql:1-7`）+ `.libra/refs/` 目录树的「相对路径 → SHA-256」清单，覆盖 HEAD、分支、标签、远端跟踪、notes（含 `notes` 表）、stash 与 replace 等全部命名空间。被测命令允许的写入仅限：`LS-15`、`LS-36`、`LS-37` 用例在测试临时目录中指定的 `--output` 目标文件（含 `LS-15` AC3 经符号链接 S 创建的目标文件 T；`LS-15` 与 `LS-37` 的 `.libra/` 拒绝用例另以清单断言 `.libra/` 不变）；textconv 执行路径（`src/command/diff.rs:3403` `run_textconv`）按现有实现创建并清理的系统临时文件；以及用户配置的外部过滤器自身的副作用——在 `LS-25` 中仅限过滤器脚本向仓库工作区之外的测试临时目录写入、每次断言前删除的调用标记文件。上述写入均不得触及 `.libra/` 与工作区内被跟踪文件。本项的逐条断言与引用快照只约束本计划新增的卡特有用例（标 `(new)`）；共享回归组是既有用例，按原样运行，只作回归证据（第 5 项）；唯一例外是后续卡取代了前序卡用例所断言的行为时（本计划仅 `LS-37` 取代 `LS-15` 的既有目标拒绝、`LS-36` 取代 `LS-15` 的 `--json --output` 拒绝），该卡在同卡内只删除被取代的断言、保留其余断言，在本卡 `Current evidence` 记录被删断言与理由，再以修订后的用例作为共享回归门。映射中标注「Git 2.54.0 预探」的值是评审时本机 Git 2.54.0 在同构夹具上的观测；标注「Git 同夹具期望」的值在开工时取得。两类期望都必须在开工时用 `git@6de20f6092` 构建的 Git 在同一夹具重新取得并硬编码进测试，与预探不一致时以 pinned 版本为准并回写本卡 `Current evidence`（超出本卡行为轴时按 ER-03 处理）。该取得规则只适用于显式标注 Git 期望的断言；Libra 专有行为（`--json` 输出、`.libra/` 防护、`LBR-*` 稳定码与退出码、与 `libra diff` 的一致性比较）以卡内写明的 Libra 基准为准，不向 Git 取值。
  5. **共享回归组**（标「共享回归组」）：可为整个 compat target、`test(=…)` 精确并集或 `test(/^…/)` 正则族；运行前以同一过滤 `cargo nextest list` 记录匹配数 N（必须 ≥ 1；卡内给出预期 N 时必须相等），运行后要求 exit code = 0、通过 = N、失败 = 0、跳过 = 0。结果只作回归证据，不计为卡特有 AC 证据，但计入该卡 `VER` 分子。
  6. **VER 计数口径（G-03）**：每条 Verification 含两个独立门——第 2/5 项的 `cargo nextest list` 预检与第 3/5 项的运行判定，因此各卡 `VER` 分子按 2 × Verification 条目数计。**AC 计数口径（G-03）**：一条 AC 只承载一个可独立失败的可观测谓词；同一取值的等价拼写（如 Git 布尔真值集合）以表驱动用例验证时计为一个谓词；不同行为、不同选项、不同命令、不同错误条件一律分条计数。**映射口径**：AC 文本与其映射表述同一个谓词，映射只给出该谓词的精确判定值（逐字期望、等值比较对象），不得加入 AC 文本之外的其它行为判据（如另一条诊断文本、行数或字段数）；为排除空输出所需的形态约束一律写进 AC 并写成对完整期望文本（或另一条已定义命令输出）的单一等值比较。成功类映射中的 `exit 0` 是读取输出的前提。失败类 AC 一律写成「以 `<LBR-*>` 拒绝/失败」，其映射只断言 stderr 含该稳定码，不另断言数值退出码——数值退出码通常由稳定码派生（`src/utils/error.rs:514-527,1068-1076`、`docs/error-codes.md:98-120`），但 `exit_code_override` 与 `LIBRA_FINE_EXIT_CODES` 可使其偏离，二者可独立失败，因此需要断言数值退出码时须限定错误码配置并单列一条 AC（如 `LS-11` AC4：默认配置下退出码均为 129）。
  7. **新增用例的进程级隔离**：本计划新增的卡特有用例一律以子进程调用 `libra`，并通过显式 `current_dir` 与逐子进程环境变量传入工作目录与 `LIBRA_PAGER`、`GIT_REPLACE_REF_BASE`、`PATH` 等设置，不修改测试进程的 cwd/环境，因而不使用 `#[serial(...)]`，不触及跨计划共享的 `tests/SERIAL_REGISTRY.tsv` 与生成的 `.config/nextest.toml`；若实现中确需 `#[serial]`，须先按 ER-03 把这两个文件加入本卡 `Implementation write set`、补 `compat_serial_registry` 共享回归门并重新核验 `DEP-LS-01`，再开工。**平台范围**：依赖 Unix 语义的卡特有用例——`LS-13`（PTY）、`LS-15` 与 `LS-37`（符号链接、硬链接、inode 与权限位）、`LS-25`（可执行过滤器脚本）——整条以 `#[cfg(unix)]` 限定；本计划的测试门（本地三门与 `base.yml` 的 `test` job）在 Unix 上执行。`LS-37` 在非 Unix 平台的描述符级防护按其 Description 以平台文件标识实现，无自动化用例覆盖，登记于「风险登记」。

- **文档与网站验收命令（ER-06/ER-06a 同卡强制门；各卡「文档 AC」引用；按序执行，任一步失败即 STOP 且本卡不得进入 `complete`）：**
  1. **仓库文档逐文件验收：** 对本卡 `Docs and compatibility impact` 所列每个仓库文件记录改动前后 `shasum -a 256 <file>`；以 `test -f docs/commands/<cmd>.md && test -f docs/commands/zh-CN/<cmd>.md` 确认 EN/zh 双份存在；对卡内「文档关键字」只在 EN/zh 两个命令页执行 `rg -n -F -- '<关键字>' <file>`（退出码 0 = 命中；1 = 缺失 → STOP；>1 = 命令失败 → STOP），有文档关键字的卡这两页改动后 SHA-256 必须与改动前不同；`docs/development/commands/<cmd>.md` 与 `COMPATIBILITY.md` 对应行逐文件做语义核对，结论为「已更新」（记录改动前后 SHA-256 与所改段落）或「无需修改」（记录理由与未变 SHA-256，例如该行已按能力类别覆盖本卡行为），不得为通过机械门而添加不合该文件粒度的内容；`COMPATIBILITY.md` 先以下方命令块 W-1 刷新目标行号（必须恰好输出一个行号；文件中另有 `| log | common-user-flow |` 等其它表的同名行，只锚定命令名不唯一），有改动时断言改动前后 `wc -l < COMPATIBILITY.md` 相等（原地修改）。本计划每张卡都改变用户可见行为（含行为修复卡对输出的修正），因此每张卡都有文档关键字，EN/zh 命令页与网站页必须同卡实际更新（ER-06a），不得以「无需修改」通过。
  2. **网站检出预检：** 前置：`DEP-LS-03`（用户 2026-11-18 11:55:09 UTC 确认，见「文档职责」）仍有效、未被用户撤回或改变，否则 STOP。按序执行：(a) `test -d /Volumes/Data/gitmono/libra-backend/.libra && test ! -e /Volumes/Data/gitmono/libra-backend/.git`，不满足即 STOP；(b) `cd /Volumes/Data/gitmono/libra-backend && libra --offline symbolic-ref --short HEAD`，输出不是 `cf` 时按 ER-06a 执行 `libra switch cf`（被脏工作区等阻止即 STOP）后重跑 (b)；(c) 此后 `libra --offline symbolic-ref --short HEAD` 输出须恰为 `cf`，且 `libra status --short --branch` 首行须以 `## cf...origin/cf` 开头，否则 STOP；(d) 本卡目标页面 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 若已有未提交改动（`libra status --short -- <页面路径>` 输出非空）即 STOP；随后执行 `libra fetch origin`，要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`（本地 `cf` 不含未推送提交）——本地落后时只允许 `libra pull --ff-only origin cf` 快进后重验，领先或分叉即 STOP；`libra status --porcelain` 中不得有任何已暂存条目（首列既非空格也非 `?`），否则 STOP。已有的无关改动保留、不暂存。
  3. **同步与漂移检查：** 以包含本卡最终 EN 参考文档的 checkout 为 `--libra-root`（默认 `/Volumes/Data/gitmono/libra`；执行前以第 1 步记录的 SHA-256 确认其中 `docs/commands/<cmd>.md` 已是本卡合入 `main` 的字节），执行 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --write --only <cmd…>`（多个命令以空格分隔，如 `--only log show`），随后 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --only <cmd…>` 必须 exit 0（1 = 漂移、2 = 安全拒绝，均 STOP）；记录每个 `apps/tanstack-app/content/docs/commands/<cmd>.en.md` 的改动前后 SHA-256。本计划每张卡都已在第 1 步改变 EN 命令页，同步后网站页 SHA-256 必须与改动前不同；若未变（同步未生效或改动未进入 `--libra-root`），即 STOP 并排查，不设「网站无变化」通过分支。每张卡都必须完成第 4–7 步。
  4. **前端校验：** `cd /Volumes/Data/gitmono/libra-backend && pnpm typecheck` 与 `pnpm build` 分别记录退出码，均须为 0。
  5. **网站评审：** 对第 3 步记录的最终页面字节取得独立 Claude Code `VERDICT: PASS`；评审后页面字节变化须重审。
  6. **精确提交与推送：** 只 `libra add apps/tanstack-app/content/docs/commands/<cmd>.en.md`（本卡页面；多命令卡逐页添加）→ `libra status --porcelain` 的已暂存条目必须恰为本卡页面集合，否则 STOP → `libra commit -s -m "docs(commands): sync <cmd> for <卡 ID>"` → 按 ER-07 对同一提交校验 `gpgsig` 与 `Signed-off-by` → `libra show <提交>:<页面路径> | shasum -a 256` 等于第 5 步受审字节 → 推送前再次 `libra fetch origin`，要求 `libra rev-parse <提交>^` 等于 `libra rev-parse refs/remotes/origin/cf`（本次推送只新增本提交）→ `libra push origin cf`，推送后 `libra ls-remote origin refs/heads/cf` 指向该提交。远端已前进（上述比较不等，或推送因非快进被拒）时不推送、不 force、不改写他人提交，按以下恢复后从第 2 步重新开始：`libra reset --mixed <提交>^`（只撤销本卡这一个未推送的提交，工作区与他人改动保留）→ `libra restore --staged --worktree -- <页面路径>`（只丢弃本卡生成的页面字节，第 3 步会重新生成）→ `libra pull --ff-only origin cf`；重新执行后第 3 步生成的页面字节与第 5 步受审字节不同时，须重新取得第 5 步 `PASS`。
  7. **部署核验（`<article>` HTML 逐字节比对）：** 基准构建前先过干净性门：`cd /Volumes/Data/gitmono/libra-backend && libra status --short --ignored --untracked-files=all` 的输出中，除后端根目录 `docs/` 下的条目（不参与站点构建；站点内容根为 `apps/tanstack-app/content/docs`，见 `scripts/gen-docs-manifest.ts:83`）与下列构建产物/依赖/缓存的忽略条目外不得有任何条目：`node_modules/`、`apps/tanstack-app/node_modules/`、`apps/tanstack-app/dist/`、`apps/tanstack-app/.wrangler/`、`apps/tanstack-app/src/generated/docs-manifest.ts`、`apps/tanstack-app/src/generated/docs-search.ts`（二者由构建从内容目录重新生成）、`scripts/__pycache__/`、`test-results/`，以及 `apps/tanstack-app/.dev.vars`（`wrangler dev` 的本地运行时变量；文档路由 `apps/tanstack-app/src/routes/$lang/(root)/docs.$.tsx` 只读取构建期文档清单，正文不依赖运行时变量）；特别地，后端根目录不得存在任何 `.env*` 文件（`apps/tanstack-app/vite.config.ts` 以仓库根为 `envDir`）；并要求 `libra rev-parse HEAD` 等于 `libra rev-parse refs/remotes/origin/cf`。任一不满足即 STOP（不清理、不暂存他人改动）。随后以 `pnpm preview:cf`（按 `apps/tanstack-app/package.json:8-9` 先构建工作区——过门后等同已验证的 `cf` 提交字节——再以 `wrangler dev --port 7001` 本地运行）作基准，对本地 `http://localhost:7001/en/docs/commands/<cmd>` 与生产 `https://libra.tools/en/docs/commands/<cmd>` 分别执行下方命令块 W-2：它在内存中取得同一次响应，要求 HTTP 状态为 200、`<article>` 存在且其文本非空，输出 `<article>` 内部 HTML 原始字节的 SHA-256 与字节数（不做任何规范化：标签、属性与链接目标、`pre`/`code` 内的空白均参与比较）；退出码非 0（1 = 请求或 HTTP 错误、2 = 缺 `<article>` 或文本为空、3 = 状态非 200）即 STOP。两份 SHA-256 必须相等，即生产页 `<article>` 的渲染 HTML 与已验证 `cf` 字节的本地构建逐字节一致——页面改动为新增、删除或重排时同一判据；不相等即 STOP（不得改用规范化文本比较放行）。只把两份哈希、字节数与观察 UTC 时间写入本计划，不落盘任何响应内容；用后停止本地预览进程。部署传播延迟按 ER-10 暂时性错误有界重试（≤ 5 次、≤ 30 分钟），超预算置 `blocked`。
  8. **时序：** 第 1 步与第 2 步（含 `DEP-LS-03` 前置与目标页面干净检查）在本卡 C 组第 ⑦ 步签名提交之前完成，任一失败则本卡不得提交、合并或发布（`Lifecycle` 置 `blocked`，不进入 `remote-pending`）；第 3–7 步在本卡 Libra PR squash merge（C 组第 ⑧ 步）与 `gh release create`（第 ⑨ 步）之后执行，使网站不先于发布公开新行为（第 3–7 步开始前重跑第 2 步）；第 3–7 步未完成前本卡停留在 `remote-pending`。

上列第 1、7 步引用的命令块（`<cmd>` 替换为本卡命令；`<URL>` 为第 7 步的本地或生产页面地址）：

```bash
# W-1：定位 COMPATIBILITY.md 中 <cmd> 的状态矩阵行；必须恰好输出一个行号（基线 log=196、show=199、diff=227）
awk -F'|' -v c=" <cmd> " '$2==c && $3 ~ /^ (supported|partial|unsupported|intentionally-different) $/ {print NR}' COMPATIBILITY.md

# W-2：在内存中取得 <URL> 的同一次响应，输出 <article> 内部 HTML 原始字节（不做规范化）的「SHA-256 字节数」；退出码须为 0，不落盘响应
python3 -I - "<URL>" <<'PY'
import hashlib, html, re, sys, urllib.request
req = urllib.request.Request(sys.argv[1], headers={"User-Agent": "curl/8.7.1"})
with urllib.request.urlopen(req, timeout=30) as r:
    if r.status != 200:
        sys.exit(3)
    s = r.read().decode("utf-8")
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

- `DEP-LS-01 -> LS-01..LS-37`（逐卡开工前重新核验各卡完整写集跨计划写锁；`LS-31`、`LS-35` 只写 `diff` 侧，按 `DEP-LS-01` 核验 `tests/command/diff_test.rs`、`diff` 文档与 `COMPATIBILITY.md:227`，按 `DEP-LS-02` 核验 `src/command/diff/`）
- `DEP-LS-03 -> LS-01..LS-37`（门控各卡「文档与网站验收命令」：已由用户 2026-11-18 11:55:09 UTC 确认，各卡第 2 步复核该确认仍有效）
- Phase 0: `LS-01 -> LS-02 -> LS-03 -> LS-04 -> LS-05 -> LS-06 -> LS-07 -> LS-08 -> LS-09 -> LS-10 -> LS-11 -> LS-12`
- Phase 1: `LS-12 -> LS-13 -> LS-14 -> LS-15 -> LS-37 -> LS-36 -> LS-16 -> LS-17 -> LS-32 -> LS-18 -> LS-19 -> LS-20 -> LS-21 -> LS-28 -> LS-29 -> LS-30 -> LS-31 -> LS-35 -> LS-22`（其中 `DEP-LS-02 -> LS-20, LS-21, LS-28, LS-29, LS-30, LS-31, LS-35, LS-22`）
- Phase 2: `LS-22 -> LS-23 -> LS-26 -> LS-33 -> LS-24 -> LS-25 -> LS-27 -> LS-34`（其中 `DEP-LS-02 -> LS-23, LS-26, LS-24, LS-25, LS-27`）

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-LS-01 | incoming | 跨计划全写集互斥锁 | `issues/1040.md`、`plan-20261117-format-patch`（`FPG-11`/`22`/`27`）、`issues/478.md`（`LG-02/04/05/06/08`）、`issues/1055.md`（`I1055-02`）及 `plan-status.md` 登记的其它在途写者 | 协调者 | **每张 `LS-*` 卡开工前**重新核实 `plan-status.md` 中该卡**完整 `Implementation write set`**（含 `src/command/log.rs`、`src/command/show.rs`、`src/command/log/`、`src/utils/pager.rs`、`tests/command/{log,show,diff}_test.rs`、`docs/commands/{log,show,diff}.md` 及其 zh-CN/开发文档、`COMPATIBILITY.md:196,199,227` 与网站页）均无 `in-progress`/`blocked` 写者，取得独占写锁并于本卡推送后释放 | `docs/development/plan/issues/1040.md`、`docs/development/plan/plan-status.md:1930` | fail-closed：若有并发写者或归属不清，禁止开工，等待释放并 rebase 后再核验 |
| DEP-LS-02 | incoming | 跨计划写锁与顺序前置 | `issues/529.md`（`FP-05`/`FP-07`）、`plan-20260927#CM-08`/`CM-09`、`plan-20261117-format-patch`（`FPG-22`/`27`） | 协调者 | **每张涉 `src/command/diff/` 的卡（`LS-20..LS-31` 与 `LS-35`）开工前**重新核实 `plan-status.md` 中 `src/command/diff/`（含 `diff.rs`、`diff/render.rs`、`diff/options.rs`、`diff/compare.rs`）无在途写者；且 **`LS-20` 须待 `issues/529.md` `FP-05`（公开 `mod render`）与 `FP-07`（抽取独立 `numstat` 渲染函数）`done`/`complete` 后方可开工** | `docs/development/plan/plan-status.md:1545-1546`、`docs/development/plan/issues/529.md:507,1392,1511` | fail-closed：若有在途写者或 `FP-05`/`FP-07` 未完成，等待释放/完成后 rebase 再开工 |
| DEP-LS-03 | incoming | 审批（已满足） | 用户对网站文档交付检出路径的确认 | 用户 | **已满足**：用户 2026-11-18 11:55:09 UTC 确认本批计划的网站文档交付检出为 `/Volumes/Data/gitmono/libra-backend`（`cf` 生产分支）；对全部卡持续有效，各卡第 2 步只复核该确认未被撤回或改变 | 用户 2026-11-18 11:55:09 UTC 确认（记录于本计划「文档职责」）；`AGENTS.md:135`（`/Volumes/Sea/libra-backend`，本机不存在，已知文档滞后，本批不改）；`plan-template.md:332`（ER-06a 同级默认） | 确认已取得，无需等待；若用户撤回或改变该确认，各卡网站步骤即 STOP——尚未到 C 组第 ⑦ 步签名提交的卡不提交、不合并、不发布并置 `Lifecycle=blocked`（不进入 `remote-pending`），已完成 C 组的卡在网站第 3–7 步 STOP 并停留 `remote-pending`；按用户新指定的路径修订本计划全部网站路径并经评审后再执行，不得改用其它检出或跳过网站交付 |

### 发布分组与并发窗口

- **并发声明:** 全串行（共享 `src/command/log.rs`、`src/command/show.rs` 与 `src/command/diff/` 写集，依据 G-10 禁止并发；每张卡开工前均须重新核验 `DEP-LS-01` 及适用时的 `DEP-LS-02`）。
- **发布者:** 单一指定发布者（协调者指派的执行 Agent）。
- **发布窗口顺序:** 按 `LS-01` → `LS-02` → `LS-03` → `LS-04` → `LS-05` → `LS-06` → `LS-07` → `LS-08` → `LS-09` → `LS-10` → `LS-11` → `LS-12` → `LS-13` → `LS-14` → `LS-15` → `LS-37` → `LS-36` → `LS-16` → `LS-17` → `LS-32` → `LS-18` → `LS-19` → `LS-20` → `LS-21` → `LS-28` → `LS-29` → `LS-30` → `LS-31` → `LS-35` → `LS-22` → `LS-23` → `LS-26` → `LS-33` → `LS-24` → `LS-25` → `LS-27` → `LS-34` 依次执行 C 组 bump 与 `gh release create`。

### Phase 0: `log` 装饰与遍历基础（LS-01..LS-12）

**目标:** 修复 `log` 嵌套 tag 装饰、`log.decorate=true`、`log.excludeDecoration`、`#952` 装饰过滤、`--simplify-by-decoration`、`--clear-decorations`、`color.decorate.*`、`replaced` 标记、`#958` `--no-abbrev`、`--no-walk`/`--tags`、`--before`/`--after` 与 `-R`。

**进入条件:** `DEP-LS-01` 核实通过，工作区干净。

**退出条件:** `LS-01`..`LS-12` 全部 `done` / `complete`。

### Phase 1: `log` 输出、过滤与行追踪（LS-13..LS-22，含拆出 LS-28..LS-32、LS-35..LS-37）

**目标:** 修复 `log` TTY 脏序列、空 `--format=`/`--pretty=`、`--output=<file>`（含 `.libra/` 防护、`--json` 写文件与写入失败诊断）、无参 `--follow`、`-L` 行级历史与参数校验、`--find-object`、`--cc`、`--raw`/`--numstat`/`--dirstat`、通用 diff 输出选项、`diff --color-moved-ws`（含 `allow-indentation-change`）与 `log --color-moved`/`--color-moved-ws`。

**进入条件:** `LS-12` 完成。

**退出条件:** `LS-13`..`LS-22`、`LS-28`..`LS-32` 及 `LS-35`..`LS-37` 全部 `done` / `complete`。

### Phase 2: `log`/`show` diff 选项对齐与 `show` 补齐（LS-23, LS-26, LS-33, LS-24, LS-25, LS-27, LS-34）

**目标:** 补齐 `show --patience`/`--anchored`、`log`/`show --word-diff`/`--color-words`、`show --color-moved --word-diff`、`show --textconv` 与 `log`/`show --diff-filter`，完成全量收口门。

**进入条件:** `LS-22` 完成。

**退出条件:** `LS-23`、`LS-26`、`LS-33`、`LS-24`、`LS-25`、`LS-27`、`LS-34` 全部 `done` / `complete`，计划完成判据全量门全绿。

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
| LS-01 | implementation | 嵌套annotated-tag装饰与%D/%d | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | DEP-LS-01,DEP-LS-03 | 序列化于 DEP-LS-01 | independent | N/A | N/A |
| LS-02 | implementation | log.decorate布尔值与--pretty=raw | immutable-release | yes | yes | 8/8 | 6/8 | 2/2 | M | LS-01,DEP-LS-01,DEP-LS-03 | 序列化于 LS-01 | independent | N/A | N/A |
| LS-03 | implementation | log.excludeDecoration与无值诊断 | immutable-release | yes | yes | 4/8 | 6/8 | 2/2 | M | LS-02,DEP-LS-01,DEP-LS-03 | 序列化于 LS-02 | independent | N/A | N/A |
| LS-04 | implementation | #952_--decorate-refs剩余边界修复 | immutable-release | yes | yes | 4/8 | 4/8 | 2/2 | M | LS-03,DEP-LS-01,DEP-LS-03 | 序列化于 LS-03 | independent | N/A | N/A |
| LS-05 | implementation | log_--simplify-by-decoration | immutable-release | yes | yes | 3/8 | 4/8 | 1/1 | M | LS-04,DEP-LS-01,DEP-LS-03 | 序列化于 LS-04 | independent | N/A | N/A |
| LS-06 | implementation | --clear-decorations与initialDecorationSet | immutable-release | yes | yes | 7/8 | 6/8 | 2/2 | M | LS-05,DEP-LS-01,DEP-LS-03 | 序列化于 LS-05 | independent | N/A | N/A |
| LS-07 | implementation | color.decorate.*与提交头颜色配置 | immutable-release | yes | yes | 6/8 | 4/8 | 2/2 | M | LS-06,DEP-LS-01,DEP-LS-03 | 序列化于 LS-06 | independent | N/A | N/A |
| LS-08 | implementation | replaced标记与GIT_REPLACE_REF_BASE | immutable-release | yes | yes | 2/8 | 4/8 | 1/1 | M | LS-07,DEP-LS-01,DEP-LS-03 | 序列化于 LS-07 | independent | N/A | N/A |
| LS-09 | implementation | #958_--no-abbrev剩余边界修复 | immutable-release | yes | yes | 6/8 | 4/8 | 1/1 | M | LS-08,DEP-LS-01,DEP-LS-03 | 序列化于 LS-08 | independent | N/A | N/A |
| LS-10 | implementation | log_--no-walk与--tags | immutable-release | yes | yes | 6/8 | 4/8 | 1/1 | M | LS-09,DEP-LS-01,DEP-LS-03 | 序列化于 LS-09 | independent | N/A | N/A |
| LS-11 | implementation | log_--before/--after别名 | immutable-release | yes | yes | 7/8 | 4/8 | 1/1 | M | LS-10,DEP-LS-01,DEP-LS-03 | 序列化于 LS-10 | independent | N/A | N/A |
| LS-12 | implementation | log_-R反转diff输入方向 | immutable-release | yes | yes | 3/8 | 6/8 | 1/1 | M | LS-11,DEP-LS-01,DEP-LS-03 | 序列化于 LS-11 | independent | N/A | N/A |
| LS-13 | implementation | log_TTY多余控制序列修复 | immutable-release | yes | yes | 3/8 | 4/8 | 2/2 | M | LS-12,DEP-LS-01,DEP-LS-03 | 序列化于 LS-12 | independent | N/A | N/A |
| LS-14 | implementation | 空--format/--pretty行为对齐 | immutable-release | yes | yes | 5/8 | 4/8 | 1/1 | M | LS-13,DEP-LS-01,DEP-LS-03 | 序列化于 LS-13 | independent | N/A | N/A |
| LS-15 | implementation | log_--output新目标写入与.libra/防护 | immutable-release | yes | yes | 8/8 | 4/8 | 1/1 | M | LS-14,DEP-LS-01,DEP-LS-03 | 序列化于 LS-14 | independent | N/A | N/A |
| LS-37 | implementation | log_--output覆盖既有目标与既有目标防护 | immutable-release | yes | yes | 8/8 | 6/8 | 1/1 | M | LS-15,DEP-LS-01,DEP-LS-03 | 序列化于 LS-15 | independent | LS-15 | N/A |
| LS-36 | implementation | log_--json与--output组合 | immutable-release | yes | yes | 2/8 | 6/8 | 1/1 | M | LS-37,DEP-LS-01,DEP-LS-03 | 序列化于 LS-37 | independent | LS-15 | N/A |
| LS-16 | implementation | log_--follow无参布尔标志 | immutable-release | yes | yes | 7/8 | 4/8 | 2/2 | M | LS-36,DEP-LS-01,DEP-LS-03 | 序列化于 LS-36 | independent | N/A | N/A |
| LS-17 | implementation | log_-L行级历史输出 | immutable-release | yes | yes | 6/8 | 4/8 | 1/1 | M | LS-16,DEP-LS-01,DEP-LS-03 | 序列化于 LS-16 | independent | N/A | N/A |
| LS-32 | implementation | log_-L参数校验 | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | LS-17,DEP-LS-01,DEP-LS-03 | 序列化于 LS-17 | independent | LS-17 | N/A |
| LS-18 | implementation | log_--find-object | immutable-release | yes | yes | 4/8 | 4/8 | 1/1 | M | LS-32,DEP-LS-01,DEP-LS-03 | 序列化于 LS-32 | independent | N/A | N/A |
| LS-19 | implementation | log_--cc组合差异 | immutable-release | yes | yes | 3/8 | 4/8 | 1/1 | M | LS-18,DEP-LS-01,DEP-LS-03 | 序列化于 LS-18 | independent | N/A | N/A |
| LS-20 | implementation | log_--raw/--numstat/--dirstat | immutable-release | yes | yes | 6/8 | 4/8 | 2/3 | M | LS-19,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-19 | independent | N/A | N/A |
| LS-21 | implementation | log_diff空白忽略(-w/-b) | immutable-release | yes | yes | 3/8 | 6/8 | 2/3 | M | LS-20,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-20 | independent | N/A | N/A |
| LS-28 | implementation | log_diff路径前缀(--no-prefix/--src-prefix/--dst-prefix) | immutable-release | yes | yes | 5/8 | 6/8 | 2/3 | M | LS-21,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-21 | independent | LS-21 | N/A |
| LS-29 | implementation | log_diff_--full-index | immutable-release | yes | yes | 2/8 | 6/8 | 2/3 | M | LS-28,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-28 | independent | LS-21 | N/A |
| LS-30 | implementation | log_diff_-U/--unified上下文行数 | immutable-release | yes | yes | 3/8 | 6/8 | 2/4 | M | LS-29,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-29 | independent | LS-21 | N/A |
| LS-31 | implementation | diff_--color-moved-ws空白模式 | immutable-release | yes | yes | 8/8 | 6/8 | 2/2 | M | LS-30,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-30 | independent | LS-22 | N/A |
| LS-35 | implementation | diff_--color-moved-ws=allow-indentation-change | immutable-release | yes | yes | 4/8 | 4/8 | 2/2 | M | LS-31,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-31 | independent | LS-31 | N/A |
| LS-22 | implementation | log_--color-moved与--color-moved-ws复用 | immutable-release | yes | yes | 5/8 | 6/8 | 2/3 | M | LS-35,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-35 | independent | N/A | N/A |
| LS-23 | implementation | show_--patience与--anchored | immutable-release | yes | yes | 2/8 | 6/8 | 2/4 | M | LS-22,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-22 | independent | N/A | N/A |
| LS-26 | implementation | log_--word-diff与--color-words | immutable-release | yes | yes | 8/8 | 4/8 | 2/3 | M | LS-23,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-23 | independent | N/A | N/A |
| LS-33 | implementation | show_--word-diff与--color-words | immutable-release | yes | yes | 8/8 | 4/8 | 1/2 | M | LS-26,DEP-LS-01,DEP-LS-03 | 序列化于 LS-26 | independent | LS-26 | N/A |
| LS-24 | implementation | show_--color-moved与--word-diff组合 | immutable-release | yes | yes | 6/8 | 6/8 | 2/4 | M | LS-33,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-33 | independent | N/A | N/A |
| LS-25 | implementation | show_--textconv与--no-textconv | immutable-release | yes | yes | 7/8 | 8/8 | 2/4 | M | LS-24,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-24 | independent | N/A | N/A |
| LS-27 | implementation | log_--diff-filter | immutable-release | yes | yes | 4/8 | 4/8 | 1/2 | M | LS-25,DEP-LS-01,DEP-LS-02,DEP-LS-03 | 序列化于 LS-25 | independent | N/A | N/A |
| LS-34 | implementation | show_--diff-filter | immutable-release | yes | yes | 4/8 | 4/8 | 1/2 | M | LS-27,DEP-LS-01,DEP-LS-03 | 序列化于 LS-27 | independent | LS-27 | N/A |

---

### Task LS-01: 修复 `log --decorate` 嵌套 annotated-tag 链与 `%D`/`%d` 输出（#949）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-20：`0.30.218`；卡特有用例 1/1、共享 `compat_pretty_format_placeholders` 全通过、版本守卫 2/2；Codex R9 `VERDICT: PASS`；EN/zh/开发/COMPATIBILITY 与网站 `log.en.md` 同卡同步；未跑全量、未等待远端 CI）

**Description:** 将 `create_reference_commit_map` 对 annotated tag 的解引用由单层改为迭代（非递归）解引用整条 tag 链（直到非 tag 目标），使 `log --decorate`、`%D` 与 `%d` 对嵌套 tag 显示链上全部标签，而非丢弃。

**Out of scope:** `log.decorate` 配置与 `--pretty=raw`（由 `LS-02` 处理）；`replaced` 装饰（由 `LS-08` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `create_reference_commit_map` 对 `TagObject::Tag(t)` 只取 `t.object_hash` 一层，嵌套 tag（tag→tag→commit）的外层标签挂到内层 tag 对象而非提交上 | `src/command/log.rs:4202-4242`（单层解引用在 `4229`） |

**Acceptance criteria:**

- [x] **AC1** `libra log --decorate=short -1` 的 stdout 首行含装饰 `(HEAD -> main, tag: t2, tag: t1)`。
- [x] **AC2** `libra log -1 --format=%D` 的 stdout 逐字为 `HEAD -> main, tag: t2, tag: t1\n`。
- [x] **AC3** `libra log -1 --format=%d` 的 stdout 逐字为 ` (HEAD -> main, tag: t2, tag: t1)\n`。
- [x] **AC4** 200 层嵌套链的 `libra log -1 --format=%D` 输出去掉行尾换行后按 `, ` 切分得到的装饰项集合包含 `tag: d1` 至 `tag: d200` 全部 200 项（逐项完整匹配，GC-10：深链有界完成）。
- [x] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F01：提交 C（分支 `main`）；annotated tag `t1`→C；annotated tag `t2`→tag 对象 `t1`；另建 200 层嵌套 annotated tag 链 `d1`→C、`d2`→`d1`…`d200`→`d199` 的独立夹具；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::decorate_nested_annotated_tags`：期望 exit 0；stdout 首行含 `(HEAD -> main, tag: t2, tag: t1)`（本机 Git 2.54.0 预探值）。
- AC2 → `command::log_test::decorate_nested_annotated_tags`：期望 exit 0；stdout 逐字等于 `HEAD -> main, tag: t2, tag: t1\n`。
- AC3 → `command::log_test::decorate_nested_annotated_tags`：期望 exit 0；stdout 逐字等于 ` (HEAD -> main, tag: t2, tag: t1)\n`。
- AC4 → `command::log_test::decorate_nested_annotated_tags`：期望 exit 0；stdout 去掉行尾换行后按 `, ` 切分得到的装饰项集合包含 `tag: d1` 至 `tag: d200` 全部 200 项（逐项完整匹配，不以子串判定；进程 panic 时退出码非 0，已由 `exit 0` 前提排除）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [x] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::decorate_nested_annotated_tags)'`（1/1 PASS）（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项执行）
- [x] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`（共享回归组：整 target，`%D`/`%d` 占位符回归；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_pretty_format_placeholders` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`%D`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=嵌套annotated-tag装饰与%D/%d; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=DEP-LS-01,DEP-LS-03; writeset=序列化于DEP-LS-01; release=independent; split-from=N/A; exception=N/A`

---

**Execution evidence (2026-11-20):** `decorate_nested_annotated_tags` covers t1→C, t2→t1 and the 200-deep d1..d200 chain, plus `%D`/`%d` forcing decoration under `--decorate=no`/`--no-decorate`; Codex R9 PASS; version guard 2/2 at `0.30.218`.


### Task LS-02: 修复 `log.decorate=true` 与 `--pretty=raw` 装饰规则（#950）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`str_to_decorate_option` 增加 Git 布尔映射（`true`/`yes`/`on`/`1`→`short`，`false`/`no`/`off`/`0`→`no`）；配置路径把非法取值映射为 `no`（Git 解析为 0，不报错、不再回落 `auto`）；`determine_decorate_option` 在显式命令行装饰之后、读取配置之前检测命令行 `--pretty=raw`/`--format=raw` 并直接返回 `No`（`format.pretty=raw` 不触发抑制）。用例 `test_log_decorate_config_boolean_and_raw_suppression` 覆盖 AC1–AC8（真/假值表驱动、非法值等同 `no` 且 stderr 为空、`--pretty=raw` 抑制、显式 `--decorate=short` 优先、`format.pretty=raw` 不抑制、`--format=raw` 抑制）；`log_test` **256/256**；文档 EN/zh-CN/开发版与 `COMPATIBILITY.md` 同步；版本面 0.30.275 → 0.30.276。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 修复 `str_to_decorate_option` / `determine_decorate_option`：`log.decorate` 接受 Git 布尔真值（启用 `short`）与假值（关闭），非法值按 Git `parse_decoration_style` 负值语义等同关闭且不报错；命令行 `--pretty=raw`/`--format=raw` 时不应用 `log.decorate` 配置装饰（显式 `--decorate` 仍生效），`format.pretty=raw` 配置不触发该抑制（与 Git 一致）。

**Out of scope:** `log.excludeDecoration`（由 `LS-03` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `determine_decorate_option` 读 `log.decorate` 后经 `str_to_decorate_option` 只识别 `no/short/full/auto`，`true` 等布尔值被丢弃并回落 `auto`；未区分 `--pretty=raw` | `src/command/log.rs:1020-1075` |
| Git 对非法 `log.decorate` 置 0（不装饰、不报错），`--pretty=raw` 时抑制配置装饰 | `git@6de20f6092:builtin/log.c`（`log.decorate` 分支）；本机 Git 2.54.0 预探 |

**Acceptance criteria:**

- [ ] **AC1** `log.decorate` 为 Git 布尔真值（表驱动 `true`/`yes`/`on`/`1`）且命令行未给 `--decorate`/`--no-decorate` 时，`libra log -1 --oneline` 的 stdout 逐字为 `<abbrev> (HEAD -> main, tag: v1) <subject>\n`（short 装饰）。
- [ ] **AC2** `log.decorate` 为 Git 布尔假值（表驱动 `false`/`no`/`off`/`0`）时 `libra log -1 --oneline` 的 stdout 逐字为 `<abbrev> <subject>\n`（不输出装饰）。
- [ ] **AC3** `log.decorate=bogus` 时 `libra log -1 --oneline` 的 stdout 与 `log.decorate=no` 时逐字相同（Git 非法值等同不装饰）。
- [ ] **AC4** `log.decorate=bogus` 时 `libra log -1 --oneline` 的 stderr 为空（Git 非法值不报错）。
- [ ] **AC5** `log.decorate=short` 与命令行 `--pretty=raw` 同用时 stdout 首行逐字为 `commit <40 位 OID>`（不输出装饰）。
- [ ] **AC6** `--pretty=raw --decorate=short` 显式开启时 stdout 首行逐字为 `commit <40 位 OID> (HEAD -> main, tag: v1)`。
- [ ] **AC7** `format.pretty=raw` 配置 + `log.decorate=short`（命令行无 `--pretty`）时 stdout 首行逐字为 `commit <40 位 OID> (HEAD -> main, tag: v1)`。
- [ ] **AC8** `log.decorate=short` 与命令行 `--format=raw`（`LogArgs::format` 字段）同用时 stdout 首行逐字为 `commit <40 位 OID>`（不输出装饰）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F02：单提交 C（HEAD → main），annotated tag `v1`→C（short 装饰为 `HEAD -> main, tag: v1`，full 装饰为 `HEAD -> refs/heads/main, tag: refs/tags/v1`）；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::log_decorate_bool_true_and_pretty_raw`：期望 每个取值 exit 0；stdout 逐字为 `<abbrev> (HEAD -> main, tag: v1) <subject>\n`（排除 full 样式与多余装饰）。
- AC2 → `command::log_test::log_decorate_bool_true_and_pretty_raw`：期望 每个取值 exit 0；stdout 逐字为 `<abbrev> <subject>\n`。
- AC3 → `command::log_test::log_decorate_bool_true_and_pretty_raw`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC4 → `command::log_test::log_decorate_bool_true_and_pretty_raw`：期望 exit 0；stderr 为 0 字节。
- AC5 → `command::log_test::log_decorate_bool_true_and_pretty_raw`：期望 exit 0；stdout 首行逐字为 `commit <40 位 OID>`。
- AC6 → `command::log_test::log_decorate_bool_true_and_pretty_raw`：期望 exit 0；stdout 首行逐字为 `commit <40 位 OID> (HEAD -> main, tag: v1)`。
- AC7 → `command::log_test::log_decorate_bool_true_and_pretty_raw`：期望 exit 0；stdout 首行逐字为 `commit <40 位 OID> (HEAD -> main, tag: v1)`（Git 2.54.0 预探）。
- AC8 → `command::log_test::log_decorate_bool_true_and_pretty_raw`：期望 exit 0；stdout 首行逐字为 `commit <40 位 OID>`（Git 2.54.0 预探：`git -c log.decorate=short log --format=raw -1`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::log_decorate_bool_true_and_pretty_raw)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`（共享回归组：整 target，配置默认值语义回归；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_config_defaults_semantics` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_invalid_decorate_uses_command_usage_error)'`（共享回归组：既有 `--decorate=<非法值>` 命令行报错用例（命令行非法值仍报错，区别于配置非法值）；预期 N = 1；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-01`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`log.decorate`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log.decorate布尔值与--pretty=raw; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=LS-01,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-01; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-03: 支持 `log.excludeDecoration` 配置（#951；无值键诊断在 Libra 不适用）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`decorate_name_allowed` 接收 `log.excludeDecoration` 多值配置；显式 `--decorate-refs` 替换该配置过滤；空串值按 Git 空排除模式处理（隐藏全部引用但保留 `HEAD` 标记）；配置读取走 `get_var_all_case_insensitive("log.", "excludeDecoration")`。用例 `log_exclude_decoration_config` 覆盖 AC1–AC4；Libra 无「无值键」形态（`config_kv.value NOT NULL`），Git 的缺值诊断在 `COMPATIBILITY.md` 登记为有意差异。`log_test` **257/257**；文档 EN/zh-CN/开发版与 `COMPATIBILITY.md` 同步；版本面 0.30.280 → 0.30.281。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 读取多值配置 `log.excludeDecoration` 作为配置级排除模式参与装饰过滤（存在时不再套用默认装饰命名空间过滤，命令行 `--decorate-refs` 可覆盖，语义对齐 Git `set_default_decoration_filter`）；空串值按 Git 语义作为空排除模式（匹配全部引用）。`#951` 所述 Git「无值键」（`[log] excludeDecoration` 不带 `=`，`t/t4202-log.sh:865-878` 期望 `error: missing value for 'log.excludeDecoration'`）在 Libra 无法表示：`config_kv.value` 为 `NOT NULL`，`libra config --add <key>` 不给值即以 `LBR-CLI-002` 拒绝；因此缺值诊断在 Libra 不适用，本卡在 `COMPATIBILITY.md` 的 `| log |` 行登记该有意差异并据此关闭 `#951` 的该部分。

**Out of scope:** `--clear-decorations` 与 `log.initialDecorationSet`（由 `LS-06` 处理）；Git 无值键的缺值诊断（Libra 配置无无值键形态，见描述）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `decorate_name_allowed` 仅读 CLI `args.decorate_refs`/`decorate_refs_exclude`，未读 `log.excludeDecoration` | `src/command/log.rs:646-672` |
| Libra 配置没有 Git 的「无值键」形态（空串是合法值，但不存在缺值的键）：`config_kv.value TEXT NOT NULL`；`libra config --add log.bar`（无值）报 `missing required argument: <value>`（`LBR-CLI-002`，2026-11-18 以 libra 0.30.207 预探） | `sql/sqlite_20260309_init.sql:12-17` |
| Git 对无值键报缺值错误、对显式空串按空排除模式处理（Git 2.54.0 预探：`-c log.excludeDecoration=` 时装饰为 `(HEAD)`） | `git@6de20f6092:t/t4202-log.sh:865-878` |

**Acceptance criteria:**

- [ ] **AC1** `log.excludeDecoration=refs/tags/*` 时 `libra log -1 --oneline --decorate=short` 的装饰恰为 `(HEAD -> main)`。
- [ ] **AC2** 两条 `log.excludeDecoration`（`refs/tags/*` 与 `refs/heads/*`）同时生效，装饰恰为 `(HEAD)`。
- [ ] **AC3** `log.excludeDecoration=refs/tags/*` 下命令行 `--decorate-refs=refs/tags/v1` 覆盖配置排除，装饰恰为 `(tag: v1)`。
- [ ] **AC4** `log.excludeDecoration` 取空串值（`libra config set log.excludeDecoration ""`）时按 Git 空排除模式处理，装饰恰为 `(HEAD)`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F03：单提交 C（HEAD → main），另有 tag `v1`→C；配置经 `libra config set` / `libra config --add` 写入；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::log_exclude_decoration_config`：期望 exit 0；stdout 的装饰部分逐字为 `(HEAD -> main)`。
- AC2 → `command::log_test::log_exclude_decoration_config`：期望 exit 0；stdout 的装饰部分逐字为 `(HEAD)`（Git 2.54.0 同构预探）。
- AC3 → `command::log_test::log_exclude_decoration_config`：期望 exit 0；stdout 的装饰部分逐字为 `(tag: v1)`。
- AC4 → `command::log_test::log_exclude_decoration_config`：期望 exit 0；stdout 的装饰部分逐字为 `(HEAD)`（Git 2.54.0 预探：`git -c log.excludeDecoration= log -1 --oneline --decorate=short`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::log_exclude_decoration_config)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`（共享回归组：整 target，配置默认值语义回归；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_config_defaults_semantics` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::log_test::lo0[78]_/)'`（共享回归组：既有 LO-07/LO-08 装饰过滤用例族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-02`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`log.excludeDecoration`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡须在该行登记有意差异，`COMPATIBILITY.md` 行必须更新。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log.excludeDecoration与无值诊断; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=LS-02,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-02; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-04: 核实 `534` 后 `#952` `--decorate-refs` 剩余边界并修复（#952）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：复现发现 `create_reference_commit_map` 只枚举本地分支，装饰中缺失 `refs/remotes/*`；补上按已配置 remote 枚举远端跟踪分支（并剥离可能存在的 `refs/remotes/<remote>/` 前缀）后，AC1–AC4 输出与 Git 同夹具期望逐字一致：`(tag: v1-tag)`、`(HEAD)`、`(tag: v1-tag, origin/main, topic, main)`、`(HEAD -> main, tag: v1-tag, origin/main, topic)`。用例 `decorate_refs_issue_952_remaining` 覆盖 AC1–AC4；`log_test` **258/258**；文档 EN/zh-CN 同步；版本面 0.30.281 → 0.30.282。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 按 GC-01 核实 `issues/534.md`（`LO-05..LO-08`）已合入的 `--decorate-refs` / `--decorate-refs-exclude` 匹配、HEAD 过滤与排列，针对 `#952` 仍未关闭的边界（与 `git@6de20f6092` 对照复现）修复。开工复现到下列 AC 之外的失败场景时，先把每个场景作为独立 AC 补入本卡并重算 G-03（超过 8 条按 ER-03 拆卡），不得在实现中静默扩张。

**Out of scope:** `--clear-decorations`（由 `LS-06` 处理）；`log.excludeDecoration`（`LS-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/534.md` 记录 `#952` 在 `LO-09` 后仍有失败项或未发帖保持 OPEN | `issues/534.md:3,11`、`src/command/log/decorate_filter.rs:15-55` |

**Acceptance criteria:**

- [ ] **AC1** 无 `refs/` 前缀的通配 `--decorate-refs='*tag*'` 选出的装饰集合等于 Git 同夹具期望。
- [ ] **AC2** `libra log -1 --oneline --decorate=short --decorate-refs=HEAD` 的装饰等于 Git 同夹具期望。
- [ ] **AC3** `libra log -1 --oneline --decorate=short --decorate-refs-exclude=HEAD` 的装饰（去掉 `HEAD -> ` 箭头后分支装饰的形态与位置）等于 Git 同夹具期望。
- [ ] **AC4** `libra log -1 --oneline --decorate=short` 中同一提交多个装饰的排列顺序等于 Git 同夹具期望。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F04：提交 C（HEAD → main，分支 `topic`，tag `v1-tag`，远端跟踪 `origin/main`）；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::decorate_refs_issue_952_remaining`：期望 exit 0；stdout 的装饰集合等于 Git 同夹具期望（Git 2.54.0 预探：{`tag: v1-tag`}）。
- AC2 → `command::log_test::decorate_refs_issue_952_remaining`：期望 exit 0；stdout 装饰逐字等于 Git 同夹具期望（Git 2.54.0 预探：`(HEAD)`）。
- AC3 → `command::log_test::decorate_refs_issue_952_remaining`：期望 exit 0；stdout 装饰逐字等于 Git 同夹具期望（Git 2.54.0 预探：`(tag: v1-tag, origin/main, topic, main)`）。
- AC4 → `command::log_test::decorate_refs_issue_952_remaining`：期望 exit 0；stdout 装饰顺序逐字等于 Git 同夹具期望（Git 2.54.0 预探：`(HEAD -> main, tag: v1-tag, origin/main, topic)`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::decorate_refs_issue_952_remaining)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::log_test::lo0[578]_/)'`（共享回归组：既有 LO-05/LO-07/LO-08 装饰顺序与过滤用例族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-03`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/log/decorate_filter.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/decorate_filter.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--decorate-refs`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=#952_--decorate-refs剩余边界修复; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=LS-03,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-03; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-05: 实现 `log --simplify-by-decoration`（#953）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：`LogArgs` 新增 `--simplify-by-decoration`；实现为「保留经装饰过滤后的被引用提交 + 仓库根提交」，装饰过滤复用 `decorate_name_allowed`/`decorate_ref_allowed`（含 `log.excludeDecoration`）；简化时即使装饰输出关闭也会构建引用映射。用例 `simplify_by_decoration` 覆盖 AC1–AC3（线性+分支+合并夹具）：输出序列 `merge, s2, b, root`、`s2, root`、`merge, s2, b, root`，与本机 `git 2.56.0` 同夹具 `git log --simplify-by-decoration` 结果逐字一致。`log_test` **259/259**；文档 EN/zh-CN/开发版同步；版本面 0.30.282 → 0.30.283。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 在 `LogArgs` 新增 `--simplify-by-decoration`，按 Git 语义以装饰为依据简化历史：被（经装饰过滤后的）分支或 tag 引用的提交必选，Git 为保持有意义的历史还会额外显示部分未装饰提交（`git@6de20f6092:Documentation/rev-list-options.adoc` 的 `--simplify-by-decoration` 段）；输出以 `git@6de20f6092` 同夹具输出为准，不以「只输出被装饰提交」实现。

**Out of scope:** `--full-history` / `--dense` / `--sparse` 等其他简化模式；与位置 pathspec 组合时 Git 另保留触及该路径的提交（`git@6de20f6092:revision.c` 的路径限定分支）的语义（均尚未排期，重启条件：新 issue）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `simplify_by_decoration` 字段 | `src/command/log.rs:172-486` |

**Acceptance criteria:**

- [ ] **AC1** `libra log --simplify-by-decoration --format=%H` 的 stdout 逐字等于 Git 同夹具期望（提交序列）。
- [ ] **AC2** `libra log --simplify-by-decoration --decorate-refs=refs/tags/v2 --format=%H` 的 stdout 逐字等于 Git 同夹具期望（按过滤后的装饰集简化）。
- [ ] **AC3** `libra log --simplify-by-decoration --decorate-refs-exclude=refs/tags/v2 --format=%H` 的 stdout 逐字等于 Git 同夹具期望。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F05：含未装饰线性提交、分支与合并、`v1`/`v2` tag 的历史；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::simplify_by_decoration`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC2 → `command::log_test::simplify_by_decoration`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC3 → `command::log_test::simplify_by_decoration`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::simplify_by_decoration)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_shallow_walk_helper`（共享回归组：整 target，历史遍历 helper 守卫；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_shallow_walk_helper` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-04`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--simplify-by-decoration`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--simplify-by-decoration; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-04,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-04; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-06: 支持 `--clear-decorations` 与 `log.initialDecorationSet`（#954）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `complete`（2026-11-21：新增 `--clear-decorations`（`overrides_with_all` 清除此前 `--decorate-refs`/`--decorate-refs-exclude`）与 `log.initialDecorationSet=all`；`decorate_name_allowed` 增加默认命名空间过滤（仅 `HEAD`/`refs/heads/`/`refs/tags/`/`refs/remotes/`/`refs/stash`），`create_reference_commit_map` 把名为 `refs/notes/*` 的 `reference` 行分类为 `ReferenceKind::Notes` 并以 `notes/<name>` 显示。用例 `log_clear_decorations_and_initial_decoration_set` 覆盖三态（默认隐藏 notes、`--clear-decorations` 显示、`initialDecorationSet=all` 显示），夹具按卡内说明直接向 `reference` 表插入 `refs/notes/custom` 行；`log_test` **260/260**；fmt + 严格 Clippy exit 0；文档 EN/zh-CN/开发版同步；版本面 0.30.283 → 0.30.284。按用户 2026-11-20 指令在 `main` 直接提交推送。）

**Description:** 在 `LogArgs` 新增 `--clear-decorations`（清除其前出现的 `--decorate-refs` / `--decorate-refs-exclude` 并放开默认装饰命名空间过滤），并支持 `log.initialDecorationSet=all`（等价于放开默认过滤）；其它取值按 Git 忽略且不报错。装饰收集须能覆盖默认命名空间之外的引用。Libra 自身命令目前不生成这类引用（`libra notes` 把 notes 存于 SQLite `notes` 表，`libra update-ref` 只支持 `refs/heads/*`，`libra clone` 不导入源仓库的 `refs/notes/*`——2026-11-18 以 libra 0.30.207 预探），但 `reference` 表可保存名为 `refs/notes/*` 的行且 `log` 已有读取路径 `collect_notes_reference_tips`，本卡以这一形态的引用为验收对象，并让装饰收集按完整引用名把名为 `refs/notes/*` 的行识别为 notes 命名空间引用（当前它们若以 `kind = 'Branch'` 存储，会被 `Branch::list_branches_best_effort` 当作本地分支装饰），从而在默认过滤下不显示、放开后显示。

**Out of scope:** 不改变 `--decorate-refs` 自身模式语法；`log.excludeDecoration`（`LS-03`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `clear_decorations`；`log/config.rs` 未读 `log.initialDecorationSet`；装饰仅收集分支/远端/tag | `src/command/log.rs:207-214,4202-4242`、`src/command/log/config.rs:1-156` |
| `reference` 表可保存完整名为 `refs/notes/*` 的行，`collect_notes_reference_tips` 按名称前缀读取；Libra 自身命令当前不生成此类引用；测试已有直接写入 `reference` 表的夹具先例 | `sql/sqlite_20260309_init.sql:19-30`、`src/command/log.rs:1522-1550`、`tests/agent_doctor_repair_test.rs:418` |
| `create_reference_commit_map` 以 `Branch::list_branches_best_effort` 收集全部 `kind = 'Branch'` 且 `remote` 为空的行作为本地分支装饰，名为 `refs/notes/*` 的行当前也会被当作本地分支 | `src/command/log.rs:4205-4219`、`src/internal/branch.rs:349-357` |
| Git 默认装饰过滤只含 `HEAD`、`refs/heads/`、`refs/tags/`、`refs/remotes/`、`refs/stash`、`refs/replace/`（`refs/notes/` 不在其中） | `git@6de20f6092:refs.c:113-166`（`ref_namespace` 的 `decoration` 字段） |
| Git 仅识别 `all`，其它值忽略 | `git@6de20f6092:builtin/log.c`（`set_default_decoration_filter`）；本机 Git 2.54.0 预探 |

**Acceptance criteria:**

- [ ] **AC1** `log.initialDecorationSet=all` 时 R 出现在装饰中。
- [ ] **AC2** `log.initialDecorationSet=useful`（非 `all` 取值）时 stdout 与未设置该配置时逐字相同（Git 忽略非 `all` 取值）。
- [ ] **AC3** `log.initialDecorationSet=useful` 时 stderr 为空（不报错）。
- [ ] **AC4** `--clear-decorations` 放开默认过滤，R 出现在装饰中。
- [ ] **AC5** `libra log -1 --oneline --decorate=short --decorate-refs=refs/heads --clear-decorations` 的 stdout 与仅 `--clear-decorations` 时逐字相同（其前的 include 被清除）。
- [ ] **AC6** `--clear-decorations --decorate-refs=refs/heads`：其后的 include 重新生效，C 的装饰恰为 `(main)`。
- [ ] **AC7** 未设置 `log.initialDecorationSet` 且无 `--clear-decorations`/`--decorate-refs*` 时，`libra log -1 --oneline --decorate=short` 的装饰恰为 `(HEAD -> main)`（R 被 Git 默认装饰过滤排除）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F06：提交 C（HEAD → main）；在任何被测调用之前，于 `.libra/libra.db` 的 `reference` 表直接写入一行 `name = 'refs/notes/commits'`、`kind = 'Branch'`、`commit = <C>`、`remote = NULL` 作为非默认命名空间引用 R（夹具级写入，被测调用前后的引用快照按第 4 项比较）；全部调用以 C 为 `-1` 目标；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::clear_decorations_and_initial_set`：期望 exit 0；stdout 装饰含 R 的完整引用名 `refs/notes/commits`（Git 2.54.0 同构预探：`(HEAD -> main, refs/notes/commits)`）。
- AC2 → `command::log_test::clear_decorations_and_initial_set`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC3 → `command::log_test::clear_decorations_and_initial_set`：期望 exit 0；stderr 为 0 字节。
- AC4 → `command::log_test::clear_decorations_and_initial_set`：期望 exit 0；stdout 装饰含 R 的完整引用名。
- AC5 → `command::log_test::clear_decorations_and_initial_set`：期望 exit 0；stdout 与仅 `--clear-decorations` 时逐字相同。
- AC6 → `command::log_test::clear_decorations_and_initial_set`：期望 exit 0；stdout 装饰恰为 `(main)`（Git 2.54.0 预探）。
- AC7 → `command::log_test::clear_decorations_and_initial_set`：期望 exit 0；stdout 装饰恰为 `(HEAD -> main)`（Git 2.54.0 同构预探）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::clear_decorations_and_initial_set)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC7；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`（共享回归组：整 target，配置默认值语义回归；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_config_defaults_semantics` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::log_test::lo0[78]_/)'`（共享回归组：既有 LO-07/LO-08 装饰过滤用例族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-05`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--clear-decorations`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--clear-decorations与initialDecorationSet; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=LS-05,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-05; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-07: 支持 `color.decorate.*` 与 `diff.color.commit` 着色配置（#959）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-21：装饰着色改为**显式 SGR 拼接**——新增 `DecorateColors`、`paint`、`color_slot_to_sgr`（`normal`/基本八色/`bold` 等属性/`#rrggbb`）与 `resolve_decorate_colors`（默认调色板按 Git 实测：`commit` 33、`HEAD` 1;36、`branch` 1;32、`remoteBranch` 1;31、`tag` 1;33），`color.diff.commit` 支持旧名 `diff.color.commit`；装饰段按 Git 分段着色（` (`, `, `, `)` 用 commit 色，`tag: ` 与 tag 名各一段），`FormatContext` 拆分 `decoration`（纯文本，供 `%D`）与 `decoration_wrapped`（带括号与颜色，供内建格式与 `%d`），`commit <oid>` 由 `commit_color` 着色。与 `git 2.56.0` 默认输出逐段比对一致；`log_test` **260/260**、严格 Clippy exit 0。AC1–AC6 已由 `command::log_test::log_colour_decorate_slots_match_git_bytes` 逐字节断言（默认调色板 + 四个装饰槽 + `color.diff.commit` + 旧名 `diff.color.commit`），全绿。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 读取 `color.decorate.{branch,remoteBranch,tag,HEAD}` 与 `color.diff.commit`（及其旧名 `diff.color.commit`），在 `libra log -1 --decorate=short --color=always`（medium 格式，首行为 `commit <oid> (<装饰>)`）下按配置为装饰与 `commit <oid>` 着色，替换硬编码颜色；着色字节按 Git 分段输出（例如 tag 装饰的 `tag: ` 前缀与 tag 名各自包裹 SGR，`git@6de20f6092:log-tree.c` 的 `format_decorations`）。

**Out of scope:** `color.decorate.stash` / `color.decorate.grafted`：Libra 当前不产生 stash/grafted 装饰，配置这两个 slot 不报错且无可见效果（尚未排期，重启条件：Libra 开始渲染 stash/grafted 装饰）；不改变非装饰区域的 diff hunk 配色。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ReferenceKind` 注释与渲染使用硬编码装饰颜色（tag 黄、remote 红、local 绿），未读 `color.decorate.*` / `color.diff.commit` | `src/command/log.rs:1120-1126` |

**Acceptance criteria:**

- [ ] **AC1** `color.decorate.branch=blue` 时 `libra log -1 --decorate=short --color=always` 的 stdout 首行（含全部 SGR）逐字等于 Git 同夹具期望（本地分支 `main` 以 blue 着色）。
- [ ] **AC2** `color.decorate.remoteBranch=blue` 时同一命令的 stdout 首行逐字等于 Git 同夹具期望（`origin/main` 以 blue 着色）。
- [ ] **AC3** `color.decorate.tag=blue` 时同一命令的 stdout 首行逐字等于 Git 同夹具期望（`tag: ` 前缀与 `v1` 两段各自以 blue 着色）。
- [ ] **AC4** `color.decorate.HEAD=blue` 时同一命令的 stdout 首行逐字等于 Git 同夹具期望（`HEAD` 以 blue 着色）。
- [ ] **AC5** `color.diff.commit=blue` 时同一命令的 stdout 首行逐字等于 Git 同夹具期望（`commit <oid>` 以 blue 着色）。
- [ ] **AC6** 旧名 `diff.color.commit=blue` 时同一命令的 stdout 首行与 AC5（`color.diff.commit=blue`）逐字相同。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F07：提交 C（HEAD → main，tag `v1`，远端跟踪 `origin/main`）；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::color_decorate_and_commit_config`：期望 exit 0；stdout 首行逐字等于 Git 同夹具期望。
- AC2 → `command::log_test::color_decorate_and_commit_config`：期望 exit 0；stdout 首行逐字等于 Git 同夹具期望。
- AC3 → `command::log_test::color_decorate_and_commit_config`：期望 exit 0；stdout 首行逐字等于 Git 同夹具期望。
- AC4 → `command::log_test::color_decorate_and_commit_config`：期望 exit 0；stdout 首行逐字等于 Git 同夹具期望。
- AC5 → `command::log_test::color_decorate_and_commit_config`：期望 exit 0；stdout 首行逐字等于 Git 同夹具期望。
- AC6 → `command::log_test::color_decorate_and_commit_config`：期望 两次调用均 exit 0；stdout 首行逐字相同。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::color_decorate_and_commit_config)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_config_defaults_semantics`（共享回归组：整 target，配置默认值语义回归；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_config_defaults_semantics` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-06`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`color.decorate.`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=color.decorate.*与提交头颜色配置; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=LS-06,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-06; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-08: `log --decorate` 显示 `replaced` 标记与支持 `GIT_REPLACE_REF_BASE`（#960）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 装饰收集读取 replace 引用（默认前缀 `refs/replace/`，可由 `GIT_REPLACE_REF_BASE` 覆盖），为存在替换的提交输出 `replaced` 装饰。

**Out of scope:** 不改变 `libra replace` 写入逻辑；不改变 `log` 对替换对象内容的读取语义。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 装饰收集只遍历分支与 tag，未读 replace 引用，也未读 `GIT_REPLACE_REF_BASE` | `src/command/log.rs:4202-4242` |
| `libra replace` 以松散引用存于 `.libra/refs/replace/<oid>` | `src/command/replace.rs:4` |

**Acceptance criteria:**

- [ ] **AC1** 存在 `refs/replace/<C1>` 时 `libra log --decorate=short --oneline` 在 C1 行输出 `(replaced)`。
- [ ] **AC2** `GIT_REPLACE_REF_BASE=refs/custom-replace/` 且仅自定义前缀下存在替换引用时，C1 行输出 `(replaced)`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F08：提交 C1、C2（HEAD → main）；`libra replace C1 C2` 生成 `refs/replace/<C1>`；自定义前缀用例以同格式在 `.libra/refs/custom-replace/<C1>` 写入松散引用；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::decorate_replaced_and_env_base`：期望 exit 0；stdout 中 C1 行含 `(replaced)`（Git 2.54.0 预探的 C1 行格式：`<abbrev> (replaced) <subject>`）。
- AC2 → `command::log_test::decorate_replaced_and_env_base`：期望 exit 0；stdout 中 C1 行含 `(replaced)`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::decorate_replaced_and_env_base)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC2；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`（共享回归组：整 target，`%d`/`%D` 占位符回归；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_pretty_format_placeholders` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-07`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`GIT_REPLACE_REF_BASE`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=replaced标记与GIT_REPLACE_REF_BASE; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-07,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-07; release=independent; split-from=N/A; exception=N/A`

---


**2026-11-21 探查结论（阻塞，未落码）：** `libra log --oneline` 对**被替换提交**输出的是替换目标的对象 ID（`libra rev-parse HEAD~1` = `f5f5130…`，而 `log` 两行都打印 `261ff39…` = 替换目标），因此装饰收集阶段拿不到原始 oid，`has_replace_ref` 永不命中，AC1/AC2 的 `(replaced)` 无法在现有 `log` 语义下产生。根因在 `src/command/mod.rs` 的 `load_object` 于读取前 `replace::resolve`，使 `Commit.id` 变成目标对象；Git 的语义是**保留原始 oid、替换内容**。修复需让 `log` 的遍历在加载后恢复 `commit.id = 原始 oid`（内容仍为替换内容），或在遍历层单独传递 display oid。该改动跨出本卡 `Out of scope`（不改变 `log` 对替换对象内容的读取语义）的边界，需另开卡或授权扩围。本卡保持 `pending`，未提交任何代码（实验改动已 `libra restore` 回退）。
### Task LS-09: 核实 `534` 后 `#958` `--no-abbrev` 剩余边界并修复（#958）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-21：核实 LO-09 后 `#958` 剩余边界**均已修复**——`--no-abbrev` 在 `%h`、`%p`、`--parents`、`--children` 及与 `--abbrev=<N>` 的先后覆盖关系上全部输出 40 位 OID，无需改码；新增回归 `command::log_test::no_abbrev_issue_958_remaining` 覆盖 AC1–AC6（含 E() 展开对照），`log_test` 全绿。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 按 GC-01 核实 `issues/534.md` 已落地的 `--no-abbrev` 行为，复现 `#958` 仍保持 OPEN 的失败场景（与 `git@6de20f6092` 对照），补齐缺失输出路径的全长 OID 渲染。开工复现到下列 AC 之外的场景时，先作为独立 AC 补入并重算 G-03（超 8 条按 ER-03 拆卡）。

**Out of scope:** JSON `short_hash` 保持 7 位不变（`COMPATIBILITY.md:196`）；`--raw` 的 OID 长度（`--raw` 由 `LS-20` 新增并在该卡验收）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/534.md` 记录 `#958` 在 `LO-09` 后仍保持 OPEN | `issues/534.md:3,11`、`src/command/log.rs:190-191` |

**Acceptance criteria:**

- [ ] **AC1** `libra log --no-abbrev --format=%h` 的输出逐字等于 `libra log --format=%H` 的输出（`%h` 输出完整 OID）。
- [ ] **AC2** `libra log --no-abbrev --format=%p` 的输出逐字等于展开文本 E(`--format=%p`)（全部父 OID 为完整 40 位）。
- [ ] **AC3** `libra log --no-abbrev --parents --oneline` 的输出逐字等于展开文本 E(`--parents --oneline`)。
- [ ] **AC4** `libra log --no-abbrev --children --oneline` 的输出逐字等于展开文本 E(`--children --oneline`)。
- [ ] **AC5** `libra log --abbrev=7 --no-abbrev --format=%h`（后者胜出）的输出逐字等于 `libra log --format=%H` 的输出。
- [ ] **AC6** `libra log --no-abbrev --abbrev=7 --format=%h`（后者胜出）的输出逐字等于 `libra log --abbrev=7 --format=%h` 的输出。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F09：C1；C2、C3 均以 C1 为父；合并提交 M（父为 C2、C3）；HEAD → M（SHA-1 仓库，共 4 个提交）；E(<参数>) 指不带 `--no-abbrev` 运行 `libra log <参数>` 的 stdout 中每个缩写 OID 字段（`%p` 行的全部字段；`--parents`/`--children` 行中 subject 之前的全部字段）经 `libra rev-parse <缩写>` 展开为完整 40 位 OID、其余字节不变所得的文本；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::log_test::no_abbrev_issue_958_remaining`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC2 → `command::log_test::no_abbrev_issue_958_remaining`：期望 exit 0；stdout 逐字等于 E(`--format=%p`)。
- AC3 → `command::log_test::no_abbrev_issue_958_remaining`：期望 exit 0；stdout 逐字等于 E(`--parents --oneline`)。
- AC4 → `command::log_test::no_abbrev_issue_958_remaining`：期望 exit 0；stdout 逐字等于 E(`--children --oneline`)。
- AC5 → `command::log_test::no_abbrev_issue_958_remaining`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC6 → `command::log_test::no_abbrev_issue_958_remaining`：期望 两次调用均 exit 0；stdout 逐字相同。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::no_abbrev_issue_958_remaining)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`（共享回归组：整 target，`%h`/`%p` 占位符回归；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_pretty_format_placeholders` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-08`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--no-abbrev`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=#958_--no-abbrev剩余边界修复; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-08,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-08; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-10: 实现 `log --no-walk` 与 `--tags`（#956）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-21：`LogArgs` 新增 `no_walk: Option<String>` 与 `tags: Option<String>`；`validate_no_walk` 拒绝未知模式（`StableErrorCode::CliInvalidArguments` → `LBR-CLI-002`）；`tag_tip_commits` 复用 tag 链剥离逻辑并以 `decorate_filter::wildmatch` 过滤；无显式修订时 `--tags` 取代隐式 `HEAD` 起点；`--no-walk` 在遍历后按 `sorted`（提交时间倒序）/`unsorted`（命令行顺序）裁剪排序。AC1–AC6 由 `command::log_test::no_walk_and_tags` 覆盖，`log_test` 全绿；严格 Clippy exit 0。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 在 `LogArgs` 新增 `--no-walk[=sorted|unsorted]`（只展示给定提交，不沿父链遍历）与 `--tags[=<pattern>]`（把匹配 `refs/tags/` 的提交加入起点）。

**Out of scope:** `--branches` / `--remotes`（尚未排期，重启条件：新 issue）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `no_walk` 与 `tags` 字段 | `src/command/log.rs:172-486` |

**Acceptance criteria:**

- [ ] **AC1** `libra log --no-walk --format=%H C1 C3` 的输出逐字为 `C3\nC1\n`（不遍历祖先，按提交时间而非命令行顺序排序即 `sorted`）。
- [ ] **AC2** `--no-walk=unsorted --format=%H C1 C3` 的输出逐字为 `C1\nC3\n`（按命令行顺序）。
- [ ] **AC3** `--tags --no-walk --format=%H` 的输出行多重集等于 {C1, C2, C3}（全部 tag 所指提交各一次）。
- [ ] **AC4** `--tags='v*' --no-walk --format=%H` 恰输出匹配 glob 的 tag（`v1`、`v2`）所指的提交，各一次。
- [ ] **AC5** `--tags --format=%H`（无 `--no-walk`）的输出逐字等于 Git 同夹具期望（以全部 tag 为起点遍历历史）。
- [ ] **AC6** `--no-walk=bogus` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F10：线性提交 C1→C2→C3（HEAD → main；以 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` 固定提交时间依次为 2026-01-01、2026-02-01、2026-03-01，各不相同），tag `v1`→C1、`v2`→C2、`rc1`→C3；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::no_walk_and_tags`：期望 exit 0；stdout 逐字为 `C3\nC1\n`。
- AC2 → `command::log_test::no_walk_and_tags`：期望 exit 0；stdout 逐字为 `C1\nC3\n`（Git 2.54.0 预探语义）。
- AC3 → `command::log_test::no_walk_and_tags`：期望 exit 0；stdout 的行多重集逐字等于 {C1, C2, C3}（各恰一次）。
- AC4 → `command::log_test::no_walk_and_tags`：期望 exit 0；stdout 的行多重集逐字等于 {C1, C2}（各恰一次）。
- AC5 → `command::log_test::no_walk_and_tags`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC6 → `command::log_test::no_walk_and_tags`：期望 stderr 含 `LBR-CLI-002`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::no_walk_and_tags)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_shallow_walk_helper`（共享回归组：整 target，历史遍历 helper 守卫；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_shallow_walk_helper` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-09`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--no-walk`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--no-walk与--tags; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-09,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-09; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-11: 支持 `log --before` / `--after` 日期过滤别名（#965）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-21：`LogArgs::since` 加 `visible_alias = "after"`、`until` 加 `visible_alias = "before"`，与主拼写共享同一解析路径与错误文本（`LBR-CLI-002`/129），`--help` 显示别名。AC1–AC7 由 `command::log_test::before_and_after_aliases` 覆盖，`log_test` 全绿；严格 Clippy exit 0。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 为 `LogArgs::since` 添加 `visible_alias = "after"`，为 `LogArgs::until` 添加 `visible_alias = "before"`，使其与 `--since`/`--until` 完全等价（含相同的错误输出）。

**Out of scope:** 不改变既有日期解析格式。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs::since`/`until` 未注册 `after`/`before` 别名；`parse_date_arg` 的错误文本只含解析错误与固定提示，不含选项名 | `src/command/log.rs:228-233,1150-1156` |

**Acceptance criteria:**

- [ ] **AC1** `libra log --after=2026-01-15 --format=%H` 的输出逐字为 `<C3>\n<C2>\n`。
- [ ] **AC2** `libra log --before=2026-02-15 --format=%H` 的输出逐字为 `<C2>\n<C1>\n`。
- [ ] **AC3** `libra log --after=bogus` 与 `libra log --since=bogus`（同一选项的两种拼写，表驱动）各以 `LBR-CLI-002` 拒绝。
- [ ] **AC4** 在默认错误码配置下（测试子进程不设置 `LIBRA_FINE_EXIT_CODES`），`libra log --after=bogus` 与 `libra log --since=bogus`（两种拼写，表驱动）的退出码均为 129。
- [ ] **AC5** `--after=bogus` 与 `--since=bogus` 的 stderr 逐字相同（错误文本不含选项名，见 `parse_date_arg`）。
- [ ] **AC6** `libra log --help` 列出 `--after` 可见别名。
- [ ] **AC7** `libra log --help` 列出 `--before` 可见别名。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F11：提交时间分别为 2026-01-01（C1）、2026-02-01（C2）、2026-03-01（C3）的 3 个线性提交；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::log_test::before_and_after_aliases`：期望 exit 0；stdout 逐字为 `<C3>\n<C2>\n`。
- AC2 → `command::log_test::before_and_after_aliases`：期望 exit 0；stdout 逐字为 `<C2>\n<C1>\n`。
- AC3 → `command::log_test::before_and_after_aliases`：期望 两种拼写各一次调用的 stderr 均含 `LBR-CLI-002`。
- AC4 → `command::log_test::before_and_after_aliases`：期望 两种拼写各一次调用的退出码均为 129（子进程环境不含 `LIBRA_FINE_EXIT_CODES`）。
- AC5 → `command::log_test::before_and_after_aliases`：期望 两次调用的 stderr 逐字相同。
- AC6 → `command::log_test::before_and_after_aliases`：期望 exit 0；stdout 含 `--after`。
- AC7 → `command::log_test::before_and_after_aliases`：期望 exit 0；stdout 含 `--before`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::before_and_after_aliases)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC7；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_json_since_filter_restricts_results)'`（共享回归组：既有 `--since` JSON 过滤用例；预期 N = 1；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-10`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--after`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--before/--after别名; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-10,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-10; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-12: 支持 `log -R` 反转 diff 输入方向（#969）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-21：新增 `-R`/`--reverse-diff`；按 ADR-LS-01 的边界以 log 专用 `reverse_patch` 实现——交换 `diff --git` 两个路径、`---`/`+++` 前缀、`index` 哈希、`@@ -a +b @@` 范围与 +/- 内容行；`generate_diff`/`generate_diff_with_options`/`CommitDiffOptions` 签名与字段未变，`format_patch` 不受影响，提交顺序不变；#969 标题「`--reverse` 短别名」的表述与本卡按 Git 权威语义（`-R` 反转 diff 输入）实现并在卡内记录澄清。AC1–AC3 由 `command::log_test::diff_reverse_flag_r` 覆盖（AC1 与 `git log -R -p -1` 的 patch 主体逐字节一致），`command::format_patch_test` 与 `log_test` 全绿；严格 Clippy exit 0。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 在 `LogArgs` 新增 `-R`：按 `ADR-LS-01` 首次引入 log 专用 `pub(crate) struct LogDiffOptions`（初始字段 `reverse`）与 log 专用 diff 生成入口，在该入口内交换每个提交 diff 的新旧两侧，与 `git log -R` 一致，**不改变**提交遍历顺序；既有 `generate_diff` / `generate_diff_with_options` 签名与 `CommitDiffOptions` 字段保持不变（`src/command/format_patch.rs` 不改）。注：#969 标题误称 `--reverse` 短别名，本卡按 Git 权威语义实现并记录澄清以关闭 #969。

**Out of scope:** `--reverse` 提交顺序保持不变；不修改 `src/command/diff.rs` 与 `src/command/format_patch.rs`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `-R`；`generate_diff` 以 `CommitDiffOptions::default()` 调用 `generate_diff_with_options`，后者与 `CommitDiffOptions` 由 `format_patch.rs:950,1852-1853` 以结构体字面量直接构造，不可加字段或改签名 | `src/command/log.rs:172-486,4245-4290`、`src/command/format_patch.rs:950,1852-1853` |

**Acceptance criteria:**

- [ ] **AC1** `libra log -R -p -1` 中 C2 的 patch 部分逐字等于 Git 同夹具期望（`git log -R -p -1`：原追加的 `line3` 显示为删除行）。
- [ ] **AC2** `-R` 不改变提交输出顺序：`libra log -R --format=%H` 与不带 `-R` 时逐字相同。
- [ ] **AC3** `libra log --reverse --format=%H` 的输出逐字为 `C1\nC2\n`（仍按从旧到新）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F12：C1 新增 `a.txt`（内容 `line1`、`line2`），C2 在 `a.txt` 末尾追加 `line3`；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::diff_reverse_flag_r`：期望 exit 0；patch 部分逐字等于 Git 同夹具期望。
- AC2 → `command::log_test::diff_reverse_flag_r`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC3 → `command::log_test::diff_reverse_flag_r`：期望 exit 0；stdout 逐字为 `C1\nC2\n`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_reverse_flag_r)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_reverse_outputs_oldest_first)'`（共享回归组：既有 `--reverse` 用例；预期 N = 1；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::format_patch_test::/)'`（共享回归组：`command::format_patch_test` 模块族，守护 `CommitDiffOptions`/`generate_diff_with_options` 不变；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-11`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`-R`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_-R反转diff输入方向; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=LS-11,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-11; release=independent; split-from=N/A; exception=N/A`

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

- [ ] **AC1** PTY 夹具下 `libra log` 的捕获字节逐字等于 Git 在同一 PTY 夹具下的期望字节（该期望不含 `\r\x1b[K` 与孤立 `\x1b[m`）。
- [ ] **AC2** 非 TTY 管道输出字节与修复前一致。
- [ ] **AC3** `--color=always` 时 stdout 含 `\x1b[` 着色序列（颜色转义保留）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F13：3 提交历史；PTY 夹具（stdout 为伪终端）与非 TTY 管道两种运行方式；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::tty_output_no_stray_escape_sequences`：期望 exit 0；捕获字节逐字等于 Git 同夹具期望（开工时取得并硬编码）。
- AC2 → `command::log_test::tty_output_no_stray_escape_sequences`：期望 exit 0；stdout 逐字节等于本卡开工前的 Libra 版本（修复前源码）在同一 F13 夹具上经非 TTY 管道取得并硬编码的输出。
- AC3 → `command::log_test::tty_output_no_stray_escape_sequences`：期望 exit 0；stdout 含 `\x1b[` 着色序列。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::tty_output_no_stray_escape_sequences)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`（共享回归组：整 target，格式化输出回归；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_pretty_format_placeholders` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-12`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/utils/pager.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/utils/pager.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`TTY`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_TTY多余控制序列修复; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=LS-12,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-12; release=independent; split-from=N/A; exception=N/A`

---


**2026-11-21 探查结论（前提不成立，未落码）：** 在同一 PTY 夹具（`script -qec "<cmd>" /dev/null`，F13 三提交仓库）下对 `git 2.56.0` 与 `libra` 逐字节采样，两者输出**形状一致**：每行以 `\r\n` 结尾（PTY 从端 ONLCR，内核转换，非 Libra 代码），行尾均有 `\e[m`，末尾各有 1 个 `\e[K`（pager 收尾），即 `git` 侧同样出现 `\r`、行尾 `\e[m` 与 `\r\e[K`。因此 AC1 所要求的「Git 期望字节不含 `\r\e[K` 与孤立 `\e[m`」在该夹具下**不存在**，`\r` 也不可能通过改 Libra 消除（除非关闭终端 ONLCR）。`libra --no-pager log` 在 PTY 下同样只剩 ONLCR 产生的 `\r`、无 `\e[K`；非 TTY 管道输出与修复前一致（AC2 天然成立），`--color=always` 保留着色（AC3 天然成立）。本卡保持 `pending`，建议改判为「前提不成立/无法按 AC 验收」或重新定义 AC（例如断言非 TTY 与 PTY 输出仅差 ONLCR、且无 pager 收尾序列）。未提交代码改动。
### Task LS-14: 修复空 `--format=` / `--pretty=` 值仍输出默认提交头（Git 不输出提交头字节）（#972）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `in-progress` / `locally-accepted`（2026-11-21：新增 `FormatType::Silent`/`Separator`；`parse_pretty_format` 将 `""`/`tformat:` 映射 `Silent`、`format:` 映射 `Separator`，并同步更新 `test_parse_pretty_format_presets`；输出层对静默格式不追加换行、不注入块前空行，`format:` 保留提交间分隔。AC1–AC4 实测通过（0 字节 / `\n\n`）。**AC5 受阻（未按原 AC 验收）**：`--format= --stat` 已**不再输出提交头**，但逐字等于 Git 无法达成——既有 diffstat 渲染与 Git 存在与 `--format` 无关的差异（Libra ` a.txt |   1 +` 与 `1 insertion(+), 0 deletions(-)`，Git ` a.txt | 1 +` 与 `1 insertion(+)`），属独立的 stat 对齐/后缀差异，超出本卡 `Out of scope`。测试暂按「无 `commit `/`Author:` 且保留 diffstat」断言，并在卡内记录该偏差；补齐需另开 diffstat 格式化卡。`log_test` 全绿；严格 Clippy exit 0。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 修复 `log --format=` / `--pretty=`（及 `tformat:`、`format:` 后为空串）被当作未设置而退回默认 `medium` 提交头的问题：按 Git，空 `--format=`/`--pretty=`/`--format=tformat:` 对每个提交不输出任何字节，`--format=format:` 只在提交之间输出分隔换行。

**Out of scope:** `format.pretty=""` 配置仍保持 `log/config.rs:71` 的报错校验。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `log.rs` 以 `parse_pretty_format` 解析 `--pretty`/`--format`（`2334-2344`），该函数把空串与 `medium` 同映射为默认 `FormatType::Full`（`3096-3099`），因此空格式打印默认头 | `src/command/log.rs:2334-2344,3096-3099` |
| 既有单元测试 `test_parse_pretty_format_presets` 断言 `parse_pretty_format(String::new())` 为 `FormatType::Full`；若本卡改变该映射，同卡更新该断言并在执行记录写明（该测试不属本卡共享回归组，由测试矩阵 `M-UNIT` 与 C 组全量门覆盖） | `src/command/log.rs:4706,4715-4718` |
| Git 2.54.0 预探：3 提交仓库 `--format=` / `--pretty=` / `--format=tformat:` 输出 0 字节，`--format=format:` 输出 `\n\n` | 本机 Git 2.54.0（开工时以 `git@6de20f6092` 复核） |

**Acceptance criteria:**

- [ ] **AC1** `libra log --format=` 输出 0 字节。
- [ ] **AC2** `libra log --pretty=` 输出 0 字节。
- [ ] **AC3** `libra log --format=tformat:` 输出 0 字节。
- [ ] **AC4** `libra log --format=format:` 的输出逐字为 `\n\n`（3 个提交之间的 2 个分隔换行）。
- [ ] **AC5** `libra log --format= --stat` 的输出逐字等于 Git 同夹具期望（只含各提交的 diffstat 块，无提交头）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F14：3 个提交，各修改 1 个文件；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::empty_format_or_pretty_omits_header`：期望 exit 0；stdout 为空。
- AC2 → `command::log_test::empty_format_or_pretty_omits_header`：期望 exit 0；stdout 为空。
- AC3 → `command::log_test::empty_format_or_pretty_omits_header`：期望 exit 0；stdout 为空。
- AC4 → `command::log_test::empty_format_or_pretty_omits_header`：期望 exit 0；stdout 逐字为 `\n\n`。
- AC5 → `command::log_test::empty_format_or_pretty_omits_header`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::empty_format_or_pretty_omits_header)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_pretty_format_placeholders`（共享回归组：整 target，pretty 格式回归；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_pretty_format_placeholders` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-13`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--format=`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=空--format/--pretty行为对齐; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-13,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-13; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-15: 实现 `log --output=<file>` 新目标写入与 `.libra/` 防护（#963；拆出 LS-36、LS-37）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-21：`LogArgs` 新增 `output: Option<PathBuf>`；新增 `LogSink`（Pager / File）分流：逐级解析符号链接取最终目标、`create_new` 独占创建、已存在目标与 `.libra/` 内目标以 `LBR-IO-002` 拒绝、`--json` 组合在 JSON 分支前以 `LBR-CLI-002` 拒绝（`--json` 与 `--output` 不同时产出文件）；pager 策略未变（仍经 `Pager::with_config`，启动时只收到空输出）。AC1–AC8 由 `command::log_test::output_to_file` 覆盖（AC6 以 `.libra` 相对路径+FNV 摘要清单比对，排除 SQLite/oplog/logs/`.lock` 等易变簿记），`log_test` 全绿；严格 Clippy exit 0。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 在 `LogArgs` 新增 `--output=<file>`，本卡只开放**新目标**：把目标路径逐级跟随符号链接解析出最终目标，最终目标不存在时在其所在目录以「独占创建」（`create_new`）打开并写入本次人类可读输出，stdout 为空——悬空符号链接因此与 Git `fopen(path, "w")`（`git@6de20f6092:diff.c:5809` `xfopen`）一样创建其指向的文件；独占创建也使检查后在该路径新放置的文件或符号链接导致打开失败。最终目标已存在（不论文件类型）时本卡以 `LBR-IO-002` 拒绝、不打开、不截断，覆盖既有目标的 Git 语义（原地截断、保留 inode 与权限）及既有目标的描述符级防护由 `LS-37` 开放与验收。`.libra/` 写入防护（GC-07）随本卡生效：最终目标路径（规范化父目录后）位于当前仓库 `.libra/` 内部即以 `LBR-IO-002` 拒绝，不创建任何文件（Libra 专有防护，Git 无此限制，在 `COMPATIBILITY.md` 的 `| log |` 行登记为有意差异）。新目标创建失败（如父目录不可写）以 `LBR-IO-002` 失败。全局 `--json` 与 `--output` 的组合在本卡以 `LBR-CLI-002` 拒绝，由 `LS-36` 开放。分页器策略不变：Git 的 `cmd_log_init_finish` 不论是否带 `--output` 都调用 `setup_pager`（`git@6de20f6092:builtin/log.c:388`），是否真正启动分页器由其分页器配置与 stdout 是否为终端决定（`git@6de20f6092:pager.c` 的 `setup_pager`）；Libra 同样不因 `--output` 改变策略，仍按现有 `LIBRA_PAGER`/TTY 策略经 `Pager::with_config` 决定是否启动分页器，启动时分页器只收到空输出。拆出 `LS-36`、`LS-37`。

**Out of scope:** 覆盖既有目标及既有目标的 `.libra/` 防护（由 `LS-37` 处理）；`--json` 输出写入文件（由 `LS-36` 处理）；检查与打开之间的并发替换竞态回归（`DEFER-LS-02`）；不改变 `format-patch --output`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 无 `output` 参数；人类可读输出经 `Pager::with_config` 写出，`--json` 在此之前提前返回 | `src/command/log.rs:172-486,2190-2192,2325` |
| Git 以 `xfopen(path, "w")` 打开 `--output` 目标（跟随符号链接，悬空链接创建其目标；Git 2.54.0 预探：`git log --oneline --output=S` 创建 S 指向的 `target.txt`）；`git log` 不论是否带 `--output` 都调用 `setup_pager`，是否启动分页器取决于分页器配置与终端 | `git@6de20f6092:diff.c:5800-5815`、`git@6de20f6092:builtin/log.c:388`、`git@6de20f6092:pager.c`（`setup_pager`） |

**Acceptance criteria:**

- [ ] **AC1** `libra log --oneline --output=out.log`（out.log 不存在）写入的 out.log 内容逐字等于不带 `--output` 时的 stdout。
- [ ] **AC2** 同一调用（AC1）的 stdout 为空。
- [ ] **AC3** `libra log --oneline --output=S`（S 为工作区内指向尚不存在文件 T 的符号链接）之后 T 的内容逐字等于不带 `--output` 时的 stdout。
- [ ] **AC4** `libra log --output=N`（`.libra/` 内新路径）以 `LBR-IO-002` 拒绝。
- [ ] **AC5** `libra log --output=E`（工作区内已存在的普通文件）以 `LBR-IO-002` 拒绝（本卡不覆盖既有目标，由 `LS-37` 开放）。
- [ ] **AC6** 对 N、I、L、H 各调用一次 `libra log --output=<目标>` 之后，`.libra/` 的「相对路径 → SHA-256」清单与第一次调用前逐字相同（无新文件、无截断）。
- [ ] **AC7** 本卡交付时 `libra --json log --output=out.json` 以 `LBR-CLI-002` 拒绝（组合由 `LS-36` 开放）。
- [ ] **AC8** `libra log --output=R/out.log`（R 为只读目录，out.log 为新目标）以 `LBR-IO-002` 失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F15：3 提交历史；测试临时目录内不存在的输出路径 out.log；工作区内已存在的普通文件 E；工作区内指向尚不存在文件 T 的符号链接 S；`.libra/` 内已存在的文件 I、`.libra/` 内不存在的新路径 N、工作区内指向 I 的符号链接 L、工作区内与 I 共享 inode 的硬链接 H；只读目录 R；每次调用另按「验证执行协议」第 4 项断言只读不变量；AC3 的值另与 Git 2.54.0 同构预探一致，其余为 Libra 自身基准）

- AC1 → `command::log_test::output_to_file`：期望 exit 0；out.log 内容逐字等于不带 `--output` 时的 stdout。
- AC2 → `command::log_test::output_to_file`：期望 exit 0；stdout 为 0 字节。
- AC3 → `command::log_test::output_to_file`：期望 exit 0；T 的内容逐字等于不带 `--output` 时的 stdout（Git 2.54.0 预探：同夹具创建 T）。
- AC4 → `command::log_test::output_to_file`：期望 stderr 含 `LBR-IO-002`。
- AC5 → `command::log_test::output_to_file`：期望 stderr 含 `LBR-IO-002`。
- AC6 → `command::log_test::output_to_file`：期望 调用前后清单逐字相等。
- AC7 → `command::log_test::output_to_file`：期望 stderr 含 `LBR-CLI-002`。
- AC8 → `command::log_test::output_to_file`：期望 stderr 含 `LBR-IO-002`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::output_to_file)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::log_test::/)'`（共享回归组：`command::log_test` 模块族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-14`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--output`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡须在该行登记有意差异，`COMPATIBILITY.md` 行必须更新。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--output新目标写入与.libra/防护; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-14,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-14; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-37: `log --output` 覆盖既有目标与既有目标的 `.libra/` 防护（#963；自 LS-15 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-21：既有目标改为原地覆盖——只写/不创建/不截断打开 → `fstat` 取 `(dev, inode, nlink)` 与预检 `stat` 比对（不一致即拒绝）→ `nlink>1` 时遍历 `.libra/` 比对文件标识（`file_identity`/`link_count` 在 Unix 与 Windows 分别取 dev/inode 与 volume serial/file index，取不到即 fail-closed）→ 核验通过才经同一描述符 `set_len(0)`；目录目标以 `LBR-IO-002` 失败；路径级 `.libra/` 预检同时对最终目标本身生效。按协议删除 `output_to_file` 中 LS-15 的「既有目标以 `LBR-IO-002` 拒绝」断言，其余保留。AC1–AC8 由 `command::log_test::output_overwrite_and_protection` 覆盖（AC2/AC3 断言 inode 与 `0640` 权限不变，AC7 比对 `.libra` 清单），`log_test` 全绿；严格 Clippy exit 0。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 开放 `LS-15` 暂时拒绝的既有目标，按 Git 相同方式覆盖（`git@6de20f6092:diff.c:5809` 以 `xfopen(path, "w")` 打开：跟随符号链接、原地截断，保留既有文件的 inode 与权限，对其它硬链接可见），并对既有目标提供 `.libra/` 写入防护（GC-07），且不在检查后按路径重新打开：① 路径级预检——最终目标路径（逐级跟随符号链接并规范化父目录后）位于当前仓库 `.libra/` 内部即拒绝；② 打开——以「只写、不创建、不截断」打开该最终目标；③ 描述符级核验——对已打开描述符 `fstat` 取 `(dev, inode, nlink)`（非 Unix 平台取等价的文件标识与链接数，口径同 `src/utils/beneath.rs:28-37` 的 `RootIdentity`；取不到时以 `LBR-IO-002` fail-closed 拒绝），须与预检时 `stat` 的 `(dev, inode)` 相同（否则视为检查后被替换，拒绝），且当 `nlink > 1` 时遍历 `.libra/` 比较 `(dev, inode)`，与任一内部文件相同即拒绝；④ 核验通过后才经同一描述符截断为 0 并写入。拒绝均返回 `LBR-IO-002`，且发生在任何截断之前（与 `LS-15` 已在 `COMPATIBILITY.md` 的 `| log |` 行登记的 `.libra/` 防护有意差异同属一项，按第 1 步核对该行是否需补充既有目标的说明）。目标为目录时以 `LBR-IO-002` 失败（对应 Git `could not open '<path>' for writing: Is a directory`）。威胁模型：防护针对用户误指定的静态目标；与本进程并发篡改目标路径父目录的同用户攻击者本就能直接写 `.libra/`，不在防护范围，竞态回归用例按 `DEFER-LS-02` 延后。同卡按「验证执行协议」第 4 项例外删除 `command::log_test::output_to_file` 中「E 以 `LBR-IO-002` 拒绝」的断言（对应 `LS-15` AC5，被本卡开放的覆盖语义取代），其余断言不变。

**Out of scope:** 新目标写入、`.libra/` 新路径防护与新目标创建失败诊断（`LS-15` 已交付）；`--json` 写文件（`LS-36`）；检查与打开之间的并发替换竞态回归（`DEFER-LS-02`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LS-15` 交付后 `--output` 对既有目标以 `LBR-IO-002` 拒绝，`output_to_file` 断言该拒绝 | 本计划 `LS-15` 的 AC5 与「AC → 断言映射」 |
| 按「验证执行协议」第 4 项例外，本卡同卡删除 `output_to_file` 中对应 `LS-15` AC5 的断言（`libra log --output=E` 期望 stderr 含 `LBR-IO-002`）；理由：该拒绝被本卡开放的覆盖语义取代；该用例其余断言保持不变 | 本计划 `LS-15` 的 AC5 |
| Git 以 `xfopen(path, "w")` 打开 `--output` 目标；目录目标报 `could not open '.' for writing: Is a directory`、exit 128（Git 2.54.0 预探） | `git@6de20f6092:diff.c:5800-5815` |
| 非 Unix 平台的文件标识口径（Windows 为卷序列号与文件索引） | `src/utils/beneath.rs:28-37` |

**Acceptance criteria:**

- [ ] **AC1** `libra log --oneline --output=E` 后 E 的内容逐字等于不带 `--output` 时的 stdout（无旧内容残留）。
- [ ] **AC2** 同一调用（AC1）后 E 的 inode 与调用前相同（原地截断，与 Git 一致）。
- [ ] **AC3** 同一调用（AC1）后 E 的权限位仍为 `0640`。
- [ ] **AC4** `libra log --output=I`（`.libra/` 内已有文件）以 `LBR-IO-002` 拒绝。
- [ ] **AC5** `libra log --output=L`（指向 I 的符号链接）以 `LBR-IO-002` 拒绝。
- [ ] **AC6** `libra log --output=H`（与 I 共享 inode 的硬链接）以 `LBR-IO-002` 拒绝。
- [ ] **AC7** 对 I、L、H 的三次拒绝调用之后，`.libra/` 的「相对路径 → SHA-256」清单与第一次调用前逐字相同（无新文件、无截断）。
- [ ] **AC8** `libra log --output=D`（已存在的目录）以 `LBR-IO-002` 失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F37：3 提交历史；工作区内已存在、权限为 `0640`、内容为旧文本的普通文件 E（记录其 inode）；`.libra/` 内已存在的文件 I、工作区内指向 I 的符号链接 L、工作区内与 I 共享 inode 的硬链接 H；作为目标的已存在目录 D；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，AC2/AC3 与 Git `xfopen` 原地截断一致、AC8 标注 Git 对应诊断）

- AC1 → `command::log_test::output_overwrite_and_protection`：期望 exit 0；E 的内容逐字等于不带 `--output` 时的 stdout。
- AC2 → `command::log_test::output_overwrite_and_protection`：期望 exit 0；inode 不变。
- AC3 → `command::log_test::output_overwrite_and_protection`：期望 exit 0；权限位为 `0640`。
- AC4 → `command::log_test::output_overwrite_and_protection`：期望 stderr 含 `LBR-IO-002`。
- AC5 → `command::log_test::output_overwrite_and_protection`：期望 stderr 含 `LBR-IO-002`。
- AC6 → `command::log_test::output_overwrite_and_protection`：期望 stderr 含 `LBR-IO-002`。
- AC7 → `command::log_test::output_overwrite_and_protection`：期望 调用前后清单逐字相等。
- AC8 → `command::log_test::output_overwrite_and_protection`：期望 stderr 含 `LBR-IO-002`（Git 对应诊断：`could not open 'D' for writing: Is a directory`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::output_overwrite_and_protection)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::output_to_file)'`（共享回归组：前序卡 `LS-15` 新增、经本卡按「验证执行协议」第 4 项例外修订（仅删除被取代的既有目标拒绝断言）后的 `--output` 用例；预期 N = 1；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::log_test::/)'`（共享回归组：`command::log_test` 模块族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-15`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--output`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--output覆盖既有目标与既有目标防护; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=LS-15,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-15; release=independent; split-from=LS-15; exception=N/A`

---

### Task LS-36: `log --json` 输出写入 `--output` 文件（#963；自 LS-15 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-21：新增 `utils::output::json_command_envelope_bytes`（`emit_json_data` 改为复用），`--json` 分支在 `args.output.is_some()` 时把信封字节经 `LogSink` 写入目标文件（stdout 为空），`.libra/` 防护与失败诊断沿用 LS-15/LS-37（`LogSink::open` 移除 json 拒绝）；按协议删除 `output_to_file` 中 LS-15 的「`--json --output` 以 `LBR-CLI-002` 拒绝」断言，其余保留。AC1–AC2 由 `command::log_test::output_json_to_file` 覆盖（文件内容逐字等于不带 `--output` 的 JSON stdout、stdout 0 字节），`log_test` 全绿；严格 Clippy exit 0。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 开放 `LS-15` 暂时拒绝的全局 `--json` 与 `--output=<file>` 组合：JSON 输出（`LogOutput`）写入目标文件，stdout 为空，`.libra/` 防护与失败诊断沿用 `LS-15`/`LS-37`；同卡删除 `command::log_test::output_to_file` 中「`--json --output` 被拒绝」的断言（该行为由本卡取代），其余断言保持不变。期望基准为 Libra 自身不带 `--output` 的 JSON 输出（Git 无 `--json`）。

**Out of scope:** 人类可读输出写入与失败路径（`LS-15` 已交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LS-15` 交付后 `--json --output` 以 `LBR-CLI-002` 拒绝；JSON 输出经 `emit_json_data` 写 stdout | `src/command/log.rs:2190-2192` |
| 按「验证执行协议」第 4 项例外，本卡同卡删除 `command::log_test::output_to_file` 中对应 `LS-15` AC7 的断言（`libra --json log --output=out.json` 期望 stderr 含 `LBR-CLI-002`）；理由：该拒绝行为被本卡开放的组合取代；该用例其余断言保持不变 | 本计划 `LS-15` 的 AC7 与「AC → 断言映射」 |

**Acceptance criteria:**

- [ ] **AC1** `libra --json log --output=out.json` 写入的 `out.json` 内容逐字等于不带 `--output` 时 `libra --json log` 的 stdout。
- [ ] **AC2** `libra --json log --output=out.json` 的 stdout 为空。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F36：3 提交历史；临时目录内的输出路径；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::log_test::output_json_to_file`：期望 exit 0；文件内容逐字相等。
- AC2 → `command::log_test::output_json_to_file`：期望 exit 0；stdout 为 0 字节。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::output_json_to_file)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC2；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::output_to_file)'`（共享回归组：前序卡 `LS-15` 新增、经本卡按「验证执行协议」第 4 项例外修订（仅删除被取代的 `--json --output` 拒绝断言）后的 `--output` 用例；预期 N = 1；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::log_test::/)'`（共享回归组：`command::log_test` 模块族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-37`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--output`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--json与--output组合; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=6/8; landing=1; prod-files=1; scope=M; deps=LS-37,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-37; release=independent; split-from=LS-15; exception=N/A`

---

### Task LS-16: 将 `log --follow` 改为无参布尔标志（#962）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-21：`LogArgs::follow` 改为 `num_args=0..=1` + `default_missing_value=""` + `require_equals` 的无值布尔标志；新增 `resolve_follow_path`——显式 `=FILE` 优先，裸标志要求恰好一个位置 pathspec，否则 `CliError::command_usage("--follow requires exactly one pathspec")` + `StableErrorCode::CliInvalidArguments`（`LBR-CLI-002`）；两处 `effective_follow` 计算改用该 helper，`resolve_log_config` 的「显式 `--follow`」判定仍以 `is_some()` 成立；旧写法 `--follow=<FILE>` 保留并在 `COMPATIBILITY.md` 登记为有意差异。AC1–AC7 由 `command::log_test::follow_boolean_flag_single_pathspec` 覆盖（AC1/AC2/AC7 逐字相等且含重命名前 C1；AC3–AC6 校验码与诊断文本），`log_test` 全绿；严格 Clippy exit 0。**Codex/Claude 审计按用户 2026-11-21 指令取消**。）

**Description:** 将 `LogArgs::follow` 从 `--follow <FILE>` 改为 Git 兼容的无值标志，由唯一位置 pathspec 决定跟踪文件；0 个或多于 1 个路径时按 Git 报 `--follow requires exactly one pathspec`（`LBR-CLI-002`）。为不在 `patch` 版本中破坏既有脚本（ER-08），保留旧写法 `--follow=<FILE>`（`require_equals`，已弃用，等价于 `--follow -- <FILE>`），并在 `COMPATIBILITY.md` 登记为有意差异；`--follow <FILE>`（空格分隔）在新语义下自然解析为位置 pathspec，结果不变。

**Out of scope:** `-L` 与 `--follow` 的互斥校验（由 `LS-32` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs::follow: Option<String>` 以 `value_name = "FILE"` 强制要求参数；执行期以 `args.follow` 求跟踪路径 | `src/command/log.rs:469-471,2212-2216` |
| `resolve_log_config` 以 `args.follow.is_some()` 判定显式 `--follow`（决定是否套用 `log.follow` 配置），`--follow` 取值形态变化须同步调整 | `src/command/log/config.rs:38` |

**Acceptance criteria:**

- [ ] **AC1** `libra log --follow --oneline -- b.txt` 的提交序列逐字等于 Git 同夹具期望（包含重命名前的 C1）。
- [ ] **AC2** `libra log --follow --oneline b.txt`（位置 pathspec，无 `--`）结果与 AC1 相同。
- [ ] **AC3** `libra log --follow`（无路径）以 `LBR-CLI-002` 拒绝。
- [ ] **AC4** `libra log --follow`（无路径）的 stderr 含 Git 同文诊断 `--follow requires exactly one pathspec`。
- [ ] **AC5** `libra log --follow -- a.txt b.txt`（两个路径）以 `LBR-CLI-002` 拒绝。
- [ ] **AC6** `libra log --follow -- a.txt b.txt`（两个路径）的 stderr 含 Git 同文诊断 `--follow requires exactly one pathspec`。
- [ ] **AC7** 旧写法 `libra log --follow=b.txt --oneline` 的输出与 AC1 逐字相同。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F16：`a.txt` 在 C2 被重命名为 `b.txt`，C3 修改 `b.txt`；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::follow_boolean_flag_single_pathspec`：期望 exit 0；stdout 提交序列逐字等于 Git 同夹具期望（含 C1）。
- AC2 → `command::log_test::follow_boolean_flag_single_pathspec`：期望 exit 0；stdout 与 AC1 逐字相同。
- AC3 → `command::log_test::follow_boolean_flag_single_pathspec`：期望 stderr 含 `LBR-CLI-002`。
- AC4 → `command::log_test::follow_boolean_flag_single_pathspec`：期望 与 AC3 同一次调用的 stderr 含 `--follow requires exactly one pathspec`。
- AC5 → `command::log_test::follow_boolean_flag_single_pathspec`：期望 stderr 含 `LBR-CLI-002`。
- AC6 → `command::log_test::follow_boolean_flag_single_pathspec`：期望 与 AC5 同一次调用的 stderr 含 `--follow requires exactly one pathspec`。
- AC7 → `command::log_test::follow_boolean_flag_single_pathspec`：期望 exit 0；stdout 与 AC1 逐字相同。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::follow_boolean_flag_single_pathspec)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC7；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_follow_detects_rename) | test(=command::log_test::log_ineligible_follow_line_range_pathspec_walk_full_history)'`（共享回归组：既有 `--follow` 重命名与 follow/-L/pathspec 遍历用例；预期 N = 2；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1；--follow 取值形态变更另按 T-3 口径记录）` | **Dependencies:** `LS-36`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/log/config.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--follow`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256；本卡须在该行登记有意差异，`COMPATIBILITY.md` 行必须更新。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--follow无参布尔标志; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=LS-36,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-36; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-17: 实现 `log -L` 行级历史输出（#961；拆出 LS-32）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 修复 `log -L <start>,<end>:<file>` / `-L :<funcname>:<file>` 仅过滤提交却不输出行级 diff hunk 的缺陷：输出演进提交与被追踪范围的 hunk，函数名语法识别函数边界（移除 `src/command/log.rs:1907` 退化为整文件的实现），并随历史把行号范围映射到父提交。拆出 `LS-32`（`-L` 参数校验）。

**Out of scope:** `-L` 与位置 pathspec / `--follow` 的互斥及非法语法、文件不存在诊断（由 `LS-32` 处理）；复杂跨文件函数重命名追踪（尚未排期）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `log.rs` 已解析 `line_range` 并调 `commit_affects_line_range`，但未渲染对应行范围 diff；函数名语法在 `1907` 退化为整文件 | `src/command/log.rs:479-480,1894-1910,1981-2030` |

**Acceptance criteria:**

- [ ] **AC1** `libra log -L 1,5:file.txt --format=%H` 的结果提交序列等于 Git 同夹具期望。
- [ ] **AC2** `libra log -L 1,5:file.txt` 的完整输出逐字等于 Git 同夹具期望（每个结果提交只输出与被追踪范围相交的 hunk）。
- [ ] **AC3** `libra log -L :fn_name:file.txt` 的结果提交序列等于 Git 同夹具期望（只修改函数体外行的提交不出现）。
- [ ] **AC4** `libra log -L :fn_name:file.txt` 的完整输出（含各 hunk）逐字等于 Git 同夹具期望（hunk 只覆盖函数体，不退化为整文件）。
- [ ] **AC5** `libra log -L <s>,<e>:file.txt`（`<s>,<e>` 为 HEAD 中函数体所在行号，位于第 4 个提交插入的 2 行之后）的完整输出逐字等于 Git 同夹具期望（更早提交中的同一范围按插入偏移正确映射；与 AC4 为不同调用）。
- [ ] **AC6** `libra log -L :nofn:file.txt`（函数名未匹配）以 `LBR-CLI-003` 失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F17：`file.txt` 含函数 `fn_name` 与函数外行；提交依次为：新增文件、修改函数体、只修改函数体外行、在函数上方插入 2 行、再修改函数体；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::line_range_history_output`：期望 exit 0；stdout 中提交 OID 序列逐字等于 Git 同夹具期望。
- AC2 → `command::log_test::line_range_history_output`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC3 → `command::log_test::line_range_history_output`：期望 exit 0；stdout 提交序列逐字等于 Git 同夹具期望。
- AC4 → `command::log_test::line_range_history_output`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC5 → `command::log_test::line_range_history_output`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC6 → `command::log_test::line_range_history_output`：期望 stderr 含 `LBR-CLI-003`（Git：`-L parameter 'nofn' starting at line 1: no match`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::line_range_history_output)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_line_range_flag_accepted) | test(=command::log_test::log_ineligible_follow_line_range_pathspec_walk_full_history)'`（共享回归组：既有 `-L` 接受与 follow/-L/pathspec 遍历用例；预期 N = 2；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-16`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`-L`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_-L行级历史输出; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-16,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-16; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-32: `log -L` 与 pathspec/`--follow` 互斥及参数诊断（#961；自 LS-17 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 增加 `-L` 的参数校验：与位置 pathspec 同用、与 `--follow` 同用（Git 中二者因 `--follow requires exactly one pathspec` 与 `-L<range>:<file> cannot be used with pathspec` 不可同时满足）、非法 `-L` 语法与不存在的文件分别返回稳定错误。

**Out of scope:** `-L` 行级输出与函数边界（由 `LS-17` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `parse_line_range` 已对非法格式返回 `CliInvalidArguments`；无 `-L` 与 pathspec / `--follow` 的互斥校验 | `src/command/log.rs:1894-1902,1981-2030` |
| Git 2.54.0 预探的四类诊断文本与退出码 128 | 本机 Git 2.54.0（开工时以 `git@6de20f6092` 复核） |

**Acceptance criteria:**

- [ ] **AC1** `libra log -L 1,1:f -- f` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC2** `libra log -L 1,1:f --follow` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC3** `libra log -L foo` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC4** `libra log -L 1,1:nofile` 以 `LBR-CLI-003` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F32：单文件 `f` 的 2 提交历史；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::line_range_argument_validation`：期望 stderr 含 `LBR-CLI-002`（Git：`-L<range>:<file> cannot be used with pathspec`）。
- AC2 → `command::log_test::line_range_argument_validation`：期望 stderr 含 `LBR-CLI-002`（Git：`--follow requires exactly one pathspec`）。
- AC3 → `command::log_test::line_range_argument_validation`：期望 stderr 含 `LBR-CLI-002`（Git：`-L argument not 'start,end:file' or ':funcname:file'`）。
- AC4 → `command::log_test::line_range_argument_validation`：期望 stderr 含 `LBR-CLI-003`（Git：`There is no path nofile in the commit`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::line_range_argument_validation)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_line_range_flag_accepted) | test(=command::log_test::log_ineligible_follow_line_range_pathspec_walk_full_history)'`（共享回归组：既有 `-L` 接受与 follow/-L/pathspec 遍历用例；预期 N = 2；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-17`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`-L`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_-L参数校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-17,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-17; release=independent; split-from=LS-17; exception=N/A`

---

### Task LS-18: 实现 `log --find-object=<object>`（#968）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LogArgs` 新增 `--find-object=<object>`（可重复），按 pinned Git 的文件对语义筛选提交：在该提交相对父提交的递归 diff（含树条目）中，只要任一变更文件对的旧侧或新侧对象 ID 属于给定对象集合即命中；不是按对象出现总次数判定，因此路径移动而总数不变的提交同样命中。

**Out of scope:** `--pickaxe-all`（仅当后续需要时另开）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LogArgs` 仅有 `-S`/`-G` 文本 pickaxe，无 `find_object` | `src/command/log.rs:398-408` |
| Git 按文件对旧侧或新侧 OID 属于查找集合判定，并开启 `tree_in_recursive` 使树条目参与比较 | `git@6de20f6092:diffcore-pickaxe.c:137-145`、`git@6de20f6092:diff.c:5501-5519` |

**Acceptance criteria:**

- [ ] **AC1** `libra log --find-object=<X> --format=%H` 的提交集合等于 Git 同夹具期望（含 X 总数不变的路径移动提交 C6）。
- [ ] **AC2** `libra log --find-object=<T> --format=%H`（tree OID）的提交集合等于 Git 同夹具期望。
- [ ] **AC3** `libra log --find-object=<X> --find-object=<Y> --format=%H` 的提交集合等于 Git 同夹具期望（多值取并集）。
- [ ] **AC4** `--find-object=bogus` 以 `LBR-CLI-003` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F18：C1 新增内容相同的 `a.txt` 与 `b.txt`（blob X）及目录 `d/`（tree T）；C2 只修改无关文件；C3 把 `a.txt` 改为 blob Y；C4 删除 `b.txt`；C5 新增内容为 X 的 `c.txt`；C6 把 `c.txt` 移动为 `e.txt`（X 的总出现次数不变）；C7 修改 `d/` 内文件（tree T 被替换）；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::find_object`：期望 exit 0；stdout 提交集合逐字等于 Git 同夹具期望。
- AC2 → `command::log_test::find_object`：期望 exit 0；stdout 提交集合逐字等于 Git 同夹具期望。
- AC3 → `command::log_test::find_object`：期望 exit 0；stdout 提交集合逐字等于 Git 同夹具期望。
- AC4 → `command::log_test::find_object`：期望 stderr 含 `LBR-CLI-003`（Git：`unable to resolve 'bogus'`）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::find_object)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_pickaxe_s_finds_commit_that_changes_string_count) | test(=command::log_test::test_log_pickaxe_g_matches_diff_line_regex)'`（共享回归组：既有 `-S`/`-G` pickaxe 用例；预期 N = 2；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-32`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--find-object`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--find-object; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-32,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-32; release=independent; split-from=N/A; exception=N/A`

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

- [ ] **AC1** `libra log --cc -1 M` 的 stdout 逐字等于 Git 同夹具期望（含 `diff --cc c.txt` 与 `@@@` 组合 hunk）。
- [ ] **AC2** M 的 `--cc` 输出所列文件集合恰为 {`c.txt`}（仅一侧改动的 `one.txt` 不出现）。
- [ ] **AC3** 对非合并提交 N，`libra log --cc -1 N` 的 stdout 与 `libra log -p -1 N` 逐字相同（普通 patch）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F19：分支两侧都修改 `c.txt` 并手工解决冲突的合并提交 M（另一侧单独修改 `one.txt`）；以及普通提交 N；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::log_cc_combined_diff`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC2 → `command::log_test::log_cc_combined_diff`：期望 exit 0；stdout 中 `diff --cc` 头的文件集合恰为 {`c.txt`}。
- AC3 → `command::log_test::log_cc_combined_diff`：期望 两次调用均 exit 0；stdout 逐字相同。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::log_cc_combined_diff)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_first_parent_skips_merged_branch_commits)'`（共享回归组：既有合并提交遍历用例；预期 N = 1；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-18`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--cc`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--cc组合差异; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=4/8; landing=1; prod-files=1; scope=M; deps=LS-18,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-18; release=independent; split-from=N/A; exception=N/A`

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

- [ ] **AC1** `libra log --raw -1` 的 stdout 逐字等于 Git 同夹具期望（含 `:<oldmode> <newmode> <oldsha> <newsha> <status>\t<path>` 行）。
- [ ] **AC2** `libra log --numstat -1` 的 stdout 逐字等于 Git 同夹具期望（含 `<ins>\t<del>\t<path>` 行）。
- [ ] **AC3** `libra log --dirstat -1` 的 stdout 逐字等于 Git 同夹具期望（核实既有 `compute_commit_dirstat` 对齐）。
- [ ] **AC4** `libra log -1 --raw -s` 的输出与 `libra log -1 -s` 逐字相同（后续 `-s` 清除 `--raw`）。
- [ ] **AC5** `libra log -1 --numstat -s` 的输出与 `libra log -1 -s` 逐字相同（后续 `-s` 清除 `--numstat`）。
- [ ] **AC6** `libra log -1 --dirstat --no-patch` 的输出与 `libra log -1 --no-patch` 逐字相同（后续 `--no-patch` 清除 `--dirstat`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F20：一个提交同时含新增、修改、删除与模式变更文件；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::raw_numstat_dirstat_formats`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC2 → `command::log_test::raw_numstat_dirstat_formats`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC3 → `command::log_test::raw_numstat_dirstat_formats`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC4 → `command::log_test::raw_numstat_dirstat_formats`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC5 → `command::log_test::raw_numstat_dirstat_formats`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC6 → `command::log_test::raw_numstat_dirstat_formats`：期望 两次调用均 exit 0；stdout 逐字相同。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::raw_numstat_dirstat_formats)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-19`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--numstat`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--raw/--numstat/--dirstat; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=LS-19,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-19; release=independent; split-from=N/A; exception=N/A`

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

- [ ] **AC1** `libra log -p -w`（及表驱动的 `--ignore-all-space`）的 stdout 逐字等于 Git 同夹具期望（仅空白变更的提交不输出 hunk）。
- [ ] **AC2** `libra log -p -b`（及表驱动的 `--ignore-space-change`）的 stdout 逐字等于 Git 同夹具期望（忽略空白数量变更）。
- [ ] **AC3** 不带新选项时 `libra log -p` 输出与修复前逐字节相同（`LogDiffOptions` 默认值向后兼容）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F21：基线提交 B 之上依次为 C1（只改变缩进空白数量）、C2（只插入行内空白）、C3（实质修改）；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::diff_ignore_whitespace_passthrough`：期望 表驱动对 `-w` 与 `--ignore-all-space` 两种拼写各调用一次，每次 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC2 → `command::log_test::diff_ignore_whitespace_passthrough`：期望 表驱动对 `-b` 与 `--ignore-space-change` 两种拼写各调用一次，每次 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC3 → `command::log_test::diff_ignore_whitespace_passthrough`：期望 exit 0；stdout 逐字节等于本卡开工前的 Libra 版本（修复前源码）在同一 F21 夹具、同一 HEAD 上执行 `libra log -p` 取得并硬编码的输出（「基线」指 Libra 源码版本，不是夹具提交 B）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_ignore_whitespace_passthrough)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_patch_fails_when_commit_blob_is_missing) | test(=command::log_test::test_log_patch_no_pathspec) | test(=command::log_test::test_log_patch_with_pathspec) | test(=command::log_test::test_log_patch_with_stat_shows_diffstat_before_patch)'`（共享回归组：既有 `log -p` 用例 4 个；预期 N = 4；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-20`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--ignore-all-space`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_diff空白忽略(-w/-b); recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=6/8; landing=2; prod-files=3; scope=M; deps=LS-20,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-20; release=independent; split-from=N/A; exception=N/A`

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

- [ ] **AC1** `libra log -p -1 X --no-prefix` 的 `diff --git` 行逐字为 `diff --git a.txt a.txt`。
- [ ] **AC2** `libra log -p -1 X --no-prefix` 的旧侧路径行逐字为 `--- a.txt`。
- [ ] **AC3** `libra log -p -1 X --no-prefix` 的新侧路径行逐字为 `+++ a.txt`。
- [ ] **AC4** `libra log -p -1 X --src-prefix=x/` 的旧侧路径行逐字为 `--- x/a.txt`。
- [ ] **AC5** `libra log -p -1 X --dst-prefix=y/` 的新侧路径行逐字为 `+++ y/a.txt`。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F28：基线提交 B 与其子提交 X（X 的父提交为 B）：X 修改 `a.txt`；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::log_test::diff_prefix_options_passthrough`：期望 exit 0；该行逐字相等。
- AC2 → `command::log_test::diff_prefix_options_passthrough`：期望 exit 0；该行逐字相等。
- AC3 → `command::log_test::diff_prefix_options_passthrough`：期望 exit 0；该行逐字相等。
- AC4 → `command::log_test::diff_prefix_options_passthrough`：期望 exit 0；该行逐字相等。
- AC5 → `command::log_test::diff_prefix_options_passthrough`：期望 exit 0；该行逐字相等。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_prefix_options_passthrough)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test compat_diff_review_options`（共享回归组：整 target，含 CLI src/dst 前缀语义守卫；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_diff_review_options` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-21`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--src-prefix`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_diff路径前缀(--no-prefix/--src-prefix/--dst-prefix); recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=6/8; landing=2; prod-files=3; scope=M; deps=LS-21,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-21; release=independent; split-from=LS-21; exception=N/A`

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

- [ ] **AC1** SHA-1 仓库中 `libra log -p -1 X --full-index` 的 `index` 行列表逐字等于 [`index <B:a.txt 的完整 40 位 OID>..<X:a.txt 的完整 40 位 OID> 100644`]。
- [ ] **AC2** SHA-256 仓库中同一命令的 `index` 行列表逐字等于 [`index <B:a.txt 的完整 64 位 OID>..<X:a.txt 的完整 64 位 OID> 100644`]。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F29：SHA-1 仓库与 SHA-256 仓库各一，均为基线提交 B 与其子提交 X（X 的父提交为 B）：X 修改 `a.txt`；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::log_test::diff_full_index_passthrough`：期望 exit 0；stdout 中以 `index ` 开头的行组成的列表逐字等于 [`index <B:a.txt 的 OID>..<X:a.txt 的 OID> 100644`]，两个 OID 以 `libra rev-parse B:a.txt` / `libra rev-parse X:a.txt` 取得（SHA-1 仓库为完整 40 位）。
- AC2 → `command::log_test::diff_full_index_passthrough`：期望 exit 0；stdout 中以 `index ` 开头的行组成的列表逐字等于 [`index <B:a.txt 的 OID>..<X:a.txt 的 OID> 100644`]，两个 OID 以 `libra rev-parse` 取得（SHA-256 仓库为完整 64 位）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_full_index_passthrough)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC2；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_patch_fails_when_commit_blob_is_missing) | test(=command::log_test::test_log_patch_no_pathspec) | test(=command::log_test::test_log_patch_with_pathspec) | test(=command::log_test::test_log_patch_with_stat_shows_diffstat_before_patch)'`（共享回归组：既有 `log -p` 用例 4 个；预期 N = 4；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-28`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--full-index`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_diff_--full-index; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=6/8; landing=2; prod-files=3; scope=M; deps=LS-28,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-28; release=independent; split-from=LS-21; exception=N/A`

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

- [ ] **AC1** `libra log -p -1 -U1` 的 stdout 逐字等于 Git 同夹具期望（hunk 上下文为 1 行）。
- [ ] **AC2** `libra log -p -1 --unified=0` 的 stdout 逐字等于 Git 同夹具期望（hunk 无上下文）。
- [ ] **AC3** 非数值 `-Ux` 以 `LBR-CLI-002` 拒绝（背景：`libra diff -Ux` 现行亦以 `LBR-CLI-002` 拒绝，2026-11-18 以 libra 0.30.207 预探；本条不比较两条命令）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F30：基线提交 B 与其子提交 X（X 的父提交为 B）：B 含 10 行文件，X 修改其第 5 行；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::diff_unified_context_passthrough`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC2 → `command::log_test::diff_unified_context_passthrough`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC3 → `command::log_test::diff_unified_context_passthrough`：期望 stderr 含 `LBR-CLI-002`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_unified_context_passthrough)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC3；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_patch_fails_when_commit_blob_is_missing) | test(=command::log_test::test_log_patch_no_pathspec) | test(=command::log_test::test_log_patch_with_pathspec) | test(=command::log_test::test_log_patch_with_stat_shows_diffstat_before_patch)'`（共享回归组：既有 `log -p` 用例 4 个；预期 N = 4；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-29`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `src/command/diff/compare.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--unified`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_diff_-U/--unified上下文行数; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=6/8; landing=2; prod-files=4; scope=M; deps=LS-29,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-29; release=independent; split-from=LS-21; exception=N/A`

---

### Task LS-31: `diff` 新增 `--color-moved-ws` 空白模式（#970；自 LS-22 拆出；拆出 LS-35）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `src/command/diff.rs` 与 `src/command/diff/render.rs:782` 新增 `--color-moved-ws=<modes>` 解析（`no`、`ignore-space-at-eol`、`ignore-space-change`、`ignore-all-space`，逗号组合；`no` 清除此前模式，与 Git `diff.c` 一致）与移动行空白归一化，供 `libra diff` 直接使用并作为 `pub(crate)` 能力留待 `LS-22` 在 `log` 复用。拆出 `LS-35`（`allow-indentation-change`）。

**Out of scope:** `allow-indentation-change` 模式及其与其它空白模式的互斥（由 `LS-35` 处理）；`LogArgs` 参数暴露（由 `LS-22` 处理）；`show --color-moved --word-diff` 组合（由 `LS-24` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 已有 `--color-moved` / `--no-color-moved`，`colorize_diff` 位于 `diff/render.rs:782`、`moved_line_bodies` 位于 `diff.rs:5456`，但尚未实现 `--color-moved-ws` | `src/command/diff.rs:438-445,5456`、`src/command/diff/render.rs:782` |
| Git 对 `--color-moved-ws` 的取值解析（`no` 清零、逗号分隔、未知值报错） | `git@6de20f6092:diff.c:325-355` |

**Acceptance criteria:**

- [ ] **AC1** 不带 `--color-moved-ws` 时，`libra diff --color=always --color-moved` 把变体 Vall 按普通增删着色（不识别为移动）。
- [ ] **AC2** `--color-moved-ws=ignore-space-at-eol` 下变体 Veol 以移动行颜色着色。
- [ ] **AC3** `--color-moved-ws=ignore-space-change` 下变体 Vchg 以移动行颜色着色。
- [ ] **AC4** `--color-moved-ws=ignore-all-space` 下变体 Vall 以移动行颜色着色。
- [ ] **AC5** `--color-moved-ws=no` 的输出与不带该选项时逐字相同。
- [ ] **AC6** `--color-moved-ws=ignore-all-space,no` 的输出与不带该选项时逐字相同（`no` 清除此前模式）。
- [ ] **AC7** `--color-moved-ws=ignore-space-at-eol,ignore-space-change` 下变体 Vchg 以移动行颜色着色（逗号分隔的后续模式生效）。
- [ ] **AC8** 非法值 `--color-moved-ws=bogus` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F31：工作区修改把一段 4 行代码块移到文件末尾，并分别制造三个移动块变体：仅行尾空白不同（Veol）、仅空白数量不同（Vchg）、仅插入行内空白（Vall）；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::diff_test::color_moved_ws_modes`：期望 exit 0；Vall 各行的 SGR 为普通增删颜色。
- AC2 → `command::diff_test::color_moved_ws_modes`：期望 exit 0；Veol 各行的 SGR 为 Libra 移动行颜色。
- AC3 → `command::diff_test::color_moved_ws_modes`：期望 exit 0；Vchg 各行的 SGR 为 Libra 移动行颜色。
- AC4 → `command::diff_test::color_moved_ws_modes`：期望 exit 0；Vall 各行的 SGR 为 Libra 移动行颜色。
- AC5 → `command::diff_test::color_moved_ws_modes`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC6 → `command::diff_test::color_moved_ws_modes`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC7 → `command::diff_test::color_moved_ws_modes`：期望 exit 0；Vchg 各行的 SGR 为 Libra 移动行颜色。
- AC8 → `command::diff_test::color_moved_ws_modes`：期望 stderr 含 `LBR-CLI-002`（Git：`unknown color-moved-ws mode 'bogus'`，exit 129）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::diff_test::color_moved_ws_modes)'`（卡特有 `(new)`，`tests/command/diff_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::diff_test::test_diff_color_moved)'`（共享回归组：既有 `--color-moved` 用例；预期 N = 1；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-30`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/diff_test.rs`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/diff_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/diff.md`、`docs/commands/zh-CN/diff.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`（由 `--only diff` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--color-moved-ws`。逐文件语义核对：`docs/development/commands/diff.md` 与 `COMPATIBILITY.md` 的 `| diff |` 行（基线第 227 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=diff_--color-moved-ws空白模式; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=LS-30,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-30; release=independent; split-from=LS-22; exception=N/A`

---

### Task LS-35: `diff` 支持 `--color-moved-ws=allow-indentation-change`（#970；自 LS-31 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LS-31` 的 `--color-moved-ws` 解析之上新增 `allow-indentation-change`：整块统一增减缩进的移动块仍识别为移动；与其它空白模式的组合按 Git 的有序解析判定——逐项解析时 `no` 清除其前全部模式，解析结束后同时含 `allow-indentation-change` 与任一 `ignore-*` 模式才报错（因此 `ignore-all-space,no,allow-indentation-change`、`allow-indentation-change,no` 均合法）。

**Out of scope:** `ignore-*` 与 `no` 模式（`LS-31` 已交付）；`log` 侧暴露（`LS-22`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `LS-31` 交付后 `--color-moved-ws` 仍不识别 `allow-indentation-change` | `src/command/diff.rs:438-445`、`src/command/diff/render.rs:782` |
| Git 逐项解析：`no` 清零此前模式（`333-334`），解析结束后同时含 `allow-indentation-change` 与任一 `XDF_WHITESPACE_FLAGS` 才报 `cannot be combined with other whitespace modes`（`349-352`，exit 129）；Git 2.54.0 预探：`allow-indentation-change,ignore-all-space` 报错，`ignore-all-space,no,allow-indentation-change`、`allow-indentation-change,no`、`no,allow-indentation-change` 均 exit 0 | `git@6de20f6092:diff.c:333-352`；本机 Git 2.54.0 预探 |

**Acceptance criteria:**

- [ ] **AC1** `libra diff --color=always --color-moved --color-moved-ws=allow-indentation-change` 把整体增加缩进的移动块以移动行颜色着色。
- [ ] **AC2** `--color-moved-ws=allow-indentation-change,ignore-all-space` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC3** `--color-moved-ws=ignore-all-space,no,allow-indentation-change` 的 stdout 与仅 `--color-moved-ws=allow-indentation-change` 时逐字相同（`no` 清除其前模式，组合合法）。
- [ ] **AC4** `--color-moved-ws=allow-indentation-change,no` 的 stdout 与不带 `--color-moved-ws` 时逐字相同（`no` 清除 `allow-indentation-change`）。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F35：工作区修改把一段 4 行代码块移到文件末尾并整体增加 4 个空格缩进；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::diff_test::color_moved_ws_allow_indentation_change`：期望 exit 0；该块各行的 SGR 为 Libra 移动行颜色。
- AC2 → `command::diff_test::color_moved_ws_allow_indentation_change`：期望 stderr 含 `LBR-CLI-002`（Git：`allow-indentation-change cannot be combined with other whitespace modes`）。
- AC3 → `command::diff_test::color_moved_ws_allow_indentation_change`：期望 两次调用均 exit 0；stdout 逐字相同（Git 2.54.0 预探：该组合 exit 0）。
- AC4 → `command::diff_test::color_moved_ws_allow_indentation_change`：期望 两次调用均 exit 0；stdout 逐字相同（Git 2.54.0 预探：该组合 exit 0）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::diff_test::color_moved_ws_allow_indentation_change)'`（卡特有 `(new)`，`tests/command/diff_test.rs` 内新增；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-31`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/diff_test.rs`, `docs/commands/diff.md`, `docs/commands/zh-CN/diff.md`, `docs/development/commands/diff.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/diff_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/diff.md`、`docs/commands/zh-CN/diff.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md`（由 `--only diff` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`allow-indentation-change`。逐文件语义核对：`docs/development/commands/diff.md` 与 `COMPATIBILITY.md` 的 `| diff |` 行（基线第 227 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=diff_--color-moved-ws=allow-indentation-change; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=2; prod-files=2; scope=M; deps=LS-31,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-31; release=independent; split-from=LS-31; exception=N/A`

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

- [ ] **AC1** `libra log -p -1 X --color=always --color-moved` 对移动行的着色与 `libra diff --color=always --color-moved B X` 对同一变更的着色逐字一致。
- [ ] **AC2** `libra log -p -1 X --color=always --color-moved --no-color-moved` 的输出与不带 `--color-moved` 时逐字相同（后写者胜出）。
- [ ] **AC3** `libra log -p -1 X --color=always --color-moved --color-moved-ws=ignore-all-space` 的移动行着色与 `libra diff` 同选项逐字一致。
- [ ] **AC4** 非法 `--color-moved=bogus` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC5** 非法 `--color-moved-ws=bogus` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F22：基线提交 B 与其子提交 X（X 的父提交为 B）：X 把 4 行代码块 P 从文件 `A` 原样移到文件 `B.txt`，并把另一段 4 行代码块 Q 从 `A` 移到 `B.txt` 且在 Q 的每行插入行内空白（Q 只在 `--color-moved-ws=ignore-all-space` 下识别为移动；`libra diff` 对这类变体的移动着色由 `LS-31` AC4 验收）；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::log_test::color_moved_and_ws`：期望 两次调用均 exit 0；块 P 各行的 SGR 序列逐字相同。
- AC2 → `command::log_test::color_moved_and_ws`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC3 → `command::log_test::color_moved_and_ws`：期望 两次调用均 exit 0；块 Q 各行的 SGR 序列逐字相同。
- AC4 → `command::log_test::color_moved_and_ws`：期望 stderr 含 `LBR-CLI-002`。
- AC5 → `command::log_test::color_moved_and_ws`：期望 stderr 含 `LBR-CLI-002`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::color_moved_and_ws)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::test_log_patch_fails_when_commit_blob_is_missing) | test(=command::log_test::test_log_patch_no_pathspec) | test(=command::log_test::test_log_patch_with_pathspec) | test(=command::log_test::test_log_patch_with_stat_shows_diffstat_before_patch)'`（共享回归组：既有 `log -p` 用例 4 个；预期 N = 4；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-35`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--color-moved`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--color-moved与--color-moved-ws复用; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=6/8; landing=2; prod-files=3; scope=M; deps=LS-35,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-35; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-23: `show` 支持 `--patience` 与 `--anchored`（#857）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `ShowArgs` 新增 `--patience` 与 `--anchored=<text>`，经 `LogDiffOptions`（`ADR-LS-01`）透传至 `src/command/diff.rs` 比较引擎（复用 `diff.rs` 既有 `patience` / `anchored`）。

**Out of scope:** 不改变 `show` 非提交对象（blob/tree/tag）输出。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 已实现 `--patience` 与 `--anchored`，`ShowArgs` 未暴露；`show.rs` 通过 `log::generate_diff` 生成差异 | `src/command/diff.rs:131-153`、`src/command/show.rs:25-28,73-187` |

**Acceptance criteria:**

- [ ] **AC1** `libra show --patience X` 的 patch 部分与 `libra diff --patience B X` 逐字相同。
- [ ] **AC2** `libra show --anchored=<text> X` 的 patch 部分与 `libra diff --anchored=<text> B X` 逐字相同。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F23：基线提交 B 与其子提交 X（X 的父提交为 B）：X 的修改在 Myers 与 patience 下产生不同 hunk，且以 AC2 的 `<text>`（`X` 版本中唯一出现的一行）作锚时产生与不带锚点不同的 hunk；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::show_test::patience_and_anchored`：期望 两次调用均 exit 0；patch 部分逐字相同。
- AC2 → `command::show_test::patience_and_anchored`：期望 两次调用均 exit 0；patch 部分逐字相同。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::patience_and_anchored)'`（卡特有 `(new)`，`tests/command/show_test.rs` 内新增；覆盖 AC1–AC2；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::show_test::/)'`（共享回归组：`command::show_test` 模块族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test compat_diff_review_options`（共享回归组：整 target，含 patience/anchored 算法语义守卫；按「验证执行协议」第 5 项先 `cargo nextest list --test compat_diff_review_options` 记录 N≥1，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-22`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/show.md`、`docs/commands/zh-CN/show.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`（由 `--only show` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--anchored`。逐文件语义核对：`docs/development/commands/show.md` 与 `COMPATIBILITY.md` 的 `| show |` 行（基线第 199 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=show_--patience与--anchored; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=6/8; landing=2; prod-files=4; scope=M; deps=LS-22,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-22; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-26: `log` 支持 `--word-diff`、`--word-diff-regex` 与 `--color-words`（#966；拆出 LS-33）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 为 `LogArgs` 接入 `--word-diff[=<mode>]`、`--word-diff-regex=<regex>` 与 `--color-words[=<regex>]`，按 `ADR-LS-01` 经 `LogDiffOptions` 复用 `src/command/diff/render.rs:61`（`render_word_diff`，提升为 `pub(crate)`）与 `diff.rs` 的词级选项解析。拆出 `LS-33`（`show` 同组选项）。

**Out of scope:** `show` 的同组选项（由 `LS-33` 处理）；`--color-moved` 与 `--word-diff` 在 `show` 中的组合（由 `LS-24` 处理）；`--color-words` 与 `--word-diff=<mode>` 的后写优先级——`libra diff` 当前恒以 `--color-words` 优先（`src/command/diff.rs:1381-1388`），与 Git 的后写者胜出不同，`log`/`show` 复用同一解析不单独偏离（尚未排期；重启条件：为 `diff` 另立卡修正后 `log`/`show` 自动继承）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 支持 `word_diff`/`color_words`/`word_diff_regex`，渲染在 `diff/render.rs:61`（`pub(super)`），`LogArgs` 未暴露 | `src/command/diff.rs:170-193,876`、`src/command/diff/render.rs:61`、`src/command/log.rs:172-486` |

**Acceptance criteria:**

- [ ] **AC1** `libra log -p -1 W --word-diff` 的 `@@` 行之后的正文逐字为 `[-foo_bar-]{+foo_BAR+} baz [-qux-]{+QUX+}\n`。
- [ ] **AC2** `libra log -p -1 W --word-diff=porcelain` 的 `@@` 行之后的正文逐字为 `-foo_bar\n+foo_BAR\n  baz \n-qux\n+QUX\n~\n`。
- [ ] **AC3** `libra log -p -1 W --color-words` 的 `@@` 行之后的正文逐字为 `\x1b[31mfoo_bar\x1b[0m\x1b[32mfoo_BAR\x1b[0m baz \x1b[31mqux\x1b[0m\x1b[32mQUX\x1b[0m\n`。
- [ ] **AC4** `libra log -p -1 W --word-diff-regex='[A-Za-z]+'` 的 `@@` 行之后的正文逐字为 `foo_[-bar-]{+BAR+} baz [-qux-]{+QUX+}\n`。
- [ ] **AC5** `libra log -p -1 W --color-words='[A-Za-z]+'` 的 `@@` 行之后的正文逐字为 `foo_\x1b[31mbar\x1b[0m\x1b[32mBAR\x1b[0m baz \x1b[31mqux\x1b[0m\x1b[32mQUX\x1b[0m\n`。
- [ ] **AC6** `libra log -p -1 W --color-words --word-diff=plain` 的 `@@` 行之后的正文逐字等于 AC3 的期望正文（沿用 `libra diff` 现有组合优先级：`--color-words` 优先）。
- [ ] **AC7** `libra log -p -1 W --word-diff=bogus` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC8** `libra log -p -1 W --word-diff-regex='['` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F26：基线提交 B 含 `w.txt`（一行 `foo_bar baz qux`），其子提交 W 把该行改为 `foo_BAR baz QUX`；各期望行取自 `libra 0.30.207` 对同夹具执行 `libra diff <模式> B W` 的预探（`plain`、`porcelain` 与 regex 结果另与本机 Git 2.54.0 一致），开工时以当时的 `libra diff <模式> B W` 复核；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::log_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC2 → `command::log_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC3 → `command::log_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC4 → `command::log_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC5 → `command::log_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC6 → `command::log_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC7 → `command::log_test::word_diff_and_color_words`：期望 stderr 含 `LBR-CLI-002`。
- AC8 → `command::log_test::word_diff_and_color_words`：期望 stderr 含 `LBR-CLI-002`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::word_diff_and_color_words)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-23`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--word-diff`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--word-diff与--color-words; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=2; prod-files=3; scope=M; deps=LS-23,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-23; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-33: `show` 支持 `--word-diff`、`--word-diff-regex` 与 `--color-words`（#966；自 LS-26 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 为 `ShowArgs` 接入 `--word-diff[=<mode>]`、`--word-diff-regex=<regex>` 与 `--color-words[=<regex>]`，经 `LS-26` 已扩展的 `LogDiffOptions` 透传，不再修改 `src/command/diff/`。

**Out of scope:** `log` 的同组选项（`LS-26` 已交付）；`show --color-moved --word-diff` 组合（由 `LS-24` 处理）；`--color-words` 与 `--word-diff=<mode>` 的后写优先级（沿用 `libra diff` 现状，`src/command/diff.rs:1381-1388`；尚未排期；重启条件：为 `diff` 另立卡修正该优先级后 `log`/`show` 经同一解析自动继承）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ShowArgs` 未暴露词级选项；`show.rs` 通过 `log::generate_diff` 生成提交 diff | `src/command/show.rs:25-28,73-187,638-693` |

**Acceptance criteria:**

- [ ] **AC1** `libra show W --word-diff` 的 `@@` 行之后的正文逐字为 `[-foo_bar-]{+foo_BAR+} baz [-qux-]{+QUX+}\n`。
- [ ] **AC2** `libra show W --word-diff=porcelain` 的 `@@` 行之后的正文逐字为 `-foo_bar\n+foo_BAR\n  baz \n-qux\n+QUX\n~\n`。
- [ ] **AC3** `libra show W --color-words` 的 `@@` 行之后的正文逐字为 `\x1b[31mfoo_bar\x1b[0m\x1b[32mfoo_BAR\x1b[0m baz \x1b[31mqux\x1b[0m\x1b[32mQUX\x1b[0m\n`。
- [ ] **AC4** `libra show W --word-diff-regex='[A-Za-z]+'` 的 `@@` 行之后的正文逐字为 `foo_[-bar-]{+BAR+} baz [-qux-]{+QUX+}\n`。
- [ ] **AC5** `libra show W --color-words='[A-Za-z]+'` 的 `@@` 行之后的正文逐字为 `foo_\x1b[31mbar\x1b[0m\x1b[32mBAR\x1b[0m baz \x1b[31mqux\x1b[0m\x1b[32mQUX\x1b[0m\n`。
- [ ] **AC6** `libra show W --color-words --word-diff=plain` 的 `@@` 行之后的正文逐字等于 AC3 的期望正文（沿用 `libra diff` 现有组合优先级：`--color-words` 优先）。
- [ ] **AC7** `libra show W --word-diff=bogus` 以 `LBR-CLI-002` 拒绝。
- [ ] **AC8** `libra show W --word-diff-regex='['` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F33：基线提交 B 含 `w.txt`（一行 `foo_bar baz qux`），其子提交 W 把该行改为 `foo_BAR baz QUX`；各期望行取自 `libra 0.30.207` 对同夹具执行 `libra diff <模式> B W` 的预探（`plain`、`porcelain` 与 regex 结果另与本机 Git 2.54.0 一致），开工时以当时的 `libra diff <模式> B W` 复核；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::show_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC2 → `command::show_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC3 → `command::show_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC4 → `command::show_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC5 → `command::show_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC6 → `command::show_test::word_diff_and_color_words`：期望 exit 0；`@@` 行之后的正文逐字相等。
- AC7 → `command::show_test::word_diff_and_color_words`：期望 stderr 含 `LBR-CLI-002`。
- AC8 → `command::show_test::word_diff_and_color_words`：期望 stderr 含 `LBR-CLI-002`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::word_diff_and_color_words)'`（卡特有 `(new)`，`tests/command/show_test.rs` 内新增；覆盖 AC1–AC8；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::show_test::/)'`（共享回归组：`command::show_test` 模块族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-26`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/show.rs`, `src/command/log.rs`, `tests/command/show_test.rs`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/show.rs`, `src/command/log.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/show.md`、`docs/commands/zh-CN/show.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`（由 `--only show` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--word-diff`。逐文件语义核对：`docs/development/commands/show.md` 与 `COMPATIBILITY.md` 的 `| show |` 行（基线第 199 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=show_--word-diff与--color-words; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=LS-26,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-26; release=independent; split-from=LS-26; exception=N/A`

---

### Task LS-24: `show` 支持 `--color-moved` 与 `--word-diff` 组合（#858）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `LS-33` 已为 `show` 接入 `--word-diff` 的基础上，在 `ShowArgs` 接入 `--color-moved[=<mode>]`、`--no-color-moved`、`--color-moved-ws` 并启用与 `--word-diff` / `--color-words` 的组合输出（匹配 `libra diff --color-moved --word-diff`）。

**Out of scope:** 独立的 `--word-diff` 实现（`LS-33` 已完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ShowArgs` 缺少 `color_moved` 字段；`show.rs` 通过 `log::generate_diff` 生成差异 | `src/command/show.rs:25-28,73-187` |

**Acceptance criteria:**

- [ ] **AC1** `libra show --color-moved --word-diff X` 退出码为 0（不再被 CLI 拒绝）。
- [ ] **AC2** `libra show --color-moved --word-diff X` 的 diff 部分与 `libra diff --color-moved --word-diff B X` 逐字相同。
- [ ] **AC3** `libra show --color=always --color-moved X` 的 diff 部分与 `libra diff --color=always --color-moved B X` 逐字相同。
- [ ] **AC4** `libra show --color=always --color-moved --no-color-moved X` 的输出与不带 `--color-moved` 时逐字相同（后写者胜出）。
- [ ] **AC5** `libra show --color=always --color-moved --color-moved-ws=ignore-all-space X` 的 diff 部分与 `libra diff` 同选项逐字相同。
- [ ] **AC6** `libra show --color-moved=bogus X` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F24：基线提交 B 与其子提交 X（X 的父提交为 B）：X 同时原样移动一段 4 行代码块 P、移动另一段 4 行代码块 Q 并在 Q 的每行插入行内空白（Q 只在 `--color-moved-ws=ignore-all-space` 下识别为移动），并在另一行替换单词；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::show_test::color_moved_with_word_diff`：期望 exit 0。
- AC2 → `command::show_test::color_moved_with_word_diff`：期望 两次调用均 exit 0；diff 部分逐字相同。
- AC3 → `command::show_test::color_moved_with_word_diff`：期望 两次调用均 exit 0；diff 部分逐字相同。
- AC4 → `command::show_test::color_moved_with_word_diff`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC5 → `command::show_test::color_moved_with_word_diff`：期望 两次调用均 exit 0；diff 部分逐字相同。
- AC6 → `command::show_test::color_moved_with_word_diff`：期望 stderr 含 `LBR-CLI-002`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::color_moved_with_word_diff)'`（卡特有 `(new)`，`tests/command/show_test.rs` 内新增；覆盖 AC1–AC6；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::show_test::/)'`（共享回归组：`command::show_test` 模块族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-33`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/show.md`、`docs/commands/zh-CN/show.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`（由 `--only show` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--color-moved`。逐文件语义核对：`docs/development/commands/show.md` 与 `COMPATIBILITY.md` 的 `| show |` 行（基线第 199 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=show_--color-moved与--word-diff组合; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=6/8; landing=2; prod-files=4; scope=M; deps=LS-33,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-33; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-25: `show` 支持 `--textconv` 与 `--no-textconv`（#862）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `ShowArgs` 新增 `--textconv`（与 Git 一致，提交 diff 默认启用 `diff.<driver>.textconv`）与 `--no-textconv`，经 `LogDiffOptions` 复用 `src/command/diff.rs:3403` 的 `run_textconv` 执行路径（受信任的仓库/用户配置 `diff.<driver>.textconv` 经临时文件执行外部过滤器，无独立进程沙箱；过滤器非零退出或命令缺失时以 `DiffError::TextconvFailed` fail-closed，经 `diff.rs:761` 映射为 `LBR-IO-001`；默认退出码 128，`LIBRA_FINE_EXIT_CODES` 与显式覆盖按现有错误协议 `src/utils/error.rs:514-545,1068-1076`）。

**Out of scope:** 不改变 `diff.rs:584,590` 内部的 `for_service`（`no_textconv: true`）plumbing 辅助构造函数。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 已实现 `--textconv`/`--no-textconv` 与 `run_textconv`，`TextconvFailed` 映射 `IoReadFailed`（`LBR-IO-001`）；`ShowArgs` 未暴露 | `src/command/diff.rs:497-511,675-676,761-765,3403`、`src/command/show.rs:25-28,73-187` |

**Acceptance criteria:**

- [ ] **AC1** `libra show --textconv X` 的 diff 部分与 `libra diff --textconv B X` 逐字相同。
- [ ] **AC2** 未给选项时 `libra show X` 的输出与 `libra show --textconv X` 逐字相同（默认启用 textconv）。
- [ ] **AC3** `libra show --no-textconv X` 的 diff 部分与 `libra diff --no-textconv B X` 逐字相同。
- [ ] **AC4** `libra show --textconv X` 调用过滤器（对照：证明测试过滤器与标记文件生效，使 AC5 可判定）。
- [ ] **AC5** `libra show --no-textconv X` 不调用过滤器。
- [ ] **AC6** 过滤器以非零码退出时 `libra show X` 以 `LBR-IO-001` fail-closed 失败。
- [ ] **AC7** 过滤器命令不存在时 `libra show X` 以 `LBR-IO-001` fail-closed 失败。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F25：`.libra_attributes` 使 `*.bin` 使用驱动 `hex`，`diff.hex.textconv` 指向测试内脚本，该脚本每次被调用时在仓库工作区之外的测试临时目录写入调用标记文件（每次断言前删除）；基线提交 B 与其子提交 X（X 的父提交为 B）：X 修改 `x.bin`；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::show_test::textconv_and_no_textconv`：期望 两次调用均 exit 0；diff 部分逐字相同。
- AC2 → `command::show_test::textconv_and_no_textconv`：期望 两次调用均 exit 0；stdout 逐字相同。
- AC3 → `command::show_test::textconv_and_no_textconv`：期望 两次调用均 exit 0；diff 部分逐字相同。
- AC4 → `command::show_test::textconv_and_no_textconv`：期望 exit 0；调用标记文件存在。
- AC5 → `command::show_test::textconv_and_no_textconv`：期望 exit 0；调用标记文件不存在。
- AC6 → `command::show_test::textconv_failure_and_missing_command`：期望 stderr 含 `LBR-IO-001`。
- AC7 → `command::show_test::textconv_failure_and_missing_command`：期望 stderr 含 `LBR-IO-001`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::textconv_and_no_textconv)'`（卡特有 `(new)`，`tests/command/show_test.rs` 内新增；覆盖 AC1–AC5；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::textconv_failure_and_missing_command)'`（卡特有 `(new)`，`tests/command/show_test.rs` 内新增；覆盖 AC6–AC7；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::show_test::/)'`（共享回归组：`command::show_test` 模块族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-24`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/show.md`、`docs/commands/zh-CN/show.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`（由 `--only show` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--textconv`。逐文件语义核对：`docs/development/commands/show.md` 与 `COMPATIBILITY.md` 的 `| show |` 行（基线第 199 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=show_--textconv与--no-textconv; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=8/8; landing=2; prod-files=4; scope=M; deps=LS-24,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-24; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-27: `log` 支持 `--diff-filter`（#973；拆出 LS-34）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 为 `LogArgs` 接入 `--diff-filter=<filter>`，按 `ADR-LS-01` 把 `src/command/diff.rs` 的 `parse_diff_filter` / `apply_diff_filter` 提升为 `pub(crate)` 并复用，过滤提交的变更文件列表与 diff 输出（无匹配变更文件的提交不输出）。拆出 `LS-34`（`show --diff-filter`）。

**Out of scope:** 不改变 `diff --diff-filter` 语义；`show --diff-filter`（由 `LS-34` 处理）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `diff.rs` 已实现私有的 `parse_diff_filter`（2228）与 `apply_diff_filter`（2265），`LogArgs` 未暴露 | `src/command/diff.rs:273,2228,2265`、`src/command/log.rs:172-486` |

**Acceptance criteria:**

- [ ] **AC1** `libra log --diff-filter=A --format=%H` 的提交集合恰为 {C1, C3}（只输出含新增文件的提交）。
- [ ] **AC2** `libra log --diff-filter=A --name-status --format=%H` 的输出逐字等于 Git 同夹具期望（仅 C1 的 `A\ta.txt`、`A\tb.txt` 与 C3 的 `A\tc.txt` 记录，无其它状态）。
- [ ] **AC3** 小写排除 `libra log --diff-filter=a --format=%H` 的 stdout 逐字等于 Git 同夹具期望。
- [ ] **AC4** 非法字符 `--diff-filter=Q` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F27：C1 新增 `a.txt` 与 `b.txt`，C2 修改 `a.txt`，C3 删除 `b.txt` 并新增 `c.txt`；每次调用另按「验证执行协议」第 4 项断言只读不变量；标注「Git 2.54.0 预探」或「Git 同夹具期望」的值按该项以 `git@6de20f6092` 取得并硬编码）

- AC1 → `command::log_test::diff_filter`：期望 exit 0；stdout 提交集合恰为 {C1, C3}。
- AC2 → `command::log_test::diff_filter`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC3 → `command::log_test::diff_filter`：期望 exit 0；stdout 逐字等于 Git 同夹具期望。
- AC4 → `command::log_test::diff_filter`：期望 stderr 含 `LBR-CLI-002`（Git：`unknown change class 'Q'`，exit 129）。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::diff_filter)'`（卡特有 `(new)`，`tests/command/log_test.rs` 内新增；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::diff_test::/)'`（共享回归组：`command::diff_test` 模块族，守护 `src/command/diff/` 共享逻辑；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-25`, `DEP-LS-01`, `DEP-LS-02`, `DEP-LS-03`

**Implementation write set:** `src/command/log.rs`, `src/command/diff.rs`, `tests/command/log_test.rs`, `docs/commands/log.md`, `docs/commands/zh-CN/log.md`, `docs/development/commands/log.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/log.rs`, `src/command/diff.rs`, `tests/command/log_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/log.md`、`docs/commands/zh-CN/log.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md`（由 `--only log` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--diff-filter`。逐文件语义核对：`docs/development/commands/log.md` 与 `COMPATIBILITY.md` 的 `| log |` 行（基线第 196 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=log_--diff-filter; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=LS-25,DEP-LS-01,DEP-LS-02,DEP-LS-03; writeset=序列化于LS-25; release=independent; split-from=N/A; exception=N/A`

---

### Task LS-34: `show` 支持 `--diff-filter`（#973；自 LS-27 拆出）

**Task type:** `implementation` | **Lifecycle / Acceptance:** `pending` / `空`

**Description:** 为 `ShowArgs` 接入 `--diff-filter=<filter>`，复用 `LS-27` 已提升为 `pub(crate)` 的 `parse_diff_filter` / `apply_diff_filter`（不再修改 `src/command/diff/`）。

**Out of scope:** `log --diff-filter`（`LS-27` 已交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `ShowArgs` 未暴露 `--diff-filter` | `src/command/show.rs:73-187` |
| Git 2.54.0 预探：`git show --diff-filter=A --name-status HEAD`（HEAD 无新增文件）输出 0 字节、exit 0 | 本机 Git 2.54.0（开工时以 `git@6de20f6092` 复核） |

**Acceptance criteria:**

- [ ] **AC1** `libra show --diff-filter=M --name-status C` 的 name-status 部分逐字为 `M\ta.txt\n`。
- [ ] **AC2** `libra show --diff-filter=M C` 的 patch 中 `diff --git` 头的文件集合恰为 {`a.txt`}（`libra show` 默认输出提交 patch）。
- [ ] **AC3** `libra show --diff-filter=A D`（无匹配文件）的 stdout 为空（不输出该提交）。
- [ ] **AC4** 非法字符 `--diff-filter=Q` 以 `LBR-CLI-002` 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：`Docs and compatibility impact` 所列每个文件按「文档与网站验收命令」第 1–7 步逐文件交付并验收（不计入卡特有 AC 分子）。

**AC → 断言映射:**（夹具 F34：提交 C 同时修改 `a.txt` 并删除 `b.txt`；提交 D 只修改 `a.txt`；每次调用另按「验证执行协议」第 4 项断言只读不变量；本卡期望均为 Libra 自身基准，不涉及 Git 期望取得）

- AC1 → `command::show_test::diff_filter`：期望 exit 0；name-status 部分逐字为 `M\ta.txt\n`。
- AC2 → `command::show_test::diff_filter`：期望 exit 0；`diff --git` 头的文件集合恰为 {`a.txt`}。
- AC3 → `command::show_test::diff_filter`：期望 exit 0；stdout 为空。
- AC4 → `command::show_test::diff_filter`：期望 stderr 含 `LBR-CLI-002`。
- 文档 AC → 「文档与网站验收命令」第 1–7 步（逐文件 SHA-256、关键字命中、网站同步与漂移检查、typecheck/build、Claude Code PASS、精确提交推送、部署核验）。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::show_test::diff_filter)'`（卡特有 `(new)`，`tests/command/show_test.rs` 内新增；覆盖 AC1–AC4；按「验证执行协议」第 1–4 项执行）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(/^command::show_test::/)'`（共享回归组：`command::show_test` 模块族；N 以运行前 list 结果为准（≥1）；按「验证执行协议」第 5 项先以同一过滤 `cargo nextest list` 核对，要求 exit 0、通过 = N、失败 = 0、跳过 = 0；不计为卡特有 AC 证据）

**Full-suite trigger:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）` | **Dependencies:** `LS-27`, `DEP-LS-01`, `DEP-LS-03`

**Implementation write set:** `src/command/show.rs`, `src/command/log.rs`, `tests/command/show_test.rs`, `docs/commands/show.md`, `docs/commands/zh-CN/show.md`, `docs/development/commands/show.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`

**Release write set:** `Inherited` | **Files likely touched:** `src/command/show.rs`, `src/command/log.rs`, `tests/command/show_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 必须同卡更新：`docs/commands/show.md`、`docs/commands/zh-CN/show.md` 与网站页 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md`（由 `--only show` 同步生成；网站 zh-CN 在渲染时镜像 EN，无独立文件）；文档关键字（用于 EN/zh 命令页）：`--diff-filter`。逐文件语义核对：`docs/development/commands/show.md` 与 `COMPATIBILITY.md` 的 `| show |` 行（基线第 199 行）——受本卡行为影响即同卡更新（兼容矩阵原地修改，不增删行），否则按第 1 步记录理由与未变 SHA-256。按「文档与网站验收命令」第 1–7 步逐文件交付并验收。`docs/error-codes.md`：N/A（复用既有稳定码）。

**Rollback mode:** `immutable-release` | **Estimated scope:** `M` | **Version increment:** `patch` | **Release boundary:** `independent` | **C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=show_--diff-filter; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=4/8; landing=1; prod-files=2; scope=M; deps=LS-27,DEP-LS-01,DEP-LS-03; writeset=序列化于LS-27; release=independent; split-from=LS-27; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `log.rs`、`log/config.rs`、`log/decorate_filter.rs`、`show.rs` 内单元测试 | 下方代码块 `M-UNIT` |
| 集成 | 全部 27 项 issue 行为用例（各卡卡特有用例按「验证执行协议」逐条精确运行） | 下方代码块 `M-INT` |
| 兼容 | pretty 占位符、配置默认值、浅遍历、diff 审阅选项、peel 选择器 | `source .env.test && cargo nextest run --test compat_pretty_format_placeholders --test compat_config_defaults_semantics --test compat_shallow_walk_helper --test compat_diff_review_options --test compat_rev_parse_peel_selectors` |
| 迁移 | N/A（无 SQLite schema 变更） | N/A |
| 安全 | `--output=<file>` 路径校验、TTY 控制字符清理、`textconv` 失败传播 | 下方代码块 `M-SEC` |
| 性能 | 嵌套 tag 迭代解引用、`-L` / `--simplify-by-decoration` / `--find-object` 有界遍历 | 下方代码块 `M-PERF` |
| live/gated | 每卡 T-1 与收口阶段双环境全量回归 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

矩阵命令（可直接复制；`|` 为 nextest 过滤表达式的并集运算符，均为共享回归口径，按「验证执行协议」第 5 项记录 N 与四计数）：

```bash
# M-UNIT
source .env.test && cargo nextest run --lib -E 'test(/^command::log::/) | test(/^command::show::/)'
# M-INT
source .env.test && cargo nextest run --test command_test -E 'test(/^command::log_test::/) | test(/^command::show_test::/) | test(/^command::diff_test::/)'
# M-SEC
source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::output_to_file) | test(=command::log_test::output_overwrite_and_protection) | test(=command::log_test::tty_output_no_stray_escape_sequences) | test(=command::show_test::textconv_failure_and_missing_command)'
# M-PERF
source .env.test && cargo nextest run --test command_test -E 'test(=command::log_test::decorate_nested_annotated_tags) | test(=command::log_test::line_range_history_output) | test(=command::log_test::simplify_by_decoration) | test(=command::log_test::find_object)'
```

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| LS-01 | #949; `log.rs:4202-4242` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::decorate_nested_annotated_tags` `(new)`；共享回归组 `compat_pretty_format_placeholders` |
| LS-02 | #950; `log.rs:1020-1075` | `src/command/log.rs`, `src/command/log/config.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::log_decorate_bool_true_and_pretty_raw` `(new)`；共享回归组 `compat_config_defaults_semantics`、`test(=command::log_test::test_log_invalid_decorate_uses_command_usage_error)` |
| LS-03 | #951; `log.rs:646-672` | `src/command/log.rs`, `src/command/log/config.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::log_exclude_decoration_config` `(new)`；共享回归组 `compat_config_defaults_semantics`、`test(/^command::log_test::lo0[78]_/)` |
| LS-04 | #952; `decorate_filter.rs:15-55` | `src/command/log.rs`, `src/command/log/decorate_filter.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::decorate_refs_issue_952_remaining` `(new)`；共享回归组 `test(/^command::log_test::lo0[578]_/)` |
| LS-05 | #953; `log.rs:172-486` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::simplify_by_decoration` `(new)`；共享回归组 `compat_shallow_walk_helper` |
| LS-06 | #954; `log/config.rs`、`log.rs:4202-4242` | `src/command/log.rs`, `src/command/log/config.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::clear_decorations_and_initial_set` `(new)`；共享回归组 `compat_config_defaults_semantics`、`test(/^command::log_test::lo0[78]_/)` |
| LS-07 | #959; `log.rs:1120-1126` | `src/command/log.rs`, `src/command/log/config.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::color_decorate_and_commit_config` `(new)`；共享回归组 `compat_config_defaults_semantics` |
| LS-08 | #960; `log.rs:4202-4242` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::decorate_replaced_and_env_base` `(new)`；共享回归组 `compat_pretty_format_placeholders` |
| LS-09 | #958; `log.rs:190-191` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::no_abbrev_issue_958_remaining` `(new)`；共享回归组 `compat_pretty_format_placeholders` |
| LS-10 | #956; `log.rs:172-486` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::no_walk_and_tags` `(new)`；共享回归组 `compat_shallow_walk_helper` |
| LS-11 | #965; `log.rs:228-233` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::before_and_after_aliases` `(new)`；共享回归组 `test(=command::log_test::test_log_json_since_filter_restricts_results)` |
| LS-12 | #969; `log.rs:4245-4290` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::diff_reverse_flag_r` `(new)`；共享回归组 `test(=command::log_test::test_log_reverse_outputs_oldest_first)`、`test(/^command::format_patch_test::/)` |
| LS-13 | #955; `log.rs:2325,2667-2683`, `pager.rs:95-103` | `src/command/log.rs`, `src/utils/pager.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::tty_output_no_stray_escape_sequences` `(new)`；共享回归组 `compat_pretty_format_placeholders` |
| LS-14 | #972; `log.rs:2334-2344,3096-3099` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::empty_format_or_pretty_omits_header` `(new)`；共享回归组 `compat_pretty_format_placeholders` |
| LS-15 | #963; `log.rs:172-486,2325`、`diff.c:5800-5815`（Git） | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::output_to_file` `(new)`；共享回归组 `test(/^command::log_test::/)` |
| LS-37 | #963; `diff.c:5800-5815`（Git）、`beneath.rs:28-37` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::output_overwrite_and_protection` `(new)`；共享回归组 `test(=command::log_test::output_to_file)`、`test(/^command::log_test::/)` |
| LS-36 | #963; `log.rs:2190-2192` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::output_json_to_file` `(new)`；共享回归组 `test(=command::log_test::output_to_file)`、`test(/^command::log_test::/)` |
| LS-16 | #962; `log.rs:469-471,2212-2216`, `log/config.rs:38` | `src/command/log.rs`, `src/command/log/config.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::follow_boolean_flag_single_pathspec` `(new)`；共享回归组 `test(=command::log_test::test_log_follow_detects_rename) \| test(=command::log_test::log_ineligible_follow_line_range_pathspec_walk_full_history)` |
| LS-17 | #961; `log.rs:479-480,1894-1910,1981-2030` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::line_range_history_output` `(new)`；共享回归组 `test(=command::log_test::test_log_line_range_flag_accepted) \| test(=command::log_test::log_ineligible_follow_line_range_pathspec_walk_full_history)` |
| LS-32 | #961; `log.rs:1894-1902,1981-2030` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::line_range_argument_validation` `(new)`；共享回归组 `test(=command::log_test::test_log_line_range_flag_accepted) \| test(=command::log_test::log_ineligible_follow_line_range_pathspec_walk_full_history)` |
| LS-18 | #968; `log.rs:398-408`、`diffcore-pickaxe.c:137-145`（Git） | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::find_object` `(new)`；共享回归组 `test(=command::log_test::test_log_pickaxe_s_finds_commit_that_changes_string_count) \| test(=command::log_test::test_log_pickaxe_g_matches_diff_line_regex)` |
| LS-19 | #833; `log.rs:172-486` | `src/command/log.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::log_cc_combined_diff` `(new)`；共享回归组 `test(=command::log_test::test_log_first_parent_skips_merged_branch_commits)` |
| LS-20 | #964; `log.rs:3416`, `diff/render.rs:278,412,503` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::raw_numstat_dirstat_formats` `(new)`；共享回归组 `test(/^command::diff_test::/)` |
| LS-21 | #967; `diff/compare.rs:405-417` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::diff_ignore_whitespace_passthrough` `(new)`；共享回归组 `test(=command::log_test::test_log_patch_fails_when_commit_blob_is_missing) \| test(=command::log_test::test_log_patch_no_pathspec) \| test(=command::log_test::test_log_patch_with_pathspec) \| test(=command::log_test::test_log_patch_with_stat_shows_diffstat_before_patch)`、`test(/^command::diff_test::/)` |
| LS-28 | #967; `diff/options.rs:124-160` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::diff_prefix_options_passthrough` `(new)`；共享回归组 `compat_diff_review_options`、`test(/^command::diff_test::/)` |
| LS-29 | #967; `diff/compare.rs:591` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::diff_full_index_passthrough` `(new)`；共享回归组 `test(=command::log_test::test_log_patch_fails_when_commit_blob_is_missing) \| test(=command::log_test::test_log_patch_no_pathspec) \| test(=command::log_test::test_log_patch_with_pathspec) \| test(=command::log_test::test_log_patch_with_stat_shows_diffstat_before_patch)`、`test(/^command::diff_test::/)` |
| LS-30 | #967; `diff/options.rs:29`, `diff/compare.rs:405-417` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs`, `src/command/diff/compare.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::diff_unified_context_passthrough` `(new)`；共享回归组 `test(=command::log_test::test_log_patch_fails_when_commit_blob_is_missing) \| test(=command::log_test::test_log_patch_no_pathspec) \| test(=command::log_test::test_log_patch_with_pathspec) \| test(=command::log_test::test_log_patch_with_stat_shows_diffstat_before_patch)`、`test(/^command::diff_test::/)` |
| LS-31 | #970; `diff.rs:438-445,5456`, `diff/render.rs:782` | `src/command/diff.rs`, `src/command/diff/render.rs` | `docs/commands/diff.md`、`docs/commands/zh-CN/diff.md`、`docs/development/commands/diff.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md` | `command::diff_test::color_moved_ws_modes` `(new)`；共享回归组 `test(=command::diff_test::test_diff_color_moved)`、`test(/^command::diff_test::/)` |
| LS-35 | #970; `diff.c:325-355`（Git）, `diff/render.rs:782` | `src/command/diff.rs`, `src/command/diff/render.rs` | `docs/commands/diff.md`、`docs/commands/zh-CN/diff.md`、`docs/development/commands/diff.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/diff.en.md` | `command::diff_test::color_moved_ws_allow_indentation_change` `(new)`；共享回归组 `test(/^command::diff_test::/)` |
| LS-22 | #970; `log.rs:172-486`, `diff/render.rs:782` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::color_moved_and_ws` `(new)`；共享回归组 `test(=command::log_test::test_log_patch_fails_when_commit_blob_is_missing) \| test(=command::log_test::test_log_patch_no_pathspec) \| test(=command::log_test::test_log_patch_with_pathspec) \| test(=command::log_test::test_log_patch_with_stat_shows_diffstat_before_patch)`、`test(/^command::diff_test::/)` |
| LS-23 | #857; `diff.rs:131-153`, `diff/options.rs:28` | `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs` | `docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md` | `command::show_test::patience_and_anchored` `(new)`；共享回归组 `test(/^command::show_test::/)`、`compat_diff_review_options` |
| LS-26 | #966; `diff.rs:170-193`, `diff/render.rs:61` | `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::word_diff_and_color_words` `(new)`；共享回归组 `test(/^command::diff_test::/)` |
| LS-33 | #966; `show.rs:73-187` | `src/command/show.rs`, `src/command/log.rs` | `docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md` | `command::show_test::word_diff_and_color_words` `(new)`；共享回归组 `test(/^command::show_test::/)` |
| LS-24 | #858; `show.rs:73-187` | `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs` | `docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md` | `command::show_test::color_moved_with_word_diff` `(new)`；共享回归组 `test(/^command::show_test::/)`、`test(/^command::diff_test::/)` |
| LS-25 | #862; `diff.rs:497-511,761-765,3403` | `src/command/show.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/options.rs` | `docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md` | `command::show_test::textconv_and_no_textconv` `(new)`、`command::show_test::textconv_failure_and_missing_command` `(new)`；共享回归组 `test(/^command::show_test::/)`、`test(/^command::diff_test::/)` |
| LS-27 | #973; `diff.rs:273,2228,2265` | `src/command/log.rs`, `src/command/diff.rs` | `docs/commands/log.md`、`docs/commands/zh-CN/log.md`、`docs/development/commands/log.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/log.en.md` | `command::log_test::diff_filter` `(new)`；共享回归组 `test(/^command::diff_test::/)` |
| LS-34 | #973; `show.rs:73-187` | `src/command/show.rs`, `src/command/log.rs` | `docs/commands/show.md`、`docs/commands/zh-CN/show.md`、`docs/development/commands/show.md`、`COMPATIBILITY.md`、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/show.en.md` | `command::show_test::diff_filter` `(new)`；共享回归组 `test(/^command::show_test::/)` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划评审通过且 `DEP-LS-01` 核实通过 | Claude & Codex `PASS` | N/A |
| M1 | Phase 0（`LS-01`..`LS-12`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 发布前 revert；发布后按 ER-10 新建 `FIX-*` 前滚发新版本 |
| M2 | Phase 1（`LS-13`..`LS-22`、`LS-28`..`LS-32`、`LS-35`..`LS-37`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 同上 |
| M3 | Phase 2（`LS-23`..`LS-27`、`LS-33`、`LS-34`）全部 `done`/`complete` + 全量收口门通过 | 全量 `nextest` + `fmt` + `clippy` 全绿 | 同上 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| `log --output=<file>` 写入中途失败 | 部分写入的目标文件 | 返回 `LBR-IO-002`，不重试。`LS-15` 发布后、`LS-37` 发布前：既有目标一律拒绝，部分写入的文件已存在，用户须确认后删除该文件或改用新目标再重跑；`LS-37` 发布后：重跑即按 Git `xfopen(path, "w")` 原地截断覆盖。`.libra/` 防护：路径预检在打开前完成（`LS-15` 起）；既有目标的描述符级 `(dev, inode)` 与硬链接核验在打开后、截断前完成（`LS-37` 起，既有目标以不截断方式打开）；任一不通过均在截断前拒绝，不会触及内部文件 | 静默成功（exit 0）、经路径/符号链接/硬链接改写 `.libra/` 内部文件 |
| 远端 D 组 CI 失败 | 已推送版本 tag 与提交 | 按 ER-04/ER-10 新建 `FIX-*` 前滚修复 | 回退已推送提交或删除已发布 tag |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `LS-16` 将 `--follow` 从带参改回布尔标志可能影响旧脚本 | 中：`--follow=<file>` 写法若被移除会在 patch 版本破坏脚本 | 保留已弃用的 `--follow=<FILE>` 写法并在 `COMPATIBILITY.md` 登记；同卡更新 `log_test` 与中英文文档 | LS-16 |
| `LS-12` `#969` 标题误称 `--reverse` 短别名但 Git `-R` 反转 diff 输入方向 | 低：若按标题误实现会与 Git 冲突 | 明确按 `git log -R` 实现并澄清关闭 #969 | LS-12 |
| `src/command/log.rs` / `src/command/diff/` 跨计划写锁冲突 | 高：合并冲突 | 每卡开工前重新核验 `DEP-LS-01` 与（若涉 `diff/`）`DEP-LS-02` | LS-01..LS-37 |
| 非 Unix 平台（如 `release.yml` 的 Windows 目标）上 `LS-37` 的描述符级 `.libra/` 防护与覆盖语义、`LS-15` 的新目标写入没有自动化用例（测试门只在 Unix 执行，相关用例以 `#[cfg(unix)]` 限定） | 中：Windows 上防护实现缺陷可能漏检 | 实现复用 `src/utils/beneath.rs` 的平台文件标识口径，取不到标识时对已存在目标 fail-closed 拒绝；路径级预检跨平台生效 | LS-15、LS-37 |
| `LS-15` 发布后、`LS-37` 发布前的一个版本内，`--output` 只写新目标，既有目标以 `LBR-IO-002` 拒绝 | 低：功能不完整但无数据风险（既有文件不被打开或截断）；`.libra/` 新路径防护与新目标创建失败诊断已随 `LS-15` 交付并验收 | `LS-37` 紧随 `LS-15` 发布，二者之间不插入其它切片 | LS-15、LS-37 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 历史遍历与 diff 渲染 | 每个提交至多加载一次：先由 `expand_excluded_commits` 展开排除端（`^A`、`A..B` 的 A）的完整祖先闭包（`src/command/log.rs:1564-1568,1691`，`walk_date_priority` 同样先展开，`1745`），再遍历正向起点，正向遍历以已访问集合 `commit_set` 去重（`1553-1576`）；每个被比较提交的 diff 成本与其变更路径数成正比 | `-n`/`--max-count` 只在 `eligible_date_priority_yield`（`src/command/log.rs:1813-1850`）判定为单一正向起点、无过滤的普通遍历时提前停止；`-L`、`--follow`、pathspec、`--find-object`、`--simplify-by-decoration`、`--diff-filter`、`--tags` 等过滤或多起点遍历走完全部可达历史，累积成本为 `O(正向起点可达集合 ∪ 排除端可达集合 的元素数)` 次提交加载（排除闭包即使不属于正向历史也须遍历）加被比较提交的 diff | 继承 GC-10：上界为正向起点与排除端各自可达集合之并（每个提交至多访问一次），不新增无界扫描或按提交的引用/配置 DB 查询（引用与配置每次调用只加载一次） | 测试矩阵 `M-PERF` 用例在深链/多提交夹具上完成；各卡 ER-05 评审核对新增遍历沿用已访问集合去重、未新增按提交的 DB 查询 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 第 196、199、227 行已按各卡第 1 步原地同步，或已记录「无需修改」的理由与未变 SHA-256。
- [ ] `docs/commands/{log,show,diff}.md` 已同步。
- [ ] `docs/commands/zh-CN/{log,show,diff}.md` 已同步。
- [ ] `docs/development/commands/{log,show,diff}.md` 已按各卡第 1 步逐文件语义核对：受影响者已同卡同步，其余已记录「无需修改」的理由与未变 SHA-256。
- [ ] `/Volumes/Data/gitmono/libra-backend` 的 `log`/`show`/`diff` 网站页已按各卡「文档与网站验收命令」第 3 步（`--write --only <cmd…>`，命令以空格分隔）同步，且 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --libra-root /Volumes/Data/gitmono/libra --only log show diff` 退出码为 0。
- [ ] `docs/error-codes.md`：`N/A`（复用现有 `LBR-CLI-002`/`003`、`LBR-IO-001`/`002`）。
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
| R17 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `129d461e8719831771095fffc467401df5d39e65cc0cdb480f5bfcd7bf7010ff` | `FAIL` | P1：悬空「文档与网站验收命令」、验证协议与回归门矛盾、断言映射机械矛盾、LS-02/03/06/14/25 与 Git 不符、AC 合并计数、LS-26/27 双命令、网站路径误归因与历史倒改、未来时间戳、GC-05 未含 `diff`、LS-12 违背 ADR-LS-01、追溯表丢失卡特有测试、LS-16 patch 破坏性变更 | P2：Verification 前缺空行、测试矩阵缺 `compat_config_defaults_semantics`、文头状态过期、landing 按文件计（均已修复） | 2026-11-18 10:39:24 UTC 修订 |
| R17 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `129d461e8719831771095fffc467401df5d39e65cc0cdb480f5bfcd7bf7010ff` | `FAIL` | P1：文档验收步骤悬空且网站页未逐文件列出、回归门与精确单命中协议冲突、LS-26/27 并集门、断言映射与 AC 矛盾、AC 合并计数（LS-17/26）、LS-26/27 双公开命令、网站路径误归因与历史倒改、未来时间且顺序倒置、GC-05 与 LS-31 不一致及 `--only` 语法 | 无 | 2026-11-18 10:39:24 UTC 修订 |
| R18 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `f1841e1cf0e4b598dde37214a732f91ab35b5296eb641f46af507d35b97b52e8` | `FAIL` | P1：LS-18/LS-27 夹具删除未创建的 `b.txt` | P2：GAP-LS-21/22 计划动作未列拆出卡、关系表缺第 227 行、测试矩阵单元行子串过滤、LS-12 夹具未给内容、性能摘要验证含糊（均已修复） | 2026-11-18 10:48:35 UTC 修订 |
| R18 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `f1841e1cf0e4b598dde37214a732f91ab35b5296eb641f46af507d35b97b52e8` | `FAIL` | P1：最终树验收自引用循环、网站无变更通过路径与完成判据互斥、测试矩阵 `\|` 不可复制、LS-26/33 夹具无父提交、AC 仍合并谓词（LS-31/26/33）、LS-02 映射不足以验证 short 样式、LS-27 夹具缺 `b.txt`、以 mtime 上界替代实际时间、网站路径替代授权无法自证 | 无 | 2026-11-18 10:48:35 UTC 修订 |
| R19 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `ac499ff03e88b9981c8561a82647b39a62f11dc6038e72e4ab2b11f21718ca7d` | `FAIL` | P1：LS-15 AC7/AC8 以「且」合并谓词致计数超限、LS-35 AC1 期望附带第二谓词、LS-16 AC5 合并谓词 | P2：LS-35 重复回归门（已修复） | 2026-11-18 10:54:22 UTC 修订 |
| R19 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `ac499ff03e88b9981c8561a82647b39a62f11dc6038e72e4ab2b11f21718ca7d` | `FAIL` | P1：网站无变化分支不能证明远端/生产一致、`DEP-LS-03` 确认晚于版本发布、LS-15 AC 分子低估、部署门对仅删除/重排变化不可执行、LS-31 未验收 `no` 清除先前模式 | 无 | 2026-11-18 10:54:22 UTC 修订 |
| R20 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `f68d318b14d9f21a9df093ddc198329e8ead7193c40643d53468b1f8481600d0` | `FAIL` | P1（与 Codex 交叉核对后确认）：LS-31 AC1/AC7 合并多变体、删除型部署门受管道掩盖、LS-15 `.libra/` 防护只比路径集合、断言映射对 Libra 专有行为套用 Git 期望规则、无变化分支生产证据不足 | P2：第 3 步第二条命令依赖前一 `cd`（已修复） | 2026-11-18 10:59:10 UTC 修订 |
| R20 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `f68d318b14d9f21a9df093ddc198329e8ead7193c40643d53468b1f8481600d0` | `FAIL` | P1：漂移检查缺 `--libra-root`（核对不成立：两条命令均已携带，仍补全 `cd`）、无变化分支不能证明生产部署、删除型部署门可能把网络错误判为通过、LS-31 AC 合并谓词、Git 期望协议与 Libra 专有 JSON 验收冲突、LS-15 `.libra/` 验收不能发现覆盖与符号链接 | 无 | 2026-11-18 10:59:10 UTC 修订 |
| R21 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `39166b226ced63b05f14864d639f30f5c3fb96bd124588b31184562992268bf8` | `FAIL` | P1（交叉核对后确认并扩展）：`LS-15` 先于防护发布 `--output`、网站推送可能夹带无关提交或暂存、空输出可通过的断言（LS-27，扩展到 LS-09/17/19/20/29，LS-09 夹具不自洽） | 无 | 2026-11-18 11:03:46 UTC 修订 |
| R21 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `39166b226ced63b05f14864d639f30f5c3fb96bd124588b31184562992268bf8` | `FAIL` | P1：`LS-15` 先于 `.libra/` 防护发布、网站推送可能发布无关提交或已暂存文件、`LS-27` 断言不足以证明 AC | 无 | 2026-11-18 11:03:46 UTC 修订 |
| R22 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `522d3e84c2e5415fd4939e46abe9edcde7c54bf4e9a876ad7aa19badeec94d2b` | `FAIL` | P1（交叉核对后确认）：`LS-15` 硬链接可绕过防护、`LS-02`/`LS-06` stdout 与 stderr 合并计数、`LS-09` 映射只查部分行、`LS-11` 错误等价未断言、无变化网站分支缺生产证据、`LS-25` 标记文件与只读不变量冲突 | 无 | 2026-11-18 11:07:47 UTC 修订 |
| R22 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `522d3e84c2e5415fd4939e46abe9edcde7c54bf4e9a876ad7aa19badeec94d2b` | `FAIL` | P1：同左六项 | 无 | 2026-11-18 11:07:47 UTC 修订 |
| R23 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `ac847b8a6cbdcfbdf22a956d1b4c0bc6481d7f7d50cdfc4a0a64f42fc0aab5a4` | `FAIL` | P1（交叉核对后确认）：`LS-34` 使用不存在的 `show -p`、`LS-25` 过滤器写入与只读协议矛盾、无变化网站分支的删除型指纹可误判旧部署、`show-ref` 不覆盖 notes/replace 等引用 | 无 | 2026-11-18 11:11:05 UTC 修订 |
| R23 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `ac847b8a6cbdcfbdf22a956d1b4c0bc6481d7f7d50cdfc4a0a64f42fc0aab5a4` | `FAIL` | P1：同左四项 | 无 | 2026-11-18 11:11:05 UTC 修订 |
| R24 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `7dbacd7d6ce408ecd89f592a194bf5eb9a33ec5011567292f120578252ad0bb0` | `FAIL` | P1：期望中「X 且 Y」复合判定（LS-12/17/19/20/34，独立发现）；交叉核对确认 LS-11 计数、回归用例与引用快照协议冲突、`libra log -S` 指纹可能无解、逐文件关键字强制修改 | 无 | 2026-11-18 11:16:10 UTC 修订 |
| R24 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `7dbacd7d6ce408ecd89f592a194bf5eb9a33ec5011567292f120578252ad0bb0` | `FAIL` | P1：LS-11 AC 计数遗漏、共享回归用例未覆盖引用快照、部署指纹门可能无解、逐文件强制关键字 | 无 | 2026-11-18 11:16:10 UTC 修订 |
| R25 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `ef8c58e6889c250dc2d1e7fdf27c535b3cf34585d1627ad7e48b036bca20480c` | `FAIL` | P1（交叉核对后确认）：正文提取失败被管道掩盖、预览基准含未提交改动、`LS-28` AC 计数、`LS-18` 语义过窄 | 无 | 2026-11-18 11:19:11 UTC 修订 |
| R25 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `ef8c58e6889c250dc2d1e7fdf27c535b3cf34585d1627ad7e48b036bca20480c` | `FAIL` | P1：同左四项 | 无 | 2026-11-18 11:19:11 UTC 修订 |
| R26 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `dfc6ba4d7efd7dbe11bf15f46d1a0be5afa88784f13f837804ef38989dc3da8c` | `FAIL` | P1（交叉核对后确认）：`COMPATIBILITY.md` 行定位不唯一（另一表同名行；双反斜杠说法不成立）、词级 diff 空输出相等且原夹具正则不可区分、`--output` 原子替换与 Git 原地截断语义不同 | 无 | 2026-11-18 11:23:10 UTC 修订 |
| R26 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `dfc6ba4d7efd7dbe11bf15f46d1a0be5afa88784f13f837804ef38989dc3da8c` | `FAIL` | P1：兼容矩阵行定位命令、词级 diff 空输出相等、`--output` 原子替换的未登记 Git 差异 | 无 | 2026-11-18 11:23:10 UTC 修订 |
| R27 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `59c28a03e5a3564334d479889ece31e45bd379663797832371833c3ceb7557f3` | `FAIL` | P1（交叉核对后确认）：`LS-37` 写入与协议冲突、`LS-15` 检查后重开竞态、行为修复卡允许不更新命令文档、`LS-15` 分页器要求无验收 | P2：事实基线版本说明过期（已修复） | 2026-11-18 11:27:10 UTC 修订 |
| R27 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `59c28a03e5a3564334d479889ece31e45bd379663797832371833c3ceb7557f3` | `FAIL` | P1：同左四项 | 无 | 2026-11-18 11:27:10 UTC 修订 |
| R28 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `dc21ba9f1954a715e471a6689fc147723811d2878ed6abda93523fc64b998415` | `PASS` | 无 P0/P1（逐字节核对 Codex 两项转义问题不成立） | P2：行内长命令易被转义视图误读，已移入命令块 W-1/W-2 | 2026-11-18 11:31:01 UTC 修订 |
| R28 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `dc21ba9f1954a715e471a6689fc147723811d2878ed6abda93523fc64b998415` | `FAIL` | P1：兼容矩阵定位命令双反斜杠、正文提取命令转义——经逐字节核对均不成立（文件为单反斜杠与普通引号，命令实测可用），仍以命令块消除歧义 | 无 | 2026-11-18 11:31:01 UTC 修订 |
| R29 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `951238f55a5183f7dc94efad0ab50b553e540254c6bf3d7fac0349ddb2c21c9b` | `PASS` | 无 P0/P1（W-1/W-2 自计划抽取实测通过） | Codex 两项意见作为加固采纳 | 2026-11-18 11:34:27 UTC 修订 |
| R29 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `951238f55a5183f7dc94efad0ab50b553e540254c6bf3d7fac0349ddb2c21c9b` | `FAIL` | P1：`mktemp` 响应文件与 v2.12 冲突、干净性门未覆盖忽略文件与构建环境 | 无 | 2026-11-18 11:34:27 UTC 修订 |
| R30 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `04db9392d94ce4495d256bacb9b8103a9d2d424bdf7be85efac5ec81730568b0` | `FAIL` | P1（交叉核对后确认）：`LS-03` 把空值当作 Git 无值键、网站无变化分支与强制更新矛盾 | P2：兼容收口条目与「无需修改」口径不一致（已修复） | 2026-11-18 11:37:51 UTC 修订 |
| R30 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `04db9392d94ce4495d256bacb9b8103a9d2d424bdf7be85efac5ec81730568b0` | `FAIL` | P1：`LS-03` 无值键与空值混淆、网站无变化通过分支与强制变更门矛盾 | 无 | 2026-11-18 11:37:51 UTC 修订 |
| R31 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `4d05f3bd2d10800509f5e1fc02b78d79562286ad5eec287526f1341a28b9d8d3` | `PASS` | 无 P0/P1（计数/锚点/表格/命令块/Git 与 Libra 预探复核） | P2：网站步骤依赖本地 `pnpm preview:cf` 与 `DEP-LS-03`（均 fail-closed）、竞态回归延后（`DEFER-LS-02`） | 本轮 PASS；Codex 意见已修订待复审 |
| R31 | Codex (`gpt-6.1-terra`, high) | 全文，受审 SHA-256 `4d05f3bd2d10800509f5e1fc02b78d79562286ad5eec287526f1341a28b9d8d3` | `FAIL` | P1：`LS-36` 修改前序用例与「共享回归组按原样运行」矛盾；VER 分子未计入 `nextest list` 预检门 | 无 | 2026-11-18 11:41:42 UTC 修订（未评审） |
| R32 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `c916505a9a8dbdea6e91090a927d691379fe66d6d09d93cbb29b78a695157935` | `FAIL` | P1：`LS-36` 例外引用第 5 项且未在 `Current evidence` 记录被删断言；`LS-16` 写集漏 `src/command/log/config.rs`（`config.rs:38`）；事实基线 `src/cli.rs:3481,3845,3898` 与 `Log`/`Show` 无关；`LS-04` AC2 合并两个选项；交叉核对确认 Codex 分区 A/B/C 全部 P1 | P2：`LS-37`/`LS-25` 否定断言缺正向对照（已补 AC）、`LS-16` 触发字段嵌套反引号（已修）、`LS-22` 空白模式夹具（残余风险） | 2026-11-18 11:55:13 UTC 修订 |
| R32 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `c916505a9a8dbdea6e91090a927d691379fe66d6d09d93cbb29b78a695157935` | `FAIL` | A：GC-05 与第 1 步「无需修改」矛盾、W-2 规范化文本会把不同部署判为相同、`DEP-LS-03` 未确认时 `remote-pending` 违反 ER-04 状态机、恢复矩阵防护时序与 `LS-15` 矛盾；B：`LS-14` 标题与 AC 矛盾、证据锚点只指参数声明；C：`LS-36` 例外引用错项、`Current evidence` 未记录被删断言；D：无 | D：`LS-22` 空白模式夹具、`LS-25` 缺过滤器执行正向断言（后者已补 AC） | 2026-11-18 11:55:13 UTC 修订 |
| R33 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `6bc1a31b1cb60e66cf2890bf765546801ddd6ee98214f704e8442e42f5aaa1c7` | `PASS` | 无 P0/P1（锚点逐行转储核对、W-1/W-2 实测、Git 2.54.0 与 Libra 预探、计数与表格脚本复核） | P2：`LS-22` 空白模式夹具（本轮一并修订） | 交叉核对 Codex R33 后采纳其 P1 |
| R33 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `6bc1a31b1cb60e66cf2890bf765546801ddd6ee98214f704e8442e42f5aaa1c7` | `FAIL` | A：评审结论称每卡 `T-1`/`T-3`；B：`LS-01` 映射额外 stderr 判据、`LS-09` 映射额外行数/字段数判据、`LS-10` AC6 退出码与稳定码合并；C：`LS-15` 跟随符号链接与 `create_new` 不跟随最终符号链接矛盾（悬空链接）；D：无 | B：`LS-10` 提交时间未固定；D：`LS-22`/`LS-24` 空白模式夹具（均已修订） | 2026-11-18 12:03:02 UTC 修订 |
| R34 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `811090a6393c58fcbbaf3c19e6a8b17dc419dd4efb5b5cf61f47676fdaccf1eb` | `PASS` | 无 P0/P1（新映射计数口径逐条脚本复核、LS-09/15/29/37 的 Libra 与 Git 2.54.0 预探） | P2：`LS-17` AC5 映射措辞（本轮修订） | 交叉核对 Codex R34 后采纳其 P1 |
| R34 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `811090a6393c58fcbbaf3c19e6a8b17dc419dd4efb5b5cf61f47676fdaccf1eb` | `FAIL` | A：网站第 2 步先要求 `cf` 后才允许切换、第 6 步远端前进后从第 2 步重来必然因分叉 STOP；B：`LS-05`「只选出被装饰提交」与 Git 不符、`LS-10` AC4 文本与映射不一致、`LS-11` AC3 映射加入固定退出码；C：`LS-37` 分页器对照在非 Unix 平台不成立；D：无 | D：`LS-23` 锚点夹具（已修订） | 2026-11-18 12:06:47 UTC 修订 |
| R35 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `f960a35a9addf41b44b93fa52a4fb6349e3a4a45cb6a4a109a2b0354d214f1b0` | `PASS` | 无 P0/P1（网站第 2/6 步、LS-05 Git 文档、平台范围锚点复核） | P2：第 6 步恢复中快进被其它改动阻止时未明写 STOP | 交叉核对 Codex R35 后采纳其 P1（`LS-06` 夹具经 libra 预探确认不可行） |
| R35 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `f960a35a9addf41b44b93fa52a4fb6349e3a4a45cb6a4a109a2b0354d214f1b0` | `FAIL` | A：第 2 项「恰为 1 个」与共享回归组 N≥1 冲突、映射口径把可独立失败的退出码与稳定码计为一条；B：`LS-06` 候选夹具 `libra notes` 不生成引用；C：`LS-16` AC1/`LS-17` AC2 映射严于 AC、`LS-30` 未验证与 `libra diff` 一致、`LS-17` AC5 被 AC4 蕴含；D：`LS-27` AC1 与映射谓词不同 | 无 | 2026-11-18 12:13:11 UTC 修订 |
| R36 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `5dd4aeac43fffa952c0ac3659933ab6899c379fd33193aa416b13538cb9be485` | `FAIL` | P1：`LS-06` 夹具的 `refs/notes/commits` 行（`kind = 'Branch'`）当前即被当作本地分支装饰，「R 出现」类 AC 空过且无默认排除断言（Git 2.54.0 同构预探） | 无 | 2026-11-18 12:16:31 UTC 修订 |
| R36 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `5dd4aeac43fffa952c0ac3659933ab6899c379fd33193aa416b13538cb9be485` | `FAIL` | A：引用快照不含 `notes` 表、`LS-36` 被删断言记录仍含 `exit 129`；B：`LS-07` tag 着色断言与 Git 分段 SGR 不符；C：同 A 的 `LS-36`、`LS-21` 映射未覆盖长选项拼写；D：`LS-34` AC1 与映射的行尾换行不一致 | 无 | 2026-11-18 12:16:31 UTC 修订 |
| R37 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `e929be6b198d95c93f50b8b585ef4938dbde903ce25f7329721b41532f097140` | `PASS` | 无 P0/P1（AC/映射字面量脚本核对、LS-06/07 Git 同构预探与源码复核） | P2：网站第 6 步恢复未重述快进受阻即 STOP | 交叉核对 Codex R37 后采纳其 P1 |
| R37 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `e929be6b198d95c93f50b8b585ef4938dbde903ce25f7329721b41532f097140` | `FAIL`（A `PASS`） | B：`LS-06` AC5 映射比较整段 stdout 而 AC 只说装饰；C：`LS-19`/`LS-20` 映射比较整段 stdout 而 AC 只说局部输出；D：`LS-35` 无条件拒绝组合与 Git `no` 清除语义不符 | B：`LS-09` 展开基准可能空过（登记为残余风险）；C：`LS-21` 基线含义（已修订） | 2026-11-18 12:19:56 UTC 修订 |
| R38 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `3ae0ad4bff14ed2bd0517a2604bb17b8b745f60816e7c7f2a99dc033a827c244` | `PASS` | 无 P0/P1（AC 与映射比较范围脚本核对、LS-35 有序解析 Git 预探） | P2：`LS-06` AC1–4 未重述完整命令、`LS-09` 展开基准、网站第 6 步恢复未重述 STOP | 交叉核对 Codex R38 后采纳其 P1，并据 `builtin/log.c:388` 删除与 Git 不符的「不启动分页器」要求 |
| R38 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `3ae0ad4bff14ed2bd0517a2604bb17b8b745f60816e7c7f2a99dc033a827c244` | `FAIL`（B `PASS`） | A：性能摘要称 `-n` 约束遍历与 `eligible_date_priority_yield` 不符；C：`LS-15` 承诺的 inode/权限保留延到 `LS-37` 验收（G-02）；D：`LS-33` 重启条件引用 `LS-26`（G-05） | B：`LS-02` 未验收 `--format=raw`（已补）、`LS-09` 展开基准可能空过（残余风险） | 2026-11-18 12:27:37 UTC 修订 |
| R39 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `d5a166ccca44d9aac1a6edb52ae1bbd2d2f72d677ed865cb7eed413594970ca8` | `PASS` | 无 P0/P1（`LS-15`/`LS-37` 重切后的验收覆盖、`builtin/log.c:388`、性能锚点复核） | P2：`LS-15` 分页器策略无专门 AC、`LS-09` 展开基准、`LS-06` 命令隐含于夹具 | 交叉核对 Codex R39 后采纳其 P1 |
| R39 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `d5a166ccca44d9aac1a6edb52ae1bbd2d2f72d677ed865cb7eed413594970ca8` | `FAIL`（B `PASS`） | A：故障恢复矩阵仍按覆盖语义要求重跑、性能上界漏计排除端闭包；C：`LS-15` 称 Git 无条件启动分页器；D：`LS-25` 把退出码无条件写成 128 | B：`LS-10` 输入顺序不能区分 sorted、`LS-11` 未证明两次调用失败（均已修订） | 2026-11-18 12:31:58 UTC 修订 |
| R40 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `a35b9d89fd28146ef774a68875e3ef683953c8f9992950dce2794d1cc06919b3` | `PASS` | 无 P0/P1（恢复矩阵、性能锚点、`pager.c` `setup_pager`、`error.rs` 退出码路径复核） | P2：`LS-09` 展开基准、`LS-06` 命令隐含于夹具 | 交叉核对 Codex R40 后采纳其 P1 |
| R40 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `a35b9d89fd28146ef774a68875e3ef683953c8f9992950dce2794d1cc06919b3` | `FAIL`（B/C/D `PASS`） | A：协议第 6 项以 `LS-11` AC4（退出码相等）为数值退出码断言示例，但该 AC 未断言数值 | B：`LS-01` 深链标签子串边界（已修订）；C：`LS-15` 分页器策略无专门 AC（残余风险，`LS-15` 已 8/8） | 2026-11-18 12:34:39 UTC 修订 |
| R41 | Claude subagent (`claude-opus-5-5`) | 全文，受审 SHA-256 `859bb860dec875ebc7889a82e042146bfc8d749df10782e60f8055f90caafdd6` | `PASS` | 无 P0/P1 | P2 残余风险：`LS-15` 分页器策略无专门 AC（与 Git 一致的既有策略，`LS-15` 已 8/8）、`LS-09` 展开基准取自 Libra 默认缩写输出、`LS-06` AC1–AC4 的命令由夹具隐含、网站第 6 步恢复未重述快进受阻即 STOP | 双审 PASS，计划可提交 |
| R41 | Codex (`gpt-6.1-terra`, high；分区 A/B/C/D) | 全文分四区，受审 SHA-256 `859bb860dec875ebc7889a82e042146bfc8d749df10782e60f8055f90caafdd6` | `PASS`（A/B/C/D 均 PASS） | 无 | C：`LS-15` 分页器策略无专门 AC（同左，登记为残余风险） | 双审 PASS，计划可提交 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-LS-01 | `log --expand-tabs` / `--mailmap` / `--show-signature` | 不在本次 27 项 issue 范围内 | 后续新 issue 需求 | `COMPATIBILITY.md` |
| DEFER-LS-02 | `log --output` 在路径预检与打开之间被并发替换父目录/目标的竞态回归用例 | 确定性复现需新增测试注入点；同用户并发篡改者本可直接写 `.libra/`，超出 `LS-37` 威胁模型（误指定的静态目标）；描述符级 `(dev, inode)` 核验已防止已存在目标在检查后被替换 | 需要防御非同用户攻击者，或 Libra 提供通用测试注入点时 | 新计划或 `LS-37` 后续 `FIX-*` |

## 完成判据

- [ ] 全量收口门须在**最终代码树**上执行，并记录被测提交的 `libra rev-parse HEAD` 与 `libra rev-parse HEAD^{tree}` 及结果。「代码树」指 `docs/development/plan/` 以外的全部路径：其后若 `libra diff --name-only <被测提交> HEAD` 出现该目录以外的任何路径即须重跑；只改 `docs/development/plan/` 下计划/状态文件的纯文档记账提交（含把本门结果写入本计划的提交）不视为代码树变化、不触发重跑。终止链末端提交例外（v2.14，预登记）：本计划最终的纯文档记账提交的 D 组结果在推送后取得，不在该提交自身登记，其 SHA、run ID、结论与观察 UTC 时间由下一次 `plan-status.md` 例行同步补记。
- [ ] 计划文本提交门：Claude（Subagent）与 Codex 双审对同一受审字节均返回字面结论 `VERDICT: PASS` 且 P0/P1 全部关闭；网站交付另需独立 Claude Code `VERDICT: PASS`，两者互不替代。
- [ ] 网站交付门：`DEP-LS-03`（用户 2026-11-18 11:55:09 UTC 确认）仍有效；每张卡按「文档与网站验收命令」完成网站门——每张卡都完成第 3–7 步（同步与漂移检查且网站页字节改变、前端验证、独立 Claude Code `PASS`、精确暂存与签名+DCO 提交、提交字节与已审字节一致、推送 `cf`、`<article>` HTML 逐字节比对的部署核验）；计划文本双审不替代该网站门，任一环节失败即 STOP 且不得标 `complete`。
- [ ] 所有任务卡满足粒度规则 `G-*`；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）。
- [ ] 全部任务卡完成后运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿，且修复全量暴露的全部 Bug（ER-13）。
- [ ] Claude 与 Codex 最终评审结论均为 `PASS`，P0/P1 全部关闭。
- [ ] 每张 `independent` 发布卡已执行 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新、提交推送及 `gh release create` 完成并取得 D 组证据（ER-08）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
