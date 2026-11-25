# mailinfo 命令开发设计

## 命令实现目标

`libra mailinfo <msg> <patch> < mail` 实现 plan-20260708 P2-02 的最小邮件
plumbing：对单封 email patch 做 bounded decode，输出 Git-shaped metadata、
body-only message 和从 `---` 开始的 patch，供 shell script 与 `am` 共用。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：stdin 单封 UTF-8 `text/plain`；mbox envelope；基本/折叠 header；
  7bit/8bit/binary/quoted-printable/base64；UTF-8/US-ASCII RFC 2047 B/Q；
  `[PATCH ...]` cleanup；in-body `From:`；body/patch split；human、quiet、
  JSON/machine；无需 repo。
- 明确延后：Git 的 `-k/-b/-m/-u/-n`、encoding/scissors/quoted-CR flags、
  multipart/attachment、UTF-8/US-ASCII/ISO-8859-1 之外的 charset、多消息 mbox、binary/non-`diff --git`
  patch。
- 边界：stdin 64 MiB；两个目标必须是 existing parent 下的 distinct file
  names，拒绝 `-`、directory 及 `out`/`./out`、symlink-parent 等 alias。

## 设计方案

- 入口：`src/cli.rs::Commands::Mailinfo` →
  `command::mailinfo::execute_safe`；preflight 为 `none()`，不初始化 repo storage。
- shared parser：`command::mailinfo::parse_mail` 负责 CRLF normalize、header
  unfolding、transfer/RFC 2047 decode、author/date/subject cleanup 与 message/patch
  split，返回 `ParsedMail`。`am::read_mail_patches` 调用同一 parser，再在 repo
  context 中执行 `apply::patch_targets` path safety validation；`mailinfo` 本身不依赖
  `util::working_dir()`，避免 plumbing 在 repo 外 panic。
- 输出：human stdout 固定四行；JSON 由 `emit_json_data("mailinfo", ...)` 输出
  metadata、路径和 byte counts；quiet 只抑制 stdout，不抑制文件写入。
- 文件安全：先 canonicalize parent 来识别 lexical/symlink-parent alias，拒绝
  missing parent 与 directory target。完整解析后用两个 `StreamingAtomicFile`
  staged payload；两个 temp 都写完后才逐个 atomic persist。单文件不会截断；两个
  独立路径无法提供 cross-file atomic transaction，该限制在用户文档明确。
- 错误：输入读取为 `LBR-IO-001`，输出为 `LBR-IO-002`，malformed/oversized mail
  为 `LBR-CLI-002`，缺参/冲突 positional 由 clap 给出 usage exit 129。

## 实现历史

- P2-02（v0.18.85）：公开 `mailinfo`，把 P2-01 位于 `am.rs` 的 parser 移到共享
  module；修复 repo 外 target extraction panic，并按 RFC 2047 忽略相邻 encoded
  words 之间的 folding whitespace。

## 当前状态

- `compat_mailinfo_basic`：Unix 8 个（非 Unix 7 个）黑盒场景覆盖 repo 外 basic extract、Git-shaped
  metadata、folded/QP/in-body author、JSON/quiet、destination alias、invalid input/
  second destination 不覆盖旧输出、multipart/non-UTF-8 fail-closed 与 help。
- `command::mailinfo::tests`：共享 parser 的 am/mailinfo 双视图、transfer/RFC 2047、
  subject cleanup 与 header-injection guard。
- `compat_mail_am_basic`：继续回归 shared parser 被 `am` 消费后的 16 个 sequencer
  场景。

## 还未实现的功能

| 类别 | 未完成项 | 处理 |
|---|---|---|
| options | Git mailinfo 的 keep-subject、keep-non-patch-brackets、message-id、encoding、scissors 等 flags | P2 后续按真实互操作需求逐项加入。 |
| MIME | multipart、attachment、UTF-8/US-ASCII/ISO-8859-1 之外的 charset | 明确错误，不 silent fallback。 |
| stream | 多消息 mbox 与输出 stdout target | 延后；当前严格一封 stdin、两个 file target。 |
| transaction | 两个路径的一次原子 commit | filesystem 不提供通用 cross-file transaction；保持 staged-before-persist + per-file atomic。 |

## 维护要求

- `am` 与 public `mailinfo` 必须继续共用 `parse_mail`；不得复制 cleanup/decode。
- repo-independent parser 不得调用 `util::working_dir()` 或读取 index/worktree。
- 新增 encoding/format 必须同时补 shared unit、mailinfo compat 与 am regression。
- 输出 path 语义变化必须证明 invalid input/destination 不截断已有文件。

- 2026-11-25（I672-01 / #672）：`mailinfo`/`am` 共享解析层新增 `ISO-8859-1`（latin1）→ UTF-8 无损转换：正文按传输编码取出原始字节后按 charset 转换（`quoted-printable`/`base64`/8bit 均适用），RFC 2047 B/Q encoded-word 同样按 charset 转换；其它 charset（如 `Shift_JIS`）保持 `unsupported text charset` 拒绝。正文解码后含 NUL 时 `am` 以退出码 129 报 `a NUL byte in commit log message not allowed`，Subject 含 NUL 时 `mailinfo`/`am` 均以 129 报 `a NUL byte in 'Subject' is not allowed`。回归：`compat_mailinfo_basic`（latin1 正文/Subject、NUL、UTF-8/us-ascii 基线、Shift_JIS 拒绝）与 `compat_mail_am_basic`（latin1 正文/Subject、NUL、基线、Shift_JIS 拒绝），两 target 59/59。
