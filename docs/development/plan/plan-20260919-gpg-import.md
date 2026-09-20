# GnuPG HOME 密钥导入仓库 vault 计划（2026-09-19）

> **模板版本:** `v2.6`。**评审已通过（2026-09-19，R29 双 PASS）；尚未开工**——Phase 0 剩余项（`plan-long.md` 索引、DEP 复核、`gpg --version` 证据、VG-00 go 结论、ADR Accepted）完成后依序执行。所有未执行任务卡、ER-13 全量门及完成判据的全量收口门一律运行 `source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。`cargo test --all` 只供诊断，不能当通过证据。
>
> **本文件只规划任务，不宣称已实现。** 任何卡在最新一轮 Codex 与 Claude **双字面 `VERDICT: PASS`** 之前不得标 `in-progress`；家族子卡在 VG-09 发布完成前不得标 `done`（ER-04：`done` 以 `complete` 为前提）。用户 2026-09-19 指示：本计划由 Codex 与 Claude 双评审，直至两者均通过。**R29 已达成**（同版 P0/P1/P2 全 0）。
>
> **ID 前缀:** `VG-*`、`ADR-VG-*`、`GAP-VG-*`、`DEP-VG-*`、`GC-VG-*`、`ER-VG-*`、`REL-VG-*`、`DEFER-VG-*`。`G-*` / `ER-*` / `GC-*` 以 [`plan-template.md`](plan-template.md) 为唯一权威。

## 文档职责

本文把「从开发者 HOME 目录的 GnuPG 密钥环导入既有 PGP 签名密钥，并在本仓库中直接用于签名/验证」收敛为一份日期计划。开发者一次导入后，`commit`/`tag -s`/`merge -S`/`push --signed` 用该密钥产生 OpenPGP 签名，`merge --verify-signatures`/`tag -v` 用同一公钥验证；私钥以仓库 vault unseal key 加密存于仓库本地配置库。

### 适用范围

- 新增 `libra config import-gpg-key [--key|--list|--file|--passphrase-file|--replace]`（发现→选码→解保护→资格判定→持久化→替换/历史）。
- 新增 `libra config export-gpg-key [--out|--fingerprint]`（仅公钥）与 `libra config remove-gpg-key [--force]`（移除/生成回落）。
- 既有 `config generate-gpg-key` 在 `source=imported` 时的原子迁移；首度导入的 `vault.signing` 启用策略。
- 签名派发（`pgp_sign`）与验证派发（`pgp_verify`）的 imported 路径、signing-subkey 资格、issuer 定位与验证时刻规则。
- `config list --gpg-keys` 元数据面；`vault.gpg.*` 键空间与 redaction（get/list/JSON/reveal）。
- 同步：`docs/commands/config.md`、`commit.md`、`tag.md`、`merge.md`、`push.md`（EN + zh-CN）、对应 `docs/development/commands/*.md`、`COMPATIBILITY.md` 五行、`../libra-backend` 网站五页。

### 非目标

- **不改** vault 初始化与 unseal key 路径（plan-20260919 GCX-03，DEP-VG-01）。
- **不新增** 第三方未导入公钥验证（DEFER-VG-06）；不做 SSH 签名（DEFER-VG-02）。
- **不做** 硬件令牌直签（DEFER-VG-01）；passphrase 缓存/钥匙串（DEFER-VG-07）。
- **不做** 多活动密钥/按提交 `-u` 选码（DEFER-VG-03）；keyserver/WKD（DEFER-VG-04）；私钥导出/备份包（DEFER-VG-05）；哈希算法协商（DEFER-VG-08）。
- **不迁移** schema；全部新键 additive。

### 成功定义

- 一次 `import-gpg-key` 后签名/验证可用；替换/移除/再生成后历史签名仍 Good。
- 私钥与口令零泄漏：argv/stdout/stderr/日志/trace/JSON/错误载荷均无；`get`/`list` redaction，`--reveal` 拒绝。
- **导入、签名、验证同批上线**（REL-VG-01）：无「已导入但未签名」「签名了但验证 Bad」的中间用户可见状态。
- 首度导入在 `vault.signing` 未设置时启用签名；显式 `false` 保持关闭并提示。
- 全部卡 `done`/`complete`，全量收口门绿，文档与矩阵同批。

## 事实基线

> 2026-09-19 成稿；R1/R2/R3 修订复核。行号开工日按 ER-02 再刷。

| 事实 | 证据 |
|---|---|
| vault PGP：生成为内部密钥、签/验走 libvault；生成**无条件覆盖** `vault.gpg.pubkey` 且 config 写错误被忽略 | `src/internal/vault.rs:156`、`:198`、`:211`、`:254`、`:627-631`（`upsert_config_value` 调用点）；密钥名 `libra-signing`（`:37`） |
| 签名编码辅助 | `src/internal/vault.rs:307/429/445` |
| vault 初始化与 unseal key | `src/internal/vault.rs:112/556/462/519` |
| 加密原语 | `src/internal/vault.rs:66/89`；`src/internal/config.rs:1007` |
| **redaction 真实行为**：`render_get_value` 对 `!encrypted` 早退；internal 判定在 encrypted 之后；`--reveal` 对 internal key 报错；list/JSON 只看 `e.encrypted` | `src/command/config.rs:1817-1836`、`:1952-1956`、`:2319/2389/2462` |
| `is_vault_internal_key` 不含 `vault.gpg.seckey_enc`；`is_sensitive_key` 子串不命中 `seckeyenc` | `src/internal/config.rs:2587-2596`、`:2536-2579` |
| prefix 读取按 key→id 排序 | `src/internal/config.rs:392-400` |
| 签名决策链：`vault.signing==true` 才签（未设置=不签）；`commit.gpgSign` 可强制/关闭 | `src/command/commit.rs:2586`、`:224`；`src/command/history_config.rs:77` |
| 签名调用点 | `src/command/commit.rs:1385/1396/1522/1533`；`src/internal/tag.rs:176`；`src/command/push.rs:1188`；merge 经 `vault_sign_commit` |
| 验证调用点 | `src/internal/tag.rs:305`；`src/command/merge.rs:1790/1792`；`verify_commit_signature` |
| config 现有密钥面与 local-only 先例 | `src/command/config.rs:517/494/3036/2531/568/2556/3135` |
| libvault 0.4.0：PGP 仅 generate/sign/verify，**import 不支持 PGP** | `Cargo.toml:123`；`.../libvault-0.4.0/src/modules/pki/path_keys.rs:291/340/485`；`types.rs:124` |
| `pgp 0.19`：primary + `secret_subkeys`；`to_armored_*` 原样输出（可能受保护）secret 包；issuer 子包存在 | `Cargo.toml:169`；`pgp-0.19.0/src/composed/key/{secret.rs:17-18,shared.rs:58,builder.rs:272}`；`packet/signature/subpacket.rs:37/51` |
| 无 `GNUPGHOME`/`~/.gnupg` 处理 | `rg` 仅 `src/internal/ai/sandbox/policy.rs:487` |
| 现有签名测试面 | `tests/command/config_test.rs:1206/1420/1471/1529`；`commit_test.rs:1841/1891`；`merge_test/gpg_sign.rs:59..187`；`init_test.rs:83/218/642/704`；`src/command/push.rs:5033/5057` |
| 文档现状与参照 | `docs/commands/{init,tag,merge}.md` 与 `COMPATIBILITY.md:378/382` 的 vault-only 表述；本机 `git 2.55.0`；`gpg --version/--with-colons/--export-secret-keys` |

### 当前缺口

| ID | 缺口 | 影响 | 证据 | 计划动作 |
|---|---|---|---|---|
| GAP-VG-01 | 无导入入口 | 无法用既有签名身份 | 事实基线 1/9 | VG-01/10/02/11/03 |
| GAP-VG-02 | libvault 不支持 PGP import | 存储/签名自持 | 10 | ADR-VG-02 / VG-03 |
| GAP-VG-03 | 无来源/指纹/签名 key id 元数据；`PGP 2048` 硬编码 | 展示错误 | 9 | VG-03 / VG-06 |
| GAP-VG-04 | 无历史允许列表；替换/移除/再生成破坏验证 | 静默不可验证 | 1 | ADR-VG-04/06；VG-13/05/08 |
| GAP-VG-05 | 无 GNUPGHOME/版本/`--file`/口令策略 | CI/无 agent 不可用 | 11 | ADR-VG-01/10；VG-01/10/02 |
| GAP-VG-06 | redaction 不覆盖新键（三处读路径） | 内部密文泄漏 | 5/6 | ADR-VG-02；VG-12 |
| GAP-VG-07 | subkey 资格/验证时刻不完整 | 选中不被 GnuPG 认可的密钥 | 10 | ADR-VG-09；VG-11/05 |
| GAP-VG-08 | 解保护→未受保护可移植证书重建路径未定义 | 导入不可实现 | 10 | VG-00 spike + ADR-VG-10 / VG-02 |
| GAP-VG-09 | 独立发布会暴露中间坏状态 | 用户看到 Bad | Codex R2 | ADR-VG-11 / REL-VG-01 / VG-09 |
| GAP-VG-10 | 首度导入未定义 `vault.signing` 策略（`--vault=false` 仓导入后仍静默未签名） | 导入成功但无签名 | 事实基线 7；`commit.rs:2597` | ADR-VG-12 / VG-03 |
| GAP-VG-11 | `generate-gpg-key` 覆盖 pubkey 且忽略 config 写错误，再生成非原子 | vault/config/history 不一致 | 事实基线 1 | ADR-VG-06 第 6/7 条；VG-03（空窗守卫）、VG-14（迁移） |

## 与其它计划的关系

| 计划 | 关系 | 约束 |
|---|---|---|
| [`plan-20260919.md`](plan-20260919.md) | **I–I 相交**：GCX-01/02 写 `src/command/config.rs`+config 文档+`COMPATIBILITY.md`+网站 config 页；GCX-03 写 `src/internal/vault.rs`+`config_test.rs`+同文档。**现状：GCX-01 `done`（v0.23.1，`8ce1eed`），GCX-02/03 `pending`** | `DEP-VG-01`；全卡序列化其后（未满足即 `blocked`，属预期外部阻塞） |
| [`plan-20260918.md`](plan-20260918.md)（执行中） | 无写集相交 | 发布由单一发布者串行 |
| [`issues/490.md`](issues/490.md) SW-03、[`issues/473.md`](issues/473.md) IN-07 | 写集含 `src/command/commit.rs` | `DEP-VG-03`（VG-04/VG-05 文档与矩阵行互斥） |
| [`issues/470.md`](issues/470.md) FM-02 | 写集含 `src/command/merge.rs`、merge 文档、`COMPATIBILITY.md` merge 行、网站 merge 页 | `DEP-VG-03` |
| [`issues/476.md`](issues/476.md)、[`issues/479.md`](issues/479.md)、[`issues/483.md`](issues/483.md) | 无相交 | 无 |

## 评审结论与修订记录

- **计划级评审:** ✅ **已通过（2026-09-19，R29）** —— Codex 与 Claude 对同一修订版（R28 文本，含 R27 技术修订）双字面 `VERDICT: PASS`，P0/P1/P2 全 0。证据：`/tmp/issue-gpg/review/{codex,claude}-plan-r29.log`。
- **R29（收口）:** Codex `PASS` + Claude `PASS`（同版，P0/P1/P2 全 0）；R28 Claude 亦 `PASS`，R28 Codex 仅两处文书（已修）。
- **R1:** Codex `FAIL`（P0 0/P1 13），Claude `FAIL`（P0 0/P1 2）→ 15 项 P1 修订（验证允许列表、生成公钥持久化、`--replace` 归属、历史顺序、redaction 扩展、选码协议、解保护 ADR、subkey ADR、拆卡、文档写集、local-scope、DEP-VG-02、修订史引用、GCX 标注、Claude redaction 复核）。
- **R2:** Codex `FAIL`（P0 0/**P1 16**），Claude `FAIL`（P0 0/P1 1，P2 4）→ 重排为 `VG-00` spike + `VG-01..VG-09`（家族 VG-01..05 + VG-09 发布点）并修：redaction encrypted=true+predicate-first+reveal 拒绝、解保护/重建 ADR-VG-10、subkey 资格、ADR-VG-11/REL-VG-01、DAG 修正（VG-05 不依赖移除）、生成生命周期、G-09、AC 计数、回滚模式、DEP-VG-04 版本闸门、DEP-VG-02 二义 VCS、D 组证据、发布者具名、VG-07 导出行为、零泄漏守卫。
- **R3:** Codex `FAIL`（**P0 1**/P1 11），Claude `FAIL`（P0 0/P1 1，P2 4）→ 本文件为 R3 修订版：
  - **P0 修复：** VG-08 恢复序列禁止触碰 `history.*`/`generated_pubkey`；显式「可删键 allowlist」+ 恢复测试。
  - **G-08/ER-04：** 家族子卡 `C/D coverage from` 全部改为 VG-09；子卡在 VG-09 完成前保持 `in-progress`/`locally-accepted`，**不得标 `done`**；Phase 1 措辞同步。
  - **G-03 重计与再拆：** 依单谓词如实计数：原 VG-01 拆出 VG-10（选码与作用域）；原 VG-02 拆出 VG-11（签名资格与选择）；原 VG-03 拆出 VG-12（redaction 读取面）与 VG-13（替换与历史）。常规卡保持单谓词 ≤8；VG-05/VG-11/VG-07/VG-08 的机械门族按 R4 的 EX-VG-01 登记并逐门列举。
  - **G-04 改级：** 新增/变更公开命令或输出面的卡（VG-01、VG-02、VG-03、VG-06、VG-07、VG-08、VG-10、VG-12）标 `M`；纯内部行为卡保持 `S`。
  - **G-09 声明：** 每张保留卡在 Description 写明「保留 <轴>，拆出 VG-xx（…）/VG-yy（…）」；新卡写明「自 VG-0X 拆出」；审计表 `split-from` 同步。
  - **DEP-VG-02：** 一律 `cd ../libra-backend`；两种 VCS 均强制 `cf`（Libra 路径解析 `libra status --short --branch` 输出的分支名）；网站路径具体化为 `../libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`；`.libra`/`.git` 同时存在或同时不存在 = 硬阻塞。
  - **VG-00：** 增加「变更文件 allowlist = 仅计划文档」守衛；sentinel 守衛补实际脚本/捕获文件/口令哨兵/三条 `rg` 退出码分支。
  - **首度导入签名策略（新 ADR-VG-12 / GAP-VG-10）：** `vault.signing` 未设置 → 导入成功后置 `true`；显式 `false` → 保持并提示；两分支都有测试。
  - **生成面原子迁移（ADR-VG-06 第 6/7 条 / GAP-VG-11）：** staged/compensated 序列（快照→生成→写新公钥→写 source），各步故障注入与恢复；`generate_pgp_key` 的 config 写错误必须传播。
  - **资格/验证 fixture：** 补 primary-only、issuer-absent（候选上限 16，超出 fail closed）、expiration-at-signature-time 具名 fixture；`五类/七类` 口径统一为「五类拒绝 + primary-only + issuer-absent」。
  - **VG-07：** 新增 flag/输出真值表（stdout/`--fingerprint`/`--out`/`--json`/`--machine`/`--quiet` 组合），逐组合测试。
  - **VG-09：** 发布流程改为「先在确认 SHA 预建并推送 annotated tag → `gh release create --verify-tag`」，断言 `status,conclusion,jobs,event,headBranch,headSha` + deref SHA 相等 + job 集合 + CDN 断言。
- **R4:** Codex `FAIL`（P0 0/P1 7）；Claude `PASS`（P0 0/P1 0，评 R3 修订版快照）→ 本文件为 R4 修订版：
  - **G-03 门族型 EX（EX-VG-01）：** VG-05/VG-11/VG-07/VG-08 的 fixture/flag/fault 门族逐门计数必然超 8；按模板白名单登记 EX-VG-01，卡内新增「判据规范」块逐门列举可复制命令，分子写 `n/8@EX-VG-01`；其余卡保持单谓词 ≤8。
  - **守衛脚本字面化：** sentinel 捕获与 allowlist 守衛改为本计划内联的完整脚本（含捕获文件、哨兵、`<REDACTED>` 断言、基线差分与 `rg` 三支退出码），不再引用「模板」。
  - **VG-00 C/D：** `no-release` 卡 C/D coverage 指向 VG-09（结论随家族发布归档）；实施顺序的 `DEP-VG-02` 全局边收窄为「含网站页写集的卡」（不含 VG-00）。
  - **网站路径具体化：** 所有写集中的省略号改为 `../libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`；每张卡片加「网站页同步」验收门；后端目标页已有未提交改动（现 `cf` 检出中 `config.en.md`/`commit.en.md`/`push.en.md`）时为硬阻塞条件。
  - **再生成 helper 重构：** ADR-VG-06 改为先抽出「纯 vault 生成 helper（不写 config）」再把每一步 config 写入做成可失败并附「逐步补偿/残留表」；附带故障注入断言。
  - **VG-07 逐组合测试；VG-09 发布脚本：** 导出真值表每行一个具名测试；VG-09 给出完整 tag/release/run-watch/assertion 命令序列（含 8 个 job 的具名集合）。
  - **Claude P2：** 追溯表 7→5 修正；`history_count` 定义明确；VG-02 手工证据加 sentinel 口令核对；G-09 声明落地（见上）。
- **R5（2026-09-19）:** Codex `FAIL`（P0 0/P1 7），Claude `FAIL`（P0 0/P1 1，均评 R4 修订版快照）→ 本文件为 R5 修订版：① 四卡判据规范逐门映射具名命令，`exception=EX-VG-01`；② allowlist 守衛改为逐卡 allowlist 取补集、不掩盖失败、含已脏路径内容 diff；③ VG-00 卡内 C/D=VG-09（Claude P1）；④ DEP-VG-02 对脏页改硬阻塞（当前 `config.en.md`/`push.en.md` 需先处置）；⑤ 网站路径去 brace 展开；⑥ ADR-VG-06 改版本化键名 `libra-signing-<unix-ts>` + `generated_key_name` + 幂等重跑/补偿表；⑦ VG-09 可执行 tag/watch/jq/CDN 脚本；⑧ 删重複 R4/Claude 行。
- **R6:** Codex `FAIL`（P0 0/P1 9），Claude `PASS`（P0 0/P1 0，评 R5 修订版）→ R6 修订（见 Review log）。
- **R7:** Codex `FAIL`（P0 2/P1 7），Claude `PASS`（P0 0/P1 0，评 R6 修订版）→ R7 修订（见 Review log）。
- **R8:** Codex `FAIL`（P0 4/P1 8/P2 2），Claude `FAIL`（P0 1/P1 0）→ R8 修订（见 Review log）。
- **R9:** Codex `FAIL`（P0 3/P1 5），Claude `FAIL`（P0 0/P1 2/P2 1）→ 本文件为 R9 修订版：allowlist 后端前缀与目录匹配；ADR-VG-06 d2 回滚 (c)；VG-02 rc-ok 断言；VG-12 reveal 显式 rc；sentinel harness 隔离一次性仓库；VG-08 G1..G18 重排；ADR-02 补 `generated_key_name` + fresh/legacy 测试；VG-10 Granularity 补 `DEP-VG-04`；修订史 R8 行回填。

### 修订历史

| 日期 | 原因 | 变更 | 拆分 | 受影响章节 |
|---|---|---|---|---|
| 2026-09-19 | 用户指令成稿 | 初稿（4 卡） | 无 | 全文 |
| 2026-09-19 | Codex R1 + Claude R1 `FAIL` | R1 修订 | VG-04 → 04/05/06 | 全文 |
| 2026-09-19 | Codex R2 + Claude R2 `FAIL` | R2 重排（VG-00 spike、家族发布、9 卡） | 旧 VG-01/02/03 三轴拆 | 全文 |
| 2026-09-19 | Codex R3（P0 1/P1 11）+ Claude R3（P1 1/P2 4）`FAIL` | R3 整合修订（见 §评审结论 R3）；14 卡、family children VG-01..VG-05+VG-10..VG-13、C/D 归 VG-09、AC 重计、S→M、首度导入策略、生成面 staged 交易、DEP/D/导出/发布流程细化 | VG-01→VG-01+VG-10；VG-02→VG-02+VG-11；VG-03→VG-03+VG-12+VG-13 | 全文 |
| 2026-09-19 | Codex R4 `FAIL`（P0 0/P1 7）；Claude R4 `PASS`（P0 0/P1 0，评 R3 修订版快照） | R4 修订（见 §评审结论 R4）：EX-VG-01 门族 waiver + 四卡判据规范块；sentinel/allowlist 内联脚本；VG-00 C/D→VG-09；DEP-VG-02 边收窄；网站具体路径 + ER-VG-08 同卡验收；ADR-VG-06 纯 helper + 逐步补偿表；VG-07 逐行测试；VG-09 释放脚本；补 `landing/prod-files` 定义（Claude P2） | 无 | 全文 |
| 2026-09-19 | Codex R5 `FAIL`（P0 0/P1 7）；Claude R5 `FAIL`（P0 0/P1 1，评 R4 修订版快照） | R5 修订：四卡判据规范逐门映射具名命令 + `exception=EX-VG-01`；allowlist 守衛改为逐卡 allowlist 取补集且不掩盖失败；VG-00 卡内 C/D=VG-09；DEP-VG-02 脏页硬阻塞；网站路径去 brace；ADR-VG-06 改版本化键名 `libra-signing-<ts>` + `generated_key_name` + 幂等重跑；VG-09 可执行 tag/watch/jq/CDN 脚本；删重複 R4/Claude 行 | 无 | 全文 |
| 2026-09-19 | Codex R6 `FAIL`（P0 0/P1 9）；Claude R6 `PASS`（P0 0/P1 0，评 R5 修订版快照） | R6 修订（见 §评审结论 R6）：四卡判据规范逐门完整命令；逐卡 Allowlist 行 + 已脏路径内容 diff 脚本；纳秒键名 + probe 重试；staged (d1/d2) before-image 补偿；VG-09 `INTENDED_SHA` 非恒真；12 卡 Verification 写入网站页同步门（ER-级不计 VER）；VG-02/VG-12 完整 sentinel 命令序列；R5 修订史行改为双方结果 | 无 | 全文 |
| 2026-09-19 | Codex R7 `FAIL`（P0 2/P1 7）；Claude R7 `PASS`（P0 0/P1 0，评 R6 修订版快照） | R7 修订：VG-02 sentinel 改卡内 harness、VG-12 去 `--gpg-keys` 并移门到 VG-06、VG-09 建目录/发布说明 + 仓内 `tests/harness/release_cdn_gate.sh` + 聚合 sentinel 门、allowlist 硬化（无 `|| true`、规范化、untracked sha256、外部仓独立守衛）、VG-08 G17-G19 逐阶命令与 19/8@EX、ADR-VG-06 d2 before-image 与 (e) 可验证过渡不变量、VG-04/VG-05 generated_key_name 查找门与 VG-04 sentinel 门、依赖补齐、修订史尾段清理 | 无 | 全文 |
| 2026-09-19 | Codex R8 `FAIL`（P0 4/P1 8/P2 2）；Claude R8 `FAIL`（P0 1/P1 0） | R8 修订：VG-00 隔离 GNUPGHOME（GC-VG-03 例外）+ Q0；allowlist 仅对基线已脏 allowlist 路径快照；VG-02/VG-12 sentinel 改固定 fixture 口令 + 显式 rc 捕获 + `rg -F`；ADR-VG-06 (c) before-image；VG-08 G15 去重、18/8@EX；审计 deps 与卡面同步；VG-09 增 Allowlist、CHANGELOG 入写集/验收（版本节+内容断言）、聚合 sentinel 可执行化；VG-12 AC6 去 trace；Review log 顺序与 R4 摘要修正 | 无 | 全文 |
| 2026-09-19 | Codex R9 `FAIL`（P0 3/P1 5）；Claude R9 `FAIL`（P0 0/P1 2/P2 1） | R9 修订：allowlist 后端前缀修正（`../libra-backend/...`）与目录前缀匹配；ADR-VG-06 d2 补回滚 (c) `generated_pubkey`；VG-02 rc-ok==0 断言；VG-12 reveal 显式 rc；三个 sentinel harness 改隔离一次性 Libra 仓库（隔离 LIBRA_HOME、合并 trap、绝对 fixture 路径）；VG-08 门重排 G1..G18；ADR-02 键空间补 `generated_key_name` + fresh-init/legacy-fallback 两测试 | 无 | 全文 |
| 2026-09-19 | Codex R10 `FAIL`（P0 1/P1 4/P2 1）；Claude R10 `PASS`（P0 0/P1 0，评 R9 修订版） | R10 修订：`pgp` 移入 `[dependencies]`（Cargo.toml/lock 入 VG-02 写集、AC 与 build 门、granularity 2/2）；网站门统一 VCS 选择 helper（`.git`→`git status`、`.libra`→`libra status`）；allowlist 对全部基线脏路径做内容快照；VG-12 加 `--show-origin` 双分支门（AC 7/8、VER 6/8）；完成判据改 fmt/clippy/nextest 三连；修订史 R9 行回填 | 无 | 全文 |
| 2026-09-19 | Codex R11 `FAIL`（P0 0/P1 4）；Claude R11 `FAIL`（P0 0/P1 2） | R11 修订：VCS helper 统一为 `[ -e .git ]` + exactly-one-VCS（含 `.git` 文件态）、每处网站门/DEP/守衛一致；守衛後端 baseline/after 改用 helper；後端髒頁新增內容 diff 快照比對；VG-12 sentinel 實際捕獲 `config list --show-origin`（human+JSON）並納入零命中/redaction 斷言 | 无 | 全文 |
| 2026-09-19 | Codex R12 `FAIL`（P0 0/P1 3）；Claude R12 `FAIL`（P0 0/P1 1） | R12 修订：allowlist/后端的已脏内容快照改为工作树字节 sha256（含 staged 与 untracked）；VG-12 增 `--show-origin --json` 的 jq redaction 断言；ADR-VG-11 增 G-08 家族发布例外授权与依据（Codex R2 发现 + 用户指令）；守衛註解改全量快照说明 | 无 | 全文 |
| 2026-09-19 | Codex R13 `FAIL`（P0 0/P1 3/P2 2）；Claude R13 `FAIL`（P0 0/P1 1） | R13 修订：后端快照补 `MISSING` 处理；VG-12 jq 逐档断言；VG-09 增「先推送 main + `ls-remote` 校验远端 SHA 再建/推 tag」；合并重复 R12 修订史行；基线注释统一为全 sha256 | 无 | 全文 |
| 2026-09-19 | Codex R14 `FAIL`（P0 2/P1 2）；Claude R14 `PASS`（P0 0/P1 0，评 R13 修订版） | R14 修订：sentinel rg 改「既有 captures 檔案陣列」；VG-09 隔離倉先建基線提交再 `tag -s`；守衛改 NUL porcelain `dirty_paths` parser（rename 兩端/引號/目錄）＋目錄遞歸 sha256；修訂史合併為單一 R12 列 | 无 | 全文 |
| 2026-09-19 | Claude R15 `FAIL`（P0 0/P1 1）；Codex R15 待回填 | R15 修订：後端髒路徑抽取改用 `dirty_paths < backend-baseline.z`（與主倉同 parser，覆蓋 rename 兩端/引號/任意位元組） | 无 | 全文 |
| 2026-09-19 | Codex R15 `FAIL`（P0 0/P1 3） | R15 修订：守衛改 byte-safe Python 單一實作（NUL porcelain + rename 兩端 + root/backend allowlist 補集 + symlink-aware/目錄不跟隨快照） | 无 | 全文 |
| 2026-09-19 | Codex R16 `FAIL`（P0 1/P1 3）；Claude R16 `FAIL`（P0 1/P1 1） | R16 修订：守衛改 capture/verify 兩段式（不可變 baseline manifest；跨外部程序工作）；root/backend 共用 exactly-one VCS 選擇器與 fail-closed；digest 補 entry 名稱/型別/空目錄/symlink target；Allowlist 移除 repo 內 `allowlist.txt` | 无 | 全文 |
| 2026-09-19 | Codex R17 `FAIL`（P0 0/P1 3）；Claude R17 `PASS`（P0 0/P1 0） | R17 修订：manifest 改独占创建拒绝覆盖；持久化 root/backend VCS 工具并在 verify 比对（掉包即 FAIL）；JSON 改 `ensure_ascii=True` 保证代理转义路径无损 | 无 | 全文 |
| 2026-09-19 | Codex R18 `FAIL`（P0 0/P1 6）；Claude R18 `PASS`（P0 0/P1 0） | R18 修订：禁止卡内重捕获（仅经授权 supervisor 重置）；manifest 持久化 XY+lstat mode 并逐项比对；backend 仅在需要时探测；给出 guard.sh/allowlist 实体化命令；VG-01/10/13/04 landing 改 2/2；补十卡 ER-06a 文档 AC（ER 级不计 G-03） | 无 | 全文 |
| 2026-09-19 | Codex R19 `FAIL`（P0 0/P1 4/P2 1）；Claude R19 `PASS`（P0 0/P1 0） | R19 修订：ADR-VG-11 增发布拓扑明文口径；family 卡/审计行逐处引 `REL-VG-01`；VG-11/VG-13 补 ER-06a 文档 AC；守衛重置改 supervisor ledger 授权（旧 manifest sha256 记账）；R16–R18 叙述回填 | 无 | 全文 |
| 2026-09-19 | Codex R20 `FAIL`（P0 0/P1 1）；Claude R20 `PASS`（P0 0/P1 0） | R20 修订：守衛重置改单次精确 permit（绑定卡 ID + 当前 manifest sha256）+ 原子消费 + supervisor 审计账本 | 无 | 全文 |
| 2026-09-19 | Codex R21 `FAIL`（P0 0/P1 2/P2 1）；Claude R21 `FAIL`（P0 0/P1 1） | R21 修订：manifest 移至 supervisor 持有目录（卡不可写）；评审 prompt 重写为现行结构（消除初稿四卡「独立发布」误导）；Review log 顺序修正 | 无 | 全文 |
| 2026-09-19 | Claude R21 `FAIL`（P0 0/P1 1） | R21 补修：`generated_key_name` 入 VG-08 禁删清单/AC8，G3 测试改名覆盖 | 无 | VG-08 |
| 2026-09-19 | Codex R22 `FAIL`（P0 2/P1 3）；Claude R22 `PASS`（P0 0/P1 0，修正 prompt 后） | R22 修订：守衛角色分離（supervisor capture/reset、卡只 verify）；generated_key_name producer/reader 测试移至新卡 VG-14；EX-VG-01 扩及 11 卡并补判据规范；VG-08 拆出 VG-14（移除 vs 再生成两轴）；移除四步故障注入；VG-06/VG-07 scope 改任何读取/输出前拒绝 | VG-08 → VG-08 + VG-14 | 全文 |
| 2026-09-19 | Claude R23 `FAIL`（P0 1/P1 4）；Codex R23 待回填 | R23 清理：makedirs 移入 capture；VG-04 去 EX；VG-08 删除与 VG-14 重复的重生成门并修正描述/计数（8/8）；VG-14=12/8@EX；EX-VG-01 收敛为 VG-05/07/11/14 | 无 | VG-04、VG-08、VG-14、审计表 |
| 2026-09-19 | Codex R23 `FAIL`（P0 0/P1 6） | R23 收口：VG-08 仅留移除轴+四步故障门；VG-14 接入 DEP-VG-02/Phase 3/M3/ER-06a/网站门；EX 收敛并逐卡对齐；VG-02 sentinel 改失败路径-only；harness 改 mktemp/umask/trap | 无 | 全文 |
| 2026-09-19 | Codex R24 `FAIL`（P0 1/P1 3/P2 1）；Claude R24 待回填 | R24 修订：VG-08 重写（仅移除轴、G1-G12）；VG-14 接入 DEP-VG-02/Phase 3（四卡）/独立序/M3；harness umask+trap 修正；非豁免卡去 @EX；Review log 排序 | 无 | 全文 |
| 2026-09-19 | Claude R25 `FAIL`（P0 0/P1 2）；Codex R25 待回填 | R25 修订：M3 改四个 patch；三 harness 顶部单次 umask + 建齐目录后单一 EXIT trap（去冗余 umask/chmod） | 无 | M3、VG-02/VG-12/VG-09 |
| 2026-09-19 | Codex R26 `FAIL`（P0 1/P1 3/P2 1） | R26 修订：VG-02 sentinel 改既有 captures 阵列；VG-14 入独立发布链/风险/追溯/GAP 表；VG-03 计 8/8 且矩阵改 3 新；矩阵逐行重对；VG-00/VG-09 补 ER-06a N/A；补具名锚点 | 无 | 全文 |
| 2026-09-19 | Claude R27 + Codex R27 `FAIL`（同一项：VG-11 G8/G10/矩阵） | R27 修订：VG-11 Verification 补 `signing_subkey_selected_by_newest_valid_self_signature`；G10 全文统一为 `primary_key_signs_when_no_usable_signing_subkey`；测试矩阵 VG-11 行改「vault 8 门（G1–G10；G1–G3 合 1）+ config_test 1（G11）」 | 无 | Task VG-11、测试矩阵 |
| 2026-09-19 | Codex R28 仅文书 `FAIL`（已修）；Claude R28 `PASS`；R29 Codex `PASS` + Claude `PASS` | **评审收口**：R29 双 PASS（P0/P1/P2 全 0）；R28 文书修正（补 R27 修訂歷史列、Review log 时序）。技术面自 R27 起未再变动 | 无 | 全文 |

## 已决议设计决策

### ADR-VG-01: `gpg` 子程序通道、显式选码与版本闸门
- **Status:** Accepted
- **Decision:** ① 枚举/导出经 `gpg --with-colons --list-secret-keys` 与 `gpg --batch --armor --export-secret-keys <FPR>`；② 解析并强制 `gpg --version >= 2.2`（过旧/不可解析时 HOME 路径拒绝，`--file` 不受限）；③ 选码：候选 >1 必须 `--key`，恰好 1 个可签名候选可省略，0 候选/多命中/模糊 EMAIL 一律 `LBR-CLI-002` 并列出候选；④ 保护状态不由枚举判定（VG-02 解析）；⑤ `gpg.program` 级联 > PATH（Windows `gpg.exe`）；`--file` 绕开子程序；⑥ 只读调用，不改用户 GnuPG home，`GNUPGHOME` 必须绝对路径。
- **Consequences:** 无 gpg 且无 `--file` fail closed；`--list` 不含保护状态（文档写明）。

### ADR-VG-02: 存储模型与 redaction（encrypted=true + predicate-first + reveal 拒绝）
- **Status:** Accepted
- **Decision:** ① `vault.gpg.seckey_enc` 以 `encrypted=true` 写入；② 键空间：`source`/`pubkey`/`seckey_enc`/`fingerprint`/`signing_key_id`/`uid`/`imported_at`/`generated_pubkey`/`generated_key_name`/`history.<FPR>.pubkey`；③ 三条同时成立：`is_vault_internal_key` 显式包含 `vault.gpg.seckey_enc`；`render_get_value` 与 list/JSON **predicate-first**（internal 判定先于 encrypted，任何路径不得先返回值）；`--reveal` 对 seckey_enc **拒绝**（Display-pin），`get`/bare read = `<REDACTED>`，list/JSON 不显示值；④ 不存 passphrase；威胁模型与 `vault.roottoken_enc` 同级；⑤ 写入 fail-closed（私钥/元数据同批，`source` 最后）；⑥ 公钥类键仅受 `vault.*` 写保护，不做值 redaction。
- **Consequences:** 旧二进制忽略新键；密钥仅存于本仓库本地库。

### ADR-VG-03: 签名按 `source`、验证按固定允许列表（source 无关）
- **Status:** Accepted
- **Decision:** ① `pgp_sign` 按 `source` 分派；imported 时以 `signing_key_id` 选 (sub)key，输出 hex 编码与既有路径一致；② `pgp_verify` 与 source 无关：内建生成密钥验证路径 + 依序尝试 `pubkey` → `generated_pubkey` → `history.*`（指纹字典序），任一通过即 true；解析失败记 debug 继续；全部失败 false 不 panic；③ 按 issuer 在受信证书内定位 (sub)key 并按签名创建时刻评估（ADR-VG-09）；④ 开关语义不变；签名侧缺失/失败 fail closed，沿用 `VaultSign` 通道。
- **Consequences:** 移除/替换不破坏历史验证。

### ADR-VG-04: 历史公钥允许列表（统一不变量 + 确定性顺序）
- **Status:** Accepted
- **Decision:** ① 不变量：**任何曾为活动 `vault.gpg.pubkey` 而现已不活动的公钥都写入 `history.<FPR>.pubkey`**（含生成密钥）；同一指纹一行、重复写幂等；② 顺序 = 指纹字典序；③ 历史只增不减，仅用户显式 `config unset` 可删单行；④ `generated_pubkey` 为生成公钥当前副本，用于移除后恢复活动槽。
- **Consequences:** 三种来源转换都不破坏历史；测试三方向。

### ADR-VG-05: 命令行表面、作用域与错误码
- **Status:** Accepted
- **Decision:** ① 三条新子命令（import/export/remove）与 `--gpg-keys` 视图；② 作用域仅 local，`--global`/`--system` 在任何读写前拒绝（共享 helper）；③ 错误码：用法/互斥/歧义/跨作用域 `LBR-CLI-002`；无 gpg 或版本过旧 `LBR-UNSUPPORTED-001`；I/O/解析/写失败 `LBR-IO-001`；冲突（未 `--replace`、无公钥导出、无 `--force` 移除）`LBR-CONFLICT-002`；reveal 内部键 = 既有拒绝语义；④ 新增文案 Display-pin；⑤ `list --gpg-keys` 新字段 additive。
- **Consequences:** 无新稳定码。

### ADR-VG-06: 替换、移除与再生成的原子生命周期
- **Status:** Accepted
- **Decision:** ① 默认拒绝覆盖活动密钥（`LBR-CONFLICT-002`），`--replace` 先写历史行（`source=generated` 且无快照时先写 `generated_pubkey`）；② 同指纹重复导入幂等；③ `remove --force` 原子收敛（写历史行→删导入侧键→恢复/清空活动公钥→`source=generated`）；④ `--force` 后签名回落生成密钥；无生成密钥时签名失败并提示；⑤ 不触碰 `vault.signing`；历史永不自动删除；⑥ **再生成迁移（需先重构 helper，且必须处理 libvault 固定键名限制）：** 现 `generate_pgp_key` 在生成阶段就写 `vault.gpg.pubkey`、吞掉 config 写错误（事实基线第 1 行），并且 libvault 的 `pgp_generate_key` **固定键名且重名直接拒绝**（`path_keys.rs:353-354`：`PgpCertBackend.fetch_cert(...).is_ok() → ErrKeyNameAlreadyExist`），libvault 只提供 revoke 而无 delete key API；因此：
  1. 抽出**纯生成 helper** `vault_generate_pgp_key(key_name, user_name, user_email)`：只调用 libvault 生成并返回公钥，不写任何 config。
  2. 生成键名改为**版本化** `libra-signing-<unix-ns>`（**纳秒**单调时间戳）；**碰撞处理**：若 `fetch_cert` 报已存在（同一纳秒不可能但需防御）则递增/probe 重试，最多 8 次，仍失败即报错；每次重试记录 debug。并持久化 `vault.gpg.generated_key_name`；既有仓库无此键时回退到遗留常量 `libra-signing`（向后兼容）。
  3. `pgp_sign`/`pgp_verify` 的 generated 路径必须改为从 `vault.gpg.generated_key_name` 解析键名（不再硬编码 `PGP_KEY_NAME`）；这也影响 VG-04/VG-05 的调用点。
  4. 失败重跑：因每次尝试用新纳秒键名，残余的旧 vault 键不会阻断重跑（**不可回收但不被引用**，文档记为无害残留）；步骤 (e) 的重跑幂等：若 `generated_key_name` 已存在则跳过生成直接补写 `source`；碰撞/连续失败测试：`generate_pgp_key_retries_on_name_collision`与 `config_generate_gpg_key_after_import_consecutive_failures_rerun`。
  5. staged 序列（a）快照入 history →（b）纯 helper 生成新键名 →（c）写 `generated_pubkey`（可失败传播）→（d1）写 `generated_key_name` →（d2）写新 `vault.gpg.pubkey` →（e）写 `source=generated`（最后）；**（d1/d2 为两步单项写入，必须记录 before-image 并逐项补偿**（见下表）；每步补偿/残留如下表。
  6. 恢复动作 = 重新导入（需原 GnuPG home 或 `--file` 备份），文档写明备份义务。

**逐步补偿/残留表（VG-08 故障注入逐行验证）：**

| 步 | 失败时动作 | 允许残留 | 禁止残留 |
|---|---|---|---|
| (a) 写历史行失败 | 中止，不生成 | 无 | 半行/覆盖活动公钥 |
| (b) 生成失败 | 中止，状态=导入 | 无 config 变更 | 阻断后续 |
| (c) 写 `generated_pubkey` 失败 | 用 before-image 恢复 `generated_pubkey` 的原值/缺失态（不得直接删除已有回退快照）；否则中止 | 历史行（幂等可重写） | 无快照进入 (d) |
| (d1) 写 `generated_key_name` 失败 | 回滚 (c) 的快照；保留版本化 vault 键 | 未引用的 versioned vault key | 新 key name 已写但 pubkey 未变 |
| (d2) 写新 `vault.gpg.pubkey` 失败 | 用 before-image 恢复 `generated_key_name`（d1）、`vault.gpg.pubkey`（若部分生效）**及 (c) 的 `generated_pubkey`**，保留版本化 vault 键 | 未引用的 versioned vault key | 新 key name + 旧 pubkey 的不一致残留；新 `generated_pubkey` 配旧 key name |
| (e) 写 `source` 失败 | **允许的过渡性不变量：** (a) 已把旧活动公钥写入 history，因此即使 (d2) 已生效、`source` 仍为 `imported`，imported 签名仍可经 history 验证；重跑幂等（`generated_key_name` 存在即跳过生成直接补 source） | 新公钥+key_name+history，`source` 旧值（可控过渡态，可由重跑收敛） | 历史/快照被删；无 history 记录的公钥切换 |
| 任一步 config 写错误 | 必须传播（`generate_pgp_key` 返回 `Result`，不得吞错） | — | 静默成功 |
- **Consequences:** 三步转换各有单一恢复动作；`generate_pgp_key` 需返回 Result 并加故障注入测试。

### ADR-VG-07: 测试策略（一次性密钥 + fake gpg + sentinel 捕获 + allowlist 守衛）
- **Status:** Accepted
- **Decision:** ① `pgp 0.19` 测试内生成 fixture（primary-only [SC]、primary [C]+signing subkey、受保护 S2K、binding 损坏、revoked/expired/disabled subkey、issuer 冲突/缺失）；② fake `gpg`（`tests/data/fake-gpg/`）模拟 `--version`/`--with-colons`/`--export-secret-keys`，经 `gpg.program` 指向；不新增 debug env hook；③ **sentinel 守衛** = 以口令哨兵值导入后，运行 `config get/list --json`、`LIBRA_LOG=trace` 的签名/验证命令，分别捕获 stdout/stderr 到 `/tmp/issue-vg/<card>/{out,err}.txt`，按模板三支 `rg` 退出码模板断言零命中，并断言 `<REDACTED>` 出现；④ **变更文件 allowlist 守衛**：每卡核对 `libra status --short` 输出 ⊆ 卡内 Implementation write set + 允许的测试/文档集，越界即失败（`rg` 退出码模板）；⑤ 真实 gpg 路径不入 L1，手工证据归档。
- **Consequences:** 可复现、可机械核对。

### ADR-VG-08: 平台矩阵
- **Status:** Accepted
- **Decision:** Linux/macOS 一等；Windows 支持 `gpg.exe` 与 `--file`，无 TTY 强制 `--passphrase-file`，否则 fail closed。
- **Consequences:** CI Linux 覆盖 fake-gpg/`--file`。

### ADR-VG-09: signing-subkey 资格、选择与验证时刻
- **Status:** Accepted
- **Decision:** ① 资格（全部满足）：Sign 能力；subkey binding 有效且由本证书 primary 签发；未 revoked；在评估时刻未 expired；未被 disabled（key-flag/self-signature）；无 issuer 冲突；有 newest-effective-self-signature 作为属性来源；② 选择：合格 subkey 按 key id 升序取第一；无合格 subkey 时 primary 自身必须合格，否则 fail closed；③ `vault.gpg.signing_key_id` 持久化；id 不在证书内 fail closed；④ 验证按 issuer 定位 primary/subkey；**issuer 缺失时按受信证书内候选 (sub)key 有界尝试，上限 16，超出 fail closed**；⑤ **验证时刻**：以签名 creation time（hashed subpacket）评估 expired/revoked；无 creation time 用当前时间并记 debug；revoked 后签名 Bad、previous 历史 Good；⑥ fixture 与具名用例：五类拒绝 + primary-only + issuer-absent + expiration-at-signature-time。
- **Consequences:** 与 GnuPG 同向；依赖 VG-00 spike 验证 pgp packet API。

### ADR-VG-10: 口令采集、解保护与未受保护可移植证书重建
- **Status:** Accepted
- **Decision:** ① Libra 自采口令（TTY 隐藏提示；非交互 `--passphrase-file`），同一口令供 `gpg --pinentry-mode loopback --passphrase-fd` 与 `pgp` unlock；② packet 级解密 secret 包并**重建**未受保护 transferable 证书（保留 primary/subkey/UID/binding），不得用 `to_armored_*` 直接输出受保护包；③ `--file` 受保护缺口令 → `LBR-CLI-002`；错误口令 → `LBR-IO-001`；两者零写入；④ 口令/明文/中间缓冲 `zeroize`，`Debug` 不含；⑤ 可行性由 VG-00 spike go/no-go 预验证，no-go 时按降级选项改计划。
- **Consequences:** 两条路径一致；导入后签名无需口令。

### ADR-VG-11: 家族发布（G-08）——显式例外授权
- **Status:** Accepted
- **发布拓扑明文口径（必须保留）：** 本计划未收到任何「VG-01..VG-04 必须各自独立 patch 发布」的外部要求；若该要求存在，必须由用户在开工前以明文给出并经双评审登记，届时按 G-08 拆卡重排。本计划按 ADR-VG-11 家族发布执行，并以本条显式记录该口径。
- **例外授权与依据（必须保留）：** 本计划显式援引模板 G-08 家族卡机制，并登记为对「每卡独立发布」默认口径的**已授权例外**：依据 = Codex R2 评审发现「VG-01..VG-03 独立发布会暴露导入未签名/签名未验证的中间坏状态」+ 用户 2026-09-19「双评审收敛」指令；家族发布的目的是满足成功定义中的**无中间坏状态硬约束**（导入/签名/验证读者必须同批上线），而非推迟版本。九张 `family child` 仍各自完成完整 ER-04 门与本地提交，唯一版本 bump/tag/发布动作在 VG-09；该契约与「无中间坏状态」互不冲突，二者互相强化。
- **Decision:** ① 家族子卡 = **VG-01、VG-10、VG-02、VG-11、VG-03、VG-13、VG-12、VG-04、VG-05**（`family child`：各自完整 ER-04、本地提交、不推送/不 bump/R=N/A；`C/D coverage from: VG-09`）；② **VG-09** 为唯一 `family release point`（bump/tag/push/发布/D 证据）；③ `REL-VG-01` 登记窗口与失败逆序回滚；④ VG-06、VG-07、VG-08、VG-14 在 VG-09 后独立发布（四张 patch）。
- **Consequences:** 用户可见面一次性上线。

### ADR-VG-12: 首度导入的 `vault.signing` 启用策略
- **Status:** Accepted
- **Context:** `commit` 仅在 `vault.signing == true` 时签名；`--vault=false` 或从未设置的仓库导入密钥后仍会静默未签名（GAP-VG-10）。
- **Decision:** ① 导入成功时若 `vault.signing` **未设置** → 写入 `true`（与 `init` 默认/`generate-gpg-key` 行为一致）；② 若已显式设置（`true` 或 `false`）→ **保持原值**；`false` 时输出可操作提示（「已导入但签名仍关闭；`libra config set vault.signing true` 启用」）；③ 仅 `import`/`generate` 的 signing 用途触发该策略；加密用途不受影响；④ 两分支均有集成测试（`vault.signing` 未设置仓、`--vault=false` 仓）。
- **Consequences:** 不再出现「导入成功但静默无签名」；显式关闭被尊重。

## 全局工程约束

- 继承 `GC-01..GC-13` 与 `AGENTS.md`；计划级：**GC-VG-01** 零泄漏（口令/私钥/明文不得进 argv、env、输出、日志、trace、JSON、错误；`zeroize`；临时文件 0600+删除；`Debug` redaction）；**GC-VG-02** 禁 `unwrap/expect/panic`（测试除外）；**GC-VG-03** 只读调用 `gpg`（唯一例外：VG-00 spike 可用隔离的 `GNUPGHOME=/tmp/issue-vg/vg00/gnupg`（0700）生成测试密钥并在卡尾删除；不得触碰开发者自身 `GNUPGHOME`）；**GC-VG-04** redaction 单一事实源（ADR-VG-02 三条）；**GC-VG-05** 绑定/状态校验不可跳过；**GC-VG-06** 家族边界不可拆散（禁止绕过 REL-VG-01 发布）；**GC-VG-07** 历史不变量不可违（任何自动路径不得删 `history.*`/`generated_pubkey`）。

## 执行检查必备需求（强制）

- 继承 `ER-01..ER-13`；计划级：**ER-VG-01** 全量门命令；**ER-VG-02** 锚点开工日刷新；**ER-VG-03** VG-02/03 手工证据含 sentinel 口令、真实 gpg、错误口令零写入、失败注入；**ER-VG-04** 网站写前按 DEP-VG-02 硬判据重核，未同步不得 `complete`；**ER-VG-05** 签名卡手工 `commit`/`tag -s`/`tag -v` 往返（含 subkey 证书）；**ER-VG-06** 家族子卡在 VG-09 前不得推送、不得标 `done`；**ER-VG-07** 每卡变更文件 allowlist 守衛（本计划内联脚本，见下）；**ER-VG-08** 每张含网站页的卡必须在其 Verification 列入「网站页同步」门（EN+zh+开发文档+对应 `*.en.md` 同批），未同步不得 `complete`；**ER-VG-09** 本计划守衛脚本一律用下面内联的两段脚本，不得改写为模糊引用。

**Sentinel 捕获脚本（VG-02/VG-12/ER-VG-03 使用；`<card>` 与 `<sentinel>` 由各卡代入）：**

```bash
set -u
DIR=/tmp/issue-vg/<card>; mkdir -p "$DIR"
SENT="<sentinel-passphrase>"        # 导入时用的口令哨兵
# 1) 以哨兵口令导入（或运行目标命令），捕获 stdout/stderr
<import-or-target-command> >"$DIR/out.txt" 2>"$DIR/err.txt"
# 2) 必须出现 redaction 标记（目标键被隐藏）
if ! rg -q '<REDACTED>' "$DIR/out.txt" "$DIR/err.txt"; then echo FAIL: redaction marker missing; exit 1; fi
# 3) 哨兵/私钥零命中（三支退出码；rg 0=命中/1=无命中/>1=错误）
if rg -F -n -e "$SENT" -e 'BEGIN PGP PRIVATE KEY' -e 'BEGIN PGP SIGNATURE' "$DIR/out.txt" "$DIR/err.txt"; then
  echo FAIL: secret material found; exit 1
else
  rc=$?
  if [ "$rc" -ne 1 ]; then echo "ERROR: rg failed with exit $rc"; exit "$rc"; fi
  echo OK: zero secret hits
fi
```

**Allowlist 守衛脚本（每卡适用；capture/verify 两段式 + byte-safe Python + root/backend VCS 选择器）：**

> 用法（实体化）：卡执行者把本 fenced block 逐字写入 `$DIR/guard.sh`（`chmod +x`），并以卡内 Implementation write set 逐行初始化 allowlist：
> ```bash
> DIR=/tmp/issue-vg/<card>; mkdir -p "$DIR"
> printf '%s\n' src/internal/vault.rs src/command/config.rs > "$DIR/allowlist.txt"   # 以本卡写集替换；网站页以 ../libra-backend/... 前缀登记
> cat > "$DIR/guard.sh"   # 将下方脚本块（set -eu 起）逐字粘贴并保存
> ```
> **角色分离：** `capture`（写 supervisor manifest）与 `reset`（消费 permit、写 ledger）**只能由 supervisor 运行**；卡执行者只运行 `bash "$DIR/guard.sh" verify`（只读 supervisor manifest/ledger 并比对），因此卡执行者从不写入 supervisor 目录。supervisor 在卡开工前执行 capture 并把结果登记入卡证据。**卡内禁止删除/重写 manifest 重捕獲**（manifest/ledger 位于 supervisor 目录，卡执行者无写权限）；唯一重置路径 = supervisor 在卡目录之外创建**单次 permit** `/tmp/issue-vg/guard-permits/<card>.permit`（内容恰为 `card\tmanifest_sha256\tpermit_id`，绑定当前 manifest SHA）；`capture` 精确解析并校验后**原子消费**（`os.replace(permit, permit.consumed.<pid>)`），再把「卡 ID + 旧 manifest sha256 + permit_id + reset」追加到 supervisor 持有的审计账本 `/tmp/issue-vg/guard-reset-ledger.tsv`。permit 目录与账本由 supervisor（评审/操作者）创建并保管，卡执行者按其运行账户不应具备写权限（由评审流程保证）。`$DIR/allowlist.txt` 为守卫私有状态（repo 之外的 `/tmp`），不属于任何 Implementation write set。

```bash
set -eu
DIR=/tmp/issue-vg/<card>; mkdir -p "$DIR"
ALLOW="$DIR/allowlist.txt"; sort -u "$ALLOW" -o "$ALLOW"
MODE="${1:-verify}"
python3 - "$DIR" "$ALLOW" "$MODE" <<'PYEOF'
import hashlib, json, os, subprocess, sys
DIR, ALLOW_FILE, MODE = sys.argv[1], sys.argv[2], sys.argv[3]
ROOT = "/mnt/gitmono/libra"
BACKEND = os.path.abspath(os.path.join(ROOT, "..", "libra-backend"))
LIBRA = os.path.join(ROOT, "target/debug/libra")
MANIFEST = os.path.join("/tmp/issue-vg/guard-manifests", os.path.basename(DIR) + ".json")  # supervisor-owned; card dir is NOT used

def read_allow():
    allow = []
    with open(ALLOW_FILE, "r", encoding="utf-8", errors="surrogateescape") as fh:
        for line in fh:
            p = line.rstrip("\n")
            if p and not p.startswith("#"):
                allow.append(p)
    return allow

def vcs_tool(base):
    git, libra = os.path.exists(os.path.join(base, ".git")), os.path.exists(os.path.join(base, ".libra"))
    if git and not libra:
        return "git"
    if libra and not git:
        return "libra"
    raise SystemExit(f"ERROR: ambiguous or missing VCS metadata in {base} (.git/.libra)")

def porcelain_entries(base, tool):
    """Yield (path, xy) byte-safely; rename/copy both endpoints share the record's XY."""
    argv = ["git", "status", "--porcelain=v1", "-z"] if tool == "git" else [LIBRA, "status", "--porcelain=v1", "-z"]
    out = subprocess.run(argv, cwd=base, capture_output=True, check=True).stdout
    parts = out.split(b"\0"); i = 0
    while i < len(parts):
        rec = parts[i]
        if not rec:
            i += 1; continue
        xy = rec[:2].decode("ascii", "replace")
        yield rec[3:].decode("utf-8", "surrogateescape"), xy
        if xy and (xy[0] in "RC" or xy[1] in "RC"):
            i += 1
            if i < len(parts) and parts[i]:
                yield parts[i].decode("utf-8", "surrogateescape"), xy
        i += 1

def mode_of(base, rel):
    try:
        return oct(os.lstat(os.path.join(base, rel)).st_mode & 0o7777)
    except OSError:
        return "MISSING"

def digest(base, rel):
    p = os.path.join(base, rel)
    if os.path.islink(p):
        return "LINK:" + hashlib.sha256(os.readlink(p).encode("utf-8", "surrogateescape")).hexdigest()
    if os.path.isdir(p):
        h = hashlib.sha256()
        for root_dir, dirs, files in os.walk(p, followlinks=False):
            dirs.sort()  # 确定子目录遍历顺序，保证 digest 稳定
            for name in sorted(dirs) + sorted(files):
                fp = os.path.join(root_dir, name)
                r = os.path.relpath(fp, p).encode("utf-8", "surrogateescape")
                if os.path.islink(fp):
                    h.update(b"L|" + r + b"|" + os.readlink(fp).encode("utf-8", "surrogateescape"))
                elif os.path.isdir(fp):
                    h.update(b"D|" + r)
                else:
                    h.update(b"F|" + r + b"|")
                    with open(fp, "rb") as fh:
                        h.update(fh.read())
        return "DIR:" + h.hexdigest()
    if os.path.isfile(p):
        with open(p, "rb") as fh:
            return "FILE:" + hashlib.sha256(fh.read()).hexdigest()
    return "MISSING"

def collect(include_backend, backend_required):
    tool = vcs_tool(ROOT)
    root_entries = list(porcelain_entries(ROOT, tool))
    root_paths = sorted({p for p, _ in root_entries})
    data = {"allow": read_allow(), "root": {p: [digest(ROOT, p), dict(root_entries).get(p, ""), mode_of(ROOT, p)] for p in root_paths},
            "root_paths": root_paths, "root_tool": tool, "backend_required": backend_required, "backend": {},
            "backend_paths": [], "backend_tool": None}
    tool_be = None
    if include_backend or backend_required:
        if os.path.isdir(BACKEND):
            tool_be = vcs_tool(BACKEND)
        else:
            raise SystemExit(f"ERROR: backend required but missing: {BACKEND}")
    if include_backend and tool_be:
        be_entries = list(porcelain_entries(BACKEND, tool_be))
        be_paths = sorted({p for p, _ in be_entries})
        data["backend_paths"] = be_paths
        data["backend_tool"] = tool_be
        data["backend"] = {p: [digest(BACKEND, p), dict(be_entries).get(p, ""), mode_of(BACKEND, p)] for p in be_paths}
    return data

def allowed(rel, allow):
    return rel in allow or any(a.endswith("/") and rel.startswith(a) for a in allow)

def fail(msg):
    print("FAIL: " + msg); raise SystemExit(1)

allow = read_allow()
backend_required = any(a.startswith("../libra-backend/") for a in allow)

if MODE == "capture":
    os.makedirs(os.path.dirname(MANIFEST), exist_ok=True)  # supervisor 在开工前预建；verify 模式不得创建目录
    if os.path.exists(MANIFEST):
        # 单次、精确、绑定卡 ID 与当前 manifest SHA 的 supervisor permit
        permits_dir = "/tmp/issue-vg/guard-permits"           # supervisor 持有（卡执行者不应有写权限）
        ledger = "/tmp/issue-vg/guard-reset-ledger.tsv"       # supervisor 持有的审计账本
        permit = os.path.join(permits_dir, os.path.basename(DIR) + ".permit")
        old_sha = hashlib.sha256(open(MANIFEST, "rb").read()).hexdigest()
        try:
            parts = open(permit, encoding="utf-8").read().rstrip("\n").split("\t")
        except FileNotFoundError:
            raise SystemExit("ERROR: baseline manifest exists; reset requires a supervisor permit file (guard-permits/<card>.permit)")
        if len(parts) != 3 or parts[0] != os.path.basename(DIR) or parts[1] != old_sha or not parts[2]:
            raise SystemExit("ERROR: permit is malformed or does not match this card/manifest SHA; reset refused")
        # 原子消费：先改名再删 manifest，同一 permit 不可能复用
        os.replace(permit, permit + ".consumed." + str(os.getpid()))
        with open(ledger, "a", encoding="utf-8") as fh:
            fh.write(f"{os.path.basename(DIR)}\t{old_sha}\t{parts[2]}\treset\n")
        os.remove(MANIFEST)
    data = collect(include_backend=backend_required, backend_required=backend_required)
    with open(MANIFEST, "x", encoding="utf-8") as fh:
        # ensure_ascii=True: surrogateescape 路径经 \udcXX 转义可无损往返 UTF-8 JSON
        json.dump(data, fh, ensure_ascii=True, sort_keys=True)
    print("OK: baseline captured -> " + MANIFEST)
elif MODE == "verify":
    base = json.load(open(MANIFEST, "r", encoding="utf-8"))
    now = collect(include_backend=bool(base["backend_tool"]) or base["backend_required"], backend_required=base["backend_required"])
    if now["root_tool"] != base["root_tool"]:
        fail("root VCS metadata changed between capture and verify")
    if now["backend_tool"] != base["backend_tool"]:
        fail("backend VCS metadata changed between capture and verify")
    for p in sorted(set(now["root_paths"]) - set(base["root_paths"])):
        if not allowed(p, base["allow"]):
            fail("unexpected changed file: " + p)
    for p, rec in base["root"].items():
        if now["root"].get(p) != rec:
            fail("baseline-dirty content/state changed (root): " + p)
    if base["backend_tool"]:
        for p in sorted(set(now["backend_paths"]) - set(base["backend_paths"])):
            if not allowed("../libra-backend/" + p, base["allow"]):
                fail("unexpected changed backend file: " + p)
        for p, rec in base["backend"].items():
            if now["backend"].get(p) != rec:
                fail("baseline-dirty content/state changed (backend): " + p)
    print("OK: changed files within per-card allowlist (all baseline-dirty content stable)")
else:
    raise SystemExit("usage: guard.sh [capture|verify]")
PYEOF
```


## 实施顺序

- `DEP-VG-01 -> 全部卡`；`DEP-VG-02 -> VG-01、VG-10、VG-02、VG-11、VG-03、VG-13、VG-12、VG-04、VG-05、VG-06、VG-07、VG-08、VG-14（含网站页写集的卡）`（含网站页写集的卡；VG-00 无网站写集故不入边）；`DEP-VG-03 -> VG-04、VG-05`；`DEP-VG-04 -> VG-01、VG-10、VG-02`
- `VG-00 -> VG-01`（spike go/no-go）
- `VG-01 -> VG-10`（发现先于选码）
- `VG-10 -> VG-02`（选码先于解保护）
- `VG-02 -> VG-11`（重建先于资格判定）
- `VG-11 -> VG-03`（资格先于持久化）
- `VG-03 -> VG-13 -> VG-12`（持久化→替换/历史→redaction 读取面；`vault.rs`/`config.rs` 串行）
- `VG-12 -> VG-04 -> VG-05`（存储/redaction 先于签名，签名先于验证）
- `VG-05 -> VG-09`（家族发布点聚合全部子卡）
- `VG-09 -> VG-06 -> VG-07 -> VG-08 -> VG-14`（管理面依次独立发布）

### 依赖登记表

| ID | direction | 类型 | 对象 | Owner | 产物与可用性判据 | 证据 | 超时与失败策略 |
|---|---|---|---|---|---|---|---|
| DEP-VG-01 | incoming | 跨计划 I–I | plan-20260919 GCX-01/02/03 | 该计划执行者 | 三卡 `done`/`complete`；解除 `vault.rs`/`internal/config.rs`/`command/config.rs`/`config_test.rs`/config 文档/`COMPATIBILITY.md`/网站 config 页相交 | 开工日复核 `Lifecycle` 行 | 未满足全卡 `blocked` |
| DEP-VG-02 | incoming | 外部仓库 | `../libra-backend` 网站文档 | docs owner | **硬判据（统一 VCS 选择 helper，与网站门/守衛同一实现）：** `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'`（`.git` 兼容文件态与目录态）；二者皆有或皆无 = helper `exit 2` 硬阻塞；**两路径分支都必须为 `cf`**；**每张待写网站页必须干净（`status --short` 不得列出该文件）或已有 owner 书面批准的处置（提交/回滚/串行窗口）；当前 `cf` 检出已脏的 `config.en.md`/`push.en.md` 在写完前必须先处置，否则含该页的卡硬 `blocked`**；与 plan-20260918/plan-20260919 不并发写同页；网站目标文件路径 = `../libra-backend/apps/tanstack-app/content/docs/commands/<cmd>.en.md`（具体文件在各卡写集列出） | 开工日完整命令输出（类型/分支/脏文件）入卡内证据 | 二义/缺失/分支非 `cf`/二进制不匹配/并发改页 → 含网站页的卡整卡 `blocked` |
| DEP-VG-03 | incoming | 跨计划写集互斥 | issues/490 SW-03、issues/473 IN-07、issues/470 FM-02 | 各计划执行者 | 相交卡不 `in-progress`；commit.rs/merge.rs 及其文档、`COMPATIBILITY.md` 行未在飞行中 | 开工日复核三计划状态行 | 未满足时 VG-04/VG-05 `blocked` |
| DEP-VG-04 | incoming | 环境前置 | `gpg >= 2.2` 或 `--file` | 执行者 | 解析 `gpg --version` 首行并强制 ≥2.2；过旧/不可解析 → HOME/loopback 拒绝，仅 `--file` | `gpg --version` 输出入卡内证据 | 不满足时 VG-01/VG-10/VG-02 的 HOME 路径 `locally-accepted` |

### 发布分组与并发窗口

- **发布者（ER-12 具名）:** 执行本计划的主 Agent（唯一发布者），负责 bump、提交、tag、推送、`release.yml` 监控、CDN 证据与逆序回滚执行。
- **REL-VG-01（家族发布）:** 成员 = 家族子卡（ADR-VG-11 第 1 条）；唯一发布点 = VG-09；窗口期（VG-01 本地提交起至 VG-09 完成）禁止其它计划修改 `src/internal/vault.rs`、`src/internal/config.rs`、`src/command/config.rs`、`Cargo.toml`、`Cargo.lock`、`install.sh`、`install.ps1`、`COMPATIBILITY.md`、config/commit/tag/merge/push 文档与网站对应页；失败回滚顺序 = VG-05 → VG-04 → VG-12 → VG-13 → VG-03 → VG-11 → VG-02 → VG-10 → VG-01 本地提交（逆序）。
- **独立发布:** VG-06 → VG-07 → VG-08 → VG-14（各 `patch + 1`）。

## Phase 0: 基线冻结、spike 与消歧
**退出条件:** ~~双 `PASS`~~ ✅ 已达成（R29）；剩余：DEP 复核；`plan-long.md` 索引；`gpg --version` 证据；VG-00 go 结论；ADR-VG-01..12 Accepted。

## Phase 1: 家族实现（VG-01、VG-10、VG-02、VG-11、VG-03、VG-13、VG-12、VG-04、VG-05；不推送/不 bump）
**退出条件:** 各卡完成实现与 focused/触发门、本地提交、Acceptance=`locally-accepted`、Lifecycle 保持 `in-progress`（**不得 `done`**）；README 无远端变更。

## Phase 2: 家族发布（VG-09）
**退出条件:** 聚合 T-2 门绿；版本面 bump；annotated tag 预建并推送；`gh release --verify-tag` + run/CDN/安装冒烟证据；家族子卡转 `done`/`complete`。

## Phase 3: 管理面（VG-06、VG-07、VG-08、VG-14）
**退出条件:** 四卡各自 `done`/`complete` 并独立发布。

## 任务卡

### 字段全局默认与例外
- **Release boundary 默认:** `independent`。
- **Task type 默认:** `implementation`。
- **Rollback mode 默认:** `immutable-release`（已发布只能前滚；本地未推送可 revert；数据面恢复见卡）。
- **Migration and rollback 默认:** N/A（无 schema 迁移）。
- **Security and privacy 默认:** 继承 GC-07/11 与 GC-VG-01..07。
- **Performance budget 默认:** 继承 GC-10。
- **C/D coverage from 默认:** `self`。
- **D 组默认:** `.github/workflows/release.yml`；证据 = ① tag deref 提交；② `gh run view <id> -R libra-tools/libra --json status,conclusion,jobs,event,headBranch,headSha`（全 job success、job 集合 = 四平台 build-and-upload + upload-install-scripts + update-homebrew-tap + verify-homebrew-formula + request-stable-manifest、`event=push`、`headBranch` 为发布分支、`headSha`=deref 提交）；③ CDN gate（manifest 签名 + 四产物 URL/size/sha256 + windows exe 字节一致 + installer 默认版本）。
- **Lifecycle / Acceptance 默认:** `pending` / 空。

**默认覆盖:**

| 任务 | 偏离字段 | 取值与理由 |
|---|---|---|
| VG-00 | `Task type`/`Rollback mode`/`Release boundary`/`C/D coverage`/`Estimated scope` | `spike`/`revert`/`no-release`/`C/D coverage from: VG-09`（结论随家族发布归档）/`M`（≤2 人日，只读 + 结论） |
| 家族子卡（VG-01/10/02/11/03/13/12/04/05） | `Release boundary`/`Release write set`/`C/D coverage from` | `family child（REL-VG-01）`/`N/A`/`VG-09` |
| VG-09 | `Task type`/`Release boundary` | `release`/`family release point（REL-VG-01）` |
| VG-01、VG-10、VG-02、VG-03、VG-06、VG-07、VG-08、VG-12 | `Estimated scope` | `M`：新增/变更公开命令或输出面（G-04 禁止 S 携带公开接口变更） |
| VG-02、VG-03 | `Security and privacy` | 展开：口令采集/零化、packet 重建、redaction、失败注入 |

**规则 waiver（EX-VG-01，G-03 门族型）：** 准入门槛四项均满足：① 每门为可复制执行的具名命令；② 门族清单在卡内「判据规范（非计数正文）」块逐条列举；③ 分子如实写作 `n/8@EX-VG-01`；④ 门族增减时同批更新本表与粒度审计表。

| 例外 ID | 任务（或作用域） | 豁免项 | 理由与补偿措施 | Approver | Review round | 证据 | 有效期 |
|---|---|---|---|---|---|---|---|
| EX-VG-01 | VG-05、VG-07、VG-08、VG-11、VG-14 | G-03 条目上限（门族型） | 同一恢复轴上的机械门族（发现/选项、依赖构建、持久化顺序、资格拒绝、验证时刻、导出 flag、故障注入、列表字段、redaction、历史不变量）；逐门计数必然超 8，按门拆卡会违反 G-01/G-02；补偿 = 每门为具名可复制命令 + 卡内判据规范块 | 计划评审（Codex + Claude R4，用户 2026-09-19 双评审授权） | R4 | 各卡「判据规范」块；`/tmp/issue-gpg/review/codex-plan-r4.log` | 本计划内 |

### 任务卡粒度审计表

> `landing / prod-files` = **行为落点数 / 生产文件数**（G-04 计数口径：非测试、非随附文档、非版本面）；`AC`/`VER` 按独立谓词/独立验证门计数，超限的门族以 `n/8@EX-ID` 登记。

| 任务 | type | axis | recovery | complete | self-contained | AC | VER | landing / prod-files | scope | deps | writeset | release | split-from | exception |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| VG-00 | spike | pgp/gpg API 可行性 | revert | yes | yes | 6/8 | 4/8 | 0/0 | M | DEP-VG-01 | 只读 | no-release | N/A | N/A |
| VG-01 | implementation | gpg 发现与版本闸门 | immutable-release | yes | yes | 6/8 | 5/8 | 2/2 | M | VG-00, DEP-VG-01/02/04 | 序列化于 VG-00 | family child（REL-VG-01） | N/A（保留；拆出 VG-10） | N/A |
| VG-10 | implementation | 选码与作用域 | immutable-release | yes | yes | 7/8 | 5/8 | 2/2 | M | VG-01, DEP-VG-01/02/04 | 序列化于 VG-01 | family child（REL-VG-01） | VG-01 | N/A |
| VG-02 | implementation | 解保护与证书重建 | immutable-release | yes | yes | 8/8 | 6/8 | 2/2 | M | VG-10, DEP-VG-01/02/04 | 序列化于 VG-10 | family child（REL-VG-01） | N/A（保留；拆出 VG-11） | N/A |
| VG-11 | implementation | 签名资格与选择 | immutable-release | yes | yes | 11/8@EX-VG-01 | 5/8 | 1/1 | S | VG-02, DEP-VG-01/02 | 序列化于 VG-02 | family child（REL-VG-01） | VG-02 | EX-VG-01 |
| VG-03 | implementation | 持久化与首度签名策略 | immutable-release | yes | yes | 8/8 | 5/8 | 2/2 | M | VG-11, DEP-VG-01/02 | 序列化于 VG-11 | family child（REL-VG-01） | N/A（保留；拆出 VG-12、VG-13） | N/A |
| VG-13 | implementation | 替换与历史不变量 | immutable-release | yes | yes | 6/8 | 4/8 | 2/2 | S | VG-03, DEP-VG-01/02 | 序列化于 VG-03 | family child（REL-VG-01） | VG-03 | N/A |
| VG-12 | implementation | redaction 读取面 | immutable-release | yes | yes | 7/8 | 6/8 | 2/2 | M | VG-13, DEP-VG-01/02 | 序列化于 VG-13 | family child（REL-VG-01） | VG-03 | N/A |
| VG-04 | implementation | 签名派发 | immutable-release | yes | yes | 8/8 | 7/8 | 2/2 | S | VG-12, DEP-VG-01/02/03 | 序列化于 VG-12 | family child（REL-VG-01） | N/A | N/A |
| VG-05 | implementation | 验证派发与允许列表 | immutable-release | yes | yes | 15/8@EX-VG-01 | 6/8 | 1/1 | S | VG-04, DEP-VG-01/02/03 | 序列化于 VG-04 | family child（REL-VG-01） | N/A | EX-VG-01 |
| VG-09 | release | 家族发布点 | immutable-release | yes | yes | 8/12 | 6/12 | 0/0 | S | 家族子卡全部 | 唯一发布点 | family release point（REL-VG-01） | N/A | N/A |
| VG-06 | implementation | 列表信息面 | immutable-release | yes | yes | 6/8 | 5/8 | 1/1 | M | VG-09, DEP-VG-01/02 | 序列化于 VG-09 | independent | N/A | N/A |
| VG-07 | implementation | 公钥导出 | immutable-release | yes | yes | 11/8@EX-VG-01 | 5/8 | 1/1 | M | VG-06, DEP-VG-01/02 | 序列化于 VG-06 | independent | N/A | EX-VG-01 |
| VG-08 | implementation | 移除与生成回落 | immutable-release | yes | yes | 12/8@EX-VG-01 | 6/8 | 2/2 | M | VG-07, DEP-VG-01/02 | 序列化于 VG-07 | independent | N/A（保留移除轴；拆出 VG-14） | EX-VG-01 |
| VG-14 | implementation | 再生成 staged 迁移 | immutable-release | yes | yes | 12/8@EX-VG-01 | 3/8 | 2/2 | M | VG-08, DEP-VG-01/02 | 序列化于 VG-08 | independent | VG-08 | EX-VG-01 |

（`VG-09` 的 `AC/VER` 分母按 `release` 卡 12 计。）

### Task VG-00: pgp/gpg 导入 API 可行性 spike（go/no-go）
**Task type:** `spike` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 只读验证 ADR-VG-09/10 依赖的 API：解析真实 gpg 导出证书（RSA/Ed25519，含 subkey）、packet 级解保护、重建未受保护可移植证书、subkey 签名与 issuer 验证。唯一行为轴 = 可行性结论。

**Out of scope:** 生产代码改动；真实用户密钥；网络。

**Current evidence:** 事实基线第 10、13 行。

**Acceptance criteria:**
- [ ] Q0：在隔离 `GNUPGHOME=/tmp/issue-vg/vg00/gnupg`（0700）内生成测试密钥，卡尾删除；不得修改开发者自身 GnuPG home。
- [ ] 文档同批判定（ER-06a，计划级门不计入 G-03）：`N/A` —— 本卡为只读 spike，无用户可见命令面；结论仅回填本文 ADR-VG-09/10。
- [ ] Q1：解析 `gpg --export-secret-keys` 的 RSA 与 Ed25519 证书（含 subkey）成功。
- [ ] Q2：用口令在 packet 级解密 S2K 受保护 secret 包成功。
- [ ] Q3：重建并序列化未受保护可移植证书，重新解析指纹不变且可再 unlock/签名。
- [ ] Q4：用选定 signing subkey 签名，且仅凭 issuer 在证书内定位该 subkey 完成验证。
- [ ] Q5：go/no-go 结论与证据落盘；no-go 时列出降级选项（如仅 `--file` 未受保护导入）并回改计划。

**Verification:**
- [ ] 临时验证程序/测试（不入库）执行 Q1..Q4，日志 `/tmp/issue-vg/vg00/`
- [ ] 真实 `gpg 2.4+` 生成并导出 RSA/Ed25519（含 subkey）证书，记录 `gpg --version`
- [ ] 只读 allowlist 守衛：变更文件 allowlist = 仅本计划文档；`libra status --short` 输出越界即失败（`rg` 退出码模板）
- [ ] ADR-VG-09/10 结论回填（Status/证据）

**Full-suite trigger:** 不触发（不改生产代码；若临时测试入库则 T-4 并在卡内登记回退）
**Dependencies:** `DEP-VG-01` **Deliverables:** 结论文档 + `/tmp/issue-vg/vg00/` 证据 + 降级清单
**Implementation write set:** `docs/development/plan/plan-20260919-gpg-import.md` **Release write set:** `N/A`
**Files likely touched:** 同写集
**Docs and compatibility impact:** 仅计划内 ADR；ER-06a `N/A`（无用户可见命令面）。**Rollback mode:** `revert` **Migration and rollback:** 无数据/接口变更。
**Security and privacy:** 一次性测试密钥；口令零化；证据脱敏。 **Performance budget:** 无。
**Estimated scope:** `M` **Version increment:** `N/A` **Release boundary:** `no-release` **C/D coverage from:** `VG-09`（结论随家族发布归档）
**Granularity:** `type=spike; axis=pgp/gpg API 可行性; recovery=revert; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=0; prod-files=0; scope=M; deps=DEP-VG-01; writeset=只读; release=no-release; split-from=N/A; exception=N/A`

### Task VG-01: gpg 发现与版本闸门（保留主轴：发现/版本；拆出 VG-10 选码与作用域）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 实现 `import-gpg-key --list` 的发现面：`gpg.program`/`GNUPGHOME` 解析、`gpg --version >= 2.2` 闸门、colon 输出解析与候选五字段展示。本卡保留「发现/版本」主轴；拆出 VG-10（选码与作用域）。

**Out of scope:** 选码协议与作用域拒绝（VG-10 承接）；解保护（VG-02）；持久化（VG-03）。

**Current evidence:** 事实基线第 9、11、13 行；ADR-VG-01。

**判据规范（非计数正文；逐门可复制命令）；每门一条可复制命令）：**
- G1 `--list` 五字段：`source .env.test && cargo test --lib internal::vault -- gpg_colon_list_parses_fingerprint_uid_and_capabilities`
- G2 零写入：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_list_is_read_only`
- G3 gpg 缺失：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_missing_gpg_maps_to_unsupported`
- G4 版本闸门：`source .env.test && cargo test --lib internal::vault -- gpg_version_gate_rejects_old_or_unparsable_version`
- G5 `gpg.program` 级联：`source .env.test && cargo test --lib internal::vault -- gpg_program_precedence_and_gnupghome_validation`
- G6 GNUPGHOME 绝对路径：同 G5 命令（同测试覆盖两断言）
**Acceptance criteria:**
- [ ] `--list` 输出 fingerprint/key id/主 UID/算法位数/能力字母。
- [ ] `--list` 零写入（配置库字节不变）。
- [ ] `gpg` 缺失 → `LBR-UNSUPPORTED-001` + `gpg.program`/`--file` 提示。
- [ ] `gpg --version < 2.2` 或不可解析 → HOME 路径 `LBR-UNSUPPORTED-001`。
- [ ] `gpg.program` 级联优先级生效（local→global→system）。
- [ ] 非绝对路径 `GNUPGHOME` → `LBR-CLI-002`（写入前拒绝）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --lib internal::vault`（new：`gpg_colon_list_parses_fingerprint_uid_and_capabilities`、`gpg_version_gate_rejects_old_or_unparsable_version`、`gpg_program_precedence_and_gnupghome_validation`）
- [ ] `source .env.test && cargo test --test command_test config_test`（new：`config_import_gpg_key_list_is_read_only`、`config_import_gpg_key_missing_gpg_maps_to_unsupported`）
- [ ] Display-pin（new）：`import_missing_gpg_message_is_pinned`、`import_old_gpg_message_is_pinned`
- [ ] 手工证据：真实 `gpg --version` 与 `--list` 归档 `/tmp/issue-vg/vg01/`
- [ ] 零写入守衛：前后 `config list --name-only` diff 为空（`rg` 退出码模板）

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 新建共享 gpg 发现/版本路径与 config 命令面`
**Dependencies:** `VG-00`、`DEP-VG-01`、`DEP-VG-02`、`DEP-VG-04` **Deliverables:** N/A
**Implementation write set:** `src/internal/vault.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`tests/data/fake-gpg/`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/internal/vault.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`tests/data/fake-gpg/`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** `N/A` **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页 EN+zh 的 `--list`/版本闸门/`GNUPGHOME`。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 本地可 revert；发布后前滚；`--list` 零写入。 **Security and privacy:** 只读 gpg；错误信息不含密钥材料。
**Performance budget:** `< 500ms`。 **Estimated scope:** `M` **Version increment:** `N/A` **Release boundary:** `family child（REL-VG-01）` **C/D coverage from:** `VG-09`
**Granularity:** `type=implementation; axis=gpg 发现与版本闸门; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=5/8; landing=2; prod-files=2; scope=M; deps=VG-00,DEP-VG-01,DEP-VG-02,DEP-VG-04; writeset=序列化于 VG-00; release=family child（REL-VG-01）; split-from=N/A（保留；拆出 VG-10）; exception=N/A`

### Task VG-10: 选码与作用域（自 VG-01 拆出）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 实现 `--key` 选码协议（显式/单候选/歧义拒绝）、`--file` 入口与 local-scope 共享 helper；自 VG-01 拆出。

**Out of scope:** 发现/版本（VG-01）；解保护（VG-02）；持久化（VG-03）。

**Current evidence:** 事实基线第 9 行；ADR-VG-01/05；`reject_global_key_generation`（`:3135`）。

**判据规范（非计数正文；逐门可复制命令）；每门一条可复制命令）：**
- G1 多候选必填 `--key`：`source .env.test && cargo test --lib internal::vault -- gpg_selector_requires_explicit_key_when_multiple_candidates`
- G2 单候选自动：`source .env.test && cargo test --lib internal::vault -- gpg_selector_auto_selects_single_signing_candidate`
- G3 0 候选拒绝：`source .env.test && cargo test --lib internal::vault -- gpg_selector_rejects_no_candidate_and_ambiguous_email`
- G4 歧义/EMAIL 多命中：同 G3 命令（同测试两断言）
- G5 `--file` 入口：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_file_mode_bypasses_missing_gpg`
- G6 scope 拒绝：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_rejects_global_and_system_scope`
- G7 非仓库目录：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_outside_repository_reports_not_a_repo`
**Acceptance criteria:**
- [ ] 候选 >1 且未给 `--key` → `LBR-CLI-002` 并列出候选指纹。
- [ ] 恰好 1 个可签名候选时省略 `--key` 自动选中。
- [ ] 0 个可签名候选 → `LBR-CLI-002`。
- [ ] EMAIL 多命中或模糊 → `LBR-CLI-002`。
- [ ] `--file <PATH>` 被接受并进入解保护链路（不静默忽略）。
- [ ] `--global`/`--system` 在任何读写前拒绝（共享 helper，后续卡复用）。
- [ ] 非仓库目录 → not-a-repo 错误（沿用 `generate-gpg-key` 措辞/码）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --lib internal::vault`（new：`gpg_selector_requires_explicit_key_when_multiple_candidates`、`gpg_selector_auto_selects_single_signing_candidate`、`gpg_selector_rejects_no_candidate_and_ambiguous_email`）
- [ ] `source .env.test && cargo test --test command_test config_test`（new：`config_import_gpg_key_rejects_ambiguous_or_unknown_selector`、`config_import_gpg_key_file_mode_bypasses_missing_gpg`、`config_import_gpg_key_rejects_global_and_system_scope`、`config_import_gpg_key_outside_repository_reports_not_a_repo`）
- [ ] 零副作用：scope 拒绝后配置库/文件系统字节不变（`rg` 退出码模板）
- [ ] Display-pin（new）：`import_scope_rejection_message_is_pinned`
- [ ] 手工证据：`--file` 路径导入候选被正确解析，存 `/tmp/issue-vg/vg10/`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 新建共享选码/作用域 helper 与 config 命令面`
**Dependencies:** `VG-01`、`DEP-VG-02`、`DEP-VG-01`、`DEP-VG-04` **Deliverables:** N/A
**Implementation write set:** `src/internal/vault.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`tests/data/fake-gpg/`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/internal/vault.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`tests/data/fake-gpg/`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** `N/A` **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页 EN+zh 选码/作用域/`--file`。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 本地可 revert；发布后前滚；拒绝路径零写入。 **Security and privacy:** 选码错误信息不含有效密钥材料。
**Performance budget:** `< 200ms`。 **Estimated scope:** `M` **Version increment:** `N/A` **Release boundary:** `family child（REL-VG-01）` **C/D coverage from:** `VG-09`
**Granularity:** `type=implementation; axis=选码与作用域; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=5/8; landing=2; prod-files=2; scope=M; deps=DEP-VG-01,VG-01,DEP-VG-02,DEP-VG-04; writeset=序列化于 VG-01; release=family child（REL-VG-01）; split-from=VG-01; exception=N/A`

### Task VG-02: 解保护与未受保护可移植证书重建（保留主轴：解保护/重建；拆出 VG-11 签名资格）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 按 ADR-VG-10 实现口令采集（TTY/`--passphrase-file`）、packet 级解保护与未受保护证书重建。本卡保留「解保护/重建」主轴；拆出 VG-11（签名资格与选择）。

**Out of scope:** 资格判定/选择（VG-11）；持久化（VG-03）。

**Current evidence:** 事实基线第 10、13 行；ADR-VG-10。

**判据规范（非计数正文；逐门可复制命令）；每门一条可复制命令）：**
- G1 pgp 依赖/build：`cargo build`（默认 profile）成功
- G2 TTY 口令采集：`source .env.test && cargo test --lib internal::vault -- tty_prompt_collects_passphrase`
- G3 非交互缺口令：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_protected_file_requires_passphrase`
- G4 `--file` 受保护：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_protected_file_requires_passphrase`
- G5 错误口令零写入：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_wrong_passphrase_writes_nothing`
- G6 重建指纹稳定：`source .env.test && cargo test --lib internal::vault -- rebuilt_certificate_is_unprotected_and_fingerprint_stable`
- G7 重建可再签名：`source .env.test && cargo test --lib internal::vault -- rebuilt_certificate_can_sign_roundtrip`
- G8 zeroize：`source .env.test && cargo test --lib internal::vault -- passphrase_and_plaintext_buffers_are_zeroized`
**Acceptance criteria:**
- [ ] `pgp = "0.19.0"` 移入 `[dependencies]` 并刷新 `Cargo.lock`；`cargo build`（非 dev）可编译且 `src/internal/vault.rs` 可直接 `use pgp`。
- [ ] TTY 下由 Libra 隐藏提示采集口令（不依赖 agent 转交）。
- [ ] 非交互且缺 `--passphrase-file` → `LBR-CLI-002` 且零写入。
- [ ] `--file` 受保护证书缺口令 → `LBR-CLI-002` 且零写入。
- [ ] 错误口令 → `LBR-IO-001` 且零写入。
- [ ] 重建结果为未受保护 transferable 证书，重新解析指纹不变。
- [ ] 重建证书可再次 unlock 并产生有效签名（往返）。
- [ ] 口令/明文/中间缓冲 `zeroize`；sentinel 不出现于任何输出（自动化断言）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `cargo build`（默认 profile，非 dev）成功：证明 `pgp` 已在 `[dependencies]`、release 可用
- [ ] `source .env.test && cargo test --lib internal::vault`（new：`protected_certificate_unlocks_with_passphrase_file`、`tty_prompt_collects_passphrase`、`rebuilt_certificate_is_unprotected_and_fingerprint_stable`、`rebuilt_certificate_can_sign_roundtrip`、`passphrase_and_plaintext_buffers_are_zeroized`）
- [ ] `source .env.test && cargo test --test command_test config_test`（new：`config_import_gpg_key_protected_file_requires_passphrase`、`config_import_gpg_key_wrong_passphrase_writes_nothing`）
- [ ] sentinel 捕获（本卡内 harness，仅用 VG-02 已交付路径）：

```bash
set -eu
umask 077
DIR=$(mktemp -d /tmp/issue-vg/vg02.XXXXXX)
# 隔离执行：在一次性 Libra 仓库与隔离 LIBRA_HOME 中运行，卡尾删除；绝不改动当前检出
ROOT=$(libra rev-parse --show-toplevel)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP" "$DIR"' EXIT
mkdir -p "$TMP/home"
( cd "$TMP" && LIBRA_HOME="$TMP/home" "$ROOT/target/debug/libra" init -q )
run(){ ( cd "$TMP" && LIBRA_HOME="$TMP/home" "$ROOT/target/debug/libra" "$@" ); }
# VG-09 需要 HEAD 才能打 tag：先建基线提交
run config set user.name "vg09 tester" >/dev/null 2>&1 || true
run config set user.email "vg09@example.invalid" >/dev/null 2>&1 || true
printf "base\n" >"$TMP/base.txt"
run add base.txt >/dev/null
run commit -m "base" >/dev/null
FIXPASS='libra-test-fixture-passphrase'   # fixture 以该固定测试口令生成
SENT="$FIXPASS"
printf '%s' "$SENT" >"$DIR/pass.txt"
# 成败不以「import 成功」判定（持久化属 VG-03）：只要求「错误口令失败且零泄漏」；
# 成功导入的端到端 sentinel 门归 VG-03（持久化落地后）。
# 错误口令（预期失败、零写入；显式捕获 rc，避免 set -e 提前中止）
printf 'wrong-passphrase' >"$DIR/wrong.txt"; chmod 600 "$DIR/wrong.txt"
if run config import-gpg-key --file "$ROOT/tests/data/fake-gpg/protected-secret.asc" --passphrase-file "$DIR/wrong.txt" >"$DIR/out-bad.txt" 2>"$DIR/err-bad.txt"; then
  echo "FAIL: wrong passphrase must fail"; exit 1
else
  rc_bad=$?; printf '%s' "$rc_bad" >"$DIR/rc-bad.txt"
fi
test "$(cat "$DIR/rc-bad.txt")" -ne 0 || { echo FAIL: wrong passphrase must fail; exit 1; }
# 固定字符串三分支零命中（-e 逐模式；0=命中失败/1=无命中通过/>1=错误）
mapfile -t CAPTURES < <(find "$DIR" -maxdepth 1 -type f \( -name 'out-*' -o -name 'err-*' \) | sort)
[ "${#CAPTURES[@]}" -gt 0 ] || { echo "FAIL: no capture files"; exit 1; }
if rg -F -n -e "$SENT" -e 'BEGIN PGP PRIVATE KEY' -e 'BEGIN PGP SIGNATURE' "${CAPTURES[@]}"; then
  echo FAIL: secret material found; exit 1
else
  rc=$?; [ "$rc" -eq 1 ] || { echo "ERROR: rg exit $rc"; exit "$rc"; }
  echo OK: zero secret hits in VG-02 captures
fi
```
- [ ] 手工证据（ER-VG-03）：真实 gpg 受保护证书导入 + 错误口令，存 `/tmp/issue-vg/vg02/`
- [ ] Display-pin（new）：`protected_import_missing_passphrase_message_is_pinned`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 新建共享 vault 解保护/重建路径（安全敏感）`
**Dependencies:** `VG-10`、`DEP-VG-02`、`DEP-VG-04`、`DEP-VG-01` **Deliverables:** N/A
**Implementation write set:** `src/internal/vault.rs`、`Cargo.toml`（`pgp` 从 `[dev-dependencies]` 移入 `[dependencies]`）、`Cargo.lock`（工具链刷新）、`tests/command/config_test.rs`、`tests/data/fake-gpg/`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/internal/vault.rs`、`Cargo.toml`、`Cargo.lock`、`tests/command/config_test.rs`、`tests/data/fake-gpg/`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** `N/A` **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页 EN+zh 口令获取/受保护证书/重建。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 本地可 revert；发布后前滚；失败零写入。 **Security and privacy:** GC-VG-01；packet 重建只保留必要字段。
**Performance budget:** `< 1s`。 **Estimated scope:** `M` **Version increment:** `N/A` **Release boundary:** `family child（REL-VG-01）` **C/D coverage from:** `VG-09`
**Granularity:** `type=implementation; axis=解保护与证书重建; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=DEP-VG-01,VG-10,DEP-VG-02,DEP-VG-04; writeset=序列化于 VG-10; release=family child（REL-VG-01）; split-from=N/A（保留；拆出 VG-11）; exception=N/A`

### Task VG-11: 签名资格与选择（自 VG-02 拆出）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 按 ADR-VG-09 第 1..3 条判定并选择签名 (sub)key；自 VG-02 拆出。

**Out of scope:** 解保护/重建（VG-02）；验证时刻/issuer 验证（VG-05）。

**Current evidence:** 事实基线第 10 行；ADR-VG-09 第 1..3 条。

**判据规范（非计数正文；逐门可复制命令）；每门一条可复制命令，无省略号）：**
- G1/G2/G3：`source .env.test && cargo test --lib internal::vault -- signing_subkey_requires_sign_flag_and_valid_binding`
- G4：`source .env.test && cargo test --lib internal::vault -- revoked_subkey_is_rejected`
- G5：`source .env.test && cargo test --lib internal::vault -- expired_subkey_is_rejected_at_evaluation_time`
- G6：`source .env.test && cargo test --lib internal::vault -- disabled_subkey_is_rejected`
- G7：`source .env.test && cargo test --lib internal::vault -- issuer_conflict_subkey_is_rejected`
- G8：`source .env.test && cargo test --lib internal::vault -- signing_subkey_selected_by_newest_valid_self_signature`
- G9：`source .env.test && cargo test --lib internal::vault -- subkey_choice_is_deterministic_by_key_id`
- G10：`source .env.test && cargo test --lib internal::vault -- primary_key_signs_when_no_usable_signing_subkey`
- G11：`source .env.test && cargo test --test command_test config_test -- no_eligible_signing_key_message_is_pinned`

**Acceptance criteria:**
- [ ] 资格要求 Sign 能力 + 有效 subkey binding + binding 由本证书 primary 签发。
- [ ] revoked (sub)key 被拒绝。
- [ ] expired (sub)key 在评估时刻被拒绝。
- [ ] disabled (key-flag/self-signature) 被拒绝。
- [ ] issuer 冲突的 (sub)key 被拒绝。
- [ ] 属性来源 = newest-effective-self-signature。
- [ ] 无合格 subkey 时 primary 自身合格可用，否则 fail closed。
- [ ] 多合格 subkey 选择确定（key id 升序）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --lib internal::vault`（new：`signing_subkey_requires_sign_flag_and_valid_binding`、`revoked_subkey_is_rejected`、`expired_subkey_is_rejected_at_evaluation_time`、`disabled_subkey_is_rejected`、`issuer_conflict_subkey_is_rejected`、`signing_subkey_selected_by_newest_valid_self_signature`、`subkey_choice_is_deterministic_by_key_id`、`primary_key_signs_when_no_usable_signing_subkey`）
- [ ] 五类拒绝 + primary-only fixture 与 ADR-VG-09 追踪表一一对应
- [ ] `source .env.test && cargo test --test command_test config_test -- config_import_gpg_key`（回归：导入使用所选 key id）
- [ ] 手工证据：真实 gpg primary-only 与 subkey 证书各一，存 `/tmp/issue-vg/vg11/`
- [ ] Display-pin（new）：`no_eligible_signing_key_message_is_pinned`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 新建共享签名资格判定路径`
**Dependencies:** `VG-02`、`DEP-VG-02`、`DEP-VG-01` **Deliverables:** N/A
**Implementation write set:** `src/internal/vault.rs`、`tests/command/config_test.rs`、`tests/data/fake-gpg/`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/internal/vault.rs`、`tests/command/config_test.rs`、`tests/data/fake-gpg/`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** `N/A` **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页 EN+zh 资格规则。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 本地可 revert；发布后前滚。 **Security and privacy:** GC-VG-05；拒绝路径不泄漏证书细节。
**Performance budget:** O(subkey 数)。 **Estimated scope:** `S` **Version increment:** `N/A` **Release boundary:** `family child（REL-VG-01）` **C/D coverage from:** `VG-09`
**Granularity:** `type=implementation; axis=签名资格与选择; recovery=immutable-release; complete=yes; self-contained=yes; AC=11/8@EX-VG-01; VER=5/8; landing=1; prod-files=1; scope=S; deps=DEP-VG-01,VG-02,DEP-VG-02; writeset=序列化于 VG-02; release=family child（REL-VG-01）; split-from=VG-02; exception=EX-VG-01`

### Task VG-03: 持久化与首度签名策略（保留主轴：持久化；拆出 VG-12、VG-13）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 把重建证书与元数据以 `encrypted=true` 持久化，落实首度导入的 `vault.signing` 策略（ADR-VG-12）与失败回滚。本卡保留「持久化」主轴；拆出 VG-13（替换与历史）、VG-12（redaction 读取面）。

**Out of scope:** 替换/历史（VG-13）；redaction 读路径（VG-12）；签名/验证（VG-04/05）。

**Current evidence:** 事实基线第 3、4、7 行；ADR-VG-02/12。
（非计数正文；逐门可复制命令）；每门一条可复制命令）：**
- G1 encrypted=true：`source .env.test && cargo test --lib internal::vault -- import_persists_encrypted_seckey_and_metadata`
- G2 同批元数据：同 G1 命令（同一测试断言批写）
- G3 失败回滚：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_failure_leaves_no_metadata`
- G4 首度签名策略：`source .env.test && cargo test --lib internal::vault -- import_sets_vault_signing_true_when_unset`
- G5 显式 false 保持：`source .env.test && cargo test --lib internal::vault -- import_keeps_explicit_false_signing_with_hint`
- G6 冲突码：`source .env.test && cargo test --test command_test config_test -- config_import_gpg_key_requires_replace_when_active_key_exists`
- G7 幂等：`source .env.test && cargo test --lib internal::vault -- duplicate_import_is_idempotent`
**Acceptance criteria:**
- [ ] `config generate-gpg-key` 在 `source=imported` 时 **fail closed**（提示迁移卡 VG-14 未上线/改用 `--file` 重新导入），不得覆盖 `vault.gpg.pubkey` 或吞掉 config 写错误。
- [ ] `seckey_enc` 以 `encrypted=true` 写入。
- [ ] `vault.gpg.*` 元数据与私钥同批写入，`source` 最后写。
- [ ] 任一步失败回滚到导入前（无半套状态）。
- [ ] `vault.signing` 未设置时导入成功置 `true`。
- [ ] `vault.signing=false` 显式设置时保持 `false` 并输出启用提示。
- [ ] 已有活动密钥且未 `--replace` → `LBR-CONFLICT-002`。
- [ ] 同一指纹重复导入幂等（不新增历史行，仅刷新 `imported_at`/`signing_key_id`）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --test command_test config_test -- config_generate_gpg_key_refuses_imported_source_until_migration`（new：空窗期 fail-closed 守卫）
- [ ] `source .env.test && cargo test --lib internal::vault`（new：`import_persists_encrypted_seckey_and_metadata`、`import_partial_failure_rolls_back`、`import_sets_vault_signing_true_when_unset`、`import_keeps_explicit_false_signing_with_hint`、`duplicate_import_is_idempotent`）
- [ ] `source .env.test && cargo test --test command_test config_test`（new：`config_import_gpg_key_requires_replace_when_active_key_exists`、`config_import_gpg_key_failure_leaves_no_metadata`）
- [ ] 手工证据：失败注入（只读配置库/写失败）与恢复，存 `/tmp/issue-vg/vg03/`
- [ ] Display-pin（new）：`import_conflict_message_is_pinned`、`import_signing_disabled_hint_is_pinned`
- [ ] 首度导入两分支（未设置 / `--vault=false` 仓）各有集成用例

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 修改共享加密写入路径与 config 命令面`
**Dependencies:** `VG-11`、`DEP-VG-01`、`DEP-VG-02` **Deliverables:** N/A
**Implementation write set:** `src/internal/vault.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/internal/vault.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** `N/A` **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页 EN+zh 键空间/首度签名策略/回滚。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 本地可 revert；发布后前滚；失败零写入；数据面恢复 = `remove-gpg-key --force`（VG-08）。 **Security and privacy:** GC-VG-01/04。
**Performance budget:** `< 200ms`。 **Estimated scope:** `M` **Version increment:** `N/A` **Release boundary:** `family child（REL-VG-01）` **C/D coverage from:** `VG-09`
**Granularity:** `type=implementation; axis=持久化与首度签名策略; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=5/8; landing=2; prod-files=2; scope=M; deps=VG-11,DEP-VG-01,DEP-VG-02; writeset=序列化于 VG-11; release=family child（REL-VG-01）; split-from=N/A（保留；拆出 VG-12、VG-13）; exception=N/A`

### Task VG-13: 替换与历史不变量（自 VG-03 拆出）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 实现 `--replace` 的历史登记与统一历史不变量（ADR-VG-04）；自 VG-03 拆出。

**Out of scope:** 持久化写入细节（VG-03）；移除（VG-08）；redaction（VG-12）。

**Current evidence:** 事实基线第 1、6 行；ADR-VG-04/06。

**判据规范（非计数正文；逐门可复制命令）；每门一条可复制命令）：**
- G1 覆盖前写历史行：`source .env.test && cargo test --lib internal::vault -- replace_writes_history_before_overwrite`
- G2 生成公钥快照：`source .env.test && cargo test --lib internal::vault -- replace_snapshots_generated_pubkey`
- G3 统一不变量：`source .env.test && cargo test --lib internal::vault -- history_invariant_covers_generated_keys`
- G4 顺序确定：`source .env.test && cargo test --lib internal::vault -- history_writes_are_idempotent_by_fingerprint`
- G5 替换不删历史行：`source .env.test && cargo test --lib internal::vault -- replace_never_deletes_history_rows`（new；移除路径的不变量归 VG-08 G3/G9-G12）
- G6 用户显式 unset：文档写明后果（手工核对 `docs/commands/config.md`；锚点定位用）
**Acceptance criteria:**
- [ ] `--replace` 在任何覆盖前把当前活动公钥写入 `history.<FPR>.pubkey`。
- [ ] `source=generated` 且无 `generated_pubkey` 时先写入快照。
- [ ] 历史不变量：任何曾活动公钥（含生成）都出现在历史；重复写幂等（同指纹一行）。
- [ ] 历史顺序确定（指纹字典序）。
- [ ] 无自动路径删除 `history.*` 或 `generated_pubkey`（GC-VG-07）。
- [ ] 用户显式 `config unset vault.gpg.history.<FPR>.pubkey` 可删单行（文档写明后果）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --lib internal::vault`（new：`replace_writes_history_before_overwrite`、`replace_snapshots_generated_pubkey`、`history_invariant_covers_generated_keys`、`history_writes_are_idempotent_by_fingerprint`）
- [ ] `source .env.test && cargo test --test command_test config_test -- config_import_gpg_key`（new：`config_replace_keeps_history_verifiable`）
- [ ] 零命中守衛：代码路径中无删除 `history.` 的调用（`rg` 退出码模板，锚点定位用）
- [ ] 手工证据：生成→导入→再导入的 history 行核对，存 `/tmp/issue-vg/vg13/`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 修改共享历史公钥登记路径`
**Dependencies:** `VG-03`、`DEP-VG-02`、`DEP-VG-01` **Deliverables:** N/A
**Implementation write set:** `src/internal/vault.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/internal/vault.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** `N/A` **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页 EN+zh 替换/历史/不可删除性。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 本地可 revert；发布后前滚。 **Security and privacy:** 历史仅公钥材料。
**Performance budget:** O(history)。 **Estimated scope:** `S` **Version increment:** `N/A` **Release boundary:** `family child（REL-VG-01）` **C/D coverage from:** `VG-09`
**Granularity:** `type=implementation; axis=替换与历史不变量; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=4/8; landing=2; prod-files=2; scope=S; deps=DEP-VG-01,VG-03,DEP-VG-02; writeset=序列化于 VG-03; release=family child（REL-VG-01）; split-from=VG-03; exception=N/A`

### Task VG-12: redaction 读取面（自 VG-03 拆出）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 让 `vault.gpg.seckey_enc` 在 get/bare-read/list/JSON/reveal 全路径不可读（predicate-first）；自 VG-03 拆出。

**Out of scope:** 持久化（VG-03）；历史（VG-13）。

**Current evidence:** 事实基线第 5、6 行；ADR-VG-02 第 3 条。

**判据规范（非计数正文；逐门可复制命令）；每门一条可复制命令）：**
- G1 internal key：`source .env.test && cargo test --lib internal::config -- vault_gpg_seckey_enc_is_internal_key`
- G2 predicate-first get：`source .env.test && cargo test --lib internal::config -- render_get_value_redacts_internal_key_before_encrypted_check`
- G3 predicate-first list：`source .env.test && cargo test --lib internal::config -- list_rendering_redacts_internal_key_even_when_plaintext_stored`
- G4 reveal 拒绝：`source .env.test && cargo test --test command_test config_test -- config_get_reveal_list_hide_imported_secret`
- G5 get 返回 `<REDACTED>`：同 G4 命令
- G6 show-origin 双分支：`source .env.test && cargo test --test command_test config_test -- config_list_show_origin_redacts_imported_secret`
- G7 sentinel：卡内 sentinel 捕获脚本（get/list/JSON/show-origin 全捕获零命中）
**Acceptance criteria:**
- [ ] `is_vault_internal_key` 显式包含 `vault.gpg.seckey_enc`。
- [ ] `render_get_value` predicate-first（internal 判定先于 encrypted 早退）。
- [ ] list/JSON 渲染 predicate-first（非 encrypted 但 internal 也 redact）。
- [ ] `--reveal` 对 `vault.gpg.seckey_enc` 拒绝（Display-pin）。
- [ ] `get`/bare read 返回 `<REDACTED>`。
- [ ] `config list --show-origin`（human 与 JSON 两分支）同样不显示 `seckey_enc` 值。
- [ ] sentinel 捕获（get/list/JSON；trace 门归 VG-04/VG-09）零命中（`rg -F` 三支退出码模板）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --lib internal::config`（new：`vault_gpg_seckey_enc_is_internal_key`、`render_get_value_redacts_internal_key_before_encrypted_check`、`list_rendering_redacts_internal_key_even_when_plaintext_stored`）
- [ ] `source .env.test && cargo test --test command_test config_test`（new：`config_get_reveal_list_hide_imported_secret`、`config_list_show_origin_redacts_imported_secret`）
- [ ] sentinel 捕获（完整命令序列）：

```bash
set -eu
umask 077
DIR=$(mktemp -d /tmp/issue-vg/vg12.XXXXXX)
# 隔离执行：在一次性 Libra 仓库与隔离 LIBRA_HOME 中运行，卡尾删除；绝不改动当前检出
ROOT=$(libra rev-parse --show-toplevel)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP" "$DIR"' EXIT
mkdir -p "$TMP/home"
( cd "$TMP" && LIBRA_HOME="$TMP/home" "$ROOT/target/debug/libra" init -q )
run(){ ( cd "$TMP" && LIBRA_HOME="$TMP/home" "$ROOT/target/debug/libra" "$@" ); }
# VG-09 需要 HEAD 才能打 tag：先建基线提交
run config set user.name "vg09 tester" >/dev/null 2>&1 || true
run config set user.email "vg09@example.invalid" >/dev/null 2>&1 || true
printf "base\n" >"$TMP/base.txt"
run add base.txt >/dev/null
run commit -m "base" >/dev/null
FIXPASS='libra-test-fixture-passphrase'
SENT="$FIXPASS"
printf '%s' "$SENT" >"$DIR/pass.txt"
if run config import-gpg-key --file "$ROOT/tests/data/fake-gpg/protected-secret.asc" --passphrase-file "$DIR/pass.txt" >"$DIR/out-import.txt" 2>&1; then :; else echo FAIL: isolated import failed; exit 1; fi
run config get vault.gpg.seckey_enc >"$DIR/out-get.txt" 2>"$DIR/err-get.txt"
if run config get --reveal vault.gpg.seckey_enc >"$DIR/out-reveal.txt" 2>&1; then echo "FAIL: reveal must be refused"; exit 1; else rc_rev=$?; printf '%s' "$rc_rev" >"$DIR/rc-reveal.txt"; fi
test "$(cat "$DIR/rc-reveal.txt")" -ne 0 || { echo FAIL: reveal must be refused; exit 1; }
run config list --json >"$DIR/out-list.json" 2>"$DIR/err-list.txt"
run config list --show-origin >"$DIR/out-origin.txt" 2>"$DIR/err-origin.txt"
run config list --show-origin --json >"$DIR/out-origin.json" 2>"$DIR/err-origin.json"
rg -q '<REDACTED>' "$DIR/out-get.txt" "$DIR/out-origin.txt" || { echo FAIL: redaction marker missing; exit 1; }
for jf in "$DIR/out-list.json" "$DIR/out-origin.json"; do
  jq -e '.. | objects | select(.key?=="vault.gpg.seckey_enc") | .value=="<REDACTED>"' "$jf" >/dev/null || { echo "FAIL: JSON redaction missing in $jf"; exit 1; }
done
mapfile -t CAPTURES < <(find "$DIR" -maxdepth 1 -type f \( -name 'out-*' -o -name 'err-*' \) | sort)
[ "${#CAPTURES[@]}" -gt 0 ] || { echo "FAIL: no capture files"; exit 1; }
if rg -F -n -e "$SENT" -e 'BEGIN PGP PRIVATE KEY' -e 'BEGIN PGP SIGNATURE' "${CAPTURES[@]}"; then
  echo FAIL: secret found; exit 1
else
  rc=$?; [ "$rc" -eq 1 ] || { echo "ERROR: rg exit $rc"; exit "$rc"; }
  echo OK: zero secret hits
fi
```
- [ ] Display-pin（new）：`reveal_internal_gpg_key_is_refused_message_is_pinned`
- [ ] 回归：`test_config_set_plaintext_on_vault_internal_key_is_failure`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 修改共享配置 redaction 判定与读取路径`
**Dependencies:** `VG-13`、`DEP-VG-01`、`DEP-VG-02` **Deliverables:** N/A
**Implementation write set:** `src/internal/config.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/internal/config.rs`、`src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** `N/A` **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页 EN+zh 的 redaction/reveal 语义。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 本地可 revert；发布后前滚。 **Security and privacy:** GC-VG-04。
**Performance budget:** O(1) 判定。 **Estimated scope:** `M` **Version increment:** `N/A` **Release boundary:** `family child（REL-VG-01）` **C/D coverage from:** `VG-09`
**Granularity:** `type=implementation; axis=redaction 读取面; recovery=immutable-release; complete=yes; self-contained=yes; AC=7/8; VER=6/8; landing=2; prod-files=2; scope=M; deps=VG-13,DEP-VG-01,DEP-VG-02; writeset=序列化于 VG-13; release=family child（REL-VG-01）; split-from=VG-03; exception=N/A`

### Task VG-04: 签名派发（含 signing subkey）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** `pgp_sign` 按 `vault.gpg.source` 分派并在 imported 路径用 `signing_key_id` 选定的 (sub)key 在进程内签名；唯一行为轴 = 签名使用哪把密钥。

**Out of scope:** 验证（VG-05）；多密钥选择（DEFER-VG-03）；新签名格式。

**Current evidence:** 事实基线第 2、7 行；ADR-VG-03。

**Acceptance criteria:**
- [ ] `source=imported` + `vault.signing=true` 时 commit 产生 `gpgsig` 且内部往返通过。
- [ ] 签名由 `signing_key_id` 指定的 (sub)key 产生。
- [ ] `tag -s` 使用导入密钥且格式与既有一致。
- [ ] `merge -S` 使用导入密钥。
- [ ] `push --signed` 的 push certificate 使用导入密钥。
- [ ] `--no-gpg-sign` / `commit.gpgSign=false` 强制不签。
- [ ] 缺 key/解密失败/签名 id 缺失 → fail closed（不回落、无半成品）。
- [ ] Debug/错误路径零密钥字节（sentinel）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --lib internal::vault`（new：`imported_source_signs_detached_payload`、`signing_subkey_material_is_used_when_present`、`missing_imported_key_fails_closed_without_fallback`）
- [ ] `source .env.test && cargo test --test command_test commit_test`（new：`commit_uses_imported_gpg_key_when_vault_signing`、`commit_no_gpg_sign_wins_over_imported_key`）
- [ ] `source .env.test && cargo test --test command_test merge_test`（new：`merge_gpg_sign_uses_imported_key`）
- [ ] `source .env.test && cargo test --test command_test tag_test`（new：`tag_sign_uses_imported_key`）
- [ ] `source .env.test && cargo test --lib command::push`（new：`push_certificate_payload_uses_imported_signing_key`）
- [ ] `source .env.test && cargo test --lib internal::vault -- generated_path_resolves_key_name_when_present`（new：`generated_key_name` 存在时 reader 解析之；producer 归 VG-14）
- [ ] `source .env.test && cargo test --lib internal::vault -- generated_path_legacy_fallback_when_key_name_absent`（new：`vault.gpg.generated_key_name` 缺失时回退遗留 `libra-signing`；本卡只实现 reader 的 legacy 回退，producer/新鲜生成断言归 VG-14）
- [ ] sentinel 门（imported 签名路径，本卡内捕获）：`source .env.test && cargo test --lib internal::vault -- imported_signing_leaks_no_secret_in_trace`（以哨兵口令导入→`LIBRA_LOG=trace libra tag -s`/`commit` 捕获 stdout/stderr，固定 `-e` 三支退出码零命中+`<REDACTED>` 存在）
- [ ] 手工证据（ER-VG-05）：commit/tag 往返归档 `/tmp/issue-vg/vg04/`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/commit.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/commit.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致；`../libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/tag.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/tag.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致；`../libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/merge.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/merge.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致；`../libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/push.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/push.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 修改共享 vault 签名派发（commit/tag/merge/push）`
**Dependencies:** `VG-12`、`DEP-VG-02`、`DEP-VG-03`、`DEP-VG-01` **Deliverables:** N/A
**Implementation write set:** `src/internal/vault.rs`、`src/command/push.rs`、`tests/command/commit_test.rs`、`tests/command/merge_test/gpg_sign.rs`、`tests/command/tag_test.rs`、`docs/commands/{commit,tag,merge,push}.md`、`docs/commands/zh-CN/{commit,tag,merge,push}.md`、`docs/development/commands/{commit,tag,merge,push}.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`
**Allowlist（ER-VG-07）:** `src/internal/vault.rs`、`src/command/push.rs`、`tests/command/commit_test.rs`、`tests/command/merge_test/gpg_sign.rs`、`tests/command/tag_test.rs`、`docs/commands/commit.md`、`docs/commands/tag.md`、`docs/commands/merge.md`、`docs/commands/push.md`、`docs/commands/zh-CN/commit.md`、`docs/commands/zh-CN/tag.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/zh-CN/push.md`、`docs/development/commands/commit.md`、`docs/development/commands/tag.md`、`docs/development/commands/merge.md`、`docs/development/commands/push.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/commit.en.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/push.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** `N/A` **Files likely touched:** 同写集
**Docs and compatibility impact:** 四命令页与开发文档签名段落。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 本地可 revert；发布后前滚；已产生签名不可撤回。 **Security and privacy:** GC-VG-01。
**Performance budget:** `< 50ms` 增量。 **Estimated scope:** `S` **Version increment:** `N/A` **Release boundary:** `family child（REL-VG-01）` **C/D coverage from:** `VG-09`
**Granularity:** `type=implementation; axis=签名派发; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/8; VER=7/8; landing=2; prod-files=2; scope=S; deps=DEP-VG-01,VG-12,DEP-VG-02,DEP-VG-03; writeset=序列化于 VG-12; release=family child（REL-VG-01）; split-from=N/A; exception=N/A`

### Task VG-05: 验证派发与允许列表
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** `pgp_verify` 固定允许列表（active→generated→history）+ issuer 定位 + 验证时刻规则；唯一行为轴 = 验证使用哪些公钥。

**Out of scope:** 第三方 keyring/SSH（DEFER-VG-02/06）；移除命令（VG-08）。

**Current evidence:** 事实基线第 8 行；ADR-VG-09 第 4..6 条。

**判据规范（非计数正文，@EX-VG-01；每门一条可复制命令，无省略号）：**
- G1：`source .env.test && cargo test --lib internal::vault -- imported_key_verifies_own_signature`
- G2：`source .env.test && cargo test --lib internal::vault -- verification_ignores_source_with_internal_fixture`
- G3：`source .env.test && cargo test --lib internal::vault -- history_public_keys_verify_replaced_key_signatures`
- G4：`source .env.test && cargo test --test command_test config_test -- config_replace_keeps_history_verifiable`
- G5：`source .env.test && cargo test --lib internal::vault -- unknown_key_signature_is_rejected`
- G6：`source .env.test && cargo test --test command_test tag_test -- tag_verify_rejects_unsigned_tag`
- G7：`source .env.test && cargo test --test command_test merge_test -- merge_verify_signatures_rejects_foreign_key`
- G8：`source .env.test && cargo test --lib internal::vault -- malformed_history_key_is_skipped_not_fatal`
- G9：`source .env.test && cargo test --lib internal::vault -- issuer_selects_trusted_subkey`
- G10：`source .env.test && cargo test --lib internal::vault -- issuer_selected_subkey_requires_eligibility`
- G11：`source .env.test && cargo test --lib internal::vault -- issuer_absent_fallback_is_bounded`
- G12：`source .env.test && cargo test --lib internal::vault -- issuer_absent_cap_exceeded_fails_closed`
- G13：`source .env.test && cargo test --lib internal::vault -- revocation_is_evaluated_at_signature_creation_time`
- G14：`source .env.test && cargo test --lib internal::vault -- expiration_is_evaluated_at_signature_creation_time`
- G15：`source .env.test && cargo test --lib internal::vault -- verification_cost_is_linear_with_history`

**Acceptance criteria:**
- [ ] imported 公钥验证自己的提交/标签签名（命令级 `merge --verify-signatures`/`tag -v`）。
- [ ] 验证与 `source` 无关（内部状态 fixture 模拟 `source=generated` 后历史仍 Good）。
- [ ] issuer 指向 subkey 时定位并先验资格。
- [ ] issuer 缺失时有界尝试（候选上限 16，超出 fail closed）。
- [ ] revoked 按签名创建时刻判定（之后 Bad、之前 Good）。
- [ ] expired 按签名创建时刻判定。
- [ ] 错误密钥 Bad、无签名 Unsigned、第三方未导入不被接受。
- [ ] 历史顺序/去重幂等；解析失败继续不 panic。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --lib internal::vault`（new：`imported_key_verifies_own_signature`、`verification_ignores_source_with_internal_fixture`、`issuer_selects_trusted_subkey`、`issuer_absent_fallback_is_bounded`、`revocation_is_evaluated_at_signature_creation_time`、`expiration_is_evaluated_at_signature_creation_time`）
- [ ] `source .env.test && cargo test --test command_test merge_test`（new：`merge_verify_signatures_accepts_imported_key`、`merge_verify_signatures_rejects_foreign_key`）
- [ ] `source .env.test && cargo test --test command_test tag_test`（new：`tag_verify_accepts_imported_key_signature`）
- [ ] `source .env.test && cargo test --lib command::commit`（回归）
- [ ] 手工证据：替换前后 Good 记录 `/tmp/issue-vg/vg05/`
- [ ] Display-pin（new）：`issuer_absent_limit_exceeded_message_is_pinned`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/merge.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/merge.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致；`../libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/tag.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/tag.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 修改共享 vault 验证路径与 merge/tag 语义`
**Dependencies:** `VG-04`、`DEP-VG-02`、`DEP-VG-03`、`DEP-VG-01` **Deliverables:** N/A
**Implementation write set:** `src/internal/vault.rs`、`tests/command/merge_test/gpg_sign.rs`、`tests/command/tag_test.rs`、`docs/commands/{merge,tag}.md`、`docs/commands/zh-CN/{merge,tag}.md`、`docs/development/commands/{merge,tag}.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`
**Allowlist（ER-VG-07）:** `src/internal/vault.rs`、`tests/command/merge_test/gpg_sign.rs`、`tests/command/tag_test.rs`、`docs/commands/merge.md`、`docs/commands/tag.md`、`docs/commands/zh-CN/merge.md`、`docs/commands/zh-CN/tag.md`、`docs/development/commands/merge.md`、`docs/development/commands/tag.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/merge.en.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/tag.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** `N/A` **Files likely touched:** 同写集
**Docs and compatibility impact:** merge/tag 页与开发文档的允许列表/时刻语义。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 本地可 revert；发布后前滚。 **Security and privacy:** 只信本仓库曾配置公钥；绑定校验 fail closed。
**Performance budget:** history ≤8 时 `< 100ms`。 **Estimated scope:** `S` **Version increment:** `N/A` **Release boundary:** `family child（REL-VG-01）` **C/D coverage from:** `VG-09`
**Granularity:** `type=implementation; axis=验证派发与允许列表; recovery=immutable-release; complete=yes; self-contained=yes; AC=15/8@EX-VG-01; VER=6/8; landing=1; prod-files=1; scope=S; deps=DEP-VG-01,VG-04,DEP-VG-02,DEP-VG-03; writeset=序列化于 VG-04; release=family child（REL-VG-01）; split-from=N/A; exception=EX-VG-01`

### Task VG-09: 家族发布点（REL-VG-01）
**Task type:** `release` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 聚合家族子卡（VG-01、VG-10、VG-02、VG-11、VG-03、VG-13、VG-12、VG-04、VG-05）一次性发布：版本面、tag、push、release.yml 监控、CDN 与安装冒烟；唯一行为轴 = 家族发布。

**Out of scope:** 新行为；管理面（VG-06/07/08）。

**Current evidence:** 版本面 `Cargo.toml:3`（`version = "0.23.x"`）、`install.sh:21`（`DEFAULT_VERSION=`）、`install.ps1`（`DEFAULT_VERSION` 同型行，开工日按 ER-VG-02 刷新）；发布工作流 `.github/workflows/release.yml:5`（`tags: v*` 触发，四平台 + installer + homebrew + stable manifest）；ADR-VG-11；REL-VG-01。

**Acceptance criteria:**
- [ ] 全家族子卡实现与各触发门绿（Acceptance=`locally-accepted`，Lifecycle=`in-progress`）。
- [ ] 版本面（`Cargo.toml`/`install.sh`/`install.ps1` + `Cargo.lock` 工具链刷新）一致。
- [ ] **发布前**在确认 SHA 预建 annotated tag 并推送（不由 `gh release create` 隐式建 tag）。
- [ ] `gh release create --verify-tag` 成功。
- [ ] `gh run view <id> --json status,conclusion,jobs,event,headBranch,headSha`：全 job success、`event=push`、`headBranch`/`headSha` 与 tag deref 提交一致。
- [ ] job 集合完整（四平台 build-and-upload + upload-install-scripts + update-homebrew-tap + verify-homebrew-formula + request-stable-manifest）。
- [ ] CDN gate PASS（manifest 签名 + 四产物 URL/size/sha256 + windows exe 字节一致 + installer 默认版本）。
- [ ] 本地安装冒烟（`--version` + commit/tag 往返）通过；子卡转 `done`/`complete`。
- [ ] 文档同批判定（ER-06a，计划级门不计入 G-03）：`N/A` —— release 卡不新增命令面；版本面/`CHANGELOG.md` 已在本卡写集内同步，命令页由家族子卡各自负责。

**Verification:**
- [ ] 聚合门：`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`（T-2）
- [ ] 版本面 guards（`compat_version_surface_sync` 等）
- [ ] 发布脚本（逐行执行）：

```bash
set -eu
mkdir -p /tmp/issue-vg/vg09
V=$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([0-9.]+)".*/\1/')
: "${INTENDED_SHA:?export INTENDED_SHA=<confirmed release commit> first}"
test "$(libra rev-parse HEAD)" = "$INTENDED_SHA" || { echo "FAIL: HEAD != INTENDED_SHA"; exit 1; }
SHA="$INTENDED_SHA"
# 发布说明落盘（可执行：至少包含版本与 CHANGELOG 本节）
{ printf 'Libra v%s\n\n' "$V"; sed -n "/^## \\[$V\\]/,/^## \\[/p" CHANGELOG.md | sed '$d'; } >/tmp/issue-vg/vg09/release-notes.md
grep -q "^## \\[$V\\]" /tmp/issue-vg/vg09/release-notes.md || { echo "FAIL: version section missing in CHANGELOG"; exit 1; }
grep -qE '^### ' /tmp/issue-vg/vg09/release-notes.md || { echo "FAIL: version section has no feature content"; exit 1; }
# 1) 先把家族本地提交推送 release 分支，并校验远端 ref 等于目标 SHA（ER-04 分支推送步）
libra push origin main
REMOTE_MAIN=$(libra ls-remote origin refs/heads/main | awk '{print $1}')
test "$REMOTE_MAIN" = "$SHA" || { echo "FAIL: remote main != intended SHA"; exit 1; }
# 2) 在 HEAD 预建 annotated tag 并推送（libra tag 无 target 参数，创建以 HEAD 为准）
libra tag -a "v$V" -m "Libra v$V"
libra push origin "v$V"
# 2b) deref 校验：tag 对象解析到同一 SHA
TAG_SHA=$(libra rev-parse "v$V^{}" 2>/dev/null || libra rev-parse "refs/tags/v$V^{}")
test "$TAG_SHA" = "$SHA" || { echo "FAIL: tag deref mismatch"; exit 1; }
# 3) 用 --verify-tag 发布（不隐式建 tag）
gh release create "v$V" -R libra-tools/libra --verify-tag --notes-file /tmp/issue-vg/vg09/release-notes.md
# 4) 选中 push 事件、headBranch=v$V 的 run 并等待
RID=$(gh run list -R libra-tools/libra --workflow release.yml --limit 10 \
  --json databaseId,headBranch,event \
  -q ".[] | select(.event==\"push\" and .headBranch==\"v$V\") | .databaseId" | head -1)
[ -n "$RID" ] || { echo "FAIL: no matching release run"; exit 1; }
gh run watch "$RID" -R libra-tools/libra --exit-status
# 5) 可执行断言：字段 + 8 个 job 具名集合
gh run view "$RID" -R libra-tools/libra \
  --json status,conclusion,jobs,event,headBranch,headSha >/tmp/issue-vg/vg09/run.json
jq -e '.status=="completed" and .conclusion=="success" and .event=="push" and .headBranch=="v'"$V"'" and .headSha=="'"$SHA"'"' /tmp/issue-vg/vg09/run.json
EXPECTED='["build-and-upload (libra, aarch64-apple-darwin, darwin, arm64, macos-latest)","build-and-upload (libra, aarch64-unknown-linux-gnu, linux, arm64, ubuntu-24.04-arm)","build-and-upload (libra, x86_64-pc-windows-msvc, windows, amd64, windows-latest)","build-and-upload (libra, x86_64-unknown-linux-gnu, linux, amd64, ubuntu-latest)","upload-install-scripts","update-homebrew-tap","verify-homebrew-formula","request-stable-manifest"]'
jq -e --argjson exp "$EXPECTED" '[.jobs[].name] | sort == ($exp | sort)' /tmp/issue-vg/vg09/run.json
# 6) CDN gate（仓内版本化脚本，见本卡写集 `tests/harness/release_cdn_gate.sh`）
bash tests/harness/release_cdn_gate.sh "$V" | tee /tmp/issue-vg/vg09/cdn.log
```

- [ ] 聚合 sentinel 门（可执行）：

```bash
set -eu
umask 077
DIR=$(mktemp -d /tmp/issue-vg/vg09.XXXXXX)
# 隔离执行：在一次性 Libra 仓库与隔离 LIBRA_HOME 中运行，卡尾删除；绝不改动当前检出
ROOT=$(libra rev-parse --show-toplevel)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP" "$DIR"' EXIT
mkdir -p "$TMP/home"
( cd "$TMP" && LIBRA_HOME="$TMP/home" "$ROOT/target/debug/libra" init -q )
run(){ ( cd "$TMP" && LIBRA_HOME="$TMP/home" "$ROOT/target/debug/libra" "$@" ); }
# VG-09 需要 HEAD 才能打 tag：先建基线提交
run config set user.name "vg09 tester" >/dev/null 2>&1 || true
run config set user.email "vg09@example.invalid" >/dev/null 2>&1 || true
printf "base\n" >"$TMP/base.txt"
run add base.txt >/dev/null
run commit -m "base" >/dev/null
SENT='libra-test-fixture-passphrase'
printf '%s' "$SENT" >"$DIR/pass.txt"
if run config import-gpg-key --file "$ROOT/tests/data/fake-gpg/protected-secret.asc" --passphrase-file "$DIR/pass.txt" >"$DIR/out-import.txt" 2>"$DIR/err-import.txt"; then :; else echo FAIL: isolated import failed; exit 1; fi
run config get vault.gpg.seckey_enc >"$DIR/out-get.txt" 2>"$DIR/err-get.txt"
run config list --json >"$DIR/out-list.json" 2>"$DIR/err-list.txt"
run config list --show-origin >"$DIR/out-origin.txt" 2>"$DIR/err-origin.txt"
run config list --show-origin --json >"$DIR/out-origin.json" 2>"$DIR/err-origin.json"
LIBRA_LOG=trace run tag -s vg09-sentinel -m sentinel >"$DIR/out-tag.txt" 2>"$DIR/err-tag.txt"
rg -F -q '<REDACTED>' "$DIR/out-get.txt" || { echo FAIL: redaction marker missing; exit 1; }
mapfile -t CAPTURES < <(find "$DIR" -maxdepth 1 -type f \( -name 'out-*' -o -name 'err-*' \) | sort)
[ "${#CAPTURES[@]}" -gt 0 ] || { echo "FAIL: no capture files"; exit 1; }
if rg -F -n -e "$SENT" -e 'BEGIN PGP PRIVATE KEY' -e 'BEGIN PGP SIGNATURE' "${CAPTURES[@]}"; then
  echo FAIL: secret material found; exit 1
else
  rc=$?; [ "$rc" -eq 1 ] || { echo "ERROR: rg exit $rc"; exit "$rc"; }
fi
```

- [ ] `gh run view` JSON 断言（上述五字段 + 8 个 job 具名集合）
- [ ] CDN gate 脚本输出（manifest 签名/四产物 URL·size·sha256/windows exe 字节一致/installer 默认版本）
- [ ] 安装冒烟日志（`/tmp/issue-vg/vg09/`）
- [ ] 失败演练记录：tag 后失败只能前滚新 patch

**Full-suite trigger:** `T-2: 发布点与聚合卡`
**Dependencies:** 全部家族子卡 **Deliverables:** 发布证据包（run/id、CDN 输出、安装冒烟日志）
**Implementation write set:** `Cargo.toml`、`install.sh`、`install.ps1`、`Cargo.lock`、`CHANGELOG.md`、`tests/harness/release_cdn_gate.sh`（新增仓内版本化 CDN 门脚本）、`docs/development/plan/plan-20260919-gpg-import.md`
**Allowlist（ER-VG-07）:** `Cargo.toml`、`install.sh`、`install.ps1`、`Cargo.lock`、`CHANGELOG.md`、`tests/harness/release_cdn_gate.sh`、`docs/development/plan/plan-20260919-gpg-import.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** Inherited **Files likely touched:** 同写集
**Docs and compatibility impact:** `CHANGELOG.md` 与版本面；ER-06a `N/A`（release 卡不新增命令面，命令页归家族子卡）。**Rollback mode:** `immutable-release`
**Migration and rollback:** 已推送 tag 不可回退，前滚新 patch。 **Security and privacy:** 证据不含密钥材料。
**Performance budget:** N/A。 **Estimated scope:** `S` **Version increment:** `patch` **Release boundary:** `family release point（REL-VG-01）` **C/D coverage from:** `self`
**Granularity:** `type=release; axis=家族发布; recovery=immutable-release; complete=yes; self-contained=yes; AC=8/12; VER=6/12; landing=0; prod-files=0; scope=S; deps=家族子卡全部; writeset=唯一发布点; release=family release point（REL-VG-01）; split-from=N/A; exception=N/A`

### Task VG-06: 密钥元数据列表信息面
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** `list --gpg-keys`（human+JSON）报告 `source`/`fingerprint`/`signing_key_id`/`uid`/`imported_at`/`history_count` 与真实 `key_type`；唯一行为轴 = 元数据展示。

**Out of scope:** 导出/移除（VG-07/08）。

**Current evidence:** 事实基线第 9 行；ADR-VG-02 第 2 条。

**判据规范（非计数正文，@EX-VG-01；每门一条可复制命令）：**
- G1 六字段 + 真实算法：`source .env.test && cargo test --test command_test config_test -- config_list_gpg_keys_reports_source_fingerprint_and_signing_key_id`
- G2 generated/imported 显示：`source .env.test && cargo test --test command_test config_test -- test_config_generate_gpg_key`（回归）
- G3 JSON additive：`source .env.test && cargo test --test command_test config_test -- config_list_gpg_keys_json_shape_is_additive`
- G4 history_count 定义：`source .env.test && cargo test --test command_test config_test -- config_list_gpg_keys_reports_source_fingerprint_and_signing_key_id`
- G5 scope 拒绝：`source .env.test && cargo test --test command_test config_test -- config_list_gpg_keys_rejects_global_and_system_scope`
- G6 无公钥缺失语义：`source .env.test && cargo test --test command_test config_test -- test_config_list_gpg_keys_outputs_configured_key_namespaces`（回归）
**Acceptance criteria:**
- [ ] human 输出六个新字段 + 真实算法/位数（无 `PGP 2048` 硬编码）。
- [ ] `generated` 与 `imported` 两种来源均正确显示。
- [ ] `--json` 新字段 additive，既有字段不变。
- [ ] `history_count` = 去重后的 `vault.gpg.history.*` 指纹数，不显示密钥内容。
- [ ] `--global`/`--system` 在**任何配置读取与输出创建之前**拒绝（复用 VG-10 helper；隔离 global-only 数据 + 输出路径不变的测试）。
- [ ] 无公钥时沿用既有缺失语义与退出码。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --test command_test config_test`（new：`config_list_gpg_keys_reports_source_fingerprint_and_signing_key_id`、`config_list_gpg_keys_json_shape_is_additive`、`config_list_gpg_keys_rejects_global_and_system_scope`）
- [ ] 回归：`test_config_list_gpg_keys_outputs_configured_key_namespaces`、`test_config_generate_gpg_key`
- [ ] 手工证据：`--gpg-keys --json` 零泄漏 `/tmp/issue-vg/vg06/`
- [ ] `--gpg-keys` 泄漏门（sentinel）：`source .env.test && cargo test --test command_test config_test -- config_list_gpg_keys_outputs_no_secret_sentinel`（本卡内捕获 `list --gpg-keys` 与 `--json` 输出，断言 `<REDACTED>` 存在且哨兵/armor 零命中；固定字符串 `-e` + 三支退出码）
- [ ] Display-pin（new）：`gpg_keys_list_imported_source_message_is_pinned`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 修改 config 命令面与共享密钥元数据`
**Dependencies:** `VG-09`、`DEP-VG-02`、`DEP-VG-01` **Deliverables:** N/A
**Implementation write set:** `src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** Inherited **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页字段表。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 发布后前滚。 **Security and privacy:** 仅公开元数据。
**Performance budget:** `< 100ms`。 **Estimated scope:** `M` **Version increment:** `patch` **Release boundary:** `independent` **C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=列表信息面; recovery=immutable-release; complete=yes; self-contained=yes; AC=6/8; VER=5/8; landing=1; prod-files=1; scope=M; deps=DEP-VG-01,VG-09,DEP-VG-02; writeset=序列化于 VG-09; release=independent; split-from=N/A; exception=N/A`

### Task VG-07: 公钥导出（`export-gpg-key`）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 新增 `libra config export-gpg-key`：stdout armor / `--fingerprint` / `--out` 原子替换；唯一行为轴 = 公钥导出。

**Out of scope:** 私钥导出（永久非目标）。

**Current evidence:** 活动公钥槽 `vault.gpg.pubkey`（`src/internal/vault.rs:198` 写入点）；原子落盘先例 `src/utils/atomic_stream.rs`（temp+rename）；`config list --gpg-keys` 渲染 `src/command/config.rs:2531`。

**Flag/输出真值表（本卡定稿）：**

| 组合 | stdout | 退出码 | 约束 |
|---|---|---|---|
| 默认 | armored 公钥 | 0 | `--out` 未给 |
| `--fingerprint` | 主指纹一行 | 0 | 与 `--out` 互斥 |
| `--out <PATH>` | 空（提示到 stderr） | 0 | 原子替换、父目录必须存在 |
| `--out` + `--fingerprint` | — | 129 | `LBR-CLI-002` |
| `--json`/`--machine`/`--quiet`（任意） | — | 129 | `LBR-CLI-002`（export 不支持机器模式） |
| 无活动公钥 | — | 128 | `LBR-CONFLICT-002` |

**判据规范（非计数正文，@EX-VG-01；每门一条可复制命令，无省略号）：**
- G1：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_default_stdout_armor`
- G2：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_fingerprint_stdout`
- G3：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_out_is_atomic`
- G4：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_out_overwrites_existing`
- G5：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_missing_parent_fails_closed`
- G6：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_fingerprint_conflicts_with_out`
- G7：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_rejects_json`
- G8：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_rejects_machine`
- G9：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_rejects_quiet`
- G10：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_missing_public_key_fails_closed`
- G11：`source .env.test && cargo test --test command_test config_test -- config_export_gpg_key_rejects_global_and_system_scope`

**Acceptance criteria:**
- [ ] 默认输出 armored 公钥到 stdout。
- [ ] `--fingerprint` 只输出主指纹。
- [ ] `--out` 以临时文件 + rename 原子替换并覆盖已有文件。
- [ ] `--out` 父目录不存在 → `LBR-IO-001`。
- [ ] `--out` 与 `--fingerprint` 组合 → `LBR-CLI-002`。
- [ ] 无活动公钥 → `LBR-CONFLICT-002` + 提示。
- [ ] `--json`/`--machine`/`--quiet` → `LBR-CLI-002`（真值表）。
- [ ] `--global`/`--system` 写入前拒绝（复用 helper）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --test command_test config_test`（new，逐行对应真值表/判据规范 G1..G11：`config_export_gpg_key_default_stdout_armor`、`config_export_gpg_key_fingerprint_stdout`、`config_export_gpg_key_out_is_atomic`、`config_export_gpg_key_out_overwrites_existing`、`config_export_gpg_key_missing_parent_fails_closed`、`config_export_gpg_key_fingerprint_conflicts_with_out`、`config_export_gpg_key_rejects_json`、`config_export_gpg_key_rejects_machine`、`config_export_gpg_key_rejects_quiet`、`config_export_gpg_key_missing_public_key_fails_closed`、`config_export_gpg_key_rejects_global_and_system_scope`）
- [ ] 表驱动断言：上列每行断言 stdout/stderr 与退出码与真值表一致
- [ ] 回归：`test_config_generate_gpg_key`
- [ ] 手工证据：导出公钥 `gpg --show-keys` 指纹一致 `/tmp/issue-vg/vg07/`
- [ ] Display-pin（new）：`gpg_key_export_missing_public_key_message_is_pinned`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。
**Full-suite trigger:** `T-1: 修改 config 命令面与共享文件写入路径`
**Dependencies:** `VG-06`、`DEP-VG-02`、`DEP-VG-01` **Deliverables:** N/A
**Implementation write set:** `src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/command/config.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** Inherited **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页导出/真值表。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 发布后前滚。 **Security and privacy:** 只输出公钥。
**Performance budget:** `< 100ms`。 **Estimated scope:** `M` **Version increment:** `patch` **Release boundary:** `independent` **C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=公钥导出; recovery=immutable-release; complete=yes; self-contained=yes; AC=11/8@EX-VG-01; VER=5/8; landing=1; prod-files=1; scope=M; deps=DEP-VG-01,VG-06,DEP-VG-02; writeset=序列化于 VG-06; release=independent; split-from=N/A; exception=EX-VG-01`

### Task VG-08: 移除与生成回落（再生成迁移已拆出至 VG-14）
**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 新增 `libra config remove-gpg-key [--force]` 与生成密钥回落（ADR-VG-06 第 3/4 条）；唯一行为轴 = 移除与生成回落；再生成 staged 迁移拆出至 VG-14。

**Out of scope:** 导入/替换（家族）；导出（VG-07）；删除历史行；**再生成 staged 迁移（VG-14）**。

**Current evidence:** 事实基线第 1 行；ADR-VG-04/06。

**可删键 allowlist（恢复/移除只允许删除下列键）：** `vault.gpg.seckey_enc`、`vault.gpg.fingerprint`、`vault.gpg.signing_key_id`、`vault.gpg.uid`、`vault.gpg.imported_at`、`vault.gpg.source`（仅改为 `generated`）。
**禁止删除：** `vault.gpg.history.*`、`vault.gpg.generated_pubkey`、`vault.gpg.generated_key_name`、`vault.gpg.pubkey`（仅可被回写）。

**判据规范（非计数正文，@EX-VG-01；每门一条可复制命令）：**
- G1：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_requires_force_for_active_key`
- G2：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_force_writes_history_row`
- G3：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_never_deletes_history_snapshot_or_keyname`
- G4：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_force_restores_generated_pubkey`
- G5：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_force_signs_with_generated_key`
- G6：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_without_generated_key_fails_signing`
- G7：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_history_stays_good`
- G8：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_is_idempotent`
- G9：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_inject_failure_history`
- G10：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_inject_failure_delete_imported`
- G11：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_inject_failure_restore_pubkey`
- G12：`source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_inject_failure_source`

**Acceptance criteria:**
- [ ] 活动导入密钥未 `--force` → `LBR-CONFLICT-002` + 提示。
- [ ] `--force` 原子收敛：写历史行（若缺）→ 删 allowlist 键 → 恢复/清空活动公钥 → `source=generated`。
- [ ] `generated_pubkey` 存在时移除后签名回落生成密钥（commit 往返）；无生成密钥时签名失败并给重新生成/导入提示。
- [ ] 移除后历史签名仍 Good（与 VG-05 联合）。
- [ ] 重复 `--force` 幂等。
- [ ] 移除各步故障注入（G9–G12）任一步失败保持移除前状态。
- [ ] 恢复/移除路径从不触碰 `history.*`/`generated_pubkey`/`generated_key_name`（allowlist 守衛测试）。
- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步。

**Verification:**
- [ ] `source .env.test && cargo test --test command_test config_test`（new：G1–G8 对应具名用例）
- [ ] `source .env.test && cargo test --test command_test config_test -- config_remove_gpg_key_inject_failure_{history,delete_imported,restore_pubkey,source}`（new，四步移除故障注入）
- [ ] `source .env.test && cargo test --test command_test commit_test -- imported`（回归回落路径）
- [ ] 手工证据：移除前后 `list --gpg-keys` + commit 往返 `/tmp/issue-vg/vg08/`
- [ ] Display-pin（new）：`gpg_key_remove_without_force_message_is_pinned`
- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous or missing VCS metadata >&2; exit 2; fi'` 必须列出本卡改动。

**Full-suite trigger:** `T-1: 修改 config 命令面与共享密钥回落路径`
**Dependencies:** `VG-07`、`DEP-VG-01`、`DEP-VG-02` **Deliverables:** N/A
**Implementation write set:** `src/command/config.rs`、`src/internal/vault.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/command/config.rs`、`src/internal/vault.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** Inherited **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页 EN+zh 的移除/回落与备份义务。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 发布后前滚；移除私钥不可找回（重新导入）；数据恢复序列 = `remove --force`（幂等）→ 按 allowlist 清理 → 重新导入。 **Security and privacy:** `--force` 显式成本；输出零密钥字节。
**Performance budget:** `< 200ms`。 **Estimated scope:** `M` **Version increment:** `patch` **Release boundary:** `independent` **C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=移除与生成回落; recovery=immutable-release; complete=yes; self-contained=yes; AC=12/8@EX-VG-01; VER=6/8; landing=2; prod-files=2; scope=M; deps=VG-07,DEP-VG-01,DEP-VG-02; writeset=序列化于 VG-07; release=independent; split-from=N/A（保留移除轴；拆出 VG-14）; exception=EX-VG-01`

### Task VG-14: `generate-gpg-key` 在 imported 前置下的 staged 迁移（自原 VG-08 拆出）

**Task type:** `implementation` **Lifecycle / Acceptance:** `pending` / 空

**Description:** 把 `generate-gpg-key` 在 `source=imported` 时的再生成迁移拆为独立卡：纯 helper、版本化键名、staged (a)–(e) 与逐步故障注入/恢复；自原 VG-08 拆出。

**Out of scope:** 移除/回落（VG-08）；导入/替换（家族）。

**Current evidence:** 事实基线第 1 行；ADR-VG-06 第 6/7 条。

**判据规范（非计数正文，@EX-VG-01；每门一条可复制命令）：**
- G1：`source .env.test && cargo test --test command_test config_test -- config_generate_gpg_key_after_import_inject_failure_after_a`
- G2：`source .env.test && cargo test --test command_test config_test -- config_generate_gpg_key_after_import_inject_failure_after_b`
- G3：`source .env.test && cargo test --test command_test config_test -- config_generate_gpg_key_after_import_inject_failure_after_c`
- G4：`source .env.test && cargo test --test command_test config_test -- config_generate_gpg_key_after_import_inject_failure_after_d1`
- G5：`source .env.test && cargo test --test command_test config_test -- config_generate_gpg_key_after_import_inject_failure_after_d2`
- G6：`source .env.test && cargo test --test command_test config_test -- config_generate_gpg_key_after_import_inject_failure_after_e`
- G7：`source .env.test && cargo test --test command_test config_test -- config_generate_gpg_key_after_import_resumes_idempotently`
- G8：`source .env.test && cargo test --test command_test config_test -- config_generate_gpg_key_after_import_same_nanosecond_collision_retries`
- G9：`source .env.test && cargo test --lib internal::vault -- generate_pgp_key_retry_exhaustion_fails_closed`
- G10：`source .env.test && cargo test --lib internal::vault -- generate_pgp_key_propagates_config_write_error`
- G11：`source .env.test && cargo test --test command_test config_test -- fresh_generation_records_versioned_key_name`（new：首次生成写入版本化键名）
- G12：`source .env.test && cargo test --lib internal::vault -- generated_verification_resolves_generated_key_name`（new：验证路径解析 `generated_key_name`）

**Acceptance criteria:**
- [ ] 抽出纯 `vault_generate_pgp_key(key_name, ...)` helper（不写 config）；`generate_pgp_key` 的 config 写错误必须传播。
- [ ] 生成键名版本化 `libra-signing-<unix-ns>` 并写 `vault.gpg.generated_key_name`；碰撞 probe 重试 ≤8 次后 fail closed。
- [ ] staged (a) 快照 →(b) 纯 helper →(c) `generated_pubkey` →(d1) `generated_key_name` →(d2) `vault.gpg.pubkey` →(e) `source=generated`；各步故障注入与补偿按 ADR-VG-06 表。
- [ ] (e) 失败后重跑幂等（`generated_key_name` 存在即跳过生成直接补 `source`）。
- [ ] 首次生成写入版本化键名；验证路径解析 `generated_key_name`。
- [ ] `source=imported` 时旧导入公钥先入 history；(d2)/(e) 的过渡态可经 history 验证。
- [ ] `config_remove_gpg_key` 不得删除 `generated_key_name`（与 VG-08 联合断言）。

- [ ] 文档同批判定（ER-06a 计划级门；不计入 G-03 计数）：本卡命令页 EN+zh-CN、开发文档、`COMPATIBILITY.md` 对应行与 `../libra-backend` 网站页均随本卡同步（具体文件见 Docs and compatibility impact）。

**Verification:**
- [ ] `source .env.test && cargo test --lib internal::vault`（new：`generate_pgp_key_propagates_config_write_error`、`generate_pgp_key_retry_exhaustion_fails_closed`、`generated_verification_resolves_generated_key_name`）
- [ ] `source .env.test && cargo test --test command_test config_test -- test_config_generate_gpg_key`（new：`config_generate_gpg_key_after_import_inject_failure_after_{a,b,c,d1,d2,e}`、`config_generate_gpg_key_after_import_resumes_idempotently`、`config_generate_gpg_key_after_import_same_nanosecond_collision_retries`、`fresh_generation_records_versioned_key_name`；回归既有生成用例）
- [ ] 手工证据：imported→generate 迁移前后 `config list --gpg-keys` + 验证记录，存 `/tmp/issue-vg/vg14/`

- [ ] 网站页同步（ER-VG-08 计划级门；按 G-03 不计入 VER 计数）：`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`：执行 `cd ../libra-backend && bash -c 'if [ -e .git ] && [ ! -e .libra ]; then git status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; elif [ -e .libra ] && [ ! -e .git ]; then /mnt/gitmono/libra/target/debug/libra status --short --branch -- apps/tanstack-app/content/docs/commands/config.en.md; else echo ERROR: ambiguous VCS metadata >&2; exit 2; fi'` 必须列出本卡改动，且 diff 与本卡 EN/zh/开发文档一致。

**Full-suite trigger:** `T-1: 修改 config 命令面、生成面与共享密钥来源迁移`
**Dependencies:** `VG-08`、`DEP-VG-01`、`DEP-VG-02` **Deliverables:** N/A
**Implementation write set:** `src/command/config.rs`、`src/internal/vault.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`
**Allowlist（ER-VG-07）:** `src/command/config.rs`、`src/internal/vault.rs`、`tests/command/config_test.rs`、`docs/commands/config.md`、`docs/commands/zh-CN/config.md`、`docs/development/commands/config.md`、`COMPATIBILITY.md`、`../libra-backend/apps/tanstack-app/content/docs/commands/config.en.md`（执行时逐行写入 `$DIR/allowlist.txt`；列表之外的任何新增/改动一律越界）。
**Release write set:** Inherited **Files likely touched:** 同写集
**Docs and compatibility impact:** config 页 EN+zh 的再生成迁移/回落/备份义务。（同卡验收门：ER-VG-08） **Rollback mode:** `immutable-release`
**Migration and rollback:** 发布后前滚；过渡态经 history 可验证，重跑幂等。 **Security and privacy:** GC-VG-01/07。
**Performance budget:** `< 300ms`。 **Estimated scope:** `M` **Version increment:** `patch` **Release boundary:** `independent` **C/D coverage from:** `self`
**Granularity:** `type=implementation; axis=再生成 staged 迁移; recovery=immutable-release; complete=yes; self-contained=yes; AC=12/8@EX-VG-01; VER=3/8; landing=2; prod-files=2; scope=M; deps=VG-08,DEP-VG-01,DEP-VG-02; writeset=序列化于 VG-08; release=independent; split-from=VG-08; exception=EX-VG-01`

## 测试矩阵

| 行为轴 | 单元（`--lib`） | 集成（`--test command_test`） | 手工/脚本证据 |
|---|---|---|---|
| spike（VG-00） | 临时（不入库） | — | Q1..Q4 日志 + allowlist 守衛 |
| 发现/版本（VG-01） | vault 3 新 | config_test 2 新 | 真实 gpg 归档 |
| 选码/作用域（VG-10） | vault 3 新 | config_test 4 新 | 零副作用核对 |
| 解保护/重建（VG-02） | vault 5 新 | config_test 2 新 | sentinel + 错误口令 |
| 资格/选择（VG-11） | vault 8 新（G1–G10 逐门；G1–G3 合 1） | config_test 1（G11 Display-pin）+ 回归 | primary-only/subkey 证书 |
| 持久化/签名策略（VG-03） | vault 5 新 | config_test 3 新（含空窗守衛） | 失败注入 |
| 替换/历史（VG-13） | vault 5 新（G1–G5） | config_test 1 新 | 三方向历史核对 |
| redaction（VG-12） | internal::config 3 新 | config_test 1 新 + 回归 | sentinel 脚本 |
| 签名（VG-04） | vault 6（含 generated_key_name reader + sentinel 门）、`command::push` 1 | commit 2、merge 1、tag 1 | commit/tag 往返 |
| 验证（VG-05） | vault 6 新 | merge 2、tag 1 + 回归 | 替换前后 Good |
| 列表（VG-06） | — | config_test 3 新 + 2 回归 | 零泄漏 JSON |
| 导出（VG-07） | — | config_test 11 新（G1–G11 逐门）+ 1 回归 | gpg 指纹核对 |
| 移除/回落（VG-08） | — | config_test 12 门（G1–G12：8 行为 + 4 故障注入） | 移除往返 |
| 再生成迁移（VG-14） | internal::vault 3 门（G9/G10/G12） | config_test 9 门（G1–G8、G11）+ 生成回归 | imported→generate 往返 |
| 家族发布（VG-09） | — | T-2 + 版本 guards | run/CDN/安装冒烟 |
| 兼容面（跨卡） | — | 既有 `merge_test/gpg_sign.rs`、`init_test.rs` 全绿 | — |

## 追溯表

| 缺口 | 卡 | AC | 验证 |
|---|---|---|---|
| GAP-VG-01、GAP-VG-05 | VG-01、VG-10、VG-02 | 各卡 AC | 单元 + config_test + 手工 |
| GAP-VG-02、GAP-VG-08 | ADR-VG-02/10 + VG-00/VG-02 | VG-00 Q1..Q4；VG-02 AC5/6 | spike 结论 + 重建往返 |
| GAP-VG-03 | VG-03、VG-06 | VG-06 AC1/2/4 | 列表字段用例 |
| GAP-VG-04 | ADR-VG-04/06 + VG-13/05/08/14 | VG-13 AC1..5；VG-05 AC2；VG-08 AC3/5/8；VG-14 AC2/6 | 替换/移除/再生成历史用例 |
| GAP-VG-06 | GC-VG-04 + VG-12 | VG-12 AC1..6 | sentinel + CLI 用例 |
| GAP-VG-07 | ADR-VG-09 + VG-11/05 | VG-11 AC1..8；VG-05 AC3..6 | 五类拒绝 + primary-only + issuer-absent + 时刻 |
| GAP-VG-09 | ADR-VG-11 + VG-09 | VG-09 AC1..8 | REL-VG-01 证据 |
| GAP-VG-10 | ADR-VG-12 + VG-03 | VG-03 AC4/5 | 两分支集成用例 |
| GAP-VG-11 | ADR-VG-06（6/7） + VG-08/VG-14 | VG-08 移除故障注入；VG-14 AC1..7 | staged 故障注入 + 过渡态可验证 |

## 里程碑验收与回滚

| 里程碑 | 完成条件 | 发布/证据 | 回滚或前滚 |
|---|---|---|---|
| M0 | Phase 0：~~双 `PASS`~~ ✅（R29）、DEP 复核、索引、VG-00 go 结论 | Review log；spike 日志 | N/A |
| M1 | Phase 1：家族子卡 `locally-accepted`（Lifecycle `in-progress`），无推送 | 各卡 focused/触发门证据 | 逆序 revert 本地提交 |
| M2 | Phase 2：VG-09 发布，子卡转 `complete` | run/CDN/安装冒烟 | 已推送 tag 前滚新 patch |
| M3 | Phase 3：VG-06/07/08/14 独立发布 | 四个 patch + 全量门 | 前滚 |

### 故障恢复矩阵

| 故障点 | 可接受残留 | 恢复动作 | 禁止结果 |
|---|---|---|---|
| VG-00 no-go | 无代码变更 | 按降级选项改计划并重评审 | 带未验证路径开工 |
| 导入写键失败 | 无（回滚到导入前） | 重跑导入 | 留下 `source=imported` 但无私钥 |
| 口令/解保护失败 | 零写入 | 重试或 `--file` | 明文落盘/argv |
| 资格判定异常 | 导入拒绝 | 修/换证书 | 跳过校验 |
| 签名私钥解密失败 | 命令失败、HEAD 不变 | 修 vault/unseal key；必要时 `remove --force` | 静默未签名或回落 |
| `remove --force` 误删风险 | 仅 allowlist 键可删 | 重新导入恢复 | 删除 `history.*`/`generated_pubkey` |
| 家族发布失败 | 已推送 tag 不可撤回 | 前滚新 patch | 删除已推送 tag |
| 网站仓元数据二义 | 含网站页卡 `blocked` | 修复元数据/改用正确 status 命令 | 猜分支/部分发布 |

## 风险登记

| 风险 | 影响 | 缓解 | 任务 |
|---|---|---|---|
| pgp 0.19 无法满足重建/subkey 需求 | 高 | VG-00 go/no-go + 降级选项 | VG-00 |
| redaction 漏洞 | 高 | encrypted=true + predicate-first + reveal 拒绝 + sentinel | VG-12 |
| 口令/明文泄漏 | 高 | Libra 自采口令 + zeroize + sentinel | VG-02/03 |
| 与 plan-20260919 I–I 冲突 | 中 | DEP-VG-01 串行 | 全部 |
| commit/merge 文档并发改 | 中 | DEP-VG-03 互斥 | VG-04/05 |
| 独立发布中间坏状态 | 高 | ADR-VG-11/REL-VG-01 | 家族子卡/VG-09 |
| 再生成覆盖导入公钥 | 中 | ADR-VG-06 第 6 条 staged + 故障注入；空窗期由 VG-03 fail-closed 守卫兜底 | VG-03、VG-14 |
| Windows agent 路径差异 | 低 | ADR-VG-08 + `--file` | VG-01/02 |
| 网站仓二义元数据 | 低 | DEP-VG-02 硬判据 | 全部 |

## 性能与容量摘要

| 操作 | 单次成本 | 累积 | 预算/上限 | 验证 |
|---|---|---|---|---|
| 导入（gpg + 解保护 + 重建 + 存储） | 1 次进程 + 解包 + 重建 + AES-GCM | 一次性 | < 3s（不含交互） | VG-02/03 |
| 每次签名 | 解密 + 1 次签名 | 每提交/标签 | < 50ms | VG-04 |
| 每次验证 | ≤ (1+generated+history)×(sub)key，issuer 缺失 ≤16 | 每验证 | history ≤8 时 < 100ms | VG-05 |
| 存储 | 私钥密文 + 历史公钥行 | 每密钥 | < 12 KiB/活动密钥 | VG-03 |

## 兼容与文档收口

- [ ] `COMPATIBILITY.md` 五行（config/commit/tag/merge/push）已同步（DEP-VG-01/03 互斥窗口）。
- [ ] `docs/commands/config.md` EN+zh、`docs/development/commands/config.md` 已同步。
- [ ] `docs/commands/{commit,tag,merge,push}.md` EN+zh 与四份开发文档已同步。
- [ ] `docs/error-codes.md` 已同步（仅当新增映射说明）。
- [ ] `tests/INDEX.md` 已同步（仅当新增 target；预期不新增顶层 target）。
- [ ] `Cargo.toml` `[[test]]` N/A。
- [ ] `plan-long.md` 索引已登记（Phase 0）。
- [ ] `../libra-backend` 五页已同步（DEP-VG-02 硬判据 + 整卡 blocked）。

## Review log

> 用户 2026-09-19 指示：**Codex 与 Claude 双评审**，各自字面 `VERDICT: PASS` 才算通过；每轮双方并行，P0/P1 修复后进入下一轮。

| Round | Reviewer | Scope | Result | P0/P1 | P2 处置 | Evidence |
|---|---|---|---|---|---|---|
| — | — | 全文（成稿） | 尚未评审 | — | — | — |
| R1 | Claude | 成稿 | `FAIL` | P1 2 | P2 0 | `/tmp/issue-gpg/review/claude-plan-r1.log` |
| R1 | Codex | 成稿 | `FAIL` | P1 13 | P2 0 | `/tmp/issue-gpg/review/codex-plan-r1.log` |
| R2 | Claude | R1 修订版 | `FAIL` | P1 1 | P2 4（已修） | `/tmp/issue-gpg/review/claude-plan-r2.log` |
| R2 | Codex | R1 修订版 | `FAIL` | P1 16 | P2 0 | `/tmp/issue-gpg/review/codex-plan-r2.log` |
| R3 | Claude | R2 修订版 | `FAIL` | P1 1（DEP-VG-02 Libra 分支未定义） | P2 4（均已修） | `/tmp/issue-gpg/review/claude-plan-r3.log` |
| R3 | Codex | R2 修订版 | `FAIL` | P0 1（VG-08 恢复序列误删 history）+ P1 11（家族 C/D、AC 计数、S→M、G-09、DEP-VG-02、VG-00 守衛、首度签名策略、再生成原子性、fixture/cap、导出真值表、release tag 流程） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r3.log` |
| R4 | Claude | R3 修订版 | **`PASS`** | P0 0；P1 0（逐项核实 R3 的 P0/P1 均已解决） | P2 1（`landing / prod-files` 字段语义未定义）——已在本表上方补定义 | `/tmp/issue-gpg/review/claude-plan-r4.log` |
| R4 | Codex | R3 修订版 | `FAIL` | P0 0；P1 7：① AC/VER 仍按行计（VG-05/11/07/08）② sentinel/allowlist 未字面化 ③ VG-00 C/D 为空 ④ `DEP-VG-02 -> 含网站页写集的卡（含 VG-14）` 含无网站写集的 VG-00 ⑤ 网站路径省略号 + 缺同卡验收 ⑥ 再生成迁移与现 helper 不相容 ⑦ VG-07 未逐组合、VG-09 释放证据不可执行 —— 均已按 R4 修订（EX-VG-01、内联守衛脚本、C/D 归 VG-09、边收窄、具体路径 + ER-VG-08、纯 helper + 补偿表、真值表逐行测试 + 释放脚本） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r4.log` |
| R5 | Claude | R4 修订版 | `FAIL` | P0 0；P1 1（VG-00 卡内 C/D 仍写 `N/A`，与覆写表矛盾）——已在 R5 修订版修为 `VG-09`（评的是快照，当前文本已合） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r5.log` |
| R5 | Codex | R4 修订版 | `FAIL` | P0 0；P1 7：① EX-VG-01 门族未映射具名命令且卡内 `exception=N/A` 与审计表不一致 ② allowlist 脚本不取补集、`|| true` 掩盖失败、对已脏路径失效 ③ VG-00 卡内 C/D 仍写 `N/A` ④ DEP-VG-02 脏页未硬阻塞 ⑤ VG-04/VG-05 网站路径 brace 简写 ⑥ ADR-VG-06 恢复与 libvault 固定键名/无 delete 不兼容 ⑦ VG-09 tag 语法错 + 断言仅注释；另 Review log 重複 R4/Claude 行 —— 均已按 R5 修订（具名命令映射 + `exception=EX-VG-01`；allowlist 重写；VG-00 C/D=VG-09；脏页硬阻塞；路径展开；版本化 `libra-signing-<ts>` + `generated_key_name` + 幂等重跑；可执行 release 脚本；删除重複行） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r5.log` |
| R6 | Claude | R5 修订版 | **`PASS`** | P0 0；P1 0（逐项核实 R5 的 8 项修复；含 EX-VG-01、allowlist、C/D、脏页硬阻塞、路径展开、ADR-VG-06、VG-09 脚本、Review log 去重） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r6.log` |
| R6 | Codex | R5 修订版 | `FAIL` | P0 0；P1 9：① 四卡判据规范仍含 `...`/裸 test 标识 ② allowlist 无卡内清单与已脏路径内容 diff ③ `libra-signing-<unix-ts>` 精度/碰撞未定 ④ staged (d) 双写半状态未补偿 ⑤ VG-09 SHA 恒真检查 ⑥ 网站页同步未写入各卡 Verification ⑦ sentinel 脚本仍有占位符 ⑧ 修订史 R5 行陈旊 —— 均已按 R6 修订（逐门全命令；逐卡 Allowlist 行 + 已脏内容 diff 脚本；纳秒+probe 重试；d1/d2 before-image 补偿；`INTENDED_SHA`；网站门写回 Verification（ER 级不计 VER）；VG-02/VG-12 完整捕获序列；R5 行改为双方结果） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r6.log` |
| R7 | Claude | R6 修订版 | **`PASS`** | P0 0；P1 0（逐项核实 Codex R6 的 9 项 P1：逐门完整命令、allowlist 补集+脏路径内容 diff、纳秒+probe、d1/d2 before-image、`INTENDED_SHA`、网站门入 Verification、sentinel 无占位符、修订史一致；无新 P0/P1） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r7.log` |
| R7 | Codex | R6 修订版 | `FAIL` | **P0 2**：① VG-02/VG-12 sentinel 门在其卡上不可执行（依赖后续卡；rg 把模式当路径）② VG-09 脚本不建目录/发布说明且调用外部缺失 CDN 脚本。**P1 7**：① allowlist 残留 `|| true`/未规范化/未快照 untracked/未覆盖外部仓 ② VG-08 门族 G14 裸名与 G9-G13 合并 ③ d2 未存 pubkey before-image、(e) 允许 pubkey/source 不一致 ④ 碰撞/重试与 generated_key_name 查找无门 ⑤ VG-04 缺 sentinel 门 ⑥ 依赖声明不一致 ⑦ 修订史重复尾段 —— 均已按 R7 修订（VG-02 改为卡内 harness、VG-12 去掉 --gpg-keys 并移门到 VG-06、VG-09 建目录/发布说明/仓内 `tests/harness/release_cdn_gate.sh`、allowlist 硬化、G17-19 逐阶命令、d2/e 不变量与 before-image、新增 4 门、依赖补齐、修史行清理） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r7.log` |
| R8 | Claude | R7 修订版 | `FAIL` | P0 1（VG-02 sentinel 仍用随机口令，无法解静态 fixture；R8 patch 未命中该行）——已修为固定 fixture 口令；P1 0（其余 Codex R8 项均已核实解决） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r8.log` |
| R8 | Codex | R7 修订版 | `FAIL` | **P0 4**：① VG-00 只读与 gpg 生成矛盾（未隔离 GNUPGHOME）② allowlist 对所有路径快照会误杀计划内编辑 ③ VG-02 随机口令无法解静态 fixture 且 `set -e` 提前中止 ④ VG-12 同③。**P1 8**：`rg -e` 非固定字符串；ADR-VG-06 (c) 无 before-image；G15 与 G3 重复（计数 19→18）；审计 deps 缺 DEP-VG-01/04；VG-09 无 Allowlist；CHANGELOG 未入写集/验收；VG-09 聚合 sentinel 为散文；VG-12 AC6 trace 无对应脚本。**P2 2**：R4 摘要陈旧；R7/Claude 行顺序 —— 均已按 R8 修订（隔离 GNUPGHOME 例外、仅对基线已脏 allowlist 路径快照、固定 fixture 口令+显式 rc、`rg -F`、before-image、G15 去重与 18/8、审计 deps 同步、VG-09 Allowlist/CHANGELOG/可执行聚合门、VG-12 AC 去 trace、顺序与摘要修正） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r8.log` |
| R9 | Claude | R8 修订版 | `FAIL` | P0 0；P1 2（VG-10 Granularity 缺 `DEP-VG-04`——已修；G15 去重疑义——R9 已重排 G1..G18）；P2 1（修订史 R8 行未回填 Claude——已修） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r9.log` |
| R9 | Codex | R8 修订版 | `FAIL` | P0 3（allowlist 后端前缀/dir 匹配、d2 未回滚 (c)）＋ P1 5（rc-ok 断言、reveal 显式 rc、sentinel 隔离、G 门重排、generated_key_name 键空间与 fallback 测试）——均已按 R9 修订 | P2 0 | `/tmp/issue-gpg/review/codex-plan-r9.log` |
| R10 | Claude | R9 修订版 | **`PASS`** | P0 0；P1 0（逐项核实 R9 修复：allowlist 前缀/目录匹配、d2 (c) 回滚、rc-ok 断言、reveal rc、sentinel 隔离、G1..G18 重排、generated_key_name 键空间与 fallback 测试、VG-10 deps） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r10.log` |
| R10 | Codex | R9 修订版 | `FAIL` | **P0 1**（`pgp` 位于 `[dev-dependencies]`，正式构建无法编译）**P1 4**（DEP-VG-02 允许 Git 后端但状态检查固定用 `libra`；allowlist 未对非 allowlist 脏路径做内容快照；VG-12 未覆盖 `--show-origin` 渲染分支；完成判据缺 fmt/clippy）**P2 1**（修订史 R9 行未回填）—— 均已按 R10 修订（pgp 移入 `[dependencies]` + Cargo.toml/lock 入 VG-02 写集与 AC + build 门；VCS 选择 helper；全量基线脏路径快照；show-origin 门；三连收口门；R9 行回填） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r10.log` |
| R11 | Claude | R10 修订版 | `FAIL` | P0 0；P1 2：① DEP-VG-02 文字仍为三分支且「两者并存=硬阻塞」，与网站门 helper 不一致；守衛外部仓调用仍硬编码（R11 已修为 backend_status helper）② 守衛首行註解與「全部已脏路径快照」逻辑不符 —— 已修（DEP 改统一 helper 文案 + 注释修正） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r11.log` |
| R11 | Codex | R10 修订版 | `FAIL` | P0 0；P1 4：① 守衛後端 baseline/after 仍硬編碼 libra status（未用 VCS helper）② helper 只認 `.git` 目錄（worktree 的 `.git` 檔案態誤走 Libra 分支）③ 後端髒頁只做狀態快照、未做內容 diff ④ VG-12 sentinel 未實際跑 `--show-origin`（human/JSON）兩分支 —— 均已按 R11 修訂（統一 `-e .git` + exactly-one-VCS helper；後端 baseline/after 用 helper；後端髒頁內容快照比對；show-origin 兩命令捕獲並納入 redaction/零命中斷言） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r11.log` |
| R12 | Claude | R11 修订版 | `FAIL` | P0 0；P1 1（守衛註解「只对 allowlist 路径」与全量快照逻辑不符）——已修（注释改为全量快照说明）；其余 R11 修复均已核实 | P2 0 | `/tmp/issue-gpg/review/claude-plan-r12.log` |
| R12 | Codex | R11 修订版 | `FAIL` | P0 0；P1 3：① allowlist 用 diff 快照无法覆盖 staged 文件（前后均为空 diff）与后端 untracked → 改全量工作树字节 sha256 ② VG-12 `--show-origin --json` 未要求 redaction 断言 → 增 jq 断言 ③ 家族发布契约需显式授权 → ADR-VG-11 增「G-08 例外授权与依据」段 —— 均已按 R12 修订 | P2 0 | `/tmp/issue-gpg/review/codex-plan-r12.log` |
| R13 | Claude | R12 修订版 | `FAIL` | P0 0；P1 1（守衛基线注释仍称 `tracked=diff，untracked=sha256`，与全 sha256 实作不符）——已修；其余 R12 三修与历史项均核实 | P2 0 | `/tmp/issue-gpg/review/claude-plan-r13.log` |
| R13 | Codex | R12 修订版 | `FAIL` | P0 0；P1 3：① 后端快照循环缺 `MISSING` 处理 ② VG-12 jq 双文件单次断言只证其一 ③ VG-09 未推送 main/未校验远端 SHA 即打 tag；P2 2：修订史 R12 重复行、注释残留 —— 均已按 R13 修订（后端 MISSING；逐档 jq；先 `libra push origin main` + `ls-remote` SHA 校验再 tag；R12 行合并；注释已改） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r13.log` |
| R14 | Claude | R13 修订版 | **`PASS`** | P0 0；P1 0（逐项核实 R13 修复：后端 MISSING、逐档 jq、VG-09 push main + 远端 SHA 校验、R12 合并行、注释统一；历史项均保持） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r14.log` |
| R14 | Codex | R13 修订版 | `FAIL` | **P0 2**：① VG-12/VG-09 sentinel 把不存在的 `err-*.json` 传给 rg（exit 2 只接受 1）② VG-09 空仓直接 `tag -s`（无 HEAD）；**P1 2**：③ 守衛 `awk {print $2}` 不處理 rename/引號/目錄 ④ 修訂史殘留 R12 行 —— 均已按 R14 修訂（CAPTURES 既有檔案陣列 + VG-09 先建基線提交；NUL porcelain `dirty_paths` parser + 目錄遞歸 sha256 + rename 兩端；單一 R12 列） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r14.log` |
| R15 | Claude | R14 修订版 | `FAIL` | P0 0；P1 1（後端髒路徑抽取用 `tr`+`${l:3}` 繞過 `dirty_paths`，rename/引號/任意位元組下漏路徑）——已修為 `dirty_paths < backend-baseline.z`；其餘 R14 修復與歷史項均核實 | P2 0 | `/tmp/issue-gpg/review/claude-plan-r15.log` |
| R15 | Codex | R14 修订版 | `FAIL` | P0 0；P1 3：① 守衛後端 after 未對 allowlist 比對、仍可能漏後端越界 ② `dirty_paths` 輸出仍行導向（換行檔名不安全）③ `hash_path` 跟隨 symlink —— 已按 R15 修訂：守衛改單一 **byte-safe Python 實作**（NUL porcelain 解析含 rename 兩端、root+backend 新增路徑 allowlist 補集、symlink 以 link target 計、目錄 `os.walk(followlinks=False)`、缺檔 MISSING） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r15.log` |
| R16 | Claude | R15 修订版 | `FAIL` | **P0 1**（守衛無兩段式/無狀態持久化，認為捕獲後立即比對不可用——R16 修訂已改 capture/verify + manifest 解決）；P1 1（`os.walk` 子目錄順序非確定）——已補 `dirs.sort()` | P2 0 | `/tmp/issue-gpg/review/claude-plan-r16.log` |
| R16 | Codex | R15 修订版 | `FAIL` | **P0 1**：守衛 capture 後立即 verify，無卡內工作中間點，不可執行；**P1 3**：root 未用 VCS 選擇器、backend 缺失/無前綴時略過檢查、digest 漏檔名/型別/空目錄/symlink-dir、Allowlist 含 repo 內 `allowlist.txt` —— 均已按 R16 修訂（改 **capture/verify 兩段式 + 不可變 baseline manifest**；root/backend 共用 exactly-one 選擇器並 fail closed；digest 記 relative name/type/空目錄/symlink target；Allowlist 移除 repo 內 `allowlist.txt`，改標 `$DIR/allowlist.txt` 守衛私有狀態） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r16.log` |
| R17 | Claude | R16 修订版 | **`PASS`** | P0 0；P1 0（逐项核实 R16 守衛重写：capture/verify 两段式 + manifest、VCS 选择器、digest 强化/确定性、Allowlist 清理；历史项均保持） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r17.log` |
| R17 | Codex | R16 修订版 | `FAIL` | P0 0；P1 3：① manifest 可被二次 capture 覆盖/外部改 ② root VCS 工具未持久、backend_tool 未比对（`.libra`↔`.git` 掉包可掩盖变更）③ `ensure_ascii=False` 对代理转义路径会崩 —— 均已按 R17 修订（manifest 独占创建、已存在即拒绝；持久化 `root_tool`/`backend_tool` 并在 verify 比对；JSON 改 `ensure_ascii=True` 无损往返） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r17.log` |
| R18 | Claude | R17 修订版 | **`PASS`** | P0 0；P1 0（逐项核实 R17 守衛完整性：独占 manifest、root/backend tool 持久比对、ensure_ascii；历史项均保持） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r18.log` |
| R18 | Codex | R17 修订版 | `FAIL` | P0 0；P1 6：① 允許刪 manifest 重捕獲 ② manifest 未存 XY/mode（staged/執行位可偷改）③ backend 對無網站卡也探測/丟棄 tool ④ `guard.sh` 未實體化 ⑤ VG-01/10/13/04 landing 計數錯 ⑥ ER-06a 未落實到 AC —— 均已按 R18 修訂（禁止卡內重捕獲；持久化 XY+lstat mode 並比對；backend 僅按需探測；給出 guard.sh/allowlist 實體化命令；四卡改 2/2；十卡加 ER-06a 文件 AC（標註不計數）） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r18.log` |
| R19 | Claude | R18 修订版 | **`PASS`** | P0 0；P1 0（逐项核实 R18 六项：禁止重捕获、XY/mode 持久比对、backend 按需、guard 实体化、landing 2/2、ER-06a AC；历史项均保持） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r19.log` |
| R19 | Codex | R18 修订版 | `FAIL` | P0 0；P1 4：① 发布拓扑需明文口径（无「四个独立 patch」外部要求）② family 边界需逐卡引 `REL-VG-01` ③ VG-11/VG-13 缺 ER-06a 文档 AC ④ 守衛重置仅散文、可删 manifest 重捕獲；P2 1：R16–R18 叙述未回填 —— 均已按 R19 修订（ADR-VG-11 增拓扑明文；34 处 `（REL-VG-01）`；VG-11/13 补 ER-06a AC；重置改 supervisor ledger 授权 + 旧 manifest sha256 记账；叙述回填） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r19.log` |
| R20 | Claude | R19 修订版 | **`PASS`** | P0 0；P1 0（逐项核实 R19：拓扑明文、REL-VG-01 引用、VG-11/13 ER-06a AC、重置授权、叙述回填；历史项均保持） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r20.log` |
| R20 | Codex | R19 修订版 | `FAIL` | P0 0；P1 1（重置授權僅字串子串匹配、無單次性/綁定，可被前次 audit 行複用）——已按 R20 修訂：改**單次精確 permit**（`guard-permits/<card>.permit` = `card\tmanifest_sha256\tpermit_id`）並 `os.replace` 原子消費 + 審計帳本；permit 目錄/帳本由 supervisor 持有 | P2 0 | `/tmp/issue-gpg/review/codex-plan-r20.log` |
| R21 | Claude | R20 修订版 | `FAIL` | P0 0；P1 1（`vault.gpg.generated_key_name` 未列入 VG-08 禁删清单/AC8/G3）——已修为禁删 + AC 扩展 + G3 测试改名；R20 permit 修复已核实 | P2 0 | `/tmp/issue-gpg/review/claude-plan-r21.log` |
| R21 | Codex | R20 修订版 | `FAIL` | P0 0；P1 2：① 「四个独立 VG-01..VG-04 patch 发布」charter——**根因确认为评审 prompt 仍带初稿四卡上下文**（已重写评审 prompt 为现行 14 卡家族发布结构）；② manifest 仍在卡可写 `$DIR`，删除后可无 permit 重捕獲——已改 manifest 到 supervisor 持有 `/tmp/issue-vg/guard-manifests/<card>.json`；P2 1：R21 占位列先于 R20 Claude PASS 行——已调整顺序 | P2 0 | `/tmp/issue-gpg/review/codex-plan-r21.log` |
| R22 | Claude | R21 修订版（修正 prompt） | **`PASS`** | P0 0；P1 0（重新以现行结构核对：14 卡 + REL-VG-01 家族发布、守衛 capture/verify、sentinel 隔离、pgp 依赖、jq、push-main、三连门、EX 门族——全数一致） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r22.log` |
| R22 | Codex | R21 修订版（修正 prompt） | `FAIL` | **P0 2**：① 守衛 capture/reset 由卡執行者運行但寫 supervisor 目錄（權限矛盾）② VG-04 需 fresh `generated_key_name` 但 producer 在 VG-08（依賴環）。**P1 3**：③ G-03 計數低估（VG-01/02/03/04/06/10/12/13）④ VG-08 混合移除+再生成兩軸 ⑤ 移除步驟缺故障注入；VG-06/VG-07 scope 拒絕時機 —— 均已按 R22 修訂（capture/reset 改 supervisor-only、卡只 verify；generated_key_name producer/reader 測試移至 VG-14；EX-VG-01 擴及 11 卡並補判據規範；VG-08 拆出 VG-14；移除四步故障注入；scope 改「任何讀取/輸出前」） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r22.log` |
| R23 | Claude | R22 修订版 | `FAIL` | **P0 1**（`os.makedirs` 在 verify 模式也建 supervisor 目录）**P1 4**（VG-04 误标 EX；VG-08 描述与 Out of scope 矛盾；VG-08 G9–G18 与 VG-14 重复；VG-08 AC 计数错）—— 均已修（makedirs 移入 capture；VG-04 恢复 N/A；VG-08 描述改「移除与生成回落」并删除重生成门；VG-08=8/8 N/A、VG-14=12/8@EX；waiver 收敛为 VG-05/07/11/14） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r23.log` |
| R23 | Codex | R22 修订版 | `FAIL` | P0 0；P1 6：① VG-08/VG-14 重生成重复（含描述/门/证据）② VG-14 未接入 DEP-VG-02/Phase 3/M3/DEP-VG-01 ③ VG-14 缺 ER-06a + 网站门 ④ EX 范围/审计不一致 ⑤ VG-08 缺四步移除故障门 ⑥ VG-02 harness 要求成功 import（持久化属 VG-03）⑦ sentinel 暂存目录权限/清理 —— 均已按 R23 修订（VG-08 仅移除+四门 G9-G12、12/8@EX；重生成全归 VG-14 并接入 DEP/Phase/M3/ER-06a/网站门；EX 收敛 VG-05/07/08/11/14 且 15 卡 Granularity 与审计逐卡对齐；VG-02 改失败路径-only、成功 sentinel 移 VG-03；三 harness 改 `mktemp -d` + `umask 077` + trap 清理） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r23.log` |
| R24 | Claude | R23 修订版 | `FAIL` | P0 0；P1 6（VG-08 重复门/计数/描述/AC、审计不同步、`LIBRA_TEST_UNPROTECT_ONLY` 未定义）—— 其中 VG-08 五项已在 R24 重写中解决；未定义 env var 已删除；VG-08 以 12 个具名门（G1–G12）计，故保留 12/8@EX | P2 0 | `/tmp/issue-gpg/review/claude-plan-r24.log` |
| R24 | Codex | R23 修订版 | `FAIL` | **P0 1**（VG-08 仍含再生文字/重复 G9-G18，与 out-of-scope 矛盾）**P1 3**（VG-14 未入全域 DEP-VG-02 边/Phase 3「三卡」/独立序/M3；三 harness trap 覆盖与 umask 顺序；非豁免卡仍标 @EX）**P2 1**（Review log 时序）—— 均已按 R24 修订（VG-08 整段重写为纯移除轴 G1-G12；VG-14 全面接线；harness 先 umask 后建目录且单一 trap；非豁免卡去 @EX；Review log 按轮次排序） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r24.log` |
| R25 | Codex | R24 修订版 | `FAIL` | **P0 1**（VG-14 在 VG-09 后的空窗：既有 generate 覆盖 bug 可伤害 imported 用户）**P1 5**（评审 prompt 卡清单漏 VG-14；VG-02 env hook；VG-13 引用移除测试；VG-07/VG-09 缺锚点；测试矩阵与门数不符）—— 均已按 R25 修订（VG-03 加空窗 fail-closed 守卫 + 测试；prompt 补 VG-14/四 patch；VG-13 G5 改 replace 范围；VG-07/VG-09 补 file:line 锚点；矩阵按 G 门数重对） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r25.log` |
| R25 | Claude | R24 修订版 | `FAIL` | P0 0；P1 2：① M3 写「三个 patch」而 Phase 3 为四卡 ② 三 harness 两次 umask + trap 在脚本中途 —— 均已按 R25 修订（M3 改四个 patch；harness 顶部单次 umask + 建齐目录后立即单一 EXIT trap，去冗余 chmod/umask） | P2 0 | `/tmp/issue-gpg/review/claude-plan-r25.log` |
| R26 | Codex | R25 修订版 | `FAIL` | **P0 1**（VG-02 harness 对不存在捕获档 rg → exit 2）**P1 3**（独立发布链/风险/追溯未含 VG-14；VG-03 计 8/8 与矩阵 3 新；矩阵逐行门数不符；VG-00/VG-09 缺 ER-06a N/A）**P2 1**（锚点无行号）—— 均已按 R26 修订（VG-02 改 CAPTURES；VG-14 入链/风险/追溯/GAP 表；VG-03 8/8 与矩阵；矩阵逐行重对；VG-00/VG-09 补 ER-06a N/A 与 Docs 理由；锚点补 `install.sh:21`/`release.yml:5`） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r26.log` |
| R26 | Claude | R25 修订版 | `FAIL` | P0 0；P1 1（VG-03 审计 AC=7/8 与卡内 8 条不符）——已在 R26 修订中改为 8/8（评的是快照，当前文本已合）；其余 15 项全数核实通过 | P2 0 | `/tmp/issue-gpg/review/claude-plan-r26.log` |
| R27 | Codex | R26 修订版 | `FAIL` | P0 0；P1 1（VG-11 G8 未入 Verification、G10 名称冲突、矩阵未明示 8+1）——与 Claude R27 同一项，已修（补 G8、统一 `primary_key_signs_when_no_usable_signing_subkey`、矩阵改 vault 8 门 + config G11） | P2 0 | `/tmp/issue-gpg/review/codex-plan-r27.log` |
| R27 | Claude | R26 修订版 | `FAIL` | P0 0；P1 1（VG-11 矩阵写 8 门/G8 缺失/G10 名称冲突）——已修（Verification 补 G8 并统一 G10 名称；矩阵改 vault 8 门 + config G11）；其余 R26 双侧全数核实通过 | P2 0 | `/tmp/issue-gpg/review/claude-plan-r27.log` |
| R28 | Codex | R27 修订版 | `FAIL` | P0 0；P1 1（修订历史缺 R27 列）P2 1（R28 占位列先于 R27 Claude）——均为文书秩序，已修；技术面明确复核完整 | — | `/tmp/issue-gpg/review/codex-plan-r28.log` |
| R28 | Claude | R27 修订版 | **PASS** | P0 0；P1 0（R27 三项已修复，历史验证点全数保持）；P2 0——首个 PASS | — | `/tmp/issue-gpg/review/claude-plan-r28.log` |
| R29 | Codex | R28 修订版 | **PASS** | P0 0 / P1 0 / P2 0——技术面与文书面均通过 | — | `/tmp/issue-gpg/review/codex-plan-r29.log` |
| R29 | Claude | R28 修订版 | **PASS** | P0 0 / P1 0 / P2 0——与 Codex R29 同版双 PASS，评审收口 | — | `/tmp/issue-gpg/review/claude-plan-r29.log` |

## 非目标与延后项

| ID | 项 | 状态 | 重启条件 |
|---|---|---|---|
| DEFER-VG-01 | 硬件令牌/智能卡直签 | 尚未排期 | 用户硬件密钥需求 |
| DEFER-VG-02 | SSH 签名（`gpg.format=ssh`） | 尚未排期 | Git 兼容优先级提升 |
| DEFER-VG-03 | 多活动密钥/`-u` 选码 | 尚未排期 | 多身份签名需求 |
| DEFER-VG-04 | keyserver/WKD 导入 | 尚未排期 | 网络信任模型就绪 |
| DEFER-VG-05 | 密钥备份/恢复自动化 | 尚未排期 | 跨机器迁移需求 |
| DEFER-VG-06 | 第三方未导入公钥验证 | 永久非目标 | 全新信任模型立项 |
| DEFER-VG-07 | passphrase 缓存/钥匙串 | 尚未排期 | 重复导入成本抱怨 |
| DEFER-VG-08 | 哈希算法协商 | 尚未排期 | 算法协商需求 |

## 完成判据

- [ ] 所有非延后任务 `Lifecycle=done` 且 `Acceptance=complete`（家族子卡以 VG-09 完成为前提）。
- [ ] 收口门三连全绿（终局修复后重跑）：`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2`。
- [ ] REL-VG-01 证据齐备（annotated tag `--verify-tag`、run/job/headSha、CDN、安装冒烟）。
- [ ] sentinel/零泄漏、allowlist 守衛、失败注入与错误口令零写入证据归档。
- [ ] 兼容与文档收口清单全勾选；`plan-long.md` 索引更新为已完成。
- [ ] 延后项按重启条件登记，未被静默实现或跳过。
