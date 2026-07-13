# `libra fetch`

从另一个仓库下载对象并更新远程跟踪引用。

## 概要

```
libra fetch [OPTIONS] [<repository> [<refspec>]]
```

## 说明

`libra fetch` 联系远程仓库，协商本地存储缺少哪些对象，将它们作为 pack 文件下载，索引该 pack，并更新对应的远程跟踪引用（例如 `refs/remotes/origin/main`）。它永远不会修改工作树或当前分支；要进行这些操作，请使用 `libra pull` 或 `libra merge`。

不带参数调用时，它从当前分支配置的 upstream 获取。给出 `--all` 时，会依次获取每个已配置远程。指定某个 `<repository>` 时，只联系该远程。可选 `<refspec>` 可精确指定源和目标：短分支名 `main` 等价于 `refs/heads/main:refs/remotes/<repository>/main`，完整 `<src>:<dst>` 只写 `<dst>`。未给 CLI refspec 时，Libra 读取全部 `remote.<name>.fetch` 值（包括 `remote add -t` / `remote set-branches` 写入的精确或通配映射）；只有没有配置值时才回退到默认 heads 与 merge-request 映射。

Fetch 支持 SSH、HTTPS、本地文件和 `git://` 传输。配置了 `vault.ssh.<remote>.privkey` 时，会自动加载 vault-backed SSH 密钥。

## 全局配置 Schema 保护

`libra fetch` 在信任远端 / tiered 对象存储设置前，会读取全局存储配置（`~/.libra/config.db`，或 `LIBRA_CONFIG_GLOBAL_DB` 指定的路径）。如果该数据库的 schema 版本比当前二进制支持的版本更新，fetch 会以 `LBR-CONFIG-001` fail-closed，而不是静默忽略全局存储配置并回退到本地对象。诊断会包含二进制路径和版本、配置 DB 路径、schema 版本，以及升级命令：
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`。

只有在明确希望本地对象访问时，才使用 `libra --offline fetch ...` 或 `LIBRA_READ_POLICY=offline|local libra fetch ...`。Libra 会告警一次，并在本次运行中忽略全局存储配置。

### 抓取相关的 config 默认值（`fetch.prune`、`remote.<name>.prune`）

未传 `--prune`/`--no-prune` 时，Libra 按严格的 local → global → system 级联读取 Git 兼容的修剪默认值：`fetch.prune=true|false` 让每次 fetch 之后默认修剪该远程已不再通告的远程跟踪引用；`remote.<name>.prune=true|false` 针对单个远程覆盖它（远程作用域的键优先，与 Git 一致）。命令行的 `--prune`/`--no-prune` 始终优先于配置。无效值会在联系远程、下载对象或写任何引用之前以 `LBR-CLI-002` fail-closed（带 `--all` 时，会先校验所有远程的修剪模式再开始第一个 fetch）；local/global 配置读取失败以 `LBR-IO-001` 失败。local/global 的加密值先解密再校验；不可读或不支持的 system scope 会被跳过（system 是级联的最后一个 scope，跳过即视该键在此 scope 未设置）。两个键都未设置时默认为 false（不修剪），与 Git 出厂默认一致。

## 选项

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `<repository>` | 要从中 fetch 的远程名称或 URL。省略时使用当前分支的 upstream 远程。 | `libra fetch origin` |
| `<refspec>` | 源引用或精确 `<src>:<dst>` 映射。需要 `<repository>`。目标支持 `refs/heads/*` 或 `refs/remotes/<repository>/*`；两侧各有一个 `*` 时支持通配。CLI refspec 覆盖 `remote.<name>.fetch` 配置。负 refspec 与标签目标仍不支持。 | `libra fetch origin refs/heads/topic:refs/remotes/origin/review` |
| `-a`, `--all` | 从每个已配置远程获取。与 `<repository>` 冲突。 | `libra fetch --all` |
| `--depth <N>` | 将获取限制为每个远程分支 tip 起的指定提交数量（shallow fetch）。支持能通告 shallow boundary 的 Git 远程；本地 Libra 远程在该传输能通告 shallow 元数据之前会以 `LBR-REPO-002` fail-closed。 | `libra fetch origin --depth 1` |
| `--tags` | 从远程获取每个标签到本地 `refs/tags/*`（覆盖默认的 auto-follow 和 `remote.<name>.tagOpt`）。 | `libra fetch origin --tags` |
| `--no-tags` | 完全不获取标签，连从已获取提交可达的标签也不获取（覆盖默认的 auto-follow）。 | `libra fetch origin --no-tags` |
| `--no-auto-gc` | fetch 后不运行 repack/gc。为对齐 Git 而接受的 no-op：Libra 的 fetch 从不触发自动 gc，故无可禁用。 | `libra fetch origin --no-auto-gc` |
| `--no-progress` | 不在 stderr 显示进度条（“Receiving objects” spinner / 远端进度），对齐 `git fetch --no-progress`。 | `libra fetch origin --no-progress` |
| `-p`, `--prune` | fetch 之后，删除该远程已不再以 `refs/heads/*` 或 `refs/mr/*` 通告其分支的 `refs/remotes/<remote>/*` 远程跟踪引用（复用 `remote prune` 的陈旧分类）。删除加一条审计 reflog 条目在同一个事务中执行。本地分支、标签、`refs/remotes/<remote>/HEAD` 和其他远程永远不会被触碰。带 `--dry-run` 时只报告陈旧引用而不删除。未传标志时，`fetch.prune` / `remote.<name>.prune` 配置可把修剪设为默认开启（见上文《抓取相关的 config 默认值》）；CLI 标志始终优先。 | `libra fetch origin -p` |
| `--no-prune` | 不修剪远程跟踪引用（默认）。`--prune`/`--no-prune` 构成 last-wins 切换：两者同时给出时，命令行最后一个生效（Git 语义）。显式 `--no-prune` 同时覆盖 `fetch.prune` / `remote.<name>.prune` 配置默认值。 | `libra fetch origin --no-prune` |
| `--notes` | 另外通过专用旁路通道从远程导入文件依赖图（`refs/notes/deps`，lore.md 3.2）。默认关闭（Git 从不自动 fetch notes）。v1 仅从**本地 Libra 源**传输 notes；网络或普通 Git 远程会发出诚实的 “not supported yet” 告警且不导入任何图（推迟，D17）。导入会与本地已有边做并集合并（union-merge）并重新校验每个端点，且按 note 容错（格式错误的 note、或其 commit 在本地缺失的 note，会带告警跳过，绝不中止 fetch）。用 `remote.<name>.fetchNotesDeps=true` 按远程持久化该 opt-in。 | `libra fetch origin --notes` |
| `-f`, `--force` | 允许非快进更新，并覆盖（clobber）指向别处的本地标签。强制更新在 `--porcelain` 中标记为 `+`，在人类输出中标记为 `(forced update)`。 | `libra fetch origin --tags --force` |
| `--dry-run` | 预览本次 fetch 将产生的远程跟踪引用更新，而不下载任何对象，也不写引用、reflog 或 `FETCH_HEAD`。 | `libra fetch origin --dry-run` |
| `--append` | 将获取到的引用记录追加到 `.libra/FETCH_HEAD`，而不是覆盖它。（`-a` 保留给 `--all`。） | `libra fetch origin --append` |
| `-v`, `--verbose` | 在 stderr 上宣告正在联系的远程；stdout 的结果契约不变。 | `libra fetch origin -v` |
| `--porcelain` | 对每个引用更新打印一行机器可读的 `<flag> <old-oid> <new-oid> <local-ref>`。与 `--json` 互斥。 | `libra fetch origin --porcelain` |
| `--json` | 向 stdout 输出结构化 JSON 信封（全局标志）。 | `libra --json fetch origin` |
| `--machine` | 紧凑单行 JSON；抑制进度（全局标志）。 | `libra --machine fetch origin` |
| `--progress none` | 在 JSON 模式下抑制 stderr 上的 NDJSON 进度事件。 | `libra --json fetch origin --progress none` |
| `--quiet` | 抑制人类可读输出。 | `libra fetch --quiet` |

## 常用命令

```bash
libra fetch
libra fetch origin
libra fetch origin main
libra fetch origin refs/heads/topic:refs/remotes/origin/review
libra fetch --all
libra fetch origin --depth 1               # shallow fetch
libra fetch origin --tags                  # 同时把所有标签取到 refs/tags/*
libra fetch --all --depth 3                # 对所有远程进行 shallow fetch
libra fetch origin --dry-run               # 预览引用更新，不写任何内容
libra fetch origin --porcelain             # 机器可读的按引用输出行
libra fetch origin -v                      # 在 stderr 上宣告远程
libra fetch origin --append                # 累积到 FETCH_HEAD
libra --json fetch origin
libra --json fetch origin --progress none
```

## 网络超时

网络 fetch（`http(s)://`、`git://`、`ssh://`）受以下超时约束，因此一个死掉或被黑洞的远程无法让命令永远挂起：

| 超时 | 默认值 | 约束什么 |
|---------|---------|----------------|
| connect | 30s | 打开连接时的 TCP（+ TLS）握手 |
| idle    | 60s | 引用通告或 pack 流传输期间没有字节到达的最长间隔（数据一到达即重置，因此慢而稳定的传输不会被切断） |
| first-byte | 30s | 从发送 `want` 列表到第一个响应字节（`NAK` / pack 头）的等待——比 idle 超时更早捕获接受了协商却从不开始流式传输的服务器。应用于 `git://`；`http(s)`/`ssh` 通过它们自己的读超时约束首个响应 |

每个超时按以下优先级顺序解析：

1. 以毫秒为单位的环境变量——`LIBRA_FETCH_CONNECT_TIMEOUT_MS`、
   `LIBRA_FETCH_IDLE_TIMEOUT_MS`、`LIBRA_FETCH_FIRST_BYTE_TIMEOUT_MS`；
2. 以整秒为单位的配置值——`fetch.<remote>.connectTimeout` /
   `fetch.<remote>.idleTimeout` / `fetch.<remote>.firstByteTimeout`，然后是
   不带作用域的 `fetch.connectTimeout` / `fetch.idleTimeout` / `fetch.firstByteTimeout`；
3. 上面的内置默认值。

```
# 只为这个远程，给不稳定的远程更长的连接时间。
libra config fetch.origin.connectTimeout 90

# 一次性覆盖（毫秒），不改配置。
LIBRA_FETCH_IDLE_TIMEOUT_MS=120000 libra fetch origin
```

本地（`file://` / 路径）远程从磁盘读取，不受网络超时约束。`git://` 连接现在受全部三个超时约束（此前它们没有任何超时）。无法解析的 env/config 值会被忽略而不是被应用，因此一个笔误永远不会让 fetch 带着为零或荒谬的超时运行。

## 浅 fetch 完整性

`--depth <N>` 只有在所选传输能返回 shallow boundary 元数据时才被接受。本地 Git 仓库和网络 Git 远程可以做到这一点；本地 Libra 仓库当前不能，因此 `libra fetch <本地 Libra 远程> --depth <N>` 会在下载对象或写入 `.libra/shallow` 之前失败，归类为 `LBR-REPO-002`。该 fail-closed 行为避免 remote-tracking ref 指向一个父提交缺失且没有 shallow 标记的提交。

## FETCH_HEAD

每次成功的 fetch 都会把每个已选择源引用记录在 `.libra/FETCH_HEAD` 中，即使对应目标已经是最新。精确 CLI refspec 是 merge candidate，因此中间字段为空；配置或通配 refspec 选中的引用以及标签使用 `not-for-merge`。`--append` 向该文件累积而不是覆盖；`--dry-run` 不写任何内容。fetch 永远不写 `.libra/ORIG_HEAD`。

选中的分支目标、fetch 到的标签、相应 reflog 和缓存的 `refs/remotes/<name>/HEAD` 在一个 SQLite 事务中提交。非快进拒绝或后续引用存储失败会回滚整组引用更新；已经下载的 pack 对象可能像 Git 一样作为不可达对象保留。

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
- `pruned[]`：修剪移除的陈旧远程跟踪引用（`{remote_ref, branch, old_oid}`）；仅在修剪至少移除一个引用时出现
- `bytes_received`：收到的 pack 流字节大小（无传输时为 0）

### Refs Updated Schema

- `remote_ref`：全限定本地远程跟踪引用，例如 `refs/remotes/origin/main`
- `old_oid`：之前的对象 ID；引用为新建时为 `null`
- `new_oid`：获取到的对象 ID
- `forced`：由前导 `+` refspec / `--force` 接受的非快进分支更新，或 `--force` 下被 clobber 的标签为 `true`；未强制的非快进会拒绝并原子回滚全部已选择引用更新

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
            "new_oid": "def5678...",
            "forced": false
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

### Pruning 是 opt-in，而非默认

Git 的出厂默认同样是 `fetch.prune = false`，只是开启它是一个常见的推荐设置，因为陈旧的远程跟踪引用会静默累积。Libra 保持同样的出厂默认——不修剪——另有两个原因：（1）在代理驱动工作流中，陈旧 tracking refs 可作为与之前远程状态做 diff 的有用历史锚点；（2）破坏性的引用清理应当是一个有意的选择。因此 pruning 通过 `--prune`/`-p` opt-in（或使用独立的 `libra remote prune <name>`）。`--no-prune` 是默认值；`--prune`/`--no-prune` 构成 last-wins 切换，与 Git 一致。

希望获得 Git 推荐姿态的仓库可以通过配置把修剪设为默认开启：`fetch.prune=true` 对每次 fetch 生效，`remote.<name>.prune=true|false` 按远程覆盖它。配置只提供默认值——命令行上的 `--prune`/`--no-prune` 始终优先。解析遵循上文《抓取相关的 config 默认值》所述的严格 local → global → system 级联与 fail-closed 语义（无效值在 fetch 之前以 `LBR-CLI-002` 失败）。两个键默认均为 false，与 Git 的出厂默认一致。

启用修剪（标志或配置）后，fetch 完成后 Libra 会移除远端已不再通告的每个 `refs/remotes/<remote>/*` 引用，按 `remote prune` 使用的同一规则分类。删除与一条非丢失（non-lossy）的审计 reflog 条目（`<old> -> 0…0`）在单个事务中执行，因此 prune 中途失败会回滚所有删除。`--dry-run` 只报告陈旧引用而不写。相对 Git 的已记录收窄：pruning 是**全远程作用域**的（它像 `remote prune` 一样清理该远程的每个陈旧 tracking ref，而不是局限于显式 refspec）；当远程完全没有通告任何引用时会**整体跳过**（因此一次瞬时的空通告不会清空所有 tracking ref）；被修剪的引用永远不会出现在 `FETCH_HEAD` 中（它只记录获取到的引用）。

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
| 修剪陈旧引用 | `libra fetch -p` / `fetch.prune`、`remote.<name>.prune` 配置 / `libra remote prune <name>` | `git fetch --prune` / 同名配置键 | 自动 |
| Shallow fetch | `libra fetch --depth N` | `git fetch --depth N` | 不支持 |
| Dry-run 预览 | `libra fetch --dry-run` | `git fetch --dry-run` | 不支持 |
| Porcelain 输出 | `libra fetch --porcelain` | `git fetch --porcelain` | 无 |
| 追加 FETCH_HEAD | `libra fetch --append` | `git fetch --append` | 无 |
| 详细诊断 | `libra fetch -v` | `git fetch -v` | 无 |
| 标签 auto-follow（默认） | 从已获取提交可达的标签会自动跟随（通过 `include-tag`） | 相同（默认） | 自动 |
| 标签获取控制 | `libra fetch --tags` / `--no-tags`；`remote.<name>.tagOpt` | `git fetch --tags` / `--no-tags`；`remote.<name>.tagOpt` | 自动 |
| 强制 fetch | `libra fetch -f` / `--force`（非 FF + 标签 clobber） | `git fetch --force` | 自动 |
| 原子引用更新 / refmap 标志 | 引用更新默认原子；未提供 `--atomic` 或 `--refmap` 标志 | `git fetch --atomic` / `--refmap` | 无 |
| 结构化输出 | `--json` / `--machine` | 无 | 无 |
| 进度事件 | stderr 上的 NDJSON | stderr 上的文本 | stderr 上的文本 |

## 错误处理

| 场景 | StableErrorCode | 退出码 | 提示 |
|----------|-----------------|------|------|
| 没有配置 upstream / detached HEAD | `LBR-REPO-003` | 128 | "checkout a branch or specify a remote" |
| 找不到远程 | `LBR-CLI-003` | 129 | "use 'libra remote -v' to see configured remotes" |
| 找不到远程分支 | `LBR-CLI-003` | 129 | "verify the remote branch name and try again" |
| fetch refspec 无效或暂不支持 | `LBR-CLI-002` | 129 | "use a branch name or `<src>:<dst>` mapping" |
| 无效远程 spec（缺少 repo、URL 格式错误、不支持的 scheme） | `LBR-CLI-003` 或 `LBR-REPO-001` | 129 / 128 | 因原因而异 |
| 发现期间认证失败 | `LBR-AUTH-002` | 128 | "check SSH key / HTTP credentials and repository access rights" |
| 网络超时 / 传输失败 | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| Packet / sideband / checksum / pack 协议失败 | `LBR-NET-002` | 128 | "the remote did not respond correctly" |
| 对象格式不匹配 | `LBR-REPO-003` | 128 | "remote uses a different hash algorithm" |
| 无法创建 pack 目录 | `LBR-IO-002` | 128 | "check filesystem permissions" |
| 无法写入 pack/index/refs | `LBR-IO-002` | 128 | "check filesystem permissions and disk space" |
| 本地状态损坏 | `LBR-REPO-002` | 128 | "inspect repository state and object integrity" |
