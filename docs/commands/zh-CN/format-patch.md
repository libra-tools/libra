# `libra format-patch`

从提交生成 mbox 格式的 email patch 文件。

## 概要

```bash
libra format-patch [OPTIONS] [revision-range]
```

## 说明

`libra format-patch` 遍历 revision range（`A..B`，或把单个提交视为 `<commit>..HEAD`）；`-1 [commit]` 精确选择一个 commit，`--root [commit]` 则包含 root 及所有可达的非 merge ancestors。命令为每个非 merge commit 生成一个 patch 文件（默认以 `--suffix` 命名，默认 `.patch`，除非设置 `--numbered-files`，此时使用裸序号）。除 `--numbered-files` 外，文件名始终带有补零序号前缀，不受 `-n` 或 `-N` 影响。每个 patch 格式化为 mbox message，包含 RFC 2822 headers、plain-text diffstat 和 unified diff。输出兼容 `git am`。

Merge commits 默认跳过。当 revision range 解析为零个提交时，命令以退出码 0 成功结束，不生成 patch。普通模式不输出内容；使用 `--json` 时，payload 中包含 `"patches": []`。仓库没有任何 commit 或 revision 无法解析时仍会报错。若所有 commit 都已在 upstream 中，`--ignore-if-in-upstream` 也会成功返回空 series。

## 选项

| 标志 | 短参数 | 说明 | 默认值 |
|------|--------|------|--------|
| `[revision-range]` | | `A..B` range 或单个 commit；单个 commit 表示 `<commit>..HEAD` | `HEAD` |
| `-1` | `-1` | 只生成指定 commit；未给 revision 时生成 `HEAD` | false |
| `--root` | | 包含 root commit 和所有可达的非 merge ancestors | false |
| `--output-directory <DIR>` | `-o` | 将 patch 文件写入 `DIR` | 当前目录 |
| `--stdout` | | 将所有 patches 打印到 stdout | false |
| `--numbered` | `-n` | 在 commit patch 的 Subject 行中加入 `[PATCH n/m]` 计数 | false |
| `--no-numbered` | `-N` | 不在 commit patch 或 cover-letter 的 Subject 行中加入 `[PATCH n/m]` 计数 | 关闭 |
| `--rfc[=<text>]` | | 在主题前缀中加入 `RFC`（`[RFC PATCH]`）；给出值时用该文本替代（`--rfc=WIP` → `[WIP PATCH]`）。值必须用 `=` 紧跟，避免吞掉版本参数 | 关闭 |
| `--no-rfc` | | 取消此前的 `--rfc`（后写者生效） | 关闭 |
| `--filename-max-length <N>` | | 将输出文件名截断到最多 `N` 字节（包含 `0001-` 前缀与后缀）；必要时可只保留序号，截断点与 Git 一致 | 不限制 |
| `--commit-list-format <FMT>` | | 封面信的提交列表。默认（或 `short`）为 Git 的按作者分组 shortlog（`Name (N):` 加 72 列换行的主题）；`modern` 渲染 `[i/N] subject`；其他值按 pretty-format 解析，支持 `%h`、`%H`、`%s`、`%an`、`%ae`、`%n`、`%%`（不支持的转换会报 `'<fmt>' is not a valid format string`） | `short` |
| `--relative[=<path>]`（不支持） | | 仅为明确缺口而接受：Libra 没有针对导出补丁的相对路径过滤，命令以 128 退出并返回 `LBR-UNSUPPORTED-001` | — |
| `--all`（不支持） | | 仅为明确缺口而接受：Libra 的版本语法没有 all-refs 选择器，请显式写分支范围（如 `main..topic`） | — |
| `--no-base` | | 不输出 `base-commit:` 尾注，覆盖 `--base`（Libra 没有 `format.useAutoBase` 配置可重置） | 关闭 |
| `--start-number <N>` | | 从 `N` 开始编号 | 1 |
| `--subject-prefix <PREFIX>` | | 在 Subject: 行使用 `PREFIX` 而不是 `PATCH` | `PATCH` |
| `--cover-letter` | | 生成 cover-letter 模板（`0000-cover-letter<suffix>`，或 `--numbered-files` 下的 `0`） | false |
| `--thread` | | 添加 `In-Reply-To` 和 `References` headers（默认开启） | true |
| `--no-thread` | | 禁用 threading headers | false |
| `--in-reply-to <MESSAGE_ID>` | | 让第一封邮件回复给定 Message-ID | 无 |
| `--to <ADDRESS>` | | 添加 `To:` header（可重复；多个地址像 git 一样折叠）。放在 MIME headers 之后，应用于每个 patch 和 cover letter | 无 |
| `--cc <ADDRESS>` | | 添加 `Cc:` header（可重复；像 git 一样折叠） | 无 |
| `--no-to` / `--no-cc` | | 抑制 `To:` / `Cc:` headers（Libra 没有可重置的 `format.to`/`format.cc` config） | false |
| `--from[=<IDENT>]` | | 在 `From:` header 中使用 `<IDENT>` 而不是 commit author（裸 `--from` 使用 committer 配置身份）。当它不同于 author 时，原 author 会作为正文内 `From:` 行保留，便于 `git am` 还原 | author |
| `--reroll-count <N>` | `-v` | 标记为版本 `N`（把 `[PATCH]` 改为 `[PATCH vN]`） | 无 |
| `--signoff` | `-s` | 向每个 commit message 追加 `Signed-off-by` trailer | false |
| `--no-signoff` | | 禁用 signoff，并覆盖 `format.signOff` | false |
| `--notes[=<REF>]` | | 将每个 commit 的 notes 追加到 `---` 行之后、diffstat 之前。裸 `--notes` 使用默认 ref（`refs/notes/commits`）；`--notes=<ref>` 读取 `<ref>`。渲染为 `Notes:`（默认 ref）或 `Notes (<ref>):`，每行缩进四个空格；没有 note 的 commit 保持不变 | off |
| `--attach` | | 将每个 patch 作为 `multipart/mixed` MIME message 发出：log message + diffstat 位于 `text/plain` part，diff 位于 `text/x-patch` part 且 `Content-Disposition: attachment`。与 `--inline` 互斥 | off |
| `--inline` | | 类似 `--attach`，但 patch part 使用 `Content-Disposition: inline` | off |
| `--base <COMMIT>` | | 记录 `base-commit:` trailer（以及 base 与 series 之间每个非 merge commit 的 `prerequisite-patch-id:` 行，按 oldest-first），使 `git am --base` 可验证 series 可应用。Trailer 位于最后一个 patch，或 `--cover-letter` 下位于 cover letter。Base 必须是 series 的祖先（否则退出 128）。不支持 `--base=auto`（退出 129）。文本 diff 的 patch-id 匹配 `git patch-id --stable`；**binary-file prerequisites 不保证匹配 Git** | off |
| `--full-index` | | 在 diff index header 行中显示完整 object IDs | false |
| `--minimal` | | 请求最小 Myers edit script；Libra 默认 Myers backend 已保证最短脚本，因此输出与默认相同 | false |
| `--histogram` | | 使用 Histogram diff algorithm 生成文本 hunks | false |
| `--ignore-if-in-upstream` | | 抑制 stable patch-id 已出现在 range 排除侧的 commits | false |
| `--src-prefix <PREFIX>` / `--dst-prefix <PREFIX>` | | 替换默认 `a/` 与 `b/` diff path prefixes | `a/`、`b/` |
| `-p`, `--no-stat` | | 抑制 diffstat 摘要 | false |
| `--keep-subject` | `-k` | 逐字保留 commit subject，不添加 `[PATCH]` 前缀或 `n/m` 计数 | false |
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
Date: <RFC 2822 date>
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
| `--base` 不是 series 的祖先 | `LBR-CLI-003`（退出 128） |
| `--base=auto`（不支持） | `LBR-CLI-002`（退出 129） |
| 输出文件写入失败 | `LBR-IO-002` |
| 输出目录创建失败 | `LBR-IO-002` |
| 配置读取失败 | `LBR-IO-001` |
| 无效 `format.signOff` / 配置的输出目录为空 | `LBR-CLI-003` |

## Blake3 仓库

当 `core.objectformat` 为 `blake3` 时，`--base` 的 prerequisite-patch-id 使用 BLAKE3 摘要（64 位十六进制），而不是 SHA-1 仓所用的 Git stable patch-id 组合器。
