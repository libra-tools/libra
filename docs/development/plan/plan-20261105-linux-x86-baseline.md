# Linux amd64 发行二进制恢复 x86-64 基线兼容计划（2026-11-05）

> 正式计划，模板 v2.16。2026-11-05 07:51:26 UTC 在分支 `plan/20261105-linux-x86-baseline` 创建。跟踪 Issue [#1060](https://github.com/libra-tools/libra/issues/1060)。2026-11-05 08:58:07 UTC 用户授权登记该 Issue、提交并推送本计划文档、创建 PR；ISA-01 仍为 `pending` / 空，不授权实施或发布。

## 文档职责

本文解决 GitHub `ubuntu-latest` 迁移到 Ubuntu 26.04 amd64v3 后，Libra Linux amd64 官方二进制在低于 x86-64-v3 的 CPU 上启动即 SIGILL 的问题。目标是让通用 `x86_64-unknown-linux-gnu` 发行产物恢复基线 x86-64 兼容，并用真实 Celeron N5105 验证官方产物。

### 适用范围

- `.github/workflows/release.yml` 的 Linux x86_64 GNU release 构建及构建前自检。
- 现有 release workflow compat 验证。
- 新官方 `libra-linux-amd64` 在 Intel Celeron N5105 上的启动验收。

### 非目标

- 不升级 `aws-lc-rs` / `aws-lc-sys` 来替代本次编译基线修复；依赖升级与本根因不同轴。
- 不修改 Rust 代码、CLI、安装脚本、签名清单格式或其它 release 平台。
- 不禁止二进制中存在带正确运行时 CPU 分派的 AVX/AVX2 汇编；不以“全文件零 AVX 指令”为验收标准。
- 不覆盖或重新发布不可变的 v0.30.152～v0.30.154 产物。
- 不通过简单改回 `ubuntu-24.04` 代替显式基线约束；若后续证明仍需 runner 调整，应先修订本卡。

### 成功定义

- Linux x86_64 GNU release 构建显式向 C 依赖传入 `-march=x86-64 -mtune=generic`。
- 构建前直接检查实际环境变量，缺失或错误时 fail-closed。
- 新 tag 产生的官方 `libra-linux-amd64` 经 SHA-256 身份核对后，在 N5105 上执行 `--version` 成功。
- 本卡全部本地、远端及实机门通过后，才可标记完成。

## 使用规则

- 推进 ISA-01 时，必须在同一变更中更新本文件与 [`plan-status.md`](plan-status.md)，时间用 `YYYY-MM-DD HH:MM:SS UTC`。
- 证据只写入本计划与 `plan-status.md` 的状态行，不新建证据文件或目录。
- 本计划不复制长期路线图；`plan-long.md` 只在「日期计划索引」登记一行，不新增 LR。
- 创建任务不是开工。ISA-01 保持 `pending` / 空，直到维护者明确授权实施。

## 事实基线

| 类别 | 当前事实 | 证据 |
|---|---|---|
| 受影响 CPU | Intel Celeron N5105，支持 SSE4.2/AES/SHA-NI，不支持 AVX、AVX2、BMI2 | 本机 `lscpu`，2026-11-05 |
| 首发复现 | 系统 Libra v0.30.152 执行 `libra --version` 退出 132，无正常版本输出 | 本机直接执行；SHA-256 `7a8c60d4f5528aee2fc202e196c1c8322f1e3a754116b05d0f46105d4e9b1641` 与官方 v0.30.152 `.sha256` 一致 |
| 最新版本仍复现 | 官方 v0.30.154 `libra-linux-amd64` 同样以 SIGILL 失败 | 官方 URL 下载后本机执行；stable manifest SHA-256 `2f45e46c559584737321e9a5f248db192eba56093157004a5cc6ebc075ad734b` |
| 崩溃指令 | v0.30.152 在关闭 ASLR 后，`si_addr=0x55555d35a0cb`，对应 ELF vaddr `0x7e060cb`，字节 `c5 f9 6e c1`，即 `vmovd %ecx,%xmm0` | `setarch -R strace ... libra --version` + `objdump` |
| 最新崩溃指令 | v0.30.154 对应 ELF vaddr `0x7df1dcb`，同样为 `vmovd %ecx,%xmm0` | 官方内存 fd 执行 + `objdump` |
| 源码归属 | 崩溃点对应 AWS-LC `OPENSSL_cpuid_setup` 写入 `OPENSSL_ia32cap_P[0..3]`；该函数负责能力探测，无法在写入前依赖 AVX 分派 | `aws-lc-sys 0.45.0` crate：`aws-lc/crypto/fipsmodule/cpucap/cpu_intel.c:170-191,272-275` |
| 编译器机制 | 同一组四个 32 位写入在 `-march=x86-64` 下生成 `movd`，在 `-march=x86-64-v3` 下生成 `vmovd` 等 VEX 指令 | 本机 GCC 15.2 对相同 C  probe 的 `-S` 对照 |
| 构建配置 | Linux amd64 使用 `ubuntu-latest`，构建只有 `RUSTFLAGS="-C strip=symbols"`，无 C ISA 约束 | `.github/workflows/release.yml:28,80-89` |
| runner 变化 | GitHub 自 2026-10-19 开始把 `ubuntu-latest` 迁移到 Ubuntu 26.04，计划 2026-11-19 完成 | `actions/runner-images#14748` |
| 同类外部事故 | GitHub Ubuntu 26.04 x64 为 amd64v3，GCC 默认 `-march=x86-64-v3`，已导致其它项目的预编译二进制在旧 CPU 上 SIGILL | `borgbackup/borg#10342`（独立旁证，不替代本机证据） |
| 当前锁定依赖 | `aws-lc-rs 1.18.1`、`aws-lc-sys 0.45.0` | `Cargo.lock:993-1006` |
| 创建时源码锚点 | `Build Binary (Unix)` 仍只有 `RUSTFLAGS="-C strip=symbols"`，无 C ISA 环境变量 | `.github/workflows/release.yml:80-89`，2026-11-05 07:51:26 UTC 复核 |
| 后续发布 | GitHub Release `v0.30.155` 发布于 2026-11-05T06:30:06Z，晚于 v0.30.154 复现 | `repos/libra-tools/libra/releases`；本机未运行该产物，不假定已修复 |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-ISA-01 | release 构建未限制 C 编译器 ISA，amd64v3 runner 将基线 C 代码编译为 VEX/AVX | 通用 Linux amd64 产物在 N5105 等旧 CPU 上启动即崩溃 | 上述 SIGILL、反汇编、源码及 runner 证据闭环 | ISA-01 |

## 与其它计划的关系

| 计划/文档 | 关系 | 本计划处理 |
|---|---|---|
| `plan-long.md` | 无对应 LR | 只登记日期计划索引，不新增长期能力 |
| 既有日期计划与 `issues/` | 无同名 x86-64 / AWS-LC 修复卡 | 不把历史计划当作本问题已处理 |
| `.github/workflows/release.yml` | 2026-11-05 07:51:26 UTC 写者刷新 | 未发现未收口卡片把该文件列入 `Implementation write set`。已收口计划只把它当作 D 组观察面，或历史上改过它。当前不补依赖边。ISA-01 真正开工前必须再刷新；出现相交写者时先补具体边 |
| `plan-status.md` | 全仓共享状态账本 | 本次只追加本计划一行。后续修改前重读，不得覆盖其它计划未提交的状态 |

本计划不采用 v2.16 具名 `code-only` 模式，按普通 `independent` 发布合同执行。

## 已决议设计决策

### ADR-ISA-01：在 Unix 构建步骤设置 target-specific CFLAGS，并检查实际环境值

- **Status:** Accepted
- **Context:** 崩溃来自 C 编译器在 AWS-LC CPUID 初始化代码中生成的 VEX 指令，而非 Rust 默认 target，也非 ML-KEM 的 AVX2/BMI2 分派缺陷。
- **Decision:** 在 `Build Binary (Unix)` 步骤设置 `CFLAGS_x86_64_unknown_linux_gnu: "-march=x86-64 -mtune=generic"`；在执行 `cargo build` 前，仅当 matrix target 为 `x86_64-unknown-linux-gnu` 时检查该环境变量逐字等于预期值，否则报错退出。
- **Alternatives considered:**
  - 仅 pin `ubuntu-24.04`：可作临时缓解，但不把基线要求固定在发布合同内，未来 runner 变化仍可能复发。
  - 升级 AWS-LC：不能撤销编译器已经生成的 VEX 指令，与本次根因不匹配。
  - 新增 Rust compat target：需要触碰 `Cargo.toml` 等注册面，且简单文本 grep 容易匹配到守卫自身；本卡改用构建时的实际环境值检查。
  - 扫描最终二进制要求零 AVX：会误杀合法的运行时分派汇编，不能作为正确判据。
- **Consequences:** target-specific 变量不影响 macOS、Linux ARM64 或 Windows；AWS-LC 等 C 依赖保留基线 C 实现及其既有运行时分派优化汇编。
- **Revisit when:** 实机仍 SIGILL、存在不经该 CFLAGS 的静态预编译对象，或发布平台合同改为显式要求 x86-64-v2/v3。

## 全局工程约束

继承模板 v2.16 的 GC-01～GC-13。本计划不改 SQLite、GlobalConfig 或 SystemConfig，GC-13 的迁移 allowlist 为 `N/A`。

## 执行检查必备需求（强制）

继承模板 v2.16 的 ER-01～ER-14 与 G-01～G-11。任一适用门未真实通过，不得把 ISA-01 标成 `done` / `complete`。

## 执行约束

- 继承模板 v2.16 的 GC-01～GC-13、ER-01～ER-14 与 G-01～G-11。
- 本卡命中 **ER-13 T-1**（修改 `.github/workflows/**`），C 组必须在 bump 后树运行双环境完整 nextest；focused 检查不能替代全量。
- 发布集成证据按 ER-14 使用 nextest；`cargo test --all` 只能诊断。
- 本卡无命令、错误码、schema、数据库或网站文档变化；不触碰 `docs/commands/**`、网站、`COMPATIBILITY.md` 与 `docs/error-codes.md`。
- 实施证据只写入正式计划文件，不新建 evidence/baseline/scratch 文件或目录。

## 依赖登记表

无本计划外前置依赖。发布窗口仍受 ER-12 串行规则约束。

## 实施顺序

1. `ISA-01`（无计划内前驱）。

## 任务卡

#### 任务卡粒度审计表

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ISA-01 | implementation | Linux x86_64 GNU release C 编译基线 | 不可变产物不覆盖，以前滚 patch 修复 | yes | yes | 4/8 | 6/8 | 1/1 | S | none | no-overlap（2026-11-05 07:51:26 UTC 复核；开工再刷新） | independent | N/A | N/A |

### Task ISA-01: 固定 Linux x86_64 GNU release 的 C 编译基线并实机验收

**Task type:** `implementation`

**Lifecycle / Acceptance:** `pending` / 空

**Issue:** [#1060](https://github.com/libra-tools/libra/issues/1060)

**Description:** 为 Linux x86_64 GNU release 的 C 依赖显式恢复 x86-64 基线，并在构建前 fail-closed 检查实际环境值，消除 amd64v3 runner 令通用官方二进制在旧 CPU 上启动即 SIGILL 的回归。

**Out of scope:**

- AWS-LC 版本升级：与本根因不同轴，尚未排期；出现独立安全或功能需求时另立卡。
- 其它 release 平台与 Rust `RUSTFLAGS`：本卡永久非目标，当前证据不涉及。
- 全文件零 AVX 扫描：永久非目标，会误伤合法运行时分派代码。
- 重新发布旧 tag：永久非目标，release 对象不可变。

**Current evidence:**

| 事实 | 证据 |
|---|---|
| 当前 Linux amd64 构建未设置 CFLAGS | `.github/workflows/release.yml:80-89` |
| N5105 无 AVX，官方 v0.30.152/v0.30.154 均 `--version` SIGILL | 本文“事实基线”的本机执行记录 |
| 崩溃指令位于 AWS-LC CPUID 结果写入，属编译器生成的 VEX | `cpu_intel.c:272-275`；两版官方二进制相同 `vmovd` 反汇编 |
| `-march=x86-64` 可将同类写入从 `vmovd` 恢复为 `movd` | GCC 15.2 `-S` 对照 probe |

**Acceptance criteria:**

- [ ] `Build Binary (Unix)` 仅对 `x86_64-unknown-linux-gnu` 使用 `CFLAGS_x86_64_unknown_linux_gnu="-march=x86-64 -mtune=generic"`，其它 target 的构建语义不变。
- [ ] Linux x86_64 GNU 在 `cargo build` 前检查实际环境变量值；缺失或不为上述精确值时非零退出并给出可操作错误，且自检不依赖可能匹配自身的文本 grep。
- [ ] 新 tag 的官方 `libra-linux-amd64` 经发布方 `.sha256` 核对后，在 Intel Celeron N5105 上执行 `--version` 退出 0，并输出该 tag 对应的 `libra <version>`。
- [ ] `Docs and compatibility impact` 所列 N/A 判定仍成立；没有借机修改命令、网站、兼容矩阵、错误码或其它平台。

**Verification:**

- [ ] `actionlint -oneline .github/workflows/release.yml`
- [ ] `source .env.test && source .env.live-test && cargo nextest run --test compat_release_rclone_env_guard`
- [ ] `source .env.test && source .env.live-test && cargo nextest run --test compat_release_homebrew_verify`
- [ ] `source .env.test && source .env.live-test && cargo nextest run --test compat_keyring_feature_gate_guard`
- [ ] 手工官方产物身份门：从 `https://download.libra.tools/libra/releases/v<VERSION>/libra-linux-amd64` 下载到仓外 `/tmp/opencode/libra-isa01/`，将实际 SHA-256 与同目录 `libra-linux-amd64.sha256` 逐字核对；不一致即 FAIL，不进入运行门。
- [ ] 手工 N5105 实机门：在上述身份门通过的同一路径执行 `./libra-linux-amd64 --version`，记录退出码与版本输出；必须 exit 0 且为 `libra <VERSION>`。若仍 SIGILL，本卡 STOP，不得以静态扫描或其它机器结果替代。

**Full-suite trigger:** `T-1: 修改 .github/workflows/release.yml；C 组第④步必须运行 source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`

**Dependencies:** 无；开工复核若发现 `release.yml` 相交写者，先补具体任务/DEP 边。

**Deliverables:** N/A（implementation）

**Implementation write set:**

- `.github/workflows/release.yml`
- 正式计划文件与 `docs/development/plan/plan-status.md` 仅按模板登记状态，不改变产品行为。

**Release write set:** `Inherited`（ER-08 权威版本面集合 + 工具链刷新的 `Cargo.lock` + release artifact）

**Files likely touched:** `.github/workflows/release.yml`；计划状态文件。

**Docs and compatibility impact:** N/A。仅修正 CI 构建 ISA 与构建前自检，不改变 CLI 命令、参数、输出、错误码、安装接口或支持平台名称，因此不修改 `docs/commands/**`、相邻网站文档、`COMPATIBILITY.md`、`docs/error-codes.md` 或测试索引。

**Rollback mode:** `immutable-release`

**Migration and rollback:** 无数据迁移。已发布对象不覆盖；若本卡产物仍有兼容缺陷，停止采用该版本并前滚一个新 patch 修复。受影响 CPU 用户只能安装已经通过 N5105 实机门的新版本；本卡不宣称任何未实测旧版本可安全降级。

**Security and privacy:** 不新增 secret、网络凭据或文件访问面；不改变现有 OIDC、R2、签名清单与 release 权限边界。

**Performance budget:** 只把通用 C 代码限定到基线 x86-64；保留依赖已有的运行时优化分派，不新增运行时扫描或热路径工作。

**Estimated scope:** `S`（1 个 CI 行为落点，1 个生产配置文件）

**Version increment:** `patch`

**Release boundary:** `independent`

**C/D coverage from:** `self`

**Granularity:** `type=implementation; axis=Linux x86_64 GNU release C 编译基线; recovery=不可变产物不覆盖并以前滚 patch 修复; complete=yes; self-contained=yes; AC=4/8; VER=6/8; landing=1; prod-files=1; scope=S; deps=none; writeset=no-overlap（2026-11-05 07:51:26 UTC 复核，开工再刷新）; release=independent; split-from=N/A; exception=N/A`

## 测试矩阵

| 类别 | 必须覆盖 | Target / command |
|---|---|---|
| workflow 静态 | release YAML 与表达式 | `actionlint -oneline .github/workflows/release.yml` |
| compat | rclone/凭据边界、Homebrew 验证、keyring feature gate | 本卡 Verification 的三个 nextest target |
| 全量 | T-1 修改 workflow，bump 后双环境全量 | `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` |
| 官方产物 | URL、版本与 SHA-256 身份一致 | 本卡 Verification 手工官方产物身份门 |
| 真实旧 CPU | N5105 无 AVX，启动不再 SIGILL | 本卡 Verification 手工 N5105 实机门 |

## 追溯表

| 任务 | 来源/证据 | Libra 落点 | 文档/兼容动作 | 指定测试 |
|---|---|---|---|---|
| ISA-01 | GAP-ISA-01；官方二进制 SIGILL + AWS-LC 源码 + amd64v3 runner | `.github/workflows/release.yml` | N/A，无公开命令文档变化 | 三个既有 release compat target + T-1 全量 + 官方产物 N5105 门 |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | 根因证据与卡片经维护者采纳 | 本草稿及当前源码锚点 | N/A |
| M1 | ISA-01 全部 A/B/C/D 与实机门通过 | 新 patch tag、release 全 job、官方 SHA、N5105 版本输出 | 不可变产物不覆盖；缺陷以前滚 patch 处理 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| 构建前 CFLAGS 自检失败 | 无新 artifact | 修正 workflow 后重跑该 job | 带错误 ISA 继续上传 |
| 新官方产物在 N5105 仍 SIGILL | 已发布的不可变坏产物保留 | 停止把本卡标完成，前滚下一个 patch | 覆盖旧 tag，或用其它机器结果代替 N5105 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| CFLAGS 未传入全部 C 翻译单元，或存在预编译 AVX 对象 | 中；新产物仍可能 SIGILL | 构建前检查实际环境值，并用 N5105 运行官方产物 | ISA-01 |
| `release.yml` 在开工前被其它卡改写 | 中；补丁冲突或语义漂移 | 开工前重读 `:80-89` 并重查写者 | ISA-01 |
| v0.30.155 及更新产物尚未在本机复验 | 低；不能据此宣布已修复或未修复 | 验收只认本卡新 tag 的官方产物 | ISA-01 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积成本 | 预算/上限 | 验证 |
|---|---|---|---|---|
| release C 编译 | 基线 x86-64 指令选择 | 无新增运行时扫描 | 不要求全文件零 AVX；保留既有运行时分派 | N5105 `--version` 与既有 release compat |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md`：`N/A`，无公开兼容面变化。
- [ ] `docs/commands/<cmd>.md`：`N/A`。
- [ ] `docs/commands/zh-CN/<cmd>.md`：`N/A`。
- [ ] `docs/development/commands/<cmd>.md`：`N/A`。
- [ ] `docs/error-codes.md`：`N/A`。
- [ ] `tests/INDEX.md`：`N/A`，不新增测试 target。
- [ ] `Cargo.toml` `[[test]]`：`N/A`。
- [x] `plan-long.md` 日期计划索引已登记；无 LR 状态需要更新。

## Codex review log

尚未评审。本表不预填 `PASS`。

| Round | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|
| 未开始 | 计划创建 | 未评审 | 无 | 无 | 2026-11-05 07:51:26 UTC 正式建卡 |

## 非目标与延后项

| ID | 延后内容 | 原因 | 重启条件 | 承接位置 |
|---|---|---|---|---|
| DEFER-ISA-01 | AWS-LC 依赖升级 | 与本次 VEX 编译根因不同轴 | 出现独立安全或功能需求 | 新任务卡 |
| DEFER-ISA-02 | 追认 v0.30.155 是否仍 SIGILL | 创建任务时未下载运行 | 若需确定现网最新产物状态 | 本计划事实基线补记，不单独开卡 |

## 完成判据

- [ ] ISA-01 的 4 项 AC 与 6 项 Verification 全部真实通过并记录。
- [ ] ER-04 的 fmt、严格 Clippy、T-1 双环境完整 nextest、release build、安装、签名+DCO、PR/main 适用检查全部通过。
- [ ] 新 tag 的 `release.yml` 全部适用 job 成功；不得用本地构建冒充官方产物。
- [ ] 官方 linux-amd64 SHA-256 与发布方值一致，N5105 `--version` exit 0。
- [ ] 计划与 `plan-status.md` 同步为最终真实状态；任何未执行项不得勾选。

## 评审结论与修订记录

| 维度 | 结论 | 修订动作 |
|---|---|---|
| 合理性 | 根因已由真实 CPU、官方二进制、反汇编、源码与编译器 probe 闭合 | 从最初“怀疑 aws-lc C 代码”收敛为可证明的 amd64v3 编译基线问题 |
| 可行性 | 单文件 CI 修改，独立发布，实机可判定 | 无 |
| 粒度 | 单一行为轴，S，AC=4、VER=6 | 不新增 compat target，避免额外注册面 |
| 依赖与顺序 | 无计划内前驱；开工复核 release.yml 写者 | 有冲突时先补边 |
| 完整性 | 包含构建自检、既有 compat、T-1 全量、官方身份与实机门 | 不以静态扫描代替运行 |
| 安全性 | 不扩大 release 权限或 secret 面 | 无 |
| 功能正确性 | 直接针对导致崩溃的 VEX 编译机制 | 无 |
| 接口兼容 | 无 CLI/schema/协议变化 | 无 |
| 数据流与控制流 | 构建前 fail-closed，避免错误配置继续发布 | 无 |
| 性能与容量 | 保留运行时优化分派 | 无 |
| 可靠性与容错 | 实机仍失败则 STOP，不标记完成 | 无 |
| 可维护性 | target-specific 配置与实际环境自检同步骤维护 | 无 |

### 修订历史

| 时间 | 触发 | 变更内容 | 原卡 → 新卡 | 受影响的引用 |
|---|---|---|---|---|
| 2026-11-05 06:25:46 UTC | 根因核实后初稿 | 新建 ISA-01 外部草稿；选择 target-specific CFLAGS 与构建时实际环境值检查 | N/A → ISA-01 | 全文 |
| 2026-11-05 07:51:26 UTC | 用户授权正式建卡 | 迁入仓库并登记状态索引。`release.yml` 无未收口实现写者，不补依赖边。ISA-01 仍为 `pending` / 空 | ISA-01 | 本文件、`plan-status.md`、`plan-long.md` |
| 2026-11-05 08:58:07 UTC | 用户授权登记 Issue、提交并推送计划、创建 PR | 创建 [#1060](https://github.com/libra-tools/libra/issues/1060) 并写入本计划。不实施 `release.yml`，ISA-01 仍为 `pending` / 空 | ISA-01 | 本文件、`plan-status.md`、`plan-long.md`、Issue #1060 |
