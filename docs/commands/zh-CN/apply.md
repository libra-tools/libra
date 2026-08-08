# `libra apply`

检查一个补丁是否能干净应用 —— `git apply --check` 的 MVP。本版本**仅校验**：解析补丁、对每个目标路径做安全检查、把每个文件的 hunk 试应用到当前工作树，**不写入任何内容**。

## 用法

```
libra apply --check [-p<n>] [<patch>...]
```

## 说明

`apply --check` 读取一个或多个补丁（来自命名文件，或在未给文件时来自 stdin），拆分为按文件的片段，对每个文件：

1. 解析 hunk（格式错误即致命错误）；
2. 解析目标路径，剥离 `<n>` 个前导组件（`-p<n>`，默认 1），并拒绝绝对路径、含空/`.`/`..` 组件、含 NUL、指向 `.libra/` 内部、或穿过已有 symlink 组件的路径；
3. 把 hunk 试应用到当前文件内容（源为 `/dev/null` 的新文件补丁以空内容为基底）。

所有文件都能应用则退出码 0；任一文件不能应用则为 1。工作树绝不被修改。真正应用补丁（临时文件 + 原子 rename）是计划中的后续扩展；当前必须带 `--check`。

补丁大于 64 MiB 会被拒绝。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `--check` | 仅校验、不写入（本版本必需）。 | `libra apply --check fix.patch` |
| `-p<n>` | 从每个路径剥离 `<n>` 个前导组件（默认 1）。 | `libra apply --check -p0 fix.patch` |
| `--json` / `--machine` | 结构化输出：`{ applies, files }`。 | `libra --json apply --check fix.patch` |

## 退出码

| 退出码 | 含义 |
|--------|------|
| `0` | 补丁可干净应用。 |
| `1` | 补丁不能应用（上下文冲突或目标缺失）。 |
| `128` | 不在仓库内、未带 `--check`、补丁格式错误/超大/非 UTF-8，或目标路径不安全。 |

## 示例

```bash
# 这个补丁能应用到当前树吗？
libra apply --check fix.patch && echo "clean"

# 无 a/ b/ 前缀的补丁
libra apply --check -p0 fix.patch

# 来自管道
git format-patch -1 --stdout | libra apply --check
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 检查补丁 | `libra apply --check p` | `git apply --check p` |
| 路径剥离 | `libra apply --check -p0 p` | `git apply --check -p0 p` |

差异与延后项：真正应用补丁（不带 `--check`）、`--index` / `--cached`、`--3way`、`--reverse`、`--unidiff-zero`、二进制补丁、rename/mode hunk 暂不支持。绝不写入冲突标记 —— `--check` 只报告。
