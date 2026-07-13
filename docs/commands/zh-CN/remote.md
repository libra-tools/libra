# `libra remote`

管理已配置远程：列出、添加、移除、重命名、检查和修改 URL，并修剪陈旧的远程跟踪引用。

## 概要

```
libra remote <subcommand> [OPTIONS] [ARGS]
libra remote show
libra remote -v
libra remote add [-f | --fetch] [-t | --track <branch>]... [-m | --master <branch>] [--tags | --no-tags] [--mirror] <name> <url>
libra remote remove <name>
libra remote rename <old> <new>
libra remote get-url [--push] [--all] <name>
libra remote set-url [--add | --delete] [--push] [--all] <name> <value>
libra remote prune [--dry-run] <name>
libra remote update [-p | --prune] [<group> | <remote>...]
```

## 说明

`libra remote` 管理存储在 SQLite 配置数据库中的具名远程集合。每个远程有一个或多个 fetch URL，并可选拥有独立 push URL。子命令允许对远程及其 URL 进行完整 CRUD 操作，也可以修剪远程上已不存在的陈旧远程跟踪分支。

远程配置存储为 SQLite `config` 表中的 `remote.<name>.url` 和 `remote.<name>.pushurl` 键，而不是扁平 `.git/config` 文件。这提供事务安全性（崩溃时不会部分写入），并让代理和工具可以查询远程元数据。

## 选项

### 子命令：`show`

列出已配置远程名称，每行一个。

| 参数 | 说明 |
|----------|-------------|
| (none) | 打印所有远程名称 |

### 子命令：`-v`（verbose 列表）

列出每个远程及其 fetch 和 push URL。

| 参数 | 说明 |
|----------|-------------|
| (none) | 为每个 URL 打印 `<name>\t<url> (fetch\|push)` |

### 子命令：`add`

注册新远程。

| 参数 | 说明 | 示例 |
|----------|-------------|---------|
| `<name>` | 远程的逻辑名称 | `origin` |
| `<url>` | 远程的 fetch URL | `https://example.com/repo.git` |
| `-f`, `--fetch` | 添加后立即从新远程 fetch | |
| `-t`, `--track <branch>` | 只跟踪指定分支——写入特定的 `remote.<name>.fetch` refspec 取代默认通配。可重复。 | `-t main -t dev` |
| `-m`, `--master <branch>` | 将远程 HEAD（`refs/remotes/<name>/HEAD`）指向 `<branch>`（即使跟踪 ref 尚不存在也会写入，与 Git 一致） | `-m main` |
| `--tags` / `--no-tags` | 设置 `remote.<name>.tagOpt` 为 fetch 全部/不 fetch 标签（互斥） | |
| `--mirror` | 将远程标记为镜像——写入 `remote.<name>.mirror=true` 标记（类似 Git `remote add --mirror=fetch`）。与 `-t`/`--track` 互斥。该标记仅为信息性：Libra 不写 `+refs/*:refs/*` refspec，因为 `libra fetch` 尚不感知镜像（与 `libra clone --mirror` 一致）。 | `--mirror` |

### 子命令：`remove`

删除远程及其所有配置键。

| 参数 | 说明 | 示例 |
|----------|-------------|---------|
| `<name>` | 要移除的远程名称 | `origin` |

### 子命令：`rename`

重命名已有远程。

| 参数 | 说明 | 示例 |
|----------|-------------|---------|
| `<old>` | 当前名称 | `origin` |
| `<new>` | 新名称 | `upstream` |

重命名在一个 SQLite 事务中执行：同时重写 `remote.<old>.*` 配置键、分支 upstream 值、SSH 凭据命名空间、fetch refspec 的目标、缓存的 `refs/remotes/<old>/*` 跟踪分支与远程 HEAD 行，以及相应 reflog 名称。任一步写入失败都会回滚整个重命名。

### 子命令：`get-url`

打印为远程配置的 URL。

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `<name>` | 远程名称 | `origin` |
| `--push` | 打印 push URL 而不是 fetch URL | `libra remote get-url --push origin` |
| `--all` | 打印所有已配置 URL（不只是第一个） | `libra remote get-url --all origin` |

### 子命令：`set-url`

添加、替换或删除远程 URL。

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `<name>` | 远程名称 | `origin` |
| `<value>` | URL 值（或 `--delete` 的子字符串模式） | `https://mirror.example.com/repo.git` |
| `--add` | 追加新 URL，而不是替换 | `libra remote set-url --add origin https://mirror.example.com/repo.git` |
| `--delete` | 移除匹配给定子字符串的 URL | `libra remote set-url --delete origin mirror` |
| `--push` | 操作 push URL（`pushurl`）而不是 fetch URL（`url`） | `libra remote set-url --push origin ssh://git@example.com/repo.git` |
| `--all` | 将替换应用到所有匹配条目 | `libra remote set-url --all origin https://new.example.com/repo.git` |

### 子命令：`prune`

删除远程上已不存在的本地远程跟踪分支。

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `<name>` | 远程名称 | `origin` |
| `--dry-run` | 显示会修剪什么，但不删除 | `libra remote prune --dry-run origin` |

### 子命令：`update`

从一个或多个远程 fetch。无参数时优先解析非空的 `remotes.default`（空白分隔的每项可以是远程名或 `remotes.<group>` 名）；未设置或为空时才 fetch 所有配置远程。显式参数使用相同的远程/组解析。

| 标志 / 参数 | 说明 | 示例 |
|-----------------|-------------|---------|
| `-p`, `--prune` | fetch 之后，修剪远端已不存在的远程跟踪分支（对应 Git 的 `remote update -p`） | `libra remote update -p` |
| `[<group> \| <remote>...]` | 要 fetch 的远程或远程组（默认：`remotes.default`，否则全部） | `libra remote update origin upstream` |

> `-p` / `--prune` 运行与 `libra remote prune <name>` 相同的修剪逻辑，但仅在所有 resolved 远端都 fetch 成功后才执行（两段式：先 fetch 全部，再统一 prune，因此后面的远端 fetch 失败不会遗留前面已删除的 ref），并以 `* [pruned] <name>/<branch>` 形式报告被删除的 ref。

### 子命令：`set-branches`

重写远程的 `remote.<name>.fetch` refspec。每个分支写为 `+refs/heads/<branch>:refs/remotes/<name>/<branch>`；`--add` 追加而非替换。后续 `fetch <name>` 与 `remote update` 会消费这些值，只更新已配置分支。

### 子命令：`set-head`

设置或删除 `refs/remotes/<name>/HEAD`。`<branch>` 与 `--auto` 要求对应跟踪分支已经存在；`--auto` 会联系远程解析其默认分支，`--delete` 幂等删除该符号引用。

## 常用命令

```bash
libra remote show
libra remote -v
libra remote add origin https://example.com/repo.git
libra remote get-url origin
libra remote get-url --all origin
libra remote set-url --add origin https://mirror.example.com/repo.git
libra remote set-url --add --push origin ssh://git@example.com/repo.git
libra remote prune --dry-run origin
```

## 人类可读输出

- `remote show` 打印已配置远程名称，每行一个。
- `remote -v` 打印每个 fetch URL 和生效 push URL：

```text
origin  https://example.com/repo.git (fetch)
origin  ssh://git@example.com/repo.git (push)
```

- `remote add` 打印 `Added remote 'origin' -> https://example.com/repo.git`
- `remote remove` 打印 `Removed remote 'origin'`
- `remote rename` 打印 `Renamed remote 'origin' to 'upstream'`
- `remote get-url` 每行打印一个选中的 URL 集合
- `remote set-url` 打印确认信息，说明 URL 是被添加、替换还是删除
- `remote prune` 打印每个被修剪分支和最终摘要；`--dry-run` 使用 `[would prune]`

```text
 * [would prune] origin/stale-feature
 * [would prune] origin/old-experiment

Would prune 2 stale remote-tracking branch(es).
```

## 结构化输出（JSON 示例）

- `--json` 向 `stdout` 写入一个成功信封
- `--machine` 以紧凑单行 JSON 写入相同 schema
- 动作特定 payload 使用 `data.action` 标记

### Action Schemas

- `add`: `name`, `url`
- `remove`: `name`
- `rename`: `old_name`, `new_name`
- `list`: `verbose`, `remotes[]`
- `urls`: `name`, `push`, `all`, `urls[]`
- `set-url`: `name`, `role`, `mode`, `urls[]`, `removed`
- `prune`: `name`, `dry_run`, `stale_branches[]`
- `update`: `remotes[]`（已 fetch 的远程名）、`pruned[]`（每项 `{remote_ref, branch}`；仅在带 `-p`/`--prune` 时出现，无任何修剪时整体省略）

示例（verbose list）：

```json
{
  "ok": true,
  "command": "remote",
  "data": {
    "action": "list",
    "verbose": true,
    "remotes": [
      {
        "name": "origin",
        "fetch_urls": ["https://example.com/repo.git"],
        "push_urls": ["ssh://git@example.com/repo.git"]
      }
    ]
  }
}
```

示例（prune dry-run）：

```json
{
  "ok": true,
  "command": "remote",
  "data": {
    "action": "prune",
    "name": "origin",
    "dry_run": true,
    "stale_branches": [
      {
        "remote_ref": "refs/remotes/origin/stale-feature",
        "branch": "origin/stale-feature"
      }
    ]
  }
}
```

### Schema 说明

- `list.remotes[].fetch_urls` 包含所有已配置 fetch URL
- `list.remotes[].push_urls` 包含生效 push URL；未配置显式 `pushurl` 时回退到 fetch URL
- `prune.stale_branches[].branch` 是面向用户的短名称，例如 `origin/feature`
- `remote show` 当前映射为 `action = "list"` 且 `verbose = false`

## 设计理由

### 为什么使用 SQLite-backed 远程存储？

Git 使用 INI 风格语法将远程配置存储在扁平文件 `.git/config` 中。该格式便于手工编辑，但没有事务保证：写入中途崩溃可能留下截断或损坏文件。Libra 将远程存储在 SQLite（`config` 表）中，提供 ACID 事务、并发读取安全和结构化查询。代理可以用单个 SQL 查询枚举所有远程，而不必解析 INI 语法。代价是远程不能直接用文本编辑器编辑，但 `libra remote` 子命令和 `libra config` 提供完整编程访问。

### 为什么有 `show` 子命令？

Git 重载 `git remote`（无子命令）列出远程名称，`git remote -v` 输出 verbose 信息。Libra 通过 `remote show`（仅名称）和 `remote -v`（带 URL 的 verbose）让列出操作显式。`show` 子命令为需要枚举远程且不解析 verbose URL 输出的代理提供清晰、可发现的入口。它也避免了裸命令含义随标志变化的歧义。

### 为什么支持多 URL？

单个远程可以有多个 fetch URL 和独立 push URL。这支持 mirror-push 工作流（同时推送到 GitHub 和自托管 GitLab）以及 read-from-cache 模式（从本地镜像 fetch，推送到规范远程）。`set-url --add` 和 `set-url --delete` 标志可在不手工编辑配置的情况下管理 URL 列表。`get-url --all` 标志暴露完整 URL 集合以供检查。配置 push URL（`pushurl`）时会优先使用；否则 fetch URL 同时用于 fetch 和 push，匹配 Git 行为。

## 参数对比：Libra vs Git vs jj

| 操作 | Libra | Git | jj |
|-----------|-------|-----|----|
| 列出名称 | `libra remote show` | `git remote` | `jj git remote list` |
| 列出 URL | `libra remote -v` | `git remote -v` | `jj git remote list`（始终 verbose） |
| 添加远程 | `libra remote add <n> <u>` | `git remote add <n> <u>` | `jj git remote add <n> <u>` |
| 添加远程并 fetch | `libra remote add -f <n> <u>` | `git remote add -f <n> <u>` | N/A |
| 添加镜像远程 | `libra remote add --mirror <n> <u>`（仅标记） | `git remote add --mirror=fetch <n> <u>` | N/A |
| 移除远程 | `libra remote remove <n>` | `git remote remove <n>` | `jj git remote remove <n>` |
| 重命名远程 | `libra remote rename <o> <n>` | `git remote rename <o> <n>` | `jj git remote rename <o> <n>` |
| 获取 URL | `libra remote get-url <n>` | `git remote get-url <n>` | N/A |
| 设置 URL | `libra remote set-url <n> <u>` | `git remote set-url <n> <u>` | N/A |
| 添加额外 URL | `libra remote set-url --add <n> <u>` | `git remote set-url --add <n> <u>` | N/A |
| 删除 URL | `libra remote set-url --delete <n> <p>` | `git remote set-url --delete <n> <p>` | N/A |
| Push 专用 URL | get-url/set-url 上的 `--push` 标志 | get-url/set-url 上的 `--push` 标志 | N/A |
| 修剪陈旧 refs | `libra remote prune <n>` | `git remote prune <n>` | 自动 |
| Prune dry-run | `libra remote prune --dry-run <n>` | `git remote prune --dry-run <n>` | N/A |
| 存储后端 | SQLite（事务性） | 扁平文件（.git/config） | TOML + oplog |
| 结构化输出 | `--json` / `--machine` | 无 | 无 |

## 错误处理

| 场景 | StableErrorCode | 退出码 | 提示 |
|----------|-----------------|------|------|
| 重复远程名称 | `LBR-CONFLICT-002` | 128 | "use 'libra remote -v' to inspect configured remotes" |
| 找不到远程 | `LBR-CLI-003` | 129 | "use 'libra remote -v' to inspect configured remotes" |
| 远程没有配置 URL | `LBR-CLI-003` | 129 | "use 'libra remote get-url --all \<name>' to inspect configured URLs" |
| URL 模式未匹配（`set-url --delete`） | `LBR-CLI-003` | 129 | "use 'libra remote get-url --all \<name>' to inspect configured URLs" |
| 无法读取远程配置 | `LBR-IO-001` | 128 | -- |
| 无法更新远程配置 | `LBR-IO-002` | 128 | -- |
| 无法列出远程跟踪分支 | `LBR-IO-001` | 128 | -- |
| 远程跟踪分支损坏 | `LBR-REPO-002` | 128 | -- |
| 无法修剪远程跟踪分支 | `LBR-IO-002` | 128 | -- |
| Prune 期间远程对象格式不匹配 | `LBR-REPO-003` | 128 | "remote uses a different hash algorithm" |
| Prune 期间远程发现 / auth / 网络失败 | 与 fetch 对齐的网络/auth 代码 | 128 | 见 `libra fetch` 错误表 |
