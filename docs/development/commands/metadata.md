# metadata 命令开发设计

## 命令实现目标

`libra metadata` 是 branch/repo 元数据 KV 存储（`lore.md` §1.5），为 branch
protect/archive/lineage 提供地基。v1 提供最小可用面：`get`/`set`/`unset`
（别名 `clear`，对齐 Lore 的 `metadata get/set/clear`）/`list`，两个作用域
（`--branch <name>` | `--repo`，必选且互斥）。类型化元数据命令族
（revision/file 作用域、typed values）为 1.10 后续，在同一命令上扩展；
protect/archive 的**执行**（reset/delete/push 阻断）统一落在未来的
branch-policy 层（1.13），本项刻意零执行——避免 lore.md §3.6 禁止的策略散点。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Git 无一等元数据存储；最近类比是
  `git config branch.<name>.*` 与 `git notes`。元数据仅本地（不 push/pull/publish）。
- 退出码：0 成功；1 为 `get`/`unset` 未命中（对齐 `config` key-miss 先例）；
  128 非仓库；129 usage（scope 缺失/重复、非法 key、超限 value、分支不存在
  ——`LBR-CLI-002`/`LBR-CLI-003`，Libra CLI 错误约定）。

## 设计方案（四面兼容矩阵，§3.0.1 门禁）

- **Git 磁盘格式**：不变——不触 objects/index/pack/refs 文件字节；元数据只在
  `.libra/libra.db`。**旧 Libra 打开新库：被显式版本守卫拒绝**（"schema
  version … is newer than this Libra binary supports"，实测确认）——这是刻意
  的防护（旧二进制不静默读写它不理解的 schema），不是静默容忍；升级二进制即
  恢复。新 Libra 打开旧库：幂等增量迁移自动补齐。运维注意：任何新二进制的
  运行（包括测试/冒烟）都会顺带升级它连接到的库——含**全局** `~/.libra/config.db`
  （config 级联读会触达）——升级后旧二进制将无法读取全局身份等配置，须同步
  更新安装的二进制（本项落地时实测踩中，`cp target/release/libra ~/.libra/bin/`
  即恢复）。
- **Git 线协议**：N/A（有因）——纯本地特性，push/pull/clone 不传输元数据。
- **SQLite schema/迁移**：增量表 `metadata_kv`（migration `2026070201`，
  `CREATE TABLE IF NOT EXISTS` 幂等，配套 `_down.sql` DROP）。列：
  `scope/target/key/value/value_type('text' 预留 1.10)/created_at/updated_at`，
  `UNIQUE(scope,target,key)`。回滚（操作可执行版）：
  `sqlite3 .libra/libra.db < sql/migrations/2026070201_metadata_kv_down.sql`
  + `DELETE FROM schema_versions WHERE version=2026070201` + **钉住旧二进制**
  （新二进制下次连接会自动重放正向迁移，空表回归）；分支元数据随 down 全部
  丢失，repo 作用域（config_kv）不受回滚影响（不对称，操作者须知）。
- **CLI/公共 API**：仅新增命令；既有 Git 兼容命令默认语义零变化。

### 存储

- 单一属主 API：`internal::metadata::MetadataKv`（lore.md §3.6 红线：所有
  读写走一个 API）——`get/set/unset/list/delete_all_for_target/rename_target/
  copy_target` 均含 `_with_conn` 变体 + pool 包装（ConfigKv 约定）。`set` 用
  `INSERT … ON CONFLICT DO UPDATE`（sea-orm `OnConflict`）原子 upsert，无
  find-then-insert 竞态。
- repo 作用域**不建新表**：`ConfigKv` 下 `metadata.*` 命名空间
  （`REPO_METADATA_PREFIX`，lore.md「repo=config_kv」）。双面入口为设计意图：
  `config --add` 造成的多值键，`metadata set/unset --repo` 以带提示的 usage
  错误拒绝（hint: `config unset-all`），`get` 取最新值；敏感样键与既有加密行被 `set --repo` **拒绝**（129 + hint 走 config 门：config 拥有 vault 加密决策，此处写明文会存裸密钥或损坏加密行）；`get`/`list` 对加密值渲染 `<REDACTED>`（解密走 `config --get --reveal`），`unset` 不受限（纯删除）。
- 生命周期：分支删除在 `Branch::delete_branch_result_with_conn` 内级联
  （`remote.is_none()` 守卫——prune 远端跟踪分支绝不误删同名本地分支元数据；
  pool 连接上 ref 删除与级联为两个隐式事务，崩溃间隙的孤儿行是惰性的——
  读取按 target 键控，且重名重建/再删除时被清扫）；重命名在删旧 ref **之前**
  `rename_target`（否则级联会先清掉待搬的行）；复制在 upstream config 复制后
  `copy_target`（`-C` 强制复制会替换目标分支元数据——与 ref 覆盖一致的破坏性，
  已文档化）。`op restore` 复活分支不复活元数据（操作日志不版本化本表）——
  v1 已知限制。
- 1.13 消费契约：`is_protected_with_conn`/`is_archived_with_conn`（fail-closed
  truthy 解析：非显式 falsy 值一律记为受保护，坏值绝不静默解除保护），供
  branch-policy 在其权威事务内读取。

### CLI

- `src/command/metadata.rs`：`MetadataArgs`/`MetadataCommand`、共享
  `ScopeArgs`（clap `#[group(required=true, multiple=false)]`）、
  `MetadataOutput`（`#[serde(tag="action")]`）。key ≤256B 无空白/控制字符，
  value ≤1MiB，空串合法且与缺失不同（get 空串退出 0，缺失退出 1 经
  `CliError::silent_exit(1)`）。设置 `protect`/`archive` 输出一行 stderr
  提示（1.13 已落地 reset/update-ref 强制，措辞更新为「enforced for `branch reset`/`update-ref`; delete/push/merge enforcement pending」）。

### revision 作用域（1.10 第二增量）

- 双层模型：不可变 trailer 块（`internal::log::trailer`，`get` 以 requested-key-as-recognized 强化混合块合格；`list` 用普通规则——文档化不对称）+ 可变 notes 层 `refs/notes/metadata`（每提交一个版本化 JSON 文档 `{version:1, entries:{key:{value,type}}}`，BTreeMap 确定性序列化，全文档 ≤ MAX_VALUE_LEN；损坏/未知版本 → 指名 ref+OID 的可操作错误 + 修复提示）。notes 优先（唯一可变层须能覆盖烤入的 trailer）；key 两层均 ASCII 大小写不敏感（trailer 约定，分支/repo 仍精确——`validate_key` 注释已同步）。写只走 notes 层（`notes::add(force=true)` 读改写；并发写可丢更新——v1 文档化限制；清空文档时 `notes::remove` 的 CAS 未命中映射为重试提示错误）。`list` 合并序：key 大小写不敏感排序，同 key note 先于 trailer，trailer 重复项保持消息序；`--prefix` 在本作用域大小写不敏感。protect/archive 通知仍仅限 branch 作用域（revision 上是普通键，1.13 不消费）。本作用域使 metadata 成为 object-touching 命令：已移入 cli.rs 的 hash-kind preflight（sha256 仓库正确散列；出仓错误由 preflight 统一给出）。「revision 用 trailers/notes」为 `gap/lore.md` §3.2 的 1.10 typed metadata 行（现 `:229`）对 §3.6「typed metadata｜repo=`config_kv`，其余走统一 metadata 表」红线（现 `:295`）的显式豁免（不开新表、单一属主 API）；本增量无收敛/替换（同 §3.6「退役策略」段，现 `:299` 空满足——无被替换物，无需只读兼容窗口）。

### file 作用域：延后（独立设计轮）

- 现状：仓库无任何 side-tree 机制（全库 grep 仅计划行与注释）；`gap/lore.md` §3.2 的 1.10 行（现 `:229`）「file 用 side-tree」与 §3.6（现 `:295`）「其余走统一 metadata 表」互相矛盾，须先裁决。廉价路线（metadata_kv `scope='file'`）亦有未解的路径生命周期语义（mv/rm/restore 级联、规范化、存在性检查）且无现成钩子。本项记录矛盾与设计草图，file 作用域待独立设计轮 + LEP 门禁。

## 实现历史

- 2026-07-02（`lore.md` Phase 1 / 1.5）：初版——migration `2026070201`、
  `metadata_kv` 模型、`MetadataKv` 存储、branch delete/rename/copy 生命周期
  钩子、`libra metadata` CLI、4 个 e2e 测试 + 存储单测 + 迁移测试扩展。

## 当前状态

- 公开状态：已公开（`Commands::Metadata`）。
- 测试：`tests/command/metadata_test.rs`（roundtrip/双面 repo 作用域/错误矩阵/
  生命周期）、`src/internal/metadata.rs` 单测（校验器、fail-closed 解析）、
  `tests/db_migration_test.rs`（注册表 14 项、up/down/re-up 往返）。
- 验证序列：`cargo test --test db_migration_test`；`cargo test metadata_test`；
  `sqlite3 .libra/libra.db "SELECT name FROM sqlite_master WHERE name='metadata_kv'"`；
  `SELECT MAX(version) FROM schema_versions`（期望 2026070201）。
- 用户文档：`docs/commands/metadata.md`。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 作用域/类型 | revision/file 作用域（`--binary`/`--numeric` 已于 1.10 第一增量落地：`MetadataValueType` + `validate_typed_value`，numeric=i64 或有限 f64、原文存储不规范化，binary=标准 base64 文本存储（原始载荷 ≤ ~值上限的 3/4）；`--repo` 拒绝类型旗标——config 存储无 value_type 列，为显式后续项） | revision 作用域随 1.10 第二增量；file 作用域待独立设计轮。 |
| 执行 | protect/archive 的 reset/delete/push/merge 阻断 | 1.13 branch-policy 层统一落地（消费 `is_protected_with_conn`）；v1 仅记录 + stderr 提示。 |
| 同步 | 元数据 push/pull | 触线协议，独立 LEP；v1 明确仅本地。 |
| 恢复 | `op restore` 复活分支时恢复元数据 | 操作日志不版本化本表；已文档化，protect 需重设。 |
| 清扫 | 绕过属主 API 的 ref 删除留下的孤儿行 | 今日无此路径（§3.6 红线禁止）；gc/fsck 孤儿清扫为后续项。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 任何对 `metadata_kv` 的读写必须经 `internal::metadata::MetadataKv`（单一
  属主 API）；新增 ref 删除路径必须复用 `Branch::delete_branch_result*`
  以保级联。迁移编号在 rebase 时须对并行落地的迁移重新校核（注册表严格递增，
  冲突会 panic-loud）。
