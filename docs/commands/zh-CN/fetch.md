# `libra fetch`

从另一个仓库下载对象并更新远程跟踪引用。

## 概要

```
libra fetch [OPTIONS] [<repository> [<refspec>]]
```

## 说明

`libra fetch` 联系远程仓库，协商本地存储缺少哪些对象，将它们作为 pack 文件下载，索引该 pack，并更新对应的远程跟踪引用（例如 `refs/remotes/origin/main`）。它永远不会修改工作树或当前分支；要进行这些操作，请使用 `libra pull` 或 `libra merge`。

不带参数调用时，它从当前分支配置的 upstream 获取。已配置的本地 upstream（`branch.<name>.remote=.`）会在任何网络或 `FETCH_HEAD` 写入前被拒绝（`LBR-CLI-003`，退出 129）；Git 2.54 的 `fetch` 可以对本地 upstream 操作——该支持延后到 [issues/480 HP-16](https://github.com/libra-tools/libra/issues/480)。显式仓库参数 `.` 仍走现有的 `remote '.' not found`。给出 `--all` 时，会依次获取每个已配置远程。指定某个 `<repository>` 时，只联系该远程。可选 `<refspec>` 选择一个源引用，并可用 `<src>:<dst>` 精确映射到本地目标。未显式给出 refspec 时会遵守 `remote.<name>.fetch`；该配置不存在时才回退为把所有远程分支映射到 `refs/remotes/<name>/*`。

Fetch 支持 SSH、HTTPS、本地文件、Git v2 bundle 文件和 `git://` 传输。远程 URL 指向 bundle 时，每次 fetch（含 `--prune` 与 `--dry-run`）都会重新读取该文件。当 `remote.<name>.fetch` 为 `+refs/*:refs/*`（`--mirror` 克隆）时，fetch 会原样更新这些 ref，`--prune` 会删除源上已不存在的镜像 ref。配置了 `vault.ssh.<remote>.privkey` 时，会自动加载 vault-backed SSH 密钥。

## 全局配置 Schema 保护

配置 schema 兼容性按角色判定。`libra fetch` 在信任配置前，以只读方式检查 GlobalConfig 与 SystemConfig 元数据。真正的配置 future schema，或未注册／名称不匹配的迁移 receipt，在命令需要该作用域时以 `LBR-CONFIG-001` fail-closed。当前 manifest 已知的 Repository-only receipt（包括 `2026090801`）不会使配置库被误判为 future，受支持的配置值仍可读取。本 build 能识别 configuration-owned legacy-reader barrier；详见[配置兼容性](config.md#配置-schema-兼容性)。

全局路径为 `LIBRA_CONFIG_GLOBAL_DB` 或 XDG 配置目录（`$XDG_CONFIG_HOME/libra/config.db`，默认 `<home>/.config/libra/config.db`），在自动迁移前回退到 legacy `<home>/.libra/config.db`；系统路径为 `LIBRA_CONFIG_SYSTEM_DB` 或 `/etc/libra/config.db`。完整的进程环境／repo-local 存储设置可以证明无需 GlobalConfig（`cloud` 还须满足 D1 设置），但不能证明无需 SystemConfig 默认值。诊断只说明受影响的 scope、ledger 与版本，不输出配置值或未信任 receipt 名称。

本阶段对未知／不支持的状态只有升级路径，不执行自动修复。安装兼容的较新 Libra：
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`。
禁止手工删除或修改 SQLite receipt。仅在明确需要本地对象访问时使用 `--offline` 或 `LIBRA_READ_POLICY=offline|local`；这些模式会告警，并不授权远端同步。

### 抓取相关的 config 默认值（`fetch.prune`、`remote.<name>.prune`）

未传 `--prune`/`--no-prune` 时，Libra 按严格的 local → global → system 级联读取 Git 兼容的修剪默认值：`fetch.prune=true|false` 让每次 fetch 之后默认修剪该远程已不再通告的远程跟踪引用；`remote.<name>.prune=true|false` 针对单个远程覆盖它（远程作用域的键优先，与 Git 一致）。命令行的 `--prune`/`--no-prune` 始终优先于配置。无效值会在联系远程、下载对象或写任何引用之前以 `LBR-CLI-002` fail-closed（带 `--all` 时，会先校验所有远程的修剪模式再开始第一个 fetch）；local/global 配置读取失败以 `LBR-IO-001` 失败。local/global 的加密值先解密再校验；不可读的 system scope 保持跳过行为。只有 dispatch 已证明无需 Global 存储配置时，不支持的 global schema 才可在一次性去重警告后被默认值读取跳过；需要该 Global scope，或 System 含 future／未注册 receipt 时，dispatch 以 `LBR-CONFIG-001` 阻断 fetch，Global 凭据覆盖不豁免 System。已知 Repository receipt 与合法 barrier 保持可读。两个键都未设置时默认为 false（不修剪），与 Git 出厂默认一致。

### Fetch refspec

`main` 这样的短源名称表示 `refs/heads/main`，默认目标为 `refs/remotes/<remote>/main`。同时支持完整映射与每侧一个 `*` 的常见通配形式：

```bash
libra fetch origin refs/heads/main:refs/remotes/origin/release
libra config set --add remote.origin.fetch \
  +refs/heads/*:refs/remotes/origin/*
```

显式 refspec 覆盖配置映射。形如 `fetch origin dev` 的裸源仍会下载该引用并写入 `FETCH_HEAD`；若已配置的 `remote.<name>.fetch` 并不映射 `dev`，则不会新建远程跟踪分支（对齐单分支克隆；issues/474 CL-05）。`remote add -t` 与 `remote set-branches` 写入的具体 `remote.<name>.fetch` 会被后续 fetch 严格执行；配置变量名大小写不敏感，因此 `remote.origin.Fetch` 之类的拼写也会生效。目标目前仅限 `refs/heads/*` 与 `refs/remotes/<remote>/*`（保留 `HEAD` 目标会被拒绝）；`+refs/*:refs/*` 的 mirror refspec 还会映射其它合法的 `refs/*` 名称。多个目标引用、对应 reflog 与 `refs/remotes/<name>/HEAD` 在同一个 SQLite 事务中提交；任何目标被拒绝都会回滚整批引用更新。非快进需要映射前导 `+` 或 `--force`；写入任一 linked worktree 正在 checkout 的本地分支会被拒绝（`core.bare=true` 的裸仓库跳过该检查）。完整 fetch 的有效映射不再包含远端默认 source 分支时，会删除失效的缓存 remote HEAD。标签目标继续由 `--tags` / `--no-tags` 管理，不通过 fetch refspec 写入。

## 选项

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `<repository>` | 要从中 fetch 的远程名称或 URL。省略时使用当前分支的 upstream 远程。 | `libra fetch origin` |
| `<refspec>` | 源引用或精确 `<src>:<dst>` 映射。需要 `<repository>`。省略时使用 `remote.<name>.fetch`，再回退为所有远程分支。 | `libra fetch origin refs/heads/main:refs/remotes/origin/release` |
| `-a`, `--all` | 从每个已配置远程获取。与 `<repository>` 冲突。 | `libra fetch --all` |
| `--depth <N>` | 将获取限制为每个远程分支 tip 起的指定提交数量（shallow fetch）。网络 Git（`git://`）、HTTP(S) 和 SSH 服务端须通告 `shallow` 能力；进程内本地 Git 路径无需能力通告也支持 `--depth`。本地 Libra 远程以 `LBR-REPO-002` fail-closed（已决终态，D20）。 | `libra fetch origin --depth 1` |
| `--unshallow` | 将浅仓库转换为完整仓库：抓取完整历史并删除浅边界记录。没有浅历史的仓库会报错，本地 Libra 源被拒绝（D20）。 | `libra fetch --unshallow origin main` |
| `--negotiation-tip <commit>` | 把协商 `have` 集限制为从给定提交或 ref 可达的提交（可重复）。本地传输同样生效（以限定集计算可达对象差集）。缺失/无法解析的 tip 会报错。 | `libra fetch --negotiation-tip <oid> origin main` |
| `--tags` | 从远程获取每个标签到本地 `refs/tags/*`（覆盖默认的 auto-follow 和 `remote.<name>.tagOpt`）。 | `libra fetch origin --tags` |
| `--no-tags` | 完全不获取标签，连从已获取提交可达的标签也不获取（覆盖默认的 auto-follow）。 | `libra fetch origin --no-tags` |
| `--no-auto-gc` | fetch 后不运行 repack/gc。为对齐 Git 而接受的 no-op：Libra 的 fetch 从不触发自动 gc，故无可禁用。 | `libra fetch origin --no-auto-gc` |
| `--no-progress` | 不在 stderr 显示进度条（“Receiving objects” spinner / 远端进度），对齐 `git fetch --no-progress`。 | `libra fetch origin --no-progress` |
| `-p`, `--prune` | fetch 之后，删除不再是有效配置 refspec 映射目标的 `refs/remotes/<remote>/*` 远程跟踪引用；在 `--mirror` 远程（`+refs/*:refs/*`）上则删除源已不再通告的镜像 ref（锁定短名如 `main` 会跳过）。一次性显式 refspec 会保留当前配置映射的 destination、普通全远程范围以及本次选中目标。删除加一条审计 reflog 条目在同一个事务中执行。非 mirror fetch 下本地分支、标签、`refs/remotes/<remote>/HEAD` 和其他远程永远不会被触碰。带 `--dry-run` 时只报告陈旧引用而不删除。未传标志时，`fetch.prune` / `remote.<name>.prune` 配置可把修剪设为默认开启（见上文《抓取相关的 config 默认值》）；CLI 标志始终优先。 | `libra fetch origin -p` |
| `-P`, `--prune-tags` | 删除远程已不再通告的本地标签。仅在与 `--prune`（或 `--mirror`）同时启用时生效，给出显式 refspec 时被忽略；单独 `--prune-tags` 不修剪（Git parity）。未传标志时遵循 `remote.<name>.pruneTags` / `fetch.pruneTags` 配置默认。 | `libra fetch origin --prune --prune-tags` |
| `--atomic` | 原子更新所有抓取的 ref：任意被拒绝（非快进）的更新会使所有 ref、reflog 与 `FETCH_HEAD` 写入回滚。Libra 的 fetch 本就在单个事务中更新 ref，因此为 Git parity 接受该参数并断言全有或全无行为。 | `libra fetch --atomic origin` |
| `--no-prune` | 不修剪远程跟踪引用（默认）。`--prune`/`--no-prune` 构成 last-wins 切换：两者同时给出时，命令行最后一个生效（Git 语义）。显式 `--no-prune` 同时覆盖 `fetch.prune` / `remote.<name>.prune` 配置默认值。 | `libra fetch origin --no-prune` |
| `--notes` | 另外通过专用旁路通道从远程导入文件依赖图（`refs/notes/deps`，lore.md 3.2）。默认关闭（Git 从不自动 fetch notes）。v1 仅从**本地 Libra 源**传输 notes；网络或普通 Git 远程会发出诚实的 “not supported yet” 告警且不导入任何图（推迟，D17）。导入会与本地已有边做并集合并（union-merge）并重新校验每个端点，且按 note 容错（格式错误的 note、或其 commit 在本地缺失的 note，会带告警跳过，绝不中止 fetch）。用 `remote.<name>.fetchNotesDeps=true` 按远程持久化该 opt-in。 | `libra fetch origin --notes` |
| `-f`, `--force` | 允许非快进更新，并覆盖（clobber）指向别处的本地标签。强制更新在 `--porcelain` 中标记为 `+`，在人类输出中标记为 `(forced update)`。 | `libra fetch origin --tags --force` |
| `--dry-run` | 预览本次 fetch 将产生的远程跟踪引用更新，而不下载任何对象，也不写引用、reflog 或 `FETCH_HEAD`。 | `libra fetch origin --dry-run` |
| `--append` | 将获取到的引用记录追加到 `.libra/FETCH_HEAD`，而不是覆盖它。（`-a` 保留给 `--all`。） | `libra fetch origin --append` |
| `--set-upstream` | 成功从命名远端单分支 fetch 后，把当前分支的 upstream 记录为 `branch.<name>.remote` / `branch.<name>.merge`。带冒号的 refspec（`src:dst`）或不带分支参数时不写入（冒号形式 Git 会告警）。 | `libra fetch --set-upstream origin main` |
| `--update-head-ok` | 允许显式 refspec 更新当前检出的分支（非快进时需 `+`）。不带它时，fetch 到检出分支会被拒绝。 | `libra fetch --update-head-ok origin master:master` |
| `--refmap=<spec>` | 用给定映射替换用于推导命令行 refspec 跟踪目标的 `remote.<name>.fetch`。空值（`--refmap=`）不更新任何跟踪 ref（只写 FETCH_HEAD）。要求有命令行 refspec。 | `libra fetch --refmap= origin main` |
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
libra fetch origin refs/heads/main:refs/remotes/origin/release
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

`--depth <N>` 只有在所选传输能返回 shallow boundary 元数据时才被接受。本地 Git 仓库和网络 Git 远程可以做到这一点。本地 Git 远程与 clone 使用同一套最短距离并集：父提交未被发送，或根提交恰好落在深度截止上时，该提交是 shallow 边界（issues/474 CL-04）。

通过 `git://`、HTTP(S) 或 SSH 访问的 Git 服务端即使在未指定 `--depth` 时，也可能通告已有的 `shallow <oid>` 边界。只有该提交已在本地、且其父提交缺失时，fetch 才将通告的边界写入 `.libra/shallow`。Git 协议客户端仅在服务端通告 `shallow` 能力时请求该能力；一次通告最多接受 4,096 个不同的边界。upload-pack 响应中的 `shallow` 与 `unshallow` 行合计去重后，最多接受 4,096 个不同的对象 ID。这些响应行（含重复）总数最多为 8,192，每个 ID 均按服务端对象格式校验；超限或格式错误返回 `LBR-NET-002`。检查通告的边界提交时，每个提交的解码后对象 payload 最多 4 MiB，单次 fetch 累计读取的解码后提交 payload 最多 64 MiB，累计最多处理 262,144 个父提交 ID。超限会中止 fetch；累计上限错误会建议减少获取的引用，或请远端所有者减少浅边界。

网络 Git（`git://`）、HTTP(S) 和 SSH fetch 会在更新引用前，对照最终浅边界校验目标对象与新获取提交的父边；未标记的缺失父提交会使 fetch 失败。校验使用的临时父边文件每次最多 1 GiB，本次 pack 的提交 ID 临时缓冲最多 64 MiB（目前最多约 2,097,152 个提交）。有效但很大的 pack 也可能触及这两项资源上限；fetch 会在更新引用前报错，并不代表 pack 损坏。若深度响应的浅边界还需进一步校验，最多检查 16,384 个请求对象或标签目标。浅边界祖先遍历另分别最多访问 262,144 个不同提交和 262,144 条父边。远端对象类型探测与读取的标签数据共用每次浅响应校验 256 MiB 的解码后对象 payload 额度。任一项超限时，可分次获取或减少选取的引用。

智能 HTTP 还会在 upload-pack POST 后重新获取通告；若边界已变化，则在写入 pack 或引用之前报告 `NetworkProtocol` 错误并提示重试。

本地 Libra 仓库不能（维持 fail-closed 为已决终态，D20），因此 `libra fetch <本地 Libra 远程> --depth <N>` 会在下载对象或写入 `.libra/shallow` 之前失败，归类为 `LBR-REPO-002`。该 fail-closed 行为避免 remote-tracking ref 指向一个父提交缺失且没有 shallow 标记的提交。

## FETCH_HEAD

每次成功的 fetch 都会把获取到的引用记录在 `.libra/FETCH_HEAD` 中，每个分支一行 `<oid>\tnot-for-merge\tbranch '<name>' of <url>`，每个获取到的标签一行 `<oid>\tnot-for-merge\ttag '<name>' of <url>`。Libra 从不指定合并目标（要合并请使用 `libra pull`），因此每一行都标记为 `not-for-merge`。`--append` 向该文件累积而不是覆盖它；`--dry-run` 不写任何内容。即使本地目标已经最新，所选源引用仍会记录；普通 fetch 不创建或修改 `ORIG_HEAD`。

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
- `refs_updated[]`：发生变化的本地目标引用
- `objects_fetched`：从收到的 pack 解析出的对象数量
- `pruned[]`：修剪移除的陈旧远程跟踪引用（`{remote_ref, branch, old_oid}`）；仅在修剪至少移除一个引用时出现
- `bytes_received`：收到的 pack 流字节大小（无传输时为 0）

### Refs Updated Schema

- `remote_ref`：全限定本地目标引用，例如 `refs/remotes/origin/main`
- `old_oid`：之前的对象 ID；引用为新建时为 `null`
- `new_oid`：获取到的对象 ID
- `forced`：非快进更新由 refspec 前导 `+` 或 `--force` 放行时为 `true`；`--force` 下 clobber 标签时也为 `true`

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

启用修剪（标志或配置）后，fetch 完成后 Libra 会移除不再是有效配置 refspec 存活 destination 的每个 `refs/remotes/<remote>/*` 引用，`remote prune` 使用相同的 destination-aware 规则；一次性显式 refspec 会保留当前配置映射的 destination、普通的全远程通告范围以及本次选中 destination。删除与一条非丢失（non-lossy）的审计 reflog 条目（`<old> -> 0…0`）在单个事务中执行，因此 prune 中途失败会回滚所有删除。`--dry-run` 只报告陈旧引用而不写。当远程完全没有通告任何引用时会**整体跳过**（因此一次瞬时的空通告不会清空所有 tracking ref）；被修剪的引用永远不会出现在 `FETCH_HEAD` 中（它只记录获取到的引用）。

### Shallow fetch（`--depth`）作为稳定标志暴露

`libra fetch --depth N` 是公共稳定标志（已在 [`docs/development/commands/clone.md`](../../development/commands/clone.md) 中审计为 C3）。内部 `fetch_repository(..., depth)` plumbing 已支持 shallow fetch 一段时间；C3 将其暴露到 CLI，并绑定契约：

- `--depth N` 将获取限制为每个远程分支的最新 `N` 个提交。
- 它可与 `--all` 组合：跨所有已配置远程的 shallow fetch 是 `libra fetch --all --depth N`。
- `fetch --depth N` 可以给原本完整的仓库增加浅边界。远端引用不变时，以相同深度重复 fetch 是幂等的：Libra 将服务器通告的浅边界持久化在 `.libra/shallow` 中，并在后续 upload-pack 协商期间发送它们。
- Sparse checkout（`clone --sparse`）**不**属于此契约；见 [`docs/development/commands/_compatibility.md`](../../development/commands/_compatibility.md)，了解为什么有意延后 sparse-checkout。

Shallow fetch 会引入通常的 Git “shallow boundary” 注意事项（blame、log、merge-base 计算可能看不到边界之外的提交）。用户可用 `--depth` 请求额外的深度限制；不带此选项时，fetch 会请求源中可用的全部历史，但源本身可能已浅。完整历史仍是 monorepo 和 AI 代理工作流的推荐姿态。对于确实需要完整历史的场景，分层云存储（S3/R2 + LRU caching）仍是带宽解决方案。

### 为什么 JSON 进度在 stderr 上？

结构化进度事件（对象数量、接收字节）作为 NDJSON 行发送到 stderr，以便代理框架解析实时进度，同时不干扰 stdout 上的最终结果信封。这遵循 Unix 将状态信息（stderr）与数据输出（stdout）分离的约定。`--progress none` 标志允许不需要进度的调用方完全抑制它，`--machine` 模式默认禁用进度，以最大化脚本友好性。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|-----------|-------|-----|----|
| 获取 upstream | `libra fetch` | `git fetch` | `jj git fetch` |
| 具名远程 | `libra fetch origin` | `git fetch origin` | `jj git fetch --remote origin` |
| 单个分支 | `libra fetch origin main` | `git fetch origin main` | `jj git fetch --remote origin --branch main` |
| 精确引用映射 | `libra fetch origin <src>:<dst>` | `git fetch origin <src>:<dst>` | 不支持 |
| 配置映射 | `remote.<name>.fetch`（每侧可有一个 `*`） | 相同 | 不支持 |
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
| Atomic / refmap | 不支持（推迟） | `git fetch --atomic` / `--refmap` | 无 |
| 结构化输出 | `--json` / `--machine` | 无 | 无 |
| 进度事件 | stderr 上的 NDJSON | stderr 上的文本 | stderr 上的文本 |

## 错误处理

| 场景 | StableErrorCode | 退出码 | 提示 |
|----------|-----------------|------|------|
| 没有配置 upstream / detached HEAD | `LBR-REPO-003` | 128 | "checkout a branch or specify a remote" |
| 找不到远程 | `LBR-CLI-003` | 129 | "use 'libra remote -v' to see configured remotes" |
| 已配置本地 upstream（`branch.<name>.remote=.`） | `LBR-CLI-003` | 129 | 用 `libra branch --unset-upstream` 清除；网络支持见 issues/480 HP-16 |
| 找不到远程分支 | `LBR-CLI-003` | 129 | "verify the remote branch name and try again" |
| 无效或通配不匹配的 fetch refspec | `LBR-CLI-002` | 129 | 使用有效的 `<src>:<dst>` 与成对可选通配符 |
| 读取配置 refspec 失败 | `LBR-IO-001` | 128 | 检查 `remote.<name>.fetch` 配置 |
| 当前 checkout 目标 / 未放行的非快进 | `LBR-CONFLICT-002` | 128 | 修改目标，或有意添加 `+` / `--force` |
| 无效远程 spec（缺少 repo、URL 格式错误、不支持的 scheme） | `LBR-CLI-003` 或 `LBR-REPO-001` | 129 / 128 | 因原因而异 |
| 发现期间认证失败 | `LBR-AUTH-002` | 128 | "check SSH key / HTTP credentials and repository access rights" |
| 网络超时 / 传输失败 | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| pkt-line discovery / 传输建立错误 / 广告为空 | `LBR-NET-002` | 128 | "check that the remote serves Git data and that a proxy has not altered the response" |
| 封包读取连接重置 / 非协议 IO 错误 | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| pkt-line 截断 / sideband / checksum / pack 协议失败 | `LBR-NET-002` | 128 | 无额外提示；不完整的 pack 除外："the connection dropped mid-transfer — retry the fetch" |
| 对象格式不匹配 | `LBR-REPO-003` | 128 | "remote uses a different hash algorithm" |
| 无法创建 pack 目录 | `LBR-IO-002` | 128 | "check filesystem permissions" |
| 无法写入 pack/index/refs | `LBR-IO-002` | 128 | "check filesystem permissions and disk space" |
| 本地状态损坏 | `LBR-REPO-002` | 128 | "inspect repository state and object integrity" |

## Fetch 期间的封包截断

收到的 pkt-line 标头或 payload 在帧中途遇到 EOF 截断时，fetch 返回 `LBR-NET-002`。
截断错误只包含固定原因，不回显远端字节。长度一至三在分配 payload 前被拒绝；
flush（`0000`）、空数据帧（`0004`）及最大长度帧（`ffff`）仍然有效。
标头解码错误也只包含固定原因，不回显收到的字节。

如果 pack 尚未完整而传输在封包中途结束，错误报告封包截断，不含已接收
字节数或额外 CLI 提示。如果传输在封包边界结束而 pack 仍不完整，错误仍报告
字节数，并附带 "the connection dropped mid-transfer — retry the fetch" 提示。

完整且校验通过的 pack 仍可在没有 flush、连接尚未关闭时完成。Fetch 也会检查
完成时已可读取的尾部封包字节；这些字节中的不完整帧会返回错误。
请检查连接或代理是否截断了响应，然后重试 fetch。
对网络远端，如果尾部帧已经开始但随后停止传输，仍适用现有空闲超时；超时会使 fetch
以 `LBR-NET-001` 失败。

## 畸形 HTTP(S) discovery 响应

在 HTTP(S) 引用发现（discovery）期间，Libra 会拒绝零字节广告和畸形
pkt-line 帧，包括不完整或非十六进制标头、小于四的帧长度以及截断的 payload。
合法的 `0000` flush 与未收到响应有明确区别；合法的空仓库广告仍受支持。
不支持的 object-format capability 使用固定错误消息
`Unsupported object format capability`，不回显远端提供的值。
请确认 URL 指向 Git smart HTTP 服务，并检查代理是否截断或替换了响应，然后重试。

fetch discovery 对空广告或畸形 pkt-line 响应返回 `LBR-NET-002`，不回显标头或
payload 字节。普通网络故障仍返回 `LBR-NET-001`；遇到协议错误时，请先检查 Git
服务及代理返回的响应，再重试。

## pkt-line 错误归类

检测到的 pkt-line 帧格式错误返回 `LBR-NET-002`（退出码128），包括空的 HTTP(S)
discovery 广告。普通连接失败、连接重置和超时返回 `LBR-NET-001`（退出码128）。
协议错误发生时请核对 Git 服务及代理响应。discovery 帧错误的提示为
`check that the remote serves Git data and that a proxy has not altered the response`。

对象传输建立阶段检测到的 pkt-line 错误使用相同协议提示。读取 fetch 流时的标头或
payload 截断保留 `LBR-NET-002`，不附加 CLI 提示。在完整帧边界结束但 pack 尚未完整时，
仍保留字节数及 `the connection dropped mid-transfer — retry the fetch` 提示。
读取封包时的普通连接重置返回 `LBR-NET-001`。

在 pack 数据开始前，upload-pack 流于帧边界遇到 EOF（包括零字节 POST 响应）
返回 `LBR-NET-001`，提示为 `check network connectivity and retry`。
空的 discovery 广告仍返回 `LBR-NET-002`。

## Git 与 SSH 广告帧边界

Git/SSH pkt-line 广告读取器拒绝声明长度 `0001` 至 `0003`、不完整的四字节标头，
以及声明 payload 内的截断 EOF。flush `0000`、空 payload `0004` 和最大 `ffff`
帧保持既有行为。读取边界的 typed pkt-line 错误经分类得到 `LBR-NET-002`；
普通传输 IO 和 idle 超时经分类仍为 `LBR-NET-001`。

在 `git://` 取对象前的 advertisement 阶段，fetch、clone 和 pull 已将这些错误
报告为 `LBR-NET-002`，包括零字节广告；提示为
`check that the remote serves Git data and that a proxy has not altered the response`。
长度 1–3 之前可能触发 panic；截断广告之前返回 `LBR-NET-001` 与网络/传输提示。
Git discovery 现在保留协议错误分类；SSH 广告透传与有界清理见下节。
所有读取点均要求四位 ASCII 十六进制标头。

该广告阶段不同于协商后的 upload-pack 响应：HTTP(S) discovery 广告帧分类和空 upload-pack 响应
分类不变。测试已覆盖真实本机 TCP 取对象广告读取及公开 fetch/clone/pull 错误转换，
不代表完整命令执行或有界 SSH 清理。畸形帧应检查远端 Git 服务或代理。

## SSH 广告错误处理

SSH advertisement 长度 `0001` 至 `0003`、不完整标头（包括零字节 EOF）及截断
payload 返回 `LBR-NET-002`。固定协议原因与 marker 保留，不插入捕获的 SSH
stdout/stderr。

必需标头不完整时有一项主机信任例外：本地 SSH 退出码为255，且 stderr 前64 KiB
包含受识别的 host-key 诊断时，返回固定主机核验指引与 `LBR-NET-001`。这项分类
本身不验证远端指纹。其它缺失广告（含认证失败）仍用 `LBR-NET-002`；能够观察到
非零本地退出状态时，追加 `SSH exited with status N` 与固定连接、可信主机、
ssh-agent 及仓库访问指引，不显示原始 SSH 诊断。

必需标头不完整时最多用100毫秒观察 SSH 退出状态，再按需请求终止；其它读取
错误立即请求终止。状态观察、直接子程序回收及输出收集共用两秒清理截止时间。
协议错误与带类型的主机信任错误优先于次要清理警告。普通 IO/超时保留传输错误
分类，可追加固定本地清理警告。终止程序可能改变观察到的退出状态；这不承诺
回收任意后代程序。

Clone 将主机核验指引放在结构化 hints 中；其它命令边界在 message 中保留固定
主机指引，并沿用 `LBR-NET-001` 的网络 hint。human、JSON 与 machine 诊断均
不包含捕获的远端 stderr 原文。

`git://` discovery 与取对象阶段均将上述帧错误归为 `LBR-NET-002`；所有异步
读取点拒绝非 ASCII/非十六进制标头并给出固定协议原因，HTTP(S) discovery 广告帧校验不变。

## SSH 认证与捕获诊断

无论是否由终端调用，Libra 都以 `BatchMode=yes` 启动 SSH，不在 Libra 命令中
询问私钥口令或进行交互式主机信任决定。重试前请先在 `ssh-agent` 中加载或解锁
加密私钥。主机信任应先通过可信服务商控制台或其它可信渠道核对指纹，再手动
更新 `~/.ssh/known_hosts`；也可以单独建立交互 SSH 连接，核对显示的指纹后才
接受。`ssh -T git@github.com` 是 GitHub 示例，请使用实际仓库 SSH 用户、主机
和端口，不要接受未经核验的指纹。

`ssh.strictHostKeyChecking` 保留既有 `ask`、`yes`、`accept-new`、`no` 设置。
`ask` 不向 SSH 传递该选项，由用户 SSH 配置决定；`BatchMode=yes` 仍禁止
交互决定。显式设置会转交 SSH，请按仓库需求选择主机信任策略。

SSH stderr 在终端会话中也始终捕获，从子程序启动时便持续读取，最多保留64 KiB，
其余字节继续计数并计算摘要。用户错误只含固定文字与可用的本地退出状态，不
打印或记录远端 stderr 原文。debug 诊断只含退出状态、总字节数、保留字节数及
已收集字节流的 SHA-256；收集失败或取消时可能没有这些元数据，不声称已有完整
摘要。摘要计算的工作量与实际读取字节数成正比。

SSH 引用广告与 receive-pack 响应各有16 MiB累计上限。广告超限以 `LBR-NET-001`
失败并提示在服务可用时使用仓库的 HTTPS URL，否则请维护者减少引用；push 响应超限以 `LBR-NET-001` 失败并提示减少推送
引用，绝不把截断响应视为成功。极大的引用集或更新可能受到影响；流式 fetch
pack 不受该上限约束。push 响应失败不代表服务端回滚了引用，重试前应检查远端
实际状态。既有 IO 超时仍然生效。

完整 discovery 广告之后，Libra 最多给 SSH 100毫秒退出，再请求终止；共用两秒
清理截止时间。捕获任务在所属操作退出或截止时间到期时取消，也覆盖后代程序
继续持有管道的情况。

### SSH 主机身份变更与诊断收集

SSH 报告主机身份已经变更时，Libra 保留独立的固定警告：可能发生拦截，也可能是合法密钥轮换。必须先通过可信渠道核对新指纹，才可替换 `~/.ssh/known_hosts` 中的旧条目；不要绕过主机密钥检查。此情况与首次未信任主机均使用 `LBR-NET-001`，但消息与操作提示不同。

如果 stderr 管道在有界收集期限后仍未关闭，已取得的完整协议输出及本地退出状态仍可使用；不会仅因诊断收集失败而拒绝完整传输。已观察到的非零退出状态及主要读取错误仍会导致失败。缺失的诊断仅记录固定的 debug 提示，不伪造空流字节数或摘要；stdout 收集或进程等待失败仍按原错误处理。

### SSH 上限与主机分类边界

上述16 MiB广告与receive-pack响应累计上限仅适用于Libra的SSH传输。
HTTPS和Git传输没有这一特定上限。若服务器提供HTTPS端点，SSH广告超限时可改用
该仓库的HTTPS远端URL；只读用户无需修改服务器引用。否则请仓库维护者减少广告中的
引用集合。流式fetch pack仍不受此累计上限约束。

主机信任分类必须同时满足：首个必需标头未完成、未观察到任何stdout字节、本地退出码255，
以及保留stderr前缀中的已知模式。一旦读到任何stdout字节（包括部分标头），
类似主机密钥错误的stderr不能触发特定信任指引；完整广告之后的失败保留固定通用诊断。
广告之前的模式仍只是诊断启发式，不等于指纹核验。

成功discovery的子程序若等待请求，通常会耗尽100 ms原生退出观察窗口，每次discovery
分别承担该成本。这与两秒直接子程序清理预算分开，不构成性能基准或任意后代清理保证。

## 严格 pkt-line 标头

pkt-line 标头必须恰好包含四位 ASCII 十六进制数字（`0`–`9`、`a`–`f` 或 `A`–`F`）。
fetch 流、`git://` 广告与 SSH 广告均拒绝 `+004` 等带符号标头、空白、非十六进制文字
及无效 UTF-8，并返回 `LBR-NET-002`（退出128）。原因固定，不回显标头或 payload。
此前发送带符号或其它不合规标头的对端，需要改为四位十六进制数字后重试。

Git discovery 对长度 `0001`–`0003`、缺失/不完整的必需标头及截断 payload 也保留
协议分类，并传递至 clone、fetch、pull、ls-remote 与 push。请检查远端 Git 服务或
代理响应。结构化错误字段保持原有格式；push 保留自己的协议 hint，其它命令亦然。

flush `0000`、空数据 `0004` 与最大长度 `ffff` 的语义不变；普通网络错误及超时保留
原有分类。尚无完整 pack 时的空 fetch 数据流仍属于网络失败，完整 pack 之后的 EOF
保留成功语义。上文 SSH 主机信任例外、捕获上限及清理截止时间保持不变。

## 空仓库 discovery 的帧校验

HTTP(S) 广告声明仓库为空后，仍会校验剩余的全部 pkt-line 帧。零 object ID 之后
出现畸形标头、小于四的帧长度或截断 payload 时，返回 `LBR-NET-002`（退出128），
原因固定且不回显远端字节，不再误报为空仓库成功。重试前请核对远端 Git 服务或代理
响应。合法空仓库、支持的 SHA-1/SHA-256 广告、既有命令 hint 和结构化错误字段保持
原有行为。

## Issue #477 notes

拒绝对本地 upstream（`remote=.`）执行
