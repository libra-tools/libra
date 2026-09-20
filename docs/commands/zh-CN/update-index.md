# `libra update-index`

直接修改 index —— `git update-index` 的一个聚焦子集。[`write-tree`](write-tree.md) 的配套命令：`--cacheinfo` 可在不读取工作树的情况下、按对象 id 注册一个 index 条目，从而纯粹用对象构造一份 index。

## 用法

```
libra update-index --add <path>...
libra update-index --remove <path>...
libra update-index --cacheinfo <mode>,<object>,<path>...
```

## 说明

`update-index` 按顺序应用：所有 `--cacheinfo` 条目，然后是位置路径，最后保存 index。`--add` 与 `--remove` 是两项**许可**而非互斥选项：单给 `--remove` 时一律把各路径从 index 删除，单给 `--add` 时从工作树（重新）暂存，两者**同时给出**时则逐路径按其**在工作树中是否存在**分流——仍存在的被暂存，已消失的被移除。

- `--cacheinfo <mode>,<object>,<path>` 直接插入/更新一个条目。该对象**无需已存在**（与 Git 一致），因此可用 `hash-object` 计算的哈希构造 index。`<mode>` 为八进制文件模式：`100644`（文件）、`100755`（可执行）、`120000`（符号链接）、`160000`（gitlink）。对象 id 长度必须匹配仓库 hash 格式。path 是 index 键 —— 绝对路径与 `..` 穿越会被拒绝。后续 `write-tree` 或 `commit` 会校验对象存在性/类型；若 blob/tree 条目仍指向缺失或类型不匹配的对象，会以 `LBR-REPO-002` 失败。
- `--add <path>...` 从工作树（重新）暂存文件，允许尚未跟踪的路径。不带 `--add` 时，位置路径必须已被跟踪。若路径是符号链接，则暂存 mode `120000`，blob 内容为链接目标字节，并且不会跟随该链接。
- `--remove <path>...` 从 index 删除指定路径。与 `--add` 同时给出时按磁盘存在性逐路径分流：存在的被暂存、消失的被移除——上游的 setup 步骤正是这样一次调用同时给出两者。`--remove` **单独**给出时一律删除该路径，无论它是否仍然存在。

从工作树暂存时，若 blob 或其耐久云索引 marker 无法写入，命令会返回 `LBR-IO-002`，不会
panic，也不会保存缺少修复 ownership 的 index 条目；正常重试会重新登记失败调用已经持久化的
payload。

本地 index 持久化与后续云目录更新具有独立的耐久边界。若 blob 与 index 已保存后，后台
`object_index` 更新遇到终止错误，`update-index` 保留正常成功输出、向 stderr 发出可操作
警告，并留下原子 repair marker，供下一条具备 schema 感知的仓库命令自动重试。修复仍待
处理时，`cloud sync` 与破坏性 agent cleanup 会 fail closed。使用
`--exit-code-on-warning` 时，已完成的本地更新返回退出码 9 / `LBR-WARN-001`；无需重跑
`update-index`。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `--add` | 允许位置路径添加新的（未跟踪）文件。 | `libra update-index --add a.txt` |
| `--remove` | 从 index 删除位置路径。与 `--add` 同时给出时按磁盘存在性逐路径分流（存在→暂存，消失→移除）。 | `libra update-index --remove old.txt` |
| `--force-remove` | 无论工作树文件是否存在都删除给定路径的索引条目；索引中不存在的路径为无操作，未合并路径的 stage 1–3 全部删除。优先于 `--add`/`--remove`。 | `libra update-index --force-remove old.txt` |
| `--add --remove` | 同时给出两项许可：存在的暂存、消失的移除。 | `libra update-index --add --remove a.txt gone.txt` |
| `--cacheinfo <mode>,<object>,<path>` | 按对象 id 注册条目（可重复）。 | `libra update-index --cacheinfo 100644,<oid>,dir/f.txt` |
| `--json` / `--machine` | 结构化输出：`{ updated: <n>, removed: <n> }`。 | `libra --json update-index --add a.txt` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | index 已更新并保存。 |
| `9` / `LBR-WARN-001` | 本地 index 已保存，但云索引修复仍待处理，且使用了 `--exit-code-on-warning`。 |
| `128` | 不在仓库内、用法错误（`--cacheinfo` 非法、未跟踪路径且无 `--add`），或工作树文件缺失。 |
| `128` / `LBR-IO-002` | 工作树 blob 或耐久云索引 marker 持久化失败；修复存储权限后重试。失败文案为规范口径：对象负载已安全写入、未暂存任何路径，直接重试复用已存储负载、无需锁文件清理（锁超时另附持有者说明，锁文件永不删除）。 |

## 示例

```bash
# 用对象 id 构造一个 index 条目，再写出 tree
OID=$(libra hash-object -w data.bin)
libra update-index --cacheinfo 100644,"$OID",assets/data.bin
libra write-tree

# 可以登记暂不存在的 cacheinfo 对象，但 write-tree 会拒绝写出
libra update-index --cacheinfo 100644,1111111111111111111111111111111111111111,missing.bin
libra write-tree   # 返回 LBR-REPO-002

# 暂存与取消暂存工作树文件
libra update-index --add src/new.rs
libra update-index --add link-to-target   # 符号链接以 mode 120000 暂存
libra update-index --remove src/old.rs
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 暂存文件 | `libra update-index --add f` | `git update-index --add f` |
| 删除路径 | `libra update-index --remove f` | `git update-index --remove f` |
| 按 id 注册 | `libra update-index --cacheinfo m,oid,p` | `git update-index --cacheinfo m,oid,p` |

延后（未公开）：裸路径 stat 刷新、`--chmod`、`--assume-unchanged`、`--skip-worktree`、`--index-info` 等 Git 标志。
