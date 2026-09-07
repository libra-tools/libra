# Delta/DeltaDB → Libra 能力差距与对策

本文以 Zed Industries 的 **Delta**（delta.dev）及其底层 **DeltaDB** 为参照，分析 Libra 在「AI 会话与代码互链、agent 产出评审、协作分享」方向的能力差距与应对策略。命令名、模块名、协议名、frontmatter 字段名保留原文，避免失真。

参照对象：Delta 桌面/Web 应用（`0.1.1-nightly`，邀请制私测）与 DeltaDB（未开源，官方承诺开源）。

参考项目路径：**无本地 checkout**。这是本目录唯一没有 `/Volumes/Data/competition/<org>/<repo>` 落地仓库的竞品——Delta 闭源私测、DeltaDB 未开源，只能按公开文档与公告复核（快照规则见 §6.3 C2）。同目录其它 gap 文档均以 competition 目录下的实际 checkout 为事实源。主题相邻且有本地 checkout 可交叉印证的竞品：`/Volumes/Data/competition/git-ai-project/git-ai`（`793066013`，行级 agent/model/prompt 归因，对应本文 B1）、`/Volumes/Data/competition/letta-ai/trajectory`（`21ae92d`，多 runtime transcript 归一化）、`/Volumes/Data/competition/rekal-dev/rekal-cli`（`aace7a29`，git-native 会话记忆，对应本文 B3）。

校验时间：2026-08-27（delta.dev 首页、`/roadmap`、`/docs` 全部 20 个文档页；Zed 官方公告三篇；第三方评测两篇——完整来源清单见附录 A）。

Libra 侧基线：`main` HEAD `89081277a`，`Cargo.toml` 版本 `0.21.27`。本文所有 Libra 现状断言均已对照 `docs/development/plan/plan-long.md`、各日期计划与 `docs/commands/*` 逐条复核；与既有计划盘点冲突处（如 PD-02 实际状态）以仓库文档为准并在文中标注。

## 0. 结论摘要

### 0.1 比较边界

本文只保留一个方向：**Delta → Libra**。以 Delta 为参照，分析 Libra 补齐用户可见能力需要做什么，同时保持 Libra 的核心身份：Git 磁盘格式兼容、SQLite 管理可变状态、本地优先、fail-closed 安全模型、AI agent 原生。

「补齐」不是复制 DeltaDB 底层。Libra 不应复制 CRDT 实时复制 worktree、键击级操作流、云优先存储或实时多人编辑。每一项能力都必须落到 Libra 自己的架构上：JSONL 会话事件、checkpoint/PatchSet、`agent_bridge_link`、operation log（LR-02）、publish 脱敏导出、sandbox/approval 门。

### 0.2 核心判断

- **Delta 不是 Libra 的同类竞品，而是镜像互补。** Delta 做协作客户端层（Git 之上的伴生层、云优先、多人实时、当前零沙箱零审批）；Libra 做完整 VCS 与安全执行底座（本地优先、单写者、审计与脱敏优先）。双方在「AI 会话是代码的第一等历史」这一判断上完全一致。
- **Delta 的 GA roadmap 逐条印证了 Libra 既有路线图的方向**：Graph 视图 ↔ `libra graph`/thread-graph；Timeline/会话分叉 ↔ LR-02 operation log + checkpoint rewind；Sandboxing ↔ 已交付的 sandbox/approval；Subagent 侧聊 ↔ sub-agent dispatch。Delta 的市场存在本身是相关排期卡片的优先级论据。
- **Libra 落后的两处**：① 会话↔代码链接粒度（Delta：字符级锚 + 双向跳转；Libra：thread/intent/plan/task/run/patchset/commit 粗粒度）；② review 体验（Delta：锚定评论 + Last-Turn diff 基线 + pending 批量投递；Libra：findings 工件，无评论概念）。两者都**不需要 CRDT**——Libra 现有 checkpoint/patchset/bridge 数据已备齐原料，缺的是查询面与交互面。
- **Libra 领先的两处**：① 外部 harness 捕获（claude-code/codex/opencode hooks + `agent import` + bridge 20 方法已交付；Delta 的 Claude Code 插件仍在 roadmap）；② 安全（Delta 的 Agentic Safety 页自认「无权限系统、无沙箱、无 worktree 信任审查」，全部安全项仅在 roadmap）。
- **应坚守而非跟进**：实时多人编辑、云执行、Rust→WASM 同构客户端。Delta 在 Hacker News 被批评最多的正是「commit 前的试错被云端序列化」——Libra 的 local-first + redaction 是直接反差优势。

### 0.3 对策速览（详见 §6）

| 组 | 条目（括注当前治理状态与建议动作） | 关联 |
|---|---|---|
| A 推进既有条目 | A1 operation log + Change ID（已排期未启动→建议提前启动）；A2 LR-08（已验证→建议立日期计划）；A3 MCP authorizer（DEFER-03 延期→建议按重启条件另立安全计划）；A4 `.agents/skills` 磁盘约定对齐（DF-07 已排期未开始 + MEM-05 候选→落地时采纳约定） | LR-02/LR-03、LR-08、plan-20260715/20260824 DEFER-03、DF-07/MEM-05 |
| B 新立候选 | B1 AI-aware blame；B2 锚定评论；B3 thread 只读分享；B4 Last-Turn diff 基线；B5 ACP 入口；B6 外部会话 Web 直播面；B7 workspace 引导钩子；B8 detach 续跑 | AG-ATTR、LR-08 先导、LR-06、PD-02（已交付）、plan-20260818、LR-01 |
| C 坚守与跟踪 | C1 local-first 对位叙事；C2 本文纳入周期竞品审计（含无公开仓库时的快照规则），DeltaDB 开源后跟踪 | plan-long §路线图维护 |
| D 不采纳 | CRDT 实时多人 / 键击级捕获；WASM 同构客户端；自建云执行 | plan-20260822 DEFER-03、plan-20260715/20260824 DEFER-04 |

## 1. Delta 是什么

Delta 是 Zed Industries（Zed 编辑器团队；2025-08 Sequoia 领投 $32M B 轮，累计融资超 $42M，DeltaDB 即 B 轮资金的明确投向）于 2026-08-12 发布的**独立应用**，自述为 "a multiplayer environment for coding with agents and reviewing what they build"。产品形态：

- 以 **thread** 为中心：一个 thread = 一段与 agent 的对话 + 一个或多个 worktree。界面「更像带文件树的聊天应用而非编辑器」（MindStudio 评测），但保留完整代码编辑器。
- 桌面端 macOS/Linux/Windows 原生构建，nightly 发版；delta.dev 网页版是**同一个 Rust 应用编译到 WebAssembly + WebGL**。
- 邀请制私测，绑定 Zed 账号；无公开定价，计费复用 Zed 的 plan（托管模型按 token 计量，共享 Zed 的额度池）。
- 商业模式官方口径："build it, open-source it, and offer an optional paid service"——DeltaDB 将开源，付费面向托管同步/云执行/托管模型。

创始人 Nathan Sobo 的论点（两篇公告的主线）：PR/快照式评审在 agent 时代结构性失效——"Increasingly, the conversation that generates the code is becoming the true source of our software"；"Forcing every AI interaction through the commit-based workflow is like trying to have a conversation through a fax machine"。

## 2. DeltaDB 技术内核

官方文档与公告中可确认的设计要点：

1. **操作级捕获**。Git 每次 commit 记快照；DeltaDB 记录 commit 之间的**每一个操作**——"A delta is a recorded change to a thread or worktree. It can represent a file edit, a change to the file tree, a message, a comment"。delta 持续自动产生，无需 stage/commit；每个 delta 携带作者与其在线程历史中的位置。
2. **稳定身份 + 字符级永久链接**。官方最技术性的表述（Sequoia 公告）："DeltaDB uses CRDTs to incrementally record and synchronize changes as they happen… fine-grained change tracking also enables character-level permalinks that survive any code transformation"。引用锚定到 delta 而非行号；从任一行代码可跳到产生它的会话，反之亦然（"From any line of code, find the conversation that produced it"）。
3. **会话与编辑同体**。"A message and the edit it produced are recorded side by side"——对话与 worktree 是同一个复制数据结构；回退对话到较早位置会**同步回退 worktree 文件**（"revert the conversation to an earlier point, restoring the thread's worktrees along with it"）。
4. **虚拟化 worktree**。隔离克隆 "effectively free"；"Any point in history is a valid branch point, **including mid-run**"——agent 运行中途也可分叉。
5. **与 Git 严格互补**。项目必须是 git 仓库或位于其中；导入遵循 `.gitignore`（被忽略的构建产物与密钥不入库不同步）；"Commits stay in git"，不用 Delta 的同事看到普通 git repo。托管 checkout 有两个 remote：`origin`（原上游，publish/PR 默认目标）与 `local`（指回用户本机仓库）——agent 可把分支直推用户仓库，**含当前检出分支**（工作区干净则原地更新，脏则被 git 原生拒绝保护）。
6. **每参与者一份 checkout，DeltaDB 为同步中枢**。"Every participant gets their own copy of the code on their local machine, kept in sync in real time"；agent 单点执行（发消息者的机器或 Delta 云机），只有**记录的文件变更**被同步。托管 checkout 位于仓库内 `.delta/worktrees/`（自动对 git 隐藏）。
7. **`.agents/prepare` 引导钩子**。仓库根部可放可执行脚本，Delta 在每个新建托管 checkout 里、agent 开工前执行（装依赖等）；失败不致命。

文档未使用 "CRDT" 一词描述具体算法（该词仅出现于 Sequoia 公告），锚定机制、冲突收敛算法、操作日志的存储增长均未公开。

## 3. 功能面盘点（截至 2026-08-27 已上线）

| 面 | 要点 |
|---|---|
| Threads | 会话即文档：光标可停在转录任何位置（diff 行、计划步骤、thinking 块）直接输入；编辑历史消息会丢弃其后对话并重答；线程持久、可归档（云同步后本地历史 3 天回收）；中途换模型对话继续；`/` 菜单调 skill。**未提供** checkpoint/undo/中断暂停机制（文档层面确认缺失）。 |
| Terminals | composer 空行输 `!` 内联终端，运行于线程 checkout；Background Mode（`Cmd+Alt+Z`）保持 dev server/watcher 常驻，保留最近 **8 MiB** 原始输出供 agent 事后检查——注意其安全页承认**先存原始字节，agent 读取时才做已知密钥脱敏**。 |
| Review & Sync | Review Changes 汇集 diff，四种基线：branch / uncommitted / **Last Turn**（自上一 agent 轮起）/ 单 commit（`default_diff_base` 可设默认）；跨文件 diff 搜索。接受 = 纯 git（agent 推 `local` 或推 `origin` 开 PR）；**无 hunk 级接受/拒绝**，拒绝靠对话让 agent 改。PR 默认附回链到源 thread（校验分享权限）。 |
| Comments | 选中转录/文件/diff 任意内容即评（file tab 有评论/编辑模式切换，逐 tab 记忆）；评论积攒为 pending，随下一条消息**批量投递**给 agent（也可单独提交）；agent 回复反向链接回评论；支持多人回复串。锚定仅声明「评论跟随其所指文本」，机制未公开。无 resolve/reopen 生命周期。 |
| 协作 | 线程默认私有；分享后所有人实时看到消息/编辑/评论/agent 活动，**人人可 steer**；草稿可多人共同编辑后发送；作者归属直达模型（"the agent sees that attribution"）。三档访问：仅邀请（邮箱，14 天过期）/ 全组织 / 任何有链接者；仅 owner 可改权限，收紧不驱逐既有参与者。共享线程中**各参与者用自己的 provider 解析等价模型**。分享线程即授予其挂载仓库的 worktree 历史访问（仓库级，非线程级——官方文档明示此扩权）。 |
| Skills | `.agents/skills/<name>/SKILL.md`（项目级，随仓库共享）+ `~/.agents/skills/<name>/SKILL.md`（个人级，项目覆盖个人）；YAML frontmatter `name` / `description` / `user-invocable` / `disable-model-invocation`——与 Anthropic Agent Skills 格式同构（文档未声明兼容，但字段与目录结构一致）；`description` 驱动自动加载，`/` 菜单显式调用。 |
| 模型 | Zed 托管（按 token 计量）/ ChatGPT Plus/Pro 订阅登录 / GitHub Copilot 登录 / BYO key（Anthropic、OpenAI、Baseten、OpenRouter；env 或 `~/.config/delta/.env`）；中途换模型；自动 + 手动 compaction——Anthropic 产出可携带文本摘要，OpenAI 用加密 provider 态（跨 provider 需重放），本地线程跨 provider 切换时向旧模型索要可携带摘要。 |
| 安全现状 | Agentic Safety 页开门见山："Delta is in early access and does not yet have the agent safeguards described below." **无权限系统**（agent 自主调用含破坏性工具，无审批）、**无沙箱**（对所在设备无限制访问）、**无 worktree 信任审查**（共享内容加载时可能自动执行代码）。全部安全项仅在 roadmap。 |
| 数据 | 云优先：Git 对象（Cloudflare R2）、线程/未提交编辑的 delta（Durable Objects/SQLite）、元数据（KV/D1），全球处理；**仓库以 Git remote URL 为键**——"Organization members working from the same remote use the same stored repository data"（同一 remote 的组织成员共用同一份服务端仓库数据，即租户边界由 remote URL 隐式划定）；客户端删线程**不删已同步的服务端副本**（官方文档原话 "it does not yet remove already-synced copies from our servers"）；遥测与 Sentry 崩溃上报（内存转储可能含线程内容）**无关闭开关**；承诺不用用户代码/会话训练模型；已识别密钥出设备前替换为 `[REDACTED]`（仅匹配已知值，不扫描任意文件）。 |

## 4. Roadmap 与成熟度

官方 roadmap（2026-08）：

- **Public Beta**：Delta on the web（进行中）；**Claude Code 插件**（进行中——"Track Claude Code conversations and code changes in DeltaDB, then review them on delta.dev"）；**仓库权限接入**（"Repository-based access"，进行中——"Use your repository's existing permissions to control access to shared Delta threads"，即把共享线程的访问控制**委托给仓库既有权限**，是 Delta 对 forge 的显式依赖点）；代码→会话导航（进行中）；**Remote runtime** 云常驻执行（进行中）；Subagent 侧聊（排队）。
- **GA**：**Sandboxing**；会话分叉（conversation branching）；LSP 支持；扩展 Git 工作流（在 Delta 内 review/stage/commit/开 PR）；Graph 视图；Timeline 视图；语音输入。

第三方信息补充（byteiota）：agent 接入走 **ACP（Agent Client Protocol）**——Zed 2025-08 开放的 JSON-RPC 标准，JetBrains/Neovim 等已支持；兼容清单含 Claude Code、Codex CLI、Gemini CLI、Cursor、goose。

成熟度判断：版本 `0.1.1-nightly`，Review Changes 标签页 2026-08-12 才上线、代码评论 08-17～08-24 分批补齐、引导流程 08-20 才有——**核心循环刚成型数周**，未经大型存量仓库验证（MindStudio 明确指出），但迭代速度极快（周级发布可感知功能）。

## 5. 与 Libra 的定位对照

| 维度 | Delta | Libra |
|---|---|---|
| 与 Git 关系 | Git 之上的伴生层，commit 之间 | Git 磁盘格式兼容的完整 VCS 替代 |
| 历史粒度 | 操作级（每次编辑，CRDT） | 事件/快照级（JSONL SessionEvent、checkpoint、PatchSet、命令级 `libra op`） |
| 会话↔代码链接 | 字符级永久锚，双向跳转 | thread/intent/plan/task/run/patchset/commit 粗粒度（`agent_bridge_link` 表）；无行级归因面 |
| Review | 锚定评论 + 四基线 diff + 对话式修订 | `review`/`investigate` findings 工件 + `--fix` 受控回写（DF-03/04/09 已交付）；无评论概念 |
| 协作 | 实时多人，人人可 steer | 单写者 controller 租约、loopback-only；多用户显式 deferred（plan-20260822 DEFER-03） |
| 数据主权 | 云优先（Cloudflare），删除不完整 | 本地优先；publish/cloud 为显式动作 + 脱敏门 |
| 安全模型 | 当前为零（roadmap） | sandbox/approval/ACL/审计/fail-closed |
| 外部 harness | Claude Code 插件在途（ACP） | 已交付：claude-code/codex/opencode hooks 捕获 + `agent import` + `agent bridge --stdio` 20 方法（plan-20260713/20260818） |
| 形态 | 闭源私测 GUI（承诺开源 DeltaDB） | 开源 CLI + 内嵌 Web Code UI |

价值主张对比：Delta 回答「agent 产码快于人审」的方式是**保留完整决策链**——review 不从 diff 反推意图，而是直接问在场的 agent；评论锚定在随代码演化的位置上；上下文长期沉淀供未来的人和 agent 召回。这与 Libra 的 Intent/Thread/证据链模型同源，差别在 Delta 把它做成了**交互体验**（锚、跳转、评论、直播），Libra 目前止步于**数据与 CLI 查询**。

市场反馈（Hacker News，经 byteiota 汇总）：①隐私——commit 前的死胡同与半成品本属本地，Delta 默认全部序列化上云；②必要性——多少团队真的需要编辑器级实时多人。两点恰好是 Libra 的既有立场。

## 6. 对策

编号为本文内部编号（DELTA-A1…），不占用 plan-long ID 空间；进入路线图时按 plan-long 治理流程以既有 ID 挂靠或新立候选。

### 6.1 A 组：既有条目获得外部印证，建议推进治理状态

本组各条目当前治理状态不同（已排期 / 已验证 / 延期 / 候选），建议动作也随之不同——分别为「提前启动」「立日期计划」「按重启条件立项」「落地时采纳约定」，不将任何延期项或候选项误写为执行指令。

**A1 — 提前启动 LR-02/LR-03（operation log + 稳定 Change ID；已排期，实现未启动）。** DeltaDB 第一支柱「rewind to any edit / 稳定身份 / 任意时刻可分叉」正是 `plan-20260822` OL-*/CH-* 卡片的价值主张；CH-04 的 `ai_operation_link` 表（Operation/Change 经稳定 ID 关联 `session_id/run_id/tool_invocation_id/intent_id`，不绑定易变 commit OID，见 `plan-20260822.md` §CH-04）就是「代码↔会话链接」的 Libra 版本。该计划目前「已排期、实现未启动」，在全局顺序中列于 CT-01/UP-01 之后；Delta 的出现是把它前移的理由。粒度上不必做键击级 CRDT：**每次 agent 写工具调用产生一个 operation** 已能覆盖「中途回退/分叉」的绝大部分价值，且与 OL-* 的既定粒度一致。

**A2 — 为 LR-08（Forge/PR/CI 与 Stacked Review；已验证，无日期计划）立日期计划。** Delta GA 项 "Expanded Git workflows"（在产品内 review/stage/commit/开 PR）说明「agent 工作台必须能一路走到 PR」是行业共识终点。LR-08（`plan-long.md` A 组 P1，已验证）目前无日期计划，是 A 类唯一无排期的 P1；建议动作是把它推进到「已排期」（立日期计划），而非直接执行。

**A3 — 按 DEFER-03 重启条件为 MCP authorizer 另立安全计划（plan-20260715/plan-20260824 均记 DEFER-03，延期项）。** Delta 的 Agentic Safety 页整页写着「安全尚未实现」——安全是 Libra 对 Delta 最锋利的差异化卖点，而生产环境 `McpAuthorizer` 目前 `authz=None` allow-all（仅靠 loopback + control token/lease + tool ACL 兜底，见 `tracing/code.md` C9）。当安全成为对外叙事时，此缺口的形象成本大于工程成本。两份计划的 DEFER-03 均写明重启条件为「MCP authz 单独立项」——本条建议正是触发该条件：新立安全日期计划，而非在既有计划内加塞。注意与 `plan-20260822` DEFER-03（权限模型整体延期，重启条件为多用户/多租户需求）是不同条目，可在同一个安全立项中合并评估但不混同。

**A4 — DF-07 落地与 MEM-05 设计时对齐 `.agents/skills` 磁盘约定。** Delta 采用 `.agents/skills/<name>/SKILL.md`（项目级）+ `~/.agents/skills`（个人级），frontmatter 与 Anthropic Agent Skills 同构——这正在成为跨工具事实标准（Claude Code、Delta 已同构）。Libra 侧现状：skill activation 卡 DF-07 属 `plan-20260824`（该日期计划整体实施中），但卡片自身 Lifecycle 为 pending、Acceptance 为空——即**已排期、尚未开始**；MEM-05 技能投影为候选。两者落地/设计时若对齐该磁盘约定，同一 repo 的技能可被 Claude Code、Delta、Libra 共同消费，且 Libra 捕获侧（A0-07 skill projection）可反向丰富它。不新建第二套 skill 发现存储（DF-07 已列永久非目标）。

### 6.2 B 组：建议新立候选

**B1 — AI-aware blame：从任一行代码找到产生它的会话（升级 AG-ATTR）。** "From any line of code, find the conversation that produced it" 是 Delta 全部叙事中最有说服力的一句，也是第三方总结的三大真实场景之首（AI 代码溯源、多 agent 交接、新人理解决策）。Libra **原料已齐**：checkpoint、PatchSet（内容寻址 diff）、`agent_bridge_link`、captured transcripts。缺的只是查询面：`libra blame --provenance` 按行给出 session/turn/intent 归因，辅以反向查询（从 commit/行找会话）。不需要 CRDT——PatchSet hunk→turn 映射即可覆盖大部分价值，且与 AG-ATTR「只读导出、不改 Git 对象默认语义」的既定边界（`plan-long.md` §AG-ATTR）完全一致。建议 AG-ATTR 由 P2 候选升为 P1，并与 CH-04 的 `ai_operation_link` 排为同一条线（CH-04 供稳定 ID，AG-ATTR 供查询面）。

**B2 — 锚定评论：Web Code UI 的 comment-on-hunk。** 经复核，「评论/锚定/多用户协作」在 Libra 无任何 plan 条目（最近邻是 LR-08 Forge 与 LR-06 团队发布）。Delta 的评论体验是其 review 价值的载体，且其投递模型可直接借鉴——评论积攒为 pending、随下一轮**批量投递**给 agent，本质是**结构化的下一轮输入**，与 Libra 的 turn 模型天然契合，不需要实时基础设施。锚定不必用 CRDT：内容锚（blob hash + hunk 上下文；`plan-20260825` TA-02 的 content-anchored registry site keys 已是同型技术的仓内先例）在快照模型上即可工作。落点建议：Web Code UI 的 patchset/review 面先做「单人→agent」评审闭环，作为 LR-08 的先导子项；多人评论留给 LR-08/LR-06 之后。

**B3 — Thread 只读分享："share the thread, not the PR" 的 Libra 版。** Delta 的分享是实时可写的；Libra 可给出**异步、只读、脱敏优先**的中间态：单个 thread/session 的分享链接，复用 publish 已有的 AI 导出（redacted projection / graph / bundle，`docs/commands/publish.md`）与 LR-06 安全发布门（seal/pin/白名单）。价值场景与 Delta 相同（评审交接、新人理解决策），却正面回应其隐私批评——「分享是显式动作、导出必过脱敏」，且避开 Delta「分享线程即授予仓库级 worktree 历史」的扩权问题（线程级、只读、白名单）。

**B4 — Last-Turn diff 基线。** Delta 的 `default_diff_base=last_turn`（「上一 agent 轮改了什么」）是高频刚需且实现成本低。建议 Web Code UI review 面与 CLI 各加基线选择：branch / uncommitted / since-last-run / since-checkpoint。后者可直接建在已交付的 PD-02 之上（scoped `review --checkpoint <id>` 与 `investigate` 同款均已 PASS，见 `plan-20260714.md` 复核表）——即 checkpoint 范围评审已有，缺的是「按 agent 轮」这一时间基线与 diff 展示面。

**B5 — ACP 作为 bridge 第二入口。** Delta 靠 ACP 一举兼容 Claude Code、Codex CLI、Gemini CLI、Cursor、goose。Libra 的 `agent bridge --stdio`（plan-20260818，20 方法自 v0.21.1 完备）已是 JSON-RPC NDJSON——加一个 ACP 适配层，可把捕获从「逐 provider 装 hook」扩展到「协议直连」，roster 覆盖面随协议生态免费扩大（尤其收益于 hook 已收缩为 uninstall-only 的 gemini，及未支持的 cursor）。需评估 ACP 消息模型与 bridge 协议 v1 的映射成本后再立卡。

**B6 — 外部 agent 会话的 Web 直播观察面。** Delta 的 Claude Code 插件（roadmap 进行中）= 终端会话直播进可分享可评论的线程。Libra 的捕获侧早已交付（hooks、import、bridge），但消费面只有 CLI（`agent session/graph`）。补一块「在 Web Code UI 里实时观看正在进行的 claude-code/codex 会话」（复用 SSE wire v2 投影与既有 observe-only 面），Libra 就在这条 Delta 尚未发布的路线上反超。与 B2 评论、B3 分享、B1 blame 串成同一条「捕获→观察→评审→归因」故事线。

**B7 — Workspace 引导钩子（LR-01 相邻，需新立卡，现有 LR-01 卡不覆盖）。** 借鉴 `.agents/prepare`：agent workspace 创建时执行 repo 自带引导脚本（装依赖等）。安全边界必须与 Delta 相反：仓库内脚本按 **untrusted 输入**对待——执行前过 sandbox preflight 与显式 approval（首次/内容变更时重新确认，绑定脚本内容摘要），失败不致命但记入审计事件；Delta 无沙箱直接执行共享内容正是其风险清单第一条，引以为戒而非照抄。

**B8 — Detach 续跑（独立候选，与 B7 分立；不触碰 DEFER-04）。** Delta 云 runner 的本地等价物：`libra code --detach` + reattach——「合上笔记本 agent 继续跑」在本机即可成立。立卡时需明确四个行为轴：①进程存活（runtime 脱离前台终端后继续持有会话与租约的方式）；②重连（reattach 经 `.libra/code/control.json` 发现 + controller 租约交接，沿用既有 lease/token 语义）；③崩溃恢复（复用 JSONL `--resume` 恢复链与 fail-closed 判据）；④资源上限（无人值守时长 TTL、自动暂停/停止策略）。始终 loopback-only，不触碰非 loopback 远程写禁区（plan-20260715/20260824 DEFER-04）。

### 6.3 C 组：坚守与跟踪

**C1 — 把 local-first + fail-closed 明确为对位叙事。** Delta 现状：全量上云（含未提交编辑）、客户端删除不删服务端、遥测/崩溃上报无退出、零沙箱零审批、共享内容可自动执行代码。Libra 的每一条对应能力都已存在（本地优先存储、显式 publish + 脱敏、sandbox/approval/审计）。建议在 README/对外文档中显式对标这组差异——这是叙事任务，不是工程任务。

**C2 — 本文纳入周期竞品审计；DeltaDB 开源后升级跟踪方式。** 本文作为 `gap/` 目录常驻文档进入 plan-long 的周期竞品审计。Delta 当前无公开仓库，plan-long 审计惯例（按 revision 增量复核）不可直接套用，故约定**无公开仓库时的快照规则**：每次审计记录①产品版本（nightly build 号，如 `0.1.1-nightly.20260824.2`）、②抓取日期、③复核的页面/公告清单（相对上次的增删）、④能力变更摘要（对照本文 §3 表逐行 diff），随后才更新 plan-long 决策记录；本文各候选进入 plan-long 时按其新候选六要素（竞品 revision——以上述快照号代替、Libra 缺口、价值、风险、依赖、最小切入点）补全，不以本文引用代替。DeltaDB 官方承诺开源——落地时应第一时间转为标准 revision 审计，并评估：①锚定与收敛算法可否借鉴（服务于 B2 内容锚的演进）；②其线程数据可否导入为 Libra 只读证据（AG-ATTR 的又一互操作格式）；③其 `local` remote 直推当前分支的交互是否值得在 Libra workspace 采纳。

### 6.4 D 组：不采纳（按 plan-long 惯例留档，理由如下）

- **CRDT 实时多人编辑 / 键击级捕获。** 与单写者 controller 租约、loopback 安全模型、快照+事件存储三者正面冲突（`plan-20260822` DEFER-03 的「默认最大权限、单用户」假设是全套 operation-log 设计前提）；需求真实性存疑（HN 反馈）；B2/B3 的异步方案取其大部分价值。若未来多租户需求成立，按 DEFER-03 的重启条件走独立安全计划，而非在现架构上嫁接 CRDT。
- **Rust→WASM 同构 Web 客户端。** Delta 的选择服务于其「桌面与 Web 完全同体验」定位；Libra 的 Web Code UI（Next.js SPA + SSE）已满足观察/审批面需求，重写成本换不来对应价值。
- **自建云执行 / 托管服务。** 维持 DEFER-04（非 loopback 远程写面延期，SSH port-forward 为正式路径）；「断线续跑」以 B8 本地 detach 满足。等 DeltaDB 开源后评估互操作，而非自建云。

## 附录 A：来源清单

**delta.dev（官方文档，2026-08-27 抓取）**：首页；`/roadmap`；`/docs/getting-started`；`/docs/whats-in-the-latest`；`/docs/concepts/delta-and-git`；`/docs/concepts/worktrees-and-machines`；`/docs/agents/threads`；`/docs/agents/terminals`；`/docs/agents/review-and-sync`；`/docs/agents/comments`；`/docs/agents/skills`；`/docs/agents/models-and-providers`；`/docs/collaboration/collaborate-thread`；`/docs/configuration/settings`；`/docs/account/plans-and-pricing`；`/docs/privacy-and-security/data-storage`；`/docs/privacy-and-security/privacy`；`/docs/privacy-and-security/security`；`/docs/privacy-and-security/agentic-safety`；`/docs/troubleshooting`（仅目录级）。

**Zed 官方**：Introducing Delta（Nathan Sobo，2026-08-12，zed.dev/blog/introducing-delta）；Software Is Made Between Commits（Nathan Sobo，2026-06-11，zed.dev/blog/introducing-deltadb）；zed.dev/deltadb 落地页；Sequoia Backs Zed's Vision for Collaborative Coding（2025-08-20，zed.dev/blog/sequoia-backs-zed——CRDT 表述与商业模式的最明确来源）。

**第三方**：MindStudio "Zed Delta: A Hands-On Look at the Git-Alternative AI Coding Agent"（2026-08-20）；byteiota "Zed Launches Delta: Multiplayer Coding Built for AI Agents"（2026-08-13——ACP、agent 兼容清单、HN 反馈汇总）。

**Libra 侧复核对象**：`docs/development/plan/plan-long.md`（LR-02/03/06/08、AG-ATTR、MEM-05/06、SB-02、竞品审计节）；`plan-20260713.md`、`plan-20260714.md`（PD-02 复核表）、`plan-20260715.md`（DEFER-02/03/04）、`plan-20260818.md`、`plan-20260819.md`、`plan-20260822.md`（OL-*/CH-04、DEFER-02/03）、`plan-20260824.md`（DF-07、DEFER 清单）、`plan-20260825.md`（TA-02）；`docs/commands/code.md`、`docs/commands/agent.md`、`docs/commands/publish.md`、`docs/commands/investigate.md`；`docs/development/tracing/code.md`（C9 authorizer）、`docs/development/tracing/web-api.md`（SSE/WebSocket 取舍）；`COMPATIBILITY.md`。
