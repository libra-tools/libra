# dirty 命令开发设计

## 命令实现目标

dirty-set 缓存（`lore.md` §1.1）：`working_dirty`/`working_dirty_meta` 两张
SQLite 表构成的**咨询性**快照，加速 agent/工具的 status 路径。默认 `status`
永不读写缓存，任何正确性决策不得依赖它。表面：`libra dirty <paths>`（人工
标记，只会导致过报——安全方向）、`status --scan`（唯一权威重建）、
`status --cached`（消费）、`status --check-dirty`（只复核缓存集）。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Git 无此表面（最近机制为 index stat
  cache 与 fsmonitor，均为内部件）。`status --cached` 与 Git 的 `--cached`
  （=索引）无关——命名沿自 `gap/lore.md` §3.0.1「样例（1.1 dirty-set）」（2026-08-27 复核现 `:176`），文档显著标注。
- 三个 status flag 互斥，且与 `--porcelain`/`--short`/`--ignored` 冲突
  （可能过期的 porcelain 流不得喂 Git-porcelain 解析器）。

## 设计方案（LEP 四面矩阵）

- Git 磁盘格式：不变。线协议：N/A（纯本地）。SQLite：增量迁移 2026070202
  （幂等 + `_down.sql`；旧二进制打开新库被版本守卫拒绝——1.5 已实证并文档化）。
  CLI：仅新增 flag/命令，默认 `status` 字节不变（JSON 无新键，测试钉住）。
- 新鲜度契约：`working_dirty_meta` 记录扫描时的 index 指纹（索引文件**尾部
  校验和**的 hex——O(1) 重算、对 mtime 粒度竞态免疫；宽度随 hash kind，绝不
  硬编码 20/32）与 HEAD OID（staged 快照是 index↔HEAD 事实，两者都参与
  校验）。消费者发现任何不匹配/缺行/显式 stale → 降级全量 reconcile +
  stderr 提示（`--scan` 重建）。因此所有改索引/HEAD 的命令**免费隐式失效**
  缓存（§7.1.1 的回退条款：索引文件与 SQLite 无跨域原子性，v1 不做逐命令
  carry-over——文档化的后续增量）。快照语义：扫描后的纯工作树编辑不改指纹，
  对 `--cached` 不可见，直至 rescan 或 `libra dirty` 标记——这正是标记的用途。
- `--scan` TOCTOU 防护：reconcile **前**捕获指纹+HEAD，reconcile 后复核，
  不一致则中止（旧快照原样保留、锁经 guard 释放）；提交为单事务
  replace-all + meta 盖章（`--cached` 读者只见旧或新快照，无半更新）。
  扫描锁：meta 行 CAS（首扫播种）、>600s 陈锁可窃取（带警告；PID 锁为
  best-effort，已文档化）。
- `--cached` 快路径：O(dirty)——不走工作树、不载 HEAD 树对象（staged 直接取
  快照行）；仅载一次索引（O(index) 文件读，用于人工标记分类与显示装配）。
  人工 `unknown` 标记逐个内容确认（calc_file_blob_hash + verify_hash，
  **刻意不用会 panic 的 `Index::is_modified`**）；干净标记从视图剔除但不写库
  （快路径只读）。无重命名检测（需对象加载，文档化）。
- `--check-dirty`：逐行复核（tracked 守卫 + 存在性 + 内容确认），干净行
  prune、幸存行盖 `verified_at`，单事务。staged_* 行不复核（fp+HEAD 已保）。
- 路径存储：仓库相对、'/' 规范化（Windows 往返经
  `native_path_to_stored`/`stored_path_to_native`，带单测）。
- 属主 API：`internal::dirty::DirtyCache`（`_with_conn` + pool 包装约定），
  其它代码不得直触两表。

## 实现历史

- 2026-07-02（lore.md Phase 1 / 1.1）：初版全套 + 5 个 e2e 测试。

## 当前状态

- 测试：`tests/command/dirty_test.rs`（scan/cached 往返含 staged 快照、
  index 写失效降级、人工标记+check-dirty prune、默认 status 不触缓存 +
  JSON 键稳定、锁释放）；`internal::dirty` 单测（路径往返、classify 矩阵）；
  迁移注册表 15 项。
- 用户文档：`docs/commands/dirty.md`；status 文档新增三 flag 说明。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| carry-over | add/commit 等命令的逐命令缓存保养（§7.1.1 正文） | v1 统一走指纹失效（其回退条款）；后续按命令渐进（write-index-then-restamp，崩溃间隙退化为 stale——仍安全）。 |
| watcher | FS 事件喂养 | 1.11 服务化的加速器，非正确性前提。 |
| perf CI | 7.6.1 基准作业 | v1 以行为测试代证（--cached 不走全树）；基准归 CI 项。 |
| 重命名 | --cached 视图的 rename 检测 | 需对象加载，与 O(dirty) 冲突；显示为 delete+new。 |

## 维护要求

- 改进前先读 [_general.md](_general.md)。任何对两表的读写必须经
  `DirtyCache`；新增 status 快路径消费者必须先 `classify` 且在任何疑问下
  降级全量——缓存可过报/降级，绝不静默漏报已记录的事实。
