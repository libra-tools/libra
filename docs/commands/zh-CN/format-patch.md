# `libra format-patch`

从提交生成 mbox 格式的 email patch 文件。

## 概要

```bash
libra format-patch [OPTIONS] [revision-range]
```

## 说明

`libra format-patch` 遍历 revision range（`A..B`，或把单个提交视为 `<commit>..HEAD`），为每个非 merge commit 生成一个 patch 文件（默认以 `--suffix` 命名，默认 `.patch`，除非设置 `--numbered-files`，此时使用裸序号），并把每个 patch 格式化为 mbox message，包含 RFC 2822 headers、plain-text diffstat 和 unified diff。输出兼容 `git am`。

Merge commits 默认跳过。当 revision range 解析为零个提交时，命令以错误退出。

## 选项

| 标志 | 短参数 | 说明 | 默认值 |
|------|--------|------|--------|
| `[revision-range]` | | `A..B` range 或单个 commit；单个 commit 表示 `<commit>..HEAD` | `HEAD` |
| `--output-directory <DIR>` | `-o` | 将 patch 文件写入 `DIR` | 当前目录 |
| `--stdout` | | 将所有 patches 打印到 stdout | false |
| `--numbered` | `-n` | 以开头序号命名文件（`0001-subject.patch`） | false |
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
| `--notes[=<REF>]` | | 将每个 commit 的 notes 追加到 `---` 行之后、diffstat 之前。裸 `--notes` 使用默认 ref（`refs/notes/commits`）；`--notes=<ref>` 读取 `<ref>`。渲染为 `Notes:`（默认 ref）或 `Notes (<ref>):`，每行缩进四个空格；没有 note 的 commit 保持不变 | off |
| `--attach` | | 将每个 patch 作为 `multipart/mixed` MIME message 发出：log message + diffstat 位于 `text/plain` part，diff 位于 `text/x-patch` part 且 `Content-Disposition: attachment`。与 `--inline` 互斥 | off |
| `--inline` | | 类似 `--attach`，但 patch part 使用 `Content-Disposition: inline` | off |
| `--base <COMMIT>` | | 记录 `base-commit:` trailer（以及 base 与 series 之间每个非 merge commit 的 `prerequisite-patch-id:` 行，按 oldest-first），使 `git am --base` 可验证 series 可应用。Trailer 位于最后一个 patch，或 `--cover-letter` 下位于 cover letter。Base 必须是 series 的祖先（否则退出 128）。不支持 `--base=auto`（退出 129）。文本 diff 的 patch-id 匹配 `git patch-id --stable`；**binary-file prerequisites 不保证匹配 Git** | off |
| `--full-index` | | 在 diff index header 行中显示完整 object IDs | false |
| `--no-stat` | | 抑制 diffstat 摘要 | false |
| `--keep-subject` | | 保留 commit subject 中原有 `[PATCH]` prefix | false |
| `--suffix <SFX>` | | 生成 patch 的文件名后缀（例如 `.txt`）；`--numbered-files` 下忽略 | `.patch` |
| `--zero-commit` | | 在每个 patch 的 `From <hash>` envelope line 中使用全零 hash | false |
| `--signature <SIGNATURE>` | | 放在每个 patch 和 cover letter 的 `-- ` 行之后的文本 | libra version |
| `--no-signature` | | 完全省略 `-- `/signature footer | false |
| `--signature-file <FILE>` | | 从文件读取 signature footer 文本（与 `--signature` 互斥） | |
| `--encode-email-headers` / `--no-encode-email-headers` | | 对包含非 ASCII 字符的 `From`/`Subject` header 值做 RFC 2047 Q-encode | off |
| `--numbered-files` | | 用裸序号命名输出文件（不应用 suffix） | false |

## 示例

### 基本 range

```bash
# 为最近三个提交生成 patches
libra format-patch HEAD~3..HEAD

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

## 错误处理

| 场景 | StableErrorCode |
|------|-----------------|
| 不在 Libra 仓库中 | `LBR-REPO-001` |
| 未知 revision 或空 range | `LBR-CLI-003` |
| `--base` 不是 series 的祖先 | `LBR-CLI-003`（退出 128） |
| `--base=auto`（不支持） | `LBR-CLI-002`（退出 129） |
| 输出文件写入失败 | `LBR-IO-002` |
| 输出目录创建失败 | `LBR-IO-002` |
