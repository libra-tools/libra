# Agenta 版本管理方案剖析与对 Libra 的改进建议

> ⚠️ **校正须知（v0.17.1759 提出，2026-08-27 v6 二次更正）**：本文件多处把 `src/command/gc.rs`（及 `collect_roots_from_database`、`agent_checkpoint_roots`、`gc.rs:1260-1297`/`gc.rs:1709` 等符号/行号）当作 GC 的现行实现来分析。**事实更正（仍成立）**：那份 `src/command/gc.rs` 是**从未在任何 `mod.rs`/`cli.rs` 声明、从未编译进二进制的孤立死代码**，已于 v0.17.1759 删除；唯一被编译、会运行的 GC 实现是 `src/command/maintenance.rs::run_gc`（`maintenance.rs:591`），其唯一命令入口是 **`libra maintenance run --task gc`**（`MaintenanceTask::Gc`，`maintenance.rs:146,221,271`）——**`libra gc` 这个命令不存在**。
>
> **但 v0.17.1759 版校正须知里“不读 database roots、也不含 operation_view roots”这一半已被推翻（2026-08-27 核实）**：现行 `run_gc` 用表驱动的 `collect_registered_store_roots`（`maintenance.rs:3308-3441`）收集 database roots，其中 `operation_view_ref.target_oid`（`CellMode::StrictOid`，非法值 fail-closed 报 `RepoCorrupt`）、`operation_view.head_target` 与 `operation_view_workspace.pointer_value`（均 `CellMode::OidIfParses`，非 hash 值静默跳过）**都已是 traced GC root**；三者另有 `GC_OBJECT_SOURCE_INVENTORY` 分型账本条目（`maintenance.rs:3081-3090,3180-3190,3191-3200`），并由 `tests/db_migration_test.rs:3693-3700` 的「OID 形列必须被账本覆盖」守卫钉住。因此本文 §5 P0-1 / §6 A1 的**前半（GC roots）已实现**，后半（`op restore` 缺对象 fail-closed）仍是缺口——详见 §0.6。下文所有 `gc.rs` 引用按历史快照处理。

> 读者：Libra（AI-agent-native、Git 兼容、refs 存于 SQLite 的版本控制系统）维护者
> 方法：先剖析 Agenta 的版本管理本质，再逐条对照 Libra 源码验证，只保留可落地的建议。所有 Libra 侧结论均已对照源码核验。

**文档元信息**
- 状态：草案（design proposal）。A/B/C 组可按本文件拆小 PR 进入实现队列；D 组仍等待维护者做产品边界决策。
- 文档版本：**v6**（2026-08-27 第六轮核验：2 处结论推翻、1 处设计判断被现实否决、1 处竞品能力反转、~30 处锚点漂移；按 §8.2 属结构性修订，故 bump）。
  **v6.1**（同日，对抗式复核后二次落笔）：修掉 v6 自身的 2 处编造事实（错误码数量、竞品测试文件位置）、2 处顶层入口口径与 §6.0 状态列相反、5 处锚点/表述偏差；**无结论翻转、无状态列变更，故按 §8.2 第 2 条不 bump 主版本**。逐条订正清单与实测证据见 **§0.6 末「v6.1 复核订正」**。 **v6.1.1**（同日第三次落笔）：只做 2 处 P2 精度订正——`lore.md` 锚点改按符号引用（v6.1 改出的 `:228`/`:606` 取自未提交工作树帧，HEAD 帧与当前工作树帧均不成立），并补记「锚点取值帧」声明；无结论翻转、无状态列变更、无条目增删，同样不 bump 主版本。
- 最近一次源码核验：**2026-08-27**，对照 git HEAD **`89081277a`**、`Cargo.toml` version **`0.21.27`**。**注意本 checkout 由 git 管理、无 `.libra/`，须用 `git rev-parse HEAD`**（v5 写的 `libra rev-parse HEAD` 在此 checkout 上已失效）。核验记录见 §0.2（首轮）、§0.3（二次交叉验证）、§0.5（v5 第三轮）与 **§0.6（v6 本轮，覆盖式更正）**。 **⚠️ 锚点取值帧（v6.1.1 补）**：Libra **源码**及 `COMPATIBILITY.md`、`docs/error-codes.md`、`plan-20260822.md` 的锚点均对照 git HEAD `89081277a`（HEAD 帧与工作树帧一致，可直接复核）；但 `docs/development/plan/plan-long.md` 与 `docs/development/gap/lore.md` 两份文档核验时**处于未提交修改状态**（`git status` 为 ` M`），文中引用它们的行号取自 **2026-08-27 当日工作树快照**，与 HEAD 帧不同——例：`plan-long.md:520/555` 在 HEAD 为 `:511/:544`，`:200-201` 的 LR-02/LR-03「已排期」在 HEAD 是 `:197-198` 的「已验证」，而 `:73,111` 记的 agenta `blocked-timeout` 在 HEAD 帧**尚不存在**（HEAD `:73` 仍写 `up-to-date`）。复核这两份文档时须先 `git diff --stat` 确认帧，或直接按标题/表格行文本定位，不要按行号盲信。
- **锚点漂移须知**：文中所有 `file:line` 均为“符号优先、行号为提示”的引用。源文件持续演进，行号会漂移（本次核验即发现多处，已记入 §0.2）。落地前请以**函数/符号名**重新定位，不要信任行号；若做长期引用，请 pin 到具体提交哈希而非行号。
- **§5 与 §6 分工**：§5 保留动机、取舍与风险；§6 只写可拆 issue/PR 的执行增量。**若两处描述冲突，以 §6 验收标准与 §0.3 二次验证为准**；§5 中的 rationale 不在 §6 重复，避免双份漂移。

---

## 0. 落地状态与执行口径

**本版结论：可以直接进入实现队列，但必须按“可回滚小 PR”落地。** Agenta 给 Libra 的价值不在于复制它的关系型“Artifact / Variant / Revision”数据模型，而在于三条工程纪律：声明状态后校验、历史记录必须可恢复、对 agent 输出稳定的机器可读 provenance。下面的建议已经按 Libra 当前源码重新收窄。**当前仅剩 2 个可落地的独立 tracer bullet：`commit --assert-staged`（B1）与 ref 级 CAS `--expect-head`/`--expect-branch`（B2）**；原列的另两个已不再是排期项——operation log GC roots **已实现**、`<ref>@vN` 稳定句柄**已被 `lore.md §1.16` / `libra revision` 替代**（详见 §0.6 第 1/2 条与 §6.0 状态列），operation log 的「全命令覆盖」半边已由 `plan-20260822` **OL-08/OL-09** 承接。（这 4 项是 v1 起就固定的 tracer-bullet 清单；清单**之外**仍有开放卡——B4「抽出 `merge_commits` 原语」与 C/D 组，完整状态一律以 **§6.0 状态列**为准。）

**Agenta 快照 revision（v6 新增）**：`/Volumes/Data/competition/agenta-ai/agenta`，revision **`53717db55ec9311887be6fe86a67b2007590b6f3`**（`main`，2026-08-21）。**证据强度限制**：`plan-long.md:73,111` 的第九次竞品审计把该仓库记为 **`blocked-timeout`**（`libra pull --ff-only` 连续 >90s 超时；本地 = `origin/main` 但**未证明是远端最新**），故本文所有竞品结论仅以「该 revision 上的源码事实」为限，不宣称覆盖 Agenta 最新上游。

**源码依据（核验范围；行号为 2026-08-27 实测，v5 及更早的行号已全部漂移）**：
- Agenta 版本内核：`/Volumes/Data/competition/agenta-ai/agenta/api/oss/src/core/git/types.py`（全文 455 行）、`core/git/dtos.py`（全文 149 行，`RevisionCommit` 在 96）、**`api/oss/src/dbs/postgres/git/dao.py`（全文 2025 行——注意它不在 `core/` 下，v5 的 `core/git/dao.py` 路径是错的）**：`fork_variant` **914**、`create_revision` **1040**、`commit_revision` **1607**、`_get_version` **1961**、`_set_version` **1985**。
- Agenta environment 指针：`api/oss/src/core/environments/dtos.py`（全文 326 行）、`core/environments/service.py`（全文 1875 行）：`commit_environment_revision` **1052**、`publish_revision_event` 调用 **1144**。
- Agenta 仓库工作流反例：`AGENTS.md`（全文 281 行；GitButler 章节起于 **29**，硬核踩坑/oplog 恢复段在 **95-180**）、`.pre-commit-config.yaml:28-37`（全文 37 行，gitleaks staged/pre-push 双 hook）、`.husky/pre-push`、`.github/workflows/01-create-release-branch.yml:57-189`（全文 189 行）。
- Libra 对照点：`src/command/maintenance.rs`（5062 行，`run_gc` **591**、`collect_registered_store_roots` **3308-3441**、`GC_OBJECT_SOURCE_INVENTORY` **2871** 起；孤立的 `src/command/gc.rs` 已于 v0.17.1759 删除）、`src/internal/operation.rs`（1616 行）、`src/internal/operation_wrapper.rs`（2132 行；`with_operation_log` **405**、`with_operation_log_with_conn` 定义 **425**（**419** 是 `with_operation_log` 内的调用点，非定义）、`collect_final_view_with_conn` **1444**、`pointer_value = head_target.clone()` **1509**）、`src/utils/util.rs`（4228 行；revspec 解析器已整体重构，见 §0.6 锚点表）、`src/command/commit.rs`（3765 行）、`src/command/push.rs`（4019 行）、`src/command/reset.rs`（2566 行）、`src/command/merge.rs`（3987 行）、`src/internal/ai/orchestrator/workspace.rs`（`sync_task_worktree_back` **1041**）。

**执行原则**：
- 每个建议必须是“新增保护 / 新增 opt-in 能力 / 新增结构化输出”，不能破坏现有 Git 兼容行为。
- 新 `StableErrorCode` 必须同步 `src/utils/error.rs`、`docs/error-codes.md`、`tests/compat/error_codes_doc_sync.rs` 覆盖的文档目录；若可复用 `LBR-CONFLICT-002` 就优先复用。**注意该守卫只识别数字后缀码**（`compat_error_codes_doc_sync` 解析逻辑只接受 `LBR-<UPPERS>-<digits>`），任何形如 `LBR-REF-INCONSISTENT` 的码会被**静默跳过**而非报错——见 §5 P2-7。
- 新命令或新公开 flag 必须同步 `docs/commands/<cmd>.md`、`docs/commands/zh-CN/<cmd>.md`（若已有对应中文页）、`COMPATIBILITY.md`、`tests/INDEX.md`（仅新增/改名 cargo test target 时）。
- **`--help` EXAMPLES 契约**：任何新命令或新顶层子命令都须提供 `<CMD>_EXAMPLES` 常量并经 `#[command(after_help = …)]` 接线，否则会被三个守卫拦下：`compat_help_examples_banner`、`cli::tests::root_after_help_lists_every_visible_command`、`compat_command_docs_examples_section`。新增 flag（如 `--assert-staged`/`--expect-head`）无须新常量，但应在对应命令的 EXAMPLES 与 `docs/commands/<cmd>.md` 的 Examples 段补一条示例。
- **结构化输出落点**：本文多条建议“给信封加字段”。Libra 实际有两种 JSON 信封形状——`--json` 路径 `emit`/`emit_list` 输出 `{ok,data}`，`--machine`/命令信封 `write_json_command_envelope` 输出 `{ok,command,data}`（`src/utils/output.rs`）。新增字段一律加在各命令的 `*Output` 结构体里（落在 `data` 内），对两种信封都安全；不要假设顶层一定有 `command` 键。
- **测试分层**：本文所有新测试都是 L1（确定性，tempdir + 内存/mock），不依赖网络或真实 LLM/云，应随 `cargo test --all` 默认运行；不要把它们误挂到 `test-network`/`test-live-ai`/`test-live-cloud` 门后。
- 每个 PR 都先加失败测试，再实现，再跑对应窄测试；不要用一次大 PR 同时改 GC、op log、commit、revspec 和 docs。
- **默认兼容口径**：除 C2 的 push dropped-path guard（默认 `warn`）与 A1 的 `op restore` 缺对象 fail-closed 外，所有新能力必须是显式 opt-in flag、附加 JSON 字段或只读 revspec。任何会改变默认 exit code、stdout/stderr 形状、对象格式、ref namespace 可见性的实现，都必须先回到本文更新设计。

---

## 0.1 多维度评审结论（2026-06-28）

本节是按 11 个维度对**本方案整体**的评审摘要（逐条建议的细化结论仍在 §5）。评级口径：**强** = 该维度已被方案系统性处理，可直接落地；**合格** = 基本到位，附小幅补强项；**需补强** = 有真实缺口，已在对应位置加注。

| 维度 | 评级 | 关键判断与已做的修订动作 |
|---|---|---|
| 合理性 | 强 | 核心论点正确：借鉴 Agenta 的**三条工程纪律**（声明后校验 / 不可变可恢复历史 / 稳定机器可寻址句柄），而非照搬其关系型 Artifact/Variant/Revision 数据模型；并明确拒绝 Agenta 自承的缺陷（O(n) 拷贝式 fork、per-variant 版本表、无 CAS 的 delta 部署）。无过度设计。 |
| 可行性 | 强 | 每条建议都落到**已核验存在**的文件/函数，附 small/medium/large 成本；P0 两条确为小/中量级，large 项（P1-5、P2-9）已诚实标注并给出增量切法。补强：§6 新增**依赖与关键路径矩阵**，避免乱序开工。 |
| 完整性 | 强 | 已补齐：`--help` EXAMPLES 三守卫、`tests/INDEX.md`、JSON 双信封、中文文档路径（`docs/commands/zh-CN/`）、§6 提案间交互与 §6.0.2 回滚矩阵。二次验证另补：B1 须避免 `changes_to_be_committed_safe()` 二次 load index（§0.3）、A2 的 `@v` 解析顺序（§0.3）、A1 错误码语义（勿用 `LBR-REPO-002`）。 |
| 安全性 | 强 | `parse_stored_hash` fail-closed 成立；P2-10 外锚判断正确。二次验证补强：B1 manifest 路径须走 repo-relative 校验防 `../` 穿越；P2-10 SQLite trigger 仅防误操作/应用层 UPDATE，**不能**抵御持有 DB 文件写权限的攻击者（须外锚或 OS 级权限）；`--assert-preview` digest 须 canonical JSON 序列化以免键序漂移误报。 |
| 功能正确性 / 接口兼容性 | 强 | 全部为附加式 opt-in、不破坏既有 dry-run JSON。`is_locked_revision` 在 `@` 处截断（`branch.rs:87`），故 `main@vN` 写操作与 `main` 同等受锁——读寻址须在 revspec 层单独实现 `@v` 后缀（见 A2）。修正：A1 缺对象应用 `LBR-REPO-003`（状态不可恢复），**不是** `LBR-REPO-002`（仓库损坏）。 |
| 数据流 / 控制流正确性 | 强 | 已独立确认 commit 的 reflog `old_oid` 在事务外计算（`commit.rs:1921`）→ §6 B2 的 TOCTOU 警告成立；“勿重载 index、复用同一内存快照”（P1-3）、“两个各自开事务的 wrapper 必须合一、不可嵌套”（P1-5）、“可恢复序列跨进程、单事务无法跨越”（P1-5）均为正确的并发推理。 |
| 性能 / 效率 | 合格 | P1-5 已识别 commit 热路径写放大并给出 view 去重方案；P0-2 改为“按需 DAG、不建表”避免写路径成本。补强：P0-2 的 `@vN` 解析为**每次 O(depth) first-parent 回溯**，深历史热循环下需注意——已在 P0-2 加性能注记。 |
| 可靠性 / 容错性 | 强 | P0-1 让 `op restore` 改 ref 前 fail-closed、P1-5 破坏性操作单步原子 undo、CAS 失败即回滚——容错姿态一致：先校验、后改 HEAD。 |
| 兼容性 / 互操作性 | 强 | 每条都以“对 git on-disk 格式零影响”为前置；deploy/AI orphan ref 不 push 到 stock git；intentionally-different revspec（`@vN`、`@{deploy:}`）已要求写入 `COMPATIBILITY.md` 并明确不宣称 git 兼容。 |
| 可扩展性 / 可维护性 | 强 | 已通过 §5/§6 分工规则、§8 文档维护约定、符号优先锚点降低漂移风险。P1-5 view 去重、P0-2 只读 depth 缓存、P2-9 共享 `Preview` 类型均为可扩展挂点。 |
| 合规性 / 标准符合性 | 强 | 遵循项目既有约定（`StableErrorCode` 命名与 doc-sync、`COMPATIBILITY.md` 四级矩阵、迁移命名与冻结的 init schema、compat 守卫）。本次补齐 `--help` EXAMPLES 契约这一原文遗漏的合规点。 |

**总评**：方案**可以进入实现队列**，A/B 组（P0-P1）无阻塞性问题；C 组为增益项；D 组（env/promote、per-worktree HEAD）必须先做产品边界决策。未发现会导致 git on-disk 兼容破坏或默认行为回归的设计错误；v4 修订主要补强落地闸门、Agenta 参考项目再核验、默认兼容口径、输入限流与可观测性要求。

## 0.2 源码锚点核验记录（2026-06-28）

本次对 §5/§6 引用的 ~33 处 Libra 源码锚点做了独立复核（4 个并行只读核验 + 维护者抽验）。**结论：0 处结论性错误（WRONG）**；方案的源码依据可信。发现的漂移均已就地修正：

| 锚点 | 原文 | 实际 | 处置 |
|---|---|---|---|
| `rebase.rs` 行数 | “3384 行” | **4227 行** | 已改（§5 P1-4 风险注记）；`with_reflog` 三处调用点 1638/2064/2246 仍准确 |
| `with_operation_log` 跨度 | `operation_wrapper.rs:317-430` | 实际至 **535** | 已改 §0 source 依据 |
| `operation_view_workspace` 丢弃理由 | “值是分支名，无独有 OID” | `pointer_value = head_target.clone()`（`operation_wrapper.rs:689`）：**detached 时是 OID** | 已在 P0-1 改为精确表述——丢弃仍**安全**（该 OID 必同时出现在步骤 2 的 `operation_view.head_target`，detached 已覆盖） |
| 合同校验函数位置 | P1-6 暗示在 merge 路径 | `detect/collect/format_contract_violations` 实际在 `internal/ai/orchestrator/workspace.rs`，校验的是 **task-worktree-back**，非 merge 命令 | 已在 P1-6 澄清位置与作用域 |

已逐一确认为**准确**的关键事实（节选）：`collect_roots_from_database` 确不读任何 `operation_view*` 表；`head_kind` 写作小写字面量 `"detached"`（故 `WHERE head_kind='detached'` 精确成立）；`with_operation_log` 全仓仅 2 处接线（`branch.rs:979` branch create + `op.rs:447` op restore），branch **delete 未覆盖**；`merge_tree_items`/`create_tree_from_items_map` 为私有纯函数（P1-6 需暴露之）；`revert.rs` 经 `Branch::update_branch` 绕过 `with_reflog`；rebase 产树路径（3519-3686）均从对象库构建、不扫 workdir（故 P2-8 砍掉 rebase/merge 内闸门正确）；`sync_task_worktree_back` 确用 `diffy::merge_bytes` 文本合并、零 VCS 对象调用；`StableErrorCode` 为闭合枚举、`ConflictOperationBlocked=LBR-CONFLICT-002`、无 `PRECONDITION/STAGE/REF/PUSH/TREE/DEPLOY` 域；`expire_defaults_with_conn` 默认 **90 天** / **30 天** unreachable（`reflog.rs:530-535`），与 GC 预 prune 联动。

---

## 0.3 二次交叉验证记录（2026-06-28，对照同工作树 HEAD）

在 §0.2 基础上，维护者/agent 对落地风险最高的路径做了第二轮只读核验。**结论：方案结论仍成立；下列为当轮据此修正或补强的实现约束，v4 继续沿用。**

| 主题 | 核验结果 | 对文档/落地的动作 |
|---|---|---|
| **A1 错误码语义** | `LBR-REPO-002` = `RepoCorrupt`（解析/存储层损坏）；缺 commit 对象是**可预期的 GC/prune 后果**，不是 corruption | A1 改用 `LBR-REPO-003`（`RepoStateInvalid`）+ `missing_oid`/`operation_id` detail；仅在对象库结构损坏时用 `LBR-REPO-002` |
| **B1 index 快照** | `run_commit` 在 `Index::load`（`commit.rs:562`）后已调 `changes_to_be_committed_safe()`（573），但该函数**内部再次 `Index::load`**（`status.rs:2001`），与“同一内存快照”目标冲突 | 断言须基于已加载的 `index` 变量做 staged-vs-HEAD diff（新增 `changes_to_be_committed_from_index(&index)` 或内联等价逻辑），**禁止**为断言再调 `changes_to_be_committed_safe()` |
| **B1 dry-run 顺序** | `dry_run && -a` 时在 auto-stage 后、create_tree 前**写回** index 快照（`commit.rs:592-594`） | `--assert-staged` 校验必须在 index 写回**之前**完成；dry-run 验收须覆盖 `-a` + `--assert-staged` 组合 |
| **A2 `@v` 解析顺序** | `split_revision_navigation`（`util.rs:739`）仅在 `~`/`^` 处切分，**不识别 `@`**；`is_locked_revision` 在 `@` 处截断（`branch.rs:87`） | 在 `get_commit_base_typed` **入口**先剥终端 `@v<digits>`，再交给现有 `~`/`^` 导航；组合形式 `main@v3~1` = 先 ordinal 再 `~1`。预留 `@v` 与未来 git `@{upstream}`/`@{push}` 的 intentionally-different 命名空间 |
| **B2 reflog TOCTOU** | `new_reflog_context` 在 `with_reflog` **事务外**读 `old_oid`（`commit.rs:1921-1930`） | CAS 实现须把 expected/actual 比较与 `old_oid` 捕获都移入 `_with_conn` 事务内（§6 B2 已列，此处独立确认） |
| **P2-9 op restore dry-run** | `op restore --dry-run` 走 `println!`（`op.rs:405-428`），无 `--json` | C3 优先级正确；补 JSON 时不得删除人类 stdout，须与 C3“附加式 preview 键”一致 |
| **P2-7 错误码策略** | 无现成 `LBR-REF-*` 枚举变体 | MVP 优先 `LBR-CONFLICT-002` + 字段级 detail；仅当 agent 需按 category 分支处理时再新增 `LBR-REF-001` 并走完整 doc-sync |

**开放问题（不阻塞 A/B 组，须在对应 PR 前关闭）**：
1. `@vN` 对 merge commit 的 second parent 不参与 ordinal——是否在 `rev-parse --json` 回显 `ordinal_parent: first` 以免 agent 误用？
2. P1-5 commit 批次的 view 去重阈值（refs 集不变即复用 `view_id`）——第一批 reset 可不做，commit 批上线前必须有度量（refs 数 × op 频率）。
3. D1 `libra env` 与 `libra publish deploy`（Cloudflare Worker）——CLI 命名已规避，是否需在 `libra help` 加 disambiguation 一行说明？

---

## 0.4 再评审结论与实现闸门（2026-06-28，v4）

本节把用户要求的 11 个评估维度转成“实现前必须满足的闸门”。结论：方案方向合理、可行且与 Agenta 参考项目一致，但**只有在下列闸门逐项满足时才保持成立**。若实现偏离这些闸门，风险评级须重新评估。

| 维度 | 再评审结论 | 实现闸门 |
|---|---|---|
| 合理性 | 成立。Agenta 最新 `core/git/types.py` 明确把 `Reference(id, slug, version)` 设计成“冗余可校验引用”，且强调裸 version 不可识别；Libra 借鉴的是契约纪律，不是关系型模型。 | 不引入 Agenta 式 Artifact/Variant/Revision 三表到 Libra；不为 `<ref>@vN` 落物化版本表。 |
| 可行性 | 成立。P0/P1 都落在现有函数边界；最重的是 B3/C3，已拆成 tracer bullet。 | 每个执行卡必须能单独 revert；schema 变更只能出现在 C4/D1，且必须有 `_down.sql` 或明确“append-only 不回滚历史数据”。 |
| 完整性 | 基本完整。文档已覆盖错误码、双 JSON 信封、中文 docs、compat 守卫、测试分层；v4 只补执行验收矩阵和输入限流。 | 每个公开 flag/命令落地时必须同步 docs、Examples、compat 说明与至少一个 JSON/错误路径测试。 |
| 安全性 | 成立但需边界清晰。A1 fail-closed、B1 repo-relative 校验、C4 外锚威胁模型都正确。 | 所有用户提供路径、ref、manifest、preview hash 都必须限长并规范化；任何 manifest/preview digest 必须用 canonical serialization；SQLite trigger 不得被描述为密码学防篡改。 |
| 功能正确性 / 接口兼容性 | 成立。新能力总体为 additive/opt-in；唯一默认行为变化是 `op restore` 对已缺对象拒绝恢复，这是从悬挂 ref 改为显式错误。 | 默认 stdout/stderr 不得被替换；JSON 只能加字段；`--json` 与 `--machine` 两种信封都要测试；`@vN` 必须在 `~`/`^` 导航前解析并声明 first-parent 语义。 |
| 数据流 / 控制流正确性 | 成立。关键事务边界已识别：B2 必须把 expected/actual 与 reflog old_oid 放入同一事务；B3 不得嵌套 transaction。 | CAS 检查、ref 写入、reflog/operation 记录必须同事务；无法同事务的 index/worktree 状态不得宣传为原子 CAS。 |
| 性能 / 效率 | 可接受但需观测。A2 的 O(depth) 解析和 B3 的 O(refs) operation snapshot 是主要成本。 | A2 增加深历史/批量解析基准或至少单测中的计数约束；B3 第一批记录 operation_view_ref 行数，commit 接线前必须有 view 去重或明确性能数据。 |
| 可靠性 / 容错性 | 成立。总体策略是先校验、再写入、失败不移动 refs。 | 每个失败验收都必须断言 HEAD、branch、reflog、operation 表“不变”；~~涉及 GC 的测试必须覆盖 `--prune=now`~~（**v6：该闸门不可执行——`libra gc` 命令与 `--prune=<date>` 语义均不存在，见 §0.6 第 4 条；GC 测试改按 `PRUNE_GRACE_SECS` + 两趟隔离账本构造**）。 |
| 兼容性 / 互操作性 | 成立。对象格式和 stock git 互通不受影响；Libra-only revspec/ref namespace 均需明示。 | `COMPATIBILITY.md` 必须把 `@vN`、`@{deploy:}`、`refs/libra/deploy/*` 标为 intentionally different；Libra internal refs 不得默认 push 到 stock git remote。 |
| 可扩展性 / 可维护性 | 成立。§5 写 rationale、§6 写执行卡的分工降低漂移。 | 实现 PR 只能修改对应执行卡列出的范围；若新增 helper，应优先放在已有模块边界，避免为单个 flag 引入长期抽象。 |
| 合规性 / 标准符合性 | 成立。遵循项目错误码、docs、help examples、migration 和 test index 约定。 | 新 `StableErrorCode` 必须是 `LBR-<DOMAIN>-NNN`，并同步 `docs/error-codes.md` 与 doc-sync 测试；生产代码不得新增未解释的 `unwrap()`/`expect()`。 |

**Agenta 参考项目再核验补充**：`api/oss/src/core/git/types.py` 的模块 docstring 已把 reference 规则、异常注册契约和裸 version 拒绝写成域契约；`test_variant_ref_version_only_400.py` 还把六类实体的 variant version-only 400 行为固定为验收测试。这加强了本文对“声明后校验”和“字段级错误”的借鉴依据。`core/environments/service.py` 的 delta path 仍是“读最新 revision → 合成完整 references → commit 新 revision → publish diff event”，因此本文对环境指针“有审计价值但缺 CAS”的判断仍成立。

**不应推进的实现形态**：
- 不做 Agenta 式 O(n) fork/history copy；Libra branch 已是 O(1) ref 行。
- 不为 ordinal 增加写路径事务或迁移；先接受读时 O(depth)。
- 不把 preview/assertion 错误做成新非数字错误码。
- 不把 DB 内 trigger/hash chain 宣传为可抵抗拥有 `.libra/libra.db` 写权限的攻击者。

---

## 0.5 v5 源码行号校正与 Agenta 深度复核（2026-06-28）

本节记录 v5 对 ~18 处 Libra 源码锚点的第三轮全量核验，以及对 Agenta DAO/service 的深度复核结论。

**Libra 侧行号校正**（符号不变，仅行号漂移）：

| 锚点 | v4 行号 | 实际行号 | 影响 |
|---|---|---|---|
| `merge_tree_items` | `merge.rs:1326` | **1332** | §5 P1-6、§6 B4 已改 |
| `create_tree_from_items_map` | `merge.rs:1486` | **1584**（偏移 ~98 行） | §5 P1-6 已改 |
| `reset.rs` `with_reflog` 调用 | `776-786` | **770-786** | §5 P1-4 已改 |
| `is_locked_revision` | `branch.rs:88` | **87** | §0.1、§0.3、§5 P0-2 已改 |
| `AddArgs` | `add.rs:70` | **67** | §5 P1-3 已改 |

**确认准确的关键锚点**（节选）：`collect_roots_from_database` 1260-1297 确不读 `operation_view*`；`with_operation_log` 317→535 跨度准确；`pointer_value = head_target.clone()` 在 689 准确；`split_revision_navigation` 739 只切 `~`/`^`；`get_commit_base_typed` 978；`resolve_commit_base_atom_typed` 836 tier 优先级准确；`lease_oid_matches` 1480；`validate_force_with_lease` 1496 读远端 OID；`incremental_objs` 2407；`StableErrorCode` 闭合枚举、无 `PRECONDITION/STAGE/REF/PUSH/TREE/DEPLOY` 域；`emit`/`emit_list` vs `write_json_command_envelope` 双信封；`expire_defaults_with_conn` 530（90/30 天）；rebase.rs 4227 行、`with_reflog` 1638/2064/2246；`revert.rs:1197` 经 `Branch::update_branch` 绕过 `with_reflog`；`sync_task_worktree_back` 1032-1116 调用 `try_merge_text_change`（1208）内的 `diffy::merge_bytes`（1238）；contract 校验三函数在 `workspace.rs` 不在 `merge.rs`。

**Agenta DAO 深度复核**（v5 新增证据，强化 §2.a）：

| 主题 | 深度复核结论 | 对本文的影响 |
|---|---|---|
| `commit_revision` 事务边界 | INSERT 在 T1 提交并释放 `FOR UPDATE` 锁 → `_get_version` 在 **T2 独立 session** COUNT → `_set_version` 在 **T3 独立 session** 无条件 UPDATE。三个独立事务、**无回滚边界**：T2/T3 失败则 revision 行已提交但 version 为 NULL/陈旧。 | §2.a "已知缺陷"须从"独立 session"升级为"三事务无回滚边界"；P0-1 借鉴"不可变"纪律的动机更强。 |
| `_set_version` 无 CAS | `UPDATE ... SET version=:version WHERE id=:revision_id`——无 `WHERE version IS NULL` 守卫、无 affected-row 检查、无条件覆写。 | 证实 Agenta 版本号不可信；Libra `<ref>@vN` 选择"不建表、读时计算"的正确性。 |
| `fork_variant` 成本 | 逐条 `commit_revision`，每条各自跑 3-session 往返（INSERT + COUNT + UPDATE）。fork 成本 = O(n) × 3 sessions × 3 网络 RTT。 | §2.a 已说 O(n)，补"× 3 sessions"精度；§4 表"Libra branch 已是 O(1)"对比更强。 |
| 环境事件发布 | `publish_revision_event` 在 DB commit **之后** best-effort 发布、**无 transactional outbox**——crash 在 commit 与 publish 之间静默丢事件。 | §2.a "历史即审计"须补注：审计事件非事务保证；P1-5 operation log 须做到"记录与 ref 写入同事务"以避免此缺陷。 |
| `is_guarded` 实际执行 | **OSS 版为 no-op**（`ensure_environment_deploy_allowed` 在 `not is_ee()` 时直接 return）；`DEPLOY_ENVIRONMENTS` 与 `EDIT_ENVIRONMENTS` 是**同级 sibling 权限**，在 DEVELOPER 角色同时授予，非严格层级。 | §5 P2-11 "受保护环境 infeasible" 判断进一步加强：Libra 无 EE/OSS 分层、无权限层级，不可移植此模式。 |
| 非初始提交无锁 | 仅 `initial=True` 路径有 `SELECT FOR UPDATE` + COUNT 守卫；非初始 `commit_revision` **无锁、无 expected-version 检查**。 | §2.a "并发安全的分支根"措辞过宽，须收窄为"仅初始提交有锁"。 |

> ⚠️ **v6 覆盖提示（2026-08-27）**：
> ① 本表「非初始提交无锁」一行**已被推翻**——Agenta 现已实现 revision 级 CAS（未传 CAS 参数的调用方才无锁），见 §0.6 第 7 条。
> ② 「`_set_version` 无 CAS」一行的**事实描述仍准确**（无条件 UPDATE、无 `WHERE version IS NULL`、无 affected-row 检查），但其「对本文的影响」栏推出的结论——「证实 Libra `<ref>@vN` 选择『不建表、读时计算』的正确性」——**已被现实否决**：Libra 实际交付的是物化 `revision_ordinal` 侧表 + 每读同事务 `ensure_fresh`，见 §0.6 第 2 条。
> ③ 其余各行经复核**仍然成立**（含三事务无回滚边界、fork O(n)×3-session、事件无 outbox、`is_guarded` OSS no-op），见 §0.6 第 9 条。
> §0.2 / §0.3 / §0.5 均保留为各自轮次的历史记录，**不就地改写**；当前口径一律以 **§0.6** 为准。

---

## 0.6 v6 刷新记录（2026-08-27，第六轮核验）

**基线**：Libra 工作树 `89081277a`（`Cargo.toml` version `0.21.27`；本 checkout 由 git 管理、无 `.libra/`，须用 `git rev-parse HEAD`）；
Agenta 快照 `/Volumes/Data/competition/agenta-ai/agenta`，revision `53717db55ec9311887be6fe86a67b2007590b6f3`（`main`，2026-08-21）。
**证据强度限制**：`plan-long.md:73,111` 的第九次竞品审计把 agenta 记为 `blocked-timeout`（`libra pull --ff-only` 连续 >90s 超时；本地 = `origin/main` 但未证明为远端最新），
故本节所有竞品结论以「该 revision 上的源码事实」为限，不宣称覆盖 Agenta 最新上游。（`plan-long.md:555` 另有一处「本轮工作区 clean、可更新」的前几轮陈旧措辞，本文不引用，也不在本次范围内代为修正。）**锚点取值帧（v6.1.1 补，与 §0 元信息同口径）**：本节 Libra 源码锚点对照 git HEAD `89081277a`；而 `plan-long.md`（`:73,111,200-201,520,555`）与 `lore.md` 两份文档带未提交改动，其行号取自 2026-08-27 工作树快照，HEAD 帧会整体上移（`plan-long.md` HEAD 仅 571 行、工作树 584 行；`lore.md` 工作树 `git diff --stat` = +98/−57），复核前请先确认帧。

**漂移量**：v5 的核验基线是 `v0.17.1759` / 2026-06-28；Libra 已跨 4 个 minor（0.17 → 0.21.27），竞品跨约两个月。
本轮复核 ~40 处锚点，其中 **2 处结论被推翻、1 处设计判断被现实否决、1 处竞品能力反转、~30 处行号/函数名漂移**。

### Libra 侧（结论级）

1. **P0-1 / A1 的 GC roots 半边已实现**，且实现手法与本文提案不同：`src/command/maintenance.rs::collect_registered_store_roots`（**3308-3441**）以 `(table, sql, columns, CellMode)` 表驱动收集 roots——
   `operation_view_ref.target_oid` 为 `CellMode::StrictOid`（非法值 fail-closed 报 `RepoCorrupt`，`maintenance.rs:3414-3422`），
   `operation_view.head_target` 与 `operation_view_workspace.pointer_value` 为 `CellMode::OidIfParses`（`parse_object_hash` 失败即静默跳过，`maintenance.rs:3424-3428`）。
   这以更简洁的方式解决了 §5 P0-1「风险/注意」第一条指出的「分支名喂进 `parse_stored_hash` 会触发 `RepoCorrupt` 让 GC 中止」问题：**无需 `WHERE head_kind='detached'`，也无需丢弃 workspace 表**。
   三处均已登记入 `GC_OBJECT_SOURCE_INVENTORY`（`maintenance.rs:3081-3090,3180-3190,3191-3200`），并由 `tests/db_migration_test.rs:3693-3700` 的「OID 形列必须被账本覆盖」守卫钉住。
   **顶部 v0.17.1759 校正须知中「`maintenance.rs::run_gc` … 不含 operation_view roots」的表述随之作废**（已就地更正）。
   A1 的另一半（`op restore` 缺对象前置 fail-closed）**仍未实现**——`src/command/op.rs::handle_op_restore`（**387**）有 5 道前置守卫，但全文无任何对象存在性校验；已改由 `plan-20260822` **OL-10** 承接。

2. **P0-2 / A2 的能力已交付，但本文的设计判断被推翻**：Libra 走的是 `lore.md §1.16`（对标 Lore 的 `revision find number`）而非本文的 Agenta 路线——
   `revision_ordinal` / `revision_ordinal_meta` **物化侧表** + 迁移 **`2026070301`**（`src/internal/db/migration.rs:981-984`；`docs/development/gap/lore.md` 的 **§1.16「revision ordinal index ✅ 已落地」整行**——迁移 2026070301 / `ensure_fresh` 指纹 / 1..N 编号，交付索引另见该文件「Lore 能力交付索引」表的 `revision metadata/find number/find metadata` 行；**v6.1.1 帧口径更正**：v6.1 改出的 `:228`/`:606` 两帧皆不成立，需数字时按帧标注——HEAD `89081277a` = `lore.md:210` / `:581`，该文件含未提交改动、工作树帧另行漂移）+ 每次读在同一事务内 `ensure_fresh`（指纹 = tip OID + `refs/replace` 摘要；快进 APPEND 不重编号，重写/replace 变更全量重建）；
   命令面为 `libra revision find -n | number <commitish> | index [--rebuild]`（`src/command/revision.rs:54-81`），`--json` 输出含 `ordinal`/`total`；`COMPATIBILITY.md:124` 已整行标为 intentionally-different。
   GC 侧把两表登记为 `IndexOnly`（可重建，永不作 anchor）。
   故 §5 P0-2「**不要建物化表、不做迁移**」的建议**已被现实否决**——否决理由是 lore 的 `ensure_fresh` 指纹方案在同事务内消解了「缓存说谎」风险，而 §5 P0-2「性能注记」担心的 O(depth) 热路径成本正是建表要解决的问题。按 §8 治理规则**保留条目、改标「已被替代」**，不删除。
   **仍是缺口的只剩 `<ref>@vN` 这一 revspec 形态**：`src/utils/util.rs` 全文零 `ordinal` 命中；`first_revision_operator`（**util.rs:1950-1954**）只把 `^`、`~`、`@{`（必须跟 `{`）当运算符，裸 `@` 不切分，故 `main@v3` 会被当作整体 atom 解析并失败。
   ⚠️ **口径冲突（若将来仍要做 `@vN` revspec 必须先解决）**：本文提案是 `@v0 = 根`（0 起），而已交付的 `libra revision` 是 **1 起（1 = root）**。必须对齐为 **1-based**，否则同一仓库出现两套版本语义——这正是 §6.0.1 最后一条想避免的情形。

3. **§3 与 §7 的「分支严格胜过同名 tag」是错的**：解析顺序已反转为 `HEAD → tag → 本地分支 → 远程跟踪 → OID 前缀`，
   源码内有明确注释 `// Git's short-ref precedence checks tags before local and remote branches.`（**`src/utils/util.rs:1797`**，顺序为 tag 1798 → 本地分支 1801 → 远程跟踪 1804 → hash 1806），`get_commit_base` 的 doc 注释（**util.rs:2145-2152**）也已写成 `1. HEAD 2. Tag 3. Local branch 4. Remote-tracking 5. Commit hash prefix`。
   同时 `resolve_commit_base_atom_typed` 与 `split_revision_navigation` **两个函数均已不存在**，revspec 解析器整体重构（见本节末锚点表）。
   §7 第一条的 `already-implemented` 判定与「多段远程跟踪名首匹配静默选取」这一残值**仍成立**（现落在 `resolve_remote_branch_atom_typed`，**util.rs:1684-1712**，经 `remote_tracking_candidates`（**util.rs:1538**）顺序取首个命中）。

4. **`libra gc` 命令不存在**：GC 的唯一入口是 **`libra maintenance run --task gc`**（`MaintenanceTask::Gc`，`maintenance.rs:146,221,271`；命令面见 `COMPATIBILITY.md:195` 的 `| maintenance | partial |` 行）。
   源码中**无** `DEFAULT_PRUNE` 常量、无 `"2.weeks.ago"`；现行防误删是 **`PRUNE_GRACE_SECS = 3600`**（1 小时 mtime 宽限，`maintenance.rs:698`）+ `.libra/gc-prune-candidates.json` **两趟隔离账本**（带文件锁，`maintenance.rs:748` 起——首次被判不可达的对象不删）。A1 的验收标准与测试命令据此重写。
   reflog 的 90/30 天默认**仍成立**（`expire_defaults_with_conn`，**`src/internal/reflog.rs:633`**，v5 记 530）。

5. **仍是真实缺口（本轮 grep 确认全零命中）**：`commit --assert-staged`、`--expect-head`/`--expect-branch`、`--ref-assert`、push 丢路径预检（`dropped_paths`/`--allow-deleted-paths`/`push.guardDroppedPaths`）、
   `op restore --json`（dry-run 仍走 `println!`，`src/command/op.rs:511-541`）、`operation` 表 append-only 触发器、`libra env`/`libra promote`/`@{deploy:}`、`merge_commits` 提取、`src/command/fork.rs`。
   `LBR-STAGE-*` / `LBR-REF-*` / `LBR-PUSH-*` / `LBR-DEPLOY-*` 四个域**仍不存在**（`src/utils/error.rs` 的 `StableErrorCode` 闭合枚举现共 **56** 个变体，`error.rs:190-343`，`as_str` 一一映射 56 条 `LBR-*` 字符串；文档面 `docs/error-codes.md` 列 **71** 行码——差额是其它模块自带的非 `StableErrorCode` 码。**v6.1 复核订正：此处 v6 原写「110 个码」，任何度量都得不到该数字，属编造，已改**）；`StableErrorCode::RepoStateInvalid = "LBR-REPO-003"` 存在（`error.rs:196,364`），A1 的错误码前提成立。
   §0.3 的两条实现约束**经复核仍完全成立**：`changes_to_be_committed_safe()` 仍在内部再次 `Index::load`（**`status.rs:6412-6418`**）；commit 的 reflog `old_oid` 仍由 `new_reflog_context`（**`commit.rs:3036-3042`**）在 `with_reflog` 事务**之外**用 `Head::current_commit()` 读取。

6. **`with_operation_log` 接线从 2 处增至 3 处**：`src/command/branch.rs:519`（branch **reset**，v5 后新增）、`branch.rs:1521`（branch create）、`src/command/op.rs:596`（op restore）。
   **branch delete 仍未覆盖**，`with_reflog_and_operation` 组合封装**仍不存在**——§5 P1-5 / §6 B3 的核心缺口判断成立。

### 与日期计划的承接关系（v6 新增，避免与 `plan-20260822` 形成两条平行排期）

`plan-20260822.md`（LR-02 Operation Log v2 + LR-03 Change ID，`plan-long.md:200-201` 记为 **已排期**；LR-01 为 **实施中**）全文零次提及本 gap 文档，本文此前也零次提及它。下表固定承接关系：

| 本文条目 | 承接方 | 证据 |
|---|---|---|
| P0-1 / A1 前半（GC roots） | **已实现**（非计划承接，直接落地于 `maintenance.rs`） | 本节第 1 条 |
| P0-1 / A1 后半（`op restore` 缺对象 fail-closed） | **OL-10** RestoreEngine（验收明列 `operation_restore_faults` 覆盖「**对象缺失**」） | `plan-20260822.md:1197-1215` |
| P1-5 / B3（全命令 operation view） | **OL-08 / OL-09**（`operation/middleware.rs` 的 `MutationClass` 七类穷举 + `classify_command` + `run_with_operation`：pre-snapshot → lease → CAS 发布，**未知 mutation fail closed**；OL-09 负责 CLI 与 Agent tool 接入） | `plan-20260822.md:1087-1196`；GAP-02（:77） |
| P2-9 / C3（`op restore --dry-run` + JSON preview） | **OL-10**（`--dry-run` 与 JSON/machine receipt + 「机器接口冻结：receipt 字段与退出码有契约测试」） | `plan-20260822.md:1206-1215` |
| P2-10 / C4（operation append-only） | **ADR-OL-07**（Undo 为追加式显式状态变换，不修改历史 Operation）+ 仓库既有 append-only trigger 先例 | `plan-20260822.md:178-186`；`sql/migrations/2026080401_agent_capture_workspace_scope.sql:198-207` |
| P0-2 / A2（`<ref>@vN`） | **已被 `lore.md §1.16` / `libra revision` 替代**（不同设计，非本计划承接） | 本节第 2 条 |
| P1-3 / B1、P1-4 / B2、P2-7 / C1、P2-8 / C2、P2-11 / D1 | **无承接**，仍是本文独有的开放提案 | 本节第 5 条 |

**与 `plan-long` 的边界对齐**：`plan-long.md:520` 明列不采纳「复制 Agenta 的 prompt/workflow 应用版本平台」，:555 明列「不把 Agenta 当源码 VCS 对标」。
本文 §5 P2-11 / §6 D1 最接近这条红线，已在该卡内加显式对齐句（借鉴的是 environment-as-pointer 的**指针语义 + CAS 纪律**，不引入 Artifact/Variant/Revision 关系模型）。本文全部条目落在 A 类版本管理，与 `MEM-*` / `SB-*` / `AG-ATTR` 无交集。

### 竞品侧（结论级）

7. **Agenta 已实现 revision 级 compare-and-set，本文「Agenta 无 CAS」的结论对其 git 内核不再成立**：
   `commit_revision`（`dao.py:1607`）新增 `expected_head_revision_id: Optional[UUID]` 与 `no_change_check: Optional[Callable]` 两个参数；
   `needs_lock = (initial or expected_head_revision_id is not None or no_change_check is not None) and variant_id`（**1663-1671**）；
   加锁前先 `SET LOCAL lock_timeout`（**1677-1681**，有界等待）；锁内 `SELECT ... FOR UPDATE`（**1683-1691**），锁到空行/跨项目行抛 `VariantNotFound`（**1695-1696**）；
   锁内**重读 head 并精确比较**（含「head 不存在」也算不符），不符抛 `RevisionConflict`（**1713-1745**）；`no_change_check` 刻意排在 expected-head 之后（**1746-1774**，注释：陈旧调用方只能拿到冲突，永远拿不到 no-change）。
   真实调用方：`api/oss/src/core/workflows/service.py:2193` 传 `expected_head_revision_id=workflow_revision_commit.base_revision_id`，并把 `RevisionConflict` 翻成带 `base_revision_id`/`current_revision_id` 的结构化 409（2199-2203）；接口声明在 `core/git/interfaces.py:300-302`，异常类型在 `core/git/types.py:133,152,171,188`（`RevisionConflict` 同时携带 expected 与 current 两个 id，让调用方一步内重读重试）。
   新增测试锚点：`api/oss/tests/pytest/unit/git/test_commit_revision_lock.py`、`test_commit_revision_race.py`、`test_commit_lock_scope.py`。
   **这与本文 §5 P1-4 `--expect-head` 的设计几乎同构（锁内重读、精确比较、错误同时回传 expected 与 actual），已从「Agenta 的坑」改列为 P1-4 的外部正面佐证。**
   相应地，§2.a「并发安全的分支根」与 §0.5「非初始提交无锁」收窄为：**未传 CAS 参数的调用方**（含 `fork_variant` 与 environments 全部路径）仍无锁、无 CAS。

8. **异常 → HTTP 注册表从 5 类扩到 8 类**（`api/oss/src/apis/fastapi/git/exceptions.py`，8 个 `except` 臂在 107-121）：新增 `RevisionConflict→409`、`CommitLockTimeout→**503**`、`VariantNotFound→**404**`。
   其中 **503 区分出「锁竞争超时」与「状态冲突」两个机器可分支类别**——Libra 若把 CAS 失败一律压进 `LBR-CONFLICT-002` 会丢掉这一维度；该取舍已记入 §5 P1-4（MVP 仍复用 `LBR-CONFLICT-002` + detail key 区分，但取舍写明）。

9. **复核后仍然成立、无需改动的竞品结论**（逐条实测）：
   `_get_version`（`dao.py:1961`）/ `_set_version`（**1985**）各自 `async with self.engine.session()` 独立事务，`_set_version` 是无条件 `UPDATE ... SET version`——**无 `WHERE version IS NULL`、无 affected-row 检查**；且 INSERT 的 `session.commit()` 在 **1778**，两者在其**后**调用（**1801-1812**），确实跨出了锁与事务 ⇒ §2.a「三事务无回滚边界」完全成立；
   `fork_variant`（**914-1032**）仍是逐条 `commit_revision` 深拷贝，且这些调用**不传** `initial`/`expected_head`/`no_change_check` ⇒ `needs_lock=False` ⇒ 每条仍是 3-session 往返（**唯一可补的新事实**：fork 现在经 `RevisionsLog(depth=artifact_fork.depth)` 支持深度截断，**932-938**，即 O(n) 可被调用方限界）；
   `publish_revision_event` 仍在 DB commit **之后** best-effort 调用、无 transactional outbox（`environments/service.py:1144`）；
   `is_guarded` 在 OSS 版仍是 no-op（`ensure_environment_deploy_allowed` 首行 `if not is_ee() or not environment_id: return`，`api/oss/src/apis/fastapi/environments/utils.py:41-49`）；
   **environments 的 delta 路径仍未启用已存在的 CAS 能力**（`commit_environment_revision`，`environments/service.py:1052`，调用 DAO 时只传 `initial=initial`，**1106-1113**）；
   `Reference(id, slug, version)` 解析代数、`is_identifying`、规则 2.a–2.e docstring 与 `applied_identifying_filter` 守卫全部仍在；`test_variant_ref_version_only_400.py` 仍在，且**仅**在 `api/oss/tests/pytest/acceptance/`（全库 `find . -name "*version_only*"` 只此一命中）；`unit/git/` 下**无**同名文件——该目录现有 `__init__.py`、`conftest.py`、`test_commit_lock_scope.py`、`test_commit_revision_lock.py`、`test_commit_revision_race.py`、`test_commit_stores_data.py`、`test_retrieval_info_utils.py`，其中前三个是本节第 7 条 CAS 的新增测试（**v6.1 复核订正：v6 原写「两处各有一份」，`unit/git/` 那一份不存在，属编造，已改**）；
   §2.b 的 GitButler 反例锚点均在（agenta checkout 仍无 `.git`、由 `.libra/` 管理；`.better-commits.json` 与 `.husky/{post-checkout,pre-commit,pre-push}` 齐全；AGENTS.md 仍写「唯一可靠恢复是 `but oplog restore`」并记录 series 坍缩，**102/155-160/166**）。

### 待复核（v6 未关闭，**不要当结论使用**）

- **OL-02 与现行 GC roots 的潜在冲突**：`ADR-OL-01` / `OL-02` 明确 v2 直接替换 v1，验收含「v1 五张表移除」并要求 `rg 'operation_view' src/internal/db.rs src/internal/model` 零命中（**`plan-20260822.md:759-800` 的 `### Task OL-02` 卡**：Description 的 v1 五表清单在 :765、「v1 五张表移除」验收在 :773、Verification 的 `rg` 零命中在 :784、`Implementation write set` 在 :789；替换原则本身见 **`ADR-OL-01`（:124-132）**，:586-608 只是 waiver 表 + 粒度审计表里的 OL-02 汇总行）。
  但本节第 1 条已落地的 GC roots **正依赖** `operation_view` / `operation_view_ref` / `operation_view_workspace` 三张表，且 `tests/db_migration_test.rs:3693-3700` 把其中两列**硬编码**为必须出现在 `GC_OBJECT_SOURCE_INVENTORY` 的守卫；
  而 OL-02 的 `Implementation write set`（`src/internal/db.rs`、`sql/**`、`src/internal/model/`、导入脚本）**未列** `src/command/maintenance.rs`、`GC_OBJECT_SOURCE_INVENTORY` 或 `tests/db_migration_test.rs`。
  ⇒ OL-02 移除 v1 五表时，须同步把 GC roots 迁到 v2（`RepoViewV2`/`WorkspaceSnapshotV2` 闭包）并更新账本与守卫；**此项是否已在 OL-02/OL-03 写集内未经计划 owner 确认**。本文不代 `plan-20260822` 改任务卡。
- **`rebase.rs:5193` 的 `create_tree_from_items_map` 重复实现**：与 `merge.rs:3376` 同名，`merge.rs` 版已是 `pub(crate)` 且被 `stash.rs` 复用。B4 提取时是否应一并收敛这两份，未评估。
- **`push.rs` 的 `incremental_objs` 锚点**：定义在 **2699**，调用点 **2052 / 2094**（v5 记的 2407 已失效）；C2 究竟挂在哪个调用点更合适，未评估。
- **`docs/commands/op.md` 全文零 `gc` 提及**：GC roots 已保护 operation view 这一事实未进用户文档，是一个可顺手补的文档缺口（不在本次刷新范围内）。

### v6 锚点漂移表（直接替换用）

| 本文引用 | 现在位置（2026-08-27 实测） |
|---|---|
| `src/command/gc.rs`（任何行号） | 已删；现行 GC = `src/command/maintenance.rs`（5062 行），`run_gc` **591** |
| `collect_roots_from_database`（`gc.rs:1260-1297`）/ `agent_checkpoint_roots`（`gc.rs:1709`） | `collect_reachable_objects`（**2289**）+ `collect_registered_store_roots`（**3308-3441**，表驱动，同时覆盖 `notes`/`operation_view*`/`agent_checkpoint`/`agent_coverage_claim`/`agent_session`/`workspace_record`/`agent_bridge_checkpoint`）；分型账本 `GC_OBJECT_SOURCE_INVENTORY`（**2871** 起） |
| `src/internal/operation.rs:501-683` | 文件 1616 行；view 持久化在 **633-960**；schema DDL 在 **1426-1481** |
| `src/internal/operation_wrapper.rs:317-535` | 文件 2132 行；`with_operation_log` **405**、`with_operation_log_with_conn` 定义 **425**（**419** 是 `with_operation_log` 内的调用点，非定义）、`collect_final_view_with_conn` **1444**、`pointer_value = head_target.clone()` **1509** |
| `src/utils/util.rs:739-990`（`split_revision_navigation:739`、`resolve_commit_base_atom_typed:836`、`get_commit_base_typed:978`、tier 注释 `993-1001`、多段远程 `862-890`） | 文件 4228 行；两个函数**均已不存在**。现为 `resolve_tag_atom_typed` **1633** / `resolve_local_branch_atom_typed` **1662** / `resolve_remote_branch_atom_typed` **1684** / `resolve_hash_atom_typed` **1716** / `resolve_object_atom_typed` **1757** / `resolve_reflog_selector_typed` **1925** / `first_revision_operator` **1950** / `parse_decimal_prefix` **1956** / `resolve_revision_expression_typed` **1974** / `resolve_tree_path_typed` **2069** / `resolve_object_spec_typed` **2124** / `get_commit_base_typed` **2141**；tier 注释 **2145-2152**；多段远程切分 **1684-1712**（`remote_tracking_candidates` **1538**） |
| `src/command/commit.rs:562-611,1899-1915,1921` | 文件 3765 行；`CommitArgs.dry_run` **165**、`run_commit` **923**、`run_commit_with_index` **995**、`Index::load` **1038/1049**、`auto_stage_tracked_changes` **1043**、`changes_to_be_committed_safe()` 调用 **1077**、`create_tree_with_persistence` **1166**（原 `create_tree`）、`update_head_and_reflog` **3018**、`new_reflog_context` **3036**（`old_oid` 于 **3040** 事务外读取） |
| `src/command/push.rs:1478-1525`（`lease_oid_matches:1480`、`validate_force_with_lease:1496`、`incremental_objs:2407`） | 文件 4019 行；`lease_oid_matches` **1685**、`validate_force_with_lease` **1701**、`incremental_objs` **2699**（调用点 2052/2094） |
| `src/command/reset.rs:770-790` | 文件 2566 行；`with_reflog` 调用 **1560** |
| `src/command/merge.rs:657-687`、`merge_tree_items:1332`、`create_tree_from_items_map:1584` | 文件 3987 行；`perform_three_way_merge` **1876**、`merge_tree_items` **3045**（**仍是私有 `fn`**）、`create_tree_from_items_map` **3376**（**已提升为 `pub(crate)`**，被 `stash.rs:35,832,834` 复用）、`reset_index_and_workdir_to_tree` **3389** |
| `src/internal/ai/orchestrator/workspace.rs:1032-1115`、`try_merge_text_change:1208`、`diffy::merge_bytes:1238` | `sync_task_worktree_back` **1041**、`try_merge_text_change` **1287**、`diffy::merge_bytes` **1317**（文本级合并仍在，P1-6 动机成立） |
| `rebase.rs` 4227 行 / `with_reflog` 1638/2064/2246 | **5539 行**；`with_reflog` **2588 / 3118 / 3459** |
| `revert.rs:1197` 经 `Branch::update_branch` 绕过 `with_reflog` | **`revert.rs:1487`**（文件 1650 行） |
| `reflog.rs:530-535` `expire_defaults_with_conn`（90/30） | **`reflog.rs:633`**，90/30 天默认不变 |
| `status.rs:2001` `changes_to_be_committed_safe` | **`status.rs:6412`**（文件 7398 行），内部二次 `Index::load` 行为不变 |
| `branch.rs:87` `is_locked_revision` | **`src/internal/branch.rs:87`**（行号准确，但须补 `internal/` 前缀——`src/command/branch.rs:87` 不是它）；`is_locked_branch` **`internal/branch.rs:60`** |
| `add.rs:67` `AddArgs` | **`add.rs:71`** |
| `branch.rs:111-118` `libra branch <name> <rev>`（§7） | `create_branch_impl` **`src/command/branch.rs:1453`**、`create_branch` **2343**、`create_branch_safe` **2354** |
| `with_operation_log` 接线 2 处（`branch.rs:979` + `op.rs:447`） | **3 处**：`branch.rs:519`（reset）、`branch.rs:1521`（create）、`op.rs:596`（restore）；branch delete 仍未覆盖 |

> **锚点纪律重申**（§0 已有，本轮再次被验证）：`file:line` 只是提示，落地前一律按**函数/符号名**重新定位。本轮 `rebase.rs` 从 4227 行增至 5539 行、`util.rs` 解析器整体改名，即为例证。

### v6.1 复核订正（2026-08-27，对抗式复核后二次落笔）

v6 成稿后由独立复核员逐条抽查，报出 9 项；本节记录**经我方逐条实测复验后实际执行的订正**（每条均附本次实测命令/证据，未复验通过的不改）。本次仅订正事实与锚点，**不新增/删除/重编任何条目编号**（§8.2 治理规则）；因无结论翻转、无状态列变更，按 §8.2 第 2 条**不 bump 文档主版本**，以 v6.1 记入本节。
**v6.1.1 P2 精度订正小结（同日第三次落笔，仅 2 条，均为「脏工作树 vs HEAD」同一根因）**：① 下表第 7 行的 `lore.md:228`/`:606` 系按未提交工作树帧取值，**两帧皆不成立**——HEAD `89081277a` 的 `:210` 恰是 §1.16 整行（非空行）、`:581` 是交付索引行，当前工作树已漂到 `:235`/`:615`；§0.6 第 2 条、§1 第 3 条与下表第 7 行三处已一律改按**符号**引用，第 7 行同时按 §8 治理规则**保留原文并标注推翻**，不删除。② 文档声明的核验基线是 git HEAD `89081277a`，但 `plan-long.md` 与 `lore.md` 两份被引文档处于 ` M` 未提交状态，文中对它们的行号（乃至 `plan-long.md:73,111` 的 agenta `blocked-timeout` 这条事实本身）只在工作树帧成立；已在 **§0 元信息**与 **§0.6 基线段**各就地补一句「锚点取值帧」声明并给出 HEAD 帧对照值。两条均**不影响任何结论、状态列或编号治理**；本次订正全部为行内改写，未新增/删除条目编号。

| # | 类别 | 订正内容 | 落点 | 实测证据 |
|---|---|---|---|---|
| 1 | **编造事实** | 「`src/utils/error.rs` 现共 **110** 个码」→ **56**（`StableErrorCode` 闭合枚举 `error.rs:190-343` 共 56 个变体；`as_str` 一一映射 56 条 `LBR-*`；文档面 `docs/error-codes.md` 列 71 行码） | §0.6 第 5 条、§3 | `awk '/pub enum StableErrorCode \{/,/^\}/' src/utils/error.rs \| grep -cE '^\s{4}[A-Z][A-Za-z0-9]*,'` → 56；`grep -oE '"LBR-[A-Z]+-[0-9]+"' src/utils/error.rs \| sort -u \| wc -l` → 56；`grep -c '^\| \`LBR-' docs/error-codes.md` → 71 |
| 2 | **编造事实** | 「`test_variant_ref_version_only_400.py` 在 acceptance 与 `unit/git/` **各有一份**」→ 全库**只有** acceptance 一份；`unit/git/` 无同名文件 | §0.6 第 9 条 | 竞品仓库 `find . -name "*version_only*"` 只 1 命中；`ls api/oss/tests/pytest/unit/git/` = `__init__.py`/`conftest.py`/`test_commit_lock_scope.py`/`test_commit_revision_lock.py`/`test_commit_revision_race.py`/`test_commit_stores_data.py`/`test_retrieval_info_utils.py` |
| 3 | **顶层结论未同步** | §1 第 2/3 条仍以现状口吻陈述两条本轮自己推翻的事实（对象会被 GC 回收 / 缺稳定 ordinal 句柄），且写「4 个写入点」 | §1 | 已各补一条 v6 覆盖提示框指向 §0.6 第 1/2 条；写入点订正为 **3 处**（`grep -rn "with_operation_log" src/command/` = `branch.rs:519` / `branch.rs:1521` / `op.rs:596`，与 §0.6 第 6 条、§4 表一致） |
| 4 | **入口口径矛盾** | §0 头部「优先落成 4 个 tracer bullet」与 §6 开篇「默认顺序 A1 → A2 → B1 → B2 → B3 → B4」把已实现/已替代/已承接的卡排在最前，与 §6.0 状态列相反 | §0、§6 | 已分别改为「仅剩 B1、B2 两个可落地 tracer bullet」与「默认顺序 B1 → B2 → B4」，并注明 A1/A2/B3 不再排期（编号与卡体保留） |
| 5 | 评级误读 | 「`COMPATIBILITY.md:153` 列为 supported」→ 该行四级评级是 **`partial`**，只是正文明列 numeric reflog selector 已支持 | §3 | `sed -n '153p' COMPATIBILITY.md` = `\| rev-parse \| partial \| …` |
| 6 | 锚点指错卡 | OL-02 锚点 `plan-20260822.md:124-132,586-608` → **`:759-800` 的 `### Task OL-02` 卡**（:765 v1 五表清单、:773 移除验收、:784 `rg` 零命中、:789 write set）；`:124-132` 实为 `ADR-OL-01`，`:586-608` 是 waiver + 粒度审计表 | §0.6 待复核第 1 条 | `grep -n '### Task OL-02' docs/development/plan/plan-20260822.md` → 759；`sed -n '124p'` → `### ADR-OL-01: …`。**所引述的三项内容本身经复核全部准确**（OL-02 write set 确未列 `maintenance.rs` / `GC_OBJECT_SOURCE_INVENTORY` / `db_migration_test.rs`），故该「待复核」结论不变，仅换锚点 |
| 7 | 锚点漂移 → **帧口径更正（v6.1.1 推翻本行 v6.1 结论）** | ~~`lore.md:210` → `lore.md:228`（交付索引 `:606`）~~ 该订正取自当日**未提交工作树**帧，`:228`/`:606` 在 HEAD 帧与当前工作树帧**均不成立**；三处引用（§0.6 第 2 条、§1 第 3 条、本行）一律改按**符号**引用——§1.16「revision ordinal index ✅ 已落地」整行 + 「Lore 能力交付索引」表的 `revision metadata/find number/find metadata` 行；需数字时标帧：HEAD `89081277a` = `:210` / `:581` | §0.6 第 2 条、§1 第 3 条、§0 元信息帧声明 | `git show HEAD:docs/development/gap/lore.md \| grep -n 'revision ordinal index'` → **210**（故 v6.1 所称「`sed -n '210p'` 为空行」不成立），交付索引 → **581**；当前工作树（该文件 ` M`，`git diff --stat` = +98/−57）→ **235** / **615**；工作树 `228` 行实为 `1.9 log --trailer` 那行、`606` 行是表格分隔符 |
| 8 | 锚点漂移 | ① `_with_conn` **419** → `with_operation_log_with_conn` 定义 **425**（419 是调用点）；② commit dry-run 分支 **1030** → **1033**（1030 实为 `let signing_policy = …`；1065/1103 准确）；③ `VariantNotFound` **1699-1700** → **1695-1696** | §0 source 依据、§0.6 漂移表、§5 P1-3、§0.6 第 7 条 | `grep -n 'fn with_operation_log' src/internal/operation_wrapper.rs` → 405/425；`sed -n '1030,1033p' src/command/commit.rs`；`sed -n '1695,1696p' <agenta>/api/oss/src/dbs/postgres/git/dao.py` |
| 9 | 内部不一致 | §3「五表」只列出四个表名 → 补全 `operation_parent` | §3 | `plan-20260822.md:765` OL-02 卡的 v1 表清单 = `operation/operation_parent/operation_view/operation_view_ref/operation_view_workspace`，与 §0.6 待复核第 1 条的引用一致 |

**遗留待复核（本轮未关闭，沿用上文「待复核」节，不得当结论使用）**：
1. OL-02 移除 v1 五表时是否已把 GC roots 迁移纳入写集——**仍未经 `plan-20260822` owner 确认**（锚点已订正为 `:759-800`，结论不变）。
2. `rebase.rs:5193` 与 `merge.rs:3376` 两份 `create_tree_from_items_map` 是否应在 B4 一并收敛——未评估。
3. `push.rs` 的 `incremental_objs`（定义 **2699**，调用点 **2052 / 2094**）中 C2 该挂哪个调用点——未评估。
4. `docs/commands/op.md` 全文零 `gc` 提及——文档缺口，不在本次刷新范围。
5. 本轮复核员另称 `docs/error-codes.md` 为「71 行 / 79 个唯一码」，实测为 **467 行 / 71 个唯一码**，故本节采用实测值 71；该差异已在上表第 1 行注明。

---

## 1. 一句话结论

**Agenta 的版本管理本质是“把 Git 搬进关系型数据库的一个三层不可变内核”**——Artifact（仓库）→ Variant（分支）→ Revision（不可变提交），用单一 `Reference(id, slug, version)` 值对象 + 一套纯函数解析代数（充分性 / 冗余一致性 / 不一致三态校验）寻址任意一次提交，并把 environment 建模成指向 revision 的可移动指针。它不是真正的 DAG/merge 系统，而是“快照 + 指针 + 强约定 + 机器可读错误”的工程化产物。

对 Libra 最有价值的 3 个借鉴点：

1. **声明—校验契约（declare-then-verify）**：让 agent 主动多写它相信的状态（HEAD/staged/refs），VCS 在不一致时报出“哪个字段对不上”的带类型错误，而不是静默尽力解析。这正是 AI agent 跨轮次持有陈旧信念时最需要的护栏，且能直接套到 Libra 的 `commit --assert-staged`、`--expect-head` 等 CAS 前置条件上。
2. **不可变历史 = 审计日志**：Agenta 每次部署都 append 一条完整 `environment_revision`，历史本身即审计。Libra 已有 jj 式 operation log，但它只有 **3 个写入点**（`branch.rs:519` reset / `branch.rs:1521` create / `op.rs:596` restore，branch delete 未覆盖）——把“不可变可恢复”这个保证补齐，是当前最高性价比的完整性修复。
   > **v6 覆盖提示（v6.1 补记入本节——v6 只改了 §0.6/§3/§4，遗漏了此处）**：本条原文另称「目标对象会被 GC 回收」，**已被本轮自己推翻**——`operation_view` / `operation_view_ref` / `operation_view_workspace` 三表已是 traced GC root（`maintenance.rs:3308-3441` + 账本 3081-3090/3180-3190/3191-3200 + `tests/db_migration_test.rs:3693-3700` 守卫），见 **§0.6 第 1 条**。剩余缺口只有两项：`op restore` 对缺失对象未 fail-closed（**OL-10** 承接）与全命令覆盖（**OL-08/OL-09** 承接）。原文的「4 个写入点」也是错的，实测 3 处（§0.6 第 6 条），已订正。
3. **稳定、可读、机器可寻址的句柄**：Agenta 的 per-variant 单调版本号给了“分支内第 N 次提交”一个稳定名字。
   > **v6 覆盖提示（v6.1 补记入本节）**：本条原文称 Libra「只有不透明 OID 和会随 tip 前移而改变的 `~N`，缺一个从根计数的稳定句柄」，**已被本轮自己推翻**——稳定 ordinal 句柄已由 `libra revision find -n | number | index`（**1 起**，1 = root；`src/command/revision.rs:54-81`、`COMPATIBILITY.md:124`、`lore.md` §1.16 整行——HEAD `89081277a` 帧为 `:210`，见 §0 元信息的锚点取值帧声明）交付，见 **§0.6 第 2 条**。**仍缺的只是 `<ref>@vN` 这一 revspec 形态**，且若要补必须复用已交付的 1-based 语义（§6.0.1 最后一条）。

---

## 2. Agenta 版本管理方案剖析

Agenta 有两套完全不同的版本管理，必须分开看。

### 2.a 配置工件的“类 Git 版本内核”（core/git）

这是 Agenta 真正自研的版本系统，用于六类实体（workflows / applications / evaluators / testsets / queries / environments），由一个泛型 `GitDAO` 实现。

**三元组数据模型（`api/oss/src/core/git/dtos.py`）**
- **Artifact** = 版本容器（仓库）：Identifier(id)+Slug+Lifecycle+Header+Metadata+FolderScope。
- **Variant** = 分支：Identifier+Slug+...，回指 artifact_id；**注意它没有 version 字段**。
- **Revision** = 不可变提交：Identifier+Slug+**Version**+Commit(author/date/message)+data 载荷，并冗余携带 artifact/variant 的 id+slug，使一次拉取自描述完整血缘。
- id（UUIDv7）与 slug（项目内唯一，正则 `^[a-zA-Z0-9_\-][...]*$`）在三层都项目唯一。

**Reference(id, slug, version) 解析代数（`core/git/types.py`）**
- `is_identifying(ref)`：携带 id 或 slug 才“可识别”；**裸 version 不可识别**——它只是分支内序号，离开分支作用域无意义。
- 解析规则 2.a–2.e（写在模块 docstring）：revision.id/slug → 该提交；variant → 该分支最新提交（tie-break `created_at DESC, id DESC LIMIT 1`）；artifact → 默认 variant（最老的，`created_at ASC, id ASC`）的最新提交；variant + revision.version → 该分支指定版本。DAO 用 `applied_identifying_filter` 守卫，**拒绝执行无作用域的 `WHERE project_id LIMIT 1`**，绝不返回任意行。
- 三态纯函数校验，DB 访问前先跑：
  - `validate_*_sufficient` → 欠定（如只给 version）抛 `RetrieveRefsInsufficient`（HTTP 400）。
  - `validate_retrieve_refs_consistent` → 允许冗余多写，但每个冗余标识符必须命中解析出的那一行；不一致抛 `RetrieveRefsInconsistent` 并**点名出错字段**（连“用 id 查时本应忽略的 version”对不上也算矛盾）。

**版本号 = per-variant 单调序号（核心不变量）**
- `_get_version`（`api/oss/src/dbs/postgres/git/dao.py:**1961**`，v5 记 1802）= `COUNT(同 variant 内 id < 本 revision.id 的行)`，0 起始字符串存回。version '0' 是分支空根（data/flags/tags/meta 置空）。
- **已知缺陷（v5 深度复核升级）**：`commit_revision` 的 INSERT 在 T1 提交并释放 `FOR UPDATE` 锁 → `_get_version` 在 **T2 独立 session** COUNT → `_set_version` 在 **T3 独立 session** 无条件 UPDATE——三个独立事务、**无回滚边界**：T2/T3 失败则 revision 行已提交但 version 为 NULL/陈旧。`_set_version` 无 `WHERE version IS NULL` 守卫、无 affected-row 检查、无条件覆写。非初始提交不加锁。产线上出现过重复/缺失版本号，需要专门修复迁移 `b3c4d5e6f7a9`。这是 Agenta 自己踩的坑。

**不可变 + fork 语义**
- commit = INSERT 新行；`edit_revision` 只能改描述性元数据（name/description/flags/tags/meta），**永不动 data/message/version/author**。
- `fork_variant`（`dao.py:**914**`，v5 记 882）= 在同一 artifact 下建新 variant 并**逐条深拷贝整段历史**（O(n) 写、内联 data 重复），每个 slug 加 `_<target_variant.id.hex>` 后缀避免冲突。每条拷贝各调一次 `commit_revision`，且这些调用**不传** `initial`/`expected_head_revision_id`/`no_change_check`（⇒ `needs_lock=False`），故各跑 3-session 往返（INSERT + COUNT + UPDATE），fork 总成本 = O(n) × 3 sessions × 3 网络 RTT。**无共享 DAG、无 merge-base**，分支靠复制而非引用共同祖先——Agenta 自己也将此列为局限。**v6 补充**：fork 现经 `RevisionsLog(depth=artifact_fork.depth)` 支持深度截断（`dao.py:932-938`），即 O(n) 可被调用方限界。

**environment 作为部署指针（`core/environments/`）**
- environment 也是普通 git artifact，但其 revision 的 `data` 不是配置而是 `references: Dict[key, Dict[entity_type, Reference]]` 部署清单，存完整血缘三元组。每次部署/晋升 = commit 一条新 environment_revision（delta set/remove → 物化成完整快照），**历史即部署审计日志**。但事件发布（`publish_revision_event`）在 DB commit **之后** best-effort 调用、**无 transactional outbox**——crash 在 commit 与 publish 之间静默丢事件，审计完整性有缺口。
- 解析“线上跑的是什么” = `environment_ref + key` 两跳间接寻址；commit 时发 `state + diff(created/updated/deleted)` 结构化事件；`is_guarded` + `DEPLOY_ENVIRONMENTS` 做受保护环境闸门。**v5 修正**：`is_guarded` 在 **OSS 版为 no-op**（`ensure_environment_deploy_allowed` 在 `not is_ee()` 时直接 return）；`DEPLOY_ENVIRONMENTS` 与 `EDIT_ENVIRONMENTS` 是**同级 sibling 权限**（在 DEVELOPER 角色同时授予），非严格层级——"比 EDIT 更强的授权"仅在 EE 版成立。
- 局限：跨环境晋升不是单一原子操作（客户端拼装）；delta 路径“读最新→重提交”无 compare-and-set，并发部署可能竞争。**v6 更正（2026-08-27）**：这一局限仍成立，但**成因已变**——CAS 现在是 `commit_revision` 的**参数**（`expected_head_revision_id`）而非 DTO 字段，所以「`RevisionCommit` DTO 不携带版本字段 ⇒ 无 CAS」这一 v5 推理**已不再有效**（`dtos.py:96` 的 `RevisionCommit(Slug, Header, Metadata)` 确实无 version，但它不能再作为论据）。正确表述是：**environments 是一个「已有 CAS 能力但未启用」的调用方**——`commit_environment_revision`（`environments/service.py:1052`）调用 DAO 时只传 `initial=initial`（**1106-1113**），不传 `expected_head_revision_id`，故非初始路径 `needs_lock=False`。见 §0.6 第 7/9 条。

**异常 → HTTP 注册表（`apis/fastapi/git/exceptions.py`）**
- 所有 git 域错误派生自 `GitError`，单个装饰器 `handle_git_exceptions` 把它们映射到稳定 HTTP 码。明文契约：**新增域异常必须同时在域层和传输层注册**。**v6 更正**：注册表已从 5 类扩到 **8 类**（8 个 `except` 臂在 `exceptions.py:107-121`）——InitialRevisionConflict→409、VariantForkError/RetrieveRefsInsufficient/RetrieveRefsInconsistent/InlineResolveInvalid→400，**新增** `RevisionConflict→409`、`CommitLockTimeout→**503**`、`VariantNotFound→**404**`。503 把「锁竞争超时」与「状态冲突」分成两个机器可分支类别，对 Libra 有独立借鉴价值（见 §5 P1-4）。

**并发安全的分支根（v5 收窄 → v6 再修正）**：`commit_revision(initial=True)` 用 `SELECT ... FOR UPDATE` 锁住 variant 行 + COUNT 守卫，保证每分支至多一个初始 revision，冲突精确抛 409。
**v6 更正（2026-08-27，本文原「非初始提交无锁」的结论已被推翻）**：非初始提交**在调用方传 `expected_head_revision_id`（或 `no_change_check`）时同样加锁并做 CAS**——`needs_lock` 覆盖三种情形（`dao.py:1663-1671`），加锁前 `SET LOCAL lock_timeout` 保证有界等待（1677-1681），锁内**重读 head 并精确比较**，不符抛 `RevisionConflict`（1713-1745，同时回传 expected 与 current 两个 id），锁超时抛 `CommitLockTimeout`，锁到空行/跨项目行抛 `VariantNotFound`。
**仍然成立的收窄**：**未传 CAS 参数的调用方**（含 `fork_variant` 与 environments 的全部路径）依旧无锁、无 expected-version 检查——版本号靠 post-hoc COUNT 分配，并发提交可重复/缺失。完整证据与调用方清单见 §0.6 第 7 条。

### 2.b 团队源码的 GitButler 工作流（及其踩坑）

这是 Agenta 仓库自身代码的版本管理实践，**与 2.a 无关**，但对 Libra 有“反面教材”价值。

- **GitButler workspace 模式**：分支 `gitbutler/workspace` 上多条 lane 同时 applied，用 `but` CLI（status/branch new --anchor/commit/rub/absorb/push/oplog）驱动并行工作流。
- **约定式提交 + 闸门**：`.better-commits.json`（feat/fix/... + `Changelog:` trailer）；pre-commit 框架经 husky 跑 ruff/prettier/turbo lint + gitleaks（commit 时扫 staged、push 时扫 merge-base..HEAD），post-checkout 在 lockfile 变化时自动 `pnpm install`。
- **发布**：手动 workflow_dispatch 锁步 bump ~9 个包的 semver，全部一致才开 `release/vX.Y.Z` PR，靠 release-drafter 按 label 出 changelog。
- **硬核踩坑（写进 AGENTS.md）**：GitButler stack 要求线性历史；merge 连接的 series 在 unapply/re-apply 时会坍缩成单条；`but pull` rebase 到 main 而非各分支上游；**唯一可靠恢复是 `but oplog restore`**。
- **关键反差**：这个 checkout 根本没有 `.git`——它由 Libra 管理（`.libra/libra.db` + objects + vault.db）。所有 git 中心的工具（husky、gitleaks `git --staged`、`git merge-base`、peter-evans create-pull-request）都配好了却跑在非 git VCS 上。这印证了 Libra 应把 hook / 密钥库 / 结构化状态查询做成 VCS 原生能力，而非 git 外挂。

---

## 3. Libra 现状速览（已做对、勿当成“建议”）

为避免把已有能力误当改进，先明确 Libra 已经具备的：

- **元数据全进 SQLite**：`reference` 表（kind∈{Branch,Tag,Head}）、`reflog`、`rebase_state`、`config/config_kv` 都是事务行；HEAD 是一行 kind='Head'。ref 写入串行化 + SQLITE_BUSY 重试。
- **Git on-disk 对象完全兼容**：loose fanout + zlib、v1/v2 pack index、sha1/sha256，因此能与 GitHub/Gitea push/pull。元数据分叉、内容格式不分叉。
- **agent-native I/O 契约**：`--json[=pretty|compact|ndjson]`/`--machine` 的结构化输出。**（v6 更正：此处原写「统一 `{ok,command,data}` 信封」，与 §0「结构化输出落点」的澄清自相矛盾。）实际是两种信封形状**——`--json` 路径的 `emit`/`emit_list` 出 `{ok,data}`（`src/utils/output.rs:380,428`），`--machine`/命令信封的 `write_json_command_envelope` 出 `{ok,command,data}`（`output.rs:99`）；**不要假设顶层一定有 `command` 键**。闭合的 `LBR-<DOMAIN>-NNN` 错误码枚举（`StableErrorCode`，`src/utils/error.rs:190-343`，现共 **56** 个变体，`as_str` 一一映射；`docs/error-codes.md` 列 71 行码）+ 固定 category + Git 风格 exit code（128/129），增删码有 `compat_error_codes_doc_sync` 守卫。
- **jj 式 operation log**：`operation/operation_parent/operation_view/operation_view_ref/operation_view_workspace` 五表（v6.1 补全——原文写「五表」却只列四个，漏了 `operation_parent`；五表清单以 `plan-20260822.md:765` OL-02 卡的 v1 表清单为准）+ `libra op log|show|restore`，已能重建 HEAD+所有分支、剪枝、保护锁定分支。
- **锁定/AI 托管分支**：main/intent/traces（旧名 agent-traces）由 `is_locked_branch`/`is_locked_revision`（连 `traces~1`/`intent^`/`@{0}` 后缀也守）跨 reset/restore/switch/checkout/branch/op 一致拒绝。
- **解析优先级已实现且已文档化（代码注释）**：确定性 tier 解析、单一真源；OID 前缀多义已返回 `InvalidReference('ambiguous argument')`。**v6 更正（原文有事实错误）**：顺序**不是**「HEAD > 本地分支 > 远程跟踪 > tag」，且**分支并不胜过同名 tag**——现行顺序是 **HEAD → tag → 本地分支 → 远程跟踪 → OID 前缀**，源码内有明确注释 `// Git's short-ref precedence checks tags before local and remote branches.`（`src/utils/util.rs:1797`；`get_commit_base` 的 doc 注释 `util.rs:2145-2152` 同）。函数也已重构：`resolve_commit_base_atom_typed`（原 `util.rs:836`）**不存在**，现为 `resolve_object_atom_typed`（`util.rs:1757`）分派到 `resolve_tag_atom_typed`/`resolve_local_branch_atom_typed`/`resolve_remote_branch_atom_typed`/`resolve_hash_atom_typed`。
- **稳定 ordinal 句柄已实现**（v6 新增，原 §5 P0-2 的动机据此收窄）：`libra revision find -n <N> | number <commitish> | index [--rebuild]` 提供逐 ref first-parent 链的 **1 起**序号（1=根），由 `revision_ordinal`/`revision_ordinal_meta` 侧表 + 迁移 `2026070301` 支撑，每次读同事务 `ensure_fresh`；`COMPATIBILITY.md:124` 已标 intentionally-different。**尚未实现的是 `<ref>@vN` 这一 revspec 形态**（见 §5 P0-2 与 §0.6 第 2 条）。
- **reflog selector `@{N}` 已实现**（v6 新增）：`resolve_reflog_selector_typed`（`util.rs:1925`），`COMPATIBILITY.md:153`（**`| rev-parse | partial |` 行**——四级矩阵评级是 `partial`，不是 `supported`；只是该行正文明列 numeric reflog selector `HEAD@{N}`/`@{N}`/`branch@{N}` 已支持）；`@{upstream}`/`@{push}`/`@{-N}`/日期 selector 仍未实现；`REV:path` 已支持（`resolve_object_spec_typed`，`util.rs:2124`）。
- **CAS 先例**：`push --force-with-lease`（`push.rs:**1685**` `lease_oid_matches`，v5 记 1480）已证明“声明期望状态、漂移即拒”模式；失败映射到 `LBR-CONFLICT-002`。
- **GC roots 保护先例**：**v6 更正**——`agent_checkpoint_roots`（`gc.rs:1709`）这个函数**已不存在**。现行先例是表驱动的 `collect_registered_store_roots`（`src/command/maintenance.rs:3308-3441`），它同时把 `notes`、`operation_view`/`operation_view_ref`/`operation_view_workspace`、`agent_checkpoint`、`agent_coverage_claim`、`agent_session`、`workspace_record`、`agent_bridge_checkpoint` 纳入 roots，并配有 `GC_OBJECT_SOURCE_INVENTORY` 分型账本。
- **dry-run 多处存在**（commit/op restore/checkpoint rewind/automation），但各自形状不同。
- **A..B 区间解析**已在 v1383 实现（曾经的 `log A..B hangs` 已修）。

---

## 4. 关键差异与可借鉴点

| 维度 | Agenta 怎么做 | Libra 现状 | 启发 / 取舍 |
|---|---|---|---|
| 提交寻址 | `Reference(id,slug,version)` 三模值对象 + 纯函数解析代数 | 单字符串 token 解析（HEAD/分支/远程/tag/hash） | 借“声明—校验”思想，**不照搬** version 维度（见下行） |
| 版本号 | per-variant 0 起单调序号，**离开分支无意义** | **（v6 更正）** `libra revision find/number/index` 已提供逐 ref first-parent **1 起**稳定序号（物化侧表 + 每读 `ensure_fresh`）；仍缺 `<ref>@vN` revspec 形态 | 本文原提案「按需 DAG 计算、0 起、不建表」**已被 `lore.md §1.16` 的实现替代**（1 起 + 物化表）；若补 revspec 须对齐 1-based，见 §0.6 第 2 条 |
| 欠定/矛盾输入 | Insufficient vs Inconsistent 两类带类型错误，点名字段 | 静默尽力解析；冗余多写无校验（当前命令也不收“标识符袋”） | 仅取“冗余一致性校验 + provenance 回显”，作 opt-in 断言 |
| 不可变性 | revision append-only，配置载荷永不可变，历史即审计 | **（v6 更正）** GC 回收问题**已修复**：`operation_view`/`_ref`/`_workspace` 三表已是 traced GC root（`maintenance.rs:3308-3441`）。写入点从 2 处增至 **3 处**（branch reset/create、op restore），branch delete 仍未覆盖 | GC roots 半边**已实现**；「全命令覆盖」半边已由 `plan-20260822` **OL-08/OL-09** 承接 |
| 默认/tip 选取 | 确定性 tie-break，拒绝返回任意行 | 同样确定性；OID 前缀多义已报错 | 已对齐，**仅剩**多段远程跟踪名首匹配的静默选取 |
| 并发分支根 / revision CAS | **（v6 更正）** 初始提交 `SELECT FOR UPDATE` + COUNT → 精确 409；**非初始提交在调用方传 `expected_head_revision_id` 时**同样在有界锁内重读 head 做 CAS → `RevisionConflict`(409) / `CommitLockTimeout`(503) / `VariantNotFound`(404)；未传该参数的调用方仍无 CAS | 事务 + 唯一索引 + busy 重试（SQLite 无行锁）；ref 级 CAS 仅 `push --force-with-lease` 有 | ① 把“竞争失败的裸 integrity error”映射成 `LBR-CONFLICT-002`；② Agenta 的 CAS 与本文 P1-4 `--expect-head` **几乎同构**，已改列为 P1-4 的外部正面佐证（见 §0.6 第 7 条） |
| environment | 一等可移动部署指针，自描述血缘，delta→快照 | **无此概念**；reference 表 `kind` 不含部署类 | 可借鉴，但属 CD/发布管理，需先与维护者确认是否在 Libra 边界内 |
| 晋升 | commit 新 environment_revision，发 state+diff 事件 | 无；reflog 已含 action/message/committer | 用 reflog + op restore 复用，仅补 CAS 守卫 |
| fork | **深拷贝整段历史**（O(n)，自承缺陷） | `libra branch <name> <rev>` 已是 O(1) ref 行 INSERT、对象共享 | Libra 在此**已优于** Agenta，勿照搬其拷贝式 fork |
| 异常→码 | 单装饰器域→HTTP 注册表，明文双注册契约 | 已有闭合 LBR 枚举 + doc-sync | 已对齐 |

---

## 5. 对 Libra 的改进建议（核心）

按价值与确定性排序。每条注明 P0/P1/P2、验证结论、置信度。所有“具体怎么改”均落到已核验的文件/函数/表。

---

### P0-1. 让 operation log 可恢复：把 operation-view 目标纳入 GC roots（修正版）

**状态（v6，2026-08-27）：前半「GC roots」已实现（实现方式与本提案不同）；后半「`op restore` 缺对象 fail-closed」仍未实现，已由 `plan-20260822` OL-10 承接。** 条目按 §8 治理规则保留，不删除。
**v5 验证结论（历史）：viable-with-caveats，置信度高。**

- **问题（前半已消解）**：`op log`/`op restore` 是 Libra 的招牌恢复功能，v5 时 `collect_roots_from_database`（`gc.rs:1260-1297`）从 references/reflog/stash/index/rebase 等播种 roots，**唯独不读任何 operation_view* 表**，于是 `op restore @{N}` 到更早操作会落到缺失对象，招牌恢复功能静默腐烂。
  **v6 更正**：该风险**已被修复**——`src/command/maintenance.rs::collect_registered_store_roots`（**3308-3441**）已把 `operation_view_ref.target_oid`（`StrictOid`）、`operation_view.head_target` 与 `operation_view_workspace.pointer_value`（均 `OidIfParses`）纳入 traced roots，并有 `GC_OBJECT_SOURCE_INVENTORY` 账本条目（3081-3090/3180-3190/3191-3200）与 `tests/db_migration_test.rs:3693-3700` 守卫。
  另两处原文事实须一并更正：(1) `agent_checkpoint_roots`（`gc.rs:1709`）这个函数**已不存在**，现行模板是上述表驱动收集器；(2) 源码中**没有** `DEFAULT_PRUNE="2.weeks.ago"`——现行防误删是 `PRUNE_GRACE_SECS = 3600`（`maintenance.rs:698`）+ `.libra/gc-prune-candidates.json` 两趟隔离账本（`maintenance.rs:748` 起，首次被判不可达不删）。reflog 90/30 天默认不变（`reflog.rs:633`）。
  **仍然成立的问题**：operation 行永久保留（`operation.rs` 无 retention），而 `op restore` 在改 ref 前**不校验目标对象是否存在**（`src/command/op.rs::handle_op_restore`，**387**，5 道前置守卫中无一项做存在性检查）——一旦对象因任何原因缺失，恢复仍会写出悬挂 ref。
- **借鉴 Agenta 什么**：append-only revision 永不物理删除，任何历史 Reference 总能解析；把这条不可变保证补到 Libra 的恢复日志上。
- **具体怎么改**（关键：必须修正提案的字段集，否则会**搞坏 GC**）。
  > **v6 实现记录（已落地，与下述提案不同）**：实际实现没有新增 `operation_view_roots(db)` 函数，而是在表驱动的 `collect_registered_store_roots` 里加三行 source 条目，用 `CellMode::OidIfParses` **无条件扫描三张表**（非 hash 值静默跳过），从而 **既不需要 `WHERE head_kind = 'detached'` 字面量、也不必丢弃 `operation_view_workspace`**。这对下述「风险/注意」第一条指出的同一风险（分支名喂进 `parse_stored_hash` 触发 `RepoCorrupt`）是**更简洁且更保守**的解法。下面的步骤 2/3 与「丢弃 workspace 表安全且无损」的论证保留为设计背景，**不再是待实现指引**。
  - ~~新增 `operation_view_roots(db)`，仿 `agent_checkpoint_roots`~~（历史提案）：
    1. `SELECT target_oid FROM operation_view_ref`（全行，永远是真 commit OID——`operation_wrapper.rs:653/675`）；
    2. `SELECT head_target FROM operation_view WHERE head_kind = 'detached'`（**只有 detached 时 head_target 才是 OID**；分支态 head_target 是分支名，已被其 ref 行覆盖）；
    3. **丢弃 `operation_view_workspace`**——已核验 `pointer_value = head_target.clone()`（`operation_wrapper.rs:689`）：在分支态它是分支名，在 detached 态它**就是那个 OID**。但即便如此，丢弃它仍是**安全且无损**的：detached 态的同一 OID 必然同时写入步骤 2 的 `operation_view.head_target`（`head_kind='detached'`），已被覆盖；分支名则被该分支自身的 ref 行覆盖。所以 workspace 表**不含任何步骤 1/2 未收集的 OID**。（v1 原文“值是分支名，无独有 OID”不精确，会让实现者误以为它永不含 OID。）
  - 用 `table_exists()` 守卫，在 `gc.rs:1295-1296` 处 `roots.extend(operation_view_roots(&db).await?)`。
  - ~~`gc --dry-run --json`/统计输出若已有 roots 分类，增加 `operation_view_roots` 计数~~（历史提案；**v6：`libra gc` 不存在**，且实际实现改用 `GC_OBJECT_SOURCE_INVENTORY` 分型账本 + `db_migration_test` 覆盖守卫达成同一目的，未改动 GC 输出 schema）。原意仍成立：避免未来维护者以为 op log 永久保留但对象保护不可见。
  - **`op restore` 加前置存在性校验**（**这一半仍未实现，是 P0-1 的现存缺口**）：改 HEAD/分支前先验证每个 target_oid（及 detached head_target）对应对象存在，缺失则报 `LBR-REPO-003`（`RepoStateInvalid`，`src/utils/error.rs:196,364`，带 `missing_oid`/`operation_id` detail 与指向 `libra maintenance run --task gc`/对象恢复的 hint），**不要**用 `LBR-REPO-002`（那是 corruption/非法 hash 语义，与 prune 后的缺对象不同）。
    **承接关系（v6）**：该项已被 `plan-20260822` **OL-10**（RestoreEngine 与 op restore 子命令）正式承接——其验收明列 `operation_restore_faults` target 须覆盖「恢复中断、**对象缺失**、并发写冲突、dry-run receipt」（`plan-20260822.md:1197-1215`）。本文不再单独排期，仅保留设计理由与错误码选型。
- **收益**：`op log` 列出的每个操作都保证可恢复；纯 SQLite 元数据，**对 git on-disk 兼容零影响**。
- **成本**：small。
- **风险/注意**：
  - 若按原提案直接把 `head_target` 和 `pointer_value` 当 root 喂进 `parse_stored_hash`（fail-closed 于非 hash），会在“HEAD 在分支上”这一常态触发 `RepoCorrupt` 让 GC 中止——**必须按上述字段集收窄**。
  - 原提案的“半恢复 worktree”措辞不准：`op restore` 只改 SQLite 的 HEAD+分支行，真实失败是悬挂 ref。
  - **丢弃原提案的 `op prune` retention**——它与本条引用的不可变原则自相矛盾、会重新制造悬挂指针。若担心无界增长，另立提案做“原子 retention”（删 op 行与其变为不可达的对象一并回收），不要做默认。

---

### P0-2. `<ref>@vN` 稳定版本号句柄（按需 DAG 计算，不建表）

**状态（v6，2026-08-27）：**能力**已被替代**——稳定 ordinal 句柄已由 `lore.md §1.16` / `libra revision` 交付，走的是与本提案**相反**的设计（物化侧表 + 1 起序号）。**本提案「不建表、0 起」的设计判断被现实否决**。仅 `<ref>@vN` 这一 revspec 形态仍未实现。条目按 §8 治理规则保留，不删除。
**v5 验证结论（历史）：viable-with-caveats，置信度高。核心判断：发功能，砍掉表。**

- **问题（已部分消解）**：v5 时 Libra 只能用不透明 OID 或 git 相对量 `branch~N` 寻址，`~N` 从移动的 tip 反向计数——每次 tip 前进，同一提交对应的数字就变；没有稳定、可读的“本分支第 N 次提交”句柄。
  **v6 更正**：这一前提**已不成立**——`libra revision find -n <N>` / `revision number <commitish>` / `revision index [--rebuild]`（`src/command/revision.rs:54-81`）已提供逐 ref first-parent 链的稳定序号，`--json` 输出含 `ordinal`/`total`，`COMPATIBILITY.md:124` 已标 intentionally-different。原文「Libra 只能用不透明 OID 或 `~N`」须按此理解为历史表述。
  **仍然成立的缺口**：`<ref>@vN` 这一 **revspec 形态**未实现——`src/utils/util.rs` 全文零 `ordinal` 命中；`first_revision_operator`（**util.rs:1950-1954**）只把 `^`、`~`、`@{`（必须跟 `{`）当运算符，裸 `@` 不切分，故 `main@v3` 会被当作整体 atom 解析并失败。（v5 写的 `util.rs:739-991` 解析器已整体重构，见 §0.6 锚点表。）
  ⚠️ **口径冲突（补做前必须先解决）**：本提案是 `@v0 = 根`（0 起），已交付的 `libra revision` 是 **1 起（1 = root）**。补 revspec 时**必须对齐 1-based**，否则同一仓库并存两套版本语义——正是 §6.0.1 最后一条要避免的情形。
- **借鉴 Agenta 什么**：Revision.version 作为分支内 0 起单调序号——一个稳定的绝对位置句柄。
- **具体怎么改**（**v6：锚点已漂移**——`get_commit_base_typed` 现在 `util.rs:**2141**`，`split_revision_navigation` **已不存在**，等价切分点是 `first_revision_operator`（`util.rs:1950`）+ `resolve_revision_expression_typed`（`util.rs:1974`）；若补做，剥除逻辑应放在 `resolve_object_spec_typed`（`util.rs:2124`）入口，并复用 `libra revision` 的既有 ordinal 查询而不是自己重算深度）：
  - 在 `get_commit_base_typed`（原 `util.rs:978`）**先于** `split_revision_navigation` 增加终端 `@v<digits>` 剥除与解析（见 §0.3）：从解析出的 ref tip 沿 **first-parent** 走到根算出深度 D，再后退 D−N 步（即 `<ref>@vN` ≡ `<ref>~(D-N)`）。`ordinal = 从根的 first-parent 深度` 是 commit DAG 的**纯函数**，复用 `--first-parent`/`rev-list --count` 已有基础设施。N>D / N<0 走现有 `InvalidReference`。
  - `--json` 的 log/show 输出加 `ordinal` 字段（`output.rs` emit/emit_list）。
  - COMPATIBILITY.md 加一行 intentionally-different（“在 git 相对 ~N 之上的稳定绝对版本号”）。
- **收益**：agent 与人得到不随 append 漂移的可发音句柄（“main 的第 42 版”），机器可读，重新唤起的 agent 无需记 SHA 即可重新 pin。
- **成本**：medium（实际约几十行解析器改动）。
- **风险/注意**：
  - ~~**不要建 `branch_revision` 物化表、不要做迁移、不要改 ref-writer 事务**~~ —— **v6：这条设计判断已被现实否决，保留为历史记录。** 实际交付的 `revision_ordinal` / `revision_ordinal_meta` **就是**逐 ref 的物化侧表，并带迁移 `2026070301`（`src/internal/db/migration.rs:981-984`）。
    **否决理由（据实记录）**：lore 的 `ensure_fresh` 指纹方案（指纹 = tip OID + `refs/replace` 摘要，在**同一事务内**校验与查询）消解了本提案担心的「缓存说谎」风险——快进只 APPEND 不重编号，重写/replace 变更全量重建，陈旧索引永不作答；而下条「性能注记」担心的 O(depth) 热路径成本，正是建表要解决的问题。GC 侧把两表登记为 `IndexOnly`（可重建、永不作 anchor），因此物化表并未污染对象可达性模型。
    原文对 Agenta 不变量的分析（“version 离开分支无意义”不迁移：Agenta variant 不共享 revision，而 Libra 分支**共享** commit，per-branch 主键会把共享主干冗余存 N 遍）**仍然正确**，实现也确实付出了这个冗余代价，只是判断为可接受（表可重建、非真源）。
  - rebase/reset 会重排被改写后缀的序号——文档须说明“ordinal 仅在最后一次改写点之前稳定”；锁定的 main/intent/traces 以 append 为主，是稳定情形。
  - `is_locked_revision` 会先剥 `@` 再查锁，故 `main@vN` 按读寻址可解析、按写操作与 `main~1` 一样被拒——可接受，解析器须把 `@vN` 严格当读寻址。
  - **性能注记（不建表的代价）**：每次解析 `<ref>@vN` 都要沿 first-parent 从 tip 回溯到根算 D，是 **O(depth)** 的对象遍历。对锁定的 main（持续 append、历史可达数千提交）若用于热循环/批量解析会有可感成本。MVP 接受此代价（正确性优先、零写放大）；若后续 profiling 显示瓶颈，再考虑**只读缓存** `(tip_oid → depth)`（tip 不变即命中），仍不必落物化表、不碰 ref-writer 事务。

---

### P1-3. `commit --assert-staged`：agent 声明预期暂存内容，漂移即拒

**验证结论：viable-with-caveats，置信度高。核心判断：发 commit 半边，砍掉 add 半边。**

- **问题**：Libra 的 `commit` 总是提交**整个 index、无 pathspec 形式**，叠加共享存储 worktree（一个 index 跨所有 worktree）产生三大已记录踩坑：`commit -a` 曾误删文档；并发 tree 竞争时 `diff --cached` 报空而实际 ~300 文件已暂存、`commit` 静默把别进程的暂存一起带走；安全套路（`restore --staged .` → 只 add 自己的 → 肉眼看 status → commit）手动、易竞争、无法强制。**agent 无法“肉眼看 status”**。
- **借鉴 Agenta 什么**：`validate_retrieve_refs_consistent` 的“声明—校验—点名不符字段”契约 + RetrievalInfo provenance 回显，套到暂存区。
- **具体怎么改**：
  **v6 锚点更正（结论不变，仅位置漂移；`grep -rn "assert_staged" src/` 仍零命中，功能确未实现）**：`CommitArgs` 现有 `dry_run` 字段在 `commit.rs:**165**`；真正持 index 的不是 `run_commit`（**923**）而是 `run_commit_with_index`（**995**），`Index::load(path::index())` 在 **1038 / 1049**，`changes_to_be_committed_safe()` 调用在 **1077**，产树函数已更名为 `create_tree_with_persistence(&index, …)`（**1166**）；`-a` 自动暂存为 `auto_stage_tracked_changes(!dry_run, …)`（**1043**），dry-run 分支在 **1033 / 1065 / 1103**（v6 写的 1030 实为 `let signing_policy = …`，最近的 dry-run 段注释起于 1033）。**§0.3 的两条实现约束经复核仍完全成立**：`changes_to_be_committed_safe()` 内部**仍然**二次 `Index::load`（现 `status.rs:**6412-6418**`）。
  - 给 `CommitArgs`（现 `commit.rs:165` 一带，注意它处处用 `..Default`，加字段安全）加 `--assert-staged <manifest>`（路径，或 path+blob-oid，`-` 读 NDJSON stdin）。
  - 在 `run_commit_with_index` 内 `Index::load`（`commit.rs:1038/1049`）与 `create_tree_with_persistence`（**1166**）之间做**只读闸门**：基于**同一**已加载 `Index` 做 staged-vs-HEAD 变更集（勿再调 `changes_to_be_committed_safe()`——它会二次 load index，见 §0.3）；per-path oid 从 `Index::get(path,0)` 取。manifest 路径须规范化并拒绝 repo 外/`../` 穿越。
  - manifest 解析必须有资源上限：限制单行长度、总行数/总字节数，并拒绝重复 path（重复声明应作为 `LBR-CONFLICT-002` 或用法错误处理，不能“最后一行赢”）。`-` stdin 读取也要同样限流，避免 agent/恶意输入把 commit 热路径变成无界内存消耗。
  - 不符时 MVP 复用 `LBR-CONFLICT-002`（staged state 与声明冲突），`details` 用 `with_detail` 分桶 `unexpected_staged / missing_from_index / oid_mismatch`（`error.rs` 已支持）。若后续确需专用码，再新增数字式 `LBR-STAGE-001` 并同步 `docs/error-codes.md`。（**v6 复核**：`LBR-STAGE-*` 域仍不存在；`compat_error_codes_doc_sync` 仍只接受 `LBR-<UPPER>-<digits>`，§0 的警告成立。）成功时把解析出的 staged manifest 作为 `CommitOutput` 的附加字段回显（已序列化进 `{ok,command,data}`）。
- **收益**：把头号静默损坏踩坑变成大声、机器可操作的结构化冲突，点名出错路径；agent 获得 compare-and-commit 语义。
- **成本**：medium。
- **风险/注意**：
  - 原提案两处事实错误须纠正：(1) 暂存真源是 **`.libra/index`（git 二进制格式），不是 `.libra/libra.db`**；(2) 比较集是 **staged-vs-HEAD，不是原始 index 全集**（后者含每个被跟踪文件，会逼 agent 声明全部路径）。
  - **砍掉 `add --assert-staged`**：`AddArgs`（`add.rs:**71**`，v5 记 67）有 250 处无 `..Default` 字面量站点（仓库内“#1 BLOCKED”结构），且 `add` 本就接受 pathspec，价值低。
  - 错误 category 重新斟酌：precondition 失败按 Cli/Usage(129) 对 agent 可能比 conflict 更好分支，与 `--force-with-lease` 拒绝的编码对齐。
  - 真正的 `commit -- <pathspec>` 是独立、更大的 git-parity 工程（需 HEAD-tree ∪ 命名 index 项的合成树），**单列**，勿与本条捆绑。

---

### P1-4. ref 级 compare-and-swap：`--expect-head` / `--expect-branch`（诚实收窄版）

**验证结论：viable-with-caveats，置信度高。核心判断：只做 ref 级，砍掉 --expect-tree，并重写动机。**

- **问题**：`push` 已证明 CAS 有用（`--force-with-lease`），但其余每个变更命令（commit/reset/switch/...）都作用于“此刻恰好”的 HEAD，agent 无法断言它相信的操作前提状态。
- **借鉴 Agenta 什么**：冗余一致性校验（多写即校验、点名不符）+ InitialRevisionConflict 的“检查—写入在受控临界区内、报精确冲突”。
  **v6 新增：Agenta 已把这条纪律实现成与本提案几乎同构的 revision 级 CAS，是 P1-4 的外部正面佐证**（此前本文把 Agenta 列为“无 CAS”的反面案例，该定性已过期）。`commit_revision`（`dao.py:1607`）接受 `expected_head_revision_id`，触发 `SET LOCAL lock_timeout`（有界等待，1677-1681）+ `SELECT ... FOR UPDATE`（1683-1691），**在锁内重读 head 并精确比较**（连“head 不存在”也算不符），不符抛 `RevisionConflict` 并**同时回传 expected 与 current 两个 id**，让调用方一步内重读重试（1713-1745；类型见 `core/git/types.py:133`）。三条可直接搬用的设计要点：
  1. **比较必须在锁内重读**——调用前做的比较是「对一个别的写者仍能移动的 head 的判断」（Agenta 在源码注释里写明了这点）。这与本条下文“在现有 ref 更新事务内读当前 ref”是同一主张。
  2. **错误同时携带 expected 与 actual**——对应本条的 `with_detail("expected_oid"/"actual_oid")`。
  3. **等待必须有界**——Agenta 用 `lock_timeout` 把无界等待变成可分类失败；Libra 的等价物是 SQLite `busy_timeout`，落地时应确认超时路径也映射到稳定错误码而非裸 `SQLITE_BUSY`。
- **具体怎么改**（**v6 锚点更正；`grep -rn "expect_head\|expect_branch" src/` 仍零命中，功能确未实现**）：
  - 仅在**真正移动 SQLite ref、且已在 `_with_conn` 事务内更新**的命令上加 `--expect-head <oid>` / `--expect-branch <name>`：先做 commit / reset / switch（最省），rebase/merge 后做（引擎侵入大）。
  - 在**现有 ref 更新事务内**读当前 ref（`update_head_and_reflog` 现 `commit.rs:**3018**`，v5 记 1899；`reset.rs` 的 `with_reflog` 调用现 **1560**，v5 记 770-786），不符即中止。因为 HEAD/分支在 `reference` 表、写入经 busy_timeout 串行，这是真正的原子 CAS——也是本提案原子性论证**唯一成立**之处。
  - 仅复用 `lease_oid_matches`（`push.rs:**1685**`，v5 记 1480，前缀容忍比较）；**不要“泛化 validate_force_with_lease”**（现 `push.rs:**1701**`；它校验的是协议广播的远程 oid，与本地 HEAD 读取是不同数据源）。
  - 复用 `LBR-CONFLICT-002`（ConflictOperationBlocked）+ `with_detail("expected_oid"/"actual_oid")`；不要新铸 `LBR-PRECONDITION-*`，当前稳定错误码目录没有这个 domain，且项目惯例是复用冲突码表达 compare-and-swap 失败。
    **v6 取舍记录**：Agenta 的注册表把「状态冲突」（`RevisionConflict`→409）与「锁竞争超时」（`CommitLockTimeout`→**503**）分成两个机器可分支类别。Libra 若把 CAS 失败一律压进 `LBR-CONFLICT-002`，就**丢掉了 busy/超时这一维度**。MVP 仍复用 `LBR-CONFLICT-002`（避免新增域触发完整 doc-sync 流程），但须用不同 detail key 区分「漂移」与「争用超时」，并把这个取舍写进命令文档；若日后 agent 确需按类别分支重试，再考虑新增数字式码。
- **收益**：把 push 之外的整个变更面铺上乐观并发控制，多进程 HEAD 竞争可被原子检出。
- **成本**：medium。
- **风险/注意（最重要：重写动机）**：
  - **砍掉 `--expect-tree`**：index（`.libra/index`）与工作树是磁盘文件、在 SQLite 写锁之外，无法成为提案宣称的原子 CAS，且 agent 几乎从不知道期望 tree oid。
  - **诚实重述卖点**：被引用的三起事故（wip-bundle 5 次恢复、rebase 丢 170 文件、并发 ~300 文件）**HEAD 全部正确**，`--expect-head` 都会通过、一个都防不住。本特性只应卖作“多进程 HEAD 竞争保护”（真实但窄）；wip/rebase 退化的真正钱该投向 rebase/cherry-pick 的 tree-rebuild-from-partial-checkout 根因（3-way replay + ref 更新前的 tree-diff 闸门）。
  - 不要一次性铺六个命令（rebase.rs 是 **5539 行**延期引擎——v5 记 4227，v4 记 3384，两版之间又长了 ~1300 行，是「勿一次铺开」的最新佐证），违反仓库有界切片规范。
  - **v6 复核：commit reflog 的 TOCTOU 前提仍完全成立**——`new_reflog_context`（`commit.rs:**3036**`）在 `with_reflog` **事务之外**用 `Head::current_commit()` 读 `old_oid`（**3040**）。CAS 实现必须把 expected/actual 比较与 `old_oid` 捕获都移入事务内。

---

### P1-5. 给每个 ref-变更命令记录整库 operation view（原子、完整 undo）

**状态（v6，2026-08-27）：已由 `plan-20260822` OL-08 / OL-09 正式承接。** 本条降为历史提案，不再独立排期——OL-08 实现 `operation/middleware.rs` 的 `MutationClass` 七类穷举 + `classify_command` + `run_with_operation`（pin RequestScope → 获取 worktree lease → stale 检测 → 外部变化先发 snapshot op → running op + journal reservation → 业务闭包 → post-view 捕获 → CAS 发布），**未知 mutation fail closed**；OL-09 负责 CLI 与 Agent tool mutation 接入（`plan-20260822.md:1087-1196`；对应 GAP-02，:77）。这正是本条想要的「在命令边界封装」的上位方案。
**本条仍有独立价值、且未被 OL-08/OL-09 明文覆盖的细节只有两条**：① branch **delete** 仍未接 `with_operation_log`；② 序列化命令（rebase/cherry-pick/merge/revert）必须坍缩成**一个** operation，不可每个内部 reflog 写点各记一条。
**v5 验证结论（历史）：viable-with-caveats，置信度高。核心判断：这是接线，不是新建。**

- **问题**：`with_operation_log` 只接到 **2 处**（branch create、op restore），且 branch **delete 未覆盖**。**v6 更正**：现为 **3 处**——`src/command/branch.rs:**519**`（branch **reset**，v5 后新增）、`branch.rs:**1521**`（branch create）、`src/command/op.rs:**596**`（op restore）；**branch delete 仍未覆盖**，`with_reflog_and_operation` 组合封装仍不存在，故核心缺口判断不变。用户/agent 最需要 undo 的破坏性命令（reset/rebase/merge/commit/switch/cherry-pick/revert）走的是 `with_reflog`（原子更新单 ref + reflog 行）但**从不记录整库 operation view**。于是 `op restore` 无法把一次 `reset --hard` 或失败 rebase 当作单一原子多 ref 步骤撤销——恢复退化为手动逐 ref reflog 手术，正是 AGENTS.md 为 GitButler 记录的痛点。
- **借鉴 Agenta 什么**：每次状态变更都 append 一条不可变完整快照，历史即可重放审计。
- **具体怎么改**：引擎已全在（`with_operation_log` 整库快照 + 5 表 + parent-DAG 选择 + dedup + busy 重试 + 可用的 `op restore`），真正缺的是接线。
  - 加薄封装 `with_reflog_and_operation(meta, scope, reflog_ctx, insert_ref, op)`：跑现有 op 事务，并在同一闭包内 append `Reflog::insert(txn,...)`（与 branch-create 已证明的同形 `FnOnce(&txn)->Future` 签名）。
  - **在命令边界封装、而非每个 reflog 写点**：rebase 单独就有 3 个 `with_reflog` 点（现 `rebase.rs:**2588 / 3118 / 3459**`，v5 记 1638/2064/2246），1:1 替换会把一次 rebase 碎成多个 operation。整段序列化命令（rebase/cherry-pick/merge/revert）须坍缩成**一个** operation。
  - 修正集成清单：**丢 restore.rs**（不动 ref）、**加 branch delete**（当前仍未记录）、**特判 revert.rs**（经 `Branch::update_branch` 直改、绕过 with_reflog，现 `revert.rs:**1487**`，v5 记 1197）。
- **收益**：所有破坏性操作单步原子 undo；`op log --json` 完整机器可查审计；恢复故事终于匹配项目对 GitButler oplog 的依赖。
- **成本**：large。
- **风险/注意**：
  - 两个封装各自开 `db.transaction`——必须**组合成一个事务**，不可嵌套，否则 SQLite 死锁。
  - **写放大**：`collect_final_view`（现 `collect_final_view_with_conn`，`operation_wrapper.rs:**1444**`；`pointer_value = head_target.clone()` 现 **1509**，v5 记 689）每次快照全部 ref，commit（最热命令）会每次写 O(refs) 行 + 分页 parent 扫描。开启到 commit 前需加 view 去重（ref 集不变则复用上一 view_id）或更轻的 commit scope。
  - 可恢复序列（rebase --continue）跨进程，单个 DB 事务无法字面跨越整个用户可见操作——须明确“仅在最终完成时记录”或把多步缝成一个 op_id。
  - 增量落地：先 reset、再 merge/commit（最清晰的 "undo --hard" 收益），最后做序列化命令的命令边界封装。

---

### P1-6. 抽出 worktree 无关的提交级 merge 原语 `merge_commits`

**验证结论：viable-with-caveats，置信度高。这是更大编排器改造里今天就能做、独立有价值的一步。**

- **问题**：AI 编排器的 `sync_task_worktree_back`（现 `workspace.rs:**1041**`，v5 记 1032-1116）用文件级 3-way `diffy::merge_bytes`（现 `workspace.rs:**1317**`，经 `try_merge_text_change` **1287**）重整，**完全脱离 VCS**（无 Commit/Tree/HEAD 调用），正是 rebase 丢文件 / commit -a 误删 / 并发暂存竞争三类踩坑的滋生地。而 Libra 现有的 `perform_three_way_merge`（现 `merge.rs:**1876**`，v5 记 646）**不能直接用于任务回合**：它要求干净的共享树、读写共享 index、移动共享 HEAD、用 `reset_index_and_workdir_to_tree`（现 `merge.rs:**3389**`）覆盖共享工作目录。**v6 复核：文本级合并仍在，本条动机成立。**
- **借鉴 Agenta 什么**：append-only 不可变提交纪律——回合必须是忠实快照，不是静默局部；“校验而非静默重整”。
- **具体怎么改**：把 `merge.rs:**3045**` 的 `merge_tree_items(base,ours,theirs)`（v5 记 1332）+ `create_tree_from_items_map`（现 **3376**，v5 记 1584）这一**纯对象图 3-way 合并核**（无 workdir/HEAD I/O）暴露为 `merge_commits(base,ours,theirs)->{tree_id,conflicts}`。这是今天唯一存在、可独立落地的部分，用它替掉脱离 VCS 的 diffy 文本合并，立即获得真 3-way 语义。
  **v6 进展记录（一半已被顺带完成）**：`create_tree_from_items_map` **已从私有提升为 `pub(crate)`**，并已有跨命令复用先例（`src/command/stash.rs:35,832,834`）；`merge_tree_items` 仍是私有 `fn`。因此本条剩余工作缩小为「暴露 `merge_tree_items` + 封装 `merge_commits`」。
  ⚠️ **待复核（v6 新观察）**：`src/command/rebase.rs:**5193**` 存在一份**同名的 `create_tree_from_items_map` 重复实现**。提取时是否应一并收敛这两份，本轮未评估——见 §0.6「待复核」。
- **收益**：去掉文件级静默重整；为后续真正的提交级 merge-back 打底；契合 Libra 不可变对象气质。
- **成本**：medium（提取 + 暴露私有核）。
- **风险/注意**：
  - 完整的“每任务真提交 + merge-back”**双重依赖未建能力**：`src/command/fork.rs`（不存在）与 per-worktree HEAD（明确延期、schema-blocked，见 §5 末“产品方向决策”）。无 per-worktree HEAD 时任何 per-task commit 会污染并发任务——这正是 AI 被告知“不要 run git commit”的原因。
  - 提案借鉴的“校验、点名路径、不静默重整”闸门**已实现**（`collect_contract_violations` + `format_contract_violation_message` + `detect_contract_violations`），**勿当新功能重提**。但须澄清其归属（已核验）：这三个函数在 **`src/internal/ai/orchestrator/workspace.rs`**、校验的是**编排器 task-worktree-back 路径**，**不在 `merge.rs` 命令路径上**。所以本条提取的 `merge_commits` 与这套校验器是两个不同位置的能力：前者给真 3-way 语义，后者在回合回写时点名违例；落地时应让 `sync_task_worktree_back` 改用 `merge_commits` 后，**继续**复用已有 contract 校验器，而非把校验器误植到 merge 命令里。
  - merge 最终 `reset_index_and_workdir_to_tree` 写入部分 checkout 仍会覆盖未物化文件——提交级 merge-back 也须避免把合并树物化进共享部分工作目录。

---

### P2-7. 精简版 ref 一致性断言 + provenance 回显（`--ref-assert`）

**验证结论：viable-with-caveats，置信度高。核心判断：大幅收窄后才可行。**

- **问题**：agent 防御性多写（“OID abc 应在 main 上”）时，Libra 不验证这些标识符是否互相吻合。对持陈旧信念的 agent，静默尽力解析 = 在错的 commit 上行动而无信号。
- **借鉴 Agenta 什么**：冗余一致性校验点名不符字段 + RetrievalInfo provenance 回显。
- **具体怎么改**：
  - 新增纯、无存储模块 `src/internal/refspec.rs`：给定 agent 已相信的 oid/branch/tag，按文档顺序解析，第一个不命中解析结果的字段在 MVP 抛 `LBR-CONFLICT-002` + 点名字段 detail；若后续 agent 需按 Repo/Ref category 分支，再新增 `LBR-REF-001` 并同步 doc-sync。
  - 在 `{ok,command,data}` 信封内加 `resolved:{oid,branch,tag,used_fields}` provenance 块，让 agent 确认实际作用对象。
  - 用独立标志 `--ref-assert oid=..,branch=..,tag=..`（**不要复用 `--ref`**，它在 notes/publish 已有单 ref 语义）。
- **收益**：agent 可防御性多写、得到带类型字段级拒绝；解析规则可脱存储单测；每次读返回可验证 provenance。
- **成本**：medium（限于模块 + 两个命令先行）。
- **风险/注意**：
  - **砍掉 ordinal 臂与 insufficiency 主卖点**：Libra 无固有 per-branch ordinal（DAG），且 oid/branch/tag 各自可识别，“裸 version 不可识别”基本蒸发。剩下的是 oid/branch/tag 互一致性，比标题更薄。
  - 错误码 MVP 用 **`LBR-CONFLICT-002`** + 字段 detail；仅当 agent 需 Ref category 分支时再新增 **`LBR-REF-001/002`**（数字后缀）：原提案的 `LBR-REF-INCONSISTENT/INSUFFICIENT` 违反 `LBR-<DOMAIN>-NNN` 约定，且会被 `error_codes_doc_sync`（只收数字后缀）静默跳过——“已有守卫覆盖”是假的。
  - 纠正前提：Libra **不用 `git rev-parse`**，用原生 `get_commit_base_typed`（现 `util.rs:**2141**`，v5 记 978）；`log A..B hangs` 是另一个已修区间 bug，本改动不修它。
  - **v6 复核**：`src/internal/refspec.rs` 与 `--ref-assert` 仍不存在、`LBR-REF-*` 域仍不存在，本条状态不变（增益项）。但**解析顺序前提已变**——见 §0.6 第 3 条：短名解析是 tag 优先于本地分支，`--ref-assert` 若同时收 `branch=` 与 `tag=` 必须按这个顺序判定「解析出的那一行」，否则一致性校验本身会给出与命令实际行为不符的结论。
  - 当前无命令收“标识符袋”，故这是 net-new opt-in 面，按“agent 确认价值”立项，不是修现有 hazard。

---

### P2-8. push 路径集成“丢路径”预检（默认 warn）

**验证结论：viable-with-caveats，置信度高。核心判断：砍掉 rebase/merge/pull 闸门，只保留 push 预检。**

- **问题**：原最高危踩坑是 rebase/merge/pull 从部分 checkout 重建树、静默丢弃磁盘上为空的 ~170 个被跟踪文件，随后 push 把它们从 origin 删除；唯一防御是手跑的 `comm -23 ls-tree` 闸门。
- **借鉴 Agenta 什么**：commit 必须是忠实快照、不是静默局部;“不一致即拒”。
- **具体怎么改**：在 push 发送前，比对待 push tip 的树路径集与远程跟踪 ref 的树（`fetch` 后已可得；`incremental_objs` 现定义在 `push.rs:**2699**`、调用点 **2052 / 2094**——v5 记的 2407 已失效，究竟挂哪个调用点更合适见 §0.6「待复核」）。若远程树存在的路径在 tip 缺失且无法被 push 提交区间的 diff 解释，作为 `details.dropped_paths` 报出，**默认 warn**（或 `push.guardDroppedPaths` 配置，默认 warn），提供 `--allow-deleted-paths` 覆盖。用 **`LBR-PUSH-*`**（风险是错误 push，不是错误 tree-build，比 `LBR-TREE-*` 更贴）。
- **收益**：把腐烂的部落知识做成机器可读防护；防御未来 rebase --autostash 等可能重新引入 workdir-based 风险的回归。
- **成本**：medium。
- **风险/注意**：
  - **砍掉 rebase/merge/pull 内闸门**：已核验当前所有产树路径（`rebase.rs:3519-3686`、`merge.rs:660-687`/`commit_tree_items`）均从对象库树或 index 构建、**不再扫 workdir**；2026-06-22 的“rebase 丢文件”是旧 workdir-based FF reset，已修。在那里加闸门要么死代码、要么对每次合法删除误报。
  - **必须默认 warn 而非 abort**：经 push 删文件是正常 git，abort-by-default 会破坏普通工作流。
  - **顺手高杠杆**：更新陈旧 agent memory（`libra_rebase_drops_files_hazard.md`、`dev_commands_improvement_loop.md` 的“NEVER rebase”），让 agent 不再每次发布都付“reset --mixed + 手动 comm-gate”税——bug 已修。

---

### P2-9. 跨破坏性命令统一的机器可校验动作预览信封 + `--assert-preview`

**状态（v6，2026-08-27）：`op restore` 那一块已由 `plan-20260822` OL-10 承接**（其验收明列「`op restore` 子命令（含 `--dry-run` 与 JSON/machine receipt）」+「机器接口冻结：receipt 字段与退出码有契约测试」，`plan-20260822.md:1206-1215`）。**其余命令（rebase/merge/switch/reset/restore）的统一 Preview 信封仍无承接，保持增益项。**
**v5 验证结论（历史）：viable-with-caveats，置信度高。核心判断：附加式、不替换，增量落地。**

- **问题**：dry-run 散在多处但**形状各异**（`op restore --dry-run` 甚至不遵守 `--json`，走 `println!`），且 rebase/merge/switch/reset/restore **完全无结构化预览**。agent 须为每命令学一套解析器。**v6 复核：仍然如此**——`op restore --dry-run` 的人类输出仍是 `println!`（现 `src/command/op.rs:**511-541**`，v5 记 405-428），仍无 `--json`；`output.rs` 的双信封前提也不变（`emit` **380** / `emit_list` **428** / `write_json_command_envelope` **99**）。
- **借鉴 Agenta 什么**：RetrievalInfo provenance + 部署 state+diff 事件；checkpoint rewind“同时显示 would-restore 与 would-delete”两侧 diff 是 Libra 自证。
- **具体怎么改**：
  - 在 `output.rs` 定义共享 `Preview` 类型 + `emit_preview`：`resolved_refs`（RetrievalInfo 式）、`writes`（会变的 objects/refs）、两侧路径 diff `{would_modify, would_add, would_delete}`。
  - **附加式**（新 `preview` 键），先落到**今天没有预览**的命令（rebase/merge/switch/reset/restore，及补上缺失 JSON 的 op restore），保留现有 commit/checkpoint/fetch 形状不动。
  - 配 `--assert-preview <hash>`：dry-run 记 digest → 实跑带 digest，状态漂移则报 `LBR-CONFLICT-002`（MVP 不新增 `003`）。digest 必须基于 canonical JSON（稳定字段顺序、稳定数组顺序、无 pretty-print 影响）和明确 schema version；在现有 `with_operation_log` 事务内（现 `op.rs:**596**`，v5 记 447）做 recompute-compare-apply，对 refs 原子。
- **收益**：agent 学一套预览 schema、获得 preview-then-apply CAS；“检视→推理→在所检视之物上行动”成为一等可靠闭环。
- **成本**：large（消整合多命令）。
- **风险/注意**：
  - **不可替换现有 dry-run JSON**（commit/checkpoint/fetch/reflog-expire）——按 `cli-error-contract-design.md:241` 是破坏性、AGENTS.md P1。
  - **不要对 merge/rebase 过度承诺两侧 diff**：非 ff 合并结果不实跑无法预知；为它们定义降级预览 `outcome: requires_merge`，仅 ff/无冲突时给精确 diff。
  - 错误码：`LBR-PRECONDITION-002` 非法（无 PRECONDITION category）；陈旧预览漂移就是 `LBR-CONFLICT-002`。
  - Preview 体积可能很大（路径 diff、writes 列表）；JSON 输出应保留完整内容，但人类 stdout 可摘要。`--assert-preview` 只接受 digest，不接受整份 preview 回传，避免命令行/环境中复制大 payload。
  - Plan-Mode 增量：tracer-bullet 先 `output.rs` 定义 Preview + 接 op restore + switch/reset 两个命令再扩展。

---

### P2-10. operation 表的 append-only 强制（+ 可选外锚 Merkle-DAG 摘要）

**验证结论：viable-with-caveats，置信度高。核心判断：保留目标，拒绝原机制，先做便宜版。**

- **问题**：Libra 把 SQLite 历史（reflog/operation/...）当可重放真源，但它们是普通可变行。错误迁移、直接 `sqlite3` 写、有 DB 访问的 agent 都能改写/重排审计行而无检测。`fsck` 只校验对象哈希，不验 history-as-data 表完整性。
- **借鉴 Agenta 什么**：append-only 不可变性是审计可信的根基。
- **具体怎么改（先便宜后昂贵）**：
  - **第一步（在 grain 内、便宜）**：加 SQLite 触发器禁止 `operation` 表的 UPDATE/DELETE（仅 INSERT/SELECT），+ `fsck` 检查行数/PK 一致性。
    **v6 复核**：`operation` 表**仍无** append-only 触发器（`sql/` 下与 operation 相关的触发器只有 `operation_scope_provenance_domain_*` 与 `operation_scope_kind_domain_*` 两组**域值校验**触发器，不是 append-only）。但仓库**已有同形先例可直接照抄**——`agent_workspace_scope_audit_append_only_update` / `_delete`（`sql/migrations/2026080401_agent_capture_workspace_scope.sql:198-207`，含配套 `_down.sql` DROP），另有 `agent_tombstone_*` / `trg_agent_subagent_boundary_delete`。因此第一步的可行性应从「新机制」降级为「照抄已有先例」。
    **目标对齐（v6）**：本条目标与 `plan-20260822` 的 **ADR-OL-07**（Undo 为**追加式**显式状态变换，不修改历史 Operation，`plan-20260822.md:178-186`）一致；落地时应确认触发器不会阻断 OL-* 的 journal/CAS 写路径。这正是 Agenta 灵感实际展示的（应用/DB 层强制不可变），也与 Libra 已规划的 append-only `agent_audit_log` 一致。**威胁模型边界**：持有 `.libra/libra.db` 写权限的攻击者可 `DROP TRIGGER` 或直接改文件——触发器防的是应用 bug 与误用 `sqlite3` CLI，不是密码学防篡改；第二步外锚才是对抗 DB 写权限的必要条件。
  - **第二步（仅当确需密码学保证）**：在 `operation` 表加摘要，但用 **Merkle-DAG** 而非线性链：`row_digest = H(canonical_content || sorted(parent_row_digests))`（operation log 是 `operation_parent` M:N 图、且并发 agent 合法分叉，线性链 + “重排/缺口检测”会对正常并发误报）。摘要**必须外锚**（用 agent 不持有的密钥签 chain head，或写入 append-only `agent_audit_log`，或 commit 进随 push 旅行的 ref/note），否则同一攻击者可重算整链 = 安全剧场。
  - 仅针对 `operation`（可选 reflog）；**不要**声称覆盖 object_index（可重建、对象自验）或 ai_* （文档化的可重建投影，真源是带 u64 seq 缺口检测的 append-only JSONL）。
- **收益**：把审计日志变防篡改，给 AI runtime 可验证 provenance 骨干。
- **成本**：medium。
- **风险/注意**：纯 in-DB 线性链不可行（攻击者可重写下游整链）；DAG 误配会对正常并发分叉误报；迁移须幂等 guarded ADD COLUMN，pre-migration 前缀须视为“不可验证”而非“已篡改”。

---

### P2-11.（需先做产品决策）部署指针集群：`libra env` + `libra promote` + `@{deploy:prod}`

**验证结论：viable-with-caveats，置信度中。强烈建议：先与维护者确认是否在 Libra 边界内。** 这是一个 CD/发布管理特性，机制可行且符合 Libra 的 SQLite-元数据 + 保留 orphan ref 习惯，但与“git 兼容核心 VCS”边界相邻而非重合；且与当前 git-parity 路线图正交。

> ⚠️ **与 `plan-long` 不采纳清单的显式对齐（v6 新增，落地前必读）**：`plan-long.md:520` 已明列不采纳「复制 Agenta 的 prompt/workflow 应用版本平台」，:555 已明列「不把 Agenta 当源码 VCS 对标」。本卡是本文距该红线最近的一条，故在此写明边界：
> 本提案借鉴的**只是** environment-as-pointer 的**指针语义 + CAS 纪律**（一个命名指针指向不可变 commit，移动时做 compare-and-swap 并留审计），**不引入** Agenta 的 Artifact / Variant / Revision 关系模型，**不**把 Libra 变成 prompt/workflow 版本管理平台，**不**引入其 delta→快照的配置载荷语义。这与 §0.4 实现闸门「不引入 Agenta 式 Artifact/Variant/Revision 三表到 Libra」是同一条约束的两处表述，须交叉遵守。
> 若这条边界无法在设计评审中守住，本卡应直接判为 **不采纳**，而不是缩小范围继续推进。

- **问题**：Libra 无“哪个 commit 在哪个环境上线”的概念；要追踪 dev/staging/prod 只能滥用分支/tag，混淆“工作线”与“部署目标”，无运行记录。`reference` 表 `kind` 也结构性不含部署类。
- **借鉴 Agenta 什么**：environment-as-pointer（一等可移动命名指针、存完整自描述血缘、survive 重命名）；deploy/promote = commit 不可变 references 快照 + state/diff 事件；`environment_ref + key` 两跳寻址“线上跑的是什么”。**Agenta 自承的两个缺陷正好让 Libra 做得更好**：跨环境晋升不是单一操作、delta 路径无 CAS。
- **具体怎么改**（三个子提案，有依赖）：
  - **(a) `libra env list|create|show|set`**：新 `deployment` 侧表（name, commit_oid, source_ref, label, deployed_by, deployed_at, guarded），指针存于 `refs/libra/deploy/<name>` orphan namespace（仿 `AI_REF`/intent/traces，kind='Branch' 行，**不 push 到 stock git**，保 on-disk 兼容）。存 OID 作真源 + source_ref + 可选 label，使指针 survive 分支 churn。**勿编辑冻结的 `sqlite_20260309_init.sql`**，加新 `sql/migrations/<date>_deployment.(sql|_down.sql)` 经 `migration.rs` `include_str!` 注册（开库自升级）。加 `Env` 到 `Commands` 会强制 **三件套同步**（COMPATIBILITY.md 行 + commands/README.md 行 + commands/env.md），否则 `compat` 测试失败。统一 ref 命名并接全部守卫（`is_locked_branch` 精确匹配 **和** `op.rs` 的 `starts_with("libra/")` 过滤 **和** branch-list 隐藏）。
  - **(b) `libra promote --from staging --to prod`**：做成薄糖——`get_target_commit` 解析源 tip，`with_reflog`（现 `reflog.rs:**405**`，v5 记 322，仍是“ref 移动 + 审计写入”单事务）包裹一个**新增 CAS 的** `update_branch_with_conn`（给它加 `expected_old_oid: Option<&str>`，目标 tip 变化即在事务内失败）。**砍掉**原提案的独立 `deployment_log` 表和 `env rollback`——复用 reflog 的 action/message/committer 列记晋升血缘、复用 `op restore` 回滚。原子晋升 ~90% 已具备，只补缺失的 CAS 守卫 + promote 动词。
  - **(c) `@{deploy:<env>}` revspec**：在短名 atom 解析器（**v6：`resolve_commit_base_atom_typed` 已不存在，现为 `resolve_object_atom_typed`，`util.rs:1757`**；`@{…}` 形式的入口另见 `first_revision_operator` `util.rs:1950` 与 `resolve_reflog_selector_typed` `util.rs:1925`——后者说明 `@{N}` 数字 reflog selector **已实现**，新臂须与之明确区分）加一个早期解析臂 → SQLite 查 → 具体 OID，则 log/diff/show/checkout 经共享解析器自动继承，`diff @{deploy:staging}..@{deploy:prod}`（“待晋升的是什么”）经现有 `normalize_diff_range` 自动可用（约 10-30 行）。**诚实文档化为 net-new、intentionally-different token**（stock git 解析不了；`is_valid_refname` 已拒 `@{`/`:`），勿宣称“扩展 git @{} 语法”（Libra 未实现 @{upstream}）。
- **收益**：运维与 agent 得到“各环境上线什么”的类型化可查答案，与分支拓扑解耦；晋升原子、可回滚、可审计，且优于 Agenta（CAS 而非读后写）。
- **成本**：medium（每子提案）。
- **风险/注意**：
  - **(c) 完全依赖 (a)**，**(b) 依赖 (a)**——不能独立评估/发布。
  - **范围契合是真正的开放问题**：这是 release/CD，与 `publish` 的 "deploy" 子命令（部署 Cloudflare Worker，非 commit 指针）重名风险——用 `libra env`/`libra promote`，**勿用 `libra deploy`**。其价值取决于 Libra 是否要把“哪个 commit 在哪上线”纳入自身 AI-agent-native 身份。
  - 受保护环境（原“guarded environments”提案）大体 **infeasible**：依赖此非存在子系统，且 Libra 权限模型是无强弱层级的自由字符串有序规则集，没有“DEPLOY > EDIT”的表示。**唯一可留的小点**：AI 发起的晋升经现有 `approved_permission`（Ask→Always）流，用新字符串权限键 `promote`；人工 CLI 拒绝复用 locked-ref 的 `ConflictOperationBlocked` 风格，**不**铸 `LBR-DEPLOY-*`（locked refs 也复用 `LBR-CONFLICT-002`）。

---

## 6. 落地执行包

这一节是可直接拆 issue / PR 的执行版。**默认顺序（v6 更正，与 §6.0 状态列一致）是 B1 → B2 → B4**；A1 前半已实现、后半由 OL-10 承接，A2 已被替代，B3 已由 OL-08/OL-09 承接，三者**不再排期**（各卡内保留状态标注与设计记录，编号按 §8.2 不重编、不删除）。C 组为增益项，D 组必须先做产品决策。每张卡都应独立合并、独立回滚。

### 6.0 追踪矩阵（建议 ↔ 执行卡 ↔ 依赖 ↔ 状态）

**状态列口径（v6，2026-08-27）**：四值——`已实现` / `已被替代` / `已由 plan-20260822 <卡号> 承接` / `待实现`（增益项在成本列外另注）。**编号一律不重编**：被现实推翻的条目保留编号并加标注（§8.2、§0.6）。

| 建议（§5） | 执行卡（§6） | 优先级 | 成本 | 依赖 | 状态（v6，2026-08-27） |
|---|---|---|---|---|---|
| P0-1 op-view 纳入 GC roots + op restore fail-closed | A1 | P0 | small | — | **前半已实现**（`maintenance.rs:3308-3441`，实现方式与提案不同）；**后半（op restore 缺对象 fail-closed）已由 plan-20260822 OL-10 承接** |
| P0-2 `<ref>@vN` 稳定句柄（不建表） | A2 | P0 | medium | — | **已被替代**（能力由 `lore.md §1.16` / `libra revision` 交付，1 起 + 物化表，与本提案设计相反）；仅 `@vN` revspec 形态待实现，且须改为 1-based |
| P1-3 `commit --assert-staged` | B1 | P1 | medium | — | 待实现（无承接；2026-08-27 复核仍零命中） |
| P1-4 ref 级 CAS `--expect-head/--expect-branch` | B2 | P1 | medium | 复用 `lease_oid_matches` | 待实现（无承接；v6 新增 Agenta CAS 作为外部佐证） |
| P1-5 ref-变更命令记录整库 operation view | B3 | P1 | large | 命令边界封装；与 P2-10 写放大相关 | **已由 plan-20260822 OL-08 / OL-09 承接**；仅余两条细节（branch delete 未接线、序列化命令须坍缩为一个 operation） |
| P1-6 抽出 `merge_commits` 纯原语 | B4 | P1 | medium | （接入 orchestrator 依赖 per-worktree HEAD = D2，**D2 已实现**） | 待实现（`create_tree_from_items_map` 已 `pub(crate)` 并被 stash 复用，剩余工作缩小） |
| P2-7 `--ref-assert` + provenance | C1 | P2 | medium | 与 A2 不重叠（无 ordinal 维度） | 待实现（增益）；须按新的 tag-优先解析顺序设计 |
| P2-8 push 丢路径预检（默认 warn） | C2 | P2 | medium | — | 待实现（增益） |
| P2-9 统一 Preview 信封 + `--assert-preview` | C3 | P2 | large | 与 P1-4 CAS 语义重叠（见下） | **`op restore` 部分已由 plan-20260822 OL-10 承接**；其余命令待实现（增益） |
| P2-10 operation append-only 强制（+可选外锚 Merkle-DAG） | C4 | P2 | medium | 第二步外锚依赖签名/audit-log；与 P1-5 INSERT 兼容 | 待实现（增益，先做便宜版）；目标与 ADR-OL-07 一致，且仓库已有 append-only trigger 先例可照抄 |
| P2-11 部署指针 `env`/`promote`/`@{deploy:}` | D1 | — | medium×3 | (c)依赖(a)，(b)依赖(a)；**先做产品决策** | 阻塞于决策；**须显式对齐 plan-long:520/555 的不采纳边界**（见 §5 P2-11 首段警示框） |
| §7 per-worktree HEAD/index 隔离 | D2 | — | large | ~~与 intentionally-different 设计冲突~~ | **已实现**（plan-20260714 Part C 反转了产品方向并交付 per-worktree HEAD/index 作用域；**本文 D2 提案作废**，与 §7 对应条目一致——v6 修正此前 `rejected` 与 §7 自相矛盾的表述） |

### 6.0.1 提案间交互（落地前必读）

- **P1-5 × P2-10（同表读写）**：P1-5 给所有破坏性命令**新增 `operation` 表 INSERT**；P2-10 第一步加的触发器**只禁 UPDATE/DELETE**，二者兼容。但落地顺序应 P1-5 在前、P2-10 在后，否则 P2-10 的 `fsck` 行数一致性校验会与 P1-5 引入的新写入点相互掩盖回归。若同期开发，二者的迁移须各自幂等、互不假设对方已落。
- **P1-5 × P2-10 写放大叠加**：P1-5 已知 commit 热路径每次写 O(refs) 行；若 P2-10 第二步再给每行算 Merkle 摘要，commit 成本进一步上升。务必先落 P1-5 的 view 去重，再考虑 P2-10 摘要，且摘要只在 `operation`（非每个 view ref）层算。
- **P1-4 × P2-9（两套 CAS）**：P1-4 的 `--expect-head`（断言操作前 ref 状态）与 P2-9 的 `--assert-preview <hash>`（断言整份预览未漂移）是**两个粒度**的乐观并发控制，可共存但不要互相替代——前者轻、面向单 ref 竞争，后者重、面向“检视→应用”闭环。二者失败都复用 `LBR-CONFLICT-002`，details 用不同 key 区分（`expected_oid` vs `preview_digest`）。
- **A2(P0-2) × C1(P2-7)**：`@vN` 句柄由 A2 提供；C1 的 `--ref-assert` **不得**再引入 ordinal 维度，避免两套版本语义并存。**v6 强化**：ordinal 现已由 `libra revision`（1 起，first-parent）实际提供，故这条约束升级为硬约束——**任何新增的 ordinal 面（含 `@vN` revspec）都必须复用 `revision_ordinal` 的 1-based 语义**，不得自建第二套编号。

### 6.0.2 回滚与特性开关矩阵

每个 tracer bullet 须能独立 revert，不留下半套 schema 或悬挂守卫。

| 执行卡 | 回滚方式 | 半落地风险 | 缓解 |
|---|---|---|---|
| A1 | **（v6：roots 半边已实现，回滚项只剩 `op.rs` 预检）** revert `src/command/op.rs` 预检 | op restore 开始拒恢复已 prune 对象（行为变更） | 预检仅 fail-closed，不删数据；文档说明需 `libra maintenance run --task gc` 前先 `op log` 确认 |
| A2 | revert `util.rs` 解析臂 + docs | 无持久状态 | 纯读路径，回滚零迁移 |
| B1 | revert flag + `CommitOutput` 字段 | agent 脚本依赖 `--assert-staged` | flag opt-in；JSON 字段 additive |
| B2 | revert flag | 无 | opt-in |
| B3 | revert wrapper 接线 | DB 中已有 operation 行（无害） | operation 行 append-only；不回滚历史 op |
| B4 | revert 提取的 `merge_commits` | orchestrator 未接则零行为变更 | 第一 PR 不改 orchestrator |
| C4 | `DROP TRIGGER` 迁移 `_down.sql` | 触发器阻止合法维护脚本 | 触发器仅 `operation` 表；维护用官方 `libra op` 路径 |
| D1 | migration `_down.sql` + 删 orphan refs | `refs/libra/deploy/*` 残留 | down 迁移 + `branch -D` 文档化清理 |

**特性开关**：除 D 组外，本文**不引入**全局 config 开关；一切新能力均为 per-invocation flag（`--assert-staged`、`--expect-head` 等），默认行为与 stock git 路径一致。

### A1. P0：operation view 目标纳入 GC roots，并让 `op restore` fail closed

**状态（v6，2026-08-27）：目标的前半已达成**——GC roots 部分**已实现**（落点见下）；**剩余待办只有「`op restore` 缺对象前置 fail-closed」，且已由 `plan-20260822` OL-10 承接**（`plan-20260822.md:1197-1215`）。本卡保留为设计与实现记录，不再作为独立执行卡排期。

**目标**：`libra op log` 列出的每个成功 operation，其 view 里引用的 commit 在 GC 剪枝后仍可恢复；如果历史对象已经缺失，`op restore` 必须在改 HEAD/refs 前失败。
（**v6 命令名更正**：`libra gc --prune=now` **不存在**；GC 的唯一入口是 **`libra maintenance run --task gc`**。）

**实现落点（已实现部分，2026-08-27 核实）**：
- `src/command/maintenance.rs`：`collect_registered_store_roots`（**3308-3441**）新增三行 source 条目——`operation_view_ref.target_oid`（`CellMode::StrictOid`）、`operation_view.head_target` 与 `operation_view_workspace.pointer_value`（均 `CellMode::OidIfParses`）。
- `src/command/maintenance.rs`：`GC_OBJECT_SOURCE_INVENTORY` 三条账本条目（**3081-3090 / 3180-3190 / 3191-3200**）。
- `tests/db_migration_test.rs:**3693-3700**`：把 `operation_view.head_target` 与 `operation_view_workspace.pointer_value` 硬编码为「必须被账本覆盖」的守卫。

**改动范围（剩余部分）**（注：原列出的 `src/command/gc.rs` / `tests/command/gc_test.rs` 已于 v0.17.1759 删除）：
- ~~`src/command/maintenance.rs`（`run_gc`）~~ — **已完成**
- `src/command/op.rs`（`handle_op_restore`，**387**：加对象存在性预检）
- ~~`tests/command/maintenance_test.rs`~~ — GC roots 侧已由 `tests/db_migration_test.rs` 守卫覆盖
- `tests/command/op_test.rs`
- `docs/commands/op.md`（**v6 顺带发现**：该文档全文零 `gc` 提及，GC roots 已保护 operation view 这一事实未进用户文档）

**实现步骤**（步骤 1-5 已由不同手法完成，保留为设计对照；**只有步骤 6 仍待做**）：
1. ~~在 `gc.rs` 新增 `operation_view_roots<C: ConnectionTrait>(db: &C)`，用 `table_exists()` 守卫~~ → **实际做法**：在 `collect_registered_store_roots` 的 `sources` 表里加三行条目；缺表由 `missing_table(&err)` 分支静默跳过（`maintenance.rs:3433`），无需 `table_exists()`。
2. ~~只收集 `operation_view_ref.target_oid` 与 `operation_view WHERE head_kind = 'detached'` 的 `head_target`~~ → **实际做法**：`operation_view.head_target` **无条件全表扫描**，用 `CellMode::OidIfParses` 让非 hash 值（分支名）静默跳过，**不依赖 `head_kind` 字面量**。
3. ~~不读取 `operation_view_workspace.pointer_value`~~ → **实际做法**：**照样读取**，同样用 `OidIfParses`。等价且更保守——原提案「丢弃 workspace 表安全且无损」的论证正确，但实现选择了保留并宽容解析。
4. ~~`roots.extend(...)` 放在 `agent_checkpoint_roots()` 附近~~ → `agent_checkpoint` 已是同一张 `sources` 表里的一行，三者天然相邻。
5. ~~新增 roots 分类计数~~ → **实际做法**：用 `GC_OBJECT_SOURCE_INVENTORY` 分型账本 + `db_migration_test` 覆盖守卫代替 JSON 计数，未改动 GC 输出 schema（与原提案「避免为可观测性重塑 GC 输出」的意图一致）。
6. **（仍待做，已由 OL-10 承接）** 在 `op restore` 的实际写 ref 事务前增加目标对象存在性预检：view refs 的 `target_oid` 与 detached `head_target` 必须存在且是 commit；失败返回 **`LBR-REPO-003`**（`RepoStateInvalid`，`src/utils/error.rs:196,364`）并带 `missing_oid` / `operation_id` detail 与恢复 hint。**禁止**用 `LBR-REPO-002`——该码保留给 `parse_stored_hash` 等 corruption 路径。

**验收标准**（v6 重写：命令名与 prune 语义已更正）：
- `libra maintenance run --task gc` 不会删除只被 `operation_view_ref.target_oid` 引用的 commit。✅ **已满足**
- `libra maintenance run --task gc` 不会删除只被 detached operation view 的 `head_target`（或 `operation_view_workspace.pointer_value`）引用的 commit。✅ **已满足**
- 三张表的 OID 形列均出现在 `GC_OBJECT_SOURCE_INVENTORY` 中，且被 `tests/db_migration_test.rs` 的覆盖守卫钉住。✅ **已满足**
- 人为删除 operation view 目标对象后，`op restore <op>` 不改 HEAD、不改任何分支、返回结构化错误。❌ **未满足（OL-10）**
- 不新增 `op prune`、不新增 retention 策略。✅ 仍成立
- **注意**：不要按旧验收去写 `--prune=now` 用例——GC 的剪枝门槛是 `PRUNE_GRACE_SECS = 3600` 的 mtime 宽限 + `.libra/gc-prune-candidates.json` 两趟隔离账本（首次判定不可达不删），**没有 `--prune=<date>` 语义可用来强制立即剪枝**；测试须据此构造，而不是假设存在 now-prune 开关。

**测试命令**（v6 重写：`command_test` 下的 `gc_*` target 名基于已删除的 `gc.rs`，不再适用）：
```bash
# GC roots 侧（已实现，现由 schema/账本守卫覆盖）
LIBRA_SKIP_WEB_BUILD=1 cargo test --test db_migration_test
# op restore 缺对象 fail-closed（待实现；OL-10 落地时用其 operation_restore_faults target）
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test op_restore_missing_target
```

### A2. P0：实现 `<ref>@vN` 稳定 first-parent ordinal 句柄

**状态（v6，2026-08-27）：已被替代。** 目标能力（稳定 first-parent ordinal 句柄）已由 `lore.md §1.16` / `libra revision find -n | number | index` 交付，走的是**与本卡相反**的实现路线：`revision_ordinal` / `revision_ordinal_meta` **物化侧表** + 迁移 `2026070301` + 每读同事务 `ensure_fresh`，序号 **1 起（1=根）**。
本卡下述「不建表、不做迁移、0 起」的执行指引**已失效**，保留为历史记录（§8.2 治理规则：不删条目）。
**若仍要补 `<ref>@vN` 这一 revspec 形态**，须按以下三条重写本卡后再执行：① 序号语义**必须 1-based**，直接复用 `revision_ordinal` 查询，不得自建第二套编号；② 解析入口是 `resolve_object_spec_typed`（`util.rs:2124`）/ `first_revision_operator`（`util.rs:1950`，当前裸 `@` 不切分），不是已不存在的 `split_revision_navigation`；③ 与已实现的 `@{N}` reflog selector（`util.rs:1925`）明确区分命名空间。

**目标（历史）**：给 agent 和人一个不随 tip append 漂移的“分支第 N 版”句柄；MVP 不建表、不改 ref writer、不做迁移。

**改动范围**：
- `src/utils/util.rs`
- `src/command/rev_parse.rs`
- `src/command/log.rs`
- `src/command/show.rs`（若当前 show 输出 commit JSON）
- `docs/commands/rev-parse.md`
- `docs/commands/log.md`
- `COMPATIBILITY.md`
- `tests/command/rev_parse_test.rs`
- `tests/command/log_test.rs`

**实现步骤**：
1. 在 `get_commit_base_typed()` 入口增加终端 suffix 解析：`<base>@v<digits>`（**先于** `split_revision_navigation` 的 `~`/`^` 切分，见 §0.3）。`base` 不能为空；`N` 必须是十进制非负整数。
2. 解析 `<base>` 得到 tip 后，沿 first-parent 到根计算深度 `D`，再后退 `D - N` 步。`N > D` 返回 `InvalidReference`，错误文本点名 requested ordinal 与 max ordinal。
3. 先只定义 first-parent 语义；merge commit 的 second parent 不参与 ordinal。
4. JSON 输出增加 `ordinal` 时必须保持向后兼容：新增字段，不重命名已有字段；建议同时输出 `ordinal_parent: "first"`（见 §0.3 开放问题 1）。
5. `COMPATIBILITY.md` 标为 Libra intentionally-different revspec，不宣称 Git 兼容；注明与 git `@{n}` reflog 语法、`@{upstream}` 未实现语法的命名空间隔离。

**验收标准**（⚠️ **v6：以下 0-based 示例已失效**——`libra revision` 已把根定为 **1**，重写本卡时须整体改为 `main@v1` = 根）：
- ~~`main@v0` 解析到 first-parent 根提交~~ → 应为 `main@v1` 解析到 first-parent 根提交。
- 在 `main` append 新提交后，旧的 `main@v1` 仍解析到同一提交（此条在 1-based 下依然成立，只是 `v1` 现在指根）。
- `main@v999` 返回结构化 invalid target，不 fallback 到 tag/hash 搜索。
- `feature@vN~1` 这类组合要么明确支持并测试，要么在文档中声明 MVP 仅支持终端 `<ref>@vN`。

**测试命令**：
```bash
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test rev_parse_ordinal
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test log_ordinal
```

### B1. P1：`commit --assert-staged` 暂存区声明校验

**目标**：agent 在 commit 前声明它准备提交的 staged set；真实 index 与声明不一致时，commit 必须拒绝并点名差异路径。

**改动范围**：
- `src/command/commit.rs`
- `src/command/status.rs`（仅在需要复用 staged diff 类型时改）
- `src/utils/error.rs` 与 `docs/error-codes.md`（仅当决定新增错误码）
- `docs/commands/commit.md`
- `tests/command/commit_test.rs`
- `tests/command/cli_error_test.rs` 或既有 JSON error 测试位置

**MVP 接口**：
```text
libra commit --assert-staged <manifest> -m "..."
libra commit --assert-staged - -m "..." < manifest.ndjson
```

manifest 使用 NDJSON，第一版只需要支持以下字段：
```json
{"path":"src/lib.rs","status":"modified","oid":"<blob-oid>"}
{"path":"old.txt","status":"deleted","oid":null}
```

**实现步骤**：
1. 给 `CommitArgs` 加 `assert_staged: Option<String>`；该 struct 已有 `Default`，新增字段不会打爆所有字面量构造。
2. 在 `run_commit` 中 `Index::load(path::index())` 后、`create_tree(&index, ...)` 前做校验；校验必须使用同一个已加载 `Index` 快照（新增 `changes_to_be_committed_from_index(&index)` 或内联 staged-vs-HEAD diff，**禁止**再调 `changes_to_be_committed_safe()`，见 §0.3）。
3. staged path set 来自上述 in-memory diff；blob oid 从 `index.get(path, 0)` 取。manifest 路径规范化，拒绝 `..` 与 worktree 外路径。
4. manifest parser 限制单行长度、总字节数、总条目数；拒绝重复 path；`-` stdin 与文件输入共享同一限流逻辑。
5. 不一致时返回 `LBR-CONFLICT-002` + details：`unexpected_staged`、`missing_from_index`、`oid_mismatch`。若后续决定新增专用码，命名必须是数字式 `LBR-STAGE-001`，并同步 error-code 文档。
6. 成功时在 JSON `CommitOutput` 中新增 `asserted_staged` 回显，包含 normalized manifest 与 matched count。
7. **dry-run + `-a`**：断言必须在 index 快照写回（`commit.rs:592-594`）之前执行。

**验收标准**：
- manifest 缺少一个 staged path → commit 拒绝，HEAD 不变。
- index 多出一个 manifest 未声明 path → commit 拒绝，HEAD 不变。
- manifest oid 与 index oid 不同 → commit 拒绝并点名 path。
- manifest 含 `../x`、repo 外路径、重复 path、超限输入时拒绝且 HEAD/index 不变。
- `--assert-staged` 与 `-a` 的顺序被文档化：断言发生在 `-a` auto-stage 之后、dry-run index 写回之前。
- `--dry-run --assert-staged`（含 `-a`）只预览，不写 index，不写 commit；`-a` + dry-run 组合须单独测试（§0.3）。

**测试命令**：
```bash
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test commit_assert_staged
LIBRA_SKIP_WEB_BUILD=1 cargo test --test compat_error_codes_doc_sync
```

### B2. P1：ref 级 CAS：`--expect-head` / `--expect-branch`

**目标**：让会移动 HEAD/branch 的命令可以声明“我看到的是这个 ref 状态”；状态漂移时，在同一个 SQLite ref 更新事务内拒绝。

**第一批只做两个命令**：
- `commit --expect-head <oid> [--expect-branch <name>]`
- `reset --expect-head <oid> [--expect-branch <name>] <target>`

**改动范围**：
- `src/command/commit.rs`
- `src/command/reset.rs`
- `src/command/push.rs`（若把 `lease_oid_matches` 移到共享 helper）
- `src/internal/branch.rs` / `src/internal/head.rs`（如果需要带 expected 的 update helper）
- `docs/commands/commit.md`
- `docs/commands/reset.md`
- `tests/command/commit_test.rs`
- `tests/command/reset_test.rs`

**实现步骤**：
1. 抽出本地可复用的 abbreviated-OID 比较 helper，语义与 `push.rs::lease_oid_matches` 一致；不要复用 `validate_force_with_lease`，它读的是远端 advertised OID。
2. 在 `commit` 的 `update_head_and_reflog` 事务内部读取当前 HEAD / branch tip，比较 `--expect-head`；不符则 rollback。
3. `--expect-branch <name>` 只校验当前 HEAD 是否位于该 branch；detached HEAD 下必失败。
4. `reset` 在现有 `with_reflog` 闭包内同样校验，避免“校验后状态又漂移”的 TOCTOU。
5. 注意 commit reflog 的 `old_oid` 现在在事务外计算；实现 CAS 时应把 old_oid 捕获移入事务，或在事务内重新校验并用实际 old_oid 写 reflog，避免失败/竞态时 reflog 与 ref 不一致。

**验收标准**：
- 正确 expected oid 时命令行为与现状一致。
- HEAD 漂移后命令返回 `LBR-CONFLICT-002`，HEAD/branch/reflog 都不变。
- abbreviated expected OID 可匹配完整 OID。
- `--expect-branch main` 在 detached HEAD 下拒绝。

**测试命令**：
```bash
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test commit_expect_head
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test reset_expect_head
```

### B3. P1：把 operation view 接线到 ref 变更命令

**目标**：让破坏性 ref 变更能被 `op restore` 作为整库状态恢复，而不只依赖单 ref reflog。

**建议分三批**：
1. `reset`：收益最大、边界最清晰。
2. `commit` / `merge`：高频路径，需处理写放大。
3. `rebase` / `cherry-pick` / `revert`：序列化命令，必须在命令边界记录一个 operation，不要每个内部 reflog 写点都记录。

**改动范围（第一批 reset）**：
- `src/internal/operation_wrapper.rs`
- `src/internal/reflog.rs`
- `src/command/reset.rs`
- `tests/command/op_test.rs`
- `tests/command/reset_test.rs`
- `docs/commands/op.md`
- `docs/commands/reset.md`

**实现步骤**：
1. 新增组合 helper：在一个 SQLite transaction 内执行业务 ref update、写 reflog、写 operation view。不要嵌套 `with_reflog` 和 `with_operation_log` 两个各自开 transaction 的 helper。
2. 第一批只包 `reset`；成功 reset 后 `op log --json` 必须出现 `command_name = "reset"` 或明确约定的命令名。
3. `op restore` 到 reset 前 operation 后，HEAD/branch set 必须回到 reset 前状态。
4. 做写放大评估：记录 refs 数量、operation_view_ref 行数；若 refs 集完全相同，后续再做 view 去重，不在第一批引入。

**验收标准**：
- `reset --hard HEAD~1` 记录 operation。
- `op restore <before-reset-op>` 能恢复 reset 前 branch tip。
- reset 失败时不写 operation、不写 reflog。
- 不改变 `op restore --dry-run` 现有人类输出，除非同时补 JSON 且保持兼容。

**测试命令**：
```bash
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test op_restore_reset
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test reset_operation_log
```

### B4. P1：抽出提交级三方合并原语 `merge_commits`

**目标**：把 `merge.rs` 里已存在的对象图合并核抽成 worktree/HEAD/index 无关的函数，为 AI task merge-back 替换文本级 `diffy` 合并打地基。

**改动范围**：
- `src/command/merge.rs` 或新建 `src/internal/merge_tree.rs`
- `src/internal/ai/orchestrator/workspace.rs`（只在后续 PR 接入；本卡可先不改）
- `tests/command/merge_test.rs` 或新增内部单测

**实现步骤**：
1. 将 `merge_tree_items` + `create_tree_from_items_map` 包装为公开到 crate 内部的 `merge_commits(base, ours, theirs) -> { tree_id, conflicts }`。
2. 函数不能读写工作树、不能读写 index、不能移动 HEAD。
3. 复用现有冲突结构；如果当前结构绑定 CLI 文案，先抽纯数据结构。
4. 第一 PR 只提取并加单测，不改 orchestrator 行为；第二 PR 再替换 `sync_task_worktree_back` 的文本 merge fallback。

**验收标准**：
- clean three-way 返回新 tree id。
- 二进制/模式冲突按现有 merge 行为返回 conflict，不静默覆盖。
- 单测覆盖 add/add、modify/delete、mode-preserve 至少三类。

**测试命令**：
```bash
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test merge_commits
```

### C 组：排在 P0/P1 后的增益项

**C1. `--ref-assert` + provenance 回显**：只在 `rev-parse` / `show` 这类读命令先做。MVP 校验 oid/branch/tag 是否互相指向同一 commit，失败用 `LBR-CONFLICT-002` + 字段 detail；仅当需要独立 Ref category 时再新增 `LBR-REF-001` 并同步 `docs/error-codes.md`。不要引入 ordinal 维度；`<ref>@vN` 已由 A2 覆盖。

**C2. push 丢路径预检**：只在 push 前做 warn，不在 merge/rebase/pull 内做 abort。默认 warn，配置项或 `--allow-deleted-paths` 覆盖。验收必须包含“正常删除文件并 push 不被默认阻断”。

**C3. 统一 Preview 信封**：先接 `op restore --json --dry-run`、`switch --dry-run`、`reset --dry-run`。新增 `preview` 字段，不重命名已有 dry-run 输出。`--assert-preview <hash>` 只在能原子重算+应用的命令上启用；digest 用 canonical JSON + schema version 计算，失败统一走 `LBR-CONFLICT-002`。

**C4. operation append-only 触发器**：先做 SQLite trigger 禁止 `operation` UPDATE/DELETE，并在 `fsck` 报不可验证/被修改状态。Merkle-DAG 摘要需要外锚，否则同 DB 攻击者可以重算整链；不要先上没有安全边界的线性 hash chain。

### D 组：先决策，后实现

**D1. `libra env` / `libra promote` / `@{deploy:<env>}`**：这是 release/CD 产品面，不是 Git 兼容修复。投入前必须先确认 Libra 是否要拥有“哪个 commit 在哪个环境上线”这一职责。若确认要做，先只落 `env list|set|show` 和 `@{deploy:<env>}` 解析；`promote` 复用 reflog/op restore，不新建独立 deployment log。

**D2. per-worktree HEAD/index 隔离**：**已实现，本提案作废**（v6 修正此前与 §7、§6.0 矩阵自相矛盾的 `rejected` 表述）。下述反对意见按 2026-07 基线写成，**已被 plan-20260714 Part C 推翻**——维护者确实反转了产品方向并交付了 per-worktree HEAD/index 作用域（`reference` 表带 `worktree_id` 列，每个 worktree 解析自己的 HEAD 行；checkout 互斥守卫由 `Head::branch_checked_out_elsewhere_result` 提供，在 `op restore` 路径实际被调用——`src/command/op.rs:566-576`，fail-closed 到 `RepoCorrupt`/`ConflictOperationBlocked`）。
原评估（历史记录）：当前与 Libra 已文档化的 intentionally-different worktree 设计冲突；除非维护者明确决定反转产品方向，否则本文件不建议实现；所有依赖它的 fork/worktree 隔离提案继续保持 rejected。
**对其它卡的影响**：B4（P1-6）注记里「接入 orchestrator 依赖 per-worktree HEAD = D2」这一前置依赖**已解除**，但 `src/command/fork.rs` 仍不存在，故 §5 P1-6 的「双重依赖」现在只剩一重。

---

## 7. 已排除项

以下提案经源码验证后剔除，附一句原因（含可回收的微小残值）：

- **CI 强制的 ref 解析文法 + 多义即类型错误** — *already-implemented*（判定不变）。解析优先级已在代码注释中文档化并作为单一真源实现；OID 前缀多义已报 `ambiguous argument`；`log A..B hangs` 动机已于 v1383 修复。
  **v6 事实更正（原句有误，2026-08-27）**：优先级**不是**「HEAD>本地分支>远程>tag>OID 前缀」，**分支也不严格胜过同名 tag**——现行顺序是 **HEAD → tag → 本地分支 → 远程跟踪 → OID 前缀**，源码注释写明 `// Git's short-ref precedence checks tags before local and remote branches.`（`src/utils/util.rs:**1797**`；tier 注释现在 `util.rs:**2145-2152**`，v5 记 993-1001）。
  **唯一真残值（仍成立）**：多段远程跟踪名（`a/b/c` 的 `(a,b/c)` vs `(a/b,c)` 切分）首匹配静默选取——现落在 `resolve_remote_branch_atom_typed`（`util.rs:**1684-1712**`，v5 记 862-890），经 `remote_tracking_candidates`（`util.rs:1538`）顺序取首个命中。可复用现有 `CommitBaseError` 多义惯用法返回类型化错误，无需新文法/CI 测试。

- **`libra fork`（原子隔离 agent 分支+worktree）** — *infeasible*（判定不变；**v6 复核：`src/command/fork.rs` 仍不存在**）。“O(1) 在 commit X 建分支、对象共享、headless/JSON” 即今天的 `libra branch <name> <rev> --json`（现 `create_branch_impl` `src/command/branch.rs:**1453**`、`create_branch` **2343**、`create_branch_safe` **2354**；v5 记 111-118。内部单 ref 行 INSERT 无拷贝），Agenta “修 O(n)→O(1)” 框架描述的正是 Libra 现状；其定义性价值（带独立 HEAD 的隔离 worktree）依赖不存在的 per-worktree HEAD；分支在 SQLite、worktree 在 worktrees.json，无法单事务原子；无 ephemeral/created_by 列可标记/回收。

- **checkout 互斥守卫（branch already checked out elsewhere）** — *已实现*（本条的 2026-07 评估基线「所有 worktree 共享一个 HEAD」已被 plan-20260714 Part C W0 推翻：`reference` 表带 `worktree_id` 列，每个 worktree 解析自己的 HEAD 行）。守卫由 `Head::branch_checked_out_elsewhere_result` 单一 fail-closed 探针提供，接入全部 shared-branch mutator 与 current-branch ref writer；`checkout --ignore-other-worktrees` 不再绕过该守卫（intentionally-different，见 ADR-0714-09），冲突返回 `LBR-CONFLICT-002`。**唯一残值**：把并发 branch-create 竞争失败的裸 sea-orm integrity error 也映射到 `LBR-CONFLICT-002`（呼应 Agenta“传播冲突而非吞掉”）——小而真，无需新码/新机制；丢弃 `SELECT..FOR UPDATE` 类比（SQLite 无行锁，busy 重试已是等价物）。

- **受保护（guarded）环境** — *infeasible*（且依赖同样不存在的 env 子系统）。`promote.rs`/`env.rs`/`deployment` 表/`guarded` 标志全不存在；权限模型无强弱层级，无 "DEPLOY > EDIT" 表示；引用的 `compat_error_codes_doc_sync` 测试名也不存在。**唯一残值**：AI 发起的晋升走现有 `approved_permission` 流（新字符串键 `promote`）——已并入 P2-11 注意事项。

- **per-worktree HEAD/index 隔离** — *已实现*（本条按 2026-07 基线写成 conflicts-with-principles；plan-20260714 Part C 反转了该产品方向并交付了 per-worktree HEAD/index 作用域，下述反对意见与「共享 HEAD/index 是 intentionally-different」的引用均为历史记录，不再描述当前行为）。原评估：

  技术可建且不破坏 git on-disk 兼容，但与 COMPATIBILITY.md:77/88、`docs/commands/worktree.md`、libra-workflow skill 明文的“共享 HEAD/index/refs 是 intentionally-different，branch-isolated worktree 是反模式、官方替代是独立 clone”直接冲突；且其核心动机是误诊（被引“并发暂存竞争”是同目录共享工作树竞争，per-worktree 作用域修不了）。**不作为改进建议**，仅作为“若维护者主动反转产品方向”时的前置能力（见路线图），届时须按“从规范 worktree 路径确定性派生 worktree_id、保 `path::index()` 为纯同步函数、保持 Branch/Tag 行共享、丢弃 worktrees.json→SQLite 正规化”收窄实现，并预算 ~124 `Head::current*` + 36 `Head::update*` + 64 `path::index()` 站点的真实爆炸半径与文档反转。

---

## 8. 文档维护约定

1. **再核验触发器**：下列任一发生即重跑 §0.2 级锚点核验并更新最新一节核验记录（当前为 §0.6）：`collect_registered_store_roots` / `GC_OBJECT_SOURCE_INVENTORY`（v6 更新：`collect_roots_from_database` 已不存在）、`with_operation_log`、`resolve_object_atom_typed` / `get_commit_base_typed` / `first_revision_operator`（v6 更新：`resolve_commit_base_atom_typed` 与 `split_revision_navigation` 已不存在）、`run_commit_with_index` / `update_head_and_reflog`、`merge_tree_items` / `create_tree_from_items_map`、`is_locked_revision`、`StableErrorCode` 枚举变更、**`revision_ordinal` 相关表与 `libra revision` 命令面**（v6 新增）、**`plan-20260822` 的 OL-* 卡状态变化**（v6 新增：本文多条已由其承接）。
2. **版本号**：结构性修订（新增执行卡、改变默认行为描述、错误码策略变更、**结论翻转或状态列批量变更**）递增文档版本；纯行号漂移修正只更新最新核验记录表格，不 bump 版本。**v6 即依此 bump**（2 处结论推翻 + 1 处设计判断否决 + 竞品能力反转 + §6.0 状态列批量改写）。
   **编号治理（v6 明确）**：任务卡/建议编号（`P0-*`/`P1-*`/`P2-*`、`A1/A2/B1..B4/C1..C4/D1/D2`）**永不重新编号、永不删除**；被现实推翻的条目一律保留编号并加「已实现 / 已被替代 / 已由 <计划><卡号> 承接 / 不采纳」标注，并写明理由与证据。
3. **Issue 链接**：每个 §6 执行卡落地时，在追踪矩阵“状态”列链接 PR/issue，避免 §5 长文与实现分叉。
4. **Agenta 侧路径**：`/Volumes/Data/competition/agenta-ai/agenta/...` 为撰写时本地路径（v6 更正：v5 及更早写的 `/Volumes/Data/agenta-ai/agenta/...` 已失效）；外部分发时改 GitHub 路径或删绝对路径，保留模块名（`core/git/types.py`、`dbs/postgres/git/dao.py` 等）即可。
   **⚠️ 竞品仓库只读**：该 checkout 是竞品参考仓库，**禁止在其中执行任何写操作**（不 checkout、不 pull、不写文件）；核验一律用只读命令。引用其结论时须同时标注快照 revision 与 `plan-long` 记录的审计状态（当前 `53717db…` / `blocked-timeout`，见 §0 与 §0.6）。
5. **测试索引**：新增 `tests/command/*` 或 compat 守卫时，同步 `tests/INDEX.md` 一行描述（Wave 1 缺省），并在对应执行卡“测试命令”段引用 `<target>::<fn>` 格式。
