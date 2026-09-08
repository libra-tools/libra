# `libra memory`

搜索和诊断当前仓库的研发历程记忆。这些记忆由 Libra 已有的 Intent、Task、
Run、会话、决策、证据和代码版本记录自动整理而来。

## 用法

```bash
libra memory search <query> [过滤条件] [--limit <n>]
libra memory show <note-id> [--revision <oid>] [--evidence]
libra memory status
libra memory rebuild [--dry-run]
```

## 它读取的内容

每条结果都是一条“研发历程摘要（Episode）”，描述一次 Task 或 Intent 迭代中
发生了什么、结果如何、涉及哪个代码版本，以及结论来自哪些证据。原始会话和
工具输出仍保存在原来的历史中，只有执行 `show --evidence` 时才按引用展开。

`search`、`show`、`status` 和 `rebuild --dry-run` 都是只读操作。普通
`rebuild` 只会重建 SQLite 中可恢复的 Memory 读取投影，不会移动 Memory
权威分支，也不会修改权威对象。

## 搜索

搜索先使用 SQLite FTS5 与 `bm25()` 排序，再应用结构化过滤和代码版本适用性
判断。

| 参数 | 含义 |
| --- | --- |
| `--limit <1..50>` | 最多返回多少条，默认 `10` |
| `--root-kind task\|intent --root-id <id>` | 指定一个根对象，两个参数必须一起使用 |
| `--intent <id>` | 只看与某个 Intent 相关的结果 |
| `--task <id>` | 只看与某个 Task 相关的结果 |
| `--ended-from <RFC3339>` / `--ended-until <RFC3339>` | 按结束时间过滤 |
| `--completion completed\|failed\|cancelled` | 按研发结果过滤 |
| `--code-change changed\|unchanged\|unknown` | 按是否修改代码过滤 |
| `--path <path>` | 精确匹配一个 Memory 分类路径 |
| `--path-prefix <prefix>` | 匹配一组路径前缀 |
| `--include-diagnostics` | 同时显示代码已变化、分叉或无法判断的结果，便于诊断 |

默认的人类可读结果会显示稳定 note ID、精确 revision OID、Task/Intent 根、
研发结果、代码变化、当前代码版本适用性、证据引用数量、BM25 分数和摘要。正常
搜索只返回可以安全注入当前 Agent 上下文的结果。

## 查看单条记忆

`show` 默认读取当前已确认版本；`--revision` 可以固定查看历史版本。
`--evidence` 会先用当前仓库的已认证身份逐条检查证据引用，再按编译时相同的
大小限制和脱敏规则展开。找不到、无权读取、内容损坏或超过预算的证据会明确列为
omission，不会被静默替换。人类可读输出会紧凑展示根对象和结果、时间范围、目标、
摘要、分类结论、代码锚点与路径，以及证据数量。

## 状态诊断

`status` 会显示：

- 仓库 Memory ref；
- 投影状态、已投影 ref 和最后事件序号；
- 自动编译作业状态、待处理 generation、有效/过期 lease、重试和错误计数；
- 当前 SQLite 是否支持 FTS5；
- 仓库摘要密钥是否可用，以及当前冻结读取视图的 hash。

它不会输出 Episode 正文、模型提示词、作业 lease token 或原始证据。
该命令检查当前 head manifest 和 SQLite 水位，并最多扫描 4,096 条自动编译作业。
JSON 中的 `jobs.scan_limit` 表示扫描上限；`jobs.truncated` 为 `true` 时，各项作业
计数只代表这段有限样本。需要完整校验权威历史时，请使用 `rebuild --dry-run`。

## 重建

`rebuild --dry-run` 会完整校验权威历史，并报告 head、事件数、note 数、revision
数和最后事件序号，全程不写 SQLite。`rebuild` 会从同一份权威历史恢复当前仓库
的投影和 FTS 索引。`status` 显示 `stale`，或者投影表被删除、损坏时，可以使用
它恢复。校验遇到损坏历史时，错误只报告 Memory head OID 或事件序号/对象 OID
等有限定位信息，不输出 note 正文或证据内容。

## JSON 输出

使用全局 `--json` 或 `--machine`。四个 envelope command 分别为
`memory.search`、`memory.show`、`memory.status` 和 `memory.rebuild`。
空搜索正常返回 `items: []`。非法过滤、note 不存在、FTS5 不可用、投影过期、
未知 schema 和历史损坏都会返回稳定的 `LBR-MEMORY-*` 错误码。结构化损坏错误
还会把同一定位信息放在 `details.damage_point`。

读取命令和 `rebuild --dry-run` 不会迁移 SQLite。普通的
`libra memory rebuild` 会先应用当前版本已知的待执行迁移，再重放投影。如果仓库
schema 比已安装的 Libra 更新，命令返回 `LBR-MEMORY-002`，需要升级 Libra 后重试。

## 示例

```bash
libra memory search "authentication retry"
libra memory search "timeout" --task task-42 --limit 5
libra memory search "parser" --path-prefix episodic.tasks
libra --json memory search "root cause"
libra memory show <note-id>
libra memory show <note-id> --revision <oid> --evidence
libra memory status
libra memory rebuild --dry-run
libra memory rebuild
```

## 当前范围

当前命令只处理仓库本地的 Task/Intent 研发历程摘要。人工 remember/delete/update、
Memory revert、合并整理、MCP 工具、团队同步和跨仓库搜索不在这组命令中。
