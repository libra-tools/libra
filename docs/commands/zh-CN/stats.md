# libra stats

显示当前工作目录文件统计的历史设计。

> 状态：未发布。`libra stats` 未注册到当前版本的公开 CLI。运行它会返回标准 unknown-command 错误（`LBR-CLI-001`）。下面的接口描述的是保留的设计材料，不是用户可见命令契约。

未发布设计是 Libra-only 扩展（没有 `git` 等价物）。它是只读命令，会递归扫描当前工作目录，统计普通文件数量，并按文件扩展名分组。会跳过 `.libra/` 元数据目录和 `target/` 构建目录。

## 概要

```bash
libra stats
```

## 说明

- 递归遍历当前工作目录。
- 统计每个普通文件并按扩展名分桶。没有扩展名的文件报告在 `no_extension` 下。
- 跳过 `.libra/` 和 `target/` 目录。
- 默认打印人类可读摘要，或通过全局 `--json` / `--machine` 标志输出结构化信封。

该命令不读取索引或任何提交；它精确报告磁盘上的当前工作树。

## 选项

如果此命令在未来版本发布，它不应接受命令专用选项，并应遵守全局输出标志：

| 标志 | 说明 |
|------|------|
| `--json[=<FORMAT>]` | 以 JSON 输出结果（`pretty`、`compact` 或 `ndjson`）。 |
| `--machine` | 严格机器模式（`--json=ndjson --no-pager --color=never --quiet`）。 |
| `--quiet` | 抑制 stdout。 |

## 输出

人类可读：

```text
File statistics:
total: 42
no_extension: 3
md: 7
rs: 32
```

JSON（`--json`）：

```json
{
  "total": 42,
  "extensions": {
    "md": 7,
    "no_extension": 3,
    "rs": 32
  }
}
```

## 示例

```bash
# 按扩展名统计工作树文件
libra stats

# 面向 agents/tooling 的结构化 JSON 输出
libra stats --json
```
