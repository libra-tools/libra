# Agenta 版本管理方案剖析与对 Libra 的改进建议

> ⚠️ **校正须知（v0.17.1759）**：本文件多处把 `src/command/gc.rs`（及 `collect_roots_from_database`、`agent_checkpoint_roots`、`gc.rs:1260-1297`/`gc.rs:1709` 等符号/行号）当作 GC 的现行实现来分析。**事实更正**：那份 `src/command/gc.rs` 是**从未在任何 `mod.rs`/`cli.rs` 声明、从未编译进二进制的孤立死代码**，已于 v0.17.1759 删除。唯一被编译、会运行的 GC 实现是 `src/command/maintenance.rs::run_gc`，其 roots 模型不同——它用 `collect_reachable_objects(&storage)` + `list_loose_objects()` 做可达性扫描，**不读 database roots、不含 `agent_checkpoint_roots`、也不含 operation_view roots**。因此本文 §A1（“op restore 静默腐烂”）的分析与改动范围是针对那份死代码得出的，**落地前必须先对照 `maintenance.rs::run_gc` 的实际 roots 来源重新核验**，下文所有 `gc.rs` 引用按历史快照处理。

> 读者：Libra（AI-agent-native、Git 兼容、refs 存于 SQLite 的版本控制系统）维护者
> 方法：先剖析 Agenta 的版本管理本质，再逐条对照 Libra 源码验证，只保留可落地的建议。所有 Libra 侧结论均已对照源码核验。

**文档元信息**
- 状态：草案（design proposal）。A/B/C 组可按本文件拆小 PR 进入实现队列；D 组仍等待维护者做产品边界决策。
- 文档版本：v5（含 2026-06-28 源码行号校正、Agenta DAO 深度复核与 §2.a 证据补强）。
- 最近一次源码核验：2026-06-28，对照工作树 HEAD（`libra rev-parse HEAD`）；核验记录见 §0.2（首轮）与 §0.3（二次交叉验证）。
- **锚点漂移须知**：文中所有 `file:line` 均为“符号优先、行号为提示”的引用。源文件持续演进，行号会漂移（本次核验即发现多处，已记入 §0.2）。落地前请以**函数/符号名**重新定位，不要信任行号；若做长期引用，请 pin 到具体提交哈希而非行号。
- **§5 与 §6 分工**：§5 保留动机、取舍与风险；§6 只写可拆 issue/PR 的执行增量。**若两处描述冲突，以 §6 验收标准与 §0.3 二次验证为准**；§5 中的 rationale 不在 §6 重复，避免双份漂移。

---

## 0. 落地状态与执行口径

**本版结论：可以直接进入实现队列，但必须按“可回滚小 PR”落地。** Agenta 给 Libra 的价值不在于复制它的关系型“Artifact / Variant / Revision”数据模型，而在于三条工程纪律：声明状态后校验、历史记录必须可恢复、对 agent 输出稳定的机器可读 provenance。下面的建议已经按 Libra 当前源码重新收窄，优先落成 4 个独立 tracer bullet：operation log GC roots、`<ref>@vN` 稳定句柄、`commit --assert-staged`、ref 级 CAS。

**源码依据（核验范围）**：
- Agenta 版本内核：`/Volumes/Data/agenta-ai/agenta/api/oss/src/core/git/types.py:1-381`、`dtos.py:24-146`、`dbs/postgres/git/dao.py:881-1001,1110-1168,1564-1668,1802-1844`。
- Agenta environment 指针：`api/oss/src/core/environments/dtos.py:118-150`、`service.py:120-182,184-258`。
- Agenta 仓库工作流反例：`AGENTS.md:31-66`、`.pre-commit-config.yaml:28-37`、`.husky/pre-push`、`.github/workflows/01-create-release-branch.yml:57-189`。
- Libra 对照点：`src/command/maintenance.rs::run_gc`（GC reachability/expire 实现；孤立的 `src/command/gc.rs` 已于 v0.17.1759 删除）、`src/internal/operation.rs:501-683`、`src/internal/operation_wrapper.rs:317-535`（`with_operation_log` 实际跨至 535，原 v1 写作 317-430）、`src/utils/util.rs:739-990`、`src/command/commit.rs:562-611,1899-1915`、`src/command/push.rs:1478-1525`、`src/command/reset.rs:770-790`、`src/command/merge.rs:657-687`、`src/internal/ai/orchestrator/workspace.rs:1032-1115`。

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
| 可靠性 / 容错性 | 成立。总体策略是先校验、再写入、失败不移动 refs。 | 每个失败验收都必须断言 HEAD、branch、reflog、operation 表“不变”；涉及 GC 的测试必须覆盖 `--prune=now`。 |
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

---

## 1. 一句话结论

**Agenta 的版本管理本质是“把 Git 搬进关系型数据库的一个三层不可变内核”**——Artifact（仓库）→ Variant（分支）→ Revision（不可变提交），用单一 `Reference(id, slug, version)` 值对象 + 一套纯函数解析代数（充分性 / 冗余一致性 / 不一致三态校验）寻址任意一次提交，并把 environment 建模成指向 revision 的可移动指针。它不是真正的 DAG/merge 系统，而是“快照 + 指针 + 强约定 + 机器可读错误”的工程化产物。

对 Libra 最有价值的 3 个借鉴点：

1. **声明—校验契约（declare-then-verify）**：让 agent 主动多写它相信的状态（HEAD/staged/refs），VCS 在不一致时报出“哪个字段对不上”的带类型错误，而不是静默尽力解析。这正是 AI agent 跨轮次持有陈旧信念时最需要的护栏，且能直接套到 Libra 的 `commit --assert-staged`、`--expect-head` 等 CAS 前置条件上。
2. **不可变历史 = 审计日志**：Agenta 每次部署都 append 一条完整 `environment_revision`，历史本身即审计。Libra 已有 jj 式 operation log，但它的目标对象会被 GC 回收、且只有 4 个写入点——把“不可变可恢复”这个保证补齐，是当前最高性价比的完整性修复。
3. **稳定、可读、机器可寻址的句柄**：Agenta 的 per-variant 单调版本号给了“分支内第 N 次提交”一个稳定名字。Libra 只有不透明 OID 和会随 tip 前移而改变的 `~N`，缺一个 `<ref>@vN`（从根计数）这样不随 append 漂移的句柄。

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
- `_get_version`（`dbs/postgres/git/dao.py:1802`）= `COUNT(同 variant 内 id < 本 revision.id 的行)`，0 起始字符串存回。version '0' 是分支空根（data/flags/tags/meta 置空）。
- **已知缺陷（v5 深度复核升级）**：`commit_revision` 的 INSERT 在 T1 提交并释放 `FOR UPDATE` 锁 → `_get_version` 在 **T2 独立 session** COUNT → `_set_version` 在 **T3 独立 session** 无条件 UPDATE——三个独立事务、**无回滚边界**：T2/T3 失败则 revision 行已提交但 version 为 NULL/陈旧。`_set_version` 无 `WHERE version IS NULL` 守卫、无 affected-row 检查、无条件覆写。非初始提交不加锁。产线上出现过重复/缺失版本号，需要专门修复迁移 `b3c4d5e6f7a9`。这是 Agenta 自己踩的坑。

**不可变 + fork 语义**
- commit = INSERT 新行；`edit_revision` 只能改描述性元数据（name/description/flags/tags/meta），**永不动 data/message/version/author**。
- `fork_variant`（`dao.py:882`）= 在同一 artifact 下建新 variant 并**逐条深拷贝整段历史**（O(n) 写、内联 data 重复），每个 slug 加 `_<target_variant.id.hex>` 后缀避免冲突。每条拷贝各调一次 `commit_revision`，即各跑 3-session 往返（INSERT + COUNT + UPDATE），fork 总成本 = O(n) × 3 sessions × 3 网络 RTT。**无共享 DAG、无 merge-base**，分支靠复制而非引用共同祖先——Agenta 自己也将此列为局限。

**environment 作为部署指针（`core/environments/`）**
- environment 也是普通 git artifact，但其 revision 的 `data` 不是配置而是 `references: Dict[key, Dict[entity_type, Reference]]` 部署清单，存完整血缘三元组。每次部署/晋升 = commit 一条新 environment_revision（delta set/remove → 物化成完整快照），**历史即部署审计日志**。但事件发布（`publish_revision_event`）在 DB commit **之后** best-effort 调用、**无 transactional outbox**——crash 在 commit 与 publish 之间静默丢事件，审计完整性有缺口。
- 解析“线上跑的是什么” = `environment_ref + key` 两跳间接寻址；commit 时发 `state + diff(created/updated/deleted)` 结构化事件；`is_guarded` + `DEPLOY_ENVIRONMENTS` 做受保护环境闸门。**v5 修正**：`is_guarded` 在 **OSS 版为 no-op**（`ensure_environment_deploy_allowed` 在 `not is_ee()` 时直接 return）；`DEPLOY_ENVIRONMENTS` 与 `EDIT_ENVIRONMENTS` 是**同级 sibling 权限**（在 DEVELOPER 角色同时授予），非严格层级——"比 EDIT 更强的授权"仅在 EE 版成立。
- 局限：跨环境晋升不是单一原子操作（客户端拼装）；delta 路径“读最新→重提交”无 compare-and-set，并发部署可能竞争（v5 深度复核确认：`commit_environment_revision` 全程无锁、无 expected-version 参数、`RevisionCommit` DTO 不携带版本字段）。

**异常 → HTTP 注册表（`apis/fastapi/git/exceptions.py`）**
- 所有 git 域错误派生自 `GitError`，单个装饰器 `handle_git_exceptions` 把它们映射到稳定 HTTP 码：InitialRevisionConflict→409、VariantForkError/Insufficient/Inconsistent/InlineResolveInvalid→400。明文契约：**新增域异常必须同时在域层和传输层注册**。

**并发安全的分支根（v5 收窄）**：**仅** `commit_revision(initial=True)` 用 `SELECT ... FOR UPDATE` 锁住 variant 行 + COUNT 守卫，保证每分支至多一个初始 revision，冲突精确抛 409。**非初始提交无锁、无 expected-version 检查**——版本号靠 post-hoc COUNT 分配，并发提交可重复/缺失。

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
- **agent-native I/O 契约**：`--json[=pretty|compact|ndjson]`/`--machine` 统一 `{ok,command,data}` 信封；闭合的 `LBR-<DOMAIN>-NNN` 错误码枚举 + 固定 category + Git 风格 exit code（128/129），增删码有 `compat_error_codes_doc_sync` 守卫。
- **jj 式 operation log**：`operation/operation_view/operation_view_ref/operation_view_workspace` 五表 + `libra op log|show|restore`，已能重建 HEAD+所有分支、剪枝、保护锁定分支。
- **锁定/AI 托管分支**：main/intent/traces（旧名 agent-traces）由 `is_locked_branch`/`is_locked_revision`（连 `traces~1`/`intent^`/`@{0}` 后缀也守）跨 reset/restore/switch/checkout/branch/op 一致拒绝。
- **解析优先级已实现且已文档化（代码注释）**：`resolve_commit_base_atom_typed`（`src/utils/util.rs:836`）按 HEAD > 本地分支 > 远程跟踪 > tag > OID 前缀的确定性 tier 解析，单一真源；分支严格胜过同名 tag；OID 前缀多义已返回 `InvalidReference('ambiguous argument')`。
- **CAS 先例**：`push --force-with-lease`（`push.rs:1480` `lease_oid_matches`）已证明“声明期望状态、漂移即拒”模式；失败映射到 `LBR-CONFLICT-002`。
- **GC roots 保护先例**：`agent_checkpoint_roots`（`gc.rs:1709`）已保护一类 history-as-data。
- **dry-run 多处存在**（commit/op restore/checkpoint rewind/automation），但各自形状不同。
- **A..B 区间解析**已在 v1383 实现（曾经的 `log A..B hangs` 已修）。

---

## 4. 关键差异与可借鉴点

| 维度 | Agenta 怎么做 | Libra 现状 | 启发 / 取舍 |
|---|---|---|---|
| 提交寻址 | `Reference(id,slug,version)` 三模值对象 + 纯函数解析代数 | 单字符串 token 解析（HEAD/分支/远程/tag/hash） | 借“声明—校验”思想，**不照搬** version 维度（见下行） |
| 版本号 | per-variant 0 起单调序号，**离开分支无意义** | 仅 OID 与“随 tip 漂移”的 `~N`，无稳定可读句柄 | 引入 `<ref>@vN`（从根 first-parent 深度，0=根），**按需 DAG 计算，不建表** |
| 欠定/矛盾输入 | Insufficient vs Inconsistent 两类带类型错误，点名字段 | 静默尽力解析；冗余多写无校验（当前命令也不收“标识符袋”） | 仅取“冗余一致性校验 + provenance 回显”，作 opt-in 断言 |
| 不可变性 | revision append-only，配置载荷永不可变，历史即审计 | operation log 不可变但**目标对象会被 GC 回收**、写入点仅 2 处 | 把 op log 的“可恢复”补成真保证（GC roots + 全命令覆盖） |
| 默认/tip 选取 | 确定性 tie-break，拒绝返回任意行 | 同样确定性；OID 前缀多义已报错 | 已对齐，**仅剩**多段远程跟踪名首匹配的静默选取 |
| 并发分支根 | `SELECT FOR UPDATE` + COUNT → 精确 409 | 事务 + 唯一索引 + busy 重试（SQLite 无行锁） | 仅需把“竞争失败的裸 integrity error”映射成 `LBR-CONFLICT-002` |
| environment | 一等可移动部署指针，自描述血缘，delta→快照 | **无此概念**；reference 表 `kind` 不含部署类 | 可借鉴，但属 CD/发布管理，需先与维护者确认是否在 Libra 边界内 |
| 晋升 | commit 新 environment_revision，发 state+diff 事件 | 无；reflog 已含 action/message/committer | 用 reflog + op restore 复用，仅补 CAS 守卫 |
| fork | **深拷贝整段历史**（O(n)，自承缺陷） | `libra branch <name> <rev>` 已是 O(1) ref 行 INSERT、对象共享 | Libra 在此**已优于** Agenta，勿照搬其拷贝式 fork |
| 异常→码 | 单装饰器域→HTTP 注册表，明文双注册契约 | 已有闭合 LBR 枚举 + doc-sync | 已对齐 |

---

## 5. 对 Libra 的改进建议（核心）

按价值与确定性排序。每条注明 P0/P1/P2、验证结论、置信度。所有“具体怎么改”均落到已核验的文件/函数/表。

---

### P0-1. 让 operation log 可恢复：把 operation-view 目标纳入 GC roots（修正版）

**验证结论：viable-with-caveats，置信度高。**

- **问题**：`op log`/`op restore` 是 Libra 的招牌恢复功能，但 `collect_roots_from_database`（`gc.rs:1260-1297`）从 references/reflog/stash/index/rebase 等播种 roots，**唯独不读任何 operation_view* 表**。operation 行永久保留（`operation.rs` 无 retention），但其目标 commit 仅靠 reflog 保护，而 GC 内部 reflog 90/30 天就过期、之后 loose object 超过 `DEFAULT_PRUNE="2.weeks.ago"` 被剪。结果：`op restore @{N}` 到更早操作会落到缺失对象，**招牌恢复功能静默腐烂**。`agent_checkpoint_roots`（`gc.rs:1709`）恰好是现成模板——团队保护了 checkpoint，却漏了 op log 本身。
- **借鉴 Agenta 什么**：append-only revision 永不物理删除，任何历史 Reference 总能解析；把这条不可变保证补到 Libra 的恢复日志上。
- **具体怎么改**（关键：必须修正提案的字段集，否则会**搞坏 GC**）：
  - 新增 `operation_view_roots(db)`，仿 `agent_checkpoint_roots`：
    1. `SELECT target_oid FROM operation_view_ref`（全行，永远是真 commit OID——`operation_wrapper.rs:653/675`）；
    2. `SELECT head_target FROM operation_view WHERE head_kind = 'detached'`（**只有 detached 时 head_target 才是 OID**；分支态 head_target 是分支名，已被其 ref 行覆盖）；
    3. **丢弃 `operation_view_workspace`**——已核验 `pointer_value = head_target.clone()`（`operation_wrapper.rs:689`）：在分支态它是分支名，在 detached 态它**就是那个 OID**。但即便如此，丢弃它仍是**安全且无损**的：detached 态的同一 OID 必然同时写入步骤 2 的 `operation_view.head_target`（`head_kind='detached'`），已被覆盖；分支名则被该分支自身的 ref 行覆盖。所以 workspace 表**不含任何步骤 1/2 未收集的 OID**。（v1 原文“值是分支名，无独有 OID”不精确，会让实现者误以为它永不含 OID。）
  - 用 `table_exists()` 守卫，在 `gc.rs:1295-1296` 处 `roots.extend(operation_view_roots(&db).await?)`。
  - `gc --dry-run --json`/统计输出若已有 roots 分类，增加 `operation_view_roots` 计数；若当前只有总数，至少在测试中断言该类 root 确实进入 roots 集合。这样能避免未来维护者以为 op log 永久保留但对象保护不可见。
  - **`op restore` 加前置存在性校验**：改 HEAD/分支前先验证每个 target_oid（及 detached head_target）对应对象存在，缺失则报 `LBR-REPO-003`（`RepoStateInvalid`，带 `missing_oid`/`operation_id` detail 与指向 `libra gc`/对象恢复的 hint），**不要**用 `LBR-REPO-002`（那是 corruption/非法 hash 语义，与 prune 后的缺对象不同）。
- **收益**：`op log` 列出的每个操作都保证可恢复；纯 SQLite 元数据，**对 git on-disk 兼容零影响**。
- **成本**：small。
- **风险/注意**：
  - 若按原提案直接把 `head_target` 和 `pointer_value` 当 root 喂进 `parse_stored_hash`（fail-closed 于非 hash），会在“HEAD 在分支上”这一常态触发 `RepoCorrupt` 让 GC 中止——**必须按上述字段集收窄**。
  - 原提案的“半恢复 worktree”措辞不准：`op restore` 只改 SQLite 的 HEAD+分支行，真实失败是悬挂 ref。
  - **丢弃原提案的 `op prune` retention**——它与本条引用的不可变原则自相矛盾、会重新制造悬挂指针。若担心无界增长，另立提案做“原子 retention”（删 op 行与其变为不可达的对象一并回收），不要做默认。

---

### P0-2. `<ref>@vN` 稳定版本号句柄（按需 DAG 计算，不建表）

**验证结论：viable-with-caveats，置信度高。核心判断：发功能，砍掉表。**

- **问题**：Libra 只能用不透明 OID 或 git 相对量 `branch~N` 寻址。`~N` 从移动的 tip 反向计数——**每次 tip 前进，同一提交对应的数字就变**。没有稳定、可读、可发音的“本分支第 N 次提交”句柄。reflog 的 `@{n}` 是会过期的相对索引，不是稳定绝对序号。被跨轮次重新唤起的 agent 无法可靠重新 pin 到先前状态，除非记住完整 SHA。（已核验：`util.rs:739-991` 的解析器只处理 `~`/`^`/HEAD/分支/远程/tag/hash，无 `@v` 分支。）
- **借鉴 Agenta 什么**：Revision.version 作为分支内 0 起单调序号——一个稳定的绝对位置句柄。
- **具体怎么改**：
  - 在 `get_commit_base_typed`（`util.rs:978`）**先于** `split_revision_navigation` 增加终端 `@v<digits>` 剥除与解析（见 §0.3）：从解析出的 ref tip 沿 **first-parent** 走到根算出深度 D，再后退 D−N 步（即 `<ref>@vN` ≡ `<ref>~(D-N)`）。`ordinal = 从根的 first-parent 深度` 是 commit DAG 的**纯函数**，复用 `--first-parent`/`rev-list --count` 已有基础设施。N>D / N<0 走现有 `InvalidReference`。
  - `--json` 的 log/show 输出加 `ordinal` 字段（`output.rs` emit/emit_list）。
  - COMPATIBILITY.md 加一行 intentionally-different（“在 git 相对 ~N 之上的稳定绝对版本号”）。
- **收益**：agent 与人得到不随 append 漂移的可发音句柄（“main 的第 42 版”），机器可读，重新唤起的 agent 无需记 SHA 即可重新 pin。
- **成本**：medium（实际约几十行解析器改动）。
- **风险/注意**：
  - **不要建 `branch_revision` 物化表、不要做迁移、不要改 ref-writer 事务**——原提案约 80% 的机制在解决一个数据模型本不存在的问题。Agenta 不变量“version 离开分支无意义”**不迁移**：Agenta variant 不共享 revision，而 Libra 分支**共享** commit，所以从根深度对共享主干是分支无关的，per-branch 主键会把共享主干冗余存 N 遍。
  - rebase/reset 会重排被改写后缀的序号——文档须说明“ordinal 仅在最后一次改写点之前稳定”；锁定的 main/intent/traces 以 append 为主，是稳定情形。
  - `is_locked_revision` 会先剥 `@` 再查锁，故 `main@vN` 按读寻址可解析、按写操作与 `main~1` 一样被拒——可接受，解析器须把 `@vN` 严格当读寻址。
  - **性能注记（不建表的代价）**：每次解析 `<ref>@vN` 都要沿 first-parent 从 tip 回溯到根算 D，是 **O(depth)** 的对象遍历。对锁定的 main（持续 append、历史可达数千提交）若用于热循环/批量解析会有可感成本。MVP 接受此代价（正确性优先、零写放大）；若后续 profiling 显示瓶颈，再考虑**只读缓存** `(tip_oid → depth)`（tip 不变即命中），仍不必落物化表、不碰 ref-writer 事务。

---

### P1-3. `commit --assert-staged`：agent 声明预期暂存内容，漂移即拒

**验证结论：viable-with-caveats，置信度高。核心判断：发 commit 半边，砍掉 add 半边。**

- **问题**：Libra 的 `commit` 总是提交**整个 index、无 pathspec 形式**，叠加共享存储 worktree（一个 index 跨所有 worktree）产生三大已记录踩坑：`commit -a` 曾误删文档；并发 tree 竞争时 `diff --cached` 报空而实际 ~300 文件已暂存、`commit` 静默把别进程的暂存一起带走；安全套路（`restore --staged .` → 只 add 自己的 → 肉眼看 status → commit）手动、易竞争、无法强制。**agent 无法“肉眼看 status”**。
- **借鉴 Agenta 什么**：`validate_retrieve_refs_consistent` 的“声明—校验—点名不符字段”契约 + RetrievalInfo provenance 回显，套到暂存区。
- **具体怎么改**：
  - 给 `CommitArgs`（`commit.rs:81`，注意它处处用 `..Default`，加字段安全）加 `--assert-staged <manifest>`（路径，或 path+blob-oid，`-` 读 NDJSON stdin）。
  - 在 `run_commit` 内 `Index::load`（`commit.rs:562`）与 `create_tree`（611）之间做**只读闸门**：基于**同一**已加载 `Index` 做 staged-vs-HEAD 变更集（勿再调 `changes_to_be_committed_safe()`——它会二次 load index，见 §0.3）；per-path oid 从 `Index::get(path,0)` 取。manifest 路径须规范化并拒绝 repo 外/`../` 穿越。
  - manifest 解析必须有资源上限：限制单行长度、总行数/总字节数，并拒绝重复 path（重复声明应作为 `LBR-CONFLICT-002` 或用法错误处理，不能“最后一行赢”）。`-` stdin 读取也要同样限流，避免 agent/恶意输入把 commit 热路径变成无界内存消耗。
  - 不符时 MVP 复用 `LBR-CONFLICT-002`（staged state 与声明冲突），`details` 用 `with_detail` 分桶 `unexpected_staged / missing_from_index / oid_mismatch`（`error.rs` 已支持）。若后续确需专用码，再新增数字式 `LBR-STAGE-001` 并同步 `docs/error-codes.md`。成功时把解析出的 staged manifest 作为 `CommitOutput` 的附加字段回显（已序列化进 `{ok,command,data}`）。
- **收益**：把头号静默损坏踩坑变成大声、机器可操作的结构化冲突，点名出错路径；agent 获得 compare-and-commit 语义。
- **成本**：medium。
- **风险/注意**：
  - 原提案两处事实错误须纠正：(1) 暂存真源是 **`.libra/index`（git 二进制格式），不是 `.libra/libra.db`**；(2) 比较集是 **staged-vs-HEAD，不是原始 index 全集**（后者含每个被跟踪文件，会逼 agent 声明全部路径）。
  - **砍掉 `add --assert-staged`**：`AddArgs`（`add.rs:67`）有 250 处无 `..Default` 字面量站点（仓库内“#1 BLOCKED”结构），且 `add` 本就接受 pathspec，价值低。
  - 错误 category 重新斟酌：precondition 失败按 Cli/Usage(129) 对 agent 可能比 conflict 更好分支，与 `--force-with-lease` 拒绝的编码对齐。
  - 真正的 `commit -- <pathspec>` 是独立、更大的 git-parity 工程（需 HEAD-tree ∪ 命名 index 项的合成树），**单列**，勿与本条捆绑。

---

### P1-4. ref 级 compare-and-swap：`--expect-head` / `--expect-branch`（诚实收窄版）

**验证结论：viable-with-caveats，置信度高。核心判断：只做 ref 级，砍掉 --expect-tree，并重写动机。**

- **问题**：`push` 已证明 CAS 有用（`--force-with-lease`），但其余每个变更命令（commit/reset/switch/...）都作用于“此刻恰好”的 HEAD，agent 无法断言它相信的操作前提状态。
- **借鉴 Agenta 什么**：冗余一致性校验（多写即校验、点名不符）+ InitialRevisionConflict 的“检查—写入在受控临界区内、报精确冲突”。
- **具体怎么改**：
  - 仅在**真正移动 SQLite ref、且已在 `_with_conn` 事务内更新**的命令上加 `--expect-head <oid>` / `--expect-branch <name>`：先做 commit / reset / switch（最省），rebase/merge 后做（引擎侵入大）。
  - 在**现有 ref 更新事务内**读当前 ref（`commit.rs:1899` `update_head_and_reflog`；`reset.rs:770-786`），不符即中止。因为 HEAD/分支在 `reference` 表、写入经 busy_timeout 串行，这是真正的原子 CAS——也是本提案原子性论证**唯一成立**之处。
  - 仅复用 `lease_oid_matches`（`push.rs:1480`，前缀容忍比较）；**不要“泛化 validate_force_with_lease”**（它校验的是协议广播的远程 oid，与本地 HEAD 读取是不同数据源）。
  - 复用 `LBR-CONFLICT-002`（ConflictOperationBlocked）+ `with_detail("expected_oid"/"actual_oid")`；不要新铸 `LBR-PRECONDITION-*`，当前稳定错误码目录没有这个 domain，且项目惯例是复用冲突码表达 compare-and-swap 失败。
- **收益**：把 push 之外的整个变更面铺上乐观并发控制，多进程 HEAD 竞争可被原子检出。
- **成本**：medium。
- **风险/注意（最重要：重写动机）**：
  - **砍掉 `--expect-tree`**：index（`.libra/index`）与工作树是磁盘文件、在 SQLite 写锁之外，无法成为提案宣称的原子 CAS，且 agent 几乎从不知道期望 tree oid。
  - **诚实重述卖点**：被引用的三起事故（wip-bundle 5 次恢复、rebase 丢 170 文件、并发 ~300 文件）**HEAD 全部正确**，`--expect-head` 都会通过、一个都防不住。本特性只应卖作“多进程 HEAD 竞争保护”（真实但窄）；wip/rebase 退化的真正钱该投向 rebase/cherry-pick 的 tree-rebuild-from-partial-checkout 根因（3-way replay + ref 更新前的 tree-diff 闸门）。
  - 不要一次性铺六个命令（rebase.rs 是 4227 行延期引擎），违反仓库有界切片规范。

---

### P1-5. 给每个 ref-变更命令记录整库 operation view（原子、完整 undo）

**验证结论：viable-with-caveats，置信度高。核心判断：这是接线，不是新建。**

- **问题**：`with_operation_log` 只接到 **2 处**（branch create、op restore），且 branch **delete 未覆盖**。用户/agent 最需要 undo 的破坏性命令（reset/rebase/merge/commit/switch/cherry-pick/revert）走的是 `with_reflog`（原子更新单 ref + reflog 行）但**从不记录整库 operation view**。于是 `op restore` 无法把一次 `reset --hard` 或失败 rebase 当作单一原子多 ref 步骤撤销——恢复退化为手动逐 ref reflog 手术，正是 AGENTS.md 为 GitButler 记录的痛点。
- **借鉴 Agenta 什么**：每次状态变更都 append 一条不可变完整快照，历史即可重放审计。
- **具体怎么改**：引擎已全在（`with_operation_log` 整库快照 + 5 表 + parent-DAG 选择 + dedup + busy 重试 + 可用的 `op restore`），真正缺的是接线。
  - 加薄封装 `with_reflog_and_operation(meta, scope, reflog_ctx, insert_ref, op)`：跑现有 op 事务，并在同一闭包内 append `Reflog::insert(txn,...)`（与 branch-create 已证明的同形 `FnOnce(&txn)->Future` 签名）。
  - **在命令边界封装、而非每个 reflog 写点**：rebase 单独就有 3 个 `with_reflog` 点（`rebase.rs:1638/2064/2246`），1:1 替换会把一次 rebase 碎成多个 operation。整段序列化命令（rebase/cherry-pick/merge/revert）须坍缩成**一个** operation。
  - 修正集成清单：**丢 restore.rs**（不动 ref）、**加 branch delete**（当前未记录）、**特判 revert.rs**（经 `Branch::update_branch` 直改、绕过 with_reflog，`revert.rs:1197`）。
- **收益**：所有破坏性操作单步原子 undo；`op log --json` 完整机器可查审计；恢复故事终于匹配项目对 GitButler oplog 的依赖。
- **成本**：large。
- **风险/注意**：
  - 两个封装各自开 `db.transaction`——必须**组合成一个事务**，不可嵌套，否则 SQLite 死锁。
  - **写放大**：`collect_final_view` 每次快照全部 ref，commit（最热命令）会每次写 O(refs) 行 + 分页 parent 扫描。开启到 commit 前需加 view 去重（ref 集不变则复用上一 view_id）或更轻的 commit scope。
  - 可恢复序列（rebase --continue）跨进程，单个 DB 事务无法字面跨越整个用户可见操作——须明确“仅在最终完成时记录”或把多步缝成一个 op_id。
  - 增量落地：先 reset、再 merge/commit（最清晰的 "undo --hard" 收益），最后做序列化命令的命令边界封装。

---

### P1-6. 抽出 worktree 无关的提交级 merge 原语 `merge_commits`

**验证结论：viable-with-caveats，置信度高。这是更大编排器改造里今天就能做、独立有价值的一步。**

- **问题**：AI 编排器的 `sync_task_worktree_back`（`workspace.rs:1032-1116`）用文件级 3-way `diffy::merge_bytes` 重整，**完全脱离 VCS**（无 Commit/Tree/HEAD 调用），正是 rebase 丢文件 / commit -a 误删 / 并发暂存竞争三类踩坑的滋生地。而 Libra 现有的 `perform_three_way_merge`（`merge.rs:646`）**不能直接用于任务回合**：它要求干净的共享树、读写共享 index、移动共享 HEAD、用 `reset_index_and_workdir_to_tree` 覆盖共享工作目录。
- **借鉴 Agenta 什么**：append-only 不可变提交纪律——回合必须是忠实快照，不是静默局部；“校验而非静默重整”。
- **具体怎么改**：把 `merge.rs:1332` 的 `merge_tree_items(base,ours,theirs)` + `create_tree_from_items_map`（1584）这一**纯对象图 3-way 合并核**（无 workdir/HEAD I/O）暴露为 `merge_commits(base,ours,theirs)->{tree_id,conflicts}`。这是今天唯一存在、可独立落地的部分，用它替掉脱离 VCS 的 diffy 文本合并，立即获得真 3-way 语义。
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
  - 纠正前提：Libra **不用 `git rev-parse`**，用原生 `get_commit_base_typed`；`log A..B hangs` 是另一个已修区间 bug，本改动不修它。
  - 当前无命令收“标识符袋”，故这是 net-new opt-in 面，按“agent 确认价值”立项，不是修现有 hazard。

---

### P2-8. push 路径集成“丢路径”预检（默认 warn）

**验证结论：viable-with-caveats，置信度高。核心判断：砍掉 rebase/merge/pull 闸门，只保留 push 预检。**

- **问题**：原最高危踩坑是 rebase/merge/pull 从部分 checkout 重建树、静默丢弃磁盘上为空的 ~170 个被跟踪文件，随后 push 把它们从 origin 删除；唯一防御是手跑的 `comm -23 ls-tree` 闸门。
- **借鉴 Agenta 什么**：commit 必须是忠实快照、不是静默局部;“不一致即拒”。
- **具体怎么改**：在 push 发送前，比对待 push tip 的树路径集与远程跟踪 ref 的树（`fetch` 后已可得；`push.rs:2407` 已算 `incremental_objs vs remote_base`）。若远程树存在的路径在 tip 缺失且无法被 push 提交区间的 diff 解释，作为 `details.dropped_paths` 报出，**默认 warn**（或 `push.guardDroppedPaths` 配置，默认 warn），提供 `--allow-deleted-paths` 覆盖。用 **`LBR-PUSH-*`**（风险是错误 push，不是错误 tree-build，比 `LBR-TREE-*` 更贴）。
- **收益**：把腐烂的部落知识做成机器可读防护；防御未来 rebase --autostash 等可能重新引入 workdir-based 风险的回归。
- **成本**：medium。
- **风险/注意**：
  - **砍掉 rebase/merge/pull 内闸门**：已核验当前所有产树路径（`rebase.rs:3519-3686`、`merge.rs:660-687`/`commit_tree_items`）均从对象库树或 index 构建、**不再扫 workdir**；2026-06-22 的“rebase 丢文件”是旧 workdir-based FF reset，已修。在那里加闸门要么死代码、要么对每次合法删除误报。
  - **必须默认 warn 而非 abort**：经 push 删文件是正常 git，abort-by-default 会破坏普通工作流。
  - **顺手高杠杆**：更新陈旧 agent memory（`libra_rebase_drops_files_hazard.md`、`dev_commands_improvement_loop.md` 的“NEVER rebase”），让 agent 不再每次发布都付“reset --mixed + 手动 comm-gate”税——bug 已修。

---

### P2-9. 跨破坏性命令统一的机器可校验动作预览信封 + `--assert-preview`

**验证结论：viable-with-caveats，置信度高。核心判断：附加式、不替换，增量落地。**

- **问题**：dry-run 散在多处但**形状各异**（`op restore --dry-run` 甚至不遵守 `--json`，走 `println!`），且 rebase/merge/switch/reset/restore **完全无结构化预览**。agent 须为每命令学一套解析器。
- **借鉴 Agenta 什么**：RetrievalInfo provenance + 部署 state+diff 事件；checkpoint rewind“同时显示 would-restore 与 would-delete”两侧 diff 是 Libra 自证。
- **具体怎么改**：
  - 在 `output.rs` 定义共享 `Preview` 类型 + `emit_preview`：`resolved_refs`（RetrievalInfo 式）、`writes`（会变的 objects/refs）、两侧路径 diff `{would_modify, would_add, would_delete}`。
  - **附加式**（新 `preview` 键），先落到**今天没有预览**的命令（rebase/merge/switch/reset/restore，及补上缺失 JSON 的 op restore），保留现有 commit/checkpoint/fetch 形状不动。
  - 配 `--assert-preview <hash>`：dry-run 记 digest → 实跑带 digest，状态漂移则报 `LBR-CONFLICT-002`（MVP 不新增 `003`）。digest 必须基于 canonical JSON（稳定字段顺序、稳定数组顺序、无 pretty-print 影响）和明确 schema version；在 `op.rs:447` 现有 `with_operation_log` 事务内做 recompute-compare-apply，对 refs 原子。
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
  - **第一步（在 grain 内、便宜）**：加 SQLite 触发器禁止 `operation` 表的 UPDATE/DELETE（仅 INSERT/SELECT），+ `fsck` 检查行数/PK 一致性。这正是 Agenta 灵感实际展示的（应用/DB 层强制不可变），也与 Libra 已规划的 append-only `agent_audit_log` 一致。**威胁模型边界**：持有 `.libra/libra.db` 写权限的攻击者可 `DROP TRIGGER` 或直接改文件——触发器防的是应用 bug 与误用 `sqlite3` CLI，不是密码学防篡改；第二步外锚才是对抗 DB 写权限的必要条件。
  - **第二步（仅当确需密码学保证）**：在 `operation` 表加摘要，但用 **Merkle-DAG** 而非线性链：`row_digest = H(canonical_content || sorted(parent_row_digests))`（operation log 是 `operation_parent` M:N 图、且并发 agent 合法分叉，线性链 + “重排/缺口检测”会对正常并发误报）。摘要**必须外锚**（用 agent 不持有的密钥签 chain head，或写入 append-only `agent_audit_log`，或 commit 进随 push 旅行的 ref/note），否则同一攻击者可重算整链 = 安全剧场。
  - 仅针对 `operation`（可选 reflog）；**不要**声称覆盖 object_index（可重建、对象自验）或 ai_* （文档化的可重建投影，真源是带 u64 seq 缺口检测的 append-only JSONL）。
- **收益**：把审计日志变防篡改，给 AI runtime 可验证 provenance 骨干。
- **成本**：medium。
- **风险/注意**：纯 in-DB 线性链不可行（攻击者可重写下游整链）；DAG 误配会对正常并发分叉误报；迁移须幂等 guarded ADD COLUMN，pre-migration 前缀须视为“不可验证”而非“已篡改”。

---

### P2-11.（需先做产品决策）部署指针集群：`libra env` + `libra promote` + `@{deploy:prod}`

**验证结论：viable-with-caveats，置信度中。强烈建议：先与维护者确认是否在 Libra 边界内。** 这是一个 CD/发布管理特性，机制可行且符合 Libra 的 SQLite-元数据 + 保留 orphan ref 习惯，但与“git 兼容核心 VCS”边界相邻而非重合；且与当前 git-parity 路线图正交。

- **问题**：Libra 无“哪个 commit 在哪个环境上线”的概念；要追踪 dev/staging/prod 只能滥用分支/tag，混淆“工作线”与“部署目标”，无运行记录。`reference` 表 `kind` 也结构性不含部署类。
- **借鉴 Agenta 什么**：environment-as-pointer（一等可移动命名指针、存完整自描述血缘、survive 重命名）；deploy/promote = commit 不可变 references 快照 + state/diff 事件；`environment_ref + key` 两跳寻址“线上跑的是什么”。**Agenta 自承的两个缺陷正好让 Libra 做得更好**：跨环境晋升不是单一操作、delta 路径无 CAS。
- **具体怎么改**（三个子提案，有依赖）：
  - **(a) `libra env list|create|show|set`**：新 `deployment` 侧表（name, commit_oid, source_ref, label, deployed_by, deployed_at, guarded），指针存于 `refs/libra/deploy/<name>` orphan namespace（仿 `AI_REF`/intent/traces，kind='Branch' 行，**不 push 到 stock git**，保 on-disk 兼容）。存 OID 作真源 + source_ref + 可选 label，使指针 survive 分支 churn。**勿编辑冻结的 `sqlite_20260309_init.sql`**，加新 `sql/migrations/<date>_deployment.(sql|_down.sql)` 经 `migration.rs` `include_str!` 注册（开库自升级）。加 `Env` 到 `Commands` 会强制 **三件套同步**（COMPATIBILITY.md 行 + commands/README.md 行 + commands/env.md），否则 `compat` 测试失败。统一 ref 命名并接全部守卫（`is_locked_branch` 精确匹配 **和** `op.rs` 的 `starts_with("libra/")` 过滤 **和** branch-list 隐藏）。
  - **(b) `libra promote --from staging --to prod`**：做成薄糖——`get_target_commit` 解析源 tip，`with_reflog`（`reflog.rs:322`，已是“ref 移动 + 审计写入”单事务）包裹一个**新增 CAS 的** `update_branch_with_conn`（给它加 `expected_old_oid: Option<&str>`，目标 tip 变化即在事务内失败）。**砍掉**原提案的独立 `deployment_log` 表和 `env rollback`——复用 reflog 的 action/message/committer 列记晋升血缘、复用 `op restore` 回滚。原子晋升 ~90% 已具备，只补缺失的 CAS 守卫 + promote 动词。
  - **(c) `@{deploy:<env>}` revspec**：在 `resolve_commit_base_atom_typed`（`util.rs:836`）加一个早期解析臂 → SQLite 查 → 具体 OID，则 log/diff/show/checkout 经共享解析器自动继承，`diff @{deploy:staging}..@{deploy:prod}`（“待晋升的是什么”）经现有 `normalize_diff_range` 自动可用（约 10-30 行）。**诚实文档化为 net-new、intentionally-different token**（stock git 解析不了；`is_valid_refname` 已拒 `@{`/`:`），勿宣称“扩展 git @{} 语法”（Libra 未实现 @{upstream}）。
- **收益**：运维与 agent 得到“各环境上线什么”的类型化可查答案，与分支拓扑解耦；晋升原子、可回滚、可审计，且优于 Agenta（CAS 而非读后写）。
- **成本**：medium（每子提案）。
- **风险/注意**：
  - **(c) 完全依赖 (a)**，**(b) 依赖 (a)**——不能独立评估/发布。
  - **范围契合是真正的开放问题**：这是 release/CD，与 `publish` 的 "deploy" 子命令（部署 Cloudflare Worker，非 commit 指针）重名风险——用 `libra env`/`libra promote`，**勿用 `libra deploy`**。其价值取决于 Libra 是否要把“哪个 commit 在哪上线”纳入自身 AI-agent-native 身份。
  - 受保护环境（原“guarded environments”提案）大体 **infeasible**：依赖此非存在子系统，且 Libra 权限模型是无强弱层级的自由字符串有序规则集，没有“DEPLOY > EDIT”的表示。**唯一可留的小点**：AI 发起的晋升经现有 `approved_permission`（Ask→Always）流，用新字符串权限键 `promote`；人工 CLI 拒绝复用 locked-ref 的 `ConflictOperationBlocked` 风格，**不**铸 `LBR-DEPLOY-*`（locked refs 也复用 `LBR-CONFLICT-002`）。

---

## 6. 落地执行包

这一节是可直接拆 issue / PR 的执行版。默认顺序是 A1 → A2 → B1 → B2 → B3 → B4；C 组为增益项，D 组必须先做产品决策。每张卡都应独立合并、独立回滚。

### 6.0 追踪矩阵（建议 ↔ 执行卡 ↔ 依赖 ↔ 状态）

| 建议（§5） | 执行卡（§6） | 优先级 | 成本 | 依赖 | 状态 |
|---|---|---|---|---|---|
| P0-1 op-view 纳入 GC roots + op restore fail-closed | A1 | P0 | small | — | 待实现 |
| P0-2 `<ref>@vN` 稳定句柄（不建表） | A2 | P0 | medium | — | 待实现 |
| P1-3 `commit --assert-staged` | B1 | P1 | medium | — | 待实现 |
| P1-4 ref 级 CAS `--expect-head/--expect-branch` | B2 | P1 | medium | 复用 `lease_oid_matches` | 待实现 |
| P1-5 ref-变更命令记录整库 operation view | B3 | P1 | large | 命令边界封装；与 P2-10 写放大相关 | 待实现（分三批） |
| P1-6 抽出 `merge_commits` 纯原语 | B4 | P1 | medium | （接入 orchestrator 依赖 per-worktree HEAD = D2） | 待实现（提取可独立做） |
| P2-7 `--ref-assert` + provenance | C1 | P2 | medium | 与 A2 不重叠（无 ordinal 维度） | 增益 |
| P2-8 push 丢路径预检（默认 warn） | C2 | P2 | medium | — | 增益 |
| P2-9 统一 Preview 信封 + `--assert-preview` | C3 | P2 | large | 与 P1-4 CAS 语义重叠（见下） | 增益 |
| P2-10 operation append-only 强制（+可选外锚 Merkle-DAG） | C4 | P2 | medium | 第二步外锚依赖签名/audit-log；与 P1-5 INSERT 兼容 | 增益（先做便宜版） |
| P2-11 部署指针 `env`/`promote`/`@{deploy:}` | D1 | — | medium×3 | (c)依赖(a)，(b)依赖(a)；**先做产品决策** | 阻塞于决策 |
| §7 per-worktree HEAD/index 隔离 | D2 | — | large | 与 intentionally-different 设计冲突 | rejected（除非反转产品方向） |

### 6.0.1 提案间交互（落地前必读）

- **P1-5 × P2-10（同表读写）**：P1-5 给所有破坏性命令**新增 `operation` 表 INSERT**；P2-10 第一步加的触发器**只禁 UPDATE/DELETE**，二者兼容。但落地顺序应 P1-5 在前、P2-10 在后，否则 P2-10 的 `fsck` 行数一致性校验会与 P1-5 引入的新写入点相互掩盖回归。若同期开发，二者的迁移须各自幂等、互不假设对方已落。
- **P1-5 × P2-10 写放大叠加**：P1-5 已知 commit 热路径每次写 O(refs) 行；若 P2-10 第二步再给每行算 Merkle 摘要，commit 成本进一步上升。务必先落 P1-5 的 view 去重，再考虑 P2-10 摘要，且摘要只在 `operation`（非每个 view ref）层算。
- **P1-4 × P2-9（两套 CAS）**：P1-4 的 `--expect-head`（断言操作前 ref 状态）与 P2-9 的 `--assert-preview <hash>`（断言整份预览未漂移）是**两个粒度**的乐观并发控制，可共存但不要互相替代——前者轻、面向单 ref 竞争，后者重、面向“检视→应用”闭环。二者失败都复用 `LBR-CONFLICT-002`，details 用不同 key 区分（`expected_oid` vs `preview_digest`）。
- **A2(P0-2) × C1(P2-7)**：`@vN` 句柄由 A2 提供；C1 的 `--ref-assert` **不得**再引入 ordinal 维度，避免两套版本语义并存。

### 6.0.2 回滚与特性开关矩阵

每个 tracer bullet 须能独立 revert，不留下半套 schema 或悬挂守卫。

| 执行卡 | 回滚方式 | 半落地风险 | 缓解 |
|---|---|---|---|
| A1 | revert `gc.rs` roots + `op.rs` 预检 | op restore 开始拒恢复已 prune 对象（行为变更） | 预检仅 fail-closed，不删数据；文档说明需 `gc` 前先 `op log` 确认 |
| A2 | revert `util.rs` 解析臂 + docs | 无持久状态 | 纯读路径，回滚零迁移 |
| B1 | revert flag + `CommitOutput` 字段 | agent 脚本依赖 `--assert-staged` | flag opt-in；JSON 字段 additive |
| B2 | revert flag | 无 | opt-in |
| B3 | revert wrapper 接线 | DB 中已有 operation 行（无害） | operation 行 append-only；不回滚历史 op |
| B4 | revert 提取的 `merge_commits` | orchestrator 未接则零行为变更 | 第一 PR 不改 orchestrator |
| C4 | `DROP TRIGGER` 迁移 `_down.sql` | 触发器阻止合法维护脚本 | 触发器仅 `operation` 表；维护用官方 `libra op` 路径 |
| D1 | migration `_down.sql` + 删 orphan refs | `refs/libra/deploy/*` 残留 | down 迁移 + `branch -D` 文档化清理 |

**特性开关**：除 D 组外，本文**不引入**全局 config 开关；一切新能力均为 per-invocation flag（`--assert-staged`、`--expect-head` 等），默认行为与 stock git 路径一致。

### A1. P0：operation view 目标纳入 GC roots，并让 `op restore` fail closed

**目标**：`libra op log` 列出的每个成功 operation，其 view 里引用的 commit 在 `libra gc --prune=now` 后仍可恢复；如果历史对象已经缺失，`op restore` 必须在改 HEAD/refs 前失败。

**改动范围**（注：原列出的 `src/command/gc.rs` / `tests/command/gc_test.rs` 已于 v0.17.1759 删除；下表已改指向现行 GC 实现 `src/command/maintenance.rs::run_gc` 及其测试，落地前按本文件顶部校正须知重新核验 roots 模型）：
- `src/command/maintenance.rs`（`run_gc` — 现行唯一 GC 实现）
- `src/command/op.rs`
- `tests/command/maintenance_test.rs`
- `tests/command/op_test.rs`
- `docs/commands/op.md`

**实现步骤**：
1. 在 `gc.rs` 新增 `operation_view_roots<C: ConnectionTrait>(db: &C) -> CliResult<HashSet<ObjectHash>>`，使用 `table_exists()` 分别守卫 `operation_view_ref` 与 `operation_view`。
2. 只收集两类 OID：
   - `SELECT target_oid FROM operation_view_ref`
   - `SELECT head_target FROM operation_view WHERE head_kind = 'detached'`
3. 不读取 `operation_view_workspace.pointer_value`，也不要无条件读取 `operation_view.head_target`；branch 态的 `head_target` 是分支名，不是 OID。
4. 在 `collect_roots_from_database()` 中 `roots.extend(operation_view_roots(&db).await?)`，位置放在 `agent_checkpoint_roots()` 附近即可。
5. 若 `gc --dry-run --json` 已有 roots 分类，新增 `operation_view_roots` 计数；若当前没有分类，先只在测试中直接断言 roots 集合包含目标 OID，避免为了可观测性重塑 GC 输出。
6. 在 `op restore` 的实际写 ref 事务前增加目标对象存在性预检：view refs 的 `target_oid` 与 detached `head_target` 必须存在且是 commit；失败返回 **`LBR-REPO-003`**（`RepoStateInvalid`）并带 `missing_oid` / `operation_id` detail 与 gc 恢复 hint。**禁止**用 `LBR-REPO-002`——该码保留给 `parse_stored_hash` 等 corruption 路径。

**验收标准**：
- `gc --prune=now` 不会删除只被 `operation_view_ref.target_oid` 引用的 commit。
- `gc --prune=now` 不会删除只被 detached operation view 的 `head_target` 引用的 commit。
- 人为删除 operation view 目标对象后，`op restore <op>` 不改 HEAD、不改任何分支、返回结构化错误。
- 如果新增 roots 分类输出，`gc --dry-run --json` 中能看到 operation-view roots 计数；否则相同事实由单测覆盖，不改变现有 JSON schema。
- 不新增 `op prune`、不新增 retention 策略。

**测试命令**：
```bash
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test gc_operation_view
LIBRA_SKIP_WEB_BUILD=1 cargo test --test command_test op_restore_missing_target
```

### A2. P0：实现 `<ref>@vN` 稳定 first-parent ordinal 句柄

**目标**：给 agent 和人一个不随 tip append 漂移的“分支第 N 版”句柄；MVP 不建表、不改 ref writer、不做迁移。

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

**验收标准**：
- `main@v0` 解析到 first-parent 根提交。
- 在 `main` append 新提交后，旧的 `main@v1` 仍解析到同一提交。
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

**D2. per-worktree HEAD/index 隔离**：当前与 Libra 已文档化的 intentionally-different worktree 设计冲突。除非维护者明确决定反转产品方向，否则本文件不建议实现；所有依赖它的 fork/worktree 隔离提案继续保持 rejected。

---

## 7. 已排除项

以下提案经源码验证后剔除，附一句原因（含可回收的微小残值）：

- **CI 强制的 ref 解析文法 + 多义即类型错误** — *already-implemented*。优先级（HEAD>本地分支>远程>tag>OID 前缀）已在 `util.rs:993-1001` 注释文档化并作为单一真源实现；分支严格胜 tag；OID 前缀多义已报 `ambiguous argument`；`log A..B hangs` 动机已于 v1383 修复。**唯一真残值**：多段远程跟踪名（`a/b/c` 的 `(a,b/c)` vs `(a/b,c)` 切分）首匹配静默选取（`util.rs:862-890`）——可复用现有 `CommitBaseError` 多义惯用法返回类型化错误，无需新文法/CI 测试。

- **`libra fork`（原子隔离 agent 分支+worktree）** — *infeasible*。“O(1) 在 commit X 建分支、对象共享、headless/JSON” 即今天的 `libra branch <name> <rev> --json`（`branch.rs:111-118`，内部单 ref 行 INSERT 无拷贝），Agenta “修 O(n)→O(1)” 框架描述的正是 Libra 现状；其定义性价值（带独立 HEAD 的隔离 worktree）依赖不存在的 per-worktree HEAD；分支在 SQLite、worktree 在 worktrees.json，无法单事务原子；无 ephemeral/created_by 列可标记/回收。

- **checkout 互斥守卫（branch already checked out elsewhere）** — *已实现*（本条的 2026-07 评估基线「所有 worktree 共享一个 HEAD」已被 plan-20260714 Part C W0 推翻：`reference` 表带 `worktree_id` 列，每个 worktree 解析自己的 HEAD 行）。守卫由 `Head::branch_checked_out_elsewhere_result` 单一 fail-closed 探针提供，接入全部 shared-branch mutator 与 current-branch ref writer；`checkout --ignore-other-worktrees` 不再绕过该守卫（intentionally-different，见 ADR-0714-09），冲突返回 `LBR-CONFLICT-002`。**唯一残值**：把并发 branch-create 竞争失败的裸 sea-orm integrity error 也映射到 `LBR-CONFLICT-002`（呼应 Agenta“传播冲突而非吞掉”）——小而真，无需新码/新机制；丢弃 `SELECT..FOR UPDATE` 类比（SQLite 无行锁，busy 重试已是等价物）。

- **受保护（guarded）环境** — *infeasible*（且依赖同样不存在的 env 子系统）。`promote.rs`/`env.rs`/`deployment` 表/`guarded` 标志全不存在；权限模型无强弱层级，无 "DEPLOY > EDIT" 表示；引用的 `compat_error_codes_doc_sync` 测试名也不存在。**唯一残值**：AI 发起的晋升走现有 `approved_permission` 流（新字符串键 `promote`）——已并入 P2-11 注意事项。

- **per-worktree HEAD/index 隔离** — *已实现*（本条按 2026-07 基线写成 conflicts-with-principles；plan-20260714 Part C 反转了该产品方向并交付了 per-worktree HEAD/index 作用域，下述反对意见与「共享 HEAD/index 是 intentionally-different」的引用均为历史记录，不再描述当前行为）。原评估：

  技术可建且不破坏 git on-disk 兼容，但与 COMPATIBILITY.md:77/88、`docs/commands/worktree.md`、libra-workflow skill 明文的“共享 HEAD/index/refs 是 intentionally-different，branch-isolated worktree 是反模式、官方替代是独立 clone”直接冲突；且其核心动机是误诊（被引“并发暂存竞争”是同目录共享工作树竞争，per-worktree 作用域修不了）。**不作为改进建议**，仅作为“若维护者主动反转产品方向”时的前置能力（见路线图），届时须按“从规范 worktree 路径确定性派生 worktree_id、保 `path::index()` 为纯同步函数、保持 Branch/Tag 行共享、丢弃 worktrees.json→SQLite 正规化”收窄实现，并预算 ~124 `Head::current*` + 36 `Head::update*` + 64 `path::index()` 站点的真实爆炸半径与文档反转。

---

## 8. 文档维护约定

1. **再核验触发器**：下列任一发生即重跑 §0.2 级锚点核验并更新 §0.3/§0.5：`collect_roots_from_database`、`with_operation_log`、`get_commit_base_typed`/`split_revision_navigation`、`run_commit`/`update_head_and_reflog`、`merge_tree_items`/`create_tree_from_items_map`（v5 发现偏移 ~98 行）、`is_locked_revision`、`StableErrorCode` 枚举变更。
2. **版本号**：结构性修订（新增执行卡、改变默认行为描述、错误码策略变更）递增文档版本（v5 → v6）；纯行号漂移修正只更新 §0.2/§0.5 表格，不 bump 版本。
3. **Issue 链接**：每个 §6 执行卡落地时，在追踪矩阵“状态”列链接 PR/issue，避免 §5 长文与实现分叉。
4. **Agenta 侧路径**：`/Volumes/Data/agenta-ai/agenta/...` 为撰写时本地路径；外部分发时改 GitHub 路径或删绝对路径，保留模块名（`core/git/types.py` 等）即可。
5. **测试索引**：新增 `tests/command/*` 或 compat 守卫时，同步 `tests/INDEX.md` 一行描述（Wave 1 缺省），并在对应执行卡“测试命令”段引用 `<target>::<fn>` 格式。
