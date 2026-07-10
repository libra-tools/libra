# `libra lfs`

管理二进制和媒体资产的大文件存储。

## 概要

```
libra lfs track [<pattern>...]
libra lfs untrack <path>...
libra lfs locks [--id <ID>] [--path <PATH>] [--limit <N>]
libra lfs lock <path>
libra lfs unlock <path> [--force] [--id <ID>]
libra lfs ls-files [--long] [--size] [--name-only]
```

## 说明

`libra lfs` 提供内置 Large File Storage，用于管理二进制文件、媒体资产和其他不适合 diff 或 merge 的大型对象。LFS 不在仓库中存储完整文件内容，而是用轻量指针文件替换大文件，并将实际内容存储在专用 LFS 服务器上。

`add` 和 `lfs ls-files` 判断路径是否带有 `filter=lfs` 时，会读取 Git/Libra attributes 来源（`core.attributesFile`、逐目录 `.gitattributes`、`.libra_attributes` 和 `.git/info/attributes`）。`track` 和 `untrack` 子命令仍管理根 `.libra_attributes` 文件，作为 Libra 的可写便捷层。文件锁定可防止无法合并的二进制文件被并发编辑，并通过 LFS lock API 在服务端强制执行。

与需要单独安装 `git-lfs` 扩展作为 smudge/clean 过滤器的 Git 不同，Libra 原生集成 LFS。LFS 客户端、指针文件解析和 attributes 管理都内置于 `libra` 二进制文件中。不需要额外安装或过滤器配置。

## 选项

`libra lfs` 没有顶层选项。所有功能都通过下方记录的子命令访问。

## 子命令

### `track`

查看或添加 LFS 跟踪模式到 Libra Attributes。

```bash
# 列出当前跟踪模式
libra lfs track

# 跟踪所有 PNG 文件
libra lfs track "*.png"

# 跟踪多个模式
libra lfs track "*.psd" "*.zip" "assets/**"
```

| 参数 | 说明 |
|----------|-------------|
| `<pattern>...` | 要添加的可选 glob 模式。省略时列出已有跟踪模式。 |

不带参数调用时，会打印每个跟踪模式及其所在 attributes 文件：

```text
Listing tracked patterns
    *.png (.libra_attributes)
    *.psd (.libra_attributes)
```

带模式调用时，会将它们追加到根 `.libra_attributes` 文件；如果文件不存在则创建。

### `untrack`

从 Libra Attributes 中移除 LFS 跟踪模式。

```bash
libra lfs untrack "*.png"
```

| 参数 | 说明 |
|----------|-------------|
| `<path>...` | 要从 `.libra_attributes` 中移除的一个或多个模式。 |

从 attributes 文件中移除指定模式的精确匹配。已经作为 LFS 指针提交的文件会保持指针状态，直到正常重新添加。

### `locks`

列出当前分支上 LFS 服务器中当前锁定的文件。

```bash
# 列出所有锁
libra lfs locks

# 按路径过滤
libra lfs locks --path assets/logo.png

# 按 lock ID 过滤
libra lfs locks --id 12345

# 限制结果
libra lfs locks --limit 10
```

| 标志 | 短选项 | 长选项 | 说明 |
|------|-------|------|-------------|
| ID | `-i` | `--id` | 按 lock ID 过滤。 |
| Path | `-p` | `--path` | 按文件路径过滤。 |
| Limit | `-l` | `--limit` | 返回的最大 lock 数。 |

输出格式：

```text
assets/logo.png    ID:12345
docs/spec.pdf      ID:12346
```

### `lock`

在 LFS 服务器上锁定文件，防止并发编辑。

```bash
libra lfs lock assets/logo.png
```

| 参数 | 说明 |
|----------|-------------|
| `<path>` | 要锁定的文件路径，相对于仓库根。 |

文件必须存在于工作树中。成功时打印 `Locked <path>`。锁定需要对仓库有 push 访问权限。

### `unlock`

在 LFS 服务器上移除文件锁。

```bash
# 按路径解锁
libra lfs unlock assets/logo.png

# 强制解锁（跳过工作树检查）
libra lfs unlock assets/logo.png --force

# 按 ID 解锁
libra lfs unlock assets/logo.png --id 12345
```

| 标志 | 短选项 | 长选项 | 说明 |
|------|-------|------|-------------|
| Force | `-f` | `--force` | 跳过文件存在性和工作树干净性检查。 |
| ID | `-i` | `--id` | 按 lock ID 解锁，而不是从路径查找 ID。 |

没有 `--force` 时，命令会在解锁前验证文件存在且工作树干净。使用 `--force` 时，这些检查会被绕过，适合解锁已删除文件或工作树有意为脏的情况。

### `ls-files`

显示索引中 LFS 跟踪文件的信息。

```bash
# 默认输出（短 OID，指针状态）
libra lfs ls-files

# 显示完整 64 字符 OID
libra lfs ls-files --long

# 包含文件大小
libra lfs ls-files --size

# 只显示文件名
libra lfs ls-files --name-only
```

| 标志 | 短选项 | 长选项 | 说明 |
|------|-------|------|-------------|
| Long | `-l` | `--long` | 显示完整 64 字符 OID，而不是前 10 个字符。 |
| Size | `-s` | `--size` | 在每行末尾括号中显示 LFS 对象大小。 |
| Name only | `-n` | `--name-only` | 只显示已跟踪文件名，不显示 OID 或状态。 |

输出使用 OID 后的 `*` 表示完整（smudged）对象，使用 `-` 表示 LFS 指针：

```text
a1b2c3d4e5 * assets/logo.png
f6g7h8i9j0 - docs/spec.pdf
```

## JSON / Machine 输出

成功的 `track`、`untrack`、`locks`、`lock`、`unlock` 和 `ls-files` 操作支持 `--json` 和 `--machine`。`--json` 向 stdout 写入一个命令信封，`--machine` 以紧凑单行 JSON 输出同一信封。

跟踪模式：

```json
{
  "ok": true,
  "command": "lfs",
  "data": {
    "action": "track",
    "patterns": ["*.png"]
  }
}
```

列出 LFS 文件：

```json
{
  "ok": true,
  "command": "lfs",
  "data": {
    "action": "ls-files",
    "show_size": true,
    "files": [
      {
        "path": "assets/logo.png",
        "oid": "a1b2c3d4e5",
        "marker": "-",
        "size": 1024,
        "display_size": " (1.00 KiB)"
      }
    ]
  }
}
```

Lock 操作包含 `path`、可用时的 `id`、`refspec`，或 `lfs locks` 的 `locks` 数组。

## 常用命令

```bash
# 为常见二进制类型设置 LFS 跟踪
libra lfs track "*.png" "*.jpg" "*.gif" "*.pdf" "*.zip"

# 查看正在跟踪什么
libra lfs track

# 查看所有 LFS 文件及大小
libra lfs ls-files --size

# 编辑前锁定文件
libra lfs lock assets/hero-image.psd

# 查看当前锁
libra lfs locks

# 提交更改后解锁
libra lfs unlock assets/hero-image.psd

# 停止跟踪某个模式
libra lfs untrack "*.gif"
```

## 设计理由

### 为什么内置 LFS 而不是单独扩展？

Git LFS 是一个独立二进制文件，通过 smudge/clean 过滤器和自定义传输代理接入 Git。这种架构有几个痛点：
- **安装摩擦**：每个开发者必须安装 `git-lfs` 并运行 `git lfs install` 来配置过滤器。忘记此步骤会静默地将指针文件作为普通 blob 提交。
- **过滤器误配置**：Smudge/clean 过滤器设置脆弱。`.gitattributes` 拼写错误或缺失过滤器配置会导致 checkout 损坏，出现指针文件而不是内容。
- **传输复杂性**：Git LFS 通过 pre-push hook 和自定义传输协议拦截 `git push`/`git pull`，增加难以调试的失败模式。

Libra 在二进制层面集成 LFS：指针格式、attribute 解析、batch API 客户端和 lock 管理都编译在内。`libra add` 会自动检测 LFS 跟踪模式并创建指针文件。`libra checkout` 会自动将指针 smudge 回完整内容。没有 hooks、没有 filters，也没有单独安装。

### 为什么文件锁定？

二进制文件（PSD、编译资产、大数据集）无法合并。当两个开发者编辑同一二进制文件时，其中一方会在 merge 时丢失工作。文件锁定提供服务端协调：`libra lfs lock` 声明独占编辑权，`libra lfs unlock` 释放它。`locks` 子命令让开发者在开始工作前查看谁锁定了什么。

`unlock` 上的 `--force` 标志是管理员释放陈旧锁的逃生口（例如锁持有者休假或离职）。

### 为什么解锁时检查工作树干净性？

在工作树为脏时解锁文件，可能意味着开发者有未提交的 LFS 更改；如果其他人立即锁定并修改该文件，这些更改可能丢失。干净性检查是提交后再释放锁的安全提醒。`--force` 可绕过此检查，用于脏状态与锁定文件无关的情况。

## 参数对比：Libra vs Git (git-lfs) vs jj

| 参数 | Libra | Git (git-lfs) | jj |
|-----------|-------|---------------|-----|
| 跟踪模式 | `libra lfs track <pattern>` | `git lfs track <pattern>` | 不可用 |
| 取消跟踪模式 | `libra lfs untrack <pattern>` | `git lfs untrack <pattern>` | 不可用 |
| 列出跟踪模式 | `libra lfs track`（无参数） | `git lfs track`（无参数） | 不可用 |
| 列出锁 | `libra lfs locks` | `git lfs locks` | 不可用 |
| 锁定文件 | `libra lfs lock <path>` | `git lfs lock <path>` | 不可用 |
| 解锁文件 | `libra lfs unlock <path>` | `git lfs unlock <path>` | 不可用 |
| 强制解锁 | `--force` | `--force` | 不可用 |
| 列出 LFS 文件 | `libra lfs ls-files` | `git lfs ls-files` | 不可用 |
| 长 OID | `--long` | `--long` | 不可用 |
| 文件大小 | `--size` | `--size` | 不可用 |
| 仅名称 | `--name-only` | `--name-only` | 不可用 |
| 需要安装 | 内置 | 单独安装 `git-lfs` + `git lfs install` | 不可用 |
| Attributes 文件 | 读取 `.gitattributes` + `.libra_attributes`；`track` 写 `.libra_attributes` | `.gitattributes` | 不可用 |
| Filter 配置 | 自动 | 手动（smudge/clean filters） | 不可用 |

注意：jj 当前没有 LFS 支持。jj 仓库中的大文件管理需要通过 jj 的 Git 后端使用 Git 的 LFS 基础设施。

## 错误处理

| 场景 | StableErrorCode | 说明 |
|----------|-----------------|-------------|
| 对不存在路径执行 `lock` | `CliInvalidTarget` | 指定文件不存在于工作树中。 |
| 无 push 权限执行 `lock` | `AuthPermissionDenied` | 用户缺少仓库 push 权限。 |
| 对已锁定文件执行 `lock` | `ConflictOperationBlocked` | 指定路径已有 lock。 |
| 对不存在路径执行 `unlock`（无 `--force`） | `CliInvalidTarget` | 指定文件不存在。 |
| 脏工作树中执行 `unlock`（无 `--force`） | `ConflictOperationBlocked` | 工作树有未提交更改。 |
| 对无 lock 文件执行 `unlock` | `RepoStateInvalid` | 未找到指定路径的 lock。 |
| 无 push 权限执行 `unlock` | `AuthPermissionDenied` | 用户缺少 push 权限。 |
| 无法读取/写入 `.libra_attributes` | IO error | Libra 可写 attributes 文件无法读取或写入。 |
| 无法加载索引 | IO error | 仓库索引损坏或缺失。 |
| LFS 服务器通信失败 | Network error | LFS 服务器返回了非预期状态码。 |
