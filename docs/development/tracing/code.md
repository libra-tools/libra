# `libra code` 开发设计

## 文档职责

本文是 `docs/development/tracing/plan.md` 的 Code 阶段目标文档，承接 C1~C8。它只描述 `libra code` 的内部 AgentRuntime、TUI/Web/headless/MCP、approval/sandbox/tool gate、session persistence 与 mutating fix bridge；`libra agent` 的 observed external-agent 捕获、hook、transcript、checkpoint 和 read-only review/investigate evidence 由 [`agent.md`](agent.md) 负责。

内部 AgentRuntime / Web-only 迁移的完整历史计划在 `docs/development/internal/code-agent-runtime.md`。本文只引用该文档中的源码锚点和 fix-bridge 证据，不恢复旧 `docs/development/code-agent-runtime.md`、`docs/development/agent.md` 或 `docs/development/web-only.md`。

## 命令实现目标

`libra code` 的目标是启动人类开发者与 AI agent 协作的受控编码会话。默认模式仍是交互式 TUI + 后台服务；普通请求先进入可审阅的 IntentSpec / 执行计划流程，再由用户确认是否执行。Code 阶段的核心目标不是发明新命令，而是把现有 mode、provider、Web/headless、MCP、session、approval、sandbox 和文档测试契约按源码事实收敛。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Libra AI extension, not a Git command。
- 该命令属于 Libra 扩展；重点是清晰边界、结构化输出、稳定错误和可测试的 mode/provider 约束，不追求 Git 同形。

## 当前源码事实

- 入口与分发：`src/cli.rs::Commands` 公开接入；`src/command/mod.rs` 导出；主要实现文件是 `src/command/code.rs`，入口为 `execute`。
- 参数模型：`CodeArgs`、`CodeProvider`。`validate_mode_args` 当前负责三类 mode 校验：TUI 默认、`--web`/`--web-only`、`--stdio`。
- `--stdio` 是 MCP stdio transport。源码在 `validate_mode_args` 中明确拒绝 `--control write` 并提示使用 `libra code-control --stdio` 做本地 automation。
- 非 TUI mode 当前调用 `reject_non_tui_flags`，会拒绝 `--resume`、`--model`、`--temperature`、`--env-file`、provider-specific flags、非默认 `--approval-policy`、`--network-access`、`--api-base` 等。该函数还会拒绝 `args.provider != CodeProvider::Gemini`（无 codex 豁免），因此当前源码行为与 help/banner 示例中**所有非 Gemini provider** 的 web-only 组合表述冲突（如 `--web-only --provider ollama`、`--web-only --provider codex --browser-control loopback`；`BrowserControlMode` 注释与 banner 亦受影响，Codex web-only 的 loopback/app-server 分支当前为 CLI 不可达代码）；C1 必须先把它分类为 docs drift、help drift 或 code behavior 再改。
- provider-specific 约束：`--codex-bin`、`--codex-port`、`--plan-mode=true` 只允许 `--provider=codex`；`--api-base` 在 `--provider=codex` 下被拒绝；Ollama/DeepSeek/Kimi 的 thinking/stream/compact flags 只能用于对应 provider。
- `--control write` 要求 loopback host；control token、control info、browser control 和 Code UI API 的安全边界必须继续由 Code UI / code-control 相关测试守卫。

```mermaid
flowchart TD
    A["src/cli.rs::Commands"] --> B["src/command/code.rs::CodeArgs"]
    B --> C["validate_mode_args"]
    C --> D{"mode"}
    D -->|"TUI default"| E["AgentRuntime + TUI/Web services"]
    D -->|"web-only"| F["Headless/Code UI server path"]
    D -->|"stdio"| G["MCP stdio server"]
    E --> H["SessionStore / projection / graph / audit"]
    F --> H
    G --> I["MCP tool surface only"]
    E --> J["approval / sandbox / tool ACL"]
```

## Code 阶段契约

| 面向 | 当前结论 | 必须保持 / 补强 |
|---|---|---|
| Mode 与参数 | TUI、web-only、stdio 已共用 `CodeArgs` 和 `validate_mode_args`，但 help/banner 与实际 web-only provider 校验存在漂移风险。 | C1 先做 source-grounded audit；C2 再决定是修 help/docs 还是放宽实现。任何 mode 变更必须有 CLI regression。 |
| Provider / env | provider-specific flags 和 `--api-base` 规则已有校验；live/provider tests 依赖 `.env.test` 时不得泄露 key。 | C3 固定 provider factory、env-file 优先级、Vault/env lookup、missing-key 错误和 feature-gated live tests。 |
| Web-only / Code UI | Code UI API、SSE、browser control、control token、diagnostics redaction 是用户可见接口。 | C4 固定 `/api/code/*` observe-only contract；control token 0600；diagnostics/SSE/control info 不泄露 secrets。 |
| Session / graph | `--resume` 只应在 TUI path 允许；projection、graph handoff、audit sink 不能与 user transcript 混用。 | C5 固定 SessionStore JSONL unknown-event-safe、truncated-tail recovery、graph handoff 和 resume audit。 |
| MCP / code-control | `libra code --stdio` 是 MCP stdio server；`libra code-control --stdio` 是 automation/control client。 | C6 禁止把 MCP stdio 当 turn control plane；双入口 tool set、shutdown、token/lease gate 都要有测试。 |
| Sandbox / approval / fix bridge | workspace mutation 只能走内部 AgentRuntime serialized queue、approval、sandbox 和 tool ACL。 | C7 是 `review --fix` / `investigate fix` 的唯一解锁点；证据不足时 Agent 阶段必须返回 `ERR_AGENT_FIX_BRIDGE_UNAVAILABLE` 对应错误码。 |
| Docs / compat | `libra code` 是 Libra-only extension；用户文档、compat matrix、tests/INDEX 必须与源码同步。 | C8 收敛 `docs/commands/code.md`、zh-CN、`COMPATIBILITY.md`、`tests/INDEX.md`、release notes。 |

## C1~C8 任务映射

| 任务 | 目标 | 关键验证 |
|---|---|---|
| C1 source-grounded audit | 核对 `CodeArgs`、`CodeProvider`、`validate_mode_args`、Code UI routes、MCP stdio、resume、graph、audit sink；输出 code behavior / docs drift / test gap / deliberate difference 清单。 | `rg -n "validate_mode_args|reject_non_tui_flags|CodeUi|HeadlessCodeRuntime|LibraMcpServer|TracingAuditSink|SessionStore" src/command/code.rs src/internal/ai` |
| C2 mode/argument hardening | 固定 TUI/web-only/stdio 的互斥、provider-specific flags、错误消息和 JSON/quiet 行为。 | `cargo test --test code_cli_dispatch_test` |
| C3 provider/runtime/env | 固定 provider factory、Codex runtime、agent profile override、dotenv/Vault/env lookup 和 missing-key errors。 | `cargo test --test code_provider_boot_test`; `cargo test --test code_codex_runtime_test` |
| C4 Web/control/SSE | 固定 Code UI observe-only API、SSE、browser control、control token、diagnostics redaction。 | `cargo test --features test-provider --test code_ui_remote_security_matrix -- --test-threads=1`; `cargo test --test ai_code_ui_wire_test` |
| C5 session/graph/persistence | 固定 resume、SessionStore JSONL、projection bundle、graph handoff 和 audit sink。 | `cargo test --features test-provider --test code_resume_test -- --test-threads=1`; `cargo test --test ai_session_jsonl_test` |
| C6 MCP/code-control | 分离 `libra code --stdio` 与 `libra code-control --stdio`。 | `cargo test --features test-provider --test code_mcp_dual_entry_test -- --test-threads=1`; `cargo test --features test-provider --test code_ui_remote_security_matrix -- --test-threads=1` |
| C7 sandbox/approval/tool gate | 固定 mutating path 的 approval/sandbox/tool ACL；控制 review/investigate fix bridge。 | `cargo test --test code_tool_acl_test`; `cargo test --features test-provider --test code_ui_remote_approval_matrix -- --test-threads=1` |
| C8 docs/compat closeout | 同步 tracing/code、用户文档、compat matrix、tests/INDEX 和 release notes。 | `cargo test --test compat_matrix_alignment`; `cargo test --all` |

（`code_ui_remote_*`、`code_resume_test`、`code_mcp_dual_entry_test` 整文件被 `#[cfg(feature = "test-provider")]` 门控，裸跑编译为 0 个测试空跑"通过"；完整验证命令口径以 plan.md §6/§9 为准。）

## 还未闭环的功能与风险

| 类别 | 风险 | 当前处理 |
|---|---|---|
| Mode 文档漂移 | help/banner 示例、`docs/commands/code.md` 或本文可能声称某 web-only provider 组合可用，但 `validate_mode_args` 实际拒绝一切非 Gemini provider（含 codex；Codex web-only loopback/app-server 分支为 CLI 不可达代码）。 | C1 必须先分类并给出修复方向；C2/C4 的 mode/provider 验收为以 C1 分类结果为前置的条件式验收（见 plan.md §6）。 |
| Mutating fix bridge | observed external agent 的 review/investigate findings 不能直接改工作区。 | 未找到内部 serialized fix bridge 证据前，Agent 阶段 fix/action 统一 unsupported。 |
| MCP/control 混同 | 把 MCP stdio 当 live turn/control plane 会绕过 token/lease/approval 边界。 | C6 固定 `code --stdio` 与 `code-control --stdio` 分工。 |
| Secret 泄露 | `.env.test`、provider key、control token、diagnostics、SSE、raw transcript 都可能泄露。 | live tests 关闭 xtrace；输出只保留 redacted summary；diagnostics/control/SSE 必测 redaction。 |

## 实现历史

- 2026-02-20 `5bef0a9e`（`invoke mcp interfaces in command code (#212)`）：基础实现节点。
- 2026-06-02 `37d0568c`（`feat(code): activate live-run registry end-to-end (child runner writes, /agents pane reads) (v0.17.1264, CEX-S2-16)`）：live-run registry 演进。
- 2026-06-02 `1723ed00`（`feat(code): wire sub-agent PatchSet store; persist merge candidates from libra code (v0.17.1232, CEX-S2-16)`）：PatchSet / merge candidate 持久化演进。
- 2026-05-31 `a94ee7d0`（`fix(code): record resume audit`）：resume audit 修正。
- 2026-05-30 `8ce6cedd`（`test(code): pin browser control matrix`）：browser control 测试契约。

历史条目只作为背景；当前行为以 C1 当轮源码复核和测试结果为准。

## 维护要求

- 改进本命令前，必须先阅读并遵循 [docs/development/commands/_general.md](../commands/_general.md)。
- 任何行为变更都要先核对实现源码，再同步 `COMPATIBILITY.md`、`docs/commands/code.md`、`docs/commands/zh-CN/code.md` 和相关测试。
- 新增或改变 public flag、JSON 字段、MCP tool、Code UI route、control file、approval/sandbox 行为时，必须明确兼容层级、稳定错误码、用户提示、测试 target 和回滚方式。
