# `libra auth`

按 host 作用域管理 HTTP token auth（Libra 扩展，lore.md §1.6）。Token-only v1：把完整生命周期 — 写入、读取、过期检测、撤销 — 放在同一个表面。

## 概要

```
libra auth login --host <host[:port]> [--username <u>] [--with-token] [--expires-at <RFC3339> | --expires-in <N>d|h|m|s]
libra auth status [--host <host>]
libra auth logout [--host <host> | --all]
libra auth clear
```

## 说明

`auth login` 为一个 host 存储 token。**刻意没有 `--token <value>` 标志** — argv 会落入 shell history 和 `/proc`；token 通过隐藏 prompt（TTY）或 `--with-token` stdin 传入：

```bash
printf '%s' "$TOKEN" | libra auth login --host git.example.com --with-token
```

### GitHub HTTPS PAT

GitHub HTTPS Git 接受个人访问令牌（PAT）作为 HTTP password。`libra auth` 会为 `github.com` host 存储该 token，并且只有后续 Libra HTTPS 请求匹配相同规范化 host:port 作用域时才以 Basic auth 发送它。

交互式终端中，优先使用隐藏 prompt，避免 token 写入 argv 或 shell history：

```bash
libra auth login --host github.com --username x-access-token --expires-in 90d
```

在隐藏 token prompt 中粘贴 PAT。脚本中，从环境 secret、密码管理器或 CI secret store 将 token 送入 stdin：

```bash
printf '%s\n' "$GITHUB_PAT" \
  | libra auth login \
      --host github.com \
      --username x-access-token \
      --with-token \
      --expires-in 90d
```

然后在不打印 secret 的情况下验证：

```bash
libra auth status --host github.com
```

使用类似 `https://github.com/OWNER/REPO.git` 的 HTTPS remote；**不要** 把 PAT 嵌进 remote URL（`https://x-access-token:PAT@github.com/...`），因为 URL 会通过 shell history、config、process lists、logs 和错误输出泄露。如果 Git-compatible consumer 调用 `libra credential fill` 时显式传入 `username=<your-login>`，请用相同 username 存储 token，因为 username pinning 会被遵守。

选择能够访问所需仓库的最小权限 GitHub token。对私有仓库、组织资源或启用 SAML 的组织，GitHub 策略可能要求额外仓库权限或 SSO 授权。见 GitHub PAT 文档：
<https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens>。

### Secret 存储与密钥处理

Token 使用全局 vault key（`~/.libra/vault-unseal-key`，0600）做 AES-256-GCM 加密，并以密文存储在全局 config DB 中 — `libra config get/list/unset` 既不能 dump、forge，也不能删除 `auth.token.*` 条目（auth 表面是唯一入口）。每个 host:port 作用域一个 token；重新 login 会覆盖。**OS-keyring backend**（lore.md 2.7）用 `libra config --global auth.backend keyring` 选择（发布二进制包含它；Linux 使用静态 vendored libdbus）：secret 随后存入平台 keychain，config store 中只保留非 secret marker。`libra auth migrate --to keyring|file` 会移动已存 token（probe、verify、幂等）；仅切换 `auth.backend` 是非破坏性的（lookup 会同时查询两边）。撤销总是触达两个 backend。`status` 会报告每个 token 的 `backend`，以及 keyring 条目缺失或服务不可用时的 `unreadable` 状态。

当全局 Libra vault key 是期望的本地 secret root 时，使用默认 file backend。若希望 GitHub PAT 和其他 host tokens 存在平台 keychain 中，使用 OS keyring backend：

```bash
libra config --global auth.backend keyring
libra auth migrate --to keyring
libra auth status --host github.com
```

`auth status` 会报告活动 backend，且永不暴露 token material。切回加密 file backend 需要显式执行：

```bash
libra auth migrate --to file
```

**Attach rules（信任边界，stored tokens）**：stored token 只会发送到规范化 host:port 匹配、且使用 **https** 的请求（http 只允许 loopback hosts — 注意未显式带端口存储的 token 规范化为 443，因此非 443 loopback remote 需要用显式端口 login）。跨 host 请求永远看不到它。会把 https→http 降级的 redirect 会被直接拒绝（reqwest 只会在 host/port 变化时去掉凭证，不会在 scheme 变化时去掉）。交互式 401 prompt 仍然是进程级 fallback，且优先级更高。`credential fill` helper 也会查询 store（仅 https，silent misses，遵守 username pinning）；`credential store/erase` 永不管理 auth tokens。

`auth status` 永远不打印 token：对每个 host，它报告 username、expiry，以及 `valid` / `expired` / `undecryptable`（key 已变化 — 重新 login）。带 `--host` 时可脚本化：当且仅当存在有效 token 时退出 0。过期 token 在使用时会伴随 `auth login` 提示发出 warning。

**交互流程**：非 TTY 运行遇到 401 会快速失败并给出 `auth login` 提示（管道协议数据永远不会被 prompt 消费）；TTY 中 prompt 显示一次提示，且在一次 prompted attempt 真正成功后，会以每个 host 一次、默认 No 的方式询问是否存储 credential（`auth.saveOnPrompt` = `ask`/`always`/`never`）。

`auth logout --host <h>` 撤销一个 host；`--all` / `auth clear`（Lore 的动词）撤销全部 — 即使 key rotation 后撤销仍可工作（不需要解密），且操作幂等。

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | 成功（`status --host`：存在有效 token）。 |
| `1` | `status --host` 没有有效 token。 |
| `128` | 存储失败。 |
| `129` | 用法错误（错误 host/expiry/username；非 TTY 且无 `--with-token`；空 token）。 |

## 示例

```bash
libra auth login --host git.example.com              # hidden prompt
printf '%s' "$TOKEN" | libra auth login --host git.example.com --with-token
libra auth login --host github.com --username x-access-token --expires-in 90d
printf '%s\n' "$GITHUB_PAT" | libra auth login --host github.com --username x-access-token --with-token
libra config --global auth.backend keyring && libra auth migrate --to keyring
libra auth login --host git.example.com:8443 --expires-in 30d
libra auth status                                    # 所有 hosts，无 secrets
libra auth status --host git.example.com && echo ok  # 可脚本化
libra auth logout --host git.example.com
libra auth clear
```

## 与 Git 对比

Git 将此委托给 credential helpers（`git credential-store` 会把明文写入 `~/.git-credentials`；managers 是外部程序）。Libra 原生提供静态加密的 host tokens；repo-scoped `libra credential` helper protocol 仍用于 Git-compatible flows。在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中分类为 `intentionally-different`。
