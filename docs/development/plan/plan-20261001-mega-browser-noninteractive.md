# Mega2 browser 非交互操作计划（2026-10-01）

> **模板版本：** [`plan-template.md`](plan-template.md) `v2.12`（2026-09-29 15:35:39 UTC 起生效）。`GC-01..GC-13`、`ER-01..ER-14`、`G-01..G-11` 全部适用；本文只补充本计划特有的约束、决策与任务卡。
>
> **状态：** 已收口（2026-10-02 01:08:49 UTC 起按发布窗口顺序执行，发布者见修订历史）。MN-10 `done` / `complete`（v0.30.14）；MN-01 `done` / `complete`（v0.30.15）；MN-02 `done` / `complete`（v0.30.16）；MN-03 `done` / `complete`（v0.30.17）；MN-11 `done` / `complete`（v0.30.18）；MN-04 `done` / `complete`（v0.30.19）；MN-08 `done` / `complete`（v0.30.20）；MN-12 `done` / `complete`（v0.30.21）；MN-05 `done` / `complete`（v0.30.24；v0.30.22、v0.30.23 先后被并发发布占用，两次重建见修订历史）；MN-06 `done` / `complete`（v0.30.25）；MN-09 `done` / `complete`（v0.30.26）；MN-07 `done` / `complete`（v0.30.27）；12 卡全部完成，计划级全量门 8839/8839 与网站示例门 1/1 全绿；最终收口审查字面 `VERDICT: PASS`；本次仅交付三个计划文档的行政收口记录；逐卡进度见各卡 `Lifecycle / Acceptance` 与「实施证据汇总」。计划级 Codex review 已取得 `PASS`：R1–R14 `FAIL` 均已修订，R15 全文评审 `PASS`，R16 对其后改动的差异确认 `PASS`；提交前上游前移到 `7f810da`，R17 对锚点复核的差异评审 `FAIL`（1×P1）已修订，R18 差异确认 `PASS`（见「Codex review log」）；全部任务卡已按 ER-MN-03 完成实现评审、逐卡验收与发布。
>
> **PR #609 行政收口状态（2026-10-04 08:27 UTC）：** PR 仍为 OPEN，当前远端 head `85776a561420211e9f807ead770f31fe165cd902`、base `892f37cf898be4d670a84ad8e45af3270a70928c`。新诊断确认 `opencode-export-linux` 拒绝 `/usr/local/bin/bwrap` 是因为 `/usr/local/bin` 对 runner 可写；现将下一版 workflow 改为 root 安装到 `/opt/libra-bubblewrap/bin/bwrap` 并通过 `LIBRA_LINUX_SANDBOX_EXE` 指定，等待提交后的 Linux 验证。格式、actionlint、全目标全特性 Clippy 已通过；最新 Actions run 尚有其他 job 在运行。
>
> **文件名：** 模板 v2.11 规定后缀词只能是 `[a-z]+`，不能写 `mega2`，因此用 `mega-browser-noninteractive` 指代 `libra mega2 browser` 的非交互操作。

## 文档职责

本文解决的问题是：`libra mega2 browser` 的建目录、删除/移动/改名目录、tag 列表/翻页/创建/删除，以及无 TTY 时的人读列表，在立项基线只能在交互 TUI 里完成，脚本、CI 和测试无法驱动。目标是给 browser 的**每一项功能**提供一条非交互调用：不需要终端，不读 stdin，不提示确认；通过本地校验后**恰好发出一次** HTTP 请求；用稳定的 envelope 输出结果；失败时给出可断言的 stable code、退出码和 HTTP 诊断细节。做到这些之后，Mega2 的黑盒测试就可以把真实的 `libra` 二进制当客户端，驱动 Mega2 storage-only（trunk）产品 HTTP 面，并对结果断言。

本文保留立项时的计划、源码锚点和任务判据，并在「实施证据汇总」记录执行结果；当前 12 张任务卡均已完成，计划级全量门和网站示例门全绿，最终收口审查字面 `VERDICT: PASS`，本次同步三个计划文档的收口记录。落地时每个任务都必须先刷新源码锚点（ER-02），再按任务卡验收。

### 本计划使用规则

- 每张卡推进 `Lifecycle` / `Acceptance`、发布版本或 commit 时，必须在同一变更中同步 [`plan-status.md`](plan-status.md)「计划一览」中本计划的行（时间格式 `YYYY-MM-DD HH:MM:SS UTC`），并写入本文「实施证据汇总」对应小节。不得新建证据文件或文件夹（模板 v2.12）。
- `plan-status.md` 由多个 Agent 并发编辑。提交前核对它的 diff 只含本计划相关的行；如果混有他人未提交的改动，先协调，不得混入本计划的提交（GC-12）。
- 计划成稿后的每次规范性变更，都在「修订历史」登记一行。

### 适用范围

- 在**同一个** `libra mega2 browser` 子命令上新增互斥的操作 flag：`--list`、`--create-dir <NAME>`、`--delete-dir <NAME>`、`--move-dir <NAME> <PARENT-PATH>`、`--rename-dir <NAME> <NEW-NAME>`、`--list-tags [--page <N>] [--per-page <N>]`、`--create-tag <NAME> [--message <TEXT>]`、`--delete-tag <NAME>`。
- 非交互操作的唯一登记表与按类别实现一次的通用规则（ADR-MN-08）。
- 四个 mega2 协议客户端（`src/internal/protocol/mega2_{tree,entry,mutate,tag}.rs`）的失败诊断：在 Libra JSON 错误信封的 `details` 中提供 `method`、`route`，以及 `http_status` 或 `transport`。
- 安全修复：`--server` URL 无法解析时，错误不再回显原始输入（其中可能含凭据）。
- 每个操作的人读输出、`--json`/`--machine` payload、stable code 与退出码表、请求计数契约。
- 文档：`docs/commands/mega2.md`（EN）、`docs/commands/zh-CN/mega2.md`、相邻网站页 `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`（部署在 `https://libra.tools/en/docs/commands/mega2`）、`docs/development/commands/mega2.md`、`docs/error-codes.md`（`details` 键）、`COMPATIBILITY.md:248`（原位改写）、`docs/development/commands/_compatibility.md:57`、`docs/development/commands/README.md:57`；MN-07 另改 `CLAUDE.md` 的测试环境变量清单与 `docs/development/integration/integration-test-plan.md`。
- 测试：新 L1 文件 `tests/command/mega2_browser_noninteractive_test.rs`，既有 transport/CLI 测试文件中追加的用例，以及在 Libra 仓内运行、env 门控的 live 门。

### 非目标

- **不新增**任何 `mega2` 子命令。plan-20260912 的 `mkdir`/`rmdir`/`mv`/`tag` 子命令墓碑（MB-06/09/12）继续有效。本计划不改 `src/cli.rs` 的代码；唯一的例外是 MN-03 改写其中一处注释（GC-MN-01）。
- 不改变交互 TUI 的键位、渲染、请求序列和错误显示（GC-MN-05）。
- 不提供 browser 现有功能以外的远端能力：文件条目的创建/删除/移动/编辑、blob 预览、`GET /api/v1/tags/{name}`、非根 tag path、tag 的 `target`/`tagger_*` 字段、一次请求同时换父目录与改名、`POST /api/v1/path/provision`、`POST /api/v1/import-repo/remove`（见 `DEFER-MN-*`）。
- 不改动任何既有 stable code 映射（例如 delete/move 的 404 仍是 `LBR-NET-002`，create-entry 的 500 仍是 `LBR-NET-002`），只增加 `details`。映射统一需要另立 ADR（`DEFER-MN-06`）。
- 不提供批处理/脚本模式、自动重试、轮询或可配置超时，也不回显服务端 `err_message` 或响应体。
- 不修改 mega2 仓的任何文件。mega2 仓内的 Libra 用例由 mega2 侧在其 `scripts/libra_smoke_storage_only.sh`（mega2 plan-20261001 的 Libra 域，`BB-65` 起编号已预留）落地，经 `DEP-MN-03` 交付；mega2 现有的 curl + git smoke 脚本（`scripts/git_protocol_smoke_storage_only.sh`、`scripts/api_write_smoke_storage_only.sh`）任何一方都不得加入 libra 用例，那才是真正违反 mega2 现有规定（ADR-MN-07）。

### 成功定义

- browser 的每一项 TUI 功能都有对应的非交互调用（映射见 ADR-MN-01），并且能在 stdin 关闭、stdout/stderr 都是管道的环境中运行。
- 每次非交互调用在通过本地校验后恰好发出一次 HTTP 请求，本地校验失败时一次也不发；不 reload、不 preflight、不重试、不提示。
- `--json`/`--machine` 下每个操作都输出带 `data.operation` 的稳定 envelope，列表 payload 只新增 `operation` 一个字段。失败时，stderr 上的 JSON 错误信封带 `details.method`、`details.route`，以及 `http_status` 或 `transport`；文档给出逐操作的 stable code 与退出码表。
- 读操作匿名，并拒绝 token flag；写操作按 ADR-MB-03 的优先序取 token；任何输出（含 URL 解析错误）都不含 token 或 URL 凭据。
- 交互 TUI 行为不变；`mega2` 仍然只有 `browser` 一个子命令。
- `MN-01..MN-12` 全部 `done`/`complete`（各自 patch 发布并取得 D 组证据，含 libra.tools stable 清单与网站标记核对）。Libra 仓内的 12 个 live 门在一个 pin 住 revision 的真实 Mega2 实例上通过，或按 `DEP-MN-04` 的失败策略正式延后。收口全量门全绿。`DEP-MN-03` 已向 mega2 侧交付（stable 版本号与契约文档链接）。

## 事实基线

> 所有行号与外部 pin 都要在开工当日按 ER-02 重刷；外部 mega2 pin 不随浮动的 `main` 自动前进。下表核对时间为 2026-10-01 12:04:09 UTC 至 13:46:48 UTC，Libra `main@9c1601b`（`0.30.10`）。本地 checkout 在 2026-10-01 12:02:18 UTC 备份工作区之后从 `c33f28b`（`0.30.8`）快进到 `9c1601b`：期间的 v0.30.9/v0.30.10（issue #574）只让 `src/cli.rs` 的 dispatch 行从 `:3610` 移到 `:3627`，mega2 相关源码、测试与文档未变。提交前（2026-10-02 00:52:28 UTC）上游已发布 v0.30.11–v0.30.13（issues/574 收口），本表对 `main@7f810da`（`0.30.13`）复核：其间改动的 31 个文件里，本计划引用到的锚点只有三处变化——`docs/development/commands/_compatibility.md` 的 mega2 行从 `:55` 移到 `:57`，`docs/development/integration/integration-test-plan.md` 的 Wave 3 标题从 `:245` 移到 `:247`，三处版本面变为 `0.30.13`；`plan-status.md` 删除了 `DEP-QP-01` 行。`COMPATIBILITY.md:248,259-261` 与 `install.sh:14,1150-1157` 内容未变，mega2 相关源码、测试与其余被引用的文档都未改动。

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 代码入口 | `mega2 browser` 只有两条路径：`--json`/`--machine` 时做一次 GET 并输出 listing；否则解析 token 后进入 TUI | `src/command/mega2.rs:173-211`（JSON 分支 `:180-196`；非 JSON 的 `--quiet` 被拒 `:198-204`；TUI `:206-210`） |
| 参数 | `BrowserArgs{server, path(默认 "/"), git_ref, token_file, token}`，没有任何操作 flag | `src/command/mega2.rs:79-101` |
| 机器输出 | `BrowserData{server, ref, path, items[{name, content_type}]}`，经 `emit_json_data("mega2 browser", …)` 输出 | `src/command/mega2.rs:103-154,192-195`；`src/utils/output.rs:412-424` |
| token 规则 | `--json` 带 token flag → `LBR-CLI-002`，消息含「TUI-only」；`LIBRA_MEGA2_TOKEN` 只在 TUI 路径读取 | `src/command/mega2.rs:181-189,206-208`；`src/internal/protocol/mega2_auth.rs:120-167` |
| URL 解析错误回显 | `validate_server_url` 解析失败时把原始 `--server` 写进错误消息，且早于凭据检查；三处文档却承诺错误不回显凭据 | `src/internal/protocol/mega2_tree.rs:88-92,110-115`；`docs/commands/mega2.md:174`；`docs/commands/zh-CN/mega2.md:147`；网站 `mega2.en.md:156` |
| TUI 动作 | `ActionResult` 有 FetchCurrent/CreateDirectory/DeleteDirectory/MoveDirectory/FetchTags/CreateTag/DeleteTag；每个写动作是一次写请求加一次 reload；delete/move 传 `known_type=Some(Directory)` | `src/command/mega2_browser/mod.rs:61-98,649-753` |
| TTY 门 | 人读交互模式在非 TTY 下返回 `LBR-UNSUPPORTED-001`，零请求 | `src/command/mega2_browser/mod.rs:563-613`；`tests/command/mega2_browser_cli_test.rs:268` |
| tag 面板常量 | 页大小 20；message ≤ 1024 字节；编辑器丢弃控制字符；空 message 表示 lightweight；固定 root `path="/"` | `src/command/mega2_browser/tag_panel.rs:13-16,148-172`；`mod.rs:703,733,741` |
| 协议客户端 | tree、create-entry、delete-entry/move-entry、tags 四个客户端各自映射状态码；错误只有 message 与 stable code，没有 `details` | `src/internal/protocol/mega2_tree.rs:313-382`；`mega2_entry.rs:148-260`；`mega2_mutate.rs:222-286,363-467`；`mega2_tag.rs:224-294,385-460` |
| 状态映射不一致 | delete/move 的 404 落入 `LBR-NET-002`（与 500 相同）；tag 的 404 是 `LBR-CLI-003`；create-entry 的 400 是 `LBR-CLI-003`（默认退出码 129），其 500 是 `LBR-NET-002` | `mega2_mutate.rs:249-266`；`mega2_tag.rs:245-262`；`mega2_entry.rs:197-214`；`src/utils/error.rs:480-494` |
| clap 用法错误 | 子命令内的参数冲突（如互斥组）经 `command_usage` 映射为 `LBR-CLI-002`，默认退出码 129 | `src/utils/error.rs:804-807`；`src/cli.rs:2939-2943` |
| 错误信封 | `CliError::with_detail` 写入 JSON `details`，发布后视为公开契约；`--json` 时错误 JSON 写 stderr；默认退出码 128/129，`LIBRA_FINE_EXIT_CODES=1` 时为类别码 | `src/utils/error.rs:984-990,1016-1030,1085-1107,1209-1226,500-511`；`src/main.rs:425-445` |
| 错误码文档 | `docs/error-codes.md` 的「JSON Schema」节描述错误信封与 `details` 字段；该文件编入二进制（`libra help error-codes`） | `docs/error-codes.md:415-429`；`tests/compat/error_codes_doc_sync.rs:22` |
| 命令注册 | `Commands::Mega2`：preflight 为 none，scope 为 ReadOnly，`MutationClass` 早退为 `ExternalOrUnknown` | `src/cli.rs:716-720,1587,1977,2006-2012,3627` |
| 文档示例缺陷 | help EXAMPLES 与三处文档用未 rooted 的 `src`、`src/pkg`，而 `normalize_path` 拒绝不以 `/` 开头的路径；照抄示例会失败 | `src/command/mega2.rs:37,48`；`docs/commands/mega2.md:194`；`docs/commands/zh-CN/mega2.md:164`；`../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md:166`；`src/internal/protocol/mega2_tree.rs:164-170`；`tests/command/mega2_browser_cli_test.rs:244-265` |
| 守卫测试 | 唯一钉住「token 仅 TUI、`--json` 永不 POST」的用例；帮助守卫禁止 browser help 出现 `mkdir`/`rmdir`/` mv `，禁止 parent help 出现 `mkdir`/`rmdir`/`mv `/`delete-entry`/`move-entry`/`tag ` | `tests/command/mega2_browser_mkdir_test.rs:311-369`；`mega2_browser_mutate_test.rs:322-341`；`mega2_browser_tag_test.rs:476-484`；`mega2_browser_cli_test.rs:358-374` |
| 测试编排 | `tests/command/*.rs` 经 `tests/command/mod.rs:957-965` 编入 `command_test`（用例全名形如 `command::<文件名>::<函数名>`）；每个文件自带 loopback mock，以 `env_clear()` 启动真实二进制；`Cli` 为 `pub(crate)`，crate 内单元测试可直接解析 argv | `tests/command/mod.rs:957-965`；`tests/command_test.rs`；`tests/command/mega2_browser_cli_test.rs:118-129`；`src/cli.rs:226-233` |
| nextest | 本机 `cargo-nextest 0.9.143`；`--test <TEST>`、`--lib` 选择二进制，位置参数是测试名过滤器（可多个），`-- --exact` 要求精确匹配 | `cargo nextest run --help`（2026-10-01） |
| libtest 多过滤器 | 测试二进制的用法为 `[OPTIONS] [FILTERS...]`：可传多个位置过滤器，运行匹配任一过滤器的用例（取并集）；`--exact` 时逐个过滤器做全名匹配。因此 `cargo test --test command_test -- <过滤器 1> <过滤器 2>` 合法 | 本仓无 toolchain pin，`cargo 1.98.1`；Rust 1.98.1 标准库 `library/test/src/cli.rs:157-163`（用法与「Multiple filter strings may be passed, which will run all tests matching any of the filters」）、`library/test/src/lib.rs:545-556`（`opts.filters.iter().any(…)`）；2026-10-01 14:18:06 UTC 实测：rustc 1.98.1 编译的三用例测试二进制以 `alpha beta` 两个过滤器运行，2 个通过、1 个被过滤 |
| 兼容矩阵 | mega2 行在 `COMPATIBILITY.md:248`，写着「token flags are TUI-only and machine mode never POSTs」；compat ledger 以绝对行号解析 `COMPATIBILITY.md:<line>` 证据，目前引用的最大行号是 180；另有文档引用第 259–261 行 | `COMPATIBILITY.md:248`；`tests/compat-ledger/t4/DIRECT_SNAPSHOT.tsv:7`；`tests/compat/compat_ledger_schema.rs:486-499`；`docs/development/gap/grit-suite-scope.md:173` |
| 用户文档 | EN、zh-CN 与网站都写着 token flag 仅 TUI 可用、机器模式永不 POST | `docs/commands/mega2.md:113-116`；`docs/commands/zh-CN/mega2.md:96-98`；网站 `mega2.en.md:78-81` |
| 网站部署 | 网站页部署在 `https://libra.tools/en/docs/commands/mega2`，服务端渲染（`curl` 取得 HTTP 200，页面文本可直接 `rg`）；`/docs/commands/mega2` 307 跳转到带语言前缀的路径 | `curl -sS -o /dev/null -w '%{http_code} %{content_type}'` 与 `curl -fsSL … | rg -c -F -- '--token-file'`（2026-10-01 13:46:22 UTC：HTTP 200、`text/html`，计数 4）；`../libra-backend/apps/tanstack-app/wrangler.jsonc:16,33-41`；`../libra-backend/apps/tanstack-app/src/routes/$lang/(root)/docs.$.tsx` |
| 版本与签名 | 当前 `0.30.13`，三处版本面一致；最新发布 `v0.30.13`（`publishedAt` 2026-10-01 18:54:41 UTC）；`commit.gpgSign=true`、`vault.signing=true`（最近一次本地 libra 提交 `c33f28b` 带 `gpgsig`） | `Cargo.toml`；`install.sh:21`；`install.ps1:25`；`gh -R libra-tools/libra release list`；`libra config get`（2026-10-01，2026-10-02 复核） |
| 外部参照：mega2 路由与 DTO | `../mega2`（Libra 仓库）`main@8ff880c`（2026-10-01；相对 `7b2a435`/`v0.40.15` 只改 README、LICENSE 与 Cargo 元数据）：路由与 DTO 字段与 Libra 客户端一致，仅行号相对 plan-20260912 的 pin `a1293686` 漂移 | mega2@`8ff880c`：`src/api/router/preview_router.rs:61,115,173,201,266`；`src/api/api_router.rs:72,83`；`src/api/router/tag_router.rs:155,219,275,324`；`src/ceres/model/git.rs:13,155,239,316,343,356,402,577`；`src/ceres/model/tag.rs:19,37,60,82`（2026-10-01 核对） |
| 外部参照：mega2 语义 | create-entry 遇到重名目录今日返回 500（`err_message:"Internal server error"`）；delete/move 按 mode 匹配：`is_directory=true`（Libra 省略该键即为此值）遇到文件目标回 400，缺失目标回 404，目标名已存在或源与目标相同回 400；trunk 产品写不能落在 `/`，写根须为 `root_dirs` 中的一级根（默认含 `project`） | mega2@`8ff880c`：`docs/refactoring/directory-entry-api.md:118,153,192-224`（重名 500 在 `:196`）；`src/ceres/api_service/mono_api_service.rs:1438-1490`；`config/config.toml:87` |
| 外部参照：mega2 tag 列表分页 | annotated tag 从数据库按名称升序分页；lightweight tag（`refs/tags/*`）只用于补满当前页，且每页都从 ref 列表开头取，因此在 lightweight tag 较多的实例上，新建的 lightweight tag 可能在任何一页都不出现，也可能在多页重复；`total` 为两者之和 | mega2@`8ff880c`：`src/jupiter/storage/mono_storage.rs:1960-1975`；`src/ceres/api_service/mono_api_service.rs:1906-1965` |
| 外部参照：mega2 鉴权 | `GET /tree`、`GET /tags/list`、`GET /tags/{name}` 不需要 Authorization（MN-07 的 tag 门据此以匿名 `GET /tags/{name}` 观察结果）；写路由走 `push_auth`；root tag 的创建与删除需要能覆盖 `/` 的 token，或 `push_auth=none` | mega2@`8ff880c`：`docs/refactoring/directory-entry-api.md:363-416`；`config/config-storage-only.none.toml` |
| 外部参照：mega2 黑盒工具分工 | mega2 的「禁止以 libra 作客户端」规定只约束它自己的 Git 协议 smoke 与 API 写 smoke（curl + git 脚本），不管 Libra 仓内的测试。mega2 plan-20261001（2026-10-01 新建、仍在编辑的未提交草稿）设 Libra 域：libra 只允许出现在 `scripts/libra_smoke_storage_only.sh`（GC-BB-01、ADR-BB-03）；browser 写操作用例以 `DEFER-BB-03` 等待本计划，经 `DEP-BB-04` 在其 Phase 3 末尾追加 `BB-65` 起的卡；其 `interop-smoke` 镜像从 libra.tools 安装最新 stable，并以环回中继 `127.0.0.1:9000` 访问 mega2 | 使用者 2026-10-01 转达的 mega2 侧建议；mega2 `docs/plan/plan-20261001.md`（未提交草稿，按 ID 引用；2026-10-01 13:46:48 UTC 核对时的行号：Libra 域 `:14,41`，写操作暂缓说明 `:49`，ADR-BB-03 `:201`，token 决策 `:244`，GC-BB-01 `:281`，`BB-65` 起编号预留 `:458`，DEP-BB-04 `:467`，DEFER-BB-03 `:3937`；草稿行号会变，开工时按 ID 重新定位）；mega2@`8ff880c` `docs/plan/plan-20260906.md:210-227`、`plan-20260917.md:284`、`plan-20260918.md:197`、`plan-20260904.md:143`、`docs/deploy-trunk.md:137` |
| 网站仓库 | `../libra-backend` 是 Git 仓库，分支 `cf`（跟踪 `origin/cf`，2026-10-01 无超前/落后），工作区有未跟踪的 `.teamx/`；门禁为 `pnpm typecheck`、`pnpm build` 与 `pnpm preview:cf`（确认页面加载）；后端 GitHub Actions 只覆盖 `main`/`develop` | `git -C ../libra-backend status --short --branch`；`../libra-backend/AGENTS.md:21,58-62`；`../libra-backend/package.json:12-15`；`../libra-backend/.github/workflows/ci.yml` |
| 测试门控范式 | L2/L3 用 `env_is_present` 加 `skipped` 早退；`LIBRA_TEST_MEGA_SERVER` 是仅 env 门控（无 Cargo feature）的先例 | `tests/cloud_storage_backup_test.rs:52`；`CLAUDE.md:265-270` |
| 发布下载地址 | 安装器从 `${BASE_URL}/${VERSION}/libra-${OS}-${ARCH}` 下载，`BASE_URL` 默认 `https://download.libra.tools/libra/releases` | `install.sh:14,1150-1157` |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-MN-01 | 建/删/移/改名目录与 tag 列/建/删只能在 TTY TUI 中执行；`--json` 只能 GET | 脚本、CI、黑盒测试无法驱动这些功能 | `src/command/mega2.rs:180-196`；`docs/commands/mega2.md:113-116` | MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09 |
| GAP-MN-02 | 人读模式在非 TTY 直接被拒绝，无法非交互地人读列表 | CI 日志与操作者只能改用 JSON | `src/command/mega2_browser/mod.rs:563-613`；`tests/command/mega2_browser_cli_test.rs:268` | MN-02 |
| GAP-MN-03 | 失败只有 stable code 与 message，多个 HTTP 状态共用一个 code | 黑盒只能解析非契约的 message 文本才能区分 404/409/500 | `mega2_mutate.rs:249-266`；`mega2_tree.rs:328-348`；`mega2_entry.rs:175-216` | MN-01 |
| GAP-MN-04 | 帮助与文档示例使用未 rooted 路径，照抄即失败 | 黑盒脚本作者从示例出发会直接撞错 | 见事实基线「文档示例缺陷」 | MN-02 |
| GAP-MN-05 | 没有对真实 Mega2 的端到端验证，live 门的运行方式也没有登记 | 无法证明客户端与真实服务端互通，mega2 侧缺少可直接照搬的参考用例 | `tests/command/mega2_*_test.rs` 全是 loopback mock | MN-07（黑盒调用约定由 MN-02 起各卡写入「Non-interactive operations」节） |
| GAP-MN-06 | mega2 的 Libra 域（`scripts/libra_smoke_storage_only.sh`）因 browser 写操作只在 TUI 而暂缓相应用例（`DEFER-BB-03`） | Mega2 的 compose 黑盒覆盖不到 browser 写操作 | mega2 `docs/plan/plan-20261001.md` 的写操作暂缓说明、DEP-BB-04、DEFER-BB-03（未提交草稿；2026-10-01 13:46:48 UTC 时位于 `:49`、`:467`、`:3937`） | 写操作卡交付后经 `DEP-MN-03` 通知 mega2 侧 |
| GAP-MN-07 | `--server` 无法解析时错误回显原始输入，可能带出 URL 凭据 | CI 日志泄漏凭据；与三处文档的承诺不符 | 见事实基线「URL 解析错误回显」 | MN-10 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| [`plan-20260912.md`](plan-20260912.md)（已收口） | 前序计划：交付 `mega2 browser`、TUI 写操作与 tag 面板 | 部分取代 ADR-MB-02：保留「唯一子命令 `browser`」，取代「写入/tag 只在 TUI」「`--json` 只列一层」两条。在 ADR-MB-03 基础上把写 token 扩到非交互写（ADR-MN-05）。ADR-MB-01/04/05/06 不变，MB-06/09/12 墓碑仍有效。已收口计划的正文不改，只在本计划与 `plan-status.md` 记录取代关系 |
| [`plan-long.md`](plan-long.md) | 无对应 LR/MEM 能力编号 | 只在「日期计划索引」登记本计划一行 |
| [`plan-20260923.md`](plan-20260923.md) CP-19 | 已实现能力的覆盖台账（audit-only），清单含 mega2 | `DEP-MN-05`（outgoing 信息移交）；无写集交集 |
| [`plan-20260916.md`](plan-20260916.md) | agent capture-push；GC-CAP-02 禁止在 capture 路径读取 `LIBRA_MEGA2_TOKEN` | 本计划不改该环境变量名，也不改 `mega2_auth` 的解析规则，对其无影响 |
| `DEP-CLI-mirror`（`plan-status.md` 跨计划依赖；原镜像行 `DEP-QP-01` 已随 issues/574 收口删除） | `src/cli.rs` 三态串行名单 | 本计划不改 `src/cli.rs` 的代码（GC-MN-01）；只有 MN-03 改写其中一处注释，经 `DEP-MN-06` 进入该名单 |
| mega2 `docs/plan/plan-20261001.md`（未提交草稿） | 接收方：Libra 域 `scripts/libra_smoke_storage_only.sh`；`DEP-BB-04`/`DEFER-BB-03` 等待本计划；`BB-65` 起编号已预留 | `DEP-MN-03`（outgoing）：本计划交付能力、契约文档与 live 门，并转告已观察到的 lightweight tag 分页行为；mega2 侧在其 Libra 域追加用例；本计划不改 `../mega2/**` |
| mega2 `docs/plan/plan-20260906.md`、`plan-20260917.md`、`plan-20260918.md`、`plan-20260904.md` | 规定 Git 协议 smoke 与 API 写 smoke（curl + git 脚本）不得以 libra 作客户端 | 本计划的测试都在 Libra 仓内，不受其约束；也不向这些脚本加入 libra 用例（ADR-MN-07） |
| mega2 `docs/refactoring/directory-entry-api.md` | 目录变更与 tag 的产品 HTTP 契约页（含「Libra pin」节） | 只读消费，经 `DEP-MN-01` pin |

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | PASS：非交互操作有明确的消费者（Mega2 黑盒；mega2 plan-20261001 的 Libra 域已为其预留 `BB-65` 起编号），并关闭 GAP-MN-01..07 | 经 `DEP-MN-03` 交付，不在本仓越权修改 mega2 |
| 可行性 | PASS：四个协议客户端已实现并有测试，工作集中在参数、调度与输出；不需要服务端改动 | 每张操作卡开工前按 GC-MN-08 重核 pin |
| 任务卡粒度 | PASS（经登记豁免）：12 张卡各自只有一个行为轴，每个公开操作独占一张卡；AC 按独立谓词如实计数。「操作 × 通用规则」的机械门族（含 MN-06 的 `--message` fixture 门）与 MN-01 的诊断门族、MN-07 的 live 门族超过 8 条，各卡门族以外的 AC 不超过 8 条，按模板白名单登记 G-03 门族型豁免 EX-MN-01/02/03（使用者批准），每个门都是卡内「判据规范」中的具名 `--exact` 命令 | 门族增减时同批更新豁免行与审计表 |
| 依赖与顺序 | PASS：DAG 无环；十二张卡因实现写集两两相交而全串行 | 发布窗口按实施顺序 |
| 完整性 | PASS：每张改变用户可见行为的卡都含实现、测试、EN/zh/网站文档与兼容行；黑盒调用约定在 MN-02 起的「Non-interactive operations」节；live 互通证明集中在 MN-07 | 无 |
| 安全性 | PASS：读匿名；写按 ADR-MB-03 取 token；不回显 token、URL 凭据与响应体（MN-10 修复解析错误回显）；不读 stdin、不提示。删除不要求额外确认，这是有意决定（ADR-MN-02），由「flag 携带确切目标」与服务端按 mode 拒绝来兜底 | R9c 的零泄漏门 |
| 功能正确性 | PASS：一次调用一次请求；`target` 与 `receipt` 分离；不 preflight，以服务端回答为准 | 用 mock 请求记录断言 |
| 接口兼容 | PASS：列表 payload 只新增字段；TUI 不变；token 拒绝消息在 MN-11 之前保持原文，MN-11 连同唯一受影响的用例一起改写；flag 名避开帮助守卫子串 | GC-MN-05、GC-MN-06 |
| 数据流与控制流 | PASS：不读写本地仓库状态；写请求不重试；超时后的「结果未知」有文档处理 | 故障恢复矩阵 |
| 性能与容量 | PASS：每次调用 O(1) 个请求，沿用 10 s 超时、1 MiB 响应、2000 项与 per_page ≤ 100 的上限 | 性能与容量摘要 |
| 可靠性与容错 | PASS：本地校验在网络之前；失败带诊断；live 门按 run id 自给自足地准备与清理，并有容量前置检查 | 故障恢复矩阵、MN-07 |
| 可维护性 | PASS：非交互执行集中在一个模块，由唯一的操作登记表分派；诊断键由一个共享 helper 产生 | GC-02、ADR-MN-08 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-10-01 11:40:42 UTC | 初稿（使用者要求为 browser 全部功能增加非交互操作，以支持 Mega2 黑盒测试） | 依模板 v2.12 建立 7 卡计划；pin mega2@`7b2a435`；登记 ADR-MN-01..07、GC-MN-01..10、ER-MN-01..03、DEP-MN-01..05、DEFER-MN-01..09 | N/A → MN-01..MN-07 | 全文；`plan-status.md`；`plan-long.md` 日期计划索引 |
| 2026-10-01 12:04:09 UTC | 自审（Codex R1 前）；mega2 侧建议（使用者转达）；上游前移 | MN-04 拆出 MN-08，MN-06 拆出 MN-09；按 mega2 侧建议改写 GAP-MN-06、DEP-MN-03、ADR-MN-07、非目标、事实基线与风险；checkout 快进到 `9c1601b`，mega2 pin 前推到 `8ff880c`；修正 `src/utils/error.rs` 与 compat ledger 锚点 | MN-04 → MN-04、MN-08；MN-06 → MN-06、MN-09 | 全文；`plan-status.md`；`plan-long.md` |
| 2026-10-01 12:31:50 UTC | Codex R1 `FAIL`（9×P1） | 新增 MN-10（URL 解析错误不回显）；MN-03 拆出 MN-11（写操作凭据），MN-08 拆出 MN-12（改名）；新增 ADR-MN-08 操作登记表与类别规则；重名改按 500 验收；R10 覆盖全部 tag 操作；MN-01 写集加 `docs/error-codes.md`；live tag 容量上限；`DEP-MN-04` 降级同改依赖边；移动与改名改为双值参数；非交互 `--message` 必须非空；记录 mega2 lightweight tag 分页行为 | MN-03 → MN-03、MN-11；MN-08 → MN-08、MN-12；新增 MN-10 | 全文；`plan-status.md`；`plan-long.md` |
| 2026-10-01 13:52:16 UTC | Codex R2 `FAIL`（4×P1、3×P2）；使用者于 2026-10-01 13:18:58 UTC 批准 G-03 门族型豁免 | ① 登记 EX-MN-01（MN-01 诊断门族）、EX-MN-02（各操作卡的「操作 × 规则」门族，以及 MN-06 `--message` 校验器的 fixture 门）、EX-MN-03（MN-07 live 门族），每卡加「判据规范（非计数正文）」逐门列出 `--exact` 命令，分子如实写作 `n/8@EX`；ADR-MN-08 规则拆为原子规则 R1–R3、R4a–R4b、R5a–R5b、R6–R8、R9a–R9c、R10a–R10b，门命名为 `op_rule_<op>_<rule>`；GC-MN-02 定义「请求记录」「本地拒绝」「服务端失败」三个判定术语，全部卡的非门族 AC 改写为单一判据并按此重新计数（各卡门族以外的 AC 不超过 8 条）；② MN-07 由 `release` 改为 `implementation`，改成 9 个自给自足的 live 门（8 个操作各一个，`--create-tag` 分 lightweight 与 annotated 两个）与 5 个 mock 驱动的 harness 门；③ 每张非全量卡写明 C 组第 ④ 步的 nextest 命令；④ `DEP-MN-04` 的 7 天时钟改从 MN-09 完成起算；⑤ D 组网站核对改为 `curl` 部署页并按本卡标记 `rg`，`DEP-MN-02` 加 `pnpm preview:cf`；⑥ mega2 草稿改为按 ID 引用，并附 2026-10-01 13:46:48 UTC 的行号；⑦ M4 只声明 `DEP-MN-03` 的稳定操作前提，交付归 M5；⑧ 容量前置改为 root tag ≤ 900 | MN-07 类型 `release` → `implementation`（卡号不变） | 状态行、成功定义、事实基线、评审结论、ADR-MN-08、GC-MN-02、GC-MN-10/11、ER-MN-01/02、依赖登记表、豁免表、审计表、全部任务卡、测试矩阵、追溯表、里程碑、风险表、性能表、完成判据、证据汇总；`plan-status.md` |
| 2026-10-01 14:24:18 UTC | Codex R3 `FAIL`（4×P1、1×P2） | ① 计数：MN-01 改为单一发送包装 `mega2_diag::run` 的结构约束（ADR-MN-04），门族改为 4 个包装分支门、2 个零命中结构门与 8 个「方法 + 路由」接线门，每门一个 fixture；ADR-MN-08 规则表加「调用模式」列，每个规则门只用一种模式调用一次；MN-07 改为 11 个单判据 live 门（`live_gate_*`）与 6 个单判据 harness 门，用户文档与网站改动移出 MN-07（黑盒调用约定由 MN-02 起各卡承载），`CLAUDE.md` 与集成测试指南各登记一行逐字文本；GC-MN-02 增「成功 envelope 匹配」术语；② libtest 多过滤器：申辩并在事实基线登记 Rust 标准库源码与实测证据，命令不改；③ 恢复模式全部改为 `immutable-release`，在「字段全局默认」写明降级指引与兼容窗口，网站卡附 `cf` 补偿提交；④ D 组网站核对改为 fail-closed 的 bash 检查（`curl` 失败、非 200、标记计数为 0 都判失败）；⑤ live 门过滤器改为 `live_gate_`，不再选中 harness 门 | 无（MN-07 标题与范围收窄，卡号不变） | ADR-MN-04、ADR-MN-07、ADR-MN-08、GC-MN-02、ER-MN-02、事实基线、GAP-MN-05、依赖边（删除 `DEP-MN-02 -> MN-07`）、依赖登记表、字段全局默认、豁免表、审计表、MN-01、MN-02、MN-07 与全部 `Rollback mode`、测试矩阵、追溯表、里程碑、性能表、兼容与文档收口、完成判据；`plan-status.md` |
| 2026-10-01 15:07:51 UTC | Codex R4 `FAIL`（3×P1、1×P2）；R3 的 libtest 申辩被接受 | ① MN-01 增 8 个末段门（每个「方法 + 路由」在 2xx 响应上的最后一道校验，如 delete/move 的 `require_commit_id`），与起点门（500）一起证明闭包覆盖从收到响应到返回结果的全部校验；ADR-MN-04 写明闭包须包含最后一道校验；② MN-01 AC-1 不再把 message、hint、退出码的不变归功于既有用例，改由结构门 G23（忽略空白的差异中无错误构造行增删）与 G24（`src/utils/error.rs` 无改动）证明，门族 14 → 24；③ MN-11 以真实二进制增验仅环境变量、仅 flag、人读模式 token 文件与其 401 不回显，守卫用例改写收敛为一条（只删「TUI-only」断言），AC 7 → 8；ADR-MN-05 写明来源矩阵只在 MN-11 验证一次；④ GC-MN-07 的理由改为保守选择：ledger 引用最大到 180 行，真正受影响的是 `grit-suite-scope.md:173` 对 259–261 行的引用 | 无 | ADR-MN-04、ADR-MN-05、GC-MN-07、事实基线、EX-MN-01、审计表、MN-01、MN-11、追溯表、风险表；`plan-status.md` |
| 2026-10-01 15:31:08 UTC | Codex R5 `FAIL`（4×P1） | ① MN-01 增 G25、G26：`run` 返回的错误在 `details` 之外与原错误相同（message、hints、stable code、usage、退出码五元组相等），证明包装不重建 `CliError`；门族 24 → 26；② MN-11 的 AC-8 拆为三条独立判据（新拒绝消息、测试文件差异恰为三处限定替换、改写后通过），凭据来源矩阵改为 fixture 门；MN-11 按 EX-MN-02 补偿措施④并入该门族（R9 规则门与凭据 fixture 门属同一门族），计数 10/8@EX-MN-02；③ MN-07 的 G12 按缺失变量拆为 G12、G13，harness 门 6 → 7，门族 17 → 18；④ 评审记录方式改为只在本计划内摘要，不新建、不保留评审输出文件，各轮 Evidence 补入输入计划文件的 SHA-256 | 无 | ADR-MN-08、EX-MN-01、EX-MN-02、EX-MN-03、审计表、MN-01、MN-11、MN-07、追溯表、Codex review log；`plan-status.md` |
| 2026-10-01 15:54:55 UTC | Codex R6 `FAIL`（4×P1） | ① 「本地拒绝」「服务端失败」不再作为单一判据：GC-MN-02 把二者定义为两门（`_code` 与 `_no_request`／`_http_status`）；ADR-MN-08 的本地拒绝类规则（R5a、R5b、R7、R8、R10a、R10b）各展开两门；各操作卡的本地拒绝与服务端失败 AC 移入 EX-MN-02 门族并拆成两门，门族以外的 AC 每卡 3–7 条；② MN-01 G23 改为保留字符串内部空白的逐行比较（只去掉行首空白，顺序不变，不再使用 `libra diff -w`）；③ G25、G26 改为整个 `CliError` 的相等比较（`PartialEq`，含 `kind` 等全部字段），期望值为原错误加上同样的 `details` 键；④ MN-07 live 门改为读取操作者导出的 `DEP-MN-04` 核对值，并以 `: "${…:?}"` 在缺少必填变量时失败，A 与 C 使用同一组命令；live 门的判据改为单字段判定，G17 改为包含错误信封原文的单一子串判定 | 无 | GC-MN-02、ADR-MN-08、任务卡粒度规则、EX-MN-02、审计表、MN-01、MN-02、MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09、MN-07、测试矩阵、追溯表；`plan-status.md` |
| 2026-10-01 16:38:56 UTC | Codex R7 `FAIL`（4×P1） | ① MN-01 的 G23 改为结构检查：与 `HEAD` 相比（只去行首空白），除 `.send()` 行外每一行都原样按序保留，新增行不得构造或转换错误（`CliError`、`with_exit_code`、`map_err`、`Err(`、字符串字面量等），从而覆盖返回表达式；ADR-MN-04 改为作用域设计（方法体整体放进 `mega2_diag::run` 的作用域，发送经作用域的 `send`），使既有行可以不改写、不重排；增 G27（发送之前返回的错误原样返回），门族 26 → 27；② MN-07 明确目录门以写根为 PATH、tag 门不带 PATH（`/`）；③ MN-11 的 AC-2 改为把 `HEAD` 版本施加三处替换后与工作区逐字比较的命令，A 组与 C 组第 ④ 步写出同一命令；④ MN-07 的两条逐字行检查改为断言计数恰为 1、`rg` 出错即失败，C 组第 ④ 步写出完整命令 | 无 | ADR-MN-04、EX-MN-01、审计表、MN-01、MN-11、MN-07；`plan-status.md` |
| 2026-10-01 16:58:49 UTC | Codex R8 `FAIL`（4×P1） | ① 增 G25：每处被替换的 `.send()` 都由同时含 `send(` 与 `transport_error` 的新行替代，确保客户端的传输错误映射仍交给作用域；② MN-11 的 AC-2 命令先断言 `HEAD` 第 6、7、312、329 行是预期原文，再把四行整行替换后与工作区文件用 `cmp` 逐字节比较（不再经命令替换去掉末尾换行），Verification 与 C 组第 ④ 步共用；③ 原 G23 的两项检查拆为 G23（既有行按序保留）与 G24（新增行不构造或转换错误），连同新增的 G25，MN-01 门族 27 → 29、Verification 8 → 10，EX-MN-01 写明同时豁免 AC 列与 Verification 列；④ 新增规则 R1b：stdin 为保持打开、无输入的管道时，调用须在 5 秒内结束，适用于全部 8 个操作，各操作卡门族加 1，GC-MN-03、GC-MN-10 同步 | 无 | ADR-MN-08、GC-MN-03、GC-MN-10、EX-MN-01、EX-MN-02、审计表、MN-01、MN-02、MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09；`plan-status.md` |
| 2026-10-01 17:13:51 UTC | Codex R9 `FAIL`（3×P1） | ① MN-10 的恢复模式改为 `forward-only`（revert 会重新暴露凭据回显），写明不变量、恢复验证命令与用户影响，并登记为默认覆盖；② MN-01 增 8 个中段门 G30–G37：每个「方法 + 路由」组合上，200 响应头已到、读取响应体中断时 `details` 带 `http_status:200`，与起点门、终点门一起覆盖收到响应之后的各段；门族 29 → 37；③ MN-07 的写操作 live 门改以写后观察到的结果状态为判据：目录门以随后的 `--list` 观察（移动与改名各分「到达」「离开」两门），tag 门以 harness 直接发出的匿名 `GET /api/v1/tags/{name}` 读取服务端回报的字段（lightweight：`tag_id` 等于 `object_id`；annotated：`message` 等于给定文本；删除后 404）；live 门 11 → 12，门族 18 → 19 | 无 | MN-10、默认覆盖表、MN-01、EX-MN-01、MN-07、EX-MN-03、审计表、DEP-MN-04、DEFER-MN-09、成功定义、完成判据、测试矩阵、性能表；`plan-status.md` |
| 2026-10-01 17:23:13 UTC | Codex R10 `FAIL`（1×P1、1×P2） | ① 里程碑 M1 的回滚改为分卡写明：MN-01 发布 revert patch，MN-10 只发布保持「URL 解析错误不回显原始输入」不变量的前滚修复（forward-only），不 revert；② ADR-MN-07 与风险表中「以删除回执证明 lightweight tag 存在」改为 MN-07 G9、G11 实际使用的匿名 `GET /api/v1/tags/{name}` 判定 | 无 | 里程碑表、ADR-MN-07、风险表；`plan-status.md` |
| 2026-10-01 17:48:22 UTC | Codex R11 `FAIL`（1×P1、2×P2） | ① ADR-MN-04 写明作用域以 tokio `task_local!`（或等价、不改函数签名的任务上下文）实现，`mega2_diag::send` 从当前作用域取得方法与路由并记录实际状态码，`post_json`、`finish` 的签名与调用行不变，delete/move 在 `post_json` 之后的 `require_commit_id` 仍在作用域内；终点门 G15–G22 改用 201 fixture，证明附加的是实际状态码而非写死的 200；② 源码注释：MN-02 改写 `src/command/mega2.rs:10-15` 与 `:181`，MN-03 改写 `src/command/mega2.rs:3` 与 `src/cli.rs:2006-2009`（只改注释）；GC-MN-01 改为「不改 `src/cli.rs` 的代码」，新增 `DEP-MN-06`（`src/cli.rs` 三态串行）与依赖边 `DEP-MN-06 -> MN-03`，MN-03 的落点与生产文件计为 3；③ 事实基线「clap 用法错误」的锚点改为 `src/cli.rs:2939-2943` | 无 | ADR-MN-01、ADR-MN-04、GC-MN-01、事实基线、与其它计划的关系、并发声明、实施顺序、依赖登记表、MN-01、MN-02、MN-03、审计表；`plan-status.md`（含 `DEP-AD-12 / DEP-CLI-mirror` 行） |
| 2026-10-01 18:06:47 UTC | Codex R12 `FAIL`（1×P1、3×P2） | ① MN-03 的 `src/cli.rs` 只改注释、不承载行为，按 G-04 不计入行为落点与生产文件（仍列在写集中），计数回到 2/2；② `src/command/mega2.rs:156-160`（`execute_safe` 的 Side Effects）分别由 MN-02（人读 `--list`）、MN-03（写请求）、MN-11（凭据）同步改写；③ `plan-status.md` 的 `DEP-QP-01` 镜像行同样登记 MN-03；④ 非目标改为「不改 `src/cli.rs` 的代码，唯一例外是 MN-03 的一处注释」；另经全量排查，把 `src/command/mega2_browser/mod.rs:1-4` 的模块注释分配给 MN-02，把 PATH 参数帮助文字（`src/command/mega2.rs:86`）分配给 MN-03（写操作的父目录）与 MN-05（tag 操作只接受 `/`） | 无 | 非目标、MN-02、MN-03、MN-11、MN-05、审计表；`plan-status.md` |
| 2026-10-01 18:35:16 UTC | Codex R13 `FAIL`（2×P1） | ① MN-07 AC-2 要求写入集成测试指南的命令行改为不含占位符、可直接运行的 `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test …`（保留必填变量检查，指南先说明导出 `DEP-MN-04` 核对值），两处逐字行计数命令同步；② MN-02 增 AC-8 与单元测试 `site_example_paths_are_rooted`：以 `LIBRA_SITE_MEGA2_DOC` 指向网站页，用真实的 `Cli::try_parse_from` 与 `normalize_path` 校验网站页每条 `mega2 browser` 示例（取代只看 `--server URL` 后一个记号的正则守卫）；此后每张写网站的卡在 A 组与 C 组第 ④ 步的 `command::mega2` 单元测试命令都设置该变量（GC-MN-09） | 无 | GC-MN-09、MN-02、MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09、MN-07、审计表、测试矩阵；`plan-status.md` |
| 2026-10-01 18:51:16 UTC | Codex R14 `FAIL`（1×P1） | `site_example_paths_are_rooted` 改为 `#[ignore]` 且没有跳过分支（变量未设置即失败）；所有门命令显式包含被忽略的测试：MN-02 的 A 组用 `-- --ignored`，C 组第 ④ 步在全量 nextest 之外另跑 `--run-ignored only` 的网站示例检查；后续写网站的卡用 `-- --include-ignored`／`--run-ignored all`；计划完成门同样另跑该检查；证据记录它的 PASS 行（GC-MN-09） | 无 | GC-MN-09、MN-02、MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09、完成判据；`plan-status.md` |
| 2026-10-01 19:09:32 UTC | Codex R15 `PASS`（0×P0、0×P1、1×P2） | 修复 R15 的 P2：测试矩阵「单元」行不再把普通的 `cargo test --lib command::mega2` 当作网站示例检查，改为列出显式运行被忽略测试的命令；追溯表 MN-02 行补上 `site_example_paths_are_rooted`。只改汇总表，不改任何任务卡、门或规则 | 无 | 测试矩阵、追溯表 |
| 2026-10-01 19:13:30 UTC | Codex R16 `PASS`（差异确认；0×P0、0×P1、1×P2） | 计划级 Codex review 取得 `PASS`（R15 全文、R16 差异确认）；按 R16 的 P2 把 R15 日志行中的两处缺陷分开描述；状态行与 `plan-status.md` 改为「计划级 review 已 PASS，可按 ER-MN-03 开工」。只改日志与状态，不改任何任务卡、门或规则 | 无 | 状态行、Codex review log；`plan-status.md` |
| 2026-10-02 00:52:28 UTC | 提交前上游前移（v0.30.11–v0.30.13，`main@7f810da`） | 按 ER-02 复核本计划引用、且被上游改动的文件：`docs/development/commands/_compatibility.md` 的 mega2 行 `:55` → `:57`（12 处 `:55` 锚点；9 张卡写集中的「只改第 55 行」当时漏改，由下一行 R17 补上），`docs/development/integration/integration-test-plan.md` 的 Wave 3 标题 `:245` → `:247`，版本事实改为 `0.30.13`／`v0.30.13`；`plan-status.md` 已随 issues/574 收口删除 `DEP-QP-01` 行，故「与其它计划的关系」、`DEP-MN-06` 与并发声明只保留 `DEP-AD-12 / DEP-CLI-mirror`；完成判据中的计划基线改为 `7f810da`。不改任何任务卡的门、规则或计数 | 无 | 事实基线、与其它计划的关系、依赖登记表、并发声明、各卡与兼容收口中引用 `_compatibility.md` 的行、MN-07 的现有证据、完成判据；`plan-status.md` |
| 2026-10-02 01:00:30 UTC | Codex R17 `FAIL`（1×P1） | MN-02、MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09 九张卡的写集仍写「`docs/development/commands/_compatibility.md`（只改第 55 行）」，而 `7f810da` 上第 55 行是 `log` 行 → 九处都改为第 57 行。至此 `_compatibility.md` 的 mega2 行引用共 21 处（12 处 `:57` 锚点与 9 处「只改第 57 行」）全部指向第 57 行。不改任何门、规则或计数 | 无 | 上述九张卡的写集；Codex review log |
| 2026-10-02 01:04:02 UTC | Codex R18 `PASS`（差异确认；0×P0、0×P1、无 P2） | 计划级 Codex review 在上游前移后重新取得 `PASS`（R17 的 P1 已关闭）；只改状态行、Codex review log 与 `plan-status.md` 的评审结论，不改任何任务卡、门或规则 | 无 | 状态行、Codex review log；`plan-status.md` |
| 2026-10-02 01:08:49 UTC | 开工（使用者以 `/goal` 指示执行本计划直至全部任务卡完成） | 发布者登记（ER-12）：本计划的单一发布者为 Claude Code 会话 `eb4841ea-53f7-4e15-8220-e2252ec39358`，负责全部卡的实现、评审、C 组（bump、构建、安装、提交、feature branch、PR、squash merge、`gh release create`）与 D 组跟踪，未经修订不移交。MN-10 按发布窗口顺序首先进入 `in-progress`，分支 `mn-10-url-parse-no-echo`；开工核对：`libra status --short --branch` 为 `## main...origin/main`、工作区干净，HEAD `f28dafc` | 无 | MN-10、实施证据汇总；`plan-status.md` |
| 2026-10-02 07:55:47 UTC | MN-11 实施中发现 AC-2 与 C 组 fmt 门冲突 | AC-2 规定的第 329 行替换文本 `assert!(err.contains("read operations take no credentials"), …);` 超过 rustfmt 的调用宽度，`cargo +nightly fmt --all --check` 会要求把它拆成多行，逐字替换与强制的 fmt 门无法同时满足。改为同样只占一行、能通过 rustfmt 的 `    assert!(err.contains("no credentials"), "unexpected stderr: {err}");`（拒绝消息的完整文本仍由 AC-1 的 `list_token_refusal_message` 逐字判定）；同步 MN-11 的 AC-2 与 Verification、C 组第 ④ 步两处差异判定命令。只改这一行的期望文本，不改门数、计数或其它判据 | 无 | MN-11 |
| 2026-10-03 01:09:45 UTC | 上游前移且占用目标版本：会话暂停期间 issues/498 TT-05（PR #602，`a2f1c34`）合入 `main` 并发布 v0.30.22，MN-05 的提交 `a4d05cb`（同为 0.30.22）与 PR #603 因此冲突 | 按 ER-MN-01「目标版本已被占用时顺延、不得复用」与长期有效的重建指示：`libra reset --mixed a2f1c34`，把上游改动的 15 个文件（含 `COMPATIBILITY.md`、`CLAUDE.md`、`plan-status.md` 与版本面）按 `a2f1c34` 恢复，`COMPATIBILITY.md` 只在原第 248 行重放本卡的 mega2 行（上游改的是第 204 行的 tag 行，行数不变），`plan-status.md` 取上游全文后只重放本计划的行（「当前快照」由本计划改写，并注明其余沿用上游 2026-10-02 16:40:07 UTC 快照），版本面改为下一个空闲 patch 0.30.23；本卡写集中其余 8 个文件（文档 5 个、源码 2 个、测试 1 个）逐字节等于 `a4d05cb`；重跑 C 组后重新提交，以 `--force-with-lease` 更新 PR #603 的分支；MN-05 的版本由 0.30.22 改为 0.30.23 | 无 | MN-05、实施证据汇总、状态行；`plan-status.md` |
| 2026-10-03 02:22:41 UTC | 上游再次前移并占用目标版本：issues/498 TT-02（PR #604，`52bf7d2`）于 2026-10-03 01:47:42 UTC 合入 `main` 并发布 v0.30.23，MN-05 的重建提交 `98f1e84`（0.30.23）与 PR #603 再次冲突（PR #603 在 `98f1e84` 上 11/12 检查已通过，offline-core 仍在运行） | 沿用 2026-10-03 01:09:45 UTC 一行的重建方法：`libra reset --mixed 52bf7d2`，把上游改动的 13 个文件按 `52bf7d2` 恢复；`COMPATIBILITY.md` 只在第 248 行重放本卡的 mega2 行（上游改的是第 197、204、215 行，行数不变）；`plan-status.md` 取上游全文后只重放本计划的行（「当前快照」由本计划改写，并注明其余沿用上游 2026-10-02 21:24:02 UTC 快照）；版本面改为下一个空闲 patch 0.30.24；本卡写集中其余 8 个文件逐字节等于 `98f1e84`（即 `a4d05cb`）；重跑 C 组后重新提交，以 `--force-with-lease` 更新 PR #603 的分支；MN-05 的版本由 0.30.23 改为 0.30.24 | 无 | MN-05、实施证据汇总、状态行；`plan-status.md` |
| 2026-10-03 10:57:41 UTC | goal 继续执行，发布者移交（ER-12） | 单一发布者由 Claude Code 会话 `eb4841ea-53f7-4e15-8220-e2252ec39358` 移交到当前 Codex goal 线程 `01a1015b-5b27-7c11-9354-fb91b085d9c2`；MN-09 的既有 await-merge 进程已正常退出（rc=0），PR #606 与 v0.30.26 远端门已现场核实；保留 MN-07 四个未提交路径，在 `bb9fbc5` 上恢复上游 issues/478、issues/498 及 plan-status 的并发文档收口，`libra pull origin main --ff-only` 返回 Already up to date；不改卡的范围或判据 | 无 | 状态、实施证据汇总、`plan-status.md` |
| 2026-10-03 13:05:47 UTC | MN-07 发布/D 组完成与跨仓交付登记 | 12 卡全部 `done`/`complete`；v0.30.27 stable、12/12 pinned-source live 与清理已核实；`DEP-MN-03` 登记已交付。后续收口只同步计划、状态和日期索引三个文档路径，继承 MN-07 发布点，不新增实现卡或 patch；全量门和最终审查仍待执行 | 无 | 本计划、`plan-status.md`、`plan-long.md` |
| 2026-10-03 13:38:39 UTC | 全量收口门完成 | 原定全量 nextest 8839/8839、网站示例 1/1、fmt / Clippy 全绿，无失败/重试/泄漏；恢复验证限制如实登记，未将未演练的生产恢复发布记为通过。12 卡判据与 D 组、跨仓交付和日期索引已同步；最后仅执行只读 Codex 审查与三个计划文档路径的签名提交/PR 交付，不新增产品能力或 artifact | 无 | 本计划、`plan-status.md`、`plan-long.md` |
| 2026-10-03 13:50:01 UTC | Codex R19 最终收口审查 `PASS` | P0/P1 无；P2 状态词修复为既有 `已收口`；14 条完成判据全部勾选，12 卡状态与计划索引同步；本次行政交付仅包含三个计划文档，继承 MN-07 发布，不再 bump / tag / artifact | 无 | 本计划、`plan-status.md`、`plan-long.md` |
| 2026-10-03 15:45:47 UTC | 行政收口 PR #609 交付期间上游前移 | 原 head `6531b0f` 的 12/12 checks 全绿；合并前发现 `origin/main` 前移到 `721fec9`（Agent Capture 基础交接），暂停合入并整合。状态表以 incoming 为底，只更新本计划的三行与快照中的 Mega2 片段，保留 ACF 交接、security/privacy RFC 边界及其他计划登记；日期索引保留双方各自的行。Mega2 实现、live 测试与双语命令文档共 10 路径相对 MN-07 发布点逐字节一致；既有本地全量/live 证据仍绑定 `faae20c`，整合后的收口 head 必须重新通过 CI，不能以旧 head 的 checks 替代 | 无 | 本计划、`plan-status.md`、`plan-long.md`；PR #609 |

| 2026-10-04 00:43:05 UTC | PR #609 合入 Capture 基础后的 CI 失败跟踪 | 保留原计划在实现基线 `faae20c` 上的 8839/8839 全量、网站 1/1、fmt/Clippy 通过事实；另记 PR head `ca5dc6d` / base `721fec9` 的结果：10/12 项检查成功，完整 nextest 9519/9523，4 个测试失败；阻塞检查为 `opencode-export-linux` 与 `compat-offline-core`。合入 main `892f37c` 的文档更新（本地合并提交 `e27561b`）后，待当前文档 head 的 CI 重新运行。在计划正文记录四个失败、诊断边界和修复要求；没有更改 Capture 源码或冻结 oracle，也未把旧 head 结果计作新 head 通过 | 无 | 状态、实施证据汇总、本节；`plan-status.md`；`plan-long.md`；PR #609 |
| 2026-10-04 01:03:53 UTC | 修订 Codex R21 文档发现 | 状态快照分别列出 compat-offline-core 的四个失败测试（其中两个涉及 bwrap）与 opencode-export-linux 集成门；拆分 R19/R20 评审范围，恢复计划链接；同步主计划、状态表与长期索引。未修改 Capture 源码、workflow 或 oracle；未运行测试 | 无 | 主计划、plan-status.md、plan-long.md；R21 |
| 2026-10-04 01:16:52 UTC | Codex R22 文档差异审查 PASS | R22 复核确认 R21 的 P1/P2 文档问题已关闭；旧 CI 证据仍绑定原 head，没有把新文档 head 写成通过。R21 FAIL 保留在评审历史；不改 Capture 源码或 workflow，未跑测试 | 无 | 本计划、plan-status.md、plan-long.md

## 已决议设计决策

实现时若需偏离本节，必须先修改计划并说明原因，不得在代码中静默改语义。

### ADR-MN-01: 非交互操作是同一 `mega2 browser` 上的互斥操作 flag

- **Status:** Accepted（2026-10-01；部分取代 plan-20260912 ADR-MB-02）
- **Context:** plan-20260912 ADR-MB-02 依 2026-09-16 使用者纠正，规定唯一公开子命令是 `mega2 browser`，写入与 tag 只在 TUI 中，`--json` 只列一层。2026-10-01 使用者要求为 browser 的所有功能增加非交互操作，以支持 Mega2 黑盒测试；「不新增子命令」并未撤回。
- **Decision:** 保留「`mega2` 只有 `browser`」，取代「写入/tag 只在 TUI」与「`--json` 只列一层」两条。在 `BrowserArgs` 上新增一个互斥参数组 `operation`（至多选一个），成员与 TUI 功能一一对应：

  | TUI 功能（键） | 非交互 flag | HTTP 请求 | 承接卡 |
  |---|---|---|---|
  | 列当前层、进入、返回、重载（启动、`Enter`、`Backspace`/`h`、`r`） | `--list`（PATH 即目标目录；无操作 flag 的 `--json` 与之等价） | `GET /api/v1/tree` | MN-02 |
  | 建目录（`+`） | `--create-dir <NAME>` | `POST /api/v1/create-entry` | MN-03（凭据 MN-11） |
  | 删除目录（`d` 加确认行） | `--delete-dir <NAME>` | `POST /api/v1/delete-entry` | MN-04 |
  | 移动目录（`m`） | `--move-dir <NAME> <PARENT-PATH>`（双值参数） | `POST /api/v1/move-entry` | MN-08 |
  | 改名目录（`R`） | `--rename-dir <NAME> <NEW-NAME>`（双值参数） | `POST /api/v1/move-entry`（同父） | MN-12 |
  | tag 面板与翻页（`t`、`n`、`p`） | `--list-tags [--page <N>] [--per-page <N>]` | `GET /api/v1/tags/list` | MN-05 |
  | 创建 tag（面板 `+`） | `--create-tag <NAME> [--message <TEXT>]` | `POST /api/v1/tags` | MN-06 |
  | 删除 tag（面板 `d` 加确认行） | `--delete-tag <NAME>` | `DELETE /api/v1/tags/{name}` | MN-09 |

  选择、退出、「文件无预览」是纯界面行为，没有非交互对应物。位置参数 PATH 等同于 TUI 的「当前目录」：目录操作以它为父目录，默认 `/`。没有操作 flag 时：人读模式进入 TUI（不变），`--json`/`--machine` 输出列表（不变，只新增 `operation` 字段）。移动与改名用双值参数（clap `num_args = 2`），不设伴随参数，避免「少给目标」这类误用。不定义短选项。flag 名刻意避开既有帮助守卫禁止的子串（GC-MN-06）。
- **Alternatives considered:** 新增 `mega2 mkdir/rmdir/mv/tag` 子命令（拒绝：2026-09-16 的使用者纠正仍有效，MB-06/09/12 是墓碑）；在 `browser` 下嵌套操作子命令（拒绝：与位置参数 PATH 冲突，名叫 `list` 之类的目录会被误解析）；单个 `--op <enum>` 加通用 `--name`（拒绝：各操作的必填参数无法声明式表达，帮助与报错都更差）；`--move-dir <NAME> --dest <PARENT>` 伴随参数形式（拒绝：多一类「伴随参数缺失」的用法错误）；把按键序列 `--keys` 回放给 TUI 状态机（拒绝：输出不结构化，测试与键位耦合，给不出逐路由回执）。
- **Consequences:** help、EN/zh-CN/网站文档与三处兼容行由各卡原位更新；`mega2_browser_mkdir_test::token_flags_are_tui_only_and_json_never_posts` 必须在 MN-11 有意改写；`MutationClass::ExternalOrUnknown` 早退（`src/cli.rs:2006-2012`）已覆盖新的写路径，不需要改 `src/cli.rs` 的代码；MN-03 只更新该处注释（GC-MN-01、`DEP-MN-06`）。
- **Revisit when:** 需要 browser 现有功能以外的操作（`DEFER-MN-*`），或使用者要求批处理模式。

### ADR-MN-02: 一次调用至多一次请求；不 reload、不 preflight、不重试、不提示

- **Status:** Accepted
- **Context:** TUI 的写操作是「一次写请求加一次 reload GET」（`src/command/mega2_browser/mod.rs:651-753`）。黑盒断言需要确定的请求计数，并且要以服务端的回答为准。
- **Decision:** 非交互调用通过本地校验后恰好发出一次请求；本地校验失败则一次也不发。写成功后不 reload。不为「文件不可操作」做 preflight GET：Libra 省略 `is_directory`，服务端缺省即视为目录并按 mode 匹配，文件目标得到 400，缺失目标得到 404（mega2@`8ff880c` `mono_api_service.rs:1438-1490`）。写请求从不重试（这是产品行为约束；ER-10 是计划执行规则，与此无关）。超时意味着「结果未知」，由调用方用 `--list`/`--list-tags` 观察。非交互路径从不读 stdin、不提示、不要求 `--yes`：flag 本身携带确切目标，就是确认。
- **Alternatives considered:** 仿照 TUI 写后 reload（拒绝：两次请求，而且读失败会掩盖写结果）；preflight 类型检查（拒绝：多一次请求、存在竞态，而服务端已按 mode 拒绝）；增加 `--yes`（拒绝：脚本总会带上它，没有安全增益）。
- **Consequences:** 写之后的状态由调用方另行 `--list` 核对；文档写明「结果未知」时的处理方式。
- **Revisit when:** Mega2 提供幂等键或条件写（例如期望的父 commit）。

### ADR-MN-03: 机器输出：`data.operation` 判别，`target` 与 `receipt` 分离

- **Status:** Accepted
- **Context:** 现有列表 payload 是 `{server, ref, path, items}`（`src/command/mega2.rs:110-118`）。服务端回执里的 `path` 只是回执，不是导航依据（ADR-MB-04/05）。
- **Decision:** 成功 envelope 仍是 `{ok:true, command:"mega2 browser", data}`。`data.operation` 的取值为 `list`、`create-dir`、`delete-dir`、`move-dir`、`rename-dir`、`list-tags`、`create-tag`、`delete-tag`。写操作的 `data` 分为 `target`（由本地已校验的输入组成）和 `receipt`（服务端回执原样）。列表 payload 只新增 `operation`，其余键不变。JSON 中的服务端字符串经 serde 转义后原样输出，与 Libra 其它 `--json` 输出一致；人读输出一律经 `mega2_browser::sanitize`。人读输出不是机器契约。成功时 stdout 只有 envelope（或人读摘要）；失败时 stdout 为空。
- **Alternatives considered:** 每个操作用一个独立的 `command` 名（拒绝：`command` 表示 CLI 命令路径，操作属于数据）；对 JSON 也做 sanitize（拒绝：黑盒断言会失真）；回执含控制字符时在写成功后报失败（拒绝：会误报远端状态）。
- **Consequences:** 列表的消费者会看到一个新增字段。JSON 中的 C1 与 DEL 字符按 serde 规则透传，与全仓 `--json` 一致，已登记为风险。
- **Revisit when:** 全仓 JSON 输出政策改为转义全部控制字符。

### ADR-MN-04: 失败诊断：`details` 带 method/route 与 http_status 或 transport，stable code 不变

- **Status:** Accepted
- **Context:** 多个 HTTP 状态共用一个 stable code（例如 delete/move 的 404 与 500 都是 `LBR-NET-002`，`src/internal/protocol/mega2_mutate.rs:249-266`），黑盒只能解析非契约的 message 文本。`CliError::with_detail` 的键在发布后就是公开契约（`src/utils/error.rs:984-990`）。
- **Decision:** 由一个共享 helper（新文件 `src/internal/protocol/mega2_diag.rs`，GC-02）为四个客户端的每种失败附加 `details`：`method`（`GET`/`POST`/`DELETE`）与 `route`（路由模板常量，例如 `/api/v1/tags/{name}`，不代入具体名称）；收到 HTTP 状态时加整数 `http_status`（包括「2xx 但响应体不合规」的情况）；传输失败时加 `transport`（`timeout`、`connect` 或 `request`）。这些值只来自本地常量与状态码，从不包含响应体、`err_message`、token、URL 或 query。stable code、message、hint 与退出码全部保持不变（错误构造代码原地保留，只允许缩进变化）。键的含义写入 `docs/error-codes.md`。结构约束：八个发出请求的公开方法（tree 的 `fetch_listing`、create-entry 的 `create_directory`、mutate 的 `delete_directory` 与 `move_entry`（二者经共用的 `post_json` 发送，`rename_directory` 委托 `move_entry`），以及 tag 的 `list_tags`、`create_tag`、`get_tag`、`delete_tag`，对应八个「方法 + 路由」组合）的函数体一律放进 `mega2_diag::run` 提供的作用域，`post_json` 中的发送改经作用域的 `send`：作用域内唯一的一次发送经该作用域的 `send` 进行，`send` 记录本次请求的方法与路由模板，以及状态码或传输失败类别；作用域结束时，若已经发出请求，就对作用域返回的任何错误附加 details——发送失败附加 `{method, route, transport}`，收到状态码之后的任何错误（状态分支、响应体读取中断、超限、解析与校验失败，直到函数在 2xx 响应上的最后一道校验，例如 delete/move 的 `require_commit_id`）附加 `{method, route, http_status}`；在发出请求之前返回的错误（本地校验）原样返回、不带这些键。作用域只在原错误上附加键值，不重建 `CliError`。作用域以 tokio `task_local!`（或等价的、不改函数签名的任务上下文）实现：`run` 在执行方法体期间设置当前作用域，`mega2_diag::send(request, transport_error)` 从当前作用域取得方法与路由并记录实际的状态码或传输类别；因此 `post_json`、`finish` 等辅助函数的签名与调用行都不变（满足 G23），delete/move 在 `post_json` 返回之后调用的 `require_commit_id` 仍在同一作用域内，其错误带上实际状态码（例如 201）。客户端源码不再直接调用 `.send()` 或 `with_detail`，既有代码行只允许改变缩进、不得改写或重排（MN-01 的 G23 核对）。
- **Alternatives considered:** 按状态码细分 stable code（拒绝：会改变 TUI 与既有机器契约，另立 `DEFER-MN-06`）；回显 `err_message`（拒绝：违反 ADR-MB-01 的不回显响应体规则，服务端文本不可信）。
- **Consequences:** 黑盒可以精确断言状态码；文档列出逐操作的映射表与已知的不一致。
- **Revisit when:** `DEFER-MN-06` 的 stable code 统一 ADR 被接受。

### ADR-MN-05: 非交互 token 规则：读匿名并拒绝 token flag，写按 ADR-MB-03 优先序

- **Status:** Accepted（在 ADR-MB-03 基础上把写 token 扩展到非交互写）
- **Context:** 现在 `--json` 带 token flag 一律被拒，提示「TUI-only」（`src/command/mega2.rs:181-189`）；环境变量只在 TUI 路径读取（`:206-208`）。
- **Decision:** 读操作（`--list`、无操作 flag 的 `--json`、`--list-tags`）在任何输出模式下都拒绝 `--token`/`--token-file`（`LBR-CLI-002`，零请求），不读取 `LIBRA_MEGA2_TOKEN`，请求不带 `Authorization`。写操作的凭据由 MN-11 引入：在人读与 JSON 模式下都用 `resolve_token_from_process`，按 `--token-file` → `LIBRA_MEGA2_TOKEN` → `--token` 的顺序取至多一个 token；有 token 时写请求恰带一个 `Authorization: Bearer`，没有就匿名（适用于 `push_auth=none` 部署）。MN-11 之前，全部非交互操作都拒绝 token flag，写操作以匿名方式发送。凭据来源矩阵（仅文件、仅环境变量、仅 flag、三者同时、无来源，以及人读模式）由 MN-11 以 `create_dir` 经真实二进制验证一次；其它写操作走同一条类别凭据路径（GC-MN-11），由各自的 R9a–R9c 门证明接线。token 不持久化，也不出现在 stdout、stderr、JSON 或 `details` 中。TUI 路径不变。mega2 的 Libra 域计划用 `LIBRA_MEGA2_TOKEN` 或临时 `--token-file` 传 token，与本规则一致（mega2 `docs/plan/plan-20261001.md` 的 token 决策，未提交草稿，2026-10-01 13:46:48 UTC 时位于 `:244`）。
- **Alternatives considered:** 读操作静默忽略 token flag（拒绝：掩盖误用，也与既有的拒绝语义不一致）；写操作强制要求 token（拒绝：`push_auth=none` 部署是合法的）。
- **Consequences:** MN-11 必须改写 `token_flags_are_tui_only_and_json_never_posts`：保留「读操作带 token 被拒时 stderr 不含 token 值」与人读模式的断言，删除对「TUI-only」措辞的断言；读操作被拒时的零请求由 `op_rule_list_r5a`、`op_rule_list_r5b` 判定。文档继续警告 `--token` 会留在 shell history。
- **Revisit when:** Mega2 发布与 push token 分离的 API 凭据。

### ADR-MN-06: 参数边界：写操作拒绝 `--ref`；tag 操作只作用于 root；message 非空且有界

- **Status:** Accepted
- **Context:** 写路由不接受 ref。TUI tag 面板固定 `path="/"`（`src/command/mega2_browser/mod.rs:703,733,741`）。TUI 的 message 编辑器丢弃控制字符，上限 1024 字节，空 message 表示 lightweight（`tag_panel.rs:15-16,148-172`）。
- **Decision:** 任何写操作与 `--ref` 同用 → `LBR-CLI-002`，零请求。任何 tag 操作（含 `--list-tags`）要求 PATH 省略或为 `/`，且不得带 `--ref`。lightweight tag 由省略 `--message` 表达；`--message` 只能与 `--create-tag` 同用，给出时必须非空、不超过 1024 字节、不含任何控制字符（包括 `\n`），否则拒绝，而不是静默丢弃。`--page`/`--per-page` 只能与 `--list-tags` 同用，默认 1 与 20，取值范围沿用客户端上限（page 1..=1000，per_page 1..=100）。
- **Alternatives considered:** 静默忽略 `--ref`（拒绝：误导）；非根 tag path（延后：`DEFER-MN-02`）；沿用 TUI「空 message 即 lightweight」（拒绝：脚本里的空字符串多半是变量未赋值，显式省略更不易出错）；静默丢弃 message 中的控制字符（拒绝：脚本无法发现输入被改写）。
- **Consequences:** 与 TUI 相比，新增的可达请求只有两类：`--per-page` 取 20 以外的值；对文件名或不存在的名称发出的 delete/move（服务端按 mode 回 400 或 404，见 ADR-MN-02）。其余请求都在 TUI 可达集合之内。
- **Revisit when:** 引入非根 tag 或文件条目操作。

### ADR-MN-07: 黑盒测试的仓库分工：Libra 仓内的 live 门，mega2 仓内的 Libra 域

- **Status:** Accepted（2026-10-01 依 mega2 侧建议修订）
- **Context:** 现有 mega2 测试全是 loopback mock。mega2 的计划禁止 Git 协议 smoke 与 API 写 smoke（curl + git 脚本）以 libra 作客户端（mega2@`8ff880c` `docs/plan/plan-20260906.md:210-227`、`plan-20260917.md:284`、`plan-20260918.md:197`、`plan-20260904.md:143`）。mega2 侧于 2026-10-01 说明（使用者转达）：这些规定管不到在 Libra 仓里测 libra 对 mega2 的行为；mega2 仓内的 libra 用例应落在 `scripts/libra_smoke_storage_only.sh`，即 mega2 plan-20261001 的 Libra 域（`BB-65` 起编号已预留），不要改 curl + git 的那几个脚本。
- **Decision:** ① Libra 仓：MN-07 在 `command_test` 中交付 12 个 live 门（名为 `live_gate_*`：8 个操作各至少一个，写操作门以写后观察到的结果状态为判据），由 `LIBRA_TEST_MEGA2_SERVER`、`LIBRA_TEST_MEGA2_WRITE_ROOT`（两者必填）与 `LIBRA_TEST_MEGA2_TOKEN_FILE`（可选）门控；未设置时打印 `skipped (...)` 并通过。每个 live 门自给自足：用带本次 run id 的名称准备自己的前置对象、只断言本操作、结束时清理，与 mega2 Libra 域「单 case 可独立运行」的约定一致。不新增 Cargo feature，沿用 `LIBRA_TEST_MEGA_SERVER` 仅 env 门控的先例（`CLAUDE.md:268`）。live 门不以列表断言 lightweight tag 是否可见（事实基线「mega2 tag 列表分页」），改由 harness 直接发出匿名 `GET /api/v1/tags/{name}?path=%2F`，以服务端回报的 `tag_id` 等于 `object_id` 证明 lightweight tag 已建成（MN-07 G9），以删除后返回 404 证明已删除（G11）。② mega2 仓：本计划不写任何文件；通过 `DEP-MN-03` 向 mega2 plan-20261001 的 `DEP-BB-04` 交付已进入 libra.tools stable 的版本号、契约文档与 live 门，并转告 lightweight tag 分页行为，由 mega2 侧在 `scripts/libra_smoke_storage_only.sh` 追加 `BB-65` 起的用例。③ 任何一方都不得向 `scripts/git_protocol_smoke_storage_only.sh`、`scripts/api_write_smoke_storage_only.sh` 加入 libra 用例。
- **Alternatives considered:** 只用 mock（拒绝：无法证明与真实服务端互通）；一个串行的长场景（拒绝：步骤相互依赖、失败难以定位，也不符合「单 case 自给自足」）；由本计划直接改 mega2 的 compose 或脚本（拒绝：跨仓写入，且 mega2 的 Libra 域由 mega2 侧维护）；把 libra 用例加进 curl + git 脚本（拒绝：违反 mega2 现有规定）；新增 `test-live-mega2` feature（拒绝：改 `Cargo.toml` 非版本行会命中 T-1，又没有额外的隔离收益）。
- **Consequences:** live 证据依赖操作者提供的实例（`DEP-MN-04`）。mega2 侧从 libra.tools 安装最新 stable 并经环回中继访问 `127.0.0.1:9000`，与本命令「`http://` 只接受环回主机」的校验相容；mega2 侧的功能下限取本计划最后一张写操作卡的发布版本。
- **Revisit when:** mega2 侧要求 Libra 提供额外能力，或 mega2 修改其黑盒工具分工。

### ADR-MN-08: 操作登记表与类别规则：通用规则只实现一次，按「操作 × 规则」逐门验收

- **Status:** Accepted（2026-10-01，回应 Codex R1 P1-3 与 R2 P1-1；门族型豁免由使用者批准）
- **Context:** 8 个操作共享一组通用规则（非 TTY、人读输出、`--quiet`、失败输出、token、`--ref`、tag 选择器、操作互斥）。逐卡各写一套会产生多份逻辑（违反 GC-02）；把它们写成全局约束不计数，又会让各卡少计判据（R1 P1-3）；把「某操作满足全部规则」记成一条，又把可分别失败的检查打包（R2 P1-1）。
- **Decision:** `src/command/mega2_browser/noninteractive.rs` 维护唯一的操作登记表：每个非交互操作一行，记录 flag、HTTP 方法与路由模板、类别（`read` 或 `write`；`dir` 或 `tag`）。通用规则按类别实现一次。验收按「操作 × 适用规则」展开为具名用例 `op_rule_<op>_<rule>`（由测试文件中的宏生成，`<op>` 取 `list`、`create_dir`、`delete_dir`、`move_dir`、`rename_dir`、`list_tags`、`create_tag`、`delete_tag`，`<rule>` 取下表规则编号的小写形式，如 `r4a`），每个用例是一个门；本地拒绝类规则（R5a、R5b、R7、R8、R10a、R10b）各展开为 `_code` 与 `_no_request` 两个用例、计两门；各操作卡在「判据规范」中逐门列出，门数如实计入 AC 分子，超出 8 的部分由 EX-MN-02 豁免。「本地拒绝」与「请求记录」的含义见 GC-MN-02。原子规则（每条是一个独立判据，每个门只用「调用模式」列的一种模式调用一次）：

  | 规则 | 适用类别 | 调用模式 | 判据 | 引入卡 |
  |---|---|---|---|---|
  | R1 | 全部 | 人读 | stdin 为 `/dev/null`、stdout/stderr 为管道时，本操作对成功 mock 的调用退出码为 0（若误入 TUI，TTY 门会令其以 `LBR-UNSUPPORTED-001` 失败） | MN-02 |
  | R1b | 全部 | 人读 | stdin 为保持打开、不写入任何数据的管道时，本操作对成功 mock 的调用在 5 秒内结束（若读取 stdin 会一直阻塞到超时，从而判失败） | MN-02 |
  | R2 | 全部 | 人读 | 人读模式的成功调用，stdout 不含字节 `0x1b` | MN-02 |
  | R3 | 全部 | 人读，加 `--quiet` | 人读模式加 `--quiet` 的成功调用，stdout 为空 | MN-02 |
  | R4a | 全部 | `--machine` | mock 对本操作的路由返回 500 时，`--machine` 下 stderr JSON 错误信封的 `details` 等于 `{method:<本操作方法>, route:<本操作路由模板>, http_status:500}` | MN-02 |
  | R4b | 全部 | `--machine` | mock 对本操作的路由返回 500 时，请求记录的长度为 1（不重试） | MN-02 |
  | R5a | `read`（MN-11 之前也适用于 `write`） | `--json`，加 `--token secret` | 带 `--token` 时，结果为本地拒绝 `LBR-CLI-002`（两门：`…_code` 断言 stable code，`…_no_request` 断言请求记录为空） | MN-02；MN-11 收窄为 `read` |
  | R5b | `read`（MN-11 之前也适用于 `write`） | 人读，加 `--token-file <文件>` | 带 `--token-file` 时，结果为本地拒绝 `LBR-CLI-002`（两门：`…_code` 断言 stable code，`…_no_request` 断言请求记录为空） | MN-02；MN-11 收窄为 `read` |
  | R6 | `read` | `--json` | `LIBRA_MEGA2_TOKEN` 已设置时，请求记录中的 Authorization 为「无」 | MN-02 |
  | R7 | 全部 | `--machine` | 与另一个操作 flag 同用时，结果为本地拒绝 `LBR-CLI-002`（两门：`…_code` 断言 stable code，`…_no_request` 断言请求记录为空）（`--list` 以外的操作与 `--list` 组合；`create_dir` 的 R7 门同时覆盖 `--list` 一侧） | MN-03 |
  | R8 | `write` | `--machine` | 与 `--ref` 同用时，结果为本地拒绝 `LBR-CLI-002`（两门：`…_code` 断言 stable code，`…_no_request` 断言请求记录为空） | MN-03 |
  | R9a | `write` | `--json` | `--token-file`、`LIBRA_MEGA2_TOKEN`、`--token` 三个来源同时设置时，请求恰带一个 `Authorization: Bearer <t>`，`t` 为 `--token-file` 的内容（优先序由既有的 `resolve_token` 及其进程包装 `resolve_token_from_process` 决定，`src/internal/protocol/mega2_auth.rs:120-167`；其两两优先序由既有单元测试 `precedence_prefers_file_then_env_then_flag`（`:188`）覆盖） | MN-11 |
  | R9b | `write` | `--json` | 没有任何 token 来源时，请求记录中的 Authorization 为「无」 | MN-11 |
  | R9c | `write` | `--json` | 带 token 且 mock 返回 401 时，进程的 stdout 与 stderr 合并输出不含 token 值 | MN-11 |
  | R10a | `tag` | `--machine` | PATH 不为 `/` 时，结果为本地拒绝 `LBR-CLI-002`（两门：`…_code` 断言 stable code，`…_no_request` 断言请求记录为空） | MN-05 |
  | R10b | `read` 且 `tag` | `--machine` | 与 `--ref` 同用时，结果为本地拒绝 `LBR-CLI-002`（两门：`…_code` 断言 stable code，`…_no_request` 断言请求记录为空）（写 tag 操作的 `--ref` 由 R8 覆盖，不重复建门） | MN-05 |

  适用矩阵（括号内为门数，本地拒绝类规则各计两门）：`list` = R1、R1b、R2、R3、R4a、R4b、R5a、R5b、R6（11）；`create_dir` 在 MN-03 = R1、R1b、R2、R3、R4a、R4b、R5a、R5b、R7、R8（14），MN-11 起以 R9a、R9b、R9c 取代 R5a、R5b（13）；`delete_dir`、`move_dir`、`rename_dir` = R1、R1b、R2、R3、R4a、R4b、R7、R8、R9a、R9b、R9c（13）；`list_tags` = R1、R1b、R2、R3、R4a、R4b、R5a、R5b、R6、R7、R10a、R10b（17）；`create_tag`、`delete_tag` = R1、R1b、R2、R3、R4a、R4b、R7、R8、R9a、R9b、R9c、R10a（15）。
- **Alternatives considered:** 每卡各写一套（拒绝：GC-02）；把规则写成全局约束不计数（拒绝：R1 P1-3）；把「`op_class_rules` 覆盖本操作」记成一条判据（拒绝：R2 P1-1，打包了可分别失败的检查）；按严格计数拆卡到每卡 ≤ 8（不采用：会产生约 20 张碎片卡，拆散同一操作的实现与验收，违反 G-02；使用者 2026-10-01 选择登记豁免）。
- **Consequences:** 新操作或新规则必须同批更新本表、对应卡的「判据规范」、EX-MN-02 与审计表；R5a、R5b 的拒绝消息在 MN-11 之前保留原有的「TUI-only」措辞，MN-11 起改为 `mega2 browser: read operations take no credentials; --token/--token-file only apply to write operations`（hint 不变），因此既有守卫用例直到 MN-11 才改写（GC-MN-05）。
- **Revisit when:** 出现不能按类别表达的通用规则，或操作数增长到登记表难以维护。

## 全局工程约束

本计划继承模板 v2.12 的 GC-01..GC-13 与 ER-01..ER-14。下列补充约束同时生效。GC-13 不适用：本计划不触及任何 SQLite schema、连接建构或数据库角色。

- **GC-MN-01 单一子命令，不改 `src/cli.rs` 的代码：** `mega2` 只暴露 `browser`。本计划不修改 `src/cli.rs` 的任何代码：命令注册、about、preflight、scope 与 `MutationClass` 全部保持原样（`operation_class_for_command` 已对 `Commands::Mega2(_)` 早退为 `ExternalOrUnknown`，`src/cli.rs:2006-2012`）。唯一的例外是 MN-03 改写 `src/cli.rs:2006-2009` 的注释（它说 mega2 只在 TUI 确认后写远端，MN-03 起不再成立），只改注释、不改代码；该改动经 `DEP-MN-06` 进入 `DEP-AD-12 / DEP-CLI-mirror` 的三态串行。若实现中发现还须改 `src/cli.rs` 的代码，先按 ER-03 修订计划。
- **GC-MN-02 请求计数与判定术语：** 非交互调用在本地校验通过后恰好发出一次 HTTP 请求，校验失败时为零次；不 reload、不 preflight、不重试（ADR-MN-02）。下列术语在全部任务卡中含义固定：**请求记录**指 mock 按到达顺序记下的 `(方法, 路径与 query, Authorization 头或「无」, 请求体或「空」)` 列表，「请求记录等于 X」是对整个列表的一次相等断言，计一个判据；**本地拒绝 `<code>`** 由两个独立判据组成，各成一门：`…_code`（进程以 stderr 报告的 stable code `<code>` 失败）与 `…_no_request`（请求记录为空）；**服务端失败 `<code>`/`<status>`** 指以 `--machine` 运行、mock 返回 `<status>` 的 fixture，同样由两个独立判据组成，各成一门：`…_code`（stderr JSON 错误信封的 `error_code` 为 `<code>`）与 `…_http_status`（其 `details.http_status` 为 `<status>`）。
- **GC-MN-03 不碰终端、不读输入：** 非交互路径不调用 `ensure_tty`、`TerminalGuard`、`read_key`，不读 stdin，不提示。「不读 stdin」由每个操作的 R1b 门判定：stdin 是保持打开、没有输入的管道时，调用必须在 5 秒内结束。
- **GC-MN-04 秘密与回显边界：** 读匿名；写按 ADR-MN-05 取 token。token、URL 凭据、响应体和 `err_message` 不进入任何输出或 `details`（URL 解析错误的回显由 MN-10 修复）。
- **GC-MN-05 TUI 不回归：** `mega2_browser::run`、`BrowserState`、`TagPanel` 的行为与既有 TUI 用例保持不变，包括无操作 flag 时的 TTY 门与非 JSON `--quiet` 拒绝。允许有意修订的既有用例只有 `tests/command/mega2_browser_mkdir_test.rs::token_flags_are_tui_only_and_json_never_posts`，且只能在 MN-11 修订；其余既有 mega2 用例必须不修改即全绿。
- **GC-MN-06 帮助守卫：** 父命令的 `MEGA2_EXAMPLES` 与 `browser` 子命令的 about 不得出现 `mkdir`、`rmdir`、`mv `、`delete-entry`、`move-entry`、`tag `；`MEGA2_BROWSER_EXAMPLES` 与 browser 的参数帮助不得出现 `mkdir`、`rmdir`、` mv `。对应守卫为 `mega2_browser_cli_test.rs:358-374`、`mega2_browser_mkdir_test.rs:352-369`、`mega2_browser_mutate_test.rs:322-341`、`mega2_browser_tag_test.rs:476-484`。tag 相关示例只放在 `MEGA2_BROWSER_EXAMPLES`。
- **GC-MN-07 兼容矩阵原位改写：** `COMPATIBILITY.md` 只原位改写第 248 行，不增删行。这是保守的选择：compat ledger 以绝对行号解析 `COMPATIBILITY.md:<line>` 证据（`tests/compat/compat_ledger_schema.rs:486-499`），但它目前引用的最大行号是 180（例如 `tests/compat-ledger/t4/DIRECT_SNAPSHOT.tsv:7`），不受第 248 行处增删的影响；真正会失准的是 `docs/development/gap/grit-suite-scope.md:173` 对 `COMPATIBILITY.md:259-261` 的引用，而且其它计划也可能并发编辑该文件。`docs/development/commands/_compatibility.md:57` 与 `docs/development/commands/README.md:57` 同样原位改写。每张触及这三处的卡都要跑 `compat_ledger_schema` 与 `compat_matrix_alignment`。
- **GC-MN-08 契约 pin 重核：** MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09、MN-07 开工前按 `DEP-MN-01` 重核 mega2 的 revision、路由、方法、请求/响应字段与鉴权形态（例如在 `../mega2` 运行 `rg -n '"/tree"|"/create-entry"|"/delete-entry"|"/move-entry"|"/tags' -g '*.rs' src/api`，再对照字段表；该命令仅用于锚点定位，非判据），结果写入该卡 `Current evidence`。发现漂移就先停卡，修订 `DEP-MN-01` 后再继续，不得猜测兼容。
- **GC-MN-09 文档三处同步：** 每张改变用户可见行为的卡，在同一卡内同步 `docs/commands/mega2.md`、`docs/commands/zh-CN/mega2.md` 与网站 `mega2.en.md`（ER-06a，`DEP-MN-02`），并同步 `docs/development/commands/mega2.md`。网站页必须包含本卡「Docs and compatibility impact」写明的网站标记（逐字），供 D 组核对。写网站的卡（MN-02 起）在 A 组与 C 组第 ④ 步以 `LIBRA_SITE_MEGA2_DOC` 指向网站页运行 `site_example_paths_are_rooted`（MN-02 新增），以真实的 `Cli::try_parse_from` 与 `normalize_path` 校验网站页的每条 `mega2 browser` 示例。该测试标为 `#[ignore]`，运行时变量未设置即失败，没有跳过分支；门命令必须显式包含被忽略的测试（`cargo test … -- --include-ignored`／`--ignored`，`cargo nextest run --run-ignored all`／`only`），证据记录它的 PASS 行，未运行或被忽略都视为该门未通过。网站仓库里未跟踪的 `.teamx/` 不属于本计划，不得暂存。
- **GC-MN-10 测试落点与运行环境：** 新的 L1 用例集中在 `tests/command/mega2_browser_noninteractive_test.rs`（MN-02 新建，并在 `tests/command/mod.rs` 注册，命中 T-4），之后各卡只往该文件追加；mock 写在该文件内，不改 `tests/helpers/`、`tests/harness/`。该文件的用例都以 `env_clear()` 启动真实二进制，stdin 为 `/dev/null`（R1b 门除外：stdin 为保持打开、不写入数据的管道，子进程以 5 秒为上限，超时即杀掉并判失败），stdout/stderr 为管道；不改进程级环境，因此不加 `#[serial]`。如确需新增，必须同步 `tests/SERIAL_REGISTRY.tsv` 并重新生成 `.config/nextest.toml`。
- **GC-MN-11 登记表治理：** 每个非交互操作必须在 ADR-MN-08 的登记表中登记，并为其适用规则生成 `op_rule_<op>_<rule>` 用例；ADR-MN-08 规则表中的通用行为只能在登记表的类别规则里实现，不得在单个操作的分支中另写一份（GC-02）。
- **ER-MN-01 发布切片：** 每张卡都是 `independent` 发布切片，严格按「发布窗口顺序」串行，由单一发布者执行完整 C 组：版本面 parity 预检 → `patch + 1`（以 `compat_version_surface_sync` 报告的集合为准，当前为三处）→ 由工具链刷新 `Cargo.lock` → 在已 bump 的树上跑 fmt、clippy，以及按 ER-13 判定的测试门：命中触发条件的卡跑 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`；未命中的卡逐条运行该卡「C 组第 ④ 步（ER-14）」字段列出的命令（测试命令以 nextest 执行，`rg` 等非测试命令原样执行；ER-14）→ `cargo build --release` → 安装 → 精确暂存并 `libra commit -s`（签名已开启：`commit.gpgSign=true`；提交后按 ER-07 校验 `gpgsig` 与 `Signed-off-by`）→ `libra push origin <feature-branch>` → `gh -R libra-tools/libra pr create` → 同一 head SHA 上 `base.yml` 与 CodeQL 全绿 → squash merge → 在 merge SHA 上 `gh -R libra-tools/libra release create v<version>`。bump 前必须先 `libra pull --ff-only` 并重新读取 `Cargo.toml` 版本；目标版本已被并发发布占用时顺延，不得复用版本号。
- **ER-MN-02 D 组标准（D-MN-STD）：** ① `.github/workflows/codeql.yml` job `analyze`，事件 `push`、ref `main`（squash merge 产生的提交），success；② `.github/workflows/release.yml` jobs `build-and-upload`、`upload-install-scripts`、`update-homebrew-tap`、`verify-homebrew-formula`、`request-stable-manifest`，事件 `push`、ref `v<version>`（由 `gh release create` 触发），全部 success；③ 以 bash 运行 `code=$(curl -sS -o /dev/null -I -w '%{http_code}' "https://download.libra.tools/libra/releases/v<version>/libra-linux-amd64") && [ "$code" = 200 ]`，`curl` 出错或状态码不是 200 都判失败（记录状态码；2026-10-01 对 `v0.30.10` 试运行得 200，对不存在的版本得 404）；④ 网站：`cf` 推送且 Cloudflare 部署完成后，以 bash 运行下面的 fail-closed 检查（`<marker>` 换成本卡「Docs and compatibility impact」写明的网站标记），记录 `cf` 推送 SHA、核对时间与输出；`curl` 失败、HTTP 状态不是 200 或标记计数为 0 都判失败：

  ```bash
  resp=$(curl -fsSL --max-time 30 -w '\n%{http_code}' https://libra.tools/en/docs/commands/mega2) || { echo "FAIL: curl exit $?"; exit 1; }
  code=${resp##*$'\n'}; body=${resp%$'\n'*}
  [ "$code" = 200 ] || { echo "FAIL: HTTP $code"; exit 1; }
  count=$(printf '%s\n' "$body" | rg -c -F -- '<marker>' || true)
  [ "${count:-0}" -ge 1 ] || { echo "FAIL: marker missing"; exit 1; }
  echo "OK: HTTP 200, marker count $count"
  ```

  站点部署不经 GitHub Actions（后端 `ci.yml` 只覆盖 `main`/`develop`），标记暂未出现就停在 `remote-pending`。不写网站的卡（MN-10、MN-07）没有第 ④ 项。任何失败只能前滚。
- **ER-MN-03 评审：** ER-05 的 Codex review 使用高召回模型（不低于 gpt-5.5 级；本计划使用本机 Codex 默认的 `gpt-6-sol`、`xhigh`），spark 级的 `PASS` 不作数；P0/P1 必须关闭。计划级评审取得 `PASS` 之前，任何卡不得标 `in-progress`。

## 执行检查必备需求（强制）

完整继承模板的 ER-01..ER-14，正文不重复。本计划的补充是上一节的 ER-MN-01..ER-MN-03。

## 实施顺序

依赖边格式：`A -> B` 表示 A 必须先于 B。

- `MN-10 -> MN-01`
- `DEP-MN-02 -> MN-01`
- `MN-01 -> MN-02`
- `DEP-MN-02 -> MN-02`
- `MN-02 -> MN-03`
- `DEP-MN-01 -> MN-03`
- `DEP-MN-02 -> MN-03`
- `DEP-MN-06 -> MN-03`
- `MN-03 -> MN-11`
- `DEP-MN-01 -> MN-11`
- `DEP-MN-02 -> MN-11`
- `MN-11 -> MN-04`
- `DEP-MN-01 -> MN-04`
- `DEP-MN-02 -> MN-04`
- `MN-04 -> MN-08`
- `DEP-MN-01 -> MN-08`
- `DEP-MN-02 -> MN-08`
- `MN-08 -> MN-12`
- `DEP-MN-01 -> MN-12`
- `DEP-MN-02 -> MN-12`
- `MN-12 -> MN-05`
- `DEP-MN-01 -> MN-05`
- `DEP-MN-02 -> MN-05`
- `MN-05 -> MN-06`
- `DEP-MN-01 -> MN-06`
- `DEP-MN-02 -> MN-06`
- `MN-06 -> MN-09`
- `DEP-MN-01 -> MN-09`
- `DEP-MN-02 -> MN-09`
- `MN-09 -> MN-07`
- `DEP-MN-01 -> MN-07`
- `DEP-MN-04 -> MN-07`

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-MN-01 | incoming | 跨仓 API 契约 | mega2 storage-only（trunk）产品 HTTP，`../mega2` `main@8ff880c` | mega2 维护者 | 路由仍挂载：`GET /api/v1/tree`（`preview_router.rs:266`）、`POST /api/v1/create-entry`（`:115`）、`POST /api/v1/delete-entry`（`:173`）、`POST /api/v1/move-entry`（`:201`），后三者在 `write_routers`（`:61`）中；`storage_only_routers_with`（`api_router.rs:72`）在 `:83` merge `tag_router`：`POST /tags`（`tag_router.rs:155`）、`GET /tags/list`（`:219`）、`GET /tags/{name}`（`:275`）、`DELETE /tags/{name}`（`:324`）。DTO 字段：`CreateEntryInfo`（`git.rs:13`）、`CreateEntryResult{commit_id,new_oid,path,cl_link}`（`:239`）、`DeleteEntryInfo{path,name,is_directory(默认 true),author_username,skip_build}`（`:316`）、`DeleteEntryResult{commit_id,path,cl_link}`（`:343`）、`MoveEntryInfo{from_path,from_name,to_path,to_name,is_directory,author_username,skip_build}`（`:356`）、`MoveEntryResult{commit_id,from_path,to_path,cl_link}`（`:402`）、`TreeBriefItem{name,path,content_type}`（`:155`）、`TreeResponse{file_tree,tree_items}`（`:577`）、`CreateTagRequest{name,target,path_context,tagger_name,tagger_email,message}`（`tag.rs:19`）、`TagResponse` 七字段（`:37`）、`TagListQuery{page,per_page,path}`（`:60`）、`DeleteTagResponse{deleted_tag,message}`（`:82`）。delete/move 按 mode 匹配（`mono_api_service.rs:1438-1490`）；状态与鉴权见契约页 `docs/refactoring/directory-entry-api.md:192-224,363-416` | 2026-10-01 12:04:09 UTC 在 `../mega2` 用 `rg` 核对上述行号；字段与 plan-20260912 的 pin `a1293686`（0.38.19）一致，仅行号漂移 | 漂移 → 对应卡 `blocked`；修订本行与受影响的卡后再继续。禁止猜测兼容、发明第三套字段或回退到 Git 推送 |
| DEP-MN-02 | incoming | 相邻网站文档仓库 | `../libra-backend`（Git，分支 `cf`），页面 `apps/tanstack-app/content/docs/commands/mega2.en.md`，部署于 `https://libra.tools/en/docs/commands/mega2` | libra-backend 维护者 | 写入前按 ER-06a 核对：只存在 `.git`，当前分支严格为 `cf`，与 `origin/cf` 的超前/落后已记录；按其 `AGENTS.md` 跑 `pnpm typecheck`、`pnpm build` 与 `pnpm preview:cf`（确认页面加载）；签名提交并普通推送 `cf` | 2026-10-01：`## cf...origin/cf`，未跟踪 `.teamx/`（不属于本计划，不得暂存）；`../libra-backend/AGENTS.md:21,58-62` | 不满足（脏到无法安全写入、不在 `cf`、无法切换、元数据歧义）→ 当卡 `blocked`，不写网站，不宣称发布；`cf` 干净但含无关超前提交时，走 ER-06a 的隔离克隆例外 |
| DEP-MN-03 | outgoing | 跨仓交付 | mega2 plan-20261001 的 Libra 域（`scripts/libra_smoke_storage_only.sh`；`DEP-BB-04` / `DEFER-BB-03`；`BB-65` 起编号已预留） | mega2 维护者（接收方） | 稳定操作前提：MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09 都已发布，且各自的 `request-stable-manifest` 成功（能力已进入 libra.tools stable）。交付在 MN-07 完成时进行，交付物：stable 版本号（作为 mega2 侧 browser 写用例的功能下限）、`docs/commands/mega2.md` 的「Non-interactive operations」节（逐操作契约）、MN-07 的 live 门（参考用例），以及事实基线「mega2 tag 列表分页」中观察到的 lightweight tag 行为。接收方只在 `scripts/libra_smoke_storage_only.sh` 追加用例；任何一方都不得向 `scripts/git_protocol_smoke_storage_only.sh`、`scripts/api_write_smoke_storage_only.sh` 加入 libra 用例 | mega2 `docs/plan/plan-20261001.md` 的 Libra 域、GC-BB-01、`BB-65` 起编号预留、DEP-BB-04、DEFER-BB-03（未提交草稿，按 ID 引用；行号见事实基线「外部参照：mega2 黑盒工具分工」）；mega2 侧建议（使用者 2026-10-01 转达） | 本计划不等待接收方。MN-07 完成时在本表与 `plan-status.md` 记为「已交付」，并附版本号与文档链接。接收方暂未追加用例时，其 `DEFER-BB-03` 保持原状，Libra 不需返工；本计划不改 `../mega2/**`；**已交付（2026-10-03 13:05:47 UTC）**：功能下限 stable `v0.30.26`；live 参考版本与当前 stable `v0.30.27`（signed manifest 已核对）。[固定提交契约](https://github.com/libra-tools/libra/blob/faae20c6d9e48c46b9bb290637a8425b6d1e073a/docs/commands/mega2.md#non-interactive-operations)、[live 参考用例](https://github.com/libra-tools/libra/blob/faae20c6d9e48c46b9bb290637a8425b6d1e073a/tests/command/mega2_browser_noninteractive_test.rs#L2651)；lightweight tag 可能在列表页缺失或跨页重复，以服务端匿名 GET 观察形态，annotated tag 可按名称分页。按本表约定完成登记即为交付，本计划不等待接收方追加用例 |
| DEP-MN-04 | incoming | 外部服务（测试实例） | 一个可写的 mega2 storage-only 实例，revision 已 pin（例如 mega2 compose `-p mega2-it --profile app` 的 `http://127.0.0.1:19180`，配置可取 `config/config-storage-only.none.toml`） | 操作者（执行 MN-07 的发布者） | 可用判据：`--list` 写根成功；写根是已存在于 `root_dirs` 的一级根（默认 `root_dirs` 含 `project`，`config/config.toml:87`；trunk 产品写不能落在 `/`，契约页 `:118,153,218`）；tag 门需要 `push_auth=none` 或能覆盖 `/` 的 token（契约页 `:402-416`）；root tag 总数不超过 900（建议使用隔离实例）；记录实例 revision 与 `push_auth` 形态 | mega2 `docs/plan/plan-20260727.md:45`（compose app profile 与端口） | 在 MN-09 `done/complete` 当日首次核对并记录可用性；自 MN-09 `done/complete` 起 7 天内仍不可用 → 通过一次登记在修订历史中的规范性修订降级：把 MN-07 判据规范中的 12 个 live 门（G1–G12）转为 `DEFER-MN-09`，**同一修订**删除 `DEP-MN-04 -> MN-07` 依赖边，并同步 MN-07 的 `Dependencies`、AC 分子、`Granularity`、EX-MN-03、审计表、Phase 4 进入条件、M5 与 `plan-status.md`；此后 MN-07 以其余判据验收；已核对可用，固定源码 revision / 二进制 SHA-256 与 12/12 真实门证据见「固定源码实例复验」；未触发降级 |
| DEP-MN-05 | outgoing | 跨计划信息移交 | [`plan-20260923.md`](plan-20260923.md) CP-19（已实现能力覆盖台账） | plan-20260923 owner | 本计划新增的 `mega2 browser` 操作 flag 进入其能力清单；双方实现写集无交集（CP-19 为 audit-only） | `plan-20260923.md:73`（Remote/cloud 行含 mega2）；CP-19 的 I 集为 N/A | 接收方开工早于本计划完成时，以其开工当日的源码为准，不等待；本计划不改该计划文件 |
| DEP-MN-06 | incoming | 跨计划写集互斥 | `src/cli.rs` 三态串行（`plan-status.md` 的 `DEP-AD-12 / DEP-CLI-mirror`） | 名单内各计划 owner | MN-03 只改写 `src/cli.rs:2006-2009` 的注释；开工前确认该文件不在名单内任何卡的「已改未推」窗口中，并在 `plan-status.md` 的 `DEP-AD-12 / DEP-CLI-mirror` 行登记「plan-20261001-mega-browser-noninteractive MN-03（仅注释）」 | `plan-status.md` 的 `DEP-AD-12 / DEP-CLI-mirror` 行（2026-10-02 核对：名单为 plan-20260918 OI-05、plan-20260904 CX-30、plan-20260912 MB-03/05、plan-20260916 CAP-07、issues/483 CO-03/04，以及本计划登记的 MN-03；原镜像行 `DEP-QP-01` 已随 issues/574 收口删除） | 窗口被占用 → MN-03 `blocked`，等该卡推送后再开工；不得与名单内的卡并行改 `src/cli.rs` |

### 发布分组与并发窗口

本计划没有家族卡或批量发布组，不登记 `REL-*`。每张卡都是 `independent` 发布切片。

**并发声明:** 全串行。MN-10 与 MN-01 同写 `src/internal/protocol/mega2_tree.rs`；其余卡的实现写集两两相交（`src/command/mega2.rs`、`src/command/mega2_browser/noninteractive.rs`、`docs/commands/mega2.md` 等），只能按实施顺序串行。跨计划方面，本计划只有 MN-03 改写 `src/cli.rs:2006-2009` 的注释，经 `DEP-MN-06` 按 `DEP-CLI-mirror` 的三态窗口串行，其余卡不触碰 `src/cli.rs`；开工时按 ER-01 复核 `libra status --short --branch`，以及目标文件是否被其它计划改动。

**发布者:** 执行本计划的单一主代理。开工时在修订历史登记会话标识；未经修订不移交（ER-12）。

**发布窗口顺序:** `MN-10 -> MN-01 -> MN-02 -> MN-03 -> MN-11 -> MN-04 -> MN-08 -> MN-12 -> MN-05 -> MN-06 -> MN-09 -> MN-07`。同一时刻至多一张卡处于「已 bump 但未完成推送或未触发 release」的状态。

### Phase 0: 安全修复与诊断基础

**目标:** 修复 URL 解析错误回显；让所有 mega2 HTTP 失败都带上可断言的诊断细节。

**进入条件:**

- 计划级 Codex review `PASS`（ER-MN-03）。
- MN-01 开工前 `DEP-MN-02` 核对通过（MN-10 不写网站，不需要）。

**退出条件:**

- MN-10、MN-01 `done`/`complete`。

### Phase 1: 非交互调度与列表

**目标:** 建立非交互执行路径与操作登记表，交付 `--list`，并修正文档示例。

**进入条件:**

- MN-01 `done`/`complete`；`DEP-MN-02` 核对通过。

**退出条件:**

- MN-02 `done`/`complete`。

### Phase 2: 目录写操作

**目标:** 交付 `--create-dir`、写操作凭据、`--delete-dir`、`--move-dir`、`--rename-dir`。

**进入条件:**

- MN-02 `done`/`complete`；`DEP-MN-01` 重核通过（GC-MN-08）。

**退出条件:**

- MN-03、MN-11、MN-04、MN-08、MN-12 `done`/`complete`。

### Phase 3: tag 操作

**目标:** 交付 `--list-tags`、`--create-tag`、`--delete-tag`。

**进入条件:**

- MN-12 `done`/`complete`；`DEP-MN-01` 重核通过。

**退出条件:**

- MN-05、MN-06、MN-09 `done`/`complete`。

### Phase 4: 互通证明与收口

**目标:** 在真实 Mega2 上证明全部非交互操作可用，补齐黑盒使用指南，向 mega2 侧交付，跑收口全量门。

**进入条件:**

- MN-09 `done`/`complete`；`DEP-MN-04` 可用，或已按其失败策略完成降级修订。

**退出条件:**

- MN-07 `done`/`complete`；「完成判据」全部满足。

## 任务卡

### 任务卡粒度规则（强制）

全部卡遵守 G-01..G-11。ER-04 的强制门，以及由本卡公开行为变更触发的 ER-06/ER-06a 同卡文档同步门，不计入各卡的 AC/VER 计数（G-03）。每条 AC 只含一个独立判据（「请求记录等于」计一个判据；「本地拒绝」与「服务端失败」各由两个门组成，见 GC-MN-02）；对既有校验器（`validate_entry_name`、`validate_tag_name`、`normalize_path`、分页校验）的委托以一个代表性 fixture 判定，校验器自身的各项规则由其既有单元测试覆盖。门族（MN-01 的诊断门；各操作卡的 `op_rule_<op>_<rule>` 规则门与本地拒绝、服务端失败的 fixture 门，以及 MN-06 新校验器与 MN-11 凭据来源矩阵的 fixture 门；MN-07 的 live 门与 harness 门）在卡内「判据规范（非计数正文）」中逐门列出，每门都是可直接复制执行的 `--exact` 命令，任一门失败即整卡不达标；门数如实计入 AC 分子，写作 `n/8@EX-ID`。`Verification` 中用一条前缀过滤命令运行整个门族，按一门计。新增用例标 `(new)`，均落在已存在的 `command_test` target（MN-02 起含新文件 `mega2_browser_noninteractive_test.rs`）或 lib 单元测试中，不新增 `--test` target。卡片按实施顺序排列；编号顺序不是执行顺序。

#### 字段全局默认与例外

- **Release boundary 默认:** `independent`（完整 C 组，含 `patch + 1` 与 `gh` 发布）。
- **Task type 默认:** `implementation`。
- **Rollback mode 默认:** `immutable-release`。本卡的 `v<version>` 发布后不可撤回，恢复动作只有一个：发布下一个 patch，其源码变更是本卡提交的 revert（走完整 C 组，ER-08）；写网站的卡在同一恢复中于 `cf` 上提交恢复页面文本的补偿提交（普通推送，不 force push），并按 ER-MN-02 ④ 核对标记已消失。降级指引：用户经 `libra upgrade` 或官方安装器升级到该修复 patch，即回到本卡之前的行为。兼容窗口：从本卡发布到修复 patch 发布之间，本卡新增的 flag 与字段保持本计划所述契约；修复 patch 发布后它们不再存在，黑盒调用方以 `libra --version` 判断可用性，`DEP-MN-03` 记录的功能下限随之更新。本卡在 C 组第 ⑧ 步推送之前失败时，丢弃本地变更即可（尚无外部副作用）。
- **Migration and rollback 默认:** `N/A`：无 SQLite、schema 或存储格式变更（GC-13 不适用）。
- **Security and privacy 默认:** 继承 GC-07、GC-11、GC-MN-03、GC-MN-04。
- **Performance budget 默认:** 继承 GC-10、GC-MN-02。

**默认覆盖**（不是例外，只是取了非默认值，无需审批）：

| 任务 | 偏离的字段 | 取值与理由 |
|---|---|---|
| MN-10 | Docs and compatibility impact | N/A：三处文档已承诺「错误不回显凭据」，本卡只让实现与之一致，不改文档文本 |
| MN-10 | Rollback mode | `forward-only`：本卡修复凭据回显，revert 会重新暴露该问题；只以保持「URL 解析错误不回显原始输入」不变量的前滚修复恢复 |
| MN-01 | Docs and compatibility impact | 三处兼容行为 N/A：命令面与 Git 兼容等级不变，只新增错误细节 |

**规则 waiver（`EX-*`，具名审批）：**

| 例外 ID | 任务（或 `ALL/<作用域>`） | 豁免项 | 理由与补偿措施 | Approver | Review round | 证据 | 有效期 |
|---|---|---|---|---|---|---|---|
| EX-MN-01 | MN-01 | G-03 条目上限（门族型验收；AC 列与 Verification 列） | 同一恢复轴「mega2 HTTP 失败的机器可读诊断」上的机械门族：共享发送包装 `run` 的 4 个分支门、2 个结构零命中门、8 个「方法 + 路由」起点门（500）、8 个中段门（读取响应体中断）、8 个末段门（2xx 上的最后一道校验）、4 个结构门（既有行按序保留、新增行不构造或转换错误、每处发送保留传输错误映射、`error.rs` 未改动）与 3 个「`run` 返回原错误」门，共 37 门，每门一个 fixture；本卡 Verification 因结构门各需一条命令而为 10 条，同由本行豁免；按门拆卡会把同一共享 helper 的接入拆散，违反 G-01/G-02。补偿：MN-01「判据规范（非计数正文）— EX-MN-01」逐门列出 `--exact` 命令，任一门失败即整卡不达标；分子写作 `39/8@EX-MN-01`、Verification 写作 `10/8@EX-MN-01`；门族增减时同批更新本行与审计表 | 使用者（本计划请求者） | Codex R2 P1-1 | 2026-10-01 13:18:58 UTC 本会话中使用者选择「批准门族型豁免」；R2 结论见「Codex review log」 | 本计划内 |
| EX-MN-02 | ALL/非交互操作卡（MN-02、MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09） | G-03 条目上限（门族型验收） | 同一恢复轴上的机械门族与 fixture 清单：ADR-MN-08 的原子规则按「操作 × 适用规则」展开为具名用例 `op_rule_<op>_<rule>`（本地拒绝类规则各计两门），各操作卡的本地拒绝与服务端失败 fixture 也各拆成两门（GC-MN-02），MN-11 另有凭据来源矩阵的 4 个 fixture 门；每卡门族 7–25 门；严格按每卡 ≤ 8 拆卡会把同一操作的实现与其规则验收拆散（G-02），并产生约 20 张碎片卡。各卡门族以外的 AC 仍不超过 8 条。补偿：每卡「判据规范（非计数正文）— EX-MN-02」逐门列出 `--exact` 命令，任一门失败即整卡不达标；分子如实写作 `n/8@EX-MN-02`；门族增减时同批更新 ADR-MN-08、本行与审计表 | 使用者（本计划请求者） | Codex R2 P1-1；R5 P1-2（MN-11 并入） | 同上；MN-11 的 R9 规则门与凭据来源 fixture 门属于本行已批准的同一门族（ADR-MN-08 的「操作 × 规则」门与校验 fixture 门），按补偿措施「门族增减时同批更新」于 Codex R5 后并入，使用者 2026-10-01 指示继续修订至 PASS | 本计划内 |
| EX-MN-03 | MN-07 | G-03 条目上限（门族型验收） | live 互通门族：12 个自给自足的 live 门（`live_gate_*`，写操作门以写后的结果状态为判据）与 7 个 mock 驱动的 harness 门（`live_harness_*`），每门一个判据，同属「非交互面的端到端互通证明」一个恢复轴；拆卡会把同一套 live harness 与其用例拆散。补偿：MN-07「判据规范（非计数正文）— EX-MN-03」逐门列出命令；`DEP-MN-04` 降级修订若移除 live 门，同批更新本行、MN-07 与审计表 | 使用者（本计划请求者） | Codex R2 P1-1 | 同上 | 本计划内 |

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| MN-10 | implementation | URL 解析错误的回显边界 | forward-only：保持不回显的前滚修复 patch | yes | yes | 3/8 | 3/8 | 1/1 | S | none | 序列化于 MN-01 之前 | independent | N/A | N/A |
| MN-01 | implementation | mega2 HTTP 失败的机器可读诊断 | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 39/8@EX-MN-01 | 10/8@EX-MN-01 | 1/6 | M | MN-10, DEP-MN-02 | 序列化于 MN-10 之后 | independent | N/A | EX-MN-01 |
| MN-02 | implementation | browser 列表功能的非交互形态与规则 R1–R6 | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 19/8@EX-MN-02 | 8/8 | 2/3 | M | MN-01, DEP-MN-02 | 序列化于 MN-01 之后 | independent | N/A | EX-MN-02 |
| MN-03 | implementation | 建目录功能的非交互形态与规则 R7、R8 | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 21/8@EX-MN-02 | 8/8 | 2/2 | M | MN-02, DEP-MN-01, DEP-MN-02, DEP-MN-06 | 序列化于 MN-02 之后 | independent | N/A | EX-MN-02 |
| MN-11 | implementation | 非交互写操作的凭据（规则 R9a–R9c） | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 10/8@EX-MN-02 | 7/8 | 2/2 | M | MN-03, DEP-MN-01, DEP-MN-02 | 序列化于 MN-03 之后 | independent | MN-03 | EX-MN-02 |
| MN-04 | implementation | 删除目录功能的非交互形态 | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 22/8@EX-MN-02 | 7/8 | 2/2 | M | MN-11, DEP-MN-01, DEP-MN-02 | 序列化于 MN-11 之后 | independent | N/A | EX-MN-02 |
| MN-08 | implementation | 移动目录功能的非交互形态 | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 22/8@EX-MN-02 | 7/8 | 2/2 | M | MN-04, DEP-MN-01, DEP-MN-02 | 序列化于 MN-04 之后 | independent | MN-04 | EX-MN-02 |
| MN-12 | implementation | 改名目录功能的非交互形态 | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 22/8@EX-MN-02 | 7/8 | 2/2 | M | MN-08, DEP-MN-01, DEP-MN-02 | 序列化于 MN-08 之后 | independent | MN-08 | EX-MN-02 |
| MN-05 | implementation | tag 列表与翻页功能的非交互形态与规则 R10a、R10b | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 29/8@EX-MN-02 | 8/8 | 2/2 | M | MN-12, DEP-MN-01, DEP-MN-02 | 序列化于 MN-12 之后 | independent | N/A | EX-MN-02 |
| MN-06 | implementation | 创建 tag 功能的非交互形态 | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 29/8@EX-MN-02 | 7/8 | 2/2 | M | MN-05, DEP-MN-01, DEP-MN-02 | 序列化于 MN-05 之后 | independent | N/A | EX-MN-02 |
| MN-09 | implementation | 删除 tag 功能的非交互形态 | immutable-release：发 revert patch；网站补偿提交 | yes | yes | 22/8@EX-MN-02 | 7/8 | 2/2 | M | MN-06, DEP-MN-01, DEP-MN-02 | 序列化于 MN-06 之后 | independent | MN-06 | EX-MN-02 |
| MN-07 | implementation | 非交互面的端到端互通证明 | immutable-release：发 revert patch；live 残留按 run id 清理 | yes | yes | 22/8@EX-MN-03 | 4/8 | 0/0 | S | MN-09, DEP-MN-01, DEP-MN-04 | 序列化于 MN-09 之后 | independent | N/A | EX-MN-03 |

#### Verification 判定口径

完整继承模板「Verification 判定口径」。本计划仅 MN-02 的网站示例守卫使用零命中判定，按模板的退出码分支写成。各卡 `Verification` 中的 `cargo test` 命令是 A 组 focused 门；`cargo test --test command_test -- <过滤器 1> <过滤器 2>` 一条命令按一门计（libtest 对多个过滤器取并集，见事实基线「libtest 多过滤器」）。`command_test` 中的用例全名形如 `command::<文件名>::<函数名>`，过滤器写成 `<文件名>::<函数名前缀>`，避免与其它文件同名前缀相撞。未命中全量触发的卡在「C 组第 ④ 步（ER-14）」字段逐条写出 nextest 命令：`--test`/`--lib` 选择二进制，位置参数与 `cargo test` 的过滤器相同，整 target 的门（如 `compat_matrix_alignment`）不带过滤器。

### Task MN-10: mega2 server URL 解析失败时不回显原始输入

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** `validate_server_url` 在 URL 无法解析时，把原始 `--server` 字符串写进错误消息（`src/internal/protocol/mega2_tree.rs:88-92`）；这一步早于凭据检查（`:110-115`），所以像 `https://user:secret@host:badport` 这样的输入会把凭据带进 stderr 与 JSON 错误信封，违背三处文档「错误不回显凭据」的承诺。本卡让解析失败的错误消息不再包含原始输入。唯一行为轴：URL 解析错误的回显边界。

**Out of scope:**

- 其它 URL 拒绝规则与它们的文案：保持不变（AC-3）。
- token 文件路径的回显：路径不是凭据，永久保持现状。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 解析失败时回显原始输入 | `src/internal/protocol/mega2_tree.rs:88-92` |
| 凭据检查在解析之后 | `src/internal/protocol/mega2_tree.rs:110-115` |
| 三处文档承诺错误不回显凭据 | `docs/commands/mega2.md:174`；`docs/commands/zh-CN/mega2.md:147`；网站 `mega2.en.md:156` |
| 既有 URL 拒绝用例 | `src/internal/protocol/mega2_tree.rs:648-667`；`tests/command/mega2_browser_cli_test.rs:286` |

**Acceptance criteria:**

- [x] AC-1：无法解析的 `--server`（例如 `https://user:MARKER@mega2.example.com:notaport`）以 `--json` 运行时，stderr（JSON 错误信封的全文）不含 `MARKER`。
- [x] AC-2：该错误的 stable code 仍为 `LBR-CLI-003`。
- [x] AC-3：既有 URL 拒绝用例（`src/internal/protocol/mega2_tree.rs:648-667` 的单元测试与 `tests/command/mega2_browser_cli_test.rs` 中的 URL 用例，覆盖非 https 或非环回 http、带凭据、带 query、带 fragment、带 path）不修改即全部通过。

**Verification:**

- [x] `source .env.test && cargo test --lib internal::protocol::mega2_tree`（含 (new) `url_parse_error_does_not_echo_input`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_cli_test::malformed_server_url_is_not_echoed`（new）
- [x] `source .env.test && cargo test --test command_test mega2_browser_cli`（既有 URL 拒绝用例不回归，证明 AC-3）

**Full-suite trigger:** `T-1: 修改被四个 mega2 客户端共用的 validate_server_url（GC-02 共享 helper）`；C 组第 ④ 步跑全量 nextest。

**Dependencies:** 无。

**Deliverables:** N/A

**Implementation write set:**

- `src/internal/protocol/mega2_tree.rs`
- `tests/command/mega2_browser_cli_test.rs`
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:** N/A：`docs/commands/mega2.md:174`、`docs/commands/zh-CN/mega2.md:147` 与网站 `mega2.en.md:156` 已承诺「错误不回显凭据」，本卡只让实现与之一致，不改文档文本；无网站标记；`COMPATIBILITY.md`、`docs/error-codes.md` 不受影响（stable code 不变）。

**Rollback mode:** `forward-only`（见「默认覆盖」）。不变量：URL 解析失败的错误（含 `--json` 错误信封）不含原始输入。恢复动作：本卡发布后若发现缺陷，发布一个保持该不变量的前滚修复 patch，不发布 revert；恢复验证命令为本卡 Verification 的三条命令；用户影响：只有该错误消息的文本变化，无数据影响。本卡不写网站。

**Migration and rollback:** `N/A`

**Security and privacy:** 本卡就是秘密回显修复；回归用例以 marker 凭据断言零回显。

**Performance budget:** N/A（只改错误消息）。

**Estimated scope:** `S`（落点 1：`src/internal/protocol/`；生产文件 1；仅错误消息文本变化）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=URL 解析错误的回显边界; recovery=发布保持「解析错误不回显原始输入」不变量的前滚修复 patch（forward-only），不 revert; complete=yes; self-contained=yes; AC=3/8; VER=3/8; landing=1; prod-files=1; scope=S; deps=none; writeset=序列化于 MN-01 之前; release=independent; split-from=N/A; exception=N/A`

### Task MN-01: mega2 HTTP 失败的结构化诊断细节

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 为四个 mega2 协议客户端的每个失败出口（HTTP 非 2xx、2xx 但响应体不合规、传输失败）在 `CliError.details` 中附加 `method`、`route`，以及 `http_status` 或 `transport`，使黑盒断言不必解析 message 文本。按 ADR-MN-04，八个发出请求的公开方法的函数体一律放进共享 helper `mega2_diag::run` 的作用域，唯一的一次发送经作用域的 `send` 进行，`details` 只在 `run` 中附加（GC-02）。stable code、message、hint 与退出码全部不变：既有代码行原地保留（只允许缩进变化），`run` 只在原错误上附加 `details`，由 G23–G29 核对。唯一行为轴：mega2 HTTP 失败的机器可读诊断。

**Out of scope:**

- 修改 stable code 映射：尚未排期，`DEFER-MN-06`，重启条件为独立 ADR 被接受。
- 成功响应的 HTTP 状态：尚未排期，`DEFER-MN-05`。
- 回显 `err_message` 或响应体：永久非目标（ADR-MB-01）。
- 非交互操作 flag：由 MN-02 起各卡承接。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 四个客户端共有七处直接的 `.send()` 调用：tree、create-entry 与 mutate 共用的 `post_json`（delete 与 move 都经它发送）各一处，tag 的四个方法各一处；四个客户端目前都不调用 `with_detail` | `src/internal/protocol/mega2_tree.rs:326`；`mega2_entry.rs:173`；`mega2_mutate.rs:376`；`mega2_tag.rs:397,416,426,440` |
| tree 客户端的状态分支、响应体处理与传输错误 | `src/internal/protocol/mega2_tree.rs:326-402` |
| create-entry 的状态分支、响应体处理与传输错误 | `src/internal/protocol/mega2_entry.rs:173-275` |
| delete/move 共用 `post_json`、`parse_common_envelope` 与 `read_bounded`，404 落入 `LBR-NET-002` | `src/internal/protocol/mega2_mutate.rs:222-299,363-380,456-482` |
| tag 的 list/create/get/delete 共用 `finish`、`parse_common_envelope` 与 `read_bounded` | `src/internal/protocol/mega2_tag.rs:224-294,374-382,449-475` |
| 各请求函数在 2xx 响应上的最后一道校验：tree 的 `validate_and_sort`、create-entry 的 `commit_id`/`new_oid` 检查、delete/move 在 `post_json` 之后调用的 `require_commit_id`、tag 的 `finish` 解析 | `src/internal/protocol/mega2_tree.rs:366,415`；`mega2_entry.rs:234`；`mega2_mutate.rs:401,434`；`mega2_tag.rs:374-381` |
| 读取 `HEAD` 中的文件与按名列出改动文件 | `libra show HEAD:<path>`、`libra diff --name-only <rev> -- <paths>`（2026-10-01 实测：未改动时输出为空；未知 revision 以退出码 129 失败）；`libra diff -w` 比较时去掉全部空白（包括字符串内部的空白），不适合 G23（`src/command/diff.rs:4210-4214`） |
| G23 命令的试运行（2026-10-01，作用于 `mega2_tree.rs` 的副本） | 未改动 → OK；把发送行换成作用域调用并只缩进其余行 → OK；改写返回表达式 `validate_and_sort(data.tree_items)`（`mega2_tree.rs:366`）→ FAIL；新增 `with_exit_code` 包装 → FAIL；消息去掉一个空格 → FAIL；G25：发送行换成 `scope.send(…, transport_error)` → OK，换成不带 `transport_error` 的 `scope.send(…)` → FAIL |
| `CliError` 派生 `Clone, PartialEq, Eq`，字段为 `kind`、`stable_code`、`message`、`hints`、`usage`、`details`、`exit_code_override`、`silent`、`report_issue_hint`；`with_detail` 只向 `details` 插入键值 | `src/utils/error.rs:698-714,984-990` |
| `with_detail`、`details()` 访问器与 `details` 序列化 | `src/utils/error.rs:924,984-990,1209-1226` |
| 错误码文档的 JSON Schema 节 | `docs/error-codes.md:415-429` |
| `--json` 时错误 JSON 写 stderr | `src/utils/error.rs:1085-1107`；`src/main.rs:425-445` |

**Acceptance criteria:**

- [x] AC-1：本卡之前已存在的 mega2 用例（`source .env.test && cargo test --test command_test mega2_` 选中者）不修改即全部通过。这些用例主要断言 stable code；message、hint 与退出码的不变由 G23–G29 证明，不归功于它们。
- [x] AC-2：真实二进制以 `--machine` 运行、tree 返回 500 时，stderr JSON 错误信封的 `details` 等于 `{method:"GET", route:"/api/v1/tree", http_status:500}`。
- [x] AC-F（门族，计 37 门，EX-MN-01）：下表 G1–G37 全部通过。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-01，37 门：** 门的划分依据 ADR-MN-04 的结构约束：`details` 只在 `mega2_diag::run` 的作用域里附加——发送失败附 `transport`，收到状态码之后作用域返回的任何错误附 `http_status`，发送之前返回的错误原样返回。G1–G4 各验证 `run` 的一个附加分支，G29 验证发送之前的错误不被改动；G5、G6 证明客户端无法绕过 `run`；G7–G14 在每个「方法 + 路由」组合上验证收到响应之后的起点（状态分支，500）；G30–G37 在同一组合上验证中段（响应头已到、读取响应体中断）；G15–G22 在同一组合上验证终点（该请求函数在 2xx 响应上的最后一道校验；fixture 一律用 201 而非 200，证明附加的是实际状态码、不是写死的 200，也证明 delete/move 在 `post_json` 返回后的 `require_commit_id` 仍在作用域内）；三者共同证明从收到响应到返回结果的各段都在作用域内；G23–G26 证明本卡没有改动任何既有代码行与错误构造、没有新增错误构造或转换、每处发送仍使用本客户端的传输错误映射，且退出码映射未改动；G27、G28 证明 `run` 只在原错误上附加 `details`，返回的仍是原错误（不重建 `CliError`：`kind`、hint 的顺序与条数等全部字段不变）。其余状态分支与响应体校验点都位于起点与终点之间的同一作用域内，不是独立的附加点，因此不逐个设门。每个门只含一个 fixture、一个判据。G23、G24 比较工作区与 `HEAD`，因此必须在本卡改动提交之前运行（A 组与 C 组第 ④ 步都在提交之前，满足这一条件；期间合入上游后 `HEAD` 随之前移，比较对象仍只是本卡的改动）。

| 门 | 判据 | 命令 |
|---|---|---|
| G1 | 作用域内经 `send` 收到状态码 500 后，作用域返回的错误被附加 `details={method, route, http_status:500}` | `source .env.test && cargo test --lib internal::protocol::mega2_diag::tests::run_attaches_status_to_scope_error -- --exact` |
| G2 | 监听端接受连接但不应答、客户端超时 200 ms 时，错误的 `details={method, route, transport:"timeout"}` | `source .env.test && cargo test --lib internal::protocol::mega2_diag::tests::run_classifies_timeout -- --exact` |
| G3 | 连接被拒绝时，错误的 `details={method, route, transport:"connect"}` | `source .env.test && cargo test --lib internal::protocol::mega2_diag::tests::run_classifies_connect -- --exact` |
| G4 | 服务端回送非 HTTP 字节后关闭连接时，错误的 `details={method, route, transport:"request"}` | `source .env.test && cargo test --lib internal::protocol::mega2_diag::tests::run_classifies_malformed_response_as_request -- --exact` |
| G5 | 四个客户端源码中 `.send()` 零命中（零命中判定，按模板的退出码分支） | `if rg -n '\.send\(\)' src/internal/protocol/mega2_tree.rs src/internal/protocol/mega2_entry.rs src/internal/protocol/mega2_mutate.rs src/internal/protocol/mega2_tag.rs; then echo "FAIL: direct send"; exit 1; elif [ $? -ne 1 ]; then echo "ERROR: rg failed"; exit 2; else echo "OK: zero hits"; fi` |
| G6 | 四个客户端源码中 `with_detail` 零命中（零命中判定，按模板的退出码分支） | `if rg -n 'with_detail' src/internal/protocol/mega2_tree.rs src/internal/protocol/mega2_entry.rs src/internal/protocol/mega2_mutate.rs src/internal/protocol/mega2_tag.rs; then echo "FAIL: direct detail"; exit 1; elif [ $? -ne 1 ]; then echo "ERROR: rg failed"; exit 2; else echo "OK: zero hits"; fi` |
| G7 | tree：`GET /api/v1/tree` 返回 500 时，错误的 `details` 等于 `{method:"GET", route:"/api/v1/tree", http_status:500}` | `source .env.test && cargo test --test command_test command::mega2_tree_transport_test::failure_details_tree_route -- --exact` |
| G8 | create-entry：`POST /api/v1/create-entry` 返回 500 时，`details` 等于 `{method:"POST", route:"/api/v1/create-entry", http_status:500}` | `source .env.test && cargo test --test command_test command::mega2_entry_transport_test::failure_details_create_entry_route -- --exact` |
| G9 | delete-entry：`POST /api/v1/delete-entry` 返回 500 时，`details` 等于 `{method:"POST", route:"/api/v1/delete-entry", http_status:500}` | `source .env.test && cargo test --test command_test command::mega2_mutate_transport_test::failure_details_delete_entry_route -- --exact` |
| G10 | move-entry：`POST /api/v1/move-entry` 返回 500 时，`details` 等于 `{method:"POST", route:"/api/v1/move-entry", http_status:500}` | `source .env.test && cargo test --test command_test command::mega2_mutate_transport_test::failure_details_move_entry_route -- --exact` |
| G11 | tag 列表：`GET /api/v1/tags/list` 返回 500 时，`details` 等于 `{method:"GET", route:"/api/v1/tags/list", http_status:500}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_list_tags_route -- --exact` |
| G12 | tag 创建：`POST /api/v1/tags` 返回 500 时，`details` 等于 `{method:"POST", route:"/api/v1/tags", http_status:500}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_create_tag_route -- --exact` |
| G13 | tag 读取：`GET /api/v1/tags/v1` 返回 500 时，`details` 等于 `{method:"GET", route:"/api/v1/tags/{name}", http_status:500}`（路由为模板，不含具体名称） | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_get_tag_route -- --exact` |
| G14 | tag 删除：`DELETE /api/v1/tags/v1` 返回 500 时，`details` 等于 `{method:"DELETE", route:"/api/v1/tags/{name}", http_status:500}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_delete_tag_route -- --exact` |
| G15 | tree 末段：201 响应的 `tree_items` 含不可信名称（`validate_and_sort` 拒绝，`src/internal/protocol/mega2_tree.rs:366,415`）时，`details` 等于 `{method:"GET", route:"/api/v1/tree", http_status:201}` | `source .env.test && cargo test --test command_test command::mega2_tree_transport_test::failure_details_tree_last_stage -- --exact` |
| G16 | create-entry 末段：201 响应的 `commit_id` 为空（`mega2_entry.rs:234` 拒绝）时，`details` 等于 `{method:"POST", route:"/api/v1/create-entry", http_status:201}` | `source .env.test && cargo test --test command_test command::mega2_entry_transport_test::failure_details_create_entry_last_stage -- --exact` |
| G17 | delete-entry 末段：201 响应的 `commit_id` 为空（`require_commit_id`，`mega2_mutate.rs:401`）时，`details` 等于 `{method:"POST", route:"/api/v1/delete-entry", http_status:201}` | `source .env.test && cargo test --test command_test command::mega2_mutate_transport_test::failure_details_delete_entry_last_stage -- --exact` |
| G18 | move-entry 末段：201 响应的 `commit_id` 为空（`require_commit_id`，`mega2_mutate.rs:434`）时，`details` 等于 `{method:"POST", route:"/api/v1/move-entry", http_status:201}` | `source .env.test && cargo test --test command_test command::mega2_mutate_transport_test::failure_details_move_entry_last_stage -- --exact` |
| G19 | tag 列表末段：201 响应的 `data` 不是对象（`finish` 的解析失败，`mega2_tag.rs:374-381`）时，`details` 等于 `{method:"GET", route:"/api/v1/tags/list", http_status:201}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_list_tags_last_stage -- --exact` |
| G20 | tag 创建末段：201 响应的 `data` 不是对象时，`details` 等于 `{method:"POST", route:"/api/v1/tags", http_status:201}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_create_tag_last_stage -- --exact` |
| G21 | tag 读取末段：201 响应的 `data` 不是对象时，`details` 等于 `{method:"GET", route:"/api/v1/tags/{name}", http_status:201}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_get_tag_last_stage -- --exact` |
| G22 | tag 删除末段：201 响应的 `data` 不是对象时，`details` 等于 `{method:"DELETE", route:"/api/v1/tags/{name}", http_status:201}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_delete_tag_last_stage -- --exact` |
| G23 | 四个客户端文件与 `HEAD` 相比（只去掉行首空白、忽略空行）：除含 `.send()` 的行之外，`HEAD` 中的每一行都原样、按原顺序保留 | 见下方「G23 命令」 |
| G24 | 同一比较中的新增行（`//` 注释除外）不含 `CliError`、`StableErrorCode`、`with_hint`、`with_usage`、`with_exit_code`、`with_detail`、`map_err`、`Err(` 或字符串字面量 | 见下方「G24 命令」 |
| G25 | 同一比较中，每个文件删除的 `.send()` 行数等于新增的、同时含 `send(` 与 `transport_error` 的行数（每处发送仍把本客户端的传输错误映射交给作用域） | 见下方「G25 命令」 |
| G26 | `src/utils/error.rs`（stable code 到退出码的映射所在）相对 `HEAD` 无改动 | 见下方「G26 命令」 |
| G27 | 作用域在收到状态码 500 之后返回错误 `e = CliError::fatal("m").with_stable_code(NetworkProtocol).with_hint("h1").with_hint("h2")` 时，`run` 返回的错误 `==` `e.clone().with_detail("method", …).with_detail("route", …).with_detail("http_status", 500)`（`CliError` 派生 `PartialEq`，比较全部字段，含 `kind`、`exit_code_override`、`silent`、`report_issue_hint`；一次断言） | `source .env.test && cargo test --lib internal::protocol::mega2_diag::tests::run_returns_original_scope_error_plus_details -- --exact` |
| G28 | 客户端的传输错误映射返回 `e = CliError::fatal("t").with_stable_code(NetworkUnavailable).with_hint("h")`、连接被拒绝时，`run` 返回的错误 `==` `e.clone().with_detail("method", …).with_detail("route", …).with_detail("transport", "connect")`（同样比较全部字段；一次断言） | `source .env.test && cargo test --lib internal::protocol::mega2_diag::tests::run_returns_original_transport_error_plus_details -- --exact` |
| G29 | 作用域在调用 `send` 之前返回错误 `e`（例如本地校验失败）时，`run` 返回的错误 `==` `e`（全字段相等，不带任何 details 键；一次断言） | `source .env.test && cargo test --lib internal::protocol::mega2_diag::tests::run_leaves_errors_before_send_untouched -- --exact` |
| G30 | tree中段：200 响应头已发出、响应体按 `Content-Length` 只写出一部分就断开连接（读取响应体中断）时，`details` 等于 `{method:"GET", route:"/api/v1/tree", http_status:200}` | `source .env.test && cargo test --test command_test command::mega2_tree_transport_test::failure_details_tree_mid_stream -- --exact` |
| G31 | create-entry中段：200 响应头已发出、响应体按 `Content-Length` 只写出一部分就断开连接（读取响应体中断）时，`details` 等于 `{method:"POST", route:"/api/v1/create-entry", http_status:200}` | `source .env.test && cargo test --test command_test command::mega2_entry_transport_test::failure_details_create_entry_mid_stream -- --exact` |
| G32 | delete-entry中段：200 响应头已发出、响应体按 `Content-Length` 只写出一部分就断开连接（读取响应体中断）时，`details` 等于 `{method:"POST", route:"/api/v1/delete-entry", http_status:200}` | `source .env.test && cargo test --test command_test command::mega2_mutate_transport_test::failure_details_delete_entry_mid_stream -- --exact` |
| G33 | move-entry中段：200 响应头已发出、响应体按 `Content-Length` 只写出一部分就断开连接（读取响应体中断）时，`details` 等于 `{method:"POST", route:"/api/v1/move-entry", http_status:200}` | `source .env.test && cargo test --test command_test command::mega2_mutate_transport_test::failure_details_move_entry_mid_stream -- --exact` |
| G34 | tag 列表中段：200 响应头已发出、响应体按 `Content-Length` 只写出一部分就断开连接（读取响应体中断）时，`details` 等于 `{method:"GET", route:"/api/v1/tags/list", http_status:200}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_list_tags_mid_stream -- --exact` |
| G35 | tag 创建中段：200 响应头已发出、响应体按 `Content-Length` 只写出一部分就断开连接（读取响应体中断）时，`details` 等于 `{method:"POST", route:"/api/v1/tags", http_status:200}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_create_tag_mid_stream -- --exact` |
| G36 | tag 读取中段：200 响应头已发出、响应体按 `Content-Length` 只写出一部分就断开连接（读取响应体中断）时，`details` 等于 `{method:"GET", route:"/api/v1/tags/{name}", http_status:200}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_get_tag_mid_stream -- --exact` |
| G37 | tag 删除中段：200 响应头已发出、响应体按 `Content-Length` 只写出一部分就断开连接（读取响应体中断）时，`details` 等于 `{method:"DELETE", route:"/api/v1/tags/{name}", http_status:200}` | `source .env.test && cargo test --test command_test command::mega2_tag_transport_test::failure_details_delete_tag_mid_stream -- --exact` |

G23 命令（bash；`libra show` 或 `diff` 出错，或 `.send()` 行以外的任何一行在缩进以外被改动、删除或重排，都判失败；不用 `libra diff -w`，因为它比较时去掉全部空白，包括字符串内部的空白）：

```bash
for f in src/internal/protocol/mega2_tree.rs src/internal/protocol/mega2_entry.rs src/internal/protocol/mega2_mutate.rs src/internal/protocol/mega2_tag.rs; do
  old=$(libra show "HEAD:$f") || { echo "ERROR: libra show HEAD:$f failed"; exit 2; }
  d=$(diff <(printf '%s\n' "$old" | sed -E 's/^[[:space:]]+//' | rg -v '^$') <(sed -E 's/^[[:space:]]+//' "$f" | rg -v '^$')); rc=$?
  [ "$rc" -le 1 ] || { echo "ERROR: diff failed in $f"; exit 2; }
  if printf '%s\n' "$d" | rg '^< ' | rg -v -F '.send()' | rg -q .; then echo "FAIL: a line other than .send() was changed or removed in $f"; exit 1; fi
done; echo "OK: existing lines kept in order"
```

G24 命令（bash；新增行构造或转换错误即判失败）：

```bash
for f in src/internal/protocol/mega2_tree.rs src/internal/protocol/mega2_entry.rs src/internal/protocol/mega2_mutate.rs src/internal/protocol/mega2_tag.rs; do
  old=$(libra show "HEAD:$f") || { echo "ERROR: libra show HEAD:$f failed"; exit 2; }
  d=$(diff <(printf '%s\n' "$old" | sed -E 's/^[[:space:]]+//' | rg -v '^$') <(sed -E 's/^[[:space:]]+//' "$f" | rg -v '^$')); rc=$?
  [ "$rc" -le 1 ] || { echo "ERROR: diff failed in $f"; exit 2; }
  if printf '%s\n' "$d" | rg '^> ' | rg -v '^> //' | rg -q -e 'CliError' -e 'StableErrorCode' -e 'with_hint' -e 'with_usage' -e 'with_exit_code' -e 'with_detail' -e 'map_err' -e 'Err\(' -e '"'; then echo "FAIL: an added line constructs or transforms an error in $f"; exit 1; fi
done; echo "OK: no error construction or transformation added"
```

G25 命令（bash；某处发送不再把 `transport_error` 交给作用域即判失败）：

```bash
for f in src/internal/protocol/mega2_tree.rs src/internal/protocol/mega2_entry.rs src/internal/protocol/mega2_mutate.rs src/internal/protocol/mega2_tag.rs; do
  old=$(libra show "HEAD:$f") || { echo "ERROR: libra show HEAD:$f failed"; exit 2; }
  d=$(diff <(printf '%s\n' "$old" | sed -E 's/^[[:space:]]+//' | rg -v '^$') <(sed -E 's/^[[:space:]]+//' "$f" | rg -v '^$')); rc=$?
  [ "$rc" -le 1 ] || { echo "ERROR: diff failed in $f"; exit 2; }
  removed=$(printf '%s\n' "$d" | rg '^< ' | rg -c -F '.send()' || true)
  added=$(printf '%s\n' "$d" | rg '^> ' | rg -F 'send(' | rg -c -F 'transport_error' || true)
  [ "${removed:-0}" = "${added:-0}" ] || { echo "FAIL: $f replaced ${removed:-0} send line(s) but ${added:-0} new send line(s) pass transport_error"; exit 1; }
done; echo "OK: every replaced send keeps the client's transport mapping"
```

G26 命令（bash）：

```bash
n=$(libra diff --name-only HEAD -- src/utils/error.rs) || { echo "ERROR: libra diff failed"; exit 2; }
if [ -z "$n" ]; then echo "OK: error.rs unchanged"; else echo "FAIL: error.rs changed"; exit 1; fi
```

**Verification:**

- [x] `source .env.test && cargo test --lib internal::protocol::mega2_diag`（G1–G4、G27–G29，new）
- [x] G5（零命中判定）：

  ```bash
  if rg -n '\.send\(\)' src/internal/protocol/mega2_tree.rs src/internal/protocol/mega2_entry.rs src/internal/protocol/mega2_mutate.rs src/internal/protocol/mega2_tag.rs; then echo "FAIL: direct send"; exit 1; elif [ $? -ne 1 ]; then echo "ERROR: rg failed"; exit 2; else echo "OK: zero hits"; fi
  ```

- [x] G6（零命中判定）：

  ```bash
  if rg -n 'with_detail' src/internal/protocol/mega2_tree.rs src/internal/protocol/mega2_entry.rs src/internal/protocol/mega2_mutate.rs src/internal/protocol/mega2_tag.rs; then echo "FAIL: direct detail"; exit 1; elif [ $? -ne 1 ]; then echo "ERROR: rg failed"; exit 2; else echo "OK: zero hits"; fi
  ```

- [x] `source .env.test && cargo test --test command_test -- mega2_tree_transport_test::failure_details_ mega2_entry_transport_test::failure_details_ mega2_mutate_transport_test::failure_details_ mega2_tag_transport_test::failure_details_`（G7–G22、G30–G37，new；libtest 对多个过滤器取并集，见事实基线「libtest 多过滤器」）
- [x] G23 命令（见上）
- [x] G24 命令（见上）
- [x] G25 命令（见上）
- [x] G26 命令（见上）
- [x] `source .env.test && cargo test --test command_test mega2_browser_cli_test::json_error_envelope_carries_http_details`（new，AC-2）
- [x] `source .env.test && cargo test --test command_test mega2_`（既有全部 mega2 用例不修改即全绿，AC-1）

**Full-suite trigger:** `T-1: 新建被四个 mega2 协议客户端共用的诊断 helper（GC-02 单一事实源），并修改 docs/error-codes.md`；C 组第 ④ 步跑全量 nextest，并原样重跑 G5、G6、G23、G24、G25、G26 六条结构命令。

**Dependencies:** MN-10（同写 `mega2_tree.rs`）；`DEP-MN-02`（网站文档写入前置）。

**Deliverables:** N/A

**Implementation write set:**

- `src/internal/protocol/mega2_diag.rs`（新）
- `src/internal/protocol/mod.rs`
- `src/internal/protocol/mega2_tree.rs`
- `src/internal/protocol/mega2_entry.rs`
- `src/internal/protocol/mega2_mutate.rs`
- `src/internal/protocol/mega2_tag.rs`
- `tests/command/mega2_tree_transport_test.rs`
- `tests/command/mega2_entry_transport_test.rs`
- `tests/command/mega2_mutate_transport_test.rs`
- `tests/command/mega2_tag_transport_test.rs`
- `tests/command/mega2_browser_cli_test.rs`
- `docs/error-codes.md`
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- 本计划文件与 `docs/development/plan/plan-status.md`（状态与证据）

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- `docs/error-codes.md`：「JSON Schema」节后新增「Command-specific details」小节，写明 `mega2 browser` 的 `method`、`route`、`http_status`、`transport` 四个键的含义、取值与出现条件（`transport` 只出现在收到状态码之前的失败中；收到状态码之后的任何失败，包括读取响应体时断连，都带 `http_status`）；该文件编入二进制（`libra help error-codes`），不新增 stable code，`compat_error_codes_doc_sync` 不受影响。
- `docs/commands/mega2.md`「Errors」节：新增「Machine error details」小表，并写明 stable code 不变。
- `docs/commands/zh-CN/mega2.md`「错误」节：同步。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`Machine error details`。
- `docs/development/commands/mega2.md`「输出与错误」：记录共享 helper 与键契约。
- `COMPATIBILITY.md`、`docs/development/commands/_compatibility.md`、`docs/development/commands/README.md`：N/A，命令面与 Git 兼容等级不变，只新增错误细节。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** `details` 只允许四个白名单键；值来自本地常量与状态码，从不含响应体、token、URL（GC-MN-04）。

**Performance budget:** 构造错误为 O(1)；不新增请求。

**Estimated scope:** `M`（落点 1：`src/internal/protocol/`。模板把「落点」定义为一个具体目录（plan-template.md:81），G-04 只把顶层目录记作一个落点视为规避（:465），因此同一目录下的六个生产文件计 1 个落点；生产文件 6，不超过 M 的 12；一处公开行为变化：错误 `details`）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=mega2 HTTP 失败的机器可读诊断; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，错误信封回到无 details 的自洽状态; complete=yes; self-contained=yes; AC=39/8@EX-MN-01; VER=10/8@EX-MN-01; landing=1; prod-files=6; scope=M; deps=MN-10,DEP-MN-02; writeset=序列化于 MN-10 之后; release=independent; split-from=N/A; exception=EX-MN-01`

### Task MN-02: 非交互调度入口、操作登记表与 `--list`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 在 `mega2 browser` 上引入互斥参数组 `operation`（本卡只有 `--list` 一个成员）、非交互执行路径 `src/command/mega2_browser/noninteractive.rs` 与 ADR-MN-08 的操作登记表（本卡登记 `list`，类别 `read`、`dir`），并引入通用规则 R1–R6 及生成 `op_rule_<op>_<rule>` 用例的测试宏。显式 `--list` 在人读模式下不需要 TTY，以纯文本输出当前层；在 `--json`/`--machine` 下与无操作 flag 的 `--json` 输出同一 payload，并新增 `data.operation:"list"`。同时修正帮助与文档示例中的未 rooted 路径。唯一行为轴：browser 列表功能的非交互形态。

**Out of scope:**

- 写操作与 tag 操作：由 MN-03 起各卡承接。
- 修改 TUI：永久非目标（GC-MN-05）。
- 让 CLI 接受未 rooted 路径：永久非目标，rooted 规则是 ADR-MB-01 的契约；本卡只修示例。
- 递归或分页列表：永久非目标（ADR-MB-01）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 没有 `--list`；人读模式在非 TTY 下被拒 | `src/command/mega2.rs:198-210`；`src/command/mega2_browser/mod.rs:605-613` |
| JSON payload 没有 `operation` | `src/command/mega2.rs:110-118` |
| token flag 与 JSON 同用被拒，消息含「TUI-only」 | `src/command/mega2.rs:181-189` |
| 示例使用未 rooted 路径 | `src/command/mega2.rs:37,48`；`docs/commands/mega2.md:194`；`docs/commands/zh-CN/mega2.md:164`；网站 `mega2.en.md:166` |
| `Cli` 为 `pub(crate)`，crate 内单元测试可直接解析 argv | `src/cli.rs:226-233` |

**Acceptance criteria:**

- [x] AC-1：`--list <PATH> --ref <REF>` 的请求记录等于 `[(GET, /api/v1/tree?path=<规范化 PATH>&refs=<REF>, 无, 空)]`。
- [x] AC-2：人读 `--list` 的 stdout 等于对条目逐行渲染 `<kind>  <name>` 的结果（`kind` 为 `dir` 或 `file`，顺序为 `mega2_tree` 校验排序后的顺序；名称经 `sanitize`）。
- [x] AC-3：`--list --json` 的 `data` 等于 `{operation:"list", server, ref, path, items}`，其中 `server`、`ref`、`path`、`items` 的取值规则与本卡之前无操作 flag 的 `--json` 相同。
- [x] AC-4：对同一 mock，无操作 flag 的 `--json` 与 `--list --json` 的 stdout 逐字节相同。
- [x] AC-5：从 `src/command/mega2.rs` 的两个帮助示例常量（`MEGA2_EXAMPLES`、`MEGA2_BROWSER_EXAMPLES`）抽取全部 `mega2 browser` 示例，经 `crate::cli::Cli::try_parse_from` 解析后，未通过 `normalize_path` 的 PATH 集合为空（一次集合断言）。
- [x] AC-6：从 `docs/commands/mega2.md` 与 `docs/commands/zh-CN/mega2.md` 的代码块抽取全部 `mega2 browser` 示例，经同样解析后，未通过 `normalize_path` 的 PATH 集合为空（一次集合断言）。
- [x] AC-7：`--list --machine` 遇到 mock 返回 500 时 stdout 为空（非交互执行器只在成功后输出；失败由统一错误出口写 stderr，`src/main.rs:425-445`，全部操作共用这一路径）。
- [x] AC-8：从网站页 `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md` 的代码块抽取全部 `mega2 browser` 示例，经同样的 `Cli::try_parse_from` 解析后，未通过 `normalize_path` 的 PATH 集合为空（一次集合断言；单元测试 `site_example_paths_are_rooted` 标为 `#[ignore]`，从环境变量 `LIBRA_SITE_MEGA2_DOC` 读取网站页路径，只在显式包含被忽略的测试时运行；运行时变量未设置或文件不可读即失败，没有跳过分支）。
- [x] AC-F（门族，计 11 门，EX-MN-02）：下表 G1–G11 全部通过（G1–G11 为规则门）。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-02，11 门（规则原文见 ADR-MN-08；调用为 `--list /`，路由 `/api/v1/tree`，方法 `GET`；调用模式见 ADR-MN-08 规则表；本地拒绝与服务端失败的每个 fixture 各拆成两门，见 GC-MN-02）：**

| 门 | 规则或 fixture | 命令 |
|---|---|---|
| G1 | R1 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r1 -- --exact` |
| G2 | R1b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r1b -- --exact` |
| G3 | R2 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r2 -- --exact` |
| G4 | R3 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r3 -- --exact` |
| G5 | R4a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r4a -- --exact` |
| G6 | R4b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r4b -- --exact` |
| G7 | R5a ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r5a_code -- --exact` |
| G8 | R5a ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r5a_no_request -- --exact` |
| G9 | R5b ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r5b_code -- --exact` |
| G10 | R5b ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r5b_no_request -- --exact` |
| G11 | R6 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_r6 -- --exact` |

**Verification:**

- [x] `source .env.test && cargo test --lib command::mega2`（含 (new) `list_payload_carries_operation`、(new) `help_example_paths_are_rooted`（AC-5）、(new) `doc_example_paths_are_rooted`（AC-6）；`site_example_paths_are_rooted` 标为 `#[ignore]`，本命令不运行它，由最后一条命令单独判定 AC-8）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::list_`（new：`list_request_record`、`list_human_output`、`list_json_payload`、`list_bare_json_matches_list_json`、`list_failure_leaves_stdout_empty`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::op_rule_list_r`（规则门 G1–G11，new）
- [x] `source .env.test && cargo test --test compat_help_examples_banner`
- [x] `source .env.test && cargo test --test compat_command_docs_examples_section`
- [x] `source .env.test && cargo test --test compat_matrix_alignment`
- [x] `source .env.test && cargo test --test compat_ledger_schema`
- [x] `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2::tests::site_example_paths_are_rooted -- --exact --ignored`（new，AC-8：网站页每条 `mega2 browser` 示例的 PATH 都 rooted；`--ignored` 使这个被忽略的测试实际运行）

**Full-suite trigger:** `T-4: 新增 tests/command/mega2_browser_noninteractive_test.rs，需要在 tests/command/mod.rs 注册`；C 组第 ④ 步跑全量 nextest（同时覆盖 GC-MN-05 要求的既有 mega2 用例回归），并另跑 `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored only command::mega2::tests::site_example_paths_are_rooted`（全量 nextest 不运行被忽略的测试），证据记录该测试的 PASS 行。

**Dependencies:** MN-01（`details` 契约已发布，G4 依赖它）；`DEP-MN-02`。

**Deliverables:** N/A

**Implementation write set:**

- `src/command/mega2.rs`
- `src/command/mega2_browser/mod.rs`（`pub mod noninteractive;`）
- `src/command/mega2_browser/noninteractive.rs`（新）
- `tests/command/mega2_browser_noninteractive_test.rs`（新）
- `tests/command/mod.rs`
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- `COMPATIBILITY.md`（只改第 248 行）
- `docs/development/commands/_compatibility.md`（只改第 57 行）
- `docs/development/commands/README.md`（只改第 57 行）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- 源码注释：`src/command/mega2.rs:10-15` 的「Two output paths」改为描述 TUI、非交互人读与机器输出三条路径；`src/command/mega2.rs:181` 的「Machine mode is one GET and never writes: token flags are TUI-only.」随调度移入操作登记表，改写为与 ADR-MN-08 类别规则一致的说明；`src/command/mega2.rs:156-160`（`execute_safe` 的 Side Effects）加上「人读 `--list` 也只读一次列表」；`src/command/mega2_browser/mod.rs:1-4` 的模块注释加上非交互执行路径（`noninteractive` 子模块）。
- `src/command/mega2.rs` 中的帮助文本：`browser` 子命令的 about 改为同时说明 TUI 与非交互操作，`MEGA2_EXAMPLES` 与 `MEGA2_BROWSER_EXAMPLES` 加入 `--list` 示例并改用 rooted 路径；全部遵守 GC-MN-06 的禁用子串。
- `docs/commands/mega2.md`：Synopsis 加 `--list`；新增「Non-interactive operations」节，含 ADR-MN-01 的 TUI 键 → flag → HTTP 映射表（本卡先写 `--list` 一行，其余行由各卡追加）、R1–R6、请求计数与输出约定（成功时 stdout 只有 envelope 或人读摘要；失败时 stdout 为空、stderr 为错误信封；默认退出码与 `LIBRA_FINE_EXIT_CODES=1` 两种模式）；把示例改成 rooted 路径。
- `docs/commands/zh-CN/mega2.md`：同步，新增「非交互操作」节。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`Non-interactive operations`。
- `docs/development/commands/mega2.md`：设计方案加入非交互路径与操作登记表；把「`--json` 永远是一次 GET」改为「无操作 flag 的 `--json` 与 `--list` 是一次 GET」。
- `COMPATIBILITY.md:248`、`docs/development/commands/_compatibility.md:57`、`docs/development/commands/README.md:57`：原位补充 `--list` 与非交互路径（GC-MN-07）。
- `docs/error-codes.md`、`tests/INDEX.md`：N/A（无新 stable code；`command_test` 行的描述仍然准确）。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** 读匿名并拒绝 token flag（ADR-MN-05）；不碰终端、不读 stdin（GC-MN-03）；listing 名称已由 `mega2_tree` 校验拒绝控制字符。

**Performance budget:** 每次调用一个 GET，沿用 10 s 超时、1 MiB 响应与 2000 项上限。

**Estimated scope:** `M`（落点 2：`src/command/`、`src/command/mega2_browser/`；生产文件 3；一处公开接口变化：`--list`）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=browser 列表功能的非交互形态与规则 R1–R6; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，回到只有 TUI 与无操作 flag 的 JSON 列表的自洽状态; complete=yes; self-contained=yes; AC=19/8@EX-MN-02; VER=8/8; landing=2; prod-files=3; scope=M; deps=MN-01,DEP-MN-02; writeset=序列化于 MN-01 之后; release=independent; split-from=N/A; exception=EX-MN-02`

### Task MN-03: 非交互建目录 `--create-dir`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 新增操作 `--create-dir <NAME>`（登记类别 `write`、`dir`）：以 PATH 为父目录，经 `Mega2EntryClient::create_directory` 恰好发出一次匿名 `POST /api/v1/create-entry`，输出 `create-dir` 回执；并引入通用规则 R7（操作互斥）与 R8（写操作拒绝 `--ref`）。本卡之后、MN-11 之前，R5a、R5b 继续让全部非交互操作拒绝 token flag，因此 `--create-dir` 只能匿名写（适用于 `push_auth=none`）。拆出 MN-11（写操作凭据）。唯一行为轴：browser 建目录功能的非交互形态。

**Out of scope:**

- 写操作凭据：拆至 MN-11。
- 删除、移动、改名目录：由 MN-04、MN-08、MN-12 承接。
- 建文件（`is_directory=false`）：尚未排期，`DEFER-MN-03`。
- 建目录后 reload：永久非目标（ADR-MN-02）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `create_directory` 已实现单次 POST、请求体与状态映射；500 → `LBR-NET-002` | `src/internal/protocol/mega2_entry.rs:148-246,209-214` |
| 名称校验规则 | `src/internal/protocol/mega2_entry.rs:73-103` |
| TUI 建目录是 POST 加 reload | `src/command/mega2_browser/mod.rs:651-659` |
| mega2 create-entry 契约；重名今日返回 500 | mega2@`8ff880c`：`src/api/router/preview_router.rs:115`；`src/ceres/model/git.rs:13,239`；`docs/refactoring/directory-entry-api.md:64-79,196` |

**Acceptance criteria:**

- [x] AC-1：`--create-dir NAME PATH` 的请求记录等于 `[(POST, /api/v1/create-entry, 无, {is_directory:true, name:NAME, path:<规范化 PATH>, content:null, skip_build:true})]`（请求体没有 `mode`、`author_*` 键）。
- [x] AC-2：成功时 `--json` 的 `data` 等于 `{operation:"create-dir", server, target:{parent, name, path}, receipt:{commit_id, new_oid, path, cl_link}}`，其中 `target` 由本地输入组成，`receipt` 为服务端回执原值（`path`、`cl_link` 可为 `null`）。
- [x] AC-3：人读模式成功时，stdout 等于一行 `created directory <target.path> (commit <commit_id>)`（经 `sanitize`）。
- [x] AC-F（门族，计 18 门，EX-MN-02）：下表 G1–G18 全部通过（G1–G14 为规则门，G15–G18 为本卡的 fixture 门）。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-02，18 门（规则原文见 ADR-MN-08；规则门的调用为 `--create-dir sub /`，路由 `/api/v1/create-entry`，方法 `POST`；调用模式见 ADR-MN-08 规则表；R5a、R5b 的四门在 MN-11 由 R9a–R9c 门取代；本地拒绝与服务端失败的每个 fixture 各拆成两门，见 GC-MN-02）：**

| 门 | 规则或 fixture | 命令 |
|---|---|---|
| G1 | R1 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r1 -- --exact` |
| G2 | R1b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r1b -- --exact` |
| G3 | R2 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r2 -- --exact` |
| G4 | R3 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r3 -- --exact` |
| G5 | R4a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r4a -- --exact` |
| G6 | R4b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r4b -- --exact` |
| G7 | R5a ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r5a_code -- --exact` |
| G8 | R5a ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r5a_no_request -- --exact` |
| G9 | R5b ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r5b_code -- --exact` |
| G10 | R5b ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r5b_no_request -- --exact` |
| G11 | R7 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r7_code -- --exact` |
| G12 | R7 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r7_no_request -- --exact` |
| G13 | R8 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r8_code -- --exact` |
| G14 | R8 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r8_no_request -- --exact` |
| G15 | fixture：NAME 为 `..`（名称校验委托既有的 `validate_entry_name`，`src/internal/protocol/mega2_entry.rs:73-103`） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_dir_rejects_dotdot_name_code -- --exact` |
| G16 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_dir_rejects_dotdot_name_no_request -- --exact` |
| G17 | fixture：服务端对重名目录返回 500（mega2 今日行为） → stderr 错误信封的 `error_code` 为 `LBR-NET-002` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_dir_duplicate_code -- --exact` |
| G18 | 同一 fixture → 错误信封的 `details.http_status` 为 500 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_dir_duplicate_http_status -- --exact` |

**Verification:**

- [x] `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2 -- --include-ignored`（`--include-ignored` 使被忽略的 `site_example_paths_are_rooted` 实际运行，校验本卡写入网站页的示例，GC-MN-09；(new) `create_dir_payload_separates_target_and_receipt`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::create_dir_`（new：非门族用例 `create_dir_request_record`、`create_dir_json_payload`、`create_dir_human_output`；fixture 门 G15–G18：`create_dir_rejects_dotdot_name_*`、`create_dir_duplicate_*`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::op_rule_create_dir_`（规则门 G1–G14，new）
- [x] `source .env.test && cargo test --test command_test -- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`（既有 TUI 与 CLI 用例、token「TUI-only」守卫与 GC-MN-06 的四处帮助守卫不修改即全绿，GC-MN-05）
- [x] `source .env.test && cargo test --test compat_help_examples_banner`
- [x] `source .env.test && cargo test --test compat_command_docs_examples_section`
- [x] `source .env.test && cargo test --test compat_matrix_alignment`
- [x] `source .env.test && cargo test --test compat_ledger_schema`

**C 组第 ④ 步（ER-14）:**

- `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored all command::mega2`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::create_dir_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::op_rule_create_dir_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`
- `source .env.test && cargo nextest run --test compat_help_examples_banner`
- `source .env.test && cargo nextest run --test compat_command_docs_examples_section`
- `source .env.test && cargo nextest run --test compat_matrix_alignment`
- `source .env.test && cargo nextest run --test compat_ledger_schema`

**Full-suite trigger:** `none`（只动本命令的参数、执行模块与本命令测试文件，不命中 T-1..T-6）

**Dependencies:** MN-02（`operation` 参数组、操作登记表、测试宏、`noninteractive.rs` 与 R1–R6）；`DEP-MN-01`；`DEP-MN-02`；`DEP-MN-06`（`src/cli.rs` 注释改动的三态串行）。

**Deliverables:** N/A

**Implementation write set:**

- `src/command/mega2.rs`（含第 3 行模块注释）
- `src/command/mega2_browser/noninteractive.rs`
- `src/cli.rs`（只改写第 2006–2009 行注释，不改代码；`DEP-MN-06`）
- `tests/command/mega2_browser_noninteractive_test.rs`
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- `COMPATIBILITY.md`（只改第 248 行）
- `docs/development/commands/_compatibility.md`（只改第 57 行）
- `docs/development/commands/README.md`（只改第 57 行）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- 源码注释：`src/command/mega2.rs:3` 的「thin, read-only adapter」改为说明带写操作 flag 时会发出一次远端写请求；`src/cli.rs:2006-2009` 中「once the TUI confirms」改为「through the TUI after confirmation or through a non-interactive write flag」（只改注释）；`src/command/mega2.rs:156-160`（`execute_safe` 的 Side Effects）加上「带写操作 flag 时恰好发出一次远端写请求」；PATH 参数的帮助文字（`src/command/mega2.rs:86`，「Rooted directory path to list」）改为同时说明它是目录写操作的父目录。
- `docs/commands/mega2.md`：映射表加 `--create-dir` 行；新增 create-dir 的 payload、人读输出与错误表（写明「重名今日由服务端返回 500，报 `LBR-NET-002`，以 `details.http_status` 区分」）；把「machine mode never POSTs」改为「非交互写操作各发一次写请求；token flag 暂不接受，写操作匿名发送」；写明写请求从不重试，超时或断连后结果未知，应以 `--list` 核对；Options 表加 `--create-dir`；加入 R7、R8。
- `docs/commands/zh-CN/mega2.md`：同步。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`--create-dir`。
- `docs/development/commands/mega2.md`：同步设计与测试清单。
- `COMPATIBILITY.md:248`、`_compatibility.md:57`、`README.md:57`：原位写明非交互建目录（GC-MN-07）。
- `docs/error-codes.md`：N/A（不新增 stable code）。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** 本卡的写请求匿名，不读任何 token 来源；凭据由 MN-11 引入。

**Performance budget:** 一次 POST，10 s 超时，响应 ≤ 1 MiB。

**Estimated scope:** `M`（落点 2：`src/command/`、`src/command/mega2_browser/`；生产文件 2；`src/cli.rs` 只改注释、不承载行为，按 G-04 不计入行为落点与生产文件，但仍列在写集中；一处公开接口变化：`--create-dir`）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=建目录功能的非交互形态与规则 R7、R8; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，回到写操作只在 TUI 的自洽状态; complete=yes; self-contained=yes; AC=21/8@EX-MN-02; VER=8/8; landing=2; prod-files=2; scope=M; deps=MN-02,DEP-MN-01,DEP-MN-02,DEP-MN-06; writeset=序列化于 MN-02 之后; release=independent; split-from=N/A; exception=EX-MN-02`

### Task MN-11: 非交互写操作的凭据

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 自 MN-03 拆出。引入规则 R9a–R9c：非交互写操作在人读与 JSON 模式下都按 `--token-file` → `LIBRA_MEGA2_TOKEN` → `--token` 取至多一个 token，以一个 `Authorization: Bearer` 发送；同时把 R5a、R5b 收窄为只管读操作（删除 `op_rule_create_dir_r5a_*`、`op_rule_create_dir_r5b_*` 四门，生成 `op_rule_create_dir_r9a`/`r9b`/`r9c`），并以真实二进制在 `create_dir` 上验证一次凭据来源矩阵（仅环境变量、仅 flag、人读模式的 token 文件与其 401 不回显）；把读操作拒绝 token flag 的消息改为不再声称「TUI-only」的措辞（`mega2 browser: read operations take no credentials; --token/--token-file only apply to write operations`），并对唯一钉住旧措辞的守卫用例做四行限定替换。唯一行为轴：非交互写操作的凭据。

**Out of scope:**

- OS keyring 或其它凭据来源：永久非目标（延续 plan-20260912 DEFER-MB-03）。
- 读操作带凭据：永久非目标（ADR-MN-05）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| token 解析优先序与 `Mega2Token` 的脱敏 | `src/internal/protocol/mega2_auth.rs:25-53,120-167` |
| 客户端在有 token 时只加一个 `Authorization: Bearer` | `src/internal/protocol/mega2_entry.rs:168-171`；`mega2_mutate.rs:372-375`；`mega2_tag.rs:412-415,436-439` |
| 现有「token 仅 TUI」的拒绝与守卫用例 | `src/command/mega2.rs:181-189`；`tests/command/mega2_browser_mkdir_test.rs:311-350` |

**Acceptance criteria:**

- [x] AC-1：`--json --list / --token secret` 被拒时，stderr JSON 错误信封的 `message` 等于 `mega2 browser: read operations take no credentials; --token/--token-file only apply to write operations`。
- [x] AC-2：`tests/command/mega2_browser_mkdir_test.rs` 逐字节等于 `HEAD` 版本把下面四行整行替换后的内容（一次断言）：第 6 行改为 `//! secret-free status, and the CLI boundary (read operations refuse token`；第 7 行改为 ``//! flags and never echo them, help has no `mkdir` subcommand).``；第 312 行改为 `fn token_flags_are_refused_for_reads_and_never_echoed() {`；第 329 行改为 `    assert!(err.contains("no credentials"), "unexpected stderr: {err}");`。
- [x] AC-3：改写后的用例 `token_flags_are_refused_for_reads_and_never_echoed` 通过。
- [x] AC-F（门族，计 7 门，EX-MN-02）：下表 G1–G7 全部通过。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-02，7 门（规则原文见 ADR-MN-08；规则门的调用为 `--create-dir sub /`，路由 `/api/v1/create-entry`，方法 `POST`；G4–G7 是凭据来源矩阵的 fixture 门，只在本卡以 `create_dir` 经真实二进制验证一次，其它写操作由各自的 R9a–R9c 门证明接到同一条类别凭据路径，ADR-MN-05）：**

| 门 | 规则或 fixture | 命令 |
|---|---|---|
| G1 | R9a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r9a -- --exact` |
| G2 | R9b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r9b -- --exact` |
| G3 | R9c | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_dir_r9c -- --exact` |
| G4 | fixture：只设置 `LIBRA_MEGA2_TOKEN=envtok` 时，`--json --create-dir sub /` 的唯一一条请求的 Authorization 为 `Bearer envtok` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_dir_token_from_env_only -- --exact` |
| G5 | fixture：只给 `--token flagtok` 时，`--json --create-dir sub /` 的唯一一条请求的 Authorization 为 `Bearer flagtok` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_dir_token_from_flag_only -- --exact` |
| G6 | fixture：人读模式 `--create-dir sub / --token-file <文件>`（文件内容为 `filetok`）对成功 mock 的唯一一条请求，Authorization 为 `Bearer filetok` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_dir_token_human_mode_sends_token_file -- --exact` |
| G7 | fixture：人读模式带同一 `--token-file` 且 mock 返回 401 时，stdout 与 stderr 合并输出不含 `filetok` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_dir_token_human_mode_401_never_echoes -- --exact` |

**Verification:**

- [x] `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2 -- --include-ignored`（`--include-ignored` 使被忽略的 `site_example_paths_are_rooted` 实际运行，校验本卡写入网站页的示例，GC-MN-09；(new) `token_gate_accepts_write_operations_only`）
- [x] `source .env.test && cargo test --test command_test -- mega2_browser_noninteractive_test::op_rule_create_dir_r9 mega2_browser_noninteractive_test::create_dir_token_ mega2_browser_noninteractive_test::list_token_refusal_message mega2_browser_noninteractive_test::op_rule_list_r5`（门族 G1–G7 与 AC-1，new；同时回归 MN-02 的 `op_rule_list_r5a_*`、`op_rule_list_r5b_*`，确认 R5 收窄后读操作仍拒绝 token flag）
- [x] 差异判定（AC-2；先确认 `HEAD` 第 6、7、312、329 行是预期的原文，再把这四行整行替换后与工作区文件用 `cmp` 逐字节比较，多出、缺少任何改动或末尾换行不同都判失败）：

  ```bash
  f=tests/command/mega2_browser_mkdir_test.rs
  line() { libra show "HEAD:$f" | sed -n "$1p"; }
  [ "$(line 6)" = '//! secret-free status, and the CLI boundary (token flags are TUI-only,' ] || { echo "ERROR: HEAD line 6 is not the expected original"; exit 2; }
  [ "$(line 7)" = '//! `--json` never POSTs, help has no `mkdir` subcommand).' ] || { echo "ERROR: HEAD line 7 is not the expected original"; exit 2; }
  [ "$(line 312)" = 'fn token_flags_are_tui_only_and_json_never_posts() {' ] || { echo "ERROR: HEAD line 312 is not the expected original"; exit 2; }
  [ "$(line 329)" = '    assert!(err.contains("TUI-only"), "unexpected stderr: {err}");' ] || { echo "ERROR: HEAD line 329 is not the expected original"; exit 2; }
  if libra show "HEAD:$f" | sed -e '6s|.*|//! secret-free status, and the CLI boundary (read operations refuse token|' -e '7s|.*|//! flags and never echo them, help has no `mkdir` subcommand).|' -e '312s|.*|fn token_flags_are_refused_for_reads_and_never_echoed() {|' -e '329s|.*|    assert!(err.contains("no credentials"), "unexpected stderr: {err}");|' | cmp -s - "$f"; then echo "OK: byte-identical to HEAD plus the four specified line replacements"; else echo "FAIL: file is not byte-identical to HEAD plus the four specified line replacements"; exit 1; fi
  ```

- [x] `source .env.test && cargo test --test command_test -- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`（含改写后的用例 AC-3；其余既有 TUI 与 CLI 用例、GC-MN-06 的四处帮助守卫不修改即全绿）
- [x] `source .env.test && cargo test --test compat_command_docs_examples_section`
- [x] `source .env.test && cargo test --test compat_matrix_alignment`
- [x] `source .env.test && cargo test --test compat_ledger_schema`

**C 组第 ④ 步（ER-14）:**

- `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored all command::mega2`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::op_rule_create_dir_r9 mega2_browser_noninteractive_test::create_dir_token_ mega2_browser_noninteractive_test::list_token_refusal_message mega2_browser_noninteractive_test::op_rule_list_r5`
- AC-2 的差异判定（非测试命令，原样执行）：

  ```bash
  f=tests/command/mega2_browser_mkdir_test.rs
  line() { libra show "HEAD:$f" | sed -n "$1p"; }
  [ "$(line 6)" = '//! secret-free status, and the CLI boundary (token flags are TUI-only,' ] || { echo "ERROR: HEAD line 6 is not the expected original"; exit 2; }
  [ "$(line 7)" = '//! `--json` never POSTs, help has no `mkdir` subcommand).' ] || { echo "ERROR: HEAD line 7 is not the expected original"; exit 2; }
  [ "$(line 312)" = 'fn token_flags_are_tui_only_and_json_never_posts() {' ] || { echo "ERROR: HEAD line 312 is not the expected original"; exit 2; }
  [ "$(line 329)" = '    assert!(err.contains("TUI-only"), "unexpected stderr: {err}");' ] || { echo "ERROR: HEAD line 329 is not the expected original"; exit 2; }
  if libra show "HEAD:$f" | sed -e '6s|.*|//! secret-free status, and the CLI boundary (read operations refuse token|' -e '7s|.*|//! flags and never echo them, help has no `mkdir` subcommand).|' -e '312s|.*|fn token_flags_are_refused_for_reads_and_never_echoed() {|' -e '329s|.*|    assert!(err.contains("no credentials"), "unexpected stderr: {err}");|' | cmp -s - "$f"; then echo "OK: byte-identical to HEAD plus the four specified line replacements"; else echo "FAIL: file is not byte-identical to HEAD plus the four specified line replacements"; exit 1; fi
  ```

- `source .env.test && cargo nextest run --test command_test mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`
- `source .env.test && cargo nextest run --test compat_command_docs_examples_section`
- `source .env.test && cargo nextest run --test compat_matrix_alignment`
- `source .env.test && cargo nextest run --test compat_ledger_schema`

**Full-suite trigger:** `none`（只动本命令的参数、执行模块与本命令测试文件，不命中 T-1..T-6）

**Dependencies:** MN-03（第一个写操作与 R5a、R5b 的写分支）；`DEP-MN-01`；`DEP-MN-02`。

**Deliverables:** N/A

**Implementation write set:**

- `src/command/mega2.rs`
- `src/command/mega2_browser/noninteractive.rs`
- `tests/command/mega2_browser_noninteractive_test.rs`
- `tests/command/mega2_browser_mkdir_test.rs`（只做 AC-2 指定的四行替换）
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- `COMPATIBILITY.md`（只改第 248 行）
- `docs/development/commands/_compatibility.md`（只改第 57 行）
- `docs/development/commands/README.md`（只改第 57 行）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- 源码注释：`src/command/mega2.rs:156-160`（`execute_safe` 的 Side Effects）中「no network request carries credentials」改为「读请求不带凭据；写请求至多带一个按 `--token-file` → `LIBRA_MEGA2_TOKEN` → `--token` 取得的 Bearer token」。
- `docs/commands/mega2.md`：把「Token flags are TUI-only」一段改写为 ADR-MN-05：读操作拒绝 token flag，写操作（TUI 与非交互）按优先序取 token；继续警告 `--token` 会留在 shell history；EXAMPLES 加一条带 `--token-file` 的非交互写示例；加入 R9a–R9c。
- `docs/commands/zh-CN/mega2.md`：同步。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`Read operations refuse token flags`。
- `docs/development/commands/mega2.md`：把「token 只在 TUI 解析」改为 ADR-MN-05。
- `COMPATIBILITY.md:248`、`_compatibility.md:57`、`README.md:57`：原位改写 token 规则（GC-MN-07）。
- `docs/error-codes.md`：N/A。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** token 只进 `Authorization` 头，不持久化、不回显（`Mega2Token` 的 `Debug`/`Display` 已脱敏）；`--token` 的 shell history 风险在文档中警告（GC-MN-04）。

**Performance budget:** N/A（不新增请求）。

**Estimated scope:** `M`（落点 2；生产文件 2；一处公开接口变化：非交互写操作接受 token flag）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=非交互写操作的凭据（规则 R9a–R9c）; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，回到非交互写操作只能匿名的自洽状态; complete=yes; self-contained=yes; AC=10/8@EX-MN-02; VER=7/8; landing=2; prod-files=2; scope=M; deps=MN-03,DEP-MN-01,DEP-MN-02; writeset=序列化于 MN-03 之后; release=independent; split-from=MN-03; exception=EX-MN-02`

### Task MN-04: 非交互删除目录 `--delete-dir`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 新增 `--delete-dir <NAME>`（登记类别 `write`、`dir`）：经 `Mega2MutateClient::delete_directory`（`known_type` 传 `None`）恰好发出一次 `POST /api/v1/delete-entry`，请求体与 TUI 相同（省略 `is_directory`，服务端缺省即视为目录）。拆出 MN-08（移动目录）。唯一行为轴：browser 删除目录功能的非交互形态。

**Out of scope:**

- 移动与改名目录：拆至 MN-08、MN-12。
- 删除文件条目（`is_directory=false`）：尚未排期，`DEFER-MN-03`。
- 客户端 preflight 类型检查：永久非目标（ADR-MN-02）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| delete 客户端 | `src/internal/protocol/mega2_mutate.rs:387-407` |
| 请求体省略 `is_directory`/`author_username`，`skip_build:true` | `src/internal/protocol/mega2_mutate.rs:79-84,146-152` |
| 状态映射：400 → `LBR-CLI-003`，404 落入 `LBR-NET-002` | `src/internal/protocol/mega2_mutate.rs:222-286` |
| TUI 的 `d` 是 POST 加 reload，`known_type=Some(Directory)` | `src/command/mega2_browser/mod.rs:661-673` |
| mega2 按 mode 匹配：文件目标 400，缺失目标 404 | mega2@`8ff880c`：`src/ceres/api_service/mono_api_service.rs:1438-1490`；`docs/refactoring/directory-entry-api.md:192-224` |
| mega2 路由与 DTO | mega2@`8ff880c`：`preview_router.rs:173`；`src/ceres/model/git.rs:316,343` |

**Acceptance criteria:**

- [x] AC-1：`--delete-dir NAME PATH` 的请求记录等于 `[(POST, /api/v1/delete-entry, 无, {path:<规范化 PATH>, name:NAME, skip_build:true})]`（请求体没有 `is_directory`、`author_username` 键）。
- [x] AC-2：成功时 `--json` 的 `data` 等于 `{operation:"delete-dir", server, target:{parent, name, path}, receipt:{commit_id, path, cl_link}}`。
- [x] AC-3：人读模式成功时，stdout 等于一行 `deleted directory <target.path> (commit <commit_id>)`（经 `sanitize`）。
- [x] AC-F（门族，计 19 门，EX-MN-02）：下表 G1–G19 全部通过（G1–G13 为规则门，G14–G19 为本卡的 fixture 门）。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-02，19 门（规则原文见 ADR-MN-08；规则门的调用为 `--delete-dir sub /`，路由 `/api/v1/delete-entry`，方法 `POST`；调用模式见 ADR-MN-08 规则表；本地拒绝与服务端失败的每个 fixture 各拆成两门，见 GC-MN-02）：**

| 门 | 规则或 fixture | 命令 |
|---|---|---|
| G1 | R1 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r1 -- --exact` |
| G2 | R1b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r1b -- --exact` |
| G3 | R2 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r2 -- --exact` |
| G4 | R3 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r3 -- --exact` |
| G5 | R4a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r4a -- --exact` |
| G6 | R4b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r4b -- --exact` |
| G7 | R7 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r7_code -- --exact` |
| G8 | R7 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r7_no_request -- --exact` |
| G9 | R8 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r8_code -- --exact` |
| G10 | R8 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r8_no_request -- --exact` |
| G11 | R9a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r9a -- --exact` |
| G12 | R9b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r9b -- --exact` |
| G13 | R9c | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_dir_r9c -- --exact` |
| G14 | fixture：NAME 为 `..`（委托 `validate_entry_name`） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_dir_rejects_dotdot_name_code -- --exact` |
| G15 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_dir_rejects_dotdot_name_no_request -- --exact` |
| G16 | fixture：目标是文件，服务端返回 400 → stderr 错误信封的 `error_code` 为 `LBR-CLI-003` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_dir_file_target_code -- --exact` |
| G17 | 同一 fixture → 错误信封的 `details.http_status` 为 400 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_dir_file_target_http_status -- --exact` |
| G18 | fixture：目标不存在，服务端返回 404 → stderr 错误信封的 `error_code` 为 `LBR-NET-002` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_dir_missing_target_code -- --exact` |
| G19 | 同一 fixture → 错误信封的 `details.http_status` 为 404 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_dir_missing_target_http_status -- --exact` |

**Verification:**

- [x] `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2 -- --include-ignored`（`--include-ignored` 使被忽略的 `site_example_paths_are_rooted` 实际运行，校验本卡写入网站页的示例，GC-MN-09；(new) `delete_dir_payload_separates_target_and_receipt`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::delete_dir_`（new：非门族用例 `delete_dir_request_record`、`delete_dir_json_payload`、`delete_dir_human_output`；fixture 门 G14–G19：`delete_dir_rejects_dotdot_name_*`、`delete_dir_file_target_*`、`delete_dir_missing_target_*`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::op_rule_delete_dir_`（规则门 G1–G13，new）
- [x] `source .env.test && cargo test --test command_test -- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`（既有 TUI 与 CLI 用例、GC-MN-06 的四处帮助守卫不修改即全绿，GC-MN-05）
- [x] `source .env.test && cargo test --test compat_command_docs_examples_section`
- [x] `source .env.test && cargo test --test compat_matrix_alignment`
- [x] `source .env.test && cargo test --test compat_ledger_schema`

**C 组第 ④ 步（ER-14）:**

- `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored all command::mega2`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::delete_dir_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::op_rule_delete_dir_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`
- `source .env.test && cargo nextest run --test compat_command_docs_examples_section`
- `source .env.test && cargo nextest run --test compat_matrix_alignment`
- `source .env.test && cargo nextest run --test compat_ledger_schema`

**Full-suite trigger:** `none`（只动本命令的参数、执行模块与本命令测试文件，不命中 T-1..T-6）

**Dependencies:** MN-11（写操作类规则 R7、R8、R9a–R9c 已落地）；`DEP-MN-01`；`DEP-MN-02`。

**Deliverables:** N/A

**Implementation write set:**

- `src/command/mega2.rs`
- `src/command/mega2_browser/noninteractive.rs`
- `tests/command/mega2_browser_noninteractive_test.rs`
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- `COMPATIBILITY.md`（只改第 248 行）
- `docs/development/commands/_compatibility.md`（只改第 57 行）
- `docs/development/commands/README.md`（只改第 57 行）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- `docs/commands/mega2.md`：映射表加 `--delete-dir` 行；新增 payload、人读输出与错误表（写明「文件目标与缺失目标由服务端回答；缺失目标目前是 `LBR-NET-002`，以 `details.http_status` 区分」）；Options 表加 `--delete-dir`。
- `docs/commands/zh-CN/mega2.md`：同步。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`--delete-dir`。
- `docs/development/commands/mega2.md`：同步设计与测试清单。
- `COMPATIBILITY.md:248`、`_compatibility.md:57`、`README.md:57`：原位改写（GC-MN-07）。
- `docs/error-codes.md`：N/A。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** 删除没有额外确认，这是有意决定（ADR-MN-02）：flag 携带确切目标，服务端按 mode 拒绝文件目标。

**Performance budget:** 一次 POST，10 s 超时。

**Estimated scope:** `M`（落点 2；生产文件 2；一处公开接口变化：`--delete-dir`）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=删除目录功能的非交互形态; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，回到只有非交互建目录的自洽状态; complete=yes; self-contained=yes; AC=22/8@EX-MN-02; VER=7/8; landing=2; prod-files=2; scope=M; deps=MN-11,DEP-MN-01,DEP-MN-02; writeset=序列化于 MN-11 之后; release=independent; split-from=N/A; exception=EX-MN-02`

### Task MN-08: 非交互移动目录 `--move-dir`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 自 MN-04 拆出。新增双值参数 `--move-dir <NAME> <PARENT-PATH>`（登记类别 `write`、`dir`）：经 `Mega2MutateClient::move_entry`（`known_type` 传 `None`）恰好发出一次 `POST /api/v1/move-entry`，名称不变、父目录改为 `<PARENT-PATH>`。拆出 MN-12（改名目录）。唯一行为轴：browser 移动目录功能的非交互形态。

**Out of scope:**

- 改名目录：拆至 MN-12。
- 一次请求同时换父目录与改名：尚未排期，`DEFER-MN-04`（TUI 不具备此功能）。
- 移动文件条目（`is_directory=false`）：尚未排期，`DEFER-MN-03`。
- 客户端 preflight：永久非目标（ADR-MN-02）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| move 客户端 | `src/internal/protocol/mega2_mutate.rs:411-441` |
| 请求体省略 `is_directory`/`author_username`，`skip_build:true` | `src/internal/protocol/mega2_mutate.rs:86-93,154-168` |
| TUI 的 `m` 是 POST 加 reload；目标父目录以 `normalize_path` 校验 | `src/command/mega2_browser/mod.rs:244-276,675-695` |
| mega2 move 契约：目标名已存在 400；跨顶层目录在 trunk 上 400 | mega2@`8ff880c`：`preview_router.rs:201`；`src/ceres/model/git.rs:356,402`；`docs/refactoring/directory-entry-api.md:153,192-224` |

**Acceptance criteria:**

- [x] AC-1：`--move-dir NAME P PATH` 的请求记录等于 `[(POST, /api/v1/move-entry, 无, {from_path:<规范化 PATH>, from_name:NAME, to_path:<规范化 P>, to_name:NAME, skip_build:true})]`。
- [x] AC-2：成功时 `--json` 的 `data` 等于 `{operation:"move-dir", server, target:{from:{parent, name, path}, to:{parent, name, path}}, receipt:{commit_id, from_path, to_path, cl_link}}`。
- [x] AC-3：人读模式成功时，stdout 等于一行 `moved directory <from> -> <to> (commit <commit_id>)`（经 `sanitize`）。
- [x] AC-F（门族，计 19 门，EX-MN-02）：下表 G1–G19 全部通过（G1–G13 为规则门，G14–G19 为本卡的 fixture 门）。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-02，19 门（规则原文见 ADR-MN-08；规则门的调用为 `--move-dir a /b /`，路由 `/api/v1/move-entry`，方法 `POST`；调用模式见 ADR-MN-08 规则表；本地拒绝与服务端失败的每个 fixture 各拆成两门，见 GC-MN-02）：**

| 门 | 规则或 fixture | 命令 |
|---|---|---|
| G1 | R1 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r1 -- --exact` |
| G2 | R1b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r1b -- --exact` |
| G3 | R2 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r2 -- --exact` |
| G4 | R3 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r3 -- --exact` |
| G5 | R4a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r4a -- --exact` |
| G6 | R4b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r4b -- --exact` |
| G7 | R7 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r7_code -- --exact` |
| G8 | R7 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r7_no_request -- --exact` |
| G9 | R8 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r8_code -- --exact` |
| G10 | R8 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r8_no_request -- --exact` |
| G11 | R9a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r9a -- --exact` |
| G12 | R9b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r9b -- --exact` |
| G13 | R9c | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_move_dir_r9c -- --exact` |
| G14 | fixture：`<PARENT-PATH>` 为未 rooted 的 `rel/dir`（路径校验委托既有的 `normalize_path`，`src/internal/protocol/mega2_tree.rs:164-170`） → 进程以 `LBR-CLI-003` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::move_dir_rejects_unrooted_parent_code -- --exact` |
| G15 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::move_dir_rejects_unrooted_parent_no_request -- --exact` |
| G16 | fixture：NAME 为 `..`（委托 `validate_entry_name`） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::move_dir_rejects_dotdot_name_code -- --exact` |
| G17 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::move_dir_rejects_dotdot_name_no_request -- --exact` |
| G18 | fixture：目标父目录下已有同名条目，服务端返回 400 → stderr 错误信封的 `error_code` 为 `LBR-CLI-003` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::move_dir_existing_destination_code -- --exact` |
| G19 | 同一 fixture → 错误信封的 `details.http_status` 为 400 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::move_dir_existing_destination_http_status -- --exact` |

**Verification:**

- [x] `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2 -- --include-ignored`（`--include-ignored` 使被忽略的 `site_example_paths_are_rooted` 实际运行，校验本卡写入网站页的示例，GC-MN-09；(new) `move_payload_separates_target_and_receipt`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::move_dir_`（new：非门族用例 `move_dir_request_record`、`move_dir_json_payload`、`move_dir_human_output`；fixture 门 G14–G19：`move_dir_rejects_unrooted_parent_*`、`move_dir_rejects_dotdot_name_*`、`move_dir_existing_destination_*`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::op_rule_move_dir_`（规则门 G1–G13，new）
- [x] `source .env.test && cargo test --test command_test -- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`（既有 TUI 与 CLI 用例、GC-MN-06 的四处帮助守卫不修改即全绿，GC-MN-05）
- [x] `source .env.test && cargo test --test compat_command_docs_examples_section`
- [x] `source .env.test && cargo test --test compat_matrix_alignment`
- [x] `source .env.test && cargo test --test compat_ledger_schema`

**C 组第 ④ 步（ER-14）:**

- `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored all command::mega2`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::move_dir_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::op_rule_move_dir_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`
- `source .env.test && cargo nextest run --test compat_command_docs_examples_section`
- `source .env.test && cargo nextest run --test compat_matrix_alignment`
- `source .env.test && cargo nextest run --test compat_ledger_schema`

**Full-suite trigger:** `none`（只动本命令的参数、执行模块与本命令测试文件，不命中 T-1..T-6）

**Dependencies:** MN-04（与本卡共享 `noninteractive.rs`、`mega2.rs` 与文档写集，按实施顺序串行）；`DEP-MN-01`；`DEP-MN-02`。

**Deliverables:** N/A

**Implementation write set:**

- `src/command/mega2.rs`
- `src/command/mega2_browser/noninteractive.rs`
- `tests/command/mega2_browser_noninteractive_test.rs`
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- `COMPATIBILITY.md`（只改第 248 行）
- `docs/development/commands/_compatibility.md`（只改第 57 行）
- `docs/development/commands/README.md`（只改第 57 行）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- `docs/commands/mega2.md`：映射表加 `--move-dir` 行；新增 payload、人读输出与错误表（含跨顶层目录在 trunk 上被拒的说明）；Options 表加 `--move-dir <NAME> <PARENT-PATH>`。
- `docs/commands/zh-CN/mega2.md`：同步。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`--move-dir`。
- `docs/development/commands/mega2.md`：同步设计与测试清单。
- `COMPATIBILITY.md:248`、`_compatibility.md:57`、`README.md:57`：原位改写（GC-MN-07）。
- `docs/error-codes.md`：N/A。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** move 对源与目标父目录各鉴权一次由服务端完成；客户端只发一次请求，凭据按 R9a–R9c。

**Performance budget:** 一次 POST，10 s 超时。

**Estimated scope:** `M`（落点 2；生产文件 2；一处公开接口变化：`--move-dir`）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=移动目录功能的非交互形态; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，回到只有非交互建目录与删除目录的自洽状态; complete=yes; self-contained=yes; AC=22/8@EX-MN-02; VER=7/8; landing=2; prod-files=2; scope=M; deps=MN-04,DEP-MN-01,DEP-MN-02; writeset=序列化于 MN-04 之后; release=independent; split-from=MN-04; exception=EX-MN-02`

### Task MN-12: 非交互改名目录 `--rename-dir`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 自 MN-08 拆出。新增双值参数 `--rename-dir <NAME> <NEW-NAME>`（登记类别 `write`、`dir`）：经 `Mega2MutateClient::move_entry`（`known_type` 传 `None`）恰好发出一次同父目录的 `POST /api/v1/move-entry`。唯一行为轴：browser 改名目录功能的非交互形态。

**Out of scope:**

- 一次请求同时换父目录与改名：尚未排期，`DEFER-MN-04`。
- 改名文件条目：尚未排期，`DEFER-MN-03`。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| rename 即同父 move | `src/internal/protocol/mega2_mutate.rs:443-453` |
| TUI 的 `R` 以 `validate_entry_name` 校验新名，是 POST 加 reload | `src/command/mega2_browser/mod.rs:277-309,675-695` |
| mega2：源与目标相同 400 | mega2@`8ff880c`：`docs/refactoring/directory-entry-api.md:192-224` |

**Acceptance criteria:**

- [x] AC-1：`--rename-dir NAME N PATH` 的请求记录等于 `[(POST, /api/v1/move-entry, 无, {from_path:<规范化 PATH>, from_name:NAME, to_path:<规范化 PATH>, to_name:N, skip_build:true})]`。
- [x] AC-2：成功时 `--json` 的 `data` 等于 `{operation:"rename-dir", server, target:{from:{parent, name, path}, to:{parent, name, path}}, receipt:{commit_id, from_path, to_path, cl_link}}`，其中 `target.to.parent` 取 `target.from.parent` 的值。
- [x] AC-3：人读模式成功时，stdout 等于一行 `renamed directory <from> -> <to> (commit <commit_id>)`（经 `sanitize`）。
- [x] AC-F（门族，计 19 门，EX-MN-02）：下表 G1–G19 全部通过（G1–G13 为规则门，G14–G19 为本卡的 fixture 门）。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-02，19 门（规则原文见 ADR-MN-08；规则门的调用为 `--rename-dir a b /`，路由 `/api/v1/move-entry`，方法 `POST`；调用模式见 ADR-MN-08 规则表；本地拒绝与服务端失败的每个 fixture 各拆成两门，见 GC-MN-02）：**

| 门 | 规则或 fixture | 命令 |
|---|---|---|
| G1 | R1 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r1 -- --exact` |
| G2 | R1b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r1b -- --exact` |
| G3 | R2 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r2 -- --exact` |
| G4 | R3 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r3 -- --exact` |
| G5 | R4a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r4a -- --exact` |
| G6 | R4b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r4b -- --exact` |
| G7 | R7 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r7_code -- --exact` |
| G8 | R7 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r7_no_request -- --exact` |
| G9 | R8 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r8_code -- --exact` |
| G10 | R8 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r8_no_request -- --exact` |
| G11 | R9a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r9a -- --exact` |
| G12 | R9b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r9b -- --exact` |
| G13 | R9c | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_rename_dir_r9c -- --exact` |
| G14 | fixture：NAME 为 `..`（委托 `validate_entry_name`） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::rename_dir_rejects_dotdot_name_code -- --exact` |
| G15 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::rename_dir_rejects_dotdot_name_no_request -- --exact` |
| G16 | fixture：NEW-NAME 为 `..`（委托 `validate_entry_name`） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::rename_dir_rejects_dotdot_new_name_code -- --exact` |
| G17 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::rename_dir_rejects_dotdot_new_name_no_request -- --exact` |
| G18 | fixture：NEW-NAME 与 NAME 相同，服务端返回 400 → stderr 错误信封的 `error_code` 为 `LBR-CLI-003` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::rename_dir_same_name_code -- --exact` |
| G19 | 同一 fixture → 错误信封的 `details.http_status` 为 400 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::rename_dir_same_name_http_status -- --exact` |

**Verification:**

- [x] `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2 -- --include-ignored`（`--include-ignored` 使被忽略的 `site_example_paths_are_rooted` 实际运行，校验本卡写入网站页的示例，GC-MN-09；(new) `rename_payload_keeps_parent`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::rename_dir_`（new：非门族用例 `rename_dir_request_record`、`rename_dir_json_payload`、`rename_dir_human_output`；fixture 门 G14–G19：`rename_dir_rejects_dotdot_name_*`、`rename_dir_rejects_dotdot_new_name_*`、`rename_dir_same_name_*`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::op_rule_rename_dir_`（规则门 G1–G13，new）
- [x] `source .env.test && cargo test --test command_test -- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`（既有 TUI 与 CLI 用例、GC-MN-06 的四处帮助守卫不修改即全绿，GC-MN-05）
- [x] `source .env.test && cargo test --test compat_command_docs_examples_section`
- [x] `source .env.test && cargo test --test compat_matrix_alignment`
- [x] `source .env.test && cargo test --test compat_ledger_schema`

**C 组第 ④ 步（ER-14）:**

- `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored all command::mega2`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::rename_dir_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::op_rule_rename_dir_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`
- `source .env.test && cargo nextest run --test compat_command_docs_examples_section`
- `source .env.test && cargo nextest run --test compat_matrix_alignment`
- `source .env.test && cargo nextest run --test compat_ledger_schema`

**Full-suite trigger:** `none`（只动本命令的参数、执行模块与本命令测试文件，不命中 T-1..T-6）

**Dependencies:** MN-08（与本卡共享 `noninteractive.rs`、`mega2.rs` 与文档写集，按实施顺序串行）；`DEP-MN-01`；`DEP-MN-02`。

**Deliverables:** N/A

**Implementation write set:**

- `src/command/mega2.rs`
- `src/command/mega2_browser/noninteractive.rs`
- `tests/command/mega2_browser_noninteractive_test.rs`
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- `COMPATIBILITY.md`（只改第 248 行）
- `docs/development/commands/_compatibility.md`（只改第 57 行）
- `docs/development/commands/README.md`（只改第 57 行）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- `docs/commands/mega2.md`：映射表加 `--rename-dir` 行；新增 payload、人读输出与错误表；Options 表加 `--rename-dir <NAME> <NEW-NAME>`。
- `docs/commands/zh-CN/mega2.md`：同步。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`--rename-dir`。
- `docs/development/commands/mega2.md`：同步设计与测试清单。
- `COMPATIBILITY.md:248`、`_compatibility.md:57`、`README.md:57`：原位改写（GC-MN-07）。
- `docs/error-codes.md`：N/A。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** 凭据按 R9a–R9c；不回显响应体。

**Performance budget:** 一次 POST，10 s 超时。

**Estimated scope:** `M`（落点 2；生产文件 2；一处公开接口变化：`--rename-dir`）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=改名目录功能的非交互形态; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，回到只有非交互建、删、移目录的自洽状态; complete=yes; self-contained=yes; AC=22/8@EX-MN-02; VER=7/8; landing=2; prod-files=2; scope=M; deps=MN-08,DEP-MN-01,DEP-MN-02; writeset=序列化于 MN-08 之后; release=independent; split-from=MN-08; exception=EX-MN-02`

### Task MN-05: 非交互 tag 列表 `--list-tags`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 新增只读操作 `--list-tags [--page <N>] [--per-page <N>]`（登记类别 `read`、`tag`）：经 `Mega2TagClient::list_tags` 恰好发出一次匿名 `GET /api/v1/tags/list?page=&per_page=&path=/`，默认 page 1、per_page 20（与 TUI 面板一致），输出带分页信息的 `list-tags` payload；并引入 tag 类规则 R10a、R10b。唯一行为轴：browser tag 面板浏览与翻页功能的非交互形态。

**Out of scope:**

- 非根 path selector：尚未排期，`DEFER-MN-02`。
- 按名称取单个 tag（`GET /api/v1/tags/{name}`）：尚未排期，`DEFER-MN-01`。
- 一次调用自动翻完所有页：永久非目标（ADR-MN-02，一次调用一次请求）。
- 修正 mega2 的 lightweight tag 分页行为：属于 mega2 侧（经 `DEP-MN-03` 转告）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `list_tags` 是单次匿名 GET，带三个必填 query 键 | `src/internal/protocol/mega2_tag.rs:385-403` |
| 分页上限：page 1..=1000，per_page 1..=100 | `src/internal/protocol/mega2_tag.rs:43-47,194-209` |
| TUI 面板页大小 20，root-only | `src/command/mega2_browser/tag_panel.rs:13-14`；`src/command/mega2_browser/mod.rs:698-708` |
| `TagInfo` 七个字段 | `src/internal/protocol/mega2_tag.rs:122-132` |
| 渲染消毒函数 | `src/command/mega2_browser/mod.rs:490-494`；`tag_panel.rs:257-307` |
| mega2 tags/list 契约与分页行为 | mega2@`8ff880c`：`src/api/router/tag_router.rs:219`；`src/ceres/model/tag.rs:37,60`；`src/ceres/api_service/mono_api_service.rs:1906-1965` |

**Acceptance criteria:**

- [x] AC-1：`--list-tags --page 3 --per-page 50` 的请求记录等于 `[(GET, /api/v1/tags/list?page=3&per_page=50&path=%2F, 无, 空)]`。
- [x] AC-2：省略 `--page` 与 `--per-page` 时，请求记录等于 `[(GET, /api/v1/tags/list?page=1&per_page=20&path=%2F, 无, 空)]`。
- [x] AC-3：`--json` 的 `data` 等于 `{operation:"list-tags", server, path:"/", page, per_page, total, has_next, items}`，其中 `has_next` 取 `page*per_page < total` 的值，`items` 每项保留 `name`、`tag_id`、`object_id`、`object_type`、`tagger`、`message`、`created_at` 的原值。
- [x] AC-4：对固定 fixture（两个 tag，其中一个的 `tagger` 与 `message` 含 ESC 与 BEL），人读 stdout 等于用例中的期望文本。期望文本的格式：每个 tag 一行 `<name>  <object_type>  <tagger>`，message 非空时下一行以四个空格缩进输出 message，末行为 `page <page> · per_page <per_page> · total <total>`，服务端字符串经 `sanitize`（控制字符渲染为 `?`）。
- [x] AC-F（门族，计 25 门，EX-MN-02）：下表 G1–G25 全部通过（G1–G17 为规则门，G18–G25 为本卡的 fixture 门）。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-02，25 门（规则原文见 ADR-MN-08；规则门的调用为 `--list-tags`，路由 `/api/v1/tags/list`，方法 `GET`；调用模式见 ADR-MN-08 规则表；本地拒绝与服务端失败的每个 fixture 各拆成两门，见 GC-MN-02）：**

| 门 | 规则或 fixture | 命令 |
|---|---|---|
| G1 | R1 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r1 -- --exact` |
| G2 | R1b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r1b -- --exact` |
| G3 | R2 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r2 -- --exact` |
| G4 | R3 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r3 -- --exact` |
| G5 | R4a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r4a -- --exact` |
| G6 | R4b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r4b -- --exact` |
| G7 | R5a ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r5a_code -- --exact` |
| G8 | R5a ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r5a_no_request -- --exact` |
| G9 | R5b ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r5b_code -- --exact` |
| G10 | R5b ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r5b_no_request -- --exact` |
| G11 | R6 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r6 -- --exact` |
| G12 | R7 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r7_code -- --exact` |
| G13 | R7 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r7_no_request -- --exact` |
| G14 | R10a ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r10a_code -- --exact` |
| G15 | R10a ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r10a_no_request -- --exact` |
| G16 | R10b ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r10b_code -- --exact` |
| G17 | R10b ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_list_tags_r10b_no_request -- --exact` |
| G18 | fixture：`--page 0`（分页校验委托既有的 `validate_pagination`，`src/internal/protocol/mega2_tag.rs:194-209`，page 1..=1000） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::list_tags_rejects_page_zero_code -- --exact` |
| G19 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::list_tags_rejects_page_zero_no_request -- --exact` |
| G20 | fixture：`--per-page 101`（同一校验，per_page 1..=100） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::list_tags_rejects_per_page_101_code -- --exact` |
| G21 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::list_tags_rejects_per_page_101_no_request -- --exact` |
| G22 | fixture：`--page 2` 不与 `--list-tags` 同用 → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::list_tags_page_requires_list_tags_code -- --exact` |
| G23 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::list_tags_page_requires_list_tags_no_request -- --exact` |
| G24 | fixture：`--per-page 50` 不与 `--list-tags` 同用 → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::list_tags_per_page_requires_list_tags_code -- --exact` |
| G25 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::list_tags_per_page_requires_list_tags_no_request -- --exact` |

**Verification:**

- [x] `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2 -- --include-ignored`（`--include-ignored` 使被忽略的 `site_example_paths_are_rooted` 实际运行，校验本卡写入网站页的示例，GC-MN-09；(new) `list_tags_payload_computes_has_next`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::list_tags_`（new：非门族用例 `list_tags_request_record_explicit_paging`、`list_tags_request_record_default_paging`、`list_tags_json_payload`、`list_tags_human_output_is_sanitized`；fixture 门 G18–G25：`list_tags_rejects_page_zero_*`、`list_tags_rejects_per_page_101_*`、`list_tags_page_requires_list_tags_*`、`list_tags_per_page_requires_list_tags_*`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::op_rule_list_tags_`（规则门 G1–G17，new）
- [x] `source .env.test && cargo test --test command_test -- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`（既有 TUI 与 CLI 用例、GC-MN-06 的四处帮助守卫不修改即全绿，GC-MN-05）
- [x] `source .env.test && cargo test --test compat_help_examples_banner`
- [x] `source .env.test && cargo test --test compat_command_docs_examples_section`
- [x] `source .env.test && cargo test --test compat_matrix_alignment`
- [x] `source .env.test && cargo test --test compat_ledger_schema`

**C 组第 ④ 步（ER-14）:**

- `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored all command::mega2`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::list_tags_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::op_rule_list_tags_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`
- `source .env.test && cargo nextest run --test compat_help_examples_banner`
- `source .env.test && cargo nextest run --test compat_command_docs_examples_section`
- `source .env.test && cargo nextest run --test compat_matrix_alignment`
- `source .env.test && cargo nextest run --test compat_ledger_schema`

**Full-suite trigger:** `none`（只动本命令的参数、执行模块与本命令测试文件，不命中 T-1..T-6）

**Dependencies:** MN-12（与本卡共享 `noninteractive.rs`、`mega2.rs` 与文档写集，按实施顺序串行）；`DEP-MN-01`；`DEP-MN-02`。

**Deliverables:** N/A

**Implementation write set:**

- `src/command/mega2.rs`
- `src/command/mega2_browser/noninteractive.rs`
- `tests/command/mega2_browser_noninteractive_test.rs`
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- `COMPATIBILITY.md`（只改第 248 行）
- `docs/development/commands/_compatibility.md`（只改第 57 行）
- `docs/development/commands/README.md`（只改第 57 行）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- 源码注释与帮助文字：PATH 参数的帮助文字（`src/command/mega2.rs:86`）加上「tag 操作只接受 `/`」。
- `docs/commands/mega2.md`：映射表加 `--list-tags` 行；新增 payload、人读输出、分页、R10a/R10b 与错误表；写明 mega2 今日的 lightweight tag 分页行为（只补满当前页、每页从 ref 列表开头取）；Options 表加 `--list-tags`、`--page`、`--per-page`；tag 示例只放在 `MEGA2_BROWSER_EXAMPLES`（GC-MN-06）。
- `docs/commands/zh-CN/mega2.md`：同步。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`--list-tags`。
- `docs/development/commands/mega2.md`：同步。
- `COMPATIBILITY.md:248`、`_compatibility.md:57`、`README.md:57`：原位改写（GC-MN-07）。
- `docs/error-codes.md`：N/A。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** 匿名读；人读输出消毒；JSON 原样（ADR-MN-03）。

**Performance budget:** 每次一个 GET，per_page ≤ 100，响应 ≤ 1 MiB，10 s 超时。

**Estimated scope:** `M`（落点 2；生产文件 2；一处公开接口变化：`--list-tags` 及其分页参数）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=tag 列表与翻页功能的非交互形态与规则 R10a、R10b; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，回到无非交互 tag 操作的自洽状态; complete=yes; self-contained=yes; AC=29/8@EX-MN-02; VER=8/8; landing=2; prod-files=2; scope=M; deps=MN-12,DEP-MN-01,DEP-MN-02; writeset=序列化于 MN-12 之后; release=independent; split-from=N/A; exception=EX-MN-02`

### Task MN-06: 非交互创建 tag `--create-tag`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 新增 `--create-tag <NAME> [--message <TEXT>]`（登记类别 `write`、`tag`）：经 `Mega2TagClient::create_tag` 恰好发出一次 `POST /api/v1/tags`，`path_context` 固定为 `/`；省略 `--message` 即 lightweight，给出非空 message 即 annotated。拆出 MN-09（删除 tag）。唯一行为轴：browser tag 面板创建功能的非交互形态。

**Out of scope:**

- 删除 tag：拆至 MN-09。
- `target`、`tagger_name`、`tagger_email` 字段，以及非根 `path_context`：尚未排期，`DEFER-MN-02`。
- 创建后自动刷新列表：永久非目标（ADR-MN-02）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `create_tag` 客户端 | `src/internal/protocol/mega2_tag.rs:406-418` |
| 创建请求体默认 `path_context="/"`，`message` 缺省时不序列化 | `src/internal/protocol/mega2_tag.rs:160-173,211-221` |
| tag 名校验 | `src/internal/protocol/mega2_tag.rs:83-120` |
| TUI 的 message 上限 1024 字节、空即 lightweight | `src/command/mega2_browser/tag_panel.rs:15-16,148-172` |
| TUI 创建是写请求加面板刷新 | `src/command/mega2_browser/mod.rs:710-725` |
| mega2 tags POST 与鉴权 path | mega2@`8ff880c`：`src/api/router/tag_router.rs:155`；`docs/refactoring/directory-entry-api.md:402-416` |

**Acceptance criteria:**

- [x] AC-1：`--create-tag NAME`（省略 `--message`）的请求记录等于 `[(POST, /api/v1/tags, 无, {name:NAME, path_context:"/"})]`。
- [x] AC-2：`--create-tag NAME --message M`（M 合法）的请求记录等于 `[(POST, /api/v1/tags, 无, {name:NAME, path_context:"/", message:M})]`。
- [x] AC-3：成功时 `--json` 的 `data` 等于 `{operation:"create-tag", server, target:{name, kind, path:"/"}, receipt:{name, tag_id, object_id, object_type, tagger, message, created_at}}`，其中 `kind` 取「是否给出 `--message`」的映射值（省略为 `lightweight`，给出为 `annotated`）。
- [x] AC-4：人读模式成功时，stdout 等于一行 `created <kind> tag <name> -> <object_id>`（经 `sanitize`）。
- [x] AC-F（门族，计 25 门，EX-MN-02）：下表 G1–G25 全部通过（G1–G15 为规则门，G16–G25 为本卡的 fixture 门）。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-02，25 门（规则原文见 ADR-MN-08；规则门的调用为 `--create-tag v1`，路由 `/api/v1/tags`，方法 `POST`；调用模式见 ADR-MN-08 规则表；`--message` 校验规则见 ADR-MN-06；本地拒绝与服务端失败的每个 fixture 各拆成两门，见 GC-MN-02）：**

| 门 | 规则或 fixture | 命令 |
|---|---|---|
| G1 | R1 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r1 -- --exact` |
| G2 | R1b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r1b -- --exact` |
| G3 | R2 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r2 -- --exact` |
| G4 | R3 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r3 -- --exact` |
| G5 | R4a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r4a -- --exact` |
| G6 | R4b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r4b -- --exact` |
| G7 | R7 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r7_code -- --exact` |
| G8 | R7 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r7_no_request -- --exact` |
| G9 | R8 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r8_code -- --exact` |
| G10 | R8 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r8_no_request -- --exact` |
| G11 | R9a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r9a -- --exact` |
| G12 | R9b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r9b -- --exact` |
| G13 | R9c | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r9c -- --exact` |
| G14 | R10a ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r10a_code -- --exact` |
| G15 | R10a ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_create_tag_r10a_no_request -- --exact` |
| G16 | fixture：NAME 为 `bad..name`（名称校验委托既有的 `validate_tag_name`，`src/internal/protocol/mega2_tag.rs:83-120`） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_rejects_bad_name_code -- --exact` |
| G17 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_rejects_bad_name_no_request -- --exact` |
| G18 | fixture：`--message x` 不与 `--create-tag` 同用 → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_message_requires_create_tag_code -- --exact` |
| G19 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_message_requires_create_tag_no_request -- --exact` |
| G20 | fixture：`--message ""`（ADR-MN-06 的 `--message` 校验） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_rejects_empty_message_code -- --exact` |
| G21 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_rejects_empty_message_no_request -- --exact` |
| G22 | fixture：1025 字节的 `--message` → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_rejects_long_message_code -- --exact` |
| G23 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_rejects_long_message_no_request -- --exact` |
| G24 | fixture：`--message $'a\nb'`（含控制字符） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_rejects_control_message_code -- --exact` |
| G25 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::create_tag_rejects_control_message_no_request -- --exact` |

**Verification:**

- [x] `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2 -- --include-ignored`（`--include-ignored` 使被忽略的 `site_example_paths_are_rooted` 实际运行，校验本卡写入网站页的示例，GC-MN-09；(new) `create_tag_payload_separates_target_and_receipt`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::create_tag_`（new：非门族用例 `create_tag_request_record_lightweight`、`create_tag_request_record_annotated`、`create_tag_json_payload`、`create_tag_human_output`；fixture 门 G16–G25：`create_tag_rejects_bad_name_*`、`create_tag_message_requires_create_tag_*`、`create_tag_rejects_{empty,long,control}_message_*`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::op_rule_create_tag_`（规则门 G1–G15，new）
- [x] `source .env.test && cargo test --test command_test -- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`（既有 TUI 与 CLI 用例、GC-MN-06 的四处帮助守卫不修改即全绿，GC-MN-05）
- [x] `source .env.test && cargo test --test compat_command_docs_examples_section`
- [x] `source .env.test && cargo test --test compat_matrix_alignment`
- [x] `source .env.test && cargo test --test compat_ledger_schema`

**C 组第 ④ 步（ER-14）:**

- `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored all command::mega2`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::create_tag_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::op_rule_create_tag_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`
- `source .env.test && cargo nextest run --test compat_command_docs_examples_section`
- `source .env.test && cargo nextest run --test compat_matrix_alignment`
- `source .env.test && cargo nextest run --test compat_ledger_schema`

**Full-suite trigger:** `none`（只动本命令的参数、执行模块与本命令测试文件，不命中 T-1..T-6）

**Dependencies:** MN-05（tag 类规则 R10a、R10b 与 `noninteractive.rs` 写集）；`DEP-MN-01`；`DEP-MN-02`。

**Deliverables:** N/A

**Implementation write set:**

- `src/command/mega2.rs`
- `src/command/mega2_browser/noninteractive.rs`
- `tests/command/mega2_browser_noninteractive_test.rs`
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- `COMPATIBILITY.md`（只改第 248 行）
- `docs/development/commands/_compatibility.md`（只改第 57 行）
- `docs/development/commands/README.md`（只改第 57 行）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- `docs/commands/mega2.md`：映射表加 `--create-tag` 行；新增 payload、人读输出与错误表（写明 root tag 需要能覆盖 `/` 的 token 或 `push_auth=none`，否则 403 → `LBR-AUTH-002`；写明非交互 `--message` 必须非空，与 TUI「空即 lightweight」不同）；Options 表加 `--create-tag`、`--message`；tag 示例只放在 `MEGA2_BROWSER_EXAMPLES`。
- `docs/commands/zh-CN/mega2.md`：同步。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`--create-tag`。
- `docs/development/commands/mega2.md`：同步。
- `COMPATIBILITY.md:248`、`_compatibility.md:57`、`README.md:57`：原位改写（GC-MN-07）。
- `docs/error-codes.md`：N/A。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** message 拒绝控制字符且有上限；凭据按 R9a–R9c；不回显响应体。

**Performance budget:** 一次 POST，10 s 超时。

**Estimated scope:** `M`（落点 2；生产文件 2；一处公开接口变化：`--create-tag` 与 `--message`）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=创建 tag 功能的非交互形态; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，回到只有非交互 tag 列表的自洽状态; complete=yes; self-contained=yes; AC=29/8@EX-MN-02; VER=7/8; landing=2; prod-files=2; scope=M; deps=MN-05,DEP-MN-01,DEP-MN-02; writeset=序列化于 MN-05 之后; release=independent; split-from=N/A; exception=EX-MN-02`

### Task MN-09: 非交互删除 tag `--delete-tag`

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 自 MN-06 拆出。新增 `--delete-tag <NAME>`（登记类别 `write`、`tag`）：经 `Mega2TagClient::delete_tag` 恰好发出一次 `DELETE /api/v1/tags/{name}?path=/`。唯一行为轴：browser tag 面板删除功能的非交互形态。

**Out of scope:**

- 非根 selector：尚未排期，`DEFER-MN-02`。
- 删除前确认或删除后刷新：永久非目标（ADR-MN-02）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| `delete_tag` 客户端；tag 名经 `path_segments_mut` 百分号编码 | `src/internal/protocol/mega2_tag.rs:360-372,431-446` |
| tag 名校验 | `src/internal/protocol/mega2_tag.rs:83-120` |
| 状态映射：404 → `LBR-CLI-003` | `src/internal/protocol/mega2_tag.rs:245-250` |
| TUI 删除是写请求加面板刷新 | `src/command/mega2_browser/mod.rs:727-736` |
| mega2 tags DELETE、selector 与鉴权 path | mega2@`8ff880c`：`src/api/router/tag_router.rs:324`；`docs/refactoring/directory-entry-api.md:402-416` |

**Acceptance criteria:**

- [x] AC-1：`--delete-tag NAME` 的请求记录等于 `[(DELETE, /api/v1/tags/<百分号编码的 NAME>?path=%2F, 无, 空)]`。
- [x] AC-2：成功时 `--json` 的 `data` 等于 `{operation:"delete-tag", server, target:{name, path:"/"}, receipt:{deleted_tag, message}}`。
- [x] AC-3：人读模式成功时，stdout 等于一行 `deleted tag <name>`（经 `sanitize`）。
- [x] AC-F（门族，计 19 门，EX-MN-02）：下表 G1–G19 全部通过（G1–G15 为规则门，G16–G19 为本卡的 fixture 门）。
- [x] ER-06/ER-06a 同卡强制门（不计入上限）：「Docs and compatibility impact」中的每个文件逐文件交付并验收。

**判据规范（非计数正文）— EX-MN-02，19 门（规则原文见 ADR-MN-08；规则门的调用为 `--delete-tag v1`，路由 `/api/v1/tags/{name}`，方法 `DELETE`；调用模式见 ADR-MN-08 规则表；本地拒绝与服务端失败的每个 fixture 各拆成两门，见 GC-MN-02）：**

| 门 | 规则或 fixture | 命令 |
|---|---|---|
| G1 | R1 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r1 -- --exact` |
| G2 | R1b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r1b -- --exact` |
| G3 | R2 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r2 -- --exact` |
| G4 | R3 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r3 -- --exact` |
| G5 | R4a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r4a -- --exact` |
| G6 | R4b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r4b -- --exact` |
| G7 | R7 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r7_code -- --exact` |
| G8 | R7 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r7_no_request -- --exact` |
| G9 | R8 ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r8_code -- --exact` |
| G10 | R8 ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r8_no_request -- --exact` |
| G11 | R9a | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r9a -- --exact` |
| G12 | R9b | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r9b -- --exact` |
| G13 | R9c | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r9c -- --exact` |
| G14 | R10a ①：进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r10a_code -- --exact` |
| G15 | R10a ②：请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::op_rule_delete_tag_r10a_no_request -- --exact` |
| G16 | fixture：NAME 为 `bad..name`（委托 `validate_tag_name`） → 进程以 `LBR-CLI-002` 失败 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_tag_rejects_bad_name_code -- --exact` |
| G17 | 同一 fixture → 请求记录为空 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_tag_rejects_bad_name_no_request -- --exact` |
| G18 | fixture：tag 不存在，服务端返回 404 → stderr 错误信封的 `error_code` 为 `LBR-CLI-003` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_tag_missing_code -- --exact` |
| G19 | 同一 fixture → 错误信封的 `details.http_status` 为 404 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::delete_tag_missing_http_status -- --exact` |

**Verification:**

- [x] `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2 -- --include-ignored`（`--include-ignored` 使被忽略的 `site_example_paths_are_rooted` 实际运行，校验本卡写入网站页的示例，GC-MN-09；(new) `delete_tag_payload_separates_target_and_receipt`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::delete_tag_`（new：非门族用例 `delete_tag_request_record`、`delete_tag_json_payload`、`delete_tag_human_output`；fixture 门 G16–G19：`delete_tag_rejects_bad_name_*`、`delete_tag_missing_*`）
- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::op_rule_delete_tag_`（规则门 G1–G15，new）
- [x] `source .env.test && cargo test --test command_test -- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`（既有 TUI 与 CLI 用例、GC-MN-06 的四处帮助守卫不修改即全绿，GC-MN-05）
- [x] `source .env.test && cargo test --test compat_command_docs_examples_section`
- [x] `source .env.test && cargo test --test compat_matrix_alignment`
- [x] `source .env.test && cargo test --test compat_ledger_schema`

**C 组第 ④ 步（ER-14）:**

- `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored all command::mega2`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::delete_tag_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::op_rule_delete_tag_`
- `source .env.test && cargo nextest run --test command_test mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test`
- `source .env.test && cargo nextest run --test compat_command_docs_examples_section`
- `source .env.test && cargo nextest run --test compat_matrix_alignment`
- `source .env.test && cargo nextest run --test compat_ledger_schema`

**Full-suite trigger:** `none`（只动本命令的参数、执行模块与本命令测试文件，不命中 T-1..T-6）

**Dependencies:** MN-06（与本卡共享 `noninteractive.rs`、`mega2.rs` 与文档写集，按实施顺序串行）；`DEP-MN-01`；`DEP-MN-02`。

**Deliverables:** N/A

**Implementation write set:**

- `src/command/mega2.rs`
- `src/command/mega2_browser/noninteractive.rs`
- `tests/command/mega2_browser_noninteractive_test.rs`
- `docs/commands/mega2.md`
- `docs/commands/zh-CN/mega2.md`
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`
- `docs/development/commands/mega2.md`
- `COMPATIBILITY.md`（只改第 248 行）
- `docs/development/commands/_compatibility.md`（只改第 57 行）
- `docs/development/commands/README.md`（只改第 57 行）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- `docs/commands/mega2.md`：映射表加 `--delete-tag` 行；新增 payload、人读输出与错误表；Options 表加 `--delete-tag`；tag 示例只放在 `MEGA2_BROWSER_EXAMPLES`。
- `docs/commands/zh-CN/mega2.md`：同步。
- `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md`：同步（`DEP-MN-02`）；网站标记：`--delete-tag`。
- `docs/development/commands/mega2.md`：同步。
- `COMPATIBILITY.md:248`、`_compatibility.md:57`、`README.md:57`：原位改写（GC-MN-07）。
- `docs/error-codes.md`：N/A。

**Rollback mode:** `immutable-release`（见「字段全局默认」；网站页以 `cf` 补偿提交恢复）

**Migration and rollback:** `N/A`

**Security and privacy:** root tag 删除需要能覆盖 `/` 的 token 或 `push_auth=none`；凭据按 R9a–R9c。

**Performance budget:** 一次 DELETE，10 s 超时。

**Estimated scope:** `M`（落点 2；生产文件 2；一处公开接口变化：`--delete-tag`）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=删除 tag 功能的非交互形态; recovery=发布本卡 revert 的下一个 patch（immutable-release）并在 cf 分支提交网站补偿，回到只有非交互 tag 列表与创建的自洽状态; complete=yes; self-contained=yes; AC=22/8@EX-MN-02; VER=7/8; landing=2; prod-files=2; scope=M; deps=MN-06,DEP-MN-01,DEP-MN-02; writeset=序列化于 MN-06 之后; release=independent; split-from=MN-06; exception=EX-MN-02`

### Task MN-07: live 互通门与 live 门登记

**Task type:** `implementation`

**Lifecycle / Acceptance:** `done` / `complete`

**Description:** 在 Libra 仓内交付 12 个 env 门控、自给自足的 live 门（`live_gate_*`：8 个操作各至少一个；写操作门以写后观察到的结果状态为判据，移动与改名各分「到达」与「离开」两门，`--create-tag` 分 lightweight 与 annotated，另有一个翻页可见性门）与 7 个 mock 驱动的 harness 门（`live_harness_*`），用真实 `libra` 二进制对真实 Mega2 证明全部 8 个非交互操作互通；在 `CLAUDE.md` 与集成测试指南登记 live 门的环境变量与运行命令；经 `DEP-MN-03` 向 mega2 的 Libra 域交付。本卡只交付测试与贡献者文档，不改生产代码，也不改用户命令文档与网站（黑盒调用约定已由 MN-02 起各卡写入「Non-interactive operations」节）。唯一行为轴：非交互面的端到端互通证明。

**Out of scope:**

- mega2 仓内的 Libra 用例（`scripts/libra_smoke_storage_only.sh`，`BB-65` 起）与 `interop-smoke` 服务：由 mega2 侧经 `DEP-MN-03` 承接。
- 修改 mega2 的 curl + git smoke 脚本：永久非目标（ADR-MN-07）。
- 用户命令文档与网站：调用约定由 MN-01..MN-12 的各卡写入，本卡不改。
- 新的产品行为或 flag：本卡不引入。
- 新的 Cargo feature：永久非目标（ADR-MN-07）。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 现有 mega2 测试全部是 loopback mock，没有真实服务端用例 | `tests/command/mega2_browser_cli_test.rs:20-116` 等各文件自带的 mock |
| 测试环境变量清单所在位置与条目格式 | `CLAUDE.md:265-270` |
| 集成测试指南的可选网络波次 | `docs/development/integration/integration-test-plan.md:247` |
| L2/L3 门控范式 | `tests/cloud_storage_backup_test.rs:52` |
| trunk 产品写不能落在 `/`；默认 `root_dirs` 含 `project`；跨顶层 move 被拒 | mega2@`8ff880c`：`docs/refactoring/directory-entry-api.md:118,153,218`；`config/config.toml:87` |
| root tag 写需要能覆盖 `/` 的 token，或 `push_auth=none` | mega2@`8ff880c`：`docs/refactoring/directory-entry-api.md:402-416`；`config/config-storage-only.none.toml` |
| tag 列表的 lightweight 补页行为 | mega2@`8ff880c`：`src/ceres/api_service/mono_api_service.rs:1906-1965` |
| compose app profile 的端口 | mega2 `docs/plan/plan-20260727.md:45`（`127.0.0.1:19180`） |
| mega2 侧接收位置 | mega2 `docs/plan/plan-20261001.md` 的 GC-BB-01、`BB-65` 起编号预留、DEP-BB-04、DEFER-BB-03（未提交草稿，按 ID 引用；2026-10-01 13:46:48 UTC 时位于 `:281`、`:458`、`:467`、`:3937`） |

**Acceptance criteria:**

- [x] AC-1：`CLAUDE.md`「Tests」环境变量清单含下面这一行（逐字；以下方 Verification 的计数命令判定恰为 1 行）：

  ```text
  - `LIBRA_TEST_MEGA2_SERVER` and `LIBRA_TEST_MEGA2_WRITE_ROOT` (both required) plus `LIBRA_TEST_MEGA2_TOKEN_FILE` (optional) — env-gated `mega2 browser` live gates against a real Mega2 storage-only instance (`command_test` cases `mega2_browser_noninteractive_test::live_gate_*`); unset prints `skipped (...)`
  ```

- [x] AC-2：`docs/development/integration/integration-test-plan.md` 的「4.3 Wave 3：网络层（可选，建议 nightly）」节含下面这一行可直接运行的命令（逐字；以下方 Verification 的计数命令判定恰为 1 行）。该行不含占位符，运行前须先把 `LIBRA_TEST_MEGA2_SERVER`、`LIBRA_TEST_MEGA2_WRITE_ROOT` 导出为 `DEP-MN-04` 核对过的值（指南在该行之前用一句话说明），缺任一变量即以 `: "${…:?}"` 失败：

  ```text
  source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test mega2_browser_noninteractive_test::live_gate_ -- --test-threads=1
  ```

- [x] AC-3：`DEP-MN-03` 在本计划「依赖登记表」中标为「已交付」。
- [x] AC-F（门族，计 19 门，EX-MN-03）：下表 G1–G19 全部通过；G1–G12 必须在 `DEP-MN-04` 的实例上（设置环境变量）通过，sanitized 摘要（mega2 revision、`push_auth` 形态、门数、通过数、耗时；不含凭据与 token）写入「实施证据汇总」。

**判据规范（非计数正文）— EX-MN-03，19 门：** live 门（G1–G12）共同约定：以真实二进制与 `--machine` 运行；目录门（G1–G7）的 PATH 为写根 `<ROOT>`（`LIBRA_TEST_MEGA2_WRITE_ROOT`）；tag 门（G8–G12）不带 PATH（即 `/`，ADR-MN-06），其准备与清理同样作用于 `/`；写操作门的判据是写操作之后观察到的结果状态：目录门以随后的 `libra --list` 观察，tag 门以 harness 直接发出的匿名 `GET /api/v1/tags/{name}?path=%2F`（不经 libra，读取服务端回报的字段）或随后的 `--list-tags` 观察；每次 `libra` 调用仍只发一次请求；对象名为 `<run>-<后缀>`，`<run>` 由 UTC 时间戳与随机后缀组成；每门只有「判据」列这一个判据，准备、被测调用与清理是该判据的执行步骤，任一步骤失败即该门失败，但不另计判据；结束时按 run id 清理；会写入 tag 的门（G9–G12）在任何写入前先以 `--list-tags --per-page 1` 读第 1 页，`total` 超过 900 时以「实例 root tag 超过 900，请改用隔离实例（DEP-MN-04）」失败；`LIBRA_TEST_MEGA2_TOKEN_FILE` 设置时写操作带 `--token-file`。「`data.<字段>`」指 stdout 解析出的成功 envelope 中的该字段，每门只判定一个字段或一个包含关系。运行 live 门之前，操作者把 `DEP-MN-04` 当日核对的实例 URL 与写根导出为 `LIBRA_TEST_MEGA2_SERVER`、`LIBRA_TEST_MEGA2_WRITE_ROOT`（例如 compose app 实例的 `http://127.0.0.1:19180` 与 `/project`），`push_auth=token` 的实例另导出 `LIBRA_TEST_MEGA2_TOKEN_FILE`（该值不进入证据）。下表命令只读取这些变量，并以 `: "${…:?}"` 在缺少必填变量时直接失败，不会落入跳过分支；A 组与 C 组第 ④ 步使用同一组命令与同一组导出值。harness 门（G13–G19）只用 loopback mock，不需要环境变量。

| 门 | 判据 | 命令 |
|---|---|---|
| G1 | `live_gate_list`：`--list <ROOT>` 输出的 `data.operation` 等于 `"list"`（只读，无状态变化） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_list -- --exact` |
| G2 | `live_gate_create_dir`：`--create-dir <run>-c` 之后，`--list <ROOT>` 的 `items` 含名为 `<run>-c` 的目录（清理：删除 `<run>-c`） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_create_dir -- --exact` |
| G3 | `live_gate_delete_dir`：准备 `<run>-d` 并执行 `--delete-dir <run>-d` 之后，`--list <ROOT>` 的 `items` 不含 `<run>-d` | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_delete_dir -- --exact` |
| G4 | `live_gate_move_dir_arrives`：准备 `<run>-m` 与 `<run>-dst` 并执行 `--move-dir <run>-m <ROOT>/<run>-dst` 之后，`--list <ROOT>/<run>-dst` 的 `items` 含 `<run>-m`（清理：删除 `<run>-dst`） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_move_dir_arrives -- --exact` |
| G5 | `live_gate_move_dir_leaves_source`：同样准备并移动之后，`--list <ROOT>` 的 `items` 不含 `<run>-m`（清理：删除 `<run>-dst`） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_move_dir_leaves_source -- --exact` |
| G6 | `live_gate_rename_dir_new_name`：准备 `<run>-r` 并执行 `--rename-dir <run>-r <run>-r2` 之后，`--list <ROOT>` 的 `items` 含 `<run>-r2`（清理：删除 `<run>-r2`） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_rename_dir_new_name -- --exact` |
| G7 | `live_gate_rename_dir_old_name_gone`：同样准备并改名之后，`--list <ROOT>` 的 `items` 不含 `<run>-r`（清理：删除 `<run>-r2`） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_rename_dir_old_name_gone -- --exact` |
| G8 | `live_gate_list_tags`：`--list-tags` 输出的 `data.operation` 等于 `"list-tags"`（只读，无状态变化） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_list_tags -- --exact` |
| G9 | `live_gate_create_tag_lightweight`：`--create-tag <run>-lw` 之后，harness 直接发出的 `GET /api/v1/tags/<run>-lw?path=%2F` 由服务端回报 `tag_id` 等于 `object_id`（lightweight：tag 直接指向提交；mega2@`8ff880c` `mono_api_service.rs:1968-1996`）（清理：删除该 tag） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_create_tag_lightweight -- --exact` |
| G10 | `live_gate_create_tag_annotated`：`--create-tag <run>-an --message <text>` 之后，harness 直接发出的 `GET /api/v1/tags/<run>-an?path=%2F` 由服务端回报的 `message` 等于 `<text>`（annotated；mega2@`8ff880c` `mono_api_service.rs:2685-2700`）（清理：删除该 tag） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_create_tag_annotated -- --exact` |
| G11 | `live_gate_delete_tag`：准备 lightweight `<run>-del` 并执行 `--delete-tag <run>-del` 之后，harness 直接发出的 `GET /api/v1/tags/<run>-del?path=%2F` 返回 404 | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_delete_tag -- --exact` |
| G12 | `live_gate_list_tags_reaches_annotated`：准备 annotated `<run>-pg` 后，`--list-tags --per-page 100` 从第 1 页翻到 `has_next=false`（至多 10 页）所得条目含 `<run>-pg`（清理：删除该 tag） | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test command::mega2_browser_noninteractive_test::live_gate_list_tags_reaches_annotated -- --exact` |
| G13 | `live_harness_skips_without_server`：只缺 `LIBRA_TEST_MEGA2_SERVER` 时，harness 的返回值等于 `Skip("skipped (set LIBRA_TEST_MEGA2_SERVER and LIBRA_TEST_MEGA2_WRITE_ROOT to run)")` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::live_harness_skips_without_server -- --exact` |
| G14 | `live_harness_skips_without_write_root`：只缺 `LIBRA_TEST_MEGA2_WRITE_ROOT` 时，harness 的返回值等于同一个 `Skip(…)` | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::live_harness_skips_without_write_root -- --exact` |
| G15 | `live_harness_cleanup_targets_only_run_objects`：mock 上同时存在本次 run id 的对象与其它对象时，清理逻辑的请求记录等于对本次 run id 对象的删除请求列表 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::live_harness_cleanup_targets_only_run_objects -- --exact` |
| G16 | `live_harness_reports_residue`：mock 对清理删除返回 500 时，清理输出列出未能删除的对象名 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::live_harness_reports_residue -- --exact` |
| G17 | `live_harness_capacity_precheck`：mock 第 1 页 `total=901` 时，会写入 tag 的门的请求记录等于 `[(GET, /api/v1/tags/list?page=1&per_page=1&path=%2F, 无, 空)]`（在任何写入之前失败） | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::live_harness_capacity_precheck -- --exact` |
| G18 | `live_harness_reports_error_envelope`：mock 失败时，门的失败消息包含该步 stderr 的 JSON 错误信封原文（一次子串断言） | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::live_harness_reports_error_envelope -- --exact` |
| G19 | `live_harness_never_echoes_token`：设置 token 文件时，harness 的全部输出不含 token 值 | `source .env.test && cargo test --test command_test command::mega2_browser_noninteractive_test::live_harness_never_echoes_token -- --exact` |

**Verification:**

- [x] `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test::live_harness_`（G13–G19，new）
- [x] `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test mega2_browser_noninteractive_test::live_gate_ -- --test-threads=1`（G1–G12 的 live 证据，new；过滤器 `live_gate_` 不选中 `live_harness_*`；变量取 `DEP-MN-04` 当日核对值，缺任一必填变量即失败）
- [x] 逐字行计数，判定 AC-1（计数必须恰为 1；`rg` 出错或计数不为 1 都判失败）：

  ```bash
  n=$(rg -c -F -x -- '- `LIBRA_TEST_MEGA2_SERVER` and `LIBRA_TEST_MEGA2_WRITE_ROOT` (both required) plus `LIBRA_TEST_MEGA2_TOKEN_FILE` (optional) — env-gated `mega2 browser` live gates against a real Mega2 storage-only instance (`command_test` cases `mega2_browser_noninteractive_test::live_gate_*`); unset prints `skipped (...)`' CLAUDE.md); rc=$?
  [ "$rc" -le 1 ] || { echo "ERROR: rg failed"; exit 2; }
  [ "${n:-0}" = 1 ] || { echo "FAIL: expected exactly 1 matching line, found ${n:-0}"; exit 1; }
  echo "OK: exactly one line"
  ```

- [x] 逐字行计数，判定 AC-2（计数必须恰为 1；`rg` 出错或计数不为 1 都判失败）：

  ```bash
  n=$(rg -c -F -x -- 'source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test mega2_browser_noninteractive_test::live_gate_ -- --test-threads=1' docs/development/integration/integration-test-plan.md); rc=$?
  [ "$rc" -le 1 ] || { echo "ERROR: rg failed"; exit 2; }
  [ "${n:-0}" = 1 ] || { echo "FAIL: expected exactly 1 matching line, found ${n:-0}"; exit 1; }
  echo "OK: exactly one line"
  ```

**C 组第 ④ 步（ER-14）:**

- `source .env.test && cargo nextest run --test command_test mega2_browser_noninteractive_test::live_harness_`
- `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo nextest run --test command_test mega2_browser_noninteractive_test::live_gate_ --test-threads=1`
- AC-1 的逐字行计数（非测试命令，原样执行）：

  ```bash
  n=$(rg -c -F -x -- '- `LIBRA_TEST_MEGA2_SERVER` and `LIBRA_TEST_MEGA2_WRITE_ROOT` (both required) plus `LIBRA_TEST_MEGA2_TOKEN_FILE` (optional) — env-gated `mega2 browser` live gates against a real Mega2 storage-only instance (`command_test` cases `mega2_browser_noninteractive_test::live_gate_*`); unset prints `skipped (...)`' CLAUDE.md); rc=$?
  [ "$rc" -le 1 ] || { echo "ERROR: rg failed"; exit 2; }
  [ "${n:-0}" = 1 ] || { echo "FAIL: expected exactly 1 matching line, found ${n:-0}"; exit 1; }
  echo "OK: exactly one line"
  ```

- AC-2 的逐字行计数（非测试命令，原样执行）：

  ```bash
  n=$(rg -c -F -x -- 'source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test mega2_browser_noninteractive_test::live_gate_ -- --test-threads=1' docs/development/integration/integration-test-plan.md); rc=$?
  [ "$rc" -le 1 ] || { echo "ERROR: rg failed"; exit 2; }
  [ "${n:-0}" = 1 ] || { echo "FAIL: expected exactly 1 matching line, found ${n:-0}"; exit 1; }
  echo "OK: exactly one line"
  ```


**Full-suite trigger:** `none`（只动本命令的测试文件与贡献者文档，不命中 T-1..T-6；计划收口的全量门在本卡之后统一运行）

**Dependencies:** MN-09（全部 8 个操作已发布）；`DEP-MN-01`；`DEP-MN-04`。

**Deliverables:** N/A

**Implementation write set:**

- `tests/command/mega2_browser_noninteractive_test.rs`（追加 `live_gate_*` 与 `live_harness_*` 用例）
- `CLAUDE.md`（「Tests」环境变量清单，只加 AC-1 的一行）
- `docs/development/integration/integration-test-plan.md`（§4.3，只加 AC-2 的一行命令）
- 本计划文件与 `docs/development/plan/plan-status.md`

**Release write set:** `Inherited`

**Files likely touched:** 同 `Implementation write set`。

**Docs and compatibility impact:**

- `CLAUDE.md`：「Tests」清单新增 AC-1 指定的一行。
- `docs/development/integration/integration-test-plan.md`：§4.3 新增 AC-2 指定的命令行。
- 用户命令文档（EN、zh-CN、网站）、`docs/development/commands/mega2.md`、`COMPATIBILITY.md`、`_compatibility.md`、开发 README：N/A，本卡不改变用户可见行为（GC-MN-09 不适用）；不写网站，D 组没有第 ④ 项。
- `docs/error-codes.md`、`tests/INDEX.md`：N/A（无新 stable code；不新增 `--test` target）。

**Rollback mode:** `immutable-release`（见「字段全局默认」；本卡不写网站；live 实例上的残留按 run id 清理，见「故障恢复矩阵」）

**Migration and rollback:** `N/A`

**Security and privacy:** live 门只在操作者显式设置环境变量时运行；只删除带本次 run id 的对象；token 只经 `--token-file` 传递，不进输出与证据（GC-MN-04）。

**Performance budget:** live 门合计至多约 50 次 `libra` 进程调用与 3 次直接 HTTP GET（G12 至多翻 10 页），每次 10 s 超时；`--test-threads=1` 时单次运行预算 ≤ 5 分钟。

**Estimated scope:** `S`（无生产文件）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=非交互面的端到端互通证明; recovery=发布本卡 revert 的下一个 patch（immutable-release），回到无 live 门的自洽状态，live 残留按 run id 清理; complete=yes; self-contained=yes; AC=22/8@EX-MN-03; VER=4/8; landing=0; prod-files=0; scope=S; deps=MN-09,DEP-MN-01,DEP-MN-04; writeset=序列化于 MN-09 之后; release=independent; split-from=N/A; exception=EX-MN-03`

## 测试矩阵

本表登记本计划最终必须覆盖的内容。执行阶段按 ER-13 只跑与本卡相关的行；整张表由收口阶段的全量门统一覆盖。

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| 单元 | 诊断 helper 的键集合；URL 解析错误不回显；操作登记与 token 闸门；各 payload 构造；帮助与命令文档的示例都可解析且 PATH rooted；网站页示例由被忽略的 `site_example_paths_are_rooted` 显式运行校验 | `source .env.test && cargo test --lib internal::protocol::mega2_diag`；`source .env.test && cargo test --lib internal::protocol::mega2_tree`；`source .env.test && cargo test --lib command::mega2`；`source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo test --lib command::mega2::tests::site_example_paths_are_rooted -- --exact --ignored` |
| 集成 | 8 个操作的请求记录、payload、人读输出、本地拒绝；`op_rule_<op>_<rule>` 门族；MN-01 诊断门族；`live_harness_*` 门 | `source .env.test && cargo test --test command_test mega2_browser_noninteractive_test`；`source .env.test && cargo test --test command_test -- mega2_tree_transport_test::failure_details_ mega2_entry_transport_test::failure_details_ mega2_mutate_transport_test::failure_details_ mega2_tag_transport_test::failure_details_` |
| 集成（回归） | 既有 TUI、CLI、transport 用例不回归 | `source .env.test && cargo test --test command_test mega2_` |
| 兼容 | 帮助 EXAMPLES、命令文档 Examples 节、兼容矩阵与 CLI 对齐、`COMPATIBILITY.md` 行号锚点 | `source .env.test && cargo test --test compat_help_examples_banner`；`compat_command_docs_examples_section`；`compat_matrix_alignment`；`compat_ledger_schema`（各自以 `source .env.test && cargo test --test <target>` 运行） |
| 迁移 | N/A：无 schema 或存储格式变更 | N/A |
| 安全 | URL 解析错误不回显凭据；读操作不带 `Authorization`（R6）；token 优先序与零泄漏（R9a–R9c）；不读 stdin、不输出 `0x1b`（R1、R2）；`details` 不含响应体或 token；敌意名称在请求前被拒 | `mega2_browser_cli_test::malformed_server_url_is_not_echoed`；`mega2_browser_noninteractive_test` 中的 `op_rule_*` 与 `*_rejects_*` 用例 |
| 性能 | 每次调用恰好一次请求（mock 计数）；既有超时、响应与条目上限不变 | 同「集成」target；既有 `mega2_tree_transport_test` 等 |
| live/gated | Libra 仓内，真实 Mega2 上的 12 个 live 门 | `source .env.test && : "${LIBRA_TEST_MEGA2_SERVER:?}" "${LIBRA_TEST_MEGA2_WRITE_ROOT:?}" && cargo test --test command_test mega2_browser_noninteractive_test::live_gate_ -- --test-threads=1` |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| MN-10 | GAP-MN-07；`mega2_tree.rs:88-92` | `src/internal/protocol/mega2_tree.rs` | N/A（文档承诺已存在） | `mega2_tree::tests::url_parse_error_does_not_echo_input`；`mega2_browser_cli_test::malformed_server_url_is_not_echoed` |
| MN-01 | GAP-MN-03；`mega2_mutate.rs:249-266`；`error.rs:984-990` | `src/internal/protocol/` | `docs/error-codes.md`；`docs/commands/mega2.md` EN/zh/网站「Errors」；开发文档 | `mega2_diag::tests::run_*`；`mega2_*_transport_test::failure_details_*`（起点门、中段门与末段门）；G5、G6、G23、G24 结构命令；`mega2_browser_cli_test::json_error_envelope_carries_http_details` |
| MN-02 | GAP-MN-02、GAP-MN-04；`mega2.rs:180-210` | `src/command/mega2.rs`、`src/command/mega2_browser/noninteractive.rs` | 三处命令文档、开发文档、三处兼容行 | `mega2_browser_noninteractive_test::{list_*, op_rule_list_*}`；`command::mega2::tests::{help_example_paths_are_rooted, doc_example_paths_are_rooted, site_example_paths_are_rooted}`（最后一个为 `#[ignore]`，须以 `--ignored` 显式运行） |
| MN-03 | GAP-MN-01；`mega2_entry.rs:148-246` | 同上 | 同上 | `mega2_browser_noninteractive_test::{create_dir_*, op_rule_create_dir_*}` |
| MN-11 | GAP-MN-01；`mega2_auth.rs:120-167` | 同上 | 同上 | `mega2_browser_noninteractive_test::{op_rule_create_dir_r9a, op_rule_create_dir_r9b, op_rule_create_dir_r9c, create_dir_token_*, list_token_refusal_message, op_rule_list_r5a_*, op_rule_list_r5b_*}`；`mega2_browser_mkdir_test::token_flags_are_refused_for_reads_and_never_echoed` |
| MN-04 | GAP-MN-01；`mega2_mutate.rs:387-407` | 同上 | 同上 | `mega2_browser_noninteractive_test::{delete_dir_*, op_rule_delete_dir_*}` |
| MN-08 | GAP-MN-01；`mega2_mutate.rs:411-441` | 同上 | 同上 | `mega2_browser_noninteractive_test::{move_dir_*, op_rule_move_dir_*}` |
| MN-12 | GAP-MN-01；`mega2_mutate.rs:443-453` | 同上 | 同上 | `mega2_browser_noninteractive_test::{rename_dir_*, op_rule_rename_dir_*}` |
| MN-05 | GAP-MN-01；`mega2_tag.rs:385-403` | 同上 | 同上 | `mega2_browser_noninteractive_test::{list_tags_*, op_rule_list_tags_*}` |
| MN-06 | GAP-MN-01；`mega2_tag.rs:406-418` | 同上 | 同上 | `mega2_browser_noninteractive_test::{create_tag_*, op_rule_create_tag_*}` |
| MN-09 | GAP-MN-01；`mega2_tag.rs:431-446` | 同上 | 同上 | `mega2_browser_noninteractive_test::{delete_tag_*, op_rule_delete_tag_*}` |
| MN-07 | GAP-MN-05、GAP-MN-06；mega2 plan-20261001 的 `DEP-BB-04` | `tests/command/mega2_browser_noninteractive_test.rs` | `CLAUDE.md`、集成测试指南 | `mega2_browser_noninteractive_test::{live_gate_*, live_harness_*}` |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 本计划成稿；pin mega2@`8ff880c`；计划级 Codex review `PASS` | 本文件与 review log | N/A |
| M1 | MN-10、MN-01 `done`/`complete` | 新 patch 版本、D-MN-STD | 分卡处理：MN-01 发布 revert patch（immutable-release，网站以 `cf` 补偿提交恢复）；MN-10 只发布保持「URL 解析错误不回显原始输入」不变量的前滚修复 patch（forward-only），不 revert |
| M2 | MN-02 `done`/`complete` | 同上 | 发布 revert patch（immutable-release） |
| M3 | MN-03、MN-11、MN-04、MN-08、MN-12 `done`/`complete` | 同上 | 按依赖逆序发布 revert patch（immutable-release） |
| M4 | MN-05、MN-06、MN-09 `done`/`complete` | 同上；`DEP-MN-03` 的稳定操作前提满足（交付在 M5） | 按依赖逆序发布 revert patch（immutable-release） |
| M5 | MN-07 `done`/`complete`；收口全量门全绿；`DEP-MN-03` 已交付 | live 门证据摘要（或 `DEP-MN-04` 降级修订）、全量 nextest 计数 | 前滚修复 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 本地校验失败 | 无 | 修正参数后重跑 | 发出任何请求 |
| token 文件不可读或为空 | 无请求 | 修复 token 文件 | 静默改用环境变量或 `--token` |
| 写请求已发出，响应前超时或断连（`details.transport`） | 远端可能已写 | 用 `--list`/`--list-tags` 观察后再决定；黑盒用例把结果记为「未知」 | 自动重试；报告成功 |
| 写请求返回 2xx 但回执不合规（`LBR-NET-002`，`details.http_status` 为 2xx） | 远端多半已写 | 同上 | 报告成功；丢失 `http_status` |
| 成功后 stdout 写失败（管道关闭） | 远端已写，无输出 | 调用方用 `--list` 核对 | 重复写入 |
| live 门中途失败 | 带本次 run id 的目录或 tag | 门内清理逻辑按 run id 删除并输出残留清单；人工按 run id 清理 | 删除不带本次 run id 的对象 |
| 发布中途失败（C 组或 D 组） | 已推送的提交或已创建的 tag | ER-09 处理推送；D 组失败只能前滚 | 回退已推送的提交或已发布的 artifact |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| mega2 侧从 libra.tools 安装最新 stable：Libra 的回归会直接让 mega2 的 Libra 域 smoke 变红 | 中：跨仓误报 | 每卡 D-MN-STD 含 `request-stable-manifest`；契约文档与 live 门先在 Libra 仓验证；`DEP-MN-03` 交付时给出功能下限版本 | MN-07 |
| 有人把 libra 用例加进 mega2 的 curl + git smoke 脚本 | 中：违反 mega2 现有规定 | ADR-MN-07 与非目标写明只用 `scripts/libra_smoke_storage_only.sh`；本计划不改 `../mega2/**` | MN-07 |
| mega2 的 lightweight tag 分页让新建 tag 在列表中不可见或重复 | 中：基于列表的黑盒断言不稳定 | live 门以匿名 `GET /api/v1/tags/{name}` 读取服务端回报的字段证明 lightweight tag 的存在与形态（MN-07 G9、G11），不依赖列表；文档写明限制；经 `DEP-MN-03` 转告 mega2 侧 | MN-05、MN-07 |
| mega2 契约在执行期间漂移 | 中：卡被阻塞 | GC-MN-08 开工前重核；漂移即停卡并修订 `DEP-MN-01` | MN-03..MN-09、MN-11、MN-12、MN-07 |
| 门族豁免被用来掩盖真实的多轴卡 | 中：粒度失控 | 豁免只覆盖 ADR-MN-08 规则门、MN-06 的 `--message` fixture 门、MN-01 诊断门与 MN-07 的 live 门和 harness 门；每门是具名 `--exact` 命令；各卡门族以外的 AC 仍受 8 条上限约束 | 全部操作卡、MN-01、MN-07 |
| 非交互删除没有确认，误删目录或 tag | 中：远端数据被改 | flag 必须携带确切名称，无默认目标；服务端按 mode 拒绝文件目标；文档提示先 `--list` | MN-04、MN-09 |
| `--token` 让 token 进入 CI 日志或 shell history | 中：凭据泄漏 | 文档推荐 `--token-file` 或环境变量；R9c 零泄漏门 | MN-11 |
| `--server` 中的 URL 凭据随解析错误进入日志 | 中：凭据泄漏 | MN-10 修复并以 marker 回归 | MN-10 |
| 写请求超时后结果未知 | 中：黑盒断言不确定 | `details.transport` 区分；文档给出处理方式；不自动重试 | MN-01、MN-07 |
| 既有守卫用例编码了旧的「仅 TUI」政策 | 低：误删守卫 | GC-MN-05 只允许 MN-11 改写一个指定用例，其余必须不改即绿 | MN-11 |
| `COMPATIBILITY.md` 增删行导致行号引用漂移 | 中：第 248 行之后的行号引用（如 `grit-suite-scope.md:173` 引用的 259–261 行）失准；若其它计划在前面增删行，ledger 用例也会失败 | GC-MN-07 只原位改写第 248 行；每卡跑 `compat_ledger_schema` | MN-02、MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09 |
| JSON 回执中的 C1/DEL 字符透传到终端 | 低：显示被干扰 | 与全仓 `--json` 政策一致；人读输出消毒；已在 ADR-MN-03 登记 | 全部操作卡 |
| 网站部署滞后，D 组标记暂未出现 | 低：卡停在 `remote-pending` | ER-MN-02 第 ④ 项按标记核对，未出现即不标 `complete` | 写网站的卡 |
| 版本号被并发发布抢占，或本地落后于上游 | 中：发布冲突 | ER-MN-01：bump 前 `libra pull --ff-only` 并重读版本，顺延不复用；`gh -R libra-tools/libra` | 全部 |
| 真实 Mega2 实例不可用或 root tag 过多 | 中：MN-07 live 证据缺失 | `DEP-MN-04` 自 MN-09 完成起 7 天的上限、root tag ≤ 900 前置，以及同一修订删除依赖边的降级路径 | MN-07 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| `--list` | 1 次 GET，响应 ≤ 1 MiB，≤ 2000 项 | 每次调用 O(1) 个请求 | 10 s 超时 | MN-02 用例 |
| `--create-dir` / `--delete-dir` | 1 次 POST | O(1) | 10 s 超时，响应 ≤ 1 MiB | MN-03、MN-04 用例 |
| `--move-dir` / `--rename-dir` | 1 次 POST | O(1) | 10 s 超时，响应 ≤ 1 MiB | MN-08、MN-12 用例 |
| `--list-tags` | 1 次 GET，per_page ≤ 100 | O(1) | 10 s 超时，page ≤ 1000 | MN-05 用例 |
| `--create-tag` / `--delete-tag` | 1 次 POST 或 DELETE | O(1) | 10 s 超时 | MN-06、MN-09 用例 |
| live 门 | 合计至多约 50 次 `libra` 进程调用与 3 次直接 HTTP GET | O(门数)，G12 至多 10 页 | 单次运行 ≤ 5 分钟；root tag ≤ 900，新建后不超过 1000，10 页足够 | MN-07 live 证据 |

## 兼容与文档收口

- [x] `COMPATIBILITY.md` 第 248 行已原位同步（MN-02、MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09），未增删行。
- [x] `docs/commands/mega2.md` 已同步（MN-01..MN-06、MN-08、MN-09、MN-11、MN-12）。
- [x] `docs/commands/zh-CN/mega2.md` 已同步（同上）。
- [x] `../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md` 已同步，`cf` 推送与网站标记核对证据已记录（同上）。
- [x] `CLAUDE.md` 与 `docs/development/integration/integration-test-plan.md` 已登记 live 门（MN-07）。
- [x] `docs/development/commands/mega2.md`、`_compatibility.md:57`、`README.md:57` 已同步。
- [x] `docs/error-codes.md` 的「Command-specific details」小节已由 MN-01 交付；本计划不新增 stable code。
- [x] `tests/INDEX.md`：N/A，不新增或重命名 `--test` target；`command_test` 行的描述仍然准确。
- [x] `Cargo.toml` `[[test]]`：N/A。
- [x] `plan-long.md` 日期计划索引已登记本计划。

## Codex review log

Result 只允许 `PASS` 或 `FAIL`。`FAIL` 必须列出 P0/P1 条目，并在下一轮复审关闭；P2 可由具名责任人书面接受为 residual risk，但不改变本轮的 `FAIL` 记录（ER-05、ER-MN-03）。评审以 `codex exec -s read-only`（`gpt-6-sol`、`xhigh`）执行。评审意见只以本表摘要与修订历史的形式进入计划；不新建、也不保留评审输出文件（模板 v2.12）。每轮的 Evidence 列记录该轮输入的计划文件 SHA-256 与输入时间。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| R1 | 计划全文（9 卡版本）与 `plan-status.md`、`plan-long.md` 的登记 | FAIL | P0 无；P1×9：① MN-03 把重名写成 400（mega2 今日 500）→ 改按 500 → `LBR-NET-002` 验收；② URL 解析错误回显原始输入 → 新增 MN-10；③ GC-MN-11/12 把各操作判据移出计数、MN-01 AC-1 与 MN-07 AC-1 打包 → ADR-MN-08 登记表方案；④ MN-01 落点少计 → 申辩：模板把落点定义为一个具体目录（plan-template.md:81），G-04 只把顶层目录记作规避（:465）；⑤ MN-08 合并 move 与 rename → 拆出 MN-12；⑥ `--list-tags --ref` 规则矛盾 → R10 覆盖全部 tag 操作；⑦ MN-01 漏 `docs/error-codes.md` → 加入；⑧ live tag 十页上限无前置 → 容量前置；⑨ `DEP-MN-04` 降级后依赖边仍在 → 降级修订同时删除依赖边 | 无 P2 | 输入为 2026-10-01 12:12:44 UTC 时的版本（计划文件 SHA-256 `5c5c988cfbd157aebbb799786f3694989ee27e1094e382dbc14e9137ee37c8d9`）；修订见修订历史 12:31:50 UTC 行 |
| R2 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×4：① 独立谓词计数仍被打包（MN-01 每客户端多种失败、MN-07 多步场景、「`op_class_rules` 覆盖本操作」）→ 原子规则 R1–R10b 逐门展开，登记 G-03 门族型豁免 EX-MN-01/02/03（使用者批准），MN-07 改为 9 个自给自足的 live 门；② C 组第 ④ 步的 nextest 命令未落实 → 每张非全量卡写明 nextest 命令；③ `DEP-MN-04` 的 7 天时钟起点不可达 → 改从 MN-09 完成起算；④ 网站验收不可复现 → `curl` 部署页并按本卡标记 `rg`。R1 的 ④（落点）申辩被接受，R1 其余条目确认关闭 | P2×3 全部修复：mega2 草稿改为按 ID 引用，并附 2026-10-01 13:46:48 UTC 的行号；M4 只声明稳定操作前提；容量前置改为 ≤ 900 | 输入为 2026-10-01 12:41:31 UTC 时的版本（计划文件 SHA-256 `c545a66479b426e5c129619bac1b5e8cea45acb4b1236d550293523225d4d41c`）；修订见修订历史中由 R2 触发的一行 |
| R3 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×4：① 门内仍打包多个可独立失败的 fixture（MN-01 G1/G7、MN-07 多步 live 门与文档要求）→ MN-01 改为单一发送包装的结构约束与每门一个 fixture，MN-07 改为单判据门与逐字文档行；② 称 libtest 只接受一个位置过滤器 → 申辩：Rust 1.98.1 `library/test/src/cli.rs:157-163` 明文允许多个过滤器并取并集，已实测，登记于事实基线；③ `revert` 与发布、网站补偿不符 → 改为 `immutable-release` 并写明降级指引与兼容窗口；④ 网站核对在 `curl` 失败时可能误判 → fail-closed 检查。R2 的 ③④ 与 P2 确认关闭 | P2×1 修复：live 过滤器改为 `live_gate_` | 输入为 2026-10-01 13:53:21 UTC 时的版本（计划文件 SHA-256 `1ecbf0ad33eab9e7c2c5c894ab6c2d1ff79a9e676c29b619c4e2c3c29fadf0f6`）；修订见修订历史中由 R3 触发的一行 |
| R4 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×3：① MN-01 只用 500 fixture，未证明 2xx 上的末段校验也带 `details` → 增 8 个末段门；② MN-01 AC-1 把 message、hint、退出码的不变归功于只断言 stable code 的既有用例 → 改由结构门 G23、G24 证明；③ MN-11 未经真实 CLI 验证人读模式写入与仅环境变量、仅 flag 的凭据 → 增 4 条 AC。R3 的 libtest 申辩、恢复模式、网站检查与 live 过滤器确认关闭 | P2×1 修复：GC-MN-07 理由改为保守选择并给出真实受影响的引用 | 输入为 2026-10-01 14:25:32 UTC 时的版本（计划文件 SHA-256 `2614c91176cc13825c1b13cc4562d7baaff9e72b18757c42062c5c4324de059a`）；修订见修订历史中由 R4 触发的一行 |
| R5 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×4：① 未证明 `run` 只在原错误上附加 `details`（可能重建 `CliError`）→ 增 G25、G26；② MN-11 AC-8 打包改名、限定改动与通过三条 → 拆为三条，凭据矩阵改为 fixture 门并按补偿措施并入 EX-MN-02；③ MN-07 G12 合并两种缺变量情形 → 拆为两门；④ 计划规定把评审原始输出存于仓库外，违反 v2.12 → 改为只在计划内摘要。R4 的末段门、真实 CLI 凭据路径与 GC-MN-07 引用确认关闭 | 无 P2 | 输入为 2026-10-01 15:08:52 UTC 时的版本（计划文件 SHA-256 `04302027abd95cfe90028b44ffc65009027cb48b239a8a290fd55fe315940a2d`）；修订见修订历史中由 R5 触发的一行 |
| R6 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×4：① 「本地拒绝」把 stable code 与零请求两条可分别失败的判据算作一条（MN-05 等）→ 拆为两门并移入门族；② MN-01 G23 用 `libra diff -w`，会漏掉字符串内部空白的改动 → 改为只去行首空白的逐行比较；③ G25、G26 的五元组漏了 `kind` → 改为整个 `CliError` 相等；④ live 门命令写死示例 URL 与写根 → 改为读取操作者导出的核对值并在缺失时失败。R5 的 MN-11、G12/G13 拆分与评审记录方式确认关闭 | 无 P2 | 输入为 2026-10-01 15:31:34 UTC 时的版本（计划文件 SHA-256 `419a6c82c829a063afa6e9ba2814b5dc2feeca28d44b683070937d82a5962716`）；修订见修订历史中由 R6 触发的一行 |
| R7 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×4：① G23 只比较选定的行，改写返回表达式等可绕过 → 改为「既有行原样按序保留＋新增行不得构造或转换错误」的结构检查，并改为作用域设计、增 G27；② MN-07 的 tag 门沿用写根作 PATH 会被本地拒绝 → tag 门不带 PATH；③ MN-11 的差异命令只打印不判定 → 改为逐字比较期望内容；④ 逐字行计数命令在出现 2 行时仍成功 → 断言计数恰为 1。R6 的四项修复确认关闭 | 无 P2 | 输入为 2026-10-01 15:55:20 UTC 时的版本（计划文件 SHA-256 `b9e10856273c33651c77ed9d95a737e614ea5122f0cf182eeb3f59f1003ccb9c`）；修订见修订历史中由 R7 触发的一行 |
| R8 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×4：① G23 放过替换后的发送行，可能丢掉客户端的 `transport_error` 映射 → 增 G25；② MN-11 的命令只检查有替换发生，且命令替换吞掉末尾换行 → 先断言四行原文再逐字节 `cmp`；③ G23 含两个独立检查 → 拆为 G23、G24 并重算分子；④「不读 stdin」没有有效判定（`/dev/null` 立即 EOF）→ 增 R1b。R7 的 tag PATH 与逐字行计数确认关闭 | 无 P2 | 输入为 2026-10-01 16:39:36 UTC 时的版本（计划文件 SHA-256 `d7557fc0fdf4b580cc43be5a741fbd6fdb83d84b5b603d5f81da422697a59f66`）；修订见修订历史中由 R8 触发的一行 |
| R9 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×3：① MN-10 按默认恢复会 revert 凭据回显修复 → 改为 `forward-only`；② MN-01 未覆盖收到状态码之后、终点之前的中段失败（如响应体读取中断）→ 增 8 个中段门；③ MN-07 的写操作 live 门只看回执或调用方输入的字段 → 改为观察写后的结果状态与服务端回报的字段。R8 的四项修复确认关闭 | 无 P2 | 输入为 2026-10-01 16:58:49 UTC 时的版本（计划文件 SHA-256 `4b8b0b49f4f1543a4d0535fc286a3da78d9a9bffe2a9680384bd401321aa4e07`）；修订见修订历史中由 R9 触发的一行 |
| R10 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×1：里程碑 M1 仍对 MN-10 规定 revert，与其 `forward-only` 恢复冲突 → 分卡写明。R9 的中段门与写后状态门确认关闭，各卡 AC 计数与审计表一致 | P2×1 修复：ADR-MN-07 与风险表仍写「删除回执证明 lightweight tag 存在」→ 改为 G9、G11 的匿名 GET 判定 | 输入为 2026-10-01 17:13:51 UTC 时的版本（计划文件 SHA-256 `116bedb03644f79a48aae39823b4c63d2c0c7fa488bd093caf6ef794a8f633d1`）；修订见修订历史中由 R10 触发的一行 |
| R11 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×1：未说明作用域与实际状态码如何到达 delete/move 在 `post_json` 之后的校验，终点门只测 200 → 写明 task-local 作用域（不改函数签名），终点门改用 201。R10 的两项确认关闭 | P2×2 修复：源码注释仍称只读、只在 TUI 写入 → 分配给 MN-02、MN-03 改写，MN-03 的 `src/cli.rs` 注释改动经新 `DEP-MN-06` 串行；clap 映射锚点改为 `src/cli.rs:2939-2943` | 输入为 2026-10-01 17:23:13 UTC 时的版本（计划文件 SHA-256 `12d401f77fa4321d8503744e9d633f41379297876c90ea1f018e53894be80fcc`）；修订见修订历史中由 R11 触发的一行 |
| R12 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×1：MN-03 把只改注释的 `src/cli.rs` 计为行为落点 → 改回 2/2。R11 的 P1 与 clap 锚点确认关闭 | P2×3 修复：`execute_safe` 的 Side Effects 注释分配给 MN-02、MN-03、MN-11；`DEP-QP-01` 镜像行补登 MN-03；非目标改为「不改代码」 | 输入为 2026-10-01 17:48:35 UTC 时的版本（计划文件 SHA-256 `028a67263601ae772e9f8dd64ceb6c01ed485dbfd66a9cf037a0a54fe6df8f9c`）；修订见修订历史中由 R12 触发的一行 |
| R13 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×2：① MN-07 要求逐字写入的命令含 `<url>`/`<root>`，在 shell 中是重定向 → 改为不含占位符、带必填变量检查的命令；② 网站示例只由正则检查，`--list src/pkg` 之类会漏过 → 增 AC-8 与 `site_example_paths_are_rooted`，以真实解析校验网站页示例，各写网站的卡都运行。R12 的四项确认关闭 | 无 P2 | 输入为 2026-10-01 18:07:27 UTC 时的版本（计划文件 SHA-256 `7a5670323ae789c1c5bd80ccf55a60aadfeab6a10a6e61e9e6c63000b815414b`）；修订见修订历史中由 R13 触发的一行 |
| R14 | 12 卡版本全文与两处登记 | FAIL | P0 无；P1×1：MN-02 的 C 组全量 nextest 未设置 `LIBRA_SITE_MEGA2_DOC`，网站示例检查会以 skipped 通过 → 改为 `#[ignore]` 且无跳过分支，所有门命令显式运行它。R13 的两项确认关闭 | 无 P2 | 输入为 2026-10-01 18:35:16 UTC 时的版本（计划文件 SHA-256 `213bd01e9a1fde8468b7aaab17621645448f5b76943c40efda992d180c2ae1f1`）；修订见修订历史中由 R14 触发的一行 |
| R15 | 12 卡版本全文与两处登记 | PASS | P0 无；P1 无。R14 的 P1 确认关闭（MN-02 的 C 组显式运行被忽略的网站示例测试并要求 PASS） | P2×1 已修复：测试矩阵「单元」行把普通的 `cargo test --lib command::mega2` 当作网站示例检查 → 改为列出显式运行被忽略测试的命令；追溯表 MN-02 行漏列 `site_example_paths_are_rooted` → 补上 | 输入为 2026-10-01 18:51:16 UTC 时的版本（计划文件 SHA-256 `5e16889f9b1049979b6eaee7cfa9da183a8792a9d056a9d977f62775fba39463`）；P2 修复见修订历史中由 R15 触发的一行 |
| R16 | R15 `PASS` 之后的差异确认（只修 P2 的汇总表与本日志） | PASS | P0 无；P1 无。R15 的 P2 确认已解决，改动未引入新问题 | P2×1 已修复：R15 行把两处缺陷写成同一问题 → 分开描述（测试矩阵误把普通命令当作网站检查；追溯表漏列该测试） | 输入为 2026-10-01 19:09:52 UTC 时的版本（计划文件 SHA-256 `5aef5eabf0945b9738790dc119ada1a8bdd839a24f1f3033fa44c4f26e162402`），评审范围为相对 R15 输入版本的差异 |
| R17 | 提交前上游前移（`main@7f810da`）后的差异确认：锚点与版本事实复核、`DEP-QP-01` 删除的跟进、`plan-status.md` 合并到上游 | FAIL | P0 无；P1×1：九张卡写集中的「`_compatibility.md`（只改第 55 行）」未随锚点刷新，`7f810da` 上第 55 行是 `log` 行 → 改为第 57 行。其余刷新的锚点与版本面、`DEP-QP-01` 的跟进、R15/R16 记录与 `plan-status.md` 的合并确认无误，`plan-long.md` 只加一行索引，任务卡的门、规则与计数未变 | 无 P2 | 输入为 2026-10-02 00:54:48 UTC 时的版本（计划文件 SHA-256 `a5a85d72dea75922bba9586cf9860d891e758c0bbff6024d4bf9ae342b9628ff`），评审范围为相对 R16 输入版本的差异，以及 `plan-status.md`、`plan-long.md` 相对 `7f810da` 的差异；修订见修订历史中由 R17 触发的一行 |
| R18 | R17 修订后的差异确认 | PASS | P0 无；P1 无。R17 的 P1 确认关闭：12 处 `:57` 锚点与九张卡写集都指向 `7f810da` 上第 57 行的 mega2 行；修订历史与 R17/R18 日志行一致，旧行号只出现在描述移动或修复的文字中 | 无 P2 | 输入为 2026-10-02 01:00:30 UTC 时的版本（计划文件 SHA-256 `30f8e4daacae24227c8f0eb7cca13d65b92192ba8f15c48fe8e1685cddbcdb6c`），评审范围为相对 R17 输入版本的差异 |
| R19 | 全计划最终收口审查：12 卡已发布/D 组、pinned-source live、全量/网站门、三个文档的收口差异与同步 | PASS | P0 无；P1 无。无正确性、范围、同步或证据问题阻止完成审查记录和行政交付后的收口；只读审查，未重跑测试 | P2×1 已修复：状态表允许值未含临时 `收口中` → 最终状态统一采用既有 `已收口`，不增加新状态值；无遗留 P2 | `gpt-6-sol` / `xhigh`；2026-10-03 13:40:11 UTC 输入，计划 SHA-256 `538f5ac3be5849762c4d905506d4337f0f53fbf862f152e9194d3f6f377a2d85`；exit 0，字面 `VERDICT: PASS`；审查输出只以内联摘要记录，无新输出文件 |
| R20 | 行政收口相对 incoming main `721fec9` 的三个文档差异与冲突整合 | PASS | P0 无；P1 无。ACF 交接与 security/privacy 边界保留，MN-07 / DEP-MN-03 / 日期索引一致；`faae20c` 与旧 head 的证据和整合后待跑 CI 已明确分开 | P2 无 | `gpt-6-sol` / `xhigh`；2026-10-03 16:18:56 UTC 完成；输入计划 SHA-256 `c6bfa914ccab216fa9b7cd1f65a5e3272fddb812f32d0faac7d0b74c2986b162`；exit 0，字面 `VERDICT: PASS`；只读差异审查、未跑测试、无新输出文件 |
| R21 | PR #609 CI 待修事项的计划与状态文档收口差异 | FAIL | P0 无；P1×1：状态快照将两个失败检查概括为 bwrap 阻塞，修订为分别列出 compat-offline-core 的四个失败测试（其中两个涉及 bwrap）和 opencode-export-linux 的 Linux bwrap 集成门；计划标题区分一项 CI 门和四个测试 | P2×2 已修复：① 分开说明 R19 计划收口审查与 R20 incoming-main 文档差异审查；② 补齐状态快照中计划文档的 Markdown 链接目标 | gpt-6-sol xhigh；输入计划 SHA-256 d6ba3d7c9bd7cf0d5c42e16a4611a4ed002e08b09070169b3e8eb4fee742bf7c；输入文档快照时间 2026-10-04 00:43:05 UTC；GitHub job logs 未能由本轮评审独立读取，未跑测试 |
| R22 | R21 修订后的三份文档差异复核 | PASS | P0/P1 无。R21 的 CI 分类 P1、R19/R20 评审范围与计划链接 P2 均已修复；旧 CI 结果仍绑定 ca5dc6d/base 721fec9，本次文档 head CI 待观测。 | P2 无 | gpt-6-sol xhigh；输入计划 SHA-256 c48edb99d0fc975cec6a78b50b5d6847ea54144e291528feffe8bee1fe81e8c0；输入文档快照时间 2026-10-04 01:03:53 UTC；exit 0，字面 VERDICT: PASS；只读审查、未跑测试；GitHub job logs 不可访问，未独立验证远端 CI |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-MN-01 | 非交互的 `GET /api/v1/tags/{name}`（按名称取单个 tag） | 不是 browser 现有功能 | 黑盒消费者明确需要，且确认它不能用 `--list-tags` 替代 | 未来 Libra 日期计划 |
| DEFER-MN-02 | 非根 tag path（`?path=` selector、非根 `path_context`）与 tag 的 `target`/`tagger_*` 字段 | browser tag 面板只操作 root | TUI 或非交互面需要按目录管理 tag | 未来 Libra 日期计划 |
| DEFER-MN-03 | 文件条目操作（`is_directory=false` 的删除/移动/改名、建文件、`edit/save`、blob 预览） | browser 中文件不可操作；内容安全与容量是独立轴（延续 plan-20260912 DEFER-MB-02） | 内容上限与 redaction 方案被接受 | 未来 Libra 日期计划 |
| DEFER-MN-04 | 一次请求同时换父目录与改名；批处理/脚本模式；轮询与 `--wait` | 不是 browser 现有功能；一次调用一次请求是本计划契约 | 黑盒消费者给出具体场景 | 未来 Libra 日期计划 |
| DEFER-MN-05 | 成功响应的 HTTP 状态与可配置超时 | 黑盒以退出码与 `ok` 判定成功已经足够；超时沿用 10 s | 需要断言具体的 2xx 状态，或服务端时延超出 10 s | 未来 Libra 日期计划 |
| DEFER-MN-06 | stable code 映射统一（delete/move 的 404 与 create-entry 的 500 都是 `LBR-NET-002`、tag 的 404 为 `LBR-CLI-003`、create-entry 的 400 退出码 129） | 会改变 TUI 与既有机器契约，需要独立 ADR 与兼容窗口 | ADR 被接受 | 未来 Libra 日期计划 |
| DEFER-MN-07 | `POST /api/v1/path/provision` 与 `POST /api/v1/import-repo/remove` | 不是 browser 功能 | 明确的 CLI 需求 | 未来 Libra 日期计划 |
| DEFER-MN-08 | mega2 仓内的 Libra 黑盒用例（`scripts/libra_smoke_storage_only.sh`，`BB-65` 起）与 `interop-smoke` 服务 | 跨仓，由 mega2 侧维护 | 本计划经 `DEP-MN-03` 交付 | mega2 plan-20261001（`DEP-BB-04`、`DEFER-BB-03`） |
| DEFER-MN-09 | MN-07 的 12 个 live 门（仅在 `DEP-MN-04` 超时后、经规范性修订启用） | 真实实例不可用 | 实例可用 | 本计划修订或后续计划 |

## 完成判据

计划只有在以下条件全部满足后才能标记完成：

- [x] 所有任务卡满足粒度规则 `G-*`：没有未登记的 L 例外、没有 XL 卡、没有碎片卡、没有未登记的合并发布；超限的 AC 分子都带有效的 `@EX-ID`；实现写集冲突均已按实施顺序消解；「任务卡粒度审计表」已填齐。
- [x] `MN-01..MN-12` 的 acceptance criteria 全部满足，且 `Lifecycle=done`、`Acceptance=complete`（ER-04）。停在 `remote-pending` 的卡必须先取得 D 组绿色证据；`blocked` 的卡必须先解除阻塞，或按 `DEFER-*` 正式延后。
- [x] 所有任务的 Verification 命令与「判据规范」中的门都已运行，结果记录在本文「实施证据汇总」。
- [x] **计划完成门（全量收口门，ER-13）**：全部任务卡完成后运行一次 `cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings` 与 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，全绿（L2/L3 未设置环境变量时打印 skipped 可以接受，失败不可以）；另跑 `source .env.test && LIBRA_SITE_MEGA2_DOC=../libra-backend/apps/tanstack-app/content/docs/commands/mega2.en.md cargo nextest run --lib --run-ignored only command::mega2::tests::site_example_paths_are_rooted`，网站示例检查须为 PASS。
- [x] **全量收口门暴露的 Bug 已全部处理（ER-13）**：本计划引起的失败已前滚修复，并重跑全量至全绿；判定为既有失败的用例附有在计划基线 `7f810da` 上同样失败的复现证据，并登记 `FIX-*` 或 `DEFER-*`。
- [x] 文档、兼容矩阵与测试索引的更新已完成（见「兼容与文档收口」）。
- [x] 每张卡的 `Rollback mode` 都已实际验证，或记录了不可验证的原因。
- [x] Codex review 最终结论为 `PASS`，P0/P1 全部关闭；只有 P2 residual risk 可以保留，且有具名接受人（ER-MN-03）。
- [x] 每张卡都已 `patch + 1`，版本面集合（以 `compat_version_surface_sync` 为准）一致，`Cargo.lock` 只由工具链刷新，构建、安装、提交、推送完成，并用 `gh release create v<version>` 触发 `release.yml`，取得 D-MN-STD 证据（ER-08）。
- [x] MN-07 的 12 个 live 门已在 pin 住的真实实例上通过，或已按 `DEP-MN-04` 的降级修订转入 `DEFER-MN-09`。
- [x] `DEP-MN-03` 已在本计划与 `plan-status.md` 登记为「已交付」。
- [x] 「修订历史」记录了成稿后的全部规范性变更（G-09）。
- [x] 本计划没有新建证据文件或文件夹；验收摘要、计数、SHA-256、run ID、门结果与快照都写在本文件内（v2.12）。
- [x] `plan-long.md` 日期计划索引与 `plan-status.md` 已同步。

## 实施证据汇总

> 按卡填写。只写 sanitized 摘要：命令、计数、结果、版本、commit、run ID、时间（`YYYY-MM-DD HH:MM:SS UTC`）。不写 token、凭据、私有绝对路径或原始响应体（ER-11）。

| 任务 | 开工核对（ER-01/ER-02/GC-MN-08） | A/B 门与门族 | C 组（版本、commit、PR、tag） | D 组（run ID、网站标记计数） | review | 更新时间 |
|---|---|---|---|---|---|---|
| MN-10 | 2026-10-02 01:08:49 UTC：`libra status --short --branch` 为 `## main...origin/main`、工作区干净，HEAD `f28dafc`；ER-02 锚点与卡一致：解析回显在 `mega2_tree.rs:88-92`，凭据检查在 `:110-115`，既有 URL 单元测试在 `:648-667`，三处文档承诺在 `docs/commands/mega2.md:174`、`docs/commands/zh-CN/mega2.md:147`、网站 `mega2.en.md:156`；分支 `mn-10-url-parse-no-echo` | A 组：`cargo test --lib internal::protocol::mega2_tree` 16/16（含新 `url_parse_error_does_not_echo_input`）；`cargo test --test command_test mega2_browser_cli` 10/10（含新 `malformed_server_url_is_not_echoed`）。负对照：已安装的官方 0.30.5 对同一输入在 stderr 与 JSON 信封中回显 `MARKER`。新消息为 `invalid mega2 server URL: <url::ParseError>`，`url` 2.5.8 的 `ParseError` 各变体都是固定文本（`parser.rs:88-99`）；B 组：`implementation` 无额外门 | ① `compat_version_surface_sync` 2/2（三处 0.30.13 一致）；② bump 至 0.30.14（`Cargo.toml`、`install.sh`、`install.ps1`）；③ `cargo build` 刷新 `Cargo.lock`，只改 libra 包的版本行；④ `cargo +nightly fmt --all --check` 0；`cargo clippy --all-targets --all-features -- -D warnings` 0；T-1 全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`（run `9386314d-4515-4c48-a791-635a1fe4a652`）8435 run：8435 passed（18 slow）、4 skipped、0 flaky，976.0 s；⑤ `cargo build --release` 2m 22s；⑥ 隔离安装 `cargo install --locked --path . --root <会话临时目录>/install --target-dir target`，安装后 `libra --version` 为 `libra 0.30.14`，对 `https://user:MARKER@mega2.example.com:notaport` 以 `--json` 运行得 `LBR-CLI-003`、退出码 129、消息 `invalid mega2 server URL: invalid port number`，不含 `MARKER`（不覆盖 `~/.libra/bin` 的官方签名安装，沿用 issues/577 的隔离安装做法）；⑦ 提交 `7600cc3`（`gpgsig` 与 `Signed-off-by` 校验通过）；⑧ 推送 `mn-10-url-parse-no-echo`，PR #593，head `7600cc3` 上 `base.yml` 7 个 job 与 CodeQL 全绿（12/12 检查），2026-10-02 03:19 UTC squash merge 为 `80f58b4`（树与 `7600cc3` 相同）；⑨ `gh release create v0.30.14 --target 80f58b4`，tag 指向 `80f58b4` | ① `codeql.yml` run `36959660259`（push `main` `80f58b4`）success（security-codeql-rust、security-codeql-actions）；② `release.yml` run `36959675389`（push tag `v0.30.14`）8/8 success：build-and-upload ×4、upload-install-scripts、update-homebrew-tap、verify-homebrew-formula、request-stable-manifest；③ `https://download.libra.tools/libra/releases/v0.30.14/libra-linux-amd64` HTTP 200（2026-10-02 03:59:32 UTC）；④ 不写网站，N/A。回滚方式 `forward-only`：未发现缺陷，未触发前滚修复，恢复验证命令即本卡三条 Verification | Codex R1（`gpt-6-sol`、xhigh，2026-10-02 01:17:18 UTC 发起）`VERDICT: PASS`，无发现 | 2026-10-02 03:59:32 UTC |
| MN-01 | 2026-10-02 02:06:30 UTC 开工：`libra status --short --branch` 为 `## mn-10-url-parse-no-echo`、工作区干净（MN-10 已提交 `7600cc3`，PR #593 CI 中），分支 `mn-01-http-failure-details` 叠在其上；`DEP-MN-02`：`../libra-backend` 只有 `.git`、分支 `cf`，`git fetch origin cf` 后落后 4 个提交、无超前，`git merge --ff-only origin/cf` 到 `83d620c`（= `ls-remote` 的远端 `cf`），未跟踪 `.teamx/` 不属本计划；ER-02：七处 `.send()` 在 `mega2_tree.rs:329`（MN-10 使其从 `:326` 下移 3 行）、`mega2_entry.rs:173`、`mega2_mutate.rs:376`、`mega2_tag.rs:397,416,426,440`，其余锚点不变 | 门族 37/37：G1–G4、G27–G29 与新增的模板钉住用例 `cargo test --lib internal::protocol::mega2_diag` 8/8；G5、G6、G23、G24、G25、G26 按卡内命令全部 OK；G7–G22、G30–G37 与 AC-2 `cargo test --test command_test -- mega2_tree_transport_test::failure_details_ … mega2_browser_cli_test::json_error_envelope_carries_http_details` 25/25；AC-1 `cargo test --test command_test mega2_` 83/83。实现：八个公开方法改为 `mega2_diag::run(<端点>, self.<方法>_scoped(..)).await`，原函数体原样移入私有 `<方法>_scoped`（不改缩进，满足 G23），七处发送改为 `mega2_diag::send(<请求>, transport_error)`；`mega2_diag` 的固定路由复用客户端路由常量。真实二进制对 500 mock：stderr `{"ok":false,"error_code":"LBR-NET-002",…,"details":{"http_status":500,"method":"GET","route":"/api/v1/tree"}}`、退出码 128。网站：`pnpm typecheck` 0、`pnpm build` 0、本地 `wrangler dev`（命令行传入仅本地有效的 `AUDIT_HASH_SECRET`，不写文件）页面 HTTP 200、标记计数 3；B 组：无 | MN-10 的 `gh release create` 触发后开始：`libra fetch` 后 `libra reset --mixed 80f58b4`（MN-10 的 squash 提交，树与 `7600cc3` 相同），工作区只剩本卡改动；① `compat_version_surface_sync` 2/2（三处 0.30.14）；② bump 至 0.30.15；③ `cargo build` 刷新 `Cargo.lock`，只改 libra 版本行；④ fmt 0；clippy `-D warnings` 0；在已 bump 的树上原样重跑 G5、G6、G23、G24、G25、G26，全部 OK；T-1 全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`（run `65205b33-94b3-4dd9-b300-f009cac51056`）8468 run：8468 passed（21 slow）、4 skipped、0 flaky，1125.7 s；⑤ `cargo build --release` 2m 30s；⑥ 隔离安装 `cargo install --locked --path . --root <会话临时目录>/install --target-dir target` 得 `libra 0.30.15`，对 500 mock 以 `--machine` 运行，stderr 信封带 `"details":{"http_status":500,"method":"GET","route":"/api/v1/tree"}`、退出码 128、stdout 为空；⑦ 提交 `b0305a0`（`gpgsig`、`Signed-off-by` 校验通过）；网站 `cf` 签名提交 `973f650`（`%G?`=G，带 `Signed-off-by`），推送前远端 `cf` = `83d620c`，推送后 = `973f650`；⑧ PR #595，head `b0305a0` 上 12/12 检查通过（`base.yml` 7 个 job 与 CodeQL），2026-10-02 05:26 UTC squash merge 为 `6e86578`（树与 `b0305a0` 相同）；⑨ `gh release create v0.30.15 --target 6e86578` | ① `codeql.yml` run `36968913909`（push `main` `6e86578`）success；② `release.yml` run `36968922078`（tag `v0.30.15`）8/8 success；③ v0.30.15 `libra-linux-amd64` HTTP 200（2026-10-02 05:52:56 UTC）；④ 网站：`cf@973f650` 部署后按 ER-MN-02 ④ 核对 `https://libra.tools/en/docs/commands/mega2`，HTTP 200、标记 `Machine error details` 计数 3（2026-10-02 05:28:05 UTC）。回滚方式 `immutable-release`：未触发 | Codex R1（2026-10-02 02:13:17 UTC 发起）`PASS`，2×P2：诊断路由改为复用客户端路由常量并在路由门断言实际请求行；中段门另断言 `NetworkUnavailable` 以区分读取中断与解析失败——均已修复；R2 差异确认（02:21:31 UTC 发起）`PASS`，无发现 | 2026-10-02 05:52:56 UTC |
| MN-02 | 2026-10-02 04:03 UTC 开工（MN-01 已提交 `b0305a0`、PR #595 CI 中）：分支 `mn-02-list-operation` 叠在其上，工作区干净；`DEP-MN-02`：`../libra-backend` 只有 `.git`、在 `cf`、与 `origin/cf` 同步于 `973f650`（含 MN-01 的网站提交），未跟踪 `.teamx/` 不属本计划；ER-02：入口与 JSON 分支在 `src/command/mega2.rs:180-210`，payload `:110-118`，示例 `:37,48`，TTY 门 `mega2_browser/mod.rs:563-613` 与卡一致 | A 组：`cargo test --lib command::mega2` 18 passed、1 ignored（新增 `list_payload_carries_operation`、`help_example_paths_are_rooted`、`doc_example_paths_are_rooted`；改文档前 `doc_example_paths_are_rooted` 因 `src/pkg` 失败，证明检查有效）；`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2::tests::site_example_paths_are_rooted -- --exact --ignored` ok（变量未设置时 panic `NotPresent`，无跳过分支）；`cargo test --test command_test mega2_browser_noninteractive_test` 16/16（门族 G1–G11 即 `op_rule_list_*` 11 个，`list_*` 5 个）；`cargo test --test command_test mega2_` 99/99（既有用例未改）；`compat_help_examples_banner` 1/1、`compat_command_docs_examples_section` 1/1、`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43；三处兼容行原位改写、行数不变（763/279/149）；网站 `pnpm typecheck` 0、`pnpm build` 0、本地预览 HTTP 200、标记 `Non-interactive operations` 计数 4；B 组：无 | MN-01 的 `gh release create` 触发后开始：`libra reset --mixed 6e86578`（树与 `b0305a0` 相同）；① `compat_version_surface_sync` 2/2（三处 0.30.15）；② bump 至 0.30.16；③ `Cargo.lock` 只改 libra 版本行；④ fmt 0；clippy 0；T-4 全量 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`（run `d3f354ce-05e7-49c8-9cf3-f972cfbe0ce3`）8489 run：8489 passed（25 slow、1 flaky、1 leaky）、5 skipped，1147.2 s；flaky 为既有的 `mega2_browser_mutate_test::confirmed_delete_and_rename_post_once_then_reload_once`（TRY 1 FAIL、TRY 2 PASS），leaky 为无关的 `internal::upgrade::txn::tests::fresh_install_probe_failure_aborts_and_leaves_nothing`；另跑 `source .env.test && LIBRA_SITE_MEGA2_DOC=… cargo nextest run --lib --run-ignored only command::mega2::tests::site_example_paths_are_rooted` 得 `PASS (1/1)`；⑤ `cargo build --release` 2m 13s；⑥ 隔离安装得 `libra 0.30.16`，人读 `--list /`（stdin 为 `/dev/null`）对 mock 输出 `dir  alpha` / `file  zeta.txt`、退出码 0；⑦ 提交 `dbdde08`（`gpgsig`、`Signed-off-by` 校验通过）；网站 `cf` 签名提交 `23c21fc`（推送前远端 `973f650`，推送后 `23c21fc`）；⑧ PR #596，head `dbdde08` 上 12/12 检查通过，2026-10-02 07:28 UTC squash merge 为 `36c05be`（树与 `dbdde08` 相同）；⑨ `gh release create v0.30.16 --target 36c05be` | ① `codeql.yml` run `36978751927`（push `main` `36c05be`）success；② `release.yml` run `36978760075`（tag `v0.30.16`）8/8 success；③ v0.30.16 `libra-linux-amd64` HTTP 200（2026-10-02 08:00:39 UTC）；④ 网站：`cf@23c21fc` 部署后 HTTP 200、标记 `Non-interactive operations` 计数 4（2026-10-02 07:30:24 UTC）。回滚方式 `immutable-release`：未触发 | Codex R1（2026-10-02 04:09:22 UTC 发起）`FAIL`：P1 登记表未驱动类别规则与分派 → 改为操作只经 `OPERATIONS` 按名解析规格、类别规则读规格的 `access`，新增 `every_operation_flag_has_exactly_one_registry_row`（内省真实 `Cli` 的 `operation` 组）与 `operations_resolve_through_the_registry`；P2 R4a 的 500 不区分路由 → `fail_500_on` 只对本操作的方法与路径回 500、其余回 418；P2 文档宣称「每项功能」→ 改为「下表列出本版本已提供的形式」（EN/zh/网站）。修订后 lib 20 passed、1 ignored，集成 16/16；R2（04:26:02 UTC 发起）`PASS`，无发现。另：`cargo test --test command_test mega2_` 三次中一次失败于既有用例 `mega2_browser_mutate_test::confirmed_delete_and_rename_post_once_then_reload_once`（单独 40 次失败 2 次）：其 mock 只 `read()` 一次，请求头与请求体分段到达时取不到请求体；该文件不在本计划写集、未改动，R2 判定与 MN-02 无因果关系，留待收口全量门处理 | 2026-10-02 04:28 UTC |
| MN-03 | 2026-10-02 06:14 UTC 开工（MN-02 已提交 `dbdde08`、PR #596 CI 中）：分支 `mn-03-create-dir` 叠在其上，工作区干净；GC-MN-08 重核 `DEP-MN-01`：`../mega2`（Libra 仓库）`main` 已由 pin `8ff880c` 前移到 `d9d69b7`（v0.41.20），`libra diff --name-only 8ff880c HEAD` 覆盖契约文件（`preview_router.rs`、`api_router.rs`、`tag_router.rs`、`model/git.rs`、`model/tag.rs`、`mono_api_service.rs`、`docs/refactoring/directory-entry-api.md`、`mono_storage.rs`、`config/config.toml`）只列出 `config/config.toml`（默认 `push_policy` 改为 trunk、样例 `[git]` 启用 `push_auth = "none"`，属部署默认值，不是路由或 DTO 变化），pin 保持 `8ff880c`；`DEP-MN-06`：`src/cli.rs` 不在任何在开 PR（#579、#594）或本地未提交改动中，`DEP-AD-12 / DEP-CLI-mirror` 名单内其它卡无「已改未推」，本卡只改 `src/cli.rs:2006-2009` 的注释、行数不变（4815）；`DEP-MN-02`：网站在 `cf@23c21fc`（含 MN-02），与远端同步 | A 组：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 22/22（含新 `create_dir_payload_separates_target_and_receipt` 与网站示例检查）；`cargo test --test command_test mega2_browser_noninteractive_test` 37/37（门族 G1–G14 即 14 个 `op_rule_create_dir_*`，fixture 门 G15–G18，`create_dir_request_record`/`create_dir_json_payload`/`create_dir_human_output`）；`cargo test --test command_test mega2_` 120/120（既有 TUI、CLI 与守卫用例未改）；四个兼容 target 全绿；三处兼容行原位改写、行数不变；R2 的 fixture 在 `commit_id` 中放 ESC，证明人读摘要经 `sanitize`；网站 typecheck 0、build 0、本地预览 HTTP 200、标记 `--create-dir` 计数 5；B 组：无 | MN-02 的 `gh release create` 触发后开始：`libra reset --mixed 36c05be`（树与 `dbdde08` 相同）；① `compat_version_surface_sync` 2/2（三处 0.30.16）；② bump 至 0.30.17；③ `Cargo.lock` 只改 libra 版本行；④ fmt 0；clippy 0；Full-suite trigger 为 `none`，按卡内「C 组第 ④ 步」逐条以 nextest 运行：`--lib --run-ignored all command::mega2` 22/22（含网站示例检查），`create_dir_` 9/9，`op_rule_create_dir_` 14/14，`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34，`compat_help_examples_banner` 1/1，`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 17s；⑥ 隔离安装得 `libra 0.30.17`，人读 `--create-dir pkg /src` 对 mock 输出 `created directory /src/pkg (commit c-1)`、退出码 0，mock 收到 `POST /api/v1/create-entry`、请求体 `{"is_directory":true,"name":"pkg","path":"/src","content":null,"skip_build":true}`；⑦ 提交 `857f885`（`gpgsig`、`Signed-off-by` 校验通过）；网站 `cf` 签名提交 `a31ced6`（推送前远端 `23c21fc`，推送后 `a31ced6`）；⑧ PR #597，head `857f885` 上 12/12 检查通过，2026-10-02 09:03:34 UTC squash merge 为 `548a33c`（树与 `857f885` 相同）；⑨ `gh release create v0.30.17 --target 548a33c`（发布于 2026-10-02 09:03:41 UTC） | ① `codeql.yml` run `36987587577`（push `main` `548a33c`）success；② `release.yml` run `36987596125`（tag `v0.30.17`）8/8 success；③ v0.30.17 `libra-linux-amd64` HTTP 200（2026-10-02 09:36:00 UTC）；④ 网站：`cf@a31ced6` 部署后 HTTP 200、标记 `--create-dir` 计数 5（2026-10-02 09:05:14 UTC）。回滚方式 `immutable-release`：未触发 | Codex R1（2026-10-02 06:20:39 UTC 发起）`FAIL`：P1 匿名写遇 401 时客户端提示用 `--token-file`/`LIBRA_MEGA2_TOKEN`，而本卡的非交互写拒绝它们 → `anonymous_write_error` 对 `LBR-AUTH-001` 保留 message、stable code 与全部 `details`，只把提示换成「用带 `--token-file` 的交互式 browser，或 `push_auth=none` 的服务端」，回归用例 `create_dir_anonymous_401_hint`（MN-11 引入写凭据后原提示恢复成立）；P1 文档把断连都归为 `details.transport` → EN/zh/网站改为列出三种结果未知的阶段（响应前失败 `LBR-NET-001`+`transport`、读取回执断连 `LBR-NET-001`+`http_status`、2xx 回执不合规 `LBR-NET-002`），并注明 `transport: connect` 表示请求未到达服务端，回归用例 `create_dir_receipt_read_drop_reports_http_status`；P2 EN 与网站选项表的 `--json` 改为「one request」。修订后 lib 22/22、`mega2_` 122/122、`create_dir_` 9/9、四个兼容 target 全绿；R2（2026-10-02 06:30:56 UTC 发起）`PASS`，无发现 | 2026-10-02 09:24:15 UTC |
| MN-11 | 2026-10-02 07:52 UTC 开工（MN-03 已提交 `857f885`、PR #597 CI 中）：分支 `mn-11-write-credentials` 叠在其上；GC-MN-08：mega2 契约文件自 pin `8ff880c` 未变（同 MN-03 的核对）；`DEP-MN-02`：网站 `cf@a31ced6`（含 MN-03）与远端同步；实施中发现 AC-2 的第 329 行替换文本不能通过 rustfmt（见修订历史 2026-10-02 07:55:47 UTC），已修订为 `assert!(err.contains("no credentials"), …);` | A 组：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 23/23（含新 `token_gate_accepts_write_operations_only`）；门族 G1–G7 与 AC-1、R5 回归：`cargo test --test command_test -- …op_rule_create_dir_r9 …create_dir_token_ …list_token_refusal_message …op_rule_list_r5` 12/12；AC-2 差异判定输出 `OK: byte-identical to HEAD plus the four specified line replacements`，且该文件 `rustfmt --check` 通过；AC-3 与其余既有用例：`cargo test --test command_test mega2_` 127/127（含评审追加的 `create_dir_missing_token_file_sends_nothing`、`create_dir_empty_token_file_sends_nothing`）；四个兼容 target 全绿；三处兼容行原位改写、行数不变；网站 typecheck 0、build 0、本地预览 HTTP 200、标记 `Read operations refuse token flags` 计数 3；`op_rule_create_dir_r5a_*`/`r5b_*` 四门按卡删除；MN-03 的临时 401 提示改写 `anonymous_write_error` 随写凭据生效而删除；B 组：无 | MN-03 的 `gh release create` 触发后开始：`libra reset --mixed 548a33c`（树 `fcbed6e` 与 `857f885` 相同）；① `compat_version_surface_sync` 2/2（三处 0.30.17）；② bump 至 0.30.18；③ `Cargo.lock` 只改 libra 版本行；④ fmt 0；clippy 0；Full-suite trigger 为 `none`，按卡内「C 组第 ④ 步」逐条以 nextest 运行：`--lib --run-ignored all command::mega2` 23/23（含网站示例检查），`op_rule_create_dir_r9`、`create_dir_token_`、`list_token_refusal_message`、`op_rule_list_r5` 12/12，AC-2 差异判定输出 `OK: byte-identical to HEAD plus the four specified line replacements`，`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34，`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 26s；⑥ 隔离安装得 `libra 0.30.18`，人读 `--create-dir pkg /src --token-file <文件>`（stdin 为 `/dev/null`）对 mock 输出 `created directory /src/pkg (commit c-1)`、退出码 0、stderr 为空，mock 收到 `POST /api/v1/create-entry`，Authorization 恰为一个 `Bearer` 加 token 文件内容；⑦ 提交 `cfa76c6`（`gpgsig`、`Signed-off-by` 校验通过）；网站 `cf` 签名提交 `42def02`（推送前远端 `a31ced6`，推送后 `42def02`）；⑧ PR #598，head `cfa76c6` 上首轮 `compat-offline-core` 失败于 CI 基础设施（keyring 步骤编译依赖 `object_store` 时 runner 找不到 `rustc`：`No such file or directory (os error 2)`；其前的 L1–L3 测试步骤全部通过），`gh run rerun 36989913569 --failed` 重跑后 12/12 检查通过，2026-10-02 11:43 UTC squash merge 为 `6230874`（树 `6286a4d` 与 `cfa76c6` 相同）；⑨ `gh release create v0.30.18 --target 6230874`（发布于 2026-10-02 11:43:19 UTC） | ① `codeql.yml` run `37002556477`（push `main` `6230874`）success；② `release.yml` run `37002565759`（tag `v0.30.18`）8/8 success；③ v0.30.18 `libra-linux-amd64` HTTP 200（2026-10-02 12:16:22 UTC）；④ 网站：`cf@42def02` 部署后 HTTP 200、标记 `Read operations refuse token flags` 计数 3（2026-10-02 09:34:00 UTC） | Codex R1（2026-10-02 07:59:23 UTC 发起）`FAIL`：P1 凭据只在 `CreateDir` 分支解析 → 改为类别校验后按 `spec.access` 统一解析一次再交给写客户端；P1 兼容表与开发文档仍写匿名 POST、token 与 `--json` 互斥 → 改写；P1 缺少 token 文件不可读/为空时零请求、不回退环境变量的真实二进制用例 → 新增两例；R1 认可 AC-2 的修订。R2（08:07:14 UTC 发起）`PASS`，无发现 | 2026-10-02 12:16:22 UTC |
| MN-04 | 2026-10-02 09:28 UTC 开工（MN-11 已提交 `cfa76c6`、PR #598 CI 中）：分支 `mn-04-delete-dir` 叠在其上，工作区干净；GC-MN-08：`../mega2` HEAD 已前移到 `2a168c3`（v0.41.24），`libra diff --name-only 8ff880c HEAD` 在契约文件中仍只列出 `config/config.toml`（部署默认值与新增的 S3 `public_endpoint_url` 注释样例，不涉路由或 DTO），delete-entry 路由 `preview_router.rs:173`、`DeleteEntryInfo`/`DeleteEntryResult` 在 `model/git.rs:316,343`、按 mode 匹配在 `mono_api_service.rs:1438-1490`（文件目标 400、缺失目标 404）与卡一致，pin 保持 `8ff880c`；`DEP-MN-02`：网站 `cf@42def02`（含 MN-11）与远端同步；ER-02：delete 客户端 `mega2_mutate.rs:387-407`、请求体 `:79-84,146-152`、状态映射 `:222-286`、TUI `d` 的 `perform_delete` 在 `mega2_browser/mod.rs:661-673` 与卡一致 | A 组：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 24/24（含新 `delete_dir_payload_separates_target_and_receipt` 与网站示例检查）；`cargo test --test command_test mega2_browser_noninteractive_test::delete_dir_` 9/9（fixture 门 G14–G19 与 `delete_dir_request_record`/`delete_dir_json_payload`/`delete_dir_human_output`）；`…::op_rule_delete_dir_` 13/13（规则门 G1–G13）；整个 `mega2_browser_noninteractive_test` 66/66（`fail_500_on` 改为 `fail_on(case, status, body)` 后既有门不变）；`-- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34（未改）；`mega2_` 149/149；`compat_help_examples_banner` 1/1、`compat_command_docs_examples_section` 1/1、`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43；三处兼容行原位改写、行数不变（763/279/149）；网站 typecheck 0、build 0、本地预览 HTTP 200、标记 `--delete-dir` 计数 5；同卡顺带订正开发文档中 MN-11 遗留的「`create_dir`：R1–R5b、R7、R8」为 R1–R4b、R7、R8、R9a–R9c；B 组：无 | MN-11 CI 等待期间，在本卡工作区之上以叠加方式预做并评审了 MN-08、MN-12、MN-05（各自终版另存），开始本组前从评审通过时的快照逐字节恢复本卡的 11 个文件与网站页（`cmp` 全部一致），并 `touch` 迫使重新编译；MN-11 的 `gh release create` 触发后开始：`libra reset --mixed 6230874`（树 `6286a4d` 与 `cfa76c6` 相同）；① `compat_version_surface_sync` 2/2（三处 0.30.18）；② bump 至 0.30.19；③ `Cargo.lock` 只改 libra 版本行；④ fmt 0；clippy 0；Full-suite trigger 为 `none`，按卡内「C 组第 ④ 步」逐条以 nextest 运行：`--lib --run-ignored all command::mega2` 24/24（含网站示例检查），`delete_dir_` 9/9，`op_rule_delete_dir_` 13/13，`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34，`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 5s；⑥ 隔离安装得 `libra 0.30.19`，人读 `--delete-dir pkg /src`（stdin 为 `/dev/null`）对 mock 输出 `deleted directory /src/pkg (commit c-1)`、退出码 0、stderr 为空，mock 收到 `POST /api/v1/delete-entry`、无 Authorization、请求体 `{"path":"/src","name":"pkg","skip_build":true}`；⑦ 提交 `018d1d1`（`gpgsig`、`Signed-off-by` 校验通过）；网站 `cf` 签名提交 `1f7aa03`（推送前远端 `42def02`，推送后 `1f7aa03`）；⑧ PR #599，head `018d1d1` 上 12/12 检查通过，2026-10-02 13:11:36 UTC squash merge 为 `81d1655`（树 `6352156` 与 `018d1d1` 相同）；⑨ `gh release create v0.30.19 --target 81d1655`（发布于 2026-10-02 13:11:43 UTC） | ① `codeql.yml` run `37011373062`（push `main` `81d1655`）success；② `release.yml` run `37011384218`（tag `v0.30.19`）8/8 success；③ v0.30.19 `libra-linux-amd64` HTTP 200（2026-10-02 13:37:24 UTC）；④ 网站：`cf@1f7aa03` 部署后 HTTP 200、标记 `--delete-dir` 计数 5（2026-10-02 13:14:08 UTC）。回滚方式 `immutable-release`：未触发 | Codex R1（2026-10-02 09:32:46 UTC 发起）`PASS`，3×P2：① 登记表的 `endpoint` 未驱动分派 → 不改，理由：`operations_resolve_through_the_registry` 钉住 `spec.endpoint == mega2_diag::DELETE_ENTRY`，`delete_directory` 在 `mega2_diag::run(DELETE_ENTRY, …)` 内发请求（MN-01 结构门），`op_rule_delete_dir_r4a` 以只对本方法与路径回 500 的 mock 断言真实请求的 `details` 方法与路由，两侧不一致即在发布前失败；改为由 `spec.endpoint` 驱动需改协议客户端，不在本卡写集；② 证据表多出 MN-04 占位行、`plan-status.md` 仍为 pending → 删除占位行并同步；③ 兼容行「no local state is read or written」忽略了 `--token-file` 的读取 → 原位改为「no local repository state or configuration is read or written — the only local read is the `--token-file` a write names」（行数不变，`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43 复跑通过）；R2 差异确认（2026-10-02 09:43:09 UTC 发起）`PASS`，无发现；其后对照 mega2 错误映射表，把三处用户文档 `--delete-dir` 错误表的 400 行由「NAME 是文件」放宽为「NAME 是文件、PATH 途经文件，或服务端拒绝该路径（例如 trunk 服务端上的顶层目录）」，R3 只读文档差异确认（2026-10-02 09:50:37 UTC 发起）`PASS`，无发现 | 2026-10-02 13:37:24 UTC |
| MN-08 | 2026-10-02 12:06 UTC 开工（MN-04 已提交 `018d1d1`、PR #599 CI 中）：分支 `mn-08-move-dir` 叠在其上；本卡改动已于 MN-11 CI 等待期间（2026-10-02 10:48–10:58 UTC）在 MN-04 工作区之上以叠加方式实现、跑过 A 组并经 Codex 评审，终版另存；开工时先核对 MN-04 提交中本卡写集的 9 个文件与网站页逐字节等于该叠加所基于的 MN-04 快照（`cmp` 全部一致），再拷入终版并 `touch`；GC-MN-08：`../mega2` HEAD 已前移到 `b045528`，`libra diff --name-only 8ff880c HEAD` 在契约文件中仍只列出 `config/config.toml`，move-entry 路由 `preview_router.rs:201`、`MoveEntryInfo`/`MoveEntryResult` 在 `model/git.rs:356,402`、契约页 move 规则与错误映射与卡一致，pin 保持 `8ff880c`；`DEP-MN-02`：网站 `cf@1f7aa03`（含 MN-04）与远端同步；ER-02：锚点有行号漂移（MN-01 的作用域改造所致，内容一致）：move 客户端现为 `mega2_mutate.rs:426-472`（卡写 `:411-441`），请求体 `:87-94,156-170`，TUI `m` 的编辑器与 `perform_move` 在 `mega2_browser/mod.rs:247-279,680-697` | 叠加层（MN-04 工作区之上）A 组：lib 25/25、`move_dir_` 9/9、`op_rule_move_dir_` 13/13、`mega2_` 171/171、四个兼容 target 全绿；拷入终版后在真实基线上复跑：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 25/25（含新 `move_payload_separates_target_and_receipt` 与网站示例检查）；`…::move_dir_` 9/9（fixture 门 G14–G19 与 `move_dir_request_record`/`move_dir_json_payload`/`move_dir_human_output`）；`…::op_rule_move_dir_` 13/13（规则门 G1–G13）；整个 `mega2_browser_noninteractive_test` 88/88；`-- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34（未改）；`mega2_` 171/171；`compat_help_examples_banner` 1/1、`compat_command_docs_examples_section` 1/1、`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43；三处兼容行原位改写、行数不变（763/279/149）；网站 typecheck 0、build 0、本地预览 HTTP 200、标记 `--move-dir` 计数 5；B 组：无 | MN-04 的 `gh release create` 触发后开始：从叠加层快照逐字节恢复本卡的 9 个文件与网站页（`cmp` 全部一致，并 `touch`）；`libra reset --mixed 81d1655`（树 `6352156` 与 `018d1d1` 相同）；① `compat_version_surface_sync` 2/2（三处 0.30.19）；② bump 至 0.30.20；③ `Cargo.lock` 只改 libra 版本行；④ fmt 0；clippy 0；Full-suite trigger 为 `none`，按卡内「C 组第 ④ 步」逐条以 nextest 运行：`--lib --run-ignored all command::mega2` 25/25（含网站示例检查），`move_dir_` 9/9，`op_rule_move_dir_` 13/13，`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34，`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 14s；⑥ 隔离安装得 `libra 0.30.20`，人读 `--move-dir pkg /lib /src`（stdin 为 `/dev/null`）对 mock 输出 `moved directory /src/pkg -> /lib/pkg (commit c-1)`、退出码 0、stderr 为空，mock 收到 `POST /api/v1/move-entry`、无 Authorization、请求体 `{"from_path":"/src","from_name":"pkg","to_path":"/lib","to_name":"pkg","skip_build":true}`；⑦ 提交 `f66f52d`（`gpgsig`、`Signed-off-by` 校验通过）；网站 `cf` 签名提交 `cea2d47`（推送前远端 `1f7aa03`，推送后 `cea2d47`）；⑧ PR #600，head `f66f52d` 上首轮 `compat-network-remotes` 失败于外网抖动（`github_lfs_batch_download_returns_response` 连接 github.com 时 `tls handshake eof`，其余 11 项通过），按集成测试指南「外网抖动允许重试一次」`gh run rerun 37013533148 --failed` 后 12/12 通过，2026-10-02 15:07:25 UTC squash merge 为 `36e826a`（树 `9f642e0` 与 `f66f52d` 相同）；⑨ `gh release create v0.30.20 --target 36e826a`（发布于 2026-10-02 15:07:34 UTC） | ① `codeql.yml` run `37024735662`（push `main` `36e826a`）success；② `release.yml` run `37024751607`（tag `v0.30.20`）8/8 success；③ v0.30.20 `libra-linux-amd64` HTTP 200（2026-10-02 15:40:05 UTC）；④ 网站：`cf@cea2d47` 部署后 HTTP 200、标记 `--move-dir` 计数 5（2026-10-02 15:10:00 UTC）。回滚方式 `immutable-release`：未触发 | Codex R1（2026-10-02 10:53:33 UTC 发起，评审对象即本卡终版相对 MN-04 快照的差异）`PASS`，无发现 | 2026-10-02 15:40:05 UTC |
| MN-12 | 2026-10-02 13:31 UTC 开工（MN-08 已提交 `f66f52d`、PR #600 CI 中）：分支 `mn-12-rename-dir` 叠在其上；本卡改动已于 MN-11 CI 等待期间（2026-10-02 11:00–11:16 UTC）在 MN-08 叠加层之上实现、跑过 A 组并经 Codex 评审，终版另存；开工时先核对 MN-08 提交中本卡写集的 9 个文件与网站页逐字节等于该叠加所基于的 MN-08 终版（`cmp` 全部一致），再拷入本卡终版并 `touch`；GC-MN-08：`../mega2` HEAD 已前移到 `0d03ccc`（v0.41.31），`libra diff --name-only 8ff880c HEAD` 在契约文件中仍只列出 `config/config.toml`，契约页「源目标相同 400」与 move-entry 路由、DTO 与卡一致，pin 保持 `8ff880c`；`DEP-MN-02`：网站 `cf@cea2d47`（含 MN-08）与远端同步；ER-02：锚点行号漂移（内容一致）：`rename_directory` 现为 `mega2_mutate.rs:475-484`（卡写 `:443-453`），TUI `R` 的编辑器在 `mega2_browser/mod.rs:280-312`（卡写 `:277-309`）、`perform_move` 在 `:680-697` | 叠加层 A 组：lib 26/26、`rename_dir_` 9/9、`op_rule_rename_dir_` 13/13、`mega2_` 193/193、四个兼容 target 全绿；拷入终版后在真实基线上复跑：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 26/26（含新 `rename_payload_keeps_parent` 与网站示例检查）；`…::rename_dir_` 9/9（fixture 门 G14–G19 与 `rename_dir_request_record`/`rename_dir_json_payload`/`rename_dir_human_output`）；`…::op_rule_rename_dir_` 13/13（规则门 G1–G13）；整个 `mega2_browser_noninteractive_test` 110/110；`-- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34（未改）；`mega2_` 193/193；`compat_help_examples_banner` 1/1、`compat_command_docs_examples_section` 1/1、`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43；三处兼容行原位改写、行数不变（763/279/149）；网站 typecheck 0、build 0、本地预览 HTTP 200、标记 `--rename-dir` 计数 5；B 组：无 | MN-08 的 `gh release create` 触发后开始：本卡工作区为叠加层快照恢复后的状态（9 个文件与网站页 `cmp` 全部一致；CLAUDE.md 与集成测试指南按 HEAD 恢复）；`libra reset --mixed 36e826a`（树 `9f642e0` 与 `f66f52d` 相同）；① `compat_version_surface_sync` 2/2（三处 0.30.20）；② bump 至 0.30.21；③ `Cargo.lock` 只改 libra 版本行；④ fmt 0；clippy 0；Full-suite trigger 为 `none`，按卡内「C 组第 ④ 步」逐条以 nextest 运行：`--lib --run-ignored all command::mega2` 26/26（含网站示例检查），`rename_dir_` 9/9，`op_rule_rename_dir_` 13/13；`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 首跑败于既有 flaky `mega2_browser_mutate_test::confirmed_delete_and_rename_post_once_then_reload_once`（MN-02 已记录：mock 只 `read()` 一次；该文件不在本计划写集），nextest 随即停止（25/34 已跑、24 通过），原样复跑 34/34 通过；`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 31s；⑥ 隔离安装得 `libra 0.30.21`，人读 `--rename-dir old new /src`（stdin 为 `/dev/null`）对 mock 输出 `renamed directory /src/old -> /src/new (commit c-1)`、退出码 0、stderr 为空，mock 收到 `POST /api/v1/move-entry`、无 Authorization、请求体 `{"from_path":"/src","from_name":"old","to_path":"/src","to_name":"new","skip_build":true}`；⑦ 提交 `c5c16cf`（`gpgsig`、`Signed-off-by` 校验通过）；网站 `cf` 签名提交 `8f0e356`（推送前远端 `cea2d47`，推送后 `8f0e356`）；⑧ PR #601，head `c5c16cf` 上 12/12 检查通过，2026-10-02 16:37:35 UTC squash merge 为 `fb1cf80`（树 `e9c0b5f` 与 `c5c16cf` 相同）；⑨ `gh release create v0.30.21 --target fb1cf80`（发布于 2026-10-02 16:37:42 UTC） | ① `codeql.yml` run `37035206595`（push `main` `fb1cf80`）success；② `release.yml` run `37035218493`（tag `v0.30.21`）8/8 success；③ v0.30.21 `libra-linux-amd64` HTTP 200（2026-10-02 17:09:37 UTC）；④ 网站：`cf@8f0e356` 部署后 HTTP 200、标记 `--rename-dir` 计数 5（2026-10-02 16:41:31 UTC）。回滚方式 `immutable-release`：未触发 | Codex R1（2026-10-02 11:10:55 UTC 发起，评审对象即本卡终版相对 MN-08 终版的差异）`PASS`，无发现 | 2026-10-02 17:09:37 UTC |
| MN-05 | 2026-10-02 15:30 UTC 开工（MN-12 已提交 `c5c16cf`、PR #601 CI 中）：分支 `mn-05-list-tags` 叠在其上；本卡改动已于 MN-11 CI 等待期间（2026-10-02 11:17–11:45 UTC）在 MN-12 叠加层之上实现、跑过 A 组并经 Codex 评审（R1 `FAIL` 后修订，R2、R3 `PASS`），终版另存；开工时先核对 MN-12 提交中本卡写集的 9 个文件与网站页逐字节等于该叠加所基于的 MN-12 终版（`cmp` 全部一致），再拷入本卡终版并 `touch`；GC-MN-08：`../mega2` HEAD 已前移到 `c9ae6df`（v0.41.34），`libra diff --name-only 8ff880c HEAD` 在契约文件中仍只列出 `config/config.toml`，tags/list 路由、`TagInfo` 字段与 `list_tags` 分页行为（`mono_api_service.rs` 的 `list_tags`、`create_tag` 写 ref）与卡及本卡文档一致，pin 保持 `8ff880c`；`DEP-MN-02`：网站 `cf@8f0e356`（含 MN-12）与远端同步；ER-02：锚点行号漂移（内容一致）：`list_tags` 现为 `mega2_tag.rs:386-404`（卡写 `:385-403`），`validate_pagination` 在 `:196-210`，`TagInfo` 在 `:125-133`，TUI 面板取页在 `mega2_browser/mod.rs:701-711`，`sanitize` 在 `mod.rs:493-497`，`TAG_PAGE_SIZE` 在 `tag_panel.rs:14` | 叠加层 A 组：lib 27/27、`list_tags_` 12/12、`op_rule_list_tags_` 17/17、`mega2_` 222/222、四个兼容 target 全绿；拷入终版后在真实基线上复跑：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 27/27（含新 `list_tags_payload_computes_has_next`、帮助/文档/网站示例检查）；`…::list_tags_` 12/12（fixture 门 G18–G25 与两个请求记录用例、`list_tags_json_payload`、`list_tags_human_output_is_sanitized`）；`…::op_rule_list_tags_` 17/17（规则门 G1–G17）；整个 `mega2_browser_noninteractive_test` 139/139；`-- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34（未改，含 GC-MN-06 帮助守卫）；`mega2_` 222/222；`compat_help_examples_banner` 1/1、`compat_command_docs_examples_section` 1/1、`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43；三处兼容行原位改写、行数不变（763/279/149）；网站 typecheck 0、build 0、本地预览 HTTP 200、标记 `--list-tags` 计数 9；B 组：无 | 首次（0.30.22，作废）：MN-12 的 `gh release create` 触发后开始：本卡工作区为叠加层快照恢复后的状态（9 个文件与网站页 `cmp` 全部一致；CLAUDE.md 与集成测试指南按 HEAD 恢复）；`libra reset --mixed fb1cf80`（树 `e9c0b5f` 与 `c5c16cf` 相同）；① `compat_version_surface_sync` 2/2（三处 0.30.21）；② bump 至 0.30.22；③ `Cargo.lock` 只改 libra 版本行；④ fmt 0；clippy 0；Full-suite trigger 为 `none`，按卡内「C 组第 ④ 步」逐条以 nextest 运行：`--lib --run-ignored all command::mega2` 27/27（含网站示例检查），`list_tags_` 12/12，`op_rule_list_tags_` 17/17，`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34，`compat_help_examples_banner` 1/1，`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 17s；⑥ 隔离安装得 `libra 0.30.22`，人读 `--list-tags`（stdin 为 `/dev/null`）对 mock 输出 `v1  tag  Ann`、缩进的 message 行与 `page 1 · per_page 20 · total 1`、退出码 0、stderr 为空，mock 收到 `GET /api/v1/tags/list?page=1&per_page=20&path=%2F`、无 Authorization；⑦ 提交 `a4d05cb`（`gpgsig`、`Signed-off-by` 校验通过）；网站 `cf` 签名提交 `01c4ac7`（推送前远端 `8f0e356`）；⑧ PR #603（2026-10-02 17:01:31 UTC 开启）在 head `a4d05cb` 上 12/12 检查通过；会话暂停、尚未合并期间 issues/498 TT-05 的 PR #602 于 2026-10-02 19:42:06 UTC 合入 `main`（`a2f1c34`）并发布 v0.30.22，PR #603 转为冲突，0.30.22 按 ER-MN-01 作废、不复用。重建（0.30.23，作废；修订历史 2026-10-03 01:09:45 UTC）：`libra reset --mixed a2f1c34`；本卡写集中 8 个文件 `cmp` 等于 `a4d05cb`，`COMPATIBILITY.md` 为 `a2f1c34` 的文件只换第 248 行（763 行），`plan-status.md` 为 `a2f1c34` 的文件只重放本计划的行；网站页不变（`cf@01c4ac7` 已上线）；① 基线 `a2f1c34` 三处版本面均为 0.30.22（`libra show` 核对），bump 后 `compat_version_surface_sync` 2/2（三处 0.30.23）；② bump 至 0.30.23；③ `Cargo.lock` 只改 libra 版本行（0.30.22 → 0.30.23）；④ fmt 0；clippy 0；按卡内「C 组第 ④ 步」逐条以 nextest 运行：`--lib --run-ignored all command::mega2` 首跑 26/27——`site_example_paths_are_rooted` 失败，原因是网站工作区已是 MN-06 未提交的页面，其 `--create-tag` 示例不能被本卡的参数解析接受（`unexpected argument found`）；把网站页恢复为 `cf@01c4ac7`（本卡已提交的页面）后同一命令 27/27，随后放回 MN-06 的页面；`list_tags_` 12/12，`op_rule_list_tags_` 17/17，`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34，`compat_help_examples_banner` 1/1，`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 33s；⑥ 隔离安装得 `libra 0.30.23`，人读 `--list-tags`（stdin 为 `/dev/null`）对 mock 输出 `v1  tag  Ann`、缩进的 message 行与 `page 1 · per_page 20 · total 1`、退出码 0、stderr 为空，mock 收到 `GET /api/v1/tags/list?page=1&per_page=20&path=%2F`、无 Authorization；⑦ 提交 `98f1e84`（`gpgsig`、`Signed-off-by` 校验通过），2026-10-03 01:30:28 UTC 以 `--force-with-lease` 更新 PR #603；⑧ PR #603 在 `98f1e84` 上 11/12 检查通过、offline-core 仍在运行时，issues/498 TT-02 的 PR #604 于 2026-10-03 01:47:42 UTC 合入 `main`（`52bf7d2`）并发布 v0.30.23，0.30.23 同样作废、不复用。第二次重建（0.30.24，修订历史 2026-10-03 02:22:41 UTC）：`libra reset --mixed 52bf7d2`；本卡写集中 8 个文件 `cmp` 等于 `98f1e84`（即 `a4d05cb`），`COMPATIBILITY.md` 为 `52bf7d2` 的文件只换第 248 行（763 行），`plan-status.md` 为 `52bf7d2` 的文件只重放本计划的行；网站页不变（`cf@01c4ac7` 已上线，标记 `--list-tags` 计数 9）；① 基线 `52bf7d2` 三处版本面均为 0.30.23，bump 后 `compat_version_surface_sync` 2/2（三处 0.30.24）；② bump 至 0.30.24；③ `Cargo.lock` 只改 libra 版本行（0.30.23 → 0.30.24）；④ fmt 0；clippy 0；按卡内「C 组第 ④ 步」逐条以 nextest 运行（期间网站页恢复为 `cf@01c4ac7` 即本卡已提交的页面，结束后放回 MN-06 的未提交页面）：`--lib --run-ignored all command::mega2` 27/27，`list_tags_` 12/12，`op_rule_list_tags_` 17/17，`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34，`compat_help_examples_banner` 1/1，`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 9s；⑥ 隔离安装得 `libra 0.30.24`，人读 `--list-tags`（stdin 为 `/dev/null`）对 mock 输出 `v1  tag  Ann`、缩进的 message 行与 `page 1 · per_page 20 · total 1`、退出码 0、stderr 为空，mock 收到 `GET /api/v1/tags/list?page=1&per_page=20&path=%2F`、无 Authorization；⑦ 提交 `4116c7f`（`gpgsig`、`Signed-off-by` 校验通过），2026-10-03 02:45:22 UTC 以 `--force-with-lease` 更新 PR #603；网站沿用已推送并上线的 `cf@01c4ac7`；⑧ PR #603 head `4116c7f` 上 12/12 检查通过（2026-10-03 03:53:23 UTC），推送前复核 `main` 仍为 `52bf7d2`、最新 release 仍为 v0.30.23，03:53:31 UTC squash merge 为 `9507623`（树 `2e0ef19` 与 `4116c7f` 相同）；⑨ `gh release create v0.30.24 --target 9507623`（发布于 2026-10-03 03:53:40 UTC，tag 指向 `9507623`） | ① `codeql.yml` run `37094694927`（push `main` `9507623`）success（`security-codeql-rust`、`security-codeql-actions` 均 success）；② `release.yml` run `37094701957`（tag `v0.30.24`）success，8/8 job（四个平台的 build-and-upload、`update-homebrew-tap`、`request-stable-manifest`、`upload-install-scripts`、`verify-homebrew-formula`）；③ `https://download.libra.tools/libra/releases/v0.30.24/libra-linux-amd64` HTTP 200（2026-10-03 04:18:34 UTC）；④ 网站：上线页 HTTP 200、标记 `--list-tags` 计数 11（2026-10-03 04:18:34 UTC，此时上线页已含 MN-06 的 `cf@57b7d12`；01:51:04 UTC 预核时 `cf@01c4ac7` 计数 9）。回滚方式 `immutable-release`：未触发 | Codex R1（2026-10-02 11:32:01 UTC 发起）`FAIL`：P1 文档把 mega2 的 tag 分页写成「只有 lightweight 补页」，而 annotated tag 也有 ref、只排除本页已有的 annotated tag，因而会重复其它页的 tag 并虚增 `total` → EN/zh/网站改写分页说明；R2（11:40:32 UTC 发起）`PASS`，无发现；其后据 MN-09 R1 的 P2，把 tag 错误表中「PATH 不为 `/`」拆为「PATH 格式不合规 → `LBR-CLI-003`」与「`/` 以外的合规 PATH → `LBR-CLI-002`」，R3（13:00:04 UTC 发起，只读终版副本）`PASS`，无发现 | 2026-10-03 04:18:52 UTC |
| MN-06 | 2026-10-02 17:01 UTC 开工（MN-05 已提交 `a4d05cb`、PR #603 CI 中）：分支 `mn-06-create-tag` 叠在其上；本卡改动已于 MN-04 CI 等待期间（2026-10-02 12:12–12:41 UTC）在 MN-05 叠加层之上实现、跑过 A 组并经 Codex 评审（R1 `FAIL` 后修订，R2、R3 `PASS`），终版另存；开工时先核对 MN-05 提交中本卡写集的 9 个文件与网站页逐字节等于该叠加所基于的 MN-05 终版（`cmp` 全部一致），再拷入本卡终版并 `touch`；GC-MN-08：`../mega2` HEAD 已前移到 `fb5ac71`（v0.41.35），`libra diff --name-only 8ff880c HEAD` 在契约文件中仍只列出 `config/config.toml`，tags POST 路由（`tag_router.rs:155`）、root tag 写的鉴权 path（契约页「哪些 tag 写需要能覆盖 `/` 的 token」）与「tag 已存在 400」与卡及本卡文档一致，pin 保持 `8ff880c`；`DEP-MN-02`：网站 `cf@01c4ac7`（含 MN-05）与远端同步；ER-02：锚点行号漂移（内容一致）：`create_tag` 现为 `mega2_tag.rs:416-419`（卡写 `:406-418`），`create_body` 在 `:212-222`，`validate_tag_name` 在 `:84-121`，TUI message 上限与编辑器在 `tag_panel.rs:16,148-172`，`perform_create_tag` 在 `mega2_browser/mod.rs:714-728`。2026-10-03 01:30 UTC 因 MN-05 重建而重新开工（修订历史 2026-10-03 01:09:45 UTC；MN-05 新提交 `98f1e84`、PR #603 CI 中）：`libra switch -C mn-06-create-tag` 指向 `98f1e84`；本卡写集中 8 个文件在 `98f1e84` 上 `cmp` 等于 `a4d05cb`（终版所基于的 MN-05），拷入本卡终版并 `touch`；`COMPATIBILITY.md` 取 `98f1e84` 的文件只换第 248 行（与 `98f1e84` 只差这一行，763 行）；计划记录从重建前的工作区副本恢复（本卡 `Lifecycle`、AC、Verification 与本行）；GC-MN-08 再核：`../mega2` HEAD 前移到 `14bb2ee`（v0.41.39），`libra diff --name-only 8ff880c HEAD` 的 Rust 改动只在 `src/config/`、`src/orbit/`、`src/orbit_api/` 与测试，`src/api/`、`src/ceres/` 无改动，tags POST 仍在 `tag_router.rs:155`，pin 保持 `8ff880c`；`DEP-MN-02`：网站 `cf@01c4ac7` 与远端同步，工作区为本卡未提交的页面（与终版 `cmp` 一致）。2026-10-03 02:45 UTC 因 MN-05 第二次重建再次重新开工（修订历史 2026-10-03 02:22:41 UTC；MN-05 新提交 `4116c7f`、PR #603 CI 中）：`libra switch -C mn-06-create-tag` 指向 `4116c7f`；本卡写集中 8 个文件在 `4116c7f` 上仍 `cmp` 等于 `a4d05cb`，拷入终版并 `touch`；`COMPATIBILITY.md` 取 `4116c7f` 的文件只换第 248 行；计划记录从第二次重建前的工作区副本恢复；GC-MN-08 再核：`../mega2` HEAD 前移到 `83e62e2`（v0.41.40），`src/api/`、`src/ceres/` 自 `8ff880c` 仍无改动，tags POST 仍在 `tag_router.rs:155`，pin 保持 `8ff880c` | 叠加层 A 组：lib 29/29、`create_tag_` 14/14（R1 修订后 16/16）、`op_rule_create_tag_` 15/15、`mega2_` 251/251（修订后 253/253）、四个兼容 target 全绿；拷入终版后在真实基线上复跑：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 29/29（含新 `create_tag_payload_separates_target_and_receipt`、`tag_message_rules` 与网站示例检查）；`…::create_tag_` 16/16（fixture 门 G16–G25、两个请求记录用例、两种 kind 的 JSON payload、人读输出、400/405/422 映射）；`…::op_rule_create_tag_` 15/15（规则门 G1–G15）；整个 `mega2_browser_noninteractive_test` 170/170；`-- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34（未改）；`mega2_` 253/253；`compat_help_examples_banner` 1/1、`compat_command_docs_examples_section` 1/1、`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43；三处兼容行原位改写、行数不变（763/279/149）；网站 typecheck 0、build 0、本地预览 HTTP 200、标记 `--create-tag` 计数 7；重建基线（`98f1e84` + 本卡终版）上复跑：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 29/29；`…::create_tag_` 16/16；`…::op_rule_create_tag_` 15/15；整个 `mega2_browser_noninteractive_test` 170/170；`-- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34；`mega2_` 253/253；`compat_help_examples_banner` 1/1、`compat_command_docs_examples_section` 1/1、`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43；第二次重建基线（`4116c7f` + 本卡终版）上复跑：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 29/29；`…::create_tag_` 16/16；`…::op_rule_create_tag_` 15/15；整个 `mega2_browser_noninteractive_test` 170/170；`-- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34；`mega2_` 253/253；`compat_help_examples_banner` 1/1、`compat_command_docs_examples_section` 1/1、`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43；B 组：无 | MN-05 的 `gh release create` 触发后开始（2026-10-03 03:53:40 UTC 发布 v0.30.24）：`libra reset --mixed 9507623`（树 `2e0ef19` 与 `4116c7f` 相同，本卡改动保留在工作区）；① 基线 `9507623` 三处版本面均为 0.30.24（`libra show` 核对），bump 后 `compat_version_surface_sync` 2/2（三处 0.30.25）；② bump 至 0.30.25；③ `Cargo.lock` 只改 libra 版本行（0.30.24 → 0.30.25）；④ fmt 0；clippy 0；Full-suite trigger 为 `none`，按卡内「C 组第 ④ 步」逐条以 nextest 运行：`--lib --run-ignored all command::mega2` 29/29（含网站示例检查，网站页为本卡页面），`create_tag_` 16/16，`op_rule_create_tag_` 15/15，`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34，`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 9s；⑥ 隔离安装得 `libra 0.30.25`，人读 `--create-tag v1 --message "release one"`（stdin 为 `/dev/null`）对 mock 输出 `created annotated tag v1 -> o-1`、退出码 0、stderr 为空，mock 收到 `POST /api/v1/tags`、请求体 `{"name":"v1","path_context":"/","message":"release one"}`、无 Authorization（未配置凭据）；网站：`cf` 由 `01c4ac7` fast-forward 到 `e908d01`（issues/498 的两个 tag 页提交，未触及 mega2 页），本卡页面在其上 typecheck 0、build 0、本地预览 HTTP 200、标记 `--create-tag` 计数 7；⑦ 提交 `3062e37`（`gpgsig`、`Signed-off-by` 校验通过）；网站 `cf` 签名提交 `57b7d12`（推送前远端 `e908d01`，推送后 `57b7d12`）；⑧ PR #605，head `3062e37` 上 12/12 检查通过（2026-10-03 05:23:37 UTC），合并前复核 `main` 仍为 `9507623`、最新 release 仍为 v0.30.24，05:23:43 UTC squash merge 为 `cdd32b7`（树 `5cafec4` 与 `3062e37` 相同）；⑨ `gh release create v0.30.25 --target cdd32b7`（发布于 2026-10-03 05:23:51 UTC） | ① `codeql.yml` run `37099680422`（push `main` `cdd32b7`）success（`security-codeql-rust`、`security-codeql-actions` 均 success）；② `release.yml` run `37099685600`（tag `v0.30.25`）success，8/8 job；③ `https://download.libra.tools/libra/releases/v0.30.25/libra-linux-amd64` HTTP 200（2026-10-03 05:47:22 UTC）；④ 网站：上线页 HTTP 200、标记 `--create-tag` 计数 7（`cf@57b7d12`，2026-10-03 05:47:22 UTC）。回滚方式 `immutable-release`：未触发 | Codex R1（2026-10-02 12:23:06 UTC 发起）`FAIL`：P1 错误表把 405/422 归入「同 `--create-dir`」，而 tag 客户端把 400/405/422 映射为 `LBR-CLI-002` → EN/zh/网站改写错误表并增 `create_tag_rejections_map_to_cli_002`；P1 缺 lightweight 的 JSON payload 用例 → 增 `create_tag_json_payload_lightweight`；R2（2026-10-02 12:31:27 UTC 发起）`PASS`，P2 一项（lightweight 回执 fixture 的 tagger 应为 mega2 默认的 `unknown`）已采纳；其后据 MN-09 R1 的 P2 把 tag 错误表的 PATH 行拆为 `LBR-CLI-003`/`LBR-CLI-002` 两行，并在「写请求不重试」中写明 tag 写操作的核对方式，R3（2026-10-02 13:00:04 UTC 发起，只读终版副本）`PASS`，无发现 | 2026-10-03 05:49:40 UTC |
| MN-09 | 2026-10-03 04:13 UTC 开工（MN-06 已提交 `3062e37`、PR #605 CI 中）：分支 `mn-09-delete-tag` 叠在其上；本卡改动已于 2026-10-02 11:52–13:05 UTC 在 MN-06 叠加层之上实现、跑过 A 组并经 Codex 评审（R1 `PASS` 附 2 项 P2，采纳后 R2 `PASS`），终版另存；开工时由脚本核对 `3062e37` 中本卡写集的 8 个文件与 `COMPATIBILITY.md:248`、网站 `cf@57b7d12` 的页面逐字节等于 MN-06 终版（`cmp` 全部一致），再拷入本卡终版并 `touch`；GC-MN-08：`../mega2` HEAD 为 `83e62e2`（v0.41.40），`src/api/`、`src/ceres/` 自 pin `8ff880c` 无改动，tags DELETE 仍在 `tag_router.rs:324`，pin 保持 `8ff880c`；`DEP-MN-02`：网站 `cf@57b7d12`（含 MN-06）与远端同步；ER-02：锚点行号漂移（内容一致）：`tag_url` 现为 `mega2_tag.rs:361-373`（卡写 `:360-372`），`delete_tag` 现为 `:451-471`（含 MN-01 的作用域包装，卡写 `:431-446`），`validate_tag_name` 在 `:84-121`（卡写 `:83-120`），404 映射在 `:246`（卡写 `:245-250`），TUI 删除在 `mega2_browser/mod.rs:731-739`（卡写 `:727-736`） | 叠加层 A 组（2026-10-02 12:46–12:49 UTC）：lib 30/30、`delete_tag_` 8/8、`op_rule_delete_tag_` 15/15、`mega2_browser_noninteractive_test` 193/193、`mega2_` 276/276、四个兼容 target 全绿；拷入终版后在真实基线上复跑：`LIBRA_SITE_MEGA2_DOC=… cargo test --lib command::mega2 -- --include-ignored` 30/30（含新单元测试与网站示例检查，网站页为本卡页面）；`…::delete_tag_` 8/8（fixture 门 G16–G19 与 `delete_tag_request_record`、`delete_tag_json_payload`、`delete_tag_human_output`）；`…::op_rule_delete_tag_` 15/15（规则门 G1–G15）；整个 `mega2_browser_noninteractive_test` 193/193；`-- mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34（未改）；`mega2_` 276/276；`compat_help_examples_banner` 1/1、`compat_command_docs_examples_section` 1/1、`compat_matrix_alignment` 9/9、`compat_ledger_schema` 43/43；三处兼容行原位改写、行数不变（763/279/149）；网站 typecheck 0、build 0、本地预览 HTTP 200、标记 `--delete-tag` 计数 5；B 组：无 | MN-06 的 `gh release create` 触发后开始（2026-10-03 05:23:51 UTC 发布 v0.30.25）：`libra reset --mixed cdd32b7`（树 `5cafec4` 与 `3062e37` 相同，本卡改动保留在工作区）；① 基线 `cdd32b7` 三处版本面均为 0.30.25，bump 后 `compat_version_surface_sync` 2/2（三处 0.30.26）；② bump 至 0.30.26；③ `Cargo.lock` 只改 libra 版本行（0.30.25 → 0.30.26）；④ fmt 0；clippy 0；Full-suite trigger 为 `none`，按卡内「C 组第 ④ 步」逐条以 nextest 运行：`--lib --run-ignored all command::mega2` 30/30（含网站示例检查，网站页为本卡页面），`delete_tag_` 8/8，`op_rule_delete_tag_` 15/15，`mega2_browser_cli_test mega2_browser_mkdir_test mega2_browser_mutate_test mega2_browser_tag_test` 34/34，`compat_command_docs_examples_section` 1/1，`compat_matrix_alignment` 9/9，`compat_ledger_schema` 43/43；⑤ `cargo build --release` 2m 9s；⑥ 隔离安装得 `libra 0.30.26`，人读 `--delete-tag v1`（stdin 为 `/dev/null`）对 mock 输出 `deleted tag v1`、退出码 0、stderr 为空，mock 收到 `DELETE /api/v1/tags/v1?path=%2F`、无请求体、无 Authorization（未配置凭据）；⑦ 提交 `dcffa08`（已核对 `gpgsig`、`Signed-off-by`）；网站 `cf@a56bce3` 签名提交与推送；⑧ PR #606 head `dcffa08` 的 12/12 checks success，2026-10-03 06:55:44 UTC squash merge `fc85e6e`（树 `9716682` 与 head 一致）；⑨ 2026-10-03 06:55:51 UTC 发布 `v0.30.26`，tag 指向 `fc85e6e` | `codeql.yml` run `37104701999`（push main `fc85e6e`）success；`release.yml` run `37104652942`（push v0.30.26 `fc85e6e`）8/8 success；下载 HTTP 200；网站 HTTP 200、`--delete-tag` 计数 5（`cf@a56bce3`）；signed stable manifest payload `version=0.30.26`、`channel=stable`。恢复 `immutable-release` 未触发：没有需恢复的已发布缺陷；不执行破坏有效功能的补偿发布 | Codex R1（2026-10-02 12:50:35 UTC 发起）`PASS`，P2×2：① 写操作超时后的核对方式对 tag 写不适用（`--list` 列目录）→ EN/zh/网站写明 tag 写的核对方式；② tag 错误表把 PATH 不为 `/` 一律写成 `LBR-CLI-002` → 拆为「PATH 格式不合规 → `LBR-CLI-003`」与「`/` 以外的合规 PATH → `LBR-CLI-002`」（同批回补 MN-05、MN-06 的终版）；R2（13:00:04 UTC 发起，只读终版副本）`PASS`，无发现 | 2026-10-03 10:57:41 UTC |
| MN-07 | 2026-10-03 05:42 UTC 开工（MN-09 已提交、PR #606 CI 中）：分支 `mn-07-live-gates` 叠在 MN-09 提交之上；本卡改动已于 2026-10-02 实现并经 Codex 评审（R1–R3 `FAIL` 后修订，R4 `PASS`），终版另存；开工时由脚本核对 HEAD 的测试文件逐字节等于 MN-09 终版、集成测试指南自 `fb1cf80` 未变，拷入终版测试文件与指南；`CLAUDE.md` 按当前上游重新生成（只在 `LIBRA_TEST_MEGA_SERVER` 行之后插入 AC-1 的一行，上游 issues/498 对该文件第 270 行的改动保留）；GC-MN-08：`../mega2` HEAD 前移到 `74f383f`（v0.41.41），`src/api/`、`src/ceres/` 自 pin `8ff880c` 无改动，pin 保持；续跑时 `../mega2@75888ab`（v0.41.46）的 `src/api`、`src/ceres` 与契约页对 `8ff880c` 仍无差异；隔离实例原生二进制 `mega2 0.41.34` 的 SHA-256 为 `e79420e56a212a49ed85622dffb0c96628e6bcc0d3678d069f0851c2a66be438`（初始化时源 revision `c9ae6df`，端点契约保持 pin）；`DEP-MN-04` 首次可用性核对（MN-09 尚未 `done`，提前核对）：隔离实例 compose 项目 `libra-mn07`（postgres/redis/rustfs 均 healthy）加原生 `mega2 0.41.34` 二进制（拷贝时 mega2 HEAD `c9ae6df`），配置取 `config/config-storage-only.none.toml`（`push_auth=none`），`http://127.0.0.1:39080`；写根 `/project` 的 `GET /api/v1/tree` HTTP 200；root tag 总数 0（≤ 900） | G13–G19 harness：`cargo test --test command_test mega2_browser_noninteractive_test::live_harness_` 11/11（7 个门 G13–G19，另 4 个评审补充用例：既有 run 条目不被触碰、不确定的 create 会被清理、相对 token 路径解析、写请求带 token 文件）；G1–G12 live：`LIBRA_TEST_MEGA2_SERVER=http://127.0.0.1:39080 LIBRA_TEST_MEGA2_WRITE_ROOT=/project`（无 token 文件），`… live_gate_ -- --test-threads=1` 12/12，耗时 11 s；单独以 `--nocapture` 复跑 `live_gate_create_tag_annotated` 未打印 `skipped`、耗时 6.6 s；跑后写根 `/project` 只剩 `.gitkeep`、root tag 总数 0（无残留）；AC-1、AC-2 逐字行计数命令均输出 `OK: exactly one line`；续跑 A 组：harness 11/11，live 12/12（12.97 s，真实实例、未跳过）；AC-1/AC-2 各恰为一行；测试文件 `cmp` 等于 R4 终版；B 组：无 | `main@bb9fbc5` 上 parity 2/2；bump 至 0.30.27，`cargo build` 通过（1m 34s），`cat-file -p HEAD:Cargo.lock` 对比证明 lock 只改 Libra 包版本；fmt 0，Clippy `--all-targets --all-features -- -D warnings` 0（11m 13s）；C 组 focused nextest harness run `333b5aaf-528c-4f93-87ee-49413efb97bb` 11/11，真实 live run `a922e116-560d-4bf5-a973-3eeb08d24c0a` 12/12（78.807 s，所选 live 门全部执行，无 skipped）；AC-1/AC-2 各恰为一行；`cargo build --release` 通过（2m 38s）；`cargo install --locked --path . --root <隔离临时安装目录> --target-dir target` 通过（15.78s），版本 `libra 0.30.27`；安装后二进制对真实实例 `--machine --list /project` 成功（`data.operation=list`、仅 `.gitkeep`）；⑦ 提交 `4d6b7da`（raw `cat-file --batch` 已核对 `gpgsig`、DCO）；精确暂存本卡 9 路径，提交后工作区 clean；⑧ 推送 `mn-07-live-gates`，`ls-remote` 证明 ref `4d6b7da1d9b8f2237c68c385657a3ea1e7701e30`，PR #608，同一 head 上 12/12 checks success（2026-10-03 12:34 UTC）；⑨ 2026-10-03 12:36:45 UTC squash merge `faae20c`（tree `308c8897` 与 head 完全一致，raw `gpgsig` / DCO 核实），12:38:37 UTC 发布 `v0.30.27`，远端 tag 指向 `faae20c` | `codeql.yml` main run `37123437271` 2/2 jobs success；`release.yml` tag run `37123533785` 8/8 jobs success；Linux amd64 下载 HTTP 200、signed stable manifest `version=0.30.27` / `channel=stable`（2026-10-03 13:03:30 UTC）；网站变更 N/A。`DEP-MN-03` 已交付；恢复发布未演练，限制见下表 | Codex R1–R3 `FAIL`（P1：run id 强度、取消登记、类型化列表、fail-closed 检查、G12 翻页；写后 500 的处理 → `require_fresh_root`、仅 400 时取消登记；相对 token 路径 → `absolute_path`，另增 Bearer 用例），R4 `PASS`；续跑整体实现评审（Codex `gpt-6-sol` / `xhigh`，只读、不跑测试）字面 `VERDICT: PASS`，P0/P1 无，覆盖八个操作、诊断、凭据、EN/zh 文档及 MN-07；明确保留尚未执行的发布、D 组、跨仓交付与最终全量门 | 2026-10-03 11:18:55 UTC |
| 收口全量门 | 12 卡于 2026-10-03 13:05:47 UTC 全部 `done`/`complete`，随后运行（ER-13）；实现基线 `faae20c`，仅有三个计划文档路径的行政同步差异 | `cargo +nightly fmt --all --check` 0；`cargo clippy --all-targets --all-features -- -D warnings` 0（1.25s）；原定 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`，run `280c19a5-cd3f-4afc-831b-bd187c9a8608`：8839/8839 passed、34 slow、5 ignored，1668.684s，0 failed / flaky / leaky（2026-10-03 13:08:07–13:35:57 UTC）；另以原定显式网站路径执行 `--lib --run-ignored only command::mega2::tests::site_example_paths_are_rooted`，run `9b53d661-9b19-4807-8274-8a79cd20fc75`：1/1 PASS（0.153s），其余 3356 为过滤掉的 lib 测试 | 不新增发布或版本；继承 MN-07 的实现发布点，后续仅行政文档提交 | MN-07 D 组 2/2 CodeQL、8/8 release 全绿 | Codex R19 字面 `VERDICT: PASS`；P0/P1 无；唯一 P2（临时状态值未登记）已通过统一为既有 `已收口` 状态解决 | 2026-10-03 13:38:39 UTC |

### 恢复验证边界（收口审计）

| 任务卡 | 验证状态与限制 |
|---|---|
| MN-10 | `forward-only`：URL 解析错误不回显原始输入的不变量已由本卡 unit/真实二进制门与负对照验证。未执行恢复发布：没有发现需前滚的发布缺陷；revert 会恢复凭据回显，违反本卡安全不变量，不能作为恢复演练。 |
| MN-01、MN-02、MN-03、MN-11、MN-04、MN-08、MN-12、MN-05、MN-06、MN-09 | `immutable-release`：未执行生产恢复发布。没有发现需撤销的已发布缺陷；真实恢复需要新增签名 patch、推进 stable，并对网站卡另发 `cf` 补偿提交，会撤销当前已验收的用户能力，因此未为演练执行。既有 tag/artifact 未改写。此项记录实际未验证的范围，不把静态恢复说明或正常路径测试当作已完成的恢复发布。 |
| MN-07 | live 清理路径已由 harness 的 run 边界、写后失败、残留报告门及真实实例运行后目录/tag 观察验证；revert patch 的生产恢复发布未演练。本卡已完成发布/D 组；未发现需撤销的发布缺陷，不为演练撤销已交付的 live 测试与贡献者指南，不把生产恢复发布记为通过。 |

### 全量门准备与历史判据核对

2026-10-03 11:44:37 UTC：在 `4d6b7da` 的实现树上执行 `source .env.test && source .env.live-test && cargo nextest list --all --message-format json`，162 个测试二进制构建成功（8m 19s）。此操作只构建和枚举，不算全量测试执行，最终全量门仍待全部任务卡完成后运行。计划中直接以 `--exact` 指定的 213 个不同测试名，当前树中存在 209 个；另四个为 MN-03 的 `op_rule_create_dir_r5a_code` / `r5a_no_request` / `r5b_code` / `r5b_no_request`，已通过 `cat-file -p 857f885:tests/command/mega2_browser_noninteractive_test.rs` 核实存在于 MN-03 的已发布实现，随后由 MN-11 引入写凭据时按该卡明确删除（原匿名写拒绝规则不再适用于写操作）。MN-11 的规则与凭据 fixture 门及真实写凭据用例取代该历史拒绝门；不以当前缺少这些已退役用例否认 MN-03 当时的执行证据，也不以此豁免当前适用的测试。

### CI 观察连接恢复

2026-10-03 11:58:21 UTC：PR #608 的监控查询因 GitHub 直连 GraphQL/REST 临时 EOF 退出；未把观察失败判为 CI 停止，也未重启 CI。只读诊断显示 API 直连 TLS `SSL_ERROR_SYSCALL`，系统已配置 loopback HTTP/HTTPS 代理；经该现有代理访问 API HTTP 200。后续 `gh` 调用按进程指定系统代理，重新获取同一 head SHA / job 的状态；没有更改系统网络配置。既有代理后来拒绝连接时重新核对直连 API HTTP 200，后续恢复直连查询；观察连接失败未改变 workflow 的运行状态。

### 固定源码实例复验（MN-07 收口）

2026-10-03 12:34 UTC：为把源码 revision 与运行二进制绑定，从 Mega2 commit `c9ae6df3c659b7eae2b009854229b9ace537efa4` 只读导出源码归档（SHA-256 `01fdf7354a0163e840576838fcaea5477662e02fbac375017a69ae63a6aa72b0`），在临时目录安全解包，`Cargo.toml`、`Cargo.lock`、`src/main.rs` 与该 commit 的 blob 逐字节相同。执行 `cargo build --release --locked --bin mega2`（`CARGO_TARGET_DIR` 指向本仓 target） 成功（4m 02s），二进制版本 `mega2 0.41.34`，SHA-256 `66ed976a57100d3d4897b62b778e1db94b204a93cc4ed22e3d21d3af002a63fd`。未修改 Mega2 checkout。新原生服务在 `http://127.0.0.1:39081` 使用既有隔离 compose 项目 `libra-mn07` 的存储服务、相同 storage-only 配置、`push_auth=none`、写根 `/project`；未重启 Docker。

首次 HTTP 探测曾因服务端数据库连接池获取超时关闭连接（Mega2 `mono_api_service.rs:1725`）；其后的全部实际 live 门成功。`LIBRA_TEST_MEGA2_SERVER=http://127.0.0.1:39081 LIBRA_TEST_MEGA2_WRITE_ROOT=/project cargo nextest run --test command_test mega2_browser_noninteractive_test::live_gate_ --test-threads 1 --success-output immediate --failure-output immediate`，run `e258cea9-b99e-439f-a3b9-cb11c0ea0782`：12/12 PASS，118.111s；4282 个过滤掉的其它测试不是 live 跳过，所选 12 个门均输出实际操作步骤，无 `skipped (...)`。安装后二进制 `libra 0.30.27` 在仓外临时 cwd 复核 `--machine --list /project` 成功、只有 `.gitkeep`，`--list-tags --per-page 1` 成功、`total=0`、`has_next=false`；无本次测试残留。此源码绑定的运行替代原先仅靠拷贝时 HEAD 与旧二进制 hash 的 provenance，作为最终真实互通证据。

## 收口 PR #609：Capture 集成后的 CI 修复

### 观测范围

2026-10-04 04:53 UTC 核对：PR #609 远端 head `747992b24bc99fae9fb6e9922128117f64fb71e8` 的完整 `Check, Build and Test` run `37168062003` 有两个失败 job：`compat-offline-core` `111335105329` 与 `opencode-export-linux` `111335105397`。此前 `compat-clippy` 的重试 job `111255764986` 已成功，含 Clippy 与 Rustdoc 链接检查；checkout 下载超时不是待修代码项。

2026-10-04 07:48 UTC 核对：head `870cbbc602c5964d9a31cea771ab3c5c7d71bad1` 的 run `37186577139` 中 `compat-clippy`、`compat-rustfmt` 与独立 Code Quality 已成功。专用 Linux job `111389653654` 下载并验证 bubblewrap 0.13.0 成功，AppArmor userns sysctl 设为 0，随后 `trusted_bwrap_preflight` 失败，bridge E2E 未运行。08:27 UTC 的诊断 head `85776a561420211e9f807ead770f31fe165cd902` 进一步确认失败不是 namespace：严格信任检查拒绝 `/usr/local/bin`，具体错误为该路径组件可被当前 runner 用户修改。该路径来自 Meson 默认 prefix，因此下一修订把已验证的 bubblewrap root 安装到 `/opt/libra-bubblewrap/bin/bwrap`，并通过绝对环境变量路径交给信任检查和 sandbox smoke；保留只测试构建继承 bwrap stderr 与 strict preflight，以供后续诊断。

### 已应用的修复与待验证门

| 失败项 | 已确认的证据 | 修复与验证状态 |
|---|---|---|
| OpenCode Linux sandbox CI 环境（[`opencode-export-linux` job](https://github.com/libra-tools/libra/actions/runs/37188804471/job/111396498650)） | 旧 Ubuntu bubblewrap 0.9.0 缺少 FD flags。0.13.0 安装到默认 `/usr/local/bin` 后，job `111396498650` 的失败日志确认其父目录对 runner 可写，因此 trust guard 正确 fail-closed；namespace smoke 尚未运行。 | 保留版本与哈希 pin、严格能力断言及 bridge E2E；改用 root 安装目录 `/opt/libra-bubblewrap/bin/bwrap` 并通过 `LIBRA_LINUX_SANDBOX_EXE` 固定使用该二进制。只有后续专用 Linux job 通过 trust guard、FD probe 和 bridge E2E 后，才算修复完成。 |
| `validate_trusted_bwrap_accepts_system_binary` | `compat-offline-core` job `111335105329` 中失败；旧断言把系统路径可信与 FD mount 能力混为一谈。 | 已拆开契约：此测试只断言 canonical 系统路径通过信任检查；能力仍由严格预检断言。待新 head 全量测试验证。 |
| `trusted_bwrap_preflight` | 自托管全量 runner 可能没有现代 bubblewrap；head `85776a5` 的 GitHub Linux job 未到 FD probe 即因 `/usr/local/bin` 可写而被严格 trust guard 拒绝。 | 已将此环境门标为 ignored，以免通用 suite 误假设 runner 能力；`opencode-export-linux` 仍以 `--include-ignored` 显式运行同一硬门，缺能力仍失败，不会 skip-green。切换到不可写的 root 安装目录后待远端验证。 |
| `internal::ai::capture::live_oracle_tests::live_checkpoint_metadata_shape_is_stable` | `compat-offline-core` 报 `storage_path() called outside a libra repository`：GitHub checkout 使用 `.git`，而不是 `.libra`。隐式 storage discovery 取决于进程 cwd。 | 已给 oracle 加 `cwd`/`env` serial lanes，并在每个临时 oracle repo 中运行 capture；`ChangeDirGuard` 恢复 cwd。ADR-ACF-10 记录这项测试上下文修订并更新冻结指纹；metadata/redaction golden shape 未改。focused oracle 与冻结架构 guard 本地均 1/1 通过，Linux CI 待新 head 验证。 |
| `internal::ai::history::tests::cleanup_helper_guard_returns_promptly_and_reaps_repeated_timeouts` | `compat-offline-core` 失败：忽略子测试通过 `--exact` 启动，但短过滤串 `cleanup_helper_child_sleeper_process` 匹配不到测试，子进程立即退出，reaper 计数超时。 | 已改用完整 libtest 名 `internal::ai::history::tests::cleanup_helper_child_sleeper_process`；及时返回、PID 消失与 reaper 回收断言保留。Linux CI 待新 head 验证。 |

### 收口条件与边界

原始全量失败证据来自 head `747992b`；第一轮源码修复推送为 `870cbbc`，诊断修订为 `85776a5`，二者的 Linux sandbox 硬门仍失败。当前本地 workflow 已改用 root 安装的 `/opt/libra-bubblewrap/bin/bwrap`，尚未提交推送。最终须继续核对最新 head 的全部必需 CI，尤其 `compat-offline-core` 与 `opencode-export-linux`；所有必需项全绿前 PR #609 保持未合并。本次不新增版本、tag 或 release。
