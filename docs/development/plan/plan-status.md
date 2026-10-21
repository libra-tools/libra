# 计划执行状况总表（plan-status.md）

> **Worktree 默认目录统一（2026-10-19）：** 用户已授权实施 [WD-01..08 与 WD-02G](plan-20261019-worktree-layout.md)，新建目录使用 `.libra/worktree`，不迁移旧数据。旧源码 `cc8f437` 的原生 Windows 与整套 CI 8/8 通过；2026-10-20 未提交修复树的 macFUSE/OrbStack Linux 真挂载各 16/16、`cargo test --all` 10533/0/26 ignored、nextest 10521/10521（18 slow、4 leaky、23 skipped）、严格 Clippy 与 Claude Code PASS。2026-10-21 已合流至 `origin/main` `3471175`；用户要求在上游 `0.30.77` 上 patch + 1 发布，当前目标为 **`0.30.78`**，仍不发布 `0.31.1`。`v0.30.67` 起的二进制支持 2026101902 receipt，`0.30.66` 及更早版本拒绝已升级仓库。最新合流树的 nextest 10664/10664 passed（20 slow、3 leaky、23 skipped）；最终 cargo 全量重跑、远端 CI、发行与网站新布局验收待做，九卡正式验收仍为 **0/9**。证据见计划正文。

> **plan-20261015 现场（2026-10-19 15:30:59 UTC）：** Mega2 的 G-13 修复聚合已退出；FIX-MTR-03 和 FIX-MTR-01 各自发布，FIX-MTR-04 不发布。旧 B8 FAIL、8/8 STOP 与 FIX-MTR-03 AC-1/VER-5 未勾选保持原记录。Mega2 MTR-87 已完成 `v0.42.55` 的实际 Docker D；MTR-10 在工作区实施中，四条 focused VER 已在 Docker 测试服务下通过，完整 C/双审/发布仍待。Libra handoff `976091c` 已是远端 main 祖先；本地 17033b9、5e55635 两笔计划文档历史在本次合并中保留。LTR-00 仍 in-progress/locally-accepted，formal 0/26，LTR-01 未开工。本次整合本身不计功能验收、版本发布或新 D。

> **plan-20261015 历史快照（2026-10-16 04:11:08 UTC）：** 当时本地 main 位于 `deabb37`、Mega2 main 位于 `b784111`；FIX-MTR-03 恢复窗已执行而 AC-1 未勾选。当时 `0.30.50` 的本机验收不外推至目前上游版本。

> **当前 thinking 数据获取基线（2026-10-18 16:23:55 UTC，用户明确授权）：** [DATA-CAP-20261019](plan-20260904.md#thinking-data-baseline-20261019) 替代下方 2026-10-18 四 provider 计划的隐私剥离/native 来源保存前置。以 OpenCode macOS 13段/4617 bytes、2个自动 checkpoint 原值一致为已证基线；系统 OpenCode 默认 Opus 5.5 medium + summarized 的无 --model 实测另得到131 bytes正文。Codex CX-00 已于2026-10-19在 main 开工并完成本地source审计（Claude R2 PASS，首轮FAIL保留并已修复），CX-12亦已本地验收并经Claude单审PASS，其余27张CX及Claude 9/Pi 11/Hermes 10卡仍pending（正式complete仍0/59）；2026-10-18只修订计划和配置事实；获取的数据可普通保留并标 Unproven/Partial，不把空壳当通过。Codex source 审计与隔离hash-list已执行，四工具真实模型smoke均未执行；用户本次已授权Codex计划实施，没有native grant。Codex CX-00/CX-12 source-qualified、当前下一卡CX-15；另三工具下一卡CC-00 / PI-00 / HM-00；实现依原逐文件串行与 schema/cloud 数据正确性边界。旧 PASS/FAIL 与完成数保留历史；2026-10-18 16:39:27 UTC 本轮 fmt/strict、104/104 focused 与 Claude R2 实际 PASS 已登记于各计划；源数据获取/可读正文仍须未来真实测试，无版本发布。

> **Grok 当前覆盖状态（2026-10-19 11:12:48 UTC）：** 功能采集暂停；GK-06架构修复locally-accepted/空，125项focused、35项Codex本机及矩阵、全量10325/10325 PASS。GK-07交接给`/root/grok_continue`，GK-08生产接入仍暂停；不提交/推送/发布。以下为先前交付门历史，不代表已获取thinking。

> **Grok 采集计划历史（2026-10-19）：** [plan-20261018-grok-capture.md](plan-20261018-grok-capture.md) 的 GK-00 为 `locally-accepted`，Acceptance 仍空，等 GK-01 的 C/D。GK-01 为 `locally-accepted`，Acceptance 仍空，等 v0.30.53 的 D-CAP-STD。GK-02 为 `locally-accepted`，Acceptance 仍空，等 v0.30.55 的 D-CAP-STD。GK-03 为 `locally-accepted`，Acceptance 仍空，等 v0.30.57 的 D-CAP-STD。GK-04 为 `locally-accepted`，Acceptance 仍空，等 v0.30.59 的 D-CAP-STD。GK-05 为 `locally-accepted`，Acceptance 仍空。v0.30.62 的 Check, Build and Test run 39218634651 失败：`compat-offline-core` 在 nextest 过滤式 `not binary(~^grok_)` 上报 no binary names matched（`~` 是包含匹配，不是正则）。CodeQL Advanced run 39218634806 与 Build and Release to R2 run 39218736253 成功。前滚 `6b089c4` / v0.30.63：CodeQL Advanced run 39226039130 成功，Build and Release to R2 run 39226204632 成功。Check run 39226039150 被后继 main `b57fdb6` 取消，`compat-offline-core` 未完成，不计通过。Acceptance 仍空。网站 `cf` `27a83d1` 已推，Claude 审查为 VERDICT PASS 且无未关闭 P0/P1。GK-01..GK-04 的 base.yml 与 CodeQL Advanced 仍是 cancelled；在当前 main Check 占用并发组时重跑会取消它，故尚未重跑。公开 grok-build 仍是 1.0.45，该事实不再停工。

> **USER-OG-THINKING-RETAIN-20261018（2026-10-18 07:36:52 UTC，工作区已实现，未提交、未发布）：** 用户要求把隐私要求降到能保存 thinking，并写入未证明字段。助手 reasoning 的字符串 `text` 与未证明 `state`、`time`、`originProof` 及顶层 `signature`、`redactedData`、`reasoningEncryptedContent`、`thoughtSignature` 进入普通 transcript。来源仍是 `Unproven` / `Partial`，不归档为 native artifact。本条只替代方案 C 当时的普通副本删除规则，不新增任务卡，不增加完成分子。历史 import 仍 `LBR-AGENT-020`。已发布 0.30.52 不含此行为。完整边界与本次测试见 [USER-OG-THINKING-RETAIN-20261018](plan-20260902.md#user-og-thinking-retain-20261018)。

> **四 provider 计划复审（2026-10-18 02:00:57 UTC，历史规划状态，Codex当前执行见上）：** 以 OpenCode 方案 C 的 ordinary 采集、已交付 ACF/SCAP/RG/COV2 为基线，Codex 29 张未完成 CX 卡、Claude 9 卡、Pi 11 卡、Hermes 10 卡全部仍为 pending（规划审查不增加实现完成数）。本轮 Codex 自审PASS；Claude Code R4及最终R5对四份及总评均实际`VERDICT: PASS`，root已采纳（各计划review log登记实际raw/report SHA及七文件输入manifest）。旧PASS/429保留原时点历史。下一候选分别为 CX-00 / CC-00 / PI-00 / HM-00 的只读 source 审计；实现须先选卡授权、消费具体 GO 产物并完成 shared I 逐文件交接。fmt/strict clippy、最终97守卫及双env完整nextest10262/10262通过（12slow、1leaky、23skip），详细证据见各计划；不记未来卡验收完成。本次仅计划文档no-release，不bump/tag；本提交Actions在Push后查询并于下一次正常状态同步记账，不预填成功。Hermes 新计划见 [plan-20261018-hermes-capture.md](plan-20261018-hermes-capture.md)。


> **现行最终范围：方案 C（USER-OG-FINAL-C-20261017；用户明确决定，随本次单 Claude PASS 与签名 main 交付采纳）。** 保留普通 OpenCode 采集、安全 ordinary 投影和字段审计；`signature`、`redactedData`、`reasoningEncryptedContent` 来源未获证明时保持 partial/不归档。本计划取消可信 native 启动补强、当前 `.3` producer 准入及三字段 native 归档的交付承诺，**不是延期等待，也不是 Source A PASS**。六张未完成卡撤出本轮范围，未修复/失败/UNRUN 原据保留；不把撤销计为完成。原始库存74卡，撤销6后范围库存68卡：正式 **66/68=97.06%**、source **66/68=97.06%**；其中 OG-15、FIX-OG-CP-01 仍维持此前用户延期/后验决定，本次不改，当前可执行 macOS 范围 **66/66=100%**，本轮任务卡及最终计划全量已全部通过。COV **26/26** 不变。OG-08、OG-06、OG-25、OG-26 与 FAST-OG-04 已继承实际 v0.30.52 / F `bc77500b8814c2f9f37ae29ba82ff43eff68055d` 的安全 CI、四平台8/8发布、稳定签名清单/CDN/Homebrew、网站及指定 curl 本机安装 C/D；Source A/三字段归档仍未获证明或启用。完整撤销表、现行依赖与完成定义见[方案 C 最终契约](plan-20260902.md#ordinary-final-c-20261017)。以下早期带“当前/下一张/延期/native 必须完成”的文字均为原时点历史；有冲突时以本契约为准。


> **上游 e6a420cf 发布阻断窄修正（2026-10-18；root按既有推荐方案授权执行，非新增本计划任务卡）：** 唯一Claude incoming R1实际FAIL rawddd8881b1c1f63cadb6172aa27fb8a6839986dff15cddb73d04156444482a399，52/52完整Read：P2为count-objects大小格式与Git不一致、machine alternates误含C显示引号。附带生产expect按AGENTS规范一并移除。修正属于已落地upstream交付前原CO-01/02实现与文档的前滚，不改变本计划native撤销/66张macOS范围，也不将issue483四卡记complete。生产仅src/command/count_objects.rs；测试仅其既有lib模块与tests/command/count_objects_test.rs；行为文档仅count-objects的EN/zh/dev与COMPATIBILITY对应说明，ER06a由cf现9项同步路径承载；三plan只记录本事实。Scope axis为Git字节格式/人类与机器路径分离，兼做同域低风险truncated/packcount/garbage reason和flags/成功出口文字纠错；不改shared storage、中心CLI分类、installer、native/issuer/当前3、通用quotepath，不实施N+1fanout缓存或previewparser框架。先实际RED（已具名大小门1/1真实FAIL：1.51对1.50）、最小forward-fix、实际GREEN/相关既有回归与fmtstrict，再唯一Claude复审PASS/根事实校核及signed-DCO main/cf交付。发布point自有I7不获业务源码写权；只消费本项另行签名已审终稿与实际最新上游。新be最终完整全量、同F CI/四平台/manifest/CDN/Brew/网站/指定curl仍必须真实通过；旧949 full不替代。其余P3同难度复杂项按用户既有策略保留后续、未修复非PASS。formal61/68、macOS61/66、source65/68、COV26/26与SourceA未证不增。


> **上游实际整合与重验（2026-10-18，最终Source尚待）：** 签推前fresh origin/main已前进至be158f57b389ff952015756c266dd605483583bc，包含e6a420cfb6bba4c66bec2bafd7e424a3798d2893的count-objects只读清点与七类preview不发布operation快照，共38路径（6新增），不属纯版本变动。旧949+候选52的10229/10229全量、2slow/23skip/0retry/0LEAK和97,389,216byte临时构建均为真实历史PASS，但不作为新be最终源码验收。旧publisher在add/commit/tag前由远端不一致门停下；七项自有改动已保存，Libra fast-forward至be，用户04/05原字节、八stash与untracked均保留，未给其他Session发消息。当前候选仍patch0.30.52（实际latest051、52tag未见）；重新同步网站当前CLI、对incoming全部delta做单Claude审查、运行最终be+bump的macOS相关门/完整全量，再签推/发布。COV26/26、source65/68、formal61/68、macOS61/66不增，SourceA未证、三字段零native归档及六withdrawn/两原延期不变。新count-objects/preview源只作为实际最新上游immutable inclusion，不宣称issue483四卡已完成或借其旧计划pending状态取消当前源码验证。


> **2026-10-17 macOS native boot 实际验收：** `FIX-OG-NATIVE-BOOT-01` 的 VER1/3/4/5/6/7/8 已有实际 PASS；本次完成 VER6、VER1、VER8。**AC2 已由最终候选的真实反例证明 FAIL，完整 VER2 harness 仍 UNRUN**，本卡未完成、未 source-qualified，不能据此放行 SRC029、OG24.3、RUN029 或 native 归档。新问题按用户既有复杂问题延期策略登记为 `DEFER-OG-NATIVE-INITIAL-ENV-01`，保留原 AC2/VER2 要求及来源 A，未将延期或未审变量行记 PASS。正式 **61/74=82.43%**、source **63/74=85.14%**、COV **26/26** 均不变；先核对可独立执行的后续卡及发布门，依赖此来源证明的卡继续等待。下方旧“下一张/待”表述保留原时点含义，以本次实际状态为准。


> **private native 启动保护 G09（2026-10-16 22:16:58 UTC；本次签名 main 交付后生效）：** 唯一 Claude Code R2 规划 PASS，raw `2c0a16b9353ed3c2be901f284b1acfdd1eeef4f88682df0944acc3fd986e58db`，root已逐字恢复13/13 required chunks及7/7原始输入并采用推荐方案。新增 `FIX-OG-NATIVE-BOOT-01`，只在新 private successor 实现单二进制 first trusted preload + macOS Hardened Runtime + sealed boot factory prerequisite；本卡不改 Libra 命令、不安装/登记该候选。父分母 **74**，正式 **61/74=82.43%**、source **63/74=85.14%**，COV **26/26** 不变；完成分子没有增加。原73卡统计/全部失败保留历史。**下一张 FIX-OG-NATIVE-BOOT-01，随后 SRC029 → OG-24 `.3` → RUN029 → OG-27。** SOURCE/profile/SourceA/native archive及ROOT19/cf grant均未放行；FIX-RG-SCOPED-04仍为用户指定计划外延期。下方旧“当前/待”段落按原时点保存，以本节及本次具名G09为准。

> **上次收口提交实际 CI：** main `7e75d4138fe452d9d9b6c92f34e98ffc26fbf950` 的 run `38957783497` 已 completed/success，**8/8 jobs success**；CodeQL `38957783597` 2/2、quality `38957783232` 均 actual success。本次规划提交自己的Actions按模板v2.14在Push后查询并于下次常规状态同步记账，不预填成功、不制造纯记账无限链。本轮不发版，已发布0.30.51和其全部实际验收保持原状。

### Native boot G09 当前交接镜像

| 顺序/DEP/DAG | 单写者/发布/恢复 | 追溯/验收矩阵 | 里程碑/风险/状态 |
|---|---|---|---|
| FIX-OG-NATIVE-LAUNCH-01 actual complete → FIX-OG-NATIVE-BOOT-01.source → DEP-OG-12-SRC029 → OG24.3.source → RUN029 → OG27；incoming SOURCEBASE/PROBE及LAUNCH-SHARED逐项登记 | root只写外部private successor12既有+1新mjs和三plan；ROOT19/cf lease withheld。REL-FAST-OG-04 strategy child，source signed handoff不单独bump，actual family C/D仅在finalmain OG24.3 approved candidate绑定＋三plan selected69 manifest/postsignSHA carrier齐全才complete；逆序FAST04→下游→SRC→BOOT，未发布bundle revert，已发布/注册只禁用并签名前滚 | AC1–8/VER1–8：first preload、exact15、path posture、freshTMP、sealed factory、autoload off、最小HR/post-SIP DYLD、actual Required export；SOURCE S1–S8另验。selected69/source8598仅预期，剩余8585 entries exact；真实模型/Session/installed profile测试仅macOS本机 | M-NATIVE-BOOT / RISK-NATIVE-EARLY-LOAD：pending/empty。pre-JS变量清单、effective cachedTMP、最终候选签名/FFI/negative controls不能被toy替代；same-user恶意控制不在既有信任界内。formal61/source63/total74，COV26/26不变 |

> **0.30.51 启动隔离修复实际收口（本次签名 main 交付生效）：** `FIX-OG-NATIVE-LAUNCH-01` 为 `done/complete`；父计划正式 **61/73=83.56%**、source **63/73=86.30%**，COV **26/26** 不变。已发布 F `4401543f1d2eeb6a29e59e9fcd026b130f5c8a44`、signed tag `9250a383d1616aa3a1108a6d2a43c17ad89b4341`、实际 release run `38951286768` **8/8 success**、稳定 Release `415307054`、签名清单/六 artifacts/四 CDN/Homebrew、cf `8c6e8f1ee22aaafeb54df14f635c5e11199d13ee` 部署及双语线上文案、指定 curl 安装和原 `.2` fresh-owned epoch0→1→2 均有实际证据。本次只收口该修复，后置源码/共享 I 返还随本次签推生效；无 OG24.3 grant、无 current3 SOURCE/SourceA/native archive GO。**下一项 SRC029，随后 OG-24 `.3`；Linux 功能未运行，由用户后验。** 下方旧“未发布/待 Push/当前”等段落保留原时点历史，以本次[实际 C/D 收据](plan-20260902.md#native-attest051-actual-cd)为准。

> **native attest 隔离修复本机验收已过（0.30.51 未发布）：** 当前最终工作树 native-origin40、架构64/文档33/Hook12/版本2、nightlyfmt、all-targets/all-features `-D warnings`、双envfull均实际exit0；full **10223/10223 passed，13slow/1flaky/1leaky，23skip**，raw `67ac0a3ab4cc60c872dc755aa4764d90a407aff48a79e85cc96eb74f6a19fd1c`，strict raw `0da2d4e9a61b2484bfcef7aa3c7cbdee917f723567b3b8d2ff7f9dfc577453e1`。2051 tracked工作树输入前后SHA/mode相同；用户04/05原改动保留、不stage，明确guard消费的原/现DEP文本等价，不能把dirty文档冒称发布树同字节。唯一Claude完整实现/文档/版本/状态链R1/R2/R3 PASS（actual29+16+5完整Read；最终补证raw `e7103750a4529c47108dab36559ac6368725bed4030a34a9aec071ea232d9447`），root已裁决。网站最终11tests/typecheck/build/双语local200均过，signed+DCO cf `8c6e8f1ee22aaafeb54df14f635c5e11199d13ee` 已准备、尚未Push/部署；本次main/cf Push、当前F CI、tag/四平台/稳定发布/清单/CDN/安装和C-D仍待。正式 **60/73=82.19%**、source **62/73=84.93%**、COV26/26不增；无current3 SOURCE/native/OG24.3 grant。以下此前“待重验/待补审”段保留为历史，本机验收不等于卡done。


> **native attest 隔离修复实施中（0.30.51 待发布）：** 规划已由 signed main `9071ad0e7f978e9c85f0e4a7bcbb399484e2b2b0` 采纳。首轮本地 native-origin 40、架构64/文档33/Hook12均实际通过；唯一Claude实现审查PASS raw `04997d05f8e278437ec46d8732de447babe9e962333c883fc0861d1becf5e7d6`，root恢复29/29完整Read（报告30为计数笔误）。其P3测试/取消措辞已补齐，当前最终源码待重验/补审；旧Clippy为该补改主动停止，不记PASS。网站首轮11文档tests/typecheck/build和双语本地预览200已过，最终同步/签推/实际部署仍待。上游稳定050、051 tag查询404后仅选定patch051，未发布；本卡独立full/最终strict/签名main-cf/四平台/发布及安装全部仍待，正式60/73=82.19%、source62/73=84.93%、COV26/26不增；无SOURCE/native/OG24.3授权。


> **native attest 启动隔离修复已采纳（本次签名 main 交付生效）：** 唯一 Claude Code 规划 R5 PASS，raw `03b328ec691520c68b53bb9885d1a6fa6b3c83e56d1df98046573c9f631be167`；root已核对3/3完整Read及R3/R4复用链并采用推荐方案。新增 `FIX-OG-NATIVE-LAUNCH-01`，父计划73卡，正式 **60/73=82.19%**、源码 **62/73=84.93%**，COV26/26完成不变；这是必要启动安全修复，不是新卡完成。原73采纳前72卡及全部FAIL/UNRUN历史保留。修复source/code/release尚待；原SOURCE/真实Bun/registered lane/SourceA/OG24.3/RUN029硬门保留，下一张先执行本修复卡。




### native attest 启动修复交接镜像（随本次签名规划采纳）

| 顺序/DEP/DAG | 写集/发布/共享slot | 追溯/验证矩阵 | 里程碑/风险/状态 |
|---|---|---|---|
| DEP-OG-NATIVE-LAUNCH-BASE→FIX-OG-NATIVE-LAUNCH-01.source-qualified→SRC029只读；FIX.release-closeout→shared-return→OG24.3 grant→RUN029→OG27 | 仅父计划attest.rs新writer、5docs/docs-contract/三plan/版本面及cf单页；独立patch全C-D/所有shared写入/postSHA登记先于OG24grant，非FAST04/COV成员；无新并发写权 | exact15/owned0700canonical空TMP/创建验证spawn及受控终态/cleanup失败/成功未reapleader同组回收；VER1–4+full/Claude/站点/四平台；真实Bun和registered lane仍阻SRC | M-NATIVE-LAUNCH / RISK-NATIVE-ATTEST-TMP-REUSE；**done/complete（本次收口签推生效）**：F440/cf8c6、实际051全部C-D与本机原`.2`登记/撤权已闭；post-image `0be80d2ca58653f7a693034eb791438b14ccd54291db6f83f507aabf5583dc0e`/0644，本次signed-return释放本卡writer。父73/formal61/source63，COV26/26不变；未授OG24.3 grant，SRC029真实Bun及registered every-launch/SourceA仍未放行。 |

> **SRC029 最新审查与窄修订（2026-10-16；修订须本次唯一Claude PASS、root裁决及signed main采纳后生效）：** 范围68已由main `f29685679e288b283d5d56f0328a27914650b4d2`实际采纳。R3静态完整Read审查297/297、sole Claude PASS（raw `88215a808d52d2a88a85fbd0d96eb1e19c10bcfd10968b3df76af88454e985de`），root已独立恢复全部68文件内容并核8597同一；随后50/50完整Read补审确认四旧测试断言保留，但Anthropic缺少block stop的两项负例真实FAIL，窄范围Claude结论FAIL/P2 blocking（raw `a652264cbac692f7005c76397d02556121b49357c16213e6f5199016611557c8`）。该新失败不倒改先前静态PASS；source/profile/native总门仍UNPROVEN。仅拟授新private successor内两个既选路径的最小修复，见父计划[明确原生结束修订](plan-20260902.md#native029-explicit-completion-scope)。父formal **60/72=83.33%**、source **62/72=86.11%**、COV **26/26**均不增加。

> **上一轮D组实际补记（2026-10-16 07:00:25 UTC）：** 收口main `75d1734bf789fd1e6f1bec00b36a095389c7e630` 的主run `38855895311` 为superseded **cancelled**，不是PASS；其CodeQL `38855895283`成功。当前signed main `f29685679e288b283d5d56f0328a27914650b4d2` 主run `38858852998` **8/8 success**、CodeQL `38858849030` **2/2 success**，quality `38858849028`及cancel策略 `38858849014`成功；2048输入相对已发布F仅三plan变化，其余2045 bytes/modes同一。本次按预登记规则补齐上一轮终止链结果，不把新维护提交的Actions预记成功，不建立纯记账提交的无限链。下方有日期或“本次待采纳”的旧段落保留原时点历史；不撤回已完成COV或误授current3。

> **SRC029 候选范围修订（本次唯一Claude规划PASS、root裁决及签名main交付后生效）：** 以现有R2未批准候选替换R1作为来源审查输入：selected68=42production/build/generated+23tests/fixtures+3README，声明source8597（8540regular+57symlink）、nonREADME8594；未批准任何binary/profile/native来源。R1的66/8595/8592与26d914、旧R2静态PASS及失败均保留历史。只准在新private successor修改三README的版本/范围/历史与已证基线说明；现有5个生产/build delta按固定SHA纳入待审输入，不能继续改TS。S1–S8全部门、1705native closure、149transform、ROOT19/网站未来lease、OG24原6AC8VER及RUN029真实模型门保留；S6仅已证SDK11子门PASS。父formal60/72=83.33%、source62/72=86.11%，COV26/26卡完成均不变，计划级末端D后补仍待；本变更不新增任务卡、不发版、不改全局OpenCode。

> **按本次收口签名 main 交付生效：** COV2 26/26 `done/complete`，父 FIX-OG-OBSERVER-RECIPE-01 继承实际 C/D 后，父正式 **60/72=83.33%**、source **62/72=86.11%**。v0.30.50 immutable F `deabb37b70d74949a59c3722ad5cee1d8d79145a` 的最终full/CODE/当前CI/四平台/manifest/CDN/Homebrew/网站/指定curl安装/原`.2`epoch0→2均已有真实证据；DEP-OG-13=complete。当前`.3`/RUN029/OG27..29/05/FAST04/SourceA仍未放行；Linux功能未运行。
>
> 以下有日期的旧开工、UNRUN、FAIL、旧计数及旧“当前/待”保留为历史；本次只写三plan事实，不改已发布F/tag/资产或授current3共享grant。收口提交自身D组按GC-COV2-06/模板v2.14预登记给下一次状态同步，未完成的新docs-SHA Actions不冒称PASS。

> **发布前历史快照：最终0.30.50本机验收已过（2026-10-16 03:08:13 UTC）：** R5已实际signed main be05采纳，新双envT2 `Summary [1134.569s] 10219 tests run: 10219 passed (2 slow), 23 skipped` /exit0/raw `f28ad74f0aef5bb220618d175c323786117fa7b7c444bf03afd76ea92ae2d27e`；六manual各1/1/version2/相关质量门及原visible1＋22audit条件映射已核。2048前后同一，full后仅本三plan事实；最终CODE/F/CI/稳定发布/安装/原.2epoch0和C-D待，SourceA/current3未放行。formal59/72=81.94%、source62/72=86.11%、COVsource25/26/formal0不变。

> **R5 范围起草历史快照（2026-10-16 02:13:58 UTC）：** main f6ff 已整合/推送且移机分支已删除；本次私有修订待 sole Claude/签名采纳。旧 release CODE R1/R2 FAIL 保留；新组合 bumped50 full、最终 CODE/F/C-D 未完成。formal59/72、source62/72、COVsource25/26/formal0不变，Source A/current3未放行。

> **R5前历史：最终 repaired50 T2 已通过（2026-10-16T00:51:06.887345+00:00）：** actual 10219/10219 PASS（2slow、23skip；0FAIL/0retry/0flaky/0LEAK），raw `215dc8242692b4da3ea3eaca8c55f9c7ff8ec1248300c2def533e6ad378a555e`，受测2048源前后相同；六manual各1/1、version2、109/fmt/strict及fresh一次visible观察已实际通过。当前main d74范围已采纳、incoming repair189/cff290已source-qualified；最终发布CODE Claude/F/currentCI/四平台/站点新部署/安装/原.2 epoch0和C/D待。父formal59/72=81.94%、source62/72=86.11%，COVsource25/26/formal0不变，SourceA/current3未放行。下方有日期旧快照仅为历史。


> **历史执行快照：当前修复源码已交付、COVREL候选50开工（2026-10-16 00:18:45 UTC）：** FIX-OG-OBSERVER-RECIPE-01 已 actual signed+DCO main `1896475b023c983e4fbb7fca956445c1cc88b950` Push/freshremote/archive2048=finalQA、source CODE Claude PASS/raw293036d0…；cf `f29097a9eea5341b5a775bcbf888eaa6a44666c6` 已部署worker23945ad4-e2db-42e6-8915-349c0640db7d/check116728686156success且EN/zh visible8concept200。父source62/72=86.11%、formal59/72=81.94%，COVsource25/26/formal0。本次G09 I11/R4已由实际signed main `d74f37aecea801b07959b40326f19ad3b8dad4bb` 采纳；8doc已审source-version/status/anchor字节及toolchain生成R4现为候选0.30.50，尚无新50测试/full/F/tag/release/install通过。SourceA/current3未放行。以下旧时点记录保留历史。


> **移机分支整合（2026-10-16）：** 用户已明确授权将 `handoff/scoped-traces-20261016` 整合到 main，并在确认远端 main 包含其全部提交后删除该分支。本次仅整合计划文档，保留 main 的 COV/OG 进展和下方移机快照；旧“不合 main”是当时的 transport 范围，不再限制本次整合。分支 tip 为 `976091c4ca1e42a21450d01118f0430ef8251343`；不因合并新增任务完成数，不放行 Source A/native、Mega2 STOP 或 LTR C/D。

> **FIX-OG-OBSERVER-RECIPE-01 本地验收已过、源码交付待（2026-10-15 23:18:52 UTC）：** 已actual通过原16/16本机登记、37/37 native unit、109/109 hook/docs、fmt严格clippy及修复后49双env T1：`Summary [1118.151s] 10219 tests run: 10219 passed (2 slow, 1 leaky), 23 skipped` / exit0，2048输入前后相同，raw `d7c23044d3d2564702c8b9088c990ed50345776132289e2f2374fdc063664b21`。最终代码Claude、signed main/cf和网站部署仍待；实际全部交付后才source-qualified、父source61→62/72=86.11%，此前61/72。formal59/72=81.94%，COV source25/26/formal0不变；不是卡done。下一COV2-REL-01，最终repaired bumped源码新T2及原installed-F .2 epoch0/完整C-D仍需；SourceA/current3未放行。

> **历史执行快照：当前兼容修复实施（2026-10-15 22:35:29 UTC）：** G09 已由 signed+DCO main `3158302c0eab2994bae571d3268542ec7c0970ab` 实际 Push/freshremote 采纳，FIX-OG-OBSERVER-RECIPE-01 开工。新独立 gold 回归在未修复 cd53 catalog 上实测 FAIL（1/1，raw `e6fc8c2cd22bd47e9b417747550c8e761c0980b82fef0a946ad65c94a4ec7456`）；已合入精确历史 recipe 及 cfg(test) 修复，GREEN/49 T1/单审/源码交付与网站部署待验。不新增完成数：parent formal59/72=81.94%、source61/72=84.72%；COV source25/26=96.15%、formal0/26。默认普通 v2 目录过滤源码原 SHA/mode 不变；下一 COV2-REL-01，旧候选50 full不能覆盖真实修复。Source A/current3/installed-F/C-D 未放行。

> **历史执行快照：当前G09兼容修复候选（由本次signed main采纳生效）：** 新增FIX-OG-OBSERVER-RECIPE-01，parent72卡/formal59/72=81.94%/source61/72=84.72%，不增加完成数；COV仍25/26 source、0/26正式。首次旧候选50 full已actual10216 PASS（2flaky），但历史producer recipe兼容缺口阻止发布冻结；先新修复卡，再COVREL最终源码复测/发布。下面旧时点统计保持历史，不代表当前门已通过。

> **历史开工快照（已由上方G09阻断/后续修复顺序覆盖）：COV2-17已actual交付，COV2-REL-01开工。** main7350099，25/26=96.15%源码就绪，DEP-OG-13 source-ready；formal0家族C/D。当前patch候选0.30.50，首次bumped最终源码全量/本机门尚待；父formal59/71不变。

> **COV2-17 A/B＋Claude PASS已闭合，签推待：** 实际10/10及唯一真实visible-only观察通过；当前source24/26，本提交实际signed交付后才25/26=96.15%及DEP-OG-13 source-ready；formal0家族C/D、父formal59/71不变。下一COV2-REL-01。

> **历史快照（已由COV17实际signed交付7350099覆盖）：COV2-17 10/10＋真实visible-only已通过，单审/交付待。** 仅一次Haiku4.5实际观察；source当前24/26=92.31%，本卡signed交付闭合后才25/26=96.15%；formal0家族C/D、父formal59/71不变。下一COV2-REL-01。

> **历史快照（2026-10-15 21:06 UTC 真实观察前；已由上方结果覆盖）：COV2-17 实际10/10通过，真实观察待。** 前驱20f7787已signed交付，COV source24/26=92.31%；本卡AC12/VER6、单审/交付仍待，formal0家族C/D，父formal59/71不变。

> **COV2-16 A/B已闭合、源码交付待：** actual12/12＋109reader/fmtstrict＋soleClaude PASS/root已完成；source当前23/26，本次signed交付闭合后才24/26=92.31%，formal0家族C-D、父formal59/71保持。下一COV2-17。

> **COV2-11 cfg前滚本机门与单审已闭合：** 原6VER各1/1、cfg单测1/1、两相关模块各37/37、109reader/fmtstrict及soleClaude实际PASS；新SHA Windows CI仍pending。仅签推同一性完成后交付该前滚，COVsource仍23/26。下一卡恢复COV2-16新12matrix，旧legacyFAIL/defer不改。

> **G09 前瞻范围修订（由本次签名提交采纳生效）：** legacy `info.status="stopped"`终态import样例实际FAIL，归DEFER-COV11-IMPORT-TERMINAL-01计划外延期；本轮import功能正例以已审当前格式替代。源码/测试不改，旧FAIL不变；仅实际soleClaude PASS、signed main采纳后新matrix生效；source23/26不增。


> **当前 COV2-11 接线验收准备（2026-10-15）：** main已同步上游 `33a5a8b`，Linux export、按目录隔离及测试锁修复均保留；R6三owner接线已整合。当前source19/26=73.08%、formal0/26，父formal59/71/source61/71。原6VER/新T1full、唯一Claude及网站新镜像交付未完成，不预记20；下张COV2-12。以下旧页首进度为相应历史时点。

> **COV2-10 执行中（2026-10-15）：** COV2-06 已实际signed+DCO main/remote `8eeecca9c6fe8e144db8b17538d666c1e87d8050`，新T1full10157/10157、soleClaudePASS及archive2048同一。当前COVsource18/26=69.23%、formal0/26；父formal59/71/source61/71。按原8AC/4VER准备typed脱敏/零候选明文，不接线生产live/import，不把preview当Source/native授权；下张11须先核必要I/G修订。

> **COV2-06 本地验收闭合、待本次 main 源码交付（2026-10-15）：** 原8AC/4VER满足，sole Claude代码PASS（0P0/P1/P2、5P3），新T1双env full实际 `Summary [1161.926s] 10157 tests run: 10157 passed (2 slow), 23 skipped` /exit0。当前COVsource17/26；最终事实门、signed+DCO main Push/remote闭合后才 **18/26=69.23%**，formal仍0/26；父formal59/71=83.10%/source61/71=85.92%。下张 **COV2-10 typed脱敏与零明文sink**。生产入口仍未激活，不运行Session/exporter/model或放行SourceA/native/家族C-D。以下旧进度为对应历史时点。

> **COV2-06 执行中（2026-10-15）：** COV2-09 已实际 signed+DCO main Push/remote `c5a138445aab2f81f4567b9789241adb25e63eff`，cf `d5824ca`/Cloudflare/EN+zh HTTP已核。当前COV source17/26=65.38%、formal0/26；父formal59/71=83.10%/source61/71=85.92%。本卡仅交付未激活纯解析器，原8AC/4VER不变，新增共享分类契约按T-1跑新full；不调用Session/exporter/model，不先计18。以下历史记录保留。

> **COV2-09 本地及网站验收闭合、待本次 main 源码交付（2026-10-15）：** 原8AC/4VER各实测满足；唯一 Claude 代码 PASS（0P0/P1/P2、6P3），网站单页 Claude PASS（0阻断、2P3），网站已 signed cf `d5824ca86a33307df09286aec21f2158d873feae` / Cloudflare success / EN+zh HTTP200。COV当前 source16/26；本次最终事实门、signed+DCO main Push/remote 同一闭合后才 **17/26=65.38%**，formal仍0/26，父formal59/71=83.10%/source61/71=85.92%。下张 **COV2-06**；不以网站部署或 synthetic reader 测试放行 Source A、native 或家族 C/D。以下旧进度均为对应历史时点。

> **COV2-09 执行中（2026-10-15）：** 前驱 COV2-05 已实际 signed+DCO main Push/remote `b1443c9ba3e8a70b4ae7a6b84a3e2ab052715021`；COV source **16/26=61.54%**、formal0/26，父 formal59/71=83.10%/source61/71=85.92%。本卡按原8AC/4VER补 graph 闭合版本校验与 doctor 隐私回归；backend clean cf/direct交付。尚未把本卡计入17；下列历史“待05交付”已由实际b144收据取代。

> **COV2-05 本地验收完成、待本次源码交付（2026-10-15 09:49:12 UTC）：** 原7AC/4VER及相关质量门已满足，唯一Claude代码评审实际PASS（0P0/P1/P2、8非阻断P3），新T1双env full实际10150/10150 passed（2slow、23skip、0FAIL/retry/flaky/LEAK，exit0）。source交付前15/26；只有本次最终事实复核、signed main Push/remote同一闭合后才16/26=61.54%，formal仍0/26，父formal59/71=83.10%/source61/71=85.92%。下张COV2-09在实际交付后开工；不以本机full放行SourceA/native/家族C-D或Linux后验。以下旧当前/暂停/待验收头部属于各自历史时点。

> **COV2-05 四owner实际采纳后实施中（2026-10-15）：** scope修订已由actualsigned+DCO main/remote `d36aa2bac136f8d80b0d8c781d38a1d861a50d52` 采纳，唯一Claude规划R4实际PASS/raw `494779f84e69722a149bb362b4f388526985507817964df63f15d4c0572f1517`，两非阻断P3由实现明确普通scope内不得调用VerifyReasoningArtifacts及None的原helper_program语义。首次失败及R3FAIL保留；当前已恢复四owner实现/原验收，代码相关QA、代码单审、T1双env full和源码签推尚未完成，仍不计source16。COVsource15/26/formal0，父formal59/71/source61/71；现安装正式49保持，非新CLI/Session/modelCI。以下草案/暂停/待采纳表述为各自历史时点。

> **COV2-05 当前实施状态（2026-10-15）：** 必要三owner写集修订已由实际signed+DCO main/remote `3e824e27eb28f3fe31475b2c1ae20030a3230dfb` 采纳，唯一Claude规划R2实际PASS/raw `094d32b1da65e51c346083c2251ddee94291bc3b8ee8473278ff1941efa8d448`、103reader/fmt/strict实际0；R1 FAIL保留。首次真实DB验收已FAIL，当前暂停前三owner未合格WIP，待第四helper-owner精确修订单审/采纳后再实现；尚无代码QA/代码评审/full/源码交付资格，不计source16。COV仍source15/26、formal0/26，父formal59/71/source61/71；下一卡COV2-09在本卡source-qualified后执行。

> **历史快照（已由页首当前状态取代）：COV2-08 已源码交付 / COV2-05 写集修订草案（2026-10-15 07:18:48 UTC）：** actual含签名头+DCO main/remote `c60b2817b0d7c2b889491f85a9777e0855356fe1` 已交付COV08（源提交9417及上游5522均保留），COV source **15/26=57.69%**、formal **0/26**，父formal **59/71=83.10%** / source **61/71=85.92%**。COV05仍pending/空、实现UNRUN；本次只将必要import binding/typed port的精确I修为3生产owner/3落点、M，原7AC/4VER/26分母与DAG不变。修订须实际规划单Claude PASS、root裁决及signed main Push/remote采纳后才生效；此前不授新增源码写权、不计source16，不以此放行SourceA/家族C-D。


> **COV2-08 上游文档整合（2026-10-15）：** 本卡已实际签名+DCO本地提交 `9417d976cd5ab78137869a2ef174fdf28abc59d7`，九个blob精确匹配受审后像，尚未Push。已实际fetch上游 `5522458235b824b4eb3a7cd7e8d6b8a492717092`；相对58b只有三个计划文档变化，全部Rust/测试/配置/版本及其余2045输入同一。现仅合并双方计划事实并补核最终文档，完整实现full仍绑定R3原输入，不能称新整树跑过full。合并后103reader/fmt/strict、唯一Claude及root裁决/签名merge/Push/remote核对均待实际完成；只有交付完成才source15/26=57.69%，此前14/26，formal0/26、父59formal/61sourceof71不变。Source A与家族C/D不由本次整合放行。


> **COV2-08 原两事实受审输入（2026-10-15，本地9417提交前）：** 原8AC/4VER在R3当前源已实际通过：36个exact各1/1、103reader、fmt/strict及完整双env全量10143/10143、23skip、exit0；唯一Claude静态PASS、0P0/P1/P2。当前只更新两计划的实际结果/勾选/生命周期，七个实现/测试/行为文档后像与受测受审源完全同一；本两事实的新reader QA/单Claude/裁决/签推仍待实际执行，不预填本次SHA。仅当这些交付条件实际完成，COV source由14/26变为15/26=57.69%，此前仍14/26；formal0/26、父59formal/61sourceof71不变。下一卡COV2-05。下列旧UNRUN、FAIL、cancelled与计数保留为各日期历史，SourceA与家族C/D不由本事实关闭。


> **历史 COV2-08 R3 准备状态（2026-10-15）：** R2 的40步相关验收已实际通过（36个exact各1/1、103reader、fmt/strict），但唯一Claude仍FAIL（0P0/P1、1P2）：durable重放底层重认证会丢失暂时错误/deadline分类。已停止R2未完成full后再改源码；其实际7607/10143 passed、1slow、23skip、exit100，余2536未运行，不记完整PASS。R3保留v2重认证的可重试分类、向既有doctor传递其它错误，并在原VER3补durable late deadline真实重放回归；当前新QA/Claude/full/签推均待。此轮先取得Claude静态PASS才启动full。source仍14/26、formal0/26，父59formal/61sourceof71，原8AC/4VER/I/R/DAG/gold与SourceA边界不变。


> **历史 COV2-08 R2 准备状态（2026-10-15）：** R1当前源36个具名exact/103reader/fmt/strict均实际通过，但唯一Claude静态审查为FAIL（0P0/P1、2P2）；因此停止其未完成full后才改源码。R1full实际7049/10143 passed（7046普通PASS+3LEAK）、23skip、exit100，余3094未运行，不记完整通过。现修测试setter的Unix cfg及v2 durable replay暂时错误分类，并扩展原VER3；R2验证/单Claude/新full/签推全部待，不借R1 PASS。当前source仍14/26、formal0/26，父59formal/61sourceof71；原8AC/4VER/I/R/DAG/gold和SourceA边界保持。


> **COV2-08 当前开工（2026-10-15）：** 前驱 COV2-22 已以 signed+DCO main `58b4c02e7ec372e5e9bfec13e108e0e68810c510` 实际 Push exit0、remote 同 SHA，指定 dcc 旧 writer / 当前 d188 完整全量 / 新增事实 reader QA 103 / 最终唯一 Claude PASS及root裁决均已核；COV2-22 source HOLD 已解除。当前 COV source **14/26=53.85%**、formal **0/26**，父 formal **59/71=83.10%**、source **61/71=85.92%**。本次只开工 COV2-08，四项新 VER/8 AC 在当前重基源仍未验收，不预计15。以下旧条件/索引历史由本条和本卡新收据覆盖；Source A、家族 C/D 与 native 同裸key边界不变。


> **状态快照 2026-10-15 05:24:15 UTC — plan-20261015 LTR-00上游整合：** 用户授权本任务root接管窗口（首次核验2026-10-15 03:07:03 UTC）仍有效。原f306审计记录已实际signed+DCO本地提交 `3c3fb183bdb1222de3b1b95f56ae501a17edcb36`，未重写、未push；现由root原生merge整合 `58b4c02e7ec372e5e9bfec13e108e0e68810c510`，本段只解决本任务status相交冲突并保护上游新事实。原H0/ER05 PASS仍仅绑定其原输入，新整合窄A/B已实际核（fmt/32+1/strict/DAG及保护）；整合R2已真实五键合格双PASS；合并signed提交/push待root实际取得；LTR-00仍in-progress/locally-accepted、formal0/26、C/D待LTR-01。f306时点13/HOLD只保留历史，当前COV资格恢复严格采用下方58b新增声明/条件，旧OG24.3/frozen.2/SHARED-RETURN/Source A门不由本次整合放行。

> **历史 WIP 迁移快照（2026-10-15 17:25:06 UTC；非计划发布）：** 仅WIP迁移检查点，目标分支`handoff/scoped-traces-20261016`；不合main、不发版、不宣称计划验收PASS。Mega2受控RED101/目标Elapsed后GREEN1/1、原用例1/1、模块8/8及fmt/strict exit0已取得，但第8次文档B于17:13:38→17:13:42 UTC exit1，原8次预算耗尽/STOP；完整A/B、own ER05、G13及FIX4仍未完成。Libra本地e98d41c与最新FETCH d1824ab未整合，当前ahead1/behind16；LTR-00仍locally-accepted/formal0/26，C/D仍待LTR-01。 详见[日期计划WIP交接段](plan-20261015.md#wip-migration-checkpoint-20261016)；原13:40:20 UTC快照原样保留为历史。


> **状态快照 2026-10-15 13:40:20 UTC — plan-20261015 R3实际双审与本地规范交接：** R3实际H0与LTR00当前治理均已Claude/Codex分别PASS；root核195冻结输入及真实原始终态，FIX5全部19AC/19VER B已真实完成，规范源码已精确signed+DCO本地交接。LTR-00 仍 in-progress/locally-accepted、formal0/26，其余25卡pending，C/D仍由LTR-01承载。R3实际Claude与隔离Codex均十项literal PASS；root已核195冻结输入hash/mode、raw/final/session及真实完成终态，本轮H0与LTR00当前治理资格已通过。Mega2 FIX5规范源码已精确signed+DCO本地交接；FIX2 fresh双审/本地提交及FIX1新child源码交接仍待，FIX3 T3未启动、初始超时根因仍未知。95Mega/26Libra共121卡及原跨仓依赖不变；原5522458的push/13jobs仅证明旧source，本轮三治理文档尚未提交推送。旧C2 FAIL与已结束的120分钟窗口保留，不授重开、服务可用或新C/D。 详细原始收据及root P2处置仅见[日期计划当前R3收据](plan-20261015.md#r3-actual-review-receipt)，本索引不复制SHA/run计数。此前R2 FAIL及dated R3起草/B快照保留原时点。

> **COV2-22 资格恢复的最终事实（2026-10-15，当前两计划事实交付条件）：** 指定 dcc 真实旧 writer、独立 clean-b644 全量及当前 signed main `d188be90c1669ce7a36a185c0668584d96b841de` 的完整 2048 输入验收均已有实际终态。当前新增事实仍须本次唯一 Claude Code PASS、root 裁决和 signed+DCO main Push/remote 同一性；这些条件实际完成后，COV source 恢复 **14/26=53.85%**，此前仍 **13/26=50.00% HOLD**，formal **0/26**。父计划仍 formal **59/71=83.10%**、source **61/71=85.92%**，不提前计 OG24 `.3`。以下旧 HOLD/UNRUN/62及采用候选是各自日期的历史，不改原 8 AC/6 VER、I/R、DAG、Granularity、gold 或 Source A 门。


> **2026-10-15 G09规范采纳事实（由本次签名提交采纳生效）：** 原OG24 `.2` source交接历史保留，当前`.3` profile仍须原SRC029/RUN029及原6AC8VER；本次仅采用既有任务的规划修订，父formal **59/71=83.10%**不变，source由旧62/71暂扣为 **61/71=85.92%**，71/26分母不变。COV当前source **13/26=50.00%**、formal **0/26**，COV22仍HOLD。指定dcc实际old-writer补核现已完成：实际1/1/0ignored、8 raw artifacts与旧reference同一、6 current literals与actual dcc同一，root独立复核收据SHA `31cd4e23b68633ce4f6efa348488067a9444c721d0e1423476f67b2303206315`；该有限固定fixture事实不等于whole-writer/native/source qualification。当前HOLD未闭原因精准为：旧full对04/05 dirty计划输入的适用边界补核、新soleClaude事实复核及root最终裁决尚待；不能仍把dcc采样写成UNRUN，也不提前增加COV22分子。原错误source14计入、R1/R2失败与R6准备期HOLD原因均保留为有日期历史。
>
> **实际验收范围：** 唯一Claude R4规划设计已实际 **PASS（0P0/P1/P2，3项nonblocking P3）**，REPORT SHA `d457f6770188d1fd517e42d597bcc7ac4e8a8218b8b9495210d6c59271eb0fee`、raw SHA `9b3c31fd370244cd82c46c0ac85177d6e179216f847c1b2e0baea1d0c416709b`，只覆盖exact R6三文档设计，不覆盖producer源码/构建/SDKgold/Source A。b644+exact R6独立2044输入已实际完成双env、无native/model opt-in的whole hook12/12、ARCH58/58、docs32/32及5个exact计划读者各1/1（共102 unique，5 exact为重复确认，均0ignored）、fmt和strict all-targets/all-features `-D warnings` exit0；RESULT SHA `b36e9ccce60da0369c8f7e587b867585efae9e1712a36725b51c23e794d94ef6`、strict raw SHA `bec4797042e15f172e1b60f60b88a891facee2792ad52476e77047df928cca8a`，2044前后SHA/bytes/kind/mode全同，冻结manifest SHA `5d191b8a4dcd0a5c17189375abab39546ec3e4052956de8ecad547b75f937bcf`。未跑full、模型/Session或producer；本事实delta本身的唯一Claude、相关reader QA、实际Libra diff与signed main/remote收据仍由root另行真实完成，不预填未知commitSHA/采纳UTC，不把R6 QA称作本新增事实字节的已验收结果。原Source A/三字段UNPROVEN、复杂OpenCode验收不上CI、GC03/GC08、全部任务判据及家族C/D门不变。

> **历史source计数更正（2026-10-15 R6准备期审计快照）：** signed18b旧收据曾将COV22计入source14/26；其GC-COV2-03指定dcc完整old-writer/gold同一性尚待真实补核，root已暂停该卡最终source资格，当前COV source **13/26=50.00%**、formal **0/26**。这是资格审计更正，不撤销旧signedsource事实或变更gold/GC03/AC/VER；COV08可继续focused，补核闭合前不full/sign。父formal **59/71=83.10%**、source **62/71**保持；本G09实际采用当前OG24.3修订时才source→61/71。后续仅真实dcc同一性收据和root裁决后才能事实更新恢复COV22 source，当前不预授。


> **2026-10-15 当前G09规范：OG24 `.3`版本维护。** 父formal **59/71=83.10%**不变；OG24原`.2`真实source收据保留历史，新`.3`要求尚UNRUN，本次profile规范采纳生效后source暂扣 **62→61/71=85.92%**（62/71为此前历史）；COV当前source **13/26=50.00%**、formal **0/26**，71/26分母不变，无新增卡。当前COV incoming固定6685`.2`snapshot；SRC029→OG24.3source→local-nonrelease-Libra ownedRUN029→OG27，同时OG27仍需DEP13 complete。REL共享三planI/R冻结、grant/return及后续C/D唯一写者按文末G09阶段表执行，不允许时间窗代替G06/G10edge。旧049四卡complete、020FAIL/deferred和SourceAUNPROVEN不变。


> **当前049实际C/D与四卡正式收口（2026-10-14 22:28:49 UTC）：** USER-TEMP-REL-049 / 0.30.49 的实际C/D已由root按完整收据 `dad01d952f7ce629354118264a4926b048693bf851258e4cd14beb9853727f8c` 逐项裁决关闭；release source/tag peel为 `04617cd2a3ffbf3a3e68ce4ddfecf94b78bc6574`，release run `38639329379` attempt `2` 的8个job全部success，当前SHA CI/CodeQL、四平台签名manifest/资产及CDN、pinned Homebrew、网站HTTP200/visible六段、用户指定curl home安装、同正式Darwin资产原子替换root目标及installed cat-file/既有BASE1345B读回均有hash绑定的实际证据。 仅四张父卡由locally-accepted转done/complete，父formal **59/71=83.10%**（原55增加4），source仍 **62/71=87.32%**；COV source **13/26=50.00%**、formal **0/26**，71/26分母不变。COV2-25/COV2-REL-01/DEP-OG-13、OG24..26的FAST-OG-04、Source A及native三字段未因此关闭；Source A UNPROVEN，历史unsupported-state import020仍FAIL/默认拒绝/计划外deferred。真实provider idle.error正例仍未观察，本卡typed机制按原7AC/3VER验收，不把成功Session或synthetic称真实错误/native来源。既有FAIL/cancelled/skip、Mega2与cat-file延期观察保留。 下方原日期状态保留为历史；本次仅例行事实记账，不改AC/VER/I/R/DAG或扩大权限。

> **历史BASE源码就绪交接（2026-10-14 20:10:13 UTC；当时正式049/C-D仍待）：** `OG-UPGRADE-029-01`已按8AC/2VER、相关门/fmt/strict、最终单Claude R2 PASS及实际双env全量完成A/B，现`in-progress / locally-accepted`、source-qualified；signed+DCO main `1e2177e11029e28e3c81afbe6ec9f1609cdde7da`与cf `93b10519e1943ea3eea7c943e816418e92b1ca0b`均Push exit0、actual remote同SHA，12root+1site reviewed postimage/mode逐项相同。最终full实际exit0：10133/10133 passed（2slow均最终PASS，23skipped不计PASS，0retry/0flaky），raw `24febb2eba3d2aad09c6f516988e06e9ae127ad8dd96b362fbda4ea45671fdb2`；2043项前后SHA/mode同一。父source **62/71=87.32%**，formal仍 **55/71=77.46%**；COVsource **13/26=50.00%**、formal **0/26**不变。网站已实际deploy0/HTTP200、六段visible text对拍通过；正式0.30.49的最终SHA安全CI/版本guard/单审发布delta/8releasejob/四平台签名manifest/CDN/Homebrew/指定curl安装及installed smoke仍pending；Source A UNPROVEN、历史import020 FAIL/default reject/deferred不变。下方旧开工、未终态full及61/71均保留历史，不倒改失败或未验收门。

> **历史BASE例行验收事实（2026-10-14 19:08:52 UTC；最终门尚待）：** `OG-UPGRADE-029-01`的8AC/2VER已有本机实际证据，Lifecycle仍in-progress / Acceptance空、未source-qualified。新owned自然自动采集普通读回1345B、prompt1、extraction.model=`glm-5.3-flash`（hook顶层仍`unknown`）；input/output/reasoning/cache_read/cache_write为7678/29/35/0/0、total7742；files仅空集合控制。1次模型、0次manual hook/import/repair，raw=false/truncated=false，旧正式0.30.48读回同bytes。官方29 SHA `7f46ba1dad86b730903d9adf54015bb4e706fc1ab6a247affc5a635cd8a663f3`/186170000B；interactive PID50453已退出，collector是同sealed verified binary的独立Required-Seatbelt租约、PID未存档，不是同PID/endpoint证明。645源码输入实际匹配、12旧protected输入和双MANIFEST冻结前缀同一；focused/质量12项exit0及网站typecheck/build exit0，仅证明各自范围。strict已exit0，final full/最终sole-Claude、root与site签推、安全CI/049发布/C-D待；父source61/71=85.92%、formal55/71=77.46%，COV source13/26=50.00%/formal0/26均不增。历史import020仍FAIL/deferred/default reject，Source A UNPROVEN。下列旧开工、失败、UNRUN与1369B均保留为历史，不由本次对拍追认。

> **历史BASE开工与G09实际采纳（2026-10-14 18:27:15 UTC）：** G09前瞻范围修订及仅Unix测试fixture cfg前滚已由signed+DCO main `84bbde7bd6a5f12dee2477755d0a955798518413`实际Push（exit0），remote同SHA；五路径PGP header/DCO和逐blob与最终sole-Claude R2 manifest同一。R2实际PASS（0P0/P1/newP2，六P3经root裁决接受），raw `4ab4911f9253eb659bb03cf6b49fb4479e92a74145ea1ee8f982e1a12df05190`；不是本轮新BASE实现的验收。网站前驱仍为signed cf `a80d3ff9d2d194b739da6148dde3297d325f1a7e`。`OG-UPGRADE-029-01`现in-progress / Acceptance空，8AC/2VER全unchecked、未source-qualified；父source61/71=85.92%、formal55/71=77.46%，COV source13/26=50.00%/formal0/26不增。GLM历史import020计划外延期、仍FAIL/默认拒绝，不记PASS。84bb安全CI仍待实际结论，旧d842 Windows7项dead_code FAIL、首ARCH57/58 FAIL均保留；其它full/正式049/C-D/Source A门未闭。下列旧草案、旧12/26及旧closing只为其原时点历史，当前事实以本条和末尾实际收据为准。


> **历史源码交接（2026-10-14 18:03:50 UTC）：** live修复已signed main `d8422534b07724349591d53aa445ce50c38453f5` / cf `a80d3ff9d2d194b739da6148dde3297d325f1a7e` 并Push、remote同SHA；source **61/71=85.92%**、formal **55/71=77.46%**，COV source13/26/formal0/26。d842 Windows测试编译FAIL已定位仅Unix fixture cfg遗漏；本次窄前滚的本机门/Claude/签推及新SHA Windows仍待，不影响已安装macOS临时版。下述G09草案仅实际sole-Claude PASS和signed采纳后生效，不预授BASE/049通过。


> **历史G09范围修订草案（2026-10-14 17:58:11 UTC，仅实际sole-Claude PASS＋signed main采纳后生效）：** `DEFER-OG29-PARTIAL-IMPORT`把GLM unsupported-state历史import020的helper认证胶囊与direct-only持久化两轴移出本计划和USER-TEMP-REL-049；原FAIL/UNRUN保留且默认拒绝，不记import PASS。BASE仍为8AC/2VER、1生产文件的official29 namespace＋真实自然自动capture/checkpoint/普通export基线。DAG与71卡分母、COV26卡分母不变；本修订不增完成分子，source保持61/71=85.92%、formal55/71=77.46%，COV source13/26=50.00%/formal0/26；BASE及049其它实测门仍待逐项关闭。此前带时间戳的“当前”段落保留为当时历史，涉及historical import为BASE/049硬前驱的旧规范由本G09明确替代，不能倒改旧收据。


> **FIX-OG-PARTIAL-LIVE-01 本机验收与临时交付（2026-10-14 17:44:21 UTC）：** 五生产文件的 storage-only ordinary partial live 修复已完成本机 A/B：六项 new exact 各1/1；最终live15/15、architecture58/58、docs31/31、相关reasoning21（1 ignored）/OpenCode32/snapshot26/checkpoint34/pending31及14项既有ER04 exact通过。nightly fmt/diff/strict all-target/all-feature实际0；sole Claude code R1 PASS（0P0/P1、4P2/7P3，raw `16a23ab014a4304487c165f92670d416cf551870b6f4bd555b6515e856a3831e`）及R2 PASS（0P0/P1/newP2，raw `63791e1e9218f05377e0981692f6da94b9e03f6f0cb3f5014668a86b7b0acdf0`）。R1四P2通过终态/escaped-key机制与准确docs收口，旧P3及R2新P3保留，不声称历次零P2。三计划本次最终单审与signed source Push尚待；此前source仍 **60/71=84.51%**，本卡与网站owner实际签推后为 **61/71=85.92%**；formal仍 **55/71=77.46%**。COV2 source13/26=50.00%、formal0/26不变。


> **当前live partial修复 G09（2026-10-14 16:15:36 UTC，本三文档实际Claude PASS＋signed main采纳后生效）：** 用户明确要求修复刚刚的自动捕获问题后构建最新本机版本再测试。新增仅一张完整卡 `FIX-OG-PARTIAL-LIVE-01`，父计划70→71；IDLE已实际签推main `95ab8f35b3af7e8a5736c955212415ca93eebeb3` / cf `bc58acc2dbe057dc039fedfc3f3d432b9b4dd332`、remote各同SHA，源码已交接。采纳前实际source60/70=85.71%、formal55/70=78.57%；采纳后source **60/71=84.51%**、formal **55/71=77.46%**，新卡UNRUN不预计完成。COV仍26卡/source13/26=50.00%/formal0/26。原70及其较早分子均保留历史；import020必要后续目前只有未采纳private骨架，不算新增Task、不伪称BASE真实import或049门已关闭。root唯一共享源码写者并发起sole-Claude，私有prototype不是实施GO。

> **当前 FIX 源码交接与 IDLE 开工（2026-10-14 15:47:50 UTC）：** FIX-OG-NATIVE-IMPORT-01 已完成实际 A/B、fmt/diff/strict 与唯一 Claude Code PASS，signed+DCO main `6524153d97ab2f5c02f7875abd5f3ed3d343cea8` 和网站cf `ddfee87c66d35cf586ca4534c0d427a7254085d0` 已 Push、各与 remote 同 SHA。父计划当前源码就绪 **59/70=84.29%**，正式 **55/70=78.57%**；COV2 当前源码就绪 **13/26=50.00%**、正式 **0/26**。OG-IDLE-ERROR-029-01 当前仅本机三个新 exact 各1/1及完整模块31/31通过，已应用source/docs的fmt/strict与相关模块门已通过，三计划最后同步、单Claude及源码/网站signed交接仍待，**不预增60/70**。下一源码卡 OG-UPGRADE-029-01 在新独立Libra原生仓执行普通29真实采集验收，当前UNRUN；本三卡/临时049 C/D、最终新树full和Source A均未因此放行。下方旧规划/开工数值仅为当时历史快照。

> **历史普通 OpenCode 2.0.29 / G-09 三卡规划快照（2026-10-14 14:30:58 UTC，本次三文档最终单Claude和signed main采纳后生效）：** 用户要求当前工作升级2.0.29。本机global与官方隔离npm platform binary已实测同为 `opencode v2.0.29`、186170000 bytes、SHA `7f46ba1dad86b730903d9adf54015bb4e706fc1ab6a247affc5a635cd8a663f3`；官方tag/commit为 `v2.0.29`/`042d0832f5f3a73eaa039a58a7d565776f6d4290`。新增三独立卡 `FIX-OG-NATIVE-IMPORT-01`→`OG-IDLE-ERROR-029-01`→`OG-UPGRADE-029-01`；旧拟UPGRADE的idle.error轴拆出IDLE，实际内容验收只在BASE。父计划67→70卡、正式55/70=78.57%、源码就绪58/70=82.86%；本三卡均pending/空且未计新增完成。COV2仍26卡，已签旧收据保留；COV25冻结full10110的结果只证明其原冻结SHA，不是含5c及三卡新源码验收。全局27/旧26与private.1/.2仅属当时观察/冻结身份，当前普通29 source、fixture namespace和本机lane独立；native.3登记及真实Source A仍独立，不阻普通29/049；151个机械案例不构成上游OpenCode全量source/producer验收，whole-producer check/full仍UNRUN。任何旧段落保留的67卡或55/67均为本次采纳前历史口径，现行统计只采用70卡；原历史数值和失败/取消不倒改。

> **用户恢复原计划 USER-RESUME-COV2-20261014 历史开工快照（2026-10-14 11:13:55 UTC）：** 用户明确继续原计划。恢复基线为真实最新上游 signed main `35f8b15db38037b9af1fb696afc2bcad295500a8`（含 cat-file 修复），HEAD/remote/三文档 preimage 已核同；恢复 G-09 → COV2-25 → COV2-22 → COV2-08 与后续 DAG。三文档最终单审和 signed Push 后本次修订采纳生效：COV2 26卡/source-ready12/26=46.15%/formal0/26；采纳前历史25卡/source-ready12/25=48.00%，父formal55/67=82.09%不变。COV2-25 AC/VER、含上游修复的最终 full 与家族 C/D/Source A 仍 UNRUN/未闭合。

> **用户临时 patch 发布 USER-TEMP-REL-049（恢复后的新增明确授权）：** 用户要求纳入上游 `cat-file` 修复，在 COV2-25 本机 A/B、最终单 Claude PASS 与 signed+DCO main 源码交付后临时发布 **0.30.49**。执行者为本对话 root 单一主仓写者/发布者；先核真实最新上游并保留双方变更，实际源码变化须在最终源码重验。此项release操作自身不新增任务卡；其本次G09兼容前驱另新增父计划FIX/IDLE/BASE三卡而COV2不增，临时操作登记REL-OG-TEMP-049，不代替 COV2-REL-01，不据此把 COV2 家族子卡或 DEP-OG-13 记 complete。原半家族禁止稳定发布的规则只对此次用户明确授权操作覆写：必须证明所有未完成 coverage-v2 生产入口仍不可触达且普通 macOS 捕获行为安全；若证明失败则先修复或保持 UNRUN，不能发布。该临时 release 须沿用用户已授权的纯版本变动例外：先把真实最新上游 `cat-file` 与本次FIX/IDLE/BASE三卡纳入最终实现树（COV25旧full不替新源码），并完成适用 focused、双 env 全量 Nextest 与唯一 Claude PASS；若后续临时 bump 仅改变三个版本面及工具链锁文件中的包版本（另可新增 `CHANGELOG.md` 的纯发布说明及本次计划记账），并以逐路径 SHA/mode 证明生产实现、测试、配置与行为文档同一，可复用该已绿 full，不再重复全量。新增 CHANGELOG 仅追加 `[0.30.49]` 节并保留全部历史节；它是发布说明的新增验收面，不从用户纯版本授权推断已测试，bump 树必须现场核 `rg -n "CHANGELOG" tests src build.rs` 的读取者，并实跑 `source .env.test && cargo test --test compat_r0_9_doc_closeout diff_docs_and_changelog_carry_the_degradation_semantics -- --exact` 具名 1/1及任何其它实际读取该文件的相关契约。bump 后仍实跑 fmt/strict、版本面契约、唯一 Claude 最终字节审查、当前 SHA 安全 CI、完整 release 流程（当前 8 个 job）全部 success、Homebrew tap 更新与 `verify-homebrew-formula` 实际运行且 success（不得 skipped 或仅缺 token warning；formula version/SHA 必须与本次 release 资产一致）、四平台签名资产/manifest、CDN 版本/manifest/资产同源及网站/安装验收、实际 macOS 安装版 `cat-file` smoke 与用户指定 curl 安装；任何实际源码/测试/配置/行为变化均须在最终实现树重新 full。网络失败仅在日志确认后用 gh 重跑。家族正式唯一发布点仍为 COV2-REL-01，Source A/native 三字段门不变。

> **当前执行授权 USER-NO-WAIT-RELEASE-20261012（2026-10-12T01:17:47.263851+00:00）:** 用户明确要求“每次无需等待 Release，继续向前推进任务卡”。本条覆盖下文尚存的“组外 implementation 前驱必须 actual complete/六RG complete后才能开工”排期文字：后继开发消费前驱实际最终 A/B、适用 focused/feature/full、fmt/strict、已关闭 Claude Code 单评审 PASS、signed-DCO main source 与逐文件 owner handoff 的 **source-qualified local Deliverables**，不等待其后置 Release/C-D。仅设计 PASS、编译成功或尚未完成的本地门不能替代这些条件；发现前驱 source/material defect 时相关接线仍阻塞。逐卡验收与直接 signed main Push继续，发布者固定快照、版本和 Release 串行；新卡 delta不得混入已冻结的早一发布快照。前驱和后继 formal done/complete仍分别等待所属实际 C/D，source-ready与formal进度分开统计；所有安全CI、macOS功能、四平台、签名manifest、Homebrew、指定curl安装、网站以及最终full均保留。真实native来源A仍要求逐字段真实证明，缺证partial/不归档，不能以此顺序覆写mint/locator权限。最多既授权并行数与单写者不变；本轮root sole writer、不创建PR或额外Codex审查。

> **本文件是全仓计划的单一执行状况视图**，按任务卡粒度汇总每一份计划的执行状态，并登记计划内的延后决策/实施项（`DEFER-*`）与跨计划依赖。**任何执行计划的 Agent 在完成/推进一张任务卡时，必须在同一变更中同步更新本文件**；新建计划时必须在「计划一览」登记一行，并把本文件的更新义务写入新计划的「使用规则」或修订历史。
>
> **维护规则（强制）**
>
> 1. 每张卡的状态推进（`pending` → `in-progress` → `blocked` → `done`，`Acceptance` 随 ER-04 转移）都在「计划一览」的对应行更新，并附发布版本 / commit / 时间（`YYYY-MM-DD HH:MM:SS UTC`）。
> 2. 计划收口、拆卡、合并发布、新增 `DEFER-*`、`DEP-*` 状态变化，同步更新「延后与未决策项」与「跨计划依赖」两节。
> 3. 新建计划：在「计划一览」加一行（类别、状态、一句话进度），并在「未启动计划」或「实施中计划」小节落位。
> 4. 以「计划一览」表为权威，其余小节是它的展开视图；冲突时以任务卡自身 `Lifecycle / Acceptance` 与 `plan-long.md` 的日期索引交叉核对。
> 5. 状态快照时间见本文件头，格式为 `YYYY-MM-DD HH:MM:SS UTC`（24 小时制、UTC、精确到秒）。每次更新必须把快照时间改成这次写入时的 UTC 时钟时间，便于多个 Agent 区分先后。已经写下的纯日期记录保持原样，不补写时间。
>
> **历史快照（原正文与数字保留，本轮当前范围见页首G09）：** 本次G09采纳后三新卡均pending，父70卡/正式55/70=78.57%、source58/70=82.86%；以下67卡快照保留为采纳前历史。 plan-20260902 已在 `71e9f446` 的 OG-17/21/22 实际 C/D 收口后补六卡，正式 **55/67=82.09%**，OG-24/25/26 本机源码 locally-accepted（按源码就绪 58/67=86.57%，家族 C/D 未结；拆卡前55/61=90.16%、`c3fb205` 草案时52/67=77.61%均为历史分母/时点）；2026-10-14 14:30:58 UTC（模板 `v2.14`：验收证据写入计划文件，不新建证据文件或文件夹；终止链末端提交的 D 组门结果由本文件例行同步补记）。[`plan-20260904.md`](plan-20260904.md) FIX-RG-01 `done`/`complete`（v0.30.39：PR #624 → 9b8fc5c7，release run 37777413227 八任务 success，默认 macOS 安装 smoke PASS；2026-10-08 13:54:21 UTC）。[`plan-20260926.md`](plan-20260926.md)：**DM-00 `done`/`complete`**（2026-10-07 01:03 UTC——`## 取材清单` 追加（34 项 memory 文件 + 迁移号跳越 + `MemoryConfidence` 来源），#456 远端事实带 UTC 时间戳冻结（`state=CLOSED`、`mergeable=MERGEABLE`、`mergeStateStatus=UNSTABLE`、149 files、26 commits、reviews=0），`plan-20260819` 已正式登记退役（M2-01..M2-16E 全部退役、R30 评审日志保留）；取材 clone 降级至 `/tmp/libra-pr456`（`/run/media/genedna/data` 不可写），pin `bc1be587e4` 与 merge-base 校验通过。`plan-20260819.md` 状态改「已取代」。）；**DM-01 `done` / `complete`**（2026-10-07：迁移 A `2026092901_memory_core` 已交付并经 REL-DM-01 独立发布至 **v0.30.31**（merge SHA `603bad3`、PR #614、release run `37588423715`）；本地全量 nextest（umask 0022）9549/9549 全绿，D 组 `base.yml`/CodeQL 全绿，`compat_version_surface_sync` 通过；期间补齐 `mutable_state_ownership` 对三张投影表的分类（`MIGRATION_ONLY_TABLES`））。**DM-10 `done` / `complete`**（2026-10-07：迁移 B `2026092902_memory_path_search` 交付并经 REL-DM-01 独立发布至 **v0.30.35**（merge `c384bc7`、PR #618；随后的版本面补丁 `b51f5c3` 把 0.30.35 对齐到 tag，`compat_version_surface_sync` 通过）；D 组 `base.yml`/CodeQL 全绿、`compat-offline-core` 全量通过；含 `mutable_state_ownership` 对二张新表的分类与模拟旧 runner `UnsupportedFuture` 断言）。**DM-02 `done` / `complete`**（2026-10-08：`src/internal/ai/memory/` commit/change 派生实现并经 REL-DM-01 独立发布至 **v0.30.37**（merge `125657b`、PR #620）；本地聚焦门 + `memory_projection_test` 全绿、DM-02 `rg` 守卫零命中、含修复 `parse::<ObjectHash>` → `object_format::parse_repo_oid` 的 `compat_object_hash_parsing_guard` 通过；D 组 `base.yml`+CodeQL 全绿、`compat-offline-core` 全量通过。ER-05 评审待记录）。**DM-13 `done` / `complete`**（2026-10-08：commit 源新鲜度/horizon/重建等价实现并经 REL-DM-01 独立发布至 **v0.30.38**（merge `99c4bce`、PR #622）；`memory_projection_test` 11/11 全绿、D 组 `base.yml`+CodeQL 全绿、`compat-offline-core` 全量通过；ER-05 评审待记录）。`plan-20260907` 仍**已收口** —— B3-00..B3-17 全部 `done`/`complete`（B3-00 `v0.23.68` → B3-10 `v0.29.0`；REL-B3-01/02 家族经 B3-15/B3-17 发布）；最终 Codex/Claude 双评审 R40 **PASS**（0×P0/0×P1）；GC-B3-01/GC-B3-02 收口守卫全绿（本次复核，含把 `bundle_client.rs` 的 `as_str` 臂与 `local_client.rs` 注释收敛到 `object_format::as_str`/注释化，消除 GC-B3-01 零命中守卫告警）；`DEP-B3-05` 已满足。`plan-20260913.md`（Media）实为 **实施中**：FL-00/01/07/02/06/03/04/05 均 `done`/`complete`（`v0.30.0`..`v0.30.5`），仅剩计划级收口门——此前本文件误列「未启动 / FL-00 pending」，已更正。 [`issues/500.md`](issues/500.md)：R41 终确认轮 Codex full-plan **PASS**（findings: clean，P0=0、P1=0、P2=0）及 Claude targeted **PASS**（P0=0、P1=0、P2=0，仅 P3 nit）——**双评门已满足**。**2026-10-05 执行收口：NO-GO**——本地 trunk compose 部署实测 CS-16/19/17 均 `not-qualified`（storage-only 读面无独立 ACL；pin 无 committed-set revoke 路由），CS-20 aggregate=`not-qualified`，且维护者未点名 `agent_session`（已发征询）；CS-00 判定 **NO-GO**，CS-10/11/12/13 延入 DEFER-CS-02/03/07/10，CS-18 登记 defer+outgoing handoff，CS-14 closeout 收口。**2026-10-05 收口 patch 发布：`v0.30.29`**（PR #613/queue head `370dc94`；版本面与 `Cargo.lock` 一致；恢复 `tests/compat-ledger/SURFACES.gen` exec bit（100755）；本地全量 `nextest --all --no-fail-fast --retries 2`（umask 0022）9525/9525 全绿（1 flaky 重试转绿），`fmt`/`clippy -D warnings` 通过；远端 D 组：D-1 base.yml PR #613 head `ade89a1`（run `37344599122` attempt 2）全绿；D-2a CodeQL PR run `37344599037` 全绿；D-3 release run `37336809254` 8/8 全绿；D-2b merge-to-main CodeQL run `37354926375` 全绿（merge SHA `cbded577`，security-codeql-actions/Rust 均 success）。**当前卡：CS-00/14/15/16/17/18/19/20 `done` / `complete`；CS-10/11/12/13 `deferred` / `not-applicable`。[`plan-20261001-mega-browser-noninteractive.md`](plan-20261001-mega-browser-noninteractive.md)：MN-05/06/09 已合入 `origin/main`（v0.30.24–v0.30.26）。[`issues/498.md`](issues/498.md)：**已收口**——TT-05 / TT-02 均为 `done`/`complete`（v0.30.22 / v0.30.23）；M3 全量收口门 8754/8754 passed；#498 CLOSED；DEFER-TT-01..08 残留。[`plan-20261001-mega-browser-noninteractive.md`](plan-20261001-mega-browser-noninteractive.md)：12 卡全部 `done`/`complete`，v0.30.27 已发布；原实现基线 `faae20c` 的全量 8839/8839 与网站示例 1/1 保持为历史证据；Codex R19 `PASS`（最终计划收口审查）与 R20 `PASS`（对照 incoming main `721fec9` 的文档差异审查）见计划评审日志。R21 FAIL 的文档问题已由 R22 PASS 复核关闭。PR #609 原 head `adb9b59`（base `892f37c`）的 run `37201651296` 有 2 个失败 job、4 个失败测试（9523 项中 9519 通过）；PR 于 2026-10-04 14:27 UTC 以该 head 合并。后续 CI 修复已从当前 main `622bed5` 拆至分支 `fix/opencode-exporter-cancel-ci`，全量本地测试、格式及 workflow lint 通过；本地 macOS Clippy 未编译 Linux-only helper，PR #612 首轮 run `37230096807` 因 `linux_pidfd_supported` unused 而失败；该未调用函数已删除并等待新 run 验证。run `37231625328` 的 `opencode-export-linux` 在 `runner_controls_preserved` 失败，因为通用 fixture runner 没有 PID namespace；现只在 trusted-bwrap 入口要求 namespace-init pidfd，bridge E2E 因前置失败未运行；本地 exporter 模块定向测试 15/15、Clippy、格式与 workflow lint 通过；run `37233480909` 的 Linux sandbox gates 通过，但 Clippy 与 bridge 集成测试编译均被 `run_bounded_exporter` 的 `dead_code` 阻断。已将该仅用于单元测试的 wrapper 限定为 `cfg(test)`，待新 run 验收 Clippy 与真实 bridge E2E。[`plan-20260924.md`](plan-20260924.md)：**已收口**（ACF-09 `Lifecycle=done`、`Acceptance=complete`）；`DEP-ACF-MIRROR` 已交接；`DEP-ACF-CAP` 的 ACF 侧必要条件已交接（CAP-04..07 仍受独立 security/privacy RFC `blocked`）；`DEP-ACF-DM06` 正式 handoff 已完成，DM-05 仍 `pending`；main/no bump/no PR/no branch，DEFER-ACF-05 不变。`issues/451.md`（Issue #451 version-aware M2 Episode memory RFC stub，从未评审）已并入 [`plan-20260926.md`](plan-20260926.md) 附录并删除原文件（2026-10-07），不新增任务卡。`issues/468.md` 已更新陈旧引用（`plan-20260819` R30 → `plan-20260926`）并明确边界：其为 ACF/SCAP capture 协调层下游的精炼/契约管道，非第二个 ingest 协调层（2026-10-07）。**[`plan-20260919.md`](plan-20260919.md) 已收口（GCX-02/03/04 全部 `done`/`complete`，2026-10-07）**：GCX-02/03/04 分别经独立发布 **v0.30.32 / v0.30.33 / v0.30.34**（PR #615/#616/#617；`compat_version_surface_sync` 通过；D 组 `base.yml`+CodeQL 全绿，含 `compat-offline-core` 全量 L1+L2+L3；GCX-04 的 `opencode-export-linux` 为 OpenCode Session Capture 开发中已知波动，不阻塞）。 2026-10-12 current correction: plan-20260902 FAST-RG-01 is formally done/complete at v0.30.46; OG19 exact-source canonical macOS full is 9880 passed (1 flaky retry-passed, 1 leaky, 10 skipped), locally-accepted but awaiting FAST-OG-02 C/D; OpenCode CI audit found no local-install/account/model-response test to remove; OG20 is next, fixed-plan progress 46/61=75.41%, Source A/native GO remains unproven.
> **跨计划登记（2026-10-13 20:58:21 UTC，COV2-03 signed source 与 G-09 25卡修订）：** OG-26 signed+DCO main `734b6f03884d3aa38400acbbfd446b557e642d94` 已交付。COV2-01/13/02/07/14/03 六卡本机 A/B 与唯一 Claude PASS，signed+DCO main 依序 `a3b51fd103fc2aa95f64c3395339a00937060356` / `cd698ade182a935c4d91b0f84cb3ad13e8325e0b` / `d4f159fd17676c7b591c1f80858f2de17094260f` / `14fc516d94955fadb867c16868e7f12792c4b52e` / `4558af64c09e63c4716fa68595776a8044f66cc7` / `d717e42e0d1ec75e6ae576b21242fd1519040abd`。COV2-14 最终双 env full 10069/10069 exit0/raw SHA `2849dcf7e2105ed1da5e5d3b0333374e2590ac909c1f478ef8ba68966e86aec4`，单Claude PASS/raw SHA `2d1e5fb7425abc9aa2802db241213811dfcd6c891410e9853e554c5fcb806966`；25卡修订口径当前 formal **0/25**、source-ready **6/25=24.00%**，原19卡收据保留历史。COV2-03 已完成本机源码交接；此前23卡 G-09 三文件协调已由最终字节 Claude PASS/raw `77d4820e329ade99f5fba7b43821d072d4d7068ee5ba639ada4704851686f4fd` 与 signed+DCO main `eaa43ec13f6c50212f0b3c2fa8a51799ec74bff5` 交付，r4 FAIL 仍保留。COV2-03 五项 VER、五项旧 v1 ER-04 与两项缺表回归各 1/1、fmt/strict Clippy/diff exit0；full-r3 实际 4587 通过/1 失败已修但不计 PASS；最终双 env full **Summary [1618.814s] 10078 tests run: 10078 passed (14 slow), 23 skipped**/raw SHA `c49dd3c33652d55541a0e4c0269c817460b9109cf261eab5271d90760a49e8a9`、exit0；唯一最终 Claude Code PASS、P0/P1/P2=0、P3=5/raw SHA `9ef25f92615eefcf2019212a306cfa7542ef416b693286bb99111006b3ea6df7`；signed+DCO 源码/文档 main `d717e42e0d1ec75e6ae576b21242fd1519040abd` 已 Push、remote 同 SHA。下一卡 COV2-19；新增19/20/21/23/04/24/22仍UNRUN，COV2-23 八项 AC/四项 VER、COV2-24 五项 AC/三项 VER 全部未执行；COV2-19 开工前须核本次三文件规划协调的最终字节单 Claude PASS raw SHA、独立 signed+DCO main SHA 与 remote 同一性，并在该卡首个状态同步登记收据。父计划 formal **55/67=82.09%**，`DEP-OG-13` 未 source-ready/complete，Source A/native GO=false。

> **COV2-24 守卫修订前执行收据（2026-10-14 04:52:51 UTC）：** COV2-04 最终七文件 manifest SHA-256 `b2206b7579e0cd0b1a05b858d6968e238ec56ce0ad9b95210509484d4b2535b2`、单 Claude PASS/raw SHA-256 `3a9749ad4679e8e038eb7f3d5d09e36e0e367de4411e4f5848371c55d6ad5aac`、signed+DCO main `03b10f887368a3db5206589f5dd6947432ffe26c` 与远端同 SHA 已复核；前驱 ER-13 `none`，未独立跑 full。当前正式 0/25、源码就绪 11/25=44.00%，父计划正式 55/67=82.09%。COV2-24 已在本机通过三项 VER 与六项继承各具名 1/1；T-5 双 env full、最终质量门、唯一 Claude 与签名 main 尚待，不预增源码就绪或冒称家族 C/D、OpenCode Source A。新提交出现后旧 Actions 按用户要求取消；只有日志证实为网络故障时才以 `gh` 重跑对应任务。

> **COV2-25 G-09 三文档规范性修订（2026-10-14 11:13:55 UTC，本次三文档签名 main 提交采纳生效）：** COV2-24 已按25卡口径交付源码就绪 12/25=48.00%；COV2-24 最终 5/5 AC、3/3 VER 与七项 mandatory ER-04 精确 1/1（十 selector；focused index `39306128fdc72bbb2ae9bca583f186f2d8bfda693b68fb1a38795b815c9e31d5`），nightly fmt/strict Clippy/diff exit0（quality index `d0c676eb165802886f6f700d869d813db610a0fc7d09bf5e9759c452b71e979b`），双 env T-5 `NEXTEST_TEST_THREADS=4` 10103/10103 passed、23 skipped、exit0（raw `2908fff258247c857ae857a4759fcde9b925c212cc5912fa373ece16b5373029`）；唯一 Claude Code PASS、P0/P1/P2=0、P3=3（raw `a6d467bcdc33c42fff3c45f55681ebbaefa213d96cfd59a8b901c5d3dff942f7`）；八路径 0644 manifest `cd43b9af3980c8f3e90ce5751df7855ebaf1f604e2803130f0d32d9556b46e98`；signed+DCO main `c56de683acf71d7327436eef7ef9185d97dcff27` 与远端同 SHA。两轮失败 full 保留为失败历史；COV2-24交接时安全 CI 未完成，其后 c56/5b0 的未完成旧 Actions 按用户新 commit 策略取消，cancelled 不记 D 组通过。 本次新增 COV2-25，当前正式0/26、源码就绪12/26=46.15%；父计划67卡、22个macOS Session案例、Source A/C-D门不变。COV2-25 AC/VER全 UNRUN；三文档最终字节唯一 Claude 与签名 main/remote 收据在 COV2-25 开工前补记。

> **COV2-24 T-5 首轮失败与精确写集修订（2026-10-14 06:22:08 UTC）：** 双 env `cargo nextest run --all --no-fail-fast --retries 2` 首轮 **10103 tests run: 10102 passed（31 slow、1 flaky、2 leaky）、1 failed、23 skipped**，exit1；唯一确定性失败 `compat_agent_architecture_guard::reasoning_pending_seal_borrows_only_bound_native_witness` 连续三次发现新增合法调用点 `pending_payload.rs::seal_bound_versioned_reasoning` 未登记，原始私有日志 SHA-256 `1d70392229a6fdb29c995a0690679aa86b173184316f634161cf02f169d90d1e`。此项不是网络失败，不用 `gh` 重跑。按 G-09/I 仅在 COV2-24 增既有 `tests/compat/agent_architecture_guard.rs` 的一个精确闭集入口，生产文件、5 AC/3 VER、25 卡分母不变；第七项 mandatory ER-04 守卫及九项原 focused、fmt/strict/full、最终唯一 Claude、signed main 仍须在修订后重验。当前 formal **0/25**、source-ready **11/25=44.00%**，父 formal **55/67=82.09%**；失败与 synthetic 均不放行家族 C/D 或 Source A。

> **COV2-24 当前本机验收收据（2026-10-14 08:19:08 UTC，单审/签名前）：** 精确八文件（两生产、一既有架构守卫、五文档），5/5 AC、3/3 VER 与七项 mandatory ER-04 在最终生产源码各 exact 1/1；focused 私有日志索引 SHA-256 `39306128fdc72bbb2ae9bca583f186f2d8bfda693b68fb1a38795b815c9e31d5`，nightly fmt、`libra diff --check`、strict all-targets/all-features Clippy exit0，quality 索引 SHA-256 `d0c676eb165802886f6f700d869d813db610a0fc7d09bf5e9759c452b71e979b`。T-5 首轮架构守卫失败 10102/10103、exit1/raw `1d70392229a6fdb29c995a0690679aa86b173184316f634161cf02f169d90d1e`；第二轮旧 `opencode_installer_rejects_non_regular_config_paths_without_blocking` 高负载计时失败 10102/10103、exit1/raw `129610d12c36494ba8769e166b079cd2f4e7e20bf10f789c23863771a4fc45e5`。该旧用例单独运行 1/1/raw `3219d447ff82032ef792dfa2ccf068391cd9f575177e5d6d1ae3c17c4fef26c6`；第三轮同一最终生产源码、双 env、未过滤的 `cargo nextest run --all --no-fail-fast --retries 2`，仅 runner 环境 `NEXTEST_TEST_THREADS=4`，**10103/10103 passed（2 slow）、23 skipped、exit0**，raw SHA-256 `2908fff258247c857ae857a4759fcde9b925c212cc5912fa373ece16b5373029`，旧用例与新增架构守卫均 PASS。两份失败全量不计绿，且非网络失败，不用 `gh` 重跑；本轮无 OpenCode Session/模型实时 CI。唯一 Claude Code 评审、signed+DCO main/remote 尚待，当前正式 **0/25**、source-ready **11/25=44.00%** 不预增；父计划正式 **55/67=82.09%**，家族 C/D、Source A 未闭合。


> **COV2-24 签名源码交接（2026-10-14）：** COV2-24 最终 5/5 AC、3/3 VER 与七项 mandatory ER-04 精确 1/1（十 selector；focused index `39306128fdc72bbb2ae9bca583f186f2d8bfda693b68fb1a38795b815c9e31d5`），nightly fmt/strict Clippy/diff exit0（quality index `d0c676eb165802886f6f700d869d813db610a0fc7d09bf5e9759c452b71e979b`），双 env T-5 `NEXTEST_TEST_THREADS=4` 10103/10103 passed、23 skipped、exit0（raw `2908fff258247c857ae857a4759fcde9b925c212cc5912fa373ece16b5373029`）；唯一 Claude Code PASS、P0/P1/P2=0、P3=3（raw `a6d467bcdc33c42fff3c45f55681ebbaefa213d96cfd59a8b901c5d3dff942f7`）；八路径 0644 manifest `cd43b9af3980c8f3e90ce5751df7855ebaf1f604e2803130f0d32d9556b46e98`；signed+DCO main `c56de683acf71d7327436eef7ef9185d97dcff27` 与远端同 SHA。两轮失败 full 保留为失败历史；该历史时点安全 CI 尚未完成；其后因 main 前进被取消，不记 D 组通过。 签名前的候选/失败历史段落保留原时点；本卡 source-ready 已按25卡口径升至12/25=48.00%，formal仍0/25。本次三文档 G-09 采用26卡分母；安全 CI 未完成或被取消均不算通过。
> **历史 COV2 G-09 快照（2026-10-13 20:58:21 UTC）：** 初始 19 卡及先前已签 23 卡 G-09 收据保持各自时点分母；新增 COV2-23/24 后为 22 implementation+2 audit+1 release，共 25 卡。COV2-03 在 signed main `d717e42e0d1ec75e6ae576b21242fd1519040abd` 交接后，当时 COV2 formal **0/25**，source-ready **6/25=24.00%**；父计划 formal **55/67=82.09%**。COV2-19/20/21/23/04/24/22 仍 UNRUN；COV2-23/24 仅注册规划，不把 AC/VER、Source A 或家族 C/D 记 PASS。COV2-19 开工前须核本次三文件规划协调的最终字节单 Claude PASS raw SHA、独立 signed+DCO main SHA 与 remote 同一性，并在该卡首个状态同步登记收据。

> **COV2-19 开工前 G-09 收据（2026-10-13 21:23 UTC）：** 三文件 25 卡规划协调最终字节由唯一 Claude Code PASS（P0/P1/P2=0，raw JSON SHA-256 `0575dd35ddb3503021d34181df6651362b0525c2c0113020d344ba8c4044781b`），独立 signed+DCO main `3e37e55dd781e44532c6463a1a0cec484c0bb759` 已 Push；本机 `HEAD=origin/main` 核为同一 SHA。该规划收据只解除 COV2-19 的开工前置，不把 COV2-19 的 AC/VER 或家族 C/D 记为通过。

> **COV2-19 预审时点 A/B 快照（2026-10-13 22:43 UTC）：** 五项 VER、16 项 inherited/v1/RG-04 与一项独立 native replay/marker/同槽升级/损坏 map 安全回归，在当前源码共 22 个具名 selector 各 1 passed/0 failed/0 ignored；日志清单及单测 SHA 见本卡正文。nightly fmt、strict all-targets/all-features Clippy 和 diff check exit0。唯一 Claude Code 的只读预审初次 P2=1 已修；复核 **PRELIM_CLEAN** P0/P1/P2=0、P3=1，raw SHA `4407a17a73a3ca0bef86cabc0e38753ea8722b5425dea52cf9f2079f3ac72268`，尚非正式 PASS。双 env full 当前正在运行，最终字节 Claude、signed+DCO main 与家族 C/D 仍待；COV2 正式 **0/25**、源码就绪仍 **6/25=24.00%**，父正式 **55/67=82.09%**，下一步仍 COV2-19。旧失败/中断、synthetic、skip 不记 Source A/通过；Linux 功能由用户后验。

> **COV2-19 签名前 A/B 最新收据（2026-10-13 23:57:33 UTC）：** 本机 8/8 AC、5/5 VER 与 16 项 inherited/v1/RG-04、独立 native replay 和架构守卫精确门均通过；nightly fmt、strict Clippy 与 diff check exit0；双 env full **Summary [1242.578s] 10084 tests run: 10084 passed (15 slow, 1 leaky), 23 skipped**、exit0、raw SHA-256 `20378e04b5c585c954421780520a6a62ebbf1f6c7ba0336db40dd76541058e43`。唯一 leaky 项为 `command::cat_file_test::test_cat_file_ai_type_nonexistent`，不在本卡写集；旧 umask077 权限误报与修订前架构守卫 FAIL 保留历史。八文件最终唯一 Claude Code 单审和 signed+DCO main Push/remote 同一性仍待，故 COV2 formal **0/25**、source-ready **6/25=24.00%**，父正式 **55/67=82.09%**；下一步仍 COV2-19，完成后接 COV2-20。家族 C/D、真实 Source A 与 Linux 后验未计通过。

> **COV2-19 最终本机交接（2026-10-14 00:07:12 UTC）：** 8/8 AC、5/5 VER、16 inherited/v1/RG-04、native replay 与架构守卫精确门各 1/1；nightly fmt、strict Clippy 与 diff check exit0；双 env full **Summary [1242.578s] 10084 tests run: 10084 passed (15 slow, 1 leaky), 23 skipped**、exit0/raw SHA-256 `20378e04b5c585c954421780520a6a62ebbf1f6c7ba0336db40dd76541058e43`。八文件最终唯一 Claude Code **PASS**，P0/P1/P2=0、P3=3/raw SHA-256 `64068902908f4c521e380dc7e5cf593134eaa99697de208344b65f75b3c3caa8`；signed+DCO main `354ff768c406bdad74607da62002bff810147c24` 已 Push，本机与 GitHub remote 同 SHA。COV2 formal **0/25**、source-ready **7/25=28.00%**，父计划 formal **55/67=82.09%**、source-ready **58/67=86.57%**；下一卡 COV2-20。三项 P3、家族 C/D、22 个最终行为源码 Session、真实 Source A 与 Linux 后验均未计通过。

> **COV2-20 实施中与前驱 Windows CI 前滚（2026-10-14 00:18:49 UTC）：** 前驱 `354ff768` 的 `compat-scoped-input-windows` job `115606533849` 在 `RUSTFLAGS=-D warnings` 下因八处 Unix 测试消费者缺席引发 `dead_code` 编译失败；原始日志确认为确定性源码问题，不以 `gh` 重跑冒充网络恢复。按 G-10/I 在 COV2-20 实施卡内先修订精确写集，新增 `src/internal/ai/capture/checkpoint.rs` 仅用于平台限定 cfg 前滚；功能 AC/VER 不变，最终两生产文件连同文档须重跑本机 focused、fmt、strict Clippy、双 env full、唯一 Claude 后 signed+DCO main 推送，再由新提交 Windows job 实证。当前 COV2-20 既有四个 focused selector 各 1/1，但这是修订前候选，不计最终验收；Windows、Source A、家族 C/D 与 Linux 后验均未通过，正式进度不变。

> **COV2-20 最终候选局部门（2026-10-14 00:27:29 UTC）：** Windows cfg 前滚后 `coverage_gate.rs`/`capture/checkpoint.rs` 两生产文件 SHA-256 分别为 `1104ba09ef8b638b34a59c6844eb74a975cc05fd1343d66d74f4e00341c84ac9`/`cae575f0d15662ffcc72c2890ace5134aee1d657831fc750e758d362011acb69`、mode 均 0644；本卡三项 VER 与 inherited v1 精确各 1/1，日志索引 SHA-256 `5dd3034d489ae5139e8ce1ce6af4f1703e6b01d08a780e19019ed0a4271255ef`，nightly fmt、strict all-target/all-features Clippy、diff check exit0。双 env full 已启动但尚未完成，唯一 Claude、signed main 与新提交 Windows CI 均待；COV2 formal **0/25**、source-ready **7/25=28.00%**，父 formal **55/67=82.09%**、source-ready **58/67=86.57%**，下一步仍 COV2-20。真实 Source A、家族 C/D 与 Linux 后验未放行。

> **COV2-20 最终源码全量收据（2026-10-14 01:00:52 UTC）：** 修订后的同一代码在 `umask 0022`、双 env 本机 `cargo nextest run --all --no-fail-fast --retries 2` 实际 exit0，**Summary [1069.256s] 10087 tests run: 10087 passed (13 slow, 4 leaky), 23 skipped**，原始日志 SHA-256 `6efa8a7d8461a9f4a5740661c5e99cb276804fe6d2b2a80d9264a9513da2124f`。四个 leaky 名称在原始日志可核，23 skipped 未作本机 Session 验收。唯一 Claude、signed main、修复后 Windows CI 尚待；COV2 formal **0/25**、source-ready **7/25=28.00%**，父 formal **55/67=82.09%**、source-ready **58/67=86.57%**，下一步仍 COV2-20。

> **COV2-23 G-09/I 扩写集与全量前滚（2026-10-14 02:55 UTC）：** COV2-21 前驱 signed+DCO main `6b24d797583afe1c0265f6e9fcbd364566671167` 已核；本卡 8/8 AC、4/4 VER 与四项继承精确测试各 1/1，初轮 fmt/strict Clippy/diff exit0。首轮双 env full exit1：10095 项中 10093 passed、2 failed、23 skipped，raw SHA `b55e5ad4986f89773c21d2beea2f28bb029403cf79ae05b7ba6c7bf8a95c1246`；两个确定性架构闭集守卫均检出新增 `prepare_pending_native_versioned_seal` 对 `from_validated_pending_context` 的调用，非网络故障。计划精确 I 增既有 `tests/compat/agent_architecture_guard.rs`，窄范围 Claude 前置复审 PASS/raw `2d1027ce9380c0c64e199a80556524af2a6d6e0eab67713012530daaa176b571`；两个守卫各 1/1 PASS，修复后 fmt/diff/strict Clippy exit0。新 full、最终七文件唯一 Claude、signed main 均待，故 COV2 formal **0/25**、source-ready **9/25=36.00%**，父 formal **55/67=82.09%**；下一步仍 COV2-23。Source A、家族 C/D、Linux 用户后验均未通过。

> **COV2-23 修订后全量候选（2026-10-14 03:42 UTC）：** 本卡四项 VER、四项继承及两项闭集守卫均精确 1/1；nightly fmt、diff check、strict Clippy exit0。第二轮双 env `cargo nextest run --all --no-fail-fast --retries 2` 实际 exit0，**Summary [1122.120s] 10095 tests run: 10095 passed (17 slow, 1 flaky, 2 leaky), 23 skipped**，raw SHA `5b20f0bfcf4ab2664b89dcb806c14ba812aa5597c7c7b48ca2cac621f5ca6599`。验收 wrapper 因错误的顶格摘要匹配在 Cargo 成功后自行 exit1；已修 parser 并在同一日志验证摘要，不改写全量结果。首轮 2 failed 保留历史，不合并计绿。最终七文件唯一 Claude 与 signed+DCO main 仍待，COV2 formal **0/25**、source-ready **9/25=36.00%**，父 formal **55/67=82.09%**；下一步仍 COV2-23。23 skipped 非 OpenCode Session/Source A 验收。

---

## 一、计划一览

状态列取值：`未启动` / `实施中` / `已收口` / `已排期`（设计计划，尚未执行）。

| 计划 | 类别 | 状态 | 一句话进度（卡片状态） |
|---|---|---|---|
| [`plan-20261019-agent-mcp-server.md`](plan-20261019-agent-mcp-server.md) | B（外部 Agent 标准 MCP 接入） | 实施中 | 41 卡（MC-01..MC-08、MC-10..MC-41、MC-43；MC-09 已撤销，MC-42 已并入 MC-06）。MC-01 `done` / `complete`（`v0.30.68`，tag `e418297`；main `e4d843d` 的 base.yml 39427673509 attempt 2 与 codeql 39427673357 成功）；MC-22 `done`（0.30.69）；MC-16 `done` / `complete`（`v0.30.71`）；MC-19 `done` / `complete`（`v0.30.72`）；MC-29 `in-progress`（本地门绿，发布候选 `0.30.79`，D 组未绿；`v0.30.76` 的 P1 已前滚）；其余 pending。`libra agent mcp` 标准 MCP 入口（stdio 供 WorkBuddy；loopback HTTP 经 HTTPS 终端供 Grok Bot 客户端工具——不是 Grok 模型），bridge 对外契约不变、只服务 DeepSeek Harness；计划级 review：Codex R64 PASS（R66 差异确认 PASS）。 |
| [`plan-20261019-worktree-layout.md`](plan-20261019-worktree-layout.md) | 横切（worktree 默认目录与任务生命周期） | 实施中 | WD-01..08 与 WD-02G 已授权；正式 0/9。旧源码 `cc8f437` CI 8/8，2026-10-20 未提交树 macFUSE/Linux 真挂载各 16/16、cargo 全量 10533/0/26 ignored、nextest 10521/10521（4 leaky、23 skipped）、Claude PASS。已合流上游 `3471175`，用户指定最新 `0.30.77` patch + 1，即 `0.30.78` 发布，`0.31.1` 仍不发布；最终树 nextest 10664/10664 已通过；cargo 全量重跑、原生 CI/发行/网站待验，旧数据不迁移。 |
| [`plan-20261015.md`](plan-20261015.md) | B（外部Agent traces）+ 横切（Mega2 Agent 历史检索） | 实施中；Mega2 MTR-10 in-progress，Libra LTR-00 locally-accepted | 2026-10-19 15:38:49 UTC：Mega2 G-13 已退出，FIX-MTR-03/01 分别完成独立发布；FIX-MTR-04 不发布。旧 B8 FAIL、8/8 STOP 及 FIX-MTR-03 AC-1/VER-5 未勾选保持历史。MTR-87 `v0.42.55` Docker D 成功；MTR-10 四项专有 VER 通过，Git 协议 smoke 与双审仍待，本卡完整 C/D 未完成。Libra 合并候选保留 handoff 两笔文档提交及上游 COV/OG，当前 H0 尚未取得 PASS，待最新两仓联合复审；LTR-00 formal 0/26、LTR-01 未开工。原交接见[WIP 交接段](plan-20261015.md#wip-migration-checkpoint-20261016)。 |
| [`plan-20261001-mega-browser-noninteractive.md`](plan-20261001-mega-browser-noninteractive.md) | 横切（Mega2 browser 非交互操作 / 黑盒驱动） | **已收口** | **2026-10-04 21:11:16 UTC**：MN-01..MN-12 全部 `done`/`complete`，v0.30.27 与 `DEP-MN-03` 已交付；PR #609 以原 head `adb9b59` 合并，后续修复基于 main `622bed5` 移至 `fix/opencode-exporter-cancel-ci`；全量本地测试、格式与 workflow lint 通过；PR #612 首轮 run `37230096807` 的 Linux Clippy 检出未使用的 `linux_pidfd_supported`，已删除，run `37231625328` 又发现通用 runner 错误要求 `NSpid=1`；现仅 trusted-bwrap runner 捕获 namespace pidfd，本地 exporter 模块 15/15、Clippy/fmt/actionlint 通过；run `37233480909` 的 sandbox gates 通过，Clippy 与 bridge 编译均遇到 `run_bounded_exporter` `dead_code`，现已限定 wrapper 为 `cfg(test)`；待新 run 验收 Clippy 与 bridge E2E。原实现基线 `faae20c`、Codex R19/R20/R22 与版本信息见计划正文；无 LR 编号|
| [`plan-20260924.md`](plan-20260924.md) | B（Agent Capture 通用架构前置） | **已收口** | 2026-10-03 14:56:26 UTC：ACF-01..20 与 FIX-ACF-01 `done`/`complete`；R91 `VERDICT: PASS`；ACF-09 已交接 `DEP-ACF-MIRROR`／ACF 侧 `DEP-ACF-CAP`／`DEP-ACF-DM06`。CAP-04..07 仍受独立 RFC `blocked`；DM-05 仍 `pending`。 |
| [`plan-20260925.md`](plan-20260925.md) | B（Session Capture 决策中层） | **已收口** | SCAP-02 / SCAP-01 均为 `done`/`complete`（`v0.23.55` / `fa3849e`；D 组 release+CodeQL 全绿；2026-09-28 最终本地门 8241/8241 passed，本轮不 bump 版本） |
| [`plan-20260923.md`](plan-20260923.md) | 横切（CLI 补全） | **实施中（CP-00 审计）** | CP-00 audit handoff delivered **2026-10-07**（`plan-20260923-coverage.tsv`：1742 行 / 0 未分配，377 命令节点 + 1365 参数覆盖；CP-00 `locally-accepted`，`done` 待 CP-06 release 覆盖）；CP-01..20 pending；CP-06 static acceptance 阻塞全部动态工作；无实现/发布认领 |
| [`plan-20260918.md`](plan-20260918.md) | 横切（`add` 命令收口） | **实施中** | OI-01..03 `done/complete`（v0.23.4/5/6）；**OI-04 `in-progress`（v0.23.7）**；WT-05/06/07 `done/complete`（intent-to-add 全表面），其余 OI-05.. `pending` |
| [`plan-20260919.md`](plan-20260919.md) | 横切（global 配置迁 XDG） | **已完成** | GCX-01 `done/complete`（v0.23.1）；GCX-02/03/04 `done/complete`（2026-10-07 分别独立发布 **v0.30.32 / v0.30.33 / v0.30.34**，PR #615/#616/#617，D 组 `base.yml`+CodeQL 全绿；GCX-04 的 `opencode-export-linux` 为 OpenCode Session Capture 开发中已知波动，不阻塞；本轮按操作者指示未调用 Codex review） |
| [`plan-20260920.md`](plan-20260920.md) | 横切（拆 Code/Publish/Worker） | **已收口** | RC-00..RC-36 全部 `done/complete`；完成判据全勾选；DEFER-RC-04/05/06/07 已关闭；RC-00 缝清单已并入正文附录；原 `issues/ci-baseline-20260920.md` 的 CI 基线稳定性修复内容已并入「附录：CI 基线稳定性汇总修复」 |
| [`plan-20260927.md`](plan-20260927.md) | 横切（六个大命令实现的模块拆分） | **实施中（核心已发布）** | 22 张卡：核心结构拆分已落地（merge→6 模块+output、worktree→doctor/operations、status→output、diff→compare/render、rebase→state、cloud→restore）；FIX-CM-01/04/08 已实现；Windows 跨编译修复；Cloudflare D1 错误上浮修复。发布 **`v0.30.8` 全平台成功**（8/8 release job 绿：Linux amd64/arm64、macOS、Windows、homebrew-tap、install-scripts、verify-formula、stable-manifest）；全量 nextest 8274/8274 绿，clippy all-targets 干净，worktree-fuse feature 编译+`fuse_repair_*` 2/2 绿。live-compat `compat-live-cloud` **21/21 绿**（`fsck_heal` 已修——测试未先 `cloud sync` 致 blob 未进 R2；`cloud_sync_name_conflict` 亦 ok）。仍待：Cloud L3 受保护 CI dispatch（live workflow 已 enable，21/21 已证）、FIX-CM-WT-MOVE 的 DEP-CM-WT-COMPAT patch 兼容证明、22 项 EX 批准与计划级 Claude `VERDICT: PASS`。具体修复动作见 closeout 临时计划。真实 Cloud L3 仅在受保护 release-SHA CI `workflow_dispatch` 执行。 |
| [`plan-20260927-closeout.md`](plan-20260927-closeout.md) | 横切收尾（剩余 gap 跟踪） | **已排期** | 跟踪 4 项剩余：G-1 fsck_heal live 测试补全、G-2 FIX-CM-WT-MOVE 补丁兼容证明、G-3 四个 Cloud FIX 卡基建（LIVE-GATE 接线/Recovery/Repo-Scope/Live-Safety）、G-4 22 项 EX + 计划级 `VERDICT: PASS`。逐项可执行 checklist 已写入该文件；只提交推送，不 bump 版本。 |
| [`plan-20260921.md`](plan-20260921.md) | 横切（GnuPG HOME 密钥导入仓库 vault） | **已完成** | 原 `plan-20260919-gpg-import.md`；R29 双 PASS。**2026-09-24 收口：** 15/15 卡 `done`/`complete`（勾选 238/238）；操作者裁决 decision (ii) 家族面 + 管理面合并发布为 **`v0.23.65`**（run `35993926394` 8/8 success、CDN gate PASS、安装冒烟 PASS）；发布前合并上游 38 提交（`c71bee8`）并过门 55（fmt/clippy + nextest 8054/8054 + CI 附加段）。遗留：本仓 vault 签名未恢复（操作者裁决维持未签名发布）。 |
| [`plan-20260926.md`](plan-20260926.md) | C（MEM-01/02 研发历程记忆 · 确定性投影） | **实施中** | 原 `plan-20260923.md`（2026-09-25 改名为 `plan-20260925.md`；2026-09-24 合并上游时因与上游 Session Capture 计划 `plan-20260925.md` 撞名，再改为 `plan-20260926.md`）。**取代 [`plan-20260819.md`](plan-20260819.md) 承担 MEM-01/02**（使用者 2026-09-23 要求在 `libra code` 拆除后独立重设计，不沿用 R30）。**14 卡**：`DM-00` **`done`/`complete`**（2026-10-07：`## 取材清单` 已追加、#456 远端事实冻结、plan-20260819 退役横幅）；**`DM-01` `done`/`complete`**（2026-10-07：迁移 A `2026092901_memory_core` 交付，REL-DM-01 独立发布至 v0.30.31 / PR #614 / merge `603bad3` / release run `37588423715`；本地 nextest（umask 0022）9549/9549 + D 组 base.yml/CodeQL 全绿；ER-05 评审待记录，发布后置 done/complete）；**`DM-10` `done`/`complete`**（2026-10-07：迁移 B `2026092902_memory_path_search` 交付，REL-DM-01 独立发布至 v0.30.35 / PR #618 / merge `c384bc7` / 版本面补丁 `b51f5c3`；D 组 base.yml/CodeQL + compat-offline-core 全绿；ER-05 评审待记录）；**`DM-02` `done`/`complete`**（2026-10-08：`src/internal/ai/memory/` commit/change 派生实现，REL-DM-01 独立发布至 v0.30.37 / PR #620 / merge `125657b`；D 组 base.yml/CodeQL + compat-offline-core 全绿；含 `parse_repo_oid`（object_format）修复；ER-05 待记录）；**`DM-13` `done`/`complete`**（2026-10-08：commit 源新鲜度/horizon/重建等价实现，REL-DM-01 独立发布至 v0.30.38 / PR #622 / merge `99c4bce`；D 组 base.yml/CodeQL + compat-offline-core 全绿；ER-05 待记录）；**`DM-03` `done`/`complete`**（2026-10-09：`libra memory` 公开命令注册与分类表面经 #623（merge `e237365`）合并；FIX-RG-01 上游原审 F1-F10 已在 DM-11/DM-12 前处置；家族发布后 DM-12 回写；D 组 base.yml/CodeQL + compat-offline-core 全绿）；**`DM-11` `locally-accepted`**（2026-10-09：`status`/`list`/`show`/`rebuild`、`LBR-MEMORY-001/002` 与五份文档页落地；家族发布后 `DM-12` 回写 `done`/`complete`；已随 #630（merge `4721ab7`）合并进 main，仍待 DM-12 发布后回写）+ `DM-04`..`DM-09` + `DM-12`全计划**无活动 `EX-*` 豁免**。`DM-09` 为唯一收口卡，AC 按 `DM-06` 的 go/no-go 结论分 A/B 两组；`NO-GO` 时 `DM-07`/`DM-08` 经规范性修订移入 `DEFER-DM-12`。发布周期：GO 分支 10 个、降级分支 8 个（R30 为 25 卡 / 约 25 周期）。**R04 范围收窄（使用者决策）：** 「未提交 Agent 工作按代码路径召回」实测不可实现（`agent_checkpoint.tree_oid` 是 traces 树、`ToolCallRecord.paths_written` 无生产者），已移入 `DEFER-DM-11`。**评审（findings 已改为直接记入计划正文的「Codex review log」章节，`*.review/` 目录已删除）：** Codex r01 `FAIL`（21×P1）、r02（18）、r03（9）、r04（13）、r05（8）、r06（11）、r07（12×P1/4×P2），Grok r09 `FAIL`（2×P1）；**Grok r11 字面 `VERDICT: PASS`（`P0=0` / `P1=0`）**。`P0` 全程为 0。正式 handoff 已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）完成；`DM-05` 仍 `pending`，开工日须按 ER-02 重核。2026-10-07：`issues/451.md`（Issue #451 version-aware M2 Episode memory RFC stub）已并入本计划附录并删除原文件，不新增任务卡。 |
| [`plan-20260917.md`](plan-20260917.md) | 横切（cargo-test 进程内剥落） | **已收口** | SH-00..SP-01 五卡全部 `done/complete`；SP-00 结论文档已并入正文附录 |
| [`plan-20260916.md`](plan-20260916.md) | B（Mega agent capture-push） | 未启动 | CAP-01..03 `pending`，仅可 zero-raw-persistence 重核；`DEP-ACF-CAP` 的 ACF 侧必要条件已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；CAP-04..07 `blocked`，须独立 security/privacy RFC 与新卡 |
| [`plan-20260913.md`](plan-20260913.md) | A（LR-09 FastCDC Media） | **实施中** | FL-00/01/07/02/06/03/04/05 全部 `done`/`complete`（发布 `v0.30.0`..`v0.30.5`）；前置 plan-20260907 已收口（`DEP-FL-04` 满足）；仅剩计划级收口门（默认+fastcdc 全量、Codex/Claude 双 review 与文档/网站同步）未勾 |
| [`plan-20260912.md`](plan-20260912.md) | B（memory boundary） | **已收口** | MB-01..MB-05、MB-07/08、MB-10/11 全部 `done/complete`（v0.23.37/.39/.40/.41/.42/.43/.44/.45/.46，各卡 ER-14 全量绿、D-MB-STD 远端 evidence 绿）；MB-06/09/12 墓碑 `done`（无对应子命令）；EX-MB-01/02（G-03）与 EX-MB-03（ER-07 签名）已登记；使用者 2026-09-21 双评审豁免已登记；完成判据全数勾选 |
| [`plan-20260911.md`](plan-20260911.md) | B（Pi capture） | 未启动 | DATA-CAP-20261019：11卡 pending；PI-00 先核系统版本/模型；PI-08 保留 thinking/thoughtSignature；云围栏与 identity/schema 顺序不变，旧 R5 为历史；本次修订实际审查见计划 |
| [`plan-20261018-hermes-capture.md`](plan-20261018-hermes-capture.md) | B（Hermes capture） | 未启动 | DATA-CAP-20261019：10卡 pending；HM-00 选含 thinking 的 export/文件/一致只读 SQLite；HM-08 最小 live 桥不要求新沙箱；真实非空数据门待验，旧 R5 为历史；本次修订实际审查见计划 |
| [`plan-20261018-grok-capture.md`](plan-20261018-grok-capture.md) | B（Grok capture） | 功能暂停；GK-06本机通过，GK-07接续 | GK-00..GK-05 `locally-accepted`，Acceptance 仍空。v0.30.63 CodeQL 39226039130 与 release 39226204632 成功；Check 39226039150 被 `b57fdb6` 取消。网站 `cf` `27a83d1` 已推。 |
| [`plan-20260910.md`](plan-20260910.md) | 横切（数据库迁移作用域） | **已收口** | MIG-00..MIG-06、MIG-R01..R03 全部 `done/complete` |
| [`plan-20260907.md`](plan-20260907.md) | 横切（BLAKE3 object format） | **已收口** | B3-00..B3-17 全部 `done`/`complete`（`v0.23.68`→`v0.29.0`；REL-B3-01/02 家族经 B3-15/B3-17）；最终 Codex/Claude R40 双 `PASS`（0×P0/0×P1）；GC-B3-01/GC-B3-02 收口守卫绿；`DEP-B3-05` 已满足，下游 FL-00 解锁；无 LR 编号 |
| [`plan-20260906.md`](plan-20260906.md) | 横切（安全扫描） | 未启动 | SC-01..SC-07、SC-CLOSE 全部 `pending` |
| [`plan-20260905.md`](plan-20260905.md) | B（Claude hooks/reasoning） | CC-00..CC-08 九卡 done / complete | v0.30.65（`262c700`，release 39285212623）与 v0.30.66（`17a6766`，Check 39314279961、CodeQL 39314279414、release 39347223159）均 success；install/Homebrew/stable manifest 与网站已核；Claude 实现审查 R1 FAIL、R2 PASS |
| [`plan-20260904.md`](plan-20260904.md) | B（RG基础 / Codex capture） | RG/scoped沿用已交付；CX待执行 | DATA-CAP-20261019：CX-00 source/模型设置本地审计完成、Claude R2 PASS；CX-12语料locally-accepted/Claude PASS，其余27张 CX pending；thinking 原文及 encrypted_content 普通保留，native 缺证不阻塞获取；旧 R5 为历史；本次修订实际审查见计划 |
| [`plan-20260903.md`](plan-20260903.md) | A（LR-05 merge） | 实施中（收尾） | MG-01..MG-21 卡片全部 `done/complete`；最终计划收口与 deferred 差异仍待完成 |
| [`plan-20260902.md`](plan-20260902.md) | B（OpenCode ordinary capture / safe partial） | **macOS范围完成：66/66=100%**；库存66/68=97.06% | v0.30.52实际发布/安装与本轮五卡C/D已关闭；两原延期、六withdrawn非PASS、SourceA未证；COV26/26不变。最终完整全量实际通过，失败/重试/skip原据保留。 |
| [`plan-20261013-coverage-schema.md`](plan-20261013-coverage-schema.md) | B（父计划DEFER-OG25-COVERAGE-V2的独立schema-v2前置） | **任务卡26/26 done/complete；计划级除终止D组后补外已满足**（本次签推生效）。v0.30.50 F `deabb37`/release38849003385/official安装及legacy2epoch0→2实际PASS；DEP13complete，父60/72正式/source62/72。 **收口前状态历史：** **当前：parent72/formal59/source62，COV26/source25/formal0；repair189/cff290已source-qualified，repaired50 T2 actual10219PASS/2slow23skip/0FAIL0retry0flaky0LEAK；release CODE R1/R2 FAIL；R5（CHANGELOG顶节＋f6ff基线）范围修订待审/采纳；旧215dc824 full不复用，需新bumped50 full，不授予发布C-D。以下原单元为历史快照：** **实施中：26卡；COV2-17实际10/10＋唯一真实visible-only、109reader/fmtstrict及Claude PASS，最终事实复核/签推待；source当前24，actual交付后25/26=96.15%且DEP13source-ready；formal0/26家族C-D；下一COV2-REL-01** | **以下原日期交付为历史收据，旧13/26、14/26及“保持”不代表当前分子；当前交付条件以本文件首条和本卡末尾为准。** 初始19卡由 signed main `c94fd8dd60d8d9fa670b2682bf7b9979bae91548` 采纳，历史分母保持；已签 G-09 细化为20 implementation、2 audit、1 patch release；已采纳 COV2-23/24 后为历史25卡；本次拆出 COV2-25，现为23 implementation、2 audit、1 patch release（26卡）。COV2-01/13/02/07/14/03 六卡已签名main、本机A/B与唯一ClaudePASS，此前COV2-14 signed+DCO main `4558af64c09e63c4716fa68595776a8044f66cc7`，最终full 10069/10069 exit0/raw `2849dcf7e2105ed1da5e5d3b0333374e2590ac909c1f478ef8ba68966e86aec4`、唯一Claude PASS/raw `2d1e5fb7425abc9aa2802db241213811dfcd6c891410e9853e554c5fcb806966`；COV2-24 在历史25卡口径签名后 source-ready **12/25=48.00%**、formal **0/25**；当前COV25已signed交接后26卡口径 source-ready **13/26=50.00%**、formal **0/26**，家族C/D仍待。COV2-03 已完成源码交接，此前23卡三文件 G-09 规划协调已 signed main `eaa43ec13f6c50212f0b3c2fa8a51799ec74bff5`；本卡当前源码 AC1–8 的五项 VER、上述五项 ER-04 与两项旧库缺表回归均各 1/1，fmt exit0，strict Clippy exit0；full-r3 失败已修但不计 PASS；最终双 env full **Summary [1618.814s] 10078 tests run: 10078 passed (14 slow), 23 skipped**/raw SHA `c49dd3c33652d55541a0e4c0269c817460b9109cf261eab5271d90760a49e8a9`、exit0，唯一最终 Claude Code PASS、P0/P1/P2=0、P3=5/raw SHA `9ef25f92615eefcf2019212a306cfa7542ef416b693286bb99111006b3ea6df7`，signed+DCO 源码/文档 main `d717e42e0d1ec75e6ae576b21242fd1519040abd` 已 Push、remote 同 SHA；五项非阻断 P3 见卡内。COV2-19 8/8 AC、5/5 VER、16项继承及 replay/架构守卫本机通过，双 env full 10084/10084 exit0（15 slow、1 leaky、23 skipped；raw SHA `20378e04b5c585c954421780520a6a62ebbf1f6c7ba0336db40dd76541058e43`），唯一 Claude PASS/raw `64068902908f4c521e380dc7e5cf593134eaa99697de208344b65f75b3c3caa8`，signed+DCO main `354ff768c406bdad74607da62002bff810147c24` 已 Push、remote 同 SHA；COV2-20 修订后 7/7 AC、3/3 VER 与 inherited v1 精确 1/1、双 env full 10087/10087 exit0（4 leaky、23 skipped），唯一 Claude PASS/raw `6f3381d9061e72afebfe32f9714331ada567665d33eb0b045782fd06646667a5`，signed+DCO main `aa0ea018ddd044f09d62a88428e812a359cef8ed` 已 Push、远端同 SHA；COV2-21 四项 VER 与五项继承精确测试各1/1、strict/full 与唯一 Claude PASS，signed+DCO main `6b24d797583afe1c0265f6e9fcbd364566671167` 已交接；COV2-23 8/8 AC、4/4 VER、四项继承与两项架构守卫各1/1，修订后双 env full 10095/10095 exit0、唯一 Claude PASS，signed+DCO main `d977d13c5e23111b93d78477b9e1a70015a111b8` 已与远端同 SHA；首轮 full 10093/10095、2 failed 保留不计绿；COV2-04 五项 focused 各1/1、fmt/strict、唯一 Claude PASS/raw `3a9749ad4679e8e038eb7f3d5d09e36e0e367de4411e4f5848371c55d6ad5aac`，signed+DCO main `03b10f887368a3db5206589f5dd6947432ffe26c` 已 Push 且远端同 SHA，ER-13 `none` 未独立跑 full；COV2-24 完整签名收据：signed main `c56de683acf71d7327436eef7ef9185d97dcff27`/remote 同 SHA，八路径 manifest `cd43b9af3980c8f3e90ce5751df7855ebaf1f604e2803130f0d32d9556b46e98`、唯一 Claude PASS raw `a6d467bcdc33c42fff3c45f55681ebbaefa213d96cfd59a8b901c5d3dff942f7`、T-5 10103/10103 exit0/raw `2908fff258247c857ae857a4759fcde9b925c212cc5912fa373ece16b5373029`，七项 inherited exact 1/1；当前提交安全 CI 仍运行；历史开工快照曾为“COV2-25 AC/VER 全 UNRUN，COV2-22/08 未实施”；现COV2-22已source交接、COV2-08开工/当前验收待；COV2-20 旧 CI 因新提交取消，Windows/network/CodeQL 先前成功，offline-core 未完成。COV2-19 开工前 G-09 已在卡内 Prestart receipt 核实。0 matched/helper-only/skip 均非 PASS。当前22审计名（21继承+1当前格式替代，另1旧legacy FAIL计划外延期）只在macOS本机非skip验收；真实Source A另由父计划证明；COV2-REL-01稳定C/D后方可将 `DEP-OG-13` 记complete；历史父计划 **55/70=78.57%**（当前口径见页首）。 ；本次G09三卡顺序COV2-25→FIX-OG-NATIVE-IMPORT-01→OG-IDLE-ERROR-029-01→OG-UPGRADE-029-01→COV2-22，新增卡A/B/C-D均UNRUN；REL-OG-TEMP-049操作本身不增卡。  历史G09当时：DEP-COV2-029-BASE只消费父namespace＋自动capture/checkpoint/export最终source；DEFER-OG29-PARTIAL-IMPORT不设入边、不解除COV2-05/11/18原import oracle。该历史时点COV source13/26、formal0/26；当前分子见本行状态栏与页首。 |
| [`plan-20260901.md`](plan-20260901.md) | 横切（SB-01 pkt-line fail-closed） | **已收口** | 全部卡 `done/complete`；最新 v0.22.47 |
| [`plan-20260830.md`](plan-20260830.md) | 横切（SB-02 sandbox export） | **已收口** | SBX-01..05 `done/locally-accepted`（发布步按 DEFER-SBX-06 延后）；ER-13 收口门绿 |
| [`plan-20260827.md`](plan-20260827.md) | 横切（SB-04 测试并行度） | **已收口** | NP-00..05 全部 `complete` |
| [`plan-20260825.md`](plan-20260825.md) | B（历史：Code provider / RT-01 后续）+ 横切测试 | **已收口** | **历史完成/封存**：PS 产品轴已随 plan-20260920 拆除，Code 专属 DEFER 已墓碑化；TA 测试并行度交付保持有效，`DEFER-PS-06/07` 仅可作为另立通用计划的候选 |
| [`plan-20260824.md`](plan-20260824.md) | B（历史：RT-01 延后项收口） | **已收口** | **历史完成/封存**：DF-01..09 全部 `done/complete`、v0.22.0 已发布；其 Code 产品面与专属 DEFER 已随 plan-20260920 拆除/墓碑化 |
| [`plan-20260822.md`](plan-20260822.md) | A（LR-02/LR-03 Operation Log v2） | 实施中（PR #503 收口） | OL-01..13、CH-01..04 `done/complete`；OL-14 **已取消**（`web/` 拆除，2026-09-20）；OL-15A `done/complete`，OL-15 `done/remote-pending`（等待 compat-offline-core） |
| [`plan-20260821.md`](plan-20260821.md) | A（UP-01） | **已收口** | 客户端与 CI 全部落地；closeout `00bc815`；DEFER-02..06 残留 |
| [`plan-20260819.md`](plan-20260819.md) | C（MEM-01/02 Memory，R30 历史方案） | **已取代**（2026-10-07，DM-00 正式退役） | R30 r37 于 2026-09-22 对冻结版 sha256 `4fbaf4bd…` 取得 Codex/Claude 同版双 PASS；25 张 M2 卡全部 `pending`、未实施。**MEM-01/02 已由 [`plan-20260926.md`](plan-20260926.md) 承接，M2-01..M2-16E 全部正式退役（DM-00 已登记）**，`src/internal/ai/memory/**` 与 `libra memory` 仅由 plan-20260926 拥有（DEP-DM-05）。R30 评审日志保留为历史事实；旧版 PASS 不代表新计划的评审门通过。 |
| [`plan-20260818.md`](plan-20260818.md) | B（deepseek-harness bridge） | **已收口** | LB-01..07 全部 `done/complete`；protocol v1 20-method 全实现 |
| [`plan-20260729.md`](plan-20260729.md) | A（CT-01） | 实施中（收尾） | CT4-01 发布卡已执行（v0.21.21）；**CT3-07 `blocked`/已延后**；完成判据未全部勾选 |
| [`plan-20260715.md`](plan-20260715.md) | B（历史：RT-01 Code Web-only） | **已收口** | **历史完成/封存**：W0..W6 + W5-01/WIO 完成证据保留；Code 产品面由 plan-20260920 拆除，DEFER-01..08 已关闭/墓碑化，DEFER-09/10 已完成关闭 |
| [`plan-20260714.md`](plan-20260714.md) | A（UP-01、LR-01）+ 横切 | **已收口** | Part A 迁移至 plan-long；Part C W1..W4 勾选；Part D 残留由 LR 承接 |
| [`plan-20260713.md`](plan-20260713.md) | B（LR-06/07/10 捕获前置） | **已收口** | DR-BASELINE..DR-07 全部实现并收口 |
| [`plan-20260708.md`](plan-20260708.md) | A（LR-04/05/09 相邻基础） | **已收口** | 41 项主线全部实现；只保留历史记录，活跃残留另行排期 |

### issues/ 下的计划（Issue 驱动的 Git 对齐修复计划）

`issues/` 目录每份文件对应一个 GitHub Issue，是独立的可执行计划。`474`/`477`/`486`/`497`/`498`/`574`/`577`/`582` 已收口或关闭、`476` 执行中（用户 2026-09-20 覆盖：执行期间不调用 Codex/Claude 评审）。其余多为设计计划。状态列取值同上。

| 计划 | Issue 主题 | 状态 | 任务卡 |
|---|---|---|---|
| [`issues/470.md`](issues/470.md) | 工作树物化丢失可执行位与 mode 变化检测 | 未启动 | FM-01/02/05（3 卡） |
| [`issues/473.md`](issues/473.md) | `init` 与 Git 对齐 | 未启动 | IN-01..IN-12（12 卡） |
| [`issues/474.md`](issues/474.md) | clone 浅克隆完整性、bundle 源、bare 与 mirror 对齐 | **已收口** | CL-01..CL-15 全 `done`/`complete`（`v0.23.47`–`v0.23.54`、`v0.23.57`–`v0.23.63`）；closeout `v0.23.64`（#569）；DEP-CL-07 完成（#474 CLOSED） |
| [`issues/475.md`](issues/475.md) | `config` Git 兼容参数层对齐 | 未启动 | CF-01..CF-15（15 卡） |
| [`issues/476.md`](issues/476.md) | 工作树命令族与 Git 对齐 | **实施中** | WT-02 `v0.23.29` / WT-04 `v0.23.30` / WT-08 `v0.23.31` / WT-09 `v0.23.32` / WT-10 `v0.23.33` / WT-11 `v0.23.34` / WT-01 `v0.23.35`（`done`/`remote-pending`）；intent-to-add WT-05..WT-07 已 `done/complete`（WT-06/07 于 2026-10-18 落地）；WT-03 `done/complete`（2026-10-18） |
| [`issues/477.md`](issues/477.md) | 历史改写命令族与 Git 对齐 | **已收口** | HF-01..HF-31（31 卡）全 `done/complete`，聚合发布 v0.22.49；子 issue #495；2026-10-07：477b 后续（#522/#523/#525/#526/#527/#533/#536，HW-01..HW-07 全部 `pending`，未启动）并入该文件「后续计划：#477b 新缺陷跟进」一节，原 `issues/477b.md` 已删除 |
| [`issues/478.md`](issues/478.md) | log/show/diff/grep/blame/notes/reflog 命令族 | 未启动 | LG-01..LG-25（25 卡） |
| [`issues/479.md`](issues/479.md) | plumbing 与 Git 对齐 | 未启动 | EC-01、RV-01/02、UI-01..03、DF-01、SR-01、UR-01（9 卡） |
| [`issues/480.md`](issues/480.md) | remote/fetch/pull/push/credential/rerere 对齐 | 未启动 | HP-01..HP-17（17 卡；HP-17 为 SSH 传输入口 host-key 校验，issue #560） |
| [`issues/481.md`](issues/481.md) | 维护与杂项命令族对齐 | **实施中** | MX-01、MX-02、MX-03、MX-06 `done`/`complete`（实现完成，2026-10-20；C 组 bump 与 D 组 release 证据待用户授权）；MX-04、MX-05、MX-07..MX-16 待开工 |
| [`issues/483.md`](issues/483.md) | `count-objects` 与预览命令零对象写入 | 未启动 | CO-01..CO-04（4 卡；CO-03/04 受 DEP-CO-04 / DEP-CLI-mirror `src/cli.rs` 实际写者串行约束） |
| [`issues/486.md`](issues/486.md) | upstream ahead/behind 计数 | **已收口** | AB-01 `done/complete`（v0.22.31，已关闭） |
| [`issues/487.md`](issues/487.md) | 本地 Git 转换挂死与中断恢复 | 未启动 | IG-01..IG-05（5 卡） |
| [`issues/488.md`](issues/488.md) | `grep` 的 `--exclude-standard` 与子目录作用域 | 执行中 | GR-01、GR-02 `in-progress`（实现+验证通过，发布阻塞） |
| [`issues/490.md`](issues/490.md) | skip-worktree 索引位与 `add` 稀疏路径诊断 | 未启动 | SW-01..SW-07（7 卡；SW-06 已迁至 plan-20260918） |
| [`issues/496.md`](issues/496.md) | 本地路径 clone 停住（Fetching objects 0% CPU） | **已收口（v0.30.28）** | CLH-01 `done`/`complete`（诊断）、CLH-02 `done`/`complete`（修复 + 回归守卫 + 文档/版本面/发布；收口二维均满足——**②** 全量 **9522/9522 全绿**（上游 #612 修复 `opencode_export` bwrap probe 与 `history::cleanup_helper_guard`）＋ **③** 后续发布 **`v0.30.28`**（release 成功，4 平台）收口 `v0.30.2` 的 ER-14 门——见 `ENV-496-01`/`EX-496-01`）；根因 ADR-CLH-01（`local_client.rs` 旧 `encode_pack_bytes` 先喂满有界输入通道再排空有界输出通道的循环等待）；已复用 `pack_writer::encode_pack_bytes`，新增 pack_writer 单元回归 + clone 集成回归；文档/`COMPATIBILITY.md` 同步，`../libra-backend`（Libra 仓库，`cf` 分支，提交 `905d7a2`）已交付（`DEP-496-03` 已满足）；版本面/CHANGELOG 标为 `0.30.1`（GitHub 实际 tag `v0.30.2`）。Codex review：R1..R14，**R13 `PASS` + R14 最终确认 `PASS`**；**计划 #496 已于 2026-10-05 正式收口（`v0.30.28`）**。 |
| [`issues/497.md`](issues/497.md) | 删除最后一个被跟踪文件后 commit 报 nothing to commit | **已收口** | CD-01..CD-04 全部 `done`/`complete`；`v0.27.2`（PR #581 squash merge `859d7fb`；PR head `base.yml` 7/7 + CodeQL 绿；`release.yml` 8/8 + stable manifest `0.27.2`）；ER-07 签名例外 EX-CD-01（仓库 vault 不可 unseal，操作者 2026-09-24 裁决维持未签名发布） |
| [`issues/498.md`](issues/498.md) | `tag <name> <commit>` 不接受显式目标提交 | **已收口** | TT-05 `done`/`complete`（v0.30.22，PR #602 `a2f1c34`，release `37055840149` 8/8；网站 `cf@a976e4c`）；TT-02 `done`/`complete`（v0.30.23，PR #604 `52bf7d2`，release `37087459978` 8/8；网站 `cf@e908d01`）；M3 收口门 2026-10-03 06:19:24 UTC 全绿（CI 口径：8754/8754 passed / 4 skipped）；完成判据全勾选；`issues/478.md` DEFER-04 已登记由 TT-02 交付；#498 CLOSED；残留 DEFER-TT-01..08（含 DEFER-TT-08 `for-each-ref`/`describe` 嵌套 peel）；TT-01/03/04 废弃，TT-06/07 合并回 TT-02；EX-TT-01 已批准 |
| [`issues/468.md`](issues/468.md) | Data collection and refinement | 未启动 | 2026-10-07 整份迁 v2.12 重写（现采用模板 v2.14），经 Codex R1..R77 七十七轮修订（R77=FAIL：P0=0/P1=7/P2=3，已全部处置——R77-P1-1..7（A.13 分支合并、ADR 规则、SHA 命令、P 过滤、Unicode 空白、卡 12、VER 19）与 R77-P2-1..3（A.11 token、stdin 描述、issues/500 同步）；见 468 修订历史）R76=FAIL：P0=0/P1=7/P2=5，已全部处置——R76-P1-1..7（id/digest 空/超 @ 违规、A.11 祖先判定、SHA 双向差集、Unicode 空白、ADR 规则、12/119、差异 AC/VER 19）与 R76-P2-1..5（单体 token、stdin 描述、计数 NUL、episode 锚点、issues/500 同步）；见 468 修订历史）R75=FAIL：P0=0/P1=6/P2=2，已全部处置——R73-P1-6/P1-7、R74-P1-2、R75-P1-1..3（SHA 对账双向差集、12-item/119、身份 SQL 校验、脱敏 range(6)、id 不透明、映射差异登记）与 R73-P2-2/R74-P2-1（收口标记精确前缀、stdin 描述）；见 468 修订历史）R74=FAIL：P0=0/P1=7/P2=1，已全部处置——R73-P1-2/P1-6/P1-7/P1-9/P2-2 与 R74-P1-1/P1-2（时间戳 NUL、SHA 对账可执行、A.10 上限 12/EX-DC-02 119、DC-02 35、收口标记绑 HEAD、脱敏 range(5)、A.14 身份空白校验）与 R74-P2-1（stdin 描述）；见 468 修订历史）R73=FAIL：P0=0/P1=10/P2=5，已全部处置——R73-P1-1..10（A.13 脱敏 3 步/拒 NUL、A.14 身份 fail-closed、A.2 复用状态行、pre-reg 标签计数、SHA 集合对账、A.10 清单上限 12/EX-DC-02 119、卡字符串对齐、DC-02 35、DC-10 推导）与 R73-P2-1..5（A.11 缩进 ≤3/收口标记绑 HEAD、A.1 telemetry 锚点、A.15 全父检查、移除孤儿片段）；见 468 修订历史）R72=FAIL：P0=0/P1=12/P2=3，已全部处置——R72-P1-1..12（A.15 rots set -e、终态复验 fail-closed、A.13 脱敏 last-@/stdin、A.13 时间戳分支、DC 卡锚点无关、EX-DC-01 算式清理、枚举开放判据、A.11 GO 连续子句、plan_shas 全提交、EX-DC-02 117/17、轮换拆分推送、A.2 允许文件干净）与 R72-P2-1..3（A.11 缩进、pre-reg 精确、DC-09 ADR-CS-08）；见 468 修订历史）R71=FAIL：P0=0/P1=11/P2=4，已全部处置——R71-P1-1..11（A.15 latestrot set -e、终态复验 fail-closed/非空、M0 hex 校验、A.13 URL 脱敏 authority-bounded、时间戳统一毫秒、DC-06 锚点无关、EX-DC-01 算式重写、枚举开放、pre-registration 恰一条、A.11 GO-only、A.15 轮换链线性）与 R71-P2-1..4（A.11 缩进、轮换协议 cur~1、须绿集合含 evidence-only、A.10 信封字段）；见 468 修订历史）R70=FAIL：P0=0/P1=13/P2=5，已全部处置——R70-P1-1..13（M0 门 fv 作用域、终态复验 fail-closed、EX-DC-01 分值 57/56/32/32、A.11 GO token、A.6 整行冻结、A.15 最新轮换目标绑 M0、A.15 cur~1 fail-closed、A.2 diff --no-renames、A.13 id 凭据脱敏、v1.x 追加变更向前兼容、逐卡证据落点、pre-registration 恰一条 done/complete、A.2 tr/grep 失败检查）与 R70-P2-1..5（残留片段、轮换协议文本、A.11 唯一表、阶段参数拒、DC-07 消费者状态）；见 468 修订历史）R69=FAIL：P0=0/P1=8/P2=5，已全部处置——R69-P1-1..8（A.15/M0 围栏感知+祖先校验、A.15 PASS 绑定到 cur 父提交、A.2 状态退出码、A.9 判据钉死、A.11 重启条件收紧、A.8 成功+违规≥分母、A.6 括号矛盾注释拒绝、A.6 描述 sync_revision 修正）与 R69-P2-1..5（EX-DC-01 分值 DC-05 55/DC-10 54/DC-06 32/DC-13 32、DC-08 夹具去 sync_revision、A.8 事件计数、A.11 分隔行、A.15 引用校正）；见 468 修订历史）R67=FAIL：P0=3/P1=12/P2=0 已处置；R68=FAIL：P0=1/P1=9/P2=3，已全部处置——R68-P0-1 DC-01 M0 互核行首锚定、R68-P1-1 `sync_revision` 谓词永假（该列为 `INTEGER NOT NULL DEFAULT 1`，非 NULL；where_sql 移除 `sync_revision IS NULL`）、R68-P1-2 副本私有权限、R68-P1-3 A.13 `lproj` 初始化、R68-P1-4 A.2 `-z` 引号路径、R68-P1-5 A.9 判据冻结常量、R68-P1-6 A.6 语义行整值冻结、R68-P1-7 A.8 成功+违规==分母、R68-P1-8 A.11 完整重启条件、R68-P1-9 补 `## 已决议设计决策` 标题；R68-P2-1..3；见 468 修订历史）R67=FAIL：P0=3/P1=12/P2=0，已全部处置——A.15 M0 行计数与占位行、A.9 判据标记围栏外识别、A.13 归一化 CASE gate、A.2 `--no-renames --untracked-files=all`、A.6 语义行冻结、A.10 A.5 重验、A.11 表行定位、A.1 file:line 锚点、A.14 输出/数值校验、终态复验固定副本、#500 锚点 :265/:306/:311、快照时间）：十三张活动卡（DC-01/DC-11/DC-02/DC-12/DC-06/DC-13/DC-08/DC-05/DC-10/DC-07/DC-09/DC-14/DC-04，全部 audit/docs，无实现卡、无发布切片；R28 起 DC-09 拆为 DC-09+DC-14）+ DC-03 取消墓碑（实现移 DEFER-DC-01，重启条件 = DC-07 判 GO 且另立实现计划）。全部 `pending`；M0 前置 = Codex 计划评审 PASS **且 EX-DC-01（G-03 清单型豁免，R65 起范围扩为九卡：覆盖 DC-01/DC-11/DC-02/DC-12/DC-05/DC-10/DC-06/DC-13/DC-08 的清单型分子当前 53/56/35/196/57/56/32/32/52 与 DC-12 VER 39，豁免上界 60/65/36/250/62/60/32/33/60 与 48——R41/R48/R51/R52/R54/R56/R57/R58/R60/R62/R63/R65/R66 定稿，原 2026-10-07 09:18:18 UTC 批准覆盖 31/30/27/24/25）**待 genedna 重新显式批准**，且 EX-DC-02（DC-09/DC-14 清单型 AC 豁免，最坏 119/20）**待 genedna 显式批准** |
| [`issues/500.md`](issues/500.md) | Feature：Centralized Storage for Libra Statistics Data | **已收口（NO-GO，v0.30.29）** | **2026-10-05 20:10:35 UTC**：本地 trunk compose 部署实测资格——CS-16/19/17 均为 `not-qualified`（storage-only read 面无 ACL：anonymous set/manifest/resolve/object GET/HEAD 读为 200；缺少独立 cross-repo/team principal，identity-specific probes 明确 `not-run`；且 pin `cead1d8` 无 committed-set revoke 路由，`DELETE /sets/{id}` 405）→ CS-20 aggregate=`not-qualified` → CS-00 判定 **NO-GO**；维护者 2026-09-21 评论只确认 Mega2 为后端，未点名 `agent_session`（DEP-500-02 数据集核准未满足，已发征询；GitHub issue #500 保持 OPEN，不把计划 NO-GO 收口等同关闭 issue）。CS-10/11/12/13 延入 DEFER-CS-02/03/07/10（重启条件：维护者在 #500 确证数据集且新 CS-00b 为 GO）；CS-18 登记 defer+outgoing handoff（DEP-500-14→genedna）；CS-14 closeout 收口；**2026-10-05 收口 patch 发布：`v0.30.29`**（PR #613 squash merge `cbded577`；tag `v0.30.29` 指向 release commit `370dc94` 且是 main 祖先；版本面/`Cargo.lock` 一致；恢复 `tests/compat-ledger/SURFACES.gen` 模式为 100755；本地 `cargo +nightly fmt --all --check`、clippy 与 `nextest --all --no-fail-fast --retries 2`（umask 0022）通过，9525/9525 passed（1 flaky 重试转绿）；D-1 base.yml PR #613 head `ade89a1`，run `37344599122` attempt 2 全绿（首次 attempt 的 OpenCode bridge marker 断言失败，retry 通过）；D-2a CodeQL PR run `37344599037` 全绿；D-3 release run `37336809254` 8/8 全绿；D-2b merge-to-main CodeQL run `37354926375` 全绿（merge SHA `cbded577`，actions/Rust jobs 均 success）。**计划卡状态：CS-00/14/15/16/17/18/19/20 `done` / `complete`；CS-10/11/12/13 正式 `deferred` / `not-applicable`。 |CS-00 AC/VER=17/20、14/20；CS-10/11/12/13=17/39/32/41@EX-01（CS-13 raw checklist 42，含 ER-06a）；CS-14=19/20、14/20 audit；CS-15=11/12、11/12 release；CS-16 audit AC/VER=20/20、20/20；CS-19 audit AC/VER=16/20、16/20；CS-17 audit AC/VER=24/20@EX-02、24/20@EX-02 audit（P01=not-qualified、P02=pass、P03=pass、P04=pass、P05=not-run、P06=not-run、P07=pass、P08=pass、P09=pass、P10=pass、P11=pass、P12=fail、P13=not-run、P14=not-run、P15=not-run、P16=not-run、P17=not-run、P18=not-qualified（pre-revoke control pass，post-revoke not-run）、P19=fail、P20=not-run、P21=not-run、P22=not-run、P23=not-run（pre-revoke control pass，post-revoke not-run）、P24=not-qualified；EX-02 清单豁免）；CS-20 audit AC/VER=17/20、17/20；CS-18 audit AC/VER=17/20、17/20 audit。 |
| [`issues/574.md`](issues/574.md) | 只讀查詢走寫操作邊界（`branch -l` 25s）與 log／rev-list 全量遍歷 | **已收口（BRL-01..05 done/complete；DEFER-BRL-01..03）** | BRL-01/BRL-02 `done`/`complete`（v0.30.9／v0.30.10，PR #587 `22b9534`，release 36843865958 8/8）；BRL-03 `done`/`complete`（v0.30.11，PR #588 `a4121a8`，release 36866068892 8/8；网站 cf@961ea3f）；BRL-04 `done`/`complete`（v0.30.12，PR #589 `f14841c`，release 36879702000 8/8；网站 cf@83d620c）；BRL-05 `done`/`complete`（v0.30.13，PR #590 squash `ce2411d`，release 36910331385 8/8；量测 AC ~15.24/15.68/14.96s under `LIBRA_READ_POLICY=local`）；完成判據全勾選；DEFER-BRL-03 executable-bit 基线已红（branch 查詢分類 → tag/remote/reflog/notes 查詢分類 → 快照 stat 短路；log 日期優先 walker → rev-list 下推）；R1-R3 雙評審 FAIL；R4 Claude FAIL、Codex 中斷；R5 兩者 FAIL；R6 Codex FAIL＋Claude PASS；R7 Claude FAIL、Codex 中斷（模型不再支援）；R8 兩者 PASS；R9 兩者 PASS；R10 Codex FAIL（golden 快照須依 v2.12 內聯）＋Claude PASS；R11 兩者 PASS（模板升 v2.12 後合規確認）；R12 兩者 PASS（結案確認）；EX-BRL-01 覆蓋 BRL-01..05（G-03 門族豁免，一門＝一完整具名測試函式＋`-E 'test(=command::…)'` 全枚舉＋零匹配失敗保護＋可執行組內數量比較，分子 33/55/28/10/18；118 個 `command_test` 門皆有 `command::` 前綴）；`BRL-02 -> BRL-03` 跨鏈串行邊（G-10）；census `--lib` 矩陣守衛；`persist_files` 持久化短路；ADR-BRL-02/04 語義修訂；`RevListArgs` 全量歸屬表；tag `no_column` 漏檢同源修正；輸出等價以 inline 期望值對拍（依 v2.12 不另存證據檔）；網站八頁與 `_compatibility.md`／integration scenarios 補齊（評審證據內聯於計劃文件）；repair 鎖殘留經 GC-01 判定已緩解（DEFER-BRL-01）；網站文件受 DEP-BRL-02（`../libra-backend` 不在本 checkout 旁）阻塞發布 |
| [`issues/577.md`](issues/577.md) | SSH 公钥认证失败误报 pkt-line 协议错误与配置文档 | **已收口** | 2026-09-27：SA-02/SA-01 均为 `done` / `complete`；PR #578 合并为 `5eb833f9`，v0.24.1 发布与 8/8 release jobs 全绿；网站 `cf@667d7da8`、Worker `c7011920` 和七个生产页验证完成；Issue #577 CLOSED |
| [`issues/582.md`](issues/582.md) | Git 相容的互動式 SSH 主機金鑰確認 | **已收口** | HKT-00 `done/complete`（no-release 設計與 HP-17／DEFER-07 移交）；HKT-01 `done/complete`（首次 clone 的 host-key policy cascade）；HKT-02 `done/complete`（受限 human-terminal unknown-host confirmation）；聚合发布 `v0.29.1`；全量 nextest 8271/8271 绿

---

## 二、未启动的计划与卡

以下计划中，`plan-20260921` 已于 2026-09-24 完成并发布为 `v0.23.65`（15/15 卡 `done`/`complete`，见下表本行注）；其余计划均未进入实施/发布阶段（多数为设计态、全部 `pending`；`plan-20260919` 已全部 `done`/`complete`，GCX-01..04 含 GCX-02/03/04 的 v0.30.32/33/34 交付见下表本行注）。按建议优先级排列，优先级依据为跨计划依赖（`DEP-*`）与产品路线（`plan-long.md`）：

| 计划 | 全部待执行卡 | 开工前置条件 |
|---|---|---|
| [`plan-20261019-agent-mcp-server.md`](plan-20261019-agent-mcp-server.md) | MC-01 `done`（0.30.68）；MC-22 `done`（0.30.69）；MC-16 `done` / `complete`（`v0.30.71`）；MC-19 `done` / `complete`（`v0.30.72`）；MC-29 `in-progress`（本地门绿，发布候选 `0.30.79`，D 组未绿；`v0.30.76` 的 P1 已前滚）；MC-02..MC-08、MC-10..MC-15、MC-17..MC-21、MC-23..MC-41、MC-43 pending（MC-09 撤销，MC-42 并入 MC-06） | 计划级 review：Codex R64 PASS（R66 差异确认 PASS）；DEP-MC-01（grok 计划 migration.rs 写集）、DEP-MC-03（并发未提交改动）按卡核对 |
| [`plan-20260924.md`](plan-20260924.md) | ACF-01..20 与 FIX-ACF-01 `done`/`complete` | **已收口**（ACF-09 `Lifecycle=done`、`Acceptance=complete`）；`DEP-ACF-MIRROR` 已交接 |
| [`plan-20260926.md`](plan-20260926.md) | DM-00..DM-13（14 卡；`DM-00` `done`/`complete`，`DM-01` `done`/`complete`，`DM-10` `done`/`complete`，`DM-02` `done`/`complete`，其余 pending） | R11 字面 `VERDICT: PASS`（`P0=0` / `P1=0`）。`DM-00` 已于 2026-10-07 `done`/`complete`（#456 取材清单、远端事实冻结、plan-20260819 退役）。`DM-01` 已于 2026-10-07 `done`/`complete`（迁移 A `2026092901_memory_core` 交付，REL-DM-01 独立发布至 v0.30.31 / PR #614 / merge `603bad3` / release run `37588423715`；本地 nextest（umask 0022）9549/9549 + D 组 base.yml/CodeQL 全绿；ER-05 评审待记录）。`DM-10` 已于 2026-10-07 `done`/`complete`（迁移 B `2026092902_memory_path_search` 交付，REL-DM-01 独立发布至 v0.30.35 / PR #618 / merge `c384bc7` / 版本面补丁 `b51f5c3`；D 组 base.yml/CodeQL + compat-offline-core 全绿；ER-05 评审待记录）。`DM-02` 已于 2026-10-08 `done`/`complete`（`src/internal/ai/memory/` commit/change 派生实现，REL-DM-01 独立发布至 v0.30.37 / PR #620 / merge `125657b`；D 组 base.yml/CodeQL + compat-offline-core 全绿；含 `parse_repo_oid`（object_format）修复；ER-05 待记录）。`DEP-DM-06` 正式 handoff 已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）完成；DM-05 仍 `pending` 并须在开工日按 ER-02 重核（经 DEP-DM-06） |
| [`plan-20260905.md`](plan-20260905.md) | CC-00..CC-08 done / complete | 见计划[最终 C/D 与完成](plan-20260905.md#cc-closeout-20261019)；v0.30.66 已发布 |
| [`plan-20260923.md`](plan-20260923.md) | CP-00 (audit handoff delivered)..CP-20 pending; expand bounded domain cards after inventory if needed | CP-00 audit handoff delivered 2026-10-07（`plan-20260923-coverage.tsv`，0 未分配）；CP-01 可消费 CP-00 审计产物；计划级 review PASS 及 DEP-CP-01..07 仍待满足；动态实现要求 CP-06 done/complete |
| [`plan-20260906.md`](plan-20260906.md) | SC-01..SC-07、SC-CLOSE | — |
| [`plan-20260907.md`](plan-20260907.md) | B3-00..B3-17 | **已收口**（B3-00..B3-17 全部 `done`/`complete`；`DEP-B3-05` 已满足，不再阻塞 plan-20260913） |
| [`plan-20260911.md`](plan-20260911.md) | PI-00..10（11卡pending） | 下一PI-00；本轮双规划审查PASS；PI-05围栏先于row writer，旧429历史不授本轮PASS |
| [`plan-20261018-hermes-capture.md`](plan-20261018-hermes-capture.md) | HM-00..09（10卡pending） | 下一HM-00；本轮双规划审查PASS；后续云围栏消费PI-05、schema消费PI-01交接；本机Hermes/live gate待证 |
| [`plan-20261018-grok-capture.md`](plan-20261018-grok-capture.md) | GK-06本机通过；功能暂停 | GK-00字段审计保留；GK-07交给`/root/grok_continue`，GK-08仍暂停；旧GO不覆盖当前生产接入 |
| [`plan-20260912.md`](plan-20260912.md) | MB-01..05、MB-07/08/10/11（MB-06/09/12 已取消） | 依赖 CAP（plan-20260916）等 |
| [`plan-20260913.md`](plan-20260913.md) | FL-00..FL-07 | **前置 plan-20260907 已完整收口（DEP-FL-04 满足）**；FL-00/01/07/02/06/03/04/05 已全部 `done`/`complete`，仅剩计划级收口门 |
| [`plan-20260916.md`](plan-20260916.md) | CAP-01..CAP-07 | 双评审已 PASS；CAP-01..03 仅按 ER-CAP-02 pin 作 zero-raw-persistence 重核；`DEP-ACF-CAP` 的 ACF 侧必要条件已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；CAP-04..07 `blocked`，须独立 security/privacy RFC 与新卡 |
| [`plan-20260919.md`](plan-20260919.md) | GCX-02/03/04 | GCX-01 已 `done`（v0.23.1）；GCX-02/03/04 均 `done/complete`（2026-10-07 分别发布 v0.30.32 / v0.30.33 / v0.30.34）；写集与 plan-20260918 串行（DEP-GCX-02） |
| [`plan-20260921.md`](plan-20260921.md) | VG-00..VG-14（15 卡） | **已完成（2026-09-24）**：15/15 卡 `done`/`complete`；`v0.23.65` 已发布（decision (ii) 合并发布；REL-VG-01 证据齐备）；证据口径按操作者裁决以替代机制满足双字面 PASS 与 supervisor capture；DEP-CP-04 所述 REL-VG-01 文件保留窗口随发布结束 |
| `issues/` 设计计划 | 见「计划一览」issues 表 | 各计划 Codex review `PASS` 前不得开工；`issues/476`/`479`/`483`/`490` 与 `plan-20260918` 写集串行（DEP-WT-09 / DEP-PL-04 / DEP-AD-06 / DEP-AD-07） |

---

## 三、实施中的计划、待退役的历史方案与当前卡

按执行窗口排序；当前执行与并行工作树见「四、当前执行指针」。

### plan-20261001-mega-browser-noninteractive（mega2 browser 非交互操作，12 卡全串行）

发布窗口顺序 MN-10 → MN-01 → MN-02 → MN-03 → MN-11 → MN-04 → MN-08 → MN-12 → MN-05 → MN-06 → MN-09 → MN-07；单一发布者为 Codex goal 线程 `01a1015b`（ER-12；移交见计划修订历史）。开工前置：`DEP-MN-02`（`../libra-backend` 为 Git 且在 `cf`，MN-01 起）；MN-03 起按 GC-MN-08 重核 `DEP-MN-01`（mega2@`8ff880c`）；MN-03 另需 `DEP-MN-06`；MN-07 另需 `DEP-MN-04`。

| 卡 | Lifecycle / Acceptance | 版本 / 证据 |
|---|---|---|
| MN-10 | `done` / `complete` | v0.30.14；PR #593 squash `80f58b4`；release `36959675389` 8/8、CodeQL main `36959660259` 绿、下载 200；2026-10-02 04:00:07 UTC |
| MN-01 | `done` / `complete` | v0.30.15；PR #595 squash `6e86578`；release `36968922078` 8/8、CodeQL main `36968913909` 绿、下载 200、网站标记计数 3（`cf@973f650`）；2026-10-02 06:11:28 UTC |
| MN-02 | `done` / `complete` | v0.30.16；PR #596 squash `36c05be`；release `36978760075` 8/8、CodeQL main `36978751927` 绿、下载 200、网站标记计数 4（`cf@23c21fc`）；2026-10-02 08:00:39 UTC |
| MN-03 | `done` / `complete` | v0.30.17；PR #597 squash `548a33c`；release `36987596125` 8/8、CodeQL main `36987587577` 绿、下载 200、网站标记计数 5（`cf@a31ced6`）；2026-10-02 09:36:00 UTC |
| MN-11 | `done` / `complete` | v0.30.18；PR #598 squash `6230874`（首轮 offline-core 败于 runner 基础设施，重跑后 12/12）；release `37002565759` 8/8、CodeQL main `37002556477` 绿、下载 200、网站标记计数 3（`cf@42def02`）；2026-10-02 12:16:22 UTC |
| MN-04 | `done` / `complete` | v0.30.19；PR #599 squash `81d1655`；release `37011384218` 8/8、CodeQL main `37011373062` 绿、下载 200、网站标记计数 5（`cf@1f7aa03`）；2026-10-02 13:37:24 UTC |
| MN-08 | `done` / `complete` | v0.30.20；PR #600 squash `36e826a`（首轮 network-remotes 败于外网抖动，重跑后 12/12）；release `37024751607` 8/8、CodeQL main `37024735662` 绿、下载 200、网站标记计数 5（`cf@cea2d47`）；2026-10-02 15:40:05 UTC |
| MN-12 | `done` / `complete` | v0.30.21；PR #601 squash `fb1cf80`；release `37035218493` 8/8、CodeQL main `37035206595` 绿、下载 200、网站标记计数 5（`cf@8f0e356`）；2026-10-02 17:09:37 UTC |
| MN-05 | `done` / `complete` | v0.30.24；PR #603 squash `9507623`（v0.30.22、v0.30.23 先后被并发发布占用，两次重建：`a4d05cb` → `98f1e84` → `4116c7f`）；release `37094701957` 8/8、CodeQL main `37094694927` 绿、下载 200、网站标记计数 11（`cf@57b7d12`）；2026-10-03 04:18:34 UTC |
| MN-06 | `done` / `complete` | v0.30.25；PR #605 squash `cdd32b7`；release `37099685600` 8/8、CodeQL main `37099680422` 绿、下载 200、网站 `--create-tag` 计数 7（`cf@57b7d12`）；2026-10-03 10:57:41 UTC |
| MN-09 | `done` / `complete` | v0.30.26；PR #606 squash `fc85e6e`；release `37104652942` 8/8、CodeQL main `37104701999` 绿、下载 200、网站 `--delete-tag` 计数 5（`cf@a56bce3`）、stable manifest 0.30.26；2026-10-03 10:57:41 UTC |
| MN-07 | `done` / `complete` | v0.30.27；PR #608 squash `faae20c`；release `37123533785` 8/8、CodeQL main `37123437271` 2/2、下载 200、stable 0.30.27；pinned-source live 12/12，`DEP-MN-03` 已交付；2026-10-03 13:05:47 UTC |

### issues/577（SSH 公钥认证拒绝诊断与设置指南）

SA-02 与 SA-01 均为 `done` / `complete`。Claude I11 第二次重基最终候选审查取得
字面 `VERDICT: PASS`（P0–P3=0）；最终 fmt、Clippy、Nextest 8208/8208、release
build 与隔离 locked install 全绿。候选 `ea89d047` 经 PR #578 的 12/12 检查后
squash merge 为 `5eb833f9`；v0.24.1 标签指向该提交，release workflow
`36296462821` 的 8/8 jobs 全绿，四平台 CDN 摘要、签名稳定清单、公开安装和上一版
固定产物均已验证。网站签名提交为 `cf@667d7da8`，活动 Worker 为 `c7011920`，
SSH 指南与六命令生产页均为 HTTP 200。Issue #577 已关闭；完整证据见
[`issues/577.md`](issues/577.md) 的「实施证据汇总」小节（原 `evidence/issues-577/` 目录已并入）。

### plan-20260927（六命令模块拆分，22 个独立切片）

2026-09-25 的干净 `main` 基线上，FUSE repair 两项测试因旧版 Operation 审计查询而 2/2 失败，故按 ER-10 新增独立 FIX-CM-04。同日 CM-01 的全量 Nextest 8115 项中通过 8114 项，`op_undo_redo::doctor_recovers_a_running_publish_journal_once` 三次均失败：物理 `a.txt` 留在 `two\n` 而非预期的 `one\n`。同一二进制从 linked CWD 失败、从 main 或 `/tmp` 通过。FIX-CM-01 只修 pinned scope 并建确定性红绿回归；Operation doctor 误报、中途回滚、undo/redo/revert 入口，以及 `switch` branch-attach 与 repo-wide refs restore 跨工作树锁序竞态，均属未修的独立 P1，由仓库维护者/单一发布者以 `DEFER-CM-OP-01` 另立高优先级安全计划或 issue。若本计划任一卡的 focused/C/D 或最终全量因此变红，须按 ER-10 立依赖 FIX 或先完成独立安全修复，再重跑至绿；DEFER 不能豁免红灯。FIX-CM-01 发布后 CM-01 重基并重跑 C/D，其余卡方可进入 C/T-5；FIX-CM-04 `done/complete` 且本修订计划取得新一轮 Claude Code 字面 `VERDICT: PASS` 后，CM-04 才可解除阻塞。R6/R9 PASS 只证明各自历史修订版；R10/R11/R12/R14 的 Codex/Claude 双 FAIL 均不放行，22 项 EX 未获批准；R15 Claude FAIL；R16 Claude FAIL（14 项 P1）；R17 修订稿待新 SHA 双评审；尚无本计划卡完成 D 组发布。

| 卡 | 本地状态 | 发布 |
|---|---|---|
| FIX-CM-01 Operation restore pinned scope | Lifecycle `done`（本地验证）/ Acceptance ；（2026-09 期间复核：`src/internal/head.rs`、`src/internal/operation/restore.rs` 已含 scoped 变体与 `restore_pinned_head_callsite_map_guard`；`tests/op_undo_redo.rs` 含 parent/child 回归）。验证：`op_undo_redo` 15/15 全绿，`doctor_recovers_a_running_publish_journal_once`、`doctor_recovery_uses_pinned_main_scope_from_linked_cwd`、`doctor_recovery_linked_cwd_child` 均 PASS；head/restore lib 33/33；三个 head guard 1/1 全绿。`tests/INDEX.md` 已更新 relevant-src。发布未执行 | 未发布 |
| FIX-CM-08 diff 生产 panic 守卫 | Lifecycle `done`（本地验证）；已在 `Cargo.toml`/`tests/INDEX.md`/`tests/compat/README.md` 注册 `compat_diff_production_expect_guard`，TARGET_FILES 覆盖 `src/command/diff.rs` + `diff/{compare,render,options}.rs`（移除不存在的 `blob_similarity.rs`）；守卫 1/1 绿，CM-08 diff 源码已移动 | 未发布 |
| FIX-CM-LIVE-GATE Cloud live 无跳过门 | Lifecycle `pending` / Acceptance 空；待 FIX-CM-01 与 CM-13 独立发布，提供无跳过脚本与纯本地 preflight；C 组按模板标准双 env/default-feature 全量，测试体 feature-off 不得静默跳过；本卡不使用真实 Cloud 资源 | 未发布 |
| FIX-CM-CLOUD-RECOVERY-AUTH 来源认证 | Lifecycle `pending` / Acceptance 空；`AC=35/8@EX-CM-REC-AUTH-G03`、`VER=35/8`；先独立发布受保护 workflow、签名来源 verifier 和零删除探针，D 不需要真实来源 artifact；EX 未批准 | 未发布 |
| FIX-CM-CLOUD-RECOVERY-CLEANUP 受限恢复 | Lifecycle `pending` / Acceptance 空；`AC=16/8@EX-CM-REC-CLEANUP-G03`、`VER=16/8`；依赖 AUTH 发布，补充精确 D1/R2 范围删除与同签名范围幂等重试；本卡 D 仍零真实删除；EX 未批准 | 未发布 |
| FIX-CM-CLOUD-REPO-SCOPE 云端仓库作用域 | Lifecycle `pending` / Acceptance 空；`AC=51/8@EX-CM-CLOUD-REPO-SCOPE`、`VER=40/8`；依赖 CLEANUP 发布，以本地假端点验证 CLI/D1 ensure/R2 sink 拒绝未登记 repo/slot 和越界写；EX 未批准 | 未发布 |
| FIX-CM-CLOUD-LIVE-SAFETY Cloud L3 写前准入 | Lifecycle `pending` / Acceptance 空；独立 `implementation`/`compensating` patch，`AC=39/8@EX-CM-CLOUD-SAFETY`、`VER=27/8@EX-CM-CLOUD-SAFETY`，EX 待新冻结 SHA 具名审批；依赖 FIX-CM-01、CM-03、CM-13、LIVE-GATE、RECOVERY-AUTH/CLEANUP、REPO-SCOPE 与 DEP-CM-05.vars-ready/recovery-ready。交付 CI-only 身份/只读探针、D1 全库加密备份与读回、全局及逐例预分配 manifest、失败关闭；自身 release-SHA 真实 D 与 `E-CM-L3-SAFETY` 必须通过且发布后才放行 CM-10。现 `premerge/postrelease=unknown`，未作真实 L3 写入 | 未发布 |
| CM-01 merge 状态与 autostash | Lifecycle `blocked` / Acceptance 空；R6 本地 A/B 与独立 Codex review `VERDICT: PASS`（`/root/cm01_review`）仅为历史预备，R11 新 sidecar/owner guard 尚未落地；首次 C 组 Nextest 8114/8115，`op_undo_redo::doctor_recovers_a_running_publish_journal_once` 三次均失败。待 FIX-CM-01 独立发布、新版计划 PASS 与 EX-CM-01 获批后重基，完成新 A/B，再于已 bump 树重跑 C/D | 未发布 |
| FIX-CM-04 FUSE repair v2 审计测试契约 | Lifecycle `done`（本地验证）；修正 `worktree_fuse_test.rs` 的 `repair_audit_rows` 查询 `command_name='worktree'` 与 `success`/`failed` 状态（v2 CLI 不再记 `worktree repair`/`succeeded`）；`worktree-fuse` feature 编译绿、`fuse_repair_*` 2/2 通过；并按 ADR-CM-01 为 `worktree.rs` 的 `mod doctor/operations` 增加显式 `#[path]` 声明 | 未发布 |
| CM-04 worktree registry | Lifecycle `blocked` / Acceptance 空；`AC=29/8@EX-CM-04`、`VER=18/8@EX-CM-04` 待具名批准；依赖 FIX-CM-04 与 CM-12；隔离本地试作默认 focused 291/291、FUSE lib 12/12，通过；FUSE command 8/10，两项失败已在干净 main 复现。预备证据不计验收，须等 FIX-CM-04 `done/complete` 及修订计划 Claude PASS 后重基重验 | 未发布 |
| FIX-CM-WT-MOVE 跨设备 move 数据保全 | Lifecycle `blocked` / Acceptance 空；`AC=57/8@EX-CM-WT-MOVE`、`VER=56/8`；依赖 CM-04 与 DEP-CM-WT-COMPAT。当前 patch 兼容的旧二进制和已打开 writer 排除证明不足，不能进行 C/merge/release；若需 breaking/minor，先提交具体兼容方案请用户决定。EX 未批准 | 未发布 |
| CM-06 status 输入、扫描与缓存 | Lifecycle `in-progress` / Acceptance 空；旧本地 A/B 与 `dirty_test` 14/14 仅为隔离预备，R11 新 owner/warning-order guard 尚未落地。待共享 INDEX 前驱发布、新版计划 PASS 与 EX-CM-06 获批后重基并重新执行正式 A/B；C/D 还须等 FIX-CM-01 发布 | 未发布 |
| CM-10 cloud sync、metadata 与 Agent catalog | Lifecycle `in-progress` / Acceptance 空；本地 cloud 命令 9/9、源内 52/52、mock/error 7/7、`compat_serial_registry` 25/25 为隔离预备。待 CM-03、CM-13、FIX-CM-CLOUD-LIVE-SAFETY 独立发布后重基；DEP-CM-05.premerge 的 vars owner/readback 已通过，但本地/mock 首写负例未证；postrelease 本卡 release-SHA 受保护 CI dispatch、`E-CM-L3-10` 身份/备份/manifest/善后与两个完整 live target 未证，阻 D/complete。本地仅 fake/mock/default C，不执行真实 Cloud 写 | 未发布 |
| CM-13 worktree repair/doctor 结构拆分 | Lifecycle `in-progress`（本地实现+验证中）；扩张 `src/command/worktree/doctor.rs`（确认/预览/scope 诊断/legacy 捕获接纳/migration 恢复/repair 驱动），保留 `doctor` 专有输出与公共 `pub(crate)` 路径；`worktree.rs` 由 7,439 降至约 4,000 行；focused 验证/全量待跑；`AC=34/8@EX-CM-13`、`VER=24/8@EX-CM-13` 待具名批准；Operation 中途回滚行为缺陷不在本卡修复范围，既有 doctor approved-project 确认写入的分类/锁/审计缺口记 `DEFER-CM-WT-01`，未被本卡修复 | 未发布 |
| CM-07 status 输出格式 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/status/output.rs`（human/JSON/porcelain/短格式渲染），status `command_test` 261/261 全绿；`StatusData` 保留在 facade；网站 `status.en.md` 交付与 DEP-CM-04 的 `cf` 远端证据待办；正式 C/D 待全量 | 未发布 |
| CM-11 cloud restore | Lifecycle `in-progress`（本地实现）；扩张 `src/command/cloud/restore.rs`（对象/metadata/Agent-catalog 恢复），`cloud.rs` 由 5,771 降至约 4,000 行；clippy 全绿；真实 L3 `E-CM-L3-11` 的受保护 CI `workflow_dispatch` 与两个完整 live target 属 postrelease D/complete 门，本地只做 fake/mock/default C | 未发布 |
| CM-05 worktree 普通操作所有权 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/worktree/operations.rs`（add/reattach/list/lock/unlock/move/prune/remove/umount + lifecycle/journal），worktree `command_test` 185/185 全绿；`worktree.rs` 由 7,439 降至约 1,700 行；全量 nextest 8273/8273 绿 | 未发布 |
| CM-02 merge 树裁决 | Lifecycle `in-progress`（本地实现+验证）；已拆出 `merge/{tree_merge,virtual_base,workdir,content,rename_merge,conflict}.rs` 六个职责模块；`merge.rs` 由 19,277 降至约 14,060 行；clippy 全绿；merge `command_test` 全量待跑（serde skip 属性已恢复）；`AC=39/8@EX-CM-02`、`VER=24/8@EX-CM-02` 待具名批准 | 未发布 |
| CM-03 merge facade/输出 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/merge/output.rs`（CLI 输出渲染/错误映射/merge message/签名），merge `command_test` 334/334 全绿；`merge.rs` 降至约 13,900 行（含内联测试）；clippy 全绿 | 未发布 |
| CM-08 diff 比较管线 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/diff/compare.rs`（revision/scan/algorithm、side 水合、pathspec/rename 检测），diff `command_test` 77/77 全绿；`diff.rs` 由 8,166 降至约 6,200 行；clippy 全绿 | 未发布 |
| CM-09 diff 渲染 | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/diff/render.rs`（word/patch/stat 渲染、summary/stat/raw/unified、colorize），diff `command_test` 77/77 全绿；compare/算法部分仍留在入口（CM-08 未做）；正式 C/D 待全量 | 未发布 |
| CM-12 rebase 状态/replay | Lifecycle `in-progress`（本地实现+验证）；扩张 `src/command/rebase/state.rs`（状态/autostash 侧车、scope-aware ref 更新与 GC roots），rebase `command_test` 86/86 全绿；interactive/replay 仍留在入口；正式 C/D 待全量 | 未发布 |

各卡发布仍按 ER-12 由单一发布者串行执行；FIX-CM-01 → CM-01，FIX-CM-04/CM-12 → CM-04 → CM-05 → CM-13，CM-07 → FIX-CM-08 → CM-08，CM-03/CM-13 → FIX-CM-LIVE-GATE → FIX-CM-CLOUD-RECOVERY-AUTH → FIX-CM-CLOUD-RECOVERY-CLEANUP → FIX-CM-CLOUD-REPO-SCOPE → FIX-CM-CLOUD-LIVE-SAFETY → CM-10 → CM-11。除 FIX-CM-01 自身外所有卡的 C/T-5 都等其独立发布 `done/complete`，须从关联工作树 cwd 实跑。`tests/INDEX.md` 全写者发布链为 FIX-CM-01 → CM-01 → CM-06 → CM-02 → CM-03 → CM-07 → FIX-CM-08 → CM-08 → CM-09 → CM-12 → CM-04 → CM-05 → CM-13 → FIX-CM-LIVE-GATE → FIX-CM-CLOUD-RECOVERY-AUTH → FIX-CM-CLOUD-RECOVERY-CLEANUP → FIX-CM-CLOUD-REPO-SCOPE → FIX-CM-CLOUD-LIVE-SAFETY → CM-10 → CM-11；共享 `tests/compat/serial_registry.rs` 另有 CM-03 → CM-10。CM-03 同卡把 INDEX 的 serial 守卫行改为无固定计数。CM-07 的 DEP-CM-04 仅核网站 `cf` 开工基线，实际 `status.en.md` 交付由本卡完成；已推送后失败走条件化 `compensating`，远端/部署不确定为 `remote-pending`。`CM-Publisher` 为本轮主 Agent（ID `/root`），负责状态、旧→新符号路径映射和发布；可委托文档代理编辑，但由发布者精确暂存与提交。`plan-status.md` 是 G-10 协调写集，允许随对应卡发布写集精确暂存，不使用 `commit -a`。Cloud 真实 L3 仅在受保护 release-SHA CI `workflow_dispatch` 写 job：先由安全卡交付 fail-closed 协议并产生独立 `E-CM-L3-SAFETY`，再由 CM-10/11 各自产生 `E-CM-L3-10/11`；本地只跑 fake/mock/default C，不凭本地 env 发真实写请求。

**规则与延期门：** 本版 22 项 `EX-CM-*` G-03 数量例外在 R14 双 FAIL、R15 Claude FAIL 后均未获批准，必须分别取得新冻结 SHA 的具名 Codex reviewer 对准确分子和证据的书面同意，以及本修订版 Claude Code 字面 `VERDICT: PASS`，才对对应卡生效；此前新卡不得开工。`DEFER-CM-OP-01` 是未修的独立 P1；CM-13 之外的 `worktree doctor --adopt/--clear-approved-project` 写入被归为 ReadOnly 且无审计/写锁，另记 `DEFER-CM-WT-01`，由仓库维护者/单一发布者立行为 FIX 或安全 issue；两者均非本计划已验收能力。`DEFER-CM-CLOUD-STATUS` 仅在既有本地 status 职责增长、cloud root 再超预算或修改反复跨 sync/restore 时重做职责审计并另立卡。

| Cloud L3 证据 ID | 已确认的共享只读背景 | 本卡强制门与状态 |
|---|---|---|
| `E-CM-L3-SAFETY`（FIX-CM-CLOUD-LIVE-SAFETY） | 用户已确认冻结 `.env.live-test` SHA256 `c8409dbd59232c9e17c970adc029564ca7d112f17a8ae25c2f56a44b5d768b6d` 的专用精确期望：D1 account `3a6c7e7927dbd254d3c78c1fc6222441`、`libra-testing` database UUID `8e067bd6-f12c-4462-a536-65f8acde59ce`，R2 同 account、bucket `libra-action`；用户最新澄清同一 account 的其它 D1 库属于其它项目，账号级 D1 建库/删库与远端整库导入均禁止。06:57 UTC 本地只读 D1 GET 200/UUID 匹配、R2 有界 ListObjectsV2 200/Name 匹配；R2 管理 API 403。后续 D1 只读 schema 清点为 25 用户表、1 SQLite 内部表、1 view、25 triggers（9 源码匹配、16 remote-existing/unmatched）、0 virtual table；16 条不得盲判安全。四个独立 repo vars 于 08:35:02 UTC 全部设置，API readback exact 4/4；live workflow id `283278914` 自 07:27 UTC `disabled_manually`，无待/运行 job。均不是本卡真实 D 证据 | `vars-ready=passed`；`premerge=unknown`（本地/mock 证明首写前 secret 对 vars、双只读探针、全库 SQL/Time Travel bookmark 的 `age` 加密 runner 外 artifact 上传读回、global/per-example 预分配 manifest、失败零写）；`postrelease=unknown`（独立受保护 environment、七项凭据迁移并移除 repo/base 默认注入、`cloud-live-recover.yml` 恢复通路与独立 custodian、实际 CI release-SHA 受保护 dispatch/精确 grant、两完整 target、写后限定清理/保留）。自身 `E-CM-L3-SAFETY` 未证，不可真实首写、D/complete 或放行 CM-10；本地 fake/mock/default C 可执行 |
| `E-CM-L3-10`（CM-10） | 继承上述用户确认期望与历史只读背景；不得借安全卡的 release 时点证据 | 安全卡先发布；本卡重基后另证 DEP-CM-05.premerge，本卡 release-SHA CI 独立核 secret/vars、D1/R2 探针、trigger 完整 SQL/owner 基线、全库加密备份读回、global/per-example UUID manifest 与写后清理/受控保留，两个完整 live target 均真跑。`E-CM-L3-10=unknown`、postrelease=unknown，阻本卡真实写与 D/complete；本地 fake/mock/default C 可执行 |
| `E-CM-L3-11`（CM-11） | 同一冻结精确期望与历史只读背景，不可沿用安全卡或 CM-10 写前证据 | CM-10 发布后本卡独立重证 DEP-CM-05.premerge，并在本卡 release-SHA CI 生成独立备份/身份/manifest/善后与两完整 live target 的 `E-CM-L3-11`。当前 `unknown`，真实写、D/complete 未放行；本地 fake/mock/default C 可执行 |

### 3.1 plan-20260918（add 收口）— 当前活动计划

执行链（依赖边）：`OI-01 → OI-02 → OI-03 → OI-04 → OI-05 → IA-01 → IA-02 → CH-01 → CH-02 → AU-01..06（验收，no-release）→ PSF-01..03 → SW-06 → FM-03/04 → WT-05..07`。

| 卡 | 状态 | 发布 |
|---|---|---|
| OI-01 进程内锁回归守卫 | `done/complete` | v0.23.4 |
| OI-02 锁超时诊断 | `done/complete` | v0.23.5 |
| OI-03 锁等待退避与预检回放 | `done/complete` | v0.23.6 |
| **OI-04 批量发布 repair marker** | **`in-progress`** | **v0.23.7** |
| OI-05 marker 失败错误契约 | `pending`（写 `src/cli.rs`，受 DEP-AD-12 / CX-30 串行约束） | — |
| IA-01/IA-02、CH-01/CH-02 | `pending` | — |
| AU-01..AU-06（验收收口，#491–#494） | `pending` | no-release（v0.22.48 已交付） |
| PSF-01..03、SW-06、FM-03/04、WT-05..07 | `pending` | — |

### 3.2 plan-20260919（XDG 配置迁移）

| 卡 | 状态 | 发布 |
|---|---|---|
| GCX-01 路径决议统一与 XDG 默认 | `done/complete` | v0.23.1 |
| GCX-02 legacy 库自动迁移 | `done/complete`（2026-10-07：实现/单测/集成/失败注入/文档、fmt+clippy+聚焦门全绿；独立发布 v0.30.32，PR #615） | v0.30.32 |
| GCX-03 全域 vault unseal key 随迁 | `done/complete`（2026-10-07：实现/单测/集成/密码学回归/文档、fmt+clippy+聚焦门全绿；独立发布 v0.30.33，PR #616） | v0.30.33 |
| GCX-04 用户级 hooks 路径对齐 | `done/complete`（2026-10-07：实现/单测/compat 回归/文档、fmt+clippy+聚焦门全绿；独立发布 v0.30.34，PR #617） | v0.30.34 |

> **2026-10-07 收口说明**：GCX-02/03/04 已全部 `done/complete`，分别经独立发布 v0.30.32 / v0.30.33 / v0.30.34（D 组 `base.yml`+CodeQL 全绿，含 `compat-offline-core` 全量 L1+L2+L3；GCX-04 的 `opencode-export-linux` 为 OpenCode Session Capture 开发中已知波动，不阻塞）。版本面 `compat_version_surface_sync` 通过。本轮按操作者指示**不调用 Codex review**，故「完成判据」的 review 门仍未勾；DEP-GCX-01（`../libra-backend`）仍不满足，仅阻塞网站页同步；DEP-GCX-03 已满足（`.env.test`/`.env.live-test` 存在）。

### 3.3 plan-20260819（Memory M2，R30 历史方案）

2026-09-21 R30 重设计：按 0.23.x 拆除后的 main 重写架构 seam（ADR-M2-05/07/10 改写，新增 ADR-M2-12..15），整份迁到模板 v2.8（ER-08 逐卡发布 + ER-14 nextest + 版本面集合以 `compat_version_surface_sync` 为权威），新增 M2-16A..E 接口融合卡与 M2-16D1（自 M2-16D 条件性拆出），并把 PR #456 改为正向移植源（禁止 merge）。R30 r37 于 2026-09-22 对冻结版 `4fbaf4bd…` 取得 Codex/Claude 同版双 PASS，但全部 M2 卡仍为 `pending`，未实施。使用者随后要求独立重设计；MEM-01/02 现由 [`plan-20260926.md`](plan-20260926.md) 承接，旧 M2 卡待该计划 DM-00 正式登记退役。本节保留 R30 历史状态，不构成当前开工许可。

| 卡 | 状态 |
|---|---|
| M2-16A #456 memory 正向移植与 seam 重挂（R30 执行起点） | `pending` |
| M2-16B `EpisodeCompilerModel` seam | `pending` |
| M2-16C bridge `memory.recall` / `memory.episode.record` | `pending` |
| M2-16D 终态适配决策（audit，no-release） | `pending` |
| M2-16D1 条件性 AgentRun 适配 | `pending`（僅 M2-16D 判定可映射時啟動並納入收口；不可映射時登記 `DEFER-M2-09` 墓碑） |
| M2-16E #456 收口与迁移映射（docs，no-release） | `pending` |
| M2-01 冻结 M2 合同与领域类型 | `pending`（R30 双评审 + M2-16A 移植后复核） |
| M2-01K 仓库本地 keyed digest 基础 | `pending` |
| M2-02 Memory/Episode SQLite schema | `pending` |
| M2-02F FTS5 构建与搜索 schema | `pending` |
| M2-02R 共享 receipt 账本与保留策略 | `pending` |
| M2-03 线性 ref CAS transaction primitive | `pending` |
| M2-04 MemoryWriter 与 repo 权威历史 | `pending` |
| M2-05 保护并隐藏本地 Memory branch | `pending` |
| M2-06 事件重放/增量投影与 rebuild | `pending` |
| M2-07 来源、证据与自动信任边界 | `pending` |
| M2-08 终态触发与可恢复 generation job | `pending` |
| M2-09 Task Episode 编译 | `pending` |
| M2-10 Intent Iteration 编译 | `pending` |
| M2-11 FTS5/BM25 召回与证据展开 | `pending` |
| M2-12 Agent 冻结上下文与 ContextSelectionReceiptV1 | `pending` |
| M2-13 最小 `libra memory` 命令 | `pending` |
| M2-14 Episode/召回 benchmark 与文章 | `pending` |
| M2-14C 核心覆盖率与 CI 门 | `pending` |
| M2-15 计划收口与最终聚合发布 | `pending` |

**分階段收口：** M2-16D/E 為 no-release 卡，在 M2-15 的 C/D 前保持 `locally-accepted`；M2-15 的 C/D 以其變更作為同一 release closure，D 證據產出後回填 `complete`，再執行 PR #456 關閉/supersede。

**外部 host 前置：** `DEP-M2-DSH-01`（DeepSeek Harness bridge 22-method 客戶端）為**非阻塞** post-M2 互操作驗證；未交付時 M2-16C 以 in-repo 新舊 host fixture 完成，真實互操作記 N/A + 殘餘風險。

**來源前置：** `DEP-M2-SRC-01`（#456 只讀 source clone；`git` 審計命令只在此 clone 執行，目標 Libra checkout 一律用 `libra`/`rg --files`）。

**阻塞登记：** PR #456（`Anduin9527:codex/memory-m2-core-draft-pr`）不可直接合并（距 main 296 commits、39 档冲突、依赖已删 Code SCC）；由 M2-16A 正向移植、M2-16E 收口关闭（ADR-M2-15）。

### 3.7 issues/474（clone 浅克隆 / bundle / bare / mirror）

发布窗口：CL-01 → … → CL-15。Phase 0 Codex R6 `PASS`（2026-09-23）。

| 卡 | 状态 | 发布 |
|---|---|---|
| CL-01 fsck 断链检测与 shallow 豁免 | `done`/`complete` | v0.23.47（#510） |
| CL-02 log/rev-list shallow helper | `done`/`complete` | v0.23.48（#511） |
| CL-03 其余历史遍历 | `done`/`complete` | v0.23.49（#512） |
| CL-04 本地 Git 浅边界 | `done`/`complete` | v0.23.50（#514） |
| CL-05 `--depth` 隐含单分支 | `done`/`complete` | v0.23.51（#516） |
| CL-06 普通路径忽略浅化参数 | `done`/`complete` | v0.23.52（#517） |
| CL-07 shallow Git 源克隆 | `done`/`complete` | v0.23.53（#518） |
| CL-08 bundle create 写入 HEAD | `done`/`complete` | v0.23.54（#520） |
| CL-09 clone 接受 bundle 源 | `done`/`complete` | v0.23.57（#561） |
| CL-10 以 bundle 为 remote 的 fetch/pull | `done`/`complete` | v0.23.58（#562） |
| CL-11 bare/mirror 默认目录名与 bare 布局 | `done`/`complete` | v0.23.59（#563） |
| CL-12 clone --mirror 全命名空间 | `done`/`complete` | v0.23.60（#564） |
| CL-13 mirror-aware fetch/prune | `done`/`complete` | v0.23.61（#565） |
| CL-14 depth 测试 + T5 live | `done`/`complete` | v0.23.62（#567） |
| CL-15 branch -a/-r 远程显示 | `done`/`complete` | v0.23.63（#568 / `d00366b`） |
| Closeout proxy hygiene | `done`/`complete` | v0.23.64（#569 / `3554f1e`；DEFER-CL-ENV-*） |

### 3.6 issues/476（工作树命令族）

发布窗口：WT-01 → WT-02 → WT-04 → WT-08 → WT-09 → WT-10 → WT-11 → WT-03。用户 2026-09-20 覆盖 ER-05（执行 Agent 自审）。

| 卡 | 状态 | 发布 |
|---|---|---|
| WT-02 `status -M` | `done`/`remote-pending` | v0.23.29 |
| WT-04 `rm` 拒绝分类 | `done`/`remote-pending` | v0.23.30 |
| WT-08 裸 stash = push | `done`/`remote-pending` | v0.23.31 |
| WT-09 stash push 预检顺序 | `done`/`remote-pending` | v0.23.32 |
| WT-10 消息格式 | `done`/`remote-pending` | v0.23.33 |
| WT-11 `--index` 恢复 | `done`/`remote-pending` | v0.23.34 |
| **WT-01 回归守卫** | **`done`/`remote-pending`（C 组落地中）** | **v0.23.35** |
| WT-03 init 默认 ignore | `pending`（DEP-WT-08 用户评审） | — |

### 3.4 plan-20260822（Operation Log v2）

| 卡 | 状态 |
|---|---|
| OL-01..OL-12、CH-01..CH-04 | `done/complete` |
| OL-13 多 worktree Operation heads 与 reconcile | `done/complete`（v0.23.0，`9da06b4`） |
| OL-14 Operation/Change Web 只读图 | **已取消**（G-09 墓碑，2026-09-20：`web/` 随 plan-20260920 拆除且不重建；`libra op log/show` 为现行查询面） |
| OL-15A v1-boundary runtime cutover | `done/complete`（PR #503：生产 mutation 统一 v2 middleware，legacy namespace 仅 migration/schema fixture 保留） |
| OL-15 移除 v1 operation 代码/表/命令 | `done/remote-pending`（PR #503：v1 wrapper/service/model/fallback 已移除；等待 compat-offline-core） |

### 3.5 plan-20260903（merge）与 plan-20260729（CT-01）

- plan-20260903：MG-01..MG-21 卡片全 `done/complete`；**计划级收口（完成判据、deferred 差异登记）仍待完成**，`plan-long.md` 标「实施中」。
- plan-20260729：CT4-01 已发布（v0.21.21）；**CT3-07 已正式延后（转换轴 → DEFER-09，已被 plan-20260825/27 承接关闭）**；完成判据 13 项未勾选。

### 3.6 plan-20260830（sandbox export）

SBX-01..05 `done/locally-accepted`；**发布步按 DEFER-SBX-06 正式延后**（DEP-SBX-05 未就绪），ER-13 收口门已绿（2026-09-01）。

---

## 四、当前执行指针（next action）

- **本对话当前指针（USER-OG-FINAL-C-20261017）：** 下一张OG-08最终方案C文档审计→OG-06 legacy doctor→FAST-OG-04唯一patch/full/C-D；OG-25/26已source-qualified待该发布收口。BOOT与OG-24/27/28/29/05撤出本轮、native-only DEP不再是前驱；初始环境DEFER按范围决定关闭（未修复/非PASS）。68库存=61完成+5待交付+2原延期，正式61/68=89.71%；可执行macOS61/66=92.42%，COV26/26不变。旧OG00/02及SRC029指针仅历史。

- **当前正在执行：** `plan-20260907` 已收口（B3-00..B3-17 全 `done`/`complete`，`v0.23.68`→`v0.29.0`；最终 Codex/Claude R40 双 PASS；GC-B3-01/02 收口守卫绿；`DEP-B3-05` 已满足）。`plan-20260913`（Media）FL-00..FL-05 亦已 `done`/`complete`（`v0.30.0`..`v0.30.5`），仅剩计划级收口门。**`plan-20260926` DM-00 已 `done`/`complete`**（2026-10-07），`plan-20260819` 已登记取代；**`DM-01`、`DM-10`、`DM-02` 已 `done`/`complete`**（DM-01 → v0.30.31；DM-10 → v0.30.35；DM-02 → v0.30.37，均 REL-DM-01 独立发布 + D 组全绿；ER-05 评审待记录）。**下一个零依赖候选为 `plan-20260916` CAP-01（先重核 `DEP-CAP-01`）；plan-20260926 内的下一张为 `DM-13`**（新鲜度、horizon 窗口与重建等价）。`plan-20260925` SCAP-01 / SCAP-02 已 `done`/`complete`（`v0.23.55` / `fa3849e`，D 组全绿；2026-09-28 本地最终门 8241/8241 passed，本轮不 bump 版本）。
- **本会话已收口：** `issues/577` SA-02/SA-01 已 `done`/`complete`；PR #578、v0.24.1、8/8 release jobs、网站 `cf` 与生产页 D 证据均完成，Issue #577 CLOSED。
- **SCAP-01 与 B3-00：** SCAP-01 已收口，`runtime.rs` 写集互斥结束；B3-00 已收口（`v0.23.68`）。
- **下一步：** `issues/474` 已收口（DEP-CL-07 完成，#474 CLOSED）。`plan-20260926` DM-00 已 `done`/`complete`、DM-01/DM-10/DM-02 已 `done`/`complete`；下一个零依赖候选为 `plan-20260916` CAP-01（先重核 `DEP-CAP-01`），plan-20260926 内下一张为 `DM-13`。
- **并行窗口（不在本执行指针）：** `plan-20260927` 的 CM-01 等 pinned scope FIX-CM-01，CM-06/10 有上述本地进度，所有卡 C/T-5 等 scope FIX 发布；CM-10 还等 CM-03/CM-13 共享文件、FIX-CM-LIVE-GATE 与 FIX-CM-CLOUD-LIVE-SAFETY 独立发布；真实 Cloud L3 只在受保护 release-SHA CI 写 job，DEP-CM-05 的 vars-ready 已过、premerge/postrelease 未过，live-compat workflow 当前 `disabled_manually`，由 root 满足受保护环境、凭据迁移与恢复门后重开；FIX-CM-04 待修订计划 Claude PASS，CM-04 阻塞；FIX-CM-08 与 CM-13 待开工，尚无本计划远端发布。Operation 独立 P1 交 `DEFER-CM-OP-01`，worktree approved-project 写入缺口交 `DEFER-CM-WT-01`，Cloud 小型 status 抽离交 `DEFER-CM-CLOUD-STATUS`。`issues/476` WT-03 仍等 DEP-WT-08；`plan-20260918` 其余 add 卡待推进。MEM-01/02 由 `plan-20260926` 承接，R11 字面 `VERDICT: PASS`；`DM-00` 已 `done`/`complete`、`DM-01` `done`/`complete`、`DM-10` `done`/`complete`，其余卡 `pending`，`DM-05` 仍受 `DEP-DM-06` 阻塞。
- **`plan-20260924` ACF-12 最新增量（2026-10-01 04:22:38 UTC）：** 修正逐条刷新队列可能突破单次批次上限的问题，doctor repair 共用最多 5 次 replay budget 与 2 秒 cooperative deadline；bounded queue 与共享预算单测各 1/1、`agent_doctor_repair_test` 30/30、`session_capture_docs_contract` 6/6、`compat_agent_docs_contract` 9/9、`cargo check --lib` exit 0。EN/zh、development command/tracing 与 `COMPATIBILITY.md` 已同步；backend 网站 mirror 当时未改（缺少 `cf` VCS 命令）；该限制已由 2026-10-01 04:49:35 UTC 的 backend 网站同步取代（见 plan-20260924 ACF-12 Verification update）。实现仍无真实 pending-artifact CAS/object publication 验证，12 条 unused warnings 未清；ACF-12 及整计划保持未验收，完整进度见 plan-20260924 当前卡证据。

---

## 四·零、零依赖可立即启动的任务卡（入度为零）

本表在每个「未执行」计划中列出**依赖图入度为零**（无内部前置卡）的任务卡，并标注它是否真的「可立即开工」，还是仍被计划级 review 门或跨计划 `DEP-*` 门控。判据：`开工许可 = 内部无前置 ∧ review 门已 PASS ∧ 外部/跨计划 DEP 已满足`。

| 计划 | 入口卡 | 卡要做什么（简述） | 内部前置 | 计划级 review 门 | 外部/跨计划门控 | 可立即开工 |
|---|---|---|---|---|---|---|
| [`plan-20261019-agent-mcp-server`](plan-20261019-agent-mcp-server.md) | MC-01 | stdio 上的 legacy MCP 协议核心（`libra agent mcp --stdio`） | 无 | Codex R64 PASS（R66 差异确认 PASS） | DEP-MC-03（并发未提交改动写集核对） | ✅ |
| [`plan-20260822`](plan-20260822.md) | （PR #503 收口；OL-14 已取消、OL-15 `remote-pending`） | — | — | — | — | ⏳ 等待 compat-offline-core 远端门禁 |
| [`plan-20260926`](plan-20260926.md) | DM-00 | 冻结 #456 事实基线、编制取材清单并登记 R30 退役 | 无 | **已过**（R11 字面 `VERDICT: PASS`，`P0=0` / `P1=0`） | **DEP-DM-03**（#456 只读 clone，`/tmp/libra-pr456` 降级路径）+ **DEP-DM-05**（R30 退役登记）均已满足 | ✅ `done`/`complete`（2026-10-07：取材清单 + 冻结远端事实 + R30 退役） |
| [`plan-20260919`](plan-20260919.md) | GCX-02 | legacy 全局 config DB 首次使用自动迁移（锁+快照+校验+原子提交） | GCX-01（已 `done/complete`） | **本轮按操作者指示未评审** | DEP-GCX-02：与 plan-20260918 串行写 `COMPATIBILITY.md`/网站页（0918 未开工，写集 clean） | ✅ `done/complete`（v0.30.32，PR #615） |
| [`plan-20260919`](plan-20260919.md) | GCX-04 | 用户级 hooks 文件路径对齐 XDG（macOS 只读回退，实现泛化到任何原生配置目录不同的平台） | GCX-01（已 `done`） | **本轮按操作者指示未评审** | DEP-GCX-01 不满足（本机无 `../libra-backend`），仅阻塞网站页；发布队列 GCX-02→GCX-03→GCX-04 | ✅ `done/complete`（v0.30.34，PR #617；opencode-export-linux 为 OpenCode 开发中波动不阻塞） |
| [`plan-20260912`](plan-20260912.md) | MB-01 | 有界 mega2 tree transport 与 wire validation | 无 | **双评审已 PASS**（Codex R7 / Claude R5） | DEP-MB-01：mega2 `a1293686` tree API pin 现场重核（2026-09-21 已前推） | ✅ `done/complete`：v0.23.37（84f6bd8）；codeql + release.yml 8/8 jobs 全绿；CDN 产物 HTTP 200 |
| [`plan-20260916`](plan-20260916.md) | CAP-01 | Agent Capture wire types、URL、uid、transport trait | 无 | **双评审已 PASS**（Codex R3 / Claude R3） | DEP-CAP-01：monoengine `2b8f365` capture HTTP pin 重核（ER-CAP-02） | ⚠️ 需重核 DEP-CAP-01 |
| [`plan-20260924`](plan-20260924.md) | ACF-01 | Agent Capture validated ingress contract | `DEP-ACF-01`（既有執行基線） | R91 字面 `VERDICT: PASS` | ACF-09 `Lifecycle=done`、`Acceptance=complete`；`DEP-ACF-MIRROR` 已交接 | ✅ **已收口**；ACF-01..20 与 FIX-ACF-01 `done`/`complete` |
| [`plan-20260913`](plan-20260913.md) | FL-00 | 核实 Media 前提与热路径（audit，no-release） | 无 | 联合 review 已 PASS（U2 `VERDICT: PASS`） | DEP-FL-04：plan-20260907 **已完整收口** | ✅ 已 `done`/`complete`（FL-05 亦完成；本计划仅剩计划级收口门） |
| [`plan-20260904`](plan-20260904.md#cx00-source-audit-20261019) | CX-15 | CX-00/12已source-qualified；Codex ordinary thinking保留与侧车 | 无；live trigger/data取得仍UNRUN | CX-00 Claude R2 PASS；CX-12 Claude R1 PASS（51turn）；实现卡审查未验 | audit无生产修改；CX-12仅source-pinned合成fixture | CX-00 in-progress/locally-accepted；CX-12 in-progress/locally-accepted；正式C/D未验，其余27卡pending |
| [`plan-20260905`](plan-20260905.md#cc04-envelope-audit-20261019) | CC-08 | legacy Claude 受控 revision 修复 | CC-05 | 计划级 DATA-CAP R2 PASS 不替代本卡实现审查 | `claude_legacy_` 1 条通过；旧对象未改写；tombstone 拒绝复活；signed main CI 与 0.30.65 发布未登记 | done / complete；v0.30.65 / v0.30.66 已发布 |
| [`plan-20260906`](plan-20260906.md) | SC-01 / SC-02（可并发） | SC-01 `base.yml` 最小权限加固；SC-02 会话入口 id 守卫 | 无 | **未定稿**（R2 PASS 已作废；R22 `FAIL`） | SC-04 受 DEP-SC-01/04/05/06；SC-07 受 DEP-SC-07 | ❌ 禁止开工 |
| [`plan-20260902`](plan-20260902.md) | OG-00 | OpenCode2.0.26 Hook/export当前契约审计 | 无；按具名main source与逐文件单写者 | 当前9/9审计门；Claude Code R2 semantic PASS，旧评审为历史 | 无新增完整RG前置，typed/native归档门仍保留 |  ✅ done/complete，具名产物在v0.30.43最终tree/site，实际FAST-OG-01 C/D继承 |
| [`plan-20260911`](plan-20260911.md) | PI-00 | Pi版本/source/事件身份只读审计 | 无 | DATA-CAP-20261019 本次 Claude R2 实际 PASS；旧 R5 是历史 | 后继PI-05先围栏；PI-01再identity/schema配对 | pending；未实施，选卡授权及具体依赖仍须核验 |
| [`plan-20261018-hermes-capture`](plan-20261018-hermes-capture.md) | HM-00 | Hermes版本/source/observer协议审计 | 无 | DATA-CAP-20261019 本次 Claude R2 实际 PASS；旧 R5 是历史 | 后继HM-02消费PI-05，HM-01消费PI-01交接；缺binary只阻支持提升 | pending；未实施，选卡授权及具体依赖仍须核验 |
| [`plan-20261018-grok-capture`](plan-20261018-grok-capture.md) | GK-00 | 记录已安装 Grok 1.0.62 的日志与 hook 形状 | 无 | 审计已写入计划；SHA 检查已取消 | 公开 HEAD 仍是 1.0.45，该事实不再停工 | GK-01..05 GO；GK-00 Acceptance 仍空，等 GK-01 的 C/D |
| [`plan-20261018-grok-capture`](plan-20261018-grok-capture.md) | GK-01 | Repository 接受 `grok` 且仍 unsupported | GK-00 字段矩阵 GO；DEP-GK-DB；DEP-GK-WRITE | 本卡 focused 测试已过；正式评审未记 PASS | 专用 CHECK 连接在本卡落地 | `locally-accepted`；Acceptance 仍空，等 v0.30.53 的 D-CAP-STD |
| [`plan-20261018-grok-capture`](plan-20261018-grok-capture.md) | GK-02 | 按序保留 thought chunk，不拼接 | GK-01；DEP-GK-BASE；DEP-GK-WRITE | 本卡 focused 测试已过；正式评审未记 PASS | 不注册 `live_capture_for` | `locally-accepted`；Acceptance 仍空，等 v0.30.55 的 D-CAP-STD |
| [`plan-20261018-grok-capture`](plan-20261018-grok-capture.md) | GK-03 | reasoning 密文进入普通 transcript，sink 零命中 | GK-02；DEP-GK-WRITE | 本卡 focused 测试已过；正式评审未记 PASS | 不改 `opencode_export.rs` | `locally-accepted`；Acceptance 仍空，等 v0.30.57 的 D-CAP-STD |
| [`plan-20261018-grok-capture`](plan-20261018-grok-capture.md) | GK-04 | 用户级只观察 hook，路径监狱 | GK-03；DEP-GK-PIN；DEP-GK-WRITE | 本卡 focused 测试已过；正式评审未记 PASS | 不打开 enable，`as_hooks()` 仍是 `None` | `locally-accepted`；Acceptance 仍空，等 v0.30.59 的 D-CAP-STD |
| [`plan-20261018-grok-capture`](plan-20261018-grok-capture.md) | GK-05 | grok supported，wave 为 grok，launcher 仍关 | GK-04；DEP-GK-DOC；DEP-GK-RELEASE；DEP-GK-WRITE | 本卡 focused 测试已过；Codex 与网站 Claude PASS 未记 | 不改 `preview.rs`；review/investigate 仍关 | `locally-accepted`；Acceptance 仍空。v0.30.63 Check 被取消，CodeQL 与 release 成功，不计 complete |
| [`plan-20261018-grok-capture`](plan-20261018-grok-capture.md) | GK-06 | 纯parser、禁生产observer、文档更正 | 最新main与用户授权 | focused125/125、35本机Codex+矩阵通过，Claude架构/网站PASS | 完整回归10325/10325 PASS；不交付 | locally-accepted / 空 |
| [`plan-20261018-grok-capture`](plan-20261018-grok-capture.md) | GK-07 | 纯字节reader/normalizer接续 | GK-06本机门通过；已指派`/root/grok_continue` | 31Grok+65架构及静态门通过，独立review PASS | 后续交其他agent；不接生产 | locally-accepted / 空 |
| [`plan-20261018-grok-capture`](plan-20261018-grok-capture.md) | GK-08 | 授权snapshot与共享writer接入 | GK-07及具体写集/选卡 | 方向草案 | 功能仍暂停 | planned / 空 |
| [`plan-20260925`](plan-20260925.md) | SCAP-01 | AgentTraces ingest 改调 `decide`。`v0.23.55` / `fa3849e` D 组全绿 | SCAP-02（登记已核对） | R9 双 `PASS` | 已收口 | `done`/`complete` |
| [`plan-20260907`](plan-20260907.md) | B3-00 | pin `git-internal`（保持 `=0.10.2`）并引入 `object_format` 事实源 | 无 | **双评审已 PASS**（R40 Codex/Claude 双 PASS） | `v0.23.68` 已发布；focused VER 绿 | `done`/`complete` |
| [`issues/470`](issues/470.md) | FM-01 | 共享写入原语与 `restore` 系物化 | 无 | 尚未 Codex review | 关闭依赖 plan-20260918 FM-03/04（DEP-FM-06/07） | ❌ 禁止开工 |
| [`issues/473`](issues/473.md) | IN-01 / IN-03 / IN-02 | 空模板自引用防护 / 存储路径前置检测 / 换格式 reinit fail-closed | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/474`](issues/474.md) | — | CL-01..CL-15 + closeout + DEP-CL-07 已完成；#474 CLOSED | — | **R6 `PASS`** | 无 | ✅ **已收口** |
| [`issues/475`](issues/475.md) | CF-02 / CF-01 | key/模式校验与退出码 / 带 value-pattern 的删除 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/500`](issues/500.md) | CS-16（upstream qualification 入口卡；其 qualified 结论受外部硬门 DEP-500-11/13/14 门控，执行本身不被阻塞；CS-00 为其下游） | upstream qualification（CS-16 → CS-19 → CS-17 → CS-20 依序执行）后由 CS-00 盘点本地统计产物并对 Mega2 artifacts 做 GO/NO-GO；CS-16 为零内部前置入口 | 无（入口 = CS-16；执行链见描述） | **R41 双评已过**（Codex full-plan PASS + Claude targeted PASS，P0/P1=0） | CS-16/CS-19/CS-17/CS-20 qualification audits 已完成并记录 deployment-bound `not-qualified` evidence；CS-00 据此 NO-GO，CS-10..13 正式延后。CS-20 aggregate 非 qualified，因此 GO 与实现卡启动硬门未满足。 | ✅ **已收口（NO-GO）**：本地 trunk compose 实测 CS-16/19/17 `not-qualified` → CS-20 `not-qualified` → CS-00 判定 NO-GO；CS-10/11/12/13 延入 DEFER-CS-02/03/07/10（重启条件：维护者在 #500 确证数据集且新 CS-00b 为 GO）；CS-18 登记 defer+outgoing handoff |
| [`issues/476`](issues/476.md) | WT-03 | `init` 不再创建默认 `.libraignore` | DEP-WT-08、DEP-WT-05 | 用户 2026-09-20 覆盖：执行 Agent 自审 | 2026-10-18 落地（DEP-WT-08 由用户修复/关闭指示视为满足） | ✅ 完成 |
| [`issues/478`](issues/478.md) | LG-01 | `log`/`rev-list` `--grep` 模式类型与匹配范围 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/479`](issues/479.md) | EC-01 | plumbing 退出码契约守卫 | 无 | 尚未 Codex review | DEP-PL-04：与 plan-20260918 `add.rs` 串行 | ❌ 禁止开工 |
| [`issues/480`](issues/480.md) | HP-01 / HP-02 | `remote add` 默认 refspec 与 `--mirror` / `remote rename` 改写推送目标 | 无 | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/481`](issues/481.md) | MX-01 | 短对象名候选去重 | 无 | 尚未 Codex review | 实现完成；lib 76/76、rev_parse 49/49、repack 7/7、restore 47/47、bisect 29/29、compat_ledger_schema 43/43；fmt/clippy 绿；C 组 bump 与 D 组 release 证据待用户授权 | 实施中 |
| [`issues/481`](issues/481.md) | MX-02 | 歧义短对象名诊断与解析失败退出 | MX-01 | 尚未 Codex review | 实现完成；lib utils::util 70/70、rev_parse 50/50、log 141/141、checkout 34/34、compat_ledger_schema 43/43；fmt/clippy 绿；C 组 bump 与 D 组 release 证据待用户授权；网站文档同步在 `../libra-backend`（协调方） | 实施中 |
| [`issues/481`](issues/481.md) | MX-03 | bisect 会话安全与统一错误 | MX-01 | 尚未 Codex review | 实现完成；lib command::bisect 3/3、command_test bisect_test 33/33、compat_serial_registry 26/26、compat_ledger_schema 43/43；fmt/clippy 绿；C 组 bump 与 D 组 release 证据待用户授权；网站文档同步在 `../libra-backend`（协调方） | 实施中 |
| [`issues/481`](issues/481.md) | MX-06 | fsck 消息严重级别 | DEP-MX-04 | 尚未 Codex review | 实现完成；lib command::fsck 9/9、command_test fsck_test 53/53、compat_ledger_schema 43/43；fmt/clippy 绿；C 组 bump 与 D 组 release 证据待用户授权；网站文档同步在 `../libra-backend`（协调方） | 实施中 |
| [`issues/483`](issues/483.md) | CO-01 | `count-objects` 无参数形式与命令契约 | 无 | 尚未 Codex review | CO-03/04 受 DEP-CO-04（`src/cli.rs`） | ❌ 禁止开工 |
| [`issues/487`](issues/487.md) | IG-01 | 本地传输复用已修复的 pack 编码器 | 无（必最先完成） | 尚未 Codex review | 无 | ❌ 禁止开工 |
| [`issues/488`](issues/488.md) | GR-01 | `grep --exclude-standard` / `--no-exclude-standard` | 无 | Codex R1 `PASS`（2026-10-06）；DEP-GR-01/02 未满足，发布步因 `gh` 未认证阻塞 | 实现完成；E1-E9 矩阵与 git 实跑一致；测试 48/48；`compat_ledger_schema` 43/43 | 实施中 |
| [`issues/488`](issues/488.md) | GR-02 | 仓库模式子目录作用域、相对路径与 `--full-name` | GR-01 | Codex R1 `PASS`；M-SCOPE 16/16 与 git 逐字节一致；S11 TC-1596 移植通过 | 实现完成；测试 54/54；fmt/clippy 通过 | 实施中 |
| [`issues/490`](issues/490.md) | SW-01 | 采用支持 index v3 扩展标志的 `git-internal` | 无 | 尚未 Codex review | DEP-AD-07：与 plan-20260918 串行 | ❌ 禁止开工 |
| [`issues/582`](issues/582.md) | HKT-00 | 固定安全 host-key interaction 設計，並移交 HP-17／DEFER-07 | 无 | R1 `PASS`（計劃自審） | DEP-HKT-01/02/03 已滿足：HKT-00 固定 Git/OpenSSH 參照並完成移交，HKT-01/02 已实现 | ✅ HKT-00/01/02 全部 `done/complete`（v0.29.1） |

> 说明：✅ = 无任何门控，可立即开工；⚠️ = 内部无前置但仍有外部/跨计划 `DEP-*` 或发布队列约束；❌ = 计划级 review 门未过（多数 issue 计划尚未 Codex review），按模板 ER-05 / GC-01 **禁止**标 `in-progress`。
>
> **一致结论：** `plan-20260907` B3-00 已收口（`done`/`complete`，`v0.23.68`）。`plan-20260913` FL-00（Media）实已 `done`/`complete`，只剩计划级收口门。**`plan-20260926` DM-00 已 `done`/`complete`**（2026-10-07：取材清单 + #456 冻结 + R30 退役），`plan-20260819` 已登记取代。当前真正「零依赖且 review 已 PASS」的**下一个可开工**未启动候选是 **`plan-20260916` CAP-01**（需先重核 `DEP-CAP-01` monoengine pin）。`plan-20260822` OL-13 已于 2026-09-20 补记账为 `done/complete`（实现随 `9da06b4`/v0.23.0 发布）；OL-14 已取消（G-09 墓碑），OL-15A 已完成 runtime cutover，OL-15 为 `done/remote-pending`，等待 compat-offline-core 远端门禁。

---

## 五、延后决策或实施项（DEFER-* 汇总）

### 5.1 plan-20260920（拆 Code/Publish）

| ID | 内容 | 状态 |
|---|---|---|
| DEFER-RC-01 | `ai_*` 表 down-migration | 延后（对象闭包风险；冷冻即可） |
| DEFER-RC-02 | 物理回收 `refs/libra/intent` / `sessions/code` 对象 | 延后（doctor 残留报告稳定后） |
| DEFER-RC-03 | 重建内部 mutating 执行器 | 延后（用户明确不要） |
| DEFER-RC-08 | 为外部捕获另建用量记账 | 延后（产品要求捕获用量时重启） |
| DEFER-RC-04/05/06/07 | 独立 MCP / 版本面四→三 / automation 去留 / 删 `worker/` | **已关闭** |

### 5.2 plan-20260919（XDG）

| ID | 内容 | 状态 |
|---|---|---|
| DEFER-GCX-01 | 显式 `libra config migrate-global` | 延后 |
| DEFER-GCX-02 | 清理 `~/.libra` 残留空目录/旧 key | 延后 |
| DEFER-GCX-03 | `XDG_DATA_HOME`/`XDG_STATE_HOME` 全量 XDG 化 | 延后 |
| DEFER-GCX-04 | OS keyring 承载全域 unseal key | 延后 |

### 5.3 plan-20260918（add）

DEFER-AD-01..16：Git advice、ignored 相对路径、`add -u --ignore-missing` 128 语义、typechange/gitlink 暂存、全局 `-c`、其余 pathspec 开关、`add -i`/`--edit`、旧锁文件/超时键、`add -n` 零写入、`add -p` 会话（回 477）、gitlink 缺失暂存、Windows symlink fixture、批量 add 剩余耗时、conflict-marker-size、`add -n`/`-v` 对齐 Git 格式。全部延后（多为外部用例/对照集升级重启）。

### 5.4 其它计划

- plan-20260904 / plan-20260902：`DEFER-RG-SCOPED-04`（2026-10-10 用户将并发目录移动NO-GO与完整所有权/退出证明移出当前计划）。未修复；原reparent组合fixture保留ignored/UNRUN，不计PASS。04仅稳定目录命名空间功能/静态no-follow，继承到02/03/RG03/FAST-RG的同一限制；native Source A不变。重启须独立方案/Claude PASS及外部审批变化后真实回归；当前不实施、不是当前硬依赖，61卡计数不增。

- **DEFER 候选（2026-10-08 13:54:21 UTC 登记，来源：NPR-19 r3 Claude 独立审查 P3-4）**：`DEFER-CFG-RAW-SET-FLAG-INHERITANCE`——`ConfigKv::set_with_conn`（raw 存储 API）保留"历史 flag 继承"：调用方传 `encrypted=false` 而行已加密时按 `effective_encrypted=true` 落明文值；CLI/import 已经由 FIX-NPR-CONFIG-01 的 guard 保护，但约四十处生产调用（init/branch/clone/vault/auth/credential/hooks）仍走 raw API，需后续单轴卡让其走同一 guard 或记录不可走的理由。

- **DEFER 候选（2026-10-08 12:25:39 UTC 登记，来源：NPR-19 r1 Codex 独立审查的 inherited upstream 项，均在 FIX-NPR-CONFIG-01/NPR-19 写集外、基线既有）**：`DEFER-CFG-IMPORT-PLAINTEXT-FALLBACK`——`src/command/config.rs` import 在缺 unseal key/加密失败时回落明文却保留 encrypted 标记（单值 :4137、多值 :4094–4107）；`DEFER-CFG-TYPED-ERROR-VALUE-DISCLOSURE`——typed-value 转换错误消息内插输入值（:2170/:2188/:2192）；`DEFER-CFG-LEGACY-PANIC-PATHS`——`src/internal/config.rs` 遗留生产 `expect`/`panic!`（:3038 等 10 处）。三项需各自单轴 FIX 卡与设计双审后处置，不借 NPR-19 载体扩写。

- issues/468：`DEFER-DC-01`（精炼原语库实现移出 468，重启条件：DC-07 判 GO 且另立实现计划）；`DEFER-DC-02`（精炼产物持久化，重启条件：真实消费者出现并给出新鲜度语义）；`DEFER-DC-03`（诊断/性能落盘采集点，重启条件：另立 capture 面计划立项）；`DEFER-DC-04`（`collect.event.v1` 对 `stats.export.v1` 的实际适配，重启条件：#500 以 CS-00b=GO 重启）。
- issues/500：`DEFER-CS-01`（中央查询/聚合 UI，重启条件：独立查询契约）；`DEFER-CS-02/03/07/10`（仅 NO-GO，由 CS-18 登记各卡独立重启条件；卡进入 DEFER 后脱开初始 GO 判定，重启条件为维护者在 #500 确证数据集且新 CS-00b 为 GO）；`DEFER-CS-04`（Mega2 committed-set revoke/retention 与完整 artifacts read-surface authorization lifecycle，重启条件：DEP-500-14 接收方计划交付 upstream qualification harness，且 CS-16 metadata-read、CS-19 object GET/HEAD、CS-17 revoke、CS-20 bundle qualification 全部 passed）；`DEFER-CS-05`（冷冻表物理删除，随 plan-20260920 RC 策略）；`DEFER-CS-06`（接线 `stats.rs` 计数器，独立产品决策）；`DEFER-CS-08`（`--since` 导出窗口，产品要求时）；`DEFER-CS-09`（gated Libra live artifacts 写；仅在 CS-16 metadata-read、CS-19 object-read、CS-17 revoke、CS-20 bundle outcome=`qualified` 与 CS-11 实现后作 publication gate）；`DEFER-CS-11`（`plan-long.md:179/:795` usage 陈旧表述更正，随 `DEP-500-12` 移交 plan-long 维护者——非阻塞本计划）。
- plan-20261001-mega-browser-noninteractive：`DEFER-MN-01..09`（按名称取 tag、非根 tag path 与 tag 的 target/tagger 字段、文件条目操作、组合 move/批处理/轮询、成功状态码与可配置超时、stable code 映射统一、`path/provision` 与 `import-repo/remove`、mega2 仓内的 Libra 黑盒用例（由 mega2 plan-20261001 `DEP-BB-04`/`DEFER-BB-03` 承接）、`DEP-MN-04` 超时后的 live 证据降级）。
- plan-20260927：`DEFER-CM-01` 暂不机械拆分 `fetch.rs`、`push.rs`、`maintenance.rs`；生产职责本身继续增长或真实任务反复跨职责修改时，先重审源码与测试归属，再另立日期计划。
- plan-20260923: DEFER-CP-01 additional shells; DEFER-CP-02 network suggestions; DEFER-CP-03 unimplemented underlying capabilities. Existing local Libra capabilities may not be hidden by these deferrals.
- plan-20260924：DEFER-ACF-01..04 保留；DEFER-ACF-05 为 TurnEnd/nonterminal 自主恢复，按用户选择方案 1 延后，不保证 provider 重投或 turn-level checkpoint 补齐；DEFER-ACF-06（snapshot/extraction 既有 AgentKind dispatch，计数 ratchet 冻结）、DEFER-ACF-07（live_capture → subagent_content 依赖边）、DEFER-ACF-08（provider live capture 实现迁至各 builtin adapter）为 ACF-06 拆分新登记。
- plan-20260925：`DEFER-SCAP-01` 已按用户指示删除，ID 不再复用；`DEFER-SCAP-03` 不引入 Entire git phase；`DEFER-SCAP-04` 不改 `docs/development/tracing/agent.md`，也不向其它计划派发该文件。owner 的 SessionStart/TurnStart 豁免已经存在，本计划不改。
- plan-20260902：`DEFER-OG-01` 仍只延期 checkpoint 回灌（import，以及回灌流程中的 session delete / `session.remove`）。捕获插件在采集路径上调用 `remove`、`compact`、`create` 或其他会改会话的方法，以及填写会改变提示、压缩结果、标题、重试或网络帧的钩子字段，按 GC-OG-03 永久禁止，无 ADR 重启。观察并转发 `session.compacted` 留在本计划。读取 `parentID` 仍是 `DEFER-OG-02`。2026-10-03 15:47:05 UTC 起，未执行卡按模板 v2.12：nextest 全量门、三处版本面、每卡 ER-06a 文档字段、验收记录只写在计划文件内。2026-10-03 15:53:30 UTC：唯一 pin 为 OpenCode 2.0.22 @ `527f0b931d1f9b3ebd34e106c51b31ce5db5b075`；成功门为 Linux 与 macOS 都捕获到内容，macOS 还要先满足 export 子进程及其后代的取消安全隔离。Codex R72 字面 `VERDICT: PASS`（2026-10-03 16:54:47 UTC）。

- plan-20260902：**DEFER-OG29-PARTIAL-IMPORT（计划外，deferred/未实施）**：原样official29 GLM unsupported-state historical import020保持FAIL/默认拒绝；helper认证partial胶囊与direct-only持久化/identity两轴另待独立计划接手。BASE按当前G09只验namespace＋自然自动capture/checkpoint/普通export；USER-TEMP-REL-049显式不含该导入能力。原raw失败、live成功、clean ordinary正例分别记，不能互替/洗源。无新增Task、71/COV26分母和完成分子不变；非BASE/049/COV2前驱，不放宽Source A、V2 Partial零bytes、pending、candidate隐私或其它full/发布门。未来root登记的独立拆卡/精确写集/单Claude/实际原样import非零正例齐备后才可解除。
- plan-20260902：**DEFER-OG05-NATIVE-FIELDS**（用户 2026-10-12 原三字段延后；2026-10-13 Anthropic 路线后续授权）：本机 OpenAI-compatible 模型的两个已测 session 未返回三字段，此结论不外推。现在可用独立私有 OpenCode `.2` 与 Anthropic Haiku 4.5 在 OG-19 后、本机手工测试 `signature` 和 `redactedData`；`reasoningEncryptedContent` 真实正例仍延后。每个字段须 native Source A、当前受信 producer/observer/issuer、live/import/pending 原字节与零 ordinary sink 的独立正例，缺项 UNRUN/partial/不归档。OG-24..29 机制卡可逐卡推进；OG-05/FAST-OG-04 的三字段成功定义及 `SourceA=UNPROVEN` 不变。OpenCode 安装/账号/模型响应及 fake-exporter Session/Checkpoint E2E（含fail-closed）只做macOS本机手工、不进CI；纯parser/codec/静态guard和bwrap沙箱单测保留CI。
- plan-20260902：**DEFER-OG25-COVERAGE-V2 当前交接（2026-10-16；本次签推生效）**：schema-v2机制由COV2/DEP-OG-13=complete、v0.30.50 F `deabb37b70d74949a59c3722ad5cee1d8d79145a`实际C/D交接；OG-05原AC1/AC2真实typed正例、Source A及三字段仍未验收，不以schema交接充native PASS。**2026-10-13起草历史（旧“owner尚未source-ready/blocked”不代表现态）：** 当前 coverage-v1 只允许原四种 semantic record，OpenCode 可见 reasoning 不得作为第五种 `SemanticRecord::Reasoning` 直接落 v1。OG-25 当前 active AC3 只验安全 ordinary redacted partial/Incomplete；原 typed 目标未通过。未来独立 schema-v2、writer/reader/digest/恢复全路径一致性和迁移/兼容测试审查后，OG-05 原 AC1 的所有非空 text（包括候选同 part）私有 carrier 安全 typed-redaction 与 AC2 的 `SemanticRecord::Reasoning` 正例才可解除硬门；本遗留不增加67卡分母，不替代三字段 Source A。经 DEP-OG-13 记账，owner 已登记为 COV2 独立计划（尚未 source-ready，仍 blocked），当前 root maintainer 只保管交接；共享 `coverage.rs`、capture `snapshot.rs`/`live_checkpoint.rs` 及 writer/reader/digest/恢复路径在 OG-26 后、OG-27 前预留排他单写者 slot，实际 exact 写集、迁移/兼容与 Claude 审查必须由未来卡另定，签名 main 交接前 OG-27..29/05 仍 NO-GO。
- plan-20260902 → [`plan-20261013-coverage-schema.md`](plan-20261013-coverage-schema.md)：`DEFER-OG25-COVERAGE-V2` 的初始19卡签名采纳仅为历史分母；此前已签 G-09 修订为23卡（20 implementation/2 audit/1 release），该历史规划协调已由最终字节单Claude PASS/raw `77d4820e329ade99f5fba7b43821d072d4d7068ee5ba639ada4704851686f4fd`、签名DCO main `eaa43ec13f6c50212f0b3c2fa8a51799ec74bff5` 交付；已采纳 COV2-23/24 后25卡为历史；本次拆出 COV2-25 后现为26卡（23 implementation/2 audit/1 release），COV2-19 已由本机 A/B、双 env full 10084/10084 exit0、唯一 Claude PASS/raw `64068902908f4c521e380dc7e5cf593134eaa99697de208344b65f75b3c3caa8` 验收，signed+DCO main `354ff768c406bdad74607da62002bff810147c24` 已 Push、remote 同 SHA；COV2-20 已完成本机 A/B、唯一 Claude PASS 与 signed+DCO main `aa0ea018ddd044f09d62a88428e812a359cef8ed` 交接；COV2-21 已由本机 A/B、唯一 Claude PASS 与 signed+DCO main `6b24d797583afe1c0265f6e9fcbd364566671167` 交接；COV2-23 唯一 Claude PASS、signed+DCO main `d977d13c5e23111b93d78477b9e1a70015a111b8` 已交接；COV2-04 五项 focused 各1/1、fmt/strict 与唯一 Claude Code PASS，signed+DCO main `03b10f887368a3db5206589f5dd6947432ffe26c` 已 Push 且远端同 SHA；COV2-24 三项 VER 与七项继承各1/1、T-5 10103/10103 exit0、唯一 Claude PASS 与 signed `c56de683acf71d7327436eef7ef9185d97dcff27` 已交接。COV2-20 旧 CI 因新提交取消，不能记完整通过。`DEP-OG-13` source-ready 需 COV2-17 当前22审计名（21继承+1当前格式替代；另1legacy FAIL计划外延期，去重required unique26）本机最终源码非skip PASS、23张 implementation 源卡实际 signed main 与单Claude；complete 需 COV2-REL-01 stable patch C/D。COV2-14 已签 main `4558af64c09e63c4716fa68595776a8044f66cc7`，COV2-24 signed main 后历史25卡口径为 formal0/25、source-ready12/25=48.00%；本修订当前26卡口径 formal0/26、source-ready12/26=46.15%，父计划55/67不变；COV2-03 当前五项 VER、五项旧 v1 ER-04 与两项缺表回归各 1/1，full-r3 失败已修但不计 PASS；最终双 env full **Summary [1618.814s] 10078 tests run: 10078 passed (14 slow), 23 skipped**/raw SHA `c49dd3c33652d55541a0e4c0269c817460b9109cf261eab5271d90760a49e8a9`、exit0，唯一最终 Claude Code PASS、P0/P1/P2=0、P3=5/raw SHA `9ef25f92615eefcf2019212a306cfa7542ef416b693286bb99111006b3ea6df7`，signed+DCO 源码/文档 main `d717e42e0d1ec75e6ae576b21242fd1519040abd` 已 Push、remote 同 SHA；此前23卡 G-09 r4 单 Claude FAIL（2×P2）保留，r5 PASS 已由上述 raw/commit 证明。2026-10-13旧安装版0.30.48 LBR-AGENT-005拒绝私有 `.2` 仅为负面事实（收据SHA `81c9d1ec7142785357c9b121bc3edf5c0700ed27f974874d1a7ff86e1a7bacb1`）；stable patch后须重验原issuer epoch0，synthetic或SQL probe不构成Source A。
- `DEFER-COV11-IMPORT-TERMINAL-01`：计划外遗留（非新Task，不计26/71分母）。保留旧classic `info.status="stopped"`历史import实际LBR-AGENT-020/FAIL、原fixture/body和默认拒绝；本G09把COV16对应旧终态兼容验收及其classic info/parts端到端、session行started_at/last_event_at映射缺口明确延期。本轮以当前格式功能正例替代，不声称旧合同或这些DB时间映射已PASS；此前“不免除COV11原AC/VER”仍只约束COV11，不得拿来撤销本次COV16前瞻改选。重启须另有界兼容任务：保留来源准入安全性、Claude Code审查后，原legacy用例实际非skip1/1并读回session时间与stopped_at；真实行为变化须重跑适用full和最终本机审计。未满足前不解除延期、不改旧FAIL。
- 2026-10-13 20:58:21 UTC COV2 G-09 历史修订：COV2-04 的同源 native pending-v2 借用门拆为 COV2-23、versioned inner projection+内存 seal 接线拆为 COV2-24（`pending_payload.rs`→`pending.rs` 两个生产落点）；严格边 COV2-21→23→04→24→22；22 implementation+2 audit+1 release=25 卡，COV2-03 signed main `d717e42e0d1ec75e6ae576b21242fd1519040abd` 后 formal 0/25、source-ready 6/25=24.00%，父计划仍 55/67。历史 19/23 卡收据不回写；COV2-23/24 全 AC/VER UNRUN，COV2-19 开工前核本协调唯一 Claude PASS raw SHA、签名 main SHA 与 remote 同一性，并在首个状态同步登记收据，DEP-OG-13/Source A/C-D 未放行。
- plan-20260830：`DEFER-SBX-06` 发布步延后（DEP-SBX-05 未就绪）。
- plan-20260729：`DEFER-09`（CT3-07 转换轴）——已被 plan-20260825 TA-01/02 + plan-20260827 NP-00 承接关闭。
- plan-20260819：`DEFER-M2-01..09`（含 `DEFER-M2-09`：AgentRun/session 终态适配，M2-16D 判定不可映射时启用）。
- plan-20260822：`DEFER-01..03`（Operation Log 范围外）；`DEFER-02`（Web 图 SSE）已随 OL-14 取消关闭；`DEFER-05` 已由 PR #503 的 OL-15A 承接并关闭；OL-15 等待远端兼容门禁收口。
- plan-20260903：`DEFER-01..12`（merge 范围外/deferred 差异）。
- plan-20260715：历史封存；Code 专属 `DEFER-01..08` 已由 plan-20260824 交付后拆除或由 plan-20260920 直接墓碑化，`DEFER-09/10` 已完成关闭；无现行可重启项。
- plan-20260824：历史封存；全部 Code 专属 DEFER 已由 plan-20260920 墓碑化，无现行可重启项。
- plan-20260825：历史封存；Code 专属 `DEFER-PS-01..04` 已墓碑化；`DEFER-PS-05` 已由 plan-20260917 关闭。仅通用测试基础设施 `DEFER-PS-06/07` 保留为另立计划候选，不得用于恢复 Code。
- issues/498：`DEFER-TT-01..08`（tree/blob 目标、nested-tag advice、`-f` 覆盖措辞、show/log 嵌套展示、传输非 commit tag、`-n` gpgsig 标题、umask 敏感测试、`for-each-ref`/`describe` 嵌套 peel）。计划已收口；DEFER-TT-02/03/06 → `issues/477.md` 的 477b HW-05；DEFER-TT-04 → 478；DEFER-TT-05 → 474/480；DEFER-TT-08 待新建 issue。

---

## 六、跨计划依赖（DEP-* 现行生效项）

| DEP-ID | 类型 | 内容 | 现状 |
|---|---|---|---|
| DEP-MEM-R6-HANDOFF | 跨计划缺陷移交（outgoing：plan-20260904 FIX-RG-01 → plan-20260926） | FIX-RG-01 R6 组合源码的独立 Codex 原审（本机路径 `/private/tmp/libra-review-original-reports/fix-rg-01-r6-codex/codex-verdict.md`，不可公开复现；SHA-256 `9a4373126529a2187e720d299a6a42a4a0ab9e08585b78bfe844b1b46ac5d46f`）在上游 memory 模块（`src/command/memory.rs`、`src/internal/ai/memory/**`，F4 另见 `src/internal/change/store.rs:273`；DM-13（已 done/complete）与 DM-03（代码经 #623 e237365d 合并，卡已 `done`/`complete`（2026-10-09））交付、与 main 913efe3d 逐字节相同）发现 F1–F10 共 6 P1/3 P2/1 P3（占位子命令静默 exit 0、rebuild 非原子、跨来源删除、horizon 窗口 clamp 1..200、meta 解码吞错、fingerprint 域不完整；revoked/aged-out 计数硬编码 0、horizon 截断不可判定、freshness 契约无实现；`memory show` 示例无效），详见 plan-20260904 2026-10-08 10:41:48 UTC 记录。不在 FIX-RG-01 prod0 写集内，FIX 不改 memory；由 plan-20260926 owner 在 DM-11/DM-12 前处置 | 生效（2026-10-08 10:41:48 UTC 登记；不阻 FIX-RG-01 version-only 0.30.39 发布） |
| DEP-468-01 / DEP-468-02 | 跨计划移交（outgoing） | issues/468 的 DC-09/DC-14 分别交付 `collect.event.v1` 对 `stats.export.v1`（issues/500）与 episode 字段（plan-20260926）的映射说明；只登记映射，不消费、不修改那两面 | 生效（468 未开工；接收方未消费不阻塞 468 收口） |
| DEP-468-03 | 跨计划前置（incoming） | issues/468 DC-01 盘点消费 ACF（plan-20260924）/SCAP（plan-20260925）落库的事实表（`agent_session` / `agent_checkpoint` 等） | 已满足（两计划均已收口） |
| DEP-468-04 | 跨计划移交（outgoing） | 468 的价值门 GO 结论授权后续实现计划开工（精炼原语库，含 ADR-DC-02/03 边界） | 生效（468 未开工；DC-07 非 GO 时不立项） |
| DEP-MN-03 | 跨仓交付（outgoing） | plan-20261001-mega-browser-noninteractive 交付 `mega2 browser` 非交互操作、机器契约与 live 门；接收方是 mega2 plan-20261001 的 Libra 域（`scripts/libra_smoke_storage_only.sh`；`DEP-BB-04`/`DEFER-BB-03`；`BB-65` 起编号已预留）。Libra 仓内的测试不受 mega2 工具链规定约束；任何一方都不得向 mega2 的 curl + git smoke 脚本加入 libra 用例 | **已交付（2026-10-03 13:05:47 UTC）**；功能下限 stable v0.30.26，live 参考与当前 stable v0.30.27；[契约](https://github.com/libra-tools/libra/blob/faae20c6d9e48c46b9bb290637a8425b6d1e073a/docs/commands/mega2.md#non-interactive-operations)、[live 门](https://github.com/libra-tools/libra/blob/faae20c6d9e48c46b9bb290637a8425b6d1e073a/tests/command/mega2_browser_noninteractive_test.rs#L2651)；lightweight tag 可能列表缺失或跨页重复，annotated tag 按名称分页。接收方在其 Libra 专用脚本追加，本计划不等待接收方 |
| DEP-MN-05 | 跨计划信息移交（outgoing） | plan-20261001-mega-browser-noninteractive 新增的 `mega2 browser` 操作 flag 进入 plan-20260923 CP-19 的能力台账；双方无实现写集交集 | 生效（信息性，不阻塞任一方） |
| DEP-SA-03 | Issue #577 与 plan-20260927 共享写集/发布窗口 | #577 SA-01 使用 `tests/command/mod.rs`、serial registry/nextest、版本面；与 CM/FIX 命中相同文件的实施和全部发布顺序串行，前卡推送后重基重测。SA-02 网站指南先行，其网站 `cf` 写集也须核对其它计划实际页面写集。 | **已解除（2026-09-27）**；#577 经两次主线重基、最终全量、PR #578、v0.24.1 和网站 D 门完成，SA-02/SA-01 均 `done`/`complete`。后续 CM/FIX 从已发布 main 重新核写集。 |
| DEP-CM-01 / DEP-CM-02 / DEP-CM-03 | 命令写集、发布窗口与 Cloud L3 | 13 张 CM + 9 张 FIX，共 22 卡；开工前核实际同文件写集。FIX-CM-01 → CM-01，FIX-CM-04 → CM-04，CM-05 → CM-13，CM-07 → FIX-CM-08 → CM-08，CM-03/CM-13 → FIX-CM-LIVE-GATE → FIX-CM-CLOUD-RECOVERY-AUTH → FIX-CM-CLOUD-RECOVERY-CLEANUP → FIX-CM-CLOUD-REPO-SCOPE → FIX-CM-CLOUD-LIVE-SAFETY → CM-10 → CM-11。其它全部卡 C/T-5 等 scope FIX 独立发布。`tests/INDEX.md` 全写者按 FIX-CM-01 → CM-01 → CM-06 → CM-02 → CM-03 → CM-07 → FIX-CM-08 → CM-08 → CM-09 → CM-12 → CM-04 → CM-05 → CM-13 → FIX-CM-LIVE-GATE → FIX-CM-CLOUD-RECOVERY-AUTH → FIX-CM-CLOUD-RECOVERY-CLEANUP → FIX-CM-CLOUD-REPO-SCOPE → FIX-CM-CLOUD-LIVE-SAFETY → CM-10 → CM-11 串行发布，CM-03 → CM-10 另串行写 serial registry；`CM-Publisher` `/root` 精确暂存。真实 Cloud L3 只在受保护 release-SHA CI dispatch 写 job 执行，按安全卡/CM-10/CM-11 各自 `E-CM-L3-SAFETY/10/11` 证独立期望、D1/R2 只读、trigger 全 SQL 基线、全库加密备份 runner 外上传读回、global/逐例 manifest、限定补偿/清理。 | 生效；CM-01、CM-04 `blocked`/空，CM-06、CM-10 `in-progress`/空，其余 17 卡 `pending`/空，均未发布。用户精确期望已确认，四 vars API readback 4/4；D1 25 triggers 中 16 remote-existing/unmatched 尚待 owner 核签。DEP-CM-03 与三个 E 仍 `unknown`；本地仅 fake/mock/default C，不执行真实 D1/R2 写。 |
| DEP-CM-04 | CM-07 网站 `cf` 开工基线 | 仅要求开工前只读核后端 VCS、远端 `cf` pre-SHA 与 `status.en.md` blob、隔离 clone、签名/DCO/普通推送配置权限和站点命令可用。网站页编辑、新源码锚点/porcelain-v2 修正、gen:docs/typecheck/build、签名推送、远端及实际部署证据，均为 CM-07 自身交付门；已推后失败按远端 post-SHA 条件化签名补偿，远端竞态/部署不确定进入 `remote-pending`。 | 开工基线未重核，网站未交付；DEP-CM-04 不要求先完成 CM-07 网站变更，避免自环；只 Git push 不算部署。 |
| DEP-CM-05 | Cloud CI 首写与发布门 | `vars-ready`：四项非密 repo vars 最后一项由 root 于 2026-09-25 08:35:02 UTC 设置，GitHub API exact readback 4/4 与用户确认 tuple 一致；workflow id `283278914` 于 07:27 UTC 禁用、API `disabled_manually`，无待/运行 job。`premerge`：安全卡先交付、CM-10/11 各自重基复核；本地/mock 负例证首写前 secret→独立 vars 比对、D1/R2 只读、trigger/schema/lease 预检、全库 SQL/Time Travel bookmark `age` 加密 runner 外 artifact 上传读回、global/逐例 manifest、失败非零/no-skip；只阻合入，不要求 release SHA 或不可读真实 secrets。`postrelease`：先将七项 Cloud repo secrets 迁受保护 environment、移除 repo 副本与 `base.yml` 默认注入，删除 schedule；独立 `cloud-live-recover.yml` 受保护恢复入口、custodian、dry-run，root 才能 enable/API 核 active。每卡自己的 release SHA 经受保护 `workflow_dispatch` nonce/run 授权，由 `genedna` 设置并回读精确 grant、无需独立 actor 审批；job 首步用仅 Variables-read token 最长 10 分钟实时 GET，严格绑定 run/attempt/ref/SHA/nonce 后才映射 Cloud 凭据并运行真实身份/备份/manifest、两个完整 target、写后善后与 artifact 后才可 D/complete。 | `vars-ready=passed; premerge=unknown; postrelease=unknown`。七项凭据迁移、protected environment、独立恢复与真实 release-SHA CI D 尚未执行；workflow 仍 `disabled_manually`，root 是恢复 owner。当前无真实 Cloud 写；不能以旧 `skip=true`、本地结果、旧 SHA 或被过滤目标替代。 |
| DEP-OG-12（final C：本轮撤销；原证据及FAST04真实字段等待只为历史、非 satisfied；不构成本轮前驱） | OpenCode 私有 producer 构建/注册来源前置 | 固定 `2.0.26-libra-native.2` 已审源码、selected binary、确定性构建配方；生产启用另需隔离 macOS 安装、实际选中 plugin/transport、fresh key、显式 exporter trust、epoch-checked issuer 注册与真实字段 receipt。 | **仅 binary-to-source 子门 PASS**：clean r4 独立重建的 4256 个编译输入描述与 metafile 语义相同；285 处 Bun nonce/hash 差异经窄算法重建后与 selected 187635570 bytes 逐字节一致，Claude Code Sonnet 单审 PASS。冻结索引 SHA-256 `ff196ff002474d16b61c434c456729e3a95bd4ca790956d9282ecffaefaca055`，持久本机副本 `/Volumes/Sea/.libra-private-evidence/og12-20261013/MIGRATION-MANIFEST.json` SHA-256 `7bbd73e49e1dff35f8515a4af72376189a9481d4df5e73a2233470fcd0418b43`，副本 30 文件哈希一致且 selected/reconstructed `cmp` 退出0。2026-10-13 10:49 UTC 独立 r2 再核：raw Bun 产物 SHA `1b8613aaa2318880d23eaa944ba7b89bde32ce3bd60b0e2aac9a426802b601d4` 与 selected 不同，经窄变换后逐字节相等；8/8 gate exit0，索引 SHA `b9071d9e5c22ef6eeecb2fa447137719a9a3189282a0adf496e37f07eec57bf1`、Claude raw SHA `a4f933f6875c2c41422b183d3081769c424d279949f306d444c96958140e85eb`。该 Claude 对有限 binary-to-source 子门 PASS，但5个 P2 分属脚本执行时绑定、canonical/raw manifest、selected binary 对 plugin/policy digest 绑定、clean lockfile 来源、tarball/member 等价后续门；不宣称 raw deterministic build、独立供应链、运行时 transport 或 Source A。 | **整体未闭合**：选中 runtime/plugin/transport、issuer/撤权、Libra Source A 和完整归档均未验；OG-27/28/29/05 与 FAST-OG-04 仍受 coverage-v2/真实字段门阻塞。OG-25 已交接；OG-26 的 DEP-OG-07 镜像、本机 A/B 与单 Claude 已通过，signed+DCO main `734b6f03884d3aa38400acbbfd446b557e642d94` 已交；FAST-OG-04 C/D 仍待发布；复杂账号/模型正例只在 macOS 本机手工。 |
| DEP-OG-13 | 已完成coverage-v2跨计划源/发布交接 | COV26/26及v0.30.50实际源/全量/四平台/网站/安装/原2兼容证据保留 | **complete**；原向OG-27的新native生产交接不再有本轮接收者，普通已交付接口仍可消费；不重开COV，不授SourceA |
| DEP-ACF-MIRROR | Agent Capture 架構前置 | 0902/0904/0905/0911 production 卡的 `DEP-ACF-MIRROR` 前置已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接 | 已交接 |
| DEP-ACF-CAP | Agent Capture 必要而非充分條件 | `DEP-ACF-CAP` 的 ACF 側必要條件已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）交接；CAP-01..03 僅可 zero-raw-persistence 重核，CAP-04..07 另須獨立 security/privacy RFC 與新卡，維持 `blocked` | 生效（ACF 側已交接；CAP-04..07 仍 `blocked`） |
| DEP-ACF-DM06 / DEP-DM-06 | Agent session 語義契約 | 正式 handoff 已由 ACF-09（`Lifecycle=done`、`Acceptance=complete`）完成。0926 DM-05 開工日仍須按 ER-02 重核 terminal、explicit CLI resume、live reactivation、import reactivation | 已交接；DM-05 目前 `pending` |
| DEP-CP-01 / DEP-CP-07 | External prerequisites | Authorized backend cf documentation access; isolated env/nextest/five-shell validation | Unverified; block applicable acceptance, no access inferred from plan |
| DEP-CP-02 | File exclusion | Completion CLI edits join DEP-CLI-mirror reservations | Pending per-card reservation |
| DEP-CP-03 | Read contract | Completion config reads must not trigger plan-20260919 migration | Pending read-only API and legacy-only fixture evidence |
| DEP-CP-04 | Release/window exclusion | Respect plan-20260921 REL-VG-01 file reservation | Verify before intersecting edits, not only before version bump |
| DEP-CP-05 | Contract/file exclusion | issues/476/478/480 command evolution and overlapping files | Refresh actual parameters and reserve conflicting files |
| DEP-CP-06 | Agent read contract | plan-20260819 models and identity-scoped readers | Verify before CP-13..15; does not block static stage |
| DEP-AD-12 / DEP-CLI-mirror | 跨计划写集互斥 | `src/cli.rs` 三态串行：plan-20260918 OI-05、plan-20260912 MB-03/05、plan-20260916 CAP-07、issues/483 CO-03/04、plan-20261001-mega-browser-noninteractive MN-03（仅改 `src/cli.rs:2006-2009` 注释，`DEP-MN-06`）、issues/500 CS-13 | 生效；OI-05、MN-03、CS-13 开工前必须核对 | <!-- CX-30为只读audit，无src/cli.rs写权。 -->
| DEP-GCX-02 | 跨计划写集互斥 | plan-20260919 与 plan-20260918 的 `COMPATIBILITY.md`/docs/网站页串行 | 生效 |
| DEP-FL-04 | 跨计划前置 | plan-20260913 依赖 plan-20260907 完整收口 | **已满足**（plan-20260907 已收口，B3-00..B3-17 全 `done`/`complete`） |
| DEP-CC-05 | 跨计划shared I互斥 | plan-20260905 CC-02后续按实际文件与CX/PI/HM及其它writer串行交接；不再等待9/04所有卡完成 | ACF/SCAP/RG/COV2具体产物已交付，目标dirty或writer in-progress仍阻塞 |
| DEP-SCAP-01 | 跨计划前置 | 改变 AgentTraces ingest 的 state 字符串或 checkpoint 类别的卡，等待 plan-20260925 SCAP-01 `done`/`complete` 后改 `decide`。import 与 session stop/resume 不在此列 | SCAP-01 已 `done`/`complete`；后续采集卡可改 `decide` |
| DEP-SCAP-03 | 跨计划写集 | OG-11、OG-05、CX-06、CX-28、CX-02、CX-07、CX-19、CX-09、CX-26、CC-02、CC-05、PI-04 在 SCAP-01 complete 前不得 in-progress | SCAP-01 已 `done`/`complete`；名单卡可开工（仍受各自计划门控） |
| DEP-SCAP-04 | 跨计划写集互斥 | plan-20260925 SCAP-01 与 plan-20260907 B3-00 不得同时改 `runtime.rs`。SCAP-01 C 组落地后互斥放开 | SCAP-01 已收口；互斥解除 |
| DEP-SBX-06 | 内部发布延后 | plan-20260830 SBX 发布步 | 未就绪 |
| DEP-05 | 内部前置（已承接） | plan-20260822 v1-boundary runtime cutover（branch/sequencer/worktree repair/v1 op restore → v2 middleware）；由 OL-15A 承接，OL-15 依赖其完成 | PR #503 OL-15A 已完成 |
| DEP-WT-09 | 跨计划前置 | issues/476 关闭依赖 plan-20260918 WT-05..07 完成 | plan-20260918 未到 WT |
| DEP-FM-06 / DEP-FM-07 | 跨计划前置 | issues/470 关闭依赖 plan-20260918 FM-03/FM-04 完成 | plan-20260918 未到 FM |
| DEP-AD-06 / DEP-AD-07 | 跨计划写集互斥 | plan-20260918 与 issues/483（CO）、issues/490（SW）、issues/479（PL）的 `add.rs`/index 写集串行 | 生效 |
| DEP-PL-04 | 跨计划写集互斥 | issues/479 与 plan-20260918 IA-01/IA-02（`add.rs`）串行 | 生效 |
| DEP-CO-04 | 跨计划写集互斥 | issues/483 CO-03/04 依照 DEP-CLI-mirror 当前写者及 `src/cli.rs` clean/in-progress 状态逐文件串行；CX-30现为只读audit，无该文件写权 | 生效；CO-03/04 开工前核对，不据此豁免其它门 |

---

## 七、完成计划清单（已收口）

日期计划：`plan-20260708`、`plan-20260713`、`plan-20260714`、`plan-20260715`（历史完成、Code 产品面已拆除）、`plan-20260818`、`plan-20260821`、`plan-20260824`（历史完成、Code 产品面已拆除）、`plan-20260825`（历史完成、PS 产品轴已拆除；TA 测试轴保留历史）、`plan-20260827`、`plan-20260901`、`plan-20260910`、`plan-20260917`、`plan-20260920`。

Issue 计划：`issues/477`（31 卡，v0.22.49）、`issues/486`（AB-01，v0.22.31）、`issues/497`（CD-01..CD-04，v0.27.2；PR #581 squash merge `859d7fb`）、`issues/498`（TT-05/TT-02，v0.30.22 / v0.30.23；#498 CLOSED）。各自完成判据见对应计划文件。

### 2026-10-07：FIX-RG-SSH-01 独立0.30.35发布提案（审批前）

plan-20260904新增ER10前驱FIX-RG-SSH-01，in-progress/locally-accepted，7/7AC、3/3VER。实际main c0eaae5/package0.30.34上的test-only SSH及版本面0.30.35五文件候选macOS全量9507/9507 PASS（13slow、2LEAK、5skip、817.873s、exit0）；SSH族32/32仅single-threaded diagnostic，旧并发31/32历史保留。两LEAK各三次无重试孤立PASS、根因未明；完整R2 blob diff及installer LF/未改hook EOL/PTY parent边界见任务卡。R2限定发布提案只追加本卡和本状态段，不引入尚未交付的RG/OG实现或aggregate dirty写集；七文件最终树C组未执行、独立双审材料修订待复核，未commit/push/PR/merge/tag/release，不能记done/complete。Linux功能由用户后验；新版本不是tag预留，发布前重新核对actual refs。

### 2026-10-07 14:00:42 UTC：用户指定#618顺序后的当前0.30.36发布候选

用户明确要求「上游已经有一个PR更新到0.30.35，#618编号，请顺延发布新的版本」；当前active版本改为0.30.36，0.30.35留给#618。该指令授权当前独立FIX-RG-SSH-01发布及必要签名提交/推送/PR/checks/merge/tag/release步骤，先完成具体候选验收与双审，不再把旧“未获发布授权”的历史状态当当前决定。只处理本卡切片，保留root其它dirty及native Source A NO-GO；Linux功能仍由用户后验。

最新只读事实：#618 open/head5a5e5e2b4dca21e2e1e7357075c7f806286291f9当前三版本面仍0.30.34；main仍c0eaae5d9bf4e9cfae035d61b759b0ac80799db7，refs查询未返回v0.30.35/v0.30.36。0.30.35是按用户发布顺序留给该PR，不冒称已有tag或已合并。本0.30.36树目前只是c0eaae5上provisional准备；正式C组必须重新pin到#618实际合入且package0.30.35的main，按真实patch+1重新检验，并等待其发布窗口结束后发布本卡。不得把未合并#618 migration夹带进本卡I或替别人merge该PR。

旧完整0.30.35五文件候选9507/9507绿保留为历史；追加卡/status后的七文件重验已因本次顺延仅向owned Nextest SIGINT，中止结果7396/9507 passed（10slow、3LEAK、5skip）、2111未运行、exit100、源1982/1982字节/mode MATCH，明确CANCELLED/SUPERSEDED而非PASS，未运行该轮release/private install。新0.30.36只改三版本面，Cargo metadata --offline由工具链刷新self package lock一行，无依赖变化；SSH代码逐byte相同。当前无正式C/D完成或publication事实。

### 2026-10-07T15:30:36.955151+00:00：0.30.36最终基线准备

实际merged main=9a92e7fbf3af34ce8cbf9b1a7a5b9574b18ef83f、package0.30.35，包含#618 merge=c384bc7757ccea6f47b70df2cdd72fb528860713；v0.30.35 tag=c384bc7757ccea6f47b70df2cdd72fb528860713，唯一release workflow=37641579067 completed/failure，先前窗口已结束；不把其失败写成绿色。本候选是该真实基线上的0.30.36 patch+1；此前c0/package0.30.34只是provisional历史，未用旧全量绿代替本树验收。

仅SSH cfg(test)期限切片、三版本面与toolchain self-lock、FIX卡/status文档，既有upstream代码不成为本卡I写集。完整patch zero-offset/fuzz准入，尚待本最终冻结树的focused/full/fmt/strictClippy/build/install及fresh独立双审、签名commit/push/PR检查/tag/release/D证据；不得提前complete。用户已授权0.30.36发布，Linux功能仍由用户后验；Source A native NO-GO及其它任务卡门不变。

上游.35 tag与package补交提交不一致的事实独立保留；本卡既不改写他人tag也不冒认该窗口绿色。窗口terminal后前滚发布自有0.30.36；发布前重新核对main/tag与串行占用。


### 2026-10-07 22:04:44 UTC：FIX-RG-01 最终基线与全量事实

R2 FIX-RG-01当前独立0.30.38候选基于实际上游84442ee658bff007f9eed5b7b996638990610b45；保留CP-00已发布四路径，不回退上游审计交付。修复owned仅两cfg(test)期限、三version面/工具链lock、本卡plan及本status。R1旧f59 full与originals冻结保留，不宣称最终全量/C-D完成。原审两个P3修正为caller-supplied reader deadline和六特有AC/8、五VER/8，强制full另列，.37保留。本1985文件冻结源的fmt/strict/focused与双环境默认并发Nextest已结束：exit0、9514/9514passed、13slow、4LEAK、5skip、0flaky，817.894s；四LEAK根因未明，待逐项隔离复验，不宣称零泄漏。两原审已各自CLOSED PASS；只在full结束后修正本卡I五处RPC参数事实及本状态UTC标题，1983非记录文件与模式保持一致。新文档双审、最终树绑定、build/install/签名提交及实际C/D仍未完成；本卡继续in-progress/空。


### 2026-10-08 03:40 UTC：#620 发布前置已满足，#621 合并后重新验收

使用者要求的顺序已满足：#620 merge 125657b 与 v0.30.37 release run 37721401960 八作业全部 success。#621 私有候选从 a242401 正常合并实际上游 e95b8bd（含 DM-02 六新增文件及发布状态），版本四面冲突保留 0.30.38；签名 DCO merge 742517d。原 9514/9514 与旧双审不代替新增功能合并后的 full/fresh 双审，当前均待执行；FIX-RG-01 继续 in-progress/空，实际 C/D 及发布未完成。


### 2026-10-08 03:57 UTC：#621 合并后本地门通过，发布仍未完成

FIX-RG-01 R4实际1991源逐门前后MATCH：fmt/strict/聚焦与双环境默认并发Nextest exit0、9522/9522、13slow/1LEAK/5skip/0FLAKY（798.323s测试、832.229s进程）。唯一LEAK为未改merge_ext_driver_clean_result_is_read_from_percent_a，一次精确隔离1/1无LEAK，根因未明、不称修复。独立Codex/Claude两原稿CLOSED PASS，Claude既有RPC注释一P3非阻断；两原稿先关闭后父级完整读取，final树/文档门和构建/安装/签名、实际PR CI/merge/release八作业与线上安装尚待关闭。full之后仅本卡两处记录变更，1989非记录源字节/模式不变。旧9514原记录不改，本卡in-progress/空。

### 2026-10-08 10:41:48 UTC：FIX-RG-01 #621 已合并，0.30.39 独立发布候选本地门通过

#621 由 genedna 于 09:46:06Z squash merge 为 main 913efe3d60ca0a714eac240e75059e7f03fbdd8e，12 项检查 SUCCESS；v0.30.38 已被 DM-13（99c4bce）占用，故本卡 patch+1 顺延 0.30.39。候选分支 release/fix-rg-01-v0.30.39 仅改三版本面与 self-lock 一行；源码与 FIX-RG-01 私审第 6 轮（R6）私有合并 f224a0be diff 0 行（R6 运行树 = f224a0be + 同一未提交 version-only bump），R6 full 9530/9530（13 slow/1 flaky/1 leaky/5 skip）按 version-only 例外复用。本树 fmt/strict clippy/`cargo nextest run --test compat_version_surface_sync` 2/2/`cargo build --locked --release`（实测 libra 0.30.39、91702144 bytes、SHA-256 ab95ea24a900cc09493d76d4a8ba1314b3f233cd27dd61562a0acf53e0cefab4）全部 exit 0；R6 full 原始日志与 R6 原稿为本机路径，SHA-256 登记于 plan-20260904 同时间记录。R6 Codex 6 P1 均属上游 memory 模块，登记 DEP-MEM-R6-HANDOFF 移交 plan-20260926；fresh 双审、签名提交、push/PR/CI/merge/tag/release/安装 smoke 待完成，FIX-RG-01 仍 in-progress/locally-accepted。执行会话切换为 Claude Code（执行者陈述：当前唯一会话、承接 ER-12 publisher）。

### 2026-10-08 10:48:46 UTC：FIX-RG-01 0.30.39 候选 delta fresh 双审最终双 PASS

四轮只读独立审查（Codex gpt-6.1-sol / Claude claude-fable-5-1，彼此不读对方报告，每轮前后 1994 跟踪文件清点零漂移）：r1 双 FAIL（概览行卡态无树内支撑、证据附件缺失）、r2 Claude PASS/Codex FAIL（概览行改写基线）、r3 Claude PASS/Codex FAIL（intake 时间戳过期）、r4 **双 PASS**（Codex P0/P1/P2/P3=0；Claude P0/P1/P2=0）。各轮报告 SHA-256 登记于 plan-20260904 同时间记录。随后执行签名 DCO 提交/push/PR/CI/merge/tag/release/安装 smoke；FIX-RG-01 仍 in-progress/locally-accepted。

### 2026-10-08 12:49:33 UTC：NPR-19 / FIX-NPR-CONFIG-01 0.30.40 发布候选树已组装（未发布）

基线公开 main 9b8fc5c7057e6e317edd88f1518ba1bb25f2929f（#624 已 squash merge；FIX-RG-01 0.30.39 的 release run 37777413227 当时仍在进行中）；owned 7 文件与 compose 树逐字节一致（实现源相同，compose 树的双 env full 9539/9539 按 version-only 例外复用）；三版本面 0.30.40。**本次 macOS 门与审查（均在实现源相同的 compose 树上完成，最终树仅加版本面与 plan 记录）：** `cargo +nightly fmt --all --check` 0；`cargo clippy --all-targets --all-features -- -D warnings` 0；NPR-19 四具名门（`npr_19_literal`/`npr_19_case`/`npr_19_exact` 3/3、`npr_19_cli_registry` 1/1）与 FIX-NPR-CONFIG-01 五具名门（CLI 3/3、unit 2/2）全过；`command::config_test::` 177/177；`compat_command_docs_examples_section`/`compat_error_codes_doc_sync` 3/3；双 env 全量 `cargo nextest run --all --no-fail-fast --retries 2` 9539/9539（22 slow、1 leaky `command::branch::tests::test_format_branch_name_with_full_remote_ref` retries 0 隔离另记、5 skipped、964.871s，source manifest `68f8b0ac2c39809e57f4bb1a05805f169ef529574db62a78bdee6b0784067749`）。fresh 独立双审 r1（代码+文档 delta）：Codex PASS 0/0/0/0（SHA-256 `00bc7b3f3fbd60e2567a4d855e9cd4cda72c33ac828deed628dc19af16c5312b`）、Claude PASS P3=3（SHA-256 `c75fda07c12859aad5b850cee10b158849ea94e08ef90c153078d0deea5d260f`）；三项 P3 已采纳（import 命令错误边界复用 `config_plaintext_replacement_error()`、rollback 失败串含原拒绝原因、`--plaintext` 表格/COMPATIBILITY 措辞与 import 节各一句）并重跑上述门；Codex 另列三项基线既有 inherited P1 已登记 DEFER 候选（plan-status 五·5.4）。网站 cf 阶段（882a850 + 三已审 backend 提交）typecheck/build 0、DOCFIX 10/10。最终树的 fresh 双审（含版本面与本记录）见后续登记。 fresh 双审、签名提交、PR/CI/merge、tag/release、安装 smoke、cf 推送/线上验收均待完成。

### 2026-10-08 13:12:40 UTC：FIX-RG-01 done / complete（v0.30.39 公开发布，正式进度 1/61）

PR #624 squash merge 9b8fc5c7057e6e317edd88f1518ba1bb25f2929f（2026-10-08T12:28:30Z by genedna (squash)）；签名 tag v0.30.39 → 9b8fc5c7057e6e317edd88f1518ba1bb25f2929f；release.yml run 37777413227 八任务 success；Release 发布 GitHub Release 页面待用户以 tag v0.30.39 创建（本会话 `gh release create` 被执行环境分类器拒绝）；四平台 artifact、CDN install 脚本、Homebrew formula、Ed25519 stable manifest 均已由 release run 发布；CDN DEFAULT_VERSION=v0.30.39 (install.sh) / v0.30.39 (install.ps1)；Homebrew 0.30.39 @ 6011b20d4b9f4c057c7840ffe2b389280384fe2b；darwin-arm64 76743008 bytes / d2c44e5a01fd3f353650dae455893eaf3e840101843e12227c77614fcfcadc6d；默认安装器隔离 smoke `libra 0.30.39` PASS。下一卡 NPR-19。详见 plan-20260904 同时间记录。

### 2026-10-08 14:51:55 UTC：NPR-19 / FIX-NPR-CONFIG-01 最终树事实更新（取代 12:49:33 记录中的 compose 树数字）

最终树 release/npr-19-v0.30.40 @ 9b8fc5c7（含 r2–r4 审查前滚：guard 错误分类器、import 回归测试、key-aware 拒绝）：fmt 0、strict clippy 0、具名 14/14、`command::config_test::` 178/178、docs/version 守卫 5/5、双 env full **9544/9544**（0 flaky/0 leaky、5 skipped）；文本修订后 docs/version/agent-docs 守卫 14/14、config_test 178/178 复跑。fresh 独立双审 r5：Codex PASS 0/0/0/0、Claude PASS P3=3（已采纳）。随后签名提交/push/PR；CI/merge/tag/release/安装/cf 仍待。

### 2026-10-09 02:51:00 UTC：NPR-19 自己的前滚 C/D（未 complete）

隔离方式：`libra worktree add -b release/npr-19-v0.30.42 /Volumes/Sea/libra-npr19-cd origin/main`。脏 main 892f37c 未提交、未 reset。不重发 v0.30.40；v0.30.41 不关闭 NPR-19。

版本面 0.30.42。fmt exit 0。clippy --all-targets --all-features -D warnings exit 0。compat_version_surface_sync 2/2。四具名门各 1 passed。full nextest 9545 passed / 0 failed（1243.572s；2 flaky 重试后过；2 leaky；5 skipped）。release binary `libra 0.30.42`。

网站 cf 源 commit acc2635 已推 origin/cf。DOCFIX vitest 10/10，typecheck 与 build exit 0。wrangler 未登录，未执行 deploy。线上 https://libra.tools/en/docs/commands/config 在 deploy 前为 HTTP 200，body 没有 “Keys are literal”。

PR、CI、merge、tag、release.yml、发布后安装 smoke、线上 cf 仍开放。NPR-19 保持 in-progress / locally-accepted。FAST-RG-01 仍被未采用的 FIX-RG-SCOPED-04 挡住。

### 2026-10-09 04:40:47 UTC：NPR-19 merge、tag、release（未 complete）

PR #627 squash merge `4a557df5163d7b3ebed50ca53859498774bf37d8`。Check run https://github.com/libra-tools/libra/actions/runs/37876625067 conclusion success。CodeQL PR https://github.com/libra-tools/libra/actions/runs/37876625016 conclusion success。main CodeQL https://github.com/libra-tools/libra/actions/runs/37882008811 conclusion success，headSha 同为该 merge。tag `v0.30.42` 指向该 merge。release.yml https://github.com/libra-tools/libra/actions/runs/37882123871 conclusion success，八个 job 全 success。

隔离安装 smoke 得到 `libra 0.30.42`，sha256 `5a440a9dab7da87a5b33a1de070346b4af642e8c0d8f355bf03c86c9b2b3c426`。用户目录里的 libra 仍是 0.30.39。

线上配置页仍未部署。`wrangler whoami` 未登录，所以没有执行 deploy。NPR-19 保持 in-progress / remote-pending，不是 done/complete。FAST-RG-01 未开始。FIX-RG-SCOPED-04 未采用。

### 2026-10-09 04:55:03 UTC：NPR-19 线上配置页已交付

https://libra.tools/en/docs/commands/config 返回 HTTP 200，`content-type: text/html; charset=utf-8`，页面 `lang=en`，正文含 “Keys are literal”。NPR-19 改为 done / complete。其 C/D 所承载的 FIX-NPR-CONFIG-01 与 FIX-RG-DOC-TEST-01 同改 done / complete。

下一发布点 FAST-RG-01 仍被 DEP-FAST-RG-IN 挡住：四 scoped fix 的 fresh 设计采用和 local deliverable 未就绪。未开始 FAST-RG-01，未采用 FIX-RG-SCOPED-04，未放开 DEP-OG-02。

### 2026-10-09 05:17:20 UTC：采纳 FIX-RG-SCOPED-04

人类采纳既有 FIX-RG-SCOPED-04，不新造跨进程协议。macOS 四具名门 4 passed。fmt 与 strict clippy exit 0。Windows `compat-scoped-input-windows` 已写入 base.yml，实跑 UNRUN。修订双审未执行。FIX-RG-SCOPED-02、FIX-RG-SCOPED-03 和 FAST-RG-01 未开始。

### 2026-10-09 05:19:40 UTC：FIX-RG-SCOPED-04 PR

2026-10-09 05:19:14 UTC：PR https://github.com/libra-tools/libra/pull/628 head 7e10632d490ad8fe1ab90e319defc442dabad355。base.yml run https://github.com/libra-tools/libra/actions/runs/37888011474 ；compat-scoped-input-windows 状态 IN_PROGRESS，job https://github.com/libra-tools/libra/actions/runs/37888011474/job/113682280952 。未称该 Windows job 通过。修订双审未执行。未开始 FIX-RG-SCOPED-02、FIX-RG-SCOPED-03、FAST-RG-01。

### 2026-10-09 05:53:41 UTC：SCOPED-04 Windows job failure

2026-10-09 05:53:41 UTC：当前 head 的 compat-scoped-input-windows 结论 failure。run https://github.com/libra-tools/libra/actions/runs/37888104494 job https://github.com/libra-tools/libra/actions/runs/37888104494/job/113682968003 。cargo test --locked --lib --list 未能编译 lib test，37 个错误，无 checkpoint_input.rs；含 opencode_export.rs 找不到 fake_exporter，以及多处仅 Windows 的 unused import。不是本卡 confined API 断言失败，未改那些无关文件，不称 Windows 通过。前一 run 37888011474 的同名 job 被 cancelled。修订双审未勾。未开始 02/03/FAST-RG-01。

### 2026-10-09 06:23:51 UTC：Windows lib test 编译修复

2026-10-09 06:23:51 UTC：只修 Windows 上 `cargo test --locked --lib --list` 在 `-D warnings` 下的 37 个编译错误。仅 Unix 使用的 import 收到 `#[cfg(unix)]`；Windows 提前返回的函数加 `cfg_attr` allow；`runner_controls_preserved` 改为 `unix` 且非 macOS，使 `#[cfg(unix)]` 的 `fake_exporter` 可见。未改 `checkpoint_input.rs` 行为。macOS `cargo clippy --all-targets --all-features -- -D warnings` exit 0；`cargo test --lib fix_rg_scoped_04_ -- --test-threads=1`：4 passed / 0 failed / 11.99s。该通过不是 Windows 通过。本提交推送后的 Check workflow 才是 `compat-scoped-input-windows` 证据，提交时结论未知。修订双审未勾。未开始 02/03/FAST-RG-01。

### 2026-10-09 06:30:39 UTC：编译修复的 Check run

2026-10-09 06:30:39 UTC：编译修复 head 6da86e8cbb64621b3ad70e57a81d099eb29da944 已在 origin/fix/rg-scoped-04。Check run https://github.com/libra-tools/libra/actions/runs/37893476162 。查看时 compat-rustfmt success；compat-scoped-input-windows job https://github.com/libra-tools/libra/actions/runs/37893476162/job/113699736070 为 in_progress，不称 Windows 通过。本会话 macOS 复跑 `cargo clippy --all-targets --all-features -- -D warnings` exit 0，`cargo test --lib fix_rg_scoped_04_ -- --test-threads=1` 为 4 passed / 0 failed / 11.76s。run 37888104494 的 compat-offline-core 与 compat-network-remotes 注释为 concurrency cancel-in-progress，不是测试断言失败，未扩 scope。修订双审未勾。未开始 02/03/FAST-RG-01。未标 FIX-RG-SCOPED-04 done/complete。

### 2026-10-09 07:08:04 UTC：Windows job 再次失败于 -D warnings

2026-10-09 07:08:04 UTC：run https://github.com/libra-tools/libra/actions/runs/37893476162 的 compat-scoped-input-windows job https://github.com/libra-tools/libra/actions/runs/37893476162/job/113699736070 结论 failure。setup-rust-toolchain 默认注入 `RUSTFLAGS=-D warnings`，`cargo test --locked --lib -- --list` 在四具名夹具之前失败：52 个错误，均为 Windows 上未调用的 Unix helper（dead_code / unused import），`throw 'Listing the scoped Windows fixtures failed'`。不是四夹具断言失败，不称 Windows 通过。本卡 job 将该 action 的 rustflags 置空，并在夹具步骤显式 `RUSTFLAGS=""`，使 `--list` 不再被这些既有 Unix helper 挡住。Linux clippy 步骤仍自带 `-D warnings`。未改 checkpoint_input.rs。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-09 07:46:11 UTC：Windows lib test 在 -D warnings 下无本仓警告

2026-10-09 07:46:11 UTC：撤回夹具步骤对 `RUSTFLAGS` 的清空，Windows job 重新使用 setup-rust-toolchain 默认的 `-D warnings`。Unix 专用、Windows lib test 不会调用的项加上 `#[cfg_attr(windows, allow(dead_code))]`，未使用的 re-export 加上 `#[cfg_attr(windows, allow(unused_imports))]`。未改这些函数在 Unix 上的行为，未改 checkpoint_input API。本机 `RUSTFLAGS="-D warnings" cargo test --locked --target x86_64-pc-windows-gnu --lib --no-run` exit 0，libra 自身无 warning。macOS `cargo clippy --all-targets --all-features -- -D warnings` exit 0。`cargo test --lib fix_rg_scoped_04_ -- --test-threads=1`：4 passed / 0 failed / 11.89s。该本机 GNU 通过不是 GitHub `windows-latest` 通过。依赖 `proc-macro-error2` 的 future-incompat 提示仍会打印，不是本仓 lint。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-09 08:26:44 UTC：Windows 四夹具通过，仍有链接警告

2026-10-09 08:26:44 UTC：compat-scoped-input-windows job https://github.com/libra-tools/libra/actions/runs/37901058709/job/113723675272 结论 success。run https://github.com/libra-tools/libra/actions/runs/37901058709 head 29a7c6cf08738ed18403698be8f485088f5eeb40，merge 27dab0616d6cfe8bd6dd1e05671e603dd0e979ce，base 426d2165cf9fd03fad629bf106515051c73b825a。步骤环境 `RUSTFLAGS=-D warnings`。四夹具各 `test result: ok. 1 passed; 0 failed; 0 ignored`（0.02s、0.15s、62.37s、0.22s）。同一次链接仍打印 `LNK4099`（openssl-sys 静态库缺少 `ossl_static.pdb`）以及 `libra (lib test) generated 1 warning`，另有 `proc-macro-error2` v2.0.1 的 future-incompat（E0365，crates.io 最新仍是 2.0.1，无法靠升级去掉）。本卡 job 在保留 `-D warnings` 的同时追加 `-C link-arg=/IGNORE:4099`，只压这一条 MSVC 链接警告。新 run 尚未出结果，不把本段写成下一次 Windows 通过。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-09 10:02:57 UTC：合并父提交与事件基线不一致

2026-10-09 10:02:57 UTC：run https://github.com/libra-tools/libra/actions/runs/37913022407 job https://github.com/libra-tools/libra/actions/runs/37913022407/job/113762449766 在夹具之前失败。检出的 merge b753f2fd7257219190115e204bc9e625db67c0e2 的父提交是 acc865ff650d28faa656d4af7c5601da097320a0 与 head 015537552cd6ddbe67b7f60c9f929ccae121d79a。事件里的 base 仍是 426d2165cf9fd03fad629bf106515051c73b825a，main 在事件快照之后前进了。脚本因此抛出 Pull merge parents do not match the actual PR base and head。不是四夹具失败，也不是 LNK4099。现改为：第二父提交必须等于 PR head；第一父提交是 GitHub 实际合并的基线，事件 base 落后时只记录、不失败。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。未标 done/complete。

### 2026-10-09 10:39:11 UTC：新基线校验后的 Windows 四夹具

2026-10-09 10:39:11 UTC：run https://github.com/libra-tools/libra/actions/runs/37915284178 job https://github.com/libra-tools/libra/actions/runs/37915284178/job/113770285993 结论 success。head 7a18553d8ebc4aa548c61043c6b3cc6cb4b40f15，merge a73f687063dc7c128f4d021574b2e6228cb9b42b。日志写明 Event base 426d2165cf9fd03fad629bf106515051c73b825a lagged the merge base acc865ff650d28faa656d4af7c5601da097320a0，然后继续测试。四夹具各 `1 passed / 0 failed / 0 ignored`（0.02s、0.18s、56.44s、0.21s）。LNK4099 与 `generated 1 warning` 均为 0 次。`proc-macro-error2` future-incompat 按决定未处理。同一次 Check 的 compat-offline-core 与 compat-clippy 查看时仍 in_progress，本记录不称整次 run 通过。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。未标 done/complete。证据先留在本地，避免取消仍在进行的 Check。

### 2026-10-09 11:25:15 UTC：compat-offline-core 失败于 live reservation deadline

2026-10-09 11:25:15 UTC：run https://github.com/libra-tools/libra/actions/runs/37915284178 已 completed，结论 failure。失败 job 是 compat-offline-core，https://github.com/libra-tools/libra/actions/runs/37915284178/job/113770286360 ，失败步骤 `Run tests (L1 + L2 + L3)`。nextest：9591 项中 9590 passed、1 failed、7 skipped，进程 exit 100。唯一失败是 `internal::ai::subagent_content::tests::subagent_content_live_reservation_deadline_returns_retryable_error_not_skip`，三次尝试都在当时的 `src/internal/ai/subagent_content.rs:6733` 断言 `rendered.contains("another writer")`。该文件相对 origin/main 为 0 行差异；30ms 预算来自 60c40df2（2026-10-03）。安静本机用原 30ms 预算 1 passed / 0.16s。并行负载下这 30ms 在第一次 reservation probe 内耗尽，错误是裸的 command deadline，还没有 “another writer”。修复：捕获预算改为既有 5 秒 `test_mutation_deadline()`（仍短于种下的 60 秒租约）；已经观察到 live writer 之后，下一次 probe 的 deadline 错误同样带上这条可重试文案。本机复跑该测试 1 passed / 5.14s。`cargo clippy --all-targets --all-features -- -D warnings` exit 0。未跑全量 nextest。`proc-macro-error2` 2.0.1 future-incompat 仍只打印，未处理。未标 FIX-RG-SCOPED-04 done/complete。修订双审未勾。未开始 FIX-RG-SCOPED-02、FIX-RG-SCOPED-03、FAST-RG-01。未把未跑的 Linux 门标成 PASS。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-09 12:56:58 UTC：PR #628 head bb05784 的 Check 全绿

2026-10-09 12:56:58 UTC：https://github.com/libra-tools/libra/actions/runs/37923903208 结论 success，head bb05784b4a5836f58ada94a3a5e9d6383f304f01。八个 job 全 success：compat-rustfmt、compat-clippy、compat-owner-liveness-macos、compat-redundancy、compat-offline-core、compat-network-remotes、opencode-export-linux、compat-scoped-input-windows。PR https://github.com/libra-tools/libra/pull/628 的检查面同时含 CodeQL / security-codeql-actions / security-codeql-rust / Analyze javascript-typescript / Analyze python，均为 SUCCESS；mergeable=MERGEABLE，mergeStateStatus=CLEAN。未 merge。未标 FIX-RG-SCOPED-04 done/complete。修订双审仍无本地命令、未闭合，故仍不开始 FIX-RG-SCOPED-02、FIX-RG-SCOPED-03、FAST-RG-01。`proc-macro-error2` future-incompat 未处理。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-09 14:06:49 UTC：PR #628 squash 合并，开始 FIX-RG-SCOPED-02

2026-10-09 14:06:49 UTC：按用户要求 squash 合并 https://github.com/libra-tools/libra/pull/628 ，merge commit f09f53a2cddbedc2c99cfcd83d8d4aedff336326，父提交 acc865ff650d28faa656d4af7c5601da097320a0。FIX-RG-SCOPED-04 仍不标 done/complete：修订双审没有本地命令。用户随后要求继续后面的任务，因此开始 FIX-RG-SCOPED-02 的 validating wrapper：`materialize_validated_checkpoint_input` 拒绝相对 storage/run，经 04 的 `cleanup_checkpoint_input` 清旧输入后再写普通 payload；investigate drive 与 review 的 scoped 物化改为调用它。同步函数 `materialize_checkpoint_input` 签名保留。`cargo test --lib fix_rg_scoped_02_ -- --test-threads=1`：2 passed / 0 failed。workflow/reader 目标门、文档和 actual cf 尚未做。未开始 FIX-RG-SCOPED-03 与 FAST-RG-01。`proc-macro-error2` future-incompat 未处理。

### 2026-10-09 15:09:24 UTC：SCOPED-02 三个现有测试目标

2026-10-09 15:09:24 UTC：在 fix/rg-scoped-02 上用 `cargo test --test <target> -- --test-threads=1`，未 source `.env.test` / `.env.live-test`，也不是 nextest。`agent_investigate_workflow_test` 16 passed / 0 failed / 18.56s。`agent_review_workflow_test` 12 passed / 0 failed / 15.02s。`agent_checkpoint_reader_test` 10 passed / 0 failed / 5.76s。含 `fix_rg_scoped_02_persisted_spec_resume` 与 `fix_rg_scoped_02_review_revalidation`。这不是卡上写的双 env nextest 命令，也不覆盖 catalog 不可用、review setup 期限/取消、文档和 actual cf。未标 FIX-RG-SCOPED-02 done/complete。未开始 03 与 FAST-RG-01。修订双审未闭合。`proc-macro-error2` future-incompat 未处理。

### 2026-10-09 15:20:08 UTC：SCOPED-02 设置期限与取消分开

2026-10-09 15:20:08 UTC：`materialize_validated_checkpoint_input` 把取消和期限分成 `ValidatedMaterializeError::Cancelled` / `Deadline`。期限或取消在 confined cleanup 写普通 payload 之前返回，`checkpoint-input` 不创建。review 的 scoped setup 期限仍是进入校验前的 `Instant::now() + request.reviewer_timeout`，到期走原 infra/Error，不启动 reviewer；取消（handle 或 run marker）走原 cancelled。investigate 在同一入口把取消放在期限之前，期限沿剩余 run budget 记为 timeout。`cargo test --lib fix_rg_scoped_02_ -- --test-threads=1`：3 passed / 0 failed。`cargo test --test agent_review_workflow_test fix_rg_scoped_02_review_revalidation -- --test-threads=1`：2 passed / 0 failed / 3.21s，含 `fix_rg_scoped_02_review_revalidation_setup_stop`。不是双 env nextest。catalog 复验仍没有可调用的 shared core：本分支没有 `src/internal/ai/checkpoint_reader.rs`，计划里的 `384351b17f0862779bbc32bc3ddd1f79bcbaae91` 不是本库对象。文档和 actual cf 未做。未标 complete。未推送：#629 head `25c0d5c` 的 Check 仍在跑。未开始 03 与 FAST-RG-01。`proc-macro-error2` future-incompat 未处理。

### 2026-10-09 15:33:45 UTC：SCOPED-02 catalog 传输失败不改暂停运行

2026-10-09 15:33:45 UTC：物化前用显式 `.libra/libra.db` 只读打开 catalog（`read_only`、`create_if_missing(false)`、连接数 1），查询预算是调用方期限与 200ms 的较小值，关闭连接后才清理和写普通 payload。暂停的 investigate continue 遇到打不开、查不了或关不上时返回原 `Store` 错误，`state.json` 与已有输入字节/权限不变，不启动 investigator。缺行仍是拒绝，不是这条传输失败。`cargo test --lib fix_rg_scoped_02_`：4 passed。`cargo test --test agent_investigate_workflow_test fix_rg_scoped_02_persisted_spec_resume`：2 passed / 5.32s。`cargo test --test agent_review_workflow_test fix_rg_scoped_02_review_revalidation`：2 passed / 2.82s。这不是角色闭包，也不是双 env nextest。文档和 actual cf 未做。未标 complete。未推送：offline-core 仍 pending。未开始 03 与 FAST-RG-01。`proc-macro-error2` future-incompat 未处理。

### 2026-10-09 15:41:29 UTC：SCOPED-02 保存 spec 必须等于 catalog 叶子

2026-10-09 15:41:29 UTC：catalog 连接关闭后、cleanup 之前，把保存 spec 的 path→OID 集合和 catalog `tree_oid` 下 `checkpoint/<id[:2]>/<id[2:]>` 的普通 blob 叶子比较。重复路径、缺叶、改 OID、非 blob 都拒绝，并且不创建 `checkpoint-input`。`cargo test --lib fix_rg_scoped_02_`：5 passed，含 `fix_rg_scoped_02_catalog_leaf_mismatch_skips_cleanup`。resume 具名门 2 passed / 2.65s。review 具名门 2 passed / 2.75s。这还不是 wrapped-v1 的 artifact 排除和完整预算闭包，也不是双 env nextest。文档和 actual cf 未做。未标 complete。未推送：offline-core 仍 pending。未开始 03 与 FAST-RG-01。`proc-macro-error2` future-incompat 未处理。

### 2026-10-10 01:42:30 UTC：SCOPED-02 排除 closed reasoning artifact

2026-10-10 01:42:30 UTC：普通叶子对照跳过精确路径 `reasoning/encrypted/<64 hex>`，不读其 blob。保存 spec 若点名该路径，在 cleanup 前拒绝。普通文件仍可物化，且输入目录不出现 `reasoning/`。`cargo test --lib fix_rg_scoped_02_ -- --test-threads=1`：6 passed，含 `fix_rg_scoped_02_ordinary_leaf_skips_reasoning_artifact`。`cargo clippy --all-targets --all-features -- -D warnings` exit 0。这不是完整 wrapped-v1 闭包，也不是双 env nextest。文档和 actual cf 未做。未标 complete。随本提交推送到 #629。未开始 03 与 FAST-RG-01。`proc-macro-error2` future-incompat 未处理。

### 2026-10-10 03:25:10 UTC：#629 squash 后在 main 补恢复说明

2026-10-10 03:25:10 UTC：https://github.com/libra-tools/libra/pull/629 squash 合并为 `c1a3c4e0162ecfda0f5f8f061102bab004e47a59`。`fix/rg-scoped-02` 与 `fix/rg-scoped-04` 的本地和远端分支已删除。EN/zh 用户文档、两个内部 agent 文档和 `COMPATIBILITY.md` 的 review/investigate 行写明：保存 spec 须匹配显式 catalog 的普通叶子，`reasoning/encrypted/<64 hex>` 不写入，暂停 continue 在 catalog 打不开时返回既有存储错误且不改 state。`docs/error-codes.md` 未新增稳定错误码。未标 FIX-RG-SCOPED-02 或 FIX-RG-SCOPED-04 done/complete。修订双审仍无本地命令。未开始 03 与 FAST-RG-01。actual cf 未改。Linux 门仍 UNRUN。

### 2026-10-10 03:41:30 UTC：SCOPED-02 具名 nextest

2026-10-10 03:41:30 UTC：在 `main` `af28385` 上 `source .env.test && source .env.live-test` 后跑 `cargo nextest run --no-fail-fast --retries 2`。`agent_investigate_workflow_test` 过滤 `fix_rg_scoped_02_persisted_spec_resume`：2 passed / 15 skipped / 3.679s。`agent_review_workflow_test` 过滤 `fix_rg_scoped_02_review_revalidation`：2 passed / 11 skipped / 2.412s。`--lib` 过滤 `fix_rg_scoped_02_`：6 passed / 3920 skipped / 0.044s。这不是三个完整 target，也不是全库 nextest。未标 FIX-RG-SCOPED-02 done/complete。未开始 03 与 FAST-RG-01。`proc-macro-error2` future-incompat 未处理。

### 2026-10-10 03:43:04 UTC：SCOPED-02 三个完整 target

2026-10-10 03:43:04 UTC：在 `main` `cd83b4a` 上同样 `source .env.test && source .env.live-test` 后跑完整 target，`cargo nextest run --no-fail-fast --retries 2`。`agent_investigate_workflow_test`：17 passed / 0 skipped / 7.510s。`agent_review_workflow_test`：13 passed / 0 skipped / 6.530s。`agent_checkpoint_reader_test`：10 passed / 0 skipped / 2.623s。未标 FIX-RG-SCOPED-02 done/complete。修订双审无本地命令。actual cf 未改。未开始 03 与 FAST-RG-01。Linux 门仍 UNRUN。`proc-macro-error2` future-incompat 未处理。

### 2026-10-10 05:09:46 UTC：SCOPED-02 全量 nextest 与 actual cf

2026-10-10 05:09:46 UTC：在 `main` `9b90a3e` 上 `umask 0022 && source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 进程 exit 0，耗时 1335s。保存的日志在汇总行之前被截断，最后保留的进度是 8225/9565，因此没有汇总计数。日志里两次首次失败都在重试通过：`hooks_apply_pending_repository_migrations_before_capture`（TRY 2 PASS）和 `machine_switch_dirty_repo_returns_only_json_error`（TRY 2 PASS）。actual cf：https://github.com/libra-tools/libra-backend/pull/1 squash 合并进 `cf`，merge `1f5baf73e22372fd46299496d101b237eb5924e1`，只改 `agent.en.md` 的暂停恢复段。未标 FIX-RG-SCOPED-02 done/complete：修订双审没有本地命令，FAST-RG-01 的 C/D 未做。Linux 门仍 UNRUN。未开始 03。

### 2026-10-10 05:27:30 UTC：SCOPED-02 全量 nextest 汇总已保留

2026-10-10 05:27:30 UTC：在 `main` `dda7a6d` 上把 `umask 0022 && source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 的输出写到持久日志。汇总行：`9565 tests run: 9565 passed (13 slow, 1 leaky), 5 skipped`，进程 `EXIT:0`，Summary 836.953s。日志中 `TRY 1 FAIL` 为 0。这补上 05:09:46 UTC 被截断、没有汇总计数的那次。未标 FIX-RG-SCOPED-02 done/complete：修订双审没有本地命令，FAST-RG-01 的 C/D 未做。Linux 门仍 UNRUN。未开始 03。

### 2026-10-10 06:05:14 UTC：SCOPED-02 有类型 blob 复验

2026-10-10 06:05:14 UTC：普通叶子在 `cleanup_checkpoint_input` 之前经 typed blob reader 核对 blob 类型、声明大小和内容哈希。非 blob、哈希不符、重复路径或越界在清掉旧输入前拒绝。缺失的 `reasoning/encrypted/<64 hex>` 松散对象不被打开，也不被重新写回。`source .env.test && source .env.live-test` 后 `cargo nextest run --no-fail-fast --retries 2`：investigate 过滤 `fix_rg_scoped_02_persisted_spec_resume` 为 2 passed / 15 skipped；review 过滤 `fix_rg_scoped_02_review_revalidation` 为 2 passed / 11 skipped；`--lib` 精确 `fix_rg_scoped_02_typed_materialization` 与 `fix_rg_scoped_02_second_materialization` 各 1 passed。`compat_agent_docs_contract` 10 passed。三个完整 target 合计 40 passed / 0 skipped。`--lib` 过滤 `fix_rg_scoped_02_` 为 6 passed / 3920 skipped。`cargo clippy --all-targets --all-features -- -D warnings` exit 0。同一工作树上 `umask 0022 && source .env.test && source .env.live-test && cargo nextest run --all --no-fail-fast --retries 2` 的持久日志汇总：`9566 tests run: 9566 passed (13 slow, 2 leaky), 5 skipped`，Summary 845.382s，`EXIT:0`，`TRY 1 FAIL` 为 0。两处 LEAK 仍是通过：`export_stage_releases_runner_lease_on_every_exit_mapping` 与 `scoped_head_decoder_rejects_a_zero_length_symbolic_name`。未标 FIX-RG-SCOPED-02 done/complete。人工红绿对照未跑。修订双审没有本地命令。Linux 门仍 UNRUN。未开始 03 与 FAST-RG-01。`proc-macro-error2` future-incompat 未处理。

### 2026-10-10 06:07:03 UTC：SCOPED-02 英文页句子已进 cf

2026-10-10 06:07:03 UTC：https://github.com/libra-tools/libra-backend/pull/2 squash 合并进 `cf`，merge `a0031852d83478a4971f327fb6bf5ef3b8eceab6`。只改 `agent.en.md`：已保存 id 若不是 blob，或字节与该 id 的内容哈希不一致，会在清掉上一次输入目录之前拒绝。当时 https://libra.tools/en/docs/commands/agent 还没有 “not a blob” 这句。未标 FIX-RG-SCOPED-02 done/complete。

### 2026-10-10 08:27:47 UTC：CX-00 因 Codex 版本漂移 blocked

2026-10-10 08:27:47 UTC：`/Users/eli/.local/bin/codex --version` 打印 `codex-cli 0.160.0`。DEP-CX-01 要求 `codex-cli 0.152.0`。CX-00 改为 blocked / 空。未创建 `plan-20260904-cx00-probe.md`，未填 ADR go/no-go。0.160.0 只作为 pin 候选登记，不替换 0.152.0。同一时刻 `/Users/eli/.opencode/bin/opencode --version` 打印 `opencode v2.0.25`，不是计划 pin 2.0.24，因此 FIX-OG-CP-01 的当前 pin 门仍 UNRUN。未开始 RG-01、FIX-RG-SCOPED-03 或 FAST-RG-01。

### 2026-10-10 08:33:48 UTC：Codex 基线 pin 改为 0.162.1

2026-10-10 08:33:48 UTC：用户决定本计划使用本机 Codex。`/Users/eli/.local/bin/codex --version` 打印 `codex-cli 0.162.1`。`plan-20260904` 的 DEP-CX-01、ADR-CX-01、CX-00 和后续真实会话门改为 0.162.1。CX-00 从 blocked 回到 pending / 空。隔离 home 探测、证据文件和 go/no-go 仍 UNRUN。2026-09-03 的 0.152 普查和 08:27:47 UTC 的 0.160.0 观察保留为历史。未开始 RG-01。

### 2026-10-10 08:29:54 UTC：SCOPED-02 线上页面已含非 blob 句子

2026-10-10 08:29:54 UTC：https://libra.tools/en/docs/commands/agent HTTP 200，`content-type: text/html; charset=utf-8`，cf-ray `a48442916ae8b865-SIN`。正文含 “A saved id that is not a blob, or whose bytes do not hash to that id, is refused before that directory is cleared.” 这补上 06:07:03 UTC 线上还没有该句的观察。未标 FIX-RG-SCOPED-02 done/complete。人工红绿对照、修订双审、Linux 与 FAST-RG-01 仍未做。

### 2026-10-09 09:25:29 UTC：IGNORE:4099 之后四夹具通过且无 LNK4099

2026-10-09 09:25:29 UTC：compat-scoped-input-windows job https://github.com/libra-tools/libra/actions/runs/37905150212/job/113737033866 结论 success（completed 2026-10-09T08:58:48Z）。run https://github.com/libra-tools/libra/actions/runs/37905150212 当时仍 in_progress。head 469a426e404314869d457dd2bbe83a3a66810dc5，merge b30c0acbc3bdeb4fdeceb1a637ba9937f6d3799f，base 426d2165cf9fd03fad629bf106515051c73b825a。步骤 `RUSTFLAGS=-D warnings -C link-arg=/IGNORE:4099`。四夹具各 `test result: ok. 1 passed; 0 failed; 0 ignored`：readonly_cleanup 0.02s，metadata_boundary 0.14s，preflight_budget 67.72s，deadline_cancel_owner 0.28s。日志中 `LNK4099` 出现 0 次，`generated 1 warning` 出现 0 次。未标 FIX-RG-SCOPED-04 done/complete。修订双审未勾。未开始 02/03/FAST-RG-01。Lifecycle / Acceptance 仍为 in-progress / 空。

### 2026-10-10：main 直接交付恢复，RG-01 开始迁入

用户授权 main 开发、逐卡签名 DCO Push、patch-only 阶段发布，并由执行者采用推荐决策；停用旧 620→621 跟进。main fast-forward 到 acee4ad6809c5a29d9f72c1c19400184c5ddda3e，原两个 dirty 计划 autostash 复回。exact 默认全局 installer exit0，Ed25519 manifest/sha256/size 检查通过，libra0.30.42。R10六RG与四scoped定义仅相关块恢复；当前CX/Codex0.162.1与计划05原改动保留。RG-01迁入type/source-shape契约、raw_value显式feature、自己的fixture/docs guard；本轮测试/审查/签名Push尚未执行，不计done/complete、不代替native来源。

### 2026-10-10：FIX-RG-01 actual successor 0.30.42 补充 C/D

最新用户授权执行者采用推荐方案。0.30.39 GitHub Release page 只读 GET 仍 HTTP404，旧被拒的元数据创建动作未重试，不能记为 PASS。采用实际已发布 successor 0.30.42 的补充交付覆盖此历史缺口，而非虚构 .39 页面或省略安全门：FIX-RG-01 自有 agent_import.rs 与 observed_agents/rpc.rs 在正式 .39 merge9b8fc5c7057e6e317edd88f1518ba1bb25f2929f 与 .42 merge4a557df5163d7b3ebed50ca53859498774bf37d8 的整文件逐字节相同，SHA256分别 ef40489d7271a85000f44c7f15567fd9b771b4f1f2fd24f30da8a3122f3fe605 / 77713bcd51d1cb54f146304ba2e366181e32c8e4856e7eef5df1dab67fcb5bf4。

.42 PR627 base run37876625067/offline-core job113646606435 实际两 descriptor cancellation 与 RPC EOF/wrong-version/wrong-id/result/timeout/malformed 均 PASS；该 CI Summary9587/9587、14slow、7skip。原 macOS full9545/9545、22slow、2flaky、2LEAK、5skip保持另一条证据，不能混为同一全量。.42真实非draft Release page、CodeQL PR37876625016/main37882008811、release37882123871八job（含四平台）、stable签名manifest/部署/install与网站记录已核实；.42 merge commit signature verified=true/valid，但 v0.30.42 是 lightweight tag，绝不称 signed annotated tag。

本轮 exact 全局命令 `curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh` exit0，Ed25519 manifest/libra-release-1、SHA256与size、official-install标记核对，实际 `libra --version` 为0.30.42。FIX-RG-01 保留 done/complete，以本 successor source-qualified 补充覆盖为当前 C/D 判据；.39 page缺失历史保留。NPR-19/FIX-NPR-CONFIG-01/FIX-RG-DOC-TEST-01实交付同时核实，固定队列正式完成4/61=6.56%，57张待完成。下一张 RG-01，当前仅迁入，未标 complete。

### 2026-10-10：USER-CLAUDE-SINGLE — 后续全部 Claude Code 单审

依据用户最新明确指令，本计划尚未关闭的设计、实现、修订、发布收口评审全部改为单一 Claude Code 评审；实际最终评审输入的 Claude Code 输出 PASS 即通过，不再新增 Codex 第二审、双审裁决或以未做双审阻塞任务。原有卡字段/工程约束中的“独立 Codex/Claude 双审”“Codex review”“parent verdict”等后续评审要求均按本具体授权覆盖；历史原稿、FAIL/PASS 和审计事实不改写。执行者仍核对实际 source 身份、修复已知缺陷并重跑相关测试；未运行/失败/synthetic 证据不得称验收通过，native 来源和实际 C/D 仍保留。

### 2026-10-10：RG-01 main 本地交付门闭合，Claude Code 单审 PASS

最终代码输入 manifest SHA983ffeedb386e3b318496b55d49d1aec7a1278af6f1c8b7bb0428e0d88cb46ef，19/19 root source SHA 与冻结评审输入一致。macOS：双env nextest reasoning/redaction/compat docs共61/61（3886其他过滤，不是full）；7/7 compile-fail doctest；fmt exit0；clippy all-targets/all-features -D warnings exit0（Cargo仍报告proc-macro-error2既有future-incompat，不称零警告）；release性能单门1/1、16MiB来源验证+原字节SHA256实测47.9115ms<=50ms。单一Claude Code执行exit0/is_error=false，44turn，REVIEW VERDICT: PASS（原输出SHA4f6a27bbe0e96a2e4023bf4026c3661e283e85bfb1ded98d50fc6ab82790b793），无P0/P1；本轮未做第二Codex评审或parent review。

按用户PASS即通过的最新授权，非阻断建议实名接受并保留：责任人为本会话Codex执行者（依用户自动决策授权），确认当前没有live调用，最小synthetic envelope不是任意真实Claude JSONL支持；真实record额外metadata必须在相应adapter卡先审schema再接线，不可偷偷放宽来源A。负例当前证明整体failclosed，但未单独隔离UniqueJsonKeys或钉各错误variant，后续RG04 shared contract在原own写集中补这一回归；裸compile-fail error-code、16MiB Display常量漂移与future preserve_order是测试维护建议。空JSON-string仅通过当前syntactic字段shape分类，不证明非空有效密文，RG02 sink仍须独立拒绝空artifact；perf仅约4%余量，实际记录保留，不调宽预算或用重试掩盖失败。

单写者/源代码及生产行为未在PASS后变动；仅本地验收勾选、评审事实和交付状态补记。RG-01 locally-accepted，签名main commit/Push随后取证；仍非done/complete。最终完整full、实际mainCI/CodeQL、FAST-RG-01 patch发布四平台/安装/网站C-D未关闭；不能把61 focused当计划61卡完成。下一卡RG-04；固定队列正式4/61=6.56%。每15分钟的620-621旧提醒已按用户请求删除，工具deleteStatus=deleted。

### 2026-10-10：RG-01 实际main Push；RG-04 开工

RG-01 signedDCO commit45c2a21a45f69e9b9d414776621636ba0f105203普通Push main exit0，GitHub remote ref同SHA。本地Libra merge --verify-signatures --ff-only HEAD exit0/Already up to date，against本仓库vault公钥真正验证签名，不改变HEAD/index/05原dirty。GitHub verification=false/reason=unknown_key，保留此区别，不称GitHub Verified，不上传或改写账号密钥。main CodeQuality run38042622482已success；CodeQL Advanced38042622632当时in_progress；base main/full仍未取得。

RG04在main迁入own生产差异，coverage前像逐字节等于RG01stage；reasoning前像精确核对并保留RG01 header、2.0.24与格式修订。测试索引保留当前memory/DM等其他行，只加own target；补真实SHA黄金值，不用digest长度冒称不变；补RG01单审建议的unselected sibling重复键独立负例/唯一键正对照、Display cap owner pin与最小synthetic envelope说明。无live/native归档声明，未改版本，未改RG02后续源码；当前门UNRUN，不标完成。旧620-621提醒已删除。

### 2026-10-10：RG-04 本次macOS本地交付门闭合，Claude Code单审PASS

基于main45c2a21的17个own路径，初审冻结23输入manifest SHAb3f9dd7666057ea84afb9964f19227c88f9848610adca7a5dde87dd4151e6cdd。macOS最终相关nextest92/92、3864过滤；8/8 compile-fail doctest（2其他过滤）；fmt exit0；clippy all-targets/all-features -D warnings exit0，仍保留proc-macro-error2既有future-incompat。完整full/CI/release/native本轮未声称PASS。

Claude Code单审exit0/is_error=false/31turn，REVIEW VERDICT: PASS，原输出SHA177eaa4b16bbcc240f8bf86c51d2a3389b53aaf913da985150b5549fbf65db07，无P0/P1。两个P2已关闭：INDEX只列本target的digest/状态表/warning检查，门/serde/canary/golden准确归lib；补跑agent_import_test49/49、零ignored/filtered，并四fixture jq验证exit0。同一Claude评审的仅INDEX/证据补充确认PASS（10turn，输出SHAc0d48d0804c31629f00f6e603458f5058d3ce8fe3f934aedceb952f9c0358e9d）；未新增第二Codex审或parent裁决。所有原生产/测试输入SHA仍与初审相同，INDEX与补充冻结输入逐字节相同；PASS后只补实际状态记录。

非阻断P3接受并跟踪于后续shared contract拥有者：fixture表部分手工期望、provider/source wire tag漂移pin、dead_code注释措辞、snapshot synthetic未redact调用、常量ProviderVisible构造的INVARIANT panic可简化；不藉此放宽native/typed/redaction契约。本卡现有真实tracing事件与非空捕获断言、serde泄漏负例及旧digest金值通过。contract.snap追加面作为本卡必要shared写集补记，未新增业务轴。此前开工记录的UNRUN为历史时点，本记录才是当前实际门。

RG04 locally-accepted，签名DCO提交并普通Push main随后取证；暂非done/complete，最终FAST-RG-01 full与实际main安全CI、patch四平台/manifest/安装/网站C-D仍开。下一卡RG-02；固定61卡正式4/61=6.56%。plan-20260905原dirty不混入。RG01 main CodeQL38042622632现已completed/success（精确45c2a21），不冒称未触发的base/full也绿。旧620-621每15分钟提醒已经实际删除，deleteStatus=deleted。

### 2026-10-10：RG04实际main Push；RG02开工

RG04 signedDCO main commit65b124165204227afb9d38cf44d66adadad78caa普通Push exit0，远端main ref同SHA；本地Libra merge --verify-signatures --ff-only HEAD实际加密签名验证exit0，GitHub仍unknown_key/verified=false，不称GitHub Verified。除plan05原dirty外工作树已清；未新bump/release/PR。

RG02只迁入rg04-stage→rg02-range-stage的owned差异，patch零fuzz，文档仅追加own段；保留main history Unix import、subagent upstream contention/Windows修复和RG04全部contract改动。patch工具产生的.orig逐项等于before备份后存证移出root，非丢弃用户数据。沿用manifest_artifacts.snap作为实际新增测试所需文件，按用户自动推荐方案授权补记原own写集；非新增task。sink补空ciphertext拒绝；metadata/locator/sourcebinding在manifest投影分配前核验，增加独立空payload/巨locator零对象写入负例；这些为原artifact可信边界收口，不放宽扇出、CPU或native来源。当前门UNRUN，后续本次macOS相关测试、两条三算法各九样本release CPU门、fmt/strict和Claude Code单审取实际结果。完整full/safety/C-D仍开；固定4/61=6.56%，Linux功能另机后验。

2026-10-10 RG02首轮focused176运行175PASS/1FAIL，唯一失败empty_artifact_set_manifest_is_byte_identical为漏迁入manifest_empty.snap的NotFound，三次均红，first-missing-fixture-gates保存完整原log/exit100；不得称PASS。现从原stage复制同SHA冻结空集金文件，未重生成或放宽比对；同时在原object format/reuse测试逐项覆盖SHA1/SHA256/BLAKE3并与独立canonical ObjectHash对照，普通default及两方向existing原wire均逐算法保持。重跑当前final candidate相关门，不复用红轮成功数当绿。

### 2026-10-10：RG02 本次macOS本地交付门闭合，Claude Code单审PASS

最终源码冻结27输入（22own+5context），manifest SHAb126c7187731fe2e9e28115a1be7c4ccef8f26750fcdca54ca2133ecbbc891f3，与root全部SHA核对相同。相关nextest176/176 exit0、8144其他excluded；保留1flaky（既有scoped_checkpoint_rechecks_lease_after_companion_before_index_upsert首尝initial fence未过、次尝通过）与1LEAK（既有compression-bomb typed read refusal），不称纯净绿；首轮漏manifest_empty.snap红记录保留。fmt exit0；clippy all-targets/all-features -D warnings exit0；default release CLI build exit0，pre-existing ingress::from_payload dead-code及proc-macro-error2 future note保留。

实际release CPU两门各1/1，三算法每路径各三样本共18/18逐样本PASS<=100000us，不平均，不重试不变源码抹红。direct CPU_us=44775,44288,44943 / 43645,43491,43960 / 47896,47260,47498；deadline parent+reaped child=96752,72963,72045 / 70953,71911,71265 / 74738,75412,74508。冷启动headroom3248us（约3.2%，保留风险），wall1554.962ms另记，不混为CPU。真实fresh release CLI path/SHA/size和全部log/source身份见cpu-receipt.json；artifact SHA/完整bytes/OID/intents/newly_written逐样本核对。

单一Claude Code初审exit0/is_error=false/36turn，REVIEW VERDICT: PASS，原输出SHA6a8a11f868b9a46b157c9c2f662de4a6cd3f20f4bcbb1c57f4e684543135ed8a，无P0/P1；补充同一审流程16turn再PASS（输出SHA2f2c3ac54dabdd8edb4573f693cba60da2e6a7f5d6dedd80f8eb8fc309ebfd2c），确认当前两个P2不适用：actual pinned registry git-internal0.10.2 CURRENT_HASH_KIND为thread_local RefCell、RAII恢复，sync/libtest和current-thread tokio无跨test共享，撤回串行锁建议，不新增serial/registry/config修改；production唯一issuer从<=16MiB source的RawValue子切片取原字节，不decode，私有字段/无Deserialize且大fixture ctor仅cfg(test)，32MiB是checkpoint总量不是单artifact承诺，故当前base64 frame可达。未来新issuer必须重新证明field cap与helper frame闭合。补跑cargo test --lib opaque -- --test-threads=8实际24/24 exit0、3936filtered，非full。

非阻断P3接受/跟踪：重复manifest预算序列化、固定不反射的聚合metadata错误文案、public CheckpointCommitParams新增field的client构造兼容说明；future issuer bound证明和冷cache CPU余量。debug/release过滤数差异保留实际数，不冒称相同全量。PASS后所有生产/测试/文档源码均保持原冻结SHA，仅本实际状态/门勾选补记；无第二Codex审或parent裁决。RG02 locally-accepted而非done/complete；签名DCO main commit/Push随后取证。下一卡RG06，原子提交/树可达性尚未接通，因此本卡不声称GC/mirror已可保留新artifact，也无任何live/native正例。最终FAST-RG-01 full/安全CI/CodeQL/patch四平台/安装/网站C-D仍开；固定正式4/61=6.56%。plan05原dirty保留，不混入。

### 2026-10-10：RG02实际main Push；RG06开工

RG02 signedDCO main3fcf83ac5bdcf90d5d1f9b575afa447ac1174cb9普通Push exit0，remote ref同SHA；vault实际签名验证exit0/Already up to date，GitHub unknown_key不称Verified。工作树只保留plan05原dirty。当前无bump/release/PR。

RG06基于此main迁入自身range diff；history29hunk中28个零fuzz匹配，单一timestamp选择hunk因RG02将metadata验证提前而改在原逻辑等价的所有验证后/第一写入前手工插入，保留empty payload/locator allocation边界及全部upstream drift。RG06独立docs guard，不把它藏入RG02函数；必要卡片纠错采用推荐方案：不生成已因opaque私有类型移除的agent_reasoning_artifact_test集成target，证据仍为原判据的history lib+实际command_test，修正AC8的实际aggregated CLI target全名。只改7个own路径，stage与原preimage逐项存证。本轮macOS相关功能/fmt/strict/Claude Code单审UNRUN；不会以synthetic型构造宣称native成功，正式4/61=6.56%，Linux功能deferred，完整full/安全CI/发布C-D仍开。

### 2026-10-10：RG06 main 本地验收与 Claude Code 单审

本次最终源相关 Nextest exit0：165/165 passed、2 LEAK、8176 filtered；两个LEAK原始标签保留，不称零泄漏。首轮E0061为RG02新增负例调用未同步artifact_params metadata参数，原红日志存证；前滚补测试参数后全相关门通过，生产验证先于timestamp/首次对象写入的次序未退化。nightly fmt与严格all-targets/all-features Clippy -D warnings均exit0。真实GC测试以synthetic树/独立CLI与fresh对象读回证明可达性，不代替live native来源。

Claude Code唯一源码评审exit0/is_error=false/53turns/PASS，stdout SHA256 76c11f0b93ac01d230069a6ccf47a8d1032eec1a4b03b07808586371b5a0b49e；没有P0/P1。三处tree布局注释补reasoning/encrypted/<sha256>条件条目；仅两文件comments/docs delta，其余冻结14输入SHA保持。文档14/14、fmt与strict再次exit0。同Claude窄复核20turns/PASS，stdout SHA e84c1c64fa60016cbe330c7aab041f48d0dd59374fe7f9cba2b570b65d6b7a8d，直接阅读三行diff/current source确认P2关闭；restricted cwd未读parent原审/日志，不能冒称其复核了这些运行receipt，实际门由原始日志独立存证。无第二评审/父级裁决。

非阻断P3跟踪：invalid_creation_time测试当前依赖object_index表尚未创建；空artifact重试原wall-clock时间行为按scope保留；旧六role narrative对应无artifact形状，未来文档可再补条件。PASS后生产/测试源码保持已审SHA，除上述已复核说明及本次真实计划勾选。RG06 locally-accepted；签名DCO ordinary Push main随后取证，无bump/PR/release。FAST-RG-01本地交付将为4/10，正式固定队列仍4/61=6.56%；最终full/安全CI/CodeQL/patch四平台/manifest/安装/网站C-D未关闭，Linux功能用户后验，native SourceA未放行。下一卡FIX-RG-SCOPED-01。旧620-621提醒已删除；plan05原dirty不纳入。

原始证据：/private/tmp/libra-main-resumption-20261010/rg06-main-r1/（first-compile-gates、gates.json、focused/fmt/strict logs、Claude原稿与窄复核、layout-gates.json）。

### 2026-10-10：FIX-RG-SCOPED-01 main 开工与必要写集纠错

RG06 signed-DCO main dafda7c1a5ba45e8749eac464a7b98bef57a7762普通Push exit0、remote同SHA；vault签名实际验证PASS，GitHub unknown_key不称Verified。阶段本地4/10，正式4/61=6.56%，无新版本/PR，旧620-621提醒已删除。

按USER-MAIN自动推荐决策，本卡core消费已关闭R4本地源而不复用其最终验收：reader SHA9dbe3eb75be7e7d97dd0b89348498043522246018691a2341ded1fcc664fd7b0迁入，command仅移入自身角色验证/读取与原R4门，当前raw/audit/rewind/pagination/summary保持。current main未含R4的RG03 public artifact字段/CLI，不提前引入；show/list为无object访问的白名单，R4 metadata projection门保留test-only并如实标注，不冒称已接实际show。既有skill单OID接口不能证明named checkpoint：必要写集补skill.rs传同row的id/tree_oid/metadata_oid，prod-files3→4/落点仍2/scope M；无新卡/target/schema/stablecode。

实际canonical reasoning/encrypted/前缀20字节，加64hex共84bytes，总路径上界8MiB+512*84=8431616；计划83/8431104为算术笔误。仅纠正当前谓词，ordinary 4096file/8MiB预算不变，历史原文与运行记录保留。本卡无直接linked Issue，继承授权计划及source限定依赖；所有本次门/Claude Code单审尚待，T1 full未关闭不complete，Linux功能另机后验。原plan05 dirty字节保持且不纳入。

### 2026-10-10：FIX-RG-SCOPED-01 main 本地 API 交付与 Claude Code 单审 PASS

基线 main dafda7c1a5ba45e8749eac464a7b98bef57a7762。最终15个own路径：四生产/两个既有测试/两源码serial登记/四文档/三plan-status；原plan05 dirty逐字节保留，index开工前为空。原R4共享reader提取后，metadata严格解析类型归shared core唯一owner，当前main未有RG03公开字段/参数，不提前引入。生产resume materializer仍未消费新core，AC1跨consumer接线保持未勾；该接线归02/RG03，不能把本API localD当整个SCOPED族或本卡formal complete。

macOS双env最终nextest82/82（含25个serial guards，8274过滤；既有page_cursor_round_trips 1次LEAK保留，不称零LEAK）；fmt0；all-target/all-feature strict Clippy -D warnings0，proc-macro-error2既有future-incompat仍报告。真实init参数/生产init writer建立SHA256/BLAKE3仓库后，将测试ambient HashKind设为SHA1，再从显式storage catalog证明格式、普通闭包和payload读；源码serial属性cwd+env与registry/census一致。Skill具名metadata ID/mode先验，真实alias门只读四wrapper/leaf树，artifact decodedbody=0；EN/zh累计16MiB childtree/选中metadata16MiB成本由现有docs契约门pin。完整blob验OID，truncated仅header-checked prefix且不给完整identity authority。

首编译E0425/exit101、初fixture EmptyTreeItems、初真实init重复create已存DB的81/82红、原冻结old-materializer真正运行RED、第一Claude FAIL及两轮PASS全部保留在 /private/tmp/libra-main-resumption-20261010/scoped01-main-r1。第二次narrow PASS原stdout SHA732028e8aa2e80b1a91cdcbcb4fce9bdd8be2562428c4ab62c8370e0ea57ab95；最终第三次narrow PASS（30turn/is_error=false/exit0）SHA22a3a7a72365a6644f38d816425f22091f27a0335cfe9473d312a673e9c357ae，最终P0/P1/P2=0、新P3=2；缺库retry文字、构造函数名和配置key字面重复等既有非阻断P3仍跟踪。只有Claude Code一名正式评审，无新Codex/parent review。

必要写集纠错：新增源码serial站点登记tests/SRC_SERIAL_REGISTRY.tsv与tests/SRC_SERIAL_CENSUS.tsv；现有generator实跑，.config/nextest.toml字节无变化，不纳入提交。剩余own AC6/7、VER6/6勾选反映已实现API，AC1跨卡接线及T1/T2 full、全部最终安全CI/四平台release/manifest/install/网站C-D仍UNRUN。Native SourceA不放行，Linux功能由用户后验。逐卡signed-DCO ordinary Push main随后存证，无PR/bump/release。阶段本地交付将为5/10=50%，正式固定队列仍4/61=6.56%，下一卡FIX-RG-SCOPED-04。

### 2026-10-10 12:54:44 UTC：SCOPED-04 NO-GO 遗留登记与执行恢复

USER-DEFER-SCOPED-04 已同步两计划与唯一遗留记录。04剩余4功能门、fmt/strict/Claude及Push尚待；下一卡FIX-RG-SCOPED-02仍需修订后04 API localD。正式4/61=6.56%，RG本地5/10=50%，无新发布。plan05无关dirty保持。SCOPED01 main aa6aae4 CodeQL run38051726726 completed/success。

### 2026-10-10 13:04:54 UTC：OpenCode核心交付优先策略

用户明确要求高复杂度同类问题延后，优先基础采集/字段审计与可信producer/verifier家族；已同步两计划。下一优先OG-00，不再自动执行scoped02。SCOPED04当前44/44 focused（2LEAK保留）、fmt/strict/lint exit0；原组合门ignored/UNRUN，full正在实际Nextest run840ba466-9300-492c-8213-9ee37f8632d6，尚不记PASS。Claude修订范围PASS/1P2/4P3；CI main并发取消P2已修，新窄评审待。

### 2026-10-10 13:26:35 UTC：SCOPED04 macOS localD，优先OG-00

SCOPED04修订范围AC5/6、VER4/4；相关44/44含2 leaky，fmt/strict及frozen actionlint exit0。T1双env Nextest实际9629/9629、1 leaky、9 skipped、exit0；原组合reparent门ignored/UNRUN，不含当前PASS。Claude R4真实PASS/0P0/0P1/0P2/3P3；文字对齐建议同期同步。Windows当前source CI、FAST-RG release/C-D/native来源门仍待。signed-DCO main Push后RG本地6/10=60%；正式固定61卡仍4/61=6.56%。下一OG-00当前2.0.26契约/fixture迁移，后续基础采集/字段审计和可信producer/verifier家族；并发目录移动DEFER不再当前实施，plan05原dirty保持。

### 2026-10-10 13:59:26 UTC：OG-00 当前localD验收

OG00 AC18/18、九审计门9/9、fmt/strict各exit0，Claude Code R2实际PASS/0P0/0P1/0P2/4P3，原FAIL及计数修正保留历史；当前43官方源逐blob/mode MATCH，78锚点，三合成审计产物恢复，无live/native/capture实现证明。按原no-code I＋状态路径signed-DCO ordinary Push main后进入OG-01；正式4/61=6.56%，RG本地6/10=60%，FAST-OG-01实际C/D和最终full仍待。plan05无关dirty保持。

### 2026-10-10：OG-01当前main开工/设计修订

当前main的OG01 AC/VER已重置为未运行，旧私有2.0.24通过仅历史。Claude设计R1 FAIL（4P1/5P2），已准备R2：旧模板显式空prompt契约前置补丁与严格parser同卡，保留完整OG02模板后续单写者；先取得fresh Claude设计PASS再改oracle/源码。本轮正式仍4/61=6.56%。

- **DEFER-OG-DOC-01（外围文档漂移）：** 网站agent.en.md已有未提交工作提前描述2.0.24导出/可用macOS Seatbelt路径；本卡只同步event/parser段并保留其它原始字节，当前不将它记作macOS/native PASS。由既有OG03/OG15在各自功能实际验收后同步清除；不增加正式卡计数、不放宽发布/来源/最终全量。

### 2026-10-10：OG-01当前源码focused通过，full/实现review进行

当前2.0.26 registry/parser/旧模板显式空prompt源已实际实现，focused89/89、producer实际Node执行marker1/1、fmt/strict与网站typecheck/build通过；全量运行中，Claude实现review随后同冻结源审查。正式仍4/61=6.56%，本卡尚非locally-accepted/complete，source commit/push尚待实际门完成。

### 2026-10-10：OG-01 Claude实现单审PASS

R1实际PASS（0P0/0P1/1P2/5P3），非阻断P2为已批准ContractA下旧模板陈旧提示缺失，FOLLOWUP-OG02-STALE-01由下一OG02安装升级验收接续；当前不改冻结源。full默认9645 cases正在运行，既有deadline首轮失败/TRY2通过和git-client LEAK标记如实保留；不是最终全量PASS。正式仍4/61=6.56%，signed-DCO源码交付待真实full完成。

### 2026-10-10 15:05 UTC：OG-01当前2.0.26 local验收通过

OG-01 AC8/8、VER15/15：focused89/89、真实Node模板marker1/1、fmt/strict/site0、full9645/9645（13slow/1flaky/3leaky/9skip，run34a9e98a）、Claude单审PASS（0P0/0P1/1非阻断P2/5P3）。全部15实现/doc hashes与测试/peer冻结源一致；源提交交付中，完成后下一优先OG02，完整新插件/Node-Bun/native证据仍后验。FAST-OG01 C/D待执行，正式仍4/61=6.56%。

- **DEFER-OG-TEST-01（外围验收检测标记）：** 本轮未修改git-client/storage_r2_test cases出现3LEAK标记，既有deadline case first-fail/TRY2-PASS；full exit0并按受控重试通过。标记原样保存于acceptance-receipt/full日志，根因未定，不称实际泄漏或已修复，后续独立诊断，不改本计划基本采集/来源/实际full标准。

### 2026-10-10：OG-01 signed-DCO main源码交付完成，进入OG02

实际main source 2bcea258e4ac8e559d172fc53ccd528be81a7545，parent befaa45，17 owned路径PGP header/DCO一次、committed blobs17/17 MATCH、ordinary push exit0/远端main一致。focused89/89、producerNode1/1、fmt/strict/site0、full9645/9645与Claude single PASS覆盖同冻结实现；full1flaky/3leaky/9skip如实保留。基础组13成员local交付2/13=15.38%，正式4/61=6.56%（C/D仍FAST-OG01）。下一OG02双运行时插件，接续FOLLOWUP-OG02-STALE01；无真实native/Bun/Linux证明提前放行。plan05原bytes保留、backend cf只本地event段增量保留未deploy；本轮无版本发布。

### 2026-10-10：OG-02 current-main 开工

基线main28aef04/GitHub ref MATCH、OG-01 signed Deliverables2bcea258已Push；当前OG-02 in-progress/空，原2.0.24私有AC/VER不复用。单writer保持plan05原hash；DEP-OG-01/03/08按当前clean+MG complete ready。Node/Bun新API插件、prompt配对、deadline/退出清理、完整模板陈旧status识别和安装路径门重验中；Claude单审/fmt/strict/本卡Push待。DEFER-OG-INSTALL-01记checks后reparent/末次快照后叶替换的对抗性并发归属证明，按用户策略移至本计划外，未证明不记PASS。正式4/61、基础local2/13不变；下张OG-03须OG-02最终local Deliverables。

### 2026-10-10：OG-02 R1 PASS、基础补强重验中

Claude单审R1字面PASS（0P0/0P1/2P2/7P3），两个基础P2前滚为滚动live ID窗口+稳定message_id和真实producer→Libra ingress门；不无限历史去重，catalog128receipt上限不改。25 VER（Linux1 UNRUN），新增Bun真实exit flush，简单P3同轴前滚；DEFER-OG-PORTABILITY-01仅非四平台其它目标。新harness PATH/括号/terminal revision断言实际失败留存并修，不改catalog/frozenoracle。补强后源码待Claude R2/实际focused/fmt/strict，OG02未localAccepted、未Push；正式4/61、基础local2/13维持。


### 2026-10-11：OG-02 R2 PASS，容量窗口前滚

Claude R2实际PASS/0P0/0P1/1P2/4P3；旧138/138与Bun sync/async timeout实际证明保留。64进程级跟踪改滚动收尾/新会话继续，early text滚动与结束后ID清理补门；27VER（Linux1UNRUN），28AC/唯一settings逻辑写集不变。当前新源验收与R3未闭合，不继承旧PASS。temp-link hygiene按DEFER-OG-TEMP-01延期；基础local2/13与正式4/61保持，下一仍OG03。


### 2026-10-11 00:16 CST：OG-02当前门通过、source Push准备

AC28/28、26/27VER实际适用通过（Linux1UNRUN），focused140/140/零重试零LEAK，Node/Bun真实运行、finalBun sync/async挂起PID终止、synthetic260prompt实际installed producer→真实ingress/catalog/stopped，fmt/严格Clippy/site均exit0。Claude R3字面PASS/0P0/0P1/0P2/4P3，原稿SHA859de99c297ff1f58cbc830c4b98f96e2d1dcfd00a8ccb52374457a6fa0bcadc。容量LRU/早到文本滚动已修；DEFER-OG-EVICTION01 pending/model恢复等非阻断残余及双转发预算具名记录，核心source/隐私/native准入不降。无全量/liveOpenCode/native/Linux证明误记。13owned路径待signedDCO/mainPush；plan05原bytes保留，backend仅owned插件段本地未部署，0.30.42无发布。正式4/61，签名交付前基础local2/13，下一OG03。


### 2026-10-11：OG-02 signed-DCO源码与main普通Push完成，下一OG03

实际source commit80fb17a86c886c47ecc8a9445702b288d21526bc，parent28aef04；13owned路径全部committed blob与最终publication SHA匹配，PGP签名header与DCO一次已核验，普通libra push origin main exit0、GitHub实际main ref一致（2026-10-10 16:17 UTC后核验）。plan05树未提交该文件本地delta，工作区原SHAa705585dcb951a8d1452f87a607eefae1313d660b4c53c8d86ea75a024915b32保持；其余owned工作区clean。AC28/28、macOS适用VER26/26（Linux VER18 UNRUN）、focused140/140、finalfmt/strict/site0、ClaudeR3 PASS均覆盖同最终实现/产品doc source，后置只回填事实plan/status不改实现/测试。Node/Bun为真实解释器的controlled exporter证明；260synthetic prompts另走真实Libra ingress；均不冒称真实OpenCode/native。FAST-OG01 stage全量/网站实际交付/必要CI与四平台发布尚未执行，正式4/61=6.56%保持；基础13成员本地交付3/13=23.08%。下一张优先OG03当前2.0.26 shape-exact extraction，不去scoped02/03复杂链；无bump/release，0.30.42保持。


### 2026-10-11：OG-03新pin/current-main开工核对

OG02 source80fb17a/receipt7b6ef27已Push且当前main一致。OG03旧private14AC/6VER归历史，本次in-progress/空；当前generic extractor缺现行nested字段。固定2.0.26源码与4额外immutable blobs核验，提出OpenCode-private投影/有界collection/overflow partial/文件位置规范设计，待Claude。真实Mac内容非零仍由OG15最后实际证明，不以synthetic替代。无独立Issue，不新增remoteIssue；plan05原bytes保留、单writer/main逐卡Push/patch策略保持。基础local3/13、正式4/61，下卡仍待本卡完成的OG09。

### 2026-10-11：OG-03遗留登记与最后候选

DEFER-OG-COMPAT-01：top-level parts单message alias与typed JSONL未支持，只固定partial，不造完整零投影；本plan-status同名条目为规范记录，计划progress只引用。DEFER-OG-DOC-01网站旧版/未来seatbelt段等待OG15/阶段owner，OG03只替换自己owned投影段；DEFER-OG-TEST-01中止暂行full的LEAK不冒称已修。用户要求类似困难外围问题延后，继续OpenCode基础采集/字段审计及可信producer/verifier；核心native allowlist/缺location/真实失败cat回归均已前滚，最后gate仍待，进度3/13与4/61保持。R2实际Claude FAIL的唯一P1为cat未真正落地，现按格式化分支修正，具名单元实际1/1 PASS；需要R3实际PASS以及最终fmt/strict/full，签名Push未做。两个并行暂行full日志混写且中止，不作验收证据，最终唯一序列化日志另记full-final。

### 2026-10-11：OG-03 final全量暴露neutral守卫，基础前滚

全量9687/9688通过，唯一架构neutral分支计数失败；原守卫19条不放宽，文件衔接改成通用summary fallback，当前provider外观以冻结oracle和全量重验。2LEAK继续DEFER-OG-TEST01。本卡未signed/Push，进度仍3/13、4/61；Claude R4、fmt/strict/focused/full最后源待。DEFER-OG-COMPAT01与DEFER-OG-EXTRACT01以本status登记为规范，plan progress只引用；R3的marker负例/宽松parity及两个弱断言为非阻断外围遗留。

### 2026-10-11：OG-03最终验收通过、待signed source Push

15AC/7VER、116相关与完整架构守卫/3冻结oracle、fmt/严格Clippy、双envfull9688/9688（0failed/13slow/3leaky/9skip）和实际ClaudeR4字面PASS关闭，同最后实现源。review原稿SHA0660bda3c96258199844a7eb8ad5322622c8e2a4369a2a8fe54ba6382d5cfbaa，fullSHA98801fccc0a8d5930012e021f623581c638b7881c172a30693d4ebd8a5833e96。13owned待精确signedDCO/mainPush，plan05原bytes保留；版本0.30.42未发布。基础Push后4/13=30.77%、formal4/61=6.56%（阶段C/D待）。下一OG09当前2.0.26 native/classic normalizer；field audit及可信producer/verifier保持优先。SourceA不降低；synthetic不是liveMac/native/Linux证明，OG15/用户后验保留。

DEFER-OG-EXTRACT-01：R3 marker否定例/宽松引号及空白parity、缺location断言强化、unknown-type负例为非阻断外围测试/精确parity遗留，不在当前基本采集链展开；R4的定义/规范指针澄清已仅以事实文档修正，不增实现。DEFER-OG-COMPAT-01：不支持top-level parts alias与typed JSONL的partial降级，以本条为唯一规范指针；不冒称完整空结果。DEFER-OG-TEST-01：本轮3个既有测试LEAK提示不称修复，finalfull仍真实exit0。

### 2026-10-11：OG-03签名源码与main普通Push完成

source9ad5fa486e47cc64835d27bc4c6b46bce18d6192、parent7b6ef27，14blob（13owned变更+bridge原样）postimage匹配，PGP header与DCO一次；普通Push exit0/实际GitHubmain同ref。原plan05未进本次tree delta，工作区SHAa705585dcb951a8d1452f87a607eefae1313d660b4c53c8d86ea75a024915b32保持，其他owned clean。最后实现仍为116聚焦、9688全量exit0、fmt/严格Clippy/site0与ClaudeR4 PASS证明源。3LEAK/9skip如实保留；synthetic不冒称liveMac/native/Linux。0.30.42无发布，FASTOG01阶段C/D待；基础4/13=30.77%、formal4/61=6.56%，下一OG09 current2.0.26正常化part/完整性。

### 2026-10-11：OG-09 current-main重新开工

OG03 signed source9ad5fa486与receipt0f4a6bdb已普通Push main；当前coverage只支持classic info/parts，旧native/三具名测试不存在，历史AC16/VER3不继承。OG09重置in-progress/空，当前pin2.0.26，Claude单审/main逐卡/阶段patch规则覆盖旧私有双审文案。DEP01/03/08逐文件无并发writer，可消费OG02/03最终local交付；无独立linked Issue，不新增远端Issue。仅OpenCode私有分支不因文件多provider触发T1，若实际共同SSOT变化则full；阶段及最终全量保留。正式4/61=6.56%，基础4/13=30.77%；Linux/native实证仍UNRUN，plan05保护。

### 2026-10-11：OG-09 Claude单审与本地验收通过

Claude R1实际PASS（0P0/0P1/1P2/5P3，31turn，SHA1e37d7eb236f565bf3727d9a50fee2bae1f5523e6a38126a6c43ed41a2e5f188）。125/125聚焦（32architecture+3原冻结oracle；零retry），fmt/严格all-target/all-feature Clippy、site typecheck/build均exit0；当前native human key/time、idle结束与保守Incomplete、tool text/error.message与附件不复制、classic九已知parts no projection完成。SourceA不降，无real export/model/native/Linux证明；当前树未跑full，原OG03全量不能继承为本卡full，阶段及最终full必须。10owned实际冻结稿，plan05原bytes保持；网站仅fresh preimage owned插入，无commit/deploy。signedDCO main Push待，基础4/13、正式4/61，下一OG11。

- **DEFER-OG-NORMALIZER-01（本计划外外围口径/兼容补强）：** R1非阻断P2为shell command/output及synthetic/system模型可见text目前按已识别metadata排除，Complete只描述ordinary allowlist表示，并非完整模型上下文；是否扩展投影/改为Incomplete后续另行评估，不移入OG11、不冒称已证明全上下文。P3为classic reasoning现保守Incomplete的既有claim迁移说明、未来native ID漂移兼容、error缺type/agents错型等额外负例与更强具名turn断言、assistant级error无content仅user记录的口径。当前实际PASS可放行；native 来源要求、未支持附件/未知reasoning的Incomplete和最终full不变。R1 preflight vs frozen plan hash差异仅证据生成时序，修正root candidate receipt并保留旧版，不改已审源码。

### 2026-10-11：OG-09签名交付、开始OG-11准备

source `2e7a8e22d4a269c04f09116fc71a666fa81c3d05` parent0f4a6bdb，10owned postimage blobs MATCH、PGP+DCO一次、普通Push exit0与实际GitHub main MATCH。125聚焦/严格Clippy/fmt/site exit0，实际Claude R1 PASS保持；DEFER-OG-NORMALIZER01不在本计划扩展，无true export/native/Linux/full当前候选冒称。plan05原hasha705585d保留且本tree delta排除，网站cf原4dirty保留。0.30.42无发布，阶段与最终full/C-D仍待。基础5/13=38.46%，正式4/61=6.56%；下一OG11自持注入前缀表和normalization/extraction共用过滤。

### 2026-10-11：OG-11 current-main开工与必要写集修订

main e3d37842、OG09 signed source2e7a8e22已交付；原过滤表/helper/具名测试缺，历史5AC本地接受不继承。自动采用推荐必要修订：接OpenCode内部coverage与prompt extraction，移除失效runtime锚点/写集（该文件原样），prod/landing2、AC6/VER1（增加malformed标签fail-visible与嵌套不泄漏，同轴≤8）。GC02过滤SSOT与generic OpenCode分支命中T1，最终源码实际双env Nextest全量后Push；Claude单审PASS/fmt/strict，阶段最终全量保留。基础5/13、正式4/61；plan05和网站原dirty保留，native硬门/真实LinuxUNRUN不变。

### 2026-10-11：OG-11最终全量与Claude单审通过

AC6/6、VER1/1，127/127聚焦（零retry）、9699/9699双env默认Nextest（13slow/1flaky/2leaky/9skip）、fmt/严格all-target/all-feature Clippy/site0。full842.257s、18:26:30.171003–18:42:53.166084 UTC、SHAf525ab0bb70c6f67144889af5f4082908676e30353ab461abbbc7c9cee2bfdf8。实际Claude R1 PASS/0P0/0P1/0P2/4P3，40turn，原稿SHAb108af359d762c5e201a95f21626b42aa0b88661d311ff0dd257b8dbc6cdcdcf。11文件冻结全量与审稿原样；prod仅extract/coverage，旧runtime原样，其它provider/canonical/oracles原样。2LEAK及旧deadline用例TRY1FAIL/TRY2PASS沿DEFER-OG-TEST01未称修复。无real OpenCode/native/Linux证明，0.30.42无发布、网站cf仅owned段本地。plan05原bytes保持；signedPush待、基础5/13与formal4/61保持，下一OG14。

- **DEFER-OG-FILTER-01（本计划外非阻断补强）：** R1的4P3为额外其它provider/assistant literal标签否定测试、既有covered文本遇此新过滤时coverage-v1 conflict的迁移说明、已移除block内redaction sentinel不再poison的先后口径、deadline string cap先于过滤而超限partial。当前精确字面/嵌套/malformed/两通路与独立usage/time已实际通过，未来另行补强；既有claim差异仍走原conflict，未称已迁移/免冲突。PASS可通过与外围延期按用户授权，不扩大此卡，不把字面标签当native来源证明。

### 2026-10-11：OG-11签名main交付完成

source `0d6f35c94894570b458c16eb0c6189df210a43a3`、parent e3d37842，11owned blobs postimage MATCH、PGP+DCO一次、普通Push exit0/GitHub main实ref MATCH。127聚焦、9699双env全量（13slow/1flaky/2leaky/9skip）与strict/fmt/site0及实际Claude R1 PASS保持有效，prod/source审稿与finalfull指纹一致。plan05原bytes未进delta，网站cf原4dirty保留且仅owned本地段；0.30.42无发布。基础6/13=46.15%，正式4/61=6.56%，阶段C/D待；下一OG14。NoGo/同难度外围按用户延期，基础与可信producer/verifier优先、native硬门不降、Linux实机UNRUN。

### 2026-10-11：OG-14 current-main开工

main0e0b149a、OG11 signed source0d6f35c9已交付；旧AC3/VER4不继承，三intelligence新门和实际typed checkpoint门须重建。现有capture普通/worker partial与固定warning保护已有，不重写生产pipeline；potential空bytes/valid非法形状零非partial缺口先负例真红确认。自动推荐必要写集调整：extract失败处置1prod，两个src tests-only新增独立case，冻结原oracle/helper不改；第四VER改真实lib typed persistence case，不借bwrap skip当mac真机证据；AC3/VER4不增计数。generic OpenCode failure branch按T1 full，Claude单审、fmt/strict、signed Push；plan05与网站原dirty保护。基础6/13、正式4/61不变，Linux/native真实正例UNRUN。

### 2026-10-11：OG-14失败形状红复现与持久化聚焦通过

当前0e0b149a main后继OG14 sole writer；3具名集成负例先1pass/2fail，空bytes及无role静默零结果已按OpenCode-only形状门修正，良构独立model/usage继续投影。ordinary/worker projection及typed pipeline真实SQLite/对象存储新持久化case通过：partial仍checkpoint、坏JSON不替换原metadata/tree。文档前focused132/132真实绿，三冻结oracle与所有32架构门保持。新增测试初轮缺serde_json::Value类型限定导致compile101已修复，失败不算通过。文档增量/网站fresh preimage保持其他dirty，最终focused/strict/fmt/site、Claude单审与当前源码T1 full待；synthetic无真实OpenCode/mac内容/native来源/Linux证明。正式4/61=6.56%，基础6/13=46.15%，完成交付OG14后下一OG15；plan05原bytes保持。

### 2026-10-11：OG-14最终全量与Claude单审PASS，待签名main交付

AC3/VER4当前具名门重建；final focused133/133、fmt/strict/site各0；双envdefault Nextest full9705/9705、830.938s（13slow/1flaky/9skipped/0leaky/0最终failed），SHA d387f5581c172b2be38b323ca894926e40884656adcfb23ac83eb89500d87a93。Claude R1实际PASS，0P0/0P1/1P2/4P3，35turn，rawSHA f8554015ec994d57e4170b4214fbc73c86d11c4493ef1fea66dc7083d3b1b955；非阻断空messages/info错型与新增count回归按本计划DEFER-OG-CONTAINMENT-01计划外登记，未实现/不称全部形状闭合。legacy角色partial文档已披露，serial疑虑由full实际src/registry全门PASS闭合，style不作门。DEFER-OG-TEST01追加本轮既有mirror fetch TRY1 I/O worker error→TRY2 PASS，未修复；旧OG11flaky/leak仅历史。三frozen oracle/helper和32架构门未改，source/full已审12SHA MATCH；postreview只写plan/status事实。synthetic exporter+真实SQLite/objects仍不是实际OpenCode/mac内容/native或Linux证据。0.30.42不变、网站未提交部署、阶段C/D与最终full保留。plan05原bytes排除；formal4/61=6.56%，基础6/13=46.15%，signed Push后7/13=53.85%，下一OG15。

### 2026-10-11：OG-14 signed source普通Push main完成

source `c578b69a2724f5dedc346cae63cb938c5170635b`已ordinary Push main，实际GitHub main同ref；PGP签名+DCO一次、12owned blobs MATCH，plan05原bytes未入delta。当前133focused/9705full/strict/fmt/site0与Claude R1 PASS有效，DEFER-OG-CONTAINMENT01及TEST01未修复如实保留；formalC/D阶段待、0.30.42未bump/release。基础7/13=53.85%，formal4/61=6.56%；下一OG15 current2.0.26 PREP核对，旧mac内容PASS不沿用。

### 2026-10-11：OG-15当前main PREP核对并重置历史验收

实际66816ada/source c578b69a已交付OG14；current2.0.26 CLI help确认serve stdio/export server/standalone形状，仅help非runtime。main两个Mac入口仍spawn前拒绝、harness禁止Mac内容分支，old2.0.24具名PASS不适用，OG15 AC/VER复位，当前Acceptance空。PREP source snapshot20paths，root仅plan05原dirty；先补native location identity实际正负门，再Mac隔离/cancel界；旧候选Linux缺main pidfd/close-range，不能whole-file覆盖。保留当前Linux源和必要安全CI，不执行Linux功能/operator。当前default3s/lease30000ms不冒称最终30s/45000ms；FIX argv/OG07各自own。Claude单审/签名main/patch阶段规则覆盖旧文字，来源A及真实内容安全前提不变；正式4/61=6.56%、基础7/13=53.85%。

2026-10-11 OG15 current2.0.26 PREP WORKING：baseline main66816ada；native identity8/8与初轮Mac Seatbelt kernel6/6真实通过，新增双端fork/输出/readiness负例及final focused/fmt/strict/full/Claude审查/Push尚待。Linux runner/pidfd/close-range三个区域逐字节保留、安全CI未改，Linux功能UNRUN/user后验。FIX argv与OG15-FINAL真实内容/source一致性/stopped/doctor仍未通过；default3s/lease30000ms未提前称30s/45s。正式4/61=6.56%、基础local7/13=53.85%，未冒称OG15 member done。cf fresh owned Mac平台段同步，原段之外bytes保留、未外部发布。

2026-10-11 OG15-PREP current2.0.26 phase locally-accepted：actual Claude单审PASS(0P0/1、1P2、9P3，raw423c27b35886b6891ce439db8512f20f688cdf0c2315ee0bf6cc0b5ccf0a034e)、focused115/115/0retry、strict/fmt/site0；final双env Nextest9714/9714、14slow/0flaky/3leaky/9skip、0finalfail、exit0、832.923s，log b78b6d03c10bfbf36f0c9c1d91350cabb7e438be9b6f3c9d0b113c10dc6a9cd0。nonblocking P2/P3→DEFER-OG-CONTAINMENT-01、3具名LEAK→DEFER-OG-TEST01均遗留未修，原review/失败完整保留。whole OG15 in-progress空，real Mac内容/stopped/doctor UNRUN、Linux用户后验UNRUN；准备16owned signed main source Push，正式4/61=6.56%、基础local7/13=53.85%不变，下一FIX-OG-CP01。版本0.30.42/no release/installer，protectedplan05 exact excluded、cf仅owned平台段未外部发布。

2026-10-11 OG15-PREP source实际signed-DCO ordinary Push main 6c65d7eba057ab122ac6637ee840f9d56778cf40，parent66816ada4b70ddca9b02938c1db30bc0217ff596，16owned路径HEAD blob逐一SHA/mode与reviewed/final source MATCH，actual GitHub main同SHA。PGP signature与1DCO真实验证，plan05 protected原dirty未stage，cf平台段仍本地未外部发布。full9714/9714(14slow/3leaky/9skip)、Claude PASS原稿与非阻断遗留保持；本receipt仅两份plan/status事实更新，不变更已审实现，不bump/release/installer。下一FIX-OG-CP01，whole OG15真实FINAL仍待，正式4/61(6.56%)、基础local7/13(53.85%)不变。

2026-10-11 FIX-OG-CP01 current2.0.26 WORKING：baseline signed main ded6e2f0，OG15-PREP actual delivered。仅adapter standalone argv、新旧session fixture$2→$3与Mac可运行no-shell/无sanitize unit；推荐必要写集修订已按用户授权落实。real Mac两次smoke待OG15-FINAL后闭合原AC3/VER2，whole FIX不提前称locally-accepted。source changed后旧full不作新证据，逐卡focused/fmt/strict/Claude与最终FAST-OG01 full保留；Linux功能UNRUN/no model calls。正式4/61=6.56%、基础local7/13=53.85%不变。

2026-10-11 FIX-OG-CP01 current argv阶段：actual main ded6e2f0开工，4owned root paths；generic trailing args=session/export/id/--standalone，Mac专用server整段保持baseline exactbytes；bridge与cancel fixture只修session位置$2→$3，nested shell自己的$1/$2不改。Mac cfg argv门真实红1FAIL(exit101, SHA0f47d5f55e5ae72dc9dc323a664023ae2635c8c69a9d1548e1f535d21049a569)→绿1/1(exit0, SHA40af4af83cdddb6d8fc63111df9771607325ff161dd993fa127a9d1ccc333420)，fmtcheck0。focused/strict与actual Claude单审最终门进行中；真实两次smoke由随后OG15-FINAL消费，AC3/VER2仍UNRUN，整FIX不称locally-accepted。3s默认/30000ms lease未调整，0modelcalls/Linux功能UNRUN，no bump/release/installer；原OG15全量仅历史，不冒称此source全量已跑。

2026-10-11 FIX-OG-CP01 argv phase locally-accepted（非whole FIX完成）：current2.0.26、4owned paths、Mac production region exact baseline retained。Actual final focused117/117、0retry、34.734s、exit0（SHAe0c5d752ed593f91e1da14ab871a8315f1472453379818efd94468a4a2225854），strict alltargets/allfeatures -D warnings exit0（SHA080ce9c9d52ea9540da0e201b30faf132fbfb5c545430aae4253d8cfc4740565），fmtcheck0、exact generic argv Mac1/1、sanitize fake-subprocess负例实际执行。bridge fake export cases在Mac缺bwrap条件不发生实际export，不冒称Linux runtime/真实OpenCode；其fixture仅$3适配。Actual Claude Code R1 PASS，0P0/0P1/1P2/4P3、35turns，rawSHA7e26d71382f06e6031eeb4d3c50d8fe760d81d1a7dddfeb1bc2a574facb458e2（2026-10-10T20:22:58.606853Z至20:26:50.023685Z）。用户单审literal PASS接受；非阻断意见登记DEFER-OG-ARGV-01，未修复，不重新加双审/零P2门。Whole FIX AC3/VER2仍待OG15-FINAL当前2.0.26真实两次连续内容smoke；final stagefull待对应actual finalsource，旧OG15全量不充当本argv变动full。正式4/61(6.56%)、FAST-OG01 basiclocal7/13(53.85%)暂不增计。0modelcalls、Linux operator UNRUN、版本仍0.30.42、no release/installer。

DEFER-OG-ARGV-01（用户优先基本功、Claude PASS的非阻断遗留）：P2 Mac生产client argv尚无具名sanitize guard，当前literal无--sanitize且整Mac region未改，generic fake测试不能宣称验证该Mac array；P3 generic负例只拒精确--sanitize而不覆盖--sanitize=true形式；P3 pin的commands flag定义与selfCommand源码应在后续source-record刷新补齐；P3 Linux独立server继承16MiB+1 RLIMIT_FSIZE可能对大store的WAL写入EFBIG，交用户Linux后验且UNRUN（含startup/shutdown3s总预算），本轮不开发Linux。原AC2语言的generic-helper限定补清是本receipt事实澄清，未声称消除其他P2/P3。重启：基本采集/字段审计及producer/verifier优先交付后另卡修；不得借此豁免当前安全/native proof/真实内容门。

2026-10-11 OG15-FINAL开工：FIX minimal argv actual signed-DCO main6d2a4dd4349e89b94fd7877bba2499184be15640、Claude PASS0P0/0P1/1P2/4P3与focused117/117已交付。当前FINAL只启用Mac真实内容harness（显式volcengine-agent-plan/glm-5.3-flash，无默认fallback；exact2.0.26版本门、临时project cwd/PWD预检；新Mac ignored keyedlane）、prompt/model/tokens/file源投影/持久化/停止/doctor/卸载保留断言，Linux原A6.5 functional branch未开发/未运行。当前真实两次smoke/focused/full/strict/fmt/Claude仍UNRUN，0modelcalls截至开工，整OG15及FIX未提前完成。必要写集既有20path baseline，FINAL实际增量按exact路径发布；producer/global配置不改，计划05 protected保持原dirty。

2026-10-11 USER-DEFER-SIMILAR 执行：DEFER-OG-OPENCODE-BOOT-01（计划外遗留，不在本计划继续排查/修复）：本机已固定OpenCode2.0.26真实`serve --stdio`在20s内无任何stdout readiness，关闭stdin后3s仍未退出、仅测试所属进程组SIGKILL回收。普通环境探针、禁FFF/filewatcher控制和完全私有空HOME/XDG/config/data控制三次均同结果，0modelcalls；不能归因Libra隔离、更不能宣称OpenCode上游已定位。SQLite原store只读schema查询正常仅排除该次读失败，不证明无锁/无其它故障。现有无关global OpenCode service未修改/停止，global安装/用户配置未改。

OG15-FINAL三轮真实gate都是FAIL/exit101：R1版本格式误比较已修（actual=`opencode v2.0.26`）；R2原认证probe未有界，109s人工结束仅其两个测试子进程，非自然PASS；R3加入Mac专用30s登录probe边界后真实超时自动回收。三次都未进入付费model session，实际prompt/model/tokens/files/source/stopped/doctor内容门UNRUN，不以kernel/mock/nonlive/全量替代。R1SHA8f0b68a41bdc040da6537dbe8cba04a47ffcd9dedcf40e5c629df746127a7443；R2SHAde8d32e9b0915753359dfdd3e633cb02cde9cc073d80b7b1b8930f7eee63a2c0；R3SHAab575de7db4a14aa0d1b4165e0e313d2236a88472075505fdf70a4976f9a32ac。新Mac harness/精确paid模型/临时项目预检/源投影断言/serial lane为可审source产物，真实验收推迟为具名遗留，未跑谓词保持未勾选，不称whole OG15/FIX complete。

按用户后续同难度问题统一延后、优先基础字段审计及producer/verifier策略：PREP→FIX argv已审signedmain6d2a4dd4→FINAL-HARNESS具名源码/非live/fmt/strict/单Claude/full→OG07/OG13→OG18/OG04。OG07只消费已审HARNESSSOURCE与PREP隔离实现，不再等待计划外BOOT遗留实际关闭；FAST-OG01释放范围/成员统计须显式列出该遗留且不冒称内容正例通过。NPR source-ready仍保留实际构建/签名/单审与完整family发布门；native SourceA/opaque归档真实证据及隔离硬门不变。当前正式4/61(6.56%)、基础local7/13(53.85%)不变，不通过降分母制造进度。全计划最终全量与必要四平台release/safety CI保留，Linux operator仍用户后验UNRUN。

2026-10-11 OG15-FINAL-HARNESS source phase locally-accepted，非whole OG15/FIX complete：12owned root paths（20path conditional baseline），exporter/guards/crypto与Linux功能branch未改。FINAL source actual focused120/120、0retry、20.714s、exit0，SHA6a0c29916dc772412edddd4db73d74372a0df100fd90bbff592ca25db9c64147；strictR2 alltargets/allfeatures -D warnings exit0（SHA371eec2017905d2d3815b578d416e9afe32fd238f098446fa1ac51fb2c4fe0b5），fmtpostfreeze0。双env final default nextest full exit0：9718/9718、13slow/1flaky/3leaky/10skip、0finalfail，829.017s（wall987.976s，2026-10-10T20:57:26.864778Z至21:13:54.840812Z，SHA5857d337083cc6a5f99cd859ba0571e65dc02fa2d62404083c16c8e6a640bdea）。新harness3非live门实跑，4ignored live门未运行，不能借全量证明内容或native。新registry lane已登记并生成器0，生成nextest config实际与baseline相同。

Actual Claude Code R1 FAIL（0P0/1P1/2P2/4P3、23turns、rawSHA56e3356e8e4963f0bad5edb192cd58bb3d2e1f485dacd408a88e59b0e87e851a）及strictR1 exit101两处empty-doc-line真实红保留；R1→R2仅删两处注释空行/旧heading，requiredClippy defect已修且strictR2实绿。Actual FINAL R2 PASS（0P0/0P1/3P2/5P3、35turns、rawSHA44676ace872b7c85dcba1f6e176eddfedf535b02f5c22d9b208cd6327edd32f0，2026-10-10T20:57:26.864614Z至21:01:43.580104Z），用户literal单Claude PASS接受，不借R2覆盖旧FAIL原文。其非阻断3P2/5P3→DEFER-OG-HARNESS-01，原R1额外非阻断意见仍保留未修。生产/source测试与public docs在review/full期间冻结；后写仅plan/status真实事实，不变更已审执行代码。protected plan05原dirty未混入，backend未修改/部署，版本仍0.30.42无release/installer。下一OG07→OG18/OG04当前41primaryblob字段审计→OG13→producer/verifier；OG13当前显式消费OG18 Deliverables，不反向执行。正式4/61(6.56%)、基础local7/13(53.85%)不变；真实Mac门DEFER-OG-OPENCODE-BOOT-01仍3FAIL/0OpenCode模型调用，未获native来源证明，Linux operator UNRUN。

DEFER-OG-HARNESS-01（用户统一延后、单Claude PASS非阻断遗留，UNFIXED）：P2 PATH symlink安装应先canonicalize再trust canonical parent（本机regular path尚未到trust阶段）；P2 session state首次看到后checkpoint期间可能改变，应在真实重启验收poll stopped；P2 current run权限/--auto及word-joining应读固定源码后再付费重跑，不猜/不加宽权限；P3 all-ignored Mac入口会启动两次Opencode，原R1此项P2/R2 P3原严重度均保留，当前验收使用--exact；P3 spawn失败/项目预检BLOCKED摘要仅stage而非blocked_reason；P3 Linux sourceguard格式脆弱（不作Linux functional proof）；P3 compatREADME local-smoke段位置；R1额外P3包括非零已完成子进程控制/禁止models-fetch env断言及INDEX具名timeout fn遗漏，未假称修复。唯一修复为原P1 requiredClippy注释问题；这些余项于基本采集/字段审计与producer/verifier之后另卡处理，不放宽native来源/隔离/真实内容谓词。

DEFER-OG-TEST-01追加本轮：未改clone source `command::clone_test::test_clone_local_git_large_object_set_completes_in_budget` TRY1 FAIL(5.633s)→TRY2 PASS(17.349s)，如实1flaky；3leaky现象保留UNFIXED（沿既有pending/GC/config工作线程来源核对日志，不声称0leak）。0finalfailed与Nextest退出0是actual full gate结果，重试与泄露不隐匿；不据此解除实机BOOT遗留。


2026-10-11 OG07 current2.0.26开工：OG15-FINAL-HARNESS actual signed-DCO source c27b2cb1bb43183e8e55a7decb431e23ff7dc7b3及remote main一致，12owned、单Claude R2 PASS；focused120/120，最终全量9718/9718、1flaky/3leaky/10skipped retained。真实OpenCode BOOT仍计划外DEFER-OG-OPENCODE-BOOT-01、0OpenCode modelcalls/native正例，whole OG15/FIX不假称complete。OG07默认30s、lease45s精确派生、同值插件render与Mac实际Seatbelt availability；Required生产profiles/runner/trust边界不改，Linux既有bwrap运行body源码保持、operator UNRUN。新增7named lib门current AC/VER重置待实际验证，旧私有结果仅历史；VER8纠正Mac桥测试早退误label，改actual kernel三门。当前source写集4prod+原docs/INDEX+plan/status，website仅fresh-preimage预算段。下一OG18 current41primary field audit→OG04→OG13→trusted producer/verifier；正式4/61(6.56%)、basic7/13(53.85%)暂不增计。用户单Claude PASS/direct-main/macOS-only/延期策略优先于旧描述，shared budget full仍在FAST-OG01最终actual bumped tree必跑，不借上一实现9718绿替代。


2026-10-11 OG07 current2.0.26 source locally-accepted（whole C/D pending）：actual focused145/145、0retry、21.734s、exit0、SHA35dbe3e742678d55714c365eb72c6f68f1deb077333a5d0c08f092eab6e584b7；含新增7门/既有plugin、全部exporter/export_job/list单测、Mac真实kernel三门、完整32architecture/serial/docs与3frozen oracles。首轮8/8 SHA73653c5e09fa9def439d3dcab633eec051a7990c7be4bba31f61915fb93976ef retained。strict alltargets/allfeatures -D warnings exit0 SHA98c951c31e9b12992b14188e803a47a344aabae19bf0089c06b2b3034a92e977，fmt0；website cf budget-only fresh-preimage typecheck/build exit0、source尚未commit/push/deploy。实际Claude Code R1 PASS、0P0/0P1/0P2/4P3、34turns、rawSHAd2c05bf366d060da88a25f90d61abc38e3fe23d16546bec959f7a8613c81b548（UTC2026-10-10T21:25:58.285851+00:00至2026-10-10T21:28:24.946296+00:00），用户literal单审门满足，无dual/zero-P2要求。review bundle只含首轮8/8 receipt，其“其它pre-push gates未在bundle”原文保留；最终145/145/strict/fmt与website外部actual receipts满足门，不倒改review原文。4prod/11rootowned exact-add/signed-DCO main Push下一步；postreview只两份plan/status真实事实，production/docs逐字节frozen未改。

当前预算default30s，lease=deadline_ms+15000=45000ms，plugin30000/45000ms联动；16MiB、Requiredprofile/runner/FD/trust/native gates不变。Mac fixedOS probe actualPASS，export_bridge可用只表示trustedbinary+sandboxprerequisite，不作OpenCode启动或内容/nativePASS。VER5 named actualdeadline拒部分输出+同run frozen livecheckpoint oracle证明既有metadata-only续行，非新real模型positive。原VER8 Mac bwrap早退误标已修currentsource-record，当前3kernelactualtests实跑；LinuxVER7/operatorVER11仍UNRUN（新Linux fname未作为已落地或实跑计分）。USER-A stage child不跑独立full，FAST-OG01最终actual bumped tree mandatory full保留；上一9718绿不是本次新budgetfull。真实modelcalls0、BOOT遗留计划外不重排查，SourceA无nativeproof不得归档。基本local8/13=61.54%，正式whole4/61=6.56%不变。下一OG18 current41primary来源审计→OG04→OG13→trustedprivateproducer/verifier。

DEFER-OG-BUDGET-01（用户统一延后、Claude PASS非阻断4P3，UNFIXED）：exporter moduleintro历史default3s注释仍旧（actualconst/docs30s）；Mac capabilityprobe允许default后denyfork/network/write，弱于完整deny-default Requiredprofile，因此仅capability而非完整profile/runtime成功证明；300ms注入/500msprobe在loadedhost可能timingflake（本机actual145/1450retry）；INDEX在bridge row描述lib门attribution不精确。review未在bundle确认hookoutercaptureceiling，保持残余范围而非声称已验证，其后续sourcecheck另记；本卡4意见不回填“fixed”，不改变profiles或延长/弱化deadline回归。待优先basic/fieldaudit及producer/verifier后另卡处理。


2026-10-11 OG18 current2.0.26开工：OG07 actual signed-DCO main96e7f52221c20abe0f9912675c72e9c729314259、remoteMATCH，四production/11rootowned，Claude PASS0P0/0P1/0P2/4P3、focused145/145/fmt/strict green已交付。当前OG18严格no-code，只plan02/status inline source-record；固定2.0.26 commit9b4ec5714d481559990db0a816d5dec19541a814、tree7e5ecbf5d14e173aa916f6c1e3b8760a8853db2b，41primary逐blob/SHA256/mode MATCH。当前审计/zero-content/writeset/单Claude/fmt/strict待本轮实跑，旧2.0.24 PASS不重用。普通三字段SourceA来源NO-GO为审计候选正确结果，不阻basic/producer源码推进、不授予opaque归档GO；真实内容BOOT延后且0modelcalls，Linux user operator UNRUN。正式whole4/61(6.56%)、basic8/13(61.54%)，本卡未提前增计。下一OG04 current字段契约→OG13legacyreader→可信producer/verifier。


2026-10-11 OG18 current2.0.26 source audit locally-accepted（whole C/D由FAST-OG01 pending）：actual source-record-final exit0、41fixed primary Gitblob/SHA256/tree100644 MATCH、39source-shape locators、bounded public-only/zero realcontent pattern/owned2/zero staged/protectedplan05 MATCH，SHAd01bfdd6619a9d6f1659ece3b2ac7a3cb6df1d85f3ed1a81978e32c731a6cabe；首次source gate locator错误exit1 SHAa45d953b1524f540cb71cea22ad552f6325e46bdd6ee20d8fd9052a62ed6fabe retained，修正session model-request hook字符串、transfer compaction及model nativeSettings定位，不修改upstream/主repo审计结论，不宣称该失败为upstream defect。strict alltargets/allfeatures -Dwarnings exit0 SHA11787046856545fa1a05c6c3fcd119ecc7c6393ed7ce4b5900a26dabcd465ff8、fmt0。audit no-release不触发full，无生产/fixture/网站/版本/模型请求delta。

实际Claude Code R1 PASS、0P0/0P1/0P2/5P3、68turns、rawSHAd88203727c641de0ee101442b7f3b8fcdaba7162c0ecf56df4353270205c9876（UTC2026-10-10T21:33:08.637953+00:00至2026-10-10T21:36:53.623236+00:00），逐current line关键source语义核对与NO-GO结论通过。Claude只读未运行hash命令；41digest由actual机械gate证明，不倒写成Claude运行。少数manifest包含但无inline语义claim的旁支只确认presence/digest，未虚称全部41内容深入review。准确bounded ordinary opaque来源NO-GO是审计Deliverable PASS，非native proof/归档GO；0OpenCode modelcalls与真实proofUNRUN、BOOT计划外遗留、LinuxoperatorUNRUN保留。current6AC/6VER闭合，用户单审literalPASS门满足，source署名main Push下一步。postreview仅plan/status事实/当前checkboxes，不改变已审inline source-record。基础local9/13=69.23%，正式whole4/61=6.56%不变；下一OG04 current字段/canary/closed truth table audit，再OG13、可信privateproducer/verifier。

DEFER-OG-AUDIT-01（用户统一延后、PASS非阻断5P3，当前原source-record UNFIXED）：transfer import locator104–111偏sessionCreated，实际encoding77–88/insert112–114；client Prepared locator594–598是test-only compileRequest，实际compile574–587；to-llm-message缺157–159 interrupted/172–183 metadata suppression及154–156 sameprovider/model记录；runner184–194 start/delta/end state spread合并未详列（后继OG04 truth table需显式消费合并后state、不作仅endpayload假设）；原manifest39行blob_sha1+2行git_blob schema异构，authoritative current-source-verification41uniform MATCH为本次机械身份事实。Locator补齐与记录整洁另卡；不冒称已修上文、不重写rawreview；这些P3不改变bounded NO-GO，当前原文不作为真实native provenance凭据。


2026-10-11 OG04 current2.0.26开工：OG18 actual signed-DCO source03aba3ef8c4e3c64f18f0c67b0c9d710a88ff899/remoteMATCH、Claude PASS0P0/0P1/0P2/5P3、6AC/6VER当前source audit已交付。此卡owned plan02/status/topMANIFEST三no-code路径，exact OG04 probe写入fieldA三candidate来源/type、visible/empty/null/unknown/compaction排除、最终merged state、UTF8byte边界/canary规范；topMANIFEST登记current版本与RG04冻结open-state反例限定适用。四RG04 fixture及agent_reasoning_contract_test byte/mode冻结，未改note/矩阵/assert，本卡规范不冒称OG05实现；所有literal2.0.24旧currentgate已重绑2.0.26/当前commit，历史不抹除。singleClaude/三方链/零内容/anchor/写集/RG04contract/fmt/strict当前待跑，0OpenCode modelcalls，native proofNO-GO且BOOT计划外，LinuxUNRUN。正式whole4/61(6.56%)、basic9/13(69.23%)暂不增计；下一OG13legacy safe reader，再FAST-OG01/可信producer-verifier。


2026-10-11 OG04 current2.0.26 locally-accepted（normative audit source，whole C/D待FASTOG01）：15AC/5VER当前审计门闭合。实际field-record-final exit0，current4版本声明exactone pin/commit相等、16version/commit/duplicate/missing负例拒绝、16explicit sourceanchor存在/linebounds、3distinct opaque canary符号、5RG04fixture/contract源码byte/mode冻结、JSONstring UTF8与escape文档不同的pure示例/零真实内容/owned3/protectedplan05 MATCH，SHA3062d6d659f5c0f8166728bf2032fe9d93960f76ec14f21d2bf4a33e3dc2c5fd。RG04+hookcontract actual7/7、0skip/0retry、exit0、SHA26ce9a379507c04a10d6906c8b96ba9e66b99171000db81fb9b6aeb0a941e7d3；strictalltargets/allfeatures-Dwarnings0 SHAca0d32c35b32318f89251bb8d1230902e26fcf9fa9dd31d7affd946c7d6444b5、fmt0。dualenv指同一run加载.env.test和.env.live-test两文件，不是两个独立platform/envmatrix run；review只含一次receipt事实正确，不补造第二次。no-code/no-release audit无需独立full；no website/source/modelcall changes。

Actual Claude Code R1 PASS、0P0/0P1/0P2/3P3、56turns、rawSHA9784cdb241d3530cb2d4c05685c29374b77776a629074d71e3e94930fb268e1a（UTC2026-10-10T21:41:35.933264+00:00至2026-10-10T21:45:38.645456+00:00），逐full/shorthand current sourceanchor人工核对、候选types/mergedstate/null/scope与NO-GO一致，用户单literalPASS满足。review只读未复跑机械hash/负例，其“privategate script不在bundle/visible redaction具体规则未包含”残余原文保留，不倒写为review运行PASS。本record是OG05后继closed truth/canary/versiondrift规范，不声称OG05classifier/fixture扩展或任何nativeartifact已经实现。RG04 frozen历史note仅open-state UnsupportedShape反例，topMANIFEST登记限定current适用、不升级native proof。source Push下一步，postreview只有plan/status真实fact和currentcheckboxes，probe与产品manifestfrozen字节未改。基础local10/13=76.92%、正式whole4/61=6.56%不变；下一OG13当前实际metadata/default export读端接线，随后FASTOG01与trusted producer/verifier。真实0OpenCode modelcalls、BOOT计划外DEFER、nativepositiveUNRUN/SourceA NO-GO，LinuxoperatorUNRUN。

DEFER-OG-FIELD-01（用户单Claude PASS非阻断3P3，UNFIXED）：reasoningField string allowlist继承OG05AC39规范，当前41source中它仅modelcompat选项、未找到reasoningstate producer，不把该allowlist当verified nativekey；reasoningDetails仅非pin illustrative catch-all例子，未在41文件出现；Anthropic signature_delta到end原locator缺1360/1406/1413/1464–1470与runner delta165补齐。当前probe原文保持已审snapshot，不假称这些locator/规范已修；后续producer/OG05消费应读取此准确残余，不为自称key制造native authority。可读canary具体字节/redact匹配与3opaque矩阵归OG05实现门，本卡仅规范不是fixturepositive。

2026-10-11 OG13 currentmain开工：基线d82197a8455683ad0714f809cedf0cd14ecc37c8；pure helper限定reasoning parts/预留metadata三legacy键，strict duplicate parser+serde JSON保留宽整数，非目标tool/user JSON不递归。实际默认export及load_checkpoint_metadata_blob_from_storage接线，catalog provider绑定/metadata kind一致性；raw authorization/audit原样。必要增加tests/command/agent_checkpoint_export_test.rs实际CLI两门，checkpoint metadata实读一门，VER由旧private3重绑current6，旧clone恒等门不算raw证明。首次compile101已修API；R2 actual5PASS+strict0仍非final acceptance。owned9与website段freshpreimage/protected05保留；final focused/strict/site/Claude待跑，native未证明NO-GO/BOOT计划外/LinuxUNRUN/0OpenCode模型。whole4/61(6.56%)、basic10/13(76.92%)暂不增计；下一FAST-OG01 aggregate再NPR。

2026-10-11 OG13 current2.0.26 locally-accepted（whole C/D后置FASTOG01）：6AC/6VER实际source门闭合；默认export已在role/manifest/OID/bounds后接入legacy helper，catalog kind须与metadata一致；缺session/不一致/坏JSON保守拒绝。三canary、u64::MAX、非目标tool/user JSON、实际bound metadata、default无审计/raw原bounded bytes与audit、catalog mismatch、坏JSON及恰好valid JSON prefix但truncated拒绝均实际测试。Final focused188/188、0retry、1leaky（旧page_cursor_rejects_malformed_values）、8302未选中，SHAa1cfbd6f170792c3d8216a8f5969193efe23db56f470dfdc37c682755bbd0941；之前R2同188PASS有6leaky（含新metadata门），不能抹除或冒称始终零leak。strictalltargets/allfeatures-Dwarnings0 SHAe2f97288aaa8b563d734871bbc32050d9c3bc930f689d55cccc63e64288dc8be，fmt0；site typecheck/build0，fresh reader段之外原bytes保留，website commit/push/deployUNRUN。三冻结oracle/32arch与RG03/04实际readguard在focused绿，未改原源码oracle。首次两个compile101为API误用已修，旧strict-final日志被后续同名gate覆盖，原失败摘要/hash仍留历史，不能声称旧raw log现存；filter初次exit94无测试运行，调整binary regex后真实188非零。bun未在PATH的website启动错误后按实际pnpm scripts执行0，不冒称初次绿。
Actual Claude Code R1 PASS、0P0/0P1/1P2/4P3、44turns、rawSHA892935536d392758691988cae732cf25e987953da5c31255f07b660b4547773c（UTC2026-10-10T22:04:12.181936+00:00至22:08:29.264007+00:00）；review source/receipts只读，parse_canon_value具体实现不在packet、未运行tests/hash，准确保留边界。用户single literalPASS足够，P2/P3登记DEFER-OG-READER-01 UNFIXED/计划外，非已修：P2所有provider新增session/metadata硬依赖且store error未附raw remedy、其它provider缺sessionE2E未测；P3有删除时普通JSON重序列化可能数字边缘归一、skill严格解析失败仍skip无diagnostic、anthropic.redactedData旧opaque不在三legacy键本卡scope、带agent_kind的transcript root接受为metadata形状。chunked OpenCode及历史多行JSONL/real OpenCode格式未实跑，不把六synthetic case称native proof。modernstate/SourceA工厂归OG05/typed链，no archive GO；Linux用户后验UNRUN，真实BOOT计划外DEFER，0OpenCode模型调用；实际full仍由FASTOG01最终bumped source必跑，旧9718不代表新增reader实现。
本次owned9 signed-DCO ordinary main Push待下一步核验，postreview只plan/status事实复选框；production/docs/tests frozen9其余bytes/mode相等，protectedplan05 a705585d unchanged。基础local11/13=84.62%，正式whole4/61=6.56%仍待阶段实际C/D；下一FASTOG01 aggregate发布，再NPR00当前源码seam重验与可信producer/verifier。私有固定2.0.26官方源码8745files/139374369bytes逐Gitblob/mode与postwriteSHA MATCH只作后续材料，不是semantics/build/native验收。

2026-10-11 FASTOG01 currentmain0.30.43开工：OG13 actualsigned main341e19d743373fca0c7f1716a9fe9f931cf921c5/remoteMATCH，6AC/6VER+focused188/strict/fmt/site+singleClaudePASS0P0/P1/1P2/4P3；basiclocal11/13=84.62%、formalwhole4/61=6.56%。当前GH stable0.30.42→本次patch0.30.43，version只改3权威faces+self lock，不改依赖/runtime。实际member/group source及已审RG inheritedbase核验，OG15/FIXBOOT三失败UNRUN项保持outsideplanDEFER，source阶段交付不是real/nativePASS。发现EN/zh插件段仍18秒，与actual budget30+margin15及本轮codec/render tests45相矛盾，推荐必要docforward两数字改45，prod0不变，finalsingleClaude覆盖；moduleintro旧3秒P3不在本补丁。cf actualfetch a003185在isolated Libra nativeworktree，只取currentEN三已审正文slice追加到最新agent页、outsidebytes保持；rootbackend4dirtydocs不夹带，不复制历史dirtyfile整体。full/fmt/strict/feature相关门/build/finalreview/mainCI/tag/stable4platform/releaseAPI/installer/actualsite均待，未记complete，无PR/dual/parent新授权。

2026-10-11 FASTOG01 0.30.43 final local assembly gate CLOSED（release/whole Acceptance尚未完成）：actual bumped current source full双env nextest退出0，9731/9731 passed、14slow、1leaky、10skipped、无retry/flaky；UTC2026-10-10T22:17:19.504773+00:00至22:37:33.171075+00:00，test execution837.118s，rawSHA49549f988356a0c471d7ea3031bc47f139a26f3aea0471dafee7aa68f7577d82。唯一leaky为settings::tests::config_device_type_is_rejected_without_opening_it，保留实际提示、不记零leak；version-surface两门真实PASS。fmt退出0；alltargets/allfeatures strict clippy-Dwarnings退出0 SHA3c63d804faf394e8648b81703f010171ad2d00138032773ae8904d3fbdbd8e38；releasebuild退出0 SHA cc6a22137e5cc66c6e8259a3bc365bfd8164c5ca7d3ad0fd8ca51c50376291c3，实际binary libra0.30.43。default releasebuild仍有既存CaptureIngressCommand::from_payload deadcode warning，allfeatures strict绿不冒称default零warning。isolated latestcf website typecheck/build均退出0；14member source phases/40 committed paths签名DCO与最终source writer/actual review closure MATCH、unknown paths0。

实际Claude Code R1 literal PASS、0P0/0P1/1P2/2P3、39turns，UTC22:29:45.880701至22:32:40.519012，rawSHAb73ff7490454a8a65e857b6f9091f5c3eddab335acb8c718d35e4973d121f9b7。review时full仍RUNNING，当前exit0是随后实际gate，不倒写成review已实跑full/PGP/hash。postreview只四plan真实事实补记，其他六source/docs/version文件冻结保持；原P3 pending文字按本段追加纠正、历史不抹除。DEFER-OG-BUDGET-DOC-01（计划外，UNFIXED，用户统一延后，owner本root待基础/producer后另卡）：tracing/agent.md343仍18秒的P2陈旧文案；已审EN/zh两数字为45，runtime30+15已测，遗留不假称fixed。moduleintro3秒原DEFER-OG-BUDGET-01继续UNFIXED。当前只准备signed-DCO main Push；final-head main安全CI、signedtag、stable四平台release/manifest/brew、exact curl安装及网站实际cf/public页面待实证，C/D不complete。whole4/61=6.56%、basiclocal11/13=84.62%不提前增计；下一FASTOG01真实发布收口→NPR00当前源码seam重验→可信producer/verifier。BOOT三实际失败/0OpenCode模型调用/nativeSourceA NO-GO/Linux用户后验UNRUN全部保留。

2026-10-11 NPR00 current2.0.26 refresh locally-accepted：8745fixed官方Gitblob/SHA/mode及postwrite身份MATCH；177历史参考重构exact91+103含重叠、39changed、无missing；仅29完整currentsource语义门，不冒称177/8745全安全审查。八named source审计currentinline原卡已列源函数/实际digest/PASS/未测。实际Claude R1 PASS0P0/P1/2P2/4P3/48turns/rawSHAc7f714e85ddfc336740b1584a2f8e7a86c3bd2ea3cce76faed8f55343ab1b5e0；最小规范producerVersion2.0.26-libra-native.1，旧R11/e7审查历史保留。DEFER-NPR-SOURCE-AUDIT-01 UNFIXED，runtime loader补集/actualtransportobject约束由NPR04/05/08核心准入继续消费，不准绕过nativeSourceA。private8745currentimplementation仅材料复制，codec/issuer/proof/model均未运行/实现；BOOT计划外DEFER，0OpenCode调用，Linuxuser后验。正式4/61=6.56%；下一NPR01 TS canonicalcodec按FASTOG01实际交付gate，source-ready≠archiveGO。FASTOG01 signeda8mainCI运行、cf64e已signedPush并部署versionf7e0569b、在线HTTP200五current文案checks真实PASS，0.30.43 tag/release/install仍UNRUN。

2026-10-11 NPR00 current no-code gate补记：本轮两env同run fmt退出0、strictalltargets/allfeatures-Dwarnings退出0 SHA5f030a24b66b6e15a23f58f4582af08e5fff28fae00880ba864f12c6c74f7026；177reference实际Gitblob/SHA256/mode重核0mismatch、29reviewfiles冻结MATCH，身份门exit0。no-code/no-release无ER13全量触发，FAST阶段9731绿为该source已有事实、非本卡新full；NPR00 wholeC/D等NPR20。source签名Push下一步。

2026-10-11 NPR01 current2.0.26开工/必要依赖修订（用户已授权推荐方案与自动推进）：为优先基本功，两个纯canonicalcodec的无权限source阶段消费FASTOG01已署名a8basic finalsource/实际9731full绿/closedsingleClaude与NPR00 currentsigned4bedauditPASS、已发布NPR19v42；不等stage远端release等待窗。此推荐只放行NPR01/02源码、focused测试与用户逐卡signedPush，不允许issuer登记/native签发/approvedbuild/source-ready归档或提前stagecomplete；NPR08/20明确仍等FASTOG01实际done/complete及原native/安全CI/四平台交付门。当前FASTa8CI运行，cf64e上线HTTP200actualcurrent5checksPASS；stage0.30.43 tag/release/install未发生。NPR01 private写集仍四declaredschema/test/fixture/README路径，currentBun1.4.2 locked schema依赖准备actualexit0、source/lock冻结后验待；rootonlyplan02/status，protectedplan05保留。producerVersion按已审currentcontract2.0.26-libra-native.1，独立3Pythoncanonical syntheticvectors398/436/476bytes供回归，不是签名/nativeproof。codec不得提供signer/witness/Session/Nodecrypto权限；真实BOOT不再排查/0OpenCode调用、SourceA保持NO-GO、Linuxuser后验。whole4/61=6.56%不变；本卡focused/rootcheck/fmt/clippy/singleClaudePASS/签名源码交付待。

2026-10-11 NPR01 current candidate：private新source native-origin.ts为browser-safeEffect封闭Receipt/Endpoint/Policy及三canonicalpreimage，sameprefix/version一次BEu32/UTF8LP/rawhex/签名不入前像；typedcodec不sign/verify/hash/grantnative，JSrawJSONduplicate边界归RustNPR02。fixed字段/protocol/version/u32/顺序/重复/控制字符/lone surrogate/UTF8length/8192proof及64KiBdescriptorcap、HTTPS exactspelling/emptyqueryfragment/port443拒绝；regexp$末尾换行漏洞自查已加exactlength与四反例，非未修后验依赖。privatefixture3receipt+2policypath+2URLspelling independentlyPython bytes，allzero假signature只wiredata、不是nativepositive。实际六exactnamedtests各1PASS/0fail；combined6PASS85assert/41ms，focusedrawSHAfcc093c59db1d409a9b19b931f6f91ad7d28c64627160ee0367835cf34c67c71；schema类型check0 SHAae47736bb2a9401aa9743893f2f64b9a605e65a99d78cd991a8021e0f7722d87，privatePrettiercheck0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20；Librafmt0/strictalltargetsallfeatures-Dwarnings0 SHAd5885e70593dabd515dd3921c59018f5faa4c1064771f1009bbbfd4af57b131b。全8745existing源码/lock/mode未改，新private只原I四paths、root只2plan/status，protected05保持。
DEFER-NPR-TOOLING-01（计划外UNFIXED，user类似hard外围延后，ownerroot）：full frozen ignore-scripts Buninstall180s实际timeout124；filtered schema准备0后newtest原staticJSON typecheck2已修Bunfile读取、missing@script由partialfullworkspace link解决。formatter首次127缺rootbin；借只读同declared版本Prettier3.6.2/tools，privateignored.bin绑定，不改global/源lock。实际root canonicalcheckR1 exit1因bun turbo误选turbo.json，绑定同pinTurbo2.10.2后R2 rootlint1509warnings/0errors、42包typecheck启动但posts缺Astro退出127（5successful/30scheduled）；绝不记root全量绿。此外围setup不在本codec修OpenCodeblog/desktop/GUI源码；NPR08/20原核心build/runtime/nativeSourceA与真正full/安全CI/四平台仍须实证，rootfull工具恢复另卡，不借已运行partial冒称publisher/source-readyGO。当前singleClaude与签名sourcePush尚未发生，不locallyaccepted；整体whole4/61=6.56%。

2026-10-11 NPR01 source local验收：actual Claude Code R1 PASS（P0=0/P1=0/P2=2/P3=4；74turns；UTC23:11:21.293339至23:17:37.871447；rawSHAcd90a53c870d979e493903b598387dcb30716db383f3e64277b70dda3461e659）。六criterion均PASS，六exactnamed tests/combined6PASS85assert、schema类型/Prettier/Librafmt/strict实际绿；fullinstall124及rootcheck127/Astro依DEFER-NPR-TOOLING-01保持FAIL，不冒称全root全量。评审P2旧NPR08版本与重复dependency记录按现已审当前绑定/source-only修订同步；P3宽throw断言/compactsize/schema单独decode宽/browser和entrypoint未测记录DEFER-NPR-CODEC-DOC-01计划外UNFIXED，后续consumer必须经过完整codec不能把Schema.decode当native校验。私有四文件冻结SHA如下；all8745original源码/modes不变。签名main source发布为元数据交付，实际C/D待NPR08/20，来源A/native仍NO-GO，无issuer/signature/native positive或OpenCode模型调用。
NPR01 private `packages/schema/src/native-origin.ts` SHA256 08744ded033129490139ab44846fc7bbcdc0024af88916c94bb378a96101d3c6 mode 0o644
NPR01 private `packages/schema/test/native-origin.test.ts` SHA256 53c89d22b3402e001922b5908e3a40c84700e3ee55e3a1574b2ff15b1599fc09 mode 0o644
NPR01 private `packages/schema/test/fixtures/native-origin-v1.json` SHA256 9bea0dd73736dfa3ce7544921ef7c7286c1adf30463b0d2d913b54f5e8e1386b mode 0o644
NPR01 private `packages/schema/README.native-origin.md` SHA256 10dad9ae70268cf1990d175e67b87a532f5d20c80c4445a04da5922097dcbbd1 mode 0o644

2026-10-11 NPR02 source开工：消费NPR01已审六criterion/六具名tests/独立vectors及actual signedmain2cfa8c7，source-onlyhandoff不提前继承FAST stage/native权限。写集仅原卡5source/fixture/docs及2plan/status；protected05不动，sourceA/issuer/nativepositive仍未实现。focused/fmt/strict/actualClaude/mainPush待。

2026-10-11 NPR02 current candidate：Rust纯wire Receipt/Policy/Endpoint codec按current2.0.26 source/contract，重用coverage strict raw duplicate guard后丢弃中间值、对original bytes直接serde封闭decode，避免float normalization。rawproof8192/descriptor65536先于解析；text原UTF8/无C0/C1/nonzeroU32/closedmembers/base64padding/orderedfieldlists/URL exactspelling/两installationpath/coredescriptor同TS。production无unwrap/expect/sign/verify/I/O/nativefactory，public wire容器mutate后每canonical入口重验。fixture与NPR01 final frozenexactbytes相同 SHA9bea0dd73736dfa3ce7544921ef7c7286c1adf30463b0d2d913b54f5e8e1386b；3receipt+2policy+2endpoint独立Python→TS→Rust逐byte/SHA全部实证。
实际focused R1六PASS0FAIL，strict R1因chunks_exact_to_as_chunks exit101 SHA8cce1cdd3c6b13b266fa224e5cf74dbb4a1554c03d518d3b2877452a4d2720b8，已正修as_chunks，无lintallow；final相关门重新实跑如下（R1不抹除）：
focused-final exit0 SHAb6bbeb1dfa374745fc70a48425c45180b9212c84608cdcf459146b27e94b976a UTC2026-10-10T23:32:39.568966+00:00→2026-10-10T23:33:06.824839+00:00
fmt-final exit0 SHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 UTC2026-10-10T23:32:36.349152+00:00→2026-10-10T23:32:39.568629+00:00
strict-final exit0 SHAa19373fecc5f404098eea354686dd53ee3ef43b989caf1b7abb61cd9efdd9e97 UTC2026-10-10T23:33:06.827266+00:00→2026-10-10T23:34:09.003529+00:00
named-canonical exit0 SHA617a3a44a3f762313d405bed48658bb266d4114b5fe66873ad087b38f40f2df3 UTC2026-10-10T23:33:06.827324+00:00→2026-10-10T23:33:07.436985+00:00
named-grammar exit0 SHA69b33933915e02bc1dbbf576bde23c7775d2de186d05f9cf440b0b056576fc34 UTC2026-10-10T23:33:07.437346+00:00→2026-10-10T23:34:09.081013+00:00
named-raw_duplicates exit0 SHA61e538a9d6093ca5c5a28be974bed305e720436127f39e2fe6a6022a80f7c6a1 UTC2026-10-10T23:34:09.081362+00:00→2026-10-10T23:34:09.704915+00:00
named-utf8 exit0 SHA8d11accf2707cc4803cfebfafaca905db98b57809b840d2ac53d26d4a231738c UTC2026-10-10T23:34:09.705308+00:00→2026-10-10T23:34:10.192122+00:00
named-bounds exit0 SHA00526dcd11c50efe74be9020835d0671b9054dbbf1e85af7dd9dfb855c9a0588 UTC2026-10-10T23:34:10.192520+00:00→2026-10-10T23:34:10.675813+00:00
named-no_authority exit0 SHA7c2fb313da638db54fded040fd0b097f3071a5b7acb313c001a482cbdb1844c8 UTC2026-10-10T23:34:10.676210+00:00→2026-10-10T23:34:11.158706+00:00
两env均source，不输出凭据；没有modelcall/issuer/runtime/sourceA positive。NPR02 leaf codec无SQL/CLI/sharedhelper写入，不触发ER13全量；旧FAST stage full9731只其a8 source实证，不冒称newRust全量。最终NPR20 full/安全CI/四平台硬门保留。singleClaude/签名mainPush尚待，不locallyaccepted；CD待NPR20，正式whole4/61(6.56%)。当前NPR01真实signed-DCO2cfa8c7 remoteMATCH、R1pushSSH失败R2成功保留。计划card直接授权，无另建Issue/PR。

2026-10-11 NPR02 source locally-accepted：actual Claude Code R1 PASS，P0=0/P1=0/P2=2/P3=4，46turns；UTC2026-10-10T23:34:41.964306+00:00→2026-10-10T23:38:33.426898+00:00；rawSHA311304107c4bf1b0adda4cfb629e0d7c8cda873ff8ee7e24850355518c33fee2。六criterion均PASS，fixture7向量/6focused/6exactnamed/fmt/alltargetsallfeatures-Dwarnings严格门已实跑绿，strictR1失败修复后final重跑0。DEFER-NPR-RUST-CODEC-01（计划外UNFIXED）：P2privatehex依赖先验证/URL双实现仅具名spellings证实，P3UTF8负例位置/1023与1026而非恰界/长URL封闭失败/receipt未列source前缀；未把P2/P3修成新的准入门。真实runner每门实际source两env，记command cargoargv而非shell前缀，runner原bytes可复核。共享coverage源码未改、rawduplicate负例实证，不虚称Claude现场hash/命令；实际Claude服务调用由审核CLI执行，0仅指OpenCode模型调用。postreview仅原卡checkbox/事实，不改变5个已审source/fixture/docsSHA，protected05保持。sourcePush是family源码交付，CD/NPR20及native来源门待；旧FASTfull不冒称newRustfull。下一NPR03 issuer安全加载与独立安装；正式whole仍4/61=6.56%。

2026-10-11 NPR03 source开工：actualNPR02 signed-DCO main24874de remoteMATCH、6AC/6exactnamed及finalfmt/strict0、ClaudePASS0P0/P1。按user自动推荐/优先producer策略，family依赖消费前卡local最终Deliverables；逐card private source与synthetic focused/main元数据Push继续，FASTOG01实际a8 source完整full/审查已完成但远端发布待，现只扩展源码开发，不给实际approvedbuild/native/source-ready、NPR08/20 C/D或阶段Acceptance。FAST既有a8基础tag/release是该source-only handoff允许的先行交付，native家族真实release互斥窗口在其actualcomplete后开启，不能以NPR02早期源码开工阻其既有交付。NPR08/20仍硬等FASTactualdone/complete；no modelcalls或globalchange。Core filtered frozen/ignore-scripts/prefer-offline install229packages80.76s exit0 SHА7e90e045bfd5d3b8ec23f22e70fcf87bef14c80f563ed849a876b5af58efea0e；all8745original bytes/modes及lock未改。current tsgo7.0.0-dev.20251207.1私有bind后core基线typecheck0 SHA8f0c4bff8dfe0ad0cea81fc8587f22596a0ee3fb280e4af824e688108e22e4cd；之前bin缺tsgo127保留，不冒称root42包全绿。写集原private5和root2，protected05不动。

2026-10-11 NPR03 current candidate：private5原写集，current2.0.26 Nodecrypto Ed25519安全issuer init/load/sign及封闭owned安装Descriptor；CSPRNG每安装独立、no-overwrite目录/stage完整验证/已有或partial不替换、0700目录与owned非symlink regular0600 key/descriptor，O_NOFOLLOW bounded reads、actualprocess.execPath streaming SHA/stat/type/mode binding；actualcontrolledselectedcommandPath测量transientobserverSHA不进stablePolicy/export。BuildRecipe明确trusted build/runtime caller输入不是由安装manifest学习许可，exactapprovedtemplate三slots/closedsubstitutions/eventmapdomain/codecpolicy重算，actualloadedbytesport需NPR04/16提供，本issuer不取代nativewitness/taint/transport证明。closedminimalJSON roundtrip拒dup/escapedalias/非canonicalnumeric/invalidUTF8+64KiBcap，key不回传/no-secret-output真实privateCLI子进程0；CLI privateinit使用同tree siblingCore显式source引用，未改package/lock/runtimeCLI/global，temporaryundeclaredpackagebind已删。独立Python公共syntheticseed准备fixtureSHA d3589b58c490a0937cc69d84f8b124b1674151f1b142c200a95af40779dbdff1，423-bytepreimage actualproductionSigner exact64bytesignature/Nodeverify绿；只synthetic，非native/model/provider正例，ring消费者仍NPR11未测。
首轮5PASS3FAIL，第二轮7PASS1FAIL/71expects，temporarylink试跑8PASS0FAIL/81expects不作final，原因test0500重写/CLI未声明core入口；已testhelper wx+首次exists/observerupgrade显式chmod，以及init相对siblingsource导入正修。首次strict+fmt0但coretypecheck2 testreadonly mutation、namedregex0tests exit1，已只test用Types.DeepMutable/top-level named门重跑。原早期toolconsole非单独rawlog，retained structuredfailfacts不伪造raw证据；保存named/coreR1真日志。最终8focused PASS81assert与8exactnamed各1PASS0FAIL，coretypecheck/format/Librafmt/strict0如下：
focused-final exit0 SHA47eb90063ccc21e5f21f6998fe7ac54822370b6c29f86055a0c5d42f3a04997e UTC2026-10-10T23:53:19.983390+00:00→2026-10-10T23:53:21.185518+00:00
core-typecheck-final exit0 SHA8f0c4bff8dfe0ad0cea81fc8587f22596a0ee3fb280e4af824e688108e22e4cd UTC2026-10-10T23:53:19.983321+00:00→2026-10-10T23:53:20.390217+00:00
private-format-check exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20 UTC2026-10-10T23:53:22.953274+00:00→2026-10-10T23:53:23.058374+00:00
fmt-final exit0 SHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 UTC2026-10-10T23:53:19.983255+00:00→2026-10-10T23:53:23.870472+00:00
strict-final exit0 SHA806fb6e9463f0390603d6c8d26c378f4c7986994679ac2a1463b67bfaffc4157 UTC2026-10-10T23:53:19.983172+00:00→2026-10-10T23:53:20.973706+00:00
named-new_key exit0 SHAe0acea55121a538f6acf3dff5301a6d14445f471152007e848e3af978a898533 UTC2026-10-10T23:53:21.185771+00:00→2026-10-10T23:53:21.423006+00:00
named-no_replace exit0 SHA36064c2e4f7a068b50fc28bdbd908d3f017db1aeb4e4b135bc05f18a14a4511e UTC2026-10-10T23:53:21.423303+00:00→2026-10-10T23:53:21.624758+00:00
named-file_posture exit0 SHA8d19b705423b92b49995b9af05acc28e856c2e63ca3e1cabd3ccb98a312eaa8b UTC2026-10-10T23:53:21.625071+00:00→2026-10-10T23:53:21.793054+00:00
named-binary_binding exit0 SHAc5466a5825d18eee69b4d553e9aa7bd71645b917f3d8d7cc67e642873ceea455 UTC2026-10-10T23:53:21.793383+00:00→2026-10-10T23:53:22.030417+00:00
named-cross_signature exit0 SHA666ff5c9e66f7bdea9e0783d0cc2b38adc5a74a1b3efb380b046a3ba8b455e5b UTC2026-10-10T23:53:22.030862+00:00→2026-10-10T23:53:22.197234+00:00
named-no_secret_output exit0 SHA8ed79695084a104bf807322d42386cb098132646aea375645bc708482c88cf6d UTC2026-10-10T23:53:22.197578+00:00→2026-10-10T23:53:22.442936+00:00
named-error exit0 SHA09d462652bfc0d56479fa34244a958689272a9f6c857453e0de5971528b1bc65 UTC2026-10-10T23:53:22.443263+00:00→2026-10-10T23:53:22.572961+00:00
named-descriptor exit0 SHA7906662921035ab5e06a4022d409013a4a7bb350b8c8d9fe17e80a515483dfc9 UTC2026-10-10T23:53:22.573254+00:00→2026-10-10T23:53:22.952958+00:00
实际rootcheck入口R1因nestedbun PATH缺失127，R2加本次pinnedBun PATH后lint1509warnings0errors/no native-origin hits、SDKdocs0problems，但www缺Astro127；0successful/30scheduled不能记完整rootPASS。延用DEFER-NPR-TOOLING-01计划外UNFIXED，不修www/blog/app桌面source；核心actualfamilybuild/runtime/full/NPR08/20来源门不借此豁免。currentinstalled同pin工具替换已知old borrowedlink前核验exactwrapperSHA，privateignored.bin不改global；all8745official bytes/modes与已审NPR01private4不变。NPR03 source/noRust/SQL/CLI公共面变更，不触发newLibra ER13 full；最终NPR20真实full必须跑。actualClaude/sourcePush待，C/D待NPR08/20，SourceA nativeGO未获准，0OpenCode modelcalls；正式whole4/61=6.56%。basic a8最终8jobCI+CodeQL已actualgreen、signedtagv0.30.43/sourcea8已remoteMATCH，release run38096699927四平台pending/installUNRUN，不把tag推送称发布complete。currentLibra模板真实owner为src/internal/ai/hooks/providers/opencode/settings.rs（R11旧settings路径是历史anchor），三个slot本次rg核验各一个模板位置，公共buildsource归NPR17/08，未改这个owner。

2026-10-11 NPR03 source locally-accepted：actualClaude Code R1 PASS，P0=0/P1=0/P2=2/P3=5，72turns；UTC2026-10-10T23:58:23.150506+00:00→2026-10-11T00:04:58.597069+00:00；rawSHA2517e1cde0f79724236f1ca10865b596122090fa3f2ca1bae0943002151b618f。8AC/8exactnamed各1PASS/combined8PASS81assert，Coretype/Prettier/Librafmt/strict实跑exit0；rootcheckwww缺Astro仍127真实FAIL/计划外DEFER，不假绿。DEFER-NPR-ISSUER-01计划外UNFIXED：P2 String.replace特殊合法路径$replacement语义failclosed（实际Libra literalrender绑定拒，普通路径已有两安装实证；后续loadedbytes/controlled渲染不得凭此扩SourceA）、CLI init静态类型未单独实跑；P3 eventkeysUTF16与ASCII实际同序/secretoutput未专测32byteseed-only/DER临时buf未zeroed/renderedPolicy直接errorprefix/固定syntheticpath并发冲突failclosed。按用户PASS单审与外围延后，不新增第二审或变相放宽native准入；将来核心实际SourceA审查若暴露material缺口仍修。实际Reviewer逐byte手验423preimage、声明未现场hash/运行，actualfixture/publicSeed/Bun signer正负门由原日志证明，ring仍NPR11未跑。原私有5文件冻结SHA登记如下；official8745和NPR01private4未改、postreview仅root2状态事实。C/D待NPR08/20，签名元数据sourcePush不是artifact/nativeGO，SourceA全链actual许可仍未实现；0OpenCode模型调用/无globalchange。下一NPR04 actual插件加载与generation准入；formalwhole4/61=6.56%。
NPR03 private `packages/core/src/native-origin/issuer.ts` SHA256 e668757a93afb61042d846c4a0840a3fe72790f981d8103c349e0f1d90e64255 mode 0o644
NPR03 private `packages/cli/script/native-origin-init.ts` SHA256 f1dd07b89152112ff54cebc4dfc08975b06d0228598db0a5dc00a6ad5f241333 mode 0o644
NPR03 private `packages/core/test/native-origin-issuer.test.ts` SHA256 91277b1575a7735a52efd10d4d5ce50e699878622bb573f527bf92117bbedf01 mode 0o644
NPR03 private `packages/core/test/fixtures/native-origin-signature-v1.json` SHA256 d3589b58c490a0937cc69d84f8b124b1674151f1b142c200a95af40779dbdff1 mode 0o644
NPR03 private `packages/core/README.native-origin.md` SHA256 5e08752b38f6c986e9de9caefa24f44aa34b72086abf998621f98c8a70054a72 mode 0o644

2026-10-11 NPR04 current2.0.26开工，源基线0a84ac2575289a1fce4f7271b42a90ac8dcef740：原11private写集，新增runtime2/build/index语义取材补集不新增许可；Builtin真实闭包、同捕获bytes求值、precache单向taint、generation/cleanup fence核心必须实跑，不作外围DEFER。NPR03 actualsignedDCO/sourcePush remoteMATCH，0.30.43 release38096699927仍in_progress，whole4/61=6.56%。

2026-10-11 NPR04 current2.0.26 candidate：9production/12privatepaths（新增当前Libra实际13600-bytetemplate小fixture），runtime-owned compiledPlugins原对象闭包与wrapper弱身份关联、compiledProviders固定importlambda；不按ID/revision/source标签授权。受控Libra有界一次bytes捕获与完整closedrender比对，Bun data module同一不可变string实际求值，namespace→Promise adapter→finaleffect因果关联；仅两node builtin static依赖，不批准effect/plugin host自发现模块。未登记依赖/扫描失败先process taint后普通loader保持。未知file/npm/provider与AISDK.runSDK/runLanguage/language先于eval/install/hooks/cache返回permanenttaint。首轮5PASS2FAIL发现Bun transitive require.cache重建module-localtaint，已不可配置不可写process Symbol marker正修；pre-policy/failedimport/unload/cache/Core依赖重建反例实跑，不能视外围DEFER。
native registry epoch/pending/actualActivation.effect/healthy配合opaque snapshot+unchanged：hold/source change/setup/transform/fallback/close令attempt失效；failurecleanup普通readiness先恢复但nativecleanup token保留直到真正Scope.close完成，保留旧finalizer.awaitreadiness契约。捕获后磁盘置换仍执行捕获source、两合法controlledpath/闭maporder负例；独立literal render guard拒NPR03已DEFER的String.replace特殊路径，不让其成为来源权限，未修issuer/未宣称这些特殊path受支持。Observers和eventmap仍synthetic，不模型/不nativefieldpositive/不SourceA准入，不改global OpenCode。7focused top names20isolatedvariants、7exactnamed各1PASS、97ordinaryregression/375assert PASS；Core/Plugintypes、Prettier、Librafmt/strict绿。rootcheck新file3Reflect errors actualFAIL，已typed in-operator修无suppression；finalrootlint1509warnings0errors/SDKdocs0问题，42包check最终session-ui缺Playwright/deps exit2（3successful/30scheduled），计划外DEFER-NPR-TOOLING-01真实FAIL，非fullPASS。canonical typecheck意外覆盖tracked storybook/playwright/story.d.ts，已按officialfixedsource/mode逐byte还原，详root-check-generated-source-restore.json，不带UI source delta。初轮保存consolefacts不伪造rawlog；gates-before-render-guard保留旧一轮绿，只final日志当最终源实证。
focused-final exit0 SHAf21762fa800c3b2139ffb9017a141f275687a9b6bf7b98e3f185522415de24ae UTC2026-10-11T00:27:19.922145+00:00→2026-10-11T00:27:27.140678+00:00
ordinary-regression exit0 SHAd1e2800b914c783dd65f4950398d10b61d275c979fa33bcff7bec21b7805091e UTC2026-10-11T00:27:15.871851+00:00→2026-10-11T00:27:19.921748+00:00
core-typecheck-final exit0 SHA8f0c4bff8dfe0ad0cea81fc8587f22596a0ee3fb280e4af824e688108e22e4cd UTC2026-10-11T00:27:15.871775+00:00→2026-10-11T00:27:16.499097+00:00
plugin-typecheck-final exit0 SHAae47736bb2a9401aa9743893f2f64b9a605e65a99d78cd991a8021e0f7722d87 UTC2026-10-11T00:27:36.386664+00:00→2026-10-11T00:27:37.475411+00:00
private-format-check exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20 UTC2026-10-11T00:27:37.475855+00:00→2026-10-11T00:27:37.648546+00:00
fmt-final exit0 SHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 UTC2026-10-11T00:27:15.869524+00:00→2026-10-11T00:27:19.517901+00:00
strict-final exit0 SHA806fb6e9463f0390603d6c8d26c378f4c7986994679ac2a1463b67bfaffc4157 UTC2026-10-11T00:27:15.869428+00:00→2026-10-11T00:27:16.858265+00:00
named-builtin exit0 SHAf3b9881cc15316e7fbf58ec9a697bec5ec607ea5b0f8e4698b1e20172196f87f UTC2026-10-11T00:27:27.141054+00:00→2026-10-11T00:27:27.826958+00:00
named-loaded_bytes exit0 SHAcb332acd4ec502b444cae9f3f038849ecb456fa2c450a91ebd94050abd25c2f7 UTC2026-10-11T00:27:27.827358+00:00→2026-10-11T00:27:29.852610+00:00
named-dependency exit0 SHA44b2bd15b3803e63d4d209d09bbb9c0cfd56c1db4395316b2312cd7efe72e8e8 UTC2026-10-11T00:27:29.852891+00:00→2026-10-11T00:27:30.494443+00:00
named-taint exit0 SHA085efa9588c84058ba5a57dfe203f4083ca39a28ef8f74c9a094eab4c01be5fb UTC2026-10-11T00:27:30.494813+00:00→2026-10-11T00:27:33.532359+00:00
named-generation exit0 SHA35a59bd1776e8b990852609a3a2feac4e39ce406deed409bba67739972bb2e98 UTC2026-10-11T00:27:33.532763+00:00→2026-10-11T00:27:34.229925+00:00
named-mutation exit0 SHA2107af21a3f5805ce47ea9619f234a2ce5937aae0479c737d6af18c61153b321 UTC2026-10-11T00:27:34.230336+00:00→2026-10-11T00:27:35.674009+00:00
named-ordinary exit0 SHA512d068a566f4909469a81c241146304032a168442cae8ad2461338fe225da7a UTC2026-10-11T00:27:35.674416+00:00→2026-10-11T00:27:36.386362+00:00
currentSourceA取材补集 runtime-modules.ts/runtime.ts/cli build86–102/index21完整复制供Claude实际语义审查；可提供模块与generatedloaders不成为准入。8originalcurrent owner受控改动之外全部8745 fixedsource/modes MATCH，NPR01 private4及NPR03 issuer/init/signaturetest/fixture4未改，README本卡顺序承接。Libra仅root2plan无ER13新fulltrigger，旧FASTfull9731只a8实证，最终NPR20真实full mandatory。NPR03 signed0a84ac2 remoteMATCH，当前NPR04 singleClaude/sourcePush尚待；C/D待NPR08/20，最终compiledrecipe/build/runtime/actualwitness/issueradmission仍须实证，SourceA/native/source-readyGO无。basic release38096699927全8jobs PASS/GitHubv0.30.43已发布；exactinstallerR1网络verify exit1保留，R2 actualexit0/installbinary0.30.43/SHA3918ea5d1fcadf181b3b983ef972b7c27bca1c06e9e3187db80db095b43f0fb4与signedmanifestdarwinarm64size77660800 MATCH。FASTcard正式C/D及member继承下一metadata收口，不提前改whole4/61=6.56%。

2026-10-11 NPR04 actualClaude R1 CLOSED literalPASS：0P0/0P1/2P2/4P3、85turns；UTC2026-10-11T00:37:47.579697+00:00→2026-10-11T00:44:44.216082+00:00，rawSHA6afd30486f539ee7abcf166078289748b0a415c6b6a7d6c3cf7730cfe92336b4，review自身只读、未运行命令/算hash，不冒称review独立重跑。7AC/7VER具名门有实际非零结果与97ordinary PASS，本source冻结private12逐SHA/mode复核后保持；PASS后仅root本卡状态/事实补记。四source audit supplement已实际语义审查：受控plugin两node依赖不准入host-effect/plugin virtual modules；generated loader/Bun.Glob build provenance仍属NPR17/08后继硬门，不放宽SourceA。
DEFER-NPR-PLUGIN-POLICY-01（计划外UNFIXED，用户统一延后，owner本root后续独立卡）：P2 capture摘要与util/hash的cache-hit及actualsupervisor→snapshot连通缺少断言；均failclosed，不把它们当已修复，最终NPR08实际healthy/native准入必须证明完整链再放行。P3 configure失败须重启/一次taint后ordinary重求值/status.pending合并状态/proc-macro-error2 future-incompat按原事实保留，不扩大本卡实施。root42package check真实exit2/3successful/30scheduled保留为DEFER-NPR-TOOLING-01，不是全仓PASS。
本source仅plan02/status signed-DCO main交付记录，private artifact/actualnativepositive/模型调用/签发/archive/source-readyGO未由此执行。下一FASTOG01正式基础C/D metadata收口→NPR05 actual stream witness；formalwhole暂4/61=6.56%，NPR04 family完整C/D待NPR08/20，最终full mandatory。

### FAST-OG-01 actual C/D 收口：2026-10-11 00:48:15 UTC
本段为当前事实，此前release/install/website PENDING和旧4/61计数是对应历史snapshot，不倒改。实际source a8b4e15faa67adbde74d6f210756161bada20038、parent341e19d743373fca0c7f1716a9fe9f931cf921c5、vault signed+DCO一次/ordinary main Push；14个member source phases/40 committed path inventory及final45path相对reviewedRG451781140dd36839f8a0df8dbd9fab0f6dc54f35逐SHA/mode核验，unknown0，所有actual闭合Claude链与签名DCO均存在；后续NPR02 Rust delta不是本次v43/fullsource，绝不借旧full覆盖新源。TAG v0.30.43对象6bcf473ffc2da4818b28ec28214fedb49d341639指a8，actualGoodPGP、tagPush0。
C实际full9731/9731、14slow/1leaky/10skipped/no retries，SHA49549f988356a0c471d7ea3031bc47f139a26f3aea0471dafee7aa68f7577d82；唯一leaky settings::tests::config_device_type_is_rejected_without_opening_it保留。strictalltargets/allfeatures-Dwarnings exit0 SHA3c63d804faf394e8648b81703f010171ad2d00138032773ae8904d3fbdbd8e38，fmt0，releasebuild0 SHA cc6a22137e5cc66c6e8259a3bc365bfd8164c5ca7d3ad0fd8ca51c50376291c3。singleactualClaude PASS0P0/0P1/1P2/2P3原rawSHAb73ff7490454a8a65e857b6f9091f5c3eddab335acb8c718d35e4973d121f9b7；doc旧预算DEFER保持UNFIXED，default release deadcodewarning不伪零warning。
D userdirectmain取代PRevent：精确a8 CheckBuildTest38092386992八jobs success、CodeQL38092387001 Actions/Rust success、CodeQuality38092386986 success。实际BuildandReleaseR2 run38096699927/head a8/refv0.30.43八jobs均success：Linuxamd64/arm64、darwinarm64、windowsamd64 build/upload、update-homebrew-tap、upload-install-scripts、request-stable-manifest、verify-homebrew-formula。Linux安全/发行CI为实跑，不冒称本机Linux功能验收。workflow成功后GitHub稳定release真实API创建exit0/verify-tag，draftfalse/prereleasefalse，https://github.com/libra-tools/libra/releases/tag/v0.30.43；broker已正常签发，未使用unsigned/pin bypass或失败流程人工替代。
实际stable manifest version0.30.43/control_revision150/keylibra-release-1/published2026-10-11T00:26:41Z，rawSHA67311bafba5290090700e8b354c19ab016c327c03ca6fb3e297e304e30436b31。manifest不含commit字段：source绑定actualrun/tag，四artifact SHA/size另逐一与精确run38096699927 smallhomebrew-sha256 artifact对拍，四组MATCH：linuxamd64 4a98e90157cec79ddef3c62968f159ef8543192fbe6a232a568f6bc54dbbb20c/96276800；linuxarm64 6f3cb8e6006172315ee78ead6851421880b707d05369fd6aecdbcbe2da43b3c0/80511896；darwinarm64 3918ea5d1fcadf181b3b983ef972b7c27bca1c06e9e3187db80db095b43f0fb4/77660800；windowsamd64 8d56ac4b95b6c24174493eeeb3d8dc049442a9ef55adea72be4e41eaa196b084/113499648。
用户exactinstaller `curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh` actualR1 networkverify exit1保留SHA10d2e24f3363fd342c946695102bdf2ccc01a511195fd1c21799851e0811fcc8；同exactcommand R2 exit0 UTC00:34:42.879839→00:35:22.306828，SHA c0d0d4093551d92c30a98b0c4f340d1077ffc2a01d4c529bb69cb8a3278c8de8；/Users/eli/.libra/bin/libra actual0.30.43/SHA3918.../size77660800与signedmanifestMATCH、officialinstallmarker写入。全局OpenCode保持2.0.26、不替换、不触碰既有进程。
网站仅isolatedlatestcf a0031852d83478a4971f327fb6bf5ef3b8eceab6＋current已审EN三个slice/11281bytes，outsidebytes保持；signedexact1filecf64e0a48b52f80693a24a71025eb72818b12ae3d5 ordinaryPush0，sourceEN SHA9e2e4670575861c96b89d8264b853014a0807cacb2607ae256c958aad1fc28f3。typecheck/build/deploy--keep-vars exit0，actualWorker f7e0569b-593a-4074-9a82-2743751e09ca；actualTLSHTTPS200/livebodySHA0cedf32a0637a264e998f3c39d6315bc540726eff8a788330ad2832ed8a84bfc，2.0.26/pinned parser/30s/45s/legacy containment五正文断言全PASS。backendroot agent/cloud/hooks/op四dirty完全保留。
11已审member OG-00,OG-01,OG-02,OG-03,OG-09,OG-11,OG-14,OG-07,OG-13,OG-18,OG-04据真实finalsource/site及C/D继承done/complete；FASTOG01 AC4/4 VER4/4 done/complete。OG15与FIXOGCP01仅已审源码阶段交付，stock2.0.26 BOOT三真实失败仍DEFER-OG-OPENCODE-BOOT-01/UNFIXED，原Mac real-content非PASS、Linux原具名UNRUN后验、0OpenCode模型calls；二卡deferred/not-applicable不新增卡、不计完成。固定61队列重新按实际cardtext核对formal16/61=26.23%，45未完成（包含deferred，不能称全部完成），basis4既有complete＋本次11member＋1stage。NPR00/01/02/03/04只source/localaccepted不继承NPR08/20 C/D。下一NPR05→06→07→13→16→17→08→15→09→10→11→12→18→20；NPR04 signedmain90f2e36a3905501c989c192def4790f45de9ec9a/singleClaudePASS为前驱source。SourceA仍未获真实native正例，未知partial/zeroartifact；finalfamilyfull/security/fourplatform/privateinstall仍必须实际过门。
本次只四plan事实/状态回写，无version/source变化、无新tag/release/install需要，既有actualstageClaudePASS/sourcefull保持；每push必要fmt/strict重新记录，protectedplan05SHAa705...及所有runtime/version/docs frozenbytes保持。

FASTOG01纯metadata收口commit前本次实际gate：fmt-final exit0 SHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 UTC2026-10-11T00:48:36.294594+00:00→2026-10-11T00:48:39.562598+00:00；strict-final exit0 SHA07a496808183251004771526f94c0946b989f3275d48ae2470bd06fd2ceaac60 UTC2026-10-11T00:48:36.294593+00:00→2026-10-11T00:48:42.106399+00:00；无root源码/version修改，v43 actualsource/full/Claude/C-D与后来NPR02新Rust最终full分开绑定，PGP/DCO/mainPush随后实际取证。

2026-10-11 00:52:03 UTC NPR05 actualimplementation开始：唯一AI source writer、root为唯一plan/README owner；NPR04 actualsigned90f2e36/PASS与FASTOG01 metadata signededc32cad/16正式complete已核；本卡原九production加单test、全部新门UNRUN。weak候选End须whole clean＋opaquefence，未知/SDK/directstep/WS/替换transport零proof，不以新设计证明native。NPR06只读取材确认step.ts真实Exit必须最小扩入后继写集，按用户推荐自动采用但本卡不越写。formal16/61=26.23%，actualC/D仍由NPR08/20，下一NPR06。

2026-10-11 NPR05 current2.0.26 source candidate：AI9production+test10及顺序CoreREADME；compiled route→canonical caller request/resolved request→HTTP prepare→actual executor/underlyingHttpClient→response/frames→actual protocol step/onHalt原eventWeakMap。默认NativeOrigin.Port缺失，无生产authority；AI不import Core/Session/key/signer。SDK metadata/direct step/合成event/compaction/同名ID不是来源。OpenAIwrapper保留wrapper身份，初选WS/fallback与换回transport sticky拒绝。candidate只是原End布尔排队提示，read/describe须whole clean success、actual final UTF8 bytes/fragment/version/metadata/opaque fence仍一致，失败/取消/incomplete/后续覆盖不证明完成。
actualURL用Effect Url.make(url,urlParams,hash)，query/hash原地修改失效；actualEffect Web response headers/stream getter每次新对象，检查source/accessor与header标量并实跑fromWeb对象。NPR16必须同源recipe+NativePolicy封闭actualobject链、request headers/body不可变与未知HTTP transforms，并在真正Fetch层禁止自动redirect；不能靠初始URL/HTTP名字/可调用register或permissivecallback授权。NPR06/16保留原End/request/part/finalbytes，step.ts actualwholeExit成功后同一Ended text/state/proof事务，失败无proof ordinaryflush，零proof-onlyevent/replay重签。
8exactnamed各1PASS0FAIL，combined147assert；637existingoffline/1818assert，AI两套tsgo/ownedPrettier/READMEcheck0。当前source根canonicalcheck实际36successful/36total/0cached exit0；完整frozenignore-scripts安装保持package+lock，旧DEFER-NPR-TOOLING01 FAIL保留历史，本轮current源码已恢复，不把scratchbaseline当current gate。Libra nightlyfmt与alltargets/allfeatures Clippy-Dwarnings0。firstiterations和wrongroot typecheck-third127保持真FAIL。
gate-native_fields exit0 SHA35ec18594ae6221916a7a9098e82bfa3f3063f677e76a1bca6ee3b8070525eec
gate-sdk exit0 SHA2f6a14e2e66069d8516bdbcb6b13977eb6b03e0d0d71190c640d11373f5d9d03
gate-direct_step exit0 SHA1862bf6cf4bc6ee594cdc8f5cd36a547e49fa68392d096adc58dabbd6dd579f9
gate-binding exit0 SHAb94ef9a368691279a6de827759d5ea50114fe893866e30ebb7d280a436ae46c3
gate-transport exit0 SHA44225175c9ba7647fccb6d211e257c85a8abf9943b1e20978968bc53a4241273
gate-completion exit0 SHAe364e2776bbdcede710a8d5e3bfd203fb92f88eda7e3ed6206335ecfc45535c0
gate-ordinary exit0 SHA20381823fbc4034eb0f590d3e570b64e6782000752d02cb0734e5042fc1a3104
gate-wrapped_route exit0 SHAd1c23007ab2e71ec790e2407fdc45f583055aae6a59e90d5e5fb3b2b741e0a36
related-offline exit0 SHA3952c525c8ffd413223d680ffcabd6e0fe5f95ac0710466c099928d7d681a1db
ai-typecheck-final exit0 SHA00b4b616ca9be17a8068364e98d969f1dfbfd4e2333e686be12c6481e584c0e6
format-final exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20
root-check-final exit0 SHA3f75171733edf042c639b893ffd354609f6c4cc77d386583aff993e4a1771572 UTC2026-10-11T01:07:07.700689+00:00→2026-10-11T01:07:24.965611+00:00
fmt-final exit0 SHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 UTC2026-10-11T01:07:07.700462+00:00→2026-10-11T01:07:11.948123+00:00
strict-final exit0 SHAa5caba6552116aa7e4490da350ccc56a0448304bc7c0acdbda17bc9b409ff657 UTC2026-10-11T01:07:07.700599+00:00→2026-10-11T01:07:44.561816+00:00
readme-format-final exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20 UTC2026-10-11T01:13:10.184783+00:00→2026-10-11T01:13:10.233609+00:00
8745official source/modes除NPR04原8+NPR05原8共16owner外MATCH，NPR01/03/04private前驱除顺序README均未改，无其他originaldelta；actualClaude与本卡signedsourcePush尚待，C/D待NPR08/20，SourceA/native/source-readyGO未证明，0OpenCodeprovider模型调用。positive本地frames明确synthetic，不真实paid/free模型/endpointadmission。Libra只有root2plan，本卡无新ER13 fulltrigger；旧FASTfull9731只a8，最终NPR20newRust全量与最终计划全量强制。正式whole16/61=26.23%，next NPR06 necessary step.ts fifthproduction已采用，不形成NPR16反向依赖环。

2026-10-11 NPR05 actualClaude R1 CLOSED literalPASS：0P0/0P1/2P2/7P3、96turns；UTC2026-10-11T01:14:48.336352+00:00→2026-10-11T01:29:52.390721+00:00，rawSHA089e60d87313330364fc5f6a2c0c0cdebcb94105c8bcb22969dd0d4fa5ff032e，exit0/is_errorfalse。8AC均PASS、8独立exactnamed各1PASS/147assert、637ordinary/1818assert、AI类型/Prettier/README/root36successful0cached/fmt/strict实际绿。review只读未运行命令/算hash；root1.5MBcheck仅阅读summary+receipt，未读HttpClient/Protocol.make/Lifecycle/Framing全源，不冒称其完整独立运行或未读源审。13frozen source身份核验，PASS后只root本卡状态/事实，不改private11，单Claude门满足、无Codex/父级第二评审。
DEFER-NPR-WITNESS-01（计划外UNFIXED，用户困难外围统一延后，ownerroot后续）：P2 response.request===prepared.request未作为AI硬条件，propertyCheck找不到descriptor时恒真；当前lock/policy默认closed不构成来源bypass，原源码保留未修。NPR16真实许可必须封闭reviewedHttpClient无未知transform、exactresponse/request实际对象/URL、immutableheaders/body/Fetch redirect:error、innermostPort与sameattempt fence；这些既有硬义务不能借本defer降为nativeGO。P3 repeatedapprove成本/complete缺encrypted字段false-negative/Bedrock多字段与AnthropicEnd无metadata须读mergedstate/Anthropicbetaquery与v1endpointgrammar不兼容/event引用生命周期/截断finish维持原严重度，不称已修；仅批准后继recipe模型，不能凭syntheticfixture新增端点。NPR06读取mergedstate并及时释放原End引用、actualclean finish边界；NPR15/16真实model准入仍未运行。
DEFER-NPR-TOOLING-01当前恢复证据：同frozenlock完整ignore-scripts依赖安装与本次patchedsource root36/36/0cached实际exit0；过去timeout/缺Astro/sessionUI/错cwd127仍历史FAIL，既有GUI源未修，8745orig除16authorized原owner外全bytes/modesMATCH。此恢复不是native/artifact/admission或真实模型正例。signed-DCO root2main源码记录随后实际Push，privateartifactC/D待NPR08/20；下一NPR06 actualpart/Ended事务，formal16/61=26.23%不增计，SourceA仍NO-GO/0OpenCodeprovidercalls，Linux用户后验、最终家族newRustfull与最终计划full保留。

2026-10-11 NPR06 current2.0.26 source开工：NPR05 actual单ClaudePASS/signedmain 8e983b237e3a6920ed09cd71083e0188a15083e3 与完整frozen11private handoff MATCH，用户推荐方案自动采用step.ts第五production及相应Step existing9scenario门，landing3/7AC/7VER/family无独立version保持。schema/core-session/core-runner唯一source writer，root唯一README/plan owner；旧双审采用记录是历史，本轮只Claude Code actualPASS。生成partID，保留原End/request/finalbytes，wholeExit后单Ended event/projection事务；默认context absent、无生产签发，NPR16实际接线后序消费不反向依赖。真实SQLiteabort/replay/legacy/newgates全部UNRUN，禁止synthetic当nativeproviderpositive。formal16/61=26.23%，下一NPR07，C/D及finalfull仍NPR08/20。

2026-10-11 NPR06 necessary-write-set自动采用：client既有contract-identity.test.ts仅修无语义part_1 fixture并新增完整A目标；原字节/模式先快照，prod5/landing3/7AC7VER不变。同当前publisher写集补充内部可选同步veto，仅撤销已AI-read/closed receipt，在实际Bus.commit同immediate事务projectors后/sequence及event插入前复核。private精确sentinel才rollback后普通End一次，任何真实DB/rollback错误传播，issuer不重试；NPR16后序接线policy/witness/坐标，无新增授权factory。边界按actualSQL时序，不声称最终COMMIT检查。所有新门待actual执行；native正例/准入仍未证明。

2026-10-11 DEFER-NPR-HTTP-EOF-01（计划外遗留，用户同等难度外围问题延后）：NPR06实际探针发现OpenAI/OpenResponses既有compiled protocol terminal/takeUntil后真实HTTP物理EOF/error不被stream消费者观察；failed flush-final SHA19a0727b5f7ce6d2c92bae7a559250b1c9000d59a2b049cb02f0e42bc198af7e保留，不追认为PASS、不声称socketdrain已实现。当前receipt契约按原compiled semanticwholeconsumer/end，不新增AI scope/physicalEOF要求；本卡改用实际Anthropic message_stop后继续body读取的真实error验证Core wholeExit，以及OpenAI terminal前actualbody失败。最终sourceREADY/真实模型/endpoint/artifact仍待NPR08/16/20，零native正例宣称；后继若增加physicaldrain须独立AI实现/Claude/验收，不阻塞本计划基础采集和producer机制交付。

2026-10-11 NPR06 necessary共享codec兼容前滚：actualprivate rootcheck exit2/5successful36total/0cached，gui-extensions ES2023消费者报native-origin.ts String.isWellFormed TS2550，SHAaf0ca84d7eccbf00c11d459e6943bf2814146aac9d97b6355b7d9ec8de6c1c95保留于root-check-r1-failed，不记PASS。采用NPR06 I必要第六production native-origin.ts用ES2023可类型检查的等价lone-surrogate检测，补既有native-origin.test.ts边界与全codec A，无GUI/config/lib/deps写入，不改变recipe/bytegrammar/domains/caps或字节向量。NPR01该两新文件由本卡前滚并另实审，不再声称这两项unchanged；其余前驱source仍冻结。当前新门与最终rootcheck待实际重跑，正式完成16/61不变。

2026-10-11 NPR06 current2.0.26 source candidate：schema3/Core publisher+updater+step六production及真实SQLite test、既有client contract-identity噪声fixture、既有NPR01 UTF16边界test共9source；共享codec ES2023兼容前滚不改domain/closedgrammar/UTF8bytes/caps或vectors，前驱该两文件本卡重新实际评审。默认nativeReceipt缺失普通输出，无生产authority/SourceA positive。原End/request/finalUTF8bytes与generated partID精确关联，真实wholeExit及clean finish后单Ended text/state/proof事务。显式wrongpart不fallback latest；legacy absent-ID沿用原fold；serialized durable .1 additive，Delta ephemeral，真实Bus.replay不重签。
同事务commit admission veto仅能撤销已AI-read且closed receipt，在projectors后、EventSequence/Event插入前；私有sentinel才回滚后同End无proof一次写入，DB/rollback故障传播，issuer只一次且错误安全可见。此线性化点不是最终COMMIT再检查。generic flush、cancel、latebodyfailure及未知完成不签发；保留普通内容。实际source边界与tests见 /private/tmp/libra-main-resumption-20261010/npr06-main-2.0.26-r1/handoff.md。
七独立exactnamed各1PASS0FAIL；五独立既有affected A非零含完整codec UTF16及frozenvectors，canonical Core/Schema/Client types及owned格式通过。正例provider frames全部synthetic，只证明机制；实际model/endpoint/recipe signer接线归NPR16，发行与准入NPR08/20仍pending。
part_identity-final-r5 exit0 SHA7d69b95ab68a0c9f8159520b4365730ae4c7ee40ed89e6fdcd85bd5e9c0064e3
association-final-r5 exit0 SHAeafaa6a6749733e484fc209c00431ebdfd57deb3f4b3ef93443324d38166dce5
legacy-final-r5 exit0 SHA5a48f4ae38e7a6f8dab32e4f5279c27abe08e49044c5a519b70d4eba4e40fe84
atomic-final-r5 exit0 SHA27d37044c627a5ec5f6c4a68775b351ef033433f53f808781e842e7690c54483
replay-final-r5 exit0 SHA45c1fc188985424ed80182b72fd4072bd6fe5282760166398194af66f0ba0c85
flush-final-r5 exit0 SHA66178db726dfdede5022d20cdac744e2914c8ff4046c342ba85c88b56d844d32
overlap-final-r5 exit0 SHA40eb59ea40335620f05beca28103b8b68ed6226547ac727ffc2e739999b55aec
schema-manifest-final-r5 exit0 SHA0591ed8dec99efeef92b4b8f81c69a264e7443361d406ec0762496a7680da481
core-message-final-r5 exit0 SHA42edbe4774c0ea730fcfccbda633938efd479533f9858510d591113e398f6c0a
core-step-subset-final-r5 exit0 SHAc4a040e0f4cee679a4b39fae3e48355de537606fb2b8e5a6fe708d392e7a6dd1
client-identity-final-r5 exit0 SHA30264e7354d0921b6720cca3b19a926f0501c5ceaaacde4b9dffe0b48747ea41
schema-codec-final-r5 exit0 SHA48cf8427a24976249e30d22126b9ea56b0ff6f76dfcd99962699dbfcad17557b
schema-typecheck-final-r5 exit0 SHAae47736bb2a9401aa9743893f2f64b9a605e65a99d78cd991a8021e0f7722d87
core-typecheck-final-r5 exit0 SHA8f0c4bff8dfe0ad0cea81fc8587f22596a0ee3fb280e4af824e688108e22e4cd
client-typecheck-final-r5 exit0 SHAae47736bb2a9401aa9743893f2f64b9a605e65a99d78cd991a8021e0f7722d87
own-format-final-r5 exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20
root-check-final exit0 SHA584aec59fe4c28cbc1394875f197a713ebba5614e73a22c1c5b06a34c0456b75 UTC2026-10-11T02:23:46.300210+00:00→2026-10-11T02:24:00.317813+00:00
fmt-final exit0 SHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 UTC2026-10-11T01:33:55.056649+00:00→2026-10-11T01:33:58.126851+00:00
strict-final exit0 SHA3890c47249dc62f9b1617d2a16f6207acda16deb7ba62c0df5221d53097fd028 UTC2026-10-11T01:33:55.056856+00:00→2026-10-11T01:33:56.362381+00:00
readme-format-final exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20 UTC2026-10-11T02:23:46.300311+00:00→2026-10-11T02:23:46.377448+00:00
8745official source/modes仅22已授权original变化，前驱private source除顺序README及NPR01本卡两owned UTF16前滚外全MATCH；零未知非build新增。root-check-r1-failed真实exit2/TS2550保留，新rootcheck只实际最终日志计PASS。actualClaude与本卡signedsourcePush待执行，正式whole16/61=26.23%不增加，next NPR07，最终NPR20及计划全量保留。

2026-10-11 NPR06 actual single Claude Code R1 PASS：rawSHA29b984703710f91cc6ee68638e99c8a5013f7ce2e70f52435d6b75baa3cb2d84，UTC2026-10-11T02:24:30.056060+00:00→2026-10-11T02:40:00.043771+00:00；P0=0/P1=0/P2=4/P3=5，7AC全部source/localPASS。本卡不新增Codex正式评审。DEFER-NPR06-REVIEW-R1计划外遗留：4P2为nativecontext启用后的UI/livefold未验收、issuer错误durable stop与runtime failure不一致、foreign旧id兼容说明、同步veto要求bounded/nonblocking；5P3为flush缺陷detach后候选不可重试、issuer/codec UTF16谓词复用、显式ID缺assistant fail-loud文档、text重复End无completedguard、transactionhook内部状态变量。全部按用户外围延后策略留UNFIXED不堵本卡，SourceA和后继正式运行门不据此放行。具体file/line/最小方向保留actualClaude原文 /private/tmp/libra-main-resumption-20261010/npr06-main-2.0.26-r1/implementation-review-r1/claude-review.md。 source10private最终SHA/modes未变；Root only2plan metadata据实际结论改7AC7VER为通过，family C/D pending，不冒称完整Acceptance/native SourceA。rootcheck历史TS2550 exit2及postterminalphysicaldrain failed保持FAIL，最终root36/36实际exit0；NPR01等价UTF16前滚及vectors复核已在本次实际Claude覆盖。Signed-DCO main sourcePush下一步待执行；formal16/61=26.23%，下一NPR07 rawtransfer/sanitize。

2026-10-11 NPR07 current2.0.26 source开工：消费NPR06 actual单ClaudePASS/signedmain 21dedfa5c415a41d79b57ab6abcf0810c7c06008 与最终frozen10private，source-implementation qualified顺序，C/D仍家族点。单production transfer/唯一newtest，root唯一README/plans；prepared6named草稿未验收，只actualapply→actualgates准入。raw保持原坐标/签名/decodedUTF8字段，sanitize全reasoning省略proof保留id且零持久修改，legacy无proof/ID不造属性；不新增factory/许可/签发，无keys/models。四existingA完整独立source门及canonicaltypes/rootcheck/fmt/strict强制，全部新门UNRUN，formal16/61=26.23%，nextNPR13。

2026-10-11 NPR07 current2.0.26 source candidate：transfer单production/六exactnew真实SQLite import/export/sanitize门，四existingA独立完整target非零；schema字段及原receipt保持NPR06最终frozen。Raw保留decodedUTF8原bytes、receipt/signature及原source坐标，零repair/重签/授权；sanitize全部reasoning实际省略originProof/保持id/time/现redaction，零持久修改；legacy缺属性不backfill。真实mismatch保持原proof只证明不追认机制，后继NPR11/12验签准入未运行。synthetic allzero无效签名为数据fixture非native正例。

raw-export-r2 exit0 SHA6810582600d7f26fa56a067a66d2edd85154da1ad52341a06042c9cafee38400
import-r2 exit0 SHA2ee2b649a59df0befcbba90804680ceea47a8eecfc6b8a60f482b49be6107564
sanitize-r2 exit0 SHAa3e03bf0bbe6693c04610f6415be8df62e9a1294539414c6543e334f976f884e
coordinates-r1 exit0 SHA8f13529ba54e81d49c59ac08d7a86ba20a2d1d3cd1230d4f496672135e1c4026
legacy-r1 exit0 SHA693ccd9939ed1ba35af0f27b6eba604ec23f75ac477a4e6b528cbedfbd589a10
state-r1 exit0 SHA1c52e9bbbcfbf2d6cdd0b13ddde881d9e7bff07dcf12109222ef54d9e3808b5f
core-session-create-r1 exit0 SHAc34044a392374b5245188d63469cc2f494bb963e237dc7ce9b1b7ccee69f5df6
server-session-import-r1 exit0 SHA3ef9dc58000aa545703fd38690ef17f13e33413c3ed60a58448e586351396596
server-message-r1 exit0 SHA7794a3c84a1f6473e50f6a245f70430a0b9631454363b2450791cbaa5b3ce937
cli-import-export-r1 exit0 SHA0165b1b269b0cdbf5adc71a4c72535c4c081d4eeabb795a800ca7630ff002e5a
core-types-r2 exit0 SHA8f0c4bff8dfe0ad0cea81fc8587f22596a0ee3fb280e4af824e688108e22e4cd
transfer-prettier-r1 exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20
transfer-test-prettier-r1 exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20
root-check-final exit0 SHA243adb2560e662bed880340a47c5cd869f0ac2a663a0c2b5c7b8c79ff8be5a8c UTC2026-10-11T02:55:07.769946+00:00→2026-10-11T02:55:13.641864+00:00
fmt-final exit0 SHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 UTC2026-10-11T02:47:47.671921+00:00→2026-10-11T02:47:51.157159+00:00
strict-final exit0 SHAc554ca9a44e128a04f3c2efe3d07184451b4c2b1524af60de717d77f88f60dca UTC2026-10-11T02:47:47.671933+00:00→2026-10-11T02:48:33.271483+00:00
readme-format-final exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20 UTC2026-10-11T02:55:07.770040+00:00→2026-10-11T02:55:07.852986+00:00
8745originalbytes/modes除23owner变化外MATCH，前驱最新actualreviewed source全MATCH除顺序README，零未知新增非build文件；actualClaude及sourcePush待执行，familyC/D未完成、SourceA未证明，formal16/61=26.23%，nextNPR13。

2026-10-11 NPR07 actual single Claude Code R1 PASS：rawSHAf632e113cebe41b4c7b05538542f215039992b76ac0d8cc12f442fe013e9e54e，UTC2026-10-11T02:57:02.849366+00:00→2026-10-11T03:04:22.777074+00:00；P0=0/P1=0/P2=2/P3=3，6AC全部source/localPASS。DEFER-NPR07-REVIEW-R1计划外外围遗留：P2-1 README把Effect pattern/schema边界过称strict decodeReceipt全cap/order/excess rejection（transfer保留serializeddata不授SourceA；应区分pattern校验和后继严格验签），P2-2 sanitize timestamp保留未显式fixture断言；P3-1 in-memory nonmutation仅由copy实现/持久后读间接证明，P3-2 fixture digest同helper仅是保真不是独立hash正确性，P3-3 root evidence应补cached29当前事实。按用户策略延后P2/P3，不放宽SourceA/receipt grammar/后继verification；raw日志和具体findings保存actualclaude-review.md，未运行的后继仍UNRUN。 当前rootcheck实际36successful/36total、29cached，非zero-cached；三private最终SHA/modes不变，六真实SQLite/Store transfer门和四完整existing A门非零通过；raw原字段receipt/signature/source坐标逐byte保持，不repair/重签，sanitize全reasoning省略proof且不修改persisted raw，legacy不backfill。本卡synthetic allzero无效签名仅保真机械fixture，NPR11/12真实verification仍UNRUN，SourceA未证明。root only2metadata，family C/D待NPR08/20，signed-DCO main sourcePush待执行；formal16/61=26.23%，下一NPR13 actualcanonical generatedclient。

2026-10-11 NPR13 current2.0.26 source开工：消费NPR07 actualsingleClaudePASS/signedmain 7b4e1d64fcbf8e363af04abf4a12a5415d47ac9b及最终3privatefrozen；父级root唯一README/plans/VCS，client generator/test唯一sourcewriter。I11全8TS+3manifest已snapshot，newtest absent。isolated priorproposal actualinstall/generate0/2delta但前07inputstale仅参考，当前actualcanonicalgeneration/四new/五完整existingA/types/importbrowser/rootcheck/fmt/strict全部UNRUN；不手写generated或新增Core/Server host依赖、不造第二receiptfactory。NPR13实际生成candidate11expected须实审固定SHAinline，NPR08重生成前已固定核对而不是freshlearn。正式16/61=26.23%，SourceA未证明/C-D待08/20，nextNPR16。

2026-10-11 NPR13 necessaryI正式采用：额外3既有Client测试在编辑前逐byte/mode snapshot及官方固定source SHA MATCH；只修正external/resume、decodedDateTime idle和零Protocol eager断言，保留exact请求/响应及Core/Server禁止依赖。prod11/landing3/AC4/VER4不变；3完整A初轮FAIL保留，修正后实际独立重跑、actualClaude与signedmain仍待执行。SourceA未证明，formal16/61=26.23%，未来bootstrap实际接线必要拆卡另由父级登记，不隐藏在NPR08 release写集。

2026-10-11 DEFER-NPR13-LEGACY-TEST-TYPES-01 计划外遗留：父级临时追加extra tsgo覆盖三完整旧runtime fixtures实际exit2（20783bytes诊断），plain-fetch Bun preconnect、宽fixture/malformed SSE与旧Effect输入等官方既有typing问题；不冒称PASS。既定额外门仅newtest+api.types实际通过，canonicalClientsrc types通过，三旧test完整runtime门已实际绿，未运行Standaloneofficialbaseline不称已复现。按用户同难度外围延后策略停止扩大types修补，invalid fixtures/exactassertions不削弱、不tsignore、不改production。全部FAIL证据保留。formal16/61=26.23%，SourceA仍UNPROVEN，actualClaude尚未执行。

2026-10-11 NPR13 current2.0.26 source candidate：actualcanonicalgenerator双emit复核11registeredregular（8TS/3manifest）bytes/modes，只有2生成输出delta；4exact new publicPromise/Effect实际HTTP/SSE/encoding/decoding门，五完整existing A独立非零通过。necessaryI三旧tests officialSHA编辑前MATCH，修正existingexternal/resume/file.write、decodedDateTime和零serviceProtocol eager，零production/API/deps改变，全部原FAIL保留。候选expected11JSON SHA311aeeb67465d5d389640076729574fbd61068fc552b116a1d84ae11e0237804为本次actualClaude必须评审的UNREVIEWED固定pin；PASS后只复制其exactbytes最终路径，NPR08先核对既定pin再重生成，禁止freshlearn。Synthetic allzero无效签名只证明browser wire保真，不授native/SourceA权限。

generate-r2 exit0 SHA2fc2d35edaa31d0d4e4f12aa09b3f1ac45d14c0f9043e227a4917d6ebb7499d5
optional-r2 exit0 SHAec3fe9842c1be17d1833420604d1dd1d906965515e75c1039de8f059830b1ae0
roundtrip-r2 exit0 SHA458e9876955e760f8c2e5afa25a2e6773c707f78e03b4278a72e636b0fd4d3b3
legacy-r2 exit0 SHA097afc95f122e21931ea80f5484c2de78cd7bd4713fffaceb6bdc8420be47f2e
existing-promise-r3 exit0 SHA06133aa5da3adedf9917a0a1b28e54a68be3c88d59d83e0fbec898e473602b3e
existing-effect-r2 exit0 SHA7c540daaf9c2c261ece70e6c49f095dfccf7f5e32a57a076d4f081b2ab45129f
existing-contract-identity-r1 exit0 SHAb3a33ecc0bf2071b0826d4bd92fcd3b367d0920d9f5c6128f723b99394059e9c
existing-import-boundaries-r2 exit0 SHAe5d38fb4c9c8dbdc5553c04a8a184aba388ac47b5a56b4579894cda287884e80
existing-shared-events-r1 exit0 SHAd200b8367f098c5eee1707d024b4de85f71dfd63ffdd7b2e92a389cb34ed0c41
client-types-r1 exit0 SHAae47736bb2a9401aa9743893f2f64b9a605e65a99d78cd991a8021e0f7722d87
test-api-types-r2 exit0 SHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
owned-format-r3 exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20
root-check-final exit0 SHA7a5572d2127043e0eb148a5d7b99f98b758672609504da0906a712cf4a27a0c6 UTC2026-10-11T03:29:24.344603+00:00→2026-10-11T03:29:38.568722+00:00
fmt-final exit0 SHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 UTC2026-10-11T03:15:27.201407+00:00→2026-10-11T03:15:30.853977+00:00
strict-final exit0 SHAd278ed5f132d7fa68b439e6596c955712db9b63357172e2f622655b496fdb5eb UTC2026-10-11T03:15:27.201410+00:00→2026-10-11T03:16:09.835027+00:00
readme-format-final exit0 SHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20 UTC2026-10-11T03:23:42.957065+00:00→2026-10-11T03:23:43.018983+00:00
8745originalbytes/modes除28owner变化外MATCH，前驱最新actualreviewed source除本卡registered生成/test和顺序README外全MATCH，零未知新增非build文件；actualClaude及sourcePush待执行，familyC/D未完成、SourceA未证明，formal16/61=26.23%，nextNPRBOOT01再16。

2026-10-11 NPR13 actual single Claude Code R1 PASS：rawSHA773888052b2298e1b3d90dad5d8e57d6b191f08013203f9088104524c7284128，UTC2026-10-11T03:31:04.329163+00:00→2026-10-11T03:38:49.129646+00:00；P0=0/P1=0/P2=2/P3=6，4AC全部source/localPASS。DEFER-NPR13-REVIEW-R1计划外遗留：P2-1生成门固定当前session证据prefix且generation变化时不finally恢复；P2-2 malformed-proof客户端Effect负例与Promise原样对照未覆盖。P3六项为UTF8更多切点、expected/gate序列按path比对、card旧UNRUN和wrapper记载、README中间编辑记录、未随bundle提供nestedgenerationreceipts、parentrunner环境记录不完整。具体file/line/最小修复方向保留actualClaude原文；P2/P3按用户外围延后，不替代后继真实gate或授native权限。原DEFER-NPR13-LEGACY-TEST-TYPES-01实际FAIL保持。P3状态/命令事实本次metadata更正，其他证据/测试限制如实保留。 currentrootcheck实际36successful/36total，Cached:    0 cached, 36 total；15owned及README最终SHA/modes不变；4new202assert及五完整existingA68tests/257assert实际green，新test+api.types及canonicalClientsrc types实际通过。额外oldfixturetypes实际FAIL/DEFER保持，不隐瞒。actualreviewed固定expected11JSON SHA311aeeb67465d5d389640076729574fbd61068fc552b116a1d84ae11e0237804已exactcopy最终路径/private/tmp/libra-opencode-native-producer-evidence/NPR-13-generated-expected.json，未从fresh08生成学习expected；08comparator/独立比较仍UNRUN。Synthetic无效签名只证wire保真，SourceA未证明，C/D待08/20，signedmain待执行；formal16/61=26.23%，下一NPRBOOT01实际启动接线后16。


### 2026-10-11 03:53:43 UTC — NPR-BOOT-01 G09原位登记及唯一源码写者起点
依据actual current2.0.26源码及已停止只读草案，采用六production/四落点/三newtests/两README，七独立AC/VER。原NPR16明确拆出BOOT，保留其Session attempt主轴并必要加入policy.ts第六owner；NPR17放弃不存在的planned handler，改用BOOT Server窄facade而CLI仍不直依Core。依赖为NPR13 actualsingleClaudePASS/最终15源摘要和signedmain ba028fec57d27fa8364720e4b1e02f94822708ce；当前固定expected SHA311aeeb67465d5d389640076729574fbd61068fc552b116a1d84ae11e0237804开工核验。实际顺序13→BOOT→16→17→08；同源码/docs写集严格串行，父级独占root metadata/README协调/VCS，private sole sourcewriter随后指定。新compiledprofile、三selector、sameprocess preboot memoization和明确graph边界已登记，全部新门UNRUN；不从envJSON/label/descriptor扩recipe许可，不implicit key init，不调用模型。BOOT T1由唯一private release08 T2承载，source children public C/D由20承载，只消费Deliverables，无Acceptance环；08保持pure release/prod0。公共ownership appendix可增加build/Server载体，不修改原v1wire/domains/caps/endpoint grammar或新工厂。04/24现有DEP/RG/archive镜像本次无状态/owner变化，不改二者；05遗留改动精确SHA不变。原61cohort正式16/61=26.23%，新增BOOT必要子卡不改变原正式分母或授完整原NPR16完成。本次登记不是actualClaudePASS/nativeGO；NPR08真实binary/full与NPR20/最终full待执行，Linux功能由用户后验。


2026-10-11 04:02:49 UTC NPR-BOOT-01当前公共输入实际测量：fresh独占isolatedHOME/XDG/TMP目录，actualLibra init与agent enable --agent opencode两命令exit0，从实际安装输出取得12项event BTreeMap及30000deadline；当前settings模板13600bytes SHA9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d，event-map原bytes SHA85977fd993350c87dde2cd8cafc4473d7f86309f362a397def7cd16efbcd5dc4、封闭canonicaleventMapDigest3fe0008b685e48e61d2f63a339de2e3510d9c9719dc572794afc6c95ef7d2ebe。将actualrendered完整bytes与当前原模板+actualcommand JSON+actualmap+deadline重构逐byte相等，非仅sourcegrep。测量observerbinary SHA0b6c5a88cba27cb51b37aa914cdb88e6f2ed61aae9096269fcdc414c8e978161仅本次真实owner工作证据，不编译到publicpolicy或假称producerbinary。public-template-inputs JSON SHA93f78f48873ca65ff23326e83e54fcb14e08cbc36b8c85af660f454835d4bfec不含私有绝对observerpath/rendered digest。realprivate publicbuildnonce bac21cc4027896b8d1836133c347d20f由OS CSPRNG16bytes固定于source review/build之前，不是source/binarydigest，不授native权限；twooriginalclosedOpenAI Responses endpoints的独立canonical前像/digests已保存并将对照实际codec。builtin实际id/sourceclosure及BOOT七newgate仍UNRUN，SourceA未证明；未初始化issuerkey/未调用provider-model/未操作globalOpenCode，formal16/61=26.23%。rootfmt实际exit0 UTC2026-10-11T04:00:07.953705+00:00→2026-10-11T04:00:11.388616+00:00，logSHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855；strictclippy实际exit0 UTC2026-10-11T04:00:07.953724+00:00→2026-10-11T04:00:52.430600+00:00，logSHAff0b2ad07f261f02049f6229b07a90326189876195aa9addcfb76356a817804a。proc-macro-error2 futureincompat notice按raw保留，不是本卡lintfailure；parentrunner继承env不称allowlisted（未输出env或credential），子test统一必须过滤isolated。


2026-10-11 04:09:27 UTC NPR-BOOT-01 fixedpublic sourcecandidate正式采用：actual原始模块导入展开pre69/post17共86uniqueIDs/82concrete declaration owners，独立TypeScript AST解析及父级读取actualrawbytes/size/modes全部86/86 MATCH；activationorder不作policy摘要顺序，按规范rawUTF8排序/BEu32+LP+sourceSHA构成5160byte前像，builtinSetDigest 137c5c7b18c756b75e520a6ffdd042397b5c29f917bac383084df5c664dd0e82。worker原activationorderdigest1f7848dae5fb2112db202366ed9adcd9a14e3544650cb062de4cf3634eabd29e只保留measurement历史，不进最终compiledprofile；r1/r2解析失败原日志仍FAIL。本次frozen固定publicrecipe11keys SHAa15654acce3946f8105e433e0c2f83ba6ab3f4031bb68e3e27352f9cd7c01632、86项mapJSON SHAd57ed674b87accef65ca302595a95fe29b5980bac331eb6e1cd9de3c25f97ed8，结合已实际render的模板/eventmap/期限和预先CSPRNGbuildnonce，无私有observerpath或rendereddigest。只作为source阶段待actualClaude评审的compiled literal候选，不通过externalJSON/manifest/runtime label取得权限；ID/source候选不是actualloadedroot/object闭合（16仍UNRUN），55固定providerimportlambdas另有机械表，不被名字扩为新endpoint许可，最终完整transformed/native/dependency/binaryclosure08仍UNRUN。未运行BOOT新门/模型/native字段正例；SourceA仍未证明、familyC/D08/20待执行、formal16/61=26.23%，下一BOOT实际graph/coldsource门后16。

2026-10-11 NPR-BOOT-01 actualsource gates（候选，actualClaude尚未执行）：七exactnamed new分别1PASS/0FAIL，assert18/184/4/8/14/5/26合259；七完整existing A共66PASS/0FAIL/426assert。真实CLI source/Bun cold在ordinary与compileddefine true各测READY/authAPI200/pid/emptyinstallation/stdin EOF exit0，invalid keydir启动非零/zeroREADY；没有finalcompiledartifact/native字段正例。实际图解析ownedbinding且global借用经location cache/invalidate保持同binding，recipe protocol/endpoint arrays及service value冻结负例已测。graph scoped两actualEffect关闭后modulebackgroundhandle仍在，由evalfixture显式process.exit0收尾；不把fixtureexit冒称完整importedmodule生命周期关闭。全部owned子进程有独立boundedkill及finally；源码准备不初始化key/不调用模型。
service initialr1实际20PASS/4FAIL原log/receipt保留；根因为Core safe wrapper注入OPENCODE_CONFIG_DIR被serviceEnv继承，fixture写自身XDGconfig而CLI读wrapperconfig，端口回到49375及占用反例错位。按用户推荐方案自动采用evidence-only preload删除该selector（原byteSHA36a0441a0737fd5ce9d5532d707d56e6ae448054741e277d2643465710b1afbb，路径/private/tmp/libra-main-resumption-20261010/npr-boot01-main-2.0.26-r1/service-isolated-selector-preload.ts）；不改wrapper/任何旧test/生产源码或原断言，仍actualsafe wrapper/filteredenv/isolatedHOME/XDG/RECORDfalse。r2实际完整24PASS/0FAIL/113assert，UTC04:26:22.817320→04:27:01.215201；如实记录新argv而不称原命令已通过。canonicalCore/Server/CLI final actual0；extra newtesttypes仍待actual，初版evidenceconfig缺package-local Buntype路径actualFAIL保留，仅调整evidenceconfig，源码不加ignore/不扩旧fixture。收口ROOTcheck/ownedfmt/actualClaude尚未运行，不提前Acceptance。记录UTC2026-10-11T04:28:24.205566+00:00；formal原cohort16/61=26.23%不变，下一NPR16，familyC/D08/20及finalfull仍待执行。

2026-10-11 NPR-BOOT-01 finalsource lint前滚：父级root-check-final actualexit1，rawSHA4ae883e53e0810e74a57a7b268117afbfe37e788355a874db3c7ad79a2e4a51c，原3error位于newCoretest两Reflect.set及CLIbuild Proxy Reflect.get。仅两个已登记owner修复：frozen-array负例改Object.defineProperty并各要求TypeError，build genuine compiler string/symbol反射用一行局部规则说明保持原义。无生产行为/旧test/I拓展；实际重跑fixed/closure/boundary及Core/CLItypes/newonlytypes/ownedfmt后0，其他无影响runtime门逐命令复用，不假称全部A均afterfinaledit。原FAIL和pre-root-lint-repair posthash保留。

core-fixed-final-r2 actualexit0 UTC2026-10-11T04:30:59.910685+00:00→2026-10-11T04:31:00.014112+00:00 rawSHA2b00337b424177767f51f17c850bf0ff5dc83dc959eba600721f2796e837b21f；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "30000", "--test-name-pattern", "^npr_boot_fixed_profile$", "test/native-origin-build.test.ts"]
core-closure-final-r2 actualexit0 UTC2026-10-11T04:31:00.041341+00:00→2026-10-11T04:31:00.166706+00:00 rawSHA5762272a27141b98fca4a93485d33abc47e54e4a587647f0b0e35279b2ae9e05；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "30000", "--test-name-pattern", "^npr_boot_closure$", "test/native-origin-build.test.ts"]
server-graph-final actualexit0 UTC2026-10-11T04:26:46.081952+00:00→2026-10-11T04:26:47.243378+00:00 rawSHA5879c9f15eaeb1063076f39c0483fac0df10fb291507d0abf9f867475ea4ccb4；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/server；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "--test-name-pattern", "^npr_boot_graph$", "test/native-origin-bootstrap.test.ts"]
server-default-final actualexit0 UTC2026-10-11T04:26:47.269549+00:00→2026-10-11T04:26:47.770689+00:00 rawSHA995e90f82026c15145e890523b6da09ca823bdb671492b65cf682c50428eada5；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/server；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "--test-name-pattern", "^npr_boot_default$", "test/native-origin-bootstrap.test.ts"]
cli-cold-final actualexit0 UTC2026-10-11T04:26:46.082112+00:00→2026-10-11T04:26:48.784624+00:00 rawSHA15a50532e77f7f32cc57b32ed66dbf58ae04da61bc570f51a7707e9ebd23150a；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "--test-name-pattern", "^npr_boot_cold_start$", "test/native-origin-bootstrap.test.ts"]
cli-isolation-final actualexit0 UTC2026-10-11T04:26:48.810303+00:00→2026-10-11T04:26:49.784051+00:00 rawSHA28324e61dbd8e7bd38ef1c8927a74990e2b8c1fe254b78e25355eee1bb5e4ec6；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "--test-name-pattern", "^npr_boot_isolation$", "test/native-origin-bootstrap.test.ts"]
cli-boundary-final-r2 actualexit0 UTC2026-10-11T04:30:59.893532+00:00→2026-10-11T04:31:01.104771+00:00 rawSHA96435225c8710e2d99305355e0b3be102e388ab98668a9ebb7078ebada9883c0；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "--test-name-pattern", "^npr_boot_boundary$", "test/native-origin-bootstrap.test.ts"]
existing-location-r1 actualexit0 UTC2026-10-11T04:17:33.407046+00:00→2026-10-11T04:17:35.530878+00:00 rawSHA770e7b2226c51ca3ee82ea9028a80d2d95ad70cf8a5289e9b62cde34780d9cae；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "5000", "test/location-layer.test.ts"]
existing-server-process-r1 actualexit0 UTC2026-10-11T04:17:33.406879+00:00→2026-10-11T04:17:34.004264+00:00 rawSHA1a6d349df80fbe6e5af2e8697107841737d8b8d9d298d23bd06bf63416f92b98；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/server；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "5000", "test/process.test.ts"]
existing-standalone-r1 actualexit0 UTC2026-10-11T04:17:33.407088+00:00→2026-10-11T04:17:34.913465+00:00 rawSHAc23b5a6443e526c1cc499f21be23b9b64c8f2b30e4747388ed868f60992f2231；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "test/standalone.test.ts"]
existing-connection-r1 actualexit0 UTC2026-10-11T04:18:48.458494+00:00→2026-10-11T04:18:49.121062+00:00 rawSHA2ff7ce8201b1bc279eb6896f51bbb81429097ca177f9391618c507dfeb5df0fb；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "test/server-connection.test.ts"]
existing-import-boundaries-r1 actualexit0 UTC2026-10-11T04:18:48.458335+00:00→2026-10-11T04:18:49.465403+00:00 rawSHA6587b9dbcbc14058adc8a34a78e96d03c438151e80dabf666113aaa8a215cae3；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "test/import-boundaries.test.ts"]
existing-service-lifecycle-r1 actualexit0 UTC2026-10-11T04:18:48.458472+00:00→2026-10-11T04:19:41.818543+00:00 rawSHA32ec6ffdec681d8cffec07002076c1ea17d116af694ee12d4d0efe101e6bdee8；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "test/service-lifecycle.test.ts"]
existing-service-r2 actualexit0 UTC2026-10-11T04:26:22.817320+00:00→2026-10-11T04:27:01.215201+00:00 rawSHA33cc1b248b96d53a69428fdd1f7e0b292b210c1db978eb573154148fae41130d；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "30000", "--preload", "/private/tmp/libra-main-resumption-20261010/npr-boot01-main-2.0.26-r1/service-isolated-selector-preload.ts", "test/service.test.ts"]
core-types-final-r2 actualexit0 UTC2026-10-11T04:31:01.065830+00:00→2026-10-11T04:31:01.400921+00:00 rawSHA8f0c4bff8dfe0ad0cea81fc8587f22596a0ee3fb280e4af824e688108e22e4cd；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "typecheck"]
server-types-final actualexit0 UTC2026-10-11T04:27:00.640783+00:00→2026-10-11T04:27:03.364748+00:00 rawSHA5b79a0e507fbfe3ee9fe68d86a274c3dec57408303abba3ba342ef01f4551dbe；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/server；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "typecheck"]
cli-types-final-r2 actualexit0 UTC2026-10-11T04:31:01.425696+00:00→2026-10-11T04:31:04.147484+00:00 rawSHA5b79a0e507fbfe3ee9fe68d86a274c3dec57408303abba3ba342ef01f4551dbe；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "typecheck"]
new-test-types-final actualexit0 UTC2026-10-11T04:31:04.172009+00:00→2026-10-11T04:31:05.720297+00:00 rawSHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/cli；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "/private/tmp/libra-opencode-native-producer-implementation-2.0.26/node_modules/@typescript/native-preview/bin/tsgo.js", "--noEmit", "--project", "/private/tmp/libra-main-resumption-20261010/npr-boot01-main-2.0.26-r1/tsconfig.boot-new-tests.json"]
owned-format-check-final-r2 actualexit0 UTC2026-10-11T04:31:02.255289+00:00→2026-10-11T04:31:02.625873+00:00 rawSHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "node_modules/prettier/bin/prettier.cjs", "--check", "packages/core/src/native-origin/build.ts", "packages/server/src/native-origin.ts", "packages/server/src/routes.ts", "packages/server/src/process.ts", "packages/cli/src/server-process.ts", "packages/cli/script/build.ts", "packages/core/test/native-origin-build.test.ts", "packages/server/test/native-origin-bootstrap.test.ts", "packages/cli/test/native-origin-bootstrap.test.ts", "packages/core/README.native-origin.md", "packages/cli/README.native-origin.md"]
owned-lint-repair-r1 actualexit0 UTC2026-10-11T04:30:43.766603+00:00→2026-10-11T04:30:43.823216+00:00 rawSHA618740daab2b20660a8f1c48dc18c139da049933a6f0e3834aa774833b8d6b5b；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "node_modules/oxlint/bin/oxlint", "packages/core/test/native-origin-build.test.ts", "packages/cli/script/build.ts"]
fixed-profile-runtime-join-r2 actualexit0 UTC2026-10-11T04:29:28.796647+00:00→2026-10-11T04:29:28.848037+00:00 rawSHAe8ab1a53a1c71e76dbe6a8a1c147ee1f2321006d17903e48ac17efa5c167c7a5；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "/private/tmp/libra-main-resumption-20261010/npr-boot01-main-2.0.26-r1/fixed-profile-runtime-join.ts"]
root-check-r2 actualexit0 UTC2026-10-11T04:31:36.809228+00:00→2026-10-11T04:31:42.625666+00:00 rawSHAbf926421be8a5affd72b7d5aa70cac1d5586285c79b2524193b6b982985ca421；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "check"]
fmt-final actualexit0 UTC2026-10-11T04:00:07.953705+00:00→2026-10-11T04:00:11.388616+00:00 rawSHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855；cwd=/Volumes/Sea/libra；argv=["cargo", "+nightly", "fmt", "--all", "--check"]
strict-final actualexit0 UTC2026-10-11T04:00:07.953724+00:00→2026-10-11T04:00:52.430600+00:00 rawSHAff0b2ad07f261f02049f6229b07a90326189876195aa9addcfb76356a817804a；cwd=/Volumes/Sea/libra；argv=["cargo", "clippy", "--all-targets", "--all-features", "--", "-D", "warnings"]
readme-format-final actualexit0 UTC2026-10-11T04:28:36.138232+00:00→2026-10-11T04:28:36.202733+00:00 rawSHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "./node_modules/prettier/bin/prettier.cjs", "--check", "packages/core/README.native-origin.md", "packages/cli/README.native-origin.md"]
最终actualrootcheck为36successful/36total、29cached（非全fresh）；private原8745逐byte/mode除累计32registeredoriginal delta外全MATCH（本卡4original生产变动+5newsource/test，README另记），前驱actualreviewed source除registered本卡/顺序README外全MATCH、unknown nonbuild0；root05 protected和既有11generatedexpected SHA均MATCH。actualruntime publicprofile/template/86builtin join亦0，compiledcandidate不授SourceA，actualSourceA/model/最终artifact均未证明。完整finalsource9及2README等待actual单Claude评审，家族C/D08/20/finalfull仍UNRUN；formal16/61=26.23%，下一16。


### 2026-10-11 04:50:33 UTC — NPR-BOOT-01 source-qualified local acceptance

Actual Claude Code R1 literal PASS，0P0/0P1/2P2/11P3，exit0；UTC 2026-10-11T04:37:00.114074+00:00→2026-10-11T04:47:52.146326+00:00，rawJSON SHA15294a37a565bee3ef962d1597bdbfbf5104dc1a1db11ad6dd7b8b07aa5e33bc。实际13owned files（6production/3test/2README/2rootplans）审查冻结与最终private11逐byte/mode MATCH；7new门各1PASS/0FAIL/259assert、7完整existing A合66PASS/0FAIL/426assert、canonical三package types/new-only types/ownedfmt及Rustfmt/strict均actual0。rootcheck r2 36successful/36total、29cached；原3Reflect lint FAIL、service r1 4FAIL、初版额外types FAIL完整保留，未重写为PASS。六既有A复用发生在最终两owner lint修订前，缺每命令同时刻ownedhash快照的证据限制保留。父级仅裁决/更新验收metadata，未修改actualreviewed private source或README；signed-DCO ordinary main Push随后执行，不tag/bump/release。

DEFER-NPR-BOOT-REVIEW-R1：actualClaude原文file/line/最小修正方向保留。P2-1 native measurement onLoad overrides Bun/import-attribute loader；P2-2闭包门86ID未在gate内与实际PluginInternal pre/post关联（本次独立测量已做，不替代未来gate）。P3-1 builder Proxy方法未绑定；P3-2 default负例检查旧issuer文件名；P3-3 build evidence含本机metafile路径位于artifact目录；P3-4子工具继承三个selector；P3-5非1 opt-in值ordinary须文档明确；P3-6 AC7 mapping；P3-7 status过期；P3-8复用A缺同时刻snapshot；P3-9 wrapper/fixture selector冲突；P3-10 prepare失败未typed映射；P3-11两个build guard负例未测。按用户外围延后策略不扩本卡生产写集：其中metadata P3-6/7本次直接对齐，其余具名后验/遗留。NPR08真实完整build/closure/binary cold不得借本次PASS代替；P2-1/P3-1等若造成其必需gate实际失败必须如实FAIL并前滚当前发布点，不能发布未验制品。

AC7实际证据映射：boundary gate验证CLI import与早期build guards；forged/cloned port在isolation/default/graph门，clone binding在fixed_profile门。BOOT源码本地Acceptance=locally-accepted，Lifecycle仍in-progress，family C/D08/20未完成。NPR16/17仍未实施，新门UNRUN；本次BOOT source具备实际预启动/graph交接，未提供loadedroots/physicaltransport/signer闭合或issuer challenge。SourceA仍UNPROVEN，无实际provider/model/native字段正例；Linux功能用户后验，最终private08/public20/计划full保留。原正式cohort16/61=26.23%，新必要BOOT child不改变61分母；下一NPR16→NPR17→NPR08。


### 2026-10-11 04:55:38 UTC — NPR-BOOT-01 actual signed handoff / NPR-16 sole writer start

BOOT source signed-DCO commit/main actual8f75b43c4f7505821dc070ef309c3282790f7dd7（parent ba028fec57d27fa8364720e4b1e02f94822708ce）；exact02plan/status两个paths，PGP signature/DCO1及commitblob逐SHA匹配，ordinarypush0、read-only GitHub main真实相等，05既有dirtySHA保持且未stage。BOOT private11 actualreviewed bytes/modes仍MATCH，literalClaudePASS SHA15294a37a565bee3ef962d1597bdbfbf5104dc1a1db11ad6dd7b8b07aa5e33bc。此source交接不是release/native GO。

NPR16依已actualClaude接受G09原位六production/一newtest/一CoreREADME及BOOT dependency启动；全部六新门/affected A仍UNRUN。冻结原8745source全部byte/mode除已登记32originaldelta外MATCH，当前8768source inventory、exact6production preimage和new index/test absent、CoreREADME preimage、expected11 pin SHA311aeeb67465d5d389640076729574fbd61068fc552b116a1d84ae11e0237804；不fresh学习expected。父级独占root metadata/CoreREADME实际写入/VCS，private solewriter为/root/npr04_loader_readiness，原BOOT写者复用串行；其可只写已登记sixproduction+one newtest及本卡isolated evidence，README先给proposal不并行写共享文档。原06publisher、05AI、03issuer、BOOT sixsources/两testsremaining、Schema/Protocol/lock/globalOpenCode受保护。

已停只读16准备handoff SHA1eb15f5983be667f25d0a96c14b164eb50206fdff963f4fc6635a40fb6275ec5仅API/真实owner观察，不冒称implementation或Claude。离线临时CA探针原Bun Fetch trustedPOST通过/untrusted证书拒绝、ownkeys已删除，仅机械可行性，不能计本卡六named gate或SourceA positive。具体16测试可用唯一newtest隔离Bun.build只改精确BOOT literal的compiled fixture、真实localhostHTTPS/临时CA/throwaway issuer/real06 SQLite pipeline；不得生产allowlist/envJSON/URLforward/TLSdisable或真实账号访问。NativeCore request↔AI normalized association、mutableheaders/body seal、originalFetch/physicalresponse/context/fence均实际闭合；WS/unknowntransport ordinary partial。没有newmodel/provider/凭据/额外依赖。正式16/61=26.23%，下一16实际singleClaude/sourcehandoff→17→08，C/D08/20/最终full保持。


NPR16 parent mandatory Rust gates：
fmt-final actualexit0 UTC2026-10-11T04:58:17.187459+00:00→2026-10-11T04:58:20.581281+00:00；cwd=/Volumes/Sea/libra；argv=["cargo", "+nightly", "fmt", "--all", "--check"]；rawSHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855。
strict-final actualexit0 UTC2026-10-11T04:58:17.187332+00:00→2026-10-11T04:58:54.926367+00:00；cwd=/Volumes/Sea/libra；argv=["cargo", "clippy", "--all-targets", "--all-features", "--", "-D", "warnings"]；rawSHA41bc23a09468fb0cc24e94dfa32af4ae287055383c3e6934d369d5cfd4df3bce。
rootRust sources未在private producer六owner实施中变化，复用编译缓存如raw保留；strict输出proc-macro-error2 futureincompat notice，非lint failure。parentrunner继承env/pinnedPATH，不冒称filteredenv；不输出env/credentials。NPR16 sixnamed/private A/types/rootcheck/actualClaude仍UNRUN，尚未locally-accepted。


### 2026-10-11T05:19:52.201908+00:00 — NPR08未来T2入口最小修订 / NPR16当前验收观察

父级按用户推荐方案自动采用已STOP的NPR08只读准备（readiness SHA1f08d0d6d4b79dc3c2d6e54c5e0dc7babae198191a0badbf79180790c11ae4da、metadata-replacement SHAb7f89614774854ad6c598252c6441f31ed63268e7dcd25d5dee2dd815b66c083、gate-manifest SHAd8698787bbdc9001562ac0cee7a4a4358d90f8b1287aaa89bfc9212b2ba43762、handoff SHA85a925245c4525999fe028f42de05278c8d42a478dff22c4e3e5f730cbefb2af均再次MATCH）。仅替换plan02当前未来T2/generate/compare的旧source root为固定2.0.26树，literal expected SHA311aeeb67465d5d389640076729574fbd61068fc552b116a1d84ae11e0237804，推荐Bun no-env-file/no-install实际未来argv及filtered fresh env前置；历史2.0.24/2.0.26证据、canonical脚本/defaulttimeout和六独立whole门不改。CLI仍自身完整run test，不预套旧service preload；prefix flags不冒称嵌套全传播。六whole/generate/independent-compare/build/artifact/install/actualchallenge全UNRUN，owned comparator尚未复制，不把缺少future tool算actual FAIL。原固定comparator3992bytes SHA6291fcda1c6a5d15d690752179832ad132881fcccf43dfa855150729d7e52a02及11expected生成文件只读MATCH，fresh actual compare仍待NPR08。此为未reviewed当前元数据，随本卡actual单Claude闭合，不改变production写集。

当前NPR16 solewriter持续实施6production+1newtest；Corecanonicaltypes r4 actualexit0 UTC05:10:59.607520→05:11:02.973200，rawSHA8f0c4bff8dfe0ad0cea81fc8587f22596a0ee3fb280e4af824e688108e22e4cd；前次type FAIL保留，后续源码变动须最终重跑。真实HTTPS/Server-location graph/SQLite六new门正在构建；actual-route r1/r2/r3 actualexit1保留，r3缺@opencode/protocol解析不得mock/external绕graph或记PASS。全部六功能验收及finalsource/Claude/sourcePush尚未完成。SourceA仍UNPROVEN，无真实模型/注册issuer/新release；正式16/61=26.23%，main同步origin、保护05dirty保持，下一16完成→17→08。


### 2026-10-11T05:35:43.085923+00:00 — NPR17 future challenge精确契约登记（实现/验收仍UNRUN）

父级消费已STOP只读17准备，STOP.json SHA f69a4e43bb43579c250ebf770b6e44cac2ca1620a536ff39c9f36558971bde2b、handoff.md SHA dab62a151af8fb98e36952ce96463dec007f6f02ea50d3a81426f48891e8b8b2、proposed-apis.md SHA a3f55cd70649d1d6e566ad65081a23ec8c9ac4bd3dd7092dd24661a8ff0c35d3 均按实际不同文件MATCH。采用推荐的原四production owner内closed10field publicresponse、独立attest-v1 directEd25519/BEu32+LP前像、严格64lowerhex argv和真正lazy ordinary imports；仅更新原卡future metadata，不改receipt-v1/endpoint/policy domains、Schema/AI/任何生产源码。actualcontrolled producer path SHA/ownedkey/generation/closedpolicy必须重测，attest仅keyassociation，不签field或resettaint/configure/loadplugin/initkey。已纠正17 existingServerBOOT target实际已创建，但17重验仍UNRUN。新规格由NPR17 actualsingleClaude/source验收闭合；当前没有TS/Rust challenge codec实现或实际binarychallenge，不能称已审/通过。NPR16唯一源码写者继续六门，待source/Claude/signed handoff后才允许17 implementation；formal16/61=26.23%、SourceA UNPROVEN。首次append guard误把STOP摘要用于handoff.md，actualexit1/未append，现按STOP所绑定各文件SHA修正；原failed记录保留，不是source/test FAIL。


### 2026-10-11T05:53:14.449988+00:00 — NPR16 final source candidate / parent freeze（actual单Claude仍UNRUN）

sole sourcewriter已terminal STOP，STOP SHAdd5140343088c140d5af850e1d3e7e48cf4750c2eb7d8ae3161fcc09c711c151；七owner已逐byte/mode及STOP evidence摘要复核。六new297assertions实际0/6PASS；六whole A实际49PASS/513assertions。A在所有最终production修订后运行，仅后续newtest erased callback参数类型标注/Prettier，按unaffected reuse登记，不称之后重跑。Core canonical实际tsgo -b tsconfig.json tsconfig.tests.json覆盖src/script/test，新test-only额外门亦0；不扩旧fixturetyping。final rootcheck实际36successful/36total/0cached，sourceclosure原8768→8770仅两new、五source变更加父级README，原8745仅35累计授权delta、unknown非build0、expected11 pin311aeeb67465d5d389640076729574fbd61068fc552b116a1d84ae11e0237804与protected05均MATCH。

actual_route同时保留独立manualclaim与原SessionRunnerLLM.drain(force:true) automaticprimary路径；原catalog/context/hooks/actualnode.mapLayer只观察实际service、真实HTTPS/Fetch/AI/Step/SQLite/proof原bytes与signature关联。临时fixture仅精确BOOT两public literal+compileddefine，throwaway issuer/tempCA/loopback synthetic frames，不是officialartifact/注册issuer/realmodel正例。Ambient普通Http memo绑定和Buffer.slice view缺陷已在六ownedproduction内前滚；初始真实FAIL永久保留。claim/Step同实际Effectscope；graphscope先关闭后fixtureprocess显式退出，不称privatebinary后台handle cleanup已通过。

计划外遗留DEFER-NPR16-MALFORMED-ERROR-01按用户外围延后策略采用：original invalid-json wire在native context-r1 actualexit1/logSHA07db505d182f391ad774f13b81f69e70ebbb253496aa015c3b782d2e23fbb508及独立compiled=false/noissuer ordinary同wire actualexit1/logSHA415ef6258824b009d95feb59eb738f38b1b065236156a3c653ef45314af49c4b均复现AI InvalidProviderOutput/Encoding SchemaError；六shared error-path owner及原Step错误分支byte均unchanged。精确broader serialization根因尚未修复或完全诊断，不记绿、不纳入本计划修复卡或正式61完成；wellformed provider-error负例是实际Exit.Failure/原AIErrorclass/ordinaryEnd/zero proof，不是Completed。25原failed command与core-types-r2 pre-spawn无subprocessreceipt明确UNRUN保留。

attempt-final-r2 actualexit0 UTC2026-10-11T05:43:59.374432+00:00→2026-10-11T05:44:00.747699+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "30000", "test/native-origin-attempt.test.ts", "-t", "^npr_16_attempt$"]；rawSHAdf4e44ceed03c0c7b3fc3ea58478ad3dbc698054e81504ec0b3963a182a46d74。
pipeline-final-r2 actualexit0 UTC2026-10-11T05:43:59.374604+00:00→2026-10-11T05:44:00.741006+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "30000", "test/native-origin-attempt.test.ts", "-t", "^npr_16_pipeline$"]；rawSHA600c058d7193337bfc2210c43ffdc9889b80e2c6ea2f81a0ac4cba56230636ba。
actual-route-final-r2 actualexit0 UTC2026-10-11T05:43:59.374493+00:00→2026-10-11T05:44:00.768659+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "30000", "test/native-origin-attempt.test.ts", "-t", "^npr_16_actual_route$"]；rawSHA2cf69dda3ad89b84046cd42964eabc2e3083f4b1d5e8652a03cadb7c6560b160。
ws-final-r2 actualexit0 UTC2026-10-11T05:44:14.688307+00:00→2026-10-11T05:44:16.040010+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "30000", "test/native-origin-attempt.test.ts", "-t", "^npr_16_ws$"]；rawSHA0d00fd0fefbc6c56dc93bc37e192fe6bf10bf52418f783b3af732ec7700c3f8a。
context-final-r2 actualexit0 UTC2026-10-11T05:44:14.688194+00:00→2026-10-11T05:44:16.219045+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "30000", "test/native-origin-attempt.test.ts", "-t", "^npr_16_context_failure$"]；rawSHA714f88e773771a070da4d036eea8ee7af13d9d8d9c4fc8731da9bf1e3806d1b0。
ordinary-final-r2 actualexit0 UTC2026-10-11T05:44:14.688931+00:00→2026-10-11T05:44:15.839248+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "30000", "test/native-origin-attempt.test.ts", "-t", "^npr_16_ordinary$"]；rawSHA954e8a0a30977b33293ca166b4ee9613b56d1bbcf3b72b9e3cbc80d36c2e1599。
existing-runner-message-r1 actualexit0 UTC2026-10-11T05:42:50.781484+00:00→2026-10-11T05:42:51.033970+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "5000", "test/session-runner-message.test.ts"]；rawSHA3a2069e3c0bfd84d3feb1d825d53f5dac14f717d81e3bd732b792ea7f265b5e9。
existing-model-hooks-r1 actualexit0 UTC2026-10-11T05:42:50.781535+00:00→2026-10-11T05:42:51.326433+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "5000", "test/session-model-request-hooks.test.ts"]；rawSHAa709b9e3c520cee8c3b320a146bf70f480bac1a3cfd4fb3bdccf9334119ebef9。
existing-plugin-policy-r1 actualexit0 UTC2026-10-11T05:42:50.781615+00:00→2026-10-11T05:42:58.783095+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "5000", "test/native-origin-plugin-policy.test.ts"]；rawSHA7099fd90526955d74333398c20bb446b82e424dca8020992a30ef27d2eef9488。
existing-persistence-r1 actualexit0 UTC2026-10-11T05:43:07.462973+00:00→2026-10-11T05:43:08.553844+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "5000", "test/native-origin-persistence.test.ts"]；rawSHA71c614bec64aaa83fcbda12f21cb9a3c687880fa6ff356c4d3f076c0f3549657。
existing-build-r1 actualexit0 UTC2026-10-11T05:43:07.462928+00:00→2026-10-11T05:43:07.639103+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "script/test.ts", "--timeout", "5000", "test/native-origin-build.test.ts"]；rawSHAed2d963f67e155fdc919850b1b4f5e6a4ff96045c8b58a35363070eb376da54a。
existing-bootstrap-r1 actualexit0 UTC2026-10-11T05:43:07.462860+00:00→2026-10-11T05:43:08.690812+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/server；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "../core/script/test.ts", "--timeout", "5000", "test/native-origin-bootstrap.test.ts"]；rawSHA7596396d9040787c75070a1b7c0ba959581e560fb7e90f91eb635725a6f1692b。
core-types-final-r2 actualexit0 UTC2026-10-11T05:43:41.354027+00:00→2026-10-11T05:43:41.680977+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "run", "typecheck"]；rawSHA8f0c4bff8dfe0ad0cea81fc8587f22596a0ee3fb280e4af824e688108e22e4cd。
newtest-only-types-final-r2 actualexit0 UTC2026-10-11T05:45:54.647186+00:00→2026-10-11T05:45:54.716121+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26/packages/core；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "../../node_modules/@typescript/native-preview/bin/tsgo.js", "--noEmit", "-p", "/private/tmp/libra-main-resumption-20261010/npr16-main-2.0.26-r1/tsconfig.native-attempt-only-r2.json"]；rawSHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855。
owned-format-check-final-r1 actualexit0 UTC2026-10-11T05:45:03.953504+00:00→2026-10-11T05:45:04.227032+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "./node_modules/prettier/bin/prettier.cjs", "--check", "packages/core/src/native-origin/index.ts", "packages/core/src/native-origin/policy.ts", "packages/core/src/session/model-request.ts", "packages/core/src/session/runner/step.ts", "packages/core/src/session/runner/llm.ts", "packages/core/src/effect/app-node-platform.ts", "packages/core/test/native-origin-attempt.test.ts", "packages/core/README.native-origin.md"]；rawSHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20。
fmt-final actualexit0 UTC2026-10-11T04:58:17.187459+00:00→2026-10-11T04:58:20.581281+00:00；cwd=/Volumes/Sea/libra；argv=["cargo", "+nightly", "fmt", "--all", "--check"]；rawSHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855。
strict-final actualexit0 UTC2026-10-11T04:58:17.187332+00:00→2026-10-11T04:58:54.926367+00:00；cwd=/Volumes/Sea/libra；argv=["cargo", "clippy", "--all-targets", "--all-features", "--", "-D", "warnings"]；rawSHA41bc23a09468fb0cc24e94dfa32af4ae287055383c3e6934d369d5cfd4df3bce。
readme-format-final actualexit0 UTC2026-10-11T05:51:12.594271+00:00→2026-10-11T05:51:12.939711+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "./node_modules/prettier/bin/prettier.cjs", "--check", "packages/core/src/native-origin/index.ts", "packages/core/src/native-origin/policy.ts", "packages/core/src/session/model-request.ts", "packages/core/src/session/runner/step.ts", "packages/core/src/session/runner/llm.ts", "packages/core/src/effect/app-node-platform.ts", "packages/core/test/native-origin-attempt.test.ts", "packages/core/README.native-origin.md"]；rawSHA17aa973d3f004560237d9a95171210b0671deff23d61628eecf7322ff5938f20。
root-check-final actualexit0 UTC2026-10-11T05:51:12.594266+00:00→2026-10-11T05:51:26.464078+00:00；cwd=/private/tmp/libra-opencode-native-producer-implementation-2.0.26；argv=["/private/tmp/libra-og-native-producer-bun-1.4.2/bin/bun", "--no-env-file", "--no-install", "run", "check"]；rawSHA7b0e534ea95446dbc3b838f67b5b1b41f35124709958450f75a27649935a9951。

本候选全部AC仍待actual单Claude裁决；SourceA UNPROVEN/nativeGO=false/0 externalmodelcalls。NPR08 T2/generate/compare/build/actualbinary/privateinstall、NPR17实际attest、NPR20/finalfull均UNRUN。本次无新公共release/tag/installer操作；formal原cohort16/61=26.23%不增计，下一16 actualClaude/signedsource→17→08。


### 2026-10-11T06:05:33.867587+00:00 — NPR16 actual single Claude R1 PASS / parent source acceptance

actual raw verdict原首行为 **PASS** (source-stage, NPR-16 only; not native GO, not Source A, not approved artifact)；rawSHA7aecb5f0d3b6f3dfccc26eba1316e51f3cab2c73271d804285944e8aac222950，UTC2026-10-11T05:53:39.205917+00:00→2026-10-11T06:04:02.578721+00:00，actualexit0/112turns。保留原Markdown **PASS** spelling及整份结果，不重写成伪literalplain前缀。actualseverity P0=0/P1=0/P2=3/P3=6，六AC全部source/localPASS。父级按真实日志及frozen10/前驱全树再次MATCH裁决source Acceptance=locally-accepted；Lifecycle in-progress及C/D08/20不继承为done，原formal16/61=26.23%不增加。

DEFER-NPR16-REVIEW-R1具名计划外遗留：P2-1首签后任意routine registry/settle/Location epoch改变会永久proofless直到restart；P2-2 sticky issuer/seal/taint转移缺固定诊断；P2-3真实长transcript可能触及4096-object seal cap。P3-1 current55-provider guard未拒未来额外builtin；P3-2首load Effect.promise不可中断的binary双hash；P3-3 request前valid()将self-deny；P3-4 cancellation negative依赖150/400ms时序；P3-5现有Step Core-internal caller nativeReceipt兼容口；P3-6卡内rootcheck argv与实际记录差异，本次仅metadata改为真实no-env-file/no-install argv。按用户外围/同难度延后策略不扩本卡production写集，P2/P3不把unknown转换为许可，不降低native SourceA。前三协议不在initialclosedallowlist、3xx/4xx/secondLocation/超seal大请求仍未实跑，明确残余风险，不称全面验收。

DEFER-NPR16-MALFORMED-ERROR-01保留native与ordinary两个真实FAIL/原错误owner，未修不记PASS。所有private八reviewed files与8745/35授权delta/8770inventory及protected05/expected11 pin再次MATCH。原AI witness、physical originalFetch/Response/bytecopies/同scope singleclaim及06 admission afterprojector/beforeseq-event边界获实际Claude确认。未来08全T2/generate/independentcompare/actualbuild/coldstart/install、17真实binarychallenge、15admissionlist、09registration、realnativefields、20publicfull/security/fourplatform/release仍UNRUN；SourceA UNPROVEN/nativeGO=false。没有新release/tag/modelcalls，下一签名source普通Push main→NPR17→NPR08。


### 2026-10-11T06:06:41.327506+00:00 — NPR16 actual signed handoff / NPR17 sole writer start

NPR16 actual signed-DCO main f9950f22028b0e266a8d2eedfe444b0cfd797323（parent 8f75b43c4f7505821dc070ef309c3282790f7dd7），exact02plan/status两个paths/普通Push/只读GitHub main相等，actualClaudePASS SHA7aecb5f0d3b6f3dfccc26eba1316e51f3cab2c73271d804285944e8aac222950；final八private bytes/modes及全8770source tree逐项再核MATCH，原8745仅35授权original变化，expected11 pin311aeeb67465d5d389640076729574fbd61068fc552b116a1d84ae11e0237804与protected05保持。此为source交接，不是release/nativeGO或完整family完成。

NPR17 current精确契约已原位登记，按用户推荐方案自动采用只读准备两项必要existing A追加：完整CLI native-origin-bootstrap及Core native-origin-build，原sixnamed/fourproduction/四落点不变。actual所有新门/六完整A/CoreServerCLI types/newtest types/ownedfmt/rootcheck与本卡Rustfmtstrict均UNRUN。唯一source writer仅可写四production和一newtest；CLI README给proposal，由父级顺序写，root两metadata/VCS/实际单Claude父级独占。NPR16六source/其newtest/CoreREADME、BOOT非本卡owner/原AI/06/Schema/Protocol/lockeddependencies冻结；不改receipt/policy/endpoint wire/domain，不初始化真实issuer或调用任何模型账号/凭据。sourcefixture不替代08实际binary attestation。

CLI src/test均不得直接或用path绕行导入Core；不得为了测试拓宽Server production facade为initializer/key API。机械installation可由独立Node crypto＋Server public buildSpecification＋allowed Schema codec构造，actual Core initializer由完整issuer A覆盖，08 actualbinary初始化仍必须实跑。此夹具调整不改变six AC/六真实CLI门或生产写集。

formal原cohort16/61=26.23%不增计；SourceA UNPROVEN，下一17 actual source/Claude/signed handoff→08唯一privateartifact/fullT2/generate/independentcompare/install；20公共family C/D及最终计划full仍必需。


### 2026-10-11T06:11:01.021795+00:00 — NPR17 parent mandatory Rust checks / source owner

唯一source/test writer已指定/root/main_progress_reconcile，仅四production+一newCLItest；父级CLI README/root两metadata/VCS/实际Claude单写者。保留existing CLI index.ts mode0755（493），其余owner按baseline模式，不为统一0644引入权限变化。

fmt-final-r2 actualexit0 UTC2026-10-11T06:09:10.787032+00:00→2026-10-11T06:09:14.017513+00:00；cwd=/Volumes/Sea/libra；argv=["cargo", "+nightly", "fmt", "--all", "--check"]；rawSHAe3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855。
strict-final-r2 actualexit0 UTC2026-10-11T06:09:10.787010+00:00→2026-10-11T06:10:01.300456+00:00；cwd=/Volumes/Sea/libra；argv=["cargo", "clippy", "--all-targets", "--all-features", "--", "-D", "warnings"]；rawSHAaf50d69b05ec228cba3bb4207ae3562b0764c6e47d8e0bc6b136cd1adb956312。

首次parent runner调用缺explicitcwd，Python把cargo当cwd而在spawn前报FileNotFoundError；fmt-final/strict-final两个emptylog与prelaunch-r1.failed.json保留，subprocess_state=UNRUN_NOT_SPAWNED，不冒称cargo test/fmt/lint实际FAIL或PASS。corrected final-r2才是真实运行。strict只proc-macro-error2 futureincompatnotice，非-Dwarnings失败；继承env/PATH的父级Rustrunner不称filtered，未输出env/credentials。private六new/六完整A/三canonicaltypes/newtesttype/ownedfmt/rootcheck/actualClaude尚未运行，不能依此宣布本卡localAcceptance。SourceA UNPROVEN，formal16/61=26.23%，下一17 actualsource/Claude/signed→08。


2026-10-11T06:30:34.913401+00:00 NPR17 final source candidate：唯一writer已terminal STOP SHA 1d4069aaf036416f96884d0b8f657f4e71e2b34ebe8102fe070046fbd2469755；16child actualfinal门通过、父级README/六filefmt/rootcheck通过，官方8745除累计37授权原件外bytes/mode相同，full8771/expected/protected05一致。actualClaude待执行，AC未裁决；formal16/61=26.23%，SourceA UNPROVEN，下一17单审/signedsource→08。root36total有29cache，不称fulltests/coldcompile；历史7childFAIL及parentprelaunch/readonlyprep原证据保留。


2026-10-11T06:43:48.675095+00:00 NPR17 actualClaude R1 PASS，rawSHA4136aed08b68939e35cca323b3d175159a908f1643572d264fd10f0b449281e2、108turns/exit0/0P0P1；六AC/VER source locally-accepted。DEFER-NPR17-REVIEW-R1记录1P2/5P3原始外围遗留与观察，frozen六private source/docs不改；family08/20 C-D仍UNRUN、SourceA UNPROVEN、formal16/61=26.23%。下一签名directmain source handoff→08实际artifact。


2026-10-11T06:55:02.141868+00:00 NPR17 signed-DCO source commit563ac06869cec55e8974fcf2ccae09eefc33ce59已ordinary Push main、远端一致；NPR08 fresh actual验收证据目录已建立，8771已审最终源码/lock/固定expected311a/owned比较器6291/保护05及依赖metadata实测一致。实际T2/generate/compare/build/install/coldstdio/attest/model/Claude仍UNRUN，正式16/61=26.23%、SourceA UNPROVEN。下一NPR08→15；未进行公共release或全局OpenCode修改。


2026-10-11T07:12:48.787049+00:00 NPR08实际进度：canonical actual-generation exit0，fixed independent-comparison exit0，11生成文件/源码8771全未变化。65具名门65PASS/0FAIL/1935assert；六T2首轮AI1785PASS/28skip、Plugin9PASS、Schema74PASS、Client228PASS、CLI328PASS/9skip均exit0；Core真实6565PASS/30FAIL/49skip/exit1，原raw与receipt留存。23搜索失败为curatedPATH缺rg而外网guard拒GitHub下载，7shell失败为系统Bash3.2差异。推荐必要纯证据工具链：owned prefix独立GNU Bash5.3.0和固定rg15.2.0（公开bin摘要实测，原生产/测试/README源码不改、无global安装）；实际四失败完整targets重跑90/577/11/16PASS、0FAIL，原shell19skip保留。Core全量r2与29独立既有A实际进程已启动，结果未终止前不称PASS。rootcheck实际exit0/36successful/36total/36cached，全cache不冒称test。canonical NPR08-evidence含九同名predicate，其中checks PASS，source/lock/focused PARTIAL，binary/install/model/review UNRUN。正式16/61=26.23%、SourceA UNPROVEN；真实模型0，下一08完成→15。


2026-10-11T07:17:03.325746+00:00 NPR08实际全部测试门收口：Core完整r2实际exit0、6595PASS/0FAIL/49skip，六T2合计9019PASS/0FAIL/86skip（原每门skip不改写），65具名1935assert已实跑，33明列A去重相同cwd/argv后29独立命令302PASS/0FAIL。frozen toolchain仅curated owned PATH，producer/原test无byte/mode变动；原Core30FAIL immutable保留，不写成首轮PASS。fixedexpected311a及ownedcomparator6291实测post-tests independent compare exit0，全8771仍MATCH。人工focused及checks谓词有actual证据，private artifact build真正启动（完整WebUI、无skip-web-ui、fixed Bun/version/channel/fresh outdir）；build仍RUNNING，binary/version/install/runtime/model/Claude未验，仍不能整体Acceptance/nativeGO。正式16/61=26.23%；下一08 actualbuild/私有安装→15。


2026-10-11T08:00:19.770007+00:00 NPR08真实模型观察：三独立实际CLI sessions均FAIL（免费Zen403；授权GLM400/evidence-only steps修正后monthlyquota），三个export实际0/parse成功；有效模型输出/native目标字段均0。AC9只是如实登记PASS，实际模型成功/SourceA/nativeGO均未证明；不换账号/不绕官方免费限制。root实际fmt/strict exit0，下一actualClaude08→15；正式16/61=26.23%，protected05未改。


2026-10-11T08:18:00.318020+00:00 NPR08实际单Claude R1 PASS，rawSHA0d388a824379310ba037ace230e5e4095ef8ff868d7b56cfbf3c417cd6e8bfa3/98turns/exit0/0P0P1，parent postreview frozen bundle/3158deps/8771source/1192metadata及immutable artifacts全部MATCH。九private谓词PASS，三modelsession真实FAIL/coveragePARTIAL，SourceA UNPROVEN/nativeGO=false。私有交付local验收完成，public family C/D20未验，所以formal16/61=26.23%不增计；NPR08 signedmain3286609已交付；NPR15 source/local六AC及六new/22existing/fmt/strict通过；正式双env nextest r2 actual9743PASS/0FAIL/0实际retry/10skip/23slow/2leaky，actualClaude R1 PASS/70turns/0P0P1/97frozen MATCH，1P2/4P3计划外DEFER-NPR15-REVIEW-R1与原诊断FAIL/PARTIAL留存，signedmain c49685a6已Push并实测remote MATCH；NPR09八AC/VER source/local PASS，50focused/fmt/strict及正式full-r2 9751PASS/0FAIL/0retry/13slow/1leaky/10skip；actualClaude R1 PASS/57turns/0P0P1/190frozen MATCH，1P2/6P3计划外留存，signedmain6275038364bac4d7408461b2fa71b673c55dac15 已Push/remote MATCH；NPR10已开始，八new/full/Claude待执行。DEFER-NPR08-REVIEW-R1明确登记2P2/4P3计划外：publisher fail-loud、未来version history、BOOT诊断、js/jsx loader、interrupt顺序、official client原baseline未测；详见plan02原review记录，当前artifact/source不变，不记修复。Linux后验、public安全CI/四平台/NPR20/finalfull及真正native正例仍保留。


### 2026-10-11：NPR15远端交付确认，NPR09开工

NPR15 signed main `c49685a6fc4c026e6268b7824835f3b488c3311d` 已实际Push，2026-10-11只读ls-remote与本机main匹配。NPR09开始受控issuer registration实现，采用显式expected-epoch CAS、原trust不变、same-connection writer-lock-first及封闭单registry；验收/Claude/交付UNRUN。正式16/61=26.23%，public C/D仍待NPR20，SourceA UNPROVEN，无新的模型调用。未混入plan-20260905改动；FIX-RG-SCOPED-04继续计划外延期。


### 2026-10-11：NPR09 focused/fmt/strict实际通过，完整树门待执行

八个actual CLI/八新unit/27existing共43PASS、0FAIL；五production SHA逐项一致。真实producer完成challenge后，同conn撤trust先胜时登记拒绝；两实际Libra路径分别重建private policy，原descriptor不回写。fmt/strict实际exit0；全量/Claude/PUSH未运行，不计formal done；16/61=26.23%，下一NPR10，SourceA UNPROVEN，0新增模型请求。


### 2026-10-11：NPR09首轮全量FAIL，前滚guard兼容及external数据登记

Nextest r1 exit100/9749PASS/2FAIL/13slow/2leaky/10skip/source unchanged，原证据不改。combined cfg导致tests被literal guard误视为production；改标准分立cfg，guard不改。external owned installation.json作为untrusted producer公开安装数据登记test allowlist；新增必要I只有test侧审计分类，production仍5。正式完成不变，重跑/Claude/PUSH待执行。


### 2026-10-11：NPR09前滚后50focused实际通过

八unit、八真实CLI及34既有/guard共50PASS；标准test cfg和external installation分类修正后的两个原失败检查均实际PASS，fmt/严格clippy通过。原始full-r1 FAIL留存，full-r2/Claude/PUSH待执行，正式16/61=26.23%不增计，下一NPR10；无新model/public release/nativeGO。


### 2026-10-11：NPR09 source/local actualPASS，待签名main交付

八AC/VER本地成立：50focused/fmt/strict实际PASS；双env nextest r2 9751PASS/0FAIL/0retry/13slow/1leaky/10skip、850.764s、actualexit0/16owned unchanged，原r1两FAIL留存。actualClaude R1原首行**PASS**/57turns/0P0P1/rawSHA441971ed59b6b3cd86b751fcf65e547c6014c3c0ad8b447ebe572093d54e25a6；full与只读评审并行，freeze时full RUNNING，终态后父级190frozen/16owned/full源码逐SHA MATCH，正式push门成立。后审只本plan/status事实，不改reviewed Rust及公开命令docs；protected05/privateartifact/globalofficial/backend均MATCH。1P2/6P3与唯一既有upgrade probe LEAK原事实按用户延后策略记计划外UNFIXED，不以LEAK称clean、不以synthetic称native，实际FAIL硬门保留。signedDCO ordinary main Push待执行，下一NPR10；locally-accepted直到NPR20 public C/D，formal16/61=26.23%，SourceA UNPROVEN/nativeGO=false，无新OpenCode模型请求或公共发布。


### 2026-10-11：NPR09已签名直推main，NPR10开工

NPR09 source/local八AC/VER actualPASS后signed DCO commit6275038364bac4d7408461b2fa71b673c55dac15 已ordinaryPush，实际remote MATCH；16blob与审后记录一致，原protected05未加入或改写。NPR10 source-only开始原子combined untrust/current permission data helper；12owned/base及backend已snapshot，八新门、focused/full/actualClaude/PUSH UNRUN。当前正式16/61=26.23%，public C/D20/SourceA与Linux用户后验约定保留，未公共发布或调用installer。

2026-10-11T12:28:43.146267+00:00 NPR10 current macOS focused事实：八actual CLI-r4各1PASS/0FAIL，三unit-r1共3PASS，所有实际门12owned before/after SHA unchanged。覆盖两安装独立新key轮换、active当前transaction许可数据、combined atomic/idempotent untrust、重信任不复活native墓碑、实际两CLI并发与故障rollback、容量/物理alias拒绝、九实际content-addressed synthetic legacy checkpoint对象及fixture tag不变、自动binary-only暂停/恢复。permission helper结果仍非NativeProven；历史对象夹具明确synthetic、非真实native正例。cli-rotation-r1 helper误读manifest issuer实际FAIL；r2 runner prelaunch UNRUN与误改输入路径后的实际CLI FAIL保留；cli-immutable-r3 unsupported update-ref tag实际FAIL，前滚采用commit包裹tree及受支持tag命令，r4原保留断言通过；fmt-r1真实exit1仅新tag调用排版差异，按其diff修正，最终fmt/strict/full/Claude/PUSH尚待。批准private安装/recipe/binary全部保持r1真实输入，0新OpenCode模型调用/global更改/publicrelease。AC未提前勾选、formal16/61=26.23%不增计，SourceA UNPROVEN/nativeGO=false；下一本卡最终验收→NPR11。

2026-10-11T12:29:23.522324+00:00 NPR10 final格式前滚实测：fmt-r2 actualexit0，三unit-r2 3PASS/0FAIL，各门12owned SHA before/after unchanged；只有已说明的测试tag调用排版规范化，无production变化。八CLI-r4通过保持，相关回归/严格lint/完整nextest/actualClaude/PUSH待执行，不提前接受AC；下一NPR11，formal16/61=26.23%不变。

2026-10-11T12:34:26.304909+00:00 NPR10 pre-full事实：八new actualCLI-r4八PASS、三unit-r2三PASS，NPR09回归unit8/CLI8共16PASS，11related注册/信任/RPC/observer/help/error/docs/配置归属/production unwrap共65PASS，精选92独立谓词全部实际exit0。所有精选门两production最终SHA MATCH，各门12owned before/after unchanged；new CLI之后只有tag调用fmt排版规范化，无生产/断言/流程变化。fmt-r2实际0，strict-r1 all-target/all-features -Dwarnings实际0，rawSHA89ff3fd6b640e77552dbf3892bbc0973a6bbffdb91cad647036fe7f97e368a74。trust.rs实际未改，最终landing/prod为2/2，原可选owner保留而无新写集。现在冻结最终源码运行双env childumask022 canonicalFull，actualClaude只读单审可并行；full/Claude未终态前AC不放行、不Push。保护05不变，无模型/public release/nativeGO；formal16/61=26.23%，下一本卡final→NPR11。

2026-10-11T12:52:57.036256+00:00 NPR10 forward fix最终事实：首canonicalfull actualexit100/source12unchanged，398.491s/6813-of9754 run/6812PASS/1FAIL/2slow/2flaky/1leaky/10skip/2941UNRUN，原始全量与SIGINT早停决策永久保留PARTIAL_REAL_FAILURE。停止原因是provider_exporter_trust_is_ungated_and_trusted_dir_bound三次失败、已明确输出角色回归，非超时观察或逃避门；生产plain行恢复原provider exporter标签且保留removed/native/artifact信息，不弱化既有测试。首actualClaude R1首行PASS/49turns/0P0P1/rawSHA17b9dc388328c03595f7b844b9e29780db6ee1b601d39dddcc75a1a7522b5d60，仅旧冻结源码审查，当前前滚必须R2实审。P2 damaged-registry可用性采用小修：EN/ZH/compat/tracing/backend说明实际config unset agent.trust.opencode只暂停binary，不删除native墓碑，重trust可恢复active，永久native需修复保留原记录并成功combineduntrust；npr10cap保留原全拒绝断言并实测这个CLI只删binary row。P3 error005扩大缺repo/开库说明与EN/ZH/backend表项已同步。准备doc helper basename误匹配tracing agent.md曾actualexit1、局部两doc已写；精确path/idempotent重跑完成同写集，不是source或测试FAIL。最终八CLI-r6八PASS/三unit-r3三PASS、NPR09 unit8/CLI8共16PASS、12related含完整原RPCtrust九门共74PASS，精选101PASS/0FAIL且两production与test最终SHA逐门MATCH；fmt-r3/严格strict-r2实际0，所有门12owned内冻结。现在第二次canonical full与最终R2只读单Claude，AC仍未裁决/PUSH待。formal16/61=26.23%，SourceA UNPROVEN/零新模型或公开release，下一NPR11。

DEFER-NPR10-FULL-OBSERVATION-01（计划外、未修复、非PASS豁免）：首full npr09_child_mechanics的synthetic2s positive期限firstFAIL/TRY2PASS；branch_store_error_display_pins_each_variant出现实际LEAK，nextest按自身语义passed(leaky)。根因未证，不能称clean或保证无泄漏；未来最终真实FAIL仍强制前滚，不以此遗留放行。DEFER-NPR10-VALIDATE-COST-01（计划外P3）：admission每次validate全部bounded registry并重渲染模板，未来热路径性能需独立量测，当前不加cache/异步或新scope。R1 parent目录重验建议交接NPR11，复用现有fallible ensure_parent_not_world_writable，在独立新provenance测量时调用，不能声称10已签发proof。

2026-10-11T13:10:49.271298+00:00 NPR10 actual latest-main integration：readonly ls-remote发现fa41532509795be2f9925a7b88ed79ec4f978c60，native fetch33objects确认6275→fa415；两个upstream提交实际11files/bridge与livecapture重试、SSH与CI/test/serial目录变化，非纯版本不能复用最终full。首R2 actualClaude PASS/48turns/0P0P1/rawSHA6aeba75ec465bac3f5f3bd3a50dc9c8e9c6f125444ce7c4b0b55ff74f4d836db保留为合并前source审查；fullR2为更新最终基线明确SIGINT终止，actual100/405.114s/6842-of9754 run/6842PASS/1slow/1leaky/10skip，其余UNRUN，未记wholePASS；不是观察超时重启。先独立backup12owned/保护05，native merge ff-only autostash完成HEAD fa415，autostash apply发生冲突但原改动安全保留3fdc2f3/stash；actualclean HEAD上从该确切stash恢复10unchanged allowed owners及原用户05工作（仅恢复原字节、始终排除本卡），对tracing/index各一处incoming replacement用baseline旧块exact1match机械拼接，双方段落保留。recovery-final证明10owners exact、2docs双变更、protected05原a705 SHA/原empty stage全部MATCH，恢复stash继续保留。当前新start-r3绑定实际HEAD fa415、before-r3为clean新基线；最终merged focused/fmt/strict/full/Claude全待重验，不以未完成两旧full或source-onlyPASS放行。首批101focused先前source证据仍真实，但不能当最终新基线全量。NPR10同2production axis，无新工厂/模型/公开发布、formal16/61=26.23%；下一最终合并源码验收→signedmain→NPR11。

DEFER-NPR10-REVIEW-R2（计划外P3，非本计划新增实现）：epoch位数/active转revoked可能让接近64KiB的历史registry增长约2bytes导致combineduntrust保守拒绝；当前已验binary-only临时quarantine且保留墓碑，未来独立headroom迁移不删记录也不放大cap。catalog历史身份继续append-only承接既有09/15约束；writer busy细分stable code、旧automatic revoke错误静默路径单独遗留，本卡不新增error code/自动恢复系统。NPR11必须独立fresh provenance+当前trust/dirs读，即使旧automatic删除失败也不得授予native许可；不把自动移除成功当唯一安全判断。human输出变化已明确COMPAT并保留原role；最终CLI重跑消除fmt后尚未重跑的旧残余。

2026-10-11T13:22:09.882762+00:00 NPR10 merged fa415最终精选185PASS/0FAIL：原101+incoming bridge9/serial25/SSH50，全部actualexit0/nonzero且两production与最终testSHA相同，各门12owned unchanged；fmt-r4/严格all-target/all-features strict-r3 actual0。下一正式双env/child022 full-r3与当前merged树actualClaude R3，完成前AC未勾选、source/PUSH未接受。用户最新追加“请在这个任务卡完成后临时做一个 bump patch 并发布新版本”，推荐执行顺序固定为本卡source验收→signed-DCO directmain→独立TEMP-NPR10-PATCH→稳定四平台及安全CI真实成功→指定curl本机安装→网站docs交付→NPR11；当前readonlyGitHub稳定v0.30.43，暂拟0.30.44且发布前再次复核。NPR20家族最终门、真实native来源及最终全量保留，formal16/61=26.23%在实际C/D之前不增计；保护05原字节/未stage、零新OpenCode模型/global更改。

2026-10-11T13:43:37.284309+00:00 NPR10最终source/local裁决：185focused/fmt-r4/strict-r3实际PASS；双env/child022 canonicalfull-r3 actualexit0，Summary [ 902.736s] 9760 tests run: 9760 passed (14 slow), 10 skipped，rawSHAb6a703c0ec50ed5171927ffafa9c7e0ca1dc9105ec5f220e089b2bce1ae05e3f，所有12owned before/after unchanged。actualClaude R3首行**PASS** for the NPR-10 source on merged main fa41532509795be2f9925a7b88ed79ec4f978c60. No P0 or P1. Push remains gated on full-r3 reaching terminal exit 0 on the unchanged twelve owned files. At freeze it was still RUNNING_NOT_PASS under pid 88742.，actualexit0/is_error=false/54turns/0P0P1/rawSHA8243b92880de1dc6d1528d2ce84638877eb57017c5e477aacbbc8cf1ad589bd6，冻结时full仍RUNNING的陈述真实；父级两终态后复核367frozen以及12root/runtime/docs/full identity全部MATCH，incoming11file融合双方变更，privateartifact/globalofficial/独立backend/原05 MATCH。仅先前focused之后两plan metadata audit变化，正式full和review冻结是当前相同字节，不把早期PARTIAL full当PASS。九非阻塞观察原文保留，DEFER-NPR10-REVIEW-R3计划外承接旧duplicate-trust recovery/headroom/taxonomy/testtightness/JSONstyle，NPR11必须writerlock-first+freshprovenance/currenttrust/dirs且不依赖自动删除成功；提交后验证树mode100644。八AC/VER勾source/local，Acceptance locally-accepted；真实TEMPpatchC/D前Lifecycle仍in-progress，formal16/61=26.23%不增。现在signed-DCO普通Push main，随后临时patch稳定发布/四平台/安全CI/网站/指定curl安装，下一NPR11；零新模型，SourceA UNPROVEN/nativeGO=false。后审只改本事实状态audit，无runtime/CLI/publicdocs变动。

2026-10-11T13:45:13.904158+00:00 TEMP-NPR10-PATCH：依用户追加指令，NPR10实际source/单Claude/full/signedmain通过后，从当前稳定v0.30.43顺延v0.30.44。本独立提交仅package自身version/lock与POSIX/PowerShell installer fallback四faces，及本计划/status事实；不改依赖/运行逻辑/测试断言。已验相同实现全量按用户授权复用，仍跑版本guard/fmt/strict及当前最终HEAD安全CI/四平台signedstable/installer/实际网站C-D。现在release/tag/site部署/安装均UNRUN，不提前完成NPR10，SourceA UNPROVEN/nativeGO=false，Linux功能由用户后验。NPR20家族最终聚合门保留。


NPR11 execution start 2026-10-11T14:33:00.820182+00:00: user latest authorizes continuing immediately while temporary release waits CI. NPR10 source952cc957/185focused/9760full/actual single Claude PASS already pushed; TEMP0.30.44 remains pinned68e35b9 and is not released/installed. NPR11 only two production owners, exact immutable NPR03 public signature vector copied SHA d3589b58c490a0937cc69d84f8b124b1674151f1b142c200a95af40779dbdff1, sealed verifier/current permission snapshot; no second archive factory. Tests/fmt/strict/Claude/Push UNRUN; no full trigger. Original plan05 SHA a705585d remains unstaged/protected. SourceA UNPROVEN/nativeGO=false, zero new model calls, formal16/61=26.23%; Linux functional user-later. Final family NPR20 retained.


NPR11 final source/local acceptance 2026-10-11T14:48:02.629384+00:00: exact immutable NPR03 vector d3589b58 realring PASS and unknownsynthetic nonce deniedcatalog; separately signedpublictestseed source/field/runtime negativecases, activecurrenttransaction actual approvedbinary SHA547c36eb measured and removaldirs/inodedrift/revoke拒绝（key/receipt仍SYNTHETIC、非native/providerpositive）。Sevenexact1PASS each; related30unique including7named/23existing（37executions）actual0；fmt-r2/strictalltargetallfeature-r1 actual0/all6owned unchanged. ActualsingleClaude R1 firstlinePASS/39turns/noP0P1/rawSHAeccdd6ad4f501455e7d4a87877940fa2f68e3f4685bd9375d7d2c42a56e63ed6；parent83frozen/6owned bytes+mode+protected05+emptyindex MATCH，only本plan/status事实后审变更，不改已审Rust/fixture/internaldoc。DEFER-NPR11-REVIEW-R1：13nonblocking观察原文留存、全部计划外UNFIXED：先cheap验签再hash/按permission snapshot一次测量、canonical目录统一、未来512MiB/1GiB共享cap、attest reader更严格parent/fd检查、JoinErrorfixed mapping、anyErr分类、冗余read/smallstat、后续移除deadcode allow、结构guard及observer-update测试强化、versioned symlink升级重新登记说明、fake-trust error taxonomy/defaultL1与其它未测边界。当前catalog唯一187MiBartifact与canonical登记路径受支持、无实际P0/P1；不重新实施人已延期fdexec/sameUID/FIX-RG-SCOPED-04。未来consumer必须writerlock-first保留当前snapshot、实际受授权source关联及任何Err=>partial零artifact；从模块私有构造不可绕过当前权限。source signedmain Push下一步，正式16/61=26.23%/family公共C-D待20；TEMPv0.30.44仅已审68e，整个CI实际PASS前无tag/release/install。下一NPR12审计，0新OpenCode模型，Linux由用户后验。


NPR12 start 2026-10-11T14:50:48.594683+00:00: source dependency NPR11 signedmain ef6adf8 actualremoteMATCH, actualClaude39turnsPASS/noP0P1/83frozen6ownedMATCH/7named+30unique/fmtstrict0。现在审计NPR08最终source8771entries（58symlink原link bytes/lstat）/actualcompilerclosure/65named/原3model失败字段coverage0，以及NPR03signer/NPR11ring；当前private source/artifact/globaloriginal实读MATCH，无重新构建/模型调用。preparation两个reader错误把source symlink当regular导致AssertionError/IsADirectoryError，保留原错误，corrected只读r2原linktargethash/lstat8771/58/0mismatch；非source FAIL、不改任何producerbytes。单actualClaude替代本卡旧dual_review谓词措辞，predicate名保留；audit/runtime/live字段层次分别记，不以synthetic或静态命中冒称真实provider。结论/Claude/Push pending，TEMP44冻结68e继续待wholeCI，formal16/61=26.23%，原05保持未stage；无新model、SourceA UNPROVEN/archiveGO=false。


NPR12 draftaudit: eight namedcriteria inline/private exact JSON;7mechanism/accurate-observation predicates PASS, actualClaude UNRUN. Actual source8771/link58/0mismatch+compiler3865/raw4104/transform152/builtin86;65namedlogs realexit0/hashMATCH reused same immutable source, earliest failures retained. All3real model sessionsFAIL/0fields; SourceA UNPROVEN/archiveGO=false. Draft source-ready conclusion limited to reviewed darwinarm64 OpenAI Responses profile after actualreview; Anthropic/Bedrock production source contracts deterministic only, no route/catalog expansion. Correct helper staleaisdk path resolved actualCore aisdk/aisdk-native via rg, originalhelperfailure retained;4NPR13 names taken fromactual receipts. NPR11 unchanged reviewed source. No new models/build/pin/global install.


NPR12 final audit 2026-10-11T15:11:28.114896+00:00: actual単Claude104turnsPASS/noP0P1/rawSHA7761fee857f1abf22c83b4e708a558045706914f42796d6ce1c3a80aaf269197；parent214frozen/2root/privateJSON bytes+modes/原05/emptyindex全MATCH。8namedcriteria exactinline/private members全部source-mechanism或真实观察登记PASS；source_ready_GO范围仅唯一已审darwinarm64 profile/two OpenAI Responses endpoints及production source+deterministic runtime/receipt/verifier，不宣称其它protocol已批准。8771source58rawlinks/0mismatch、3865compiler4104raw152transform86builtin348package、65实际named真实log/exit0 reused同源；原full失败/两reader错误及过时aisdk helper路径失败保留。三realmodel session全FAIL/0字段/HTTPretrycountUNOBSERVED，0新模型/改producer/global源码或安装；SourceA实际服务字段UNPROVEN/archiveGO=false；原OG gate不放行。后审仅精确事实修订：Bedrock producer解码/聚合/reencode而Libra hash存储string不改写，补实际schema event/message/native-origin源SHA（旧core两shim明确），单独保存原IsADirectoryError metadata，填actualreview/source_ready结论；无runtime/source/test/assertion变化。DEFER-NPR12-REVIEW-R1承接原NPR08 twoP2及NPR11 13nonblocking计划外UNFIXED；不以统计绿色代native。现在signedDCO exact两plan Push main（private报告inline携带），formal16/61=26.23%直到NPR20公共C/D；下一NPR18迁移，sharedharness按现行AGENTS ER13在本卡实际full，历史card延期措辞届时修订。TEMP44冻结68e等待wholeCI、未tag/release/install。


NPR18 start 2026-10-11T15:17:18.160260+00:00: consumeNPR12 actualsingleClaude104PASS/source_readyexactprofile/signedmain edb9147。Recommended necessary精确修订：currentacceptance pin2.0.26-libra-native.1、officialhistory/base2.0.26/9b4ec571独立，不修改旧JSONfixtures/OG00block/已审observertemplate/catalog/privateproducer；新publicdescriptor绑定实际artifact/source而不冒称base含patch。Mac smoke explicit separatebinary opt-in+当前version/SHA漂移拒绝、全child选中binary一致，保留Linux运行逻辑供用户后验，禁止自动globalfallback/模型请求。现行userAGENTS SharedHarness ER13要求本卡canonicalfull，修订旧card延至20条句；stale不存在的mandatorynamedguard改currentactualsmoketarget非ignored全跑，不用0testPASS。15rootowned+backendagentonlysnapshot，原05/a705unstaged及backend其它3userdocs保护；source/focused/full/actualClaude/Push UNRUN，formal16/61=26.23%，SourceA UNPROVEN/archiveGO=false，TEMP44冻结68e等待wholeCI，0新models。


NPR18 source/local final 2026-10-11T15:53:17.772228+00:00: actualsingleClaude50turnsPASS/0P0P1/rawSHAc03d0be0c87e48e99430bf3b811c5a5eb96163a7580befcd2ada40f25951326b；84frozen/15root+backendbytesmode父级MATCH、原05/a705及backend其它3dirtydocs unstaged原字节保持。六hookexact各1PASS、37独立related/43executions/fourpaidignored、fmt/strict0；actualprivate187MB hash与--version正例已测，不调model/login/register。canonical双envfull实际9776PASS/0FAIL/13slow/1flaky/4leaky/10skipped，execution849.126s，原history lease TRY1FAIL/TRY2PASS不可抹除。DEFER-VALIDATION-HISTORY-LEASE-01和DEFER-NPR18-REVIEW-R1七P2均outsideplanUNFIXED；无source/断言/skip/filter变更，无新NativeProven/archive/模型、SourceA UNPROVEN。历史OG00sourceblock/五JSON/稳定observertemplate/approvedcatalog/codec/verifier/privateproducer/globalOpenCode全部unchanged；Mac显式privateoptin/currentSHA/version/size漂移拒绝、Linux用户后验。现仅两plan事实更新后signedDCO exact15 mainPush；NPR18 C/D归20，family source/local19/21=90.48%，正式16/61=26.23%不提前增计，下一NPR20最终聚合与source-ready finalbinary真实permission/signature门。TEMP44 frozen68e仍等待wholeCI，未tag/release/install/website部署。


NPR20 serialstart 2026-10-11T15:56:56.824522+00:00: dependencyNPR18 actualsignedmain17f6273/remoteMATCH、sixnew+37related/full9776PASS/13slow/1flaky/4leaky/10skip、actualsingleClaude50PASS/84frozen15rootMATCH。family source/local19/21=90.48%，formal16/61=26.23%不提前计。7rootowned执行纯patchmain44→45预留（lateststable43，TEMP44 owned68e已预约未tag，保证actual44C/D先于45）；所有Rust/test/privateproducer/observer/catalog原字节保持，1454implementation entries用于独立同源对拍；用户version-only full复用例外仅同implementationMATCH后采用，source变化重full，实际finalCLIpermission与同源compiledRustpreservedreceipt/signature/currentauthority门必须分别实跑并准确标artifact，不宣称CLI已有archive入口。必要基线/旧dual审措辞以当前singleClaude/两级GO精准更新，SourceA UNPROVEN/archiveGO=false，无新模型/global升级/Linux功能；currentstage全验收/aggregateClaude/signedPush/publicCI/四platform/release/install/site未完成。TEMP44继续等待wholeactualCI，先推进source。原05/a705unstaged及backend其它3保留、NPR18agentappendonly尚待20site，原stash不动。


NPR20 qualifiedsource-ready local 2026-10-11T16:29:16.806864+00:00: actualsingleClaude68turnsPASS/0P0P1/rawSHA1626169d7c3b0f1cd13669f7e0acfb80081e86521b809ceb8e53ab22b77f3a57；106frozen/7root/actualCLI+8testartifacts parentindependentMATCH、sixLibrachildren最终actualClaude raws重新hashPASS。actualfinalCLI0.30.45 SHA382208680f59b17c91ee8a7dcb2480c3843415b2e46aa3811977f3a78fefa320/187795384bytes、eightactualcurrentpermission tests；samefinalsource separate libtest runs7ring/originalreceipt/source/field/runtime/currentauthority/sealedtype/no-factory gates，实际私有187MB authority正例已跑但key/receipt synthetic。versionfaces2PASS、fmtstrict0、1454entries对拍1450same+4纯selfpatch，无Rust/test/config/CI/privateproducer/依赖变化；根据user versiononly例外复用实际NPR18full9776/0FAIL/13slow/1flaky/4leaky/10skip，不假称重跑或首轮全绿；plan finalfull仍必跑。原CLI-r1漏test-live-agent target gate实际101/0tests保留，r2 eightPASS；emptyfeature无src使用，helper为postrepair、receiptargv权威。CargoLock初始手改保留并原lock通过cargo update--workspace--offline exactregen45，无dependencydelta。后审建议仅事实docs补requiredfeature+4localpaths/actualtagwindow与installedhalfpending、清旧37constraint，workplans600不变parentGitmode100644实际验证，新commitmode待核。qualifiedSOURCE_READY_GO仅已审darwinarm64 OpenAIResponses机制，最终bumped门通过，installedoptimized45 artifact/currentpermission/CD仍UNRUN，SourceA UNPROVEN/archiveGO=false/nativeartifacts0/newmodel0；无publicCLI verifier入口，不能宣称unit就是releaseCLI。当前5source谓词勾选、publish留空、family source/local20/21=95.24%，formal16/61=26.23%等待真正wholeCI/tag/stable4platform/installer/site与20完整CD。TEMP44只frozen68e wholeCI PASS后发行，先44actualCD再核actuallatest发45；不混入source18/20、不发PR或其它Session消息。下一signedmain exact7sourcePush→NPR14 qualifiedhandoff，原05/a705/backend其它3/stash保留。


NPR14 handoff draft 2026-10-11T16:44:23.802111+00:00: actual NPR20 signedpublicmain ba102974、qualifiedClaude68PASS、17非零机制门/currentdebugCLI45及separate verifierlibtest artifact逐SHA复核，sole approved darwinarm64 2.0.26-libra-native.1 build bac21cc与547c36 binary复测，原policy/issuer/codec/endpoint/renderer/coordinate-originalUTF8field/taint/revocation规则列明。actual optimized45manifest/installedpermission/publicCD未知或UNRUN，SourceA UNPROVEN/archiveGO=false/新model0。当前仅twoPlan/private同名npr14四谓词draft，actualsingleClaude/fmtstrict仍UNRUN；DEP11仍需NPR20实际done/complete，RGsix未完成仍hardblockedtyped，不启动carrier/factory/pending生产，OG05单一factoryowner与FASTOG04最终archive门不变。formal16/61=26.23%，sourcefamily20/21直到14实际review/push，不勾未完publicAcceptance；FASTOG02继承C-D仍pending。无Rust/test/installer/producer/backend写入，跨plan04/24状态未变不写，05/stash保留。


NPR14 actual qualifiedsource handoff 2026-10-11T16:51:39.185763+00:00: soleClaude68turnsPASS/0P0P1/rawSHA1e1b88554944cb50275af5c43079289fc485afb257ff4c3eed9a726823e713ae；70frozen/2root及actualprior receipts/testCLI/approvedproducer byte-mode独立复hash全MATCH。四manual源谓词实际PASS：oneapproveddarwinarm64artifact/closedissuer-policy-codec/17非零机制门+actualsignedpublicmainba身份；OG05future唯一factoryowner不改；原DEP11需要20actualdone/complete+RGsixcomplete硬门保持NOT_CLOSED，sourcehandof不反向等archive也不新增循环；SourceA UNPROVEN/archiveGO=false/no nativefields/新model0，失败/synthetic/未安装优化artifact不计真来源。fmtstrict实际0，docno-release无fulltrigger且无新full声称；后审仅status模板locally-accepted、actualledger、receiptmode/byte与immutable wire2fixtureSHA，无Rust/test/installer/backend/producer/source变动。signedexact2sourcePush NEXT后sourcefamily21/21=100%，formal16/61=26.23%仍待publicfamilyCD/14 FASTOG02，无C-D提前complete。下一先TEMP44冻结68e wholeCI PASS后actual签tag/stable4platform/exactinstaller/site，再45installedpermission/finalpublicCD；blockedtyped/factory/pending不启动，原05/stash不动。


## 2026-10-11 NPR10 / temporary v0.30.44 actual public completion

NPR10 done/complete after source185focused/full9760/Claude54PASS and patch+websiteClaude68PASS, actual source68e baseCI38145394138 eightSUCCESS, safetyactualSUCCESS, release38161948711 eightSUCCESS/four-platform signedmanifest and carrierSHA/sizeMATCH, stableGitHub v0.30.44, humanexactcurl installation verified0.30.44, Homebrew verification artifact, websitecf ea45063/Cloudflare79a60c6c-fc85-434f-907f-dda5be7d4aca100% and liveHTTP200/fiveownershipmarkers. Proof `/private/tmp/libra-main-resumption-20261010/temp-npr10-patch-r1/public-CD-final.json`. Formal17/61=27.87% (onlyNPR10 newlyclosed). FrozenNPR20ba/45 actualwholeCI stillqueued/no45tag; NPR14 remainsFASTOG02 child. SCOPED02 candidate11prefixPASS and currentreal immutableoldsource firstpassCLIred101/newgreen0; expandedr3 actual104PASS/1docguardFAIL including1LEAK, fixes and revalidationpending. Original05/backend4 source/userdelta and immutable04helper preserved. SourceA UNPROVEN/nativeGO=false; allNPR21source/local ready is notallformalclosed. No models; paidGLMembargo unchanged; Linux user-later.

2026-10-12 FIX-RG-SCOPED-02 source/local D accepted: current owned13 paths, actual full9778PASS/0FAIL/0retry/10skip/1既有SSHLEAK, fmt/strict0/docs22PASS, singleClaude55PASS/0P0P1/60freeze MATCH; exact frozen ba CLI同test red101→currentgreen0。DEFER-RG-SCOPED-02-REVIEW-R1 四P2/五P3计划外UNFIXED，HashKind线程局部P3不适用。网站原3browser实际10PASS5FAIL与未改发布基线相同；DEFER-SCOPED02-WEBSITE-LEGACY-E2E-01计划外未修复，保留原断言录像，新owned agent browser1PASS HTTP200/typecheckbuild0/原docsunit10PASS，最终FASTRG在线C-D仍未执行。本轮source/local acceptance非formal complete；原05和backend4保护，SourceA UNPROVEN/Linux用户后验，无新model。下一SCOPED03，正式17/61=27.87%；NPR20 frozenba/version45 wholeCI继续待真正完成，不提前tag/release/install。


## 2026-10-11T21:19:01.303291+00:00 FIX-RG-SCOPED-03 source/local D accepted

Actual single Claude Code R3 PASS46/0P0P1, raw3302bc62f9919491012fdce786f3cd2f70d6d6def6003453d8526c8bdd677a7a; 121 frozen/live byte+mode inputs MATCH. Two prior NO-GOs and their rejected-source full runs remain preserved. Both P1s are forward-fixed: valid own terminal state with matching manifest terminal=None (state-first interruption), and review infrastructure Error with empty state rows/requested reviewers retained in manifest. Nonempty reviewer rows and all schema/kind/run/start/scope/non-null-terminal conflicts remain checked.

Final exact-source canonical double-env full-r9: Summary [ 841.952s] 9788 tests run: 9788 passed (13 slow, 3 leaky), 10 skipped; actual exit0, raw2e7074b9fe62a1dd00ceebd9c58946d0af3b7d075acc7257b3d2e60fdf6e91e4, inventory880c7de1fffbd53884dfe686bb2ecdf4cfff650d4aed5c976e6dfe1867143259. Related final gates81PASS/0retry, all23doc cases, nightlyfmt0 and strict all-target/all-feature clippy0. Same compiled fixtures run on source-bound old binaries RED and current GREEN; real synthetic completed readonly --run/all and actual infrastructure Error --run/two review --all branches pass. Investigate public --all is one locked per-run sweep; its direct library bulk has separate unit coverage. New Error test compares every schema object/all logical rows of the actual database; all original physical-byte/privacy assertions remain.

Private website final owner d3ba4d7a06ce01dca25bf879339ae8f97fed53033ab7a9bb4eb3fabb70b50e09: actual typecheck/build0/docs10PASS/owned agent page1PASS HTTP200, trace/video retained. Prior original3-spec browser matrix remains10PASS/5FAIL, identical unchanged published base; DEFER-SCOPED02-WEBSITE-LEGACY-E2E-01 outsideplan UNFIXED, not blanket browser PASS. Public website and FAST-RG C/D remain pending.

DEFER-RG-SCOPED-03-REVIEW-R3 outsideplan UNFIXED: one P2 unparseable manifest refusal/whole sweep abort (now explicitly documented); three P3 doc placement, missing exact pins for three recovery sentences, trailing blank line. Remaining R1/R2 nonblocking findings stay UNFIXED except individually described fixes. Existing namespace concurrency DEFER-RG-SCOPED-04 unchanged; extended consumer mid-phase race/depth/path injection beyond actual named/helper cases UNRUN, never inferred PASS. SourceA UNPROVEN/nativearchiveGO=false. Linux local functional DEFER-LINUX-SCOPED-03 user later UNRUN.

Source/local Deliverables accepted for RG-03 final; lifecycle in-progress/locally-accepted until FAST-RG C/D. Formal17/61=27.87% unchanged. Exact15-owned-path signedDCO ordinary main Push is the next operation; original05 unstaged SHA a705585dcb951a8d1452f87a607eefae1313d660b4c53c8d86ea75a024915b32 protected. Next RG-03, then RG-05, FAST-RG. No new model/native/secret access. Evidence /private/tmp/libra-main-resumption-20261010/scoped03-main-current-r1/source-review-parent-final-r3.json.


## 2026-10-11T21:22:27.190262+00:00 RG-03 current main final implementation begins

Consume actual SCOPED03 accepted source/localD and signedmain52f9cc26/remoteMATCH. Four actual production owners across command/agent and observed_agents; existing skill/shared reader/cleanup callers stay byte-identical. Add --reasoning-artifact, audit-first independent action, metadata-only optional JSON projection, complete role/OID/digest/length/cap verification, private no-clobber Unix output and empty decryptor registry. Six implementation/test candidate files applied only after exact preimage checks; docs applied additively to current SCOPED03 root, never stale full copies. No new Cargo target/dependency/stable code/SQL/key/model/native gate. All new focused/full/Claude/publicCD gates currently UNRUN. Native SourceA UNPROVEN/archiveGO=false, Linux userlater, formal17/61 unchanged.

Necessary recommended corrections: nonexistent agent_reasoning_artifact_test target replaced by registered --lib command::agent::checkpoint::tests::reasoning_artifact gates; actual skill hookup already exists, so no invented fifth production edit. Registry/reader/writer/audit/skill regressions remain. Shared export/public error documentation triggers a fresh final canonical full before this card Push, with final FAST-RG full retained. Only actual single Claude Code PASS required. Windows private ACL export DEFER-RG03-PRIVATE-ACL-01 outsideplan UNIMPLEMENTED; cfg(not(unix)) route refuses/audits without lookup/body/file. macOS0600 delivered only after actual gates. Rollback must preserve opaque role exclusion and audit history; cannot expose cipher through ordinary raw transcript.

DEFER-SCOPED03-FULL-LEAK-01: actual full-r9 exit0/9788passed reported three LEAK warnings in unchanged existing codex_transcript_root_honors_codex_home_override, empty_artifact_set_manifest_is_byte_identical, porcelain_v2_z_rename_record_nul_paths. Raw2e7074b9fe62a1dd00ceebd9c58946d0af3b7d075acc7257b3d2e60fdf6e91e4 retained. Outsideplan UNTRIAGED/UNFIXED under human deferral policy; no inferred root cause or pretend fix. SCOPED03 current source/main localD valid, FAST-RG publicCD pending.


## 2026-10-11T22:02:39.131369+00:00 RG-03 source/local D accepted

Actual sole Claude Code PASS64/0P0P1; raw6ce7e4bd3d386aeebfdc5966f6575fffe779410943386ebd37cbb7eebb49d9e0; 76 frozen/live byte+mode inputs MATCH. Four production behavior files/two landings. Shared checkpoint_reader/input, existing skill caller/skill tests, complete original reader integration tests and original top-level writer tests stay byte-identical. No SQL/stable-code/provider decryptor/key/network changes; actual zero-hit key/network guard exit1. SourceA UNPROVEN/nativeGO=false.

Actual related final118PASS/0FAIL; first115PASS/2FAIL preserved and forward-fixed (nonempty wrong-type fixture and legacy unhydrated root). Missing root only preserves old safe show summary/omits optional artifact array; present corrupt/invalid layout still refuses; artifact export remains strict. One existing cursor LEAK and8344 selection skips retained, not acceptance. Actual final canonical double-env full-r4: Summary [ 842.988s] 9801 tests run: 9801 passed (13 slow, 4 leaky), 10 skipped; exit0/rawf90d6d4a949200a8d935708e4f46df1dc72d71b27037002338767e4fc2e9bfb0, 22-source inventory53b43f4e09cc4902d33722829ae07a30d5f41f9a037751867342265195289320 MATCH; actual retry/flake lines0 andLEAK lines4 recorded verbatim separately. Nightlyfmt0/strict all-target/all-feature0. Runtime/tests/public docs unchanged after Claude PASS/full; final followup only this three-plan facts/AC/VER.

Actual synthetic CLI verifies default metadata/ordinary body exclusion, seven-field array, complete byte-exact0600 no-clobber artifact file, separate granted/denied audit before lookup/body/output, audit failure, five permission/justification/output/mixedraw/lookup denials, existing file/link/dir/missing-parent preservation and full integrity matrix. Actual lib probes distinguish artifact body read0 from ordinary/proof positive; complete32MiB positive/declared overcap read0/undeclared overcap rejection, SHA/size/content-addressed OID/header/type/selected-checkpoint closure; registry allthreeNone. No truncated prefix or source evidence is treated as native proof. NonUnix export refuses/audits before lookup; DEFER-RG03-PRIVATE-ACL-01 outsideplan UNIMPLEMENTED, not Windows functionPASS.

Private owned website 2435f4c7191e8999e1c4aab280cd7c1c8dca04b5cf4e898e31620938f1c2954a: actualtypecheck/build0/docs10PASS/ownedagent1PASS HTTP200 withtrace/video. Firstworkerstartup1FAIL retained; unchanged scenario one retry on newport succeeded, rootcauseUNPROVEN. Broader legacybrowser matrix notrerun for this card; inherited DEFER-SCOPED02-WEBSITE-LEGACY-E2E-01 remains UNFIXED. Site UNPUSHED/UNDEPLOYED pending FAST-RG C/D with actualNPR20cf changes preserved.

DEFER-RG03-REVIEW-R1 outsideplan UNFIXED: oneP2 internal-treeOID showerrors/missing009 on present corruptroot; fourP3 ordinary wide-treebudget wording, malformedselector test coverage, optiontable gap and hardlink filesystem documentation. MaterialP0/P1 none; no pretend fixes. DEFER-RG-SCOPED-04 namespace concurrency unchanged, not reopened. Linux functional userlaterUNRUN.

RG-03 source/local Deliverables accepted for RG-05; formal lifecycle remains in-progress/locally-accepted until FAST-RG public C/D. Formal17/61=27.87% unchanged. SignedDCO ordinary main Push exact15 owned paths next; original05 a705585dcb951a8d1452f87a607eefae1313d660b4c53c8d86ea75a024915b32 and stash preserved. Next RG-05, then FAST-RG; no child version/release. Evidence /private/tmp/libra-main-resumption-20261010/rg03-main-final-r1/source-review-parent-final-r2.json.


## 2026-10-11T22:10:38.374422+00:00 NPR20 v0.30.45 actual public C/D complete

Frozen signedsource ba1029749c5907f689db7a24b430042dce0531e3, actualbase38155836501 all8SUCCESS plusCodeQL/QualitySUCCESS; signedtag9c9828fc4b1c6178befb3b86c3ea97a28b29ac0f verified; actualrelease38175211817 all8SUCCESS, including four actualplatform builds, signedstable manifest/installer/Homebrew poststeps. Whole envelope Ed25519 signature actualverified with compiled trustkey/domain; four actualdownload SHA/size match. Original downloader Windows suffix assertion blocked fourth; exact signedURL corrected, successful three original receipts reused only after hash verification; no Linux carrier execution or functionPASS.

Actual exact user curl HTTPS/TLS1.2 pipeline exit0 andstable signature verification; installedv0.30.45 Darwin payload78032240 bytes/SHA1ffb193424b2e05fba03a51b9401d5b0971bd0f3b7a66ef021f7c09bc44ac0d1. Installedgate actual8CLI PASS plus7distinct same-source optimized verifier PASS; frozen1452signed files/two localFinder metadata bound; newer continuing main source/index/HEAD andbothgeneratedCLI payloads preserved/restored. Separate crateprivate verifier evidence never claimed to be a publicCLI archive invocation. ActualHomebrew formula bec8fdc16caa9ad4c4ae1b08b92157db9903c2ac with matching buildrun/source/platform SHA+size report.

Website exactone ENowner3d204e708074b6afe2a24361e3a87695710ea1b16f22df23cb5f28b3657a122e (only reviewed1835byteNPR18 append to actual44source) actualsingleClaude67PASS/typecheckbuild0. SignedDCO cf7f18cf3b9cf4cd6b80b0eeee6355d370fe35f512 ordinarynamed Push, actualdeployment a0c9984a-06c8-4983-a9c1-a19803a87ea5/version5e63e2ca-7b50-4272-98be-41547b17b629 at100%; live200/eleven actualidentity+nativeorigin limitation markers. Read-only unsupported branchpattern129 anddetachedPush128 failures retained before correct ownbranchswitch/push. FirstHTTP200 check had an invented capitalized oldheading; actualreviewed unchangedsource heading matched onsecondreadonly GET; no redeployment/sourcechange. Rootbackend fourdirty preserved; no broadbrowser PASS claimed.

Actual qualified NPR20 SOURCE_READY_GO finalinstalled permission/signature half closed; source family C/D fulfills DEP-OG-11 NPR20 condition together with already-reviewed NPR14 sourcehandoff Deliverables. NPR14 formalC/D stillFAST-OG-02; RGsix formalcomplete prerequisite retained; OG05 remains唯一nativefactory/realfields SourceA hardgate, not released bySOURCE_READY.

Formal fixed61 queue actualcardtext recount: 17previous complete+18family members=35/61=57.38%;26 remain including currentdeferred. Newly formalcomplete: NPR-00, NPR-01, NPR-02, NPR-03, NPR-04, NPR-05, NPR-06, NPR-07, NPR-08, NPR-09, NPR-11, NPR-12, NPR-13, NPR-15, NPR-16, NPR-17, NPR-18, NPR-20. NPRBOOT01 additional structural child complete bysameactual08/20 C-D butexcluded original61 denominator. NPR10/TEMP44 andNPR19 alreadycomplete, no duplicatecount; NPR14 locallyaccepted only. RG03 actualsource/localD b7f4c2af/full9801/Claude64 remainsFAST-RG C/D pending. All named outsideplan review/flake/leak/namespace deferrals remainUNFIXED, SourceA UNPROVEN/nativeGO=false, authorized model quota embargo retained/no newcalls. Next RG-05→FAST-RG.

Release actualfinal evidence /private/tmp/libra-main-resumption-20261010/npr20-public-delivery-r1/public-CD-final.json. No newtag/bump/install, no runtime/test/dependency change at this metadata closeout.


## 2026-10-11T22:14:48.011091+00:00 RG-05 current main implementation begins

Actual prerequisite RG03 singleClaude64/full9801/signedmain b7f4c2af and NPR20 publicCD45/installed15/siteHTTP200 closeout49db. Formal35/61=57.38%; currentcard allnewgates UNRUN. Recommended exactwrite/test correction recorded inplan04; no phantomartifact test target, no SourceA/nativeGO/namespace04 change, no newmodels. Next complete artifact lifecycle then FAST-RG.


## 2026-10-11T23:23:52.597018+00:00 RG-05 final main source/local acceptance

Actual soleClaude Code source R1 PASS81/noP0P1/rawSHAab03ec6dcc52000240365a0b563327ff6dcd4eac82ad40e55c69ddc8938f9e71; frozen82/current29 bytes+mode MATCH. Final same source focused262/262 exit0, mandatory feature-selected realD1/R2 live1/1 exit0 (r6), nightlyfmt check0 and strict alltarget/allfeature clippy0. Private-helper actual IPC three hash kinds/role aliases/deadline/content-free proof covered. Cloud missing/size/digest faults after repair assert no generation complete; first absent object uploaded successfully. ActualCLI export/erase/list/show/graph/localGC zeroorphan/denied export and doctor digest/indexsize/type/missing with canonical checkpoint finding PASS. Consent literal and both real source call sites pinned; docs4 contract PASS. Eleven AC and five VER source/local mapped; no raw/native/provider/decryptor/physical remoteerase claim.

Fresh canonical双envfull actualexit0/rawSHAfca7edee94b6be66177e3a254d0c8be330fc45843c14d18fa6d0c99504fea685;      Summary [ 870.723s] 9810 tests run: 9810 passed (13 slow, 1 flaky, 2 leaky), 10 skipped. Firstattempt/retry and LEAK evidence retained verbatim in /private/tmp/libra-main-resumption-20261010/rg05-main-final-r1/full-r1.log and full-r1-observations-final.json; not called zero-flake/zero-leak. Named outsideplan followup DEFER-RG05-FULL-R1 keeps these unchanged-owner diagnostics UNFIXED, not a production-fix claim. Original ignored namespace reparent gates remainUNRUN; Linux localfunctional userlaterUNRUN, required safetyCI/fourplatform release remain.

Website actuallatestcf7f18cf3 preserved; onlytwo exact already-reviewed EA→RG03 predecessor hunks plus new RG05 lifecycleparagraph fromEN docs. Initial page gate failed due missing pending predecessor and failed adjacency inclusion attempt made zero filemutation; original assertions retained. Final offlineinstall/typecheck/build/docunits0, actualbrowser1PASS/HTTP200/screenshot/video/trace, actualsoleClaude websitePASS41/rawc76e44e; frozen27/currentownerMATCH. Private signedDCO source40ab64eb7c8306ec8c0cfe10032b813b92555d38, ownerSHAf8dd34a47fb98e7b74e9ce70b44d176565f5d8371a57c3b74294aad8b69c2596; rootbackend4 dirty and main preserved, publicPush/deploy NOTYET/FAST-RG immutable carrier.

DEFER-RG05-REVIEW-R1 outsideplan UNFIXED: oneP2 wholecatalog artifact-helper timeout/failure makes all nonlegacy checkpoints manual and suppresses unrelated automaticrepair; fiveP3 infrastructureerror wording, IPC-negative response-tag assertion, valid-artifact doctor positive coverage, wholecatalog rehash/output-cap scaling, INDEX selectedmissingcredentials vs featureabsence wording. DEFER-RG05-WEB-REVIEW-R1 preserves fiveP3 observations about browser spec outside reviewerfreeze, preexisting inconsistency-count/GC-window text, canonical-id redaction qualification and conditional websitezh lag. No source edited afterPASS. SourceA remainsUNPROVEN/nativeGO=false; no new OpenCode provider calls.

RG05 final local Deliverables accepted; signedDCO ordinary mainPush NEXT, formalcomplete only after FAST-RG actual C/D. Fixed61 formal35/61=57.38% unchanged. Next FAST-RG-01 aggregates ten qualified members, scoped04 only approved revised static scope; namespaceNO-GO remains outsideplan. Patchonly candidate0.30.46 if actualupstream still0.30.45; final implementation/full/securityCI/mainCI/fourplatform/artifactmanifest/Homebrew/install/website must bind actualsource before publication. Original plan05 SHAa705585dcb951a8d1452f87a607eefae1313d660b4c53c8d86ea75a024915b32 and stash preserved.


## 2026-10-11T23:32:50.284725+00:00 FAST-RG-01 final main release implementation begins

Actual latest stable v0.30.45 read-only verified; candidate patch0.30.46. Ten qualified source/local members now signed ordinary main, RG05 b4b7c3c actualsame-source full9810/singleClaude81+site41. Eight-path release writer only; runtime source unchanged. Public tag/release/install/deploy NOTSTARTED until actualsame-source mainCI/security PASS and required C/D. OldPR/dual/privatecommit-only prose is historical; authoritative USER-MAIN directmain/singleClaude/patchonly/CI-after rule applied in FASTRG AC/deps/VER. Scoped04 accepted revisedstatic API only, originalnamespaceNO-GO stays outsideplan; nativeSourceA remainsUNPROVEN/noarchiveGO, Linux operatorlaterUNRUN/safetyCI/fourplatform retained. Formal35/61=57.38% unchanged. Evidence /private/tmp/libra-main-resumption-20261010/fast-rg-main-final-r1/start.json.


## 2026-10-12T00:17:28.845612+00:00 RG-05 fixture forward fix / FAST-RG final source acceptance

Original bumped full-r1 actualexit100 after root-owned SIGINT: fixture doctor digest TRY1 unique object_index(repo_id,o_id), TRY2 PASS; interruptedobsolete run is NOT PASS. RawSHA2d7dbfef89ce2db04881b43b058e34c9b80ac45753ea817836b9ba0f12843221 retained. Originalowner RG05 changes only tests/helpers/reasoning_artifact.rs one SQL ON CONFLICT(repo_id,o_id) DO NOTHING; original ClientStorage background indexer can insert between fixture SELECT and INSERT. Other SQL errors and byte/digest/size/doctor/lifecycle assertions remain intact; zero production behavior delta. Before SHA d802c545382279d821839d566a2b7c5639207f41e2efd8cc8c4824f36ae2afd3 -> after SHA2a00c7814647fb0eefeef6ca91847b72a6170845708d42e281ca8d08385902e2; nativeblob88754c141d6b36914fd2cb855c23292f38425d52 -> c52ab2e29a033307e56b9858d4d33aa489240c16. SoleactualClaude fixture PASS20/raw27a4586b3678433347f9ce4822a7c085b7899f254cceb2a0cecaea4ad4abb2cb; focused14PASS, stress20/20iterations PASS with retries0/flaky-resultfail, actualD1R2 1PASS/143.63sec, fmt/strictalltargetsallfeatures/versionfaces2PASS actual0. Asynccomment/reverse-order boundedretry P3 observations remain unchanged, not repaired.

Final canonical doubleenv+childumask022 full-r2 actualexit0: Summary [ 863.096s] 9810 tests run: 9810 passed (13 slow, 1 leaky), 10 skipped; rawSHA745e8c039a336f87ab467ae52f5717f0839b21754eb47467f51c21e9ba9cb60f. Actualobservations retained: ["LEAK [   0.226s] (  57/9810) libra cli::tests::max_connections_resolves_from_flag_and_env"]. DEFER-FAST-RG-FULL-R2 outside-plan UNFIXED: unchanged-owner leak/flake observations are diagnostics under existing userdeferral, not zero-leak/flake-free claims. Current1LEAK cli::tests::max_connections_resolves_from_flag_and_env had no changedproductionowner; no testassertionfailure or flakyretry in finalr2. Fixeddoctor case passedfirstattempt; selected20stress nonzero20PASS and realcloud nonzero1PASS are distinct from fullskips.

Final FAST-RG soleactualClaude R2 PASS70/raw86fdf0e52ddc3036315ba05cf19968fb9fea5f700b48bc1800107e47a44830db, 0P0/0P1, frozen58MATCH. P2 nativeOID evidence correction CLOSED: originalsource-r2 contentSHA/mode and fullfreeze were correct, but helper native_blob accidentally retainedoldOID; original r2 preserved and canonicalr3 independently recomputedall2024actualblobOIDs. CorrectedinventorySHA978ff18cb2a0b402e8dd4fa48cbc79dbd2ccee5480ae49471005dcc95f117be0; originalfullinventorySHA586a0c585cf5a35b990baa726318a602c5d4c7ef0a7a34ad9387dd45824e6b10. P2 ownerprovenance closes only with ensuing distinct signedDCO RG05 helper+state commit and exactinclusion ledger before pointPush. Parent records actualhash closure only, no Codex review.

RG05 stays locallyaccepted and tenRG/scoped localdeliverables stay10/10; FAST-RG source/full ready, publicC/D NOTSTARTED. Next signedownerhandoff -> pointpatch0.30.46 signedmain sourcePush -> correspondingexactsource CI/CodeQL/Windowsfour PASS -> signedtag/fourplatform/stablemanifest/Homebrew/exactcurlinstaller/websitecf+deployment+HTTP. No release beforeCI; allfuturepublication remainspending. Formal35/61=57.38% unchanged. SourceA UNPROVEN/nativeGO=false; concurrentnamespace originalNO-GO stays outsideplanDEFER/UNFIXED/notexecuted; Linux function userlater, safetyCI retained. Root05 originalSHAa705585dcb951a8d1452f87a607eefae1313d660b4c53c8d86ea75a024915b32/mode600/stash unchanged.


## 2026-10-12T00:19:31.280304+00:00 FAST-RG-01 final immutable inclusion / signed source push preparation

Original RG05 fixture forwardfix actual distinct signedDCO main commit 457ac8ecc9b91c5c40bc1203fcbbfc797c460ab3, parentb4b7c3cb9f870e369edc2e82efbbd276368e8091, exactonlytests/helpers/reasoning_artifact.rs; localvaultcrypto exit0/rawnativeOID and SHA2a00c7/c52ab2MATCH. Mandatory finalfull-r2 actual0, soleactualClaude fixturePASS20 and finalpointPASS70. P2 ownerprovenance CLOSED through explicitownerhandoff and tenmember inclusion-r2; nativeOIDmetadata P2 CLOSED through actualall2024correctedr3, oldr2/FAIL/interruption retained. No newbusinesslogic bypoint, no secondCodex review.

Actualsource/full/local ready; fourfaces 0.30.46, privateRG05 website40ab64eb/f8dd preservesbothlatestcf7f18NPR20appendix and exacttwoSCOPED02/03/RG03reviewedhunks. Point ordinarysignedmain sourcePush NEXT, exactfinalsource baseCI/CodeQL/Windowsfour and publictag/release/install/Homebrew/website C-D stillUNRUN/PENDING. Formal35/61=57.38% unchanged; tenRG/scoped formalcomplete onlyafteractualFAST-RGCD. Next afterpointCD OG16 typedsource seam, thenOG23/19/20; no prematuredependencyGO. SourceA UNPROVEN/nativeGO=false; userLinuxfunctionafterward/safetyCI retained; oldnamespaceNO-GO remains outsideplanDEFER/UNFIXED/ignored.

- plan-20260902：`FIX-CI-MACOS-OWNER-01` 发布前辅助入口修复归FAST-RG-01 root publisher，固定61卡外；base macOS owner job绿色但实际0测试，不能PASS。精确CI/guard写集与新固定0.30.46 source重验义务见FAST-RG-01；修复尚UNRUN，不阻source-qualified后继开发。

- 2026-10-12当前 `FIX-CI-MACOS-OWNER-01`：已补13 exact实际macOS入口与两项matrix_alignment守卫，local13/13和2/2实际PASS，零测试/选错/ignored/命令失败均拒绝；全量/Claude及新准确CI未完成，不计complete。OG16首次main Push在该CI source冻结后，后续不等Release；正式35/61保持。

**2026-10-12 FIX-CI-MACOS-OWNER-01最终local门：** 双env canonical Nextest实际exit0，9812 PASS（13 slow、2 flaky、2 leaky）、10 skipped，原首败/LEAK名称和日志完整保留，rawSHA256=3a593d5b7b04818f4ac71c89ed9db11a07ed4e359896570e608f9051ad0ce12d；不能表述0flaky或0leak。该修复没有生产改动；13真实macOS exact及2入口守卫通过，focused全部matrix11/architecture32/docs25、fmt及strict clippy exit0；实际Claude R1 PASS/0P0/0P1，原P2的CLI形式已用Libra只读核实，其余非阻塞限制登记。当前source签名main Push与冻结NEXT，准确push/main CI及0.30.46所有公开C-D仍UNRUN，不计formal complete；正式35/61=57.38%。OG16原型未混入发布source，后续开发不等Release。

2026-10-12T04:13:57.289963+00:00 OG16 current: 60AC/24VER实际macOS PASS/finalfull9838/Claude48+34，private网站46fbc7签名且未发布；locally-accepted，formal35/61=57.38%保持，下一OG23。2026-10-12当前固定f03发布CI attempt2 compat-offline-core测试步骤actualexit100，OTLP/keyring/upgrade实际skipped；job日志404、attempt归档emptyZIP，具体失败用例/原因UNPROVEN。旧attempt1runnerlostcommunication保留，不能套作attempt2原因。0.30.46未tag/release/install/deploy，保留发布硬门；source卡按noWait继续。 SourceA/native archive仍UNPROVEN/false。

2026-10-12T04:15:17.116770+00:00 FAST-RG46 private publication notes execution handoff: 实际soleClaude notes R2 13turns PASS，rawSHA a136c35c955697a8d52cd133e1ee56b8b466378758f30beb567e4fc56021fad5，six frozen/current输入MATCH；唯一后续执行入口publish-github-stable-r2.py SHA 4fb484ad3e71671a186424e7a5b6be02b3b68b42017963daae075bb538a0b344，原r1保持历史byte-identical，不执行。仅两factual hunk修正f03实际full9812 summary/receipt校验与notes渲染，未弱化CI/source/tag/四平台/signature/latest/duplicate/readback门；不会引用本OG16 full9838代替f03 full。DEFER-FAST-RG-PUBLICATION-NOTES-R2计划外UNFIXED：summary数量未机械比对、继承stress/D1/security事实需其它原qualifiedproof、历史r1输出basename复用只可exclusive执行、firstfail次数措辞。当前f03CI attempt2 actualtest100，release_GO=false，公开tag/release/install/deploy仍未运行。

2026-10-12T04:20:50.764226+00:00 OG16 signedmain62b508b/remoteMATCH source-local交接完成；OG23 in-progress，原8AC6VER未运行，whole-scope GC护栏先于identity writer。formal35/61=57.38%不增，SourceA UNPROVEN/native_GO=false，0.30.46因f03CI测试100保持未发布，继续noWait源码开发。

2026-10-12T05:17:43.430399+00:00 OG23 final source-local macOS: original8AC/6VER closed. Actual VER1-5 R1 each1PASS, strengthened VER6 R2 actual1PASS; unchanged production SQL guard, only AC8 repaired private value gains real private_oid. Actual preimage VER2 RED exit101/1failed preserved; earlier test-only compile101 not red proof. Maintenance22/docs27/serial25 PASS; fmt/strict current R2 actual0. Final canonical R2 Summary [ 875.739s] 9845 tests run: 9845 passed (14 slow, 2 flaky, 1 leaky), 10 skipped; rawSHA a28d150d0f9636a9f430465de8c5a931257c5543d2154668155d84b926fbffa8, inventorySHA 0133511972687f9072720dbbefb37e1b2f968da126df8d2f6d3dadc0d179884b, 2024 tracked source byte/modes unchanged during run; original six tests each actual final-full PASS. R1 full9845/1flaky retained, DEFER-OG23-FULL-R1 outsideplan UNFIXED, exact mega2_browser_mkdir failure_keeps_last_listing_and_never_leaks_the_token firstFAIL thenPASS; root cause UNPROVEN. Actual sole Claude R1 PASS47/raw7d44661941f0ace1ab7a7a86a22e62421ed2a7c28d92b231668444881c4f98c1 and R2 PASS55/raw5f09ada80a636956398c91d6285fa4a482fce6ff72bc147cd3b0c669a8191f5c; R1 AC8 vacuity fixed and copied-mode observation resolved by actual root preimages. DEFER-OG23-REVIEW-R1/R2 outsideplan UNFIXED: reserved-scope literal, internal inventory/cosmetic note, nonblocking extra corrupt-before-repair retention assertion and probe-route excerpt completeness; no new defect asserted from scanner/CLI uncertainty. Private website 4c61934b106999a17028c3054341ccc51894a156 signed-DCO/crypto0, ENsha6e4726280a0d9ee3bd9f05ef2ed691a02f0d4f240c4b529987a262871d565ab7, install/typecheck/build0/unit10/browser1 HTTP200/22markers; unpushed/undeployed FAST-OG-02 handoff, excluded frozenRG46 source. Shared source only one production landing maintenance; no enum/codec/writer/key/native activation/schema/new target/version/model calls. Protected05 SHAa705/mode600, stash/backend4 preserved. Formal35/61=57.38% unchanged, FAST-OG-02 C/D PENDING, Linux function userlater UNRUN; SourceA UNPROVEN/native_GO=false. Source signed main handoff NEXT; then OG19 all91AC31VER, no narrowed helper-only scope. Frozen f03 release46 CI attempt2 actualfail/test100/skipped extra feature steps, stable45 verified, no tag/release/install/deploy and no inferred failure cause. DEFER-OG23-FULL-R2 outsideplan UNFIXED preserves actual residual lines; root cause UNPROVEN, no skipped/failed run counted PASS.

2026-10-12T05:26:11.916500+00:00 OG19 start: actual predecessor OG23 signed-DCO main487cf5edde1f12f9e46287bcd88f8cbdfa7a841c/remoteMATCH, original8AC6VER/currentfull9845/Claude47＋55 closed; private site4c619 sourcehandoff. Original91AC31VER intact, nine production owners/two cfg(test)-only maintenance-cloud owners, no new test target/schema or reasoning.rs production edit. Existing-only identity/record HMAC entry first; all new tests/full/Claude/Push currently UNRUN. SOURCE_READY mechanism only, SourceA UNPROVEN/native_GO=false/modelcalls0. Formal35/61=57.38% unchanged, FAST-OG-02 C/D pending; frozen f03 release46 failure/test100/no artifacts/log404 remains unproven cause, lateststable45. Original05/stash/backend4 remain protected. Next full keyed per-turn identity and actual reserve/store/erase/retention/live-import partial consumers, not a helper-only subset.

2026-10-12T05:45:49.559529+00:00 OG19 first primitives actual: VER10 independent identity/record/pending/allthree source HMAC domains plus keyless no-create control actual1PASS/rawbce254b130d35ceb6fb2661ce8e60859ad98d8ef16fedb3d97a9ed0fcda8af8e; VER28 closed versioned private codec/context7-axis mismatch/unknown fields/duplicate version/wrong domains/payload-free diagnostics/debug privacy actual1PASS/rawbfd39b71c091160ed2abd303a7ac707b4a24a457c497d222fec4d9ecfff4e1a0. Only key.rs and metadata.rs production touched; prefix SST stays key owner, existing pending/source domains unchanged. Codec synthetic MAC strings are syntax-only, NOT actual cryptographic record authentication/native proof; actual scope-bound key API consumers/reserve/store/ref transaction/erase/retention/doctor/live/import partial flows and remaining29 exact names not yet implemented. Original91AC31VER stay unchecked until final all-source acceptance; interim unused API warnings expected, final strict/full/soleClaude/sourcePush UNRUN. SourceA UNPROVEN/native_GO=false, modelcalls0, formal35/61=57.38% unchanged, no release46 bypass.


2026-10-12T06:32:45.632514+00:00 OG19 interim R4/R5 actual macOS: original named tests implemented12, each actual1PASS (8 coverage identity comparisons + key domain + closed record/session-map codec + actual CLI doctor + actual history erasure). R4 current coverage-related26PASS; original doctor conflict and OG16 source-preparation rejection controls each1PASS. R5 history erasure exact1PASS/raw42c096fc882a308373d25b5be423f159d714bad1a2b7ee99dd34978bab658f4b plus two existing keyless/pending erasure controls each1PASS. Each test receipt bytes/modes unchanged; seven R4 owners remain identical after history-only R5. Implemented sealed per-turn metadata descriptor, existing-only independent key domains, authenticated bounded session map with duplicate-slot rejection, actual reservation identity comparison/forced conflict despite equivalent second redaction, mixed-partial transaction rollback, fenced SQL companion, fixed doctor reason and keyless local-owner session map erasure with actual rollback/late-writer/other-owner controls. This is NOT all91AC31VER acceptance: actual shared-store atomic append, whole-None candidate-free prefix lookup, full original matrices/budgets/retention/privacy tests and actual live/export/import consumers still pending. Preparatory unused APIs retain interim warnings; finalstrict/full/soleClaude/mainPush UNRUN. No model calls/issuer/locator admission; SourceA UNPROVEN/native_GO=false, formal35/61=57.38% unchanged, next remains OG19 integration before OG20. Current signedmain487cf CI security/core/mac fmt+clippy success, Windows selected fixture compile fails due session_row Unix-only helper dead_code and network LFS actual TLS handshake EOF; helper cfg(unix) corrected in current owned live source, Windows rerun UNRUN, transport root cause beyond EOF UNPROVEN. Frozen releasef03 CI attempt2 remains failure/test100 and log404; read-only recheck still404/215bytes, no rerun/tag/release/install. Protected original05 SHAa705/mode600 unchanged; no staged files. Evidence: /private/tmp/libra-main-resumption-20261010/og19-main-r1/interim-source-progress-r5.json.

2026-10-12T07:16:56.679762+00:00 OG19 interim R6/R10 actual macOS: original named tests implemented with nonzero PASS now14 (not all91AC31VER closed). VER9 actual keyed candidate-free legacy prefix and whole-None reader1PASS/raw9bc60aad854bca44208014150edcd3a4dc78d53796c19cab926edf0190368350; VER22 actual import handler all-skip, mixed and cross64 bounded batch partial1PASS/raw12b34923541b2106623503589cc95ecb26ac4ee42c45a8c8038826a29b4069f1. Final focused current CG27PASS/rawc337d44f93ec5ad605bcfab5d5e0a8fd5ddaba83e96501a13d393bb3fdb287e8 and agent_import33PASS/raw4c602ca03f563b81b2177ab1b84c49853a146e0882faa1134821f12f747df359; fmt0/source bytes-modes MATCH/protected05 MATCH. R7-R9 fixture failures retained as failed, corrected fixtures do not loosen production validation. Ordinary imports now use existing-key scope reader and preserve reasoning partial before no-op/child parsing; fenced import finalization partial with cursor unchanged/no subset checkpoint; earlier bounded batches abandoned without completing. NativeProven import/live/export and actual shared-store atomicity/full matrices remain pending. finalstrict/full/soleClaude/mainPush UNRUN. New human local-model authorization overrides older no-model-call scheduling for this exact provider only: endpoint200/exact model present, actual two isolated CLI sessions using deepseek-v4-flash-vision/deepseek-v4-flash-vision-uncensored succeeded/export parsed/READY, installed OpenCode2.0.27 plus approved private2.0.26-libra-native.1. Actual ordinary reasoning text88/57 UTF8 bytes; signature/redactedData/reasoningEncryptedContent all absent in both exports. No native issuer opt-in, SourceA UNPROVEN/native_GO=false/no Libra native artifacts; global config and source unchanged during observations, official/private acceptance pin not silently changed. Evidence /private/tmp/libra-main-resumption-20261010/og19-main-r1/interim-source-progress-r11.json and /private/tmp/libra-main-resumption-20261010/local-deepseek-observation-r1/model-authorization-and-results-r1.json. Formal35/61=57.38% unchanged; next OG19 remaining integrations before OG20; frozen46CI block unchanged, Linux user later UNRUN.

2026-10-12T08:09:42+00:00 OG19 interim actual macOS after user deferred three native fields: recorded DEFER-OG05-NATIVE-FIELDS; OG05 native archive remains blocked/SourceA UNPROVEN. Production now routes both live and export through the same scoped, existing-key reasoning identity reader and typed reservation result; a known partial exits before covered-noop/checkpoint continuation, and a mixed partial batch rolls back its newly reserved subset. Actual `live_and_export_reasoning_reader_preserves_all_skip_and_mixed_partial` 1/1 PASS; `live_checkpoint::tests` 6/6 PASS; `coverage_gate::tests` 28/28 PASS. Expanded `reasoning_identity_commit_is_atomic_with_claim` 1/1 PASS across live/export/import; injected identity-row and revision-row transaction failures leave ref/catalog/claims/map unchanged, then success commits them together. Prior attempts retained: test compile failed on missing Utc import; fixture query failed on nonexistent `agent_reasoning_artifact_index`; store attempts exposed invalid fixture timestamp and helper-required deadline, all corrected without relaxing production checks. OG19 original 91AC/31VER not closed; candidate-bearing live/export producer handoff, terminal/nonterminal finalizer matrix, remaining erase/retention/privacy/cap/deadline matrix, strict/full/Claude/main push still pending. Overall formal progress remains 35/61=57.38%; no Linux functional acceptance or release claimed.


2026-10-12T09:18:00Z OG19 R12 actual macOS progress: user deferred real `signature`/`redactedData`/`reasoningEncryptedContent` provider/native integration pending a provable connection path; SourceA remains a hard partial/no-archive gate. `cargo test --lib reasoning_ -- --test-threads=1` actual71/71 PASS before VER20 was added; rerun pending. Exact named VER 29/31 actual one-test PASS; VER20 forward recovery fence preservation now PASS after reopening SQLite and checking persisted identity/fences plus fail-closed unproven redelivery. Only VER21 live/export actual handler finalizer matrix and VER30 legacy bounded-finalizer matrix remain zero-run/unimplemented. New VER31 actual live→import shared-store test PASS covers same stable descriptor, SkipCovered, wrong-turn rejection, and changed-carrier conflict using synthetic carrier only. 61 AC mapped from passed VER are now checked; formal plan progress remains35/61=57.38%, OG19 in-progress; no source push/release/Claude review yet. Formatting check PASS; all-target strict Clippy rerun underway. Local opencode now reports v2.0.27 while plan source pin remains v2.0.26; no OpenCode source baseline or native field claims changed in this OG19 work.

2026-10-12T11:16:37Z OG19 R13: 已复核 CI，未发现依赖本机 OpenCode/账号/模型响应的 runtime 测试，因此没有要从 CI 移除的测试；受 `test-live-agent` feature gate 的 live tests 未被 CI 启用，bridge/bwrap 是受控 exporter 离线安全契约，保留。Claude R1 的 per-session map capacity 与 NativeProven identity-plan 持久化守卫已前向修复；doctor 对损坏 map 的诊断/修复与事务内重复 key/scope 解析按用户延后策略记录 DEFER。macOS reasoning 74/74、architecture guard 35/35、fmt 均 PASS；strict Clippy 正在运行，canonical full / 最终 Claude / source Push 未运行。OG19 仍进行中，正式进度35/61=57.38%；SourceA 仍 UNPROVEN，Linux 功能验收由用户后验。

R12 中“`reasoning_partial_outcome_reaches_actual_live_handlers` 零运行”的记录保留为历史状态；R13 对应实际测试已随 reasoning 回归集通过。该更正不关闭另行 DEFER 的 VER21/VER30 完整矩阵。

2026-10-12T11:46:01Z OG19 R14：macOS fmt check 0、strict all-target/all-feature Clippy 0（仅依赖 future-incompat notice）、canonical 双 env Nextest exit0，Run ID 984c711f-fbbd-42c1-be92-85d1b4741526，Summary [881.750s] 9878/9878 PASS，13 slow、1 flaky（重试后通过）、10 skipped；不能写成零flaky/skipped。29 active VER/61 active AC范围未变，VER21/VER30及对应 deferred AC仍延后。下一步最终只读 Claude Code R2；通过后再处理签名 main source Push。OG19仍未完成正式D/C继承门，SourceA/native GO仍UNPROVEN，Linux功能验收由用户后验。

2026-10-12T11:59:27Z OG19 Claude R2 actual single-review PASS（P0=0/P1=0/P2=3/P3=6，rawSHA256 d7fe72d74354b7c9183158b9c6a1fa5f0bcc8cf33372db6fa91170c7b5bbe440）。发现 per-session serialized map 虽有 1 MiB 限制，但 Record HMAC key-owner仍限256 KiB；已把 Record HMAC input cap调整到session map上限，Descriptor和单record codec 仍用原256 KiB。新 actual existing-key HMAC/verify 1024-record map >256 KiB测试，以完整限定名运行1/1 PASS。先前错误的短 `--exact` filter 0 tests 不计结果。R2后代码发生变化，formatter/strict/full/Claude均须更新，最终source仍待R3 review。

保留两个明确延后P2：DEFER-OG19-INVALID-MAP-DOCTOR-01 与 DEFER-OG19-IDENTITY-READ-CONTEXT-01。更正R13：map底层Invalid错误提示新session，但generic live/export partial文案仍提示doctor，故misleading diagnostic并未完整修复。另将Claude R2四项低优先级潜在P3列入DEFER-OG19-REVIEW-R2-P3-01；OG20 import None行为是AC77已定设计，不算缺陷。SourceA/native GO仍UNPROVEN，CI无本机OpenCode/模型runtime测试可移除。

2026-10-12T12:34:43Z OG19 R16: CI 审计未发现依赖本机 OpenCode/账号/真实模型响应的 runtime 测试，故没有 CI 测试需要删除；feature-gated live-agent 测试未启用，bridge/bwrap 为受控离线 fixture 安全测试并保留。修复后的 macOS canonical 双 env Nextest exit0：9879 passed（23 slow、1 flaky）、10 skipped；fetch/mirror 用例首试 status IO worker 失败，重试通过。R3 Claude、source Push 与继承 D/C 未完成，OG19仍in-progress，正式35/61=57.38%，Source A/native GO仍UNPROVEN。

2026-10-12T12:50:20Z OG19 R17: Claude R3 PASS 的文档 P2 已前滚修复：EN/zh-CN agent 用户文档写明 partial、doctor conflict、4096条/1MiB session map 上限及 invalid-map 限制，并用现有 `compat_agent_docs_contract` 固定。文档目标28/28、精确容量 HMAC 回归1/1、fmt、strict Clippy均PASS。改动仅文档/契约测试/计划记录，Post-fix full仍对应同一生产代码树；Claude R4复核与main Push待完成。formal35/61=57.38%，OG19仍进行中。

2026-10-12T12:55:39Z OG19 R18: R4 PASS并关闭R3 P2；修正 absent legacy identity map 与 corrupt/unverifiable map 的差异表述。文档目标28/28、fmt、strict Clippy均PASS；R5最终Claude复核待完成。没有新增测试target，INDEX不需改。OG19仍未Push，正式35/61=57.38%。

2026-10-12T13:03:36Z OG19 R19: 最终Claude R6 PASS，P0/P1/P2=0，R5文档精度问题关闭；当前唯一新P3为plan记录遗漏，已由对应R19条目补齐。文档契约28/28、HMAC容量精确1/1、fmt及strict Clippy通过；全量结果保持post-fix 9879 passed/1 flaky/10 skipped，不宣称文档变更后重跑全量。CI没有需要删除的本机OpenCode/真实模型runtime测试。接下来签名main source Push；正式35/61=57.38%，OG19仍待继承FAST-OG-02 C/D，Source A/native GO UNPROVEN。


## 2026-10-12T15:10:05Z plan-20260902 current card handoff

FAST-RG-01 v0.30.46 actual C/D is verified against main commit `077e66f75a5bf4bb793b95f904c82017fa1f0e86`: main workflow `38256073051` success; signed tag `v0.30.46` points to tag object `e7d0a3d57e2b4673a6fd81ca1222a7d28b442704`; release workflow `38262194805` success with four platform builds and publication jobs; requested `curl --proto '=https' --tlsv1.2 -sSf https://download.libra.tools/install.sh | sh` completed and installed macOS arm64 `libra 0.30.46`. Website content check was HTTP 200. The 6 RG + 4 scoped fix members and FAST-RG-01 are now formally done; fixed progress is 46/61=75.41%.

OG19 final-source commit `077e66f75a5bf4bb793b95f904c82017fa1f0e86` is on remote main; main CI run `38256073051` is green, final Claude Code review is PASS, and post-fix canonical macOS full is 9879 passed, 1 flaky (retry passed), 10 skipped. OG19 is locally-accepted but remains formally pending FAST-OG-02 C/D. CI audit found no runtime test that requires the user's local OpenCode installation, account, or live model response; no test was removed. The controlled fake-exporter/bwrap tests remain. OG20 dependencies DEP-OG-11, OG19, DEP-OG-02, DEP-OG-10, and DEP-OG-03 are satisfied, so OG20 (45 AC / 25 VER) is the next in-progress card. SourceA/native GO remains UNPROVEN; Linux functional acceptance remains user-later/UNRUN.

2026-10-12T15:45:44Z plan-20260902 execution update

FAST-RG-01 v0.30.46 actual C/D is complete at main `077e66f75a5bf4bb793b95f904c82017fa1f0e86`; exact main CI `38256073051`, signed tag `v0.30.46`, and release run `38262194805` succeeded. The macOS owner-safety gate executed 13/13 exact tests (each 1 passed, 0 failed, 0 ignored).

OG19 exact-source macOS canonical full run `9e934196-8592-4550-8c54-492ae37f0977` completed exit 0: 9880 passed (13 slow, 1 flaky, 1 leaky), 10 skipped. The retry-passed flaky test is `internal::protocol::mega2_mutate::tests::rename_is_same_parent_move`. OG19 remains `in-progress / locally-accepted` until FAST-OG-02 C/D; Source A/native GO remains UNPROVEN.

CI audit found zero runtime tests requiring a local OpenCode install, logged-in account, or real model response. `.github/workflows/base.yml` jobs `compat-offline-core`, `compat-owner-liveness-macos`, `opencode-export-linux` and `.github/workflows/live-compat.yml` job `compat-live-cloud` were checked. No workflow enables `test-live-agent` or `LIBRA_RUN_LIVE_AGENT_GATE=1`; controlled fake-exporter/bwrap tests remain. No matching CI test existed to remove.

Formal plan progress remains 46/61 = 75.41%; next active card OG20 (45 AC / 25 VER). Its implementation does not wait for FAST-OG-02 C/D under the user's USER-NO-WAIT-RELEASE-20261012 instruction; Source A still gates actual native archive. Linux functional acceptance remains user-later/UNRUN.

2026-10-12T15:54:00Z plan-20260902/FAST-RG-01 status reconciliation: the current snapshot and plan overview append the exact-source 9880-test macOS result and the latest OG20 pointer while preserving old rows as history. The complete workflow audit found no real OpenCode/local-account/model-response test in CI; compat-scoped-input-windows and the release workflow were explicitly included in the audit. No CI test was removed because none matched the user criterion.


2026-10-12T16:14:54Z plan-20260902 evidence update: exact push run metadata confirms event=push and head=077e66f; v0.30.46 peels to the same commit. Relevant successful check job IDs and the owner-safety job ID are recorded in plan-20260902. Nextest flaky/leaky labels are classifications inside the 9880 final test outcomes; the one retry-passed unrelated unit-test flake is registered as DEFER-OG19-MACOS-FULL-FLAKE-01. The 29-path source inventory is bound by command and sorted-path SHA-256; user authorization quotes are recorded verbatim.

2026-10-12T18:05:29Z plan-20260902 OG-20 source-local progress: remote main advanced from 077e66f to 76a9415 (mega2 source). Libra fetch and fast-forward brought that actual upstream source into local main before final validation; 16 dirty files were restored byte-for-byte, only the OG-20 section was inserted into the concurrently updated COMPATIBILITY.md, and unrelated plan-20260904/05 dirty bytes remained unchanged. On the merged source, all 21 active named VER each ran one test and passed; focused architecture 42/42, docs 29/29, real import CLI 53/53, import library 36/36, import command 35/35, reasoning 18 passed/1 pre-existing ignored, serial registry 1/1, fmt, diff check and strict all-feature Clippy passed. First Claude review failed 2P2 (key-factory caller closure; vacuous native-proof rejection assertion); both have code/test fixes and focused passes, but final Claude review is still UNRUN. OG-20 remains in-progress, with original AC18–24/VER15–18 explicitly DEFERRED/UNRUN as DEFER-OG20-REAL-BINARY-01; no synthetic result counts as native proof. Website cf source prepared in an isolated latest clone, typecheck/build PASS, not yet committed/pushed/deployed. Windows cross-target local attempt was cancelled during cold dependency compilation (exit 130), so Windows local validation is UNRUN and release CI remains mandatory. Source A/native GO is false; Linux function belongs to the user later. Formal plan progress remains 46/61 = 75.41%; after OG-20 local delivery, FAST-OG-02 is the next executable release card, then OG-17.

2026-10-12T18:13Z OG-20 post-review correction: the first active VER24 run failed because a new assertion read `agent_session.redaction_report.bytes_scanned` instead of the durable `agent_session.redaction_report.import.bytes_scanned`; corrected the test and reran VER24/25, all 21 active named commands, and focused suites to green on the same merged source. VER7 now explicitly runs both production-caller and key-factory AST guards, each 1/1, within the original single VER gate. Sole Claude Code R2 returned PASS with P0/P1/P2=0; raw review SHA256=3becc1da8c44ab18bc6b00fdb735cd9cee2865dd2f5c92ea3a52d2ba51c3e133. Remaining P3s do not authorize native GO or replace remote Windows release CI; the ImportError classifier uses chain-aware downcast_ref, and the Nextest generator does not read SRC_SERIAL_REGISTRY. Signed main commit/push, isolated cf docs commit/push and deployment, FAST-OG-02 full/release C/D are still pending. Formal progress remains 46/61 = 75.41%.

2026-10-12T18:15Z OG-20 source handoff: final 38 active AC and 21 active VER were accepted locally on macOS with fmt/strict Clippy and sole Claude Code R2 PASS. Signed-DCO `libra/main` commit `0ebe473f459d27ae51e9dc57fb8c4ca0e8d1be1c` and signed-DCO website `libra-backend/cf` commit `466b69877d0f348850ad3809a56e51b2b73925b9` were pushed and remote refs verified. The original 7 AC/4 VER real-binary cells remain DEFER-OG20-REAL-BINARY-01 UNRUN, Source A/native GO remains false, and Linux function remains user-later. FAST-OG-02 is next; formal progress is still 46/61 = 75.41% until its full CI/release/site C/D succeed.

2026-10-12T18:19Z FAST-OG-02 preparation: latest public stable was v0.30.46 and next patch candidate is v0.30.47. The reviewed `cf` tree at `466b69877d0f348850ad3809a56e51b2b73925b9` rebuilt and deployed as Worker version `7eb163ca-6f73-4e34-b700-d7abad57e88d`, latest deployment 100%; the EN agent docs page returned HTTP 200 and contained the OG-20 heading and Source A warning. Website content SHA256 `7125c351c59002f8f17ae91f9961410a78ee283a571719f88b381b4b4b682c6d`. Version faces are only prepared locally; full/fmt/strict/review, final main push CI, tag, four-platform release, manifest and exact curl install are not yet accepted. Preserve pre-existing unstaged plan-20260904/05 changes. Formal progress remains 46/61 = 75.41%.

2026-10-12T18:34Z OG-20 correction and FAST-OG-02 hold: actual source push `0ebe473f` workflow `38289802244` failed `compat-scoped-input-windows` job `114946885300` because Windows `-D warnings` found Unix-only imports/helpers and a Unix-only test helper unused before fixture listing. A cfg-only forward fix is local: macOS import 53/53, architecture guard 42/42, fmt, diff check and strict all-feature Clippy passed; Claude Code sole follow-up review PASS P0/P1/P2=0, raw SHA256 `6e4ddf81098d0a6ec8ebd54972ba01658f06471b3113a625b5a657383a3e057a`. Windows cross-target check is running; signed follow-up main push and exact new Windows CI are not yet accepted. The pre-fix v0.30.47 full run was cancelled while compiling (exit 130), and interrupted release review output was `Execution error`; neither is PASS. Interleaved signed mega2 upstream `76a9415` changed real source and its earlier full had one failed test, so final release full/review must be fresh. Source A/native GO false; formal progress 46/61 = 75.41%.

2026-10-12T18:39Z OG-20 signed correction handoff: cfg-only source commit `a12d327918bd08c7a198c5af6173504d87d5d31c` carries PGP gpgsig and DCO, ordinary `libra push origin main` succeeded, and remote main matched exactly. This does not erase failed Windows job `114946885300`; new exact-head Windows CI `38292090214` remains required. Mac targeted 53/53 and 42/42, fmt/diff check/strict Clippy and sole Claude fix review PASS are actual. Optional macOS-host Windows-GNU cross-target Clippy was cancelled during cold dependency build (exit 130), thus UNRUN; final v0.30.47 full is running, not yet PASS. Formal progress stays 46/61 = 75.41%.

2026-10-12T18:53Z second Windows CI correction: the `a12d327` exact push workflow `38292090214` failed `compat-scoped-input-windows` job `114954341485` before fixture listing on two remaining unused imports (`ImportTransportKey` in `import.rs`, snapshot commitment re-export in `hooks/runtime.rs`). The second cfg-only patch is local; necessary write set revision adds just the re-export line in `runtime.rs` to OG-20, production path count now five, AC/VER unchanged. The queued post-first-fix full run was cancelled while compiling (exit 130), and its incomplete complete-source Claude review returned `Execution error`; both require fresh runs. On the second patch macOS import 53/53, architecture guard 42/42, fmt/diff check and strict all-target/all-feature Clippy passed; sole Claude targeted review PASS P0/P1/P2=0, raw SHA256 `3f73f3d327a225f171bbe352edd6849a965f3c3dfdfb97c522743c4efebd490f`. Its complete-source Claude review, signed main follow-up push and new exact Windows CI remain pending. Optional Windows-GNU cross-check was cancelled in cold dependencies (exit 130), UNRUN. No Windows PASS or formal progress increment is claimed.

2026-10-12T19:03Z OG-20 second correction and FAST-OG-02 review reconciliation: signed+DCO `7efcf868d49a5183e7f0851a89abebee91357155` was pushed to remote `main`; exact Windows scoped job `114961614404` passed all four active fixtures, and `opencode-export-linux` job `114961614693` passed. The complete, untruncated source-chain Claude Code single review returned PASS, P0/P1/P2=0, with input SHA256 `72fc403ac071f8647747d54683311e3bf80d42cced1958db1e1c578c89b4bc0e` and output SHA256 `c95728d8b76891461cb899b71a5de8e741250be0af16f7f09842b843e4b49f9f`, bound to `7efcf868` tree `ce4fc3647f27c523b19733965afe591f3aa660a6`. The external mega2 commit's historical failed `macos_export_requires_trusted_binary_before_spawn` passed at 2385/9940 on the running final-source macOS Nextest; the full run, other source CI, local release build, release-head CI, signed tag, four-platform artifacts, manifest and exact curl installation are still pending. Claude Code's first FAST-OG-02 release-card/version-face review returned FAIL on stale commit enumeration and missing review/test closure; the card is now corrected and must be reviewed again before publication. Source A/native GO remains false; Linux functional acceptance remains user-later/UNRUN; formal progress remains 46/61 = 75.41%.

2026-10-12T19:08Z FAST-OG-02 scoped release-point Claude Code R2 PASS with P0/P1/P2=0 after the card's three R1 findings were closed; raw input SHA256 `4dfab5f501b9abfe68c31a14d79759986872426b78ae08b1fe38ca696e6cc409`, output SHA256 `08fcb583dbfc79639cc27a8f6f7ebe6ad78f2fd5a6bf687b59dc35def0b663bd`. The reviewed second cfg patch reconstructed from signed `a12d327..7efcf868` source objects matches the reviewed patch byte-for-byte at SHA256 `47a0af9dd04206d2e47db288a6dbd97a035c8c2600c580bd7cb2b2470142d1cd`. Latest stable remains v0.30.46, candidate is v0.30.47, `fmt --check` and `libra diff --check` pass; full/remaining CI/release C/D remain pending. Existing dirty plan-20260904/05 files are not staged for this point. Formal progress remains 46/61 = 75.41%.


## 2026-10-12 21:02:16 UTC plan-20260902 FAST-OG-02 v0.30.47 收口

FAST-OG-02 和继承本阶段 C/D 的 NPR-14、OG-16、OG-23、OG-19、OG-20 均为 `done / complete`。精确 main/source `84e6c52`、macOS canonical full 9940/9940 passed（10 skipped）、Claude Code 单评审 PASS、exact-head base CI `38295856216` 8/8、CodeQL `38295856151`、Code Quality `38295854977`、签名 tag `v0.30.47`、release run `38304328213` 8/8、Ed25519 稳定清单、四平台公开二进制 SHA/size、Homebrew 公式、GitHub latest Release、用户指定 curl 安装 exit0 与已安装 macOS binary SHA/CLI smoke 均实际通过；逐项原始身份与摘要记于本计划 FAST-OG-02 末尾。网站 OG-20 cf 文档已部署且 HTTP 200。Linux 功能后验归用户，UNRUN 不计 PASS；Source A/native archive GO=false，OG-19/20 具名 deferred cell 仍保持 deferred。CI 没有依赖本机 OpenCode、账号或真实模型响应的 runtime 测试可移除，受控 fake-exporter/bwrap 安全测试保留。正式进度 **52/61 = 85.25%**，下一张 OG-17；其候选虽已有本机 19 VER 与完整 Claude PASS，尚待本阶段 C/D 后的 main 交付，不提前计完成。

## 2026-10-12 21:12 UTC OG-17 本机局部验收及源码交付窗口

OG-17 closed pending reasoning v2 codec 的27 AC及19具名exact VER已在隔离候选逐项实际通过（19次均为1/1）；同一最终候选的grouped 22/22、architecture 45/45、docs 30/30、shared reasoning 20 PASS/1历史ignore、serial registry 25/25、fmt、严格全target/全feature Clippy和diff check均通过；根仓合入后再跑pending 22/22、architecture 45/45、docs 30/30、fmt、严格Clippy和diff check均exit0。完整Claude Code R5单审原文为PASS，P0=0/P1=0/P2=6/P3=3，input/output SHA256分别为`9187ee2c2fc489e18ceb199078b80dc385e9119fc89dd78bbefbb30b97a7de68`/`f47fc908407b4b43b2225a2460a590327ea9083a448de52893667fa7127e8cd1`；按用户PASS门验收，但不把P2后续建议或未运行Source A字段验收记为绿色。最终候选交接manifest SHA256为`2ea7129bf42f8f98764c7e0875aca37c2db58e2564d2ba9b02114f5771169a50`，生产v2 writer仍不开启直至OG-21配对。网站EN文档经独立cf工作树提交`signed+DCO e7df4d42cb2ee7aea8bff14d786cadebee8281b7`并Push cf，文件SHA256 `817a91d28e779e148d5629254f7de8dfb1596cdc241c1afbc1822448ad891433`；pnpm typecheck/build实际PASS，部署留给OG-22。根仓Libra signed+DCO源码提交`0cc0a711134a36f5685b86815b484d5c714ba397`已直接Push main，`libra ls-remote`核远端ref完全一致，且未纳入其它两份预存dirty计划文件。OG-17目前`in-progress / locally-accepted`，仍要继承OG-22实际patch C/D才计`done/complete`；正式进度仍**52/61 = 85.25%**，下一步OG-21。Linux功能UNRUN由用户后验，Source A/native archive GO=false。

2026-10-12T21:23Z OG-17 Windows exact-head failure and local forward fix: main`2f26cb2`的base run`38309969859` Windows job`115013015076`实际FAIL，`pending_payload.rs::with_synthetic_reasoning`为Unix-only合成测试helper却在Windows lib-test编译，触发`-D dead-code`。单行叠加`#[cfg(unix)]`，生产行为未变；根仓修复后pending 22/22、精确architecture guard 1/1、fmt、strict all-feature Clippy、diff check实际exit0。Claude Code单审`PASS`，原文input/output SHA256=`2e6a21b69845eb98e9ddea67db2de0483e83048a75079e5888dfd0943d9c08e1`/`cb3e4ec601dd6b46e331f318bbfe57dd97aab26b54a75f6c4bb48c4318423720`；其P1条件是精确guard运行，已1/1满足。失败原文保留，不提前计Windows CI通过；修复待signed+DCO main Push后重新验exact-head安全门。正式进度不变，OG-21并行推进。

## 2026-10-13 OG-21 本机局部门及已审源码交接

源码与本卡证据已在signed+DCO提交`44322f1af9fddac8894b78a971c8e9c6b19ad84c`直接Push main，`libra ls-remote origin refs/heads/main`当时核远端同SHA；该次Push后工作区剩余仅原有`plan-20260904.md`和`plan-20260905.md`未暂存改动。本卡未独立bump/tag/Release，下一卡OG-22负责唯一patch发布。

后续exact-head Windows scoped-input job `115031088133` 对`44322f1`实际FAIL（测试专用`catalog.rs::persist_pending_artifact`在Windows lib-test触发dead_code）。已将该方法限定`cfg(all(test, unix))`，macOS精确测试1/1、fmt、严格Clippy与diff check通过，Claude Code聚焦复审`PASS`且P0/P1/P2均为0；签名DCO修复`b00a2274b540a30f5ac1d6383ffa4469e6528ce0`已直接Push main并核远端SHA。新Windows CI待结果；旧失败和被中止的修复前全量(exit130，未进入最终用例运行)均不计通过。正式进度不变，OG-22最终全量须在包含修复的树重跑。

OG-21 的67/67 AC与33条具名exact VER在隔离最终候选逐项实际1/1 PASS（注入根仓`.env.test`后重跑33/33仍1/1）；reasoning group 141/141、architecture 54/54、docs 30/30、fmt、严格全target/全feature Clippy及binary build均PASS。根仓以manifest SHA256 `7963962eb2dca996682cf42e8babbeaf4a5a764a0e5bf98407d5fb8f9d61b4b1`独立核对15/15文件的原SHA、目标SHA、权限及review diff，按字节合入后pending 97/97、architecture 54/54、docs 30/30、fmt、严格Clippy和diff check均exit0。Claude Code实现单审原文`PASS (P0=0, P1=0)`，完整diff SHA256 `3afb80ec2e7b374e22c6c8f705091ba19a9f995931a4c786b6b5595d8196dd26`、输出JSON SHA256 `2fad2ce1f9ec2abed4b898792412a76098b0d5c0389f5ef38b0d8766f9d9af38`。五条P2按用户策略保留为`DEFER-OG21-REVIEW-P2-01..05`，不冒称已修复。网站EN已审源码在Libra backend cf `b21eb956cf138479fb0f14bd74bb290e2ddc5eb2`且远端一致，尚未部署。Source A/native archive GO仍未证明；非空正向恢复验收使用cfg(test) issuer，不属于真实OpenCode模型验收，未加入CI。OG-21当前`in-progress / locally-accepted`，须继承OG-22实际patch C/D后才计done/complete；正式进度仍**52/61 = 85.25%**，下一卡OG-22。Linux功能UNRUN，由用户后验。

## 2026-10-13 02:04 UTC OG-17/OG-21/OG-22 v0.30.48 收口

OG-17、OG-21 与唯一家族发布卡 OG-22 的实际 C/D 全部通过，现为 `done / complete`。精确源码 `c3fb205ebb0819cae09197d10bbbb28aa1391fd8`：macOS 最终全量 10002/10002 passed（10 skipped、14 slow、2 leaky），Claude Code 单审 PASS，base CI 8/8、CodeQL 2/2、Code Quality 2/2，签名 tag `v0.30.48`、发布 run `38332037679` 8/8、签名稳定清单/四平台产物/Homebrew/公开 GitHub Release、用户指定 curl 安装和本机 Darwin SHA/大小、cf 网站 100% 新部署与线上 HTTP 200 均实际验证。完整身份、摘要和失败/延后边界见本计划 OG-22 的 2026-10-13 收口记录；聚合收据 SHA256 `55843a29ef8c28040899502c414a6cbba07ce5468146538cdaf509a71bfc70f7`。历史 Windows FAIL、取消的测试与 Source A 未证明均没有冒充通过；Linux 功能后验由用户执行。正式进度依次为 OG-17 后 **53/61=86.89%**（下一张 OG-21）、OG-21 后 **54/61=88.52%**（下一张 OG-22）、OG-22 后 **55/61=90.16%**（下一步 OG-05 必要六卡拆分，先实施 OG-24）。

## 2026-10-12 23:08:37 UTC（北京时间 2026-10-13）plan-20260902 OG-05 拆卡状态同步

`c3fb205` 固定计划基线的历史判断：彼时 OG-22 仍只准备0.30.48 release，未取得的四平台/签名manifest/安装/网站门不能算完成；实际后继收口见上节 `71e9f446`。六张新增卡中 OG-24/25 为 `planned / 空`；OG-26 因 DEP-OG-07 的 9/04 镜像未同步为 `blocked / 空`，仅可准备设计、不写 `reasoning.rs`；OG-24/25 仍须按前驱实际 C/D 或 source-qualified local Deliverables 与逐路径单写者核对后开工；OG-27..29 实际应为 blocked：OG-27 除有界 T1 准入事务设计外，coverage-v2 全路径及真实 Source A 完整生产checkpoint亦为 NO-GO 门（T2 持久提交独立）；OG-24/25可逐卡推进机制/隐私局部门，OG-26待镜像同步后再实施。原OG-05保留40 AC与19个原标号；原VER15三条独立命令按G-03拆为15a/b/c，故实际21 VER，其中三个真实字段手工门均UNRUN，仍 `blocked / 空`。在该 c3 历史时点按已完成旧52卡重算为 `52/67=77.61%`；其后 OG-17/21/22 实际 C/D 已完成，当前为 `55/67=82.09%`。用户 Linux 功能后验/UNRUN，不做当前 macOS PASS 依据。Claude Code 单审 PASS 为新卡评审门，复杂本机OpenCode/账号/模型响应测试未加入CI。

## 2026-10-13 OG-25 coverage-v1 契约裁决（计划修订候选）

只读协议审计发现 `coverage_schema_version=1` 的规范只允许原四种 semantic record，直接新增 OpenCode `SemanticRecord::Reasoning` 违反版本契约。按用户对同等难度问题的延后策略，原 OG-25 AC3 typed 目标登记 `DEFER-OG25-COVERAGE-V2` 为本计划外未修复遗留；OG-25 当前五条 active AC 的 AC3 改为仅 state/proof 均缺省的 visible-only text 经安全 ordinary redaction；任何明确 state（含 `{}`/仅allowlisted字段）、originProof 或 unsupported reasoning part 的 text 从 ordinary 副本删除且 partial；reasoning-bearing turn 保持 Incomplete，零新增 v1 record/零 native artifact。所有 role 的 part/message envelope 和 export document/`info` provider metadata 及保留的深层 envelope/model/error/files/tool `state.content` 子树及非 schema 对象 `state.status/title/raw` 亦须清除嵌套 provider metadata 与具名密文提示，不能由非 assistant role 或嵌套层级绕过；capture/snapshot 只接受单 JSON export object+`messages[]`；flat/JSONL/缺失或错型 `messages`/重复键须 content-free Partial、`SourceAuthorizationMismatch` 和 ordinary 拒绝，可解析未知 root/message/classic-info 键须剥离且 Unproven/Partial、安全可见字段保留；独立 `extract_opencode` best-effort 不绕前置门，以上行为均须具名测试和文档明示，不能静默丢采集。对应具名 VER1/VER2 分别只证明安全 ordinary redaction 与 coverage-v1 Incomplete，VER7 须以经独立审查确认与 OpenCode schema 匹配的 native/classic clean export 在同一精确测试中证明 Ordinary+coverage/extractor parity，安全 classic step/tool 与 native variant/compaction/error-retry 字段不得误删；provider response、classic user summary object/files/skills 与 assistant summary（任意形状）、format/structured、session fork/permissions/revert/share/location.workspaceID 与 native location-switched 额外标识等 schema-valid 敏感结构须按设计 partial 并文档披露；八门（含非 assistant message 错型 `text`、assistant text part 非白名单嵌套字段及 document 与 tool `state.content` 嵌套 provider metadata 与非 schema 对象 `state.status/title/raw` 零 sink，及 native text/file union、classic schema-valid pending raw String Ordinary 与非 schema content/错型 raw Partial 的对照，以及逐保留非自由文本结构容器的非 allowlist leaf-injection VER8）均不能冒称 typed Reasoning 正例。OG-05 的原 AC1 候选同part可读text私有carrier安全投影与 AC2/最终 typed 正例仍明确保留且未通过，须先由计划外 schema-v2/全路径兼容及迁移验收解除门，三字段归档另须真实 Source A。此裁决不新增第68卡，不计 OG-25/OG-05 完成；OG-27/28/29 因依赖完整coverage-v2/真实Source A保持blocked，不以deterministic T1/MAC/replay测试替代完整生产checkpoint；初稿基于 `c3fb205` 时正式进度 `52/67=77.61%`；本修订已结合 `71e9f446` OG-22 实际 C/D 重算为当前 `55/67=82.09%`。复杂 OpenCode 模型/账号验收仍只在 macOS 本机手工执行，不加入 CI；Linux 由用户后验。

2026-10-13 OG-25 R14 隔离候选的既有范围经 Claude Code 单审 PASS、24/24 broad 与原八 VER 通过，manifest SHA-256 `05fb94a434d0703d32ba980992e58c8259c9ea337b099bcf1c0308f31fac8748`，review raw SHA-256 `897933f21d84b803999ed49ab8a336339a13947daefe642a7dd5569031b3e73c`。本次计划评审新增 VER8 的逐保留容器非 allowlist leaf-injection 硬门，R14 没有运行该门，故 OG-25 仍 `planned / 空`，不得凭旧范围 PASS 合 main/完成；后续新实现和新鲜 Claude 单审另验。

2026-10-13 OG-25 R20 最终范围已从 `dd41e9a` 的相同 12 路径 preimage 精确回放到主工作区，12/12 postimage/mode 匹配；补丁 SHA-256 `ba38444c6c549b78c5b400c6fad5ed3449384ab311c7bf23e931407b694b208b`、manifest SHA-256 `36a15e49bd667203b50854f59bc18abb62c8852cea7036e2fc9532b209dfef52`。当前源码八条具名 VER（VER6 22/22、其余各1/1）、OG25 24/24、相关集成和文档守卫、fmt、严格 Clippy、diff check 全绿；19 门日志索引 SHA-256 `c98ef334a691c49c173241e228b8b6999136dce04e2a042ca6d93f596d5c0439`。唯一 Claude Code Sonnet R20 单审 PASS（P0/P1/P2=0；raw SHA-256 `a0f8a299d052ccc16f85f380a6a47dcbae2d08fdba8baf6d4aeaf63dc71fb835`）。私有 `.2` 的一次真实 Haiku 4.5 工具导出确认 completed read 仅有 `truncated:bool`，另有 read error，安全收据 SHA-256 `7ad568d494801e5c0af930d431b8b1c9f4ab2f5801de0efaf78eed56adb047ce`；同一真实 export 经匿名 OS 管道进入按当前 classifier 全文件与 parser/disposition 精确源码切片编译的隔离 harness，得到 `Ordinary`、reasoning 维度 Complete、completed read 布尔 metadata 1→0、六类敏感命名键泄漏0；分类收据 SHA-256 `d7e9cfb9037defd8d1e225d359dea2e9efd6f361cc38b4d2e650476234cd4947`，无第二次模型调用或原始 export 落盘。这仍不是完整 archive 或 native Source A。OG-25 暂为 `in-progress / locally-accepted`，与 OG-24 同等候 FAST-OG-04 C/D，正式 **55/67=82.09%**，下张 OG-26；Linux 功能验收由用户后验。

2026-10-13 06:05 UTC 运行时分 lane 复核：全局 OpenCode 已是 `v2.0.27`，OG-00/18/04 对 `v2.0.26` 的冻结来源/fixture PASS 不追认为新版 PASS；全局新版普通 live 采集的来源/schema/功能为 BLOCKED/UNRUN，由 DEP-OG-04 验收机维护者/root 和未来选择该 lane 的卡关闭；不阻塞 OG-25 私有 lane。OG-25 真实工具分类仅绑定隔离私有 `.2` selected binary SHA-256 `28174e893a2b63b75f58af4c77212747f6e1eebce5dd0252cfe64847b342615c` 与本机收据，不等于全局新版兼容、完整归档或 Source A。DEP-OG-04 已按 lane 修订；私有 `.1` 历史普通内容基线保留，正式进度仍 55/67。

## 2026-10-13 03:02 UTC OG-05 六卡拆分在 v0.30.48 收口后生效

OG-17/21/22 的实际 C/D 已由 signed+DCO main `71e9f44699932b43d31037d0d0c8f49606d97c99` 记账；本次增加 OG-24..29 六张按机制、live、import 与 replay 拆分的任务卡并修订 OG-05 的前驱/写集/验收，不把准备中的 OG-24/25、blocked 的 OG-26..29 或 Source A 真字段归档记为通过。原61卡中已完成55卡不变；新增六卡后正式 **55/67=82.09%**。下一张 OG-24 先交付封闭 verifier 机制；OG-25 随后做脱敏前分类与安全 ordinary projection，真实 native 字段、coverage-v2 及完整归档仍按各卡 hard gate。用户 Linux 功能后验，复杂本机 OpenCode/账号/模型响应手工验收不进入 CI。2026-10-13 03:17:46 UTC 的私有 `.2` / Anthropic Haiku 4.5 high 本机 raw export 仅观察到一个 `state.signature`（636 UTF-8 bytes，SHA-256 `777d92e62db984ede17df0edcf03e5bf987b67c302b2005d2bc8c296c49bc77b`），无另两字段、`originProof`/已登记 issuer/可验 receipt；观测收据 SHA-256 `d5b4cb370fe4a35f64e0511dacbac2dc4d5ba594c2d5cf824c7f14984a80cf28`，`archive_go=false`，不计 Source A 或任务卡通过。其后隔离 issuer opt-in 的第二次 Haiku high export 已在同一 part 产生 `signature` + `originProof`；独立 Ed25519 核安装 challenge、字段 receipt、source 坐标、UTF-8 digest/length、Anthropic endpoint digest 均 PASS，export SHA-256 `b57426f11edcefb2b7a9ef3736641db037c0fa0b85db8dd8cfdcfef98e35a603`、脱敏收据 SHA-256 `51c0bed89b11fbdfc08f7640dc82a71d4ef5eb102c275dd9978e43eae986942f`。这只闭合 producer 侧一个字段：Libra main 仍只批准 `.1`、未登记 `.2` issuer，`libra_source_a_go=false` 与 `archive_go=false`，无 `redactedData` 正例；不能替代 Libra verifier/selected binary 注册/归档卡门，真实模型测试仍只本机手工。


## 2026-10-13 OpenCode Session CI 验收范围修订（dcc 已推送，远端 CI 未绿）

用户后续明确要求：OpenCode CLI/hook/export/import 进入 Session/Checkpoint 采集链的集成正例及失败时零写入负例仅在本机 macOS 显式验收，不由默认 Cargo/Nextest 或 GitHub Actions 执行；纯解析、codec、静态契约和不启动 Session 采集的 sandbox 单元门继续保留。2026-10-12 曾记“受控 fake-exporter/bwrap CI 保留”的历史结论仅适用于当时口径；本次把 fake-exporter Session 链改为本机门，不能把历史 CI PASS 充作新的本机 PASS。

独立隔离清理候选基线是 signed main `5cafe322fc1bb3d309430b58bb57f40b162ed215`：16 路径补丁 SHA-256 `63d73f0ce9036350fcb2c55df7bc2580c306b019f32b95dddc66e15d1558b8ae`、逐路径 manifest SHA-256 `5fd9bb80936074e3aa63b44fa74ccd57d6539ca8e10c2e7b202bcff1e5cec9aa`；Claude Code 单审首轮 FAIL 后修复并重审 PASS（0 findings），最终 raw SHA-256 `76d9373b5402845caa39b659ea6a4c73af7d8518798dcc86b8fbeac3913d33cf`。原 `opencode-export-linux` CI 作业改为仅跑八项 bubblewrap sandbox 单测的 `opencode-export-sandbox-linux`，桥接 E2E 从 workflow 移除并由 Cargo feature + 逐项 ignore 双门保护；混合矩阵只把 OpenCode 行移至显式本机运行，Claude/Codex 回归在默认 CI 保留。另合入“新 main 提交取消旧提交未完成 Actions”的独立六路径补丁；两处共同修改 workflow/guard 在根工作树合并后正重跑最终验证，尚未 signed+DCO Push 或取得新 CI 结果。

本机显式 ignored 的 hook、byte/replay/metadata oracle、import 前置门已在隔离候选实际通过；`opencode_extraction_failure_continues_checkpoint` 在精确 `5cafe322` 候选本机运行 **FAIL**（exit 101），日志 SHA-256 `7c2db01299c553ae028475db111d521f00a5ba6af46fece709a7ab5b024a19d4`，错误为 reasoning source 没有 ready capture consumer。保留原测试与失败，不计整套 OpenCode 本机验收 PASS；现行 `live_checkpoint.rs` 对无 ready consumer 的来源有意 fail-closed，后续卡须按产品契约复测并修正旧断言或提供真实 consumer，不得放宽此入口。另一个 Linux bridge 历史 CI run `38357267845` 的旧导入用例曾以 `LBR-AGENT-020` FAIL，移出默认 CI 不修复该历史失败。

最终联合补丁已由唯一 Claude Code 复审 **PASS**（P0/P1/P2=0；raw SHA-256 `ff159c5053fc08aa3f3a65f466de75a830cc7659b910addbd8cc54618c5c1726`）；本机 ER-13 `cargo nextest run --all --no-fail-fast --retries 2` exit 0，10041/10041 passed、22 skipped、14 slow，原始日志 SHA-256 `fd978869ea624edbc105cf6d3009c69db6631d7fc14941e60032a7a0101abd7f`。跳过项不是本机 OpenCode 验收 PASS；workflow guard 3/3、冻结 oracle 1/1、Actions 清理脚本 25/25、nightly fmt、严格 all-target/all-features Clippy 和 diff check 均已通过。联合评审后的两处修订仅为 workflow 中移除 self-hosted Python 单测步骤及 tracing 文档的显式本机 oracle 命令；这两处已包括在最终 Claude 复审内，workflow guard、Python 单测和 fmt 在全量结束后又针对最终工作树复跑通过。signed+DCO main `dcc468bd31c88a7e8d8c0d2957d6e80b65908990` 已直接 Push；托管取消旧提交 Action run `38373460785` success，随后人工清理无旧 nonterminal run。该提交 CI run `38373460519` 在 2026-10-13 10:35:00 UTC 观察时仍 in_progress：fmt、Clippy、Windows scoped-input、redundancy、pure sandbox Linux 与 macOS owner-liveness success，`compat-network-remotes` 因 GitHub 第二次 discovery 请求的 reqwest 发送错误 failure，`compat-offline-core` 尚未结束；本机同名 network 精确用例 1/1 不能替代远端绿证据。失败 job `115215856987` 在 workflow 未结束时经 `gh run rerun --job` 返回 `cannot be rerun`；若下张签名 main 提交先发生则按用户要求取消旧提交 run，改验新精确 head。正式进度 **55/67=82.09%**，下一卡 OG-26，Source A/native GO=false，Linux 功能由用户后验。

## 2026-10-13 11:01:52 UTC OG-26 本机 A/B 验收与后续门

OG-26 的九条实现/测试/文档路径已在 signed main `dcc468bd31c88a7e8d8c0d2957d6e80b65908990` 上按逐文件 pre/post hash/mode 对拍，计划内 VER4 不可能的 `0 filtered` 条件已修正为具名 1 passed/0 failed/0 ignored，实测其余 14 个用例 filtered-out。最终源码 VER1–3 各 1/1、OG26 lib 8/8、architecture 55/55、docs 30/30，唯一 Claude Code R4 **PASS**（P0/P1/P2=0）；真实 `.libra` 根的 macOS 全量 Nextest exit0，10050/10050 passed、23 skipped、1 flaky 重试通过、2 leaky，日志 SHA-256 `768cdac31bbf6918bb222252e8337fee278a5bf8ef60ae6176010beebca569e3`。根 fmt 与严格 Clippy 均 exit0；签名 main Push 待执行；隔离环境先前失败的 full/carrier 测试不倒记 PASS。OG-26 为 `in-progress / locally-accepted`，FAST-OG-04 C/D 尚未完成，父计划正式 **55/67=82.09%**；将 OG-24/25/26 的本机源码就绪单独计数为 **58/67=86.57%**，不替代正式完成。下一步在 OG-26 签名 main 后先立卡并单 Claude 审查独立 coverage-v2 计划；草案中的 COV2-01 尚未正式立卡、owner 未分配，当前不可视为可执行卡。OG-27 仍须 coverage-v2 与真实 Source A 双门。Linux 功能由用户后验。


### 2026-10-14 — COV2-25 开工与 G-09 实际交付

三文档 G-09 的唯一 Claude Code R2 实际 PASS（P0/P1/P2=0、P3=3），raw SHA-256 `078014fb07a626ba2bf80de0afdae79fbcafce4404a9f8aa3ea67df4221d65d9`；完整三路径 SHA/mode manifest `c0a907fed96d481125bdeed1a931daf070b497bd98e41086c22cb3cf9f874eab` 已对拍 signed+DCO main `2b80a56242c89518e706eaca18ceb4b0d66798d6`，Push 后 Libra/GitHub main 同 SHA。最终 nightly fmt/diff/strict Clippy exit0、hook contract 11/11，quality index `bea125fa600546496462c208f1d268de9b46b5b66d2adbfa38cbb4f1ef71d085`；R1 两项 P2 已由 R2 闭合，R1 FAIL 及其 raw `69d1778c017383ea4e3bff16872dbddab553faf589ff98be5dd3864c34897da4` 保留，不倒写历史。正式0/26、源码就绪12/26=46.15%、父正式55/67=82.09%不预增。

COV2-25 现为 in-progress，批准的 key verifier、既有 Unix tests 与精确 architecture guard 已实施，八 AC/四 VER/八 mandatory 回归、最终 full 与单审仍待实际结果。当前最终实现树包含上游35f8的cat-file源码修复，临时0.30.49只在本卡源码交付后执行，且不解除 COV2家族/DEP-OG-13/Source A。没有新增 OpenCode Session或模型 CI，复杂Session仍macOS本机，Linux功能由用户后验；此源码卡不调用模型，不碰两份无关计划工作区改动。下一项为临时0.30.49发布，再推进COV2-22真实持久化。


COV2-25 focused r1 的 signers闭集与VER1各1/1，VER2实际FAIL/raw `467fdb6501037ae2dacdea8f0abcd8c890cd54771be40f49a2b6481e29790b09`：同仓库子路径不是foreign repository，旧scope/storage解析允许；仅以独立初始化repo替换测试fixture。无生产鉴权改变、不扩I、不缩减AC/VER；r2整组及最终full/单审仍待，旧失败不倒记PASS。


COV2-25 r2 focused实际全部通过：四VER、八mandatory、三闭集及三上游cat-file exact共18 selector各1/1，日志index SHA-256 `6be14aba178bf940dcf9012ee2b1cc05e8c5c0334fddf54b9cd70df78e6b861b`。八AC本机机制断言通过，None-source token不能匹配实际Some source digest；synthetic proof/codec fixtures不证明Source A。首轮VER2错误fixtureFAIL保留；最终fmt/strict/full、单Claude及signedPush待，因此source-ready仍12/26、formal0/26，父formal55/67不变。


COV2-25 r2 fmt/diff exit0；strict Clippy初始化后长时无CPU/输出进展，root仅终止本次20个cargo/clippy子进程，实际exit1为interrupted/unqualified，不判源码缺陷或网络故障且不计PASS。采用runner-only `CARGO_BUILD_JOBS=4` 重跑同一all-target/all-feature `-D warnings` 检查；本轮不改仓库/CI配置、不缩减测试门。


COV2-25 source Claude R1实际FAIL/raw `5c621a92df654579df39509008ced7fa62c6848ac6afff570b3c7c9557f0c1b4`（0P0/0P1/1P2/4P3）；生产 verifier无缺陷，r2历史收据把组合foreign root/storage拒绝误记为独立storage验收。现仅在VER2补原root+foreign repo storage独立负例；原r2结果不能替代新增断言。P3的deadline和v1 golden证据边界写明，ACcheckbox仅本机机制状态；最终exact/strict/full/单审/signedPush仍待，source-ready不预增。


COV2-25 r4补齐独立foreign-storage参数负例后，18 exact全部各1/1，index `066a7684766b957208e3774c91dd97b3d0a59fff045670c89fb423468bc5d45f`。r3四jobs strict/fmt/diff exit0为补storage断言前的快照（qualityindex `208360fb18549e1ff7867e976442352e61ae72f2d549e2bfbbc3c9814a5ca5fb`），最终r4质量/full/单审仍独立执行；不复用旧结果为新增断言PASS。source-ready12/26/formal0/26/父55/67保持。


COV2-25 r4最终quality fmt/diff/strict all-target/all-feature `-D warnings` 四jobs实际exit0，index `8cdf4695a85138e2e7c2ac1cdfc9c4f5214e10385a8d81ee8a0367e3a4a5b7d2`。临时49只读AST检查640src，manifest `8a1d7b82ee1f02b4716b52b78ec3e86a235fe8501d53b28b2ed583dd43e122ed`，新keyauth→decoder仅内部引用，auth自己零生产caller；保留embedding/native证明边界。本段时点dual-env full与最终Claude/signedPush仍待。

### 2026-10-14 13:36:45 UTC — COV2-25 冻结2b候选 full 通过；新版合流门未预记

r4同时加载两env，runner-only四jobs/四test threads，Nextest实际exit0：10110/10110 passed（2 slow）、23 skipped、2645.131s，raw SHA-256 `01dafbcbf51f87db809fa1c3b57ad44333913286f0cc72daf181e6735e34b789`。八候选路径在full期间原字节/mode不变，测试已含独立foreign-storage负例；key/pending/guard精确后像见COV2-25。23 skipped不计通过，Source A仍未证明。此full是HEAD2b加COV2-25、包含35f8，未包含期间新上游5c的cat-file批处理/nonUTF8变化或29普通适配；临时49实际最终源码必须另验，不能冒用冻结收据。最终单Claude、signed+DCO main Push仍待，source-ready12/26、formal0/26与父55/67暂不变。真实本机OpenCode29全局binary已核官方同SHA/size，但版本核验不代表内容捕获通过；新原生目录导入修复和普通29独立协调仍为私有准备，不纳入本卡key轴。


## 2026-10-14 14:30:58 UTC — ordinary29三卡G09 R3当前协调（实际ID为OG-UPGRADE-029-01）


**REL-OG-TEMP-049 当前窗口覆写（2026-10-14 14:30:58 UTC，同步USER-TEMP-REL-049）：** 临时release操作自身不新增任务卡；本次兼容前驱G-09另新增父计划三卡 `FIX-OG-NATIVE-IMPORT-01`、`OG-IDLE-ERROR-029-01`、`OG-UPGRADE-029-01`，COV2不增卡。0.30.49精确成员为最新上游cat-file已审变化＋COV2-25源码＋按`COV2-25→FIX-OG-NATIVE-IMPORT-01→OG-IDLE-ERROR-029-01→OG-UPGRADE-029-01→COV2-22`串行交付的三卡最终实现/测试/文档；反向定位`OG-UPGRADE-029-01→OG-IDLE-ERROR-029-01→FIX-OG-NATIVE-IMPORT-01→COV2-25`，已持久化/已发布部分只前滚，不改tag/asset。当前R拥有者为本对话root，唯一发布操作ID是USER-TEMP-REL-049，三卡`C/D coverage from`均明确指向该ID（其实际门由REL-OG-TEMP-049登记）；不是新增release Task，COV2-REL-01及DEP-OG-13不因此complete。最终full必须含latest5c cat-file及本三卡新源码；旧COV25 full不能替代。最终含源码变化的实现树完成适用focused、fmt/strict、单Claude及双env full之后，若bump仅既有版本faces/self-lock/纯release说明和记账且逐文件实现/测试/配置/行为文档SHA/mode同一，才按用户既授权例外复用该final full，不再重复；其它源码变化重跑。安全CI、8个releasejob全部success、四平台/manifest/CDN/Homebrew/网站/HTTP、指定curl本机安装和installed cat-file smoke、未完成v2入口不可达证明全部保留，复杂OpenCode Session/account/model仅macOS本机，绝不进入CI。三卡实际release前locally-accepted，不记formalcomplete；签后source允许按noWait继续COV2，不以成员后置C/D制造依赖循环。

**本次DEP镜像：** DEP-OG-029-PLAN/COV25/SITE/COV-REBASING与COV2 DEP-COV2-029-BASE均本三文档同改；FIX→IDLE→BASE→COV2-22顺序，后续05/06/11/15/18及所有Icore相交者从BASE精确重基；原Source A、22本机Session与COV2-REL唯一家族点不改。


### 2026-10-14 14:30:58 UTC — R3精确preimage及待验收边界

**历史G09 R3签前preimage边界（已由后续实际交接收据覆盖）：** signed+DCO merge `dc1aab0905c770b3badd80054734c81862cc6a0e`，parents `783041fe9e0db6daa360edc41222c0d3f48415ec`（COV25）/`5c0eb71c57f5cf73e8f711e459826758086a4c11`（upstream cat-file）；merge尚未Push。COV25 actual Claude R2 PASS raw `4c6487fcfa0eea755457819506984a0c79ec3fd47ff7e9f89973f7445b12fefe`，其冻结full10110/10110只证明旧树；不得把它记为含5c及三卡的最终49full。父source-ready分子和COV source-ready不预增，三卡所有新门UNRUN。本三文档修订须最终singleClaude、signed+DCO main/remote同一后采纳。

**DEP-OG-029-SITE已核开工事实（不代表文档交付）：** root复用干净Libra原生隔离仓 `/private/tmp/og20-cf.iE5Kb7/libra-backend`（.libra，无.git），fetch/ff后cf为 `597b86cf254c212cfc142ebc070d54b112d2af89`，status空；唯一页面 `apps/tanstack-app/content/docs/commands/agent.en.md` 开工SHA256 `871a541dddd965b38d4d7953c5bca63f57cd0e9963aaeeacdac167872849d0c3`/0644，由root后续唯一页面writer。原backend四dirty页面未写；新页面编辑/typecheck/build/Claude/signed源/部署HTTP均UNRUN，不能记交付。


### 2026-10-14 14:56:42 UTC — FIX-OG-NATIVE-IMPORT-01 本机源码开工/具名门

G09 三文档修订实际 Claude R4 PASS（0P0/0P1/0P2/3P3），raw `cf0da86d05ae1044a504f0d839d8fc9b85089ae88ca473a250e172c02b1abe5e`；三文件最终manifest `44548edb2893d22d452174a5fa4218933efdff92ec617fbaaf8dc9504f6a2b58`，独立signed+DCO main `4c628d4d3d5a3990eccbaee6c502bc64c73c52cd`/实际GitHub main同SHA，fmt/diff/strict exit0、docs-contract30/30。R1/R2/R3 FAIL历史和R4三个非阻断P3保留；这是规划采纳，不是新源码或Source A验收。

COV2-25 最终代码此前已独立 Claude R2 PASS（0P0/0P1/0P2/3P3），raw `4c6487fcfa0eea755457819506984a0c79ec3fd47ff7e9f89973f7445b12fefe`；signed+DCO `783041fe9e0db6daa360edc41222c0d3f48415ec` 通过signed merge `dc1aab0905c770b3badd80054734c81862cc6a0e` 随4c628d4实际Push。原八owned最终postimages/PGP header/DCO及18个exact各1/1、冻结2b实现树full10110/10110均已核；5c cat-file本机lib20/20与command33/33、组合strict exit0。COV2-25 现在source-qualified locally-accepted，COV当前源码13/26=50.00%、正式0/26；旧12/26仅为Push前历史。上述冻结full不含5c及三张ordinary29新实现，实际49最终full仍待，不据此称家族complete或Source A。

本FIX从signed4c628d4开工，唯一production修改collect_facts，独立收集directory/cwd/location.directory字符串；既有canonical/same-storage门未改。源码postSHA `7dad06a4d5829abdbd172db9e53db87040078ff1f9554719f6e2a2e0dcb117f5`、mode0644；两个具名新门各1/1，分别raw `e46e7b0547ff860cadc369975d9410ce3cd91c2581a66d66fd8322b33d357c2e` / `bf3eac3e3e6ee5817872339e9db9d38086c4dcb234a55f732e8b1dd02b060726`。完整agent_import模块38/38/raw `e7afa18878f0911bc3a96838342978d0b1ac67f43c325ecb56ade4abea91e4f1`、docs-contract30/30/raw `c50b16ae555c172a0f8c96669c186ec6dea3ce85928ef52397c40b9f2541d150`。VER2包含三种目录冲突、缺失、五类非string、classic有效string控制、跨repo与词法/Unix symlink别名。两unit不直接覆盖descriptor helper；真实existing Session非零内容仅在后续OG-UPGRADE-029-01 VER2实际验收，当前UNRUN，原空checkpoint不可据此称已补全。

精确旧JSON/MANIFEST、catalog、settings/helper等开工/收口SHA及mode比较未变化；不改native来源/issuer/CI/model权限。隔离网站cf597b86唯一页面追加与EN同段，typecheck/build实际exit0，签名源交接及49部署HTTP待。最终本FIX fmt/strict/Claude/source主与网站Push待；父正式55/70=78.57%、源码58/70=82.86%暂不预增。下一源码卡OG-IDLE-ERROR-029-01，真实内容与普通29基线后置；新实现树full/049 C-D所有门保留。


### 2026-10-14 15:47:50 UTC — FIX 已交接；IDLE 本机机械门与后续独立仓验收

FIX-OG-NATIVE-IMPORT-01 唯一 Claude Code 实际 PASS（0P0/0P1/0P2/5P3），raw SHA256 `ac889106b8764b2d4ecfba26bfef88b650200c692d4cb6c199bfa762eaf210a8`。root交接收据 `/private/tmp/libra-fix-og-native-import-review-r1/signed-local-source-receipts.json` 的SHA256为 `86249934f689efeff06f3cefe99fbd99292a4154517dd550dde9b36e8a898f26`：主仓signed+DCO `6524153d97ab2f5c02f7875abd5f3ed3d343cea8`、网站cf signed+DCO `ddfee87c66d35cf586ca4534c0d427a7254085d0` 均Push exit0、remote同SHA、reviewed owned blobs一致；两exact各1/1、import模块38/38、docs30/30、fmt/diff/strict与网站typecheck/build exit0。网站已签页面postimage SHA256 `ca7673546b434551f25ffbec2d07653a10f7d000574a023e9508197466698042`、mode0644；部署HTTP和实际049 C/D仍待。父formal55/70=78.57%、source-ready59/70=84.29%，COV formal0/26、source-ready13/26=50.00%；FIX源可由IDLE消费，未称release complete。

FIX五个P3保留为正式审查历史：非string directory配有效cwd的组合未单独pin、descriptor helper独有scope gate未由两unit直接覆盖、cross-repo错误码路径差异、旧计划Lifecycle标签/merge时态卫生及段落空行排版。本次例行同步仅清理旧生命周期重复标签并标注过时签前状态；不冒称其它P3已修复，descriptor/真实内容门仍由后续独立仓BASE验收且当前UNRUN。

OG-IDLE-ERROR-029-01 从上述已交接FIX源码开工。唯一production路径 `src/internal/ai/observed_agents/opencode_reasoning.rs` 当前SHA256 `7da0e57485c5dc02179240b928a0f67f9f134efe7c7b76029c21ae80bfea4cc5`、0644；三新exact分别实际1/1（0failed/0ignored），VER1/2/3 raw SHA256为 `b91253f55c5e45bd7b902100098a9832d78f18b4e427a4d753d9a205ce5f1d0d` / `deea4de5ff5f21f17ad14b2fffdc506b5836a13dc9cbb046d47d3c50ffa99c29` / `ef3b05a90632170268e55f7a5c532622f6d9cef3e6cbf5221fac75aa733e1bda`；完整opencode_reasoning模块实际31/31、0failed/0ignored，raw SHA256 `e084224c7a96df8437b80b2e93998abd49218bd461b5268d18ad933d69ea2133`。这些只证明idle.error typed ordinary投影的离线机制；真实失败事件未观察，不能称Source A或native三字段来源通过。EN/zh/tracing/COMPAT与三计划为本卡既定同卡文档，根仓最终I为8路径、production仍1路径；网站1页由root唯一owner串行承接FIX cf。网站typecheck/build root实际exit0，对应raw SHA256 `38e40aea10a235b64913aaad108f33fce8ae6d81da82886313a8fd72d4ee0f27` / `a3901b05e2fd699f3bc116161d95b1fe986a2edb7792c3126e75e43aafbfd1d8`；候选页SHA256 `287edb4afb51c5849a0671e34f8c8c7fdc0d898b812413e8c938999d6c675306`/0644，signed源交接和部署HTTP待。

**IDLE最终收据待root据实补齐：** 当前已应用五source/docs路径的fmt/diff/strict实际exit0（strict raw SHA256 `27ed297e64983359a3e7128122f595b7a5eec8625ba986d58d73ec37eba30aed`）；module31/31、docs30/30、architecture57/57、hook11/11均非零通过，后三raw SHA256依次为 `b9c4ccdad36fd39a7d73b855bf05fa0cda54214d7113184c105cb3ced95c8376` / `d202a33dc9321a243ae80fbf2a20ba1f13cdd60178983d1ee6d85e4285908f3f` / `194ded6e69b84e1dbcd9e43282f7f496aff3ceb176f8680bfd9c49680e2c09f6`。本次例行三计划正式应用后最后diff/docs同步=实际exit0：最终diff raw SHA256 e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855，最终docs-contract30/30 raw SHA256 ab172a47fe0401c7f348628472d83eed8b4a91f02794fd764996968550e97e0c，不把追加前结果自动记为新增计划字节验收；唯一Claude=IDLE-029-CLAUDE-PENDING；主仓signed+DCO/remote=IDLE-029-SOURCE-PENDING；网站signed+DCO/remote=IDLE-029-SITE-PENDING。这些文字是明确pending字段，不是SHA/成功占位证据。上述最终门关闭且Push后才登记IDLE source-qualified并把父源码分子59→60；当前Lifecycle=in-progress/Acceptance空、formal55不变。临时049最终含latest cat-file/FIX/IDLE/BASE新树full和全部C/D继续待，不复用COV25旧冻结full冒充。

**后续BASE实际场地：** OG-UPGRADE-029-01 VER2在本轮新建的干净隔离Libra原生仓（.libra、无.git）执行；root记录该独立repo的canonical路径/storage、selected官方29二进制SHA、同29 server/PID、ordinary trust与Required Seatbelt。Session必须绑定该独立repo；仅由root按既有授权显式选择/建立真实Session，验收recipe不自动新增模型调用。export→capture/import→checkpoint/export非零并逐项对拍prompt/model/token及既有files兼容门；SessionEnd显式恢复若使用仍逐实记。既往UNRUN/空内容历史保留，不能借新场地抹去或算通过；当前本新lane也UNRUN。不改变生产写集、AC/VER计数、native登记或Source A判据，不加入CI。

**当前临时本机安装收据：** `/private/tmp/libra-temp-install-20261014-r1/temporary-install-receipt.json` SHA256 `1bca5c4f834ffb1e632ada8ef6bed0b207bd2201683e808266cab33ae6f91e1d`。两个目标 `/Users/eli/.libra/bin/libra` 与 `/Volumes/Sea/libra/libra` 为同一193866248bytes/0755/SHA256 `1747731f8df72dcc6eb52e541056640440e1cdbf89eb762516d47f1edf830e00` 的本机debug构建，版本面仍libra0.30.48，基于signedFIX及当时尚未提交的IDLE代码；不是官方新release或049安装通过。原正式binary/marker已备份，临时official marker已移除。正式049实际验收后按用户指定curl安装并验证release digest/size/marker，再原子替换repo内第二目标，使两路径同release SHA/版本；若之后用户改动目标，先保留该改动再替换。本临时receipt不增加任务卡、版本提交或C/D PASS。


### 2026-10-14 16:15:36 UTC — 新增storage-only ordinary partial live修复；本机临时构建先行

IDLE实际唯一Claude PASS（0P0/P1/P2/5P3），raw SHA256 `829c06700313d1740ccad0c5ad1e3d9acf9816e2df8f2c0d2708a85dedebf1ee`；signed+DCO main `95ab8f35b3af7e8a5736c955212415ca93eebeb3` / cf `bc58acc2dbe057dc039fedfc3f3d432b9b4dd332` 两Push exit0、GitHub remote各同SHA，PGP header/DCO预检及trailer门实际0。收据为 `/private/tmp/libra-opencode29-idle-error-review-r1/signed-local-source-receipts.json`。已知源码60/70、formal55/70为本G09采纳前事实，IDLE5P3保留；当前新live源码/本机成功/full/正式C-D均UNRUN。旧Actions取消请求列表为空，不推断任何旧run已通过。

本G09只新增 `FIX-OG-PARTIAL-LIVE-01` 一条live direct checkpoint轴，完整sealed producer→snapshot current sourceHMAC→专用payload factory→emptyclaim直接store为五生产文件/四落点，8AC/8VER/M；不写agent_import、pending[_payload]、coverage_gate、native registry/issuer/CI。原始树任何originProof（含错型/未知嵌套）或typedcandidate禁止fallback；generic Unproven不是permit。默认ordinary/capture入口不放开，普通来源保持Partial/Unproven，nativeartifact/identity/ACK/Complete coverage0。只有已确认durable direct commit可给明确partial ordinary lifecycle收据；失败保持dirty/content-free pending/no成功ACK，不能重命名为Complete或seal partial replay。

**实施/DEP/G10新串行边：** `COV2-25→FIX-OG-NATIVE-IMPORT-01→OG-IDLE-ERROR-029-01→FIX-OG-PARTIAL-LIVE-01→OG-UPGRADE-029-01→COV2-22→COV2-08`。新卡先消费actualIDLE postimages，后BASE及所有相交COV Icore按 `DEP-OG-029-COV-REBASING` / `DEP-COV2-029-BASE` 从该五源保留修复、逐路径hash/mode重新交接。新卡不需要BASE或native3作为前驱。网站同owner `FIX→IDLE→FIX-OG-PARTIAL-LIVE-01→BASE→COV2-09→11`；root从actual cfbc58单页preimage串行写，不覆盖原dirty checkout。

> **历史规范：** 下段保留原结论；其historical import作为BASE/049前驱的部分，由2026-10-14 17:58:11 UTC G09显式替代，原020失败未改为PASS。

**REL-OG-TEMP-049新增精确成员与反序：** USER-TEMP-REL-049仍是唯一临时正式release操作，不新增release Task；当前精确成员含最新upstream cat-file、COV25及FIX/IDLE/本live卡/BASE，按上述串行链交接。失败定位逆序 `BASE→FIX-OG-PARTIAL-LIVE-01→IDLE→FIX→COV25`，已持久化/发布只前滚，新卡C/D明确继承USER-TEMP-REL-049。正式0.30.49最终实现树full必须含本live修复及实际后来源变化；用户既有纯版本同一实现复用例外保留。BASE实际GLM import020已失败，不能手工剥原provider形状造clean输入充PASS；新live成功不代替该门，普通import修复须另行精确规划/Claude采纳，故BASE/049 import门仍未闭合。原native OG27/28、SourceA与COV2家族C/D不由此次解除。

**用户当前本机临时交付授权：** 修复后先构建本机debug版并在独立owned仓复测，不等待BASE或正式049 release。本卡必要focused、fmt/strict、实际源码sole-Claude及源身份核对须真通过；根代码只由root应用/签推，私有prototype不等于实施或PASS。临时安装两已有目标及旧正式marker/backup按前条实际receipt保留，再记录新构建源码manifest/binSHA/size/version，不bump、不冒称官方release/full/C-D，正式049后按指定curl安装同源正式版。


**本G09 R2实际审查与规范修订：** 核心Claude实际PASS（0P0/P1、6P2/3P3），raw `69f387501d3a170f1382f31e85bd13783074177345f392b92c75ddfbb4ee0e0a`；仅设计/core，不是源码/模型/full/三镜像PASS。三镜像R1实际FAIL（0P0/P1、1P2/5P3），raw `132725fee31f79e4a8e5a93cc70d7fce6bf8a668f14eaad6e0c3aea0cd664a0c`；本R2补父修订历史row，core六P2具体wording与OG26窄例外/实施UNRUN。不同idle允许bounded whole-source重复；HMAC plainString不是snapshot自行验证scope的凭证，carrier绑定repo/libraSID/providerSID/kind后与actualrequest核对；info.id精确映射。已存在auth/scope/length及closed native/pending门作为ER04继承安全契约，不添第九AC。默认v2import Partial zeroBytes仍拒，existingexitvariants/ENTRY_RECOUNT不扩。本R2最终三镜像单Claude/signed采纳待；源码及所有运行新门UNRUN，不伪造零P2或actualimport通过。


### 2026-10-14 17:44:21 UTC — FIX-OG-PARTIAL-LIVE-01 实际 A/B 与用户临时安装

G09已由signed+DCO main `dea12cf6d1f41569e51851b8e8537fe0765be151`实际Push采纳。生产只改既定五源；共享live私有接口使用provider-neutral `VerifiedOrdinaryPartialSnapshot` / `VerifiedOrdinaryPartialBinding`及对应consumer，唯一mint仍在strict OpenCode producer，不扩generic Unproven/native/pending/V2import权限。ARCH精确增加同一match不同arm的两个sealed ExportStage handoff，单settle/单checkpoint消费与alias拒绝仍闭合；CheckpointStageExit仍只由原class helper构造。未改SQL、CI、CLI、native issuer、pending或agent_import。公开wire enum/serde五定义及旧pending decoder逐字节不变；ER04 validator/readback机械清单不替运行。

实际六VER new exact各1/1，最终live模块15/15（包含terminal成功/durable replay/过期content-free pending）、ARCH58/58、docs31/31及ER04旧selectors14项各1/1；reasoning21通过/1 ignored、OpenCode32、snapshot26、checkpoint34、pending31通过。`cargo +nightly fmt --all --check`、`libra diff --check`与`cargo clippy --all-targets --all-features -- -D warnings`实际exit0，strict3m49s。网站同owner单页typecheck/build0。首compile两错误、早期中断focused/strict、ARCH undeclared quote与55/3、57/1失败、store fixture路径与CAS竞争ref误断言失败均保留；只最终实际运行记通过，不将取消或synthetic称成功。单元测试是纯parser/key/store/SQL lease/AST seam，不调用OpenCode Session/exporter/CLI/账号/模型，不新增CI采集。

macOS真实一次 `volcengine-coding-plan/glm-5.3-flash`，隔离repo `/Volumes/Sea/libra-partial-live-ver8-ar5mj9gh`；官方29二进制SHA `7f46ba1dad86b730903d9adf54015bb4e706fc1ab6a247affc5a635cd8a663f3`，ownedstdio同PID/endpoint、cwd和PWD先核。provider `ses_ec483a846ffeAQghpE0mS079I7`；自然自动生成checkpoint（初观`3e6ef7e6-f77b-5f04-b552-5b3f49fe9f0f`及后续idle `e08d6f2c-c04a-5b5c-9bc4-b79bc567f386`），后者普通export **1369 bytes**、SHA `16d44f3f9e527a49b27bc509d03c5239691ce4ea79fdd260934b77214c2a1191`，raw=false/truncated=false、partial/source_authorization_mismatch、trusted_export/current HMAC。真实unsupported reasoning state及关联text/time已剔除，普通user/assistant与message/part坐标读回对拍；artifact/identity/Complete coverage/pending/inflight均0，job clean/processed与实际generation一致。无manual hook/import/repair、仅一次模型。旧官方48 SHA `26878d8b52ad8ba87789a07e8823cbcc2c4fa554f19fafb8197e0a431e13686e`正常reader实际读回相同1369bytes/SHA。Source A仍UNPROVEN。

原private verify最后一步误把旧show `data.checkpoint.checkpoint_id`当平铺字段，实际CLI与两份payload均已成功，原FAIL/VERIFY.once/STATE不抹去。只读R2复核既有证据与exact-owned DB（不再CLI/模型/hook）实际PASS；receipt `/private/tmp/libra-partial-live-ver8-verification-resume-r2-vhzhorvj/VER8-RECEIPT-r2.json` SHA `9575ffd297f68b60e25a1500dbc133c6d06690dd80dd6034135713689029de60`。这不将失败脚本自身改称通过。

用户要求的新临时debug artifact已实际安装到 `/Users/eli/.libra/bin/libra` 与 `/Volumes/Sea/libra/libra`：均193956968bytes/0755/SHA `a70adfbbf474a741f16497384b7c833214979507dfba4367f37bcd2e566dfd1f`、`libra 0.30.48`；不是正式049或版本bump。源manifest SHA `052dda0aeedccec8def356cff792638d556f1a021c3aea52dbab6463f07c57c6`、codesign验证0，旧临时和原正式备份保留、无official marker。安装receipt `/private/tmp/libra-partial-live-temp-install-yxw0xwia/temporary-install-receipt.json`。正式049通过后仍按用户指定HTTPS curl安装，验证正式manifest/digest/marker后替换这两个路径。未改用户Demo数据。

**历史live签前收尾（对应2026-10-14 17:44:21 UTC收据，原文保留，已由当前实际signed交接及G09范围采纳覆盖）：**

本卡保持in-progress/Acceptance空（仅A/B locally accepted，当前三计划单审与signed root/site Push待）。实际签推后父source61/71，formal55/71；不将temp/fixture/本机A/B充C/D。BASE真实import仍020拒绝、相关必要后续尚未另行采纳，不因本live成功或旧reader读回放行BASE/049 import门；最终049新实现树双envfull、四平台/安全CI/稳定release/Homebrew/CDN/指定curl及native Source A都未完成。下一步先完成本卡source交接与三计划收据，继续普通29基线及import边界的必要后续；不重触用户Demo。


### 2026-10-14 17:58:11 UTC — G09：BASE自动捕获基线与DEFER-OG29-PARTIAL-IMPORT范围修订

**依据与效力：** 按用户既有“同类复杂问题延后、基础采集优先”和无需再问默认推荐授权，本次提出明确范围修订；只在最终三文档实际sole-Claude PASS、signed+DCO main采纳后生效。它不是认定旧门已通过，也不使BASE/049自动通过。本修订前日期记录、raw失败/UNRUN、旧完整门原文作为历史保留；旧关于“BASE/049 import门仍未闭合”的规范在本采纳后由本段前瞻性替代，而非修改其当时结论。

**范围与计数：** 原OG-UPGRADE-029-01编号保留namespace＋真实自然自动capture/checkpoint/普通export主轴，原8AC/2VER及1 landing/1生产文件、原I/R与逐文件文档/网站义务不变；仅AC5与VER2移除historical import正例。AC1/2 pin/namespace、AC3/4实际同29 binary/runtime、AC6/7/8真实prompt/model/token投影仍逐项实际验收，files原兼容控制保留。未完成项保持unchecked/UNRUN，不因计数8/8文字把8项写成已通过。DEFER-OG29-PARTIAL-IMPORT登记为计划外未实施两轴，不新增Task或完成分子；父71卡/source61/71/formal55/71，COV26卡/source13/26/formal0/26保持，BASE自身待门。

**延期与负例：** 原GLM unsupported-state历史import020仍未修复，默认Unproven拒绝；live自然自动partial checkpoint非零成功仅证明live，不是import PASS，也不是native/Complete claim/Source A。不得修改原provider字段、清洗源、补假证据或用synthetic对象伪造正例；无state ordinary若另有真实通过只按实际源单独记。两轴helper认证partial胶囊与direct-only导入持久化由计划外遗留承接，未来另做具体设计/写集/单Claude/原样实际正例，当前不实现，不授权新增模型调用。

**DAG / DEP / G10：** 严格顺序仍为COV2-25→FIX-OG-NATIVE-IMPORT-01→OG-IDLE-ERROR-029-01→FIX-OG-PARTIAL-LIVE-01→OG-UPGRADE-029-01→COV2-22→COV2-08；无新边或卡。DEP-OG-029-PLAN及SITE、DEP-OG-029-COV-REBASING与incoming DEP-COV2-029-BASE只按修订后BASE最终A/B/fmt/strict/单Claude/signed源码/hash/mode交接，不把计划外导入修复作为incoming前驱。相交COV2-05/06/11/15/18及Icore从最终源精确重基，原cursor/candidate/隐私/字节对齐oracle不减；若执行这些卡需要新consumer，应在其各自范围实现或明确复议，不能引用本延期跳验。root共享单写者、干净cf owner及FIX→IDLE→live→BASE→COV2-09→11站点顺序不变。

**REL-OG-TEMP-049 / USER-TEMP-REL-049：** 唯一临时patch0.30.49操作，成员保持latest upstream cat-file、COV2-25和FIX/IDLE/live/BASE最终实现；显式排除DEFER-OG29-PARTIAL-IMPORT能力，发布说明和网站必须披露unsupported-state历史import仍拒绝/未修复。此排除只改变发布范围，不证明release可执行：最终实现树focused/fmt/strict/sole-Claude及双env full、实际最新SHA安全CI、8releasejob/四平台签名manifest与资产、CDN/Homebrew、网站typecheck/build/签源/部署HTTP、指定HTTPS curl安装及installed cat-file smoke仍全部必需。纯版本同一实现复用例外不变；未完成v2入口不可达证明、COV2家族C/D和Source A硬门不变，不以本临时发布complete COV family/DEP-OG-13。

**安全与证据边界：** 没有生产代码/默认ordinary或native权限/旧V2 Partial零bytes/pending replay变化；G09规范修订自身仅三计划，同次收口另含原FIX范围内的Unix测试helper cfg前滚，后续BASE仍只写其原exact I，不能顺带实现延期6文件。真实OpenCode Session/account/model/hook/CLI只macOS本机、绝不加入CI，不新增自动模型重试。既有复杂失败不被延期为PASS；最终全量若暴露实际回归仍须forward-fix。未实施能力在最终说明中明确列为限制，已发布只新patch前滚。

**历史草案当前门（原草案时点，已由84bb实际采纳收据覆盖）：** 本段草案未做正式Claude、没有VCS/模型/cargo/真实验收；root采纳时须补实际最终三文件manifest、sole-Claude raw/结论、signed+DCO main与remote同SHA。该规划采纳不改变BASE unchecked及049 C/D状态。下一实施仍为BASE current namespace和自然自动capture内容门，随后COV2-22；延期不占当前写slot。


### 2026-10-14 18:03:50 UTC — 已签 live 源码交接及 Unix 测试 fixture 前滚

FIX-OG-PARTIAL-LIVE-01 实际 signed+DCO main `d8422534b07724349591d53aa445ce50c38453f5`（精确14路径）与网站 cf `a80d3ff9d2d194b739da6148dde3297d325f1a7e`（精确1页）均已 Push exit0、remote同SHA、PGP header/DCO/逐blob同一性核验通过。三计划例行收据实际sole-Claude PASS，raw SHA `e30b717ec6754916b7fd87a171346357227abfa526081d797e7f54ec6cd35557`，0P0/P1/P2、5P3保留。本卡source-qualified，父source61/71=85.92%、formal55/71=77.46%；COV source13/26=50.00%、formal0/26。此前“签推待”是当时事实，不回改旧收据。

当前 d842 的 Windows 必要安全CI job `116010393408` / run `38609801760` 实际失败：四个仅Unix测试消费的常量、`VerifiedPartialStoreFixture`及其impl、`verified_partial_fixture_snapshot`未同步Unix cfg，`-D dead_code`导致lib-test编译失败。原始日志SHA `d22b0a896e123b351caf01e0437290a88a1c09aba618539491b1bf513533d481`；属于确定性代码问题，不作为network重试。只在原FIX写集 `capture/live_checkpoint.rs` 和 `capture/snapshot.rs` 的测试helper补7处cfg属性，无生产行为、旧测试删除/ignored、CI或新卡/写集扩张。当前 macOS fmt/strict/focused、单Claude及本次signed Push仍待；Windows只有后续实际新SHA成功才可关闭，不能以本机编译代替。

G09规范修订自身仅三计划；同次收口另包含上述原FIX范围内测试cfg修正。BASE全部8AC/2VER仍unchecked，本次不预增完成。已有真实自然自动采集的prompt/model/token/files只读核对输入可作为后续BASE候选：prompt1、typed extraction.model同原native assistant、五token input7681/output32/reasoning14/cache0/0与持久化7727总数一致；files仅零工具/空路径对照，不是修改文件正例。顶层hook model与native model不同，不能拿它冒称实际模型；只用已核typed extraction.model。该核查未新增CLI/服务/模型，未重跑live或import，不独自构成BASE PASS。GLM历史import020依G09明确延期，Source A仍UNPROVEN；正式049最终full/安全CI/四平台/发布/安装及网站门全部保留。


### 2026-10-14 18:10:19 UTC — G09 R1 P2 规范澄清（最终字节复审待）

R1实际sole-Claude为PASS（0P0/P1、4P2/4P3），raw SHA `6921a0cb37742edc903c5f106104f80bdf507a1440e7270031936ec864aed479`；保留该结论与全部意见，不把其写成零P2。最终复审另核完整长行覆盖，不用“读到EOF”替代完整字节覆盖。

1. **source-qualified与CI分层：** source-qualified是本计划内部的最终A/B、适用focused/fmt/strict、唯一Claude、signed+DCO source Push及hash/mode交接；Windows/其它远端CI属于ER-04 D组，按用户no-wait授权不阻下一张源码卡开工，但失败必须保留且阻正式release/C-D。因此d842的内部source61/71成立，Windows失败不记通过，formal仍55/71；本次前滚新SHA必须实际Windows绿后才可关闭该远端门。DEP-OG-029-PLAN/DEP-COV2-029-BASE按内部source交接，不声称D组已通过，不把source-ready充formal complete。

2. **真实证据复用范围：** 1369-byte VER8摘要本身只支撑对应的AC5/6/7/8普通内容、prompt/model/token及files空对照，不能单独支撑AC1–4。AC1/2必须由BASE新增常量/namespace及VER1实际核验；AC3/4须另核原官方29实物version/SHA/size、同一server PID/endpoint/runtime与对应exporter的独立precheck原收据。BASE最终Libra源码到原实际capture运行时的逐路径同一性、仅测试cfg/无运行consumer常量变化及新候选普通读回必须由root机械核实并经最终Claude接受；任何未证实项维持UNRUN，不声称不同binary SHA相同。不得用新无state源替代原GLM失败。

3. **强制范围披露：** BASE的ER-06 Deliverables必须同步EN、zh、tracing、COMPAT与唯一cf agent页，明确“含不支持推理状态的历史OpenCode Session导入仍以LBR-AGENT-020拒绝，修复延期”，并保留自动捕获可保存安全ordinary Partial、native Source A未证明的边界。新增既有docs-contract函数`opencode29_ordinary_extension_is_distinct_from_frozen_native_source`须逐四root文档断言该披露；网站逐页验收由root核与EN新增段逐字相同及实际typecheck/build/签源/部署HTTP，不把外部网站文件引入CI。USER-TEMP-REL-049的实际CHANGELOG0.30.49说明与最终网站须实核同一限制；不新增本卡特有AC/VER或版本写权。

4. **旧规范显式历史化：** 原“REL-OG-TEMP-049新增精确成员与反序”每份现存未引用正文前插入本G09替代标记，保留原文字与旧FAIL结论。日期快照仍保留；本次仅前瞻性调整发布范围，不修改曾经失败的import门结果。

**历史G09签前closing（原文保留，末句待审/待签状态已由84bb实际采纳收据覆盖）：**

AC5的对应性明确只指validator安全普通投影；snapshot保持Partial/Unproven，unsupported state及关联reasoning text/time被擦除，不承诺原始完整性、typed/native归档或Complete coverage。文件维度仍仅empty control，不记修改文件正例。最终G09仍须实际单Claude复审PASS及signed采纳，BASE仍全unchecked，不预授049或COV完成。


### 2026-10-14 18:10:59 UTC — 测试cfg写法与既有AST守卫对齐

窄前滚首轮fmt/diff实际0、live15/15、snapshot26/26通过；ARCH实际57/58、1失败，raw SHA `50a6986e1eaec4ca69423502645a6304c915d80f31aedde7b21b1d815abc6a03`，保持FAIL。原因是既有provider-neutral AST守卫只识别独立`cfg(test)`，把新`cfg(all(test, unix))` helper误算为生产AgentKind入口。改为保留原独立`#[cfg(test)]`并另加`#[cfg(unix)]`，Rust含义同为test AND unix；四常量/fixture struct/impl仍各Unix限定，共七个新增Unix属性。不修改或放宽AST守卫/旧计数、不新增生产provider分支。最终ARCH/docs/fmt/strict及完整长行单Claude复审仍待，Windows须新SHA实际验证。


### 2026-10-14 18:27:15 UTC — G09实际采纳、测试fixture前滚交接与BASE开工（例行事实同步）

**实际signed交接：** root已核signed+DCO main `84bbde7bd6a5f12dee2477755d0a955798518413`，parent为`d8422534b07724349591d53aa445ce50c38453f5`；实际main Push exit0，remote精确同SHA。五路径PGP header/DCO及blob/mode等于最终sole-Claude R2 manifest：R2 raw SHA `4ab4911f9253eb659bb03cf6b49fb4479e92a74145ea1ee8f982e1a12df05190`，actual PASS/0P0/0P1/0newP2；R1 PASS的四P2已由R2逐项关闭，六个P3经父级裁决接受，不伪称历次零P2。此门只覆盖84bb已签的原五路径，不借其PASS覆盖后续BASE实现或本例行记账新字节。实际交接收据时间2026-10-14 18:23:51 UTC；网站source前驱保持signed cf `a80d3ff9d2d194b739da6148dde3297d325f1a7e`，BASE网站新段、发布部署/HTTP仍须自身实际门。

**前滚与QA：** 旧d842 Windows job `116010393408`真实FAIL（七项仅Unix测试fixture在Windows引出dead_code）；root只给四常量、fixture struct/impl、snapshot test helper共七项增独立Unix cfg，保留原cfg(test)，未改生产运行函数、CI或原测试计数。首轮ARCH真实57/58 FAIL/raw `50a6986e1eaec4ca69423502645a6304c915d80f31aedde7b21b1d815abc6a03`保留；最终QA七门实际exit0：fmt、diff、live15/15/raw `1e1be5ea5b783a1f18807d95a7675170fd804f672a9cf9afe57706f7376823de`，snapshot26/26/raw `731a884a4651dc09e68170e1b3c4195d19fbbc4330ac77c3ee52befc0ed9c9cc`，ARCH58/58/raw `8025733a319e6249dbdb444be67eb0d09686e8e109a0cc5c87b59e712c914246`，docs31/31/raw `9066fed57587926c671d3af7e626a40835d53e6c2e4f7fead428233e4ce87d23`，strict all-target/all-feature -Dwarnings 18:12:46→18:22:01 UTC实际0/raw `3a9e6d6a12396ff911c7b47353de076c7d06ce158532029d0b33dc6ff7eae7a4`。这是84bb本机适用门，不是新SHA Windows或BASE/049 full验收。

**CI仅开工观察：** root在BASE开始时只读看到84bb CodeQL `38614865589`为running、base `38614865565`为queued；Quality `38614866097`和cancel workflow `38614865604`实际success。它们不证明base/Windows/完整安全CI通过，后续须按精确SHA和job真实结论另记。本例行同步未重新查询CI。旧d842 base `38609801760`的取消请求实际成功，terminal结论当时未查；不能写已cancelled或PASS。所有failed/cancelled/queued/running和未运行门不得充PASS，用户新commit取消旧Actions策略保留。

**当前范围与下一卡：** G09已前瞻生效，DEFER-OG29-PARTIAL-IMPORT仍计划外deferred/未修复；GLM unsupported-state历史import020仍FAIL/默认拒绝，不能由live成功、无state源或洗源替代。BASE现in-progress/Acceptance空，只实现official29 namespace＋自然自动capture/checkpoint/ordinary export的安全prompt/model/token与files空控制；全部8AC/2VER仍unchecked，AC1/2/VER1、本机VER2逐项证据同一性与内容对拍、同卡四root文档/网站披露及最终QA/Claude/sourcepush另录。父source61/71、formal55/71，COVsource13/26、formal0/26不增；BASE签推与source-qualified门闭合后才转COV2-22。本次不新增任务、不改其它卡的生命周期/AC/VER，不记source62/71。

**历史收据与发布门：** 页首较早“草案待采纳”、live签前closing和12/26段仅为其原时点历史，原正文/失败不回写。84bb当前13/26和本次实际采纳为现行事实；旧full只覆盖旧冻结树。最终049仍要求最终实现树适用focused/fmt/strict/sole-Claude/双envfull、实际安全CI/8releasejob/四平台签名manifest/CDN/Homebrew、BASE网站typecheck/build/签源/部署HTTP、指定HTTPS curl安装及installed cat-file smoke；Source A/native GO、V2 Partial零bytes、pending、未完v2入口不可达与COV家族C/D门均不变。复杂OpenCode Session/account/model/CLI只macOS本机，不进入CI；Linux功能仍由用户后验。

**同文件文档guard必要对齐（本卡范围，尚非实现/通过）：** `tests/compat/agent_docs_contract.rs`仍在BASE原I，允许新增`opencode29_ordinary_extension_is_distinct_from_frozen_native_source`，并仅在既有`opencode_verified_partial_live_capture_keeps_native_and_replay_closed`（84bb现1334行起）中把现1389行`BASE's real import gate`和1459行`BASE 真实 import 门`这两项过时文档披露term，改为与G09一致的“历史unsupported-state import仍LBR-AGENT-020拒绝/修复延期”准确断言。最终词面/两行postimage须由文档owner与真实四root文档对齐，root精确diff和最终sole-Claude裁决后才应用旧fn；不为守旧字符串把内部BASE实施门写进产品文档。该fn其它privacy/native/Partial/V2zero等全部原断言保持，其余30个既有fn逐字保持；旧31fn数量保持、新增BASE后完整target预期32（不是已PASS，须实际运行）。先前私有“整旧31fn字节prefix冻结”只是未采纳草案，本次明确以这两term必要修正替代该过宽主张，不涉及原ER04冻结JSON/MANIFEST/native fixture/decoder字节。此为ER06同一文档轴，不新增文件/生产/卡/AC/VER/landing或放宽安全守卫。


### 2026-10-14 19:08:52 UTC — BASE例行事实同步，完成分子不增

本轮新owned自动采集1345B/prompt1、GLM extraction model及五token7678/29/35/0/0(total7742)、空files控制的实际收据与剩余门，见父计划`OG-UPGRADE-029-01`本卡及同日最新例行收据。VER2收据SHA256 `c02902d46324ec28ec2968e8d13132fbc58479b54baccaf42dd7acbdd273caae`；独立对拍SHA256 `0a1de7f096516737d9ecc2545592ff516baf73716714a2aab5df2a977919d924`。1次模型、0次manual hook/import/repair，raw=false/truncated=false、旧48读回同bytes；官方29版本/SHA同一但interactive PID50453已退出且collector独立租约PID未存档，不声称同PID。645构建输入与12旧protected、双MANIFEST prefix同一，docs旧31仅两项获准term同步、新BASE为32；本轮snapshot实际49/49而非旧26。strict已exit0；full/最终Claude、signed source/site与049 C/D待，父source61/71/formal55/71、COV13/26/formal0/26保持；旧1369B及import020 FAIL/deferred/default reject、Source A UNPROVEN不改。

### 2026-10-14 20:10:13 UTC — BASE source-qualified交接与临时049例行事实（不改规范）

**实际源码与裁决：** `OG-UPGRADE-029-01`的8AC/2VER和既定mandatory门已按本卡实际证据关闭A/B，Lifecycle为`in-progress / locally-accepted`、source-qualified，不是formalcomplete。sole Claude Code R2 raw SHA256 `1da1fc13e2798ab51918f09f83de5499e87f204deea9614612b6ea6c01ca9c56`实际PASS（0P0/0P1/0P2，8P3由root接受）；此前BASE R1为PASS并含1P2（已修复），更早失败及其余历史观察保留。本轮12root+1site的postimage/mode均与实际R2 manifest同一；该PASS只覆盖已签原字节，本次三计划新增事实随发布delta再交soleClaude，不借旧审查预授新字节。root实际交接收据`/private/tmp/libra-base29-final-source-claude-r2/SOURCE-QUALIFIED-HANDOFF.json`，SHA256 `80a5018684f5f61a63381e999e38ed3b3f88e48f990502dac15d3b7f1a038cc9`。

**最终实现full真实终态：** 同时加载两份本地测试环境的`cargo nextest run --all --no-fail-fast --retries 2`，2026-10-14 19:13:00→20:03:39 UTC实际exit0：`Summary [2009.557s] 10133 tests run: 10133 passed (2 slow), 23 skipped`；2slow均最终PASS，实际0retry/0flaky，23skipped不计通过，不代替本机OpenCode/账号/模型或22例家族Session验收。raw SHA256 `24febb2eba3d2aad09c6f516988e06e9ae127ad8dd96b362fbda4ea45671fdb2`；`RESULT.json` SHA256 `da15c86a546db964843cc9ccb0ab64554e248ec471a03b53b6ef23be6f8a1ae3`。全程2043项源码/测试/配置/文档输入的前后SHA/mode相同；该full包含最新5c cat-file、FIX/IDLE/live/BASE当前最终实现，取代旧2b+COV25冻结full作为本次实现验收，旧记录仍按原边界保留。仅后三计划例行事实及纯版本faces/self-lock/发布说明变动，可依用户原授权逐文件证明实现同一后复用此full；实际实现变化仍须重新验证，不解除计划最终全量要求。

**signed源与计数：** main `1e2177e11029e28e3c81afbe6ec9f1609cdde7da`（parent84bb）和网站cf `93b10519e1943ea3eea7c943e816418e92b1ca0b`（parenta80d）均signed+DCO、Push实际0；root实际Libra ls-remote各terminal0核同SHA。父source从61/71变为 **62/71=87.32%**，formal仍 **55/71=77.46%**；COVsource **13/26=50.00%**、formal **0/26**均不变，不新增Task/分母。后继COV2-22可消费该精确source链按原DAG/共享I逐hash/mode串行重基；本次049冻结发布期间不并发Push新main源码。网站cf `93b10519e1943ea3eea7c943e816418e92b1ca0b`已实际`pnpm deploy:cf` exit0，765 tracked source前后SHA/mode同一；Worker `f446c1f2-117d-4095-939f-b4db87037512`，生产`https://libra.tools/en/docs/commands/agent`实际HTTP200，BASE整节六个完整段落在去script/style的visible HTML中规范空白逐段匹配全true（不是整页pixel比较）。部署RESULT SHA256 `057564431f1382edcfe37c59b17cfde40f94721c9b81bf9efe5fe8a20803e603`、HTTP-RESULT SHA256 `018d8899dc7450104205f0683c98d067716e06ea167a98352f7326870912118c`；旧同目录RESULT的HTTP=PENDING只是部署完成/HTTP测量前历史，后续HTTP-RESULT已实际覆盖。

**继承遗留明确延期，不计卡/不记修复：** Mega2 interactive `--ref`显示所选ref但变更写trunk的既有风险，actual passive Claude raw `ca9acecec67478006ce9cab2e8acf2cb2a5a0200fb6b16111b8a705d67efe99c`为literalPASS/0P0/P1、1P2/5P3，按用户外围难点延期政策保留UNFIXED/deferred。最新upstream5c cat-file actual Claude raw `8ff6548ff34d745d407239b724c9e919f510f714bb5db66b4915058f5c39a292`为literalPASS/0P0/P1、3P2/2P3，root按用户单Claude PASS规则接受；buffer队列/损坏对象及EPIPE专项测试缺口、长期blocking read、既有BrokenPipe exit0及更大buffer丢失窗口、trim文案精度、无界输入queue/line观察全部未实现修复/延期。最终full中现存cat-file用例实际通过，不宣称补齐缺失场景。默认CLI的main→cli::parse静态路径使用Tokio multi_thread；这只澄清默认runtime事实，不证明任意embedding或取消/阻塞边界已修复。

**版本操作事实（root执行，不在本三计划候选写集）：** root已对Cargo.toml/install.sh/install.ps1三个版本faces、Cargo.lock自包及CHANGELOG作最小0.30.49 bump；`cargo update --workspace --offline`实际exit0，lock仅libra自包48→49、无依赖漂移。该动作不等于版本guard/发布审查、签推/安全CI、tag/稳定发布/8job或安装通过；本候选没有改任何版本文件。

**发布与权限边界：** USER-TEMP-REL-049仍是唯一临时patch0.30.49操作。最新SHA必要安全CI、版本面/fmt/strict/发布delta单Claude、8releasejob全部success、四平台签名manifest/资产、CDN/Homebrew、用户指定HTTPS curl安装home正式目标、以同一正式资产原子替换仓库临时目标及installed cat-file smoke均尚待实际结果；不能因本次full或source-qualified记发布/C-D/全计划complete。历史unsupported-state OpenCode import仍LBR-AGENT-020 FAIL/默认拒绝且计划外延期；Source A/native GO仍UNPROVEN，旧V2 Partial零bytes、pending/native权限与COV2家族C/D/DEP-OG-13均不放宽。复杂OpenCode Session/account/model仅macOS本机，Linux功能用户后验；不将未运行/失败/skip/synthetic记PASS。

### 2026-10-14 22:28:49 UTC — USER-TEMP-REL-049 四卡实际C/D收口

USER-TEMP-REL-049 / 0.30.49 的实际C/D已由root按完整收据 `dad01d952f7ce629354118264a4926b048693bf851258e4cd14beb9853727f8c` 逐项裁决关闭；release source/tag peel为 `04617cd2a3ffbf3a3e68ce4ddfecf94b78bc6574`，release run `38639329379` attempt `2` 的8个job全部success，当前SHA CI/CodeQL、四平台签名manifest/资产及CDN、pinned Homebrew、网站HTTP200/visible六段、用户指定curl home安装、同正式Darwin资产原子替换root目标及installed cat-file/既有BASE1345B读回均有hash绑定的实际证据。

**精确覆盖四卡：** `FIX-OG-NATIVE-IMPORT-01`、`OG-IDLE-ERROR-029-01`、`OG-UPGRADE-029-01`、`FIX-OG-PARTIAL-LIVE-01`。原逐卡A/B、单Claude、signed source/网站交接及fresh BASE证据依现行四卡映射继承；不新增模型/Session或历史import验收。仅四张父卡由locally-accepted转done/complete，父formal **59/71=83.10%**（原55增加4），source仍 **62/71=87.32%**；COV source **13/26=50.00%**、formal **0/26**，71/26分母不变。COV2-25/COV2-REL-01/DEP-OG-13、OG24..26的FAST-OG-04、Source A及native三字段未因此关闭；Source A UNPROVEN，历史unsupported-state import020仍FAIL/默认拒绝/计划外deferred。真实provider idle.error正例仍未观察，本卡typed机制按原7AC/3VER验收，不把成功Session或synthetic称真实错误/native来源。既有FAIL/cancelled/skip、Mega2与cat-file延期观察保留。

**实际身份：** signed manifest核验后的Darwin binary SHA256 `fffd723d0997d2cc0a2fccad7e31c0ae0d19f55d648ed635b3636f1ac051409e`、`79866176` bytes；home与root两个安装目标为同一正式资产。BASE原saved checkpoint `7d465f02-26f6-5fb8-a1d6-72164ac425f2`普通读回1345B/SHA `96583a9c4f4db01efffee8a6a31fe9f4a791ed866cda9c2a6893ebf8fd422d4a`、raw=false/truncated=false/native artifacts0。完整本机C/D收据 `/private/tmp/libra-temp49-parent-complete-r1-tdqm0h5x/USER-TEMP-REL-049-COMPLETE.json` / SHA256 `dad01d952f7ce629354118264a4926b048693bf851258e4cd14beb9853727f8c`；它只证明所列原门，不追认任何旧失败。

### 2026-10-15 — COV2-22 采用候选：本机机制与条件源码交付

root 最新 0.30.49 十四门及 `FIX-OG-NATIVE-IMPORT-01`、`OG-IDLE-ERROR-029-01`、`OG-UPGRADE-029-01`、`FIX-OG-PARTIAL-LIVE-01` 四父卡实际 done/complete 仍以本文件原 22:28:49 UTC 收据 `dad01d952f7ce629354118264a4926b048693bf851258e4cd14beb9853727f8c` 为准。父 formal **59/71=83.10%**、source **62/71=87.32%** 不变；本候选没有回退 049 实际安装/签名/CDN/Homebrew/网站证据，也不把它们误记为 COV 家族 C/D 或 Source A。

COV2-22 两生产 owner／三行为文档采用已受测精确后像；两计划只增加原 8 AC/6 VER checkbox、lifecycle 采用候选与例行事实。原 21 unique exact 各 1/1，pending70/70、payload31/31、architecture58/58、docs32/32、fmt/strict 实际0，strict raw `8a0830ace293d01c005fcffde899cc5196399034c7539c65cdee82d177238f68`。完整 8AC/6VER 对应与逐路径前后 SHA/mode 在 COV 本卡正文；原判据、命令、I/R、DAG、Granularity、冻结 oracle 不变，不新增 Task、分母或豁免。

保留原 r4 full exit100（10133 passed/6 failed/2 slow/23 skipped，六项各三次 FAIL=18 failed attempts/12 additional retries），raw `3b6c0c7ea30bd73c4f61460aff7204d3499e1be86113f5553499c2cc38a954fd`。原私有纯源码目录缺真实仓库上下文，引出一个 macOS exporter storage_path 和五个 serial classifier root-precondition 失败。root 仅授权真实私有 `libra init --vault false --initial-branch main` 修正测试 harness，DB/HEAD 有效、2043 原文件 SHA/mode 不变、无根元数据/凭据复制、无 commit/push/remote/全局 vault 或默认额外 untracked 文件；原六项各 Nextest exact 1/1，并在下次 full 均实际 PASS，不改源码/registry/阈值蒙混。

双 env r5 `cargo nextest run --all --no-fail-fast --retries 2`（jobs2、threads4）实际 21:57:47.085756→22:26:35.513934 UTC exit0：**10139/10139 passed，2 slow、2 leaky、23 skipped，0 failed/flaky/retry/timeout/abort/execfail**；raw `59e34e861d296be52474835a559ebb259f58e4f8501836f8311d6c1136382cec`，完整收据 `87594346f55f6c724e639415e64324cc6ded20581b1b52132204606da50832fa`。两条 LEAK 分别是 authorized_read 的 `helper_report_accepts_evolving_offsets_and_cumulative_lengths` 和 command_test 的 `diff_plumbing_outside_repository_is_an_error`，后续同环境各一次原 Nextest exact 均 1 run/1 passed、0 LEAK/重试（raw `faf7f5af66d55e6662f9723b409896d99244f88e26e3087d2638f253067db95a`／`6adced633bc8ecdb279907251f1d6206efba540999ef403311e82ca99c421a1c`）；不擦除原 full 两条警示、不证明原因、不新增零 LEAK 门禁。原 r4、VER3/VER6 错误 fixture/Python-only KAT、历史 sampler r1 编译失败均保留。

VER6 的 expected 仅来自原 `7efcf868d49a5183e7f0851a89abebee91357155`/0.30.46 完整旧源码 writer 实际 seal→persist→commit→load 后先行封存的原存储字节/MAC（receipt `1bb278f923d0f5e3ab83a1deba498b056218553cd12878eac676f0f39a18ed11`、原 2104B envelope SHA `bf87589384983e2ad5ae74f3773f8459c9a20c56e4736775eb3129fe52a005ef`）；只加采样 test、不改旧生产/依赖/其余2029文件，不用当前 serializer/loader/重签生成期望，不称历史发布 binary 或真实 native Source A。冻结 decoder `96d71660e8b75d61cff2dccaa59a410b90b499afd63e10c9dcc2cc678312e32c` 与原 v1 writer/reader 不变。

root 唯一 Claude R1 实际 SOURCE PASS、P0/P1/P2=0，raw `9cd81f9801f5313ac59813f796d7008736a8c5436d160a4b712b6b93b27ea020`，root 已核七 diff 完整 Read。该审查只覆盖原受测后像；本次新增两计划事实仍待 root 最终文档单审、父级裁决、本次实际 signed+DCO main 交付和 remote/hash/mode 同一性，不伪造未知 SHA 或提前 source-qualified。受测 2043 文件前后与诊断后同一（manifest `8592fc2bcc13c4a2eac6cac205f49eaba553b988697ccacfe8eabda75227afe9`、ZIP `fb700edeeb8927c794b023befe2ea005be3c363de0b834cc52706d1ca7b016ea`）；采用后的两计划及 root 父计划 049 纯事实已不同，**不能声称新的完整2043字节与 full 相同**。受测实现/测试/config/原行为契约及三行为文档的适用性由逐路径程序核对和 root 最终裁决证明。

仅在本次采用实际 source qualification 完成后，COV source 从13/26变为 **14/26=53.85%**，formal **0/26** 保持；此前仍13/26，本私有候选不自行增加分子。父 **59/71 formal、62/71 source** 保持；Source A/native 三字段、历史 unsupported-state import020 FAIL/default reject/deferred、Linux 功能与 COV2-REL-01/DEP-OG-13 不放行。之后只按原 DAG 推进 COV2-08，本次未实施该卡。


## 2026-10-15 G09：OG24 native.3当前修订与跨计划共享写权

| 对象 | 当前规范状态（不是新测试/模型PASS） | 下一步/独占写者 |
|---|---|---|
| OG24 `.3` | 同一原6AC8VER封闭producer-profile版本维护；旧2source收据真实留历史，当前source暂扣，parent61/71；formal59/71不变 | SRC029完整来源门后、真实sharedgrant才由root唯一落19rootI（2行为+2cfgtest+1commentsrc等）/原cf独立owner单页；attest只读；原8commands不变，AC2/6仅声明澄清 |
| DEP12-SRC029 | UNRUN/UNPROVEN，参父本日G09 S1–S8；不重开doneNPR08（原9AC） | source/archive/当前全68（42+23+3）及1705nativeclosure/依赖assets/SDKindependentofficial29expected/repro实际门；candidate不立gold，旧311a/6291不改 |
| DEP12-RUN029 | UNRUN，无stable49/COVstable依赖 | exact OG24.3 signedmain **本地构建非releaseLibra** binary/sourceSHA/size/version，ownedHOME/XDG/config/freshregistry；真实selected3/plugin/transport/freshkey/epoch/priortrust与signature；不改用户全局opencodeslot |
| COVincoming OG24 | frozen2snapshot6685b82a938919a8e51f46b7c51cc305b40189a2已满足历史资源，不指liveOG24lifecycle | COV当前13/26source、0formal；原25非发布+22case门全满足才source-ready；26分母不变 |
| DEP13/sharedlease | source-ready和complete原条件不减；另具名source-ready.sharedpaths记录REL三planI/R源冻结F、path/SHA/mode/owner/route/UTC | A: frozen2→COVsource→source-ready→RELfreezeF→grant→OG24.3landing→signedreturn→RELcloseout→complete；B:SRC未好则freezeF→RELcloseout/complete→grant→OG24landing→return，route互斥不混装 |
| sharedfacts / OG27 | R仅root对F发布；COV/REL三plan事实grant期间停写，非保护region仅root按逐signed非OG24账本串行接入、signedreturn后仅root在返回postimage写actualC/D；OG24不改COVR | 路径为三plans、tracing/agent、EN/zhagent、COMPAT、既有docscontract，cf单页独立owner；F不会因较新main变成含3；OG27仍须complete+RUN真signature |

本G09无新Task/分母/waiver/稳定发布操作；旧049四卡complete、原020FAIL/deferred/defaultreject、nativeSourceA/三字段UNPROVEN、old2有限285/45455proof与9143mechanism/skip/历史FAIL不改。no embeddedWebUI保留TUI，buildInputsSHA/空archive921b/完整argv具名，template26label有意沿用/all3profile与settings绑定；真实全局切换待OG27+DEP13complete另显式optin。原其他计划与用户dirty04/05完全不写。

**版本化边一致性：** OG25/26原OG24 source/interface及其Granularity/父审计主图均绑定6685 `.2`不可变snapshot；当前OG24.3不指向OG25/26/COVsource，只有current3→RUN029与DEP13complete共同先于OG27。已签OG25/26旧AC/VER不改，不产生重审源循环。

**A阶段abort-return（唯一post-grant A→B条件）：** root先核没有任何OG24.3实现/站点landing提交已signed或pushed（G09规范采纳不是实现landing）。每个shared path必须等于grant SHA/mode，或严格等于下述逐项准许的非OG24 signed delta重放结果；不能仅以“无关改动”标签放行。未提交OG24草案只留私有目录，不夹入COV事实/R。root出具具名abort-return receipt，绑定grant/F/route、root/cf签推核查、逐path grant/expected/current blob-OID/SHA/mode、全部准许commit及独立ledger SHA、保护region校验、UTC和唯一返还owner后，才返还共享写权并显式A→B。任何3实现/站点landing一旦signed或pushed，即使字节复原也必须原forward-only源交接/修复和成功return，不得abort换B；F、真实gate和原route选择条件不变。

**非OG24 signed-delta重基账本（abort equality的唯一扩展）：** grant时对每个shared path记录完整blob-OID/SHA/mode、唯一边界anchor及该边界内原始bytes/SHA。保护区逐项覆盖OG24卡片与其I/criteria、当前G09/DEP12/13/stage/DAG/统计归属、COVREL与其F/版本source/排除条件、所有lease/grant/return字段；无法用唯一anchor完整界定的path默认整文件受保护。生产源码/fixture/manifest的OG24归属全文件受保护；cf同样由唯一owner登记页面保护段。grant后其它计划仍可推进独立路径和已登记的非保护region，不能并发写共享文件：其signed变更只由root按唯一写者顺序接入，记录实际commit/parent-chain、每path前后blob-OID/完整SHA/mode、精确diff和owner/卡归属，不全局冻结无关计划。abort前root在独立只读ledger逐项核签名/DCO及实际对象链，确认准许delta未触任何保护区/anchor、OG24或COVREL归属、lease、版本source与F条件；模式变化、跨区hunk、未知owner/路径或无法还原的rebase一律拒绝。再从grant完整preimage按实际有序commit delta机械重放（重基也列原/新parent与blob链、精确delta），逐path证明生成的完整expected SHA/mode等于当前actual SHA/mode，同时原保护region bytes/anchors/SHA仍相同。没有delta则expected就是grant；任何差异未被枚举或重放不等即不出abort receipt。此ledger不移动F、不改变source-ready/原gate、不授权OG24已签实现回退。

**R6准备期状态快照（2026-10-15；下段命令/判据继续有效，原UNRUN状态由页首实际QA事实覆写）：**

**本次G09规范采纳提交的读者门（mandatory ER04，不新增Task/AC/VER）：** 三plan本身也是编译/测试输入；在本次exact拟采纳字节上实际运行 `source .env.test && cargo test --test opencode_hook_contract_test`（整target，含原VER8读者）、`source .env.test && cargo test --test compat_agent_docs_contract`、`source .env.test && cargo test --test compat_agent_architecture_guard`、`cargo +nightly fmt --all --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`libra diff --check`，并用rg核实际其它plan读者再跑相关target。全部非零实际通过、soleClaude本exact规范PASS后root才能signed+DCO采纳；此时均UNRUN，不以probe字节相同代替运行。原OG24.3源码landing另跑原8VER/full等门不减。

**F独立legacy兼容lane（原门保留，非current profile替代）：** GC-COV2-08及COVREL VER2对immutableF的`.2`source/admission兼容核验和installed-F原`.2` issuer在fresh owned registry中按epoch0登记重验仍按原文执行，属于COVREL发布/安装验收owner；只在新owned HOME/XDG/registry与固定旧`.2`产物运行，freshregistry才0，不改用户globalOpenCode/exporterslot。它读取F/历史6685`.2`许可，不依赖current3landing/RUN029/OG27或真实native字段归档，故不向源链回加边。该lane通过也只证F legacy兼容，不是official29/`.3`验收或SourceA；currentSRC/RUN3及OG05三字段门仍独立。旧0.30.48拒绝事实保留。

**RouteA分支边明确登记：** `SHARED.grant_A→OG24.3.unsigned_attempt→RETURN.abort_A→REL.closeout→DEP13.complete→SHARED.grant_B→OG24.3.landing→RETURN.signed_B` 仅在abort严格谓词全部满足后成立；否则只可原成功A边`grant_A→OG24.3.landing→RETURN.signed_A→REL.closeout`。abort不是signed实现回退，不把两分支同时当活动边；所有C/D事实仍由root在实际return后写。


### 2026-10-15 — COV2-22 指定旧 writer、最终源码与资格裁决事实

**指定来源与有限证明。** GC-COV2-03 authority 保持 `dcc468bd31c88a7e8d8c0d2957d6e80b65908990`（crate 0.30.48）。独立完整旧源码 R3 实际执行生产 `seal→persist(&DatabaseTransaction)→SQLite commit→load`，exact **1 passed/0 failed/0 ignored**、exit0；raw SHA `ee368e027b13a0d01eece6be855900c7ff1eef4b9b5207a51d64d8e5f5102774`，run receipt `eae79978e425dc81badfc26267564164786fc77b870a3ed13fecb5c1325ec69a`。2040 原路径/类型/mode 已核，2039 其它文件与原像同一；仅追加 cfg(test) sampler，普通 caller 显式 `reasoning: None` 适配旧 API，原生产代码及固定输入不变。原 Cargo.lock 未改；与 7ef 整锁不同，849 个 nonself dependency records 同一。独立 owned HOME/XDG/target、offline/locked/Seatbelt，无模型、账号或 Session 调用；R1/R2 失败保留。

8 个真实已存储/载荷 raw 与首次 7ef 参考逐 byte 相同，6 个当前 VER6 literal 与实际 dcc raw 逐 byte 相同；第九份 provenance receipt 仅真实 crate/revision 不同。比较收据 `6587297f4190bbf5a8b67ce6a173fe0fc7718dac7e6b09b80b3b011bdb206b85`，root 独立审计收据 `31cd4e23b68633ce4f6efa348488067a9444c721d0e1423476f67b2303206315`。原 `HISTORICAL_SOURCE_7EFCF_*` 名称、常量/MAC、冻结 decoder 均不修改；7ef 是首次采样历史，不能替换指定 dcc authority。只证明 VER6 固定受控 fixture，不证明全部输入等价、历史发布 binary 或 native Source A。signed18b 初次计14及随后发现缺口回13的 HOLD 均保留，不追认早前已证明。

**干净历史输入。** signed `b644c59a43f9f40a02f48e4e38f9f93f3256bbe0` 全 2044 原 blob/type/mode 的独立 run 实际 exit0，**10139/10139 passed、23 skipped、2 distinct slow（3 SLOW events）、0 FAIL/RETRY/LEAK/FLAKY**；raw `d8ec8ac6efedee227255cfd5677d9b965eb286d76927ab8141e2df55d87452c3`，sealed receipt `e68884e030a27d728e4fabc056bcc3e262b7708f84f36188ee5613215e6309e6`，source BEFORE=AFTER SHA `4901735dc5047b6966ee23b3e255049cd8f1882285538cea059e282de30edcd0`。signed04/05 原 reader 实际 PASS；168 实际运行产物独立 CoW/0400 封存，manifest `6382c6a8b2f83b653a36002a816b4757435d3a7b9d2480409c5c67e64d852379`。原 dirty R5 的两条 LEAK、R4 环境失败与所有诊断保留，不能重标 clean 或以未知 reader closure 转移 PASS。

**当前交付输入。** `052e992b2b0d20d735348257ad6a062fb7c4e9ac` 的18文档/契约路径加 README 双语提交 `d188be90c1669ce7a36a185c0668584d96b841de` 已实际 signed+DCO Push main、remote 同 SHA。实际 d188 archive 全 **2048** 路径的 body/type/mode 与新 full 输入逐项相同，manifest SHA `3ec0ed184246988884bf0ff1d9256d602d8ab2bcfaac13630181d64014328d28`。两生产 owner 与 clean-b644 相同：pending.rs `f8f1160cd1832de2f76ccedde9e67fecd36c8e47628619e635ee8c8d7c0abde9`、pending_payload.rs `cadba46d0960cde91650ea4129a03113333303683a4b891b1a82a24c774b667c`；coverage-v2.md 仍 `0520cdb977ace11c247f8eda2b7050d99c03ea1d6484a7f3ea45dcc48f5bc95b`。agent.md 当前 `8894df4e189ad9a479ce3e0f0e6cf13e2a8d27c3b749fbd84c098086caaad9b0`、COMPATIBILITY 当前 `288be0318a1c87cf78dd305f37114f044a4d65a8bde78bb0ff05703ac132b127` 有已审文档差异；agent_docs_contract 4条 literal 也有变动，不称整树/测试/合同与旧 b644 相同。当前 QA 为 docs **32/32 + examples1/1**（合计33，非单target33）、fmt0、strict all-targets/all-features Clippy `-D warnings`0，README 增量唯一 Claude PASS raw `ab13436d786bd0282c7d24a78458ac42aca0fc3ec81d91df116572f827e8fd5f`。

新完整 macOS full 于 **2026-10-15T04:06:56.885206+00:00→2026-10-15T04:44:40.708937+00:00** 实际 exit0，原命令 `cargo nextest run --all --no-fail-fast --retries 2`、jobs2/threads4、先 `.env.test` 后 `.env.live-test`（不输出值），清空 native/model/local-agent opt-in，child umask0022：**Summary [2262.447s] 10139 tests run: 10139 passed (2 slow), 23 skipped**。raw SHA `0f64294d77e9a25fb847ad3e5ca69ccaac65c302eafb880782e9acd4f59ea746`，FULL-RESULT SHA `71fdd3f25f728b062c796c595f7e553efd9d8be716f5ed1ecc7a4bdaf8e47fc4`；2048 BEFORE=AFTER，无 LEAK 警示。原始事件计数 `{"ABORT": 0, "EXECFAIL": 0, "FAIL": 0, "FLAKY": 0, "LEAK": 0, "LEAK-FAIL": 0, "RETRY": 0, "SLOW": 2, "TIMEOUT": 0}`；不新增零 LEAK 门禁，任何失败/重试警示均按原收据保留并由 root 裁决。此运行包含 signed04/05 原像，工作区两份用户 dirty 计划保留且未进入提交。

**不成功的运行仍保留。** 较早 052 run 在7514普通PASS+2LEAK后失去进程，没有 Summary/exit，原因未知，不能记全量通过。随后 d188 attemptA 因私有启动器误继承 umask0077，使三个可执行位测试0755得到0700：每项三次 FAIL，共9 failed attempts；5736 run/5733 passed/3 failed/23 skipped、exit100，余4403未运行，owned组被主动中断。未改业务源码、gold或断言，只修 child umask0022；三项 exact 随后各1/1/0ignored，才启动上面的完整新 run。原失败 raw `d2946792ddf95d6ceeb1c574b07e6072c77a203d3bc200907a4841c35e667ac4` 保留；旧 attemptA parser 漏掉 `TRY n FAIL` 的空列表不作零失败证明。

**本次两路径事实交付边界。** 本增量只写 COV 计划与 plan-status，原六 VER、继承 ER-04、8 AC 机制的实际原运行继续按原收据逐项消费；当前新增事实仍须自己的 reader QA、fmt/strict、最终唯一 Claude Code PASS、root 裁决与 signed main Push/remote 核对，未发生项不预填 SHA/PASS。本次不改任何生产/测试/配置/CLI、I/R、依赖或验收门，不接网站新命令，不新增 full 触发面；新完整 d188 运行证明上述实现输入，不把加入本两计划记账后的整树字节称为与 d188 相同。条件实际完成后仅恢复 COV source **14/26=53.85%**，formal **0/26**、父 formal **59/71=83.10%** / source **61/71=85.92%** 不变，下一卡 **COV2-08**。家族 C/D、DEP-OG-13、OG24.3、Source A、真实 native 三字段及历史 import020 FAIL/default reject/deferred 均不由本事实关闭。


### 2026-10-15 — COV2-08 当前签名基线开工收据

实际基线为 signed+DCO main `58b4c02e7ec372e5e9bfec13e108e0e68810c510`，parent `d188be90c1669ce7a36a185c0668584d96b841de`，二者实际 Push exit0、remote 同一性已核。COV2-22 真实 source 资格恢复收据的唯一 Claude raw `197a661f09e2de5aae74dcdfce07248c634e92fb716cf535ba91596c9a093835`（0 P0/P1/P2、5非阻断P3由root接受）、新增事实 QA result `6d27e6521965c16b5c79ae7ab1e1031ef0ca08a21c5bd629e7e7fc60298b1a22`、root裁决 `9385257cbbc2ef1ed8c3dbd182993cd2bfb7a5eae4681d2d30e2271724d0b257`；两已提交blob精确匹配被审后像。指定 dcc 的8原始产物/6常量固定fixture同一性及 clean-b644、d188 两次独立全量按其原收据继承，不改 gold、GC03或Source A。COV2-22 现为 source-qualified/locally-accepted；正式 C/D 仍由 COV2-REL-01 负责。此前标题/索引中的13与“未实施”是原日期历史，当前明确为14，不能倒改失败记录或把当前事实条目当新模型/native验收。

本卡按原8 AC/4 VER、I-core五文档与六生产owner预算实施，当前实际选择四个原owner `catalog.rs`、`checkpoint.rs`、`pending_payload.rs`、`recovery.rs`；三个行为文档保留完整当前前缀及原COV08追加段，另两I-core计划只做生命周期、依赖交接与实际验收事实。卡片未给出单独Issue链接，按用户已授权的内部任务卡执行；不创建远端Issue或PR。旧隔离候选的36具名focused、ARCH/docs/质量结果仅是旧输入历史，不冒充本次重基输入验收；四Rust后像虽与原受测候选相同，本次仍从实际58b全2048输入逐路径重基并重新测试。

当前 COV2-08 为 in-progress / Acceptance空，8 AC/4 VER 未通过当前源验证。后续须原36个独立selector各1/1、当前完整architecture58/docs32/examples1/hook12、nightly fmt、严格all-targets/all-features Clippy `-D warnings`，并因共享pending payload/checkpoint/recovery命中 ER-13 T-1，运行最终实现的双env完整Nextest；最后唯一Claude审查、root裁决与实际signed main Push/remote交接后才条件计COV source15/26。已通过的d188全量10139不能当成本卡含四Rust改动的全量。缓存只作加速，不是源码或运行证明；所有旧隔离源/封存产物保持不变。

生产OpenCode v2 claim入口仍按后继COV2-11 activation开放，本卡不增加公开CLI旗标、Session/exporter/model CI或可信native来源；同裸key跨版本native pending仍在OG-29正例与真实Source A前fail closed。修复后源完整性、所有实际输出与失败/skip/LEAK/重试将逐项保留，不改旧reader/gold/阈值或原AC/VER/I/R/DAG/Granularity。当前 COV source14/26、formal0/26，父formal59/71/source61/71均不增；本机基准为macOS，Linux功能仍由用户后验。


### 2026-10-15 — COV2-08 R1 审查与 R2 必要前滚

R1唯一Claude实际exit0但静态结论FAIL，raw `ca7fe6731a7f278d72563a5c27dc86a413cd3128aabc22f6eb8c7e07c8dcb0d5`，0P0/P1、2P2及10组非阻断P3。P2分别为仅Unix测试调用的setter在Windows lib-test上可能dead_code，以及durable-v2 catalog/store暂时错误误归ManualRequired并被doctor当永久失败处理。前者采用与唯一调用相同的`#[cfg(unix)]`；后者保持ScopeRejected/不匹配拒绝，将其它catalog错误带上下文交给既有doctor retryable classifier，store的StoreFailure保留RetryLater。Windows真实CI仍待新commit，不能把macOS静态cfg修订当Windows实测。

原VER3扩展同一真实sealed/persisted/committed artifact的catalog Database一次故障和真实marker删除SQL trigger故障：保留pending原bytes/MAC、receipt/claims/ref/identity等行，不给成功ACK；故障解除后同一artifact可结算。测试缝只存在cfg(test)，setter同样Unix限定，不接模型、Session/exporter或生产开关。另修正原函数doc/属性位置及v2错误上下文；其它非阻断P3保持观察，不削减验收或默认放行。是否通过仍以R2实际结果为准。

为上述修复，在任何源码修改前对owned full进程组发SIGINT。其真实终态为exit100，7049/10143 tests run、7049 passed（3leaky）、23skipped，0failed/retry；剩余3094未执行。raw `de4e7b15c76d554cbd4c7c55932ff1e204fcebcb9093f1456ad997460c4910d6`，2048输入前后同一、manifest `1fc510833fe49d2bc09369d292fb04f6c611480c4b65d59f008764fcd1dcfe86`。这是审查前滚而中断的run，不是完整PASS，也不是3项测试失败；3LEAK原样保留，不新增零LEAK门。R1的旧9后像/源码ZIP/运行前后/实际40步QA与FAIL审查均独立保留。

R2仍只写原9路径；catalog/checkpoint/recovery承接窄修复与VER3扩展，pending_payload及三个行为文档未改。因此“4Rust与旧受测后像相同”只再描述R1历史，不能转移R1结果给当前3Rust新字节。当前8AC/4VER仍未勾选、Lifecycle in-progress/Acceptance空；须R2全36focused、103reader、fmt/strict、完整新full与唯一Claude、root裁决及signed main交付才可条件source15。既有source14、formal0、父59/61及SourceA/native三字段/家族C-D/历史import020 deferred均不变。


### 2026-10-15 — COV2-08 R2 实际终态与 R3 前滚

R2全部40步相关验收实际exit0：36个具名selector各1/1、architecture58/docs32/examples1/hook12共103、fmt与严格all-targets/all-features Clippy通过，2048源输入前后同一，manifest `41f130b52dda94b7a640dc179ace416970fe482c3e8c344e9fc573600bcbe241`。唯一Claude实际exit0但静态FAIL（0P0/P1、1P2），raw `49244febb5f8760937ef000bf6a29c22ee2c9227b2d31a057b134913beb4bf9e`；Windows setter cfg问题已关闭，剩余P2为durable-v2底层原始envelope/late key-owner/stable identity认证将deadline或暂时DB错误抹为InvalidRequest，继而被当永久失败隔离。原R1与R2的FAIL及全部非阻断观察均保留，不以相关测试PASS替代审查。

在R3任何源码修改前已对owned R2 full进程组发SIGINT并取得真实exit100：7607/10143 tests run、7607 passed（1slow）、23skipped，0FAIL/RETRY/LEAK/FLAKY，余2536未执行；raw `3b9defc50ac185fddec629006534cb7234d89b55ed0503eee9435690bcc7b281`，2048输入前后同一41f。该run不是完整PASS。R2封存的9后像、原始输出、源码ZIP/manifest与终态均不改写。

R3仅在原checkpoint/recovery owner内保留v2认证错误的deadline/既有retryable分类，其余durable结算错误带可操作context交给doctor；v1分支不变。原VER3增加真实sealed/persisted/committed artifact在durable晚期认证暂停并到期的子例，要求RetryLater、原pending bytes/MAC及八表逐行同一，之后同artifact新deadline结算；不新增selector、target、全局env/cwd或Session/model CI。这是待运行的机制回归，不是native来源正例。

（历史，2026-10-15 R3准备期）当前R3新相关验收、唯一Claude、完整full、最终两计划事实复核和signed main Push仍待；先实际Claude静态PASS才启动full，代码/测试/行为七路径若再变化必须重新验证。原8AC/4VER未勾选、Lifecycle in-progress/Acceptance空及全部I/R/DAG/gold不变。COV source14/26、formal0/26，父formal59/71/source61/71不增加；后续COV2-05仅在08实际source-qualified交付后开工，家族C/D、Source A与历史import020延期边界不变。


### 2026-10-15 — COV2-08 最终实现验收与两计划事实交付条件

**实际实现输入。** 基线signed+DCO main `58b4c02e7ec372e5e9bfec13e108e0e68810c510`。R3全2048输入manifest `d76e75ebb43e5f3e2cb04137bc907cda3b9ba43ea1045a2a9a1ecdc2d8a7485e`，四Rust owner与三个行为文档的精确后像见下表；它们在相关QA、唯一Claude静态审查及完整full期间不变，本次两计划事实增量不改变这七路径。d188旧10139或R1/R2被中断的10143运行不能替代本R3完整运行。

| 实现/行为路径 | R3受测受审SHA-256 |
| --- | --- |
| `COMPATIBILITY.md` | `1c6b10d2f14e86328038b509d97ef60dc23c488ac1e4b81f227393ffe4b5f39c` |
| `docs/development/tracing/agent.md` | `91bd52dc0aabbb9b0b5f1d552d8c1d59c07f9fbaf9da27bcd84140a2c94672fe` |
| `docs/development/tracing/coverage-v2.md` | `61c7b6533877bc4bff961b615d33d97d221a2cce06ebb9ccfbae3fc379c729e7` |
| `src/internal/ai/capture/catalog.rs` | `079457d12e7a1c120a51a46ab8cd1c6424c39a3f2b6b1852509198314506b432` |
| `src/internal/ai/capture/checkpoint.rs` | `dc964d0fa7da47586fa432a9befb44a7d0ee8c869bf82a60cd1dc393f21b0d48` |
| `src/internal/ai/capture/pending_payload.rs` | `5b1b5669b26365bb00500c9942f217e902f6313d674d66e355a3b68bf0e9cd67` |
| `src/internal/ai/capture/recovery.rs` | `a94100341bcb7f7c726b43cb7a877489e9f235731580528121b6329640bed754` |

**A/B及质量门。** R3实际40步骤全部exit0：helper从当前源构建；36个独立exact selector各1passed/0failed/0ignored（原4VER、继承COV22/25的21及既有v1/catalog/store/recovery的11），完整architecture58/docs32/examples1/hook12共103，nightly fmt与all-targets/all-features Clippy `-D warnings`均exit0。原VER3新增的真实durable晚期deadline子例包含于实际1/1，整个selector约18秒；测试未启动/模拟OpenCode Session、exporter或模型。相关QA收据SHA `f99e7be2ade0159469604eee5e1390b7b4464ea3c31112604c86078ca975ea54`，源2048前后同一。

原8AC保持原谓词，当前本机映射为VER1→AC1精确有序(version,key)；VER2→AC2/3/4旧owner/fence/revision零ACK；VER3→AC5/6/7 late COV25原始成员重验失败、故障/中断/暂时catalog和SQL cleanup故障及durable晚期deadline均保留原bytes/MAC和原行，不给假ACK，故障解除同artifact可结算；VER4→AC8 MAC/local integrity不能构造独立native发行权，sidecar遗失不能降级ordinary/partial/versioned写入。全部为纯Rust固定fixture机制证明，不是真实native SourceA，native跨schema同裸key仍fail closed。

**实际完整full。** `2026-10-15T06:18:46.951794+00:00`至`2026-10-15T06:39:00.451990+00:00`，双env、无本机native/model opt-in，`cargo nextest run --all --no-fail-fast --retries 2`，8threads、child umask022，原serial groups/registry/profile/阈值未改。真实exit0，原始Summary如下，23skip不计PASS：

```text
     Summary [1159.064s] 10143 tests run: 10143 passed (2 slow, 2 leaky), 23 skipped
```

普通PASS行10141；LEAK事件2，SLOW事件2；FAIL/LEAK-FAIL/RETRY/FLAKY/TIMEOUT/ABORT/EXECFAIL实际计数为 `{"ABORT": 0, "EXECFAIL": 0, "FAIL": 0, "FLAKY": 0, "LEAK-FAIL": 0, "RETRY": 0, "TIMEOUT": 0}`。LEAK原样保留，不推断根因、不称普通PASS、不新增零LEAK门。raw SHA `062a5f2538eb3b8025d92bd5a12dfe3573cb36c803ca1a209febe08e32e9237e`，full结果SHA `5ffec8cee81761f4180bf9e4ea7b4d31a7015ae20d97c23922ec41d72a8b90dc`；2048源输入前后SHA/bytes/kind/mode全部同一。本次两计划事实新增后整树不是full同一字节；仅七实现/行为路径及其余非事实输入逐项不变，更新事实由后续reader/质量/单审单独覆盖。

```text
        LEAK [   0.210s] ( 1673/10143) libra internal::ai::capture::key::tests::capture_dedup_staging_lock_retries_only_bootstrap_enoent
        LEAK [   0.627s] ( 6043/10143) libra::command_test command::fetch_test::test_ssh_changed_host_key_never_prompts
        SLOW [> 60.000s] (───────────) libra::command_test command::merge_test::rename_path_collisions::binary_rename_path_collisions_keep_bytes_and_unmerged_stages
        SLOW [> 60.000s] (───────────) libra::command_test command::reset_test::test_reset_clears_single_pick_and_revert_state_matrix
```

编译器/链接器原始警示（不记成Clippy失败，也不隐去）：

```text
warning: linker stderr: ld: __eh_frame section too large (max 16MB) to encode dwarf unwind offsets in compact unwind table, performance of exception handling might be affected
warning: `libra` (lib test) generated 1 warning
warning: the following packages contain code that will be rejected by a future version of Rust: proc-macro-error2 v2.0.1
```

**唯一Claude及root。** R3九文件静态实际PASS，0P0/P1/P2，raw `6a171689c04d812797f6d1d6c861f54c5ed8aa72834c4d7d87284a8c50fe12bc`，报告SHA `1cf4e511a7607f9b76ea80a5c8a1d701fb69f02a4bb10ebd0a5f4d4b808bee39`，root静态裁决SHA `155464d68f517063571b7c8bf388c62475d9b28ecb05ae77602b73e19532e902`。R1两P2、R2剩余durable错误分类P2均关闭；历史FAIL与两次owned-SIGINT partial runs保留。非阻断P3继续保留：diagnostic stage/cause；非durable StoreFailure的Refused/RetryLater不对称但不隔离/丢数据；manual durable与v1一致但无专项测试；暂时DB与后两认证调用为共享映射间接覆盖；短测试窗口/钩子命名；损坏v1的有界v2 retry；identity/缺checkpoint的有界重试、三次HMAC成本、旧dead_code属性/长SQL/英文文档中文追加段。当前索引旧13段明确标为历史。上述不是新行为/新门，也不把旧审核称历次无P2。

**本次事实交付条件。** 当前Lifecycle为in-progress/locally-accepted（A/B及适用实现full已实际满足），正式C/D只由COV2-REL-01继承。两计划最终字节仍须新103reader、fmt/strict、唯一Claude事实审查（绑定七路径与上述受审受测后像同一）、root最终裁决、signed+DCO main提交与实际Push/remote同一；未实际完成项不预填PASS/未知commitSHA。仅完成该交付后COV source15/26=57.69%，此前14/26；formal0/26，父formal59/71=83.10%/source61/71=85.92%均不增加。下一卡COV2-05。无本卡bump/tag/release或新公开命令，三个机制文档对应本实现；无需本卡网站命令更新。必要Windows/security CI与四平台发布留在家族D门，macOS为本机功能基准，Linux由用户后验。所有原AC/VER文本、I/R、DAG、Granularity、gold与SourceA边界不变。


### 2026-10-15 — COV2-08 交付前的上游纯文档整合

本卡原九文件已由实际signed+DCO本地 `9417d976cd5ab78137869a2ef174fdf28abc59d7` 保存，parent58b；原七实现/行为后像、完整R3双env10143/10143（2slow/2leaky/23skip、exit0、无FAIL/retry/flaky）、两计划新103reader/fmt/strict均按上述实际收据保留。最终两事实唯一Claude已实际PASS（0P0/P1/P2、2非阻断P3），raw `8c11ef08650f7cc244aa42f6e382a4aa940f616057e36d4f6d309802da28f5e0`；该PASS覆盖其原两事实后像，不预授本次合并后新字节。两P3的条件分子措辞与R3准备段历史标签在本次事实增量明确化，不改任何判据。

推送前实际GitHub只读compare与Libra fetch确认，上游5522458比58b多两笔已提交文档历史。原生archive/ls-tree对全部2048文件逐blob OID、bytes、kind、mode核对，只有plan-20261015、plan-long、plan-status三路径变化；其余2045输入同一，Rust、测试、配置、lock、installer、版本面、公开命令文档均无变化。上游两独占文档完整保留其blob字节/模式；共享status按两边原文合并，保留LTR-00标题/索引行及全部COV事实。本次不改LTR任务判据/状态、不替上游授权声明作新裁决，也不据它授出OG/COV grant或SourceA。

相对原R3完整full输入，最终候选仅四份计划事实文档不同（COV计划、status及上游plan-20261015/plan-long）；其余2044文件及七个受审受测实现/行为路径逐项相同。因此继承完整full对相同实现的证明，新四文档内容由合并后的完整103reader（ARCH58/docs32/examples1/hook12）、nightly fmt、严格all-target/all-feature Clippy及唯一Claude整合审查覆盖；不把旧整树字节说成新整树full，也不因事实更新重写旧raw。未运行的新门不预填PASS/未知合并SHA。原8AC/4VER字面、I/R、DAG、gold、Granularity和COV05尚pending的边界不变，必要家族C/D仍另行真实验收。

两个原有用户计划改动仅以本任务具名stash与独立备份暂存，不进入本卡或合并提交；须合并签名提交后恢复并逐SHA/模式核对，再仅删除本任务stash，原七stash及未跟踪安装binary/lock保留。该保护不是交付或源码资格。最终实际Push/remote与上述门全部闭合后COV source才15/26=57.69%，此前14/26；formal0/26、父formal59/71/source61/71不变，下一卡COV2-05。无bump/tag/release、新公开CLI/网站行为或OpenCode Session/model CI。


### COV2-08 实际源码交付与 COV2-05 必要写集修订（2026-10-15 07:18:48 UTC）

**已发生的前驱交付。** COV08源码本地9417与上游5522由实际含PGP签名头+DCO的merge `c60b2817b0d7c2b889491f85a9777e0855356fe1` 保留双父关系，NativePush exit0、fresh远端同SHA；最终归档2048 blobs的OID/SHA/bytes/kind/mode与合并QA输入逐项同一，archive SHA `91fc2e1e4d804c12361074223c29d4a157ad1389d5509f08f8501d42b68af28f`。四Rust/三个行为文档保持原受测受审后像；R3双env完整full实际10143/10143 passed（普通PASS10141、2slow/2leaky、23skip、exit0，0FAIL/retry/flaky），raw `062a5f2538eb3b8025d92bd5a12dfe3573cb36c803ca1a209febe08e32e9237e`。原R1/R2 FAIL及中断run保持历史。合并仅四计划事实异于full，其余2044输入同一，最终103reader/fmt/strict实际0；唯一Claude整合实际PASS、0P0/P1/P2、4非阻断P3，raw `80eba1a36baadd723f2894e445b5f8fe4f70731cb8bab6c56f2ac591b0d102cc`，root裁决后实际签推。未独立作PGP数学验证，不把签名头当SourceA。用户两dirty计划已恢复原SHA/权限，仅本任务stash删除，原七stash及正式49安装保持。新main CI run38696864722/38696864608当时queued/in-progress，旧非终态main为0，不记CI通过。COV08现source-qualified/locally-accepted，formal仍由COVREL验收。此前本轮“待签推/14”表述为其原时点历史。

**当前源码依据与最小修订。** 原COV05只列agent_import一个生产owner，但真正的import reservation/binding在coverage_gate，typed prebound checkpoint写入口在capture/checkpoint；legacy bind硬编码coverage1，typed request不能接versioned companion，单改cursor无法让原VER2走真实双版本原子提交。当前仅增这两个精确I路径，三生产落点为import编排/cursor-digest、coverage reservation/binding、typed checkpoint port，3/3且M；不拆行为/测试/文档，不增卡、AC、VER或waiver。原测试与INDEX许可保留，INDEX仅target索引确有变化才写。与08及后继的共享I按原严格DAG串行；新增两源码路径未在已合入LTR计划的实现路径中出现，既有status/docs协调仍为当前root串行写入；保留上游审计记录，不据其引用授权获得新动作。

**显式direct入口的边界。** 可增加且只增加一处公开库入口，接受现有ImportRequest和与request.turns按index逐项同序的显式coverage版本sidecar；旧import_prepared及CLI/provider路径保留v1默认到COV11。普通四record的v2只允许由新direct入口的私有准入模式显式选择；coverage_gate须新增此受控例外，旧reservation/live/default import继续按select_coverage_schema自动选择，v1仍拒绝Reasoning。不同请求的同一裸key在v1/v2各自独立，即调用方明确选择时允许live-v1与direct-import-v2各写一次；这是既有COV2-02独立命名空间语义，不能自动把live内容迁为v2，也不适用于native别名。原integration VER2只用synthetic normalized turn+temp DB实际reserve/bind/commit/repeat，不启动或模拟OpenCode Session/exporter、模型或provider响应。不暴露或弱化ProviderVisibleText/native/SourceA构造器，不新增可序列化issuer/证明字段；unknown版本、sidecar长度或index错配、重复tuple、单请求内同一裸logical_turn_key跨版本重复，均在lease/写入前拒绝；sidecar必须随turns的key排序一起置换，禁止按排序后的index误配。公开入口接收有界Duration，由agent_import内部建立并沿途保留同一CaptureCommitDeadline，不增加公开deadline构造器。direct入口按(turns,versions)重算content_digest，不信任调用方填入值，all-v1仍用冻结legacy算法。不能从privacy identity_schema_version、裸key、metadata字段猜coverage版本。原native跨版本同裸key拒绝与独立issuer要求不变。

**实现归属与恢复。** 三owner只承接同一version-aware import轴：deadline/scope/reasoning-context的显式版本reservation，精确(version,key) claim binding，保留opaque store的typed versioned import request、原子claim/ref/catalog/identity伴随事务，cursor按完整请求tuple的提交状态推进及失败清理。禁止在agent_import构造generic traces store、复制私有marker/ref逻辑或用SQL fallback代替port；绑定后版本不可重猜。all-v1 digest与identity preimage逐字不变，含v2的source digest使用显式独立版本domain；privacy import identity1/2与coverage1/2独立，mutable privacy-v1仍拒绝。既有失败/过期/跨scope/native规则与资源上限不削弱。checkpoint端须明确新增仅typed import adapter可达的Prebound+versioned分支；同一事务内versioned bind与apply_bound_claims保留claim、marker、scope、deadline和ref CAS围栏，不能绕开现有InvalidRequest后直接写generic store。

**原验收与证据边界。** 原7AC/4VER命令和谓词逐字保持、仍unchecked，stale-marker和failed-claim须真实注入事务故障，不能图测试替代；integration依然实际本地DB幂等而非seed成功行。旧v1有限oracle来自已实际执行并核验的指定dcc旧helper：9个pure helpers/37产物，独立storage path control不作portable gold，三DB helper未采样；不以当前serializer重生旧expected，不称模型/native证明。实现同步原三机制文档；没有新CLI旗标/当前命令行为，网站命令文档N/A，若实际引入公开命令差异须先修I再同步EN/zh/backendcf。

**采纳顺序和计数。** 本次只改两计划，无Rust/测试/配置/公开命令或gold变化，规划delta不命中新full触发；须本次实际103reader、fmt、严格all-target/all-feature Clippy、唯一Claude规划PASS、root裁决及实际signed main Push/remote同一后采纳，未发生项不预填SHA/PASS。实施在采纳后才开始；因三owner的共享binding/port，COV05最终实现明确命中T-1，必须新的双env完整Nextest并通过逐卡相关门/单Claude/签推，不能借COV08旧full。规划修订本身不计source16，当前仍COVsource15/26、formal0/26、父59formal/61sourceof71；原DEP/DAG/REL家族边界、SourceA/native三字段与Linux后验均不变。


**COV2-05 有限旧 oracle 的脱敏实测收据（本次供复核，不授实现通过）。** authority `dcc468bd31c88a7e8d8c0d2957d6e80b65908990`（crate0.30.48），实际2026-10-15 01:00:59.085321至01:32:18.881409 UTC、exact sampler 1 passed/0 failed/0 ignored、exit0，raw SHA256 `021eaae37ae7bfd4fa734e904198496be0ad081e89dedd47c6d18805af5a8b78`。9个原pure helpers为source_digest、identity_id、import_identity_id_for_parts、import_lifecycle_event_id、import_provider_commitment、import_source_preimage、update_length_delimited_import_preimage、import_identity_schema_version、contiguous_next_ordinal。产物清单FROZEN-ARTIFACTS37.json的SHA256 `851300718e874ec747af7e176eeadf039be8c176079955bdaea9aa4af8a2b1ad`，准确为36 raw产物+1 sample-receipt=37文件；其中storage-path-bound-old-result.bin依赖采样仓库路径，只作path控制、不作portable gold，余35 raw可移植。样本收据SHA256 `4d3dacb716a5b1ae22b90c5e57cb63aee886bb9759020e2e2c6eb0a8db64ff2d`；固定输入SHA256 `9a13fb6c8af2c8d8fbc93b4e66d6cef7cfabccbdf238b8ee7658ae48ae86f217`。原始完整材料在本机`/private/tmp/libra-cov05-dcc-sampler-prep-r1-1zpwbu6a/`，root复核全部36 raw、旧源2040输入before/after相同及9函数体与当前同字节；仅有限函数/输入结论，非完整writer/事务/依赖等价。committed_source_ordinals、bind_attempt、finalize_noop_identity三个DB helper未采样；重建preimage不是截获内部buffer。R1遗漏正文收据属审查FAIL，保留原raw及FAIL；本次补收据不把当前COV05或SourceA记PASS。


**COV2-05 helper 边界修订草案（非实现资格）。** 第一次实际原VER1因测试未接入已有helper定位/接缝，在真实DB写入处FAIL（0/1、exit101），raw `bc81340c3f1fb095fbde64da6394692860646f97cfea8c12db1edd783c21efbe`。仅隔离测试候选加临时cfg(test)诊断，复现原错误为“checkpoint object-I/O helper is unavailable in this host”，不是业务claim成功。对VER1本身，这是单测遗漏现有cfg(test)测试接缝；必须新增第四owner的理由是VER2及外部embedding host不能使用该cfg(test)接缝。现有authorized_read只接受main登记的当前Libra程序；库测试/embedding host不会被误当成Libra。因此R2只证明codec/claim/typed port方案，未证明外部direct入口能到达真实有deadline writer；原R2规划PASS保留其有限范围，首次实现仍UNQUALIFIED，原四VER不得放宽或替换。

最小修订为同卡I精确新增`src/internal/ai/authorized_read.rs`，landing/prod=4/4（M上限4/12），仍单一版本化import轴、同ID/7AC/4VER/26卡，DEP/DAG/REL不变。公开API仍只新增一处：显式direct入口允许额外传入调用方明确选择并信任的绝对Libra binary路径（可选；None只可消费main原登记），不从PATH/env/current_exe猜测试宿主，也不自动登记全局binary。路径准入/平台支持/同一总deadline在lease前校验；无helper时提前可行动失败，不留下写入中的identity。路径选择是embedding调用方的显式执行授权，不能声称已证明binary来源或赋予SourceA。

authorized_read只提供pub(crate)的异步task-local scope，且override仅对既有固定checkpoint-object-I/O helper argument生效；source reader、discovery、preparation、index repair、producer/native验证及其它argument仍只走既有main登记。固定checkpoint-arg的显式覆盖优先于现有TEST_AUTHORIZED_READ_HELPER_PROGRAM；其余argument仍调用既有helper_program()。helper_program、全局OnceLock、其它并发任务不变；scope的成功、错误、取消均恢复。显式入口先拒绝Reasoning/native、完成redaction/版本/shape校验，再在该私有scope内调用既有bounded/process-group/helper协议；无新private argv、CLI旗标、RPC/Session/exporter或proof字段。默认CLI/live/import没有此override能力。四owner分别为import编排/cursor-digest、gate reservation/bind、typed checkpoint port、窄checkpoint-helper选择；不允许generic traces fallback或取消deadline。

实现时原VER2仍为现有agent_import_test target、synthetic normalized turns+真实temp DB；通过显式传入本次实际构建的CARGO_BIN_EXE_libra到生产入口执行真实writer，不能fake helper或seed成功。原VER1/3/4在同轮、同一最终源码树先执行`cargo build --locked --offline --bin libra`，记录成功退出、source manifest、该binary SHA256及测试前后同一性；cargo test --lib本身不会构建bin，不能使用未核对的旧target/debug/libra。这三个lib单测只在#[cfg(test)]内复用既有native_import_test_helper_program()定位Cargo target/debug/libra，传给新入口Some(path)（或既有测试接缝）；该测试定位与生产禁止current_exe推断宿主不矛盾，禁止把定位逻辑搬入生产。原VER1纯旧gold可跨平台；需要实际bounded对象helper的DB部分以及VER2/3/4按现有Unix containment能力限于Unix（本机macOS各exact1/1），非Unix入口在lease前fail closed，Windows必要编译/安全/四平台发布保留，不宣称Windows功能验收，Linux由用户后验。额外scope隔离/只限固定argument/取消与缺helper零写入测试写在该owner或既有I测试文件，不新增测试target/公共factory/guard豁免。History的VerifySnapshot只在cfg(not(test))走helper，由真实VER2覆盖；其响应信任来自调用方显式选中的binary，不能冒称独立校验证明，机制文档须说明写入OID有比对而VerifySnapshot回复仍属于该信任边界。相关tests/fmt/strict、代码单Claude和T1双env full仍必须在最终四owner源码上新执行；临时诊断须移除。

本次两计划修订先在实际已采纳3e824e2的2046未改生产/测试/配置输入上运行103reader/fmt/strict，并取得本次单Claude规划PASS、root裁决及signed main Push/remote才采纳第四owner；root保留前三owner未合格WIP，暂停源码修改，不将其作为本次规划QA输入，也不stage用户04/05或WIP。修订后才继续源码与原四项验收。source15/26/formal0、父59formal/61sourceof71不变，不能计16或SourceA/native/C-D。


**helper规划R3审查处置（2026-10-15 08:46:57 UTC）：** 实际R3 FAIL（0P0/0P1/1P2/6P3）保留；P2补明三个lib门的cfg(test)定位、前置同源build/binSHA收据与首败原因归属；六P3同步修复baseline锚点/状态、信任边界、覆盖优先级、VerifySnapshot生产路径及历史头部时态。此R4只修改同两计划，前三ownerWIP仍暂停、不进QA或stage；实际单Claude R4、root及signed Push/remote未发生前第四owner仍未采纳，不预填PASS或source16。


### COV2-05 最终本地验收与签推交付条件（2026-10-15 09:49:12 UTC）

**范围和输入。** 三owner由actualsigned main/remote3e824e2采纳，第四owner由d36aa2bac136f8d80b0d8c781d38a1d861a50d52采纳；实际代码写集为agent_import、coverage_gate、capture/checkpoint、authorized_read四生产文件/四落点M，只有一处新增public direct库入口，既有agent_import_test和三个机制文档及本两计划同交付，共10路径，无新target/DDL/CLI旗标/Session模型CI。最终实现与code评审及full绑定的2048输入manifest SHA256 `d5e6060ac8ade10929bfdfd1e02769241b23da0be6fc522652853fed72588e4e`，before/after同一。普通prepared入口显式版本、同序sidecar排序、租约前redaction/shape/time/scope/SQLite数据库与storage配对、限定helper选择、exact tuple binding与typed prebound port保持deadline/marker/owner/fence/revision/ref/catalog/identity原子性。旧默认CLI/live导入不激活v2，native来源不由该入口供给。

**逐项门。** 原VER1/2/3/4各实际1/1、exit0，分别冻结旧digest/identity/tuple cursor、真实tempDB混合与交替重放、真实stale-generation故障零完成cursor、真实claim SQL失败零durable imported artifact并retry成功；命令/判据未替换。VER3额外比较traces提交目标前后同一，而非误把引用行数当作提交推进；原zero completed cursor/checkpoint/committed claim保留，替换generation的marker仍保留。VER4同时核对象文件/索引、ref、catalog及cursor不变。新增helper scope三exact各1/1；相关authorized_read14/14、import41/41、gate55/55、checkpoint34/34；agent_import_test53 passed/0failed/1ignored，ignored为OpenCode Session手工门，不当PASS。reader58+32+1+12=103；nightly fmt与all-target/all-feature严格Clippy -D warnings各exit0。17步骤实际全0。

先cargo build --locked --offline --bin libra成功（其binary SHA6571872b9c63b470af989224b2e9b2d5b4cd81bf9322ac8c19ba1d18c635a36a），再cargo test --no-run预备真实测试产物成功。预备helper SHA `c3c42638825801dd33b5f8cf61e57476116723420561bc149d20109270e7c440`、195827096B，在每个实际test/quality步骤及full后逐次同一；不把两个构建阶段的不同bytes说成相同，也不称其binary attestation或SourceA。lib测试只在cfg(test)取同轮实际bin路径，integration经CARGO_BIN_EXE_libra走真实生产helper/VerifySnapshot，不fake exporter/Session/helper或seed成功。需要bounded对象helper的DB例按现有Unix containment在macOS实测，pure gold可跨平台，Windows必要编译/安全/四平台发布保留，Linux用户后验。

**单审及全量。** 唯一Claude代码评审actualexit0/PASS，0P0/P1/P2、8非阻断P3，raw SHA `961c3f28a65fc484e6dbb3e0087c35284b92f04ebe33c2ec69f36993fce6a0ce`；root裁决后才启动新T1，不复用COV08旧full。实际UTC2026-10-15 09:26:10.429291至09:46:24.055105，双env静默加载.env.test+.env.live-test，childumask022、jobs2、Nextest8threads；`Summary [1127.838s] 10150 tests run: 10150 passed (2 slow), 23 skipped`，exit0、普通PASS10150，FAIL/LEAK-FAIL/LEAK/retry/flaky/timeout/abort/execfail全0，raw SHA `881a529375329b4a714c8403ecd072776e99a7a0a74b398facc49bf179521202`。两slow为binary_rename_path_collisions_keep_bytes_and_unmerged_stages和test_reset_clears_single_pick_and_revert_state_matrix，最终均PASS；无新增零LEAK门、阈值/serial registry/profile变化。先前missing-admission编译101、unit缺helper0/1以及R2错误额外ref行数断言0/1和binary变更均保留FAIL，不并入本次绿；R3重新核实际head及每步helper SHA。

**非阻断维护建议与限制（计划外跟踪，未声称已实现）。** 8个P3原文及root处置随受审收据保存：cross-native reader错误的显式rollback/专项负例、ordinary入口拒Reasoning纵深回归、额外boundary/Float/depth负例、legacy稀疏ordinal noop语义、过期dead-code attr、宏内手工换行、canonical数据库路径说明、文档精简/INDEX建议。当前已有行为限制为：tool input须已normalized，Float拒绝；SQLite数据库文件与storage为字面匹配，调用方须传数据库对应的canonical绝对.libra路径；noop cursor按连续ordinal前缀推进，稀疏旧输入可低于旧len值，当前未用于控制流。公开路径参数仅调用方的执行授权，不构成可信binary/SourceA；同一固定helper参数的VerifyReasoningArtifacts在ordinary scope不可达且不得引入。INDEX按test target维护，本卡未增/改名target，现有行已涵盖idempotent claims/checkpoints，未为单case追加行或改索引。旧dcc有限pure gold不证明三个旧DB helpers/writer/事务等价，helper snapshot回复依赖调用方选择，prepared redaction_report/snapshot仍沿既有调用方信任模型。真实native三字段/SourceA及家族C-D未闭合。

**最后交付门与计数。** full后仅本两事实计划变化，四Rust/一个测试/三个机制文档共八路径及其余2046输入仍与受审受测后像同一；须新103reader/fmt/strict、唯一Claude事实审查与root裁决，然后只stage本卡10路径、actualsigned+DCO main Push及fresh remote同一。未完成步骤/未知提交SHA不预填；本次签名提交本身承载证据，实际SHA由root交付收据和下一卡ER02绑定。只有交付闭合才COVsource16/26=61.54%，此前15/26；Lifecycle in-progress/locally-accepted，家族COV2-REL-01 actualC/D后才done/complete。父formal59/71/source61/71不变。两用户04/05 dirty改动及未跟踪正式49/lock均保留，不stage或覆盖；正式安装49不替换，demo不处理。本卡无用户可见命令变化，backendcf只读check实测107commands/108nav、drift0，未写站点；下卡09按站点实际clean cf/directpush和当前CLI同步。

### COV2-09 本地 A/B 与网站交付收据（2026-10-15，源码签推前最终事实）

**来源与写集。** 本卡基线 actual signed+DCO main/remote `b1443c9ba3e8a70b4ae7a6b84a3e2ab052715021`；前驱 COV2-05 原7AC/4VER、full10150/10150、代码Claude PASS及实际Push已闭合。保持原8AC/4VER，M=1 landing/1生产文件，仅graph私有版本解码；doctor生产代码未改。根11个精确I路径、cf一页；无新target，INDEX每target一行无需改；无新稳定错误码/CLI/schema/DDL/API，未运行OpenCode Session/exporter/model。
**四项指定实测。** .env.test静默加载成功；四 exact 均1/1、exit0：
- VER1：raw SHA256 `83d4ae134d6d118b3c94eee57271f6ed5eb4fc1d24f69e56a513fb4020a3dfe5`。
- VER2：raw SHA256 `02ff90b2739f4b2d68a693cb569f61622e2d0d64a17999365678456778bcf8a3`。
- VER3：raw SHA256 `d777effd9d1557bb835870970a3a6b560bd226cf07e87fc57451fd0bdd848b75`。
- VER4：raw SHA256 `18691d92462d0df1fa93a6ea057b2938c48da6ac1a6140af69c0a4b88e64027b`。

**附加实测。** graph14+doctor39=53passed/0failed/0ignored，graph只读架构单测1/1，reader58+32+1+12=103passed；fmt exit0，strict all-targets/all-features `-D warnings` exit0。2048源码before/after逐项SHA/blob-OID/size/mode/kind同一，manifest SHA256 `1144b46796570d844cd2e6450ad7b976b1f3196b4fe7d09c4149cd0004e5d9a2`。`Full-suite trigger=none`，依据见本卡字段及唯一Claude确认；本卡不声称运行新full，不以05的full替代；家族发布首次T2和最终全量保留。
**保留失败。** 首轮prepare编译E0277/exit101：私有required<T>缺i64标注，raw `e27cd40300489ac2aec493f2bb1831b9e7ead839bb8ef9e386eeaa4b2789f31b`；第二轮VER1/2/3各1/1，VER4 0/1/exit101，测试错误地从stdout解析错误JSON，raw `97df4b1385c69bb3593d68ffee645d12081da556f381dde9d56ecd2af0a72729`。现已按原CLI stderr错误契约修正，原AC不减；第三轮四exact及其余质量门全绿，旧失败不计PASS。
**唯一评审与父级裁决。** Claude Code代码审查实际exit0/PASS、0P0/P1/P2、6非阻断P3，raw `398e3ae0ad040a0ebe7eb8319a9736b001b0080fcb11d8f356511ef6343a3fff`；网站单页exit0/PASS、0阻断、2P3，raw `2b6f6bf32264b540aabd1b501a809db7d3b59889d98c621cda77748bc79db12d`。root实读报告后接受PASS及ER13 none。P3保留而非假称修复：doctor不专门诊断未知版本、两条doctor hint重复、支持版本提示缺精确断言、graph canary只覆盖未选隐私列、human finding存在性未显式断言、v2顶层字段/ordinal断言可扩充；网站文字对selected committed row/erased边界略宽及doctor期望同类建议亦保留。旧行为描述仅为静态推断，未声称执行baseline-negative。Linux/Windows实际功能、大catalog性能及未来生产v2数据未在本卡证明。
**网站同卡实际交付。** 开工原clean cf19d91bf；签推前fresh remote发现 `06a74c631cf007fa3d32fcafa4cd6f86ba4714e1`（四个非重叠安装示例/文案路径）。仅stash本卡一页，原空stash→确认自有ID335bebd908a7dd9b5df192bb9998c5dc8c1a529c→native ff→apply逐SHA/mode相同→drop自有stash并恢复空列表；保留所有上游。最终树重新sync-check107commands/108nav/drift0、fixture tests、docs unit9/9、typecheck/build全exit0；受审页面SHA/mode未变。实际signed+DCO cf `d5824ca86a33307df09286aec21f2158d873feae`，parent06a74c6，stage/commit/Push exit0、fresh remote同SHA；页SHA256 `15845768144be81301bf87b6c17a985f24a0508ca350c7ece7c5141cb694afc5` /0644。Cloudflare check `116372523665` completed success、Worker `ae715879-f044-4f0c-95a3-a3430d1d4e5c`；EN/zh-CN两页面实际HTTP200含新增conditional/unreleased段落；发布能力措辞未伪称当前49已包含v2。frontend全量未运行，既有6基线失败不冒称全绿；不把site静态评审冒称production验证，后者由实际check+HTTP单独闭合。
**受审受测的九个非事实路径后像。** 以下均0644；两个事实计划自身SHA由签名提交/后续ER02实际绑定，不预填自指hash：
- `src/command/agent/graph.rs` SHA256 `675ca2dce74c5c8ac22f752f71e28051e5f57dc583351bcd11cb593d3edc089e`。
- `tests/agent_graph_test.rs` SHA256 `a95146a6bfc977c2f2d8f3ba00c10b0d3894afd7881982dbcbb8672558ec8c98`。
- `tests/agent_doctor_repair_test.rs` SHA256 `809bcc6344ce8f15379e09c83b590a0cd8cb3260939d1980100ab0bf2a1f73e7`。
- `docs/development/tracing/coverage-v2.md` SHA256 `910e78e2ea42002a8d0f7d1d9640d1e389b092e18a3e61b2631fda876fd854f3`。
- `docs/development/tracing/agent.md` SHA256 `e7451aa17835975c1483ff9b6aef90b37a218810d9019705e5a090f21178bce8`。
- `COMPATIBILITY.md` SHA256 `65cf7e7af258554d0c675ff59ef996e334f8d318e3d96e23c7c1b94dd432cece`。
- `docs/development/commands/agent.md` SHA256 `091def29d1058386ed1ed0fa8b47129351802ad859000a043e4fed5d68f72787`。
- `docs/commands/agent.md` SHA256 `69968640d18ee478ad18f085f8663f81017f4caa5cb3c736d7a77a224107e7bf`。
- `docs/commands/zh-CN/agent.md` SHA256 `592f302a757d28eb36c134dd843b1dcc52c56f5901b73714384c7a39076b4c68`。

**最终门与计数。** 代码/测试/命令文档/机制文档九路径与上述受审受测后像冻结；最后只更新两事实计划，并重跑103reader/fmt/strict、root核全部2048差异仅两计划，再只stage本卡11I路径、signed+DCO commit、Push与fresh remote同一。未完成步骤不写通过、未知rootSHA不预填；实际交付收据与下一卡ER02绑定本提交。只有该交付闭合才COVsource17/26=65.38%，此前16/26；in-progress/locally-accepted仍继承COV2-REL-01 actualC/D，formal0/26。父formal59/71/source61/71不变。两用户04/05 dirty改动及untracked正式49/lock保留，官方安装49不替换；demo不处理。不bump/tag/release本卡；下一COV2-06。

### COV2-06 本地 A/B 与 T1 新全量收据（2026-10-15，源码签推前最终事实）

**前驱与原契约。** actual signed+DCO main/remote c5a138445aab2f81f4567b9789241adb25e63eff；09实际8AC/4VER、53affected+1readonly+103readers/fmt/strict/soleClaudePASS及cf d5824ca/CF/HTTP交接均已消费；BASE1e2177e与OG26原source734b6f0仍保留。原8AC/4VER不变，2个生产owner（允许3）、1个已有测试target、3机制文档及2事实计划，共8精确I路径。无新公共API/CLI/SQL/schema/稳定错误码/target/网站行为；INDEX按target不按用例，无需改。
**四项原指定实测。** 静默加载.env.test成功，各exact实际1/1、exit0：
- VER1 raw SHA256 `56dc10e26a8c619f193ca066438d381f6304ce5dd4fe77ec6fc2d04ebbb18a3d`。
- VER2 raw SHA256 `5325f38d7a1653f513cc08d0acd25a09be1fe68d4551f678806e559dfe976680`。
- VER3 raw SHA256 `d488a84733c9b23797b62ec915fa996cb7914181f3a87b28a79fc72293446be9`。
- VER4 raw SHA256 `e7c7681f6486704c5d277f6a11031f6c817c1c6a57f0a716be492f741da3d089`。

**相关门。** parser模块35passed；reasoning模块21passed/1ignored（既有release-only `verified_source_16_mib_budget`，不计PASS）；contract target7passed；reader58+32+1+12=103passed；fmt和strict all-targets/all-features `-D warnings`均exit0。2048源码before/after逐SHA/blobOID/size/mode/kind同一。新私有解析器在原始字段被擦除前分类；source/message/part坐标有界，state/proof候选不生成typed record；partial仅OR，不被有效兄弟清除。原生产normalizer仍incomplete/v1，完整ordinary control证明并非无关畸形包导致；live/import/snapshot无新parser调用，COV2-11负责激活及改写临时guard。
**保留两次失败。** r1：4VER各1/1，但架构target57passed/1failed，新增只读METHOD触及四个构造方法封闭守卫，raw `9c589dabf5e83fbf9798c27248d476b5d9d02595ee7675b620ed3995a57b7636`。现将查询改为同模块只读FREE函数；守卫文件/四构造方法不变，未豁免权限。r2：4VER、各模块/contract/103reader/fmt通过，但strict Clippy `type_complexity` exit101，raw `bfb8e9dcf89774970197c5570a1bdbbd0c0637d07f734bbcb9e6e89695f9fb14`；以私有类型别名修复，无lint suppression。r3上述11步骤均实际通过，旧失败不算PASS。
**唯一代码审查。** Claude Code actual exit0/PASS、0P0/P1/P2、5非阻断P3，raw `574b34929a42fadb464a2c942fcd9c3b8ef5bb2acd631cb7a85dd9ff94eb4918`，root实读接受。P3逐项保留：文档须区分Partial(error)与Parsed(partial=true)；可补有效visible与无效兄弟/未知root混合测试；现有ProviderVisibleText String不wipe（只拒绝序列化/隐藏Debug，不能误称擦除文本）；两个bounded parse同时留树的峰值内存可降；最终事实须补r2失败。最后一项已在本段如实闭合，其余留COV2-10评估，未假称已修。3机制文档/2Rust/1test六路径在受审后冻结。
**新T1双env全量。** 新共享分类/owning carrier按T1更严口径：只在上述focused/fmt/strict及soleClaudePASS/root裁决后，实际加载.env.test和.env.live-test，清除模型/本机Session opt-in，child umask022、jobs2、Nextest8threads、原profile/serialgroups/thresholds，运行 `cargo nextest run --all --no-fail-fast --retries 2`。实际：Summary [1161.926s] 10157 tests run: 10157 passed (2 slow), 23 skipped；exit 0，rawSHA256 `a3f4fbfdc2560f76a4757b948a7e0788ec96dfa70f1c93e2e066f19ab036e3e2`；source2048前后同一，manifest `46ac85492ce4054f6b9dc5b8df0ee530ffbc8912943aec2c20e3d5080ae79b94`。事件实际计数：FAIL=0, LEAK-FAIL=0, LEAK=0, RETRY=0, FLAKY=0, SLOW=2, TIMEOUT=0, ABORT=0, EXECFAIL=0。23skip以actual summary为准，不记为PASS；不新增zeroLEAK门，不用旧full或CI替代。prepared helper前后同一=True，afterSHA256 `b57e912498bedb97c42f2b2ef4d49163b9f7cd908ec8bdadeb93c3f015599202`；这是同源码测试产物记录，不是native/binary attestation。家族首次bumped T2与全部卡最终full仍保留。
**网站判定。** 本卡没有用户可见命令变化；仅readonly sync-check实际107commands/108nav/drift0、exit0，无cf写入/发布。不用09部署证明新命令能力。所有Session/exporter/model实测仍只在macOS本机manual/ignored入口，本卡纯buffer/default suite不发模型请求，不借synthetic机制门证明SourceA/native。
**受审受测六个非事实路径后像。** 均0644；两个事实计划的自指SHA不预填，由实际签名提交和下一卡ER02绑定：
- `src/internal/ai/observed_agents/opencode_reasoning.rs` SHA256 `2297739439874df08a9a9c8b874fe6f79164b5783d774c955d694ed6f4c0c9a5`。
- `src/internal/ai/observed_agents/reasoning.rs` SHA256 `23471297893a03ba5b2bcae8d82e60497a022d3c171ca212d6afa528ecc86064`。
- `tests/agent_reasoning_contract_test.rs` SHA256 `d7cb67fa95543543e7f9b46bfd55292c6c64b0a11ce4c0f498a2be8de0946ca1`。
- `docs/development/tracing/coverage-v2.md` SHA256 `e3075578d1c7144f666ac49f91cb004238c77ad6576f0e11f4598b0555943eb8`。
- `docs/development/tracing/agent.md` SHA256 `fc80c133efa51744720774a650157d6f8e5daf60ceadac60dc2dce09ceb92d21`。
- `COMPATIBILITY.md` SHA256 `29587b3c22f0aaf5f53c835e80c19456891b589c8eaad34e85c298dce1f2961e`。

**最终交付门。** full后只更新本两个事实计划；须新103reader/fmt/strict、root核2048差异仅2计划、六非事实路径与CODEPASS/full后像一致，再仅stage本卡8I路径，signed+DCO main commit/Push与fresh remote同一。未做步骤不预写PASS，root提交SHA未知不预填；实际收据/后继ER02绑定。仅交付闭合才COVsource18/26=69.23%，此前17；Lifecycle in-progress/locally-accepted，formal0/26继承COV2-REL-01 actualC/D。父formal59/71/source61/71不变；保留用户04/05dirty、untracked正式49/lock及官方安装，不处理demo；不bump/tag/release本卡。下张COV2-10。

COV2-10 pre-review upstream sync: main/remote b4851995e48488a8886c963081eda2b4b1837105 adds only plan-long.md. Native fetch→exact13 tracked-path stash→ff→verified restore/drop actual0, original7stash order/IDs and13 SHA/modes preserved; no untracked official49/lock or demo touched. Before sync original4exact/35parser/21reasoning+1ignored/30coverage/28snapshot/9contract/104readers/fmt/strict all actual0, but new final-tree QA/Claude/T1full still required. Source18/26formal0/26; no card credit yet.


### COV2-10 本机A/B闭合，最终源码交付待闭合（2026-10-15）


**COV2-10 本机验收闭合，源码交付最终门待执行（2026-10-15）：** 原8AC/4VER全部满足；本卡仅纯内存数据准备，实际capture/live/import仍未激活。新增一个公开library方法 `CaptureSnapshotService::preview_redacted_coverage_v2`，只返回RedactedBytes/安全JSON，envelope1、每turn显式coverage1/2。原source坐标确认→旧bounded raw redactor→原始decoded visible text脱敏→有visible时整棵decoded tree脱敏→boundedJSON重编码→typed normalization/redaction→versioned canonical/digest/projection。公开preview经专用消费函数销毁sealed/native/permit权限，只保留ordinarybytes+partialbool；现有授权构造器/原closedcaller守卫不扩权。source.partial只OR；普通无visible路径/Claude/Codex保留既有字节与v1结果，不运行Session/exporter/model。

**实际focused/static（合入upstream b485后的最终实现）：**

| 门 | exit | 实际计数（重复选择不累计为unique tests） | raw SHA256 |
|---|---:|---|---|
| prepare-test-binaries | 0 | static/build | `2cbd5ada87545dca907e4f2285158784b6c2b3518b2e8f69091d0e3353e378c8` |
| VER1 | 0 | 1passed/0failed/0ignored | `e07b066d5d222c9b5eb7ea0871352c1985587afb57d0f64233777f80874de80d` |
| VER2 | 0 | 1passed/0failed/0ignored | `452d7227d6bb7ae98fb0df2538436edf9e3cd73193afbffb0f92b4730cb627af` |
| VER3 | 0 | 1passed/0failed/0ignored | `37c503b3a6c7bcf9040fec99e305adf92effc2123068aa216ff68a1936f7ebc9` |
| VER4 | 0 | 1passed/0failed/0ignored | `bb1309d135d4be4bcb59556d751eb73a27b7b138f6b613da0ed3915bd6b8a234` |
| parser-module | 0 | 35passed/0failed/0ignored | `d3a062f0a7aba9f764de6bba31d93458242878c01aa6e9966003b4e4a1a3d60d` |
| reasoning-module | 0 | 21passed/0failed/1ignored | `1ed54c9e749de3256a95e767e76f1f7262542c96709298370ba0445066fb7349` |
| coverage-module | 0 | 30passed/0failed/0ignored | `367bb721865541318632abc4cb1f5796b4b3e3f4ae73a2c74ade7c3189fb01e7` |
| snapshot-module | 0 | 28passed/0failed/0ignored | `e7f9e23ace61c0473a1e771be9ff454fda431650748871bb4c52c528cf100d06` |
| contract-target | 0 | 9passed/0failed/0ignored | `614799f118e94a8ba33aa17dded8ab4cb0ee5dabc4d2ef8a9fd1c679e2e76562` |
| docs-readers | 0 | 59passed/0failed/0ignored; 32passed/0failed/0ignored; 1passed/0failed/0ignored; 12passed/0failed/0ignored | `84f6b62e549d242e8d58259fffa80ccb1f5f2d2827dd130c7b219d5a53628f3c` |
| fmt | 0 | static/build | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| strict | 0 | static/build | `47f956192f03642fedb0d5d393935e4547a323a28ac03b176f57f88c6a5d2da5` |

原4VER各实际1/1；parser35、reasoning21passed/1ignored、coverage30、snapshot28、contract9、readers59+32+1+12=104。ignored为既有release-only16MiB门，未计PASS。补native/classic交错text/reasoning/tool的顺序、真实idle要求、source大小/过期deadline，以及validvisible+invalid兄弟/unknownroot保partial回归。旧bounded-writer测试原断言不删，随私有owner迁入coverage模块，无新target/INDEX/serial基建。三机制/API文档同卡同步；网站只读107commands/108navigation、drift0/writes0，backend保持cf d582已部署；无命令行为差异，不造网站提交。

**失败与修复历史：** r1 libtest编译101，writer移动后旧测试访问私有旧owner（raw9e0f66ee01659dc4a08e3f77d7aeed3e14fb449e3bececcfc2d3484bcde68d1c）；迁原测试到新owner。r2四exact及模块通过，architecture58/59 FAIL（rawc33e2ff7f382a1dd89ded5d6ed5f73b53a1010b1763c368bbeb3f5dedbe9b692），preview误走closed授权adapter；改为专用one-use数据消费函数，旧授权allowlist不变，VER3新增exactcaller/无authority返回守卫。r3全部13门绿后发现上游b485仅改plan-long；nativefetch→exact13trackedstash→ff→restoreSHA/mode→dropown全部actual0、原7stash不变，user04/05/untracked正式49/lock保留。r4因upstream canonical状态“已完成”替代“已收口” reader58/59 FAIL（raw77db4d798cc4f278a9f51fa63d831146fa448497322489947e4066e61e7c16f8）；现有in-I守卫接受legacyclosure或状态列exact已完成，未闭合分支拒绝两者、含正反control；保留upstream plan-long原SHAf5d35f4ead33d5adfb8126a0ea43cdeb3aa89e70cc4c6d8f1f3b5572d8099507。r5最终13门实际绿；以上FAIL均保留，不冒称首轮全绿。

**唯一正式代码评审与遗留：** ClaudeCode actualexit0、literalVerdictPASS，raw `481ec64cdd5bfe4e05892711caeb713fedbc2844502d5296225b429ad0a380d9`；0P0/0P1、1P2/7P3，root实读接受，按用户“单ClaudePASS即可/外围与同难度问题延后”执行。`DEFER-COV10-REDACTED-COORD-01`（P2，未修复/不新增Task或依赖）：raw credential-URI regex可能吞并compactJSON结构、可读record在已partial结果中错位；评审确认成功受影响路径必partial，无candidateplaintext泄漏或falsecomplete。建议以后在decoded树写入前检查type/count/source位置并加credentialURI反例；COV11激活时若触及coreAC须重新裁决，不把遗留当已解决。七P3均未记修复：无visible路径的escapedsecret保持legacy、整树extra-matchpartial只适用于有visible路径；缺decoded tool/metadata extra-matchpartial专项回归；merged report offsets来自多坐标空间且scan计数是多pass总量，不是单一原文offset/RSS证明；preview malformed/unsupported仍映射既有SourceAuthorizationMismatch，不代表其授予权限；两私有datahelpers pubcrate可收窄；moved writer旧rustdoc附到enum；closure guard未处理bold“已完成”。ProviderVisibleText普通String不wipe，序列化拒绝/hiddenDebug不等于擦除；保留显式注释与文档。上述不放行SourceA/native三字段/opaqueissuer或家族C/D。

**新T1双env全量 actual terminal：** 2026-10-15T12:43:56.061965+00:00 → 2026-10-15T13:04:09.698027+00:00，exit0，`     Summary [1115.187s] 10163 tests run: 10163 passed (2 slow, 1 leaky), 23 skipped`。事件实际 {"FAIL": 0, "LEAK-FAIL": 0, "LEAK": 1, "RETRY": 0, "FLAKY": 0, "SLOW": 2, "TIMEOUT": 0, "ABORT": 0, "EXECFAIL": 0}，LEAK按Nextest真实语义记录，不新增zeroLEAK门。raw `b7c890ba8aa7d6e4fea1250777682e3f60f818b2f26ade230d0a1cbf8ea146f2`；2048源码before/after逐SHA/blobOID/size/mode/kind同一，QA manifestSHA `1eb7938f2557d1544b0b8e0a100b7c9dddeda6577bac4233479a448f14752d85`；debughelper full前后same=True，after `19113858361e83679ce6bb680181467301494ad91834f5a760bc73f0a7cb1064`，它只为本地对象helper测试，不是二进制attestation/SourceA。默认8threads、既有nextest groups/profile/retry2不改。无Linux功能/Windows当前SHA/four-platformreleasePASS声称；家族首次bumped T2与全卡最终全量仍保留。

**受审/full冻结的9非事实路径：**

| 路径 | after SHA256 | mode |
|---|---|---|
| `src/internal/ai/capture/snapshot.rs` | `11e3f3bdbefea68470abfee381d2b9c1b14c1600fed3ca24bdd8357d1569abbe` | 0o644 |
| `src/internal/ai/observed_agents/reasoning.rs` | `b7627bb251d7a4d13a4c240a927ebf21c0f72a2bde3a8dab6e849656225ec69b` | 0o644 |
| `src/internal/ai/observed_agents/coverage.rs` | `9dbc53ae9070e416a2cf64305ba084e9bd9877f41fb57d6998ae1cc0a84bf9ba` | 0o644 |
| `src/internal/ai/observed_agents/opencode_reasoning.rs` | `2765a7bb1c07d388bbb594c537f11fe97caa6c138f81e51ef9b086c4b7302c88` | 0o644 |
| `tests/agent_reasoning_contract_test.rs` | `1354cd241fe63f81f3dccceb33ed05b9fff09135ebabd49ab9cc41e10d70fd99` | 0o644 |
| `tests/compat/agent_architecture_guard.rs` | `8475a57a50bd8d8213f2c5e18785474f05bdbb707e522fb404b2a3827b70d23d` | 0o644 |
| `docs/development/tracing/coverage-v2.md` | `7bf86a46215b5d21373c25a788747eb8a5104b731fe2672332b59273f9cb6c2c` | 0o644 |
| `docs/development/tracing/agent.md` | `8118ebcb6909f5a6d17b0fe12132f7322adf1ffd305b5ce71b36ffba668d9ea0` | 0o644 |
| `COMPATIBILITY.md` | `29f5f489c01e77ed8024c27b4d074b3ee190163731ef72085e22746388cdce1d` | 0o644 |

自身两事实计划不作selfhash：finalQA manifest/actualcommitblob及后继ER02绑定。full后只改本两个事实计划；接下来须fresh104reader/fmt/strict、root核2048仅两事实差异及9非事实同一，再explicit11I stage、signed+DCOmaincommit/Push、actualarchive2048等于finalQA、freshremote。尚未执行的最终门不预写PASS、不预填本commitSHA。全部闭合才COVsource由18/26到19/26=73.08%；formal仍0/26、继承COV2-REL-01 C/D；父formal59/71=83.10%/source61/71=85.92%不变。Lifecycle in-progress/locally-accepted。下一COV2-11先完成必要I/G前瞻修订/单Claude规划PASS再实施；不在本次事实收口偷偷改后继规范。本卡不bump/tag/release或替换official49、不改demo。


### COV2-10 实际源码交付 / COV2-11 前瞻范围采纳（2026-10-15）

COV2-10 actual signed+DCO main/remote 31a219969d8506555012ee2d63bcad82270d8e37，11I commit/archive/push实际0，PGP签名头及DCO同提交存在（不声称独立crypto复验）；actualarchive2048逐SHA/blobOID/size/mode/kind=finalQA。archiveSHA7c7f21ac635cfe76c570659deaf1da25e8842908389920fb40563df0416ab683；finalmanifestbc396dc8a32966da1d0bb63fd6454fe2868839efddb50aa3d110c61a54fe315e，final104/fmtstrict实际0。full10163/10163passed2slow1leaky23skip、0failretry/flaky，rawb7c890ba8aa7d6e4fea1250777682e3f60f818b2f26ade230d0a1cbf8ea146f2；soleCODEPASSraw481ec64cdd5bfe4e05892711caeb713fedbc2844502d5296225b429ad0a380d9，1P2/7P3依用户延期。user04/05dirty及untrackedofficial49/lock、installed49不变。旧mainAction无待取消，31a最新CodeQuality/CodeQL已观察success，CheckBuild仍inprogress，不冒称全部CI绿。COVsource19/26=73.08%，formal0继承familyC/D；父formal59/71=83.10%/source61/71=85.92%不变。

COV2-11原8AC/6literalVER/26cards/DAG/R不变，R4当前契约见主计划；max10prod/4landings/rootI23+site1。R1FAIL88b864...删unreachableordinarypendingwire，推荐A现有exportpendingreceipt；R2FAILaa86c8...补权威coveredV2expectedset/rejectionhistory/一次重采；R3FAIL360475...补同动作V1/empty先只读结算/nextNEWaction才V2、wholeattemptstall文案。R4soleClaudePLANNINGactualPASSraw09d94eda943aa9644c051e78134b9a548e7cc73dea21e5ab8e8db8e821456840（0P0/P1/P2,4P3）root实读接受。P3按建议落实施说明：绝不V1reservationgate含freshkeysV2history反例、terminal/nonterminal已有settlement+freshworkDirty、两pointreads、empty动作也可升级后。只规范2plans和actual31a非事实anchors，scope104reader/fmtstrict/signedpush待实际执行；implementation6VER/CODEreview/T1fullUNRUN，无source20/native/SourceA/C-D信用。网站actualcf/d582freshremote clean，直接现有cf，不建branch/PR/worktree；当前isolated/dirty指令已同步，datedhistory保留。

实际实现/readonly prehash锚点（两事实计划由actualcommitblob/后继ER02绑定，不selfhash）：

| 路径 | actual31a SHA256 | mode |
|---|---|---|
| `src/internal/ai/capture/live_checkpoint.rs` | `5e5dd2b53ec492e38f41a309619098ae770c3368df02e1f3ff57a12fcbc7e98d` | 0o644 |
| `src/internal/ai/agent_import.rs` | `4ea2d9414592b0b0306e6a0d7c7ad15f6ee48d4550fbecb8bce646ad7269e1b4` | 0o644 |
| `src/internal/ai/capture/snapshot.rs` | `11e3f3bdbefea68470abfee381d2b9c1b14c1600fed3ca24bdd8357d1569abbe` | 0o644 |
| `src/internal/ai/capture/recovery.rs` | `a94100341bcb7f7c726b43cb7a877489e9f235731580528121b6329640bed754` | 0o644 |
| `src/internal/ai/observed_agents/mod.rs` | `e703995210ddc6932b40a269c139c15600d46072e7bffc56e61623f99e9153ec` | 0o644 |
| `src/internal/ai/coverage_gate.rs` | `421ee7b3d8bb361710f02c2d39864ba79ffbb28daf7bdca8ca2436dfc4198561` | 0o644 |
| `src/internal/ai/capture/checkpoint.rs` | `717af0971e89e54480b97ce69e70757e05ed6c7cbdb89995db802a174a63fbdf` | 0o644 |
| `src/internal/ai/observed_agents/coverage.rs` | `9dbc53ae9070e416a2cf64305ba084e9bd9877f41fb57d6998ae1cc0a84bf9ba` | 0o644 |
| `src/internal/ai/observed_agents/reasoning.rs` | `b7627bb251d7a4d13a4c240a927ebf21c0f72a2bde3a8dab6e849656225ec69b` | 0o644 |
| `src/internal/ai/observed_agents/opencode_reasoning.rs` | `2765a7bb1c07d388bbb594c537f11fe97caa6c138f81e51ef9b086c4b7302c88` | 0o644 |
| `docs/development/tracing/coverage-v2.md` | `7bf86a46215b5d21373c25a788747eb8a5104b731fe2672332b59273f9cb6c2c` | 0o644 |
| `docs/development/tracing/agent.md` | `8118ebcb6909f5a6d17b0fe12132f7322adf1ffd305b5ce71b36ffba668d9ea0` | 0o644 |
| `COMPATIBILITY.md` | `29f5f489c01e77ed8024c27b4d074b3ee190163731ef72085e22746388cdce1d` | 0o644 |
| `docs/development/commands/agent.md` | `091def29d1058386ed1ed0fa8b47129351802ad859000a043e4fed5d68f72787` | 0o644 |
| `docs/commands/agent.md` | `69968640d18ee478ad18f085f8663f81017f4caa5cb3c736d7a77a224107e7bf` | 0o644 |
| `docs/commands/zh-CN/agent.md` | `592f302a757d28eb36c134dd843b1dcc52c56f5901b73714384c7a39076b4c68` | 0o644 |
| `README.md` | `d0b30387b6485d01ac2da97bd2520b815d296e838795d7da78f9752adfa6e41f` | 0o644 |
| `README.zh-CN.md` | `785f8d9053c7782b9740ee8594d160a3e5e19981ad19f76d04dd0e87cbe44f45` | 0o644 |
| `tests/agent_reasoning_contract_test.rs` | `1354cd241fe63f81f3dccceb33ed05b9fff09135ebabd49ab9cc41e10d70fd99` | 0o644 |
| `tests/INDEX.md` | `4a1b8629a7a872386e9a4f1381dcfdbc9808201c39fdca1730513fcf26000fc8` | 0o644 |
| `tests/compat/agent_architecture_guard.rs` | `8475a57a50bd8d8213f2c5e18785474f05bdbb707e522fb404b2a3827b70d23d` | 0o644 |
| `src/internal/ai/observed_agents/builtin/stable_promoted.rs` | `71f5a8228cff647462392d38fc0979e8e9dc2b380dd6f7c8f9172f50c816e24a` | 0o644 |
| `src/internal/ai/observed_agents/live_capture.rs` | `11199950983ac41f537fd7e65e2f0023acf4224b6207724b3b0eb475d4cf3c85` | 0o644 |
| `src/internal/ai/capture/catalog.rs` | `079457d12e7a1c120a51a46ab8cd1c6424c39a3f2b6b1852509198314506b432` | 0o644 |
| `src/internal/ai/capture/pending.rs` | `f8f1160cd1832de2f76ccedde9e67fecd36c8e47628619e635ee8c8d7c0abde9` | 0o644 |
| `src/internal/ai/capture/pending_payload.rs` | `5b1b5669b26365bb00500c9942f217e902f6313d674d66e355a3b68bf0e9cd67` | 0o644 |
| `src/internal/ai/authorized_read.rs` | `62ae2b1b33f1b1ad8532e5a599a29995aaf20b81e9e1543298d2585b5009628d` | 0o644 |
| `src/internal/ai/observed_agents/adapter.rs` | `c7cd1261ffa207894073fa5a38b9ae41439e00e63292574e321efccb91f4fe4c` | 0o644 |
| `src/internal/ai/traces.rs` | `49b86e0e2f86ec275880439e9c5e93ac934ee54a8a9e4b30bedebecba2d97d5b` | 0o644 |


### COV2-11 R6 私有导入传输范围采纳 / 实施开始（2026-10-15）

实际main/remote `04acabfd9ee87472ac7b5d8909b0eb58c70b33fb` 已交付R4范围；COV2-11仍in-progress/空，source19/26=73.08%、formal0/26，父59formal/61sourceof71不增。实际helper需要跨子进程serde传回脱敏typed数据，而ProviderVisibleText通用serde继续拒绝；增加精确owner `src/command/agent/import.rs`，当前rootI24+site1/max11prod/4landings/一处公开行为激活，原8AC/6literalVER/DAG/R不变。R5唯一ClaudePLANNING实际FAIL/raw55c9382a1ba678df8da9d2c9f9764de8ad73807b697145f539109d7df38e5a45的P1活跃末轮和P2新会话预检已在R6修订；R6唯一ClaudePLANNING实际PASS/raw7823707998f40d8d2df95380742ee04a6913e6811b5ac79431c08d406d750ad7，root实读裁决采纳。helper与decoder双重保留active最后Incomplete；导入迁移前只查scope/deadline/history，缺session不误拒绝，首个reservation事务再次检验rejected+v2，最多2次prefix查，后续batch保持逐key防降级。私有脱敏DTO无native能力；OpenCode成功帧必含envelope，缺失/未知不降级；fullprojection nativeMAC先验再取DTO，native+typed拒绝，非OpenCode旧None字节保持。正文当前I/Estimated/Granularity/R4计数同步为24/11，精确R6契约见主计划。

本地两事实采纳后才开始新增owner实现；不另造中间scope发布。源码四文件与gate两文件已从隔离副本按before/after SHA/mode核对整合；其focused与parent初步compile0只是阶段证据。website仅agent镜像内容变动，sync107commands/108nav、文档unit/typecheck/build已实际PASS；首次wrong-cwd启动FAIL保留，不称wholefrontend全绿。最终6VER、related/fmtstrict、soleClaudeCODE、freshT1full、signedmain/remote/archive绑定及同卡cf单审提交部署均仍须实际闭合。installedofficial0.30.49与用户04/05改动、demo、原stash保持；Linux功能由用户后验，不加OpenCode Session/exporter/model CI。无SourceA/native/归档/C-D/release信用。

## COV2-11 上游重基与实际接线状态（2026-10-15）

当前实际 main 已原生 fast-forward 到 `33a5a8b8954a072a091362124e38b738f923d5b2`，包含 `315c7c6` OpenCode Linux export/native launcher 修复、`fee5cfd` 按仓库目录过滤 Session 和 `33a5a8b` sandbox test lock-order 修复。2048 个源文件已逐 SHA/mode 核对：18 个上游变更完整保留；本地23个dirty路径中5个文档/INDEX采用无冲突diff3，其余逐字恢复；用户04/05、原7stash及未跟踪正式49 binary/lock均保持。实际 stash apply exit128（重叠文档冲突）保留原始失败；使用预先备份和已验证三方结果恢复、整树核对后才删除本任务stash，不将该命令记为PASS。

R6三个import/command/codec owner已按冻结manifest整合，原6literal VER与8AC仍待父级最终验收，source仍19/26=73.08%、formal0/26，父formal59/71/source61/71不变。当前私有snapshot以原始授权字节长度记录durable redaction_bytes_scanned，详细RedactionReport保留所有bounded passes的合计；这修复既有catalog complete-source长度契约，不改catalog/DDL/public projection形状。NativeProven无typedfragment保持原capture-consumer readiness，不因运行时伴随数据误降级。

`DEFER-COV11-IMPORT-TERMINAL-01`：既有严格OpenCode ordinary preparer对info.status的接受范围与历史import的terminal facts不一致，暂不扩展终止契约。VER2使用真实可准入、无status的idle源码，历史import保留active和最后一turn Incomplete；不能把TurnIdle当Session ended。live与import的canonical bytes/digest不包含completeness，独立literal gold仍必须相同；该延期不免除原AC1–8/VER1–6。来源/native/归档不获放行。

最终验收须绑定当前33a5及全部接线：新6exact/相关模块、readers、fmt/strict、唯一Claude CODE PASS/root后新T1双env full；网站镜像必须基于三方合并后的命令文档重新同步、验证、单审、cf签推及部署核对。此前旧网站页PASS及COV10 full不能替代本次最终字节验收。macOS本机功能，Linux用户后验；无OpenCode Session/exporter/model CI，无本卡bump/release。未知结果/SHA不预填。



### COV2-11 CODE R1 实际 FAIL 与核心前滚（2026-10-16）

唯一 Claude Code R1 实际 exit0、Verdict FAIL：0P0/1P1/1P2/4P3，raw `c258f6b6636ece19a7b5c63da260fbbc0d94886f21f593e16814d6cd78d2d3f9`；root 未接受为 PASS，新 T1 full 尚未运行。P1 是原 scrubber 已授权 Ordinary/NativeProven consumer 的不确定 typed fragment 被错误升级为 source partial，连带拒绝既有 v1 导入、普通 turn 和 terminal noop。修复限定为保留既有 consumer、丢弃 typed 数据并标 classifier rejected；有 v2 history 时整个 attempt 仍拒绝，Candidate/Unproven/partial permit 不升级。decoded extra-match 的 capture fallback 只保留首次 bounded raw-redacted bytes/report；公开 preview 保留原 partial OR。P2 是已提交且 exact revision/current set 绑定的 incomplete v2 回执不能只读结算；删除额外 complete-only 条件，完整性仍必须匹配已提交状态，不升级 turn。两项均属原 AC/VER 核心前滚，不延期或改验收标准。

修复前父级真实回归各匹配1例、各 exit101/0passed/1failed：P1 实际 Complete→Partial（raw `48d313119f4d7876e2f8bc836a0d72cde99178ed9c221cb80db7845db2d20156`）；P2 实际 durable incomplete checkpoint 后 redelivery 报 ReplayPayloadMismatch（raw `9f4e7586bdded4c90726b2dcbb77ae71c489524c2570bbb92a7864f8bc29aac4`）。不是编译失败，也不是 PASS。P2 窄修复已按冻结 SHA 整合，另两隔离 owner 正在补来源与真实临时数据库导入回归；最终相关 QA、唯一 Claude CODE 重审、新双 env full、同卡 cf 部署及 signed main 交付均待实际完成。四个外围 P3（实际 stage continuation 专测、legacy v1 terminal 专测、runtime cap 下游预算确认、多 batch/tool DTO 专测）依用户策略延后，不免除核心回归。

最终 QA 输入排除用户04/05的未提交改动，使用实际33a5上游两文件，根工作区原字节保持；旧 QA5 含用户改动的结果不作为发布 archive/full 同一性证据。QA6 的29步相关门实际通过，但属于本次前滚之前的输入，不能转移给新修复。当前原8AC/6VER仍未勾选；COV source19/26=73.08%、formal0/26、父 formal59/71/source61/71不变。上游33a5对应 Check and Build run38755158034实际8项成功，不冒称未提交本卡的 CI 或 Linux 功能验收成功。


**COV2-11 R1 核心修复实际回归（2026-10-16，最终聚合门仍待运行）：** 三隔离owner均已冻结并按baseline/final SHA整合；新source5/5、import2/2（完整import45/45、原VER2 exact1/1）、checkpoint7个focused及旧None control实际通过。父级新真实P1 live回归、原VER4和bound incomplete replay回归分别exact1/1，raw依次 `bdd4aa298803e0f4d1303fbb4802fb39d8c4d71f314fc8131878e1a9ad806768`、`f763346107f94e2dd308f85a457137a72f8bb40323fa1985f33d4541549ac3be`、`9bb64271de85b4ac922b2fe921023b6f066c21c4af2f38627943145365c9b00b`。新增同动作terminal V1真实post-CAS中断→只读结算→重复AlreadyApplied，覆盖R1对应P3的具体缺口；不再将该项称未测试。另三P3仍延期。首次parent post-fix两轮新测试误把旧normalizer的incomplete turn当fresh covered/empty，实际失败已保留；修正新fixture预期并改用真实中断回执，未修改旧production/gold/guard。

两个foreign坐标import新矩阵保留七行：原33a5 frozen源码逐字比对证明，旧scrubber剥sessionID/messageID后report scan353与原source375不等，既有catalog equality拒绝；public v1 writer与private runtime均保持同样拒绝及零写，不将Ordinary disposition当作catalog已准入。该继承问题延期，未改catalog或放宽准入；其余五行必须实际写v1旧literal并幂等。七行带v2history且仅unrelated新keys全部被preflight拒绝，五个旧可准入行另受实际txn history recheck。当前6VER/29步相关聚合、soleClaude CODE R2/newfull、cf/main交付仍未完成，8AC/6VER保持未勾选、source19/26不增。


**COV2-11 CODE R2 实际 PASS与窄前滚（2026-10-16）：** 唯一Claude Code实际exit0、literalPASS、0P0/0P1、1P2/2新P3，raw `979d5446e39775cce37ce9eb15d4c544c6b231145a8c925542aa4a04b00cd5a5`；确认R1两个核心问题修复正确、新真实回归有效、QA8的29步及原6exact/105readers/fmtstrict有效，无上游18项交互回归。其P2重新触及COV10脱敏后结构/坐标完整性：某些credential-uri/env-password规则会吞JSON语法，导致原consumer全会话持续拒绝，或错ordinal归属的可能性。root采用推荐窄修复，仅readyconsumer时将FIRST bounded raw-redacted后的结构/原绑定坐标失配归为typed classifier rejection，保留FIRST bytes/report和既有v1行为；先整批核原session/message/part IDs/type再写typed树，不改泛用redactor/preview/native授权/caps/deadline，history整体拒绝保持。该项在原I24、原8AC/6VER内自主前滚，不增加评审者或要求用户再决策。

R2是实际PASS而非失败；本次新修复仍须自己的真实修前失败、修后回归、related/fmtstrict、唯一Claude CODE新字节PASS及新full，不能转移QA8/R2结果。R2两新P3（decode-only敏感项阻断原因文案、early/late scan metric继承差异）和三旧P3按用户策略延期；泛用rawredactor词边界问题亦未声称修复。此前R1 legacyV1terminal P3已由真实中断回执覆盖。当前full仍UNRUN、8AC/6VER未勾选、source19/26/formal0不增，无SourceA/native/发布信用。


**COV2-11 结构边界窄修复阶段实证（2026-10-16）：** source owner冻结2文件（仅coverage生产变化，snapshot生产前缀与R2逐字同一，reasoning未改），新3/3实际通过；真实172B fixture证明FIRST rawredactor后JSON仍可parse而reasoning ordinal0的id由a变b，仅是raw-pass错位，原第二坐标丢失，未观察/声称完整runtime falsecomplete。当前capture先整批核root session和每个原message/part id/type，再写任何typed text；FIRST parse/绑定失败复用原首次bytes/report并标content-free rejection，deadline/caps/prebinding错误仍Err。无新module helper，既有inner AST visit覆盖局部closure，无新issuer或写集。父级真实live新exact修前1/1失败（Partial，raw `26fee524897f628ac5d41a4231d5f69becff6e1161913abfb079cb25579bcc32`），修后exact1/1通过（raw `2f6b140a208c7e25f456ae7d18795d4119d29cbb363a9c87e38355b2914d009b`）：无history沿旧v1 poisoned-empty-turn canonical `[]`/incomplete，真实terminal post-CAS中断回执只读结算；已有v2history+仅unrelated新keys拒绝零新写。泛用redactor结构/词边界遗留并未修复或称完整内容保真。

隔离import的首次post命令实际0且真实descriptor helper已重建，但copy2保留旧mtime使in-process libtest可能缓存；此轮明确limited，不作为最终source验收。owner正在只touch其owned文件（bytes不变）强制fresh crate rebuild后重跑。父级QA准备对全部2048候选write_bytes刷新mtime；QA8与parent结构修前/后均有实际Compiling libra，libtest总数4423→4424→4427与新测试增量相符。最终聚合仍须含fresh import patch的原6/related29/fmtstrict、新字节唯一Claude R3及新full。source19/26、formal0、原8AC/6VER未勾选，网站CF/main签推与部署尚未发生。


**COV2-11 import结构回归fresh收口（2026-10-16）：** 隔离owner已强制fresh重编译并实际终态全部exit0：真实helper build、新exact1/1、原VER2 exact1/1、import46/46、fmt；实际libtest总数4427（首轮cached4424），编译日志明确Compiling libra，testbinary mtime晚于owned source touch且SHA已记录。最终test-only agent_import SHA `888246ac1c65915345dd0fbe65de2da2a7516ed497f5ea22c8533bfe78a86533`，production prefix与原七行矩阵/既有断言逐字同一。修前真实descriptor SourceAuthorization失败保留；首轮post仍limited，不转成最终PASS。父级已按baseline/final SHA整合；新的原6/related29、soleClaude R3和full仍待实际完成，未计source20。


### COV2-11 最终评审前刷新实际 d182 上游（2026-10-16）

新的实际main/remote基线为 `d1824ab61cfb36f91a2858044f8c41c37fd830df`，继33a5后3个真实提交：3b38188关闭bundled SQLite memory statistics、dd0f24e区分lock等待与open预算、d182修锁竞争fixtures。11个上游路径完整保留；checkpoint/coverage_gate仅各自锁holder测试调用有重叠，预先diff3无冲突。Native fetch、exact25 tracked stash、ff、全2048 SHA/mode恢复核对、仅drop本任务stash最终脚本exit0；stash apply实际128保留未称PASS，按预先备份/两个三方结果恢复后才drop。user04/05、原7stash、空stage和untracked/installed official49保持；无PR/worktree或demo修改。旧None函数与旧v2拒绝negative继续与实际d182基线逐字同一。

此前33a5输入QA9既有Mac cap用例34/35失败、exact失败，原断言/预算不改，后续同2048 unmodified module35/35实际通过；QA10只复用19项实际PASS和该模块真实复核，再新跑余9项，最终29门actual0且源码2048前后同一。所有失败与limited诊断保留，根因尚未获证明；不把诊断PASS当正式源码验收。其结果现在仅为旧33a5输入历史，不能代替d182最终验收。

当前d182涉及共享DB/config真实源码，最终相关门在原29项外增加6个既有目标/模块（DB、client-storage、history、subagent-content、workspace-lease、scoped-config），共35项；原8AC/6literalVER、I24+site1、max11prod/4landings及family C/D不变。新字节related/fmtstrict、唯一Claude CODE R3/newfull均待实际运行；full仍UNRUN，不复用此前COV10全量或旧QA。card source仍19/26=73.08%、formal0、父59formal/61sourceof71不增。macOS标准、Linux用户后验，未新增Session/exporter/model CI。


### COV2-11 本机 A/B 验收闭合，最终源码交付门待闭合（2026-10-16）

原8AC/6VER实际满足，currentd182上游变更完整保留；真正live/export和历史import的私有typed伴随路径已激活。源分类先于普通预处理/脱敏，只有支持的visible Reasoning turn选择v2，普通turn仍v1且literal bytes/digest不变；native三字段、Source A和归档未获证明。候选保持partial，不以synthetic fixture证明native。实际pending receipt的post-CAS中断、terminal/nonterminal、exact mixedset/currentrevision/completeness和重复交付均受验。

| 门 | exit | 实际计数（重复 selector 不累计为 unique） | raw SHA256 |
|---|---:|---|---|
| prepare-test-binaries | 0 | static/build | `14f4300b4b6b8e38256e82ffd1049659df14dc6a53bec8ad6db55e00b668b4e4` |
| docs-readers | 0 | 60passed/0failed/0ignored; 32passed/0failed/0ignored; 1passed/0failed/0ignored; 12passed/0failed/0ignored | `b3d8637602cbbb2ae7c2cc38b78d2216a6db4ee947af00bfcfa9e3fa1b30d60a` |
| upstream-db-module | 0 | 86passed/0failed/0ignored | `00e685890b69d095789920b6bfb57fb6a9574998dea15a7d857890ac505d5d14` |
| upstream-storage-module | 0 | 1passed/0failed/0ignored; 65passed/0failed/0ignored | `6c8a82272a2209539e9feb4398f94f3f51d198cbbd433093e531e7b4d1f537cc` |
| upstream-history-module | 0 | 85passed/0failed/3ignored | `a29c9b46ebf367b4f1d420836de9abc4607d97c9d0b68e376b97a84f71892e26` |
| upstream-subagent-content-module | 0 | 59passed/0failed/0ignored | `7fa4c356c683672af9809b1d9e48cbca12b572ab0bdf131b140b6db3374b99b5` |
| upstream-workspace-lease | 0 | 34passed/0failed/0ignored | `f0f29981f175cd863eeffa461e50746be5aa8c84a355f224d7a71ff2e553aff8` |
| upstream-config-future | 0 | 54passed/0failed/0ignored | `17c1b6d1676c82a35651853b75345ada908aba68a5882ba966fbe09dd6ea8d8a` |
| VER1 | 0 | 1passed/0failed/0ignored | `d0a647d09efc48d8c1a4a28b8022102b6052b07b75949e1bfaf106180828a091` |
| VER2 | 0 | 1passed/0failed/0ignored | `e3512c5135232a17bc536fe6feeec0a32aa792b5234252923484f67cfb02184c` |
| VER3 | 0 | 1passed/0failed/0ignored | `669ade064532558f76149ba0fa517b7b4af2db593376e1a21aa7484adf26e4f6` |
| VER4 | 0 | 1passed/0failed/0ignored | `7d2edf25fca05d4e63b341370c84479e4066fbc4f3836d75a16c45dc11536b45` |
| VER5 | 0 | 1passed/0failed/0ignored | `0bf65228b6530018bf9c18cfb72642fdc67258e459d70ce214d9a26b3c150902` |
| VER6 | 0 | 1passed/0failed/0ignored | `a1eca44d81f292742d16331bbdfacdf6152b51d37c82443938ad833f293e09ca` |
| parser-module | 0 | 38passed/0failed/0ignored | `222714f41bf84e17e644d3004ad2a0f0e4a891aef691a4e64a7e7c82b1e83e07` |
| reasoning-module | 0 | 22passed/0failed/1ignored | `3d3953dee6d1ad0a4197c51522f10030df94cad5faa043ad2da5557638192b06` |
| coverage-module | 0 | 31passed/0failed/0ignored | `cb1400755dc730b91f56b34801479c6a3654d42c1ed025d07b9b909e6819a95b` |
| snapshot-module | 0 | 37passed/0failed/0ignored | `962edc47a782828fac5532453b8ef6b102a897655d80c36e6b5ec802cb6dda8a` |
| gate-module | 0 | 59passed/0failed/0ignored | `ce2f9b5961e84102c87e074d96c4665c44dc48d2ca2629ac7e6f86a0322287c1` |
| checkpoint-module | 0 | 37passed/0failed/0ignored | `da7929afdbcd6464d390c9359a88becce79b75cdcd8c155950ffd99941a65d2f` |
| recovery-module | 0 | 8passed/0failed/0ignored | `4003fe7fa9511b1b6aa6f393c5b4023b1e142bff645ae6a4de2fc1a432189897` |
| live-module | 0 | 19passed/0failed/0ignored | `615d7df71d04f6f366d77e5d7c9bef62ab47bab0409f3cb38a14bf081e54d4e9` |
| import-module | 0 | 46passed/0failed/0ignored | `2da86c10223833769f37c1734e7f20fb42d01c51e3f331805d7591f9388e188e` |
| command-import-module | 0 | 37passed/0failed/0ignored | `323af6b4b22d5e2ce96e12ee479d3d97a7532d1956f230eebfe263736165b1f3` |
| upstream-trust-module | 0 | 10passed/0failed/0ignored | `85363797c8751ef64b58e5491c9613dd332436d7a1f27ea0245c38bbed0d7fa5` |
| upstream-export-module | 0 | 35passed/0failed/0ignored | `2e1e6d1b0881c482363df0170a768269e22ddf0b615b614811e1a47c5c59ca35` |
| upstream-settings-module | 0 | 33passed/0failed/0ignored | `ec5c887e6b8ec9b61bea40f56bfec1d9f58358476cbe700a602c06b8ef38c707` |
| upstream-sandbox-module | 0 | 43passed/0failed/0ignored | `bca5bd0e9cf8fd05ff7afdf2f27c1271dc70433f2521caaeb36bcf6ec3e4ac4f` |
| contract-target | 0 | 10passed/0failed/0ignored | `ce97ccef7f7cc7d1e6139541791639a8cc8cdab9b25563ede2b1db5beb3acab9` |
| upstream-checkpoint-command | 0 | 15passed/0failed/0ignored | `a99931fe71bdfc8394912e923c9d2736f2915e2c59938dfcf0c3de27de532001` |
| upstream-rpc-command | 0 | 11passed/0failed/0ignored | `9f06a045d05105981312bc80fdc6546068fecb7a98a156255d268a083a785aa4` |
| upstream-install-contract | 0 | 12passed/0failed/0ignored | `3fe2b8b0bd27b76eb944916b7915cd784b05e7743f20834921c9ce05953a3171` |
| upstream-serial-contract | 0 | 26passed/0failed/0ignored | `9cf31baa80871d5a03bb3dfb45d1472ab7c7df8a74b9d25c52864876349edd7d` |
| fmt | 0 | static/build | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| strict | 0 | static/build | `51dbf3da697f6fb123badbb31bd27e785926dec64c7c72932826271baeca0a76` |

唯一Claude CODE R1真实FAIL/rawc258f6b6636ece19a7b5c63da260fbbc0d94886f21f593e16814d6cd78d2d3f9及全部失败保留；P1不确定typedfragment修为已有consumer的content-free rejection，保留legacy普通bytes/authority/旧错误，无v2history走原v1，有任意v2history拒绝整个attempt；decode额外匹配仅capture复用首次bounded RedactedBytes/report，preview保守partial不变，不声称修复旧escaped-secret/coordinate debt。P2移除额外complete-only门，已提交incomplete v2仍必须绑定真实exactset/revision/currentrow/checkpoint，结算不升级completeness。新测试保留原scrubber的duplicateIDs/missingcontent Unproven及原legacy Incomplete行为，不为过关扩大准入；早期新fixture错误预期与真实产品回归分开记录。原None replay函数/旧拒绝v2测试未改。 R2虽实际PASS（raw979d5446e39775cce37ce9eb15d4c544c6b231145a8c925542aa4a04b00cd5a5），其P2仍作同I内窄前滚：readyconsumer在FIRST bounded rawredaction后整批核原session/message/part IDs/type，先于任何typed树写入；结构失败/失配仅拒typed并返回FIRST bytes/report，preview/native/caps/deadline不改。URI/env-password真实修前失败、修后source/live/import回归均保留；parseable ordinal错位fixture只证明raw-pass错位，不声称已观察完整runtime falsecomplete。最终唯一Claude CODE R3 actualexit0/literalPASS raw `6d14592b62932520adf3f8326ba50fa1384b6d18705bee1ce71d7d93b6fd890d`，root实读接受（0P0/0P1/0P2，新增1P3）。新增P3为raw规则可能改变最后一个visible reasoning之后的非reasoning消息结构，当前身份检查不覆盖这些消息；legacy v1读取同一raw-redacted bytes，未观察完整runtime falsecomplete，归既有generic raw-redactor结构债延后。R2两个P3（decoded-only秘密/换行导致history stall的文档说明、early/late scan metric差异）及R1三个P3（真实stage continuation独立行为测试、下游batch预算确认、multi-batch/tool DTO专例）仍保留，未称修复。`DEFER-COV11-IMPORT-TERMINAL-01`及COV10已partial coordinate遗留不放行核心或native门。

相关QA9的既有Mac输出cap单测曾module34/35失败，exact重跑也失败；原断言/预算/生产均未改。相同2048输入后续unmodified module35/35实际通过，QA10明确复用原19PASS+module复核并新跑余9门，不称29门全部新跑。所有FAIL、诊断only与实际复核日志保留，失败原因未证明，不称已修复。

新T1双env全量实际 2026-10-15T17:35:48.378948+00:00 → 2026-10-15T17:56:21.761483+00:00，exit0：`     Summary [1134.295s] 10212 tests run: 10212 passed (2 slow), 23 skipped`；事件 {"FAIL": 0, "LEAK-FAIL": 0, "LEAK": 0, "RETRY": 0, "FLAKY": 0, "SLOW": 2, "TIMEOUT": 0, "ABORT": 0, "EXECFAIL": 0}，LEAK/skip/retry按实际记录，不新增zeroLEAK门。raw `395e3b88d0bfb003f6f6d75a20ef6a0086920f105bc2f35bba1833e77344d117`；2048候选文件前后SHA/blobOID/size/mode/kind同一，manifest `b6201d842c57c9ef2f9a8d2b99d146608090c65ef56f11c2a40b8cd0e8e6c2a9`；helper full前后same=True、after `7ab654deaa3f81538c53aeb6135186a91523c24d36a902bb02dd76944a9500c8`，仅本地私有对象helper验证，不是binary/native attestation。8threads与原groups/profile/retry2不改；不加OpenCode Session/exporter/model CI。macOS功能标准，Linux用户后验；不把上游早前33a5 CI当本卡CI或Windows实测。

当前同卡网站页已单ClaudePASS/raw9c7349e75867a92eeafc8ca381ec0f4b2e1158ed82681b7de411341e3c890e64、sync/fixtures/docsunit/typecheck/build实际通过，仍须最终freshsync、signed cf Push及Cloudflare/HTTP核对；未发生的commit/deploy不预填。full后仅本两事实计划修改，21个owned非事实后像继续绑定CODE/full，最终105reader/fmtstrict/root裁决及main signed+DCO Push/remote/archive2048门待实际闭合。全部闭合后才COVsource由19/26到20/26=76.92%；formal0/26继承familyC/D，父formal59/71=83.10%/source61/71=85.92%不增。下一卡COV2-12原macOS Session入口；本卡不bump/release，不替换official49、不修改demo或user04/05/stashes。


### COV2-12 开工与旧 extraction 原样预复现（2026-10-16）

前驱 COV2-11 实际 signed+DCO main `95a40996c0eb7343e1fe2704979df83c58e0830d` 已签推且freshremote同一，2048实际commit文件与最终A/B后像SHA/blobOID/size/mode/kind同一；原6VER/35相关QA、soleClaude R3 PASS/raw `6d14592b62932520adf3f8326ba50fa1384b6d18705bee1ce71d7d93b6fd890d`、新T1双env全量10212PASS/2slow/23skip及最终105reader/fmtstrict均actual0。网站cf `3f7ca9c4eb862d75a9c1b66811927632bd03151b` Cloudflare成功且EN/ZHHTTP200。COV source为20/26=76.92%、formal0/26继承家族C/D；父formal59/71不增，新mainCI尚非PASS。

COV2-12原8AC/6VER和I12不变，无单独关联Issue。ER02刷新：BridgeRepo init仍38；无consumer政策live_checkpoint.rs1048/reasoning.rs524；旧用例live_pipeline_tests.rs5553，export-stage matrix5849。先在上述实际2048同源树构建helper（exit0，SHA `8892c39109ad4ec6bd5ea775da99950274d0578f4f05439d159d2bd2f9e49da1`），再原样exact运行旧VER6：实际1selected/0PASS/1FAIL/0ignored、exit101，raw `4b4c2721dff5595ea16fcb780b4b2ae45848609e4c301e74ae2522dff950c9ea`。实际无内容错误为reasoning preparation无ready capture consumer，要求保留pending后重试；2048源码未改。此轮失败不是本卡验收PASS，也不追认旧全量10212中被ignored的该Session用例已验。原历史5cafe322失败与本次失败均保留。

按既有AC6/8只前滚该旧fixture的断言，准确命名为 `opencode_extraction_no_consumer_keeps_pending_without_checkpoint`；VER6及INDEX/integration/local-opt-in guard同步名称，保留原输入和broken-input场景，不改生产consumer/issuer/native授权。最终还须实际证明zero completed checkpoint和pending receipt；当前错误文本不冒称已读取这两个真实DB状态。其余21旧body只读，共享helper只按原I修macOS Required Seatbelt路径，不以pre-exec拒绝替代内容/oversize验收。3新增Session例继续ignored/feature-gated且仅本机manual；不加CI、模型调用或新serial配置。尚未运行本卡最终6VER/fmtstrict/soleClaude，不计source21。


### COV2-12 本机 A/B 验收闭合，最终源码交付待闭合（2026-10-16）

前驱实际signed main95a4099，原8AC/6VER保持；四个测试文件与七份文档为全部11个changed路径，生产文件全同前驱，opencode_export SHA59245be55256401d5d0c3ba62c705788910431b151b17bae2cbd0666646617ca。Mac共享fixture使用现有Required Seatbelt、canonical隔离store、真实trust/provenance、server readiness/stdinlease和sandboxclient；env_clear同时隔离global/system config，缺前置明确失败。原9bridge bodies逐字节同一；旧extraction函数之外完整prefix/suffix同一，其余未触及旧用例文件保持前驱SHA；旧architecture457183bytes完整prefix保留，workflow既有变化仅准确名称替换。

| 门 | exit | 实际计数（重复selector不累计unique） | raw SHA256 |
|---|---:|---|---|
| prepare-default-binaries | 0 | static/build | `23fac48ee2e6a84f7643a5dd2e01de5b108fcd3b4d9c289a4a565a1d0854dba2` |
| VER1 | 0 | 1passed/0failed/0ignored | `a8170deb21de45ababe1f5b8bcedc6a4d687f18cccd53d6f84744ed49dde1b34` |
| VER5 | 0 | 1passed/0failed/0ignored | `658ef913ec8cde480e46b5cd58b5f17033e261eb379696bd9c50231fff6e3484` |
| prepare-manual-binary | 0 | static/build | `9d5bc9dcc82d7fbf63202598e7de67e1904c93c2fb9e18e88e723109a5c30a96` |
| VER2 | 0 | 1passed/0failed/0ignored | `694d642f4961e336a75fc9452762b6ac7c7cd83a980abc3c2f5d636efb8280b5` |
| VER3 | 0 | 1passed/0failed/0ignored | `f181e70552c5cfb8659b7e38f561b77d5dac8973e3c581a21643208c182b1c70` |
| VER4 | 0 | 1passed/0failed/0ignored | `e34e4b5b59054e80ef20858c662c95c7952c0d44be96c44f7a5872854aedaeee` |
| build-default-helper | 0 | static/build | `bd5e8c9c5cd47da8da3cf8ef378e38331000864a5118ac028a17d48151487739` |
| VER6 | 0 | 1passed/0failed/0ignored | `76b3dc940c9c38a838ddac18e80494f24d91e02cd84b949b90ec5ac49af176e8` |
| docs-readers | 0 | 62passed/0failed/0ignored; 32passed/0failed/0ignored; 1passed/0failed/0ignored; 12passed/0failed/0ignored | `76e51a4e67919f29510ee37b08666e27d4d3fe7186e41b78929e46da4f64d0b3` |
| workflow-module | 0 | 3passed/0failed/0ignored | `c02026ca423726b55bffa6d65d21411c1697a84366af97cfa4effd36a7ad5c1c` |
| serial-module | 0 | 26passed/0failed/0ignored | `feeacb9afd29589706277ad6cc9d9e4d72599233be9c59202d2418011d43b2df` |
| fmt | 0 | static/build | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| strict | 0 | static/build | `31ade9d6db20a51f0c154e818518469b82c4acb04b029072b359fde4059aa03c` |

全部14门实际exit0，原六精确VER各1selected/1PASS/0ignored；107reader、workflow3、serial26、fmt及alltarget/allfeature严格clippy实际通过，2048源码前后SHA/blobOID/size/mode/kind完全一致。ER13实际判none：只有单target局部fixture/测试守卫/文档，无生产、DDL、命令、配置、Cargo/CI/installer或被至少3target复用的测试helper变化。已知fixture失败逐一归因，未带未归因失败过门；本卡没有新增full，不把前驱10212PASS冒称当前树full。COV2-REL-01仍须最终bumped树T2双env全量与发布C/D。

VER2真实live路径提交v2，same event_id重投不新增client调用，fresh event_id重导同export确有client调用而checkpoint/claim/revision不增。VER3从独立live/import仓的真实checkpoint export读回持久化语义，严格重建canonical bytes并对独立literal/goldSHA7973604f43b515e1af6b1206e85c4fdc88447eed5a7822ca0ec3a77224b792a9，两个实际committed claim digest也对同一gold；依赖SHA抗碰撞，不声称读出私有runtime buffer或仅对两次preview。明确live Complete/import Incomplete，native idle/import active和import-terminal遗留不改。VER4是valididentity/invalidmessages的实际post-export source拒绝：client marker、固定publicLBR-INTERNAL-001/消息、SourceReadError、ownerless failed job与真实pending receipt均核验，0 completedcheckpoint/claim/revision/contentpublication；Session/job/pending metadata允许写，名字zero_write不是零DB操作。私有sealed error不经CLI暴露，该较早拒绝无checkpoint-write诊断，不增加公开API。

原旧VER6在95a4099原样1selected/1FAIL/raw4b4c2721dff5595ea16fcb780b4b2ae45848609e4c301e74ae2522dff950c9ea保留；修正名称及断言仅对原PARTIAL和broken两次输入。最终真实DB checkpoint0/claim0，pending1→2，complete仍为SessionStart baseline1、每次新增complete0，session active/stoppedNULL、错误与metadata无canary；actual sealedSourcePreparationRejected与既有FakeLease ObserveIdle→ReleaseFailed均核。该fakeport没有persistjob，真实jobrows0，不冒称dirtyjob。中间错误query引用不存在checkpoint_count列已仅删该冗余断言，实际checkpoint表计数保留；所有旧/中间失败日志保留。新增harness的unsupportedstatus、私有错误可见性、legacy无eventID/noledger与早期无checkpointdiagnostic四类错误预期也保留，均为fixture适配、不声称修复生产缺陷。

首轮soleClaude CODE实际PASS但指出1P2宽泛ignored CI守卫缺口，raw3aa7320c7c975744838ba5d1c871229eca127b59ad5f7f6d15af6a2d89d1b947。父在原I内仅扩展architectureguard，拒绝broad lib/targetless ignored及nextest run-ignored，并只允许已精确绑定的Linux unit dispatcher body，新增反例，原workflow无写入；重跑最终14门后进行第二轮soleClaude。最终CODE实际exit0/PASS，raw `0d26e2bf6278d47a025b6b123ae1cd44cbc24387e7e6bc4e61c3ac3e94a59498`，root实读接受，实际评审计数与非阻断遗留为 `{"P0": 0, "P1": 0, "P2": 1, "P3": 5}` / `["P2: staticCIscanner doesnotcover cargo+toolchain/globalflags/llvm-cov, glob --test/alltargets or extra whitespace. No currentworkflowuses these to selectmanualSession; futureCIedit mustreview these forms, notassume exhaustive guard. Deferredoutsidecurrentimplementation underuserstrategy.", "P3 new: compoundcommands/commentquotedhash and --list mixedrun notfullyparsed; currentworkflows clean, deferredwithscannerrobustness.", "P3: oldoversizebody may hitforkdenial before16MiBcap; COV16/17must independentlyprovecap beforeacceptance.", "P3: newMac cases relydocumentedexactthreads1; arbitrarywholebinaryparallel notproven, no newserialregistrychange.", "P3: twofuturecards explicitlyrequireoldselector synchronization; 0selectedneverPASS.", "P3: bothenvnormativeacceptancekept; bridgeenv_clear blockscredentialinheritance. FakeLeasejobcount0structural, notrealDBleaseproof."]`，未称修复。没有模型/账号/真实OpenCode调用，没有新OpenCode Session CI；纯parser/crypto/sandboxunit/staticguard仍可CI。这里不证明signature/redactedData/reasoningEncryptedContent、Source A或native归档，Linux由用户后验，其他21旧body全套manual审计仍归COV2-16/17。本卡没有用户命令行为变化，因此没有网站写集/新cf交付；前驱cf3f7ca9已部署的命令文档继续有效。

CODE/QA后仅本两事实计划修改，另9个owned非事实后像继续绑定已审已测SHA；最终107reader/fmtstrict/root事实裁决、signed+DCO main commit/archive2048/Push/freshremote仍待实际闭合。全部闭合后COVsource20/26→21/26=80.77%，formal0/26继承家族C/D；父formal59/71=83.10%、source61/71=85.92%不增。下一卡COV2-15候选live Session权限防火墙。本卡不bump/tag/release，不更换official49，不改demo/user04/05或原7stashes。


### COV2-15 开工与 COV2-12 实际源码交接（2026-10-16）

COV2-12 已按最终14门、原6VER各1/1、107reader/fmtstrict、soleClaude R2实际PASS与root裁决，signed+DCO main `4a7fbfb96f13103e786ee293aa488e3afd83dd63`、Push0/freshremote同SHA及archive2048逐SHA/OID/size/mode/kind同最终QA闭合；archiveSHA `1ad4c0b0568bf1fefdf23119ae4b1c40e633de6d84e7af39ab65468b0f026d75`，reviewraw `0d26e2bf6278d47a025b6b123ae1cd44cbc24387e7e6bc4e61c3ac3e94a59498`。本地A/B与source-qualified交付完成，formal家族C/D仍pending。COVsource现21/26=80.77%，父formal59/71=83.10%、source61/71=85.92%不变；本卡完成源码门后才可22/26，不预支。

本卡保留原8AC/2VER/I10与max2prod/2landing，BASE及12实际前驱满足，按最终4a7fbfb全2048源码刷新；没有独立linkedIssue，不创建Issue/外部消息。root在main单写，两隔离普通源码副本分别仅候选live测试与静态守卫；旧25manual body只读、SharedMacRequired/trust/sandbox/deadline不改。首先通过真实sandboxfakeexport/hook证明candidate classification partial，再独立核ordinary/typed/persisted/log/error/nativeissuer/opaqueartifact，不以pureparser或syntheticnative代替。预计只加局部测试/文档则ER13none，须按实际结果重判；家族REL最终bumped树T2双env全量不免。本卡无发布/version/installer/公开命令差异，无网站新增写集；若实际实现需要改路由/公开行为，先按授权修写集与验收，不偷偷扩展。Linux不验、模型/账号不调用，source21/26不增直到实际交付。


### COV2-15 本机 A/B 闭合，源码交付门待闭合（2026-10-16）

原8AC/2VER与I10保持，生产2文件及全部其他生产路径同前驱signedmain4a7fbfb；实际变更只有2测试文件+6文档/index。真实Required sandbox/trust/stdinlease/client、原25manual主体及共享fixture不改。测试data-onlypreview只能作为普通/typed语义的辅助检查，不当作live运行时来源权；实际live partial warning、clientmarker、Session/receipt/job、checkpoint读回及decodedobjects/manifest为本机核心链路证据。未验证proof/state内容不能生成native来源证明；不使用syntheticnative替换Source A。

| 门 | exit | 实际计数（重复selector不累计unique） | raw SHA256 |
|---|---:|---|---|
| prepare-default-binaries | 0 | static/build | `e7617159f8aa7ce2632c77a0f12260cdeb6d2774288405cb5b08f40703972b53` |
| VER2 | 0 | 1passed/0failed/0ignored | `66dc097226d9026b973d25a6e9ac91a5e18052d4dbe49478dc2bf7f417f3766a` |
| prepare-manual-binary | 0 | static/build | `dca218ba048b2fe8285f232e5bc05028ab825f03732ee721f2a8d5d4e8bf7703` |
| VER1 | 0 | 1passed/0failed/0ignored | `db4b5982ddfb60c552d9e97ca43b4d73c158b7c14769dc4216437d2f651bb20d` |
| REG-live | 0 | 1passed/0failed/0ignored | `0e95efb504aef1083f7aa9447406e45689f986d8e6a87ec54bd291a70a05fd21` |
| REG-import | 0 | 1passed/0failed/0ignored | `3f0d9038f7d8bb6cbc8ccafd375fe5f669a3129bec7aa8222b43a5bfe3d00bdb` |
| REG-failed | 0 | 1passed/0failed/0ignored | `ab073031a934dfadf5c09dae27821da529429ab875fe494c61ad485a8d72909b` |
| docs-readers | 0 | 63passed/0failed/0ignored; 32passed/0failed/0ignored; 1passed/0failed/0ignored; 12passed/0failed/0ignored | `c7836f35c85437393057d304b3cd6031d8720527a0476a21bb15047214c9ea6a` |
| workflow-module | 0 | 3passed/0failed/0ignored | `3f37710a9964283292c8eb02d5eeefcbe61680ae02f293f4765e4f2fcd239095` |
| serial-module | 0 | 26passed/0failed/0ignored | `5073454f1d7c68e4f9bd5cd79a3569c67666081b874c0b53c4b3bc453a0dc25d` |
| fmt | 0 | static/build | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| strict | 0 | static/build | `cf880ce34bb9d0818819e63505cfeb32297d76d5b9727cc15a451534e2801ec9` |

最终12门实际allPASS，同2048源码前后SHA/blobOID/size/mode/kind完全相同，两个原VER各实际1selected/1PASS/0ignored，前驱3个manual回归各1/1，108reader(63+32+1+12)、workflow3、serial26、fmt及alltarget/allfeature严格clippy通过。ER13最终none：局部新测试/静态守卫/文档，无生产/DDL/命令/config/Cargo/CI/installer或>=3target复用helper变化；本卡无新增full，前驱full不是当前树full证据；COV2-REL-01仍须最终bumped树T2双env全量/发布C-D。

受控前集成缺新用例的guard1FAIL以及其余中间失败若有均保留在本机证据，不能写成通过；最终实际门才计数。原始8AC的每一sink单独断言及其非空/权限界限，以已冻结test最终代码、父整合证明和soleClaude报告为准，不能从零checkpoint推断全链路已测。没有真实OpenCode/账号/模型调用，也无Linux/Windows功能验收。CI只跑机制unit/static，不跑Session捕获。继承COV12R2词法守卫未来命令写法P2/P3遗留，不声称解析任意shell或修复旧oversize/parallel遗留。

soleClaude CODE实际exit0/PASS raw `f75a35e338898b73dfde7bfa9553b8405c1d37fc9612ed5b693d1a300d0262cd`，root实读接受；实际findings `{"P0": 0, "P1": 0, "P2": 0, "P3": 5}`；非阻断遗留 `["P3: DBcanaryscancoversSessionmetadata/metadata_kv andallactualcheckpointobjects; othercolumns/configDB/WALrawbytesnotfullyscanned. No claimofwholeDB/WAL audit.", "P3: AC7durableauthorityonly; transientcrateprivateissuerunobserved, combinednoidentity/noartifact/no reasoningpath/0claims is accepted. No native/SourceAproof granted.", "P3: proofrefusalpublicenvelope/receipt/jobdoesnotindependentlypinprivatecause; sameactualpartialwarning+clientmarkersreduceambiguity. Do notexposeprivateerrorforatest.", "P3: tree/looseobjectchecks assume20byte/40hexSHA1fixture; SHA256wouldfailclosed, nottestedclaimed.", "P3: integrationtestplanoutsideI10doesnotlistnewmanualcase. Newexactselectordocumented3Ibehaviorpages/index; generaldiscoverabilitysyncdeferredoutsidecurrentcard."]`，未修复项不声称修复。没有公开命令行为差异、网站新增写集或cf推送；前驱网站仍有效。没有bump/tag/release，不更换official49，不改demo/user04/05或原7stashes。

CODE/QA后只改本两事实计划，另6owned非事实SHA继续绑定已审已测后像；最终108reader/fmtstrict及root事实裁决、signed+DCO main/archive2048/Push/freshremote仍须实际完成。全部闭合后才COVsource21/26→22/26=84.62%，formal0/26仍继承家族C-D；父formal59/71=83.10%、source61/71=85.92%不增。下一卡COV2-18候选import Session权限防火墙。


### COV2-18 开工与 COV2-15 实际源码交接（2026-10-16）

COV2-15原8AC/2VER、12项最终门(原VER各1/1、3个COV12manual回归各1/1、108reader/workflow3/serial26/fmtstrict)、soleClaude actualPASS P0/P1/P2=0 P3=5、root裁决与signed+DCO main `6227e931193d0ba8ce1bda4ef916fe4543bef656`/Push0/freshremote同SHA、archive2048逐SHA/OID/size/mode/kind同最终验收闭合；archiveSHA `72b28f6f6cf04933a566f93a264c8d238fefd015b7a86c54c508a63d8459adf1`，reviewraw `f75a35e338898b73dfde7bfa9553b8405c1d37fc9612ed5b693d1a300d0262cd`。五P3非阻断遗留保留，不扩大为已修复；生产全同前驱，本机A/B及source-qualified完成，formal家族C-D仍pending。COVsource现22/26=84.62%，父formal59/71=83.10%、source61/71=85.92%不增；本卡实际源码门完成后才23/26，不预支。

本卡原8AC/2VER/I10/max2prod2landing保持，BASE及15实际前驱满足，全部2048最终源码刷新。无独立linkedIssue，不创建Issue/外部消息。root在main单写，两普通源码副本分别仅候选import用例/静态守卫，旧26manual body与共享Required fixture保持；先真实import路径证明partial，再逐sink核ordinary/typed/persisted/log/error/native/opaque，不用live或preview替代历史import。若现有路由正确只补局部测试/文档，最终重判ER13；若实际bug需要生产/公开行为变更，先在已授权范围修精确I/粒度/验收，再前滚并重跑live15。本卡无独立bump/release，不改globalOpenCode/官方49/user04/05/原7stashes，不触发模型/账号/Linux验收或SessionCI。


### COV2-18 验证补强与可观测性边界（2026-10-15 19:24:12 UTC）

开工只读确认私有import helper丢弃stderr且候选在projection前拒绝；本卡不增加production输出。原8AC/I10/2VER/max2prod2landing保持，VER2显式覆盖AC1–2并补实际私有准备路径静态耦合，VER1补普通可见成功对照和partial=false/true差分。同输入preview+静态route+actual拒绝/零写入仅为有界推断，非私有字段直接观测；AC2是否满足仍等最终actual验收和soleClaude/root，当前不勾选、不预支source23。此补强不增加行为轴/写集/依赖/release或Linux/模型门，G01–11原判据保持通过；G03仍8AC2VER，G04仍M2landing/max2prod(计划0prod)，G05加入agent_import.rs1605/1645–1740及command/agent/import.rs2640–2690只读锚点，G09无拆合卡，G10两隔离测试owner仍不相交。用户常设推荐方案自治授权，本条不是要求用户新决定。


### COV2-18 本机 A/B 闭合，源码交付门待闭合（2026-10-16）

原8AC/2VER与I10保持，生产2文件及全部其他生产路径同前驱signedmain6227e93；实际变更只有2测试文件+6文档/index。真实Required sandbox/trust/stdinlease/client、原26manual主体及共享fixture不改。普通同envelope对照preview partial=false且actualimport成功提交gold；两个候选preview partial=true，actualtrustedexportclient后以既有LBR-AGENT-020SourceAuthorization在projection前拒绝，独立0Session/cursor/identity/checkpoint/claim/revision/exportjob/对象发表及authority/inflight检查。privatehelperenv_clear/stderr(null)保持，不暴露其partial字段或警告；原AC2的partial是preview差分、actual特定拒绝/零写入和VER2实际ASTsource/snapshot/reasoning/runtimepartial-before-projection控制流的有界推断，不是直接观测私有进程。soleClaude/root明确审其充分性；不以preview授予来源权，不用syntheticnative替换Source A。

| 门 | exit | 实际计数（重复selector不累计unique） | raw SHA256 |
|---|---:|---|---|
| prepare-default-binaries | 0 | static/build | `138fcb87c43d57c1ef84f632665aa81946ed4f8929b94f7cacc0e6e1ad129054` |
| VER2 | 0 | 1passed/0failed/0ignored | `1cedfafc0c09fe0b61743bfd968480296f97a2fcd271c0b0a94c4b1ec69f8a67` |
| prepare-manual-binary | 0 | static/build | `cf6ba7b65e37182c6ba93ffa1adc6335a62ab55f46049a528ed6b72fc7ae3319` |
| VER1 | 0 | 1passed/0failed/0ignored | `0bb1b53d93dd56a1a5e23d4d51947bc31082236209baa654cee02b0056a944c4` |
| REG-candidate-live | 0 | 1passed/0failed/0ignored | `99d32e242e2b18e17ccac3c32e9f82d597dd2065fbefc724345d02452802c79e` |
| REG-live | 0 | 1passed/0failed/0ignored | `6b4da69cda53dbe13851dd6f1c37a319a5bec0e8df84b7feaedee4f9167448e5` |
| REG-import | 0 | 1passed/0failed/0ignored | `803d943645d37dbfde22772c098ddd782143957ffc2f3aa6f87fa792ce38a4fb` |
| REG-failed | 0 | 1passed/0failed/0ignored | `6192c77043ef203f17341f204c72f0dfaaf79992d33171302eb9f5a9ab5a935d` |
| docs-readers | 0 | 64passed/0failed/0ignored; 32passed/0failed/0ignored; 1passed/0failed/0ignored; 12passed/0failed/0ignored | `0af0f6a4593f1dad0178a1ed6132bd9dc5ed76e4329042c2ed5e9a05d5448157` |
| workflow-module | 0 | 3passed/0failed/0ignored | `c02026ca423726b55bffa6d65d21411c1697a84366af97cfa4effd36a7ad5c1c` |
| serial-module | 0 | 26passed/0failed/0ignored | `966c4d0377995d089c24519003fd0a55e76cace133634c16618cedec14a7cae9` |
| fmt | 0 | static/build | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| strict | 0 | static/build | `bf754fccaa25f47515f6d9ceae83df9c81a14d5e93bf866cc80f3c4396395802` |

最终13门实际allPASS，同2048源码前后SHA/blobOID/size/mode/kind完全相同，两个原VER各实际1selected/1PASS/0ignored，前驱COV15与COV12共4个manual回归各1/1，109reader(64+32+1+12)、workflow3、serial26、fmt及alltarget/allfeature严格clippy通过。ER13最终none：局部新测试/静态守卫/文档，无生产/DDL/命令/config/Cargo/CI/installer或>=3target复用helper变化；本卡无新增full，前驱full不是当前树full证据；COV2-REL-01仍须最终bumped树T2双env全量/发布C-D。

受控前集成缺新用例的guard1FAIL以及其余中间失败若有均保留在本机证据，不能写成通过；最终实际门才计数。原始8AC的每一sink单独断言及其非空/权限界限，以已冻结test最终代码、父整合证明和soleClaude报告为准，不能从零checkpoint推断全链路已测。没有真实OpenCode/账号/模型调用，也无Linux/Windows功能验收。CI只跑机制unit/static，不跑Session捕获。继承COV12R2词法守卫未来命令写法P2/P3遗留，不声称解析任意shell或修复旧oversize/parallel遗留。

soleClaude CODE实际exit0/PASS raw `825197205036d2356ca76157df676eac1e4d2c850f55884d2ec3d00b290d1d06`，root实读接受；实际findings `{"P0": 0, "P1": 0, "P2": 0, "P3": 3}`；非阻断遗留 `["P3AC2 residual: public020alsoaliasesAuthorizedReaderFailed/Unavailable; input-specifichelpercrashnotfullyexcludedbycontrol. StaticguarddoesnotpinseparatesnapshotPartialmark. Futurein-cratecfgtest downcast/directsnapshotunit maystrengthen, no claimfixed.", "P3zero-writeextent: object_index/workspace_recordemptyrowsnotpinned; completeobjectfileeqandSession0 keepcurrentexposurerisklow. InheritedDB/WALrawscanlimitationsremain.", "P3comment: data-onlypreview carriesnoauthority ratherthandestroysauthority; currentwordingleftasreviewed.", "InheritedCOV15P3 privateephemeralissuerunobserved, SHA1fixtureonly, fullDBWALscanpartial, integrationdocsdiscoverability andrefusalcausebounded; no fakefixclaims.", "InheritedCOV12R2 CIscannerfuturecargoforms/compoundcommands limitation andoldoversize/parallel/oldselector/audit limitations unchanged."]`，未修复项不声称修复。没有公开命令行为差异、网站新增写集或cf推送；前驱网站仍有效。没有bump/tag/release，不更换official49，不改demo/user04/05或原7stashes。

CODE/QA后只改本两事实计划，另6owned非事实SHA继续绑定已审已测后像；最终109reader/fmtstrict及root事实裁决、signed+DCO main/archive2048/Push/freshremote仍须实际完成。全部闭合后才COVsource22/26→23/26=88.46%，formal0/26仍继承家族C-D；父formal59/71=83.10%、source61/71=85.92%不增。下一卡COV2-16既有12例本机审计。


### COV2-16 开工与 COV2-18 实际源码交接（2026-10-15 19:43:24 UTC；北京时间2026-10-16）

前驱COV2-18已按actual13最终门+2事实plan后109reader/fmtstrict、soleClaude actualPASS P0/P1/P2=0 P3=3及root对AC2有界推断的明确裁决、signed+DCO main `95a9400c841bdad183fd627b74094195dd0f4936`/Push0/freshremote同SHA/archive2048逐SHA/OID/size/mode/kind同验收树闭合；archiveSHA `43e40a4ab7e4588c45f2ec0abe75d87e0c4d8b34915e0dea13cccb4074acf440`，reviewraw `825197205036d2356ca76157df676eac1e4d2c850f55884d2ec3d00b290d1d06`。不声称privatehelperpartial字段直接可见、不豁免nativeSourceA、三P3保留。COVsource现23/26=88.46%，formal0家族C-D，父formal59/71=83.10%、source61/71=85.92%不增。

本卡只写I-audit三份文档，原13AC5VER/max20/S/0prod0landing保持，生产与测试全只读。原VER2名字按COV12actual已审映射 old opencode_extraction_failure_continues_checkpoint→opencode_extraction_no_consumer_keeps_pending_without_checkpoint同步后才执行，原失败历史不改。刷新MacRequired assert/Some的执行路径与三文件全body库存，实际9oldbridge+2pipeline+OG26=12例，0selected/ignore/earlyskip不能通过。所有失败集合如实保存并回到具体实现owner前滚；不在audit改代码，也不把进程fallback/缺前置当成cap/native证明。计划原要求的本机matrix、文档门、fmtstrict仍须实际执行，无账号/模型/真OpenCode/Linux功能测试，无SessionCI、不bump/不安装。无独立linkedIssue。source24与本卡AC/VER均不预支。


### COV2-16 legacy import 终态样例延期与当前格式替代（2026-10-15 19:53:41 UTC，由本次signed main采纳生效）

95a9400最终行为源码只读审计实际11/12通过，唯一失败 `opencode_historical_import_e2e_uses_trusted_export_bytes` 的fixture包含classic `info.status="stopped"`，来源投影closed-info不接纳该非当前格式字段而标unproven，实际import返回LBR-AGENT-020；原9组为8PASS/1FAIL/0ignored，rawSHA `b1ab0f5fe8a6637a25c7484753146bb89d4f391395357cc869f1579589a9e077`，不能记9/9或把失败改PASS。collect_facts虽仍认识legacy status终态，当前ordinary29源要求与该旧契约不一致；把status添加到来源准入会涉及legacy终态兼容，归既有 `DEFER-COV11-IMPORT-TERMINAL-01` 计划外延后，当前默认拒绝不改。用户已授权相似复杂外围问题延后、优先当前OpenCode基础采集与producer；本次不增加生产写权，不弱化candidate/native/Source A边界。

当前审计AC1改用已经COV12source-reviewed的 `cov2_macos_fake_exporter_import_session_capture`：真实trusted exporter经当前格式历史import写入独立checkpoint，绑定独立canonical gold7973604f...与live/import readback，保留actualimport Incomplete/active fence。这**替代一项本轮功能验收**，不是旧compatibility PASS，旧测试body/fixture全部保留不改。COV16当前12名=8个其余旧bridge+1当前格式import+2pipeline+1OG26；COV17仍10个旧名。当前两个audit共22个选定名=21继承+1当前替代；全部新增Sessioncase仍单独要求，不重复计算：原27名immutable/static CI inventory保留，release必需unique26=21继承+5新增，另1legacy名明确DEFER/FAIL。旧22 inherited allPASS历史判据由本段及当前卡/REL/DEP镜像明确覆盖，不倒改任何旧事实。

COV16仍13AC/5VER/S/0prod0landing，26卡和71父卡分母、DAG、SourceA/真实模型/四平台/安全CI/站点安装/最终bumped full要求均不变。此四文档G09协调仅source23/26=88.46%、formal0/26，父formal59/71=83.10%、source61/71=85.92%不增；COV16仍in-progress/空，原失败不能被批准后的新矩阵回写。实际单Claude PASS/root裁决、109reader/fmtstrict和signed+DCO main/Push/freshremote后才采纳；新8+1+1+1+1矩阵仍须实际运行，本提案不预授audit/source24。


**G09 协调写集（与 COV2-16 审计写集分开）：** 本次仅同步以下四条既有文档精确路径：`docs/development/plan/plan-20261013-coverage-schema.md`、`docs/development/plan/plan-status.md`、`docs/development/plan/plan-20260902.md`、`docs/development/tracing/coverage-v2.md`。这是验收范围/依赖镜像的前瞻协调，不是 COV2-16 的实现写权扩展；COV2-16 后续审计仍仅 I-audit 原三文档。四路径只由 root 串行写入，无新增任务/依赖边/生产或测试写权、无版本/安装/网站行为变更，恢复为 signed+DCO main 前滚。G-01–11 复核：单一 legacy 终态兼容延期与当前格式功能替代；COV16 保持 13 AC/5 VER、S、0 生产文件/0 落点，REL 保持 12 AC/12 VER，26/71 卡分母与既有 DAG 不变；原始失败、legacy fixture 和 CI 中仅静态库存检查全部保留。仅单 Claude Code 实际 PASS、root 裁决、文档门/fmt/strict 和签名 main 交付闭合后采纳；不授 source24。


**替代覆盖边界与遗留重启：** 当前格式import正例不覆盖旧classic `info`/`parts`端到端兼容，也没有查询session行来证明`started_at`/`last_event_at`由`info.time`映射，更不证明`stopped_at`终态；pure collect_facts timestamp单测不等于该DB端到端证明。这些缺口与原终态例的FAIL一并明确归计划外`DEFER-COV11-IMPORT-TERMINAL-01`，未修复、不记PASS；将来另有界兼容任务须保留来源安全边界、soleClaude审查并使原legacy用例实际非skip1/1、读回上述时间与终态后才可解除。原COV11的“不免除原AC/VER”不被追溯更改，本次只前瞻调整COV16和依赖/发布集合。

**R1评审事实：** soleClaude实际exit0但VERDICT FAIL，rawSHA `91ceab399575e8dd72f1832a06b1b960e45eebaae30f84cf9df621b0f0d70372`；1P2为五处当前规范未同步，另4P3为覆盖缺口、遗留登记/重启、采纳措辞与时区顺序。本版同步这些项；旧R1不是PASS、尚未采纳。当前修订须自己的实际R2PASS与签推门，不预授新matrix/source24。


### COV2-11 Windows cfg 编译边界前滚（2026-10-15 20:16 UTC；实施中）

COV16延期G09已由actual signed+DCO main `a4a863f67856bf1a3053fcef72e8743b6d0a1eea`、Push0/freshremote同SHA、archive2048同最终109reader/fmtstrict后像以及soleClaudeR2 PASS（raw `a7d1dcb6b40309fcf637b3f091f2bbd30c6e7c034790fddd53e2afea0db984af`，0P0/P1/P2、3P3延期）实际采纳；R1 FAIL仍保留，source23/26不增。当前22名=21继承+1当前格式替代，required unique26；旧classic import终态/时间映射FAIL计划外defer，不追认为PASS。COV16新12matrix仍UNRUN。

发现实际95a9400的必要Windows CI run38795955431/job116633088835失败：`src/command/agent/import.rs:62`的`decode_redacted_import_runtime`仅Unix路径使用却无cfg限制；`snapshot.rs:3132`的unit test调用Unix专用`verified_partial_fixture_snapshot`。返回原COV11实现owner，在既有I24内只把import归入现有`#[cfg(unix)] use`、给依赖Unix fixture的原unit test加`#[cfg(unix)]`，并按本文件既有惯例给`validate_prepared_runtime_response`加`#[cfg_attr(windows, allow(dead_code))]`，保留其Windows编译但允许该平台暂未调用。函数体、原断言/fixture/许可边界、Session CI政策、原8AC/6VER及26/71分母全部不变；COV16 audit I3仍只读，不在审计卡改源码。

此编译范围修正不改runtime/共享SSOT/CLI/CI配置/installer行为，ER13按cfg-only followup为none；原COV11 T1的10212PASS仅保留其原输入历史，不冒称本新整树跑full，家族最终bumped T2双env full仍mandatory。当前需实际helper、原6exact、相关Mac unit/modules/109reader/fmtstrict、soleClaude及signed+DCO main交付；Windows实际新SHA CI仍须真绿，不能以Mac替代。无公开命令变化/网站新增写权、不bump/tag/release/安装，不改globalOpenCode或用户04/05/7stash。当前source23/26=88.46%、formal0家族C-D；父formal59/71=83.10%、source61/71=85.92%保持，不因修复同卡再增加分子。闭合后再执行COV16最终源码12例。


**COV11 cfg CODE R1实际FAIL与R2前滚：** soleClaude R1 actualexit0/VERDICT FAIL，raw `9109468c907f29eebe93efa6319b1359c50d27f8ee29c80b39441b732fc0b970`，1P1指出初始两处错误消除后Windows late-lint还会发现validator及其callee仅Unix调用。原13门Mac PASS仍只绑定R1候选，不是R2通过。R2采用既有Windows定点dead_code属性，不放宽其它平台或全局warnings，不改任何functionbody/assertion；原2Rust/2plans精确I和计数不变。新13门/soleClaude与签推仍须实际完成，Windows新SHA CI仍pending；不把静态推断写成已发生的Windows新FAIL或PASS。


### COV2-11 Windows cfg 前滚本机闭合事实（2026-10-15 20:30:52 UTC）

候选13步实际全部exit0：默认helper build、原6VER各1selected/1PASS/0ignored、Unix partial fixture exact1/1、commandimport37/37、snapshot37/37、64+32+1+12=109readers、fmt与strictall-targets/all-features-Dwarnings。2048输入前后SHA/OID/size/mode/kind同一，manifestSHA `caea7dc3650f755dff974fef45aa996f6ee7dc776144f6f641b7d62eb9eed8c3`。soleClaude CODE实际PASS，rawSHA `2e95bbe94161a611df30e4893da5de23084fa14cbaff5d5bd47bac659cf30f5e`，root实读裁决后才写本事实；审查残留按原报告保存，不擅称已修复。

两Rust精确逆归一化与actual a4a863f逐字相同：仅decoder import移至已有cfgunix组、一个依赖Unix testfixture的unit加cfgunix、validator加定点Windowsdeadcode属性；所有functionbody/断言/fixture不变，非Windows runtime修复。此前95a Windows编译FAIL保留，新SHA实际Windows必要CI仍未运行/不预授PASS；原4scoped fixtures、requiredsecurity及四平台发布门均保留。当前cfg-only followup ER13none、未跑新full，不把旧10212PASS称本次整树full；REL首次bumped T2和全计划最终full仍mandatory。

CODE后只更新本两个事实计划，另两Rust字节仍绑定CODE/QA后像；须最后109reader/fmtstrict、root2048核对、仅stage本卡四路径、actual signed+DCO main/archive/Push/freshremote同一。实际未知新SHA不预填，receipt由后继开工绑定；source23/26=88.46%不重复增加、formal0/26家族C-D，父formal59/71=83.10%/source61/71=85.92%不变。保留用户04/05、原7stash和official49，网站N/A、无版本/安装/模型/SessionCI变化。之后才准备最终源码COV16新12matrix（原11/12 legacyFAIL仍计划外延期），此处不记source24。


### COV2-16 当前12例最终源码本机审计（2026-10-15 20:37:17 UTC；A通过，单审/交付待闭合）

前驱CFG窄修复已实际signed+DCO main `8a547afd0fd5a73247ba47ec979541b3aacba8c9`、Push0/remote同SHA、actualarchive2048同13QA+final109reader/fmtstrict后像；codeR2soleClaude PASS/raw `2e95bbe94161a611df30e4893da5de23084fa14cbaff5d5bd47bac659cf30f5e`。仅import/cfg/lint变化、全部runtime body不变，原CODE R1 FAIL1P1保留。Windows必要CI仍pending，不以Mac冒称通过。此处测试冻结actual8a，root审计仍只写I3文档，不改任何生产/测试。

本轮实际12个不重复功能用例全部PASS/0FAIL/0ignored：其余old bridge8/8（raw `29bd53356664efd2204229f29f3b23f4fd2aac7be6ef9df03adcf2aade56ad42`）＋当前格式import替代1/1（raw `e37324c9f50ea2931634034ec2841790b63c8219c248e8e1782f6ea8d78567c1`）＋no-consumer extraction1/1（raw `83be6e7d6387ccc1d60a9b854f26e230e16a4cf6b192c31dd5bfce33fc6082b4`）＋lease1/1（raw `bec70f4136de611348cc0f283ad3fe66bfe01fb14067bb74b818f5ccb9a20b5a`）＋OG26 zero-normal-sinks1/1（raw `919475e9442ea40514b2018a73cd77b05fc461d0daa9d785775bb92e6a262bb5`）。每个实际testname、匹配数、exit及start/end均与原始Cargo报告相符；静态non-skip/manual-only两门各1/1，64+32+1+12=109readers、fmt和strictalltargets/allfeatures-Dwarnings实际exit0。共12个QA步骤，2048文件前后SHA/OID/size/mode/kind同一，manifestSHA `4f0fde5edd16d4d9fdc21e017b3c6ef45e8e1ff90c655021177a517d4ad05208`。

- `src/internal/ai/capture/live_pipeline_tests.rs` SHA256 `3413f5059f1899c499e7c8e0067da5e1c5b1babd11502071c2ab2c4467ad9722` mode `0o644`。

- `tests/agent_checkpoint_redaction_test.rs` SHA256 `ef1896407d1a0db29be11dcb25892659fb96f5d3df1a524d72eecc247f3b7cb2` mode `0o644`。

- `tests/agent_opencode_bridge_test.rs` SHA256 `c04d3544f3e1dd492a6be269efaa9962f90855e46d507af7bf16c0c2c5b915c8` mode `0o644`。

Required macOS Seatbelt assert和每个选定body的执行路径已对照，未以缺sandbox/0selected/早return计PASS；static guard两门并不额外算功能用例。旧oversize body若先遇到sandboxfork拒绝，仅证明保守降级，不扩大成实际触达byte-cap正例。旧classifier私有字段仍只有此前审定的有界路由推断，不声称直接观测或SourceA。原parser postprocessing首次同一行ok正则漏识别nocapture插入日志，断言失败保留；只修私有收据解析，用实际name+exit+pass/fail/ignored结果校核，源码和测试从未改动。

本轮范围严格依actual signed G09 a4a863f：AC1为current-format功能替代。旧classic status用例在95a实际8PASS/1FAIL的原FAIL/raw b1ab0f5...保留计划外DEFER-COV11-IMPORT-TERMINAL-01，classic info/parts、session行时间映射/终态缺口未修复、不追认兼容PASS；原27静态inventory与所有测试body未改。两个audit当前22名=21继承+1替代，替代已在5新增中；required unique26。COV17十例/真实visible-only、SourceA/native三字段、家族bumped full/安全CI/四平台/站点安装门均不减。

原13AC/5VER按实际测试勾选，但Lifecycle仍in-progress/Acceptance空，须新增三文档相关QA、soleClaude审计PASS/root裁决及signed+DCO main/archive2048/Push/freshremote后才source24/26=92.31%；此前仍23/26=88.46%、formal0/26家族C-D。父formal59/71=83.10%、source61/71=85.92%不增；71/26分母不变。无新full、Linux功能/账号模型/nativeissuer/注册/网站/版本/安装/SessionCI操作。当前runtime已装official49原SHA保持，用户04/05及原7stash保留。下张COV2-17只在本卡源码交付真实闭合后开工。


### COV2-16 单审与最终事实闭合（2026-10-15 20:48:59 UTC；signed交付仍待）

soleClaude实际exit0/VERDICT PASS，P0/P1/P2=0、3P3，rawSHA `102e022978e69fa137bcb6c11c89d074a404f6be987d95dd5d745da7e98098f9`；root已实读并接受actual12/12、原13AC5VER、source2048同一性和审计边界。三P3在本两事实计划明确关闭：VER4刷新actual8a macOSassert40–44/nonMacbwrapreturn49–57；本卡Current evidence记录COV12 old→new exact映射；本段补实际argv与env加载。无生产/测试/验收范围变更，不预授nativeSourceA/旧legacy兼容或真实byte-cap正例。

实际每个QA子进程由zsh -f静默加载root `.env.test`和`.env.live-test`，任一source失败会exit90；本轮全部QA exit0，所以两文件加载实际成功。仅记录文件名，不记录值/密钥；清除了模型/native/local Session opt-in，cargo jobs2，target为隔离warm源树。实际使用额外`--locked --offline`，保持已审锁定依赖；五功能命令的真实argv如下（分别8/8+1/1+1/1+1/1+1/1，合计12 unique）：
- `cargo test --locked --offline --features test-live-agent --test agent_opencode_bridge_test opencode_ -- --ignored --skip opencode_historical_import_e2e_uses_trusted_export_bytes --nocapture --test-threads=1`。
- `cargo test --locked --offline --features test-live-agent --test agent_opencode_bridge_test cov2_macos_fake_exporter_import_session_capture -- --ignored --exact --nocapture --test-threads=1`。
- `cargo test --locked --offline --lib internal::ai::capture::live_pipeline::tests::export_runner_lease::end_to_end::opencode_extraction_no_consumer_keeps_pending_without_checkpoint -- --ignored --exact --nocapture --test-threads=1`。
- `cargo test --locked --offline --lib internal::ai::capture::live_pipeline::tests::export_runner_lease::end_to_end::export_stage_releases_runner_lease_on_every_exit -- --ignored --exact --nocapture --test-threads=1`。
- `cargo test --locked --offline --test agent_checkpoint_redaction_test og26_candidate_has_zero_normal_sink_hits -- --ignored --exact --nocapture --test-threads=1`。

另有helper预备构建、staticno-skip/manual-only各1/1、109reader/fmtstrict全PASS；原functional12步完整2048输入前后同一，新增I3docs三门也实际109/fmtstrict通过且其余2045输入同functional8a一致。原parser-only断言失败不改测试结果。`coverage-v2.md`保持本轮CODE已审字节；其“单审/交付待”属于20:37UTC前审计快照，当前生命周期/门结果以本两事实计划与实际签推收据为准。

实际必要Windowsjob `116656195322`（run `38802675784`、head `8a547afd0fd5a73247ba47ec979541b3aacba8c9`）现completed/success，completed_at `2026-10-15T20:41:24Z`；只证明该8a的libtest编译/4scopedfixtures，不冒称后续未知SHA CI或全部CI全绿。旧a4构建run38800340609已fresh确认completed/cancelled，旧提交自runner转最新8a；当前Windows无需重复因同一失败重跑，后续新commit仍保留其自身必要CI门。

本卡现in-progress/locally-accepted、A/B已闭合，formal0仍继承家族C/D。CODE后仅这两事实计划变化，机制doc与全部生产/测试保持已审已测SHA；须最后109reader/fmtstrict、root2048同一性、仅stage I3、actual signed+DCO main/archive/Push/freshremote后才source24/26=92.31%；此前仍23/26=88.46%。未知提交SHA不预填，后继ER02绑定真实收据。父formal59/71=83.10%、source61/71=85.92%不变，26/71分母不变；旧legacy FAIL/DEFER、SourceA/三native字段、COV17真实观察、bumpedT2full/四平台/站点安装门保留。没有新full、bump/tag/release/安装、Linux功能或模型/账号/nativeissuer操作，网站N/A（无公开命令变化）。下一张COV2-17只在实际源码交付闭合后开工。


### COV2-17 开工与10例实际审计（2026-10-15 21:06:04 UTC；真实响应观察仍UNRUN）

前驱COV16实际signed+DCO main `20f7787b8c8589e1af24c71d1835973adbce3d98`，parent8a547af；archiveSHA `ac25cbcfb3cfd6cbccaecd742c046f10e810d85b34a91a215e8e8d4ebd1924fc`，2048文件与最终QA后像相同，3个I文档外2045行为输入一致；实际Push0/freshremote同一，soleClaude PASS与root裁决已闭合。source当前24/26=92.31%，formal0/26仍家族C/D，父formal59/71=83.10%、source61/71=85.92%不变；Windows8a特定job实际success不预授20f新SHA的CI。

本卡在actual20f最终行为源码运行：lifecycle7/7＋crash1/1＋import确认1/1＋oracle1/1，共10uniquePASS/0FAIL/0ignored；两个no-skip/manual-only静态门各1/1，109reader（64+32+1+12）、fmt与strictalltargets/allfeatures-Dwarnings全部实际exit0；共10个QA步骤，2048前后SHA/OID/size/mode/kind同一。

- VER1-lifecycle7 actual 7/actual7，raw SHA256 `1a816571d12de02af202ed609f18dd5a55a24e2b6cb27e8f29543b48ecb7394a`，UTC 2026-10-15T20:56:24.558735+00:00→2026-10-15T20:56:53.387131+00:00。

- VER2-crash actual 1/actual1，raw SHA256 `2b78a5b2bc5029be01943873564f0134eb5c0f42ec755009a2f8655df0142abf`，UTC 2026-10-15T20:56:53.387309+00:00→2026-10-15T20:57:01.473408+00:00。

- VER3-consent actual 1/actual1，raw SHA256 `776dbbc2e3ad631876ae93c4f0dabf7b904efcc7c5583469059fcbecd1044c18`，UTC 2026-10-15T20:57:01.473599+00:00→2026-10-15T20:57:05.552888+00:00。

- VER4-oracle actual 1/actual1，raw SHA256 `493cd059e627aeb66a06a460e8363204d872f81f4d56eb95c3d5e8012e59a328`，UTC 2026-10-15T20:57:05.553099+00:00→2026-10-15T20:57:19.608628+00:00。

- `tests/agent_lifecycle_event_test.rs` SHA256 `c0bdb6ca4e4ce3f77c2e7e031f2af3551cfed58a2b3798b6143a7c6989facad4` mode `0o644`。

- `tests/agent_hook_crash_test.rs` SHA256 `5c16c45c9e4fc6929b4681a30b4a8f541357240c708d61d1199e68a18f87e977` mode `0o644`。

- `tests/agent_import_test.rs` SHA256 `9f165781a65971f9dbac5ed6442353e681be429ae73bef39987351371680dc74` mode `0o644`。

- `src/internal/ai/capture/live_oracle_tests.rs` SHA256 `2493e17afad7211842e046c41f5327ae5ed7e902c6fd64ce76871904c301d0b8` mode `0o644`。

原具名10项与实际Cargo所选name/positivecount/exit一一对应；lifecycle public-contract表包含OpenCode surface；crash固定8row中两条OpenCode逐一进入for循环，fault/replay/claim/receipt/session oracle无早退；metadata三provider包含OpenCode桥不可用row并与冻结整张gold逐字断言。所选body没有环境skip或return；两个静态guard不额外算功能用例。这些本机fixture并非真实native来源/正例；原COV16legacy FAIL/defer及SourceA仍保留。

AC1–11/VER1–5按实际通过勾选；AC12/VER6仍未运行、不勾选。当前已验证官方OpenCode2.0.29 SHA7f46ba1...、88个公开builtin plugin active、外部plugin0、官方Anthropic endpoint配置一致、Haiku4.5 enabled、thinking disabled；工具权限最后wildcard deny后仅有browser deny，属于更窄deny而非allow/ask。官方2.0.29 `packages/core/src/permission.ts:87–95` 使用 `findLast`（第91行），故最后匹配rule优先；追加browser deny不会重开工具。实际四轮私有preflight停止，均在任何模型请求前（attempt0）；首轮只记总阶段、未保留更细原因，后续修正owned路径canonical比较及Location.response wrapper，并将过严的最后一条恰为wildcard检查改为最后wildcard deny之后只能继续deny。第四轮内容无敏元数据证实追加browser deny。这些是collector准备，不是模型FAIL或模型重试；其原STOPPED记录保留。已修正私有collector，不改任何仓库源码/测试/原AC/VER。独立私有Rust classifier初次误用cfg(test)辅助方法编译失败保留，修正调用现有public with_deadline后实际编译exit0；仅依已审pure preview API，不授native能力。

本卡只写原I4文档，无生产、测试、CI、installer/版本或公开命令变化，网站N/A。Linux功能由用户后验；无新full，家族首次bumped T2全量/安全CI/四平台/站点安装仍必需。未登记issuer、不live-mint、不修改globalauth/config，owned credential以内存转移到临时store，完成后清理；不存响应正文或密钥。Source A/native三字段不因可读响应放行。未知后续SHA/结果不预填，soleClaude/root/signed交付待；source25/26只在本卡全部门实际闭合后登记。


### COV2-17 真实visible-only观察结果（2026-10-15 21:07:35 UTC；功能A通过，单审/交付待）

预登记 `COV17-OBSERVATION-PREREG-20f7787` 在实际启动前已写本文；gate UTC `2026-10-15T21:06:04.673195+00:00`，绑定prereg SHA `f9ac142572ba7f629669b1db78546e60fc7af69e8f2ffe338a30739fb41e4d63` 与当时plan SHA `4606c0a9a235511d421fd84a67552bd0ca6eb2cc9f184807559a903c0109b161`。实际唯一attempt UTC `2026-10-15T21:06:04.962167+00:00`，结果UTC `2026-10-15T21:06:07.317946+00:00`；同一owned服务上重核88 builtin/0external、配置/endpoint/model/权限后才发起，genuine provider run/原始export/当前classifier实际exit均0，elapsed `2.355785`秒，小于600秒，不是按elapsed估算成功。1个新Session、1个step_start/1个text事件；无工具事件，无附加模型尝试，也不把一次CLI启动说成只有一次HTTP请求。

当前pure classifier在actual20f行为源码实际编译的artifact上读同一未经sanitize/字段删除的export bytebuffer，实际classifier_ok=true、partial=false、1turn、coverage版本[1]。原始树全层有界检查state/signature/redactedData/reasoningEncryptedContent/originProof/thoughtSignature计数均0；结果HMAC-SHA256 `c1275413b2e9f8b84c87438d89182b61f51b26c258a18c6338c6844dd1d8f6ac`，新鲜HMAC key只在内存、未留存。只观察到普通可见文本；coverage-v1不证明typed Reasoning positive，native/SourceA credit=false。原始响应、stderr/credential内容不进证据；owned运行store已在进程终结后清理，保留的只是固定字段名、JSON类型/数量与以下无敏元数据。并未更改global auth/config、登记issuer、调用live mint或给native三字段放行。source-side `auth export anthropic` 使用现有用户环境且没有显式 `--server`，可能ensure/启动其现有managed服务（official29 cli auth/export.ts:12 与services/server-connection.ts:21–52）；未宣称该源端的服务或文件系统副作用为零，目标auth import仍仅owned store。

观察收据SHA256 `5eda73cd5b53e4354a3314d55e754441da808cafddfcd020a63f8befc2daf97a`；独立原始树计数/classifier无敏收据SHA `c601d95d0712ccf8d4820c0299de56193daa7b6ae4eb5d1275b7b83a54b5bbe1`。内容无敏字段清单如下（`<other>`合并未知字段名，未保留任意自由字段/值）：

```json
{
  "field_names_types_counts": {
    "info:object": 1,
    "id:string": 7,
    "projectID:string": 1,
    "agent:string": 3,
    "model:object": 2,
    "providerID:string": 2,
    "<other>:string": 6,
    "cost:number": 2,
    "tokens:object": 2,
    "input:number": 2,
    "output:number": 2,
    "reasoning:number": 2,
    "cache:object": 2,
    "read:number": 2,
    "write:number": 2,
    "time:object": 5,
    "created:number": 5,
    "updated:number": 1,
    "<other>:number": 2,
    "title:string": 1,
    "<other>:object": 1,
    "directory:string": 1,
    "messages:array": 1,
    "type:string": 5,
    "text:string": 2,
    "<other>:array": 1,
    "completed:number": 1,
    "content:array": 1,
    "finish:string": 1
  },
  "state_proof_candidate_counts": {
    "originProof": 0,
    "reasoningEncryptedContent": 0,
    "redactedData": 0,
    "signature": 0,
    "state": 0,
    "thoughtSignature": 0
  },
  "node_count": 72
}
```

AC12/VER6仅按上述实际观察勾选；原12AC/6VER现均有具名实证，Lifecycle仍in-progress/Acceptance空，不能在Claude/root/signed交付前称本卡已交付。后续只有I4文档变化，全部2044非I源码/测试/配置/fixtures与actual20f行为同SHA/mode；需新增I4文档109reader/fmtstrict、唯一Claude PASS/root裁决、签名+DCO main/archive2048/Push/freshremote，同一性闭合后才source25/26=96.15%并登记DEP-OG-13 source-ready。formal0/26仍家族C/D，父formal59/71=83.10%/source61/71=85.92%不变。下张COV2-REL-01首次bumped最终源码双env full、安全CI、四平台签名发布、站点/Homebrew/curl安装及原`.2`兼容门均保留；这次普通响应不关闭Source A或current`.3` SRC029/RUN029。


### COV2-17 唯一Claude PASS及事实收口（2026-10-15 21:15:21 UTC）

本卡CODE/审计唯一Claude Code实际exit0、result_is_error=false，最后原文VERDICT: PASS；0P0/P1/P2、3非阻断P3，raw SHA256 `4c177563ae451dbdad7ba32a852efd4d14ec86fa602f41b3ed380ce5de660a48`，实际结束UTC `2026-10-15T21:13:14.169412+00:00`。root已按原12AC/6VER与actual10uniquePASS/0FAIL/0ignored＋1次真实visible-only、109reader/fmtstrict和原始2048同一性接受；未改生产、测试、gold、native授权或任何AC/VER/I/R。三P3以本I4事实澄清关闭：旧未观察横幅标为21:06前历史；last-match权限语义补official29 permission.ts:91；源auth export可能ensure已有managed服务的副作用不声称为零。原R1 PASS与全部4次attempt0 preflight STOP及私有classifier R1 compileFAIL历史保留；不将它们改记模型PASS。

Lifecycle现在in-progress/locally-accepted（A/B已满足），formal仍0/26继承COV2-REL-01实际C/D。本轮最后事实/标签补写只在原I4，全部2044非I源码与actual20f行为相同；还须对这些最终事实字节重新109reader/fmtstrict、soleClaude事实复核，之后root只stage I4、signed+DCO main/archive2048/Push/freshremote同一才交付。CODE审查之后未再执行模型/fixture；不重试20f同一行为源码。未知final提交SHA与事实复核结果不预填，受审后像/实际SHA由root交付收据及后继ER02绑定。

**DEP-OG-13 source-ready条件交接：** 仅当本签名提交实际main Push/freshremote与以上源码/审查门闭合，25张nonrelease（23implementation+2audit）才全部source-qualified，COVsource24→25/26=96.15%，登记source-ready并允许隔离准备；在Push前当前仍24。current22auditnames为21继承+1current-format替代，actual12+10；替代与5新增去重required26；旧classic终态1FAIL明确计划外延期。父formal59/71=83.10%、source61/71=85.92%不增。source-ready不等complete，OG27 main Push仍须COVRELstable patch实际full/securityCI/四平台/网站/Homebrew/curl安装C-D及独立SourceA；native029 SRC/RUN/三字段不因此关闭。

下一卡COV2-REL-01，先刷新upstreamlatest并仅patch顺延（最近只读latest0.30.49，拟0.30.50，冻结前再核）；首次bumped实现树双envNextestfull＋VER3–9本机六项/一次真实观察必须实际通过，原`.2` fresh owned epoch0兼容登记另验。当前不bump/tag/release/安装，无Linux功能或SessionCI操作；Windows/security CI留真实当前releaseSHA，旧cancelled不得记绿。网站无公开命令变化N/A；release网站/下载站与安装D门继续保留。用户04/05dirty/原7stash/正式49两binary必须保持。


### COV2-REL-01 ER02开工与唯一R写权（2026-10-15 21:22:49 UTC）

前驱COV17实际signed+DCO main `73500993352fb53b2a7ebe61f136745280d0e784`，parent20f7787；实际stage/commit/archive/Push exit0、freshremote同一，archive SHA256 `3864227fd776c57949b2d3078886986c4e7b18aaadd96be72323ce09f1a4a5a5`、manifest SHA `4f012e2c7e952afd0cb4f57a34a5f047d114ad8752a3e6e0f4ca63addd4b5c43`，all2048与最后QA相同、all2044非I行为输入与actual20f相同。原12AC6VER、actual10uniquePASS/0FAIL/0ignored＋one genuinevisible-only（1attempt2.355785s，partialfalse/v1/no native），soleClaude主审raw `4c177563ae451dbdad7ba32a852efd4d14ec86fa602f41b3ed380ce5de660a48` 与最终I4事实复核PASS/raw `67152e3a30931c776c58e3cc12502bdf294ace55a0bc5442f1c8e5f6b7ad5d41`、root已闭。COVsource实际25/26=96.15%，DEP-OG-13现为source-ready；formal0/26仍待本卡真实C/D，父formal59/71=83.10%/source61/71=85.92%不增。COV17机制文档“单审/交付待”顶部为R1前快照，当前事实以上述signed收据为准；R2两个P3均非阻断，其中source范围root实际核了permission.ts87–95/findLast91和server-connection.ts21–52→resolveManaged74–84。私有postprocessor两次误猜literal范围失败保留，不是源码/测试/模型FAIL，不冒称密码/响应读取或extra模型重试。

本卡原12AC/12VER、I3（三plans）/R4（Cargo.toml、install.sh、install.ps1、toolchain Cargo.lock）及既有release tag/assets写集不变；root唯一writer，用户04/05dirty、原7stash与两official49binary受保护。所有25nonrelease原AC/VER当前均勾选且locally-accepted；其历史签前pending文字不代替实际signed交接收据。全部源码以735 archive2048开工；未引入OG27/current`.3`实现。选择原已批准RouteB：SRC029仍未ready（原149 UNPROVEN/prospective68未采纳），不发sharedgrant，先本COVREL closeout/DEP13complete再按原门grant；R仅root针对本F候选，禁止native29源码/共享writer并入。此处不是授出lease或宣称SourceA。

实际刷新GHlatest stable为v0.30.49、v0.30.50 matchingtags空；R候选仅patch0.30.50。三版本面仅49→50，Cargo.lock由实际cargo metadata --offline --format-version1生成且机械核只libra rootpackage version49→50，其余lock/deps字节不变；尚未commit/tag/release，不把拟版本当已发布。冻结前及实际发布前再核upstream；仅纯上游patch顺延、实现/测试/行为文档逐路径SHA-mode等价时依用户授权复用首次bumped已绿full，绝不复用不同源码。当前首次T2 full尚UNRUN，不能拿COV11T1或049旧full代替。

VER3–8须在首次bumped最终行为树实际重跑六项非skip本机case；VER9仅另一次预算内genuine visible-only观察，须事前完整sanitized argv/build/model/plugin/transport/source登记，不能重用COV17旧样本。VER10/11逐行为文件SHA/mode同一才可引用12+10原始审计，否则完整重跑；旧classicstatus/time终态FAIL仍计划外延期，required22=21继承+1替代/与5新增去重26，不准把失败记绿。当前全部release AC/VER尚不勾选，模型/fixture/full新结果都UNRUN。

当前backend正式根`/Volumes/Sea/libra-backend`已只读核clean/synced cf `3f7ca9c4eb862d75a9c1b66811927632bd03151b`，当前命令文档部署继承COV11既有3f7真实记录；本卡无新CLI行为，不修改站点命令，实际站点/下载/CDN/manifest/Homebrew/curl安装仍要在release D再核。Linux功能继续用户后验；security/Windows及四平台稳定构建要求保留，任何旧cancelledCI不记当前SHA通过。source-only已完成不等家族正式done，nativeSourceA/三字段/SRC029/RUN029独立保留。


### COV2-REL-01 首次bumped0.30.50本机六项与审计同一性（2026-10-15 21:35:17 UTC）

本候选R三版本面实际50、toolchain lock仅rootpackage49→50；在该bumped树实际sixuniquePASS/0FAIL/0ignored（含首次featured no-run与defaulthelper实际build0），versionparity2/2、109reader64+32+1+12、fmt及strictalltargets/allfeatures-Dwarnings全部exit0，共12QAsteps；source2048前后SHA/OID/size/mode/kind同一。full_suite_run=false：这不是T2full通过；首次bumped Nextestfull仍UNRUN。

- VER3-live `cov2_macos_fake_exporter_live_session_capture` actual1/1，rawSHA256 `2486c08563dd07e945b6b481999bd6cf484505539c8e069d9fa0c94be243bfc0`，UTC 2026-10-15T21:25:11.511364+00:00→2026-10-15T21:25:16.776445+00:00。

- VER4-import `cov2_macos_fake_exporter_import_session_capture` actual1/1，rawSHA256 `db6630d139ce9d410ecdafa9e160e420d903733c324b80ae83b2cf373a3e13f1`，UTC 2026-10-15T21:25:16.776652+00:00→2026-10-15T21:25:23.032356+00:00。

- VER5-failure `cov2_macos_failed_export_session_zero_write` actual1/1，rawSHA256 `fdad30811d5100fad5f6d2d99b71fff7ecad709068a0abaed594f332881644ec`，UTC 2026-10-15T21:25:23.032540+00:00→2026-10-15T21:25:26.396680+00:00。

- VER7-candidate-live `cov2_macos_fake_exporter_candidate_live_partial_no_plaintext` actual1/1，rawSHA256 `4b6aa7d5c1996e8d3fb5e59eecb8d7f8b7aedd443eb3d65fee357e71bf797a26`，UTC 2026-10-15T21:25:26.396869+00:00→2026-10-15T21:25:30.531691+00:00。

- VER8-candidate-import `cov2_macos_fake_exporter_candidate_import_partial_no_plaintext` actual1/1，rawSHA256 `31c4606c781de6759fc20be21f1c710f7c6b268745ba88103f4f1ac839e801df`，UTC 2026-10-15T21:25:30.531889+00:00→2026-10-15T21:25:35.396630+00:00。

- VER6-extraction `internal::ai::capture::live_pipeline::tests::export_runner_lease::end_to_end::opencode_extraction_no_consumer_keeps_pending_without_checkpoint` actual1/1，rawSHA256 `08666a4acd93043c294343ab65bef64dc2a7667334f739e527e2e784543281b0`，UTC 2026-10-15T21:26:44.626943+00:00→2026-10-15T21:28:24.174630+00:00。

VER10/11依原门核以下7个行为文件SHA/mode与实际COV16(8a)/COV17(20f)审计完全一致，故可引用其actual12/12与10/10、OpenCode行结构/非skip路径及原日志；没有把旧model样本重用。其余全部2045非I3输入（含R4）随后须同一，任何行为变化回owner并重跑受影响全量/完整12+10。

- COV16 `tests/agent_opencode_bridge_test.rs` SHA256 `c04d3544f3e1dd492a6be269efaa9962f90855e46d507af7bf16c0c2c5b915c8` mode`0o644`。

- COV16 `src/internal/ai/capture/live_pipeline_tests.rs` SHA256 `3413f5059f1899c499e7c8e0067da5e1c5b1babd11502071c2ab2c4467ad9722` mode`0o644`。

- COV16 `tests/agent_checkpoint_redaction_test.rs` SHA256 `ef1896407d1a0db29be11dcb25892659fb96f5d3df1a524d72eecc247f3b7cb2` mode`0o644`。

- COV17 `tests/agent_lifecycle_event_test.rs` SHA256 `c0bdb6ca4e4ce3f77c2e7e031f2af3551cfed58a2b3798b6143a7c6989facad4` mode`0o644`。

- COV17 `tests/agent_hook_crash_test.rs` SHA256 `5c16c45c9e4fc6929b4681a30b4a8f541357240c708d61d1199e68a18f87e977` mode`0o644`。

- COV17 `tests/agent_import_test.rs` SHA256 `9f165781a65971f9dbac5ed6442353e681be429ae73bef39987351371680dc74` mode`0o644`。

- COV17 `src/internal/ai/capture/live_oracle_tests.rs` SHA256 `2493e17afad7211842e046c41f5327ae5ed7e902c6fd64ce76871904c301d0b8` mode`0o644`。

Release新AC/VER暂仍不勾选（source-qualified/freeze/C-D谓词未齐），不会以候选本机6项完成冒称source26/26或formaldone。真实visible-only还未启动：当前owned预检已就绪，firstbumped另计一次模型预算且事先注册；COV17旧观察attempt=1保留、不会在20f相同行为源码重试。本次编译pure classifier为实际Cargo package `libra@0.30.50`，与已bump行为manifest绑定；runtime代码/原API未修改，仍无SourceA/native权限。

旧legacyclassicstatus/time终态FAIL计划外延期，22auditnames=21继承+1current替代，替代与新增case去重required26；fake/exporter/fixture不当真实native。当前source25/26=96.15%、formal0家族C/D，父formal59/71/source61/71不变，SRC029/NativeSourceA/三字段独立保留。RouteB无sharedgrant，root仍唯一I3/R4writer，网站cfclean3f7只读预核不当本release站点D通过。


### COV2-REL-01 firstbumped真实响应已观察；T2full尚待（2026-10-15 21:37:08 UTC）

依原VER9提前注册COVREL050-OBSERVATION-PREREG-e26b43c，gate UTC `2026-10-15T21:35:17.550954+00:00`、prereg SHA `6c57761820c8c23380913fda46ceae94b6133c7f1e58c46a0177c983a2b49871`、当时planSHA `272f4ff6167b8f376f70ee1e0bbde09f925308ee078521b7a0975f62d9b5ae1c` 已先落盘。实际one additional firstbumped attempt UTC `2026-10-15T21:35:17.886424+00:00`，结果UTC `2026-10-15T21:35:21.190626+00:00`，actual run/export/classifier exit0/0/0、elapsed `3.304198`秒（<600）；1个newSession、step_start1/text1，文本非空检查实际通过，未留内容。当前bumped50自身另一次预算已用1，不再重试同一行为源码；20f旧观察未替代、未新增任何其它推理请求承诺，一CLI仍不等一HTTP请求。

Same original export不sanitize、不改字段，bounded fulltree state/signature/redactedData/reasoningEncryptedContent/originProof/thoughtSignature计数全0；actualcurrent50 pureclassifier partial=false/1turn/coverage_versions[1]。新鲜memory-onlykey HMAC-SHA256 `16822af650c711ce2b1d8abbb06cdeb954fdd35362143cfb9859526bb923ade8`，key不留存，观察无敏收据SHA256 `ab6797d46232fd7b02b6c8791fc6080cf3970decae23d0377ff82a1f82016a50`。下表只含固定字段名、JSON类型/数量；没有模型正文、密钥/credential/header或SessionID。owned临时运行store已清理；source-side global managed-service可能ensure/start的既有caveat保留，未登记issuer、live-mint或改global auth/config。text-only/v1不授typed Reasoning/native/SourceA。

```json
{
  "field_names_types_counts": {
    "info:object": 1,
    "id:string": 7,
    "projectID:string": 1,
    "agent:string": 3,
    "model:object": 2,
    "providerID:string": 2,
    "<other>:string": 6,
    "cost:number": 2,
    "tokens:object": 2,
    "input:number": 2,
    "output:number": 2,
    "reasoning:number": 2,
    "cache:object": 2,
    "read:number": 2,
    "write:number": 2,
    "time:object": 5,
    "created:number": 5,
    "updated:number": 1,
    "<other>:number": 2,
    "title:string": 1,
    "<other>:object": 1,
    "directory:string": 1,
    "messages:array": 1,
    "type:string": 5,
    "text:string": 2,
    "<other>:array": 1,
    "completed:number": 1,
    "content:array": 1,
    "finish:string": 1
  },
  "state_proof_candidate_counts": {
    "originProof": 0,
    "reasoningEncryptedContent": 0,
    "redactedData": 0,
    "signature": 0,
    "state": 0,
    "thoughtSignature": 0
  },
  "node_count": 72
}
```

当前Libra行为identity为firstbumped50 manifest `e26b43c1ad6141a4351e07940c7a0d45487b422f2166dbb064a07f6559083846`：all2045非I3（含R4versionfaces）与bumped六项受测/当前classifier源码完全相同；自其后只变I3事实/预登记。下一步先对新增I3最终字节109reader/fmtstrict，再冻结full输入2048，按双env `cargo nextest run --locked --offline --all --no-fail-fast --retries 2` 实际执行首次T2full；此处full仍UNRUN，不填任何未知summary/sha/通过。full过程中不写被测源码；之后若仅I3纯事实变动，以2045行为同一性绑定finalsigned F。任何真实源码变化则回owner/重验，绝不沿用不同实现full。

COVsource仍25/26、formal0家族C/D、父formal59/71/source61/71。Release 12AC/12VER尚不预勾：sourcefreeze/review/signedmain/securityCI/tag/四平台/manifest/Homebrew/站点/curl安装/旧`.2` freshownedepoch0兼容检查均按实际门完成后登记。Linux用户后验；旧classic终态FAIL、SRC029/SourceA/native三字段边界保留。RouteB当前无sharedgrant，无OG27/current`.3`实现混入本release候选。


## G09 — 历史 observer recipe 兼容前滚（2026-10-15 22:05:45 UTC）

本节为本次三文档具体规范候选；仅实际soleClaude PASS、root裁决、相关文档门/fmt/strict与signed+DCO main采纳后生效，不是实现/发布PASS。依据用户“推荐方案无需再决策”的既有授权，新增独立兼容修复卡FIX-OG-OBSERVER-RECIPE-01，避免把生产变更塞入prod0发布卡。原卡/历史收据不倒改；不新建Issue，不向其它Session发消息。root唯一共享写者。

**实际发现：** 原signed6685b82a938919a8e51f46b7c51cc305b40189a2的approved_build.rs公开前像46313B/SHA5c358618b914d495a1fde9edc84e1c9f3a34be4e2120e3b5cbb466b138bc6175，`.1/.2`均绑定13600B template9c67213be10e16538bfa550501a2dcb7a21c24493b3a8f05592f6c96ff4be27d；current735两catalog改为cd53b2535c3dccd147ea851c8089c405ea84fcfaf2237e4a8284969520bdfdbd而固定producer未变。原`.2`recipe对象与旧source完整恢复相等。load原observer或重读原registry完整policy会被拒；这是有限source推导，不伪称已执行注册。修复只恢复旧固定构建已有静态recipe，不回退普通OpenCode多目录observer，不扩unknown/current`.3`许可。

**当前验收事实：** firstbumped50实际full exit0，10216/10216 passed、2slow、2flaky、23skipped，rawSHA c085795ab90a2cf876670935da0e48d5a9bf253425f5aef19e4da6965f527f70，2048输入前后同一。两TRY1FAIL均TRY2PASS，确切首次失败原因UNKNOWN并保留；未把它们写成首遍全绿。六manual和额外一次genuinevisible-only也实际PASS但均属本次修复前行为树；未来源变化必须重新验证最终源码，不复用旧full/样本假通过。旧legacyterminalimport FAIL/DEFER、SourceA/current3与三字段硬门全保留。rootownedR4候选50已私有park，root四面恢复signed735原49字节（lock整文件原样恢复），无F/tag/release/install产生。

**计数与顺序：** 采纳后parent71→72，formal59/72=81.94%、source61/72=84.72%（分子不增）；旧71均只作其原时点历史。COV仍26卡/source25已交付/formal0。顺序为本G09采纳→新修复卡actualA/B＋site＋T1＋Claude＋signed-source→COVREL最终bumped50新T2/六manual/事前登记的一次最终真实visible观察/审计同一性→F exact CI/四平台/站点/安装/原`.2`epoch0及C/D→修复卡与COV家族正式收口/DEP13complete→原RouteB current3/SRC/RUN/OG27。任何门失败保留原行并停止冻结，推荐窄修，不让失败变PASS。

| 协调项 | 当前规范/owner/证据/失败路径 |
|---|---|
| DEP-COV2-OBSERVER-RECIPE | incoming：parent FIX-OG-OBSERVER-RECIPE-01→COV2-REL-01。root sourceowner交出exact源manifest/SHA/mode、gold/负例、全native/hook/docs/fmt/strict、actual49T1、soleClaude PASS、signed+DCO main及freshremote/archive；未齐则pending并阻freeze。只等source deliverables，不等下游Acceptance，避免C/D环。 |
| 发布分组与单写者 | 新卡是COVREL显式familychild，I10root＋site1，prod3 touched保守（1runtime+2test-only）/3落点/6AC6VER，独立sourcecommit，不写R；COVREL仍I3/R4和12AC12VER、prod0。root暂停pre-freeze发布候选，先授新卡legacy-only排他slot；没有current`.3` sharedgrant，也不改原SRC029/RUN029/OG24当前6AC8VER。 |
| 追溯/测试矩阵 | catalog静态漂移→新卡VER1/3；旧registry各状态重读→VER2；公开docs/site→VER4；sourcequalification/T1→VER5；既有COVREL VER2/12 installed-F原`.2`freshownedepoch0与完整C/D→VER6。普通插件目录隔离源码与既有回归保持。 |
| 里程碑与回滚 | 新卡source-ready才重启COVREL冻结；actualC/D后新卡done/complete。FAST-OG-04下游C/D独立pending，current3/SourceA未闭。发布前修复只前滚源码；发布后immutable tag/assets不得改，缺陷新patch前滚。 |
| 风险/边界 | restore的是已审历史完整recipe，不能从descriptor反向扩catalog、替换binary hash、接受任意新template或让revoked重新active。所有旧catalog条件/priortrust/challenge/CAS/epoch/permission保留；`.2`安装门成功仍不等SourceA。 |

**修订历史：**

| UTC | 原因/原卡与新卡/受影响引用 |
|---|---|
| 2026-10-15 22:05:45 UTC | 预发布发现不可变旧producer模板漂移；新增FIX-OG-OBSERVER-RECIPE-01，COVREL保持原12/12且追加incoming source前置。同步parent/COV/status的DAG、分组、追溯、测试、里程碑、风险和计数；无源码/模型/issuer实施授权提前授出。 |

新卡完整原子契约见[父计划FIX-OG-OBSERVER-RECIPE-01](plan-20260902.md#task-fix-og-observer-recipe-01-固定历史-producer-的-observer-recipe)。本镜像不为实现扩权；root共享old-only slot与R4分离，实际C/D待原发布验收。


### 原observer修复R1具名补齐（规范镜像）

必须逐字恢复signed6685 INDEPENDENT_VECTORS、明确改npr_15_policy旧catalog==default耦合为13600B/9c672独立gold及currentv2拒绝；LEGACY_V1_TEMPLATE不可跟默认漂移。精确修复observer错误Display及pin。不删回归、不改当前settings。所有5文档明确v1无目录隔离（shared多项目服务禁止；优先ordinary当前路径）、doctor/status为not-installed、agent enable覆盖旧v1而破坏legacy匹配；本release原`.2`ownedepoch0门只放精确旧observer、不调用enable、不运行Session/account/model或shared服务。R1FAIL保留，原raw6f2ad28f…；source/注册未实施。

只读6个具名persistent roots没有cd53策略行，两个历史reg仅revoked9c672；stable049源码仍9c672。此有限核验不是扫描所有用户repo；若开发版手注cd53行则保持拒绝、备份保留原库/registry，暂停该库native操作并另受审修复；不自动改/删/迁移或升权限。所有5文档披露，不假称未知原库已修复。新卡VER3的agent_native_origin_test明确feature=test-live-agent仅本机，四个显式复测历史输入LIBRA_NATIVE_TEST_PRODUCER/BUN/SOURCE/RECIPE；Session/live/model opt-ins unset，不加入CI。


### FIX-OG-OBSERVER-RECIPE-01 本机登记测试装配修订（2026-10-15 22:40:05 UTC；草案待唯一Claude审查）

原VER3要求清空producer输入却执行必须显式四输入的真实本机target，actual16FAIL/rawba65e33d…全部在required_path，保留失败不当PASS。测试本身还用普通agent enable安装v2，无法匹配固定legacyv1。保持原6AC/6VER、72/26分母与DAG，精确增加既有tests/agent_native_origin_test.rs为第11写路径（root10+site1）；3Rust touched/1runtime+2test-only，3落点/M，仍一条历史recipe兼容行为轴，不新增卡/target/CI/fixture文件。仅ownedtemp fixture从独立13600B/9c672模板、85977event-map和30000ms旧recipe渲染，保留原16测试，增加普通v2拒绝及alternatepath旧v1装配。VER3纠正为四个显式复测历史输入，其余live/model入口清空；本地producer只初始化独立测试key/登记挑战，绝不Session/server/account/model/网络或用户global操作。原key/descriptor只读，正式F原`.2`freshregistry epoch0门仍独立不替换。原FAIL与strict needless_range_loop FAIL保留；strict只修新加cfgtest迭代器。新路径须actualClaude范围/具体postimage PASS及root裁决后才写入root；当前仅private草案，不构成source-qualified/GO/SourceA。


### FIX-OG-OBSERVER-RECIPE-01 实际 A/B 与 final49 T1收据（2026-10-15 23:18:52 UTC）

此为事实推进，不新增任务卡/AC/VER/DAG/I/R。signed3158302为实际基线；scope补充actual soleClaude R2 PASS/raw931aa3e48f2325c429440994cc3a79d2fa493cff0f1fb2dc48b2c9d60a57994c及root裁决先采纳、后写harness。R1 FAIL/raw501778a…保留。现行root10+site1/3Rust touched（1runtime+2test-only）/3落点/M，原6AC6VER与72/26分母不变。

- signed6685两完整catalog rows、原13600B/SHA9c672 v1template与INDEPENDENT_VECTORS已精确恢复，catalog SHA97b5f404d9799166ea00a220bcbaff5ebc9296dc6c0b693d93ef132cbe711e5d；ordinary settings SHA ec4cd10f1b0628ec031be8526e15407d8e2b02b78d67564c02fdfea8de796c5c/mode原样，目录过滤默认未回退。registry生产与原tests字节保持，新增test clippy前滚后SHA dfecf5e931572922154f43f62128fa13ff47b3a23493f112d18387fdcb9db682；本机harness SHA644b67e59ac9745d997653ed8335e843f9a4070964161d57300effdca53fcf65。
- 新独立gold正例覆盖12组（.1/.2×2synthetic路径×3状态），否定20项；未修catalog baseline新positive actual1FAIL/rawe6fc8c2c…保留，不冒称baseline全部12组都执行。最终native unit37PASS/rawff8747334f44c49d11af215c77dbc675554d68bacf7c187731815454fb110b60。
- 原feature-gated本机16案例逐名保留，actual16PASS/0FAIL/0ignored/raw94f1133f8cb4eb9ef51dbc071938a64c50437c5b6a42832d7a74eb909260eb3a。四显式输入实际复核原.1测量producer、原Bun1.4.2、原publicrecipe与仅init用3个已审原source字节（晚期schema12526，非早期08744或.2schema8b）。9有限依赖按原锁5dbf从官方registry取得，SHA512 SRI一致、与两旧installed副本2474regular files逐字相等；新owned helper2503项before/after一致。这只恢复有限初始化helper，不重建完整producer编译闭包，原完整archive/manifest缺失如实保留。只owned测试新key/登记挑战，无Session/server/账号/模型/原key读取/全局变更/CI接入；原.2 issuer installed-F门仍待COVREL。
- 最初错误unset输入的16FAIL/rawba65e33d…及新增test迭代器strictFAIL/raw2b014a5f…保留；原VER3和owned fixture已reviewed前滚。最终16、37、109、fmt、strict均actual0，没有失败/skip转PASS。
- 本卡final49 T1：`Summary [1118.151s] 10219 tests run: 10219 passed (2 slow, 1 leaky), 23 skipped` / exit0；start 2026-10-15T22:56:49.089436+00:00 / end 2026-10-15T23:17:25.442851+00:00；raw d7c23044d3d2564702c8b9088c990ed50345776132289e2f2374fdc063664b21，2048输入前后相同。终态原事件计数：{"ABORT": 0, "EXECFAIL": 0, "FAIL": 0, "FLAKY": 0, "LEAK": 1, "LEAK-FAIL": 0, "RETRY": 0, "SLOW": 2, "TIMEOUT": 0}。skip不计PASS，不另发zeroLEAK门；旧修复前50 full10216PASS含2flaky仍只历史，不能代替本T1或后续最终bumpedT2。
- 五docs已同步v1无目录过滤/禁止共享多项目、doctor/status旧v1未安装及enable覆盖v2/非legacy修复、原.2freshregistry无Session兼容门及未许可cd53row保存backup/failclosed。网站1页candidate4122be…，sync/fixture/gen/docsunit9/typecheck/build、本机EN+zh可见main8概念HTTP200均actual0；网站单Claude PASS/raw13b5d3fa14283a0607a32587d19074566fd7f0931a133235f790acc3665c65fb，2Low由root按明确version-scoped/unreleased边界接受。正式cf Push/Cloudflare/公网HTTP未验收。

本卡当前in-progress/locally-accepted，只是A/B通过；最终源码Claude、signed+DCO main Push/freshremote/archive、cf交付与DEP-COV2-OBSERVER-RECIPE source handoff尚待。实际取得这些交付后才在root私有收据记source-qualified/父source62/72；下张COVREL I3再登记实际SHA，不预填futurecommit。AC4/5、VER4/5的source交付部分须真实后置收据补齐；AC6/VER6待COVREL exactF/C-D才done/complete。父formal59/72不变，COV25source/0formal；current3 sharedgrant/SRC029/RUN029/OG27、FAST04、SourceA、三真实字段及旧unsupported-stateimport020 FAIL/deferred边界不因此放行。


**DEFER-OG-EXPORT-TEST-LEAK-01（计划外遗留，不新增卡/门）：** 本次T1 Nextest对既有cfg(test) `opencode_export_failure_omits_stderr`报告1项LEAK，测试断言通过且整个进程exit0、0FAIL/0retry。此处用owned假shell exporter（固定stderr后exit3），不接入真实OpenCode/账号/Session；source与本卡基线逐字同一。真实资源遗留原因尚UNKNOWN，不能凭PASS推断无害，也不将告警重标为无告警。按用户复杂旁支问题延后政策保留原始full/raw及有限诊断，后续独立排查；本卡不修改exporter runner/Nextest profile/timeout，不新增zeroLEAK门，不影响SourceA/当前3/真实字段硬门。

## G09 — COV2-REL-01 源码版本标签同轴写集（本次候选，实际采纳前不生效）

本增补须唯一 Claude scope PASS、root 裁决、相关 reader/fmt/strict 及实际 signed+DCO main 采纳后生效；不借 FIX-OG-OBSERVER-RECIPE-01 的 CODE PASS 或 final49 T1 授予这些新字节。原修复10路径/49签推freeze及其回执保持原像，不回改。已交付 incoming 为 actual main `1896475b023c983e4fbb7fca956445c1cc88b950`（source-qualified；cf/Cloudflare/HTTP由独立原收据绑定）；它不是未来F或新50T2通过。父source62/72、formal59/72，COVsource25/26/formal0不因本scope再增加。

仅 COV2-REL-01 同一 release availability 轴将 I3→I11：原三plan之外精确增加 `COMPATIBILITY.md`；`README.md`；`README.zh-CN.md`；`docs/commands/agent.md`；`docs/commands/zh-CN/agent.md`；`docs/development/tracing/agent.md`；`docs/development/tracing/coverage-v2.md`；`docs/development/commands/agent.md`；R4 `Cargo.toml`/`install.sh`/`install.ps1`/toolchain `Cargo.lock`及原tag/manifest/assets不变。production behavior0、M、AC/VER数量仍12/12、parent72/COV26和25nonrelease继承C/D不变；仅AC1/AC2/VER1/VER2比较口径明确允许八doc已审exact status/anchor delta，其余判据及out-of-scope原字节保持，无新test/target/registry/CI写权。root是唯一main I/R writer；网站仍由 DEP-COV2-SITE 原owner在 `apps/tanstack-app/content/docs/commands/agent.en.md` 单页对应同步，不默示其它网站路径。

八文档最终postimage SHA/mode和唯一已审status/anchor patch须明确绑定F；runtime/tests/其它行为段原字节不变。八文档是首次新50T2真实输入，不能当I3记账跳过；后续纯上游patch复用仍须八doc最终SHA/mode同一，否则新full。八文档用相同statusface描述拟0.30.50源码线及兼容版本范围，不宣告稳定版已发布或安装已改变；实际可用性以official release及installed `libra --version`为准。若上游顺延patch，root先真实核最新stable并重验所有版本pin，不能机械冒用50。保留official49 ordinary-live历史、旧COV状态引文/失败、旧anchor兼容id、cd53开发行拒绝、未发布local-dev标志、SourceA/current3/native字段/opaque artifact独立门。

顺序：独立scope审/采纳→最终新bump及已采纳labels→fresh六manual各1/1、fresh classifier/预登记后唯一visible-only一次（≤600s）及相关quality→新50 T2双env full→final发布单Claude/root/signedF/currentSHA CI/四平台/manifest/Homebrew/站点/curl/installedF原2freshownedepoch0及C/D。旧50T2实际10216/2slow/2flaky/23skip和修复49T1实际10219/2slow/1leaky/23skip均保留，不挪作新50 PASS；unknown LEAK原因不改称已修。12+10审计仅七行为文件SHA/mode及原具名gate完全同一且root冻结点再核才可映射，原legacyFAIL/defer不转PASS。未来F/role receipt/new50终态均由真实收据后填，不预写。

**R1 FAIL及R2精确修正（2026-10-16 00:04:23 UTC，候选未采纳）：** 原scope R1 raw `c1ffeb83a136dd4b0baea7e07c9a4177a30fc105877400fd2799eed1dae6d900` 为FAIL，保留B1网站版本说明落入代码块、B2README误提升普通采集最低版本、B3漏developer命令页。R2以既有同步器纯函数从root候选重新生成网站单页；保留official49+OpenCode29普通采集基线和原普通采集标题/链接，仅三项新增行为及coverage-v2要求50；新增第八公开doc `docs/development/commands/agent.md`，因此I11/R4/root15+site1/prod0/M，12AC12VER与72/26不变。修订同步当前slot/summary/DEP及精确比较口径；其它行为/native门无变化。

**锚点与后继边界：** root Markdown内保留旧heading兼容HTML id；网站现有Markdown编译器不保留raw HTML id，仅承诺新标题生成的实际站点锚点及现有普通49标题不变，不伪称旧coverage-unreleased站点片段已获兼容。这个有限网站旧片段兼容缺口作为计划外低优先事项延后，不扩大本次单页写集到renderer、不开general raw HTML权限。未来OG24/27/28/29写入这些页时必须给新增能力独立标版本/未发布状态，不能借本coverage-v2源码说明授予native/SourceA；如上游占用50需要修改本受审doc字节，须重新审查具体doc delta/链接并按真实源码差异验证，不能冒称doc逐字等价。只R4纯版本且8doc/生产/测试其余字节不变的同实现顺延仍沿用户已授权复用规则。


### COV2-REL-01 I11/R4实际采纳、候选50写入（2026-10-16 00:18:45 UTC）

原scope R1 FAIL/rawc1ffeb83a136dd4b0baea7e07c9a4177a30fc105877400fd2799eed1dae6d900（网站frame/README基线/遗漏developer页）；R2 FAIL/raw0e56207bc65e0f7ba6bfed007d93111a669e619e4f097fdf9e4daae685c58baa（两处I10/七的标签）；R3仅修两标签后soleClaude PASS/raw3b875be5d4152b18e4617af54f3afd69cb82609de0e904446775e7c78e5b8a40、root独立11postimage SHA/mode/其余10root+site同一性核验及实际3plan QA109/fmtstrict0，source2048前后一致。实际signed+DCO scope main `d74f37aecea801b07959b40326f19ad3b8dad4bb`、Push0/freshremote/归档SHA dbee2b3a1acab7ea2880c8be9467b256f637c2ae06f8c71c551d71aadec492f0 才采纳I11/R4；此前未写root8doc/R4。原私有QA准备SyntaxError在任何测试前发生并保留，不能称testFAIL或PASS；R1/R2/R3不同candidate的实际QA分别绑定其输入。

修复卡source已actual189/cff290及站点交付；AC4/5、VER4/5实际源条件现勾选，仍in-progress/locally-accepted，AC6/VER6及C/D等待COVREL原installed-F .2freshowned epoch0与整套发布，非FAST04/current3/SourceA完成。原49T1 10219/2slow/1LEAK/23skip、0FAIL0retry0flaky不替代新50T2；LEAK causeUNKNOWN仍DEFER-OG-EXPORT-TEST-LEAK-01。原CODE review非阻断P2 registry旧Observer仍含enable误导，保留计划外DEFER-OG-LEGACY-REGISTRY-DIAGNOSTIC-01，原生产bytes冻结/失败关闭及五doc匹配历史配方指引不变。

本次首次实际bump为0.30.50：fresh lateststable49、tag50未存在，Cargo/install三faces同步；Cargo.lock由实际cargo metadata --offline --format-version 1生成，逐字仅root package libra49→50、其它packages完全同一。8doc精确受审postimage已落root，普通capture基线仍49+OpenCode29，仅相应三新增行为与coverage-v2要求50；不宣告stable50已发布。root sole I11/R4/0prod/M、12AC12VER/72与26分母及全部COVREL checkbox不变。六manual、一次fresh prereg genuinevisible、22审计冻结同一性、新50T2、final release Claude/F/CI/四平台/签名manifest/Homebrew/站点/指定curl安装/原.2epoch0仍UNRUN，不预填未知SHA或门PASS。


### COV2-REL-01 repaired50 本机预验收完成、真实观察事前登记（2026-10-16T00:27:14.785783+00:00）

actual d74scope采纳后首次repaired50六manual各1/1、version2、109readers、fmt/strict全部exit0，2048源前后相同；behavior2045 SHA `98cbaff7923fb7a78670b685c9c327b72df1ec08a502abf24b69a82081321d6e`。新classifier已actual编译，接下来严格一次Haiku4.5 visible-only本机观察≤600s已在coverage计划完整登记 `COVREL-REPAIRED-0.30.50-OBSERVATION-PREREG-98cbaff7923f` / readySHA `c0fbebdaddca011cdea8b6885ae5da267ee2e556c77201f5d5a860ce85ec862c`；登记时attempts0，尚无观察/full/F/发布/安装通过，不挪用旧50/49T1。父formal59/72=81.94%、source62/72=86.11%、COVsource25/26/formal0不变，SourceA/current3未放行。


### COV2-REL-01 repaired50 新真实可读观察已完成、T2待开跑（2026-10-16T00:28:00.630352+00:00）

本树事前登记SHA `c0fbebdaddca011cdea8b6885ae5da267ee2e556c77201f5d5a860ce85ec862c` 后actual一次Haiku4.5 run/export/classifier exit0/0/0，2.270449秒、1Session/1text非空、partial=false/1turn/coverage version1；六类state/proof候选计数全0，原export未改、无raw transcript落盘、临时HMAC key未保留、owned store实际回收。结果receiptSHA `58667b739bf7a55ad0e25e37c220ddf5a7ae6ab89e1c8743ab0b9b3075d6b886`。这是ordinary visible-only观察，不证明native来源、不授予SourceA/current3/归档。新修复行为本次预算1已用，不再自动重试，旧样本未复用。

七审计行为文件SHA/mode与原actual12+10及COV16/17原具名AC/VER完全同一，冻结点收据SHA `68964825d9c4a25f40607562d96dd2ea68a937d81cb83ba50f748965d69fda50`，按原VER10/11只映射原12/10真实审计，不声称新跑22；classic unsupported import020原FAIL/defer保留。唯一26新增case计数不扩大。full/F/发布Claude/currentCI/4平台/签名manifest/站点发布/安装/原.2epoch0及C/D仍待，正式59/72=81.94%、source62/72=86.11%、COVsource25/26/formal0不变。


### COV2-REL-01 repaired50 首次最终行为 T2 实际完成（2026-10-16T00:49:56.998427+00:00）

actual dual-env `cargo nextest run --locked --offline --all --no-fail-fast --retries 2` exit0；     Summary [1124.297s] 10219 tests run: 10219 passed (2 slow), 23 skipped。开始 `2026-10-16T00:28:20.887379+00:00`，结束 `2026-10-16T00:49:10.202178+00:00`；rawSHA256 `215dc8242692b4da3ea3eaca8c55f9c7ff8ec1248300c2def533e6ad378a555e`。2048源前后SHA/mode同一，behavior2045 manifestSHA `98cbaff7923fb7a78670b685c9c327b72df1ec08a502abf24b69a82081321d6e`（精确排除三plan纯事实；含8doc最终字节及R4）。原49T1、旧73550full均仅历史；本次是真实修复后首次50T2。skip不计PASS，不新增zeroLEAK或zeroFLAKY门，也不把warning/retry/LEAK原因写成已修或无害。


完整事件见coverage计划；最终发布单审/F及C-D待，formal59/72、source62/72、COV25/26/formal0不变，SourceA/current3未放行。


## G09 — COVREL R5 发布说明与 main 整合后的最终冻结（2026-10-16 02:13:58 UTC；独立 signed main 采纳后生效）

AC3–11/VER3–11 的原勾选只绑定旧 215dc824 树，本修订全部重开；六 manual 在新 bumped50 树重跑后、AC9–11 在 root 冻结点完成条件等价复核后，才能分别重新勾选。

最终 release CODE R1 实际 FAIL（raw `828b147df894a683c23489a75fe3e4f5a957d81bd93d2308074e55923df74583`）：缺少 CHANGELOG 0.30.50；R2 实际 FAIL（raw `828e643ab3dc46a1ba9f6d91d3d6c4e0407d51ed338dc2b08469e9e1de78c6ea`）：私有草案误把 VER4 改为 VER5、历史 R4 park 改为 R5。原失败保留，本次精确恢复这两个字面值。COV2-REL-01 仍同一 release 轴，I11/R5（R 加 `CHANGELOG.md` 已审 0.30.50 新顶节），root16/site1、prod0/M、12AC/12VER、父72/COV26、全部依赖和 Source A/current3 边界不变。先 sole Claude scope PASS、49 源码三 plan 相关门及 signed+DCO/PGP/main Push，才在共享 root 写 CHANGELOG；不新增卡、Issue 或免审发布。

当前基线是实际 signed main `f6ff2643cd20a19cf3b595dd2ea3957d60dcd08f`：已包含 f2080f3 的两份 README Quick Start 和 976091c 的三份计划文档交接。它们按 VER2 逐提交分类保留；traces 的 B8 FAIL/8次 STOP、formal0/26 及未来 C/D 不因乘载文档变成通过。README 的 Quick Start 是已审上游基线，发布文档增量仍仅原八文档的版本说明/锚点。旧 repaired50 T2 raw `215dc8242692b4da3ea3eaca8c55f9c7ff8ec1248300c2def533e6ad378a555e` 和新 main49 full raw `b1b511ddb02cd3773be0b30d3a54e5738f09de520aedee6dcb382f54b7c433b1` 都不是新组合发布树的 full。最终 bumped50 必须包含新 CHANGELOG、八文档和本三 plan，实际重跑双 env T2、六项本机 case、版本2/2、CH读者1/1、109读者、fmt/strict 及 runbook notes 本地检查。全量之后只允许精确三 plan 的实际事实收据更新，重新跑读者/fmt/strict、最终 Claude，并证明其余2045逐路径 SHA/mode 不变；其它字节变化重新 full，不启用旧四路径 notes-only full 复用例外。

已实际执行的一次 repaired50 genuine visible-only 观察仅在全部生产源码/测试/config/build、OpenCode/classifier 输入契约及 COV16/17 七个行为文件 SHA/mode 与原已审输入一致时，才可逐项映射到本次最终冻结；冻结其事前登记、原 exit/无内容 shape/HMAC 收据及明确的 v1 visible-only 范围，不声称新执行模型调用或真实 v2 typed-reasoning 端到端正例。任一不等价则 AC9/VER9 保持未完成，按原模型授权/预算重验，不以 synthetic 替代。六 fake/manual 在新树重跑，22审计继续按原 VER10/11 的七文件及原 AC/VER 同一性核验。CHANGELOG 旧历史尾/mode逐字保留，不增加 CLI 或 native 权限。原当前 SHA 安全/Windows CI、四平台、签名 manifest、Homebrew/cf/HTTP、指定 curl 安装、installed-F 原 `.2` fresh-owned epoch0 登记/撤销及 C/D 全部保留。两项新 main49 LEAK 原因 UNKNOWN，按用户既有策略登记计划外遗留，不称已修复或零异常。formal59/72=81.94%、source62/72=86.11%、COVsource25/26/formal0不变。

## COV2-REL-01：R5 已采纳后的最终 0.30.50 本机验收（2026-10-16 03:08:13 UTC）

R5 范围已 actual signed+DCO/PGP main `be05b4d32ed07cdc5bc8ade34afba8d02986b701` Push/freshremote，2048归档与49三plan QA同一；sole Claude范围R4 PASS/raw `917f339a2c56c7e72dfe4a3cf37a6875f3b4ae47f595a7463879c594da95effa`。旧release CODE R1/R2 FAIL及scope R3 FAIL不改写；该范围PASS不替代最终CODE。之后才写已审CHANGELOG新050顶节，旧49及以后历史尾/mode不变；保留f6ff的README Quick Start及scoped-traces三文档交接，后者的B8 FAIL/STOP/LTR C-D未放行。

新实际 bumped50 双env T2 exit0：`Summary [1134.569s] 10219 tests run: 10219 passed (2 slow), 23 skipped`，raw `f28ad74f0aef5bb220618d175c323786117fa7b7c444bf03afd76ea92ae2d27e`；2048输入前后完全相同，不能换成旧215dc824或main49结果。六新macOS manual各实际1/1、版本面2/2、CH1＋109读者、fmt/strict及runbook CH/notes本地检查已过，同一最终候选source逐路径SHA/mode绑定。原repaired50一次真实v1 visible-only观察仅按已采纳G09的完整prod/test/config/build/Cargo/OpenCode/classifier与七行为文件/原ACVER同一性映射；原ready文件SHA及原classifier源码/二进制SHA已actual复核；原12＋10审计复核同一，不称重新执行模型或22项，legacy classic失败/延期保留。真实v2模型/native/SourceA/current3仍未通过。故此处只重勾AC3–11/VER3–11，AC1/2/12与VER1/2/12仍待最终F/冻结及C-D。

本次full之后只更新精确三plan事实，其余2045输入（含CHANGELOG、八公开文档、R4、runtime/tests/config）SHA/mode必须与本次新T2完全一致；重新110读者/fmtstrict及最终单Claude CODE后才signed F/PGP/归档/Push。当前F/tag、当前F安全/Windows CI、四平台、manifest/Homebrew/cf/HTTP、指定curl安装、installed-F原`.2` fresh-owned epoch0登记/撤销与全部C-D均待；不预填未知F或PASS。新full slow/leak/retry等以原raw实际记录，不推断无害或修复；早先main49两项LEAK原因UNKNOWN（DEFER-HISTORY-REACHABILITY-TEST-LEAK-01、DEFER-IGNORE-ATTRIBUTES-TEST-LEAK-01）及原export-test遗留均计划外延期。formal59/72=81.94%、source62/72=86.11%、COVsource25/26/formal0不变。


## 0.30.50发布继承的本次收口

**按本次收口签名 main 交付生效：** COV2 26/26 `done/complete`，父 FIX-OG-OBSERVER-RECIPE-01 继承实际 C/D 后，父正式 **60/72=83.33%**、source **62/72=86.11%**。v0.30.50 immutable F `deabb37b70d74949a59c3722ad5cee1d8d79145a` 的最终full/CODE/当前CI/四平台/manifest/CDN/Homebrew/网站/指定curl安装/原`.2`epoch0→2均已有真实证据；DEP-OG-13=complete。当前`.3`/RUN029/OG27..29/05/FAST04/SourceA仍未放行；Linux功能未运行。

完整[逐门实证与逐卡C/D ledger](plan-20261013-coverage-schema.md#covrel050-actual-cd)绑定同F/release38849003385/稳定Release414541147及原`.2`epoch0→2，不替代SourceA/current3/FAST04。本三plan仍需本次reader/fmt/strict、单Claude与signed+DCO main Push/freshremote；交付后生效。原Linux功能后验/旧FAIL及延期边界不变，终止纯事实提交自身D组按GC-COV2-06/模板v2.14预登记。

SDK11 independent A/B、完整11body/22diff单审与root裁决、derived versioned expected794c、固定6291实际比较11/11和同native checker类型检查已实际通过；首轮隔离Node/父metadata失败保留，最终11字节/mode不变。此仅S6 SDK11子门；完整S1–S8/source/profile/native闭包、依赖来源、selected binary关系及SourceA仍UNPROVEN，不授共享grant或当前3catalog写权。详见同一实证章节。


## 例行状态同步：SRC029候选范围采纳（2026-10-16 05:41:51 UTC）

**SRC029 候选范围修订（本次唯一Claude规划PASS、root裁决及签名main交付后生效）：** 以现有R2未批准候选替换R1作为来源审查输入：selected68=42production/build/generated+23tests/fixtures+3README，声明source8597（8540regular+57symlink）、nonREADME8594；未批准任何binary/profile/native来源。R1的66/8595/8592与26d914、旧R2静态PASS及失败均保留历史。只准在新private successor修改三README的版本/范围/历史与已证基线说明；现有5个生产/build delta按固定SHA纳入待审输入，不能继续改TS。S1–S8全部门、1705native closure、149transform、ROOT19/网站未来lease、OG24原6AC8VER及RUN029真实模型门保留；S6仅已证SDK11子门PASS。父formal60/72=83.33%、source62/72=86.11%，COV26/26卡完成均不变，计划级末端D后补仍待；本变更不新增任务卡、不发版、不改全局OpenCode。

本次规划source前像main75d1734；具体判据和全部68 fixed R2 SHA/mode见[父计划范围登记](plan-20260902.md#native029-candidate68-scope)。R1→R2五个生产/build后像和两个新增test只读供审，当前新private successor只允许三README修版本/范围/历史；未知TS修复仍另作owner/轴修订。官方29原import-boundaries真实0/1FAIL和R2精确修正版真实1/1PASS均保留；前者不记候选失败、后者不记完整SOURCE PASS。SRC029总门UNPROVEN，S6独立SDK11子门actualPASS，不授当前3catalog/root19/cf共享lease；OG24仍6AC8VER，RouteB原依赖和真实native字段门不变。此纯规划无新增任务/进度/版本/网站命令delta；three-plan focused110/fmtstrict/单Claude/root/signPush在规划准备时均未跑；后续实测见末尾分阶段事实，不把准备时状态当最终结果。COV终止75d SHA的D组另按原预登记收实际结论，运行中的CI不记通过；不为本变更重启COV终止链。


**SRC029候选范围修订的实际规划QA前像（2026-10-16 05:43:55 UTC）：** 新CH1、architecture64、docs32、examples1、hook12共110/110、0ignored/fail，nightly fmt与strict all-targets/all-features `-D warnings`均exit0；docs raw SHA `3418ba9710f5b3b8f09a37de5c98a1fd9122c23208c1c3bb6835a4be1a1aaaa0`、strict raw SHA `0ef1bf4ecfb80b0dd982dcf9e3a410084b2ca2f1b0a8f30c44d6637ab5bc45ff`，2048受测输入前后同一。此结果只绑定加入本事实前的三计划规划前像，其2045非规划输入仍与signed75d完全同一；新增事实后最终reader/fmt/strict另验，不预授最后字节。未跑full、producer全量/check/构建、账号/模型/Session；原official boundary FAIL与R2窄修正版1/1事实保留，SOURCE S1–S8整体未获PASS。唯一Claude及signed main尚未取得，本修订规范仍不生效。

## SRC029 下一执行项（2026-10-16）

先完成[两路径G09](plan-20260902.md#native029-explicit-completion-scope)相关QA、唯一Claude及signed main采纳，再新private successor修复Anthropic未闭合reasoning撤销与回归；之后继续S1–S8整体来源门。不是当前3或native批准。上一轮75d cancelled/f296实际全部CI成功已按顶部补记；不重开已完成COV家族，不改版本、网站命令或全局OpenCode。正式60/72=83.33%、source62/72=86.11%，下张执行卡仍OG-UPGRADE-029-01的SRC029来源子门。


<a id="native-attest-launch051-localacceptance"></a>
### native attest 0.30.51 本机验收事实（签名源码/发布仍待）

本卡工作树后像 `src/internal/ai/observed_agents/native_origin/attest.rs` 全文件SHA-256 `0be80d2ca58653f7a693034eb791438b14ccd54291db6f83f507aabf5583dc0e`、0644，唯一生产prefix自Claude R1起字节不变；R2仅补同文件tests与取消措辞，R3补实际Cargo.lock/父计划hunk（Libra LargeFile占位不当作已读）。三轮actual完整Read分别29/16/5，root逐行恢复工具返回，未凭报告计数笔误或占位授GO。R1 raw `04997d05f8e278437ec46d8732de447babe9e962333c883fc0861d1becf5e7d6`、R2 raw `dee29fa6b9011037556b20f41c8d3f248cf08b318cb0ab8996eac46e0d5dd39f`、R3 raw `e7103750a4529c47108dab36559ac6368725bed4030a34a9aec071ea232d9447`；全部PASS只覆盖实现/文档/patch意图，不是SOURCE/native/发布通过。

新VER1/2/3及旧4个NPR09在定向和完整full均首试PASS。native-origin40、架构64+文档33+Hook12+版本2、fmt与strict全部0。双env `cargo nextest run --all --no-fail-fast --retries 2` 实际exit0、wall1867.40s、Nextest868.421s；10223/10223通过，13slow、1flaky、1leaky、23skip，原raw SHA `67ac0a3ab4cc60c872dc755aa4764d90a407aff48a79e85cc96eb74f6a19fd1c`。原40/109/fmt首轮与被主动中止的旧Clippy -2、最初非Send编译FAIL均保留，不倒记为最终PASS。最终严格Clippy275.33s/exit0，raw `0da2d4e9a61b2484bfcef7aa3c7cbdee917f723567b3b8d2ff7f9dfc577453e1`；附加生产lib/allfeatures lint93.53s/exit0，关闭先前let-and-return疑问。不是Linux功能验收。

**计划外既有遗留追加，不新增卡/不改73分母：** `DEFER-OG-TEST01` 追加同一mirror fetch `test_fetch_into_mirror_updates_and_prunes_all_refs` 的TRY1 FAIL4.501s、TRY2 FAIL18.437s、TRY3 PASS38.616s / FLAKY3/3（此前2026-10-10及10-14已有同名观察），根因仍UNKNOWN/UNFIXED。`DEFER-VALIDATION-NEXTEST-LEAK-01` 追加 `agent_push_seed_readiness_accepts_missing_or_empty_marker_dir` 的LEAK0.215s；测试本体通过、进程/stdio残留归因未定，未修复，不记零leak。按用户延后同类难题的指令仅留实证，不扩大本卡写集、不移除原测试或改变full标准。

网站cf `8c6e8f1ee22aaafeb54df14f635c5e11199d13ee` 为本卡单页signed+DCO提交，已核committed page SHA `c5f82020de058867d593a4bfdd41f45b0b36ca300453f9b32441cd6a13aef6f9`=最终sync/check/typecheck/build/11docs测试输入，双语本地预览200含新措辞。当前仅local commit，Push/Cloudflare/线上新文案未发生；线上old基线使用既有acceptance UA双语200、尚无新句。首次裸urllib403保留，不当发布失败或PASS；普通浏览器也已确认old文档正常可见，无CAPTCHA或安全绕过。

只在本卡实际signed main交付并由root验证commit内上述post-image/mode后，SRC029才可消费此只读新像；具体F SHA由实际交付receipt实录，不能猜测。OG24自己的attest仍只读；修复全部shared I/独立release/C-D/三plan收口/postSHA登记及signed-return完毕前，不授OG24.3 grant。真实selected9f消费新TMP、registered lane every-launch、完整S1–S8与Source A仍阻断；合成helper/本次full不代替真实native字段或注册。主/站Push、当前F安全CI、签名tag、四平台、稳定release、manifest/CDN/Homebrew、指定curl安装均须actual完成后才卡done；正式60/73/source62/73/COV26/26不变。


## 2026-10-17 native boot 实测状态同步

`FIX-OG-NATIVE-BOOT-01` 为 blocked / empty、未 source-qualified：VER1/3/4/5/6/7/8实际PASS，AC2的初始 exact15要求已有最终同一f9候选的16/17项真实反例；完整VER2 harness仍UNRUN。按用户既有复杂问题延期策略登记计划外 `DEFER-OG-NATIVE-INITIAL-ENV-01`，不是豁免AC2或将该卡标完成。诊断单Claude PASS/raw `bbf77e0f5eca2f30ffa0ff9702f23355dba504d56aa1d8efdd159cdc231905c6`，root无损恢复23chunks/15正文；4次隔离version启动、0模型/账号/attest/真实用户Session调用、4个owned进程组实际已退出。root结果摘要SHA `d99449ad3e325db3a3c946fee4a908d1e8b9c7931ddf98b891613d92d2ecff44`。VER8新建model-free空测试Session的599B导出仅为Required ordinary兼容正例，绝非native字段/SourceA。

逐门范围、具名失败/复用与计划外遗留（完整历史见证据capsule）见[父计划实际验收](plan-20260902.md#native-boot-macos-实际验收与初始环境遗留2026-10-17)。最终候选SHA `f9ab85e955f99f8a3027e4f7cd7233087b762db4f828a403b537c65f9422d06d`，当前manifest `268a79ac70a3f6f93ddf37b5a5486199a503c757a35f1ec35fbbf5050c9d4e5d`。原VER3/4/7收据与历史manifest身份保留，不改称当前新运行。BOOT验收没有修改原始key、已安装OpenCode与Libra51、用户04及05dirty或原8个stash；本卡本身无agent公开CLI变更，其agent网站页租约仍扣留。本次独立校正上游checkout/commit命令文档，不含新native能力。没有新main功能/full/CI/release结果可预填。

父正式61/74=82.43%、source63/74=85.14%、COV26/26不变；原AC/VER、DAG、写集、REL-FAST-OG-04成员及实际C/D要求不变。SRC029/OG24.3/RUN029/OG27等native依赖仍不能放行，ROOT19/cf lease withheld；按既有依赖继续检查可独立执行的后续卡，不能因延期将native归档当完成。防重启证据capsule `native-boot9dc-seven-of-eight-PASS-20261017T125553Z`，3090files，MANIFEST SHA `7e0f7fbb70a214e0667307f14beba7df9b0ee79ef1b24e6df0505dad1029d9ea`（位于当前对话task-handoffs）；不包含原issuer key/测试认证配置或live runtime，不宣称全dependency离线资格。


### 剩余任务开工核对与上游基线（2026-10-17）

未完成的13卡按原依赖逐一核对：`FIX-OG-NATIVE-BOOT-01`、OG24/27/28/29/05/08/06、FAST-OG-04共9卡存在实际失败或未满足前置；OG25/26两卡已有source Deliverables、仍待FAST-OG-04实际C/D；OG15与FIX-OG-CP-01两卡为此前用户明确延期。当前没有可以独立完成的未交付卡，不能靠改依赖、登记未审producer或将延期记done来增加百分比。新初始环境遗留在计划外，原74卡分母和61个正式完成分子均保留，计划整体仍未完成。

另已通过Libra原样快进上游 main `56e8a7ff19d42376f49760e6c992377d89f917b7`（checkout/status/commit修复，9路径），不是本卡新增native实现。2051项上游archive核对表明 `opencode_export.rs` 与 `native_origin/attest.rs` 字节均未变，selected f9与其manifest也未变；VER8实际renderer仍是当时9dc源码的具名证据，不改称56e的新执行。本次三plan状态提交自身不修改生产代码，Root19/OG24的agent文档cf lease仍 withheld；上游checkout/commit网站文档按用户常驻要求另作同步。原有5份dirty文档内容、2个untracked及8个stash均保留；合并wrapper继承077 umask造成的14处普通文件权限变化已按备份/上游archive逐项恢复0644，未改任何issuer key。旧9dc的111reader/fmt/strict/diff实际PASS保留；因56e含实际源码变化，须对最终工作树重新验证，不能把旧full当作当前full或计划收口通过。

## USER-OG-FINAL-C-20261017 最终范围同步

用户取消native启动问题的延期，将普通采集/安全partial确定为最终交付。六卡撤销、DEP/DAG/发布/写权/统计和完成定义以[父计划现行契约](plan-20260902.md#ordinary-final-c-20261017)为准；本表旧dated FAIL/UNRUN/61of74不倒改，三字段native未实现/不归档。现行剩余5张可交付卡按OG08→OG06→FAST04执行，OG25/26继承实际C/D；原两延期卡与其它DEFER保持原决定。没有卡因本次计划修订新增done，没有native注册/密钥/模型/Session或版本操作。

### 改范围前历史（原文保留）

以下两行是范围修改前状态表原文，保留其中全部日期、FAIL/UNRUN/验收收据；不是当前执行指针或native交付授权。

<details>
<summary>原plan-20260902一览行与DEP-OG-13行</summary>

```text
| [`plan-20260902.md`](plan-20260902.md) | B（OpenCode capture / native） | **实施中：61/73=83.56%**（本次收口签推生效） | FIX-OG-NATIVE-LAUNCH-01 已随 v0.30.51 实际 C/D 完成，source63/73=86.30%，COV26/26、DEP13=complete；source F440/cf8c6、release38951286768/8任务成功、指定curl安装及原`.2`epoch0→2均有实证。当前3/SourceA/FAST04仍未放行；下一SRC029→OG24.3。 **收口前单元历史：** 基础采集/字段审计与可信producer/verifier家族已发布；typed载体开发中 **当前：parent72/formal59/source62，COV26/source25/formal0；repair189/cff290已source-qualified，repaired50 T2 actual10219PASS/2slow23skip/0FAIL0retry0flaky0LEAK；release CODE R1/R2 FAIL；R5（CHANGELOG顶节＋f6ff基线）范围修订待审/采纳；旧215dc824 full不复用，需新bumped50 full，不授予发布C-D。以下原单元为历史快照：** **当前父71卡；source61/71=85.92%、formal59/71=83.10%；USER-TEMP-REL-049四卡实际C/D已完成，收据SHA256 `dad01d952f7ce629354118264a4926b048693bf851258e4cd14beb9853727f8c`。COVsource15/26=57.69%（COV2-08已actual c60b281 source交接）/formal0/26；Source A UNPROVEN、历史import020 FAIL/default reject/deferred，COV2-REL-01与DEP13仍独立待验。** 历史OG-26交接口径：OG-26 本机 A/B 与单 Claude 已通过、signed+DCO main `734b6f03884d3aa38400acbbfd446b557e642d94` 已交接：55/70=78.57% formal（OG-24/25/26 本机源码门 locally-accepted，继承 FAST-OG-04 C/D 未完成；三个子卡的本机进度为 58/70=82.86%，不替代正式分子）。OG-24..29 是 FAST-OG-04 的 strategy child：即使逐卡通过本机门并推 signed main，在唯一家族发布 C/D 未实际完成前仅记 locally-accepted，正式分子仍55；以下35/61、46/61等为历史时点。历史：正式35/61=57.38%；FAST-OG-01及11基础member、NPR20 v0.30.45及18新family member实际C/D complete；NPRBOOT额外child另记，NPR14随FAST-OG-02正式收口。RG六卡及四scoped final local/source已验收并signed main push a985e20；0.30.46冻结发布source f03；修正后macOS安全CI实际13门及Windows4门PASS，wholeCI attempt2 offline-core测试exit100，具体失败日志暂不可得，发布验收仍pending；旧0测试结果不计PASS。按USER-NO-WAIT-RELEASE-20261012继续OG-16，不等待Release；NPR14已发布机制8CLI/7verifier原证据复核可消费。OG-16原60AC/24VER实际macOS PASS、最终R2 canonical full9838PASS、Claude48＋34PASS，source locally-accepted，FAST-OG-02 C/D待收口，不计formal complete；OG19 in-progress：R19 local source/doc review已PASS并Push main 943a996；CI run 38253223360 的Windows scoped-input因Unix-only test items在Windows编译告警失败，现已加平台cfg修复，macOS reasoning 75/75、fmt及strict clippy通过，Claude Code R2单审PASS（0P0/0P1/0P2）；compat_agent_architecture_guard 35/35与opencode_hook_contract_test 10/10通过，新source Windows CI待完成。SourceA UNPROVEN，Linux用户后验，既有遗留不计PASS。; 2026-10-12 current correction: FAST-RG-01 v0.30.46 C/D complete; exact main workflow 38256073051 and release workflow 38262194805 succeeded; final OG19 full is 9880 (the preceding 9879 run belongs to the prior source tree), workflow audit found no CI test requiring a local OpenCode install/account/real model response, so no matching test was present to remove; the deterministic fake-exporter/bwrap safety suite remains. The current-source OG20 audit found unsupported checked AC24–AC27 without mapped VER evidence and restored them to pending. OG19 remains locally-accepted pending FAST-OG-02; next OG20; progress 46/61=75.41%.; 2026-10-13 current correction: OG-17/OG-21/OG-22 inherited v0.30.48 actual C/D are done/complete; final macOS 10002/10002, exact-head main CI 8+2+2, signed four-platform release 8/8, public GitHub Release, manifest/carriers/Homebrew, required local curl install and live cf docs all verified. Pre-amendment formal progress 55/61=90.16%; six-card OG-05 amendment is this change; after adoption the current denominator is 67 and next card is OG-24. Source A/native GO remains false, Linux functional acceptance is user-later.; 2026-10-13 OG-24 main-bound 14 路径 postimage 核对、VER1–7 各1/1、VER8 10/10、native-origin 34/34、docs 30/30、architecture 54/54、fmt/strict Clippy/diff exit0、Claude Code 单审 PASS（P0/P1/P2=0；P3=2）均已实测；源证据见计划 OG-24 卡。网站 cf 已独立签名交接 `597b86cf`、typecheck/build/docs10/10/Claude PASS、线上 HTTP 200 内容PASS；但随后另一部署成为当前版本，其完整 bundle 对本卡源码同一性 UNPROVEN，FAST-OG-04 最终需重核。DEP-OG-12 binary-to-source、生产 issuer、Source A 与 FAST-OG-04 C/D 未闭合；本卡正式分子不变。 ；本次G09三卡顺序COV2-25→FIX-OG-NATIVE-IMPORT-01→OG-IDLE-ERROR-029-01→OG-UPGRADE-029-01→COV2-22，新增卡A/B/C-D均UNRUN；REL-OG-TEMP-049操作本身不增卡。 |

| DEP-OG-13 | 跨计划前置（incoming：`plan-20261013-coverage-schema.md` → plan-20260902 OG-27） | 23 implementation/2 audit/唯一 COV2-REL-01（现行26卡；历史25卡与此前23卡收据保留）；此前23卡 G-09 三文件协调已最终字节单Claude PASS、签名DCO main `eaa43ec13f6c50212f0b3c2fa8a51799ec74bff5` 交付。COV2-17 完成当前22个审计名（21继承+1当前格式替代；另1legacy终态FAIL计划外延期，结合5新增去重required unique26）的macOS非skip验收及23张源卡最终A/B、单Claude、逐卡signed main才可 source-ready；release安全CI、四平台、网站、Homebrew、manifest、指定curl安装与继承C/D真绿后complete；OG-27另需DEP-OG-12真实Source A/native signature。 | **complete（本次签推生效）**：COV26/26继承v0.30.50 F `deabb37`实际C/D，release38849003385全部8job、原.2登记/撤权及签名安装已过；OG27仍需DEP12。 **收口前证据历史：**  **实施中/blocked**：COV2修订口径COV2-24 在历史25卡口径 source-ready 12/25=48.00%、formal 0/25；本修订现行26卡口径 source-ready 12/26=46.15%、formal 0/26，历史 COV2-03 源码/文档 signed main `d717e42e0d1ec75e6ae576b21242fd1519040abd`；此前 COV2-14 signed main `4558af64c09e63c4716fa68595776a8044f66cc7`；COV2-03 五项 VER、五项旧 v1 ER-04 与两项缺表回归当前各 1/1，full-r3 失败已修但不计 PASS；最终双 env full **Summary [1618.814s] 10078 tests run: 10078 passed (14 slow), 23 skipped**/raw SHA `c49dd3c33652d55541a0e4c0269c817460b9109cf261eab5271d90760a49e8a9`、exit0，唯一最终 Claude Code PASS、P0/P1/P2=0、P3=5/raw SHA `9ef25f92615eefcf2019212a306cfa7542ef416b693286bb99111006b3ea6df7`，signed+DCO 源码/文档 main `d717e42e0d1ec75e6ae576b21242fd1519040abd` 已 Push、remote 同 SHA；COV2-19 本机 A/B 与双 env full 10084/10084 exit0、唯一 Claude PASS/raw `64068902908f4c521e380dc7e5cf593134eaa99697de208344b65f75b3c3caa8`，signed+DCO main `354ff768c406bdad74607da62002bff810147c24` 已 Push、remote 同 SHA；COV2-20 本机 A/B、唯一 Claude PASS、signed+DCO main `aa0ea018ddd044f09d62a88428e812a359cef8ed` 已交接，Windows job `115625055658` success、旧 CI 因新提交取消；COV2-21 本机 A/B、唯一 Claude PASS、signed+DCO main `6b24d797583afe1c0265f6e9fcbd364566671167` 已交接；COV2-23 唯一 Claude PASS、signed+DCO main `d977d13c5e23111b93d78477b9e1a70015a111b8` 已交接；COV2-04 五项 focused 各1/1、fmt/strict、唯一 Claude PASS，signed+DCO main `03b10f887368a3db5206589f5dd6947432ffe26c` 已 Push 且远端同 SHA；COV2-24 三项 VER 与七项继承各1/1，T-5 10103/10103 exit0/raw `2908fff258247c857ae857a4759fcde9b925c212cc5912fa373ece16b5373029`、唯一 Claude PASS/raw `a6d467bcdc33c42fff3c45f55681ebbaefa213d96cfd59a8b901c5d3dff942f7`、signed main `c56de683acf71d7327436eef7ef9185d97dcff27` 已交接；此前23卡 G-09 r5 已 PASS 并签 main；OG-26 signed main已交。旧0.30.48 LBR-AGENT-005拒绝 `.2` 仅限定旧安装时点；stable patch后重验原issuer epoch0。OG-27..29/05与FAST-OG-04依旧NO-GO。  |
```

</details>

**上游兼容修订实际 Claude 复审（2026-10-18）：** incoming be158f57 的 count-objects 两项 P2 已修订；R2 另发现按 R1 错误建议引入的孤立 sidecar 告警回归，已按固定 Git 源码恢复并补 JSON/stderr 回归。R3 sole Claude Code actual PASS，raw SHA256 `f147b148e57600220b4f1defd59c1e6a7da9bbdf45651d19a9297daed3d5f279`，80/80 完整 Read 可无损恢复 17 输入体；父级仅核实际报告/摘要/读完整性，不是第二评审。原 FAIL 不改写为 PASS。仅六路径独立 forward-fix 与既有九网站文档同步；N+1/preview guard 重设计/不可读目录 fixture 等 P3 延后，当前 QA、全量、CI、发布与安装必须另实际通过，macOS 计数仍 61/66。Source A/native 三字段及 Linux 功能不记通过。


<a id="ordinary052-actual-cd"></a>
## 最终方案 C 实际 v0.30.52 收口记录

实际 release source F `bc77500b8814c2f9f37ae29ba82ff43eff68055d`；signed tag v0.30.52；release run `39078049943` 八任务成功，GitHub stable Release `416134191`。public Ed25519 manifest、四平台资产摘要/大小、真实同run artifact与Homebrew marker、网站cf `fa33128754d8d881ad54cea495ed5f231f561785`/部署双语200、指定 HTTPS curl安装exit0和安装后version/upgrade-check/marker均已核。darwin-arm64安装SHA256 `f4858c3f83fd69d77bcc854f9a6ff2707359f95e673556c2eb5e89cc1e811986`，81572016 bytes。未修改原OpenCode issuer key、全局OpenCode、registry/trust或原用户04/05改动、八stash、untracked文件。

OG-08 → OG-06 → OG-25 → OG-26 → FAST-OG-04 的正式完成累计为62/66（93.94%）、63/66（95.45%）、64/66（96.97%）、65/66（98.48%）、66/66（100%）。库存66/68（97.06%）保留两原延期；六撤销不是完成。旧native启动FAIL/UNRUN、Source A未证、Linux功能UNRUN及其他既有遗留保持原状态。

Current-source complete macOS R2 actually ran10262of10262PASS(2slow,1flaky),23skipped; raw36497a38ea40edb2060f35ec9d448d3809a4f074a28c35793cee5b7b62b68606. The fetch mirror Q3case first failed withLBR-INTERNAL-001 working-copy scan/statusIOworker(1.731s), then configuredTRY2PASS(11.375s). Its executor/protocol/test bytes equal published051; cause unproven and not claimed fixed, retained as preexisting unresolved residual outside this C plan. No assertion/selection/timeout/source change or waived failure is used to make it pass.

Actual upstream registry failure: CI39066223720/job117494361573 failed serial_registry_matches_the_classifier three attempts for9missing lane:cwd rows. The superseded macOS full was controlledSIGINT at7450of10262passed(1slow),23skipped,2812notrun; exit100/raw d972a1313388f5786af416db39afe40aa38ecb5463cd1f920c521db5bed07d8d, NOTfullPASS. Independent metadata owner added only9rows in tests/SERIAL_REGISTRY.tsv; generated nextest bytes and all test selection/assertions/timeouts/annotations/classifier unchanged. Actual26of26focused/fmtstrict and soleClaudePASS d44d7abb8fadc55b30ef866c748d6c071bb97877df6454fcf194a5128810035b(12losslesschunks/11bodies) preceded a new complete current-source full, whose actual receipt is separate. This is not a new C card or point business write; final64paths/macOS66scope unchanged.

首轮bumped全量的配置路径5秒断言三次失败与SSH测试LEAK原据保留；该配置测试未改源码独立1/1通过，后续旧源码完整全量仅降低测试并发到4；上游be158f57实际源码与count-objects修订已另外重跑当前源码全量，不放宽断言或选择/跳过用例。原因尚未证明，不把原失败改PASS。完整结果、重试/slow/LEAK/skip按实际收据分别登记。最终计划收口全量已实际执行通过，精确结果见本次末端收据；此前待执行措辞保留原时点意义。


### 最终状态协调写集（仅发布后事实）

本轮产品/member I 与 immutable v0.30.52/F 不回写；发行时I7保留历史，发布后的状态协调I按本G09仅扩为I8，新增plan-long状态索引。本次按用户已授权的自动完成/推荐方案补齐四个状态文档：本计划、plan-status.md、plan-20261013-coverage-schema.md、plan-long.md；最后一项只把过时“所有卡pending”的索引改为实际macOS范围完成及两原延期。没有业务、测试、CLI、schema或网站内容写权扩展，不新增任务卡/完成分子。状态提交须本机fmt/strict、最终完整双环境全量与实际读者守卫、Claude Code单审PASS、signed+DCO main Push；其自身Actions按模板既有下一次常规同步规则，不预记成功或制造纯记账无限链。



**Final whole-plan test receipts (2026-10-18T00:34:08.737337+00:00):** actual macOS full nextest exit0: `Summary [1787.572s] 10262 tests run: 10262 passed (2 slow), 23 skipped`; raw SHA256 `25691bc3823476f278ce0b4fa06dc6098eee7a1708518de72931446b7d80c72b`. Doc-tests actual exit0:12 passed,3 ignored,0failed; stdout SHA256 `4f70cf7d926efa7b5eb156da63f4529456426560f801cf36f974fcd29e431089`. All2057 tracked physical input SHA/modes remained unchanged during those tests. User04/05 dirty vs signed-tree projection qualification remains explicit.

The original combined final-quality-r1 receipt remains pass=false: its last diff check exited2 for extra EOF blank lines in only plan-status.md and plan-20261013-coverage-schema.md, while fmt/strict/full/doc-tests were actual exit0. This original failure is preserved, never overwritten. Only the four owned status documents now receive factual receipts/completion wording and EOF whitespace cleanup; runtime/tests/config/version/website bytes remain unchanged. Focused five-reader guards, fmt/strict/diff, sole Claude review and signed main delivery must independently close this status-only delta; their actual results are recorded separately. This follows layered document verification, without claiming a full rerun after a whitespace/factual-only change or claiming original diff success.

Final macOS card delivery66of66=100percent; inventory66of68=97.06percent with2original defers unchanged. Six native-only withdrawals are NOTPASS; SourceA unproven/three fields unarchived and Linux functional UNRUN remain unchanged. Current new main status commit CI is not precredited.

## 2026-10-19：CX-00 main 开工与 probe 合并

用户要求在 main 执行 plan-20260904。基线3f0aa12，原两个未跟踪文件保留；原 probe 全文与摘要并入[主计划](plan-20260904.md#cx00-probe-20261010)，删除独立文件并修正 plan-long 索引。CX-00 固定官方 rust-v0.162.1 commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d，完成12-event/rollout/token/turn/source定位和 offline-only index 合同。隔离系统 hooks/list 三个 currentHash 与独立复算一致、trusted；不等于真实hook触发，旧 trust 失败原因仍未证明。系统模型配置summary未设置，固定catalog默认none；隔离summary=auto、effort=medium的strict-config载入ok，doctor整体因未设auth为fail。未启动模型、未复制auth、未改全局配置、未扫描用户历史，无生产变更。CX-00 locally-accepted；Claude R1 FAIL 的持久化矩阵遗漏已由R2 PASS关闭；提交/推送/承载C-D待验；其余28张CX未实现，后继CX-12。

**CX-00 审计复审通过：** Claude 2.1.300 R1 FAIL/66turn/raw c5dfe296b92a00df5bbdaa54b119b1a348ad28829c7a866b3c547cbb0ffea09c；补全持久化过滤及 turn_aborted/rollback/settings/goal 映射后，R2 PASS/39turn/raw fbdb825b02d14508a90d7486aa3efdcd557851fd91f91183912ffbb12e2d9d3a，0P0/P1。26项source blob与官方固定commit完整tree及SHA256实核一致，fmt/严格clippy/diff/35本地链接/原文SHA还原通过。当前唯一下一卡CX-12；CX-00 source-qualified、in-progress/locally-accepted，正式complete仍0/59，承载CX-15 C/D与真实模型采集未验。

**CX-00已signed main交接，CX-12开工：** a6a62f03b1a193ef147714aa4eb996f164641b7f push成功（base39203590342/CodeQL39203590295首次查询未完，不填PASS）。CX-12只新增固定0.162.1合成语料及MANIFEST末尾段：46行/12433 bytes，44严格JSON+明确重复key与截断尾两负例；旧8725字节前缀摘要相同。实际模型和parser实现仍未验，focused/单审待验；当前CX-12，后继CX-15。

**CX-12本地focused通过：** source-pinned46行case语料及人工字段矩阵完成，strict JSON44行+重复key/坏尾两个预期故障均核实；OpenCode hook contract实际12/12 exit0、零ignored/filtered，冻结前缀守卫通过。尚无Codex parser/实际模型获取结论，Claude本卡审查及交付待验。

**CX-12最终source-qualified：** legacy无ordinal修正后同一最终fixture bb1453259ac10a91c9bfd2529eb4b64ab1c963c70bf8e67fb5941d4d65b1fe69/12433bytes，focused12/12、fmt/严格clippy再跑exit0；Claude R1 PASS/51turn/raw42b15b9c4a224ba9667811e0fe9d0fe0c9b9a05a3d73912f552d262baa4af6af，无P0/P1。采用文档澄清，缺失的兼容变体和字节属性核验具名交CX-15/04，未伪称已有。当前唯一下一卡CX-15；正式complete仍0/59，真实Codex模型/数据及承载C-D未验。

**CX-00提交CI后续实查：** 同一a6a62f0的base39203590342中fmt/clippy/Windows scoped/Linux export/redundancy已success，macOS owner仍in-progress，offline-core/network-remotes已cancelled；CodeQL39203590295为cancelled。未将取消或未结束记PASS，C/D仍未关闭；本地源矩阵与合成语料source-qualified不冒充远端全绿。


**2026-10-19 10:45:31 UTC Grok架构修复授权：** 用户已暂停Grok功能采集，授权先更新main与架构/文档，随后交其他agent继续。当前fetch+ff至e270bfe/0.30.65，CX-15已备份三方恢复（保留新Claude附加字段和旧Codex retention），不提交/推送/发布。新增GK-06 in-progress/Acceptance空；GK-07纯字节normalizer planned，须GK-06本机门通过后由受派agent串行开工；GK-08生产接入仍暂停，未选卡。详见plan-20261018-grok-capture末尾ADR-GK-05，原GK-04/05历史勾选不代表生产thinking落库已验收。Codex测试继续仅本机、禁止加入CI。


**2026-10-19 11:11:43 UTC GK-06最终本机门：** 完整nextest exit0，10325 PASS（13 slow、1 leaky）、23 skipped，846.440s；流SHA-256 `6c7e6d88ac2ecb930e7fead8700575eea0340a5d24ffdfcec6416b7d98db3d18`。2008非计划路径前后冻结MATCH，默认JUnit新增Codex手动case0。唯一未改fetch测试LEAK诊断保留；不是零泄漏声明。GK-06 locally-accepted/Acceptance空；125focused、35Codex本机+矩阵、网站本地门与只读审查已通过。GK-07交接`/root/grok_continue`，GK-08生产采集与真实模型继续暂停；后继源码变化必须独立验证。CX-15仍未交付，既有备份/stash保留；无提交、推送、发布或部署。


**2026-10-19 11:12:48 UTC GK-07交接已发送：** `/root/grok_continue` 已收到正式实现任务并运行中，GK-07 in-progress/Acceptance空，仅两份纯字节reader与追加合成测试；GK-06本机收尾复验fmt/strict Clippy/diff实际exit0。GK-08生产Thinking接入仍暂停，无真实模型运行或任何交付操作。


**2026-10-19 11:34:48 UTC Grok提交/开发分工：** GK-07 reader本机31/31+architecture65/65、fmt/strict Clippy/diff exit0，独立review PASS（无P0/P1/P2），locally-accepted/Acceptance空；GK-08仍暂停。用户授权单独本地提交Grok部分，后续Grok由其他agent，本会话仅继续Codex；Codex WIP保留未提交，Codex测试不入CI，不推送/发布。


**2026-10-19 11:41:37 UTC Grok-only提交快照隔离验证：** 使用HEAD加所选index内容的临时源码快照（不含codex_reasoning.rs、Codex本机target、admission转发或Claude oracle覆盖）验证：Grok31/31、kind1/1、docs34/34、capability5/5实际PASS；nightly fmt和严格all-target/all-feature Clippy exit0。架构target初次63/65：Grok引入第20个AgentKind分支的问题用既有provider-neutral partial fallback及下降至15的guard修复纳入本提交；另一项capture_modules_do_not_import_hook_entry_layers来自HEAD原有Claude admission直接引用，ingress/Claude源码逐字节等于HEAD，修复继续保留在Codex WIP，不夹带。先前联合工作区的10325全量PASS不是本次独立commit的全量PASS。仅本地提交，不推送；其它已有非Grok工作保留。
