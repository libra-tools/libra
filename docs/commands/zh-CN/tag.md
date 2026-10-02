# `libra tag`

创建、列出或删除标签。

## 概要

```
libra tag [<name> [<commit>]] [-a] [-m <message> | -F <file>] [-e] [-f]
libra tag -l [-n <lines>] [--column[=<mode>]]
libra tag -d <name>
```

## 说明

`libra tag` 管理轻量标签和附注标签。轻量标签只是指向对象（通常是提交）的具名指针，而附注标签会存储带有消息、打标签者身份和时间戳的完整标签对象。

不带参数（或带 `-l`）时，命令列出所有标签。给出名称时，它会在 HEAD 处创建新标签；同时给出 `<commit>` 时则在该对象上创建（`libra tag v1.0 HEAD~1`）。`<commit>` 接受任意 commit-ish——分支、提交 id、`HEAD~2` 之类的表达式或另一个标签——并与 Git 一样**不 peel** 地记录：给出附注标签名时，新标签指向该标签对象——附注或签名标签由此成为嵌套标签（标签的标签），轻量标签则只是该标签对象的另一个名字。若要标记附注标签所指的提交，用 `<tag>^{}`（`libra tag v1.0-commit 'v1.0^{}'`）。解析到 tree 或 blob 的目标会被拒绝（Libra 目前无法传输 tree / blob 的标签）。给出 `<commit>` 时不读取 HEAD，因此在未诞生的分支上也能打标签。目标在读取 `-F` 文件、打开编辑器以及写入任何标签引用或标签对象之前解析，无效目标不会留下新的标签引用或标签对象。添加 `-a`/`--annotate`、`-m <message>` 或 `-F <file>`（从文件或 `-` 表示 stdin 读取消息）会创建附注标签，而不是轻量标签；单独使用 `-a` 会打开编辑器，`-e`/`--edit` 在编辑器中撰写消息（有 `-m`/`-F` 时预填）。与 `-d`/`-l`/`-v` 组合时，`-a` 为用法错误。`-f` 标志允许覆盖同名已有标签。

标签引用与分支引用一起存储在 SQLite 数据库中，提供相同的事务保证。

列表形态（`-l`、`-n`、`--contains`、`--no-contains`、`--points-at`、`--merged`、`--no-merged`、`--sort`、`--column`、`--no-column`，含 `--no-column <pattern>`）以及 `--verify` / `-v` 不会写入 Operation v2 记录。创建或删除标签仍各记录一笔（`libra op log --command tag`）。

## 选项

| 标志 | 长选项 | 值 | 说明 |
|------|------|-------|-------------|
| | `<name>` | 位置参数（可选） | 要创建、显示或删除的标签名 |
| | `<commit>` | 位置参数（可选） | 新标签指向的对象（默认 HEAD）：任意 commit-ish，不 peel 地记录——给出附注标签名时新标签指向该标签对象（要其提交用 `<tag>^{}`）；解析到 tree / blob 的目标被拒绝。只在创建标签时有效 |
| `-l` | `--list` | | 列出所有标签 |
| `-d` | `--delete` | | 删除具名标签 |
| `-a` | `--annotate` | | 创建附注标签。单独使用时打开编辑器（清理后为空则中止且不写 ref）。与 `-m`/`-F`/`-e` 组合时创建附注标签；与 `-d`/`-l`/`-v` 组合为用法错误。 |
| `-m` | `--message` | `<msg>` | 使用给定消息创建附注标签 |
| `-F` | `--file` | `<file>` | 创建附注标签，从文件读取消息（`-` 表示 stdin）。与 `-m` 互斥。 |
| `-e` | `--edit` | | 打开编辑器撰写或编辑附注标签消息。有 `-m`/`-F` 时编辑器以该消息预填，否则撰写新消息。单独使用 `-a` 走同一编辑器路径。注释行被剥离；结果为空则中止。 |
| `-f` | `--force` | | 覆盖已有标签 |
| `-n` | `--n-lines` | `<lines>` | 列出时显示的附注行数（0 = 只显示名称） |
| | `--column` | `[options]` | 以多列布局列出标签。逗号/空格分隔的选项：启用 `always`/`auto`/`never`（缺省 = `always`）、填充顺序 `column`（自上而下，默认）/ `row`（自左而右）/ `plain`（单列）、列宽 `dense`（每列自适应）/ `nodense`（等宽，默认）。与 `git tag --column` 字节一致。不能与 `-n` 同用。 |
| | `--no-column` | | 不以多列布局列出标签（等价于 `--column=never`），撤销先前的 `--column`（最后出现者生效）。标签默认每行一个，故单独使用时为 no-op。 |
| | `--points-at` | `<object>` | 只列出指向给定对象（peel 到其提交）的标签；隐含列表模式。每个标签沿整条标签链完全 peel（标签的标签 peel 到最终提交）；链终点为 tree 或 blob 的标签不参与该过滤。 |
| | `--contains` | `<commit>` | 只列出 tip 以 `<commit>` 为祖先的标签。每个标签沿整条标签链完全 peel（标签的标签 peel 到最终提交）；链终点为 tree 或 blob 的标签不参与该过滤。 |
| | `--no-contains` | `<commit>` | 只列出 tip 不以 `<commit>` 为祖先的标签。每个标签沿整条标签链完全 peel（标签的标签 peel 到最终提交）；链终点为 tree 或 blob 的标签不参与该过滤。 |
| | `--merged` | `<commit>` | 只列出可从 `<commit>` 到达的标签。每个标签沿整条标签链完全 peel（标签的标签 peel 到最终提交）；链终点为 tree 或 blob 的标签不参与该过滤。 |
| | `--no-merged` | `<commit>` | 只列出不可从 `<commit>` 到达的标签。每个标签沿整条标签链完全 peel（标签的标签 peel 到最终提交）；链终点为 tree 或 blob 的标签不参与该过滤。 |
| | `--sort` | `<key>` | 按键排序列表（`refname`、`-refname`、`creatordate`、`-creatordate`——`creatordate` 以对象哈希序近似）。优先于 `tag.sort` 配置默认（严格 local → global → system 级联；无效配置值以 `LBR-CLI-002`、local/global 配置库不可读以 `LBR-IO-001`，均在任何列表输出前 fail-closed——例外：schema 比二进制新的全局配置库会在一次性警告后被跳过（见 `LBR-CONFIG-001`）；重复配置值只应用胜出 scope 的最后一个——Git 会叠成多键排序）。标志与配置都未设置时按 `refname` 升序列出（Git 默认）。配置的 `tag.sort` 不会把创建标签变成列表操作 |
| `-s` | `--sign` | | 用 vault PGP 密钥为附注标签签名（需要 `-m`；不与 Git GPG 互操作）。 |
| | `--no-sign` | | 不签名标签，撤销先前的 `-s`/`--sign`（命令行最后出现者生效）。标签默认不签名，故单独使用时为 no-op。 |
| `-v` | `--verify` | | 验证具名附注标签的 vault PGP 签名（**退出码：0＝良好、1＝不良**）。 |

### 标志示例

```bash
# 在 HEAD 创建轻量标签
libra tag v1.0

# 在指定提交（而非 HEAD）上打标签
libra tag v1.0 HEAD~1

# 在另一分支的顶端创建附注标签
libra tag -m "Release v1.1" v1.1 side

# 创建带消息的附注标签
libra tag -a -m "Release v1.1" v1.1
libra tag -m "Release v1.1" v1.1

# 从文件（或 stdin，用 -）读取消息创建附注标签
libra tag -F release-notes.txt v1.1
libra log -1 --format=%B | libra tag -F - v1.1

# 强制覆盖已有标签
libra tag -f v1.0

# 把已有标签移到另一个提交
libra tag -f v1.0 HEAD~1

# 标记附注标签所指的提交，而不是标签对象
libra tag v1.0-commit 'v1.0^{}'

# 列出所有标签
libra tag -l

# 列出标签并预览附注（2 行）
libra tag -l -n 2

# 删除标签
libra tag -d v1.0

# 面向代理的 JSON 输出
libra tag --json v1.0
```

## 常用命令

```bash
libra tag v1.0                        # 在 HEAD 创建轻量标签
libra tag v1.0 HEAD~1                 # 在指定提交（而非 HEAD）上打标签
libra tag -m "Release v1.1" v1.1 side  # 在分支 side 的顶端创建附注标签
libra tag -f v1.0 HEAD~1              # 把已有标签移到另一个提交
libra tag -a -m "Release v1.1" v1.1   # 创建附注标签
libra tag -m "Release v1.1" v1.1      # 创建附注标签
libra tag -l -n 2                     # 列出标签，最多显示 2 行附注
libra tag -d v1.0                     # 删除标签
libra tag --json v1.0                 # 面向代理的结构化 JSON 输出
```

## 人类可读输出

- `libra tag -l`：打印标签列表，每行一个；使用 `-n` 时缩进显示附注行
- `libra tag v1.0`：`Created lightweight tag 'v1.0' at abc1234`
- `libra tag v1.0 HEAD~1`：`Created lightweight tag 'v1.0' at <HEAD~1 的短 id>`
- `libra tag -m "msg" v1.0`：`Created annotated tag 'v1.0' at abc1234`
- `libra tag -d v1.0`：`Deleted tag 'v1.0' (was abc1234)`
- 默认创建路径保留当前人类可读输出

## 结构化输出（JSON 示例）

`--json` / `--machine` 使用 `action` 区分操作：

创建标签：

```json
{
  "ok": true,
  "command": "tag",
  "data": {
    "action": "create",
    "name": "v1.0",
    "hash": "abc123...",
    "tag_type": "lightweight",
    "message": null
  }
}
```

显式给出目标时（`libra tag --json v1.0 HEAD~1`）信封相同：轻量标签的 `hash` 为目标对象，附注标签的 `hash` 为新标签对象。

创建附注标签：

```json
{
  "ok": true,
  "command": "tag",
  "data": {
    "action": "create",
    "name": "v1.1",
    "hash": "abc123...",
    "tag_type": "annotated",
    "message": "Release v1.1"
  }
}
```

列出标签：

```json
{
  "ok": true,
  "command": "tag",
  "data": {
    "action": "list",
    "tags": [
      { "name": "v1.0", "hash": "abc123...", "tag_type": "lightweight", "message": null },
      { "name": "v1.1", "hash": "def456...", "tag_type": "annotated", "message": "Release v1.1" }
    ]
  }
}
```

删除标签：

```json
{
  "ok": true,
  "command": "tag",
  "data": {
    "action": "delete",
    "name": "v1.0",
    "hash": "abc123..."
  }
}
```

`action=list` 返回 `tags` 数组；`action=delete` 返回 `name` 和 `hash`。对于格式异常标签引用的恢复性删除，当存储目标缺失时，`hash` 可以为 `null`。

## 设计理由

### 为什么用 vault PGP 而不是 Git GPG 签名？

Git 的 `--sign` 用 GPG 生成嵌入标签对象的内联 PGP 签名。Libra **支持** `-s`/`--sign`，但通过 **vault PGP 密钥**而非每个开发者本地的 GPG keyring：

- **GPG 密钥管理脆弱**：开发者经常丢失密钥、让密钥过期，或误配置 gpg-agent，导致签名工作流损坏。在 CI/CD 环境中，安全管理 GPG keyring 是运维负担。
- **基于 Vault 的签名是预期路径**：Libra 架构围绕基于 vault 的签名模型设计（见 `libra init` 上的 `--vault`），加密操作委托给安全密钥存储，而不是要求每个开发者维护本地 GPG 密钥。这种方式集中信任并简化密钥轮换。
- **通过 SQLite 保证标签完整性**：因为标签引用位于事务数据库而不是 loose 文件中，GPG 签名原本要缓解的篡改表面已经降低。未经授权的引用修改需要数据库访问，而不只是文件系统写入。

`--no-sign` 撤销先前的 `-s`/`--sign`（最后出现者生效）；标签默认不签名，故单独使用时为 no-op。由于 Libra 的签名是 vault PGP 而非 Git GPG，签名不与 `git tag -v` 互操作。

### --verify

`-v`/`--verify` 验证具名附注标签的 PGP 签名，而非逐标签 GPG 检查。验证使用本仓库曾配置过的公钥允许列表（活动、生成、历史），避免了 Git 中 `git tag -v` 因签名者公钥不在本地 keyring 而令人困惑地失败的情况。

吊销与过期按**签名自身的创建时刻**判定：密钥仍有效时签出的标签继续可验证；而在密钥被吊销或过期之后签出的签名会被拒绝。

### 为什么区分轻量标签和附注标签？

Libra 保留 Git 的两层标签模型，以保持磁盘格式兼容。轻量标签是简单 ref 指针（适合临时标记），而附注标签存储对发布有用的元数据。消息来源是开关：提供 `-a`、`-m`、`-F` 或 `-e`（在编辑器中撰写消息）时创建附注标签，都不提供时创建轻量标签。单独使用 `-a` 会打开编辑器，与 `git tag -a` 一致。

## 参数对比：Libra vs Git vs jj

| 功能 | Git | Libra | jj |
|---------|-----|-------|----|
| 创建轻量标签 | `git tag <name>` | `libra tag <name>` | `jj tag create <name>` |
| 在指定提交上打标签 | `git tag <name> <commit>` | `libra tag <name> <commit>`（不 peel；tree / blob 目标被拒绝） | `jj tag set <name> -r <rev>` |
| 创建附注标签 | `git tag -a -m "msg" <name>` | `libra tag -a -m "msg" <name>`（或 `-m` / `-F` / `-e`） | 不支持（仅轻量） |
| 从文件读取附注消息 | `git tag -F <file> <name>` | `libra tag -F <file> <name>`（`-` 表示 stdin） | N/A |
| 编辑器编辑消息 | `git tag -e <name>`（配合 `-a`/`-m`/`-F`） | `libra tag -a <name>` 或 `libra tag -e <name>`（撰写附注消息；`-m`/`-F` 预填） | N/A |
| 列出标签 | `git tag -l` | `libra tag -l` | `jj tag list` |
| 带消息列出 | `git tag -l -n3` | `libra tag -l -n 3` | N/A |
| 多列布局 | `git tag --column[=<options>]` | `libra tag --column[=<options>]`（always/auto/never + column/row/plain + dense/nodense；`--no-column` 撤销） | N/A |
| 删除 | `git tag -d <name>` | `libra tag -d <name>` | `jj tag delete <name>` |
| 强制覆盖 | `git tag -f <name>` | `libra tag -f <name>` | `jj tag create <name>`（总是覆盖） |
| 签名标签 | `git tag -s <name>` | `libra tag -s -m "msg" <name>`（vault PGP；`--no-sign` 撤销） | N/A |
| 验证标签 | `git tag -v <name>` | `libra tag -v <name>`（vault PGP） | N/A |
| 结构化输出 | 无 | `--json` / `--machine` | `--template` |

## 错误处理

| 场景 | 错误码 | 提示 |
|----------|-----------|------|
| 标签已存在 | `LBR-CONFLICT-002` | "delete it first with 'libra tag -d <name>'." |
| HEAD 没有可打标签的提交 | `LBR-REPO-003` | "create a commit first before tagging HEAD." |
| `<commit>` 无法解析（名字不存在、短 id 有歧义，或完整 id 对应的对象不存在）："Failed to resolve '<commit>' as a valid ref." | `LBR-CLI-003` | "use 'libra log --oneline' to see available commits."（退出码 129；Git 退出 128——名字不存在时文案相同，短 id 歧义时先列出候选再给同一行，完整 id 对应的对象不存在时则在之后以 `cannot update ref … nonexistent object` 或 `bad object type.` 失败） |
| `<commit>` 解析到 tree 或 blob："cannot tag '<commit>': it resolves to a tree object, not a commit"（或 `blob`） | `LBR-CLI-003` | "only a commit, or a tag that peels to a commit, can be tagged"（退出码 129） |
| `<commit>` 与 `-l`/`-d`/`-v` 或任一列表模式旗标（`-n`、`--points-at`、`--contains`、`--no-contains`、`--merged`、`--no-merged`、`--sort`、`--column`、`--no-column`）同用："the <commit> argument '<x>' is only valid when creating a tag" | `LBR-CLI-002` | "list, delete and verify take a single tag name or pattern."（退出码 129，在打开仓库之前检查） |
| 第三个位置参数（`libra tag a b c`）："unexpected argument 'c' found" | `LBR-CLI-002` | clap 用法文本（退出码 129；Git 报 "too many arguments"） |
| `<commit>` 经由未诞生的 HEAD（`HEAD`、`@`、`HEAD~1`）："Cannot create tag: HEAD does not point to a commit" | `LBR-REPO-003` | "create a commit first before tagging HEAD."（退出码 128，与 Git 相同） |
| `<commit>` 无法读取："failed to resolve '<commit>': the object store could not be read" | `LBR-IO-001` | "check that the repository is readable and retry."（退出码 128） |
| `<commit>` 的对象图损坏（如标签的目标缺失）："failed to resolve '<commit>': its object graph is corrupt or incomplete" | `LBR-REPO-002` | "run 'libra fsck' to inspect missing objects."（退出码 128） |
| 标签未找到（delete/show） | `LBR-CLI-003` | "use 'libra tag -l' to list available tags." |
| --delete/--message/--file/--edit/--force 缺少标签名 | `LBR-CLI-002` | "use 'libra tag <name>' to create or update a tag"（`--edit` 为 "tag name is required when using --edit"） |
| `-m`/`-F`/`-e` 与非创建模式（list/delete/verify/过滤）组合 | `LBR-CLI-002` | "-m/--message, -F/--file, and -e/--edit are only valid when creating a tag" |
| 编辑消息为空（`-e` 缓冲全为注释/空白） | `LBR-REPO-003` | "write a non-comment message in the editor, or pass -m/--message." |
| `-e` 无可用编辑器（无 GIT_EDITOR/core.editor/VISUAL/EDITOR 且无 TTY） | `LBR-REPO-003` | "set GIT_EDITOR, core.editor, VISUAL, or EDITOR" |
| 无法解析 HEAD | `LBR-IO-001` 或 `LBR-REPO-002` | -- |
| 无法序列化附注标签 | `LBR-REPO-005` | -- |
| 无法存储对象 | `LBR-IO-002` | -- |
| 无法持久化引用 | `LBR-IO-002` | -- |
| 无法删除标签 | `LBR-IO-002` | -- |
| 无法列出标签（DB 错误） | `LBR-IO-001` | -- |
| 无法列出标签（对象损坏） | `LBR-REPO-002` | -- |
| 过滤列表（`--points-at`/`--contains`/`--no-contains`/`--merged`/`--no-merged`）遇到无法 peel 的标签链（对象缺失或标签环）："tag '<name>' cannot be peeled to a commit: its tag chain is broken" | `LBR-REPO-002` | "run 'libra fsck' to inspect missing objects." |
| 过滤列表无法读取某标签的标签链："failed to read the tag chain of '<name>'" | `LBR-IO-001` | "check that the repository is readable and retry." |
