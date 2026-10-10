# `libra agent`

管理 Claude Code、Codex 与 OpenCode 的外部代理捕获。

## 概要

```bash
libra agent status
libra agent list [--schema-version <1|2>] [--json]
libra agent import (--session <id> | --path <path> | --since <rfc3339> | --all) [--agent <name>] [--limit <n>] [--cursor <n>] --yes
libra --json agent graph <session> [--repo <path>]
libra agent enable [--agent <name>]...
libra agent add [<name>...]
libra agent disable [--agent <name>]...
libra agent remove [<name>...]
libra agent session <subcommand>
libra agent checkpoint <subcommand>
libra agent skill <subcommand>
libra agent clean [--all]
libra agent doctor [--repair]
libra agent workspace list [--limit <n>] [--cursor <token>] [--state <state>]...
libra agent workspace show <workspace-id>
libra agent push [--remote <name>] [--force-rewrite]
libra agent rpc <subcommand>
libra agent bridge --stdio
```

## 说明

`libra agent` 管理 Libra 的外部代理捕获表面。它安装和移除提供商 hook，报告已捕获的 session/checkpoint 状态，暴露只读诊断，并可将 `refs/libra/traces` 推送到远程。

`libra agent workspace list|show` 是 workspace 注册表（`workspace_record`）的
只读机器接口：agent runtime 关联过的每个 linked worktree、task worktree 或
remote workspace，含生命周期状态（`provisioning`/`active`/`releasing`/
`released`/`orphaned`）、owner、lease fence/到期与 canonical 路径。`list`
走 keyset 分页（按 `workspace_id` 升序；默认 `--limit 50`，上限 500；
`next_cursor` 原样回传），`--state` 可重复过滤；`show <workspace-id>` 返回
冻结的 schema v1 单条记录。lease 变更不在此暴露——它属于 agent runtime 的
内部服务。

支持的 roster 为 `claude-code`、`codex`、`opencode`（首批），三者均可安装 hook：`claude-code` 写 `.claude/settings.json`；`codex` 写用户级 `$CODEX_HOME/hooks.json` 并在 `$CODEX_HOME/config.toml` 写入 Libra 托管的 trust 条目（未受信的 Codex hook 会被静默跳过，trust 条目是安装的一部分）；`opencode` 写 Libra 托管插件 `.opencode/plugin/libra-hooks.js`（注意：`opencode --pure` 会禁用包括捕获在内的全部外部插件）。`gemini` 已从支持 roster 降级为仅卸载通道：`libra agent remove gemini` 可移除历史安装的 Libra 托管 hook（幂等），已捕获会话保持可读；对它或其它非 roster 代理执行 `add`/`enable` 会返回可操作的 unsupported 错误。

对 Codex，`enable` 与 `disable` 会在协作式 Libra 进程锁下更新耦合的
`$CODEX_HOME/hooks.json` 与 `config.toml`。它们会先准备两个文件，并拒绝在替换前可观察到的
编辑；但任意编辑器无法被可移植的原子“比较并替换”覆盖。命令运行期间请不要编辑这两个文件。
若报告非明文完整性恢复日志路径，说明继续自动覆盖已不安全；请保留该日志，先手动解决编辑冲突，
移除所报告的日志后再重试。日志只记录有限的事务元数据（schema 版本、操作、阶段和恢复围栏）
以及域隔离的文件指纹，不含原始 hook 或 config 内容；未知指纹状态会刻意保持零写入，而非自动修复。

## 子命令

| 子命令 | 说明 |
|------------|-------------|
| `status` | 报告已捕获的外部代理会话状态 |
| `list` | 列出受支持代理的能力矩阵（roster、hook、安装状态） |
| `import` | 经明确同意后，发现并导入历史 Claude/Codex transcript 文件，或导入一次受信、沙箱化的 OpenCode export |
| `graph <session>` | 只读检查 session → turn → revision → subagent 捕获图；需要全局 `--json`/`--machine`（交互式 TUI 入口已在 W5 breaking 发布中删除） |
| `enable` | 启用一个或多个外部代理并安装 hook |
| `add` | `enable` 的别名：`add <name>` ≡ `enable --agent <name>` |
| `disable` | 禁用一个或多个外部代理并卸载 hook |
| `remove` | `disable` 的别名：`remove <name>` ≡ `disable --agent <name>` |
| `session list` | 列出已捕获会话 |
| `session show <id>` | 显示一个已捕获会话 |
| `session stop <id>` | 将已捕获会话标记为 stopped |
| `session resume <id>` | 将已停止的已捕获会话重新标记为 active |
| `session derive-tool-calls <id>` | 从已捕获会话推导工具调用记录 |
| `checkpoint list` | 列出已捕获 checkpoint |
| `checkpoint show <id>` | 显示安全的 checkpoint 结构摘要（隐藏 metadata 和内部对象 ID） |
| `checkpoint rewind <id>` | 检查或应用某个 checkpoint 的工作树回退 |
| `checkpoint export <id>` | 导出 checkpoint transcript：默认脱敏（无需授权）；raw（未脱敏）导出须 `--allow-raw --raw` 并写入 append-only `agent_audit_log`（缺失授权时拒绝并返回 `LBR-AGENT-013`） |
| `skill search` | 按 `--skill`、`--provider`、`--session`、RFC3339 `--since`/`--until` 搜索捕获的 skill events（`--limit`/`--cursor` keyset 分页、`--json`）。基于 checkpoint metadata 的读时投影，无独立表 |
| `skill list` | `skill search` 的别名（同过滤项） |
| `skill registry` | 展示各 agent 的 curated 可发现 skill 注册表（`--provider <slug>` 限定；公开 SkillDiscoverer 面） |
| `clean` | 清理已停止会话的临时 checkpoint（prune 遇到进行中的 checkpoint 写入、traces 引用可达但无 catalog 行的提交、或仍有耐久 object-index repair 待处理时 fail-closed 拒绝；同时删除因此不可达的 `object_index` 行） |
| `doctor` | 诊断 hook 安装和捕获状态；检测（`--repair` 时修复）checkpoint 存储不一致。对象检查刻意只读取本仓库的本地对象目录：不可用的 alternate/remote 对象会保守地报告为不可用，而不会在产生诊断时解析外部可控的 alternate 路径。只读的 `legacy_code_residue` 字段（人读为「Frozen Code residue」行）报告冷冻的 Code 时代路径 `.libra/sessions/code/`、`.libra/code/` 与 `libra/intent` ref 是否仍存在；它不会删除或改写该状态，清理另由 plan-20260920 ADR-RC-04 / DEFER-RC-02 承接 |
| `push` | 将 `refs/libra/traces` 推送到远程（`clean` prune 重写后的非快进推送用 `--force-rewrite`，采用 force-with-lease 语义） |
| `rpc list` | 列出 `PATH` 上发现的 `libra-agent-*` 二进制（含 trusted/quarantined 状态）；需先开启 external-agents 开关 |
| `rpc trust <slug>` | 信任一个已发现的二进制——记录 path + sha256 + device/inode/mtime 来源（所在目录 world-writable、或二进制不在受信目录下时拒绝——`LBR-AGENT-005`）。provider-exporter slug `opencode` 则改为固定 provider 自身的 CLI 二进制——只从已注册受信目录解析、绝不扫描 `$PATH`——供沙箱化 export bridge 使用；该形式无需 external-agents opt-in。来源 sha256 以单次流式读取计算、不设固定大小上限，因此大型单文件 provider CLI（OpenCode Bun 构建约 171 MiB）仍可受信；计算期间读出字节超过其报告大小的文件会被拒绝（`LBR-AGENT-005`） |
| `rpc trust --dir <path>` | 注册一个受信目录（`agent.external_agents.trusted_dirs`，默认 `~/.libra/agents`）：外部二进制的 canonical path 必须位于其中之一才可被信任。路径会被 canonicalize，且必须是存在且非 world-writable 的目录 |
| `rpc untrust <slug>` | 撤销信任；二进制回到隔离状态（始终可用，不受开关限制） |
| `rpc invoke` | 在**已信任**的 `libra-agent-*` 二进制上调用一个 JSON-RPC 方法 |
| `bridge --stdio` | 在 stdin/stdout 上运行仓库级 DeepSeek Harness 桥接（JSON-RPC 2.0 NDJSON）。这是 Harness 的**唯一**标准入站写入传输；它不是 `libra code --control`。stdout 每条只承载一个协议帧；诊断只写 stderr。协议 v1：20 个 method 的 allowlist、256 KiB 帧上限、64 个 in-flight 请求、默认 30 秒 deadline。20 个 method 全部已实现：`initialize` 握手；session/event ingress `session.open`、`event.append`（batch ack / 幂等 / digest conflict / 服务端脱敏）、`session.flush`、`session.close`、`evidence.append`、`provenance.append`；只读方法 `context.get`、`status.get`、`history.search`、`checkpoint.list`、`checkpoint.show` 与 `diff.get`；mutation `checkpoint.create`、`commit.create`、`checkpoint.restore` 与 `review.run`（均带 `operation_id` 幂等、actor binding 与 approval 门禁）；以及 workspace lease `workspace.claim` / `workspace.renew` / `workspace.release`（owner 从已认证的 bridge session 派生）。`diff.get` 只接受封闭的 `mode`（`worktree` / `staged` / `checkpoint`）加上经校验的仓库相对 `paths`——绝不接受自由形式的 revision 或 pathspec——并强制 `--no-ext-diff` / `--no-textconv`，使仓库配置无法转化为进程执行。`commit.create` 只提交当前 index（无 `-a`、无 pathspec、无 amend、无 author 覆盖），关联图写入 `agent_bridge_link` 而不是拼进 commit message。`checkpoint.restore` 必须携带显式 `expected_head` fence 且要求 index/worktree 干净，并且绝不移动 HEAD。`review.run` 启动只读 review 并返回 `run_id`；用同一个 `operation_id` 重放会返回该 run 的当前状态，而不会启动第二个 run。HEAD 漂移或 worktree 脏在任何写入之前以 `LBR-AGENT-038` 拒绝。ack 只表示脱敏 projection 已 durable，不代表 Harness 原始 transcript 已迁移到 Libra。不是 Git 命令。 |

## 常用选项

| 标志 | 子命令 | 说明 |
|------|------------|-------------|
| `--agent <name>` | `enable`, `disable` | 选择代理名称；省略时针对支持 roster（`add`/`remove` 以位置参数接收名称） |
| `--schema-version <1\|2>` | `list` | 选择 machine schema；版本 1 保持冻结旧行结构，版本 2 增加 `transcript_discoverable`、`importable`、`export_bridge` 的 `methods[]` 可用性 |
| `--session <id>` / `--path <path>` / `--since <rfc3339>` / `--all` | `import` | 四选一的历史导入范围；`--path` 还必须指定 `--agent`，OpenCode 通过 export bridge 支持显式 `--session` |
| `--yes` | `import` | JSON/非 TTY 导入必需；确认 Libra 可以读取私有 provider session 内容、脱敏并把类型化投影写入当前仓库 |
| `--restore-erased` | `import` | 显式移除本地防复活 tombstone 后重试；要求 `--yes`，并追加审计记录 |
| `--limit <n>` / `--cursor <n>` | `import` | 有界发现分页（默认 20、硬上限 100）以及上一页返回的下一个零基游标；单次调用还有 64 MiB 累计原始输入上限。每源有效 cap 为 `min(agent.max_transcript_read_bytes, 16 MiB adapter hard cap)`；显式配置更大值时会诊断实际有效 cap |
| `--repo <path>` | `graph` | 从另一个 Libra 仓库读取捕获元数据，而不是从当前目录发现仓库 |
| `--limit <n>` | `session list`, `checkpoint list` | 每页最大行数（默认 50，硬上限 500——超过时钳制并在 stderr 提示；`0` 按 `1` 处理） |
| `--cursor <cursor>` | `session list`, `checkpoint list` | 上一页 `next_cursor` 返回的不透明 keyset 游标；不要手工构造 |
| `--extract-transcript <path>` | `session show` | 从当前已验证的 Claude Code transcript 来源重新派生并最多复制 16 MiB 到一个新的本地文件；绝不回退使用已捕获的 metadata 路径，也绝不覆盖已有路径，超限时不发布输出，代理不受支持或当前来源不可用时会失败。使用 `--json` 时，结果为安全 envelope：`{ "session": <session row>, "extracted_transcript": { "output_path": <path>, "bytes": <count> } }`；提取块只包含目标路径和字节数。JSON 和人类输出都不暴露 provider 来源路径。 |
| `--allow-raw` / `--raw` | `checkpoint export` | 授权并请求 raw（未脱敏）导出；缺少 `--allow-raw` 时 `--raw` 请求会被拒绝（`LBR-AGENT-013`）并记入审计 |
| `--justification <text>` / `-o <path>` | `checkpoint export` | raw 导出的审计理由与输出文件 |
| `--all` | `clean` | 清理所有已停止会话的 checkpoint，而不只是最近一个 |
| `--gc` / `--retention-days <n>` / `--dry-run` | `clean` | 三窗口保留期 GC：(1) 删除已停止会话中早于 `agent.retention.transcript_days`（默认 90；用 `--retention-days` 覆盖）的 checkpoint；(2) 清理早于 `agent.retention.stderr_days`（默认 30）的**终态** run 的 reviewer stderr 诊断日志，保留聚合记录；(3) **A0-09** 删除早于 `agent.retention.findings_days`（默认 90）的**终态** review/investigate run 整个目录（`findings.md`/`manifest.json`/`state.json`/reviewer 日志）。对象化的 findings blob 是内容寻址对象，由仓库级 `libra maintenance run --task gc` 可达性回收（PD-04）：孤儿 findings blob 连同其 `object_index` 行一起回收，live-run manifest 引用的 OID 保持存活（per-run retention 绝不删除可能被共享的对象）。non-terminal/时间戳不可解析的 run 一律 fail-safe 跳过；永不触碰 `agent_audit_log`。`--dry-run` 仅预览各窗口及配套清理的 would-be 删除（包括 JSON `findings_runs_pruned`/`import_identities_pruned`），不实际删除 |
| `--repair` | `doctor` | 修复检测到的 checkpoint 存储不一致（重建 catalog 行、补插 `object_index` 行、安全 drain 有效的过期普通 writer marker，并忽略普通 TTL 立即 drain `cleanup_pending` marker）；损坏 marker 保持 `manual_required`；省略时仅检测 |
| `--remote <name>` | `push` | 选择用于推送代理 trace 引用的远程 |
| `--force-rewrite` | `push` | 允许本地 `clean` prune 之后的非快进推送（traces 引用由 Libra 托管，prune 即整链重写）；采用针对本仓库最近一次推送记录的 force-with-lease 语义——绝非无条件 force——远程被别处重写时仍 fail-closed 拒绝 |
| `--dry-run` | `checkpoint rewind` | 显示影响而不修改文件；这是默认值 |
| `--apply` | `checkpoint rewind` | 恢复所选 checkpoint 的工作树 |

## 范围化 checkpoint 的暂停恢复

`review --checkpoint <id>` 与 `investigate start --checkpoint <id>` 仍把普通 checkpoint 文件以只读形式物化到 `<run_dir>/checkpoint-input/`。`investigate continue` 只有在显式仓库 catalog 的普通叶子具有相同路径和 blob id 时，才会再次写入已保存的 spec。路径 `reasoning/encrypted/<64 hex>` 不是普通输入：它不会被写入；已保存 spec 若点名该路径，会在清掉上一次输入目录之前拒绝。已保存 id 若不是 blob，或字节与该 id 的内容哈希不一致，也会在清掉该目录之前拒绝。catalog 无法打开、查询或关闭时，暂停中的 continue 返回既有存储错误，且不改变该 run 的 state 与已有输入字节。review 的设置预算为零时，按既有基础设施错误结束，且不启动 reviewer；取消则按既有 cancelled 结束。不新增稳定错误码。普通 payload 仍是单文件 64 MiB、合计 256 MiB。每次 catalog 树读取单独限制为 16 MiB，不并入上述 payload 上限。

## JSON 输出

支持结构化输出的子命令使用全局 `--json` 和 `--machine` 信封。例如：

```bash
libra --json agent status
libra --json agent list
libra --json agent graph <session>
libra --json agent checkpoint list
libra --json agent rpc list
```

`agent list --json` 携带稳定的 `schema_version`，并为每个受支持代理输出一行——首批 roster `claude-code`、`codex`、`opencode`。非首批代理（`gemini`、`cursor`、`copilot`、`factory-ai`）仍保留在注册表中以保证历史会话可读，但不会出现在该列表里。每行携带 `slug`、`agent_kind`、`stability`、`supported`、`support_wave`、`registered`、`transcript_readable`、`hook_installable`、`installed`、`launchable_review`、`launchable_investigate`、`external_binary`、`config_paths`、`protected_dirs`、`capabilities`。行结构是面向自动化的冻结契约。Claude Code 会声明 `capabilities.transcript_preparer=true`：Libra 先安全打开并 pin 已授权 descriptor，再只通过同一 descriptor 短暂等待末尾 JSONL 记录完成 flush；等待与 tail probe 均有界，preparer 不会按路径重新打开文件。

只有调用方理解扩展时才请求 `agent list --schema-version 2 --json`。其
`methods[]` 报告 transcript 发现、历史导入与 OpenCode export bridge 的
支持及当前可用状态；默认版本 1 的 payload 保持原有字段集合，不会隐式
增加扩展字段。OpenCode 的 `transcript_discoverable` 明确为 unsupported（不支持
批量发现）；显式 ID 的 `importable`/`export_bridge` 可用性取决于受信任离线
exporter 与 sandbox。

在 Unix 上，exporter 子程序及其后代的 `RLIMIT_CORE` soft/hard limit 均设为零。
当系统 core handler 尊重该限制时，预期的限制执法与意外 exporter 崩溃都不会
保存 core 文件；系统日志仍可能记录信号事件。Libra 保留退出状态及有界 stderr
诊断。该设置只作用于子程序，不修改 Libra 父程序限制或系统配置。

macOS 不支持 OpenCode 内容导出：Seatbelt 无法为可 fork 的 exporter 提供可随 hook 取消安全收束的后代进程隔离，因此 Libra 会在启动 `sandbox-exec` 或 exporter **之前** fail-closed，hook 捕获保持 metadata-only。Linux 使用 Required bubblewrap containment。启用 Linux exporter 的方式：先注册包含已核验 `opencode`
二进制的目录，再固定它——`libra agent rpc trust --dir <path>`，然后
`libra agent rpc trust opencode`。两步都不会打开 external-RPC 表面
（`agent.external_agents.enabled` 保持不变）；二进制只从已注册受信目录解析，
绝不扫描 `$PATH`。Linux 内容导出还要求受信的 `bwrap` 通过 Libra 的 descriptor-native
`--bind-fd` 安全探测；不支持或过旧的 `bwrap` 会报告 unavailable 并 fail-closed 为
metadata-only 捕获。应安装或升级系统 `bwrap` 包，绝不启用未沙箱化回退。不支持的 schema version 以 usage 错误（exit 129、category
`cli`）返回 `LBR-AGENT-017`。当平台无法提供 Libra 的 provider-root 安全打开
原语时，Claude/Codex 的发现与导入会如实报告为 unavailable。

`agent graph --json` 输出冻结的 capture-graph schema version 1。其 `data` 对象只含
`schema_version`、`state`、`session`、`turns`、`subagents`。存在的 session
只暴露 session id、agent kind、状态和时间；indexed turn 暴露逻辑键、派生的
零基 ordinal、coverage schema/completeness/current revision、checkpoint id、
source channel 与 append-only revision 历史。同一个 whole-transcript checkpoint
可同时出现在多个 turn 下，绝不会因其中一个 turn 升级而被隐藏为 superseded。
M1 前的 capture 以 `coverage_state="unindexed"` checkpoint 时间线返回，不伪造
revision。subagent 节点只暴露 checkpoint/link 结构，并明确保留 `resolved` 或
`unresolved`。

JSON/machine graph 查询不会打开 transcript/object blob，也不 SELECT working directory、
description、metadata JSON、redaction report 或 coverage digest。**Breaking change（W5-08）：**
交互式 capture-graph TUI 已在 W5 breaking 发布中删除——其受限、已脱敏的内容预览
（事件计数与紧凑的 user/assistant 消息预览，从关联 checkpoint 读取、每个最多 256 KiB）
随之移除；上述冻结 JSON v1 schema 从未携带这些字段。
本地已擦除 session 成功返回 `state="erased"`、null session、空 turns 与 unavailable
subagents，且不会重建；session 与 tombstone 均不存在时返回 `LBR-AGENT-021`。未指定全局
`--json` 或 `--machine` 时，`libra agent graph` 在读取任何捕获状态之前以 usage error
加迁移提示退出（exit 129，`LBR-CLI-002`）。

`agent import` 返回 schema version 1 的 `results`、`skipped`、`partial_results`、
`failures` 与 `next_cursor`。每项状态固定为 `imported`、`noop`、`partial`、
`skipped` 或 `failed`。自动发现的跨仓库/已擦除候选进入 `skipped`，session id
哈希脱敏并携带稳定 reason code；显式 selector 遇到同类条件仍是失败。
`results` 仅含完整成功项；发生耐久 turn 进度但最终
失败的 selection 进入 `partial_results`，不会计入 `succeeded`。批次部分成功时以 `LBR-AGENT-018` 非零退出；结构化错误详情
保留成功摘要及逐项失败，失败 session id 使用哈希脱敏。单项归属、cwd、
已擦除与源授权失败分别保留 `LBR-AGENT-015`、`016`、`019`、`020`；provider 时间戳（含推导的 turn 时间）若超出导入主机时钟五分钟以上，会在影响 session 时序前被拒绝：该 selection 以固定消息 `the transcript contains timestamps beyond the permitted clock skew`（`LBR-AGENT-018`）失败且不写入任何耐久状态；失败详情
仍携带 `schema_version`，并原样保留 nullable `next_cursor`，不会把 `null` 改成 0。
成功与 partial 摘要分别用 `checkpoints_written` 报告父 turn 写入、用
`subagent_checkpoints_written` 报告独立发现的 child-content 写入；只有 child
写入时状态仍为 `imported`，不是 `noop`。平台无法提供安全 child discovery 的
诊断本身不会让完整的父 transcript import 失败，因为它不能证明 child 内容存在。

历史导入严格限定当前仓库且 fail-closed：transcript 必须给出唯一无歧义的
`cwd`，Libra 解析其 storage 并要求与当前仓库相同。共享该 canonical storage
的 sibling linked worktree 属于同一仓库；其它仓库不属于。文件源必须位于所选
provider 的 protected root；Unix 使用 descriptor-relative no-follow 单次
打开。provider root 按 component 逐段安全打开；Claude 每个 source 与 Codex 的
year/month/day 各层都在同意前相对 pinned 目录 descriptor 打开，因此 root、嵌套
目录或 source-file symlink 均不能逃逸 provider root；没有等价安全打开的
平台直接拒绝。持久化内容仅含经过字段级脱敏的
coverage-v1 user/assistant/tool 类型投影；原始 provider envelope、provider-home
源路径和未知字段不会落盘；已验证仓库 `working_dir` 是文档化兼容例外。重放
幂等；incomplete turn 可前推为唯一 complete
revision，但不会改变 checkpoint 的结构仓库父节点。若之后有不同的 complete
payload 声明同一逻辑 turn，claim 会停在 `conflicted`，并在
`agent_coverage_conflict` 中只保留**第一个** challenger：typed canonical payload
先脱敏，再连同 digest、source channel、observed time 与确定性 redaction report
持久化；后续 challenger 不覆盖首份证据，raw provider envelope 与命中的密钥原文
永不落盘。operator 显式解决前，incumbent revision 保持 append-only 且仍为 current。
本地 session erase 会先
写耐久防复活 tombstone，再删除 catalog；自动发现和在途 writer 都不能重建。
唯一的本地绕过是经审计的 `--restore-erased --yes`。

新的 V2 导入只把来源保留为 repository-keyed、domain-separated 的
`source/hmac-v2/<64 lower-hex>` commitment。新的 V2 subagent content 则使用其独立
domain 的 `source/subagent-hmac-v2/<64-lower-hex>` commitment。raw source locator
或未加钥的 source SHA-256 不得进入 import/subagent 的 catalog metadata、claim、marker、
diagnostic 或 cloud record。可选的 V2 snapshot digest 是对脱敏 snapshot content
使用独立 domain 的 repository-keyed `source/hmac-v2/<64 lower-hex>` commitment；
helper 的瞬态 SHA-256 绝不持久化。bare 或带标签的未加钥 SHA-256 只可作为不可变
legacy evidence，不能写入新的 V2 或 cloud state。V1 行只可读作 proof，不能当作转换捷径：只有 exact
scoped proof 匹配且 identity、catalog 与可迁移 repair marker 都已 committed、
quiescent 时，Libra 才在同一原子事务中迁移它们。active、partial 或
repair-pending 的 V1 state 保持 V1，绝不创建并行 V2 record。

首次读取内容或执行 export 前，交互确认会显示 agent 范围、仅当前仓库边界、
候选数/上限、脱敏写入，以及后续 `libra agent push` 可能上传脱敏 traces 的
提示。`--yes` 只确认该隐私影响，不放宽 provider root、仓库、大小、deadline
或平台授权。跨 session 批次 best-effort；后续 turn 失败时仍精确报告该 session
已经耐久提交的进度。64 MiB 批次预算按 held descriptor 实际读取的字节计费；
成功、格式错误、未授权及超限候选均计费，也包括授权后文件继续增长的部分。
如果有界 child discovery 在返回可信字节数前失败，Libra 会保守地计入该 child
剩余的全部每源 allowance；重试不能利用 helper 失败绕过批次 cap。
120 秒绝对 deadline 在 discovery 前开始，并在 helper 阶段、遍历、解析、分批
reservation、对象构造与 CAS 前后检查。SQLite commit 一旦开始就不会被超时取消：Libra 在每次
commit 前立即检查 deadline，再等待该 commit 得到确定结果；因此最终 turn 已完整
提交时，即使等待结果期间越过 deadline 也仍报告成功。deadline 失败会 abandon
当前 owner 的全部未提交 lease/marker；若该恢复事务自身失败，命令会连同原错误
报告 cleanup 失败和可执行的 `agent doctor --repair` 提示，而不是静默吞掉。
discovery 和已授权 fd 读取 helper 都是可于超时杀死的私有进程。读取 helper 消费
父进程已经打开的同一 descriptor；其 control wire 只有固定的安全上限和 commitment，
绝不包含 locator、路径、provider session 值或 raw bytes。这些 helper 阶段共享该绝对
deadline。当前命令进程仍会在交接前同步执行 source 分类、canonicalize、安全打开和
rewind；此处 NFS/FUSE stall 无法被强制取消，因此 120 秒设置不是严格的端到端
wall-clock deadline。child discovery 使用较短的子 deadline，为独立有效的父
checkpoint 预留提交时间；父进程在遍历有界响应时也会持续复核该子 deadline。因此，
迟到或过大的 child 结果只会令父结果降级为 partial evidence，不会吞掉父 checkpoint。
consent 之后，持有 descriptor 的 preparation 阶段在可杀 helper 中完成 parse 和
redaction，且不能按 locator 重新打开 source。source-root 授权和初始安全打开在该
交接之前发生。checkpoint loose-object 写入以及 traces ref 拼接所需的 commit/tree
读取保留各自的有界路径。此设计保留 descriptor-bound raw read，却尚不能在单个
command/hook 进程中同时提供 strict deadline、FD-only wire 和 timeout 后的
autonomous replay；该组合需要 long-lived owner 或由 provider ABI 提供
pre-authorized descriptor，见 [Session Capture 计划](../../development/plan/plan-20260924.md)。
对象读取只接受请求的 `commit`/`tree` 类型，校验完整 OID 与声明长度，并在分配前执行
16 MiB 解压 payload 上限；恶意压缩对象不能把有界 helper 工作转化为无界内存占用。

批量 import 在进入下一个候选前，会等待当前已完成候选排队的 object-index 写入
落入 SQLite。若 drain 耗尽命令 deadline，partial 进度归属于当前候选，绝不错误
归到尚未开始的下一个候选。后台更新的终态错误同样是 barrier failure，不能只记
日志后假装成功。import 在 checkpoint 写入前取得 session 级耐久 repair barrier；
其中的 owner、generation 与 lease 会串行化并发进程，只有精确 owner/generation
可以退役它，索引失败也只能把产生该结果的精确 identity/fence 降为 partial。
超时或更新失败会令 barrier 进入 repair-pending，并把精确 identity 标为 partial；
进程崩溃则保留 active generation，lease 过期后的 takeover 必须先修复再写入。
replay 在允许返回 `noop` 前，会在可杀 helper 中幂等前台修复该 session 的完整 E4 object 集；helper
持有 SQLite writer slot，并在同一事务复核 session/erase tombstone、更新索引及保留
所属 barrier，因此并发 erase 后不会重新插入可上传行。成功候选只退役自己的
generation；若有界修复无法完成，运行 `libra agent doctor --repair` 后重试。

每个可能新建的 loose object 在写入前，都会先把 OID 作为 provisional preclaim
持久化到 attempt marker；只有本 writer 赢得不覆盖发布，并把 OID 耐久迁入
`created_oids` 后，才获得可删除的所有权。发布后、确认前崩溃会选择安全泄漏：
未确认 preclaim 永远不会被自动删除。
对象先压缩到共享私有目录 `objects/info/libra-tmp` 中的唯一临时文件，再以
不覆盖方式发布；若 final path 已存在，必须解压并逐字节验证后才可复用。后续写入
每次最多检查该私有目录 64 项，只清理超过 24 小时且名称精确匹配
`.<40-or-64-lowercase-hex-oid>.tmp-<decimal-pid>-<uuid>` 的普通文件；其它文件和目录保留。
只有启用 `--sync-data` 或 `LIBRA_SYNC_DATA` 时执行文件/目录 fsync；不覆盖原子发布
始终启用。在删除不可达 `object_index` 行前，`agent clean` 会取得仓库级 repair-marker
generation fence、重新核验每个候选 OID，并一直持有到 prune 事务提交。这样，即使 marker
在较早的命令 preflight 之后发布，cleanup 也会 fail-closed，而不会让其迟到队列更新复活
已删除行。启用 `--sync-data` 时，repair marker 退役还会 fsync 所在目录。正常 append、
失败 finalizer 与 erase 都不执行全仓库 reachability drain；
被拒绝 append 会把精确 generation 标为耐久 `cleanup_pending`，由
`agent doctor --repair` 忽略普通 writer TTL，立即执行有界、root-fenced 的 ownership
退役。同 session cleanup job 未退役时 erase 会拒绝；erase 本身
不运行该 drain。inline recovery 绝不 unlink 共享
loose object，也不删除其 `object_index` 行：worktree index writer 不共享 SQLite lock，
所以对象可达性证明与物理回收交给具备 grace/locking 策略的仓库 GC。doctor 覆盖
refs、reflog、全部已注册 worktree index、进行中的 sequencer 状态以及 marker/catalog
状态：先在 SQLite 写事务外快照，再在最终 ownership 退役事务内复核完整快照。由于
该路径不删除 payload 或 `object_index` 行，它不会遍历无关对象历史。每次 marker 注册还带
随机 writer `generation`；对象 preclaim、ownership 确认、最终 ref CAS 与 marker
清理都必须精确比较该 generation，过期 writer 不能接管或删除同 checkpoint 的
takeover marker。ref/reflog roots 最多收集 250,000 行；注册 index 快照位于 30 秒 deadline
控制的可杀 helper 中，以 no-follow/nonblocking 打开并要求普通文件，从持有 descriptor
执行一次 `limit + 1` 增长检查，再对同一批精确字节校验 checksum/解析。index 最多 256 个且合计
最多读取 64 MiB。任一上限触发时均 fail-closed 延后，保留 durable ownership
供诊断和后续重试；成功 drain 只退役 attempt ownership，孤儿 payload 留给仓库 GC。
lease takeover 后，零进度 provisional session 按其
持久化 `import_provisional` 标记清理；live/export 在 marker 注册后的失败会释放
claim/job lease，并且只清普通 marker，绝不清除 `cleanup_pending` job。

`agent clean --gc` 会物理删除最终 coverage 已被清除的终态、无 owner import identity，
也覆盖零 checkpoint identity；不会把它们重置为可再次 replay 的 `discovered`。
dry-run 在回滚事务中模拟 coverage 清理，因此 `import_identities_pruned` 与相同真实
GC 运行一致，同时不修改 catalog。
冲突证据跟随 claim 生命周期，不形成独立 retention root：erase session 或 prune
最终 coverage claim 会级联删除首个 challenger，dry-run 模拟同一删除；checkpoint
history rebuild/prune 将 current claim 回退到更早的存活 revision 时，会删除已经
失效的 challenger 证据，并把 claim 恢复为非冲突的 committed 状态。

tombstone 会传播。`libra cloud sync` 在 generation fence 下把它发布到 D1，
并删除该 session 在远端 catalog 的镜像行；`libra cloud restore` 则是**双向
tombstone 优先**——在 restore 事务内同时过滤带远端 tombstone 的行与匹配本仓库
`agent_import_tombstone` 的行。因此本机 erase 之后、再从尚未见过该 tombstone
的镜像 restore，也不会把 session 复活。

**仍 deferred 的只有 R2 物理删除**：已擦除内容的 payload 仍留在 R2，未见过该
tombstone 的另一台机器仍可取回。所以 `agent erase` 应视为对**本仓库**不可逆，
而不是「所有副本都已消失」的跨机保证。

私有 capture recovery 证据只保存在本仓库。local erase 先提交防复活 tombstone，
再 prune checkpoint history，最后在同一 catalog 删除事务清除可归属的 recovery
headers/chunks 和 session aliases。删除依据本地 catalog PK/incarnation 归属，
不依赖 recovery MAC 或可被淘汰的 receipt；缺 key 不阻止可归属清理。foreign 或
无法归属的损坏证据保留，提交后只记录不含内容的 lost-capacity 诊断；它仍占用容量，
并可能阻止创建新关联。没有一致性的原 repository DB 备份时，这部分容量明确 lost，
doctor/retry 不提供 force-discard。整份 DB-copy 备份含敏感 alias 关联，不是匿名化
备份；恢复旧完整 DB 备份可能撤销删除及 tombstone。正常 receipt Complete 则在同一
事务回收对应 artifact，只有无剩余引用时才删除 alias。

pending 与 quarantine header 若冲突于同一 checkpoint key，清理只删除已确认
属于本 session 的 header；未知/外来 header 及共享 chunks 原样保留。explicit
erase 会报告保留容量，不证明每一份私有副本都已删除。

session incarnation 不可读，或同 PK 存在其他 incarnation 的已知 alias 时，最终
删除会拒绝：恢复一致的 session/alias metadata 后重试；第一阶段 tombstone 仍生效。
只要保留一份不可解码 header，就会阻断本仓库所有新建/重投 recovery artifact 及
destructive GC/root-walking maintenance，而不只是损失其自己的槽位。该 header
阻止证明无引用时，Complete 也会保留 alias；可归属的 explicit erase 仍可执行。
out-of-band 超容量状态最多读 17 份 header 和 17 份关联；未检查部分可能仍含本
session 的内容。警告不是「所有私有副本已删除」的证明。

`agent session list --json` 与 `agent checkpoint list --json` 每次返回一页：`data` 携带 `schema_version`、位于 `sessions` / `checkpoints` 下的行（单行结构不变），以及 `next_cursor`——传回 `--cursor` 的不透明游标，列表耗尽时为 `null`。页序为最新在前（`started_at` / `created_at` 降序，行 id 作为并列时的次序键）。

人类可读的 `agent session list` 表格会把 `started_at` 按当前机器时钟显示为相对时间（例如 `2 hours ago`）；JSON 输出仍保留原始 Unix 时间戳，供自动化使用。

`agent checkpoint show <id>` 刻意不是 metadata dump。默认的人类与 JSON 输出只含固定的 checkpoint 摘要：`checkpoint_id`、受限词表中的 `scope`、Unix `created_at`，以及是否记录了 parent snapshot。它绝不读取或渲染 `metadata.json`、session 标识、source locator 或 commitment、redaction 细节或 catalog 对象 ID。transcript 内容只能走显式的 checkpoint export 路径，并由该路径执行自己的授权和脱敏策略；不要把默认 `show` 输出当作内部 metadata 接口。

每个 checkpoint 行携带 `scope`。`committed` checkpoint 在 turn/session 边界（`Stop` / `SessionEnd`）写入，携带脱敏的 transcript 快照。`subagent` 下有两类刻意分离的证据：Codex 的 `SubagentStart` / `SubagentEnd` hook 写空 transcript 的 **boundary checkpoint**，并以 `parent_checkpoint_id` 表达结构归属；Claude `<session>/subagents/*.jsonl` 则逐文件写独立的 **content checkpoint**，其 `parent_checkpoint_id` 保持 null。新的 content claim 只会在安全打开 source 并重新核验 root、storage 绑定、scope 与 workspace fence 后，写入仓库密钥、域隔离的 HMAC V2 commitment（`source/subagent-hmac-v2/<64-lower-hex>`）；append-only revision 据此选出唯一 current content leaf，本地 project slug/文件名不落库，物理历史也不改写。历史 V1 `source/sha256/...` claim、revision 与 checkpoint metadata 是不可变、只读证据；命中同一 legacy claim 时不会静默创建并行 V2 行，必须继续走 recovery/`agent doctor` 路径。Claude 当前不提供能与 boundary 对齐的稳定 ID，因此常态是 `link_state=unresolved`；`agent doctor` 会报告但不会猜测。只有 provider 给出稳定 ID 且唯一匹配时，才在独立 association 行中关联 boundary，不修改不可变 traces commit。两类证据均可 list/show/export/prune，且 doctor 可见。

Codex 捕获会安装当前文档定义的全部生命周期事件：`SessionStart`、`SessionEnd`、`UserPromptSubmit`、`PreToolUse`、`PermissionRequest`、`PostToolUse`、`PreCompact`、`PostCompact`、`Stop`、`SubagentStart` 与 `SubagentStop`。checkpoint 的事件投影会保留 `turn_id`、`tool_use_id`、`agent_id`、`agent_type`、压缩触发原因、权限模式、工具输入和工具输出等关联字段。Codex 的 `encrypted_content` / 加密 reasoning payload 会被刻意从 capture projection 丢弃：Hook 没有解密密钥，Libra 在该路径既不保留加密占位，也不保留明文内部 reasoning。

持久化的 checkpoint 元数据只保留 Libra 能安全分类的字段。live hook capture 在任何持久化之前就会丢弃 provider 提供的 `model`、`source`、`tool_name` 与 session-reference（transcript 指针）字段，因此读取 `checkpoint export` 输出或 `refs/libra/traces` sidecar 的消费者不应再期待这些字段：在 hook 捕获的 checkpoint 中，`metadata.json` 始终记录 `"model": "unknown"`，其 `events/lifecycle.jsonl` 行从不包含 `model` 或 `tool_name` 且记录 `"source": null`，Codex subagent 边界元数据中的 `subagent.tool` / `subagent.source` 为 null。hook 捕获的 lifecycle 行的 `provenance.hook_event_name` 是 canonical snake_case lifecycle kind（例如 `tool_use` 或 `turn_end`，与 `kind` 一致），而不是 provider 的原生事件名。`events/lifecycle.jsonl` 为 schema v2：每行都带必填的 `identity_scheme`（`native_replay_hmac_v2`、`fallback_action_hmac_v1`、`generic_lifecycle_uuid_v5` 或 `import_uuid_v5`），声明其 `event_id` 的派生方式；它只是声明，而不是可验证的 replay 凭据。

云端镜像在 token-fenced generation 下按依赖顺序发布 catalog 批次（`session → checkpoint → revision → link → claim`）。session、checkpoint、link 与可变 claim 投影各自使用独立单调 sync generation，因此 prune 回退 current 子节点时不会制造同代冲突，retained traces 链的重写也不会被旧 clone 写回。显式 `--restore-erased` import 会为 session 与 opaque child-source namespace 启动新的耐久复制 incarnation；仅 R2 payload 物理删除仍 deferred 时也不会复用旧 key；D1 tombstone 传播已生效。generation 同时绑定远端 object index 的 canonical digest；sync 会在 D1 与 R2 复验每个 checkpoint 对象，restore 只有在读取前后 manifest 与 object-index digest 均未变化时才接受 catalog。当前客户端只使用版本化的 v2 远端 session/checkpoint 表；v2 激活后，D1 trigger 会以升级提示拒绝旧客户端的无 fence 写入，避免其破坏一致恢复快照。

## 示例

```bash
# 显示已捕获会话数量和最近 checkpoint 摘要
libra agent status

# 显示代理能力矩阵（支持 roster、hook、安装状态）
libra agent list

# 协商带 import/export 方法的版本化能力矩阵
libra agent list --schema-version 2 --json

# 明确同意后导入一个历史 Claude Code session
libra agent import --session <provider-session-id> --agent claude-code --yes

# 导入某时间之后修改的一页 Codex rollout
libra agent import --since 2026-07-01T00:00:00Z --agent codex --limit 20 --yes --json

# 以冻结 JSON v1 schema 读取一个捕获 session 的 turn/revision/subagent 结构
libra --json agent graph <session-id>

# 在自动化中或从另一个仓库安全读取同一图
libra --json agent graph <session-id> --repo /path/to/repo

# 启用 Claude Code 捕获并安装它的 hook（enable 的别名）
libra agent add claude-code

# 启用 Claude Code 捕获并安装它的 hook
libra agent enable --agent claude

# 一次启用所有支持的代理
libra agent enable

# 禁用 Claude Code 捕获并卸载它的 hook（disable 的别名）
libra agent remove claude-code

# 移除历史 gemini hook（仅卸载通道；幂等）
libra agent remove gemini

# 禁用 Claude Code 捕获并卸载它的 hook
libra agent disable --agent claude

# 列出已捕获会话
libra agent session list

# 显示一个会话并复制其当前已验证的 Claude Code transcript 来源
libra agent session show <session-id> --extract-transcript /tmp/session.jsonl

# 停止一个已捕获会话
libra agent session stop <session-id>

# 继续一个已停止的已捕获会话
libra agent session resume <session-id>

# 列出已捕获 checkpoint
libra agent checkpoint list

# 分页浏览 checkpoint（默认每页 50；JSON 携带 next_cursor）
libra agent checkpoint list --limit 100
libra agent checkpoint list --cursor <next_cursor>

# 显示 checkpoint 的安全结构摘要
libra agent checkpoint show <id>

# 将 checkpoint 回放为 JSONL transcript
libra agent checkpoint rewind <id>

# 从最近停止的会话中丢弃临时 checkpoint
libra agent clean

# 从每个已停止会话中丢弃临时 checkpoint
libra agent clean --all

# 诊断 hook 安装和捕获状态
libra agent doctor

# 将 refs/libra/traces 推送到默认远程
libra agent push

# 将 refs/libra/traces 推送到具名远程
libra agent push --remote origin

# `libra agent clean` 重写 traces 链后重新推送（force-with-lease）
libra agent push --force-rewrite

# 发现 PATH 上的 libra-agent-<name> RPC 二进制文件
libra agent rpc list

# 在 libra-agent-<slug> 二进制文件上调用单个 JSON-RPC 方法
libra agent rpc invoke <slug> <method> --params '<json>'

# 在 stdio 上运行 DeepSeek Harness 桥接（JSON-RPC 2.0 NDJSON）；
# 喂入一个 fixture，stdout 每条响应恰好一帧
libra agent bridge --stdio < bridge-initialize.ndjson

# 面向代理的结构化 JSON 信封
libra agent --json status
```

`libra agent --help` 会渲染同一横幅，因此文档和 CLI 表面保持同步（跨命令 `--help` EXAMPLES 推出，见 `docs/development/commands/_general.md` 条目 B）。

## 延后 parity（非目标）

以下 external-agent parity 表面在本波次**有意不**公开。它们连同处理方式与重启条件一并记录在 Agent tracing 契约（[`../../development/tracing/agent.md`](../../development/tracing/agent.md) 的「还未实现的功能」表）中，在此点名以免脚本或用户产生依赖：

- **`agent add`/`remove` 的 `--local-dev` / `--force` 标志**未发布——只使用规范的 `enable` / `disable`（及其 `add` / `remove` 别名）。若未来发布，会同时接到规范动词及其别名上。
- **Provider 专属 transcript compaction / reassemble trait** 是未来 parity 项。writer 已把大 transcript 存为 manifest 相对 chunk，但尚无 provider 专属 compactor/reassembler。
- **可选 capability trait**（`ProtectedFilesProvider`、`TranscriptCompactor`、`HookResponseWriter`、`RestoredSessionPathResolver` 等）在已落地的 `DeclaredAgentCaps` 矩阵之外尚无公开行为。
- **v2 `info` / capability gate 之外的 external-RPC method family** 未实现；未声明某项 capability 的代理继续 fail-closed。
- **非首批 roster 不受支持。** 仅 `claude-code`、`codex` 与 `opencode` 为 supported、hook-installable 且可用于 review/investigate 启动。`gemini`（仅 uninstall，见上文说明）、`cursor`、`copilot` 与 `factory-ai` 均为 `supported=false`，不出现在 `agent list`，`add`/`enable` 返回可操作的 unsupported 错误；它们不可启动。

## 说明

- 外部 `libra-agent-*` 代理**默认禁用**。使用 `libra config set agent.external_agents.enabled true`（仓库级）显式开启；开启前 `rpc list`/`libra-agent-*` 的 `rpc trust`/`rpc invoke` 会以 `LBR-AGENT-002` 拒绝（`rpc untrust` 始终可用——撤销信任只会收紧安全面；`rpc trust --dir <path>` 与 provider-exporter 信任如 `rpc trust opencode` 属纯准备动作——不扫描 `$PATH`、本身不启用任何东西，因此门控下亦可用）。已发现的二进制在 `rpc trust <slug>` 记录来源前保持隔离（world-writable 目录中的二进制拒绝信任）；每次 invoke 都会复验来源（漂移即撤销信任，`LBR-AGENT-005`）；子进程环境被清空为白名单注入，stderr 被捕获/限长/脱敏——绝不继承。invoke 超时、broken pipe、malformed frame 映射 `LBR-AGENT-012`；IO 硬上限超限映射 `LBR-AGENT-007`。

- `libra agent enable` 为 Claude Code 与 Codex 安装的 hook 配置调用顶层 `libra hooks claude|codex <verb>`，并带有 installer 根据 handler timeout 生成的受限隐藏 `--capture-budget-ms` 参数（默认 Claude `10s → 9000`、Codex 普通事件 `30s → 29000`；Codex `SessionEnd` 因宿主三秒上限为 `3s → 2000`；一秒的历史 handler 仍保留可用的 `500ms` capture 切片，另保留最多 1000ms 用于 terminal finalizer 清理）。重跑 enable 会刷新旧配置，隐藏别名也接受该参数以兼容历史已安装命令。已安装的 OpenCode plugin（`.opencode/plugin/libra-hooks.js`）则通过隐藏的 `libra agent hooks opencode <verb>` 入口转发所有事件，不带 installer budget；其 capture deadline 为 OpenCode export deadline。Claude 与隐藏别名在 hook envelope 未通过大小 / UTF-8 / JSON / schema / reported cwd / transcript 路径校验时，会以 `LBR-AGENT-008`（退出码 128）fail-closed 拒绝，且绝不回显 raw stdin。两个路径类字段会在存储解析前限制为 4096 字节。已安装的 Codex 表面会有意确认 malformed/unbound callback 与辅助性的 nonterminal 捕获失败，以免失败阻断宿主任务：通用 ingress、scope 和存储失败只输出经净化诊断且不创建 capture 行。在任何 Libra 仓库之外触发、格式正确的回调没有可绑定的 scope：已安装的 Codex hook 会确认它（退出码 `0`，包括 `SessionEnd`），而 Claude 与隐藏别名返回固定、不含路径的仓库未找到错误 `LBR-REPO-001`（退出码 128）。已损坏的活动仓库不视为“仓库之外”：Claude、隐藏别名（Claude Code / OpenCode / Codex）、gemini 入口以及已安装 Codex 的 `SessionEnd`（仍以非零退出）返回与其他所有仓库命令相同、不含路径的 `LBR-REPO-003`（无法解析的 linked worktree storage，附 `libra worktree repair --confirm <worktree-path>` 修复建议）、`LBR-REPO-002`（数据库缺失或 object format 不受支持）或 `LBR-IO-001`（数据库无法打开——包括由更新版本 Libra 写入——或 object format 无法读取），绝不返回通用的 `LBR-INTERNAL-001` capture 失败（见 `libra hooks`）。与其他所有仓库命令一样，这些入口打开数据库时会自动应用待处理的仓库 schema 迁移。但已校验且已绑定 scope 的 `SessionEnd` 必须持久化或观察到 durable terminal completion/pending receipt；在该证据存在前发生的有界数据库/finalizer 失败会以非零退出，而不会被静默确认。已有 catalog reservation 后的狭义 checkpoint 或 maintenance-lock 失败，才可能仅持久化一条不含敏感内容、可重试的 catalog 诊断。该预算约束可杀 helper、数据库和其它 deadline-aware 阶段；它不是严格的 whole-process host deadline，因为 source 分类、canonicalize、安全打开和 rewind 当前仍在 descriptor 交接前同步发生。NFS/FUSE stall 因而无法被强制取消，这条路径也不承诺 timeout 后的 FD-only autonomous replay。对不一致 store 执行 checkpoint 操作（如 `checkpoint rewind`）——catalog 行的 `parent_commit` 非法或指向缺失的 traces 对象——会以 `LBR-AGENT-009`（退出码 128）失败；运行 `libra agent doctor` 检查 store。
- 受管 Session Capture 当前要求 Unix 才能安全初始化 repository-private deduplication key。non-Unix platform 会在创建 key file 前 fail-closed 并给出可操作的诊断，而不会使用较弱的 path-based publication fallback。该固定能力诊断会显式到达两个 Codex 入口：nonterminal `hooks codex` callback（包括隐藏历史 alias）向 stderr 输出不含路径的 Unix-host remedy 但以 `0` 退出；`SessionEnd` 以同一 remedy 非零退出。fail-closed 的已安装 Claude 表面与隐藏的 `agent hooks` Claude Code / OpenCode 别名对每个事件都携带同一 remedy 与 `LBR-UNSUPPORTED-001` 非零退出。这个例外不改变 alias 对 malformed envelope 的 fail-closed 契约。它只适用于在 Libra 仓库内触发的回调：在任何 Libra 仓库之外触发的回调在所有平台（无论是否为 Unix）上都遵循上文的仓库外契约。
- `checkpoint rewind --apply` 只恢复工作树文件；代理自身的 transcript 文件不会被重写。
- Hook 和捕获诊断采用 best-effort 方式，设计目标是报告可操作的安装状态，而不是静默忽略缺失的提供商。

### Doctor checkpoint 存储修复（`--repair`）

`libra agent doctor` 按 AG-20 修复矩阵扫描 checkpoint 存储及 writer marker registry；不带 `--repair` 时严格只读，仅报告 `--repair` 将执行的动作：

| `inconsistency_type` | 含义 | `--repair` 动作 |
|----------------------|------|----------------|
| `stale_catalog_row` | `agent_checkpoint` 行的 `traces_commit`/`tree_oid`/`metadata_blob_oid` 与仍可从 `refs/libra/traces` 到达的 checkpoint 不一致 | 从 ref 重建该行的 OID 列（幂等 UPDATE） |
| `missing_objects` | checkpoint 对象在对象库中真正缺失（且无法从 ref 重建）——best-effort 的对象/manifest 枚举覆盖 E4 协议树：`manifest.json`、`events/lifecycle.jsonl`、`transcript/<agent_kind>.jsonl`（含分片）、`redaction_report.json`、`content_hash.txt` 与中间 tree。它只下钻协议拥有的 `events`/`transcript` 子树，并限制 tree 条目、对象总数和 manifest 声明数；命中上限即标记 `manual_required`，绝不做部分修复。它不解析或验证 lifecycle 行 schema、UUID 派生或 `identity_scheme` 值。 | 无——标记 `manual_required`；doctor 绝不执行破坏性动作（可尝试 `libra fsck --heal` 或从云端/备份恢复） |
| `missing_catalog_row` | ref 可达的 checkpoint 没有 catalog 行（崩溃窗口 B 残留） | 通过 writer 同款「先探测再插入」的幂等路径重插该行，字段从 commit 的 `metadata.json` 重建（v1 与 v2 两种 shape 均可解析） |
| `missing_object_index` | checkpoint 对象在 `object_index` 中缺行（`libra cloud sync` 看不到）——覆盖 traces commit 加有界的 E4 协议对象集 | 按 writer 行语义幂等补插（tree 记 `tree`，transcript blob 记 `agent_transcript`，sidecar 记 `blob`）。修复大小前 doctor 会在 transcript cap 内流式完整性校验已持有的本地 loose object；绝不采用 manifest `byte_len`，也不渲染 payload。 |
| `expired_inflight_marker` | 有效 traces writer marker 已过 TTL，包括 provisional preclaim 与已确认新建 loose-object OID 的崩溃残留 | 在最终 ref 事务中 fence 过期 writer，执行串行化的全仓库 root 证明并退役 marker；inline recovery 不 unlink 共享 payload、不删除 `object_index`，物理回收交给仓库 GC |
| `invalid_inflight_marker` | marker JSON、行身份、commit 或 OID 损坏 | 无——标记 `manual_required`；无法解码所有权时自动删除不安全 |
| `conflicted_coverage_claim` | 两个不同的完整 payload 声明了同一逻辑 turn；doctor 只报告每次报告内的不透明序号、受限的 schema/revision/completeness 事实，以及已保留 challenger 脱敏证据这一事实 | 无——标记 `manual_required`；检查耐久脱敏候选并显式选择恢复方案，不能静默丢弃 provenance；`--repair` 绝不替 operator 选择 winner |
| `inconsistent_subagent_content` | current 子代理 content claim 与其不可变 revision、checkpoint catalog 行或 association link 缺失或不一致 | 无——标记 `manual_required`；在 companion 关系及 checkpoint 对象/ref 可达性恢复前，unchanged replay 一律 fail-closed |
| `unresolved_subagent_link` | current 子代理 content checkpoint 没有唯一、provider-stable 的 boundary 匹配（Claude 的常态） | 无——标记 `manual_required`；content 与 boundary 证据分别保留，doctor 绝不猜测关联 |

补充规则：

- 仓库与数据库失败沿用共享的仓库契约（exit 128）。在仓库之外运行时，doctor 以 `LBR-REPO-001` 失败，并给出与其他仓库命令相同的 `libra init` / Git 转换提示；已脱离（detached）、迁移中或损坏的 linked worktree 以 `LBR-REPO-003` 失败，并原样给出该 worktree 的修复指引。doctor 自行打开仓库数据库：数据库缺失为 `LBR-REPO-002`，无法打开（包括由更新版本 Libra 写入）为 `LBR-IO-001`，不支持的 `core.objectformat` 为 `LBR-REPO-002`。这些数据库错误消息不包含数据库路径，也不回显存储的 object format 值。
- **legacy-v1 checkpoint**（升级前布局，无 `manifest.json`）计入 `legacy_v1_checkpoints`，永不进入 checkpoint 对象修复类别，也永不被 `--repair` 改写。
- pending terminal-finalizer 诊断使用只读 **keyset pagination**：每页 32 个 session，每次最多扫描 512 个 session、返回 128 个 receipt finding，receipt metadata 在解码前限制为 1 MiB。坏行通过固定、无敏感内容的 note 报告，并继续扫描后续有效行；跳过坏 key 或达到上限意味着报告不完整，不能认为未列出的 receipt 健康或已修复。
- 没有 durable artifact 的 receipt 属于 **`pending_source`** 证据；doctor 不会重开 provider source 或制造可重放 snapshot。后续 stop/resume 使 receipt superseded 后，它仍标为 `manual_required`，session-quarantined receipt 也是如此；即使 `--repair` 也不改写当前 session 或原 receipt，quarantine 不代表成功捕获。有 artifact 的 receipt 达到原始重试上限时，quarantine routing 只移动私有 artifact header，保留原 receipt 的 revision、status、counter、generation 和 source fence。符合条件的 authenticated artifact 只有在 alias、receipt、marker 和 coverage fences 全部复核后才会本地重放。单次 `doctor --repair` 最多计入五次重放尝试；自动重放共享一个 2 秒 cooperative deadline，每次 audited 人工尝试各自取得新的 2 秒 deadline；因上限或 retry-later 而延期的 finding 不计数、保持未修复，重新运行命令可继续有界队列。只有 checkpoint durable 写入且原 terminal receipt 完成后，finding 才标记为已修复。过期 artifact 隔离后仅允许一次 audited manual replay；失败或 fence 过期的尝试仍需人工恢复。永久拒绝的自动 replay 只把 artifact header 停放到 quarantine；原始重试/时间上限到期前，doctor 不会自动重试该 header，也不会让它消耗后续 replay 配额。
- schema-v1 lifecycle JSONL 行没有类型化 `identity_scheme`，始终是 opaque legacy evidence。`doctor`、`checkpoint show` 与 `checkpoint export` 不会从这类行或 event UUID 推断 HMAC/replay 信任。
- 被**存活的 traces in-flight marker** 覆盖的 checkpoint 是写入中的 writer，不算不一致，会被跳过。
- finding 标识保持可操作，但绝不回显损坏输入：checkpoint-store finding 携带规范 checkpoint UUID（in-flight marker 类别为 writer attempt 的 checkpoint id，该 checkpoint 可能从未发布），`findings_store` finding 携带可用于 `libra review show` / `libra investigate show` 的 review/investigate `run_id`。存储的 checkpoint 标识若不是规范 UUID，只显示为 `checkpoint-id-redacted` 或每次报告内的不透明标签；`conflicted_coverage_claim` 始终使用每次报告内的序号。
- **过期的 terminal writer marker** 不授权将后来的 transcript 重绑到其 pending receipt。`--repair` 只退役陈旧 marker 的所有权，绝不改变 receipt 的 source 或 marker fence。只有原来的已授权 source 仍可用时，才可重跑原 hook/replay；同一 native terminal 事件若携带变更后的 source，会以 `source_digest_conflict` 隔离并要求人工恢复，绝不发布 checkpoint、traces ref 或 stopped 状态。
- **没有 checkpoint 的 session 是合法中间态**（active session 尚未产生首个 stop），绝不被标记；只有 checkpoint-without-session 才算 orphan。
- 已捕获的 **gemini 行保持可读**且绝不被标记；残留的 gemini hook **配置**会得到指向仅卸载通道（`libra agent remove gemini`）的提示。
- 对属于 **scoped** captured session 的 catalog 行修复，会把该 session 的 durable workspace lease 复核放在最终数据库写入处。若 lease 已 release、过期或被重新 fence，doctor 保持 finding 未修复并标为 `manual_required`；只有显式 `legacy_unknown` 行继续走历史上的无 fence 兼容修复路径。
- 所有修复均幂等——连续两次运行 `doctor --repair`，第二次不会做任何事。带 `--repair` 时，每次修复尝试发出一个 `agent.doctor.repair` tracing span（`inconsistency_type`、`repaired`、`manual_required`），transcript 内容绝不进入日志。

## Reasoning type contract (RG-01)

reasoning 类型契约区分受限元数据、经来源验证的内存 opaque 字节和
持久化 artifact；当前类型契约不改变上述运行时采集行为。
五态是 `provider_visible`、`encrypted_unavailable`、`opaque_archived`、
`not_present` 和 `unsupported_shape`。`encrypted_unavailable` 表示 provider
已声明密文但没有获授权的 decryptor，**不是采集失败**。未知 reasoning-like
形状在后续 provider 接线时必须使 turn partial 并写无 payload warning。
被选入密文字段校验器的 text/tool 块也按 `unsupported_shape` 拒绝，
即使带有未声明的 signature-like 键；普通内容须在调用校验器前分派。
`OpaqueEncryptedBytes` 不能序列化进普通 transcript，也不能转换为
`RedactedBytes`。只有 reasoning 模块能在已授权的来源上验证 Claude assistant
`thinking.signature` / `redacted_thinking.data` 的字段形状与 JSON 字符串类型后
构造该内存类型；原始转义 UTF-8 字节不重序列化、不做 base64 解码。
形状正确的 JSON 不是 provider 身份凭据。OpenCode 2.0.24 的
`reasoning.state` 是开放记录，不能仅凭它识别密文。此类型尚未接入持久化；当前尚无 live provider adapter 产生 artifact。
OpenCode 密文采集仍待注册经验证的 provider 字段。

本类型校验器仅接受最小合成 envelope；真实会话记录及额外 metadata 须在对应 provider adapter 卡审查后接入，当前不声称真实 JSONL 已支持。

## Readable reasoning projection (RG-04)

RG-04 加入可读 reasoning 投影路径：`provider_visible` 的 reasoning 文本是
唯一能进入 coverage 投影的 reasoning 内容，且必先经过 typed redaction ——
reasoning 文本中的 canary 秘密会计入 redaction report 并在投影、checkpoint
持久化或任何 metadata/digest 写入前移除。可读 reasoning 以独立的
`reasoning` 记录类型投影，携带 `provider` 与 `source_kind` 元数据；
它从不冒充 assistant 回答，也绝不写入未脱敏的 metadata 或日志。

`ProviderVisibleText` 的 serde 序列化/反序列化明确拒绝；调用者须经分类与 typed redaction 后使用 `canonical_turn_bytes` 或 `safe_turn_projection`。RG-04 不接入真实 provider adapter。


RG-02 将已核验的密文归档为 opaque artifact：每个 checkpoint manifest 可携带
`reasoning_artifacts[]` 数组（path/oid/sha256/byte_len/locator/provider/source_kind/
availability/decrypt_capability），以 `reasoning/encrypted/<sha256>` 存储逐字节精确对象；
相同密文字节去重为单对象、重复 locator 被拒绝、扇出有界且 fail-closed（512 条 /
256 KiB manifest / 32 MiB 总量）。`content_hash` 仍只覆盖四个普通角色；空 artifact
集保持 checkpoint 树逐字节不变。

新建 opaque artifact 对象使用标准 zlib stored blocks（level 0）以限制压缩 CPU；
canonical blob 字节和 Git OID 不变。高熵密文因此占用更多存储及镜像带宽；
普通对象仍用默认压缩，已存在的有效对象直接复用，不重新压缩。

artifact 的树可达性由 RG-06 接入；受控读取及导出由 RG-03 接入。
