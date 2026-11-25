# `libra mailinfo`

从一封纯文本 email patch 中提取 commit metadata、commit-message body 和
patch 文本。这是 `git mailinfo` 的小型、可脚本化子集，并与 `libra am`
共用同一个 parser。

## 用法

```text
libra mailinfo <MSG> <PATCH> < mail
```

`MSG` 和 `PATCH` 是输出文件路径；邮件固定从 stdin 读取，因此命令可以在
Libra 仓库外运行。

## 行为

输入上限为 64 MiB，必须是 UTF-8 single-part `text/plain`。支持 `7bit`、
`8bit`、`binary`、`quoted-printable` 和 `base64` transfer encoding；要求
`From:`、`Date:`、`Subject:`。parser 支持 folded header、UTF-8/US-ASCII
RFC 2047 B/Q encoded words、可选 mbox `From ` envelope、开头的
`[PATCH ...]` subject marker，以及标准 in-body `From:` override。

成功时：

- stdout 输出 `Author:`、`Email:`、清理后的 `Subject:` 和 `Date:`；
- `MSG` 只写入 `---` separator 之前的 decoded message body；非空 body
  以换行结束；
- `PATCH` 从 `---` separator 开始，包含 diffstat、`diff --git` patch 和
  尾部 signature block。

两个目标的 parent directory 必须已存在；解析 parent-directory alias 后必须
是不同文件；目标不能是 `-` 或 directory。完整输入和两个 temporary payload
都会在替换目标前完成校验与写入。每个目标都通过 atomic replace 发布，但两个
不同 filesystem path 无法组成一次跨文件原子 transaction。

## 输出控制

| 选项 | 含义 |
|---|---|
| `--quiet` | 写入 `MSG`/`PATCH`，不在 stdout 输出 metadata。 |
| `--json` / `--machine` | 在标准 JSON envelope 中输出 metadata、目标路径和字节数；仍会写文件。 |

JSON `data` 包含 `author`、`email`、`subject`、`date`、`message_path`、
`patch_path`、`message_bytes`、`patch_bytes`。

## 退出码

| 退出码 | 含义 |
|---|---|
| `0` | 邮件解析成功，两个输出文件都已替换。 |
| `128` | 输入、encoding、header、patch 拆分、路径或文件 I/O 校验失败。 |
| `129` | 缺少或错误的输出参数。 |

## 示例

```bash
libra mailinfo message.txt patch.diff < 0001-fix.patch
libra --quiet mailinfo message.txt patch.diff < 0001-fix.patch
libra --json mailinfo message.txt patch.diff < 0001-fix.patch
```

## 当前限制

P2-02 最小 surface 不提供 Git `mailinfo` 的 `-k`、`-b`、`-m`、`-u`、
`--encoding`、`--scissors`、`--quoted-cr` 等选项。MIME multipart、
attachment、UTF-8/US-ASCII/ISO-8859-1 之外的 charset、多消息 mbox、binary patch，以及没有
`diff --git` section 的 patch 文本都会被拒绝。
