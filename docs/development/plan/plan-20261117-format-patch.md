# format-patch Git 兼容补齐计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：Claude R41 与 Codex R41 双审 PASS（受审 SHA-256 前 12 位 `7249da6bea02`），计划可提交；全部任务 pending，尚未执行；记录时间 2026-11-18 14:13:26 UTC。
> 审阅合同：按用户 2026-11-18 指令，本计划的计划文本提交门为 Claude（Subagent）与 Codex 双审，二者须对同一受审字节（同一 SHA-256）均给出字面 `VERDICT: PASS` 方可提交（取代 2026-11-17 的 Codex 单审口径）；网站交付仍须独立的 Claude Code `PASS`，与计划文本双审不可互相替代。
> 跟踪范围：GitHub open issues #880–#891、#892–#901、#903–#906、#908–#912、#914–#917（共 35 项）。

## 文档职责

- **网站交付路径（权威）:** 唯一生产网站 checkout 为 `/Volumes/Data/gitmono/libra-backend`（`cf` 分支）。依据：用户 2026-11-18 11:55:09 UTC 确认本批计划的网站文档交付检出为该路径（`cf` 生产分支）。仓库 `AGENTS.md:135` 仍写 `/Volumes/Sea/libra-backend`（本机不存在），登记为已知文档滞后，本计划不改 `AGENTS.md`。写入前须核验该路径存在、仅含单一 VCS 元数据且分支为 `cf`；任一不满足即 STOP，禁止改用其它检出。

本文解决 `libra format-patch` 与 Git `git format-patch`（基线 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`，v2.56.0-116，t4014/t4015/t4021）之间的 35 项已登记兼容缺口，目标是使 `format-patch` 在序列号格式、邮件头编码与折叠、配置项级联、封面信、文件名生成、diff 选项透传、`--base=auto`、`--interdiff`、`--relative` 与线程化头等方面达到与 Git 一致的 CLI 与 mbox 输出契约。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收（GC-01）。

### 适用范围

- 命令与 CLI：`libra format-patch` 参数解析、校验与执行（`src/command/format_patch.rs`、`src/cli.rs`、`src/command/log.rs`、`src/command/diff/`）。
- 配置与环境变量：`format.numbered`、`format.headers`、`format.to`、`format.cc`、`format.notes`、`format.signature`、`format.signatureFile`、`format.from`、`format.coverLetter`、`format.coverFromDescription`、`format.commitListFormat`、`format.encodeEmailHeaders`、`format.attach`、`format.thread`、`format.useAutoBase`、`format.filenameMaxLength`、`diff.relative`、`branch.<name>.description`、`GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`。
- 测试与文档：`tests/command/format_patch_test.rs`（`command_test` 目标）、`tests/compat/format_patch_mail_roundtrip_test.rs`、`tests/compat/format_patch_flag_surface.rs`、`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md`（第 224 行原地更新，不增删行）及通过 `cd /Volumes/Data/gitmono/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra` 同步的 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`。

### 非目标

- 不实现邮件发送能力（`libra send-email` 仍按 `COMPATIBILITY.md` 保持 `unsupported`）。
- 不实现 `--range-diff`（仅实现 #898 要求的 `--interdiff`）。
- 不重复实现 `issues/529.md` 已承接并交付的能力（如 `-N` 文件名、`09/10` 主题计数宽度、`--numstat`、`--output`、`-M`/`--no-renames`）；对 #880、#881、#888、#899、#904、#909 中与 `529.md` 重叠的部分，按 GC-01 在前置依赖完成后核实并只补齐剩余缺口。
- 不更改 `libra am` / `libra mailinfo` 的既有解析契约。

### 成功定义

- 35 个关联 issue（#880–#917 指定集合）全部在「追溯表」中闭环：`#880` 由 `issues/529#FP-02`/`FP-04` 交付并按 529 的 `DEP-FP-07` 关闭（本计划经 `DEP-FPG-01` 等待，不重复验收），`#888` 第 1–4 项与 `#898` 第 3 项的拒绝契约分别由 `issues/529#FP-03`、`FP-10` 交付并在 529 计划内验收（本计划在 FPG-09、FPG-19 登记并在共享回归组中运行其门族）；其余验收项由本计划任务卡的 AC 与边界测试闭环；唯一例外是 `#905` 第 1 项中封面信 diffstat 的 `rename … (100%)` 摘要行：它由共享 stat 渲染器产生，随 529 `DEFER-FP-02` 登记为 `DEFER-FPG-03`，`#905` 在该项落地前只部分闭环、保持 open（其余各项由 `FPG-58`、`FPG-13`、`FPG-39` 闭环）。
- `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md` 与后端网站文档同步反映新增 flags 与 config。
- 全部任务卡完成后通过 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 及 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

## 使用规则

- 每次状态推进与「修订历史」写入都记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时间。
- 推进或完成任务卡时，执行者须在同一变更中同步更新 [`plan-status.md`](plan-status.md)（本计划入库时的初始登记行由协调者随本批计划合入一并登记）。
- 「任务卡」一节的卡片正文按「实施顺序」排列（拆出卡按其执行位置插入）；G-09「在所属 Phase 末尾追加新编号」指新卡编号取当时最大编号之后、不重排既有编号，不约束正文位置。
- 本文全部任务卡均未开工；任务 ID（`FPG-01`..`FPG-64`）固定，拆分时按 G-09 在所属 Phase 末尾追加新编号并登记「修订历史」。
- 本计划不单独新建证据目录或证据文件（v2.12），所有门禁结果、SHA 与测试摘要写在本计划内。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。以下锚点核对于 `2026-11-17 03:08:00 UTC`（`Cargo.toml` = `0.30.207`）。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `FormatPatchArgs`（97-364）、`validate_cli_args`（437-458）、`execute_safe`（474-628）、`resolve_range_commits`（663-756）、`format_patch_body`（782-1002）、`format_cover_letter`（1361-1445）、`resolve_format_config`（1861-1902） | `src/command/format_patch.rs:97-2091` |
| 数据/状态 | 只读 commit/tree/blob/notes/config，不修改 SQLite/HEAD/index；非 `--stdout` 写补丁文件采用单文件 `atomic_write`（后序文件失败会保留已写入的前序文件） | `src/command/format_patch.rs:596-619,1824-1828` |
| 用户命令 | `Commands::FormatPatch(command::format_patch::FormatPatchArgs)`、`validate_cli_args` 预检、分发到 `execute_safe` | `src/cli.rs:496-500,3662-3663,3952-3953` |
| 机器输出 | `--json` 输出 `FormatPatchOutput { patches: Vec<PatchRecord> }`（字段 `number`/`commit`/`subject`/`path`）；空系列输出 `patches` 为空数组的对象 | `src/command/format_patch.rs:520-529,621-625,634-645` |
| 错误码 | `FormatPatchError` 映射 `LBR-REPO-001`、`LBR-CLI-002`、`LBR-CLI-003`、`LBR-IO-001`/`002`、`LBR-UNSUPPORTED-001` | `src/command/format_patch.rs:370-423,500-513`、`docs/error-codes.md` |
| 文档 | 中英文命令页、开发文档及兼容矩阵第 224 行已存在（`tests/compat-ledger/t4/` 锚定 `COMPATIBILITY.md` 行号，仅可原地修改第 224 行；后端 `/Volumes/Data/gitmono/libra-backend` 为 Libra 仓库，分支 `cf`，在 `/Volumes/Data/gitmono/libra-backend` 执行 `python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra` 同步） | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224` |
| 测试 | `tests/command/format_patch_test.rs` 挂载为 `command_test` 模块；compat 独立 target：`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface`、`compat_broken_pipe_output` | `tests/command_test.rs:10`、`tests/command/mod.rs:1050`、`Cargo.toml:343,347,525`、`tests/INDEX.md:87,89,181` |
| 外部参照 | 上游 Git 测试树固定 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0-116，t4014/t4015/t4021）；在途同域计划 `issues/529.md`（`FP-02` done，`FP-03` remote-pending，`FP-04..13` pending） | `docs/development/plan/issues/529.md:3,107`（2026-11-17 核对） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-FPG-01 | `#896`：多补丁默认不编号且忽略 `format.numbered`（`#880` 的计数宽度与前缀组合由 `issues/529#FP-02`/`FP-04` 交付并关闭） | 系列补丁缺少 `1/N` 默认编号 | `src/command/format_patch.rs:855-861,1861-1902` | FPG-01、FPG-32、FPG-56 |
| GAP-FPG-02 | `#881`：`resolve_signoff_identity` 忽略 `GIT_COMMITTER_*`（identity 由 `529 FP-11` ADR-FP-07 承接；t4036 端到端另依赖 `#897` MIME 头） | 环境变量身份验证与 t4036 兼容闭环未完成 | `src/command/format_patch.rs:2050-2065`、`issues/529.md:179` | FPG-02、FPG-53（身份）、FPG-30、FPG-51（#881 完整闭环） |
| GAP-FPG-03 | `#882`：`format-patch --ignore-if-in-upstream` 在 t4014#4 带 tag 场景多输出 1 个补丁（报告于 0.30.91；2026-11-18 以 0.30.207 按 t4014 setup 复现时输出 2 个补丁、与 Git 一致，`DEP-FPG-01` 解除后复核）；另：上游侧 patch-id 取排除端的全部祖先，而 Git 只取 `b..a`（自上游可达、自 topic 不可达的提交） | 上游已合入提交未被正确过滤；与 merge base 之前某提交 patch-id 相同的补丁被误删 | `src/command/format_patch.rs:688-750,1126-1213` | FPG-03 |
| GAP-FPG-04 | `#883`/`#914`：忽略 `format.headers`/`format.to`/`format.cc`，不支持 `--add-header`/`--no-add-header`，且 `--no-to`/`--no-cc` 抹除后续 `--to`/`--cc` | 无法通过配置或命令行注入邮件头，同名头不折叠，CLI 顺序覆盖不符 Git | `src/command/format_patch.rs:194-210,1861-1935` | FPG-04、FPG-44、FPG-37、FPG-57 |
| GAP-FPG-05 | `#884`：缺少 `--no-notes` 且忽略 `format.notes` | 无法通过配置启用或 `--no-notes` 关闭 notes | `src/command/format_patch.rs:236-240,1012-1057,1861` | FPG-05、FPG-36、FPG-41 |
| GAP-FPG-06 | `#885`：默认 signature 省略产品前缀 `libra ` 且忽略 `format.signature` | 签名页脚不符合预期且不可配置 | `src/command/format_patch.rs:1062-1071,1861` | FPG-54、FPG-06、FPG-55 |
| GAP-FPG-07 | `#911`：`commit_subject_line` 仅取首行而不拼接首段 | 多行首段被截断 | `src/command/format_patch.rs:833,1497-1500` | FPG-07 |
| GAP-FPG-08 | `#887`：默认不编码非 ASCII 邮件头、忽略 `format.encodeEmailHeaders`，空格编为 `_` 而非 Git 的 `=20`（#911） | 与 Git 默认 `--encode-email-headers` 不符 | `src/command/format_patch.rs:345-358,1981-2018` | FPG-08 |
| GAP-FPG-09 | `#888`：第 1–4 项已由 `529 FP-03` 落地 `validate_cli_args` 与拒绝门族；第 5 项（单独 `-k`/`--subject-prefix`/`--rfc` 成功路径）的既有用例在输出为空时也会通过，需补强后闭环 | 需确认无遗留 `-k` 与 `--subject-prefix`/`--rfc` 边界差异 | `src/command/format_patch.rs:437-458`、`tests/command/format_patch_test.rs:2589` | FPG-09 |
| GAP-FPG-10 | `#889`：`parse_from_ident` 接受无邮箱字符串输出 `From: foo <>` | 产生非法 `From:` 邮件头 | `src/command/format_patch.rs:2022-2045` | FPG-10 |
| GAP-FPG-11 | `#890`：`--relative` 硬编码拒绝且忽略 `diff.relative` | 子目录下无法生成相对路径补丁 | `src/command/format_patch.rs:285-290,500-506,1852-1859` | FPG-11、FPG-48 |
| GAP-FPG-12 | `#900`/`#909`：忽略 `format.coverLetter`（`--commit-list-format` 隐含封面由 `529 FP-13` 承接，需复核） | 无法通过配置自动生成封面信 | `src/command/format_patch.rs:178-180,549-564,1861` | FPG-12 |
| GAP-FPG-13 | `#905`：封面信省略系列 diffstat 汇总，且正文开头多出 Git 没有的 `*** SUBJECT HERE ***` 行 | 封面信缺少变更统计、正文与 Git 不同 | `src/command/format_patch.rs:1361-1445` | FPG-58、FPG-13 |
| GAP-FPG-14 | `#899`：`--commit-list-format` 不支持 `%(count)`/`%(total)` 原子、忽略 `format.commitListFormat`，且接受无原子字面量（issue 所称 `full` 关键字在 `git@6de20f6092` 中不存在，按无效格式串处理；`shortlog` 由 `529 FP-13` 承接） | Git 的封面信列表格式被拒或被错误接受 | `src/command/format_patch.rs:1535-1584,1621-1644` | FPG-14、FPG-49 |
| GAP-FPG-15 | `#892`：仅接受单个 `revision_range`，拒绝多参数及 `^<rev>` 排除语法 | `format-patch A ^B` 无法执行 | `src/command/format_patch.rs:108-109,663-685` | FPG-15 |
| GAP-FPG-16 | `#893`：mbox `"From "` 分隔符使用提交/当前时间而非 Git 固定 `Mon Sep 17 00:00:00 2001`；补丁 `Date:` 取提交者时间并按 UTC 渲染，而 Git 取作者时间与作者时区；封面信信封用全零 oid，而 Git 用系列末个提交 oid | 与 Git mbox 黄金输出不一致，`am` 往返丢失作者时区 | `src/command/format_patch.rs:794-807,828-829,1373-1376,1837-1840` | FPG-16、FPG-59 |
| GAP-FPG-17 | `#894`：`--base=auto` 与 `format.useAutoBase` 未实现 | 无法自动探测上游基线提交 | `src/command/format_patch.rs:242-247,534,1214-1284` | FPG-64、FPG-17、FPG-43 |
| GAP-FPG-18 | `#895`/`#912`：`--attach` 为布尔 flag 拒绝 `--attach=<name>` 且忽略 `format.attach` | 无法自定义 boundary 或从配置开启附件 | `src/command/format_patch.rs:249-256,883-905,1861` | FPG-18、FPG-38 |
| GAP-FPG-19 | `#898`：不支持 `--interdiff=<rev>`（三态前置检查由 `529 FP-10` 承接，实际输出未实现） | 无法生成系列间 interdiff | `src/command/format_patch.rs:97-364,1361-1445` | FPG-19、FPG-34 |
| GAP-FPG-20 | `#901`：文件名缺少 `vN-` reroll 前缀，且文件路径打到 `stderr` 而非 `stdout` | `-v` 文件名与脚本读取 stdout 失败 | `src/command/format_patch.rs:610-612,1699-1737` | FPG-20、FPG-45 |
| GAP-FPG-21 | `#908`/`#910`：`patch_slug` 强制小写且 `truncate_slug` 硬编码 52 字符上限忽略 `--filename-max-length` | 文件名大小写被破坏且长度被卡在 56 字符 | `src/command/format_patch.rs:1646-1681,1699-1737` | FPG-21、FPG-40 |
| GAP-FPG-22 | `#904`（部分）：`-U`/`--unified` 与 `--full-diff` 未支持 | 无法控制补丁上下文行数与全量 diff | `src/command/format_patch.rs:258-284,1852-1859` | FPG-22、FPG-61 |
| GAP-FPG-23 | `#917`、`#916`（部分）：默认开启 `--thread`、忽略 `format.thread=false`/空、拒绝 `--thread=deep` | 默认行为与配置开关不符 Git | `src/command/format_patch.rs:181-189,1285-1353,1861` | FPG-35、FPG-23、FPG-47、FPG-50 |
| GAP-FPG-24 | `#891`：封面信忽略 `branch.<name>.description` 且默认 `From:` 留空 | 封面信缺少分支描述与发件人 | `src/command/format_patch.rs:1386-1396,1422-1424` | FPG-24、FPG-60 |
| GAP-FPG-25 | `#906`：不支持 `--cover-from-description` 且未校验 `format.coverFromDescription` | 无法控制封面信描述来源且非法配置未报错 | `src/command/format_patch.rs:97-364,1861-1902` | FPG-25、FPG-33 |
| GAP-FPG-26 | `#903`：`--reroll-count` 仅接受 `usize`，拒绝小数/非数字值（Git 接受并清洗文件名前缀） | `-v4.4`、`-v4rev2` 等 Git 合法值被拒绝 | `src/command/format_patch.rs:220-226` | FPG-26、FPG-52 |
| GAP-FPG-27 | `#904`（部分）：`--diff-filter` 未支持（`-M`/`--no-renames` 由 `529 FP-06` 承接，需复核） | 无法按变更类型过滤补丁文件 | `src/command/format_patch.rs:258-284,1852-1859` | FPG-27、FPG-39、FPG-62 |
| GAP-FPG-28 | `#916`（部分）：`build_thread_id` 生成互相为子串的 Message-ID | 邮件线索匹配出现子串碰撞 | `src/command/format_patch.rs:1285-1353` | FPG-63、FPG-28 |
| GAP-FPG-29 | `#886`：长 `Subject:` 邮件头行不按 RFC 2822 折叠 | 超长 Subject 单行输出 | `src/command/format_patch.rs:863-869` | FPG-29 |
| GAP-FPG-30 | `#897`（及 `#881` t4036 闭环）：纯 ASCII 补丁仍强制输出 MIME 头 | 与 Git 纯 ASCII 省略 MIME 行为不符 | `src/command/format_patch.rs:893-905,1415-1417` | FPG-30、FPG-51、FPG-42 |
| GAP-FPG-31 | `#915`：缺少 `--no-from` 与 `format.from` 配置 | 无法通过配置控制发件人 | `src/command/format_patch.rs:212-217,1861-1902` | FPG-31、FPG-46 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `issues/529.md` | **前置写锁持有者**：`529.md` 当前持有 `src/command/format_patch.rs` 与 `src/cli.rs` 写锁 | 通过 `DEP-FPG-01` 严格等待 `issues/529#CLOSE-529` 完成释放写锁后再启动 `FPG-01`；`#880` 全部（`FP-02` 计数宽度、`FP-04` 前缀组合）由 529 交付并按其 `DEP-FP-07` 关闭，本计划不重复验收；对 529 已交付的 `#888`/`#881`/`#899`/`#904`/`#909` 部分按 GC-01 核实并只补齐剩余缺口；529 `DEFER-FP-01`/`DEFER-FP-02`（共享 diff 引擎与 stat 渲染器的版式差异）不由本计划修正，本计划以「验证执行协议」第 3 项的差异隔离夹具规避，并登记 `DEFER-FPG-03` |
| `plan-20260927.md` | `CM-08`/`CM-09` 涉及 `src/command/diff/` 与 `diff/{compare,render}.rs` | 通过 `DEP-FPG-02` 在 `FPG-22`、`FPG-27` 开工前核实无在途写者再串行修改 `src/command/diff/` |
| `issues/1040.md` / 同批 `log-show` 计划 | 均触及 `src/command/log.rs` | 通过 `DEP-FPG-03` 在 `FPG-11`/`FPG-22`/`FPG-27` 开工前核实 `src/command/log.rs` 无在途写者并串行协调 |
| `plan-20261117-global-init-infra.md` 及同批写 `src/internal/config.rs` 的计划 | 写入 `src/internal/config.rs` | 通过 `DEP-FPG-05` 在 `FPG-04`、`FPG-57` 开工前核实 `src/internal/config.rs` 无在途写者并串行协调 |
| `plan-long.md` | 长期兼容路线 | 本计划为具体执行落地，不直接改写 `plan-long.md` |
| `COMPATIBILITY.md` | 第 224 行 `format-patch` 兼容契约（被 `compat_ledger_schema` 锚定行号） | 仅原地修改第 224 行，不增删行 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 35 项 issue 均指向明确 Git `format-patch` 兼容缺口 | 拆分为 64 张单轴卡（R16 按独立谓词重算后拆出 `FPG-32`/`FPG-33`/`FPG-34`；R18 对照 35 个 issue 的验收标准补齐后拆出 `FPG-35`/`FPG-36`/`FPG-37`；R19 拆出 `FPG-38`/`FPG-39`；R20 拆出 `FPG-40`..`FPG-43`；R22 拆出 `FPG-44`/`FPG-45`；R23 拆出 `FPG-46`/`FPG-47`；R24 拆出 `FPG-48`..`FPG-50`；R25 拆出 `FPG-51`；R26 拆出 `FPG-52`；R27 拆出 `FPG-53`；R31 拆出 `FPG-54`..`FPG-57`；R32 拆出 `FPG-58`/`FPG-59`；R38 拆出 `FPG-60`；R39 拆出 `FPG-61`..`FPG-63`；R40 拆出 `FPG-64`），与 `issues/529.md` 边界清晰 |
| 可行性 | 集中在 `format_patch.rs` 及 `log.rs`/`diff/` | 通过 `DEP-FPG-01`、`DEP-FPG-02`、`DEP-FPG-03`、`DEP-FPG-05` 避开在途写锁，`DEP-FPG-04` 串行化共享交付与发布窗口，全串行交付 |
| 任务卡粒度 | R3 拆出 `FPG-29..31`；R16 按「验证执行协议」第 5 项逐卡把 AC 改写为每条一个独立谓词并重算，`FPG-01`（10）、`FPG-25`（9）、`FPG-19`（9）超限，按 G-09 拆出 `FPG-32`、`FPG-33`、`FPG-34`；R18 按 issue 验收标准补齐后 `FPG-04`、`FPG-05`、`FPG-23` 超限，拆出 `FPG-37`、`FPG-36`、`FPG-35`；R19 `FPG-18`、`FPG-27` 补齐后拆出 `FPG-38`、`FPG-39`；R20 `FPG-21`、`FPG-36`、`FPG-30`、`FPG-17` 补齐后拆出 `FPG-40`、`FPG-41`、`FPG-42`、`FPG-43`；R22 `FPG-04`、`FPG-20` 补齐后拆出 `FPG-44`、`FPG-45`，`FPG-28` 提前到 `FPG-35`/`FPG-23` 之前；R23 `FPG-31`、`FPG-23` 补齐后拆出 `FPG-46`、`FPG-47`；R24 `FPG-11`、`FPG-14`、`FPG-47` 补齐后拆出 `FPG-48`、`FPG-49`、`FPG-50`；R25 `FPG-30` 补齐 #881 签名行判据后拆出 `FPG-51`；R26 `FPG-26` 补齐 `-v4rev2` 主题判据后拆出 `FPG-52`；R27 `FPG-02` 补齐 `user.useConfigOnly` 下环境变量优先的成功路径后拆出 `FPG-53`；R31 修复前专项清扫后 `FPG-32`（多层与重复取值）、`FPG-37`（`To:`/`Cc:` 统一列表）、`FPG-06`（签名块布局、`format.signatureFile`）超限，拆出 `FPG-56`、`FPG-57`、`FPG-54`、`FPG-55`，`FPG-23` 的 `format.thread` 关闭取值并入 `FPG-47`；R32 按 Codex R31 单轴意见自 `FPG-13` 拆出正文模板 `FPG-58`（先于 `FPG-13` 执行）、自 `FPG-16` 拆出补丁 `Date:` 作者时间与时区 `FPG-59`；R38 按 Codex R37 单轴意见自 `FPG-24` 拆出封面信默认 `From:` 身份 `FPG-60`；R39 按 Codex R38 单轴意见自 `FPG-22` 拆出 `--full-diff` 接受 `FPG-61`、自 `FPG-39` 拆出封面信写入失败 `FPG-62`、自 `FPG-28` 拆出线程头位置 `FPG-63`（先于 `FPG-28` 执行）；R40 按 Codex R39 单轴意见自 `FPG-17` 拆出 base 区块位置 `FPG-64`（先于 `FPG-17` 执行），`FPG-62` 的失败契约与不留部分输出分计 2 条 AC；G-03 计数排除强制文档门，`VER` 只计具名验收门；G-09 原卡均已双向登记拆出卡 | 全部卡 `AC ≤ 8`、`VER ≤ 8`、`scope ∈ {S, M}` |
| 依赖与顺序 | 线性 DAG `DEP-FPG-01 -> FPG-01 -> FPG-32 -> FPG-02 -> FPG-53 -> FPG-03 -> FPG-04 -> FPG-56 -> FPG-44 -> FPG-37 -> FPG-57 -> FPG-05 -> FPG-36 -> FPG-41 -> FPG-54 -> FPG-06 -> FPG-55 -> FPG-07 -> FPG-08 -> FPG-29 -> FPG-30 -> FPG-51 -> FPG-42 -> FPG-09 -> FPG-10 -> FPG-31 -> FPG-46 -> FPG-11 -> FPG-48 -> FPG-12 -> FPG-58 -> FPG-13 -> FPG-14 -> FPG-49 -> FPG-15 -> FPG-16 -> FPG-59 -> FPG-64 -> FPG-17 -> FPG-43 -> FPG-24 -> FPG-60 -> FPG-25 -> FPG-33 -> FPG-18 -> FPG-38 -> FPG-19 -> FPG-34 -> FPG-20 -> FPG-45 -> FPG-26 -> FPG-52 -> FPG-21 -> FPG-40 -> FPG-22 -> FPG-61 -> FPG-27 -> FPG-39 -> FPG-62 -> FPG-63 -> FPG-28 -> FPG-35 -> FPG-23 -> FPG-47 -> FPG-50`（叠加 `DEP-FPG-05 -> FPG-04`、`DEP-FPG-05 -> FPG-57`、 `DEP-FPG-03 -> FPG-11`、`DEP-FPG-02`/`DEP-FPG-03 -> FPG-22`、`DEP-FPG-02`/`DEP-FPG-03 -> FPG-27` 跨计划写锁协调；`DEP-FPG-04` 约束全部卡的开工与发布窗口） | 无环、无 I–I 并发冲突、Phase 出入条件与依赖链一致 |
| 完整性 | 每卡含按「验证执行协议」区分的具名验收门与共享回归组、AC → 断言映射表、ER-13 `T-1` 全量触发、D 组真实 CI job、按「文档与网站验收命令」交付的 EN/zh/dev/`COMPATIBILITY.md`/网站页 | 符合 ER-04、ER-06/ER-06a、ER-08、ER-13、ER-14 |
| 安全性 | 邮件头控制字符过滤、`--relative`/`--output-directory` 路径校验 | 继承 GC-07、GC-11 |
| 功能正确性 | 依据 `git@6de20f6092`、`529` ADR-FP-07 与 35 个 issue 的验收标准（2026-11-18 以 `gh issue view -R libra-tools/libra` 逐个核对）对齐每个边界 | 消除二义性判据与 `#881` 过早关闭；R18 修正 FPG-14/26/25/33 与 issue 相反或无关的判据 |
| 接口兼容 | `--json` `FormatPatchOutput` 向后兼容 | 保持既有字段稳定 |
| 数据流与控制流 | 先完整渲染全系列，再逐文件 `atomic_write` | 明确单文件原子写语义 |
| 性能与容量 | 复用既有 commit walk 与 diff 计算 | 继承 GC-10 |
| 可靠性与容错 | `Result` + `CliError` 返回可操作诊断，无生产 `unwrap()` | 继承 GC-11 |
| 可维护性 | 解析与渲染集中于 `format_patch.rs` 单一事实源 | 继承 GC-02 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-17 02:51:12 UTC | 初始成稿 | 建立 `plan-20261117-format-patch.md`，映射 35 项 issue | N/A | 全文 |
| 2026-11-17 03:02:00 UTC | Claude R1 FAIL + Codex R1 FAIL | 前缀改为 `FPG-*` 避让 `529.md`、修正 `command_test`/`nextest`、补齐 D 组/基线/外部参照、拆出 `FPG-24..28` | FP-01..23 → FPG-01..28; FPG-12 → FPG-12, FPG-24; FPG-13 → FPG-13, FPG-25; FPG-20 → FPG-20, FPG-26; FPG-22 → FPG-22, FPG-27; FPG-23 → FPG-23, FPG-28 | 全文 |
| 2026-11-17 03:08:00 UTC | Claude R2 FAIL + Codex R2 FAIL | ① 全部 `independent` 卡登记 `T-1`；② `base.yml` 移除不存在的 `network-remotes`；③ `FPG-02` 对齐 `529` ADR-FP-07 且 `#881` 完整闭环延至 `FPG-30`；④ `Rollback mode` 改为 `immutable-release`；⑤ 修正 Phase 与依赖链对齐及 G-09 双向拆分；⑥ 拆出 `FPG-29..31` | FPG-07 → FPG-07, FPG-29; FPG-08 → FPG-08, FPG-30; FPG-10 → FPG-10, FPG-31 | 实施顺序、依赖表、粒度表、追溯表、里程碑、Review log |
| 2026-11-17 03:12:00 UTC | Claude R3 FAIL + Codex R3 FAIL | ① 全卡 `Implementation write set` 补齐 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`，同步命令补 `--libra-root /Volumes/Data/gitmono/libra` 与执行目录；② `FPG-02` 错误码改为 `LBR-CLI-003` 并覆盖全部 ADR-FP-07 失败消息；③ `FPG-01` 验证命令拆出独立精确门；④ 补全 `DEP-FPG-03`（`src/command/log.rs` 跨计划写锁）并修正 `DEP-FPG-02` 锚点；⑤ 重算 `FPG-01`/`FPG-04` 独立谓词 AC 数；⑥ `FPG-19` 明确继承 `529 FP-10` 三态前置检查并撤回不支持分类 | N/A | 依赖表、粒度表、FPG-01/02/04/11/19/22/27、全局约束、Review log |
| 2026-11-17 03:22:00 UTC | Claude R4 FAIL + Codex R4 FAIL | ① `FPG-01` 精确 filter 改为 `test(=command::format_patch_test::format_numbered_config)`；② `FPG-02` 补齐 `user.useConfigOnly` 缺邮箱失败、`EMAIL` 回退及独立 AC 计数（`AC=5/8`）；③ `FPG-27` 补入 `DEP-FPG-02`；④ `FPG-19` 补齐继承 `529 FP-10` 封面信三态回归门与分叉系列对照（`AC=4/8`）；⑤ D 组补全 `release.yml` 5 个 job 与 CDN 检查；⑥ 修正 `DEP-FPG-01`/`DEP-FPG-03` 锚点与兼容窗口说明 | N/A | D 组门、依赖表、实施顺序、粒度表、FPG-01/02/19/27、Review log |
| 2026-11-17 03:28:00 UTC | Codex R5 FAIL | ① `FPG-19` 明确采用 series-level interdiff（比较旧系列与当前系列变更，而非分叉基线直接 tip-to-tip 树对比）；② 修正 `FPG-30` 回滚说明，移除 `--encode-email-headers` 可恢复纯 ASCII MIME 头的误述 | N/A | 字段全局默认与例外、FPG-19、Review log |
| 2026-11-17 03:32:00 UTC | Codex R6 FAIL | `FPG-19` 明确仅在多补丁系列（`total > 1`）且封面信最终状态被显式禁用时拒绝 `--interdiff`，补充单补丁禁用源与 CLI 覆盖配置回归 | N/A | FPG-19、Review log |
| 2026-11-17 04:01:04 UTC | Codex R7 FAIL | 将 `FPG-03..FPG-08, FPG-10..FPG-31` 全部 `(new)` 验证命令由子串过滤改为精确完整函数路径 `test(=command::format_patch_test::<fn>)` 并统一 `command::` 模块前缀 | N/A | FPG-01/03..31、测试矩阵、Review log |
| 2026-11-17 04:09:11 UTC | Codex R8 FAIL + Claude R8 P2 | ① 将 `FPG-09` 验证命令拆分为独立精确 `test(=command::format_patch_test::...)` 门（`VER=4/8`）消除 `\|` 掩蔽零命中风险；② 同步文头 R9 时间、`FPG-19` `AC=4/8` 计数与 R7 历史描述 | N/A | 文头、粒度表、FPG-09、FPG-19、Review log |
| 2026-11-17 05:00:00 UTC | Codex R9 FAIL | ① 对齐 `AGENTS.md:135`，将全计划网站交付路径统一为 `/Volumes/Data/gitmono/libra-backend`（`cf` 分支），并在 `GC-05`/`兼容收口` 补齐前端校验、Claude Code `PASS`、签名 `libra commit`/推送 `cf` 及 Cloudflare 部署核验门（路径不可用时 fail-closed）；② 新增 `DEP-FPG-04` 并在「发布分组与并发窗口」规定每卡开工与发布前均须核验 `plan-status.md` 对 `COMPATIBILITY.md`、版本面（`Cargo.toml`/`install.sh`/`install.ps1`/`Cargo.lock`）、`plan-status.md` 与 `/Volumes/Data/gitmono/libra-backend` 的跨计划独占锁（fail-closed） | N/A | 文头、GC-05、依赖表（DEP-FPG-04）、并发窗口、兼容收口、Review log |
| 2026-11-17 05:02:00 UTC | Codex R10 FAIL | ① 修正 `DEP-FPG-04` 锁持有期至 `gh release create` 与 D 组 `release.yml`/CDN 核验全部完成后释放，并明确 `plan-status.md` 中锁获取、归属（卡号/owner）与释放状态机记录方式；② 修正粒度审计表 `FPG-19` `AC` 为 `4/8`（与卡内一致） | N/A | 文头、DEP-FPG-04、粒度表、Review log |
| 2026-11-17 05:05:00 UTC | Codex R11 FAIL | 为 `DEP-FPG-04` 补全跨工作区可见的原子获取与仲裁协议：以远端 `origin/main` 的 `plan-status.md` 为唯一权威账本，由单一协调者/指定发布者串行仲裁（或通过 `libra fetch` + PR/push 到 `origin/main` 原子登记并回读确认唯一持有者）后再写入或发布，释放亦同步推送 `origin/main` 确认 | N/A | 文头、DEP-FPG-04、发布窗口、Review log |
| 2026-11-17 05:07:00 UTC | Codex R12 FAIL | 明确 `DEP-FPG-04` 共同仲裁约束：指定单一授权协调者/发布者（`genedna` 或其显式指派的单一发布 Agent，须在 `origin/main` `plan-status.md` 登记）统一串行仲裁所有参与计划（`format-patch`/`log-show`/`global-init-infra`/`plan-20260927`），所有计划均须遵守基于最新 `origin/main` 检查、竞争后重检与回读确认协议 | N/A | 文头、DEP-FPG-04、发布窗口、Review log |
| 2026-11-17 05:09:00 UTC | Codex R13 FAIL | 将「共同串行仲裁规则已在 `origin/main` `plan-status.md` 合入生效，且各在途参与计划均已采纳遵守同一协议（或已收口关闭）」设为 `DEP-FPG-04` 的强制开工前置条件（未满足前 `FPG-01` 保持 `pending`） | N/A | 文头、DEP-FPG-04、Review log |
| 2026-11-18 08:19:08 UTC | 本轮 Codex 复审修订 | 统一网站交付路径为 `/Volumes/Data/gitmono/libra-backend`（用户最新指令指定；`AGENTS.md` 的 `/Volumes/Sea/libra-backend` 已被取代且本机不存在），并在「文档职责」写明权威依据与 STOP 条件 | N/A | 全文对应章节 |
| 2026-11-18 09:43:54 UTC（R16 更正：原记 `10:35:00 UTC` 晚于实际写入，属未来时间；此处取文件 mtime，为实际写入时间上界） | Codex 复审 FAIL 修复（验证契约与门禁） | 新增「验证执行协议」（精确 `-E 'test(=完整路径)'`、先行 `cargo nextest list` 恰好 1 匹配、记录 exit code 与四计数、逐条 AC 断言、模块回归标为共享回归组）；裸名过滤补全模块路径、损坏过滤 `-E 'test(=X)'::后缀` 修正、模块过滤统一为 `test(/^X::/)` 并标共享回归组；每卡补「AC → 断言映射」并写明 exit code／stdout+stderr 文案／前后状态期望；补「文档 AC」逐文件验收映射与「文档与网站验收命令」块（含家族子卡只做本地门、唯一发布点统一推送、哈希与提交树核对、部署绑定 `$FINAL`、STOP 条件）；完成判据补计划 Codex 终审字面 `VERDICT: PASS` 与最终树 `HEAD`/`HEAD^{tree}` 记录及树变重跑；追溯表模块过滤标注共享回归组 | N/A | 执行检查必备需求、全部任务卡、完成判据、追溯表、修订历史 |
| 2026-11-18 10:12:27 UTC | 用户 2026-11-18 指令（评审门口径变更） | 计划文本提交门由「Codex 单审」改为「Claude（Subagent）与 Codex 双审，二者对同一受审 SHA-256 均 `VERDICT: PASS` 方可提交」；仅改文头审阅合同与完成判据中的计划文本提交门措辞，执行期网站交付 Claude Code `PASS` 门不变 | N/A | 文头、完成判据 |
| 2026-11-18 10:35:23 UTC | Claude R16 FAIL + Codex R16 FAIL（受审 SHA-256 `de36d50c813c7bb3cc075f386e12783c520d6812a6aa6bdd42cb37296df74937`） | ① 补齐 2026-11-18 修订声称已加但未落盘的「文档与网站验收命令」第 1–8 步（同步 + 只读漂移检查、`pnpm typecheck`/`pnpm build`、Claude Code `PASS` 绑定最终字节、精确暂存与签名+DCO、`libra push origin cf` 与 `ls-remote` 回读、线上页面核验、STOP 条件），并把 `DEP-FPG-04` 锁持有期延至网站部署核验完成；② 重写「验证执行协议」：区分具名验收门（精确项或全锚定门族正则，`cargo nextest list` 集合逐字相等）与共享回归组（M ≥ 1），统一 `--no-tests=fail`，规定 AC → 断言映射只断言真实输出、默认前后状态契约与独立谓词计数口径；③ 全部卡的 AC 改为每条一个独立谓词并以映射表绑定具名测试、退出码与必须出现/不出现的真实输出，删除机械生成的 issue 号/参数名/测试名「输出」断言；④ 按独立谓词重算后 `FPG-01`、`FPG-25`、`FPG-19` 超 8，按 G-09 拆出 `FPG-32`（`format.numbered` 配置与 CLI 覆盖）、`FPG-33`（`format.coverFromDescription` 配置）、`FPG-34`（单补丁 `--interdiff`）；⑤ FPG-09 改用既有拒绝门族 `fp03_prefix_k__*`/`fp03_rfc_k__*`，FPG-03 补跑既有 `ignore_if_in_upstream_suppresses_patch_equivalent_commits`，FPG-19 成功与拒绝路径分别绑定测试并引用 `529 FP-10` 门族；⑥ FPG-22 `--full-diff` 改为「接受且无 pathspec 时不改变输出」（format-patch 无 pathspec），FPG-28 子串判定改为去尖括号的 ID 值，FPG-10 收敛到 #889 的 `invalid ident line: foo`，FPG-17 去掉非 Git 的范围基线推导，FPG-14 去掉「等」；⑦ 各卡 `Docs and compatibility impact` 逐文件列出网站生成页与验收标记，新增可见 flag 的卡写集补 `tests/compat/format_patch_flag_surface.rs`；⑧ 更新 `src/cli.rs`、`tests/INDEX.md`、`Cargo.toml` 锚点，DAG 摘要补全，测试矩阵补 `compat_broken_pipe_output` 并标注共享回归组，追溯表列出各卡具名测试，完成判据要求最终全量门在干净检出上运行并在树或工作区变化时重跑；⑨ 修订历史断表空行删除、未来时间戳更正；GC-05 与兼容收口的 `AGENTS.md:135` 引用改为「流程沿用、路径以用户指令为准」 | FPG-01 → FPG-01, FPG-32; FPG-25 → FPG-25, FPG-33; FPG-19 → FPG-19, FPG-34 | 文头、事实基线、评审结论、执行检查必备需求、实施顺序、依赖登记表（DEP-FPG-04）、Phase 0–2、粒度审计表、全部任务卡、测试矩阵、追溯表、里程碑、风险登记、兼容与文档收口、Codex review log、完成判据 |
| 2026-11-18 10:45:25 UTC | Claude R17 FAIL + Codex R17 FAIL（受审 SHA-256 `a9d455260cc2ef25a63833baedb178a34c20da217faaa77c34cbbec5614adf0d`） | ① 完成判据消除记账循环：全量门绑定 `TESTED_HEAD`，其后只改 `docs/development/plan/**` 的记账提交以 `libra diff --name-only` 证明后不重启全量门，并按 v2.14 预登记终止链末端 D 组补记；② 「文档与网站验收命令」第 3 步改为以 `MERGE_SHA` 绑定同步源字节、增加「页面无变化」分支与区分探针 `PROBE`（新页面含、旧页面不含），第 8 步先做 HTML 实体解码再匹配并以 `PROBE` 绑定 `$SITE_SHA` 版本；③ 「验证执行协议」允许全锚定字面量交替正则、规定行为迁移、允许断言错误消息中按契约出现的键/值/revision、按 `CliError` 种类与 `with_exit_code` 覆盖写明退出码、补充黄金值归一化（`--no-thread --no-signature`）与单一谓词形式；④ 各卡把可分别失败的子性质改为对指定区段的单一黄金值或行级正则判定：FPG-04 头区、FPG-05 notes 区块、FPG-07 正文与 `am` 结果、FPG-08 编码头、FPG-13/24/25 封面信区段、FPG-18 MIME 骨架、FPG-19/34 interdiff 区段（含相对 diffstat/`---` 位置）、FPG-29 折行、FPG-30 MIME 判定与 t4036 全文、FPG-23/28 三补丁线程结构；⑤ FPG-09/FPG-19 门族扩为七门（含 `__json`/`__machine`），FPG-19 增空格形式 `--interdiff <rev>` 成功场景；FPG-17 增 `prerequisite-patch-id:` 黄金值并把自动基线失败对齐既有 `--base` 约定（`fatal:`、`LBR-CLI-003`、128）；FPG-11 的仓库外/不存在前缀改为与 Git 一致（本机 Git 2.54 探针均 exit 0 且不输出 diff 段）；FPG-20 拆出 stderr 不再输出路径为独立 AC；FPG-14/25/26/27 拒绝映射补齐首行前缀与 `Error-Code:`；FPG-32 AC5 映射改为完整 `Subject:` 前缀；⑥ FPG-01/FPG-19/FPG-25 的 axis 去除与拆出卡重叠的描述；评审结论「可行性」补 `DEP-FPG-03`/`DEP-FPG-04`；风险登记补 Git 黄金值版本漂移 | N/A | 文头、评审结论、执行检查必备需求、粒度审计表、FPG-01/04/05/06/07/08/09/10/11/13/14/15/16/17/18/19/20/21/23/24/25/26/27/28/29/30/32/34、测试矩阵、追溯表、风险登记、Codex review log、完成判据 |
| 2026-11-18 11:07:35 UTC | Claude R18 FAIL + Codex R18 FAIL（受审 SHA-256 `7a671546d7bb809b2c7bf2e6bb2d414379f76e6c4c2a894ee0241f20da56f976`） | ① 以 `gh issue view -R libra-tools/libra` 逐个核对 35 个 issue 的验收标准后对齐各卡：FPG-14 改为 #899 的 `%(count)`/`%(total)`/`log:`/`full`/`format.commitListFormat` 与拒绝无原子字面量；FPG-26 改为按 Git 清洗 `-v` 文件名前缀（#903）；FPG-25/33 非法模式改为 Git 的 128 与 `invalid cover from description mode`（#906）；FPG-08 补 `format.encodeEmailHeaders` 与 #911 的 `=20` 编码；FPG-29 补非 ASCII 折行；FPG-16 补 `--zero-commit`；FPG-13 补重命名 stat；FPG-12 补 `modern`；FPG-17 补 criss-cross；FPG-18 补 `--attach --base`、封面信非 multipart、`format.attach=""`；FPG-30 按 t4036 #2–#5 逐项；FPG-01 补封面信编号与 `--start-number 3`；FPG-31 补反向顺序；FPG-15 用 #892 的 `^main rebuild-1`；② 补齐后超限的卡按 G-09 拆出：FPG-04 → FPG-04（`format.headers`、`--add-header`/`--no-add-header`）, FPG-37（`format.to`/`format.cc` 与顺序，#914）；FPG-05 → FPG-05（`--no-notes` 与命令行顺序）, FPG-36（`format.notes`）；FPG-23 → FPG-23（默认关闭与 `format.thread`）, FPG-35（`--thread=shallow`/`deep` 与 deep 链式线程，按关系逐条计数）；③ 「验证执行协议」补区段定义（邮件结束、头区、扩展头序列、`---` 段、变更段、notes 区块、描述段、列表段、diffstat 段），去掉「上游门族整体计 1 条」改为同一 argv 人读五门共同断言一次错误契约，`Error-Code:` 改为独立行断言，黄金值版本措辞修正；④ 映射修正：跨次运行比较只比区段（封面信 `Date:` 为运行时间），FPG-04/30 只比本卡负责的区段，FPG-03 改为一次列表相等，FPG-34 补 `--stdout`，FPG-09 拆为三个 argv 的人读五门；⑤ 「文档与网站验收命令」：命令一律以子 shell 指定仓库，第 2 步在 `MERGE_SHA` 上复核 4 个仓库文件，第 3 步按 EN 页判定分支，无变化分支记录摘要、`SITE_SHA` 与沿用的已核验部署，第 8 步以 Workers Builds 构建记录绑定提交 SHA 并用 `mktemp`；⑥ 完成判据补网站分支口径、`remote-pending`/`blocked`、既有失败复现证据与 `plan-long.md` N/A；适用范围补 `format.commitListFormat`、`format.encodeEmailHeaders`；GAP-FPG-04/05/08/14/23/26 同步 | FPG-04 → FPG-04, FPG-37; FPG-05 → FPG-05, FPG-36; FPG-23 → FPG-23, FPG-35 | 文头、适用范围、当前缺口、评审结论、ADR-FPG-03、执行检查必备需求、实施顺序、Phase 0/2、粒度审计表、FPG-01..FPG-37 中改写的卡、测试矩阵、追溯表、里程碑、风险登记、Codex review log、完成判据 |
| 2026-11-18 11:17:59 UTC | Claude R19 FAIL + Codex R19 FAIL（受审 SHA-256 `40d420f700dd81884f9cbd5d33b90b6bf7a5cd2e4816459c20d57a6ae45db691`） | ① 「文档与网站验收命令」改为发布前段（第 1–5 步：仓库文档、网站基线须等于远端、同步与探针、前端验证、Claude Code 审查）与发布后段（第 6–8 步：`MERGE_SHA` 复核 4 文件与 `FINAL`、远端未前进、提交父提交须为 `SITE_BASE`、推送与回读、构建记录绑定）；`ls-remote` 一律经 `remote_sha` 取第一列且要求恰一行；每卡开始重置变量，无变化分支令 `FINAL=$POST`；`curl` 与解码分步检查退出码并要求非空；删除型变更可用 `NEG_PROBE`；② 「验证执行协议」：`VER` 只计具名验收门，共享回归组移到每卡单独小节；补充 `notes 段`、`列表段`/`描述段`（非循环定义）与「归一化线程结构」，线程用 Git 黄金比较；X 型门族删除的行为迁移口径；③ `#880` 由 `issues/529#FP-02`/`FP-04` 交付并关闭，FPG-01 只保留 #896 默认编号（成功定义、GAP-FPG-01、与其它计划的关系、追溯表同步）；④ 多值配置：FPG-04 在 `src/internal/config.rs` 新增级联多值 helper（新增 `DEP-FPG-05` 协调 `global-init-infra` 等写者），FPG-37/36 复用；⑤ 按 issue 补齐并拆卡：FPG-18 → FPG-18（`--attach=`/`--inline=`）, FPG-38（`format.attach`，含本地空值覆盖全局）；FPG-27 → FPG-27（`--diff-filter`）, FPG-39（`529 FP-06` 重命名核验与 #904 封面信写入失败/继承）；FPG-03 补非重复范围；FPG-29 补短标题单行并改到 FPG-08 之后执行；FPG-30 改为三头行级判定并补 multipart 类型；FPG-09 补单独 `-k`/`--subject-prefix`/`--rfc` 成功路径；FPG-31 用三身份夹具对照 Git 并补 `--from` 覆盖配置；FPG-13 `--no-stat`/`-p` 用黄金区段；FPG-15 用 `--cover-letter` 与变更段比较；FPG-33 覆盖全部配置取值；FPG-26 补 `-v2.2` interdiff 标题；FPG-21 补 `ALL CAPS` 与 15/25/35；FPG-23/35 改为对应 t4014 用例的归一化线程结构黄金比较并补 `--no-thread`、deep 变体与单独 `--in-reply-to`；FPG-28 补无封面信三补丁；FPG-34 改比变更段；FPG-37 改为头逐字相等；⑥ 事实基线空系列 JSON 描述更正 | FPG-18 → FPG-18, FPG-38; FPG-27 → FPG-27, FPG-39 | 文头、成功定义、事实基线、当前缺口、与其它计划的关系、评审结论、ADR-FPG-02/03、执行检查必备需求、实施顺序、依赖登记表（DEP-FPG-05）、Phase 0/2、粒度审计表、全部任务卡、追溯表、里程碑、风险登记、Codex review log、完成判据 |
| 2026-11-18 11:24:48 UTC | Claude R20 FAIL + Codex R20 FAIL（受审 SHA-256 `873d34428fe0c17b9bd649fbdaa38d860920b37c0a594ca347d50ddbf1a98c3b`） | ① 文档门：发布前段（第 1–5 步）明确为 `locally-accepted` 前置、发布后段（第 6–8 步）为 `remote-pending → complete` 后置门（各卡 ER-06 行同步，消除与本地验收的循环）；`remote_sha` 先单独检查 `ls-remote` 退出码并校验十六进制 SHA；合入树文档与审定字节不一致时按 `FIX-*` 前滚发新版本而非回到旧版本；风险登记同步新顺序；② 协议：新增「规范头区」「封面信正文」，「diffstat 段」含重命名摘要行，「归一化线程结构」按 t4014 `check_threading` 保留邮件边界；去掉以上游 `refusal_gates!` 门族作为本卡 AC 的口径；③ #888 第 1–4 项与 #898 第 3 项的拒绝契约由 `issues/529#FP-03`/`FP-10` 交付并验收，FPG-09/FPG-19 不再以其门族计 AC（仍在共享回归组运行）；FPG-09 改为补强单独 `-k`/`--subject-prefix`/`--rfc` 成功路径；④ 按 issue 补齐后超限拆卡：FPG-21 → FPG-21（#908 大小写）, FPG-40（#910 长度，15/25/35 分条）；FPG-36 → FPG-36（`format.notes` 取值）, FPG-41（配置与命令行交互，#884 第 3、4b 项）；FPG-30 → FPG-30（补丁邮件 MIME，补非 ASCII 作者与 `--inline`）, FPG-42（封面信 MIME）；FPG-17 → FPG-17（`--base=auto`，补显式 `--base` 不变）, FPG-43（`format.useAutoBase`）；⑤ 映射修正：FPG-04/37 改比规范头区并补连续空格保留（不得复用折叠空白的 `sanitize_header_value`）；FPG-13 重命名摘要与 `--no-stat`/`-p` 改比封面信正文；FPG-15 两种写法均带 `--cover-letter` 比较；FPG-07 `git log %B` 去记录终止符；FPG-18 补 `--attach=mimemime` 封面信；FPG-20 补 `-v23 --interdiff` 文件输出；FPG-35 单独 `--in-reply-to` 改为多补丁 Git 黄金值；FPG-39 封面信继承 `-M` 与 `--no-renames` 分条；FPG-27 删除过期证据行；追溯表来源与 FPG-04 落点同步 | FPG-21 → FPG-21, FPG-40; FPG-36 → FPG-36, FPG-41; FPG-30 → FPG-30, FPG-42; FPG-17 → FPG-17, FPG-43 | 文头、当前缺口、评审结论、ADR-FPG-03、执行检查必备需求、实施顺序、Phase 0–2、粒度审计表、全部任务卡、追溯表、里程碑、风险登记、Codex review log |
| 2026-11-18 11:28:57 UTC | Claude R21 FAIL + Codex R21 FAIL（受审 SHA-256 `1a7456e10f0f1bf1364850038f2cc348de4ef80b98b093d07d6daca9e7ecf6e9`） | ① 成功定义补记 `#888` 第 1–4 项与 `#898` 第 3 项的拒绝契约由 `issues/529#FP-03`/`FP-10` 交付验收；② ER-04 补 squash 合并后主分支提交须自带签名与 DCO 且树等价，否则用保留签名的受保护合并或 STOP；③ 文档块第 3 步旧页面先落临时文件并检查 `libra show` 退出码，再按零命中口径判定 `PROBE`/`NEG_PROBE`；完成判据补 v2.14 次一工作日兜底；④ 协议新增「补丁序列摘要」区段，`am` 往返一律在独立接收仓库执行；⑤ 卡片：FPG-04 控制字符统一「移除后成功」；FPG-41 补具名 ref 加 `--no-notes`；FPG-29 拆分短标题与多行首段两个场景；FPG-30 `--attach`/`--inline` 改为断言头区 MIME 序列；FPG-12 补 `full` 隐含封面信；FPG-14 AC5 改为 `format.coverLetter` 与 `format.commitListFormat` 同时配置；FPG-15 各 Git 对照改比补丁序列摘要；FPG-43 改对照 Git 非空 base 黄金值并补 `whenAble` 静默；FPG-25 补空格形式并以空格形式验证非法值；FPG-19 补无 `-v` 场景；FPG-20/26 补逐文件 `Subject:` 版本与文件名一致；FPG-22/27 写集补 `src/command/diff.rs`（`DEP-FPG-02` 同步覆盖）；FPG-27 删除过期证据行；FPG-35 无线程场景改为显式 `--no-thread`；各卡「已在 X 完成」改为「由 X 承接」 | N/A | 成功定义、执行检查必备需求、依赖登记表、FPG-04/12/14/15/19/20/22/25/26/27/29/30/35/41/43 及多卡 Out of scope、粒度审计表、追溯表、Codex review log、完成判据 |
| 2026-11-18 11:33:20 UTC | Claude R22 PASS（仅 P2）+ Codex R22 FAIL（受审 SHA-256 `04756b384fa552ef21dae26cd882970fd0b203ecf1c61681769371e6d438415d`） | ① 协议：「diffstat 段」允许对齐空白并要求不同长度路径夹具；「归一化线程结构」逐字复现 t4014 `check_threading`（子串替换），并把 `FPG-28` 提前到 `FPG-35`/`FPG-23` 之前；行为迁移清单补 FPG-29 的 `encode_email_headers_splits_long_words_under_75_chars`；② 文档块第 6、7 步的 `libra show` 改为先落临时文件、检查退出码再取摘要；兼容收口网站条目按有无变化分支；③ 拆卡：FPG-04 → FPG-04（`format.headers`）, FPG-44（`--add-header`/`--no-add-header`，补命令行注入场景）；FPG-20 → FPG-20（`vN-` 前缀与 `Subject:` 版本，封面信与补丁分条）, FPG-45（路径列表输出到 stdout）；FPG-26 封面信与补丁 `Subject:` 版本分条；④ 修正：FPG-04 续行缩进以 Git 黄金值为准（现行四空格）；FPG-10 补裸 `--from` 用提交者；`full` 隐含封面信从 FPG-12 移到 FPG-14（实现先于验收），FPG-14 配置场景改用含 `%(total)`/`%an` 的格式；FPG-15 补无封面信 `^main rebuild-1`；mail target 测试名在映射与 Verification 统一为顶层名；FPG-20/26 `Subject:` 前缀补全；FPG-22/27 `diff.rs` 锚点互换更正（`parse_diff_filter` 2228、`rewrite_unified_diff_context` 4267）并补全 `docs/development/plan/issues/529.md` 路径，追溯表落点补 `src/command/diff.rs`；FPG-39 单列 `fp06_*` 共享回归（M = 14）；FPG-23 区分 `--in-reply-to` 外部父头与系列线程；FPG-43 AC1 明确比较 FPG-17 AC2 的 base 区块；FPG-14 AC5 只保留一次黄金比较 | FPG-04 → FPG-04, FPG-44; FPG-20 → FPG-20, FPG-45 | 文头、评审结论、ADR-FPG-03、执行检查必备需求、实施顺序、Phase 0/2、粒度审计表、FPG-04/10/12/14/15/16/20/22/23/26/27/28/29/35/37/39/43/44/45、测试矩阵、追溯表、里程碑、风险登记、兼容与文档收口、Codex review log |
| 2026-11-18 11:38:31 UTC | Claude R23 PASS（仅 P2）+ Codex R23 FAIL（受审 SHA-256 `1255da6f47a1f4a29a7cb9f5a48181a420eaa184fb986fa0ee52bc686c5ea4c8`） | ① 协议：「归一化线程结构」按基线 Perl 片段语义说明（每行只登记首个 ID、单次子串替换、多 ID `References:` 夹具对照验证）；网站「页面无变化」分支只按生成页摘要判定，允许 `$EN` 仅在同步脚本会规范化的部分变化；② 依赖与卡片：FPG-28 的过期先行依赖改正并改 `scope=M`；FPG-26/22/25/30 的 Out of scope 指向正确承接卡；FPG-02 补纯环境变量身份成功场景；FPG-03 改比补丁序列摘要；FPG-44 按 Git `header_callback` 清空配置头、命令行头与 `--to`/`--cc` 并补顺序场景；FPG-31 → FPG-31（`format.from` 取值）, FPG-46（`--no-from`/`--from` 覆盖配置，补裸 `--from` 覆盖）；FPG-23 → FPG-23（默认关闭、`--in-reply-to` 单独使用、`false`/空值）, FPG-47（启用取值与命令行覆盖）；FPG-11 补仓库外哨兵不写入；FPG-15 归一化去掉封面信信封与 `Date:`；FPG-16 封面信首行改用 #893 判据正则；FPG-38 `format.attach=true` 按 Git 为字面 boundary；FPG-19 规定分叉基线夹具；FPG-45 写明夹具并以 `serde_json::Value` 解析；FPG-27 非法 filter 文案对齐 `invalid argument to diff-filter`；FPG-40 固定 t4014「excessive subject」夹具并补显式 64；FPG-45 夹具 argv（R23 Claude P2） | FPG-31 → FPG-31, FPG-46; FPG-23 → FPG-23, FPG-47 | 文头、评审结论、ADR-FPG-03、执行检查必备需求、实施顺序、Phase 0–2、粒度审计表、FPG-02/03/11/15/16/19/22/23/25/26/27/28/30/31/38/40/44/45/46/47、测试矩阵、追溯表、里程碑、风险登记、Codex review log |
| 2026-11-18 11:43:11 UTC | Claude R24 PASS + Codex R24 FAIL（受审 SHA-256 `64d871da62f2c0be617a2fb7c469229baf42f1380819ac50fdfc1e6f9627e805`） | ① 文档块无变化分支在进入第 6、8 步前令 `SITE_SHA=$SITE_BASE`；diffstat 对齐夹具规则豁免 issue 规定的单文件夹具；② FPG-02、FPG-03 `scope` 改为 `M`；FPG-03 补全部非重复范围；FPG-04 补级联多值 helper 的跨层顺序与读取失败单元测试、注入场景改为对清洗后取值的 Git 黄金比较并登记与 Git 的有意差异（FPG-44/37 同步）；FPG-37 承接卡更正；FPG-06 页脚对照同一二进制 `libra --version` 并补无配置 `--signature`；③ 拆卡：FPG-11 → FPG-11, FPG-48（仓库外/不存在前缀与安全，不写入与不暴露路径分条）；FPG-14 → FPG-14（`%(count)`/`%(total)` 分条、`log:` 按 Git 去前缀并说明与 #899 文字的冲突）, FPG-49（`full`、`format.commitListFormat`、无封面信）；FPG-47 → FPG-47（启用取值）, FPG-50（`--thread`/`--thread=<style>`/`--no-thread` 按命令行顺序覆盖配置，#917 第 4 项）；④ FPG-20/26 各补丁文件的 `Subject:` 判据分条；FPG-26 `-v2.2` 的 interdiff 标题按 Git 为 `Interdiff:`；FPG-22/27 写集改为逐文件（`diff.rs`、`diff/render.rs`；`diff.rs`、`diff/compare.rs`、`diff/options.rs`）并重算生产文件数；FPG-27 夹具同时含新增、修改与删除 | FPG-11 → FPG-11, FPG-48; FPG-14 → FPG-14, FPG-49; FPG-47 → FPG-47, FPG-50 | 文头、评审结论、ADR-FPG-03、执行检查必备需求、实施顺序、Phase 1/2、粒度审计表、FPG-02/03/04/06/11/14/20/22/26/27/37/44/47/48/49/50、测试矩阵、追溯表、里程碑、风险登记、Codex review log |
| 2026-11-18 11:56:52 UTC | Claude R25 FAIL + Codex R25 FAIL（受审 SHA-256 `887d19d54eb5bb9dddc5728086e3dba46680f3ab988519f96f99ed20cada9106`） | ① 协议：「变更段」补「不含 `---` 行时为空串」并新增「邮件正文」；「归一化线程结构」改为按 Perl `$printing` 状态机（线程头后的空行亦输出）、逐行移植并以线程头紧接空行的夹具对照，线程黄金比较夹具至少 3 个补丁；完成判据的 `status`/`rev-parse`/`diff` 检查退出码并用 `--untracked-files=all`；② 网站检出路径依据改为「用户 2026-11-18 11:55:09 UTC 确认」，`AGENTS.md:135` 登记为已知文档滞后（本计划不改该文件）；③ 按 `git show 6de20f6092:builtin/log.c` 更正：`--commit-list-format` 无 `full` 关键字（FPG-49 改为按无效格式串拒绝，并补 `format.commitListFormat` 的 t4014 配置用例、命令行覆盖配置、配置不隐含封面信；#899/#909 冲突在关闭 issue 时说明）；`--interdiff` 直接对比 `<rev>` 与末个提交的树（FPG-19 撤销 R5 的系列级求差描述）；签名页脚按 `print_signature` 以空行结尾（FPG-06）；④ FPG-48 改比邮件正文并接收探针证据；FPG-04 AC1 拆为 `To:`/`Cc:`；FPG-44 补 `format.headers` 的 `Cc:` 与 `--cc` 折叠（t4014#15）；FPG-37 注入黄金按 `sanitize_header_value` 规则并注明有意差异；FPG-26 AC1 改用长选项 `--reroll-count=4.4`；FPG-39 封面信文件存在与重命名夹具；FPG-47 测试改名；⑤ 拆卡：FPG-30 → FPG-30（#897 按需 MIME 头）, FPG-51（#881 t4036 #3–#5，补非 ASCII 签名行原样写入） | FPG-30 → FPG-30, FPG-51 | 文头、文档职责、当前缺口、评审结论、ADR-FPG-01/03、GC-05、执行检查必备需求（验证执行协议）、实施顺序、Phase 0、粒度审计表、FPG-02/04/06/08/11/14/19/26/30/37/39/42/44/47/48/49/51、测试矩阵、追溯表、里程碑、风险登记、Codex review log、完成判据 |
| 2026-11-18 12:04:01 UTC | Claude R26 PASS + Codex R26 FAIL（受审 SHA-256 `10ed04d12ffb5db1fe5a91ed40b3417ed5bd56c4af26401c0218bd3b45caf628`） | ① 协议：新增「补丁 diff 段」「补丁 diffstat 段」「保留外部 ID 的归一化线程结构」与「各封补丁邮件的规范头区（按输出顺序拼接）」口径；完成判据在每次记账提交后与最终标记前再检查工作区（`status --untracked-files=all`，退出码）；② 按 `git show 6de20f6092:pretty.c` 更正 FPG-30：仅作者名非 ASCII 时只编码 `From:`、不输出 MIME 头（#897 第 2 项冲突在关闭 issue 时说明），补非 ASCII 主题触发 MIME 头；FPG-51 签名行限定在邮件正文；③ FPG-02 AC7 限定未设 `GIT_COMMITTER_EMAIL` 且 `EMAIL` 不回退；FPG-04/44/37 改为两补丁夹具逐封比较；FPG-29 短标题改为完整 `Subject:` 黄金比较；FPG-09 成功路径改为 `Subject:` 行序列单一判定；FPG-24 补 `^main rebuild-1` 分支描述（#892 第 4 项）；FPG-22 更正 `CommitDiffOptions` 锚点至 `log.rs:4254-4270`、默认值拆为上下文字段与既有 4 字段两条、改比补丁 diff 段；FPG-27 改比补丁 diffstat 段并补 diff-filter 默认值单元测试；FPG-39 补 `d/` 补丁内容；FPG-23/35 的 `--in-reply-to` 场景改用保留外部 ID 的归一化；④ 拆卡：FPG-26 → FPG-26（文件名前缀，AC1 用长选项）, FPG-52（`Subject:` 原样含 `-v4rev2`、`-v2.2` interdiff 标题） | FPG-26 → FPG-26, FPG-52 | 文头、当前缺口、评审结论、ADR-FPG-03、执行检查必备需求（验证执行协议）、实施顺序、Phase 2、粒度审计表、FPG-02/04/09/20/21/22/23/24/26/27/29/30/35/37/39/44/51/52、测试矩阵、追溯表、里程碑、风险登记、Codex review log、完成判据 |
| 2026-11-18 12:09:09 UTC | Claude R27 PASS + Codex R27 FAIL（受审 SHA-256 `f5d4677ed7e018fb6058856d0d98b53c2221d6c3025285a3062293d91a248ce7`） | ① 文档块：页面只调整顺序、无合格新增或删除短语时改用顺序探针 `ORD_A`/`ORD_B`（第 3 步选定、第 8 步在线核验），三类探针都选不出即 STOP；② 按 `git show 6de20f6092:builtin/log.c:1311-1320` 更正 FPG-25：`auto` 阈值为格式化主题的字节长度，AC6 夹具改为不足 100 字符但超过 100 字节的非 ASCII 首段；③ FPG-30 AC4 只断言不输出 MIME 头（`From:` 编码由 FPG-08 AC2 验收），并登记既有 `no_attach_stays_plain_text` 的行为迁移；FPG-22 默认值按字段拆为 4 条并固定可区分 `-U1`/默认/`--unified=5` 的夹具；FPG-23/50 无线程场景改为归一化线程结构黄金比较（同时判定三封邮件）；④ 拆卡：FPG-02 → FPG-02（成功路径，补 `user.useConfigOnly` 下 `GIT_COMMITTER_EMAIL` 优先，529 探针 #37）, FPG-53（3 类失败路径） | FPG-02 → FPG-02, FPG-53 | 文头、评审结论、ADR-FPG-01/03、执行检查必备需求（文档与网站验收命令）、实施顺序、Phase 0、粒度审计表、FPG-02/03/22/23/25/30/50/53、追溯表、里程碑、风险登记、Codex review log |
| 2026-11-18 12:12:37 UTC | Claude R28 PASS + Codex R28 FAIL（受审 SHA-256 `046ba94c3c2ce6d050af986cd037aa9d1dbf17fd72faf52343f46ec492d6ae41`；分区 e PASS） | ① 文档块：页面无变化分支须对当前生产部署取证（提交为 `$SITE_SHA` 或其后继且其中 `$PAGE` 摘要等于 `FINAL`），不再允许只引用历史部署；完成判据同步；② 协议第 4 项：非 `--stdout` 成功路径的写出文件集合等于同夹具同命令下 Git 的文件名集合（映射只断言部分文件时不缩小该集合）；③ FPG-41 补 `format.notes=note1` 下 `--no-notes --notes=note2` 只输出 note2（t4014「format-patch with multiple notes refs」）；FPG-30 补 `--from` 改写后非 ASCII in-body `From:` 触发 MIME 头；FPG-12 补 `format.coverLetter=false` 下 `--cover-letter` 覆盖配置；FPG-13 AC1 改比封面信正文（同时判定 diffstat 位于列表之后）；FPG-20/52 的补丁 `Subject:` 映射写明文件前缀 | N/A | 文头、执行检查必备需求（验证执行协议、文档与网站验收命令）、FPG-12/13/20/30/41/52、Codex review log、完成判据 |
| 2026-11-18 12:17:03 UTC | Claude R29 PASS + Codex R29 FAIL（受审 SHA-256 `386d9c7ca6892f65c2cb2849715f7b274db4e210038c0b1a298d25789d906e2d`；分区 a、e PASS） | ① 协议「规范头区」排除 `Date:`（Libra 取提交者时间的 UTC 表示，Git 取作者时间与时区），新增 `DEFER-FPG-02` 登记该差异，FPG-16 AC4 改为如实的非回归判据；② 按 `git show 6de20f6092:builtin/log.c` 第 1702 行起的 `get_base_commit` 更正 FPG-17：以整个修订列表求唯一 merge-base 并校验共同祖先、祖先关系与不在列表内，补多分支修订列表的成功与「base 在修订列表内」拒绝两条（`base-commit:` 按 `print_bases` 只输出一次）；③ FPG-46 登记 `--no-from` 与 #915 字面验收的冲突（Git `from_callback` 回到作者身份）；FPG-18 补 `--attach=mimemime` 附件分段（`text/x-patch`、disposition 与 diff）的 Git 黄金比较 | N/A | 文头、执行检查必备需求（验证执行协议）、FPG-16/17/18/46、非目标与延后项、Codex review log |
| 2026-11-18 13:17:49 UTC | Claude R30 PASS + Codex R30 FAIL（受审 SHA-256 `8a61871f353b48de63bf4e6cb9e6e2d002466a3e73a4422b2ffe2920ad96cb02`；分区 e PASS）；随后修复前专项清扫（6 个 Claude 子代理按卡分区逐 AC 核对夹具区分力、Git 上游与 Libra 源码） | ① 撤销 `DEFER-FPG-02`：`FPG-16` 按 #893 第 3 项把补丁 `Date:` 改为 Git 的作者时间与作者时区、封面信信封改用系列末个提交 oid，夹具作者 ≠ 提交者且作者时区 `+0800`（Codex a/d）；② `format.numbered` 按 Git 逐条处理多层与同层取值，自 `FPG-32` 拆出 `FPG-56`（Codex b）；③ `FPG-13` 同卡移除封面信正文多余的 `*** SUBJECT HERE ***` 行并单列 AC，按上游更正 `--no-stat`/`-p` 仍输出、`--root` 不输出系列 diffstat（Codex c）；④ `FPG-24` 夹具区分当前分支与 `find_branch_name` 选中的分支（Codex d）；⑤ 清扫所得：协议第 4 项改为全量快照前后状态与「只断言稳定错误码」的失败契约，第 3 项新增「差异隔离夹具」（避开 529 `DEFER-FP-01`/`DEFER-FP-02`）、提交标题约束与「封面信 diffstat 后段」，文档块改为进程内比对不落临时文件；`FPG-03` 按 Git 只取 merge base 之后的上游 patch-id；`FPG-15`/`FPG-03` 改比补丁选择；`FPG-17` base 取 merge-base 并只放第一封邮件；`FPG-34` interdiff 位于补丁 diff 之后且有封面信时只在封面信；`FPG-28` 线程头移到信封之后；`FPG-10`/`FPG-31`/`FPG-46` ident 校验时机与覆盖；`FPG-11`/`FPG-21`/`FPG-45`/`FPG-40`/`FPG-35`/`FPG-30`/`FPG-42`/`FPG-07`/`FPG-08`/`FPG-29`/`FPG-05`/`FPG-36`/`FPG-41`/`FPG-01`/`FPG-14`/`FPG-43`/`FPG-33`/`FPG-25`/`FPG-39`/`FPG-18`/`FPG-19` 补夹具或 AC；`FPG-23` 的 `format.thread` 关闭取值并入 `FPG-47`；新增 `DEFER-FPG-03`（diffstat `--summary` 行，`#905` 部分闭环）、`DEFER-FPG-04`（不在 35 项内的已知 CLI 差异）；⑥ 拆卡：`FPG-32` → `FPG-32`, `FPG-56`；`FPG-37` → `FPG-37`, `FPG-57`；`FPG-06` → `FPG-06`, `FPG-54`（先于 `FPG-06` 执行）, `FPG-55` | FPG-32 → FPG-32, FPG-56；FPG-37 → FPG-37, FPG-57；FPG-06 → FPG-06, FPG-54, FPG-55 | 成功定义、当前缺口、评审结论、ADR-FPG-02、执行检查必备需求（验证执行协议第 3–4 项、文档与网站验收命令）、实施顺序、依赖登记表、Phase 0、粒度审计表、多张任务卡、追溯表、里程碑、风险登记、Codex review log、非目标与延后项 |
| 2026-11-18 13:31:25 UTC | Claude R31 未完成（仅分区 1 即 `FPG-01`..`FPG-05` 区间 PASS 附 7 条 P2；分区 2–5 因上一接续 Agent 上下文耗尽中止，不构成全文结论）+ Codex R31 FAIL（受审 SHA-256 `f4d410a18e48f3e33868036eebaa46e9d9259f54d656db455ca84e8518ff300f`；5 分区均 FAIL，共 12 条 P1） | ① 协议第 4 项：非 `--stdout` 成功路径允许新增路径含为输出文件新建的输出目录及其祖先目录（`create_dir_all`），夹具预建时只有输出文件（Codex a）；② 单轴拆卡：`FPG-13` 的封面信正文模板拆出 `FPG-58`（先于 `FPG-13` 执行，`Subject:` 头单列 AC，Codex a/d）；`FPG-16` 的补丁 `Date:` 作者时间与作者时区拆出 `FPG-59`（`FPG-16` 只保留信封时间戳与封面信信封 oid，Codex a）；③ `FPG-56` 增同层 `true` 后接 `false` 夹具，可区分只读首值的实现（Codex b）；④ `FPG-57` 粒度按写集改为 `landing=3; prod-files=2`（Codex b）；⑤ `FPG-05` AC4 夹具改为 `--notes=note2 --notes=note1 --notes=note2`（Codex b）；⑥ `FPG-11` 夹具根目录 `file` 改为与 `sub/dir/file` 等长的 `top/dir/file`，满足差异隔离（Codex c）；⑦ `FPG-17` AC6 改为整个修订列表求得的 base 不同于单个修订的拒绝夹具，并以互换的两种参数顺序各运行一次（Codex d）；⑧ `FPG-45` 按 Git `set_outdir` 区分绝对、相对与默认输出目录，新增绝对 `-o` 的 AC6（Codex e）；⑨ `FPG-39` AC3/AC5 改比去掉 `@@` hunk 头行后的完整补丁 diff 段，描述与 AC6 去掉数值 `128`、只断言 `LBR-IO-002`（Codex e）；⑩ 采纳 Claude R31 分区 1 的 P2：`FPG-57` AC1 改为交错夹具（`T1`、`H`、`T2`）以区分按键分组拼接、AC6 改用小写键 `to: `、AC7 注明 Git 出处，`FPG-44` 描述注明其后 `--to` 由 `FPG-57` AC7 验收，`FPG-04` 限定夹具不混合 `To:`/`Cc:` 与其它附加头，`FPG-03` 清除残留的「tag 场景修复」措辞；P2 第 5 条已由 ⑤ 关闭；第 7 条驳回（4 个仓库文档当前不含 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`，标记计数可大于基线）；⑪ 与其它计划的关系登记 529 `DEFER-FP-01`/`DEFER-FP-02` 由差异隔离夹具规避并经 `DEFER-FPG-03` 承接；`GAP-FPG-13`/`GAP-FPG-16` 改指 `FPG-58`/`FPG-59` | FPG-13 → FPG-58, FPG-13；FPG-16 → FPG-16, FPG-59 | 文头、当前缺口、与其它计划的关系、评审结论、执行检查必备需求（验证执行协议第 4 项）、实施顺序、Phase 1、粒度审计表、FPG-03/04/05/11/13/16/17/39/44/45/56/57/58/59、追溯表、里程碑、风险登记、Codex review log |
| 2026-11-18 13:39:26 UTC | Claude R32 FAIL + Codex R32 PASS（受审 SHA-256 `1eb427832f4c042c8bed0c05211fbf33fd6d76f0e5ed140fe0e97eeb51cd5b71`；Codex 5 分区均 PASS） | ① `FPG-16` 夹具定为两补丁系列，AC4/AC5 改为对 AC1 的同一 `-2 --stdout` mbox 在独立接收仓库中应用、断言新增两个提交及其标题序列（原映射「新增一个提交」在两补丁 mbox 下对正确实现也失败，Claude P1）；`FPG-59` 夹具写明两个提交共用同一作者时间与提交者时间，AC2/AC3 断言新增的两个提交均保留作者时间与时区（同一 P1）；② P2 采纳：成功定义的 `#905` 闭环卡补 `FPG-58`；当前缺口表「计划动作」补全拆出卡（GAP-01/02/04/06/11/19/20/23/25/31）；Phase 1 目标补正文模板与补丁 `Date:`；`FPG-39` AC5 文字收窄到实际比较的 diff 段（Codex e P2 同提）；③ 登记残余：`FPG-33` AC2/AC3/AC5 与不读配置的实现结果相同（AC1/AC4/AC7 已区分），不扩卡 | N/A | 文头、成功定义、当前缺口、Phase 1、FPG-16、FPG-59、FPG-39、Codex review log |
| 2026-11-18 13:42:30 UTC | Claude R33 PASS + Codex R33 FAIL（受审 SHA-256 `a86a8d1773fa763a5d7f21030fc0a40df95a531a6ae456460cc706ba56d1c2fd`；分区 b、c PASS，a、d、e 各 1 条 P1） | ① Codex review log 的 `Result` 列只允许 `PASS`/`FAIL`：把「R30 补充」清扫与「R31 Claude 未完成」两行移入新增的「非评审轮与未完成评审记录」表，保留其真实状态，R31 Codex 行范围补受审 SHA-256（Codex a）；② `FPG-33` AC3 改用格式化后首段不超过 100 字节、标题与正文可区分的夹具（`auto` 期望同 `subject`，不同于默认 `message`），阈值另一侧由 `FPG-25` AC5/AC6 验收（Codex d）；③ `FPG-20` AC6 改为比较写出的 `v23-0000-cover-letter.patch` 的封面信 diffstat 后段与 Git 黄金值（夹具同 `FPG-19` AC1），同时判定 interdiff 标题、正文与位置（Codex e）；④ P2 采纳：`FPG-04` AC1/AC2 改为逐封比较同名头，保留邮件边界（Codex b）；`FPG-10` AC2 由「头区含」收紧为头区唯一 `From:` 行逐字相等（Claude 自查） | N/A | 文头、FPG-04、FPG-20、FPG-33、Codex review log（新增非评审轮记录表） |
| 2026-11-18 13:46:42 UTC | Claude R34 PASS + Codex R34 FAIL（受审 SHA-256 `c504ed8909d93351f39688ba65d47d92d79459cdd197d1673ee424cfef3c3605`；分区 b、c PASS，a 2 条、d 1 条、e 2 条 P1） | ① `DEP-FPG-01` 可用性判据与失败策略明确要求 `issues/529#CLOSE-529` 全量收口完成，与文中其它处一致（Codex a）；② Codex review log 中结论为 `FAIL` 但无 P0/P1 的 9 行（R5 Claude 中断、R7 Codex、R9–R14 Claude 未运行/未重跑、R15+ Codex 未编号复审）移入「非评审轮与未完成评审记录」表，如实保留原结论（Codex a）；③ `FPG-13` 更正 `--root` 规则：只有系列含根提交或边界不唯一时不输出系列 diffstat，`--root` 本身不抑制；AC5 改为 `--root HEAD`，新增 AC7（`--root HEAD~2..HEAD` 仍输出），AC 6→7（Codex d）；④ `FPG-34` AC1–AC3 启用 `--signature=SIG`，黄金比较同时判定 interdiff 位于签名分隔之前（Codex e）；⑤ `FPG-28` AC5 加 `--in-reply-to`，改为逐封比较头字段名序列，覆盖三个线程头相对信封与 `From:` 的位置（Codex e） | N/A | 依赖登记表、FPG-13、FPG-28、FPG-34、粒度审计表、Codex review log |
| 2026-11-18 13:49:28 UTC | Claude R35 PASS + Codex R35 FAIL（受审 SHA-256 `7f307276cddab0dc47199c077e907c733067d985da65c37e9243c46811918ccc`；分区 a、b、d、e PASS，c 1 条 P1） | ① `FPG-07` 描述声称的封面信列表与 `%s` 使用拼接后首段补齐验收：新增 AC6（默认 shortlog 列表）、AC7（`modern` 列表）、AC8（自定义格式 `%s`），分别对应现行三处只取首行的 `commit_subject_line` 调用（`src/command/format_patch.rs:1552,1565,1632`），AC 5→8（Codex c）；② P2 采纳：协议第 3 项「补丁 diff 段」注明映射显式启用签名时含签名块（与 `FPG-34` 一致，Codex e）；③ Claude 自查「描述承诺而无 AC」同类缺口并补齐：`FPG-21` AC4 夹具含连续 `.` 与末尾 `.`；`FPG-47` 描述按 Git 改为关键字 `strcasecmp`、AC2 用 `Shallow`；`FPG-55` 的 `git_config_pathname` 路径展开移入 Out of scope；`FPG-24` 新增 AC5（无 revision 时取当前分支），AC 4→5 | N/A | 执行检查必备需求（验证执行协议第 3 项）、FPG-07、粒度审计表、Codex review log |
| 2026-11-18 13:54:03 UTC | Claude R36 PASS + Codex R36 FAIL（受审 SHA-256 `7169bf454ece2cebba236d40fc1a176e588780a9549e9e3959d29464f9d9d3e3`；分区 a、c、e PASS，b 1 条、d 2 条 P1） | ① `FPG-17` AC6 由「两种参数顺序各调用一次」（按 G-03 计 2 条、使本卡超 8）改为单次调用的三分支夹具：`side1`、`side3` 自 `A` 分出、`side2` 自 `B` 分出、时间依次递增，`^B side1 side2 side3` 按 Git 拒绝，按命令行或时间顺序只取首个/末个修订的实现均得到 `A` 并成功（Codex d）；② `FPG-57` AC2/AC3 夹具同时含 `To:` 与 `Cc:`，映射改为规范头区黄金比较，验证 `--no-to`/`--no-cc` 只清空对应列表（Codex b）；同类自查：`FPG-37` AC4/AC6 同样改为含另一类收件人的夹具与黄金比较；③ `FPG-43` 新增 AC7：有上游时全局 `true`、本地 `false` 不输出 base 区块，AC 6→7（Codex d） | N/A | FPG-17、FPG-37、FPG-43、FPG-57、粒度审计表、Codex review log |
| 2026-11-18 13:59:34 UTC | Claude R37 PASS + Codex R37 FAIL（受审 SHA-256 `378e341e5b933d69762af373957bd4d9ae4ec5958e13491d7074b0b6edcffdf5`；分区 c、e PASS，a、b、d 各 1 条 P1） | ① ADR-FPG-01 的 MIME 判定改为与 `FPG-30`/`FPG-42`/`FPG-51` 一致（补丁按提交说明与 in-body `From:`，封面信按提交对象缓冲区，仅作者名非 ASCII 只编码 `From:`，diff/notes 不触发），长 `Subject:` 折叠口径改为 78 显示列（Codex a；折叠口径为 Claude 同步自查）；② `FPG-03` 把 `b..a` 更正为两端可达集合之差（线性分叉时即 merge base 之后的上游提交），GAP-FPG-03、Current evidence、AC3、文档行同步；多 merge base 历史的单列验收不在 #882 验收标准内，按范围控制登记 `DEFER-FPG-05`（#882 仍由 `FPG-03` 完整闭环）（Codex b）；③ 单轴拆卡：`FPG-24` 的封面信默认 `From:` 身份拆出 `FPG-60`（排在 `FPG-24` 之后、`FPG-25` 之前），`FPG-24` 保留分支描述 AC1–AC3（原 AC3–AC5），`FPG-60` 承接原 AC1–AC2 并改为头区唯一 `From:` 行断言；`FPG-12` 描述与 Out of scope、GAP-FPG-24、DAG、Phase 1、审计表、追溯表、卡数 59→60 同步（Codex d） | FPG-24 → FPG-24, FPG-60 | ADR-FPG-01、当前缺口、评审结论、实施顺序、Phase 1、粒度审计表、FPG-03、FPG-12、FPG-24、FPG-25、FPG-60、追溯表、里程碑、风险登记、Codex review log、非目标与延后项 |
| 2026-11-18 14:05:25 UTC | Claude R38 PASS + Codex R38 FAIL（受审 SHA-256 `f3452cddbc094cad6c8e06f911fb20b40c4e1f58412b867cc732f2853b98b2ba`；分区 a–d PASS，e 3 条 P1） | 按 G-01 单轴拆卡（Codex e）：① `FPG-22` 只保留 `-U`/`--unified` 上下文行数（AC 8→7，命名测试改为 `unified_context_lines`），`--full-diff` 接受拆出 `FPG-61`；② `FPG-39` 只保留重命名选项与封面信继承 diff 选项（AC 8→7），#904 第 2 项封面信写入失败且不留部分输出拆出 `FPG-62`；③ `FPG-28` 只保留 Message-ID 唯一性（AC 5→4），线程头移到信封行之后拆出 `FPG-63`，按 `FPG-54` 先例排在 `FPG-28` 之前执行；DAG 改为 `FPG-22 -> FPG-61 -> FPG-27 -> FPG-39 -> FPG-62 -> FPG-63 -> FPG-28`，`FPG-27`/`FPG-28` 依赖、审计表、追溯表、Phase 2 与卡数 60→63 同步 | FPG-22 → FPG-22, FPG-61；FPG-39 → FPG-39, FPG-62；FPG-28 → FPG-28, FPG-63 | 文头、评审结论、实施顺序、Phase 2、粒度审计表、FPG-22、FPG-27、FPG-28、FPG-39、FPG-61、FPG-62、FPG-63、追溯表、里程碑、风险登记、Codex review log |
| 2026-11-18 14:09:15 UTC | Claude R39 PASS + Codex R39 FAIL（受审 SHA-256 `5e2baeebdd3b5559e518ccf4f88b7d6e97fe88b1c08764c48a2b31e6c2dd05a2`；分区 a–c PASS，d 1 条、e 2 条 P1） | ① `FPG-17` 的 base 区块位置（显式 `--base` 与 `--base=auto` 共用）拆出 `FPG-64`，先于 `FPG-17` 执行，`FPG-17` AC 8→7、VER 3→2（Codex d）；② `FPG-62` 把「以写入失败退出」与「不留部分输出」分为 AC1、AC2（Codex e）；③ Codex e 关于 G-09「新卡正文须追加到 Phase 末尾」的 P1 不采纳：G-09 原文为「新子卡在所属 Phase 末尾追加新编号，不重排既有编号」，约束的是编号而非正文位置（本计划 `FPG-32`..`FPG-60` 均按执行位置插入且历轮评审接受）；为消除歧义，「使用规则」写明卡片正文按「实施顺序」排列，并把正文整体重排为与 DAG 完全一致（原 `FPG-29` 与 `FPG-08`、`FPG-23`/`FPG-47`/`FPG-50`/`FPG-35`/`FPG-28` 的正文位置与执行顺序不一致，一并更正，卡片内容不变）；④ P2 采纳：GAP-FPG-17/22/27/28 计划动作补 `FPG-64`/`FPG-61`/`FPG-62`/`FPG-63`；测试矩阵「全部 35 项 issue」改为本计划承接的 issue 并注明 `#880`、`DEFER-FPG-03` 例外（Codex a） | FPG-17 → FPG-64, FPG-17 | 使用规则、当前缺口、评审结论、实施顺序、Phase 1、粒度审计表、任务卡正文顺序、FPG-17、FPG-62、FPG-64、测试矩阵、追溯表、里程碑、风险登记、Codex review log |
| 2026-11-18 14:11:35 UTC | Claude R40 PASS + Codex R40 FAIL（受审 SHA-256 `d0ff84ca58b13fe3b9fed5e6199f91f877f7cac8f0bca501300a290b50238f24`；分区 a、b、c、e PASS，d 1 条 P1） | `FPG-64` 夹具改为 `--base=HEAD~3 -2`（`HEAD~2` 为前置提交，区块含 `prerequisite-patch-id:`），AC1 改为逐封比较 `base-commit:`/`prerequisite-patch-id:` 行序列与 Git 黄金值，新增 AC2 覆盖有封面信时只在封面信、补丁不重复，AC 1→2（Codex d） | N/A | FPG-64、粒度审计表、Codex review log |
| 2026-11-18 14:13:26 UTC | Claude R41 PASS + Codex R41 PASS（受审 SHA-256 `7249da6bea021ee9524d3c00b0198761b9d0b632e33b81e09aba780a5287ebab`） | 记账：双审对同一受审字节通过，本行与 review log 两行及文头状态行为 PASS 后仅有的改动 | N/A | 文头、修订历史、Codex review log |

## 已决议设计决策

### ADR-FPG-01: 邮件头默认编码、MIME 省略与换行折叠策略

- **Status:** Accepted
- **Context:** #886、#887、#897、#911 涉及 `Subject:` 多行拼接、超长折叠、默认 RFC 2047 编码以及纯 ASCII 补丁省略 MIME 头。
- **Decision:** 默认启用非 ASCII 邮件头的 RFC 2047 Q 编码（`--no-encode-email-headers` 可关闭）；补丁邮件仅当提交说明（含主题与签名行）或 in-body `From:` 含非 ASCII，或启用 `--attach`/`--inline` 时才输出 `MIME-Version` / `Content-Type` / `Content-Transfer-Encoding`（`FPG-30`、`FPG-51`；仅作者名非 ASCII 时只对 `From:` 头做 RFC 2047 编码，diff 与 notes 内容不触发），封面信按系列提交对象缓冲区是否含非 ASCII 判定（`FPG-42`）；超长 `Subject:` 按 Git `strbuf_add_wrapped_bytes` 以 78 显示列在词间折行、续行前导单空格（`FPG-29`）。
- **Alternatives considered:** 维持始终输出 MIME 头且默认不编码（与 Git 行为及 t4014/t4036 不兼容，拒绝）。
- **Consequences:** 需同步更新依赖固定 MIME 头的既有 `format_patch_test` / `compat_format_patch_mail_roundtrip` 断言；`#881` 的 t4036 端到端闭环在 `FPG-51` 完成时达成（依赖 `FPG-30` 的 #897 按需 MIME 头；身份切片由 `FPG-02`、`FPG-53` 核实）。
- **Revisit when:** 若后续引入 RFC 6532 SMTPUTF8 模式再扩展开关。

### ADR-FPG-02: 配置项级联与 CLI 顺序覆盖语义

- **Status:** Accepted
- **Context:** #883、#884、#885、#896、#900、#906、#912、#914、#915、#917 要求补齐十余个 `format.*` / `diff.relative` / `branch.<name>.description` 配置及 `--no-to`/`--to` 等顺序覆盖。
- **Decision:** 所有单值配置通过 `read_format_config`（`read_cascaded_config_value_strict`）按 local → global → system 严格读取并校验类型，多值键（`format.headers`/`format.to`/`format.cc`/`format.notes`）经 `FPG-04` 在 `src/internal/config.rs` 新增的级联多值 helper 按 Git 顺序读取全部值；对于可重复与否定成对参数（`--to`/`--no-to`、`--cc`/`--no-cc`、`--notes`/`--no-notes`、`--from`/`--no-from`），按命令行参数出现位置顺序求值，后者覆盖前者及配置列表。`format.headers` 中的 `To:`/`Cc:` 值、`format.to`/`format.cc`、`--add-header` 中的 `To:`/`Cc:` 与 `--to`/`--cc` 进入同一累积列表（Git `extra_to`/`extra_cc`），配置侧跨键按条目出现顺序合并，`--no-to`/`--no-cc`/`--no-add-header` 清空该列表；非 `To:`/`Cc:` 的附加头先于合并后的 `To:`、`Cc:` 输出（`FPG-57`）。`format.numbered` 按 Git 逐条处理全部取值（`true` 之后的 `auto` 不清除编号，`FPG-56`）。
- **Alternatives considered:** 仅用布尔 flag 覆盖（无法支持 `--no-to --to addr`，导致 #914 无法解决，拒绝）。
- **Consequences:** 需在解析入口保留参数位置索引。
- **Revisit when:** 若全局引入统一可否定多值 CLI 解析器可统一迁移。

### ADR-FPG-03: 共享写集 `format_patch.rs` 的串行化与 `529.md` 错峰策略

- **Status:** Accepted
- **Context:** `issues/529.md` 当前持有 `src/command/format_patch.rs` 写锁；本计划 64 张卡均修改 `src/command/format_patch.rs`。
- **Decision:** 本计划首卡 `FPG-01` 依赖 `DEP-FPG-01`（`issues/529#CLOSE-529` 完成）；计划内部采用线性依赖链（G-10）。
- **Alternatives considered:** 与 `529.md` 并发修改同一文件或合并为巨型卡（违反 G-01/G-10，拒绝）。
- **Consequences:** 无并发写冲突，每卡均为独立可回滚发布切片。
- **Revisit when:** `529.md` 合入并关闭后 `DEP-FPG-01` 解除。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01`..`GC-13`。特别强调：
- **GC-01 现状核实前置:** 每卡开工前必须重新核对 `issues/529.md` 合入后的最新代码，已满足部分只补测试/文档并关闭。
- **GC-05 / ER-06a 文档同步:** 每张改变公开 CLI/配置/输出行为的卡必须同卡更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md`（第 224 行原地修改），并沿用 `AGENTS.md:135` 的网站交付流程（检出路径为用户 2026-11-18 11:55:09 UTC 确认的 `/Volumes/Data/gitmono/libra-backend`；`AGENTS.md:135` 中的 `/Volumes/Sea/libra-backend` 为已知文档滞后，见「文档职责」）在 `cf` 分支同卡交付（保留无关改动、不建新分支/PR/worktree）：用同步脚本生成 `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（禁止手改生成页），具体命令与判据见「执行检查必备需求」的「文档与网站验收命令」第 1–8 步；若 `/Volumes/Data/gitmono/libra-backend` 不可用，fail-closed 停止网站交付，除非用户明确授权替代检出。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01`..`ER-14`：
- **ER-04 / ER-08 / ER-13 / ER-14:** 本计划每张 `implementation` 卡均为 `independent` 发布切片，完成后按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 权威三处（`Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`；因每张卡均修改 `install.sh`/`install.ps1`（`T-1`），C 组第 ④ 步除运行本卡 A 组 focused `nextest` 外，必须跑双环境全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`、`fmt`、`clippy`、`cargo build --release`，随后签名提交推送 PR；合并前核对合并方式：若采用 squash，则 squash 后主分支上的新提交本身须含有效 `gpgsig` 与 `Signed-off-by:`（按 ER-07 在同一提交上校验）且其树与已验收的 PR head 树相同，否则改用保留已签名提交的受保护合并方式，仍无法满足即 STOP；合并后 `gh release create v<version>`。
- **ER-04 D 组远端后置门（每张 `independent` 卡默认适用）:**
  - `base.yml`（PR / `push` `main`）：`format`、`clippy`、`redundancy`、`test` 全部 green。
  - `codeql.yml`（PR / `push` `main`）：CodeQL 扫描 green。
  - `release.yml`（`push` `v*` tag，由 `gh release create "v${VERSION}"` 触发）：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 green；并通过 `gh run list --workflow=release.yml` 及 `download.libra.tools` / stable manifest（`HEAD` 200、range `GET` 206）核验发布产物与 CDN 可获取性。

- **验证执行协议（对全计划每条 `Verification` 条目强制）：** 每条 `Verification` 只属于下列两类之一，命令一律带 `--no-tests=fail`，并按该类判据执行；任一判据不满足即 STOP。
  1. **具名验收门（卡特有 AC 证据）：** 过滤式只能是一个精确项 `test(=<完整测试名>)`，或一个全锚定（`^…$`）、除字面量交替 `(<字面量1>|<字面量2>|…)` 外不含任何正则元字符的正则（如 `test(/^command::format_patch_test::fp03_prefix_k__(msg|code|exit|stdout|fs)$/)`）；条目必须写出期望集合（N 个完整测试名）。运行前用同一 target 与同一过滤执行 `cargo nextest list`（例如 `source .env.test && cargo nextest list --test command_test -E '<同一过滤>'`），列出的名称集合须与期望集合逐字相等（缺、多或为 0 均 STOP）；运行后记录 exit code 与「匹配 / 通过 / 失败 / 跳过」四计数，要求 exit code = 0、失败 = 0、跳过 = 0、通过 = 匹配 = N。条目须写明所证明的 AC 编号。
  2. **共享回归组（不计为卡特有 AC 证据）：** 整个 `--test` target，或 `test(/^command::format_patch_test::/)`、`test(/^command::(log|show|stash)_test::/)` 这类模块正则。运行前用同一 target 与过滤执行 `cargo nextest list` 记录匹配数 M，要求 M ≥ 1；运行后要求 exit code = 0、失败 = 0、跳过 = 0、通过 = M，并记录四计数。共享回归组列在每卡单独的「共享回归组」小节（不在 `Verification` 内）。本卡未点名的既有用例不得删除或弱化断言；本卡有意改变的既有契约（卡内 `Current evidence` 点名的用例，如 FPG-08 的空格编码、FPG-29 的 `encode_email_headers_splits_long_words_under_75_chars`、FPG-11 的 `unsupported_git_options_are_classified`、FPG-17 的 `base_auto_is_rejected`、FPG-19/FPG-34 替代的 `529 FP-10` X 型门族）按「行为迁移」处理：在同卡把该用例改写为新契约（保留其中仍适用的断言；拒绝路径改为成功时原拒绝门无仍适用断言，整组删除并以卡内点名的新成功用例替代），并在卡内记录迁移前后断言。
  3. **AC → 断言映射口径：** 每张卡以表格逐条写出「AC → 具名测试 → 期望退出码 → 可观测断言（输出通道与必须出现 / 必须不出现的具体内容）」。只断言程序真实输出；不得把凭空的 issue 编号、测试名、外部 revision 或占位符当作输出内容，但实际输出中按契约出现的配置键、参数值或 revision 应逐字断言（错误路径只按第 4 项「失败契约」断言）。「Git 黄金值」指在 `git@6de20f6092` 构建的 Git 上对同一夹具运行得到、以常量固化在测试内的期望值（测试注释记录生成命令与 Git 版本；若用其它版本 Git 生成，须先证明该夹具输出与基线 revision 一致，否则改用基线构建）。比较前双方统一加映射行注明的参数（`--no-thread --no-signature` 去除必然不同的线程 ID 与版本签名），且只比较映射行指明的区段。区段定义（按 `--stdout` 输出或单个补丁文件计）：**邮件结束** = 下一封邮件的信封行（以 `From ` 加 40 或 64 位十六进制开头）之前或输出/文件末尾；**头区** = 邮件首行至首个空行之前；**规范头区** = 头区去掉首行信封、`Date:` 行（`FPG-59` 之前 Libra 以提交者时间的 UTC 表示输出，`FPG-59` 起按 Git 取作者时间与作者时区；为使头区比较不受卡序影响一律排除，`Date:` 只由 `FPG-59` AC1 判定）与 `MIME-Version:`、`Content-Type:`、`Content-Transfer-Encoding:` 行（比较时双方另加 `--no-thread`，故无线程头）；映射写「各封补丁邮件的规范头区（按输出顺序拼接）」时，指把全部补丁邮件（不含封面信）的规范头区按输出顺序拼接后做一次比较；**扩展头序列** = 规范头区中 `Subject:` 行（含续行）之后的行；**封面信正文** = 封面信头区之后至邮件结束；**补丁序列摘要** = 按输出顺序，每封补丁邮件（不含封面信）的 `Subject:` 行（含续行）后接其变更段；**`---` 段** = 补丁的 `---` 行至首个 `diff --git` 行之前；**变更段** = 补丁的 `---` 行至邮件结束（补丁不含 `---` 行时为空串）；**补丁 diff 段** = 补丁首个 `diff --git` 行至邮件结束（映射未另写时均加 `--no-signature`，故不含签名块；映射显式启用签名时含签名块）；**补丁 diffstat 段** = 补丁 `---` 行之后首个「以空格开头、含 ` | `」的统计行起至其后首个空行之前（含统计行、汇总行与摘要行）；**邮件正文** = 补丁邮件头区之后至邮件结束（与「封面信正文」同口径，用于不含 `---` 行的补丁）；**notes 段** = 补丁 `---` 行之后、首个 diffstat 统计行（无 diffstat 时为首个 `diff --git` 行）之前的全部行，去掉首尾空行（多个 ref 的 notes 均在其中）；**列表段** = 封面信 commit list：未配置分支描述的封面信（描述为 `*** BLURB HERE ***` 一行）自该行之后的首个非空行起，配置了分支描述的封面信只使用默认 shortlog 样式、自首个形如 `<作者> (<n>):` 的行起；终点为 diffstat 段首行之前（无 diffstat 时为邮件结束），去掉首尾空行；**描述段** = 封面信头区之后至列表段首行之前，去掉首尾空行；**diffstat 段** = 封面信中第一个「以空格开头、含 ` | ` 且竖线前允许任意对齐空白」的统计行起至其后首个空行之前（含全部统计行、汇总行与摘要行）；**封面信 diffstat 后段** = 封面信 diffstat 段的汇总行（形如 ` N file(s) changed`）起至邮件结束；**归一化线程结构** = 逐字复现 t4014 `check_threading` 的变换（`/Volumes/Data/gitmono/git/t/t4014-format-patch.sh:548-567`，以 `git@6de20f6092` 同名函数为准）：输出范围按该 Perl 片段的 `$printing` 状态机：以不区分大小写的 `message-id`/`references`/`in-reply-to` 开头的行置位，其它以非空白字符开头的行清零，空行与以空白开头的续行不改变状态；状态为真时输出该行（经下述 ID 替换；因此线程头之后紧随的头区结束空行也被输出）；每遇不区分大小写以 `From ` 开头的行另输出一行 `---`（保留邮件边界）；ID 处理与该 Perl 片段语义一致：每行只登记该行第一个 `<…>` 内的 ID（尚未登记才分配下一个从 0 起的序号），随后对每个已登记 ID 在该行做一次（首个出现处）子串替换；含多个 ID 的 `References:` 行只登记首个 ID、其余 ID 仅在此前已登记时被替换；替换按 Perl 以 ID 为正则、不带 `g` 的 `s///` 语义；ID 互不为子串时替换顺序不影响结果，ID 互为子串时比较必然失败，故线程黄金比较须在 `FPG-28` 修复 ID 之后执行；测试实现须是该片段的逐行移植，并用含多 ID `References:`、以及线程头紧接头区结束空行的两类夹具与基线 Perl 片段输出对照验证。**保留外部 ID 的归一化线程结构** = 同「归一化线程结构」，但在处理第一行之前先把 `--in-reply-to` 给定的 ID 按 Git `clean_message_id`（`git show 6de20f6092:builtin/log.c` 第 1488 行起）去掉首尾尖括号后的值登记（替换结果为该值本身、不占序号；夹具参数按 t4014 写作 `<test.message>`），双方（Libra 输出与 Git 黄金值）按同一变换处理，使错误的外部父 ID 不会被归一化掩盖；凡以归一化线程结构作黄金比较的映射，夹具均为至少 3 个补丁的系列（与 t4014 线程用例同形），使 shallow 与 deep 的 `In-Reply-To`/`References` 结构可区分。**差异隔离夹具（强制）：** 共享 diff 引擎对空旧侧的 hunk 头（529 `DEFER-FP-01`）与共享 stat 渲染器的版式（名称对齐、计数宽度、`0 deletions(-)` 子句、图形缩放、`--summary` 行，529 `DEFER-FP-02`）与 Git 的差异不属本计划交付（529 FP-05 起补丁 diffstat 与封面信 diffstat 均由该渲染器产生）。凡映射以 Git 黄金值比较的区段包含补丁 diffstat、hunk、封面信 diffstat 或 interdiff，夹具须满足：只修改既有文本文件（不新建、删除、重命名、改模式，不含二进制）；每个被改文件同时含新增与删除行且改动少于 10 行；同一 diffstat 中的路径等长；不得含 `create mode`/`rename`/`mode change` 摘要。卡内夹具因验收对象本身必须违反上述条件时（如重命名、新增文件），该卡只与不含上述单元的区段或行做黄金比较，或改与同一二进制 `libra diff` 的输出比较，并在映射中写明。**提交标题约束：** `FPG-21`（文件名按 Git 清洗）与 `FPG-40`（文件名长度）完成之前执行的卡，凡以 Git 黄金值比较文件名、文件名集合或附件名的夹具，提交标题只含小写 ASCII 字母、数字与单个空格，且清洗后 slug 不超过 40 字节。
  4. **默认前后状态契约**（各卡映射未另写时适用）：`format-patch` 只读仓库——每个用例在执行前后各取一次全量快照并逐项比较：工作区（含输出目录所在的临时根）全部路径集合与每个文件的字节、`.libra/index` 文件字节、`HEAD` 与全部 refs（名称 → 目标）、全部 config 条目（层、键、值与顺序），以及 `.libra/libra.db` 全部表的行集合（逐表全量查询后比较，不比较 SQLite 文件字节）；除下列允许的输出外两次快照须完全相同（只比计数或抽查部分对象不算通过）。成功路径在 `--stdout` 下快照不变，在非 `--stdout` 下新增路径恰为输出文件及为其新建的输出目录（含 `create_dir_all` 新建的祖先目录，`src/command/format_patch.rs:1770-1789`；夹具预建输出目录时则只有输出文件），且文件名集合等于同夹具同命令下 Git 生成的集合（映射另写完整文件集合时以映射为准；只断言其中部分文件内容的映射不缩小该集合）；拒绝路径快照不变。涉及 `libra am`/`git am` 往返的映射一律在独立的临时接收仓库中应用补丁：默认前后状态契约只约束生成补丁的源仓库，接收仓库的预期变化（新增一个提交及其内容）由映射行断言。**失败契约：** 拒绝路径只断言「退出码非 0 + stderr 中独立的一行 `Error-Code: <码>`（稳定错误码）+ stdout 为空 + 快照不变」，映射的期望退出码列写「非 0」；数值退出码可被 `LIBRA_FINE_EXIT_CODES` 等覆盖（`src/utils/error.rs:33,60`），`fatal:`/`error:` 前缀与消息文本属 Libra 自身文案，均不作为与 Git 的兼容契约（Git 的 `fatal:`/128 与 Libra CLI 类稳定码默认 129 之差登记于 `DEFER-FPG-04`）；同一卡内多个失败场景共用同一稳定码时，映射另写 stderr 须含的 Libra 区分短语，该短语与稳定码同属一次调用的错误契约。同文件既有的 `assert_format_patch_refusal` 与 `refusal_gates!`（`tests/command/format_patch_test.rs:2435-2499`）可复用。
  5. **AC 计数口径（G-03）：** 每条 AC 只含一个独立谓词——一个输入场景下一项可独立失败的可观测性质。下列形式各计 1 条：同一测试对一次调用断言的错误契约（上游计划以 `refusal_gates!` 拆成多个单门测试的拒绝契约属上游验收，本计划不以其作为本卡 AC，只在共享回归组中运行）；对映射行指明的区段与单一黄金值（Git 黄金值或另一次 Libra 输出）做的一次逐字节相等比较，前提是该区段由同一实现单元产生（不同占位符、不同配置值等可分别实现的单元须分条）；一条行级的存在/不存在判定；对归一化线程结构与 Git 黄金值的一次比较（对应 t4014 每个 threading 用例的单次 `check_threading` 断言）；不经黄金比较而直接断言的线程关系按关系分条（`In-Reply-To:` 与 `References:` 是不同关系），每条为该关系在全部邮件上的全称判定。同一 AC 行内不得以「与 / 或 / 、」并列多个可分别失败的场景或分别断言的子性质。ER-06/ER-06a 同卡文档门不计入分子；`Verification` 分子只计具名验收门（模板 G-03：只登记本卡特有判据），共享回归组不计入。

- **文档与网站验收命令（每张卡的 ER-06/ER-06a 同卡文档门；本计划无家族卡，每卡自行完成全部步骤）：** 分两段执行，全程处于 `DEP-FPG-04` 锁持有期内：**发布前段**（第 1–5 步）在本卡实现与 A/B 组验收期间完成，是本卡 `locally-accepted` 的前置（ER-05 代码 review 后文档字节若变化须重走），生成并审定待交付网站字节；**发布后段**（第 6–8 步）在本卡 C 组 ⑨ `gh release create` 成功后执行（网站不得先于对应版本公开）。每张卡开始时先执行 `unset PRE POST FINAL PROBE NEG_PROBE ORD_A ORD_B OLD LIVE SITE_BASE SITE_SHA MERGE_SHA`，再设 `CARD=<本卡 ID>`、`MARKERS=(<本卡 Docs and compatibility impact 所列验收标记>)`、`LIBRA=/Volumes/Data/gitmono/libra`、`SITE=/Volumes/Data/gitmono/libra-backend`、`PAGE=apps/tanstack-app/content/docs/commands/format-patch.en.md`、`EN=docs/commands/format-patch.md`；`MERGE_SHA` 在发布后段取本卡在 `main` 上的合入提交。每条命令以子 shell 显式指定仓库（`(cd "$LIBRA" && …)` 或 `(cd "$SITE" && …)`）。远端 `cf` 的 SHA 一律经 `remote_sha() { local out; out=$(cd "$SITE" && libra ls-remote origin refs/heads/cf) || return 1; printf '%s\n' "$out" | awk -F'\t' 'NR==1{s=$1; r=$2} END{if (NR!=1 || r!="refs/heads/cf" || s !~ /^[0-9a-f]+$/) exit 1; print s}'; }` 取得：先单独检查 `libra ls-remote` 退出码，再要求恰为一行、ref 名匹配且第一列为十六进制 SHA，否则非零退出（STOP）。全部比对在进程内完成：旧版本内容与线上页面只存于 shell 变量（`OLD`、`LIVE`），摘要以 `(set -o pipefail; … | shasum -a 256)` 取得（`pipefail` 使上游命令失败时整条命令非零退出），不创建任何临时文件、快照或证据文件（v2.12，`plan-template.md:74`）。任一步非零退出或判据不符即 STOP：发布前段失败不得进入 C 组 ⑦；发布后段失败时本卡停在 `remote-pending`，不得标 `complete`；均按 ER-10 归属处理。
  1. **仓库文档逐文件验收：** 对 `$EN`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md` 记录开工基线摘要与本卡本地最终摘要（`shasum -a 256`）。每个文件须同时满足：(a) 最终摘要不同于基线，或在本卡记录「已核对无需改动」及逐段依据；(b) 有标记的卡：对 3 个命令文档逐个标记以 `(cd "$LIBRA" && rg -c -F -- "$m" <file>)` 计数（退出码 `1` 计为 0，`>1` 即命令失败 STOP），开工时以同一命令记录基线计数，最终计数须大于基线计数；对 `COMPATIBILITY.md` 以 `(cd "$LIBRA" && sed -n 224p COMPATIBILITY.md | rg -F -q -- "$m")` 判定第 224 行含该标记（退出码 `0` 才通过，`1` 即缺失、`>1` 即命令失败，均 STOP）。`COMPATIBILITY.md` 行数不变且除第 224 行外逐字节不变：`awk 'NR!=224' COMPATIBILITY.md | shasum -a 256` 与 `wc -l < COMPATIBILITY.md` 均与基线相同。
  2. **网站检出前置与基线：** `test -d "$SITE/.libra" && test ! -e "$SITE/.git"`；`(cd "$SITE" && libra status --short --branch)` 首行分支为 `cf`，`(cd "$SITE" && libra --offline symbolic-ref --short HEAD)` 输出恰为 `cf`；记录既有脏文件清单（保留，不清理、不暂存）；`$PAGE` 已有未提交改动即 STOP；`SITE_BASE=$(cd "$SITE" && libra rev-parse HEAD)` 须等于 `remote_sha` 的输出（本地 `cf` 无未推送提交，否则 STOP）。
  3. **同步、只读漂移检查与区分探针：** 记录 `PRE`（`$SITE/$PAGE` 的摘要），运行 `(cd "$SITE" && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root "$LIBRA")`（exit 0）与只读检查 `(cd "$SITE" && python3 scripts/sync-libra-command-docs.py --only format-patch --libra-root "$LIBRA")`（exit 0 = 一致；`1` = 漂移、`2` = 安全拒绝，均 STOP），再记录 `POST`。网站页只由 `$EN` 生成，分支只按 `$EN` 判定：
     - **页面有变化（`PRE` ≠ `POST`）：** 本卡第 1 步中 `$EN` 须有变化（页面变化只能来自同步源），否则 STOP 并按 ER-03 归属漂移来源。至少选定一个区分探针（均取自单个源行、不含 Markdown 标记的纯文本短语）：`PROBE` 取本卡在 `$EN` 新增或改写后的内容，先以 `OLD=$(cd "$SITE" && libra show "HEAD:$PAGE")` 把旧页面读入 shell 变量（该赋值的退出码即 `libra show` 的退出码，须为 0，且 `test -n "$OLD"`），下文「对 `$OLD`」的判定一律以 `printf '%s\n' "$OLD" | rg …` 形式执行；再要求 `rg -F -q -- "$PROBE" "$SITE/$PAGE"` 退出码 0 且 `printf '%s\n' "$OLD" | rg -F -q -- "$PROBE"` 退出码 1；本卡只删除或改写既有内容而无合格新增短语时，改取 `NEG_PROBE`（旧页面含、新页面不含），要求对 `$SITE/$PAGE` 退出码 1、对 `$OLD` 退出码 0；本卡变化只调整既有内容顺序、既无合格新增短语也无删除短语时，改选顺序探针 `ORD_A`、`ORD_B`（各取自单个源行、在新旧页中均恰出现一次的纯文本短语）：新页中 `ORD_A` 所在行号小于 `ORD_B`、`$OLD` 中相反（行号由 `rg -n -F` 取得，命中次数不为 1 即另选；三类探针都选不出时 STOP 并按 ER-03 说明页面变化来源）；凡要求退出码 1 的判定按模板「Verification 判定口径」区分 `1`（零命中）与 `>1`（命令失败即 STOP）；比对后 `unset OLD`。执行第 4–5 步。
     - **页面无变化（`PRE` = `POST`）：** 分支只由生成页摘要决定：`$EN` 未变，或其变化只落在同步脚本会去除或规范化的部分（一级标题、frontmatter 等，见 `/Volumes/Data/gitmono/libra-backend/scripts/sync-libra-command-docs.py:228-230,270,303-312`）时均属本分支，须在本卡记录 `$EN` 变化与「生成页不受影响」的依据；`$EN` 仍按第 1 步与第 6 步独立核对；其它 3 个仓库文件的变化不影响本分支。令 `FINAL=$POST`，`PROBE` 取当前页面中一段取自单个源行的纯文本短语；网站字节未变，第 4、5、7 步不适用（无新提交可构建、可审、可推送）；在进入第 6、8 步前令 `SITE_SHA=$SITE_BASE`。
  4. **前端验证：** `(cd "$SITE" && pnpm typecheck)`（脚本内含 `gen:docs`）与 `(cd "$SITE" && pnpm build)` 均 exit 0。
  5. **Claude Code 审查：** 记录 `FINAL=$(shasum -a 256 "$SITE/$PAGE" | cut -d' ' -f1)`，对该最终字节取得独立 Claude Code `VERDICT: PASS`；审查后 `$PAGE` 任一字节变化即作废，重走第 3–5 步。
  6. **合入树复核（发布后段起点）：** 对 4 个仓库文件逐一运行 `(set -o pipefail; cd "$LIBRA" && libra show "$MERGE_SHA:<file>" | shasum -a 256)`（退出码须为 0，失败即 STOP），其摘要须等于第 1 步记录的本地最终摘要；`(cd "$LIBRA" && shasum -a 256 "$EN")` 须等于 `MERGE_SHA` 中 `$EN` 的摘要；`shasum -a 256 "$SITE/$PAGE"` 须仍等于 `FINAL`；`remote_sha` 须仍等于 `SITE_BASE`。合入树的文档字节与第 1 步不同时（版本已发布）本卡保持 `remote-pending`，不得对旧 `MERGE_SHA` 部署新字节：按 ER-10 新建 `FIX-*` 前滚修复文档并发新 `patch + 1` 版本，以新合入提交为 `MERGE_SHA` 重走发布前段与发布后段；远端 `cf` 已前进则 STOP，按 ER-09 归属后重走第 2–5 步。
  7. **精确暂存、签名提交与推送（仅页面有变化分支）：** `(cd "$SITE" && libra add "$PAGE")`，随后 `(cd "$SITE" && libra status --short)` 中已暂存条目恰为 `$PAGE`；`(cd "$SITE" && libra commit -s -m "docs(commands): sync format-patch for $CARD")`；`SITE_SHA=$(cd "$SITE" && libra rev-parse HEAD)`，按 ER-07 校验同一提交含 `gpgsig` 与 `Signed-off-by:`；`(set -o pipefail; cd "$SITE" && libra show "$SITE_SHA:$PAGE" | shasum -a 256)` 退出码须为 0 且摘要等于 `FINAL`，`(cd "$SITE" && libra rev-parse "$SITE_SHA^")` 须等于 `SITE_BASE`；推送前 `remote_sha` 须仍等于 `SITE_BASE`，随后 `(cd "$SITE" && libra push origin cf)`（禁止 force），推送后 `remote_sha` 须等于 `SITE_SHA`；不等即按 ER-09 处理，不得盲目重试。页面无变化分支令 `SITE_SHA=$SITE_BASE`。
  8. **部署核验（绑定提交 SHA 与页面内容）：** ① 以 Cloudflare 仪表盘 Workers Builds 的构建记录（或等价的 Cloudflare API 输出）取证：`cf` 分支对 `$SITE_SHA`（或其后继提交，且该后继中 `$PAGE` 的摘要等于 `FINAL`）的生产构建与部署状态为成功；在本卡记录构建记录 ID、提交 SHA、状态与观察 UTC 时间（sanitized 摘要，不另存文件）；页面无变化分支同样须对**当前**生产部署取证，不得只引用历史部署记录：当前生产部署对应的提交须为 `$SITE_SHA` 或其后继，且该提交中 `$PAGE` 的摘要等于 `FINAL`（`(set -o pipefail; cd "$SITE" && libra show "<该提交>:$PAGE" | shasum -a 256)` 退出码须为 0 且摘要等于 `FINAL`；该提交在本地不可读即 STOP 并按 ER-09 归属）；取不到即 STOP。② `LIVE=$(set -o pipefail; curl -fsS https://libra.tools/en/docs/commands/format-patch | python3 -c 'import html,sys; sys.stdout.write(html.unescape(sys.stdin.read()))')` 退出码须为 0 且 `test -n "$LIVE"`（线上页面只存于 shell 变量，先做 HTML 实体解码，使含 `<`/`>` 的标记可匹配）；`PROBE`（若选定）与有标记卡的每个标记以 `printf '%s\n' "$LIVE" | rg -F -q -- "<短语>"` 判定退出码 0，`NEG_PROBE`（若选定）退出码 1（`>1` 即命令失败，STOP）；`ORD_A`/`ORD_B`（若选定）以 `printf '%s' "$LIVE" | python3 -c 'import sys; s=sys.stdin.read(); a,b=sys.argv[1],sys.argv[2]; sys.exit(0 if s.count(a)==1 and s.count(b)==1 and s.find(a)<s.find(b) else 1)' "$ORD_A" "$ORD_B"` 判定（退出码非 0 即 STOP）；完成后 `unset LIVE`。暂时性错误按 ER-10 有界重试（≤ 5 次、总计 ≤ 30 分钟），超预算置 `blocked` 并记录。

## 实施顺序

- `DEP-FPG-01 -> FPG-01`
- `DEP-FPG-05 -> FPG-04`、`DEP-FPG-05 -> FPG-57`
- Phase 0: `FPG-01 -> FPG-32 -> FPG-02 -> FPG-53 -> FPG-03 -> FPG-04 -> FPG-56 -> FPG-44 -> FPG-37 -> FPG-57 -> FPG-05 -> FPG-36 -> FPG-41 -> FPG-54 -> FPG-06 -> FPG-55 -> FPG-07 -> FPG-08 -> FPG-29 -> FPG-30 -> FPG-51 -> FPG-42 -> FPG-09 -> FPG-10 -> FPG-31 -> FPG-46`
- Phase 1: `FPG-46 -> FPG-11 -> FPG-48 -> FPG-12 -> FPG-58 -> FPG-13 -> FPG-14 -> FPG-49 -> FPG-15 -> FPG-16 -> FPG-59 -> FPG-64 -> FPG-17 -> FPG-43 -> FPG-24 -> FPG-60 -> FPG-25 -> FPG-33`（其中 `DEP-FPG-03 -> FPG-11`）
- Phase 2: `FPG-33 -> FPG-18 -> FPG-38 -> FPG-19 -> FPG-34 -> FPG-20 -> FPG-45 -> FPG-26 -> FPG-52 -> FPG-21 -> FPG-40 -> FPG-22 -> FPG-61 -> FPG-27 -> FPG-39 -> FPG-62 -> FPG-63 -> FPG-28 -> FPG-35 -> FPG-23 -> FPG-47 -> FPG-50`（其中 `DEP-FPG-02 -> FPG-22`、`DEP-FPG-02 -> FPG-27`、`DEP-FPG-03 -> FPG-22`、`DEP-FPG-03 -> FPG-27`）

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-FPG-01 | incoming | 跨计划写锁 | `issues/529#FP-13`（及 `CLOSE-529` 收口） | `#529` 执行者 | `issues/529.md` 全部实现卡达到 `done`/`complete`，且 `issues/529#CLOSE-529` 全量收口完成（以 `529.md` 中 `CLOSE-529` 的完成记录为证据，开工时记入本计划），并释放 `src/command/format_patch.rs` 与 `src/cli.rs` 写锁；只完成实现卡而 `CLOSE-529` 未完成不算满足 | `docs/development/plan/issues/529.md`（2026-11-17 核对） | `CLOSE-529` 完成并释放写锁前 `FPG-01` 保持 `pending`，不得抢跑写 `format_patch.rs` |
| DEP-FPG-02 | incoming | 跨计划写锁 | `plan-20260927#CM-08` / `CM-09` | `plan-20260927` 执行者 | 开工 `FPG-22` 与 `FPG-27` 前核实 `src/command/diff.rs` 与 `src/command/diff/` 无在途写者（2026-11-15 `plan-status.md` 记为 `pending`/空） | `docs/development/plan/plan-status.md:1544-1545`、`issues/529.md:106` | 若有在途修改则等待其释放后再动 `src/command/diff.rs` 与 `src/command/diff/` |
| DEP-FPG-03 | incoming | 跨计划写锁 | `issues/1040.md` 及同批 `log-show` 计划 | 协调者 / 各计划执行者 | 开工 `FPG-11`/`FPG-22`/`FPG-27` 前核实 `src/command/log.rs` 无在途写者并串行协调 | `docs/development/plan/issues/1040.md` | 存在并发写者时等待其释放 `src/command/log.rs` 后 rebase 再开工 |
| DEP-FPG-04 | incoming | 跨计划共享交付与发布窗口锁 | `plan-status.md` 登记的其它在途计划（含 `log-show`、`global-init-infra`、`plan-20260927` 等） | 仓库维护者 `genedna` 或其显式指派的单一发布 Agent（须在 `origin/main` `plan-status.md` 登记授权） | **共同串行仲裁前置与执行协议：0) 开工前置条件：** 本计划 `FPG-01` 启动前，须核实上述共同串行仲裁规则与单一授权发布者已在 `origin/main` `plan-status.md` 合入生效，且全部在途竞争计划（`log-show`、`global-init-infra`、`plan-20260927` 等）均已在其计划及 `plan-status.md` 中采纳同一协议（或已收口关闭）；未满足前本计划保持 `pending` 不得开工。**执行协议：** ① 登记前 `libra fetch` 基于最新 `origin/main` 核实 `plan-status.md` 无其它 `in-progress`/`blocked` 持有者；② 提交 PR/合入 `origin/main` 登记本卡 `in-progress` 与唯一锁归属（卡号 + 授权发布者），若合入发生竞争冲突则放弃并基于最新 `origin/main` 重新检查；③ 合入后再次 `libra fetch` 回读 `origin/main`（及 `gh release list --limit 5`）确认本卡为唯一可见持有者后方可写入共享面或发布；④ **持续持有至 `gh release create`、D 组 `release.yml`/CDN 核验与「文档与网站验收命令」第 8 步网站部署核验全部完成**，更新 `origin/main` `plan-status.md` 为 `done`/`complete`（含版本与 SHA）并确认合入后方释放 | `docs/development/plan/plan-status.md`（`origin/main`） | fail-closed：前置条件未满足、未获单一发布者授权、或回读发现并发写者/竞争登记/归属不清，立即中止写入与发布，等待 `origin/main` 满足/释放并 rebase 后重试 |
| DEP-FPG-05 | incoming | 跨计划写锁 | `plan-20261117-global-init-infra.md` 及同批写 `src/internal/config.rs` 的计划 | 协调者 / 各计划执行者 | 开工 `FPG-04` 与 `FPG-57` 前核实 `src/internal/config.rs` 无在途写者（`plan-status.md` 与各计划写集），并串行协调本计划新增的级联多值 helper 及其多键变体 | `docs/development/plan/plan-20261117-global-init-infra.md`（2026-11-18 核对其写集含 `src/internal/config.rs`） | 存在并发写者时等待其释放并 rebase 后再开工；不得在他人在途期间修改该文件 |

### 发布分组与并发窗口

- **并发声明:** 全串行（所有卡共享 `src/command/format_patch.rs`、`COMPATIBILITY.md`、版本面、`plan-status.md` 与 `/Volumes/Data/gitmono/libra-backend` 写集，依据 G-10 禁止计划内与跨计划并发）。
- **发布者:** 单一指定发布者（协调者指派的执行 Agent）。
- **发布窗口顺序:** 待 `DEP-FPG-01` 与 `DEP-FPG-04` 满足后，按实施顺序依次执行 C 组 bump 与 `gh release create`，每卡开工与发布前均须重新核验 `DEP-FPG-04`（及适用时的 `DEP-FPG-02`/`03`/`05`），前一卡未完成推送与 D 组触发前不得开启下一卡 bump。

### Phase 0: 序号、身份、上游去重与邮件头基础（FPG-01..FPG-10, FPG-29..FPG-32, FPG-36, FPG-37, FPG-41, FPG-42, FPG-44, FPG-46, FPG-51, FPG-53..FPG-57）

**目标:** 完成多补丁默认编号（`#880` 由 529 交付）与 `format.numbered`、环境变量身份核验、`--ignore-if-in-upstream`、`format.headers`/`--add-header`、`format.to`/`format.cc` 与顺序、`--no-notes`/`format.notes`、`format.signature`、多行 Subject 拼接、长行折叠、默认 RFC2047 编码、纯 ASCII 省略 MIME（及 `#881` t4036 闭环）、`-k` 核验与 `--from`/`format.from`。

**进入条件:** `DEP-FPG-01` 满足（`529.md` 释放写锁），当前工作区干净。

**退出条件:** `FPG-01`..`FPG-10`、`FPG-29`..`FPG-32`、`FPG-36`、`FPG-37`、`FPG-41`、`FPG-42`、`FPG-44`、`FPG-46`、`FPG-51`、`FPG-53`..`FPG-57` 全部 `Lifecycle=done`、`Acceptance=complete`。

### Phase 1: 路径过滤、封面信与修订范围（FPG-11..FPG-17, FPG-24, FPG-25, FPG-33, FPG-43, FPG-48, FPG-49, FPG-58..FPG-60, FPG-64）

**目标:** 完成 `--relative`/`diff.relative`、封面信自动触发/正文模板/diffstat/`--commit-list-format` 扩展、多修订范围与 `^<rev>`、固定 mbox 时间戳与补丁 `Date:` 作者时间、`--base=auto`、`branch.<name>.description`、`--cover-from-description` 与 `format.coverFromDescription`。

**进入条件:** Phase 0（`FPG-46`）完成。

**退出条件:** `FPG-11`..`FPG-17`、`FPG-24`、`FPG-25`、`FPG-33`、`FPG-43`、`FPG-48`、`FPG-49`、`FPG-58`..`FPG-60`、`FPG-64` 全部 `Lifecycle=done`、`Acceptance=complete`。

### Phase 2: 附件、Interdiff、文件名、Diff 选项与线程化（FPG-18..FPG-23, FPG-26..FPG-28, FPG-34, FPG-35, FPG-38..FPG-40, FPG-45, FPG-47, FPG-50, FPG-52, FPG-61..FPG-63）

**目标:** 完成 `--attach=<name>`、多补丁与单补丁 `--interdiff`、reroll 前缀与 stdout 列表、非整数 `-v`、文件名大小写与长度上限、Git diff 选项透传、`--thread` 默认/配置与 Message-ID 去重，完成全量收口门。

**进入条件:** Phase 1（`FPG-33`）完成。

**退出条件:** 全部 64 张卡 `Lifecycle=done`、`Acceptance=complete`，计划完成判据全量门全绿。

## 任务卡

### 任务卡粒度规则（强制）

继承 [plan-template.md](plan-template.md) 的 `G-01`..`G-11`。

### 字段全局默认与例外

- **Release boundary 默认:** `independent`（每卡独立执行完整 C 组含 `patch + 1`、`compat_version_surface_sync` 同步与 `gh release create`，并适用 ER-04 D 组：`base.yml`、`codeql.yml`、`release.yml`）
- **Task type 默认:** `implementation`
- **Rollback mode 默认:** `immutable-release`（推送/发布前：本地 `revert` 单提交；推送/发布后：已发布版本与 tag 不可撤回，按 ER-04/ER-10 新建 `FIX-*` 前滚修复并发新 `patch + 1` 版本；兼容窗口与降级指引：同 `0.30.x` 次版本内无 schema 变更，用户可回退安装上一版本 `v<prev>`；`FPG-01` 默认编号、`FPG-23` 默认关线程可通过 `-N` / `--thread` 在兼容窗口内显式回退，`FPG-30` 纯 ASCII 省略 MIME 恢复路径为前滚 `FIX-*` 或回退安装 `v<prev>`）
- **Migration and rollback 默认:** `N/A：无 schema 迁移`
- **Security and privacy 默认:** 继承 GC-07、GC-11（校验邮件头控制字符与路径边界，无裸 `unwrap()`）
- **Performance budget 默认:** 继承 GC-10（不引入无界全库遍历）
- **Release write set 默认:** `Inherited`（`Cargo.toml`、`install.sh`、`install.ps1` + 工具链刷新的 `Cargo.lock` + release artifact）
- **Full-suite trigger 默认:** `T-1: 版本面集合同步修改 install.sh / install.ps1（ER-08 / ER-13 T-1）`
- **Deliverables 默认:** `N/A`

**默认覆盖:** 无。

**规则 waiver（`EX-*`）:** 无。

### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| FPG-01 | implementation | 多补丁默认编号（`auto` 缺省语义） | immutable-release | yes | yes | 5/8 | 1/8 | 2/1 | M | DEP-FPG-01 | 序列化于 DEP-FPG-01 | independent | N/A | N/A |
| FPG-32 | implementation | `format.numbered` 配置与 CLI 覆盖 | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-01 | 序列化于 FPG-01 | independent | FPG-01 | N/A |
| FPG-02 | implementation | `GIT_COMMITTER_*` 身份核验 | immutable-release | yes | yes | 6/8 | 1/8 | 2/1 | M | FPG-32 | 序列化于 FPG-32 | independent | N/A | N/A |
| FPG-53 | implementation | `529 FP-11` 身份失败路径核实 | immutable-release | yes | yes | 3/8 | 1/8 | 2/1 | M | FPG-02 | 序列化于 FPG-02 | independent | FPG-02 | N/A |
| FPG-03 | implementation | `--ignore-if-in-upstream` 上游 patch-id 只取 `b..a` | immutable-release | yes | yes | 4/8 | 2/8 | 2/1 | M | FPG-53 | 序列化于 FPG-53 | independent | N/A | N/A |
| FPG-04 | implementation | `format.headers` 与级联多值读取 | immutable-release | yes | yes | 8/8 | 3/8 | 3/2 | M | FPG-03,DEP-FPG-05 | 序列化于 FPG-03 | independent | N/A | N/A |
| FPG-56 | implementation | `format.numbered` 多值逐条处理 | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-04 | 序列化于 FPG-04 | independent | FPG-32 | N/A |
| FPG-44 | implementation | `--add-header`/`--no-add-header` | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-56 | 序列化于 FPG-56 | independent | FPG-04 | N/A |
| FPG-37 | implementation | `format.to`/`format.cc` 与 `--to`/`--no-to` 顺序 | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-44 | 序列化于 FPG-44 | independent | FPG-04 | N/A |
| FPG-57 | implementation | `To:`/`Cc:` 统一累积列表与附加头顺序 | immutable-release | yes | yes | 8/8 | 1/8 | 3/2 | M | FPG-37,DEP-FPG-05 | 序列化于 FPG-37 | independent | FPG-37 | N/A |
| FPG-05 | implementation | `--no-notes` 与命令行 notes 顺序 | immutable-release | yes | yes | 5/8 | 1/8 | 2/1 | M | FPG-57 | 序列化于 FPG-57 | independent | N/A | N/A |
| FPG-36 | implementation | `format.notes` 配置 | immutable-release | yes | yes | 5/8 | 1/8 | 2/1 | M | FPG-05 | 序列化于 FPG-05 | independent | FPG-05 | N/A |
| FPG-41 | implementation | `format.notes` 与命令行 notes 交互 | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-36 | 序列化于 FPG-36 | independent | FPG-36 | N/A |
| FPG-54 | implementation | 签名块与 `--stdout` 邮件分隔布局 | immutable-release | yes | yes | 6/8 | 1/8 | 2/1 | M | FPG-41 | 序列化于 FPG-41 | independent | FPG-06 | N/A |
| FPG-06 | implementation | signature 前缀与 `format.signature` | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-54 | 序列化于 FPG-54 | independent | N/A | N/A |
| FPG-55 | implementation | `format.signatureFile` 与空 `--signature` | immutable-release | yes | yes | 6/8 | 1/8 | 2/1 | M | FPG-06 | 序列化于 FPG-06 | independent | FPG-06 | N/A |
| FPG-07 | implementation | 多行 Subject 首段拼接 | immutable-release | yes | yes | 8/8 | 2/8 | 2/1 | M | FPG-55 | 序列化于 FPG-55 | independent | N/A | N/A |
| FPG-08 | implementation | 默认 RFC 2047 邮件头编码 | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-07 | 序列化于 FPG-07 | independent | N/A | N/A |
| FPG-29 | implementation | 长 `Subject:` 邮件头折叠 | immutable-release | yes | yes | 7/8 | 2/8 | 2/1 | M | FPG-08 | 序列化于 FPG-08 | independent | FPG-07 | N/A |
| FPG-30 | implementation | 补丁邮件按需输出 MIME 头 | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-29 | 序列化于 FPG-29 | independent | FPG-08 | N/A |
| FPG-51 | implementation | `#881` t4036 签名者 MIME 闭环 | immutable-release | yes | yes | 4/8 | 1/8 | 2/1 | M | FPG-30 | 序列化于 FPG-30 | independent | FPG-30 | N/A |
| FPG-42 | implementation | 封面信按需输出 MIME 头 | immutable-release | yes | yes | 2/8 | 1/8 | 2/1 | M | FPG-51 | 序列化于 FPG-51 | independent | FPG-30 | N/A |
| FPG-09 | implementation | `#888` `-k` 互斥核验与关闭 | immutable-release | yes | yes | 4/8 | 2/8 | 2/1 | S | FPG-42 | 序列化于 FPG-42 | independent | N/A | N/A |
| FPG-10 | implementation | `--from=<ident>` 合法性校验 | immutable-release | yes | yes | 6/8 | 1/8 | 2/1 | M | FPG-09 | 序列化于 FPG-09 | independent | N/A | N/A |
| FPG-31 | implementation | `format.from` 配置取值 | immutable-release | yes | yes | 4/8 | 1/8 | 2/1 | M | FPG-10 | 序列化于 FPG-10 | independent | FPG-10 | N/A |
| FPG-46 | implementation | `--from`/`--no-from` 覆盖 `format.from` | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-31 | 序列化于 FPG-31 | independent | FPG-31 | N/A |
| FPG-11 | implementation | `--relative` 与 `diff.relative` | immutable-release | yes | yes | 6/8 | 1/8 | 3/2 | M | FPG-46,DEP-FPG-03 | 序列化于 FPG-46 | independent | N/A | N/A |
| FPG-48 | implementation | `--relative` 仓库外与不存在前缀 | immutable-release | yes | yes | 4/8 | 1/8 | 2/1 | M | FPG-11 | 序列化于 FPG-11 | independent | FPG-11 | N/A |
| FPG-12 | implementation | `format.coverLetter` 与 `#909` 核验 | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-48 | 序列化于 FPG-48 | independent | N/A | N/A |
| FPG-58 | implementation | 封面信正文模板 | immutable-release | yes | yes | 2/8 | 1/8 | 2/1 | M | FPG-12 | 序列化于 FPG-12 | independent | FPG-13 | N/A |
| FPG-13 | implementation | 封面信系列 diffstat | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-58 | 序列化于 FPG-58 | independent | N/A | N/A |
| FPG-14 | implementation | `--commit-list-format` 原子与 `log:` 前缀 | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-13 | 序列化于 FPG-13 | independent | N/A | N/A |
| FPG-49 | implementation | `format.commitListFormat` 与 `full`/无封面信边界 | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-14 | 序列化于 FPG-14 | independent | FPG-14 | N/A |
| FPG-15 | implementation | 多修订参数与 `^<rev>` 排除语法 | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-49 | 序列化于 FPG-49 | independent | N/A | N/A |
| FPG-16 | implementation | mbox `From ` 固定 2001-09-17 时间戳与封面信信封 oid | immutable-release | yes | yes | 5/8 | 2/8 | 2/1 | M | FPG-15 | 序列化于 FPG-15 | independent | N/A | N/A |
| FPG-59 | implementation | 补丁 `Date:` 作者时间与时区 | immutable-release | yes | yes | 3/8 | 2/8 | 2/1 | M | FPG-16 | 序列化于 FPG-16 | independent | FPG-16 | N/A |
| FPG-64 | implementation | base 区块只在第一封邮件 | immutable-release | yes | yes | 2/8 | 1/8 | 2/1 | M | FPG-59 | 序列化于 FPG-59 | independent | FPG-17 | N/A |
| FPG-17 | implementation | `--base=auto` 自动基线 | immutable-release | yes | yes | 7/8 | 2/8 | 2/1 | M | FPG-64 | 序列化于 FPG-64 | independent | N/A | N/A |
| FPG-43 | implementation | `format.useAutoBase` 配置 | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-17 | 序列化于 FPG-17 | independent | FPG-17 | N/A |
| FPG-24 | implementation | 封面信 `branch.<name>.description` | immutable-release | yes | yes | 3/8 | 1/8 | 2/1 | M | FPG-43 | 序列化于 FPG-43 | independent | FPG-12 | N/A |
| FPG-60 | implementation | 封面信默认 `From:` 身份 | immutable-release | yes | yes | 2/8 | 1/8 | 2/1 | M | FPG-24 | 序列化于 FPG-24 | independent | FPG-24 | N/A |
| FPG-25 | implementation | `--cover-from-description` CLI 模式输出与取值校验 | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-60 | 序列化于 FPG-60 | independent | FPG-13 | N/A |
| FPG-33 | implementation | `format.coverFromDescription` 配置与 CLI 覆盖 | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-25 | 序列化于 FPG-25 | independent | FPG-25 | N/A |
| FPG-18 | implementation | `--attach=<boundary>`/`--inline=<boundary>` | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-33 | 序列化于 FPG-33 | independent | N/A | N/A |
| FPG-38 | implementation | `format.attach` 配置与 `--no-attach` | immutable-release | yes | yes | 5/8 | 1/8 | 2/1 | M | FPG-18 | 序列化于 FPG-18 | independent | FPG-18 | N/A |
| FPG-19 | implementation | 多补丁 `--interdiff` 封面信输出 | immutable-release | yes | yes | 6/8 | 2/8 | 2/1 | M | FPG-38 | 序列化于 FPG-38 | independent | N/A | N/A |
| FPG-34 | implementation | 单补丁 `--interdiff` 输出位置 | immutable-release | yes | yes | 5/8 | 1/8 | 2/1 | M | FPG-19 | 序列化于 FPG-19 | independent | FPG-19 | N/A |
| FPG-20 | implementation | reroll `vN-` 文件名前缀与 `Subject:` 版本 | immutable-release | yes | yes | 6/8 | 1/8 | 2/1 | M | FPG-34 | 序列化于 FPG-34 | independent | N/A | N/A |
| FPG-45 | implementation | 生成路径列表输出到 stdout | immutable-release | yes | yes | 6/8 | 1/8 | 2/1 | M | FPG-20 | 序列化于 FPG-20 | independent | FPG-20 | N/A |
| FPG-26 | implementation | 非整数/非常规 `--reroll-count` 文件名前缀 | immutable-release | yes | yes | 3/8 | 1/8 | 2/1 | M | FPG-45 | 序列化于 FPG-45 | independent | FPG-20 | N/A |
| FPG-52 | implementation | 非整数 reroll 的 `Subject:` 与 interdiff 标题 | immutable-release | yes | yes | 6/8 | 1/8 | 2/1 | M | FPG-26 | 序列化于 FPG-26 | independent | FPG-26 | N/A |
| FPG-21 | implementation | 文件名按 Git 清洗并保留大小写 | immutable-release | yes | yes | 5/8 | 1/8 | 2/1 | M | FPG-52 | 序列化于 FPG-52 | independent | N/A | N/A |
| FPG-40 | implementation | 文件名长度上限与 `format.filenameMaxLength` | immutable-release | yes | yes | 8/8 | 1/8 | 2/1 | M | FPG-21 | 序列化于 FPG-21 | independent | FPG-21 | N/A |
| FPG-22 | implementation | `-U`/`--unified` | immutable-release | yes | yes | 7/8 | 2/8 | 3/4 | M | FPG-40,DEP-FPG-02,DEP-FPG-03 | 序列化于 FPG-40 | independent | N/A | N/A |
| FPG-61 | implementation | `--full-diff` 接受 | immutable-release | yes | yes | 1/8 | 1/8 | 2/1 | M | FPG-22 | 序列化于 FPG-22 | independent | FPG-22 | N/A |
| FPG-27 | implementation | `--diff-filter` | immutable-release | yes | yes | 4/8 | 2/8 | 3/5 | M | FPG-61,DEP-FPG-02,DEP-FPG-03 | 序列化于 FPG-61 | independent | FPG-22 | N/A |
| FPG-39 | implementation | 重命名选项核验与封面信继承 diff 选项 | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-27 | 序列化于 FPG-27 | independent | FPG-27 | N/A |
| FPG-62 | implementation | 封面信写入失败不留部分输出 | immutable-release | yes | yes | 2/8 | 1/8 | 2/1 | M | FPG-39 | 序列化于 FPG-39 | independent | FPG-39 | N/A |
| FPG-63 | implementation | 线程头位于信封行之后 | immutable-release | yes | yes | 1/8 | 1/8 | 2/1 | M | FPG-62 | 序列化于 FPG-62 | independent | FPG-28 | N/A |
| FPG-28 | implementation | 非子串唯一 `Message-ID` 生成 | immutable-release | yes | yes | 4/8 | 1/8 | 2/1 | M | FPG-63 | 序列化于 FPG-63 | independent | FPG-23 | N/A |
| FPG-35 | implementation | `--thread=shallow`/`deep` 与 deep 链式线程 | immutable-release | yes | yes | 7/8 | 1/8 | 2/1 | M | FPG-28 | 序列化于 FPG-28 | independent | FPG-23 | N/A |
| FPG-23 | implementation | `--thread` 默认关闭 | immutable-release | yes | yes | 2/8 | 1/8 | 2/1 | M | FPG-35 | 序列化于 FPG-35 | independent | N/A | N/A |
| FPG-47 | implementation | `format.thread` 取值 | immutable-release | yes | yes | 6/8 | 1/8 | 2/1 | M | FPG-23 | 序列化于 FPG-23 | independent | FPG-23 | N/A |
| FPG-50 | implementation | 命令行线程选项按顺序覆盖配置 | immutable-release | yes | yes | 6/8 | 1/8 | 2/1 | M | FPG-47 | 序列化于 FPG-47 | independent | FPG-47 | N/A |

---

### Task FPG-01: 实现多补丁默认编号（#896 默认编号部分；自本卡拆出 FPG-32）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 实现 `#896` 的默认编号：未传 `-n`/`-N` 且无 `format.numbered` 时，多补丁系列（`total > 1`）默认输出 `[PATCH n/m]`，单补丁输出 `[PATCH]`（Git `format.numbered` 缺省 `auto` 语义）。`#880` 的计数宽度与前缀组合由 `issues/529#FP-02`/`FP-04` 交付并在 529 计划内验收、按其 `DEP-FP-07` 关闭，本卡经 `DEP-FPG-01` 等待其完成且不重复验收；`format.numbered` 配置读取与 CLI 覆盖由自本卡拆出的 `FPG-32` 承接。

**Out of scope:** `#880` 计数宽度与前缀组合（由 `issues/529#FP-02`/`FP-04` 交付与关闭，见 `DEP-FPG-01`）；`format.numbered` 配置与 `-n`/`-N` 覆盖配置（由 `FPG-32` 承接）；文件名序号宽度（尚未排期，重启条件：新 issue）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `subject_counter_width` 已按 `displayed_total` 宽度计算（`529 FP-02` 交付） | `src/command/format_patch.rs:857,1402,1762-1764` |
| 仅当 `args.numbered \|\| args.cover_letter` 时才编号，未读 `format.numbered` | `src/command/format_patch.rs:855,1861-1902` |
| 默认编号改变后，既有显式 `-n`/`-N` 用例（如 `subject_counter_width_follows_displayed_total`、`numbered_flags_follow_last_occurrence`）须同卡核对断言 | `tests/command/format_patch_test.rs:1875,1984` |
| Git 默认编号判定为 `!keep_subject && auto_number && (total > 1 \|\| cover_letter)`，`total` 为实际补丁数 | `git show 6de20f6092:builtin/log.c` 第 2357、2375–2377 行；`git show 6de20f6092:t/t4021-format-patch-numbered.sh` 第 129–132 行 |

**Acceptance criteria:**

- [ ] AC1：未传 `-n`/`-N` 且无 `format.numbered` 的两补丁系列默认编号。
- [ ] AC2：未传 `-n`/`-N` 且无配置的单补丁默认不编号。
- [ ] AC3：两补丁系列传 `-N` 时不编号。
- [ ] AC4：未传 `-n`/`-N` 且无 `format.numbered` 时，单补丁加 `--cover-letter`（`--cover-letter -1 --stdout`）仍编号（t4021#13；Git `auto_number && (total > 1 || cover_letter)`）。
- [ ] AC5：`--start-number 3 -1 --stdout`（单补丁、无封面信）不编号（Git 以实际补丁数 `total` 判定，不计起始号）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::default_numbering_auto` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH 1/2] <标题1>`、`Subject: [PATCH 2/2] <标题2>` |
| AC2 | `command::format_patch_test::default_numbering_auto` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] <标题>` |
| AC3 | `command::format_patch_test::default_numbering_auto` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] <标题1>`、`Subject: [PATCH] <标题2>` |
| AC4 | `command::format_patch_test::default_numbering_auto` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH 0/1] *** SUBJECT HERE ***`、`Subject: [PATCH 1/1] <标题>` |
| AC5 | `command::format_patch_test::default_numbering_auto` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] <标题>` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（默认编号（多补丁 `[PATCH n/m]`、单补丁 `[PATCH]`）说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::default_numbering_auto)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::default_numbering_auto`；证明 AC1–AC5）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited（T-1：版本面集合同步修改 install.sh / install.ps1）`

**Dependencies:** `DEP-FPG-01`（等待 `issues/529#CLOSE-529` 释放写锁）

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：默认编号（多补丁 `[PATCH n/m]`、单补丁 `[PATCH]`）说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=多补丁默认编号（auto缺省语义）; recovery=immutable-release（发布前revert单提交，发布后FIX前滚）; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=DEP-FPG-01; writeset=序列化于DEP-FPG-01; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-32: `format.numbered` 单值配置读取与 `-n`/`-N` 覆盖（#896 配置部分；自 FPG-01 拆出；自本卡拆出 FPG-56）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-01` 拆出的独立轴：在 `resolve_format_config` 经 `read_format_config`（严格 local → global → system 级联，取首个命中值）读取 `format.numbered`（`true`/`false`/`auto` 及 Git 布尔别名），非法值返回 `FormatPatchError::InvalidConfig`（`LBR-CLI-003`）；CLI `-n`/`-N` 覆盖配置；`false` 同时关闭由封面信触发的编号（Git `auto_number = auto_number && numbered`）。Git 对多层与同层重复取值逐条处理（全局 `true`、本地 `auto` 时单补丁仍编号），该语义需 `FPG-04` 的级联多值 helper，由自本卡拆出的 `FPG-56` 承接；本卡的夹具每个场景只在一层写一个值。

**Out of scope:** 默认 `auto` 编号行为（由 `FPG-01` 承接）；多层与同层重复取值的逐条语义、非法值与 `-n` 同给、`-k` 复位（由 `FPG-56` 承接）；文件名序号宽度（尚未排期，重启条件：新 issue）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_format_config` 未读取 `format.numbered`；`read_format_config` 提供严格级联读取 | `src/command/format_patch.rs:1861-1916` |
| `InvalidConfig` 以 `error:` 前缀映射 `LBR-CLI-003`，CLI 类稳定码退出码为 129 | `src/command/format_patch.rs:388-393,419-420`、`src/utils/error.rs:374-375,514-527` |

**Acceptance criteria:**

- [ ] AC1：`format.numbered=true` 时单补丁编号。
- [ ] AC2：`format.numbered=false` 时两补丁不编号。
- [ ] AC3：`format.numbered=auto` 时单补丁不编号。
- [ ] AC4：`format.numbered=auto` 时两补丁编号。
- [ ] AC5：`format.numbered=bogus` 时拒绝执行。
- [ ] AC6：`format.numbered=false` 下 `-n -2 --stdout` 编号（CLI 覆盖配置）。
- [ ] AC7：`format.numbered=true` 下 `-N -1 --stdout` 不编号（CLI 覆盖配置）。
- [ ] AC8：`format.numbered=false` 下 `--cover-letter -1 --stdout` 的封面信与补丁均不编号（#896 Expected behavior 第 4 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_numbered_config` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH 1/1] <标题>` |
| AC2 | `command::format_patch_test::format_numbered_config` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] <标题1>`、`Subject: [PATCH] <标题2>` |
| AC3 | `command::format_patch_test::format_numbered_config` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] <标题>` |
| AC4 | `command::format_patch_test::format_numbered_config` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH 1/2] <标题1>`、`Subject: [PATCH 2/2] <标题2>` |
| AC5 | `command::format_patch_test::format_numbered_config` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `format.numbered`；stdout 为空 |
| AC6 | `command::format_patch_test::format_numbered_config` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH 1/2] <标题1>`、`Subject: [PATCH 2/2] <标题2>` |
| AC7 | `command::format_patch_test::format_numbered_config` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] <标题>` |
| AC8 | `command::format_patch_test::format_numbered_config` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] *** SUBJECT HERE ***`、`Subject: [PATCH] <标题>` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.numbered` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_numbered_config)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_numbered_config`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-01`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.numbered`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.numbered配置与CLI覆盖; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-01; writeset=序列化于FPG-01; release=independent; split-from=FPG-01; exception=N/A`

---

### Task FPG-02: 核实 `529 FP-11` 的 `GIT_COMMITTER_*` 身份解析成功路径（#881 身份切片；自本卡拆出 FPG-53）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 GC-01 核实 `529 FP-11`（ADR-FP-07）合入后的 `resolve_signoff_identity` 是否严格遵循 Git 规则（`GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` 优先于 `user.name`/`user.email`，`user.useConfigOnly=true` 时环境变量仍优先，邮箱再回退 `EMAIL`；空邮箱成功输出 `<>`），补齐边缘测试并完成身份切片成功路径验收；3 类 ADR-FP-07 失败路径由自本卡拆出的 `FPG-53` 核实。本卡不关闭 `#881`（完整 `#881` t4036 闭环在 `FPG-51` 达成）。

**Out of scope:** ADR-FP-07 失败路径（由 `FPG-53` 承接）；`#881` 的 t4036 MIME 头部分（由 `FPG-30`、`FPG-51` 闭环）；不改 `libra commit` 身份逻辑。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/529.md` `FP-11` 按 ADR-FP-07 负责 `GIT_COMMITTER_*` 身份解析并记录 `#881` 另依赖 `#897`；`CliInvalidTarget` 映射 `LBR-CLI-003` | `src/command/format_patch.rs:2050-2065`、`src/utils/error.rs:375`、`issues/529.md:166,179,324` |

**Acceptance criteria:**

- [ ] AC1：无任何 `user.*` 配置、只设 `GIT_COMMITTER_NAME="C O Mitter"` 与 `GIT_COMMITTER_EMAIL=committer@example.com` 时 `-s --stdout -1` 成功（#881 第 1 项）。
- [ ] AC2：`--signoff` 且同时设置 `GIT_COMMITTER_NAME=Env Name` 与 `user.name=Cfg Name` 时，`Signed-off-by:` 姓名取 `Env Name`。
- [ ] AC3：同时设置 `GIT_COMMITTER_EMAIL=env@example.com` 与 `user.email=cfg@example.com` 时邮箱取 `env@example.com`。
- [ ] AC4：未设 `user.email` 与 `GIT_COMMITTER_EMAIL`、设 `EMAIL=fallback@example.com` 且未启用 `user.useConfigOnly` 时邮箱回退 `EMAIL`。
- [ ] AC5：已配置 `user.email=cfg@example.com` 且 `GIT_COMMITTER_EMAIL=""` 时空环境变量作为有效空邮箱（不回退到配置，529 探针 #35 同形）。
- [ ] AC6：`user.useConfigOnly=true`、无 `user.email`、设 `GIT_COMMITTER_EMAIL=e@x` 时 `-s --stdout -1` 成功且邮箱取 `e@x`（环境变量优先，529 探针 #37）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::signoff_git_committer_env_precedence` | 0 | `Signed-off-by:` 行逐字为 `Signed-off-by: C O Mitter <committer@example.com>` |
| AC2 | `command::format_patch_test::signoff_git_committer_env_precedence` | 0 | stdout 含 `Signed-off-by: Env Name <`；必须不出现 `Cfg Name` |
| AC3 | `command::format_patch_test::signoff_git_committer_env_precedence` | 0 | stdout 的 `Signed-off-by:` 行以 `<env@example.com>` 结尾 |
| AC4 | `command::format_patch_test::signoff_git_committer_env_precedence` | 0 | stdout 的 `Signed-off-by:` 行以 `<fallback@example.com>` 结尾 |
| AC5 | `command::format_patch_test::signoff_git_committer_env_precedence` | 0 | stdout 的 `Signed-off-by:` 行以 ` <>` 结尾（不含 `cfg@example.com`） |
| AC6 | `command::format_patch_test::signoff_git_committer_env_precedence` | 0 | stdout 的 `Signed-off-by:` 行以 `<e@x>` 结尾 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `GIT_COMMITTER_NAME`、`GIT_COMMITTER_EMAIL` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::signoff_git_committer_env_precedence)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::signoff_git_committer_env_precedence`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-32`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`GIT_COMMITTER_NAME`、`GIT_COMMITTER_EMAIL`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=GIT_COMMITTER_*身份核验; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-32; writeset=序列化于FPG-32; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-53: 核实 `529 FP-11` 身份解析的失败路径（#881 身份切片；529 探针 #34、#36 与身份缺失；自 FPG-02 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-02` 拆出的独立轴：按 GC-01 核实 ADR-FP-07 的 3 类失败（`GIT_COMMITTER_NAME` 为空、`user.useConfigOnly=true` 下无提交者邮箱、身份完全缺失）均以 `LBR-CLI-003` 失败（沿用 `529 FP-11` 交付的退出码约定）且 stdout 为空。

**Out of scope:** `GIT_COMMITTER_*` 优先与回退的成功路径（由 `FPG-02` 承接，本卡依赖其先行完成）；`#881` 的 t4036 MIME 头部分（由 `FPG-30`、`FPG-51` 闭环）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/529.md` `FP-11` 按 ADR-FP-07 定义 3 类身份失败；`CliInvalidTarget` 映射 `LBR-CLI-003` | `src/command/format_patch.rs:2050-2065`、`src/utils/error.rs:375`、`issues/529.md:166,324-330` |

**Acceptance criteria:**

- [ ] AC1：`GIT_COMMITTER_NAME=""` 时拒绝执行。
- [ ] AC2：`user.useConfigOnly=true`、未设置 `GIT_COMMITTER_EMAIL`、无 `user.email` 且设 `EMAIL=m@x` 时拒绝执行（`EMAIL` 不作回退；529 探针 #36）。
- [ ] AC3：清空 `GIT_COMMITTER_*`、`GIT_AUTHOR_*`、`EMAIL` 与 `user.*` 后身份完全缺失时拒绝执行。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::signoff_identity_failures` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `empty ident name`；stdout 为空 |
| AC2 | `command::format_patch_test::signoff_identity_failures` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `no email was given`；stdout 为空 |
| AC3 | `command::format_patch_test::signoff_identity_failures` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `identity unknown`；stdout 为空 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（身份解析失败路径说明（529 已交付；核对无需改动时记录结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::signoff_identity_failures)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::signoff_identity_failures`；证明 AC1–AC3）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-02`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：身份解析失败路径说明（529 已交付；核对无需改动时记录结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=529FP-11身份失败路径核实; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-02; writeset=序列化于FPG-02; release=independent; split-from=FPG-02; exception=N/A`

---

### Task FPG-03: `--ignore-if-in-upstream` 按 Git 只取 `b..a` 可达集合差的上游 patch-id 并补 t4014#4 回归门（#882）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 GC-01 先在 `DEP-FPG-01` 解除后的代码上复现 #882（t4014#4，含 tag 引用；报告于 0.30.91，2026-11-18 以 0.30.207 按 t4014 setup 复现时输出 2 个补丁、与 Git 一致）：仍复现则定位 `resolve_range_commits`/`git_patch_id_for_commit` 的根因并修复，不复现则只补回归门。同时按 Git 只对 `b..a`（自排除端可达、自包含端不可达的提交，即两端可达集合之差；Git 以对调两端 `UNINTERESTING` 标记的遍历求得，线性分叉时即 merge base 之后的上游侧提交）计算 patch-id（`git show 6de20f6092:builtin/log.c` 第 1192–1204 行「given a range a..b get all patch ids for b..a」），修正现行把排除端全部祖先计入上游、误删与 merge base 之前某提交 patch-id 相同的补丁的问题（`src/command/format_patch.rs:690-711,734-746`）。各 AC 只比较补丁选择结果（`Subject:` 行序列，夹具各提交标题互不相同），不比较 diff 字节（差异隔离见「验证执行协议」第 3 项）。

**Out of scope:** 多 merge base（criss-cross）历史下 `b..a` 上游集合的单列验收（实现按可达集合差，见 `DEFER-FPG-05`）；不修改二进制文件的 Git patch-id 兼容边界；无范围参数的 `--ignore-if-in-upstream`（Git 报 `need exactly one range`，见 `DEFER-FPG-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_range_commits` 通过 `get_reachable_commits` 与 `git_patch_id_for_commit` 计算上游 patch-id 过滤 | `src/command/format_patch.rs:688-750,1126-1213` |
| 排除端经 `get_reachable_commits` 收集其全部祖先（含自身）作为上游集合并全部计算 patch-id，而 Git 只对可达集合差 `b..a`（自上游可达、自 topic 不可达）计算 | `src/command/log.rs:1080`、`src/command/format_patch.rs:688-750` |

**Acceptance criteria:**

- [ ] AC1：按 `git@6de20f6092` `t/t4014-format-patch.sh` 第 4 个用例（setup 含 `update-index --chmod=+x` 与 `apply --index`）构造带 tag 的夹具时，`--ignore-if-in-upstream --stdout v2..v1` 选出的补丁与同夹具 Git 一致。
- [ ] AC2：既有补丁等价过滤场景保持通过。
- [ ] AC3：上游在 merge base 之前加入又撤销某变更、topic 重新加入该变更时，`--ignore-if-in-upstream --stdout main..topic` 只过滤 patch-id 出现在 `b..a` 上游提交（本线性夹具中即 merge base 之后的上游提交）中的补丁（#882 第 2–3 项；夹具：`main` 上 `base` → `add X` → `revert X`，topic 自此切出后依次提交 `re-add X`（与 `add X` 同 patch-id）、`dup`（与 topic 切出后 `main` 上新增的 `dup-upstream` 同 patch-id）、`add Y`）。
- [ ] AC4：无 tag、全部变更均未出现在上游的范围在 `--ignore-if-in-upstream` 下输出全部提交（#882 第 3 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::ignore_if_in_upstream_tag_t4014_4` | 0 | `Subject:` 行序列与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 为 `Subject: [PATCH 1/2] Side changes #1`、`Subject: [PATCH 2/2] Side changes #3 with \n backslash-n in it.`） |
| AC2 | `ignore_if_in_upstream_suppresses_patch_equivalent_commits`（target `compat_format_patch_mail_roundtrip`） | 0 | 既有断言不变（`tests/compat/format_patch_mail_roundtrip_test.rs:479`） |
| AC3 | `command::format_patch_test::ignore_if_in_upstream_tag_t4014_4` | 0 | `Subject:` 行序列与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 为 `Subject: [PATCH 1/2] re-add X`、`Subject: [PATCH 2/2] add Y`） |
| AC4 | `command::format_patch_test::ignore_if_in_upstream_tag_t4014_4` | 0 | `Subject:` 行序列与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（`--ignore-if-in-upstream` 只取 `b..a`（上游相对 topic 的可达集合差）上游提交的说明（无需改动时记录核对结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::ignore_if_in_upstream_tag_t4014_4)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::ignore_if_in_upstream_tag_t4014_4`；证明 AC1、AC3–AC4）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail -E 'test(=ignore_if_in_upstream_suppresses_patch_equivalent_commits)'`（既有；具名验收门，期望集合 N=1：`ignore_if_in_upstream_suppresses_patch_equivalent_commits`（target `compat_format_patch_mail_roundtrip`）；证明 AC2）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-53`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：`--ignore-if-in-upstream` 只取 `b..a`（上游相对 topic 的可达集合差）上游提交的说明（无需改动时记录核对结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--ignore-if-in-upstream上游patch-id只取b..a; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-53; writeset=序列化于FPG-53; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-04: 支持 `format.headers`（#883 第 1–5 项；自本卡拆出 FPG-37、FPG-44）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `src/internal/config.rs` 新增按 Git 顺序（system → global → local，同层按写入顺序）返回同一键全部值的严格级联读取 helper（`read_format_config` 只返回首个值，不能满足多值键），并在 `resolve_format_config` 用它读取多值 `format.headers`，原样插入每封邮件头区（带尾随换行的取值不产生空行，同名 `To:`/`Cc:` 按 Git 折叠为逗号结尾的头加缩进续行，续行缩进以 Git 黄金值为准，现行 `fold_addresses` 为四个空格）；取值统一按「移除 CR/LF 及其它控制字符后成功输出」处理以防头注入（只移除控制字符、保留普通空白，不得复用会折叠连续空白的 `sanitize_header_value`）。`--add-header`/`--no-add-header` 由自本卡拆出的 `FPG-44` 承接，`format.to`/`format.cc` 与 `--to`/`--no-to` 顺序由 `FPG-37` 承接。 与 Git 的有意差异（GC-03/GC-07）：Git 原样输出含 CR/LF 的头值，本计划移除控制字符，须在 `COMPATIBILITY.md:224` 与命令文档注明。 本卡命令夹具均为两补丁系列（`-2 --stdout`），#883 要求每封邮件都带这些头，映射中的规范头区比较均按「验证执行协议」第 3 项对各封补丁邮件拼接后比较。 AC3、AC5、AC6 的夹具只含非 `To:`/`Cc:` 的附加头，AC1、AC2、AC4 的夹具只含 `To:`/`Cc:` 值且 `To:` 先于 `Cc:` 写入（`To:`/`Cc:` 与其它附加头的相对顺序由 `FPG-57` AC6 验收）。

**Out of scope:** `--add-header`/`--no-add-header`（由 `FPG-44` 承接）；`format.to`/`format.cc` 与命令行收件人顺序（由 `FPG-37` 承接）；`sendemail.*` 配置（永久非目标，理由：本计划不实现邮件发送）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_recipients` 虽使用 `args.to`/`args.cc`，但 `no_to`/`no_cc` 为简单布尔标志，会抹除在其之后出现的 `--to`/`--cc` | `src/command/format_patch.rs:194-210,1918-1930` |
| `resolve_format_config` 未读取 `format.headers`/`format.to`/`format.cc` | `src/command/format_patch.rs:1861-1902` |
| `FormatPatchArgs` 无 `--add-header`/`--no-add-header`；收件人头由 `push_recipient_headers` 输出，控制字符由 `sanitize_header_value` 过滤 | `src/command/format_patch.rs:97-364,1947-1979` |
| `read_format_config` 返回 `Option<String>`，底层 `read_cascaded_config_value_strict` 找到首个值即返回；`libra config set --add` 可写入多值 | `src/command/format_patch.rs:1904-1916`、`src/internal/config.rs:1872` |
| `sanitize_header_value` 会折叠连续空白，不能用于需原样保留的 `format.headers` | `src/command/format_patch.rs:1959-1979` |

**Acceptance criteria:**

- [ ] AC1：`format.headers` 依次 `--add` 两值 `To: R E Cipient <rcipient@example.com>`、`Cc: S E Cipient <scipient@example.com>` 时插入 `To:` 头（t4014#12/#13）。
- [ ] AC2：同一夹具插入 `Cc:` 头（t4014#12/#13）。
- [ ] AC3：`format.headers` 取值带尾随换行时不产生空行。
- [ ] AC4：`format.headers` 含两个 `To:` 值时折叠为一个逗号结尾的头加Git 风格缩进续行（t4014#14）。
- [ ] AC5：`format.headers` 值含 `\r\n` 注入（如 `X-A: 1\r\nBcc: evil@example.com`）时不产生独立头行。
- [ ] AC6：含连续空格的合法 `format.headers` 值（如 `X-Note: a  b`）原样保留。
- [ ] AC7：级联多值 helper 按 system → global → local、同层按写入顺序返回同一键全部值（隔离的 `LIBRA_CONFIG_SYSTEM_DB`/`LIBRA_CONFIG_GLOBAL_DB` 与仓库库：system 写 `s1`；global 依次写 `g2`、`g1`；local 依次写 `l2`、`l1`，同层写入顺序与字典序相反，GC-13）。
- [ ] AC8：全局层配置库不可读时 helper 严格失败而非静默跳过（隔离的全局库路径指向目录；系统层沿用 `read_cascaded_config_value_strict` 对系统库读失败即跳过的既有契约）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_headers_and_add_header` | 0 | 两补丁系列（`-2 --stdout`）中逐封比较：每封补丁邮件规范头区中以 `To:` 开头的头（含续行）与同夹具 Git 黄金值中同序号邮件的对应头逐字一致（双方均加 `--no-thread --no-signature`；两封均相等才通过，保留邮件边界） |
| AC2 | `command::format_patch_test::format_headers_and_add_header` | 0 | 两补丁系列（`-2 --stdout`）中逐封比较：每封补丁邮件规范头区中以 `Cc:` 开头的头（含续行）与同夹具 Git 黄金值中同序号邮件的对应头逐字一致（双方均加 `--no-thread --no-signature`；两封均相等才通过，保留邮件边界） |
| AC3 | `command::format_patch_test::format_headers_and_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（夹具中带尾随换行的值之后仍有其它头，头区若提前结束即不等） |
| AC4 | `command::format_patch_test::format_headers_and_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC5 | `command::format_patch_test::format_headers_and_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与「同一夹具、取值先去除控制字符后」的 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（即被清洗的头仍保留、且不存在独立的注入头行） |
| AC6 | `command::format_patch_test::format_headers_and_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC7 | `internal::config::tests::cascaded_values_strict_preserves_layer_order`（lib） | 0（单元测试） | 返回的值序列逐项等于 `s1`、`g2`、`g1`、`l2`、`l1` |
| AC8 | `internal::config::tests::cascaded_values_strict_reports_read_failure`（lib） | 0（单元测试） | 返回 `Err`，错误文本含不可读的配置键名；不返回部分值 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.headers` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_headers_and_add_header)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_headers_and_add_header`；证明 AC1–AC6）
- [ ] `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=internal::config::tests::cascaded_values_strict_preserves_layer_order)'`（`(new)`；具名验收门，期望集合 N=1：`internal::config::tests::cascaded_values_strict_preserves_layer_order`（lib）；证明 AC7）
- [ ] `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=internal::config::tests::cascaded_values_strict_reports_read_failure)'`（`(new)`；具名验收门，期望集合 N=1：`internal::config::tests::cascaded_values_strict_reports_read_failure`（lib）；证明 AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-03`, `DEP-FPG-05`（核实 `src/internal/config.rs` 无在途写者）

**Implementation write set:** `src/command/format_patch.rs`, `src/internal/config.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `src/internal/config.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.headers`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.headers与级联多值读取; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=3/8; landing=3; prod-files=2; scope=M; deps=FPG-03,DEP-FPG-05; writeset=序列化于FPG-03; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-56: `format.numbered` 多层与同层重复取值按 Git 逐条处理（#896 配置部分；自 FPG-32 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-32` 拆出的独立轴：经 `FPG-04` 新增的级联多值 helper，按 Git 配置顺序（system → global → local，同层按写入顺序）逐条处理全部 `format.numbered` 取值：`auto` 只置自动编号、不清除此前由 `true` 置位的编号；布尔值置编号，为假时同时清除自动编号（`git show 6de20f6092:builtin/log.c` 第 1009–1016 行）。Git 先读配置后解析参数（第 2102、2134 行），故非法值在给出 `-n`/`-N` 时同样报错；仅由配置置位的编号遇命令行 `-k` 时复位而非冲突（第 2204–2209 行，t4021#8）。全局层经临时 `LIBRA_CONFIG_GLOBAL_DB` 隔离（GC-13）。

**Out of scope:** 单层单值读取与 `-n`/`-N` 覆盖（由 `FPG-32` 承接，本卡依赖其先行完成）；级联多值 helper 本身（由 `FPG-04` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `read_format_config` 只返回首个命中值，无法表达 `true` 之后 `auto` 仍编号 | `src/command/format_patch.rs:1904-1916` |
| Git `format.numbered` 回调：`auto` 只置 `auto_number`；布尔值置 `numbered` 并令 `auto_number = auto_number && numbered` | `git show 6de20f6092:builtin/log.c` 第 1009–1016 行 |

**Acceptance criteria:**

- [ ] AC1：隔离的全局层 `format.numbered=true`、本地 `format.numbered=auto` 时 `-1 --stdout` 仍编号（`auto` 不清除先前的 `true`）。
- [ ] AC2：本地以 `--add` 依次写入 `format.numbered=true`、`format.numbered=auto` 时 `-1 --stdout` 仍编号（只取同层最后一个值的实现不编号）。
- [ ] AC3：本地以 `--add` 依次写入 `format.numbered=true`、`format.numbered=false` 时 `-1 --stdout` 不编号（只读同层首个值的实现会编号）。
- [ ] AC4：隔离的全局层 `format.numbered=auto`、本地 `format.numbered=false` 时 `-2 --stdout` 不编号。
- [ ] AC5：`format.numbered=true` 下 `-k -1 --stdout` 成功且主题不加前缀（仅由配置置位的编号被 `-k` 复位，t4021#8）。
- [ ] AC6：`format.numbered=bogus` 下给出 `-n -1 --stdout` 时仍拒绝（配置先于参数读取）。
- [ ] AC7：`format.numbered=yes`（Git 布尔别名）时 `-1 --stdout` 编号。
- [ ] AC8：`format.numbered=auto` 下 `-N -2 --stdout` 不编号（t4021#10）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_numbered_config_layers` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH 1/1] <标题>` |
| AC2 | `command::format_patch_test::format_numbered_config_layers` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH 1/1] <标题>` |
| AC3 | `command::format_patch_test::format_numbered_config_layers` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] <标题>` |
| AC4 | `command::format_patch_test::format_numbered_config_layers` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] <标题1>`、`Subject: [PATCH] <标题2>` |
| AC5 | `command::format_patch_test::format_numbered_config_layers` | 0 | `Subject:` 行序列逐字为 `Subject: <标题>` |
| AC6 | `command::format_patch_test::format_numbered_config_layers` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `format.numbered`；stdout 为空 |
| AC7 | `command::format_patch_test::format_numbered_config_layers` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH 1/1] <标题>` |
| AC8 | `command::format_patch_test::format_numbered_config_layers` | 0 | `Subject:` 行序列逐字为 `Subject: [PATCH] <标题1>`、`Subject: [PATCH] <标题2>` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.numbered` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_numbered_config_layers)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_numbered_config_layers`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-04`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.numbered`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.numbered多值逐条处理; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-04; writeset=序列化于FPG-04; release=independent; split-from=FPG-32; exception=N/A`

---

### Task FPG-44: 支持 `--add-header`/`--no-add-header`（#883 第 6、6b、7 项；自 FPG-04 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-04` 拆出的独立轴：新增可重复的 `--add-header=<header>` 与 `--no-add-header`，按命令行出现顺序求值（ADR-FPG-02）：`--no-add-header` 与 Git `header_callback` 一致，清空此前累积的 `format.headers` 值、`--add-header` 头与 `--to`/`--cc` 地址，其后的 `--add-header`/`--to`/`--cc` 重新生效（其后 `--to` 重新生效由 `FPG-57` AC7 验收）；命令行头与配置头按 Git 合并折叠；取值与 `FPG-04` 相同按「移除控制字符后成功输出」防注入。`format.headers` 的 `Cc:` 与 `--cc` 地址同样按 Git 合并折叠为一个 `Cc:` 头（t4014#15，#883）。 本卡命令夹具均为两补丁系列（`-2 --stdout`），#883 要求每封邮件都带这些头，映射中的规范头区比较均按「验证执行协议」第 3 项对各封补丁邮件拼接后比较。

**Out of scope:** `format.headers` 读取与折叠（由 `FPG-04` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FormatPatchArgs` 无 `--add-header`/`--no-add-header` | `src/command/format_patch.rs:97-364` |
| Git `header_callback` 在否定时清空 `extra_hdr`、`extra_to`、`extra_cc` | `git show 6de20f6092:builtin/log.c` 第 1649–1660 行 |

**Acceptance criteria:**

- [ ] AC1：`--add-header="Cc: R E Cipient <rcipient@example.com>"` 被接受并插入该头（t4014#17）。
- [ ] AC2：配置了 `format.headers` 时 `--no-add-header` 丢弃全部配置头并保留常规头（t4014#33）。
- [ ] AC3：`format.headers` 的 `Cc:` 值与 `--add-header` 的 `Cc:` 值折叠为一个头（t4014#18）。
- [ ] AC4：`--add-header` 值含 `\r\n` 注入（如 `X-A: 1\r\nBcc: evil@example.com`）时不产生独立头行。
- [ ] AC5：`--add-header="X-A: 1" --no-add-header` 不输出 `X-A:`。
- [ ] AC6：`--to=a@example.com --no-add-header` 不输出 `To:`（与 Git 一致）。
- [ ] AC7：`--no-add-header --add-header="X-B: 2"` 输出 `X-B: 2`（其后重新添加生效）。
- [ ] AC8：`format.headers="Cc: R E Cipient <rcipient@example.com>"` 加 `--cc="S E Cipient <scipient@example.com>"` 时折叠为一个 `Cc:` 头（配置值在前；t4014#15，#883）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::add_header_and_no_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC2 | `command::format_patch_test::add_header_and_no_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC3 | `command::format_patch_test::add_header_and_no_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC4 | `command::format_patch_test::add_header_and_no_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与「同一夹具、取值先去除控制字符后」的 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（即被清洗的头仍保留、且不存在独立的注入头行） |
| AC5 | `command::format_patch_test::add_header_and_no_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC6 | `command::format_patch_test::add_header_and_no_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC7 | `command::format_patch_test::add_header_and_no_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC8 | `command::format_patch_test::add_header_and_no_add_header` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--add-header`、`--no-add-header` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::add_header_and_no_add_header)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::add_header_and_no_add_header`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-56`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--add-header`、`--no-add-header`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--add-header/--no-add-header; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-56; writeset=序列化于FPG-56; release=independent; split-from=FPG-04; exception=N/A`

---

### Task FPG-37: 支持 `format.to`/`format.cc` 与 `--to`/`--no-to`、`--cc`/`--no-cc` 出现顺序语义（#883 第 9 项, #914；自 FPG-04 拆出；自本卡拆出 FPG-57）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-04` 拆出的独立轴：读取多值 `format.to`/`format.cc` 输出 `To:`/`Cc:`，并按命令行实际出现顺序求值 `--to`/`--no-to`、`--cc`/`--no-cc`（`--no-to` 清空此前的配置与命令行地址，其后的 `--to` 仍生效；`--to=… --no-to` 不输出 `To:`）；取值经 `sanitize_header_value` 防止头注入（控制字符替换为空格并折叠连续空白；与 Git 原样输出的有意差异同 `FPG-04`，须在 `COMPATIBILITY.md:224` 与命令文档注明）。 多值读取复用 `FPG-04` 新增的级联多值 helper。 本卡命令夹具均为两补丁系列（`-2 --stdout`），#883 要求每封邮件都带这些头，映射中的规范头区比较均按「验证执行协议」第 3 项对各封补丁邮件拼接后比较。

**Out of scope:** `format.headers`（由 `FPG-04` 承接，本卡依赖其先行完成）；`--add-header`/`--no-add-header`（由 `FPG-44` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `no_to`/`no_cc` 为简单布尔，`resolve_recipients` 会抹除其后出现的 `--to`/`--cc`；`resolve_format_config` 未读取 `format.to`/`format.cc` | `src/command/format_patch.rs:194-210,1861-1902,1918-1935` |

**Acceptance criteria:**

- [ ] AC1：`format.to` 以 `--add` 依次写入 `R E Cipient <rcipient@example.com>`、`S E Cipient <scipient@example.com>` 两值时输出折叠的 `To:` 头（t4014#22 的多值扩展）。
- [ ] AC2：`format.cc` 以 `--add` 依次写入 `R E Cipient <rcipient@example.com>`、`S E Cipient <scipient@example.com>` 两值时输出折叠的 `Cc:` 头。
- [ ] AC3：`format.to=a@example.com` 下 `--no-to --to="Someone Else <else@out.there>"` 只保留后者（#914）。
- [ ] AC4：`--to=a@example.com --cc=c@example.com --no-to` 不输出 `To:` 而保留 `Cc:`（#914；`--no-to` 只清空 `To:` 列表）。
- [ ] AC5：`format.cc=a@example.com` 下 `--no-cc --cc=b@example.com` 只保留后者。
- [ ] AC6：`--to=t@example.com --cc=a@example.com --no-cc` 不输出 `Cc:` 而保留 `To:`（`--no-cc` 只清空 `Cc:` 列表）。
- [ ] AC7：`format.to` 值为 `a@example.com\r\nBcc: evil@example.com` 时不产生独立头行。
- [ ] AC8：`format.cc` 值为 `a@example.com\r\nBcc: evil@example.com` 时不产生独立头行。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_to_cc_and_no_to_ordering` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC2 | `command::format_patch_test::format_to_cc_and_no_to_ordering` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC3 | `command::format_patch_test::format_to_cc_and_no_to_ordering` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC4 | `command::format_patch_test::format_to_cc_and_no_to_ordering` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值无 `To:` 行、保留 `Cc: c@example.com`） |
| AC5 | `command::format_patch_test::format_to_cc_and_no_to_ordering` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC6 | `command::format_patch_test::format_to_cc_and_no_to_ordering` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值无 `Cc:` 行、保留 `To: t@example.com`） |
| AC7 | `command::format_patch_test::format_to_cc_and_no_to_ordering` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与「同一夹具、取值先按 `sanitize_header_value` 规则（控制字符替换为空格后折叠连续空白）清洗后」的 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（即被清洗的头仍保留、且不存在独立的注入头行） |
| AC8 | `command::format_patch_test::format_to_cc_and_no_to_ordering` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与「同一夹具、取值先按 `sanitize_header_value` 规则（控制字符替换为空格后折叠连续空白）清洗后」的 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（即被清洗的头仍保留、且不存在独立的注入头行） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.to`、`format.cc` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_to_cc_and_no_to_ordering)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_to_cc_and_no_to_ordering`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-44`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.to`、`format.cc`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.to/format.cc与--to/--no-to顺序; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-44; writeset=序列化于FPG-44; release=independent; split-from=FPG-04; exception=N/A`

---

### Task FPG-57: `To:`/`Cc:` 统一累积列表与附加头顺序（#883 第 9 项、#914 第 1 项；自 FPG-37 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-37` 拆出的独立轴：按 Git 把 `format.headers` 中的 `To:`/`Cc:` 值（不区分大小写匹配 `to: `/`cc: `，`git show 6de20f6092:builtin/log.c` 第 958–975 行）、`format.to`/`format.cc`、`--add-header` 中的 `To:`/`Cc:` 与 `--to`/`--cc` 放入同一累积列表：配置侧跨键按条目出现顺序合并（在 `src/internal/config.rs` 为 `FPG-04` 的级联多值 helper 增加按多个键返回 `(键, 值)` 条目的变体，层序同 `FPG-04`，同层按写入顺序即配置表自增 id），`--no-to`/`--no-cc`/`--no-add-header` 清空对应列表（第 1649–1660、2060–2061 行）；非 `To:`/`Cc:` 附加头先于合并后的 `To:`、`Cc:` 输出（第 2166–2191 行）；封面信同样输出这些头。各值仍按其来源卡的清洗规则（与 Git 原样输出的差异见 `FPG-04`、`FPG-37`）。本卡命令夹具除 AC8 外均为两补丁系列（`-2 --stdout`）。

**Out of scope:** `format.headers` 单键读取与折叠（由 `FPG-04` 承接）；`--add-header`/`--no-add-header` 解析（由 `FPG-44` 承接）；`format.to`/`format.cc` 读取与 `--to`/`--no-to` 顺序（由 `FPG-37` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format.headers` 的 `To:`/`Cc:`、`format.to`/`format.cc` 与命令行地址现分别处理，无统一列表；封面信不输出附加头 | `src/command/format_patch.rs:1361-1445,1918-1979` |
| Git `add_header` 把 `to: `/`cc: ` 开头的值放入 `extra_to`/`extra_cc`；输出时先附加头、再合并的 `To:`、`Cc:` | `git show 6de20f6092:builtin/log.c` 第 958–975、2166–2191 行 |

**Acceptance criteria:**

- [ ] AC1：本地以 `--add` 依次写入 `format.to="T1 <t1@example.com>"`、`format.headers="To: H <h@example.com>"`、`format.to="T2 <t2@example.com>"` 时合并为一个按配置出现顺序（T1、H、T2）列出三地址的 `To:` 头（按键分组拼接的实现会输出 T1、T2、H）。
- [ ] AC2：AC1 的配置另加 `format.cc="C <c@example.com>"` 时，`--no-to` 清空包括 `format.headers` 来源在内的全部 `To:` 地址而保留 `Cc:`（#914 第 1 项；Git `--no-to` 只清空 `extra_to`）。
- [ ] AC3：`format.headers="Cc: H <h@example.com>"` 与 `format.to="T <t@example.com>"` 下 `--no-cc` 不输出 `Cc:` 而保留 `To:`（Git `--no-cc` 只清空 `extra_cc`）。
- [ ] AC4：`format.to=t@example.com` 下 `--no-add-header` 不输出 `To:`（`--no-add-header` 同样清空 `format.to` 来源）。
- [ ] AC5：`format.cc=c@example.com` 下 `--no-add-header` 不输出 `Cc:`。
- [ ] AC6：`format.headers` 以 `--add` 依次写入小写键的 `to: R E Cipient <rcipient@example.com>`（Git 不区分大小写匹配 `to: `）、`X-Extra: one` 时该值并入 `To:` 头且 `X-Extra:` 先于 `To:` 输出。
- [ ] AC7：`--no-add-header --to=b@example.com` 时其后的 `--to` 重新生效（#883 第 9 项；Git `header_callback` 只清空此前累积的值，`--to` 为 `OPT_STRING_LIST` 继续追加，`git show 6de20f6092:builtin/log.c` 第 1649–1660、2060–2061 行）。
- [ ] AC8：`format.headers` 以 `--add` 依次写入 `X-Foo: bar`、`To: R E Cipient <rcipient@example.com>` 时 `--cover-letter -2 --stdout` 的封面信同样输出这些头。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::recipient_lists_unify_like_git` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 黄金值依次为 `T1`、`H`、`T2`） |
| AC2 | `command::format_patch_test::recipient_lists_unify_like_git` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值无 `To:` 行、保留 `Cc: C <c@example.com>`） |
| AC3 | `command::format_patch_test::recipient_lists_unify_like_git` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值无 `Cc:` 行、保留 `To: T <t@example.com>`） |
| AC4 | `command::format_patch_test::recipient_lists_unify_like_git` | 0 | 各封补丁邮件的头区均不存在以 `To:` 开头的行 |
| AC5 | `command::format_patch_test::recipient_lists_unify_like_git` | 0 | 各封补丁邮件的头区均不存在以 `Cc:` 开头的行 |
| AC6 | `command::format_patch_test::recipient_lists_unify_like_git` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC7 | `command::format_patch_test::recipient_lists_unify_like_git` | 0 | 两补丁系列（`-2 --stdout`）中各封补丁邮件的规范头区（按输出顺序拼接）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC8 | `command::format_patch_test::recipient_lists_unify_like_git` | 0 | 封面信头区中以 `X-Foo:`、`To:` 开头的头（含续行）按出现顺序组成的序列与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.to`、`format.headers` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::recipient_lists_unify_like_git)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::recipient_lists_unify_like_git`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-37`, `DEP-FPG-05`（核实 `src/internal/config.rs` 无在途写者）

**Implementation write set:** `src/command/format_patch.rs`, `src/internal/config.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `src/internal/config.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.to`、`format.headers`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=To/Cc统一累积列表与附加头顺序; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=3; prod-files=2; scope=M; deps=FPG-37,DEP-FPG-05; writeset=序列化于FPG-37; release=independent; split-from=FPG-37; exception=N/A`

---

### Task FPG-05: 支持 `--no-notes` 与 `--notes`/`--no-notes` 最后出现者生效（#884 命令行部分；自本卡拆出 FPG-36）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 新增 `--no-notes`，`--notes[=<ref>]`/`--no-notes` 按最后出现者生效，重复 `--notes=<ref>` 合并各 ref 的 notes；`format.notes` 配置由自本卡拆出的 `FPG-36` 承接。 notes 夹具（`FPG-05`、`FPG-36`、`FPG-41` 共用）：同一提交在 `refs/notes/commits`（`default note`）、`refs/notes/note1`（`this is note 1`）、`refs/notes/note2`（`this is note 2`）上各有一条 note。

**Out of scope:** `format.notes` 配置（由 `FPG-36` 承接）；`libra notes` 子命令行为（永久非目标）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FormatPatchArgs` 仅有 `notes`，无 `no_notes`；`resolve_format_config` 未读 `format.notes` | `src/command/format_patch.rs:236-240,1861-1902` |

**Acceptance criteria:**

- [ ] AC1：`-1 --stdout --no-notes` 被接受且不输出 notes。
- [ ] AC2：`--notes --no-notes` 不输出 notes（后者生效）。
- [ ] AC3：`--no-notes --notes` 输出 notes（后者生效）。
- [ ] AC4：`--notes=note2 --notes=note1 --notes=note2` 按首次出现顺序合并两个 ref 的 notes，重复的 `note2` 只输出一次（Git `notes.c` 追加前去重）。
- [ ] AC5：`--notes=note1 --no-notes --notes=note2` 只输出 note2（`--no-notes` 清空此前累积的 ref，Git `disable_display_notes`）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::no_notes_and_ordering` | 0 | stdout 不存在含夹具任一 note 正文的行 |
| AC2 | `command::format_patch_test::no_notes_and_ordering` | 0 | stdout 不存在含夹具任一 note 正文的行 |
| AC3 | `command::format_patch_test::no_notes_and_ordering` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC4 | `command::format_patch_test::no_notes_and_ordering` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 黄金值依次为 note2、note1 各一段） |
| AC5 | `command::format_patch_test::no_notes_and_ordering` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值只含 note2 正文） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--no-notes` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::no_notes_and_ordering)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::no_notes_and_ordering`；证明 AC1–AC5）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-57`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--no-notes`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--no-notes与命令行notes顺序; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-57; writeset=序列化于FPG-57; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-36: 支持 `format.notes` 配置取值（#884 配置部分；自 FPG-05 拆出；自本卡拆出 FPG-41）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-05` 拆出的独立轴：经 `FPG-04` 新增的级联多值 helper 读取 `format.notes`（`true`/`false`/`<ref>`，多值累加，`false` 清空此前累积值）。配置与命令行 `--notes`/`--no-notes` 的交互由自本卡拆出的 `FPG-41` 承接。 notes 夹具（`FPG-05`、`FPG-36`、`FPG-41` 共用）：同一提交在 `refs/notes/commits`（`default note`）、`refs/notes/note1`（`this is note 1`）、`refs/notes/note2`（`this is note 2`）上各有一条 note。

**Out of scope:** 命令行 `--no-notes` 与顺序（由 `FPG-05` 承接）；配置与命令行交互（由 `FPG-41` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_format_config` 未读取 `format.notes`；notes 渲染在 `render_notes_block` | `src/command/format_patch.rs:1012-1057,1861-1902` |

**Acceptance criteria:**

- [ ] AC1：`format.notes=true` 时输出默认 ref 的 notes。
- [ ] AC2：隔离的全局层 `format.notes=true`、本地 `format.notes=false` 时不输出 notes（后出现的 `false` 清空）。
- [ ] AC3：`format.notes=note1` 时输出该 ref 的 notes。
- [ ] AC4：`format.notes` 以 `--add` 依次写入 `note2`、`note1` 时按写入顺序累加输出（#884 第 3b 项；Git 不排序）。
- [ ] AC5：`format.notes` 依次 `--add` `note1`、`false`、`note2` 时只输出 `note2`（`false` 清空此前累积的 ref，#884 第 3b 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_notes_config` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC2 | `command::format_patch_test::format_notes_config` | 0 | stdout 不存在含夹具任一 note 正文的行 |
| AC3 | `command::format_patch_test::format_notes_config` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC4 | `command::format_patch_test::format_notes_config` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 黄金值中 note2 在 note1 之前） |
| AC5 | `command::format_patch_test::format_notes_config` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.notes` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_notes_config)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_notes_config`；证明 AC1–AC5）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-05`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.notes`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.notes配置; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-05; writeset=序列化于FPG-05; release=independent; split-from=FPG-05; exception=N/A`

---

### Task FPG-41: `format.notes` 与命令行 `--notes`/`--no-notes` 的交互（#884 第 3、4b 项；自 FPG-36 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-36` 拆出的独立轴：配置启用或指定 ref 时，命令行 `--no-notes` 关闭、`--notes` 强化或与配置合并，重复的 `--notes`/`--no-notes` 以最后出现者为准。 notes 夹具（`FPG-05`、`FPG-36`、`FPG-41` 共用）：同一提交在 `refs/notes/commits`（`default note`）、`refs/notes/note1`（`this is note 1`）、`refs/notes/note2`（`this is note 2`）上各有一条 note。

**Out of scope:** `format.notes` 取值解析（由 `FPG-36` 承接）；纯命令行顺序（由 `FPG-05` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `render_notes_block` 只处理单个 notes 来源，配置与命令行未合并 | `src/command/format_patch.rs:1012-1057` |

**Acceptance criteria:**

- [ ] AC1：`format.notes=true` 下 `--no-notes` 不输出 notes（#884 第 3 项）。
- [ ] AC2：`format.notes=note1` 下 `--no-notes` 不输出该具名 ref 的 notes（#884 第 4b 项）。
- [ ] AC3：`format.notes=true` 下 `--notes` 输出默认 notes（#884 第 3 项）。
- [ ] AC4：`format.notes=true` 下 `--notes --no-notes` 不输出 notes。
- [ ] AC5：`format.notes=true` 下 `--no-notes --notes` 输出默认 notes。
- [ ] AC6：`format.notes=note2` 下 `--notes=note1` 与配置合并（#884 第 4b 项）。
- [ ] AC7：`format.notes=note1` 下不带值的 `--notes` 合并默认 ref（#884 第 4b 项）。
- [ ] AC8：`format.notes=note1` 下 `--no-notes --notes=note2` 只输出 note2（先清除配置 ref 再启用另一 ref；t4014「format-patch with multiple notes refs」，#884 第 4 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_notes_cli_interaction` | 0 | stdout 不存在含夹具任一 note 正文的行 |
| AC2 | `command::format_patch_test::format_notes_cli_interaction` | 0 | stdout 不存在含夹具任一 note 正文的行 |
| AC3 | `command::format_patch_test::format_notes_cli_interaction` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC4 | `command::format_patch_test::format_notes_cli_interaction` | 0 | stdout 不存在含夹具任一 note 正文的行 |
| AC5 | `command::format_patch_test::format_notes_cli_interaction` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC6 | `command::format_patch_test::format_notes_cli_interaction` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 黄金值中 note2 在 note1 之前） |
| AC7 | `command::format_patch_test::format_notes_cli_interaction` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC8 | `command::format_patch_test::format_notes_cli_interaction` | 0 | notes 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值只含 note2 正文，不含 note1 正文） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.notes` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_notes_cli_interaction)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_notes_cli_interaction`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-36`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.notes`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.notes与命令行notes交互; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-36; writeset=序列化于FPG-36; release=independent; split-from=FPG-36; exception=N/A`

---

### Task FPG-54: 签名块尾随空行与 `--stdout` 邮件分隔对齐 Git（#885 第 6 项的字节形状；自 FPG-06 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-06` 拆出的独立轴（排在 `FPG-06` 之前执行，使其签名内容判据建立在正确布局之上）：按 Git `print_signature`（`git show 6de20f6092:builtin/log.c` 第 1222–1231 行）在签名文本后补换行（文本已以换行结尾则不补）并再输出一个空行；`--stdout` 按 Git 只在两封补丁邮件之间插入一个空行（同文件第 2520–2527 行 `shown_one` 注释；封面信之后、最后一封邮件之后均不插入，第 2454–2467 行）。现行 `push_signature` 只输出 `-- \n<sig>\n`（`src/command/format_patch.rs:1062-1071`），`write_patch_file` 在 `--stdout` 下于每封邮件之后追加 `\n`（同文件第 1804–1813 行）。夹具为差异隔离夹具（「验证执行协议」第 3 项）：文件 `f` 内容为 `1\n2\n3\n`，每个补丁只改第 2 行，diff 末行为上下文行 ` 3`。

**Out of scope:** 签名文本的取值与 `format.signature`（由 `FPG-06` 承接）；`format.signatureFile` 与空 `--signature`（由 `FPG-55` 承接）；`--attach` 的 MIME 终止分隔（由 `FPG-18` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `push_signature` 只输出 `-- \n<sig>\n`；`--stdout` 在每封邮件后追加 `\n`（含最后一封） | `src/command/format_patch.rs:1062-1071,1804-1813` |
| Git `print_signature` 输出 `-- \n<sig>`、按需补换行再加一个空行；`--stdout` 由 log-tree 在下一封邮件前插入空行 | `git show 6de20f6092:builtin/log.c` 第 1222–1231、2520–2537 行 |

**Acceptance criteria:**

- [ ] AC1：`-o d -1 --signature="my sig"` 的补丁文件以签名块加一个空行结尾。
- [ ] AC2：`-2 --stdout --signature="my sig"` 时第一封补丁邮件的签名块之后另有分隔空行。
- [ ] AC3：`-2 --stdout --signature="my sig"` 时最后一封邮件之后不再追加空行。
- [ ] AC4：`-2 --stdout --no-signature` 时两封邮件之间恰有一个空行。
- [ ] AC5：`-1 --stdout --no-signature` 时输出以 diff 末行结束。
- [ ] AC6：`--cover-letter -1 --stdout --signature="my sig"` 时封面信以签名块加一个空行结束，与其后首封补丁之间不再插入分隔空行（Git 只在补丁之间插入；t4014「format-patch --signature --cover-letter」）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::signature_block_layout_matches_git` | 0 | `d/` 中补丁文件自最后一个 `-- ` 行起至文件末尾逐字为 `-- \nmy sig\n\n` |
| AC2 | `command::format_patch_test::signature_block_layout_matches_git` | 0 | 第一封补丁邮件自其 `-- ` 行起至邮件结束逐字为 `-- \nmy sig\n\n\n` |
| AC3 | `command::format_patch_test::signature_block_layout_matches_git` | 0 | stdout 自最后一个 `-- ` 行起至末尾逐字为 `-- \nmy sig\n\n` |
| AC4 | `command::format_patch_test::signature_block_layout_matches_git` | 0 | 第一封补丁邮件（至邮件结束）的最后两行逐字为 ` 3` 与空行 |
| AC5 | `command::format_patch_test::signature_block_layout_matches_git` | 0 | stdout 的最后一行逐字为 ` 3`（其后无空行） |
| AC6 | `command::format_patch_test::signature_block_layout_matches_git` | 0 | 封面信自其 `-- ` 行起至邮件结束逐字为 `-- \nmy sig\n\n` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（签名块尾随空行与 `--stdout` 邮件分隔说明（无需改动时记录核对结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::signature_block_layout_matches_git)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::signature_block_layout_matches_git`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-41`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：签名块尾随空行与 `--stdout` 邮件分隔说明（无需改动时记录核对结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=签名块与stdout邮件分隔布局; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-41; writeset=序列化于FPG-41; release=independent; split-from=FPG-06; exception=N/A`

---

### Task FPG-06: 修复默认 signature 产品前缀并支持 `format.signature`（#885；自本卡拆出 FPG-54、FPG-55）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 默认 signature 输出 `libra <version>`（包含产品前缀），并在 `resolve_format_config` 中支持 `format.signature`（空字符串视为禁用，CLI `--signature`/`--signature-file`/`--no-signature` 覆盖配置）。 签名块尾随空行与 `--stdout` 邮件分隔由自本卡拆出、先于本卡执行的 `FPG-54` 承接；`format.signatureFile` 与空 `--signature` 由自本卡拆出的 `FPG-55` 承接。签名文件内容按 Git 原样使用（不剥离末尾换行，现行 `trim_end_matches('\n')` 须移除）。

**Out of scope:** 不修改 `am` 对 signature 的剥离规则。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `push_signature` 默认仅输出 `env!("CARGO_PKG_VERSION")`，且未读 `format.signature` | `src/command/format_patch.rs:1062-1071,1861-1902` |
| 既有 `signature_controls_patch_footer` 断言默认页脚，默认值改变后须同卡修订 | `tests/command/format_patch_test.rs:984` |
| Git `print_signature` 输出 `-- \n<sig>`、签名不以换行结尾时补换行、再输出一个空行；Libra `--stdout` 在每封邮件后追加 `\n` | `git show 6de20f6092:builtin/log.c` 第 1222–1231 行、`src/command/format_patch.rs:1062-1071,1804-1810` |

**Acceptance criteria:**

- [ ] AC1：默认签名页脚为 `-- ` 加同一待测二进制 `libra --version` 的输出（#885 第 1 项）。
- [ ] AC2：无配置时 `--signature="my sig"` 页脚为该值（#885 第 2 项）。
- [ ] AC3：`format.signature=Custom Sig` 时页脚为该值。
- [ ] AC4：`format.signature=""` 时不输出签名。
- [ ] AC5：`format.signature=Cfg` 下 `--signature=Cli` 页脚为 `Cli`。
- [ ] AC6：`format.signature=Cfg` 下 `--signature-file=<文件>`（文件内容为 `\nfile sig\n\n`，首行与末行为空行）页脚为文件原样内容。
- [ ] AC7：`format.signature=Cfg` 下 `--no-signature` 不输出签名。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_signature_config_and_prefix` | 0 | stdout 自最后一个 `-- ` 行起至末尾逐字等于 `-- \n`、同一二进制运行 `libra --version` 得到的输出行（含换行）与一个空行（t4014 `signature` helper 的 `printf "%s\n%s\n\n"` 形状） |
| AC2 | `command::format_patch_test::format_signature_config_and_prefix` | 0 | stdout 自最后一个 `-- ` 行起至末尾逐字为 `-- \nmy sig\n\n`（Git `print_signature` 在签名后另输出空行） |
| AC3 | `command::format_patch_test::format_signature_config_and_prefix` | 0 | stdout 自最后一个 `-- ` 行起至末尾逐字为 `-- \nCustom Sig\n\n` |
| AC4 | `command::format_patch_test::format_signature_config_and_prefix` | 0 | 必须不出现 `-- ` 签名分隔行 |
| AC5 | `command::format_patch_test::format_signature_config_and_prefix` | 0 | stdout 自最后一个 `-- ` 行起至末尾逐字为 `-- \nCli\n\n` |
| AC6 | `command::format_patch_test::format_signature_config_and_prefix` | 0 | stdout 自最后一个 `-- ` 行起至末尾逐字为 `-- \n\nfile sig\n\n\n`（Git 对已以换行结尾的签名不再补换行，再输出一个空行） |
| AC7 | `command::format_patch_test::format_signature_config_and_prefix` | 0 | 必须不出现 `-- ` 签名分隔行 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.signature` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_signature_config_and_prefix)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_signature_config_and_prefix`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-54`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.signature`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=signature前缀与format.signature; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-54; writeset=序列化于FPG-54; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-55: 支持 `format.signatureFile` 与空 `--signature`（#885 第 6 项其余 t4014 签名用例；自 FPG-06 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-06` 拆出的独立轴：读取 `format.signatureFile`（文件路径；`~` 等路径展开不在本卡范围，见 Out of scope），按 Git 优先级（`git show 6de20f6092:builtin/log.c` 第 2410–2433 行：`--signature`/`--no-signature` > `--signature-file` > `format.signature` > `format.signatureFile` > 默认版本串）选择签名；`--signature=""` 不输出签名块（`print_signature` 对空签名直接返回）；签名文件内容原样使用。`--signature` 与 `--signature-file` 同时给出的差异见 `DEFER-FPG-04`。夹具同 t4014「prepare mail-signature input」：签名文件 `mail-signature` 首行与末行为空行。

**Out of scope:** 默认签名文本与 `format.signature`（由 `FPG-06` 承接，本卡依赖其先行完成）；签名块布局（由 `FPG-54` 承接）；`format.signatureFile` 取值的 `~`/`%(prefix)` 路径展开（Git `git_config_pathname`；不在 #885 诉求内，尚未排期，重启条件：新 issue）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_format_config` 未读取 `format.signatureFile`；`--signature=""` 仍输出 `-- ` 与空行 | `src/command/format_patch.rs:1062-1071,1861-1902` |
| Git 签名优先级与 `format.signaturefile` 读取 | `git show 6de20f6092:builtin/log.c` 第 1050–1052、2410–2433 行 |

**Acceptance criteria:**

- [ ] AC1：`format.signatureFile=<mail-signature>`（无命令行签名选项）使用该文件内容（t4014「format.signaturefile works」）。
- [ ] AC2：`format.signatureFile` 下 `--no-signature` 不输出签名（t4014「--no-signature suppresses format.signaturefile」）。
- [ ] AC3：`format.signatureFile` 下 `--signature-file=<另一文件>` 以命令行文件为准（t4014「--signature-file overrides format.signaturefile」）。
- [ ] AC4：`format.signatureFile` 下 `--signature="my sig"` 以命令行值为准（t4014「--signature overrides format.signaturefile」）。
- [ ] AC5：`format.signature=Cfg` 与 `format.signatureFile` 同时配置时以 `format.signature` 为准。
- [ ] AC6：`--signature=""` 不输出签名块（t4014「format-patch --signature="" suppresses signatures」）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_signature_file_config` | 0 | stdout 自最后一个 `-- ` 行起至末尾与同夹具 Git 黄金值逐字一致 |
| AC2 | `command::format_patch_test::format_signature_file_config` | 0 | stdout 不存在逐字为 `-- ` 的行 |
| AC3 | `command::format_patch_test::format_signature_file_config` | 0 | stdout 自最后一个 `-- ` 行起至末尾与同夹具 Git 黄金值逐字一致 |
| AC4 | `command::format_patch_test::format_signature_file_config` | 0 | stdout 自最后一个 `-- ` 行起至末尾逐字为 `-- \nmy sig\n\n` |
| AC5 | `command::format_patch_test::format_signature_file_config` | 0 | stdout 自最后一个 `-- ` 行起至末尾逐字为 `-- \nCfg\n\n` |
| AC6 | `command::format_patch_test::format_signature_file_config` | 0 | stdout 不存在逐字为 `-- ` 的行 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.signatureFile` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_signature_file_config)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_signature_file_config`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-06`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.signatureFile`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.signatureFile与空--signature; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-06; writeset=序列化于FPG-06; release=independent; split-from=FPG-06; exception=N/A`

---

### Task FPG-07: 多行 Subject 首段拼接（#911；自本卡拆出 FPG-29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 Git 规则将提交消息首段（首个空行之前的所有非空行）以空格拼接为完整 Subject，正文从首个空行之后开始（自本卡拆出 `FPG-29` 处理长 `Subject:` 邮件头折叠）。 `Subject:`、封面信列表（默认 shortlog、`modern` 与自定义格式中的 `%s`，现行三处分别调用只取首行的 `commit_subject_line`，`src/command/format_patch.rs:1497-1500,1552,1565,1632`）使用拼接后的首段；文件名 slug 仍按 Git `%f` 只取首行（`git show 6de20f6092:pretty.c` 第 1765–1768 行），不得让 `patch_slug` 复用拼接后的标题。

**Out of scope:** 长 `Subject:` 邮件头折叠（由 `FPG-29` 承接）；非 ASCII RFC 2047 编码（由 `FPG-08` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_patch_body` 与 `commit_subject_line` 仅取 `lines().next()` | `src/command/format_patch.rs:833,1497-1500` |

**Acceptance criteria:**

- [ ] AC1：提交消息 `first line\nsecond line\n\nbody para` 的 `Subject:` 为首段各行以单空格拼接。
- [ ] AC2：同一提交的补丁正文从首个空行之后开始。
- [ ] AC3：该补丁经 `libra am` 应用后还原拼接后的标题与正文。
- [ ] AC4：该补丁经 `git am` 应用后还原拼接后的标题与正文。
- [ ] AC5：同一提交 `-o d -1` 的文件名 slug 只取首行（Git `%f`，t4014「multi-line subjects」读 `patches/0001-one.patch`）。
- [ ] AC6：同一提交 `--cover-letter -1 --stdout` 的默认 shortlog 封面信列表使用拼接后的首段。
- [ ] AC7：同一提交 `--cover-letter --commit-list-format=modern -1 --stdout` 的封面信列表使用拼接后的首段。
- [ ] AC8：同一提交 `--cover-letter --commit-list-format="%s" -1 --stdout` 的封面信列表中 `%s` 展开为拼接后的首段。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::multiline_subject_join` | 0 | `Subject:` 行逐字等于 `Subject: [PATCH] first line second line` |
| AC2 | `command::format_patch_test::multiline_subject_join` | 0 | 头区之后至 `---` 之前的正文区段去掉首尾空行后逐字等于 `body para` |
| AC3 | `command::format_patch_test::multiline_subject_join` | 0 | 接收仓库 `libra log -1 --format=%B` 的 stdout 逐字等于 `first line second line\n\nbody para\n`（不比较提交对象原文） |
| AC4 | `multiline_subject_roundtrips_with_git_am`（target `compat_format_patch_mail_roundtrip`） | 0 | Git 新提交 `git log -1 --format=%B` 的输出去掉末尾一个记录终止换行后逐字等于 `first line second line\n\nbody para\n` |
| AC5 | `command::format_patch_test::multiline_subject_join` | 0 | `d/` 文件名集合恰为 `0001-first-line.patch` |
| AC6 | `command::format_patch_test::multiline_subject_join` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值列表项为 `first line second line`） |
| AC7 | `command::format_patch_test::multiline_subject_join` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值为 `[1/1] first line second line`） |
| AC8 | `command::format_patch_test::multiline_subject_join` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值为 `first line second line`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（多行首段拼接为 Subject 的说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::multiline_subject_join)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::multiline_subject_join`；证明 AC1–AC3、AC5–AC8）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail -E 'test(=multiline_subject_roundtrips_with_git_am)'`（`(new)`；具名验收门，期望集合 N=1：`multiline_subject_roundtrips_with_git_am`（target `compat_format_patch_mail_roundtrip`）；证明 AC4）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-55`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：多行首段拼接为 Subject 的说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=多行Subject首段拼接; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-55; writeset=序列化于FPG-55; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-08: 默认启用非 ASCII 邮件头 RFC 2047 编码（#887；自本卡拆出 FPG-30）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 `ADR-FPG-01` 默认对非 ASCII 邮件头做 RFC 2047 Q 编码（空格按 Git 编为 `=20`），支持 `format.encodeEmailHeaders` 配置，`--encode-email-headers`/`--no-encode-email-headers` 覆盖配置（自本卡拆出 `FPG-30` 处理纯 ASCII 省略 MIME 头）。

**Out of scope:** 纯 ASCII 省略 MIME 头与 `#881` t4036 闭环（由 `FPG-30`、`FPG-51` 承接）；不修改 `mailinfo` 解码逻辑。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `encode_email_headers` 默认 `false` | `src/command/format_patch.rs:347-358,825,835` |
| 现有 `encode_email_header` 把空格编为 `_`，Git 期望 `=20`（#911）；既有 `encode_email_headers_*` 用例按行为迁移改写 | `src/command/format_patch.rs:1981-2018` |
| 既有 `encode_email_headers_q_encodes_nonascii_subject` 断言不带旗标时原样输出 `café`；默认编码后按行为迁移改为断言编码输出，保留带旗标时的断言 | `tests/command/format_patch_test.rs:1101-1144` |

**Acceptance criteria:**

- [ ] AC1：默认时标题 `Foö` 编码为 `=?UTF-8?q?Fo=C3=B6?=`。
- [ ] AC2：默认时非 ASCII 作者名（夹具作者 `Jöhn-Paul Q. Smith <jp@example.com>`）的 `From:` 按 Git 地址类规则编码。
- [ ] AC3：`--no-encode-email-headers` 保留原始 UTF-8。
- [ ] AC4：`format.encodeEmailHeaders=false` 保留原始 UTF-8。
- [ ] AC5：`format.encodeEmailHeaders=false` 下 `--encode-email-headers` 强制编码。
- [ ] AC6：多行首段 `en\ntvå\ntre` 默认编码为单个编码字（#911）。
- [ ] AC7：默认时含 ASCII 标点的非 ASCII 标题 `Foö: a-b. (x_y)` 只对 Git 主题类特殊字符编码（`git show 6de20f6092:pretty.c` 第 328–374 行 `is_rfc2047_special`）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::encode_email_headers_default_on` | 0 | `Subject:` 行逐字为 `Subject: [PATCH] =?UTF-8?q?Fo=C3=B6?=` |
| AC2 | `command::format_patch_test::encode_email_headers_default_on` | 0 | `From:` 行逐字为 `From: =?UTF-8?q?J=C3=B6hn-Paul=20Q=2E=20Smith?= <jp@example.com>`（与同夹具 Git 黄金值一致） |
| AC3 | `command::format_patch_test::encode_email_headers_default_on` | 0 | `Subject:` 行逐字为 `Subject: [PATCH] Foö` |
| AC4 | `command::format_patch_test::encode_email_headers_default_on` | 0 | `Subject:` 行逐字为 `Subject: [PATCH] Foö` |
| AC5 | `command::format_patch_test::encode_email_headers_default_on` | 0 | `Subject:` 行逐字为 `Subject: [PATCH] =?UTF-8?q?Fo=C3=B6?=` |
| AC6 | `command::format_patch_test::encode_email_headers_default_on` | 0 | `Subject:` 行逐字为 `Subject: [PATCH] =?UTF-8?q?en=20tv=C3=A5=20tre?=` |
| AC7 | `command::format_patch_test::encode_email_headers_default_on` | 0 | `Subject:` 行逐字为 `Subject: [PATCH] =?UTF-8?q?Fo=C3=B6:=20a-b.=20(x=5Fy)?=` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--no-encode-email-headers`、`format.encodeEmailHeaders` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::encode_email_headers_default_on)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::encode_email_headers_default_on`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-07`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--no-encode-email-headers`、`format.encodeEmailHeaders`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=默认RFC2047邮件头编码; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-07; writeset=序列化于FPG-07; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-29: 超长 `Subject:` 邮件头折叠（#886）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-07` 拆出的独立轴：对超过 78 显示列的 `Subject:` 头（按 Git `strbuf_add_wrapped_bytes` 以 `utf8_width` 计宽，`git show 6de20f6092:pretty.c` 第 2115–2131 行）按 RFC 2822 在词间折行（续行前导单空格），非 ASCII 标题在 RFC 2047 编码字之间折行（依赖 `FPG-08` 的 `=20` 编码）；短标题保持单行。

**Out of scope:** 多行提交首段拼接由 `FPG-07` 承接（本卡依赖其先行完成）；非 ASCII RFC 2047 编码由 `FPG-08` 处理。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `Subject:` 直接单行输出，无折行逻辑 | `src/command/format_patch.rs:863-869` |
| 既有 `encode_email_headers_splits_long_words_under_75_chars` 只读取 `Subject:` 首行；折行后编码字分布在续行，按行为迁移改为收集 `Subject:` 头及续行后检查编码字，保留每个编码字 ≤ 75 字符等仍适用断言 | `tests/command/format_patch_test.rs:1146-1180` |

**Acceptance criteria:**

- [ ] AC1：512 字符 ASCII 标题按 RFC 2822 在词间折行（t4014 长标题用例）。
- [ ] AC2：`--encode-email-headers` 下的长非 ASCII 标题在编码字之间折行，续行以单空格加 `=?UTF-8?q?` 开头（t4014 rfc2047 用例）。
- [ ] AC3：普通短标题仍输出单行 `Subject:`（#886 第 3 项）。
- [ ] AC4：t4014「multi-line subjects」夹具的多行首段拼接后仍输出单行 `Subject:`（#886 第 3 项）。
- [ ] AC5：折叠后的补丁经 `libra am` 应用后标题逐字还原。
- [ ] AC6：折叠后的补丁经 `git am` 应用后标题逐字还原。
- [ ] AC7：`--no-encode-email-headers` 下 t4014 rfc2047 用例的同一长非 ASCII 标题（`föö bar ` 重复 64 次）按显示列宽在词间折行。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::subject_header_folding` | 0 | `Subject:` 头（含续行）与同夹具 Git 黄金值逐字一致 |
| AC2 | `command::format_patch_test::subject_header_folding` | 0 | `Subject:` 头（含续行）与同夹具 Git 黄金值逐字一致 |
| AC3 | `command::format_patch_test::subject_header_folding` | 0 | `Subject:` 头（含续行）与同夹具 Git 黄金值逐字一致（该黄金值为单行完整标题，故同时判定内容与无续行） |
| AC4 | `command::format_patch_test::subject_header_folding` | 0 | `Subject:` 头（含续行）与同夹具 Git 黄金值逐字一致（该黄金值为单行完整标题，故同时判定内容与无续行） |
| AC5 | `command::format_patch_test::subject_header_folding` | 0 | 新提交标题与原标题逐字一致 |
| AC6 | `folded_subject_roundtrips_with_git_am`（target `compat_format_patch_mail_roundtrip`） | 0 | Git 新提交 `%s` 与原标题逐字一致 |
| AC7 | `command::format_patch_test::subject_header_folding` | 0 | `Subject:` 头（含续行）与同夹具 Git 黄金值逐字一致 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（长 `Subject:` 折叠说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::subject_header_folding)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::subject_header_folding`；证明 AC1–AC5、AC7）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail -E 'test(=folded_subject_roundtrips_with_git_am)'`（`(new)`；具名验收门，期望集合 N=1：`folded_subject_roundtrips_with_git_am`（target `compat_format_patch_mail_roundtrip`）；证明 AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-08`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：长 `Subject:` 折叠说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=长Subject邮件头折叠; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-08; writeset=序列化于FPG-08; release=independent; split-from=FPG-07; exception=N/A`

---

### Task FPG-30: 补丁邮件按需输出 MIME 头（#897, #881 t4036#2；自 FPG-08 拆出；自本卡拆出 FPG-42、FPG-51）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-08` 拆出的独立轴：补丁邮件按 Git 只在提交说明（含主题与签名行）或 in-body `From:` 含非 ASCII，或启用 `--attach`/`--inline` 时输出 `MIME-Version`/`Content-Type`/`Content-Transfer-Encoding`；仅作者名非 ASCII 时只对 `From:` 头做 RFC 2047 编码（`FPG-08`）而不输出 MIME 头——Git 判定 8bit 时跳过提交对象头区、只扫描提交说明（`git show 6de20f6092:pretty.c` 第 2326–2342 行），`pp_email_subject` 另检查 in-body 头（同文件第 2133–2141 行），#897 第 2 项把「author」列入触发条件与上游不符，本计划以上游为准并在关闭 #897 时说明。按 `git@6de20f6092` `t/t4036-format-patch-signer-mime.sh` #2 闭环 `#897` 第 1 项。封面信的 MIME 头由自本卡拆出的 `FPG-42` 承接；`#881` 的 t4036 #3–#5（签名者身份与 MIME）由自本卡拆出的 `FPG-51` 承接。

**Out of scope:** 封面信 MIME 头（由 `FPG-42` 承接）；t4036 #3–#5 与非 ASCII 签名行（由 `FPG-51` 承接）；非 ASCII RFC 2047 编码（由 `FPG-08` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_patch_body` 与 `format_cover_letter` 无条件输出 MIME 头 | `src/command/format_patch.rs:897-905,1415-1417` |
| 本机 Git 2.54 探针：非 ASCII 作者、ASCII 说明只编码 `From:`、无 MIME 头；非 ASCII 主题输出三行 MIME 头 | 2026-11-18 本机探针（记录于修订历史 R26 行） |
| 既有 `no_attach_stays_plain_text` 对 ASCII 提交断言输出 `Content-Type: text/plain; charset=UTF-8`，与 AC1 冲突；本卡按「验证执行协议」第 2 项的行为迁移把该断言改为不存在三行 MIME 头，保留其「非 multipart」断言 | `tests/command/format_patch_test.rs:480-493` |
| 既有 `recipient_headers_to_and_cc` 在纯 ASCII 提交上以 `Content-Transfer-Encoding:` 定位 MIME 块，与 AC1 冲突；按行为迁移改为在头区内定位 `To:`/`Cc:`，保留其折叠与不编码地址断言（开工时按 GC-01 核对 FPG-04/44/37/57 是否已改写） | `tests/command/format_patch_test.rs:1190-1224` |

**Acceptance criteria:**

- [ ] AC1：t4036#2：纯 ASCII 提交 `--stdout -1` 不输出 MIME 头（#897 第 1 项）。
- [ ] AC2：纯 ASCII 提交加 `--attach` 仍在邮件头区输出 multipart MIME 头（#897 第 2 项）。
- [ ] AC3：纯 ASCII 提交加 `--inline` 仍在邮件头区输出 multipart MIME 头。
- [ ] AC4：非 ASCII 作者名、纯 ASCII 提交说明的提交不输出 MIME 头（`From:` 的 RFC 2047 编码由 `FPG-08` AC2 验收；#897 第 2 项「author」的冲突说明见 Description）。
- [ ] AC5：非 ASCII 主题、纯 ASCII 正文的提交输出 MIME 头（#897 第 2 项）。
- [ ] AC6：提交说明正文（非标题）含非 ASCII 字节的提交输出 MIME 头。
- [ ] AC7：`--from="C O Mitter <committer@example.com>"` 改写发件人、原作者名含非 ASCII 而提交说明为纯 ASCII 时，非 ASCII 的 in-body `From:` 触发 MIME 头（Git `pp_email_subject` 检查 in-body 头）。
- [ ] AC8：提交说明（标题与正文）纯 ASCII、diff 新增行含 `café`、且该提交在默认 notes ref 上有 note `nöte` 并带 `--notes` 时不输出 MIME 头（Git 只扫描提交说明与 in-body 头，`git show 6de20f6092:pretty.c` 第 2325–2342 行）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::patch_mime_headers_on_demand` | 0 | stdout 不存在以 `MIME-Version:`、`Content-Type:` 或 `Content-Transfer-Encoding:` 开头的行（单一行级判定） |
| AC2 | `command::format_patch_test::patch_mime_headers_on_demand` | 0 | 头区中以 `MIME-Version:`、`Content-Type:` 开头的行组成的序列为 `MIME-Version: 1.0` 后接一行以 `Content-Type: multipart/mixed; boundary=` 开头的行 |
| AC3 | `command::format_patch_test::patch_mime_headers_on_demand` | 0 | 头区中以 `MIME-Version:`、`Content-Type:` 开头的行组成的序列为 `MIME-Version: 1.0` 后接一行以 `Content-Type: multipart/mixed; boundary=` 开头的行 |
| AC4 | `command::format_patch_test::patch_mime_headers_on_demand` | 0 | stdout 不存在以 `MIME-Version:`、`Content-Type:` 或 `Content-Transfer-Encoding:` 开头的行（单一行级判定）；本机 Git 2.54 探针同样无 MIME 头，开工以 `git@6de20f6092` 黄金值核实，不符即 STOP 并按 ER-03 修订本卡 |
| AC5 | `command::format_patch_test::patch_mime_headers_on_demand` | 0 | 头区中 `MIME-Version:`、`Content-Type:`、`Content-Transfer-Encoding:` 三行组成的序列与同夹具 Git 黄金值逐字一致 |
| AC6 | `command::format_patch_test::patch_mime_headers_on_demand` | 0 | 头区中 `MIME-Version:`、`Content-Type:`、`Content-Transfer-Encoding:` 三行组成的序列与同夹具 Git 黄金值逐字一致 |
| AC7 | `command::format_patch_test::patch_mime_headers_on_demand` | 0 | 头区中 `MIME-Version:`、`Content-Type:`、`Content-Transfer-Encoding:` 三行组成的序列与同夹具 Git 黄金值逐字一致 |
| AC8 | `command::format_patch_test::patch_mime_headers_on_demand` | 0 | stdout 不存在以 `MIME-Version:`、`Content-Type:` 或 `Content-Transfer-Encoding:` 开头的行（单一行级判定） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（纯 ASCII 省略 MIME 头说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::patch_mime_headers_on_demand)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::patch_mime_headers_on_demand`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-29`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：纯 ASCII 省略 MIME 头说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=补丁邮件按需MIME头; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-29; writeset=序列化于FPG-29; release=independent; split-from=FPG-08; exception=N/A`

---

### Task FPG-51: 闭环 `#881` t4036 #3–#5：签名者身份与 MIME 头（#881 第 3–5 项；自 FPG-30 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-30` 拆出的独立轴：在 `FPG-02` 核实的 `GIT_COMMITTER_*` 身份与 `FPG-30` 按需 MIME 头之上，按 `git@6de20f6092` `t/t4036-format-patch-signer-mime.sh` #3–#5 闭环 `#881`：ASCII 签名者不触发 MIME 头、非 ASCII 签名者触发 UTF-8 MIME 头且签名行以 UTF-8 原样写入正文、`--attach -s` 不重复 `MIME-Version:`。#881 第 3 项「signer name encoded」按 Git 解释：Git 不对正文中的签名行做 RFC 2047 编码，而以 `charset=UTF-8` 声明 8bit 正文。

**Out of scope:** 补丁邮件按需 MIME 头的通用判定（由 `FPG-30` 承接，本卡依赖其先行完成）；`GIT_COMMITTER_*` 身份解析（由 `FPG-02` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_patch_body` 无条件输出 MIME 头；签名行由 `resolve_signoff_identity` 解析的身份拼入正文 | `src/command/format_patch.rs:897-905,2050-2065` |
| t4036 #4 只断言存在 `Content-Type`，#5 断言 `MIME-Version:` 恰 1 行 | `git show 6de20f6092:t/t4036-format-patch-signer-mime.sh` |

**Acceptance criteria:**

- [ ] AC1：t4036#3：仅由 `GIT_COMMITTER_*` 提供 ASCII 身份时 `-s --stdout -1` 不输出 MIME 头。
- [ ] AC2：t4036#4：非 ASCII `GIT_COMMITTER_NAME` 签名时输出 UTF-8 MIME 头。
- [ ] AC3：t4036#4 场景（`GIT_COMMITTER_NAME="はまの ふにおう"`、`GIT_COMMITTER_EMAIL=committer@example.com`）的签名行以 UTF-8 原样写入正文（#881 第 3 项）。
- [ ] AC4：t4036#5：非 ASCII 签名者加 `--attach -s` 时 `MIME-Version:` 不重复。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::t4036_signer_mime_cases` | 0 | stdout 不存在以 `MIME-Version:`、`Content-Type:` 或 `Content-Transfer-Encoding:` 开头的行（单一行级判定） |
| AC2 | `command::format_patch_test::t4036_signer_mime_cases` | 0 | stdout 存在以 `Content-Type: text/plain; charset=UTF-8` 开头的行（t4036#4 判据） |
| AC3 | `command::format_patch_test::t4036_signer_mime_cases` | 0 | 补丁邮件正文中存在逐字为 `Signed-off-by: はまの ふにおう <committer@example.com>` 的行（头区中不得出现该行） |
| AC4 | `command::format_patch_test::t4036_signer_mime_cases` | 0 | 以 `MIME-Version:`（不区分大小写）开头的行恰为 1 行（t4036#5 判据） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（非 ASCII 签名者触发 MIME 头与 t4036 闭环说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::t4036_signer_mime_cases)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::t4036_signer_mime_cases`；证明 AC1–AC4）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-30`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：非 ASCII 签名者触发 MIME 头与 t4036 闭环说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=#881t4036签名者MIME闭环; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-30; writeset=序列化于FPG-30; release=independent; split-from=FPG-30; exception=N/A`

---

### Task FPG-42: 封面信按需输出 MIME 头（#897 封面信部分；自 FPG-30 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`format_cover_letter` 仅在系列任一提交的原始对象缓冲（headers+message）含非 ASCII 字节时输出 MIME 头（Git `make_cover_letter` 规则）；全 ASCII 系列不再输出 MIME 头。AC1/AC2 由 `format_patch_test::cover_letter_mime_on_demand` 覆盖（纯 ASCII 封面无 MIME；3 提交中仅第 2 提交 committer 为 `Cömmitter` 时输出 MIME）；`format_patch_test` 136/136、`compat_format_patch_mail_roundtrip` 14/14；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-30` 拆出的独立轴：`format_cover_letter` 不再无条件输出 MIME 头，仅当系列中任一提交的原始对象缓冲区（含 author/committer 头与提交说明）含非 ASCII 字节时输出（Git `make_cover_letter` 对 `repo_get_commit_buffer` 调用 `has_non_ascii`，`git show 6de20f6092:builtin/log.c` 第 1417–1425 行）；补丁邮件的判定仍按 `FPG-30`。

**Out of scope:** 补丁邮件 MIME 头（由 `FPG-30` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_cover_letter` 无条件输出 MIME 头 | `src/command/format_patch.rs:1415-1417` |

**Acceptance criteria:**

- [ ] AC1：纯 ASCII 系列的封面信不输出 MIME 头。
- [ ] AC2：3 提交系列中说明与作者均为 ASCII、仅第 2 个提交的提交者名为 `Cömmitter`，以 ASCII 身份运行 `--cover-letter -3 --stdout` 时封面信输出 MIME 头。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::cover_letter_mime_on_demand` | 0 | 封面信邮件不存在以 `MIME-Version:`、`Content-Type:` 或 `Content-Transfer-Encoding:` 开头的行（单一行级判定） |
| AC2 | `command::format_patch_test::cover_letter_mime_on_demand` | 0 | 封面信头区中 `MIME-Version:`、`Content-Type:`、`Content-Transfer-Encoding:` 三行组成的序列与同夹具 Git 黄金值逐字一致 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（封面信 MIME 头按需输出说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::cover_letter_mime_on_demand)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::cover_letter_mime_on_demand`；证明 AC1–AC2）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-51`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：封面信 MIME 头按需输出说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=封面信按需MIME头; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-51; writeset=序列化于FPG-51; release=independent; split-from=FPG-30; exception=N/A`

---

### Task FPG-09: 补齐 #888 单独选项成功路径并闭环 `#888`（#888 第 5 项；第 1–4 项由 `529 FP-03` 交付）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`529 FP-03` 已交付 `-k` 与 `--subject-prefix`/`--rfc` 的互斥拒绝；本卡补齐 #888 第 5 项的单独成功路径：`-k`、`--subject-prefix=X`、`--rfc` 各自成功且 `Subject:` 行恰为一行（`rename` / `[X] rename` / `[RFC PATCH] rename`），`--rfc= -k` 与 Git 一致（既有 `fp03_empty_rfc_with_k`）。AC1 由 `fp03_empty_rfc_with_k` 覆盖，AC2–AC4 由新增 `keep_subject_and_prefix_alone_succeed` 覆盖；`format_patch_test` 137/137。文档核对：EN/zh/开发 format-patch 文档与 `COMPATIBILITY.md` 已包含 `-k`/`--subject-prefix`/`--rfc` 互斥说明，本卡无新增标记。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** `#888` 第 1–4 项（`-k` 与 `--subject-prefix`/`--rfc` 互斥拒绝及诊断）由 `issues/529#FP-03` 的拒绝门族交付并在 529 计划内验收（其门族在本卡共享回归组中运行，不计本卡 AC）；本卡按 GC-01 核实 `--rfc= -k` 与 `git@6de20f6092` 一致，并为 #888 第 5 项补齐单独 `-k`、`--subject-prefix`、`--rfc` 的成功路径断言（既有用例在输出目录为空时也会通过，不足以证明）。

**Out of scope:** `-k` 互斥拒绝与诊断（由 `issues/529#FP-03` 交付与验收）；`format.subjectPrefix` 在 `-k` 下被忽略的既有行为（不变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `validate_cli_args` 已由 `529 FP-03` 处理 `-k` 互斥，已有 `fp03_empty_rfc_with_k` | `src/command/format_patch.rs:437-458`、`tests/command/format_patch_test.rs:2589` |
| `refusal_gates!` 宏为每组拒绝生成 `__msg`/`__code`/`__exit`/`__stdout`/`__fs`/`__json`/`__machine` 门，`assert_format_patch_refusal` 断言 exit 129 与 `LBR-CLI-002` | `tests/command/format_patch_test.rs:2435-2511` |

**Acceptance criteria:**

- [ ] AC1：`--rfc= -k -1 --stdout` 被接受且 `Subject:` 行与 `git@6de20f6092` 一致（空 `--rfc=` 不置 subject prefix，`git show 6de20f6092:builtin/log.c` 第 2151–2156、2213–2214 行）。
- [ ] AC2：单独 `-k -1 --stdout` 继续工作（#888 第 5 项）。
- [ ] AC3：单独 `--subject-prefix=X -1 --stdout` 继续工作（#888 第 5 项）。
- [ ] AC4：单独 `--rfc -1 --stdout` 继续工作（#888 第 5 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::fp03_empty_rfc_with_k` | 0 | `Subject:` 行逐字为 `Subject: rename` |
| AC2 | `command::format_patch_test::keep_subject_and_prefix_alone_succeed` | 0 | stdout 中以 `Subject:` 开头的行组成的序列恰为一行 `Subject: rename`（单一序列相等判定，同时排除多封邮件） |
| AC3 | `command::format_patch_test::keep_subject_and_prefix_alone_succeed` | 0 | stdout 中以 `Subject:` 开头的行组成的序列恰为一行 `Subject: [X] rename`（单一序列相等判定，同时排除多封邮件） |
| AC4 | `command::format_patch_test::keep_subject_and_prefix_alone_succeed` | 0 | stdout 中以 `Subject:` 开头的行组成的序列恰为一行 `Subject: [RFC PATCH] rename`（单一序列相等判定，同时排除多封邮件） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（`-k` 与 `--subject-prefix`/`--rfc` 互斥说明（无需改动时记录核对结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::fp03_empty_rfc_with_k)'`（既有；具名验收门，期望集合 N=1：`command::format_patch_test::fp03_empty_rfc_with_k`；证明 AC1）
- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::keep_subject_and_prefix_alone_succeed)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::keep_subject_and_prefix_alone_succeed`；证明 AC2–AC4）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-42`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：`-k` 与 `--subject-prefix`/`--rfc` 互斥说明（无需改动时记录核对结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=#888_-k互斥核验与关闭; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=1; scope=S; deps=FPG-42; writeset=序列化于FPG-42; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-10: `--from=<ident>` 合法性校验（#889；自本卡拆出 FPG-31）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--from=<ident>` 严格校验——必须含成对 `<`/`>`（`Name <>` 空邮箱合法），否则 `invalid ident line: <input>` + `LBR-CLI-002`；校验在 revision 解析与空系列判定之前，`--from=foo HEAD..HEAD` 同样失败；裸 `--from` 改用执行时（`GIT_COMMITTER_*` 感知）提交者身份。AC1–AC6 由 `format_patch_test::from_invalid_ident_rejected` 覆盖（非法 ident 及空范围、合法身份改写与 in-body 原作者、裸 `--from` 的 C O Mitter 身份、`Name <>`）；`format_patch_test` 140/140、`compat_format_patch_mail_roundtrip` 15/15；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 校验 `--from=<ident>` 必须能按 Git 拆分为 ident：`<` 与其后的 `>` 成对即合法（`Name <>` 的空邮箱合法），对不含 `<…>` 的字符串（如 `--from=foo`）报 `invalid ident line: foo` 并失败，而非输出 `From: foo <>`；校验在修订解析与空系列判定之前进行（`git show 6de20f6092:builtin/log.c` 第 2195–2198 行先于 `setup_revisions`；现行 `resolve_from_identity` 在 `src/command/format_patch.rs:540` 调用，晚于区间解析与空系列返回）（自本卡拆出 `FPG-31` 处理 `--no-from` 与 `format.from`）。

**Out of scope:** `--no-from` 与 `format.from` 配置（由 `FPG-31` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `parse_from_ident` 对无 `<>` 字符串返回 `(ident, "")` | `src/command/format_patch.rs:2022-2032` |

**Acceptance criteria:**

- [ ] AC1：`--from=foo` 被拒绝。
- [ ] AC2：作者与 `--from="Alice <alice@example.com>"` 不同时邮件头改写为该身份。
- [ ] AC3：同一场景正文首行保留原作者 in-body `From:`。
- [ ] AC4：裸 `--from`（不带值）使用执行时的提交者身份（#889 第 4 项）。
- [ ] AC5：空系列 `--from=foo --stdout HEAD..HEAD` 同样被拒绝（ident 校验先于修订解析与空系列判定）。
- [ ] AC6：`--from="Name <>" -1 --stdout` 被接受（Git 只要求 `<` 与其后的 `>` 成对，空邮箱合法）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::from_invalid_ident_rejected` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-002`，且 stderr 含 Libra 区分短语 `invalid ident line`；stdout 为空 |
| AC2 | `command::format_patch_test::from_invalid_ident_rejected` | 0 | 头区中以 `From:` 开头的行恰为一行且逐字为 `From: Alice <alice@example.com>`（夹具作者不同于该身份） |
| AC3 | `command::format_patch_test::from_invalid_ident_rejected` | 0 | 空行后的正文首行为 `From: <原作者名> <原作者邮箱>` |
| AC4 | `command::format_patch_test::from_invalid_ident_rejected` | 0 | 邮件头 `From:` 行逐字为 `From: C O Mitter <committer@example.com>`（夹具中作者、执行 format-patch 时的提交者身份（经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` 设为 `C O Mitter <committer@example.com>`，且不同于创建提交时的提交者）与 `format.from` 三者互不相同） |
| AC5 | `command::format_patch_test::from_invalid_ident_rejected` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-002`，且 stderr 含 Libra 区分短语 `invalid ident line`；stdout 为空 |
| AC6 | `command::format_patch_test::from_invalid_ident_rejected` | 0 | 邮件头 `From:` 行逐字为 `From: Name <>` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（`--from=<ident>` 合法性说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::from_invalid_ident_rejected)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::from_invalid_ident_rejected`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-09`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：`--from=<ident>` 合法性说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--from=<ident>合法性校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-09; writeset=序列化于FPG-09; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-31: 支持 `format.from` 配置取值（#915 第 2 项；自 FPG-10 拆出；自本卡拆出 FPG-46）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`resolve_format_config` 读取 `format.from`——`true` 使用执行时提交者身份，`false`/空保持作者，其它值作为 ident 行；配置值仅在没有命令行 `--from` 时生效，非法配置值在解析后按 `FPG-10` 的 ident 校验以 `invalid ident line` + `LBR-CLI-002` 拒绝。AC1–AC4 由 `format_patch_test::format_from_and_no_from` 覆盖（true/false 与 Git 黄金 `From:` 逐字、ident 原样、非法值稳定码）；`format_patch_test` 141/141、`compat_format_patch_mail_roundtrip` 15/15；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；`--no-from` 与命令行覆盖由 `FPG-46` 承接，本卡未新增该 flag。

**Description:** 自 `FPG-10` 拆出的独立轴：读取 `format.from`（`true` 改写为提交者身份、`false` 不改写、`Name <email>` 使用该身份），非法取值不在读取时校验，而在命令行覆盖之后按 `FPG-10` 的 ident 校验以 `LBR-CLI-002` 拒绝（被 `--from`/`--no-from` 覆盖时不报错，`git show 6de20f6092:builtin/log.c` 第 1078–1086、2195–2198 行；覆盖场景由 `FPG-46` 验收）。`--no-from` 与命令行覆盖由自本卡拆出的 `FPG-46` 承接。

**Out of scope:** `--from=<ident>` 格式校验（由 `FPG-10` 承接，本卡依赖其先行完成）；`--from`/`--no-from` 与配置的命令行覆盖（由 `FPG-46` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FormatPatchArgs` 缺少 `--no-from`；`resolve_format_config` 未读取 `format.from` | `src/command/format_patch.rs:212-217,1861-1902` |

**Acceptance criteria:**

- [ ] AC1：`format.from=true` 时改写为提交者身份。
- [ ] AC2：`format.from=false` 时不改写。
- [ ] AC3：只设 `format.from="F R Om <from@example.com>"` 时使用该身份（#915 第 2 项）。
- [ ] AC4：只设 `format.from=foo`（非布尔且非合法 ident，无命令行 `--from`/`--no-from`）时以与 `--from=foo` 相同的稳定错误码拒绝。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_from_and_no_from` | 0 | 邮件头 `From:` 行与同夹具 Git 黄金值逐字一致（夹具中作者、执行 format-patch 时的提交者身份（经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` 设为 `C O Mitter <committer@example.com>`，且不同于创建提交时的提交者）与 `format.from` 三者互不相同） |
| AC2 | `command::format_patch_test::format_from_and_no_from` | 0 | 邮件头 `From:` 行与同夹具 Git 黄金值逐字一致（夹具中作者、执行 format-patch 时的提交者身份（经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` 设为 `C O Mitter <committer@example.com>`，且不同于创建提交时的提交者）与 `format.from` 三者互不相同） |
| AC3 | `command::format_patch_test::format_from_and_no_from` | 0 | 邮件头 `From:` 行逐字为 `From: F R Om <from@example.com>` |
| AC4 | `command::format_patch_test::format_from_and_no_from` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-002`，且 stderr 含 Libra 区分短语 `invalid ident line`；stdout 为空 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.from` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_from_and_no_from)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_from_and_no_from`；证明 AC1–AC4）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-10`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.from`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.from配置取值; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-10; writeset=序列化于FPG-10; release=independent; split-from=FPG-10; exception=N/A`

---

### Task FPG-46: 支持 `--no-from` 与 `--from` 对 `format.from` 的命令行覆盖（#915 第 1、3 项；自 FPG-31 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：新增 `--no-from`，与 `--from` 通过 clap `overrides_with` 双向最后者胜出；`--no-from` 使配置解析跳过 `format.from`，并使非法配置 ident 在命令行覆盖时不校验。AC1–AC7 由 `format_patch_test::from_and_no_from_override_config` 覆盖（`--no-from` 与 Git 黄金一致、显式 `--from` 覆盖、裸 `--from` 提交者身份、双向 last-wins、非法 `format.from` 被 `--no-from`/`--from` 覆盖后成功）；`format_patch_test` 142/142、`compat_format_patch_mail_roundtrip` 15/15；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-31` 拆出的独立轴：新增 `--no-from`，`--from[=<ident>]`/`--no-from` 按最后出现者生效并覆盖 `format.from`；裸 `--from` 始终使用提交者身份。 #915 Expected behavior 第 1 项与验收标准第 1 条称 `--no-from` 后 `From:` 为提交者身份；Git `from_callback` 在 `--no-from` 时清空发件人覆盖，`From:` 回到作者身份（`git show 6de20f6092:builtin/log.c` 第 1664–1677 行），t4014「--no-from overrides format.from」只断言不再是 `format.from`。本计划以上游为准（夹具中作者与提交者身份不同以区分），并在关闭 #915 时于 issue 中说明。

**Out of scope:** `format.from` 取值解析（由 `FPG-31` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FormatPatchArgs` 无 `--no-from`；`resolve_from_identity` 未考虑 `format.from` | `src/command/format_patch.rs:212-217,2037-2048` |

**Acceptance criteria:**

- [ ] AC1：`format.from="F R Om <from@example.com>"` 下 `--no-from` 不改写（#915 第 1 项）。
- [ ] AC2：`format.from="F R Om <from@example.com>"` 下 `--from="Cli <cli@example.com>"` 覆盖配置（#915 第 3 项）。
- [ ] AC3：`format.from="F R Om <from@example.com>"` 下裸 `--from` 使用提交者身份（#915 第 3 项）。
- [ ] AC4：`--no-from --from="Cli <cli@example.com>"` 以最后出现者为准。
- [ ] AC5：`--from="Cli <cli@example.com>" --no-from` 以最后出现者为准。
- [ ] AC6：`format.from=foo`（非法 ident）下 `--no-from -1 --stdout` 成功（命令行覆盖后不再校验配置值）。
- [ ] AC7：`format.from=foo` 下 `--from="Cli <cli@example.com>" -1 --stdout` 成功。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::from_and_no_from_override_config` | 0 | 邮件头 `From:` 行与同夹具 Git 黄金值逐字一致（夹具中作者、执行 format-patch 时的提交者身份（经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` 设为 `C O Mitter <committer@example.com>`，且不同于创建提交时的提交者）与 `format.from` 三者互不相同） |
| AC2 | `command::format_patch_test::from_and_no_from_override_config` | 0 | 邮件头 `From:` 行逐字为 `From: Cli <cli@example.com>` |
| AC3 | `command::format_patch_test::from_and_no_from_override_config` | 0 | 邮件头 `From:` 行与同夹具 Git 黄金值逐字一致（夹具中作者、执行 format-patch 时的提交者身份（经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` 设为 `C O Mitter <committer@example.com>`，且不同于创建提交时的提交者）与 `format.from` 三者互不相同） |
| AC4 | `command::format_patch_test::from_and_no_from_override_config` | 0 | 邮件头 `From:` 行逐字为 `From: Cli <cli@example.com>` |
| AC5 | `command::format_patch_test::from_and_no_from_override_config` | 0 | 邮件头 `From:` 行与同夹具 Git 黄金值逐字一致（夹具中作者、执行 format-patch 时的提交者身份（经 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` 设为 `C O Mitter <committer@example.com>`，且不同于创建提交时的提交者）与 `format.from` 三者互不相同） |
| AC6 | `command::format_patch_test::from_and_no_from_override_config` | 0 | 邮件头 `From:` 行与同夹具 Git 黄金值逐字一致（作者身份） |
| AC7 | `command::format_patch_test::from_and_no_from_override_config` | 0 | 邮件头 `From:` 行逐字为 `From: Cli <cli@example.com>` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--no-from` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::from_and_no_from_override_config)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::from_and_no_from_override_config`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-31`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--no-from`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--from/--no-from覆盖format.from; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-31; writeset=序列化于FPG-31; release=independent; split-from=FPG-31; exception=N/A`

---

### Task FPG-11: 实现 `--relative` 与 `diff.relative` 路径过滤与相对化（#890；自本卡拆出 FPG-48）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：实现 `--relative[=<path>]`、`diff.relative` 与 `--no-relative`（最后者胜出）；`<path>` 相对仓库根解析，裸 `--relative` 用当前目录；父提交经共享 diff 引擎（`run_diff_for_service` 新增 relative 剥离），根提交经 `CommitDiffOptions.relative` 过滤/重写；同时修正 diffstat 与 diff 之间、以及 `--stdout` 末补丁后的多余换行，与 Git 逐字一致。AC1–AC6 由 `format_patch_test::diff_relative_filters_paths` 覆盖（AC1/AC4/AC5 与 Git 黄金逐字，AC2/AC3/AC6 与 AC1 逐字节）；`format_patch_test` 143/143、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步，`--relative` 不再列为 unsupported。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 取消 `--relative` 的 `Unsupported` 拒绝，支持 `--relative[=<path>]`、`--no-relative` 与 `diff.relative` 配置：无值时以当前工作目录为前缀，`<path>` 按 Git 以仓库根为基准（不随当前目录解析，与 `libra diff --relative` 现行按当前目录解析不同），过滤变更并输出相对路径的 diffstat 与 diff。夹具（#890 最小复现改写为差异隔离夹具）：base 提交含 `sub/` 外的 `top/dir/file` 与 `sub/dir/file`（两路径等长，满足差异隔离；#890 原复现的根目录 `file` 改名以免 diffstat 对齐差异）；被测提交同时修改两者（各含新增与删除行）；各 AC 命令均为 `format-patch --stdout -1 --no-thread --no-signature`。

**Out of scope:** 仓库外与不存在前缀的 Git 一致性与安全判据（由 `FPG-48` 承接）；`libra diff` 自身的 `--relative`（不变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `execute_safe` 遇到 `args.relative.is_some()` 返回 `Unsupported`；`CommitDiffOptions` 无 relative 字段 | `src/command/format_patch.rs:500-506,1852-1859` |
| 既有 `unsupported_git_options_are_classified` 断言 `--relative=sub` 以 `LBR-UNSUPPORTED-001` 拒绝；本卡须删除该分支并保留 `--all` 拒绝；`--relative` 现为 `hide = true`，本卡改为可见并加入 flag 面必列清单 | `tests/command/format_patch_test.rs:2351-2383`、`src/command/format_patch.rs:285-290` |

**Acceptance criteria:**

- [ ] AC1：在 `sub/` 中且 `diff.relative=true`（无 flag）时只含 `sub/` 内文件且路径相对 `sub/`（#890）。
- [ ] AC2：在 `sub/` 中运行 `--relative`（无配置）同上。
- [ ] AC3：在仓库根运行 `--relative=sub` 同上。
- [ ] AC4：在 `sub/` 中且未设 `diff.relative` 时保持仓库相对路径。
- [ ] AC5：在 `sub/` 中且 `diff.relative=true` 时 `--no-relative` 保持仓库相对路径（含 `sub/` 外的文件）。
- [ ] AC6：在 `sub/` 中运行 `--relative=sub` 同 AC1（`<path>` 相对仓库根，而非当前目录）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::diff_relative_filters_paths` | 0 | 变更段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC2 | `command::format_patch_test::diff_relative_filters_paths` | 0 | 变更段与 AC1 的 Libra 输出逐字节相同 |
| AC3 | `command::format_patch_test::diff_relative_filters_paths` | 0 | 变更段与 AC1 的 Libra 输出逐字节相同 |
| AC4 | `command::format_patch_test::diff_relative_filters_paths` | 0 | 变更段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC5 | `command::format_patch_test::diff_relative_filters_paths` | 0 | 变更段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC6 | `command::format_patch_test::diff_relative_filters_paths` | 0 | 变更段与 AC1 的 Libra 输出逐字节相同 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--relative`、`diff.relative` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::diff_relative_filters_paths)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::diff_relative_filters_paths`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）
- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::(log|show|stash)_test::/)'`（`log::CommitDiffOptions`/`generate_diff` 的 `log`、`show`、`stash` 调用方；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-46`, `DEP-FPG-03`（核实 `src/command/log.rs` 无在途写者）

**Implementation write set:** `src/command/format_patch.rs`, `src/command/log.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `src/command/log.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--relative`、`diff.relative`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--relative与diff.relative; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=3; prod-files=2; scope=M; deps=FPG-46,DEP-FPG-03; writeset=序列化于FPG-46; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-48: `--relative` 仓库外与不存在前缀的 Git 一致性与安全（#890 边界；自 FPG-11 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--relative` 前缀缺失或逃逸仓库时输出空补丁正文（无 `---`/diffstat/diff），逃逸前缀以内部哨兵拒绝调用 diff 引擎，不写任何文件且不打印仓库外路径；`---` 分隔符改为仅在确有 notes/stat/diff 且非 `--numstat` 时输出。AC1–AC4 由 `format_patch_test::relative_outside_and_missing_prefix` 覆盖（AC1/AC2 与 Git 空正文一致、AC3 仓库外哨兵文件内容与 mtime 不变、AC4 stdout/stderr 均无哨兵绝对路径）；`format_patch_test` 144/144、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-11` 拆出的独立轴：`--relative=<prefix>` 的前缀在仓库外或不存在时按 Git 行为输出（本机 Git 2.54 探针均 exit 0 且不输出 diff 段，开工时以 `git@6de20f6092` 重新生成黄金值为准），并保证不写入仓库外文件、不在输出中暴露仓库外绝对路径（GC-07）。

**Out of scope:** `--relative`/`diff.relative` 的正常过滤与相对化（由 `FPG-11` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `execute_safe` 现对 `--relative` 返回 `Unsupported` | `src/command/format_patch.rs:500-506` |
| 本机 Git 2.54 探针：`--relative=nonexistent` 与 `--relative=../outside` 均 exit 0；补丁只有头区与提交说明，不含 `---` 行与 diff 段（开工时以 `git@6de20f6092` 重新生成黄金值为准） | 2026-11-18 本机探针（记录于修订历史 R17、R25 行） |

**Acceptance criteria:**

- [ ] AC1：`--relative=../outside`（前缀在仓库外）与 Git 一致。
- [ ] AC2：`--relative=nonexistent` 与 Git 一致。
- [ ] AC3：`--relative=../outside` 时不写入仓库外文件。
- [ ] AC4：`--relative=../outside` 时 stdout 与 stderr 不暴露仓库外绝对路径。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::relative_outside_and_missing_prefix` | 0（生成黄金值时须核实 Git 同为 0，否则 STOP 并按 ER-03 修订本卡） | 补丁邮件正文与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（区段按「验证执行协议」第 3 项「邮件正文」；Git 此时不输出 `---` 行，Libra 多出 `---`、diffstat 或 diff 即不等） |
| AC2 | `command::format_patch_test::relative_outside_and_missing_prefix` | 0（同上核实） | 补丁邮件正文与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（区段按「验证执行协议」第 3 项「邮件正文」；Git 此时不输出 `---` 行，Libra 多出 `---`、diffstat 或 diff 即不等） |
| AC3 | `command::format_patch_test::relative_outside_and_missing_prefix` | 0 | 仓库外哨兵目录在执行前后的快照（文件名、内容、mtime）相同 |
| AC4 | `command::format_patch_test::relative_outside_and_missing_prefix` | 0 | stdout 与 stderr 均不含哨兵目录的绝对路径字符串 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（`--relative` 仓库外与不存在前缀行为说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::relative_outside_and_missing_prefix)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::relative_outside_and_missing_prefix`；证明 AC1–AC4）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-11`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：`--relative` 仓库外与不存在前缀行为说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--relative仓库外与不存在前缀; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-11; writeset=序列化于FPG-11; release=independent; split-from=FPG-11; exception=N/A`

---

### Task FPG-12: 支持 `format.coverLetter` 自动触发与 `#909` 核验（#900, #909；自本卡拆出 FPG-24）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：读取 `format.coverLetter`（`true`/`false`/`auto`；`auto` 在 `total > 1` 生成），新增 `--no-cover-letter` 并与 `--cover-letter` 双向 last-wins、命令行覆盖配置；`--commit-list-format` 隐含封面信但可被 `--no-cover-letter` 抑制；`shortlog` 识别为 Git 默认作者分组列表。AC1–AC8 由 `format_patch_test::format_cover_letter_auto_config` 覆盖（true/auto 双补丁、auto 单补丁、--no-cover-letter、modern 隐含 `[1/1]`、shortlog 列表段与 Git 黄金逐字、--no-cover-letter 抑制隐含、false+--cover-letter）；`format_patch_test` 145/145、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；封面信 diffstat 仍由 `FPG-13` 承接。

**Description:** 读取 `format.coverLetter`（`true`/`false`/`auto`，其中 `auto` 在 `total > 1` 时生成封面信），CLI `--cover-letter`/`--no-cover-letter` 覆盖配置；并按 GC-01 核实 `529 FP-13` 合入的 `--commit-list-format` 隐含启用封面信行为（自本卡拆出 `FPG-24` 处理 `branch.<name>.description`，其后 `FPG-24` 再拆出 `FPG-60` 处理封面信默认 `From:`）。

**Out of scope:** `branch.<name>.description` 与封面信默认 `From:`（分别由 `FPG-24`、`FPG-60` 承接）；封面信 diffstat（由 `FPG-13` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `execute_safe` 未读 `format.coverLetter`；`--commit-list-format` 隐含封面由 `529 FP-13` 承接 | `src/command/format_patch.rs:178-180,549-564,1861-1902`、`issues/529.md:168` |

**Acceptance criteria:**

- [ ] AC1：`format.coverLetter=true` 时 `-o d HEAD~2` 写出封面信。
- [ ] AC2：`format.coverLetter=auto` 时两补丁写出封面信。
- [ ] AC3：`format.coverLetter=auto` 时单补丁不写封面信。
- [ ] AC4：`format.coverLetter=true` 下 `--no-cover-letter` 不写封面信。
- [ ] AC5：`--commit-list-format=modern -o p HEAD~1` 隐含封面信（#909）。
- [ ] AC6：`--commit-list-format=shortlog -o p HEAD~1` 隐含封面信（#909，核实 `529 FP-13`）。
- [ ] AC7：`--commit-list-format=modern --no-cover-letter -o p HEAD~1` 不写封面信（#909）。
- [ ] AC8：`format.coverLetter=false` 下 `--cover-letter -o d HEAD~2` 写出封面信（命令行覆盖配置，#900 第 3 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_cover_letter_auto_config` | 0 | `d/` 文件名集合恰为 `0000-cover-letter.patch` 与两个补丁文件 |
| AC2 | `command::format_patch_test::format_cover_letter_auto_config` | 0 | 输出目录文件名集合含 `0000-cover-letter.patch` |
| AC3 | `command::format_patch_test::format_cover_letter_auto_config` | 0 | 输出目录文件名集合恰为 1 个补丁文件 |
| AC4 | `command::format_patch_test::format_cover_letter_auto_config` | 0 | 输出目录文件名集合不含 `0000-cover-letter.patch` |
| AC5 | `command::format_patch_test::format_cover_letter_auto_config` | 0 | `p/0000-cover-letter.patch` 中存在逐字为 `[1/1] <标题>` 的行 |
| AC6 | `command::format_patch_test::format_cover_letter_auto_config` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC7 | `command::format_patch_test::format_cover_letter_auto_config` | 0 | `p/` 文件名集合恰为 1 个补丁文件 |
| AC8 | `command::format_patch_test::format_cover_letter_auto_config` | 0 | `d/` 文件名集合恰为 `0000-cover-letter.patch` 与两个补丁文件 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.coverLetter` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_cover_letter_auto_config)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_cover_letter_auto_config`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-48`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.coverLetter`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.coverLetter与#909核验; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-48; writeset=序列化于FPG-48; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-58: 封面信正文模板对齐 Git（`*** SUBJECT HERE ***` 只在 `Subject:` 头；#905 封面信正文前置；自 FPG-13 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：封面信正文对齐 Git `prepare_cover_text`——标题占位符只写入 `Subject:` 头，正文以 `*** BLURB HERE ***` 开头。AC1/AC2 由 `format_patch_test::cover_letter_body_template_matches_git` 覆盖（正文以 BLURB 开头且不含标题占位符；Subject 行逐字 `[PATCH 0/2] *** SUBJECT HERE ***`）；`format_patch_test` 146/146、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；开发文档已记录核对结论（EN/zh 文档与 `COMPATIBILITY.md` 本卡无新增标记，核对无改动）。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；封面信 diffstat 仍由 `FPG-13` 承接。

**Description:** 自 `FPG-13` 拆出的独立轴（先于 `FPG-13` 执行）：把封面信正文模板改为 Git `prepare_cover_text` 的形式——标题占位符 `*** SUBJECT HERE ***` 只写入 `Subject:` 头，正文以 `*** BLURB HERE ***` 开头（`git show 6de20f6092:builtin/log.c` 第 1288–1323 行）；现行 `format_cover_letter` 在正文先多写一行 `*** SUBJECT HERE ***`（`src/command/format_patch.rs:1422-1423`），使之后各卡的封面信正文、描述段黄金比较不受该差异影响。

**Out of scope:** 封面信系列 diffstat（由 `FPG-13` 承接）；分支描述与 `--cover-from-description`（由 `FPG-24`、`FPG-25`、`FPG-33` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_cover_letter` 正文先写 `*** SUBJECT HERE ***` 再写 `*** BLURB HERE ***`；Git 正文只有 `*** BLURB HERE ***` | `src/command/format_patch.rs:1422-1423`、`git show 6de20f6092:builtin/log.c` 第 1296–1297、1321–1322 行 |

**Acceptance criteria:**

- [ ] AC1：无分支描述时封面信正文以 `*** BLURB HERE ***` 开头（Git 不在正文重复标题占位符）。
- [ ] AC2：无分支描述时封面信 `Subject:` 头保留标题占位符。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::cover_letter_body_template_matches_git` | 0 | `--cover-letter -2 --stdout` 的封面信描述段逐字为 `*** BLURB HERE ***` |
| AC2 | `command::format_patch_test::cover_letter_body_template_matches_git` | 0 | `--cover-letter -2 --stdout` 的封面信 `Subject:` 行逐字为 `Subject: [PATCH 0/2] *** SUBJECT HERE ***` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（封面信正文模板说明（无需改动时记录核对结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::cover_letter_body_template_matches_git)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::cover_letter_body_template_matches_git`；证明 AC1–AC2）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-12`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：封面信正文模板说明（无需改动时记录核对结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=封面信正文模板; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-12; writeset=序列化于FPG-12; release=independent; split-from=FPG-13; exception=N/A`

---

### Task FPG-13: 封面信输出系列 diffstat 汇总（#905；自本卡拆出 FPG-25、FPG-58）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：封面信在 commit list 后输出系列 diffstat——由唯一边界提交（最旧补丁父提交）到末个补丁，经共享 `libra diff --stat` 渲染；`--no-stat`/`-p` 仅影响补丁；无唯一边界（root 系列）省略，`--root` 与显式有界范围同用仍输出；diffstat 与签名/`--base` 间补空行。AC1–AC7 由 `format_patch_test::cover_letter_includes_diffstat` 覆盖（默认/`--no-stat`/`-p`/root/签名/root+范围与 Git 黄金逐字，重命名系列 stat 与 `libra diff --stat` 逐字节）；`format_patch_test` 147/147、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；`--summary` 行与版式仍按 `DEFER-FPG-03`。

**Description:** 在封面信 commit list 之后输出整套系列的汇总 diffstat：Git 只在存在唯一边界提交（origin）时输出，取 origin 的树到末个提交的树（`git show 6de20f6092:builtin/log.c` 第 1263–1279、1455–1457、2341–2346 行）；系列含根提交（如 `--root <rev>` 自根提交起格式化）或边界不唯一时不输出（`--root` 本身只置 `show_root_diff`，与有唯一边界的显式范围同用时仍输出，`git show 6de20f6092:revision.c` 第 2671–2672 行）；`--no-stat`/`-p` 只影响补丁，封面信仍输出（#905 Expected behavior 第 3 项与验收标准第 2 项称二者抑制 stat 块，与上游不符，本计划以上游为准并在关闭 #905 时于 issue 中说明）。diffstat 由 529 FP-05 起的共享 stat 渲染器产生并继承补丁的 diff 选项（重命名选项的继承由 `FPG-39` AC7/AC8 验收，#905 第 3 项），其版式与 `--summary` 行随 529 `DEFER-FP-02`（`DEFER-FPG-03`）。封面信正文模板（`*** SUBJECT HERE ***` 只在 `Subject:` 头）由自本卡拆出、先于本卡执行的 `FPG-58` 承接，本卡的封面信正文黄金比较以其为前提。AC1、AC3、AC4、AC6、AC7 的夹具为差异隔离夹具：至少 3 个提交，单文件 `f`，补丁 1 把第 2 行由 `a` 改为 `b`、补丁 2 再改为 `c`（系列 diffstat `f | 2 +-` 不同于逐补丁统计之和），命令为 `--cover-letter -2 --stdout`。（自本卡拆出 `FPG-25` 处理 `--cover-from-description`、`FPG-58` 处理正文模板。）

**Out of scope:** `--cover-from-description`（由 `FPG-25` 承接）；`--commit-list-format` 占位符扩展（由 `FPG-14` 承接）；封面信 diffstat 继承重命名选项的验收（由 `FPG-39` 承接）；`--summary` 行与版式对齐 Git（`DEFER-FPG-03`）；封面信正文模板（由 `FPG-58` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_cover_letter` 未计算系列 diffstat | `src/command/format_patch.rs:1428-1444` |

**Acceptance criteria:**

- [ ] AC1：默认两补丁封面信在列表段之后输出整系列 diffstat（系列树差，而非逐补丁统计之和）。
- [ ] AC2：重命名系列（单提交 `mv file foo`，重命名检测按 `529 FP-05`/`FP-06` 默认开启）`--cover-letter -1 --stdout` 的封面信 diffstat 由共享 stat 渲染器对 origin 与末个提交求得（#905 第 1 项的统计行与汇总行）。
- [ ] AC3：`--no-stat` 时封面信仍输出系列 diffstat（Git 只影响补丁）。
- [ ] AC4：`-p` 时封面信仍输出系列 diffstat。
- [ ] AC5：`--cover-letter --root HEAD --stdout`（系列自根提交起，无边界提交）的封面信不输出 diffstat。
- [ ] AC6：`--signature=SIG` 时封面信 diffstat 位于签名之前（#905 Expected behavior 第 2 项）。
- [ ] AC7：`--cover-letter --root HEAD~2..HEAD --stdout`（`--root` 与有唯一边界的显式范围同用）仍输出系列 diffstat。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::cover_letter_includes_diffstat` | 0 | 封面信正文与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC2 | `command::format_patch_test::cover_letter_includes_diffstat` | 0 | 封面信 diffstat 段逐字节等于同一二进制 `libra diff --stat <origin> <末个提交>` 的 stdout（Git 的 `rename file => foo (100%)` 摘要行见 `DEFER-FPG-03`） |
| AC3 | `command::format_patch_test::cover_letter_includes_diffstat` | 0 | 封面信正文与同夹具 Git `--no-stat` 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC4 | `command::format_patch_test::cover_letter_includes_diffstat` | 0 | 封面信正文与同夹具 Git `-p` 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC5 | `command::format_patch_test::cover_letter_includes_diffstat` | 0 | 封面信正文与同夹具 Git `--root` 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 黄金值在列表段之后直接到邮件结束） |
| AC6 | `command::format_patch_test::cover_letter_includes_diffstat` | 0 | 封面信正文与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --signature=SIG`） |
| AC7 | `command::format_patch_test::cover_letter_includes_diffstat` | 0 | 封面信正文与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 黄金值含系列 diffstat 段） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（封面信系列 diffstat（`--no-stat`/`-p` 仍输出、无唯一边界时省略）说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::cover_letter_includes_diffstat)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::cover_letter_includes_diffstat`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-58`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：封面信系列 diffstat（`--no-stat`/`-p` 仍输出、无唯一边界时省略）说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=封面信系列diffstat; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-58; writeset=序列化于FPG-58; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-14: 支持 Git `--commit-list-format` 原子 `%(count)`/`%(total)` 与 `log:` 前缀（#899；自本卡拆出 FPG-49）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--commit-list-format` 支持 `%(count)`/`%(total)`（count 按总数位宽补零、自 1 起且不受 `--start-number` 影响）与 `log:` 前缀；识别顺序 `log:` → `shortlog`/`modern` → 其余必须含 `%`，否则 `'<fmt>' is not a valid format string` + `LBR-CLI-002`。AC1–AC7 由 `format_patch_test::commit_list_format_git_atoms` 覆盖（count/total/组合、`log:`、非法字面量、10 补丁补零、start-number 均与 Git 列表段逐字一致）；`format_patch_test` 156/156、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；`format.commitListFormat`/`full` 由 `FPG-49` 承接。

**Description:** 让 `render_pretty_format_commit` 接受 Git `--commit-list-format` 的 `%(count)`、`%(total)` 原子（与既有 `%s`/`%an` 等组合）与 `log:` 前缀，并按 Git 判据（依次识别 `log:` 前缀、`shortlog`、`modern`，其余串含 `%` 才接受，否则拒绝，`git show 6de20f6092:builtin/log.c` 第 1443–1453 行）拒绝如 `this should fail` 的串（`log:` 前缀后的字面量按 Git 接受）。`%(count)` 按总数位宽补零，计数自 1 起且不随 `--start-number` 变化（`git show 6de20f6092:pretty.c` 第 1551–1557 行、`builtin/log.c` 第 1368–1386 行）。Libra 保留的 `short` 别名见 `DEFER-FPG-04`。`log:` 按 Git 上游行为去掉前缀后渲染（`git@6de20f6092` `builtin/log.c` 与 t4014「cover letter with subject, author and count」期望 `[1/1] This is a subject (A U Thor)`）；#899 第 2 项文字称输出保留字面 `log:`，与上游不符，本计划以上游为准，并在关闭 #899 时于 issue 中说明。`format.commitListFormat`、`full` 取值（Git 基线无此关键字）与无封面信场景由自本卡拆出的 `FPG-49` 承接。本卡全部夹具不配置分支描述（列表段按「验证执行协议」第 3 项自 `*** BLURB HERE ***` 之后提取）。

**Out of scope:** `format.commitListFormat`、`full` 取值与 `--no-cover-letter` 组合（由 `FPG-49` 承接）；`shortlog` 关键字与隐含封面信（由 `issues/529#FP-13` 交付）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `render_commit_list` 只识别 `short`/`modern` 与自定义串；自定义串经 `render_pretty_format_commit` 只支持 `%h`/`%H`/`%s`/`%an`/`%ae`/`%n`/`%%`，不支持 `%(count)`/`%(total)`，且无原子字面量被接受 | `src/command/format_patch.rs:1535-1584,1621-1644` |
| `resolve_format_config` 未读取 `format.commitListFormat` | `src/command/format_patch.rs:1861-1902` |

**Acceptance criteria:**

- [ ] AC1：`--cover-letter --commit-list-format="[%(count)] %s" -2 --stdout` 渲染计数原子。
- [ ] AC2：`--cover-letter --commit-list-format="[%(total)] %s" -2 --stdout` 渲染总数原子。
- [ ] AC3：`--cover-letter --commit-list-format="[%(count)/%(total)] %s (%an)" -2 --stdout`（t4014 组合用例）。
- [ ] AC4：`--commit-list-format="log:[%(count)/%(total)] %s (%an)" -o patches HEAD~1` 按 Git 去掉 `log:` 前缀渲染。
- [ ] AC5：无原子字面量 `--cover-letter --commit-list-format="this should fail" -1 --stdout` 被拒绝。
- [ ] AC6：10 补丁系列（仓库至少 11 个提交）`--cover-letter --commit-list-format="[%(count)/%(total)] %s" -10 --stdout` 的计数按总数位宽补零。
- [ ] AC7：`--start-number=5 --cover-letter --commit-list-format="[%(count)/%(total)] %s" -2 --stdout` 的计数自 1 起、不随起始号变化。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::commit_list_format_git_atoms` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC2 | `command::format_patch_test::commit_list_format_git_atoms` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC3 | `command::format_patch_test::commit_list_format_git_atoms` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC4 | `command::format_patch_test::commit_list_format_git_atoms` | 0 | `patches/0000-cover-letter.patch` 的封面信列表段与同夹具 Git 黄金值逐字一致（上游期望 `[1/1] This is a subject (A U Thor)`） |
| AC5 | `command::format_patch_test::commit_list_format_git_atoms` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-002`，且 stderr 含 Libra 区分短语 `is not a valid format string`；stdout 为空 |
| AC6 | `command::format_patch_test::commit_list_format_git_atoms` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（首行为 `[01/10] <标题>`） |
| AC7 | `command::format_patch_test::commit_list_format_git_atoms` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 为 `[1/2]`、`[2/2]` 两行） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `%(count)`、`%(total)` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::commit_list_format_git_atoms)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::commit_list_format_git_atoms`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-13`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`%(count)`、`%(total)`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--commit-list-format原子与log:前缀; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-13; writeset=序列化于FPG-13; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-49: 支持 `format.commitListFormat` 并按 Git 基线处理 `full` 取值与无封面信组合（#899 第 3、4 项, #900 第 4 项, #909 第 4 项；自 FPG-14 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：读取 `format.commitListFormat` 作为默认列表格式，命令行优先；仅命令行格式选项隐含封面信，配置不隐含；`full` 按 Git 基线以无效格式串拒绝；`--no-cover-letter` 与 CLI 格式组合成功且不写封面信。AC1–AC7 由 `format_patch_test::commit_list_format_config_and_full` 覆盖（配置 log/atoms、modern、shortlog、CLI 覆盖、配置不隐含封面、no-cover-letter 组合，列表段与 Git 逐字；`full` 稳定错误码）；`format_patch_test` 159/159、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-14` 拆出的独立轴：读取 `format.commitListFormat`（取值语法同命令行；命令行 `--commit-list-format` 优先；按 Git 只有命令行格式选项隐含封面信，配置不隐含），并保证 `--no-cover-letter` 与格式选项同用时成功且不写封面信。#899 第 4 项与 #909 第 4 项所称 `full` 关键字及上游「cover letter full format」用例在 `git@6de20f6092` 中不存在（`builtin/log.c` 只接受 `log:` 前缀、`shortlog`、`modern` 与含 `%` 的串，其余 `die`；该 revision 的 t4014 无此用例）；本计划以上游为准，`full` 按无效格式串拒绝（与 `FPG-14` 无原子字面量同一错误契约），并在关闭 #899、#909 时于 issue 中说明。

**Out of scope:** 原子 `%(count)`/`%(total)` 与 `log:` 前缀（由 `FPG-14` 承接，本卡依赖其先行完成）；`format.coverLetter` 取值（由 `FPG-12` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `render_commit_list` 只识别 `short`/`modern` 与自定义串，无 `full`；`resolve_format_config` 未读取 `format.commitListFormat` | `src/command/format_patch.rs:1535-1584,1861-1902` |
| Git 基线：`--commit-list-format` 只接受 `log:` 前缀、`shortlog`、`modern` 与含 `%` 的串，否则 `die("'%s' is not a valid format string")`；仅命令行格式选项在 `cover_letter == -1` 时置 1，配置值只作缺省格式 | `git show 6de20f6092:builtin/log.c` 第 1443–1453、2359–2365 行 |

**Acceptance criteria:**

- [ ] AC1：`--cover-letter --commit-list-format=full -2 --stdout` 按 Git 基线作为无效格式串被拒绝（#899 第 4 项、#909 第 4 项的冲突说明见 Description）。
- [ ] AC2：`format.coverLetter=true` 与 `format.commitListFormat="log:[%(count)/%(total)] %s (%an)"`、命令行无封面信与格式 flag 时 `-o patches HEAD~2`（t4014「cover letter config with count, subject and author」，#899 第 3 项、#900 第 4 项）。
- [ ] AC3：`format.coverLetter=true` 与 `format.commitListFormat=modern` 时 `-o patches HEAD~2`（t4014「cover letter config commitlistformat set to modern」）。
- [ ] AC4：`format.coverLetter=true` 与 `format.commitListFormat=shortlog` 时 `-o patches HEAD~2`（t4014「cover letter config commitlistformat set to shortlog」）。
- [ ] AC5：`format.commitListFormat=modern` 下命令行 `--commit-list-format="[%(count)] %s" -o patches HEAD~2` 以命令行取值渲染。
- [ ] AC6：只配置 `format.commitListFormat="[%(count)] %s"`（无 `format.coverLetter`、无封面信与格式 flag）时 `-o patches HEAD~1` 不生成封面信（配置不隐含封面信）。
- [ ] AC7：`--no-cover-letter --commit-list-format="[%(count)] %s" -o patches HEAD~1` 成功且不写封面信（t4014「no cover letter but with format specified」）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::commit_list_format_config_and_full` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-002`，且 stderr 含 Libra 区分短语 `is not a valid format string`；stdout 为空 |
| AC2 | `command::format_patch_test::commit_list_format_config_and_full` | 0 | `patches/0000-cover-letter.patch` 的封面信列表段与同夹具、同配置的 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC3 | `command::format_patch_test::commit_list_format_config_and_full` | 0 | `patches/0000-cover-letter.patch` 的封面信列表段与同夹具、同配置的 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC4 | `command::format_patch_test::commit_list_format_config_and_full` | 0 | `patches/0000-cover-letter.patch` 的封面信列表段与同夹具、同配置的 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC5 | `command::format_patch_test::commit_list_format_config_and_full` | 0 | `patches/0000-cover-letter.patch` 的封面信列表段与同夹具、同配置的 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC6 | `command::format_patch_test::commit_list_format_config_and_full` | 0 | `patches/` 文件名集合恰为 1 个 `0001-` 开头的补丁文件 |
| AC7 | `command::format_patch_test::commit_list_format_config_and_full` | 0 | `patches/` 文件名集合恰为 1 个 `0001-` 开头的补丁文件 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.commitListFormat` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::commit_list_format_config_and_full)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::commit_list_format_config_and_full`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-14`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.commitListFormat`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.commitListFormat与边界取值; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-14; writeset=序列化于FPG-14; release=independent; split-from=FPG-14; exception=N/A`

---

### Task FPG-15: 支持多修订参数与 `^<rev>` 排除语法（#892）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：位置参数改为 `Vec<String>`，支持多修订与 `^<rev>` 排除；单裸修订仍 `<rev>..HEAD`，`--root`/`-1`/`-<n>` 保留 newest-first 截断；多修订/范围按 Git 修订集合取并集、减去所有排除 tip 祖先，并按拓扑+提交时间排序。为满足 AC4，默认多补丁/带封面单补丁按 `format.numbered=auto` 输出 `[PATCH n/m]`。AC1–AC7 由 `format_patch_test::revision_range_caret_exclusion` 覆盖（`^main rebuild-1`、显式范围等价、两个正向修订、内部 `^r1`、单修订、`a~n..b`）；`format_patch_test` 160/160、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；`format.numbered` 配置/CLI 覆盖仍由 `FPG-01`/`FPG-32` 承接（`FPG-01` 外部 `CLOSE-529` 门未解除）。

**Description:** 将 `FormatPatchArgs::revision_range` 扩展为支持多个位置修订参数（`Vec<String>`），并解析 `^<rev>` 排除语义（如 `topic ^main` 等价于 `main..topic`）。 夹具（全部提交以逐个递增的固定 `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` 创建，标题互不相同）：`main` 上 `base` → `m1` → `m2`；`rebuild-1` 自 `base` 分出 `r1` → `r2`；各 AC 执行时 HEAD 位于 `main`（使 `^main rebuild-1` 不等于 `main..HEAD`）。各 AC 只比较补丁选择结果（`Subject:` 行序列或封面信列表段），不比较 diff 字节（AC4 含根提交，见「验证执行协议」第 3 项差异隔离）。

**Out of scope:** `--all` 仍保持既有 `Unsupported` 说明。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `revision_range: Option<String>` 仅接收 1 个位置参数 | `src/command/format_patch.rs:108-109,663-685` |

**Acceptance criteria:**

- [ ] AC1：`--cover-letter ^main rebuild-1 --stdout` 与 Git 一致（#892 第 1 项）。
- [ ] AC2：不带封面信的 `^main rebuild-1 --stdout` 与 Git 一致（#892 第 2 项）。
- [ ] AC3：`--cover-letter ^main rebuild-1 --stdout` 与 `--cover-letter main..rebuild-1 --stdout` 输出相同（#892 第 1 项）。
- [ ] AC4：`main rebuild-1 --stdout`（两个正向 revision）与 Git 一致（#892 第 2 项）。
- [ ] AC5：`base..rebuild-1 ^r1 --stdout` 再排除 `r1` 可达的提交，只输出 `r2`（`r1` 位于 `base..rebuild-1` 之内）。
- [ ] AC6：单个 `<rev>`（`HEAD~2 --stdout`）语义不变（#892 第 3 项）。
- [ ] AC7：`rebuild-1~2..rebuild-1 --stdout`（`a~n..b`）语义不变（#892 第 3 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::revision_range_caret_exclusion` | 0 | 封面信列表段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC2 | `command::format_patch_test::revision_range_caret_exclusion` | 0 | `Subject:` 行序列与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC3 | `command::format_patch_test::revision_range_caret_exclusion` | 0 | 两次 stdout 去掉封面信的首行信封与 `Date:` 行（二者含运行时间，`FPG-16` 前尤甚）后逐字节相同 |
| AC4 | `command::format_patch_test::revision_range_caret_exclusion` | 0 | `Subject:` 行序列与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC5 | `command::format_patch_test::revision_range_caret_exclusion` | 0 | `Subject:` 行序列与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 为一行 `Subject: [PATCH] r2`） |
| AC6 | `command::format_patch_test::revision_range_caret_exclusion` | 0 | `Subject:` 行序列与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（Git 为 `m1`、`m2` 两封） |
| AC7 | `command::format_patch_test::revision_range_caret_exclusion` | 0 | `Subject:` 行序列与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `^<rev>` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::revision_range_caret_exclusion)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::revision_range_caret_exclusion`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-49`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`^<rev>`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=多修订参数与^rev排除语法; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-49; writeset=序列化于FPG-49; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-16: mbox `"From "` 分隔符使用 Git 固定 `Mon Sep 17 00:00:00 2001` 时间戳（#893；自本卡拆出 FPG-59）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：普通补丁与封面信首行 mbox `From <oid> <date>` 的时间戳统一为 Git 固定 `Mon Sep 17 00:00:00 2001`；补丁用该提交完整 oid，封面信用系列末个提交 oid，`--zero-commit` 按哈希长度补零；补丁 `Date:` 仍取作者时间。AC1–AC4 由 `format_patch_test::mbox_envelope_fixed_2001_timestamp` 覆盖（固定信封、zero-commit、封面信封、`libra am` 拆分应用两封），AC5 由 `compat_format_patch_mail_roundtrip::fixed_envelope_mbox_applies_with_git_am` 覆盖（真实 `git am` 应用）；`format_patch_test` 138/138、`compat_format_patch_mail_roundtrip` 全绿；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 将普通补丁与封面信首行 mbox `"From <oid> ..."` 分隔符的时间戳统一为 Git 固定的 `Mon Sep 17 00:00:00 2001`（封面信信封按 Git 使用系列末个提交的 oid，`git show 6de20f6092:builtin/log.c` 第 1403、1416 行 `log_write_email_headers(rev, head, …)`；`--zero-commit` 时为全零）。#893 第 3 项（补丁 `Date:` 取作者时间与作者时区）由自本卡拆出的 `FPG-59` 承接。夹具为两补丁系列（标题互不相同）；AC4、AC5 对 AC1 的同一 `-2 --stdout` mbox 分别在独立的临时接收仓库中应用。

**Out of scope:** 补丁 `Date:` 头（由 `FPG-59` 承接）；封面信 `Date:`（仍为当前时间，与 Git 一致）；`libra am` 的 `Date:` 解析（不变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_patch_body` 与 `format_cover_letter` 在 `"From "` 行写入了 `ts`/`now` | `src/command/format_patch.rs:803-807,1373-1376` |
| `format_cover_letter` 的信封写全零 oid 与当前时间 | `src/command/format_patch.rs:1373-1376` |

**Acceptance criteria:**

- [ ] AC1：每封补丁邮件首行为 `From <该提交完整 oid> Mon Sep 17 00:00:00 2001`。
- [ ] AC2：`--zero-commit` 时首行为 40 个 `0` 加固定时间（SHA-1 仓库）。
- [ ] AC3：封面信首行使用系列末个提交 oid 与固定时间（#893 第 1 项）。
- [ ] AC4：AC1 的两补丁固定信封 mbox 经 `libra am` 拆分并应用两封补丁。
- [ ] AC5：同一 mbox 经 `git am` 拆分并应用两封补丁。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::mbox_envelope_fixed_2001_timestamp` | 0 | `-2 --stdout`（无封面信）中第 i 封邮件首行逐字等于 `From <第 i 个补丁提交的完整 oid> Mon Sep 17 00:00:00 2001`（对全部邮件的全称判定） |
| AC2 | `command::format_patch_test::mbox_envelope_fixed_2001_timestamp` | 0 | 每封邮件首行逐字为 `From 0000000000000000000000000000000000000000 Mon Sep 17 00:00:00 2001` |
| AC3 | `command::format_patch_test::mbox_envelope_fixed_2001_timestamp` | 0 | `--cover-letter -2 --stdout` 的封面信首行逐字等于 `From <系列末个提交的完整 oid> Mon Sep 17 00:00:00 2001` |
| AC4 | `command::format_patch_test::mbox_envelope_fixed_2001_timestamp` | 0 | 接收仓库新增两个提交，其 `libra log -n 2 --format=%s` 的 stdout 逐字为两源提交标题（较新者在前） |
| AC5 | `fixed_envelope_mbox_applies_with_git_am`（target `compat_format_patch_mail_roundtrip`） | 0 | Git 接收仓库新增两个提交，其 `git log -2 --format=%s` 的 stdout 逐字为两源提交标题（较新者在前） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `Mon Sep 17 00:00:00 2001` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::mbox_envelope_fixed_2001_timestamp)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::mbox_envelope_fixed_2001_timestamp`；证明 AC1–AC4）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail -E 'test(=fixed_envelope_mbox_applies_with_git_am)'`（`(new)`；具名验收门，期望集合 N=1：`fixed_envelope_mbox_applies_with_git_am`（target `compat_format_patch_mail_roundtrip`）；证明 AC5）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-15`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`Mon Sep 17 00:00:00 2001`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=mboxFrom固定时间戳与封面信信封oid; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-15; writeset=序列化于FPG-15; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-59: 补丁 `Date:` 头取作者时间与作者时区（#893 第 3 项；自 FPG-16 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：补丁 `Date:` 改为作者时间+作者时区（RFC 2822；封面信仍为当前时间）；`libra am` 与 `git am` 均保留作者时间/偏移。AC1–AC3 由 `format_patch_test::patch_date_uses_author_time_and_zone` 覆盖（Date 行与 Git 逐字 `Thu, 7 Apr 2005 15:13:13 +0800`；两种 am 后 `%at %ai` 均为 `1112857993 2005-04-07 15:13:13 +0800`）；`format_patch_test` 157/157、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；libra am 对多封 mbox 的现有多邮件解析问题不在本卡范围，测试按单封应用同一 mbox 内容。

**Description:** 自 `FPG-16` 拆出的独立轴：按 #893 第 3 项把补丁 `Date:` 头改为 Git 的作者时间与作者时区（Git 以 `DATE_RFC2822` 渲染作者签名）；现行 `timestamp_from_commit` 取提交者时间并以 UTC 渲染（`src/command/format_patch.rs:828-829,1837-1840`）；`libra am` 已保留 `Date:` 的时区（`src/command/mailinfo.rs:747-759`）。夹具：两补丁系列，两个提交的作者时间均为 `2005-04-07 15:13:13 +0800`、提交者时间均为 `2006-01-02 03:04:05 +0000`（作者 ≠ 提交者、作者时区非 UTC）；AC2、AC3 对 AC1 的同一 `-2 --stdout` mbox 分别在独立的临时接收仓库中应用。

**Out of scope:** mbox 信封（由 `FPG-16` 承接，本卡依赖其先行完成）；封面信 `Date:`（仍为当前时间，与 Git 一致）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `Date:` 由 `timestamp_from_commit`（提交者时间戳，`DateTime<Utc>`）渲染 | `src/command/format_patch.rs:828-829,1837-1840` |
| `libra am`/`mailinfo` 按 `Date:` 的偏移保留作者时区 | `src/command/mailinfo.rs:747-759` |

**Acceptance criteria:**

- [ ] AC1：补丁 `Date:` 头按 Git 为作者时间与作者时区（#893 第 3 项）。
- [ ] AC2：该 mbox 经 `libra am` 应用后保留作者时间与作者时区。
- [ ] AC3：该 mbox 经 `git am` 应用后保留作者时间与作者时区。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::patch_date_uses_author_time_and_zone` | 0 | `-2 --stdout` 中每封补丁邮件的 `Date:` 行与同夹具 Git 黄金值逐字一致（首封为 `Date: Thu, 7 Apr 2005 15:13:13 +0800`） |
| AC2 | `command::format_patch_test::patch_date_uses_author_time_and_zone` | 0 | 接收仓库新增两个提交，二者的作者时间戳与时区均等于 `1112857993 +0800`（源提交作者的时间，不同于源提交者时间） |
| AC3 | `author_date_and_zone_roundtrip_with_git_am`（target `compat_format_patch_mail_roundtrip`） | 0 | Git 接收仓库 `git log -2 --format=%ai` 的输出逐字为两行 `2005-04-07 15:13:13 +0800` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（补丁 `Date:` 取作者时间与作者时区的说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::patch_date_uses_author_time_and_zone)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::patch_date_uses_author_time_and_zone`；证明 AC1–AC2）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail -E 'test(=author_date_and_zone_roundtrip_with_git_am)'`（`(new)`；具名验收门，期望集合 N=1：`author_date_and_zone_roundtrip_with_git_am`（target `compat_format_patch_mail_roundtrip`）；证明 AC3）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-16`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：补丁 `Date:` 取作者时间与作者时区的说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=补丁Date作者时间与时区; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-16; writeset=序列化于FPG-16; release=independent; split-from=FPG-16; exception=N/A`

---

### Task FPG-64: base 区块只放在第一封邮件（#894 第 4 项相关；自 FPG-17 拆出，先于 FPG-17 执行）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：base 区块只放在第一封邮件——有封面信在封面信、否则第一封补丁（Git `print_bases` 只输出一次），不再放最后一封。AC1/AC2 由 `format_patch_test::base_block_rides_on_first_mail` 覆盖（无封面首补丁含 base-commit+prerequisite、次补丁为空；封面含区块且两补丁为空；逐封与 Git 黄金比较，独立夹具 OID 归一化）；`format_patch_test` 158/158、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；`--base=auto` 由 `FPG-17` 承接。

**Description:** 自 `FPG-17` 拆出的独立轴（先于 `FPG-17` 执行，`--base=auto` 直接继承该位置）：按 Git 把 base 区块（`base-commit:` 与 `prerequisite-patch-id:`）只放在第一封邮件——有封面信时在封面信，否则在第一封补丁（`git show 6de20f6092:builtin/log.c` 第 1894–1913、2534 行 `print_bases` 只输出一次）；现行无封面信时放在最后一封补丁（`src/command/format_patch.rs:532-533,569-574`）。既有显式 `--base` 用例均为单补丁，不受影响（#894 第 4 项）。

**Out of scope:** `--base=auto` 的基线选择（由 `FPG-17` 承接，在本卡之后执行）；base 区块内容与 `prerequisite-patch-id` 算法（不变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 无封面信时 base 区块追加在最后一封补丁 | `src/command/format_patch.rs:532-533,569-574` |
| Git `print_bases` 只在第一封邮件输出 base 区块 | `git show 6de20f6092:builtin/log.c` 第 1894–1913、2534 行 |

**Acceptance criteria:**

- [ ] AC1：显式 `--base=HEAD~3 -2 --stdout`（无封面信；`HEAD~2` 为 base 与系列之间的前置提交，故区块含 `prerequisite-patch-id:` 行）的完整 base 区块只出现在第一封补丁邮件（Git `print_bases` 只输出一次）。
- [ ] AC2：同一夹具加 `--cover-letter` 时完整 base 区块只出现在封面信，补丁邮件不重复输出。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::base_block_rides_on_first_mail` | 0 | 逐封比较：每封邮件中以 `base-commit:` 或 `prerequisite-patch-id:` 开头的行组成的序列与同夹具 Git 黄金值中同序号邮件的序列逐字一致（Git 为第一封含 `base-commit:` 与 `prerequisite-patch-id:` 各行、第二封为空序列；只移动 `base-commit:` 而留下 `prerequisite-patch-id:` 的实现不等） |
| AC2 | `command::format_patch_test::base_block_rides_on_first_mail` | 0 | 逐封比较：每封邮件（含封面信）中以 `base-commit:` 或 `prerequisite-patch-id:` 开头的行组成的序列与同夹具 Git 黄金值中同序号邮件的序列逐字一致（Git 为封面信含完整区块、两封补丁均为空序列） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（base 区块所在邮件的说明（无需改动时记录核对结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::base_block_rides_on_first_mail)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::base_block_rides_on_first_mail`；证明 AC1–AC2）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-59`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：base 区块所在邮件的说明（无需改动时记录核对结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=base区块只在第一封邮件; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-59; writeset=序列化于FPG-59; release=independent; split-from=FPG-17; exception=N/A`

---

### Task FPG-17: 支持 `--base=auto` 自动基线探测（#894 命令行部分；自本卡拆出 FPG-43、FPG-64）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：实现 `--base=auto`——上游跟踪 ref 与每个正向修订的 merge base 归约后要求唯一；无上游/歧义/非祖先/base 在列表内分别以 `LBR-CLI-003`/128 失败；显式 `--base` 与首封邮件位置不变。AC1–AC7 由 `format_patch_test::base_auto_cli`（同源 Git 夹具逐字、无上游、criss-cross、whole-list 祖先、base 在列表内）与既有 `base_records_base_commit_and_prerequisite_patch_ids` 覆盖；`base_auto_is_rejected` 已迁移；`format_patch_test` 161/161、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；`format.useAutoBase` 由 `FPG-43` 承接。

**Description:** 实现 `--base=auto`：按 Git `get_base_commit`（`git show 6de20f6092:builtin/log.c` 第 1702 行起）取当前分支上游跟踪分支与**整个修订列表**的唯一 merge-base（不唯一即 `could not find exact merge base`），再要求修订列表两两求得的公共祖先唯一（否则 `failed to find exact merge base`）、base 是其祖先（否则 `base commit should be the ancestor of revision list`）且不在修订列表内（否则 `base commit shouldn't be in revision list`），输出 `base-commit:` / `prerequisite-patch-id:`；无上游时失败；显式 `--base=<commit>` 不变。多个正向 revision 由 `FPG-15` 先行支持。`format.useAutoBase` 由自本卡拆出的 `FPG-43` 承接。 base 区块只放在第一封邮件（对显式 `--base` 与 `--base=auto` 相同）由自本卡拆出、先于本卡执行的 `FPG-64` 承接。

**Out of scope:** `format.useAutoBase` 配置（由 `FPG-43` 承接）；base 区块所在邮件（由 `FPG-64` 承接）；BLAKE3 仓库 `prerequisite-patch-id` 的既有算法说明（不变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `build_base_block` 未处理 `"auto"`，`COMPATIBILITY.md` 记录 `--base=auto` 被拒绝 | `src/command/format_patch.rs:1214-1284`、`COMPATIBILITY.md:224` |
| 既有 `base_auto_is_rejected` 断言 `--base=auto` 以 129 拒绝；本卡把该用例按「验证执行协议」第 2 项的行为迁移改写为新契约 | `tests/command/format_patch_test.rs:1594-1606` |
| 既有 `--base` 失败路径以 `CliError::fatal(...).with_exit_code(128).with_stable_code(CliInvalidTarget)` 返回；自动基线失败沿用该约定（Git 亦以 128 退出） | `src/command/format_patch.rs:1236-1256` |
| `base_auto_is_rejected` 迁移后断言：无上游时非 0 退出，stderr 含 `failed to get upstream` 与独立一行 `Error-Code: LBR-CLI-003`（同 AC3） | `tests/command/format_patch_test.rs:1594-1606` |

**Acceptance criteria:**

- [ ] AC1：有上游跟踪分支、且本地分支自上游 `U1` 分出后上游又前进到 `U2` 时，`--base=auto -2 --stdout` 以修订列表与上游的 merge-base `U1`（而非上游 tip `U2`）为 base（#894 第 1 项）。
- [ ] AC2：base 与系列之间存在前置文本提交时输出 `prerequisite-patch-id:`（#894 第 2 项）。
- [ ] AC3：无上游时 `--base=auto` 被拒绝。
- [ ] AC4：criss-cross 历史下 `--base=auto` 失败（#894 第 3 项）。
- [ ] AC5：显式 `--base=<commit>` 行为不变（#894 第 4 项）。
- [ ] AC6：上游 tip 为 `B`（`A` 为其父提交），`side1`、`side3` 自 `A` 分出，`side2` 自 `B` 分出，三个分支提交的时间按 `side1`、`side2`、`side3` 递增时，`--base=auto --stdout ^B side1 side2 side3` 按 Git 拒绝：以整个修订列表求得的 merge-base `B` 不是 `side1`/`side3` 的祖先（只按首个或末个修订计算的实现——无论按命令行顺序还是按时间顺序——都取到 `side1` 或 `side3`，得到 `A` 并成功）。
- [ ] AC7：修订列表包含上游 merge-base 本身（上游 tip `U`，`side1`、`side2` 均从 `U` 分出，`A` 为 `U` 的父提交，`--base=auto --stdout ^A side1 side2`）时按 Git 拒绝。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::base_auto_cli` | 0 | `base-commit:` 行逐字为 `base-commit: <U1 的完整 oid>`（与同夹具 Git 黄金值一致） |
| AC2 | `command::format_patch_test::base_auto_cli` | 0 | base 区块（`base-commit:` 行至最后一个 `prerequisite-patch-id:` 行）与同夹具 Git 黄金值逐字一致 |
| AC3 | `command::format_patch_test::base_auto_cli` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `failed to get upstream`；stdout 为空；无补丁文件 |
| AC4 | `command::format_patch_test::base_auto_cli` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `exact merge base`；stdout 为空 |
| AC5 | `command::format_patch_test::base_records_base_commit_and_prerequisite_patch_ids` | 0 | 既有用例对 `base-commit:` 与 `prerequisite-patch-id:` 的断言不变 |
| AC6 | `command::format_patch_test::base_auto_cli` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `should be the ancestor of revision list`；stdout 为空 |
| AC7 | `command::format_patch_test::base_auto_cli` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `shouldn't be in revision list`；stdout 为空 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--base=auto` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::base_auto_cli)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::base_auto_cli`；证明 AC1–AC4、AC6–AC7）
- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::base_records_base_commit_and_prerequisite_patch_ids)'`（既有；具名验收门，期望集合 N=1：`command::format_patch_test::base_records_base_commit_and_prerequisite_patch_ids`；证明 AC5）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-64`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--base=auto`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--base=auto自动基线; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-64; writeset=序列化于FPG-64; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-43: 支持 `format.useAutoBase`（#894 配置部分；自 FPG-17 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：读取 `format.useAutoBase`——`true`=`--base=auto`、`false` 关闭、`whenAble` 自动基线失败时静默省略；`--no-base`/显式 `--base` 覆盖配置；本地覆盖全局。AC1–AC7 由 `format_patch_test::format_use_auto_base` 覆盖（base 区块与同源 Git 逐字、无上游 true 失败、whenAble 静默且 stderr 无额外诊断、--no-base、本地 false 覆盖全局 true）；`format_patch_test` 162/162、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-17` 拆出的独立轴：经 `read_format_config` 读取 `format.useAutoBase`（`false`/`true`/`whenAble`），`true` 等价 `--base=auto`，`whenAble` 无法确定基线时静默省略，`--no-base` 覆盖配置。

**Out of scope:** `--base=auto` 本身（由 `FPG-17` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_format_config` 未读取 `format.useAutoBase` | `src/command/format_patch.rs:1861-1902` |

**Acceptance criteria:**

- [ ] AC1：有上游时 `format.useAutoBase=true`（无 flag）输出与 `--base=auto` 相同的非空 base 区块。
- [ ] AC2：无上游时 `format.useAutoBase=true` 被拒绝。
- [ ] AC3：无上游时 `format.useAutoBase=whenAble` 静默省略。
- [ ] AC4：同一 `whenAble` 场景不输出额外诊断。
- [ ] AC5：`format.useAutoBase=true` 下 `--no-base` 不输出 base。
- [ ] AC6：有上游时 `format.useAutoBase=whenAble`（无 flag）输出 base 区块（t4014「format-patch format.useAutoBase option with whenAble」）。
- [ ] AC7：有上游时隔离的全局层 `format.useAutoBase=true`、本地 `format.useAutoBase=false`（无 flag）不输出 base 区块（Git 布尔假值即不自动取基线，本地值覆盖全局；全局库经临时 `LIBRA_CONFIG_GLOBAL_DB` 隔离，GC-13）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_use_auto_base` | 0 | `-2 --stdout` 的 base 区块与 FPG-17 AC1 同夹具（上游已前进到 `U2`）的 Git 黄金 base 区块逐字一致 |
| AC2 | `command::format_patch_test::format_use_auto_base` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `failed to get upstream`；stdout 为空；无补丁文件 |
| AC3 | `command::format_patch_test::format_use_auto_base` | 0 | stdout（`--stdout` 运行）不存在以 `base-commit:` 开头的行 |
| AC4 | `command::format_patch_test::format_use_auto_base` | 0 | stderr 的行序列逐字等于同夹具、同命令但不配置 `format.useAutoBase` 时的 stderr 行序列 |
| AC5 | `command::format_patch_test::format_use_auto_base` | 0 | stdout（`--stdout` 运行）不存在以 `base-commit:` 开头的行 |
| AC6 | `command::format_patch_test::format_use_auto_base` | 0 | `-2 --stdout` 的 base 区块与 FPG-17 AC1 同夹具的 Git 黄金 base 区块逐字一致 |
| AC7 | `command::format_patch_test::format_use_auto_base` | 0 | `-2 --stdout`（FPG-17 AC1 同夹具，有上游）的 stdout 不存在以 `base-commit:` 开头的行（只读全局层或把 `false` 当作启用的实现会输出 base 区块） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.useAutoBase` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_use_auto_base)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_use_auto_base`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-17`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.useAutoBase`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.useAutoBase配置; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-17; writeset=序列化于FPG-17; release=independent; split-from=FPG-17; exception=N/A`

---

### Task FPG-24: 封面信支持 `branch.<name>.description`（#891 分支描述部分、#892 第 4 项；自本卡拆出 FPG-60）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：封面信正文读取 `branch.<name>.description` 替换 `*** BLURB HERE ***`；无修订/单一裸修订取当前分支，范围/`^<rev>`/多修订取唯一正向分支，多条正向省略，空描述回退占位符。AC1–AC3 由 `format_patch_test::cover_letter_branch_description` 覆盖（当前 topic 描述、`^main rebuild-1` 选 rebuild 且不含 side、无修订 `-2` 用当前分支，均与 Git 描述段逐字）；`format_patch_test` 163/163、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；默认封面信 `From:` 由 `FPG-60`、`--cover-from-description` 由 `FPG-25` 承接。

**Description:** 自 `FPG-12` 拆出的独立轴：封面信按 Git 选分支读取 `branch.<name>.description`：单一 revision（隐含 `..HEAD`）或无 revision 时取当前分支（`git show 6de20f6092:builtin/log.c` 第 2288–2320 行 `check_head`），其余写法（`^main rebuild-1`、`main..topic`，由 `FPG-15` 先行支持）按 `find_branch_name`（第 1233–1261 行）取唯一正向 revision 对应的分支（#892 第 4 项）。封面信默认 `From:` 身份由自本卡拆出的 `FPG-60` 承接。

**Out of scope:** `--cover-from-description` 模式校验由 `FPG-25` 处理；未传 `--from` 时封面信 `From:` 的默认身份（由 `FPG-60` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_cover_letter` 未读 `branch.<name>.description`，正文固定写占位符 | `src/command/format_patch.rs:1422-1424` |

**Acceptance criteria:**

- [ ] AC1：当前分支 `topic` 配置 `branch.topic.description` 时，`--cover-letter main --stdout`（单一 revision，Git 按 HEAD 取当前分支）把该描述写入封面信正文（Git 默认 `message` 模式）。
- [ ] AC2：当前分支为 `side`（`branch.side.description=side-desc`）、`branch.rebuild-1.description=rebuild-desc` 时，`--cover-letter ^main rebuild-1 --stdout` 按 Git `find_branch_name` 使用 `rebuild-1` 的描述（t4014「cover letter using branch description (3)」，#892 第 4 项）。
- [ ] AC3：当前分支 `topic` 配置 `branch.topic.description` 时，不给 revision 的 `--cover-letter -2 --stdout`（Git 以默认 `HEAD` 取当前分支）把该描述写入封面信正文。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::cover_letter_branch_description` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（夹具另给 `main` 配置可区分的描述 `main-desc`） |
| AC2 | `command::format_patch_test::cover_letter_branch_description` | 0 | 封面信描述段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值含 `rebuild-desc`、不含 `side-desc`） |
| AC3 | `command::format_patch_test::cover_letter_branch_description` | 0 | 封面信描述段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（黄金值含 `branch.topic.description` 的内容；不读描述的实现输出 `*** BLURB HERE ***`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `branch.<name>.description` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::cover_letter_branch_description)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::cover_letter_branch_description`；证明 AC1–AC3）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-43`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`branch.<name>.description`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=封面信branch.description; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-43; writeset=序列化于FPG-43; release=independent; split-from=FPG-12; exception=N/A`

---

### Task FPG-60: 封面信默认 `From:` 取执行时提交者身份（#891 默认 `From:` 部分；自 FPG-24 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：未传 `--from`/`format.from` 时封面信 `From:` 取执行时提交者身份（`GIT_COMMITTER_*` 优先，其次 `user.name`/`user.email`），补丁仍用作者。AC1/AC2 由 `format_patch_test::cover_letter_default_from_identity` 覆盖（环境身份覆盖、配置身份且与全部提交作者不同）；`format_patch_test` 164/164、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-24` 拆出的独立轴：未传 `--from` 时以执行时的提交者身份（Git `git_committer_info`，`git show 6de20f6092:builtin/log.c` 第 1411 行；`GIT_COMMITTER_*` 优先，其次 `user.name`/`user.email`）填入封面信 `From:`，而非现行的空值；补丁邮件的 `From:` 不变（仍为作者）。

**Out of scope:** 分支描述（由 `FPG-24` 承接，本卡依赖其先行完成）；`--from`/`format.from` 改写（由 `FPG-10`、`FPG-31`、`FPG-46` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_cover_letter` 在 `from_identity` 为 `None` 时输出 `From: \n` | `src/command/format_patch.rs:1386-1396` |

**Acceptance criteria:**

- [ ] AC1：未传 `--from` 且设 `GIT_COMMITTER_NAME=Env Name`、`GIT_COMMITTER_EMAIL=env@example.com` 时 `--cover-letter -2 --stdout` 的封面信 `From:` 为该身份。
- [ ] AC2：未设身份环境变量时封面信 `From:` 为执行时的 `user.name <user.email>`（不取提交作者）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::cover_letter_default_from_identity` | 0 | 封面信头区中以 `From:` 开头的行恰为一行且逐字为 `From: Env Name <env@example.com>`（夹具作者与 `user.*` 均不同于该身份） |
| AC2 | `command::format_patch_test::cover_letter_default_from_identity` | 0 | 封面信头区中以 `From:` 开头的行恰为一行且逐字为 `From: Config User <config@example.com>`（夹具在全部提交完成后才把 `user.name`/`user.email` 改为该值，与全部提交作者不同） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（封面信默认 `From:` 取执行时提交者身份的说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::cover_letter_default_from_identity)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::cover_letter_default_from_identity`；证明 AC1–AC2）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-24`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：封面信默认 `From:` 取执行时提交者身份的说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=封面信默认From身份; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-24; writeset=序列化于FPG-24; release=independent; split-from=FPG-24; exception=N/A`

---

### Task FPG-25: 实现 `--cover-from-description` 模式输出与 CLI 值校验（#906 CLI 部分；自 FPG-13 拆出；自本卡拆出 FPG-33）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：新增 `--cover-from-description` 五模式（空格/= 均可）：default/message 全描述入正文、none 占位符、subject 首段入标题余段入正文、auto 仅格式化首段 ≤100 字节用 subject；非法值无论是否启用封面信均 `invalid cover from description mode` + `LBR-CLI-002`。AC1–AC8 由 `format_patch_test::cover_from_description` 覆盖（Git 三模式逐字、default=message、空格形式、100/120 字节边界、无封面非法值）；`format_patch_test` 165/165、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过（含新 flag 必列）；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；`format.coverFromDescription` 由 `FPG-33` 承接。

**Description:** 自 `FPG-13` 拆出的独立轴：新增 `--cover-from-description=<mode>`（仅接受 `default`/`none`/`message`/`subject`/`auto`），按模式决定分支描述写入封面信标题还是正文；非法 CLI 值不论是否启用封面信均拒绝并报 `invalid cover from description mode`（`LBR-CLI-002`；由本卡的模式校验产生，不得在 clap 取值枚举阶段拒绝；Git `fatal:`/128 的差异见 `DEFER-FPG-04`）。`format.coverFromDescription` 由自本卡拆出的 `FPG-33` 承接。

**Out of scope:** 封面信 diffstat（由 `FPG-13` 承接，本卡依赖其先行完成）；`format.coverFromDescription` 配置（由 `FPG-33` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 无 `--cover-from-description` CLI 参数及 `format.coverFromDescription` 校验 | `src/command/format_patch.rs:97-364,1861-1902` |
| Git `auto` 模式以格式化后主题的字节长度与阈值 100 比较 | `git show 6de20f6092:builtin/log.c` 第 56、1311–1320 行（`prepare_cover_text`） |

**Acceptance criteria:**

- [ ] AC1：`--cover-from-description=none` 保留占位符。
- [ ] AC2：`--cover-from-description=message` 把整段描述写入正文。
- [ ] AC3：`--cover-from-description=default` 与 `message` 一致。
- [ ] AC4：`--cover-from-description=subject` 把首段（两行，Git 以空格拼接）写入标题、其余段落写入正文。
- [ ] AC5：`--cover-from-description=auto` 且格式化后的首段主题恰为 100 字节（ASCII）时同 `subject`（Git 以 `subject_sb.len > COVER_FROM_AUTO_MAX_SUBJECT_LEN` 判定）。
- [ ] AC6：`--cover-from-description=auto` 且首段为不足 100 个字符、但格式化后超过 100 字节的非 ASCII 文本时同 `message`（字节阈值边界）。
- [ ] AC7：空格形式 `--cover-from-description subject` 与 `=` 形式一致。
- [ ] AC8：空格形式 `--cover-from-description garbage`（不带 `--cover-letter`）因模式非法被拒绝（#906 第 2 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::cover_from_description` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC2 | `command::format_patch_test::cover_from_description` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与 Git `message` 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC3 | `command::format_patch_test::cover_from_description` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与 AC2 的 Libra 输出逐字节相同 |
| AC4 | `command::format_patch_test::cover_from_description` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与 Git `subject` 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC5 | `command::format_patch_test::cover_from_description` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与同一描述在 `subject` 模式下的 Libra 输出逐字节相同 |
| AC6 | `command::format_patch_test::cover_from_description` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与同一描述在 `message` 模式下的 Libra 输出逐字节相同 |
| AC7 | `command::format_patch_test::cover_from_description` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与 AC4 的 Libra 输出逐字节相同 |
| AC8 | `command::format_patch_test::cover_from_description` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-002`，且 stderr 含 Libra 区分短语 `invalid cover from description mode`；stdout 为空；无补丁文件 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--cover-from-description` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::cover_from_description)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::cover_from_description`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-60`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--cover-from-description`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--cover-from-description CLI模式输出与取值校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-60; writeset=序列化于FPG-60; release=independent; split-from=FPG-13; exception=N/A`

---

### Task FPG-33: `format.coverFromDescription` 配置读取、校验与 CLI 覆盖（#906 配置部分；自 FPG-25 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：严格级联读取 `format.coverFromDescription`，配置回调阶段校验；非法值即使 CLI 合法/无封面信也报 `invalid cover from description mode` + `LBR-CLI-003`；合法配置作为默认模式，CLI 覆盖。AC1–AC8 由 `format_patch_test::format_cover_from_description_config` 覆盖（五模式与 CLI 逐字节、auto 阈值、CLI none 覆盖、无封面非法配置、合法 CLI 不掩盖非法配置）；`format_patch_test` 166/166、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-25` 拆出的独立轴：经 `read_format_config` 严格级联读取 `format.coverFromDescription`（与 CLI 同一取值集），非法值在读取配置时即拒绝并报 `invalid cover from description mode`（`LBR-CLI-003`；不论命令行是否给出合法 `--cover-from-description`、是否启用封面信，`git show 6de20f6092:builtin/log.c` 第 1101–1103 行在配置回调中 `die`，先于参数解析），CLI `--cover-from-description` 覆盖配置。

**Out of scope:** 各模式的封面信渲染（由 `FPG-25` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_format_config` 未读取 `format.coverFromDescription`；Git 对非法模式以 128 退出并报 `invalid cover from description mode`（#906） | `src/command/format_patch.rs:1861-1916` |

**Acceptance criteria:**

- [ ] AC1：`format.coverFromDescription=subject` 同 `--cover-from-description=subject`。
- [ ] AC2：`format.coverFromDescription=default` 同 `--cover-from-description=default`。
- [ ] AC3：`format.coverFromDescription=auto` 同 `--cover-from-description=auto`（首段格式化后不超过 100 字节、标题与正文可区分的夹具，期望同 `subject`，从而不同于默认的 `message`；阈值另一侧由 `FPG-25` AC5/AC6 验收）。
- [ ] AC4：`format.coverFromDescription=none` 同 `--cover-from-description=none`。
- [ ] AC5：`format.coverFromDescription=message` 同 `--cover-from-description=message`。
- [ ] AC6：`format.coverFromDescription=garbage` 时不带 `--cover-letter` 的 `-1 --stdout` 被拒绝（#906 第 1 项；Git 读配置即校验）。
- [ ] AC7：`format.coverFromDescription=subject` 下 `--cover-from-description=none` 以 CLI 为准。
- [ ] AC8：`format.coverFromDescription=garbage` 下 `--cover-letter --cover-from-description=subject -2 --stdout` 仍被拒绝（命令行合法值不掩盖非法配置）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_cover_from_description_config` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与该 CLI 场景的 Libra 输出逐字节相同 |
| AC2 | `command::format_patch_test::format_cover_from_description_config` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与该 CLI 场景的 Libra 输出逐字节相同 |
| AC3 | `command::format_patch_test::format_cover_from_description_config` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与该 CLI 场景的 Libra 输出逐字节相同 |
| AC4 | `command::format_patch_test::format_cover_from_description_config` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与该 CLI 场景的 Libra 输出逐字节相同 |
| AC5 | `command::format_patch_test::format_cover_from_description_config` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与该 CLI 场景的 Libra 输出逐字节相同 |
| AC6 | `command::format_patch_test::format_cover_from_description_config` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `invalid cover from description mode`；stdout 为空；无补丁文件 |
| AC7 | `command::format_patch_test::format_cover_from_description_config` | 0 | 封面信 `Subject:` 行与描述段（一次拼接比较）与 `none` 模式 CLI 场景的 Libra 输出逐字节相同 |
| AC8 | `command::format_patch_test::format_cover_from_description_config` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `invalid cover from description mode`；stdout 为空；无补丁文件 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.coverFromDescription` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_cover_from_description_config)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_cover_from_description_config`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-25`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.coverFromDescription`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.coverFromDescription配置与CLI覆盖; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-25; writeset=序列化于FPG-25; release=independent; split-from=FPG-25; exception=N/A`

---

### Task FPG-18: 支持 `--attach=<boundary>`/`--inline=<boundary>` 自定义 boundary（#895；自本卡拆出 FPG-38）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--attach`/`--inline` 改为可选 boundary（`require_equals`），自定义值为 `------------<BOUNDARY>`，无值默认既有 boundary；空格形式作为修订参数；base trailer 位于附件内、封面信非 multipart。AC1–AC7 由 `format_patch_test::attach_custom_boundary` 覆盖（boundary、inline disposition、base 与终止行、封面非 multipart、空格修订）；`format_patch_test` 167/167、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；`format.attach`/`--no-attach` 由 `FPG-38` 承接。

**Description:** 将 `--attach` 与 `--inline` 升级为可选值参数 `--attach[=<boundary>]`/`--inline[=<boundary>]`（Git 的 boundary 为 `------------<值>`），保持与 `--base` 组合时 trailer 在附件部分内、封面信仍为非 multipart；`format.attach` 配置与 `--no-attach` 由自本卡拆出的 `FPG-38` 承接。

**Out of scope:** `format.attach` 配置与 `--no-attach`（由 `FPG-38` 承接）；multipart 分段结构以外的 MIME 变更（永久非目标）；`--attach` 与 `--inline` 同时出现时的取舍（见 `DEFER-FPG-04`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `attach`/`inline` 为 `bool` 拒绝 `--attach=<name>`；`resolve_format_config` 未读 `format.attach` | `src/command/format_patch.rs:251-256,883-891,1861-1902` |

**Acceptance criteria:**

- [ ] AC1：`--attach=mimemime --stdout -1` 生成以该值构成 boundary 的 multipart 附件（#895 第 2 项）。
- [ ] AC2：`--inline=mimemime --stdout -1` 生成同 boundary 的 inline 附件。
- [ ] AC3：`--attach=mimemime --base=HEAD~ -1 --stdout` 同时含 base 与终止分隔行（#895 第 1 项）。
- [ ] AC4：`--attach --cover-letter -1` 的封面信不是 multipart（#895 第 4 项）。
- [ ] AC5：`--attach=mimemime --cover-letter -1 --stdout` 的封面信不是 multipart（#895）。
- [ ] AC6：`--attach=mimemime --stdout -1` 的附件分段含补丁类型、disposition 与 diff（#895 第 2 项）。
- [ ] AC7：`--attach HEAD~1 --stdout`（空格形式）中的 `HEAD~1` 按 Git 作为修订参数（`--attach` 为可选值参数，值须用 `=` 紧贴）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::attach_custom_boundary_and_config` | 0 | MIME 骨架（`Content-Type: multipart/mixed` 行与全部分隔行、终止行）与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC2 | `command::format_patch_test::attach_custom_boundary_and_config` | 0 | MIME 骨架与 `Content-Disposition:` 行与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC3 | `command::format_patch_test::attach_custom_boundary_and_config` | 0 | 自 `base-commit:` 行至终止分隔行的区段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC4 | `command::format_patch_test::attach_custom_boundary_and_config` | 0 | 封面信邮件不存在含 `multipart/` 的行 |
| AC5 | `command::format_patch_test::attach_custom_boundary_and_config` | 0 | 封面信邮件不存在含 `multipart/` 的行 |
| AC6 | `command::format_patch_test::attach_custom_boundary_and_config` | 0 | 邮件正文中自第二个分隔行 `--------------mimemime` 起至终止分隔行（含）的区段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（含 `Content-Type: text/x-patch`、`Content-Disposition: attachment` 与 diff） |
| AC7 | `command::format_patch_test::attach_custom_boundary_and_config` | 0 | `Subject:` 行序列与同夹具 `git format-patch --attach --stdout HEAD~1` 的黄金值逐字一致（只含 1 封补丁） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--attach=` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::attach_custom_boundary_and_config)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::attach_custom_boundary_and_config`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-33`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--attach=`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--attach=<boundary>/--inline=<boundary>; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-33; writeset=序列化于FPG-33; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-38: 支持 `format.attach` 配置与 `--no-attach`（#912；自 FPG-18 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：读取 `format.attach`——非空值（含字面 `true`）作为 boundary、空值关闭；本地空值覆盖全局；新增 `--no-attach` 覆盖配置与 `--attach`/`--inline`。AC1–AC5 由 `format_patch_test::format_attach_config` 覆盖（true 字面 boundary、自定义、空值关闭、全局+本地空值、--no-attach）；`format_patch_test` 168/168、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过（含新 flag）；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；无值配置形式仍未排期。

**Description:** 自 `FPG-18` 拆出的独立轴：经 `read_format_config` 严格级联读取 `format.attach`，按 Git 语义：非空值（含字面 `true`）作为 boundary，空值关闭，本地空值覆盖全局值；新增 `--no-attach` 覆盖配置。Git 的无值形式（等价默认 boundary）在 Libra 配置中不可表达（尚未排期，重启条件：配置支持无值键）。

**Out of scope:** `--attach=<boundary>`/`--inline=<boundary>` 命令行取值（由 `FPG-18` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_format_config` 未读取 `format.attach`；`FormatPatchArgs` 无 `--no-attach` | `src/command/format_patch.rs:97-364,1861-1902` |

**Acceptance criteria:**

- [ ] AC1：`format.attach=true`（无 flag）按 Git 把 `true` 当作字面 boundary。
- [ ] AC2：`format.attach=attachment-separator` 使用该 boundary（#912 第 1 项）。
- [ ] AC3：`format.attach=""` 不生成 multipart（#912 第 2 项）。
- [ ] AC4：隔离的全局 `format.attach=attachment-separator` 与本地 `format.attach=""` 并存时本地空值生效（#912 第 2 项；全局库经临时 `LIBRA_CONFIG_GLOBAL_DB` 隔离，GC-13）。
- [ ] AC5：`format.attach=true` 下 `--no-attach` 输出纯文本补丁。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_attach_config` | 0 | 存在逐字为 `Content-Type: multipart/mixed; boundary="------------true"` 的行（与同夹具 Git 黄金值一致） |
| AC2 | `command::format_patch_test::format_attach_config` | 0 | 存在逐字为 `Content-Type: multipart/mixed; boundary="------------attachment-separator"` 的行 |
| AC3 | `command::format_patch_test::format_attach_config` | 0 | 输出不存在含 `multipart/` 的行 |
| AC4 | `command::format_patch_test::format_attach_config` | 0 | 输出不存在含 `multipart/` 的行 |
| AC5 | `command::format_patch_test::format_attach_config` | 0 | 输出不存在含 `multipart/` 的行 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.attach`、`--no-attach` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_attach_config)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_attach_config`；证明 AC1–AC5）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-18`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.attach`、`--no-attach`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.attach配置与--no-attach; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-18; writeset=序列化于FPG-18; release=independent; split-from=FPG-18; exception=N/A`

---

### Task FPG-19: 实现多补丁 `--interdiff=<rev>` 封面信输出（#898 多补丁部分；自本卡拆出 FPG-34）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：新增多补丁 `--interdiff=<REV>`（空格形式亦可），封面信 diffstat 后输出 `Interdiff against v<n-1>:`（无 `-v` 为 `Interdiff:`）及 `<REV>` 树与末补丁树的统一 diff；未显式关闭封面信时隐含封面信；非法 revision `LBR-CLI-003`。AC1–AC6 由 `format_patch_test::interdiff_multi_patch_cover_letter` 覆盖（AC1–AC5 封面 diffstat 后段与同源 Git 逐字、AC6 非法 revision）；`format_patch_test` 169/169、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过（含新 flag）；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；单补丁位置由 `FPG-34` 承接。

**Description:** 在 `529 FP-10` 已落地的封面信三态与前置诊断基础上，保留其 U 型拒绝（多补丁且封面信被显式禁用），把多补丁场景的 X 型「不支持」分类（`fp10_interdiff_implied_cover`、`fp10_interdiff_spaced_value`）替换为实际 series interdiff 输出：按 `git@6de20f6092` 语义直接对比 `<rev>` 的树与当前系列末个提交的树（`builtin/log.c` 令 `idiff_oid1 = <rev>`、`idiff_oid2` 为末个提交的树，`diff-lib.c` `show_interdiff` 以 `diff_tree_oid` 求差；R5 曾误改为两系列各自组合变更之差，R25 按上游源码更正），在封面信中输出 `Interdiff:` 区块（有 `-v` 时为 Git 的 `Interdiff against v<n-1>:`）。单补丁位置由自本卡拆出的 `FPG-34` 承接。 `#898` 第 3 项（`--no-cover-letter` 与 `format.coverLetter=no` 时拒绝）由 `issues/529#FP-10` 的 U 型门族交付并在 529 计划内验收，本卡不改变其契约，其门族在本卡共享回归组中运行（不计本卡 AC）；不带 `--stdout` 时 `v23-0000-cover-letter.patch` 的文件输出组合由 `FPG-20` 验收。 夹具为差异隔离夹具：被比较的文件在 `main` 上已存在，新旧系列只修改其既有行（使 interdiff 不含空旧侧 hunk，见「验证执行协议」第 3 项）。

**Out of scope:** 单补丁（`total = 1`）`--interdiff` 输出位置（由 `FPG-34` 承接，本卡保持 `529 FP-10` 对单补丁的既有分类不变）；`--range-diff`（永久非目标，见 `DEFER-FPG-01`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/529.md` `FP-10` 仅做前置条件诊断并显式归类 `--interdiff` 为不支持 | `issues/529.md:42,165`、`COMPATIBILITY.md:224` |
| `529 FP-10` 计划交付 U 型门族 `fp10_interdiff_needs_cover__*`、`fp10_config_no_interdiff_needs_cover__*`（本卡保留）与 X 型门族 `fp10_interdiff_implied_cover__*`、`fp10_interdiff_spaced_value__*`（行为迁移：原门断言拒绝契约，路径改为成功后无仍适用断言，整组删除并由 AC1、AC4 新成功用例替代）；开工时按 GC-01 用 `cargo nextest list` 核实名称，缺失即 STOP 并按 ER-03 修订本卡 | `docs/development/plan/issues/529.md:1798-1806` |
| Git `--interdiff` 直接对 `<rev>` 与当前系列末个提交的树求差 | `git show 6de20f6092:builtin/log.c` 第 2383–2385、1459–1461 行；`git show 6de20f6092:diff-lib.c` 第 722–740 行 |

**Acceptance criteria:**

- [ ] AC1：未给封面信开关的 `--interdiff=<v22 系列> -2 -v23 --stdout` 隐含封面信并输出 `Interdiff against v22:`（#898 第 1 项；替代 `fp10_interdiff_implied_cover` X 型拒绝）。
- [ ] AC2：`--cover-letter --interdiff=<v1 系列> -2 -v2 --stdout` 输出 `Interdiff against v1:`（#898 第 2 项）。
- [ ] AC3：`format.coverLetter=false` 下 `--cover-letter --interdiff=<v1 系列> -2 -v2 --stdout` 正常输出。
- [ ] AC4：空格形式 `--cover-letter --interdiff <v1 系列> -2 -v2 --stdout` 正常输出（替代 `fp10_interdiff_spaced_value` X 型拒绝）。
- [ ] AC5：不带 `-v` 的 `--cover-letter --interdiff=<v1 系列> -2 --stdout` 输出 `Interdiff:` 区块。
- [ ] AC6：`--cover-letter --interdiff=no-such-rev -2 --stdout` 被拒绝。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::interdiff_multi_patch_cover_letter` | 0 | 封面信 diffstat 后段与同夹具 `git@6de20f6092` 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（同时判定标题、内容及其位于 diffstat 之后）；夹具：新旧系列基于不同 base，且两 tip 树之差与「两系列各自相对 base 的组合变更之差」不同，用以区分错误的系列级求差实现（夹具前置断言，不计 AC） |
| AC2 | `command::format_patch_test::interdiff_multi_patch_cover_letter` | 0 | 封面信 diffstat 后段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC3 | `command::format_patch_test::interdiff_multi_patch_cover_letter` | 0 | 封面信 diffstat 后段与 AC2 的 Libra 输出逐字节相同 |
| AC4 | `command::format_patch_test::interdiff_multi_patch_cover_letter` | 0 | 封面信 diffstat 后段与 AC2 的 Libra 输出逐字节相同 |
| AC5 | `command::format_patch_test::interdiff_multi_patch_cover_letter` | 0 | 封面信 diffstat 后段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC6 | `command::format_patch_test::interdiff_invalid_rev_rejected` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-003`，且 stderr 含 Libra 区分短语 `no-such-rev`；stdout 为空 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--interdiff` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::interdiff_multi_patch_cover_letter)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::interdiff_multi_patch_cover_letter`；证明 AC1–AC5）
- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::interdiff_invalid_rev_rejected)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::interdiff_invalid_rev_rejected`；证明 AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-38`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--interdiff`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=多补丁--interdiff封面信输出; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-38; writeset=序列化于FPG-38; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-34: 单补丁 `--interdiff=<rev>` 输出位置（#898 单补丁部分；自 FPG-19 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：单补丁无封面信时 interdiff 位于补丁 diff 之后、签名之前（空行+标题+两空格缩进 diff）；有封面信时只在封面信、补丁不重复；单补丁不再隐含封面信。AC1–AC5 由 `format_patch_test::interdiff_single_patch_placement` 覆盖（补丁 diff 段与同源 Git 逐字、--no-cover-letter、config false、封面区块、补丁无重复）；`format_patch_test` 170/170、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-19` 拆出的独立轴：单补丁系列（`total = 1`）把 `529 FP-10` 的 X 型「不支持」分类（`fp10_interdiff_single_patch` 及单补丁显式启用封面信的组合）替换为实际输出：无封面信时（含被 `--no-cover-letter` 或 `format.coverLetter=false` 禁用）`Interdiff:` 区块位于补丁 diff 之后、签名分隔 `-- ` 之前——先输出一个空行，再输出标题行（整数 `-v` 时为 `Interdiff against v<n-1>:`），其后 interdiff 每行缩进两个空格（`git show 6de20f6092:log-tree.c` 第 695–711、1299–1300 行；t4014「interdiff: solo-patch」）；启用封面信时 interdiff 只出现在封面信、补丁中不重复（`git show 6de20f6092:builtin/log.c` 第 2465–2467 行；t4014「interdiff: cover-letter」）。夹具同 t4014「interdiff: setup」并改为差异隔离形式：`blorp` 在 `main` 上已存在，`boop` 分支依次提交 `fnorp`、`fleep`，均只修改 `blorp` 的既有行；参数 `--interdiff=boop~2 -1 boop`；夹具前置断言 interdiff 文本不同于补丁 diff 文本（不计 AC）。

**Out of scope:** 多补丁封面信 interdiff（由 `FPG-19` 承接）；`--range-diff`（永久非目标，见 `DEFER-FPG-01`）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `529 FP-10` 把单补丁且显式启用封面信的 `--interdiff` 也归为 X 型拒绝；本卡 AC4 按行为迁移替换（开工时按 GC-01 核实门名） | `docs/development/plan/issues/529.md:306` |
| `529 FP-10` 计划把单补丁 `--no-cover-letter --interdiff=HEAD~1 -1` 归为 X 型「不支持」（`fp10_interdiff_single_patch`）；行为迁移：路径改为成功后原拒绝断言无仍适用者，整组删除并由 AC2 新成功用例替代；开工时按 GC-01 核实名称 | `docs/development/plan/issues/529.md:1804` |

**Acceptance criteria:**

- [ ] AC1：单补丁 `--interdiff=boop~2 -1 boop --stdout --signature=SIG`（无封面信）在补丁 diff 之后、签名分隔 `-- ` 之前输出 interdiff。
- [ ] AC2：单补丁 `--no-cover-letter --interdiff=boop~2 -1 boop --stdout --signature=SIG` 同 AC1（替代 `fp10_interdiff_single_patch` X 型拒绝）。
- [ ] AC3：`format.coverLetter=false` 下单补丁 `--interdiff=boop~2 -1 boop --stdout --signature=SIG` 同 AC1。
- [ ] AC4：`--cover-letter --interdiff=boop~2 -1 boop --stdout` 的 interdiff 输出在封面信中（t4014「interdiff: cover-letter」）。
- [ ] AC5：同一调用中补丁邮件不重复输出 interdiff。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::interdiff_single_patch_placement` | 0 | 补丁 diff 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --signature=SIG`；本映射启用签名，该区段自首个 `diff --git` 至邮件结束、含签名块，同时判定 interdiff 的内容、两空格缩进及其位于补丁 diff 之后、`-- ` 签名分隔之前） |
| AC2 | `command::format_patch_test::interdiff_single_patch_placement` | 0 | 补丁 diff 段与 AC1 的 Libra 输出逐字节相同 |
| AC3 | `command::format_patch_test::interdiff_single_patch_placement` | 0 | 补丁 diff 段与 AC1 的 Libra 输出逐字节相同 |
| AC4 | `command::format_patch_test::interdiff_single_patch_placement` | 0 | 封面信 diffstat 后段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`） |
| AC5 | `command::format_patch_test::interdiff_single_patch_placement` | 0 | 补丁邮件（不含封面信）中不存在以 `Interdiff` 开头的行 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--interdiff` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::interdiff_single_patch_placement)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::interdiff_single_patch_placement`；证明 AC1–AC5）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-19`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--interdiff`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=单补丁--interdiff输出位置; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-19; writeset=序列化于FPG-19; release=independent; split-from=FPG-19; exception=N/A`

---

### Task FPG-20: reroll 文件名 `vN-` 前缀与 `Subject:` 版本一致（#901 第 1、3、4 项；自本卡拆出 FPG-26、FPG-45）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--reroll-count` 为输出文件名加 `v<val>-` 前缀（含封面信），与 `Subject:` 的 `[PATCH v4 n/m]` 版本一致；文件输出携带完整 interdiff 区块。AC1–AC6 由 `format_patch_test::reroll_filename_prefix` 覆盖（v4 文件名集合、封面/补丁 Subject 版本、文件输出 interdiff 与同源 Git 逐字）；`format_patch_test` 171/171、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 在 `--reroll-count`（`-v`）时于输出文件名前添加 `v<val>-` 前缀，并保证每封邮件的 `Subject:` 版本与文件名前缀一致（自本卡拆出 `FPG-26` 处理非整数 `--reroll-count`、`FPG-45` 处理生成路径列表输出到 stdout）。

**Out of scope:** 非整数 `--reroll-count`（由 `FPG-26` 承接）；生成路径列表输出到 stdout（由 `FPG-45` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `patch_filename` 未拼接 `vN-` 前缀；文件列表使用 `eprintln!` 输出到 stderr | `src/command/format_patch.rs:610-612,1699-1737` |

**Acceptance criteria:**

- [ ] AC1：`-v 4 -2 -o out` 生成带 `v4-` 前缀的补丁文件（#901 第 1 项）。
- [ ] AC2：`-v 4 --cover-letter -2 -o out` 生成 `v4-0000-cover-letter.patch`。
- [ ] AC3：`-v 4 --cover-letter -2 -o out` 的封面信 `Subject:` 版本与文件名前缀一致（#901 第 4 项）。
- [ ] AC4：同一场景 `v4-0001-…` 的 `Subject:` 版本与文件名前缀一致（#901 第 4 项）。
- [ ] AC5：同一场景 `v4-0002-…` 的 `Subject:` 版本与文件名前缀一致（#901 第 4 项）。
- [ ] AC6：不带 `--stdout` 的 `--interdiff=<v22 系列> -2 -v23 -o out` 写出的 `v23-0000-cover-letter.patch` 含完整 interdiff 区块（标题与差异正文，#898 第 1 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::reroll_filename_prefix` | 0 | `out/` 文件名集合恰为 `v4-0001-<slug>.patch`、`v4-0002-<slug>.patch` |
| AC2 | `command::format_patch_test::reroll_filename_prefix` | 0 | `out/` 文件名集合含 `v4-0000-cover-letter.patch` |
| AC3 | `command::format_patch_test::reroll_filename_prefix` | 0 | `out/v4-0000-cover-letter.patch` 的 `Subject:` 行以 `Subject: [PATCH v4 0/2] ` 开头 |
| AC4 | `command::format_patch_test::reroll_filename_prefix` | 0 | `out/` 中以 `v4-0001-` 开头的补丁文件的 `Subject:` 行以 `Subject: [PATCH v4 1/2] ` 开头 |
| AC5 | `command::format_patch_test::reroll_filename_prefix` | 0 | `out/` 中以 `v4-0002-` 开头的补丁文件的 `Subject:` 行以 `Subject: [PATCH v4 2/2] ` 开头 |
| AC6 | `command::format_patch_test::reroll_filename_prefix` | 0 | `out/v23-0000-cover-letter.patch` 的封面信 diffstat 后段与同夹具 `git@6de20f6092` 黄金值逐字一致（双方均加 `--no-thread --no-signature`；夹具同 `FPG-19` AC1：新旧系列基于不同 base、两 tip 树之差非空，同时判定 `Interdiff against v22:` 标题、差异正文及其位于 diffstat 之后） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（`vN-` 文件名前缀与 `Subject:` 版本说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::reroll_filename_prefix)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::reroll_filename_prefix`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-34`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：`vN-` 文件名前缀与 `Subject:` 版本说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=reroll_vN-文件名前缀与Subject版本; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-34; writeset=序列化于FPG-34; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-45: 生成文件路径列表输出到 stdout（#901 第 2 项；自 FPG-20 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：非 `--stdout`/`--json`/全局 `--quiet` 时生成路径逐行写 stdout；绝对 `-o` 绝对路径、相对 `-o` 相对 cwd、无 `-o` 裸文件名；`--quiet`/`--json` 抑制。AC1–AC6 由 `format_patch_test::generated_paths_on_stdout` 覆盖（相对/绝对/裸文件名、stderr 无路径、quiet、JSON-only）；`format_patch_test` 172/172、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-20` 拆出的独立轴：非 `--stdout`、非 `--json`、非全局 `--quiet` 时把生成的文件路径逐行写到 stdout（不再写 stderr），`--quiet` 抑制，`--json` 只输出 JSON。 打印形式按 Git `set_outdir`/`outdir_offset`（`git show 6de20f6092:builtin/log.c` 第 1140–1158、1510–1527 行）：绝对 `-o` 打印绝对路径；相对 `-o` 打印相对当前工作目录的路径；未指定输出目录时只打印文件名；现行 `resolve_output_dir` 以 `current_dir()` 拼出绝对路径（`src/command/format_patch.rs:1772-1789`）。

**Out of scope:** 文件名前缀（由 `FPG-20` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 文件列表使用 `eprintln!` 输出到 stderr | `src/command/format_patch.rs:610-612` |

**Acceptance criteria:**

- [ ] AC1：在仓库根以 `-2 -o out`（非 `--stdout`、非 `--json`、非全局 `--quiet`）运行时生成路径逐行输出到 stdout（#901 第 2 项）。
- [ ] AC2：同一场景 stderr 不再输出路径列表。
- [ ] AC3：全局 `--quiet -2 -o out` 时不输出路径。
- [ ] AC4：`--json -2 -o out` 时 stdout 只含 JSON。
- [ ] AC5：在仓库根不带 `-o` 且未配置 `format.outputDirectory` 运行 `-2` 时只打印文件名（t4014 第 1090–1097 行）。
- [ ] AC6：`-2 -o <绝对路径>` 时按 Git 打印绝对路径。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::generated_paths_on_stdout` | 0 | stdout 行序列逐字为 `out/0001-<slug1>.patch`、`out/0002-<slug2>.patch`（slug 在测试中写成常量，与同夹具 Git 黄金值一致） |
| AC2 | `command::format_patch_test::generated_paths_on_stdout` | 0 | stderr 不存在等于任一生成文件路径的行 |
| AC3 | `command::format_patch_test::generated_paths_on_stdout` | 0 | stdout 为空 |
| AC4 | `command::format_patch_test::generated_paths_on_stdout` | 0 | stdout 整体可用 `serde_json::from_slice::<serde_json::Value>` 解析成功（不混入纯文本路径行） |
| AC5 | `command::format_patch_test::generated_paths_on_stdout` | 0 | stdout 行序列逐字为 `0001-<slug1>.patch`、`0002-<slug2>.patch`（与同夹具 Git 黄金值一致） |
| AC6 | `command::format_patch_test::generated_paths_on_stdout` | 0 | stdout 行序列逐字为 `<绝对路径>/0001-<slug1>.patch`、`<绝对路径>/0002-<slug2>.patch`（与同夹具 Git 黄金值一致） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（生成路径列表写到 stdout 的说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::generated_paths_on_stdout)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::generated_paths_on_stdout`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-20`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：生成路径列表写到 stdout 的说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=生成路径列表输出到stdout; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-20; writeset=序列化于FPG-20; release=independent; split-from=FPG-20; exception=N/A`

---

### Task FPG-26: 支持非整数与非常规 `--reroll-count` 值的文件名前缀（#903 第 1–3 项；自本卡拆出 FPG-52）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-20` 拆出的独立轴：将 `FormatPatchArgs::reroll_count` 从 `Option<usize>` 改为字符串，按 Git 接受小数与非数字 reroll 标记（`--reroll-count=4.4`、`-v4rev2`、`-v4---..././../--1/.2//`），文件名前缀使用 Git 的清洗结果（路径分隔符等被替换，文件始终写在输出目录内）。`Subject:` 原样字符串与 interdiff 标题由自本卡拆出的 `FPG-52` 承接。

**Out of scope:** `Subject:` 版本字符串与非整数版本的 interdiff 标题（由 `FPG-52` 承接）；生成路径列表输出到 stdout（由 `FPG-45` 承接，本卡依赖其先行完成）；`vN-` 文件名前缀基础实现（由 `FPG-20` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `reroll_count: Option<usize>` 拒绝非整数值 | `src/command/format_patch.rs:220-226` |

**Acceptance criteria:**

- [ ] AC1：`-o p --cover-letter --reroll-count=4.4 -1`（长选项）的文件名带 `v4.4-` 前缀（#903 第 1 项；短选项 `-v4.4` 见 `FPG-52` AC4–AC6）。
- [ ] AC2：`-v4rev2 -o p -1` 的文件名带 `v4rev2-` 前缀（#903 第 2 项）。
- [ ] AC3：`-v4---..././../--1/.2// -o p -1` 按 Git 清洗文件名前缀且只写入 `p/`（#903 第 3 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::reroll_non_integer` | 0 | `p/` 文件名集合恰为 `v4.4-0000-cover-letter.patch`、`v4.4-0001-<slug>.patch` |
| AC2 | `command::format_patch_test::reroll_non_integer` | 0 | `p/` 唯一文件名以 `v4rev2-0001-` 开头 |
| AC3 | `command::format_patch_test::reroll_non_integer` | 0 | `p/` 文件名集合与同夹具 Git 黄金值逐项相等（前缀为 `v4-.-.-.-1-.2-`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（非整数与非常规 `-v` 取值及文件名清洗说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::reroll_non_integer)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::reroll_non_integer`；证明 AC1–AC3）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-45`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：非整数与非常规 `-v` 取值及文件名清洗说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=非整数reroll文件名前缀; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-45; writeset=序列化于FPG-45; release=independent; split-from=FPG-20; exception=N/A`

---

### Task FPG-52: 非整数与非常规 `--reroll-count` 的 `Subject:` 与 interdiff 标题（#903 第 2、4 项；自 FPG-26 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-26` 拆出的独立轴：`Subject:` 使用命令行给出的原样 reroll 字符串（不清洗、不补零），非整数版本的 interdiff 标题按 Git `diff_title` 为不带版本的 `Interdiff:`。 #898 Expected behavior 第 2 项称 `-v2.2` → `Interdiff against v1.2:`，与 `git show 6de20f6092:builtin/log.c` 第 1915–1928 行 `diff_title`（非整数版本输出 `Interdiff:`）不符；本计划以上游为准，并在关闭 #898 时于 issue 中说明。

**Out of scope:** 字符串化 `reroll_count` 与文件名前缀清洗（由 `FPG-26` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| Git `diff_title` 在 reroll 串不能按整数解析时使用通用标题 | `git show 6de20f6092:builtin/log.c` 第 1915–1927 行（`diff_title`） |
| `reroll_count: Option<usize>` 拒绝非整数值；`Subject:` 前缀由数值版本拼接 | `src/command/format_patch.rs:220-226,836-851,1377-1381` |

**Acceptance criteria:**

- [ ] AC1：`-v4---..././../--1/.2// -o p -1` 的 `Subject:` 保留原样字符串（#903 第 4 项）。
- [ ] AC2：`-v4rev2 -o p -1` 的 `Subject:` 保留原样 reroll 字符串（#903 第 2、4 项）。
- [ ] AC3：`--cover-letter --interdiff=<v1 系列> -2 -v2.2 --stdout` 的 interdiff 标题按 Git 为不带版本的 `Interdiff:`（非整数版本不递减；t4014「interdiff: reroll-count with a non-integer」）。
- [ ] AC4：`-o p --cover-letter -v4.4 -2` 的封面信 `Subject:` 版本与文件名前缀一致（#903 分数版本上游场景）。
- [ ] AC5：同一场景 `v4.4-0001-…` 的 `Subject:` 版本与文件名前缀一致。
- [ ] AC6：同一场景 `v4.4-0002-…` 的 `Subject:` 版本与文件名前缀一致。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::reroll_non_integer_subjects` | 0 | `p/` 唯一文件的 `Subject:` 行以 `Subject: [PATCH v4---..././../--1/.2//] ` 开头 |
| AC2 | `command::format_patch_test::reroll_non_integer_subjects` | 0 | `p/` 唯一文件的 `Subject:` 行以 `Subject: [PATCH v4rev2] ` 开头 |
| AC3 | `command::format_patch_test::reroll_non_integer_subjects` | 0 | 封面信中以 `Interdiff` 开头的行恰为一行且逐字为 `Interdiff:`（单一行级判定） |
| AC4 | `command::format_patch_test::reroll_non_integer_subjects` | 0 | `p/v4.4-0000-cover-letter.patch` 的 `Subject:` 行以 `Subject: [PATCH v4.4 0/2] ` 开头 |
| AC5 | `command::format_patch_test::reroll_non_integer_subjects` | 0 | `p/` 中以 `v4.4-0001-` 开头的补丁文件的 `Subject:` 行以 `Subject: [PATCH v4.4 1/2] ` 开头 |
| AC6 | `command::format_patch_test::reroll_non_integer_subjects` | 0 | `p/` 中以 `v4.4-0002-` 开头的补丁文件的 `Subject:` 行以 `Subject: [PATCH v4.4 2/2] ` 开头 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（非整数 reroll 的 `Subject:` 与 interdiff 标题说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::reroll_non_integer_subjects)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::reroll_non_integer_subjects`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-26`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：非整数 reroll 的 `Subject:` 与 interdiff 标题说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=非整数reroll的Subject与interdiff标题; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-26; writeset=序列化于FPG-26; release=independent; split-from=FPG-26; exception=N/A`

---

### Task FPG-21: 补丁文件名按 Git `format_sanitized_subject` 生成并保留大小写（#908；自本卡拆出 FPG-40）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 Git `format_sanitized_subject`（`git show 6de20f6092:pretty.c` 第 941–975 行）生成文件名 slug：保留大小写，只保留 ASCII 字母数字与 `.`、`_`（连续 `.` 压缩为一个），其余字符（含非 ASCII）视为分隔并以单个 `-` 连接，去掉首尾分隔与末尾 `.`/`-`；现行 `patch_slug` 转小写、把 `.`/`_` 换成 `-` 且保留 Unicode 字母数字（`src/command/format_patch.rs:1646-1667`）。#908 正文称「其余清洗一致」，与上游清洗规则不符，本计划以上游为准并在关闭 #908 时说明。文件名长度上限由自本卡拆出的 `FPG-40` 承接。

**Out of scope:** 文件名长度上限与 `format.filenameMaxLength`（由 `FPG-40` 承接）；`--numbered-files` 的纯数字命名（不变）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `patch_slug` 调用 `.to_lowercase()`；`truncate_slug` 硬编码 `MAX_CHARS = 52` | `src/command/format_patch.rs:1646-1681` |

**Acceptance criteria:**

- [ ] AC1：t4014「multiple files」夹具的文件名保留大小写（#908 第 1 项）。
- [ ] AC2：`Mixed CASE Subject` 的文件名保留大小写（#908 第 2 项）。
- [ ] AC3：`ALL CAPS` 的文件名保留大小写（#908 第 2 项）。
- [ ] AC4：标题 `Update README..md v1.2_x.` 的文件名按 Git 保留 `.` 与 `_`、把连续 `.` 压缩为一个并去掉末尾 `.`。
- [ ] AC5：标题 `Fix café crash` 的文件名按 Git 丢弃非 ASCII 字符。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::filename_git_sanitized_slug` | 0 | 输出目录文件名集合与同夹具 Git 黄金值逐项相等（如 `0001-Side-changes-1.patch`） |
| AC2 | `command::format_patch_test::filename_git_sanitized_slug` | 0 | 文件名逐字为 `0001-Mixed-CASE-Subject.patch` |
| AC3 | `command::format_patch_test::filename_git_sanitized_slug` | 0 | 文件名逐字为 `0001-ALL-CAPS.patch` |
| AC4 | `command::format_patch_test::filename_git_sanitized_slug` | 0 | 文件名逐字为 `0001-Update-README.md-v1.2_x.patch`（与同夹具 Git 黄金值一致；不压缩连续 `.` 或不去末尾 `.` 的实现得到不同文件名） |
| AC5 | `command::format_patch_test::filename_git_sanitized_slug` | 0 | 文件名逐字为 `0001-Fix-caf-crash.patch`（与同夹具 Git 黄金值一致） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（文件名保留大小写说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::filename_git_sanitized_slug)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::filename_git_sanitized_slug`；证明 AC1–AC5）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-52`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：文件名保留大小写说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=文件名按Git清洗并保留大小写; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-52; writeset=序列化于FPG-52; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-40: 按 `--filename-max-length`/`format.filenameMaxLength` 控制文件名长度（#910；自 FPG-21 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-21` 拆出的独立轴：移除 `truncate_slug` 的 52 字符硬编码截断，使默认上限（Git 64）、显式 `--filename-max-length` 与 `format.filenameMaxLength` 按 Git 规则截断整个文件名。

**Out of scope:** 文件名大小写（由 `FPG-21` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `truncate_slug` 硬编码 `MAX_CHARS = 52`；`patch_filename` 只在显式上限时按字节截断 | `src/command/format_patch.rs:1672-1681,1699-1751` |
| 既有 `filename_max_length_truncates_like_git` 覆盖 10/12/15/20/30（行为迁移时保留仍适用断言） | `tests/command/format_patch_test.rs:2199` |

**Acceptance criteria:**

- [ ] AC1：t4014「excessive subject」夹具（第 4 个提交为超长标题）加 `--filename-max-length=100` 与 Git 一致（#910 第 1 项）。
- [ ] AC2：t4014「excessive subject」夹具（第 4 个提交为超长标题）未传 flag 与配置时按默认 64 截断（#910 第 2 项）。
- [ ] AC3：t4014「excessive subject」夹具（第 4 个提交为超长标题）加显式 `--filename-max-length=64` 同默认（#910 第 2 项）。
- [ ] AC4：t4014「excessive subject」夹具（第 4 个提交为超长标题）加 `--filename-max-length=15` 与 Git 一致（#910 第 3 项）。
- [ ] AC5：t4014「excessive subject」夹具（第 4 个提交为超长标题）加 `--filename-max-length=25` 与 Git 一致（#910 第 3 项）。
- [ ] AC6：t4014「excessive subject」夹具（第 4 个提交为超长标题）加 `--filename-max-length=35` 与 Git 一致（#910 第 3 项）。
- [ ] AC7：t4014「excessive subject」夹具（第 4 个提交为超长标题）配置 `format.filenameMaxLength=40`（无 flag）同 `--filename-max-length=40`。
- [ ] AC8：t4014「excessive subject」夹具加 `-v4`（无长度 flag 与配置）时 `v4-` 前缀计入默认 64 字节预算（`git show 6de20f6092:log-tree.c` 第 447–461 行）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::filename_max_length_git_parity` | 0 | 第 4 个文件名与同夹具 Git 黄金值逐字一致 |
| AC2 | `command::format_patch_test::filename_max_length_git_parity` | 0 | 第 4 个文件名逐字为 `0004-This-is-an-excessively-long-subject-line-for-a-messa.patch` |
| AC3 | `command::format_patch_test::filename_max_length_git_parity` | 0 | 第 4 个文件名逐字为 `0004-This-is-an-excessively-long-subject-line-for-a-messa.patch` |
| AC4 | `command::format_patch_test::filename_max_length_git_parity` | 0 | 第 4 个文件名与同夹具 Git 黄金值逐字一致 |
| AC5 | `command::format_patch_test::filename_max_length_git_parity` | 0 | 第 4 个文件名与同夹具 Git 黄金值逐字一致 |
| AC6 | `command::format_patch_test::filename_max_length_git_parity` | 0 | 第 4 个文件名与同夹具 Git 黄金值逐字一致 |
| AC7 | `command::format_patch_test::filename_max_length_git_parity` | 0 | 第 4 个文件名与该 CLI 场景的 Libra 输出逐字相同 |
| AC8 | `command::format_patch_test::filename_max_length_git_parity` | 0 | 第 4 个文件名逐字为 `v4-0004-This-is-an-excessively-long-subject-line-for-a-me.patch` |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.filenameMaxLength` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::filename_max_length_git_parity)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::filename_max_length_git_parity`；证明 AC1–AC8）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-21`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.filenameMaxLength`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=文件名长度上限与format.filenameMaxLength; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-21; writeset=序列化于FPG-21; release=independent; split-from=FPG-21; exception=N/A`

---

### Task FPG-22: 支持 `format-patch` 的 `-U`/`--unified` 选项（#904 部分；自本卡拆出 FPG-27、FPG-61）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：新增 `-U<n>`/`--unified=<n>`；`FormatPatchArgs.unified` 经 `CommitDiffOptions.context_lines`（默认 3）与父提交路径的 `DiffArgs --unified=<n>` 透传到共享 diff 渲染引擎；`CommitDiffOptions::default()` 的 histogram/full_index/prefix 字段不变。AC1/AC2 由 `format_patch_test::unified_context_lines` 覆盖（`-U1`、`--unified=5` 与 Git 黄金 diff 段逐字且两两不同），AC3–AC7 由 `log::commit_diff_options_tests::defaults_keep_three_context_lines_and_prefixes` 覆盖；`format_patch_test` 134/134。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 在 `FormatPatchArgs` 新增 `-U<n>`/`--unified=<n>`，经 `log::CommitDiffOptions` 透传到 `src/command/diff/` 渲染引擎控制上下文行数。自本卡拆出 `FPG-27` 处理 `--diff-filter` 与重命名核验、`FPG-61` 处理 `--full-diff`。

**Out of scope:** `--diff-filter`（由 `FPG-27` 承接）；`--full-diff`（由 `FPG-61` 承接）；`-M`/`--find-renames`/`--no-renames` 核验与 #904 封面信项（由 `FPG-39` 承接）；`format-patch -- <path>` pathspec 限制提交及其与 `--full-diff` 的组合（尚未排期，重启条件：新 issue 要求 pathspec）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `src/command/diff/` 目录现有 `compare.rs`、`options.rs`、`render.rs`；本卡只预计改 `render.rs`，开工按 GC-01 核对，触及清单外文件须先按 ER-03 修订写集 | `src/command/diff/` |
| `FormatPatchArgs` 无 `-U`/`--unified`；`log::CommitDiffOptions` 仅含 `histogram`/`full_index`/`source_prefix`/`destination_prefix` 4 字段（定义与 `Default` 在 `log.rs`，`format_patch.rs` 的 `commit_diff_options` 负责构造） | `src/command/format_patch.rs:258-284,1852-1859`、`src/command/log.rs:4254-4270` |
| 上下文重写 `rewrite_unified_diff_context` 为 `src/command/diff.rs` 私有函数，复用须改该文件 | `src/command/diff.rs:4267` |

**Acceptance criteria:**

- [ ] AC1：`-U1` 控制 hunk 上下文为 1 行。
- [ ] AC2：`--unified=5` 控制 hunk 上下文为 5 行（与 AC1 同一夹具）。
- [ ] AC3：`CommitDiffOptions::default()` 新增的上下文行数字段为 3。
- [ ] AC4：`CommitDiffOptions::default()` 的 `histogram` 仍为 `false`。
- [ ] AC5：`CommitDiffOptions::default()` 的 `full_index` 仍为 `false`。
- [ ] AC6：`CommitDiffOptions::default()` 的 `source_prefix` 仍为 `a/`。
- [ ] AC7：`CommitDiffOptions::default()` 的 `destination_prefix` 仍为 `b/`。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::unified_context_lines` | 0 | 补丁 diff 段与同夹具 Git `-U1` 黄金输出逐字一致（双方均加 `--no-thread --no-signature`）；夹具：单文件 20 行只改第 10 行，Git 的 `-U1`、默认与 `--unified=5` 三种黄金 diff 两两不同（夹具前置断言，不计 AC） |
| AC2 | `command::format_patch_test::unified_context_lines` | 0 | 补丁 diff 段与同夹具 Git `--unified=5` 黄金输出逐字一致（双方均加 `--no-thread --no-signature`） |
| AC3 | `command::log::tests::commit_diff_options_default_is_backward_compatible`（lib） | 0（单元测试） | 默认值的上下文行数字段等于 3 |
| AC4 | `command::log::tests::commit_diff_options_default_is_backward_compatible`（lib） | 0（单元测试） | 默认值的 `histogram` 字段等于 `false` |
| AC5 | `command::log::tests::commit_diff_options_default_is_backward_compatible`（lib） | 0（单元测试） | 默认值的 `full_index` 字段等于 `false` |
| AC6 | `command::log::tests::commit_diff_options_default_is_backward_compatible`（lib） | 0（单元测试） | 默认值的 `source_prefix` 字段等于 `"a/"` |
| AC7 | `command::log::tests::commit_diff_options_default_is_backward_compatible`（lib） | 0（单元测试） | 默认值的 `destination_prefix` 字段等于 `"b/"`；`log`/`show`/`stash` 调用方由共享回归组覆盖 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--unified` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::unified_context_lines)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::unified_context_lines`；证明 AC1–AC2）
- [ ] `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=command::log::tests::commit_diff_options_default_is_backward_compatible)'`（`(new)`；具名验收门，期望集合 N=1：`command::log::tests::commit_diff_options_default_is_backward_compatible`（lib）；证明 AC3–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）
- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::(log|show|stash|diff)_test::/)'`（`log`、`show`、`stash` 调用方与 `src/command/diff/` 渲染；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-40`, `DEP-FPG-02`（核实 `src/command/diff/` 无在途写者）, `DEP-FPG-03`（核实 `src/command/log.rs` 无在途写者）

**Implementation write set:** `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--unified`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=-U上下文行数透传; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=2/8; landing=3; prod-files=4; scope=M; deps=FPG-40,DEP-FPG-02,DEP-FPG-03; writeset=序列化于FPG-40; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-61: 支持 `--full-diff` 选项（#904 部分；自 FPG-22 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：新增可见 `--full-diff`，`FormatPatchArgs.full_diff` 仅被接受；Libra `format-patch` 不接受 `-- <path>` pathspec，故无 pathspec 时输出与不带该 flag 逐字节相同（与 Git 同语义）。AC1 由 `format_patch_test::full_diff_accepted_without_pathspec` 覆盖，`compat_format_patch_flag_surface` 的 help 必列清单已加入 `--full-diff`；`format_patch_test` 135/135、`compat_format_patch_mail_roundtrip` 13/13。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-22` 拆出的独立轴：`FormatPatchArgs` 新增 `--full-diff` 并按 Git 接受（现行以 clap `unexpected argument` 拒绝）；Libra `format-patch` 不接受 `-- <path>` pathspec，Git 在无 pathspec 时该选项不改变输出，故接受后输出与不带该 flag 时相同；新增可见 flag 同卡加入 `tests/compat/format_patch_flag_surface.rs` 必列清单。

**Out of scope:** `format-patch -- <path>` pathspec 限制提交及其与 `--full-diff` 的组合（尚未排期，重启条件：新 issue 要求 pathspec）；`-U`/`--unified`（由 `FPG-22` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FormatPatchArgs` 无 `--full-diff` | `src/command/format_patch.rs:258-284` |

**Acceptance criteria:**

- [ ] AC1：`-1 --stdout --full-diff` 被接受且无 pathspec 时输出与不带该 flag 时逐字节相同。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::full_diff_accepted_without_pathspec` | 0 | stdout 与同夹具、同命令但不带 `--full-diff` 时的 stdout 逐字节相同（现行实现以非 0 拒绝该 flag 即不满足） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--full-diff` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::full_diff_accepted_without_pathspec)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::full_diff_accepted_without_pathspec`；证明 AC1）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-22`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--full-diff`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--full-diff接受; recovery=immutable-release; complete=yes; self-contained=yes; AC=1/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-22; writeset=序列化于FPG-22; release=independent; split-from=FPG-22; exception=N/A`

---

### Task FPG-27: 支持 `--diff-filter`（#904 部分；自 FPG-22 拆出；自本卡拆出 FPG-39）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：新增 `--diff-filter=<FILTER>`；父提交路径经 `DiffArgs --diff-filter=<FILTER>` 与 `CommitDiffOptions.diff_filter` 透传到共享 diff 引擎，同时过滤补丁与 diffstat；非法字母由共享引擎以 `LBR-CLI-002` 拒绝且 stdout 为空；`CommitDiffOptions::default().diff_filter` 为空，log/show/stash 行为不变。AC1–AC3 由 `format_patch_test::diff_filter` 覆盖（同一提交新增/修改/删除，仅输出新增路径的补丁与统计；非法 `Q` 带 `Error-Code: LBR-CLI-002` 且 stderr 含 `diff-filter`），AC4 由 `log::tests::commit_diff_options_default_has_no_diff_filter` 覆盖；`format_patch_test` 139/139、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 全绿；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；root 提交路径仍走 `log::generate_diff_with_options`（其 diff_filter 字段已存在但本卡仅父提交路径接入共享引擎，root 全部为新增，卡内无需过滤场景）。

**Description:** 自 `FPG-22` 拆出的独立轴：在 `FormatPatchArgs` 新增 `--diff-filter=<filter>`，经 `CommitDiffOptions` 透传到 `src/command/diff/`，同时过滤补丁 diff 与 diffstat；重命名选项核验与 #904 封面信相关验收由自本卡拆出的 `FPG-39` 承接。 AC1、AC2 夹具中新增与删除的文件内容互不相似（避免默认重命名检测把二者配对）。

**Out of scope:** `-U` 与 `--full-diff`（由 `FPG-22` 承接，本卡依赖其先行完成）；`-M`/`--find-renames`/`--no-renames` 核验与 #904 封面信写入/继承项（由 `FPG-39` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FormatPatchArgs` 仍缺 `--diff-filter`（`-M`/`--find-renames`/`--no-renames` 由 `docs/development/plan/issues/529.md` 的 `FP-06` 交付） | `src/command/format_patch.rs:258-284,1852-1859`、`docs/development/plan/issues/529.md:180` |
| `src/command/diff/` 目录现有 `compare.rs`、`options.rs`、`render.rs`；本卡预计改 `compare.rs`、`options.rs`，开工按 GC-01 核对，触及清单外文件须先按 ER-03 修订写集 | `src/command/diff/` |
| `--diff-filter` 解析 `parse_diff_filter` 为 `src/command/diff.rs` 私有函数，复用须改该文件 | `src/command/diff.rs:2228` |

**Acceptance criteria:**

- [ ] AC1：`--diff-filter=A` 只输出新增文件的 diff 段（夹具为同一提交内同时新增、修改与删除文件）。
- [ ] AC2：同一场景 diffstat 只列新增文件。
- [ ] AC3：`--diff-filter=Q`（非法字母）被拒绝。
- [ ] AC4：`CommitDiffOptions::default()` 新增的 diff-filter 字段为空（不过滤，`log`/`show`/`stash` 调用方行为不变）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::diff_filter` | 0 | 全部 `diff --git` 段的路径集合恰为夹具中新增文件集合 |
| AC2 | `command::format_patch_test::diff_filter` | 0 | 补丁 diffstat 段中统计行的路径集合恰为新增文件集合 |
| AC3 | `command::format_patch_test::diff_filter` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-CLI-002`，且 stderr 含 Libra 区分短语 `diff-filter`；stdout 为空 |
| AC4 | `command::log::tests::commit_diff_options_default_has_no_diff_filter`（lib） | 0（单元测试） | 默认值的 diff-filter 字段为空 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--diff-filter` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::diff_filter)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::diff_filter`；证明 AC1–AC3）
- [ ] `source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=command::log::tests::commit_diff_options_default_has_no_diff_filter)'`（`(new)`；具名验收门，期望集合 N=1：`command::log::tests::commit_diff_options_default_has_no_diff_filter`（lib）；证明 AC4）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）
- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::(log|show|stash|diff)_test::/)'`（`log`、`show`、`stash` 调用方与 `src/command/diff/` 渲染；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-61`, `DEP-FPG-02`（核实 `src/command/diff.rs` 与 `src/command/diff/` 无在途写者）, `DEP-FPG-03`（核实 `src/command/log.rs` 无在途写者）

**Implementation write set:** `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `src/command/diff/options.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `src/command/diff/options.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--diff-filter`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--diff-filter; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=3; prod-files=5; scope=M; deps=FPG-61,DEP-FPG-02,DEP-FPG-03; writeset=序列化于FPG-61; release=independent; split-from=FPG-22; exception=N/A`

---

### Task FPG-39: 核实 `529 FP-06` 重命名选项并闭环 #904 重命名相关验收项（#904 第 1、3、4 项；自 FPG-27 拆出；自本卡拆出 FPG-62）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：核实 `-M`/`--find-renames`/`--no-renames` 双向 last-wins；封面信系列 diffstat 继承补丁重命名选项；`--no-renames --cover-letter` 正常写出封面信与补丁，diff 段与 Git 一致（hunk 头按 `DEFER-FP-01`、rename summary 行按 `DEFER-FPG-03` 不在本卡范围）。AC1–AC7 由 `format_patch_test::rename_options_and_cover_inherit` 覆盖；`format_patch_test` 148/148（含 14 个 `fp06_*` 门）、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；封面信写入失败由 `FPG-62` 承接。

**Description:** 自 `FPG-27` 拆出的独立轴：按 GC-01 核实 `issues/529#FP-06` 交付的 `-M`/`--find-renames`/`--no-renames` 与 Git 一致，并闭环 #904 重命名相关验收项：`--no-renames --cover-letter` 在可写路径成功、封面信继承 diff 选项；`0000-cover-letter.patch` 被目录占用时的写入失败与不留部分输出（#904 第 2 项）由自本卡拆出的 `FPG-62` 承接。 `issues/529#FP-06` 的 `fp06_*` 门族在本卡共享回归组中运行（不计本卡 AC）。

**Out of scope:** `--diff-filter`、`-U`、`--full-diff`（由 `FPG-27`、`FPG-22`、`FPG-61` 承接）；封面信写入失败与不留部分输出（由 `FPG-62` 承接）；`-C` 复制检测（尚未排期，重启条件：新 issue）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `529 FP-06` 交付重命名三选项与 `fp06_*` 门族 | `docs/development/plan/issues/529.md:1423,2158` |

**Acceptance criteria:**

- [ ] AC1：`--no-renames -M -1 --stdout` 对纯重命名提交恢复 rename 形式（后出现的 `-M` 生效；#904 第 3 项）。
- [ ] AC2：`--no-renames --find-renames -1 --stdout` 同 AC1。
- [ ] AC3：`--no-renames -1 --stdout` 输出删除与新增两段。
- [ ] AC4：`--no-renames --cover-letter -1 -o d` 在可写路径成功写出封面信与补丁文件（#904 第 1 项）。
- [ ] AC5：同一调用写出的 `d/0001-` 补丁的 diff 段（不含 `@@` hunk 头行）与 Git 禁用重命名时一致（#904 第 1 项）。
- [ ] AC6：封面信继承 `-M`：`--no-renames -M --cover-letter -1 --stdout`（t4014「cover-letter inherits diff options」，#904 第 4 项、#905 第 3 项）。
- [ ] AC7：封面信继承 `--no-renames`：`--no-renames --cover-letter -1 --stdout`（同一上游用例）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::rename_options_and_cover_inherit` | 0 | 补丁 diff 段与同夹具 Git 黄金值逐字一致（双方均加 `--no-thread --no-signature`）（纯重命名 100% 无 hunk，符合「验证执行协议」第 3 项差异隔离） |
| AC2 | `command::format_patch_test::rename_options_and_cover_inherit` | 0 | 补丁 diff 段与 AC1 的 Libra 输出逐字节相同 |
| AC3 | `command::format_patch_test::rename_options_and_cover_inherit` | 0 | 补丁 diff 段去掉以 `@@` 开头的 hunk 头行后，与同夹具 Git `--no-renames` 黄金值同样处理的结果逐字一致（新增文件 hunk 头差异属 529 `DEFER-FP-01`，不比较 hunk 头） |
| AC4 | `command::format_patch_test::rename_options_and_cover_inherit` | 0 | `d/` 文件名集合与同夹具 Git 黄金值逐项相等（`0000-cover-letter.patch` 与一个 `0001-` 补丁文件） |
| AC5 | `command::format_patch_test::rename_options_and_cover_inherit` | 0 | `d/` 中 `0001-` 补丁文件的补丁 diff 段去掉以 `@@` 开头的 hunk 头行后，与同夹具 Git `--no-renames` 黄金值同样处理的结果逐字一致 |
| AC6 | `command::format_patch_test::rename_options_and_cover_inherit` | 0 | 封面信 diffstat 段中统计行的路径集合恰为 {`file => foo`}（版式随 `DEFER-FPG-03`，只比较路径） |
| AC7 | `command::format_patch_test::rename_options_and_cover_inherit` | 0 | 封面信 diffstat 段中统计行的路径集合恰为 {`file`, `foo`} |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（重命名选项与封面信继承 diff 选项说明（无需改动时记录核对结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::rename_options_and_cover_inherit)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::rename_options_and_cover_inherit`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::fp06_/)'`（`issues/529#FP-06` 交付的 `fp06_*` 门族；要求 M 等于 529 计划登记的 14 个门（`docs/development/plan/issues/529.md:2158`），开工时以 `cargo nextest list` 核实，不等即 STOP；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-27`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：重命名选项与封面信继承 diff 选项说明（无需改动时记录核对结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=重命名选项核验与封面信继承diff选项; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-27; writeset=序列化于FPG-27; release=independent; split-from=FPG-27; exception=N/A`

---

### Task FPG-62: 封面信路径被目录占用时写入失败且不留部分输出（#904 第 2 项；自 FPG-39 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：核实封面信先于补丁写出且单文件原子写；`d/0000-cover-letter.patch` 被目录占用时以 `LBR-IO-002`/128 失败、stdout 为空、不留下补丁或临时文件，既有实现已满足，无需代码改动。AC1/AC2 由 `format_patch_test::cover_letter_write_failure_leaves_no_partial_output` 覆盖（稳定码与路径短语；全量目录快照前后相同）；`format_patch_test` 149/149、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；开发文档已记录核对结论。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；数值退出码仍按 `DEFER-FPG-04`。

**Description:** 自 `FPG-39` 拆出的独立轴：按 GC-01 核实并补门：`-o d` 下 `d/0000-cover-letter.patch` 被目录占用时，`format-patch` 以写入失败（`LBR-IO-002`，而非 CLI 拒绝）退出，且不留下任何部分输出（Git t4014「failure to write cover-letter aborts gracefully」）；现行 `write_patch_file` 为单文件原子写、封面信先于补丁写出（`src/command/format_patch.rs:1795-1836`），开工按 GC-01 核实失败时后续补丁不再写出，不满足即在本卡修复。

**Out of scope:** 数值退出码与 `fatal:` 前缀（Git 为 128，见「验证执行协议」第 4 项失败契约与 `DEFER-FPG-04`）；重命名选项（由 `FPG-39` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `write_patch_file` 单文件原子写，`OutputWrite` 映射 `LBR-IO-002`（Fatal） | `src/command/format_patch.rs:381,410-412,1795-1836` |

**Acceptance criteria:**

- [ ] AC1：`--no-renames --cover-letter -1 -o d` 在 `d/0000-cover-letter.patch` 预先建为目录时以写入失败退出（#904 第 2 项）。
- [ ] AC2：同一调用失败后不留部分输出（#904 第 2 项）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::cover_letter_write_failure_leaves_no_partial_output` | 非 0 | stderr 中有独立一行逐字等于 `Error-Code: LBR-IO-002`，且 stderr 含 Libra 区分短语 `0000-cover-letter.patch`；stdout 为空 |
| AC2 | `command::format_patch_test::cover_letter_write_failure_leaves_no_partial_output` | 非 0 | 执行前后全量快照（「验证执行协议」第 4 项）相同：`d/` 中只有预建的 `0000-cover-letter.patch` 目录，无任何补丁文件或临时文件 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（封面信写入失败不留部分输出的说明（无需改动时记录核对结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::cover_letter_write_failure_leaves_no_partial_output)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::cover_letter_write_failure_leaves_no_partial_output`；证明 AC1–AC2）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-39`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：封面信写入失败不留部分输出的说明（无需改动时记录核对结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=封面信写入失败不留部分输出; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-39; writeset=序列化于FPG-39; release=independent; split-from=FPG-39; exception=N/A`

---

### Task FPG-63: 线程头按 Git 位于信封行之后、`From:` 之前（#916 前置；自 FPG-28 拆出，先于 FPG-28 执行）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：线程头 `Message-ID:`/`In-Reply-To:`/`References:` 移至信封行之后、`From:` 之前，补丁与封面信一致；仅改位置，取值仍由 `FPG-28` 承接。AC1 由 `format_patch_test::thread_headers_follow_envelope` 覆盖（4 封邮件逐封与 Git 头字段名序列逐字一致，含封面外部父邮件与补丁回复头）；`format_patch_test` 150/150、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；开发文档已记录核对结论。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-28` 拆出的独立轴（先于 `FPG-28` 执行，使其后的 `check_threading` 式黄金比较不因头区位置不同而失败）：把补丁与封面信的 `Message-ID:`/`In-Reply-To:`/`References:` 移到信封行之后、`From:` 之前（Git `log_write_email_headers`，`git show 6de20f6092:log-tree.c` 第 508–522 行；现行在 `Subject:` 之后输出，`src/command/format_patch.rs:866-879,1404-1413`）。本卡只改头的位置，不改 Message-ID 取值（由 `FPG-28` 承接）。

**Out of scope:** Message-ID 取值的唯一性（由 `FPG-28` 承接，在本卡之后执行）；`--thread` 默认值与 deep 线程（由 `FPG-23`、`FPG-35` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 线程头在 `Subject:` 之后输出 | `src/command/format_patch.rs:866-879,1404-1413` |
| Git 在信封行之后依次输出 `Message-ID:`、`In-Reply-To:`、`References:` | `git show 6de20f6092:log-tree.c` 第 508–522 行 |

**Acceptance criteria:**

- [ ] AC1：`--thread --cover-letter --in-reply-to="<test.message>" -3 --stdout` 中每封邮件（含带外部父邮件的封面信与带回复头的补丁）的 `Message-ID:`、`In-Reply-To:`、`References:` 均位于信封行之后、`From:` 之前（Git 头序）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::thread_headers_follow_envelope` | 0 | 逐封比较：每封邮件头区去掉首行信封后、各头字段名（冒号前部分，续行并入所属字段）组成的序列与同夹具 Git 黄金值中同序号邮件的序列逐字一致（只比字段名，不比 ID 等取值；双方均加 `--no-signature`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（线程头位置说明（无需改动时记录核对结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::thread_headers_follow_envelope)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::thread_headers_follow_envelope`；证明 AC1）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-62`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：线程头位置说明（无需改动时记录核对结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=线程头位置; recovery=immutable-release; complete=yes; self-contained=yes; AC=1/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-62; writeset=序列化于FPG-62; release=independent; split-from=FPG-28; exception=N/A`

---

### Task FPG-28: 修复 `build_thread_id` 生成互不为子串的唯一 `Message-ID`（#916 部分；自本卡拆出 FPG-63）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：Message-ID 改为系列 ID + 固定 8 位补丁序号（封面 `-cover`），任意两封互不为子串；无封面时后继补丁回复首补丁实际 ID，有封面时全部回复封面信。仅改 ID 生成，未改 `--thread` 默认值与 deep 链式结构。AC1–AC4 由 `format_patch_test::message_id_no_substring` 覆盖（3 封与 12 封两两无子串、封面回复关系、归一化父索引结构与 Git 一致）；`format_patch_test` 151/151、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；开发文档已记录核对结论。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-23` 拆出的独立轴：修改 `build_thread_id` / `push_threading_headers` 的 Message-ID 生成格式，使同系列任意两封邮件（含封面信与首个补丁）的 Message-ID 值（去掉尖括号后）互不为对方子串；当前首补丁值 `X` 是 `X-p2`、`X-cover` 的子串。 本卡先于 `FPG-35`、`FPG-23` 执行，使其后的 `check_threading` 式黄金比较在 ID 互不为子串的前提下成立；deep 链式结构由 `FPG-35` 验收。 线程头的位置（信封行之后、`From:` 之前）由自本卡拆出、先于本卡执行的 `FPG-63` 承接。

**Out of scope:** `--thread` 默认关闭与 `format.thread`（由 `FPG-23`、`FPG-47` 承接，二者在本卡之后执行）；deep 链式线程（由 `FPG-35` 承接，在本卡之后执行）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `push_threading_headers` 中首封用 `<{series_id}>` 而后续用 `<{series_id}-p{n}>`，前者为后者子串 | `src/command/format_patch.rs:1331-1336,1409` |

**Acceptance criteria:**

- [ ] AC1：`--thread -3`（无封面信）三封邮件的 Message-ID 值（去尖括号）两两互不为子串（#916 第 1 项）。
- [ ] AC2：`--thread --cover-letter -11 --stdout` 系列（仓库至少 12 个提交）去掉尖括号后的 12 个 Message-ID 值两两互不为对方子串（覆盖序号 1 与 10、11 的前缀关系）。
- [ ] AC3：shallow 时每个补丁的 `In-Reply-To:` 等于封面信 `Message-ID:`。
- [ ] AC4：修复后 t4014「thread」用例（`--thread`，无封面信）的归一化线程结构与 Git 一致（按 `check_threading` 的子串替换规则，ID 互为子串时必然失败）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::message_id_no_substring` | 0 | 对 3 个 ID 值两两做双向 `contains` 判定均为假 |
| AC2 | `command::format_patch_test::message_id_no_substring` | 0 | 对封面信与 11 个补丁的 12 个 ID 值两两做双向 `contains` 判定均为假 |
| AC3 | `command::format_patch_test::message_id_no_substring` | 0 | 三补丁加封面信系列逐封比对相等（线程结构整体判定） |
| AC4 | `command::format_patch_test::message_id_no_substring` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（Message-ID 唯一性说明（无需改动时记录核对结论））；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::message_id_no_substring)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::message_id_no_substring`；证明 AC1–AC4）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-63`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：Message-ID 唯一性说明（无需改动时记录核对结论））；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=非子串唯一Message-ID; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-63; writeset=序列化于FPG-63; release=independent; split-from=FPG-23; exception=N/A`

---

### Task FPG-35: 支持 `--thread=shallow|deep` 取值与 deep 链式线程（#916 部分；自 FPG-23 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--thread` 支持 `shallow`/`deep` 可选值（`require_equals`，裸 `--thread`=shallow）；deep 逐封回复前一封、References 保持线程根；`--no-thread --in-reply-to` 为每封邮件保留外部父头但不生成 Message-ID。未改 `--thread` 默认值（由 `FPG-23` 承接）。AC1–AC7 由 `format_patch_test::thread_shallow_and_deep` 覆盖（7 个场景归一化线程结构与 Git 逐封一致，外部 ID 保留）；`format_patch_test` 152/152、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-23` 拆出的独立轴：`--thread` 接受可选值 `shallow`/`deep`（无值即 `shallow`），`deep` 时每封邮件回复上一封且 `References:` 依序列出外部父邮件、封面信与全部祖先；显式 `--no-thread` 时 `--in-reply-to` 按 Git 为每封邮件添加外部父邮件头（默认关闭由 `FPG-23` 承接，本卡以显式 `--no-thread` 验收）。默认值与 `format.thread` 由 `FPG-23` 承接。 浅线程在有封面信与 `--in-reply-to` 时，补丁 `References:` 依次列出外部父邮件与封面信（现行 `push_threading_headers` 只列外部父邮件，`src/command/format_patch.rs:1350`）。`--thread` 的值须以 `=` 紧贴（可选值参数），裸 `--thread` 之后的参数按修订解析。

**Out of scope:** `--thread` 默认关闭与 `format.thread`（由 `FPG-23` 承接）；Message-ID 去重（由 `FPG-28` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--thread` 为 `default_value_t = true` 的布尔开关，不接受 `=shallow`/`=deep`；`push_threading_headers` 只生成浅线程 | `src/command/format_patch.rs:183-188,1319-1353` |

**Acceptance criteria:**

- [ ] AC1：`--cover-letter --in-reply-to="<test.message>" --thread=shallow main`（t4014「thread explicit shallow」原样参数）的浅线程：补丁 `References:` 依次列出外部父邮件与封面信。
- [ ] AC2：`--thread=deep`（t4014「thread deep」）。
- [ ] AC3：`--thread=deep --in-reply-to=<id>`（t4014「thread deep in-reply-to」）。
- [ ] AC4：`--thread=deep --cover-letter`（t4014「thread deep cover-letter」）。
- [ ] AC5：`--thread=deep --cover-letter --in-reply-to=<id>`（t4014「thread deep cover-letter in-reply-to」）。
- [ ] AC6：`--no-thread --in-reply-to=<id>` 三补丁系列与 Git 一致（Git 为每封邮件添加外部父邮件头；不依赖 `FPG-23` 的默认值）。
- [ ] AC7：裸 `--thread` 后紧跟修订参数（`--thread main`）生成 shallow 线程（t4014 一律写作 `--thread main`）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::thread_shallow_and_deep` | 0 | 保留外部 ID 的归一化线程结构与同夹具 Git 黄金值（t4014 `expect.cl-irt`）逐字一致 |
| AC2 | `command::format_patch_test::thread_shallow_and_deep` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| AC3 | `command::format_patch_test::thread_shallow_and_deep` | 0 | 保留外部 ID 的归一化线程结构与同夹具 Git 黄金值逐字一致（区段按「验证执行协议」第 3 项；错误的外部父 ID 不会被归一化掩盖） |
| AC4 | `command::format_patch_test::thread_shallow_and_deep` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| AC5 | `command::format_patch_test::thread_shallow_and_deep` | 0 | 保留外部 ID 的归一化线程结构与同夹具 Git 黄金值逐字一致（区段按「验证执行协议」第 3 项；错误的外部父 ID 不会被归一化掩盖） |
| AC6 | `command::format_patch_test::thread_shallow_and_deep` | 0 | 保留外部 ID 的归一化线程结构与同夹具 Git 黄金值逐字一致（区段按「验证执行协议」第 3 项；错误的外部父 ID 不会被归一化掩盖） |
| AC7 | `command::format_patch_test::thread_shallow_and_deep` | 0 | 归一化线程结构与同夹具 Git 黄金值（t4014 `expect.thread`）逐字一致 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `--thread=deep` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::thread_shallow_and_deep)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::thread_shallow_and_deep`；证明 AC1–AC7）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-28`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`--thread=deep`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--thread=shallow/deep与deep链式线程; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-28; writeset=序列化于FPG-28; release=independent; split-from=FPG-23; exception=N/A`

---

### Task FPG-23: 修复 `--thread` 默认关闭（#917 第 1 项；自本卡拆出 FPG-28、FPG-35、FPG-47）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：`--thread` 默认关闭；无 flag 时三补丁系列不输出线程头；单独 `--in-reply-to` 仅逐封添加外部父头、不生成 Message-ID。`format.thread` 取值仍由 `FPG-47` 承接。AC1/AC2 由 `format_patch_test::format_thread_default_off` 覆盖（默认无头与 Git 一致、外部父邮件逐封保留且与 Git 一致）；`format_patch_test` 153/153、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 将 `--thread` 默认行为对齐 Git：默认不输出系列线程头；`--in-reply-to` 单独使用时只为每封邮件添加外部父邮件头、不启用系列线程。`format.thread` 的全部取值（含 `false`/空值关闭，#917 第 2 项）由自本卡拆出的 `FPG-47` 承接——默认关闭之后 `false`/空值与未配置不可区分，须在启用取值已实现时以跨层与同层夹具验收；`--thread=shallow|deep` 与 deep 链式线程由 `FPG-35` 承接，Message-ID 去重由 `FPG-28` 承接。

**Out of scope:** `format.thread` 全部取值（由 `FPG-47` 承接）与命令行覆盖配置（由 `FPG-50` 承接）；`--thread=shallow|deep` 与 deep 链式线程（由 `FPG-35` 承接，本卡依赖其先行完成）；Message-ID 去重（由 `FPG-28` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--thread` 设为 `default_value_t = true` 且不接受 `=shallow`/`=deep`；`resolve_format_config` 未读 `format.thread` | `src/command/format_patch.rs:183-188,1861-1902` |
| 既有 `thread_flag_adds_message_id`、`no_thread_suppresses_headers` 及默认带线程头的用例在默认关闭后须同卡核对断言 | `tests/command/format_patch_test.rs:611,642` |

**Acceptance criteria:**

- [ ] AC1：无 `--thread`/`--in-reply-to`/`format.thread` 的三补丁系列不输出线程头（#917 第 1 项）。
- [ ] AC2：不带 `--thread`/`--no-thread` 时三补丁 `--in-reply-to=<id>` 与 Git 一致（默认关闭后为每封邮件添加外部父邮件头；t4014「format-patch --in-reply-to」的多补丁扩展）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_thread_default_off` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致（三补丁无线程时该结构恰为三行 `---`，一次比较同时判定邮件数与不存在线程头；对应 t4014 `expect.no-threading`） |
| AC2 | `command::format_patch_test::format_thread_default_off` | 0 | 保留外部 ID 的归一化线程结构与同夹具 Git 黄金值逐字一致（区段按「验证执行协议」第 3 项；错误的外部父 ID 不会被归一化掩盖） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 本卡无新增标记：4 个仓库文件按第 1 步核对（`--thread` 默认关闭与 `--in-reply-to` 单独使用的说明）；网站页按第 3 步「有变化 / 无变化」分支处理并以 `PROBE` 核验线上页面 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_thread_default_off)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_thread_default_off`；证明 AC1–AC2）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-35`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡无新增验收标记（按第 1 步核对：`--thread` 默认关闭与 `--in-reply-to` 单独使用的说明）；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--thread默认关闭; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-35; writeset=序列化于FPG-35; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-47: 支持 `format.thread` 取值（#917 第 2、3 项；自 FPG-23 拆出；自本卡拆出 FPG-50）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：读取 `format.thread`——`deep`/`shallow` 不区分大小写，`true` 为 shallow，`false`/空值关闭；本地覆盖全局、同层后写者生效；命令行 flag 覆盖配置。AC1–AC6 由 `format_patch_test::format_thread_config_values` 覆盖（true/Shallow/deep、全局 deep+本地 false/空值、同层 `--add deep` 后 `false`，均与 Git 归一化结构一致）；`format_patch_test` 154/154、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试；命令行覆盖配置由 `FPG-50` 承接。

**Description:** 自 `FPG-23` 拆出的独立轴：读取 `format.thread`：`deep`（不区分大小写）为 deep，`shallow`（不区分大小写）与布尔真值（`true` 等）为 shallow，布尔假值与空值关闭（`git show 6de20f6092:builtin/log.c` 第 1030–1040 行，关键字以 `strcasecmp` 比较）；每个取值完全决定线程样式，按 Git 后出现者生效（跨层即 local 优先，同层取最后写入者，`read_format_config` 的单值严格级联即可表达）。关闭取值以「另一层或同层先前取值为启用」的夹具验收，使忽略配置的实现不能通过；全局层经临时 `LIBRA_CONFIG_GLOBAL_DB` 隔离（GC-13）。命令行覆盖由自本卡拆出的 `FPG-50` 承接。

**Out of scope:** 默认关闭（由 `FPG-23` 承接，本卡依赖其先行完成）；命令行覆盖配置（由 `FPG-50` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_format_config` 未读取 `format.thread` | `src/command/format_patch.rs:183-188,1861-1902` |

**Acceptance criteria:**

- [ ] AC1：`format.thread=true`（t4014「thread via config」）。
- [ ] AC2：`format.thread=Shallow`（大小写混合，Git 以 `strcasecmp` 匹配）。
- [ ] AC3：`format.thread=deep`（t4014「thread deep via config」）。
- [ ] AC4：隔离的全局层 `format.thread=deep`、本地 `format.thread=false` 时不输出线程头（#917 第 2 项）。
- [ ] AC5：隔离的全局层 `format.thread=deep`、本地 `format.thread=`（空值）时不输出线程头（#917 第 2 项）。
- [ ] AC6：本地以 `--add` 依次写入 `format.thread=deep`、`format.thread=false` 时不输出线程头（同层后写者生效）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::format_thread_config_values` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| AC2 | `command::format_patch_test::format_thread_config_values` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| AC3 | `command::format_patch_test::format_thread_config_values` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| AC4 | `command::format_patch_test::format_thread_config_values` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致（三补丁无线程时该结构恰为三行 `---`，一次比较同时判定邮件数与不存在线程头；对应 t4014 `expect.no-threading`） |
| AC5 | `command::format_patch_test::format_thread_config_values` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致（三补丁无线程时该结构恰为三行 `---`，一次比较同时判定邮件数与不存在线程头；对应 t4014 `expect.no-threading`） |
| AC6 | `command::format_patch_test::format_thread_config_values` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致（三补丁无线程时该结构恰为三行 `---`，一次比较同时判定邮件数与不存在线程头；对应 t4014 `expect.no-threading`） |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.thread` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::format_thread_config_values)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::format_thread_config_values`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-23`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.thread`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.thread取值; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-23; writeset=序列化于FPG-23; release=independent; split-from=FPG-23; exception=N/A`

---

### Task FPG-50: `--thread`/`--thread=<style>`/`--no-thread` 按命令行顺序覆盖 `format.thread`（#917 第 4 项、#916 第 2 项；自 FPG-47 拆出）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `locally-accepted`（2026-11-25：核实 clap 双向 `overrides_with` + CLI 存在即跳过配置，已满足 argv 顺序覆盖：裸 `--thread`=shallow、`--thread=<style>` 按样式、`--no-thread` 关闭，后出现者胜出；无代码改动，补验收门。AC1–AC6 由 `format_patch_test::thread_cli_overrides_config` 覆盖（deep+裸 `--thread`→shallow、deep+`--no-thread`→无头、shallow+`--thread=deep`、deep+`--thread=shallow`、`--thread=deep --no-thread`→无头、`--no-thread --thread=deep`→deep，均与 Git 归一化结构一致）；`format_patch_test` 155/155、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过；EN/zh/开发文档与 `COMPATIBILITY.md` 已同步。偏差（记录）：用户已取消独立 Claude/Codex 审计与全量测试。

**Description:** 自 `FPG-47` 拆出的独立轴：命令行线程选项按出现顺序覆盖 `format.thread`：裸 `--thread` 为 shallow，`--thread=<style>` 为对应样式，`--no-thread` 关闭；后出现者覆盖先出现者与配置。

**Out of scope:** `format.thread` 取值解析（由 `FPG-47` 承接，本卡依赖其先行完成）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--thread`/`--no-thread` 现为布尔开关，未按出现顺序与配置合并 | `src/command/format_patch.rs:183-188` |

**Acceptance criteria:**

- [ ] AC1：`format.thread=deep` 下命令行 `--thread` 生成 shallow 线程（t4014「thread config + override」，#916 第 2 项）。
- [ ] AC2：`format.thread=deep` 下 `--no-thread` 不输出线程头（t4014「thread config + --no-thread」）。
- [ ] AC3：`format.thread=shallow` 下 `--thread=deep` 生成 deep 线程。
- [ ] AC4：`format.thread=deep` 下 `--thread=shallow` 生成 shallow 线程。
- [ ] AC5：`format.thread=true` 下 `--thread=deep --no-thread` 不输出线程头（后者生效）。
- [ ] AC6：`format.thread=false` 下 `--no-thread --thread=deep` 生成 deep 线程（后者生效）。
- [ ] ER-06/ER-06a 同卡强制门：按「文档与网站验收命令」逐文件交付并验收 `Docs and compatibility impact` 所列 5 个文件——发布前段第 1–5 步是本卡 `locally-accepted` 的前置，发布后段第 6–8 步是 `remote-pending → complete` 的后置门（不计入卡特有 AC 分子）。

**AC → 断言映射:**（口径见「验证执行协议」第 3–5 项；未另写的前后状态适用第 4 项默认前后状态契约）

| AC | 具名测试 | 期望退出码 | 可观测断言（输出通道：必须出现 / 必须不出现） |
|---|---|---|---|
| AC1 | `command::format_patch_test::thread_cli_overrides_config` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| AC2 | `command::format_patch_test::thread_cli_overrides_config` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致（三补丁无线程时该结构恰为三行 `---`，一次比较同时判定邮件数与不存在线程头；对应 t4014 `expect.no-threading`） |
| AC3 | `command::format_patch_test::thread_cli_overrides_config` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| AC4 | `command::format_patch_test::thread_cli_overrides_config` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| AC5 | `command::format_patch_test::thread_cli_overrides_config` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致（三补丁无线程时该结构恰为三行 `---`，一次比较同时判定邮件数与不存在线程头；对应 t4014 `expect.no-threading`） |
| AC6 | `command::format_patch_test::thread_cli_overrides_config` | 0 | 归一化线程结构与同夹具 Git 黄金值逐字一致 |
| 文档 | 「文档与网站验收命令」第 1–8 步 | 各步 0 | 4 个仓库文件均命中本卡标记 `format.thread` 且相对基线有改动；网站页按第 3 步分支同步并以区分探针 `PROBE` 核验，前端构建、Claude Code `PASS`、签名推送与线上页面命中 |

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(=command::format_patch_test::thread_cli_overrides_config)'`（`(new)`；具名验收门，期望集合 N=1：`command::format_patch_test::thread_cli_overrides_config`；证明 AC1–AC6）

**共享回归组（ER-04 A 组 focused 门；按「验证执行协议」第 2 项执行，不计入 `VER`，不作卡特有 AC 证据）：**

- `source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'`（全部 format-patch 命令用例；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`（Git↔Libra `am` 往返；M ≥ 1）
- `source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`（`--help` flag 面；M ≥ 1）

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-47`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件交付并验收 5 个文件：`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改，不增删行）、`/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（只由同步脚本生成，禁止手改）；本卡验收标记 `MARKERS`：`format.thread`；按「文档与网站验收命令」第 1–8 步执行；新增可见 flag 时同卡把该 flag 加入 `tests/compat/format_patch_flag_surface.rs` 必列清单（ER-06 help 同步）。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=命令行线程选项按顺序覆盖配置; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=1/8; landing=2; prod-files=1; scope=M; deps=FPG-47; writeset=序列化于FPG-47; release=independent; split-from=FPG-47; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `FormatPatchError` 文案、`CommitDiffOptions` 默认值（FPG-22） | 共享回归组：`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(/^command::format_patch::tests::/)'`；具名：`source .env.test && cargo nextest run --lib --no-tests=fail -E 'test(=command::log::tests::commit_diff_options_default_is_backward_compatible)'` |
| 集成 | 本计划承接的 issue 行为用例（`#880` 由 529 交付、`#905` 的 `--summary` 摘要行见 `DEFER-FPG-03`；编号、配置、环境变量、封面信、interdiff、diff 选项、线程化），各卡具名验收门见追溯表 | 共享回归组：`source .env.test && cargo nextest run --test command_test --no-tests=fail -E 'test(/^command::format_patch_test::/)'` |
| 兼容 | Git↔Libra `am` 往返、flag surface 契约、`format-patch --stdout` BrokenPipe 输出 | 共享回归组：`source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip --no-tests=fail`、`source .env.test && cargo nextest run --test compat_format_patch_flag_surface --no-tests=fail`、`source .env.test && cargo nextest run --test compat_broken_pipe_output --no-tests=fail` |
| 迁移 | N/A（无 SQLite schema 变更） | N/A |
| 安全 | 邮件头 CR/LF 注入过滤（FPG-04、FPG-44、FPG-37）、`--relative` 仓库外与不存在前缀与 Git 一致、不写入仓库外文件且输出不暴露其路径（FPG-48）、`-v` 非常规值按 Git 清洗文件名前缀且只写入输出目录（FPG-26；`Subject:` 原样见 FPG-52） | 各卡具名验收门（见追溯表）+ 集成行共享回归组 |
| 性能 | 继承 GC-10，无无界全库遍历 | 集成行共享回归组 |
| live/gated | 每卡 T-1 与收口阶段双环境全量回归 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| FPG-01 | #896（默认编号部分）；`#880` 由 `issues/529#FP-02`/`FP-04` 交付并关闭（`DEP-FPG-01`）; `format_patch.rs:855-861` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::default_numbering_auto`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-32 | #896（配置部分）; `format_patch.rs:1861-1916` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_numbered_config`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-02 | #881（身份切片）; `format_patch.rs:2050-2065` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::signoff_git_committer_env_precedence`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-53 | #881（身份失败路径）; `format_patch.rs:2050-2065` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::signoff_identity_failures`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-03 | #882; `format_patch.rs:688-750,1126` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::ignore_if_in_upstream_tag_t4014_4`、`ignore_if_in_upstream_suppresses_patch_equivalent_commits`（target `compat_format_patch_mail_roundtrip`）；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-04 | #883（第 1–5 项）; `format_patch.rs:1861-1916`、`internal/config.rs:1872` | `src/command/format_patch.rs`, `src/internal/config.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_headers_and_add_header`、`internal::config::tests::cascaded_values_strict_preserves_layer_order`（lib）、`internal::config::tests::cascaded_values_strict_reports_read_failure`（lib）；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-56 | #896（配置部分，多层与重复取值）; `format_patch.rs:1904-1916`、`internal/config.rs`（`FPG-04` helper） | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_numbered_config_layers`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-44 | #883（第 6、6b、7 项）; `format_patch.rs:97-364` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::add_header_and_no_add_header`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-37 | #883（第 9 项）, #914; `format_patch.rs:194-210,1918-1935` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_to_cc_and_no_to_ordering`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-57 | #883（第 9 项）, #914（第 1 项）; `format_patch.rs:1918-1979`、`internal/config.rs` | `src/command/format_patch.rs`, `src/internal/config.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::recipient_lists_unify_like_git`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-05 | #884（命令行部分）; `format_patch.rs:236-240,1012-1057` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::no_notes_and_ordering`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-36 | #884（配置取值）; `format_patch.rs:1012-1057,1861-1902` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_notes_config`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-41 | #884（第 3、4b 项）; `format_patch.rs:1012-1057` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_notes_cli_interaction`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-54 | #885（第 6 项，签名块字节形状）; `format_patch.rs:1062-1071,1804-1813` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::signature_block_layout_matches_git`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-06 | #885; `format_patch.rs:1062-1071` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_signature_config_and_prefix`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-55 | #885（第 6 项其余 t4014 签名用例）; `format_patch.rs:1062-1071,1861-1902` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_signature_file_config`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-07 | #911; `format_patch.rs:833,1497` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::multiline_subject_join`、`multiline_subject_roundtrips_with_git_am`（target `compat_format_patch_mail_roundtrip`）；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-08 | #887; `format_patch.rs:347-358,1981` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::encode_email_headers_default_on`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-29 | #886; `format_patch.rs:863-869` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::subject_header_folding`、`folded_subject_roundtrips_with_git_am`（target `compat_format_patch_mail_roundtrip`）；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-30 | #897, #881（t4036 #2）; `format_patch.rs:893-905` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::patch_mime_headers_on_demand`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-51 | #881（t4036 #3–#5）; `format_patch.rs:897-905,2050-2065` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::t4036_signer_mime_cases`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-42 | #897（封面信）; `format_patch.rs:1415-1417` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::cover_letter_mime_on_demand`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-09 | #888（第 5 项与 `--rfc=`；第 1–4 项由 `issues/529#FP-03` 交付）; `format_patch.rs:437-458` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::fp03_empty_rfc_with_k`、`command::format_patch_test::keep_subject_and_prefix_alone_succeed`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-10 | #889; `format_patch.rs:2022-2032` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::from_invalid_ident_rejected`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-31 | #915（第 2 项）; `format_patch.rs:1861-1902` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_from_and_no_from`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-46 | #915（第 1、3 项）; `format_patch.rs:212-217,2037-2048` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::from_and_no_from_override_config`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-11 | #890; `format_patch.rs:500-506,1852` | `src/command/format_patch.rs`, `src/command/log.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::diff_relative_filters_paths`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface`、`command::{log,show,stash}_test::*` |
| FPG-48 | #890（边界）; `format_patch.rs:500-506` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::relative_outside_and_missing_prefix`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-12 | #900, #909; `format_patch.rs:549,1861` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_cover_letter_auto_config`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-58 | #905（封面信正文黄金比较的前置）; `format_patch.rs:1422-1423` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::cover_letter_body_template_matches_git`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-13 | #905; `format_patch.rs:1428-1444` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::cover_letter_includes_diffstat`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-14 | #899（原子与 `log:`）; `format_patch.rs:1535-1584,1621-1644` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::commit_list_format_git_atoms`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-49 | #899（配置）, #909（第 4 项）, #900（第 4 项）; `format_patch.rs:1535-1584,1861-1902` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::commit_list_format_config_and_full`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-15 | #892; `format_patch.rs:108-109,663` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::revision_range_caret_exclusion`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-16 | #893; `format_patch.rs:803,1373` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::mbox_envelope_fixed_2001_timestamp`、`fixed_envelope_mbox_applies_with_git_am`（target `compat_format_patch_mail_roundtrip`）；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-59 | #893（第 3 项）; `format_patch.rs:828-829,1837-1840` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::patch_date_uses_author_time_and_zone`、`author_date_and_zone_roundtrip_with_git_am`（target `compat_format_patch_mail_roundtrip`）；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-64 | #894（第 4 项相关：base 区块位置）; `format_patch.rs:532-533,569-574` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::base_block_rides_on_first_mail`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-17 | #894（`--base=auto`）; `format_patch.rs:1214-1284` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::base_auto_cli`、`command::format_patch_test::base_records_base_commit_and_prerequisite_patch_ids`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-43 | #894（配置部分）; `format_patch.rs:1861-1902` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_use_auto_base`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-24 | #891（分支描述）、#892 第 4 项; `format_patch.rs:1422` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::cover_letter_branch_description`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-60 | #891（默认 `From:`）; `format_patch.rs:1386-1396` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::cover_letter_default_from_identity`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-25 | #906; `format_patch.rs:97-364` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::cover_from_description`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-33 | #906（配置部分）; `format_patch.rs:1861-1916` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_cover_from_description_config`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-18 | #895; `format_patch.rs:249-256,883-905` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::attach_custom_boundary_and_config`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-38 | #912; `format_patch.rs:1861-1902` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_attach_config`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-19 | #898（多补丁；第 3 项拒绝由 `issues/529#FP-10` 交付）; `issues/529.md:1798-1806` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::interdiff_multi_patch_cover_letter`、`command::format_patch_test::interdiff_invalid_rev_rejected`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-34 | #898（单补丁部分）; `issues/529.md:1804` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::interdiff_single_patch_placement`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-20 | #901（第 1、3、4 项）; `format_patch.rs:1699-1737` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::reroll_filename_prefix`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-45 | #901（第 2 项）; `format_patch.rs:610-612` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::generated_paths_on_stdout`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-26 | #903（第 1–3 项）; `format_patch.rs:220-226` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::reroll_non_integer`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-52 | #903（第 2、4 项）; `format_patch.rs:220-226` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::reroll_non_integer_subjects`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-21 | #908; `format_patch.rs:1646-1667` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::filename_git_sanitized_slug`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-40 | #910; `format_patch.rs:1672-1681,1699-1751` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::filename_max_length_git_parity`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-22 | #904; `format_patch.rs:258,1852` | `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/render.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::unified_context_lines`、`command::log::tests::commit_diff_options_default_is_backward_compatible`（lib）；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface`、`command::{log,show,stash,diff}_test::*` |
| FPG-61 | #904（`--full-diff`）; `format_patch.rs:258-284` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::full_diff_accepted_without_pathspec`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-27 | #904（`--diff-filter`）; `format_patch.rs:258-284,1852-1859` | `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff.rs`, `src/command/diff/compare.rs`, `src/command/diff/options.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::diff_filter`、`command::log::tests::commit_diff_options_default_has_no_diff_filter`（lib）；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface`、`command::{log,show,stash,diff}_test::*` |
| FPG-39 | #904（重命名与封面信继承项）; `issues/529.md:1423` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::rename_options_and_cover_inherit`；共享回归组：`command::format_patch_test::*`、`command::format_patch_test::fp06_*`（M = 14）、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-62 | #904（第 2 项）; `format_patch.rs:1795-1836` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::cover_letter_write_failure_leaves_no_partial_output`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-63 | #916（线程黄金比较前置）; `format_patch.rs:866-879,1404-1413` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::thread_headers_follow_envelope`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-28 | #916; `format_patch.rs:1331-1336` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::message_id_no_substring`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-35 | #916（deep 部分）; `format_patch.rs:183-188,1319-1353` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::thread_shallow_and_deep`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-23 | #917（第 1 项）; `format_patch.rs:183-188,1293` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_thread_default_off`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-47 | #917（第 2、3 项）; `format_patch.rs:1861-1902` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::format_thread_config_values`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |
| FPG-50 | #917（第 4 项）, #916（第 2 项）; `format_patch.rs:183-188` | `src/command/format_patch.rs` | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`、网站 `format-patch.en.md` | `command::format_patch_test::thread_cli_overrides_config`；共享回归组：`command::format_patch_test::*`、`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划评审通过且 `DEP-FPG-01`（`529.md`）释放写锁 | Claude & Codex `PASS` + `529 CLOSE-529` 完成 | N/A |
| M1 | Phase 0（`FPG-01`..`FPG-10`, `FPG-29`..`FPG-32`, `FPG-36`, `FPG-37`, `FPG-41`, `FPG-42`, `FPG-44`, `FPG-46`, `FPG-51`, `FPG-53`..`FPG-57`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest`、D 组与「文档与网站验收命令」全绿 | 发布前 revert 本地提交；发布后按 ER-10 新建 `FIX-*` 前滚发新版本 |
| M2 | Phase 1（`FPG-11`..`FPG-17`, `FPG-24`, `FPG-25`, `FPG-33`, `FPG-43`, `FPG-48`, `FPG-49`, `FPG-58`..`FPG-60`, `FPG-64`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 同上 |
| M3 | Phase 2（`FPG-18`..`FPG-23`, `FPG-26`..`FPG-28`, `FPG-34`, `FPG-35`, `FPG-38`..`FPG-40`, `FPG-45`, `FPG-47`, `FPG-50`, `FPG-52`, `FPG-61`..`FPG-63`）全部 `done`/`complete` + 全量收口门通过 | 全量 `nextest` + `fmt` + `clippy` 全绿 | 同上 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 渲染阶段中途失败（如非法 config / 无效 `--from`） | 无补丁文件写入 | 返回 `CliError` 并中止，无半成品文件残留 | 写入部分补丁文件 |
| `write_patch_file` 单文件 I/O 失败 | 已写入的前序完整补丁文件 | 单文件 `atomic_write` 保证无半截文件，返回 `IoWriteFailed` | 静默截断文件 |
| 远端 D 组 CI 失败 | 已推送版本 tag 与提交 | 按 ER-04/ER-10 新建 `FIX-*` 前滚修复 | 回退已推送提交或删除已发布 tag |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `issues/529.md` 写锁未释放前并发改动 `format_patch.rs` | 高：代码冲突与重复实现 | 严格执行 `DEP-FPG-01` 门禁，`FPG-01` 在 `529 CLOSE-529` 后开工 | FPG-01..FPG-64 |
| 默认开启 `--encode-email-headers`（#887）、纯 ASCII 省略 MIME（#897）及 `--thread` 默认关闭（#917）改变既有测试输出 | 中：既有断言需同步调整 | 同卡更新对应断言并对照 `git@6de20f6092` 验证 | FPG-08, FPG-30, FPG-23 |
| `COMPATIBILITY.md` 增删行导致 `compat_ledger_schema` 失败 | 中：CI 兼容账本守卫报错 | 仅原地修改第 224 行，不增删行 | FPG-01..FPG-64 |
| issue 验收标准在执行时已被修订，或 `issues/529.md` 交付的测试门族名称（`fp03_*`、`fp10_*`）或 `529 FP-06` 重命名用例与本计划引用不一致 | 中：具名验收门零匹配或集合不等 | 开工按 GC-01 用 `cargo nextest list` 核实名称，缺失即 STOP 并按 ER-03 修订本卡后再开工 | FPG-09, FPG-19, FPG-27, FPG-34 |
| Git 黄金值随本机 Git 版本漂移（本机为 Git 2.54，计划基线为 `git@6de20f6092` v2.56.0-116） | 中：黄金常量与基线 Git 不一致导致误判 | 「验证执行协议」第 3 项要求在基线 revision 构建的 Git 上生成黄金值并在测试注释记录命令与版本 | FPG-03..FPG-05, FPG-08, FPG-11..FPG-19, FPG-21..FPG-27, FPG-29..FPG-31, FPG-34..FPG-64 |
| 网站同步早于版本发布或跨卡并发写 `/Volumes/Data/gitmono/libra-backend` | 中：文档先于二进制公开或网站提交混入他卡改动 | 「文档与网站验收命令」发布前段（第 1–5 步）在本地验收期间生成并审定网站字节，发布后段（第 6–8 步）在 C 组 ⑨ 之后复核合入树、提交推送并核验部署，全程在 `DEP-FPG-04` 锁内，精确暂存并经 `remote_sha` 回读 | FPG-01..FPG-64 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 系列补丁渲染 | `O(N_commits * commit_diff)` | `O(N_commits)` 内存缓冲（先渲染后逐文件 `atomic_write`） | 继承 GC-10，单系列受现有 repo 规模约束 | `command_test::format_patch_test` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 第 224 行已原地同步。
- [ ] `docs/commands/format-patch.md` 已同步。
- [ ] `docs/commands/zh-CN/format-patch.md` 已同步。
- [ ] `docs/development/commands/format-patch.md` 已同步。
- [ ] `/Volumes/Data/gitmono/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md` 已按 `GC-05` 与「文档与网站验收命令」在 `/Volumes/Data/gitmono/libra-backend`（`cf` 分支）完成同步与只读漂移检查；网站页有变化的卡另完成 `pnpm typecheck`/`pnpm build`、Claude Code `PASS`、签名提交推送 `cf`（`remote_sha` 回读）与构建记录及线上页面核验，网站页无变化的卡记录摘要、`SITE_SHA` 与当前生产部署的核验记录。
- [ ] `docs/error-codes.md`：`N/A`（复用现有 `LBR-CLI-002` / `LBR-CLI-003` / `LBR-IO-*`，未新增错误码）。
- [ ] `tests/INDEX.md` 已核对（仅扩充既有 `command_test` / `compat_format_patch_*`，未新增 target）。
- [ ] `Cargo.toml` `[[test]]`：`N/A`（未新增 `--test` target）。
- [ ] `plan-long.md`：`N/A`（由协调者统一维护）。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。`FAIL` 必须列出 P0/P1 条目并在下一轮复审关闭；P2 可由具名责任人书面接受为 residual risk，但不改变本轮 `FAIL` 记录（ER-05）。

| Round | Reviewer | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|---|
| R1 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P0: 529 写锁与命名冲突；P1: target/D组/多轴/虚构依赖/二义性 | 补全基线与文档 | R2 修订 |
| R1 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `--test` 错误、ER-14 `nextest`、FP-03/13/17/22 缺陷 | 补全模式判据与原子写描述 | R2 修订 |
| R2 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: 4 卡 `T-1` 缺失、`FPG-02` 冲突 `529` ADR-FP-07 | 修正 `base.yml` job/anchor、拆出 `FPG-29..31`、网站同步脚本 | R3 部分关闭 |
| R2 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: filter 精确匹配、`#881` 闭环顺序、Phase 与依赖对齐、`immutable-release` | 修正 `G-03` AC 计数与 `G-09` 双向拆分 | R3 部分关闭 |
| R3 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `FPG-02` 仍写 `LBR-CLI-002`（应为 `LBR-CLI-003`）；缺 `src/command/log.rs` 跨计划写锁 `DEP-FPG-03` | `DEP-FPG-02` 锚点、DAG 摘要、`FPG-19`/`529 FP-10` 继承与 `plan-status.md` 说明 | R4 部分关闭 |
| R3 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: 网站同步缺 `--libra-root` 与写集、`FPG-02` `LBR-CLI-003`、`FPG-01` filter 含 `OR` | 重算 `FPG-01`/`FPG-04` 独立谓词 AC 数 | R4 部分关闭 |
| R4 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `FPG-02` 缺 `useConfigOnly`/`EMAIL` 规则；`FPG-27` 缺 `DEP-FPG-02` | `DEP-FPG-03` 证据、`G-09` 历史拆分行、兼容窗口 | R5 修订 |
| R4 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `FPG-01` filter 缺 `command::` 前缀；`FPG-02` 缺 `useConfigOnly`；`FPG-27` 缺 `DEP-FPG-02`；`FPG-19` 封面三态；D 组 `release.yml` job/CDN 不全 | N/A | R5 修订 |
| R5 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `FPG-19` 误写 tip-to-tip 树对比（应为 series interdiff） | `FPG-30` 回滚不应引用 `--encode-email-headers` | R6 修订 |
| R6 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `FPG-19` 拒绝判据未限定多补丁系列（`total > 1`） | 补充单补丁禁用源与 CLI 覆盖回归 | R7 修订 |
| R8 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `PASS` | 无 P0/P1 | P2: 文头/FPG-19 计数/R7 描述微调 | R9 顺带清理 |
| R8 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `FPG-09` `\|` 合并精确与子串过滤可能掩蔽零命中 | 无 | R9 修订 |
| R9 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: ① 网站路径 `../libra-backend` 与 `AGENTS.md:135` `/Volumes/Data/gitmono/libra-backend` 不一致且缺前端/部署门；② 缺 `COMPATIBILITY.md`/版本面/`plan-status.md`/网站跨计划共享写锁（`DEP-FPG-04`） | 无 | R9 修订 |
| R10 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `DEP-FPG-04` 在提交推送后即释放发布锁（须持续至 `gh release create` 与 D 组核验完成并明确 `plan-status.md` 登记方式） | P2: 粒度审计表 `FPG-19` `AC=5/8` 与卡内 `4/8` 不一致 | R10 修订 |
| R11 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `DEP-FPG-04` 缺少跨工作区可见的原子获取与并发仲裁协议 | 无 | R11 修订 |
| R12 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `DEP-FPG-04` 缺少单一授权发布者身份/授权机制及所有参与计划共同遵守的串行仲裁规则 | 无 | R12 修订 |
| R13 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `DEP-FPG-04` 未将共同仲裁规则合入及各在途计划采纳设为开工前置条件 | 无 | R13 修订 |
| R14 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `PASS` | 无 P0/P1 | P2: `FPG-01`/粒度表/Phase 0 可同步补列 `DEP-FPG-04`（全局依赖表已强制） | R14 复审通过 |
| R15 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `PASS` | 无 P0/P1 | P2: 第 200 行发布者表述与 `DEP-FPG-04` 统一、`FPG-01`/Phase 0 同步列 `DEP-FPG-04`（可接受残余风险） | R15 复审通过 |
| R16 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `de36d50c813c7bb3cc075f386e12783c520d6812a6aa6bdd42cb37296df74937`） | `FAIL` | P1: 「文档与网站验收命令」块缺失；AC → 断言映射机械生成、断言非输出内容；回归命令与精确单匹配协议矛盾；FPG-09/03/19 映射错误；AC 独立谓词低估（FPG-01/19/25 超限）；FPG-22 `--full-diff` 不可达成；FPG-28 子串判定失效；网站页未逐文件列入；锚点过期；DAG 摘要缺卡；未来时间戳；测试矩阵缺 BrokenPipe；追溯表缺具名测试；完成判据树证据不足；`DEP-FPG-04` 未覆盖网站部署 | P2: 断表空行、完成判据结构、`AGENTS.md:135` 路径引用、FPG-10/17/14 措辞、文头状态、未登记复审、既有用例未点名——均已在 R16 修订处理 | R17 复审 |
| R16 | Codex (`gpt-6.1-terra`, high；4 分区) | 全文（同一受审 SHA-256） | `FAIL` | P1: 同上各项（分区 a: 锚点、DAG、未来时间戳、协议矛盾、AC 计数、BrokenPipe、追溯表、最终树证据；分区 b/c/d: 映射断言、悬空文档块、整 target 精确门矛盾、FPG-03/09/19 映射、网站页未列、AC 计数、FPG-28 尖括号） | P2: 修订历史断表空行（已修） | R17 复审 |
| R17 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `a9d455260cc2ef25a63833baedb178a34c20da217faaa77c34cbbec5614adf0d`） | `FAIL` | P1: FPG-01/19/25 axis 与拆出卡重叠；文档块第 8 步未做 HTML 实体解码；第 3 步 HEAD/目录干净判据过严 | P2: 第 1 步标记可能早已存在；FPG-16 AC3 判据；「可行性」漏 DEP-FPG-03/04——均已修 | R18 复审 |
| R17 | Codex (`gpt-6.1-terra`, high；4 分区) | 全文（同一受审 SHA-256） | `FAIL` | P0: 完成判据重跑条件与记账提交形成循环；P1: 网站无变化分支缺失、部署未绑定本卡版本、协议禁止断言错误消息中的键/值、`LBR-CLI-003` 退出码一刀切、FPG-32 AC5 前缀、FPG-04/29/30/25/23 子性质合并计数与映射不全、FPG-17 缺 prerequisite 与行为迁移冲突、FPG-11/14/25 映射不全、FPG-19 缺空格形式与七门族及位置断言 | 无 | R18 复审 |
| R18 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `7a671546d7bb809b2c7bf2e6bb2d414379f76e6c4c2a894ee0241f20da56f976`） | `FAIL` | P1: 文档块第 3 步按 4 个文件判分支；多卡与对应 issue 验收标准不符（FPG-14/26/25/33 相反或无关，FPG-04/05/08/29/16/13/12/17/18/23/30/01 缺项） | P2: `$TMPDIR`、黄金值版本措辞、完成判据缺既有失败与 `plan-long.md` 行——均已修 | R19 复审 |
| R18 | Codex (`gpt-6.1-terra`, high；4 分区) | 全文（同一受审 SHA-256） | `FAIL` | P1: 文档块跨仓库 `cd` 错误、无变化分支判据与证据、部署未绑定 SHA、只复核 EN；线程关系与门族合并计数；`--no-signature` 下区段终点不存在；FPG-04/30 黄金比较受前序缺口影响；FPG-32 `Error-Code` 非首行；FPG-03 多性质；FPG-30 attach/inline 合并；FPG-09 结构化 `message` 前缀；FPG-14 占位符合并；FPG-15 只比文件名与标题；FPG-25/33 比较含运行时 `Date:`；FPG-31 缺反向顺序；FPG-19 门族计数与区段终点；FPG-34 缺 `--stdout` | 无 | R19 复审 |
| R19 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `40d420f700dd81884f9cbd5d33b90b6bf7a5cd2e4816459c20d57a6ae45db691`） | `FAIL` | P1: 「描述段」「列表段」定义循环不可判定 | P2: FPG-14 AC2–AC4 未写完整 argv——均已修 | R20 复审 |
| R19 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL` | P1: `ls-remote` 输出比较、无变化分支 `FINAL` 未定义、本地 `cf` 未推送提交可能混入、`curl` 失败被掩盖、删除型文档无 `PROBE`、网站审查晚于发布；FPG-01 封面信/补丁合并计数；FPG-03 缺非重复范围；多值配置无读取接口；FPG-37 双断言；notes 区块只取首个；FPG-29 依赖 FPG-08 编码且缺短标题；`VER` 计入共享回归；FPG-30 三头与 multipart；FPG-09 既有测试过弱；FPG-31 身份口径；FPG-13 `--no-stat` 判据；FPG-15 等价比较；FPG-33 缺配置取值；FPG-18 缺本地空值覆盖；FPG-19 缺非整数 interdiff；X 型门族迁移口径；FPG-34 区段截断；FPG-21 缺 `ALL CAPS` 与 15/25/35；FPG-27 缺 #904 封面信项；FPG-23 线程比较与 `--no-thread`/`--in-reply-to`；FPG-35 缺 deep 变体；FPG-28 缺无封面信 | P2: 事实基线空系列 JSON 描述（已修） | R20 复审 |
| R20 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `873d34428fe0c17b9bd649fbdaa38d860920b37c0a594ca347d50ddbf1a98c3b`） | `FAIL` | P1: 追溯表来源列未随拆卡与范围调整同步（FPG-01/04/05/14/18/23/27） | 无 | R21 复审 |
| R20 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL` | P0: 文档门发布前后段与本地验收形成循环；P1: 列表段边界、`remote_sha` 吞掉 `ls-remote` 失败、发布后合入树不一致的处置、追溯表与风险表未同步、`sanitize_header_value` 折叠空白、FPG-04 映射不全、FPG-37 只保留判据、FPG-36 交互缺项、FPG-07 `%B` 换行、FPG-30 缺非 ASCII 作者与 `--inline`、FPG-13 重命名摘要与 `--no-stat` 判据、FPG-15 等价比较、FPG-17 缺显式 `--base`、FPG-19 门族计数、FPG-21 15/25/35 合并、FPG-19/20 缺 `-v23` 文件输出、FPG-18 缺自定义 boundary 封面信、FPG-35 单独 `--in-reply-to` 与 Git 不符、归一化线程结构缺邮件边界、FPG-39 缺 `-M` 封面信继承、FPG-27 过期证据行 | P2: FPG-28 锚点说明（保留） | R21 复审 |
| R21 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `1a7456e10f0f1bf1364850038f2cc348de4ef80b98b093d07d6daca9e7ecf6e9`） | `FAIL` | P1: 成功定义未登记 `#888`/`#898` 拒绝契约由 529 交付 | 无 | R22 复审 |
| R21 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL` | P1: FPG-35 无线程 `--in-reply-to` 依赖后续默认值；squash 签名/DCO；`libra show` 管道掩盖失败；终止链兜底；FPG-04 拒绝/清洗二义；FPG-41 具名 ref；`am` 往返与默认状态契约冲突；FPG-29 场景合并；FPG-30 `--inline` 判据；FPG-12/14 `full` 与双配置；FPG-15 只比标题；FPG-43 空值相等与静默；FPG-25 空格形式；FPG-19 无 `-v`；FPG-20/26 版本一致性；FPG-22/27 漏 `diff.rs`；FPG-27 过期证据 | 无 | R22 复审 |
| R22 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `04756b384fa552ef21dae26cd882970fd0b203ecf1c61681769371e6d438415d`） | `PASS` | 无 P0/P1 | P2: FPG-14 AC5 双判定、FPG-43 AC1 比较对象表述——均已在 R22 修订处理 | R23 随双方复审 |
| R22 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL` | P1: diffstat 段对齐空白；`libra show` 管道；追溯表落点漏 `diff.rs`；兼容收口未分支；FPG-04 续行缩进与 `--add-header` 注入；FPG-29 既有测试迁移；FPG-12 `full` 早于实现；FPG-10 裸 `--from`；FPG-14 配置格式；FPG-15 无封面信场景；FPG-16 测试名不一致；FPG-20/26 `Subject:` 前缀与合并计数；FPG-22/27 锚点与路径；FPG-39 `fp06_*` 回归；FPG-23 `--in-reply-to` 描述；线程归一化与 FPG-28 顺序 | 无 | R23 复审 |
| R23 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `1255da6f47a1f4a29a7cb9f5a48181a420eaa184fb986fa0ee52bc686c5ea4c8`） | `PASS` | 无 P0/P1 | P2: FPG-45 夹具 argv（已在 R23 修订处理） | R24 随双方复审 |
| R23 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL` | P1: FPG-28 先行依赖矛盾与 `scope=S`；线程归一化与上游 Perl 语义不符；网站无变化分支对 `$EN` 过严；FPG-02 缺纯环境变量场景；FPG-03 只比标题；FPG-44 `--no-add-header` 语义；FPG-31 缺裸 `--from` 覆盖配置；FPG-15 封面信信封含运行时间；FPG-16 封面信 OID；FPG-11 仓库外安全；FPG-38 `true` 字面 boundary；FPG-19 分叉夹具；FPG-45 私有类型反序列化；FPG-27 错误文案；FPG-23 缺单独 `--in-reply-to`；FPG-40 夹具与显式 64 | P2: FPG-26/22 承接卡（已修） | R24 复审 |
| R24 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `64d871da62f2c0be617a2fb7c469229baf42f1380819ac50fdfc1e6f9627e805`） | `PASS` | 无 P0/P1 | 无 | R25 随双方复审 |
| R24 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL` | P1: 无变化分支 `SITE_SHA` 未赋值；FPG-02/03 `scope=S`；FPG-04 多值 helper 缺跨层与失败判据；注入场景未证明清洗后保留；FPG-03 缺纯非重复范围；FPG-06 版本对照与显式签名；FPG-11 判据合并；FPG-13 单文件夹具与对齐规则冲突；FPG-14 占位符合并与 `log:` 冲突；FPG-26 `-v2.2` 标题与 Git 相反；FPG-20/26 多文件合并；FPG-22/27 写集用目录；FPG-27 夹具；FPG-47 缺命令行顺序覆盖 | P2: FPG-37 承接卡（已修） | R25 复审 |
| R25 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `887d19d54eb5bb9dddc5728086e3dba46680f3ab988519f96f99ed20cada9106`） | `FAIL` | P1: FPG-49 `full` 关键字在 Git 基线不存在；FPG-49 AC3 引用错位；FPG-48 变更段在 Git 输出中不存在；FPG-06 页脚缺尾随空行 | P2: FPG-47 测试名、FPG-11 探针证据归属（均已修） | R26 随双方复审 |
| R25 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL` | P0: FPG-19 系列级 interdiff 与 Git tip 树对比矛盾；P1: 线程归一化漏 `$printing` 状态下的空行；完成判据未检查退出码与未跟踪文件；FPG-37 清洗规则与注入黄金不一致；FPG-04 AC1 合并 `To:`/`Cc:`；缺 `format.headers` `Cc:` 与 `--cc` 折叠；FPG-49 AC3；FPG-48 变更段；FPG-30 缺非 ASCII 签名行判据；FPG-26 缺长选项；FPG-35/47/50 夹具补丁数；FPG-39 封面信存在与重命名夹具 | 无 | R26 复审 |
| R26 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `10ed04d12ffb5db1fe5a91ed40b3417ed5bd56c4af26401c0218bd3b45caf628`） | `PASS` | 无 P0/P1 | P2: FPG-49 Out of scope 可补「本卡依赖其先行完成」（FPG-12 已在其前） | R27 随双方复审 |
| R26 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL` | P1: 完成判据记账后未复查工作区；FPG-02 AC7 未排除 `GIT_COMMITTER_EMAIL`；FPG-04 未要求每封邮件；FPG-29 短标题未比内容；FPG-30 非 ASCII 作者与 Git 不符；FPG-51 签名行未限定正文；FPG-09 合并谓词；FPG-15/24 缺 #892 第 4 项；FPG-26 缺 `-v4rev2` 主题；FPG-22 锚点错误与默认值合并、补丁 diff/diffstat 区段未定义；FPG-39 缺补丁内容；单独 `--in-reply-to` 被归一化掩盖 | 无 | R27 复审 |
| R27 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `f5d4677ed7e018fb6058856d0d98b53c2221d6c3025285a3062293d91a248ce7`） | `PASS` | 无 P0/P1 | P2: FPG-49 Out of scope 措辞（沿用） | R28 随双方复审 |
| R27 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL` | P1: 页面仅调整顺序时选不出区分探针；FPG-02 AC7 含未映射的成功路径；FPG-30 未登记 `no_attach_stays_plain_text` 迁移、AC4 含未映射的编码性质；FPG-25 `auto` 阈值按字符而非字节；FPG-22 默认值合并、上下文夹具不可区分；FPG-23/50 无线程场景未证明产出三封邮件 | 无 | R28 复审 |
| R28 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `046ba94c3c2ce6d050af986cd037aa9d1dbf17fd72faf52343f46ec492d6ae41`） | `PASS` | 无 P0/P1 | P2: FPG-49 Out of scope 措辞（沿用） | R29 随双方复审 |
| R28 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 e `PASS`） | P1: 无变化分支沿用历史部署未证明当前部署；FPG-41 缺先清除再启用另一 ref；FPG-30 缺非 ASCII in-body `From:`；FPG-12 缺 `--cover-letter` 覆盖 `false`；文件输出映射与默认前后状态契约冲突；FPG-13 未判定 diffstat 位置；FPG-20/52「该文件」指代错误 | 无 | R29 复审 |
| R29 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `386d9c7ca6892f65c2cb2849715f7b274db4e210038c0b1a298d25789d906e2d`） | `PASS` | 无 P0/P1 | P2: FPG-49 Out of scope 措辞（沿用） | R30 随双方复审 |
| R29 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 a、e `PASS`） | P1: 规范头区含 `Date:` 而 Libra 取提交者时间；FPG-46 未登记与 #915 的冲突；FPG-17 自动基线未按整个修订列表计算与校验；FPG-18 带值 `--attach` 未验收附件分段 | 无 | R30 复审 |
| R30 | Claude (subagent `claude-opus-5-5`) | 全文（受审 SHA-256 `8a61871f353b48de63bf4e6cb9e6e2d002466a3e73a4422b2ffe2920ad96cb02`） | `PASS` | 无 P0/P1 | P2: FPG-49 Out of scope 措辞（沿用） | R31 随双方复审 |
| R30 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 e `PASS`） | P1: `DEFER-FPG-02` 与 #893 第 3 项冲突；`format.numbered` 未按 Git 逐条处理多层取值；FPG-13 正文多出 `*** SUBJECT HERE ***` 无前置卡负责；FPG-16 AC4 夹具测不出仍取提交者时间；FPG-24 夹具测不出总读当前分支 | 无 | R31 复审 |
| R31 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（受审 SHA-256 `f4d410a18e48f3e33868036eebaa46e9d9259f54d656db455ca84e8518ff300f`） | `FAIL`（5 分区均 `FAIL`） | P1（12 条）：快照契约未允许新建输出目录；`FPG-13` 合并系列 diffstat 与正文模板两轴；`FPG-16` 审计轴遗漏补丁 `Date:`；`FPG-13` AC6 未断言 `Subject:` 头；`FPG-56` 同层夹具不能区分只读首值；`FPG-57` `landing`/`prod-files` 少计 `src/internal/config.rs`；`FPG-05` AC4 夹具无重复 ref；`FPG-11` diffstat 路径不等长违反差异隔离；`FPG-17` AC6 多分支夹具不区分单个修订；`FPG-45` 绝对 `-o` 打印形式与 Git 不符；`FPG-39` AC3/AC5 只比 `diff --git` 行；`FPG-39` AC6 的 `128` 与失败契约冲突 | 无 | R32 修订（见修订历史） |
| R32 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `1eb427832f4c042c8bed0c05211fbf33fd6d76f0e5ed140fe0e97eeb51cd5b71`） | `FAIL` | P1: `FPG-16` AC4/AC5 与 `FPG-59` AC2/AC3 的 `am` 往返映射与两补丁 mbox 夹具不一致（正确实现也失败） | P2: 成功定义 `#905` 漏 `FPG-58`；缺口表计划动作列不全；Phase 1 目标；`FPG-39` AC5 措辞（均 R33 修）；`FPG-33` AC2/AC3/AC5 区分力（登记残余） | R33 修订（见修订历史） |
| R32 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `PASS`（5 分区均 `PASS`） | 无 | P2: `FPG-39` AC5 措辞宽于映射（R33 修） | R33 随双方复审 |
| R33 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `a86a8d1773fa763a5d7f21030fc0a40df95a531a6ae456460cc706ba56d1c2fd`；R32 全文逐卡评审 + 本轮差异逐条核对 + 结构脚本） | `PASS` | 无 | P2: `FPG-33` AC2/AC5 与不读配置的实现结果相同（AC1/AC4/AC7 已区分）；`landing` 口径在含 `tests/compat/` 的卡中不一（均不超 M） | R34 随双方复审 |
| R33 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 b、c `PASS`） | P1: review log `Result` 含非 `PASS`/`FAIL` 取值；`FPG-33` AC3 长首段夹具与默认模式相同；`FPG-20` AC6 只断言 interdiff 标题 | P2: `FPG-04` AC1/AC2 拼接比较丢失邮件边界（R34 修） | R34 修订（见修订历史） |
| R34 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `c504ed8909d93351f39688ba65d47d92d79459cdd197d1673ee424cfef3c3605`；R32 全文逐卡评审 + R33/R34 差异核对 + 映射与配置默认等价专项扫描） | `PASS` | 无 | P2: `landing` 口径不一（均不超 M）；`FPG-33` AC2/AC5 只检出取值误解析 | R35 随双方复审 |
| R34 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 b、c `PASS`） | P1: `DEP-FPG-01` 判据未要求 `CLOSE-529`；review log 有无 P0/P1 的 `FAIL` 行；`FPG-13` 把 `--root` 等同无唯一边界；`FPG-34` 黄金比较关闭签名测不出 interdiff 与签名的相对位置；`FPG-28` AC5 只查 `Message-ID:` | 无 | R35 修订（见修订历史） |
| R35 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `7f307276cddab0dc47199c077e907c733067d985da65c37e9243c46811918ccc`；R32 全文逐卡评审 + R33–R35 差异核对 + 位置类 AC 专项扫描） | `PASS` | 无 | P2: `landing` 口径不一（均不超 M）；`FPG-33` AC2/AC5 只检出取值误解析 | R36 随双方复审 |
| R35 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 a、b、d、e `PASS`） | P1: `FPG-07` 声称封面信列表与 `%s` 用拼接首段但无 AC | P2: 协议「补丁 diff 段」未注明映射可启用签名（R36 修） | R36 修订（见修订历史） |
| R36 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `7169bf454ece2cebba236d40fc1a176e588780a9549e9e3959d29464f9d9d3e3`；R32 全文逐卡评审 + R33–R36 差异核对 + 描述承诺/AC 覆盖专项扫描） | `PASS` | 无 | P2: `landing` 口径不一（均不超 M）；`FPG-33` AC2/AC5 只检出取值误解析 | R37 随双方复审 |
| R36 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 a、c、e `PASS`） | P1: `FPG-57` `--no-to`/`--no-cc` 夹具不能检出同时清空两类收件人；`FPG-17` AC6 两次调用计为一条致超 8；`FPG-43` 未覆盖 `format.useAutoBase=false` | 无 | R37 修订（见修订历史） |
| R37 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `378e341e5b933d69762af373957bd4d9ae4ec5958e13491d7074b0b6edcffdf5`；R32 全文逐卡评审 + R33–R37 差异核对 + 多调用/否定夹具/配置取值专项扫描） | `PASS` | 无 | P2: `landing` 口径不一（均不超 M）；`FPG-33` AC2/AC5 只检出取值误解析 | R38 随双方复审 |
| R37 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 c、e `PASS`） | P1: ADR-FPG-01 MIME 判定与 FPG-30 冲突；`FPG-03` 把 `b..a` 等同单 merge base 截取；`FPG-24` 合并分支描述与默认 `From:` 两轴 | 无 | R38 修订（见修订历史） |
| R38 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `f3452cddbc094cad6c8e06f911fb20b40c4e1f58412b867cc732f2853b98b2ba`；R32 全文逐卡评审 + R33–R38 差异核对） | `PASS` | 无 | P2: `landing` 口径不一（均不超 M）；`FPG-33` AC2/AC5 只检出取值误解析 | R39 随双方复审 |
| R38 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 a–d `PASS`） | P1: `FPG-22`（`-U` 与 `--full-diff`）、`FPG-39`（重命名与写入失败）、`FPG-28`（Message-ID 与线程头位置）各合并两个可独立恢复的行为轴 | 无 | R39 修订（见修订历史） |
| R39 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `5e2baeebdd3b5559e518ccf4f88b7d6e97fe88b1c08764c48a2b31e6c2dd05a2`；R32 全文逐卡评审 + R33–R39 差异核对） | `PASS` | 无 | P2: `landing` 口径不一（均不超 M）；`FPG-33` AC2/AC5 只检出取值误解析；卡数增至 63 | R40 随双方复审 |
| R39 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 a–c `PASS`） | P1: `FPG-17` 合并自动基线与 base 区块位置两轴；`FPG-62` 一条 AC 含两个可独立失败的谓词；（不采纳）G-09 新卡正文位置，理由见修订历史 | P2: GAP-FPG-22 计划动作缺 `FPG-61`；测试矩阵「全部 35 项」措辞（R40 修） | R40 修订（见修订历史） |
| R40 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `d0ff84ca58b13fe3b9fed5e6199f91f877f7cac8f0bca501300a290b50238f24`；R32 全文逐卡评审 + R33–R40 差异核对 + 重排前后卡片正文逐字比对） | `PASS` | 无 | P2: `landing` 口径不一（均不超 M）；`FPG-33` AC2/AC5 只检出取值误解析；卡数增至 64 | R41 随双方复审 |
| R40 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256） | `FAIL`（分区 a、b、c、e `PASS`） | P1: `FPG-64` 只检查 `base-commit:` 所在邮件，测不出 `prerequisite-patch-id:` 留在其他邮件的实现，且未覆盖封面信位置 | 无 | R41 修订（见修订历史） |
| R41 | Claude (`claude-opus-5-5`，评审 Agent 本身，未派生子代理) | 全文（受审 SHA-256 `7249da6bea021ee9524d3c00b0198761b9d0b632e33b81e09aba780a5287ebab`；R32 全文逐卡评审 + R33–R41 差异核对） | `PASS` | 无 | P2（接受为残余风险）: `landing` 口径在含 `tests/compat/` 的卡中不一（均不超 M）；`FPG-33` AC2/AC5 只检出取值误解析；卡数增至 64 | 计划提交 |
| R41 | Codex (`gpt-6.1-terra`, high；5 分区) | 全文（同一受审 SHA-256 `7249da6bea021ee9524d3c00b0198761b9d0b632e33b81e09aba780a5287ebab`；分区 d 完整复审，分区 a、b、c、e 在 R40 对 `d0ff84ca…` 完整 PASS 后做差异确认） | `PASS`（5 分区均 `PASS`） | 无 | 无 | 计划提交 |

### 非评审轮与未完成评审记录

以下记录不是完整评审轮（中断、未运行或未重跑、未登记 P0/P1 的结论、专项清扫、未完成分区评审），不计入上表的 `PASS`/`FAIL` 结论，只保留其真实发生的事实与处置；上表每个 `FAIL` 均列出 P0/P1。

| 记录 | 执行者 | 范围 | 状态 | 发现 | 处置 | 去向 |
|---|---|---|---|---|---|---|
| R5 Claude（中断） | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | 历史记录结论为 `FAIL`，但该轮未形成含 P0/P1 的完整评审，如实保留 | 网络中断（`ECONNRESET`） | 无 | R8 重跑 |
| R7 Codex（结论 FAIL 但未登记 P0/P1） | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | 历史记录结论为 `FAIL`，但未登记任何 P0/P1（按 ER-05 不能作为 `FAIL` 轮），如实保留 | 无 P0/P1 | P2: 28 卡 `(new)` 测试使用子串过滤而非 `test(=command::...)` 精确过滤 | R8 修订 |
| R9 Claude（未重跑） | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | 历史记录结论为 `FAIL`，但该轮未形成含 P0/P1 的完整评审，如实保留 | 未重跑（R8 已 `PASS`，随 R9 修订一并复审） | 无 | R10 重跑 |
| R10 Claude（未运行） | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | 历史记录结论为 `FAIL`，但该轮未形成含 P0/P1 的完整评审，如实保留 | 未运行（随 R10 修订一并复审） | 无 | R11 重跑 |
| R11 Claude（未运行） | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | 历史记录结论为 `FAIL`，但该轮未形成含 P0/P1 的完整评审，如实保留 | 未运行（随 R11 修订一并复审） | 无 | R12 重跑 |
| R12 Claude（未运行） | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | 历史记录结论为 `FAIL`，但该轮未形成含 P0/P1 的完整评审，如实保留 | 未运行（随 R12 修订一并复审） | 无 | R13 重跑 |
| R13 Claude（未运行） | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | 历史记录结论为 `FAIL`，但该轮未形成含 P0/P1 的完整评审，如实保留 | 未运行（随 R13 修订一并复审） | 无 | R14 重跑 |
| R14 Claude（未运行） | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | 历史记录结论为 `FAIL`，但该轮未形成含 P0/P1 的完整评审，如实保留 | 未运行（待 R15 复审） | 无 | R15 重跑 |
| R15+ Codex（未编号复审） | Codex（2026-11-18 两次未编号复审） | `plan-20261117-format-patch.md` | 历史记录结论为 `FAIL`（据修订历史「Codex 复审 FAIL 修复」行），但该轮未形成含 P0/P1 的完整评审，如实保留 | 原始 P0/P1 条目与轮次编号未登记，R16 无法追溯 | 无 | R16 补登记；以修订历史 2026-11-18 08:19:08 与 09:43:54 两行为准 |
| R30 补充（专项清扫） | Claude 子代理专项清扫（6 分区，逐卡逐 AC） | 全文（R30 受审 SHA-256 `8a61871f353b48de63bf4e6cb9e6e2d002466a3e73a4422b2ffe2920ad96cb02`） | 清扫，非评审轮 | 约 56 条 P1 级夹具区分力与 Git 事实问题，按修订历史 R30 行处理 | 约 66 条 P2，部分采纳，余者登记为残余风险 | R31 复审 |
| R31 Claude（未完成） | Claude (subagent `claude-opus-5-5`) | 仅分区 1（第 355–1191 行，`FPG-01`..`FPG-05` 区间；受审 SHA-256 `f4d410a18e48f3e33868036eebaa46e9d9259f54d656db455ca84e8518ff300f`） | 未完成：分区 1 `PASS`，分区 2–5 因上下文耗尽中止，不构成全文结论 | 分区 1 无 P0/P1 | P2 7 条：`FPG-57` AC1 交错夹具、AC7 出处、描述中无 AC 的小写匹配与 `--add-header` 并入、`FPG-04` 夹具不混合头、`FPG-05` AC4 重复 ref、`FPG-03` 残留措辞、`FPG-02` 文档行；R32 采纳第 1–4、6 条，第 5 条随 Codex P1 关闭，第 7 条驳回（见修订历史） | R32 随双方复审 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-FPG-01 | `format-patch --range-diff` | 不在本次 35 项 issue 范围内 | 用户或新 issue 明确提出需求 | `COMPATIBILITY.md` |
| DEFER-FPG-03 | 封面信与补丁 diffstat 的 `--summary` 行（`rename … (100%)`、`create mode …`）及名称对齐、`0 deletions(-)` 子句等版式对齐 Git | 由 529 FP-05 起共享的 stat 渲染器产生，已由 529 `DEFER-FP-02` 登记，修正需独立计划；`#905` 第 1 项的 `rename file => foo (100%)` 行因此在本计划内未闭环（`#905` 部分闭环、保持 open） | 共享 stat 渲染器另立计划修正后，在本计划或后续计划补 `#905` 第 1 项的 Git 黄金比较并关闭 `#905` | 529 `DEFER-FP-02` 的承接计划 |
| DEFER-FPG-04 | 不在 35 项 issue 范围内的已知 CLI 差异：Git 拒绝路径的 `fatal:` 前缀与 128 退出码（Libra CLI 类稳定码默认 `error:`/129，见「验证执行协议」第 4 项失败契约）；`--attach` 与 `--inline` 同时给出时 Git 以最后出现者为准（Libra 既有 `attach_and_inline_are_mutually_exclusive` 以 `LBR-CLI-002` 拒绝）；`--signature` 与 `--signature-file` 同时给出时 Git 取前者（Libra clap `conflicts_with` 拒绝）；`-U 1`（值与选项分开）在 Git 中把 `1` 当作修订；无范围参数的 `--ignore-if-in-upstream` 在 Git 中报 `need exactly one range`；Libra 保留 Git 基线不接受的 `--commit-list-format=short` 别名 | 均非 35 项 issue 的验收项，改动会触及既有用例与公开契约 | 用户或新 issue 明确提出需求 | `COMPATIBILITY.md:224` 注明，后续独立计划 |
| DEFER-FPG-05 | `--ignore-if-in-upstream` 在多 merge base（criss-cross）历史下的上游 patch-id 集合不单列验收：`FPG-03` 按 Git 的可达集合差 `b..a` 实现（不按单个 merge base 截取），AC3 以线性分叉夹具验收 | 多 merge base 组合不在 #882 验收标准（t4014#4 与「上游已有 patch-id 的提交不输出、不同变更仍输出」）之内；为该组合另设夹具需在本卡构造交叉合并与回退，超出 #882 诉求（R37 Codex b 提出，按范围控制登记）；#882 由 `FPG-03` 按其验收标准完整闭环，不因本项保持 open | 新 issue 报告多 merge base 历史下 `--ignore-if-in-upstream` 与 Git 不一致 | 新 issue；实现位置仍为 `FPG-03` 所改的 `resolve_range_commits` |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [ ] 全量收口门须在**最终代码树**的干净检出上执行：运行前后 `libra status --untracked-files=all --porcelain` 均须退出码为 0 且输出为空（`.env.test`/`.env.live-test` 由忽略规则排除），记录 `TESTED_HEAD=$(libra rev-parse HEAD)`、`libra rev-parse 'HEAD^{tree}'`（两者退出码均须为 0）与结果；任一命令非零退出即 STOP，不得以空输出判定干净。此后登记门结果、SHA、状态等证据的记账提交只允许改动 `docs/development/plan/**`（本计划文件与 `plan-status.md`）；每次记账提交后以 `libra diff --name-only "$TESTED_HEAD" HEAD`（退出码须为 0，非零即 STOP）证明改动只落在 `docs/development/plan/**`，并再次运行 `libra status --untracked-files=all --porcelain`（退出码须为 0 且输出为空，非零即 STOP；存在任何未提交或未跟踪改动时先处理再按本条重判）；最终标记计划完成前再做一次同样的工作区检查——满足则不重启全量门，任何代码、测试、文档、配置或版本面变化则必须在新树上重跑。终止链末端记账提交的 D 组结果按模板 v2.14 预登记：不在该提交自身登记，由下一次 `plan-status.md` 例行同步补记其 SHA、run ID、结论与观察 UTC 时间；补记搭车既有例行同步提交，若至下一工作日仍无例行同步，由操作者以一笔明确的 `plan-status.md` 例行维护提交补记（该提交属 `plan-status.md` 维护，不属本计划终止链、不重启终止链）。
- [ ] 计划文本提交门：Claude（Subagent）与 Codex 双审对同一受审 SHA-256 均返回字面结论 `VERDICT: PASS` 且 P0/P1 全部关闭；网站交付另需独立 Claude Code `VERDICT: PASS`，两者互不替代。
- [ ] 网站交付门：每张卡均按「文档与网站验收命令」第 1–8 步完成仓库文档逐文件验收、第 6 步合入树复核、`/Volumes/Data/gitmono/libra-backend` 的同步与只读漂移检查；网站页有变化的卡另完成前端验证、对最终字节取得独立 Claude Code `PASS`、精确暂存、签名+DCO 提交、核对提交树与已审字节一致、推送 `cf`（`ls-remote` 回读）并以构建记录与线上页面核验部署；网站页无变化的卡记录摘要、`SITE_SHA` 与当前生产部署的核验记录；计划文本 Claude 与 Codex 双审不替代该网站门，任一环节失败即 STOP 且不得标 `complete`。
- [ ] 所有任务卡满足粒度规则 `G-*`：无未登记的 L 例外、无 XL 卡、无碎片卡、无未登记的合并发布例外、实现写集冲突均已消解；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）；停在 `remote-pending` 的卡须先取得 D 组与网站部署证据，`blocked` 的卡须先解除阻塞或按 `DEFER-*` 正式延后。
- [ ] 所有任务的 Verification 命令已运行并记录结果。
- [ ] **计划完成门 / 全量收口门（ER-13）**：全部任务卡完成后运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿。
- [ ] **全量收口门暴露的 Bug 已全部处理（ER-13）**：定位到本计划的失败已前滚修复并重跑全量至全绿；判定为既有失败的用例已附「计划基线 commit 上同样失败」的复现证据并登记 `FIX-*` 或 `DEFER-*`，不得带着红色全量结果通过完成门。
- [ ] 必要的 docs/compat/test-index 更新已完成。
- [ ] 每张卡的 `Rollback mode` 已被验证或记录。
- [ ] Claude 与 Codex 最终评审结论均为 `PASS`，P0/P1 全部关闭。
- [ ] 每张 `independent` 发布卡已执行 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新、构建/安装/提交/推送及 `gh release create` 完成并取得证据（ER-08）。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
- [ ] `plan-long.md`：`N/A`（本计划不改写长期路线图，由协调者统一维护日期计划索引）。
