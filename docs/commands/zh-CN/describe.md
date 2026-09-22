# `libra describe`

查找某个提交可达的最近标签，并将其格式化为人类可读的版本描述。

**别名：** `desc`

## 概要

```
libra describe [OPTIONS] [COMMIT]
```

## 说明

`libra describe` 从给定提交（默认 `HEAD`）开始遍历提交祖先图（BFS），查找最近的标签。浅克隆中，`.libra/shallow` 列出的提交视为根（`--contains` 同样在边界停止）。损坏的 `.libra/shallow` fail-closed（`LBR-REPO-002`）。输出遵循 Git 的 describe 格式：

- 精确匹配：`v1.2.3`
- 带距离的可达标签：`v1.2.3-4-gabc1234`
- 回退（`--always`）：`abc1234`

默认只考虑附注标签。传递 `--tags` 可同时匹配轻量标签。当多个标签以相同距离可达时，优先选择附注标签；仍相同时按字典序打破平局。

当找不到标签且未使用 `--always` 时，命令会失败，并给出建议使用 `--tags` 或 `--always` 的可操作提示。

`--exact-match` 会把命令限制为只接受直接指向目标提交的标签。如果没有精确匹配的标签，即使同时传入 `--always` 也会失败。

`--dirty[=<mark>]` 会在跟踪内容偏离 `HEAD` 时追加后缀。默认后缀是 `-dirty`；可使用 `--dirty=<mark>` 指定自定义标记。未跟踪文件会被忽略，这与 Git 对该命令的 dirty 判定一致。

## 选项

| 标志 | 说明 | 默认值 |
|------|-------------|---------|
| `<COMMIT>` | 要描述的 commit-ish。接受 `HEAD`、分支名、标签名、原始 SHA-1、`HEAD~N`。 | `HEAD` |
| `--tags` | 在搜索中包含轻量标签（而不只是附注标签）。 | 关闭 |
| `--all` | 考虑任意 ref（本地分支、远程跟踪分支与标签，含轻量标签），而不仅是标签。名称带 `heads/`、`remotes/` 或 `tags/` 前缀；同一提交上标签优先，其次 heads，再次 remotes。 | 关闭 |
| `--abbrev <N>` | 输出中缩写提交哈希的十六进制位数。 | `7` |
| `--always` | 当没有标签可描述目标时，回退到缩写提交哈希，而不是失败。 | 关闭 |
| `--exact-match` | 仅在目标提交精确匹配某个标签时成功。 | 关闭 |
| `--candidates <N>` | `N=0` 仅接受精确标签匹配（等价于 `--exact-match`）；`N≥1` 维持 Libra 确定性的最近-tag BFS（不强制正数上界）。 | — |
| `--long` | 即使在精确匹配时也强制 `tag-N-gHASH` 输出（精确匹配输出 `tag-0-gHASH`）。 | 关闭 |
| `--dirty[=<mark>]` | 当跟踪内容偏离 `HEAD` 时追加 dirty 标记。 | 关闭；启用时默认标记为 `-dirty` |
| `--first-parent` | 遍历历史时只跟随合并提交的第一个父。 | 关闭 |
| `--match <pattern>` | 仅考虑名称匹配该 glob 的标签（可重复；OR 语义，wax glob ≤256 字符）。 | 无 |
| `--exclude <pattern>` | 排除名称匹配该 glob 的标签（可重复；优先于 `--match`）。 | 无 |
| `--contains` | 反向包含查询（git name-rev）：用最近的「后代」标签命名目标，输出 `<tag>`、`<tag>~<n>` 或 `<tag>~<n>^<m>~<k>`。隐含含轻量标签；等权重并列按标签名字典序确定；无后代标签时报错。 | 关闭 |

### 示例

```bash
# 仅使用附注标签描述 HEAD
libra describe

# 包含轻量标签
libra describe --tags

# 即使没有标签也始终产生输出
libra describe --always

# 仅接受精确标签匹配
libra describe --exact-match

# 描述特定提交
libra describe HEAD~5

# 用最近的后代标签命名提交（如 v1.0~2）
libra describe --contains HEAD~2

# 使用更长的缩写哈希
libra describe --abbrev 12

# 跟踪内容偏离 HEAD 时追加 -dirty
libra describe --dirty

# 使用自定义 dirty 标记
libra describe --dirty=-worktree

# 面向自动化的 JSON 输出
libra describe --json
```

## 常用命令

```bash
libra describe
libra describe --tags
libra describe --always
libra describe --exact-match
libra describe --dirty
libra describe HEAD~1
libra describe --contains HEAD~2
libra describe --json
libra describe --tags --abbrev 10
```

## 人类可读输出

- 精确标签匹配：`v1.2.3`
- 可达标签：`v1.2.3-4-gabc1234`
- `--always` 回退：`abc1234`
- tracked 内容变更时的 `--dirty`：`v1.2.3-dirty`
- tracked 内容变更时的 `--dirty=-worktree`：`v1.2.3-worktree`

`--quiet` 会抑制 `stdout`。

## 结构化输出（JSON 示例）

`--json` / `--machine` 返回：

### 标签匹配（精确）

```json
{
  "ok": true,
  "command": "describe",
  "data": {
    "input": "HEAD",
    "resolved_commit": "abc1234def5678901234567890abcdef12345678",
    "result": "v1.2.3",
    "tag": "v1.2.3",
    "distance": 0,
    "abbreviated_commit": null,
    "exact_match": true,
    "used_always": false,
    "dirty": false,
    "dirty_mark": null
  }
}
```

### 标签匹配（带距离）

```json
{
  "ok": true,
  "command": "describe",
  "data": {
    "input": "HEAD",
    "resolved_commit": "abc1234def5678901234567890abcdef12345678",
    "result": "v1.2.3-4-gabc1234",
    "tag": "v1.2.3",
    "distance": 4,
    "abbreviated_commit": "abc1234",
    "exact_match": false,
    "used_always": false,
    "dirty": false,
    "dirty_mark": null
  }
}
```

### 回退（`--always`，未找到标签）

```json
{
  "ok": true,
  "command": "describe",
  "data": {
    "input": "HEAD",
    "resolved_commit": "abc1234def5678901234567890abcdef12345678",
    "result": "abc1234",
    "tag": null,
    "distance": null,
    "abbreviated_commit": "abc1234",
    "exact_match": false,
    "used_always": true,
    "dirty": false,
    "dirty_mark": null
  }
}
```

当使用 `--always` 且没有标签匹配时，`tag` 和 `distance` 为 `null`，`abbreviated_commit` 包含输出的哈希。

### Dirty 后缀

```json
{
  "ok": true,
  "command": "describe",
  "data": {
    "input": "HEAD",
    "resolved_commit": "abc1234def5678901234567890abcdef12345678",
    "result": "v1.2.3-dirty",
    "tag": "v1.2.3",
    "distance": 0,
    "abbreviated_commit": null,
    "exact_match": true,
    "used_always": false,
    "dirty": true,
    "dirty_mark": "-dirty"
  }
}
```

## 设计理由

### 实现了 Git `describe` 的哪个子集？

Libra 已公开 `--tags`、`--abbrev`、`--always`、`--exact-match`、`--long`、`--dirty[=<mark>]`、`--first-parent`、`--match`/`--exclude`（wax glob，≤256 字符，exclude 优先）以及 `--candidates <N>`（`N=0` 等价 `--exact-match`；`N≥1` 维持确定性最近-tag BFS，不强制 Git 的正数候选上界）。基于 BFS 的算法直接且可预测。`--all` 已支持：将本地分支（`heads/`）、远程跟踪分支（`remotes/`）与标签（`tags/`，含轻量标签）一并加入候选集合后复用同一 BFS，同一提交上标签优先、其次 heads、再次 remotes。`--contains` 已支持：运行 Git 的反向遍历包含算法（name-rev）——从每个 tag commit 反向做 Dijkstra，first-parent 步便宜、其它父步昂贵，故最近后代 tag 的最直路径胜出——输出 `<tag>`、`<tag>~<n>` 或 `<tag>~<n>^<m>~<k>`。它隐含包含轻量 tag（如 `git name-rev --tags`），且当没有后代 tag 时报错。

### 为什么简化输出格式？

Libra 默认产生标准 `tag-N-gHASH` 格式（精确匹配时仅标签名），并支持 `--long` 在精确匹配时强制 `tag-0-gHASH` 长格式。JSON 输出还包含独立的 `tag`、`distance`、`abbreviated_commit` 和 `exact_match` 字段，因此任何需要区分精确匹配和非精确匹配的消费者都可以直接检查 `exact_match`，无需解析字符串格式。

### 为什么使用 BFS 而不是 Git 的候选算法？

Git 的 `describe` 使用更复杂的算法，考虑多个标签候选并选择距离最小者，同时用启发式方法避免遍历整个图。Libra 从目标提交开始使用更简单的 BFS，保证找到最近标签（DAG 中的最短路径）。对于 Libra 面向的仓库规模（带结构化标签的 monorepo），BFS 足够快且行为非常容易预测。代价是带有许多标签的极深历史可能比 Git 的剪枝搜索更慢，但实践中这还不是问题。

## 参数对比：Libra vs Git vs jj

| 功能 | Libra | Git | jj |
|---------|-------|-----|----|
| 默认目标 | `HEAD` | `HEAD` | N/A（无内置 describe） |
| 仅附注标签 | 默认行为 | 默认行为 | N/A |
| 包含轻量标签 | `--tags` | `--tags` | N/A |
| 缩写哈希长度 | `--abbrev <N>`（默认 7） | `--abbrev=<N>`（默认动态选择） | N/A |
| 回退到哈希 | `--always` | `--always` | N/A |
| 仅精确匹配 | `--exact-match` | `--exact-match` | N/A |
| 强制长格式 | `--long` | `--long` | N/A |
| 匹配标签模式 | `--match <glob>`（wax，≤256 字符，可重复） | `--match <glob>` | N/A |
| 排除标签模式 | `--exclude <glob>`（优先于 match） | `--exclude <glob>` | N/A |
| 候选数量 | `--candidates <N>`（N=0 等价 exact-match；N≥1 为确定性最近-tag BFS） | `--candidates=<N>`（默认 10） | N/A |
| 仅 first-parent | `--first-parent` | `--first-parent` | N/A |
| 考虑所有 ref | `--all`（heads/remotes/tags，带前缀） | `--all` | N/A |
| 查找包含提交的标签 | `--contains`（name-rev，输出 `<tag>~<n>^<m>`） | `--contains` | N/A |
| Dirty 后缀 | `--dirty[=<mark>]` | `--dirty[=<mark>]` | N/A |
| JSON 输出 | `--json`，带类型字段 | 无 | 无 |
| 算法 | BFS（最短路径） | 启发式多候选 | N/A |

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 无效修订 | `LBR-CLI-003` | 129 |
| `HEAD` 没有提交 | `LBR-REPO-003` | 128 |
| 无标签可描述目标且未使用 `--always` | `LBR-REPO-003` | 128 |
| `--exact-match` 目标没有精确标签（含 `--contains --exact-match` 仅命中相对名 `~N`） | `LBR-REPO-003` | 128 |
| `--contains` 目标没有后代标签 | `LBR-REPO-003` | 128 |
| 无法读取引用或对象 | `LBR-IO-001` / `LBR-REPO-002` | 128 |
