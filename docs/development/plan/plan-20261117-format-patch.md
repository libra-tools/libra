# format-patch Git 兼容补齐计划（2026-11-17）

> 模板版本：v2.16；继承 [plan-template.md](plan-template.md) 的 GC-01..13、ER-01..14、G-01..11。
> 状态：计划已修订（R13），尚未执行；记录时间 2026-11-17 05:09:00 UTC，全部任务 pending。
> 跟踪范围：GitHub open issues #880–#891、#892–#901、#903–#906、#908–#912、#914–#917（共 35 项）。

## 文档职责

本文解决 `libra format-patch` 与 Git `git format-patch`（基线 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`，v2.56.0-116，t4014/t4015/t4021）之间的 35 项已登记兼容缺口，目标是使 `format-patch` 在序列号格式、邮件头编码与折叠、配置项级联、封面信、文件名生成、diff 选项透传、`--base=auto`、`--interdiff`、`--relative` 与线程化头等方面达到与 Git 一致的 CLI 与 mbox 输出契约。

本文只规划任务，不宣称实现完成。落地时每个任务都必须先刷新源码锚点，再按任务卡验收（GC-01）。

### 适用范围

- 命令与 CLI：`libra format-patch` 参数解析、校验与执行（`src/command/format_patch.rs`、`src/cli.rs`、`src/command/log.rs`、`src/command/diff/`）。
- 配置与环境变量：`format.numbered`、`format.headers`、`format.to`、`format.cc`、`format.notes`、`format.signature`、`format.from`、`format.coverLetter`、`format.coverFromDescription`、`format.attach`、`format.thread`、`format.useAutoBase`、`format.filenameMaxLength`、`diff.relative`、`branch.<name>.description`、`GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`。
- 测试与文档：`tests/command/format_patch_test.rs`（`command_test` 目标）、`tests/compat/format_patch_mail_roundtrip_test.rs`、`tests/compat/format_patch_flag_surface.rs`、`docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md`（第 224 行原地更新，不增删行）及通过 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra` 同步的 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`。

### 非目标

- 不实现邮件发送能力（`libra send-email` 仍按 `COMPATIBILITY.md` 保持 `unsupported`）。
- 不实现 `--range-diff`（仅实现 #898 要求的 `--interdiff`）。
- 不重复实现 `issues/529.md` 已承接并交付的能力（如 `-N` 文件名、`09/10` 主题计数宽度、`--numstat`、`--output`、`-M`/`--no-renames`）；对 #880、#881、#888、#899、#904、#909 中与 `529.md` 重叠的部分，按 GC-01 在前置依赖完成后核实并只补齐剩余缺口。
- 不更改 `libra am` / `libra mailinfo` 的既有解析契约。

### 成功定义

- 35 个关联 issue（#880–#917 指定集合）全部在「追溯表」与任务卡中闭环，对应行为与边界测试通过。
- `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md` 与后端网站文档同步反映新增 flags 与 config。
- 全部任务卡完成后通过 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 及 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。

## 使用规则

- 每次状态推进与「修订历史」写入都记录实际 `YYYY-MM-DD HH:MM:SS UTC` 时间。
- 推进或完成任务卡时，执行者须在同一变更中同步更新 [`plan-status.md`](plan-status.md)（本计划入库时的初始登记行由协调者随本批计划合入一并登记）。
- 本文全部任务卡均未开工；任务 ID（`FPG-01`..`FPG-31`）固定，拆分时按 G-09 在所属 Phase 末尾追加新编号并登记「修订历史」。
- 本计划不单独新建证据目录或证据文件（v2.12），所有门禁结果、SHA 与测试摘要写在本计划内。

## 事实基线

> 所有行号和源码锚点必须在开工当天刷新。以下锚点核对于 `2026-11-17 03:08:00 UTC`（`Cargo.toml` = `0.30.207`）。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `FormatPatchArgs`（97-364）、`validate_cli_args`（437-458）、`execute_safe`（474-628）、`resolve_range_commits`（663-756）、`format_patch_body`（782-1002）、`format_cover_letter`（1361-1445）、`resolve_format_config`（1861-1902） | `src/command/format_patch.rs:97-2091` |
| 数据/状态 | 只读 commit/tree/blob/notes/config，不修改 SQLite/HEAD/index；非 `--stdout` 写补丁文件采用单文件 `atomic_write`（后序文件失败会保留已写入的前序文件） | `src/command/format_patch.rs:596-619,1824-1828` |
| 用户命令 | `Commands::FormatPatch(command::format_patch::FormatPatchArgs)`、`validate_cli_args` 预检、分发到 `execute_safe` | `src/cli.rs:498-500,3612-3613,3902-3903` |
| 机器输出 | `--json` 输出 `FormatPatchOutput { patches: Vec<PatchRecord> }`（字段 `number`/`commit`/`subject`/`path`）；空系列为 `[]` | `src/command/format_patch.rs:520-529,621-625,634-645` |
| 错误码 | `FormatPatchError` 映射 `LBR-REPO-001`、`LBR-CLI-002`、`LBR-CLI-003`、`LBR-IO-001`/`002`、`LBR-UNSUPPORTED-001` | `src/command/format_patch.rs:370-423,500-513`、`docs/error-codes.md` |
| 文档 | 中英文命令页、开发文档及兼容矩阵第 224 行已存在（`tests/compat-ledger/t4/` 锚定 `COMPATIBILITY.md` 行号，仅可原地修改第 224 行；后端 `/Volumes/Sea/libra-backend` 为 Libra 仓库，分支 `cf`，在 `/Volumes/Sea/libra-backend` 执行 `python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra` 同步） | `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224` |
| 测试 | `tests/command/format_patch_test.rs` 挂载为 `command_test` 模块；compat 独立 target：`compat_format_patch_mail_roundtrip`、`compat_format_patch_flag_surface`、`compat_broken_pipe_output` | `tests/command/mod.rs:1050`、`Cargo.toml:342-347`、`tests/INDEX.md:86,88,179` |
| 外部参照 | 上游 Git 测试树固定 `git@6de20f6092dcf9bdb1c8efe03db4b70c82b423dd`（v2.56.0-116，t4014/t4015/t4021）；在途同域计划 `issues/529.md`（`FP-02` done，`FP-03` remote-pending，`FP-04..13` pending） | `docs/development/plan/issues/529.md:3,107`（2026-11-17 核对） |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-FPG-01 | `#880`（已由 `529 FP-02` 修复 Subject 计数宽度）与 `#896`：多补丁默认不编号且忽略 `format.numbered` | 系列补丁缺少 `1/N` 默认编号 | `src/command/format_patch.rs:855-861,1861-1902` | FPG-01 |
| GAP-FPG-02 | `#881`：`resolve_signoff_identity` 忽略 `GIT_COMMITTER_*`（identity 由 `529 FP-11` ADR-FP-07 承接；t4036 端到端另依赖 `#897` MIME 头） | 环境变量身份验证与 t4036 兼容闭环未完成 | `src/command/format_patch.rs:2050-2065`、`issues/529.md:179` | FPG-02（身份）、FPG-30（#881 完整闭环） |
| GAP-FPG-03 | `#882`：`format-patch --ignore-if-in-upstream` 在 t4014#4 带 tag 场景多输出 1 个补丁 | 上游已合入提交未被正确过滤 | `src/command/format_patch.rs:688-750,1126-1213` | FPG-03 |
| GAP-FPG-04 | `#883`/`#914`：忽略 `format.headers`/`format.to`/`format.cc`，且 `--no-to`/`--no-cc` 抹除后续 `--to`/`--cc` | 无法通过配置注入邮件头，CLI 顺序覆盖不符 Git | `src/command/format_patch.rs:194-210,1861-1930` | FPG-04 |
| GAP-FPG-05 | `#884`：缺少 `--no-notes` 且忽略 `format.notes` | 无法通过配置启用或 `--no-notes` 关闭 notes | `src/command/format_patch.rs:236-240,1012-1057,1861` | FPG-05 |
| GAP-FPG-06 | `#885`：默认 signature 省略产品前缀 `libra ` 且忽略 `format.signature` | 签名页脚不符合预期且不可配置 | `src/command/format_patch.rs:1062-1071,1861` | FPG-06 |
| GAP-FPG-07 | `#911`：`commit_subject_line` 仅取首行而不拼接首段 | 多行首段被截断 | `src/command/format_patch.rs:833,1497-1500` | FPG-07 |
| GAP-FPG-08 | `#887`：默认不编码非 ASCII 邮件头 | 与 Git 默认 `--encode-email-headers` 不符 | `src/command/format_patch.rs:345-358,1981-2018` | FPG-08 |
| GAP-FPG-09 | `#888`：已由 `529 FP-03` 落地 `validate_cli_args` 与 `fp03_empty_rfc_with_k`，需在 `529` 合入后按 GC-01 复核关闭 | 需确认无遗留 `-k` 与 `--subject-prefix`/`--rfc` 边界差异 | `src/command/format_patch.rs:437-458`、`tests/command/format_patch_test.rs:2589` | FPG-09 |
| GAP-FPG-10 | `#889`：`parse_from_ident` 接受无邮箱字符串输出 `From: foo <>` | 产生非法 `From:` 邮件头 | `src/command/format_patch.rs:2022-2045` | FPG-10 |
| GAP-FPG-11 | `#890`：`--relative` 硬编码拒绝且忽略 `diff.relative` | 子目录下无法生成相对路径补丁 | `src/command/format_patch.rs:285-290,500-506,1852-1859` | FPG-11 |
| GAP-FPG-12 | `#900`/`#909`：忽略 `format.coverLetter`（`--commit-list-format` 隐含封面由 `529 FP-13` 承接，需复核） | 无法通过配置自动生成封面信 | `src/command/format_patch.rs:178-180,549-564,1861` | FPG-12 |
| GAP-FPG-13 | `#905`：封面信省略系列 diffstat 汇总 | 封面信缺少变更统计 | `src/command/format_patch.rs:1361-1445` | FPG-13 |
| GAP-FPG-14 | `#899`：`render_pretty_format_commit` 仅支持 7 个占位符（`shortlog` 由 `529 FP-13` 承接） | Git 常用 `--commit-list-format` 占位符被拒 | `src/command/format_patch.rs:1621-1644` | FPG-14 |
| GAP-FPG-15 | `#892`：仅接受单个 `revision_range`，拒绝多参数及 `^<rev>` 排除语法 | `format-patch A ^B` 无法执行 | `src/command/format_patch.rs:108-109,663-685` | FPG-15 |
| GAP-FPG-16 | `#893`：mbox `"From "` 分隔符使用提交/当前时间而非 Git 固定 `Mon Sep 17 00:00:00 2001` | 与 Git mbox 黄金输出不一致 | `src/command/format_patch.rs:794-807,1373-1376` | FPG-16 |
| GAP-FPG-17 | `#894`：`--base=auto` 与 `format.useAutoBase` 未实现 | 无法自动探测上游基线提交 | `src/command/format_patch.rs:242-247,534,1214-1284` | FPG-17 |
| GAP-FPG-18 | `#895`/`#912`：`--attach` 为布尔 flag 拒绝 `--attach=<name>` 且忽略 `format.attach` | 无法自定义 boundary 或从配置开启附件 | `src/command/format_patch.rs:249-256,883-905,1861` | FPG-18 |
| GAP-FPG-19 | `#898`：不支持 `--interdiff=<rev>`（三态前置检查由 `529 FP-10` 承接，实际输出未实现） | 无法生成系列间 interdiff | `src/command/format_patch.rs:97-364,1361-1445` | FPG-19 |
| GAP-FPG-20 | `#901`：文件名缺少 `vN-` reroll 前缀，且文件路径打到 `stderr` 而非 `stdout` | `-v` 文件名与脚本读取 stdout 失败 | `src/command/format_patch.rs:610-612,1699-1737` | FPG-20 |
| GAP-FPG-21 | `#908`/`#910`：`patch_slug` 强制小写且 `truncate_slug` 硬编码 52 字符上限忽略 `--filename-max-length` | 文件名大小写被破坏且长度被卡在 56 字符 | `src/command/format_patch.rs:1646-1681,1699-1737` | FPG-21 |
| GAP-FPG-22 | `#904`（部分）：`-U`/`--unified` 与 `--full-diff` 未支持 | 无法控制补丁上下文行数与全量 diff | `src/command/format_patch.rs:258-284,1852-1859` | FPG-22 |
| GAP-FPG-23 | `#917`、`#916`（部分）：默认开启 `--thread`、忽略 `format.thread=false`/空、拒绝 `--thread=deep` | 默认行为与配置开关不符 Git | `src/command/format_patch.rs:181-189,1285-1353,1861` | FPG-23 |
| GAP-FPG-24 | `#891`：封面信忽略 `branch.<name>.description` 且默认 `From:` 留空 | 封面信缺少分支描述与发件人 | `src/command/format_patch.rs:1386-1396,1422-1424` | FPG-24 |
| GAP-FPG-25 | `#906`：不支持 `--cover-from-description` 且未校验 `format.coverFromDescription` | 无法控制封面信描述来源且非法配置未报错 | `src/command/format_patch.rs:97-364,1861-1902` | FPG-25 |
| GAP-FPG-26 | `#903`：`--reroll-count` 仅接受 `usize` 拒绝小数/非数字值 | `-v 2.1` 等 Git 合法值被拒绝 | `src/command/format_patch.rs:220-226` | FPG-26 |
| GAP-FPG-27 | `#904`（部分）：`--diff-filter` 未支持（`-M`/`--no-renames` 由 `529 FP-06` 承接，需复核） | 无法按变更类型过滤补丁文件 | `src/command/format_patch.rs:258-284,1852-1859` | FPG-27 |
| GAP-FPG-28 | `#916`（部分）：`build_thread_id` 生成互相为子串的 Message-ID | 邮件线索匹配出现子串碰撞 | `src/command/format_patch.rs:1285-1353` | FPG-28 |
| GAP-FPG-29 | `#886`：长 `Subject:` 邮件头行不按 RFC 2822 折叠 | 超长 Subject 单行输出 | `src/command/format_patch.rs:863-869` | FPG-29 |
| GAP-FPG-30 | `#897`（及 `#881` t4036 闭环）：纯 ASCII 补丁仍强制输出 MIME 头 | 与 Git 纯 ASCII 省略 MIME 行为不符 | `src/command/format_patch.rs:893-905,1415-1417` | FPG-30 |
| GAP-FPG-31 | `#915`：缺少 `--no-from` 与 `format.from` 配置 | 无法通过配置控制发件人 | `src/command/format_patch.rs:212-217,1861-1902` | FPG-31 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `issues/529.md` | **前置写锁持有者**：`529.md` 当前持有 `src/command/format_patch.rs` 与 `src/cli.rs` 写锁 | 通过 `DEP-FPG-01` 严格等待 `issues/529#CLOSE-529` 完成释放写锁后再启动 `FPG-01`；对 `529` 已交付的 `#880`/`#888`/`#881`/`#899`/`#904`/`#909` 部分按 GC-01 核实并只补齐剩余缺口 |
| `plan-20260927.md` | `CM-08`/`CM-09` 涉及 `src/command/diff/` 与 `diff/{compare,render}.rs` | 通过 `DEP-FPG-02` 在 `FPG-22`、`FPG-27` 开工前核实无在途写者再串行修改 `src/command/diff/` |
| `issues/1040.md` / 同批 `log-show` 计划 | 均触及 `src/command/log.rs` | 通过 `DEP-FPG-03` 在 `FPG-11`/`FPG-22`/`FPG-27` 开工前核实 `src/command/log.rs` 无在途写者并串行协调 |
| `plan-long.md` | 长期兼容路线 | 本计划为具体执行落地，不直接改写 `plan-long.md` |
| `COMPATIBILITY.md` | 第 224 行 `format-patch` 兼容契约（被 `compat_ledger_schema` 锚定行号） | 仅原地修改第 224 行，不增删行 |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 35 项 issue 均指向明确 Git `format-patch` 兼容缺口 | 拆分为 31 张单轴卡，与 `issues/529.md` 边界清晰 |
| 可行性 | 集中在 `format_patch.rs` 及 `log.rs`/`diff/` | 通过 `DEP-FPG-01` 与 `DEP-FPG-02` 避开在途写锁，全串行交付 |
| 任务卡粒度 | R3 已进一步拆出 `FPG-29..31`；G-03 计数排除强制文档门并按独立谓词核定；G-09 原卡均已双向登记拆出卡 | 全部卡 `AC ≤ 8`、`VER ≤ 8`、`scope ∈ {S, M}` |
| 依赖与顺序 | 线性 DAG `DEP-FPG-01 -> FPG-01 -> ... -> FPG-17 -> FPG-24 -> FPG-25 -> FPG-18 -> FPG-19 -> FPG-20 -> FPG-26 -> FPG-21 -> FPG-22 -> FPG-27 -> FPG-23 -> FPG-28`（Phase 内闭环，叠加 `DEP-FPG-02` 与 `DEP-FPG-03` 跨计划写锁协调） | 无环、无 I–I 并发冲突、Phase 出入条件与依赖链一致 |
| 完整性 | 每卡含 `nextest` 精确 filter、ER-13 `T-1` 全量触发、D 组真实 CI job、EN/zh/dev/后端脚本同步/`COMPATIBILITY.md` | 符合 ER-04、ER-06/ER-06a、ER-08、ER-13、ER-14 |
| 安全性 | 邮件头控制字符过滤、`--relative`/`--output-directory` 路径校验 | 继承 GC-07、GC-11 |
| 功能正确性 | 依据 `git@6de20f6092` 与 `529` ADR-FP-07 对齐每个边界 | 消除二义性判据与 `#881` 过早关闭 |
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
| 2026-11-17 03:12:00 UTC | Claude R3 FAIL + Codex R3 FAIL | ① 全卡 `Implementation write set` 补齐 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`，同步命令补 `--libra-root /Volumes/Data/gitmono/libra` 与执行目录；② `FPG-02` 错误码改为 `LBR-CLI-003` 并覆盖全部 ADR-FP-07 失败消息；③ `FPG-01` 验证命令拆出独立精确门；④ 补全 `DEP-FPG-03`（`src/command/log.rs` 跨计划写锁）并修正 `DEP-FPG-02` 锚点；⑤ 重算 `FPG-01`/`FPG-04` 独立谓词 AC 数；⑥ `FPG-19` 明确继承 `529 FP-10` 三态前置检查并撤回不支持分类 | N/A | 依赖表、粒度表、FPG-01/02/04/11/19/22/27、全局约束、Review log |
| 2026-11-17 03:22:00 UTC | Claude R4 FAIL + Codex R4 FAIL | ① `FPG-01` 精确 filter 改为 `test(=command::format_patch_test::format_numbered_config)`；② `FPG-02` 补齐 `user.useConfigOnly` 缺邮箱失败、`EMAIL` 回退及独立 AC 计数（`AC=5/8`）；③ `FPG-27` 补入 `DEP-FPG-02`；④ `FPG-19` 补齐继承 `529 FP-10` 封面信三态回归门与分叉系列对照（`AC=4/8`）；⑤ D 组补全 `release.yml` 5 个 job 与 CDN 检查；⑥ 修正 `DEP-FPG-01`/`DEP-FPG-03` 锚点与兼容窗口说明 | N/A | D 组门、依赖表、实施顺序、粒度表、FPG-01/02/19/27、Review log |
| 2026-11-17 03:28:00 UTC | Codex R5 FAIL | ① `FPG-19` 明确采用 series-level interdiff（比较旧系列与当前系列变更，而非分叉基线直接 tip-to-tip 树对比）；② 修正 `FPG-30` 回滚说明，移除 `--encode-email-headers` 可恢复纯 ASCII MIME 头的误述 | N/A | 字段全局默认与例外、FPG-19、Review log |
| 2026-11-17 03:32:00 UTC | Codex R6 FAIL | `FPG-19` 明确仅在多补丁系列（`total > 1`）且封面信最终状态被显式禁用时拒绝 `--interdiff`，补充单补丁禁用源与 CLI 覆盖配置回归 | N/A | FPG-19、Review log |
| 2026-11-17 04:01:04 UTC | Codex R7 FAIL | 将 `FPG-03..FPG-08, FPG-10..FPG-31` 全部 `(new)` 验证命令由子串过滤改为精确完整函数路径 `test(=command::format_patch_test::<fn>)` 并统一 `command::` 模块前缀 | N/A | FPG-01/03..31、测试矩阵、Review log |

| 2026-11-17 04:09:11 UTC | Codex R8 FAIL + Claude R8 P2 | ① 将 `FPG-09` 验证命令拆分为独立精确 `test(=command::format_patch_test::...)` 门（`VER=4/8`）消除 `\|` 掩蔽零命中风险；② 同步文头 R9 时间、`FPG-19` `AC=4/8` 计数与 R7 历史描述 | N/A | 文头、粒度表、FPG-09、FPG-19、Review log |
| 2026-11-17 05:00:00 UTC | Codex R9 FAIL | ① 对齐 `AGENTS.md:135`，将全计划网站交付路径统一为 `/Volumes/Sea/libra-backend`（`cf` 分支），并在 `GC-05`/`兼容收口` 补齐前端校验、Claude Code `PASS`、签名 `libra commit`/推送 `cf` 及 Cloudflare 部署核验门（路径不可用时 fail-closed）；② 新增 `DEP-FPG-04` 并在「发布分组与并发窗口」规定每卡开工与发布前均须核验 `plan-status.md` 对 `COMPATIBILITY.md`、版本面（`Cargo.toml`/`install.sh`/`install.ps1`/`Cargo.lock`）、`plan-status.md` 与 `/Volumes/Sea/libra-backend` 的跨计划独占锁（fail-closed） | N/A | 文头、GC-05、依赖表（DEP-FPG-04）、并发窗口、兼容收口、Review log |
| 2026-11-17 05:02:00 UTC | Codex R10 FAIL | ① 修正 `DEP-FPG-04` 锁持有期至 `gh release create` 与 D 组 `release.yml`/CDN 核验全部完成后释放，并明确 `plan-status.md` 中锁获取、归属（卡号/owner）与释放状态机记录方式；② 修正粒度审计表 `FPG-19` `AC` 为 `4/8`（与卡内一致） | N/A | 文头、DEP-FPG-04、粒度表、Review log |
| 2026-11-17 05:05:00 UTC | Codex R11 FAIL | 为 `DEP-FPG-04` 补全跨工作区可见的原子获取与仲裁协议：以远端 `origin/main` 的 `plan-status.md` 为唯一权威账本，由单一协调者/指定发布者串行仲裁（或通过 `libra fetch` + PR/push 到 `origin/main` 原子登记并回读确认唯一持有者）后再写入或发布，释放亦同步推送 `origin/main` 确认 | N/A | 文头、DEP-FPG-04、发布窗口、Review log |
| 2026-11-17 05:07:00 UTC | Codex R12 FAIL | 明确 `DEP-FPG-04` 共同仲裁约束：指定单一授权协调者/发布者（`genedna` 或其显式指派的单一发布 Agent，须在 `origin/main` `plan-status.md` 登记）统一串行仲裁所有参与计划（`format-patch`/`log-show`/`global-init-infra`/`plan-20260927`），所有计划均须遵守基于最新 `origin/main` 检查、竞争后重检与回读确认协议 | N/A | 文头、DEP-FPG-04、发布窗口、Review log |
| 2026-11-17 05:09:00 UTC | Codex R13 FAIL | 将「共同串行仲裁规则已在 `origin/main` `plan-status.md` 合入生效，且各在途参与计划均已采纳遵守同一协议（或已收口关闭）」设为 `DEP-FPG-04` 的强制开工前置条件（未满足前 `FPG-01` 保持 `pending`） | N/A | 文头、DEP-FPG-04、Review log |

## 已决议设计决策

### ADR-FPG-01: 邮件头默认编码、MIME 省略与换行折叠策略

- **Status:** Accepted
- **Context:** #886、#887、#897、#911 涉及 `Subject:` 多行拼接、超长折叠、默认 RFC 2047 编码以及纯 ASCII 补丁省略 MIME 头。
- **Decision:** 默认启用非 ASCII 邮件头的 RFC 2047 Q 编码（`--no-encode-email-headers` 可关闭）；仅当邮件头或正文包含非 ASCII 字符或启用 `--attach`/`--inline` 时才输出 `MIME-Version` / `Content-Type` / `Content-Transfer-Encoding`；超长 `Subject:` 按 RFC 2822 78 字节软限制以 `\n ` 折叠。
- **Alternatives considered:** 维持始终输出 MIME 头且默认不编码（与 Git 行为及 t4014/t4036 不兼容，拒绝）。
- **Consequences:** 需同步更新依赖固定 MIME 头的既有 `format_patch_test` / `compat_format_patch_mail_roundtrip` 断言；`#881` 的 t4036 端到端闭环将在 `FPG-30`（#897）完成后达成。
- **Revisit when:** 若后续引入 RFC 6532 SMTPUTF8 模式再扩展开关。

### ADR-FPG-02: 配置项级联与 CLI 顺序覆盖语义

- **Status:** Accepted
- **Context:** #883、#884、#885、#896、#900、#906、#912、#914、#915、#917 要求补齐十余个 `format.*` / `diff.relative` / `branch.<name>.description` 配置及 `--no-to`/`--to` 等顺序覆盖。
- **Decision:** 所有配置通过 `read_format_config`（`read_cascaded_config_value_strict`）按 local → global → system 严格读取并校验类型；对于可重复与否定成对参数（`--to`/`--no-to`、`--cc`/`--no-cc`、`--notes`/`--no-notes`、`--from`/`--no-from`），按命令行参数出现位置顺序求值，后者覆盖前者及配置列表。
- **Alternatives considered:** 仅用布尔 flag 覆盖（无法支持 `--no-to --to addr`，导致 #914 无法解决，拒绝）。
- **Consequences:** 需在解析入口保留参数位置索引。
- **Revisit when:** 若全局引入统一可否定多值 CLI 解析器可统一迁移。

### ADR-FPG-03: 共享写集 `format_patch.rs` 的串行化与 `529.md` 错峰策略

- **Status:** Accepted
- **Context:** `issues/529.md` 当前持有 `src/command/format_patch.rs` 写锁；本计划 31 张卡均修改 `src/command/format_patch.rs`。
- **Decision:** 本计划首卡 `FPG-01` 依赖 `DEP-FPG-01`（`issues/529#CLOSE-529` 完成）；计划内部采用线性依赖链（G-10）。
- **Alternatives considered:** 与 `529.md` 并发修改同一文件或合并为巨型卡（违反 G-01/G-10，拒绝）。
- **Consequences:** 无并发写冲突，每卡均为独立可回滚发布切片。
- **Revisit when:** `529.md` 合入并关闭后 `DEP-FPG-01` 解除。

## 全局工程约束

继承 [plan-template.md](plan-template.md) 的 `GC-01`..`GC-13`。特别强调：
- **GC-01 现状核实前置:** 每卡开工前必须重新核对 `issues/529.md` 合入后的最新代码，已满足部分只补测试/文档并关闭。
- **GC-05 / ER-06a 文档同步:** 每张改变公开 CLI/配置/输出行为的卡必须同卡更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md`（第 224 行原地修改），并严格按 `AGENTS.md:135` 在授权生产网站仓库 `/Volumes/Sea/libra-backend`（`cf` 分支，保留无关改动、不建新分支/PR/worktree）同卡交付：① 运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra` 同步 `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`（禁止手改生成页）；② 运行网站文档检查与前端验证（`gen:docs` / `typecheck` / `build`）、取得 Claude Code `PASS`、用 `libra commit` 签名提交并推送 `cf`、核验 Cloudflare 部署完成；若 `/Volumes/Sea/libra-backend` 不可用，fail-closed 停止网站交付，除非用户明确授权替代检出。

## 执行检查必备需求（强制）

继承 [plan-template.md](plan-template.md) 的 `ER-01`..`ER-14`：
- **ER-04 / ER-08 / ER-13 / ER-14:** 本计划每张 `implementation` 卡均为 `independent` 发布切片，完成后按 ER-08 执行 `patch + 1`、同步 `compat_version_surface_sync` 权威三处（`Cargo.toml`、`install.sh`、`install.ps1`）、工具链刷新 `Cargo.lock`；因每张卡均修改 `install.sh`/`install.ps1`（`T-1`），C 组第 ④ 步除运行本卡 A 组 focused `nextest` 外，必须跑双环境全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`、`fmt`、`clippy`、`cargo build --release`，随后签名提交推送 PR，squash merge 后 `gh release create v<version>`。
- **ER-04 D 组远端后置门（每张 `independent` 卡默认适用）:**
  - `base.yml`（PR / `push` `main`）：`format`、`clippy`、`redundancy`、`test` 全部 green。
  - `codeql.yml`（PR / `push` `main`）：CodeQL 扫描 green。
  - `release.yml`（`push` `v*` tag，由 `gh release create "v${VERSION}"` 触发）：`build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest` 全部 green；并通过 `gh run list --workflow=release.yml` 及 `download.libra.tools` / stable manifest（`HEAD` 200、range `GET` 206）核验发布产物与 CDN 可获取性。

## 实施顺序

- `DEP-FPG-01 -> FPG-01`
- Phase 0: `FPG-01 -> FPG-02 -> FPG-03 -> FPG-04 -> FPG-05 -> FPG-06 -> FPG-07 -> FPG-29 -> FPG-08 -> FPG-30 -> FPG-09 -> FPG-10 -> FPG-31`
- Phase 1: `FPG-31 -> FPG-11 -> FPG-12 -> FPG-13 -> FPG-14 -> FPG-15 -> FPG-16 -> FPG-17 -> FPG-24 -> FPG-25`（其中 `DEP-FPG-03 -> FPG-11`）
- Phase 2: `FPG-25 -> FPG-18 -> FPG-19 -> FPG-20 -> FPG-26 -> FPG-21 -> FPG-22 -> FPG-27 -> FPG-23 -> FPG-28`（其中 `DEP-FPG-02 -> FPG-22`、`DEP-FPG-02 -> FPG-27`、`DEP-FPG-03 -> FPG-22`、`DEP-FPG-03 -> FPG-27`）

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-FPG-01 | incoming | 跨计划写锁 | `issues/529#FP-13`（及 `CLOSE-529` 收口） | `#529` 执行者 | `issues/529.md` 全部实现卡达到 `done`/`complete` 并释放 `src/command/format_patch.rs` 与 `src/cli.rs` 写锁 | `docs/development/plan/issues/529.md`（2026-11-17 核对） | 未释放前 `FPG-01` 保持 `pending`，不得抢跑写 `format_patch.rs` |
| DEP-FPG-02 | incoming | 跨计划写锁 | `plan-20260927#CM-08` / `CM-09` | `plan-20260927` 执行者 | 开工 `FPG-22` 与 `FPG-27` 前核实 `src/command/diff/` 无在途写者（2026-11-15 `plan-status.md` 记为 `pending`/空） | `docs/development/plan/plan-status.md:1544-1545`、`issues/529.md:106` | 若有在途修改则等待其释放后再动 `src/command/diff/` |
| DEP-FPG-03 | incoming | 跨计划写锁 | `issues/1040.md` 及同批 `log-show` 计划 | 协调者 / 各计划执行者 | 开工 `FPG-11`/`FPG-22`/`FPG-27` 前核实 `src/command/log.rs` 无在途写者并串行协调 | `docs/development/plan/issues/1040.md` | 存在并发写者时等待其释放 `src/command/log.rs` 后 rebase 再开工 |
| DEP-FPG-04 | incoming | 跨计划共享交付与发布窗口锁 | `plan-status.md` 登记的其它在途计划（含 `log-show`、`global-init-infra`、`plan-20260927` 等） | 仓库维护者 `genedna` 或其显式指派的单一发布 Agent（须在 `origin/main` `plan-status.md` 登记授权） | **共同串行仲裁前置与执行协议：0) 开工前置条件：** 本计划 `FPG-01` 启动前，须核实上述共同串行仲裁规则与单一授权发布者已在 `origin/main` `plan-status.md` 合入生效，且全部在途竞争计划（`log-show`、`global-init-infra`、`plan-20260927` 等）均已在其计划及 `plan-status.md` 中采纳同一协议（或已收口关闭）；未满足前本计划保持 `pending` 不得开工。**执行协议：** ① 登记前 `libra fetch` 基于最新 `origin/main` 核实 `plan-status.md` 无其它 `in-progress`/`blocked` 持有者；② 提交 PR/合入 `origin/main` 登记本卡 `in-progress` 与唯一锁归属（卡号 + 授权发布者），若合入发生竞争冲突则放弃并基于最新 `origin/main` 重新检查；③ 合入后再次 `libra fetch` 回读 `origin/main`（及 `gh release list --limit 5`）确认本卡为唯一可见持有者后方可写入共享面或发布；④ **持续持有至 `gh release create` 与 D 组 `release.yml`/CDN 核验全部完成**，更新 `origin/main` `plan-status.md` 为 `done`/`complete`（含版本与 SHA）并确认合入后方释放 | `docs/development/plan/plan-status.md`（`origin/main`） | fail-closed：前置条件未满足、未获单一发布者授权、或回读发现并发写者/竞争登记/归属不清，立即中止写入与发布，等待 `origin/main` 满足/释放并 rebase 后重试 |

### 发布分组与并发窗口

- **并发声明:** 全串行（所有卡共享 `src/command/format_patch.rs`、`COMPATIBILITY.md`、版本面、`plan-status.md` 与 `/Volumes/Sea/libra-backend` 写集，依据 G-10 禁止计划内与跨计划并发）。
- **发布者:** 单一指定发布者（协调者指派的执行 Agent）。
- **发布窗口顺序:** 待 `DEP-FPG-01` 与 `DEP-FPG-04` 满足后，按实施顺序依次执行 C 组 bump 与 `gh release create`，每卡开工与发布前均须重新核验 `DEP-FPG-04`（及适用时的 `DEP-FPG-02`/`03`），前一卡未完成推送与 D 组触发前不得开启下一卡 bump。

### Phase 0: 序号、身份、上游去重与邮件头基础（FPG-01..FPG-10, FPG-29..FPG-31）

**目标:** 完成多补丁默认编号与 `format.numbered`、环境变量身份核验、`--ignore-if-in-upstream`、邮件头配置与顺序、`--no-notes`/`format.notes`、`format.signature`、多行 Subject 拼接、长行折叠、默认 RFC2047 编码、纯 ASCII 省略 MIME（及 `#881` t4036 闭环）、`-k` 核验与 `--from`/`format.from`。

**进入条件:** `DEP-FPG-01` 满足（`529.md` 释放写锁），当前工作区干净。

**退出条件:** `FPG-01`..`FPG-10`、`FPG-29`..`FPG-31` 全部 `Lifecycle=done`、`Acceptance=complete`。

### Phase 1: 路径过滤、封面信与修订范围（FPG-11..FPG-17, FPG-24..FPG-25）

**目标:** 完成 `--relative`/`diff.relative`、封面信自动触发/diffstat/`--commit-list-format` 扩展、多修订范围与 `^<rev>`、固定 mbox 时间戳、`--base=auto`、`branch.<name>.description` 与 `--cover-from-description`。

**进入条件:** Phase 0（`FPG-31`）完成。

**退出条件:** `FPG-11`..`FPG-17`、`FPG-24`、`FPG-25` 全部 `Lifecycle=done`、`Acceptance=complete`。

### Phase 2: 附件、Interdiff、文件名、Diff 选项与线程化（FPG-18..FPG-23, FPG-26..FPG-28）

**目标:** 完成 `--attach=<name>`、`--interdiff`、reroll 前缀与 stdout 列表、非整数 `-v`、文件名大小写与长度上限、Git diff 选项透传、`--thread` 默认/配置与 Message-ID 去重，完成全量收口门。

**进入条件:** Phase 1（`FPG-25`）完成。

**退出条件:** 全部 31 张卡 `Lifecycle=done`、`Acceptance=complete`，计划完成判据全量门全绿。

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
| FPG-01 | implementation | 多补丁默认编号与 `format.numbered` | immutable-release | yes | yes | 6/8 | 3/8 | 2/1 | M | DEP-FPG-01 | 序列化于 DEP-FPG-01 | independent | N/A | N/A |
| FPG-02 | implementation | `GIT_COMMITTER_*` 身份核验 | immutable-release | yes | yes | 5/8 | 2/8 | 2/1 | S | FPG-01 | 序列化于 FPG-01 | independent | N/A | N/A |
| FPG-03 | implementation | `--ignore-if-in-upstream` tag 场景修复 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | S | FPG-02 | 序列化于 FPG-02 | independent | N/A | N/A |
| FPG-04 | implementation | 邮件头配置与 `--to`/`--no-to` 顺序 | immutable-release | yes | yes | 6/8 | 2/8 | 2/1 | M | FPG-03 | 序列化于 FPG-03 | independent | N/A | N/A |
| FPG-05 | implementation | `--no-notes` 与 `format.notes` | immutable-release | yes | yes | 3/8 | 2/8 | 2/1 | M | FPG-04 | 序列化于 FPG-04 | independent | N/A | N/A |
| FPG-06 | implementation | signature 前缀与 `format.signature` | immutable-release | yes | yes | 3/8 | 2/8 | 2/1 | M | FPG-05 | 序列化于 FPG-05 | independent | N/A | N/A |
| FPG-07 | implementation | 多行 Subject 首段拼接 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-06 | 序列化于 FPG-06 | independent | N/A | N/A |
| FPG-29 | implementation | 长 `Subject:` 邮件头折叠 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-07 | 序列化于 FPG-07 | independent | FPG-07 | N/A |
| FPG-08 | implementation | 默认 RFC 2047 邮件头编码 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-29 | 序列化于 FPG-29 | independent | N/A | N/A |
| FPG-30 | implementation | 纯 ASCII 省略 MIME 头与 `#881` 闭环 | immutable-release | yes | yes | 3/8 | 2/8 | 2/1 | M | FPG-08 | 序列化于 FPG-08 | independent | FPG-08 | N/A |
| FPG-09 | implementation | `#888` `-k` 互斥核验与关闭 | immutable-release | yes | yes | 2/8 | 4/8 | 2/1 | S | FPG-30 | 序列化于 FPG-30 | independent | N/A | N/A |
| FPG-10 | implementation | `--from=<ident>` 合法性校验 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-09 | 序列化于 FPG-09 | independent | N/A | N/A |
| FPG-31 | implementation | `--no-from` 与 `format.from` 配置 | immutable-release | yes | yes | 3/8 | 2/8 | 2/1 | M | FPG-10 | 序列化于 FPG-10 | independent | FPG-10 | N/A |
| FPG-11 | implementation | `--relative` 与 `diff.relative` | immutable-release | yes | yes | 2/8 | 2/8 | 3/2 | M | FPG-31,DEP-FPG-03 | 序列化于 FPG-31 | independent | N/A | N/A |
| FPG-12 | implementation | `format.coverLetter` 与 `#909` 核验 | immutable-release | yes | yes | 3/8 | 2/8 | 2/1 | M | FPG-11 | 序列化于 FPG-11 | independent | N/A | N/A |
| FPG-13 | implementation | 封面信 diffstat 汇总 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-12 | 序列化于 FPG-12 | independent | N/A | N/A |
| FPG-14 | implementation | `--commit-list-format` 占位符扩展 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-13 | 序列化于 FPG-13 | independent | N/A | N/A |
| FPG-15 | implementation | 多修订参数与 `^<rev>` 排除语法 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-14 | 序列化于 FPG-14 | independent | N/A | N/A |
| FPG-16 | implementation | mbox `From ` 固定 2001-09-17 时间戳 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-15 | 序列化于 FPG-15 | independent | N/A | N/A |
| FPG-17 | implementation | `--base=auto` 与 `format.useAutoBase` | immutable-release | yes | yes | 4/8 | 2/8 | 2/1 | M | FPG-16 | 序列化于 FPG-16 | independent | N/A | N/A |
| FPG-24 | implementation | 封面信 `branch.<name>.description` 与默认 `From:` | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-17 | 序列化于 FPG-17 | independent | FPG-12 | N/A |
| FPG-25 | implementation | `--cover-from-description` 与模式输出 | immutable-release | yes | yes | 6/8 | 2/8 | 2/1 | M | FPG-24 | 序列化于 FPG-24 | independent | FPG-13 | N/A |
| FPG-18 | implementation | `--attach=<name>` 与 `format.attach` | immutable-release | yes | yes | 3/8 | 2/8 | 2/1 | M | FPG-25 | 序列化于 FPG-25 | independent | N/A | N/A |
| FPG-19 | implementation | `--interdiff=<rev>` 输出 | immutable-release | yes | yes | 4/8 | 3/8 | 2/1 | M | FPG-18 | 序列化于 FPG-18 | independent | N/A | N/A |
| FPG-20 | implementation | reroll `vN-` 前缀与 stdout 文件列表 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-19 | 序列化于 FPG-19 | independent | N/A | N/A |
| FPG-26 | implementation | 非整数/小数 `--reroll-count` 支持 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-20 | 序列化于 FPG-20 | independent | FPG-20 | N/A |
| FPG-21 | implementation | 文件名保留大小写与 `--filename-max-length` | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-26 | 序列化于 FPG-26 | independent | N/A | N/A |
| FPG-22 | implementation | `-U`/`--unified` 与 `--full-diff` | immutable-release | yes | yes | 3/8 | 2/8 | 4/3 | M | FPG-21,DEP-FPG-02,DEP-FPG-03 | 序列化于 FPG-21 | independent | N/A | N/A |
| FPG-27 | implementation | `--diff-filter` 与 `-M`/`--no-renames` 核验 | immutable-release | yes | yes | 2/8 | 2/8 | 4/3 | M | FPG-22,DEP-FPG-02,DEP-FPG-03 | 序列化于 FPG-22 | independent | FPG-22 | N/A |
| FPG-23 | implementation | `--thread` 默认、`format.thread` 与 `deep` | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | M | FPG-27 | 序列化于 FPG-27 | independent | N/A | N/A |
| FPG-28 | implementation | 非子串唯一 `Message-ID` 生成 | immutable-release | yes | yes | 2/8 | 2/8 | 2/1 | S | FPG-23 | 序列化于 FPG-23 | independent | FPG-23 | N/A |

---

### Task FPG-01: 核实 `#880` 计数器宽度并实现多补丁默认编号与 `format.numbered`（#880, #896）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 GC-01 核实 `529 FP-02` 已修复的 `#880` Subject 计数器宽度（`1/1`、`09/10`），并实现 `#896`：多补丁系列（`total > 1`）默认启用 `[PATCH n/m]` 编号及支持 `format.numbered`（`true`/`false`/`auto`）配置。

**Out of scope:** 文件名 `0001-` 序列号宽度不变。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `subject_counter_width` 已按 `displayed_total` 宽度计算（`529 FP-02` 交付） | `src/command/format_patch.rs:857,1402,1762-1764` |
| 仅当 `args.numbered \|\| args.cover_letter` 时才编号，未读 `format.numbered` | `src/command/format_patch.rs:855,1861-1902` |

**Acceptance criteria:**

- [ ] 核实 `#880` 单补丁 `-n` 输出 `[PATCH 1/1]`。
- [ ] 核实 `#880` 两补丁 `--start-number=9` 输出 `09/10`、`10/10`。
- [ ] 多补丁系列（`total > 1`）默认输出 `[PATCH n/m]`。
- [ ] 单补丁默认输出 `[PATCH]`。
- [ ] 读取 `format.numbered`（`true`/`false`/`auto`），非法值返回 `FormatPatchError::InvalidConfig`。
- [ ] CLI `-n`/`-N` 覆盖 `format.numbered` 配置。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新 `Docs and compatibility impact` 所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::format_numbered_config)'`（`(new)`，零命中即失败）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(command::format_patch_test::) & test(numbered)'`
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited（T-1：版本面集合同步修改 install.sh / install.ps1）`

**Dependencies:** `DEP-FPG-01`（等待 `issues/529#CLOSE-529` 释放写锁）

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件同步 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`（原地修改），并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra` 同步网站页。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=多补丁默认编号与format.numbered; recovery=immutable-release（发布前revert单提交，发布后FIX前滚）; complete=yes; self-contained=yes; AC=6/8; VER=3/8; landing=2; prod-files=1; scope=M; deps=DEP-FPG-01; writeset=序列化于DEP-FPG-01; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-02: 核实 `529 FP-11` 的 `GIT_COMMITTER_*` 身份解析切片（#881 身份切片）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 GC-01 核实 `529 FP-11`（ADR-FP-07）合入后的 `resolve_signoff_identity` 是否严格遵循 Git 规则（`GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` 优先于 `user.name`/`user.email`，邮箱再回退 `EMAIL`；空邮箱成功输出 `<>`；全部 3 类 ADR-FP-07 失败均返回 `LBR-CLI-003`），补齐边缘测试并完成身份切片验收。本卡不关闭 `#881`（完整 `#881` t4036 闭环在 `FPG-30` 完成 `#897` MIME 头省略后达成）。

**Out of scope:** `#881` 的 t4036 MIME 头部分（由 `FPG-30` 闭环）；不改 `libra commit` 身份逻辑。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/529.md` `FP-11` 按 ADR-FP-07 负责 `GIT_COMMITTER_*` 身份解析并记录 `#881` 另依赖 `#897`；`CliInvalidTarget` 映射 `LBR-CLI-003` | `src/command/format_patch.rs:2050-2065`、`src/utils/error.rs:364`、`issues/529.md:166,179,324` |

**Acceptance criteria:**

- [ ] `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL` 优先于 `user.name`/`user.email` 生效，邮箱未配置且未设 `GIT_COMMITTER_EMAIL` 时回退 `EMAIL`（非 `user.useConfigOnly`）。
- [ ] 已设置 `GIT_COMMITTER_EMAIL=""` 时作为有效空邮箱输出 `<>`。
- [ ] 空姓名（如 `GIT_COMMITTER_NAME=""`）返回 `empty ident name (for <...>) not allowed`（`LBR-CLI-003`）。
- [ ] `user.useConfigOnly=true` 且缺邮箱时返回 `no email was given and auto-detection is disabled`（`LBR-CLI-003`）。
- [ ] 身份完全缺失时返回 `identity unknown`（`LBR-CLI-003`）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件核对并同步所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::signoff_git_committer_env_precedence)'`（`(new)`，覆盖上述优先级、空邮箱 `<>` 与全部 3 类 `LBR-CLI-003` 失败消息）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-01`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件核对并同步 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=GIT_COMMITTER_*身份核验; recovery=immutable-release; complete=yes; self-contained=yes; AC=5/8; VER=2/8; landing=2; prod-files=1; scope=S; deps=FPG-01; writeset=序列化于FPG-01; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-03: 复现并修复 `--ignore-if-in-upstream` 在 t4014#4 带 tag 场景多输出补丁（#882）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 复现 Git t4014#4（含 tag 引用）下 `libra format-patch --ignore-if-in-upstream` 多输出一个补丁的问题，定位 `resolve_range_commits` / `git_patch_id_for_commit` 中的根因并修复，使输出补丁集与 `git@6de20f6092` 一致。

**Out of scope:** 不修改二进制文件的 Git patch-id 兼容边界。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_range_commits` 通过 `get_reachable_commits` 与 `git_patch_id_for_commit` 计算上游 patch-id 过滤 | `src/command/format_patch.rs:688-750,1126-1213` |
| `get_reachable_commits` 包含起始提交自身，故根因需通过 t4014#4 复现定位 | `src/command/log.rs:1080` |

**Acceptance criteria:**

- [ ] 新增复现 t4014#4 带 tag 场景的回归测试，修复后输出补丁数量与提交列表与 `git@6de20f6092` 一致。
- [ ] 不破坏既有 `ignore_if_in_upstream` 用例与 `compat_format_patch_mail_roundtrip`。
- [ ] ER-06/ER-06a 同卡强制门：逐文件核对并同步所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::ignore_if_in_upstream_tag_t4014_4)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-02`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件核对并同步 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--ignore-if-in-upstream_tag场景修复; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=S; deps=FPG-02; writeset=序列化于FPG-02; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-04: 支持 `format.headers`/`format.to`/`format.cc` 与 `--no-to`/`--to` 顺序语义（#883, #914）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 读取 `format.headers`、`format.to`、`format.cc` 多值配置，并按 CLI 参数实际出现顺序处理 `--to`/`--no-to` 与 `--cc`/`--no-cc`（`--no-to` 清空先前的配置与 CLI 值，其后的 `--to` 生效）。

**Out of scope:** 不支持 `sendemail.*` 配置。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `resolve_recipients` 虽使用 `args.to`/`args.cc`，但 `no_to`/`no_cc` 为简单布尔标志，会抹除在其之后出现的 `--to`/`--cc` | `src/command/format_patch.rs:194-210,1918-1930` |
| `resolve_format_config` 未读取 `format.headers`/`format.to`/`format.cc` | `src/command/format_patch.rs:1861-1902` |

**Acceptance criteria:**

- [ ] 支持 `format.headers` 多值配置输出自定义邮件头。
- [ ] 支持 `format.to` 多值配置输出 `To:` 邮件头。
- [ ] 支持 `format.cc` 多值配置输出 `Cc:` 邮件头。
- [ ] `--no-to --to addr` 清空先前 `To:` 并保留 `addr`。
- [ ] `--no-cc --cc addr` 清空先前 `Cc:` 并保留 `addr`。
- [ ] `format.headers` 与 `To:`/`Cc:` 均经 `sanitize_header_value` 过滤控制字符，防止头注入。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::format_headers_and_no_to_ordering)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-03`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=邮件头配置与--to/--no-to顺序; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-03; writeset=序列化于FPG-03; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-05: 支持 `--no-notes` 与 `format.notes` 配置（#884）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 新增 `--no-notes` CLI 参数，并在 `resolve_format_config` 读取 `format.notes`（boolean 或 ref 名），使 CLI `--notes`/`--no-notes` 按最后出现优先级覆盖配置。

**Out of scope:** 不改变 `libra notes` 子命令行为。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FormatPatchArgs` 仅有 `notes`，无 `no_notes`；`resolve_format_config` 未读 `format.notes` | `src/command/format_patch.rs:236-240,1861-1902` |

**Acceptance criteria:**

- [ ] `libra format-patch --no-notes` 被接受并抑制所有 notes 输出。
- [ ] `format.notes`（`true`/`false`/`<ref>`）生效。
- [ ] CLI `--no-notes`/`--notes` 按顺序覆盖 `format.notes` 配置。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::format_notes_and_no_notes)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-04`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--no-notes与format.notes; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-04; writeset=序列化于FPG-04; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-06: 修复默认 signature 产品前缀并支持 `format.signature`（#885）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 默认 signature 输出 `libra <version>`（包含产品前缀），并在 `resolve_format_config` 中支持 `format.signature`（空字符串视为禁用，CLI `--signature`/`--signature-file`/`--no-signature` 覆盖配置）。

**Out of scope:** 不修改 `am` 对 signature 的剥离规则。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `push_signature` 默认仅输出 `env!("CARGO_PKG_VERSION")`，且未读 `format.signature` | `src/command/format_patch.rs:1062-1071,1861-1902` |

**Acceptance criteria:**

- [ ] 默认签名页脚为 `-- \nlibra <CARGO_PKG_VERSION>\n`。
- [ ] `format.signature` 非空值覆盖默认签名，空值等效 `--no-signature`。
- [ ] CLI `--signature`/`--signature-file`/`--no-signature` 优先级高于 `format.signature`。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::format_signature_config_and_prefix)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-05`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=signature前缀与format.signature; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-05; writeset=序列化于FPG-05; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-07: 多行 Subject 首段拼接（#911；自本卡拆出 FPG-29）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 Git 规则将提交消息首段（首个空行之前的所有非空行）以空格拼接为完整 Subject，正文从首个空行之后开始（自本卡拆出 `FPG-29` 处理长 `Subject:` 邮件头折叠）。

**Out of scope:** 长 `Subject:` 邮件头折叠（由 `FPG-29` 承接）；非 ASCII RFC 2047 编码（由 `FPG-08` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_patch_body` 与 `commit_subject_line` 仅取 `lines().next()` | `src/command/format_patch.rs:833,1497-1500` |

**Acceptance criteria:**

- [ ] 多行提交首段（无空行分隔）被完整拼接为单句 Subject，正文从首个空行之后开始。
- [ ] `libra am` / `git am` 能正确还原拼接后的 Subject 与正文。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::multiline_subject_join)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-06`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=多行Subject首段拼接; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-06; writeset=序列化于FPG-06; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-29: 超长 `Subject:` 邮件头折叠（#886）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-07` 拆出的独立轴：对超过 78 字节的 `Subject:` 头行按 RFC 2822 在空格边界折行（续行前导单空格）。

**Out of scope:** 多行提交首段拼接已在 `FPG-07` 完成；非 ASCII RFC 2047 编码由 `FPG-08` 处理。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `Subject:` 直接单行输出，无折行逻辑 | `src/command/format_patch.rs:863-869` |

**Acceptance criteria:**

- [ ] 超长 `Subject:` 行在合适边界折行并添加前导单空格缩进。
- [ ] `libra am` / `git am` 能无损展开折叠后的 `Subject:`。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::subject_header_folding)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-07`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=长Subject邮件头折叠; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-07; writeset=序列化于FPG-07; release=independent; split-from=FPG-07; exception=N/A`

---

### Task FPG-08: 默认启用非 ASCII 邮件头 RFC 2047 编码（#887；自本卡拆出 FPG-30）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 `ADR-FPG-01` 将 `--encode-email-headers` 默认行为设为开启，使非 ASCII `Subject:`/`From:` 默认进行 RFC 2047 Q 编码，传入 `--no-encode-email-headers` 时保留原始 UTF-8（自本卡拆出 `FPG-30` 处理纯 ASCII 省略 MIME 头）。

**Out of scope:** 纯 ASCII 省略 MIME 头与 `#881` t4036 闭环（由 `FPG-30` 承接）；不修改 `mailinfo` 解码逻辑。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `encode_email_headers` 默认 `false` | `src/command/format_patch.rs:347-358,825,835` |

**Acceptance criteria:**

- [ ] 默认情况下非 ASCII `Subject:`/`From:` 自动进行 RFC 2047 Q 编码。
- [ ] 传入 `--no-encode-email-headers` 时保留原始 UTF-8 邮件头。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::encode_email_headers_default_on)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-29`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=默认RFC2047邮件头编码; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-29; writeset=序列化于FPG-29; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-30: 纯 ASCII 补丁省略 MIME 头并闭环 `#881` t4036（#897, #881 完整闭环）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-08` 拆出的独立轴：仅在邮件头或正文含非 ASCII 字节或启用 `--attach`/`--inline` 时才输出 `MIME-Version`、`Content-Type`、`Content-Transfer-Encoding` 头；纯 ASCII 补丁省略 MIME 头，并随 `FPG-02` 身份修复共同完成 `#881`（t4036）端到端闭环。

**Out of scope:** 非 ASCII RFC 2047 编码已在 `FPG-08` 完成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_patch_body` 与 `format_cover_letter` 无条件输出 MIME 头 | `src/command/format_patch.rs:897-905,1415-1417` |

**Acceptance criteria:**

- [ ] 纯 ASCII 补丁（无 `--attach`/`--inline`）不输出 `MIME-Version`、`Content-Type`、`Content-Transfer-Encoding` 头。
- [ ] 含非 ASCII 正文或启用 `--attach`/`--inline` 时仍正确输出 MIME 头。
- [ ] t4036（`--signoff` + `GIT_COMMITTER_*` + 纯 ASCII 无 MIME 头）端到端测试通过，闭环 `#881` 与 `#897`。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::pure_ascii_omits_mime_headers)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-08`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=纯ASCII省略MIME头与#881闭环; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-08; writeset=序列化于FPG-08; release=independent; split-from=FPG-08; exception=N/A`

---

### Task FPG-09: 核实 `529 FP-03` 的 `-k` 与 `--subject-prefix`/`--rfc` 互斥并闭环 `#888`（#888）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 按 GC-01 在 `529 FP-03` 合入后核对 `-k` 与 `--subject-prefix`/`--rfc` 冲突矩阵（含 `fp03_empty_rfc_with_k`），确认与 `git@6de20f6092` 一致并补全任何缺失边界用例与文档说明。

**Out of scope:** `format.subjectPrefix` 配置在 `-k` 下仍允许被忽略；不推翻 `fp03_empty_rfc_with_k`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `validate_cli_args` 已由 `529 FP-03` 处理 `-k` 互斥，已有 `fp03_empty_rfc_with_k` | `src/command/format_patch.rs:437-458`、`tests/command/format_patch_test.rs:2589` |

**Acceptance criteria:**

- [ ] `libra format-patch -k --subject-prefix=FOO` 与 `-k --rfc` 均返回 `LBR-CLI-002` fatal 错误。
- [ ] `libra format-patch -k --rfc=` 保持与 `git@6de20f6092` 一致。
- [ ] ER-06/ER-06a 同卡强制门：核对并同步所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::fp03_empty_rfc_with_k)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::keep_subject_retains_bracket_prefix)'`
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::keep_subject_short_flag_is_accepted)'`
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-30`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件核对并同步 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=#888_-k互斥核验与关闭; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=4/8; landing=2; prod-files=1; scope=S; deps=FPG-30; writeset=序列化于FPG-30; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-10: `--from=<ident>` 合法性校验（#889；自本卡拆出 FPG-31）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 校验 `--from=<ident>` 必须为合法 `Name <email>` 格式，拒绝无邮箱字符串（如 `--from=foo`）产生 `From: foo <>`（自本卡拆出 `FPG-31` 处理 `--no-from` 与 `format.from`）。

**Out of scope:** `--no-from` 与 `format.from` 配置（由 `FPG-31` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `parse_from_ident` 对无 `<>` 字符串返回 `(ident, "")` | `src/command/format_patch.rs:2022-2032` |

**Acceptance criteria:**

- [ ] `--from=foo`（缺 `<email>` 或邮箱为空）报 `LBR-CLI-002` 错误而非输出 `From: foo <>`。
- [ ] 合法 `--from="Alice <alice@example.com>"` 正常输出 `From:` 头与必要的 in-body `From:`。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::from_invalid_ident_rejected)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-09`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--from=<ident>合法性校验; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-09; writeset=序列化于FPG-09; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-31: 新增 `--no-from` 与 `format.from` 配置（#915）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-10` 拆出的独立轴：新增 `--no-from` CLI flag 并读取 `format.from`（`true`/`false`/`Name <email>`），使 `--from` 与 `--no-from` 按最后出现标志覆盖配置。

**Out of scope:** `--from=<ident>` 格式校验已在 `FPG-10` 完成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FormatPatchArgs` 缺少 `--no-from`；`resolve_format_config` 未读取 `format.from` | `src/command/format_patch.rs:212-217,1861-1902` |

**Acceptance criteria:**

- [ ] 支持 `format.from`（`true`/`false`/`Name <email>`），非法 ident 或布尔值报错。
- [ ] `--no-from` 覆盖 `format.from` 并使用提交原作者。
- [ ] `--from` 与 `--no-from` 按最后出现标志生效。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::format_from_and_no_from)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-10`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--no-from与format.from配置; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-10; writeset=序列化于FPG-10; release=independent; split-from=FPG-10; exception=N/A`

---

### Task FPG-11: 实现 `--relative` 与 `diff.relative` 路径过滤与相对化（#890）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 取消 `--relative` 的 `Unsupported` 拒绝，支持 `--relative[=<path>]`、`--no-relative` 与 `diff.relative` 配置，按当前工作目录或指定目录过滤变更并输出相对路径的 diffstat 与 diff。

**Out of scope:** 不改变 `libra diff` 命令自身的 `--relative`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `execute_safe` 遇到 `args.relative.is_some()` 返回 `Unsupported`；`CommitDiffOptions` 无 relative 字段 | `src/command/format_patch.rs:500-506,1852-1859` |

**Acceptance criteria:**

- [ ] 在子目录运行或设置 `diff.relative=true` / `--relative=<dir>` 时，仅包含该目录内的文件且路径相对于该目录。
- [ ] 仓库外路径遍历或不存在的目录给出清晰错误（GC-07）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::diff_relative_filters_paths)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-31`, `DEP-FPG-03`（核实 `src/command/log.rs` 无在途写者）

**Implementation write set:** `src/command/format_patch.rs`, `src/command/log.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `src/command/log.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--relative与diff.relative; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=3; prod-files=2; scope=M; deps=FPG-31,DEP-FPG-03; writeset=序列化于FPG-31; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-12: 支持 `format.coverLetter` 自动触发与 `#909` 核验（#900, #909；自本卡拆出 FPG-24）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 读取 `format.coverLetter`（`true`/`false`/`auto`，其中 `auto` 在 `total > 1` 时生成封面信），CLI `--cover-letter`/`--no-cover-letter` 覆盖配置；并按 GC-01 核实 `529 FP-13` 合入的 `--commit-list-format` 隐含启用封面信行为（自本卡拆出 `FPG-24` 处理 `branch.<name>.description` 与默认 `From:`）。

**Out of scope:** `branch.<name>.description` 与默认 `From:`（由 `FPG-24` 承接）；封面信 diffstat（由 `FPG-13` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `execute_safe` 未读 `format.coverLetter`；`--commit-list-format` 隐含封面由 `529 FP-13` 承接 | `src/command/format_patch.rs:178-180,549-564,1861-1902`、`issues/529.md:168` |

**Acceptance criteria:**

- [ ] `format.coverLetter=true` 或 `auto`（当 `total > 1`）自动生成封面信。
- [ ] `--no-cover-letter` 显式关闭 `format.coverLetter` 自动封面信。
- [ ] 核实指定 `--commit-list-format` 时自动启用封面信（除非显式 `--no-cover-letter`）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::format_cover_letter_auto_config)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-11`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=format.coverLetter与#909核验; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-11; writeset=序列化于FPG-11; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-13: 封面信输出系列 diffstat 汇总（#905；自本卡拆出 FPG-25）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在封面信 commit list 之后输出整套系列的汇总 diffstat（首个提交父树到末个提交树的统计），`--no-stat`/`-p` 时省略（自本卡拆出 `FPG-25` 处理 `--cover-from-description`）。

**Out of scope:** `--cover-from-description`（由 `FPG-25` 承接）；`--commit-list-format` 占位符扩展（由 `FPG-14` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_cover_letter` 未计算系列 diffstat | `src/command/format_patch.rs:1428-1444` |

**Acceptance criteria:**

- [ ] 封面信在 commit list 后包含系列整体 diffstat（受 `--stat`/`--no-stat`/`-p` 控制）。
- [ ] 系列首提交为 root commit 时与空树比较正确生成 diffstat。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::cover_letter_includes_diffstat)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-12`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=封面信diffstat汇总; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-12; writeset=序列化于FPG-12; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-14: 扩展 `--commit-list-format` Git pretty-format 占位符支持（#899）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 扩展 `render_pretty_format_commit` 支持 Git `--commit-list-format` 常用占位符（`format:`/`tformat:` 前缀、`%cn`/`%ce`/`%ad`/`%cd`/`%as`/`%cs`/`%at`/`%ct`/`%b`/`%B`/`%f`/`%t`/`%p` 等），避免合法 Git 格式串报 `not a valid format string`。

**Out of scope:** `shortlog` 别名按 GC-01 核实 `529 FP-13`；不改变 `libra log --format`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `render_pretty_format_commit` 仅支持 `%h`/`%H`/`%s`/`%an`/`%ae`/`%n`/`%%` | `src/command/format_patch.rs:1621-1644` |

**Acceptance criteria:**

- [ ] 支持 `format:` / `tformat:` 前缀及 Git 常用作者/提交者日期、邮箱、树/父 OID、`%b`/`%B`/`%f` 等占位符。
- [ ] 对未知占位符仍返回明确格式错误。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::commit_list_format_extended_placeholders)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-13`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--commit-list-format占位符扩展; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-13; writeset=序列化于FPG-13; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-15: 支持多修订参数与 `^<rev>` 排除语法（#892）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 将 `FormatPatchArgs::revision_range` 扩展为支持多个位置修订参数（`Vec<String>`），并解析 `^<rev>` 排除语义（如 `topic ^main` 等价于 `main..topic`）。

**Out of scope:** `--all` 仍保持既有 `Unsupported` 说明。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `revision_range: Option<String>` 仅接收 1 个位置参数 | `src/command/format_patch.rs:108-109,663-685` |

**Acceptance criteria:**

- [ ] 支持 `libra format-patch B ^A`、`libra format-patch A..B ^C` 等多修订参数组合。
- [ ] 单修订 `<rev>` 与 `A..B` 既有语义保持完全向后兼容。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::revision_range_caret_exclusion)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-14`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=多修订参数与^rev排除语法; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-14; writeset=序列化于FPG-14; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-16: mbox `"From "` 分隔符使用 Git 固定 `Mon Sep 17 00:00:00 2001` 时间戳（#893）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 将普通补丁与封面信首行 mbox `"From <oid> ..."` 分隔符的时间戳统一为 Git 固定的 `Mon Sep 17 00:00:00 2001`（邮件 `Date:` 头仍使用提交/当前真实时间）。

**Out of scope:** 不修改 `Date:` RFC 2822 头。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_patch_body` 与 `format_cover_letter` 在 `"From "` 行写入了 `ts`/`now` | `src/command/format_patch.rs:803-807,1373-1376` |

**Acceptance criteria:**

- [ ] 每个补丁与封面信首行均为 `From <oid> Mon Sep 17 00:00:00 2001`。
- [ ] `libra am` 与 `git am` 仍能正确解析该 mbox 分隔符与 `Date:` 头。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::mbox_envelope_fixed_2001_timestamp)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-15`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=mboxFrom固定2001-09-17时间戳; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-15; writeset=序列化于FPG-15; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-17: 支持 `--base=auto` 与 `format.useAutoBase` 自动基线探测（#894）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 实现 `--base=auto` 与 `format.useAutoBase`（`false`/`true`/`whenAble`）：从当前分支上游跟踪分支（或范围排除端与系列提交的 merge-base）自动推导 base commit 并输出 `base-commit:` / `prerequisite-patch-id:`。

**Out of scope:** 不改变 BLAKE3 仓库 `prerequisite-patch-id` 的既有哈希算法说明。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `build_base_block` 未处理 `"auto"`，`COMPATIBILITY.md` 记录 `--base=auto` 被拒绝 | `src/command/format_patch.rs:1214-1284`、`COMPATIBILITY.md:224` |

**Acceptance criteria:**

- [ ] `--base=auto` 或 `format.useAutoBase=true` 在存在上游/范围基线时自动解析 merge-base 并输出 `base-commit:` trailer。
- [ ] 显式 `--base=auto` 或 `format.useAutoBase=true` 无法确定基线时返回明确失败错误（`LBR-CLI-002`）。
- [ ] `format.useAutoBase=whenAble` 无法确定基线时静默省略 `base-commit:`。
- [ ] `--no-base` 始终覆盖禁用 `base-commit:` 输出。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::base_auto_and_when_able)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-16`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--base=auto与format.useAutoBase; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-16; writeset=序列化于FPG-16; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-24: 封面信支持 `branch.<name>.description` 与默认 `From:` 身份（#891）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-12` 拆出的独立轴：封面信默认读取当前分支 `branch.<name>.description` 填充正文，并在未传 `--from` 时解析本地配置/环境变量身份填入封面信 `From:` 而非留空。

**Out of scope:** `--cover-from-description` 模式校验由 `FPG-25` 处理。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `format_cover_letter` 在 `from_identity` 为 `None` 时输出 `From: \n`，未读 `branch.<name>.description` | `src/command/format_patch.rs:1386-1396,1422-1424` |

**Acceptance criteria:**

- [ ] 未传 `--from` 时封面信 `From:` 使用本地解析的身份（`GIT_COMMITTER_*` / `user.name`+`user.email`）。
- [ ] 当前分支配置了 `branch.<name>.description` 时，默认将其写入封面信正文区域。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::cover_letter_branch_description)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-17`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=封面信branch.description与默认From; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-17; writeset=序列化于FPG-17; release=independent; split-from=FPG-12; exception=N/A`

---

### Task FPG-25: 实现 `--cover-from-description` 与 `format.coverFromDescription` 校验及模式输出（#906）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-13` 拆出的独立轴：支持 `--cover-from-description=<mode>` 与 `format.coverFromDescription`（仅接受 `default`/`none`/`message`/`subject`/`auto`），严格按各模式决定分支描述写入封面信标题还是正文，非法值明确报错。

**Out of scope:** 封面信 diffstat 已在 `FPG-13` 完成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 无 `--cover-from-description` CLI 参数及 `format.coverFromDescription` 校验 | `src/command/format_patch.rs:97-364,1861-1902` |

**Acceptance criteria:**

- [ ] `none` 保留 `*** SUBJECT HERE ***` 与 `*** BLURB HERE ***`。
- [ ] `message` 与 `default` 将整段分支描述写入封面信正文。
- [ ] `subject` 将首段写入 `Subject:`、其余段落写入正文。
- [ ] `auto` 在首段长度 ≤ 100 字符（Git `cover_from_description` 阈值）时同 `subject`，否则同 `message`。
- [ ] 非法 `format.coverFromDescription` 或 `--cover-from-description` 值返回 `LBR-CLI-002` / `InvalidConfig` 错误。
- [ ] CLI `--cover-from-description` 覆盖 `format.coverFromDescription` 配置。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::cover_from_description)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-24`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--cover-from-description与模式输出; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-24; writeset=序列化于FPG-24; release=independent; split-from=FPG-13; exception=N/A`

---

### Task FPG-18: 支持 `--attach=<name>` 自定义 boundary 与 `format.attach` 配置（#895, #912）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 将 `--attach` 与 `--inline` 升级为可选值参数 `--attach[=<boundary>]`/`--inline[=<boundary>]`（支持 `--no-attach`），并读取 `format.attach` 配置。

**Out of scope:** 不更改 multipart MIME part 结构。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `attach`/`inline` 为 `bool` 拒绝 `--attach=<name>`；`resolve_format_config` 未读 `format.attach` | `src/command/format_patch.rs:251-256,883-891,1861-1902` |

**Acceptance criteria:**

- [ ] `--attach=custom-boundary` 与 `--inline=custom-boundary` 生成使用指定 boundary 的 MIME multipart 邮件。
- [ ] 读取 `format.attach`（boolean 或自定义 boundary 字符串）。
- [ ] `--no-attach` 覆盖 `format.attach` 配置关闭 multipart 附件。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::attach_custom_boundary_and_config)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-25`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--attach=name与format.attach; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-25; writeset=序列化于FPG-25; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-19: 实现 `--interdiff=<rev>` 输出支持（#898）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `529 FP-10` 已落地的三态前置条件基础上，保留其「需要封面信或单补丁」的拒绝门禁，退除 `529` 对 `--interdiff` 的「显式不支持」分类与对应拒绝测试，实现 `--interdiff=<rev>` 实际 series interdiff 计算与输出：分别计算旧系列（`<rev>` 对应系列）与当前系列的组合变更差异（而非在基线分叉时直接做两 tip 树对比），在封面信中（或无封面信单补丁的 `---` 与正文 diff 之间）输出 `Interdiff:` 区块。

**Out of scope:** `--range-diff` 仍不在本计划范围内。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/529.md` `FP-10` 仅做前置条件诊断并显式归类 `--interdiff` 为不支持 | `issues/529.md:42,165`、`COMPATIBILITY.md:224` |

**Acceptance criteria:**

- [ ] `--interdiff=<rev>` 在分叉系列夹具上与 `git@6de20f6092` 对照一致输出 `Interdiff:` 区块（多补丁/启用封面时置于封面信；单补丁即使含 `--no-cover-letter` 或 `format.coverLetter=false` 也置于 `---` 与正文 diff 之间）。
- [ ] 未显式指定封面信开关的多补丁系列传入 `--interdiff=<rev>` 时隐含生成封面信（继承 `529 FP-10`）；显式 `--cover-letter` 覆盖 `format.coverLetter=false` 正常输出。
- [ ] 仅在多补丁系列（`total > 1`）且最终封面信状态被显式禁用（`--no-cover-letter` 或未被 CLI 覆盖的 `format.coverLetter=false`）时按 `529 FP-10` 契约拒绝 `--interdiff=<rev>`（仅替换原「不支持」测试）。
- [ ] `<rev>` 无法解析时返回 `LBR-CLI-003` 错误（`docs/error-codes.md`）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::interdiff_cover_and_single_patch)'`（`(new)`，含分叉系列与隐含封面信验证）
- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::interdiff_no_cover_letter_rejected)'`（`(new)`，保留 `529 FP-10` 拒绝回归）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-18`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--interdiff输出支持; recovery=immutable-release; complete=yes; self-contained=yes; AC=4/8; VER=3/8; landing=2; prod-files=1; scope=M; deps=FPG-18; writeset=序列化于FPG-18; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-20: reroll 文件名 `vN-` 前缀与文件路径列表输出到 `stdout`（#901；自本卡拆出 FPG-26）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `--reroll-count`（`-v`）时于输出文件名前添加 `v<val>-` 前缀；非 `--stdout`、非 `--json` 模式下将生成的文件路径列表打印到 `stdout` 而非 `stderr`（自本卡拆出 `FPG-26` 处理非整数 `--reroll-count`）。

**Out of scope:** 非整数 `--reroll-count` 参数解析（由 `FPG-26` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `patch_filename` 未拼接 `vN-` 前缀；文件列表使用 `eprintln!` 输出到 stderr | `src/command/format_patch.rs:610-612,1699-1737` |

**Acceptance criteria:**

- [ ] `-v 2` 生成 `v2-0001-...patch`（含 `v2-0000-cover-letter.patch`）。
- [ ] 非 `--stdout`、非 `--json`、非 `--quiet` 模式下，生成的文件路径逐行输出到 `stdout`（`--quiet` 下抑制）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::reroll_filename_prefix_and_stdout_list)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-19`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=reroll_vN-前缀与stdout列表; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-19; writeset=序列化于FPG-19; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-26: 支持非整数与小数 `--reroll-count` 值（#903）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-20` 拆出的独立轴：将 `FormatPatchArgs::reroll_count` 从 `Option<usize>` 改为 `Option<String>`，支持 Git 允许的小数与非数字 reroll 标记（如 `-v 2.1`、`-v rfc`），同步输出 `[PATCH v<val>]` 与 `v<val>-` 文件名前缀。

**Out of scope:** stdout 文件路径输出已在 `FPG-20` 完成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `reroll_count: Option<usize>` 拒绝非整数值 | `src/command/format_patch.rs:220-226` |

**Acceptance criteria:**

- [ ] `-v 2.1` 与 `-v foo` 合法接受，Subject 渲染为 `[PATCH v2.1]` / `[PATCH vfoo]`，文件名使用 `v2.1-` / `vfoo-` 前缀。
- [ ] 对含路径分隔符或控制字符的非法值拒绝（GC-07）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::reroll_non_integer)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-20`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=非整数--reroll-count支持; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-20; writeset=序列化于FPG-20; release=independent; split-from=FPG-20; exception=N/A`

---

### Task FPG-21: 补丁文件名保留原始大小写并按 `--filename-max-length` 控制上限（#908, #910）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 移除 `patch_slug` 中的 `.to_lowercase()` 以保留提交标题大小写，并移除 `truncate_slug` 的 52 字符硬编码截断，使默认长度上限（Git 64 字节）与显式 `--filename-max-length` / `format.filenameMaxLength` 实际生效。

**Out of scope:** 不改变 `--numbered-files` 的纯数字命名。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `patch_slug` 调用 `.to_lowercase()`；`truncate_slug` 硬编码 `MAX_CHARS = 52` | `src/command/format_patch.rs:1646-1681` |

**Acceptance criteria:**

- [ ] 补丁文件名 slug 保留提交标题的大小写（如 `Fix-Bug` 而非 `fix-bug`）。
- [ ] `--filename-max-length=100` 可生成超过 56 字符的文件名；未传时按默认 64 字节（或 `format.filenameMaxLength`）截断。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::filename_preserves_case_and_max_length)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-26`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=文件名保留大小写与--filename-max-length; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-26; writeset=序列化于FPG-26; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-22: 支持 `format-patch` 的 `-U`/`--unified` 与 `--full-diff` 选项（#904 部分；自本卡拆出 FPG-27）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 在 `FormatPatchArgs` 新增 `-U`/`--unified=<n>` 与 `--full-diff`，并透传至 `log::CommitDiffOptions` 与 `src/command/diff/` 渲染引擎（自本卡拆出 `FPG-27` 处理 `--diff-filter` 与重命名核验）。

**Out of scope:** `--diff-filter` 与 `-M`/`--no-renames` 核验（由 `FPG-27` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `FormatPatchArgs` 无 `-U`/`--unified` 与 `--full-diff`；`CommitDiffOptions` 仅含 4 字段 | `src/command/format_patch.rs:258-284,1852-1859` |

**Acceptance criteria:**

- [ ] `-U<n>` / `--unified=<n>` 控制补丁 hunk 上下文行数。
- [ ] `--full-diff` 在带路径过滤时仍输出完整提交差异。
- [ ] `CommitDiffOptions` 默认值保持向后兼容，不破坏现有调用方。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::unified_and_full_diff)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-21`, `DEP-FPG-02`（核实 `src/command/diff/` 无在途写者）, `DEP-FPG-03`（核实 `src/command/log.rs` 无在途写者）

**Implementation write set:** `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff/`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff/render.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=-U与--full-diff透传; recovery=immutable-release; complete=yes; self-contained=yes; AC=3/8; VER=2/8; landing=4; prod-files=3; scope=M; deps=FPG-21,DEP-FPG-02,DEP-FPG-03; writeset=序列化于FPG-21; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-27: 支持 `--diff-filter` 并核验 `529 FP-06` 的 `-M`/`--no-renames`（#904 部分）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-22` 拆出的独立轴：在 `FormatPatchArgs` 新增 `--diff-filter=<filter>` 透传至 `CommitDiffOptions` 与 `src/command/diff/`，并按 GC-01 核验 `529 FP-06` 交付的 `-M`/`--find-renames`/`--no-renames` 完整闭环 `#904`。

**Out of scope:** `-U` 与 `--full-diff` 已在 `FPG-22` 完成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `issues/529.md` `FP-06` 交付 `-M`/`--find-renames`/`--no-renames`；`FormatPatchArgs` 仍缺 `--diff-filter` | `src/command/format_patch.rs:258-284,1852-1859`、`issues/529.md:180` |

**Acceptance criteria:**

- [ ] `--diff-filter` 正确过滤补丁 diff 与 diffstat 中的文件集合，非法 filter 返回 `LBR-CLI-002`。
- [ ] 核实 `-M`、`--find-renames`、`--no-renames` 与 `git@6de20f6092` 一致并闭环 `#904`。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::diff_filter)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-22`, `DEP-FPG-02`（核实 `src/command/diff/` 无在途写者）, `DEP-FPG-03`（核实 `src/command/log.rs` 无在途写者）

**Implementation write set:** `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff/`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff/options.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--diff-filter与重命名选项核验; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=4; prod-files=3; scope=M; deps=FPG-22,DEP-FPG-02,DEP-FPG-03; writeset=序列化于FPG-22; release=independent; split-from=FPG-22; exception=N/A`

---

### Task FPG-23: 修复 `--thread` 默认值、`format.thread` 与 `--thread=deep`（#917, #916 部分；自本卡拆出 FPG-28）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 将 `--thread` 默认行为对齐 Git（默认关闭，仅当传入 `--thread[=shallow|deep]` 或配置 `format.thread` 为 `true`/`shallow`/`deep` 时启用；`format.thread=false`/空值关闭线程），并支持 `--thread=deep` 链式回复（自本卡拆出 `FPG-28` 处理非子串唯一 Message-ID）。

**Out of scope:** Message-ID 子串去重（由 `FPG-28` 承接）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `--thread` 设为 `default_value_t = true` 且不接受 `=shallow`/`=deep`；`resolve_format_config` 未读 `format.thread` | `src/command/format_patch.rs:183-188,1861-1902` |

**Acceptance criteria:**

- [ ] 默认不输出 `Message-ID`/`In-Reply-To`/`References`（除非启用 `--thread`、`--in-reply-to` 或 `format.thread`）。
- [ ] 支持 `--thread=shallow`、`--thread=deep` 与 `format.thread`（`true`/`false`/空/`shallow`/`deep`）。
- [ ] ER-06/ER-06a 同卡强制门：逐文件更新所列全部文档并验收。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::format_thread_default_off_and_deep)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_flag_surface`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-27`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_flag_surface.rs`, `COMPATIBILITY.md`

**Docs and compatibility impact:** 逐文件更新 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `M`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=--thread默认、format.thread与deep; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=M; deps=FPG-27; writeset=序列化于FPG-27; release=independent; split-from=N/A; exception=N/A`

---

### Task FPG-28: 修复 `build_thread_id` 生成互不为子串的唯一 `Message-ID`（#916 部分）

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / `空`

**Description:** 自 `FPG-23` 拆出的独立轴：修改 `build_thread_id` / `push_threading_headers` 中的 Message-ID 生成格式，确保同系列任意两封邮件（含封面信与首个补丁）的 `Message-ID` 互不为另一 ID 的子串。

**Out of scope:** `--thread` 默认开关与 `format.thread` 配置已在 `FPG-23` 完成。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `push_threading_headers` 中首封用 `<{series_id}>` 而后续用 `<{series_id}-p{n}>`，前者为后者子串 | `src/command/format_patch.rs:1331-1336,1409` |

**Acceptance criteria:**

- [ ] 同系列中任意两封邮件的 `<Message-ID>` 字符串互不包含对方作为子串。
- [ ] `In-Reply-To` 与 `References` 在 `shallow` 与 `deep` 模式下均正确指向对应父邮件 `Message-ID`。
- [ ] ER-06/ER-06a 同卡强制门：核对并同步所列文档。

**Verification:**

- [ ] `source .env.test && cargo nextest run --test command_test -E 'test(=command::format_patch_test::message_id_no_substring)'`（`(new)`）
- [ ] `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`

**Full-suite trigger:** `Inherited`

**Dependencies:** `FPG-23`

**Implementation write set:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`, `docs/commands/format-patch.md`, `docs/commands/zh-CN/format-patch.md`, `docs/development/commands/format-patch.md`, `COMPATIBILITY.md`, `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md`

**Release write set:** `Inherited`

**Files likely touched:** `src/command/format_patch.rs`, `tests/command/format_patch_test.rs`, `tests/compat/format_patch_mail_roundtrip_test.rs`

**Docs and compatibility impact:** 逐文件核对并同步 `docs/commands/format-patch.md`、`docs/commands/zh-CN/format-patch.md`、`docs/development/commands/format-patch.md`、`COMPATIBILITY.md:224`，并在 `/Volumes/Sea/libra-backend`（确认 `cf` 分支）运行 `cd /Volumes/Sea/libra-backend && python3 scripts/sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`。

**Rollback mode:** `immutable-release`

**Estimated scope:** `S`

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=非子串唯一Message-ID; recovery=immutable-release; complete=yes; self-contained=yes; AC=2/8; VER=2/8; landing=2; prod-files=1; scope=S; deps=FPG-23; writeset=序列化于FPG-23; release=independent; split-from=FPG-23; exception=N/A`

---

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | `FormatPatchError`、邮件头编码/折叠、Message-ID 唯一性、slug/filename 截断 | `source .env.test && cargo nextest run --lib format_patch` |
| 集成 | 全部 35 项 issue 行为用例（编号、配置、环境变量、封面信、interdiff、diff 选项、线程化） | `source .env.test && cargo nextest run --test command_test -E 'test(command::format_patch_test::)'` |
| 兼容 | Git↔Libra `am` 往返、flag surface 契约、BrokenPipe 输出 | `source .env.test && cargo nextest run --test compat_format_patch_mail_roundtrip`、`source .env.test && cargo nextest run --test compat_format_patch_flag_surface` |
| 迁移 | N/A（无 SQLite schema 变更） | N/A |
| 安全 | 邮件头 CR/LF 注入过滤、`--relative` / `--output-directory` 路径校验 | `source .env.test && cargo nextest run --test command_test -E 'test(command::format_patch_test::)'` |
| 性能 | 继承 GC-10，无无界全库遍历 | `source .env.test && cargo nextest run --test command_test -E 'test(command::format_patch_test::)'` |
| live/gated | 每卡 T-1 与收口阶段双环境全量回归 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| FPG-01 | #880, #896; `format_patch.rs:855-861,1861` | `src/command/format_patch.rs` | `format-patch.md` EN/zh/dev + `COMPATIBILITY.md:224` + 后端同步 | `command_test::format_patch_test::format_numbered_config` `(new)` |
| FPG-02 | #881（身份切片）; `format_patch.rs:2050-2065` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::signoff_git_committer_env_precedence` `(new)` |
| FPG-03 | #882; `format_patch.rs:688-750,1126` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::ignore_if_in_upstream_tag_t4014_4` `(new)` |
| FPG-04 | #883, #914; `format_patch.rs:194-210,1918` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::format_headers_and_no_to_ordering` `(new)` |
| FPG-05 | #884; `format_patch.rs:236-240,1012` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::format_notes_and_no_notes` `(new)` |
| FPG-06 | #885; `format_patch.rs:1062-1071` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::format_signature_config_and_prefix` `(new)` |
| FPG-07 | #911; `format_patch.rs:833,1497` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::multiline_subject_join` `(new)` |
| FPG-29 | #886; `format_patch.rs:863-869` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::subject_header_folding` `(new)` |
| FPG-08 | #887; `format_patch.rs:347-358,1981` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::encode_email_headers_default_on` `(new)` |
| FPG-30 | #897, #881（t4036 闭环）; `format_patch.rs:897` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::pure_ascii_omits_mime_headers` `(new)` |
| FPG-09 | #888; `format_patch.rs:437-458` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::fp03_empty_rfc_with_k` |
| FPG-10 | #889; `format_patch.rs:2022-2032` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::from_invalid_ident_rejected` `(new)` |
| FPG-31 | #915; `format_patch.rs:212-217,1861` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::format_from_and_no_from` `(new)` |
| FPG-11 | #890; `format_patch.rs:500-506,1852` | `src/command/format_patch.rs`, `src/command/log.rs` | 同上 | `command_test::format_patch_test::diff_relative_filters_paths` `(new)` |
| FPG-12 | #900, #909; `format_patch.rs:549,1861` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::format_cover_letter_auto_config` `(new)` |
| FPG-13 | #905; `format_patch.rs:1428-1444` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::cover_letter_includes_diffstat` `(new)` |
| FPG-14 | #899; `format_patch.rs:1621-1644` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::commit_list_format_extended_placeholders` `(new)` |
| FPG-15 | #892; `format_patch.rs:108-109,663` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::revision_range_caret_exclusion` `(new)` |
| FPG-16 | #893; `format_patch.rs:803,1373` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::mbox_envelope_fixed_2001_timestamp` `(new)` |
| FPG-17 | #894; `format_patch.rs:1214-1284` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::base_auto_and_when_able` `(new)` |
| FPG-24 | #891; `format_patch.rs:1386,1422` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::cover_letter_branch_description` `(new)` |
| FPG-25 | #906; `format_patch.rs:97-364` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::cover_from_description` `(new)` |
| FPG-18 | #895, #912; `format_patch.rs:251,883` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::attach_custom_boundary_and_config` `(new)` |
| FPG-19 | #898; `format_patch.rs:97-364` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::interdiff_cover_and_single_patch` `(new)` |
| FPG-20 | #901; `format_patch.rs:610,1699` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::reroll_filename_prefix_and_stdout_list` `(new)` |
| FPG-26 | #903; `format_patch.rs:220-226` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::reroll_non_integer` `(new)` |
| FPG-21 | #908, #910; `format_patch.rs:1646-1681` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::filename_preserves_case_and_max_length` `(new)` |
| FPG-22 | #904; `format_patch.rs:258,1852` | `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff/` | 同上 | `command_test::format_patch_test::unified_and_full_diff` `(new)` |
| FPG-27 | #904; `format_patch.rs:258,1852` | `src/command/format_patch.rs`, `src/command/log.rs`, `src/command/diff/` | 同上 | `command_test::format_patch_test::diff_filter` `(new)` |
| FPG-23 | #917, #916; `format_patch.rs:183,1861` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::format_thread_default_off_and_deep` `(new)` |
| FPG-28 | #916; `format_patch.rs:1331-1336` | `src/command/format_patch.rs` | 同上 | `command_test::format_patch_test::message_id_no_substring` `(new)` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 计划评审通过且 `DEP-FPG-01`（`529.md`）释放写锁 | Claude & Codex `PASS` + `529 CLOSE-529` 完成 | N/A |
| M1 | Phase 0（`FPG-01`..`FPG-10`, `FPG-29`..`FPG-31`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 发布前 revert 本地提交；发布后按 ER-10 新建 `FIX-*` 前滚发新版本 |
| M2 | Phase 1（`FPG-11`..`FPG-17`, `FPG-24`, `FPG-25`）全部 `done`/`complete` | 各卡 `patch + 1`、`nextest` 与 D 组全绿 | 同上 |
| M3 | Phase 2（`FPG-18`..`FPG-23`, `FPG-26`..`FPG-28`）全部 `done`/`complete` + 全量收口门通过 | 全量 `nextest` + `fmt` + `clippy` 全绿 | 同上 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 渲染阶段中途失败（如非法 config / 无效 `--from`） | 无补丁文件写入 | 返回 `CliError` 并中止，无半成品文件残留 | 写入部分补丁文件 |
| `write_patch_file` 单文件 I/O 失败 | 已写入的前序完整补丁文件 | 单文件 `atomic_write` 保证无半截文件，返回 `IoWriteFailed` | 静默截断文件 |
| 远端 D 组 CI 失败 | 已推送版本 tag 与提交 | 按 ER-04/ER-10 新建 `FIX-*` 前滚修复 | 回退已推送提交或删除已发布 tag |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| `issues/529.md` 写锁未释放前并发改动 `format_patch.rs` | 高：代码冲突与重复实现 | 严格执行 `DEP-FPG-01` 门禁，`FPG-01` 在 `529 CLOSE-529` 后开工 | FPG-01..FPG-31 |
| 默认开启 `--encode-email-headers`（#887）、纯 ASCII 省略 MIME（#897）及 `--thread` 默认关闭（#917）改变既有测试输出 | 中：既有断言需同步调整 | 同卡更新对应断言并对照 `git@6de20f6092` 验证 | FPG-08, FPG-30, FPG-23 |
| `COMPATIBILITY.md` 增删行导致 `compat_ledger_schema` 失败 | 中：CI 兼容账本守卫报错 | 仅原地修改第 224 行，不增删行 | FPG-01..FPG-31 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 系列补丁渲染 | `O(N_commits * commit_diff)` | `O(N_commits)` 内存缓冲（先渲染后逐文件 `atomic_write`） | 继承 GC-10，单系列受现有 repo 规模约束 | `command_test::format_patch_test` |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 第 224 行已原地同步。
- [ ] `docs/commands/format-patch.md` 已同步。
- [ ] `docs/commands/zh-CN/format-patch.md` 已同步。
- [ ] `docs/development/commands/format-patch.md` 已同步。
- [ ] `/Volumes/Sea/libra-backend/apps/tanstack-app/content/docs/commands/format-patch.en.md` 已按 `GC-05`（`AGENTS.md:135`）在 `/Volumes/Sea/libra-backend`（`cf` 分支）完成 `sync-libra-command-docs.py --write --only format-patch --libra-root /Volumes/Data/gitmono/libra`、前端校验、Claude Code `PASS`、签名提交推送 `cf` 与 Cloudflare 部署核验。
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
| R5 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | 网络中断（`ECONNRESET`） | 无 | R8 重跑 |
| R5 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `FPG-19` 误写 tip-to-tip 树对比（应为 series interdiff） | `FPG-30` 回滚不应引用 `--encode-email-headers` | R6 修订 |
| R6 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `FPG-19` 拒绝判据未限定多补丁系列（`total > 1`） | 补充单补丁禁用源与 CLI 覆盖回归 | R7 修订 |
| R7 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | 无 P0/P1 | P2: 28 卡 `(new)` 测试使用子串过滤而非 `test(=command::...)` 精确过滤 | R8 修订 |
| R8 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `PASS` | 无 P0/P1 | P2: 文头/FPG-19 计数/R7 描述微调 | R9 顺带清理 |
| R8 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `FPG-09` `|` 合并精确与子串过滤可能掩蔽零命中 | 无 | R9 修订 |
| R9 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | 未重跑（R8 已 `PASS`，随 R9 修订一并复审） | 无 | R10 重跑 |
| R9 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: ① 网站路径 `../libra-backend` 与 `AGENTS.md:135` `/Volumes/Sea/libra-backend` 不一致且缺前端/部署门；② 缺 `COMPATIBILITY.md`/版本面/`plan-status.md`/网站跨计划共享写锁（`DEP-FPG-04`） | 无 | R9 修订 |
| R10 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | 未运行（随 R10 修订一并复审） | 无 | R11 重跑 |
| R10 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `DEP-FPG-04` 在提交推送后即释放发布锁（须持续至 `gh release create` 与 D 组核验完成并明确 `plan-status.md` 登记方式） | P2: 粒度审计表 `FPG-19` `AC=5/8` 与卡内 `4/8` 不一致 | R10 修订 |
| R11 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | 未运行（随 R11 修订一并复审） | 无 | R12 重跑 |
| R11 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `DEP-FPG-04` 缺少跨工作区可见的原子获取与并发仲裁协议 | 无 | R11 修订 |
| R12 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | 未运行（随 R12 修订一并复审） | 无 | R13 重跑 |
| R12 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `DEP-FPG-04` 缺少单一授权发布者身份/授权机制及所有参与计划共同遵守的串行仲裁规则 | 无 | R12 修订 |
| R13 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | 未运行（随 R13 修订一并复审） | 无 | R14 重跑 |
| R13 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `FAIL` | P1: `DEP-FPG-04` 未将共同仲裁规则合入及各在途计划采纳设为开工前置条件 | 无 | R13 修订 |
| R14 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `FAIL` | 未运行（待 R15 复审） | 无 | R15 重跑 |
| R14 | Codex (`gpt-6.1-terra`, medium) | `plan-20261117-format-patch.md` | `PASS` | 无 P0/P1 | P2: `FPG-01`/粒度表/Phase 0 可同步补列 `DEP-FPG-04`（全局依赖表已强制） | R14 复审通过 |
| R15 | Claude (`claude-opus-5-5`, medium) | `plan-20261117-format-patch.md` | `PASS` | 无 P0/P1 | P2: 第 200 行发布者表述与 `DEP-FPG-04` 统一、`FPG-01`/Phase 0 同步列 `DEP-FPG-04`（可接受残余风险） | R15 复审通过 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-FPG-01 | `format-patch --range-diff` | 不在本次 35 项 issue 范围内 | 用户或新 issue 明确提出需求 | `COMPATIBILITY.md` |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [ ] 所有任务卡满足粒度规则 `G-*`：无未登记的 L 例外、无 XL 卡、无碎片卡、无未登记的合并发布例外、实现写集冲突均已消解；「任务卡粒度审计表」已填齐。
- [ ] 所有非延后任务的 acceptance criteria 已满足，且 `Lifecycle=done` **且** `Acceptance=complete`（ER-04）。
- [ ] 所有任务的 Verification 命令已运行并记录结果。
- [ ] **计划完成门 / 全量收口门（ER-13）**：全部任务卡完成后运行 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 全绿。
- [ ] **全量收口门暴露的 Bug 已全部处理（ER-13）**：定位到本计划的失败已前滚修复并重跑全量至全绿。
- [ ] 必要的 docs/compat/test-index 更新已完成。
- [ ] 每张卡的 `Rollback mode` 已被验证或记录。
- [ ] Claude 与 Codex 最终评审结论均为 `PASS`，P0/P1 全部关闭。
- [ ] 每张 `independent` 发布卡已执行 `patch + 1`、版本面集合一致、`Cargo.lock` 仅由工具链刷新、构建/安装/提交/推送及 `gh release create` 完成并取得证据（ER-08）。
- [ ] 「修订历史」已记录成稿后的全部规范性变更（G-09）。
- [ ] 本计划未新建证据文件或证据文件夹（v2.12）。
