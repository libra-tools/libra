# `libra rev-parse`

解析修订名，并打印规范化的提交 ID、符号引用或仓库路径。

## 概要

```bash
libra rev-parse [OPTIONS] [SPEC]...
```

## 说明

`libra rev-parse` 会将类似修订的输入解析为以下三种形式之一：

- 完整提交 ID（默认）
- 使用 `--short` 得到的短提交 ID
- 使用 `--abbrev-ref` 得到的符号分支名

它还支持 `--show-toplevel`，用于打印工作树的绝对仓库根目录。未提供 `<SPEC>` 时默认为 `HEAD`；提供多个 `<SPEC>` 时各自单独成行解析。输出过滤标志（`--flags`/`--no-flags`/`--revs-only`/`--no-revs`）则把每个参数分类为 flag、revision 或 path，并打印过滤后的子集。

## 选项

| 标志 | 说明 |
|------|-------------|
| `--short` | 打印无歧义的缩写对象 ID。 |
| `--sq` | 对解析出的对象名做单引号 shell 引用，便于安全地交给 shell 消费。仅影响解析出的修订输出，不影响 `--show-toplevel` 等查询模式。 |
| `--abbrev-ref` | 打印符号分支名，而不是提交哈希。 |
| `--symbolic-full-name` | 将 spec 解析为完整 ref 名（`refs/heads/<分支>`、`refs/tags/<标签>`、`refs/remotes/<远程>/<分支>`，分离 HEAD 时为 `HEAD`）。有效但非 ref 的对象不输出（退出码 0）；不可解析名以退出码 128 失败。 |
| `--symbolic` | 按“尽量接近原始输入”的形式打印 spec：可解析的 ref、revision 表达式或对象 id 一律**原样回显**（如 `main` 保持 `main`，而非 `refs/heads/main`）。不可解析名以退出码 128 失败。与 `--symbolic-full-name`/`--short`/`--abbrev-ref` 互斥。 |
| `--flags` | 输出过滤模式：把每个 `<SPEC>` 参数分类，仅打印 flag（以 `-` 开头、在 `--` 之前）与解析出的 revision，丢弃非 flag 的 path。 |
| `--no-flags` | 输出过滤模式：丢弃 flag 参数，保留解析出的 revision 与非 flag 的 path。 |
| `--revs-only` | 输出过滤模式：仅打印解析为 revision 的参数（以对象名输出），丢弃 flag 与非 revision 的 path。 |
| `--no-revs` | 输出过滤模式：丢弃 revision 参数，保留 flag 与非 revision 的 path。 |
| `--show-toplevel` | 打印顶层工作树的绝对路径。 |
| `--is-shallow-repository` | 当 `.libra/shallow` 至少包含一个 shallow boundary 时打印 `true`，否则打印 `false`。 |
| `--git-dir` | 打印 `.libra` 目录路径（Libra 的 `$GIT_DIR`）；在 Libra 中始终为绝对路径。 |
| `--absolute-git-dir` | 同 `--git-dir`，但始终为规范化后的绝对路径。（Libra 中 `--git-dir` 已是绝对路径，故两者一致。） |
| `<SPEC>...` | 要解析的修订（可多个）。省略时默认为 `HEAD`；多个时各自解析。 |

## 常用命令

```bash
libra rev-parse
libra rev-parse HEAD~1
libra rev-parse --short HEAD
libra rev-parse --abbrev-ref HEAD
libra rev-parse --show-toplevel
libra rev-parse --is-shallow-repository
libra rev-parse --absolute-git-dir
libra --json rev-parse --short HEAD
```

## 人类可读输出

默认输出为包含已解析值的单行。

```text
abc1234def5678901234567890abcdef12345678
```

使用 `--short`：

```text
abc1234
```

使用 `--abbrev-ref`：

```text
main
```

使用 `--show-toplevel`：

```text
/home/alice/project
```

## 结构化输出

```json
{
  "ok": true,
  "command": "rev-parse",
  "data": {
    "mode": "short",
    "input": "HEAD",
    "value": "abc1234"
  }
}
```

`mode` 是 `resolve`、`short`、`abbrev_ref`、`symbolic_full_name`、`symbolic`、`show_toplevel`、`show_prefix`、`show_cdup`、`is_inside_work_tree`、`is_inside_git_dir`、`is_bare_repository`、`git_dir` 或 `absolute_git_dir` 之一。

单个 `<SPEC>` 时，`data` 是上述单个对象；**多个** `<SPEC>` 时 `data` 是这些对象按顺序组成的 JSON **数组**（每个 spec 一项）。在**输出过滤**模式（`--flags`/`--no-flags`/`--revs-only`/`--no-revs`）下，`data` 是过滤后 token 的 JSON **字符串数组**（revision 为解析出的对象名，保留的 flag/path 原样）。

## 参数对比：Libra vs Git vs jj

| 功能 | Libra | Git | jj |
|---------|-------|-----|----|
| 解析完整提交 ID | `rev-parse <spec>` | `git rev-parse <spec>` | `jj log -r <rev> --no-graph -T commit_id` |
| 缩写提交 ID | `--short` | `--short` | `jj log -r <rev> -T change_id.short()` |
| 符号分支名 | `--abbrev-ref` | `--abbrev-ref` | N/A |
| 完整 ref 名 | `--symbolic-full-name` | `--symbolic-full-name` | N/A |
| 符号（原样）名 | `--symbolic` | `--symbolic` | N/A |
| 输出过滤 | `--flags`/`--no-flags`/`--revs-only`/`--no-revs` | 同 | N/A |
| Shell 引用输出 | `--sq` | `--sq` | N/A |
| 工作树根目录 | `--show-toplevel` | `--show-toplevel` | `jj root` |
| JSON 输出 | `--json` | 无 | 无 |

`--` 分隔符在所有模式下都把 revision 与 path 分开（`rev-parse <rev> -- <path>`）：`--` 之后的参数一律是 path 而非 revision，且在有 path 输出的模式下会回显 `--`。单 revision 模式 `--verify`、`--short` 只打印解析出的那一个对象（不打印 path）。

> **与 git 的有意差异**：`--verify` 或 `--short` 与任一输出过滤标志（`--flags`/`--no-flags`/`--revs-only`/`--no-revs`）组合会以用法错误（`LBR-CLI-002`，退出码 129）被拒绝。git 在该组合下行为不确定，Libra 选择拒绝而非猜测。

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 无效目标引用 | `LBR-CLI-003` | 129 |
| `--verify`/`--short` 与输出过滤标志组合 | `LBR-CLI-002` | 129 |
| 无效工作树状态 | `LBR-REPO-003` | 128 |
| 无法读取仓库元数据 | `LBR-IO-001` | 128 |
| 存储的引用/配置损坏 | `LBR-REPO-002` | 128 |
