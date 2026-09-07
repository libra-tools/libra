# `libra investigate` 开发设计

## 命令实现目标

`libra investigate`（plan.md Task A8 / AG-23）交付 read-only 的外部 agent
investigate workflow：以**严格轮询（strict round-robin）**——每轮一个
investigator，按 `--agent` 顺序——把首批三个外部 CLI（`claude-code`、`codex`、
`opencode`）在隔离 workspace 中以最小权限只读形态拉起，收集每轮 stance 落盘为
可审计的 run 目录。绝不把 A7 review 的并发扇入模型套到 investigate 上
（plan.md:996）。每个 run 要么收敛到 terminal state
（`quorum`/`max_turns`/`cancelled`/`timeout`/`error`），要么 PAUSE
（`stalled`/`agent_failure`）并可 `continue` 续跑。`fix` 校验已有 run 后只向活跃、
获授权的 Code runtime 提交固定可信请求；它复用 `review --fix` 的同一条 serialized
runtime 与全部安全 gate，不读取或转发 investigation 的不可信内容。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Libra read-only agent investigate
  extension (AG-23), not a Git command。
- 该命令是 Libra AI 扩展；重点是隔离执行、严格轮询、可审计 run wire、
  可续跑的 pause 语义、结构化输出与 fail-closed 错误，而不是 Git 同形。

## 设计方案

- 入口与分发：顶层命令 `src/cli.rs::Commands::Investigate`（CLI 面固定为顶层
  命令，与 `agent`/`review` 平级归入 ROOT_AFTER_HELP 的 "AI And Automation"
  组）。实现文件为 `src/command/agent/investigate.rs`——放在 `command/agent/` 下
  是为了复用 `checkpoint.rs` 中 `pub(super)` 的 AG-20 keyset 分页助手
  （`resolve_page_limit` / `encode_page_cursor` / `decode_page_cursor`），与
  `libra review` 同一复用。
- **引擎分层与复用**：`src/internal/ai/investigate/` 是 A7 `review/` 的严格轮询
  兄弟模块，**复用**（不复制）A7 的机器：
  - §0.3.2 只读 real-CLI argv builder + 最小 allowlist spawn
    （`review::build_reviewer_command` / `review::spawn_reviewer`）；
  - 有界 sink 捕获、redaction 管线、控制字符清洗、`render_untrusted_findings`
    （ANSI/控制序列剥离）、`findings_section`（spotlighting 定界）；
  - 隔离 workspace public seam（`materialize_isolated_workspace`）；
  - run-id / reviewer-name 校验与 redaction 汇总
    （`review::store::{is_valid_run_id, sanitize_reviewer_name}` +
    `RedactionReportSummary`，从 review store re-export）。
  investigate **自有**的两块：
  - `store.rs` —— 轮询 run state（`turn`/`next_agent_idx`/`completed_rounds`/
    `quorum`/`stances`/`pending_turn` …）、`kind="investigate"` 的 E8 manifest、
    单写 `findings.md`、以及 OS 级 per-run 锁（`RunLock`，flock）；
  - `runner.rs` —— 严格串行 turn loop（一次一个 investigator，按 agent 顺序）、
    quorum/max-turns terminal 分类、stall/agent-failure PAUSE（可续跑）、run
    级超时、共享 cancel/cleanup、`agent.investigate.run` span。
- 参数模型：`InvestigateArgs`（纯 subcommand，无裸形态）：`start --topic <text>
  --agent <slug>... [--max-turns N] [--quorum N]`、`list [--limit] [--cursor]`、
  `show <run_id>`、`continue <run_id>`、`cancel <run_id>`、
  `clean [--run <id>|--all]`、`fix <run_id>`。全局 `--json` 输出结构化 envelope。
- 输出与错误契约：全部经 `OutputConfig` / `emit_json_data` / `CliError`；
  `list`/`show`/run 的 JSON envelope 均带 `schema_version`；`list` envelope 为
  `{schema_version, items, next_cursor, has_more}`（统一分页契约，默认 50 /
  cap 500 / 不透明 keyset cursor，排序键 `started_at DESC, run_id DESC`）。

### Run 目录布局（E8-libra run wire，与 review 同目录）

```text
.libra/sessions/agent-runs/<run_id>/
  state.json          # schema_version、topic、agents、max_turns、quorum、
                      #   completed_rounds、turn、next_agent_idx、stances、
                      #   findings_doc、starting_sha、started_at、updated_at、pending_turn
  manifest.json       # E8 精确 12 键：schema_version、run_id、kind="investigate"、
                      #   agents、starting_sha、target_scope、terminal_state、
                      #   created_at、updated_at、findings_oid、redaction_report、manual_attach
  findings.md         # 单写、raw-redacted、spotlighting 定界、provenance=untrusted
  cancel.requested    # 跨进程取消标记（存在即请求；driver 每 200ms 轮询）
  .lock               # OS 级 per-run flock（同 run 并发 continue fail-closed）
  reviewers/<slug>.stdout.redacted.log
  reviewers/<slug>.stderr.redacted.log
```

`list`/`clean` 仅作用于 `kind="investigate"` 的目录，与 review run 共享目录但
互不干扰。`manual_attach` 由 `libra investigate attach <run_id> <file>`（A0-06）
填充：外部文件字节先经 `redact_untrusted` 脱敏，再内容寻址对象化（object_index
`o_type = agent_findings`），manifest 追加 `{oid,name,provenance:"manual",size,
attached_at}` 条目（只存 basename，防路径泄露）。`findings_oid` 在 terminal
manifest 落盘时由 findings.md 内容寻址写入；`libra agent doctor` 覆盖
`missing_findings_object` / `missing_findings_object_index` 两类扫描与修复。

### Terminal / pause 语义与 cancel

- **terminal**（`terminal_state` 落盘）：`quorum`（≥ quorum 个不同 investigator
  提交 concluding stance）、`max_turns`（turn 预算耗尽）、`cancelled`、`timeout`
  （run 级墙钟预算 `max_turns × 120s`，上限 3600s，`agent.md` 强制补强项 #11）、
  `error`（基础设施失败）。
- **paused**（`pending_turn` 落盘，`terminal_state` 仍为 None，可 `continue`）：
  `stalled`（成功 turn 空输出）、`agent_failure`（启动失败/非零退出/per-turn
  超时）。`continue` 从 `pending_turn`/`next_agent_idx` 续跑并重试该轮。
- **quorum 定义**（`agent.md` §5 未固定，故保守定义并测试钉死）：一个 stance 当其
  redacted stdout 含大小写不敏感的 `conclud` 记为 concluding；quorum 达成当**不同**
  agent（按 slug 去重）的 concluding stance 数 ≥ `quorum`。
- **stall 定义**：一个成功 turn 的 redacted stdout trim 后为空 → stall（pause），
  记 `pending_turn` 供 `continue` 重试。
- cancel 是一条共享 cleanup 路径（`InvestigateCancelHandle`）：前台 run 的
  SIGINT/SIGTERM → `cancel()`；`investigate cancel <run_id>` 写 `cancel.requested`
  标记，live driver 轮询到后 `cancel()`；CLI 侧等待最多 3s（15×200ms）确认，
  无人认领时直接 `store.mark_cancelled`。两条路径都杀 investigator 进程组、
  释放 workspace、`cancelled` 落盘。run lock 随 driver 进程退出自动释放。

### run-id 并发锁

`InvestigateRunStore::try_lock_run` 用 `flock(LOCK_EX|LOCK_NB)` 锁
`<run_dir>/.lock`。一次 drive（`start` 或 `continue`）持锁全程；同 run 并发
`continue` 的第二个持锁失败 → `InvestigateRunError::RunLocked` → CLI fail-closed
actionable error（plan.md:997，测试钉死）。flock 在 fd 关闭（含进程崩溃）时释放，
崩溃的 driver 不会留下永久卡死的 run。

### §0.3.2 spawn 形态（复用 A7 launcher，argv 与 review 一致）

Builtin investigator 复用 `review::build_reviewer_command`（§0.3.2 只读 argv：
codex `exec --sandbox read-only`、claude `-p --permission-mode plan`、opencode
`run`；禁用 flag 永不出现），gate 于 `launchable_investigate`（与 review 独立
声明的 capability 标记，首批同为 claude-code/codex/opencode）。每轮以 turn 专属的
spotlit prompt 重新构造 argv。

### 安全说明

- **隔离 workspace 必选**：investigator 一律运行在
  `materialize_isolated_workspace` 物化的镜像中，绝不 in-place；copy 后端按
  ignore 规则排除 secret 文件；AG-23 钉死 copy 后端。
- **untrusted seed 与 prompt spotlighting**：topic 是 untrusted seed；它与作为
  上下文注入的每个先前 stance，在进入任一 turn prompt 前都经 `redact_untrusted`
  并以明确定界（`<<<investigate-topic …>>>` / `<<<prior-investigator-stances …>>>`）
  包裹；定界符不可被 seed/stance 伪造（闭合定界被替换为 U+FFFD）。
- **untrusted findings**：`findings.md` 为 provenance=untrusted 的 raw-redacted
  文本；`show`（人类与 JSON 输出均是）必经 `render_untrusted_findings` 剥离
  ANSI/终端控制序列后才渲染。
- **`fix` 复用受控 bridge**：`investigate fix` 校验 run 存在后，调用 A7
  `review --fix` 的共享 controller lifecycle 与 runtime helper，提交编译期固定的
  `INVESTIGATE_FIX_ADMISSION_MESSAGE`。topic、stances、findings、attachments、run id
  均不进入请求，也不新建 mutation queue、hardening 表或 approval 表。无活跃/授权
  runtime 返回 `LBR-AGENT-010`；任何 `ReviewFixInput::UntrustedSeed` 在 control
  discovery 前返回 `LBR-AGENT-011`。后续 plan/approval/sandbox/network/ACL/tool gate
  以及 patch/repair 投影完全由既有 Code runtime 执行。

### 可观测性

引擎每次 drive 发出一个 `agent.investigate.run` span（`agent.md` §6）：必带
`run_id`、`turn`、`next_agent_idx`、`terminal_state`；seed raw text 为禁止字段。

## Examples

```bash
libra investigate start --topic "why is startup slow" --agent codex
libra investigate start --topic "auth bug" --agent codex --agent claude-code --max-turns 8 --quorum 2
libra investigate list --json
libra investigate show <run_id> --json
libra investigate continue <run_id>
libra investigate cancel <run_id>
libra investigate clean --all
```

## 当前状态

- 公开状态：已公开；`src/cli.rs::Commands::Investigate` +
  `command::agent::investigate`。
- 用户文档：`docs/commands/investigate.md`（zh-CN 同步页
  `docs/commands/zh-CN/investigate.md`）。
- compat 接线：`COMPATIBILITY.md` 顶层矩阵行（intentionally-different）、
  ROOT_AFTER_HELP "AI And Automation" 组行、`INVESTIGATE_EXAMPLES` +
  `after_help`、`tests/compat/help_examples_banner.rs` VISIBLE_COMMANDS 行、
  `tests/compat/agent_capability_matrix_pin.rs` 的 `launchable_investigate` pin。

## 还未实现的功能

- 无 `investigate fix` bridge 残留；未来若要让 investigation findings 驱动 mutation，
  必须另立设计并重新经过 provenance、redaction、approval、sandbox 与 tool gate，不能
  扩大当前固定请求的信任边界。
