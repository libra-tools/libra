# merge-file 命令开发设计

## 命令实现目标

`libra merge-file` 对三个磁盘文件做文件级三路合并（以 `<base>` 为祖先合并 `<current>` 与 `<other>`），不触碰 branch merge sequencer。复用 `merge.rs` 的共享内建 driver 内容层，保证 text 冲突标记一致，并在仓库内继承按路径的 binary/union 分派。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`merge-file [-p|--stdout] [--diff3] [-q|--quiet] <current> <base> <other>`、`--json`/`--machine`、仓库内 `merge` gitattribute / `merge.default` 的 text/binary/union 分派、二进制输入检测、空文件、就地写入 + worktree-local gitdir 的 `merge-file-backup/` 备份（W2 §C.4.3）。仓库外恒按 text，忽略同目录 attributes 与 repo config。
- 退出码：0 干净 / 1 冲突（无论冲突数固定 1）/ 128 错误。
- **差异**：冲突标记标签为 `ours`/`theirs`（与 `merge.rs` 的 diffy 输出一致），非文件名；冲突退出码固定 1（Git 报告冲突数量——本实现按 grit-gap 计划固定 1）。
- 未公开（延后，`diffy` 0.4 不支持自定义标签/择边）：`-L <label>`、`--ours`/`--theirs`/`--union`、`--marker-size`。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::MergeFile` → `command::merge_file::execute_safe`。
- 源码分层：`src/command/merge_file.rs`：`MergeFileArgs`（`stdout`/`diff3`/`quiet`/`current`/`base`/`other`）、`execute`/`execute_safe`、`MergeFileOutput`（`--json`：`conflict`/`written`/`merged?`）、`read_input`/`write_with_backup`/`backup_path`。
- 合并核心：仓库内先用 `merge::read_merge_default_driver` + `builtin_merge_driver_for_path(<current>)` 选择内建 driver，再调共享 `merge_bytes_with_driver(base,current,other)`；仓库外直接固定 `BuiltinMergeDriver::Text` 且不读取 attributes/config。`--diff3` 只控制 text 冲突样式。**注意底层参数序为 (ancestor, ours, theirs)**。
- 二进制检测：三方任一含 NUL 字节 → 128 `cannot merge binary files: <file>`（`StableErrorCode::Unsupported`）。
- 输出：`-p` 非 json → 原始字节写 stdout；`-p` + json → merged 文本放进 JSON `merged` 字段（避免与 envelope 混在 stdout）；无 `-p` → `write_with_backup` 覆盖 `<current>`。
- 备份：仅在仓库内（`util::try_get_worktree_gitdir(None).ok()`）时，把原 `<current>` 备份到**当前 worktree local gitdir** 的 `merge-file-backup/<sanitized-path>`（W2 §C.4.3：备份对象属于本 worktree，两个 worktree 合并同名文件互不覆盖/清理对方备份；main 的 local gitdir 即 `.libra`，单 worktree 行为不变）；干净合并删备份，冲突保留 + （非 `-q`）提示。仓库外不备份（合并照常）。
- 退出码：冲突 → `CliError::silent_exit(1)`（标记已写出，冲突非错误）；错误 → 128。
- 底层操作对象：三个磁盘文件 + （写模式）worktree-local `merge-file-backup/`。无对象库/refs/网络写入；不校验内容对应 blob。

## 实现历史

- 2026-06-30（GGT-07，`grit-gap.md` 阶段 2）：新增；复用 `diffy`（与 `merge.rs::try_merge_blob_contents` 同源）。

## 当前状态

- 公开状态：已公开（`Commands::MergeFile`）。
- 测试：`tests/command/merge_file_test.rs`（干净合并 / 冲突标记 + exit 1 / diff3 / `-p` 不改文件 / 写模式覆盖 / 干净无备份 / 冲突留备份 / 二进制 128 / 空文件 / 缺文件 128 / `--json` / 仓库内 union+binary driver / 仓库外恒 text）。
- 用户文档：`docs/commands/merge-file.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 标签/择边 | `-L <label>`、`--ours`/`--theirs`/`--union`、`--marker-size` | 延后；`diffy` 0.4 不支持自定义标签/择边。 |
| 退出码精度 | 冲突退出码固定 1（非冲突计数） | 按 grit-gap 计划固定 1（稳定、不随 `-p` 变化）。 |
| EOL | 完整 CRLF/`text=auto` 归一化 | 暂按字节处理（D5 textconv 有意差异）。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 合并核心必须继续走 `merge_bytes_with_driver`；其中 text/union 的行级合并继续由共享 `diffy` 路径承载，不得自建第二套行级合并/标记逻辑。
