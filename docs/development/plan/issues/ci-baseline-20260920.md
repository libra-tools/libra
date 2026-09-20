# CI 基线稳定性汇总修复（2026-09-20）

## 目标与范围

在最新 `upstream/main` 上建立 `fix/ci-baseline-20260920`，汇总替代
#505、#506、#507。保留三个原提交的作者、作者日期、DCO 和独立提交边界。
不修改 main，不关闭 #504、#503、#456，不删除远端分支，不合并或发布。

## 已实施

- #505：修复 history/traces、旧 source 层和 worktree writer 的 Rustdoc 链接。
- #506：CI 使用 umask 022 和私有 scratch 目录，配置 repair fixture 显式设置安全权限。
- #507：archive pathspec 单元测试初始化临时 Libra 仓库并持有 ChangeDirGuard。
- 新增编译阻塞：AddArgs 新增 sparse 字段后，am、rerere 和大量测试初始化未同步。
  已补齐 243 处初始化为 `sparse: false`，保持默认拒绝稀疏范围外暂存；不新增公开参数。

## 实际验收状态

用户在 2026-09-20 调整执行顺序：本机性能有限，先创建汇总 PR，将剩余门禁交给线上 CI。
本地重型测试已停止，不以未执行的本地结果代替 CI 验收。

- 默认 `cargo check` 通过（exit 0）；首次离线缓存缺少 git-internal 0.10.2，
  后续使用现有代理下载恢复。未在修改前动态复现 E0063。
- `cargo +nightly fmt --all --check` 通过。
- `umask 0002` 下 `cargo test --test compat_global_config_schema_future global_schema_repair`：14 passed。
- 全 targets/features Clippy 已执行，暴露 5 处 nonminimal_bool（cli、worktree-fuse、merge、
  history、serial_registry），均已做等价简化；最终通过状态以线上 CI 为准。
- 完整静态扫描核对新增 243 处 `sparse: false`；不改变默认稀疏暂存限制。
- CI 固定版 nextest 0.9.143 的官方构建已下载至临时目录，SHA-256 和版本检查通过。
  本地未执行其全量测试，也未完成 cargo test --all、Rustdoc、doctest 和其余定向矩阵。
- 原 CI job 106045482918 的 22 项失败已核对：archive 2、文件模式 12、配置修复 7、
  数据库迁移 1；必须包含 db_migration_test::global_schema_repair_writes_configuration_ledger_only。
- 汇总 PR：[#508](https://github.com/libra-tools/libra/pull/508)。#505、#506、#507 保持开启，最终提交全绿后才关闭。

## 线上验收与关闭门禁

1. 默认与全部 targets/features 编译通过，并确认完整差异仅增加 sparse 默认值。
2. 从 #505 失败日志核对全部 22 项并逐项重跑；不得以 executable_bit 过滤代替
   checkout 的 test_checkout_restores_entry_mode，以及 restore 的
   test_restore_applies_entry_mode_matrix、test_restore_honors_process_umask_for_entry_modes。
3. 配置 repair fixture 在 umask 0002 下验证；模式测试在 umask 022 下验证；
   archive 单元与集成测试、am、rerere、add --sparse 行为回归通过。
   设置 LIBRA_TEST_SCRATCH_DIR 为任务私有临时目录，避免 fixture 回退到真实 HOME。
4. nightly fmt、全 targets/features Clippy -D warnings、Rustdoc 断链、
   工作流固定版本 nextest 全量和 doctest 全部通过；本地 cargo test --all 未完成如实记录。
5. 使用 Libra 创建带 DCO 和签名的修复提交，推送 fork，创建面向 libra-tools/libra:main
   的汇总 PR，记录替代关系、共同编译根因、精确测试结果与最终提交 SHA。
6. 跟进最终 SHA 的全部 CI，包括网络、macOS、OpenCode、OTLP、keyring、upgrade、CodeQL。
   全绿且核对三个旧 PR 改动完整保留后，分别评论替代链接并关闭 #505、#506、#507。
   任一代码或基础设施阻塞未解决时，保留旧 PR 开启。
