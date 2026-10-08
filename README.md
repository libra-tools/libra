[中文](README.zh-CN.md) | English

![Libra](docs/image/libra-banner.png)

<div align="center">

# Libra — An AI-native Extended VCS Built for Agents

**Versioning the entire software creation lifecycle, not just code.**

</div>

Libra is an AI-native version control system that combines Git-compatible code history with capture of external AI coding sessions. It records supported agents' session and checkpoint metadata and, when transcript content is available, stores redacted snapshots in `refs/libra/traces`, so developers can inspect the context behind changes and carry that history with a repository.

Libra fits into existing developer workflows through Git-compatible objects and wire protocols. Its capture hooks support Claude Code, Codex, and OpenCode; those tools run the coding agents, while Libra records their work.

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
| **Versioned Artifacts** | Code history | Code history + captured agent checkpoints and redacted transcripts |
| **AI Collaboration** | Manual commit messages | External-agent session and checkpoint history |
| **Knowledge Reuse** | Code snapshots | Inspectable captured context alongside repository history |
| **Security** | External GPG/SSH setup | Built-in vault with per-repo key isolation |
| **Agent Integrations** | N/A | Capture hooks for Claude Code, Codex, and OpenCode |
| **Automation** | External CI/CD | Repository rules for cron and VCS events |

---

## Quick Start

### Install

```bash
# macOS (Apple Silicon) / Linux (amd64, arm64) — recommended
# Intel macOS has no prebuilt binary; build from source with `cargo build --release`.
# The installer verifies an Ed25519-signed release manifest and the binary's
# sha256/size before installing; see docs/installation.md for the trust model.
curl -fsSL https://download.libra.tools/install.sh | sh

# Homebrew (macOS)
brew install libra

# From source (requires Rust)
git clone https://github.com/wingwangsz/libra.git
cd libra
cargo build --release
```

The script installer also creates the optional shorthand
`~/.libra/bin/lba -> libra` as a relative symlink. Re-running the same version
repairs a missing alias without replacing the binary. Use `--no-alias` or
`LIBRA_NO_ALIAS=1` to opt out; an existing user-owned `lba` is never
overwritten. See [installer behavior and options](docs/installation.md).

#### Auto-upgrade (opt-in)

Official script installs can opt in to automatic upgrades:

```bash
libra config set --global upgrade.mode auto   # auto | manual | off (default off)
```

When `auto` is set, Libra checks a signed release manifest at
`https://download.libra.tools` alongside your normal commands and installs a
newer signed release in the background; the check is throttled, budget-limited,
and never affects your command's outcome. First-phase support is Linux
x86_64/aarch64 and macOS aarch64; Windows is published but auto-upgrade returns
`UnsupportedPlatform` and leaves the binary untouched. Third-party and manual
installs (Homebrew, from-source, package managers) never auto-upgrade and are
never marked official. Failed upgrades roll back to the previous version
automatically. The mode lives in `{LIBRA_HOME}/upgrade/settings.json`, not in
the SQLite config. See [auto-upgrade](docs/auto-upgrade.md).

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

### 🧠 External Agent Capture & Trace History

`libra agent` records sessions and checkpoints from supported external coding agents. Checkpoint metadata is indexed locally; when transcript content is available, redacted snapshots are stored as repository objects reachable from `refs/libra/traces`. Use `libra agent session`, `checkpoint`, and `doctor` to inspect captured state, and `libra agent push` to share traces with a remote.

The built-in `libra code` UI and runtime, its MCP `--stdio` entry point, and the `libra publish` site host were removed in the 0.23.x release line. External-agent capture continues through `libra agent`; repository backup uses `libra cloud`.

```
.libra/
├── libra.db              # SQLite: VCS state + agent capture metadata
├── vault.db              # Encrypted signing keys and credentials
├── objects/              # Object store (loose + pack, compatible with Git)
└── sessions/agent/       # Local agent capture event logs
```

### 🔄 Git-Compatible Foundation

Libra speaks Git's language. On-disk formats (objects, index, pack, pack-index) and wire protocols are fully compatible with standard Git servers (GitHub, GitLab, Gitea, etc.). You can `push` and `pull` to any Git remote with zero friction.

Key difference: Git manages files. Libra manages **creation**.

### 🔐 Vault-Backed Security

By default, `libra init` creates a per-repository vault for encrypted key management:
- **GPG signing keys** for commit verification
- **SSH keys** for remote authentication

Signing and authentication keys are isolated per repository.

### 🛡️ Sandbox Diagnostics

`libra sandbox status` reports the backend and enforcement available for Libra operations that request its internal sandbox. External coding agents keep their own execution and safety policies.

### ☁️ Tiered Cloud Storage & Backup

- **Tiered storage**: Offload large objects to S3/R2/RustFS with local LRU caching
- **Cloud backup**: Sync your entire repository state (including AI history) to Cloudflare D1 + R2
- **Portable**: Move a Libra repository between machines with all AI context intact

---

## Supported External Agents

Libra supports hook-based capture for Claude Code, Codex, and OpenCode. The built-in model-provider runner was removed with `libra code` in 0.23.x.

> See the [agent command guide](docs/commands/agent.md) for capture setup and supported agents.

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

For Windows installation, see the [Windows installation instructions](docs/installation.md#windows).

For detailed contribution guidelines, see [docs/contributing.md](docs/contributing.md).

---

## License

MIT License — see [LICENSE](LICENSE) for details.

Copyright (c) 2025-2026 Web3 Infrastructure Foundation.

Copyright (c) 2026 GitMono Limited.

---

<div align="center">

**[Get Started](https://docs.libra.tools/en/docs/getting-started) · [Join Discord](https://discord.gg/MTbb5rDYsC) · [Follow on X](https://x.com/git_mono_AI)**

</div>
