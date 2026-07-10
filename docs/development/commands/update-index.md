# update-index 命令开发设计

## 命令实现目标

`libra update-index` 直接修改 `.libra/index`：`--add`/`--remove` 暂存/移除工作树路径，`--cacheinfo` 按对象 id 注册条目（不读工作树），用于纯对象构造可被 `write-tree` 读取的 index。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`--add`、`--remove`、`--cacheinfo <mode>,<object>,<path>`（mode ∈ 100644/100755/120000/160000；对象登记时无需已存在，与 Git 一致；后续 `write-tree`/`commit` 会校验 blob/tree 对象存在和类型），`--json`/`--machine`。
- 未公开（延后）：裸路径 stat 刷新、`--force-remove`、`--chmod`、`--assume-unchanged`、`--skip-worktree`、`--index-info`、`--refresh` 等。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::UpdateIndex` → `command::update_index::execute_safe`。
- 源码分层：`src/command/update_index.rs`：`UpdateIndexArgs`（`add`/`remove`/`cacheinfo: Vec<String>`/`paths`）、`execute`/`execute_safe`、`UpdateIndexOutput`（`--json`：`updated`/`removed`）、`parse_cacheinfo`、`resolve_within_worktree`。复用 `git_internal::Index`（`add`/`update`/`remove`/`save`）、`IndexEntry::new_from_blob`/`new_from_file`、`object_ext::BlobExt`（`from_file`/`from_lfs_file`/`save`）、`util::is_sub_path`、`lfs::is_lfs_tracked`。
- 执行路径：`require_repo` → `Index::load` → 应用 `--cacheinfo`（`parse_cacheinfo`：splitn(3,',') 解析 mode/oid/path；mode 白名单校验；oid 经 `ObjectHash::from_str` + `HashKind::hex_len()` 长度校验；path 拒绝绝对/`..`；`new_from_blob`+设 mode；`index.update`）→ 应用位置路径（`--remove` → `index.remove`；否则要求已跟踪或 `--add`，`resolve_within_worktree`（`is_sub_path` 守卫）+ `symlink_metadata` 工作树存在性校验 + 读取普通文件/LFS pointer 或 symlink target bytes 写 blob + `IndexEntry::new_from_file` + `index.update`）→ `index.save`。
- 安全：`--cacheinfo` path 与 `--add` 路径均拒绝逃出 worktree（path-traversal/绝对路径）；`--cacheinfo` 不写对象（仅注册），与 Git 一致；对象登记时不要求存在，但 `write-tree`/`commit` 的 P0-09 预检会在写 tree/commit 前 fail-closed（`LBR-REPO-002`）。
- 底层操作对象：`.libra/index`、对象库（`--add` 写 blob）。无 refs/网络写入。
- 输出与错误契约：human 静默 / `--json` 计数；用法错误 `command_usage`+`with_exit_code(128)`，工作树文件缺失/无效 oid 用 `CliInvalidTarget`/`RepoStateInvalid` → 128。

## 实现历史

- 2026-06-30（GGT-06，`grit-gap.md` 阶段 2）：与 `update-ref` 同属 GGT-06；本命令先行公开。

## 当前状态

- 公开状态：已公开（`Commands::UpdateIndex`）。
- Synopsis：`libra update-index [--add|--remove] <path>... | --cacheinfo <mode>,<object>,<path>...`。
- 测试：`tests/command/update_index_test.rs`（cacheinfo→write-tree round-trip、`--add`、`--remove`、非法 mode/oid → 128、未跟踪路径无 `--add` → 128、非仓库 128、`--json`）；`tests/compat/write_tree_missing_object_test.rs` 覆盖未解析 cacheinfo 对象被后续写入路径拒绝；`tests/compat/symlink_basic_test.rs` 覆盖 `--add` symlink mode `120000` / link target blob。
- 用户文档：`docs/commands/update-index.md`（EN + zh-CN）。
- plan-20260708 P0-11 后，`--add` symlink 不跟随目标路径，blob 内容为 link target bytes，index mode 由 `IndexEntry::new_from_file` 记录为 `120000`。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容差异项 | 裸路径 stat 刷新、`--force-remove`/`--chmod`/`--assume-unchanged`/`--skip-worktree`/`--index-info`/`--refresh` | 延后；按需补齐并同步矩阵与测试。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 新增 flag 必须明确 tier、退出码、JSON/机器输出契约与回归测试。
