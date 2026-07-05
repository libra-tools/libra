# `libra prune`

从仓库中 prune 不可达对象的历史设计。

> 状态：未发布。`libra prune` 未注册到当前版本的公开 CLI。运行它会返回标准 unknown-command 错误（`LBR-CLI-001`）。下面的接口描述的是保留的设计材料，不是用户可见命令契约。

## 概要

```
libra prune [OPTIONS] [HEAD]...
```

## 说明

未发布设计实际上会使用 `refs/` 中所有可用 refs 运行 `libra fsck --unreachable`，可选地加上命令行指定的一组 heads，并从仓库中 prune 所有无法从这些 head objects 到达的 unpacked objects。此外，它还会 prune 那些也已存在于 packs 中的 unpacked objects。

具体来说，在 pack 中发现的不可达对象会保留。更多关于不可达对象的细节，请参考 `libra fsck --unreachable` 文档。

## 选项

### `-n, --dry-run`

报告将要移除的对象，但不实际移除任何内容。

```bash
$ libra prune -n
d670460b4b4aece5915caf5c68d12f560a9fe3e4 blob
```

### `-v, --verbose`

报告所有已移除对象。

```bash
$ libra prune -v
d670460b4b4aece5915caf5c68d12f560a9fe3e4 blob
```

### `--expire <TIME>`

只过期早于 `<TIME>` 的 loose objects。

```bash
$ libra prune --expire "2 weeks ago"
$ libra prune --expire 2024-01-01
```

### `[HEAD]...`

除了可从任意引用到达的对象外，还保留可从列出的 `HEAD`s 到达的对象。

```bash
$ libra prune HEAD~2
$ libra prune v1.0 v1.1
$ libra prune 74689c87fb53b6d666de95efea667d99ba2fa52a
```

## 示例

```bash
# Prune refs 无法到达的对象
libra prune

# 以 dry-run 模式 prune
libra prune -n

# 只 prune 已过期不可达对象，并显示详细输出
libra prune -v --expire "2 weeks ago"

# 只 prune 已过期不可达对象
libra prune --expire 2024-01-01

# 除 refs 外，还保留可从指定 heads 到达的对象
libra prune HEAD~2
```

## 人类可读输出

普通 prune（无 flags）：

```text
(no output)
```

Verbose mode：

```text
d670460b4b4aece5915caf5c68d12f560a9fe3e4 blob
```

Dry-run mode：

```text
d670460b4b4aece5915caf5c68d12f560a9fe3e4 blob
```

全局 `--quiet` 会抑制 dry-run 和 verbose human output，但保留 stderr 上的 warnings 和 errors。

## 结构化输出

如果此命令在未来版本发布，`libra prune` 应在成功 prune 时支持全局 `--json` 和 `--machine` 标志。

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- 成功时 `stderr` 保持干净
- dry-run output 报告计划 prune 的对象，但不实际移除对象。

示例：

```json
{
  "command": "prune",
  "data": {
    "expire": null,
    "heads": [
      "test"
    ],
    "objects": [
      {
        "object_id": "b13c288e945d00a4d16f195b33bf003b53d73dac",
        "object_type": "blob"
      },
      {
        "object_id": "74689c87fb53b6d666de95efea667d99ba2fa52a",
        "object_type": "blob"
      }
    ],
    "dry_run": true,
    "verbose": false
  },
  "ok": true
}
```

## 说明

大多数情况下，用户不需要直接调用 `libra prune`，而应调用 `libra gc`，它会把 pruning 与许多其他 housekeeping tasks 一起处理。

当 `libra prune` 与另一个进程并发运行时，它存在删除另一个进程正在使用但尚未创建引用的对象的风险。这可能只是导致另一个进程失败，也可能在该进程随后添加指向被删除对象的引用时损坏仓库。

通常，显式 `--expire` 值能显著缓解此问题。当用户确实需要直接运行此命令时，建议附加类似 `--expire 2.weeks.ago` 的过期值，并先以 dry-run 模式预览将被 prune 的对象。

## 错误处理

| 场景 | StableErrorCode | 退出 |
|------|-----------------|------|
| 不在 Libra 仓库中 | `LBR-REPO-001` | 128 |
| 无效或缺失 `--expire` 值 | `LBR-CLI-002` | 129 |
| 模糊对象名（匹配多个对象） | `LBR-CLI-002` | 129 |
| 无效 `HEAD` 参数或对象名 | `LBR-CLI-003` | 129 |
| Refs/reflogs/HEAD metadata 无效、缺失或指向缺失对象 | `LBR-REPO-002` | 128 |
| 加载 commit/tree/tag 数据或解析对象类型失败 | `LBR-REPO-002` | 128 |
| 读取 objects 目录、条目、metadata 或 pack indexes 失败 | `LBR-IO-001` | 128 |
| 移除 loose object 或空 prefix directory 失败 | `LBR-IO-002` | 128 |
| Pruning path 时内部不变量被违反 | `LBR-INTERNAL-001` | 128 |
