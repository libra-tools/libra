# `libra mv`

移动或重命名文件和目录。

## 概要

```
libra mv [<options>] <source>... <destination>
```

## 说明

`libra mv` 在工作树中移动或重命名文件和目录，并相应更新索引。最后一个参数始终是目标；所有前面的参数都是来源。当存在多个来源时，目标必须是已有目录。

该命令验证所有来源路径都存在、已在索引中跟踪、不处于冲突状态，并且位于仓库工作目录内。使用 `-k` / `--skip-errors` 时，无效来源候选会被跳过，有效候选继续执行。目录移动作为单次文件系统 rename 执行，同时为目录内每个已跟踪文件更新对应索引条目。被移动目录内的未跟踪文件会随文件系统 rename 一起移动，但不会被添加到索引。

所有文件系统移动成功后，索引会以原子方式更新：旧条目被移除，新条目（带重新计算的 blob 哈希）被插入。索引只会在所有操作成功完成后保存。

## 选项

| 标志 | 短选项 | 长选项 | 说明 |
|------|-------|------|-------------|
| Verbose | `-v` | `--verbose` | 在每次 rename 操作发生时打印它。 |
| Dry run | `-n` | `--dry-run` | 显示会移动什么，但不实际执行任何移动。 |
| Force | `-f` | `--force` | 覆盖已有目标文件，而不是报告错误。仅适用于普通文件和符号链接；目录不能被覆盖。 |
| Skip errors | `-k` | `--skip-errors` | 跳过无效来源候选，并移动剩余有效候选。 |
| Sparse | | `--sparse` | 接受 Git 的 sparse-checkout 标志；Libra 不维护 sparse-checkout 状态，因此这是 no-op。 |

### 选项细节

**`-v` / `--verbose`**

执行期间打印每次 rename 操作：

```bash
$ libra mv -v old.rs new.rs
Renaming old.rs to new.rs
```

**`-n` / `--dry-run`**

预览 rename 操作，不执行；不写入对象，也不记录 operation：

```bash
$ libra mv -n old.rs new.rs
Checking rename of 'old.rs' to 'new.rs'
Renaming old.rs to new.rs
```

Dry-run 模式不会进行文件系统更改或索引更新。

**`-f` / `--force`**

允许覆盖已有目标。没有此标志时，移动到已有路径是错误：

```bash
$ libra mv -f src/old.rs src/new.rs
```

**`-k` / `--skip-errors`**

跳过预检会失败的来源候选，并继续处理剩余有效候选：

```bash
$ libra mv -k missing.rs tracked.rs src/
```

如果所有来源都被跳过，命令会成功退出且不修改工作树或索引，与 Git 的 `mv -k` 行为一致。越过仓库边界的路径错误，以及多个来源移动到非目录目标，仍然是致命错误。

**`--sparse`**

为 Git CLI 兼容而接受。Libra 没有 sparse-checkout cone 状态，因此该标志不会改变 move plan、文件系统写入、索引更新或结构化输出。

## 常用命令

```bash
# 重命名文件
libra mv old_name.rs new_name.rs

# 将文件移动到目录中
libra mv utils.rs src/

# 将多个文件移动到目录中
libra mv a.rs b.rs c.rs src/

# 将目录移动到另一个目录中
libra mv old_dir/ parent_dir/

# 预览会发生什么
libra mv -n old.rs new.rs

# 强制覆盖
libra mv -f src/draft.rs src/final.rs

# 跳过无效来源
libra mv -k missing.rs tracked.rs src/

# 接受 Git sparse 标志并作为 no-op
libra mv --sparse old.rs new.rs

# Verbose 输出
libra mv -v old.rs new.rs
```

## 人类可读输出

普通移动（无标志）：

```text
(no output)
```

Verbose 模式：

```text
Renaming old.rs to new.rs
```

Dry-run 模式：

```text
Checking rename of 'old.rs' to 'new.rs'
Renaming old.rs to new.rs
```

全局 `--quiet` 会抑制 dry-run 和 verbose 人类输出，同时保留 stderr 上的警告和错误。

## 结构化输出

`libra mv` 在成功移动时支持全局 `--json` 和 `--machine` 标志。

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- 成功时 `stderr` 保持干净
- dry-run 输出报告计划的移动对，不改变文件系统或索引
- `moves` / `index_updates` 只列出实际计划或移动的来源候选
- `-k` / `--skip-errors` 增加 `skipped` 数组——每个被丢弃的来源一条 `{ "source", "reason" }`（如缺失或未跟踪的来源）。无跳过时省略该字段。人类模式对跳过保持静默（与 Git `mv -k` 一致），细节仅在 JSON 中体现。
- `--sparse` 是 no-op，不会增加 `sparse` 字段

示例：

```json
{
  "ok": true,
  "command": "mv",
  "data": {
    "moves": [
      {
        "source": "old.rs",
        "destination": "new.rs"
      }
    ],
    "index_updates": [
      {
        "source": "old.rs",
        "destination": "new.rs"
      }
    ],
    "dry_run": false,
    "forced": false,
    "verbose": false
  }
}
```

Dry-run：

```json
{
  "ok": true,
  "command": "mv",
  "data": {
    "moves": [
      {
        "source": "old.rs",
        "destination": "new.rs"
      }
    ],
    "index_updates": [
      {
        "source": "old.rs",
        "destination": "new.rs"
      }
    ],
    "dry_run": true,
    "forced": false,
    "verbose": false
  }
}
```

被跳过的来源（`-k` / `--skip-errors`）：

```json
{
  "ok": true,
  "command": "mv",
  "data": {
    "moves": [
      {
        "source": "tracked.rs",
        "destination": "src/tracked.rs"
      }
    ],
    "index_updates": [
      {
        "source": "tracked.rs",
        "destination": "src/tracked.rs"
      }
    ],
    "dry_run": false,
    "forced": false,
    "verbose": false,
    "skipped": [
      {
        "source": "missing.rs",
        "reason": "bad source, source=missing.rs, destination=src"
      }
    ]
  }
}
```

## 设计理由

### 为什么基于路径，而不是显式 `--source` / `--dest`？

Libra 遵循与 Git 的 `mv` 和 Unix `mv` 命令相同的约定：最后一个参数是目标，所有前面的参数是来源。这对每个 Unix 用户都很熟悉，并避免了用具名标志描述本质上是位置参数操作的冗长。

代价是命令要求至少两个参数，并且语义会根据目标是否为已有目录而变化。这与 Unix `mv` 和 Git `mv` 的取舍相同，几十年的使用证明它在实践中是直观的。

### 为什么 `--sparse` 是 no-op？

Git 的 `mv` 支持 `--sparse`，以允许移动 sparse-checkout cone 外的文件。Libra 尚未实现 sparse checkout 状态，因此没有需要放宽的 cone 成员关系。该标志被接受是为了让 Git 兼容脚本继续工作，但它不会改变正常的仓库边界校验。

### 为什么验证 tracked 状态？

与普通文件系统 `mv` 不同，`libra mv` 拒绝移动未在索引中跟踪的文件。这可以防止用户移动一个文件并期望版本控制记录 rename，但该文件从未被跟踪的困惑。如果需要移动未跟踪文件，请使用系统 `mv` 命令。

### 为什么拒绝冲突文件？

移动处于冲突状态的文件（索引中的 stages 1-3）会丢失冲突信息。Libra 要求先解决冲突，然后才能移动文件。

### 这与 Git 和 jj 如何比较？

Git 的 `mv` 命令设计类似：它在工作树中移动文件并更新索引。Libra 支持常用 Git 标志，包括 `-k` / `--skip-errors`；`--sparse` 在 Libra 具备 sparse-checkout 状态前作为 no-op 接受。

jj 没有 `mv` 命令。因为 jj 使用工作树自动快照，文件移动会由 working-copy 扫描器自动检测。用户只需使用系统 `mv` 命令移动文件，jj 会在下一次快照中记录更改。这对简单重命名效果很好，但对于大型重构，无法可靠地区分移动和删除后新建。

Libra 提供显式 `mv` 命令（类似 Git），因为其基于索引的模型需要显式通知 rename，以保持准确跟踪。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 来源路径 | `<source>...`（位置参数） | `<source>...`（位置参数） | N/A（使用系统 `mv`） |
| 目标 | 最后一个位置参数 | 最后一个位置参数 | N/A |
| Verbose | `-v` / `--verbose` | `-v` / `--verbose` | N/A |
| Dry run | `-n` / `--dry-run` | `-n` / `--dry-run` | N/A |
| 强制覆盖 | `-f` / `--force` | `-f` / `--force` | N/A |
| 结构化 JSON 输出 | `--json` / `--machine` | N/A | N/A |
| 跳过错误 | `-k` / `--skip-errors` | `-k` | N/A |
| Sparse checkout | `--sparse` 作为 no-op 接受 | `--sparse` | N/A |

注意：jj 没有专用 mv 命令。文件重命名由 working-copy 快照机制自动检测。

## 错误处理

| 场景 | 错误消息 |
|----------|---------------|
| 参数少于 2 个 | 打印用法信息 |
| 来源不存在 | `fatal: bad source, source=<src>, destination=<dst>` |
| 来源与目标相同 | `fatal: can not move directory into itself` |
| 多个来源且目标不是目录 | `fatal: destination '<dst>' is not a directory` |
| 来源未在索引中跟踪 | `fatal: not under version control, source=<src>, destination=<dst>` |
| 来源有 merge 冲突 | `fatal: conflicted, source=<src>, destination=<dst>` |
| 目标已存在且未使用 `--force` | `fatal: destination already exists, source=<src>, destination=<dst>` |
| 目录目标已包含来源名 | `fatal: destination already exists, source=<src>, destination=<dst>` |
| 路径在仓库外部 | `fatal: '<path>' is outside of the repository at '<workdir>'` |
| 多个来源指向同一目标路径 | `fatal: multiple sources moving to the same target path` |
| 使用 `-k` 时来源无效 | 跳过该来源；只要未发生仓库边界或目标形态致命错误，命令成功 |
| 文件系统 rename 失败 | `fatal: failed to move, source=<src>, destination=<dst>, error=<err>` |
| 索引保存失败 | `fatal: failed to save index after mv: <err>` |

mv renames a directory to a new nonexistent path.
