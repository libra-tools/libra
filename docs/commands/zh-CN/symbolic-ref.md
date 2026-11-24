# `libra symbolic-ref`

读取或更新 Libra 的符号 `HEAD` 引用。

## 概要

```bash
libra symbolic-ref [--short] [--quiet] [-d] [HEAD]
libra symbolic-ref HEAD refs/heads/<branch>
```

## 说明

`libra symbolic-ref` 是一个与 Git 兼容的 plumbing 命令，用于检查或修改存储在 `HEAD` 中的符号引用。Libra 目前支持本地 `HEAD` 符号引用。其他符号引用会被拒绝，因为 Libra 将引用存储在 SQLite 中，而不是 `.git/` 下的 loose 文件中。

当 `HEAD` 指向某个分支时，读取形式会打印 `refs/heads/<branch>`。当 `HEAD` 处于 detached 状态时，命令会以 invalid-target 错误退出。使用 `--quiet` 时，Libra 会抑制面向用户的提示，但仍会通过正常的结构化错误契约报告失败。

更新形式成功时不会产生人类可读输出。

`refs/heads/<branch>` 形态的名字存于 Repository 的 `symbolic_ref` 表（PM-01）：读取输出完整目标；写入只接受 `refs/heads/<branch>` 或 `refs/remotes/<remote>/<branch>`（目标可为悬空），同名本地实体分支已存在时以 `LBR-CLI-003` 拒绝；`-d` 删除该行；其它命名空间以 `LBR-CLI-003` 拒绝。`libra checkout -f <symref-branch>` 会把 `refs/heads/*` 符号引用解析一层到其目标的提交并以分离 HEAD 检出（Libra 的 `HEAD` 只能附着本地实体分支，属有意差异）；目标本身仍是符号引用时按"分支不存在"处理（链式解析延后）。

`refs/remotes/<remote>/HEAD` 是 Libra 支持的另一类非本地符号引用：读取时打印 `refs/remotes/<remote>/<branch>`（该行不存在时以 `LBR-CLI-003` 失败）；写入只接受**同一远端**下的目标 `refs/remotes/<remote>/<branch>`，其它远端或 `refs/heads/*` 以 `LBR-CLI-003` 拒绝（有意差异——该行存的是分支名而非完整引用）；`-d` 删除该行。

## 选项

| 选项 | 说明 |
|--------|-------------|
| `--short` | 只打印分支名，例如 `main` |
| `-q`, `--quiet` | 当 `HEAD` 不是符号引用时抑制额外指导 |
| `-d`, `--delete` | 为 Git `t1401` 契约接受，但永不删除任何东西：`-d HEAD` 以 128 退出并报 `deleting 'HEAD' is not allowed`；其它名称以 128 退出并报 `Cannot delete <name>, not a symbolic ref` |
| `HEAD` | 要检查或更新的符号引用。省略时默认为 `HEAD` |
| `refs/heads/<branch>` | `HEAD` 的新符号目标 |

## 示例

```bash
libra symbolic-ref HEAD
libra symbolic-ref --short HEAD
libra symbolic-ref HEAD refs/heads/main
libra --json symbolic-ref HEAD
```

## 结构化输出

```json
{
  "ok": true,
  "command": "symbolic-ref",
  "data": {
    "name": "HEAD",
    "target": "refs/heads/main",
    "short": "main",
    "action": "read"
  }
}
```

对于更新，`action` 为 `set`。

## 兼容性说明

- Libra 仅支持 `HEAD`。
- 更新目标必须是 `refs/heads/` 下的本地分支引用。
- 该命令可以让 `HEAD` 指向一个尚未出生的分支，这与 Git 在分支提交前也能存储符号分支目标的能力一致。
