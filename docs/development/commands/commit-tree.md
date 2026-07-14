# commit-tree 命令开发设计

## 命令实现目标

lore.md §1.15（Git-plumbing 形态）：把既有 tree + parents + message 包成
commit **对象**，零 index/worktree/HEAD/ref 副作用；与
`update-index/write-tree/read-tree --index-file`（scratch 索引，
GIT_INDEX_FILE 等价物）合成离线修订组合环路。发布走 `update-ref`
（refs/heads 写入已受 1.13 protect/archive 策略守护）。

## 对比 Git 与兼容性

- 级别：`partial`。`-p` 可重复（重复告警忽略，父必须 load 为 commit）；
  `-m` 段落可重复 + `-F`（`-` = stdin）+ 裸管道 stdin；`-m`+`-F` 可混用但
  按组序拼接（argv 交错不保留——文档化差异）；tree 操作数复用严格 tree-ish
  resolver，支持 commit/tag/ref 与 `^{tree}` 等 typed peel（Libra 超集）。
- 有意差异：空消息拒绝（D-empty-message 全库规则；git plumbing 接受——
  重放含空消息的外部历史暂不可行，已注明）；v1 恒不签名（git 此处尊重
  commit.gpgsign——vault 签名为后续项；注意 libra init 默认 vault.signing=true
  下 porcelain 全签而 plumbing 不签的不对称已文档化）；不支持
  GIT_AUTHOR_DATE/GIT_COMMITTER_DATE（OID 跨运行不可复现——后续项）；
  TTY 无消息源即 usage 错误（agent 安全：绝不挂起等输入）。

## 设计要点

- **消息序列化陷阱（审阅 must-fix）**：git-internal 的
  `Commit::to_data()` 在 committer 行后**不加分隔符**——头/体空行分隔必须是
  message 字段自身的前导 `\n`。与 porcelain 同走 `format_commit_msg(msg,
  None)`（= `"\n{msg}"`）。cat-file 断言钉住。潜在同源坑：
  `internal/ai/history.rs` 的三处 `Commit::new` 未走该包装（预先存在，
  记录于此，独立修复项）。
- 复用面：`read_tree::resolve_tree_ish`（pub(crate) 化）、
  `commit::create_commit_signatures`（pub(crate) 化）、`save_object`。
- `--index-file`：三命令的 Index::load/save 改道 scratch 路径；显式旗标 +
  文件缺失 = 空索引（write-tree 得 canonical empty tree——GIT_INDEX_FILE
  对齐）；对象仍写常规对象库（内容寻址，无副作用）。

## 延后（有因）

MCP-first 有状态 handle（行文的另一形态；MCP 服务器 28 工具全一次性，
首个跨调用状态需 handle 生命周期/TTL 驱逐/授权设计轮；草图：
revision_tree_open/update/write/seal 覆于服务器持有的 Index map）；
`mktree`（被 --index-file + write-tree 严格支配）；vault 签名 `-S`；
日期覆盖 env；`LIBRA_INDEX_FILE` 全局 env（爆炸半径过大，逐命令旗标即
范围化等价物）；`run_libra_vcs` MCP allowlist 扩容（安全面变更，
--index-file 落地后的候选项）。

## 实现历史

- 2026-07-03（lore.md Phase 1 / 1.15，Phase 1 收官）：初版 + 3 e2e。

## 维护要求

- 新增消息路径必须经 `format_commit_msg`（前导 \n 分隔符不变量）；
  任何新的 ref 写入面须复用 update-ref/branch-reset 的策略守护。
