# `libra show`

显示提交、标签、树、blob，或 `REV:path` 引用的 blob。

## 概要

```
libra show [OPTIONS] [OBJECT] [-- <PATHS>...]
```

## 说明

`libra show` 解析单个对象引用并渲染其内容。默认目标是 `HEAD`。它理解 commit-ish 引用（`HEAD~2`、分支名、标签名）、原始 SHA-1 哈希，以及用于从给定修订的树中提取特定 blob 的 `REV:path` 语法。

对于提交，输出包含头部（作者、提交者、日期、消息），随后是 unified diff（“patch”）。`--no-patch`、`--stat` 和 `--name-only` 等标志控制显示多少 diff 上下文。对于附注标签，会先打印 tagger 元数据和消息，然后打印目标对象。树会列出其条目，blob 会打印文本内容（或二进制摘要）。

当 stdout 被管道连接且下游命令提前退出时，`libra show` 会静默正常结束，不打印 panic/backtrace 或 `Broken pipe` 诊断。

## 选项

| 标志 | 短选项 | 说明 |
|------|-------|-------------|
| `<OBJECT>` | | 对象名（提交、标签、树、blob）或 `<object>:<path>`。默认为 `HEAD`。 |
| `--no-patch` | `-s` | 跳过 patch 输出，只显示对象元数据。 |
| `--oneline` | | `--pretty=oneline` 的简写，在一行中打印哈希和主题。 |
| `--pretty <FORMAT>` | | 以预设（`oneline`、`short`、`full`、`fuller`、`reference`、`raw`）或 `%` 占位符模板（`format:`/`tformat:`/裸模板）格式化 commit header。复用 `libra log --format` 的同一组自定义占位符，包括 `%b`、`%B`、`%n`、ASCII/control `%xNN`、`%%`、`%aI`、`%cI`、`%at`、`%ct`、`%D`、`%m` 和颜色占位符。 |
| `--format <FORMAT>` | | `--pretty=<FORMAT>` 的别名（Git 的 `--format`）。与 `--pretty` 互斥。 |
| `--abbrev-commit` | | 把默认 header 的 commit 对象名缩写为 7 位前缀。 |
| `--no-abbrev-commit` | | 显示完整（未缩写）commit 对象名，撤销先前的 `--abbrev-commit`（最后出现者生效）。完整哈希是默认，故单独使用时为 no-op。 |
| `--name-only` | | 只显示已更改文件名（没有 diff hunk）。 |
| `--raw` | | 以原始 diff 格式 `:<old-mode> <new-mode> <old-sha> <new-sha> <status>\t<path>`（对象 id 缩写 7 位）显示而非 patch，类似 `git show --raw`。 |
| `--stat` | | 显示 diff 统计（每个文件的插入 / 删除）。 |
| `--patch-with-stat` | | 先显示 diffstat 块，再显示完整 patch（Git 对 `-p --stat` 的旧式同义词）。 |
| `--summary` | | 显示创建/删除文件的精简摘要（mode 与路径），类似 `git show --summary`。仅含创建/删除文件，不做 rename/copy 检测。 |
| `--no-expand-tabs` | | 不在提交消息中展开 tab。接受式 no-op：Libra 的 show 逐字打印 tab。 |
| `--no-notes` | | 不显示提交 notes。接受式 no-op：Libra 的 show 从不内联显示 notes。 |
| `--no-mailmap` | | 不应用 `.mailmap`。接受式 no-op：Libra 的 show 显示记录的原始身份。 |
| `--no-show-signature` | | 不显示已签名提交的 GPG 签名。接受式 no-op：Libra 的 show 从不内联显示提交签名。（Git 的 `--show-signature` 未实现。） |
| `<PATHS>...` | | 将输出限制为匹配路径（提交 diff 的 pathspec 过滤器）。 |

### 示例

```bash
# 显示最新提交及完整 patch
libra show HEAD

# 只显示标签元数据（无 diff）
libra show --no-patch v1.0.0

# 显示某个修订中的特定文件
libra show HEAD:src/main.rs

# 提交的一行摘要
libra show --oneline abc1234

# 仅 diff 统计
libra show --stat HEAD~1

# 将 diff 限制到子目录
libra show HEAD -- src/command/
```

## 常用命令

```bash
libra show                          # 显示 HEAD 提交和 patch
libra show HEAD~3                   # 显示祖先提交
libra show -s v2.0.0                # 只显示标签元数据
libra show HEAD:Cargo.toml          # 打印 HEAD 中的文件
libra show --name-only HEAD         # 列出已更改文件
libra show --stat HEAD              # diff 统计
libra show --patch-with-stat HEAD   # 先 diffstat 再完整 patch
libra show --summary HEAD           # 创建/删除文件 mode 摘要
libra --json show HEAD              # 结构化 JSON 输出
```

## 人类可读输出

人类模式保留现有呈现：

- 提交：头部加可选 patch / stat / name-only 输出
- 附注标签：标签元数据，随后是目标对象
- 树：树条目列表
- Blob：文本内容或二进制摘要
- `--quiet`：验证对象引用但抑制人类输出
- 人类输出使用共享分页器策略；传递全局 `--no-pager` 强制直接 stdout

## 结构化输出（JSON 示例）

`data.type` 决定 schema。可能值：`commit`、`tag`、`tree`、`blob`。

### Commit

```json
{
  "ok": true,
  "command": "show",
  "data": {
    "type": "commit",
    "hash": "abc1234def5678901234567890abcdef12345678",
    "short_hash": "abc1234",
    "author_name": "Alice",
    "author_email": "alice@example.com",
    "author_date": "2026-04-01T10:00:00+00:00",
    "committer_name": "Alice",
    "committer_email": "alice@example.com",
    "committer_date": "2026-04-01T10:00:00+00:00",
    "subject": "feat: add new feature",
    "body": "",
    "parents": ["def456..."],
    "refs": ["HEAD -> main"],
    "files": [
      { "path": "tracked.txt", "status": "added" }
    ]
  }
}
```

### Tag

```json
{
  "ok": true,
  "command": "show",
  "data": {
    "type": "tag",
    "tag_name": "v1.0.0",
    "tagger_name": "Alice",
    "tagger_email": "alice@example.com",
    "tagger_date": "2026-04-01T10:00:00+00:00",
    "message": "Release v1.0.0",
    "target_hash": "abc1234def5678901234567890abcdef12345678",
    "target_type": "commit"
  }
}
```

### Tree

```json
{
  "ok": true,
  "command": "show",
  "data": {
    "type": "tree",
    "entries": [
      { "mode": "100644", "object_type": "blob", "hash": "abc123...", "name": "README.md" },
      { "mode": "040000", "object_type": "tree", "hash": "def456...", "name": "src" }
    ]
  }
}
```

### Blob

```json
{
  "ok": true,
  "command": "show",
  "data": {
    "type": "blob",
    "hash": "abc123...",
    "size": 1024,
    "is_binary": false,
    "content": "fn main() { ... }"
  }
}
```

说明：

- Commit JSON `refs` 是 best-effort 装饰元数据；无关分支/标签行不再阻塞 `show`
- 人类 `--quiet` 仍会验证目标对象，但会抑制 stdout，并且不会初始化分页器
- Commit patch / stat 路径保持严格：损坏的历史 blob 会以 `LBR-REPO-002` 失败，而不是回退到工作树内容

## 设计理由

### 为什么支持 `REV:path` 语法？

`REV:path` 记法（例如 `HEAD:src/main.rs`）是 Git 中最有用的惯用法之一，因为它允许用户和工具在历史中的任意时间点检索任意文件，而无需 checkout 整个提交。对 AI 代理来说这尤其有价值：代理可以读取特定修订上的特定文件，以跨分支或时间比较实现，而不修改工作树。Libra 保留此语法以实现完整 Git 兼容性，也因为它自然映射到 Libra 已执行的内部 tree-walk 操作。

### `--pretty` / `--format` 与结构化 JSON

`--pretty=<fmt>` 及其别名 `--format=<fmt>` 以 `oneline` 预设或 `%` 占位符模板（`format:`/`tformat:`/裸模板）渲染 commit header，复用 `libra log` 的 formatter。命名预设 `short` / `full` / `fuller` / `reference` / `raw` 已单独渲染（结构对齐 Git 预设），`medium` 映射默认格式（这与 `--raw` diff 格式不同——见 `--raw` 选项，它选择原始 `:<old-mode> <new-mode> …` diff 格式而非预设）。对程序消费者，仍推荐 `--json`：它在类型良好、按类型判别的 schema 中提供每个字段（类型化字段而非字符串解析），避免格式字符串的脆弱性。

### 为什么使用类型感知 JSON schema？

`data.type` 判别器（`commit`、`tag`、`tree`、`blob`）意味着 JSON 消费者可以按类型 switch，并只访问该对象类型存在的字段。这比包含许多 nullable 字段的扁平 schema 更符合人体工程学，也镜像 Git 自身的对象模型。每个变体只携带有意义的字段（例如 `tagger_name` 只出现在标签中，`parents` 只出现在提交中），从而消除代理工具中“字段为 null 但我预期它存在”这一类 bug。

## 参数对比：Libra vs Git vs jj

| 功能 | Libra | Git | jj |
|---------|-------|-----|----|
| 默认目标 | `HEAD` | `HEAD` | N/A（`jj show` 已移除；使用 `jj log -r @`） |
| `REV:path` 语法 | 是 | 是 | 否（使用 `jj file show -r REV path`） |
| `--no-patch` / `-s` | 是 | 是 | N/A |
| `--oneline` | 是 | 是 | N/A（使用 `jj log --template`） |
| `--name-only` | 是 | 是 | N/A |
| `--stat` | 是 | 是 | N/A（`jj diff --stat -r REV`） |
| `--pretty` / `--format` | 是（`oneline` + `%` 模板；预设待补） | 是 | 否（使用模板） |
| `--abbrev-commit` | 是 | 是 | N/A |
| `--quiet` | 是（仅验证） | 否 | N/A |
| JSON 输出 | `--json`，带类型 schema | 无 | 无 |
| Pathspec 过滤 | 是（尾随 `<PATHS>...`） | 是 | 否（使用 `jj diff --from/--to`） |
| 感知标签的显示 | 自动检测附注标签 | 自动检测附注标签 | 无标签对象 |

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 仓库外部 | `LBR-REPO-001` | 128 |
| 无效修订或缺失路径 | `LBR-CLI-003` | 129 |
| 无法读取对象 | `LBR-REPO-002` | 128 |
