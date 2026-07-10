# `libra check-attr`

报告一个或多个路径的 Git/Libra attributes——Libra 版的 `git check-attr`。

> 有意差异（见
> [`docs/development/commands/_compatibility.md`](../../development/commands/_compatibility.md)
> 决策 **D5**）：Libra **不**实现 Git `.gitattributes` 的 smudge/clean filter 桥接。
> `check-attr` 是对 attributes 的只读查询，而非 filter 驱动。

Attributes 来源按从低到高的优先级应用：`core.attributesFile`、从根到子目录的 `.gitattributes`、同目录 `.libra_attributes`、最后是 `.git/info/attributes`。Libra 扩展文件会覆盖同目录 `.gitattributes` 的匹配规则，而 `.git/info/attributes` 保持 Git 的最高优先级工作树本地层级。

## 用法

```
libra check-attr [-z] <attr>... [--] <pathname>...
libra check-attr [-z] -a | --all [--] <pathname>...
libra check-attr [-z] (<attr>... | --all) --stdin
```

## 说明

对每个 `(路径, 属性)` 组合，`check-attr` 打印属性值。取值之一：

- `lfs` —— 当 `filter` 属性命中（某个 attributes 来源有匹配该路径的 `filter=lfs` 模式）。
- `set` / `unset` —— 裸属性或 `-attr` 规则。
- `unspecified` —— 该属性未在路径上设置。

`--all` 仅报告路径上**已设置**的属性（例如 `filter: lfs` 或 `diff: <driver>`）。

命令成功时总是退出 `0`（即使所有属性都是 `unspecified`）；用法或仓库错误退出 `128`。

## 参数形式

属性名与路径都是位置参数，按以下规则区分：

- `--all`：所有位置参数都是路径。
- 显式 `--`：之前是属性，之后是路径。
- `--stdin`：位置参数是属性名，路径从标准输入读取。
- 否则：**第一个**位置参数是属性，其余是路径（多个属性请用 `--`）。

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `<attr>...` | 要查询的属性名。 | `libra check-attr filter a.bin` |
| `<pathname>...` | 要检查的路径（在 `--` 之后，或紧跟属性）。 | `libra check-attr filter -- a.bin b.c` |
| `-a`, `--all` | 报告每个路径上已设置的全部属性。 | `libra check-attr --all data.bin` |
| `--stdin` | 从标准输入读取路径。 | `libra check-attr filter --stdin` |
| `-z` | 对 `--stdin` 输入和输出使用 NUL 分隔。 | `libra check-attr -z filter --stdin` |
| `--json` / `--machine` | 结构化输出：`{ results: [{ path, attr, value }] }`。 | `libra check-attr --json filter a.bin` |

## 输出

- 默认：每行 `<路径>: <属性>: <值>`。
- `-z`：三个字段以 NUL 分隔，每条记录以 NUL 终止。

## 示例

```bash
# a.bin 是否走 LFS filter？
libra check-attr filter a.bin
# -> a.bin: filter: lfs   （若某个 attributes 来源跟踪 *.bin）

# 查询多个属性（用 -- 分隔）
libra check-attr filter text -- a.bin notes.txt

# 路径上已设置的全部属性
libra check-attr --all a.bin

# 从其他命令流式读取路径
libra ls-files -z | libra check-attr -z filter --stdin

# 面向 agent 的结构化输出
libra check-attr --json filter a.bin
```

## 与 Git 对比

| 任务 | Libra | Git |
|------|-------|-----|
| 查询属性 | `libra check-attr filter a.bin` | `git check-attr filter a.bin` |
| 全部属性 | `libra check-attr --all a.bin` | `git check-attr --all a.bin` |
| 从 stdin | `libra check-attr filter --stdin` | `git check-attr filter --stdin` |

Libra 读取 `filter`、`diff`、`export-ignore` 等属性，但不运行 smudge/clean filter。Git 的 `--cached`、`--source` 与 attributes 宏展开尚未公开。
