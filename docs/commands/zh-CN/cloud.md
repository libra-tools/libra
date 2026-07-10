# `libra cloud`

云备份和恢复操作（D1/R2）。

## 概要

```
libra cloud sync [--force] [--batch-size <N>]
libra cloud restore [--repo-id <ID> | --name <NAME>] [--metadata-only]
libra cloud status [--verbose]
```

## 说明

`libra cloud` 使用 Cloudflare D1（serverless SQLite）存储对象索引和元数据，并使用 Cloudflare R2（S3 兼容对象存储）存储 git 对象，从而提供备份和恢复能力。这支持将完整仓库备份到云端，并带有增量同步能力。

同步工作流通过本地 `object_index` 表中的 `is_synced` 标志跟踪已上传对象。选择工作前，sync 会把本地 `.libra/objects` 存储调和进 `object_index`，避免旧 loose 或 packed 对象被跳过。每次默认同步都会选择本地未同步或 D1 中缺失的对象，因此重复同步很高效，同时仍能在 D1 数据库变化后修复陈旧的本地同步标志。`--force` 标志允许重新同步所有已索引的本地对象，也是 R2 bucket 侧数据丢失后的恢复路径。对象同步完成后，仓库元数据（references/branches）会序列化为 JSON 并上传到 R2，并通过内容哈希检查避免不必要上传。

每个仓库由 UUID（`libra.repoid` 配置键）标识，并可选一个人类可读项目名（`cloud.name` 配置键或目录名）。项目名注册在 D1 `repositories` 表中，用于恢复时查找。

恢复可以用 UUID（`--repo-id`）或项目名（`--name`）定位仓库。它会从 D1 下载对象索引，可选地从 R2 下载对象，恢复元数据（references），并从 HEAD 填充工作目录。

## 全局配置 Schema 保护

`libra cloud` 在信任远端 / tiered 对象存储设置前，会读取全局存储配置（`~/.libra/config.db`，或 `LIBRA_CONFIG_GLOBAL_DB` 指定的路径）。如果该数据库的 schema 版本比当前二进制支持的版本更新，cloud 命令会以 `LBR-CONFIG-001` fail-closed，而不是静默忽略全局存储配置并回退到本地对象。诊断会包含二进制路径和版本、配置 DB 路径、schema 版本，以及升级命令：
`curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh`。

只有在明确希望本地对象访问时，才使用 `libra --offline cloud ...` 或 `LIBRA_READ_POLICY=offline|local libra cloud ...`。Libra 会告警一次，并在本次运行中忽略全局存储配置。

## 选项

### 子命令：`sync`

将本地仓库同步到云端。把对象上传到 R2，并把索引写入 D1。

| 标志 | 说明 |
|------|------|
| `--force` | 同步所有已索引的本地对象，不考虑本地/D1 同步状态。适用于有意重新 upsert 每个对象，或在 R2 bucket 侧数据丢失后恢复。 |
| `--batch-size <N>` | 每批处理的对象数。默认：`50`。必须至少为 1。较小批次会产生更频繁的进度输出；较大批次会减少开销。 |

```bash
# 增量修复同步
libra cloud sync

# 强制重新同步全部内容
libra cloud sync --force

# 使用较小批次获得更详细进度
libra cloud sync --batch-size 10
```

### 子命令：`restore`

从云端恢复仓库。下载 D1 中的对象索引、R2 中的对象，并恢复元数据和工作目录。

| 标志 | 说明 |
|------|------|
| `--repo-id <ID>` | 要恢复的仓库 UUID。与 `--name` 互斥。`--repo-id` 和 `--name` 必须提供一个。 |
| `--name <NAME>` | 要恢复的人类可读项目名。在 D1 `repositories` 表中查找。与 `--repo-id` 互斥。 |
| `--metadata-only` | 只把对象索引恢复到本地数据库。不从 R2 下载对象，也不恢复工作目录。适合在完整恢复前检查仓库包含什么。 |

```bash
# 按仓库 ID 恢复
libra cloud restore --repo-id a1b2c3d4-e5f6-7890-abcd-ef1234567890

# 按项目名恢复
libra cloud restore --name my-project

# 只恢复元数据（对象索引）
libra cloud restore --name my-project --metadata-only
```

### 子命令：`status`

显示当前仓库的云同步状态。

| 标志 | 说明 |
|------|------|
| `--verbose` | 显示单个未同步对象的详情（最多 20 个）。 |

```bash
# 显示同步状态摘要
libra cloud status

# 显示带未同步对象列表的详细状态
libra cloud status --verbose
```

## 常用命令

```bash
# 首次同步到云端
libra cloud sync

# 检查同步进度
libra cloud status

# 显示待处理对象的详细状态
libra cloud status --verbose

# 失败后强制重新同步
libra cloud sync --force

# 在新目录中按名称恢复仓库
libra init
libra cloud restore --name my-project

# 不下载对象，预览会恢复什么
libra cloud restore --name my-project --metadata-only
```

## 人工输出

**`cloud sync`**（有对象需要同步）：

```text
Starting cloud sync...
Found 42 objects to sync.
Progress: 42/42 synced, 0 failed
Sync complete: 42 synced, 0 failed
Syncing metadata...
Metadata synced (3 references).
```

**`cloud sync`**（没有需要同步的对象）：

```text
Starting cloud sync...
No objects to sync.
Syncing metadata...
Metadata unchanged, skipping upload.
```

**`cloud restore`**：

```text
Starting restore for repo: a1b2c3d4-e5f6-7890-abcd-ef1234567890
Found 42 objects in cloud for repo.
Restored 42 object indexes to local database.
Restore complete: 38 downloaded, 4 skipped (already exist), 0 failed
Restoring metadata...
Metadata restored.
Restoring working directory to HEAD (abc1234)
Successfully restored working directory files.
```

**`cloud restore --metadata-only`**：

```text
Starting restore for repo: a1b2c3d4-e5f6-7890-abcd-ef1234567890
Found 42 objects in cloud for repo.
Restored 42 object indexes to local database.
Metadata-only restore complete.
```

**`cloud status`**：

```text
Cloud Sync Status:
  Repo ID:       a1b2c3d4-e5f6-7890-abcd-ef1234567890
  Total objects: 42
  Synced:        40 (95%)
  Pending:       2

By object type:
  blob: 30/32 synced
  tree: 8/8 synced
  commit: 2/2 synced
```

**`cloud status --verbose`**：

```text
Cloud Sync Status:
  Repo ID:       a1b2c3d4-e5f6-7890-abcd-ef1234567890
  Total objects: 42
  Synced:        40 (95%)
  Pending:       2

By object type:
  blob: 30/32 synced
  tree: 8/8 synced
  commit: 2/2 synced

Unsynced objects:
  abc123def456... (blob, 1024 bytes)
  789012abc345... (blob, 512 bytes)
```

## 结构化输出

`cloud status` 和 `cloud sync` 支持 `--json` 与 `--machine`。
`--json` 输出命令信封，`--machine` 以单行 NDJSON 输出相同信封。

```json
{
  "ok": true,
  "command": "cloud.status",
  "data": {
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "total_objects": 42,
    "synced": 40,
    "pending": 2,
    "synced_percent": 95,
    "by_type": [
      {
        "object_type": "blob",
        "total": 32,
        "synced": 30,
        "pending": 2
      }
    ]
  }
}
```

设置 `--verbose` 时，status payload 还会包含最多 20 个 `unsynced_objects` 条目，每个条目带 `oid`、`object_type` 和 `size`。

成功同步时，`cloud sync --json` / `--machine` 输出 `cloud.sync`：

```json
{
  "ok": true,
  "command": "cloud.sync",
  "data": {
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "project_name": "my-project",
    "total_unsynced": 42,
    "synced_count": 42,
    "failed_count": 0,
    "metadata": {
      "status": "synced",
      "references": 3
    },
    "agent_capture": {
      "status": "completed",
      "sessions_synced": 2,
      "sessions_failed": 0,
      "checkpoints_synced": 6,
      "checkpoints_failed": 0
    }
  }
}
```

成功恢复时，`cloud restore --json` / `--machine` 输出 `cloud.restore`：

```json
{
  "ok": true,
  "command": "cloud.restore",
  "data": {
    "repo_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "metadata_only": false,
    "total_objects": 42,
    "indexes_restored": 42,
    "object_restore": {
      "downloaded": 30,
      "skipped": 12,
      "failed": 0
    },
    "metadata": {
      "status": "restored"
    },
    "agent_capture": {
      "status": "restored"
    }
  }
}
```

对于 `cloud restore --metadata-only`，payload 保留 `metadata_only: true`，并省略 `object_restore`。

`cloud sync --progress=json` 向 stderr 输出 NDJSON 进度事件（stdout 上没有旧的人类进度文本）。事件名覆盖对象、元数据和 agent-capture 阶段，例如：

```json
{"event":"cloud_sync.start"}
{"event":"cloud_sync.objects.total","total":42}
{"event":"cloud_sync.objects.progress","synced":42,"total":42,"failed":0}
{"event":"cloud_sync.metadata.synced","references":3}
{"event":"cloud_sync.agent_capture.complete","sessions_synced":2,"sessions_failed":0,"checkpoints_synced":6,"checkpoints_failed":0}
```

`cloud sync` 默认模式仍使用旧的人类进度输出。`cloud restore` 和 `cloud sync` 的失败继续通过 Libra 的标准 CLI 错误机制处理。

## 环境变量

云操作需要以下密钥。Libra 先读取仓库本地 `vault.env.*` 条目，再读取全局 `vault.env.*`，最后读取匹配的环境变量。如果某个必需键在所有层级都缺失，命令会报告该键，并要求你在重试前配置它。

### D1（所有操作必需）

| 键 | 说明 |
|----|------|
| `LIBRA_D1_ACCOUNT_ID` | Cloudflare 账号 ID |
| `LIBRA_D1_API_TOKEN` | 具有 D1 访问权限的 Cloudflare API token |
| `LIBRA_D1_DATABASE_ID` | D1 数据库 UUID |

### R2（sync 和完整 restore 必需）

| 键 | 说明 |
|----|------|
| `LIBRA_STORAGE_ENDPOINT` | S3 兼容 endpoint URL |
| `LIBRA_STORAGE_BUCKET` | Bucket 名称 |
| `LIBRA_STORAGE_ACCESS_KEY` | Access key ID |
| `LIBRA_STORAGE_SECRET_KEY` | Secret access key |
| `LIBRA_STORAGE_REGION` | 区域（默认 `auto`） |

注意：对 `restore` 使用 `--metadata-only` 时，只需要 D1 变量。

## 设计动机

### 为什么特定选择 D1/R2？

Libra 出于几个原因面向 Cloudflare 生态。D1 提供 serverless SQLite，与 Libra 本地基于 SQLite 的架构一致：相同查询模式和数据模型可以同时用于本地和云端。R2 提供 S3 兼容对象存储且没有 egress 费用，这对对象经常被下载的 VCS 很关键。二者结合提供了完全 serverless、无需管理基础设施的备份后端。

### 为什么不用通用云存储？

Libra 已经通过 `LIBRA_STORAGE_*` 环境变量为分层对象缓存提供通用 S3 兼容存储支持。`cloud` 命令用途不同：它负责完整仓库备份，包括元数据（references、HEAD、config）。这需要用于对象索引的结构化数据库（D1），而不只是 blob store。通用后端需要在每种存储 provider 之上实现元数据层，增加复杂度且收益不明确。需要备份到其他 provider 的用户可以改用对象级存储分层。

### 为什么有 `batch-size` 参数？

对象同步需要为每个对象上传到 R2，然后在 D1 中建立索引。对于拥有数千对象的大型仓库，这可能需要很长时间。`--batch-size` 参数控制打印一次进度报告前处理多少对象。较小批次反馈更及时；较大批次减少每批开销。默认 50 在两者之间取得平衡。允许批次大小为 1，以便调试时获得最大粒度。

### 为什么 `--repo-id` 和 `--name` 互斥？

仓库 UUID 稳定且无歧义，但不便于人类使用。项目名便于人类使用，但可能冲突或被重命名。将它们设为互斥并要求提供一个，确保用户明确选择查找策略。UUID 存在本地配置（`libra.repoid`）中，是权威标识；名称是存储在 D1 `repositories` 表中的便利别名。

### 为什么 restore 会尝试填充工作目录？

裸对象恢复（索引 + 对象）会让仓库处于对象存储中已有文件、但工作目录为空的状态。对大多数用户而言，恢复的目标是回到可工作的状态。Libra 在恢复对象后会自动检出 HEAD（或用 `main` 分支作为 fallback）。这符合用户预期，也避免额外手动步骤。`--metadata-only` 标志会为只需要索引的用户跳过这一步。

## 参数对比：Libra vs Git vs jj

| 操作 | Libra | Git | jj |
|------|-------|-----|----|
| 同步到云端 | `cloud sync` | N/A（使用 `push` 到远程） | N/A（使用 `push` 到远程） |
| 强制同步 | `cloud sync --force` | N/A | N/A |
| 批次大小 | `cloud sync --batch-size <N>` | N/A | N/A |
| 从云端恢复 | `cloud restore --name <N>` | `clone <url>` | `git clone <url>` |
| 按 ID 恢复 | `cloud restore --repo-id <ID>` | N/A | N/A |
| 只恢复元数据 | `cloud restore --metadata-only` | N/A | N/A |
| 同步状态 | `cloud status` | N/A | N/A |
| 详细状态 | `cloud status --verbose` | N/A | N/A |
| 后端 | Cloudflare D1 + R2 | Git remotes（SSH/HTTPS） | Git remotes（SSH/HTTPS） |
| 增量同步 | 自动（is_synced 标志） | 自动（pack negotiation） | 自动（通过 Git） |
| 对象校验 | 恢复时哈希检查 | 传输时哈希检查 | 传输时哈希检查 |
| 元数据备份 | 自动（references JSON） | 包含在 push/fetch 中 | 包含在 push/fetch 中 |

注意：Git 和 jj 都没有内置云备份命令。它们依赖推送到远程仓库进行备份和协作。Libra 的 `cloud` 命令填补了不同空位：无需 Git 服务器，即可将完整仓库状态（包括本地分支、config 和对象索引）备份到 serverless 云后端。

## 错误处理

| 代码 | 条件 |
|------|------|
| `LBR-REPO-001` | 不是 libra 仓库 |
| `LBR-CLI-002` | 缺少必需 Vault/env 凭据键（会列出缺失键） |
| `LBR-CLI-002` | Batch size 必须至少为 1 |
| `LBR-CLI-002` | restore 未提供 `--repo-id` 或 `--name` |
| `LBR-CLI-003` | D1 中未找到给定名称的仓库 |
| `LBR-CONFLICT-002` | 项目名已被另一个仓库占用 |
| `LBR-IO-001` | D1 client 初始化失败 |
| `LBR-IO-001` | 创建 D1 表失败 |
| `LBR-IO-001` | 数据库查询失败 |
| `LBR-IO-002` | R2 上传失败 |
| `LBR-IO-002` | R2 下载失败 |
| `LBR-IO-002` | 恢复对象哈希不匹配 |
| `LBR-IO-002` | 保存恢复对象到本地存储失败 |
| `LBR-IO-002` | 元数据同步/恢复失败 |
