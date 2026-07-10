# `libra check-ignore`

报告哪些路径被 Git/Libra ignore 规则忽略（排除）——等价于 `git check-ignore`，同时保留 Libra 扩展文件。

Libra 会读取 Git 标准来源（`.gitignore`、`.git/info/exclude`、`core.excludesFile`）以及 Libra 扩展来源（`.libraignore`）。同一目录内 `.libraignore` 优先于 `.gitignore`；更近目录的来源优先于祖先目录；`.git/info/exclude` 和 `core.excludesFile` 是较低优先级 fallback。模式语法使用 Git ignore 语法。

## 用法

```
libra check-ignore [-v] [-n] [-z] [--no-index] <pathname>...
libra check-ignore [-v] [-n] [-z] [--no-index] --stdin
```

## 说明

对每个 `<pathname>`（命令行给出，或经 `--stdin` 读取），`check-ignore` 按当前 ignore 来源判定，并打印**被忽略**（排除）的路径。它是只读查询，不修改 index 或工作树。

默认情况下，已被 index 跟踪的路径会被报告为**未忽略**（显式 `add` 覆盖规则）。使用 `--no-index` 可对已跟踪路径也按纯模式匹配上报——用于调试为何某路径未如预期被忽略。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `<pathname>...` | 要检查的一个或多个路径。与 `--stdin` 互斥。 | `libra check-ignore build/ a.log` |
| `--stdin` | 从标准输入读取路径（默认换行分隔，配合 `-z` 则 NUL 分隔）。 | `libra check-ignore --stdin < paths.txt` |
| `-z` | 对 `--stdin` 输入和输出使用 NUL（`\0`）分隔，适用于含空白的路径名。 | `libra check-ignore -z --stdin` |
| `-v`, `--verbose` | 对每个匹配路径，额外打印判定规则：`<来源>:<行号>:<模式>\t<路径>`。行号通过扫描来源文件重建。 | `libra check-ignore -v target/` |
| `-n`, `--non-matching` | 同时输出未匹配任何模式的路径（来源/行号/模式字段为空）。需配合 `-v`。 | `libra check-ignore -v -n a.txt b.log` |
| `--no-index` | 不查询 index；即使路径已被跟踪也上报模式匹配。 | `libra check-ignore --no-index tracked.log` |
| `--json` / `--machine` | 结构化输出：`{ results: [{ path, ignored, source?, line?, pattern? }] }`。 | `libra check-ignore --json target/` |

## 退出码

与 Git 对齐：

| 退出码 | 含义 |
|--------|------|
| `0` | 至少一个给定路径被忽略。 |
| `1` | 没有路径被忽略（一种干净信号，而非错误；stderr 无输出）。 |
| `128` | 用法错误（如 `-n` 未配 `-v`、路径与 `--stdin` 同时给出）或不在仓库内。 |

## 输出

- 默认：每行一个被忽略的路径（`-z` 时以 NUL 终止）。
- `-v`：每行 `<来源>:<行号>:<模式>\t<路径>`；`-z` 时四个字段以 NUL 分隔、记录以 NUL 终止。
- `-n`（配合 `-v`）：未匹配路径以空的来源/行号/模式输出。

## 示例

```bash
# target/ 是否被忽略？
libra check-ignore target/

# 显示忽略每个路径的规则
libra check-ignore -v build/ debug.log

# 从其他命令流式读取路径，使用 NUL 分隔
libra ls-files --others -z | libra check-ignore -z --stdin

# 调试：即使已跟踪，该路径是否会匹配某条规则？
libra check-ignore --no-index src/generated.rs

# 面向 agent 的结构化输出
libra check-ignore --json target/ node_modules/
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 检查路径 | `libra check-ignore target/` | `git check-ignore target/` |
| 显示匹配规则 | `libra check-ignore -v target/` | `git check-ignore -v target/` |
| 从 stdin 读取 | `libra check-ignore --stdin` | `git check-ignore --stdin` |
| 忽略 index | `libra check-ignore --no-index p` | `git check-ignore --no-index p` |

未公开（延后）：Git 的 `--exclude`、`--exclude-from`、`--exclude-per-directory` 与完整 pathspec magic。
