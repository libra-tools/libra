# `libra log`

显示提交历史。

**别名：** `hist`, `history`

## 概要

```
libra log [OPTIONS] [<revision-range>] [[--] <path>...]
```

## 说明

`libra log` 从当前 HEAD 开始显示提交历史。它支持多种输出格式，包括 oneline、自定义 pretty-print、图形可视化和结构化 JSON。提交可按作者、日期范围和文件路径过滤。Diff 输出（`--patch`、`--stat`、`--shortstat`、`--name-only`、`--name-status`）可以限制到特定路径。

人类模式保留当前 `--oneline`、`--graph`、`--pretty`、`--stat`、`--patch` 和相关输出样式。`--quiet` 抑制人类输出，但仍验证请求的历史范围。

当 stdout 被管道连接且下游命令提前退出时，`libra log` 会静默正常结束，不打印 panic/backtrace 或 `Broken pipe` 诊断。

## 选项

### `-n, --number <N>`

限制显示的提交数量。

```bash
libra log -n 5
libra log --number 10
```

### `--oneline`

`--pretty=oneline --abbrev-commit` 的简写。以缩写哈希和主题在单行显示每个提交。

```bash
libra log --oneline
```

### `--abbrev-commit`

显示缩写提交哈希，而不是完整 40 字符哈希。

```bash
libra log --abbrev-commit
```

### `--abbrev <LENGTH>`

设置缩写提交哈希长度。

```bash
libra log --abbrev 8
```

### `--no-abbrev-commit`

显示完整提交哈希。覆盖 `--abbrev-commit`。

```bash
libra log --no-abbrev-commit
```

### `--pretty=<format>` / `--format=<format>`

选择提交显示格式。接受命名预设与 `format:`/`tformat:` 自定义模板前缀（及裸 `%` 占位符模板）。`--format` 是 `--pretty` 的 Git 别名。

| 预设 | 输出 |
|---|---|
| `oneline` | 单行 `<hash> <subject>` |
| `medium`（默认） | `commit` + `Author` + `Date` + 缩进消息 |
| `short` | `commit` + `Author` + 缩进 subject（无 date/body） |
| `full` | `commit` + `Author` + `Commit` + 缩进消息（无 date） |
| `fuller` | `commit` + `Author`/`AuthorDate` + `Commit`/`CommitDate` + 消息 |
| `reference` | 单行 `<abbrev> (<subject>, <short-date>)` |
| `raw` | 提交对象的 `tree`/`parent`/`author`/`committer` 头 + 缩进消息 |

预设继承 `libra log` 既有惯例（时间戳渲染为 UTC `+0000`；`--pretty` 缩写哈希；存储消息中 subject/body 间空行已折叠），故与 Git 预设**结构**一致而非逐字节相同。`libra show --pretty=<preset>` 使用相同格式。

自定义模板支持 `%H` / `%h`（完整 / 缩写提交哈希）、`%P` / `%p`（完整 / 缩写父提交哈希列表）、`%s` / `%f`（主题 / 清理后的主题）、`%b` / `%B`（正文 / 原始 subject+body）、`%n`、ASCII/control `%xNN`、`%%`、`%an` / `%ae` / `%ad` / `%aI` / `%at`（作者）、`%cn` / `%ce` / `%cd` / `%cI` / `%ct`（提交者）、`%d` / `%D`（装饰）、`%m`，以及 `%Cred`、`%C(red)`、`%C(always,red)`、`%Creset` 等常见颜色占位符。未知占位符会按 Git pretty-format 规则原样保留。颜色复位遵循 Git 策略：`%C(always,...)` 即使普通颜色关闭也会强制输出 ANSI，而 `%Creset` 只在颜色输出启用时复位；强制颜色模板若也要强制复位，请使用 `%C(always,reset)`。

```bash
libra log --pretty=short
libra log --pretty=fuller
libra log --pretty=reference
```

### `-p, --patch`

显示每个提交的 diff（patch）。可与路径参数组合，将 diff 限制到特定文件。

```bash
libra log -p
libra log -p -- src/main.rs
```

### `--name-only`

只显示每个提交中已更改文件的名称。

```bash
libra log --name-only
```

### `--name-status`

显示每个提交中已更改文件的名称和状态（added/modified/deleted）。

```bash
libra log --name-status
libra log --name-status -- src/
```

### `-z, --null`

使用 NUL 分隔 log 记录和变更路径输出。与 `--name-only` 或
`--name-status` 组合时，格式化后的提交文本以 `NUL` 结束，路径区块按
Git 规则分隔，每个路径/状态字段均以 NUL 终止。

```bash
libra log -z --name-status --format=%s
```

### `--stat`

显示每个提交的 diffstat（文件变更统计），展示每个文件的插入和删除。

### `--shortstat`

只显示 `--stat` 输出的最后一行：每个提交的 ` N files changed, M insertions(+), K deletions(-)`（插入/删除为零时省略对应子句），不含逐文件明细。

### `--patch-with-stat`

Git 中 `-p --stat` 的同义词：先显示 diffstat 块，再显示每个提交的完整 patch。显式的 `-p --stat` 组合等价，同样会同时显示两者。（diffstat 与 patch 块沿用 Libra 既有的 `--stat`/`-p` 渲染，故格式与 Git 略有差异。）

```bash
libra log --stat
libra log --shortstat
libra log --patch-with-stat -1
libra log --range main..feature
libra log --all --oneline
libra log --reverse --oneline
libra log --follow src/main.rs
```

### `--author <PATTERN>`

只显示作者姓名或 email 匹配给定模式的提交。

```bash
libra log --author alice
libra log --author "alice@example.com"
```

### `--grep <PATTERN>` / `-i` / `--invert-grep`

按提交消息过滤。`--grep` 保留消息包含该（大小写敏感）子串的提交；`-i` /
`--regexp-ignore-case` 改为大小写不敏感匹配（author/committer 在 Libra 中本就大小写不敏感）；
`--invert-grep` 保留消息**不**匹配的提交。

```bash
libra log --grep "fix(" -n 20
libra log --grep fix -i              # 大小写不敏感
libra log --grep WIP --invert-grep   # 隐藏 WIP 提交
```

### `--trailer <KEY[=VALUE]>` / `--only-trailers`（Libra 扩展）

Git 无此二 flag（最近等价：过滤用脆弱的 `--grep='^Key: '`，展示用 `--pretty='%(trailers)'`）。`--trailer KEY` 只保留其**合格 trailer 块**（按 Git 规则解析：末段、绝非标题段；key 仅 ASCII 字母数字/连字符；混合块需含 `Signed-off-by` 等可识别 trailer 且 trailer 行 ≥25%）携带该 key 的提交（ASCII 大小写不敏感）；`KEY=VALUE` 另要求展开后的值精确相等；可重复（全部须命中，AND）。`--only-trailers` 把每个提交的消息替换为其 trailer 块（展开的 `Key: value` 行；`(cherry picked from commit …)` 原样），本身不过滤；与 `--trailer` 组合时仅展示所选 key；与 `--oneline`/`--pretty`/`--format` 互斥。`--json` 下每个提交带增量 `trailers` 数组（`[{key,value}]`，无合格块时为空数组）；`body` 不变。

### `--since <DATE>`

显示晚于指定日期的提交。

```bash
libra log --since 2026-01-01
libra log --since "2 weeks ago"
```

### `--until <DATE>`

显示早于指定日期的提交。

```bash
libra log --until 2026-03-01
```

### `--pretty <FORMAT>`

自定义 pretty-print 格式字符串。支持上文列出的同一组占位符，包括 `%b`、`%B`、`%n`、ASCII/control `%xNN`、`%%`、严格 ISO 日期 `%aI` / `%cI`、原始时间戳 `%at` / `%ct`、原始装饰 `%D`、`%m` 和颜色占位符。

```bash
libra log --pretty="%h - %s (%an)"
libra log --pretty="format:%H %s"
libra log --pretty=%P -1
```

### `--format <FORMAT>`

`--pretty=<FORMAT>` 的别名（Git 的 `--format`）。接受与 `--pretty` 相同的预设名和 `%` 占位符模板。与 `--pretty` 互斥。

```bash
libra log --format="%h %s"
libra log --format=oneline
```

### `--decorate[=<style>]`

在提交旁打印 ref 名称（分支、标签）。样式：`short`（默认）、`full`、`no`。

```bash
libra log --decorate
libra log --decorate=full
```

### `--no-decorate`

不打印 ref 名称。覆盖 `--decorate`。

```bash
libra log --no-decorate
```

### `--graph`

绘制基于文本的提交历史图形表示，直观显示分支和合并。

```bash
libra log --graph
libra log --oneline --graph
```

### 修订范围（位置参数或 `--range <SPEC>`）

限定提交历史到某个修订范围。范围可**位置式**给出（Git 风格）或用显式 `--range` 标志。支持形式：
- `A..B` — 从 `B` 可达但不可从 `A` 到达的提交。
- `A...B` — 对称差（在 `A` 或 `B` 中但不在其合并基的提交）。
- `^A`（排除）配合一个 include，例如 `^A B`。
- 单个引用，例如 `main` 或 `HEAD~3`。

位置式下，前导参数按解析结果作为 revision，直到第一个非 revision，其后均作 pathspec，故 `log A..B path/` 把范围限定到改动 `path/` 的提交。既是有效 revision 又是现有 path 的裸名会被判为歧义并报错——用 `--range <rev>` 选定 revision。

```bash
libra log main..feature            # 位置式范围
libra log HEAD~3..HEAD src/        # 位置式范围 + pathspec
libra log ^v1.0 HEAD               # 排除 + include
libra log --range main..feature    # 显式标志形式
```

### `--all`

显示从所有本地分支和标签可达的提交，而不是仅从 HEAD。

```bash
libra log --all
libra log --all --oneline
```

### `--reverse`

按反时间顺序输出提交（最旧在前）。

```bash
libra log --reverse
libra log --reverse --oneline
```

### `--author-date-order`

按作者日期而非提交者日期排序（最新在前）。Libra 仅按时间戳排序，不附加 Git 的拓扑（“父不先于子”）约束。相对 Libra 自身的提交者日期默认顺序，仅当作者日期与提交者日期不一致时才会不同；相对 Git，还会在拓扑约束本应重排提交的地方额外不同。

```bash
libra log --author-date-order
libra log --author-date-order --oneline
```

### `--date-order`

按提交者日期排序（最新在前）。这是 Libra 的默认顺序，故该标志为对齐 Git 而接受、显式选择默认顺序；与 `--author-date-order` 互斥。

```bash
libra log --date-order
libra log --date-order --oneline
```

### `--no-expand-tabs`

不在日志消息中展开 tab。为对齐 Git 而接受的 no-op：Libra 从不展开提交消息中的 tab（逐字打印），故该标志已是默认行为。（Git 的反向标志 `--expand-tabs[=<n>]` 未实现。）

```bash
libra log --no-expand-tabs
```

### `--no-notes`

不显示提交 notes。为对齐 Git 而接受的 no-op：Libra 的 log 从不内联显示 notes，故该标志已是默认行为。（Git 的反向标志 `--notes[=<ref>]` 未实现；读 note 用 `libra notes show <commit>`。）

```bash
libra log --no-notes
```

### `--no-mailmap`

不使用 `.mailmap` 重写 author/committer 身份。为对齐 Git 而接受的 no-op：Libra 的 log 从不应用 mailmap，直接显示记录的原始身份。（Git 的反向标志 `--mailmap` 未实现。）

```bash
libra log --no-mailmap
```

### `--no-show-signature`

不显示已签名提交的 GPG 签名。为对齐 Git 而接受的 no-op：Libra 的 log 从不内联显示提交签名，故已是默认行为。（Git 的反向标志 `--show-signature` 未实现。）

```bash
libra log --no-show-signature
```

### `--follow <FILE>`

Best-effort 跨重命名追踪单个文件历史。文件路径相对于当前目录解析。

```bash
libra log --follow src/main.rs
```

### `--parents` / `--children`

在每个提交哈希后追加提交 id。`--parents` 显示每个提交的父 id；`--children` 显示*本次 log 输出中*以它为父的其他提交 id（子映射在已渲染的提交集合上构建，因此不会列出展示范围之外的子提交）。这些 id 使用与提交哈希相同的缩写，出现在 full 和 oneline 格式中。两者互斥。

```bash
libra log --oneline --parents
libra log --children
```

### `-L <RANGE:FILE>`

接受 Git 风格行范围语法。完整的 blame 级行归属尚未实现；当前版本中该标志作为路径过滤解析。

```bash
libra log -L1,10:src/main.rs
```

### `[PATHS...]`

将 diff 输出限制到指定路径。与 `-p`、`--name-only`、`--name-status`、`--stat` 或 `--shortstat` 一起使用。

```bash
libra log -- src/
libra log -p -- src/main.rs tests/
```

## 常用命令

```bash
libra log
libra log -n 5
libra log --oneline --graph
libra log --author alice --since 2026-01-01
libra log --name-status src/
libra --json log -n 1
```

## 人类可读输出

默认人类模式以详细多行格式显示提交：

```text
commit abc1234def5678901234567890abcdef12345678 (HEAD -> main, origin/main)
Author: Test User <test@example.com>
Date:   Sat Mar 30 10:00:00 2026 +0800

    Add new feature
```

Oneline 格式：

```text
abc1234 (HEAD -> main) Add new feature
def5678 Fix bug in parser
```

Graph 格式：

```text
* abc1234 (HEAD -> main) Add new feature
* def5678 Fix bug in parser
|\ 
| * 1234567 Feature branch commit
|/
* 7890abc Initial commit
```

`--quiet` 会抑制所有人类输出。

## 结构化输出

`--json` / `--machine` 返回经过过滤的结构化提交列表：

```json
{
  "ok": true,
  "command": "log",
  "data": {
    "commits": [
      {
        "hash": "abc123...",
        "short_hash": "abc1234",
        "author_name": "Test User",
        "author_email": "test@example.com",
        "author_date": "2026-03-30T10:00:00+08:00",
        "committer_name": "Test User",
        "committer_email": "test@example.com",
        "committer_date": "2026-03-30T10:00:00+08:00",
        "subject": "base",
        "body": "",
        "parents": [],
        "refs": ["HEAD -> main"],
        "files": [
          { "path": "tracked.txt", "status": "added" }
        ]
      }
    ],
    "total": 1
  }
}
```

### Schema 说明

- `-n` 也适用于 JSON 模式
- 仅在未提供 `-n` 时，`total` 反映过滤后的提交数量；使用 `-n` 时始终为 `null`
- `--graph`、`--pretty` 和 `--oneline` 不改变 JSON schema
- `--decorate` 只影响人类渲染；JSON 始终返回 `refs` 数组，辅助 ref 元数据以 best-effort 收集
- `files` 始终是结构化变更摘要，永远不包含 patch 文本

## 设计理由

### 位置式修订范围与 `--range` 备选

Git 接受 `git log A..B` 这种位置修订表达式（其后可跟 pathspec）。Libra 已支持该位置形式：前导参数按解析结果分流为 revision，直到第一个非 revision，其后为 pathspec。由于 Libra 看不到 `--` 分隔符（在命令前已被消费），分流按解析进行：range 语法 token（`A..B`/`A...B`/`^A`）能解析时为 revision，不能解析但命中现有 path（如 `../file`）时为 pathspec，否则报错（把拼错的 revision 报为未知 revision/path，而非按不存在的 path 静默过滤）。裸 token 仅在能解析为提交时才是 revision；既是 revision 又是现有 path 的裸名会报歧义。显式 `--range A..B` 标志作为无歧义备选保留，也是强制把与 path 同名的名字当作 revision 的方式。

### `--all` 实现

`--all` 枚举 SQLite `reference` 表中的本地分支和轻量标签，收集其 tip 提交，并遍历这些历史的并集。

### `--reverse`

`--reverse` 收集过滤后的提交并按最旧优先打印。它应用在所有其他过滤之后，因此 `-n` 仍限制结果集大小。

### `--author-date-order`

`--author-date-order` 按作者时间戳（最新在前）排序结果集，而非默认的提交者时间戳。排序仅按时间戳——Libra 不施加 Git 的拓扑约束——故仅当某提交的作者日期与提交者日期不同（如 rebase 或 cherry-pick 后）时才与默认不同。`--reverse` 仍会翻转最终顺序。

### `--date-order`

`--date-order` 显式选择默认的提交者时间戳顺序。它是接受式 no-op（Libra 本就按提交者日期排序），与 `--author-date-order` 互斥。与 Libra 其它排序标志一样，排序仅按时间戳（无拓扑约束）。

### `--follow`

`--follow` 通过遍历历史并匹配被移除/新增 blob 哈希来进行 best-effort 重命名检测。它不能处理复杂目录重命名或内容相似重命名。

### `-L`

`-L` 已被解析和接受；完整的 blame 级行归属尚未实现。当前版本中该标志作为路径过滤。

### 文本渲染的 `--graph`

Libra 将 `--graph` 实现为基于文本的 ASCII/Unicode 图渲染器，类似 Git 内置 graph 输出。与 GUI 工具（GitKraken、SourceTree）或带外部 graph renderer 的 Git `--format` 不同，Libra 的图直接在终端内渲染。这让 CLI 自包含，并确保跨平台输出一致。Graph renderer 处理分支、合并和 octopus merges，绘制父子提交之间的连接线。

### JSON 始终返回 `refs` 数组，不受 `--decorate` 影响

在人类输出中，`--decorate` 控制是否在提交哈希旁显示 ref 名称（分支、标签）。在 JSON 模式中，无论 `--decorate` 标志如何，`refs` 数组总是填充。这一设计选择体现了 JSON 输出应为程序消费者提供最大信息量的原则。解析 JSON 输出的 AI 代理或 CI 工具不应需要记得传 `--decorate` 才能获得 ref 信息。`--decorate` 标志只影响人类渲染层。

## 参数对比：Libra vs Git vs jj

| 参数 / 标志 | Git | jj | Libra |
|---|---|---|---|
| 显示 log | `git log` | `jj log` | `libra log` |
| 限制数量 | `git log -n <N>` | `jj log -n <N>` | `libra log -n <N>` |
| Oneline 格式 | `git log --oneline` | 默认格式为 oneline | `libra log --oneline` |
| 缩写哈希 | `git log --abbrev-commit` | 默认 | `libra log --abbrev-commit` |
| 缩写长度 | `git log --abbrev=<N>` | N/A | `libra log --abbrev <N>` |
| 完整哈希 | `git log --no-abbrev-commit` | `jj log --no-short-hash` | `libra log --no-abbrev-commit` |
| 显示 patch | `git log -p` | `jj diff -r <rev>`（单独命令） | `libra log -p` / `--patch` |
| 仅名称 | `git log --name-only` | N/A | `libra log --name-only` |
| 名称和状态 | `git log --name-status` | N/A | `libra log --name-status` |
| NUL 路径输出 | `git log -z --name-status` | N/A | `libra log -z --name-status` |
| Diffstat | `git log --stat` | `jj diff --stat -r <rev>` | `libra log --stat` |
| 简短 diffstat | `git log --shortstat` | 无 | `libra log --shortstat` |
| 按作者过滤 | `git log --author=<pat>` | `jj log --author <pat>`（revset） | `libra log --author <pat>` |
| Since 日期 | `git log --since=<date>` | Revset 表达式 | `libra log --since <date>` |
| Until 日期 | `git log --until=<date>` | Revset 表达式 | `libra log --until <date>` |
| 自定义格式 | `git log --pretty=<fmt>` / `--format=<fmt>` | `jj log -T <template>` | `libra log --pretty <fmt>` / `--format <fmt>` |
| Decorate refs | `git log --decorate` | 始终显示 | `libra log --decorate` |
| 不 decorate | `git log --no-decorate` | N/A | `libra log --no-decorate` |
| Graph 视图 | `git log --graph` | `jj log`（默认有 graph） | `libra log --graph` |
| 所有 refs | `git log --all` | `jj log -r 'all()'` | `libra log --all` |
| 仅分支 | `git log --branches` | `jj log -r 'branches()'` | N/A |
| 仅远程 | `git log --remotes` | `jj log -r 'remote_branches()'` | N/A |
| 修订范围 | `git log A..B` | `jj log -r 'A..B'` | `libra log A..B`（位置式）或 `libra log --range A..B` |
| Grep 消息 | `git log --grep=<pat>` | Revset `description()` | `libra log --grep <pat>` |
| 大小写不敏感 grep | `git log -i --grep=<pat>` | N/A | `libra log -i --grep <pat>` |
| 反向 grep | `git log --invert-grep --grep=<pat>` | N/A | `libra log --invert-grep --grep <pat>` |
| 路径过滤 | `git log -- <paths>` | N/A（使用 revset） | `libra log -- <paths>` |
| 反向顺序 | `git log --reverse` | `jj log --reversed` | `libra log --reverse` |
| 作者日期顺序 | `git log --author-date-order` | N/A | `libra log --author-date-order`（仅时间戳） |
| 日期顺序 | `git log --date-order` | N/A | `libra log --date-order`（接受式 no-op；默认） |
| 追踪重命名 | `git log --follow <file>` | N/A | `libra log --follow <file>` |
| 仅 merge commits | `git log --merges` | N/A | N/A |
| 仅 first parent | `git log --first-parent` | N/A | `libra log --first-parent` |
| 结构化 JSON 输出 | N/A | N/A | `--json` / `--machine` |
| 错误提示 | 最少 | 最少 | 每种错误类型都有可操作提示 |

## 错误处理

| 场景 | 错误码 | 退出码 | 提示 |
|----------|-----------|------|------|
| 仓库外部 | `LBR-REPO-001` | 128 | -- |
| 空分支或空 HEAD | `LBR-REPO-003` | 128 | "create a commit first before running 'libra log'" |
| 无效日期参数 | `LBR-CLI-002` | 129 | -- |
| 无效 `--decorate` 选项 | `LBR-CLI-002` | 129 | -- |
| 无效对象名 | `LBR-CLI-003` | 129 | "check the revision name and try again" |
| 损坏的 commit/tree/blob | `LBR-REPO-002` | 128 | -- |
| 无法读取历史对象 | `LBR-REPO-002` | 128 | -- |

## 兼容性说明

- `--branches` 和 `--remotes` 尚未实现
- `--all` 遍历本地分支和轻量标签；远程跟踪引用和 stash 不包含在内
- 修订范围语法支持位置式 `git log A..B`/`A...B`/`^A`（以及显式 `--range A..B`/`A...B`）
- `--follow` 使用 best-effort 重命名检测，可能遗漏复杂重命名
- `-L` 已被接受，但尚未提供 blame 级行精度
- `--reverse` 已支持
- `--author-date-order` 已支持（仅时间戳；无拓扑约束）
- `--date-order` 已支持（接受式 no-op；显式选择默认的提交者日期顺序）
- jj 的 log 使用模板语言（`-T`）进行格式化；Libra 使用 Git 兼容的 `--pretty` 格式字符串
- 在 JSON 模式中，`files` 包含结构化变更摘要；JSON 输出永远不包含 patch 文本
