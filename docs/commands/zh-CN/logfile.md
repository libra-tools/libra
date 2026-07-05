# `libra logfile`

检查 Libra 的 tracing 日志文件配置。镜像 Lore 的 `logfile` 命令。这是用于文件日志 sink 的诊断辅助命令，该 sink 由 `LIBRA_LOG_FILE`、`LIBRA_LOG_ROTATION` 和 `LIBRA_LOG` / `RUST_LOG` 环境变量控制。

## 概要

```
libra logfile info
```

## 说明

`logfile info` 会报告当前进程如何从环境中解析并配置文件日志：

- **enabled** — tracing 是否开启（是否解析到了 filter directive）。
- **file** — `LIBRA_LOG_FILE` 路径；未设置时为 stderr。
- **rotation** — rolling 策略（`never` / `minutely` / `hourly` / `daily`）。
- **filter** — 已解析的 `LIBRA_LOG` / `RUST_LOG` directive（或仅设置 `LIBRA_LOG_FILE` 时使用的 `libra=debug` fallback）。
- **size** — 磁盘上日志文件的总大小和数量（`never` 下是单个文件；否则是所有 rolled file）。

它不需要仓库。

### 日志文件环境变量

| 变量 | 含义 |
|------|------|
| `LIBRA_LOG_FILE` | append/rolled tracing sink 的路径。未设置时，日志写到 stderr（仅当设置 filter 时）。 |
| `LIBRA_LOG_ROTATION` | `never`（默认）、`minutely`、`hourly` 或 `daily`。rolling 时，每个活动文件写作 `<file>.<date-suffix>`，避免单个文件无限增长。Rotation 只按时间 *拆分* 日志 — 它 **不会** 删除旧文件，因此总磁盘占用仍需外部保留策略（例如 `logrotate`，或把 `LIBRA_LOG_FILE` 指向专用目录）。 |
| `LIBRA_LOG` / `RUST_LOG` | `tracing-subscriber` env filter。仅设置 `LIBRA_LOG_FILE` 时回退为 `libra=debug`。 |

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `info` | 显示已解析的日志配置。 | `libra logfile info` |
| `--json` / `--machine` | 结构化 `{ enabled, file, rotation, filter, size_bytes, file_count }`。 | `libra --json logfile info` |

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | 配置已报告。 |

## 示例

```bash
# 显示当前环境下日志会写到哪里。
libra logfile info

# 按天滚动日志并检查解析后的配置。
LIBRA_LOG_FILE=/var/log/libra/libra.log LIBRA_LOG_ROTATION=daily libra logfile info

# 面向工具的结构化输出。
libra --json logfile info
```

## 与 Git 对比

Git 没有等价命令；这是 Libra 的诊断扩展（类似 Lore 的 `logfile`），在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中分类为 `intentionally-different`。
