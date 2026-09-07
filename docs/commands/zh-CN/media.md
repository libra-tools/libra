# `libra media`

FastCDC LFS 媒体分块客户端（lore.md §6），是受 `fastcdc` 功能开关控制的 Libra 扩展，只有使用 `--features fastcdc` 构建才会编译，**默认二进制中不存在**。它按内容为媒体文件分块，构建带版本的 manifest，将块存入私有本地存储，重组并验证文件，并与远端协商分块 LFS 能力；远端不支持时回退到标准 Git LFS。

`media` 是 Libra 专有扩展（`intentionally-different`）：Git 没有媒体分块概念。它不修改 Git 对象图，chunk hash 不是 Git object ID；块和 manifest 存放在与 `objects/` 同级的私有 `.libra/media/` 中。`media_oid` 始终是完整文件的 SHA-256，独立于 `core.objectformat`，与标准 LFS pointer OID 一致。

## 子命令

| 子命令 | 说明 | 示例 |
|---|---|---|
| `chunk <path> [--store]` | 对文件做 FastCDC 分块并输出 manifest；`--store` 会把 chunks + manifest 持久化到 `.libra/media`。 | `libra media chunk big.psd --store` |
| `inspect <manifest>` | 解析并验证一个 manifest JSON 文件。 | `libra media inspect .libra/media/manifests/<oid>.json` |
| `verify <path> \| --media-oid <oid>` | 从本地 chunk store 重组并验证完整 `media_oid`（永不发布损坏文件）。 | `libra media verify big.psd` |
| `probe [--remote <name>]` | 探测远端 media capability endpoint 并报告传输决策（chunked vs standard-LFS fallback）。 | `libra media probe --remote origin` |
| `--json` | stdout 上的结构化 JSON 信封（全局标志）。 | `libra --json media chunk big.psd` |

## 安全回退

`media probe` 只报告远端能力：`chunked (fastcdc-v1)`，或 `standard-lfs (fallback)` 并附带原因，例如没有能力端点、服务端禁用、算法不兼容、所需能力不足、协议版本不兼容或退避后的服务端错误。它假定仓库允许分块且本地存在完整 fallback，**不会读取 `lfs.fastcdc`**，在这些假定下也不会报告 `blocked`。因此，probe 输出 `chunked` 不等于当前仓库已经启用实际分块传输。

实际 LFS 传输还会检查 `lfs.fastcdc`，并要求服务端保留标准完整对象、允许 manifest。仅提供 chunk-only 的远端回退 basic LFS。以 `--features fastcdc` 构建的 Mega 实现了需要认证的扩展；其他远端继续使用标准 Git LFS。

## 与 Mega 联动传输

在 Libra 源码仓库执行 `cargo build --features fastcdc`；在 Mega 仓库按正常服务配置执行 `cargo run -p mono --features fastcdc -- service http` 构建并启动 HTTP 服务。两端默认构建均关闭该 feature。以下 `libra` 命令必须使用刚构建的二进制（`target/debug/libra`，Windows 为 `libra.exe`）；编译不会替换 PATH 中另行安装的版本。

先通过 Mega 现有的已登录用户令牌签发流程（`POST /api/v1/user/token/generate`）取得 **Mono 访问令牌**。`libra auth login` 只在本地保存已有令牌，不会替 Mega 签发令牌；GitHub PAT 或浏览器会话 cookie 不能代替 Mono access token。

以本机 8000 端口的 Mega HTTP 服务为例，在 Libra 仓库中执行：

```bash
libra config remote.origin.url http://localhost:8000/project/demo.git
libra auth login --host http://localhost:8000
# 在隐藏提示中粘贴 Mono 访问令牌。
libra auth status --host http://localhost:8000
libra config lfs.fastcdc true
libra media probe --remote origin
```

编入 feature 后，未设置 `lfs.fastcdc` 时默认允许自动协商；`true` 显式启用，`false` 在该仓库禁用传输扩展。令牌绑定的**主机和端口**必须与远端一致。非 loopback 服务必须使用 HTTPS，例如 `--host https://mega.example.com:8443`；HTTP 仅允许为 loopback 附加令牌。`--host` 只传 origin，不带仓库路径，不要将令牌放进 URL。脚本通过 `--with-token` 从 stdin 读取令牌，详见 [`libra auth`](../auth.md)。

`origin` 保留仓库 URL；LFS 客户端使用 `<repo>.git/info/lfs`，在该地址后追加 `libra/media/v1/capabilities` 探测能力，并自动将本地存储的令牌附加为 Bearer header。

正常 LFS push/upload 会准备 manifest、查询缺块、只上传缺失块，再请求 finalize。Mega 校验块的 SHA-256、完整文件 SHA-256 和冻结的 FastCDC 分块边界，保存标准 LFS 完整对象后才发布 manifest。重新 push 会再次查询缺块并续传。下载只使用已 finalize 的 manifest，复用校验通过的本地块，并在完整文件校验成功后原子替换目标。远端清单或块损坏会报错，保留已有目标文件；没有 manifest、能力不兼容或功能被禁用时使用标准完整对象 LFS。不支持仅保存块而丢弃完整对象的上传。

Mega 当前按「认证用户＋仓库路径」隔离块和 manifest，其他用户通过既有标准 LFS 完整对象路径下载。这些端点要求 Bearer 访问令牌，不提供公开的裸 chunk-hash 查询或下载；这并不等于实现了完整仓库 ACL。manifest 上限为 10 MiB / 8192 块，单块上限为 8 MiB。

Pending 描述符在 24 小时后过期，重新准备 manifest 可继续查询和上传缺块；过期数据不会自动回收。此扩展需要显式启用，部署前应规划保留策略与配额，不能对仍被已发布 manifest 引用的块直接设置生命周期删除。

## 延后项

共享仓库 ACL、自动孤儿块 GC、配额统计、服务端 fsck/heal、obliteration、仅存块策略和按字节范围水合尚未实现。当前传输扩展不代表已完成 Lore §6.5–6.8 的全部生产要求。

## 示例

```bash
libra media chunk big.psd                 # 对文件分块；打印 manifest 摘要
libra media chunk big.psd --store         # 同时本地持久化 chunks + manifest
libra media inspect .libra/media/manifests/<oid>.json
libra media verify big.psd                # 从 store 重组并验证 media_oid
libra media probe --remote origin         # capability probe；回退到标准 LFS
libra --json media chunk big.psd          # 给 agents 使用的结构化 JSON 输出
```
