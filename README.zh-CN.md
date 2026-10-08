[English](README.md) | 中文

![Libra](docs/image/libra-banner.png)

<div align="center">

# Libra — 面向 AI Agent 的 AI 原生扩展版本控制系统

**版本化整个软件创造生命周期，而非仅仅是代码。**

</div>

Libra 是 AI 原生版本控制系统，将兼容 Git 的代码历史与外部 AI 编程 Agent 捕获结合起来。它记录受支持 Agent 的会话、检查点元数据，并在会话内容可捕获时将脱敏快照存入 `refs/libra/traces`，让开发者可以查阅变更背景，并随仓库保存这些历史。

Libra 的对象格式和传输协议兼容 Git，可融入现有开发流程。捕获 hook 支持 Claude Code、Codex 和 OpenCode；这些工具负责运行编程 Agent，Libra 负责记录其工作。

<div align="center">

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/wingwangsz/libra/actions/workflows/base.yml/badge.svg)](https://github.com/wingwangsz/libra/actions/workflows/base.yml)
[![Discord](https://img.shields.io/badge/Discord-加入社区-%235865F2?logo=discord&logoColor=white)](https://discord.gg/MTbb5rDYsC)
[![X](https://img.shields.io/badge/X-%40git_mono_AI-%231DA1F2?logo=x&logoColor=white)](https://x.com/git_mono_AI)
[![文档](https://img.shields.io/badge/文档-docs.libra.tools-29B1FF)](https://docs.libra.tools)

</div>

---

## 核心差异

| 能力 | 传统版本控制（Git） | Libra |
|-----------|----------------------|-------|
| **版本化内容** | 代码历史 | 代码历史 + 捕获的 Agent 检查点和脱敏会话记录 |
| **AI 协作** | 手动提交信息 | 外部 Agent 会话与检查点历史 |
| **知识复用** | 代码快照 | 可随仓库历史查阅的捕获上下文 |
| **安全** | 外部 GPG/SSH 配置 | 内置 Vault，每个仓库独立隔离密钥 |
| **Agent 集成** | 不适用 | Claude Code、Codex 和 OpenCode 捕获 hook |
| **自动化** | 外部 CI/CD | 仓库级 Cron 与 VCS 事件规则 |

---

## 快速开始

### 安装

```bash
# macOS / Linux（推荐）
# 安装器先验证 Ed25519 签名的 release manifest 及二进制 sha256/size 再安装；
# 信任模型见 docs/installation.zh-CN.md。
curl -fsSL https://download.libra.tools/install.sh | sh

# Homebrew（macOS）
brew install libra

# 从源码编译（需要 Rust）
git clone https://github.com/wingwangsz/libra.git
cd libra
cargo build --release
```

脚本安装器还会创建可选短命令 `~/.libra/bin/lba -> libra`（相对 symlink）。
重复安装同一版本会修复缺失的 alias，不替换二进制。使用 `--no-alias` 或
`LIBRA_NO_ALIAS=1` 可关闭；已存在的用户自有 `lba` 绝不会被覆盖。详见
[安装器行为与选项](docs/installation.zh-CN.md)。

### 初始化你的第一个仓库

```bash
# 创建新的 Libra 仓库
libra init my-project
cd my-project

# 或从现有 Git 仓库转换
libra init --from-git-repository /path/to/existing/git/repo
```

### 使用 Agent 捕获

```bash
# 为你的 Agent 启用捕获钩子（以 codex 为例）
libra agent enable

# 正常使用你的 Agent 工具——Libra 会自动捕获会话和检查点
codex

# 查看已捕获的会话
libra agent session list
libra agent checkpoint list
```

> 查看 [Agent 捕获文档](https://docs.libra.tools/en/docs/getting-started/agent)了解所有支持的 Agent、高级配置和会话管理。

---

## 核心特性

### 🧠 外部 Agent 捕获与 traces 历史

`libra agent` 记录受支持的外部编程 Agent 的会话和检查点。检查点元数据保存在本地索引中；会话内容可捕获时，脱敏快照作为仓库对象存储，可从 `refs/libra/traces` 到达。可用 `libra agent session`、`checkpoint` 和 `doctor` 查阅捕获状态，用 `libra agent push` 将 traces 推送到远端。

内置 `libra code` UI 与执行器、其 MCP `--stdio` 入口及 `libra publish` 站点宿主已在 0.23.x 版本线移除。外部 Agent 捕获由 `libra agent` 提供；仓库备份使用 `libra cloud`。

```
.libra/
├── libra.db              # SQLite：VCS 状态 + Agent 捕获元数据
├── vault.db              # 加密的签名密钥与认证凭据
├── objects/              # 对象存储（loose + pack，与 Git 兼容）
└── sessions/agent/       # 本地 Agent 捕获事件日志
```

### 🔄 Git 兼容基础

Libra 使用 Git 的语言。磁盘格式（objects、index、pack、pack-index）和传输协议与标准 Git 服务器（GitHub、GitLab、Gitea 等）完全兼容。你可以零摩擦地向任何 Git 远程仓库 `push` 和 `pull`。

关键区别：Git 管理文件。Libra 管理**创造**。

### 🔐 Vault 安全

默认情况下，`libra init` 会创建仓库级加密密钥库：
- **GPG 签名密钥**用于提交验证
- **SSH 密钥**用于远程认证

签名和认证密钥按仓库隔离。

### 🛡️ 沙箱诊断

`libra sandbox status` 报告申请使用 Libra 内部沙箱的操作可用的后端与执行策略。外部编程 Agent 仍使用各自的执行与安全策略。

### ☁️ 分层云存储与备份

- **分层存储**：将大对象卸载到 S3/R2/RustFS，本地 LRU 缓存
- **云端备份**：将完整仓库状态（含 AI 历史）同步到 Cloudflare D1 + R2
- **可移植**：在不同机器之间迁移 Libra 仓库，AI 上下文完整保留

---

## 支持的外部 Agent

Libra 支持 Claude Code、Codex 和 OpenCode 的 hook 捕获。内置模型提供商执行器已随 `libra code` 在 0.23.x 版本线移除。

> 查看 [Agent 命令指南](docs/commands/zh-CN/agent.md)，了解捕获配置和支持的 Agent。

---

## 社区与资源

| 资源 | 链接 |
|----------|------|
| **官网** | [libra.tools](https://www.libra.tools) |
| **文档** | [docs.libra.tools](https://docs.libra.tools) |
| **Discord** | [加入社区](https://discord.gg/MTbb5rDYsC) |
| **X / Twitter** | [@git_mono_AI](https://x.com/git_mono_AI) |
---

## 贡献指南

我们欢迎来自开发者、AI 研究人员和所有热爱软件创造未来的人的贡献。在提交 Pull Request 之前，请确保你的代码通过我们的质量检查：

```bash
# 运行 clippy，所有警告视为错误
cargo clippy --all-targets --all-features -- -D warnings

# 检查代码格式（需要 nightly 工具链）
cargo +nightly fmt --all --check

# 如需要自动修复格式
cargo +nightly fmt --all
```

Windows 安装方法见 [Windows 安装说明](docs/installation.zh-CN.md#windows)。

详细贡献指南请参见 [贡献指南](docs/contributing.md)。

---

## 许可证

MIT 许可证 — 详情见 [LICENSE](LICENSE)。

Copyright (c) 2025-2026 Web3 Infrastructure Foundation.

Copyright (c) 2026 GitMono Limited.

---

<div align="center">

**[开始使用](https://docs.libra.tools/en/docs/getting-started) · [加入 Discord](https://discord.gg/MTbb5rDYsC) · [关注 X](https://x.com/git_mono_AI)**

</div>
