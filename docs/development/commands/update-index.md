# update-index 命令开发设计

## 命令实现目标

`libra update-index` 直接修改 `.libra/index`：`--add`/`--remove` 暂存/移除工作树路径，`--cacheinfo` 按对象 id 注册条目（不读工作树），用于纯对象构造可被 `write-tree` 读取的 index。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`--add`、`--remove`、`--cacheinfo <mode>,<object>,<path>`（mode ∈ 100644/100755/120000/160000；对象登记时无需已存在，与 Git 一致；后续 `write-tree`/`commit` 会校验 blob/tree 对象存在和类型），`--json`/`--machine`，`--force-remove`（issues/490 SW-02），`--skip-worktree`/`--no-skip-worktree`（issues/490 SW-07），`--refresh`/`--really-refresh`/`-q`/`--ignore-missing`/`--unmerged`、`--chmod=(+|-)x`、`--info-only`、`--verbose`、`--index-info`/`--stdin`/`-z`（issues/479 UI-01/UI-02/UI-03）。
- 未公开（延后）：`--assume-unchanged`/`--no-assume-unchanged`（DEFER-02）、`--unresolve`/`--again`（DEFER-01）等。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::UpdateIndex` → `command::update_index::execute_safe`。
- 源码分层：`src/command/update_index.rs`：`UpdateIndexArgs`（`add`/`remove`/`force_remove`/`skip_worktree`/`no_skip_worktree`/`cacheinfo: Vec<String>`/`paths`）、`execute`/`execute_safe`、`UpdateIndexOutput`（`--json`：`updated`/`removed`）、`parse_cacheinfo`、`resolve_within_worktree`。复用 `git_internal::Index`（`add`/`update`/`remove`/`save`）、`IndexEntry::new_from_blob`/`new_from_file`、`object_ext::BlobExt`（`from_file`/`from_lfs_file`/`save`）、`util::is_sub_path`、`lfs::is_lfs_tracked`、`utils::index_ext`（SW-03 helper：`update_preserving_flags`/`update_preserving_file_mode`；SW-07：`set_skip_worktree`）。
- 执行路径：`require_repo` → `Index::load` → 应用 `--cacheinfo`（`parse_cacheinfo`：splitn(3,',') 解析 mode/oid/path；mode 白名单校验；oid 经 `ObjectHash::from_str` + `HashKind::hex_len()` 长度校验；path 拒绝绝对/`..`；`new_from_blob`+设 mode；`index.update`）→ 应用位置路径：① `--skip-worktree`/`--no-skip-worktree` 优先于 `--force-remove`（git `update_one` 先处理 mark 模式）：经 `index_ext::set_skip_worktree` 原地翻转 stage-0 条目的 `skip_worktree` 位（不读工作树、不清 `intent_to_add`），索引中不存在的路径报 `Unable to mark file <path>`（128）且不保存 index；② `--force-remove` 删除全部 stage、未知路径无操作；③ skip-worktree 条目短路 add/remove 处理（git `process_path` 的 skip-worktree 分支）：显式 `--remove` 删除条目（exit 0），其余情况为 no-op——工作树副本永不重暂存；④ 其余路径走既有 `--remove`/`--add` 分流与暂存 → `index.save`。
- 有序选项漫游（UI-01/02/03，2026-10-20）：`ordered_operations` 按原始 argv 顺序把 token 映射为 `Op::{Path, Cacheinfo, Refresh, IndexInfo, Stdin}`，每个 `Op::Path` 捕获当时位置的 `PathContext`（`--add`/`--remove`/`--force-remove`/`--skip-worktree`/`--no-skip-worktree`/`--info-only`/`--verbose`/`--chmod` 状态）——`-q`/`--ignore-missing`/`--unmerged` 必须位于 `--refresh` 之前（git 按序处理选项）；`--index-info`/`--stdin` 必须是最后一个参数（129），两者在流尾以最终 flag 状态（`ctx_final`）处理。全部操作作用于内存 index 后一次 `index.save`：任何一行非法（`parse_index_info_line` 失败）→ 128 且 index 字节不变（GC-PL-02 原子性）。
- `--index-info` 行格式（git `builtin/update-index.c` 语义）：`<mode> SP <oid> TAB <path>`、`<mode> SP <type> SP <oid> TAB <path>`（type token 忽略）、`<mode> SP <oid> SP <stage> TAB <path>`（stage 0-3）；`mode 0` 删除全部 stage（oid 仍需为合法 hex 长度）；写 stage-0 会移除更高 stage，写 stage-1/2/3 保留 stage-0（与 git 2.56 实跑一致）；`"` 引号路径仅在 LF 模式做 C 风格 unquote；`verify_path` 拒绝的路径软跳过（git 行为）。
- 本轮 TC-3689 输出修复只处理 quiet 来源：command-position `-q` 按 `ordered_operations` 的先后顺序影响 refresh；全局 `--quiet` 及命令前 `-q` 仍抑制全部 stdout，JSON/machine 不打印 human refresh 行。不得把 Clap 最终 quiet 值 OR 到每次刷新而让末尾 `-q` 追溯生效；共享 RefreshReport 的事件与错误判定保持原有行为。
- `--refresh` 家族（ADR-PL-06）：`src/command/refresh.rs` 为 `add --refresh` 与 `update-index --refresh` 共享的单一实现（`filter_refresh_candidates`/`refresh_candidates`），按 git 2.56 实跑语义：stat-only 漂移静默刷新、内容修改/文件缺失 → stdout `<path>: needs update` + exit 1、未合并 → `<path>: needs merge` + exit 1；`-q` 抑制「needs update」行与退出码但不抑制「needs merge」；`--ignore-missing` 跳过缺失文件；`--unmerged` 跳过未合并条目；`--really-refresh` ≡ `--refresh`（无 assume-unchanged 位，文档化等价）；带位置路径时仍刷新整个 index（git 实跑行为）。哈希用 `command::calc_file_blob_hash`（正确 blob 哈希），不复用 `git_internal::Index::refresh` 的裸内容哈希。
- `--chmod=(+|-)x`（ADR-PL-06/UI-03）：`refresh::chmod_entry` 为 `add` 与 `update-index` 共享的单一 mode 翻转实现；按 argv 位置作用于其后路径直至被覆盖（t2107:93-97 矩阵）；非普通文件/未跟踪且无 `--add` → 128。
- `--info-only`：`stage_working_tree_path(info_only=true)` 走 `calc_file_blob_hash` + `verified_index_entry`，不调用 `blob.try_save()`——暂存路径本身不写对象（操作日志快照可能捕获工作树内容，属既有独立行为，已登记为文档化差异）。
- `--verbose`：stdout 逐条 `add '<path>'` / `remove '<path>'` / `chmod (+|-)x '<path>'`（human 模式；JSON 与全局 `-q` 保持机器输出）。
- 安全：`--cacheinfo` path 与 `--add` 路径均拒绝逃出 worktree（path-traversal/绝对路径）；`--cacheinfo` 不写对象（仅注册），与 Git 一致；对象登记时不要求存在，但 `write-tree`/`commit` 的 P0-09 预检会在写 tree/commit 前 fail-closed（`LBR-REPO-002`）。
- 底层操作对象：`.libra/index`、对象库（`--add` 写 blob）。无 refs/网络写入。
- 输出与错误契约：human 静默 / `--json` 计数；用法错误 `command_usage`+`with_exit_code(128)`，工作树文件缺失/无效 oid 用 `CliInvalidTarget`/`RepoStateInvalid` → 128。

## 实现历史

- 2026-10-20（issues/479 UI-01/UI-02/UI-03）：新增 `--index-info`/`--stdin`/`-z`、`--refresh`/`--really-refresh`/`-q`/`--ignore-missing`/`--unmerged`、`--chmod=(+|-)x`、`--info-only`、`--verbose`；共享 refresh/chmod helper 抽至 `src/command/refresh.rs`（`add.rs` 同源调用，`add_test` 原样通过）；测试 `update_index_test` 34 用例 + 单测 7 个 + EC-01 契约行；COMPATIBILITY.md `:241` 原行追加；C 组 bump / D 组发布待授权。
- 2026-06-30（GGT-06，`grit-gap.md` 阶段 2）：与 `update-ref` 同属 GGT-06；本命令先行公开。
- 2026-08-10（plan-20260729 CTF-P03/CTF-P05）：**`--add` 与 `--remove` 同给时按磁盘存在性分流**。此前定位路径循环以 `if args.remove { …; continue; }` **无条件先行**，`--add` 在两者同给时永远得不到执行——`update-index --add --remove <存在的文件>` **退出 0 却什么都没暂存**（`ls-files` 里没有该路径），静默失败无任何诊断。Git 的口径是：`--add` 与 `--remove` 是两项**互不排斥的许可**（`--add` 许可加入 index 尚不认识的路径，`--remove` 许可丢弃工作树已消失的路径），两者同给时由**路径是否存在于磁盘**决定走哪一支；上游 t4 的多处 setup 正是一次调用同时给出两者。现改为条件分流：
  ```rust
  if args.remove {
      let present = workdir.join(path_str).symlink_metadata().is_ok();
      if !args.add || !present { /* index.remove; continue */ }
  }
  // 否则落到既有暂存路径
  ```
  三个实现要点：① 存在性用 `symlink_metadata` 而非 `metadata`——**悬空符号链接必须算存在**，因为链接本身就是可暂存条目（mode `120000`，内容为 target bytes）；② base 复用循环上方既有的 `workdir` 绑定，`Path::join` 遇绝对参数会替换 base，与下方 `resolve_within_worktree` 同口径，不会两处对「哪个文件」有分歧；③ **`--remove` 单独给出时刻意保持既有语义**（一律删除该路径，无论其是否仍存在）——这是有意收窄而非疏漏：Git 在该情形会 refresh 仍存在的路径，但那超出本次修复的行为轴，且 `--help` 示例（`libra update-index --remove old.txt`）与既有测试都依赖现语义，改它属于另一张卡的事。条件式 `!args.add || !present` 在未给 `--add` 时恒真，故该路径的行为可证不变。
  已知边缘变化（轴外、已登记）：组合模式下若路径逃出 worktree 且存在，现在会落到 `resolve_within_worktree` 报 usage 错误，而非静默走 `index.remove`。
  存在性探测与随后的暂存是两次独立的文件系统观察，存在 TOCTOU 窗口——这是本命令既有形态，非本次引入。
  回归覆盖：`tests/command/update_index_test.rs` 全量 + 迁移用例 `t4_port_t4001_renamed_and_edited_the_file`、`t4_port_t4004_prepare_work_tree`、`t4_port_t4007_prepare_work_tree`。

## 当前状态

- 公开状态：已公开（`Commands::UpdateIndex`）。
- Synopsis：`libra update-index [--add|--remove] <path>... | --cacheinfo <mode>,<object>,<path>...`。
- 测试：`tests/command/update_index_test.rs`（cacheinfo→write-tree round-trip、`--add`、`--remove`、非法 mode/oid → 128、未跟踪路径无 `--add` → 128、非仓库 128、`--json`）；`tests/compat/write_tree_missing_object_test.rs` 覆盖未解析 cacheinfo 对象被后续写入路径拒绝；`tests/compat/symlink_basic_test.rs` 覆盖 `--add` symlink mode `120000` / link target blob。
- 用户文档：`docs/commands/update-index.md`（EN + zh-CN）。
- plan-20260708 P0-11 后，`--add` symlink 不跟随目标路径，blob 内容为 link target bytes，index mode 由 `IndexEntry::new_from_file` 记录为 `120000`。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 兼容差异项 | `--assume-unchanged`/`--no-assume-unchanged`（DEFER-02）、`--unresolve`/`--again`（DEFER-01） | 延后；按需补齐并同步矩阵与测试。`--force-remove`（SW-02）、`--skip-worktree`/`--no-skip-worktree`（SW-07）、stat 刷新/`--chmod`/`--index-info`/`--stdin`/`--info-only`/`--verbose`（issues/479 UI-01/02/03）已实现。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 新增 flag 必须明确 tier、退出码、JSON/机器输出契约与回归测试。
