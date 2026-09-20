# service 命令开发设计

## 命令实现目标

lore.md §1.11：无头 `libra service` + notification v1。本地事件总线 +
dirty-mark 摄入（自动化触发承载），显式**不做 hosted server**。

## 对比 Git 与兼容性

- 兼容级别：`intentionally-different`。Git 无本地服务面（`git daemon` 是对
  网络提供线协议——正相反）。
- 仅新增命令；既有命令零变化。

## 设计方案（LEP 四面矩阵，§3.0.1）

- Git 磁盘格式：不变。线协议：N/A（纯本地，绑定期+对端双重环回强制）。
  SQLite：**零迁移**——唯一持久写入是经 1.1 属主 API 的咨询 dirty 标记
  （`DirtyCache::mark_paths`，仓库逃逸整批拒绝、只会过报）。CLI：仅新增。
- **本地性两道闸**：`--host` 解析期须为字面环回 IP（127.0.0.0/8 或 ::1，
  拒绝主机名与 0.0.0.0，129/LBR-CLI-002）；`SocketAddr::new(ip, port)` 直接
  构造（IPv6 安全，不走 format!-parse）；每个端点再校验对端环回。
- **最小本机访问控制**（lore 行文点名 dirty/automation 承载）：SSE 事件流与
  两个 POST 端点均须 `X-Libra-Service-Token`（0600 令牌文件，复用
  `ensure_control_token_file`/`validate_token_file_perms`；Windows 上模式位
  校验为 no-op——机密性依赖默认 profile ACL，已文档化）。**事件流也被门禁**
  ——总线携带 dirty 路径与自动化载荷，其它本机 uid 不受信任。请求体上限
  256KiB（对齐 code 路由的刻意限制）。
- **notification v1 语义（显式契约）**：内存 broadcast(256)，at-most-once；
  滞后消费者收 `resync` 事件（应重读权威态：`dirty --list`/`status`）；
  `seq` 随服务重启归零。§7.9：权威态只在 SQLite（标记），总线内容皆可推导。
- **§7.10 kill -9 行**：e2e 实测——kill -9 后标记存活（SQLite）、重启回收
  陈锁（既有 advisory-lock 机制：进程死亡即释放 OS 锁）、`status` 对死 pid
  报 stale 并退出 1。启动时对陈旧的 dirty 扫描锁**只报告不代抢**（扫描者
  语义在 `try_acquire_scan_lock`；服务代抢可能与真在跑的长扫描竞态）。
- 单实例/发现/生命周期：`.libra/service/{service.lock,service.json,
  service-token}`，复用 `code_control_files` 全套（`acquire_control_lock`/
  `write_control_info`/`pid_is_live`/`cleanup_control_files`）；前台运行，
  Ctrl-C/SIGTERM（Unix）优雅停机，Drop guard 清理。

## 刻意延后（含理由，_general.md 要求）

| 项 | 理由 |
|---|---|
| UDS 传输 | lore 行是「UDS(0600) **或** 环回」——环回分支已满足；仓内无 UnixListener 先例，axum UDS 需另套 connect-info 且无 Windows 对等。 |
| 文件监视器喂标记 | 加速器而非正确性前提（1.1 契约）；需引入新重依赖（notify）+ 风暴/符号链接语义；标记可经令牌门 POST 流入。 |
| repo/status 只读透传 | 处理器在 code 路由内部，抽取面大；v1 聚焦 lore 行核心（总线+通知+dirty 承载）。 |
| 外部工具协议 | lore 行未要求；只做通知总线与 dirty 标记。 |
| 守护化/systemd | 「不要做 hosted server」；前台 + 外部监督（同 `libra code` 默认 Web 模型）。 |
| §7.7 自动重放 | 依赖不存在的操作台账；v1 保证「标记不静默丢失」。 |
| code_ui 事件流重基到共享总线 | 服务总线为独立小模块（信封 `{seq,type,at,data}`）；code SSE 线格式由既有测试钉住，重基属纯重构后续项。 |
| 依赖 1.6 | lore 依赖列 1.6（HTTP 远端令牌）；本服务纯本地，最小访问控制由 0600 令牌满足——此读法记录于此，如维护者按字面依赖裁决，落地顺序移至 1.6 后。 |

## 实现历史

- 2026-07-02（lore.md Phase 1 / 1.11）：初版 run/status/events + 2 e2e + 2 单测。

## 当前状态

- 测试：`tests/command/service_test.rs`（环回拒绝矩阵/仓外 128；端到端：
  status 健康、无令牌 401、逃逸 400 整批拒绝、mark+notify 事件经 SSE 送达、
  标记 SQLite 持久、kill -9 存活 + 重启回收 + stale status 退出 1）；
  `command::service` 单测（环回校验矩阵、信封序列化键稳定）。
- 用户文档：`docs/commands/service.md`。

## 维护要求

- 改进前先读 [_general.md](_general.md)。新端点必须过两道闸（对端环回 +
  令牌）并保持体积上限；任何 dirty 写入必须走 `DirtyCache` 属主 API。
