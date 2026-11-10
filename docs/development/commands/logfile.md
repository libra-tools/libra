# logfile 命令开发设计

## 命令实现目标

`libra logfile info` 报告本进程解析出的 tracing 日志文件配置（路径、滚动策略、
过滤器、当前大小），对齐 Lore `logfile` 命令（`lore.md` §0.7）。纯环境变量检查，
不需要仓库。配套的核心能力是**滚动日志**：`LIBRA_LOG_FILE` 现支持
`LIBRA_LOG_ROTATION`（`never`/`minutely`/`hourly`/`daily`）经 `tracing-appender`
按时间滚动（每个文件不再无界增长）。刻意**不**启用 `max_log_files` 剪除：其按
文件名前缀删除，会误删日志目录下无关的 `<file>.*` 文件（有数据丢失风险）；总磁盘
占用交由外部 retention（logrotate 或专用日志目录）控制——`logfile info` 的
`size_bytes`/`file_count` 便于监控。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Git 无对应；Libra 诊断扩展。
- 已支持：`logfile info`（human + `--json`/`--machine`：`{ enabled, file, rotation, filter, size_bytes, file_count }`）。`size_bytes`/`file_count` 对 rolling 会汇总目录下所有 `<name>.*` 滚动文件（`never` 则为单文件），故不会因为活动文件带日期后缀而误报「未创建」。
- 退出码：0。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::Logfile` → `command::logfile::execute_safe`；
  列入 `CommandPreflight::none()`（无需仓库/hash-kind preflight）。
- 配置收口：`src/utils/log_config.rs::resolve_log_config` 统一解析
  `LIBRA_LOG`/`RUST_LOG`/`LIBRA_LOG_FILE`/`LIBRA_LOG_ROTATION`，`main.rs::init_tracing`
  与 `logfile info` 共用同一解析，保证二者对配置的理解一致。`LogRotation` 枚举
  （Never/Minutely/Hourly/Daily）+ 大小写不敏感 `parse`。
- 未设置日志文件时，fmt layer 显式使用 stderr writer，stdout 保留给命令结果，
  包括 `--json` / `--machine` 响应。agent push 的 scope discovery 失败回归同时
  验证 warning 在 stderr 可见、整个 stdout 可解析为单个 JSON、pending receipt
  及后续重试/复用语义；不通过关闭日志或过滤 stdout 绕过验证。
- 滚动实现（`main.rs`）：`rotation=never` 保持旧行为（`OpenOptions` 单文件追加、
  精确路径，向后兼容）；`rotation!=never` 用 `tracing_appender::rolling::RollingFileAppender`
  的 **fallible builder**（非会 panic 的 `::new`）构建，写前 `create_dir_all` 日志
  目录；阻塞写（非 `non_blocking`，故短生命周期 CLI 退出不丢日志、无需 WorkerGuard）；
  init 失败只禁用日志、绝不 crash CLI。
- 源码分层：`src/command/logfile.rs`：`LogfileArgs`（子命令 `LogfileCommand::Info`）、
  `LogfileInfo`（serde）、`execute_safe`/`info`。
- 底层操作对象：只读进程环境变量 + 对 `LIBRA_LOG_FILE` 做一次 `metadata` 取大小。无对象库/refs/网络。

## 实现历史

- 2026-07-02（`lore.md` Phase 0 / 0.7）：滚动日志 + `logfile info`；新增
  `tracing-appender` 依赖与 `utils::log_config` 模块。

## 当前状态

- 公开状态：已公开（`Commands::Logfile`）。
- 依赖：新增 `tracing-appender = "0.2.3"`。
- 测试：`tests/command/logfile_test.rs`（默认 disabled、`LIBRA_LOG_FILE`+rotation
  报告、`--json` 信封、仓库外可运行）+ `log_config.rs` 单测（rotation parse/round-trip）。
- 用户文档：`docs/commands/logfile.md`。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 滚动维度 | 按大小滚动、保留份数上限（`max_log_files`） | 延后；当前仅按时间滚动。`max_log_files` 因按前缀删除有误删风险，暂不启用；总量交外部 retention。 |
| 子命令 | Lore `logfile` 的其它子命令 | 仅 `info`；有需求再扩展。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 任何对日志环境变量语义的改动必须同时改 `utils::log_config`，使 `init_tracing`
  与 `logfile info` 保持一致，不得各自解析。
