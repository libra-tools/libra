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
{"event":"cloud_sync.agent_capture.complete","sessions_synced":2,"sessions_failed":0,"checkpoints_synced":6,"checkpoints_failed":0,"subagent_rows_synced":3,"subagent_rows_failed":0}
```

完成计数只包含本次增量同步实际发送的行；subagent companion 计数覆盖耐久 source claim、
append-only source revision 与 boundary/content link 行。agent catalog 发布先取得一个短时本地 SQLite snapshot，在有界对象存储
扫描前释放 rollback-journal 读锁，再要求第二个 catalog snapshot 与已完成的
`object_index.is_synced` 代际完全相同；并发 capture 会让该阶段重试，而不会阻塞 hook/import
提交或先暴露缺少对象的 catalog 行。普通路径的 capture 阶段只查询 checkpoint 可达 OID 的 object-index
行，因此无关的大型仓库对象历史不会消耗 100,000 行 capture 安全上限。远端按有界分页读取，只把缺失或严格更新的代际用有界多行请求
发送。session、checkpoint、可变 claim 与 link 的先后关系使用事务内递增的显式 sync revision，而不是墙钟时间。这些计数器只在各 clone 内单调，因此 Libra 还会记录最近一次成功 sync 或 restore 对应的精确远端 completed generation；已有可变行只有在该 generation 确实是本地已知祖先时才能前进，否则 sync 会 fail-closed 并要求先 restore。进入 `publishing` 的转换也会对同一 generation 执行 compare-and-swap。checkpoint 在 prune 重写 retained traces/tree，或已验证的 doctor repair 修正 tree/metadata/traces OID 时推进代次；其它 immutable 字段必须一致，因此旧 clone 不能写回修复前的行。
全部远端冲突和对象耐久性预检通过后、第一次远端 capture-catalog mutation 前，才会把 generation manifest 标成
`publishing`；活跃 writer 保留 fence，若进程崩溃，则服务端时间戳的五分钟发布租约到期后，后续 sync 可原子接管。每个 batch 都受唯一 writer
token fence，完整 companion 图重新读取并验证后才会标成 `complete`。因此中断的发布可由
下一次 sync 安全续传或接管。manifest 同时绑定 fenced traces head、object-index catalog
generation 与 **checkpoint 可达 object-index 投影**的 canonical digest/count；capture 阶段既不下载也不计数无关的大型 Git 历史，只用有界批次
查询所需 OID，并以固定的 32 对象并发页在完成前重算 R2 payload 的内容 hash；缺失或损坏的 payload 会由已验证的本地对象
覆盖并回读，之后 manifest 才可完成。不可变 revision 冲突、同代 checkpoint/claim/link 分歧均
fail-closed；revision 与 link 依赖先发布，单调 claim high-water 最后发布。每个 D1 请求
有 30 秒 timeout，整个 agent-capture 阶段有 120 秒 deadline。任意 mirror 失败都会让
`cloud sync` 非零退出，machine 输出也不会先发一个成功 envelope。

普通 `agent clean` checkpoint retention 会在 ref/catalog 重写的同一本地事务写入耐久 prune
tombstone。cloud sync 先发布该 fence，再按 current claim、revision、link、checkpoint 的可续传
顺序删除；每个中断边界仍是合法 publishing 状态，回退后的本地 claim 会由后续单调 claim batch
重建。D1
拒绝随后对已 tombstone identity 的 checkpoint 写入，因此旧 clone 无法复活它。session erase
刻意不创建这种普通 prune fence。restore 会在下载任何对象或应用通用 refs metadata **之前**，
用一个稳定的 completed 远端代次检查本地普通 prune tombstone；验证 capture 所有权期间，通用
metadata 会暂缓 `traces` ref。completed generation 会安装其 fenced ref；若 generation 与 capture
行都不存在，则应用暂缓的 legacy metadata ref，保证 pre-manifest traces 仍可达。存在无 manifest
的 capture 行时会要求先用当前版本 sync，不会在冲突所有权之间猜测。若上一份 completed 远端代次仍包含其中的 checkpoint，restore 会拒绝并提示
先运行 cloud sync，而不会复活该 checkpoint 的对象、ref、catalog 或 companion 行。若 D1 是因为 session erase
而保留一个本地不存在且无 fence 的 checkpoint，
sync 会在启动新 capture generation 前失败，并保留上一份 completed 远端快照；这与已记录的
跨设备 erase deferred 语义一致。

sync 会拒绝发布 restore 无法读取的 generation：session、checkpoint、prune tombstone、claim、revision、link 与 fenced object-index 投影共享同一个
100,000 行聚合安全预算；restore 对其消费的行采用同样的聚合上限。capture 范围的完整 object-index 读取采用 keyset 分页，并在前后核对由
trigger 维护的 catalog generation。普通全仓库 object restore 仍采用分页，但不受 capture catalog
的 100,000 行上限约束。超过共享 capture 上限会给出可操作错误，而不会把无界远端输入留在内存。
分页期间发生 insert/update/delete 会使本次读取失效并最多重试三次，避免边界行重复或遗漏。
一次性 v2 adoption 会先清理旧 best-effort mirror 留下的无 session checkpoint orphan，再执行
严格依赖校验；当前本地的 session/checkpoint 对随后可一致发布。

restore 会在全部有界分页前后读取同一个 completed manifest（代际变化时最多重试三次），
先预检完整 companion 代际，再在一个本地 SQLite transaction 内依次应用
session、checkpoint、skeleton claim、revision、link 与最终 claim 前推。冲突或 FK/写入
错误会保持原有本地代际不变；更新的本地 session/checkpoint/link 不会倒退；较新的 checkpoint
代次只能更新 prune 或已验证 doctor repair 会重写的 traces/tree/metadata 字段。空 checkpoint
catalog 只有在 fenced traces head 也为空时才合法；restore 会拒绝没有对应 catalog 行的崩溃窗口 head。
没有 completed manifest 的旧远端 capture 行必须先用当前版本执行一次 `libra cloud sync`
完成接管；没有任何 capture 行的旧远端仍是有效的纯 Git 备份，restore 会跳过这个可选层。
restore 只读探测远端 capture schema，不安装写屏障也不收养行，因此失败/只读 restore
不会禁用旧客户端写入。当前客户端使用版本化的 v2 远端 session/checkpoint 表；D1 会先安装 legacy
写屏障，再取得一次性的 generation-0 adoption 快照，因此旧客户端无法在复制与完成标记
之间竞态写入。无 fence 的单行 v2 写入也会 fail closed；当前发布只走带 writer token
的批量写入。restore 会从 generation-stable snapshot 读取 object index，并拒绝 fenced
投影已不匹配 manifest digest 的 catalog 状态；后续无关 object-index 新增不会使未变化的
checkpoint 投影失效。它从 manifest 绑定的 traces head
验证可达性，并把该 head 与 catalog 原子恢复，不信任独立上传的通用 ref metadata。已有本地对象会被解码并校验 hash；损坏路径会原子重下，并在
catalog transaction 开始前验证完整 traces 链及每个 checkpoint tree/blob。session 擦除 tombstone
现已传播：`cloud sync` 在同一 generation fence 下把本地 `agent_import_tombstone`
行发布到 D1 并级联删除被擦除 session 的 mirror 行；restore 则 tombstone 优先——
被擦除的 session 绝不回灌，其 fence 会持久化到恢复方本机，重复删除/恢复幂等。
R2 payload 字节尚未物理删除（唯一剩余 deferral）。显式本地
`--restore-erased` import 仍会保留新的复制 incarnation，使 session generation 与
child source namespace 不会和旧保留行冲突。

`cloud sync` 默认模式仍使用旧的人类进度输出。`cloud restore` 和 `cloud sync` 的失败继续通过 Libra 的标准 CLI 错误机制处理。

在任一 cloud 操作读取或修改本地 `object_index` 前，CLI preflight 会重放先前对象写命令因后台索引
更新失败而保留的、有界原子 repair marker。只有精确索引行插入或调和成功后才删除
marker。重放只打开一次仓库数据库，每次调用最多枚举 repair 目录中的 100,000 个原始条目，
并执行有界多行 upsert；若仍有下一页，本次会保留已完成的进展，但 cloud 命令
fail-closed，后续仓库命令继续回放下一页。重放者与排队 writer 在索引行更新及 marker
退役期间持有同一个来自固定 65,536 个 OID 分片命名空间、进程崩溃可恢复的 ownership lock；因此迟到 writer 看到
marker 已被重放退役时会跳过，不会在实际删除后复活索引行。若 marker 在重放打开前已被
并发退役，则按已完成处理。canonical 最终 marker 的文件名与内容都严格校验。当前原子写使用
独立的同级 staging 目录；有界重放会清理 marker 目录中的旧 `.tmp*` 遗留，避免它们占满每一页并
永久遮住真实修复工作。每次 staging 扫描最多检查 1,024 个条目、删除 256 个超过 24 小时的
普通临时文件，既保留活动 writer 又最终回收崩溃遗留；marker OID 长度还必须匹配仓库配置的
SHA-1 或 SHA-256 对象格式。marker 只会在配置后端对象写入
成功后创建，因此当分层后端淘汰本地缓存、payload 仅存在于远端时，它仍是充分的写入来源
证明；sync 在上传前仍会读取并校验 payload。若 marker 损坏、仍有下一页或数据库更新仍失败，cloud 操作会在读取凭据、上传、输出成功进度或
JSON 成功信封之前以 `LBR-IO-002` fail-closed；因此 `--force` 不能替代本地 catalog
修复。

后台队列记账按 CLI 调用隔离：并发的直接 `ClientStorage` 调用既不会延长该命令的 drain 等待，
也不会让该命令发出警告或以 9 退出。命令归属更新与直接库调用还使用两个独立的有界 FIFO
lane，因此既有的直接调用 backlog 不会占用 cloud preflight 的有限 drain 预算。marker
发布与 destructive cleanup 通过仓库级 generation fence 串行化；cleanup 从精确候选复核到
事务提交始终持有该 fence。启用 `--sync-data` 时，marker 退役会 fsync 所在目录。

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
| `LBR-IO-002` | cloud 操作前无法重放耐久的本地 object-index repair marker |
