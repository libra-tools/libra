# `libra mergetool`

为当前工作树中每个普通内容冲突运行已配置的外部合并工具。

## 概要

```text
libra mergetool [--tool <tool>]
libra mergetool --tool-help
```

## 行为

`libra mergetool` 读取未解决的 index stage。对普通文件冲突，它会在仅当前用户可访问的临时目录中建立：

- `BASE`：stage 1；add/add 冲突时为空文件；
- `LOCAL`：stage 2（当前分支版本）；
- `REMOTE`：stage 3（待合并分支版本）；
- `MERGED`：可编辑的冲突工作树文件副本。

工具成功解决后，Libra 将 `MERGED` 写回工作树，并用一个 stage 0 条目替换该路径的 stage 1/2/3。此命令不会创建最终 merge commit；全部冲突解决并暂存后运行 `libra merge --continue`。

默认判定遵循 Git：工具启动后 `MERGED` 的修改时间发生变化即视为已解决。若没有变化，交互式终端会询问确认；stdin EOF 或非交互运行一律视为未解决，绝不暂存该路径。仅当工具的退出码能可靠表达成功时，才设置 `mergetool.<tool>.trustExitCode=true`；此时只看退出码。

符号链接、文件模式和 modify/delete 冲突不会被静默跳过或交给工具。Libra 会指出路径，并要求手工解决该冲突形状。若冲突工作树路径自身或任一父级组件是符号链接，Libra 同样会拒绝处理；它在读取初始 `MERGED` 副本、创建备份或写回解决结果时绝不跟随该链接。

## 工具选择与配置

`--tool <tool>` 优先于 `merge.tool`；两者都未设置时默认尝试 `vimdiff`。

内建描述包括 `vimdiff`、`nvimdiff`、`meld`、`vscode` 与 `opendiff`。默认可执行文件从 `PATH` 查找；`mergetool.<tool>.path` 优先于该查找。

其它工具可配置 `mergetool.<tool>.cmd`。它会像 Git 一样由 `sh -c` 求值，并导出 `BASE`、`LOCAL`、`REMOTE`、`MERGED` 环境变量。该命令属于受信任的本地配置；不要从不受信任的仓库或工单复制。Libra 不会把来自冲突路径的内容拼接进命令字符串。

```bash
libra config set merge.tool review
libra config set mergetool.review.cmd 'review-tool "$LOCAL" "$REMOTE" "$MERGED"'
libra config set mergetool.review.trustExitCode true
```

`mergetool.keepBackup` 默认 `true`。成功解决后会把原有冲突标记文件保存为 `<path>.orig`；设为 `false` 可停止创建新的备份。

当前只支持 `mergetool.keepBackup` 及每个工具的 `.cmd`、`.path`、`.trustExitCode` 设置。其它任意 `mergetool.*` 键都会以可操作错误 fail closed（DEFER-11），不会被静默忽略。GUI 平台探测细节和完整的上游工具目录仍暂缓实现；若内建工具不适用，请使用自定义 `.cmd`。

## 退出状态

- `0`：所有选定冲突路径均已解决并暂存，或已显示 `--tool-help`。
- 非零：没有未解决路径、工具未知或不可用、路径仍未解决，或遇到必须手工处理的冲突形状。未解决路径不会被暂存。

## 示例

```bash
# 查看内建工具，不启动解析器。
libra mergetool --tool-help

# 用 merge.tool 配置的工具解决所有普通冲突。
libra mergetool

# 单次覆盖默认工具。
libra mergetool --tool vimdiff

# 所有冲突解决并暂存后完成 merge。
libra merge --continue
```
