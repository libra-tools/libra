# `libra push`

将本地提交和对象发送到远程仓库，并更新远程 refs。
支持 SSH 和 HTTPS 传输、LFS 文件上传（仅 HTTP）、快进检测、强制推送、dry-run 预览、多 refspec 更新、删除远程 ref、推送标签和镜像预览。

## 概要

```
libra push [OPTIONS] [<repository> [<refspec>...]]
```

## 说明

`libra push` 将提交、树、blob 和标签从本地仓库传输到远程。无参数调用时，它会把当前分支推送到已配置的上游远程。给出 `repository` 和一个或多个 `refspec` 值时，所有 refspec 会在任何网络写入前完成校验，然后作为一个 receive-pack 请求发送。`--tags` 推送所有本地标签，`--mirror` 将本地分支/标签 refs 镜像到远程，包括删除远程独有 refs。

该命令会与远程协商以确定缺失对象，把它们打包为单个 pack 文件，并随 ref-update 请求一起发送。如果远程 ref 已分叉（非快进），除非使用 `--force`，否则推送会被拒绝。

LFS 跟踪文件会在 HTTP 推送期间透明上传，不需要单独执行 `lfs push`。

## 全局配置 Schema 保护

`libra push` 在信任远端 / tiered 对象存储设置前，会读取全局存储配置（`~/.libra/config.db`，或 `LIBRA_CONFIG_GLOBAL_DB` 指定的路径）。如果该数据库的 schema 版本比当前二进制支持的版本更新，push 会以 `LBR-CONFIG-001` fail-closed，而不是静默忽略全局存储配置并回退到本地对象。诊断会包含二进制路径和版本、配置 DB 路径、schema 版本，以及升级命令：
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`。

只有在明确希望本地对象访问时，才使用 `libra --offline push ...` 或 `LIBRA_READ_POLICY=offline|local libra push ...`。Libra 会告警一次，并在本次运行中忽略全局存储配置。

## 选项

| 标志 / 参数 | 说明 | 示例 |
|-------------|------|------|
| `<repository>` | 远程名称（例如 `origin`）。使用 `<refspec>`、`--tags` 或 `--mirror` 时必需。 | `libra push origin main` |
| `<refspec>...` | 本地 ref、`<src>:<dst>` 映射，或 `:<dst>` 删除。多个值作为一个更新集合发送。 | `libra push origin main feature:release` |
| `-u`, `--set-upstream` | 单分支推送成功后设置上游跟踪分支。 | `libra push -u origin feature-x` |
| `-f`, `--force` | 允许覆盖远程历史的非快进更新。 | `libra push --force origin main` |
| `-d`, `--delete` | 删除命名的远程 ref（每个 `<refspec>` 改写为 `:<ref>` 删除）。至少需要一个 ref；与 `--set-upstream`/`--tags`/`--mirror` 互斥。 | `libra push -d origin feature-x` |
| `-n`, `--dry-run` | 执行协商和对象收集，但跳过实际上传。报告会推送什么。 | `libra push --dry-run` |
| `--tags` | 推送所有本地 `refs/tags/*` refs。已存在且相同的远程标签会跳过。 | `libra push --tags origin` |
| `--mirror` | 将本地 `refs/heads/*` 和 `refs/tags/*` 镜像到远程，删除远程独有分支/标签 refs。配合 `--dry-run` 预览。 | `libra push --mirror --dry-run origin` |
| `--no-verify` | 绕过 `pre-push` hook。为兼容而接受的 **no-op**：Libra 的 push 不运行客户端 `pre-push` hook，故无可绕过。 | `libra push --no-verify origin main` |
| `--no-progress` | 在 stderr 抑制进度条（“Compressing objects” / “Writing objects” reporters），对齐 `git push --no-progress`。 | `libra push --no-progress origin main` |
| `--json` | 向 stdout 输出结构化 JSON 信封（全局标志）。 | `libra push --json` |
| `--machine` | 紧凑单行 JSON；抑制进度（全局标志）。 | `libra push --machine` |
| `--quiet` | 抑制 stdout 摘要；警告仍写入 stderr。 | `libra push --quiet` |

## 常用命令

```bash
libra push
libra push origin main
libra push -u origin feature-x
libra push --force origin main
libra push --dry-run
libra push origin local_branch:release
libra push origin main feature:release
libra push origin :stale-branch
libra push origin refs/tags/v1.0:refs/tags/v1.0
libra push --tags origin
libra push --mirror --dry-run origin
libra push --json
```

## 人工输出

默认人工模式将进度写入 `stderr`，将 push 摘要写入 `stdout`。

普通推送：

```text
To git@github.com:user/repo.git
   abc1234..def5678  main -> main
 256 objects pushed (1.2 MiB)
```

新分支：

```text
To git@github.com:user/repo.git
 * [new branch]      feature-x -> feature-x
 12 objects pushed (48.0 KiB)
```

删除远程 ref：

```text
To git@github.com:user/repo.git
 - [deleted]         stale-branch
```

新标签：

```text
To git@github.com:user/repo.git
 * [new tag]      v1.0 -> v1.0
```

已是最新：

```text
Everything up-to-date
```

强制推送：

```text
To git@github.com:user/repo.git
 + abc1234...def5678 main -> main (forced update)
 128 objects pushed (512.0 KiB)
warning: force push overwrites remote history
```

Dry-run：

```text
To git@github.com:user/repo.git
   abc1234..def5678  main -> main (dry run)
 256 objects would be pushed
```

设置上游：

```text
To git@github.com:user/repo.git
   abc1234..def5678  main -> main
 256 objects pushed (1.2 MiB)
branch 'main' set up to track 'origin/main'
```

`--quiet` 会抑制 `stdout`，但保留 `stderr` 上的警告（例如强制推送）。

## 结构化输出（JSON 示例）

`libra push` 支持全局 `--json` 和 `--machine` 标志。

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- JSON/machine 模式会抑制进度输出
- 成功时 `stderr` 保持干净

示例：

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [
      {
        "kind": "update",
        "local_ref": "refs/heads/main",
        "remote_ref": "refs/heads/main",
        "old_oid": "abc1234...",
        "new_oid": "def5678...",
        "forced": false
      }
    ],
    "objects_pushed": 256,
    "bytes_pushed": 1258291,
    "lfs_files_uploaded": 0,
    "dry_run": false,
    "up_to_date": false,
    "upstream_set": null,
    "warnings": []
  }
}
```

已是最新：

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [],
    "objects_pushed": 0,
    "bytes_pushed": 0,
    "lfs_files_uploaded": 0,
    "dry_run": false,
    "up_to_date": true,
    "upstream_set": null,
    "warnings": []
  }
}
```

Dry-run：

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [
      {
        "kind": "update",
        "local_ref": "refs/heads/main",
        "remote_ref": "refs/heads/main",
        "old_oid": "abc1234...",
        "new_oid": "def5678...",
        "forced": false
      }
    ],
    "objects_pushed": 256,
    "bytes_pushed": 0,
    "lfs_files_uploaded": 0,
    "dry_run": true,
    "up_to_date": false,
    "upstream_set": null,
    "warnings": []
  }
}
```

强制推送：

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [
      {
        "kind": "update",
        "local_ref": "refs/heads/main",
        "remote_ref": "refs/heads/main",
        "old_oid": "abc1234...",
        "new_oid": "def5678...",
        "forced": true
      }
    ],
    "objects_pushed": 128,
    "bytes_pushed": 524288,
    "lfs_files_uploaded": 0,
    "dry_run": false,
    "up_to_date": false,
    "upstream_set": null,
    "warnings": ["force push overwrites remote history"]
  }
}
```

设置上游：

```json
{
  "ok": true,
  "command": "push",
  "data": {
    "remote": "origin",
    "url": "git@github.com:user/repo.git",
    "updates": [
      {
        "kind": "update",
        "local_ref": "refs/heads/main",
        "remote_ref": "refs/heads/main",
        "old_oid": "abc1234...",
        "new_oid": "def5678...",
        "forced": false
      }
    ],
    "objects_pushed": 256,
    "bytes_pushed": 1258291,
    "lfs_files_uploaded": 0,
    "dry_run": false,
    "up_to_date": false,
    "upstream_set": "origin/main",
    "warnings": []
  }
}
```

### Schema 说明

- `updates` 列出每个 ref 更新；已是最新时为空
- `kind` 对分支/标签更新为 `update`，对远程 ref 删除为 `delete`
- 删除更新使用空 `local_ref`，并以全零对象 ID 作为 `new_oid`
- 新分支没有先前远程 ref，因此 `old_oid` 为 `null`
- 需要 `--force` 的更新（非快进）中 `forced` 为 `true`
- `bytes_pushed` 是 pack 数据大小（字节）；dry-run 时为 `0`
- `lfs_files_uploaded` 统计已传输的 LFS 对象（仅 HTTP 传输）
- 使用 `-u` / `--set-upstream` 时，`upstream_set` 非 null
- `warnings` 包含强制推送警告或其他建议性消息

## Refspec 语义

支持以下形式：

| 调用 | 含义 |
|------|------|
| `libra push` | 将当前分支推送到其已配置的跟踪远程 |
| `libra push origin main` | 将本地 `refs/heads/main` 推送到远程 `refs/heads/main` |
| `libra push origin local:release` | 将本地 `refs/heads/local` 推送到远程 `refs/heads/release` |
| `libra push origin main feature:release` | 一起校验并发送多个 ref 更新 |
| `libra push origin :feature` | 删除远程 `refs/heads/feature` |
| `libra push -d origin feature` | 删除远程 `refs/heads/feature`（短形式） |
| `libra push origin refs/tags/v1.0:refs/tags/v1.0` | 推送标签 ref |
| `libra push --tags origin` | 推送所有本地标签 refs |
| `libra push --mirror --dry-run origin` | 预览镜像分支/标签 refs 并删除远程独有 refs |

空目标语法（`src:`）、格式错误的 ref 名称、重复目标 refs，以及 `--mirror` 与显式 refspec 组合，都会在任何网络写入前被拒绝。无效形式返回 `InvalidRefspec`，退出码 129。

## 设计动机

### 为什么要求显式的 repository+refspec 组合？

Git 允许 `git push origin`（将当前分支推送到同名远程分支），并把 `repository` 与 `refspec` 视为相互独立的可选参数，带有复杂默认规则（`push.default`、`remote.pushDefault`、分支跟踪配置）。这种灵活性是意外推送到错误分支的知名来源。Libra 有意采取更受限的立场：命名远程时也必须命名 ref。裸 `libra push` 形式（无参数）使用跟踪配置，语义明确。这在不降低脚本化或 agent 驱动工作流表达力的前提下，消除了整类“我不小心推到了生产分支”的错误。

### 为什么继续拒绝本地文件远程？

Libra 仍将本地文件远程 push 视为有意不同的表面。C8 ref update 扩展适用于网络 receive-pack 传输；本地路径远程继续 fail closed，以避免未定义的并发文件系统变更语义。

### 为什么集成 LFS push？

Git LFS 需要单独的二进制（`git-lfs`）和 post-push hook 来上传大文件。这种两阶段设计意味着 LFS 失败可能让远程处于不一致状态：提交引用了尚未到达的 LFS 指针后端对象。Libra 在对象收集阶段检测 LFS 指针 blob，并在 HTTP push 事务中内联上传它们。这保证了原子性：要么所有对象（包括 LFS）都到达，要么 push 干净失败。该集成是透明的，用户不需要安装或配置单独的 LFS 工具。

## 参数对比：Libra vs Git vs jj

| 参数 | Libra | Git | jj |
|------|-------|-----|----|
| 基础 push | `libra push` | `git push` | `jj git push` |
| 命名远程 + ref | `libra push origin main` | `git push origin main` | `jj git push --remote origin --branch main` |
| 设置上游 | `libra push -u origin main` | `git push -u origin main` | N/A（jj 跟踪 bookmarks） |
| 强制推送 | `libra push --force` | `git push --force` | `jj git push --allow-new` |
| Dry-run | `libra push --dry-run` | `git push --dry-run` | `jj git push --dry-run` |
| Refspec 映射 | `libra push origin src:dst` | `git push origin src:dst` | N/A |
| 多 refspec | `libra push origin main feature:release` | `git push origin main feature:release` | N/A |
| 删除远程分支 | `libra push -d origin branch` 或 `libra push origin :branch` | `git push -d origin branch` / `git push origin :branch` | `jj git push --delete branch` |
| 推送标签 | `libra push --tags origin` | `git push --tags origin` | N/A |
| 镜像预览 | `libra push --mirror --dry-run origin` | `git push --mirror --dry-run origin` | N/A |
| 结构化输出 | `--json` / `--machine` | 无 | 无 |
| 远程名称建议 | 模糊匹配 “did you mean?” | 无 | 无 |
| 错误提示 | 每种错误都有可操作提示 | 最少 | 最少 |
| LFS 集成 | HTTP push 期间透明处理 | `git lfs push`（独立） | N/A |

## 错误处理

每个 `PushError` 变体都映射到显式 `StableErrorCode`。远程名称拼写错误会通过编辑距离触发模糊匹配建议。

| 场景 | 错误码 | 退出 | 提示 |
|------|--------|------|------|
| HEAD 已分离 | `LBR-REPO-003` | 128 | "checkout a branch before pushing" |
| 未配置远程 | `LBR-REPO-003` | 128 | "use 'libra remote add' to configure a remote" |
| 找不到远程 | `LBR-CLI-003` | 129 | "use 'libra remote -v'" + 模糊 "did you mean?" |
| 无效 refspec | `LBR-CLI-002` | 129 | "use '\<name>' or '\<src>:\<dst>'" |
| 找不到源 ref | `LBR-CLI-003` | 129 | "verify the local branch/ref exists" |
| 本地文件远程 | `LBR-CLI-003` | 129 | "push supports network remotes only" |
| 无效远程 URL | `LBR-CLI-002` | 129 | "check the remote URL" |
| 认证失败 | `LBR-AUTH-001` | 128 | "check SSH key or HTTP credentials" |
| Discovery 失败 | `LBR-NET-001` | 128 | "check the remote URL and network connectivity" |
| 网络超时 | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| 非快进 | `LBR-CONFLICT-002` | 128 | "pull first, or use --force (data loss risk)" |
| 对象收集失败 | `LBR-INTERNAL-001` | 128 | Issues URL |
| Pack 编码失败 | `LBR-INTERNAL-001` | 128 | Issues URL |
| 远程 unpack 失败 | `LBR-NET-002` | 128 | "retry or check server logs" |
| 远程 ref 更新被拒绝 | `LBR-NET-002` | 128 | "check branch protection rules" |
| 网络错误 | `LBR-NET-001` | 128 | "check network connectivity and retry" |
| LFS 上传失败 | `LBR-NET-001` | 128 | "check LFS endpoint configuration" |
| 跟踪 ref 更新失败 | `LBR-IO-002` | 128 | -- |
| 仓库状态错误 | `LBR-REPO-002` | 128 | "try 'libra status' to verify" |

### 超时策略

- Discovery / 连接：60s 连接超时
- 上传 / receive-pack：600s idle 超时（无数据进度会触发超时）
- 超时会映射为带 `phase` 细节的 `NetworkUnavailable`
