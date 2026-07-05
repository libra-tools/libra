# `libra notes`

为 commits 添加、追加、复制、编辑、显示、列出或移除 notes，而不修改 commit 本身。

> 状态：`partial`。`libra notes` 现在已注册到公开 CLI。核心操作（`add`、`append`、`copy`、`edit`、`list`、`show`、`remove`）和 `merge`（flat note rows 的 2-way merge，带 `--strategy`）已支持。`prune` 和 `get-ref` 也已支持。当 `add`/`edit`/`append` 未给 `-m`/`-F` 时，支持交互式 editor fallback。

## 概要

```
libra notes add [-m <message> | -F <file>] [-f] [<object>]
libra notes append [-m <message> | -F <file>] [<object>]
libra notes edit [-m <message> | -F <file>] [<object>]
libra notes copy [-f] <from-object> <to-object>
libra notes list [<object>]
libra notes show [<object>]
libra notes remove [<object>...]
libra notes merge [-s|--strategy <manual|ours|theirs|union|cat_sort_uniq>] <other-ref>
libra notes prune [-n|--dry-run] [-v]
libra notes get-ref
```

## 说明

`libra notes` 管理附着到 commit 对象的 annotations。与 commit message 不同，notes 可在 commit 创建后添加或移除 — 原始 commit hash 保持不变。这使它们适合存储事后 metadata，例如 code-review 结果、CI 状态或部署跟踪。

Notes 作为 blob objects 存储在 notes ref 下（默认 `refs/notes/commits`）。使用 `--ref <ref>` 可操作不同 namespace（例如 `refs/notes/review`）。

省略子命令时默认是 `list`。

## 选项

| 标志 | 长参数 | 值 | 说明 |
|------|--------|----|------|
| | `<object>` | positional（可选） | 要 annotate、show 或 remove notes 的 commit。默认 HEAD。 |
| `-m` | `--message` | `<msg>` | Note message 文本。可重复；消息之间用空行分隔。 |
| `-F` | `--file` | `<file>` | 从文件读取 note message（`-` 表示 stdin）。 |
| `-f` | `--force` | | 覆盖现有 note（用于 `add` 和 `copy`）。 |
| | `--ref` | `<ref>` | 操作指定 notes ref（默认：`refs/notes/commits`）。 |

### 子命令

| 子命令 | 说明 |
|--------|------|
| `add` | 给对象添加 note。若 note 已存在则失败；用 `-f` 覆盖。没有 `-m`/`-F` 时打开 editor — 新 note 为空 buffer，或在 `-f` 且已有 note 时预填现有 note（已有 note 且无 `-f` 时会在打开 editor 前中止）。 |
| `append` | 向对象 note 追加 message（用空行分隔），缺失时创建 note。没有 `-m`/`-F` 时打开 editor（空 buffer）。 |
| `edit` | 设置（替换）对象 note，缺失时创建 — 与 `add` 不同，它无条件覆盖。没有 `-m`/`-F` 时打开 editor 并预填现有 note。 |
| `copy` | 将 `<from-object>` 的 note 复制到 `<to-object>`。若源没有 note 或目标已有 note 则失败（用 `-f` 覆盖）。 |
| `list` | 列出 note objects 及其 annotate 的 commits（默认子命令）。 |
| `show` | 显示某个对象的 note 文本。 |
| `remove` | 删除一个或多个对象的 notes。 |
| `merge` | 将另一个 notes ref（`<other-ref>`）合并到当前 `--ref`。这是 flat note rows 的 2-way merge：复制当前 ref 中不存在的新 notes、跳过相同 notes，并按 `-s`/`--strategy`（默认 `manual`，也可为 `ours`、`theirs`、`union`、`cat_sort_uniq`）解决差异 note。 |
| `prune` | 删除被 annotate 的对象已不在对象库中的 notes。默认静默；`-n`/`--dry-run` 报告将会 prune 什么，`-v` 打印每个 pruned object id。 |
| `get-ref` | 打印操作作用的 notes ref（遵守 `--ref`；默认 `refs/notes/commits`）。 |

### 标志示例

```bash
# 用 review 结果 annotate HEAD
libra notes add -m "Reviewed-by: Alice <alice@example.com>"

# 从文件添加
libra notes add -F review-summary.txt abc1234

# 强制覆盖现有 note
libra notes add -m "Updated review" -f HEAD

# 向 HEAD 的 note 追加另一行（空行分隔）
libra notes append -m "Deployed-by: CI"

# 从一个 commit 复制 note 到另一个 commit
libra notes copy abc1234 def5678

# 无条件设置（替换）HEAD 的 note
libra notes edit -m "Replaces any existing note"

# 列出所有 notes
libra notes list

# 显示 HEAD 上的 note
libra notes show

# 显示指定 commit 上的 note
libra notes show abc1234

# 移除 HEAD 上的 note
libra notes remove

# 从多个 commits 移除 notes
libra notes remove abc1234 def5678

# 使用自定义 namespace
libra notes --ref refs/notes/ci add -m "Passed all tests" HEAD
libra notes --ref refs/notes/ci show HEAD

# 将另一个 notes ref merge 进 refs/notes/commits（冲突时取 theirs）
libra notes merge --strategy=theirs refs/notes/ci

# 给 agents 使用的 JSON 输出
libra notes show --json
libra notes list --json
```

## 常用命令

```bash
libra notes add -m "Reviewed-by: Alice"       # 给 HEAD 添加 note
libra notes show                                # 显示 HEAD 上的 note
libra notes list                                # 列出所有 notes
libra notes remove abc1234                      # 移除 note
libra notes add -f -m "Updated" HEAD            # 强制覆盖 note
libra notes --json show                         # 结构化 JSON 输出
```

## 人类可读输出

- `libra notes add -m "msg"`：`Added note to abc1234 in refs/notes/commits`
- `libra notes show`：按原样打印 note 文本
- `libra notes list`：`<note-hash> <annotated-object-hash>`，每行一个
- `libra notes remove abc1234`：`Removed note from abc1234 in refs/notes/commits`
- `libra notes`（无参数）：等同 `list`

## 结构化输出（JSON 示例）

带 `--json` / `--machine` 时，信封的 `action` 字段区分操作：

### `add`

```json
{
  "ok": true,
  "command": "notes",
  "data": {
    "action": "add",
    "ref": "refs/notes/commits",
    "object": "abc1234...",
    "note_hash": "def5678..."
  }
}
```

### `show`

```json
{
  "ok": true,
  "command": "notes",
  "data": {
    "action": "show",
    "ref": "refs/notes/commits",
    "object": "abc1234...",
    "note_hash": "def5678...",
    "text": "Reviewed-by: Alice <alice@example.com>"
  }
}
```

### `list`

```json
{
  "ok": true,
  "command": "notes",
  "data": {
    "action": "list",
    "ref": "refs/notes/commits",
    "notes": [
      { "note_hash": "def5678...", "annotated_object": "abc1234..." },
      { "note_hash": "1111222...", "annotated_object": "def5678..." }
    ]
  }
}
```

给出 `<object>` 且没有 note 时，`note_hash` 为 `null`。

### `remove`

```json
{
  "ok": true,
  "command": "notes",
  "data": {
    "action": "remove",
    "ref": "refs/notes/commits",
    "removed": [
      { "object": "abc1234...", "note_hash": "def5678..." }
    ]
  }
}
```

## 设计动机

### Editor fallback

像 Git 一样，`add`/`edit`/`append` 在既没有 `-m` 也没有 `-F` 时打开 editor。Editor 按 `GIT_EDITOR` → `core.editor` → `VISUAL` → `EDITOR` 解析（只有在终端上才 fallback 到 `vi`）；在 headless/non-terminal 环境且未配置 editor 时，命令会以清晰的 “no editor configured” 错误失败。`edit` 会用现有 note 预填 buffer，`add -f` 在已有 note 时也会这样（普通 `add` 若 note 已存在，会在打开 editor 前中止）；`add`（新 note）和 `append` 从空 buffer 开始。保存后的 buffer 用 `git stripspace` whitespace 规则清理，但 — 不同于 commit/tag messages — 会 **保留** `#` 行，因为 note 可以合法包含它们。空结果会中止。

对 headless 或 agent-driven workflows，优先使用 `-m <message>` 或 `-F <file>`（`-` 表示 stdin），它们永不调用 editor。

### `merge`、`prune` 和 `get-ref`

`notes merge <other-ref>` 将另一个 notes ref merge 到当前 ref（`--ref`，默认 `refs/notes/commits`）。因为 Libra 将 notes 存为 flat SQLite rows，而不是 Git 的 commit-backed notes trees，所以没有 common base 可用于 Git 真正的 3-way merge — 这里是 2-way merge：只存在于 other ref 的 annotated objects 会被复制，相同 notes 会跳过，双方都有不同 note 的 object 是冲突并由 `--strategy` 解决：

- `manual`（默认）：若有任何 note 冲突，中止整个 merge（Libra 没有用于手工解决的 NOTES_MERGE worktree）。
- `ours` / `theirs`：保留当前 note / 采用 other ref 的 note。
- `union`：连接两个 note contents。
- `cat_sort_uniq`：连接后排序并去重合并行。

`notes prune` 删除被 annotate 的对象已不存在于对象库中的 notes — Libra 会检查每个 flat note row 的对象是否存在于对象库，并删除孤儿 rows（在事务中执行，且用 blob compare-and-swap，保证并发改写的 note 保持不变）。默认静默；`-n` / `--dry-run` 报告将要 prune 什么但不删除，`-v` 打印每个 pruned object id。`notes get-ref` 打印操作作用的 notes ref（遵守 `--ref`；默认 `refs/notes/commits`）。

### 为什么是 SQLite-backed notes refs？

Libra 将 notes refs 存入 SQLite，而不是 `.git/refs/notes/` 下的 loose files。这提供原子事务（一次操作内 add/remove）、高效查询（列出所有 notes 是一次查询，而不是目录扫描），并通过 SQLite WAL mode 提供并发安全。

## 参数对比：Libra vs Git vs jj

| 功能 | Git | Libra | jj |
|------|-----|-------|----|
| 添加 note | `git notes add [-m <msg>] [<obj>]` | `libra notes add [-m <msg>] [<obj>]`（无 `-m`/`-F` 时 editor fallback） | N/A |
| 列出 notes | `git notes list [<obj>]` | `libra notes list [<obj>]` | N/A |
| 显示 note | `git notes show [<obj>]` | `libra notes show [<obj>]` | N/A |
| 移除 note | `git notes remove [<obj>...]` | `libra notes remove [<obj>...]` | N/A |
| Append | `notes append` | 支持 | N/A |
| Copy | `notes copy [-f] <from> <to>` | 支持 | N/A |
| Edit | `notes edit`（`-m`/`-F` 或 editor） | 支持（editor 预填现有 note） | N/A |
| Merge | `notes merge [-s <strategy>] <ref>` | `libra notes merge [-s <strategy>] <ref>`（2-way flat-row merge） | N/A |
| Prune | `notes prune [-n] [-v]` | `libra notes prune [-n] [-v]` | N/A |
| Get ref | `notes get-ref` | `libra notes get-ref` | N/A |
| Custom ref | `--ref <ref>` | `--ref <ref>` | N/A |
| File input | `-F <file>` | `-F <file>` | N/A |
| Editor support | 交互式 editor（默认） | 无 `-m`/`-F` 时 editor fallback（`edit` 预填；保留 `#` 行） | N/A |
| Structured output | 无 | `--json` / `--machine` | N/A |
| Ref storage | Loose files + packed-refs | SQLite（libra.db） | N/A |

注意：jj 没有 notes 功能。

## 错误处理

| 场景 | Error Code | Hint |
|------|------------|------|
| 对象已有 note（add 或 copy target） | `LBR-CONFLICT-002` | "use '-f' to overwrite the existing note." |
| 对象没有 note（show/remove） | `LBR-CLI-003` | "use 'libra notes list' to see which objects have notes." |
| 未配置 editor 且没有 `-m`/`-F`（non-terminal env） | `LBR-REPO-003` | "set GIT_EDITOR, core.editor, VISUAL, or EDITOR" / 传入 `-m`/`-F`。 |
| 编辑后的 note buffer 为空（无 `-m`/`-F`） | `LBR-CLI-002` | "write some text in the editor, or pass -m/--message." |
| 无效 object reference | `LBR-CLI-003` | "use 'libra log' to find valid commit references." |
| 无效 notes ref name | `LBR-CLI-002` | "notes refs must start with 'refs/notes/'; e.g. 'refs/notes/commits'." |
| 默认 `manual` strategy 下 `merge` 冲突 | `LBR-CONFLICT-002` | "re-run with --strategy=ours/theirs/union/cat_sort_uniq …" |
| `merge` 使用未知 `--strategy` 值 | `LBR-CLI-002` | "valid strategies: manual, ours, theirs, union, cat_sort_uniq" |
| 不是 libra 仓库 | `LBR-REPO-001` | 使用 `libra init` 初始化，或进入仓库目录。 |
| 加载/存储 blob object 失败 | `LBR-IO-002` | 检查仓库完整性。 |
| 读写 notes ref 失败 | `LBR-IO-002` | 检查数据库权限和可写性。 |
