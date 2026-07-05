# `libra for-each-ref`

列出本地 refs，支持过滤和自定义格式。

> 状态：公开 CLI，部分 Git 兼容。该命令枚举存储在 Libra SQLite-backed ref 模型中的 references。它覆盖本地分支、remote-tracking branches、tags 和 `--points-at` 过滤。它不读取 `.git/refs` 或 `packed-refs`。

## 概要

```sh
libra for-each-ref [--heads] [--tags] [--remotes] [--all] [--format=<format>] [--sort=<key>] [--count=<n>] [--points-at=<object>] [--shell | --perl | --python | --tcl] [<pattern>...]
```

## 说明

`libra for-each-ref` 枚举仓库中存储的 refs（branches、tags 和 remote-tracking refs），并打印每个 ref 的对象哈希和名称。使用 `--heads`、`--tags` 或 `--remotes` 可限制到一个命名空间；默认是 `--all`。

位置参数 `<pattern>` 会作为全限定 ref 名（例如 `refs/heads/main`）上的 substring filters。只有名称匹配、包含或以至少一个 pattern 结尾的 refs 会被纳入。

使用 `--points-at <object>` 可保留指向已解析对象的 refs。Annotated tags 同时匹配其 tag object 和 peeled target commit，匹配 Git 常见的 `for-each-ref --points-at HEAD` 行为。

`--format` 选项接受一个简单 atom 语言。支持的 atoms：

| Atom | 值 |
|---|---|
| `%(refname)` | 完整 ref 名，例如 `refs/heads/main` |
| `%(refname:short)` | 短 ref 名（去掉 namespace prefix），例如 `main` |
| `%(refname:lstrip=N)` | 去掉 `N` 个开头路径组件的 ref 名（`N<0` 保留最后 `|N|` 个） |
| `%(refname:rstrip=N)` | 去掉 `N` 个结尾路径组件的 ref 名（`N<0` 保留前 `|N|` 个） |
| `%(objectname)` | ref 指向的对象哈希 |
| `%(objectname:short)` | 缩写对象哈希（7 字符） |
| `%(objectname:short=N)` | 缩写对象哈希到 `N` 字符（不超过全长） |
| `%(objecttype)` | 对象类型：`commit`、`tag`、`tree` 或 `blob` |
| `%(*objectname)` | annotated tag 解引用到的对象（peeled target）；非 tag ref 为空 |
| `%(*objectname:short)` | 缩写解引用对象哈希（7 字符）；非 tag ref 为空 |
| `%(*objecttype)` | 解引用对象的类型（例如 `commit`）；非 tag ref 为空 |
| `%(*objectsize)` | 解引用对象的字节大小；非 tag ref 为空 |
| `%(objectsize)` | ref 直接指向对象的字节大小（annotated tag 为 tag object，不是 peeled commit） |
| `%(HEAD)` | 如果 ref 是当前 checkout 的分支则为 `*`，否则为空格 |
| `%(upstream)` | 分支的 upstream tracking ref（例如 `refs/remotes/origin/main`）；没有时为空 |
| `%(upstream:short)` | 去掉 `refs/remotes/` prefix 的 upstream ref（例如 `origin/main`） |
| `%(push)` | 分支的 push tracking ref。Push remote 依次遵循 `branch.<name>.pushRemote`、`remote.pushDefault`、`branch.<name>.remote`；没有时为空 |
| `%(push:short)` | 去掉 `refs/remotes/` prefix 的 push ref |
| `%(symref)` | 对 symbolic ref（例如 `refs/remotes/<remote>/HEAD`），为其指向的完整 ref 名；普通 ref 为空 |
| `%(symref:short)` | 去掉 namespace prefix 的 symbolic-ref target（例如 `origin/main`）；普通 ref 为空 |
| `%(symref:lstrip=N)` / `%(symref:rstrip=N)` | 去掉 `N` 个开头/结尾路径组件的 symbolic-ref target（`N<0` 保留最后/最前 `\|N\|` 个）；普通 ref 为空 |
| `%(worktreepath)` | checkout 了该 ref 的 worktree 绝对路径；否则为空。Libra worktrees 共享一个 HEAD，因此 checked-out branch 是当前 HEAD branch，路径是运行命令的当前 worktree — 对单 worktree 仓库与 git 兼容。 |
| `%(subject)` | ref 对象 message 的第一行（commit 或 annotated-tag message）；tree/blob 为空 |
| `%(contents)` | commit/annotated-tag 对象的完整 message |
| `%(contents:subject)` | 与 `%(subject)` 相同 |
| `%(body)` / `%(contents:body)` | Message body — 第一段空行之后的全部内容 |
| `%(authorname)` | Commit author name（非 commit refs 例如 annotated tags 为空） |
| `%(authoremail)` | Commit author email，带尖括号（例如 `<a@example.com>`）；非 commit refs 为空 |
| `%(committername)` | Commit committer name；非 commit refs 为空 |
| `%(committeremail)` | Commit committer email，带尖括号；非 commit refs 为空 |
| `%(taggername)` | Annotated-tag tagger name；非 tag refs（lightweight tags 和 commits）为空 |
| `%(taggeremail)` | Annotated-tag tagger email，带尖括号；非 tag refs 为空 |
| `%(authordate)` | Git 默认格式的 commit author date；非 commit refs 为空 |
| `%(committerdate)` | Git 默认格式的 commit committer date；非 commit refs 为空 |
| `%(taggerdate)` | Git 默认格式的 annotated-tag tagger date；非 tag refs 为空 |
| `%(creatordate)` | Ref 创建日期 — commit/lightweight tag 使用 committer date，annotated tag 使用 tagger date |
| `%(authordate:<fmt>)` / `%(committerdate:<fmt>)` / `%(taggerdate:<fmt>)` / `%(creatordate:<fmt>)` | 以所选格式输出相同日期（见下方日期格式说明） |
| `%(tree)` / `%(tree:short)` | Commit 的 tree id（完整 / 7 字符）；非 commit refs 为空 |
| `%(parent)` / `%(parent:short)` | Commit 的 parent ids，以空格分隔（完整 / 7 字符）；root commit 或非 commit ref 为空 |
| `%(numparent)` | Commit 的 parent 数量；非 commit refs 为空 |
| `%(color:<spec>)` | ANSI color/attribute escape（例如 `%(color:red)`、`%(color:bold green)`、`%(color:reset)`），只在启用颜色时输出（`--color=always`，或 `auto` 且连接终端）；`--color=never`/`NO_COLOR` 下为空（在 `--shell` 等模式下仍像其他 atom 一样被 quote）。spec 是空格分隔列表：最多两个颜色（前景再背景）加 attributes。支持 8 个基础名称 + `bright<name>`、`default`、256-color indices、`#rrggbb`，以及 `bold`/`dim`/`italic`/`ul`/`blink`/`reverse`/`strike` attributes（支持紧凑 `nobold` 和连字符 `no-bold` negation）。第三个颜色或无法识别的词是 format error。当一行结束时仍启用颜色，会追加 trailing reset（`\x1b[m`，Git 的 `GIT_COLOR_RESET`），避免颜色 bleed 到下一行（在 `--shell` 等模式下它是单独 quoted field）；显式结尾 `%(color:reset)` 不会重复。 |

日期 atoms 使用 Git 默认格式（`Day Mon DD HH:MM:SS YYYY +ZZZZ`），并且像 `libra log` 一样以 UTC（`+0000`）渲染，而不是提交原始时区。日期 atoms 接受 `:<format>` modifier — `%(committerdate:iso)`、`%(authordate:short)`、`%(taggerdate:unix)`、`%(creatordate:relative)` 等。支持格式：`default`、`short`、`iso`/`iso8601`、`iso-strict`/`iso8601-strict`、`rfc`/`rfc2822`、`unix`、`raw` 和 `relative`（git 风格 “… ago”）。`%(creatordate)` 对 commits/lightweight tags 解析为 committer date，对 annotated tags 解析为 tagger date。尚不支持 `local`/`human`/`format:<strftime>` modifiers（它们会回退到默认格式）。

`%(align:<width>[,<position>])` … `%(end)` block 会把包含内容 padding 到 `<width>` 显示列。`<position>` 是 `left`（默认）、`right` 或 `middle`；width 和 position 可按任意顺序作为位置参数给出（`%(align:10,right)` 或 `%(align:right,10)`），也可作为 `width=`/`position=` 键值对给出。内容已达到或超过宽度时保持不变（不截断），align blocks 可嵌套。在 `--shell`/`--perl`/`--python`/`--tcl` 下，block 内容渲染时不 quote，整个 padded block 作为单个字符串字面量 quote 一次（匹配 Git：只有最外层 align block quote；嵌套 block 和 block literals 不单独 quote）。

`%(if[:equals=<v>|:notequals=<v>])` … `%(then)` … [`%(else)` …] `%(end)` conditional 在 `%(if…)` 与 `%(then)` 之间的条件成立时输出 then 分支，否则输出 else 分支（可省略）。普通 `%(if)` 在渲染后的条件去除 whitespace 后非空时为 true；`equals`/`notequals` 比较原始渲染值。Conditionals 可嵌套，也可嵌套在 `%(align)` blocks 中（共享 `%(end)` terminator）。`%(raw)` / `%(raw:size)` 输出原始解压对象内容及其字节大小（大小等于 `%(objectsize)`；`%(raw)` 在 `--shell`/`--python`/`--tcl` 下被拒绝，匹配 Git）。`%(raw)` 支持文本对象（commits、annotated tags）；非 UTF-8（二进制）对象会被拒绝，而不是 lossy transcode。`%(describe[:<opts>])` 对每个 ref 的 commit 运行 `git describe`，支持 `tags`、`abbrev=<n>`、`match=<glob>` 和 `exclude=<glob>` 选项（逗号分隔；未知选项是用法错误，即使没有 ref 匹配也会验证）；没有可达 tag 的 commit 渲染为空字符串。`%(symref)` / `%(symref:short)` / `%(symref:lstrip=N)` / `%(symref:rstrip=N)` 给出 symbolic ref 指向的 target（例如 `refs/remotes/<remote>/HEAD`）；普通 ref 为空。`%(worktreepath)` 给出 checkout 了该 ref 的 worktree 绝对路径（否则为空）；Libra worktrees 共享一个 HEAD，因此 checked-out branch 是当前 HEAD branch，路径是运行命令的当前 worktree — 匹配单 worktree 仓库中的 git。支持 atom 集合已较完整；剩余小众 atom `%(deltabase)` 未实现。

## 选项

| 选项 | 说明 |
|---|---|
| `--heads` | 列出 `refs/heads/` 下的本地分支 refs。 |
| `--tags` | 列出 `refs/tags/` 下的 tag refs。 |
| `--remotes` | 列出 `refs/remotes/` 下的 remote-tracking refs。 |
| `--all` | 列出所有支持的 ref namespaces。未给 namespace 标志时默认如此。 |
| `--format=<format>` | 渲染简单 atoms。支持 atoms：`%(refname)`、`%(refname:short)`、`%(refname:lstrip=N)`、`%(refname:rstrip=N)`、`%(objectname)`、`%(objectname:short)`（7-char）、`%(objectname:short=N)`、`%(objecttype)`、`%(objectsize)`、`%(*objectname)`、`%(*objectname:short)`、`%(*objecttype)`、`%(*objectsize)`、`%(HEAD)`、`%(upstream)`、`%(upstream:short)`、`%(push)`、`%(push:short)`、`%(subject)`、`%(contents)`、`%(contents:subject)`、`%(body)`、`%(contents:body)`、`%(authorname)`、`%(authoremail)`、`%(committername)`、`%(committeremail)`、`%(taggername)`、`%(taggeremail)`、`%(authordate)`、`%(committerdate)`、`%(taggerdate)`、`%(creatordate)`（这四个都接受 `:<format>` modifier，例如 `%(committerdate:iso)`）、`%(tree)`、`%(tree:short)`、`%(parent)`、`%(parent:short)`、`%(numparent)`、`%(color:<spec>)`、`%(raw)`、`%(raw:size)`、`%(describe[:<opts>])`（对每个 ref 运行 `git describe`；选项 `tags`/`abbrev=<n>`/`match=<glob>`/`exclude=<glob>`）、`%(symref)`、`%(symref:short)`、`%(symref:lstrip=N)`、`%(symref:rstrip=N)`（symbolic ref 例如 `refs/remotes/<remote>/HEAD` 指向的 target；普通 ref 为空）。 |
| `--sort=<key>` | 按 `refname`、`objectname`、`version:refname`（别名 `v:refname`；嵌入数字按数值排序，因此 `v1.9` 在 `v1.10` 前）、日期键 — `committerdate`、`authordate` 或 `creatordate` — `objectsize`（ref 对象字节大小），或解引用键 `*objectname` / `*objecttype` / `*objectsize`（annotated tag target object id / type / byte size — 非 tag refs 为空，因此排在前面）排序。日期键会把 annotated tags peel 到 commit；`creatordate` 使用 annotated tag 自身的 tagger date。任意 key 可加 `-` 前缀反转。 |
| `--count=<n>` | 过滤和排序后最多输出 `n` 个 refs。 |
| `--points-at=<object>` | 保留指向该对象的 refs。Annotated tags 也匹配其 peeled target。 |
| `--contains=<commit>` / `--no-contains=<commit>` | 保留（或排除）tip 以 `<commit>` 为祖先的 refs。 |
| `--merged=<commit>` / `--no-merged=<commit>` | 保留（或排除）tip 可从 `<commit>` 到达的 refs（已经 merge 进它）。 |
| `--shell` / `--perl` / `--python` / `--tcl` | 将每个插值字段 quote 为对应语言的字符串字面量，使输出可被 `eval`/source。互斥。 |
| `--exclude=<pattern>` | 不列出匹配 `<pattern>` 的 refs（可重复；应用在位置 include patterns 之后）。 |
| `<pattern>...` | 保留 full name 匹配、包含或以 pattern 结尾的 refs。 |

## 示例

```sh
libra for-each-ref
libra for-each-ref --heads
libra for-each-ref --tags --format='%(refname) %(objectname)'
libra for-each-ref --points-at HEAD --format='%(refname) %(objecttype)'
libra for-each-ref --sort=-refname --count=5
libra for-each-ref --format='%(refname:short) %(committerdate:relative)' --sort=-committerdate
libra --json for-each-ref --remotes
```

## 兼容性

兼容级别为 `partial`。支持 `--contains` / `--no-contains`（过滤 tip 拥有或不拥有给定 commit 作为祖先的 refs），也支持 `--merged` / `--no-merged`（过滤 tip 可从或不可从给定 commit 到达的 refs）和 `--exclude`（丢弃匹配给定 pattern 的 refs，应用在位置 include patterns 之后）。支持的 sort keys 为 `refname`、`objectname`、`version:refname`、日期键 `committerdate` / `authordate` / `creatordate`、`objectsize`（ref 对象字节大小 — 也可作为 `%(objectsize)` atom 使用），以及解引用键 `*objectname` / `*objecttype` / `*objectsize`（annotated tag 的 dereferenced object id / type / byte size — 也可作为 `%(*objectname)` / `%(*objectname:short)` / `%(*objecttype)` / `%(*objectsize)` atoms 使用；非 tag refs 为空，因此排在前面），每个都可用 `-` 前缀反转。支持输出 quoting modes `--shell`、`--perl`、`--python` 和 `--tcl`（互斥）：每个插值字段会包装为对应语言的字符串字面量（atoms 之间的 literal text 和默认 `<oid> <refname>` 分隔符保持未 quote）。`%(align:<width>[,<position>])` … `%(end)` block 会把渲染内容 padding 到列宽（`position` 为 `left`（默认）、`right` 或 `middle`；内容超过宽度时不截断；blocks 可嵌套），`%(if[:equals|:notequals])` … `%(then)` … [`%(else)` …] `%(end)` conditional block 会通过测试条件选择分支（普通 `%(if)` trim whitespace；`equals`/`notequals` 比较原始值；blocks 可嵌套，包括在 align 内）。也支持 `%(tree)`/`%(tree:short)`/`%(parent)`/`%(parent:short)`/`%(numparent)` commit-graph atoms。`%(raw)` / `%(raw:size)` 输出原始解压对象内容及其字节大小（大小等于 `%(objectsize)`；`%(raw)` 在 `--shell`/`--python`/`--tcl` 下拒绝，匹配 Git）。`%(raw)` 支持文本对象（commits、annotated tags）；非 UTF-8（二进制）对象会被拒绝，而不是 lossy transcode。`%(describe[:<opts>])` 对每个 ref 的 commit 运行 `git describe`，支持 `tags`、`abbrev=<n>`、`match=<glob>` 和 `exclude=<glob>` 选项（逗号分隔；未知选项是用法错误，即使没有 ref 匹配也会验证）；没有可达 tag 的 commit 渲染为空字符串。`%(symref)` / `%(symref:short)` / `%(symref:lstrip=N)` / `%(symref:rstrip=N)` 给出 symbolic ref 指向的 target（例如 `refs/remotes/<remote>/HEAD`）；普通 ref 为空。`%(worktreepath)` 给出 checkout 了该 ref 的 worktree 绝对路径（否则为空）；Libra worktrees 共享一个 HEAD，因此 checked-out branch 是当前 HEAD branch，路径是运行命令的当前 worktree — 匹配单 worktree 仓库中的 git。支持 atom 集合已较完整；剩余小众 atom `%(deltabase)` 未实现。Git flat-file ref storage parity 对 Libra 刻意不适用。

## 结构化输出

`--json` 和 `--machine` 返回标准 Libra 信封。`data` 是 entries 数组，每项包含 `refname`、`objectname` 和 `objecttype` 字段，并且对 symbolic refs（例如 `refs/remotes/<remote>/HEAD`）额外包含可选 `symref` 字段（target ref name）。
