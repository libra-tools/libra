# am 命令开发设计

## 命令实现目标

`libra am <patch...>` 提供 plan-20260708 P2-01 的最小邮件补丁流：解析普通 `format-patch` 邮件、把文本 diff 应用并逐封建 commit，以及可恢复的 `--continue` / `--skip` / `--abort` sequencer。重点是 fail-closed 路径安全和中断/abort 回滚，不扩张到完整 Git mail flags。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：多个独立 patch mail 文件、基本/折叠 headers、`[PATCH ...]` subject cleanup、in-body `From:`、RFC 2822 author date、UTF-8/US-ASCII RFC 2047 B/Q、single-part 7bit/8bit/binary/quoted-printable/base64、文本 new/modify/delete diff、author/message/date 保留、`--continue`/`--skip`/`--abort`、JSON/machine。
- 明确延后：stdin/多消息 mbox、MIME multipart、binary、rename-only/mode-only、3-way marker、applypatch hooks、全部 mail flags；P2-02 已公开共用 parser 的独立 `mailinfo`。
- 边界：总输入 64 MiB、最多 10,000 mail；需要 local branch + existing HEAD；新 series 拒绝 staged/tracked dirty state，并直接以 index + filesystem 拒绝所有 existing non-index target（含 ignored），不依赖 status 的 untracked projection。

## 设计方案

- 入口：`src/cli.rs::Commands::Am` → `command::am::execute_safe`。
- parser：`command::mailinfo::parse_mail` 做 CRLF normalize、header unfolding/transfer decode、identity/date/subject/message split 与 diff extraction；public `mailinfo` 和 `am` 共用这一个 seam。`am` 在 repo context 继续执行 `patch_targets` guard，解析出的 `MailPatch` 连同 targets 进入持久状态。状态 load 会重新解析 patch targets 并与保存值比对，外部篡改不能把 cleanup 路径改到工作树之外。
- patch engine：复用 `command::apply::{patch_targets,prepare_patch}`。所有 file section 在 memory 中 test-apply 后才写；path guard 拒绝 absolute/empty/`.`/`..`/NUL/`.libra`/symlink component，避免非规范拼写绕过 untracked collision；写入使用 `write_atomic` 并恢复已有 permissions。公开 `apply` 仍保持 check-only。
- staging：worktree 写入后以 exact targets 调用 `add::run_add(force=true)`，继续保留 layer exclusion、LFS lock policy、case collision 与 index save 约束。
- commit：从 index 走 `tree_plumbing::write_tree_from_index`，author/date 来自 mail，committer 来自当前 config/env；commit object 先落 object store，再由 `with_reflog` transaction 同时更新 branch、reflog 和下一 `sequence_state`（最后一封则 clear）。因此不存在“branch 已移动但 state 仍指旧邮件”的 crash window；失败只可能留下 unreachable object。
- state：`am` 通过 crate-private kind 共用统一 `sequence_state` row，不向 public `SequenceKind` enum 增加会破坏 exhaustive match 的 variant；`head_orig` 是 abort anchor，`current_oid` 存当前步骤预期 HEAD，payload 存 bounded mails/current ordinal。continue/skip 必须同时匹配 branch name 和 expected HEAD；若 state 已推进但 index/tracked worktree 仍 pristine（initial save 后或 commits 间中断），continue 重试 current mail；abort 只要求原 branch，允许从 same-branch tip drift 恢复。
- rollback：skip reset 当前 HEAD；abort reset `head_orig`。两者在 reset 后清理“当前 mail target 且 restored index 不跟踪”的文件，并向上删除空目录，覆盖 write 后/stage 前中断的新文件残留；新 series 已预拒绝所有 target untracked collision，因此不会删用户原有 untracked data。
- continue guard：拒绝 unresolved stage、当前 target unstaged、无关 tracked worktree change、空 staged resolution，以及 staged path 超出当前 mail targets；仅完全 pristine 的 clean-window recovery 会自动重试 current mail。
- sequencer mutex：crate-private `ensure_none_for_am` 与统一 table 使 am/merge/rebase/revert/cherry-pick 互斥；status 显示 am recovery hint。
- status detection：读取统一/legacy sequencer 失败或遇到未知 kind 时以 `LBR-REPO-003` fail-closed，不把损坏状态静默吞成“无进行中操作”。

## 测试

- `compat_mail_am_basic`：16 个黑盒场景覆盖 generated `format-patch` replay、message/author/date、series、单封 add+delete、SHA-256、conflict→continue、skip、abort、dirty/untracked/ignored/staged guards、非规范 path alias 防护、same-branch HEAD drift、initial-state/commits 间 pristine resume、injected write→stage interruption cleanup、executable preservation、JSON/help。
- `command_test::command::apply_test`：16 个 shared parser regression，包括非规范 alias 与 symlink-component escape rejection。
- `command::mailinfo::tests`：plain mail 的 mailinfo/am 双视图、quoted-printable/RFC 2047（含 folded adjacent words）、subject cleanup。
- `compat_mailinfo_basic`：Unix 8 个（非 Unix 7 个）repo-independent 黑盒场景固定共享 parser、body/patch outputs、JSON/quiet 与 output safety。
- `internal::sequencer::tests`：crate-private `am` unified row round-trip、双向跨操作 mutex，以及 public `SequenceKind` source-compat guard。

## 还未实现的功能

| 类别 | 未完成项 | 处理 |
|---|---|---|
| interop | Git format-patch→Libra am 与 Libra format-patch→Git am 的完整矩阵 | P2-03；P2-01 只固定 Libra 默认 single-part 输出。 |
| conflict | `--3way`、自动 conflict marker、rerere | 延后；当前 exact apply stop + manual stage。 |
| formats | multipart/attachment、多消息 mbox、binary/rename/mode-only | 延后；明确错误，不 silent fallback。 |
| hooks/options | applypatch hooks 与 Git am 完整 flag/config surface | 延后；用户文档明确不会运行。 |

## 维护要求

- 修改 mail parser 时必须同步 public `mailinfo` 与 `am` 的测试，不能产生两套 cleanup 语义。
- branch/ref/sequencer advance 必须继续保持单 DB transaction；任何拆分都按 P1 correctness regression 处理。
- rollback 新增 path class 时必须加入 interruption-before-stage 测试，证明 abort 不留文件且不删 pre-existing untracked data。

## 显式换行比较（#1062）

干净度预检遵循显式 `text`/`eol` 规则，因此等价的 CRLF 表示不会阻止应用邮件补丁。真实未暂存修改和已有序列或冲突状态仍保留原有保护。 `text=auto` 同样使用该比较链路，并保留二进制探测和历史 CRLF 索引保护；`--renormalize`、索引读取额度及剩余配置边界见 [add](add.md)。

调用现有共享状态入口并消费 `worktree_conversion` 的显式策略；保留原始快照/物化 API，未将其它命令的 raw blob 读取批量改成 clean。属性版本变化及读取失败不得复用旧的 clean 结论。回归位于既有 `command_test` 对应模块的 `i1062_` 用例（am 为 `worktree_isolation_test`）。
