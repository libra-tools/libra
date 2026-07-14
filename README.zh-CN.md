[English](README.md) | 中文

![Libra](docs/image/libra-banner.png)

<div align="center">

# Libra — 面向 AI Agent 的 AI 原生扩展版本控制系统

**版本化整个软件创造生命周期，而非仅仅是代码。**

</div>

Libra 是一个 AI 原生基础设施，捕获并结构化软件开发的完整生命周期，记录从人类意图、AI 推理到验证和发布的每一个步骤。

我们的使命是确保每一次软件创造都成为持久的知识，而非被丢弃的工作流数据，赋能开发者、团队和 AI 系统去检索、复用并基于每一份软件背后的智能进行构建。

当 AI 成为软件的主要生产者时，Libra 提供基础架构来保存、累积并释放软件创造的长期价值。

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
| **版本化内容** | 仅源代码 | 代码 + AI 推理 + 决策 + 验证报告 + 会话记录 |
| **AI 协作** | 手动提交信息 | 原生 AI Agent 线程，完整审计追踪 |
| **知识复用** | 代码快照 | 跨项目可复用的智能资产 |
| **安全** | 外部 GPG/SSH 配置 | 内置 Vault，每个仓库独立隔离密钥 |
| **供应商锁定** | 不适用 | 7+ 家 AI 提供商，自由切换 |
| **自动化** | 外部 CI/CD | 内置 Cron 驱动的 Agent 自动化 |

---

## 快速开始

### 安装

```bash
# macOS / Linux（推荐）
curl -fsSL https://download.libra.tools/install.sh | sh

# Homebrew（macOS）
brew install libra

# 从源码编译（需要 Rust）
git clone https://github.com/wingwangsz/libra.git
cd libra
cargo build --release
```

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

### 🧠 AI 原生线程与持久化

每一个 AI Agent 会话都是 Libra 中的一等公民。线程、计划、任务、决策、验证报告、工具调用和代码补丁快照都直接持久化在仓库中，与代码共存。没有外部状态——一切都是可持久化、可查询、可回放的。

```
.libra/
├── libra.db              # SQLite：Git 核心 + AI 线程 + 运行时合约
├── vault.db              # 加密密钥库（提供商密钥、签名密钥）
├── objects/              # 对象存储（loose + pack，与 Git 兼容）
├── sessions/             # AI 会话记录（JSONL 格式）
└── ai/                   # AI 运行时工作文件
```

### 🔄 Git 兼容基础

Libra 使用 Git 的语言。磁盘格式（objects、index、pack、pack-index）和传输协议与标准 Git 服务器（GitHub、GitLab、Gitea 等）完全兼容。你可以零摩擦地向任何 Git 远程仓库 `push` 和 `pull`。

关键区别：Git 管理文件。Libra 管理**创造**。

### 🔐 Vault 安全

每次 `libra init` 自动创建仓库级加密密钥管理：
- **GPG 签名密钥**用于提交验证
- **SSH 密钥**用于远程认证
- **AI 提供商凭证**安全存储

无需外部密钥管理配置。每个仓库的密钥独立隔离，永不离库。

### 🛡️ 命令安全沙箱

每个 AI Agent 的工具调用都经过可配置的安全沙箱，包含命令预检、网络策略执行和可选的 seccomp/seatbelt 限制。定义 Agent 能做什么、不能做什么。

### ☁️ 分层云存储与备份

- **分层存储**：将大对象卸载到 S3/R2/RustFS，本地 LRU 缓存
- **云端备份**：将完整仓库状态（含 AI 历史）同步到 Cloudflare D1 + R2
- **可移植**：在不同机器之间迁移 Libra 仓库，AI 上下文完整保留

### 🌐 原生 MCP 协议支持

Libra 原生支持 [Model Context Protocol](https://modelcontextprotocol.io/)，可直接与 Claude Desktop、Cursor 和任何 MCP 兼容客户端集成。配置一次，到处使用。

```json
{
  "mcpServers": {
    "libra": {
      "command": "/path/to/libra",
      "args": ["code", "--stdio"],
      "cwd": "/path/to/your/libra/repo"
    }
  }
}
```

---

## 支持的 AI 提供商

Libra 目前已支持 Claude Code、CodeX 和 OpenCode；对其他主流 Agent 的支持将陆续发布。

> 前往 [docs.libra.tools](https://docs.libra.tools/en/docs/getting-started/agent) 查看提供商配置详情。

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

Windows 构建用户请查看 [Windows 构建指南](docs/installation/windows.md) 了解 OpenSSL 配置。

详细贡献指南请参见 [docs/development/contributing.md](docs/development/contributing.md)。

---

## 许可证

MIT 许可证 — 详情见 [LICENSE](LICENSE)。

Copyright (c) 2025-2026 Web3 Infrastructure Foundation.

Copyright (c) 2026 GitMono Limited.

---

<div align="center">

**[开始使用](https://docs.libra.tools/en/docs/getting-started) · [加入 Discord](https://discord.gg/MTbb5rDYsC) · [关注 X](https://x.com/git_mono_AI)**

</div>
