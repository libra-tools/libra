# Libra 命令参考

本目录包含所有 Libra CLI 命令的详细文档。每份文档都包含概要、选项参考、人工输出和结构化（JSON）输出示例、设计动机，以及与 Git 和 jj 的参数对比。

## 全局标志

每个 Libra 命令都接受以下全局标志：

| 标志 | 短参数 | 说明 |
|------|--------|------|
| `--json` | `-J` | 输出 JSON（格式：`pretty`、`compact`、`ndjson`） |
| `--machine` | | 严格机器模式（隐含 `--json=ndjson --no-pager --color=never --quiet`） |
| `--no-pager` | | 禁用分页器（`less`） |
| `--color` | | 何时使用颜色（`auto`、`never`、`always`） |
| `--no-color` | | 禁用颜色；等价于 `--color=never` |
| `--quiet` | `-q` | 抑制 stdout |
| `--exit-code-on-warning` | | 出现警告时返回退出码 9 |
| `--progress` | | 控制进度输出（`json`、`text`、`none`、`auto`） |

## 命令索引

### 仓库设置

| 命令 | 别名 | 说明 | 文档 |
|------|------|------|------|
| `libra init` | | 创建新的 Libra 仓库，带 SQLite 元数据、vault 签名和可选 Git 导入 | [init.md](init.md) |
| `libra clone` | | 克隆远程仓库，支持 vault 引导、浅克隆和单分支 | [clone.md](clone.md) |
| `libra config` | `cfg` | 管理仓库本地和用户全局配置，并用 vault 加密 secret | [config.md](config.md) |
| `libra completions` | | 从实时 CLI 生成 shell completion 脚本（`bash`/`zsh`/`fish`/`powershell`/`elvish`） | [completions.md](completions.md) |

### 暂存与工作树

| 命令 | 别名 | 说明 | 文档 |
|------|------|------|------|
| `libra add` | | 将工作树文件更改暂存到索引 | [add.md](add.md) |
| `libra rm` | `remove`、`delete` | 从工作树和/或索引移除文件 | [rm.md](rm.md) |
| `libra mv` | | 移动或重命名文件、目录或符号链接 | [mv.md](mv.md) |
| `libra restore` | `unstage` | 恢复工作树文件，或从索引取消暂存 | [restore.md](restore.md) |
| `libra clean` | | 从工作树移除未跟踪文件（要求 `-n` 或 `-f`） | [clean.md](clean.md) |
| `libra stash` | | 用 push/pop/list/apply/drop 子命令保存和恢复临时更改 | [stash.md](stash.md) |
| `libra status` | `st` | 显示工作树、暂存区和上游跟踪状态 | [status.md](status.md) |
| `libra dirty` | | status 缓存的咨询式 dirty-set 标记（Libra 扩展） | [dirty.md](dirty.md) |
| `libra revision` | | first-parent chains 上的 revision ordinal index（Libra 扩展） | [revision.md](revision.md) |
| `libra commit-tree` | `git commit-tree` | 从 tree 创建 commit 对象（plumbing） | [commit-tree.md](commit-tree.md) |
| `libra auth` | | 按 host 作用域管理 HTTP token auth（Libra 扩展） | [auth.md](auth.md) |
| `libra service` | | 无头本地服务：notification bus + dirty-mark ingestion（Libra 扩展） | [service.md](service.md) |

### 提交与历史

| 命令 | 别名 | 说明 | 文档 |
|------|------|------|------|
| `libra commit` | `ci` | 将已暂存更改记录为新提交，支持可选 vault 签名和 conventional 格式 | [commit.md](commit.md) |
| `libra log` | `hist`、`history` | 显示提交历史，支持图形、补丁、统计和自定义格式 | [log.md](log.md) |
| `libra logfile` | | 检查 tracing log-file 配置（路径、rotation、filter、size） | [logfile.md](logfile.md) |
| `libra shortlog` | `slog` | 按作者汇总可达提交 | [shortlog.md](shortlog.md) |
| `libra show` | | 显示提交、标签、树、blob 或 `REV:path` 内容 | [show.md](show.md) |
| `libra diff` | | 比较 HEAD、索引、工作树或两个 revisions 之间的差异 | [diff.md](diff.md) |
| `libra diff-tree` | | 比较两个 trees（git diff-tree） | [diff-tree.md](diff-tree.md) |
| `libra diff-index` | | 比较一个 tree 与工作树（git diff-index） | [diff-index.md](diff-index.md) |
| `libra diff-files` | | 比较 index 与工作树（git diff-files） | [diff-files.md](diff-files.md) |
| `libra fast-export` | | 把历史导出为 git fast-import 流 | [fast-export.md](fast-export.md) |
| `libra fast-import` | | 导入 git fast-import 流 | [fast-import.md](fast-import.md) |
| `libra blame` | | 将文件每一行追溯到引入它的提交 | [blame.md](blame.md) |
| `libra describe` | `desc` | 找到最近的可达标签，并格式化为 `tag-N-g<abbrev>` | [describe.md](describe.md) |
| `libra grep` | | 在已跟踪文件中搜索模式，支持正则、revision 和 index | [grep.md](grep.md) |
| `libra reflog` | | 查看、删除或检查引用变更日志是否存在 | [reflog.md](reflog.md) |
| `libra rev-list` | | 列出从 revision 可达的 commit objects | [rev-list.md](rev-list.md) |
| `libra rev-parse` | | 解析 revision names、缩写 refs 并打印仓库路径 | [rev-parse.md](rev-parse.md) |

### 分支与导航

| 命令 | 别名 | 说明 | 文档 |
|------|------|------|------|
| `libra branch` | `br` | 创建、删除、重命名、列出和检查分支 | [branch.md](branch.md) |
| `libra metadata` | | Branch/repo metadata key-value store（protect/archive/lineage foundation） | [metadata.md](metadata.md) |
| `libra tag` | | 创建、列出或删除轻量标签和附注标签 | [tag.md](tag.md) |
| `libra switch` | `sw` | 切换分支、创建新分支或分离 HEAD，并提供模糊建议 | [switch.md](switch.md) |
| `libra checkout` | | 分支兼容表面和显式 `--` 路径恢复别名；优先使用 `switch` / `restore` | [checkout.md](checkout.md) |

### 历史操作

| 命令 | 别名 | 说明 | 文档 |
|------|------|------|------|
| `libra reset` | | 移动 HEAD，并可选择重置索引或工作目录 | [reset.md](reset.md) |
| `libra merge` | | 将分支快进合并到当前分支 | [merge.md](merge.md) |
| `libra merge-file` | | 对三个文件做三路合并（git merge-file） | [merge-file.md](merge-file.md) |
| `libra merge-base` | | 查找两个提交的最佳共同祖先 | [merge-base.md](merge-base.md) |
| `libra rebase` | `rb` | 在另一个基底 tip 上重新应用提交，并支持冲突解决 | [rebase.md](rebase.md) |
| `libra cherry-pick` | `cp` | 将已有提交的更改应用到当前分支 | [cherry-pick.md](cherry-pick.md) |
| `libra revert` | | 创建新提交以撤销指定提交的更改 | [revert.md](revert.md) |
| `libra replace` | | 在读取时用另一个对象替换它（refs/replace） | [replace.md](replace.md) |
| `libra rerere` | | 复用已记录的冲突解决 | [rerere.md](rerere.md) |
| `libra bisect` | | 用二分搜索找到引入 bug 的提交；支持 `start` / `bad` / `good` / `reset` / `skip` / `log` / `run` / `view` | [bisect.md](bisect.md) |
| `libra bundle` | | 创建与检查 Git v2 bundle 文件（`create` / `verify` / `list-heads`） | [bundle.md](bundle.md) |

### 远程操作

| 命令 | 别名 | 说明 | 文档 |
|------|------|------|------|
| `libra remote` | | 管理远程仓库：添加、移除、重命名、检查 URL、清理陈旧 refs | [remote.md](remote.md) |
| `libra fetch` | | 从一个或所有 remotes 下载对象并更新 remote-tracking refs | [fetch.md](fetch.md) |
| `libra ls-remote` | | 不获取对象，列出远程仓库公布的 refs | [ls-remote.md](ls-remote.md) |
| `libra push` | | 将本地 commits 和对象发送到远端，集成 LFS | [push.md](push.md) |
| `libra pull` | | Fetch 并快进 merge 到当前分支 | [pull.md](pull.md) |
| `libra open` | | 在系统浏览器中打开仓库 remote URL | [open.md](open.md) |
| `libra lfs` | | 管理 Large File Storage：track、lock、unlock、列出 LFS 文件 | [lfs.md](lfs.md) |
| `libra credential` | | Vault-backed Git credential helper（fill/store/erase） | [credential.md](credential.md) |

### 云与存储

| 命令 | 别名 | 说明 | 文档 |
|------|------|------|------|
| `libra cloud` | | 通过 Cloudflare D1/R2 执行云备份和恢复操作 | [cloud.md](cloud.md) |
| `libra cache` | | 检查 tiered-storage / LRU cache 配置（type、threshold、budget） | [cache.md](cache.md) |
| `libra publish` | | 管理只读 Cloudflare Worker 发布 | [publish.md](publish.md) |
| `libra worktree` | `wt` | 管理附加到仓库的多个工作树 | [worktree.md](worktree.md) |

### AI 与开发

| 命令 | 别名 | 说明 | 文档 |
|------|------|------|------|
| `libra code` | | 带 AI agent、Web 服务器和 MCP 集成的交互式 TUI | [code.md](code.md) |
| `libra code-control` | | 驱动本地 Libra Code TUI 自动化控制会话 | [code-control.md](code-control.md) |
| Codex data storage | | 将 `libra code --provider codex` 连接到 Codex app-server，并持久化 Codex 会话数据 | [codex-data-storage.md](codex-data-storage.md) |
| `libra automation` | | 列出、运行和检查 AI automation rules | [automation.md](automation.md) |
| `libra usage` | | 报告并修剪 AI provider/model 使用聚合 | [usage.md](usage.md) |
| `libra graph` | | 在专用 TUI 中检查 Libra Code thread version graph | [graph.md](graph.md) |
| `libra sandbox` | | 检查 AI sandbox diagnostics，包括 OS backend 可用性和 downgrade warnings | [sandbox.md](sandbox.md) |
| `libra agent` | | 管理外部 agent 捕获、checkpoints、hooks 和 RPC adapters | [agent.md](agent.md) |

### 底层与检查

| 命令 | 别名 | 说明 | 文档 |
|------|------|------|------|
| `libra apply` | | 检查 unified-diff patch 能否应用（`--check`） | [apply.md](apply.md) |
| `libra cat-file` | | 按类型、大小或漂亮打印内容检查 Git objects 和 AI objects | [cat-file.md](cat-file.md) |
| `libra check-attr` | | 报告 pathnames 的 `.libra_attributes` 属性（例如 `filter`） | [check-attr.md](check-attr.md) |
| `libra check-mailmap` | | 通过 `.mailmap` 解析 `Name <email>` contacts | [check-mailmap.md](check-mailmap.md) |
| `libra check-ignore` | | 报告哪些 pathnames 被 `.libraignore` rules 排除 | [check-ignore.md](check-ignore.md) |
| `libra fsck` | | 校验 Libra 仓库中 objects、refs 和 index 的完整性 | [fsck.md](fsck.md) |
| `libra hash-object` | | 从文件或标准输入计算 Git-compatible blob object IDs | [hash-object.md](hash-object.md) |
| `libra write-tree` | | 把当前 index 写成一个 tree object | [write-tree.md](write-tree.md) |
| `libra read-tree` | | 把一个 tree object 读入 index（仅 index） | [read-tree.md](read-tree.md) |
| `libra update-index` | | 直接修改 index（add/remove/cacheinfo） | [update-index.md](update-index.md) |
| `libra update-ref` | | 安全地更新、创建或删除 refs/heads/<branch> ref | [update-ref.md](update-ref.md) |
| `libra verify-pack` | | 对照 pack archives 验证 pack index files | [verify-pack.md](verify-pack.md) |
| `libra show-ref` | | 列出本地 refs（branches、tags、HEAD）及其 object IDs | [show-ref.md](show-ref.md) |
| `libra symbolic-ref` | | 读取或更新 symbolic HEAD ref | [symbolic-ref.md](symbolic-ref.md) |
| `libra index-pack` | | 为现有 `.pack` archive 构建 `.idx` pack index 文件（隐藏） | [index-pack.md](index-pack.md) |
| `libra hooks` | | 外部 AI agent（Claude Code / Gemini）hook 入口；由 `libra agent enable` 安装的配置调用（隐藏） | [hooks.md](hooks.md) |

## 结构化输出信封

所有支持 `--json` / `--machine` 的命令都会返回一致的 JSON 信封：

```json
{
  "ok": true,
  "command": "<command-name>",
  "data": { ... }
}
```

出错时：

```json
{
  "ok": false,
  "command": "<command-name>",
  "error": {
    "code": "LBR-XXX-NNN",
    "message": "Human-readable error description",
    "hint": "Suggested fix or next step"
  }
}
```

## 错误码命名空间

| 前缀 | 领域 |
|------|------|
| `LBR-REPO-*` | 仓库状态错误（不是仓库、对象损坏、引用缺失） |
| `LBR-CLI-*` | CLI 参数校验错误（无效标志、缺少必需参数） |
| `LBR-NET-*` | 网络和传输错误（认证失败、超时、DNS） |
| `LBR-FS-*` | 文件系统错误（权限拒绝、磁盘已满、路径编码） |
| `LBR-IDX-*` | 索引/暂存区错误（索引损坏、锁竞争） |
| `LBR-OBJ-*` | 对象存储错误（对象缺失、哈希不匹配） |
| `LBR-VAULT-*` | Vault 和加密错误（解封失败、密钥生成） |

## 设计理念

Libra 的命令行接口基于以下原则设计：

1. **在合理处保持 Git 兼容** — 大多数命令复用 Git 的标志名和行为，让既有肌肉记忆可以直接迁移。
2. **结构化输出是一等能力** — `--json` 和 `--machine` 是全局标志，并且随着每个命令表面现代化逐步启用结构化输出。各命令页面会记录当前稳定的机器可读契约。
3. **SQLite 优先于扁平文件** — Refs、config 和 metadata 存储在 SQLite 中，以获得事务一致性和原子更新。
4. **默认安全** — 默认启用 vault-backed signing 和 secret encryption，而不是要求用户显式选择。
5. **显式优先于隐式** — `clean` 等命令要求 `-f` 或 `-n`；`status --exit-code` 是显式 opt-in，而不是 Git 中含糊的退出码行为。
6. **可操作的错误** — 每个错误都包含稳定代码（`LBR-*`）、人类可读消息和解决提示。
7. **AI 原生开发** — `libra code` 命令将 AI agents 直接集成到版本控制工作流，并支持多 provider 和 MCP 协议。
8. **云原生存储** — 内置分层存储（S3/R2）和云备份（D1/R2），服务分布式 monorepo 工作流。
