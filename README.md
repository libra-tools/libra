[中文](README.zh-CN.md) | English

![Libra](docs/image/libra-banner.png)

<div align="center">

# Libra — An AI-native Extended VCS Built for Agents

**Versioning the entire software creation lifecycle, not just code.**

</div>

Libra is an AI-native infrastructure that captures and structures the full lifecycle of software development, documenting every step from human intent and AI reasoning to validation and release.

Our mission is to ensure that every software creation becomes lasting knowledge instead of discarded workflow data, empowering developers, teams, and AI systems to retrieve, reuse, and build upon the intelligence behind every piece of software.

As AI becomes the primary producer of software, Libra provides the foundational infrastructure that preserves, compounds, and unlocks the long-term value of software creation.

<div align="center">

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![CI](https://github.com/wingwangsz/libra/actions/workflows/base.yml/badge.svg)](https://github.com/wingwangsz/libra/actions/workflows/base.yml)
[![Discord](https://img.shields.io/badge/Discord-join-%235865F2?logo=discord&logoColor=white)](https://discord.gg/MTbb5rDYsC)
[![X](https://img.shields.io/badge/X-%40git_mono_AI-%231DA1F2?logo=x&logoColor=white)](https://x.com/git_mono_AI)
[![Docs](https://img.shields.io/badge/docs-docs.libra.tools-29B1FF)](https://docs.libra.tools)

</div>

---

## Key Differentiators

| Capability | Traditional VCS (Git) | Libra |
|-----------|----------------------|-------|
| **Versioned Artifacts** | Source code only | Code + AI reasoning + decisions + validation reports + session transcripts |
| **AI Collaboration** | Manual commit messages | Native AI agent threads with full audit trail |
| **Knowledge Reuse** | Code snapshots | Reusable intelligence assets across projects |
| **Security** | External GPG/SSH setup | Built-in vault with per-repo key isolation |
| **Provider Lock-in** | N/A | 7+ AI providers, switch freely |
| **Automation** | External CI/CD | Built-in cron-driven agent automation |

---

## Quick Start

### Install

```bash
# macOS / Linux (recommended)
curl -fsSL https://download.libra.tools/install.sh | sh

# Homebrew (macOS)
brew install libra

# From source (requires Rust)
git clone https://github.com/wingwangsz/libra.git
cd libra
cargo build --release
```

### Initialize Your First Repository

```bash
# Create a new Libra repository
libra init my-project
cd my-project

# Or convert an existing Git repository
libra init --from-git-repository /path/to/existing/git/repo
```

### Use Agent Capture

```bash
# Enable capture hooks for your agent (e.g., codex)
libra agent enable

# Run your agent tool normally — Libra captures sessions and checkpoints
codex

# Inspect captured sessions
libra agent session list
libra agent checkpoint list
```

> See [Agent Capture documentation](https://docs.libra.tools/en/docs/getting-started/agent) for all supported agents, advanced configuration, and session management.

---

## Core Features

### 🧠 AI-Native Threading & Persistence

Every AI agent session is a first-class citizen in Libra. Threads, plans, tasks, decisions, validation reports, tool invocations, and patchset snapshots are all persisted directly in the repository alongside your code. No out-of-band state — everything is durable, queryable, and replayable.

```
.libra/
├── libra.db              # SQLite: Git core + AI threads + runtime contracts
├── vault.db              # Encrypted secrets (provider keys, signing keys)
├── objects/              # Object store (loose + pack, compatible with Git)
├── sessions/             # AI conversation transcripts in JSONL
└── ai/                   # AI runtime working files
```

### 🔄 Git-Compatible Foundation

Libra speaks Git's language. On-disk formats (objects, index, pack, pack-index) and wire protocols are fully compatible with standard Git servers (GitHub, GitLab, Gitea, etc.). You can `push` and `pull` to any Git remote with zero friction.

Key difference: Git manages files. Libra manages **creation**.

### 🔐 Vault-Backed Security

Every `libra init` automatically creates a per-repository vault for encrypted key management:
- **GPG signing keys** for commit verification
- **SSH keys** for remote authentication
- **AI provider credentials** securely stored

No external key management setup required. Keys are isolated per repository and never leave the vault.

### 🛡️ Command Safety Sandbox

Every tool invocation from an AI agent passes through a configurable safety sandbox with command preflight checks, network policy enforcement, and optional seccomp/seatbelt restrictions. Define what agents can and cannot do.

### ☁️ Tiered Cloud Storage & Backup

- **Tiered storage**: Offload large objects to S3/R2/RustFS with local LRU caching
- **Cloud backup**: Sync your entire repository state (including AI history) to Cloudflare D1 + R2
- **Portable**: Move a Libra repository between machines with all AI context intact

### 🌐 MCP Protocol Native

Libra natively supports the [Model Context Protocol](https://modelcontextprotocol.io/), enabling direct integration with Claude Desktop, Cursor, and any MCP-compatible client. Configure once, use everywhere.

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

## Supported AI Providers

Libra already works with Claude Code, CodeX and OpenCode; support for other mainstream Agents will be released gradually.

> See [docs.libra.tools](https://docs.libra.tools/en/docs/getting-started/agent) for provider setup and configuration details.

---

## Community & Resources

| Resource | Link |
|----------|------|
| **Website** | [libra.tools](https://www.libra.tools) |
| **Documentation** | [docs.libra.tools](https://docs.libra.tools) |
| **Discord** | [Join the community](https://discord.gg/MTbb5rDYsC) |
| **X / Twitter** | [@git_mono_AI](https://x.com/git_mono_AI) |
---

## Contributing

We welcome contributions from developers, AI researchers, and anyone passionate about the future of software creation. Before submitting a Pull Request, please ensure your code passes our quality checks:

```bash
# Run clippy with all warnings treated as errors
cargo clippy --all-targets --all-features -- -D warnings

# Check code formatting (requires nightly toolchain)
cargo +nightly fmt --all --check

# Fix formatting automatically if needed
cargo +nightly fmt --all
```

For Windows builds, please see the [Windows build guide](docs/installation/windows.md) for OpenSSL setup instructions.

For detailed contribution guidelines, see [docs/development/contributing.md](docs/development/contributing.md).

---

## License

MIT License — see [LICENSE](LICENSE) for details.

Copyright (c) 2025-2026 Web3 Infrastructure Foundation.

Copyright (c) 2026 GitMono Limited.

---

<div align="center">

**[Get Started](https://docs.libra.tools/en/docs/getting-started) · [Join Discord](https://discord.gg/MTbb5rDYsC) · [Follow on X](https://x.com/git_mono_AI)**

</div>
