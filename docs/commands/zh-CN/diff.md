# `libra diff`

比较 HEAD、索引、工作树或两个修订之间的差异。

## 概要

```
libra diff [<pathspec>...]
libra diff <commit> [<commit>] [--] [<pathspec>...]
libra diff <commit>..<commit> | <commit>...<commit> [--] [<pathspec>...]
libra diff --staged [<commit>] [<pathspec>...]
libra diff --old <commit> --new <commit> [<pathspec>...]
libra diff [--raw | --name-only | --name-status | --numstat | --stat | --shortstat | --summary]
           [-s | --no-patch] [--exit-code] [--check] [-R] [-z]
libra diff [--compact-summary] [--diff-filter=<FILTER>] [--full-index]
           [-S <STRING> | -G <REGEX>]
           [--src-prefix=<PREFIX> --dst-prefix=<PREFIX>]
           [--word-diff[=<MODE>] | --color-words]
           [--algorithm <name>] [--output <file>]
```

## 说明

`libra diff` 显示仓库不同状态之间的更改。默认情况下，它比较索引和已跟踪工作树路径（未暂存更改）。未跟踪文件不属于默认 diff，因此不会影响 `--quiet`、`--exit-code`、`--name-status`、`--numstat` 或 `--shortstat`；请用 `libra status`、`libra ls-files --others` 或 `libra add` 检查或纳入未跟踪文件。使用 `--staged` 时，它比较 HEAD 和索引（已暂存更改）。使用 `--old` 和 `--new` 时，它比较两个任意提交。

Diff 引擎当前只接入 histogram 后端。解析器为前向兼容接受 `myers` 与 `myersMinimal`，但执行时会以 `LBR-CLI-002` 显式拒绝，而不会静默换用其他算法。输出可通过 `--output` 写入文件，并提供代码评审与摘要格式（`--raw`、`--name-only`、`--name-status`、`--numstat`、`--stat`、`--compact-summary`、`--shortstat`、`--summary`）。Pickaxe 过滤 `-S <STRING>` / `-G <REGEX>` 分别按出现次数变化或匹配的增删行筛选 file pair。可用 `-s`/`--no-patch` 配合 `--exit-code` 做仅状态检查；`-z`/`--null` 让 raw/name/numstat 输出以 NUL 终止，便于安全脚本解析。`--word-diff[=<mode>]` 以单词粒度重绘补丁，裸 `--color-words` 是其颜色模式简写。

当工作树存在未合并冲突条目时，默认工作区 diff 会输出冲突感知的 `diff --cc <path>` 记录，而不是把冲突文件误报为从 `/dev/null` 新增。

工作树中的已跟踪符号链接按链接目标 blob 字节参与 diff。修改 symlink 目标会显示旧目标和新目标的内容差异；dangling symlink 不会因为目标不存在而被当作删除路径。

Pathspec 参数会将 diff 过滤为只显示匹配文件或目录中的更改。

当 stdout 被管道连接且下游命令提前退出时，stdout `BrokenPipe` 会被视为正常管道终止；不会打印 panic/backtrace 或 `Broken pipe` 诊断。

## 选项

| 选项 | 短选项 | 长选项 | 说明 |
|--------|-------|------|-------------|
| Old commit | | `--old <COMMIT>` | 指定比较的“旧”侧。使用 `--staged` 时默认为 HEAD，否则默认为索引。 |
| New commit | | `--new <COMMIT>` | 指定“新”侧。需要 `--old`。与 `--staged` 冲突。 |
| Staged | | `--staged` | 比较 HEAD 和索引（已暂存更改）。与 `--new` 冲突。 |
| 修订 | | 位置参数 | 最多两个前导修订，Git 风格：`diff A`（A 对工作树）、`diff A B`（≡`A..B`）、`diff A..B`、`diff A...B`（merge-base 对 B）、`diff --staged A`（A 对索引）。给出 `--old`/`--new` 时不做修订解释。 |
| Pathspec | | 位置参数 | 一个或多个文件或目录，用于限制 diff（位于修订之后；用 `--` 强制按路径解读）。支持精确文件、目录前缀、默认通配符，以及 `:(top)` / `:(exclude)` / `:(icase)` / `:(literal)` / `:(glob)` magic。`--` 前的路径须存在，或带有通配符语法 / 已支持的 pathspec magic；`--` 后的路径原样接受。 |
| Algorithm | | `--algorithm <name>` | Diff 算法。仅 `histogram` 后端已实现；`myers` 与 `myersMinimal` 可被解析，但会在扫描前显式失败。 |
| Output file | | `--output <FILENAME>` | 将人类可读输出写入文件而不是 stdout。在 `--json` 模式中忽略。 |
| Name only | | `--name-only` | 只显示已更改文件名。 |
| Name status | | `--name-status` | 显示已更改文件名和状态字母（适用时为 A/D/M/R/T/U）。 |
| Raw | | `--raw` | 输出 Git 形状的 `:<oldmode> <newmode> <oldid> <newid> <status>\t<path>` 记录。ID 缩写为 7 字符；工作树或缺失侧为零。重命名带 `R<score>` 与旧/新路径。任意路径名请配合 `-z`。 |
| Word diff | | `--word-diff[=<mode>]` | 以单词粒度重新渲染补丁。MODE 为 `plain`（默认；删除词 `[-…-]`、新增词 `{+…+}`）、`color`（终端着色、无括号）、`porcelain`（每 token 一行，` `/`-`/`+` 前缀，`~` 标记换行）或 `none`（常规补丁）。词按空白分隔。须写作 `--word-diff` 或 `--word-diff=<mode>`。 |
| 单词着色 | | `--color-words` | 裸参数是 `--word-diff=color` 的简写。自动颜色策略下，即使 stdout 被重定向也输出单词颜色；显式全局 `--color=never` 仍禁止 ANSI。带 regex 的 `--color-words=<regex>` 与 `--word-diff-regex` 尚未实现，会显式失败而非静默忽略。 |
| Numstat | | `--numstat` | 以机器友好的制表符分隔格式显示插入/删除数量。 |
| Stat | | `--stat` | 显示带 +/- 条形图的 diffstat 摘要。 |
| 紧凑摘要 | | `--compact-summary` | 输出 `--stat`，并追加 `(new)`、`(gone)` 及 `(+x)`/`(+l)` 等可执行位/符号链接元数据。隐含 `--stat`。 |
| Diff 过滤 | | `--diff-filter=<FILTER>` | 按 A/C/D/M/R/T/U/X/B 状态筛选。大写为包含、小写为排除；`*` 表示 all-or-none。空值或无效字符在扫描/输出前以 `LBR-CLI-002` 失败。 |
| 字符串 pickaxe | `-S <STRING>` | | 仅保留 STRING 的非重叠出现次数在两侧发生变化的 file pair；次数相同即使位置变化也不匹配。textconv 激活时搜索转换结果，否则搜索原始字节（包括二进制内容）。空 STRING 不匹配。与 `-G` 互斥。 |
| 正则 pickaxe | `-G <REGEX>` | | REGEX 匹配 hunk 的新增或删除内容时保留 file pair（不匹配上下文或 patch 头）。使用 Rust `regex` 方言（线性时间，不支持 look-around/backreference）。无效表达式在扫描/输出前以 `LBR-CLI-002` 失败。与 `-S` 互斥。 |
| 上下文行数 | `-U<n>` | `--unified=<n>` | patch 中每处变更周围的上下文行数（默认 3；未传该标志时 `diff.context` 配置默认生效——严格 local → global → system 级联、非负 Git 整数、可带 `k`/`m`/`g` 的 1024 倍后缀；无效或溢出值在进度与 diff 输出前以 `LBR-CLI-002` fail-closed）。只改变周围上下文、不改变 `+`/`-` 行，故 `--stat`/`--name-only`/`--numstat` 计数不受影响；`--json` 的 hunk 范围与行数组随 `<n>` 变化。 |
| 忽略空白 | `-w` | `--ignore-all-space` | 比较行时忽略所有空白。仅空白的变更不再报告（若这是文件唯一的变更则该文件不出现）；上下文行取新一侧。受影响文件会重新 diff，故 `--stat`/`--name-only`/`--numstat`/JSON 都反映忽略空白后的结果。遵循 `-U<n>`。 |
| 忽略空白数量 | `-b` | `--ignore-space-change` | 只忽略空白*数量*的变化：连续空白视为单个空格、忽略行尾空白，但空白的有无仍然重要（`a  b` 等于 `a b`；`a b` 仍不同于 `ab`）。重新 diff/丢弃行为同 `-w`。二者同时给出时 `-w` 优先。 |
| 忽略行尾空白 | | `--ignore-space-at-eol` | 只忽略行尾空白变化；前导与内部空白精确比较。重新 diff/丢弃行为同 `-w`。与 `-w`/`-b` 组合时后者优先。 |
| 忽略行尾回车 | | `--ignore-cr-at-eol` | 忽略行尾回车：CRLF↔LF-only 的变更被丢弃；行尾空格或行中 `\r` 的变更仍显示。最弱空白标志——被 `-w`/`-b`/`--ignore-space-at-eol` 各自涵盖且组合时后者优先。（对 Git 的近似：比较前剥除全部尾部 CR，而非 Git 非传递的「允许各留一个 CR」规则——仅病态多 CR 结尾有别，日常 CRLF 场景与 Git 一致。） |
| 忽略空白行 | | `--ignore-blank-lines` | 忽略全为空白（真正空）行的变更：仅由增删空行构成的变更不报告（若新增/删除的文件内容全为空行，仍以零计数列出该文件），而紧邻真实编辑的空行则完整显示。重新 diff 受影响文件（故 `--stat`/`--name-only`/`--numstat`/JSON 反映结果）；遵循 `-U<n>`。与空白标志（`-w`/`-b`/`--ignore-space-at-eol`/`--ignore-cr-at-eol`）复合：任一空白标志下全空白行即视为空行（匹配 Git `xdl_blankline`）。 |
| Shortstat | | `--shortstat` | 只显示 `--stat` 的汇总行（文件数/插入/删除），零项省略对应子句。 |
| Summary | | `--summary` | 显示创建/删除、已检测重命名及 mode 变更的精简摘要（默认开启重命名检测；`diff.renames=false` 或 `--no-renames` 可关闭）。纯内容修改不产生行。 |
| No patch | `-s` | `--no-patch` | 抑制 patch（diff 主体）。与 `--exit-code` 组合做状态检查。 |
| 空白检查 | | `--check` | 不输出 diff，而是对新增行的安全问题告警：尾随空白、indent 中 space-before-tab、遗留冲突标记、EOF 新增空行。打印 `<path>:<line>: <message>`，发现即退出码 2；优先于其他输出模式。 |
| 反向 | `-R` | `--reverse` | 交换两侧，使新增变删除、删除变新增（即可撤销该变更的 patch）。 |
| 文本 | `-a` | `--text` | 把所有文件按文本处理：即便检测为二进制（任一侧含 NUL 字节，或非 UTF-8 内容）也输出内容 diff，抑制 “Binary files … differ” 行。Libra 的 diff 基于文本，故非 UTF-8 改动若经 lossy-UTF-8 转换后相同，仍显示 “Binary files … differ”。 |
| 二进制 patch | | `--binary` | 对二进制文件输出 `GIT binary patch`（两个方向的 base85 `literal` 块），而非 “Binary files … differ”；隐含 `--full-index`。该补丁有效且可 apply，但压缩字节与 Git 不完全一致（Libra 用不同的 zlib，且始终输出 `literal` 而非 Git 的 literal/delta 取小）。 |
| 完整 index | | `--full-index` | 在 patch 的 `index` 行显示完整 pre-image/post-image 对象 ID，也覆盖普通二进制 marker diff。 |
| 源前缀 | | `--src-prefix=<PREFIX>` | 替换 patch 源前缀；CLI 值覆盖 `diff.srcPrefix`，如需尾随斜杠须自行包含。 |
| 目标前缀 | | `--dst-prefix=<PREFIX>` | 替换 patch 目标前缀；CLI 值覆盖 `diff.dstPrefix`，`-R` 会交换最终前缀对。 |
| 禁用外部 diff | | `--no-ext-diff` | 本次运行禁用外部 diff 驱动，强制使用内建引擎。 |
| 外部 diff | | `--ext-diff` | 允许已配置的外部 diff 驱动（`diff.external`）生成每个文件的 patch（配置后默认即启用，此 flag 为 `--no-ext-diff` 的显式反面）。 |
| 对移动行着色 | | `--color-moved[=<mode>]` | 在彩色输出中，对“一处删除、另一处新增”的行用独立颜色着色（删除→粗体洋红，新增→粗体青）。裸 `--color-moved` 与块模式（`default`/`zebra`/`blocks`/`dimmed-zebra`）被接受但以 `plain` 近似——所有移动行都着色；Libra 不实现 Git 保守的移动块显著性/zebra 条带。`--color-moved=no` / `--no-color-moved` 关闭。仅影响彩色输出（终端或 `--color=always`）。 |
| 不对移动行着色 | | `--no-color-moved` | 不对移动行单独着色（默认行为；countermand 先前的 `--color-moved`）。 |
| 检测重命名 | `-M[<n>]` | `--find-renames[=<n>]` | 检测重命名：内容足够相似的 删除+新增 文件对会合并为一条重命名（`similarity index N%` / `rename from`/`rename to`，name-status/numstat/summary 表面显示 `R<score>` / `old => new`）。与 Git 一致，默认按 50% 阈值开启。裸 `-M` 使用 50%；`-M<n>` / `-M<n>%` / `--find-renames=<n>` 设定阈值（裸整数与 Git 一样读作 `0.<digits>`，故 `-M5` 为 50%、`-M100%` 仅精确匹配）。精确阶段按 blob id 建索引；任一剩余侧超过 1000 个文件时保留精确重命名、跳过二次方非精确阶段并告警。相似度对真实内容与 Git 一致（分块哈希不同，故专门构造的哈希碰撞输入可能不同）；同时重命名多个文件时，所选的 old/new 配对可能与 Git 不同。`diff.renames=true|false|copies` 通过严格 local → global → system 级联生效；真值或未设置时使用 50%，`copies`/`copy` 退化为普通重命名检测（Libra 无 `-C`），`--no-renames`/`-M` 恒胜。无效值在进度或 diff 输出前以 `LBR-CLI-002` fail-closed；该 porcelain-only 配置被 `diff-tree`、`diff-index`、`diff-files` 忽略。裸 `-M`/`--find-renames` 后不能紧跟 pathspec——请置于该 flag 之前或 `--` 之后。 |
| 不检测重命名 | | `--no-renames` | 关闭重命名检测，覆盖默认行为、`diff.renames` 与先前的 `-M`/`--find-renames`。 |
| 相对路径 | | `--relative[=<path>]` | 将 diff 限定到某个目录并显示相对该目录的路径：带值时 `<path>` 相对当前目录解析，裸 `--relative` 用当前目录。该目录之外的文件被排除，显示路径剥离该前缀（`--stat` 与 JSON 同样如此）。配合外部 `diff.external` 驱动时，文件集仍按前缀过滤，但不对驱动的 verbatim 输出剥离前缀。 |
| 不用相对路径 | | `--no-relative` | 显示完整的仓库根相对路径。这是 Libra 的默认行为；为 Git 兼容而接受，并优先于 `--relative`（两者同时给出时关闭相对输出）。 |
| 不用 indent 启发式 | | `--no-indent-heuristic` | 禁用 hunk 边界的 indent 启发式。接受式 no-op：Libra 的 diff 不使用 Git 的 indent 启发式。（Git 的 `--indent-heuristic` 不支持。） |
| Textconv | | `--textconv` | 运行 textconv 过滤器使内容可读地 diff：文件的 `diff=<driver>` 属性来自 Git/Libra attributes 来源，指向一个配置了 `diff.<driver>.textconv` 命令的 driver 时，diff 前先用该命令转换两侧内容。与 Git 一致，`diff` 默认开启；此 flag 为 `--no-textconv` 的显式反面。生成的补丁用于阅读，不可 apply。textconv 命令失败为致命错误；`--check` 或 `diff.external` 激活时不应用。 |
| 不用 textconv | | `--no-textconv` | diff 原始内容，跳过 textconv 过滤器（countermand 先前的 `--textconv`）。 |
| Exit code | | `--exit-code` | 仍打印 diff，但存在差异时退出码为 1（否则 0）。区别于 `--quiet`，不抑制 diff。 |
| NUL 输出 | `-z` | `--null` | 对 `--raw`/`--name-only`/`--name-status`/`--numstat` 用 NUL 终止每条记录。raw 重命名与 name-status 字段拆为独立 NUL 字段；其他模式不受影响。 |
| JSON | | `--json` | 输出结构化 JSON。 |
| Quiet | | `--quiet` | 抑制 stdout；存在差异时退出码为 1，否则为 0。与 `--output` 组合时，文件仍会被写入。 |

### 选项细节

**`--old` / `--new`**

比较两个特定提交。指定 `--new` 时也必须指定 `--old`：

```bash
# 比较两个提交
libra diff --old HEAD~3 --new HEAD

# 比较标签和 HEAD
libra diff --old v1.0 --new HEAD
```

**`--staged`**

显示已为下一次提交暂存的内容：

```bash
libra diff --staged
libra diff --staged src/
```

**`--algorithm`**

选择 diff 算法。Histogram 是当前唯一实现的后端；其余可解析名称会在工作树扫描前以 `LBR-CLI-002` 显式失败：

```bash
libra diff --algorithm histogram
```

**`--output`**

将 diff 输出写入文件。适合保存 diff 以供评审：

```bash
libra diff --output changes.patch
libra diff --staged --output staged.diff
```

**摘要格式：**

```bash
# 仅文件名
libra diff --name-only

# 文件名和状态字母
libra diff --name-status
# Output: M	src/main.rs
#         A	src/new_file.rs

# 机器友好的数量
libra diff --numstat
# Output: 5	2	src/main.rs

# 可视条形图
libra diff --stat
# Output:  src/main.rs | 7 +++++--
```

**Pickaxe 过滤：**

```bash
# STRING 出现次数发生变化的文件
libra diff -S'old_api' --name-only

# 增删行匹配正则的文件
libra diff -G'unsafe\(' --name-status
```

`-S` / `-G` 可与 pathspec、`--diff-filter`、评审输出模式、JSON 和退出码检查组合。过滤发生在已配置 external diff 驱动执行之前，因此驱动只处理命中的 file pair。textconv 默认开启；`-S` 复用已转换的两侧内容，不会再次执行命令；传 `--no-textconv` 可搜索原始字节。在 AI 的 `run_libra_vcs` 中，除非同时提供 `--no-textconv --no-ext-diff`，pickaxe 仍需人工批准，因为这些配置过滤器可能执行 shell 命令。

## 常用命令

```bash
# 显示未暂存更改
libra diff

# 显示已暂存更改
libra diff --staged

# 比较两个提交
libra diff --old HEAD~1 --new HEAD

# 显示子目录的 diff 统计
libra diff --stat src/

# 输出 NUL-safe 的对象/mode 元数据，并筛选评审路径
libra diff --raw -z
libra diff --name-only --diff-filter=AM

# 按 literal 次数或增删行正则搜索 file pair
libra diff -S'old_api' --name-only
libra diff -G'handler_v[0-9]+' --name-status

# 显示完整 patch 对象 ID，并指定路径前缀
libra diff --full-index --src-prefix=old/ --dst-prefix=new/

# 使用不同的上下文行数（0，或多于默认的 3）
libra diff -U0
libra diff --unified=5 src/main.rs

# 忽略仅空白的变更（重新缩进不会显示）
libra diff -w

# 只忽略空白数量的变化（a  b == a b）
libra diff -b

# 忽略仅由空白行构成的变更
libra diff --ignore-blank-lines

# 将 diff 保存到文件
libra diff --output my.patch

# 面向代理的 JSON 输出
libra --json diff --staged
```

## 配置默认值

未提供 CLI 前缀覆盖时，patch 路径前缀按严格 local → global → system
级联读取。`diff.noPrefix=true` 移除两侧前缀；否则
`diff.mnemonicPrefix=true` 会按 index–worktree、commit–worktree、commit–index、
commit–commit 比较分别选择 `i/`/`w/`、`c/`/`w/`、`c/`/`i/`、`c/`/`c/`，`-R`
交换前缀顺序。两个布尔均未启用时，`diff.srcPrefix` 与 `diff.dstPrefix` 分别替换 `a/` 与 `b/`；值按
原样使用，如需斜杠应自行包含。优先级为 `noPrefix` → `mnemonicPrefix` → 自定义
前缀 → `a/`/`b/`。无效布尔或不可读的 local/global 配置在进度/输出前失败；
不可读或不支持的 system scope 按既有配置契约跳过。前缀改写发生在
`--relative` 之后，作用于内建 rename/binary 与 `commit -v` patch，不改写外部
diff 驱动的 verbatim 输出。与 Git 一样，`commit -v` 始终使用内建 staged diff，
忽略 `diff.external`。`--src-prefix` 与 `--dst-prefix` 分别覆盖对应配置值；两者
同时给出时，不读取无关的前缀默认配置。`-R` 交换最终前缀对。

## 人类可读输出

支持的输出模式：

- 默认 unified diff（检测到终端时带 ANSI 颜色）
- `--raw`（mode/object/status 记录；任意路径名请配合 `-z`）
- `--name-only`
- `--name-status`
- `--numstat`
- `--stat`
- `--compact-summary`（`--stat` 加 new/gone/可执行位/符号链接注记）
- `--shortstat`（只有 `--stat` 的汇总行，零项子句省略）
- `--summary`（精简的 create/delete/rename/mode-change 摘要；重命名默认检测、可显式关闭）
- `-s` / `--no-patch` 抑制 patch 主体（用于仅状态检查）
- `--diff-filter=<FILTER>` 将全部输出、JSON 与退出码判断限制到所选变更类型
- `-z` / `--null` 对 `--raw`/`--name-only`/`--name-status`/`--numstat` 用 NUL 终止记录（raw 重命名与 name-status 路径字段拆为独立 NUL 字段）
- `--check` 对新增行检测尾随空白、indent 中 space-before-tab、遗留冲突标记和 EOF 新增空行，打印 `<path>:<line>: <message>`，发现即退出码 2
- `-R` / `--reverse` 交换两侧得到反向 diff（新增↔删除）
- `-a` / `--text` 把所有文件按文本处理：即便检测为二进制也输出内容 diff（抑制 “Binary files … differ”）；`--binary` 则对二进制文件输出 `GIT binary patch`
- `--no-ext-diff` 本次运行禁用外部 diff 驱动，强制内建引擎；`--ext-diff` 允许已配置的 `diff.external` 外部驱动生成 patch（按 Git GIT_EXTERNAL_DIFF 协议，仅 patch 输出模式；`--stat`/name/numstat/`-s`/`--check` 绕过）
- `--exit-code` 仍打印 diff，但存在差异时退出码为 `1`
- `--quiet` 抑制 stdout，并用退出码 `1` 表示存在差异

默认情况下，这些偏机器输出的 diff 模式只报告 tracked/index 与工作树之间的差异。未跟踪文件（包括未跟踪的 `.libraignore`）不会出现，也不会让 `--quiet` 或 `--exit-code` 失败。

未合并冲突路径在默认工作区 diff 中以 `diff --cc <path>` 头显示。

`--output <file>` 将人类可读输出写入文件。在 `--quiet` 模式下仍会写入文件，但存在差异仍返回退出码 `1`。在 `--json` 模式下，该标志会被忽略，输出始终发送到 stdout。

连接到终端时，输出会自动分页。

## 结构化输出（JSON）

```json
{
  "ok": true,
  "command": "diff",
  "data": {
    "old_ref": "index",
    "new_ref": "working tree",
    "files": [
      {
        "path": "tracked.txt",
        "status": "modified",
        "insertions": 1,
        "deletions": 0,
        "hunks": [
          {
            "old_start": 1,
            "old_lines": 1,
            "new_start": 1,
            "new_lines": 2,
            "lines": [" tracked", "+updated"]
          }
        ]
      }
    ],
    "total_insertions": 1,
    "total_deletions": 0,
    "files_changed": 1
  }
}
```

`status` 字段是 `added`、`deleted`、`modified` 或 `renamed` 之一。`renamed` 条目
（默认检测，可显式关闭；`-M`/`--find-renames` 用于设定阈值）额外带 `rename_from`（原路径；`path` 为新名）
与 `similarity`（相似度指数，整数百分比），例如：

```json
{
  "path": "src/new.txt",
  "status": "renamed",
  "rename_from": "src/old.txt",
  "similarity": 90,
  "insertions": 1,
  "deletions": 1,
  "hunks": [ /* ... */ ]
}
```

二进制文件（除非 `--text`）带 `binary` 为 `[old_size, new_size]` 字节数对，其 `insertions`/`deletions` 为 `0`、`hunks` 为空。

`old_ref` 和 `new_ref` 字段表示比较了什么（例如 `"index"`、`"working tree"`、`"HEAD"` 或提交引用）。

## 设计理由

### 位置修订参数与 `--old` / `--new`

Git 风格的位置修订已支持：`libra diff A`（A 对工作树）、`libra diff A B`（等价 `A..B`）、`libra diff A...B`（merge-base(A,B) 对 B）、`libra diff --staged A`（A 对索引）。歧义处理与 Git 一致：`--` 之后一律为路径；`--` 之前既是修订又是现存文件的记号报 `ambiguous argument` 错，两者皆非的记号报 `unknown revision or path not in the working tree`（glob pathspec 如 `*.c` 豁免）。这些错误退出 129（`LBR-CLI-002`/`LBR-CLI-003`，Libra 的 CLI 错误约定；Git 此处为 128）。超过两个修订被拒绝（Git ≥2.38 的 merge combined-diff 形态为 declined）。

Libra 专有的具名标志（`--old`、`--new`）仍是无歧义的编程形式——给出任一时，所有位置参数都保持 pathspec、完全不做修订解释。对以编程方式构造命令的 AI 代理尤其有价值：每种意图只有一种表达方式，无重名风险。

### 为什么 histogram 是默认算法？

Git 出于历史原因默认使用 Myers 算法。Libra 当前只接入 histogram 后端，以获得可读的源代码 diff。解析器为未来兼容保留 `myers` 与 `myersMinimal` 名称，但执行会显式拒绝，避免它们成为静默 no-op。

### `--cached` 别名

`--cached` 已作为 Git 兼容的可见别名被接受，等价于规范拼写 `--staged`（与 `libra status`、`libra restore --staged` 的术语一致）。

### 为什么 `--new` 要求 `--old`？

允许只有 `--new` 而没有 `--old` 会产生模糊比较（new 与什么比较？）。当指定 `--new` 时要求 `--old`，让比较显式且可预测。对于与 HEAD 比较的常见场景，请使用 `--staged`。

### `--word-diff` 与 `--color-words`

`--word-diff[=<mode>]` 已支持（见上方选项）：以单词粒度重新渲染补丁，支持 `plain`（默认）、`color`、`porcelain`、`none` 模式，词按空白分隔。裸 `--color-words` 已作为 color 模式简写实现，并在自动颜色策略下对重定向 stdout 仍启用单词颜色；显式全局 `--color=never` 优先。带 regex 的 `--color-words=<regex>` 与自定义 `--word-diff-regex` 暂未实现并会显式失败。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 未暂存更改 | `diff`（默认） | `diff`（默认） | `jj diff`（显示所有未提交更改） |
| 已暂存更改 | `--staged` | `--staged` / `--cached` | N/A（无暂存区） |
| 两个提交 | `--old <A> --new <B>` | `<A> <B>` 或 `<A>..<B>` | `--from <A> --to <B>` |
| Pathspec 过滤 | `<pathspec>...` | `-- <pathspec>...` | `<paths>...` |
| 算法 | `--algorithm=histogram`（唯一已实现后端；其他可解析名称失败） | `--diff-algorithm`（patience/histogram/myers/minimal） | N/A（使用内部算法） |
| 输出到文件 | `--output <file>` | `--output <file>` | N/A（使用 shell redirect） |
| 仅名称 | `--name-only` | `--name-only` | `--name-only` |
| 名称和状态 | `--name-status` | `--name-status` | N/A |
| 数字统计 | `--numstat` | `--numstat` | `--stat`（组合） |
| Stat 摘要 | `--stat` | `--stat` | `--stat` |
| 短统计 | `--shortstat` | `--shortstat` | N/A |
| Summary | `--summary` | `--summary` | `--summary` |
| Raw 元数据 | `--raw`（支持 `-z`） | `--raw`（支持 `-z`） | N/A |
| 变更过滤 | `--diff-filter=<FILTER>` | `--diff-filter=<FILTER>` | N/A |
| 字符串 pickaxe | `-S <STRING>` | `-S <STRING>` | N/A |
| 正则 pickaxe | `-G <REGEX>`（Rust regex 方言） | `-G <REGEX>` | N/A |
| 紧凑 stat | `--compact-summary` | `--compact-summary` | N/A |
| 完整 patch ID | `--full-index`（`--binary` 隐含） | `--full-index` | N/A |
| Patch 前缀 | `--src-prefix` / `--dst-prefix` | `--src-prefix` / `--dst-prefix` | N/A |
| 抑制 patch | `-s` / `--no-patch` | `-s` / `--no-patch` | N/A |
| 退出码 | `--exit-code` | `--exit-code` | N/A |
| NUL 终止输出 | `-z` / `--null`（raw/name/numstat） | `-z` | N/A |
| 空白检查 | `--check`（尾随空白 / space-before-tab / 冲突标记 / blank-at-eof） | `--check` | N/A |
| 反向 diff | `-R` / `--reverse` | `-R` | N/A |
| 按文本处理 | `-a` / `--text`（强制二进制文件按内容 diff） | `-a` / `--text` | N/A |
| Word diff | `--word-diff[=<mode>]` / 裸 `--color-words`（不含带 regex 简写/`--word-diff-regex`） | `--word-diff` / `--color-words` | N/A |
| Binary diff（二进制 patch） | `--binary`（有效可 apply；压缩字节与 Git 不同） | `--binary` | N/A |
| 上下文行数 | `-U<n>` / `--unified=<n>`（默认 3） | `-U<n>` / `--unified=<n>` | `--context <n>` |
| 忽略空白 | `-w` / `--ignore-all-space` | `-w` / `--ignore-all-space` | N/A |
| 忽略空白数量 | `-b` / `--ignore-space-change` | `-b` / `--ignore-space-change` | N/A |
| 忽略行尾空白 | `--ignore-space-at-eol` | `--ignore-space-at-eol` | N/A |
| 忽略行尾回车 | `--ignore-cr-at-eol` | `--ignore-cr-at-eol` | N/A |
| 忽略空白行 | `--ignore-blank-lines` | `--ignore-blank-lines` | N/A |
| 颜色 | 自动（终端检测） | `--color` / `--no-color` | `--color` / `--no-color` |
| 禁用外部 diff | `--no-ext-diff`（禁用已配置的 `diff.external` 驱动，强制内建引擎） | `--no-ext-diff` | N/A |
| 外部 diff 工具 | `diff.external` + `--ext-diff` / `--no-ext-diff`（GIT_EXTERNAL_DIFF 协议；仅 patch 输出） | `diff.external` + `--ext-diff` / `--no-ext-diff` | `--tool <name>` |
| Quiet（仅退出码） | `--quiet` | `--quiet` | N/A |
| JSON 输出 | `--json` | 不支持 | N/A |
| Rename 检测 | `-M[<n>]` / `--find-renames[=<n>]` 加 `diff.renames`（默认 true；相似度对真实内容与 Git 一致） | `-M` / `--find-renames`、`diff.renames` 默认 true | 自动 |
| 移动行着色 | `--color-moved[=<mode>]` / `--no-color-moved`（plain 语义；块模式以 plain 近似） | `--color-moved[=<mode>]` | N/A |
| Textconv | `--textconv` / `--no-textconv`（默认开启；Git/Libra attributes 的 `diff=<driver>` + `diff.<driver>.textconv`） | `--textconv` / `--no-textconv` | N/A |
| Copy 检测 | 不支持 | `-C` / `--find-copies` | N/A |
| Three-dot diff | `<A>...<B>`（从 merge base 起） | `<A>...<B>`（merge base） | N/A |

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 仓库外部 | `LBR-REPO-001` | 128 |
| 无效修订 | `LBR-CLI-003` | 129 |
| 无效 `-G` 正则表达式 | `LBR-CLI-002` | 129 |
| 无法读取索引或对象存储 | `LBR-REPO-002` | 128 |
| 无法读取文件 | `LBR-IO-001` | 128 |
| 无法写入输出文件 | `LBR-IO-002` | 128 |
