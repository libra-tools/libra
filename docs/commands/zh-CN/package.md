# `libra package`

安装、列出和 diff **capability packages** 的历史设计 — 这些包是可审计、checksum-verified 的 skills、slash-commands、Source Pool sources 和 sub-agent definitions bundle（CEX-S2-17，Step 2.7）。这是 Libra-only AI ecosystem 扩展，不是 Git 命令。

> 状态：未发布。`libra package` 未注册到当前版本的公开 CLI。运行它会返回标准 unknown-command 错误（`LBR-CLI-001`）。下面的接口描述的是保留的设计材料，不是用户可见命令契约。

## 概要

```
libra package list
libra package diff <path>
libra package install <path> [--yes] [--enable]
libra package uninstall <package-id>
```

## 说明

Capability package 是一个本地目录，包含 `manifest.json` 和 bundled content files。Manifest 声明 package id、version、publisher、覆盖 bundled content 的 SHA-256 `checksum`、`bundled` capabilities（skills / commands / sources / sub-agents）、`requested_permissions` 和 `install_warnings`。

未发布设计使用 `libra package` 作为这些 bundles 的 trust gate：

- **`list`** 打印每个已安装 package 及其 version 和 enabled state，读取 per-repo store `.libra/capability_packages.json`。
- **`diff <path>`** 加载本地 package 并预览它会授予的 capabilities（new skills / commands / sources / sub-agents / permissions），不安装。若 package bundle 了新的 *mutating* capability（source 或 sub-agent），会标记为需要确认。
- **`install <path>`** 验证 manifest，重新计算并校验 content checksum（被篡改或截断的 package 会被拒绝，且不记录任何内容），计算 capability diff，并记录 package。Install 是 **default-deny**：除非传入 `--enable`，否则 package 记录为 *disabled*；授予新的 mutating capability 的 package，或 content checksum 变化的 update，需要 `--yes` 接受。

记录 package 只会将其持久化到 store；bundled capabilities 会在 session startup 时从该 store 激活进 live session，永不在 install 时隐式激活。

## 选项

- `--yes` — 不经交互确认接受 capability diff（对授予新的 mutating capability 的 package，或 checksum 变化的 update 必需）。
- `--enable` — 立即启用 package，而不是保持 installed-but-disabled（default-deny）。

## 示例

```
# 列出已安装内容。
libra package list

# 在信任 package 前预览它会授予什么。
libra package diff ./my-package

# 审核并记录 package（打印 capability diff；默认拒绝启用）。
libra package install ./my-package

# 接受一个 bundle 了 mutating source/sub-agent 的 package，并启用它。
libra package install ./my-package --yes --enable

# 按 id 卸载已记录 package。
libra package uninstall acme.toolkit
```

`uninstall` 形式会从 per-repo store 删除 package；其 bundled capabilities（overlap-safe）会在下次 session start 时消失。

## 另见

- [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) — `package` 是 Libra-only 扩展（没有 Git 等价物）。
- `docs/development/tracing/agent.md` Step 2.7（CEX-S2-17）— capability-package / plugin-trust 设计。
