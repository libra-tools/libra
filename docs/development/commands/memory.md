# memory 命令开发设计

## 命令实现目标

`libra memory` 是「可重建侧表 + 读时新鲜度校验」范式的查询面（ADR-DM-02，
对齐既有 `revision_ordinal` 模式）：读取并重建 plan-20260926 MEM-01/02 的
零权威 Agent 研发历程记忆投影。它不持有任何权威状态，所有 `memory_episode`
都是仓库 durable 事实的纯函数，可随时以 `libra memory rebuild` 重新派生
（GC-DM-01）。

本命令分两卡交付：

- `DM-03`（已 done/complete）：注册 `libra memory` 公开命令表面（clap
  变体、dispatch、`command_scope=ReadOnly`、`classify_command=ReadOnly`），
  四个子命令名为占位。
- `DM-11`（本卡）：交付 `status` / `list` / `show` / `rebuild` 行为、稳定
  错误码 `LBR-MEMORY-001/002` 与五份文档页。本卡是 `REL-DM-02` 家族子卡
  2/3，不 bump。

`recall`（DM-04）与 `search`（DM-08）不在本卡。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Libra-only 确定性 Agent
  development-history 投影，无 Git 类比（`COMPATIBILITY.md` 仅登记这一张
  表）。`status`/`list`/`show`/`rebuild` 是只读命令（`ReadOnly`），但
  `rebuild` 触达投影表。
- 退出码：0 成功；128 `LBR-MEMORY-001`（陈旧 fail-closed）与
  `LBR-MEMORY-002`（Episode 不存在）；129 usage（`LBR-CLI-002`）。非仓库
  报 `LBR-REPO-001`。
- 稳定错误码契约（ADR-DM-10）：陈旧投影永不作答，除非显式 `--allow-stale`
  （后者在 `--json` 置 `"stale": true` 并在人读输出打横幅）。

## 设计方案

### 读取语义（fail-closed）

- `status` 读 `memory_projection_state` 中 `repo_id + source_kind='commit'`
  的行，逐字取自持久化列：`schema_version`、`rules_version`、
  `horizon_truncated`、`revoked_count`、`aged_out_count`、`rebuilt_at`。
  `selector_version` 是冻结常量 `1`（GC-DM-05）。
- 新鲜度判定 = 存储指纹 与「当前 horizon 窗口 operation 集合指纹」比较。
  不用 `rebuilt_at` 或 `max(end_ts)`（ADR-DM-03）：`operation` 原地终态化，
  `max` 会漏同毫秒/迟到终态化。`commit_fingerprint` 与 `rebuild` 同一实现，
  保证「fresh == 重建后一致」。
- 无投影行（从未派生）是「空基线」：`status` 报告默认值并把 `stale` 置为
  `true` 但不 fail-closed（不当作陈旧错误）——因为没有任何可作答的陈旧索引，
  一个空窗口就是诚实的当前状态。`list`/`show` 同理返回空窗口/未命中。
- `list` 返回窗口内 Episode，主键为确定性顺序（`source_kind, source_key`
  升序，GC-DM-05）；`show` 按 `episode_id` 取单条，未命中映射
  `LBR-MEMORY-002`。
- `--json`（ER-11）：绝不泄漏主机绝对路径。`code_path` 以仓库相对字节串
  输出（`render_code_path` 只做展示级归一化——丢控制字符、剥 `./`/绝对前缀，
  不改写入库字节）。

### 稳定错误码

| 代码 | 分类 | 触发 | 语义 |
|---|---|---|---|
| `LBR-MEMORY-001` | `repo` | `status`/`list`/`show` 遇到陈旧投影且未传 `--allow-stale` | ADR-DM-10 fail-closed |
| `LBR-MEMORY-002` | `repo` | `show` 传入不存在的 `episode_id` | GC-DM-01 / ADR-DM-01 |

两者均 exit `128` 并进 `repo` 分类（仓库投影状态问题，非 CLI usage）。
登记完整（变体、`as_str`、`category`、`description`）于
`src/utils/error.rs`，同步 `docs/error-codes.md` 与 `compat_error_codes_doc_sync`
守卫。

### 读取层（`src/internal/ai/memory/`）

- `reader.rs`：`list_episodes` / `read_episode` / `read_status` /
  `projection_is_stale`，以及 `EpisodeView`/`ProjectionStatus` 展示类型。只读
  投影表，绝不写。`commit_operations` 是 `projection.rs` 的专用镜像
  （`rebuild` 的指纹实现保持封装）。
- `episode.rs`：为 `SourceKind`/`Outcome` 增加 `TryFrom<&str>`（DB 行反序列化
  需要），golden 向量不变。
- `derive_commit.rs`：`load_commit_fields` 增加 `strip_commit_signing_headers`
  ——`Commit.message` 会包含 `gpgsig`/`gpgsig-sha256` 头块（Libra 默认
  签名提交），之前会把签名头误当作 subject/body；本卡修正为剥离头块后取首行与
  其余正文。

### CLI

- `src/command/memory.rs`：`execute_safe(args, &OutputConfig)`（跟随
  `execute_safe` 约定并把 `--json` 落到 `emit_json_data`）；`allow_stale` 为
  clap `global` 旗标（`memory status --allow-stale` 与
  `memory --allow-stale status` 均可用）。

## 实现历史

- 2026-10-08（DM-03）：注册公开表面（占位子命令、`ReadOnly`、无 `operation`
  行）。PR #623，merge `e237365`。
- 2026-10-09（DM-11）：交付 `status`/`list`/`show`/`rebuild` 行为、
  `LBR-MEMORY-001/002`、五份文档页与相应测试。

## 当前状态

- 公开状态：`Commands::Memory`。
- 测试：`tests/command/memory_test.rs`（`memory_writes_no_operation_row`、
  `status_envelope_is_frozen`、`stale_projection_fails_closed`、
  `list_show_rebuild_roundtrip`）。
- 验证序列：见 DM-11 卡的 Verification 与「不计入 G-03 的强制门」。
- 用户文档：`docs/commands/memory.md`、`docs/commands/zh-CN/memory.md`；
  后端站点页 `../libra-backend/apps/tanstack-app/content/docs/commands/memory.en.md`
  由 `DM-12` 按 `DEP-DM-07` 推送。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 子命令 | `recall`（路径召回 + 读时 drift） | `DM-04`；`selector_version` 排序随之冻结 |
| 子命令 | `search`（FTS5 BM25） | `DM-08`；仅 `search` 降级 |
| 增量 | 读时自动增量追平 | 本卡只读 + 显式 `rebuild`；增量追平在 DM-13 已落地投影层，命令面暂不自动触发 |
| 源 | `agent_session` / `agent_run` 适配器 | `DM-05` / `DM-07`，不在本卡 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- 任何新增/变更的稳定错误码必须同步 `src/utils/error.rs`、
  `docs/error-codes.md` 与 `compat_error_codes_doc_sync`、Add 一个
  Display-pin 测试。
- 冻结读取契约（`status_envelope_is_frozen`）不可在无 `selector_version`
  bump 的前提下改排序或字段。
