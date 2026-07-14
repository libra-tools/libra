# `libra cat-file`

检查仓库中存储的 Git 对象和 Libra AI 历史对象。

## 概要

```
libra cat-file [OPTIONS] [OBJECT]
```

## 说明

`libra cat-file` 是类似 `git cat-file` 的低层调试工具。它可以打印任何 Git 对象（commit、tree、blob、tag）的类型、大小或美化内容，也可以检查对象是否存在。

Libra 用 `--ai*` 标志扩展了经典命令，用于检查存储在 `libra/intent` orphan 分支上的 AI 工作流对象（Intent、Task、Run、Plan、PatchSet、Evidence、Session 等）。这为开发者和代理提供了一个单一入口，用于同时内省版本控制对象和 AI 流程产物。

必须且只能指定一个模式标志。Git 模式（`-t`、`-s`、`-p`、`-e`）需要位置 `OBJECT` 参数。AI 模式（`--ai`、`--ai-type`、`--ai-list`、`--ai-list-types`）忽略 `OBJECT`，并在 AI history 分支上操作。

Git 对象模式与 batch 输入复用 `rev-parse` 的严格 resolver：支持 ref/唯一 OID 前缀、`@`、parent 导航、typed/递归 peel、数字 reflog selector 与 `REV:path`。resolver 必须消费完整输入，因此 `HEAD^{tree}junk` 这类非法后缀会被拒绝，不会只解析一部分。

Batch 模式仅把不可解析或不存在的名称输出为 `<input> missing`。ref 数据库读取失败或对象损坏/不可读会让命令以结构化错误失败，避免把存储损坏静默伪装成缺失对象。

## 选项

| 标志 | 短选项 | 说明 |
|------|-------|-------------|
| `-t` | | 打印对象类型（`commit`、`tree`、`blob`、`tag`）。 |
| `-s` | | 打印对象大小（字节）。 |
| `-p` | | 美化打印对象内容。 |
| `-e` | | 检查对象是否存在。不带 `--json` 时仅退出状态（0 = 存在，1 = 缺失），无 stdout；带 `--json`/`--machine` 时输出 `{ "exists": bool }`，并保持相同退出码。 |
| `--batch-check[=<fmt>]` | | 从 stdin 逐行读对象名，输出 `<sha> <type> <size>`（无法解析时输出 `<input> missing`）。可选格式原子 `%(objectname)`/`%(objecttype)`/`%(objectsize)`。 |
| `--batch[=<fmt>]` | | 同 `--batch-check`，并附加 raw 对象内容与换行。 |
| `--batch-command[=<fmt>]` | | 从 stdin 逐行读命令：`info <object>`（仅 header）或 `contents <object>`（header + 内容）。`flush` 命令仅在 `--buffer` 下有效。 |
| `--buffer` | | 缓冲 batch 输出，仅在显式 `flush`（或输入结束）时写出；这使 `--batch-command` 的 `flush` 生效。需配合 batch 模式。 |
| `--batch-all-objects` | | 配合 `--batch`/`--batch-check`，对存储中每个对象（loose + packed）按 id 排序处理，替代从 stdin 读取。 |
| `--ai <ID>` | | 按 ID 美化打印 AI 对象。接受 `TYPE:ID` 以消除歧义。 |
| `--ai-type <ID>` | | 打印给定 ID 的 AI 对象类型。 |
| `--ai-list <TYPE>` | | 列出给定类型的所有 AI 对象（例如 `intent`、`patchset`、`event`）。 |
| `--ai-list-types` | | 列出 history 分支中存在的所有 AI 对象类型。 |
| `<OBJECT>` | | Git 对象 spec：哈希/ref、typed peel、数字 reflog selector 或 `REV:path`。`-t`/`-s`/`-p`/`-e` 必需；`--ai*` 模式忽略；batch 模式改从 stdin 读取对象 spec。 |

### 示例

```bash
# 打印 HEAD 的类型
libra cat-file -t HEAD

# 打印特定对象的大小
libra cat-file -s 40d352ee7190f92dcf7883b8a81f2c730fd8a860

# 美化打印 HEAD 提交
libra cat-file -p HEAD

# 检查 peeled tree 或按路径选中的 blob
libra cat-file -t 'HEAD^{tree}'
libra cat-file -p 'HEAD:src/main.rs'

# 检查存在性（退出码 0 = 存在）
libra cat-file -e abc1234

# 以 JSON 形式检查存在性（{ "exists": bool }；退出码保持不变）
libra cat-file -e abc1234 --json

# 结构化 JSON 类型查询
libra cat-file -t HEAD --json

# 列出所有 AI intent 对象
libra cat-file --ai-list intent

# 美化打印 AI 对象（用 TYPE:ID 消除歧义）
libra cat-file --ai patchset:call_KjR3NB4cQaT5Rm1c7zXjsskQ

# 打印 AI 对象类型
libra cat-file --ai-type debug-local-1772707227

# 列出仓库中的所有 AI 对象类型
libra cat-file --ai-list-types --json
```

## 常用命令

```bash
libra cat-file -t HEAD
libra cat-file -s HEAD
libra cat-file -p HEAD
libra cat-file -t 'HEAD^{tree}'
libra cat-file -p 'HEAD:src/main.rs'
libra cat-file -t HEAD --json
libra cat-file --ai-list-types --json
libra cat-file --ai-list intent
libra cat-file --ai <session-id>
```

## 人类可读输出

- `-t`：在单行打印对象类型（例如 `commit`）
- `-s`：在单行打印字节大小（例如 `342`）
- `-p`：根据类型美化打印内容：
  - Commit：头字段和消息
  - Tree：每个条目为 `<mode> <type> <hash>\t<name>`
  - Blob：原始文本内容
  - Tag：tag 头和消息
- `-e`：无输出；对象存在时退出码为 0，否则非零
- `--ai <ID>`：打印格式化摘要（`ai_session` 对象为 session 摘要，其他对象为完整 JSON）
- `--ai-list <TYPE>`：每行一个对象 ID
- `--ai-list-types`：每行一个类型名

## 结构化输出（JSON 示例）

### Type 模式（`-t`）

```json
{
  "ok": true,
  "command": "cat-file",
  "data": {
    "mode": "type",
    "object": "HEAD",
    "hash": "abc1234def5678901234567890abcdef12345678",
    "object_type": "commit"
  }
}
```

### Size 模式（`-s`）

```json
{
  "ok": true,
  "command": "cat-file",
  "data": {
    "mode": "size",
    "object": "HEAD",
    "hash": "abc1234def5678901234567890abcdef12345678",
    "size": 342
  }
}
```

### Pretty-print 模式（`-p`）-- commit

```json
{
  "ok": true,
  "command": "cat-file",
  "data": {
    "mode": "pretty",
    "object": "HEAD",
    "hash": "abc1234def5678901234567890abcdef12345678",
    "object_type": "commit",
    "content": {
      "tree": "def456...",
      "parents": ["abc123..."],
      "author": "Alice <alice@example.com> 1711929600 +0000",
      "committer": "Alice <alice@example.com> 1711929600 +0000",
      "message": "feat: add new feature"
    }
  }
}
```

### AI list types

```json
{
  "ok": true,
  "command": "cat-file",
  "data": {
    "mode": "ai-list-types",
    "types": ["intent", "patchset", "plan", "run", "task"]
  }
}
```

说明：

- `cat-file -e --json` / `--machine` 会向 stdout 输出 `{ "exists": bool }`，同时保留退出码契约（存在 → 0，格式正确但缺失 → 1，非法对象名 → 129）
- Blob/tag 美化打印 JSON 要求 UTF-8 内容；非文本 payload 会显式失败，而不是返回有损数据

## 设计理由

### 为什么添加 `--ai*` 标志？

Libra 的 AI 代理基础设施将流程产物（intents、plans、tasks、runs、patch sets、evidence、sessions）作为 Git 对象存储在 orphan 分支上。无需单独检查工具，`cat-file` 是自然归宿，因为它已经处理“按 ID 显示对象原始内容”的需求。`--ai*` 标志将这一能力扩展到 AI 对象命名空间，同时保持熟悉接口。这意味着单个命令既能回答“这个提交是什么类型？”，也能回答“这个 AI 计划包含什么？”，这在调试代理工作流时尤其有用。

### Batch 模式与结构化输出

Git 的 batch 模式从 stdin 读取对象 ID（或命令）以批量检查。Libra 已公开 `--batch-check`、`--batch` 和 `--batch-command`（后者逐行分发 `info`/`contents`），三者共享同一个逐对象 formatter，并支持可选的 `=<format>` 原子展开。`--batch-all-objects`（配合 `--batch`/`--batch-check`）遍历存储中的每个对象——loose 加 packed——按 id 排序，替代从 stdin 读取。对代理而言，仍推荐 `--json`——它在一次调用中返回类型化字段。`--buffer` 已支持（缓冲 batch 输出，仅在显式 `flush` 或输入结束时写出——这使 `--batch-command` 的 `flush` 生效；无 `--buffer` 时 `flush` 像 Git 一样被拒绝，且 `--buffer` 需配合 batch 模式）。`--follow-symlinks` 未公开。

### `-e` 在 `--json` 下如何表现？

默认情况下 `-e`（存在性检查）是静默探测，仅通过退出码传达结果：0 表示对象存在，非零表示不存在。这是布尔谓词的 Unix 约定，脚本可以写 `if libra cat-file -e $hash; then ...`。

对偏好结构化输出的代理，`-e --json`（或 `--machine`）会在 stdout 输出 `{ "exists": bool }` 信封，但**不改变**退出码契约：对象存在仍 exit 0，格式正确但缺失仍 exit 1（JSON 先写出），非法对象名仍是硬错误（`LBR-CLI-003`，exit 129）且不输出信封。

## 参数对比：Libra vs Git vs jj

| 功能 | Libra | Git | jj |
|---------|-------|-----|----|
| 打印对象类型 | `-t` | `-t` | N/A（无直接等价） |
| 打印对象大小 | `-s` | `-s` | N/A |
| 美化打印内容 | `-p` | `-p` | N/A（blob 使用 `jj file show`） |
| 检查存在性 | `-e` | `-e` | N/A |
| Batch 模式 | `--batch`, `--batch-check`, `--batch-command`, `--batch-all-objects`（带可选 `=<format>`） | `--batch`, `--batch-check`, `--batch-command`, `--batch-all-objects` | N/A |
| AI 对象检查 | `--ai`, `--ai-type` | N/A | N/A |
| AI 对象列出 | `--ai-list`, `--ai-list-types` | N/A | N/A |
| JSON 输出 | `--json` | 无 | 无 |
| 对象解析 | SHA-1/SHA-256、refs、`@`、`^N`/`~N`、typed/递归 peel、数字 reflog selector、`REV:path` | 完整 rev-parse 语法 | Change IDs、revsets |
| `--filters` | 否 | `--filters`（与外部格式互转） | N/A |
| `--textconv` | 否 | `--textconv` | N/A |

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 无效对象 / 修订 | `LBR-CLI-003` | 129 |
| 不支持的参数组合 | `LBR-CLI-002` | 129 |
| 无法读取对象数据 | `LBR-IO-001` / `LBR-REPO-002` | 128 |
