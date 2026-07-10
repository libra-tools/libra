# `libra fetch`

从另一个仓库下载对象并更新远程跟踪引用。

## 概要

```
libra fetch [OPTIONS] [<repository> [<refspec>]]
```

## 说明

`libra fetch` 联系远程仓库，协商本地存储缺少哪些对象，将它们作为 pack 文件下载，索引该 pack，并更新对应的远程跟踪引用（例如 `refs/remotes/origin/main`）。它永远不会修改工作树或当前分支；要进行这些操作，请使用 `libra pull` 或 `libra merge`。

不带参数调用时，它从当前分支配置的 upstream 获取。给出 `--all` 时，会依次获取每个已配置远程。指定某个 `<repository>` 时，只联系该远程。可选 `<refspec>` 会将 fetch 缩小到单个分支。

Fetch 支持 SSH、HTTPS、本地文件和 `git://` 传输。配置了 `vault.ssh.<remote>.privkey` 时，会自动加载 vault-backed SSH 密钥。

## 全局配置 Schema 保护

`libra fetch` 在信任远端 / tiered 对象存储设置前，会读取全局存储配置（`~/.libra/config.db`，或 `LIBRA_CONFIG_GLOBAL_DB` 指定的路径）。如果该数据库的 schema 版本比当前二进制支持的版本更新，fetch 会以 `LBR-CONFIG-001` fail-closed，而不是静默忽略全局存储配置并回退到本地对象。诊断会包含二进制路径和版本、配置 DB 路径、schema 版本，以及升级命令：
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`。

只有在明确希望本地对象访问时，才使用 `libra --offline fetch ...` 或 `LIBRA_READ_POLICY=offline|local libra fetch ...`。Libra 会告警一次，并在本次运行中忽略全局存储配置。

## 选项

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `<repository>` | 要从中 fetch 的远程名称或 URL。省略时使用当前分支的 upstream 远程。 | `libra fetch origin` |
| `<refspec>` | 要获取的分支名。需要 `<repository>`。省略时获取远程的所有分支。 | `libra fetch origin main` |
| `-a`, `--all` | 从每个已配置远程获取。与 `<repository>` 冲突。 | `libra fetch --all` |
| `--depth <N>` | 将获取限制为每个远程分支 tip 起的指定提交数量（shallow fetch）。仅支持能声明 shallow boundary 的 Git 远程；本地 Libra 远程在补齐 shallow 元数据前会以 `LBR-REPO-002` fail-closed。 | `libra fetch origin --depth 1` |
| `--json` | 向 stdout 输出结构化 JSON 信封（全局标志）。 | `libra --json fetch origin` |
| `--machine` | 紧凑单行 JSON；抑制进度（全局标志）。 | `libra --machine fetch origin` |
| `--progress none` | 在 JSON 模式下抑制 stderr 上的 NDJSON 进度事件。 | `libra --json fetch origin --progress none` |
| `--quiet` | 抑制人类可读输出。 | `libra fetch --quiet` |
| `--no-auto-gc` | fetch 后不运行 repack/gc。为对齐 Git 而接受的 no-op：Libra 的 fetch 从不触发自动 gc，故无可禁用。 | `libra fetch origin --no-auto-gc` |
| `--no-progress` | 不在 stderr 显示进度条（“Receiving objects” spinner / 远端进度），对齐 `git fetch --no-progress`。 | `libra fetch origin --no-progress` |
| `--no-prune` | 不修剪远端已不存在的 remote-tracking 引用。为对齐 Git 而接受的 no-op：Libra 的 fetch 从不修剪，故已是默认行为。（Git 的 `--prune`/`-p` 未公开。） | `libra fetch origin --no-prune` |

## 常用命令

```bash
libra fetch
libra fetch origin
libra fetch origin main
libra fetch --all
libra fetch origin --depth 1               # shallow fetch
libra fetch --all --depth 3                # 对所有远程进行 shallow fetch
libra --json fetch origin
libra --json fetch origin --progress none
```

## 浅 fetch 完整性

`--depth <N>` 只有在所选传输能返回 shallow boundary 元数据时才会执行。本地 Git 仓库和网络 Git 远程可以走该路径；本地 Libra 仓库当前不能声明 shallow boundary，因此 `libra fetch <本地 Libra 远程> --depth <N>` 会在下载对象或写入 `.libra/shallow` 前失败，并返回 `LBR-REPO-002`。该 fail-closed 行为避免 remote-tracking ref 指向缺父提交且没有 shallow 标记的对象闭包。

## 人类可读输出

成功的人类模式打印紧凑摘要：

```text
From /path/to/remote.git
 * [new ref]         origin/main
 32 objects fetched
```

没有变化时：

```text
From /path/to/remote.git
Already up to date with 'origin'
```

## 结构化输出（JSON 示例）

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- `stdout` 只保留给最终信封

### 顶层 Schema

- `all`：是否使用了 `--all`
- `requested_remote`：显式远程名称；`--all` 时为 `null`
- `refspec`：提供时为请求的分支/refspec
- `remotes[]`：每个远程的 fetch 结果

### 每个远程结果 Schema

- `remote`：逻辑远程名称
- `url`：规范化远程 URL/路径
- `refs_updated[]`：已更新的远程跟踪引用
- `objects_fetched`：从收到的 pack 解析出的对象数量
- `bytes_received`：收到的 pack 流字节大小（无传输时为 0）

### Refs Updated Schema

- `remote_ref`：全限定本地远程跟踪引用，例如 `refs/remotes/origin/main`
- `old_oid`：之前的对象 ID；引用为新建时为 `null`
- `new_oid`：获取到的对象 ID

示例（单个远程）：

```json
{
  "ok": true,
  "command": "fetch",
  "data": {
    "all": false,
    "requested_remote": "origin",
    "refspec": null,
    "remotes": [
      {
        "remote": "origin",
        "url": "git@github.com:user/repo.git",
        "refs_updated": [
          {
            "remote_ref": "refs/remotes/origin/main",
            "old_oid": "abc1234...",
            "new_oid": "def5678..."
          }
        ],
        "objects_fetched": 32,
        "bytes_received": 4096
      }
    ]
  }
}
```

示例（已经最新）：

```json
{
  "ok": true,
  "command": "fetch",
  "data": {
    "all": false,
    "requested_remote": "origin",
    "refspec": null,
    "remotes": [
      {
        "remote": "origin",
        "url": "git@github.com:user/repo.git",
        "refs_updated": [],
        "objects_fetched": 0,
        "bytes_received": 0
      }
    ]
  }
}
```

## 进度

- 在 `--json` 模式下，进度默认为 stderr 上的 NDJSON 事件
- 使用 `--progress none` 可在 JSON 模式下保持 `stderr` 安静
- `--machine` 会自动禁用进度，并在成功时保持 `stderr` 干净

## 设计理由

### 为什么默认没有 --prune？

Git 添加 `fetch.prune = true` 作为推荐默认值，因为陈旧的远程跟踪引用会静默累积。Libra 选择默认不 prune 有两个原因：（1）prune 需要额外往返来枚举远程当前引用，这会为每次 fetch 增加延迟；（2）在代理驱动工作流中，陈旧 tracking refs 可作为与之前远程状态做 diff 的有用历史锚点。需要 pruning 时，`libra remote prune <name>` 提供显式、可审计的操作。这让 `fetch` 保持快速且可预测，同时给用户一个有意的 pruning 路径。Git 的 `--no-prune`（不修剪）作为 no-op 接受，因为它已是 Libra 的默认行为；Git 的反向 `--prune`/`-p`（fetch 时修剪）未公开。

### Shallow fetch（`--depth`）作为稳定标志暴露

`libra fetch --depth N` 是公共稳定标志（已在 [`docs/development/commands/clone.md`](../../development/commands/clone.md) 中审计为 C3）。内部 `fetch_repository(..., depth)` plumbing 已支持 shallow fetch 一段时间；C3 将其暴露到 CLI，并绑定契约：

- `--depth N` 将获取限制为每个远程分支的最新 `N` 个提交。
- 它可与 `--all` 组合：跨所有已配置远程的 shallow fetch 是 `libra fetch --all --depth N`。
- 完整历史 fetch 后再执行 `fetch --depth N` 是幂等的。
- 对已经 shallow 的仓库以相同深度再次 fetch 也是幂等的：Libra 将服务器通告的 shallow 边界持久化在 `.libra/shallow` 中，并在后续 upload-pack 协商期间发送它们。
- Sparse checkout（`clone --sparse`）**不**属于此契约；见 [`docs/development/commands/_compatibility.md`](../../development/commands/_compatibility.md)，了解为什么有意延后 sparse-checkout。

Shallow fetch 会引入通常的 Git “shallow boundary” 注意事项（blame、log、merge-base 计算可能看不到边界之外的提交）。这个取舍是用户可见旋钮，而不是默认值；完整历史 fetch 仍是默认行为，也是 monorepo 和 AI 代理工作流的推荐姿态。对于确实需要完整历史的场景，分层云存储（S3/R2 + LRU caching）仍是带宽解决方案。

### 为什么 JSON 进度在 stderr 上？

结构化进度事件（对象数量、接收字节）作为 NDJSON 行发送到 stderr，以便代理框架解析实时进度，同时不干扰 stdout 上的最终结果信封。这遵循 Unix 将状态信息（stderr）与数据输出（stdout）分离的约定。`--progress none` 标志允许不需要进度的调用方完全抑制它，`--machine` 模式默认禁用进度，以最大化脚本友好性。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 获取 upstream | `libra fetch` | `git fetch` | `jj git fetch` |
| 具名远程 | `libra fetch origin` | `git fetch origin` | `jj git fetch --remote origin` |
| 单个分支 | `libra fetch origin main` | `git fetch origin main` | `jj git fetch --remote origin --branch main` |
| 所有远程 | `libra fetch --all` | `git fetch --all` | `jj git fetch --all-remotes` |
| Prune 陈旧引用 | `libra remote prune <name>` | `git fetch --prune` | 自动 |
| Shallow fetch | `libra fetch --depth N` | `git fetch --depth N` | 不支持 |
| 结构化输出 | `--json` / `--machine` | 无 | 无 |
| 进度事件 | stderr 上的 NDJSON | stderr 上的文本 | stderr 上的文本 |

## 错误处理

| 场景 | StableErrorCode | 退出码 | 提示 |
|----------|-----------------|------|------|
| 没有配置 upstream / detached HEAD | `LBR-REPO-003` | 128 | "checkout a branch or specify a remote" |
| 找不到远程 | `LBR-CLI-003` | 129 | "use 'libra remote -v' to see configured remotes" |
| 找不到远程分支 | `LBR-CLI-003` | 129 | "verify the remote branch name and try again" |
| 无效远程 spec（缺少 repo、URL 格式错误、不支持的 scheme） | `LBR-CLI-003` 或 `LBR-REPO-001` | 129 / 128 | 因原因而异 |
| 发现期间认证失败 | `LBR-AUTH-002` | 128 | "check SSH key / HTTP credentials and repository access rights" |
| 网络超时 / 传输失败 | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Packet / sideband / checksum / pack 协议失败 | `LBR-NET-002` | 128 | "the remote did not respond correctly" |
| 对象格式不匹配 | `LBR-REPO-003` | 128 | "remote uses a different hash algorithm" |
| 无法创建 pack 目录 | `LBR-IO-002` | 128 | "check filesystem permissions" |
| 无法写入 pack/index/refs | `LBR-IO-002` | 128 | "check filesystem permissions and disk space" |
| 本地状态损坏 | `LBR-REPO-002` | 128 | "inspect repository state and object integrity" |
