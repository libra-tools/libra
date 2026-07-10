# `libra shortlog`

按作者汇总可达提交。

**别名：** `slog`

## 概要

```
libra shortlog [<revision>] [-n] [-s] [-e] [-c] [--no-merges | --merges] [--since <date>] [--until <date>] [-w[<W>[,<I1>[,<I2>]]]] [--format <FORMAT>]

git log | libra shortlog [-n] [-s] [-e] [--group <TYPE>] [--author <pattern>] [-w[...]]
```

## 说明

`libra shortlog` 汇总按作者分组的可达提交，主要用于发布公告和贡献者概览。它从指定修订（默认 HEAD）开始遍历提交图，并按作者聚合提交，显示每个作者的提交数量，以及可选的提交主题。

当未给定修订、且标准输入为带数据的非终端（管道）时，`libra shortlog` 改为汇总管道传入的 `git log` / `libra log` 输出（如 `git log | libra shortlog`），与 Git 的 stdin 模式对等：解析 medium/fuller 日志格式（`Author:` / `Commit:` 身份头与 4 空格缩进的消息），仅作用于分组与显示选项（`-n` / `-s` / `-e` / `--group` / `--author` / `-w` / `--top` / `--min-count` / `--reverse`），walk-only 过滤（`--since` / `--until` / `--merges` / `--no-merges` / `--format`）被忽略。空或终端 stdin 回落到 `HEAD` 默认；管道模式仍需在 Libra 仓库内运行。

默认情况下，作者按姓名字母顺序排序。使用 `-n` 时，按提交数量降序排序。`-s` 标志生成只包含数量的摘要，抑制单个提交主题。`-e` 标志会在输出中包含作者 email 地址。

通过 `--since` 和 `--until` 的日期过滤会基于 committer 时间戳限制包含哪些提交，支持 `YYYY-MM-DD`、`"N days ago"` 和 Unix 时间戳等格式。

## 选项

| 选项 | 短选项 | 长选项 | 说明 |
|--------|-------|------|-------------|
| Numbered | `-n` | `--numbered` | 按每个作者的提交数量降序排序，而不是按字母顺序。 |
| Summary | `-s` | `--summary` | 抑制提交描述；只显示每个作者的提交数量。 |
| Email | `-e` | `--email` | 在作者名旁显示 email 地址。启用后，作者按 `name <email>` 对分组。 |
| Committer | `-c` | `--committer` | 按提交者（committer）身份分组，而不是作者。 |
| Group | | `--group <TYPE>` | 按 `author`（默认）、`committer` 或 `trailer:<key>`（按指定提交消息 trailer 的每个值分组，例如 `trailer:Co-authored-by`）分组。优先于 `-c`。 |
| No merges | | `--no-merges` | 聚合前排除 merge 提交（父提交多于一个）。 |
| Merges | | `--merges` | 只包含 merge 提交（`--no-merges` 的反向；两者互相覆盖）。 |
| Top | | `--top <N>` | 只显示排序后的前 N 个身份。 |
| Min count | | `--min-count <N>` | 只显示提交数至少为 N 的身份。 |
| Reverse | | `--reverse` | 反转输出顺序。 |
| Author | | `--author <PATTERN>` | 只汇总作者匹配 PATTERN 的提交（大小写不敏感）。 |
| Since | | `--since <date>` | 只包含比指定日期更新的提交。 |
| Until | | `--until <date>` | 只包含比指定日期更旧的提交。 |
| Wrap | `-w` | `--wrap [<W>[,<I1>[,<I2>]]]` | 在宽度 `W`（默认 76）处换行主题，首行缩进 `I1`（6），续行缩进 `I2`（9）。`-w0` 仅缩进不换行。 |
| Format | | `--format <FORMAT>` | 在作者标题下用自定义模板渲染每条提交行（取代默认 subject）。支持与 `libra log --format` 相同的 `%` 占位符，包括 `%H`、`%h`、`%P`、`%p`、`%s`、`%f`、`%b`、`%B`、`%n`、ASCII/control `%xNN`、`%%`、`%an`、`%ae`、`%ad`、`%aI`、`%at`、`%cn`、`%ce`、`%cd`、`%cI`、`%ct`、`%d`、`%D`、`%m` 和颜色占位符。 |
| Revision | | 位置参数（可选） | 要从中汇总的修订。默认为 `HEAD`。 |
| JSON | | `--json` | 输出结构化 JSON。 |
| Quiet | | `--quiet` | 抑制人类可读输出。 |

### 选项细节

**`-n` / `--numbered`**

按提交数量降序排序作者。当两个作者数量相同时，按字母顺序排序：

```bash
$ libra shortlog -n
   5  Alice
   3  Bob
   1  Charlie
```

**`-s` / `--summary`**

产生只包含数量的紧凑输出，省略单个提交主题：

```bash
$ libra shortlog -s
   2  Test User
```

不使用 `-s` 时，提交主题列在每个作者下方：

```bash
$ libra shortlog
   2  Test User
      initial
      follow-up
```

**`-e` / `--email`**

将 email 地址追加到每个作者。启用后，同名但不同 email 的作者会分开列出：

```bash
$ libra shortlog -e
   2  Test User <test@example.com>
      initial
      follow-up
```

**`--since` / `--until`**

按 committer 时间戳过滤提交。支持的日期格式包括：

- `YYYY-MM-DD`（例如 `2026-01-01`）
- 相对日期（例如 `"7 days ago"`、`"2 weeks ago"`）
- Unix 时间戳

```bash
# 最近一个月的提交
libra shortlog --since "30 days ago"

# 某个日期范围内的提交
libra shortlog --since 2026-01-01 --until 2026-03-31
```

**Revision 参数**

指定 HEAD 以外的起点：

```bash
# 汇总最近 5 个提交
libra shortlog HEAD~5

# 从标签汇总
libra shortlog v1.0
```

## 常用命令

```bash
# 从 HEAD 生成默认 shortlog
libra shortlog

# 只显示数量摘要，并按数量排序
libra shortlog -n -s

# 包含 email 地址
libra shortlog -e

# 最近 5 个提交摘要
libra shortlog HEAD~5

# 日期范围内的提交
libra shortlog --since 2026-01-01 --until 2026-03-31

# 面向脚本的 JSON 输出
libra shortlog --json
```

## 人类可读输出

默认（按字母顺序，包含主题）：

```text
   2  Test User
      initial
      follow-up
```

摘要模式（`-s`）抑制主题。`-e` 会追加 `<email>`。

主题提取会跳过嵌入的签名头，并使用第一条有意义的提交消息行。

数量列会基于所有作者中的最大数量使用一致宽度右对齐。

## 结构化输出（JSON）

```json
{
  "ok": true,
  "command": "shortlog",
  "data": {
    "revision": "HEAD",
    "numbered": false,
    "summary": false,
    "email": false,
    "total_authors": 1,
    "total_commits": 2,
    "authors": [
      {
        "name": "Test User",
        "email": null,
        "count": 2,
        "subjects": ["initial", "follow-up"]
      }
    ]
  }
}
```

摘要模式下，`subjects` 是空数组。启用 `-e` 时，`email` 字段包含作者的 email 字符串；否则为 `null`。

`total_authors` 和 `total_commits` 字段为脚本和代理提供便捷聚合数量。

## 设计理由

### `--group` 如何工作？

Git 的 `--group=author`/`--group=committer`/`--group=trailer:<key>` 选择按什么分组。Libra 三者都支持：`author`（默认）与 `committer` 镜像 `-c` 也提供的身份分组；`trailer:<key>` 按指定提交消息 trailer 的每个值分组（例如 `--group=trailer:Co-authored-by`），适合分析 co-authored 提交或通过 `Signed-off-by` 等 trailer 记录的归属。单个提交可贡献多个 trailer 分组（每条匹配的 trailer 行一个）或不贡献。`--group` 优先于 `-c`/`--committer`。trailer key 大小写不敏感，在每条提交消息的末段（trailer 块）中匹配，`Name <email>` 形式的值会拆分为 name 与 email。Git 完整的 `interpret-trailers` 配置（折叠、分隔符、自定义配置）未建模。

### 位置修订与管道输入

Git 的 `shortlog` 可在两种模式下运行：从 stdin 读取经管道传入的 `git log` 输出，或直接遍历提交历史。Libra **两种都支持**，且两种模式都可输出 `--json`。主模式将修订作为位置参数（默认 `HEAD`），直接从提交图读取——更简单、更快（无序列化往返）。当未给定修订且 stdin 为带数据的非终端（管道）时，Libra 改为解析该管道日志输出（`git log | libra shortlog`），以获得 Unix 组合性与 Git stdin 模式的对等。由于管道数据是序列化文本而非提交对象，管道模式仅限于日志格式所携带的身份/主题信息：分组与显示选项生效，但提交图过滤器（`--since`/`--until`/`--merges`/`--no-merges`）与依赖对象的 `--format` 模板不生效（与 Git 一致）。空或终端 stdin 回落到 `HEAD` 默认（相对 Git 的便利差异，Git 无默认修订）；管道模式仍需在 Libra 仓库内运行。

### 为什么是精选的过滤子集而不是完整 log 选项？

Git 的 `shortlog` 在直接使用时（非管道）继承完整 `git log` 选项集——`--author`、`--grep`、`--no-merges` 等几十个选项。Libra 暴露一个精选子集，覆盖常见的 shortlog 需求——日期过滤（`--since`/`--until`）、`--author` 和 `--merges`/`--no-merges`——同时避免继承 log 命令选项空间的全部复杂性。较少用的 log 过滤器（如 `--grep`）不暴露。

### 为什么使用 committer 时间戳进行过滤？

`--since`/`--until` 过滤器使用 committer 时间戳（不是 author 时间戳），匹配 Git 行为。Committer 时间戳反映提交实际应用到当前分支的时间（例如 rebase 后），这对发布周期摘要比原始作者时间更相关。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 按数量排序 | `-n` / `--numbered` | `-n` / `--numbered` | N/A（无 shortlog 命令） |
| 仅摘要 | `-s` / `--summary` | `-s` / `--summary` | N/A |
| 显示 email | `-e` / `--email` | `-e` / `--email` | N/A |
| Since 日期 | `--since <date>` | `--since <date>` / `--after <date>` | N/A |
| Until 日期 | `--until <date>` | `--until <date>` / `--before <date>` | N/A |
| 修订 | `<revision>`（位置参数） | `<revision range>...` | N/A |
| Group by | `--group=author\|committer\|trailer:<key>` | `--group=author\|committer\|trailer:<key>` | N/A |
| 格式 | `--format=<format>` | `--format=<format>` | N/A |
| Committer 分组 | `-c` / `--committer` | `--committer`（已弃用，使用 `--group=committer`） | N/A |
| 管道输入 | `git log \| libra shortlog`（无修订、非 tty 且有数据的 stdin；仓库内） | 通过管道时从 stdin 读取 | N/A |
| No merges | `--no-merges` | `--no-merges` | N/A |
| 仅 merges | `--merges` | `--merges` | N/A |
| Author 过滤 | `--author=<pattern>` | `--author=<pattern>` | N/A |
| 输出换行 | `-w[<width>[,<i1>[,<i2>]]]` | `-w[<width>[,<i1>[,<i2>]]]` | N/A |
| Grep 过滤 | 不支持 | `--grep=<pattern>` | N/A |
| JSON 输出 | `--json` | 不支持 | N/A |
| Quiet 模式 | `--quiet` | 不支持 | N/A |

注意：jj 没有 shortlog 命令。类似信息可通过过滤 `jj log` 输出获得，但没有内置作者聚合。

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 无效 `--since` / `--until` 日期 | `LBR-CLI-002` | 129 |
| 无效修订 | `LBR-CLI-003` | 129 |
| HEAD 没有提交 | `LBR-REPO-003` | 128 |
| 无法读取引用或提交图 | `LBR-IO-001` / `LBR-REPO-002` | 128 |
