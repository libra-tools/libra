# `libra clone`

将仓库克隆到新目录。

## 概要

```
libra clone [OPTIONS] <REMOTE_REPO> [LOCAL_PATH]
```

## 说明

`libra clone` 通过获取对象、配置 `origin` 并检出工作树，创建远程仓库的本地副本。它会初始化一个由 vault 支撑的仓库，并透明复用 `run_init()` 完成本地元数据设置。

克隆会从远程获取所有对象和 refs，创建带 SQLite 元数据存储的 `.libra` 目录，设置 `origin` 远程，并检出默认分支（或用 `-b` 指定的分支）。克隆期间始终会引导 vault 签名，与 `libra init` 的默认值一致。对于非裸克隆，检出的 `.gitignore` 文件会复制为对应的 `.libraignore` 文件，使 Libra 忽略规则立即生效。

对于裸克隆，不会执行工作树检出，仓库目录本身会直接成为对象存储。裸克隆不会创建 `.libraignore`。

## 全局配置 Schema 保护

配置 schema 兼容性按角色判定。`libra clone` 在信任配置前，以只读方式检查 GlobalConfig 与 SystemConfig 元数据。真正的配置 future schema，或未注册／名称不匹配的迁移 receipt，在命令需要该作用域时以 `LBR-CONFIG-001` fail-closed。当前 manifest 已知的 Repository-only receipt（包括 `2026090801`）不会使配置库被误判为 future，受支持的配置值仍可读取。本 build 能识别 configuration-owned legacy-reader barrier；详见[配置兼容性](config.md#配置-schema-兼容性)。

全局路径为 `LIBRA_CONFIG_GLOBAL_DB` 或 XDG 配置目录（`$XDG_CONFIG_HOME/libra/config.db`，默认 `<home>/.config/libra/config.db`），在自动迁移前回退到 legacy `<home>/.libra/config.db`；系统路径为 `LIBRA_CONFIG_SYSTEM_DB` 或 `/etc/libra/config.db`。完整的进程环境／repo-local 存储设置可以证明无需 GlobalConfig（`cloud` 还须满足 D1 设置），但不能证明无需 SystemConfig 默认值。诊断只说明受影响的 scope、ledger 与版本，不输出配置值或未信任 receipt 名称。

本阶段对未知／不支持的状态只有升级路径，不执行自动修复。安装兼容的较新 Libra：
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`。
禁止手工删除或修改 SQLite receipt。仅在明确需要本地对象访问时使用 `--offline` 或 `LIBRA_READ_POLICY=offline|local`；这些模式会告警，并不授权远端同步。

检出的文件会按 tree 条目 mode 的权限位物化（`100755` 可执行、`100644` 普通），并受进程 `umask` 约束（plan issues/470 FM-01）。

## 选项

### `<REMOTE_REPO>`（必需）

要克隆的远程仓库 URL。支持 SSH（`git@host:user/repo.git`）和 HTTPS（`https://host/user/repo.git`）协议，也支持本地文件系统路径。原 Cloudflare 发布站点恢复源已随 Publish 产品拆除；再使用该源会得到用法错误（退出码 129），并提示改用 git remote 或 `libra cloud` 做仓库备份。对应的 clone-domain 配置键已冷冻，不再读取。

```bash
libra clone git@github.com:user/repo.git
libra clone https://github.com/user/repo.git
libra clone /path/to/local/repo
```

### `[LOCAL_PATH]`

可选目标目录。省略时，Libra 会从仓库 URL 推断目录名（例如从 `repo.git` 推断 `repo`）。如果无法推断，会返回错误，要求用户显式指定路径。

```bash
libra clone git@github.com:user/repo.git my-dir
```

### `-b, --branch <NAME>`

检出 `<NAME>`，而不是远程 HEAD。该分支必须存在于远程；否则会报 “remote branch not found” 错误。

```bash
libra clone -b develop git@github.com:user/repo.git
```

### `--single-branch`

只获取通向单个分支 tip 的历史（HEAD，或 `-b` 给出的分支）。当大型仓库只需要一个分支时，可减少传输量。`--depth`、`--shallow-since`、`--shallow-exclude` 在未给出 `--no-single-branch` 时隐含此标志（对齐 `git clone`）。单分支克隆会写入 `remote.<name>.fetch=+refs/heads/<branch>:refs/remotes/<name>/<branch>`。只有 Git 远程支持这种传输优化。

```bash
libra clone --single-branch -b main git@github.com:user/repo.git
```

### `--no-single-branch`

克隆所有分支的历史（默认），撤销先前的 `--single-branch`（命令行最后出现者生效）。clone 默认获取所有分支，故单独使用时为 no-op。

```bash
libra clone --single-branch --no-single-branch git@github.com:user/repo.git
```

### `--bare`

创建没有工作树的裸仓库。目标目录会直接成为对象存储。适用于中心/服务端仓库。

```bash
libra clone --bare git@github.com:user/repo.git
```

### `--mirror`

建立源仓库的镜像（类似 `git clone --mirror`）。隐含 `--bare`，把已获取的分支原样映射到 `refs/heads/*`、tag 保留在 `refs/tags/*`——不保留任何 `refs/remotes/*` tracking ref——并写入 `remote.<name>.mirror=true` 标记。适用于服务端托管或备份仓库。

相对 Git 的收窄：(1) Git 原样镜像 `refs/*:refs/*`；Libra 只镜像其 fetch 传输的内容——每个已获取分支提升到 `refs/heads/*`、tag 保留，但 Libra 不获取的命名空间（如 `refs/notes/*`）不镜像。(2) 由于 Libra 的 fetch 把 `refs/heads/mr/*` 与 `refs/mr/*` 折叠进同一 tracking 命名空间，这类 ref 会被镜像为 `refs/heads/mr/*`（不保留出处）。(3) `mirror=true` 仅为标记——不写 `+refs/*:refs/*` refspec，且 `libra fetch` 尚不感知镜像，故刷新镜像不是自动的。

```bash
libra clone --mirror git@github.com:user/repo.git repo-mirror.git
```

### `--filter <spec>` / `--shallow-since <date>` / `--shallow-exclude <rev>`

Git 用于*减少*传输内容的 fetch 整形标志：`--filter`（如 `blob:none`）是部分克隆，`--shallow-since`/`--shallow-exclude` 按日期或排除 ref 限定浅历史。**Libra 没有 partial-clone/promisor 支持，其 fetch 也只支持 `--depth` 浅历史**，故这些标志被接受但**忽略并告警**——即不应用该优化（克隆仍会取回这些标志本会裁剪掉的内容，仅在同时给出 `--depth` 时按 `--depth` 限定）。不带 `--depth` 时即为**完整克隆**——是被过滤/按日期限定克隆结果的正确超集，故结果始终可用；这与 Git 自身在服务器无法处理 `--filter` 时告警并回退到完整克隆一致。`--shallow-exclude` 可多次给出。

```bash
libra clone --filter blob:none git@github.com:user/repo.git
libra clone --shallow-since "2 weeks ago" git@github.com:user/repo.git
```

### `-l, --local` / `--no-local`

Libra **从不硬链接**对象——始终复制。普通文件系统 Git 路径按本地克隆处理：`--depth`、`--shallow-since`、`--shallow-exclude`、`--filter` 被忽略，并打印 Git 的本地克隆警告（`--depth is ignored in local clones; use file:// instead.`，其余标志有对应原文）。`--quiet` 仍会打印这些警告。`file://` 与 `--no-local` 保持传输浅化语义；`-l` / `--local` 恢复本地克隆路径。本地 Libra 源不变。两个标志互相覆盖，最后出现者生效。

```bash
libra clone -l /path/to/source /path/to/dest
```

### `--depth <N>`

创建浅克隆，将历史截断到指定提交数。`N` 必须是正整数。除非给出 `--no-single-branch`，否则隐含 `--single-branch`（对齐 `git clone`）。
只有 Git 远程支持浅传输。
本地 Libra 源会以 `LBR-REPO-002` 拒绝 `--depth`：该传输路径不能声明 shallow boundary，若接受会留下缺父提交的克隆。此 fail-closed 行为是已接受的终态（开发兼容登记 D20 决策），不是待补缺口。
普通文件系统 Git 路径会忽略 `--depth` 并告警（issues/474 CL-06）。
通过 `file://` 或 `--no-local` 访问的本地 Git 源按各 want 的最短距离截断，再做一次边界计算：有父提交未被发送，或根提交恰好落在深度截止上时，该提交写入 `.libra/shallow`（issues/474 CL-04）。

```bash
libra clone --depth 1 git@github.com:user/repo.git
libra clone --depth 50 git@github.com:user/repo.git
```

### `--reject-shallow`

若**源**仓库是浅克隆则失败，对齐 `git clone --reject-shallow`（exit 128），且不留下目标目录。本地 Git 浅源在创建目标前检查；不带该标志克隆浅源时，会把源的 `.git/shallow` 边界并入 `.libra/shallow`，使 `log` / `fsck` 可遍历。本地 Libra 源带 `--depth` 仍在对象传输前以 `LBR-REPO-002` fail-closed。

对能协商 shallow boundary 的网络远程，未请求 `--depth` 时仍会在 fetch 后拒绝意外的浅结果。

```bash
libra clone --reject-shallow git@github.com:user/repo.git
```

### `--reference <repo>` / `--reference-if-able <repo>` / `--shared`（`-s`） / `--dissociate`

Git 的对象共享标志，用于设置 `objects/info/alternates`，让克隆从另一个本地对象库借用或共享对象。**Libra 没有对象 alternates**——它总是把每个对象拷贝进克隆——因此 Libra 克隆始终完全自包含。故这些标志按**no-op**接受以兼容：

- `--reference <repo>` 与 `--shared`（`-s`）会追加一条说明性 warning，指出它们没有生效（对象是拷贝而非借用/共享）。`--reference` 可多次给出。
- `--reference-if-able <repo>` 被静默忽略——这与 Git 一致：Git 对无法使用的引用静默丢弃（此处没有可用引用）。可多次给出。
- `--dissociate` 是静默 no-op：从来没有需要 dissociate 的借用。

克隆仍会成功，并产生完整、自包含的仓库。

```bash
libra clone --reference /path/to/local/mirror git@github.com:user/repo.git
libra clone --dissociate git@github.com:user/repo.git
```

### `--no-progress`

在克隆期间抑制 fetch 进度条（“Receiving objects” spinner），对齐 `git clone --no-progress`。其它输出不受影响。

```bash
libra clone --no-progress git@github.com:user/repo.git
```

### `--no-checkout`

克隆后不把 HEAD 检出到工作区，对齐 `git clone --no-checkout`。objects、refs 与 HEAD 仍会设置，只跳过工作区检出，因此目标目录只有仓库元数据而没有检出的文件。

```bash
libra clone --no-checkout git@github.com:user/repo.git
```

### `-o`, `--origin <NAME>`

用 `<NAME>` 命名远端（及其 `refs/remotes/<NAME>/*` 跟踪引用），取代默认的 `origin`，对齐 `git clone -o`。分支跟踪配置（`branch.<branch>.remote`）与 `remote.<NAME>.url` 都使用所选名称。该选项适用于标准克隆。

```bash
libra clone -o upstream git@github.com:user/repo.git
```

## 常用命令

```bash
libra clone git@github.com:user/repo.git
libra clone https://github.com/user/repo.git
libra clone git@github.com:user/repo.git my-dir
libra clone --bare git@github.com:user/repo.git
libra clone -b develop git@github.com:user/repo.git
libra clone --single-branch -b main git@github.com:user/repo.git
libra clone --depth 1 git@github.com:user/repo.git
```

## 人工输出

默认人工模式将分阶段进度写入 `stderr`，最终摘要写入 `stdout`。

阶段：

- `Connecting to <url> ...`
- `Initializing repository ...`
- `Fetching objects ...`
- `Configuring repository ...`
- `Checking out working copy ...`（仅非裸仓库）

成功输出：

```text
Cloned into 'repo'
  remote: origin -> git@github.com:user/repo.git
  branch: main
  signing: enabled

Tip: using existing SSH key at ~/.ssh/id_ed25519
```

裸克隆：

```text
Cloned into bare repository '/path/to/repo.git'
  remote: origin -> git@github.com:user/repo.git
  branch: main
  signing: enabled
```

空远程：

```text
Cloned into 'empty'
  remote: origin -> git@github.com:user/empty.git
  signing: enabled

warning: You appear to have cloned an empty repository.
```

`--quiet` 会抑制所有进度和最终成功摘要，包括警告。

## 结构化输出

`libra clone` 支持全局 `--json` 和 `--machine` 标志。

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- 两者都会抑制进度输出和嵌套的 init/fetch 输出
- 成功时 `stderr` 保持干净

示例：

```json
{
  "ok": true,
  "command": "clone",
  "data": {
    "path": "/Users/eli/projects/my-repo",
    "bare": false,
    "remote_url": "git@github.com:user/repo.git",
    "remote_name": "origin",
    "branch": "main",
    "object_format": "sha1",
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "vault_signing": true,
    "ssh_key_detected": "/Users/eli/.ssh/id_ed25519",
    "shallow": false,
    "warnings": [],
    "gitignore_converted": [".libraignore"],
    "objects_fetched": 42,
    "bytes_received": 4096
  }
}
```

空远程返回 `"branch": null` 和一个警告：

```json
{
  "ok": true,
  "command": "clone",
  "data": {
    "path": "/Users/eli/projects/empty-repo",
    "bare": false,
    "remote_url": "git@github.com:user/empty-repo.git",
    "remote_name": "origin",
    "branch": null,
    "object_format": "sha1",
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "vault_signing": true,
    "ssh_key_detected": null,
    "shallow": false,
    "warnings": [
      "You appear to have cloned an empty repository."
    ],
    "gitignore_converted": [],
    "objects_fetched": 0,
    "bytes_received": 0
  }
}
```

### Schema 说明

- `remote_name` 是配置的远端名称（默认 `origin`，标准克隆下为 `-o`/`--origin` 的值）
- `branch` 是实际检出的分支；远程没有 refs 时为 `null`
- `gitignore_converted` 列出从 `.gitignore` 转换写出的 `.libraignore` 文件（工作区相对路径）；始终存在（裸克隆或源无 `.gitignore` 时为空）
- 使用 `--depth` 时，`shallow` 为 `true`
- 普通 Git/本地克隆会省略 `source_kind` 和 `cloud_site`
- init 中的 `ref_format` 和 `converted_from` 被有意排除
- `objects_fetched` / `bytes_received` 给出 Git 源 fetch pack 的对象数与字节大小

## 设计动机

### 没有 `--recurse-submodules`

Git 的 submodule 系统（`--recurse-submodules`）经常给开发者带来摩擦：submodule 需要独立的 fetch/checkout 循环，会创建嵌套 `.git` 目录，并破坏许多假定单一工作树的工具。Libra 不实现 submodule。对于 monorepo 工作流，所有代码都位于单个仓库中。对于多仓库组合，Libra 鼓励使用显式依赖管理（包管理器、vendoring），而不是把仓库嵌进仓库。这让 clone 操作保持简单且可预测。

### 克隆期间引导 vault

Libra 在 clone 期间复用与 `libra init` 相同的 `run_init()` 路径来初始化由 vault 支撑的签名。这意味着每个克隆出的仓库无需额外设置即可立即生成签名提交。Git 要求用户在克隆后手动配置 GPG/SSH 签名，这意味着大多数克隆仓库默认会产生未签名提交。通过在克隆时引导 vault，Libra 确保克隆仓库的安全姿态与新初始化仓库一致。

### 忽略文件转换

Libra 使用 `.libraignore` 作为忽略策略。非裸克隆期间，每个检出的 `.gitignore` 都会复制到同级 `.libraignore`。已有的用户自有 `.libraignore` 文件会被保留并作为警告展示；原始 `.gitignore` 文件保持不变。

### 用 `--depth` 进行浅克隆

浅克隆对于 CI/CD 流水线和不需要完整历史的大型 monorepo 很重要。Libra 对能协商 shallow boundary 的 Git 远程支持 `--depth N`：历史会截断到指定提交数。depth 值在解析时校验（必须是正整数），并传递到 fetch 协议层。本地 Libra 源维持 fail-closed 返回 `LBR-REPO-002`（已决终态，见开发兼容登记 D20）。对于 `--shallow-since`/`--shallow-exclude`（以及 partial-clone 的 `--filter`）：Libra 的 fetch 只支持 `--depth`、无 partial-clone/promisor 支持，故这些 flag 按 no-op 接受——忽略并告警；**不应用该优化**，历史仅在同时给出 `--depth` 时按 `--depth` 限定，不带 `--depth` 时即为完整克隆（被过滤/浅克隆结果的正确超集）。每个给出的 flag 追加一条 warning（与 Git 在服务器不支持 `--filter` 时告警回退到完整克隆一致）。

### `--sparse` 被有意不支持

稀疏检出（`git clone --sparse`、`git sparse-checkout`）被有意不实现。Sparse cone/skip-worktree 依赖 Git 管理的工作树配置，而 Libra 已将 config / HEAD / refs 迁移到 SQLite。桥接并非零成本；基于审计的决策是推迟 `--sparse`，直到出现无法通过分层云存储满足的具体 monorepo 子树检出需求。重启条件见 [`docs/development/commands/_compatibility.md`](../../development/commands/_compatibility.md) 条目 **D10**。

### `--recurse-submodules` 被有意不支持

按照更广泛的产品边界（没有 submodule 子命令表面），`clone --recurse-submodules` 也不受支持。重启条件见 [`docs/development/commands/_compatibility.md`](../../development/commands/_compatibility.md) 条目 **D1**（submodule）和 **D4**（clone --recurse-submodules）。

### `--single-branch` 标志

与 `--branch` 组合时，`--single-branch` 通过只获取指定分支的历史来减少 clone 期间传输的数据量。`--depth` / `--shallow-since` / `--shallow-exclude` 在未给出 `--no-single-branch` 时同样收窄。这对包含许多长期分支的大型仓库尤其有用，例如 CI 构建某个特定 release 分支时只需要一个分支。Git 也支持此能力；jj 不支持，因为它的 operation-log 模型按设计获取所有 refs。

## 参数对比：Libra vs Git vs jj

| 参数 / 标志 | Git | jj | Libra |
|---|---|---|---|
| 远程 URL（位置参数） | `git clone <url>` | `jj git clone <url>` | `libra clone <url>` |
| 目标目录 | `git clone <url> <dir>` | `jj git clone <url> <dir>` | `libra clone <url> <dir>` |
| 指定分支 | `-b` / `--branch` | `-b` / `--branch`（jj 0.17+） | `-b` / `--branch` |
| 单分支 | `--single-branch` | N/A | `--single-branch` |
| 不限单分支 | `--no-single-branch` | N/A | `--no-single-branch`（撤销 `--single-branch`；默认即所有分支） |
| 裸克隆 | `--bare` | N/A | `--bare` |
| 浅克隆（depth） | `--depth <n>` | N/A | Git 远程支持；本地 Libra 源 fail-closed (`LBR-REPO-002`)；云端拒绝 |
| 按日期浅克隆 | `--shallow-since=<date>` | N/A | Git 远程按 no-op 接受（忽略+告警；不应用、仅按 `--depth` 限定）；云端拒绝 |
| 排除浅边界 | `--shallow-exclude=<rev>` | N/A | Git 远程按 no-op 接受（忽略+告警；不应用、仅按 `--depth` 限定）；云端拒绝 |
| 镜像克隆 | `--mirror` | N/A | `--mirror`（隐含 `--bare`；把已获取分支映射到 `refs/heads/*`、保留 tag、无 tracking ref、设 `remote.<name>.mirror` 标记；收窄——仅 fetch 的分支/tag，刷新不感知镜像） |
| 引用仓库 | `--reference <repo>` / `--reference-if-able <repo>` | N/A | 接受式 no-op（Libra 总是拷贝对象、无 alternates）；`--reference` 告警，`--reference-if-able` 静默 |
| 共享对象库 | `--shared` / `-s` | N/A | 接受式 no-op（总是拷贝）；告警 |
| 从引用仓库脱离 | `--dissociate` | N/A | 接受式 no-op（已自包含）；静默 |
| 禁用硬链接 | `--no-hardlinks` | N/A | N/A |
| 递归 submodule | `--recurse-submodules` | N/A | N/A（无 submodule） |
| 浅 submodule | `--shallow-submodules` | N/A | N/A |
| 独立 git dir | `--separate-git-dir=<dir>` | N/A | N/A（已移除） |
| 模板目录 | `--template=<dir>` | N/A | N/A（由 init 内部处理） |
| Quiet 模式 | `-q` / `--quiet` | `--quiet` | `--quiet`（全局标志） |
| Verbose / 进度 | `--progress` / `--verbose` | N/A | 分阶段 stderr 进度（默认） |
| 不检出 | `-n` / `--no-checkout` | N/A | `--no-checkout` |
| 稀疏检出 | `--sparse` | N/A | N/A |
| Filter（部分克隆） | `--filter=<spec>` | N/A | Git 远程按 no-op 接受（忽略+告警；不应用、仅按 `--depth` 限定）；云端拒绝 |
| Bundle URI | `--bundle-uri=<uri>` | N/A | N/A |
| Vault 签名引导 | N/A | N/A | 始终启用（匹配 init） |
| SSH key 检测 | N/A | N/A | 自动检测 + 提示 |
| 结构化 JSON 输出 | N/A | N/A | `--json` / `--machine` |
| 错误提示 | 最少消息 | 最少消息 | 每种错误都有可操作提示 |

## 错误处理

每个 `CloneError` 变体都映射到显式 `StableErrorCode`，不依赖消息子串推断。

| 场景 | 错误码 | 退出 | 提示 |
|------|--------|------|------|
| 无法推断目标路径 | `LBR-CLI-002` | 129 | "please specify the destination path explicitly" |
| 目标已存在且非空 | `LBR-CLI-003` | 129 | "choose a different path or empty the directory first" |
| 目标已包含仓库 | `LBR-REPO-003` | 128 | "the destination already contains a libra repository" |
| 无法创建目标目录 | `LBR-IO-002` | 128 | "check directory permissions and disk space" |
| 本地路径不存在 | `LBR-REPO-001` | 128 | "use a valid libra repository path or a reachable remote URL" |
| URL 格式错误或 scheme 不支持 | `LBR-CLI-003` | 129 | "check the clone URL or scheme" |
| 认证 / 权限拒绝 | `LBR-AUTH-002` | 128 | "check SSH key / HTTP credentials and repository access rights" |
| 网络不可达 | `LBR-NET-001` | 128 | "check the remote host, DNS, VPN/proxy, and network connectivity" |
| pkt-line discovery / 传输帧错误 | `LBR-NET-002` | 128 | "check that the remote serves Git data and that a proxy has not altered the response" |
| 其他 discovery 协议错误 | `LBR-NET-002` | 128 | "the remote did not complete discovery successfully; retry and inspect server/protocol settings" |
| 找不到远程分支 | `LBR-REPO-003` | 128 | "use `-b <branch>` to specify an existing branch" |
| 对象格式不匹配 | `LBR-REPO-003` | 128 | "the remote and local repository use different object formats" |
| 检出解析失败 | `LBR-REPO-003` | 128 | "working tree checkout target could not be resolved" |
| 检出读取失败 | `LBR-IO-001` | 128 | "failed to read repository state while checking out" |
| 检出写入失败 | `LBR-IO-002` | 128 | "files could not be written" |
| 检出 LFS 下载失败 | `LBR-NET-001` | 128 | "LFS content transfer failed" |
| 内部不变量 | `LBR-INTERNAL-001` | 128 | Issues URL |

Init 错误会通过 `InitError -> CliError` 透明转发。

### 清理失败可见性

当 clone 失败时，`cleanup_failed_clone()` 会尝试删除部分创建的目录。如果清理本身也失败，该警告会通过 `with_priority_hint()` 附加到错误上，使其同时出现在人工和 JSON 错误输出中，而不是被静默吞掉。

### 非裸检出是成功条件

`setup_repository()` 使用 `execute_checked_typed()`，它返回类型化的 `RestoreError` 变体。如果检出失败，clone 会报告失败，不会静默成功并留下损坏的工作树。

## Vault 与身份

- Clone 始终使用 `vault: true` 初始化，与 `libra init` 默认值一致
- init 的 `vault_signing` 和 `ssh_key_detected` 会透明转发到 `CloneOutput`
- SSH key 检测使用 init 阶段隔离出的 `HOME`

## 兼容性说明

- 不支持 `--recurse-submodules`；Libra 不实现 submodule
- `--reference`/`--reference-if-able`/`--shared`/`--dissociate` 按接受式 no-op 处理（Libra 无对象 alternates、总是拷贝对象，故克隆天然自包含；`--reference`/`--shared` 告警，其余静默）
- Clone 始终引导 vault 签名；如有需要，可在克隆后使用 `libra config` 禁用
- `--depth` 值必须是正整数；0 或负数会在解析时被拒绝
- `--no-checkout` 会设置 objects/refs/HEAD 但跳过工作区检出；若想完全不要工作树（无 `.libra` 工作区布局），改用 `--bare`

## 畸形 HTTP(S) discovery 响应

在 HTTP(S) 引用发现（discovery）期间，Libra 会拒绝零字节广告和畸形
pkt-line 帧，包括不完整或非十六进制标头、小于四的帧长度以及截断的 payload。
合法的 `0000` flush 与未收到响应有明确区别；合法的空仓库广告仍受支持。
不支持的 object-format capability 使用固定错误消息
`Unsupported object format capability`，不回显远端提供的值。
请确认 URL 指向 Git smart HTTP 服务，并检查代理是否截断或替换了响应，然后重试。

## pkt-line 错误归类

检测到的 pkt-line 帧格式错误返回 `LBR-NET-002`（退出码128），包括空的 HTTP(S)
discovery 广告。普通连接失败、连接重置和超时返回 `LBR-NET-001`（退出码128）。
协议错误发生时请核对 Git 服务及代理响应。discovery 帧错误的提示为
`check that the remote serves Git data and that a proxy has not altered the response`。

对象传输阶段检测到的 pkt-line 错误（包括标头或 payload 截断）使用相同协议错误码及
提示。discovery 中的普通 IO 错误仍为 `LBR-IO-001`；host-key 指引见下方 SSH 章节。

pack 完整性与 pkt-line 帧格式是不同错误：在完整帧边界结束但 pack 未完整时，clone
仍使用既有 `LBR-NET-001` 传输错误与网络重试提示；fetch/pull 对此使用 `LBR-NET-002`。

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
