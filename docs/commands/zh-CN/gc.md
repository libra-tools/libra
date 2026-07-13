# `libra gc`

清理不可达 loose object，并移除陈旧的 pack 辅助文件。

## 概要

```bash
libra gc [--dry-run] [--prune=<date> | --no-prune] [--aggressive] [--auto] [--force]
```

## 说明

`libra gc` 会先使用默认的 `gc.reflogExpire` 与
`gc.reflogExpireUnreachable` 策略过期 reflog，然后从仓库引用、剩余 reflog 的
old/new 两端、annotated-tag target、全部 index stage、文件型 stash reflog 的每个条目、
merge/rebase held-autostash sidecar、进行中的操作状态以及本地 AI catalog 出发追踪可达对象；
root 读取/解析失败或可达对象缺失/损坏会在删除前中止。随后按照
prune 截止时间删除不可达的 loose object。配置了云备份时，尚未同步的
`object_index` 行会把对应 loose object 作为待备份数据保留；这些对象会报告为
retained unreachable object，不会混入 reachable graph roots。它还会检查
`.libra/objects/pack/`，在没有对应 `.keep` 保护且达到 prune 条件时，清理孤立
`.idx` 等陈旧 pack 辅助文件。

有效的 `.pack` + `.idx` 配对会复用 Libra 已有的 `verify-pack` / pack 解码路径
进行校验。格式异常的 pack 组会被保留并报告，不会阻塞其它无关清理。如果可达对象
遍历不完整，非 dry-run 的 loose-object 删除会在本次运行中跳过，并在 `warnings[]`
中给出原因。当前实现不会重写有效 pack、不会做 delta compression、不会创建 cruft
pack，也不会把 loose reachable objects 重新打包。

## 选项

| 标志 | 短选项 | 说明 | 默认值 |
|------|-------|-------------|---------|
| `--dry-run` | `-n` | 只报告将被删除的对象和 pack 辅助文件，不实际删除 | 关闭 |
| `--prune <DATE>` | | 删除早于 `<DATE>` 的不可达 loose object；支持 `now`、`never`、Unix timestamp、RFC3339 timestamp、`YYYY-MM-DD`，以及 `N.seconds.ago`、`N.minutes.ago`、`N.hours.ago`、`N.days.ago`、`N.weeks.ago`、`N.months.ago` 和 `N.years.ago` | `2.weeks.ago` |
| `--no-prune` | | 禁用删除，只检查可达性和 pack 状态 | 关闭 |
| `--aggressive` | | 为 Git 兼容接受该参数；Libra 当前尚不执行 repack 或 delta compression | 关闭 |
| `--auto` | | 为 Git 兼容接受该参数；Libra 仍执行一次确定性的本地检查 | 关闭 |
| `--force` | | 仅当 `gc.lock` 包含有效 PID 且该进程已不再运行时，替换已有锁文件继续运行 | 关闭 |
| `--json` | | 输出结构化 JSON 信封 | 关闭 |
| `--machine` | | 以一行紧凑 JSON 输出同一信封 | 关闭 |

## 示例

```bash
libra gc
libra gc --dry-run --prune=now
libra gc --prune=now
libra gc --prune=never --json
```

## 人类可读输出

人类可读模式会打印 loose object 摘要和 pack 目录统计：

```text
Enumerating loose objects: 3 scanned, 2 reachable, 1 unreachable.
Expired 1 reflog entry across 2 ref(s).
Pruned 1 loose object(s).
Checked 1 pack(s), containing 42 indexed object(s).
Cleaned 0 stale pack file(s).
```

`--dry-run` 会把删除行切换为 `Would prune` / `Would clean`。`--quiet` 会抑制
stdout，同时保留 stderr 上的错误和警告。

## 结构化输出

使用 `--json` 时，`libra gc` 返回 `gc` 信封，包含：

- `loose_objects.scanned`、`reachable`、`unreachable`、`pruned`、`retained`
- `reflogs.refs_scanned`、`entries_scanned`、`pruned`、`rewritten`
- `reachable_objects`
- `unreachable_objects[]`：对象 ID、类型、动作和原因
- `pack_files.packs_verified`、`objects_in_packs`、`stale_files[]`
- `warnings[]`：兼容参数、陈旧 root、遍历不完整和强制替换锁文件的说明

```json
{
  "ok": true,
  "command": "gc",
  "data": {
    "prune": "now",
    "dry_run": false,
    "loose_objects": {
      "scanned": 3,
      "reachable": 2,
      "unreachable": 1,
      "pruned": 1,
      "retained": 0
    },
    "reflogs": {
      "refs_scanned": 2,
      "entries_scanned": 12,
      "pruned": 1,
      "rewritten": 0
    },
    "reachable_objects": 2,
    "unreachable_objects": [
      {
        "oid": "0123456789abcdef0123456789abcdef01234567",
        "object_type": "blob",
        "action": "pruned",
        "reason": "unreachable loose object matched prune policy"
      }
    ],
    "pack_files": {
      "directory_exists": true,
      "packs_verified": 1,
      "objects_in_packs": 42,
      "stale_files": []
    },
    "warnings": []
  }
}
```

## 兼容性

该命令对齐 Git 的核心安全语义：可达对象会被保留，不可达 loose object 只有在
prune 策略允许时才会删除。在清理对象前，Libra 会执行等价于
`libra reflog expire --all` 的默认策略，但不启用 `--rewrite`、`--updateref`
或 `--stale-fix`。实现范围比 `git gc` 更窄：当前不做完整 repack、bitmap
生成、commit-graph 维护或 cruft-pack 创建。

`.libra/gc.lock` 只互斥并发的 `libra gc` 运行。它不是仓库级写锁：写入新对象或更新
ref 的命令当前不会获取这把锁，因此 `--prune=now` 应在确认没有其它写入者时使用。`--force`
仅在 Libra 能确认锁文件记录的 PID 已不再运行时替换陈旧锁。

| 功能 | Libra | Git | jj |
|---------|-------|-----|----|
| 保留可达对象 | 支持 | 支持 | N/A |
| 清理旧的不可达 loose object | `--prune <date>` | `--prune=<date>` | N/A |
| Dry run | `-n` / `--dry-run` | `--dry-run` | N/A |
| 禁用清理 | `--no-prune` | `--no-prune` | N/A |
| Pack 校验 | 对有效 pack/index 配对复用 `verify-pack` | 维护流程中 repack/verify | N/A |
| GC 锁 | 使用 `.libra/gc.lock` 仅互斥并发 `gc` | 支持 | N/A |
| 重新打包有效对象 | 不支持 | 支持 | N/A |
| Cruft packs | 不支持 | 支持 | N/A |
| Reflog 过期 | 默认 `gc.reflogExpire` / `gc.reflogExpireUnreachable` 策略 | 支持 | N/A |
| JSON 输出 | `--json` / `--machine` | N/A | N/A |

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 不在 Libra 仓库内 | `LBR-REPO-001` | 128 |
| Prune 日期无效 | `LBR-CLI-002` | 129 |
| 无法读取对象存储 | `LBR-IO-001` | 128 |
| 对象目录是 symlink 或不是目录 | `LBR-REPO-002` | 128 |
| 另一个 GC 进程持有 `gc.lock` | `LBR-CONFLICT-002` | 2 |
| 删除对象或 pack 辅助文件失败 | `LBR-IO-002` | 128 |
