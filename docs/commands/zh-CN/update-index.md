# `libra update-index`

直接修改 index —— `git update-index` 的一个聚焦子集。[`write-tree`](write-tree.md) 的配套命令：`--cacheinfo` 可在不读取工作树的情况下、按对象 id 注册一个 index 条目，从而纯粹用对象构造一份 index。

## 用法

```
libra update-index --add <path>...
libra update-index --remove <path>...
libra update-index --cacheinfo <mode>,<object>,<path>...
libra update-index --skip-worktree <path>...
libra update-index --refresh
libra update-index --index-info < <(printf '100644 <oid>\t<path>\n')
libra update-index --chmod=+x <path>...
```

## 说明

`update-index` 按顺序应用：所有 `--cacheinfo` 条目，然后是位置路径，最后保存 index。`--add` 与 `--remove` 是两项**许可**而非互斥选项：单给 `--remove` 时一律把各路径从 index 删除，单给 `--add` 时从工作树（重新）暂存，两者**同时给出**时则逐路径按其**在工作树中是否存在**分流——仍存在的被暂存，已消失的被移除。

- `--cacheinfo <mode>,<object>,<path>` 直接插入/更新一个条目。该对象**无需已存在**（与 Git 一致），因此可用 `hash-object` 计算的哈希构造 index。`<mode>` 为八进制文件模式：`100644`（文件）、`100755`（可执行）、`120000`（符号链接）、`160000`（gitlink）。对象 id 长度必须匹配仓库 hash 格式。path 是 index 键 —— 绝对路径与 `..` 穿越会被拒绝。后续 `write-tree` 或 `commit` 会校验对象存在性/类型；若 blob/tree 条目仍指向缺失或类型不匹配的对象，会以 `LBR-REPO-002` 失败。
- `--add <path>...` 从工作树（重新）暂存文件，允许尚未跟踪的路径。不带 `--add` 时，位置路径必须已被跟踪。若路径是符号链接，则暂存 mode `120000`，blob 内容为链接目标字节，并且不会跟随该链接。
- `--remove <path>...` 从 index 删除指定路径。与 `--add` 同时给出时按磁盘存在性逐路径分流：存在的被暂存、消失的被移除——上游的 setup 步骤正是这样一次调用同时给出两者。`--remove` **单独**给出时一律删除该路径，无论它是否仍然存在。
- `--skip-worktree <path>...` 为已跟踪路径设置 skip-worktree 位（index v3 扩展标志）：此后 `status`/`diff` 忽略该路径的工作树副本，且任何命令都不会重新暂存它——该路径可以保持缺失或本地分叉而不产生噪音。标记只写入 index，不读取工作树，条目内容不变。索引中不存在的路径报 `Unable to mark file`（退出码 128）。设置该位后 index 以 v3 格式写出；`--no-skip-worktree` 清除该位，当不再有任何条目携带扩展标志时 index 回到 v2。没有标记路径的仓库，index 字节保持 v2 不变。

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
| `--skip-worktree` | 为已跟踪路径设置 skip-worktree 位（index v3）：status/diff 忽略其工作树副本且不再暂存。未跟踪路径报 128。 | `libra update-index --skip-worktree s.txt` |
| `--no-skip-worktree` | 清除 skip-worktree 位；无扩展标志时 index 回到 v2。 | `libra update-index --no-skip-worktree s.txt` |
| `--add --remove` | 同时给出两项许可：存在的暂存、消失的移除。 | `libra update-index --add --remove a.txt gone.txt` |
| `--cacheinfo <mode>,<object>,<path>` | 按对象 id 注册条目（可重复）。 | `libra update-index --cacheinfo 100644,<oid>,dir/f.txt` |
| `--refresh` | 对照工作树重新 stat index：仅 stat 漂移的条目静默刷新；内容已修改或文件缺失的条目输出 `<path>: needs update` 并退出 1；未合并条目输出 `<path>: needs merge`。命令后的短旗标 `-q` 只抑制 needs update 的输出与对应退出码，不抑制 needs merge 的输出或退出码 1；`-q`、`--ignore-missing`、`--unmerged` 必须位于 `--refresh` 之前才影响该次刷新。Libra 全局 `--quiet`（或 `update-index` 之前的 `-q`）抑制全部 stdout，但不改变 refresh 退出码；其输出抑制优先于命令后的 `-q`。 | `libra update-index --refresh` |
| `--really-refresh` | 与 `--refresh` 相同（Libra 无 assume-unchanged 位）。 | `libra update-index --really-refresh` |
| `--chmod=(+\|-)-x` | 对随后（直至被覆盖）的位置路径与 `--stdin` 路径翻转可执行位；不触碰工作树文件。 | `libra update-index --chmod=+x a.sh` |
| `--info-only` | 记录对象 id，但暂存路径不写入 blob。 | `libra update-index --add --info-only new.txt` |
| `--verbose` | 在 stdout 逐条报告动作：`add '<path>'`、`remove '<path>'`、`chmod +x '<path>'` / `chmod -x '<path>'`。成功的 chmod 请求即使条目原本已有该 mode 也会输出记录。 | `libra update-index --add --verbose a.txt` |
| `--index-info` | 从 stdin 读取 index 记录（`-z` 为 NUL 分隔），必须是最后一个参数。接受 `<mode> SP <oid> TAB <path>`、`<mode> SP <type> SP <oid> TAB <path>` 或 `<mode> SP <oid> SP <stage> TAB <path>`；`mode 0` 删除全部 stage。非法行退出 128 且 index 不变。 | `printf '100644 <oid> 3\tfoo\n' \| libra update-index --index-info` |
| `--stdin` | 从 stdin 逐行读取路径（`-z` 为 NUL 分隔），必须是最后一个参数，按当前位置路径的相同规则处理。 | `printf 'a.txt\n' \| libra update-index --add --stdin` |
| `-z` / `--null` | 配合 `--index-info`/`--stdin` 使用 NUL 分隔记录。 | `libra update-index -z --add --stdin` |
| `--json` / `--machine` | 结构化输出：`{ updated: <n>, removed: <n> }`。 | `libra --json update-index --add a.txt` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | index 已更新并保存。 |
| `1` | `--refresh` 发现需更新/需合并的条目；index（含刷新后的 stat 字段）已保存。 |
| `9` / `LBR-WARN-001` | 本地 index 已保存，但云索引修复仍待处理，且使用了 `--exit-code-on-warning`。 |
| `128` | 不在仓库内、用法错误（`--cacheinfo` 非法、未跟踪路径且无 `--add`）、工作树文件缺失，或 `--skip-worktree`/`--no-skip-worktree` 作用于索引中不存在的路径。 |
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

# 将路径标记为仅索引：status/diff 忽略其工作树副本
libra update-index --skip-worktree s.txt
libra ls-files -v          # `S s.txt`
libra update-index --no-skip-worktree s.txt

# 暂存与取消暂存工作树文件
libra update-index --add src/new.rs
libra update-index --add link-to-target   # 符号链接以 mode 120000 暂存
libra update-index --remove src/old.rs

# 对照工作树刷新 index 的 stat 缓存
libra update-index --refresh
libra update-index -q --refresh        # 抑制 needs update，不抑制 needs merge

# 从 stdin 直接写入未合并 stage 条目
printf '100644 %s 3\tfoo\n' "$OID" | libra update-index --index-info
printf 'foo\0bar\0' | libra update-index -z --add --stdin

# 不触碰工作树地翻转可执行位
libra update-index --chmod=+x script.sh
libra update-index --add --info-only staged-later.txt   # 仅 index 条目
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 暂存文件 | `libra update-index --add f` | `git update-index --add f` |
| 删除路径 | `libra update-index --remove f` | `git update-index --remove f` |
| 标记 skip-worktree | `libra update-index --skip-worktree f` | `git update-index --skip-worktree f` |
| 按 id 注册 | `libra update-index --cacheinfo m,oid,p` | `git update-index --cacheinfo m,oid,p` |
| 刷新 stat 缓存 | `libra update-index --refresh` | `git update-index --refresh` |
| stdin 批量记录 | `libra update-index --index-info` / `--stdin [-z]` | `git update-index --index-info` / `--stdin [-z]` |
| 翻转可执行位 | `libra update-index --chmod=(+\|-)-x` | `git update-index --chmod=(+\|-)-x` |
| 仅登记对象 id 不存对象 | `libra update-index --add --info-only f` | `git update-index --add --info-only f` |

延后（未公开）：`--assume-unchanged`/`--no-assume-unchanged`、`--unresolve`、`--again` 等 Git 标志。

### index v3 与降级说明

对路径执行 `--skip-worktree` 后，index 以 Git 索引格式 v3 写出（上游 Git 自 2006 年起
使用的扩展标志格式）。Libra 同时读取 v2 与 v3。更早的 Libra 版本（index 读取器 ≤ 0.23.x）
会以「index 版本不受支持」错误拒绝 v3 index。降级前请先清除标记，让 index 回到 v2：

```bash
libra ls-files -v | grep '^S ' | cut -d' ' -f2- |
  xargs -r libra update-index --no-skip-worktree
```
