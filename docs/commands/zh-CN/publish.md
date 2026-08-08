# `libra publish`

准备 Libra 的只读 Cloudflare Worker 发布表面。

当前实现状态：

- `libra publish init` 会将嵌入的 Worker 模板 materialise 到 `worker/` 下，并记录 `.libra/publish/worker-template-manifest.json`。该 manifest 位于仓库**共享存储**（经 `commondir` 链解析的共享 `.libra`），因此 `init`/`status`/`deploy`/`unpublish` 在任何 worktree 中读写的都是同一份 manifest；`worker/` 目录本身是被跟踪的工作树内容，按 worktree 独立。输出中报告的 `manifest_path`：当 manifest 位于调用 worktree 之下（main worktree）时为相对路径，否则（linked worktree）为绝对路径。
- `libra publish status` 报告本地 Worker 模板状态：`missing`、`current`、`modified`、`outdated` 或 `conflicted`；配置 site id 时，还可以将本地 branch/tag refs 与 D1 `publish_refs` 比较。
- `libra publish sync --dry-run` 扫描本地 branch/tag refs，校验 `--ref`，报告脏树警告，并在没有 Cloudflare 凭据的情况下输出本地发布计划。
- `libra publish sync` 将代码快照和 AI artifacts 写入 R2，并在 D1 中 upsert `publish_sync_runs`、`publish_revisions`、`publish_files`、`publish_ai_objects`、`publish_ai_versions` 和 `publish_refs`；只有完整的 all-refs sync 才会推进 `publish_sites.latest_revision_oid`。内置 AI 导出 planner 读取本地 AI 历史，输出脱敏的 snapshot/event 对象，并为发布 AI index、graph 和 bundle 添加 projection 对象。
- `libra publish deploy` 校验本地 Worker 模板，要求生成的 Worker config/bindings，运行 `pnpm build`，并且除非设置 `--skip-deploy`，否则会应用 D1 migrations 并通过 Wrangler/OpenNext 部署 Worker。
- `libra publish unpublish --yes` 通过 Wrangler D1 execute 将 `publish_sites.status = 'disabled'`，从而禁用已发布站点。Worker 已经会对 disabled 站点返回 HTTP 410。
- Worker API route 测试覆盖 private-site 403、disabled-site 410，以及缺失 D1 file 行或缺失 R2 内容时的类型化 404 信封。
- Worker 项目使用 `wrangler types --env-interface CloudflareEnv cloudflare-env.d.ts` 作为 binding 类型来源。提交的 `env.d.ts` 只用可选 Cloudflare Access secret 名称扩展生成类型。
- Worker `build` 脚本运行 `cf-typegen` 和 OpenNext；OpenNext 配置为内部调用 `pnpm next:build`，所以 `pnpm build` 不会递归调用自身。
- Worker e2e runner 在 `BASE_URL` 未设置时用本地 fixture D1/R2 bindings 启动 `next dev`，并对发布 landing page、代码浏览器、文件查看器、AI model page、refs、status，以及 empty/non-text 状态运行桌面和移动 Chromium 断言。
- `libra clone libra+cloud://<clone-domain>/<slug>` 会把已发布的 Git 对象、refs 元数据以及 publish AI index/graph/bundle/object 信封从 D1/R2 恢复到本地 Libra 仓库。
- 剩余 live-only publish gate 记录在 `docs/development/commands/publish.md`；它要求真实 all-refs sync、cloud clone restore、已部署 Worker refs/tree/file API smoke，以及具有部署权限的 Cloudflare 凭据。

## 概要

```
libra publish init      [OPTIONS]
libra publish sync      [OPTIONS]
libra publish status    [OPTIONS]
libra publish deploy    [OPTIONS]
libra publish unpublish [OPTIONS]
```

## 说明

`libra publish` 是 `libra cloud` 面向外部的对应功能。当前交付切片覆盖本地 Worker-template 初始化和状态、离线 sync dry-run、云快照和 AI artifact 上传、云 ref 状态比较、Worker build/deploy/unpublish 编排，以及用于已发布只读快照表面的 `libra+cloud://` clone restore。

## 子命令

### `libra publish init`

```
libra publish init \
    [--slug <slug>] \
    [--clone-domain <clone-domain>] \
    [--display-origin <origin>] \
    [--name <human-name>] \
    [--visibility public|private] \
    [--worker-name <name>] \
    [--max-preview-bytes <bytes>]
```

- 确认当前目录是 Libra 仓库。
- 复用当前仓库根作为模板目标。
- 写入 `.libra/publish/worker-template-manifest.json`，其中包含嵌入模板版本、render policy，以及每个托管文件的 SHA-256 baseline。
- 接受上面列出的 site-shaping 标志以保持前向兼容。当前实现不会把这些值持久化到仓库配置；它只使用 CLI parser 校验标志形状。
- `--max-preview-bytes <bytes>`：必须 `> 0`；CLI 会在写入模板前拒绝 `0`。
- 从嵌入的 Worker 模板 materialise `worker/`。缺失文件会新写入，逐字节相同的模板文件保持 current，用户修改过或符号链接路径会以 `LBR-CONFLICT-002` fail closed；不会写入冲突标记。
- **不**要求 Cloudflare 连通性。

### `libra publish sync`

```
libra publish sync [--ref <branch|tag|full-ref>]
                   [--dry-run]
                   [--fail-on-dirty]
                   [--ai-redaction default|strict]
                   [--allow-sensitive-path <path>]…
                   [--force]
                   [--json]
```

当前行为：

- `--dry-run` 扫描本地 `refs/heads/*` 和 `refs/tags/*`，按 revision oid 去重，统计每个唯一提交树中的文件，并输出计划。它不读写 Cloudflare D1/R2，也不要求 Cloudflare 凭据。
- Dry-run 会加载每个计划 revision 已提交的 `.librapublishignore`，并应用内置 publish deny 规则。被拒绝路径会作为带 `builtin_credential` 或 `user_ignore` 原因的警告报告。
- `--ref <branch|tag|full-ref>` 将 dry-run 过滤到一个分支或标签。如果短名称同时存在为分支和标签，命令以 `LBR-CLI-003` 失败，并要求使用 `refs/heads/<name>` 或 `refs/tags/<name>`。
- 不带 `--dry-run` 时，命令要求 `publish.site_id` 以及 `LIBRA_D1_ACCOUNT_ID`、`LIBRA_D1_API_TOKEN`、`LIBRA_D1_DATABASE_ID`、`LIBRA_STORAGE_ENDPOINT`、`LIBRA_STORAGE_BUCKET`、`LIBRA_STORAGE_ACCESS_KEY` 和 `LIBRA_STORAGE_SECRET_KEY`。Libra 先读取仓库本地 `vault.env.*`，再读取全局 `vault.env.*`，最后读取导出的环境变量。它会从 D1 加载匹配的 `publish_sites` 行，用于 `repo_id`、visibility、max preview bytes 和 `refs_generation`。
- 完整 sync 为每个唯一的本地 branch/tag revision 写入一个代码快照，上传 text previews 和 `code-manifest.json` 到 R2，将 binary、too-large 和 ignored 文件只作为 D1 元数据写入，上传 `refs.json` 和 `latest.json`，并通过 refs-generation CAS 推进 `publish_sites`。该 CAS 成功后，同一 site 旧 sync runs 中的陈旧 `publish_refs` 行会被删除。
- 重复 sync 会跳过已有的 revision `code-manifest.json`、text preview objects、`ai/index.json`、AI object JSON、AI graph 和 AI bundle objects，除非传入 `--force`。
- 非 dry-run sync 上的 `--ref` 只写入选中的 ref 及其 revision 快照。它不会上传 `refs.json`/`latest.json`，也不会推进完整 refs generation。
- 脏工作树会发出警告，因为 sync 只规划已提交 refs。`--fail-on-dirty` 会把该条件转换为 `LBR-REPO-003`。
- `--json` 返回 `siteId`、`refsCount`、`revisionCount`、`defaultRef`、`latestRevisionOid`、`fileCount`、`aiObjectCount`、`aiBundleCount`、`warnings`，以及选中 ref/revision 详情。Dry-run 期间 `siteId` 为 `null`；每个 revision 条目还包含 `preflightDeniedCount`。

### `libra publish status`

```
libra publish status [--site-id <uuid>] [--json]
```

当前行为：该子命令始终检查本地 Worker 模板和 manifest。如果传入 `--site-id <uuid>`，或仓库配置中存在 `publish.site_id`，它还会读取 D1 `publish_refs`，并把已发布 branch/tag refs 与本地 `refs/heads/*` 和 `refs/tags/*` 比较。

状态包括：

- `missing`：manifest 或一个或多个嵌入模板文件缺失。
- `current`：每个嵌入模板文件都匹配当前 Libra 模板，且 manifest 存在。
- `modified`：某个托管模板文件既不同于当前嵌入模板，也不同于 manifest baseline。
- `outdated`：某个托管模板文件仍匹配 manifest baseline，但 Libra 嵌入了更新的模板版本。
- `conflicted`：`worker/` 根或某个托管模板路径是符号链接或非文件路径。

`--json` 返回 total、current、missing、modified、outdated 和 conflicted 文件计数。它还包含 `publishedRefs`。没有可用 site id 时，`publishedRefs.state` 是 `unconfigured`。比较运行时，`publishedRefs.state` 是 `compared`，对象包含 matching、changed、local-only 和 published-only ref 计数以及受影响 ref 行。当 D1 `publish_refs` 行指向缺失或非 `published` 的 `publish_revisions` 快照时，同一对象还会报告 `snapshotIssueCount`、`snapshotMissingCount`、`snapshotUnpublishedCount` 和 `snapshotIssues`。

D1 比较需要 `LIBRA_D1_ACCOUNT_ID`、`LIBRA_D1_API_TOKEN` 和 `LIBRA_D1_DATABASE_ID`，使用与 `libra cloud` 相同的 env/vault 解析。缺失或不可达的 D1 配置会使命令失败，而不是静默报告陈旧发布状态。

### `libra publish deploy`

```
libra publish deploy [--skip-deploy]
```

当前行为：

- 要求存在来自 `libra publish init` 的 `worker/` 和 `.libra/publish/worker-template-manifest.json`。
- 在模板缺失、conflicted、outdated，或 `worker/wrangler.jsonc` 仍包含 `REPLACE_WITH_D1_DATABASE_ID` 或 `REPLACE_WITH_R2_BUCKET_NAME` 时，会在运行命令前失败。
- 允许 `modified` 模板状态，因此用户自有 Worker 编辑可以被有意部署。
- 从 `worker/` 运行 `pnpm build`。
- 不带 `--skip-deploy` 时，运行 `pnpm exec wrangler d1 migrations apply LIBRA_PUBLISH_DB --remote`，然后运行 `pnpm exec opennextjs-cloudflare deploy`。
- 解析部署输出并打印/返回第一个部署 URL。如果部署成功但没有 URL，命令会失败，避免脚本静默丢失发布 endpoint。
- 带 `--skip-deploy` 时，只运行本地 build；跳过 D1 migrations 和 Worker deploy 步骤。这是在没有 Cloudflare 凭据时安全的 CI smoke 路径。

### `libra publish unpublish`

```
libra publish unpublish --yes [--site-id <uuid>]
```

当前行为：

- 要求 `--yes`；没有它时，命令会在读取 config 或运行云命令前失败。
- 提供 `--site-id <uuid>` 时使用该值；否则从仓库配置读取 `publish.site_id`。
- 构造 SQL 前校验 site id 是 UUID。
- 要求本地 Worker 模板和已配置 `worker/wrangler.jsonc` 中的 `LIBRA_PUBLISH_DB` binding。
- 从 `worker/` 运行 `pnpm exec wrangler d1 execute LIBRA_PUBLISH_DB --remote --yes --command <UPDATE>`，为选中 site 设置 `publish_sites.status = 'disabled'`。
- 不删除 D1 rows、R2 objects、Worker routes 或 Worker deployments。已发布 Worker 会对 disabled 站点返回 HTTP 410。

## 配置

`libra publish init` 当前不会将 publish keys 写入 `ConfigKv`。它只记录 Files 小节中描述的 Worker 模板 manifest。

当以下仓库配置键存在时，publish 命令和 cloud clone restore 会读取它们：

| 键 | 说明 |
|----|------|
| `publish.site_id` | init 时生成的 UUIDv4。稳定。 |
| `publish.slug` | 人类可读 slug；在 clone domain 内唯一。 |
| `publish.clone_domain` | `slug` 解析所在的命名空间。 |
| `publish.display_origin` | 浏览器访问的 HTTPS origin（例如 `https://code.example.com`）。 |
| `publish.name` | 站点显示名。 |
| `publish.visibility` | `public` 或 `private`。 |
| `publish.worker_name` | Wrangler worker 名称。 |
| `publish.max_preview_bytes` | 每个文件的 preview 大小上限。 |

`libra publish sync`、`libra publish status --site-id` 和 `libra+cloud://` clone restore 会从与 `libra cloud` 相同的 `LIBRA_D1_*` / `LIBRA_STORAGE_*` 键读取 Cloudflare account ids、API tokens 和 R2 S3 凭据，并且 `vault.env.*` 优先于进程环境。这些 secrets 绝不会写入 Worker 模板。

## 文件

- `sql/publish/0001_publish.sql` — D1 schema 的事实来源。
- `sql/publish/0002_publish_digest_check.sql` — 追加式 trigger migration，对已经应用 0001 的租户强制每个 digest 列都是小写 64 字符 hex。需要它是因为 SQLite 的 `CREATE TABLE IF NOT EXISTS` 在表已存在时是 no-op，所以 0001 中的列级 CHECK 添加不会到达已有数据库。
- `worker/migrations/<NNNN>_*.sql` — `sql/publish/` 下每个文件的逐字节相同镜像；除非设置 `--skip-deploy`，`libra publish deploy` 会用 `wrangler d1 migrations apply` 应用它们。`publish_schema_contract_worker_mirror_is_byte_equal` 测试会遍历两个目录并拒绝任何漂移。
- `worker/` — Next.js + React + OpenNext-for-Cloudflare 项目。随 Libra 二进制嵌入发布；`libra publish init` 会把它 materialise 到目标仓库根目录。
- `.libra/publish/worker-template-manifest.json` — 本地 manifest，记录渲染的模板版本以及用户修改了哪些文件。
- `.librapublishignore` — 每仓库 ignore list，叠加在内置 deny 规则之上。

## D1 schema migrations

Publish D1 schema 源已经位于 `sql/publish/` 下，每个 `.sql` 文件都有一个逐字节相同的镜像位于 `worker/migrations/` 下（`publish_schema_contract_worker_mirror_is_byte_equal` Rust 测试会遍历两个目录并拒绝任何漂移）。当前 `libra publish deploy` 会在 Worker deploy 步骤前通过 Wrangler 应用这些 migrations，除非设置 `--skip-deploy`。

当前链：

1. `sql/publish/0001_publish.sql` — 初始 schema。表：`publish_sites`、`publish_revisions`、`publish_refs`、`publish_files`、`publish_ai_objects`、`publish_ai_versions`、`publish_sync_runs`。添加 composite FKs、ref-name shape CHECKs、sync-run state-machine CHECK、小写 hex digest CHECKs。
2. `sql/publish/0002_publish_digest_check.sql` — 追加式 trigger migration。SQLite 的 `CREATE TABLE IF NOT EXISTS` 在表已存在时是 no-op，因此 0001 中的列级 CHECK 添加不会到达已有数据库。Migration 0002 添加 `BEFORE INSERT`/`BEFORE UPDATE` triggers，重新强制：
   * `publish_files.content_sha256`、`publish_ai_objects.payload_sha256` 和 `publish_ai_versions.bundle_sha256` 的小写 64 字符 hex 形状；
   * `publish_sites.max_preview_bytes > 0`（0001 列级 CHECK 只强制 `>= 0`；0002 trigger 在已有租户上固定更严格的 publish 语义不变量，并在行级 UPDATE 触发，因此省略该列的 SET 语句也会重新校验）。

   所有 triggers 都是幂等的（`CREATE TRIGGER IF NOT EXISTS`），可安全重复应用。

未来 schema 更改进入 `0003_<topic>.sql` 等文件。每个 migration **必须**是追加式的（`CREATE TABLE … IF NOT EXISTS`、`ALTER TABLE … ADD COLUMN …`、`CREATE INDEX … IF NOT EXISTS`、`CREATE TRIGGER … IF NOT EXISTS`），这样在新 shard 上重新应用会得到与增量应用链相同的最终状态。

## Live Gate

Live publish gate 是 opt-in 的，因为它会写入真实 Cloudflare D1/R2 资源，并在 deploy-smoke 变量存在时探测已部署 Worker：

```bash
LIBRA_ENABLE_TEST_LIVE_CLOUD=1 \
cargo test --features test-live-cloud publish_live -- --test-threads=1
```

D1/R2 前置部分需要以下内容，可以作为 `vault.env.*` 配置条目、导出的环境变量，或仓库 `.env.test` 文件中的 key/value 行：

- `LIBRA_D1_ACCOUNT_ID`
- `LIBRA_D1_API_TOKEN`
- `LIBRA_D1_DATABASE_ID`
- `LIBRA_STORAGE_ENDPOINT`
- `LIBRA_STORAGE_BUCKET`
- `LIBRA_STORAGE_ACCESS_KEY`
- `LIBRA_STORAGE_SECRET_KEY`

已部署 Worker refs/tree/file API smoke 还要求 `LIBRA_PUBLISH_LIVE_WORKER_ORIGIN`，指向使用相同 D1/R2 bindings 部署的 Worker。默认情况下，live gate 会在该 Worker 的 host namespace 中播种一个新 slug 并探测它。当 Worker host 与 D1 `clone_domain` 不同时设置 `LIBRA_PUBLISH_LIVE_CLONE_DOMAIN`；只有在探测预先存在的已部署站点时才设置 `LIBRA_PUBLISH_LIVE_SLUG`；当根树没有直接 file entry 可探测时设置 `LIBRA_PUBLISH_LIVE_FILE_PATH`。

## 示例

```bash
# Materialise 本地 Worker 模板 scaffold
libra publish init --slug my-site --clone-domain code.example.com

# 检查本地 Worker 模板 / D1 ref 漂移
libra publish status

# 按 UUID 检查指定已发布站点
libra publish status --site-id <uuid>

# 规划发布，但不写入 D1/R2
libra publish sync --dry-run

# 将默认 refs 同步到 D1/R2
libra publish sync

# 同步单个命名 ref
libra publish sync --ref refs/heads/main

# 不考虑 CAS revision，重新上传每个 file/object
libra publish sync --force

# 允许 deny list 通常会阻止的路径（private sites）
libra publish sync --allow-sensitive-path docs/private.md

# 构建 Worker 并部署到 Cloudflare
libra publish deploy

# 只构建；跳过 Cloudflare 变更
libra publish deploy --skip-deploy

# 禁用已发布站点，但不删除 D1/R2 数据
libra publish unpublish --site-id <uuid> --yes

# 面向 agents 的结构化 JSON 信封
libra publish --json sync --dry-run
```

同一 banner 由 `libra publish --help` 渲染，因此文档和 CLI 表面保持同步（跨命令 `--help` EXAMPLES rollout，见 `docs/development/commands/_general.md` 条目 B）。

## 另见

- `libra clone` — 通过 `libra+cloud://<clone-domain>/<slug>` 源 scheme 恢复 Cloudflare D1 / R2 发布快照。
- `libra cloud` — `publish` 构建于其上的私有 Cloudflare 备份。
- `docs/development/commands/publish.md` — 内部设计 + 分阶段 rollout。
- `docs/ai/object-model-reference.md` — `publish` 导出的 AI 对象模型契约。
