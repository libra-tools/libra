# upgrade 命令开发设计

## 命令实现目标

`libra upgrade` 是不依赖仓库的 Libra 扩展：检查 Ed25519 签名的 stable
release manifest，并在用户确认或传入 `--yes` 后，以可回滚的安装事务替换官方
安装的当前二进制。它不读取或修改仓库状态；Git 没有对应命令。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。
- 已支持：默认交互检查和安装、`--check`（只报告）、`-y`/`--yes`（非交互安装），
  以及全局 `--json`/`--machine` 输出。
- `--check` 与 `--yes` 互斥。机器输出和 quiet 模式绝不提示；发现可用版本时，
  调用方必须选择 `--check` 或 `--yes`。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::Upgrade` →
  `command::upgrade::execute_safe`，无需仓库 preflight。
- `command::upgrade::UpgradeArgs` 仅承载确认策略；签名 manifest 的获取、平台
  选择、反回滚状态、安装标记和安装事务由 `internal::upgrade/` 统一负责。
- 每次手工检查先验证 manifest 并持久化其反回滚 floors。确认安装前会再次获取和
  验证 manifest；控制面变化、暂停或撤销都会拒绝继续使用旧计划。
- 安装事务受 `internal::upgrade::lock::UpgradeLock` 保护，下载内容同时验证 size
  和 sha256，并运行新二进制的 probe；事务或 probe 失败时恢复旧二进制。
- 仅安装脚本写入了官方 install marker 的二进制可自升级。源代码构建、改名副本和
  不受支持的平台会给出可操作的拒绝结果。

## 当前状态

- 公开状态：已公开（`Commands::Upgrade`）。
- 用户文档：[docs/commands/upgrade.md](../../commands/upgrade.md)。
- 测试：`tests/command/upgrade_cmd_test.rs` 覆盖 CLI/官方安装标记路径；
  `tests/upgrade_auto_test.rs` 和 `tests/upgrade_publish_contract_test.rs` 在
  `--features test-upgrade` 下覆盖签名、状态转换、安装/回滚与发布契约。

## 维护要求

- 改动命令参数或用户可见状态时，同时更新用户文档、此设计文档、
  `COMPATIBILITY.md` 和 `docs/development/commands/README.md`。
- 改动 manifest、反回滚 floor、安装 marker 或事务语义时，必须同步维护
  `internal::upgrade/` 的跨层测试；不能将验证或持久化错误静默降级。
