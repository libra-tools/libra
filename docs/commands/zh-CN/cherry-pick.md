# `libra cherry-pick`

应用一些已有提交引入的更改。

**别名：** `cp`

## 概要

```
libra cherry-pick [-n|--no-commit] [-x] [-s|--signoff] [-e|--edit]
                  [-m <n>|--mainline <n>] [--ff] [-S|--gpg-sign]
                  [-X <ours|theirs>] [--rerere-autoupdate | --no-rerere-autoupdate]
                  [--allow-empty] [--allow-empty-message] [--keep-redundant-commits]
                  [--empty=<mode>] [--cleanup=<mode>] [--json] [--quiet] <commit>...
libra cherry-pick (--continue | --skip | --abort | --quit)
```

## 说明

`libra cherry-pick` 将指定提交引入的更改应用到当前分支。对于每个具名提交，Libra 会计算该提交与其父提交之间的 diff，将得到的 changeset 应用到当前索引和工作树，并且（除非给出 `--no-commit`）记录一个新提交。需要在新提交消息中包含原始提交哈希时，使用 `-x`。

这适合在不合并的情况下，将一个分支上的提交选择性应用到另一个分支。提供多个提交时，它们会按给定顺序应用，每个提交都会先成为当前分支上的新提交，然后再处理下一个。

该命令要求处于活动分支（不是 detached HEAD）。非 merge commit 直接应用；cherry-pick merge commit 需用 `-m <n>`/`--mainline <n>` 指定沿哪个父提交做 diff。

自动提交的 cherry-pick 会保留源提交的 author metadata（姓名、邮箱、author date 与时区）。committer 使用当前身份/日期，并遵循 `GIT_COMMITTER_*` 覆盖。带签名的源提交会先剥离 `gpgsig` 消息块，再执行消息清理与 trailer 追加，因此签名块不会成为重放后的 subject。

子模块永不参与合并（见 `docs/commands/zh-CN/merge.md`）：若本次 pick 的三路输入（父提交树 / 当前索引 / 被 pick 的树）中 `160000` gitlink 记录的 object id 不一致，cherry-pick 会在写入任何内容之前被拒绝，错误码 `LBR-UNSUPPORTED-001`，消息包含路径（`cherry-pick would have to merge the submodule (gitlink) entry '<path>': Libra does not support submodules`）。三侧一致的 gitlink 则原样保留，不再被丢弃。

当某提交无法干净应用时，Libra 执行三方 apply（base = 父提交树，ours = 当前索引，theirs = 被 pick 的树），并把未解决的发散路径写入索引（stage 1/2/3）与工作树（行级冲突标记，与 Git 一致）。`-X ours/theirs` 可只解决重叠 hunk 而保留 clean 变更。进行中的序列持久化到统一 SQLite `sequence_state` 表，因此你可以解决剩余冲突后用 `--continue` 续作、用 `--skip` 丢弃冲突提交，或用 `--abort`/`--quit` 撤销整个序列。cherry-pick 序列进行期间，其他 sequencer 操作被阻止（`LBR-CONFLICT-002`）。

新的 cherry-pick 在索引存在未合并条目时拒绝开始：在解析任何目标、写入索引、工作树、引用或序列状态之前，以 exit 128 与 `LBR-CONFLICT-001` 退出，并列出最多 10 条未合并路径（Git 同样拒绝）。逐条解决后 `libra add`，或用 `libra reset --hard` 放弃冲突，然后重新执行 cherry-pick；该拒绝不产生序列，`--continue`、`--skip`、`--abort`、`--quit` 对它不适用。发生冲突的 `-n`/`--no-commit` pick 同样不产生可继续的序列：解决后 `libra add`（或 `libra rm`）并执行 `libra commit`，或用 `libra reset --hard` 放弃已暂存的 pick。会覆盖未跟踪工作树文件的 pick 在该提交写入任何内容之前被拒绝：移走或删除提示中的文件后重新执行命令；若逐提交序列中之前的提交已落地，则改为执行 `libra cherry-pick --continue`（会重新尝试停住的提交，Git 则会丢弃它）；若 `--no-commit` 运行中之前的 pick 已暂存，则没有序列，用 `-n` 重新 pick 剩余提交，或用 `libra reset --hard` 放弃全部。

逐提交模式的多提交 pick 在应用第一个提交之前就记录序列，并随每个应用的提交推进（普通提交与分支更新在同一数据库事务里完成；`--ff` 快进则在重置前先写入恢复标记），因此运行被中断、或在之前的提交落地后遇到非冲突错误而停止时，序列保持进行中：用 `libra cherry-pick --continue`、`--skip` 或 `--abort` 收尾。`--ff` 会沿用到 `--continue` 与 `--skip` 应用的提交。`--skip` 与 `--abort` 在重置之前先记录自身；若被中断，重跑同一命令即可完成，完成前 `--continue` 以 `LBR-REPO-003` 拒绝。

该内容合并与 `libra merge` 完全共用路径上的 `merge` gitattribute 及
`merge.default` 回退：内建 `text`、`binary`、`union`，未知名称回退
`text`。因此 union driver 可把重叠 pick 解析为 current 内容后接 picked
内容；binary driver 冲突则保留完整的存活侧（current 存在时优先），不插入文本标记。

已停止的 pick 不会比它所在的工作树活得更久：之后的 reset 会结束已停止的单提交 pick（在清除索引冲突阶段后），下一次 cherry-pick 可以正常开始。解决冲突后再执行一次之后的 commit 会结束已停止的单提交 pick。多提交序列会保留剩余提交并记录被停提交已结束；`--continue` 不会重新提交已在序列外结束的停止项，而是继续应用剩余提交。

工作树物化按条目 mode 语义执行（plan issues/470 FM-02）：文件按条目权限位创建（`100755`→`0777`、`100644`→`0666`）并受进程 `umask` 约束，经同目录临时文件原子替换；索引/树条目保留 mode（`100755`/`100644`/`120000`）。

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

### `-X <ours|theirs>`、`--strategy-option=<ours|theirs>`

仅对三方应用中真正重叠的冲突 hunk 选择一侧：`ours` 为当前 index/HEAD，`theirs` 为被 pick 的提交；两侧不冲突的 clean hunk 仍会合并。该参数可重复，最后一个值生效；add/add 与 modify/delete 冲突按所选侧处理。有效值会随多提交 sequencer 状态保存并在续作时复用。

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

进行中的序列持久化到统一 SQLite `sequence_state` 表；cherry-pick 进行期间其他 sequencer 操作被阻止（`LBR-CONFLICT-002`）。

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

Git 维护 `.git/CHERRY_PICK_HEAD` 与 sequencer 状态文件。Libra 把进行中的序列持久化到统一 SQLite `sequence_state` 表，并与跨操作 sequencer mutex 共用同一状态源。保存是事务性的，不会留下半写状态，也没有可能与 refs 漂移的松散 dotfile。AI-agent 协议与 Git 相同：检测冲突码（`LBR-CONFLICT-001`）、解决、`libra add`，再 `--continue`（或 `--skip`/`--abort`/`--quit`）。

### 行级冲突 hunk

发散路径以行级冲突标记呈现，与 Git 一致：三方合并（base = 父提交树，ours = 当前索引，theirs = 被 pick 的树）仅把发散的 hunk 包在 `<<<<<<< HEAD` / `=======` / `>>>>>>> <abbrev7> (<subject>)` 之间，两侧共享的行留在标记之外。删除/修改冲突（某一侧缺失）或二进制内容回退为整文件呈现（此时行级合并无意义）。`merge.conflictStyle=diff3` 时祖先标签为 `parent of <abbrev7> (<subject>)`。

Git 兼容配置 `merge.conflictStyle` 同样被尊重（与 `libra merge` 一致）：`merge` 重新 diff 双方 postimage 以移出共同边缘和较长共同片段；`diff3` 加入完整 ancestor 块；`zdiff3` 保留该 ancestor 块并移出共同前后缀。遇到未知值且确实需要内容合并时，会在索引或工作树写入前直接报错。所有可识别输入行尾均为 CRLF 时 marker 行也使用 CRLF，否则使用 LF。详见 [merge 文档](merge.md)。

### 自定义策略仍保持显式边界

内置三方应用已支持 `-X ours/theirs`，且只偏向冲突 region。`--rerere-autoupdate` 会暂存回放解法，`--no-rerere-autoupdate` 保持未暂存；最后出现的标志生效，两个均省略时继承 `rerere.autoUpdate`。rerere 按规范化 hunk 两侧匹配，并且只写入干净的三方回放。所选值会保留在 SQLite sequencer state 中，故 `--continue` 仍保持它。rerere 禁用时两个标志都是 no-op。外部/自定义 `--strategy <name>` 仍以 `LBR-UNSUPPORTED-001`（退出 128）显式拒绝。

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
| 策略选项 | `-X <option>` | N/A | `-X ours/theirs`（可重复，last-wins，仅偏向冲突 hunk） |
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
| `LBR-REPO-003` | HEAD detached、`--continue`/`--skip`/`--abort`/`--quit` 时没有进行中的 cherry-pick、`--continue` 在错误的分支上，或被中断的 `--skip`/`--abort` 尚未完成（完成前 `--continue` 拒绝） | 切换到分支 / 先发起 cherry-pick / 切回序列所在分支 / 重跑被中断的 `--skip` 或 `--abort`（或用 `--quit` 放弃序列） |
| `LBR-REPO-003` | `--continue` 时之后的 reset 已结束该停止提交 | 用 `libra cherry-pick --skip` 消化剩余提交，或用 `--quit` 清除序列并保留 reset 结果；`--abort` 则恢复序列开始前状态（丢弃之后的已跟踪改动，包括 reset 目标） |
| `LBR-REPO-002` | 序列行声称被停提交已结束，却没有剩余提交——任何写入方都不会产生这种形态 | 状态原样不动：用 `libra cherry-pick --abort` 或 `--quit` 结束序列 |
| `LBR-CLI-003` | 无法解析提交引用 | 使用 `libra log` 查找有效提交引用 |
| `LBR-CLI-002` | merge commit 未带 `-m`、`-m` 越界、非法 `--cleanup`/`--empty` mode、空提交未带 `--allow-empty`、冗余提交未带 `--keep-redundant-commits`/`--empty=drop`/`--empty=keep`，或空消息未带 `--allow-empty-message` | 使用提示中指明的标志 |
| `LBR-UNSUPPORTED-001` | 传入了不支持的自定义 `--strategy`，**或** pick 序列的输入中存在需要裁决的 `160000` gitlink（submodule） | 去掉 `--strategy`；gitlink 情形请在 Libra 之外解决 submodule 指针，或从相关提交中移除该条目——拒绝发生在任何索引/工作树/状态写入之前 |
| `LBR-CONFLICT-001` | 三方冲突使逐提交 pick 序列停止 | 解决冲突并 `libra add` 后用 `libra cherry-pick --continue`（或 `--skip`/`--abort`/`--quit`） |
| `LBR-CONFLICT-001` | 索引已有未合并条目时新的 pick 被拒绝（不发起序列） | 逐条解决并 `libra add`（或用 `libra reset --hard` 放弃）后重新执行 cherry-pick；`--continue`/`--skip`/`--abort` 不适用 |
| `LBR-CONFLICT-001` | `--no-commit` pick 因冲突停止（没有可续作的序列） | 解决后 `libra add`（或 `libra rm`）并执行 `libra commit`，或用 `libra reset --hard` 放弃已暂存的 pick；`--continue`/`--skip`/`--abort` 不适用 |
| `LBR-CONFLICT-001` | pick 会覆盖未跟踪的工作树文件（在该提交写入索引、工作树或引用之前拒绝） | 移走或删除提示中的文件。若本次尚未应用或暂存任何内容，则未写入任何内容，重新执行同一命令；若逐提交序列中之前的提交已落地，序列停在该提交之前，执行 `libra cherry-pick --continue`（会重新尝试该提交）、`--skip` 或 `--abort`；若 `--no-commit` 运行中之前的 pick 已暂存，则没有序列：用 `-n` 重新 pick 剩余提交，或用 `libra reset --hard` 放弃全部 |
| `LBR-CONFLICT-002` | cherry-pick 进行中时启动了其他 sequencer 操作（`merge`、`rebase` 或 `revert`），或在进行中的序列上又发起新的 pick | 先完成或取消进行中的序列 |
| `LBR-IO-001` | 无法加载对象或 cherry-pick 状态 | 检查仓库完整性并重试 |
| `LBR-IO-002` | 无法保存对象、索引，或更新分支引用/状态 | 检查文件系统权限和仓库可写性 |

Cherry-pick revision 使用 sidecar Change ID 投影和类型化 predecessor 谱系。已有 commit header
仍可用于导入读取，但新提交不依赖也不会注入 `change-id` header。

## Issue #477 notes

--continue 不会重新提交已在序列外结束的停止项
冲突标记以缩写提交与主题标注被 pick 的一侧
