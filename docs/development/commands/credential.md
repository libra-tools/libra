# credential 命令开发设计

## 命令实现目标

`libra credential get|store|erase`（`fill` 为 `get` 的 legacy 别名；未知操作静默忽略）是 vault 支撑的 Git 凭证助手：讲 Git 凭证 key/value 协议，凭证经 vault AES-256-GCM 加密存储，绝不明文落盘、绝不泄露到日志/错误/trace。

## 对比 Git 与兼容性

- 兼容级别：`partial`。
- 已支持：`get`（`fill` 别名）/`store`/`erase` 的 Git 凭证 stdin/stdout 协议；未知操作静默忽略（Git helper 约定，exit 0 无输出）；`url=` 展开为 protocol/host/path；`password_expiry_utc` 过期（默认 30 天）；vault 加密存储。
- 退出码：0（fill 命中或空；store/erase 完成）；128（store 缺 username/password、过期时间戳、vault 未初始化，或请求不可读）。
- **有意差异**：存储为 vault 加密（非明文 `~/.git-credentials`）且**仓库范围**（unseal key 按 repo-id）；每个 `protocol/host/path` 仅一条凭证。
- 未公开（延后）：`credential-cache`；每 host 多用户名；**消费侧 `credential.helper` 链校验（拒绝 `!cmd`/相对路径）—— 那属于 fetch/push 调用助手的路径，本命令是「助手本身」，不调用外部助手，故有意不在此实现**。

## 设计方案

- 入口与分发：`src/cli.rs::Commands::Credential` → `command::credential::execute_safe`。**加入 `command_preflight` 的 `none()` 组**：跳过 hash-kind preflight，`fill` 在仓库外也能干净未命中（exit 0 空），vault 延迟解析。
- 源码分层：`src/command/credential.rs`：`CredentialArgs`（子命令 fill/store/erase）、`CredentialAttrs`（protocol/host/path/username/password/password_expiry_utc）、`StoredCredential`（serde，加密前的 {username,password,expires_at}）、`read_attrs`（key=value，空行终止；`url=` 展开，显式字段覆盖）。
- 存储：`credential_key` = `credential.{sha256(v1\0protocol\0host\0path)}`（不可逆，config 不含明文 host/user）。值 = `hex(vault::encrypt_token(unseal_key, json))`，经 `ConfigKv::set(key, hex, false)`（预加密、按原样存，与 vault root token 一致）。`vault::load_unseal_key()` 取 key。
- fill：load_unseal_key → ConfigKv::get（`.ok().flatten()`，缺仓库/缺条目=未命中）→ hex decode → decrypt_token（解密失败=轮换→未命中）→ serde → 过期检查 → username 匹配 → 输出 protocol/host/path/username/password/password_expiry_utc。**全路径任何分支都 exit 0**（无侧信道）。
- store：要求 username+password（否则 routing_error 128，不回显密码）；`password_expiry_utc` 过去→拒绝 128；缺省 expires_at=now+30d；encrypt_token + ConfigKv::set。
- erase：`ConfigKv::unset_all`（幂等）。
- **安全不变量**：密码/token 绝不 tracing/println/错误回显；`routing_error` 只含 `protocol://host`；fill 全 exit 0；解密只在 vault 边界（ring AES-256-GCM）内。
- 底层操作对象：repo config（加密凭证）+ vault unseal key（`~/.libra/vault-keys/<repo-id>`）。无对象库/网络/工作树写入。

## 实现历史

- 2026-06-30（GGT-08，`grit-gap.md` 阶段 3）：新增；复用 `vault::{encrypt_token,decrypt_token,load_unseal_key}` + `ConfigKv`。

## 当前状态

- 公开状态：已公开（`Commands::Credential`）。
- 测试：`tests/command/credential_test.rs`（store→fill round-trip、未知 host 空+exit0、erase、用户名不符未命中、过期时间戳拒绝 128、缺密码错误不泄露、`RUST_LOG=debug` 下密码不入 stderr、仓库外 fill 空）+ `credential.rs` 单测（url 解析、显式覆盖、key 哈希稳定且不含明文、空密码视为缺省）。
- 用户文档：`docs/commands/credential.md`（EN + zh-CN）。

## 还未实现的功能

| 类别 | 未完成项 | 当前处理 |
|---|---|---|
| 消费侧 | `credential.helper` 链校验（拒绝 `!cmd`/相对路径/PATH 查找） | 有意不在此命令；属于 fetch/push 调用助手路径，后续在那里收口。 |
| HTTPS 集成 | fetch/push 经 `credential.helper` 调用本助手以减少交互式 `ask_basic_auth` | 后续在 fetch/push 侧接线。 |
| 多用户 | 每 host 多用户名 | 延后；首版每 `protocol/host/path` 一条。 |
| 轮换测试 | 双 unseal key 交叉验证 | 解密失败=未命中已实现（fill 路径）；交叉测试后续补。 |

## 维护要求

- 改进本命令前先阅读 [docs/development/commands/_general.md](_general.md)。
- **绝不**新增任何把 password/token/解密明文写入 stdout（fill 协议响应除外）、stderr、日志或错误的路径；fill 必须对所有分支 exit 0。
