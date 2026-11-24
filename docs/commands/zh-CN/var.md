# `libra var`

打印 Libra 的身份变量（`GIT_AUTHOR_IDENT`、`GIT_COMMITTER_IDENT`）。

## 用法概要

```bash
libra var <NAME>
libra var -l
libra var --json <NAME>
```

## 说明

`libra var` 是一个与 Git 兼容的小型 plumbing 命令，按 Git 的 ident 形式打印 Libra 会写入提交签名的身份：

```text
Name <email> <unix-timestamp> <±hhmm>
```

身份取自 `user.name` 与 `user.email`（先本地配置、再全局配置），时间戳与时区取自当前本地时间，因此 `TZ` 环境变量决定打印出的偏移。

`GIT_AUTHOR_IDENT` 解析的正是 `libra commit` 会写入的**作者**身份：名字按 `GIT_AUTHOR_NAME` > `GIT_COMMITTER_NAME` > `LIBRA_COMMITTER_NAME` > `user.name`，邮箱按 `GIT_AUTHOR_EMAIL` > `GIT_COMMITTER_EMAIL` > `EMAIL` > `LIBRA_COMMITTER_EMAIL` > `user.email`，并生效 `GIT_AUTHOR_DATE`（非法值以 `LBR-CLI-002` 失败）。设置 `user.useConfigOnly` 时忽略身份类环境变量，但日期仍然生效。只校验作者侧日期，因此非法的 `GIT_COMMITTER_DATE` 不影响该变量。

`GIT_COMMITTER_IDENT` 对应提交者侧：名字按 `GIT_COMMITTER_NAME` > `GIT_AUTHOR_NAME` > `LIBRA_COMMITTER_NAME` > `user.name`，邮箱按 `GIT_COMMITTER_EMAIL` > `GIT_AUTHOR_EMAIL` > `EMAIL` > `LIBRA_COMMITTER_EMAIL` > `user.email`，并生效、校验 `GIT_COMMITTER_DATE`。只校验提交者侧日期，因此非法的 `GIT_AUTHOR_DATE` 不影响该变量。

两个变量因此报告的都是 `libra commit` 实际会写入的值；跨角色回退与"环境变量先于配置"的顺序是 Libra 已记录的有意差异。

`-l` 按 `GIT_COMMITTER_IDENT`、`GIT_AUTHOR_IDENT` 的顺序逐行输出 `NAME=value`，并跳过身份无法解析的变量（与 Git 的 `var -l` 一致）。未知名称、缺少名称或多参数以 `LBR-CLI-002` 失败；身份缺失以 `LBR-AUTH-001` 失败；命令需要仓库（仓库外 `LBR-REPO-001`，与 Git 的有意差异）。

`GIT_EDITOR` 报告 `libra commit` 会启动的编辑器：`GIT_EDITOR` > `core.editor` > `VISUAL` > `EDITOR`。四者都未设置时：`TERM` 可用（已设置且不为 `dumb`）则输出 `vi`，否则以退出码 1 结束且 stdout 为空。`GIT_SEQUENCE_EDITOR` 先走 `rebase -i` 的解析链（`GIT_SEQUENCE_EDITOR` > `sequence.editor`，其后是 `GIT_EDITOR` 的解析结果），回落规则与前者一致。

## 选项

| 选项 | 说明 |
|--------|-------------|
| `-l`, `--list` | 列出所有支持的变量，形式为 `NAME=value` |
| `--json` | 全局标志：输出 `{"command":"var","data":…,"ok":true}` 信封 |
| `<NAME>` | `GIT_COMMITTER_IDENT` 或 `GIT_AUTHOR_IDENT` |

## 示例

```bash
libra var GIT_COMMITTER_IDENT
# C O Mitter <c@d> 1795494061 +0800

libra var -l
# GIT_COMMITTER_IDENT=C O Mitter <c@d> 1795494061 +0800
# GIT_AUTHOR_IDENT=C O Mitter <c@d> 1795494061 +0800

libra --json var GIT_AUTHOR_IDENT
# {"command":"var","data":{"name":"GIT_AUTHOR_IDENT","value":"C O Mitter <c@d> 1795494061 +0800"},"ok":true}
```
