# `libra gc`

清理仓库对象与引用（`git gc`）。

## 概要

```bash
libra gc [--prune=<date>] [--no-prune] [--aggressive] [--auto]
```

## 说明

`libra gc` 复用既有维护原语，按 Git `gc` 的顺序依次执行维护步骤，不另写可达性遍历：

1. **`pack-refs`** —— 把松散引用合并进 `packed-refs`；
2. **reflog 过期** —— 按默认 `gc.reflogExpire` / `gc.reflogExpireUnreachable`
   策略（90/30 天默认）过期 reflog 条目；
3. **`repack -a -d`** —— 把全部可达对象打包进一个新 pack，并删除已入包的
   松散副本；
4. **不可达松散对象删除** —— 按 prune 过期时间删除不可达松散对象，复用共享
   维护可达性清单（refs、reflog、全部 index stage、stash/autostash sidecar、
   sequencer 状态）与 alternates 借用保护；
5. **`worktree prune`** —— 清理路径已消失的 worktree 注册表条目。

可达对象绝不会被删除。删除步骤在其它仓库通过 alternates 借用本对象库时拒绝
执行（被借用的对象对借用方可达，但不在本仓库遍历之内），并在真正删除前于
独占维护锁下再次复核可达性。

人类输出为每个已执行步骤打印一行 `gc <步骤>:`；`--json`/`--machine` 输出
`gc` envelope，其 `data.steps[]` 汇总每个步骤（task、success，以及
`objects_removed`、`refs_packed`、`reflog_entries_pruned` 等按步骤计数）。

## 选项

`--auto` 只在值得维护时运行：`gc.auto <= 0` 从不触发；否则当**不带 `.keep` 的 pack 数**超过 `gc.autoPackLimit`（默认 50，非正值关闭该触发条件）或松散对象数达到 `gc.auto`（默认 6700）时运行。触发时向 stderr 输出 `Auto packing the repository for optimum performance.`。该判定与 `fetch` 的自动打包共用同一函数。

### `--prune <DATE>`

删除早于 `<DATE>` 的不可达松散对象。缺省取 `gc.pruneExpire` 配置，回退到
`2.weeks.ago`。支持 `now`（立即删除）、`never`（禁用删除）、裸整数（天数）、
Git 的点分 approxidate 形式（`N.seconds.ago`、`N.minutes.ago`、`N.hours.ago`、
`N.days.ago`、`N.weeks.ago`、`N.months.ago`、`N.years.ago`）以及共享日期解析器
的绝对形式（Unix 时间戳、RFC3339、`YYYY-MM-DD`）。非法值以 `LBR-CLI-002` /
exit 129 失败，且在任一步骤写入之前。

```bash
libra gc --prune=now
libra gc --prune=3.days.ago
```

### `--no-prune`

不删除任何不可达松散对象；其余步骤照常执行。

```bash
libra gc --no-prune
```

### `--aggressive`

花费更多时间优化 pack（更大的 delta 压缩窗口）。加大窗口
（`gc.aggressiveWindow`/`gc.aggressiveDepth`，Git 默认 250/50）随
`repack --window`/`--depth`（MX-09）落地；此前该标志被接受并在 `--json`
envelope 中报告（`data.aggressive`），但不改变 pack 参数。

```bash
libra gc --aggressive
```

### `--auto`

仅当松散对象数不少于 `gc.auto`（默认 6700）时执行。低于阈值时**零写入**、
exit 0，并输出 `Auto gc skipped`。非正 `gc.auto` 完全禁用 auto-gc，与 Git 的
`too_many_loose_objects` 一致。

```bash
libra gc --auto
libra config set gc.auto 2000 && libra gc --auto
```

## Examples

```bash
libra gc                              # 默认策略：删除早于 2.weeks.ago 的不可达对象
libra gc --prune=now                  # 立即删除不可达松散对象
libra gc --no-prune                   # 清理但不删除任何对象
libra gc --auto                       # 低于 gc.auto（6700）时为 no-op
libra gc --aggressive                 # 优化 pack（已报告；窗口随 MX-09 落地）
libra gc --quiet                      # 不输出步骤汇总
libra --json gc                       # 结构化步骤汇总
```

## 人类输出

```text
gc pack-refs: packed 2 refs into packed-refs
gc reflog-expire: expired 0 reflog entries across 4 ref(s)
gc repack: repacked all reachable objects and removed the now-packed loose copies
gc prune: removed 1 unreachable loose objects
gc worktree-prune: pruned worktree registry entries whose paths vanished
```

`--quiet` 抑制汇总输出，警告与错误仍保留在 stderr。

## 结构化输出

```json
{
  "ok": true,
  "command": "gc",
  "data": {
    "auto": false,
    "auto_skipped": false,
    "aggressive": false,
    "prune": "2.weeks.ago",
    "steps": [
      { "task": "pack-refs", "success": true, "refs_packed": 2, "message": "..." },
      { "task": "reflog-expire", "success": true, "reflog_entries_pruned": 0, "message": "..." },
      { "task": "repack", "success": true, "message": "..." },
      { "task": "prune", "success": true, "objects_removed": 1, "message": "..." },
      { "task": "worktree-prune", "success": true, "message": "..." }
    ]
  }
}
```

被跳过的 `--auto` 运行报告 `auto_skipped: true` 且 `steps[]` 为空。

## 兼容性

| 特性 | Libra | Git |
|------|-------|-----|
| 保留可达对象 | 支持（共享维护可达性清单） | 支持 |
| pack-refs + reflog 过期 + worktree prune | 支持（顺序与 `git gc` 相同） | 支持 |
| 重新打包可达松散对象（`repack -a -d`） | 支持（单个未 deltify pack；冗余 *pack* 删除随 MX-08 落地） | 支持 |
| 删除过期不可达松散对象 | `--prune=<date>` / `--no-prune`（默认 `gc.pruneExpire` = `2.weeks.ago`） | `--prune=<date>` / `--no-prune` |
| 自动模式 | `--auto`（低于 `gc.auto` = 6700 时零写入；`gc.auto <= 0` 禁用） | `--auto` |
| 激进重新打包 | 接受并报告；加大窗口随 MX-09 落地 | 更大 delta 窗口 |
| Alternates 借用保护 | 有活跃借用方时删除步骤拒绝（`LBR-CONFLICT-002`） | 无（Libra 扩展） |
| Cruft pack / commit-graph / bitmap | 不支持 | 支持 |
| JSON 输出 | `--json` / `--machine` 步骤汇总 | 无 |

## 错误处理

| 场景 | StableErrorCode | Exit |
|------|-----------------|------|
| 不在 Libra 仓库内 | `LBR-REPO-001` | 128 |
| 非法 prune 日期 | `LBR-CLI-002` | 129 |
| `--prune` 与 `--no-prune` 同时给出 | `LBR-CLI-002` | 129 |
| 有活跃仓库借用本对象库（alternates） | `LBR-CONFLICT-002` | 128 |
| 对象存储不可读 | `LBR-IO-001` | 128 |
| 对象删除失败 | `LBR-IO-002`（按消息推断） | 128 |
