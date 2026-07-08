# `libra agent`

管理 Claude Code 和 Gemini 等工具的外部代理捕获。

## 概要

```bash
libra agent status
libra agent list [--json]
libra agent enable [--agent <name>]...
libra agent add [<name>...]
libra agent disable [--agent <name>]...
libra agent remove [<name>...]
libra agent session <subcommand>
libra agent checkpoint <subcommand>
libra agent clean [--all]
libra agent doctor [--repair]
libra agent push [--remote <name>] [--force-rewrite]
libra agent rpc <subcommand>
```

## 说明

`libra agent` 管理 Libra 的外部代理捕获表面。它安装和移除提供商 hook，报告已捕获的 session/checkpoint 状态，暴露只读诊断，并可将 `refs/libra/traces` 推送到远程。

支持的 roster 为 `claude-code`、`codex`、`opencode`（首批），三者均可安装 hook：`claude-code` 写 `.claude/settings.json`；`codex` 写用户级 `$CODEX_HOME/hooks.json` 并在 `$CODEX_HOME/config.toml` 写入 Libra 托管的 trust 条目（未受信的 Codex hook 会被静默跳过，trust 条目是安装的一部分）；`opencode` 写 Libra 托管插件 `.opencode/plugin/libra-hooks.js`（注意：`opencode --pure` 会禁用包括捕获在内的全部外部插件）。`gemini` 已从支持 roster 降级为仅卸载通道：`libra agent remove gemini` 可移除历史安装的 Libra 托管 hook（幂等），已捕获会话保持可读；对它或其它非 roster 代理执行 `add`/`enable` 会返回可操作的 unsupported 错误。

## 子命令

| 子命令 | 说明 |
|------------|-------------|
| `status` | 报告已捕获的外部代理会话状态 |
| `list` | 列出代理能力矩阵（roster、hook、安装状态） |
| `enable` | 启用一个或多个外部代理并安装 hook |
| `add` | `enable` 的别名：`add <name>` ≡ `enable --agent <name>` |
| `disable` | 禁用一个或多个外部代理并卸载 hook |
| `remove` | `disable` 的别名：`remove <name>` ≡ `disable --agent <name>` |
| `session list` | 列出已捕获会话 |
| `session show <id>` | 显示一个已捕获会话 |
| `session stop <id>` | 将已捕获会话标记为 stopped |
| `session resume <id>` | 将已停止的已捕获会话重新标记为 active |
| `session promote <id>` | 将已捕获会话提升为 Libra intent 元数据 |
| `session derive-tool-calls <id>` | 从已捕获会话推导工具调用记录 |
| `checkpoint list` | 列出已捕获 checkpoint |
| `checkpoint show <id>` | 显示 checkpoint 元数据 |
| `checkpoint rewind <id>` | 检查或应用某个 checkpoint 的工作树回退 |
| `checkpoint export <id>` | 导出 checkpoint transcript：默认脱敏（无需授权）；raw（未脱敏）导出须 `--allow-raw --raw` 并写入 append-only `agent_audit_log`（缺失授权时拒绝并返回 `LBR-AGENT-013`） |
| `clean` | 清理已停止会话的临时 checkpoint（prune 遇到进行中的 checkpoint 写入或 traces 引用可达但无 catalog 行的提交时 fail-closed 拒绝；同时删除因此不可达的 `object_index` 行） |
| `doctor` | 诊断 hook 安装和捕获状态；检测（`--repair` 时修复）checkpoint 存储不一致 |
| `push` | 将 `refs/libra/traces` 推送到远程（`clean` prune 重写后的非快进推送用 `--force-rewrite`，采用 force-with-lease 语义） |
| `rpc list` | 列出 `PATH` 上发现的 `libra-agent-*` 二进制（含 trusted/quarantined 状态）；需先开启 external-agents 开关 |
| `rpc trust <slug>` | 信任一个已发现的二进制——记录 path + sha256 + device/inode/mtime 来源（所在目录 world-writable 时拒绝） |
| `rpc untrust <slug>` | 撤销信任；二进制回到隔离状态（始终可用，不受开关限制） |
| `rpc invoke` | 在**已信任**的 `libra-agent-*` 二进制上调用一个 JSON-RPC 方法 |

## 常用选项

| 标志 | 子命令 | 说明 |
|------|------------|-------------|
| `--agent <name>` | `enable`, `disable` | 选择代理名称；省略时针对支持 roster（`add`/`remove` 以位置参数接收名称） |
| `--limit <n>` | `session list`, `checkpoint list` | 每页最大行数（默认 50，硬上限 500——超过时钳制并在 stderr 提示；`0` 按 `1` 处理） |
| `--cursor <cursor>` | `session list`, `checkpoint list` | 上一页 `next_cursor` 返回的不透明 keyset 游标；不要手工构造 |
| `--extract-transcript <path>` | `session show` | 将会话元数据中的已捕获 transcript 路径复制到本地文件 |
| `--all` | `clean` | 清理所有已停止会话的 checkpoint，而不只是最近一个 |
| `--gc` / `--retention-days <n>` | `clean` | 保留期 GC：删除已停止会话中早于 `agent.retention.transcript_days`（默认 90；用 `--retention-days` 覆盖）的 checkpoint，并清理早于 `agent.retention.stderr_days`（默认 30）的**终态** review/investigate run 的 reviewer stderr 诊断日志，同时保留该 run 的聚合记录（`state.json`/`manifest.json`/`findings.md` 与 stdout 日志）；永不触碰 `agent_audit_log` |
| `--repair` | `doctor` | 修复检测到的 checkpoint 存储不一致（从 `refs/libra/traces` 重建过期/缺失的 catalog 行，补插缺失的 `object_index` 行）；省略时仅检测 |
| `--remote <name>` | `push` | 选择用于推送代理 trace 引用的远程 |
| `--force-rewrite` | `push` | 允许本地 `clean` prune 之后的非快进推送（traces 引用由 Libra 托管，prune 即整链重写）；采用针对本仓库最近一次推送记录的 force-with-lease 语义——绝非无条件 force——远程被别处重写时仍 fail-closed 拒绝 |
| `--dry-run` | `checkpoint rewind` | 显示影响而不修改文件；这是默认值 |
| `--apply` | `checkpoint rewind` | 恢复所选 checkpoint 的工作树 |

## JSON 输出

支持结构化输出的子命令使用全局 `--json` 和 `--machine` 信封。例如：

```bash
libra --json agent status
libra --json agent list
libra --json agent checkpoint list
libra --json agent rpc list
```

`agent list --json` 携带稳定的 `schema_version` 与每个已知代理一行（`slug`、`agent_kind`、`stability`、`supported`、`support_wave`、`registered`、`transcript_readable`、`hook_installable`、`installed`、`launchable_review`、`launchable_investigate`、`external_binary`、`config_paths`、`protected_dirs`、`capabilities`）。行结构是面向自动化的冻结契约。

`agent session list --json` 与 `agent checkpoint list --json` 每次返回一页：`data` 携带 `schema_version`、位于 `sessions` / `checkpoints` 下的行（单行结构不变），以及 `next_cursor`——传回 `--cursor` 的不透明游标，列表耗尽时为 `null`。页序为最新在前（`started_at` / `created_at` 降序，行 id 作为并列时的次序键）。

`agent checkpoint show --json` 额外报告 `layout` 摘要（`e4-libra`、`legacy-v1` 表示 AG-20 之前的存量 checkpoint、`unknown` 表示 checkpoint tree 本地不可读），包含 manifest 角色、按 manifest 顺序列出的 transcript 分片、`content_hash` 格式校验，以及 transcript `availability` 标志（`present`/`missing`/`unknown`）——全程不读取 transcript blob 内容。

## 示例

```bash
# 显示已捕获会话数量和最近 checkpoint 摘要
libra agent status

# 显示代理能力矩阵（支持 roster、hook、安装状态）
libra agent list

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

# 显示一个会话并复制其已捕获 transcript
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

# 按 id 显示单个 checkpoint
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

# 面向代理的结构化 JSON 信封
libra agent --json status
```

`libra agent --help` 会渲染同一横幅，因此文档和 CLI 表面保持同步（跨命令 `--help` EXAMPLES 推出，见 `docs/development/commands/_general.md` 条目 B）。

## 说明

- 外部 `libra-agent-*` 代理**默认禁用**。使用 `libra config set agent.external_agents.enabled true`（仓库级）显式开启；开启前 `rpc list`/`rpc trust`/`rpc invoke` 会以 `LBR-AGENT-002` 拒绝（`rpc untrust` 始终可用——撤销信任只会收紧安全面）。已发现的二进制在 `rpc trust <slug>` 记录来源前保持隔离（world-writable 目录中的二进制拒绝信任）；每次 invoke 都会复验来源（漂移即撤销信任，`LBR-AGENT-005`）；子进程环境被清空为白名单注入，stderr 被捕获/限长/脱敏——绝不继承。invoke 超时、broken pipe、malformed frame 映射 `LBR-AGENT-012`；IO 硬上限超限映射 `LBR-AGENT-007`。

- 顶层 `agent hooks` 入口是隐藏的，面向由 `libra agent enable` 安装的 hook 配置；用户通常不会直接调用它。
- `checkpoint rewind --apply` 只恢复工作树文件；代理自身的 transcript 文件不会被重写。
- Hook 和捕获诊断采用 best-effort 方式，设计目标是报告可操作的安装状态，而不是静默忽略缺失的提供商。

### Doctor checkpoint 存储修复（`--repair`）

`libra agent doctor` 按 AG-20 修复矩阵扫描 checkpoint 存储的三类不一致；不带 `--repair` 时严格只读，仅报告 `--repair` 将执行的动作：

| `inconsistency_type` | 含义 | `--repair` 动作 |
|----------------------|------|----------------|
| `stale_catalog_row` | `agent_checkpoint` 行的 `traces_commit`/`tree_oid`/`metadata_blob_oid` 与仍可从 `refs/libra/traces` 到达的 checkpoint 不一致 | 从 ref 重建该行的 OID 列（幂等 UPDATE） |
| `missing_objects` | checkpoint 对象在对象库中真正缺失（且无法从 ref 重建）——检查覆盖完整 E4 树：`manifest.json`、`events/lifecycle.jsonl`、`transcript/<agent_kind>.jsonl`（含分片）、`redaction_report.json`、`content_hash.txt`、中间 tree，以及 manifest 声明的全部 blob | 无——标记 `manual_required`；doctor 绝不执行破坏性动作（可尝试 `libra fsck --heal` 或从云端/备份恢复） |
| `missing_catalog_row` | ref 可达的 checkpoint 没有 catalog 行（崩溃窗口 B 残留） | 通过 writer 同款「先探测再插入」的幂等路径重插该行，字段从 commit 的 `metadata.json` 重建（v1 与 v2 两种 shape 均可解析） |
| `missing_object_index` | checkpoint 对象在 `object_index` 中缺行（`libra cloud sync` 看不到）——覆盖 traces commit 加完整 E4 对象集 | 按 writer 行语义幂等补插（tree 记 `tree`，transcript blob 记 `agent_transcript`，sidecar 记 `blob`） |

补充规则：

- **legacy-v1 checkpoint**（升级前布局，无 `manifest.json`）计入 `legacy_v1_checkpoints`，永不进入三类不一致，也永不被 `--repair` 改写。
- 被**存活的 traces in-flight marker** 覆盖的 checkpoint 是写入中的 writer，不算不一致，会被跳过。
- **没有 checkpoint 的 session 是合法中间态**（active session 尚未产生首个 stop），绝不被标记；只有 checkpoint-without-session 才算 orphan。
- 已捕获的 **gemini 行保持可读**且绝不被标记；残留的 gemini hook **配置**会得到指向仅卸载通道（`libra agent remove gemini`）的提示。
- 所有修复均幂等——连续两次运行 `doctor --repair`，第二次不会做任何事。带 `--repair` 时，每次修复尝试发出一个 `agent.doctor.repair` tracing span（`inconsistency_type`、`repaired`、`manual_required`），transcript 内容绝不进入日志。
