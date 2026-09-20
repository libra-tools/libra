# Libra 长期功能规划

## 文档职责与维护协议

本文是 Libra 不绑定具体发布日期和版本号的长期能力组合路线图。它回答「哪些能力值得长期投资、为什么、依赖什么、何时具备进入日期计划的条件」，不是 release 承诺、owner 清单或逐项实施任务表。具体设计、迁移、拆分、发布和回滚只进入按日期计划或后续 RFC/ADR。上次审计基线为 2026-09-14（第十一次）；本轮只更新允许的审计快照与路线图状态。

**本次改版：2026-09-17（第十二次）竞品审计。** 审计机为 macOS（Darwin 26.6.2，`git 2.54.0`，`libra 0.22.45`），本轮以实际 `$LIBRA_REPO` / `$COMP_ROOT` 与 `SCRATCH=/Volumes/Data/competition/libra-competitor-audit-2026-09-17` 为准；第十、十一次的 Linux 机器路径（含 `/run/media/...`）只保留在历史记录。核心变化：快照更新为 44 个仓库（39 Git + 5 Libra 类型；crabbuild 五仓本地缺失，`Einsia/agent-git`、`akitaonrails/ai-memory`、`anomalyco/opencode` 首次纳入）；Libra 推进至 `v0.22.47`——SB-01 pkt-line 切片随 plan-20260901 收口，operation v2 restore/undo/redo 与 sidecar Change ID 模块合入（未发布），**LR-03 已排期→实施中、MEM-06 候选→已验证**。本轮无优先级（P 级）变化。

状态定义：

| 状态 | 含义 |
|---|---|
| 候选 | 有问题线索，但 Libra 缺口、架构适配或证据尚不足 |
| 已验证 | 已同时核对竞品证据与 Libra 当前源码/测试，确认问题和可执行缺口真实存在 |
| 已排期 | 已有按日期计划覆盖该项的明确范围，并从本文链接 |
| 实施中 | 日期计划已有已合入和未完成切片，长期完成判据仍未全部满足 |
| 已实现 | 当前可发布版本中的代码、测试、用户/兼容文档共同证明完成判据已满足 |
| 已替代 | 原问题仍有效，但由另一项或更合适的机制承接 |
| 不采纳 | 经审计确认不适合 Libra，保留编号与理由 |

只有当前 checkout 的代码、测试、兼容性与用户文档，以及可发布版本证据共同成立时，才能标记「已实现」。日期计划写完、竞品已有、存在 schema 或文档声明都不构成实现证明。编号一经引用不重编号。

## 规划原则

1. **三类分工清晰。** 版本管理回答「代码与历史如何正确」；Agent 生成代码回答「Agent 如何安全地改代码并可追溯」；Memory 回答「跨会话/跨 Agent 如何记住并召回工程判断」。三者共享 SQLite、对象库、稳定错误码与 `--json`，但不互相替代事实源。
2. **开发者价值优先于命令数量。** 不以 Git flag parity 或竞品功能清单长度衡量进展。
3. **Libra-native，不复制竞品实现。** 复用 Git 对象/pack 兼容、SQLite 可变状态、AgentRuntime、sandbox、cloud。
4. **Git 互操作仍是底线。** 扩展元数据可以是 Libra 专有，但普通提交、对象传输和远端协作不能无故破坏 Git 兼容。
5. **所有 mutation 必须可观察、可恢复。** 进入 operation log；具备 preview、原子提交与失败恢复。
6. **机器接口先于交互外壳。** 先冻结 Rust API 与 `--json`/`--machine`，再做 Web/TUI。
7. **逻辑身份与存储身份分离。** commit OID 是内容身份；change / intent / review / memory / capsule 使用稳定逻辑身份。
8. **共享数据必须经过安全发布。** 原始 prompt、tool call、transcript、私有路径不得因写入对象库就自动成为团队可读数据。
9. **先确定性、后智能化。** preflight、hunk identity、overlap、recall 先提供确定性基线；LLM 只作为带 provenance、可撤销的增强层。
10. **计划状态必须据代码更新。** 每次实施前重核 `src/`、`COMPATIBILITY.md`、命令文档与测试。

---

## 本次竞品审计快照

审计时间：**2026-09-17（第十二次）**。审计机：macOS（Darwin 26.6.2），`git 2.54.0`，`libra 0.22.45`；Libra 主仓 `/Volumes/Data/GitMono/libra`（HEAD `9da06b4bf700472781c2e76ec48e96815475caf3`，最新 tag `v0.22.47`），竞品根 `/Volumes/Data/competition`。范围严格限定为竞品根下直接两层仓库（**44 个：39 Git + 5 Libra 类型**；本轮无空一级目录）。Git 仓库在 `git status --porcelain` 为空且有 upstream 时按本轮执行 `git fetch --prune` + `git merge --ff-only @{u}` 两步更新；Libra 类型竞品按 `libra pull --ff-only` 更新。本轮 15 个 fast-forward、15 个已是最新、14 个 blocked（10 `blocked-forced-update`、2 `blocked-network`（agenta、agent-trace）、1 `blocked-shallow`（cursor/cursor）、1 `blocked-local-ahead`（ledgermind，上游再次改写））。第十、十一轮在 Linux 机器执行；本机 clone 对 jj、gitbutler、dolt、git-ai 落后于其审计 revision 且被 forced-update 阻塞，这些仓本轮增量记 0（增量已由第 9–11 轮覆盖）；lore、letta-code、go-git 的本地 HEAD 在本轮前已领先其审计 revision（+14/+15/+9，本轮已按当前判据审读，标「未证明远端最新」）。`blocked-*` 只表示本地 revision 可读，**不**表示已更新到远端最新。仓库身份按规范化 remote 键匹配、目录名只作展示；集合变动见下附表。scratch 目录为 `/Volumes/Data/competition/libra-competitor-audit-2026-09-17`。
上次快照：2026-09-14（第十一次）；本轮对照其 revision 增量，并以当前 checkout 的 Libra 代码、测试、文档与发布 tag 为事实源。

| 竞品（目录） | remote | 类型 | 归类 | 分支 | 上次 revision | 审计 revision | 更新结果 | 增量/覆盖 | 证据入口（≤80 字） |
|---|---|---|---|---|---|---|---|---|---|
| `facebook/sapling` | facebook/sapling | Git | 版本管理 | `main` | `8395cae28` | `f79bbabcc` | **fast-forward** | +458 / 层3 抽样（热区100%、主题<30%，未声称全量） | 崩溃重启陈旧 inode 元数据 `a3b03945ae9`（E2）；FUSE killpriv v2；pushrebase 冲突预计算 |
| `jj-vcs/jj` | jj-vcs/jj | Git | 版本管理 | `main` | `c09b0c337` | `efe0cf178`（本地落后） | **blocked-forced-update** | +0 / 沿用（9–11 轮已覆盖） | immutable_heads 纳入 untracked remote tags `efe0cf178`（E2，上轮账本关闭） |
| `GitButler/gitbutler` | gitbutlerapp/gitbutler | Git | 版本管理 | `master` | `32dd13413` | `6446b0662`（本地落后） | **blocked-forced-update** | +0 / 沿用（9–11 轮已覆盖） | reorder 单分支 tip 修复 `62c064e61f`（E2，上轮账本关闭） |
| `GitButler/grit` | gitbutlerapp/grit | Git | 版本管理 | `main` | `dfb079967` | `dfb079967` | up-to-date | +0 / 沿用 | 上游 Git 套件兼容治理（CT-01 参照） |
| `epicgames/lore` | epicgames/lore | Git | 版本管理 | `main` | `82dcce98e` | `fa606b087`（未证明远端最新） | **blocked-forced-update** | +14 / 层1，主题 100% | 拒绝同仓重叠 link 挂载 `a03a32a`（E2）；shutdown 后调用显式失败 `7ccb6a1`（E2）；JWT issuer 轮换 |
| `git/git` | git/git | Git | 版本管理（参考基线） | `master` | `1630431f` | `47ce80527c` | **blocked-forced-update** | +58 / 层2，主题 100% | MIDX 引用已删 pack 的恢复 `8f909ff4e9`（E2）；worktree_basename 越界读 `997c1daf1d`（E2） |
| `go-git/go-git` | go-git/go-git | Git | 版本管理（架构参考） | `main` | `52f84ef3e` | `e9e5820fe`（未证明远端最新） | **blocked-forced-update** | +9 / 层1，主题 100% | HTTP 传输 redirect 源校验/nil hop fail-closed `c9b1dc59`+`877a8f43`（E2） |
| `go-git/go-billy` | go-git/go-billy | Git | 版本管理（架构参考） | `main` | `7bd0594` | `7bd0594` | **blocked-forced-update** | +0 / 沿用 | FS 抽象与 capability |
| `entireio/forgemark` | entireio/forgemark | Git | 版本管理（协作参考） | `main` | `47f57bf` | `47f57bf` | up-to-date | +0 / 沿用 | Forge metadata |
| `dolthub/dolt` | dolthub/dolt | Git | 版本管理（相邻） | `main` | `3ca268096` | `a8f5de154`（本地落后） | **blocked-forced-update** | +0 / 沿用（9–11 轮已覆盖） | prolly key 带外 GC 数据丢失（E2，沿用）；submodule 未更新 |
| `lorevcs/lore` | lorevcs/lore | Git | 版本管理（相邻） | `main` | `1fd2ea9` | `1fd2ea9` | up-to-date | +0 / 沿用 | intent 记录（单人项目） |
| `nervosys/Lit` | nervosys/lit | Git | 版本管理（相邻） | `master` | `a930e44` | `a930e44` | up-to-date | +0 / 沿用 | CHANGELOG 1.6.0 加密声明未验证反例（沿用） |
| `treeverse/lakeFS` | treeverse/lakefs | Git | 版本管理（相邻） | `master` | `4bb11638e` | `4bb11638e` | up-to-date | +0 / 沿用 | GHSA-gf2q-q6wc-x7fm（E3 沿用） |
| `walgit/walgit` | tobi/walgit | Git | 版本管理（相邻） | `main` | `6d8fa54ba` | `80e9a20b2` | **fast-forward** | +39 / 层2，主题 100% | 退役前可达性守恒证明 `bf65c01`（E2）；空 pack push tips 校验 `d5e75ca`（E2）；OIDC 显式 issuer `202ebcc`（E1） |
| `git-ai-project/git-ai` | git-ai-project/git-ai | Git | Agent 生成代码（相邻） | `main` | `f8e39c2c8` | `6fbc1ef0f`（本地落后） | **blocked-forced-update** | +0 / 沿用（9–11 轮已覆盖） | codex checkpoint 按 rollout 文件名键控 `7ace11b09`（E2，上轮账本关闭）；submodule 未更新 |
| `xai-org/grok-build` | xai-org/grok-build | Git | Agent 生成代码 | `main` | `72a61251` | `482711333` | **fast-forward** | +3 / 层1，主题 100% | fast-worktree GC 集成测试 `37949780`（E2）；ACP line reader 收敛 |
| `getcursor/cursor` | getcursor/cursor | Git | Agent 生成代码（相邻） | `main` | `654b1b4` | `654b1b4` | **blocked-shallow** | +0 / 沿用 | issue 信号源，无产品源码 |
| `mainline-org/mainline` | mainline-org/mainline | Git | Agent 生成代码 | `main` | `5704305` | `5704305` | up-to-date | +0 / 沿用 | intent seal、preflight、hook 预算 |
| `StepzeroLab/research-git` | stepzerolab/research-git | Libra | Agent 生成代码 | `main` | `62bcdf5` | `62bcdf5` | up-to-date（类型 Git→Libra） | +0 / 沿用 | Feature Capsule、recall/compose；LLM 承担 reapply 非确定性算法 |
| `letta-ai/letta-code` | letta-ai/letta-code | Git | Agent 生成代码 | `main` | `e356d4068` | `1b5290bb9`（未证明远端最新） | **blocked-forced-update** | +15 / 层1，主题 100% | 慢 Bash 自动后台化并在完成时通知 `feb32e33`（E2）；in-flight 检索合并 |
| `letta-ai/letta-agent-sdk` | letta-ai/letta-agent-sdk | Git | Agent 生成代码 | `main` | `9ae7b8792` | `9c8e854d9` | **fast-forward** | +11 / 层1，主题 100% | 保留 ephemeral worker lineage 与身份 `628bbc7`（E2） |
| `letta-ai/trajectory` | letta-ai/trajectory | Git | Agent 生成代码 | `main` | `21ae92d` | `21ae92d` | up-to-date | +0 / 沿用 | transcript 归一化 |
| `letta-ai/skills` | letta-ai/skills | Git | Agent 生成代码 | `main` | `16352df` | `b03323bfe` | **fast-forward** | +1 / 层1，主题 100% | 移除 letta-api-client skill（提示词仓，§1.3 排除路径） |
| `letta-ai/agent-file` | letta-ai/agent-file | Git | Agent 生成代码 | `main` | `78212eb` | `78212eb` | up-to-date | +0 / 沿用 | `.af` 可移植格式 |
| `anomalyco/opencode` | anomalyco/opencode | Git | Agent 生成代码（相邻，首次纳入） | `dev` | —（首次纳入） | `bbd72fb8b0` | **blocked-forced-update** | 窗口 90 天（1501 条）/ 层3 抽样 | 权限拒绝后停止 run `709af586`（E2）；home 相对权限路径展开 `fd9ee435`（E2） |
| `deepseek-ai/deepseek-harness` | deepseek-ai/deepseek-harness | Git | Agent 生成代码（相邻） | `master` | `76fda7297` | `0d1f50007` | **fast-forward** | +1488 / 层3 抽样（热区100%、PR 标题抽样，未声称全量） | `SESSION_FORMAT_VERSION` 0→3（bridge 事件面未变，E2）；persistence format history 文档化 |
| `diegoxtr/ctx-open` | diegoxtr/ctx-open | Git | Memory（相邻） | `main` | `862e12b` | `862e12b` | up-to-date | +0 / 沿用 | 认知对象版本化（source-available，概念参考） |
| `memorax-ai/memorax-code` | memorax-ai/memorax-code | Git | Memory（相邻） | `main` | `acd6f1614` | `0e56e9a07` | **fast-forward** | +96 / 层2，主题 100% | cwd-less scope 迁移校验 `0a47119`（E2）；适配 session format 3 `abc98ac`（E2）；自动更新恢复仍无验签 |
| `rekal-dev/rekal-cli` | rekal-dev/rekal-cli | Git | Memory | `main` | `aace7a29` | `4550e602e` | **fast-forward** | +3 / 层1，主题 100% | 安装器版本解析去 GitHub API 依赖；无新结论 |
| `rohitg00/agentmemory` | rohitg00/agentmemory | Git | Memory | `main` | `e04ba88` | `e04ba88` | up-to-date | +0 / 沿用 | 四层记忆、混合检索（主要证据源） |
| `MachineWisdomAI/fava-trails` | machinewisdomai/fava-trails | Git | Memory | `main` | `6653f9f` | `10f689f74` | **fast-forward** | +69 / 层2，主题 100% | 持久化前拒绝明显秘密 `c91d644`（E2）；深嵌套 fail closed `094af6b`（E2）；紧凑 MCP 面 |
| `ruvnet/agentic-flow` | ruvnet/agentic-flow | Git | Memory | `main` | `d3735a3` | `e993605b8` | **fast-forward** | +9 / 层1，主题 100% | jj bookmark 迁移修复；无新结论；submodule 未更新 |
| `graphwisdom/perstate` | graphwisdom/perstate | Git | Memory | `master` | `95e27e3` | `95e27e3` | up-to-date | +0 / 沿用 | 反例：push+rebase 重试非并发安全模型 |
| `matrixorigin/Memoria` | matrixorigin/memoria | Git | Memory | `main` | `627934261` | `689f3f9ba` | **fast-forward** | +7 / 层1，主题 100% | 跨 schema 版本恢复保全数据 `a2e1e25`（E2）；owner-scoped master authority |
| `sachinsharma9780/memweave` | sachinsharma9780/memweave | Git | Memory | `main` | `2ff82df` | `2ff82df` | up-to-date | +0 / 沿用 | Markdown+SQLite 索引 |
| `sl4m3/ledgermind` | sl4m3/ledgermind | Git | Memory（反例） | `main` | `99220d1`（本地） | `99220d1` | **blocked-local-ahead** | +0 / 上游再次改写 | 仍只当宣传材料（第 11 轮所见 4d7d35621 不在本机） |
| `sqliteai/sqlite-memory` | sqliteai/sqlite-memory | Git | Memory | `main` | `0f0aede` | `0f0aede` | up-to-date | +0 / 沿用 | submodule 未更新；SQLite 混合检索 |
| `Einsia/agent-git` | einsia/agent-git | Git | 版本管理（相邻，首次纳入；网页型转本地） | `main` | —（首次纳入） | `531bfcec0` | **fast-forward** | +24（总 168，首提交 2026-09-01）/ 层1，主题 100% | 会话观察落库前秘密保护 `8e222bc`（E2）；历史快照读隔离 `25acb65`（E2）；MIT |
| `akitaonrails/ai-memory` | akitaonrails/ai-memory | Git | Memory（首次纳入） | `main` | —（首次纳入） | `a200127c5` | **fast-forward** | 窗口 200 条（总 1846，首提交 2026-05-21）/ 层2 抽样 | 跨 Agent 记忆（Rust）：hook 载荷 JSON 转义修复及 no-op 回归 `2be13836`+`c83076b3`（E2）；跨项目 inbox/queue `74bd791c`（E3，随 v2.3.0 发布） |
| `cursor/agent-trace` | cursor/agent-trace | Git | Agent 生成代码（相邻，本地恢复） | `main` | `2754f07` | `2754f07` | **blocked-network** | +0 / 沿用 | 归因互操作格式（RFC 未复核，沿用） |
| `entireio/cli` | github.com/entireio/cli | Libra | 版本管理（相邻，本地恢复） | `main` | 7d16639e（不在本地历史） | `ad42643a9` | **fast-forward**（基线重置） | 切片 435 / 层3 抽样 | secret patterns 在 git 失败时 fail closed `c8a8d16`（E1）；checkpoint push 拒因上浮 |
| `entireio/cli-checkpoints` | github.com/entireio/cli-checkpoints | Libra | 版本管理（相邻，本地恢复） | `entire/checkpoints/v1` | `0204a02` | `0204a02` | up-to-date | +0 / 沿用 | refs checkpoint（沿用第 9 轮分析） |
| `entireio/git-sync` | github.com/entireio/git-sync | Libra | 版本管理（相邻，本地恢复） | `main` | 3ee99835（不在本地历史） | `013012ac8` | **fast-forward**（基线重置） | +14 / 层1，主题 100% | bootstrap marker 按 branch 作用域收敛（E1）；沿用 pack relay 分析 |
| `agenta-ai/agenta` | github.com/agenta-ai/agenta | Libra | 相邻参考（本地恢复） | `main` | `53717db` | `53717db` | **blocked-network** | +0 / 沿用 | prompt/workflow 版本化（沿用第 9 轮分析） |

| 变动类型 | 仓库（目录） | 上次 revision / 当前 HEAD | 说明 |
|---|---|---|---|
| 本地缺失 | `crabbuild/compass` | 5a9081f93 / — | 本地缺失（第 11 轮 5a9081f93；上游状态未验证），差距矩阵参照标「沿用（本地缺失）」 |
| 本地缺失 | `crabbuild/crab` | 77a9dc868 / — | 本地缺失（第 11 轮 77a9dc868；上游状态未验证） |
| 本地缺失 | `crabbuild/prolly` | 6ee959eae / — | 本地缺失（第 11 轮 6ee959eae；上游状态未验证） |
| 本地缺失 | `crabbuild/silo` | 7f71a06b0 / — | 本地缺失（第 11 轮 7f71a06b0；上游状态未验证） |
| 本地缺失 | `crabbuild/trail` | 9823ed755 / — | 本地缺失（第 11 轮 9823ed755；上游状态未验证） |
| 目录改名 | `tobi/walgit` → `walgit/walgit` | 6d8fa54ba / 80e9a20b2 | 仅目录名；remote 键 tobi/walgit 一致 |
| 目录改名 | `gitbutlerapp/gitbutler` → `GitButler/gitbutler` | 32dd13413 / 6446b0662 | 仅目录名（第 10 轮反向改名的回摆） |
| 目录改名 | `gitbutlerapp/grit` → `GitButler/grit` | dfb079967 / dfb079967 | 仅目录名 |
| 目录改名 | `mainline-org/mainline` → `mainline/mainline` | 5704305 / 5704305 | 仅目录名 |
| 目录改名 | `EpicGames/lore` → `epicgames/lore` | 82dcce98e / fa606b087 | 仅大小写回摆 |
| 目录改名 | `getcursor/cursor` → `cursor/cursor` | 654b1b4 / 654b1b4 | 仅目录名 |
| 首次纳入 | `Einsia/agent-git` | — / 531bfcec0 | 网页型竞品转本地（§1.5 路径）：MIT；总 168 提交、首提交 2026-09-01；窗口=全部 168 条中本轮抽读近期主题，网页结论不继承 |
| 首次纳入 | `akitaonrails/ai-memory` | — / a200127c5 | MIT；总 1846 提交、首提交 2026-05-21；窗口=最近 200 条，主题抽样 |
| 首次纳入 | `anomalyco/opencode` | — / bbd72fb8b0 | 无 LICENSE 文件（仓库无产品级 LICENSE 标注）；总 15680 提交、首提交 2025-03-21；窗口=最近 90 天 1501 条，层 3 抽样 |
| 类型变化 | `StepzeroLab/research-git` | 62bcdf5 / 62bcdf5 | 类型 Git→Libra（恢复第 9 轮形态）；revision 未变，仍可作增量基线 |
| 本地恢复 | `agenta-ai/agenta` | 53717db / 53717db | 第 10–11 轮本地缺失后恢复；Libra 类型 clone，revision 与第 9 轮一致；本轮 `libra pull` blocked-network |
| 本地恢复 | `entireio/cli` | 7d16639e / ad42643a9 | 第 10–11 轮本地缺失后恢复；Libra 类型 clone；上次 revision 不在本地历史（基线重置），以 `61dac01..ad42643a9` 切片兜底 |
| 本地恢复 | `entireio/cli-checkpoints` | 0204a02 / 0204a02 | 第 10–11 轮本地缺失后恢复；Libra 类型 clone，revision 与第 9 轮一致 |
| 本地恢复 | `entireio/git-sync` | 3ee99835 / 013012ac8 | 第 10–11 轮本地缺失后恢复；Libra 类型 clone；上次 revision 不在本地历史（基线重置），以 `5270fb0..013012ac8` 切片兜底 |
| 本地恢复 | `cursor/agent-trace` | 2754f07 / 2754f07 | 第 10–11 轮本地缺失后恢复；Git 类型，revision 未变；本轮 fetch blocked-network |

待验证账本索引（E1；全量在 `$SCRATCH/pending.tsv`）：

| 关联编号 | repo@sha | 最小验证步骤 |
|---|---|---|
| RT-01 | deepseek@0d1f50007 | 比对 `packages/core/session/src/types.ts` SessionEvent 字段与 bridge ingress 解析面 |
| SB-02 | walgit@202ebcc | 读 OIDC 显式 issuer diff，确认是否有测试与失败路径 |
| LR-05 | sapling@fcfb7acf06a | 读 pushrebase 冲突预计算 diff，判是否可作 LR-05 判据 |
| SB-01 | go-git@c9b1dc59 | 读 nil hop fail-closed diff 与同系列测试归属 |
| MEM-04 | fava@6527b6c | 读紧凑 MCP 面的 tool 清单与 session-init 测量实现 |

| 审计日期 | 仓库数 | 更新摘要 | 路线图结论 |
|---|---:|---|---|
| 2026-09-17（第十二次） | 44（39 Git + 5 Libra） | 15 个 fast-forward、15 个 up-to-date、14 个 blocked（10 forced-update、2 network、1 shallow、1 local-ahead）；集合变动 20 行（本地缺失 5：crabbuild/*，目录改名 6，首次纳入 3：agent-git/ai-memory/opencode，类型变化 1：research-git Git→Libra，本地恢复 5）；jj/gitbutler/dolt/git-ai 本机落后于第 11 轮 revision 且被 forced-update 阻塞（增量 0） | **LR-03 已排期→实施中**（sidecar Change ID 模块合入未发布）、**MEM-06 候选→已验证**（ai-memory v2.3.0 inbox/queue，E3）；SB-01 pkt-line 切片收口（plan-20260901 完成，v0.22.47）；LR-02 显著缩小（v2 restore/undo/redo 合入）；SB-02/SB-04/LR-01/LR-09/MEM-01 补充判据；上轮待验证账本 6 项全部关闭；无优先级（P 级）变化、无新增编号 |
| 2026-09-14（第十一次） | 41（41 Git + 0 Libra） | 14 个 fast-forward、21 个 up-to-date、6 个 `blocked-forced-update`（dolt、git-ai、git/git、gitbutler、go-billy、jj）；首次纳入 `crabbuild/*` 五仓 | Libra 已发布 `v0.22.19`；merge 主线、operation v2 foundation 与 FastCDC 相关差距缩小；竞品新增证据继续补强既有 SB-01..04、MEM-01 与 AG-ATTR，不新增编号、不改变优先级 |
| 2026-09-03（第十次） | 36（36 Git + 0 Libra） | 9 个 fast-forward（sapling、gitbutler、lore、go-git、dolt、git-ai、grok-build、letta-code、letta-agent-sdk、memorax-code、Memoria、ledgermind 中 9 个达 fast-forward，其余 up-to-date）、25 个已是最新、2 个 `blocked-forced-update`（git/git、jj）；集合变动 12 行（本地缺失 5、目录改名 4、首次纳入 walgit、基线重置 ledgermind、类型变化 research-git、空目录 cursor/） | **UP-01、RT-01 推进为「已实现」（Libra 自身证据驱动）；LR-02、SB-02、SB-04 推进为「实施中」；SB 表新增状态列。** 竞品侧安全/可靠性证据面加厚（jj 并发写丢失、dolt 带 GC 数据丢失、git/git UAF、letta shell 解析绕过、git-ai 迁移原子化、walgit 授权缺口）全部映射到既有 SB-01/SB-02/SB-03/SB-04/MEM-01 补充判据，无新增编号、无优先级升降 |
| 2026-08-25（第九次） | 40（35 Git + 5 Libra） | 15 个 fast-forward（13 Git：sapling、jj、gitbutler、lore、git/git、go-git、dolt、git-ai、grok-build、letta-code、letta-agent-sdk、memorax-code、agentmemory；2 Libra：entireio/cli、git-sync）、23 个已是最新、2 个 blocked（agenta `blocked-timeout`、agent-trace `blocked-network` 远端仍 404）；无新增/删除仓库 | 竞品侧无优先级变化（安全/可靠性证据面加厚：go-git 循环 delta 栈溢出、grok-build shell 写权限 fail-closed、lore 内容尺寸上限、git/git 溢出与 unchecked-returns 加固、entireio redaction fail-closed、memorax 数据隔离与 lineage）——全部为既有 SB/MEM/LR 的补充完成判据或竞品证据，无新增编号。**Libra 自身进展为主**：CT4-01 发布卡执行、FIX-05 B 段 waves 发布；`plan-20260715`（RT-01）关闭；新增 `plan-20260821`（UP-01）、`plan-20260822`（LR-02/LR-03）、`plan-20260825`（B Code provider）；**LR-02/LR-03 由已验证推进为已排期**；更正上版把 `plan-20260822` 误标为 UP-01 的链接 |
| 2026-08-22（第八次） | 40（35 Git + 5 Libra） | 1 个 fast-forward（letta-code）、38 个 up-to-date、1 个 `blocked-network`（agent-trace 远端 404）；新纳入 10 仓库（deepseek-harness、ctx-open、dolt、lorevcs/lore、memorax-code、Lit、rekal-cli、git-ai、lakeFS、cursor/cursor） | **RT-01 推进为实施中、UP-01 改判已排期、MEM-01/02 推进已排期**（均为 Libra 自身进展驱动）；竞品侧 rekal-cli 与 letta-code shared-memory skills 加强 MEM-* 证据；无优先级降级或新增编号 |
| 2026-08-09（第七次） | 30 | 9 个 fast-forward（Lore、Sapling、git/git、GitButler、jj、letta-code、letta-agent-sdk、grok-build、entireio/cli）、20 个已是最新、1 个 `blocked-dirty`（agenta） | **CT-01 由「已验证（下一个执行任务）」推进为「实施中」。** 版本管理侧证据面加厚；Memory 类证据面不变，MEM-01/MEM-02 维持已验证。 |
| 2026-08-07（第六次） | 30 | 2 个 fast-forward（Lore、Sapling）、27 个已是最新、1 个 `blocked-dirty`（agenta）；新纳入 `matrixorigin/Memoria`、`memweave`、`ledgermind`、`sqlite-memory`（4 个 Memory 参考） | **无优先级变化。** Memory 类证据面加厚；CT-01 仍是下一个执行任务，MEM-01/MEM-02 维持已验证。 |
| 2026-08-07（第五次） | 26 | 1 个 fast-forward（Lore）、24 个已是最新、1 个 `blocked-dirty`（agenta）；首次按三类重组；新纳入 `letta-ai/*`（5）与 `rohitg00/agentmemory` | **结构重组。** Memory 升格为第一类长期能力（`MEM-*`）；CT-01 仍是版本管理类下一个执行任务；MEM-01 为 Memory 类首个验证任务。 |
| 2026-08-02（第四次） | 20 | 9 个 fast-forward、10 个已是最新、1 个 blocked-dirty | 无优先级变化 |

**本次结论：** 本轮最重要的变化再次来自 Libra 自身：SB-01 的 pkt-line 切片随 plan-20260901 收口（`PktFrameError` 校验 helper、7 个 unwrap 守卫、`LBR-NET-002` 文档化，v0.22.29..v0.22.47 发布），`9da06b4` 合入 operation v2 crash-safe restore/undo/redo/doctor 与 sidecar Change ID 模块（`src/internal/change/`，未发布）使 LR-02 显著缩小、LR-03 进入实施中。竞品侧：ai-memory `74bd791c`（随 v2.3.0 发布）证实跨 Agent 协调通道可行，MEM-06 推进为已验证；lore/git-git/walgit/go-git/opencode 的安全与可靠性修复只补充既有 SB/LR 判据。deepseek `SESSION_FORMAT_VERSION` 0→3 经复核不影响 bridge（按方法分发、不锁版本）。deepseek/sapling 两仓增量大、本轮仅完成抽样级审读（见覆盖率表），未声称全量。本轮无优先级（P 级）变化，无新增编号。

Top-5 最重要差距（两榜合成）：

| 排名 | 榜 | 关联编号 | 差距一句话 | S/D/X/U/C/E | 分 | 竞品证据 | Libra 证据 | 动作 |
|---|---|---|---|---|---:|---|---|---|
| 1 | A | SB-02 | MCP authorizer 生产仍未安装（默认 None=不鉴权）、shell 写重定向非 fail-closed；权限路径匹配与拒绝后行为缺确定性规范 | 2/1/2/2/2/E3 | 8 | `anomalyco/opencode@709af586`（E2，拒绝后停止 run）+`fd9ee435`（E2，home 相对路径展开）；`MachineWisdomAI/fava-trails@094af6b`（E2，深嵌套 fail closed） | `src/internal/ai/mcp/server.rs:46-47`（authz 默认 None）；`src/internal/ai/tools/utils.rs` 写重定向 needs_human | 保持实施中；补充完成判据 ×2 |
| 2 | A | SB-01 | pkt-line 切片已收口，但生产 panic 面未清零：`ToolRegistry::new()` 在 cwd 解析失败时 `panic!`，越界/下溢类解析防护需持续对齐 | 2/1/2/2/2/E4 | 8 | `git/git@997c1daf1d`（E2，worktree_basename 越界读修复） | `src/internal/ai/tools/registry.rs:100`（cwd panic）；pkt-line 已 fallible（`src/git_protocol.rs:227` `read_pkt_line` 返回 `Result`） | 保持实施中；补充完成判据 |
| 3 | A | SB-04 | 测试隔离半边已落地，child scope / 中断清理 / shutdown 后行为仍未统一 | 1/1/2/2/2/E4 | 6 | `epicgames/lore@7ccb6a1`（E2，shutdown 后调用显式失败不挂起）；`letta-ai/letta-code@feb32e33`（E2，慢命令后台化） | `grep -rn ProcessScope src tests` = 0；`src/internal/process_terminate.rs:12` | 保持实施中；补充完成判据 |
| 4 | B | MEM-01 | VCS-native Memory 存储与隐私基线仍无任何实现；竞品已在转义/作用域校验等隐私细节上出现真实修复波 | 1/1/3/3/1/E4 | 7 | `akitaonrails/ai-memory@2be13836`+`c83076b3`（E2，转义修复曾是 no-op 后跨实现回归） | `ls src/internal/ai/memory` 不存在；`src/cli.rs` 无 memory 子命令 | 保持已排期 +补充完成判据 |
| 5 | B | LR-02 | v1 已发布、v2 snapshot/view 已随 v0.22.44+ 发布，crash-safe restore/undo/redo 已合入未发布；restore 并发与发布验收未收口 | 0/1/3/3/1/E4 | 7 | `jj-vcs/jj@0a9b86970`（E2，沿用：合并后保留最新状态） | `src/internal/operation/store.rs:375`（RepoViewV2）；`9da06b4`（restore/undo/redo/doctor）；`src/command/op.rs:183-192`（restore_v2） | 保持实施中 +补充完成判据 |

能力差距矩阵（本轮完整覆盖 `ids.old` 的 24 个编号）：

| 编号 | 类别 A/B/C/SB | 状态（旧→新） | 最佳竞品参照 repo@sha path:line + 参照来源 | Libra 现状 file:line / test / 可复算命令 | 差距一句话 | 本轮变化 + 驱动方（Libra/竞品/双方） | 动作 | E |
|---|---|---|---|---|---|---|---|---|
| CT-01 | A | 实施中→实施中 | `gitbutlerapp/grit@dfb0799` `TESTING.md`（沿用） | `tests/command/t4_port_test.rs`（82 test）；`tests/compat-ledger/t4`（34 toml） | 部分 wave 已合入，S4 族 waves 与 S2 离线发现器仍未收口 | 不变（本轮核对：`ls tests/compat-ledger`） | 保持 | E4 |
| UP-01 | A | 已实现→已实现 | `memorax-ai/memorax-code@1491fbb` update 恢复（E2反例，仍无验签） | `src/internal/upgrade/manifest.rs:194`；`upgrade_auto_test`；tags v0.22.1..v0.22.47 均经签名链发布 | 签名升级链已完成并持续发布；文档债（CHANGELOG 0.22.1..0.22.10）仍登记 | 不变（本轮核对：`libra tag \| sort -V \| tail -1` = v0.22.47） | 保持 | E4 |
| LR-01 | A | 实施中→实施中 | `epicgames/lore@a03a32a` 拒绝同仓重叠 link 挂载（E2） | `src/command/worktree.rs:87`；`run_worktree_doctor`；`worktree_isolation_test`（119） | worktree 隔离/doctor 基础存在；挂载重叠拒绝、崩溃重启元数据一致性未成判据 | 扩大（竞品） | 补充完成判据 ×2 | E4 |
| LR-02 | A | 实施中→实施中 | `jj-vcs/jj@0a9b86970` 合并后保留最新状态（E2，沿用） | `src/internal/operation/store.rs:375` RepoViewV2；`src/command/op.rs:183-192` restore_v2；`9da06b4` restore/undo/redo/doctor；`op_test`/`restore_test` | v2 snapshot/view 已发布，crash-safe restore/undo/redo 已合入未发布，restore 并发与发布验收未收口 | 缩小（Libra；`9da06b4`、v0.22.44+） | 更新竞品证据 | E4 |
| LR-03 | A | 已排期→实施中 | `jj-vcs/jj@efe0cf178` immutable_heads 纳入 untracked tags（E2，账本关闭） | `src/internal/change/{identity,genealogy,store,resolve,builder,workflows}.rs`；`tests/command/change_revision_provenance_test.rs`；ADR-OL-04 sidecar-only | sidecar Change ID 与 rewrite genealogy 已合入（未发布），重写谱系与 immutable 边界未验收 | 缩小（Libra；`9da06b4`） | 更新状态（已排期→实施中） | E4 |
| LR-04 | A | 已验证→已验证 | `gitbutlerapp/gitbutler@32dd134` hunk mutation（E2，沿用） | `src/command/apply.rs`；`apply_patch` 单测；`grep -rn 'HunkId\|hunk_assign' src` = 0 | 有只读 hunk 基础，非交互 assignment／stack mutation 缺失 | 不变（gitbutler 本机无新增量） | 保持 | E4 |
| LR-05 | A | 实施中→实施中 | `EpicGames/lore@074eb0b` `lore-revision/src/merge`（E2，沿用） | `src/command/merge.rs`；`plan-20260903.md` MG-01..MG-21；cherry-pick 序列修复 `3128bc2`/`7190507`/`9be09fe`（v0.22.39-42） | merge 主线与序列一致性继续收敛；versioned conflict object / modeless sequencer 仍无 | 缩小（Libra） | 保持 | E4 |
| LR-06 | A | 已验证→已验证 | `letta-ai/letta-agent-sdk@628bbc7` 保留 ephemeral worker lineage（E2） | `src/internal/ai/intentspec/`；`grep -rn 'seal\|intent_pin' src/internal/ai/intentspec` = 0 | intent／checkpoint 有基础，seal、pin 与 publication 边界缺失 | 不变 | 保持 | E4 |
| LR-07 | A | 已验证→已验证 | `MachineWisdomAI/fava-trails@bb8580a` preflight MCP envelope（E2） | `src/internal/ai/intentspec/scope.rs:12`；`grep -rni 'preflight\|overlap' src/internal/ai/intentspec` = 0 | 缺确定性 pre-edit overlap gate | 不变 | 保持 | E4 |
| LR-08 | A | 已验证→已验证 | `walgit/walgit@4ff4f7a` 原生 Git URI 供包（E2） | `grep -rn 'trait Forge\|pull_request\|check_runs' src` = 0 | 无 Forge／PR／CI 机器接口 | 不变 | 保持 | E4 |
| LR-09 | A | 已验证→已验证 | `walgit/walgit@bf65c01` 退役前可达性守恒证明（E2） | `src/internal/sparse/mod.rs:26`；`src/utils/media/transfer.rs`；`media_fastcdc_test` | sparse／hydrate／FastCDC 有基础；partial clone/VFS 缺，对象退役无守恒证明 | 扩大（竞品） | 补充完成判据 ×1 | E4 |
| LR-10 | B | 已验证→已验证 | `StepzeroLab/research-git@62bcdf5` capsule／provenance（E2，沿用） | `src/internal/ai/capability_package/manifest.rs:62`；`src/cli.rs` 未注册 package | artifact／skill 有基础，capsule lifecycle／ablation 缺失 | 不变 | 保持 | E4 |
| RT-01 | B | 已实现→已实现 | `deepseek-ai/deepseek-harness@0d1f50007` session 事件面（E2） | `src/internal/ai/runtime/worker.rs`；`a643dfb`；v0.22.0；bridge `session/created\|event\|flush\|disposed` 面未变 | Web-only runtime 与 SSE v2 已发布；deepseek 格式升 v3 不影响按方法分发的 bridge | 不变（本轮复核 bridge 事件面）；Code UI/Web 执行器与公开 `libra code` 已由 plan-20260920 拆除（产品表面已拆除） | 保持 | E4 |
| AG-ATTR | B | 候选→候选 | `git-ai-project/git-ai@7ace11b09` 会话按 rollout 文件名键控（E2，账本关闭） | `src/internal/ai/agent_import.rs`；`grep -rn ai_edit_trace src sql` = 0 | 原生 transcript 导入存在，归一化行级归因仍缺 | 不变 | 保持 | E4 |
| MEM-01 | C | 已排期→已排期 | `akitaonrails/ai-memory@2be13836`+`c83076b3` 载荷转义修复与 no-op 回归（E2） | `ls src/internal/ai/memory` 不存在；`src/cli.rs` 无 memory 命令 | VCS-native storage／privacy baseline 未实现；竞品隐私细节修复波加剧时间压力 | 扩大（竞品） | 补充完成判据 ×1 | E4 |
| MEM-02 | C | 已排期→已排期 | `rohitg00/agentmemory@e04ba88` hybrid retrieval（E2，沿用）；`ai-memory` v2.3.0 多 provider embedding（E2） | `grep -rn 'fts5\|bm25' src sql Cargo.toml` = 0 | 无本地 FTS/BM25 与有界 SessionStart 注入 | 不变 | 保持 | E4 |
| MEM-03 | C | 已验证→已验证 | `memorax-ai/memorax-code@80123b9` 拒绝 turn ID 冲突写记忆（E2，账本关闭）；`matrixorigin/Memoria@a2e1e25` 跨 schema 恢复保全数据（E2） | `src/internal/ai/history.rs:3487`；tombstone 迁移 `2026071403/04` | erase/tombstone 基础存在，consolidation／Trust Gate 未完成 | 不变 | 保持 | E4 |
| MEM-04 | C | 已验证→已验证 | `MachineWisdomAI/fava-trails@6527b6c` 紧凑 MCP 面（E1，转待验证） | `src/internal/ai/mcp/authz.rs:96`；`server.rs:46-47` 默认 None | Memory MCP 生产 authorizer 尚未接线 | 不变 | 保持 | E4 |
| MEM-05 | C | 候选→候选 | `letta-ai/agent-file@78212eb` `.af` format（E2，沿用） | `src/command/agent/skill.rs:37`；无 portable Memory export | portable export／skill projection 尚缺 | 不变 | 保持 | E4 |
| MEM-06 | C | 候选→已验证 | `akitaonrails/ai-memory@74bd791c` 跨项目 agent inbox/queue（E3，随 v2.3.0 发布） | `grep -rn 'MemoryCoordinator\|CoordinationView' src` = 0；`workspace.rs:211` WorkspaceLease 同构基础 | 协调通道问题域被竞品实证可行，Libra 侧实现为零 | 扩大（竞品） | 更新状态（候选→已验证） | E3 |
| SB-01 | SB | 实施中→实施中 | `git/git@997c1daf1d` worktree_basename 越界读修复（E2） | `src/git_protocol.rs:227 read_pkt_line` 返回 `Result`（pkt-line 已收口）；`src/internal/ai/tools/registry.rs:100` cwd `panic!` | pkt-line 切片完成；生产 panic 面未清零（ToolRegistry 等），解析越界/下溢防护需持续对齐 | 缩小（Libra）+扩大（竞品）＝双方 | 更新判据 | E4 |
| SB-02 | SB | 实施中→实施中 | `anomalyco/opencode@709af586` 拒绝后停止 run（E2） | `src/internal/ai/mcp/server.rs:46-47` authz 默认 None；`src/internal/ai/tools/utils.rs` 写重定向 needs_human | authorizer 生产接线与 shell fail-closed 仍缺；权限路径规范化与拒绝后行为缺规范 | 扩大（竞品） | 补充完成判据 ×2 | E4 |
| SB-03 | SB | 已验证→已验证 | `walgit/walgit@bf65c01` 退役权限与守恒证明（E2，参照列同时服务 LR-09） | `ensure_publish_schema` 逐语句、无事务（**历史锚点：已随 plan-20260920 RC-35 删除**）；wrangler 第二套 runner | D1 runner 仍缺事务账本与单一迁移事实源 | 不变（本轮核对锚点行号） | 保持 | E2 |
| SB-04 | SB | 实施中→实施中 | `epicgames/lore@7ccb6a1` shutdown 后调用显式失败（E2） | `grep -rn ProcessScope src tests` = 0；`src/internal/process_terminate.rs:12` ProcessTerminateGate；nextest CI `a8218ac` | 测试隔离已改善；child scope 抽象、中断清理与 shutdown 语义未统一 | 扩大（竞品） | 补充完成判据 ×1 | E4 |

不做 Top-3（按 (S+D+X) 从「不采纳/延后」候选中取）：

| 排名 | 关联编号/来源 | 内容 | 理由 | E |
|---|---|---|---|---|
| 1 | 不采纳（memorax-code） | 8h 轮询 npm 自动更新并替换进程（`ca6c46d`/`fed82ea`/`073c006`；本轮 `1491fbb`/`45a6215` 只为该形态补崩溃恢复，仍无验签） | 无验签供应链形态；Libra 升级必须走 UP-01 签名通道 | E2 |
| 2 | 不采纳（akitaonrails/ai-memory） | 首次 `run` 时自动安装 harness hooks + MCP（`da8d07dc`，默认开启 boot-time 回填 `c380278e`） | 运行时静默改写用户 harness 配置属供应链暴露；Libra 的 hook/skill 安装必须显式确认（SB-02） | E2 |
| 3 | 不采纳（deepseek-harness） | 删除 SQLite persistence backend、改用 handle-based seam 的产品形态（`4553c9d957` 本轮验读关闭） | Libra operation log 以 SQLite 为状态真源（规划原则 1/5）；handle seam 只作接口参考 | E2 |

本轮竞品要点（更新增量审计）——6 类 × {发现数, 值得借鉴数, 进入 plan-long 数}：

| 类别 | 发现 | 值得借鉴 | 进入 plan-long |
|---|---:|---:|---:|
| security | 17 | 10 | 4 |
| reliability | 15 | 8 | 2 |
| bugfix | 10 | 2 | 0 |
| compat-migration | 5 | 2 | 0 |
| improvement | 8 | 2 | 0 |
| feature | 6 | 3 | 1 |

本轮进入 plan-long 的竞品要点（≤12 条；对应差距矩阵动作 ≠ 保持的行）：

- **LR-01** lore `a03a32a`：拒绝同一仓库的重叠 link 挂载（E2，含测试）——worktree/挂载入口必须显式拒绝重叠注册。
- **LR-01** sapling `a3b03945ae9`：非正常关机后 overlay 陈旧 inode 元数据被新 inode 继承（fuzz 发现，E2）——崩溃重启后不得继承旧元数据。
- **LR-09** walgit `bf65c01`：精确 pack 覆盖快照 + 持久退役权限，退役前证明索引守恒（E2）。
- **MEM-01** ai-memory `2be13836`+`c83076b3`：hook 载荷 JSON 控制字符转义，修复在 BusyBox awk 上曾是 no-op，跨三种 awk 实现回归（E2）。
- **MEM-06** ai-memory `74bd791c`：跨项目 agent inbox/queue + 启动通知（E3，随 v2.3.0 发布）——协调通道可行性实证。
- **SB-01** git/git `997c1daf1d`：`worktree_basename()` 零长度路径越界读修复（E2）——边界解析须先判空。
- **SB-02** opencode `709af586`：权限被拒绝后 run 必须停止而非继续（E2，10 个测试文件）。
- **SB-02** opencode `fd9ee435`：home 相对权限路径先展开再匹配（E2）。
- **SB-02** fava-trails `094af6b`：Trust Gate 扫描遇深嵌套 fail closed（E2）。
- **SB-04** lore `7ccb6a1`：shutdown 后的调用显式失败而非挂起（E2）。
- **SB-04** letta-code `feb32e33`：慢 Bash 命令自动后台化并在完成时通知（E2）——阻塞执行须有生命周期出口。
- **SB-02** fava-trails `c91d644`：持久化与远端评审前拒绝明显秘密（E2）——秘密门在写入之前。

Libra 自身（HEAD `9da06b4bf700472781c2e76ec48e96815475caf3`，`Cargo.toml` version `0.22.47`，审计日期 2026-09-17；自上次审计基线 `1524ecab` 起 `libra log --oneline 1524ecab..HEAD` 共 91 条：feat 6 / fix 26 / test 4 / docs(plan) 48 / docs 2 / other+merge 5，已发布版本 = `v0.22.47`（本周期发布 v0.22.20..v0.22.47 共 28 个 tag），未发布提交 = `libra log --oneline v0.22.47..HEAD` 共 3 条）：

- **SB-01 / plan-20260901**：**pkt-line 切片已收口**——计划完成（`07ba2d9` docs(plan): complete pkt-line hardening plan； nineteen 卡、十六发布、v0.22.47）；代码 `read_pkt_line` 返回 `Result`（`src/git_protocol.rs:227`，`PktFrameError::LengthBelowHeader/LengthAboveMaximum`），异步帧校验（`6a8ce38`、`1edca32`、`6062118`、`fdcf979`）、discovery 错误传播（`3509b18`）、push 状态行归类（`842d2e6`、`da8237e`）、SSH stderr 脱敏（`2a8594d`、`560cbfc`）；文档 `docs/error-codes.md LBR-NET-002`；unwrap 守卫扩展为 7 个测试文件。**状态保持实施中**：`src/internal/ai/tools/registry.rs:100` 在 cwd 解析失败时 `panic!`，生产 panic 面未清零。
- **LR-02**：显著缩小——operation v2 `RepoViewV2`/`WorkspaceSnapshotV2` 随 v0.22.44+ 发布（`libra ls-tree v0.22.44 -- src/internal/operation` 22 文件）；`9da06b4`（#485）合入 crash-safe restore engine、append-only undo/redo/revert、operation doctor 并关闭 M2/M3 operation 任务；`src/command/op.rs:183-192` 已有 `restore_v2` 事件面。状态保持实施中（未发布 + 发布验收未收口）。
- **LR-03**：**已排期→实施中**——sidecar Change ID 模块 `src/internal/change/{identity,genealogy,store,resolve,builder,workflows}.rs` 与 `tests/command/change_revision_provenance_test.rs` 随 `9da06b4` 合入（HEAD，**未发布**，v0.22.47 无 `internal/change/`）；`plan-20260822.md:123` ADR-OL-04 冻结 sidecar-only（不写 commit header）。
- **LR-05**：cherry-pick 序列一致性修复（`3128bc2` HF-01、`7190507`、`9be09fe`，v0.22.39-42）与 unmerged staging（`06d0840`）；merge 主线继续收敛，versioned conflict object 仍无，状态保持实施中。
- **CT-01**：仍实施中；本轮 `tests/compat-ledger/t4` 仍 34 toml 无新 wave；DEFER-09 关闭表述沿用第 11 轮。
- **UP-01 / RT-01**：保持已实现；RT-01 的产品表面（`libra code`/Web 执行器）已由 plan-20260920 拆除（本周期 28 个 tag 均经签名链发布，属既有四证据的持续兑现，不重复登记）。
- **SB-02 / plan-20260830**：SBX-01..05 已合入维持；authorizer 生产仍未安装（`server.rs:46-47` 默认 None）。**SB-04 / plan-20260827**：nextest CI 与序列注册维持；`grep -rn ProcessScope src tests` = 0（child scope 仍缺）。
- **Memory**：仍无实现——`ls src/internal/ai/memory` 不存在、`src/cli.rs` 无 memory 子命令、`grep -rn 'fts5\|bm25'` = 0、`grep -rn 'MemoryCoordinator\|CoordinationView' src` = 0；MEM-01/02 维持已排期，MEM-06 本轮由竞品证据推进为已验证。
- **未发布变更（v0.22.47..HEAD，3 条）**：`9da06b4` operation/change genealogy milestones（上两行）；`06d0840` add unmerged staging、`-u` pathspec 检查、literal-pathspecs（用户可见行为变更）；`07ba2d9` pkt-line 计划收口文档。CHANGELOG `[Unreleased]` 另有 isolated agent task 单一 `agent.task.sync-back` operation 语义与 operation-v2 HEAD pinning（触及「兼容与迁移」「数据正确性」门禁，须随发布补迁移/回滚证据）。
- **stale facts 更正**：第 10 轮遗留的 `ssh.strictHostKeyChecking` 文档债已闭合（`COMPATIBILITY.md:617` 与 clone/config/fetch/push 命令文档均已记录）；plan-20260901 已完成（索引状态同步更新）。
- **日期计划对账**：磁盘含 `plan-20260918.md`（`add` 收口，排在 `issues/477` 之后）与 `plan-20260917.md`（cargo-test 进程内剥落）；索引已补齐 `plan-20260902`..`plan-20260918` 与 [`plan-20260921.md`](plan-20260921.md)（原 `plan-20260919-gpg-import.md`，R29 双 PASS，尚未开工）；`plan-20260916.md`（Mega remote Agent Capture 客户端）为设计态；`plan-20260901` 状态由实施中更新为已完成（其自述收口门与 DEFER-02 登记以其修订史为准）。
- deepseek-harness bridge：`plan-20260818.md` 事实不变；本轮复核上游 `session/created|event|flush|disposed` 事件面仍在（deepseek 上游 `packages/core/session/src/index.ts` 的 50–81 行），bridge 按方法分发、不锁定 `SESSION_FORMAT_VERSION`（现 v3），事件面依赖成立；载荷字段级兼容列入待验证账本。
---

## 逐竞品分析：功能重叠、Libra 优势与差异化

本节以 **2026-09-17 第十二次审计快照**为比较基线，覆盖快照内 **44 个仓库（39 Git + 5 Libra 类型）**，并单列 **5 个本轮本地恢复的历史参照**；`crabbuild/*` 五仓自本轮起本地缺失，其下文各行仅保留第 11 轮历史分析。同一组织的仓库按职责分别分析，避免把 SDK、格式、基础库和完整产品视为同等竞争者。竞品 revision、更新限制与证据入口沿用上方快照及差距矩阵；这里的分析是该基线上的产品判断，不代表重新完成远端更新或全量实现审计。

**比较口径：**「高重叠」表示争取同一核心开发工作流；「中重叠」表示覆盖其中一个环节；「低重叠」表示相邻数据领域、基础组件或生态接口。重叠程度包含长期目标，表中会明确区分 Libra 当前能力与规划。**现有优势**只指已有功能组合带来的适用性；**潜在优势**必须等相应 LR/MEM/SB 完成后才能成立。功能更多不等于性能、可靠性、安全性或用户体验更好，本节没有跨产品基准测试，因而不作这类排名。

Libra 比较基线可从 [Code UI / AgentRuntime](../../commands/code.md)、[operation 命令及恢复边界](../../commands/op.md)、[兼容性清单](../../../COMPATIBILITY.md)以及上方「Libra 自身」证据核对：Git 对象与远端互操作、worktree/lease、operation 基础、Agent 捕获与 Web runtime 已构成可复用底座；完整快照恢复、稳定 Change ID、hunk/stack 编辑与 VCS-native Memory 仍须按各自完成判据验收。尤其不能把现有 session/history 当作 MEM-01/02 已实现。

### 版本管理、存储与代码理解（19 个仓库）

| 竞品 / 角色 | 核心功能与适用场景 | 与 Libra 的功能重叠 | Libra 优势或差异化空间 | 对方长处、Libra 缺口与取舍 |
|---|---|---|---|---|
| **Sapling**（`facebook/sapling`；直接对标） | Smartlog、提交栈与大仓工作区；EdenFS/VFS 面向大型仓库的按需访问。 | **高**：日常版本管理、worktree、提交组织；大仓物化对应 LR-09。 | **现有**：同一产品中提供源码版本管理和 AgentRuntime/执行轨迹，适合希望把 Agent 工作纳入仓库流程的团队。 | Libra 尚缺同等完整的 stack/VFS 工作流；借鉴可视历史、预取与工作区可靠性，推进 LR-01/04/09，并验证部署假设。 |
| **Jujutsu / jj**（`jj-vcs/jj`；直接对标） | operation DAG、稳定 Change ID、一等冲突与后代自动 rebase；强调可重写、可恢复的开发流程。 | **高**：LR-02/03/05 的核心对标；两者都需处理 Git 互操作。 | **现有**：Agent 会话、工具执行和源码管理共处一套产品；**潜在**：让意图、记忆与变更身份共同演进。 | jj 是变更身份与恢复语义的重要参照；Libra 的 operation 基础不能等同于完整 undo/redo，须先完成 LR-02/03/05。不把 Git 互操作表述成 Libra 独占优势。 |
| **GitButler**（`gitbutlerapp/gitbutler`；直接对标） | 并行分支/workspace、hunk 归属、提交栈编辑与 Forge 协作；近期涉及 committed hunk mutation。 | **高**：并行 Agent 修改、局部变更组织与 stacked review，对应 LR-01/03/04/08。 | **现有**：源码操作与 AgentRuntime 同产品提供；**潜在**：把 hunk mutation 接入 operation 与 intent，供 Agent 稳定调用。 | Libra 缺稳定 hunk 身份、assignment 和完整 stack mutation；优先冻结机器接口及 ID 迁移契约，再完善交互体验。 |
| **Grit**（`gitbutlerapp/grit`；兼容性参考） | Git 实现及以上游测试套件治理兼容性的工程方法。 | **中**：Git 命令行为与 conformance，主要对应 CT-01。 | **现有**：Libra 的兼容账本已有分型与部分 wave 证据，同时服务其 Agent/VCS 产品面。 | Grit 的价值在可复算的兼容验证；Libra 仍需收尾 CT-01。采用净室场景与自有断言，不用功能数量代替兼容性证据。 |
| **EpicGames Lore**（`EpicGames/lore`；大仓对标） | 大二进制、sparse/virtual 工作区、批量物化与 replica 生命周期。 | **高（大仓场景）**：对象传输、merge、按需获取与 Media。 | **现有**：Git 互操作、源码命令与 Agent 工作流同产品提供；FastCDC Media 已增加分块传输基础。 | Libra 的 whole-object hydrate 与默认关闭的 FastCDC 不等于完整虚拟工作区；推进 LR-09，并借鉴内容上限、路径验证与副本回收。 |
| **Git**（`git/git`；互操作基线） | 源码历史、分支、合并、对象与远端协议，是 Libra 兼容性契约的参照。 | **高**：日常源码管理与远端协作。 | **现有**：提供集成的 AgentRuntime、执行轨迹与命令级 operation 入口，减少这些环节另行拼装的需求。 | Libra 尚有兼容性与协议健壮性缺口，不能据 Rust 实现宣称更安全；CT-01、SB-01 先保证互操作与错误处理。 |
| **go-git**（`go-git/go-git`；嵌入式实现参考） | 可嵌入应用的 Git 实现；对象、协议、commit graph 等接口。 | **中**：Git 库能力与多协议实现，产品终端用户不完全相同。 | **现有**：Libra 同时提供完整 CLI 入口与 Agent 执行工作流；这是产品范围差异，不是对 Go 库的性能优势。 | 借鉴缺口矩阵与对象解析负向测试；commitgraph 缺失父引用、循环 delta 等案例进入 CT-01/SB-01。 |
| **go-billy**（`go-git/go-billy`；文件系统基础库） | 文件系统抽象及 capability，为不同存储/测试后端提供统一接口。 | **低**：与 worktree I/O、存储适配和测试替身相邻。 | **现有**：Libra 的 worktree I/O 与仓库状态、lease、operation 有业务关联；双方不构成完整产品替代。 | 重点借鉴后端 capability 与 conformance；Libra 应显式区分原子替换、锁和同步能力，支撑 LR-01/02 与 SB-04。 |
| **Forgemark**（`entireio/forgemark`；协作格式参考） | Forge metadata 的表示与交换。 | **中（协作目标）**：review/Forge 元数据关联；Libra LR-08 尚缺机器接口。 | **潜在**：以稳定 Change ID 连接 review、intent 和提交重写，形成跨 Forge 的仓库内关联。 | 格式层可补互操作，但不能替代 PR/CI adapter；先完成 LR-03，再以 LR-08 验证一条端到端协作路径。 |
| **Dolt**（`dolthub/dolt`；数据版本相邻产品） | SQL 数据的版本、差异、分支与合并；prolly 结构支持数据存储。 | **低（用户任务）/中（底层机制）**：都管理历史与合并，但 Libra 主要管理源码。 | **现有**：源码 checkout、Git 远端与 Agent 改码是 Libra 的适用场景；不将 SQL 数据库能力视为待补齐清单。 | 学习引用自描述、GC 可达性及迁移正确性，纳入 SB-01/03；不扩展为通用 SQL 数据版本产品。 |
| **Lore VCS**（`lorevcs/lore`；意图记录相邻项目） | 围绕开发意图保留记录；与 EpicGames Lore 是不同项目。 | **中**：Libra 已有 intent/session/checkpoint 基础，长期对应 LR-06。 | **现有**：意图捕获可与既有 VCS 命令和 Agent 执行轨迹关联；**潜在**：seal、pin 与团队安全发布闭环。 | 单人项目的意图表达可提供场景参考，尚不足以证明生产成熟度；以 LR-06 的稳定身份、发布和撤销判据筛选。 |
| **Lit**（`nervosys/Lit`；相邻 VCS） | 项目以 agent-first VCS 和加密能力定位；已有审计指出密钥轮换声明与运行情况不一致。 | **中**：Agent/VCS 结合与供应链信任主题。 | **现有**：Libra 已有 UP-01 的签名升级发布证据及兼容账本，可用可验证流程说明能力。 | 不从对方加密宣传推导性能或安全结论，也不由其缺陷推导 Libra 整体安全领先；密码与迁移能力须有可运行测试，映射 UP-01/SB-01/03。 |
| **lakeFS**（`treeverse/lakeFS`；数据湖版本相邻产品） | 面向对象存储数据集的版本与分支管理，提供 S3 gateway 等访问面。 | **低（源码工作流）/中（存储）**：与 cloud、对象历史和发布授权相邻。 | **现有**：Libra 面向开发者本地源码与 Agent 改码，已有 Git 互操作入口；产品定位区别不代表数据湖能力更强。 | 借鉴对象发布与权限隔离；S3 gateway 授权绕过案例强化 SB-02/03，不据此新增数据湖产品线。 |
| **WalGit**（`tobi/walgit`；服务端托管参考） | 对象存储 WAL、lease 与 Git hosting；仓库写入和管理授权。 | **中**：remote/cloud、并发写入和恢复机制。 | **现有**：Libra 覆盖客户端源码管理与 AgentRuntime；**潜在**：将本地 operation 与远端发布的审计关联。 | hosting 与 Libra 只读 publish 不是同一能力；借鉴 WAL/lease 和独立 admin 权限，映射 SB-02/03、LR-08，托管成熟度仍需验证。 |
| **Compass**（`crabbuild/compass`；代码理解相邻产品） | README 描述本地符号/依赖知识图、影响分析、历史图差异、只读 CompassQL、MCP 与可视导出。 | **中**：服务开工前上下文与代码调查；Libra 的 session graph 不等同于源码结构图。 | **现有**：Libra 能承接分析后的代码修改、版本操作和执行记录；**潜在**：将结构证据接入 LR-07 preflight。 | 对方功能定位集中于确定性结构查询；本轮只补读 README，不认定全量实现已验证。先评估固定 revision 的只读结果接入，不新增图数据库真源。 |
| **Crab**（`crabbuild/crab`；大文件直接对标） | README 描述 Git pointer、内容去重分块、用户自管对象存储及 remote helper，适合模型、数据集、媒体与构建产物。 | **高（Media）**：大文件分块、去重、上传下载及对象存储；与 LR-09 相邻基础直接重叠。 | **现有**：Libra 将 Media 与源码 VCS、AgentRuntime 放在同一产品中；减少工作流切换是其整合价值。 | Crab 的专门大文件入口及多云路径值得验证；Libra FastCDC 默认关闭，不能宣称吞吐、成本或 provider 覆盖领先。以 LR-09 验证重试、完整性与按需物化。 |
| **Prolly**（`crabbuild/prolly`；数据结构基础库） | README 描述不可变有序 KV、内容寻址、结构共享、diff/merge 与可插拔 Store；`prolly-vcs` 另属提案。 | **低（产品）/中（机制）**：与对象索引、快照与增量同步相邻。 | **现有**：Libra 提供仓库/CLI/Agent 的应用语义；基础库本身不是完整替代品。 | 借鉴确定性编码与结构共享，评估格式、GC 和迁移成本；不把树级 merge 当源码 merge，也不将提案视为已交付 VCS，映射 LR-02/09、SB-01。 |
| **SILO**（`crabbuild/silo`；对象版本账本） | README 描述 S3 上的不可变对象历史、原子多文件会话、CAS refs、writer fencing、可恢复检查与 GC；payload 保持 whole-object。 | **中**：对象发布、快照、并发写入与恢复；与 Media 分块传输职责有别。 | **现有**：Libra 具备本地源码操作和 Agent 执行入口；**潜在**：形成从本地 mutation 到安全发布的连续证据链。 | 其服务商一致性前提和可恢复维护流程值得核验；不能把普通对象存储适配当作同等账本保证，借鉴 SB-03、LR-02/09。 |
| **Trail**（`crabbuild/trail`；Agent/VCS 交叉对标） | README 描述 Git 旁路的本地 operation DB、transcript/checkpoint/rewind、稳定 LineId、lane 协调、readiness 与 CLI/HTTP/MCP。 | **高**：operation、未提交工作、Agent 轨迹、并行工作区和行级归因，多项直接对应 LR-01/02/07、AG-ATTR。 | **现有**：Libra 自身承载源码 VCS 与 AgentRuntime；**潜在**：统一这些流程的身份、恢复与发布契约。 | Trail 的未提交行身份、lane readiness 值得专项验证；本轮 README 不足以证明其完整保证。Libra 尚缺对应闭环，不能仅凭原生 VCS 定位宣称领先。 |

Crabbuild 五仓的功能定位补读入口均固定到本轮快照：[Compass README](https://github.com/crabbuild/compass/blob/5a9081f931ebb11e8c6556eea29ccea1d063a503/README.md)、[Crab README](https://github.com/crabbuild/crab/blob/77a9dc8682724f4e431b0d1ca57aaab8dfae64ba/README.md)、[Prolly README](https://github.com/crabbuild/prolly/blob/6ee959eaed2625bf086ae0c4d1a2b5934f7e3872/README.md)、[SILO README](https://github.com/crabbuild/silo/blob/7f71a06b0560fb1ef85c3aa57bcac365dbe9d7be/README.md)、[Trail README](https://github.com/crabbuild/trail/blob/9823ed7551a1c53bb1a2c1dc329d9faf30e6f460/README.md)。这些补读用于说明角色和重叠范围，不提高既有审计证据等级。

### Agent 生成代码与执行生态（11 个仓库）

| 竞品 / 角色 | 核心功能与适用场景 | 与 Libra 的功能重叠 | Libra 优势或差异化空间 | 对方长处、Libra 缺口与取舍 |
|---|---|---|---|---|
| **Git AI**（`git-ai-project/git-ai`；归因对标） | 行级 agent/model/prompt 归因、checkpoint 与相关统计。 | **高（归因目标）**：Libra 已有 Agent transcript 导入，行级归因仍属 AG-ATTR 候选。 | **现有**：Libra 同时管理源码操作和执行轨迹；**潜在**：将归因连接稳定 change/intent，保留重写谱系。 | 行级来源与跨工具格式是 Libra 的缺口；先只读互操作，再验证重写后的归因。迁移事务/去重经验纳入 SB-03，不扩展遥测重摄取产品面。 |
| **Grok Build**（`xai-org/grok-build`；runtime 对标） | 隔离执行、ACP/headless、权限策略、子进程 scope 与故障注入。 | **高**：AgentRuntime、sandbox、工具执行及资源回收。 | **现有**：Libra 的 runtime 与源码仓库状态、worktree 和 operation 基础同产品提供。 | 子进程生命周期、shell fail-closed 与 ODB 工作复用值得借鉴；Libra 仍需收口 SB-02/04，不能由集成程度推导隔离保证更强。 |
| **Cursor**（`getcursor/cursor`；产品需求信号） | 本地仓库提供 issue 信号，可反映编辑器/Agent 用户的工作流问题；不含可审计的完整产品源码。 | **中（可确认的需求层）**：AI 改码、上下文与开发者反馈；无法据该仓库完成产品能力对等比较。 | **现有定位**：Libra 以仓库 CLI、Web runtime 和可核对的本地源码为入口；是否优于编辑器体验尚无验证。 | 以 issue 提取可复现需求；补充产品级证据后再比较编辑体验、模型效果与规模，不推断闭源实现或虚构缺失能力。 |
| **Mainline**（`mainline-org/mainline`；意图协作对标） | intent seal、commit pin、确定性 preflight 与 hook 上下文预算。 | **高（长期目标）**：Libra intent/checkpoint 是基础，LR-06/07 尚缺 seal/pin/pre-edit gate。 | **现有**：Libra 同时控制源码操作与执行记录；**潜在**：用 Change ID 和 Memory 召回降低意图与实际修改脱节。 | 对方直接覆盖「开工前避免重复/冲突」；Libra 应先交付确定性 overlap receipt，不以 LLM 判断代替身份与范围校验。详见 [Mainline 差距分析](../gap/mainline.md)。 |
| **Research Git**（`StepzeroLab/research-git`；研究工作流对标） | Feature Capsule、recall/compose、实验 provenance 与 ablation；reapply 依赖 LLM 非确定性处理。 | **高（研究目标）**：Libra 有 artifact/skill/intent 捕获，LR-10 的 capsule lifecycle 尚缺。 | **现有**：Git 互操作与 Agent 执行底座已具备；**潜在**：让 capsule 复用具有确定性 preview、恢复和来源证明。 | 借鉴实验比较及可移除能力单元；Libra package 尚未注册为稳定命令，不能当交付证明。按 [Research Git 分析](../gap/research-git.md)推进 LR-10。 |
| **Letta Code**（`letta-ai/letta-code`；有状态 Agent 对标） | 有状态编码 harness、memory、hooks/permissions、skills 与工作区生命周期工具。 | **高**：runtime/工具/worktree 已重叠；持久记忆召回对应尚未实现的 MEM-01/02。 | **现有**：Libra 以源码版本操作和 Agent 执行为共同入口；**潜在**：把记忆与 change/operation 的生命周期绑定。 | memory 写入限额、shell 解析和离开工作区的保护值得借鉴；先收口 SB-02/04 与 MEM-01/02，再谈跨会话体验优势。 |
| **Letta Agent SDK**（`letta-ai/letta-agent-sdk`；集成参考） | 程序化 Agent/session 使用、cloud sandbox 仓库 commit pin 与 dispose 资源释放。 | **中**：Libra runtime 控制、bridge 与工作区资源生命周期。 | **现有**：Libra 直接提供源码仓库语义和 CLI/Web 工作流；SDK 更偏宿主应用集成。 | 借鉴精确 revision pin 和资源释放契约；Libra 的 bridge 不自动等于 SDK 生态覆盖，映射 LR-06、SB-04。 |
| **Trajectory**（`letta-ai/trajectory`；轨迹格式参考） | 将多个 runtime 的 transcript 归一为可分析记录。 | **中**：Libra capture/import 已有基础，跨格式归一仍属 AG-ATTR。 | **现有**：Libra 可将导入轨迹放入仓库与 session 上下文；**潜在**：关联具体变更及重写谱系。 | 归一 adapter 是互补入口；先验证只读导入、来源保留与重复导入幂等，不强制替换现有 capture schema。 |
| **Letta Skills**（`letta-ai/skills`；内容生态） | skills/提示词材料，提供技能组织与加载使用场景。 | **低（实现）/中（入口）**：Libra 已有 skill 注册与 activation，Memory→skill 投影仍属 MEM-05。 | **现有**：Libra 能把 skill activation 纳入实际 runtime 和捕获流程；功能优势不能由提示词数量衡量。 | 此仓主要是提示词，不是安全或检索实现证据；参考分层加载需求，MEM-05 再验证可移植子集。 |
| **Agent File**（`letta-ai/agent-file`；可移植格式） | `.af` Agent 状态交换格式。 | **中（导出目标）**：Libra 有 Agent/skill 基础，portable Memory export 尚缺。 | **潜在**：导出受控子集时携带仓库来源、逻辑身份与隐私边界。 | 格式可移植性是对方直接价值；Libra 需完成 MEM-05 往返测试和兼容范围文档，导入默认成为私有 draft。 |
| **DeepSeek Harness**（`deepseek-ai/deepseek-harness`；runtime 及互操作参考） | session 事件、持久化接口与 handle 生命周期；为外部 Agent 宿主提供运行时集成面。 | **高（runtime）**：Libra RT-01 与 bridge 已有实现，session 事件可进入 Libra。 | **现有**：Libra 已提供 bridge 入站契约，并结合自身 VCS、SQLite 状态与 Web runtime；可作为跨 runtime 的仓库记录层。 | 上游 breaking 变化要求持续契约测试；保持 session 事件适配，借鉴 handle 生命周期，保留 Libra 自己的 operation 状态真源，映射 RT-01/SB-04。 |

### Memory 与跨 Agent 记忆（11 个仓库）

本组多数功能与 Libra 的**规划**重叠。当前没有 MEM-01/02 实现证据，因此「VCS-native」在这里主要是差异化机会；已有 SQLite、对象库和 session 只能证明具备建设基础。

| 竞品 / 角色 | 核心功能与适用场景 | 与 Libra 的功能重叠 | Libra 优势或差异化空间 | 对方长处、Libra 缺口与取舍 |
|---|---|---|---|---|
| **ctx-open**（`diegoxtr/ctx-open`；认知对象概念参考） | 对工程认知对象进行版本化；source-available，沿用审计的概念参考边界。 | **中（规划）**：intent/decision 基础与 Memory 生命周期目标相邻。 | **潜在**：让认知对象与源码 change、operation 和证据引用共同演进。 | 参考对象身份与版本语义，映射 LR-06、MEM-01/03；不把概念文档视为实现成熟度或复制实现的依据。 |
| **Memorax Code**（`memorax-ai/memorax-code`；编码记忆相邻产品） | 编码记忆层、按仓隔离、turn lineage 与上下文压缩；npm 分发。 | **高（记忆目标）**：跨会话上下文与来源保留；Libra 目前仍以 history/capture 为基础。 | **现有**：UP-01 提供签名升级证据；**潜在**：记忆使用仓库原生身份、可恢复操作与显式晋升。 | 仓隔离、压缩保留来源和冲突 turn 拒写值得学习；MEM-01/03 待落地。query token 与无验签自动更新作为 SB-02/UP-01 反例，不等于整体产品比较结论。 |
| **Rekal CLI**（`rekal-dev/rekal-cli`；会话记忆对标） | commit 时捕获、写前 secret 脱敏/home 匿名化、本地索引与 embedding、仅 merged 工作的共享边界；`.rekal/` 为 gitignored DuckDB 本地库。 | **高（记忆目标）**：Libra 已有轨迹捕获，缺记忆检索与共享晋升。 | **潜在**：通过类型化证据引用连接源码、记忆和 operation，复用统一写入器与隐私门禁。 | 对方提供会话到记忆的具体链路；Libra 应先交付 MEM-01/02，再补 MEM-03。不得沿用「`.rekal/` 原文全量入 Git」的旧误述。 |
| **Agentmemory**（`rohitg00/agentmemory`；Memory 主对标） | 四层记忆、BM25/vector/graph 混合检索、hook 捕获、预算注入、遗忘与跨 Agent MCP。 | **高（规划）**：覆盖 MEM-01..04 的主要用户任务；Libra 当前缺检索与巩固层。 | **潜在**：将记忆来源与源码变更、意图和可恢复操作绑定；以本地确定性召回提供基础路径。 | 功能链路是 Libra 主要缺口参照；先实现有界 FTS5/BM25 与 citation，再评估向量/图，不追逐工具数量。 |
| **Fava Trails**（`MachineWisdomAI/fava-trails`；共享记忆对标） | jj 后端共享记忆、draft/Trust Gate/晋升、op_restore 与结构化冲突。 | **高（规划）**：记忆版本化、晋升和多 Agent 协调，映射 MEM-01/03/06。 | **潜在**：直接复用 Libra 仓库、worktree/lease 与 operation，连接代码修改和记忆晋升。 | 对方已有 VCS-backed 方向，故「使用 VCS 存记忆」并非 Libra 独有；应以跨代码/记忆的一致性证明差异，避免单仓全局锁和单一 LLM Trust Gate。 |
| **Agentic Flow**（`ruvnet/agentic-flow`；编排需求参考） | 多 Agent 编排、共享记忆与 trajectory 的需求信号；本轮以宣传性材料为主，submodule 未更新。 | **中（需求层）**：Agent 编排、共享上下文与协调目标。 | **现有**：Libra 有可核对的 runtime/worktree 基础；**潜在**：用有界协调记录连接工作所有权与代码写入。 | 未验证的性能/QuantumDAG 指标不纳入比较；MEM-06 只吸收 claim/handoff/冲突声明等可测场景。 |
| **Perstate**（`graphwisdom/perstate`；状态持久化参考） | branch-as-identity、人格与状态持久化，使用 Git 同步。 | **中（规划）**：长期 Agent 身份与记忆状态；不等同于源码工作区隔离。 | **现有基础**：Libra 已有 workspace lease；**潜在**：协调条目采用 CAS、TTL 与来源身份。 | push+rebase 重试不能提供并发安全保证；借鉴使用场景，以 MEM-03/06 的冲突与过期判据验证，不能把 lease 当已完成 Memory 协调。 |
| **Memoria**（`matrixorigin/Memoria`；记忆版本对标） | 记忆 snapshot/branch/merge/rollback 与 MCP；本轮含 MatrixOne 兼容修复。 | **高（规划）**：版本化记忆、恢复与跨 Agent 访问。 | **潜在**：把记忆与源码/operation 放入关联生命周期，减少平行状态的对账。 | 「Git for memory」定位已有同类探索；Libra 需通过 MEM-01/03/04 证明晋升、恢复、隔离和召回，不把 SQLite 选型本身当优势。 |
| **Memweave**（`sachinsharma9780/memweave`；本地检索参考） | Markdown + SQLite 索引、零外部服务的 recall 基线。 | **高（首切片目标）**：本地可解释检索，直接对应 MEM-01/02。 | **潜在**：在本地检索之外加入源码来源、历史重写关联与团队晋升。 | 离线和 SQLite 都不是 Libra 独有；对方的简单本地路径值得借鉴，Libra 应先交付可用召回，再增加生命周期复杂度。 |
| **LedgerMind**（`sl4m3/ledgermind`；宣传材料/反例） | 自演进记忆管理的产品叙述；本轮源码已移除，当前可读内容为文档/品牌。 | **中（概念）**：自动巩固与遗忘目标相邻，无法核验运行行为。 | **潜在**：采用可审计晋升、确定性规则与 tombstone 形成可验证替代路径。 | 无法比较召回质量、性能或实现完整度；MEM-03 只吸收「自主变异必须可追溯、可撤销」的风险问题，不据宣传增加能力编号。 |
| **SQLite Memory**（`sqliteai/sqlite-memory`；存储/召回参考） | Markdown + SQLite 混合检索与离线同步；submodule 未更新，验证范围受限。 | **高（首切片目标）**：本地存储、混合召回与可移植数据，对应 MEM-01/02/05。 | **潜在**：结合仓库来源、白名单晋升和 operation 恢复；本地 SQLite 架构本身不是独有差异。 | 借鉴索引可重建与离线体验，核验同步边界；Libra 先保证无 embedding 配置仍可召回，再评估导出与同步。 |

### 本地恢复的历史参照（5 个仓库）

下表 5 个参照在第 10–11 轮曾本地缺失，本轮全部恢复（`agenta-ai/agenta`、`entireio/cli`、`entireio/cli-checkpoints`、`entireio/git-sync` 以 Libra 类型 clone 恢复，`cursor/agent-trace` 以 Git 恢复）；其中 cli 与 git-sync 的上次 revision 不在本地历史（基线重置），增量以本轮切片兜底审读。

| 历史参照 / revision | 功能与 Libra 重叠 | Libra 优势或差异化空间 | 缺口、证据限制与后续方向 |
|---|---|---|---|
| **Entire CLI**（`entireio/cli`；`7d16639e`） | session↔commit 链接、多 Agent review 与工作区歧义处理；与 Libra capture/runtime **高重叠**。 | **现有**：Libra 自身提供 VCS 和 AgentRuntime；**潜在**：以稳定 change/intent 关联跨会话证据。 | 沿用历史分析；seal/pin、完整 rewind 仍需 LR-02/06，不声称对方当前能力弱于 Libra。 |
| **Entire Checkpoints**（`entireio/cli-checkpoints`；`0204a02`） | refs checkpoint、rewind/resume；与 Libra checkpoint/operation **高重叠**。 | **潜在**：用统一恢复视图覆盖源码、工作区和 Agent 上下文，减少状态错配。 | Libra 现有 checkpoint 不等于完整工作区恢复；LR-02 需验证未提交改动、并发 workspace 与崩溃路径，竞品实现沿用历史证据。 |
| **Entire Git Sync**（`entireio/git-sync`；`3ee99835`） | pack relay/同步；与 Libra remote/cloud **中重叠**。 | **现有**：Libra 提供客户端源码工作流及自身协议/存储实现，具备端到端集成入口。 | 不以 relay 替代自身 remote/cloud；恢复可读 revision 后再核验认证、重试、幂等和传输能力，映射 SB-01/03、LR-09。 |
| **Agenta**（`agenta-ai/agenta`；`53717db`） | prompt/workflow 版本化；与 Libra intent/artifact/研究工作流 **中重叠**，不属源码 VCS 同类产品。 | **现有**：Libra 管理实际代码修改及对应执行轨迹；**潜在**：把实验结论连接 capsule 与变更历史。 | 仅参考实验谱系、评估结果和版本关联，不扩展成 prompt 应用平台；见 [Agenta 分析](../gap/libra-improvements-from-agenta-versioning.md)，映射 LR-10。 |
| **Agent Trace**（`cursor/agent-trace`；`2754f07`） | 文件/行级 AI 归因互操作格式；与 AG-ATTR **高重叠（目标）**。 | **现有**：Libra 有原生轨迹导入；**潜在**：把外部归因作为有来源的只读仓库证据。 | RFC 与当前实现未复核；先按 [Agent Trace 分析](../gap/agent-trace.md)验证只读适配，不写入默认 commit 语义或宣称兼容完成。 |

### 对 Libra 产品投资的含义

| 竞争主题 | 可成立的 Libra 价值 | 必须补齐的证明 | 路线图落点 |
|---|---|---|---|
| **版本管理与 Agent 执行一体化** | 已有 VCS、worktree、operation 基础与 Web runtime，用户能在同一产品中管理修改及执行来源。 | 完整恢复、稳定身份、并行 mutation 和授权闭环；jj、GitButler、Trail 已使「有操作历史」不足以构成差异。 | CT-01、LR-01..05、SB-01/02/04 |
| **开工前减少重复劳动与冲突** | intent/checkpoint、bridge 可作为输入基础；Compass 的结构证据和 Mainline 的确定性 gate 提供互补参照。 | 有界检索、精确 revision、overlap receipt 与误报/漏报基线；目前属于待交付价值。 | LR-06/07、MEM-01/02/06 |
| **跨 Agent 的仓库原生记忆** | 潜在价值是代码、意图、记忆共用可追溯身份与生命周期；Fava Trails、Rekal、Memoria 表明 VCS-backed 并非独有概念。 | 先交付本地存储/召回，再证明晋升不泄漏、遗忘可解释、重写后来源仍有效；不能用架构设计替代可用体验。 | MEM-01..06、LR-03/06、SB-02/03 |
| **源码与大文件共同协作** | 已有 Git 互操作、对象存储及 FastCDC Media 基础，适合同时涉及源码与媒体/模型产物的项目。 | 对照 Lore/Crab 验证传输完整性、重试、容量与成本，再推进 sparse/partial clone/VFS；目前无性能领先证据。 | LR-09、SB-01/03 |
| **开放格式与可靠交付** | 已有 bridge、原生轨迹导入、兼容账本和签名升级证据，可支撑生态接入。 | 格式往返、幂等、版本迁移和负向测试；Forge/PR/CI 与可移植 Memory 仍有明显缺口。 | CT-01、UP-01、LR-08、AG-ATTR、MEM-05 |

维护本节时，每个新增仓库都应补齐上述比较维度；每次将「潜在优势」改为「现有优势」须链接代码、测试、文档和发布证据。竞品功能宣告只有在完成实现核验后，才能用于调整差距等级与执行优先级。

---

## 三类能力总览

| 类 | 最要完成（按执行优先） | 既有/新增编号 |
|---|---|---|
| **A. 版本管理** | CT-01 收尾 -> LR-01 收尾 -> LR-02 -> LR-03 -> LR-04/LR-05 -> LR-08 -> LR-09（UP-01 已实现） | CT-01, UP-01, LR-01..05, LR-08, LR-09 |
| **B. Agent 生成代码** | 工程安全 SB-02/SB-04 收口 -> LR-06 -> LR-07 -> LR-10 -> 归因/trajectory -> harness bridge（plan-20260818）（RT-01 已实现） | LR-06, LR-07, LR-10, RT-01, AG-ATTR；横切 SB；日期计划 plan-20260715 / plan-20260818 |
| **C. Memory** | MEM-01 存储与隐私 → MEM-02 混合召回 → MEM-03 巩固/晋升 → MEM-04 MCP 面 → MEM-05 可移植导出 → MEM-06 并行协调 | MEM-01..MEM-06 |

横切工程门禁 **SB-01..SB-04** 适用于三类，不单独占一类名额。

```mermaid
flowchart LR
  subgraph VCS[A 版本管理]
    CT01[CT-01 Compat ledger]
    UP01[UP-01 Signed upgrade]
    LR01[LR-01 Worktree]
    LR02[LR-02 Op log]
    LR03[LR-03 Change ID]
    LR04[LR-04 Hunk/Stack]
    LR05[LR-05 Conflicts]
    LR08[LR-08 Forge]
    LR09[LR-09 Sparse/VFS]
  end
  subgraph AG[B Agent 生成代码]
    LR06[LR-06 Intent seal]
    LR07[LR-07 Preflight]
    RT[RT-01 Runtime / Code UI]
    LR10[LR-10 Capsule]
    AGATTR[AG-ATTR Attribution]
  end
  subgraph MEM[C Memory]
    MEM01[MEM-01 Store]
    MEM02[MEM-02 Recall]
    MEM03[MEM-03 Lifecycle]
    MEM04[MEM-04 MCP]
    MEM05[MEM-05 Portable]
    MEM06[MEM-06 Coordinate]
  end
  SB[SB-01..04 横切门禁]
  SB --> CT01
  SB --> RT01
  SB --> MEM01
  LR01 --> LR05
  LR02 --> LR04
  LR03 --> LR06
  LR06 --> LR07
  MEM01 --> MEM02
  MEM02 --> LR07
  MEM03 --> LR06
  LR07 --> LR10
  MEM02 --> LR10
  MEM06 --> LR07
  MEM03 --> MEM06
```

---

## A. 版本管理

### 竞品角色

| 竞品 | Libra 应学的问题 | 不应照搬 |
|---|---|---|
| Jujutsu | operation DAG、稳定 Change ID、一等冲突、descendant rebase | 放弃 Git 默认互操作 |
| GitButler | 并行 workspace、hunk 归属、change-keyed Forge、diff-anchored 元数据 | 复制其 UI 产品形态 |
| Sapling | Smartlog、提交栈、EdenFS/VFS | 绑定 Facebook 内部部署假设 |
| Lore | 大二进制、sparse/virtual、batch materialization、replica lifecycle | 另起一套对象格式 |
| Grit + git/git | 外部兼容证据账本、conformance 测试模式 | 逐字 vendor GPLv2 `t*.sh` |
| go-git / go-billy | 缺口矩阵、多后端 conformance、FS capability | 用 Go 实现替换 Libra |
| git-sync / forgemark | pack relay、Forge metadata | 替代 Libra remote/cloud |

### A 类最要完成的任务

| ID | 任务 | 优先级 | 状态 | 一句话缺口 |
|---|---|---:|---|---|
| **CT-01** | 上游 Git 套件驱动的兼容性证据账本 | P0 | 实施中 | 首个 t4 wave 与 FIX-01..05 B 段 waves 已合入并发布；**DEFER-09 已由 plan-20260825 TA-01/02 + plan-20260827 NP-00 承接关闭**（更正：非「转 blocked」）；测试并行度已落地（`a8218ac` nextest、`b6959e5`/`315132a` 序列注册）；剩余 S4 族 waves 与 S2 离线发现器（DEP-01 + SB-04 前置）；机制归 [`../gap/grit-gap.md`](../gap/grit-gap.md) GGT-00A |
| **UP-01** | 官方签名自动升级链 | P0 | 已实现 | 四证据齐备：代码 `895589d`（手动 `libra upgrade`）+ `2ea10cc`/`a0cb725`/`4bb5672`/`fc9c203`；测试 `upgrade_auto_test`（31 fn）等；文档 `docs/commands/upgrade.md`、`COMPATIBILITY.md:118`、`docs/error-codes.md LBR-UPGRADE-001`、`release-signing-auto-upgrade.md`（D1–D10）；tags v0.22.1/2/6..10（D10 首签 v0.22.7，closeout `00bc815`）。残留 DEFER-02..06 与 CHANGELOG 0.22.1..0.22.10 条目文档债 |
| **LR-01** | 完整多工作区隔离与并行 Agent 工作区 | P0 | 实施中 | W1–W2/lease/list\|show/doctor（`run_worktree_doctor`、`begin_repair_operation`）已合入；缺 parallel lanes、挂载/注册重叠拒绝、崩溃矩阵完整性、capture/export ownership 复核 |
| **LR-02** | 全命令 Operation Log、完整快照与 Undo/Redo | P0 | 实施中（PR #503 收口） | v1 已发布；v2 `RepoViewV2`/`WorkspaceSnapshotV2`、crash-safe restore/undo/redo/doctor 与多 worktree reconcile 已发布；[`plan-20260822.md`](plan-20260822.md) M2/M3/M6 已关；OL-14（Web 图）已取消；OL-15A runtime cutover 与 OL-15 v1 retirement 已在 PR #503 落地，等待远端兼容门禁 |
| **LR-03** | 稳定 Change ID 与历史重写谱系 | P0 | 实施中 | sidecar 模块 `src/internal/change/`（identity/genealogy/store/resolve/builder/workflows）+ `change_revision_provenance_test.rs` 已随 `9da06b4` 合入并随 v0.23.0 发布；ADR-OL-04 sidecar-only（`plan-20260822.md:123`） |
| **LR-04** | 非交互 Hunk API、归属与 Stack 编辑 | P0 | 已验证 | 有只读 hunk；无稳定 ID、assignment、mutation；gitbutler 本轮把未提交区 ID `zz`→`@` 并支持 committed hunk mutation（Agent 面向 ID 契约变更，E1 线索） |
| **LR-05** | 一等冲突对象与 Modeless Sequencer | P1 | 实施中 | merge 主路径、rename/D-F/octopus/mergetool/签名已随 `plan-20260903` 交付；versioned conflict object / descendant rebase 仍无 |
| **LR-08** | Forge/PR/CI 与 Stacked Review | P1 | 已验证 | 无 Forge trait、PR/CI 状态、stack mapping |
| **LR-09** | Materializing Sparse、Partial Clone、VFS Hydration | P2 | 已验证 | sparse-view 只读；hydrate 为 whole-object；无 promisor/VFS；FastCDC media transport 已合入（`ca997dd`，feature `fastcdc` 默认 OFF，`COMPATIBILITY.md:116 media`）；对象退役无守恒证明 |

### A 类完成判据（摘要）

- **CT-01**：按命令族可复算的证据账本入库；`direct`/`adapted`/`declined`/`blocked` 分型；净室边界不被突破；首批 wave 有回归。**当前进度**：首个 t4 wave 与 FIX-01..05 B 段 waves（CT1-01..CT3-06、CTF-P01..P05）已合入；**CT4-01 发布卡已执行**；DEFER-09 已关闭；测试并行度已落地（`a8218ac`）；剩余 S4 族 waves 与 S2 离线发现器待推进。
- **UP-01**：非空 `PRODUCTION_TRUSTED_KEYS`、发布签名 job、官方 install 验签；未签名包 fail closed。**已实现**（v0.22.10 四证据齐备）。
- **LR-01**：linked worktree 的 HEAD/index/sequencer/lease 崩溃与并行矩阵通过；`worktree doctor` 可诊断/修复（doctor/repair 已合入）。**本轮新增**：① 同一仓库的重复挂载/注册必须在创建入口被显式拒绝，而非静默共存（lore `a03a32a` 拒绝同仓重叠 link 挂载，E2）；② 非正常关机重启后，worktree/工作区元数据不得从已释放路径继承旧值（sapling `a3b03945ae9` 崩溃后陈旧 inode 元数据被新 inode 继承，fuzz 发现，E2）。
- **LR-02**：生产 mutation 默认进 operation log；snapshot 含恢复所需状态；`op restore` 可验证；restore/undo 不得覆盖已被 worktree checkout 且 ref 不一致的 ref（GitButler `95527608ec` 拒绝此类 oplog 恢复，防数据丢失）；oplog/snapshot 合并并发 head 后必须保留最新已保存状态、不得丢失新写入（jj `0a9b86970` stacked_table 并发写丢失修复，E2）。
- **LR-03**：rewrite 后 review/intent/Forge 仍能锚定同一 change。本轮补充：genealogy 判定须尊重 immutable heads/untracked remote tags，不得把不可变引用纳入可重写集（jj `efe0cf178`，E2）。
- **LR-04**：Agent 可非交互完成 hunk 归属与 stack 编辑，且进 operation log。
- **LR-05**：冲突可作为可版本化对象存在；modeless 继续工作；推送冲突有显式策略。
- **LR-08**：至少一个 Forge 的 PR/CI/stack 状态可从 Libra 机器接口读写。
- **LR-09**：materializing sparse + partial clone 在大仓基准下正确；失败可诊断。**本轮新增**：对象/包退役（GC/清理）前必须先证明索引可达性守恒并持持久退役权限，不得凭启发式直接删除（walgit `bf65c01` 精确 pack 覆盖快照 + 守恒证明后再退役，E2；git/git `8f909ff4e9` 陈旧 MIDX 引用已删 pack 时的恢复为反面参照，E2）。

### CT-01 分阶段契约（摘要）

CT-01 的可执行切片与任务卡在 [`plan-20260729.md`](plan-20260729.md)；机制与净室边界在 [`../gap/grit-gap.md`](../gap/grit-gap.md) 的 `GGT-00A`。本文只固定阶段名与准入关系，避免与日期计划漂移。

| 阶段 | 含义 | 本日期计划是否承接 |
|---|---|---|
| **S0** | 范围裁定与合规边界（无生产行为变更） | 是（CT0-*） |
| **S1** | **预先计划的** test-oracle / 兼容前提修复（不是「唯一」可改 Libra 行为的阶段） | 是（前两项：`config` 裸读、`update-ref` 值操作数；`.libraignore` 抑制随 S2 延后） |
| **S2** | 离线 gap 发现器（代码入库、上游语料不入库）；五分列统计随本阶段 | 否（DEFER；前置 DEP-01 许可 + **SB-04**） |
| **S3** | 兼容证据账本 schema 与守卫 | 是（CT2-*） |
| **S4** | 逐族 clean-room wave；**可经评审的 `CTF-0n` 修复迁移暴露的实现缺陷**，wave 在全绿前不得准出 | 是（t4 首个 wave：CT3-*） |
| **S5** | CI 落点与证据面（非默认阻断门） | 否（后续日期计划） |

S4 不要求 S1 全部候选项先发布：每个 wave 只以其候选集实际触及的 S1 项为行为前置。不得把 Grit/上游通过率当作完成判据；排除项必须带 `reason` / `category` / `owner` / `review_date`（实施面见 S3）。

### A 类详细规格入口

- CT-01 阶段契约见上表；任务卡、ADR、净室门与发布模型以 [`plan-20260729.md`](plan-20260729.md) 为准。
- UP-01 / LR-01..LR-05 / LR-08 / LR-09 的细规格以对应日期计划与当前代码复核为准；本文总览只保留状态与一句话缺口。
- 日期计划：[`plan-20260708.md`](plan-20260708.md)、[`plan-20260714.md`](plan-20260714.md)、[`plan-20260729.md`](plan-20260729.md)、[`plan-20260821.md`](plan-20260821.md)（UP-01）、[`plan-20260822.md`](plan-20260822.md)（LR-02/LR-03）、[`plan-20260825.md`](plan-20260825.md)（B 类 `libra code` provider 解析与凭据文案收口；其测试并行度轴由 [`plan-20260827.md`](plan-20260827.md) 承接完成）。

---

## B. Agent 生成代码

### 竞品角色

| 竞品 | Libra 应学的问题 | 不应照搬 |
|---|---|---|
| Entire CLI + checkpoints | session↔commit 链接、refs checkpoint、rewind/resume、multi-agent review、worktree ambiguity | 复制其云端产品与默认 branch 策略 |
| Mainline | sealed intent、commit pin、确定性 preflight、hook 上下文预算 | 「near-100% pin」宣传指标 |
| Grok Build | hermetic runtime、ACP/headless、ProcessScope 子进程回收、fault injection、进程级 git ODB 门控（status/diff 串行 + 快照复用） | 复制 TUI/品牌外壳为 VCS 能力 |
| Letta Code / SDK | 有状态 harness、hooks/permissions、subagent、skill 加载、`EnterWorktree`/`ExitWorktree` 工作区生命周期工具 | 把 Libra 变成通用 chatbot 平台 |
| research-git | Feature Capsule、recall/compose、ablation/provenance | 实验 DSL 绑定单一 Agent |
| agent-trace | 文件/行级 AI 归因互操作 | 未冻结 RFC 前当完成标准 |
| trajectory | 多 runtime transcript 归一为可验证记录 | 强制替换 Libra 既有 capture schema |
| Agenta | prompt/workflow 版本化（相邻） | 当作源码 VCS 对标；且本轮 dirty |

### B 类最要完成的任务

| ID | 任务 | 优先级 | 状态 | 一句话缺口 |
|---|---|---:|---|---|
| **SB-02** | 统一 AI Tool / MCP / sandbox 信任边界 | P1 | 实施中 | SBX-01..05 已合入（共享 SandboxManager transform、macOS seatbelt，plan-20260830）；authorizer 生产仍未安装（`server.rs:46-47` 默认 None=不鉴权）、shell 写重定向为 `needs_human` 非 fail-closed、权限路径规范化与拒绝后行为缺规范 |
| **SB-04** | 测试与子进程资源生命周期隔离 | P1/P2 | 实施中 | nextest CI 与序列注册已落地（`a8218ac`、`315132a`）；child scope（ProcessScope 同类：closed-scope / late-spawn kill / PID-reuse 防护）、shutdown 后调用语义与阻塞任务后台化未统一 |
| **LR-06** | Intent Seal、Intent-Commit Pin、安全团队发布 | P1 | 已验证 | 本地 Intent/Decision/checkpoint 有；seal/pin/白名单 publication 无 |
| **LR-07** | 开工前意图检索与语义冲突 Preflight | P1 | 已验证 | 缺团队 intent projection、确定性 overlap receipt、pre-edit gate |
| **RT-01** | AgentRuntime / Code UI 中立承载（日期计划） | P1 | 已实现 | [`plan-20260715.md`](plan-20260715.md) 完成判据与 Checkpoint A–D 全部满足并经 DF-05..08（v0.21.28/29、v0.22.0 breaking SSE v1 移除 `a643dfb`）收口：Code TUI 已删除、`libra code` 默认 Web Code UI、runtime 为唯一状态机 owner；剩余仅 DEFER-01..10，按各自重启条件独立立项 |
| **LR-10** | Feature/Research Capsule 与实验谱系 | P2 | 已验证 | 有 artifact/skill 捕获；无 capsule lifecycle / compare / ablation |
| **AG-ATTR** | Agent 代码归因与 transcript 归一（候选） | P2 | 候选 | agent-trace / trajectory / **git-ai（行级 agent/model/prompt 归因）**证明互操作需求；先只读导出，不改 Git 对象默认语义 |

### B 类完成判据（摘要）

- **SB-02 / SB-04**：见下文「工程安全基线」；Agent 新 mutation 不得绕过。
- **LR-06**：intent 可 seal；与 commit/change 稳定 pin；团队发布经白名单与 redaction；可撤销/tombstone。
- **LR-07**：开工前确定性 overlap receipt；可注入有界上下文；误报/漏报有可测基线。
- **RT-01**：runtime 与 TUI/Web 解耦（TUI 已退场）；审批/preflight/lease 单一事实源；plan-20260715 完成判据全部满足才算关闭。**已实现**（DF-05..08 收口，v0.22.0）。
- **LR-10**：capsule 可捕获、召回、在今日代码上安全 reapply/remove，并带 provenance。
- **AG-ATTR**：至少一种外部 transcript/归因格式可导入为只读证据；默认不污染 Git 历史。

### B 类与 Memory 的边界

- Agent **session / checkpoint / transcript** 属于 B（执行轨迹）。
- 从轨迹中**巩固出的长期事实、技能、决策偏好**属于 C（Memory）。
- Intent seal（LR-06）发布到团队前，应走 Memory 的晋升/Trust 门禁（MEM-03），避免原始 transcript 直接共享。

---

## C. Memory

### 竞品角色

| 竞品 | Libra 应学的问题 | 不应照搬 |
|---|---|---|
| **agentmemory** | 四层巩固、混合检索（BM25+vector+graph）、hook 自动捕获、token 预算注入、隐私过滤、跨 Agent MCP、遗忘/矛盾解决 | 54 工具堆砌；默认外部 embedding SaaS；与 VCS 脱节的平行数据库 |
| **fava-trails** | draft→Trust Gate→原子晋升、op_log/op_restore、结构化冲突、doctor | 单仓全局锁；把 LLM Trust Gate 当唯一安全边界 |
| **Letta agent-file / MemFS / skills** | 可移植 Agent 状态（`.af`）、git 跟踪的 memory blocks、skill 分层加载 | 把 harness 自改造成产品主线 |
| **perstate** | branch-as-identity、人格/状态持久化场景 | shell 自动 pull/push 当并发安全模型 |
| **agentic-flow** | 编排侧对共享记忆/trajectory 的需求信号 | 宣传性 QuantumDAG；不可移植封装 |
| **Memoria** | 记忆的 snapshot/branch/merge/rollback 与 MCP 面 | 「Git for memory」宣传口径；平行 DB 默认同步 |
| **memweave** | Markdown 文件 + SQLite 索引、零外部服务、recall 基线 | 单机库形态不替代 Libra VCS-native 边界 |
| **ledgermind** | 自演进记忆管理（反例：自主变异不可审计） | 无监督自主改写当默认行为 |
| **sqlite-memory** | Markdown + SQLite 混合检索、离线同步 | 默认上传托管服务 |
| **rekal-cli**（新纳入，Apache-2.0） | git-native 会话记忆全链路：commit 时自动捕获、`.rekal/` 即存储、仅 merged 共享、写入前 secret 脱敏 + home 匿名化、本地背景索引/embedding | 单仓 git-hook 捕获不含对象库/SQLite 双层与 Trust Gate；复制其「raw 会话全量入 git」形态 |
| **ctx-open** / **memorax-code**（新纳入，相邻） | 认知对象版本化（ctx-open，source-available 许可只作概念参考）；npm 记忆层产品形态（memorax-code） | 作为 Memory 主线证据；不复制实现 |

### 为什么现在升格

旧版将 Memory 竞品标为「不新增 LR」。第五次审计后变更理由：

1. agentmemory / Letta MemFS 证明「编码 Agent 的长期记忆」已是独立产品面，不再只是 VCS 的附属注释。
2. Libra 已有 session/checkpoint/skill/intent 捕获，但**没有**可检索的巩固层与跨 Agent 共享召回——LR-07 preflight 会持续缺燃料。
3. Libra 的差异化应是 **VCS-native Memory**：记忆对象、晋升与遗忘进入 SQLite/对象库/operation log，而不是再挂一个与仓库无关的记忆 SaaS。

### C 类最要完成的任务

| ID | 任务 | 优先级 | 状态 | 主要竞品证据 |
|---|---|---:|---|---|
| **MEM-01** | VCS-native Memory 存储与隐私基线 | P0 | 已排期 | agentmemory 管道；fava-trails draft 隔离；**rekal-cli `.rekal/` 本地 DuckDB + 写入前 secret 脱敏/home 匿名化**（更正：`.rekal/` 为 gitignored 本地库，非「入 git」）；letta-code `9047f71c` memory 限额 pre-commit 强制（E2）；M2 计划 [`plan-20260819.md`](plan-20260819.md)（MemoryNote/MemoryEvent、MemoryWriter 单一写入器），尚无实现合入 |
| **MEM-02** | 混合召回与会话注入（有界 token） | P0 | 已排期 | agentmemory BM25+vector+graph + provenance；SessionStart 注入；M2 首切片固定 SQLite FTS5 + `bm25()`（`libra memory search/show/status/rebuild`），尚无实现合入 |
| **MEM-03** | 巩固、衰减、遗忘与团队晋升门禁 | P1 | 已验证 | agentmemory 四层 + decay；fava-trails Trust Gate；rekal-cli「仅 merged 工作才随 push 共享」作为晋升边界证据；memorax-code `80123b9` 拒绝 turn ID 冲突写记忆（E2，账本关闭）；Memoria `a2e1e25` 跨 schema 恢复保全数据（E2） |
| **MEM-04** | 经鉴权的 Memory MCP / 机器接口 | P1 | 已验证 | agentmemory 54 tools（规模作反例）；fava `6527b6c` 紧凑 MCP 面（E1→待验证账本）；须服从 SB-02 |
| **MEM-05** | 可移植导出（`.af` / MemFS 子集）与 skill 投影 | P2 | 候选 | Letta agent-file、skills、MemFS |
| **MEM-06** | 并行多 Agent 协调 Memory（协调通道） | P1 | 已验证 | ai-memory `74bd791c` 跨项目 agent inbox/queue + 启动通知（E3，随 v2.3.0 发布）；并行工作区需求；Libra worktree/lease 基础；复用 MEM-01/03 |

### MEM-01：VCS-native Memory 存储与隐私基线

**开发者问题：** Agent 每天产生大量 tool 观察与决策，但重启或换 Agent 后只能靠 `MEMORY.md` 或口头重述；且原始 transcript 含秘密，不能直接当团队记忆。

**目标能力：**

- 以 Libra 仓库为边界，持久化 Memory 记录（逻辑 ID、来源 session/checkpoint、时间、层级、内容摘要、可选 embedding 引用）。
- 写入前强制隐私过滤（密钥、token、`<private>`、凭证路径）；过滤失败则拒绝入库。
- 原始观察与巩固后的事实分层存储；原始层默认私有。
- 所有写入可审计，并可选进入 operation log（至少晋升/删除/遗忘必须）。

**非目标：** 替换云端向量数据库产品；默认上传第三方 embedding；无鉴权的全局共享记忆。

**完成判据：**

- 本地单仓可记录、列出、删除 Memory；秘密探针不出现在存储与日志。
- 与现有 `agent session/checkpoint` 可链接，不复制第二套 session 真源。
- schema/migration 有 forward + 测试；损坏数据 fail loud。
- Memory 存储路径锚定绝对化，不得用 cwd-relative 存数据（agentmemory `e04ba88` 曾因 engine 无 cwd 导致「数据全丢」）。
- 多仓 / worker 级 Memory 数据隔离（memorax-code `5498144` 把 repo-memory worker DB 按仓隔离）。
- redaction 失败路径 fail-closed 为全量脱敏，不泄漏私密（本轮以 `letta-ai/letta-code@1d506973` 的权限／memory 边界测试作为 E2 参照）。
- Memory 写入有可配置上限（单文件字符数、目录深度）且在提交入口强制，超限写入被拒绝（letta-code `9047f71c` pre-commit 强制 memory 限额，E2）。
- 载荷转义/脱敏的修复必须带「防 no-op 回归」验证：同一修复在全部受支持实现（shell/awk/平台）上行为一致，且以负向测试证明转义确实发生（ai-memory `2be13836` 转义修复在 BusyBox awk 上曾是 no-op，`c83076b3` 修复修复并以三 awk 实现回归 `b129a684`，E2）。

### MEM-02：混合召回与会话注入

**开发者问题：** 全量塞进上下文既贵又噪声；纯关键词漏语义；纯向量丢文件名/符号。

**目标能力：**

- 确定性基线：路径/符号/BM25（或等价）检索，不依赖外部模型即可工作。
- 可选向量通道与实体图通道；融合排序（如 RRF）并做 session 去重。
- `libra code` / AgentRuntime SessionStart（或等价钩子）按 token 预算注入 top-K；预算可配置且有硬上限。
- `--json` 返回命中、分数分量、来源 citation（可追溯到 observation/session）。

**完成判据：**

- 无 embedding 配置时召回仍可用且可测。
- 注入不超过预算；citation 可人工核验。
- 与 LR-07 preflight 共享同一检索服务，不各写各的。

### MEM-03：巩固、衰减、遗忘与团队晋升

**开发者问题：** 原始观察不能当真理；过时记忆会误导；团队共享需要显式晋升而非默认同步。

**目标能力：**

- 四层或等价模型：working → episodic → semantic → procedural（命名可 Libra-native，语义对齐）。
- 巩固任务可本地、可调度；矛盾检测与 supersession 有确定性规则，LLM 仅增强。
- 衰减/遗忘 API：TTL、重要性、显式 `forget`；遗忘写 tombstone，不假装跨 clone 物理擦除。
- 团队晋升：draft → review/Trust Gate（可插拔，默认确定性规则 + 可选 LLM）→ 白名单 publication；复用 LR-06 安全发布边界。

**完成判据：**

- 巩固与遗忘有单测 + 集成测；晋升失败不泄漏私有原文。
- doctor 可报告记忆健康（膨胀、矛盾、过期）。
- 巩固 / compaction 必须保留 turn lineage 与 provenance 证据，不因压缩丢失溯源（memorax-code `039f2ec`/`1af0359`）。

### MEM-04：经鉴权的 Memory MCP / 机器接口

**开发者问题：** 多 Agent（Claude/Codex/Cursor/…）需要同一记忆面，但开放 MCP 无认证不可接受。

**目标能力：**

- 小而稳定的 Memory tool 面（search/get/put/forget/promote 量级），不是几十个平铺工具。
- 默认 loopback；非 loopback 必须认证 + fail-closed authorizer（SB-02）。
- principal 不来自模型自报；mutation 声明 approval。

**完成判据：**

- deny-all / 角色 authorizer 覆盖全部 Memory tools。
- 与 `libra agent` CLI 同源服务。

### MEM-05：可移植导出与 skill 投影

**开发者问题：** 用户希望带走 Agent 人格/技能子集，或与 Letta 等生态交换，但不想绑定单一 vendor。

**目标能力：**

- 可选导出 Memory/技能子集为开放格式（评估 `.af` 子集或 Libra 自有包）；导入为新私有 draft。
- skill 注册表与仓库内 `.agents/skills` / 捕获 skill 事件投影对齐（已有 `libra agent skill` 基础）。

**非目标：** 完整兼容 Letta 云；自动双向 sync 任意 GitHub memory repo。

**完成判据：** 至少一条导出→清空→导入→召回仍命中的往返测试；文档明确兼容范围。

### MEM-06：并行多 Agent 协调 Memory（协调通道）

**开发者问题：** 多个 Agent 在同一仓库并行执行开发工作时，缺一个共享、有界、可审计、可过期的通道来协调**所有权（谁改什么）**、**移交（做完交给谁）**、**冲突声明（哪里撞了）**与**同步点**；靠猜测、共享文件或 merge 后撞冲突都会造成重复劳动、覆盖与延迟发现。完整设计见 [`tracing/memory.md`](../tracing/memory.md) §19。

**目标能力：**

- 新增保留 namespace `coordination` 与 `MemoryCoordinator` Module（`claim`/`release`/`handoff`/`progress`/`conflict_declare`/`sync_point`），复用 `MemoryWriter` 单一 seam（§4.2.1）。
- 所有权声明用 cell CAS 保证**单写者赢**；协调条目带短 TTL 自动过期，不毒化后续工作。
- `CoordinationView` 在 SessionStart 注入（活跃声明、待处理移交、未解冲突、同步点），TurnEnd 经 Working 缓冲回写。
- 协调条目默认 ephemeral，仅达到晋升门槛（sync-point 复用、handoff 稳定）才经 consolidation + Trust Gate 巩固为持久 note。

**非目标：** 实时消息总线 / agent IM；分布式锁替代（写入冲突仍由 ref CAS / 冲突检测兜底）；默认进入 `default` 持久团队知识；复制 mainline intent-team publication。

**依赖：** MEM-01（存储/隐私）、MEM-03（Trust Gate / 巩固）；与 LR-01 worktree/lease 与 SB-02 授权边界相容。

**完成判据：**

- 单写者赢：并发 `claim` 同一 cell 恰一成功，释放后可重 claim。
- 移交闭环：A handoff → B（或 `any`）在 SessionStart 注入，B ack 后 A 释放。
- 过期不毒化：TTL 过期条目从 `CoordinationView` 排除，历史可审计、不阻塞新 claim。
- 冲突声明触发 `contradicts` 链接并进入隔离；`SecretLike`/`Confidential` 不进协调通道，actor 不信任自报。
- 协调条目从 `refs/libra/memory/*` 可重建；`MemoryCoordinator` 不绕过 `MemoryWriter`。

---

## 工程安全基线（横切）

以下不占用三类产品名额，但是 A/B/C 进入实施与发布前的门禁。

| ID | 主题 | 优先级 | 状态 | 阻断范围 |
|---|---|---:|---|---|
| SB-01 | 消除生产路径可触发 panic | P1 | 实施中 | 网络协议、仓库打开、全部 CLI |
| SB-02 | 统一 AI Tool、MCP、sandbox 信任边界 | P1 | 实施中 | `libra code`、MCP、AgentRuntime、Memory MCP |
| SB-03 | D1 schema 迁移原子性与单一事实源 | P1 | 已验证 | publish、cloud、Worker |
| SB-04 | 测试进程共享状态与资源生命周期隔离 | P1/P2 | 实施中 | CI、并行测试、Agent child 回收 |

要点（完整修复要求仍以代码审计为准）：

- **SB-01**：pkt-line / DB / HEAD / ToolRegistry 全面 fallible；生产 `unwrap`/`expect`/`panic!` CI 守卫；pack/delta 路径须环检测 + 深度上限 + 溢出防护（go-git `e258d68a` 循环 delta 栈溢出、git/git pack/delta `size_t` 宽化 `d50ac11724`/`58f35eea9b`）、对象/内容尺寸上限（lore `07b75f6`/`fd6d075`）、未检查返回值须显式处理（git/git Coverity 批次）、协议 v2 服务端解析须防 NULL 解引用（git/git `serve` NULL-deref 崩溃修复）；编码/引用外部对象时对「不在索引内的引用」显式报错，禁止静默写零值或 nil 解引用（go-git `2ef9e4b0`，E2）；带外引用值须在节点内自描述，防被 GC 误回收（dolt `01dea76505`，E2）。Libra 的 `src/utils/storage/load_cost/pack.rs:15` 已有 `MAX_DELTA_DEPTH` + 环检测 + `MAX_VALIDATED_DELTA_BYTES` + `checked_add`，写/`index-pack` 路径须保持同级别防护。**pkt-line 切片已收口**（plan-20260901 完成，v0.22.47：`read_pkt_line` 返回 `Result`、`PktFrameError` 下界/上界、`LBR-NET-002` 文档化）。**本轮新增**（聚合 ≤3）：① 生产 panic 面继续清零——`src/internal/ai/tools/registry.rs:100` 在 cwd 解析失败时 `panic!`，须改显式错误（E4）；② 字符串/路径边界解析须先判空再取切片，禁止 `name-1` 式越界读（git/git `997c1daf1d` worktree_basename 越界读，E2）；③ 并发合并共享结构后必须保留最新已保存状态，不得丢失新写入（jj `0a9b86970`，E2，沿用）。
- **SB-02**：非 loopback MCP 强制认证；authorizer fail closed；shell `env_clear`；写权限对「无法提取目标的写重定向」（`> $OUT`）fail-closed（grok-build `shell_access.rs` `unextracted_write_redirect`）；shell 命令解析遇「不可解析片段」（如尾缀 `&&`/`||`）必须 fail-closed 拒绝而非放行（letta-code `3785e254`，E2）；破坏性操作的授权档位须独立于写权限（walgit `527c7d1` 仓库删除 require_admin，E2）；认证 token 不得接受来自 URL query string（memorax-code `request.ts` 反例，E2）；secret 集中管理面（letta-code `letta secret` `70955190`）；mutating tool 真审批；apply_patch TOCTOU 收敛。**本轮新增**（聚合 ≤3）：① 权限被拒绝后当前 run 必须停止，不得以降级模式继续执行后续工具（opencode `709af586` stop after declined permissions，E2）；② 权限/信任路径匹配前必须先展开与规范化 `~`/相对路径，再与受控目录比较（opencode `fd9ee435` home 相对权限路径展开，E2）；③ 持久化与外发前的秘密门必须在写入之前生效，且对不可解析的深嵌套结构 fail closed（fava-trails `c91d644`+`094af6b`，E2）。SBX-01..05 已合入（plan-20260830），authorizer 生产接线仍缺。
- **SB-03**：D1 迁移单一事实源；禁止逐语句半迁移窗口。迁移脚本必须逐脚本原子提交（崩溃后不得留下「半迁移永久失败」状态），且去重约束升级须先迁移存量数据（git-ai `1bc9d49e2`，E2）——`src/utils/d1_client.rs:3286` 逐语句执行正是其反面；wrangler 第二套 runner（`src/command/publish.rs:627`）须收口；退役/清理类维护操作须先证明守恒再执行（walgit `bf65c01`，E2，同 LR-09）。
- **SB-04**：统一 env/CWD/DB/child/server fixture；对齐 Grok `ProcessScope` 的 closed-scope / late-spawn kill / PID-reuse 防护；中断/取消时清理阻塞子任务与流（letta-code `ff0e2158`/`356d54fb`/`46c23664`、`d490443f` silent stream 恢复）；连接/子进程断开不得在持锁状态下触发同步回调重入自死锁，断开后 pending 请求须显式失败并可诊断（sapling `bf0537023d6`，E2）。**本轮新增**：① shutdown 之后的调用必须显式失败（RESOURCE_EXHAUSTED 类错误）而非无限挂起（lore `7ccb6a1`，E2）；② 长阻塞命令须有后台化 + 完成通知的生命周期出口，不得占死交互会话（letta-code `feb32e33`/`cf3be1ec`，E2）。nextest CI 与序列注册已落地（`a8218ac`、`315132a`）；child scope 抽象仍缺。

---

## 实施顺序

### 下一个执行任务（全局）

1. **CT-01 收尾**（版本管理）：CT4-01 发布卡已执行（v0.21.21）；DEFER-09 已承接关闭；剩余 CT 后续 S4 族 waves 与 S2 离线发现器（DEP-01 + SB-04 前置）。
2. ~~**UP-01**（版本管理）~~：**已实现**（v0.22.10，四证据齐备）；残留 DEFER-02..06 与 CHANGELOG 文档债按各自条件处置，不再占据执行队列。
3. **LR-02/LR-03**（版本管理）：按 [`plan-20260822.md`](plan-20260822.md) 执行；v2 `RepoViewV2`/`WorkspaceSnapshotV2`、crash-safe restore/undo/redo、多 worktree reconcile（OL-13）与 sidecar Change ID 已发布；OL-14 已取消，OL-15A runtime cutover 与 OL-15 v1 retirement 已由 PR #503 收口，剩余为远端兼容证据与计划记账。
4. ~~**RT-01 收尾**（Agent 生成代码）~~：已实现——plan-20260715 完成判据全勾选并经 plan-20260824（DF-01..DF-09，v0.22.0）收口；后续按 DEFER-08 等重启条件独立立项。
5. **SB-01/SB-02/SB-04 收口**（横切）：SB-01 的 pkt-line 切片已随 plan-20260901 完成收口（v0.22.47），剩余生产 panic 面清零（如 `registry.rs:100` cwd panic）作为后续日期计划候选；SB-02 的 authorizer 生产接线与 SB-04 的 child scope 抽象是下一批日期计划候选。
6. **B 类 Code provider / 凭据 UX**：plan-20260825 已完成（逐卡 review-PASS，发布按 2026-08-30 豁免裁决闭合）。
7. **MEM-01/MEM-02**（Memory）：按 M2 计划 [`plan-20260819.md`](plan-20260819.md) 执行首个纵向切片；不得在 SB-02 完成前开放非 loopback Memory MCP。
8. ~~deepseek-harness bridge（plan-20260818）按其任务卡排期执行~~：Libra 侧已完成（LB-01..LB-07，`v0.21.1`）；本轮复核 deepseek 上游 session 事件面未变，bridge 无需变更；TypeScript `@libra-tools/dsh-bundle` 在兄弟仓 `REL-TS-01`。M2 不得再抢 `agent bridge` 面。

### 阶段零：工程安全

SB-01 → SB-02 → SB-03 → SB-04（可部分并行；负向门禁见旧审计：禁止新 panic、禁止无认证远程 MCP、禁止第二套 D1 runner、禁止散落测试 env mutation）。

### 阶段一：版本管理安全并发

LR-01 收尾 → LR-02 → LR-03。

### 阶段二：版本管理变更组织

LR-04 → LR-05；并行推进 LR-08 设计。

### 阶段三：Agent 意图与运行时

LR-06 -> LR-07（RT-01 / plan-20260715 已完成，不再是本阶段前置）；Memory MEM-01/MEM-02（plan-20260819 M2 切片）向 LR-07 供数；deepseek-harness bridge（plan-20260818）按任务卡独立排期。

### 阶段四：Memory 巩固与规模

MEM-03 → MEM-04；LR-09；LR-10；MEM-05 / AG-ATTR 按需；MEM-06（并行协调）依赖 MEM-01/03，可与 LR-01 worktree/lease 并行推进设计。

---

## 跨功能验收门禁

### 数据正确性

- refs/HEAD/index/sequencer/worktree/memory 晋升 mutation 要么完整成功要么可验证回滚。
- SHA-1 与 SHA-256；不硬编码 OID 长度。
- side projection 可从真源重建。

### 安全与隐私

- 外部 Agent、Forge、Memory 导入、远端 intent 均不可信。
- 进终端/prompt/对象库/SQLite/日志/MCP/publication 前：cap、validation、redaction、provenance、authorization。
- 不宣称无法证明的跨 clone 物理擦除。

### 机器接口

- 新公共命令稳定 `--json`/`--machine`；新错误稳定 `LBR-*` 并同步 `docs/error-codes.md`。
- 列表有界；检索有 token/超时上限。

### 兼容与迁移

- Git 默认行为变更有显式窗口；新元数据丢失时降级或 fail loud。
- `COMPATIBILITY.md`、命令文档、`tests/INDEX.md`、compat 测试同步。

### 性能

- 热路径不因 Memory/intent 默认全历史扫描。
- 大 transcript/embedding/VFS 流式或内容引用；承接 plan-20260713 DEFER-DR-02 的存储重构约束。

---

## 不进入本长期优先队列的项

- 以「更接近 100% Git flag parity」为唯一理由的长尾 flag（submodule 全家桶、octopus、reftable 互操作等）——登记在兼容文档与 CT-01 账本 `declined`，不自动提级。
- 复制 Agenta 的 prompt/workflow 应用版本平台。
- 复制 Grok/Letta 的完整产品外壳或自修改 harness 哲学。
- 把 fava-trails 单仓锁或 agentmemory / Memoria / ledgermind 的平行 DB 当 Libra 并发/存储模型。
- 逐字迁移 Grit/Git GPLv2 测试资产（CT-01 净室边界）。
- 未冻结的 agent-trace RFC 直接写进默认 commit 元数据。

---

## 日期计划索引

| 日期计划 | 主要归属 | 当前状态 | 说明 |
|---|---|---|---|
| [`plan-20260708.md`](plan-20260708.md) | A（LR-04/05/09 相邻基础） | 已完成 | 主线记为历史完成，活跃残留另行排期；不关闭对应 LR |
| [`plan-20260713.md`](plan-20260713.md) | B（LR-06/07/10 捕获前置） | 已完成 | 不覆盖 seal/preflight/capsule |
| [`plan-20260714.md`](plan-20260714.md) | A（UP-01、LR-01）+ 横切 | 已完成 | Part A 已迁移至 plan-long UP-01（已实现）；Part C W1–W4 已勾选、Part D 残留由 LR-01/LR-02 承接 |
| [`plan-20260715.md`](plan-20260715.md) | B（RT-01） | 已完成 | W0–W6 主线、W5-01 家族（v0.20.0 breaking minor）与正交 WIO-01..03 / W6-03 全部合入；W5-04/05/10 与 W6-01/02 已收口（v0.21.19 正式关闭），完成判据与 Checkpoint A–D 全部勾选；不覆盖 DEFER-01..10（含 SSE v1 物理移除 DEFER-08，部分由 plan-20260824 承接） |
| [`plan-20260729.md`](plan-20260729.md) | A（CT-01） | 实施中 | 首个 t4 wave（含 `t4_port_test.rs`）与 FIX-01..05 B 段 waves（CT1-01..CT3-06、CTF-P01..P05）已合入；**CT4-01 发布卡已执行**（v0.21.21）；DEFER-09 已由 plan-20260825 TA-01/02 + plan-20260827 NP-00 承接关闭；不覆盖 S2 离线发现器、S5 CI 落点与其余族 wave |
| [`plan-20260818.md`](plan-20260818.md) | B（deepseek-harness bridge） | 已完成 | `libra agent bridge --stdio` 唯一标准入站面；LB-01..LB-07 全部合入，protocol v1 的 20 个 method 自 `v0.21.1` 起全部实现（`v0.21.0` 首发）；不覆盖 MCP/旧工具服务器恢复，TypeScript 侧 `@libra-tools/dsh-bundle` 归兄弟仓 `REL-TS-01` |
| [`plan-20260819.md`](plan-20260819.md) | C（MEM-01/02） | 已排期 | M2 研发历程记忆首个纵向切片（MemoryNote/MemoryEvent、MemoryWriter、FTS5/BM25、`libra memory` 命令面）；实现未开始；不覆盖 MCP 面、向量检索、团队同步与 MEM-03..06 |
| [`plan-20260821.md`](plan-20260821.md) | A（UP-01） | 已完成 | 客户端与发布 CI 侧全部落地（trust table、generation floor、`release.yml` OIDC publish、install 验签）；closeout `00bc815`（2026-09-01）；D10 首签随 v0.22.7、v0.22.8 收全绿 run；残留 DEFER-02..06 与 CHANGELOG 0.22.1..0.22.10 文档债 |
| [`plan-20260822.md`](plan-20260822.md) | A（LR-02/LR-03） | 实施中（PR #503 收口） | OL-01..13、CH-01..04 全部 `done/complete`；**OL-14（Web 图）已取消**；OL-15A `done/complete`、OL-15 `done/remote-pending`（v1 runtime retirement 已实现，等待 compat-offline-core） |
| [`plan-20260824.md`](plan-20260824.md) | B（RT-01 延后项收口） | 已完成 | 承接 0715 的 DEFER-01/08/10 与 skill activation 残差；DF-01..DF-09 九卡全部 done/complete（文档事实源、fix bridge、SSE v2 默认、skill activation provider 消费、v1 物理删除）；DEP-02 以 v0.21.29 满足，v0.22.0（minor，breaking：SSE 仅支持 wire v2）已发布 |
| [`plan-20260825.md`](plan-20260825.md) | B（Code provider / RT-01 后续） | 已完成 | `libra code` provider 解析与凭据文案收口全部落地（凭据探测三态、`code.defaultProvider`、生效 provider 标签单源、会话 provenance 与 `--resume` 继承）；TA-03/06/07 由 plan-20260827 承接完成；发布面按用户 2026-08-30 豁免裁决闭合（代码已随 v0.21.28..v0.22.0 实际发布） |
| [`plan-20260827.md`](plan-20260827.md) | 横切（SB-04 测试并行度与序列注册） | 已完成 | NP-00..05 六卡全部 complete（nextest 离线 CI face `a8218ac`、串行注册 `315132a`、TA-03/06/07 承接）；D 组 CI 证据环境受阻部分按 backfill 窗口记录 |
| [`plan-20260830.md`](plan-20260830.md) | 横切（SB-02 sandbox export） | 已完成 | SBX-01..05 五卡 done/locally-accepted（共享 SandboxManager transform、macOS seatbelt OpenCode export）；ER-13 全量收口门绿（2026-09-01）；DEFER-SBX-06 发布步延后 |
| [`plan-20260901.md`](plan-20260901.md) | 横切（SB-01 pkt-line fail-closed） | 已完成 | 原PKT01..14及FIX-PKT01..05全部done/complete；十六实际发布C/D完整，家族02/03/04由05覆盖，最新v0.22.47。所有卡D后新的无过滤默认全量10221/10221（231 binaries、零失败/重试）和原113具名门及FIX05四新增门通过，Codex/Claude最终PASS；同步/异步协议与CLI分类、SSH诊断/BatchMode、push ng净化、空仓库尾部帧校验及五恢复修复全部交付。失败历史与全部DEFER条目/具名P2如计划，不冒称历史SIGKILL根因已查明或已本地安装。 |
| [`plan-20260902.md`](plan-20260902.md) | B（OpenCode artifact／memory） | 实施中 | 计划仍有未完成卡；以当前计划状态为准 |
| [`plan-20260903.md`](plan-20260903.md) | A（LR-05 merge） | 实施中 | MG-01..MG-21 已有代码、测试与发布提交；最终计划收口与 deferred 差异仍待完成 |
| [`plan-20260904.md`](plan-20260904.md) | B（Codex reasoning） | 已排期 | 设计计划，任务卡尚未完成 |
| [`plan-20260905.md`](plan-20260905.md) | B（Claude hooks/reasoning） | 已排期 | 设计计划，任务卡尚未完成 |
| [`plan-20260906.md`](plan-20260906.md) | 横切（安全扫描） | 已排期 | 设计计划，任务卡尚未完成 |
| [`plan-20260907.md`](plan-20260907.md) | 横切（BLAKE3 object format） | 已排期 | 设计计划，任务卡尚未执行；与 Media 计划的边界见其关系表 |
| [`plan-20260910.md`](plan-20260910.md) | 横切（数据库迁移作用域） | 已排期 | 设计计划，任务卡尚未执行 |
| [`plan-20260911.md`](plan-20260911.md) | B（hook boundary） | 已排期 | 设计计划，任务卡尚未执行 |
| [`plan-20260912.md`](plan-20260912.md) | B（memory boundary） | 已排期 | 设计计划，任务卡尚未执行 |
| [`plan-20260913.md`](plan-20260913.md) | A（LR-09 FastCDC Media） | 已排期 | 设计计划，任务卡尚未执行；Libra 侧以前置 `plan-20260907` 完整收口为准 |
| [`plan-20260916.md`](plan-20260916.md) | B（Mega agent capture-push） | 已排期 | 承接 monoengine `DEFER-AC-01`：新增 `libra agent capture-push` HTTP 客户端；双评审 PASS，任务卡尚未执行 |
| [`plan-20260917.md`](plan-20260917.md) | 横切（cargo-test 进程内剥落） | 已排期 | 收口与 nextest 分组无关的 `--lib` 串行锁对齐 + `command_test` 高并行 spawn；禁止改 nextest 成员 |
| [`plan-20260918.md`](plan-20260918.md) | 横切（`add` 命令收口） | 已完成（WT-06/WT-07 blocked） | 合并原 issues/469、484、489、491-494 及 490/476/470 的 add 卡。23/25 卡 `done`/`complete`（SW-06→v0.23.25/27、FM-03→v0.23.26/27、FM-04→v0.23.27、WT-05→v0.23.28，其余见各卡 D 组记录）；WT-06/WT-07 因 DEP-AD-11（[`issues/476.md`](issues/476.md) 全部 `pending`）按依赖失败策略保持 `blocked`。`add -p` 仍由 477 Phase 4 交付 |
| [`plan-20260920.md`](plan-20260920.md) | 横切（拆除 `libra code` / Publish / Worker） | 实施中（收尾） | 公开 Code/Publish 表面已随 0.23.0 删除；内部 SCC、leftover、Code UI 测试面与 `worker/` 已删；剩余 RC-32 文档收口 |
| [`plan-20260919.md`](plan-20260919.md) | 横切（global 配置迁到 XDG） | 已排期 | 用户 2026-09-19 裁决：global config DB + 全域 vault unseal key 迁到 `<XDG_CONFIG_HOME|~/.config>/libra`（macOS 同）；旧库首次使用自动迁移并保留备份；`~/.libra` 仍为 `LIBRA_HOME`；四个 `independent` 卡、`patch` 发布 |
| [`plan-20260921.md`](plan-20260921.md) | 横切（GnuPG HOME 密钥导入仓库 vault） | 已排期 | 2026-09-21 由 `plan-20260919-gpg-import.md` 改名。用户 2026-09-19 指示 Codex+Claude 双评审：**R29 同版双 `PASS`（P0/P1/P2 全 0）**；15 卡（家族 REL-VG-01 + 四张独立 patch VG-06/07/08/14），任务卡尚未执行，Phase 0 剩余项：DEP 复核、`gpg --version` 证据、VG-00 go 结论、ADR Accepted |
| （待建）Memory 后续日期计划 | C（MEM-03..06） | 未建 | 待用户独立编写；M2 切片落地后按证据再议 |

---

## 已替代 / 不采纳 / 已实现摘要

### 已替代

- 无整项替代。旧表述「Memory 竞品不新增 LR」被本版 **MEM-*** 升格替代；原「相邻参考」判断对实现细节仍有效。

### 不采纳

- 不把 Agenta 当源码 VCS 对标（本轮工作区 clean、可更新，但其本地 revision 前移过程非本审计执行，仍不作强证据）。
- 不把 Grok portable agent definition / TUI 复制为新 VCS LR（可作 SB-02/SB-04 证据）。
- 不采用 Grok hook 通用 fail-open。
- 不采用 Grit 二元 skip 元数据与「绝不修改测试」原文策略；CT-01 用分型账本。
- 不逐字 vendor GPLv2 测试。
- 不采纳未经限定的竞品宣传指标作为完成判据（含 Lit 的「agent-first VCS / 后量子密码」口径、agentic-flow 宣传指标）。
- 不复制 ctx-open（source-available 许可）的认知对象实现，也不复制 dolt 的 SQL 数据版本面为 Libra 能力（相邻参考）。
- 不把 rekal-cli 的「raw 会话全量入 git」当 Libra 存储形态--Libra 保存有界摘要 + 类型化证据引用（M2 计划口径）；rekal 的「仅 merged 才共享、写入前脱敏」作为 MEM-01/03 边界证据。
- 不把 Grok 进程级 git ODB 门控（`git_odb.rs`/`git_gate.rs`）当作 Libra 的并发模型照搬——Libra 的 SQLite 状态与对象库访问路径不同；其「相同 in-flight 工作 join + 短快照复用 + 超时不取消」可作 SB-04 资源生命周期与 LR-01 并行工作区性能的参考。
- 不把 Letta `EnterWorktree`/`ExitWorktree` 的「跨 Agent 锁释放 + 拒绝未合入改动删除」直接复制为 Libra 的 worktree 语义——Libra 已有 `worktree doctor`/lease 模型；其「离开前释放锁、删除前拒绝未合入改动」是 LR-01 完成判据的补充证据。
- 不把 MEM-06 协调通道实现为实时消息总线 / agent IM / 分布式锁替代：它只协调工作所有权（CAS 单写者赢），真正写入冲突仍由 ref CAS / 冲突检测兜底；不承诺实时投递，也不替代 mainline intent-team publication。
- 不把 git-ai 的 `reingest` 遥测 daemon / usage 计费重摄取当 Libra 能力（Libra 已有 `usage` 统计，遥测 daemon 与重摄取 DB 与 VCS 长期能力无关；第十次审计复核：本轮 +107 提交中 `src/token_usage` 70 文件、`src/daemon` 45 文件，均为该面）；其 claude hook 工具名大小写、zizmor CI 门禁为低价值参考，不纳入。
- 不采纳 memorax-code 的 8h 轮询 npm 自动更新并替换进程形态（`ca6c46d`/`fed82ea`/`073c006`，本轮 `1491fbb`/`45a6215` 仅补崩溃恢复，仍无验签证据）：Libra 升级必须走 UP-01 签名 stable 通道，禁止无验签的运行中自更新。
- 不采纳 ai-memory 在首次 `run` 时自动安装 harness hooks + MCP 的形态（`da8d07dc`，默认开启 boot-time 回填 `c380278e`）：运行时静默改写用户 harness 配置属供应链暴露；Libra 的 hook/skill 安装必须显式确认并走 SB-02 边界。其 hook 载荷转义的跨实现回归方法（`2be13836`/`c83076b3`/`b129a684`）与跨项目 inbox/queue（`74bd791c`）分别作为 MEM-01 判据与 MEM-06 证据吸收。
- 不采纳 deepseek-harness 删除 ApiProxy / SQLite persistence backend 的形态作为 Libra operation 存储参照（`4553c9d957`/`4f00a8b82a`）：Libra operation log 以 SQLite 为真源（规划原则 1/5），其 handle-based seam 只作接口设计参考。

### 已实现

- 无 LR-01..LR-10 / MEM-01..MEM-06 满足全部长期完成判据。部分基础（worktree、operation、Agent capture、sparse-view、Code Web-only UI 默认化）据实记录在总览，不提前关闭整项。
- **CT-01 部分落地**：首个 t4 wave（含 `t4_port_test.rs`）与 FIX-01..05 B 段 waves、**CT4-01 发布卡**均已合入/执行；仍余 S4 族 waves 与 S2 离线发现器，故 CT-01 仍为「实施中」，不标「已实现」。
- **RT-01 已实现**：plan-20260715 完成判据与 Checkpoint A–D 全部满足（v0.21.19 正式关闭），TUI 已退场、`libra code` 默认 Web Code UI、runtime 为唯一状态机 owner；DF-05..08 经 plan-20260824 收口（v0.21.28/v0.21.29/v0.22.0 发布，SSE v1 物理移除 `a643dfb`）；不覆盖 DEFER-01..10（部分按重启条件承接）。
- **UP-01 已实现**（第十次审计登记）：代码（手动 `libra upgrade` `895589d`；fail-closed Ed25519 安装验签 `2ea10cc`；OIDC broker/publish `a0cb725`；generation floor `4bb5672`；trust root `fc9c203`）+ 测试（`upgrade_auto_test` 31 fn、`upgrade_publish_contract_test`、`install_smoke_test`）+ 文档（`docs/commands/upgrade.md`、`COMPATIBILITY.md:118`、`docs/error-codes.md LBR-UPGRADE-001`、`release-signing-auto-upgrade.md` D1–D10）+ 已发布 tags v0.22.1/2/6..10（D10 首签 v0.22.7）。文档债：CHANGELOG 缺 0.22.1..0.22.10 条目，不阻断但须补；残留 DEFER-02..06。

---

## 路线图维护

- 每次竞品审计：同步可安全更新的直接两层仓库；按 **版本管理 / Agent 生成代码 / Memory** 三类归表；dirty/失败按实际 revision 记录。
- **每次竞品更新后，先审计 revision 增量中的新功能、改进、Bug 与安全修复，再分析整体能力**；不得只读最新 commit，也不得只凭 commit message 下结论。
- **审计机切换（Linux 发行版 ↔ macOS）或路径迁移时，快照必须记录机器事实、路径与集合变动**。
- **仓库身份按规范化 remote 键匹配，目录名只作展示**。
- **每编号每轮新增完成判据 ≤2、竞品证据 ≤1；验证性修复（Libra 已有同类防护且有测试）只进汇报账本，不进本文**。
- 每季度或重大架构变更后重核代码与文档；不得复制上次「当前基础」文字代替复核。
- 编号不重编；废弃用「已替代/不采纳」。
- 新候选必须同时给出竞品 revision、Libra 缺口、价值、风险、依赖与最小切入点。
- 进入日期计划时只更新总览状态与链接；完成只以可发布代码+测试+文档为准。
- 日期计划推进（如 CT-01 的 `plan-20260729.md` 首个 wave 合入）时，须据当前 checkout 复核并把对应 LR/CT 状态从「已验证/已排期」推进为「实施中」，不得停留在旧状态；反向亦然：竞品变化只能改竞品证据、完成判据与候选，不得单独改变编号状态（例外见本文状态迁移规则）。
