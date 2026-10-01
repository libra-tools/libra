# `libra reflog`

管理引用变更日志（HEAD、分支）。

## 概要

```
libra reflog show [<ref_name>] [--pretty <format>] [--since <date>] [--until <date>] [--grep <pattern>] [--author <pattern>] [-n <N>] [-p/--patch] [--stat] [--no-abbrev]
libra reflog delete <selector>...
libra reflog exists <ref_name>
```

## 说明

`libra reflog` 记录并显示仓库中引用变更的历史。接入 Libra reflog 集成的命令（commit、switch、merge、rebase、reset、fetch、pull、push、clone 等）会创建条目，包含旧对象 ID、新对象 ID、时间戳、committer 身份，以及动作描述。

Reflog 条目存储在 SQLite `reflog` 表中，提供事务安全性和可查询历史。这与 Git 的扁平文件方式形成对比，Git 中每个 ref 在 `.git/logs/` 下都有单独 reflog 文件。

`show` 子命令是检查 reflog 历史的主要接口，支持按时间范围、消息内容和作者过滤。`delete` 子命令移除特定条目，`exists` 是供脚本检查某个引用是否有 reflog 条目的 plumbing 命令。

裸 `libra reflog`、`reflog show`、`reflog exists` 以及 `reflog expire --dry-run` 不会写入 Operation v2 记录。`reflog delete` 以及不带 `--dry-run` 的 `reflog expire` 仍各记录一笔（`libra op log --command reflog`）。不带 ref 或 `--all` 的裸 `reflog expire --dry-run` 仍是用法错误。

## 选项

### 子命令：`show`

显示某个引用的 reflog 条目。

| 选项 / 参数 | 短选项 | 长选项 | 说明 |
|-------------------|-------|------|-------------|
| `<ref_name>` | | | 要显示的引用。默认为 `HEAD`。裸分支名会展开为 `refs/heads/<name>`；包含 `/` 的名称会与已配置远程检查，如果存在匹配远程，则展开为 `refs/remotes/<name>`。 |
| Pretty format | | `--pretty` | 输出格式。可选：`oneline`（默认）、`short`、`medium`、`full`。 |
| Since | | `--since` | 显示晚于给定日期的条目。接受人类可读日期字符串（例如 `2024-01-01`、`yesterday`）。 |
| Until | | `--until` | 显示早于给定日期的条目。日期格式与 `--since` 相同。 |
| Grep | | `--grep` | 过滤 `action: message` 文本包含给定模式的条目（大小写不敏感）。 |
| Author | | `--author` | 过滤 committer 名称或 email 包含给定模式的条目（大小写不敏感）。 |
| Limit | `-n` | `--number` | 要显示的最大条目数。 |
| Patch | `-p` | `--patch` | 显示每个 reflog 条目引用提交引入的 diff。 |
| Stat | | `--stat` | 为每个 reflog 条目显示 diffstat（变更文件、插入、删除）。 |
| No abbrev | | `--no-abbrev` | 打印完整对象名而非缩写的 7 位前缀（对所有 `--pretty` 格式生效）。 |

```bash
# 显示 HEAD reflog（默认）
libra reflog show

# 显示特定分支的 reflog
libra reflog show feature-branch

# 使用 medium 格式显示（包含日期）
libra reflog show --pretty medium

# 按日期范围过滤
libra reflog show --since 2024-01-01 --until 2024-06-30

# 按 action/message 内容过滤
libra reflog show --grep "commit"

# 按作者过滤
libra reflog show --author "alice"

# 限制输出并显示 diff
libra reflog show -n 5 -p

# 显示 stat 摘要
libra reflog show --stat
```

### 子命令：`delete`

按 selector 删除特定 reflog 条目。

| 参数 | 说明 |
|----------|-------------|
| `<selector>...` | 一个或多个 `ref@{N}` 格式的 reflog selector（例如 `HEAD@{3}`、`main@{0}`）。裸分支名会展开为 `refs/heads/<name>`。多个 selector 可以针对不同 refs；同一 ref 内的条目会按逆索引顺序删除，以保持索引。 |

```bash
# 删除单个 reflog 条目
libra reflog delete HEAD@{3}

# 删除多个条目
libra reflog delete HEAD@{1} HEAD@{3} main@{0}
```

### 子命令：`exists`

检查某个引用是否有任何 reflog 条目。如果至少存在一个条目则成功退出（0），未找到条目则失败。主要用于脚本和自动化。

| 参数 | 说明 |
|----------|-------------|
| `<ref_name>` | 要检查的引用名（必需）。裸分支名会展开为 `refs/heads/<name>`。 |

```bash
# 检查 HEAD 是否有 reflog 条目
libra reflog exists HEAD

# 检查分支
libra reflog exists main
```

## 常用命令

```bash
# 查看最近 HEAD reflog 条目
libra reflog show

# 查看带日期的分支 reflog
libra reflog show main --pretty medium

# 在 reflog 中查找特定作者的提交
libra reflog show --author "alice" -n 10

# 查找 merge 相关 reflog 条目
libra reflog show --grep "merge"

# 显示最近条目及 diff
libra reflog show -n 3 -p

# 删除陈旧 reflog 条目
libra reflog delete HEAD@{5}

# 检查分支是否有 reflog（脚本）
libra reflog exists feature-branch
```

## 人类可读输出

**`reflog show`**（oneline 格式，默认）：

```text
abc1234 HEAD@{0}: commit: add new feature
def5678 HEAD@{1}: checkout: moving from main to feature-branch
ghi9012 HEAD@{2}: commit: initial commit
```

**`reflog show --pretty short`**：

```text
commit abc1234
Reflog: HEAD@{0} (Alice <alice@example.com>)
Reflog message: commit: add new feature
Author: Alice <alice@example.com>

  add new feature
```

**`reflog show --pretty medium`**（包含日期）：

```text
commit abc1234
Reflog: HEAD@{0} (Alice <alice@example.com>)
Reflog message: commit: add new feature
Author: Alice <alice@example.com>
Date:   Mon Jan 15 10:30:00 2024 -0800

  add new feature
```

**`reflog show --pretty full`**（包含 committer）：

```text
commit abc1234
Reflog: HEAD@{0} (Alice <alice@example.com>)
Reflog message: commit: add new feature
Author: Alice <alice@example.com>
Commit: Alice <alice@example.com>

  add new feature
```

**`reflog exists`**（找到 ref）：

无输出，退出码 0。

**`reflog exists`**（未找到 ref）：

```text
fatal: reflog entry for 'nonexistent' not found
```

## JSON / Machine 输出

`show`、`delete` 和 `exists` 支持 `--json` 和 `--machine`。`--json` 输出命令信封，`--machine` 以单条 NDJSON 行输出同一信封。

**`reflog show`**：

```json
{
  "ok": true,
  "command": "reflog.show",
  "data": {
    "ref_name": "HEAD",
    "pretty": "oneline",
    "count": 1,
    "total_count": 3,
    "filters": {
      "since": null,
      "until": null,
      "grep": null,
      "author": null,
      "number": 1,
      "patch": false,
      "stat": false
    },
    "entries": [
      {
        "selector": "HEAD@{0}",
        "index": 0,
        "ref_name": "HEAD",
        "old_oid": "def5678...",
        "new_oid": "abc1234...",
        "short_new_oid": "abc1234",
        "timestamp": 1715788800,
        "datetime": "Wed May 15 16:00:00 2024 +0000",
        "committer": {
          "name": "Alice",
          "email": "alice@example.com"
        },
        "action": "commit",
        "message": "add new feature",
        "summary": "commit: add new feature",
        "commit": {
          "author": {
            "name": "Alice",
            "email": "alice@example.com"
          },
          "message": "add new feature"
        },
        "patch": null,
        "stat": null
      }
    ]
  }
}
```

设置 `--patch` 或 `--stat` 时，对应条目字段包含渲染后的 patch 或 stat 字符串；否则为 `null`。

**`reflog delete`**：

```json
{
  "ok": true,
  "command": "reflog.delete",
  "data": {
    "selectors": ["HEAD@{0}"],
    "deleted_count": 1
  }
}
```

**`reflog exists`**：

```json
{
  "ok": true,
  "command": "reflog.exists",
  "data": {
    "ref_name": "HEAD",
    "exists": true
  }
}
```

## 设计理由

### 为什么基于子命令，而不是 Git 的隐式 `show`？

Git 将 `git reflog` 视为 `git reflog show` 的简写，其子命令（`expire`、`delete`、`exists`）有些隐藏。Libra 将所有操作做成显式子命令：`show`、`delete` 和 `exists`。这消除了人类用户和 AI 代理的歧义，使命令表面可通过 `--help` 完整发现。它也符合 Libra 的一般原则：每个操作都应是具名子命令，而不是隐式默认值。

### 为什么提供 `--grep` 和 `--author` 过滤？

Git 的 `git reflog` 通过 `git log` 选项支持过滤，因为它共享同一底层机制。然而，这种关联对用户并不明显。Libra 在 `reflog show` 上将 `--grep` 和 `--author` 作为一等选项，使 reflog 条目可以按内容和 committer 搜索这件事立即清楚。两个过滤器都大小写不敏感以方便使用。`--grep` 过滤器匹配组合后的 `action: message` 字符串（例如 `commit: add new feature`），因此用户可以按动作类型或消息内容过滤。

### 为什么使用 `FormatterKind` 而不是 Git 的 `--format`？

Git 的 `--format` 接受带 `%H`、`%s` 等占位符的任意格式字符串。这很强大但复杂，并且很少用于 reflog。Libra 通过 `--pretty` 提供四个具名格式（`oneline`、`short`、`medium`、`full`），覆盖常见用例。这更容易实现和文档化，并足以检查 reflog。默认 `oneline` 适合扫描；`medium` 为取证添加日期；`full` 为审计轨迹添加 committer。

### 为什么 reflog 上有 `--patch` 和 `--stat`？

这些选项借自 `libra log`，允许用户直接查看每个 reflog 条目实际改变了什么，而无需分别为每个提交运行 `libra show` 或 `libra diff`。这在调查回归时尤其有用：reflog 显示 HEAD 何时移动，而 `--patch`/`--stat` 显示每一步改变了什么。

### 为什么使用 SQLite 而不是扁平文件？

Git 将 reflog 存储为 `.git/logs/` 下的追加文本文件。这很简单，但没有事务保证，并且需要解析才能查询。Libra 将 reflog 条目存储在 SQLite `reflog` 表中，提供 ACID 事务、结构化查询，以及无需重写整个文件即可删除单个条目的能力。代价是 reflogs 在磁盘上不是人类可读的，但 `reflog show` 命令提供所有必要检查能力。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 显示 reflog | `reflog show [ref]` | `reflog [show] [ref]`（隐式） | `op log`（operation log） |
| 默认 ref | `HEAD` | `HEAD` | N/A（显示所有操作） |
| 格式 | `--pretty oneline\|short\|medium\|full` | `--format <string>` / `--oneline` | 内置格式 |
| 日期过滤（since） | `--since <date>` | `--since <date>`（通过 log 选项） | N/A |
| 日期过滤（until） | `--until <date>` | `--until <date>`（通过 log 选项） | N/A |
| 消息过滤 | `--grep <pattern>` | `--grep <pattern>`（通过 log 选项） | N/A |
| 作者过滤 | `--author <pattern>` | N/A（reflog 上不直接支持） | N/A |
| 限制条目 | `-n <N>` | `-n <N>`（通过 log 选项） | `-n <N>` |
| 显示 patch | `-p` / `--patch` | `-p`（通过 log 选项） | `--patch` on `op show` |
| 显示 stat | `--stat` | `--stat`（通过 log 选项） | `--stat` on `op show` |
| 完整对象名 | `--no-abbrev` | `--no-abbrev` | N/A |
| 删除条目 | `reflog delete <selector>...` | `reflog delete <ref@{N}>` | N/A（operation log 只追加） |
| 检查存在性 | `reflog exists <ref>` | `reflog exists <ref>` | N/A |
| 过期旧条目 | 不支持 | `reflog expire` | N/A（GC 处理清理） |
| 存储 | SQLite 表 | 扁平文件（`.git/logs/`） | Operation log（自定义格式） |

注意：jj 没有 reflog。相反，它维护 operation log（`jj op log`），记录每次仓库变更。这提供类似取证能力，但位于 operation 层而不是 reference 层。

## 错误处理

| 代码 | 条件 |
|------|-----------|
| `LBR-REPO-001` | 不是 libra 仓库 |
| `LBR-CLI-002` | 无效 `--since` 或 `--until` 日期格式 |
| `LBR-CLI-002` | 无效 reflog selector 格式（必须为 `ref@{N}`） |
| `LBR-CLI-003` | Reflog 条目未找到（用于 `exists` 或 `delete`） |
| `LBR-REPO-002` | Reflog 条目指向缺失或无效提交对象 |
| `LBR-IO-001` | 无法从数据库读取 reflog 条目 |
| `LBR-IO-002` | 无法从数据库删除 reflog 条目 |
