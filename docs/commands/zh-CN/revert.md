# `libra revert`

回滚一些已有提交。

## 概要

```
libra revert [-n | --no-commit] [-m | --mainline <parent-number>] [-s | --signoff] [-e | --edit] [--no-edit] [--no-rerere-autoupdate] [--json] [--quiet] <commit>...
libra revert --continue
libra revert --skip
libra revert --abort
```

## 说明

`libra revert` 会创建一个新提交，用于撤销指定提交引入的更改。与会重写历史的 `reset` 不同，`revert` 对共享分支是安全的，因为它保留原始提交，并在其上方添加一个新提交。

该命令通过计算目标提交与其父提交之间的 diff，然后将该 diff 的逆应用到当前工作树和索引来工作。如果结果状态干净，会记录一个新提交，消息格式为 `Revert "<original subject>"`。

revert 提交使用当前 author 与 committer 身份/日期，并在创建提交时遵循与 `libra commit` 相同的 `GIT_AUTHOR_*` 与 `GIT_COMMITTER_*` 环境变量规则。生成的 subject 来自目标提交剥离签名后的消息正文，因此嵌入的 `gpgsig` 块不会被当作原始 subject。

回滚 root 提交（没有父提交的提交）会产生空树，实际效果是撤销初始提交的更改。

该命令要求处于活动分支（不是 detached HEAD）。它接受一个或多个提交引用，按给定顺序依次回滚（每个各自生成一个 revert commit）；冲突会停止该序列，用 `libra revert --continue` 收尾、`libra revert --skip` 跳过当前提交，或 `libra revert --abort` 撤销。当冲突中断多提交回滚时，其后仍待处理的提交会被记住，并在 `--continue`/`--skip` 续作序列时自动回滚。`-n/--no-commit` 与 `-m/--mainline` 仅适用于单个提交。

## 选项

### `-n`, `--no-commit`

将逆向更改应用到索引和工作树，但**不**创建新提交。当你想检查结果，或在提交前调整更改时，这很有用。`--no-commit` 仅适用于单个提交，给定多个提交时会被拒绝。

```bash
# 暂存 revert 但不提交
libra revert -n abc1234

# 查看发生了什么变化
libra diff --cached

# 使用自定义消息提交
libra commit -m "revert abc1234 with adjustments"
```

### `<commit>...`（位置参数，必需）

要回滚的一个或多个提交引用，按给定顺序应用。每个可以是完整 SHA-1 哈希、缩写哈希、分支名、`HEAD`，或任何解析为提交的引用。（仅在 `--continue`/`--skip`/`--abort` 时位置参数可省略。）

```bash
# 回滚最近一次提交
libra revert HEAD

# 按哈希回滚
libra revert abc1234

# 回滚某个分支指向的提交
libra revert feature-branch
```

### `--json`

输出机器可读 JSON，而不是人类可读文本。见下方[结构化输出](#结构化输出-json-示例)。

### `--quiet`

抑制所有人类可读输出。退出码仍然表示成功或失败。

### `-e`, `--edit`

在提交前于编辑器中打开自动生成的 revert 消息（`Revert "<subject>"`），编辑器级联与 `commit` 相同（`$GIT_EDITOR`、`core.editor`、`$VISUAL`、`$EDITOR`）。编辑后的消息会剥离 `#` 注释行并去除首尾空行；结果为空则中止 revert。与 Git 不同，Libra 的 revert 默认**不**打开编辑器——`--edit` 为显式选用。冲突时 `--edit` 会被记住，故 `--continue` 也会打开编辑器。与 `--no-edit` 互斥。

```bash
libra revert HEAD --edit
```

### `--no-edit`

接受自动生成的 revert 消息（`Revert "<subject>"`）而不启动编辑器——这是 Libra 的默认行为，故为对齐 Git 接受的 no-op。要在提交前编辑消息，请用 `-e`/`--edit`（与本标志互斥）。

### `--no-rerere-autoupdate`

不更新 rerere（reuse recorded resolution）索引。为对齐 Git 而接受的 no-op：Libra 无 rerere，无可更新。（Git 的 `--rerere-autoupdate` 未公开。）

## 常用命令

```bash
# 回滚最近一次提交
libra revert HEAD

# 按哈希回滚特定提交
libra revert abc1234

# 回滚但不自动提交（用于编辑或组合）
libra revert -n HEAD

# 为 AI 代理或脚本输出 JSON
libra revert --json HEAD
```

## 人类可读输出

使用自动提交（默认）进行 revert 时：

```
[def5678] Revert commit abc1234
```

不使用自动提交（`-n`）进行 revert 时：

```
Changes staged for revert. Use 'libra commit' to finalize.
```

## 结构化输出（JSON 示例）

```json
{
  "command": "revert",
  "data": {
    "reverted_commit": "abc1234abcdef1234567890abcdef1234567890ab",
    "short_reverted": "abc1234",
    "new_commit": "def5678abcdef1234567890abcdef1234567890ab",
    "short_new": "def5678",
    "no_commit": false,
    "files_changed": 3
  }
}
```

使用 `--no-commit` 时，`new_commit` 和 `short_new` 为 `null`：

```json
{
  "command": "revert",
  "data": {
    "reverted_commit": "abc1234abcdef1234567890abcdef1234567890ab",
    "short_reverted": "abc1234",
    "new_commit": null,
    "short_new": null,
    "no_commit": true,
    "files_changed": 3
  }
}
```

## 设计理由（为什么不同于 Git/jj）

### 多个提交（`<commit>...`）

`libra revert <commit1> <commit2> ...` 按给定顺序依次回滚一系列提交，每个相对前一次结果各自生成一个 revert commit。若序列中某次 revert 冲突，操作就此停止；已完成的保留，其后仍待处理的提交会被记住。随后用 `libra revert --continue` 收尾冲突项（解决后）、`libra revert --skip` 丢弃当前提交，或 `--abort` 撤销；`--continue` 与 `--skip` 会在收尾前自动回滚被记住的待处理提交。注意 `-n/--no-commit` 与 `-m/--mainline` 仅适用于单个提交，给定多个提交时会被拒绝。

### 合并提交支持（`--mainline`）

Git 的 `--mainline <parent-number>` 会选择合并提交的某个父提交，用于计算逆向 diff。Libra 已支持：回滚合并提交时**必须**用 `-m/--mainline <parent-number>` 指定主线父提交（相对该父提交的树计算合并引入的变更），生成的 revert commit 仍只记录单个父提交（当前 HEAD）。对非合并提交传 `-m`、或对合并提交省略 `-m`，均以 exit 128 失败。

### 冲突处理（`--continue`、`--skip`、`--abort`）

冲突的 revert 会向工作树写入三方冲突标记，把 revert 状态记录到 `revert-state.json`，并返回 `LBR-CONFLICT-001`。随后解决冲突并运行 `libra revert --continue` 收尾、`libra revert --skip` 丢弃当前提交继续，或 `libra revert --abort` 恢复 revert 前状态。

1. **显式、对代理友好的错误。** 报告具体路径与错误码，便于代理以编程方式解决冲突并续作。
2. **可预测的状态。** revert 状态集中在单个 `revert-state.json` 文件，而非散落的隐式标记。
3. **序列感知。** 当冲突中断多提交回滚时，其后仍待处理的提交会存入状态文件（`remaining`），故 `--continue`（解决后）与 `--skip`（丢弃当前提交）都会自动完成序列其余部分。`--skip` 在无剩余提交时仅清理状态、不创建提交。

### 冲突模型（三方合并）

Libra 的 revert 以路径级三方合并应用逆向更改。结果无歧义时干净更新文件；与后续更改重叠时，向工作树写入标准冲突标记，把未合并状态与 revert 进度记录到 `revert-state.json`，并返回 `LBR-CONFLICT-001`。随后解决标记并运行 `libra revert --continue`、用 `libra revert --skip` 跳过当前提交，或 `libra revert --abort` 撤销。

## 参数对比：Libra vs Git vs jj

| 参数 | Git | jj | Libra |
|-----------|-----|-----|-------|
| 位置提交 | `git revert <commit>...` | N/A（使用 `jj backout`） | `libra revert <commit>...`（多个，按序回滚） |
| No-commit 模式 | `--no-commit` / `-n` | N/A | `--no-commit` / `-n` |
| 接受默认消息 | `--no-edit` | N/A | `--no-edit`（接受式 no-op；Libra 默认不打开编辑器——用 `-e`/`--edit` 选用） |
| 不更新 rerere | `--no-rerere-autoupdate` | N/A | `--no-rerere-autoupdate`（接受式 no-op；无 rerere） |
| 编辑消息 | `-e`/`--edit` | N/A | `-e`/`--edit`（在生成消息上打开编辑器；与 Git 不同，Libra 默认不打开，需显式选用） |
| Mainline 父提交 | `--mainline <n>` / `-m <n>` | N/A | `--mainline <n>` / `-m <n>`（合并提交必需） |
| 冲突后继续 | `--continue` | N/A | `--continue`（解决冲突后；自动续作剩余提交） |
| 中止进行中操作 | `--abort` | N/A | `--abort`（恢复 revert 前状态） |
| 跳过当前提交 | `--skip` | N/A | `--skip`（丢弃冲突提交，继续序列） |
| 策略 | `--strategy <s>` | N/A | 不支持 |
| 策略选项 | `-X <option>` | N/A | 不支持 |
| GPG 签名 | `--gpg-sign` / `-S` | N/A | 不支持（计划中） |
| JSON 输出 | N/A | N/A | `--json` |
| Quiet 模式 | `--quiet` | N/A | `--quiet` |
| 变更文件数量 | N/A | N/A | 包含在 JSON 输出中 |

**注意：** jj 使用 `jj backout -r <rev>` 作为 `git revert` 的等价操作。它会创建一个新提交，该提交是目标修订的逆。

## 错误处理

| 代码 | 条件 | 提示 |
|------|-----------|------|
| `LBR-REPO-001` | 不在 libra 仓库内 | 使用 `libra init` 初始化或进入仓库 |
| `LBR-REPO-003` | HEAD detached（不在分支上） | 使用 `libra switch <branch>` 切换到分支 |
| `LBR-CLI-003` | 无法解析提交引用 | 使用 `libra log` 查找有效提交引用 |
| `LBR-CLI-002` | 合并提交缺 `-m`、对非合并提交传 `-m`，或父编号越界（exit 128）；或在 `-e`/`--edit` 下未配置编辑器、编辑器中止或消息为空（exit 129） | 合并提交传有效 `-m <父编号>`（非合并提交省略）；`--edit` 需设置 `$GIT_EDITOR`/`core.editor` 并保存非空消息 |
| `LBR-CONFLICT-001` | 文件已被后续提交修改，产生冲突 | 解决冲突后 `libra revert --continue`、用 `libra revert --skip` 跳过当前提交，或 `libra revert --abort` 取消 |
| `LBR-IO-001` | 无法加载对象（提交、树、blob） | 检查仓库完整性 |
| `LBR-IO-002` | 无法保存对象、索引或更新 HEAD | 检查文件系统权限和仓库可写性 |
