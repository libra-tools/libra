# `libra op`

检查和恢复命令级 operation history。

## 概要

```bash
libra op log [OPTIONS]
libra op show [OPTIONS] <OP_REF>
libra op restore [OPTIONS] <OP_REF>
```

## 说明

`libra op` 在命令行上暴露 operation service 和 wrapper layers 持久化的 operation graph。

当前支持三个子命令：

- `op log`：列出已记录 operations，支持分页和可选命令过滤。
- `op show`：检查一个 operation，并可选显示捕获的 restore view。
- `op restore`：将 HEAD 和分支 refs 恢复到先前捕获的 view。

## Operation References

`<OP_REF>` 可以是：

- 具体 operation id，例如 `019e3f00-8ee5-7e62-a54c-0ab1f1bba0f9`
- reflog 风格索引，例如 `@{0}` 表示最新 operation，`@{1}` 表示前一个

## `libra op log`

列出 operation history。

```bash
libra op log [--page <N>] [-n <PER_PAGE>] [--command <NAME>] [--verbose]
```

### 选项

### `-n, --number <PER_PAGE>`

每页显示的 operations 数量。默认 `50`。

```bash
libra op log -n 20
```

### `--page <N>`

要显示的页码。默认 `1`。

```bash
libra op log --page 2 -n 20
```

### `--command <NAME>`

按精确命令名过滤 operations，例如 `branch` 或 `op restore`。

```bash
libra op log --command branch
libra op log --command "op restore"
```

### `--verbose`

将一个 operation 显示为包含 actor、status 和 timestamp 的多行块。

```bash
libra op log -n 5 --verbose
```

## `libra op show`

检查单个 operation。

```bash
libra op show [--view] <OP_REF>
```

### 选项

### `--view`

打印捕获的 restore view，包括 HEAD target 和 refs。

```bash
libra op show @{0} --view
```

## `libra op restore`

将仓库状态恢复到先前捕获的 operation view。HEAD 和捕获的 branch refs 会重置为目标 view，本地分支中不存在于该 view 的会被 prune，因此 restore 会复现该 operation 的精确本地分支集合。恢复后的 HEAD branch 始终保留；remote-tracking refs 和 Libra-owned internal refs（locked `main`/`intent`/`traces` branches 以及保留 `libra/` namespace，例如 AI history branch `libra/intent`）永不 prune。

```bash
libra op restore [--force] [--dry-run] <OP_REF>
```

### 选项

### `--force`

即使工作树 dirty，也允许继续 restore。

```bash
libra op restore @{0} --force
```

### `--dry-run`

显示目标 HEAD 和 refs，但不写入新的 restore operation。

```bash
libra op restore @{0} --dry-run
```

## 示例

```bash
# 列出最新十个 operations
libra op log -n 10

# 只显示第 2 页上的 branch operations
libra op log --command branch --page 2 -n 5

# 检查最新 operation 及其 view snapshot
libra op show @{0} --view

# 恢复到前一个 operation view
libra op restore @{1}

# 预览 restore，不修改仓库状态
libra op restore @{1} --dry-run
```

## 说明

- `op restore` 成功时会记录一个新的 `op restore` operation。
- `op restore --dry-run` 不写入新 operation。
- Restore 会重置 HEAD 和目标 view 中捕获的 branch refs，并 prune 该 view 中不存在的本地分支（恢复后的 HEAD branch 始终保留；remote-tracking refs 保持不变）。
