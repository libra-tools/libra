# update-ref 命令开发设计

## 命令实现目标

`libra update-ref` 安全地更新/创建/删除 `refs/heads/<branch>`，支持可选的 compare-and-swap（CAS）。ref 读取 + 写/删 + reflog 写入在**同一 SQLite 事务**内完成，CAS 失败原子回滚。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`update-ref refs/heads/<branch> <new> [<old>]`（`<old>` = 全零 → 仅创建；= 完整 oid → CAS；省略 → 无条件创建/覆盖）、`-d` 删除（含可选 `<old>` CAS）、`-m <reason>` reflog 原因、`--json`/`--machine`。
- **有意 v1 范围收窄**：仅 `refs/heads/*`（Libra `reference` 表能直接建模的分支 tip）。`HEAD`、`refs/tags/*`、`refs/remotes/*`、任意命名空间均拒绝（128）并给出指引（HEAD→`symbolic-ref`/`switch`，tag→`tag`）。
- **plan-vs-Git 调和**：grit-gap 早期验收写「省略 `<old>` 等价于要求 ref 存在、否则 exit 1」，与 Git 实际行为**不符**（Git 省略 `<old>` 时无条件创建/更新，不要求存在）。本实现按 **Git 正确语义**：省略 `<old>` = 无条件创建或覆盖。验收文案已据此调和。
- **`<newvalue>` 的 revision 解析（plan-20260729 CT1-02）**：值操作数经 `util::resolve_object_spec_typed` 解析（`src/command/update_ref.rs` 的 `resolve_new_value`），支持分支/tag/`HEAD`/`~`/`^`/缩写 oid/`<rev>^{commit}`。**不做隐式 peel**——解析结果本身必须是 commit（用 `ClientStorage::get_object_type` 判定，同一次查询顺带证明对象存在），因此 lightweight tag 通过、bare annotated tag 被拒并在错误信息里点名解析到的类型，与 Git 一致。错误分层：`ref:` 前缀与全零 id 是**语法层**拒绝，保持 `LBR-CLI-002`；无法解析、解析结果非 commit、对象不存在为 `LBR-CLI-003`；解析器抛出的 `CommitBaseError::ReadFailure`/`CorruptReference` 分别保持 `LBR-IO-001`/`LBR-REPO-002`，**不降级为输入错误**（否则运维会去查命令行而不是查对象库）。全部 fatal 仍统一 exit 128（`execute_safe` 的既有契约，见 `docs/error-codes.md` 的 update-ref 例外段）。`<oldvalue>` 的 revision 化见下一条（CT1-03）。回归：`tests/command/update_ref_test.rs` 的 12 个 `update_ref_revision_*` 与 3 个 `update_ref_output_shape_*`。
- **策略不变量（CT1-03 断言）**：protect/archive 元数据读取仍在**同一事务内**、位于 CAS 判定之前，update/create/delete 三种拼写各自 fail-closed（`LBR-POLICY-001`）；元数据读取失败（如 `metadata_kv` 不可读）经 `UpdateRefTxError::Storage` → `RepoStateInvalid`（`LBR-REPO-003`）拒绝写入，绝不当作「没有策略」。拒绝后 ref 与 reflog 均无变化。回归：`tests/command/update_ref_test.rs` 的 9 个 `update_ref_policy_*` 与 4 个 `update_ref_oldvalue_*`。
- 未公开（延后）：非 `refs/heads/*` 命名空间、`HEAD`、`--stdin` 批量、`--create-reflog`、`--no-deref`。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::UpdateRef` → `command::update_ref::execute_safe`。
- 源码分层：`src/command/update_ref.rs`：`UpdateRefArgs`（`delete`/`message`/`ref_name`/`value`/`old_value`）、`execute`/`execute_safe`、`UpdateRefOutput`（`--json`：`ref`/`old`/`new`/`deleted`）、`UpdateRefTxError`（事务内错误，映射为 128）、`OldValue`（`MustNotExist`/`Exact`）、`parse_heads_ref`/`resolve_new_value`/`resolve_old_value`/`resolver_error`/`usage_fatal`/`invalid_target_fatal`/`write_reflog`。
- 位置参数消歧：`-d <ref> [<old>]`（位置 2 = old）vs `<ref> <new> [<old>]`（位置 2 = new、位置 3 = old）。`-d` 时位置 3 必须为空。
- 校验：`parse_heads_ref`（仅 `refs/heads/`，HEAD/其它命名空间拒绝）+ `util::is_valid_refname`（对齐 `git check-ref-format`）；`<new>` 经 `resolve_new_value`（CT1-02 起）：先做**语法层**拒绝（`ref:` 符号值、全零 new），再交 `util::resolve_object_spec_typed` 解析任意 revision 表达式，最后用 `ClientStorage::get_object_type` 断言解析结果**本身就是 commit**（不隐式 peel），该次查询同时证明对象存在，故不再单独调 `objects_storage().get()`；`<old>`（CT1-03 起）经 `resolve_old_value` 走**同一**解析入口，但**不做 commit 类型校验**——它断言的是 ref 的当前状态，解析结果按原样参与精确比较，故 annotated tag 作 `<old>` 得到普通 CAS 不匹配而非「非 commit」拒绝（与 Git 一致）；全零 → `MustNotExist` 的语义不变；两个操作数共用 `resolver_error` 做错误分层。原定长 hex 校验函数 `validate_oid` 随之删除（两个操作数都不再走它）；`<old>` 经 `parse_old_value`（全零→`MustNotExist`，否则 `Exact`）。
- 事务（`get_db_conn_instance().await.transaction(move |txn| Box::pin(async move {...}))`）：
  1. `Branch::find_branch_result_with_conn(txn, branch, None)` 读当前 tip（`Option<ObjectHash>`→hex）。
  2. CAS 判定：`MustNotExist` 但已存在 → `MustNotExist` 错；`Exact(want)` 但当前 != want → `CasMismatch`。
  3. delete：当前为空 → `DoesNotExist`；否则 `delete_branch_result_with_conn` + reflog(old→zero)。
  4. update：`update_branch_with_conn(txn, branch, new, None)` + reflog(old_or_zero→new)。
  - `TransactionError` → `CliError` 128（`RepoStateInvalid`）。
- reflog：新增 `ReflogAction::UpdateRef { message }`（+ `ReflogActionKind::UpdateRef` → action 列 = `"update-ref"`；`ReflogContext` Display = message）。**`<old>` CAS 操作数绝不写入 reflog**——只记录真实前后 oid（`write_reflog` 仅取 current/new）。
- 错误码：复用既有 `StableErrorCode`。`CliInvalidArguments` 用于用法/refname/**`<new>` 的语法层拒绝**/CAS-arg；`CliInvalidTarget` 用于 `<new>` 无法解析、解析结果非 commit、对象不存在；`IoReadFailed`/`RepoCorrupt` 分别承接解析器的 `CommitBaseError::ReadFailure`/`CorruptReference` 与 `get_object_type` 的非 `ObjectNotFound` 失败（**不降级为输入错误**）；`RepoStateInvalid` 用于事务失败/CAS 不匹配。**未新增** `StableErrorCode` 变体（`docs/error-codes.md` 只补了 `LBR-CLI-003` 的适用场景描述）。全部 128，对齐 Git fatal——该 128 覆盖与总表 129 的差异由 `docs/error-codes.md` 的 update-ref 例外段登记（plan-20260729 FIX-02）。
- 底层操作对象：SQLite `reference` + `reflog` 表（事务）。无对象库/网络/工作树写入。

## 实现历史

- 2026-06-30（GGT-06 part 2，`grit-gap.md` 阶段 2）：与 `update-index` 同属 GGT-06；本命令第二个发布；新增 `ReflogAction::UpdateRef`。

## 当前状态

- 公开状态：已公开（`Commands::UpdateRef`）。
- 测试：`tests/command/update_ref_test.rs`（创建/更新/CAS 成功+失败/全零仅创建/删除/删除 CAS 不匹配/删除不存在/拒绝 HEAD/拒绝 refs/tags/非法 oid/拒绝符号值/`--json`/非仓库）。
- 用户文档：`docs/commands/update-ref.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 命名空间 | 非 `refs/heads/*`（tags/remotes/HEAD/任意） | 拒绝 + 指引；Libra `reference` 表不直接建模。 |
| 兼容差异项 | `--stdin` 批量、`--create-reflog`、`--no-deref` | 延后。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- ref 读/写/删 + reflog 必须保持在单事务内（原子性）；新增命名空间支持需先评估 `reference` 表建模。
