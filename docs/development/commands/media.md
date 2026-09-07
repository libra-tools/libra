# `libra media` 开发设计

## 命令实现目标

保留默认关闭的 `fastcdc = []` 功能，复用既有确定性分块器、manifest 和本地 chunk store，
把 FastCDC 接到 Libra 的真实 LFS 上传/下载路径，并与 Mega 的可选服务端实现联动。
不增加客户端依赖，不修改 Git 对象图或标准 LFS pointer 的 SHA-256 标识。

## 对比 Git 与兼容性

`intentionally-different`：`media chunk/inspect/verify/probe` 是 Libra 扩展。
默认构建仍使用标准 LFS；启用功能后，远端没有兼容能力或 manifest 时回退完整对象。
`lfs.fastcdc=false` 可按仓库关闭传输扩展。所有远端现在都保留仓库路径，
例如 `/project/demo.git/info/lfs`，不再把 Mega 的地址截断到主机根路径。

## 设计方案

- 算法保持冻结：in-tree gear hash + normalized chunking，MIN 512 KiB / AVG 2 MiB /
  MAX 8 MiB，固定 SplitMix64 GEAR 表。它不是第三方 crate 的 v2020 算法。
- `MediaManifest` 字段保持 v1；新增拒绝零长度、超大块、溢出、错误 fallback_oid、
  不支持的 checksum 和超过 8192 块的清单。JSON 上限为 10 MiB。
- `capability` 在仓库 LFS URL 后追加 `libra/media/v1/capabilities`，
  使用 host-scoped Bearer token、请求超时、有界响应和既有退避。
- `transfer::MediaClient` 上传先准备 manifest/查询缺块，只上传缺失内容，再 finalize；
  服务端逐块校验、完整 SHA-256 校验和 FastCDC 边界校验后保存完整 LFS fallback，
  最后发布可下载 manifest。失败重试重新查询缺块即可。
- 下载只使用 Finalized manifest；按实际 offset/length 对应的内容块缓存恢复，不使用
  旧等长分块的除法推算。缓存块读取时重算 SHA-256；远端坏块或坏清单拒绝发布。
- `chunk_store::reassemble` 使用既有 `StreamingAtomicFile`，独占临时文件、
  错误时自动清理、完整校验后原子覆盖目标。缓存仍在私有 `.libra/media` 中。
- `LFSClient::upload_object/download_object` 的新调用严格在 feature gate 内。
  标准 LFS batch 保持 basic，不向普通服务端发送扩展上传请求。

## Mega 协议与权限边界

端点位于 `<repo>.git/info/lfs/libra/media/v1`：

| 方法 | 路径 |
|---|---|
| GET | /capabilities |
| POST | /manifests |
| PUT | /manifests/{id}/chunks/{hash} |
| POST | /manifests/{id}/finalize |
| GET | /manifests/by-media/{oid} |
| GET | /manifests/by-media/{oid}/chunks/{hash} |

prepare 返回 manifest_id 和 missing_chunks。manifest_id 是紧凑 JSON 数组
`[version,algorithm,hash_algorithm,media_oid,media_size,chunks]` 的 SHA-256，
不包含客户端 provenance。冻结边界保证同一内容的合法 manifest ID 一致。

Mega 新端点要求 Mono access token，并保留 URI 改写前的仓库路径。
由于现有 Mega LFS 尚无完整仓库 ACL，本版按「认证用户＋仓库」隔离存储，
再由 manifest ID / media OID 限定对象范围。不同用户/仓库不能查询或读到彼此的块；
另一用户的下载回退既有完整 LFS 对象。没有公开的裸 chunk-hash GET。
服务端必须以 `--features fastcdc` 显式构建，默认不暴露扩展端点。

## 测试

既有分块/manifest/cache 单测；`media_fastcdc_test` 的 CLI 测试；
坏块不覆盖目标、普通服务端完整 LFS 回退测试；
手动 `mega_fastcdc_http_interop` 连接 Mega 的真实 HTTP 路由和令牌验证，
覆盖实际变长分块上传/下载、只补缺块、缓存恢复/修复、跨用户拒绝和空文件。
两进程命令见 Mega 的 `docs/lfs-api.md`，共享 `MEGA_FASTCDC_READY_FILE`。
测试结果以本次实际运行记录为准；不把编译失败或 skipped/ignored 计为通过。

## 未完成项

本次交付传输链路，不宣称完成 Lore §6 的全部生产门禁。
共享仓库 ACL、自动孤儿块 GC、quota、服务端 fsck/heal、obliteration、
chunk-only 策略、字节范围水合、跨租户 dedup 均未开放。
Pending 描述符 24 小时到期，过期数据不会自动回收；部署方需明确保留策略，
不得对仍被 Finalized manifest 共享的块设置无条件生命周期删除。
