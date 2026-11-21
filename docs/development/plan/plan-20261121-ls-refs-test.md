# Git protocol v2 ls-refs 回归测试计划（2026-11-21）

> 状态：测试规格的最终评审结论以 `plan-status.md` 中与本文 SHA-256 对应的记录为准；尚未执行。继承 [plan-template.md](plan-template.md) 的适用约束。本文是测试规格和移交计划，不授权产品实现、运行测试、提交、登录或部署。现有 `FP-23`/`FP-09` 验收见 [fetch/push 计划](plan-20261117-fetch-push.md)。

## 目标与事实基线

避免 `ls-refs` 的错误请求和不完整响应让非空远端被报告为空。2026-11-21 以前在临时目录用 Wrangler 4.165.0、Git 2.54.0、Libra 0.30.207 对本地 Artifacts 的观察：服务通告 `ls-refs=unborn`，Libra 把 `unborn` 放在 delim `0001` 前，服务返回 HTTP 200、零字节 body；测试代理仅将它移到 delim 后，服务返回 refs，clone 的 HEAD 与文件内容正确。临时环境已清理，原始证据不可重放，以上作为待重新验证的历史观察，不作为验收 PASS；代理不可用于产品通过证据。

当前源码 `src/internal/protocol/v2/ls_refs.rs` 的 `build_request` 在 delim 前加入 `unborn`；`parse_response` 对空 body 返回空集合且没有要求最终 flush。`src/internal/protocol/https_client.rs` 消费此结果；`src/command/fetch.rs` 对空发现返回成功。现有 unit test 只检查 `unborn` 存在，不检查位置。开工时刷新源码、版本、测试命令与写集。`gitprotocol-v2` 的 ls-refs 响应为 `*ref flush-pkt`：合法空结果是 `0000`；零字节或数据后 EOF 缺少 flush。不得把合法空结果一律判错。

## 落地合同与依赖

| ID | 类型 / 状态 | 产物及边界 |
|---|---|---|
| LS-H01 | `handoff` / `pending` | 本文测试规格与脱敏旧版实验摘要；`Deliverables = docs/development/plan/plan-20261121-ls-refs-test.md`；`Release boundary = no-release`。本轮仅撰写和评审，测试及实现均不执行。 |
| DEP-LS-01 | outgoing / 未接受 | 候选接收方为 Libra 协议维护者；移交日期待实际发送后记录（本计划尚未发送），拟由其创建并接受修复卡 `LS-R01`。移交请求包含 LS-T1–T6、对象与引用验收、失败路径和写集；本计划完成规格后不重做产品实现或执行验收。若无人接手，LS-H01 保持 pending，风险在本文持续登记、不得记为已关闭；收到具体修复卡、责任人和接收记录前，不得宣称修复已排期或测试已交付。 |

**LS-H01 字段：** `Lifecycle / Acceptance = pending / 空`；`Implementation write set = docs/development/plan/plan-20261121-ls-refs-test.md`；`Docs and compatibility impact = 本测试规格，无产品文档变更`；`Rollback mode = revert 文档变更`；`Version increment = none`；`C/D coverage from = LS-R01（拟议接收卡，尚未创建或接受；未获得覆盖前不宣称完成）`；`Granularity = type=handoff; axis=ls-refs测试规格移交; recovery=文档回退; complete=yes; self-contained=yes; AC=2/20; VER=1/20; landing=0; prod-files=0; scope=S; deps=DEP-LS-01; writeset=本文; release=no-release; split-from=N/A; exception=N/A`。AC：① 最终规格取得 Claude PASS；② 承接方与实际移交日期已记录。Verification：核对同一受审 SHA 的 Claude 结论和移交记录；未获接收时保持 pending。

未来经明确授权执行时，先在隔离工作区做红绿验证，旧基线的失败测试不得单独提交到 main；测试与产品修复、相应文档在同一完整实现卡中落地（G-02）。证据摘要、命令、计数及受审 SHA 只写本计划「评审记录」或将来承接计划的证据章节，不在仓库新建证据文件。按 G-10 协调 `fetch.rs`、测试和文档共享写集；按 ER-12 串行发布。测试计划评审 PASS 只表示规格通过，不代表执行、修复或部署通过。

## 隔离夹具与强制前提

- 临时目录保存虚构裸 Git 仓库、HOME、独立 `LIBRA_CONFIG_GLOBAL_DB` 和客户端工作树；使用仓库 `ConfigDbFixture` 模式隔离 Libra 配置，Git 设置 `GIT_CONFIG_NOSYSTEM=1`、隔离 HOME 并用 `-c protocol.version=2`。HTTP 服务仅绑定 `127.0.0.1` 的随机端口，不连接真实 Cloudflare、不登录、不部署、不升级。关闭服务并等待所有子进程后清理目录。
- 真实对象服务参考 `tests/protocol_capability_negotiation.rs` 的 axum 到 `git upload-pack --stateless-rpc` 模式，在默认测试层实现可复用的本地服务：子进程按客户端 `Git-Protocol` header 设置 `GIT_PROTOCOL`；v2 模式 GET 输出 v2 advertisement，v0 模式用于建立前置状态，POST 将客户端 body 原样送给 upload-pack 并返回真实结果；不用该测试的 shallow 状态变更逻辑。服务先按 pkt-line 解析并检查 `command`、能力段（允许 `agent`、`object-format`、`session-id`、`server-option`）、至多一个 delim（有参数时必需）、参数段与 flush；能力段出现已知 ls-refs 参数（`unborn`、`peel`、`symrefs` 或 `ref-prefix`）时固定返回 HTTP 200、零字节 body，以确定性复现本次症状；其他非白名单 token 或非法请求返回 HTTP 400 并记录违规。压缩请求必须按 `Content-Encoding: gzip` 解压，不得将压缩字节当 pkt-line。v0 前置阶段跳过 v2 分段校验，原样转发。合法请求不得改写或代理修正。该夹具必须能由 Git 和 Libra 实际下载并读取真实 pack；原参考 target 受 `test-network` feature 门控，仅借鉴代码，不将新夹具放入该 target。请求可能多次发现，不固定 GET/POST 次数。
- 每个请求记录脱敏的方法、协议头、pkt-line 段及 response 长度；不记录凭据。v2 阶段硬断言两端实际发送 `Git-Protocol: version=2` 且 POST 含 `command=ls-refs`；在通告 unborn 的 T3(ii)、T4、T5、T6 另硬断言 `ls-refs=unborn`，T3(i) 反向断言 `ls-refs` 不含 `unborn`。v0 前置阶段不要求 v2 header。缺失相应前提即失败而非 skip；若安装 Git 不满足 v2/unborn 能力也明确失败，不自行安装。测试不依赖固定版本号推测能力。
- T4 恶意响应与 T5/T6 正常真实 pack 服务使用独立夹具。禁止只验退出码或固定假 pack；验证提交 OID 与文件精确字节。可选补充 Wrangler 测试必须 `remote = false`，不替代本地 CI 强制门。

## 场景与基线预期

| ID | 场景与完成判据 | 已知旧实现的预期（须实测记录） |
|---|---|---|
| LS-T1 | 单元测试用 `read_pkt_frame` 逐帧解码请求，SHA-1 与已通告 `object-format=sha256` 的 SHA-256 两种配置分别检查能力段 `command=ls-refs`、`agent` 和条件性 `object-format` 的位置；参数段的条件性 `unborn` 恰出现一次且在唯一 delim 后、flush 前；`peel`、`symrefs`、`ref-prefix` 保持构造顺序。 | 声明 unborn 时因位置错误失败。 |
| LS-T2 | 不通告 unborn 的请求，其他选项与 pkt-line 边界合法且不发送 unborn。 | 应通过，作为守卫；不得强求红灯。 |
| LS-T3 | 解析层分别测 `0000` 和 `unborn HEAD symref-target:refs/heads/trunk` 加 flush：前者 refs 为空，后者 refs 为空且 `head_symref` 为 `refs/heads/trunk`。命令层用两种空仓库独立夹具：(i) GET 和 POST 的 upload-pack 均以 `git -c lsrefs.unborn=ignore` 关闭 unborn 通告并返回 `0000`；(ii) 用 `git init --bare -b trunk` 建空仓库，通告 unborn，并在客户端同时发送 `unborn` 和 `symrefs` 且 ref-prefix 未排除 HEAD 时返回 `unborn HEAD symref-target:refs/heads/trunk` 加 flush。分别运行 `ls-remote`、clone、fetch，三者须退出 0；`ls-remote` 无 refs、clone 无提交，(i) HEAD 为客户端默认的 main，(ii) 在 `https_client` 的 discovery 层断言 capabilities 含 `symref=HEAD:refs/heads/trunk`；clone HEAD 与 Git 同夹具的差异仅作观察，不作为本计划通过门。fetch 不改变预先建立的 tracking ref。前置 ref 由非空阶段 v0 fetch 构造后切空远端并切回 v2；设置非空 FETCH_HEAD，调用前快照。合法空结果下记录 FETCH_HEAD 的存在与字节状态及 Git 同夹具对照；其对齐行为不作为本计划通过门。先断言服务返回的精确帧及通告，使 (i) 的旧请求也合法、(ii) 的旧请求被严格拒绝；(ii) 使用手工构造的合规 `command=ls-refs`、delim、`unborn`、`symrefs`、flush 请求探测，不以可能未发送 unborn 的 Git `ls-remote` 代替。discovery 能力断言放在集成测试层，通过公开 `HttpsClient` API 使用同一真实服务，并隔离配置与 cwd。 | 解析层应通过；命令层和 FETCH_HEAD 按实际基线记录，不预设旧行为。 |
| LS-T4 | 解析层及 HTTP 命令层分别测试：a) HTTP 200 零字节 body；b) 合法 ref 数据行后直接 EOF；均缺失最终 flush。`ls-remote`、clone、`fetch --prune` 必须非零并给出明确错误，不得报告成功。T4 使用可切换正常/恶意响应的同一独立服务：恶意模式的 ls-refs POST 跳过请求分段检查，按 a/b 指定体直接返回，以确保旧实现也实际接收 b 的数据行加 EOF；先以正常模式在隔离 Libra 客户端中设置 `protocol.version=0`，clone/fetch 建立 M1 tracking ref，并写入非空 FETCH_HEAD；将 `protocol.version` 显式设为 `2` 并将服务切为恶意响应，在调用前快照并断言生效配置及请求头。fetch 的既有 tracking refs、非空 FETCH_HEAD 前后逐字节相同；新建 clone 目标失败后不存在，预先存在的空目标保持为空。Git 同样对照零字节和数据行后 EOF 两种响应的失败。 | 当前解析器对两种缺失 flush 都可能返回 Ok；命令的实际退出、输出及 FETCH_HEAD 前后状态逐项记录，不能假设会打印 `Already up to date`。 |
| LS-T5 | 正常严格 v2 服务非空源：main→M1，附注 tag A1→M1，固定虚构文件；先以 Git 读取相同服务验收夹具。无代理 Libra `ls-remote` 返回 M1、A1；clone HEAD 为 M1、文件字节正确；抓到 `want M1` 和真实 pack。`ls-remote` 必须分别报告 tag 对象 A1 与 `A1^{}` 指向 M1；clone 后的 tag 是否持久化不是本场景验收，失败须与协议发现和 tag 跟随分别归因。 | 旧客户端参数段不合格，严格夹具固定返回零字节；旧实现可能假成功，按 OID/内容断言应失败。 |
| LS-T6 | 从 LS-T5 夹具的成功 clone 开始，推进 main 至 M2、更新文件；Libra fetch 后 tracking ref 为 M2、按 ref 读取新文件精确字节，抓到 `want M2`。与 Git 相同源对照。 | 旧实现因 LS-T5 无法建立有效 clone；基线在 Libra 客户端以 v0 从正常服务 clone 得到 M1，切回 v2 后推进服务至 M2，调用前快照 tracking ref、FETCH_HEAD 与文件，按目标 OID 和内容断言失败；不能用 Git 的 `.git` 客户端替代 `.libra` 客户端。 |

`delim` / `response-end` 是否允许在 ls-refs 响应中终止、HTTP Content-Type 校验以及 v2 fetch 的空响应解析不属于本次测试规格；若调查发现会影响 LS-T4 的失败安全性，先在本文新增待分配的范围记录并暂停受影响验收，不隐式扩写。现有 delimiter/response-end 行为及相关单测保持不变，本计划不要求将其判错。

## 承接执行与验证

建议未来实现测试的写集为 `src/internal/protocol/v2/ls_refs.rs` 内测试模块、`src/internal/protocol/https_client.rs` 测试模块、`tests/command/ls_remote_test.rs`、`tests/command/clone_cli_test.rs`、`tests/command/fetch_test.rs` 及必要的测试服务模块；新增/注册 target 或共享 helper 时同步 `Cargo.toml`、`tests/INDEX.md`、`tests/SERIAL_REGISTRY.tsv` 与生成的 `.config/nextest.toml`（仅实际适用时）。这些是未来候选，不是本轮授权。检查 `tests/command/mod.rs` 的真实模块路径后再选位置；跨进程配置或环境需明确 lane，按仓库注册规则串行。

承接者先创建测试并对旧版逐项记录上表红/绿结果，再与获授权的修复同一变更跑绿。为每个新测试确定唯一完整名称，先以 `cargo nextest list --lib -E 'test(=<完整单元测试名>)'` 或 `cargo nextest list --test command_test -E 'test(=<完整命令测试名>)'` 确认恰好命中 1 项，再用相同过滤条件的 `cargo nextest run` 执行并记录通过/失败/跳过；测试名须以实际注册为准。随后运行对应模块回归、格式及 Clippy，并依据实际写集执行 ER-04/ER-13（触及共享测试入口或 helper 时重核 T-4），缺失工具或未执行如实记未完成。

## 完成判据与范围

LS-H01 完成仅要求规格经 Claude 明确 PASS、无未关闭阻断意见，并向未来修复卡移交上述规格；在未获后续执行授权时旧版红灯证据标为未执行，不得捏造。实现通过须由接收卡以修复后相同测试全绿、真实对象与引用结果证明。空仓库 clone 消费 unborn HEAD 与合法空 fetch 的 FETCH_HEAD 对齐仅记录差异、由未来独立卡决定；直接 URL 目标映射、trace 原命名空间、认证和 push HEAD 语义均不在本文范围；前两项分别由 `FP-23`、`FP-09` 承接。

## 评审记录

- Claude R1：`FAIL`，受审文件 SHA-256 `5dc7002a729dbfc1d5d485329a67f35463b57109e2954fa90b9f7fad548171d6`。要求明确红绿与移交边界、严格真实 pack 夹具、基线预期、帧分段和不完整响应、配置隔离、计划结构；本次修订逐项回应，待复审。R1 建议把证据写到临时路径或提前提交 ignored 测试，与本任务仅规划测试及仓库证据卫生要求不一致，未采纳。

- Claude R2：`FAIL`，受审 SHA-256 `c8c1846b6d93e3b0b6b25d9abe069be6c86e8daffaceb30aa683dbd4c22ce31f`。要求明确空仓库两种服务配置、旧版前置仓库构造与移交卡字段；同时补充严格夹具和 tag 判据。已修订，待复审。

- Claude R3：`FAIL`，受审 SHA-256 `39fef1ffbe67360419aa3e8d4ff2bb3d5718d71a45b7775d89361c82fa68669d`。指出通告硬前提与无 unborn 场景冲突、空仓库默认分支无法证明 symref 被消费；修订区分 v0/v2 与两种服务配置、采用 trunk、补非空前置快照和 Git 对照，待复审。

- Claude R4：`FAIL`，受审 SHA-256 `eed3331a9c672b9d640d454f59ea44f78f99b6e19237c0e20fdee3784f07ff86`。指出解析输入与 trunk 期望不一致，以及空 clone 消费 unborn symref 是额外产品行为；修订统一 trunk 输入、将 symref 验收限定 discovery 层并把 clone 和合法空 FETCH_HEAD 对齐降为观察；明确恶意响应模式和 v2 配置，待复审。

- Claude R5：`PASS`，P0/P1 均为 0，受审 SHA-256 `88e859f51e8f8b6d3a8102080f4030cda004f832d7fccc1915fb7abe330986ea`；三项非阻断建议涉及 unborn 帧探针、discovery 测试层和 200/400 边界，本次予以澄清，最终字节需重新复审。
