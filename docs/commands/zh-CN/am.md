# `libra am`

把一个或多个纯文本 `format-patch` 邮件文件依次应用为提交。每个提交保留邮件中的 subject/body、author 和 `Date:`；committer 使用当前 Libra 身份。

## 用法

```text
libra am [-3] <patch|mbox|-> ...
libra am --continue
libra am --skip
libra am --abort
```

## 行为

新 series 必须在已有提交的本地分支上启动，并且不能有 staged 或 tracked 工作树改动。无关的 untracked 文件会保留；但只要任一邮件会触及已有的 non-index 路径（包括 ignored 路径），命令就会在保存 sequencer 状态前拒绝。邮件总输入上限为 64 MiB，邮件数上限为 10,000。

每个输入可以是单封邮件或一个 mbox：首行是 mbox `From ` envelope 行的文件（或
stdin）会被切分为其中的每封邮件、按序应用。envelope 判定为 ctime 形状
（`hh:mm:ss` 时间 token 后紧跟 4 位年份，年份后允许时区/UUCP 尾巴），因此
`From my reading of RFC 9110` 这类正文散文绝不会切开邮件；正文内容（包括
`>From ` quoting）按 git 默认（mboxo）读取方式逐字节保留。`-` 从标准输入读取一封邮件或一个 mbox（最多出现一次）；完整邮件内容
持久化在 sequencer 状态中，因此来自 stdin 的 series 与文件 series 一样支持
`--continue` / `--skip` / `--abort`。多邮件来源在输出与状态中带位置标签
`<source>#<n>`。

除纯文本 diff 外，邮件还可携带 git 扩展 section：`GIT binary patch`
载荷（`literal` 与 `delta` 两种，base85 + zlib 解码并有界解压）、
`rename from`/`rename to`（带或不带内容 hunks——源删除与目标在同一提交中
一并暂存）、`copy from`/`copy to`，以及仅改 mode 的 `old mode`/`new mode`
变更（可执行位直接写入工作树并暂存）。所有扩展目标都经过同一 path-safety
检查。

mail parser 接受 UTF-8 邮件与 `7bit`、`8bit`、`binary`、quoted-printable、
base64 transfer encoding——single-part `text/plain`，或 MIME multipart 容器
（`multipart/mixed`/`alternative`，嵌套有深度上限）：按声明的 boundary 切分，
每个受支持的 text part（`text/plain`、`text/x-patch`、`text/x-diff`）按各自的
transfer encoding 解码并按序拼接，HTML alternative 与二进制附件被跳过，没有任何
受支持 text part 的 multipart 邮件 fail-closed。因此 `format-patch --attach`
的输出可直接应用。它读取 `From:`、`Date:`、`Subject:`，清理前导 `[PATCH ...]`，支持标准 in-body `From:` 覆盖，并从 `---` 分隔线之后提取文本 `diff --git`。UTF-8/US-ASCII 的 RFC 2047 `B`/`Q` encoded word 会被解码。

每个目标都会拒绝绝对路径、空/`.`/`..` 路径组件、NUL、`.libra/` 和已有 symlink 路径组件。单封邮件中的所有文件会先全部试应用，再进行第一次写入。文件替换使用原子 rename；内容补丁保留已有 permission bits。

工作树写入前会先持久化 sequencer 状态。每个成功提交会在同一个 SQLite transaction 中移动 branch、写 reflog，并推进或清除 `am` 位置。`--continue` / `--skip` 会拒绝 tip 已在 sequencer 之外移动的分支。如果中断发生在状态保存后、当前邮件尚未写入前（包括两次 commit 之间），`--continue` 会重试该邮件。`--abort` 恢复原始 branch tip、index 和 tracked 工作树；如果中断发生在新文件写入后、stage 前，也会清理该新文件目标。

## Hooks

applypatch hook 族从 `.libra/hooks` 经沙箱化 repo-hook runner 运行（与 commit
hooks 同一契约；`LIBRA_NO_HOOKS=1` 可旁路）：

- `applypatch-msg <msg-file>`——在任何工作树写入之前。拟提交信息写入当前
  worktree 的 `COMMIT_EDITMSG`（唯一可写 hook 文件）；hook 可原地编辑；非零
  退出拒绝该邮件，series 保持可恢复。
- `pre-applypatch`——工作树写入并暂存之后、提交之前；非零退出使 series 带着
  已暂存改动暂停。它同样把关决议后的 `--continue` 提交（Git `--resolved`
  语义）；`applypatch-msg` 不会在该路径重跑。
- `post-applypatch`——提交之后；advisory（失败只警告，绝不使已应用邮件失败）。

## 冲突恢复

使用 `-3`/`--3way` 时，无法应用的文本补丁会回退到三方合并：base 取 `index`
头记录的旧 blob（从本地对象库解析；缩写 id 仅在无歧义时解析），theirs 为补丁
应用到该 base 的结果，ours 为当前内容。合并干净则静默应用；有冲突则把
`<<<<<<<` 标记写入工作树并暂停 series（解决、暂存、`--continue`）。base 不在
本地时保持普通拒绝——回退绝不伪造内容。`-3` 的选择持久化在 series 状态中，
恢复时语义不变。

不使用 `-3` 时，补丁无法应用则当前 branch tip 不动，并保留可恢复的 series：

1. 手工解决受影响路径；
2. 用 `libra add` 只 stage 当前补丁包含的路径；
3. 运行 `libra am --continue`。

`--skip` 丢弃当前补丁并继续下一封邮件；`--abort` 丢弃整个 series 并恢复 `am` 前状态。

## 选项

| 选项 | 含义 |
|---|---|
| `--continue` | 提交完整 staged resolution 并继续。当前补丁仍有 unstaged 路径、无关 tracked 改动、unresolved index entry、空 resolution 或无关 staged 路径时会拒绝；pristine recovery state 会重试当前邮件。 |
| `--skip` | reset 当前补丁并继续剩余邮件。 |
| `--abort` | 恢复原始 branch tip、index 和 tracked 工作树并清除 sequencer。 |
| `--json` / `--machine` | 在标准 envelope 中输出 action、已应用邮件的源文件/subject/commit ID，以及可选 restored HEAD。 |

## 示例

```bash
# 生成并重放 series
libra format-patch -o outgoing origin/main..HEAD
libra switch target
libra am outgoing/0001-*.patch outgoing/0002-*.patch

# 把整个 series 作为一个 mbox 管道输入
libra format-patch --stdout origin/main..HEAD | libra am -

# 解决停止的补丁
$EDITOR src/lib.rs
libra add src/lib.rs
libra am --continue

# 取消整个 series
libra am --abort
```

## 当前限制

尚未达到完整 Git `am` parity。当前不公开 Git 的完整 flag 集（`--signoff`、`--keep`、`--scissors` 等）。共享 parser 也通过独立的 [`libra mailinfo`](mailinfo.md) 命令公开。
