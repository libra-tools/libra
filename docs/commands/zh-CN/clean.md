# `libra clean`

从工作树移除未跟踪文件（以及可选的目录）。

## 概要

```
libra clean -n [-d] [-x | -X] [-e <pattern> | --exclude <pattern>]... [--json] [--quiet] [pathspec]...
libra clean -f [-d] [-x | -X] [-e <pattern> | --exclude <pattern>]... [--json] [--quiet] [pathspec]...
```

## 说明

`libra clean` 从工作树移除未跟踪文件。与 Git 不同，Libra 要求显式模式标志：`-n` 用于 dry-run 预览，`-f` 用于实际删除。不带任一标志运行 `libra clean` 是错误。这通过强制用户明确意图来防止意外数据丢失。

默认情况下，只移除文件，并遵守 Git/Libra ignore 来源（忽略文件会被跳过）。`-d` 标志选择同时移除未跟踪目录；`-x` 选择移除原本会受 ignore 规则保护的文件；`-X` 会反转规则，使得*只有*被忽略文件会被移除。每个候选路径都会被规范化并验证位于工作树根目录内，然后才删除，从而防止 symlink-escape 攻击。

可选 pathspec 会将 clean 候选限制为匹配的未跟踪文件或目录前缀。这是 `clean` 当前使用的字面前缀匹配器；`:(exclude)` / `:(glob)` 等共享 pathspec magic 尚未对删除路径启用。

## 选项

| 标志 | 短选项 | 长选项 | 说明 |
|------|-------|------|-------------|
| Dry run | `-n` | `--dry-run` | 显示会被移除的内容，但不删除任何东西。 |
| Force | `-f` | `--force` | 实际移除未跟踪文件。 |
| Directories | `-d` | `--dir` | 同时移除未跟踪目录（否则只移除文件）。 |
| Include ignored | `-x` | | 移除未跟踪文件，**包括**被 ignore 规则匹配的文件。 |
| Only ignored | `-X` | | **仅**移除被 ignore 规则匹配的未跟踪文件。 |
| Exclude | `-e` | `--exclude <pattern>` | 添加额外排除模式；可重复。 |
| Pathspec | | 位置参数 | 将候选限制为匹配文件或目录前缀。`clean` 尚未启用共享 pathspec magic。 |
| JSON | | `--json` | 输出结构化 JSON（见下方）。 |
| Quiet | | `--quiet` | 抑制所有人类可读 stdout。 |

`-x` 和 `-X` 互斥；`-x` 会在普通未跟踪文件之外*包含*被忽略文件，`-X` 则将操作限制为仅被忽略文件。

### 选项细节

**`-n` / `--dry-run`**

预览模式。列出每个*会*被删除的未跟踪路径，但不触碰文件系统：

```bash
$ libra clean -n
Would remove build/output.log
Would remove notes.txt
```

**`-f` / `--force`**

删除模式。移除每个未跟踪路径并报告每次移除：

```bash
$ libra clean -f
Removing build/output.log
Removing notes.txt
```

**`-d` / `--dir`**

显式选择未跟踪目录。没有 `-d` 时，未跟踪目录会保留原位（如果目录本身被跟踪，其内容仍会被考虑）。使用 `-d` 时，会遍历目录树，并在文件移除后移除空目录。

**`-x`**

覆盖配置的 ignore 来源。没有此标志时，被忽略文件（构建产物、缓存等）会被跳过。使用 `-x` 后，它们会像任何其他未跟踪文件一样被移除。

**`-X`**

`-x` 的反向。只移除 ignore 来源通常会保护的文件。适合“清理构建产物但保留手工编辑文件”的场景。

**`-e` / `--exclude <pattern>`**

为本次调用添加额外排除模式（使用 Git ignore 语法）。可多次传递以叠加模式：

```bash
libra clean -f --exclude '*.log' --exclude 'tmp/**'
```

**组合 `-n` 和 `-f`**：两个标志都传递时，dry-run 优先，不会删除文件。

## 常用命令

```bash
# 预览会移除什么
libra clean -n

# 移除所有未跟踪文件（仅文件）
libra clean -f

# 也移除未跟踪目录
libra clean -fd

# 移除未跟踪文件，包括被忽略文件（构建产物、缓存）
libra clean -fx

# 只移除被忽略文件（保留手工编辑文件）
libra clean -fX

# 在配置的 ignore 来源之上叠加一个额外排除模式
libra clean -f --exclude '*.log'

# 以 JSON 格式预览（适合脚本）
libra clean -n --json
```

## 人类可读输出

Dry-run：

```text
Would remove build/output.log
Would remove notes.txt
```

强制移除：

```text
Removing build/output.log
Removing notes.txt
```

`--quiet` 会抑制 stdout。

## 结构化输出（JSON）

```json
{
  "ok": true,
  "command": "clean",
  "data": {
    "dry_run": true,
    "removed": ["build/output.log", "notes.txt"]
  }
}
```

没有可清理内容时，`removed` 为空。

## 设计理由

### 为什么要求显式模式标志？

Git 的 `clean` 在没有 `-f` 时（且没有 `clean.requireForce = false`）会打印要求 `-f` 的错误。这是一个依赖配置的护栏。Libra 让护栏无条件生效：你必须始终传递 `-n` 或 `-f`。没有配置可以削弱这一要求。这消除了一整类“我不小心运行了 clean”的事故。

### 为什么没有交互模式（`-i`）？

Git 的交互式 clean 模式会显示菜单来选择文件。Libra 面向 AI 代理和脚本工作流，在这些场景中交互式提示不可用。dry-run/force 两步工作流在完整自动化支持下实现同样的安全性：运行 `-n --json` 检查，然后运行 `-f` 执行。

### 为什么在最初拒绝后又提供 `-d` / `-x` / `-X`？

最初的 `clean` 设计出于安全考虑有意拒绝目录和 ignore 覆盖标志（`docs/development/commands/clean.md` 将它们列为非目标）。后续用户反馈显示，代理驱动环境中的构建工作流经常需要清理被忽略产物，缺失这些标志迫使用户退回到原始 `rm -rf`，而这严格来说不如 `clean` 安全（没有 symlink-escape 验证，没有 dry-run 预览）。这些标志加入时使用了与基础模式相同的工作树限制和 symlink 检查，在恢复与 `git clean` 对等能力的同时保留安全保证。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| Dry run | `-n` / `--dry-run` | `-n` / `--dry-run` | N/A（无 clean 命令） |
| 强制删除 | `-f` / `--force` | `-f` / `--force` | N/A |
| 移除目录 | `-d` / `--dir` | `-d` | N/A |
| Ignore 覆盖（全部） | `-x` | `-x` | N/A |
| Ignore 覆盖（仅被忽略） | `-X` | `-X` | N/A |
| 排除模式 | `-e <pattern>` / `--exclude <pattern>`（可重复） | `-e <pattern>`（可重复） | N/A |
| 交互模式 | 不支持 | `-i` | N/A |
| Quiet 模式 | `--quiet` | `-q` / `--quiet` | N/A |
| JSON 输出 | `--json` | 不支持 | N/A |
| Pathspec 过滤 | 字面文件/目录前缀 pathspec | `<pathspec>...` | N/A |
| Require force 配置 | 始终要求 | `clean.requireForce`（默认 true） | N/A |

注意：jj 没有 `clean` 命令，因为其工作副本模型会自动跟踪所有文件，未跟踪文件不是 jj 数据模型中的概念。

## 错误处理

| 场景 | StableErrorCode | 退出码 |
|----------|-----------------|------|
| 缺少 `-f` / `-n` | `LBR-CLI-002` | 129 |
| 索引损坏或未跟踪扫描失败 | `LBR-IO-001` | 128 |
| 路径解析到工作树外部 | `LBR-CONFLICT-002` | 128 |
| 文件删除失败 | `LBR-IO-002` | 128 |
