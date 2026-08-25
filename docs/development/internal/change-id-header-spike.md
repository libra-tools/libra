# OL-00 Change ID 持久化路径 Spike 结论

日期：2026-08-25
任务：`OL-00`（`plan-20260822.md`）

## 范围

本 spike 只比较两种持久化路径：

1. 将 `change-id` 写入 Git commit header；
2. 将 Libra 的 Change ID 保存在 sidecar/投影中，不修改 Git commit object。

不实现 `ChangeId` 类型、change 表或 rewrite builder；这些由 `CH-01`、`CH-02` 和 `CH-03` 承接。

## 决策

**Libra 新写入采用 sidecar-only。** Libra 不向新建或 rewrite 的 Git commit 写入 `change-id` header。已有 commit 中的合法 `change-id` header 可以作为导入兼容信息读取，并在导入记录中标记 `origin=header`；这不是 Libra 的写入协议。

理由：真实 Git 可以保留并传递未知 commit header，但 header 属于 commit object 内容，会改变 Commit OID；本次 spike 没有证明所有 rewrite、导入和第三方写入路径都能稳定维护该 header。sidecar/投影可以保存稳定的 Libra Change ID，同时保持 Git commit object、OID 和 Git 原生对象闭包不变。

## 验证证据

`tests/commit_change_id_header_spike.rs` 使用真实 Git 命令和临时仓库验证：

- header 路径：构造含 `change-id` header 的 commit，`git cat-file` 可读，`git fsck --full --no-reflogs` 通过；经本地 bare remote push 和 clone 后 header 仍存在，clone 的 fsck 也通过。
- sidecar-only 路径：正常 Git commit 的 OID 不因 sidecar 变化；commit、tree、blob 均可由 `git rev-list --objects --all` 枚举，`git cat-file -e` 和 fsck 均通过。

按 `.github/workflows/base.yml` 的后端相关门执行：

```text
cargo +nightly fmt --all --check
RUSTUP_TOOLCHAIN=stable LIBRA_SKIP_WEB_BUILD=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 RUST_MIN_STACK=16777216 cargo test --test commit_change_id_header_spike
```

结果：格式检查通过；stable 1.98.0 focused test 通过，`2 passed; 0 failed`。本任务没有修改 `src/`、`web/src/`、`worker/src/` 或其他生产运行时表面。

## 承接约束

- `CH-01`：随机 Change ID 和 legacy synthetic Change ID 的 canonical 持久化使用 sidecar/投影；已有 header 仅作为可选导入来源。
- `CH-03`：`ChangeRevisionBuilder` 的 commit 写入路径不得注入 `change-id` header；commit object、Change ID 投影和 predecessor 记录必须在既定事务/幂等协议中协调。
- Gate-5：验证 Change ID 格式、synthetic 生成和 sidecar 投影；不把“Libra 写入 header”作为一致性前提。

## 限制与后续风险

本 spike 不验证第三方 Git 工具对 header 的语义理解，也不验证每一种 rewrite 工具是否保留未知 header。因此，header 兼容证据不能升级为 header 写入承诺。后续实现必须覆盖 sidecar 丢失、commit OID 与 sidecar 不一致、重复导入和碰撞的 fail-closed 行为。
