# `libra cache`

检查 Libra 的分层存储 / LRU 缓存配置（lore.md §0.10）。这是一个诊断辅助命令，用来呈现现有 `LIBRA_STORAGE_*` 可调参数，便于确认运行中的存储后端会应用什么配置。

## 概要

```
libra cache info
```

## 说明

`cache info` 按分层存储后端解析配置的同一方式报告已解析的存储/缓存参数（先环境变量，再全局 config DB），因此报告值与后端实际使用值一致：

- **storage** — 原始 `LIBRA_STORAGE_TYPE` 值（未设置时为 `local`；否则保留你的精确值，例如 `s3` / `r2` — 大小写错误的 `R2` 会原样显示并报告为非分层）。
- **tier** — 配置是否选择了持久层：`LIBRA_STORAGE_TYPE` 必须是区分大小写的 `s3` / `r2`，并通过后端连接前应用的所有静态检查（非空 bucket、可解析的 `LIBRA_STORAGE_ENDPOINT`、非空 `LIBRA_STORAGE_ACCESS_KEY` / `LIBRA_STORAGE_SECRET_KEY`）。因此大小写错误的 `R2`、空 key 或格式错误的 endpoint 会报告为非分层，而不是误导你。缓存参数只在启用分层时生效；纯本地仓库不会缓存任何内容。实际连接还需要有效凭证，本静态报告不会验证凭证。
- **threshold** — 小/大对象阈值，单位字节（`LIBRA_STORAGE_THRESHOLD`，默认 1 MiB）。大小达到或超过此值的对象会进入 LRU 缓存，而不是永久存储。
- **cache** — 大型缓存对象的本地 LRU 磁盘预算，单位字节（`LIBRA_STORAGE_CACHE_SIZE`，默认 200 MiB）。

不可解析的数字值会回退到默认值（与存储后端的宽松解析一致），因此 `cache info` 不会因为坏值失败。它不需要仓库。

### 存储 / 缓存环境变量

| 变量 | 含义 |
|------|------|
| `LIBRA_STORAGE_TYPE` | 后端类型。未设置 → 仅本地；`s3` / `r2` → 分层（持久层 + 本地 LRU 缓存）。 |
| `LIBRA_STORAGE_THRESHOLD` | 小/大对象阈值，单位字节（默认 `1048576`）。对象 `>=` 此值时进入 LRU 缓存；更小对象永久存储在本地。 |
| `LIBRA_STORAGE_CACHE_SIZE` | 大型缓存对象的本地 LRU 磁盘预算，单位字节（默认 `209715200`）。 |

## 选项

| 选项 | 说明 | 示例 |
|------|------|------|
| `info` | 显示已解析的存储/缓存配置。 | `libra cache info` |
| `--json` / `--machine` | 结构化 `{ storage_type, tiered, threshold_bytes, cache_size_bytes }`。 | `libra --json cache info` |

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | 配置已报告。 |
| 非零 | 某个存储配置值无法解析（例如全局 config DB 不可读）；失败会显式暴露，而不是静默报告默认值。 |

## `cache evict`（lore.md 2.9）

从本地缓存逐出 **已验证持久化** 的大型 loose objects（>= 分层阈值），按物化时间从最旧开始，直到低于配置预算（`LIBRA_STORAGE_CACHE_SIZE`，可用 `--max-size` 覆盖；`--max-size 0` 逐出所有已验证候选）。安全性：每次 unlink 都受一个紧邻执行的、错误感知的持久性探测保护 — 持久层 *确认不存在* 的对象会跳过（请 push/backup 使其持久化），探测 *错误* 永远不视为不存在，前三个探测连续失败会中止本轮且不删除任何内容。`--dry-run` 报告结果（仍会运行探测）；`--min-age`（默认 600s）跳过刚物化的对象。被逐出的对象在读取时会从持久层透明自愈（重新验证、重新缓存），但离线时不可用。纯本地仓库没有可逐出对象；离线读取策略会拒绝（无法探测）。后台用法：`libra maintenance run --task cache-evict`（不在默认任务集中）。残余风险（已记录）：存在性不等于完整性 — 远端副本损坏会导致没有好副本；v1 依赖 S3/R2 服务端完整性。

## 示例

```bash
# 用当前环境显示已解析的存储/缓存参数。
libra cache info

# 检查一个分层（R2）配置及自定义 LRU 预算。
LIBRA_STORAGE_TYPE=r2 LIBRA_STORAGE_CACHE_SIZE=536870912 libra cache info

# 面向工具的结构化输出。
libra --json cache info
```

## 与 Git 对比

Git 没有等价命令；这是 Libra 分层对象库的诊断扩展，在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中分类为 `intentionally-different`。
