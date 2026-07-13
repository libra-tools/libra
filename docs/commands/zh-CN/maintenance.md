# `libra maintenance`

运行任务以优化 Git 仓库数据。

## 概要

```
libra maintenance <subcommand> [options]
```

## 说明

`maintenance` 命令运行一组计划维护任务，帮助 Libra 仓库保持高效和健康。它以 Git 的 `git maintenance` 命令（Git 2.29 引入）为模型。

任务可以单独运行，也可以一次全部运行。`--dry-run` 模式可预览将要更改的内容，而不执行任何写入。

## 子命令

### `run`

运行一个或多个维护任务。

```
libra maintenance run [--task <task>] [--dry-run] [--quiet]
```

**选项**

- `--task <task>` — 要运行的任务（可多次给出）。默认运行所有任务。
- `--dry-run` — 报告将要执行的操作，但不做任何更改。
- `--quiet`, `-q` — 抑制进度输出。

**支持的任务**

| 任务 | 说明 |
|---|---|
| `gc` | 递归追踪 SQLite refs/reflogs（含 annotated-tag target）、全部 index stage、文件型 stash reflog 的每个条目与 merge/rebase held-autostash sidecar 后删除不可达 loose objects；root 或可达对象损坏/不可读时在删除前 fail-closed |
| `loose-objects` | 将旧 loose objects 打包进新的 pack 文件 |
| `pack-refs` | 将单独 ref 文件折叠进 `packed-refs` |
| `incremental-repack` | 重新打包现有 pack 文件 |
| `commit-graph` | 写出 Git-compatible v1 commit-graph 文件（包括通过 EDGE chunk 支持 octopus merges，以及 32-byte OIDs + SHA-256 trailer 的 SHA-256 仓库） |
| `prefetch` | 预取 remote refs（需要 remote config；会跳过） |

### `register`

为当前仓库注册周期维护。

```
libra maintenance register [--schedule <schedule>]
```

- `--schedule <schedule>` — 类 cron schedule 表达式（默认：`hourly`）。

### `unregister`

取消当前仓库的周期维护注册。

```
libra maintenance unregister
```

### `status`

显示该仓库是否已注册维护。

```
libra maintenance status
```

## 示例

运行所有维护任务：

```
libra maintenance run
```

只运行垃圾回收：

```
libra maintenance run --task gc
```

预览将要执行的操作：

```
libra maintenance run --dry-run
```

用 daily schedule 注册仓库：

```
libra maintenance register --schedule=daily
```

以 JSON 显示注册状态：

```
libra --json maintenance status
```

## 另见

- [`libra gc`](./gc.md)（尚未实现）
- [`libra fsck`](./fsck.md)
