# `libra revert` 开发设计

## 命令实现目标

`libra revert` 的目标是生成抵消已有提交的反向变更，并保留冲突处理和提交控制的清晰边界。当前实现支持单父提交的反向变更、merge commit mainline revert（`-m/--mainline`）、no-commit 流程、`--continue`/`--abort` 冲突恢复和稳定错误输出，`--skip` 与多提交冲突的自动续作（剩余提交队列）也已实现。自动创建的 revert commit 使用当前 author/committer 身份（含 `GIT_AUTHOR_*`/`GIT_COMMITTER_*` 日期覆盖），默认消息从被 revert 提交的去签名正文中取 subject，避免从 `gpgsig` 块派生错误标题。仅自定义策略（strategy）列为未完成。

## 对比 Git 与兼容性

- 兼容级别：`partial`。单/多提交 revert、`-n/--no-commit`、`-m/--mainline`（merge commit revert）、`-s/--signoff`（Signed-off-by trailer）、`-e/--edit`（在生成的 revert 消息上打开编辑器，复用 commit 的编辑器级联 `$GIT_EDITOR`/`core.editor`/`$VISUAL`/`$EDITOR`；与 git 不同，Libra revert 默认**不**打开编辑器，需显式 `--edit` 选用，与 `--no-edit` 互斥）、`--no-edit`（接受式 no-op，即默认行为）、`--no-rerere-autoupdate`（接受式 no-op，Libra 无 rerere）与冲突 sequencer（`--continue`/`--abort`/`--skip`，3-way 冲突标记 + `revert-state.json`；`--edit` 经 `RevertState.edit` 串到 `--continue`/`--skip`；冲突时把剩余提交队列存入 `RevertState.remaining`，`--continue`/`--skip` 自动续作其余提交）已支持；`--rerere-autoupdate`、strategy surface 尚未公开。

- 当前矩阵承诺常用 Git 行为已支持；新增语义必须同步矩阵、用户文档和测试。


## 设计方案

- 入口与分发：已公开接入 `src/cli.rs::Commands`；已由 `src/command/mod.rs` 导出。CLI 层在 `src/cli.rs` 把解析后的参数交给命令模块，命令模块负责把领域错误转换为 `CliError` / `CliResult`。
- 源码分层：主要实现文件为 `src/command/revert.rs`。参数/子命令类型包括：`RevertArgs`；输出、错误或状态类型包括：`RevertOutput`；主要执行函数包括：`execute`、`execute_safe`。
- 执行路径：`execute_safe` 负责 CLI 安全包装、错误映射和输出配置；索引路径会加载、比较、刷新或保存 `.libra/index`；对象路径会解析 revision 并读写 blob/tree/commit/tag 等对象；引用路径会读取或更新 SQLite refs、HEAD 与 reflog。

- 流程图：以下流程图按当前源码分层展示主路径和底层对象边界，便于维护者把代码入口、执行函数和副作用范围对应起来。

```mermaid
flowchart TD
    A["入口与分发<br/>src/cli.rs::Commands"] --> B["源码分层<br/>src/command/revert.rs"]
    B --> C["参数模型<br/>RevertArgs"]
    C --> D["执行路径<br/>execute / execute_safe"]
    D --> E["底层对象<br/>IndexEntry / Index / .libra/index / Blob"]
    D --> F["输出与错误<br/>RevertOutput"]
    E --> G["副作用边界<br/>写入分支需先预检"]
```

- 底层操作对象：`IndexEntry`（索引条目，承载路径、mode、object id 和 stat 元数据）；`Index` / `.libra/index`（暂存区状态、路径条目和刷新/保存边界）；`Blob`（文件内容或 LFS pointer 写入对象库后的 blob 对象）；`Commit`（提交对象、父提交关系和提交消息载荷）；`TreeItem` / `TreeItemMode`（tree 中的路径项和 mode）；`Tree`（由索引或对象遍历生成的目录树对象）；`Branch` / branch store（SQLite refs 上的分支读写、过滤和上游关系）；`Head`（SQLite 中的 HEAD 指向、当前分支和 detached 状态）；`ObjectHash`（SHA-1/SHA-256 对象 ID 和 revision 解析结果）
- 输出与错误契约：人类输出、`--json` / `--machine` 输出和 quiet/verbose 分支必须继续走现有 `OutputConfig` / `emit_json_data` / `CliError` 路径；新增失败模式要补稳定错误码、用户提示和回归测试。
- 副作用边界：凡是写入索引、对象库、refs/HEAD、reflog、SQLite/D1、工作树或远端的路径，都必须先完成参数校验和 dry-run/预检分支，再执行持久化，避免部分写入后静默成功。

## 实现历史

- 本节依据本地 main 分支提交历史重写，筛选与该命令实现、测试或文档路径直接相关的提交；以下是归纳后的实现脉络。
- 基础实现节点：当前 HEAD 支持单父提交的反向变更（`<commit>` + `-n/--no-commit`），并通过 `revert_single_commit` 中的 mainline 选择逻辑支持 merge commit revert（`-m/--mainline`）。
- 2026-05-21 `752c516f`（`test(revert): pin RevertError Display + stable_code surfaces (v0.17.703)`）：测试契约：pin RevertError Display + stable_code surfaces (v0.17.703)；相关行为已有回归守卫，后续变更需要继续满足。
- 2026-06-18：恢复 `-m/--mainline` merge commit revert（原始内容由一次 reconcile 丢弃，仅遗留提交消息），重新应用 `b5af38a` 的源码、错误变体（`MainlineRequired` / `MainlineForNonMerge` / `InvalidMainline`，全部 exit 128）、测试与文档。
- 历史结论：当前文档应以这些提交之后的代码、测试和兼容矩阵为准；更早的迁移式文档只保留为背景，不再作为事实来源。

## 当前状态

- 公开状态：已公开；模块状态：已导出。
- 用户文档：`docs/commands/revert.md`。
- Synopsis：`libra revert [-n | --no-commit] [-m | --mainline <parent-number>] [-s | --signoff] [-e | --edit] [--no-edit] [--no-rerere-autoupdate] [--json] [--quiet] <commit>...` ｜ `libra revert --continue` ｜ `libra revert --skip` ｜ `libra revert --abort`。
- 公开参数/子命令包括：`<commit>...`（位置参数，`--continue`/`--skip`/`--abort` 时可省略）、`-n, --no-commit`、`-m, --mainline <parent-number>`、`-s, --signoff`、`-e, --edit`（在默认 `Revert "<subject>"` 消息上打开编辑器，经 `edit_revert_message`→`editor::resolve_editor`/`edit_message`；编辑后剥离 `#` 注释行并 trim，空消息报错 `EmptyMessage`；与 `--no-edit` 互斥）、`--no-edit`（接受式 no-op：即 Libra 默认的非交互行为）、`--no-rerere-autoupdate`（接受式 no-op：Libra 无 rerere，无可更新；`no_rerere_autoupdate` 字段解析后不被读取。Git 的反向 `--rerere-autoupdate` 未公开）、`--continue`、`--skip`、`--abort`、`--json`、`--quiet`。多个 commit 按给定顺序依次 revert，每个相对前一次产生的 HEAD 各自生成一个 revert commit；中途冲突即停止并把剩余 commit 存入 `RevertState.remaining`，已完成的保留，`--continue`/`--skip` 自动续作其余。`-n/--no-commit` 与 `-m/--mainline` 与多 commit 互斥（映射 `RevertError::MultiCommitUnsupported`）。`-s/--signoff` 经 `signoff_trailer` 追加 `Signed-off-by`。
- **冲突 sequencer**：当某文件自被 revert 的提交后又被改动且 revert 与之重叠时，`three_way_revert_blob`（`diffy::merge_bytes`，base=被 revert 提交的 blob / ours=当前 / theirs=父提交）写入冲突标记到 index+worktree，并把 `RevertState`（orig_head/reverted_commit/signoff/conflicted_paths）经 `utils::atomic_write::write_atomic`（临时文件 → fsync → rename → fsync 父目录，`lore.md` §7.7）原子且 fsync 持久化到 `.libra/revert-state.json`——崩溃只会留下完整或缺失的 state，绝不残留半截文件破坏 `--continue`/`--abort` 恢复；revert 以 exit 1 暂停（`RevertError::Conflicts`）。冲突时把序列中尚未处理的提交存入 `RevertState.remaining`（`#[serde(default)]`，JSON state，无需 DB 迁移）。`--continue` 拒绝 index 中仍含 `<<<<<<<` 标记（`UnresolvedConflicts`），否则从已解决的 index 构建单亲 revert commit，随即清理当前冲突的 state（在续作前清，避免续作中途的非冲突错误留下指向已完成提交的陈旧 state），再经共享的 `revert_sequence` 续作 `remaining` 队列（队列中再冲突则 `revert_sequence` 重存新 state）；`--skip` 经 `restore_to_orig_head` 丢弃当前冲突提交的改动（恢复到 `orig_head` 的树，HEAD 在冲突时已等于 `orig_head`）后同样先清理 state、再续作 `remaining`；`--abort` reset 到 `orig_head` 并清理 state。开始新 revert 前若已有进行中的 revert 会报 `RevertInProgress`。（区别于 cherry-pick 的 SQLite stage-1/2/3 模型：revert 用 JSON state + stage-0 标记。）


## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| ✅ 已实现 | `-e`/`--edit` 与 `--no-edit` | `--edit` 在默认 `Revert "<subject>"` 消息上打开编辑器（`edit_revert_message`：`editor::resolve_editor` 解析 `$GIT_EDITOR`/`core.editor`/`$VISUAL`/`$EDITOR`，`editor::edit_message` 打开，结果剥离 `#` 注释行 + trim，空→`EmptyMessage`，无编辑器→`Editor` 错误，均 129）；编辑在 `resolve_revert_message`（默认消息→`edit_revert_message`）中**在改动工作树之前**完成（`create_revert_commit`/`create_empty_revert_commit` 只接收最终消息），故编辑器失败/空消息不会留下半应用的 revert；直接路径用 `args.edit`、`--continue` 经 `RevertState.edit`（`#[serde(default)]`，向后兼容）、root 路径用 `resolve_root_revert_message`。与 git 不同 Libra revert 默认不打开编辑器，`--edit` 为 opt-in，与 `--no-edit`（默认行为的 no-op）`conflicts_with` 互斥。带集成测试 `test_revert_edit_opens_editor`（`core.editor` 脚本改写消息 + `--edit --no-edit` 冲突）。 |
| ✅ 已实现 | `--no-rerere-autoupdate` | 接受式 no-op：Libra 无 rerere，无可更新（带集成测试 `test_revert_no_rerere_autoupdate_is_accepted_noop`）。Git 的反向 `--rerere-autoupdate` 未公开。 |
| ✅ 已实现 | 编辑消息 `-e`/`--edit` | 见上方 `-e`/`--edit` 与 `--no-edit` 行：在生成消息上打开编辑器（opt-in，与 git 默认不同）。 |
| ✅ 已实现 | Skip 当前 commit | `--skip`：`run_revert_skip` 经 `restore_to_orig_head` 丢弃当前冲突提交后用 `revert_sequence` 续作 `RevertState.remaining`；剩余为空时清理 state 不建提交。与 `--continue`/`--abort` 互斥。带回归测试 `test_revert_skip_continues_with_remaining` / `test_revert_skip_with_nothing_remaining`。 |
| ✅ 已实现 | 多提交冲突自动续作 | 冲突时把剩余提交队列存入 `RevertState.remaining`；`--continue`/`--skip` 经共享 `revert_sequence` 自动续作其余提交（此前剩余提交会被静默丢弃）。带回归测试 `test_revert_continue_drains_remaining_commits`。 |
| ✅ 已实现 | P0-08 identity/subject 保真 | `create_revert_commit` / `create_empty_revert_commit` 不再使用 `Commit::from_tree_id` 的固定 `mega <admin@mega.org>` 身份，而是走 `commit::create_commit_signatures(None, None)`；`build_revert_message` 使用 `parse_commit_msg` 后的首行作为 `Revert "<subject>"`。带 compat 测试 `compat_sequencer_message_author::revert_uses_current_identity_and_strips_signed_subject`。 |
| 兼容差异项 | 策略 | 原始对照：--strategy <s>；相关参数/替代：不适用；当前说明：不支持。 后续实现时需要补对应回归测试并同步兼容矩阵。 |

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](_general.md)；这是命令设计、实现、测试和文档同步的强制要求。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/<cmd>.md` 和相关测试。
- 新增 Git 兼容参数时必须明确 tier、错误码、JSON/机器输出契约和回归测试。
