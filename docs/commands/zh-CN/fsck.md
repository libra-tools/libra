# `libra fsck`

通过检查对象、引用和索引来验证仓库完整性。

## 概要

```
libra fsck [OPTIONS] [OBJECT]
```

## 说明

`libra fsck` 验证 Libra 仓库中对象、引用和索引文件的完整性。它类似于 `git fsck`，是检测仓库损坏、断裂引用或数据不一致的主要诊断工具。

失败路径会遵守 `--json` 和 `--machine` 等全局结构化错误标志。例如，无效对象 ID 会在 stderr 上返回标准 Libra CLI 错误信封，而不是绕过 dispatcher。

该命令执行以下检查：

- **对象哈希完整性**：重新计算 SHA1/SHA256 哈希，并验证它与存储哈希匹配
- **对象格式有效性**：验证对象结构（blob、tree、commit、tag）
- **引用一致性**：验证所有引用都指向存在且有效的对象
- **索引完整性**：验证索引文件结构，并用对象存储交叉检查条目
- **可达性分析**：从 refs、reflogs 和 index 开始通过 BFS 检测 dangling 和 unreachable 对象
- **断链连通性**：确认每个提交的树与父提交、每个树条目都存在。缺失时按 Git 风格输出 `broken link from <type> <oid>` / `to <type> <oid>` 以及 `missing <type> <oid>`，并以非零码退出。`.libra/shallow` 中的提交视为根，不检查其父提交。损坏的 `.libra/shallow` 会 fail-closed（`LBR-REPO-002`）。带 obliteration tombstone 的缺失对象报告为 `intentionally absent`，不失败、不进入 `broken_links`。`--heal` 在该检查之前运行。`--json` 增加 `broken_links` 与 `missing_objects` 数组。

## 选项

### `[OBJECT]`

按 ID 检查单个对象。未提供时检查仓库中的所有对象。

```bash
libra fsck 2f24194cb3d41c1ac5b1f40c4c9331a2a40a76a7
```

### `-v, --verbose`

验证期间打印详细进度信息。

```bash
libra fsck --verbose
```

### `--no-reflogs`

跳过 reflog 验证。默认情况下，reflog 会作为可达性分析的起点。排除 reflog 可能导致更多对象被报告为 dangling。

```bash
libra fsck --no-reflogs
```

### `--unreachable`

报告所有 unreachable 对象，而不只是 dangling commits。

```bash
libra fsck --unreachable
```

### `--dangling`, `--no-dangling`

控制 dangling 对象报告。默认只报告 dangling commits（匹配 git fsck 行为）。

- `--dangling` 或 `--dangling=true`：报告 dangling commits
- `--no-dangling`：隐藏 dangling 对象报告

```bash
libra fsck --dangling          # 报告 dangling commits（默认）
libra fsck --no-dangling       # 隐藏 dangling 报告
```

### `--name-objects`

在 verbose 输出中显示对象的人类可读名称。名称收集自：
- Refs：`refs/heads/master`、`refs/tags/v1.0`
- Reflogs：`HEAD@{1778158193}`、`refs/heads/main@{1778158193}`
- Index：`:path/to/file.txt`

```bash
libra fsck --verbose --name-objects
```

### `--lost-found`

将 dangling/unreachable 对象写入 `.libra/lost-found/` 目录：
- `lost-found/commit/<hash>`：用于 commit 和 tree 对象（存储哈希）
- `lost-found/other/<hash>`：用于 blob 对象（存储实际内容）

该选项隐含 `--no-reflogs` 以进行 dangling 检测，匹配 `git fsck --lost-found` 行为。

```bash
libra fsck --lost-found
```

### `--root`

报告 root commits（没有父提交的提交）。

输出格式：`root <commit-hash>`

```bash
libra fsck --root
```

### `--tags`

报告带标签的提交。

输出格式：`tagged commit <commit-hash> (<tag-name>)`

```bash
libra fsck --tags
```

### `--connectivity-only`

只检查对象存在性，跳过内容验证。明显更快，但**不会**检测：
- 哈希不匹配（内容已损坏但对象存在）
- 格式错误（对象无法解析）

仍会检测 commit、tree 或 refs 引用的缺失对象，包括 Git 风格的断链报告。浅边界提交不要求其父提交存在。损坏的 `.libra/shallow` 仍会 fail-closed。

```bash
libra fsck --connectivity-only
```

### `--strict`

额外的格式与图检查（作为错误报告，会导致非零退出）：

- 提交的 author/committer 邮箱必须包含 `@`，时区必须是合法的 `±HHMM` 且在 ±1400 内；
- 提交的树与父提交必须存在且类型正确（存在性检查在不加 `--strict` 时也会做；`--strict` 额外检查类型）；
- 树条目必须存在、类型与 mode 匹配，并按 Git 规范排序。

`.libra/shallow` 中的浅边界提交在所有模式（含 `--strict`）下都不检查父提交。

```bash
libra fsck --strict
```

注意：这是 `git fsck --strict` 的有意收窄子集。`.gitmodules` / HFS+ / NTFS 路径检查以及按消息的 `fsck.<msg-id>` 严重级别配置尚未实现。

### `--full` / `--no-full`

校验 packfile 完整性。**默认开启**（与 Git 一致），用 `--no-full` 跳过。逐个校验 `.pack` 的尾部校验和与 `.idx` 的索引校验和，故损坏（含截断或 body 损坏的 pack）会被报告为错误并以非零码退出。该检查读取原始字节、**不解码** pack 对象，因此对损坏 pack 是报错而非崩溃。

```bash
libra fsck --full      # 默认行为，显式写出
libra fsck --no-full   # 跳过 packfile 校验
```

## 示例

```bash
# 完整完整性检查
libra fsck

# 带对象名称的 verbose 输出
libra fsck --verbose --name-objects

# 查找 dangling commits
libra fsck --dangling

# 将 dangling 对象写入 lost-found
libra fsck --lost-found

# 报告 root commits
libra fsck --root

# 报告带标签提交
libra fsck --tags

# 快速连接性检查
libra fsck --connectivity-only

# 检查单个对象
libra fsck abc123def456...
```

## 输出格式

### 诊断消息（stdout）

诊断消息打印到 stdout，且**不会**导致非零退出码：

```text
missing <type> <object-id>
hash mismatch <type> <object-id>
dangling <type> <object-id>
unreachable <type> <object-id>
```

### 错误消息（stderr）

错误消息打印到 stderr，并导致非零退出码：

```text
bad object sha1: <type> <object-id>
bad tree: <object-id>
unknown type: <type> <object-id>
missing author: <object-id>
missing committer: <object-id>
bad ref content: <ref-name>: invalid hash format
index corruption: <details>
```

### 干净仓库

无输出（静默成功）。

### 有 Dangling 对象

```text
dangling commit 8ae045f3b2c1d9e7f6a5b4c3d2e1f0a9b8c7d6e5
```

### 有缺失对象

```text
missing commit 6678874f0d5b658ae5c88b04020c64219f51f743
```

## 退出码

| 退出码 | 含义 |
| --------- | ------- |
| 0 | 所有检查通过，或只发现 dangling/unreachable 对象（信息性） |
| 1 | 发现错误：哈希不匹配、格式无效、对象缺失、引用断裂、索引损坏 |
| 1 | 致命错误：不是仓库、对象 ID 无效、数据库错误 |

**注意**：
- `dangling` 和 `unreachable` 仅为信息性，**不会**导致非零退出码。
- `missing`、`hash_mismatch` 和格式错误会导致退出码 1。

## 实现细节

### 检查阶段

fsck 命令按以下顺序执行检查：

1. **目录扫描**：枚举所有 loose objects 和 pack 文件
2. **对象验证**：验证每个对象的哈希完整性和格式
3. **HEAD 验证**：检查 HEAD 是否指向有效 ref
4. **Reflog 检查**：验证 reflog 条目引用的对象
5. **Ref 验证**：验证所有 refs 都指向有效对象
6. **索引验证**：检查索引文件结构和条目完整性
7. **连接性检查**：使用可选名称解析重新验证所有对象
8. **可达性分析**：通过 BFS 识别 dangling 和 unreachable 对象
9. **Root commit 报告**：（带 `--root`）列出没有父提交的提交
10. **Tag 报告**：（带 `--tags`）列出带标签提交

### 对象类型

Libra 支持与 Git 相同的对象类型：

- **blob**：文件内容
- **tree**：带 mode、name 和对象引用的目录列表
- **commit**：带 tree、parents、author、committer 的快照元数据
- **tag**：带必需 `object`、`type`、`tag` 和 `tagger` 头以及可选消息的附注标签。缺失或格式错误的 tag 头会使 fsck 以 tag 特定诊断失败，例如 `missing tagger`。

### 哈希算法

Libra 支持 SHA1 和 SHA256 哈希算法，由仓库配置决定。

### Reflog 行为

默认情况下，reflogs 中提到的对象被视为可达，不会被报告为 dangling。使用 `--no-reflogs` 可将 reflog 条目排除在可达性分析之外。
