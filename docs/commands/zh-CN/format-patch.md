# `libra format-patch`

从提交生成 mbox 格式的 email patch 文件。

## 概要

```bash
libra format-patch [OPTIONS] [<revision>...]
```

## 说明

每封补丁的 mbox 分隔行为 `From <提交 oid> Mon Sep 17 00:00:00 2001`（Git 固定信封时间戳）；`--zero-commit` 时哈希全零。封面信分隔行使用系列末个提交 oid 与同一固定时间戳。

封面信仅当系列中任一提交的原始对象缓冲含非 ASCII 字节时才输出 `MIME-Version`/`Content-Type`/`Content-Transfer-Encoding` 头（Git `make_cover_letter` 规则）；全 ASCII 系列不输出 MIME 头。

封面信还会输出系列 diffstat（从唯一边界提交——最旧补丁的父提交——到末个补丁的树差异）；`--no-stat`/`-p` 只抑制逐补丁 diffstat，系列自根提交开始（无唯一边界）时省略封面信 diffstat；封面信 diffstat 继承补丁的重命名选项（`-M`/`--find-renames`/`--no-renames`，以最后者为准）。

`libra format-patch` 遍历 revision range（`A..B`，或把单个提交视为 `<commit>..HEAD`）；`-1 [commit]` 精确选择一个 commit，`--root [commit]` 则包含 root 及所有可达的非 merge ancestors。多个位置修订取并集，`^<rev>` 排除其可达提交，`A..B` 排除 `A` 并包含 `B`。命令为每个非 merge commit 生成一个 patch 文件（默认以 `--suffix` 命名，默认 `.patch`，除非设置 `--numbered-files`，此时使用裸序号）。除 `--numbered-files` 外，文件名始终带有补零序号前缀，不受 `-n` 或 `-N` 影响。每个 patch 格式化为 mbox message，包含 RFC 2822 headers、plain-text diffstat 和 unified diff。输出兼容 `git am`。

Merge commits 默认跳过。当 revision range 解析为零个提交时，命令以退出码 0 成功结束，不生成 patch。普通模式不输出内容；使用 `--json` 时，payload 中包含 `"patches": []`。仓库没有任何 commit 或 revision 无法解析时仍会报错。若所有 commit 都已在 upstream 中，`--ignore-if-in-upstream` 也会成功返回空 series。

## 选项

| 标志 | 短参数 | 说明 | 默认值 |
|------|--------|------|--------|
| `[<revision>...]` | | 一个或多个修订：单个提交等价 `<commit>..HEAD`；`A..B` 排除 `A`；`^<rev>` 排除其可达提交；多个正向修订取并集 | `HEAD` |
| `-1` | `-1` | 只生成指定 commit；未给 revision 时生成 `HEAD` | false |
| `--root` | | 包含 root commit 和所有可达的非 merge ancestors | false |
| `--output-directory <DIR>` | `-o` | 将 patch 文件写入 `DIR` | 当前目录 |
| `--stdout` | | 将所有 patches 打印到 stdout | false |
| `--numbered` | `-n` | 在 commit patch 的 Subject 行中加入 `[PATCH n/m]` 计数 | auto（多补丁或封面信） |
| `--no-numbered` | `-N` | 不在 commit patch 或 cover-letter 的 Subject 行中加入 `[PATCH n/m]` 计数 | 关闭 |
| `--rfc[=<text>]` | | 在主题前缀中加入 `RFC`（`[RFC PATCH]`）；给出值时用该文本替代（`--rfc=WIP` → `[WIP PATCH]`）。值必须用 `=` 紧跟，避免吞掉版本参数 | 关闭 |
| `--no-rfc` | | 取消此前的 `--rfc`（后写者生效） | 关闭 |
| `--filename-max-length <N>` | | 将输出文件名截断到最多 `N` 字节（包含 `0001-` 前缀与后缀）；必要时可只保留序号，截断点与 Git 一致 | 不限制 |
| `--commit-list-format <FMT>` | | 封面信的提交列表。默认（或 `short`）为 Git 的按作者分组 shortlog（`Name (N):` 加 72 列换行的主题）；`modern` 渲染 `[i/N] subject`；其他值按 pretty-format 解析（可带 `log:` 前缀），支持 `%h`、`%H`、`%s`、`%an`、`%ae`、`%n`、`%%`、`%(count)`/`%(total)`（`%(count)` 按 `%(total)` 位宽补零、自 1 起且不受 `--start-number` 影响）；不含 `%` 且无 `log:` 的字面量以 `'<fmt>' is not a valid format string` 拒绝 | `short` |
| `--relative[=<path>]` | | 将系列限制到某个目录，并从 diffstat 与补丁路径中剥离该前缀。带值时 `<path>` 相对仓库根解析（与 `libra diff --relative` 相对当前目录不同）；裸 `--relative` 使用当前目录。未给 flag 时 `diff.relative=true` 同样生效；`--no-relative` 恢复仓库相对路径（两者以最后者为准）。前缀在仓库内不存在或逃逸仓库（`../outside`）时按 Git 输出空补丁正文（仅头区与提交说明，无 `---`/diffstat/diff），不写任何文件，也不打印仓库外路径 | 关闭 |
| `--all`（不支持） | | 仅为明确缺口而接受：Libra 的版本语法没有 all-refs 选择器，请显式写分支范围（如 `main..topic`） | — |
| `--no-base` | | 不输出 `base-commit:` 尾注，覆盖 `--base`（Libra 没有 `format.useAutoBase` 配置可重置） | 关闭 |
| `--start-number <N>` | | 从 `N` 开始编号 | 1 |
| `--subject-prefix <PREFIX>` | | 在 Subject: 行使用 `PREFIX` 而不是 `PATCH` | `PATCH` |
| `--cover-from-description <MODE>` | | 分支描述如何进入封面信：`default`/`message` 整段写入正文，`subject` 把首段（空格拼接）写入 `Subject:`、其余写入正文，`none` 保留占位符，`auto` 仅在格式化首段不超过 100 字节时用 subject；非法模式即使没有 `--cover-letter` 也拒绝 | `default` |
| `--cover-letter` | | 生成 cover-letter 模板（`0000-cover-letter<suffix>`，或 `--numbered-files` 下的 `0`） | false  覆盖 `format.coverLetter=false`；未给 `--from`/`format.from` 时，封面信 `From:` 默认取执行时提交者身份（`GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`，否则 `user.name`/`user.email`）；补丁仍用提交作者。 `format.coverLetter=true|false|auto` 提供默认值（`auto` 仅在多补丁系列生成），`--commit-list-format` 隐含封面信（`--no-cover-letter` 可关闭）。 |
| `--no-cover-letter` | | 不生成封面信；覆盖 `format.coverLetter` 以及 `--commit-list-format` 隐含的封面信（与 `--cover-letter` 以最后者为准） | — |
| `--thread[=<STYLE>]` | | 添加生成的 `Message-ID`/`In-Reply-To`/`References` headers（默认关闭）。裸 `--thread` 或 `--thread=shallow` 使每封补丁回复封面信/首补丁；`--thread=deep` 使每封补丁回复前一封，`References` 指向线程根 | false |
| `--no-thread` | | 禁用生成的 threading headers；显式 `--in-reply-to` 仍会为每封邮件添加外部父邮件头 | false |
| `format.coverFromDescription`（配置） | | 封面信描述默认模式（取值同 `--cover-from-description`，CLI 优先）。配置值非法时即使 CLI 给出合法模式、即使未启用封面信，也以 `invalid cover from description mode` 拒绝 | `default` |
| `branch.<name>.description`（配置） | | 用该分支描述替换封面信正文的 `*** BLURB HERE ***`。分支选择遵循 Git：无修订或单一裸修订取当前分支，否则取唯一正向修订对应的分支 | — |
| `format.useAutoBase`（配置） | | `true` 等价 `--base=auto`；`whenAble` 仅在能确定唯一上游 merge base 时自动取基线、否则静默省略 base 区块；`false` 关闭。`--no-base` 与显式 `--base` 覆盖配置 | — |
| `format.commitListFormat`（配置） | | 封面信列表默认格式（语法同 `--commit-list-format`，含 `log:`/原子）；命令行选项优先，且与命令行不同，配置本身不隐含封面信；`full` 按无效格式串拒绝 | — |
| `format.thread`（配置） | | `true`/`shallow`（不区分大小写）为浅线程，`deep` 为深线程，`false`/空值关闭；本地层覆盖全局层，同层后写者生效；命令行 flag 覆盖配置，且 `--thread`/`--thread=<style>`/`--no-thread` 以最后者为准 | — |
| `--in-reply-to <MESSAGE_ID>` | | 让第一封邮件回复给定 Message-ID | 无 |
| `--to <ADDRESS>` | | 添加 `To:` header（可重复；多个地址像 git 一样折叠）。放在 MIME headers 之后，应用于每个 patch 和 cover letter | 无 |
| `--cc <ADDRESS>` | | 添加 `Cc:` header（可重复；像 git 一样折叠） | 无 |
| `--no-to` / `--no-cc` | | 抑制 `To:` / `Cc:` headers（Libra 没有可重置的 `format.to`/`format.cc` config） | false |
| `--from[=<IDENT>]` | | 在 `From:` header 中使用 `<IDENT>` 而不是 commit author（裸 `--from` 使用 committer 配置身份）。当它不同于 author 时，原 author 会作为正文内 `From:` 行保留，便于 `git am` 还原 | author  `<IDENT>` 必须是合法 ident 行（`<` 与其后的 `>` 成对；`Name <>` 合法），并在 revision 解析前校验，因此空范围下 `--from=foo` 仍失败；裸 `--from` 使用执行时提交者身份（含 `GIT_COMMITTER_NAME`/`GIT_COMMITTER_EMAIL`）。 未给 CLI flag 时由 `format.from` 提供同值（`true`=提交者身份，`false`=保持作者，其它值=ident 行）。 |
| `--no-from` | | 不改写 `From:` 头；覆盖 `format.from`。`--from` 与 `--no-from` 同时出现时以最后者为准 | 作者 |
| `--reroll-count <N>` | `-v` | 标记为版本 `N`（把 `[PATCH]` 改为 `[PATCH vN]`） | 无 |
| `--signoff` | `-s` | 向每个 commit message 追加 `Signed-off-by` trailer | false |
| `--no-signoff` | | 禁用 signoff，并覆盖 `format.signOff` | false |
| `--notes[=<REF>]` | | 将每个 commit 的 notes 追加到 `---` 行之后、diffstat 之前。裸 `--notes` 使用默认 ref（`refs/notes/commits`）；`--notes=<ref>` 读取 `<ref>`。渲染为 `Notes:`（默认 ref）或 `Notes (<ref>):`，每行缩进四个空格；没有 note 的 commit 保持不变 | off |
| `--attach[=<BOUNDARY>]` | | 生成 multipart/mixed，提交日志在正文、补丁作为独立 `attachment` 部分；可选 boundary 变为 `------------<BOUNDARY>`（空格形式会被当作修订参数，boundary 须用 `=`） | 关闭 |
| `--inline[=<BOUNDARY>]` | | 同 `--attach`，但补丁部分使用 `Content-Disposition: inline`；可选 boundary 规则相同 | 关闭 |
| `--base <COMMIT>` | | 记录 `base-commit:` trailer（以及 base 与 series 之间每个非 merge commit 的 `prerequisite-patch-id:` 行，按 oldest-first），使 `git am --base` 可验证 series 可应用。Trailer 位于最后一个 patch，或 `--cover-letter` 下位于 cover letter。Base 必须是 series 的祖先（否则退出 128）。不支持 `--base=auto`（退出 129）。文本 diff 的 patch-id 匹配 `git patch-id --stable`；**binary-file prerequisites 不保证匹配 Git** | off |
| `--full-index` | | 在 diff index header 行中显示完整 object IDs | false |
| `-U <N>`, `--unified=<N>` | | 生成 hunk 时使用 `N` 行上下文（Git `-U<n>` / `--unified=<n>`）；默认 3 | 3 |
| `--diff-filter=<FILTER>` | | 仅包含匹配 Git diff-filter 字母的变更路径（如 `A` 表示新增）；同时过滤补丁与 diffstat | 全部 |
| `--full-diff` | | 为 Git 兼容而接受；Libra 此处不接受 `-- <path>` pathspec，因此无 pathspec 时输出不变 | false |
| `--minimal` | | 请求最小 Myers edit script；Libra 默认 Myers backend 已保证最短脚本，因此输出与默认相同 | false |
| `--histogram` | | 使用 Histogram diff algorithm 生成文本 hunks | false |
| `--ignore-if-in-upstream` | | 抑制 stable patch-id 已出现在 range 排除侧的 commits | false |
| `--src-prefix <PREFIX>` / `--dst-prefix <PREFIX>` | | 替换默认 `a/` 与 `b/` diff path prefixes | `a/`、`b/` |
| `-p`, `--no-stat` | | 抑制 diffstat 摘要 | false |
| `--keep-subject` | `-k` | 逐字保留 commit subject，不添加 `[PATCH]` 前缀或 `n/m` 计数；与显式 `-n`、非空 `--rfc`、`--subject-prefix`（含空前缀）互斥；配置的 `format.subjectPrefix` 不冲突 | false |
| `--suffix <SFX>` | | 生成 patch 的文件名后缀（例如 `.txt`）；`--numbered-files` 下忽略 | `.patch` |
| `--zero-commit` | | 在每个 patch 的 `From <hash>` envelope line 中使用全零 hash | false |
| `--signature <SIGNATURE>` | | 放在每个 patch 和 cover letter 的 `-- ` 行之后的文本 | libra version |
| `--no-signature` | | 完全省略 `-- `/signature footer | false |
| `--signature-file <FILE>` | | 从文件读取 signature footer 文本（与 `--signature` 互斥） | |
| `--encode-email-headers` / `--no-encode-email-headers` | | 对包含非 ASCII 字符的 `From`/`Subject` header 值做 RFC 2047 Q-encode | off |
| `--numbered-files` | | 用裸序号命名输出文件（不应用 suffix） | false |

## 配置

未给对应 CLI option 时，`format.subjectPrefix`、`format.signOff`、
`format.outputDirectory`、`format.suffix` 按严格的 local → global → system
级联读取。CLI 值优先（包括 `--no-signoff`）；`--stdout` 不读取
`format.outputDirectory`。无效 Git boolean 或配置读取失败会明确报错，不会静默回退。

## 示例

### `--numstat`

`--numstat` 用 numstat 块替换 diffstat（`<插入>\t<删除>\t<路径>`，重命名显示为 `{old => new}`，二进制为 `-\t-`），并省略 `---` 分隔行；补丁照常输出。各行与 `libra diff --numstat` 逐字节相同。`-p`（`--no-stat`）不会抑制 numstat。

```bash
libra format-patch --numstat -1 --stdout
```

### 补丁正文中的重命名检测

补丁正文与 diffstat 由与 `libra diff` 相同的引擎生成，默认检测重命名（Git 的 50% 阈值）并遵循 `diff.renames`：纯重命名输出 `similarity index 100%` 与 `rename from`/`rename to`，不含内容 hunk；`diff.renames=false` 则输出删除 + 新增两段。该设置只影响正文，不改变提交选择。

### 基本 range

```bash
# 为最近三个提交生成 patches
libra format-patch HEAD~3..HEAD

# 精确把 HEAD 生成为 stream
libra format-patch -1 --stdout

# 包含一直到 root commit 的历史
libra format-patch --root --stdout

# 在目录中生成带编号 patches
libra format-patch -n -o patches/ main..feature

# 带 cover letter 和 threading
libra format-patch --cover-letter --thread origin/main..

# 版本 2，回复先前 thread
libra format-patch -v 2 --in-reply-to '<msgid@example>' origin/main..

# 管道给外部工具
libra format-patch --stdout origin/main.. | git am

# 记录 series 应用到的 base（用于 `git am --base`）
libra format-patch --base=origin/main --stdout origin/main..HEAD

# 跳过 upstream 已有 change，并使用自定义 diff prefixes
libra format-patch --ignore-if-in-upstream --src-prefix=old/ --dst-prefix=new/ origin/main..HEAD
```

## 输出格式

每个 patch 文件都是 mbox message：

```
From <commit-oid> <unix-mbox-date>
From: Author Name <email>
Date: <RFC 2822 作者时间与作者时区>
Subject: [PATCH n/m] commit subject
MIME-Version: 1.0
Content-Type: text/plain; charset=UTF-8
Content-Transfer-Encoding: 8bit

commit message body
---
diffstat summary
unified diff
--
<libra-version>
```

`-- ` footer 默认是 libra version；`--signature <text>` 用自定义文本替换它，`--signature-file <file>` 从文件读取 footer 文本，`--no-signature` 完全省略 footer。`--encode-email-headers` 会对包含非 ASCII 字符的 `From`/`Subject` header 值做 RFC 2047 Q-encode。Libra 默认关闭它（没有 `format.encodeEmailHeaders` config knob）；Git 的默认值来自该 config，而该 config 自身默认也是关闭，除非显式设置。

带 `--json` 或 `--machine` 时，`data.patches` 列出每个生成的输出。设置 `--cover-letter` 时，列表会在 commit patch records 前包含 record number `0` 的 cover letter。其文件名为带配置 suffix 的 `0000-cover-letter`（默认 `.patch`），或在 `--numbered-files` 下为 `0`。

`-N` 会取消 commit patch 与 cover-letter Subject 中的 `[PATCH n/m]` 计数，
文件名仍保留 `NNNN-` 序号前缀。未指定 `-N` 时，请求的 cover letter 会带
`[PATCH 0/N]` 计数。计数宽度跟随显示的最终编号，也包括 `--start-number`：
单个 patch 为 `[PATCH 1/1]`；从 9 开始的两个 patch，其 cover letter 为
`[PATCH 00/10]`，两个 patch 分别为 `[PATCH 09/10]` 和 `[PATCH 10/10]`。
从 1 开始的十个 patch，其 cover letter 也为 `[PATCH 00/10]`。`-n` 不会改变文件名。
`--numbered-files` 是例外，会使用纯数字序号。
较早的 Libra 版本会在 `-N` 下省略前缀；依赖旧文件名的脚本需要自行重命名。

完整 series 会在创建任何输出文件前先全部渲染；每个 patch 文件再通过临时文件 + atomic rename 持久化。管道 stdout 遵循 Libra 的 quiet BrokenPipe 行为。

## 错误处理

| 场景 | StableErrorCode |
|------|-----------------|
| 不在 Libra 仓库中 | `LBR-REPO-001` |
| 未知 revision | `LBR-CLI-003` |
| `-k` 与 `-n`、非空 `--rfc` 或显式 `--subject-prefix` 冲突 | `LBR-CLI-002`（退出 129；Git 退出 128），诊断列出冲突参数，不写入补丁 |
| `--base` 不是 series 的祖先 | `LBR-CLI-003`（退出 128） |
| `--base=auto`（不支持） | `LBR-CLI-002`（退出 129） |
| 输出文件写入失败 | `LBR-IO-002` |
| 输出目录创建失败 | `LBR-IO-002` |
| 配置读取失败 | `LBR-IO-001` |
| 无效 `format.signOff` / 配置的输出目录为空 | `LBR-CLI-003` |

## Blake3 仓库

当 `core.objectformat` 为 `blake3` 时，`--base` 的 prerequisite-patch-id 使用 BLAKE3 摘要（64 位十六进制），而不是 SHA-1 仓所用的 Git stable patch-id 组合器。
