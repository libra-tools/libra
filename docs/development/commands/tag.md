# `libra tag` 开发设计

## 命令实现目标

`libra tag` 的目标是创建、列出、过滤、删除、签名和验证标签。实现需要支持 force、`-n` 展示、annotated tag message（经 `-a`/`-m`/`-F`）、`--points-at` 过滤、轻量标签路径，以及 vault-PGP 的 `-s`/`--sign`（`--no-sign` 撤销）与 `-v`/`--verify`；`-a`/`--annotate` 与 `-e`/`--edit` 编辑器消息录入已支持，尚未公开的是 Git GPG 互操作。

## 对比 Git 与兼容性

- 兼容级别：`partial`。轻量标签、message-based annotated tags（经 `-a`/`-m`/`-F`）、`-a`/`--annotate`（与 `-m`/`-F`/`-e` 组合时创建附注 tag；单独使用时打开编辑器，清理后为空则中止且不写 ref；与 `-d`/`-l`/`-v` 组合为用法错误 129）、`-F`/`--file`（从文件或 stdin 读取 annotated 消息）、force/delete/list/`-n`、`--points-at <object>`、`--contains <commit>`/`--no-contains <commit>`、列表模式下的 `<pattern>` glob 过滤（`tag -l 'v1.*'`）、`-s`/`--sign`（vault-PGP 签名）、`--no-sign`（撤销先前的 `-s`/`--sign`，命令行最后出现者生效；标签默认不签名，故单独使用时为 no-op）、`-v`/`--verify`（vault-PGP 验签）、`--merged <commit>`/`--no-merged <commit>`、`--sort=<key>`、`--column[=<options>]`（逗号/空格分隔，混合 `always`/`auto`/`never` + `column`/`row`（填充顺序）+ `dense`/`nodense`（列宽），默认 column-major+nodense，宽度取 `COLUMNS` 或 80，与 `-n` 互斥；列数与布局与 `git tag --column` 字节一致）、`-e`/`--edit`（打开编辑器撰写/编辑附注消息，注释行剥离，结果为空则中止）已支持；Git GPG 互操作尚未公开。

- 当前矩阵承诺常用 Git 行为已支持；新增语义必须同步矩阵、用户文档和测试。


## 设计方案

- 入口与分发：已公开接入 `src/cli.rs::Commands`；已由 `src/command/mod.rs` 导出。CLI 层在 `src/cli.rs` 把解析后的参数交给命令模块，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/tag.rs`。参数/子命令类型包括：`TagArgs`；输出、错误或状态类型包括：`TagOutput`、`TagListEntry`、`TagError`（crate-private 错误枚举）；主要执行函数包括：`execute`、`execute_safe`。
- 执行路径：`execute_safe` 负责 CLI 安全包装、错误映射和输出配置；创建路径经 `tag::create` 解析 HEAD 提交并写入轻量或附注标签对象；引用路径会读取或更新 SQLite refs（创建/删除标签 ref，不写 reflog，不解析 remote/网络）；数据库路径会通过 SeaORM/SQLite 持久化标签引用。

### 只读查询与 operation 分类（ADR-BRL-01 / #574 BRL-02）

- `tag_is_read_only_query` 与 `run_tag` 共用 `tag_is_list_mode`：`verify` 或列表模式为 `MutationClass::ReadOnly`。
- 列表模式包含 `no_column`。旧条件只测试 `column.is_some()`，因此 `tag --no-column <pattern>` 会创建标签而不是列出。
- `name.is_none()` 只让裸 `tag` 进入列表。`validate_message_source_create_only` 不把缺名当成非创建，所以裸 `tag -m` 仍是缺名用法错误；`--no-column` 经 `tag_requests_list` 算列表旗标，与 `-m` 组合仍是用法错误（`LBR-CLI-002`）。
- `cli.rs` 只调用 `tag_is_read_only_query`；`command_scope` 仍为 `Repository`。

### 列表过滤器的标签链 peel（issues/498 TT-05）

- `collect_tags` 只在 `--points-at`、`--contains`、`--no-contains`、`--merged`、`--no-merged` 之一生效时为每个标签求 peel 终点；不带这些过滤器的列表（含 `-n`、pattern、`--sort`、`--column`）不做深 peel，输出与读取次数不变。
- `tag_filter_commit`：ref 直接指向 commit 时取该 commit；直接指向 tree / blob 时返回 `None`（不参与过滤）；指向 tag 对象时调用 `util::peel_to_non_tag_typed`。该包装委托 `src/utils/util.rs` 的私有 peel 循环（`peel_object_to_type_typed(id, None, ..)`，含环检测），再读取终点类型；tag 模块不再保留自有 peel 函数（旧的一层 peel `tag_peeled_commit` 已删除）。
- 终点为 commit 的标签以该 commit 参与全部过滤（嵌套标签 `outer → ann → commit` 因而能被 `--points-at` / `--contains` 命中）；终点为 tree / blob 的标签从这五个过滤器的结果中排除（`--contains` / `--no-contains` / `--merged` / `--no-merged` 与 Git ref-filter 一致；此前 `--contains` 会把 tag 对象或 tree 当 commit 遍历而整体失败，`--no-merged` 会误列 tree / blob 标签）。`--points-at` 的参数侧仍经 `get_target_commit` peel 到 commit 再与各标签的终点 commit 比较；Git 以未 peel 的参数与链上每个对象比较，因此 tag / tree / blob 参数的结果不同——该参数侧差异由 #533（`issues/477b.md` HW-05）承接。链断裂时 Libra fail closed，而 Git 对四个可达性过滤器静默跳过该标签（`--points-at` 则报 `malformed object`），属有意差异。
- 链断裂（中间对象缺失）或环 fail closed：`TagError::TagChainBroken` → `fatal: tag '<name>' cannot be peeled to a commit: its tag chain is broken`（`LBR-REPO-002`，exit 128，hint `run 'libra fsck' to inspect missing objects.`）；读失败：`TagError::TagChainRead` → `fatal: failed to read the tag chain of '<name>'`（`LBR-IO-001`，exit 128，hint `check that the repository is readable and retry.`）。stdout 不输出部分列表。文案为固定文本，不拼接底层存储错误（可能含本地对象路径）；底层细节只经 `tracing::warn!` 记入日志。
- 测试 failpoint：同时设置 `LIBRA_TEST=1` 与 `LIBRA_TEST_TAG_FAIL_CHAIN_READ=<tag 名>` 时，只有名字等于该值、且 ref 指向 tag 对象的那个标签在调用包装之前得到 `CommitBaseError::ReadFailure`，其余标签照常处理（报错的标签与迭代顺序无关）。`tag_chain_read_failpoint` 只编译进 debug 构建（`#[cfg(debug_assertions)]`；release 构建为恒返回 `false` 的同名实现），release 二进制没有该路径。
- 回归：`tests/command/tag_test.rs` 的 `tag_filter_*` / `tag_list_without_filters_*`（F5 夹具：`old`、`lw`、`ann`、嵌套 `outer`、终点为 tree / blob 的 `atree` / `ablob` / `ttree` / `tblob`，F5′ 为断链变体）与 `src/utils/util.rs` 的 `peel_to_non_tag_typed_*`。

- 流程图：以下流程图按当前源码分层展示主路径和底层对象边界，便于维护者把代码入口、执行函数和副作用范围对应起来。

```mermaid
flowchart TD
    A["入口与分发<br/>src/cli.rs::Commands"] --> B["源码分层<br/>src/command/tag.rs"]
    B --> C["参数模型<br/>TagArgs"]
    C --> D["执行路径<br/>execute / execute_safe"]
    D --> E["底层对象<br/>Blob / Commit / Tree / Branch"]
    D --> F["输出与错误<br/>TagOutput / TagListEntry / TagError"]
    E --> G["副作用边界<br/>写入分支需先预检"]
```

- 底层操作对象：`Blob`（标签指向的 blob 对象，列表展示时读取其 ID）；`Commit`（标签指向的提交对象及提交消息载荷）；`Tree`（标签指向的目录树对象）；`Branch` / branch store（`branch::BranchStoreError`：解析 HEAD 提交时的错误来源）；SeaORM / `.libra/libra.db`（`DbErr`：读写 refs 等 SQLite 表的错误来源）
- 输出与错误契约：人类输出、`--json` / `--machine` 输出和 quiet/verbose 分支必须继续走现有 `OutputConfig` / `emit_json_data` / `CliError` 路径；新增失败模式要补稳定错误码、用户提示和回归测试。
- 副作用边界：tag 仅写入对象库（附注标签对象）和 SQLite refs（标签引用），不触及索引、reflog、D1、工作树或远端；写入前必须先完成参数校验（如删除/创建前的 name 校验），再执行持久化，避免部分写入后静默成功。

## 实现历史

- 本节依据本地 main 分支提交历史重写，筛选与该命令实现、测试或文档路径直接相关的提交；以下是归纳后的实现脉络。
- 2025-10-02 `3879a44a`（`feat: add argument -f/--force for tag command`）：基础实现节点：add argument -f/--force for tag command；当前实现的主要轮廓可追溯到该提交。
- 2026-06-07 `8fecc10d`（`feat(tag): add -a/--annotate flag requiring a message (v0.17.1409)`）：历史资料中曾记录 `-a/--annotate`，中间版本撤回；HF-11（issues/477）按 ADR-HF-11 重新公开该 flag（与 `-m`/`-F`/`-e` 组合创建附注 tag，单独使用走编辑器）。
- 2026-06-06 `58b0cc16`（`feat(tag): add --points-at list filter (v0.17.1406)`）：新增 `--points-at <object>`（字段 `points_at`），列表模式下按 peel-to-commit 过滤标签；不可解析对象映射为 `LBR-CLI-003`（exit 129）。该提交曾在一次 reconcile 中从工作树丢失，已于 2026-06-18 依据原提交 diff 恢复（含单元测试、端到端测试与文档）。
- 2026-05-18 `b534c401`（`fix(commit,stash,index-pack,tag): restore Issues URL on internal-invariant paths`）：实现修正：restore Issues URL on internal-invariant paths；该节点把边界行为、错误处理或兼容差异纳入当前实现约束。
- 2026-05-16 `fff9cbb0`（`test(tag): pin Display for 5 static-message TagError variants (v0.17.292)`）：测试契约：pin Display for 5 static-message TagError variants (v0.17.292)；相关行为已有回归守卫，后续变更需要继续满足。
- 2026-07-11（plan-20260708 P1-05d，sort 片）：`tag.sort` 配置默认接入严格 local→global→system 级联（`configured_tag_sort`，`--sort` 优先）。配置在 list 模式判定之后解析，因而配置的排序不会把 `libra tag <name>`（创建）翻成列表。两者皆未设置时列表按 `refname` 升序（Git 默认；此前为 DB 插入序）。无效配置值 → `TagError::InvalidSortConfig`（`LBR-CLI-002`），读取失败 → `SortConfigRead`（`LBR-IO-001`），均在输出前。`creatordate` 仍为对象哈希近似（与 `--sort` 相同，文档已注明）。已记录收窄：重复配置值只应用胜出 scope 的最后一个（Git 叠成多键排序）。回归：`compat_config_defaults_semantics` 的 `tag_sort_config_orders_list_without_forcing_list_mode`（含多值 last-wins）、`sort_config_read_failure_is_io_error_before_listing`（不可读 global 库 → LBR-IO-001 点名键）。
- 2026-10-02（issues/498 TT-05，v0.30.22）：`--points-at` / `--contains` / `--no-contains` / `--merged` / `--no-merged` 沿整条标签链 peel（新增 `util::peel_to_non_tag_typed`，删除只 peel 一层的 `tag_peeled_commit`），终点为 tree / blob 的标签被排除，断链 / 环 / 读失败 fail closed（`LBR-REPO-002` / `LBR-IO-001`）；新增 debug-only 测试 failpoint `LIBRA_TEST_TAG_FAIL_CHAIN_READ`。
- 历史结论：当前文档应以这些提交之后的代码、测试和兼容矩阵为准；更早的迁移式文档只保留为背景，不再作为事实来源。

## 当前状态

- 公开状态：已公开；模块状态：已导出。
- 用户文档：`docs/commands/tag.md`。
- Synopsis：`libra tag [OPTIONS] [-l | -d | -f] [-a] [-m <MESSAGE> | -F <FILE>] [-e] [-n <N_LINES>] [--points-at <object>] [--contains <commit>] [--no-contains <commit>] [--merged <commit>] [--no-merged <commit>] [--sort <key>] [--column[=<mode>]] [--no-column] [NAME]`。
- 公开参数/子命令包括：`-l, --list`、`-d, --delete`、`-a, --annotate`、`-m, --message <MESSAGE>`、`-F, --file <FILE>`、`-f, --force`、`-n, --n-lines <N_LINES>`、`--points-at <object>`、`--contains <commit>`、`--no-contains <commit>`、`--merged <commit>`、`--no-merged <commit>`、`--sort <key>`、`--column[=<options>]`（逗号/空格分隔：`always`/`auto`/`never` + `column`/`row` + `dense`/`nodense`，缺省 `always`+column-major+nodense，与 `-n` 互斥，未知选项报 `LBR-CLI-002`）、`--no-column`（等价于 `--column=never`，经 clap `overrides_with` 与 `--column` 互为最后一个生效；`column` 字段读出 last-wins 结果，`no_column` 不直接读取；标签默认每行一个，故单独使用为 no-op）、`-s, --sign`、`--no-sign`（经 clap `overrides_with` 与 `--sign` 互为最后一个生效；`sign` 字段读出 last-wins 结果，`no_sign` 不直接读取）、`-v, --verify`、`[NAME]`（创建时为标签名；列表模式下作为 fnmatch glob 过滤模式，如 `tag -l 'v1.*'`，`*`/`?`/`[...]` 经 `compile_tag_glob` 锚定匹配标签名）。
- `-F, --file <FILE>`（与 `-m` 互斥）：从文件读取 annotated 标签消息（`-` 表示从 stdin 读取），由 `resolve_tag_message` 解析，提供后即创建 annotated 标签。读文件失败报 `TagError::MessageFileRead`→`LBR-IO-001`（`IoReadFailed`）。签名（`-s`）当前仍要求 `-m`（因此与 `-F` 不组合）。
- `-a/--annotate 创建附注 tag`：与 `-m`/`-F`/`-e` 组合时走既有附注创建路径；单独使用时等价于 `-e`（打开编辑器，清理后为空则 `EmptyEditedMessage`、不写 ref）。与 `-d`/`-l`/`-v` 等非创建模式组合为用法错误 129（`MessageOptionRequiresCreate`）。不进入 clap `action` 组，以免挡住 `-a -f`。
- `-e, --edit`：打开编辑器撰写或编辑附注标签消息。编辑器缓冲以 `-m`/`-F` 的 base 消息（如有）加注释说明块预填，经 `editor::resolve_editor`（`GIT_EDITOR`→`core.editor`→`VISUAL`→`EDITOR`，无配置且有 TTY 时回退 `vi`，否则报 `TagError::NoEditor`→exit 128）→`editor::edit_message`（落 `TAG_EDITMSG`）打开。保存后用 `clean_tag_message`（`git stripspace` 语义：剥离整行注释、去行尾空白、折叠空行）清理；为空则报 `TagError::EmptyEditedMessage`→exit 128（`failure`+`RepoStateInvalid`，对齐 `commit` 空消息）。`-a` 单独使用走同一编辑器路径。
- `-s, --sign`（clap `requires = "message"`，即要求 `-m`；`-e` 可进一步编辑该 `-m` 消息，但 `-s` 不接受 `-F` 或仅编辑器消息）：用 vault PGP 密钥对规范化的未签名标签内容（`object/type/tag/tagger/\n\n/message`）签名，并把 armored 签名块（`vault::signature_to_armored`）追加到标签消息后，对齐 Git 的 signed-tag 布局；tagger 仅构建一次以保证被签名字节与落库对象一致。无 unseal key 时报 `CreateTagError::VaultSign`→`TagError::VaultSign`。
- `-v, --verify <name>`：`internal::tag::verify` 在签名标记处切分标签消息、重建未签名内容、`vault::armored_to_signature_hex` 还原签名后调用 `vault::pgp_verify`（进程内验证本仓库允许列表公钥：活动 → 生成 → 历史；**吊销与过期按签名自身的创建时刻判定**——`signature_creation_time_secs` 取 hashed `SignatureCreationTime`（缺失回退 now），`key_revoked_at`/`subkey_revoked_at` 取吊销签名时刻，`revocation_applies`/`expiry_applies` 决定是否拒收）。好签名打印 `Good signature for tag '<name>'`（exit 0）；坏签名 `TagError::BadSignature`（exit 1）；未签名/非 annotated/未找到/无密钥经 `map_verify_tag_error` 报错。
- `--contains <commit>` / `--no-contains <commit>`：仅保留（或排除）其 peeled commit 以 `<commit>` 为祖先的标签（即 tag “包含”该 commit），隐含 list 模式；peeled commit 沿整条标签链求得，终点为 tree / blob 的标签不参与（见「列表过滤器的标签链 peel」）；复用 `log::get_reachable_commits` 对每个 tag 的 peeled commit 做一次可达性遍历。


## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| ✅ 已实现 | 签名标签 | 原始对照：git tag -s <name>；当前说明：已实现 `-s/--sign`（vault PGP，armored 签名块追加到 tag message；要求 `-m`）。Libra 的签名为 vault-PKI，非 GPG 互通。 |
| ✅ 已实现 | 验证标签 | 原始对照：git tag -v <name>；当前说明：已实现 `-v/--verify`（`vault::pgp_verify` 经 libvault `pki/keys/verify`，重建未签名内容后验签）。Libra 验签为 vault-PKI，非 GPG 互通。 |
| ✅ 已实现 | 按包含提交过滤 | 原始对照：git tag --contains / --no-contains <commit>；当前说明：已实现（`TagArgs.contains`/`no_contains`，复用 `log::get_reachable_commits` 逐 tag 可达性过滤，隐含 list 模式）。 |
| ✅ 已实现 | 从文件读取消息 | 原始对照：git tag -F <file>（`-` 为 stdin）；当前说明：已实现 `-F`/`--file`（`resolve_tag_message` 读取文件或 stdin，与 `-m` 互斥，提供后即 annotated）。签名 `-s` 当前仍要求 `-m`，故不与 `-F` 组合。带集成测试（`test_tag_dash_f_reads_message_from_file_and_stdin`）。 |
| ✅ 已实现 | 按合并状态过滤 | 原始对照：git tag --merged / --no-merged；当前说明：已实现（`TagArgs.merged`/`no_merged`，复用可达性判定，隐含 list 模式）。 |
| ✅ 已实现 | 排序输出 | 原始对照：git tag --sort=<key>；当前说明：已实现 `--sort`（`refname`/`-refname`/`creatordate`/`-creatordate`，经 `sort_tags`）。 |
| ✅ 已实现 | 多列输出 `--column` | `--column[=<options>]`（缺省 `always`）按 `parse_column_spec` 解析逗号/空格分隔的选项：启用（`always`/`auto`/`never`）、填充顺序（`column` 默认 column-major / `row` row-major）、列宽（`nodense` 默认等宽 / `dense` 每列按自身最长项）。`format_tag_columns` 复刻 git `display_table`/`display_dense`：**dense** 取使总填充宽度严格 `< width` 的最少行数（最多列数）；**nodense** 列数 = `(width-1)/列宽`（git 严格 `<` 适配），column-major 再 `cols=ceil(n/rows)` 收缩空列、row-major 保留；**`plain`**（git 布局 token）强制单列（每行一项）。列宽与列数按**终端显示宽度**计算（`unicode_width::UnicodeWidthStr`，宽 CJK=2、组合字符=0；按显示宽度手工填充，非 Rust 的按字符数填充），与 git `utf8_strwidth` 一致；项长 + 2 padding，宽度取 `COLUMNS` 或 80；末列尾随空白裁剪。`auto` 仅 stdout 为终端时生效；与 `-n` 互斥（clap conflicts_with）；未知选项报 `LBR-CLI-002`。`--no-column`（= `--column=never`）经 `overrides_with` 撤销先前的 `--column`（last-wins）。**已与 `git tag --column` 跨多种 spec×`COLUMNS` 宽度（含 row/dense/nodense/plain + CJK 显示宽度）字节级比对一致**。带集成测试 `tag_column_lays_out_in_column_major_order`（column+row）、`tag_column_dense_row_and_boundaries_match_git`（dense/nodense 列数、严格 `<` 边界 78/79、row-major 不收缩、plain、空格分隔、later-wins）、`tag_column_unknown_option_is_usage_error`。注：libra 默认 `tag -l` 未排序（插入序），与 git 默认 refname 排序不同，是独立于 `--column` 的既有差异（用 `--sort=refname` 对齐）。 |
| ✅ 已实现 | `-a/--annotate` 创建附注 tag | 原始对照：git tag -a [--message/--file/--edit] <name>；当前说明：已实现 `-a`/`--annotate`。与 `-m`/`-F`/`-e` 组合创建附注 tag；单独使用打开编辑器（空消息中止、零写入）；与 `-d`/`-l`/`-v` 组合为用法错误 129。带集成测试 `test_tag_annotate_flag_matrix`（M-TAG G1–G7）。 |
| ✅ 已实现 | 编辑器消息录入 `-e`/`--edit` | 原始对照：git tag -e <name>（配合 `-a`/`-m`/`-F`）；当前说明：已实现 `-e`/`--edit`，经 `compose_tag_message` 用 `editor::resolve_editor`/`edit_message`（落 `TAG_EDITMSG`）打开编辑器，缓冲以 `-m`/`-F` 的 base 消息加注释块预填；保存后 `clean_tag_message`（`git stripspace`）清理，空消息报 `TagError::EmptyEditedMessage`→exit 128。`-a` 单独使用走同一编辑器路径；`-s` 仍要求 `-m`（clap `requires = "message"`）。带集成测试 `tag_edit_composes_seeds_and_aborts_via_editor`。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
