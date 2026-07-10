# `libra cherry-pick`

应用一些已有提交引入的更改。

**别名：** `cp`

## 概要

```
libra cherry-pick [-n|--no-commit] [-x] [-s|--signoff] [-e|--edit]
                  [-m <n>|--mainline <n>] [--ff] [-S|--gpg-sign]
                  [--allow-empty] [--allow-empty-message] [--keep-redundant-commits]
                  [--empty=<mode>] [--cleanup=<mode>] [--json] [--quiet] <commit>...
libra cherry-pick (--continue | --skip | --abort | --quit)
```

## 说明

`libra cherry-pick` 将指定提交引入的更改应用到当前分支。对于每个具名提交，Libra 会计算该提交与其父提交之间的 diff，将得到的 changeset 应用到当前索引和工作树，并且（除非给出 `--no-commit`）记录一个新提交。需要在新提交消息中包含原始提交哈希时，使用 `-x`。

这适合在不合并的情况下，将一个分支上的提交选择性应用到另一个分支。提供多个提交时，它们会按给定顺序应用，每个提交都会先成为当前分支上的新提交，然后再处理下一个。

该命令要求处于活动分支（不是 detached HEAD）。非 merge commit 直接应用；cherry-pick merge commit 需用 `-m <n>`/`--mainline <n>` 指定沿哪个父提交做 diff。

自动提交的 cherry-pick 会保留源提交的 author metadata（姓名、邮箱、author date 与时区）。committer 使用当前身份/日期，并遵循 `GIT_COMMITTER_*` 覆盖。带签名的源提交会先剥离 `gpgsig` 消息块，再执行消息清理与 trailer 追加，因此签名块不会成为重放后的 subject。

当某提交无法干净应用时，Libra 执行三方 apply（base = 父提交树，ours = 当前索引，theirs = 被 pick 的树），并把任何发散路径写入索引（stage 1/2/3）与工作树（行级冲突标记，与 Git 一致）。进行中的序列持久化到 SQLite `cherry_pick_state` 表，因此你可以解决冲突后用 `--continue` 续作、用 `--skip` 丢弃冲突提交，或用 `--abort`/`--quit` 撤销整个序列。cherry-pick 序列进行期间，`merge` 与 `rebase` 被阻止（`LBR-CONFLICT-002`）。

## 选项

### `-n`, `--no-commit`

将源提交的更改应用到索引和工作树，但**不**创建新提交。这样你可以在手动运行 `libra commit` 前检查或组合更改。

`--no-commit` 支持多个提交：每个提交的更改依次累积到索引/工作树而不创建提交。注意：`--no-commit` 多提交序列没有逐步快照，因此其间发生冲突是终止性的——不会写入可续作的 sequencer 状态，需用 `libra reset --hard`/`libra restore` 手动清理。

```bash
# 暂存 abc1234 的更改但不提交
libra cherry-pick -n abc1234

# 检查暂存更改，然后手动提交
libra status
libra commit -m "cherry-picked and adjusted abc1234"
```

### `-x`

在新提交消息中追加 `(cherry picked from commit <hash>)`。不带 `-x` 时，Libra 保留源提交消息且不添加来源行，与 Git 默认行为一致。

```bash
# 在新提交消息中记录原始提交哈希
libra cherry-pick -x abc1234
```

### `-s`, `--signoff`

在新提交消息中追加 `Signed-off-by: <name> <email>` trailer（取自配置的 `user.name`/`user.email`）。与 `-x` 组合时，先输出 `(cherry picked from commit ...)` 行、`Signed-off-by` 在最后，与 Git 的 trailer 顺序一致。

### `-e`, `--edit`

提交前在编辑器中打开组装好的提交消息。编辑器按 `core.editor` → `$VISUAL` → `$EDITOR` 解析。在机器/JSON 模式或无交互 TTY 时，`-e` 降级为直接使用组装好的消息、不启动编辑器（因此永不阻塞自动化）。

### `-m <n>`, `--mainline <n>`

以父提交编号 `<n>`（从 1 起）作为 diff base 来 cherry-pick 一个 merge commit。merge commit 必须带 `-m`——不带 `-m` 的 merge commit 会被拒绝。在非 merge commit 上使用 `-m`、或父编号越界，同样被拒绝（`LBR-CLI-002`）。

```bash
# 沿第一个父提交 cherry-pick 一个 merge commit
libra cherry-pick -m 1 <merge-commit>
```

### `--ff`

当被 pick 的提交是 HEAD 的直接单父子提交、且未设置任何会重写提交的修饰符（如 `-x`/`-s`/`-e`/`-m`）时，直接将 HEAD 快进到该提交，而不重放或重写它（无 hash 漂移）。

### `-S`, `--gpg-sign`

使用 libra vault 签名密钥对 cherry-pick 出的提交签名。无论 `vault.signing` 配置默认值如何，显式请求时都会签名。若 vault 无可用签名密钥，则该 pick 失败，而不会产出未签名提交。

### `--allow-empty`

即使提交自身的 changeset 为空（其树等于其父树）也 cherry-pick。默认这类提交被拒绝（`LBR-CLI-002`）。

### `--allow-empty-message`

允许以空消息创建新提交。默认空消息被拒绝（`LBR-CLI-002`）。

### `--keep-redundant-commits`

保留重放后变得冗余（结果树与当前 HEAD 相同）的提交。默认这类冗余提交被拒绝（`LBR-CLI-002`）。等价于 `--empty=keep`。

### `--empty=<mode>`

控制重放后相对 HEAD 变得冗余的提交：`stop`（默认——停下交由你决定）、`drop`（跳过该提交，HEAD 不前进，并打印 `dropping <sha> <subject> -- patch contents already upstream`）、`keep`（保留这个空提交，等价 `--keep-redundant-commits`）。非法 mode 为用法错误（`LBR-CLI-002`，退出 129），且在任何提交（以及 `--continue`/`--skip`/`--abort`/`--quit`）之前校验。

### `--cleanup=<mode>`

清理重放的提交消息。`<mode>` 为 `strip`/`whitespace`/`verbatim`/`scissors`/`default`。先清理被 pick 的正文（及 `-e` 编辑缓冲），再追加生成的 `-x`/`Signed-off-by` trailer（保留其分隔空行）。无编辑器时 `default`/`scissors` 回退为 `whitespace`（与 Git“若消息将被编辑”的语义一致）。非法 mode 为用法错误（`LBR-CLI-002`，退出 129），且在任何提交（以及 `--continue`/`--skip`/`--abort`/`--quit`）之前校验。省略时消息仅做 trim，与既有行为一致。

## 冲突 sequencer

pick 发生冲突时，解决相关文件、用 `libra add` 暂存，然后续作或取消：

### `--continue`

解决冲突后续作进行中的 cherry-pick。索引必须没有未解决的冲突 stage，否则 `--continue` 被拒绝（`LBR-CONFLICT-001`）。它会敲定冲突的那个提交，并应用序列中剩余的提交。

### `--skip`

丢弃当前冲突的提交（将工作树恢复到上一个成功的 tip），并继续序列的其余部分。

### `--abort`

取消进行中的 cherry-pick，并把 HEAD/工作树重置回序列开始之前的状态。

### `--quit`

放弃进行中的 cherry-pick，但不改动索引或工作树（冲突标记保留原样）。

进行中的序列持久化到 SQLite `cherry_pick_state` 表；cherry-pick 进行期间 `merge`/`rebase` 被阻止（`LBR-CONFLICT-002`）。

```bash
# 某次 pick 冲突；解决、暂存、续作：
libra cherry-pick abc1234 def5678
# ... 编辑冲突文件 ...
libra add <resolved-files>
libra cherry-pick --continue

# 或只丢弃冲突的那个提交：
libra cherry-pick --skip

# 或撤销整个序列：
libra cherry-pick --abort
```

### `<commit>...`（位置参数，必需）

要 cherry-pick 的一个或多个提交引用。每个值可以是完整 SHA-1 哈希、缩写哈希、分支名、`HEAD`，或任何解析为提交的引用。提交从左到右应用。

```bash
# 按哈希应用单个提交
libra cherry-pick abc1234

# 按顺序应用多个提交
libra cherry-pick abc1234 def5678 ghi9012
```

### `--json`

输出机器可读 JSON，而不是人类可读文本。见下方[结构化输出](#结构化输出-json-示例)。

### `--quiet`

抑制所有人类可读输出。退出码仍表示成功或失败。

## 常用命令

```bash
# 将单个提交 cherry-pick 到当前分支
libra cherry-pick abc1234

# 按顺序 cherry-pick 多个提交
libra cherry-pick abc1234 def5678

# Cherry-pick 但不提交，用于编辑或组合更改
libra cherry-pick -n abc1234

# Cherry-pick 并在新提交消息中记录原始提交哈希
libra cherry-pick -x abc1234

# 沿第一个父提交 cherry-pick 一个 merge commit，并附 Signed-off-by
libra cherry-pick -m 1 -s <merge-commit>

# 解决冲突后续作
libra add <resolved-files> && libra cherry-pick --continue

# 为 AI 代理或脚本输出 JSON
libra cherry-pick --json abc1234
```

## 人类可读输出

使用自动提交（默认）进行 cherry-pick 时：

```
[def5678] cherry-picked from abc1234
```

不使用自动提交（`-n`）进行 cherry-pick 时：

```
Changes from abc1234 staged. Use 'libra commit' to finalize.
```

## 结构化输出（JSON 示例）

```json
{
  "command": "cherry-pick",
  "data": {
    "picked": [
      {
        "source_commit": "abc1234abcdef1234567890abcdef1234567890ab",
        "short_source": "abc1234",
        "new_commit": "def5678abcdef1234567890abcdef1234567890ab",
        "short_new": "def5678"
      }
    ],
    "no_commit": false
  }
}
```

使用 `--no-commit` 时，`new_commit` 和 `short_new` 为 `null`：

```json
{
  "command": "cherry-pick",
  "data": {
    "picked": [
      {
        "source_commit": "abc1234abcdef1234567890abcdef1234567890ab",
        "short_source": "abc1234",
        "new_commit": null,
        "short_new": null
      }
    ],
    "no_commit": true
  }
}
```

## 设计理由（为什么不同于 Git/jj）

### sequencer 状态存于 SQLite，而非 dotfile

Git 维护 `.git/CHERRY_PICK_HEAD` 与 sequencer 状态文件。Libra 改为把进行中的序列持久化到 SQLite `cherry_pick_state` 表，与“元数据存 SQLite”的约定一致：`DELETE`+`INSERT` 保存在单个事务内，故 sequencer 写入不会半途残留，也不会有可能与 refs 漂移的松散 dotfile。AI-agent 协议与 Git 相同：检测冲突码（`LBR-CONFLICT-001`）、解决、`libra add`，再 `--continue`（或 `--skip`/`--abort`/`--quit`）。

### 行级冲突 hunk

发散路径以行级冲突标记呈现，与 Git 一致：三方合并（base = 父提交树，ours = 当前索引，theirs = 被 pick 的树）仅把发散的 hunk 包在 `<<<<<<< HEAD` / `=======` / `>>>>>>> <short-source>` 之间，两侧共享的行留在标记之外。删除/修改冲突（某一侧缺失）或二进制内容回退为整文件呈现（此时行级合并无意义）。`>>>>>>>` 标签为被 pick 提交的缩写（Libra 省略了 Git 追加的提交主题）。

Git 兼容配置 `merge.conflictStyle` 同样被尊重（与 `libra merge` 一致）：`diff3` 额外在 `||||||| base` 标记与 `=======` 分隔符之间输出共同祖先内容；不支持的值（如 `zdiff3`）在需要渲染冲突时直接报错。详见 [merge 文档](merge.md)。

### 不支持的 Git 选项被拒绝，而非静默忽略

`--strategy`、`-X/--strategy-option`、`--rerere-autoupdate` 以 `LBR-UNSUPPORTED-001`（退出 128）显式拒绝，而非静默忽略，以免脚本误以为某个未实现的行为已生效。Libra 使用单一内置三方合并；自定义合并策略不在范围内。

## 参数对比：Libra vs Git vs jj

| 参数 | Git | jj | Libra |
|-----------|-----|-----|-------|
| 位置提交 | `git cherry-pick <commit>...` | N/A（使用 `jj rebase`） | `libra cherry-pick <commit>...` |
| No-commit 模式 | `--no-commit` / `-n` | N/A | `--no-commit` / `-n`（也支持多提交） |
| 记录来源 | `-x` | N/A | `-x` |
| 签名行 | `--signoff` / `-s` | N/A | `--signoff` / `-s` |
| 编辑消息 | `--edit` / `-e` | N/A | `--edit` / `-e`（机器模式下降级） |
| Mainline 父提交 | `--mainline <n>` / `-m <n>` | N/A | `--mainline <n>` / `-m <n>` |
| 冲突后继续 | `--continue` | N/A | `--continue` |
| 中止进行中操作 | `--abort` | N/A | `--abort` |
| 跳过当前提交 | `--skip` | N/A | `--skip` |
| 退出 sequencer | `--quit` | N/A | `--quit` |
| 快进 | `--ff` | N/A | `--ff` |
| 策略 | `--strategy <s>` | N/A | 拒绝（`LBR-UNSUPPORTED-001`） |
| 策略选项 | `-X <option>` | N/A | 拒绝（`LBR-UNSUPPORTED-001`） |
| GPG 签名 | `--gpg-sign` / `-S` | N/A | `--gpg-sign` / `-S`（经 libra vault） |
| 允许空提交 | `--allow-empty` | N/A | `--allow-empty` |
| 允许空消息 | `--allow-empty-message` | N/A | `--allow-empty-message` |
| 保留冗余提交 | `--keep-redundant-commits` | N/A | `--keep-redundant-commits` |
| 空提交模式 | `--empty=<mode>` | N/A | `--empty=<mode>`（`stop`/`drop`/`keep`） |
| 消息清理 | `--cleanup=<mode>` | N/A | `--cleanup=<mode>`（`strip`/`whitespace`/`verbatim`/`scissors`/`default`；先清理正文/编辑缓冲，再追加 trailer） |
| JSON 输出 | N/A | N/A | `--json` |
| Quiet 模式 | `--quiet` | `--quiet` | `--quiet` |

**注意：** jj 没有直接的 cherry-pick 等价操作。最接近的是 `jj rebase -r <rev> -d <dest>`，它将提交移动或复制到新目标。

## 错误处理

| 代码 | 条件 | 提示 |
|------|-----------|------|
| `LBR-REPO-001` | 不在 libra 仓库内 | 使用 `libra init` 初始化或进入仓库 |
| `LBR-REPO-003` | HEAD detached、`--continue`/`--skip`/`--abort`/`--quit` 时没有进行中的 cherry-pick，或 `--continue` 在错误的分支上 | 切换到分支 / 先发起 cherry-pick / 切回序列所在分支 |
| `LBR-CLI-003` | 无法解析提交引用 | 使用 `libra log` 查找有效提交引用 |
| `LBR-CLI-002` | merge commit 未带 `-m`、`-m` 越界、非法 `--cleanup`/`--empty` mode、空提交未带 `--allow-empty`、冗余提交未带 `--keep-redundant-commits`/`--empty=drop`/`--empty=keep`，或空消息未带 `--allow-empty-message` | 使用提示中指明的标志 |
| `LBR-UNSUPPORTED-001` | 传入了不支持的 Git 选项（`--strategy`、`-X`、`--rerere-autoupdate`） | 去掉该选项；libra cherry-pick 不支持它 |
| `LBR-CONFLICT-001` | Cherry-pick 期间发生冲突（三方冲突，或未跟踪文件会被覆盖） | 解决冲突并 `libra add` 后用 `libra cherry-pick --continue`（或 `--skip`/`--abort`/`--quit`） |
| `LBR-CONFLICT-002` | cherry-pick 进行中时启动了 `merge`/`rebase`，或在进行中的序列上又发起新的 pick | 先完成或取消该 cherry-pick |
| `LBR-IO-001` | 无法加载对象或 cherry-pick 状态 | 检查仓库完整性并重试 |
| `LBR-IO-002` | 无法保存对象、索引，或更新分支引用/状态 | 检查文件系统权限和仓库可写性 |
