# `libra revision`

Revision ordinal index（Libra 扩展，lore.md §1.16，移植 Lore 的 `revision find number`）。Git 没有等价表面。

## 概要

```
libra revision find --number <N> [--ref <branch>]
libra revision number <commitish> [--ref <branch>]
libra revision index [--ref <branch>] [--rebuild]
libra revision import --manifest <file>
```

## 说明

每个分支的 **first-parent chain** 获得单调的 1-based 编号（1 = root，N = tip），存储在可重建的 SQLite side table 中。编号是 tip 的纯函数（跨重建和机器确定性一致）。只通过合并进来的 side branch 可达的提交 **没有** ordinal — 反向查询会明确说明，而不是发明编号。

每次读取都会在与查询同一个事务中重新验证新鲜度：fast-forward 追加（已有 ordinal 永不改变）；history rewrite（rebase/amend/reset）和 `refs/replace` 变化会触发完整确定性重建。陈旧索引永不回答。长分支上的第一次查询会遍历整条链一次（O(chain) object loads，在分层存储下可能访问远端）；后续查询为索引命中。

| 子命令 | 用途 |
|---|---|
| `find --number <N>` | 打印 revision #N 的 OID（Lore 的 `revision find number`）。越界 → 退出 1 并说明链长度；`N < 1` → 129。 |
| `number <commitish>` | 反向查询：某个提交在 ref 链上的 ordinal。不在链上 → 退出 1 并给出覆盖范围说明。 |
| `index [--rebuild]` | 新鲜度报告（tip、count、built-at）；`--rebuild` 强制确定性重建并修剪已删除分支的索引行。 |

`--ref <branch>` 指向任意本地分支；默认是当前分支（detached HEAD → 报错并建议 `--ref`）。`--json` 输出结构化结果。`find --metadata`（按 1.10 revision metadata 搜索）是已记录后续项 — ordinal index 提供这种扫描会遍历的确定性迭代顺序。

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | 成功。 |
| `1` | 未命中（ordinal 越界；commit 不在链上）。 |
| `128` | 致命错误（不在仓库中；detached HEAD 且未给 `--ref`）。 |
| `129` | 用法错误（`--number` < 1）。 |

## 示例

```bash
libra revision find -n 1                 # root revision
libra revision number HEAD               # mainline 有多长？
libra revision find -n 42 --ref main     # main 的第 42 个 revision
libra revision index --rebuild           # 确定性重建 + prune
libra --json revision number HEAD        # 结构化输出
```

## 与 Git 对比

最接近的 Git 对应物：`git rev-list --first-parent --count <oid>`（反向）和 `<tip>~<k>` 后缀算术（正向）。在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中分类为 `intentionally-different`。

## 离线历史导入

`revision import --manifest <file>` 将当前仓库、冻结 HEAD 下的不可变 Git 提交登记为持久 change 事实；普通 clone 和 `revision index` 不执行这一步。之后调用 `memory rebuild` 派生 Episode。

版本 1 JSON 只接受 `schema_version`、`repo_id`、`object_format`、`base_commit`、`visibility_policy`、`cutoff_ms`、`commit_oids`、`commit_inventory_sha256`、`objects`、`object_inventory_sha256`、`shallow_cuts`、`sources`。对象条目为 `oid/kind/size`；来源为 `commit_oid/available_at_ms`；浅边界为 `child/parent`。各层未知字段均拒绝。提交摘要对排序后的 `OID\n` 行计算 SHA-256；对象摘要对排序后的 `OID TYPE SIZE\n` 行计算 SHA-256，均为 UTF-8，OID 必须是完整小写十六进制。

校验真实 repo identity、SHA-1/SHA-256 格式、当前 HEAD、对象哈希、精确有限祖先集合与 tree/blob 闭包。拒绝 replace refs；声明的浅边界父对象必须确实在本地缺失，未知缺失父边、清单中的未来或不可达对象都失败。仅用现存本地对象，不经 alternates、云回退或联网补全。上限为清单 2 MiB、5,000 提交、20,000 对象、对象正文合计 64 MiB、单对象 16 MiB（单提交 1 MiB）、每批 128 来源。

`strict_time` 必须给截止时间，base 或祖先晚于截止均拒绝；`base_snapshot` 明确表示回顾性快照，不证明原历史任务合格。给出截止时间时，来源可见时间晚于截止始终拒绝。公开/合入时间需外部独立审计，Git 时间戳不能替代；原样保留清单、时间证据和导入回执。

复用已登记且可见的 change；纯 Git 提交使用既有 object-format 隔离的 synthetic Change ID。不凭相似补丁猜身份，也不接受未经验证的外部 sidecar。通过既有 revision writer 在同一事务写入整批事实并使旧 Memory generation 失效；失败保留原事实与投影，操作日志可以记录本次失败。重复同一清单复用身份和 revision，不使未变化的投影失效。已有事实超出新 base 可见集合时拒绝。给出 cutoff 时，sources 必须列全该视图允许的既有 revision，遗漏即拒绝，避免较窄时间门继承旧的宽松视图。请为每个 base/cutoff/policy 使用新仓库。

当前 operation 记录清单 SHA-256 和现在的导入时间。导入成功不等于历史修复/测试成功：导入 Episode 的 outcome 为 `unknown`，不伪造历史成功 operation 或 agent_session。新批次成功后先 `memory rebuild` 再读取。路径召回、大窗口选择、撤销计数及 consumer 的既有缺口仍独立处理。

`--json` 回执含清单摘要、base、导入/复用数量、有限历史标记及 commit→change 映射。拒绝或事务失败退出 128，错误码 `LBR-IO-002`。被拒绝的清单字段名中的终端控制序列会从诊断中移除。
