# `libra service`

无头本地服务：notification bus + dirty-mark ingestion（Libra 扩展，lore.md §1.11）。Git 没有等价物。

## 概要

```
libra service run [--host <LOOPBACK-IP>] [--port <PORT>]
libra service status
libra service events
```

## 说明

`libra service run` 启动一个前台、**仅本地** HTTP 服务：

- `--host` 必须是字面量 loopback IP（`127.0.0.0/8` 或 `::1`）— 主机名和非 loopback IP 会被拒绝（退出 129）。服务永远不会打开外向 TCP 端口；每个 endpoint 还会额外拒绝非 loopback peer。
- `--port` 默认 `0`（OS 分配）；真实地址发布到 `.libra/service/service.json`。每个仓库一个实例（`service.lock`）；已死亡进程留下的陈旧 lock 会被回收。
- 用 Ctrl-C（或 Unix 上的 SIGTERM）停止 — shutdown 会移除 discovery 文件并释放 lock。

**Endpoints**（全部做 loopback check；携带数据的 endpoint 要求 `X-Libra-Service-Token` header 匹配 0600 文件 `.libra/service/service-token` — 不信任其他本地用户）：

| Endpoint | Auth | 用途 |
|---|---|---|
| `GET /api/health` | loopback | Liveness probe。 |
| `GET /api/service/events` | token | SSE notification stream。 |
| `POST /api/service/dirty/mark` | token | `{"paths":[...]}` — 通过已验证 owner API 的咨询式 dirty marks（只要任何路径逃出 repo，整个 batch 拒绝；仅 over-report）。 |
| `POST /api/service/notify` | token | `{"type":"...","data":{...}}` — 发布自定义通知（automation triggers）。 |

**Notification v1 语义**：事件为 `{seq,type,at,data}`，`seq` 在每次 service run 内单调递增。投递是 **at-most-once** — 落后的消费者会收到 `resync` 事件，并应重新读取权威状态（`libra dirty --list`、`libra status`）；服务重启时 `seq` 重置。持久事实（marks）保存在 SQLite 中且能跨 `kill -9` 存活；bus 上的一切都可派生。请求体上限 256 KiB。

`libra service status` 报告运行中的实例（pid、URL、health；无实例时退出 1）。`libra service events` tail 事件流（human lines；`--json` 下为 NDJSON；服务消失时干净退出）。

## 退出码

| 代码 | 含义 |
|------|------|
| `0` | 成功。 |
| `1` | `status` 且没有运行中的实例。 |
| `128` | 不在仓库中。 |
| `129` | 用法错误（非 loopback `--host`）。 |

## 示例

```bash
libra service run                      # loopback，OS 分配端口
libra service status                   # pid、URL、health
libra service events                   # tail 通知
TOKEN=$(cat .libra/service/service-token)
URL=$(libra --json service status | jq -r .data.base_url)
curl -H "X-Libra-Service-Token: $TOKEN" -X POST "$URL/api/service/dirty/mark" \
     -H 'content-type: application/json' -d '{"paths":["src/main.rs"]}'
```

## 与 Git 对比

Git 没有本地服务表面（`git daemon` 将 wire protocol 服务到网络 — 与此设计相反）。在 [`COMPATIBILITY.md`](../../../COMPATIBILITY.md) 中分类为 `intentionally-different`。
