# `libra sandbox`

检查当前机器的 AI 沙箱诊断信息。

## 概要

```bash
libra sandbox status
libra --json sandbox status
```

## 说明

`libra sandbox status` 报告 Libra 会用于 AI shell 执行诊断的沙箱后端。它不需要仓库，可用于排查外部 agent 捕获与只读 review 所在的 provider 或 CI 主机。

默认运行时采用 best-effort 策略：Linux 使用 `LIBRA_LINUX_SANDBOX_EXE` 配置的外部 helper；如果该 helper 不可用，`libra` 会尝试内置 `bwrap` 后端（可由 `LIBRA_BWRAP_BINARY` 覆盖）。当 `/usr/bin/sandbox-exec` 可用时，macOS 使用 Seatbelt；不支持或未配置的主机会报告警告，而不是声称提供隔离。设置 `LIBRA_SANDBOX_ENFORCEMENT=required` 后，当请求 Libra 内部沙箱但没有可应用的受支持后端时，命令会失败。

在 macOS 上，默认 Seatbelt 策略保持项目文件可读，但拒绝常见凭证、令牌和浏览器配置文件路径。内置拒绝列表包括 `~/.ssh`、`~/.aws`、`~/.gnupg`、`~/.netrc`、`.azure`、`.docker`、`.npmrc`、`.pypirc`、Cargo/Gem 凭证、`~/.config/gcloud`、`~/.config/gh`、`~/.config/hub`、`~/.kube`、`~/.config/libra/vault`、Firefox、Chrome、Chromium 和 Brave 配置目录、macOS `Library/Cookies`，以及 `/etc/shadow`。仓库可以通过 `.libra/sandbox.toml` 的 `deny_read = [...]` 追加项目特定路径。不可读或无法解析的 `sandbox.toml` 会 fail-closed（不会回退到空策略）。在 linked worktree 中以仓库文件为基线，per-worktree overlay 只能收紧 `deny_read` / `[sandbox.network]`，不能把 `denied` 放宽为 `full`。

## 人类可读输出

```text
Sandbox status
  platform: linux
  sandbox_type: none
  enforcement: best_effort
  effective_enforcement: best_effort
  network: denied
  proxy_backend: noop
  bwrap_available: false
  bwrap_requested: false
  seatbelt_available: false
  helper_path: (not configured)
  helper_path_exists: false
  writable_roots:
    - /path/to/workspace
  warnings:
    - linux sandbox helper is not configured; AI shell commands currently fall back to no OS sandbox
```

## JSON 输出

```json
{
  "ok": true,
  "command": "sandbox.status",
  "data": {
    "platform": "linux",
    "sandbox_type": "none",
    "enforcement": "best_effort",
    "effective_enforcement": "best_effort",
    "writable_roots": ["/path/to/workspace"],
    "network": {
      "mode": "denied",
      "allowlist": []
    },
    "proxy_backend": "noop",
    "bwrap_available": false,
    "bwrap_requested": false,
    "seatbelt_available": false,
    "helper_path": {
      "path": null,
      "exists": false
    },
    "warnings": []
  }
}
```

## 字段

| 字段 | 说明 |
|-------|-------------|
| `platform` | 运行中 Libra 二进制文件的 Rust 目标 OS |
| `sandbox_type` | 生效的 OS 沙箱后端；当前没有可用后端时为 `none` |
| `enforcement` | 来自 `LIBRA_SANDBOX_ENFORCEMENT` 的当前强制策略；`required` 会拒绝缺失内部沙箱，而 `best_effort` 会报告降级风险但不使命令失败 |
| `effective_enforcement` | 环境解析和回退警告后的强制模式 |
| `writable_roots` | 解析当前目录和临时目录后的默认 workspace-write 根 |
| `network.mode` | 当前网络策略摘要（`denied`、`allowlist`、`full`） |
| `network.allowlist` | 当 `network.mode` 为 `allowlist` 时的主机/服务允许列表 |
| `proxy_backend` | 选中的网络代理策略：`noop`（拒绝全部，由 `denied` 模式使用）、`allowlist`（按主机允许的代理，由 `allowlist` 模式使用）、`loopback-only`（`full` 模式诊断占位符），或 `none`（请求了 `allowlist` 模式代理但无法构建，原因可能是 `required` 强制层拒绝运行，或 `prefer_strict` / `best_effort` 将其降级为拒绝全部；原因见 `warnings`） |
| `bwrap_available` | `PATH` 上是否有可执行的 `bwrap` |
| `bwrap_requested` | 是否启用了 `LIBRA_USE_LINUX_SANDBOX_BWRAP` |
| `seatbelt_available` | `/usr/bin/sandbox-exec` 是否可执行 |
| `helper_path` | `LIBRA_LINUX_SANDBOX_EXE` 路径和可执行探测结果 |
| `warnings` | 降级或不支持平台的诊断 |

## 示例

```bash
# 显示 AI 工具执行的生效沙箱诊断
libra sandbox status

# 面向代理的结构化 JSON 输出
libra sandbox --json status

# 机器严格 JSON（隐含 --json=ndjson --no-pager --color=never --quiet）
libra sandbox --machine status
```

`libra sandbox --help` 会渲染同一横幅，因此文档和 CLI 表面保持同步（跨命令 `--help` EXAMPLES 推出，见 `docs/development/commands/_general.md` 条目 B）。
