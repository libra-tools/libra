# `libra format-patch` 开发设计

## 命令实现目标

`libra format-patch` 将提交导出为 mbox 格式的邮件补丁文件。该命令解析 `A..B` 修订范围（空左右侧均默认 `HEAD`，单修订号按 `<spec>..HEAD` 处理），对范围内的每个非合并提交生成一个补丁文件（文件名后缀由 `--suffix` 决定，默认 `.patch`；`--numbered-files` 下改用纯序号并忽略后缀）。除 `--numbered-files` 外，文件名始终带补零序号前缀，不受 `-n`/`-N` 影响。每个文件按 RFC 2822 标准生成 mbox 信封（`From ` 行）、邮件头（`From:`、`Date:`、`Subject:`、`Message-ID` 等）和邮件体（提交正文 + `---` 分隔 + diffstat + unified diff）。输出通过 `-o` 写入目录或通过 `--stdout` 输出到标准输出。

## 对比 Git 与兼容性

- 兼容级别：`partial`。核心补丁导出能力已公开，支持 15+ 参数（含 `--suffix <sfx>`，默认 `.patch`；`--zero-commit`；`--signature`/`--no-signature`；`--signature-file <file>`；`--encode-email-headers`/`--no-encode-email-headers`），merge 提交默认跳过。收件人头 `--to`/`--cc`（可重复，按 git 折叠续行，置于 MIME 头之后，cover letter 同样输出）与 `--no-to`/`--no-cc`（抑制——Libra 无 `format.to`/`format.cc` 配置可重置）已实现。`--from [<ident>]`（改写 `From:` 头为给定身份，无值时用 committer 配置身份；与提交作者不同则将原作者保留为 in-body `From:` 供 `git am` 还原）已实现。`--notes[=<ref>]`（在 `---` 之后、diffstat 之前附加 notes，复用 notes 子系统）已实现。`--attach` / `--inline`（将补丁包成 `multipart/mixed` MIME：log+diffstat 为 `text/plain` part，diff 为 `text/x-patch` part，`Content-Disposition` 分别为 `attachment`/`inline`；boundary 取工具版本，二者互斥）已实现。`--base=<commit>`（记录 `base-commit:` 尾注 + 介于 base 与系列之间各提交的 `prerequisite-patch-id:`，供 `git am --base`）已实现：尾注挂在最后一个补丁上，或在 `--cover-letter` 时挂在 cover letter 上；base 必须是系列的祖先（否则 `base commit should be the ancestor of revision list`，退出 128）；patch-id 复用 git 的 stable 算法（逐行去空白、跳过 `index` 行、忽略 hunk 行号、按文件 SHA 后用逐字节进位加法合并，即 git `flush_one_hunk`，非 XOR），对 text diff 与 `git patch-id --stable` 字节一致（含多文件）；前置补丁跳过 merge 提交；**binary 文件前置补丁不保证字节一致**（见缺口表说明）。`--base=auto`（依赖上游推断 base）暂以用法错误（退出 129）拒绝。未实现的 Git 选项包括 `--interdiff`、`--range-diff`（`--force` 非 Git format-patch 标志）。

### P2-03 邮件输出可靠性闭环

- Revision：`-1 [REV]` 精确导出一个 commit；`--root [REV]` 从指定 tip 遍历至 root。范围解析为零个提交时退出 0，不生成 patch；启用 `--json` 时返回空的 `patches` 列表。仓库没有 commit 或 revision 无法解析时仍报错。`--ignore-if-in-upstream` 用 default stable patch-id 与排除侧可达 commits 比较，全部命中时成功输出空 series。排除侧遍历错误不再被静默吞掉。
- Diff：`log::generate_diff_with_options` 为 format-patch 提供 `--histogram`、真实 `--full-index`、`-U<n>`/`--unified=<n>` 上下文行数及 `--src-prefix`/`--dst-prefix`；`--minimal` 明确复用本就无 deadline、保证 shortest edit script 的 Myers backend。Histogram 通过 `diff::rewrite_unified_diff_histogram` 复用共享 hunk renderer。
- Config：`format.subjectPrefix`、`format.signOff`、`format.outputDirectory`、`format.suffix` 使用严格 local→global→system cascade，显式 CLI 值短路无关坏默认；`--no-signoff` 覆盖 config，`--stdout` 不读取 output-directory 默认。Signoff identity 同样严格读取 cascaded `user.name`/`user.email`，不吞 config 错误。
- Mail/thread：外部 `--in-reply-to` 与本系列生成的 Message-ID 分离；cover letter 先回复外部 parent，各 patch 再回复 cover，避免重复 Message-ID 与双尖括号。MIME attachment boundary/closing delimiter 已由真实 `git am` 消费测试固定。
- 输出：整套邮件先完整渲染，再创建任何文件；单文件通过 temp+rename atomic write，stdout 复用统一 BrokenPipe policy。
- 互操作：`compat_format_patch_mail_roundtrip` 的 8 个 L1 场景覆盖 Libra format-patch→Git am（含 `-N` 文件名 glob）、Git format-patch→Libra am、config/CLI precedence、cover/thread、MIME、root/algorithm/full-index/prefix、upstream patch-id suppression。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::FormatPatch` 公开顶层 CLI，`src/command/mod.rs` 导出 `format_patch` 模块；CLI 层在 `src/cli.rs` 把解析后的参数交给 `command::format_patch::execute_safe`，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/format_patch.rs`。参数类型为 `FormatPatchArgs`；错误类型为 `FormatPatchError`（`thiserror::Error` derive），映射到 `CliError` 的 `NotInRepo`、`CliInvalidTarget`、`IoWriteFailed` 等稳定错误码；主要执行函数为 `execute_safe`、`resolve_range_commits`、`format_patch_body`、`format_cover_letter`、`write_patch_file`。
- 执行路径：`execute_safe` 负责 CLI 安全包装、错误映射和输出配置；核心流程解析修订范围 → 遍历提交图 → 生成 diff → 格式化 mbox → 写入文件或 stdout。

- 流程图：

```mermaid
flowchart TD
    A["入口与分发<br/>Commands::FormatPatch"] --> B["源码分层<br/>src/command/format_patch.rs"]
    B --> C["参数模型<br/>FormatPatchArgs"]
    C --> D["执行路径<br/>execute_safe"]
    D --> E1["resolve_range_commits<br/>BFS 遍历 + 排除集合"]
    D --> E2["format_patch_body<br/>RFC 2822 mbox 格式化"]
    D --> E3["format_cover_letter<br/>封面信模板"]
    E1 --> F["底层对象<br/>Commit / Tree / Blob"]
    E2 --> F
    E2 --> G["复用<br/>log::generate_diff<br/>log::compute_commit_stat"]
    D --> H["输出与错误<br/>CliResult / PatchRecord"]
    H --> I["副作用边界<br/>只写补丁文件/stdout"]
```

- 底层操作对象：`Commit`（通过 `log::get_reachable_commits` 加载，读取 `id`、`tree_id`、`parent_commit_ids`、`author`、`committer`、`message`）；`Tree` / `Blob`（由 `log::generate_diff` 和 `log::compute_commit_stat` 内部加载）。
- 输出与错误契约：`execute_safe` 支持 `--json` 输出结构化 `PatchRecord[]`；空范围在 JSON 中返回空的 `patches` 列表；`--cover-letter` 写出封面（文件名为 `0000-cover-letter` + 后缀，后缀默认 `.patch` 由 `--suffix` 决定；`--numbered-files` 下为纯 `0`）时，该文件以 `number = 0` 的记录出现在 `PatchRecord[]` 中；`--quiet` 抑制文件路径打印；`--no-pager` 跳过分页器；失败通过 `CliError` / `CliResult` 传播，携带稳定错误码：`RepoNotFound`（`LBR-REPO-001`）、`CliInvalidTarget`（`LBR-CLI-003`，revision 无法解析或仓库没有指向 commit 的 `HEAD`；空范围不报错）、`IoWriteFailed`（`LBR-IO-002`，文件写入失败）。新增失败模式要补稳定错误码、用户提示和回归测试。
- 副作用边界：该命令不应修改索引、对象库、refs/HEAD 或 reflog；唯一写入面是补丁文件（后缀默认 `.patch`，可由 `--suffix` 改变；`--numbered-files` 下为纯序号无后缀）或 stdout。

## 实现历史

- 2026-06-20 `3b43f2ca`（`feat(format-patch): add core implementation`）：初始实现，包含 FormatPatchArgs、范围解析、mbox 格式化、CLI 接入。
- 2026-06-20 `cd7696d4`（`docs(format-patch): add user documentation and compatibility entry`）：用户文档和兼容矩阵。
- 2026-06-20 `66e7a339`（`test(format-patch): add integration tests and compat surface guard`）：20 个集成测试和表面守卫。
- 2026-06-20 `945acee8`（`fix(format-patch): fix commit ordering, double-load, and cover letter reroll`）：修复提交排序、重复加载和封面信版本号。
- 2026-06-20 `1c2fd7b1`（`fix(format-patch): handle empty left side in ..B range and --start-number 0`）：修复空 `..` 侧默认值和编号 0 冲突。
- 2026-06-21 `e7e6c07b`（`fix(format-patch): default empty .. sides to HEAD for Git compatibility`）：空左右侧统一默认 `HEAD`。
- 2026-06-21 `574a6588`（`fix(format-patch): always number filenames and use UTF-8 safe slug truncation`）：文件名始终含序号，slug 截断 UTF-8 安全。

## 当前状态

- 公开状态：已公开；模块状态：`src/command/mod.rs` 导出 `format_patch`，`src/cli.rs::Commands::FormatPatch` 负责 CLI 接入。
- 用户文档：`docs/commands/format-patch.md`。
- Synopsis：`libra format-patch [OPTIONS] [revision-range]`。
- 文件名与主题计数：`-n`/`-N` 不改变文件名；`-N` 抑制 commit patch 与 cover-letter Subject 的 `[PATCH n/m]` 计数，`--cover-letter` 默认保留 `[PATCH 0/N]`。计数宽度按显示的最终编号确定，并包含 `--start-number`，例如从 9 开始的两项为 `09/10`，十项系列的 cover letter 为 `00/10`。普通文件名始终带补零序号前缀；`--numbered-files` 仍使用裸序号。依赖旧版 `-N` 无前缀文件名的脚本需自行重命名。
- 公开参数包括：`[revision-range]`、`-1`、`--root`、`-o, --output-directory <DIR>`、`--stdout`、`-n, --numbered`、`-N, --no-numbered`、`--start-number <N>`、`--subject-prefix <PREFIX>`、`--cover-letter`、`--thread` / `--no-thread`、`--in-reply-to <MESSAGE_ID>`、`--to <ADDRESS>`、`--cc <ADDRESS>`、`--no-to`、`--no-cc`、`--from [<IDENT>]`、`-v, --reroll-count <N>`、`-s, --signoff`、`--no-signoff`、`--full-index`、`-U<n>`/`--unified=<n>`、`--diff-filter=<FILTER>`、`--full-diff`（无 pathspec 时 no-op）、`--minimal`、`--histogram`、`--ignore-if-in-upstream`、`--src-prefix`、`--dst-prefix`、`--no-stat`、`--keep-subject`、`--suffix <SFX>`、`--zero-commit`、`--signature <SIGNATURE>`、`--no-signature`、`--signature-file <FILE>`、`--encode-email-headers` / `--no-encode-email-headers`、`--numbered-files`、`--notes [<REF>]`、`--attach`、`--inline`。`--signature-file` 在 `execute_safe` 早期读文件并填入 `signature` 槽（与 `--signature` 互斥，trim 尾换行）；`--encode-email-headers` 经 `encode_email_header` 对含非 ASCII 的 `From` 名称与 `Subject` 做整值 RFC 2047 Q 编码（`=?UTF-8?q?...?=`），纯 ASCII 或未开启时原样输出。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| ✅ 已实现 | `--attach` / `--inline`（MIME 附件/内联模式） | 已公开：`format_patch_body` 在 `--attach`/`--inline` 下输出 `multipart/mixed`——`Content-Type` 头改为 `multipart/mixed; boundary="------------libra <ver>"`，正文为 “This is a multi-part message in MIME format.” + `text/plain`（log+`---`+notes+diffstat）part + `text/x-patch`（diff，`name=`/`filename=` 取补丁文件名）part + 关闭 boundary；`--attach` 用 `Content-Disposition: attachment`，`--inline` 用 `inline`，二者 clap `conflicts_with` 互斥（违反报 `LBR-CLI-002`/退出 129）。与 git `format-patch --attach` 结构字节对齐（boundary 取版本而非 git 版本串）。带集成测试 `attach_wraps_patch_in_mime_multipart`、`inline_uses_inline_content_disposition`、`attach_and_inline_are_mutually_exclusive`、`no_attach_stays_plain_text`。`--no-attach` 为默认行为，未单列。 |
| ✅ 已实现 | `--suffix <sfx>`（文件名后缀，默认 `.patch`） | 已公开：通过 `patch_filename` 的 `suffix` 参数与 cover-letter 列表统一使用；默认 `.patch` 保持原行为。带集成测试（`suffix_changes_patch_filename_extension`）。 |
| ✅ 已实现 | `--signature <sig>` / `--no-signature`（自定义/省略签名） | 已公开：`push_signature` 统一两处页脚（补丁正文 + cover letter）——`--no-signature` 完全省略 `-- ` 页脚；`--signature <s>` 设置文本；默认仍为 libra 版本号。带集成测试（`signature_controls_patch_footer`）。 |
| ✅ 已实现 | `--signature-file <file>`（从文件读取签名） | 已公开：`execute_safe` 早期读文件内容（trim 尾换行）填入 `signature` 槽，与 `--signature` 互斥，`--no-signature` 仍优先；读失败报 `LBR-IO-001`。带集成测试（`signature_file_sets_the_footer`）。 |
| ✅ 已实现 | `--to` / `--cc` / `--no-to` / `--no-cc`（邮件收件人/抄送头） | 已公开：`--to`/`--cc` 可重复，`resolve_recipients` 应用 `--no-to`/`--no-cc`（Libra 无 `format.to`/`format.cc` 配置，故直接抑制对应头），`fold_addresses` 按 git 以 `,`+4 空格续行折叠多地址，`push_recipient_headers` 在 MIME 头之后注入 `To:`/`Cc:`（每个补丁与 cover letter 都注入，与 git 头序一致）。值仅经 `sanitize_header_value`（剥离控制符防头注入），**不做 RFC2047 编码**——git 即便在 `--encode-email-headers` 下也原样输出收件人地址，已字节级差分验证。带集成测试（`recipient_headers_to_and_cc`，含头序、折叠、抑制、cover letter 与非 ASCII 原样用例）。 |
| ✅ 已实现 | `--from [<ident>]`（覆盖 `From:` 头，作者移入 in-body `From:`） | 已公开：`from: Option<Option<String>>`（`num_args=0..=1`+`require_equals`）。`resolve_from_identity` 解析 `--from=<ident>`（`parse_from_ident` 拆 `Name <email>`）或 `--from` 无值（`resolve_signoff_identity` 取 committer 配置身份）。`format_patch_body` 用该身份生成 `From:` 头（按 `--encode-email-headers` 编码），若 name/email 与提交作者不同，则在头/正文分隔空行后注入 in-body `From: <原作者>`（仅 sanitize，不编码，供 `git am` 还原）。cover letter 的 `From:` 同样用该身份。已与 git 字节级差分验证（differ→in-body、no-arg→committer、same→无 in-body）。带集成测试（`from_header_rewrites_author`）。 |
| ✅ 已实现 | `--base <commit>`（记录基础提交，供 `git am --base` 使用） | 已公开：`build_base_block` 解析 base（拒绝 `--base=auto`，退出 129），校验 base 是系列祖先（`reach(series_parent)` 含 base，否则 `base commit should be the ancestor of revision list` 退出 128），把 `reach(series_parent) \ reach(base)` 按 oldest-first 作为前置补丁，逐个经 `git_patch_id_for_commit`（git stable patch-id：`log::generate_diff` 输出逐行 `strip_whitespace`、跳过 `index` 行、`scan_hunk_header` 仅取行数忽略行号、按文件 `patch_id_digest`(SHA1/SHA256 依 `commit.id.kind()`) 后用逐字节进位加法合并（git `flush_one_hunk`，非 XOR）；与 `git patch-id --stable` 及 `prerequisite-patch-id` 字节一致，含多文件提交）算出 `prerequisite-patch-id`。前置补丁集合按 git 语义跳过 merge 提交（`parent_commit_ids.len() <= 1`）。**已知限制**：text diff 与 `git patch-id --stable` 字节一致；**binary 文件前置补丁**不保证字节一致——Libra 的 diff 渲染器输出裸 `Binary files differ`（git 为 `Binary files a/X and b/X differ`），且 git format-patch 计算 binary 前置 id 走内部路径、与其自身 `git patch-id` 输出都不同（实测 `git diff-tree -p|git patch-id` 与 `format-patch --base` 前置 id 对同一 binary 提交不一致）。邮件补丁系列中的 binary blob 罕见，text 用例（真实场景）已精确。尾注块（空行 + `base-commit:` + 各 `prerequisite-patch-id:`）在签名 `-- ` 之前注入：无 cover letter 时挂最后一个补丁，有 `--cover-letter` 时挂 cover letter。带集成测试 `base_records_base_commit_and_prerequisite_patch_ids`、`base_direct_parent_has_no_prerequisites`、`base_on_non_ancestor_fails`、`base_auto_is_rejected`、`base_with_cover_letter_lands_on_cover`。`--base=auto`（依赖上游推断）暂以用法错误退出 129 拒绝。 |
| Git flag | `--interdiff <prev>` / `--range-diff <prev>`（补丁间差异/范围差异） | 未公开；依赖 interdiff/range-diff 引擎。命令层。 |
| ✅ 已实现 | `--notes[=<ref>]`（在补丁中附加 notes） | 已公开：`notes: Option<Option<String>>`（`num_args=0..=1`+`require_equals`）。`render_notes_block` 在 `---` 分隔符之后、diffstat 之前，按 git 字节级格式注入：空行 + `Notes:`（默认 ref `refs/notes/commits`）或 `Notes (<short>):`（其他 ref，剥离 `refs/notes/` 前缀）头，note 每行缩进 4 空格（空行亦缩进，与 git 一致，先 trim 尾换行），再补一空行。复用 `internal::notes::{normalize_notes_ref, show}`；无 note（`NotFound`）则整块省略，与 git 一致。带集成测试（`notes_appends_block_after_separator`、`notes_custom_ref_uses_parenthesized_header`）。`--no-notes` 为默认行为，未单列。 |
| ✅ 已实现 | `--encode-email-headers` / `--no-encode-email-headers`（QP 编码非 ASCII 邮件头） | 已公开：`encode_email_header` 对含非 ASCII 的 `From` 名称与 `Subject` 做 RFC 2047 Q 编码（拆分为 ≤75 字符的多个 encoded-word，不跨多字节字符），纯 ASCII 或未开启时原样。默认关闭（Libra 无 `format.encodeEmailHeaders` 配置；git 由该配置决定，未设置时亦关闭）。带集成测试（`encode_email_headers_q_encodes_nonascii_subject`）。 |
| 非 Git flag | `--force`（强制覆盖已有文件） | `--force` 并非 Git format-patch 标志（git 默认即静默覆盖），故不实现；保留此行仅作澄清。 |
| ✅ 已实现 | `--zero-commit`（每个补丁的 `From <hash>` 信封行输出全零 hash） | 已公开：`format_patch_body` 按 `commit.id` 的十六进制长度生成全零串，仅替换信封行（其余补丁内容不变），与 `git format-patch` 一致。带集成测试（`zero_commit_zeroes_the_envelope_hash`）。 |
| ✅ 已实现 | `--numbered-files`（纯序号文件名，不含 slug/后缀） | 已公开：`patch_filename` 在该模式下直接返回 `patch_num.to_string()`（cover letter 为 `0`），忽略 `--suffix`，与 `git format-patch` 一致。带集成测试（`numbered_files_uses_bare_sequence_numbers`）。 |
| 行为差异 | 合并提交默认跳过，无可选 `--diff-merges` 参数 | 当前实现直接在范围解析时过滤掉所有多父提交。 |
| 兼容矩阵 | `COMPATIBILITY.md` 已登记该命令。 | 已纳入用户可见兼容矩阵和矩阵守卫。 |
| CLI 接入 | `src/cli.rs::Commands::FormatPatch` 已公开。 | 已接入 CLI；后续扩展参数时同步文档、矩阵和测试。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
- 若决定发布该命令，最小闭环是：CLI 变体、`src/command/mod.rs` 导出、dispatch、用户文档、兼容矩阵和测试。

## Blake3 仓库

当 `core.objectformat` 为 `blake3` 时，`--base` 的 prerequisite-patch-id 使用 BLAKE3 摘要（64 位十六进制），而不是 SHA-1 仓所用的 Git stable patch-id 组合器。

### `-k` 的 CLI 冲突检查（FP-03）

在配置填充 `subject_prefix` 前检查显式参数：`-k` 与 `-n`、非空 `--rfc`、显式 `--subject-prefix`（包括空值）互斥，以 `fatal` + `LBR-CLI-002` 退出 129（Git 为 128），且在创建目录、写补丁或 stdout 之前失败。配置 `format.subjectPrefix` 不冲突；`--rfc=` 和 `--rfc --no-rfc` 可与 `-k` 并用。测试 `fp03_*` 分别验证消息、稳定码、退出码、空 stdout、完整文件快照、JSON 与 machine 输出，并核对测试集合。

- 2026-11-23（FP-05 / #529）：补丁正文与 diffstat 改由 `libra diff` 引擎生成（`diff_engine_patch_parts`，经 clap 构造 `DiffArgs` 调 `run_diff_for_service`；根提交保留旧生成器）；默认按 `diff.renames` 检测重命名；`run_diff_for_service` 现在与 CLI 一样应用 `--src-prefix`/`--dst-prefix`。回归：`pure_rename_is_detected_by_default`、`stat_block_matches_libra_diff_stat`、`partial_rename_is_detected_by_default`、`diff_renames_false_disables_detection`、`rename_patch_applies_with_git_am`、`rename_patch_applies_with_libra_am`，以及既有 `format_patch_test` 114/114 与 mail roundtrip 13/13。

- 2026-11-23（FP-06/FP-07 / #529）：新增 `-M[<n>]`/`--find-renames[=<n>]`/`--no-renames`（后者胜出；裸 `-M` 不吞修订参数，`-M<n>` 由 `cli.rs` 预处理为 `--find-renames=<n>`；非法阈值 `fatal: invalid argument to find-renames: '<v>'`，`LBR-CLI-002`/129）与 `--numstat`（提取 `render::format_numstat_output` 供 `diff`/`format-patch` 共用；替换 stat 块并省略 `---`，`-p` 不抑制）。回归：`fp06_*` 14 门、`numstat_*`/`no_stat_with_numstat_keeps_numstat` 5 门。

- 2026-11-25（FPG-22 / #904）：新增 `-U<n>`/`--unified=<n>`：`FormatPatchArgs.unified` 经 `CommitDiffOptions.context_lines` 与 `DiffArgs --unified=<n>` 透传到共享 diff 渲染引擎；默认仍为 3，`CommitDiffOptions::default()` 的 histogram/full_index/prefix 字段不变。回归：`format_patch_test::unified_context_lines`（`-U1`/`--unified=5` 与 Git 黄金 diff 段逐字、两两不同）与 `commit_diff_options_tests::defaults_keep_three_context_lines_and_prefixes`；`format_patch_test` 134/134。

- 2026-11-25（FPG-61 / #904）：新增可见 `--full-diff`：`FormatPatchArgs.full_diff` 仅被接受；由于 Libra `format-patch` 不接受 `-- <path>` pathspec，无 pathspec 时输出与不带该 flag 逐字节相同（Git 同语义）。回归：`format_patch_test::full_diff_accepted_without_pathspec`、`compat_format_patch_flag_surface`（help 必列 `--full-diff`）、`format_patch_test` 135/135、`compat_format_patch_mail_roundtrip`。

- 2026-11-25（FPG-42 / #897）：`format_cover_letter` 仅在系列任一提交的原始对象缓冲（headers+message）含非 ASCII 字节时输出 MIME 头，对齐 Git `make_cover_letter`；全 ASCII 系列不再输出 MIME 头。回归：`format_patch_test::cover_letter_mime_on_demand`（AC1 ASCII、AC2 仅第 2 提交 committer 为 `Cömmitter`）；`format_patch_test` 136/136、`compat_format_patch_mail_roundtrip` 14/14。

- 2026-11-25（FPG-16 / #893）：普通补丁与封面信首行 mbox `From <oid> <date>` 的时间戳统一为 Git 固定 `Mon Sep 17 00:00:00 2001`；补丁使用该提交完整 oid，封面信使用系列末个提交 oid，`--zero-commit` 时按哈希长度补零；补丁 `Date:` 仍取作者时间。回归：`format_patch_test::mbox_envelope_fixed_2001_timestamp`（AC1–AC4：固定信封、zero-commit、封面信、`libra am` 拆分应用）、`compat_format_patch_mail_roundtrip::fixed_envelope_mbox_applies_with_git_am`（AC5）；`format_patch_test` 138/138、`compat_format_patch_mail_roundtrip` 15/15。

- 2026-11-25（FPG-27 / #904）：新增 `--diff-filter=<FILTER>`：父提交路径经 `DiffArgs --diff-filter=<FILTER>` 与 `CommitDiffOptions.diff_filter` 透传到共享 diff 引擎，同时过滤补丁与 diffstat；非法字母由共享引擎以 `LBR-CLI-002` 拒绝且无 stdout；默认 `CommitDiffOptions::default().diff_filter` 为空，log/show/stash 行为不变。回归：`format_patch_test::diff_filter`（AC1–AC3）、`log::tests::commit_diff_options_default_has_no_diff_filter`（AC4）；`format_patch_test` 139/139、`compat_format_patch_mail_roundtrip`/`compat_format_patch_flag_surface` 全绿。

- 2026-11-25（FPG-10 / #889）：`--from=<ident>` 改为严格校验：必须含成对 `<`/`>`（`Name <>` 空邮箱合法），否则 `invalid ident line: <input>` + `LBR-CLI-002`；校验在 revision 解析与空系列判定之前，因此 `--from=foo HEAD..HEAD` 同样失败；裸 `--from` 改用执行时（`GIT_COMMITTER_*` 感知的）提交者身份，而非 `format.from`。回归：`format_patch_test::from_invalid_ident_rejected`（AC1–AC6）；`format_patch_test` 140/140、`compat_format_patch_mail_roundtrip` 15/15。

- 2026-11-25（FPG-31 / #915）：`resolve_format_config` 读取 `format.from`：`true` → 裸 `--from`（执行时提交者身份），`false`/空 → 不改写，其它值 → 作为 ident 行；配置值仅在没有命令行 `--from` 时生效，非法值在配置解析后按 `FPG-10` 的 ident 校验以 `invalid ident line` + `LBR-CLI-002` 拒绝。回归：`format_patch_test::format_from_and_no_from`（AC1–AC4，true/false 与 Git 黄金 `From:` 逐字、ident 原样、非法值稳定码）；`format_patch_test` 141/141、`compat_format_patch_mail_roundtrip` 15/15。

- 2026-11-25（FPG-46 / #915）：新增 `--no-from`（clap `overrides_with`）与 `--from` 互为最后者胜出；`--no-from` 使 `resolve_format_config` 跳过 `format.from`，且与显式 `--from` 一样使配置的非法 ident 值不再校验（`--no-from`/`--from` 覆盖后成功）。回归：`format_patch_test::from_and_no_from_override_config`（AC1–AC7：`--no-from` 对齐 Git 黄金、CLI 覆盖、双向 last-wins、非法配置被覆盖）；`format_patch_test` 142/142、`compat_format_patch_mail_roundtrip` 15/15。

- 2026-11-25（FPG-11 / #890）：实现 `--relative[=<path>]` 与 `diff.relative`（含 `--no-relative` 反向覆盖）。`<path>` 相对仓库根解析；父提交路径通过共享 diff 引擎的 `--relative` 处理（`run_diff_for_service` 现在也执行 relative 前缀剥离），根提交路径在 `CommitDiffOptions.relative` 中过滤并重写。同时修正两处输出字节：diffstat 与 unified diff 之间补空行、`--stdout` 仅在非最后补丁后追加分隔空行，与 Git 逐字一致。移除 `--relative` 的 `Unsupported` 分支与旧测试断言，flag 面新增 `--relative`/`--no-relative`/`--no-from`。回归：`format_patch_test::diff_relative_filters_paths`（AC1–AC6，AC1/AC4/AC5 与 Git 逐字、AC2/AC3/AC6 与 AC1 逐字节）；`format_patch_test` 143/143、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过。

- 2026-11-25（FPG-48 / #890 边界）：`--relative=<prefix>` 的前缀在仓库内不存在时输出空补丁正文；前缀逃逸仓库（`../outside` 或绝对路径）同样按 Git 输出空正文，且不调用 diff 引擎、不写任何文件、不在 stdout/stderr 暴露仓库外绝对路径（内部 `RELATIVE_OUTSIDE` 哨兵）。`---` 分隔符改为仅在确有 notes/diffstat/diff（且非 `--numstat`）时输出。回归：`format_patch_test::relative_outside_and_missing_prefix`（AC1/AC2 与 Git 空正文一致、AC3 仓库外哨兵文件与 mtime 不变、AC4 两个输出流无仓库外绝对路径）；`format_patch_test` 144/144、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过。

- 2026-11-25（FPG-12 / #900、#909）：读取 `format.coverLetter`（`true`/`false`/`auto`，`auto` 在 `commits.len() > 1` 时生成封面信）；新增 `--no-cover-letter` 与 `--cover-letter` 双向 last-wins，命令行覆盖配置；`--commit-list-format` 隐含封面信，但被 `--no-cover-letter` 抑制；`commit-list-format=shortlog` 按 Git 默认识别为作者分组 shortlog（此前被误当作 pretty-format 字面量）。回归：`format_patch_test::format_cover_letter_auto_config`（AC1–AC8，含 modern `[1/1]`、shortlog 列表段与 Git 黄金逐字）；`format_patch_test` 145/145、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过。

- 2026-11-25（FPG-58 / #905）：封面信正文模板对齐 Git `prepare_cover_text`——`*** SUBJECT HERE ***` 仅保留在 `Subject:` 头（`Subject: [PATCH 0/N] *** SUBJECT HERE ***`），正文直接以 `*** BLURB HERE ***` 开头，不再重复标题占位符。回归：`format_patch_test::cover_letter_body_template_matches_git`（AC1 正文以 BLURB 开头且无标题占位符、AC2 Subject 行逐字）；`format_patch_test` 146/146、`compat_format_patch_mail_roundtrip` 15/15、`compat_format_patch_flag_surface` 通过。
